//! A copy of the website's dashboard contract types (repository slop_ninja_web,
//! branch `tracker`, `src/model.rs`), compiled only into the publisher's
//! tests: every document the publisher writes must round-trip through these
//! types unchanged (no field the website drops, none it lacks; a `null` and
//! an absent key are the same value). Refresh it verbatim (everything below
//! this header) when the website's model changes.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

pub const NOW_SCHEMA: &str = "slop_ninja.dashboard.now/v1";
pub const COHORTS_SCHEMA: &str = "slop_ninja.dashboard.cohorts/v1";
pub const COHORT_SCHEMA: &str = "slop_ninja.dashboard.cohort/v1";
pub const INDEX_SCHEMA: &str = "slop_ninja.dashboard.index/v1";
pub const UNAVAILABLE_SCHEMA: &str = "slop_ninja.dashboard.unavailable/v1";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Chain {
    pub genesis: Option<String>,
    pub netuid: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Header {
    pub schema: String,
    pub network: String,
    pub chain: Chain,
    /// Unix seconds of the publisher run behind the document, to the minute.
    pub generated_at: Option<u64>,
    /// Always `false` outside the local sample build.
    pub sample: bool,
}

/// How the scores of one document were published: the coordinator's rule id
/// and grid width `w` (a decimal string). A score is shown only when both
/// are present and it carries no more decimal places than `w`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Quantization {
    pub rule: Option<String>,
    pub width: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Clock {
    pub confirmed_block: Option<u64>,
    pub confirmed_epoch: Option<u64>,
    /// Age of the coordinator's chain observation at `generated_at`.
    pub age_secs: Option<u64>,
    /// The block and epoch of the last observed tempo boundary.
    pub boundary_block: Option<u64>,
    pub boundary_epoch: Option<u64>,
    /// Read from the chain by the publisher; never a constant in the page.
    pub tempo_blocks: Option<u64>,
    pub block_secs_nominal: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pause {
    /// Set only by an explicit pause in the publisher's dashboard tree.
    pub paused: bool,
    /// A closed class (`coordinator_silent`, `waiting_on_infrastructure`,
    /// `operator_hold`, `calendar_paused`, `observation_lag`).
    pub class: Option<String>,
}

/// How a boundary point's time is known: seen live by the coordinator at
/// that boundary, computed from the evaluation engine's boundary records,
/// `unresolved` (no time: the engine has records around the boundary but
/// none supports one, or a check removed it), or (for a future boundary
/// only) the earliest it can be. No basis and no time: before tracking
/// began.
pub const BASES: &[&str] = &["observed", "derived", "unresolved", "earliest"];

/// The tempos one stage spanned, from its start boundary to its stop
/// boundary (to the last observed boundary for the current stage). A closed
/// object: any other key refuses the document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PhaseTempos {
    /// Epochs from the start boundary to the stop boundary.
    pub elapsed: Option<u64>,
    /// Credited tempos in that span (only tempos the coordinator observed
    /// live are credited).
    pub counted: Option<u64>,
    /// The template's requirement, for the two schedule stages only.
    pub required: Option<u64>,
}

/// A stage change, dated by the tempo boundary at which it was observed.
/// Stages are contiguous at boundaries: a stage stops where the next one
/// starts. A closed object: a time field outside the contract (a minute
/// inside a tempo, a block inside a tempo) refuses the document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Phase {
    pub stage: String,
    /// The tempo boundary (epoch) at which the stage was first observed.
    pub epoch: Option<u64>,
    /// That boundary's first block (the chain's `LastEpochBlock`), never a
    /// block inside a tempo.
    pub block: Option<u64>,
    /// That boundary's wall-clock time, to the minute.
    pub at: Option<u64>,
    /// `observed`, `derived` or `unresolved` (see [`BASES`]); `None` with no
    /// time and no record.
    pub basis: Option<String>,
    pub tempos: Option<PhaseTempos>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tempos {
    pub credited: Option<u64>,
    pub required: Option<u64>,
}

/// Every value is the earliest possible epoch or block: a lower bound.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Expected {
    pub stage_end_epoch: Option<u64>,
    pub stage_end_block: Option<u64>,
    pub close_epoch: Option<u64>,
    pub close_block: Option<u64>,
    pub packaging_deadline_epoch: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Admission {
    pub admitted: Option<u64>,
    pub roster_cap: Option<u64>,
}

/// The work-stage percent grid: 0, 10, ..., 90. A work stage reaches 100%
/// by ending, so 100 is never published.
pub const WORK_GRID: &[u64] = &[0, 10, 20, 30, 40, 50, 60, 70, 80, 90];

/// The current work stage's percent on [`WORK_GRID`], as of the last tempo
/// boundary. A closed object: any other key refuses the document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkProgress {
    pub stage: String,
    pub pct: u64,
    #[serde(default)]
    pub as_of_epoch: Option<u64>,
}

