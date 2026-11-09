//! The registry and the per-cohort rosters of `dashboard/v1` (bounds rule
//! v3): the documents behind the public status API's miner list
//! (`slop_ninja.api.cohort_status/v1`, served by the website).
//!
//! - `registry.json` (`slop_ninja.dashboard.registry/v1`): every registered
//!   UID of the subnet with its hotkey and registration block, as the
//!   coordinator's latest signing operation read them
//!   (`chain_journal.registry`, rule `slop_ninja.coordinator.registry/v1`).
//!   Public chain facts. Written each run from a coordinator that reports
//!   the field; `epoch`, `block` and `rows` are `null` while the read is
//!   absent, refused or malformed. A coordinator that does not report the
//!   field writes no registry (the document stays missing, or keeps its
//!   last version).
//! - `m/<key>/rosters/<label>.json` (`slop_ninja.dashboard.roster/v1`): one
//!   cohort's per-hotkey status. A closed, listed cohort's roster comes from
//!   its public summary alone (`source: "summary"`): each entrant's UID,
//!   hotkey, status (`scored`, `withheld`, `not_executed`), roster position,
//!   gate and public-copy gate bits, band and cluster, its artifact state,
//!   and `R_k` and `R_k-1` by digest. No `q`, cutoff or band edge: the
//!   roster carries no score. It is rewritten with the cohort documents.
//!   A running cohort's roster is written each run only at the level the
//!   held-out exposure review accepts ([`RosterLevel`], `DASHBOARD_ROSTER`):
//!   from the eval Worker's per-hotkey admission rows
//!   (`admission.cohorts[].hotkeys`, rule
//!   `slop_ninja.eval.admission_hotkeys/v1`, `source: "admission"`) or, from
//!   scoring start, the published artifacts record (`source: "artifacts"`).
//!
//! Rows are in ascending UID order (rows without a UID last), then by
//! hotkey: never roster or score order. Before close no row carries a
//! result or a roster position; `admitted` and `evaluating` come from the
//! frozen roster and the stage `now.json` shows (read at tempo boundaries),
//! never from per-entrant work. At the sessions level an `admitted` or
//! `evaluating` row also carries its challenge counts (`challenges`, from
//! the eval Worker's `admission.cohorts[].progress`, rule
//! `slop_ninja.eval.scoring_progress/v1`, read each run): `pending` until
//! the entrant's scoring starts, then documents resolved of its total.
//! Counts only (the 2026-11-06 decision): no score, leader, score
//! order, per-challenge identity, failure or retry detail. At the `live`
//! level a running roster also carries the cohort's live values (the eval
//! Worker's `/eval/live`, rule `slop_ninja.eval.live_scores/v1`): each
//! executed entrant's `live-v1` segment readings and its pre-finalized
//! quantized score (`live` on its row), and the rule's parameters, `R_k`'s
//! counts in segment steps, its readings and the references' pre-finalized
//! scores with the margin line (the top-level `live` block). A roster's
//! level is `live` exactly when it carries that block; otherwise a running
//! roster is written at `sessions`. Still no rank, position, leader, score
//! order, band, gate, cluster or unit value before close. A running
//! roster is not rewritten once its cohort leaves the running stages; the
//! closed roster replaces it at the same key when the cohort is listed.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use slop_ninja_protocol::AccountId;

use super::{Chain, Doc, NowDoc, Settings, chain, minute};
use crate::artifacts::StartRecord;
use crate::index::{ArtifactEntry, ArtifactState};
use crate::status::{AdmissionCohortView, EvalView, HotkeyRow, LiveScoresView, REFUSAL_CLASSES, RegistryView};
use crate::summary::CohortSummary;

/// How much of a running cohort's per-hotkey status the rosters carry. Each
/// level carries everything the levels before it do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum RosterLevel {
    /// Closed, listed cohorts only, from their public summaries.
    #[default]
    Closed,
    /// A running cohort's roster from scoring start: each entrant
    /// `evaluating` with its artifact state, from the published artifacts
    /// record (`v1/cohorts/<label>/artifacts.json`).
    Scoring,
    /// The eval Worker's per-hotkey rows: `none` or `committed` (from the
    /// logged commitment alone) before the roster freezes; from the freeze
    /// `admitted` (`evaluating` from `generating`), `refused` with its
    /// class, or `overflow`.
    Frozen,
    /// Also each hotkey's upload and verification state before the freeze
    /// (`uploading`, `verifying`, `verified`, `refused` with its class),
    /// and each admitted entrant's challenge counts ([`Challenges`]).
    Sessions,
    /// Also the running cohort's live values ([`LiveRow`], [`LiveBlock`]):
    /// `live-v1` segment readings and pre-finalized scores. A document is
    /// written at this level only when it carries the live block.
    Live,
}

impl RosterLevel {
    pub const ALL: [RosterLevel; 5] = [Self::Closed, Self::Scoring, Self::Frozen, Self::Sessions, Self::Live];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Scoring => "scoring",
            Self::Frozen => "frozen",
            Self::Sessions => "sessions",
            Self::Live => "live",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == v)
    }
}

/// The roster statuses, in the order a hotkey moves through them.
pub const ROSTER_STATUSES: &[&str] = &[
    "none",
    "committed",
    "uploading",
    "verifying",
    "verified",
    "refused",
    "overflow",
    "admitted",
    "evaluating",
    "scored",
    "withheld",
    "not_executed",
];

