//! Removal cases (DESIGN 14.1; BACKEND 8): the two-step intake of the
//! removal form, the status page, the automatic first steps, the case state
//! machine, strikes, and the effects of the case ops ([`crate::ops`] verifies
//! and journals each op and calls in here).
//!
//! - **Intake** ([`removal`]): `step = review` checks the track's fields and
//!   the target and writes nothing; `step = send` makes the case. The notice
//!   (the track's fields as the claimant wrote them, and the target page's
//!   link) is encrypted under `K_cases` into R2 `c/<case_id>` first; D1 then
//!   keeps the case's state and targets only, by `id_h`. A signed-in
//!   submitter is recorded as `submitter_h`. The case's `legal_notice`
//!   message takes the notice to the alerts relay's legal stream. No mail
//!   goes to the claimant: the case id and the status token, shown once,
//!   replace it. A counter-notice's proof (the secret page's manage token)
//!   is checked and never stored.
//! - **Automatic first steps**, in the case's own batch: a complete
//!   copyright notice against a live public page withholds its excerpt at
//!   once (pending review: the page is de-indexed and its card generic); an
//!   illegal-content report against a secret page disables it with its
//!   re-run copy family; a wrong-result report on a public page queues a
//!   background re-check; a report on an X post's page from its author
//!   removes the page at once (the X removal below). Everything else waits
//!   for the operator.
//! - **States** ([`case_edge`]): `received → reviewing → accepted →
//!   countered → restored`, `rejected`, `incomplete → received`, and
//!   `closed` from any other state. Each op names its `from` states in its
//!   statements' `WHERE` clauses.
//! - **Strikes**: an accepted copyright case against a page with an owner
//!   adds a `strikes` row with the page's `owner_via`. A strike on a claimed
//!   page counts only after `review_strike` says so.
//! - **The X removal** ([`x_removal`]): every page about one X post goes, as
//!   one removal, whether from the form, from `remove` or from `x_remove`:
//!   the opt-out tombstone that keeps only `canon_key` is written first, in
//!   its own batch, so no check of the post can store after it; then the run
//!   documents are deleted from R2, and one D1 batch removes the detections
//!   (they stay as content-free `x_removed` tombstones), deletes the runs it
//!   read and the source's aliases, names the tombstone in the case's
//!   `source_id`, and journals `x_remove {reason, canon_key}`; a last read
//!   of the tombstone deletes anything a check stored meanwhile.
//! - **Secret pages** removed by a case keep their encrypted document in
//!   quarantine (`q/<case_id>/<id_h>`, wrapped again under `K_cases`) for a
//!   counter-notice, until the case closes. Once a counter-notice's wait has
//!   passed, `status` lists the countered case under `restores_due`, with
//!   the counter-notice beside it; `restore` of either brings the page back
//!   and moves both on.
//!
//! Nothing here stores a notice's text in D1, writes a secret slug or an
//! email address to a row, a queue message or a metric point, sends a
//! secret slug to the legal stream ([`Notice::for_relay`]), or names an X
//! post by its link or id.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use slop_ninja_detect_page::api::{
    AutoStep, RemovalReceived, RemovalRequest, RemovalReviewAnswer, RemovalStatusAnswer,
};
use slop_ninja_detect_page::codes::RemovalStep;
use slop_ninja_detect_page::codes::{CaseKind, CaseState, DetState, HostRole, Kind, OwnerVia, Platform};
use slop_ninja_detect_page::ids::UserId;
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::queue::{Job, QueueMessage};
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_page::slug::{self, Folded, PUBLIC_LEN, PathMatch, SECRET_LEN, SlugRoute, TOKEN_LEN};
use slop_ninja_detect_page::sn::Principal;

use crate::db::{Done, Io, IoError, Stmt, Val, delete_docs};
use crate::doc::RunDoc;
use crate::keys::{Bytes32, NONCE_BYTES, StoreKeys, ct_eq, h, open_with, seal_with};
use crate::lookup::{SlugForm, public_lookup, secret_lookup};
use crate::queue::CASE_AAD;
use crate::share::Keys;
use crate::sitemap::SHARD_URLS;
use crate::store::{DET_COLUMNS, DetRow, run_doc_key, secret_doc_key};
use crate::x_flow;

const DAY: u64 = 86_400_000;

/// A case id: this prefix and [`CASE_ID_CHARS`] characters of the slug
/// alphabet (40 bits; a collision fails the case's batch, never merges two).
pub const CASE_ID_PREFIX: &str = "td-";
pub const CASE_ID_CHARS: usize = 8;
/// The domain of a status token's hash (`cases.status_h`).
pub const STATUS_DOMAIN: &[u8] = b"case_status_v1";
/// The associated data of a quarantined document (`q/<case_id>/<id_h>`),
/// followed by the case id and the page's id, so a copy opens only as itself.
pub const QUARANTINE_AAD: &[u8] = b"slop_ninja_detect_quarantine_v1";
/// The associated data of an operator note (`c/<case_id>/n/<key>`).
pub const NOTE_AAD: &[u8] = b"slop_ninja_detect_case_note_v1";
/// The domain of an X source's key: `canon_key = H("source_v1" ‖ key)`.
pub const SOURCE_DOMAIN: &[u8] = b"source_v1";

/// The notice's field bytes, at most: the website's removal form body bound
/// (256 KiB). Decoding a form never lengthens its values, so no form the
/// website reads can carry more; the bound holds a hand-made request to it.
pub const NOTICE_FIELD_BYTES: usize = 256 * 1024;
/// The notice's JSON, at most: every value byte escaped to at most two
/// (control characters other than tab and line breaks are refused, so no
/// value needs `\u00XX`), each field's name and quotes, and the envelope.
pub const NOTICE_JSON_BYTES: usize = 2 * NOTICE_FIELD_BYTES + MAX_FIELDS * (MAX_FIELD_NAME + 8) + 4096;
/// The most fields a track has (the copyright notice's nine).
pub const MAX_FIELDS: usize = 9;
/// The longest field name of any track.
pub const MAX_FIELD_NAME: usize = 16;
/// An operator note's bytes, at most.
pub const NOTE_BYTES: usize = 8 * 1024;
/// A counter-notice's wait: § 512(g)(2)(C) restores no sooner than ten and
/// no later than fourteen business days after it is received. Fourteen
/// calendar days is the earliest moment ten business days are certain to
/// have passed (two weeks hold ten weekdays).
pub const COUNTER_WAIT_MS: u64 = 14 * DAY;
/// The jurisdiction a removal is recorded under when its op names none: the
/// operator's own (SlopNinja LLC).
pub const DEFAULT_JURISDICTION: &str = "US";

// --------------------------------------------------------------- the tracks

/// A field's form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    /// A checkbox: the value `yes`.
    Check,
    /// One of these values.
    Select(&'static [&'static str]),
}

/// A field of a track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldSpec {
    pub name: &'static str,
    pub required: bool,
    pub kind: FieldKind,
}

const fn f(name: &'static str, required: bool, kind: FieldKind) -> FieldSpec {
    FieldSpec { name, required, kind }
}

use FieldKind::{Check, Select, Text};

/// The copyright notice's scope choices.
pub const SCOPES: &[&str] = &["excerpt", "page"];
/// "I wrote or published this": who is asking.
pub const RELATIONS: &[&str] = &["author", "publisher"];
/// The illegal-content report's categories.
pub const ILLEGAL: &[&str] = &["personal_harm", "child_safety", "terrorism", "other"];

/// The counter-notice's proof for a secret page: its manage token, checked
/// against the page and never kept in the notice.
pub const MANAGE_FIELD: &str = "manage";

const COPYRIGHT: &[FieldSpec] = &[
    f("work", true, Text),
    f("scope", true, Select(SCOPES)),
    f("name", true, Text),
    f("email", true, Text),
    f("address", true, Text),
    f("details", false, Text),
    f("good_faith", true, Check),
    f("accurate", true, Check),
    f("signature", true, Text),
];
const AUTHOR: &[FieldSpec] = &[
    f("relation", true, Select(RELATIONS)),
    f("name", true, Text),
    f("email", true, Text),
    f("verification", true, Text),
    f("details", false, Text),
];
const PERSONAL: &[FieldSpec] = &[
    f("name", true, Text),
    f("email", true, Text),
    f("data", true, Text),
    f("country", false, Text),
    f("details", false, Text),
];
const WRONG: &[FieldSpec] = &[f("what", true, Text), f("email", false, Text)];
const ILLEGAL_FIELDS: &[FieldSpec] = &[
    f("category", true, Select(ILLEGAL)),
    f("explanation", true, Text),
    f("name", false, Text),
    f("email", false, Text),
    f("good_faith", true, Check),
];
const COUNTER: &[FieldSpec] = &[
    f("name", true, Text),
    f("email", true, Text),
    f("address", true, Text),
    f("material", true, Text),
    f("mistake", true, Check),
    f("consent", true, Check),
    f("signature", true, Text),
    f(MANAGE_FIELD, false, Text),
];
const COURT: &[FieldSpec] = &[
    f("authority", true, Text),
    f("reference", true, Text),
    f("country", true, Text),
    f("email", true, Text),
    f("details", true, Text),
];
const OTHER: &[FieldSpec] =
    &[f("basis", true, Text), f("name", true, Text), f("email", true, Text), f("details", true, Text)];

/// A track's fields (the website's form names each `<track>.<field>` and
/// forwards the chosen track's without the prefix). An `x_request` case is
/// never a form track.
pub fn track_fields(kind: CaseKind) -> Option<&'static [FieldSpec]> {
    Some(match kind {
        CaseKind::Copyright => COPYRIGHT,
        CaseKind::Author => AUTHOR,
        CaseKind::PersonalData => PERSONAL,
        CaseKind::WrongResult => WRONG,
        CaseKind::Illegal => ILLEGAL_FIELDS,
        CaseKind::CounterNotice => COUNTER,
        CaseKind::CourtOrder => COURT,
        CaseKind::OtherLegal => OTHER,
        CaseKind::XRequest => return None,
    })
}

// ------------------------------------------------------------ the state machine

/// Whether a case may move from `from` to `to`.
pub fn case_edge(from: CaseState, to: CaseState) -> bool {
    use CaseState::*;
    match (from, to) {
        (Closed, _) => false,
        (_, Closed) => true,
        (Received, Reviewing | Accepted | Rejected | Incomplete | Restored) => true,
        (Incomplete, Received | Reviewing | Accepted | Rejected) => true,
        (Reviewing, Accepted | Rejected | Restored) => true,
        (Accepted, Countered | Restored) => true,
        (Countered, Restored) => true,
        _ => false,
    }
}

/// The states a case can move to `to` from, as an SQL list.
pub fn from_states(to: CaseState) -> String {
    let inner: Vec<String> =
        CaseState::ALL.iter().filter(|f| case_edge(**f, to)).map(|f| format!("'{}'", f.as_str())).collect();
    format!("({})", inner.join(","))
}

// ------------------------------------------------------------------ checks

/// What the review of a request found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldCheck {
    /// Required fields that are absent or not in their form, in the track's
    /// order, then `target` and `manage` when those are missing.
    pub missing: Vec<String>,
}

/// Checks a track's fields: every name is the track's (anything else is
/// malformed), every value is within the notice's bound and holds no
/// control character other than tab and line breaks, and the required ones
/// are present in their form.
pub fn check_fields(kind: CaseKind, fields: &BTreeMap<String, String>) -> Result<FieldCheck, RefusalCode> {
    let spec = track_fields(kind).ok_or(RefusalCode::MalformedRequest)?;
    let mut total = 0usize;
    for (name, value) in fields {
        if !spec.iter().any(|f| f.name == name) {
            return Err(RefusalCode::MalformedRequest);
        }
        total = total.saturating_add(value.len());
        if value.chars().any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r')) {
            return Err(RefusalCode::MalformedRequest);
        }
    }
    if total > NOTICE_FIELD_BYTES {
        return Err(RefusalCode::TooLarge);
    }
    let mut missing = Vec::new();
    for fs in spec.iter().filter(|f| f.required) {
        let v = fields.get(fs.name).map(|v| v.trim()).filter(|v| !v.is_empty());
        let ok = match (fs.kind, v) {
            (_, None) => false,
            (Text, Some(_)) => true,
            (Check, Some(v)) => v == "yes",
            (Select(choices), Some(v)) => choices.contains(&v),
        };
        if !ok {
            missing.push(fs.name.to_owned());
        }
    }
    Ok(FieldCheck { missing })
}

/// The canonical slug a request's `target` names: a page's slug (folded
/// as a page path folds it), or a link to a page on one of the two
/// configured origins (any query or fragment dropped: a pasted manage link
/// never reaches the notice). `None` for anything else.
pub fn target_slug(origins: &Origins, target: &str) -> Option<(SlugForm, String)> {
    let t = target.trim();
    for (len, form) in [(PUBLIC_LEN, SlugForm::Public), (SECRET_LEN, SlugForm::Secret)] {
        match slug::fold(t, len) {
            Folded::Canonical => return Some((form, t.to_owned())),
            Folded::Fold(c) => return Some((form, c)),
            Folded::Invalid => {}
        }
    }
    let t = t.split(['#', '?']).next()?;
    for (role, form) in [(HostRole::Site, SlugForm::Public), (HostRole::Secret, SlugForm::Secret)] {
        let Some(path) = t.strip_prefix(origins.of(role).as_str()) else { continue };
        if !path.starts_with('/') {
            continue;
        }
        let m = match slug::match_path(role, path) {
            PathMatch::Redirect(p) => slug::match_path(role, &p),
            m => m,
        };
        if let PathMatch::Serve { slug, .. } = m {
            return Some((form, slug));
        }
    }
    None
}

