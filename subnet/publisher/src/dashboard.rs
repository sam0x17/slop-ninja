//! The public dashboard's documents (`dashboard/v1`): the mechanism
//! registry, the current cohort (`m/a/now.json`), the cohort list and one
//! document per listed or ended cohort, written each run into a dedicated
//! bucket that only the publisher writes and only the private dashboard feed
//! Worker reads ([`contract`], shared with that Worker).
//!
//! Every document is built from typed fields of the status views
//! ([`crate::status`]), the cohort index and the allow-listed cohort
//! summaries, never from a copy of a status response, and every write passes
//! [`check`]: its key is in the grammar, its bytes fit the kind's bound, it
//! carries its schema and only the paths the kind's allow-list names, no
//! forbidden key ([`crate::redact`]), `sample` false, and a work percent only
//! on the 10% grid for the current work stage. A document that fails is not
//! written and the run reports it (an alert).
//!
//! What the documents never carry (the held-out exposure review): anything
//! per document or per task, receipts, exact work counts (only a percent on
//! the grid 0, 10, ..., 90, read at a tempo boundary and kept until the
//! next), per-kind or per-entrant progress, ranks, exact scores (no score at
//! all until the coordinator publishes its quantized `q`, so no quantization
//! block either), fault codes, notes and step errors, spend, model names,
//! hosts and keys.
//!
//! Stage times are tempo boundaries. A stage is shown as of the last boundary
//! the coordinator observed: a change the engine logged at a boundary step
//! (an opening, a publication, a close) is seen at that boundary, any other
//! (work committing) at the next one ([`visible_phases`]). So the stage, its
//! phase list and the work percent move only when the epoch does.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use slop_ninja_protocol::ChainBinding;

use crate::index::{ArtifactEntry, ArtifactState, CohortEntry, CohortIndex};
use crate::layout::Cache;
use crate::status::{CalendarCurrent, CohortView, EvalView, Observed};
use crate::store::{Put, Store, StoreError};
use crate::summary::CohortSummary;

#[path = "../../dashboard-feed/src/contract.rs"]
pub mod contract;

pub use contract::Doc;

/// Bumped whenever a document's derivation changes, so every cohort
/// document is rebuilt once ([`Memo::cohorts_key`]).
pub const RULES: &str = "slop_ninja.dashboard.publisher/v2";

/// The `cohort/v1` bar stages in order, with their kind (progress-hero
/// spec, section 2).
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

/// The work-stage percent grid. A work stage reaches 100% by ending, so 100
/// is never written.
pub const WORK_GRID: &[u64] = &[0, 10, 20, 30, 40, 50, 60, 70, 80, 90];

/// The closed pause classes (PLAN section 3).
pub const PAUSE_CLASSES: &[&str] = &[
    "coordinator_silent",
    "waiting_on_infrastructure",
    "operator_hold",
    "calendar_paused",
    "observation_lag",
];

/// The stage codes `now.json` may carry: the bar stages plus `queued`,
/// `ended`, `idle` and `unknown`.
pub const NOW_STAGES: &[&str] = &[
    "queued",
    "admission_open",
    "admission_closing",
    "generating",
    "sealing",
    "scoring",
    "packaging",
    "transition",
    "closed",
    "ended",
    "idle",
    "unknown",
];

/// The promotion rule code of a testnet record (the leaderboard's
/// [`crate::leaderboard::PromotionRule`]); the page owns the wording.
pub const TESTNET_PROMOTION_RULE: &str = "testnet_no_canary";

pub fn stage_kind(stage: &str) -> Option<&'static str> {
    BAR_STAGES
        .iter()
        .find(|(s, _)| *s == stage)
        .map(|(_, k)| *k)
}

fn bar_index(stage: &str) -> Option<usize> {
    BAR_STAGES.iter().position(|(s, _)| *s == stage)
}