/// The statuses of a closed cohort's entrants.
const CLOSED_STATUSES: &[&str] = &["scored", "withheld", "not_executed"];
/// The statuses only [`RosterLevel::Sessions`] carries.
const SESSION_STATUSES: &[&str] = &["uploading", "verifying", "verified"];
/// The summary's gate bits.
pub const GATES: &[&str] = &["pass", "fail", "withheld", "not_executed"];
pub const ARTIFACT_STATES: &[&str] = &["published", "pending", "withheld"];
pub const ROSTER_SOURCES: &[&str] = &["summary", "admission", "artifacts"];
/// The challenge count states, in order: before the entrant's scoring
/// starts, while it runs, once every challenge is resolved.
pub const CHALLENGE_STATES: &[&str] = &["pending", "running", "complete"];
/// The bar stages in which an admitted entrant is `evaluating`.
const EVALUATING_STAGES: &[&str] = &["generating", "sealing", "scoring", "packaging", "transition"];
/// The stages a running roster is written in.
const RUNNING_STAGES: &[&str] = &[
    "admission_open",
    "admission_closing",
    "generating",
    "sealing",
    "scoring",
    "packaging",
    "transition",
];

// ------------------------------------------------------------------ types

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryRow {
    pub uid: u16,
    /// SS58 (prefix 42).
    pub hotkey: String,
    /// The same key as 64 lowercase hex characters.
    pub hotkey_hex: String,
    pub registration_block: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryDoc {
    pub schema: String,
    pub network: String,
    pub chain: Chain,
    pub generated_at: Option<u64>,
    pub sample: bool,
    /// `slop_ninja.coordinator.registry/v1`, the read this was built from.
    pub rule: String,
    /// The epoch and confirmed block of the read; `null` with `rows`.
    pub epoch: Option<u64>,
    pub block: Option<u64>,
    /// Ascending UIDs, each hotkey once; `null` while no whole read is
    /// available.
    pub rows: Option<Vec<RegistryRow>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RosterCohort {
    pub cohort: u64,
    pub label: String,
    /// The bar stage `now.json` showed when the roster was written, or
    /// `closed`.
    pub stage: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AsOf {
    pub epoch: u64,
    pub block: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterRef {
    pub uid: u16,
    pub hotkey: String,
}

/// A closed cohort entrant's published result (the summary's meanings).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowResult {
    /// [`GATES`].
    pub gate: String,
    /// `pass` or `fail`; `null` for a withheld entrant.
    pub copy_gate: Option<String>,
    /// `1` the top occupied band; `null` below the cutoff.
    pub band: Option<u32>,
    /// The near-copy cluster, named by its first member in roster order.
    pub cluster: Option<ClusterRef>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RosterRow {
    pub uid: Option<u16>,
    /// SS58 (prefix 42).
    pub hotkey: String,
    pub hotkey_hex: Option<String>,
    /// [`ROSTER_STATUSES`].
    pub status: String,
    /// [`REFUSAL_CLASSES`], only with `refused`.
    pub reason_class: Option<String>,
    /// 1-based roster order, closed cohorts only.
    pub roster_position: Option<u32>,
    pub result: Option<RowResult>,
    /// [`ARTIFACT_STATES`], from scoring start.
    pub artifact_state: Option<String>,
    /// The entrant's challenge counts: `admitted` and `evaluating` rows of
    /// a running roster at the sessions level only; `null` otherwise, and
    /// while the eval Worker reports none for the row.
    pub challenges: Option<Challenges>,
    /// The entrant's live values: `admitted` and `evaluating` rows of a
    /// running roster at the `live` level only (absent otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<LiveRow>,
}

/// One executed entrant's live values while its cohort is scored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveRow {
    /// Its `live-v1` segment readings, in order, at most `segments - 1`,
    /// each a multiple of the block's `width` with exactly its decimals, or
    /// `withheld`; released once and never changed; none while stopped.
    pub checkpoints: Vec<String>,
    /// More attributable failures than the minimum-success limit allows: no
    /// further reading, and the result will withhold it.
    pub stopped: bool,
    /// Its quantized score under `quant-v1` (or `withheld`), marked
    /// pre-finalized: the value the signed result will carry unless a
    /// signed correction replaces it. `null` until it and `R_k` finish.
    pub prefinal: Option<String>,
}

/// A running cohort's live block: the rule's parameters and counted bits,
/// `R_k`'s counts and readings, and the references' pre-finalized scores.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveBlock {
    /// `live-v1`.
    pub rule: String,
    /// The live grid; `null` without readings.
    pub width: Option<String>,
    /// `L`: `0` (no readings) or at least 2.
    pub segments: u8,
    /// `L - 1`.
    pub released_max: u8,
    /// One reading's standard deviation, rounded up to thousandths.
    pub noise_sd: Option<String>,
    pub eps_live_milli: u32,
    pub bits: LiveBits,
    pub reference: Option<LiveReference>,
    pub prefinal: Option<LivePrefinal>,
}

/// The cohort's counted bits: every rule's at the seal, the readings', and
/// `floor(n h / F)`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveBits {
    pub counted: u32,
    pub live: u32,
    pub bound: u64,
}

/// `R_k` while the cohort is scored: its batch tasks done in segment steps,
/// its readings (`none` after a failure) and whether it failed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveReference {
    pub done: u32,
    pub total: u32,
    pub checkpoints: Vec<String>,
    pub stopped: bool,
}