/// A request's target page, found.
#[derive(Clone, Debug)]
pub struct Target {
    pub slug: String,
    pub row: DetRow,
}

impl Target {
    fn is_public(&self) -> bool {
        self.row.kind == Kind::PublicUrl
    }

    fn is_x(&self) -> bool {
        self.row.platform == Platform::X
    }

    /// The page's link, as the notice records it.
    fn url(&self, origins: &Origins) -> String {
        if self.is_public() { origins.public_page_url(&self.slug) } else { origins.secret_page_url(&self.slug) }
    }
}

async fn find_target(
    io: &impl Io,
    keys: Keys<'_>,
    origins: &Origins,
    target: &str,
) -> Result<Option<Target>, RefusalCode> {
    let Some((form, slug)) = target_slug(origins, target) else { return Ok(None) };
    let row = match form {
        SlugForm::Public => public_lookup(io, &slug).await.map_err(|_| RefusalCode::Unavailable)?.0,
        SlugForm::Secret => {
            secret_lookup(io, keys.peppers, &slug).await.map_err(|_| RefusalCode::Unavailable)?.0.map(|f| f.row)
        }
    };
    Ok(row.map(|row| Target { slug, row }))
}

// ------------------------------------------------------------------ intake

/// The answer of the removal route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemovalAnswer {
    Review(RemovalReviewAnswer),
    Received(RemovalReceived),
}

/// What the claimant wrote, as `c/<case_id>` keeps it (encrypted).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notice {
    pub v: u8,
    pub track: CaseKind,
    /// The page's canonical link. A secret page's link holds its slug, which
    /// opens the page: it stays in this sealed copy, for the operator's
    /// `case_show`, and the legal stream's copy ([`Notice::for_relay`])
    /// names the page only as [`PRIVATE_TARGET`].
    pub target: String,
    /// Whether the target is a secret page.
    pub secret: bool,
    /// The track's fields, without a counter-notice's manage token (which
    /// is masked wherever else the claimant pasted it).
    pub fields: BTreeMap<String, String>,
    pub received_at: u64,
    /// Whether the submitter was signed in.
    pub signed_in: bool,
}

/// The notice document's version.
pub const NOTICE_V: u8 = 1;

/// How the legal stream names a secret page (the relay's own stand-in).
pub const PRIVATE_TARGET: &str = "private detection";
/// What a secret page's slug reads as in the legal stream's copy of a
/// notice, wherever the claimant pasted it.
pub const SLUG_MASK: &str = "[private detection]";
/// What a counter-notice's manage token reads as wherever else the
/// claimant pasted it.
pub const TOKEN_MASK: &str = "[manage token]";

/// A byte as a page path folds a slug's: ASCII case, `i` and `l` as `1`,
/// `o` as `0`.
pub fn fold_byte(b: u8) -> u8 {
    match b.to_ascii_lowercase() {
        b'i' | b'l' => b'1',
        b'o' => b'0',
        c => c,
    }
}