// ------------------------------------------------------------------ types
//
// Field for field the website's `src/model.rs` (a test round-trips every
// document through a copy of it). `deny_unknown_fields` everywhere, so the
// website's fixtures parse here only while the two agree.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Never {}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chain {
    pub genesis: Option<String>,
    pub netuid: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quantization {
    pub rule: Option<String>,
    pub width: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clock {
    pub confirmed_block: Option<u64>,
    pub confirmed_epoch: Option<u64>,
    pub age_secs: Option<u64>,
    pub boundary_block: Option<u64>,
    pub boundary_epoch: Option<u64>,
    pub tempo_blocks: Option<u64>,
    pub block_secs_nominal: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pause {
    pub paused: bool,
    pub class: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Phase {
    pub stage: String,
    /// The tempo boundary the stage started at (its stop is the next
    /// phase's start).
    pub epoch: Option<u64>,
    /// That boundary's first block.
    pub block: Option<u64>,
    /// That boundary's time, to the minute.
    pub at: Option<u64>,
    /// How `at` is known: `observed`, `derived`, `unresolved` (no time: the
    /// engine has records around the boundary but none supports one, or a
    /// check removed it) or `None` (no time: before tracking began).
    pub basis: Option<String>,
    pub tempos: Option<PhaseTempos>,
}

/// A stage's tempos: epochs from its start to its stop (to the observed
/// boundary for the current stage), credited tempos among them, and the
/// template's requirement for a schedule stage.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseTempos {
    pub elapsed: Option<u64>,
    pub counted: Option<u64>,
    pub required: Option<u64>,
}

/// A cohort's opening, admission cutoff and close as boundary points; the
/// close of a running cohort is its earliest close (`basis: earliest`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortBounds {
    pub open: Option<Mark>,
    pub cutoff: Option<Mark>,
    pub close: Option<Mark>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tempos {
    pub credited: Option<u64>,
    pub required: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    pub stage_end_epoch: Option<u64>,
    pub stage_end_block: Option<u64>,
    pub close_epoch: Option<u64>,
    pub close_block: Option<u64>,
    pub packaging_deadline_epoch: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Admission {
    pub admitted: Option<u64>,
    pub roster_cap: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkProgress {
    pub stage: String,
    pub pct: u64,
    pub as_of_epoch: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub admission: Option<Admission>,
    pub work: Option<WorkProgress>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub k_tempos: Option<u64>,
    pub admission_tempos: Option<u64>,
    pub transition_tempos: Option<u64>,
    pub work_window_tempos: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagePlan {
    pub stage: String,
    pub kind: String,
    pub expected_tempos: Option<u64>,
    pub earliest_epoch: Option<u64>,
    pub earliest_block: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub template: Template,
    pub stages: Vec<StagePlan>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortNow {
    pub cohort: Option<u64>,
    pub label: Option<String>,
    pub stage: String,
    pub phases: Vec<Phase>,
    pub tempos: Option<Tempos>,
    pub expected: Expected,
    pub progress: Progress,
    pub plan: Option<Plan>,
    pub results: bool,
    pub mode: Option<String>,
    pub bounds: Option<CohortBounds>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Next {
    pub cohort: Option<u64>,
    pub earliest_open_epoch: Option<u64>,
    pub earliest_open_block: Option<u64>,
    pub admission_tempos: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    pub uid: Option<u16>,
    pub hotkey: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub state: String,
    pub download: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Best {
    pub index: u64,
    pub label: String,
    pub since_cohort: Option<u64>,
    pub since_label: Option<String>,
    pub owner: Option<Owner>,
    pub latest_score: Option<String>,
    pub latest_score_label: Option<String>,
    pub quantization: Option<Quantization>,
    pub artifact: Option<Artifact>,
    pub licence: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NowDoc {
    pub schema: String,
    pub network: String,
    pub chain: Chain,
    pub generated_at: Option<u64>,
    pub sample: bool,
    pub mechanism: String,
    pub clock: Clock,
    pub pause: Pause,
    pub cohort: CohortNow,
    pub next: Option<Next>,
    pub best: Option<Best>,
    pub latest_cohort: Option<String>,
    pub notice: Vec<String>,
    /// Reserved for validator advisory scores; always `null`.
    pub advisory: Option<Never>,
    pub sources_ok: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mark {
    pub epoch: Option<u64>,
    pub block: Option<u64>,
    pub at: Option<u64>,
    /// `observed`, `derived`, `unresolved`, `earliest` (a bound's earliest
    /// close only) or `None` (see [`Phase::basis`]).
    pub basis: Option<String>,
}

/// The clock's tempo boundary projected to a listed cohort's opening at the
/// tempo in force (`clock.boundary_block - (clock.boundary_epoch - epoch) *
/// tempo_blocks`): the reference every block of the row and its
/// document must agree with. Stable while the tempo is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub epoch: Option<u64>,
    pub block: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortRow {
    pub cohort: u64,
    pub label: String,
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
    pub quantization: Option<Quantization>,
    pub leaderboard: bool,
    pub ended_class: Option<String>,
    /// Cohort documents only (`None` in the list).
    pub bounds: Option<CohortBounds>,
    /// The clock's tempo length, with [`CohortRow::anchor`] the reference of
    /// every block of the row (and its document). Without both, no block.
    pub tempo_blocks: Option<u64>,
    pub anchor: Option<Anchor>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordRef {
    pub first_label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortsDoc {
    pub schema: String,
    pub network: String,
    pub chain: Chain,
    pub generated_at: Option<u64>,
    pub sample: bool,
    pub mechanism: String,
    pub cohorts: Vec<CohortRow>,
    pub records: Vec<RecordRef>,
    pub record_prefix: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entrant {
    pub uid: u16,
    pub hotkey: String,
    pub score: Option<String>,
    pub band: Option<u32>,
    pub eligible: bool,
    pub row_share_pct: Option<String>,
    pub gates_passed: bool,
    pub promoted: bool,
    pub artifact: Option<Artifact>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cutoff {
    pub q: Option<String>,
    pub rule: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Band {
    pub band: u32,
    pub lower: Option<String>,
    pub upper: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quality {
    pub cutoff: Option<Cutoff>,
    pub bands: Option<Vec<Band>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceRow {
    pub index: u64,
    pub label: String,
    pub score: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortDoc {
    pub schema: String,
    pub network: String,
    pub chain: Chain,
    pub generated_at: Option<u64>,
    pub sample: bool,
    pub mechanism: String,
    pub cohort: CohortRow,
    pub phases: Vec<Phase>,
    pub plan: Option<Plan>,
    pub quality: Option<Quality>,
    pub entrants: Option<Vec<Entrant>>,
    pub reference: Option<ReferenceRow>,
    pub predecessor: Option<ReferenceRow>,
    pub score_better: Option<String>,
    pub promotion_rule: Option<String>,
    pub previous: Option<String>,
    pub next: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MechanismEntry {
    pub key: String,
    pub name: String,
    pub state: String,
    pub chain_mechanism: Option<u16>,
    pub stage_set: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexDoc {
    pub schema: String,
    pub network: String,
    pub chain: Chain,
    pub generated_at: Option<u64>,
    pub sample: bool,
    pub mechanisms: Vec<MechanismEntry>,
}

// ------------------------------------------------------------ allow-list

const HEADER: &[&str] = &["schema", "network", "chain", "chain.genesis", "chain.netuid", "generated_at", "sample"];

const INDEX_PATHS: &[&str] = &[
    "mechanisms",
    "mechanisms[].key",
    "mechanisms[].name",
    "mechanisms[].state",
    "mechanisms[].chain_mechanism",
    "mechanisms[].stage_set",
];

const PHASE: &[&str] = &[
    "stage",
    "epoch",
    "block",
    "at",
    "basis",
    "tempos",
    "tempos.elapsed",
    "tempos.counted",
    "tempos.required",
];
const STAGE_PLAN: &[&str] = &["stage", "kind", "expected_tempos", "earliest_epoch", "earliest_block"];
const TEMPLATE: &[&str] = &["k_tempos", "admission_tempos", "transition_tempos", "work_window_tempos"];
const QUANT: &[&str] = &["rule", "width"];
const OWNER: &[&str] = &["uid", "hotkey"];
const ARTIFACT: &[&str] = &["state", "download"];
const MARK: &[&str] = &["epoch", "block", "at", "basis"];
const BOUNDS: &[&str] = &["open", "cutoff", "close"];

const NOW_PATHS: &[&str] = &[
    "mechanism",
    "clock",
    "clock.confirmed_block",
    "clock.confirmed_epoch",
    "clock.age_secs",
    "clock.boundary_block",
    "clock.boundary_epoch",
    "clock.tempo_blocks",
    "clock.block_secs_nominal",
    "pause",
    "pause.paused",
    "pause.class",
    "cohort",
    "cohort.cohort",
    "cohort.label",
    "cohort.stage",
    "cohort.phases",
    "cohort.tempos",
    "cohort.tempos.credited",
    "cohort.tempos.required",
    "cohort.expected",
    "cohort.expected.stage_end_epoch",
    "cohort.expected.stage_end_block",
    "cohort.expected.close_epoch",
    "cohort.expected.close_block",
    "cohort.expected.packaging_deadline_epoch",
    "cohort.progress",
    "cohort.progress.admission",
    "cohort.progress.admission.admitted",
    "cohort.progress.admission.roster_cap",
    "cohort.progress.work",
    "cohort.progress.work.stage",
    "cohort.progress.work.pct",
    "cohort.progress.work.as_of_epoch",
    "cohort.plan",
    "cohort.plan.template",
    "cohort.plan.stages",
    "cohort.results",
    "cohort.mode",
    "cohort.bounds",
    "next",
    "next.cohort",
    "next.earliest_open_epoch",
    "next.earliest_open_block",
    "next.admission_tempos",
    "best",
    "best.index",
    "best.label",
    "best.since_cohort",
    "best.since_label",
    "best.owner",
    "best.latest_score",
    "best.latest_score_label",
    "best.quantization",
    "best.artifact",
    "best.licence",
    "latest_cohort",
    "notice",
    "advisory",
    "sources_ok",
];

const ROW: &[&str] = &[
    "cohort",
    "label",
    "state",
    "opened",
    "admission_closed",
    "published",
    "closed",
    "mode",
    "promoted",
    "entrants",
    "reference_index",
    "best_score",
    "quantization",
    "leaderboard",
    "ended_class",
    "bounds",
    "tempo_blocks",
    "anchor",
    "anchor.epoch",
    "anchor.block",
];

const ENTRANT: &[&str] = &[
    "uid",
    "hotkey",
    "score",
    "band",
    "eligible",
    "row_share_pct",
    "gates_passed",
    "promoted",
    "artifact",
];

const COHORT_PATHS: &[&str] = &[
    "mechanism",
    "cohort",
    "phases",
    "plan",
    "plan.template",
    "plan.stages",
    "quality",
    "quality.cutoff",
    "quality.cutoff.q",
    "quality.cutoff.rule",
    "quality.bands",
    "quality.bands[].band",
    "quality.bands[].lower",
    "quality.bands[].upper",
    "entrants",
    "reference",
    "reference.index",
    "reference.label",
    "reference.score",
    "predecessor",
    "predecessor.index",
    "predecessor.label",
    "predecessor.score",
    "score_better",
    "promotion_rule",
    "previous",
    "next",
];

/// Every path a document of `doc`'s kind may carry, in
/// [`crate::allowlist::paths`]'s form.
pub fn allowed_paths(doc: &Doc) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = HEADER.iter().map(|s| (*s).to_owned()).collect();
    let mut add = |prefix: &str, list: &[&str]| {
        for p in list {
            out.insert(if prefix.is_empty() {
                (*p).to_owned()
            } else {
                format!("{prefix}.{p}")
            });
        }
    };
    match doc {
        Doc::Index => add("", INDEX_PATHS),
        Doc::Now(_) => {
            add("", NOW_PATHS);
            add("cohort.phases[]", PHASE);
            add("cohort.plan.template", TEMPLATE);
            add("cohort.plan.stages[]", STAGE_PLAN);
            add("best.owner", OWNER);
            add("best.quantization", QUANT);
            add("best.artifact", ARTIFACT);
            add("cohort.bounds", BOUNDS);
            for b in BOUNDS {
                add(&format!("cohort.bounds.{b}"), MARK);
            }
        }
        Doc::Cohorts(_) => {
            add("", &["mechanism", "cohorts", "records", "records[].first_label", "record_prefix"]);
            add("cohorts[]", ROW);
            for m in ["opened", "admission_closed", "published", "closed"] {
                add(&format!("cohorts[].{m}"), MARK);
            }
            add("cohorts[].promoted", OWNER);
            add("cohorts[].quantization", QUANT);
        }
        Doc::Cohort(..) => {
            add("", COHORT_PATHS);
            add("cohort", ROW);
            for m in ["opened", "admission_closed", "published", "closed"] {
                add(&format!("cohort.{m}"), MARK);
            }
            add("cohort.promoted", OWNER);
            add("cohort.quantization", QUANT);
            add("cohort.bounds", BOUNDS);
            for b in BOUNDS {
                add(&format!("cohort.bounds.{b}"), MARK);
            }
            add("phases[]", PHASE);
            add("plan.template", TEMPLATE);
            add("plan.stages[]", STAGE_PLAN);
            add("entrants[]", ENTRANT);
            add("entrants[].artifact", ARTIFACT);
        }
        // Month documents (P3) are not written yet: nothing is allowed.
        Doc::Month(..) => return BTreeSet::new(),
    }
    out
}

/// [`crate::redact::scan_value`], with the two contract keys it refuses by
/// name allowed exactly where the contract puts them and only with their
/// closed values: `kind` (a stage kind in a plan) and `results` (a boolean).
pub fn scan(v: &Value) -> Result<(), String> {
    fn walk(v: &Value, path: &mut Vec<String>) -> Result<(), String> {
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    let at = path.join(".");
                    let excepted = match k.as_str() {
                        "kind" => {
                            (at.ends_with("plan.stages[]"))
                                && child
                                    .as_str()
                                    .is_some_and(|s| matches!(s, "schedule" | "work" | "step" | "end"))
                        }
                        "results" => at == "cohort" && child.is_boolean(),
                        _ => false,
                    };
                    if !excepted {
                        crate::redact::scan_value(&serde_json::json!({ k: Value::Null }))
                            .map_err(|_| format!("forbidden key {at}.{k}"))?;
                    }
                    path.push(k.clone());
                    walk(child, path)?;
                    path.pop();
                }
                Ok(())
            }
            Value::Array(items) => {
                let last = path.pop().unwrap_or_default();
                path.push(format!("{last}[]"));
                for child in items {
                    walk(child, path)?;
                }
                Ok(())
            }
            Value::String(s) if s.len() > crate::redact::MAX_PUBLIC_STRING => {
                Err(format!("string of {} bytes at {}", s.len(), path.join(".")))
            }
            _ => Ok(()),
        }
    }
    walk(v, &mut Vec::new())
}

/// Checks one dashboard write. Content-free errors.
pub fn check(key: &str, bytes: &[u8]) -> Result<Doc, String> {
    let doc = Doc::parse_key(key).ok_or_else(|| format!("{key}: outside the dashboard grammar"))?;
    if bytes.len() > doc.bound() {
        return Err(format!(
            "{key}: {} bytes exceed the bound of {} ({})",
            bytes.len(),
            doc.bound(),
            contract::BOUNDS_RULE
        ));
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|_| format!("{key}: not JSON"))?;
    if v.get("schema").and_then(Value::as_str) != Some(doc.schema()) {
        return Err(format!("{key}: not schema {}", doc.schema()));
    }
    if v.get("sample") != Some(&Value::Bool(false)) {
        return Err(format!("{key}: sample is not false"));
    }
    let allowed = allowed_paths(&doc);
    let extra: Vec<String> = crate::allowlist::paths(&v)
        .into_iter()
        .filter(|p| !allowed.contains(p))
        .collect();
    if !extra.is_empty() {
        return Err(format!("{key}: fields outside the allow-list: {}", extra.join(", ")));
    }
    scan(&v).map_err(|why| format!("{key}: {why}"))?;
    if let Doc::Now(_) = doc {
        let c = &v["cohort"];
        let stage = c["stage"].as_str().unwrap_or_default();
        if !NOW_STAGES.contains(&stage) {
            return Err(format!("{key}: stage outside the closed set"));
        }
        let work = &c["progress"]["work"];
        if !work.is_null() {
            let pct = work["pct"].as_u64();
            if !pct.is_some_and(|p| WORK_GRID.contains(&p)) || !work["pct"].is_u64() {
                return Err(format!("{key}: work percent off the 10% grid"));
            }
            if work["stage"].as_str() != Some(stage) || stage_kind(stage) != Some("work") {
                return Err(format!("{key}: work percent outside the current work stage"));
            }
        }
        if let Some(class) = v["pause"]["class"].as_str()
            && !PAUSE_CLASSES.contains(&class)
        {
            return Err(format!("{key}: pause class outside the closed list"));
        }
    }
    if let Doc::Now(_) = doc {
        // No stage or bound dated after the boundary the clock reports: a
        // stage is shown as of the last boundary, so a later epoch would
        // show a change before its boundary (the work-timing guard).
        let c = &v["cohort"];
        let clock = v["clock"]["boundary_epoch"].as_u64().or(v["clock"]["confirmed_epoch"].as_u64());
        let late = |p: &Value| p["epoch"].as_u64().is_some_and(|e| clock.is_none_or(|x| e > x));
        let phases = c["phases"].as_array().map(Vec::as_slice).unwrap_or_default();
        let bounds = ["open", "cutoff", "close"]
            .iter()
            .map(|b| &c["bounds"][*b])
            .filter(|m| m["basis"].as_str() != Some("earliest"));
        if phases.iter().chain(bounds).any(late) {
            return Err(format!("{key}: a stage dated after the clock's boundary"));
        }
    }
    if let Doc::Cohort(..) = doc {
        let state = v["cohort"]["state"].as_str().unwrap_or_default();
        if !matches!(state, "published" | "closed") && (!v["entrants"].is_null() || !v["quality"].is_null()) {
            return Err(format!("{key}: entrants before results"));
        }
        let phases = v["phases"].as_array().map(Vec::as_slice).unwrap_or_default();
        if let Some(end) = phases.iter().position(|p| matches!(p["stage"].as_str(), Some("closed" | "ended")))
            && end + 1 < phases.len()
        {
            return Err(format!("{key}: a stage after the cohort's end"));
        }
    }
    // Stage times: the document parses into the contract's types (no field
    // outside them, such as a work commit's own time) and the checks of
    // [`clean_now`], [`clean_cohorts`] and [`clean_cohort`] change nothing.
    fn unchanged<T: Clone + PartialEq + for<'d> Deserialize<'d>>(v: Value, clean: fn(&mut T)) -> Result<(), &'static str> {
        let d: T = serde_json::from_value(v).map_err(|_| "not the contract's shape")?;
        let mut c = d.clone();
        clean(&mut c);
        if c == d { Ok(()) } else { Err("stage times or blocks fail the contract's checks") }
    }
    match doc {
        Doc::Now(_) => unchanged::<NowDoc>(v, clean_now),
        Doc::Cohorts(_) => unchanged::<CohortsDoc>(v, clean_cohorts),
        Doc::Cohort(..) => unchanged::<CohortDoc>(v, clean_cohort),
        _ => Ok(()),
    }
    .map_err(|why| format!("{key}: {why}"))?;
    Ok(doc)
}

// ------------------------------------------------------------ stage times
//
// The contract's checks of boundary points (PLAN section 9; the website's
// `clean.rs` runs the same): every block agrees with a reference boundary
// (the clock's, or a listed cohort's anchor projected from it) at the
// clock's tempo, so it can only be a tempo-boundary block, and without a
// reference no block is kept; every time is on the minute, no later than
// the document and never going backwards with the epoch; equal epochs carry
// equal points; and each value says how it is known. The builders run these
// checks before writing, and [`check`] refuses a document they would change.
//
// What the checks cannot verify: that a time is the boundary's time rather
// than another minute inside the same tempo. That rests on the producer
// (the engine's boundary journal holds only boundary observations and
// boundary-step records, never a work commit's time) and is stated in the
// contract.

/// A boundary point's fields, for the checks.
struct Pt<'a> {
    epoch: Option<u64>,
    block: &'a mut Option<u64>,
    at: &'a mut Option<u64>,
    basis: &'a mut Option<String>,
    /// A phase (phase epochs never go back).
    phase: bool,
    /// A bound's close, which may be an earliest estimate.
    earliest_ok: bool,
}

fn pt_phase(p: &mut Phase) -> Pt<'_> {
    Pt {
        epoch: p.epoch,
        block: &mut p.block,
        at: &mut p.at,
        basis: &mut p.basis,
        phase: true,
        earliest_ok: false,
    }
}

fn pt_mark(m: &mut Mark, earliest_ok: bool) -> Pt<'_> {
    Pt {
        epoch: m.epoch,
        block: &mut m.block,
        at: &mut m.at,
        basis: &mut m.basis,
        phase: false,
        earliest_ok,
    }
}

/// `at` only with `observed` or `derived`; `earliest` only on a close and
/// without `at`; a time removed (or never derivable) is `unresolved`, so it
/// never reads as before tracking; nothing without an epoch.
fn couple(p: &mut Pt<'_>) {
    if p.epoch.is_none() {
        (*p.block, *p.at, *p.basis) = (None, None, None);
        return;
    }
    match p.basis.as_deref() {
        Some("earliest") if p.earliest_ok => *p.at = None,
        Some("observed" | "derived") if p.at.is_some() => {}
        Some("observed" | "derived" | "unresolved") => (*p.at, *p.basis) = (None, Some("unresolved".into())),
        _ => (*p.at, *p.basis) = (None, None),
    }
}

/// The point checks over every point of one document. `tempo` is the tempo
/// length the blocks must follow and `reference` a boundary (epoch, block)
/// every block must agree with; without both, no block is kept.
fn clean_points(pts: &mut [Pt<'_>], generated_at: Option<u64>, tempo: Option<u64>, reference: Option<(u64, u64)>) {
    for p in pts.iter_mut() {
        couple(p);
        if p.at.is_some_and(|a| a % 60 != 0 || generated_at.is_none_or(|g| a > g)) {
            *p.at = None;
            couple(p);
        }
    }
    // Phase epochs never go back, and every point at one boundary (phases,
    // marks and bounds alike, except an earliest estimate) carries its one
    // point.
    let phases: Vec<usize> = (0..pts.len()).filter(|&i| pts[i].phase).collect();
    let back = phases
        .windows(2)
        .any(|w| matches!((pts[w[0]].epoch, pts[w[1]].epoch), (Some(x), Some(y)) if y < x));
    let firm: Vec<usize> = (0..pts.len()).filter(|&i| pts[i].basis.as_deref() != Some("earliest")).collect();
    let split = firm.iter().enumerate().any(|(k, &i)| {
        firm[k + 1..].iter().any(|&j| {
            let (a, b) = (&pts[i], &pts[j]);
            a.epoch.is_some() && a.epoch == b.epoch && (*a.block, *a.at, &*a.basis) != (*b.block, *b.at, &*b.basis)
        })
    });
    if back || split {
        for p in pts.iter_mut() {
            (*p.block, *p.at) = (None, None);
        }
    }
    // Times never go back as the epoch grows.
    let timed: Vec<(u64, u64)> = pts.iter().filter_map(|p| Some((p.epoch?, (*p.at)?))).collect();
    if timed.iter().any(|(e1, a1)| timed.iter().any(|(e2, a2)| e1 < e2 && a1 > a2)) {
        for p in pts.iter_mut() {
            *p.at = None;
        }
    }
    // Every block is a boundary block: it agrees with the reference at the
    // tempo; with no reference or tempo, none is kept.
    // `tempo_blocks` is the chain's boundary interval: consecutive tempo
    // boundaries are that many blocks apart (the live journal: 360).
    let step = tempo.filter(|t| *t > 0).map(i128::from);
    for p in pts.iter_mut() {
        let ok = match (step, reference, p.epoch, *p.block) {
            (Some(st), Some((re, rb)), Some(e), Some(b)) => {
                i128::from(b) - i128::from(rb) == (i128::from(e) - i128::from(re)) * st
            }
            _ => false,
        };
        if !ok {
            *p.block = None;
        }
    }
    for p in pts.iter_mut() {
        couple(p);
    }
}

/// The stage tempos checks: `elapsed` is the epochs to the next phase's
/// start (to `current` for the last phase of a running cohort), `counted`
/// at most that, `required` only on the schedule stages and within the
/// template. A phase that fails loses its tempos.
fn clean_tempos(phases: &mut [Phase], current: Option<u64>, plan: Option<&Plan>) {
    let template = plan.map(|p| &p.template);
    let next_epochs: Vec<Option<u64>> = (0..phases.len()).map(|i| phases.get(i + 1).and_then(|n| n.epoch)).collect();
    let n = phases.len();
    for (i, p) in phases.iter_mut().enumerate() {
        let Some(t) = &p.tempos else { continue };
        let instant = matches!(p.stage.as_str(), "closed" | "ended");
        let stop = if i + 1 < n { next_epochs[i] } else { current.filter(|_| !instant) };
        let expected = stop.zip(p.epoch).and_then(|(s, e)| s.checked_sub(e));
        let required_ok = match p.stage.as_str() {
            "admission_open" => t.required.is_none() || t.required == template.and_then(|t| t.admission_tempos),
            "transition" => t.required.is_none_or(|r| {
                template.is_some_and(|t| t.transition_tempos.is_some_and(|tt| r >= tt) && t.k_tempos.is_some_and(|k| r <= k))
            }),
            _ => t.required.is_none(),
        };
        let ok = !instant
            && expected.is_some()
            && t.elapsed == expected
            && t.counted.is_none_or(|c| Some(c) <= t.elapsed)
            && required_ok;
        if !ok {
            p.tempos = None;
        }
    }
}

/// The bound checks: the opening and cutoff are those phases' boundaries;
/// the close is an earliest estimate exactly while the cohort runs, and
/// then the expected close (`earliest`, epoch and block; `None` where no
/// estimate may appear).
fn clean_bounds(b: &mut Option<CohortBounds>, phases: &[Phase], earliest: Option<(Option<u64>, Option<u64>)>) {
    let Some(bounds) = b.as_mut() else { return };
    let epoch_of = |st: &str| phases.iter().find(|p| p.stage == st).map(|p| p.epoch);
    if bounds.open.as_ref().is_some_and(|o| epoch_of("admission_open") != Some(o.epoch)) {
        bounds.open = None;
    }
    if let (Some(c), Some(pe)) = (&bounds.cutoff, epoch_of("admission_closing"))
        && c.epoch != pe
    {
        bounds.cutoff = None;
    }
    let running = !phases.last().is_some_and(|p| matches!(p.stage.as_str(), "closed" | "ended"));
    if let Some(c) = &bounds.close {
        let est = c.basis.as_deref() == Some("earliest");
        let ok = if est {
            running && earliest.is_some_and(|(e, blk)| e == c.epoch && blk == c.block)
        } else {
            !running && epoch_of("closed").is_some_and(|e| e == c.epoch)
        };
        if !ok {
            bounds.close = None;
        }
    }
}

fn bound_points(b: &mut Option<CohortBounds>) -> Vec<Pt<'_>> {
    let Some(b) = b.as_mut() else { return Vec::new() };
    let CohortBounds { open, cutoff, close } = b;
    let mut out = Vec::new();
    out.extend(open.as_mut().map(|m| pt_mark(m, false)));
    out.extend(cutoff.as_mut().map(|m| pt_mark(m, false)));
    out.extend(close.as_mut().map(|m| pt_mark(m, true)));
    out
}

fn row_points(r: &mut CohortRow) -> Vec<Pt<'_>> {
    let CohortRow {
        opened,
        admission_closed,
        published,
        closed,
        ..
    } = r;
    [opened, admission_closed, published, closed]
        .into_iter()
        .filter_map(|m| m.as_mut().map(|m| pt_mark(m, false)))
        .collect()
}

/// [`clean_points`], [`clean_tempos`] and [`clean_bounds`] over `now.json`:
/// blocks follow the clock's tempo and boundary; the last phase of a running
/// cohort counts to the clock's boundary epoch.
pub fn clean_now(d: &mut NowDoc) {
    let c = &mut d.cohort;
    let earliest = Some((c.expected.close_epoch, c.expected.close_block));
    clean_bounds(&mut c.bounds, &c.phases, earliest);
    clean_tempos(&mut c.phases, d.clock.boundary_epoch, c.plan.as_ref());
    if c.stage == "transition"
        && let (Some(t), Some(p)) = (&c.tempos, &c.plan)
        && t.required.is_some_and(|r| {
            p.template.transition_tempos.is_some_and(|tt| r < tt) || p.template.k_tempos.is_some_and(|k| r > k)
        })
    {
        c.tempos = None;
    }
    let reference = d.clock.boundary_epoch.zip(d.clock.boundary_block);
    let CohortNow { phases, bounds, .. } = c;
    let mut pts: Vec<Pt<'_>> = phases.iter_mut().map(pt_phase).collect();
    pts.extend(bound_points(bounds));
    clean_points(&mut pts, d.generated_at, d.clock.tempo_blocks, reference);
}

/// A row's reference boundary, when its anchor is whole.
fn anchor_of(r: &CohortRow) -> Option<(u64, u64)> {
    r.anchor.as_ref().and_then(|a| a.epoch.zip(a.block))
}

/// The checks over the cohort list: each row's marks against its anchor at
/// the clock's tempo; bounds are not list fields.
pub fn clean_cohorts(d: &mut CohortsDoc) {
    for r in &mut d.cohorts {
        r.bounds = None;
        let (tempo, reference) = (r.tempo_blocks, anchor_of(r));
        clean_points(&mut row_points(r), d.generated_at, tempo, reference);
    }
}

/// The checks over one cohort document: its phases, row marks and bounds
/// against the row's anchor at the clock's tempo; no earliest estimate
/// appears.
pub fn clean_cohort(d: &mut CohortDoc) {
    clean_bounds(&mut d.cohort.bounds, &d.phases, None);
    clean_tempos(&mut d.phases, None, d.plan.as_ref());
    let (tempo, reference) = (d.cohort.tempo_blocks, anchor_of(&d.cohort));
    let CohortRow { bounds, .. } = &mut d.cohort;
    let mut bounds = bounds.take();
    {
        let mut pts: Vec<Pt<'_>> = d.phases.iter_mut().map(pt_phase).collect();
        pts.extend(row_points(&mut d.cohort));
        pts.extend(bound_points(&mut bounds));
        clean_points(&mut pts, d.generated_at, tempo, reference);
    }
    d.cohort.bounds = bounds;
}

/// The dashboard bucket: every write runs [`check`], and nothing else is
/// offered (no delete, no list, no copy).
pub struct DashboardStore<'a, S>(&'a S);

impl<'a, S: Store> DashboardStore<'a, S> {
    pub fn new(store: &'a S) -> Self {
        Self(store)
    }

    pub async fn head(&self, key: &str) -> Result<Option<crate::store::ObjectMeta>, StoreError> {
        self.0.head(key).await
    }

    /// Writes `bytes` at `key` once [`check`] accepts them, with their
    /// SHA-256 recorded by the store.
    pub async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), StoreError> {
        let doc = check(key, &bytes).map_err(|_| StoreError::NotAllowed)?;
        let sha = slop_ninja_protocol::object_sha256(&bytes);
        let cache = match doc.max_age() {
            60 => Cache::Snapshot,
            secs => Cache::Bounded(u64::from(secs)),
        };
        self.0
            .put(
                key,
                bytes,
                &Put {
                    content_type: "application/json",
                    cache,
                    sha256: Some(&sha),
                },
            )
            .await
    }
}

// ------------------------------------------------------------- derivation

/// What the publisher keeps between runs for the dashboard (in its Durable
/// Object state): the values it reads only at a tempo boundary, and the
/// cohort documents' rebuild cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memo {
    /// The boundary (epoch) the latched values were read at.
    #[serde(default)]
    pub epoch: Option<u64>,
    /// The cohort and stage they belong to.
    #[serde(default)]
    pub cohort: Option<u64>,
    #[serde(default)]
    pub stage: Option<String>,
    /// The work percent on [`WORK_GRID`], for a work stage.
    #[serde(default)]
    pub pct: Option<u64>,
    /// The pause class a cohort fault gives (`operator_hold` or
    /// `waiting_on_infrastructure`), read at the boundary too: a fault turns
    /// on when hidden work fails, so its time is not published finer.
    #[serde(default)]
    pub hold: Option<String>,
    /// The minute (unix seconds) each recent boundary was first observed by
    /// a run whose save succeeded (a failed save restores the memo), for
    /// a stage seen at it ([`visible_phases`]). Pruned to the current and
    /// the previous cohort. Its keys read from numbers or decimal strings
    /// ([`crate::durable::u64_keys`]): the state calls carry it as JSON.
    #[serde(default, with = "crate::durable::u64_keys")]
    pub seen: BTreeMap<u64, u64>,
    /// The inputs of the cohort documents last written in full
    /// ([`cohorts_key`]); a change rebuilds them.
    #[serde(default)]
    pub cohorts_key: Option<String>,
    /// The inputs of a rebuild that ran out of wall time, and the last
    /// cohort document it wrote (`cursor`): the next run with the same
    /// inputs continues after it; other inputs restart the rebuild.
    #[serde(default)]
    pub pending_key: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
}

/// The settings the documents depend on.
pub struct Settings<'a> {
    pub chain: &'a ChainBinding,
    pub tempo_blocks: u64,
    pub block_secs: u64,
    /// The coordinator counts as silent once its chain observation is older
    /// than this (the stalled-watermark alert's bound).
    pub silent_ms: u64,
    /// An operator notice is configured.
    pub operator_notice: bool,
    pub record_prefix: Option<&'a str>,
    pub publish_from_epoch: Option<u64>,
}

fn minute(ms: u64) -> u64 {
    let s = ms / 1000;
    s - s % 60
}

fn chain(c: &ChainBinding) -> Chain {
    Chain {
        genesis: Some(format!("0x{}", c.genesis.hex())),
        netuid: Some(u64::from(c.netuid)),
    }
}

/// A cohort of the public record: not a load cohort or a probe build (whose
/// close commits with work, not in a boundary step), its label under the
/// record prefix, opened at or after the record's first epoch.
pub fn in_record(c: &CohortView, s: &Settings<'_>) -> bool {
    !c.load
        && !c.dash.probe_build
        && s.record_prefix.is_none_or(|p| c.label.starts_with(p))
        && contract::safe_label(&c.label)
        && s
            .publish_from_epoch
            .is_none_or(|f| c.opened_epoch.is_none_or(|o| o >= f))
}

/// The display stage of an engine phase; `None` for a phase the dashboard
/// does not show (`queued`, or one this build does not know, which keeps the
/// previous stage rather than naming new work).
fn stage_of(phase: &str) -> Option<&'static str> {
    Some(match phase {
        "admission" => "admission_open",
        "generating" => "generating",
        "sealing" => "sealing",
        "scoring" => "scoring",
        "packaging" => "packaging",
        "transition" => "transition",
        "closed" => "closed",
        "abandoned" => "ended",
        _ => return None,
    })
}

/// Phases the engine enters in its boundary step (an opening, a close):
/// seen at the boundary logged, dated by that step's own record. A
/// reward-v1 cohort's results are published in the boundary step too, but
/// the engine logs `transition` earlier, when the package commits (a work
/// commit inside a tempo), and records only the publication's epoch: that
/// stage is dated by `published_epoch` ([`visible_phases`]), never by the
/// log entry. Every other change commits with work and is seen at the next
/// boundary.
fn at_boundary(phase: &str) -> bool {
    matches!(phase, "admission" | "closed")
}

/// The stage changes of cohort `c` as seen at boundary `x`, in order, each
/// dated by the boundary it was seen at, and monotone in the bar's order: a
/// stage the engine returns to (a send-back from sealing to building) or a
/// phase it logs again is not shown again. `window` is the calendar's
/// window state when it describes this cohort; the closing step starts at
/// the boundary the cohort's window closed at (the cohort's own E1 field,
/// else the calendar's), or at `x` from the live flag while the engine
/// still admits.
pub fn visible_phases(
    c: &CohortView,
    x: u64,
    window: Option<&CalendarCurrent>,
    seen_at: &BTreeMap<u64, u64>,
) -> Vec<Phase> {
    let mut out: Vec<Phase> = Vec::new();
    let mut last_epoch = 0;
    let mut last_bar: Option<usize> = None;
    for m in &c.dash.log {
        let (Some(stage), Some(e)) = (stage_of(&m.phase), m.epoch) else {
            continue;
        };
        if out.last().is_some_and(|p| p.stage == "ended") {
            break;
        }
        // The boundary the change is seen at, and whether the log entry's
        // own time is that boundary step's.
        let (seen, step_time) = if m.phase == "transition" && c.dash.reward_v1 {
            // Published at the first boundary after the package commit;
            // until then the cohort still shows packaging.
            match c.dash.published_epoch {
                Some(p) => (p, false),
                None => continue,
            }
        } else if at_boundary(&m.phase) {
            (e, true)
        } else {
            (e.saturating_add(1), false)
        };
        let seen = seen.max(last_epoch);
        if seen > x {
            break;
        }
        let bar = bar_index(stage);
        if out.iter().any(|p| p.stage == stage) || bar.zip(last_bar).is_some_and(|(b, l)| b <= l) {
            continue;
        }
        // Without the engine's boundary journal: the boundary step's own
        // record, or the minute the publisher first saw the boundary.
        let fallback = if step_time && seen == e {
            m.at_ms.map(minute)
        } else {
            seen_at.get(&seen).copied()
        };
        last_epoch = seen;
        if bar.is_some() {
            last_bar = bar;
        }
        out.push(dated(c, stage, seen, fallback));
    }
    // The closing step.
    let w = window.filter(|w| w.cohort == c.cohort);
    if let Some(i) = out.iter().position(|p| p.stage == "admission_open") {
        let closing_epoch = c
            .dash
            .window_closed_epoch
            .or(w.and_then(|w| w.window_closed_epoch))
            .filter(|e| *e <= x);
        let live = w.is_some_and(|w| {
            out.len() == i + 1
                && c.phase == "admission"
                && (w.window_closing == Some(true) || w.window_open == Some(false))
        });
        if let Some(e) = closing_epoch.or(if live { Some(x) } else { None }) {
            let e = e.max(out[i].epoch.unwrap_or(0));
            let fits = out.get(i + 1).is_none_or(|n| n.epoch.is_some_and(|ne| ne >= e));
            if fits {
                out.insert(i + 1, dated(c, "admission_closing", e, seen_at.get(&e).copied()));
            }
        }
    }
    // Stages that start at one boundary carry that boundary's one point.
    for j in 1..out.len() {
        if out[j].epoch == out[j - 1].epoch {
            let (block, at, basis) = (out[j - 1].block, out[j - 1].at, out[j - 1].basis.clone());
            let p = &mut out[j];
            (p.block, p.at, p.basis) = (block, at, basis);
        }
    }
    out
}

/// Stage `stage` starting at boundary `epoch`, dated by the engine's
/// resolved boundary (its block, its time to the minute and how that time
/// is known). `fallback` is the time for a coordinator that reports no
/// boundary journal (the boundary step's own record or the minute the
/// publisher first saw the boundary). A work commit's own time is never
/// read.
fn dated(c: &CohortView, stage: &str, epoch: u64, fallback: Option<u64>) -> Phase {
    let b = c.dash.boundaries.get(&epoch);
    let (at, basis) = if c.dash.boundaries.is_empty() {
        (fallback, fallback.map(|_| "observed".to_owned()))
    } else {
        // The engine's journal is the only source: a boundary it has no time
        // for is unresolved when the journal has rows around it, otherwise
        // not recorded (before tracking began).
        match b.and_then(|b| Some((b.at_ms.map(minute)?, b.at_basis.clone()?))) {
            Some((at, basis)) => (Some(at), Some(basis)),
            None if b.is_some_and(|b| b.at_reason.as_deref() == Some("not_derivable")) => {
                (None, Some("unresolved".to_owned()))
            }
            None => (None, None),
        }
    };
    Phase {
        stage: stage.into(),
        epoch: Some(epoch),
        block: b.and_then(|b| b.block),
        at,
        basis,
        tempos: None,
    }
}

/// The credited-tempo count after boundary `e`: the cohort's own record at
/// its opening and publication, else the boundary journal's.
fn active_at(c: &CohortView, e: u64) -> Option<u64> {
    match () {
        _ if c.opened_epoch == Some(e) && c.opened_active.is_some() => c.opened_active,
        _ if c.dash.published_epoch == Some(e) && c.dash.published_active.is_some() => c.dash.published_active,
        _ => c.dash.boundaries.get(&e).and_then(|b| b.active),
    }
}

/// The tempos the results stage needs before the close: the engine closes
/// once `transition_tempos` are credited after publication and `k_tempos`
/// after the opening, so `max(transition, opened + k - published)`, at most
/// `k`.
fn effective_required(c: &CohortView) -> Option<u64> {
    let (k, _, t) = template(c)?;
    let by_k = c
        .opened_active
        .zip(c.dash.published_active)
        .and_then(|(o, p)| o.checked_add(k)?.checked_sub(p));
    Some(by_k.map_or(t, |v| v.max(t)).min(k))
}

/// Each phase's tempos: epochs to the next phase's start (to `current`'s
/// boundary for the last phase of the current cohort), credited tempos in
/// that span, and the requirement of a schedule stage. An instant (a close,
/// an end) carries none.
fn with_tempos(c: &CohortView, phases: &mut [Phase], current: Option<(u64, Option<u64>)>) {
    let a = template(c).map(|(_, a, _)| a);
    let n = phases.len();
    for i in 0..n {
        phases[i].tempos = None;
        let stage = phases[i].stage.clone();
        let Some(start) = phases[i].epoch else { continue };
        if matches!(stage.as_str(), "closed" | "ended") {
            continue;
        }
        let required = match stage.as_str() {
            "admission_open" => a,
            "transition" => effective_required(c),
            _ => None,
        };
        let (stop, counted) = if let Some(next) = phases.get(i + 1) {
            let by_records = next
                .epoch
                .and_then(|s| active_at(c, s)?.checked_sub(active_at(c, start)?));
            let counted = match stage.as_str() {
                // The window closes at the first boundary that reaches the
                // minimum, and each boundary credits at most one tempo; an
                // admission that ended otherwise (abandoned) counts by the
                // records.
                "admission_open" if matches!(next.stage.as_str(), "admission_closing" | "generating") => a,
                _ => by_records,
            };
            (next.epoch, counted)
        } else if let Some((x, now_active)) = current {
            let counted = now_active.and_then(|v| v.checked_sub(active_at(c, start)?));
            let counted = match stage.as_str() {
                "admission_open" => counted.zip(a).map(|(v, a)| v.min(a)),
                "transition" => counted.zip(required).map(|(v, r)| v.min(r)),
                _ => counted,
            };
            (Some(x), counted)
        } else {
            (None, None)
        };
        let Some(elapsed) = stop.and_then(|s| s.checked_sub(start)) else { continue };
        phases[i].tempos = Some(PhaseTempos {
            elapsed: Some(elapsed),
            counted: counted.filter(|v| *v <= elapsed),
            required,
        });
    }
}

fn mark_of(p: &Phase) -> Mark {
    Mark {
        epoch: p.epoch,
        block: p.block,
        at: p.at,
        basis: p.basis.clone(),
    }
}

/// The opening, cutoff and close of a cohort from its phases; a running
/// cohort's close is `earliest_close` (epoch and block, `basis: earliest`).
fn cohort_bounds(phases: &[Phase], earliest_close: Option<(u64, Option<u64>)>) -> Option<CohortBounds> {
    if phases.is_empty() {
        return None;
    }
    let find = |st: &str| phases.iter().find(|p| p.stage == st).map(mark_of);
    let ended = phases.last().is_some_and(|p| p.stage == "ended");
    let close = find("closed").or_else(|| {
        earliest_close.filter(|_| !ended).map(|(e, b)| Mark {
            epoch: Some(e),
            block: b,
            at: None,
            basis: Some("earliest".into()),
        })
    });
    Some(CohortBounds {
        open: find("admission_open"),
        cutoff: find("admission_closing"),
        close,
    })
}

/// The cohort shown as current at boundary `x`: the newest cohort of the
/// record whose opening is visible and that has not closed, or the newest
/// ended one when nothing newer opened. `None`: no cohort is running.
fn current<'a>(e: &'a EvalView, s: &Settings<'_>, x: u64, seen: &BTreeMap<u64, u64>) -> Option<(&'a CohortView, Vec<Phase>)> {
    let window = e.calendar.as_ref().and_then(|k| k.current.as_ref());
    let newest = e
        .cohorts
        .iter()
        .filter(|c| in_record(c, s))
        .map(|c| (c, visible_phases(c, x, window, seen)))
        .filter(|(_, p)| !p.is_empty())
        .max_by_key(|(c, _)| c.cohort)?;
    match newest.1.last().map(|p| p.stage.as_str()) {
        Some("closed") => None,
        _ => Some(newest),
    }
}

/// The work percent of `stage` from the cohort's aggregate counts, on the
/// grid, never 100: a stage the engine has already left reads 90.
fn work_pct(c: &CohortView, stage: &str) -> Option<u64> {
    let counts = c.dash.work?;
    let (done, total) = match stage {
        "generating" => counts.generation?,
        "scoring" => counts.scoring?,
        _ => return None,
    };
    if c.phase != stage {
        return Some(90);
    }
    if total == 0 {
        return Some(0);
    }
    let pct = done.min(total).saturating_mul(100) / total;
    Some((pct - pct % 10).min(90))
}

/// The pause class a cohort fault gives: one that clears by itself waits on
/// infrastructure; any other waits for the operator.
fn hold_of(c: &CohortView) -> Option<String> {
    c.fault.as_ref()?;
    Some(match (c.dash.self_clearing, c.operator_clear_needed) {
        (Some(true), _) | (None, Some(false)) => "waiting_on_infrastructure",
        _ => "operator_hold",
    }
    .into())
}

/// Advances the latched values to the observed boundary. Values read only
/// at a boundary (the work percent and the fault class) are read only by
/// the first observation of a boundary that follows the saved one in
/// sequence (the saved memo is at `x - 1`). The run saves the memo before
/// it writes a document carrying them ([`publish`]), so a later run in the
/// same tempo keeps them rather than reading again. Any other change (a
/// fresh or lost memo, a jump past several boundaries, or the cohort or its
/// stage changing within a tempo) leaves them out until the next boundary.
/// The first observation of a boundary in sequence also records its minute
/// (a jump leaves the boundaries it passed undated).
pub fn roll(memo: &mut Memo, s: &Settings<'_>, e: &EvalView, now_ms: u64) {
    let Some(x) = e.epoch else { return };
    if memo.epoch.is_some_and(|m| m > x) {
        return;
    }
    let in_sequence = memo.epoch.is_some_and(|m| m.saturating_add(1) == x);
    if in_sequence {
        memo.seen.entry(x).or_insert(minute(now_ms));
    }
    let cur = current(e, s, x, &memo.seen);
    let (cohort, stage) = match &cur {
        Some((c, p)) => (Some(c.cohort), p.last().map(|p| p.stage.clone())),
        None => (None, None),
    };
    if in_sequence {
        memo.pct = cur
            .as_ref()
            .zip(stage.as_deref())
            .and_then(|((c, _), st)| work_pct(c, st));
        memo.hold = cur.as_ref().and_then(|(c, _)| hold_of(c));
    } else if memo.epoch != Some(x) || memo.cohort != cohort || memo.stage != stage {
        memo.pct = None;
        memo.hold = None;
    }
    memo.epoch = Some(x);
    memo.cohort = cohort;
    memo.stage = stage;
    // Keep the boundaries of the current cohort and the one before it.
    let mut opened: Vec<u64> = e
        .cohorts
        .iter()
        .filter(|c| in_record(c, s))
        .filter_map(|c| c.opened_epoch)
        .collect();
    opened.sort_unstable();
    let keep_from = opened.iter().rev().nth(1).or(opened.last()).copied().unwrap_or(x);
    memo.seen.retain(|epoch, _| *epoch >= keep_from || *epoch == x);
}

/// The cohort template, when complete and consistent.
fn template(c: &CohortView) -> Option<(u64, u64, u64)> {
    let (k, a, t) = (c.k_tempos?, c.admission_tempos?, c.transition_tempos?);
    (a.checked_add(t)? <= k).then_some((k, a, t))
}

/// The bar stages of `c`'s template, with each later stage's earliest
/// boundary when `earliest` is given.
fn plan_of(c: &CohortView, earliest: Option<&[Option<u64>; 8]>, block: &dyn Fn(u64) -> Option<u64>) -> Option<Plan> {
    let (k, a, t) = template(c)?;
    Some(Plan {
        template: Template {
            k_tempos: Some(k),
            admission_tempos: Some(a),
            transition_tempos: Some(t),
            work_window_tempos: Some(k - a - t),
        },
        stages: BAR_STAGES
            .iter()
            .enumerate()
            .map(|(i, (stage, kind))| {
                let e = earliest.and_then(|l| l[i]);
                StagePlan {
                    stage: (*stage).into(),
                    kind: (*kind).into(),
                    expected_tempos: match *stage {
                        "admission_open" => Some(a),
                        "transition" => Some(t),
                        "closed" => Some(0),
                        _ => None,
                    },
                    earliest_epoch: e,
                    earliest_block: e.and_then(block),
                }
            })
            .collect(),
    })
}

/// The lower bounds of the current cohort (PLAN section 4): each later
/// stage's earliest start boundary, the current stage's end and the
/// earliest close, all credit-adjusted, never behind the next boundary.
struct Bounds {
    earliest: [Option<u64>; 8],
    stage_end: Option<u64>,
    close: Option<u64>,
    tempos: Option<Tempos>,
}

fn bounds(c: &CohortView, phases: &[Phase], stage: &str, e: &EvalView, x: u64, cal: Option<&CalendarCurrent>) -> Bounds {
    let mut out = Bounds {
        earliest: [None; 8],
        stage_end: None,
        close: None,
        tempos: None,
    };
    let Some((k, a, t)) = template(c) else { return out };
    let next = x + 1;
    let cal = cal.filter(|w| w.cohort == c.cohort);
    let cur = match stage {
        "queued" => -1,
        s => match bar_index(s) {
            Some(i) if i < 7 => i as i64,
            _ => return out,
        },
    };
    let phase_epoch = |st: &str| phases.iter().find(|p| p.stage == st).and_then(|p| p.epoch);
    let credited = |mark: Option<u64>| e.active_tempos.zip(mark).map(|(n, m)| n.saturating_sub(m));
    let opened = c.opened_epoch.or(phase_epoch("admission_open"));
    let credited_open = credited(c.opened_active);
    // The opening boundary of a queued cohort: the calendar's next opening.
    let open = match cur {
        -1 => e
            .calendar
            .as_ref()
            .and_then(|k| k.next.as_ref())
            .filter(|n| n.cohort == c.cohort)
            .and_then(|n| n.earliest_open_epoch)
            .unwrap_or(next)
            .max(next),
        _ => opened.unwrap_or(x),
    };
    out.stage_end = match stage {
        "queued" => Some(open),
        "admission_open" => Some(
            cal.and_then(|w| w.earliest_cutoff_epoch)
                .or(credited_open.map(|cr| x + a.saturating_sub(cr).max(1)))
                .unwrap_or((open + a).max(next))
                .max(next),
        ),
        "admission_closing" => Some(next),
        _ => None,
    };
    out.tempos = match stage {
        "admission_open" => Some(Tempos {
            credited: credited_open.map(|v| v.min(a)),
            required: Some(a),
        }),
        // The results stage needs its own minimum and the cohort's K
        // credited tempos since the opening ([`effective_required`]).
        "transition" => {
            let r = effective_required(c).unwrap_or(t);
            Some(Tempos {
                credited: credited(c.dash.published_active).map(|v| v.min(r)),
                required: Some(r),
            })
        }
        _ => None,
    };
    let mut prev: Option<u64> = None;
    let mut transition_start = phase_epoch("transition");
    for (j, (st, _)) in BAR_STAGES.iter().enumerate().take(7) {
        if j as i64 <= cur {
            continue;
        }
        let v = if j as i64 == cur + 1 {
            out.stage_end.unwrap_or(next)
        } else {
            // Results are published at the first boundary after the package
            // commits, which can fall in the tempo packaging starts in: the
            // earliest publication is packaging's earliest boundary (seen
            // one boundary after its commit), not one later.
            let p = prev.unwrap_or(next);
            match *st {
                "admission_closing" => p.max(open + a),
                _ => p,
            }
        }
        .max(next);
        out.earliest[j] = Some(v);
        if *st == "transition" {
            transition_start = Some(v);
        }
        prev = Some(v);
    }
    // The close: K credited tempos after the opening, the results stage its
    // full length, and the calendar's own credited bound.
    let by_k = match (cur, credited_open) {
        (-1, _) => open + k,
        (_, Some(cr)) => (x + k.saturating_sub(cr)).max(open + k),
        (_, None) => open + k,
    };
    let by_t = if stage == "transition" {
        match credited(c.dash.published_active) {
            Some(cr) => x + t.saturating_sub(cr),
            None => transition_start.unwrap_or(x) + t,
        }
    } else {
        transition_start.unwrap_or(next) + t
    };
    let close = by_k
        .max(by_t)
        .max(next)
        .max(cal.and_then(|w| w.earliest_close_epoch_credited).unwrap_or(0));
    out.earliest[7] = Some(close);
    if stage == "transition" {
        out.stage_end = Some(close);
    }
    if cur >= 0 {
        out.close = Some(close);
    }
    out
}

/// What the run looked up for the best model: the cohort that promoted the
/// current reference, its summary and its artifacts.
pub struct BestFacts<'a> {
    pub index: u64,
    pub since: Option<&'a CohortEntry>,
    pub summary: Option<&'a CohortSummary>,
    pub artifacts: &'a [ArtifactEntry],
}

/// The cohort whose promotion made the current reference `R_k`: the newest
/// listed cohort that promoted from `R_k-1`.
pub fn promoted_by(idx: &CohortIndex, k: u64) -> Option<&CohortEntry> {
    let k1 = k.checked_sub(1)?;
    idx.cohorts
        .iter()
        .rev()
        .find(|c| c.promoted.is_some() && c.reference_index == k1)
}

fn artifact_of(a: &ArtifactEntry) -> Artifact {
    let state = match a.state {
        ArtifactState::Published => "published",
        ArtifactState::Withheld => "withheld",
        ArtifactState::Pending => "pending",
    };
    let download = match a.state {
        ArtifactState::Published => a
            .path
            .clone()
            .filter(|p| crate::allowlist::classify(p) == Some(crate::allowlist::Kind::Artifact)),
        _ => None,
    };
    Artifact {
        state: state.into(),
        download,
    }
}

fn owner_in(summary: Option<&CohortSummary>, hotkey: &str) -> Owner {
    Owner {
        uid: summary.and_then(|s| s.entrants.iter().find(|e| e.hotkey == hotkey).map(|e| e.uid)),
        hotkey: hotkey.to_owned(),
    }
}

fn best(b: &BestFacts<'_>) -> Best {
    let promoted = b.since.and_then(|c| c.promoted.as_deref());
    Best {
        index: b.index,
        label: format!("R{}", b.index),
        since_cohort: b.since.map(|c| c.cohort),
        since_label: b.since.map(|c| c.label.clone()),
        owner: promoted.map(|h| owner_in(b.summary, h)),
        latest_score: None,
        latest_score_label: None,
        quantization: None,
        artifact: promoted.and_then(|h| b.artifacts.iter().find(|a| a.hotkey == h).map(artifact_of)),
        licence: None,
    }
}

/// `m/a/now.json`.
pub fn now_doc(
    s: &Settings<'_>,
    o: &Observed,
    idx: Option<&CohortIndex>,
    best_facts: Option<&BestFacts<'_>>,
    memo: &Memo,
    now_ms: u64,
) -> NowDoc {
    let e = o.eval.as_ref();
    let x = e.and_then(|e| e.epoch);
    let boundary = e.and_then(|e| e.boundary_block).zip(x);
    let tempo = s.tempo_blocks;
    let block = |epoch: u64| -> Option<u64> {
        let (bb, be) = boundary?;
        let d = epoch.checked_sub(be)?;
        bb.checked_add(d.checked_mul(tempo)?)
    };
    let sources_ok = o.coordinator.is_some() && o.eval_worker.is_some() && e.is_some();
    let age_ms = o.eval_worker.as_ref().and_then(|w| w.confirmed_age_ms);
    let clock = Clock {
        confirmed_block: e.and_then(|e| e.confirmed_block),
        confirmed_epoch: x,
        age_secs: age_ms.map(|ms| ms / 1000),
        boundary_block: boundary.map(|(b, _)| b),
        boundary_epoch: boundary.map(|(_, x)| x),
        tempo_blocks: Some(tempo),
        block_secs_nominal: Some(s.block_secs),
    };
    let mut notice = vec!["testnet".to_owned()];
    let launch_burn = e.and_then(|e| e.reward.as_ref()).and_then(|r| r.launch_burn);
    let mode = launch_burn.map(|b| if b { "burn" } else { "target" }.to_owned());
    if let Some(m) = &mode {
        notice.push(m.clone());
    }
    if s.operator_notice {
        notice.push("operator".into());
    }
    let mut cohort = CohortNow {
        stage: "unknown".into(),
        mode: mode.clone(),
        ..Default::default()
    };
    let mut pause = Pause::default();
    let mut next = None;
    if let (Some(e), Some(x)) = (e, x) {
        let cal = e.calendar.as_ref().and_then(|k| k.current.as_ref());
        let cur = current(e, s, x, &memo.seen);
        let queued = e
            .cohorts
            .iter()
            .filter(|c| in_record(c, s) && c.phase == "queued")
            .min_by_key(|c| c.cohort);
        let (view, phases, stage): (Option<&CohortView>, Vec<Phase>, String) = match cur {
            Some((c, p)) => {
                let st = p.last().map(|p| p.stage.clone()).unwrap_or_else(|| "unknown".into());
                (Some(c), p, st)
            }
            None => match queued {
                Some(q) => (Some(q), Vec::new(), "queued".into()),
                None => (None, Vec::new(), "idle".into()),
            },
        };
        cohort.stage = stage.clone();
        if let Some(c) = view {
            cohort.cohort = Some(c.cohort);
            cohort.label = Some(c.label.clone());
            let b = bounds(c, &phases, &stage, e, x, cal);
            cohort.expected = Expected {
                stage_end_epoch: b.stage_end,
                stage_end_block: b.stage_end.and_then(block),
                close_epoch: b.close,
                close_block: b.close.and_then(block),
                packaging_deadline_epoch: match stage.as_str() {
                    "generating" | "sealing" | "scoring" | "packaging" => template(c)
                        .zip(c.opened_epoch)
                        .and_then(|((k, _, t), o)| (o + k).checked_sub(t + 1)),
                    _ => None,
                },
            };
            cohort.tempos = b.tempos;
            let running = stage != "ended";
            cohort.plan = plan_of(c, running.then_some(&b.earliest), &block);
            if matches!(stage.as_str(), "admission_open" | "admission_closing") {
                let admitted = e.admitted.iter().find(|(n, _)| *n == c.cohort).map(|(_, a)| *a);
                let cap = c.dash.roster_cap;
                if (admitted.is_some() || cap.is_some())
                    && !matches!((admitted, cap), (Some(n), Some(m)) if n > m)
                {
                    cohort.progress.admission = Some(Admission {
                        admitted,
                        roster_cap: cap,
                    });
                }
            }
            if stage_kind(&stage) == Some("work")
                && memo.epoch == Some(x)
                && memo.cohort == Some(c.cohort)
                && memo.stage.as_deref() == Some(stage.as_str())
                && let Some(pct) = memo.pct.filter(|p| WORK_GRID.contains(p))
            {
                cohort.progress.work = Some(WorkProgress {
                    stage: stage.clone(),
                    pct,
                    as_of_epoch: Some(x),
                });
            }
            let mut phases = phases;
            with_tempos(c, &mut phases, Some((x, e.active_tempos)));
            cohort.bounds = cohort_bounds(&phases, cohort.expected.close_epoch.map(|e| (e, block(e))));
            cohort.phases = phases;
            let close = cohort.expected.close_epoch;
            // A later cohort opens at the boundary that closes this one, at
            // the earliest.
            next = e.calendar.as_ref().and_then(|k| k.next.as_ref()).map(|n| {
                let open = n.earliest_open_epoch.map(|v| match close {
                    Some(cl) if n.cohort > c.cohort => v.max(cl),
                    _ => v,
                });
                Next {
                    cohort: Some(n.cohort),
                    earliest_open_epoch: open,
                    earliest_open_block: open.and_then(block),
                    admission_tempos: n.admission_tempos,
                }
            });
        } else {
            next = e.calendar.as_ref().and_then(|k| k.next.as_ref()).map(|n| Next {
                cohort: Some(n.cohort),
                earliest_open_epoch: n.earliest_open_epoch,
                earliest_open_block: n.earliest_open_epoch.and_then(block),
                admission_tempos: n.admission_tempos,
            });
        }
        // Pause: a silent coordinator first (tempos are not credited), then
        // a cohort fault as read at the boundary, then an observation that
        // is catching up, then a paused calendar with nothing running.
        let silent = age_ms.is_some_and(|a| a > s.silent_ms);
        let hold = (view.map(|c| c.cohort) == memo.cohort && memo.epoch == Some(x))
            .then(|| memo.hold.clone())
            .flatten()
            .filter(|_| !matches!(stage.as_str(), "queued" | "idle" | "closed"));
        let class = if silent {
            Some("coordinator_silent".to_owned())
        } else if hold.is_some() {
            hold
        } else if e.epoch_live == Some(false) {
            Some("observation_lag".to_owned())
        } else if matches!(stage.as_str(), "queued" | "idle")
            && e.calendar.as_ref().and_then(|k| k.template_paused) == Some(true)
        {
            Some("calendar_paused".to_owned())
        } else {
            None
        };
        pause = Pause {
            paused: class.is_some(),
            class,
        };
    }
    let latest_cohort = idx.and_then(|i| i.cohorts.last()).map(|c| c.label.clone());
    let mut d = NowDoc {
        schema: Doc::Now("a").schema().into(),
        network: "testnet".into(),
        chain: chain(s.chain),
        generated_at: Some(minute(now_ms)),
        sample: false,
        mechanism: "a".into(),
        clock,
        pause,
        cohort,
        next,
        best: best_facts.map(best),
        latest_cohort,
        notice,
        advisory: None,
        sources_ok: Some(sources_ok),
    };
    clean_now(&mut d);
    d
}

/// `index.json`: A live on chain mechanism `chain.mechanism`, B and C
/// planned.
pub fn index_doc(s: &Settings<'_>, now_ms: u64) -> IndexDoc {
    let planned = |key: &str, name: &str| MechanismEntry {
        key: key.into(),
        name: name.into(),
        state: "planned".into(),
        chain_mechanism: None,
        stage_set: None,
    };
    IndexDoc {
        schema: Doc::Index.schema().into(),
        network: "testnet".into(),
        chain: chain(s.chain),
        generated_at: Some(minute(now_ms)),
        sample: false,
        mechanisms: vec![
            MechanismEntry {
                key: "a".into(),
                name: "Detection".into(),
                state: "live".into(),
                chain_mechanism: Some(u16::from(s.chain.mechanism)),
                stage_set: Some("cohort/v1".into()),
            },
            planned("b", "Rewriting"),
            planned("c", "Shared grammar"),
        ],
    }
}

/// One row of the cohort list, and what its document is built from.
pub struct Listed<'a> {
    pub row: CohortRow,
    pub entry: Option<&'a CohortEntry>,
    pub view: Option<&'a CohortView>,
}

fn mark(phases: &[Phase], stage: &str) -> Option<Mark> {
    phases.iter().find(|p| p.stage == stage).map(mark_of)
}

/// The mark at `epoch` (an index entry's own epoch), dated by the phase
/// `stage` when it starts at that boundary, else by any phase starting
/// there (equal epochs carry equal points), else undated.
fn mark_at(phases: &[Phase], stage: &str, epoch: u64) -> Mark {
    mark(phases, stage)
        .filter(|m| m.epoch == Some(epoch))
        .or_else(|| phases.iter().find(|p| p.epoch == Some(epoch)).map(mark_of))
        .unwrap_or(Mark {
            epoch: Some(epoch),
            ..Default::default()
        })
}

/// The phases of a listed cohort: from its phase log when the status still
/// carries it, else its opening and close epochs from the index, undated.
/// Only boundary-step changes carry a time here (the engine's own record of
/// that step, which the status keeps for every cohort); the minute the
/// publisher first saw a later boundary is kept only for the current cohort
/// ([`Memo::seen`]), so a document rebuilt later never loses a time it once
/// carried.
fn phases_of(view: Option<&CohortView>, entry: Option<&CohortEntry>, x: u64) -> Vec<Phase> {
    match view {
        Some(v) if !v.dash.log.is_empty() => {
            let mut p = visible_phases(v, x, None, &BTreeMap::new());
            with_tempos(v, &mut p, None);
            p
        }
        _ => entry
            .map(|c| {
                let undated = |stage: &str, epoch| Phase {
                    stage: stage.into(),
                    epoch: Some(epoch),
                    ..Default::default()
                };
                let mut p = vec![undated("admission_open", c.opened_epoch), undated("closed", c.closed_epoch)];
                if let Some(v) = view {
                    p = p.into_iter().map(|q| dated(v, &q.stage, q.epoch.unwrap_or(0), None)).collect();
                    with_tempos(v, &mut p, None);
                }
                p
            })
            .unwrap_or_default(),
    }
}

/// The listed cohorts, ascending: closed cohorts of the index, cohorts
/// retired before publication, and cohorts the engine ended (abandoned)
/// once their end is visible.
pub fn listed<'a>(s: &Settings<'_>, idx: &'a CohortIndex, e: Option<&'a EvalView>) -> Vec<Listed<'a>> {
    let x = e.and_then(|e| e.epoch).unwrap_or(u64::MAX);
    let view_of = |label: &str| e.and_then(|e| e.cohorts.iter().find(|c| c.label == label));
    let mut out: Vec<Listed<'a>> = Vec::new();
    for c in &idx.cohorts {
        let view = view_of(&c.label);
        let phases = phases_of(view, Some(c), x);
        out.push(Listed {
            row: CohortRow {
                cohort: c.cohort,
                label: c.label.clone(),
                state: "closed".into(),
                opened: Some(mark_at(&phases, "admission_open", c.opened_epoch)),
                admission_closed: mark(&phases, "admission_closing"),
                published: mark(&phases, "transition"),
                closed: Some(mark_at(&phases, "closed", c.closed_epoch)),
                mode: None,
                promoted: c.promoted.as_ref().map(|h| Owner {
                    uid: None,
                    hotkey: h.clone(),
                }),
                entrants: Some(c.entrants),
                reference_index: Some(c.reference_index),
                best_score: None,
                quantization: None,
                leaderboard: true,
                ended_class: None,
                bounds: None,
                tempo_blocks: None,
                anchor: None,
            },
            entry: Some(c),
            view,
        });
    }
    for r in &idx.retired_unpublished {
        if out.iter().any(|l| l.row.label == r.label) || !contract::safe_label(&r.label) {
            continue;
        }
        out.push(Listed {
            row: CohortRow {
                cohort: r.cohort,
                label: r.label.clone(),
                state: "retired_unpublished".into(),
                opened: Some(Mark {
                    epoch: Some(r.opened_epoch),
                    ..Default::default()
                }),
                ..Default::default()
            },
            entry: None,
            view: None,
        });
    }
    if let Some(e) = e {
        for c in e.cohorts.iter().filter(|c| in_record(c, s) && c.phase == "abandoned") {
            if out.iter().any(|l| l.row.label == c.label) {
                continue;
            }
            let phases = visible_phases(c, x, None, &BTreeMap::new());
            if phases.last().map(|p| p.stage.as_str()) != Some("ended") {
                continue;
            }
            out.push(Listed {
                row: CohortRow {
                    cohort: c.cohort,
                    label: c.label.clone(),
                    state: "ended".into(),
                    opened: mark(&phases, "admission_open"),
                    admission_closed: mark(&phases, "admission_closing"),
                    published: mark(&phases, "transition"),
                    closed: None,
                    ..Default::default()
                },
                entry: None,
                view: Some(c),
            });
        }
    }
    // Every row's reference: the clock's boundary projected to its opening.
    let clock = e.and_then(|e| e.epoch.zip(e.boundary_block));
    for l in &mut out {
        let epoch = l.row.opened.as_ref().and_then(|m| m.epoch);
        l.row.anchor = anchor(clock, s.tempo_blocks, epoch);
        l.row.tempo_blocks = l.row.anchor.as_ref().map(|_| s.tempo_blocks);
    }
    out.sort_by_key(|l| l.row.cohort);
    out
}

/// The clock's boundary `(epoch, block)` projected back to `epoch` at
/// `tempo`: `None` without either, or for an epoch after the clock's.
fn anchor(clock: Option<(u64, u64)>, tempo: u64, epoch: Option<u64>) -> Option<Anchor> {
    let ((x, bb), e) = (clock?, epoch?);
    let block = bb.checked_sub(x.checked_sub(e)?.checked_mul(tempo)?)?;
    Some(Anchor {
        epoch: Some(e),
        block: Some(block),
    })
}

/// `m/a/cohorts/index.json`.
pub fn cohorts_doc(s: &Settings<'_>, idx: &CohortIndex, rows: &[Listed<'_>], now_ms: u64) -> CohortsDoc {
    let mut d = CohortsDoc {
        schema: Doc::Cohorts("a").schema().into(),
        network: "testnet".into(),
        chain: chain(s.chain),
        generated_at: Some(minute(now_ms)),
        sample: false,
        mechanism: "a".into(),
        cohorts: rows.iter().map(|l| l.row.clone()).collect(),
        records: idx
            .previous_records
            .iter()
            .filter(|r| contract::safe_label(&r.first_label))
            .map(|r| RecordRef {
                first_label: r.first_label.clone(),
            })
            .collect(),
        record_prefix: idx.record_prefix.clone(),
    };
    clean_cohorts(&mut d);
    d
}