/// Aggregates for the active stage only: never per kind or per entrant, and
/// never an exact count for a work stage. A closed object: any other key
/// (`done`, `total`, a per-entrant map, the old bucket keys) refuses the
/// document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Progress {
    pub admission: Option<Admission>,
    pub work: Option<WorkProgress>,
}

/// The cohort's frozen template, in tempos.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Template {
    pub k_tempos: Option<u64>,
    pub admission_tempos: Option<u64>,
    pub transition_tempos: Option<u64>,
    /// `k_tempos - admission_tempos - transition_tempos`: the tempos left for
    /// closing, building, sealing, scoring and packaging.
    pub work_window_tempos: Option<u64>,
}

/// The bar stages of the `cohort/v1` stage set, in order, with their kind.
pub const BAR_STAGES: &[(&str, &str)] = &[
    ("admission_open", "schedule"),
    ("admission_closing", "step"),
    ("generating", "work"),
    ("sealing", "step"),
    ("scoring", "work"),
    ("packaging", "step"),
    ("transition", "schedule"),
    ("closed", "end"),
];

/// The kind of a `cohort/v1` bar stage, or `None` for any other code.
pub fn stage_kind(stage: &str) -> Option<&'static str> {
    BAR_STAGES
        .iter()
        .find(|(s, _)| *s == stage)
        .map(|(_, k)| *k)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StagePlan {
    pub stage: String,
    /// `schedule`, `work`, `step` or `end`.
    pub kind: String,
    /// The template length for schedule stages, `None` for work and step
    /// stages (they share the work window), 0 for the end.
    pub expected_tempos: Option<u64>,
    /// The publisher's credit-adjusted earliest start boundary, only for
    /// stages after the current one (`now.json` only).
    pub earliest_epoch: Option<u64>,
    pub earliest_block: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Plan {
    pub template: Template,
    pub stages: Vec<StagePlan>,
}

/// Reads a boolean, or `false` for anything else (the value is dropped, not
/// the document).
fn lenient_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(matches!(
        serde_json::Value::deserialize(d)?,
        serde_json::Value::Bool(true)
    ))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CohortNow {
    pub cohort: Option<u64>,
    pub label: Option<String>,
    pub stage: String,
    pub phases: Vec<Phase>,
    pub tempos: Option<Tempos>,
    pub expected: Expected,
    pub progress: Progress,
    pub plan: Option<Plan>,
    /// `true` once `cohorts/<label>.json` carries this cohort's results
    /// (stage 08 and later).
    #[serde(deserialize_with = "lenient_bool")]
    pub results: bool,
    pub mode: Option<String>,
    /// The cohort's opening, admission cutoff and close boundaries.
    pub bounds: Option<Bounds>,
    /// The frozen roster's hotkeys, sorted by hotkey string (never in roster
    /// or score order), from the freeze on. `None` until the publisher is
    /// cleared to emit it.
    pub roster: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Next {
    pub cohort: Option<u64>,
    pub earliest_open_epoch: Option<u64>,
    pub earliest_open_block: Option<u64>,
    pub admission_tempos: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Owner {
    pub uid: Option<u16>,
    pub hotkey: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Artifact {
    /// `published`, `pending` or `withheld`.
    pub state: String,
    /// A model file on the network's public host: an entrant artifact
    /// (`v1/artifacts/<sha256>.bin`) or a reference package manifest
    /// (`v1/reference/packages/<sha256>/package.json`). Nothing else.
    pub download: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Best {
    pub index: u64,
    /// Always `R<index>`; rebuilt by the Worker.
    pub label: String,
    pub since_cohort: Option<u64>,
    pub since_label: Option<String>,
    /// `None`: an operator reference package, not a miner's model.
    pub owner: Option<Owner>,
    pub latest_score: Option<String>,
    pub latest_score_label: Option<String>,
    pub quantization: Option<Quantization>,
    pub artifact: Option<Artifact>,
    pub licence: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NowDoc {
    #[serde(flatten)]
    pub header: Header,
    pub mechanism: String,
    pub clock: Clock,
    pub pause: Pause,
    pub cohort: CohortNow,
    pub next: Option<Next>,
    pub best: Option<Best>,
    /// The newest listed cohort's label (its document is `cohorts/<label>.json`).
    pub latest_cohort: Option<String>,
    /// Notice kinds; the page renders its own fixed wording for each.
    pub notice: Vec<String>,
    /// Reserved for validator advisory scores, which never affect rows. Not
    /// read from the publisher until the page renders them.
    #[serde(skip_deserializing)]
    pub advisory: Option<()>,
    /// `false` when the coordinator's or eval status did not answer the
    /// publisher's last run.
    pub sources_ok: Option<bool>,
    /// The coordinator's latest weight commit on chain, `None` before the
    /// publisher reports it.
    pub weights: Option<Weights>,
    /// The subnet's UID-to-hotkey map as of a tempo boundary, `None` before
    /// the publisher reports it. A map that does not parse drops alone; the
    /// rest of the document is kept.
    #[serde(deserialize_with = "lenient_uids")]
    pub uids: Option<Uids>,
}

/// The subnet's UID-to-hotkey map, as public chain facts: for each UID, the
/// hotkey of its latest registration that committed on the subnet, as of the
/// tempo boundary `epoch` (first block `block`). Rows in strictly ascending
/// UID order, each hotkey once. A UID whose new holder has not committed yet
/// still names its earlier holder. A closed object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Uids {
    pub epoch: u64,
    pub block: u64,
    pub rows: Vec<UidRow>,
}

/// One UID and its hotkey (SS58).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UidRow {
    pub uid: u16,
    pub hotkey: String,
}

/// Reads the UID map, or `None` for anything that is not one (the map is
/// dropped, not the document).
fn lenient_uids<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Uids>, D::Error> {
    Ok(serde_json::from_value(serde_json::Value::deserialize(d)?).unwrap_or(None))
}

/// The coordinator's latest commit-reveal weight commit: public chain facts
/// only. `epoch` is the tempo it committed for; `commit_block` and
/// `commit_index` locate its commit extrinsic; `reveal` is its outcome
/// (`pending`, `revealed` or `missed`: the chain will never reveal it) and
/// `reveal_block` the block whose events carry the chain's reveal (only with
/// `revealed`). No weights, no row and no extrinsic hash. A closed object,
/// like [`Phase`], whose `epoch`, `commit_block` and `reveal` are required.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Weights {
    pub epoch: u64,
    pub commit_block: u64,
    pub commit_index: Option<u32>,
    pub reveal: String,
    pub reveal_block: Option<u64>,
}

/// A tempo boundary: its epoch, its first block, its time to the minute and
/// how that time is known. A closed object, like [`Phase`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Mark {
    pub epoch: Option<u64>,
    /// The boundary's first block, never a block inside a tempo.
    pub block: Option<u64>,
    /// The boundary's time, to the minute.
    pub at: Option<u64>,
    /// `observed`, `derived`, `unresolved`, or `earliest` (an earliest
    /// close: no time).
    pub basis: Option<String>,
}

/// The clock's tempo boundary projected back to a cohort's opening at the
/// tempo in force: the reference every block of the row and its document
/// must agree with. A closed object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Anchor {
    pub epoch: Option<u64>,
    pub block: Option<u64>,
}

/// A cohort's three boundaries. `close` is `earliest` until the cohort
/// closes or ends.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Bounds {
    pub open: Option<Mark>,
    /// The admission cutoff; `None` for a cohort without coordinator
    /// admission.
    pub cutoff: Option<Mark>,
    pub close: Option<Mark>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CohortRow {
    pub cohort: u64,
    pub label: String,
    /// `closed`, `ended`, `published` (results out, not closed yet; the
    /// cohort document only), `publishing` or `retired_unpublished`.
    pub state: String,
    pub opened: Option<Mark>,
    pub admission_closed: Option<Mark>,
    pub published: Option<Mark>,
    pub closed: Option<Mark>,
    pub mode: Option<String>,
    pub promoted: Option<Owner>,
    pub entrants: Option<u32>,
    pub reference_index: Option<u64>,
    pub best_score: Option<String>,
    /// Applies to `best_score` and, in the cohort document, to every score.
    pub quantization: Option<Quantization>,
    /// Whether `cohorts/<label>.json` carries this cohort's leaderboard.
    pub leaderboard: bool,
    pub ended_class: Option<String>,
    /// Cohort documents only: the cohort's boundaries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    /// The clock's tempo length; with `anchor`, the reference of every block
    /// of the row (and its document). Without both, no block is kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tempo_blocks: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordRef {
    pub first_label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CohortsDoc {
    #[serde(flatten)]
    pub header: Header,
    pub mechanism: String,
    /// Ascending by cohort number.
    pub cohorts: Vec<CohortRow>,
    /// Earlier chain records, oldest first.
    pub records: Vec<RecordRef>,
    /// The label prefix of the live record, when one is configured.
    pub record_prefix: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entrant {
    pub uid: u16,
    pub hotkey: String,
    pub score: Option<String>,
    /// `kappa`: 1 for the top occupied band. Set exactly when `eligible`.
    pub band: Option<u32>,
    /// Met every gate and the quality cutoff, so it holds a reward band.
    pub eligible: bool,
    /// Share of the cohort's weight row, in percent to one decimal place.
    pub row_share_pct: Option<String>,
    pub gates_passed: bool,
    pub promoted: bool,
    pub artifact: Option<Artifact>,
}

/// The quality cutoff at grid precision, with its rule code.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cutoff {
    pub q: Option<String>,
    pub rule: Option<String>,
}

/// A reward band's edges at grid precision. For `higher` scores band 1's
/// `upper` is open (`None`); for `lower`, its `lower`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Band {
    pub band: u32,
    pub lower: Option<String>,
    pub upper: Option<String>,
}

/// The reward bands, from the top occupied band (1) down to the band whose
/// edge is the cutoff.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Quality {
    pub cutoff: Option<Cutoff>,
    pub bands: Option<Vec<Band>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReferenceRow {
    pub index: u64,
    /// Always `R<index>`; rebuilt by the Worker.
    pub label: String,
    pub score: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CohortDoc {
    #[serde(flatten)]
    pub header: Header,
    pub mechanism: String,
    pub cohort: CohortRow,
    pub phases: Vec<Phase>,
    /// As in `now.json`, without the earliest fields.
    pub plan: Option<Plan>,
    /// Only for `published` and `closed` cohorts.
    pub quality: Option<Quality>,
    /// `None` when this cohort's leaderboard is not published as data. No
    /// rank: entrants carry no order.
    pub entrants: Option<Vec<Entrant>>,
    pub reference: Option<ReferenceRow>,
    pub predecessor: Option<ReferenceRow>,
    /// `higher` or `lower`.
    pub score_better: Option<String>,
    /// A promotion rule code; the page owns the wording.
    pub promotion_rule: Option<String>,
    pub previous: Option<String>,
    pub next: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MechanismEntry {
    pub key: String,
    pub name: String,
    /// `live`, `planned`, `paused` or `retired`.
    pub state: String,
    pub chain_mechanism: Option<u16>,
    /// The stage vocabulary, so B and C can define their own later.
    pub stage_set: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexDoc {
    #[serde(flatten)]
    pub header: Header,
    pub mechanisms: Vec<MechanismEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Unavailable {
    pub schema: &'static str,
    pub available: bool,
    /// `not_enabled`, `upstream_unavailable`, `upstream_invalid` or `not_listed`.
    pub reason: &'static str,
}

impl Unavailable {
    pub fn new(reason: &'static str) -> Self {
        Self {
            schema: UNAVAILABLE_SCHEMA,
            available: false,
            reason,
        }
    }
}