/// `s` with every run of `canonical.len()` bytes that folds
/// ([`fold_byte`]) to `canonical` replaced by `mask`. A match is ASCII
/// only, so the result stays UTF-8.
pub fn mask_folded(s: &str, canonical: &str, mask: &str) -> String {
    let (src, want) = (s.as_bytes(), canonical.as_bytes());
    if want.is_empty() || src.len() < want.len() {
        return s.to_owned();
    }
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while let Some(&b) = src.get(i) {
        let window = src.get(i..i.saturating_add(want.len()));
        if window.is_some_and(|w| w.iter().zip(want).all(|(a, c)| fold_byte(*a) == *c)) {
            out.extend_from_slice(mask.as_bytes());
            i += want.len();
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

impl Notice {
    /// The copy the legal stream carries: a secret page is named only as
    /// [`PRIVATE_TARGET`], and its slug is masked in every field, so its link
    /// reaches neither the relay's storage nor the legal mailbox (the sealed
    /// copy keeps it for `case_show`). `None` when a secret target's link
    /// does not end in a secret slug (never for a notice made here), so the
    /// caller sends nothing rather than a copy it could not mask.
    pub fn for_relay(&self) -> Option<Notice> {
        if !self.secret {
            return Some(self.clone());
        }
        let slug = self.target.rsplit('/').next().filter(|s| slug::is_canonical(s, SECRET_LEN))?;
        Some(Notice {
            target: PRIVATE_TARGET.to_owned(),
            fields: self.fields.iter().map(|(k, v)| (k.clone(), mask_folded(v, slug, SLUG_MASK))).collect(),
            ..self.clone()
        })
    }
}

/// A status token's hash.
pub fn status_h(token: &str) -> Bytes32 {
    h(STATUS_DOMAIN, token.as_bytes())
}

/// A case's notice key in R2.
pub fn notice_key(case_id: &str) -> String {
    format!("c/{case_id}")
}

/// A quarantined copy's key in R2.
pub fn quarantine_key(case_id: &str, id_h: &Bytes32) -> String {
    format!("q/{case_id}/{}", hex::encode(id_h))
}

/// An X source's `canon_key`: `H("source_v1" ‖ "x:status:<id>")`, the
/// URL check's own key of the post ([`crate::x_flow::x_canon_key`]).
pub fn x_canon_key(id: u64) -> Bytes32 {
    crate::x_flow::x_canon_key(id)
}

/// Whether `s` has a case id's exact form.
pub fn is_case_id(s: &str) -> bool {
    s.strip_prefix(CASE_ID_PREFIX).is_some_and(|r| slug::is_canonical(r, CASE_ID_CHARS))
}

fn draw_case_id(io: &impl Io) -> String {
    let mut fill = |b: &mut [u8]| io.fill(b);
    format!("{CASE_ID_PREFIX}{}", slug::draw(CASE_ID_CHARS, &mut fill))
}

/// The page purge URLs of a public slug (the page, the X widget route and
/// the card).
pub fn purge_urls(origins: &Origins, slugs: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for s in slugs {
        for route in [SlugRoute::Page, SlugRoute::XWidget, SlugRoute::Card] {
            out.push(origins.site.url(&slug::path(HostRole::Site, route, s)));
        }
    }
    out
}

/// Queues the purges of these public slugs' URLs, at most 30 a message (the
/// purge op's own bound). A failed send leaves the cache bound to clear them.
pub async fn queue_purges(io: &impl Io, origins: &Origins, slugs: &[String]) {
    let urls = purge_urls(origins, slugs);
    let msgs: Vec<QueueMessage> = urls.chunks(30).map(|c| QueueMessage::new(Job::Purge { urls: c.to_vec() })).collect();
    if io.enqueue(msgs).await.is_err() {
        io.metric("error", &["purge_enqueue"]);
    }
}

/// `POST /api/detect/v1/removal`: the form's two steps.
pub async fn removal(
    io: &impl Io,
    keys: Keys<'_>,
    origins: &Origins,
    principal: Option<Principal>,
    req: &RemovalRequest,
) -> Result<RemovalAnswer, RefusalCode> {
    if !req.track.is_form_track() {
        return Err(RefusalCode::MalformedRequest);
    }
    let mut check = check_fields(req.track, &req.fields)?;
    let target = find_target(io, keys, origins, &req.target).await?;
    match &target {
        None => check.missing.push("target".into()),
        Some(t) if req.track == CaseKind::CounterNotice && !t.is_public() => {
            // Only someone who directed the storage counter-notifies: the
            // page's manage token, or the account that owns it.
            let by_token = t.row.manage_ok(req.fields.get(MANAGE_FIELD).map(|s| s.trim()));
            let by_owner = principal
                .is_some_and(|p| t.row.owner_h.as_ref().is_some_and(|o| ct_eq(o, &keys.store.owner_h(&p.user_id.0))));
            if !by_token && !by_owner {
                check.missing.push(MANAGE_FIELD.into());
            }
        }
        Some(_) => {}
    }
    if req.step == RemovalStep::Review {
        return Ok(RemovalAnswer::Review(RemovalReviewAnswer {
            complete: check.missing.is_empty(),
            missing: check.missing,
        }));
    }
    let (Some(t), true) = (target, check.missing.is_empty()) else { return Err(RefusalCode::RemovalIncomplete) };
    send(io, keys, origins, principal, req, t).await.map(RemovalAnswer::Received)
}

/// The automatic first step a request takes.
fn auto_step(req: &RemovalRequest, t: &Target) -> Option<AutoStep> {
    let state = t.row.state;
    match req.track {
        CaseKind::Copyright if t.is_public() && !t.is_x() && state == DetState::Live => Some(AutoStep::ExcerptWithheld),
        CaseKind::Illegal if !t.is_public() && matches!(state, DetState::Live | DetState::Pending) => {
            Some(AutoStep::Disabled)
        }
        // A result to re-check: a shown page (a pending or failed page names
        // only its domain-only source, which is no page to fetch).
        CaseKind::WrongResult
            if t.is_public()
                && !t.is_x()
                && matches!(state, DetState::Live | DetState::ExcerptWithheld)
                && t.row.source_id.is_some() =>
        {
            Some(AutoStep::Recheck)
        }
        CaseKind::Author if t.is_x() && !state.is_ended() => Some(AutoStep::Removed),
        _ => None,
    }
}

async fn send(
    io: &impl Io,
    keys: Keys<'_>,
    origins: &Origins,
    principal: Option<Principal>,
    req: &RemovalRequest,
    t: Target,
) -> Result<RemovalReceived, RefusalCode> {
    let now = io.now_ms();
    let case_id = draw_case_id(io);
    let token = {
        let mut fill = |b: &mut [u8]| io.fill(b);
        slug::draw_token(&mut fill)
    };
    let mut fields = req.fields.clone();
    // The manage token proves the counter-notice and is never kept: not as
    // its field, nor where the claimant pasted it again (a manage link).
    if let Some(given) = fields.remove(MANAGE_FIELD) {
        let token = match slug::fold(given.trim(), TOKEN_LEN) {
            Folded::Canonical => Some(given.trim().to_owned()),
            Folded::Fold(c) => Some(c),
            Folded::Invalid => None,
        };
        if let Some(token) = token {
            for v in fields.values_mut() {
                *v = mask_folded(v, &token, TOKEN_MASK);
            }
        }
    }
    let notice = Notice {
        v: NOTICE_V,
        track: req.track,
        target: t.url(origins),
        secret: !t.is_public(),
        fields,
        received_at: now,
        signed_in: principal.is_some(),
    };
    let plain = serde_json::to_vec(&notice).map_err(|_| RefusalCode::Internal)?;
    let mut nonce = [0u8; NONCE_BYTES];
    io.fill(&mut nonce);
    let sealed = seal_with(keys.store.cases_key(), &nonce, CASE_AAD, &plain);
    drop(plain);
    io.doc_put(&notice_key(&case_id), sealed).await.map_err(|_| RefusalCode::Unavailable)?;

    let auto = auto_step(req, &t);
    let submitter = principal.map(|p| keys.store.owner_h(&p.user_id.0));
    let due = (req.track == CaseKind::CounterNotice && !t.is_public()).then(|| now.saturating_add(COUNTER_WAIT_MS));
    let mut batch = vec![
        Stmt::new(
            "INSERT INTO cases (case_id, status_h, kind, state, submitter_h, received_at, updated_at, due_at, \
notice_key) VALUES (?1, ?2, ?3, 'received', ?4, ?5, ?5, ?6, ?7)",
            vec![
                Val::text(&case_id),
                Val::blob(&status_h(&token)),
                Val::text(req.track.as_str()),
                Val::opt_blob(submitter.as_ref().map(|s| s.as_slice())),
                Val::int(now),
                Val::opt_int(due),
                Val::text(notice_key(&case_id)),
            ],
        ),
        Stmt::new(
            "INSERT INTO case_targets (case_id, det_id_h) VALUES (?1, ?2)",
            vec![Val::text(&case_id), Val::blob(&t.row.id_h)],
        ),
    ];
    if due.is_some() {
        // The copyright cases that removed this page are now countered.
        batch.push(Stmt::owned(
            format!(
                "UPDATE cases SET state = 'countered', updated_at = ?2 WHERE kind = 'copyright' AND state IN {} \
AND case_id IN (SELECT case_id FROM case_targets WHERE det_id_h = ?1)",
                from_states(CaseState::Countered)
            ),
            vec![Val::blob(&t.row.id_h), Val::int(now)],
        ));
    }
    let mut purge = Vec::new();
    match auto {
        Some(AutoStep::ExcerptWithheld) => {
            batch.extend(withhold_stmts(&t.row, &t.slug, req.track.as_str(), DEFAULT_JURISDICTION, now));
            purge.push(t.slug.clone());
        }
        Some(AutoStep::Disabled) => batch.push(disable_family(&t.row, now)),
        Some(AutoStep::Removed) => {
            // The X removal runs this case's batch itself.
            let canon_key = x_source_of(io, &t.row).await?.ok_or(RefusalCode::Unavailable)?;
            let reason = XReason::Author;
            let r = x_removal(
                io,
                origins,
                &canon_key,
                reason,
                XCase::Existing { case_id: &case_id, insert: batch },
                vec![auto_x_journal(&canon_key, reason, now)],
            )
            .await;
            if r.is_err() {
                let _ = io.doc_delete(&[notice_key(&case_id)]).await;
            }
            r?;
            io.metric("removal", &[req.track.as_str(), "x_removed"]);
            legal_notice(io, &case_id, req.track).await;
            return Ok(RemovalReceived { case_id, status_token: token, auto });
        }
        Some(AutoStep::Recheck) | None => {}
    }
    if io.batch(batch).await.is_err() {
        // The case did not happen: its notice goes too.
        let _ = io.doc_delete(&[notice_key(&case_id)]).await;
        return Err(RefusalCode::Unavailable);
    }
    if auto == Some(AutoStep::Recheck)
        && let Some(source_id) = t.row.source_id
        && io.enqueue(vec![QueueMessage::new(Job::RerunOp { source_id })]).await.is_err()
    {
        io.metric("error", &["recheck_enqueue"]);
    }
    if !purge.is_empty() {
        queue_purges(io, origins, &purge).await;
    }
    io.metric("removal", &[req.track.as_str(), "received"]);
    if let Some(a) = auto {
        io.metric("removal", &[req.track.as_str(), a.as_str()]);
    }
    legal_notice(io, &case_id, req.track).await;
    Ok(RemovalReceived { case_id, status_token: token, auto })
}

async fn legal_notice(io: &impl Io, case_id: &str, kind: CaseKind) {
    let msg = QueueMessage::new(Job::LegalNotice { case_id: case_id.to_owned(), kind });
    if io.enqueue(vec![msg]).await.is_err() {
        // The case and its notice stand (the operator's `status` lists the
        // case); the alert path raises the failed send.
        io.metric("error", &["legal_enqueue"]);
    }
}

/// `POST /api/detect/v1/removal/status`: a case's state and dates, never
/// its notice. A token of the wrong form reads nothing.
pub async fn removal_status(io: &impl Io, token: &str) -> Result<RemovalStatusAnswer, RefusalCode> {
    if !slug::is_canonical(token, TOKEN_LEN) {
        return Err(RefusalCode::StatusUnknown);
    }
    let d = io
        .run(Stmt::new(
            "SELECT case_id, state, received_at, updated_at, due_at FROM cases WHERE status_h = ?1",
            vec![Val::blob(&status_h(token))],
        ))
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    let r = d.first().ok_or(RefusalCode::StatusUnknown)?;
    Ok(RemovalStatusAnswer {
        case_id: r.text("case_id").ok_or(RefusalCode::Internal)?.to_owned(),
        state: r.text("state").and_then(CaseState::parse).ok_or(RefusalCode::Internal)?,
        received_at: r.u64("received_at").ok_or(RefusalCode::Internal)?,
        updated_at: r.u64("updated_at").ok_or(RefusalCode::Internal)?,
        due_at: r.u64("due_at"),
    })
}

// ------------------------------------------------------- page state changes

/// Marks a public page's sitemap shard for the next rebuild.
fn shard_dirty(id_h: &Bytes32) -> Stmt {
    Stmt::new(
        "INSERT INTO sitemap_shards (shard, dirty, urls, built_at) SELECT sitemap_seq / CAST(?2 AS INTEGER), 1, 0, NULL \
FROM detections WHERE id_h = ?1 AND sitemap_seq IS NOT NULL ON CONFLICT (shard) DO UPDATE SET dirty = 1",
        vec![Val::blob(id_h), Val::int(SHARD_URLS)],
    )
}

fn removal_row(public_slug: Option<&str>, scope: &str, basis: &str, jurisdiction: &str, now: u64) -> Stmt {
    Stmt::new(
        "INSERT INTO removals (at, public_slug, scope, basis, jurisdiction) VALUES (?1, ?2, ?3, ?4, ?5)",
        vec![Val::int(now), Val::opt_text(public_slug), Val::text(scope), Val::text(basis), Val::text(jurisdiction)],
    )
}

/// A live public page's excerpt withheld: the state, its removal record and
/// its shard. The run document is left as it is (the display withholds by
/// state), so a rejected notice can return the page as it was.
fn withhold_stmts(row: &DetRow, slug: &str, basis: &str, jurisdiction: &str, now: u64) -> Vec<Stmt> {
    vec![
        Stmt::new(
            "UPDATE detections SET state = 'excerpt_withheld', state_v = state_v + 1, updated_at = ?2 \
WHERE id_h = ?1 AND kind = 'public_url' AND platform <> 'x' AND state = 'live'",
            vec![Val::blob(&row.id_h), Val::int(now)],
        ),
        removal_row(Some(slug), "excerpt", basis, jurisdiction, now),
        shard_dirty(&row.id_h),
    ]
}

/// A secret page disabled pending review, with every live or pending member
/// of its re-run copy family (a disabled page cannot be re-run, so the
/// report cannot be copied around).
fn disable_family(row: &DetRow, now: u64) -> Stmt {
    let root = row.family_root();
    Stmt::new(
        "UPDATE detections SET state = 'disabled', state_v = state_v + 1, updated_at = ?2 \
WHERE (id_h = ?1 OR copy_of = ?1) AND kind IN ('private_text','private_paid') AND state IN ('live','pending')",
        vec![Val::blob(&root), Val::int(now)],
    )
}

/// How a secret page comes back from a review (reject or restore): live,
/// or `failed` (`job_stopped`) for a paid page that never had a document (its
/// job never scored: the disable or removal stopped it and its hold went
/// back, so nothing was charged). No paid page goes live without one.
const RETURN_SET: &str = "state = CASE WHEN kind = 'private_paid' AND doc_key IS NULL THEN 'failed' ELSE 'live' END, \
refusal = CASE WHEN kind = 'private_paid' AND doc_key IS NULL THEN COALESCE(refusal, 'job_stopped') ELSE refusal END";

/// One secret page back from `from` (`disabled` or `removed`) by
/// [`RETURN_SET`].
fn return_secret(id_h: &Bytes32, from: &str, now: u64) -> Stmt {
    Stmt::owned(
        format!(
            "UPDATE detections SET {RETURN_SET}, state_v = state_v + 1, updated_at = ?2 WHERE id_h = ?1 \
AND kind IN ('private_text','private_paid') AND state = '{from}'"
        ),
        vec![Val::blob(id_h), Val::int(now)],
    )
}

/// The other cases a review must leave a public page to: when a restore or
/// a reject would return a page from `removed` or `excerpt_withheld`, the
/// page stays as it is while any case but the op's own still holds it. A
/// case holds the page when it targets it and is `accepted` or `countered`
/// (its removal or withhold stands), or is a copyright case still awaiting
/// its decision (`received`, `incomplete`, `reviewing`: its notice withheld
/// the page at intake, or arrived while the page was already held). With
/// `closed_since`, a `closed` case closed at or after that time holds it as
/// an `accepted` one would: a closed case no longer says how it was decided,
/// so a restore keeps the page rather than lift a removal that stood when
/// the case closed. With `run`, such a decided case (`accepted`,
/// `countered`, or `closed` as above) on another page of the same run (the
/// same text) holds it too, since an accepted withhold covers every page of
/// the run. A case that is not the op's own but was restored or rejected
/// holds nothing. With `op_hold`, the operator's own removal outside any case
/// (`remove {slug}`, which sets the page's `op_hold_at`) holds it too: only
/// `restore {slug}` lifts that one, so it is the one op that leaves it out.
struct Others<'a> {
    /// The op's own cases, as a JSON list of case ids.
    own: &'a str,
    run: Option<u64>,
    closed_since: Option<u64>,
    op_hold: bool,
}

impl Others<'_> {
    /// The holding condition on the page `?{p}`, with its four values at
    /// `?{p+1}` to `?{p+4}` (`Others::vals`).
    fn sql(p: usize) -> String {
        let (run, own, since, op) = (p + 1, p + 2, p + 3, p + 4);
        format!(
            "(EXISTS (SELECT 1 FROM case_targets ht JOIN cases hc ON hc.case_id = ht.case_id \
WHERE hc.case_id NOT IN (SELECT value FROM json_each(?{own})) \
AND (ht.det_id_h = ?{p} OR ht.det_id_h IN (SELECT hs.id_h FROM detections hs WHERE hs.run_id = ?{run})) \
AND (hc.state IN ('accepted','countered') OR (hc.state = 'closed' AND hc.updated_at >= ?{since}) \
OR (ht.det_id_h = ?{p} AND hc.kind = 'copyright' AND hc.state IN ('received','incomplete','reviewing')))) \
OR (?{op} = 1 AND EXISTS (SELECT 1 FROM detections ho WHERE ho.id_h = ?{p} AND ho.op_hold_at IS NOT NULL)))"
        )
    }

    fn vals(&self, id_h: &Bytes32) -> [Val; 5] {
        [
            Val::blob(id_h),
            Val::opt_int(self.run),
            Val::text(self.own),
            Val::opt_int(self.closed_since),
            Val::int(u64::from(self.op_hold)),
        ]
    }

    /// Whether another case, or the operator's own removal, holds the page
    /// now.
    async fn hold(&self, io: &impl Io, id_h: &Bytes32) -> Result<bool, RefusalCode> {
        let d = io
            .run(Stmt::owned(format!("SELECT {} AS hit", Others::sql(1)), self.vals(id_h).into()))
            .await
            .map_err(|_| RefusalCode::Unavailable)?;
        Ok(d.first().and_then(|r| r.int("hit")).unwrap_or(1) != 0)
    }
}

/// The op's own case ids as the JSON list `Others` takes.
fn own_cases<'a>(ids: impl IntoIterator<Item = &'a str>) -> String {
    Value::Array(ids.into_iter().map(Value::from).collect()).to_string()
}

/// Whether a copyright case's notice withheld this page at intake: the
/// record its automatic first step wrote (scope `excerpt`, at the case's
/// `received_at`). The page was live then.
async fn intake_withheld(io: &impl Io, slug: &str, received_at: u64) -> Result<bool, RefusalCode> {
    let d = io
        .run(Stmt::new(
            "SELECT count(*) AS n FROM removals WHERE public_slug = ?1 AND scope = 'excerpt' AND at = ?2",
            vec![Val::text(slug), Val::int(received_at)],
        ))
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    Ok(d.first().and_then(|r| r.u64("n")).unwrap_or(0) > 0)
}

/// The state a review returns a public page from, as its SQL literal: the
/// state the op read (`removed` or `excerpt_withheld`), so a page whose state
/// changed since the read is left as it is; `None` for any other state.
fn returned_from(s: DetState) -> Option<&'static str> {
    match s {
        DetState::Removed => Some("removed"),
        DetState::ExcerptWithheld => Some("excerpt_withheld"),
        _ => None,
    }
}

/// A public page a review returns live from `from` ([`returned_from`]; its
/// report is kept), unless another case or the operator's own removal holds
/// it (`Others`). The operator's hold goes with the state change (only
/// `restore {slug}` returns a page that has one).
fn return_public_live(id_h: &Bytes32, from: &str, others: &Others<'_>, now: u64) -> Vec<Stmt> {
    let mut vals = vec![Val::blob(id_h), Val::int(now)];
    vals.extend(others.vals(id_h));
    vec![
        Stmt::owned(
            format!(
                "UPDATE detections SET state = 'live', op_hold_at = NULL, state_v = state_v + 1, updated_at = ?2 \
WHERE id_h = ?1 AND kind = 'public_url' AND state = '{from}' AND NOT {}",
                Others::sql(3)
            ),
            vals,
        ),
        shard_dirty(id_h),
    ]
}