/// `m/a/cohorts/<label>.json` for row `i` of `rows`: a closed cohort with its
/// summary carries the outcome, its entrants (roster order, no rank) with
/// band, eligibility and gate bits, and the occupied bands; an ended cohort
/// its phases only. No score, cutoff value or band edge until the
/// coordinator publishes quantized scores.
pub fn cohort_doc(
    s: &Settings<'_>,
    rows: &[Listed<'_>],
    i: usize,
    summary: Option<&CohortSummary>,
    artifacts: &[ArtifactEntry],
    x: u64,
    now_ms: u64,
) -> CohortDoc {
    let l = &rows[i];
    let documented = |r: &Listed<'_>| r.row.state != "retired_unpublished";
    let previous = rows[..i].iter().rev().find(|r| documented(r)).map(|r| r.row.label.clone());
    let next = rows[i + 1..].iter().find(|r| documented(r)).map(|r| r.row.label.clone());
    let mut row = l.row.clone();
    let phases = phases_of(l.view, l.entry, x);
    let results = row.state == "closed" && summary.is_some_and(|sm| sm.label == row.label);
    let summary = summary.filter(|_| results);
    if let (Some(p), Some(sm)) = (row.promoted.as_mut(), summary) {
        *p = owner_in(Some(sm), &p.hotkey);
    }
    row.leaderboard = results;
    let entrants = summary.map(|sm| {
        let mut seen_hotkeys = BTreeSet::new();
        sm.entrants
            .iter()
            .filter(|en| seen_hotkeys.insert(en.hotkey.clone()))
            .map(|en| Entrant {
                uid: en.uid,
                hotkey: en.hotkey.clone(),
                score: None,
                band: en.band,
                eligible: en.band.is_some(),
                row_share_pct: None,
                gates_passed: en.gate == "pass" && en.copy_gate == "pass",
                promoted: sm.promoted.as_deref() == Some(en.hotkey.as_str()),
                artifact: artifacts.iter().find(|a| a.hotkey == en.hotkey).map(artifact_of),
            })
            .collect::<Vec<_>>()
    });
    // The occupied bands, 1 for the top one (reward-v1's `kappa`), without
    // edges: they are offsets from the reference score.
    let quality = entrants.as_ref().map(|list| {
        let top = list.iter().filter_map(|e| e.band).max().unwrap_or(0);
        Quality {
            cutoff: None,
            bands: Some(
                (1..=top)
                    .map(|band| Band {
                        band,
                        lower: None,
                        upper: None,
                    })
                    .collect(),
            ),
        }
    });
    let reference = summary.map(|sm| ReferenceRow {
        index: sm.references.index,
        label: format!("R{}", sm.references.index),
        score: None,
    });
    let predecessor = summary.and_then(|sm| {
        sm.references.predecessor.as_ref()?;
        let k = sm.references.index.checked_sub(1)?;
        Some(ReferenceRow {
            index: k,
            label: format!("R{k}"),
            score: None,
        })
    });
    let testnet = crate::leaderboard::PromotionRule::of(&slop_ninja_admission_wire::json::Chain::from_binding(s.chain)).is_some();
    row.bounds = cohort_bounds(&phases, None);
    let mut d = CohortDoc {
        schema: Doc::Cohort("a", String::new()).schema().into(),
        network: "testnet".into(),
        chain: chain(s.chain),
        generated_at: Some(minute(now_ms)),
        sample: false,
        mechanism: "a".into(),
        cohort: row,
        phases,
        plan: l.view.and_then(|v| plan_of(v, None, &|_| None)),
        quality,
        entrants,
        reference,
        predecessor,
        score_better: None,
        promotion_rule: (testnet && results).then(|| TESTNET_PROMOTION_RULE.into()),
        previous,
        next,
    };
    clean_cohort(&mut d);
    d
}

/// The inputs of every cohort document except the summaries and artifact
/// states, which change only with the index entry: a digest that changes
/// whenever any cohort document would.
pub fn cohorts_key(rows: &[Listed<'_>], x: u64) -> String {
    let parts: Vec<Value> = rows
        .iter()
        .map(|l| {
            serde_json::json!({
                "row": l.row,
                "entry": l.entry,
                "phases": phases_of(l.view, l.entry, x),
                "template": l.view.and_then(template),
            })
        })
        .collect();
    let bytes = serde_json::to_vec(&serde_json::json!({"rules": RULES, "rows": parts})).unwrap_or_default();
    slop_ninja_protocol::object_sha256(&bytes).hex()
}

// -------------------------------------------------------------------- run

/// What one dashboard pass reads.
pub struct RunInputs<'a> {
    pub settings: Settings<'a>,
    pub observed: &'a Observed,
    /// The cohort index this run left (or read); `None` when it failed.
    pub index: Option<&'a CohortIndex>,
    pub now_ms: u64,
    /// The run's memory budget ([`crate::arena`]): every summary and
    /// publication record read, with what is decoded and built from it, and
    /// the cohort list are reserved from it before they are allocated.
    pub budget: &'a crate::arena::Budget,
}

/// What one pass did; refusals raise the publisher's alert.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub written: Vec<String>,
    pub refused: Vec<(String, String)>,
    /// The cohort documents stopped at the wall budget (continued next run).
    pub incomplete: bool,
}

