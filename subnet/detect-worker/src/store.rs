//! The detection state machine and the store's rules (pure).
//!
//! A detection row moves only along the edges of [`edge`]; every statement
//! that changes a state names its `from` states in its `WHERE` clause, and a
//! test checks each statement's list against this table. The rules:
//!
//! - **Ended states.** `deleted` and `expired` are tombstones: the row keeps
//!   its id, kind and times so the slug is never reused, and loses its
//!   document key, manage hash and owner. Nothing leaves them.
//! - **Legal states.** `excerpt_withheld` (public pages only), `disabled`
//!   (secret pages only, pending a review) and `removed` come from removal
//!   cases and operator ops; `restore` returns a page to `live`.
//! - **Expiry.** A secret page's `expires_at` is enforced on every read
//!   ([`read_state`]): a `live`, `pending` or `failed` row past it reads as
//!   `expired` before the hourly sweep tombstones it. A page under a legal
//!   state keeps that state until the case ends.
//! - **Copies.** A secret re-run copy (`copy_of` names its family root) ends
//!   when its root is deleted or expires, and its expiry never passes the
//!   root's (`expiry_cap`).

use slop_ninja_detect_page::codes::{DetState, Expiry, Kind, OwnerRelation};
use slop_ninja_detect_page::refusal::RefusalCode;

use crate::db::{Row, Val};
use crate::keys::{Bytes32, ct_eq};

/// Whether a row may move from `from` to `to` (for a page of `kind`).
pub fn edge(kind: Kind, from: DetState, to: DetState) -> bool {
    use DetState::*;
    let public = kind == Kind::PublicUrl;
    let secret = kind.is_secret();
    match (from, to) {
        (Deleted | Expired, _) => false,
        (_, ExcerptWithheld) | (ExcerptWithheld, _) if !public => false,
        (_, Disabled) | (Disabled, _) if !secret => false,
        (_, Expired) if !secret => false,
        (Pending, Live | Failed | Deleted | Expired | Removed | Disabled) => true,
        (Live, ExcerptWithheld | Removed | Disabled | Deleted | Expired) => true,
        (Failed, Deleted | Expired | Removed) => true,
        // A page back from a review returns failed when it has no result to
        // show: a secret page that never had a document (a paid job the
        // review stopped), a public page whose report is no longer kept.
        (ExcerptWithheld, Live | Failed | Removed | Deleted) => true,
        (Disabled, Live | Failed | Removed | Deleted) => true,
        (Removed, Live | Failed | ExcerptWithheld | Deleted) => true,
        _ => false,
    }
}

/// The states a delete (by manage token, owner or operator) tombstones.
pub const DELETABLE: &[DetState] = &[DetState::Live, DetState::Pending, DetState::Failed, DetState::Disabled];
/// The states the expiry sweep and the read rule expire.
pub const EXPIRING: &[DetState] = &[DetState::Live, DetState::Pending, DetState::Failed];
/// The states in which a page's own expiry can be changed and its owner
/// claimed.
pub const MANAGEABLE: &[DetState] = &[DetState::Live, DetState::Pending];
/// The states of a family root's copies that end with it.
pub const COPY_ENDS: &[DetState] = &[DetState::Live, DetState::Pending];

/// The SQL list `('a','b')` of `states`, for checking statements.
pub fn sql_list(states: &[DetState]) -> String {
    let inner: Vec<String> = states.iter().map(|s| format!("'{}'", s.as_str())).collect();
    format!("({})", inner.join(","))
}

/// The state a row reads as at `now`: expiry is enforced on read.
pub fn read_state(state: DetState, expires_at: Option<u64>, now: u64) -> DetState {
    match expires_at {
        Some(t) if t <= now && EXPIRING.contains(&state) => DetState::Expired,
        _ => state,
    }
}

/// The refusal an ended page answers on the owner and manage routes
/// (`page_gone`, `page_removed`, `page_disabled`); `None` for a page that
/// can still be saved or managed.
pub fn ended_refusal(state: DetState) -> Option<RefusalCode> {
    match state {
        DetState::Deleted | DetState::Expired => Some(RefusalCode::PageGone),
        DetState::Removed => Some(RefusalCode::PageRemoved),
        DetState::Disabled => Some(RefusalCode::PageDisabled),
        _ => None,
    }
}

/// The re-run gate of a secret page (checked before anything is read or
/// decrypted): only a live `private_text` page whose run is older than the
/// serving package is re-run.
pub fn rerun_gate(kind: Kind, state: DetState, differs_from_run: bool) -> Result<(), RefusalCode> {
    match state {
        DetState::Deleted | DetState::Expired => Err(RefusalCode::RerunGone),
        DetState::Removed | DetState::Disabled => Err(RefusalCode::RerunBlocked),
        _ if kind != Kind::PrivateText => Err(RefusalCode::RerunUnavailable),
        DetState::Live if differs_from_run => Ok(()),
        DetState::Live => Err(RefusalCode::RerunNotNeeded),
        _ => Err(RefusalCode::RerunUnavailable),
    }
}