/// A public page a review returns with no report to show (its run document
/// is gone, or no longer keeps its text): `failed` with `report_gone` (a
/// page that had failed before its removal keeps its own reason), never
/// live without a document, and no fetch is queued behind the reader's back.
/// Its run leaves the page; the run's `content_key` is cleared, so checking
/// the link (or the same text) again scores afresh instead of reusing the
/// run, and the run row goes once no page names it. The run document goes
/// after the batch ([`delete_unread_run_doc`]). Since `failed` is not a
/// legal state, the page no longer blocks new checks of its source. A page
/// another case holds (`Others`) is left as it is, run and all: the state
/// change is the first statement, and the run's statements apply only once
/// the page has left the run. As in [`return_public_live`], the page returns
/// only from `from`, and its operator's hold goes with the state change.
fn return_public_failed(id_h: &Bytes32, from: &str, run_id: Option<u64>, others: &Others<'_>, now: u64) -> Vec<Stmt> {
    let mut vals = vec![Val::blob(id_h), Val::int(now)];
    vals.extend(others.vals(id_h));
    let mut v = Vec::with_capacity(4);
    v.push(Stmt::owned(
        format!(
            "UPDATE detections SET state = 'failed', refusal = COALESCE(refusal, 'report_gone'), run_id = NULL, \
op_hold_at = NULL, state_v = state_v + 1, updated_at = ?2 WHERE id_h = ?1 AND kind = 'public_url' \
AND platform <> 'x' AND state = '{from}' AND NOT {}",
            Others::sql(3)
        ),
        vals,
    ));
    if let Some(r) = run_id {
        v.push(Stmt::new(
            "UPDATE runs SET content_key = NULL WHERE run_id = ?1 \
AND NOT EXISTS (SELECT 1 FROM detections WHERE id_h = ?2 AND run_id = ?1)",
            vec![Val::int(r), Val::blob(id_h)],
        ));
        v.push(Stmt::new(
            "DELETE FROM runs WHERE run_id = ?1 AND NOT EXISTS (SELECT 1 FROM detections WHERE run_id = ?1)",
            vec![Val::int(r)],
        ));
    }
    v.push(shard_dirty(id_h));
    v
}

/// Whether a public page's run document can show it again: present, and
/// still holding its text.
async fn report_kept(io: &impl Io, run_id: Option<u64>) -> Result<bool, RefusalCode> {
    let Some(r) = run_id else { return Ok(false) };
    match io.doc_get(&run_doc_key(r)).await.map_err(|_| RefusalCode::Unavailable)? {
        Some(b) => Ok(RunDoc::decode(&b).is_ok_and(|d| !is_withheld_doc(&d))),
        None => Ok(false),
    }
}

/// The source key of an X page's source, from its row.
async fn x_source_of(io: &impl Io, row: &DetRow) -> Result<Option<Bytes32>, RefusalCode> {
    let Some(sid) = row.source_id else { return Ok(None) };
    let d = io
        .run(Stmt::new(
            "SELECT hex(canon_key) AS canon_key FROM sources WHERE source_id = ?1 AND adapter = 'x'",
            vec![Val::int(sid)],
        ))
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    Ok(d.first().and_then(|r| r.hex32("canon_key")))
}

// ------------------------------------------------------------------ X removal

/// Why an X post's pages were removed (`x_remove`'s `reason`, and the
/// `removals.basis` of each page).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XReason {
    /// The post's author asked.
    Author,
    /// X asked.
    X,
    /// The operator's own removal (`remove` on an X page).
    Other,
}

impl XReason {
    pub fn as_str(self) -> &'static str {
        match self {
            XReason::Author => "author",
            XReason::X => "x",
            XReason::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<XReason> {
        [XReason::Author, XReason::X, XReason::Other].into_iter().find(|r| r.as_str() == s)
    }
}

/// The case an X removal belongs to.
pub enum XCase<'a> {
    /// A new content-free `x_request` case (`x_remove`, or `remove` outside a
    /// case), drawn here.
    New,
    /// The form's own case (its insert statements run first in the batch),
    /// or a case the operator named.
    Existing { case_id: &'a str, insert: Vec<Stmt> },
}

/// The removal's result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XRemoved {
    pub case_id: String,
    /// Detections removed now.
    pub removed: u64,
}

/// `x_remove`'s journal row when no signed op carries it (the form, and
/// `remove`): `auto:x_tombstone`, unsigned.
pub fn auto_x_journal(canon_key: &Bytes32, reason: XReason, now: u64) -> Stmt {
    let op =
        canonical(&json!({"op": "x_remove", "args": {"reason": reason.as_str(), "canon_key": hex::encode(canon_key)}}));
    Stmt::new(
        "INSERT INTO op_journal (at, actor, op, signature, nonce_h) VALUES (?1, 'auto:x_tombstone', ?2, NULL, NULL)",
        vec![Val::int(now), Val::text(op)],
    )
}

/// The canonical JSON the journal keeps.
pub fn canonical(v: &Value) -> String {
    slop_ninja_signed_op::canonical_json(v).unwrap_or_default()
}

/// The removal records of the pages this removal moved to `removed` (`?1`
/// its time, `?2` the post's `canon_key`, `?3` the basis, `?4` the
/// jurisdiction).
const X_RECORD_REMOVALS: &str = "INSERT INTO removals (at, public_slug, scope, basis, jurisdiction) \
SELECT ?1, d.public_slug, 'page', ?3, ?4 FROM detections d \
WHERE d.source_id = (SELECT source_id FROM sources WHERE canon_key = ?2 AND adapter = 'x') AND d.state = 'removed' \
AND d.refusal = 'x_removed' AND d.updated_at = ?1 AND d.public_slug IS NOT NULL \
AND NOT EXISTS (SELECT 1 FROM removals r WHERE r.public_slug = d.public_slug AND r.at = ?1 AND r.scope = 'page')";
/// The case's targets: every X page of the post (`?1` the case, `?2` the
/// post's `canon_key`).
const X_CASE_TARGETS: &str = "INSERT OR IGNORE INTO case_targets (case_id, det_id_h) SELECT ?1, d.id_h \
FROM detections d WHERE d.source_id = (SELECT source_id FROM sources WHERE canon_key = ?2 AND adapter = 'x') \
AND d.platform = 'x'";

/// Every page about the post whose source key hashes to `canon_key` goes,
/// as one removal, whether from the form, from `remove` or from `x_remove`.
/// A post we never held still gets its tombstone, so no new check of it can
/// start.
///
/// 1. The tombstone first, in its own batch with the read of its id
///    ([`x_flow::UPSERT_TOMBSTONE`], [`x_flow::SELECT_BY_KEY`]). From here
///    the URL check's store fence (`url_flow::X_BLOCKED`) ends every check
///    of the post that has not stored yet `failed` (`x_removed`), keeping
///    nothing of it; a check that stored before is in the set read next.
/// 2. The deletion ([`x_flow::delete_set_with`], cause
///    [`x_flow::Cause::Removed`]): the run documents and page slugs are
///    read, the documents deleted from R2, then one D1 batch: the signed
///    op's journal row first (so its seq is the batch's journal row), the
///    case's own insert statements, the detections (`removed`, content-free
///    `x_removed` tombstones), only the runs it read, the source's aliases,
///    the source reduced to the opt-out tombstone that keeps only
///    `canon_key`, the removal records of the pages it moved, the case's
///    targets, its `source_id` and state, then the rest of `journal`.
/// 3. A last read of the tombstone ([`x_flow::settle`]): a live page or a
///    run still on it (none is expected) is deleted the same way, and
///    failing that `x_delete` finishes it.
///
/// When the R2 delete or the batch of step 2 fails, nothing of that batch is
/// written, an `x_delete` message finishes the deletion and the caller
/// answers `unavailable` (the tombstone stays; the operator's retry is safe:
/// each step repeats).
pub async fn x_removal(
    io: &impl Io,
    origins: &Origins,
    canon_key: &Bytes32,
    reason: XReason,
    case: XCase<'_>,
    journal: Vec<Stmt>,
) -> Result<XRemoved, RefusalCode> {
    let now = io.now_ms();
    let key = Val::blob(canon_key);
    let found = io
        .batch(vec![
            Stmt::new(x_flow::UPSERT_TOMBSTONE, vec![key.clone()]),
            Stmt::new(x_flow::SELECT_BY_KEY, vec![key.clone()]),
        ])
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    let Some(source_id) = found.get(1).and_then(Done::first).and_then(|r| r.u64("source_id")) else {
        return Err(RefusalCode::Unavailable);
    };
    let (case_id, insert) = match case {
        XCase::New => {
            let id = draw_case_id(io);
            let insert = Stmt::new(
                "INSERT INTO cases (case_id, status_h, kind, state, received_at, updated_at, notice_key) \
VALUES (?1, NULL, 'x_request', 'accepted', ?2, ?2, NULL)",
                vec![Val::text(&id), Val::int(now)],
            );
            (id, vec![insert])
        }
        XCase::Existing { case_id, insert } => (case_id.to_owned(), insert),
    };
    let mut journal = journal.into_iter();
    let mut pre: Vec<Stmt> = journal.next().into_iter().collect();
    pre.extend(insert);
    let n_pre = pre.len();
    let records = || {
        vec![
            Stmt::new(
                X_RECORD_REMOVALS,
                vec![Val::int(now), key.clone(), Val::text(reason.as_str()), Val::text(DEFAULT_JURISDICTION)],
            ),
            Stmt::new(X_CASE_TARGETS, vec![Val::text(&case_id), key.clone()]),
        ]
    };
    let mut post = records();
    post.extend([
        Stmt::new(
            "UPDATE cases SET source_id = (SELECT source_id FROM sources WHERE canon_key = ?2), updated_at = ?3 \
WHERE case_id = ?1",
            vec![Val::text(&case_id), key.clone(), Val::int(now)],
        ),
        Stmt::owned(
            format!(
                "UPDATE cases SET state = 'accepted' WHERE case_id = ?1 AND state IN {}",
                from_states(CaseState::Accepted)
            ),
            vec![Val::text(&case_id)],
        ),
    ]);
    post.extend(journal);
    let ids = [source_id];
    let (deleted, done) = match x_flow::delete_set_with(io, &ids, x_flow::Cause::Removed, now, pre, post).await {
        Ok(r) => r,
        Err(IoError) => {
            let msg = QueueMessage::new(Job::XDelete {
                source_id,
                cause: slop_ninja_detect_page::codes::XDeleteCause::XRemoved,
            });
            let _ = io.enqueue(vec![msg]).await;
            return Err(RefusalCode::Unavailable);
        }
    };
    // The deletion's first statement moved the pages to `removed`.
    let removed = done.get(n_pre).map_or(0, |d| d.changes);
    queue_purges(io, origins, &deleted.slugs).await;
    x_flow::settle(io, origins, source_id, now, records()).await;
    Ok(XRemoved { case_id, removed })
}

/// `restore` of an X case: the opt-out tombstone its removal left is
/// lifted, in foreign-key order, and nothing more (the removed pages stay
/// removed; the post can be checked again as a new check). `Ok(false)` when
/// the case's `source_id` is already NULL.
pub fn x_restore_stmts(case_id: &str, source_id: u64, now: u64) -> Vec<Stmt> {
    vec![
        Stmt::new("UPDATE detections SET source_id = NULL WHERE source_id = ?1", vec![Val::int(source_id)]),
        Stmt::new("UPDATE cases SET source_id = NULL WHERE source_id = ?1", vec![Val::int(source_id)]),
        Stmt::owned(
            format!(
                "UPDATE cases SET state = 'restored', updated_at = ?2 WHERE case_id = ?1 AND state IN {}",
                from_states(CaseState::Restored)
            ),
            vec![Val::text(case_id), Val::int(now)],
        ),
        Stmt::new(
            "DELETE FROM sources WHERE source_id = ?1 AND adapter = 'x' AND state = 'opted_out' AND canon_url IS NULL",
            vec![Val::int(source_id)],
        ),
    ]
}

// ------------------------------------------------------------- case reads

/// A case row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaseRow {
    pub case_id: String,
    pub kind: CaseKind,
    pub state: CaseState,
    pub received_at: u64,
    pub updated_at: u64,
    pub due_at: Option<u64>,
    /// The X tombstone this case's removal left.
    pub source_id: Option<u64>,
    #[serde(skip)]
    pub notice_key: Option<String>,
}

/// Reads a case; `not_found` for an id of another form or no such case.
pub async fn read_case(io: &impl Io, case_id: &str) -> Result<CaseRow, RefusalCode> {
    if !is_case_id(case_id) {
        return Err(RefusalCode::OpMalformed);
    }
    let d = io
        .run(Stmt::new(
            "SELECT case_id, kind, state, received_at, updated_at, due_at, source_id, notice_key FROM cases \
WHERE case_id = ?1",
            vec![Val::text(case_id)],
        ))
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    case_row(d.first().ok_or(RefusalCode::NotFound)?)
}

fn case_row(r: &crate::db::Row) -> Result<CaseRow, RefusalCode> {
    Ok(CaseRow {
        case_id: r.text("case_id").ok_or(RefusalCode::Internal)?.to_owned(),
        kind: r.text("kind").and_then(CaseKind::parse).ok_or(RefusalCode::Internal)?,
        state: r.text("state").and_then(CaseState::parse).ok_or(RefusalCode::Internal)?,
        received_at: r.u64("received_at").ok_or(RefusalCode::Internal)?,
        updated_at: r.u64("updated_at").ok_or(RefusalCode::Internal)?,
        due_at: r.u64("due_at"),
        source_id: r.u64("source_id"),
        notice_key: r.text("notice_key").map(str::to_owned),
    })
}