async fn write<S: Store>(out: &DashboardStore<'_, S>, doc: &Doc, value: &impl Serialize, pass: &mut Pass) -> bool {
    let key = doc.key();
    let bytes = match serde_json::to_vec(value) {
        Ok(b) => b,
        Err(_) => {
            pass.refused.push((key, "not serializable".into()));
            return false;
        }
    };
    if let Err(why) = check(&key, &bytes) {
        pass.refused.push((format!("dashboard {key}"), why));
        return false;
    }
    match out.put(&key, bytes).await {
        Ok(()) => {
            pass.written.push(key);
            true
        }
        Err(e) => {
            pass.refused.push((format!("dashboard {key}"), e.to_string()));
            false
        }
    }
}

/// What one cohort's document is built from: its summary and its artifact
/// states, read with their memory reserved.
struct Facts<'b> {
    summary: Option<CohortSummary>,
    artifacts: Vec<ArtifactEntry>,
    _hold: Option<crate::arena::Hold<'b>>,
}

/// Holds per byte of a JSON object read for a document: the raw bytes, the
/// decoded value (owned strings, no larger than their JSON) and its share
/// of the document built from it.
const HELD_PER_READ_BYTE: u64 = 3;

/// Reads a cohort's summary and publication record (both written by this
/// publisher), each within the cohort document's bound and its stored size,
/// after reserving their memory. A missing object reads as absent; a store
/// failure or a refused reservation is an error, so the document is retried.
async fn cohort_facts<'b, S: Store>(
    public: &crate::store::PublicStore<'_, S>,
    budget: &'b crate::arena::Budget,
    entry: Option<&CohortEntry>,
) -> Result<Facts<'b>, String> {
    let Some(c) = entry else {
        return Ok(Facts {
            summary: None,
            artifacts: Vec::new(),
            _hold: None,
        });
    };
    let summary_key = crate::layout::join(&crate::layout::cohort_dir(&c.label), crate::summary::SUMMARY);
    let publication_key = crate::layout::publication(&c.label);
    let bound = contract::bounds::ROSTER as u64;
    let mut sizes = 0u64;
    for key in [&summary_key, &publication_key] {
        match public.head(key).await {
            Ok(Some(m)) if m.size <= bound => sizes += m.size,
            Ok(Some(m)) => return Err(format!("{key}: {} bytes exceed {bound}", m.size)),
            Ok(None) => {}
            Err(e) => return Err(format!("{key}: {e}")),
        }
    }
    let hold = budget
        .reserve(sizes.saturating_mul(HELD_PER_READ_BYTE))
        .map_err(|r| format!("{}: needs {} bytes with {} held of {}", c.label, r.want, r.held, r.limit))?;
    let read = |key: String| async move {
        match public.get(&key, bound).await {
            Ok(b) => Ok(b),
            Err(e) => Err(format!("{key}: {e}")),
        }
    };
    let summary = match read(summary_key).await? {
        Some(b) => serde_json::from_slice::<CohortSummary>(&b).ok(),
        None => None,
    };
    let artifacts = match read(publication_key).await? {
        Some(b) => serde_json::from_slice::<crate::index::Publication>(&b)
            .map(|p| p.artifacts)
            .unwrap_or_default(),
        None => Vec::new(),
    };
    Ok(Facts {
        summary,
        artifacts,
        _hold: Some(hold),
    })
}