/// The references' pre-finalized scores on the cohort's `quant-v1` grid
/// and the margin line that follows from `R_k`'s.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LivePrefinal {
    pub quantization: LiveQuantization,
    pub current: String,
    pub predecessor: Option<String>,
    pub margin_line: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveQuantization {
    pub rule: String,
    pub width: String,
}

/// One entrant's scoring progress, counts only. `pending`: its scoring has
/// not started (`done` 0; `total` `null` until the cohort's batch is
/// sealed). `running`: `done < total`. `complete`: `done == total`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenges {
    /// [`CHALLENGE_STATES`].
    pub state: String,
    pub done: u32,
    pub total: Option<u32>,
}

impl Challenges {
    fn pending(total: Option<u32>) -> Self {
        Self { state: "pending".into(), done: 0, total }
    }

    /// The counts of one eval progress row.
    fn of(r: &crate::status::ScoringProgressRow) -> Self {
        let state = if !r.started {
            "pending"
        } else if r.done == r.total {
            "complete"
        } else {
            "running"
        };
        Self { state: state.into(), done: r.done, total: Some(r.total) }
    }

    /// The state agrees with the counts.
    fn consistent(&self) -> bool {
        match (self.state.as_str(), self.total) {
            ("pending", t) => self.done == 0 && t != Some(0),
            ("running", Some(t)) => self.done < t,
            ("complete", Some(t)) => t >= 1 && self.done == t,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDigests {
    pub content: String,
    pub artifact: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RosterReferences {
    /// `k`.
    pub index: u64,
    pub current: ModelDigests,
    pub predecessor: Option<ModelDigests>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RosterDoc {
    pub schema: String,
    pub network: String,
    pub chain: Chain,
    pub generated_at: Option<u64>,
    pub sample: bool,
    pub mechanism: String,
    pub cohort: RosterCohort,
    /// The [`RosterLevel`] the publisher ran with (`closed` for a closed
    /// cohort's roster).
    pub level: String,
    /// [`ROSTER_SOURCES`].
    pub source: String,
    /// The boundary the eval Worker's rows describe; `null` otherwise.
    pub as_of: Option<AsOf>,
    pub rows: Vec<RosterRow>,
    /// `R_k` and `R_k-1` by digest, closed cohorts only.
    pub references: Option<RosterReferences>,
    /// The running cohort's live block, exactly at the `live` level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<LiveBlock>,
}

// ------------------------------------------------------------- allow-list

pub(super) const REGISTRY_PATHS: &[&str] = &[
    "rule",
    "epoch",
    "block",
    "rows",
    "rows[].uid",
    "rows[].hotkey",
    "rows[].hotkey_hex",
    "rows[].registration_block",
];

pub(super) const ROSTER_PATHS: &[&str] = &[
    "mechanism",
    "cohort",
    "cohort.cohort",
    "cohort.label",
    "cohort.stage",
    "level",
    "source",
    "as_of",
    "as_of.epoch",
    "as_of.block",
    "rows",
    "rows[].uid",
    "rows[].hotkey",
    "rows[].hotkey_hex",
    "rows[].status",
    "rows[].reason_class",
    "rows[].roster_position",
    "rows[].result",
    "rows[].result.gate",
    "rows[].result.copy_gate",
    "rows[].result.band",
    "rows[].result.cluster",
    "rows[].result.cluster.uid",
    "rows[].result.cluster.hotkey",
    "rows[].artifact_state",
    "rows[].challenges",
    "rows[].challenges.state",
    "rows[].challenges.done",
    "rows[].challenges.total",
    "references",
    "references.index",
    "references.current",
    "references.current.content",
    "references.current.artifact",
    "references.predecessor",
    "references.predecessor.content",
    "references.predecessor.artifact",
    "rows[].live",
    "rows[].live.checkpoints",
    "rows[].live.stopped",
    "rows[].live.prefinal",
    "live",
    "live.rule",
    "live.width",
    "live.segments",
    "live.released_max",
    "live.noise_sd",
    "live.eps_live_milli",
    "live.bits",
    "live.bits.counted",
    "live.bits.live",
    "live.bits.bound",
    "live.reference",
    "live.reference.done",
    "live.reference.total",
    "live.reference.checkpoints",
    "live.reference.stopped",
    "live.prefinal",
    "live.prefinal.quantization",
    "live.prefinal.quantization.rule",
    "live.prefinal.quantization.width",
    "live.prefinal.current",
    "live.prefinal.predecessor",
    "live.prefinal.margin_line",
];

fn hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The SS58 spelling agrees with the hex key.
fn same_key(ss58: &str, hex_key: &str) -> bool {
    crate::ss58::decode(ss58).is_some_and(|a| hex::encode(a.0) == hex_key)
}

/// `registry.json`'s own checks: ascending UIDs, distinct hotkeys whose two
/// spellings agree, registrations at or before the read's block, at most
/// the bound's rows, and `epoch`, `block` and `rows` all set or all `null`.
pub(super) fn check_registry(d: &RegistryDoc) -> Result<(), String> {
    if d.rule != crate::status::REGISTRY_RULE {
        return Err("registry rule".into());
    }
    let Some(rows) = &d.rows else {
        return match (d.epoch, d.block) {
            (None, None) => Ok(()),
            _ => Err("a registry boundary without rows".into()),
        };
    };
    let block = d.block.ok_or("registry rows without a block")?;
    d.epoch.ok_or("registry rows without an epoch")?;
    if rows.len() > super::contract::registry::ROWS {
        return Err("more registry rows than the bound".into());
    }
    let mut keys = BTreeSet::new();
    for (i, r) in rows.iter().enumerate() {
        if i > 0 && rows[i - 1].uid >= r.uid {
            return Err("registry UIDs not ascending".into());
        }
        if !hex64(&r.hotkey_hex) || !same_key(&r.hotkey, &r.hotkey_hex) || !keys.insert(&r.hotkey_hex) {
            return Err("registry hotkey not SS58 of its hex, or repeated".into());
        }
        if r.registration_block > block {
            return Err("a registration after the read's block".into());
        }
    }
    Ok(())
}

fn row_order(r: &RosterRow) -> (bool, u16, String) {
    (r.uid.is_none(), r.uid.unwrap_or(0), r.hotkey.clone())
}

/// A roster's own checks: closed vocabularies, results and roster positions
/// only once the cohort is closed (and then on every row), session states
/// only at the sessions level, a refusal class exactly with `refused`,
/// distinct hotkeys and UIDs in ascending order, references only when
/// closed.
pub(super) fn check_roster(d: &RosterDoc) -> Result<(), String> {
    let closed = d.cohort.stage == "closed";
    if !closed && !RUNNING_STAGES.contains(&d.cohort.stage.as_str()) {
        return Err("roster stage outside the bar".into());
    }
    let level = RosterLevel::parse(&d.level).ok_or("roster level")?;
    if !ROSTER_SOURCES.contains(&d.source.as_str()) || closed != (d.source == "summary") {
        return Err("roster source".into());
    }
    if closed != (level == RosterLevel::Closed) || (!closed && d.references.is_some()) {
        return Err("a running roster with a closed cohort's fields".into());
    }
    // The live level exactly with a live block, never on a closed roster.
    if (level == RosterLevel::Live) != d.live.is_some() {
        return Err("the live level without a live block, or a live block below it".into());
    }
    if let Some(b) = &d.live {
        check_live_block(b)?;
    }
    if d.rows.len() > super::contract::roster::ROWS {
        return Err("more roster rows than the bound".into());
    }
    let mut keys = BTreeSet::new();
    let mut uids = BTreeSet::new();
    for (i, r) in d.rows.iter().enumerate() {
        let st = r.status.as_str();
        if !ROSTER_STATUSES.contains(&st) || closed != CLOSED_STATUSES.contains(&st) {
            return Err(format!("roster status {st:?} for this stage"));
        }
        if SESSION_STATUSES.contains(&st) && level < RosterLevel::Sessions {
            return Err("a session state below the sessions level".into());
        }
        if d.source == "artifacts" && st != "evaluating" {
            return Err("an artifacts roster row that is not evaluating".into());
        }
        match &r.reason_class {
            Some(c) if st != "refused" || !REFUSAL_CLASSES.contains(&c.as_str()) => {
                return Err("refusal class".into());
            }
            None if st == "refused" => return Err("refused without a class".into()),
            _ => {}
        }
        if closed != r.result.is_some() || closed != r.roster_position.is_some() {
            return Err("a result or roster position before close".into());
        }
        if let Some(res) = &r.result {
            let scored = matches!(res.gate.as_str(), "pass" | "fail");
            let expect = if scored { "scored" } else { res.gate.as_str() };
            if !GATES.contains(&res.gate.as_str()) || st != expect {
                return Err("gate".into());
            }
            if res.copy_gate.as_deref().is_some_and(|c| !matches!(c, "pass" | "fail"))
                || (res.gate == "withheld") != res.copy_gate.is_none()
            {
                return Err("public-copy gate".into());
            }
            if res.cluster.as_ref().is_some_and(|c| crate::ss58::decode(&c.hotkey).is_none()) {
                return Err("cluster hotkey".into());
            }
        }
        if r.artifact_state.as_deref().is_some_and(|a| !ARTIFACT_STATES.contains(&a)) {
            return Err("artifact state".into());
        }
        if let Some(ch) = &r.challenges {
            if closed || level < RosterLevel::Sessions || !matches!(st, "admitted" | "evaluating") {
                return Err("challenge counts outside a running sessions roster's entrants".into());
            }
            if !CHALLENGE_STATES.contains(&ch.state.as_str()) || !ch.consistent() {
                return Err("challenge counts".into());
            }
        }
        if let Some(lv) = &r.live {
            let b = d.live.as_ref().ok_or("live values outside a live roster")?;
            if !matches!(st, "admitted" | "evaluating") {
                return Err("live values outside a running roster's entrants".into());
            }
            check_live_row(b, lv)?;
        }
        match &r.hotkey_hex {
            Some(h) if !hex64(h) || !same_key(&r.hotkey, h) => return Err("roster hotkey spellings disagree".into()),
            None if crate::ss58::decode(&r.hotkey).is_none() => return Err("roster hotkey not SS58".into()),
            _ => {}
        }
        if !keys.insert(&r.hotkey) || r.uid.is_some_and(|u| !uids.insert(u)) && !closed {
            return Err("roster hotkey or UID repeated".into());
        }
        if i > 0 && row_order(&d.rows[i - 1]) >= row_order(r) {
            return Err("roster rows not in UID order".into());
        }
    }
    if let Some(refs) = &d.references {
        let digests = std::iter::once(&refs.current).chain(refs.predecessor.as_ref());
        if digests.into_iter().any(|m| !hex64(&m.content) || !hex64(&m.artifact)) {
            return Err("reference digests".into());
        }
    }
    Ok(())
}

/// A live block's own checks: the rule; the live grid on the ladder with
/// its own spelling, exactly when there are segments; `released_max` is
/// `L - 1`; counted plus live bits within the bound; `R_k`'s checkpoints on
/// the grid (or `none`), at most `released_max`, its counts in range; the
/// references' pre-finalized values on their grid with the margin line at
/// or above `R_k`'s.
fn check_live_block(b: &LiveBlock) -> Result<(), String> {
    use slop_ninja_protocol::{live, quant};
    if b.rule != live::LIVE_RULE || b.segments == 1 || b.released_max != b.segments.saturating_sub(1) {
        return Err("live rule or segments".into());
    }
    let readings = b.segments >= live::MIN_SEGMENTS;
    let w = b.width.as_deref().map(grid_width).transpose()?;
    if readings != w.is_some() || readings != b.noise_sd.is_some() {
        return Err("live grid".into());
    }
    if b.noise_sd.as_deref().is_some_and(|s| !milli_decimal(s)) {
        return Err("live noise sd".into());
    }
    if readings && u64::from(b.bits.counted) + u64::from(b.bits.live) > b.bits.bound {
        return Err("live bits over the bound".into());
    }
    if let Some(r) = &b.reference
        && (r.done > r.total || !values_ok(&r.checkpoints, w, b.released_max, live::NONE))
    {
        return Err("live reference".into());
    }
    if let Some(p) = &b.prefinal {
        let qw = grid_width(&p.quantization.width)?;
        if p.quantization.rule != quant::QUANT_RULE {
            return Err("live quantization rule".into());
        }
        let cur = quant::parse_grid(&p.current, qw).map_err(|_| "live current off the grid")?;
        if p.predecessor.as_deref().is_some_and(|v| quant::parse_grid(v, qw).is_err()) {
            return Err("live predecessor off the grid".into());
        }
        if let Some(m) = &p.margin_line
            && quant::parse_grid(m, qw).map_err(|_| "live margin line off the grid")? < cur
        {
            return Err("live margin line below the reference".into());
        }
    }
    Ok(())
}

/// An entrant's live values: checkpoints on the block's grid (or
/// `withheld`), at most `released_max`; a pre-finalized value only with the
/// references', on their grid (or `withheld`).
fn check_live_row(b: &LiveBlock, r: &LiveRow) -> Result<(), String> {
    use slop_ninja_protocol::{live, quant};
    let w = b.width.as_deref().map(grid_width).transpose()?;
    if !values_ok(&r.checkpoints, w, b.released_max, live::WITHHELD) {
        return Err("live checkpoints".into());
    }
    if let Some(v) = &r.prefinal {
        let p = b.prefinal.as_ref().ok_or("an entrant's prefinal before the reference's")?;
        let qw = grid_width(&p.quantization.width)?;
        if v != live::WITHHELD && quant::parse_grid(v, qw).is_err() {
            return Err("live prefinal off the grid".into());
        }
    }
    Ok(())
}

/// A grid width printed with its own decimals, on the one-digit ladder.
fn grid_width(text: &str) -> Result<u64, String> {
    use slop_ninja_protocol::quant;
    (1..=18u32)
        .chain(core::iter::once(0))
        .find_map(|d| {
            let w = quant::parse_grid(text, 10u64.pow(18 - d)).ok()?;
            (quant::on_ladder(w) && quant::decimals(w).ok()? == d).then_some(w)
        })
        .ok_or_else(|| "live grid width".to_owned())
}

/// `d.ddd`.
fn milli_decimal(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 5 && s.split_once('.').is_some_and(|(w, f)| !w.is_empty() && w.bytes().all(|c| c.is_ascii_digit()) && f.len() == 3 && f.bytes().all(|c| c.is_ascii_digit()))
}

fn values_ok(values: &[String], w: Option<u64>, max: u8, symbol: &str) -> bool {
    values.len() <= usize::from(max)
        && values.iter().all(|v| v == symbol || w.is_some_and(|w| slop_ninja_protocol::quant::parse_grid(v, w).is_ok()))
}

// ------------------------------------------------------------- derivation

fn ss58(key: &[u8; 32]) -> String {
    crate::ss58::encode(&AccountId(*key))
}

fn state_str(s: ArtifactState) -> &'static str {
    match s {
        ArtifactState::Published => "published",
        ArtifactState::Withheld => "withheld",
        ArtifactState::Pending => "pending",
    }
}

/// `registry.json` from the coordinator's read (`None`: no whole read).
pub fn registry_doc(s: &Settings<'_>, view: Option<&RegistryView>, now_ms: u64) -> RegistryDoc {
    RegistryDoc {
        schema: Doc::Registry.schema().into(),
        network: "testnet".into(),
        chain: chain(s.chain),
        generated_at: Some(minute(now_ms)),
        sample: false,
        rule: crate::status::REGISTRY_RULE.into(),
        epoch: view.map(|v| v.epoch),
        block: view.map(|v| v.block),
        rows: view.map(|v| {
            v.rows
                .iter()
                .map(|(uid, key, reg)| RegistryRow {
                    uid: *uid,
                    hotkey: ss58(key),
                    hotkey_hex: hex::encode(key),
                    registration_block: *reg,
                })
                .collect()
        }),
    }
}

/// Sorts rows into UID order and keeps each hotkey once (the first).
fn ordered(mut rows: Vec<RosterRow>) -> Vec<RosterRow> {
    let mut seen = BTreeSet::new();
    rows.retain(|r| seen.insert(r.hotkey.clone()));
    rows.sort_by_key(row_order);
    rows
}

fn header(s: &Settings<'_>, label: &str, now_ms: u64) -> RosterDoc {
    RosterDoc {
        schema: Doc::Roster("a", label.to_owned()).schema().into(),
        network: "testnet".into(),
        chain: chain(s.chain),
        generated_at: Some(minute(now_ms)),
        sample: false,
        mechanism: "a".into(),
        ..Default::default()
    }
}

/// A closed cohort's roster, from its summary and publication record only.
pub fn closed_roster(s: &Settings<'_>, sm: &CohortSummary, artifacts: &[ArtifactEntry], now_ms: u64) -> RosterDoc {
    let rows = sm
        .entrants
        .iter()
        .enumerate()
        .map(|(i, e)| RosterRow {
            uid: Some(e.uid),
            hotkey: e.hotkey.clone(),
            hotkey_hex: crate::ss58::decode(&e.hotkey).map(|a| hex::encode(a.0)),
            status: match e.gate.as_str() {
                "pass" | "fail" => "scored",
                "withheld" => "withheld",
                _ => "not_executed",
            }
            .into(),
            reason_class: None,
            roster_position: u32::try_from(i + 1).ok(),
            result: Some(RowResult {
                gate: e.gate.clone(),
                copy_gate: e.copy_gate.clone(),
                band: e.band,
                cluster: e.cluster.as_ref().map(|c| ClusterRef {
                    uid: c.uid,
                    hotkey: c.hotkey.clone(),
                }),
            }),
            artifact_state: artifacts
                .iter()
                .find(|a| a.hotkey == e.hotkey)
                .map(|a| state_str(a.state).into()),
            challenges: None,
            live: None,
        })
        .collect();
    let digests = |m: &crate::summary::ModelRef| ModelDigests {
        content: m.content.clone(),
        artifact: m.artifact.clone(),
    };
    RosterDoc {
        cohort: RosterCohort {
            cohort: sm.cohort,
            label: sm.label.clone(),
            stage: "closed".into(),
        },
        level: RosterLevel::Closed.as_str().into(),
        source: "summary".into(),
        as_of: None,
        rows: ordered(rows),
        references: Some(RosterReferences {
            index: sm.references.index,
            current: digests(&sm.references.current),
            predecessor: sm.references.predecessor.as_ref().map(digests),
        }),
        live: None,
        ..header(s, &sm.label, now_ms)
    }
}

/// One eval row's public status at `level`: before the freeze only `none`
/// or `committed`, unless the sessions level shows the session's state;
/// from the freeze the roster's outcome. A state the level does not show
/// reads as `committed` or `none`. Below the sessions level that comes
/// from the logged commitment alone (chain data), so a session without a
/// commitment naming the cohort reads as `none` and is not revealed; at
/// the sessions level any session also reads as `committed`.
fn status_of(r: &HotkeyRow, level: RosterLevel, frozen: bool, evaluating: bool) -> (&'static str, Option<String>) {
    let sessions = level >= RosterLevel::Sessions;
    let committed = if r.committed_block.is_some() || (sessions && r.state != "none") { "committed" } else { "none" };
    match r.state.as_str() {
        "admitted" if frozen => (if evaluating { "evaluating" } else { "admitted" }, None),
        "overflow" if frozen => ("overflow", None),
        "refused" if frozen || sessions => ("refused", r.refusal_class.clone()),
        "uploading" if sessions && !frozen => ("uploading", None),
        "verifying" if sessions && !frozen => ("verifying", None),
        "verified" if sessions && !frozen => ("verified", None),
        _ => (committed, None),
    }
}

/// The running cohort's roster at `level`, for the cohort and stage `now`
/// shows: `None` at the closed level, outside the running stages, or with
/// nothing to build it from. The eval Worker's rows win (from the frozen
/// level); else, from scoring start, the published artifacts record
/// (`start`, from the scoring level).
pub fn running_roster(
    s: &Settings<'_>,
    level: RosterLevel,
    now: &NowDoc,
    e: Option<&EvalView>,
    live: Option<&LiveScoresView>,
    start: Option<&StartRecord>,
    now_ms: u64,
) -> Option<RosterDoc> {
    if level == RosterLevel::Closed {
        return None;
    }
    let (cohort, label) = (now.cohort.cohort?, now.cohort.label.clone()?);
    let stage = now.cohort.stage.as_str();
    if !RUNNING_STAGES.contains(&stage) {
        return None;
    }
    let evaluating = EVALUATING_STAGES.contains(&stage);
    let start = start.filter(|r| r.cohort == cohort && r.label == label);
    let artifact = |hotkey: &str| start.and_then(|r| r.entry(hotkey)).map(|e| state_str(e.state).to_owned());
    let adm: Option<&AdmissionCohortView> = (level >= RosterLevel::Frozen)
        .then(|| e?.admission.iter().find(|a| a.cohort == cohort))
        .flatten();
    // An entrant's counts: sessions level, eval progress for this cohort.
    let progress = adm.filter(|_| level >= RosterLevel::Sessions).and_then(|a| a.progress.as_ref());
    // The live values: at the live level, for this cohort, with every live
    // row an executed entrant the progress report names; otherwise none,
    // and the roster is written at the sessions level.
    let live = live
        .filter(|_| level == RosterLevel::Live)
        .filter(|l| l.cohort == cohort)
        // A stop, or a fault that stops every value: no live block.
        .filter(|l| !l.stopped && !matches!(l.fault.as_deref(), Some("release" | "over_bound")))
        .filter(|l| progress.is_some_and(|p| p.sealed && l.rows.iter().all(|r| p.row(&r.hotkey).is_some())));
    let live_row = |status: &str, hotkey: &[u8; 32]| -> Option<LiveRow> {
        let l = live.filter(|_| matches!(status, "admitted" | "evaluating"))?;
        let i = l.rows.binary_search_by(|r| r.hotkey.cmp(hotkey)).ok()?;
        let r = l.rows.get(i)?;
        Some(LiveRow { checkpoints: r.checkpoints.clone(), stopped: r.stopped, prefinal: r.prefinal.clone() })
    };
    let challenges = |status: &str, hotkey: &[u8; 32]| -> Option<Challenges> {
        let p = progress.filter(|_| matches!(status, "admitted" | "evaluating"))?;
        if !p.sealed {
            return Some(Challenges::pending(None));
        }
        // Sealed without a row: an entrant the seal does not execute.
        p.row(hotkey).map(Challenges::of)
    };
    let (source, as_of, rows) = match adm.and_then(|a| Some((a, a.hotkeys.as_ref()?))) {
        Some((a, h)) => {
            // The roster's outcome is shown once the page has left the
            // admission window, so the two never disagree.
            let frozen = a.frozen_epoch.is_some() && stage != "admission_open";
            let rows = h
                .rows
                .iter()
                .map(|r| {
                    let (status, reason_class) = status_of(r, level, frozen, evaluating);
                    let hotkey = ss58(&r.hotkey);
                    RosterRow {
                        challenges: challenges(status, &r.hotkey),
                        live: live_row(status, &r.hotkey),
                        uid: r.uid,
                        artifact_state: matches!(status, "admitted" | "evaluating")
                            .then(|| artifact(&hotkey))
                            .flatten(),
                        hotkey,
                        hotkey_hex: Some(hex::encode(r.hotkey)),
                        status: status.into(),
                        reason_class,
                        roster_position: None,
                        result: None,
                    }
                })
                .collect();
            ("admission", Some(AsOf { epoch: h.epoch, block: Some(h.block) }), rows)
        }
        None => {
            let r = start.filter(|_| evaluating)?;
            let rows = r
                .entrants
                .iter()
                .map(|en| RosterRow {
                    challenges: crate::ss58::decode(&en.hotkey).and_then(|a| challenges("evaluating", &a.0)),
                    live: crate::ss58::decode(&en.hotkey).and_then(|a| live_row("evaluating", &a.0)),
                    uid: Some(en.uid),
                    hotkey: en.hotkey.clone(),
                    hotkey_hex: crate::ss58::decode(&en.hotkey).map(|a| hex::encode(a.0)),
                    status: "evaluating".into(),
                    reason_class: None,
                    roster_position: None,
                    result: None,
                    artifact_state: Some(state_str(en.state).into()),
                })
                .collect();
            ("artifacts", None, rows)
        }
    };
    let block = live.map(live_block);
    // The live level exactly with its block: else the sessions level.
    let level = if level == RosterLevel::Live && block.is_none() { RosterLevel::Sessions } else { level };
    Some(RosterDoc {
        cohort: RosterCohort {
            cohort,
            label: label.clone(),
            stage: stage.into(),
        },
        level: level.as_str().into(),
        source: source.into(),
        as_of,
        rows: ordered(rows),
        references: None,
        live: block,
        ..header(s, &label, now_ms)
    })
}

/// The roster's live block from the eval Worker's checked view.
fn live_block(l: &LiveScoresView) -> LiveBlock {
    let grid = |w: u64| {
        let mut buf = [0u8; slop_ninja_protocol::quant::MAX_DECIMAL_LEN];
        slop_ninja_protocol::quant::format_grid(w, w, &mut buf).map(str::to_owned).unwrap_or_default()
    };
    LiveBlock {
        rule: slop_ninja_protocol::live::LIVE_RULE.into(),
        width: l.width.map(grid),
        segments: l.segments,
        released_max: l.segments.saturating_sub(1),
        noise_sd: l.noise_sd.clone(),
        eps_live_milli: l.eps_live_milli,
        bits: LiveBits { counted: l.bits.0, live: l.bits.1, bound: l.bits.2 },
        reference: l.reference.as_ref().map(|r| LiveReference {
            done: r.done,
            total: r.total,
            checkpoints: r.checkpoints.clone(),
            stopped: r.stopped,
        }),
        prefinal: l.prefinal.as_ref().map(|p| LivePrefinal {
            quantization: LiveQuantization { rule: slop_ninja_protocol::quant::QUANT_RULE.into(), width: grid(p.width) },
            current: p.current.clone(),
            predecessor: p.predecessor.clone(),
            margin_line: p.margin_line.clone(),
        }),
    }
}

/// The roster without its live values, at the sessions level: what a
/// refused live roster is written as, so a live defect never costs the
/// counts.
pub fn without_live(mut d: RosterDoc) -> RosterDoc {
    d.live = None;
    for r in &mut d.rows {
        r.live = None;
    }
    if d.level == RosterLevel::Live.as_str() {
        d.level = RosterLevel::Sessions.as_str().into();
    }
    d
}

// ------------------------------------------------------- shown values

/// One live value this publisher has published: never replaced by a later
/// run, and the pre-finalized ones are compared with the result at close.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveShown {
    pub cohort: u64,
    /// An entrant's hotkey (64 hex), `current`, `predecessor` or `margin`.
    pub subject: String,
    /// `reading` or `prefinal`.
    pub kind: String,
    pub checkpoint: u8,
    pub value: String,
}

/// Every live value of a running roster, as records.
pub fn live_values(d: &RosterDoc) -> Vec<LiveShown> {
    let mut out = Vec::new();
    let Some(b) = &d.live else { return out };
    let cohort = d.cohort.cohort;
    let mut push = |subject: &str, kind: &str, checkpoint: usize, value: &str| {
        out.push(LiveShown {
            cohort,
            subject: subject.into(),
            kind: kind.into(),
            checkpoint: u8::try_from(checkpoint).unwrap_or(u8::MAX),
            value: value.into(),
        });
    };
    if let Some(r) = &b.reference {
        for (i, v) in r.checkpoints.iter().enumerate() {
            push("current", "reading", i + 1, v);
        }
    }
    if let Some(p) = &b.prefinal {
        push("current", "prefinal", 0, &p.current);
        if let Some(v) = &p.predecessor {
            push("predecessor", "prefinal", 0, v);
        }
        if let Some(v) = &p.margin_line {
            push("margin", "prefinal", 0, v);
        }
    }
    for r in &d.rows {
        let (Some(lv), Some(hk)) = (&r.live, &r.hotkey_hex) else { continue };
        for (i, v) in lv.checkpoints.iter().enumerate() {
            push(hk, "reading", i + 1, v);
        }
        if let Some(v) = &lv.prefinal {
            push(hk, "prefinal", 0, v);
        }
    }
    out
}

/// Whether `d` would replace a value already published (the record is
/// keyed by cohort, subject, kind and checkpoint), checked against the
/// durable record, never against the last roster written, so it survives a
/// dropped block, a stop or a restart.
pub fn replaces_shown(shown: &[LiveShown], d: &RosterDoc) -> bool {
    live_values(d).iter().any(|v| {
        shown.iter().any(|s| {
            s.cohort == v.cohort && s.subject == v.subject && s.kind == v.kind && s.checkpoint == v.checkpoint && s.value != v.value
        })
    })
}

/// Adds `d`'s values to the record (each once), and drops the records of
/// cohorts two or more before `d`'s, whose close check can no longer run
/// on a running roster (an abandoned or declined cohort).
pub fn record_shown(shown: &mut Vec<LiveShown>, d: &RosterDoc) {
    let cohort = d.cohort.cohort;
    shown.retain(|s| s.cohort + 2 > cohort);
    for v in live_values(d) {
        if !shown.iter().any(|s| s.cohort == v.cohort && s.subject == v.subject && s.kind == v.kind && s.checkpoint == v.checkpoint) {
            shown.push(v);
        }
    }
}

/// The close check: every pre-finalized value shown for the summary's
/// cohort must be the result's (`q`, or `withheld`). Returns the subjects
/// that differ (the result's value is what the cohort's documents serve)
/// and drops the cohort's records.
pub fn close_check(shown: &mut Vec<LiveShown>, sm: &CohortSummary) -> Vec<String> {
    let mut bad = Vec::new();
    for s in shown.iter().filter(|s| s.cohort == sm.cohort && s.kind == "prefinal") {
        let signed = match s.subject.as_str() {
            "current" => sm.references.current.q.clone(),
            "predecessor" => sm.references.predecessor.as_ref().and_then(|p| p.q.clone()),
            "margin" => continue,
            hk => sm.entrants.iter().find(|e| e.hotkey_hex == hk).map(|e| match e.gate.as_str() {
                "withheld" => slop_ninja_protocol::live::WITHHELD.to_owned(),
                _ => e.q.clone().unwrap_or_default(),
            }),
        };
        if signed.as_deref() != Some(s.value.as_str()) {
            bad.push(s.subject.clone());
        }
    }
    shown.retain(|s| s.cohort != sm.cohort);
    bad
}