/// The countered copyright cases a counter-notice answers: those holding a
/// page among its targets (a removal names the whole re-run family among
/// its targets, so any member the counter-notice names finds it).
pub async fn countered_behind(io: &impl Io, counter_notice: &str) -> Result<Vec<CaseRow>, RefusalCode> {
    let d = io
        .run(Stmt::new(
            "SELECT DISTINCT c.case_id AS case_id, c.kind AS kind, c.state AS state, c.received_at AS received_at, \
c.updated_at AS updated_at, c.due_at AS due_at, c.source_id AS source_id, c.notice_key AS notice_key \
FROM case_targets n JOIN case_targets t ON t.det_id_h = n.det_id_h JOIN cases c ON c.case_id = t.case_id \
WHERE n.case_id = ?1 AND c.kind = 'copyright' AND c.state = 'countered' ORDER BY c.case_id",
            vec![Val::text(counter_notice)],
        ))
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    d.rows.iter().map(case_row).collect()
}

/// After a copyright case is restored (earlier in the same batch): each
/// counter-notice on its pages has had its restore once no countered case
/// is left behind it, so it is restored too and leaves `restores_due`.
fn counter_notices_restored(case_id: &str, now: u64) -> Stmt {
    Stmt::owned(
        format!(
            "UPDATE cases SET state = 'restored', updated_at = ?2 WHERE kind = 'counter_notice' AND state IN {} \
AND case_id IN (SELECT n.case_id FROM case_targets n JOIN case_targets t ON t.det_id_h = n.det_id_h \
WHERE t.case_id = ?1) AND NOT EXISTS (SELECT 1 FROM case_targets n JOIN case_targets t ON t.det_id_h = n.det_id_h \
JOIN cases c ON c.case_id = t.case_id WHERE n.case_id = cases.case_id AND c.kind = 'copyright' \
AND c.state = 'countered')",
            from_states(CaseState::Restored)
        ),
        vec![Val::text(case_id), Val::int(now)],
    )
}

/// A case's target pages.
pub async fn case_targets(io: &impl Io, case_id: &str) -> Result<Vec<DetRow>, RefusalCode> {
    let sql = format!(
        "SELECT {DET_COLUMNS} FROM case_targets t JOIN detections d ON d.id_h = t.det_id_h \
LEFT JOIN detections n ON n.id_h = d.superseded_by WHERE t.case_id = ?1 ORDER BY d.id_h"
    );
    let d = io.run(Stmt::owned(sql, vec![Val::text(case_id)])).await.map_err(|_| RefusalCode::Unavailable)?;
    d.rows.iter().map(|r| DetRow::from_row(r).map_err(|_| RefusalCode::Internal)).collect()
}

/// A secret page's family (its root and the root's copies), in id order.
async fn family(io: &impl Io, row: &DetRow) -> Result<Vec<DetRow>, RefusalCode> {
    let sql = format!(
        "SELECT {DET_COLUMNS} FROM detections d LEFT JOIN detections n ON n.id_h = d.superseded_by \
WHERE (d.id_h = ?1 OR d.copy_of = ?1) AND d.kind IN ('private_text','private_paid') ORDER BY d.id_h"
    );
    let d =
        io.run(Stmt::owned(sql, vec![Val::blob(&row.family_root())])).await.map_err(|_| RefusalCode::Unavailable)?;
    d.rows.iter().map(|r| DetRow::from_row(r).map_err(|_| RefusalCode::Internal)).collect()
}

/// The case's new state, from the states it may come from.
pub fn case_state_stmt(case_id: &str, to: CaseState, now: u64) -> Stmt {
    Stmt::owned(
        format!("UPDATE cases SET state = ?2, updated_at = ?3 WHERE case_id = ?1 AND state IN {}", from_states(to)),
        vec![Val::text(case_id), Val::text(to.as_str()), Val::int(now)],
    )
}

/// Strikes for an accepted copyright case: one per owner of its targets,
/// carrying the page's `owner_via`.
fn strike_stmts(case: &CaseRow, targets: &[DetRow], now: u64) -> Vec<Stmt> {
    if case.kind != CaseKind::Copyright {
        return Vec::new();
    }
    targets
        .iter()
        .filter_map(|t| Some((t.owner_h?, t.owner_via?)))
        .map(|(o, via)| {
            Stmt::new(
                "INSERT OR IGNORE INTO strikes (owner_h, case_id, at, owner_via) VALUES (?1, ?2, ?3, ?4)",
                vec![Val::blob(&o), Val::text(&case.case_id), Val::int(now), Val::text(via.as_str())],
            )
        })
        .collect()
}

// -------------------------------------------------------------- the effects

/// What an op did, for its answer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Effect {
    /// Pages whose state changed.
    pub pages: u64,
    /// Public pages a restore returned `failed` (`report_gone`) because their
    /// report was no longer kept; their links can be checked again.
    #[serde(skip_serializing_if = "is_zero")]
    pub report_gone: u64,
    /// Public pages a restore or reject left as they were because another
    /// case still holds them, or (a reject) because an accepted withhold
    /// took their text; for a restore by case, also removed pages the
    /// operator's `remove {slug}` holds or the restored case cannot have
    /// removed. A page no open case holds can still be returned by
    /// `restore {slug}`.
    #[serde(skip_serializing_if = "is_zero")]
    pub on_hold: u64,
    /// Documents an R2 step left for a retry of the same op.
    #[serde(skip_serializing_if = "is_zero")]
    pub docs_pending: u64,
    /// The X removal's case, when one ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x_case_id: Option<String>,
    /// The countered cases a counter-notice's `restore` restored.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restored_cases: Vec<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// The arguments of a removal op that name its record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Basis {
    pub basis: String,
    pub jurisdiction: String,
}

impl Basis {
    /// From an op's `basis` and `jurisdiction` (each optional: the case's
    /// kind and [`DEFAULT_JURISDICTION`]).
    pub fn of(args: &Map<String, Value>, kind: Option<CaseKind>) -> Result<Basis, RefusalCode> {
        let basis = match args.get("basis") {
            None => kind.map_or("other", CaseKind::as_str).to_owned(),
            Some(Value::String(s))
                if !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') =>
            {
                s.clone()
            }
            Some(_) => return Err(RefusalCode::OpMalformed),
        };
        let jurisdiction = match args.get("jurisdiction") {
            None => DEFAULT_JURISDICTION.to_owned(),
            Some(Value::String(s)) if (2..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_uppercase()) => {
                s.clone()
            }
            Some(_) => return Err(RefusalCode::OpMalformed),
        };
        Ok(Basis { basis, jurisdiction })
    }
}

/// `withhold_excerpt {case_id}`: every live public page among the case's
/// targets has its excerpt withheld, and so has every other live public page
/// of the same run (the same text); then each run document is rewritten
/// without its text (its result stays, so the page keeps its score). The
/// case is accepted.
pub async fn withhold_excerpt(
    io: &impl Io,
    origins: &Origins,
    case: &CaseRow,
    basis: &Basis,
    journal: Stmt,
) -> Result<Effect, RefusalCode> {
    let now = io.now_ms();
    let targets = case_targets(io, &case.case_id).await?;
    let mut runs = BTreeSet::new();
    for t in targets.iter().filter(|t| t.kind == Kind::PublicUrl && t.platform != Platform::X) {
        if matches!(t.state, DetState::Live | DetState::ExcerptWithheld)
            && let Some(r) = t.run_id
        {
            runs.insert(r);
        }
    }
    let runs_json = Value::Array(runs.iter().map(|r| Value::from(*r)).collect()).to_string();
    let pages = io
        .run(Stmt::new(
            "SELECT hex(id_h) AS id_h, public_slug FROM detections WHERE run_id IN (SELECT value FROM json_each(?1)) \
AND kind = 'public_url' AND platform <> 'x' AND state = 'live' ORDER BY id_h",
            vec![Val::text(&runs_json)],
        ))
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    let mut batch = vec![journal];
    let mut slugs = Vec::new();
    for r in &pages.rows {
        let (Some(id), Some(slug)) = (r.hex32("id_h"), r.text("public_slug")) else { continue };
        let row_stub = stub_row(id);
        batch.extend(withhold_stmts(&row_stub, slug, &basis.basis, &basis.jurisdiction, now));
        slugs.push(slug.to_owned());
    }
    // Pages withheld at intake (pending review) keep their record.
    batch.push(case_state_stmt(&case.case_id, CaseState::Accepted, now));
    batch.extend(strike_stmts(case, &targets, now));
    io.batch(batch).await.map_err(|_| RefusalCode::Unavailable)?;
    let mut pending = 0;
    for run in &runs {
        if rewrite_without_text(io, *run).await.is_err() {
            pending += 1;
        }
    }
    queue_purges(io, origins, &slugs).await;
    io.metric("removal", &[case.kind.as_str(), "excerpt_withheld"]);
    Ok(Effect { pages: slugs.len() as u64, docs_pending: pending, ..Effect::default() })
}

/// A row with only its id, for the statements that take one.
fn stub_row(id_h: Bytes32) -> DetRow {
    DetRow {
        id_h,
        pepper_v: None,
        public_slug: None,
        kind: Kind::PublicUrl,
        platform: Platform::Web,
        state: DetState::Live,
        state_v: 1,
        refusal: None,
        created_at: 0,
        updated_at: 0,
        expires_at: None,
        source_id: None,
        run_id: None,
        superseded_by: None,
        superseded_slug: None,
        package_load: None,
        manage_h: None,
        owner_h: None,
        owner_via: None,
        doc_key: None,
        index_force: None,
        copy_of: None,
        expiry_cap: None,
    }
}

/// Whether a run document no longer keeps its text.
pub fn is_withheld_doc(d: &RunDoc) -> bool {
    d.body.is_empty() && d.anchors.is_empty() && d.further.is_empty()
}

/// Rewrites a run document without its text: no body, anchors or listed
/// passages; its result, words and page hash stay. Repeating it changes
/// nothing; a missing document is left missing.
pub async fn rewrite_without_text(io: &impl Io, run_id: u64) -> Result<(), IoError> {
    let key = run_doc_key(run_id);
    let Some(b) = io.doc_get(&key).await? else { return Ok(()) };
    let mut d = RunDoc::decode(&b).map_err(|_| IoError)?;
    drop(b);
    if is_withheld_doc(&d) {
        return Ok(());
    }
    d.body.clear();
    d.anchors.clear();
    d.further.clear();
    io.doc_put(&key, d.encode()).await
}

/// `remove {case_id}`: every target goes. A public page answers 451 from
/// now on (its run document is deleted when no other shown page reads it);
/// a secret page and its re-run copies move their documents to quarantine;
/// an X page takes the X removal, with this case. The case is accepted, and
/// a copyright case's owned targets leave strikes.
pub async fn remove_case(
    io: &impl Io,
    keys: &StoreKeys,
    origins: &Origins,
    case: &CaseRow,
    basis: &Basis,
    journal: Stmt,
) -> Result<Effect, RefusalCode> {
    let now = io.now_ms();
    let targets = case_targets(io, &case.case_id).await?;
    let mut effect = Effect::default();
    let mut journal = Some(journal);
    // X pages first: their removal journals the signed op itself.
    for t in targets.iter().filter(|t| t.platform == Platform::X && !t.state.is_ended()) {
        let Some(canon_key) = x_source_of(io, t).await? else { continue };
        let mut rows = Vec::new();
        rows.extend(journal.take());
        rows.push(auto_x_journal(&canon_key, XReason::Other, now));
        let r = x_removal(
            io,
            origins,
            &canon_key,
            XReason::Other,
            XCase::Existing { case_id: &case.case_id, insert: Vec::new() },
            rows,
        )
        .await?;
        effect.pages += r.removed;
        effect.x_case_id = Some(r.case_id);
    }
    let mut batch: Vec<Stmt> = journal.into_iter().collect();
    let mut slugs = Vec::new();
    let mut runs = BTreeSet::new();
    let mut quarantined = Vec::new();
    let mut seen = BTreeSet::new();
    for t in targets.iter().filter(|t| t.platform != Platform::X) {
        if t.kind == Kind::PublicUrl {
            if !matches!(t.state, DetState::Live | DetState::ExcerptWithheld | DetState::Pending | DetState::Failed) {
                continue;
            }
            batch.push(Stmt::new(
                "UPDATE detections SET state = 'removed', state_v = state_v + 1, updated_at = ?2 WHERE id_h = ?1 \
AND kind = 'public_url' AND state IN ('live','excerpt_withheld','pending','failed')",
                vec![Val::blob(&t.id_h), Val::int(now)],
            ));
            let slug = t.public_slug.clone().unwrap_or_default();
            batch.push(removal_row(Some(&slug), "page", &basis.basis, &basis.jurisdiction, now));
            batch.push(shard_dirty(&t.id_h));
            slugs.push(slug);
            if let Some(r) = t.run_id {
                runs.insert(r);
            }
            effect.pages += 1;
        } else {
            for m in family(io, t).await? {
                // A family is read once, whichever of its pages the case names.
                if !seen.insert(m.id_h)
                    || !matches!(m.state, DetState::Live | DetState::Pending | DetState::Disabled | DetState::Failed)
                {
                    continue;
                }
                // Quarantine first: a copy that cannot be kept is not removed.
                if quarantine(io, keys, &case.case_id, &m.id_h).await.is_err() {
                    for id in &quarantined {
                        let _ = io.doc_delete(&[quarantine_key(&case.case_id, id)]).await;
                    }
                    return Err(RefusalCode::Unavailable);
                }
                quarantined.push(m.id_h);
                batch.push(Stmt::new(
                    "UPDATE detections SET state = 'removed', state_v = state_v + 1, updated_at = ?2 WHERE id_h = ?1 \
AND kind IN ('private_text','private_paid') AND state IN ('live','pending','disabled','failed')",
                    vec![Val::blob(&m.id_h), Val::int(now)],
                ));
                batch.push(Stmt::new(
                    "INSERT OR IGNORE INTO case_targets (case_id, det_id_h) VALUES (?1, ?2)",
                    vec![Val::text(&case.case_id), Val::blob(&m.id_h)],
                ));
                batch.push(removal_row(None, "page", &basis.basis, &basis.jurisdiction, now));
                effect.pages += 1;
            }
        }
    }
    batch.push(case_state_stmt(&case.case_id, CaseState::Accepted, now));
    batch.extend(strike_stmts(case, &targets, now));
    if io.batch(batch).await.is_err() {
        for id in &quarantined {
            let _ = io.doc_delete(&[quarantine_key(&case.case_id, id)]).await;
        }
        return Err(RefusalCode::Unavailable);
    }
    // The live documents go only now that the pages answer 451.
    let live: Vec<String> = quarantined.iter().map(secret_doc_key).collect();
    if delete_docs(io, &live).await.is_err() {
        effect.docs_pending += live.len() as u64;
    }
    for run in runs {
        if delete_unread_run_doc(io, run).await.is_err() {
            effect.docs_pending += 1;
        }
    }
    queue_purges(io, origins, &slugs).await;
    io.metric("removal", &[case.kind.as_str(), "removed"]);
    Ok(effect)
}