/// One dashboard pass: rolls the latched values to the observed boundary
/// and saves them (`checkpoint`, before any document carries them; nothing
/// is written when the save fails), writes the registry and the current
/// cohort, and, while the coordinator's status is readable, the cohort list
/// and the cohort documents when their inputs changed (resuming a rebuild
/// the wall budget cut short). Without the status the list and the
/// documents keep their last version: a missing source never rewrites them
/// with fewer facts. `within_budget` is checked before each cohort
/// document.
pub async fn publish<S: Store>(
    out: &DashboardStore<'_, S>,
    public: &crate::store::PublicStore<'_, S>,
    i: &RunInputs<'_>,
    memo: &mut Memo,
    checkpoint: impl AsyncFnOnce(&Memo) -> bool,
    within_budget: &dyn Fn() -> bool,
) -> Pass {
    let mut pass = Pass::default();
    let s = &i.settings;
    let e = i.observed.eval.as_ref();
    let saved = memo.clone();
    if let Some(e) = e {
        roll(memo, s, e, i.now_ms);
    }
    if !checkpoint(memo).await {
        // The memo stays what the state object holds, so the run's final
        // save can still carry this refusal to the next run's alerts.
        *memo = saved;
        pass.refused.push(("dashboard".into(), "dashboard state not saved; nothing written".into()));
        return pass;
    }
    write(out, &Doc::Index, &index_doc(s, i.now_ms), &mut pass).await;
    // The best model: the cohort that promoted the current reference.
    let k = e
        .and_then(|e| e.reward.as_ref())
        .and_then(|r| r.reference_index)
        .or_else(|| i.index.and_then(|x| x.cohorts.last()).map(|c| c.reference_index + u64::from(c.promoted.is_some())));
    let since = i.index.zip(k).and_then(|(x, k)| promoted_by(x, k));
    let since_facts = match cohort_facts(public, i.budget, since).await {
        Ok(f) => Some(f),
        Err(why) => {
            pass.refused.push(("dashboard best model".into(), why));
            None
        }
    };
    let facts = k.map(|index| BestFacts {
        index,
        since,
        summary: since_facts.as_ref().and_then(|f| f.summary.as_ref()),
        artifacts: since_facts.as_ref().map_or(&[][..], |f| &f.artifacts),
    });
    let now = now_doc(s, i.observed, i.index, facts.as_ref(), memo, i.now_ms);
    write(out, &Doc::Now("a"), &now, &mut pass).await;
    let (Some(idx), Some(e)) = (i.index, e) else { return pass };
    let x = e.epoch.unwrap_or(u64::MAX);
    drop(since_facts);
    // The cohort list: at most the bound's 1 KiB a row, while it is built
    // and serialized.
    let rows = listed(s, idx, Some(e));
    let row_bytes = (rows.len() as u64).saturating_add(64).saturating_mul(1024);
    match i.budget.reserve(row_bytes.saturating_mul(2)) {
        Ok(_hold) => {
            write(out, &Doc::Cohorts("a"), &cohorts_doc(s, idx, &rows, i.now_ms), &mut pass).await;
        }
        Err(r) => pass.refused.push((
            "dashboard cohort list".into(),
            format!("needs {} bytes with {} held of {}", r.want, r.held, r.limit),
        )),
    }
    // Cohort documents: rebuilt when their inputs change, from the cursor of
    // a rebuild with the same inputs that ran out of time.
    let key = cohorts_key(&rows, x);
    if memo.cohorts_key.as_deref() == Some(key.as_str()) {
        memo.pending_key = None;
        memo.cursor = None;
        return pass;
    }
    if memo.pending_key.as_deref() != Some(key.as_str()) {
        memo.pending_key = Some(key.clone());
        memo.cursor = None;
    }
    let start = memo
        .cursor
        .as_deref()
        .and_then(|c| rows.iter().position(|r| r.row.label == c))
        .map_or(0, |p| p + 1);
    let mut failed = false;
    for (n, l) in rows.iter().enumerate().skip(start) {
        if l.row.state == "retired_unpublished" {
            continue;
        }
        if !within_budget() {
            // A failure earlier in this pass restarts the rebuild next run,
            // so the failed document is retried.
            if failed {
                memo.pending_key = None;
                memo.cursor = None;
            }
            pass.incomplete = true;
            return pass;
        }
        let target = Doc::Cohort("a", l.row.label.clone());
        match cohort_facts(public, i.budget, l.entry).await {
            Ok(f) => {
                let doc = cohort_doc(s, &rows, n, f.summary.as_ref(), &f.artifacts, x, i.now_ms);
                if !write(out, &target, &doc, &mut pass).await {
                    failed = true;
                }
            }
            Err(why) => {
                pass.refused.push((format!("dashboard {}", target.key()), why));
                failed = true;
            }
        }
        memo.cursor = Some(l.row.label.clone());
    }
    memo.pending_key = None;
    memo.cursor = None;
    memo.cohorts_key = (!failed).then_some(key);
    pass
}

#[cfg(test)]
#[path = "dashboard_web_model.rs"]
mod web_model;

#[cfg(test)]
#[path = "dashboard_tests.rs"]
mod tests;