/// A new `expires_at` for `choice` at `now`, clamped to a copy's cap.
pub fn new_expiry(choice: Expiry, now: u64, cap: Option<u64>) -> Option<u64> {
    let want = choice.duration_ms().map(|d| now.saturating_add(d));
    match (want, cap) {
        (_, None) => want,
        (None, Some(c)) => Some(c),
        (Some(w), Some(c)) => Some(w.min(c)),
    }
}

/// The expiry choices a page's manage page offers: all of them, or for a
/// re-run copy only those that end no later than its cap.
pub fn expiry_choices(now: u64, cap: Option<u64>) -> Vec<Expiry> {
    Expiry::ALL
        .iter()
        .copied()
        .filter(|e| match (cap, e.duration_ms()) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(c), Some(d)) => now.saturating_add(d) <= c,
        })
        .collect()
}

/// A row that does not have the shape its reader expects (a value out of
/// its column's form).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadRow;

/// A detection row as the store reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetRow {
    pub id_h: Bytes32,
    pub pepper_v: Option<u32>,
    pub public_slug: Option<String>,
    pub kind: Kind,
    pub platform: slop_ninja_detect_page::codes::Platform,
    pub state: DetState,
    pub state_v: u32,
    pub refusal: Option<RefusalCode>,
    pub created_at: u64,
    pub updated_at: u64,
    pub expires_at: Option<u64>,
    pub source_id: Option<u64>,
    pub run_id: Option<u64>,
    pub superseded_by: Option<Bytes32>,
    /// The newer page's public slug (`superseded_by` joined).
    pub superseded_slug: Option<String>,
    pub package_load: Option<String>,
    pub manage_h: Option<Bytes32>,
    pub owner_h: Option<Bytes32>,
    pub owner_via: Option<slop_ninja_detect_page::codes::OwnerVia>,
    pub doc_key: Option<String>,
    pub index_force: Option<slop_ninja_detect_page::codes::IndexForce>,
    pub copy_of: Option<Bytes32>,
    pub expiry_cap: Option<u64>,
}

/// The columns [`DetRow::from_row`] reads, from `detections d` (with `n`
/// the left-joined newer detection).
pub const DET_COLUMNS: &str = "hex(d.id_h) AS id_h, d.pepper_v AS pepper_v, d.public_slug AS public_slug, \
d.kind AS kind, d.platform AS platform, d.state AS state, d.state_v AS state_v, d.refusal AS refusal, \
d.created_at AS created_at, d.updated_at AS updated_at, d.expires_at AS expires_at, d.source_id AS source_id, \
d.run_id AS run_id, hex(d.superseded_by) AS superseded_by, n.public_slug AS superseded_slug, \
d.package_load AS package_load, hex(d.manage_h) AS manage_h, hex(d.owner_h) AS owner_h, \
d.owner_via AS owner_via, d.doc_key AS doc_key, d.index_force AS index_force, hex(d.copy_of) AS copy_of, \
d.expiry_cap AS expiry_cap";

/// `SELECT <DET_COLUMNS> FROM detections d LEFT JOIN detections n ON
/// n.id_h = d.superseded_by WHERE d.id_h = ?1`.
pub const SELECT_DET: &str = concat!(
    "SELECT hex(d.id_h) AS id_h, d.pepper_v AS pepper_v, d.public_slug AS public_slug, ",
    "d.kind AS kind, d.platform AS platform, d.state AS state, d.state_v AS state_v, d.refusal AS refusal, ",
    "d.created_at AS created_at, d.updated_at AS updated_at, d.expires_at AS expires_at, d.source_id AS source_id, ",
    "d.run_id AS run_id, hex(d.superseded_by) AS superseded_by, n.public_slug AS superseded_slug, ",
    "d.package_load AS package_load, hex(d.manage_h) AS manage_h, hex(d.owner_h) AS owner_h, ",
    "d.owner_via AS owner_via, d.doc_key AS doc_key, d.index_force AS index_force, hex(d.copy_of) AS copy_of, ",
    "d.expiry_cap AS expiry_cap ",
    "FROM detections d LEFT JOIN detections n ON n.id_h = d.superseded_by WHERE d.id_h = ?1"
);

fn opt_hex32(r: &Row, name: &str) -> Result<Option<Bytes32>, BadRow> {
    match r.get(name) {
        None | Some(Val::Null) => Ok(None),
        // `hex(NULL)` is the empty string in SQLite.
        Some(Val::Text(s)) if s.is_empty() => Ok(None),
        Some(_) => r.hex32(name).map(Some).ok_or(BadRow),
    }
}