/// The operator's hold on a public page that `remove {slug}` removed or
/// found removed (`op_hold_at`, first time kept): no restore by case returns
/// the page while it stands, and only `restore {slug}` clears it.
fn op_hold_stmt(id_h: &Bytes32, now: u64) -> Stmt {
    Stmt::new(
        "UPDATE detections SET op_hold_at = COALESCE(op_hold_at, ?2) WHERE id_h = ?1 AND kind = 'public_url' \
AND platform <> 'x' AND state = 'removed'",
        vec![Val::blob(id_h), Val::int(now)],
    )
}

/// `remove {slug}` of a public page outside a case: an X page takes the X
/// removal with a new `x_request` case; any other public page is removed
/// with its record and no case, under the operator's hold
/// (`op_hold_stmt`), so restoring a case on the page never returns it:
/// only `restore {slug}` does. A page already removed (by a case) changes
/// state no further and writes no second record, but takes the hold too.
pub async fn remove_slug(
    io: &impl Io,
    origins: &Origins,
    slug: &str,
    basis: &Basis,
    journal: Stmt,
) -> Result<Effect, RefusalCode> {
    let now = io.now_ms();
    let (row, _) = public_lookup(io, slug).await.map_err(|_| RefusalCode::Unavailable)?;
    let row = row.ok_or(RefusalCode::NotFound)?;
    if row.platform == Platform::X {
        let canon_key = x_source_of(io, &row).await?.ok_or(RefusalCode::NotFound)?;
        let r = x_removal(
            io,
            origins,
            &canon_key,
            XReason::Other,
            XCase::New,
            vec![journal, auto_x_journal(&canon_key, XReason::Other, now)],
        )
        .await?;
        return Ok(Effect { pages: r.removed, x_case_id: Some(r.case_id), ..Effect::default() });
    }
    if row.state == DetState::Removed {
        // Already removed: the op is journaled and holds the page.
        io.batch(vec![journal, op_hold_stmt(&row.id_h, now)]).await.map_err(|_| RefusalCode::Unavailable)?;
        return Ok(Effect::default());
    }
    if !matches!(row.state, DetState::Live | DetState::ExcerptWithheld | DetState::Pending | DetState::Failed) {
        // Ended: the op is journaled and changes nothing.
        io.batch(vec![journal]).await.map_err(|_| RefusalCode::Unavailable)?;
        return Ok(Effect::default());
    }
    // The hold follows the state change, so a page a case removed since the
    // read takes it as well.
    let batch = vec![
        journal,
        Stmt::new(
            "UPDATE detections SET state = 'removed', state_v = state_v + 1, updated_at = ?2 WHERE id_h = ?1 \
AND kind = 'public_url' AND state IN ('live','excerpt_withheld','pending','failed')",
            vec![Val::blob(&row.id_h), Val::int(now)],
        ),
        op_hold_stmt(&row.id_h, now),
        removal_row(Some(slug), "page", &basis.basis, &basis.jurisdiction, now),
        shard_dirty(&row.id_h),
    ];
    let done = io.batch(batch).await.map_err(|_| RefusalCode::Unavailable)?;
    let changed = done.get(1).map_or(0, |d| d.changes);
    let mut effect = Effect { pages: changed, ..Effect::default() };
    if let Some(r) = row.run_id
        && delete_unread_run_doc(io, r).await.is_err()
    {
        effect.docs_pending += 1;
    }
    queue_purges(io, origins, &[slug.to_owned()]).await;
    Ok(effect)
}

/// Deletes a run's document when no shown page reads it any more.
async fn delete_unread_run_doc(io: &impl Io, run_id: u64) -> Result<(), IoError> {
    let d = io
        .run(Stmt::new(
            "SELECT count(*) AS n FROM detections WHERE run_id = ?1 AND state IN ('live','excerpt_withheld','pending')",
            vec![Val::int(run_id)],
        ))
        .await?;
    if d.first().and_then(|r| r.u64("n")).unwrap_or(1) > 0 {
        return Ok(());
    }
    io.doc_delete(&[run_doc_key(run_id)]).await
}

fn quarantine_aad(case_id: &str, id_h: &Bytes32) -> Vec<u8> {
    let mut aad = QUARANTINE_AAD.to_vec();
    aad.push(0);
    aad.extend_from_slice(case_id.as_bytes());
    aad.push(0);
    aad.extend_from_slice(id_h);
    aad
}

/// Copies a secret page's encrypted document into the case's quarantine,
/// wrapped again under `K_cases`. A page whose document is already gone has
/// nothing to keep.
async fn quarantine(io: &impl Io, keys: &StoreKeys, case_id: &str, id_h: &Bytes32) -> Result<(), IoError> {
    let Some(b) = io.doc_get(&secret_doc_key(id_h)).await? else { return Ok(()) };
    let mut nonce = [0u8; NONCE_BYTES];
    io.fill(&mut nonce);
    let wrapped = seal_with(keys.cases_key(), &nonce, &quarantine_aad(case_id, id_h), &b);
    io.doc_put(&quarantine_key(case_id, id_h), wrapped).await
}

/// Puts a quarantined document back as the page's own; `Ok(false)` when the
/// quarantine holds none.
async fn unquarantine(io: &impl Io, keys: &StoreKeys, case_id: &str, id_h: &Bytes32) -> Result<bool, IoError> {
    let Some(b) = io.doc_get(&quarantine_key(case_id, id_h)).await? else { return Ok(false) };
    let plain = open_with(keys.cases_key(), &quarantine_aad(case_id, id_h), &b).map_err(|_| IoError)?;
    io.doc_put(&secret_doc_key(id_h), plain).await?;
    Ok(true)
}

/// Whether a case can have removed a page: only `remove {case_id}` removes
/// one, and it accepts the case, so a case still awaiting its decision
/// (`received`, `incomplete`, `reviewing`) or `rejected` has removed none.
fn may_have_removed(s: CaseState) -> bool {
    !matches!(s, CaseState::Received | CaseState::Incomplete | CaseState::Reviewing | CaseState::Rejected)
}

/// `restore {case_id}`: an X case lifts its opt-out ([`x_restore_stmts`]);
/// otherwise each target returns: a disabled secret page (with its family)
/// and a removed one whose quarantined document is put back are live again;
/// a public page whose run document still holds its text is live again, and
/// one whose report is no longer kept (its document deleted at the removal,
/// or rewritten without its text) returns `failed` with `report_gone`
/// (`return_public_failed`), so its page says to check the link again or
/// paste the text. A public page another case still holds (any case but the
/// ones this op restores that targets it and is `accepted` or `countered`,
/// or is a copyright case awaiting its decision; one that is `accepted` or
/// `countered` on another page of its run; and one `closed` since the
/// restored case's own notice found the page live, or ever when it found
/// none) stays as it is, with its run and document, and counts as `on_hold`;
/// it returns with the last restore of the cases holding it, or by
/// `restore {slug}` once no open case holds it. So does a removed page this
/// op cannot have removed: one under the operator's own `remove {slug}`
/// (only `restore {slug}` returns it), and any removed target of a case that
/// never accepted a removal (`may_have_removed`). The case is restored, and
/// so is each counter-notice on its pages that no countered case is left
/// behind.
///
/// A counter-notice holds no removal of its own: its `restore` restores the
/// countered copyright cases on its targets (the cases whose removal and
/// quarantine hold the page, as `status` lists them under `restores_due`),
/// then the counter-notice. With none behind it the op answers `not_found`
/// and changes nothing.
pub async fn restore_case(
    io: &impl Io,
    keys: &StoreKeys,
    origins: &Origins,
    case: &CaseRow,
    journal: Stmt,
) -> Result<Effect, RefusalCode> {
    let now = io.now_ms();
    if case.kind == CaseKind::XRequest || case.source_id.is_some() {
        let Some(sid) = case.source_id else {
            // Its tombstone was lifted or purged already: nothing changes.
            return Ok(Effect::default());
        };
        let mut batch = vec![journal];
        batch.extend(x_restore_stmts(&case.case_id, sid, now));
        let done = io.batch(batch).await.map_err(|_| RefusalCode::Unavailable)?;
        return Ok(Effect { pages: done.last().map_or(0, |d| d.changes), ..Effect::default() });
    }
    let restoring = if case.kind == CaseKind::CounterNotice {
        let behind = countered_behind(io, &case.case_id).await?;
        if behind.is_empty() {
            return Err(RefusalCode::NotFound);
        }
        behind
    } else {
        vec![case.clone()]
    };
    let mut batch = vec![journal];
    let mut effect = Effect::default();
    let mut slugs = Vec::new();
    // Each quarantined copy put back, under the case that held it.
    let mut put_back: Vec<(String, Bytes32)> = Vec::new();
    // Each public page's state change: its index in the batch, its slug,
    // its run, and whether it returns failed (its run's document goes).
    let mut public: Vec<(usize, String, Option<u64>, bool)> = Vec::new();
    // Pages already returned (or found held) by an earlier case of this op.
    let mut returned = BTreeSet::new();
    let own = own_cases(restoring.iter().map(|c| c.case_id.as_str()).chain([case.case_id.as_str()]));
    for c in &restoring {
        let targets = case_targets(io, &c.case_id).await?;
        let mut seen = BTreeSet::new();
        for t in &targets {
            if t.kind == Kind::PublicUrl {
                let Some(from) = returned_from(t.state) else { continue };
                if t.platform == Platform::X || !returned.insert(t.id_h) {
                    continue;
                }
                // A case that never accepted a removal (awaiting its
                // decision, or rejected) removed nothing: its only step of
                // its own is the intake withhold, so a removed page stays.
                if t.state == DetState::Removed && !may_have_removed(c.state) {
                    effect.on_hold += 1;
                    continue;
                }
                let slug = t.public_slug.clone().unwrap_or_default();
                // The page was live when this case's notice withheld it, so
                // no case closed before then holds it.
                let since = if c.kind == CaseKind::Copyright && intake_withheld(io, &slug, c.received_at).await? {
                    c.received_at
                } else {
                    0
                };
                let others = Others { own: &own, run: t.run_id, closed_since: Some(since), op_hold: true };
                if others.hold(io, &t.id_h).await? {
                    effect.on_hold += 1;
                    continue;
                }
                let at = batch.len();
                if report_kept(io, t.run_id).await? {
                    batch.extend(return_public_live(&t.id_h, from, &others, now));
                    public.push((at, slug, t.run_id, false));
                } else {
                    batch.extend(return_public_failed(&t.id_h, from, t.run_id, &others, now));
                    public.push((at, slug, t.run_id, true));
                }
            } else {
                for m in family(io, t).await? {
                    // A family is read once per case, whichever of its pages
                    // the case names.
                    if !seen.insert(m.id_h) || returned.contains(&m.id_h) {
                        continue;
                    }
                    match m.state {
                        DetState::Disabled => batch.push(return_secret(&m.id_h, "disabled", now)),
                        DetState::Removed => {
                            if unquarantine(io, keys, &c.case_id, &m.id_h)
                                .await
                                .map_err(|_| RefusalCode::Unavailable)?
                            {
                                put_back.push((c.case_id.clone(), m.id_h));
                                batch.push(Stmt::new(
                                    "UPDATE detections SET state = 'live', state_v = state_v + 1, updated_at = ?2 \
WHERE id_h = ?1 AND kind IN ('private_text','private_paid') AND state = 'removed'",
                                    vec![Val::blob(&m.id_h), Val::int(now)],
                                ));
                            } else if m.kind == Kind::PrivatePaid && m.doc_key.is_none() {
                                // Never scored (a paid page whose job the
                                // review stopped): it returns failed.
                                batch.push(return_secret(&m.id_h, "removed", now));
                            } else {
                                continue;
                            }
                        }
                        _ => continue,
                    }
                    returned.insert(m.id_h);
                    effect.pages += 1;
                }
            }
        }
        batch.push(case_state_stmt(&c.case_id, CaseState::Restored, now));
        if c.kind == CaseKind::Copyright {
            batch.push(counter_notices_restored(&c.case_id, now));
        }
    }
    if case.kind == CaseKind::CounterNotice {
        batch.push(case_state_stmt(&case.case_id, CaseState::Restored, now));
        effect.restored_cases = restoring.iter().map(|c| c.case_id.clone()).collect();
    }
    let done = match io.batch(batch).await {
        Ok(d) => d,
        Err(_) => {
            let keys: Vec<String> = put_back.iter().map(|(_, id)| secret_doc_key(id)).collect();
            let _ = delete_docs(io, &keys).await;
            return Err(RefusalCode::Unavailable);
        }
    };
    let q: Vec<String> = put_back.iter().map(|(c, id)| quarantine_key(c, id)).collect();
    let _ = delete_docs(io, &q).await;
    // A public page counts as returned only when its state changed: one a
    // case came to hold since the read (or that changed otherwise) is left
    // as it is, and so is its run's document.
    let mut gone_runs = BTreeSet::new();
    for (at, slug, run, gone) in public {
        if done.get(at).map_or(0, |d| d.changes) == 0 {
            effect.on_hold += 1;
            continue;
        }
        effect.pages += 1;
        if gone {
            effect.report_gone += 1;
            gone_runs.extend(run);
        }
        slugs.push(slug);
    }
    for r in gone_runs {
        if delete_unread_run_doc(io, r).await.is_err() {
            effect.docs_pending += 1;
        }
    }
    queue_purges(io, origins, &slugs).await;
    io.metric("removal", &[case.kind.as_str(), "restored"]);
    Ok(effect)
}