fn opt_code<T>(r: &Row, name: &str, parse: impl Fn(&str) -> Option<T>) -> Result<Option<T>, BadRow> {
    match r.text(name) {
        None => Ok(None),
        Some(s) => parse(s).map(Some).ok_or(BadRow),
    }
}

impl DetRow {
    /// Reads a row selected with [`DET_COLUMNS`]; `Err` for any value out of
    /// its column's form.
    pub fn from_row(r: &Row) -> Result<DetRow, BadRow> {
        use slop_ninja_detect_page::codes::{IndexForce, OwnerVia, Platform};
        Ok(DetRow {
            id_h: r.hex32("id_h").ok_or(BadRow)?,
            pepper_v: r.u64("pepper_v").map(|v| u32::try_from(v).map_err(|_| BadRow)).transpose()?,
            public_slug: r.text("public_slug").map(str::to_owned),
            kind: Kind::parse(r.text("kind").ok_or(BadRow)?).ok_or(BadRow)?,
            platform: Platform::parse(r.text("platform").ok_or(BadRow)?).ok_or(BadRow)?,
            state: DetState::parse(r.text("state").ok_or(BadRow)?).ok_or(BadRow)?,
            state_v: u32::try_from(r.u64("state_v").ok_or(BadRow)?).map_err(|_| BadRow)?,
            refusal: opt_code(r, "refusal", RefusalCode::parse)?,
            created_at: r.u64("created_at").ok_or(BadRow)?,
            updated_at: r.u64("updated_at").ok_or(BadRow)?,
            expires_at: r.u64("expires_at"),
            source_id: r.u64("source_id"),
            run_id: r.u64("run_id"),
            superseded_by: opt_hex32(r, "superseded_by")?,
            superseded_slug: r.text("superseded_slug").map(str::to_owned),
            package_load: r.text("package_load").map(str::to_owned),
            manage_h: opt_hex32(r, "manage_h")?,
            owner_h: opt_hex32(r, "owner_h")?,
            owner_via: opt_code(r, "owner_via", OwnerVia::parse)?,
            doc_key: r.text("doc_key").map(str::to_owned),
            index_force: opt_code(r, "index_force", IndexForce::parse)?,
            copy_of: opt_hex32(r, "copy_of")?,
            expiry_cap: r.u64("expiry_cap"),
        })
    }

    /// The state this row reads as at `now`.
    pub fn state_at(&self, now: u64) -> DetState {
        read_state(self.state, self.expires_at, now)
    }

    /// Whether `token` checks against the row's manage hash.
    pub fn manage_ok(&self, token: Option<&str>) -> bool {
        match (token, &self.manage_h) {
            (Some(t), Some(h)) => ct_eq(&crate::keys::manage_h(t), h),
            _ => false,
        }
    }

    /// The asking account's relation to the row's owner.
    pub fn owner_relation(&self, owner_h: &Bytes32) -> OwnerRelation {
        match &self.owner_h {
            None => OwnerRelation::None,
            Some(o) if ct_eq(o, owner_h) => OwnerRelation::You,
            Some(_) => OwnerRelation::Other,
        }
    }

    /// The family root a re-run copy of this page belongs to.
    pub fn family_root(&self) -> Bytes32 {
        self.copy_of.unwrap_or(self.id_h)
    }
}

/// A secret page's document key in R2: `p/<hex id_h>`.
pub fn secret_doc_key(id_h: &Bytes32) -> String {
    format!("p/{}", hex::encode(id_h))
}

/// A public run's document key in R2: `r/<run_id>`.
pub fn run_doc_key(run_id: u64) -> String {
    format!("r/{run_id}")
}