/// `restore {slug}`: a public page removed outside a case returns, as a
/// case's target would: live while its report is kept, else `failed` with
/// `report_gone` (`return_public_failed`), and the operator's hold its
/// `remove {slug}` left goes with it. It is the operator's decision on the
/// page itself, so it returns a page held only by that hold, by a `closed`
/// case or by a case on another page of its run, but never one an open case
/// holds (`accepted`, `countered`, or a copyright case awaiting its
/// decision): that page stays as it is (`on_hold`), the operator's hold
/// included, until those cases are restored or rejected.
pub async fn restore_slug(io: &impl Io, origins: &Origins, slug: &str, journal: Stmt) -> Result<Effect, RefusalCode> {
    let now = io.now_ms();
    let (row, _) = public_lookup(io, slug).await.map_err(|_| RefusalCode::Unavailable)?;
    let row = row.filter(|r| r.platform != Platform::X).ok_or(RefusalCode::NotFound)?;
    let Some(from) = returned_from(row.state) else {
        // Neither removed nor withheld: journaled, and nothing changes.
        io.batch(vec![journal]).await.map_err(|_| RefusalCode::Unavailable)?;
        return Ok(Effect::default());
    };
    let others = Others { own: "[]", run: None, closed_since: None, op_hold: false };
    if others.hold(io, &row.id_h).await? {
        // Journaled, and nothing else changes.
        io.batch(vec![journal]).await.map_err(|_| RefusalCode::Unavailable)?;
        return Ok(Effect { on_hold: 1, ..Effect::default() });
    }
    let kept = report_kept(io, row.run_id).await?;
    let mut batch = vec![journal];
    // The state change is the batch's second statement either way.
    if kept {
        batch.extend(return_public_live(&row.id_h, from, &others, now));
    } else {
        batch.extend(return_public_failed(&row.id_h, from, row.run_id, &others, now));
    }
    let done = io.batch(batch).await.map_err(|_| RefusalCode::Unavailable)?;
    let pages = done.get(1).map_or(0, |d| d.changes);
    let mut effect = Effect { pages, ..Effect::default() };
    if !kept && pages > 0 {
        effect.report_gone = pages;
        if let Some(r) = row.run_id
            && delete_unread_run_doc(io, r).await.is_err()
        {
            effect.docs_pending += 1;
        }
    }
    queue_purges(io, origins, &[slug.to_owned()]).await;
    Ok(effect)
}

/// `reject {case_id}`: the case is rejected and its automatic first step
/// undone: a secret family it disabled is live again, and a public page it
/// withheld at intake (its record of that step, at the case's
/// `received_at`, shows it did) returns with that record deleted. A page
/// still held is left withheld with its record (`on_hold`): when another case
/// targets it and is `accepted`, `countered` or a copyright case awaiting
/// its decision, or when an accepted withhold of the same text rewrote its
/// run document without the text, since it cannot go live as it was.
pub async fn reject_case(
    io: &impl Io,
    origins: &Origins,
    case: &CaseRow,
    journal: Stmt,
) -> Result<Effect, RefusalCode> {
    let now = io.now_ms();
    if !case_edge(case.state, CaseState::Rejected) {
        return Err(RefusalCode::OpMalformed);
    }
    let targets = case_targets(io, &case.case_id).await?;
    let mut batch = vec![journal, case_state_stmt(&case.case_id, CaseState::Rejected, now)];
    let mut effect = Effect::default();
    let own = own_cases([case.case_id.as_str()]);
    let others = Others { own: &own, run: None, closed_since: None, op_hold: true };
    // Each public page's state change: its index in the batch, its slug.
    let mut public: Vec<(usize, String)> = Vec::new();
    for t in &targets {
        match (t.kind, t.state, case.kind) {
            (Kind::PublicUrl, DetState::ExcerptWithheld, CaseKind::Copyright) => {
                let slug = t.public_slug.clone().unwrap_or_default();
                if !intake_withheld(io, &slug, case.received_at).await? {
                    // Its notice found the page already held: nothing of
                    // this case's to undo.
                    continue;
                }
                if !report_kept(io, t.run_id).await? || others.hold(io, &t.id_h).await? {
                    effect.on_hold += 1;
                    continue;
                }
                let mut vals = vec![Val::blob(&t.id_h), Val::int(now)];
                vals.extend(others.vals(&t.id_h));
                public.push((batch.len(), slug.clone()));
                batch.push(Stmt::owned(
                    format!(
                        "UPDATE detections SET state = 'live', state_v = state_v + 1, updated_at = ?2 WHERE id_h = ?1 \
AND kind = 'public_url' AND state = 'excerpt_withheld' AND NOT {}",
                        Others::sql(3)
                    ),
                    vals,
                ));
                // The record goes only with the withhold it records.
                batch.push(Stmt::new(
                    "DELETE FROM removals WHERE public_slug = ?1 AND scope = 'excerpt' AND at = ?2 \
AND EXISTS (SELECT 1 FROM detections WHERE id_h = ?3 AND state = 'live')",
                    vec![Val::text(&slug), Val::int(case.received_at), Val::blob(&t.id_h)],
                ));
                batch.push(shard_dirty(&t.id_h));
            }
            (Kind::PrivateText | Kind::PrivatePaid, DetState::Disabled, CaseKind::Illegal) => {
                batch.push(Stmt::owned(
                    format!(
                        "UPDATE detections SET {RETURN_SET}, state_v = state_v + 1, updated_at = ?2 \
WHERE (id_h = ?1 OR copy_of = ?1) AND kind IN ('private_text','private_paid') AND state = 'disabled'"
                    ),
                    vec![Val::blob(&t.family_root()), Val::int(now)],
                ));
                effect.pages += 1;
            }
            _ => {}
        }
    }
    let done = io.batch(batch).await.map_err(|_| RefusalCode::Unavailable)?;
    let mut slugs = Vec::new();
    for (at, slug) in public {
        if done.get(at).map_or(0, |d| d.changes) == 0 {
            effect.on_hold += 1;
        } else {
            effect.pages += 1;
            slugs.push(slug);
        }
    }
    queue_purges(io, origins, &slugs).await;
    io.metric("removal", &[case.kind.as_str(), "rejected"]);
    Ok(effect)
}

/// `close_case {case_id}`: the case is closed and its quarantine deleted.
/// Pages it left disabled stay disabled (the answer counts them), so a
/// later decision is the operator's.
pub async fn close_case(io: &impl Io, case: &CaseRow, journal: Stmt) -> Result<Value, RefusalCode> {
    let now = io.now_ms();
    if case.state == CaseState::Closed {
        return Err(RefusalCode::OpMalformed);
    }
    let done = io
        .batch(vec![
            journal,
            case_state_stmt(&case.case_id, CaseState::Closed, now),
            Stmt::new(
                "SELECT count(*) AS n FROM case_targets t JOIN detections d ON d.id_h = t.det_id_h \
WHERE t.case_id = ?1 AND d.state = 'disabled'",
                vec![Val::text(&case.case_id)],
            ),
        ])
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    let disabled = done.get(2).and_then(Done::first).and_then(|r| r.u64("n")).unwrap_or(0);
    let deleted = delete_quarantine(io, &case.case_id).await.unwrap_or(0);
    io.metric("removal", &[case.kind.as_str(), "closed"]);
    Ok(json!({"disabled_left": disabled, "quarantine_deleted": deleted}))
}

/// Deletes a case's quarantine (`q/<case_id>/`), page by page.
pub async fn delete_quarantine(io: &impl Io, case_id: &str) -> Result<u64, IoError> {
    let prefix = format!("q/{case_id}/");
    let mut n = 0;
    loop {
        let page = io.doc_list(&prefix, None).await?;
        if page.objects.is_empty() {
            return Ok(n);
        }
        let keys: Vec<String> = page.objects.into_iter().map(|(k, _)| k).collect();
        n += keys.len() as u64;
        delete_docs(io, &keys).await?;
        if page.cursor.is_none() {
            return Ok(n);
        }
    }
}

/// The hourly backstop: the quarantine of every closed or rejected case.
/// Bounded per run by the listing's pages; the next hour continues.
pub async fn sweep_quarantine(io: &impl Io) -> Result<u64, IoError> {
    let mut cursor: Option<String> = None;
    let mut seen = BTreeSet::new();
    let mut deleted = 0;
    for _ in 0..10 {
        let page = io.doc_list("q/", cursor.as_deref()).await?;
        for (k, _) in &page.objects {
            if let Some(case_id) = k.strip_prefix("q/").and_then(|r| r.split('/').next()) {
                seen.insert(case_id.to_owned());
            }
        }
        cursor = page.cursor;
        if cursor.is_none() {
            break;
        }
    }
    for case_id in seen {
        let d = io.run(Stmt::new("SELECT state FROM cases WHERE case_id = ?1", vec![Val::text(&case_id)])).await?;
        let ended = match d.first().and_then(|r| r.text("state")).and_then(CaseState::parse) {
            None => true,
            Some(s) => matches!(s, CaseState::Closed | CaseState::Rejected),
        };
        if ended {
            deleted += delete_quarantine(io, &case_id).await?;
        }
    }
    Ok(deleted)
}

/// An operator note's R2 key.
pub fn note_key(case_id: &str, at: u64, nonce_hex: &str) -> String {
    format!("c/{case_id}/n/{at:013}-{nonce_hex}")
}

/// `case_note {case_id, note}`: the note is kept encrypted beside the
/// notice (never in D1); a received case is now under review.
pub async fn case_note(
    io: &impl Io,
    keys: &StoreKeys,
    case: &CaseRow,
    note: &str,
    nonce_hex: &str,
    journal: Stmt,
) -> Result<Value, RefusalCode> {
    if note.trim().is_empty() || note.len() > NOTE_BYTES {
        return Err(RefusalCode::OpMalformed);
    }
    let now = io.now_ms();
    let key = note_key(&case.case_id, now, nonce_hex);
    let mut nonce = [0u8; NONCE_BYTES];
    io.fill(&mut nonce);
    let sealed = seal_with(keys.cases_key(), &nonce, NOTE_AAD, note.as_bytes());
    io.doc_put(&key, sealed).await.map_err(|_| RefusalCode::Unavailable)?;
    let batch = vec![
        journal,
        Stmt::new(
            "UPDATE cases SET state = 'reviewing', updated_at = ?2 WHERE case_id = ?1 AND state = 'received'",
            vec![Val::text(&case.case_id), Val::int(now)],
        ),
    ];
    if io.batch(batch).await.is_err() {
        let _ = io.doc_delete(&[key]).await;
        return Err(RefusalCode::Unavailable);
    }
    Ok(json!({}))
}

/// `case_show {case_id}`: the case, its targets, strikes, the notice
/// opened and the notes (the operator only).
pub async fn case_show(io: &impl Io, keys: &StoreKeys, case: &CaseRow) -> Result<Value, RefusalCode> {
    let targets = case_targets(io, &case.case_id).await?;
    let notice = match &case.notice_key {
        None => Value::Null,
        Some(k) => match io.doc_get(k).await.map_err(|_| RefusalCode::Unavailable)? {
            None => Value::Null,
            Some(b) => {
                let plain = open_with(keys.cases_key(), CASE_AAD, &b).map_err(|_| RefusalCode::Internal)?;
                serde_json::from_slice(&plain).map_err(|_| RefusalCode::Internal)?
            }
        },
    };
    let mut notes = Vec::new();
    let page = io.doc_list(&format!("c/{}/n/", case.case_id), None).await.map_err(|_| RefusalCode::Unavailable)?;
    for (k, at) in page.objects {
        if let Some(b) = io.doc_get(&k).await.map_err(|_| RefusalCode::Unavailable)?
            && let Ok(plain) = open_with(keys.cases_key(), NOTE_AAD, &b)
        {
            notes.push(json!({"at": at, "note": String::from_utf8_lossy(&plain)}));
        }
    }
    let strikes = io
        .run(Stmt::new(
            "SELECT hex(owner_h) AS owner_h, owner_via, review FROM strikes WHERE case_id = ?1 ORDER BY owner_h",
            vec![Val::text(&case.case_id)],
        ))
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    let strikes: Vec<Value> = strikes
        .rows
        .iter()
        .map(|r| json!({"owner": r.text("owner_h").map(str::to_ascii_lowercase), "owner_via": r.text("owner_via"), "review": r.text("review")}))
        .collect();
    let targets: Vec<Value> = targets
        .iter()
        .map(|t| {
            json!({"id_h": hex::encode(t.id_h), "public_slug": t.public_slug, "kind": t.kind.as_str(),
                   "platform": t.platform.as_str(), "state": t.state.as_str()})
        })
        .collect();
    Ok(json!({"case": case, "targets": targets, "notice": notice, "notes": notes, "strikes": strikes}))
}

/// `review_strike {case_id, counts}`: the operator's review of the strikes
/// a case left on claimed pages.
pub async fn review_strike(io: &impl Io, case: &CaseRow, counts: bool, journal: Stmt) -> Result<Value, RefusalCode> {
    let review = if counts { "counts" } else { "not_counted" };
    let done = io
        .batch(vec![
            journal,
            Stmt::new(
                "UPDATE strikes SET review = ?2 WHERE case_id = ?1 AND owner_via = 'claimed'",
                vec![Val::text(&case.case_id), Val::text(review)],
            ),
        ])
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    Ok(json!({"reviewed": done.get(1).map_or(0, |d| d.changes)}))
}

// ------------------------------------------------------------- owner match

/// The account Worker's ids, page by page (its `/internal/v1/users`).
#[allow(async_fn_in_trait)]
pub trait Accounts {
    async fn users_page(
        &self,
        after: Option<UserId>,
        limit: u32,
    ) -> Result<slop_ninja_account_wire::UsersPage, IoError>;
}

/// Account pages one `owner_match` reads, at most. Each is one binding call,
/// and Cloudflare allows 32 Worker invocations per request (the website's
/// forward and this Worker are two of them): 25 leaves room for the rest.
pub const OWNER_MATCH_PAGES: u32 = 25;

/// `owner_match {case_id, after?}`: the accounts behind a case's strike
/// owner hashes (or, before any strike, behind its owned targets). Up to
/// [`OWNER_MATCH_PAGES`] pages of account ids are read, each id's owner hash
/// derived and compared; `next` is where to continue when more are left.
pub async fn owner_match(
    io: &impl Io,
    accounts: &impl Accounts,
    keys: &StoreKeys,
    case: &CaseRow,
    after: Option<UserId>,
) -> Result<Value, RefusalCode> {
    let d = io
        .batch(vec![
            Stmt::new(
                "SELECT hex(owner_h) AS owner_h, owner_via, review FROM strikes WHERE case_id = ?1",
                vec![Val::text(&case.case_id)],
            ),
            Stmt::new(
                "SELECT hex(d.owner_h) AS owner_h, d.owner_via AS owner_via FROM case_targets t \
JOIN detections d ON d.id_h = t.det_id_h WHERE t.case_id = ?1 AND d.owner_h IS NOT NULL",
                vec![Val::text(&case.case_id)],
            ),
        ])
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    let mut owners: BTreeMap<Bytes32, (Option<OwnerVia>, Option<String>)> = BTreeMap::new();
    for (i, done) in d.iter().enumerate() {
        for r in &done.rows {
            let Some(o) = r.hex32("owner_h") else { continue };
            let via = r.text("owner_via").and_then(OwnerVia::parse);
            let review = if i == 0 { r.text("review").map(str::to_owned) } else { None };
            owners.entry(o).or_insert((via, review));
        }
    }
    let mut matches = Vec::new();
    let mut cursor = after;
    let mut scanned = 0u64;
    let mut next = None;
    if !owners.is_empty() {
        for _ in 0..OWNER_MATCH_PAGES {
            let page = accounts
                .users_page(cursor, slop_ninja_account_wire::USERS_PAGE_MAX)
                .await
                .map_err(|_| RefusalCode::Unavailable)?;
            for u in &page.users {
                scanned += 1;
                let o = keys.owner_h(&u.user_id.0);
                if let Some((via, review)) = owners.get(&o) {
                    matches.push(json!({"user_id": u.user_id.hex(), "state": u.state.as_str(),
                                        "owner_via": via.map(OwnerVia::as_str), "review": review}));
                }
            }
            cursor = page.next;
            next = page.next;
            if next.is_none() || matches.len() == owners.len() {
                break;
            }
        }
    }
    if matches.len() == owners.len() {
        next = None;
    }
    Ok(json!({"case_id": case.case_id, "owners": owners.len(), "matches": matches, "scanned": scanned,
              "next": next.map(|u| u.hex())}))
}

// ---------------------------------------------------------------- status

/// The `status` op's case part (content-free).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CaseStatus {
    /// Cases by kind and state.
    pub cases: Vec<(String, String, u64)>,
    /// Strikes per owner hash: counted (created, paid, and claimed ones
    /// reviewed as counting) and claimed ones awaiting review.
    pub strikes: Vec<StrikeCount>,
    /// The restores due: each countered copyright case behind a
    /// counter-notice whose wait has passed, the case to `restore` (its
    /// removal and quarantine hold the page), with the counter-notice beside
    /// it. Restoring either id brings the page back and clears the entry.
    pub restores_due: Vec<RestoreDue>,
    /// Open cases (received, incomplete or reviewing), oldest first.
    pub open: Vec<String>,
}

/// A restore due (§ 512(g)(2)(C)).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RestoreDue {
    /// The countered case to restore.
    pub case_id: String,
    /// The counter-notice whose wait has passed.
    pub counter_notice: String,
    /// When its wait passed.
    pub due_at: u64,
}

/// One owner hash's strikes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StrikeCount {
    pub owner: String,
    pub counted: u64,
    pub awaiting: u64,
}

/// Reads [`CaseStatus`].
pub async fn status(io: &impl Io) -> Result<CaseStatus, RefusalCode> {
    let now = io.now_ms();
    let d = io
        .batch(vec![
            Stmt::new("SELECT kind, state, count(*) AS n FROM cases GROUP BY kind, state ORDER BY kind, state", Vec::new()),
            Stmt::new(
                "SELECT hex(owner_h) AS owner_h, \
sum(CASE WHEN owner_via <> 'claimed' OR review = 'counts' THEN 1 ELSE 0 END) AS counted, \
sum(CASE WHEN owner_via = 'claimed' AND review IS NULL THEN 1 ELSE 0 END) AS awaiting \
FROM strikes GROUP BY owner_h ORDER BY counted DESC, owner_h",
                Vec::new(),
            ),
            Stmt::new(
                "SELECT DISTINCT c.case_id AS case_id, k.case_id AS counter_notice, k.due_at AS due_at FROM cases k \
JOIN case_targets n ON n.case_id = k.case_id JOIN case_targets t ON t.det_id_h = n.det_id_h \
JOIN cases c ON c.case_id = t.case_id WHERE k.kind = 'counter_notice' AND k.due_at IS NOT NULL \
AND k.due_at <= ?1 AND k.state IN ('received','reviewing') AND c.kind = 'copyright' AND c.state = 'countered' \
ORDER BY k.due_at, k.case_id, c.case_id",
                vec![Val::int(now)],
            ),
            Stmt::new(
                "SELECT case_id FROM cases WHERE state IN ('received','incomplete','reviewing') ORDER BY received_at LIMIT 500",
                Vec::new(),
            ),
        ])
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    let rows = |i: usize| d.get(i).map(|x| x.rows.clone()).unwrap_or_default();
    Ok(CaseStatus {
        cases: rows(0)
            .iter()
            .filter_map(|r| Some((r.text("kind")?.to_owned(), r.text("state")?.to_owned(), r.u64("n")?)))
            .collect(),
        strikes: rows(1)
            .iter()
            .filter_map(|r| {
                Some(StrikeCount {
                    owner: r.text("owner_h")?.to_ascii_lowercase(),
                    counted: r.u64("counted").unwrap_or(0),
                    awaiting: r.u64("awaiting").unwrap_or(0),
                })
            })
            .collect(),
        restores_due: rows(2)
            .iter()
            .filter_map(|r| {
                Some(RestoreDue {
                    case_id: r.text("case_id")?.to_owned(),
                    counter_notice: r.text("counter_notice")?.to_owned(),
                    due_at: r.u64("due_at")?,
                })
            })
            .collect(),
        open: rows(3).iter().filter_map(|r| r.text("case_id").map(str::to_owned)).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use CaseState::*;

    #[test]
    fn every_edge_of_the_case_table() {
        let allowed: &[(CaseState, CaseState)] = &[
            (Received, Reviewing),
            (Received, Accepted),
            (Received, Rejected),
            (Received, Incomplete),
            (Received, Restored),
            (Incomplete, Received),
            (Incomplete, Reviewing),
            (Incomplete, Accepted),
            (Incomplete, Rejected),
            (Reviewing, Accepted),
            (Reviewing, Rejected),
            (Reviewing, Restored),
            (Accepted, Countered),
            (Accepted, Restored),
            (Countered, Restored),
        ];
        for &from in CaseState::ALL {
            for &to in CaseState::ALL {
                let want = allowed.contains(&(from, to)) || (to == Closed && from != Closed);
                assert_eq!(case_edge(from, to), want, "{from} -> {to}");
            }
        }
        assert_eq!(from_states(Countered), "('accepted')");
        assert!(!from_states(Closed).contains("'closed'"));
    }

    #[test]
    fn every_form_track_has_fields_and_the_bounds_hold_them() {
        for k in CaseKind::ALL {
            let f = track_fields(*k);
            assert_eq!(f.is_some(), k.is_form_track(), "{k}");
            for spec in f.unwrap_or_default() {
                assert!(spec.name.len() <= MAX_FIELD_NAME, "{}", spec.name);
            }
            assert!(f.unwrap_or_default().len() <= MAX_FIELDS, "{k}");
        }
        // A track's fields at the notice's bound fit the notice's JSON bound,
        // whatever they hold (two bytes an escaped byte at most).
        let mut fields = BTreeMap::new();
        fields.insert("what".to_owned(), "\"".repeat(NOTICE_FIELD_BYTES));
        assert!(check_fields(CaseKind::WrongResult, &fields).is_ok());
        let n = Notice {
            v: NOTICE_V,
            track: CaseKind::WrongResult,
            target: "https://site.example/d/k7m2xq9".into(),
            secret: false,
            fields,
            received_at: u64::MAX,
            signed_in: true,
        };
        assert!(serde_json::to_vec(&n).unwrap().len() <= NOTICE_JSON_BYTES);
    }

    #[test]
    fn targets_are_slugs_or_links_to_our_pages() {
        let o = Origins::from_vars("https://site.example", "https://s.site.example").unwrap();
        let secret = "a".repeat(SECRET_LEN);
        let cases = [
            ("k7m2xq9", Some((SlugForm::Public, "k7m2xq9".to_owned()))),
            (" k7m2xq9 ", Some((SlugForm::Public, "k7m2xq9".to_owned()))),
            ("K7M2XQO", Some((SlugForm::Public, "k7m2xq0".to_owned()))),
            ("https://site.example/d/k7m2xq9#d=x", Some((SlugForm::Public, "k7m2xq9".to_owned()))),
            ("https://site.example/d/k7m2xq9/card.png", Some((SlugForm::Public, "k7m2xq9".to_owned()))),
            (&format!("https://s.site.example/{secret}#m=token"), Some((SlugForm::Secret, secret.clone()))),
            (&format!("https://site.example/{secret}"), None),
            ("https://s.site.example/d/k7m2xq9", None),
            ("https://site.example/detect/removals/", None),
            ("javascript:alert(1)", None),
        ];
        for (t, want) in cases {
            assert_eq!(target_slug(&o, t), want, "{t}");
        }
        assert!(is_case_id("td-8k2m4q7x") && !is_case_id("td-8k2m4q7") && !is_case_id("xx-8k2m4q7x"));
    }

    #[test]
    fn a_removal_body_fits_the_scan_memory() {
        let n = crate::routes::REMOVAL_BODY_BYTES;
        assert!(crate::routes::removal_footprint(n) < crate::routes::SCAN_MEMORY_BYTES / 4);
        assert!(n as usize >= NOTICE_JSON_BYTES);
    }
}