/// A paid job's text key and its section prefix: `j/<hex id_h>/...`.
pub fn job_prefix(id_h: &Bytes32) -> String {
    format!("j/{}/", hex::encode(id_h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use DetState::*;

    #[test]
    fn every_edge_of_the_table() {
        // (from, to) pairs allowed for a secret text page.
        let secret: &[(DetState, DetState)] = &[
            (Pending, Live),
            (Pending, Failed),
            (Pending, Deleted),
            (Pending, Expired),
            (Pending, Removed),
            (Pending, Disabled),
            (Live, Removed),
            (Live, Disabled),
            (Live, Deleted),
            (Live, Expired),
            (Failed, Deleted),
            (Failed, Expired),
            (Failed, Removed),
            (Disabled, Live),
            (Disabled, Failed),
            (Disabled, Removed),
            (Disabled, Deleted),
            (Removed, Live),
            (Removed, Failed),
            (Removed, Deleted),
        ];
        let public: &[(DetState, DetState)] = &[
            (Pending, Live),
            (Pending, Failed),
            (Pending, Deleted),
            (Pending, Removed),
            (Live, ExcerptWithheld),
            (Live, Removed),
            (Live, Deleted),
            (Failed, Deleted),
            (Failed, Removed),
            (ExcerptWithheld, Live),
            (ExcerptWithheld, Failed),
            (ExcerptWithheld, Removed),
            (ExcerptWithheld, Deleted),
            (Removed, Live),
            (Removed, Failed),
            (Removed, ExcerptWithheld),
            (Removed, Deleted),
        ];
        for (kind, allowed) in [(Kind::PrivateText, secret), (Kind::PrivatePaid, secret), (Kind::PublicUrl, public)] {
            for &from in DetState::ALL {
                for &to in DetState::ALL {
                    assert_eq!(edge(kind, from, to), allowed.contains(&(from, to)), "{kind} {from} -> {to}");
                }
            }
        }
        // The statement lists follow the table.
        for &s in DELETABLE {
            assert!(edge(Kind::PrivateText, s, Deleted), "{s}");
        }
        for &s in EXPIRING {
            assert!(edge(Kind::PrivateText, s, Expired), "{s}");
        }
        assert_eq!(sql_list(DELETABLE), "('live','pending','failed','disabled')");
    }

    #[test]
    fn expiry_is_enforced_on_read() {
        assert_eq!(read_state(Live, Some(10), 10), Expired);
        assert_eq!(read_state(Live, Some(11), 10), Live);
        assert_eq!(read_state(Pending, Some(5), 10), Expired);
        assert_eq!(read_state(Disabled, Some(5), 10), Disabled, "a legal state holds");
        assert_eq!(read_state(Deleted, Some(5), 10), Deleted);
        assert_eq!(read_state(Live, None, u64::MAX), Live);
    }

    #[test]
    fn the_rerun_gate_per_state_and_kind() {
        assert_eq!(rerun_gate(Kind::PrivateText, Live, true), Ok(()));
        assert_eq!(rerun_gate(Kind::PrivateText, Live, false), Err(RefusalCode::RerunNotNeeded));
        assert_eq!(rerun_gate(Kind::PrivateText, Deleted, true), Err(RefusalCode::RerunGone));
        assert_eq!(rerun_gate(Kind::PrivateText, Expired, true), Err(RefusalCode::RerunGone));
        assert_eq!(rerun_gate(Kind::PrivateText, Removed, true), Err(RefusalCode::RerunBlocked));
        assert_eq!(rerun_gate(Kind::PrivateText, Disabled, true), Err(RefusalCode::RerunBlocked));
        assert_eq!(rerun_gate(Kind::PrivateText, Pending, true), Err(RefusalCode::RerunUnavailable));
        assert_eq!(rerun_gate(Kind::PrivateText, Failed, true), Err(RefusalCode::RerunUnavailable));
        assert_eq!(rerun_gate(Kind::PrivatePaid, Live, true), Err(RefusalCode::RerunUnavailable));
        assert_eq!(rerun_gate(Kind::PrivatePaid, Deleted, true), Err(RefusalCode::RerunGone));
        assert_eq!(rerun_gate(Kind::PrivatePaid, Disabled, true), Err(RefusalCode::RerunBlocked));
    }

    #[test]
    fn copies_never_outlive_their_cap() {
        let day = 86_400_000;
        assert_eq!(new_expiry(Expiry::Week, 0, None), Some(7 * day));
        assert_eq!(new_expiry(Expiry::Keep, 0, None), None);
        assert_eq!(new_expiry(Expiry::Keep, 0, Some(3 * day)), Some(3 * day));
        assert_eq!(new_expiry(Expiry::Week, 0, Some(3 * day)), Some(3 * day));
        assert_eq!(new_expiry(Expiry::Day, 0, Some(3 * day)), Some(day));
        assert_eq!(expiry_choices(0, None), Expiry::ALL.to_vec());
        assert_eq!(expiry_choices(0, Some(7 * day)), vec![Expiry::Day, Expiry::Week]);
        assert_eq!(expiry_choices(0, Some(day - 1)), Vec::<Expiry>::new());
    }

    #[test]
    fn ended_pages_refuse_owner_actions() {
        assert_eq!(ended_refusal(Deleted), Some(RefusalCode::PageGone));
        assert_eq!(ended_refusal(Expired), Some(RefusalCode::PageGone));
        assert_eq!(ended_refusal(Removed), Some(RefusalCode::PageRemoved));
        assert_eq!(ended_refusal(Disabled), Some(RefusalCode::PageDisabled));
        assert_eq!(ended_refusal(Live), None);
        assert_eq!(ended_refusal(Pending), None);
    }
}
