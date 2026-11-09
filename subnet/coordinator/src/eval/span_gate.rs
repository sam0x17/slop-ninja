//! The span gate in report mode: rule `slop_ninja_span_gate_v1` under the
//! 24-word floor (`slop_ninja_span_eval_floor24_v1`), computed for every
//! cohort whose plan names the optional `reward_v1.localization` block, with
//! enforcement off.
//!
//! - **Plan** ([`LocalizationPlan`]): the rule ids, the mode (`report` only
//!   in this build: a plan naming `enforce` is refused), the fixed anchor
//!   (SlopNinja's first span package) and, once measured, the frozen margins
//!   and the threshold record's digests. The block joins the gate policy the
//!   manifest binds (`v3::gate_policy_json`); plans without it are unchanged.
//! - **Lanes** request spans for every document of every job of such a cohort,
//!   so the request never reveals a document's set; the scorer's reply is
//!   read into one content-free [`SpanCell`] per batch document
//!   (`slop_ninja_span_eval::compute`) beside the document outcome. Receipts
//!   stay a function of the document outcome alone, and a malformed span
//!   reply never changes it: it is a format fault, which waives the cohort.
//! - **Anchor job**: when the anchor's package is not the reference's, a job
//!   with role `anchor` scores the batch with spans. Its results live only in
//!   private components; its attempts are receipted like any other.
//! - **Package**: the anchor check (`slop_ninja_span_eval::anchor_check`), the
//!   structural and measured conditions per executed entrant, and the signed
//!   public record [`LocalizationOutcomeV1`] beside the package and the
//!   result: one value per scored entrant, none for a withheld or unexecuted
//!   one, and the cohort state. In report mode no gate failure enters the
//!   package (`span_gate` stays false), and nothing here ever holds a
//!   package: any failure of the evaluation itself drops the record and is
//!   reported privately.
//! - **Operator void** ([`void`]): a journaled, idempotent, withdrawable
//!   action that waives the measured conditions for one cohort, accepted
//!   until its packaging starts.
//! - **Span-equivalent reference decision**: the operator's signed D10(b)
//!   decision of kind `span_equivalent` (`POST /eval/references/decision`,
//!   [`crate::eval::decide::reference_decision`]), under the offline
//!   operator-decision key. Store v30 replaced the interim
//!   `reference_decisions` table of [`SCHEMA_V29`] with that decision's own.
//!
//! Limits of this build (report mode):
//!
//! - The span cells live in `preds` columns only: no job summary carries
//!   them and no digest of a job's span rows (or of each reply's
//!   `span_output_sha256`) is stored, so the cells are not bound into the
//!   receipts or the package beyond the private components.
//! - The population signature ([`population_signature`]) is computed at
//!   packaging from this build's generation constants, not recorded when the
//!   batch was generated: a build whose constants changed between the two
//!   would not see the change (the check fails open). The trivial policies
//!   are also computed at packaging, from the stored batch texts.
//! - The operator void is `POST /eval/span-gate/void`; its state and journal
//!   show in the cohort's `span_gate` field of `/eval/status`.
//! - The package step's memory bound counts the span gate's evaluation
//!   (`arena_routes::span_gate_bytes`).

use crate::eval::engine::{self, Cohort, EvalError, R, i, opt_int, opt_text, s};
use crate::eval::plan::{ArtifactJson, CohortPlan, PlanError, PoolRef, parse_hex32};
use crate::eval::textgen;
use crate::sql::{Sql, SqlValue, bind_u64, int, text};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use slop_ninja_protocol::localization::{
    LocalizationMode, LocalizationOutcomeV1, LocalizationState, LocalizationValue,
};
use slop_ninja_protocol::{Digest, EntrantOutcome, QuantizedResult, Record};
use slop_ninja_span_eval::trivial::{Geometry, TrivialSet};
use slop_ninja_span_eval::{
    self as se, Agg, AnchorCheck, CohortState, Emission, Entrant, Kind, Margins, PublicValue, Structure, Waiver,
};
use std::collections::BTreeMap;

pub const MODE_REPORT: &str = "report";
pub const MODE_ENFORCE: &str = "enforce";
pub const METRIC_CHAR_F1: &str = "char_f1";
/// The population signature's schema ([`population_signature`]).
pub const POPULATION_SCHEMA: &str = "slop_ninja_span_gate_population_v1";
/// The threshold record's rule id (the record itself stays private).
pub const THRESHOLD_RECORD_RULE: &str = "slop_ninja_span_threshold_record_v1";
/// Role of the anchor job.
pub const ANCHOR_ROLE: &str = "anchor";

/// Whether a job of `role` scores the batch only (the references and the
/// anchor) rather than every task (an entrant).
pub fn batch_only(role: &str) -> bool {
    role != "entrant"
}

/// Whether a job of `role` has its failures attributed to its artifact (an
/// entrant's count toward its model errors; the anchor's leave its documents
/// without a prediction, which the anchor check reads) rather than pausing
/// the cohort (a reference's).
pub fn attributable(role: &str) -> bool {
    role == "entrant" || role == ANCHOR_ROLE
}

// --------------------------------------------------------------------- plan

/// The frozen margins of the measured conditions (the threshold record's).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarginsPlan {
    /// `Δ_F1`, ppb of mean character F1 over in-scope mixed documents.
    pub f1_ppb: u32,
    /// `Δ_C`, ppb of the in-context human false-positive rate.
    pub in_context_ppb: u32,
    /// `Δ_H`, human-only documents with a span.
    pub humans: u32,
    /// `n_H`, the human-only documents the margin was fitted at.
    pub n_humans: u32,
}

/// The anchor's measured range per condition, reported and alerted on,
/// never a waiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnchorDrift {
    pub f1_ppb: [u32; 2],
    pub in_context_ppb: [u32; 2],
    pub humans: [u32; 2],
}

/// The `reward_v1.localization` block (module docs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalizationPlan {
    /// [`se::GATE_RULE`].
    pub rule: String,
    /// [`se::EVAL_RULE`].
    pub eval_rule: String,
    /// [`se::DECODER_RULE`].
    pub decoder_rule: String,
    /// [`se::FLOOR_WORDS`].
    pub floor: u32,
    /// `report` (this build) or `enforce` (refused until enforcement is built).
    pub mode: String,
    /// The fixed anchor's staged package, as a plan names a reference.
    pub anchor: ArtifactJson,
    /// The anchor's inference content (lowercase hex).
    pub anchor_content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margins: Option<MarginsPlan>,
    /// `k_span`: emission failures allowed on documents where the anchor emitted.
    pub emission_max_failed: u32,
    /// [`METRIC_CHAR_F1`] (span F1 at IoU 0.5 is not built).
    pub metric: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_drift: Option<AnchorDrift>,
    /// The census's lower quantile of `|M*|`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_scope_min: Option<u32>,
    /// The measured population's signature ([`population_signature`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub population_sha256: Option<String>,
    /// The private threshold record's digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_sha256: Option<String>,
    /// A short content-free description of the measurement (like `sigma_basis`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
}

/// Why this build refuses an enforcing block.
pub const ENFORCE_REFUSAL: &str =
    "reward_v1.localization.mode (enforce is not built: this build runs the span gate in report mode only)";

impl LocalizationPlan {
    /// The block's checks (report mode only), given the plan's batch size.
    pub fn validate(&self, n: u32) -> Result<(), PlanError> {
        let e = |c: bool, what: &'static str| if c { Ok(()) } else { Err(PlanError(what)) };
        e(self.rule == se::GATE_RULE, "reward_v1.localization.rule")?;
        e(self.eval_rule == se::EVAL_RULE, "reward_v1.localization.eval_rule")?;
        e(self.decoder_rule == se::DECODER_RULE, "reward_v1.localization.decoder_rule")?;
        e(self.floor == se::FLOOR_WORDS, "reward_v1.localization.floor (24)")?;
        e(self.mode == MODE_REPORT || self.mode == MODE_ENFORCE, "reward_v1.localization.mode")?;
        e(self.mode == MODE_REPORT, ENFORCE_REFUSAL)?;
        e(self.metric == METRIC_CHAR_F1, "reward_v1.localization.metric (char_f1)")?;
        e(parse_hex32(&self.anchor_content).is_some(), "reward_v1.localization.anchor_content")?;
        e(
            parse_hex32(&self.anchor.package_sha256).is_some() && !self.anchor.manifest_key.is_empty(),
            "reward_v1.localization.anchor",
        )?;
        e(self.anchor.id().is_ok(), "reward_v1.localization.anchor (digest)")?;
        e(self.emission_max_failed < n, "reward_v1.localization.emission_max_failed (below n)")?;
        if let Some(m) = &self.margins {
            e(
                u64::from(m.f1_ppb) <= se::PPB && u64::from(m.in_context_ppb) <= se::PPB && m.humans <= n && m.n_humans > 0,
                "reward_v1.localization.margins",
            )?;
        }
        e(self.in_scope_min.is_none_or(|m| m <= n), "reward_v1.localization.in_scope_min")?;
        for d in [&self.population_sha256, &self.record_sha256].into_iter().flatten() {
            e(parse_hex32(d).is_some(), "reward_v1.localization digests")?;
        }
        e(self.basis.as_ref().is_none_or(|b| !b.is_empty() && b.len() <= 200), "reward_v1.localization.basis")?;
        Ok(())
    }

    pub fn margins(&self) -> Option<Margins> {
        self.margins.map(|m| Margins { f1_ppb: m.f1_ppb, in_context_ppb: m.in_context_ppb, humans: m.humans })
    }

    /// Whether the cohort schedules an anchor job: the anchor is not the
    /// reference's package (the reference job's spans are the anchor's then).
    pub fn anchor_job(&self, reference: &ArtifactJson) -> bool {
        self.anchor.package_sha256 != reference.package_sha256
    }

    pub fn mode(&self) -> LocalizationMode {
        if self.mode == MODE_ENFORCE { LocalizationMode::Enforce } else { LocalizationMode::Report }
    }

    /// The block as the gate policy document binds it.
    pub fn policy_json(&self) -> Value {
        json!({"rule": self.rule, "eval_rule": self.eval_rule, "decoder_rule": self.decoder_rule,
            "floor": self.floor, "mode": self.mode, "metric": self.metric,
            "anchor": {"content": self.anchor_content, "package_sha256": self.anchor.package_sha256,
                "manifest_key": self.anchor.manifest_key},
            "margins": self.margins, "emission_max_failed": self.emission_max_failed,
            "anchor_drift": self.anchor_drift, "in_scope_min": self.in_scope_min,
            "population_sha256": self.population_sha256, "record_sha256": self.record_sha256,
            "basis": self.basis, "threshold_record_rule": THRESHOLD_RECORD_RULE})
    }
}

/// The population signature of a cohort drawing from `pool`: the splice,
/// scratch and word policies and the prompt version this build generates
/// with, the generator roster (and a generation v2 plan), the pool, the mix,
/// `n`, the floor and the evaluation rule. A threshold record holds the
/// signature of the population it measured; a cohort whose own signature
/// differs is waived (`waived.population`), never gated on margins measured
/// on another population.
pub fn population_signature(plan: &CohortPlan, pool: &PoolRef) -> String {
    let v = json!({"schema": POPULATION_SCHEMA, "splice": textgen::SPLICE_POLICY,
        "scratch": textgen::SCRATCH_POLICY, "word": textgen::WORD_POLICY, "prompt": textgen::PROMPT_VERSION,
        "generators": plan.generators, "generation": plan.generation,
        "pool": {"id": pool.pool_id, "manifest_sha256": pool.manifest_sha256},
        "mix": plan.mix, "n": plan.n, "floor": se::FLOOR_WORDS, "eval_rule": se::EVAL_RULE});
    engine::sha_hex(v.to_string().as_bytes())
}

// -------------------------------------------------------------------- lanes

/// One batch document's span statistics as a lane records them beside the
/// document outcome (`preds` columns; never in a receipt).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanCell {
    /// [`se::Population`] tag.
    pub pop: u8,
    pub f1: u32,
    pub fp: u32,
    pub human: u32,
    /// [`se::DocStats::flags`]: flagged, emission failed, format fault.
    pub flags: u8,
    pub spans: u32,
}

impl SpanCell {
    pub fn of(d: &se::DocStats) -> Self {
        SpanCell { pop: d.population.tag(), f1: d.f1_ppb, fp: d.fp_human, human: d.human, flags: d.flags(), spans: d.spans }
    }

    pub fn emission_failed(&self) -> bool {
        self.flags & 2 != 0
    }
}

/// The longest decoder rule name a job's facts keep (in characters).
pub const DECODER_RULE_CHARS: usize = 64;

/// What the loaded artifact declares (the scorer's load facts): a token head
/// and its decoder. The structural conditions read them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanFacts {
    pub token_head: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoder_rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoder_min: Option<u32>,
}

impl SpanFacts {
    /// From a ready scorer's `/healthz` (`load.release`). `None` when the
    /// facts are unknown: a token head with no `decoder` load fact at all
    /// (`null` is the fact "no decoder"; an absent field is a scorer image
    /// older than the fact). Such a job records no facts, so its entrant
    /// gets no value (`facts_missing`) rather than `decoder_under_floor`.
    /// A load without a token head is a fact on every scorer image.
    pub fn of_healthz(h: &Value) -> Option<Self> {
        let r = &h["load"]["release"];
        let token_head = r["token_head"] == true;
        let d = r.get("decoder");
        if token_head && d.is_none() {
            return None;
        }
        let d = d.unwrap_or(&Value::Null);
        Some(SpanFacts {
            token_head,
            // The artifact names its rule: kept to `DECODER_RULE_CHARS`, so a
            // job's facts stay small (no long name equals the known rule).
            decoder_rule: d["rule"].as_str().map(|r| r.chars().take(DECODER_RULE_CHARS).collect()),
            decoder_min: d["min_words"].as_u64().and_then(|m| u32::try_from(m).ok()),
        })
    }

    pub fn structure(&self) -> Structure<'_> {
        Structure {
            token_head: self.token_head,
            decoder: self.decoder_rule.as_deref().zip(self.decoder_min),
        }
    }
}

/// A batch document's label as the sealed shard carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpanLabel {
    pub kind: String,
    pub gold: Option<(u32, u32)>,
}

/// The batch documents' labels of one sealed shard, by task (probe and audit
/// documents have none: they are reported only, and never get a cell).
pub fn shard_labels(body: &[u8]) -> Result<BTreeMap<u32, SpanLabel>, &'static str> {
    let mut out = BTreeMap::new();
    for line in std::str::from_utf8(body).map_err(|_| "batch shard utf-8")?.lines() {
        let v: Value = serde_json::from_str(line).map_err(|_| "batch shard line")?;
        let Some(task) = v["task"].as_u64().and_then(|t| u32::try_from(t).ok()) else {
            return Err("batch shard fields");
        };
        // Plans sealed before the `set` field carried only batch documents.
        if v.get("set").is_some_and(|s| s != "batch") {
            continue;
        }
        let gold = match (v["ai_byte_span"][0].as_u64(), v["ai_byte_span"][1].as_u64()) {
            (Some(a), Some(b)) => Some((u32::try_from(a).map_err(|_| "ai span")?, u32::try_from(b).map_err(|_| "ai span")?)),
            _ => None,
        };
        out.insert(task, SpanLabel { kind: v["kind"].as_str().unwrap_or("").to_owned(), gold });
    }
    Ok(out)
}

/// One batch document's cell: from the scorer's reply when the prediction
/// succeeded (`reply`), else as a failed prediction. `None` when the
/// document's label cannot be evaluated (a metrics error the package reads
/// as a missing cell).
pub fn cell(text: &str, label: &SpanLabel, reply: Option<&Value>, decoder_min: Option<u32>) -> Option<SpanCell> {
    let parsed = reply.map(|r| se::span_reply(r, true));
    let emission = parsed.as_ref().map_or(Emission::PredictionFailed, |p| p.emission());
    se::compute(text, Kind::of(&label.kind), label.gold, emission, decoder_min).ok().map(|d| SpanCell::of(&d))
}

// --------------------------------------------------------------------- store

/// v29: the span gate. Each prediction's span cell; each job's load facts;
/// the operator's void journal; an interim table for span-equivalent
/// reference decisions, which no route ever wrote (store v30 drops it and
/// creates the operator decision's own, `crate::eval::decide::SCHEMA_V30`).
pub const SCHEMA_V29: &[&str] = &[
    "ALTER TABLE preds ADD COLUMN span_pop INTEGER",
    "ALTER TABLE preds ADD COLUMN span_f1 INTEGER",
    "ALTER TABLE preds ADD COLUMN span_fp INTEGER",
    "ALTER TABLE preds ADD COLUMN span_human INTEGER",
    "ALTER TABLE preds ADD COLUMN span_flags INTEGER",
    "ALTER TABLE preds ADD COLUMN span_spans INTEGER",
    "ALTER TABLE jobs ADD COLUMN span_facts TEXT",
    "CREATE TABLE span_gate_actions (
        id INTEGER PRIMARY KEY, seq INTEGER NOT NULL,
        action TEXT NOT NULL CHECK (action IN ('void','withdraw')),
        decision TEXT NOT NULL UNIQUE, reason TEXT NOT NULL, at_ms INTEGER NOT NULL, epoch INTEGER
    ) STRICT",
    "CREATE TABLE reference_decisions (
        id INTEGER PRIMARY KEY, kind TEXT NOT NULL, decision TEXT NOT NULL UNIQUE,
        content TEXT NOT NULL, artifact TEXT NOT NULL, reason TEXT NOT NULL,
        body_sha256 TEXT NOT NULL, at_ms INTEGER NOT NULL, epoch INTEGER
    ) STRICT",
];

/// The cell columns' values for a prediction row (NULL without a cell).
pub fn cell_values(c: Option<&SpanCell>) -> [SqlValue; 6] {
    match c {
        Some(c) => [
            i(i64::from(c.pop)),
            i(i64::from(c.f1)),
            i(i64::from(c.fp)),
            i(i64::from(c.human)),
            i(i64::from(c.flags)),
            i(i64::from(c.spans)),
        ],
        None => [SqlValue::Null, SqlValue::Null, SqlValue::Null, SqlValue::Null, SqlValue::Null, SqlValue::Null],
    }
}

/// Records a job's load facts, first report wins (every instance loads the
/// same files).
pub fn record_facts(sql: &impl Sql, seq: i64, job: i64, f: &SpanFacts) -> R<()> {
    let j = serde_json::to_string(f).map_err(|_| EvalError::State("span facts"))?;
    sql.exec("UPDATE jobs SET span_facts = COALESCE(span_facts, ?) WHERE seq = ? AND job = ?", &[s(j), i(seq), i(job)])?;
    Ok(())
}

// --------------------------------------------------------------- operator void

/// The phases in which the operator may void (or withdraw a void of) a
/// cohort's span gate: before its packaging starts.
const VOID_PHASES: [&str; 6] = ["queued", "admission", "generating", "sealing", "scoring", "packaging"];

/// The operator's void of one cohort's span gate (`POST /span-gate/void`),
/// or its withdrawal. Journaled in `span_gate_actions`; a repeated decision
/// id with the same body answers the stored result, with another body
/// `decision_reused`. Refused once the cohort's packaging has started
/// (`span_gate_void_after_packaging`), so the package reads one settled
/// state. Signed with the offline operator-decision key once that key
/// exists; until then the operator's authentication carries it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoidRequest {
    pub label: String,
    pub decision: String,
    pub reason: String,
    #[serde(default)]
    pub withdraw: bool,
}

pub fn void(sql: &impl Sql, v: &VoidRequest, now_ms: u64) -> R<Value> {
    let code_ok = |x: &str| !x.is_empty() && x.len() <= 128 && x.bytes().all(|b| b.is_ascii_graphic() || b == b' ');
    if !code_ok(&v.decision) || !code_ok(&v.reason) {
        return Err(EvalError::Plan("span_gate_void_fields: decision and reason are 1 to 128 printable characters"));
    }
    let action = if v.withdraw { "withdraw" } else { "void" };
    let prior = sql.exec("SELECT seq, action, reason FROM span_gate_actions WHERE decision = ?", &[s(&v.decision)])?;
    let c = engine::cohorts(sql)?.into_iter().find(|c| c.label == v.label).ok_or(EvalError::Plan("cohort_unknown"))?;
    if let Some(p) = prior.first() {
        if int(p, 0)? == c.seq && text(p, 1)? == action && text(p, 2)? == v.reason {
            return Ok(json!({"label": c.label, "action": action, "replayed": true, "voided": voided(sql, c.seq)?}));
        }
        return Err(EvalError::Plan("decision_reused"));
    }
    if !VOID_PHASES.contains(&c.phase.as_str()) || c.phase == "packaging" {
        return Err(EvalError::Plan("span_gate_void_after_packaging: the package reads the gate state when packaging starts"));
    }
    if c.plan.reward_v1.as_ref().and_then(|r| r.localization.as_ref()).is_none() {
        return Err(EvalError::Plan("span_gate_absent: the cohort's plan names no localization block"));
    }
    let now_void = voided(sql, c.seq)?;
    if v.withdraw && !now_void {
        return Err(EvalError::Plan("span_gate_not_voided"));
    }
    if !v.withdraw && now_void {
        return Err(EvalError::Plan("span_gate_already_voided"));
    }
    let o = engine::observed(sql)?;
    sql.exec(
        "INSERT INTO span_gate_actions (seq, action, decision, reason, at_ms, epoch) VALUES (?, ?, ?, ?, ?, ?)",
        &[
            i(c.seq),
            s(action),
            s(&v.decision),
            s(&v.reason),
            bind_u64(now_ms)?,
            o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)),
        ],
    )?;
    Ok(json!({"label": c.label, "action": action, "replayed": false, "voided": !v.withdraw}))
}

/// Whether the cohort's span gate stands voided: its newest action is a void.
pub fn voided(sql: &impl Sql, seq: i64) -> R<bool> {
    let r = sql.exec("SELECT action FROM span_gate_actions WHERE seq = ? ORDER BY id DESC LIMIT 1", &[i(seq)])?;
    Ok(r.first().map(|r| text(r, 0)).transpose()?.is_some_and(|a| a == "void"))
}

/// The cohort's span-gate state on the operator's status route: `null`
/// without the block; else the mode, whether the measured conditions are
/// voided, and the void journal. No span value or statistic: those exist
/// only from packaging, in the private components.
pub fn status(sql: &impl Sql, c: &Cohort) -> R<Value> {
    let Some(block) = c.plan.reward_v1.as_ref().and_then(|v| v.localization.as_ref()) else {
        return Ok(Value::Null);
    };
    Ok(json!({"mode": block.mode, "voided": voided(sql, c.seq)?, "actions": void_journal(sql, c.seq)?}))
}

/// The cohort's void journal, for its status.
pub fn void_journal(sql: &impl Sql, seq: i64) -> R<Value> {
    let rows = sql.exec("SELECT id, action, decision, reason, at_ms, epoch FROM span_gate_actions WHERE seq = ? ORDER BY id", &[i(seq)])?;
    rows.iter()
        .map(|r| {
            Ok(json!({"id": int(r, 0)?, "action": text(r, 1)?, "decision": text(r, 2)?, "reason": text(r, 3)?,
                "at_ms": int(r, 4)?, "epoch": opt_int(r, 5)}))
        })
        .collect::<R<Vec<_>>>()
        .map(Value::from)
}

// ------------------------------------------------------------------- package

/// The cohort's localization outcome, built at packaging.
#[derive(Clone, Debug)]
pub struct LocalizationBuild {
    /// The public record, signed with the package key beside the result.
    pub record: Option<LocalizationOutcomeV1>,
    /// Private: every entrant's statistics, the anchor's, the anchor check
    /// and the advisory reasons (the score components' `localization` block).
    pub components: Value,
}

/// One job's sums over its batch predictions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct JobStats {
    agg: Agg,
    /// Batch predictions without a cell.
    missing: u32,
    /// Batch predictions.
    rows: u32,
    /// Batch documents whose prediction failed (a resolved model error,
    /// parse failure or invalid output).
    failed: u32,
    facts: Option<SpanFacts>,
}

fn job_stats(sql: &impl Sql, seq: i64) -> R<BTreeMap<i64, JobStats>> {
    let mut out: BTreeMap<i64, JobStats> = BTreeMap::new();
    let rows = sql.exec(
        "SELECT p.job, COUNT(*),
            SUM(CASE WHEN p.span_pop IS NULL THEN 1 ELSE 0 END),
            SUM(CASE WHEN p.span_pop = 1 THEN 1 ELSE 0 END),
            SUM(CASE WHEN p.span_pop = 1 THEN p.span_f1 ELSE 0 END),
            SUM(CASE WHEN p.span_pop = 1 THEN p.span_fp ELSE 0 END),
            SUM(CASE WHEN p.span_pop = 1 THEN p.span_human ELSE 0 END),
            SUM(CASE WHEN p.span_pop = 2 THEN 1 ELSE 0 END),
            SUM(CASE WHEN p.span_pop = 2 THEN (p.span_flags & 1) ELSE 0 END),
            SUM(CASE WHEN p.span_pop IS NOT NULL THEN ((p.span_flags >> 1) & 1) ELSE 0 END),
            SUM(CASE WHEN p.span_pop IS NOT NULL THEN ((p.span_flags >> 2) & 1) ELSE 0 END),
            SUM(COALESCE(p.span_spans, 0)),
            SUM(CASE WHEN p.outcome != 1 THEN 1 ELSE 0 END)
         FROM preds p JOIN docs d ON d.seq = p.seq AND d.task = p.task
         WHERE p.seq = ? AND d.docset = 'batch' GROUP BY p.job ORDER BY p.job",
        &[i(seq)],
    )?;
    let u = |r: &crate::sql::Row, k: usize| -> R<u64> { u64::try_from(opt_int(r, k).unwrap_or(0)).map_err(|_| EvalError::State("span sums")) };
    for r in &rows {
        let rows_n = u(r, 1)? as u32;
        let missing = u(r, 2)? as u32;
        let agg = Agg {
            docs: rows_n - missing,
            in_scope: u(r, 3)? as u32,
            f1_sum: u(r, 4)?,
            fp_human: u(r, 5)?,
            human: u(r, 6)?,
            humans: u(r, 7)? as u32,
            flagged: u(r, 8)? as u32,
            emission_failed: u(r, 9)? as u32,
            format_faults: u(r, 10)? as u32,
            spans: u(r, 11)?,
        };
        let failed = u(r, 12)? as u32;
        out.insert(int(r, 0)?, JobStats { agg, missing, rows: rows_n, failed, ..JobStats::default() });
    }
    for r in &sql.exec("SELECT job, span_facts FROM jobs WHERE seq = ?", &[i(seq)])? {
        if let (Some(j), Some(f)) = (out.get_mut(&int(r, 0)?), opt_text(r, 1)) {
            j.facts = serde_json::from_str(&f).ok();
        }
    }
    Ok(out)
}

/// Each job's emission failures on batch documents where `anchor`'s job
/// emitted: its prediction succeeded and its emission did not fail. A
/// document where the anchor's prediction or emission failed is no excess
/// (`|E \ A|`, `slop_ninja_span_eval::emission_excess`, counted in the store
/// so no task list is held). Jobs without one are absent.
fn emission_excess(sql: &impl Sql, seq: i64, anchor: i64) -> R<BTreeMap<i64, u32>> {
    sql.exec(
        "SELECT p.job, COUNT(*) FROM preds p JOIN docs d ON d.seq = p.seq AND d.task = p.task
         WHERE p.seq = ? AND d.docset = 'batch' AND p.span_pop IS NOT NULL AND (p.span_flags & 2) != 0
            AND NOT EXISTS (SELECT 1 FROM preds a WHERE a.seq = p.seq AND a.job = ? AND a.task = p.task
                AND (a.outcome != 1 OR (a.span_pop IS NOT NULL AND (a.span_flags & 2) != 0)))
         GROUP BY p.job",
        &[i(seq), i(anchor)],
    )?
    .iter()
    .map(|r| Ok((int(r, 0)?, u32::try_from(int(r, 1)?).map_err(|_| EvalError::State("excess"))?)))
    .collect()
}

/// Batch documents read per page by [`trivial_set`]: each page's texts are
/// held at once, one document's geometry at a time
/// (`arena_routes::span_gate_bytes` counts one page).
pub const TRIVIAL_PAGE: i64 = 16;

/// The trivial policies over the batch, from the anchor job's predicted
/// shares and the batch texts (held until close).
fn trivial_set(sql: &impl Sql, seq: i64, anchor_job: i64) -> R<TrivialSet> {
    let mut set = TrivialSet::default();
    let mut after = -1i64;
    loop {
        let page = sql.exec(
            "SELECT d.task, d.kind, d.text, d.span_start, d.span_end, p.outcome, p.share_ppb
             FROM docs d LEFT JOIN preds p ON p.seq = d.seq AND p.task = d.task AND p.job = ?
             WHERE d.seq = ? AND d.docset = 'batch' AND d.task > ? ORDER BY d.task LIMIT ?",
            &[i(anchor_job), i(seq), i(after), i(TRIVIAL_PAGE)],
        )?;
        for r in &page {
            let txt = opt_text(r, 2).ok_or(EvalError::State("batch text"))?;
            let gold = match (opt_int(r, 3), opt_int(r, 4)) {
                (Some(a), Some(b)) => Some((u32::try_from(a).map_err(|_| EvalError::State("span"))?, u32::try_from(b).map_err(|_| EvalError::State("span"))?)),
                _ => None,
            };
            let pop = se::population(&txt, Kind::of(text(r, 1)?), gold).map_err(|e| EvalError::State(e.0))?;
            let share = match (opt_int(r, 5), opt_int(r, 6)) {
                (Some(1), Some(sh)) => Some(u32::try_from(sh).map_err(|_| EvalError::State("share"))?),
                _ => None,
            };
            set.add(&Geometry::of(&txt, pop, gold, &textgen::sentences(&txt)), share);
        }
        match page.last() {
            Some(r) if page.len() as i64 == TRIVIAL_PAGE => after = int(r, 0)?,
            _ => break,
        }
    }
    Ok(set)
}

fn agg_json(a: &Agg) -> Value {
    json!({"docs": a.docs, "in_scope": a.in_scope, "mean_f1_ppb": a.mean_f1_ppb(), "in_context_ppb": a.in_context_ppb(),
        "humans": a.humans, "flagged": a.flagged, "emission_failed": a.emission_failed,
        "format_faults": a.format_faults, "spans": a.spans})
}

fn value_of(p: PublicValue) -> LocalizationValue {
    match p {
        PublicValue::Passed => LocalizationValue::Passed,
        PublicValue::NoTokenHead => LocalizationValue::NoTokenHead,
        PublicValue::DecoderUnderFloor => LocalizationValue::DecoderUnderFloor,
        PublicValue::BelowAnchor => LocalizationValue::BelowAnchor,
    }
}

fn state_of(s: CohortState) -> LocalizationState {
    match s {
        CohortState::Applied => LocalizationState::Applied,
        CohortState::Waived => LocalizationState::Waived,
        CohortState::Void => LocalizationState::Void,
    }
}

/// Inputs from the package being built.
pub struct PackageInputs<'a> {
    pub cohort: &'a Cohort,
    pub block: &'a LocalizationPlan,
    pub result: &'a QuantizedResult,
    pub gate_policy: Digest,
    /// The cohort's batch size.
    pub n: u32,
    /// Job number of the first entrant (after the references).
    pub first_entrant_job: i64,
    /// Whether the cohort drew fresh stock (its population's pool).
    pub fresh: bool,
    /// The cohort's complete counted bits at the result's grid
    /// (`quant::cohort_bits` under `RewardV1Plan::counted_terms`, this
    /// record's term included): the record's `result_bits`.
    pub counted_bits: u32,
}

/// The build when the evaluation itself fails: no public record, and why in
/// the private components. The package is unaffected.
pub fn evaluation_failed(block: &LocalizationPlan, e: &EvalError) -> LocalizationBuild {
    LocalizationBuild {
        record: None,
        components: json!({"rule": se::GATE_RULE, "mode": block.mode, "error": e.code(),
            "note": "the span gate evaluation failed: no localization record; the package is unaffected"}),
    }
}

/// The cohort's span-gate outcome (module docs). Never an error that holds
/// the package: a failure of the evaluation itself drops the public record
/// and reports why in the private components.
pub fn evaluate(sql: &impl Sql, p: &PackageInputs<'_>) -> LocalizationBuild {
    match evaluate_inner(sql, p) {
        Ok(b) => b,
        Err(e) => evaluation_failed(p.block, &e),
    }
}

fn evaluate_inner(sql: &impl Sql, p: &PackageInputs<'_>) -> R<LocalizationBuild> {
    let c = p.cohort;
    let block = p.block;
    let stats = job_stats(sql, c.seq)?;
    let roles = sql.exec("SELECT job, role FROM jobs WHERE seq = ? ORDER BY job", &[i(c.seq)])?;
    let job_of = |role: &str| -> R<Option<i64>> {
        Ok(roles.iter().find(|r| text(r, 1).is_ok_and(|x| x == role)).map(|r| int(r, 0)).transpose()?)
    };
    let anchor_job = if block.anchor_job(&c.plan.reference) { job_of(ANCHOR_ROLE)? } else { job_of("reference")? };
    let complete = |j: &JobStats| j.missing == 0 && j.rows == p.n;
    let margins = block.margins();
    let k_span = block.emission_max_failed;
    // An anchor whose predictions failed on more batch documents than
    // `k_span` was not scored on the batch: check (a).
    let anchor = anchor_job.and_then(|j| stats.get(&j)).filter(|a| a.failed <= k_span);
    let anchor_complete = anchor.filter(|a| complete(a));
    let metrics_error = anchor.is_some() && anchor_complete.is_none();
    let format_faults: u32 = stats.values().map(|j| j.agg.format_faults).sum();
    let population = population_signature(
        &c.plan,
        match (&c.plan.admission, p.fresh) {
            (Some(a), true) => a.fresh_pool.as_ref().unwrap_or(&c.plan.pool),
            _ => &c.plan.pool,
        },
    );
    let population_ok = block.population_sha256.as_ref().is_none_or(|x| *x == population);
    let trivial = match (margins.as_ref(), anchor_job, anchor_complete) {
        (Some(_), Some(j), Some(_)) => Some(trivial_set(sql, c.seq, j)?),
        _ => None,
    };
    let trivial_passes = match (&trivial, margins.as_ref(), anchor_complete) {
        (Some(t), Some(m), Some(a)) => t.any_passes(m, &a.agg).map_err(|_| EvalError::State("trivial policies unpaired"))?,
        _ => false,
    };
    let void = voided(sql, c.seq)?;
    let waiver = se::anchor_check(&AnchorCheck {
        void,
        anchor: anchor_complete.map(|a| &a.agg),
        k_span,
        format_faults,
        trivial_passes,
        in_scope_min: block.in_scope_min,
        metrics_error,
        population_ok,
        margins: margins.as_ref(),
    });
    let state = waiver.map_or(CohortState::Applied, Waiver::state);
    let applies = waiver.is_none();
    // Emission failures where the anchor emitted (its prediction and its
    // emission succeeded); without a complete anchor, every one.
    let excess_by_job = match (anchor_job, anchor_complete) {
        (Some(j), Some(_)) => Some(emission_excess(sql, c.seq, j)?),
        _ => None,
    };
    let excess_of = |j: i64, js: &JobStats| match &excess_by_job {
        Some(m) => m.get(&j).copied().unwrap_or(0),
        None => js.agg.emission_failed,
    };
    let mut values = Vec::with_capacity(p.result.entrants.len());
    let mut private = Vec::with_capacity(p.result.entrants.len());
    for (pos, e) in p.result.entrants.iter().enumerate() {
        let job = p.first_entrant_job + pos as i64;
        let js = stats.get(&job);
        let scored = matches!(e.outcome, EntrantOutcome::Scored { .. });
        let (value, reason, advisory) = match js {
            Some(js) if scored => {
                let facts = js.facts.clone();
                let excess = excess_of(job, js);
                match &facts {
                    None => (None, Some("facts_missing".to_owned()), None),
                    Some(f) => {
                        let ent = Entrant { structure: f.structure(), agg: &js.agg, emission_excess: excess };
                        let measured_ok = complete(js);
                        let advisory = match (margins.as_ref(), anchor_complete, measured_ok) {
                            (Some(m), Some(a), true) => {
                                se::advisory(&ent, &a.agg, m, k_span).ok().map(|r| r.map_or("passed", |r| r.code()))
                            }
                            _ => None,
                        };
                        if let Some(r) = se::structural(&f.structure()) {
                            (Some(value_of(r.public())), Some(r.code().to_owned()), advisory)
                        } else if !applies {
                            // Waived or void: the measured conditions decide no entrant.
                            (Some(LocalizationValue::Passed), None, advisory)
                        } else if !measured_ok {
                            (None, Some("entrant_metrics_incomplete".to_owned()), advisory)
                        } else {
                            let anchor_agg = anchor_complete.map_or(Agg::default(), |a| a.agg);
                            match se::verdict(&ent, &anchor_agg, margins.as_ref(), k_span, true) {
                                Ok(r) => (Some(value_of(PublicValue::of(r))), r.map(|r| r.code().to_owned()), advisory),
                                Err(_) => (None, Some("unpaired".to_owned()), advisory),
                            }
                        }
                    }
                }
            }
            _ => (None, None, None),
        };
        values.push(value);
        private.push(json!({"position": pos, "job": job, "executed": e.outcome.executed(), "scored": scored,
            "value": value.map(LocalizationValue::code), "reason": reason, "advisory": advisory,
            "facts": js.and_then(|j| j.facts.clone()), "stats": js.map(|j| agg_json(&j.agg)),
            "missing": js.map(|j| j.missing),
            "emission_excess": js.map(|j| excess_of(job, j))}));
    }
    let record = LocalizationOutcomeV1 {
        chain: p.result.chain.clone(),
        cohort: p.result.cohort,
        cohort_label: p.result.cohort_label.clone(),
        cohort_manifest: p.result.cohort_manifest,
        result: p.result.digest().map_err(|_| EvalError::State("result digest"))?,
        mode: block.mode(),
        gate_policy: p.gate_policy,
        anchor_content: Digest(parse_hex32(&block.anchor_content).ok_or(EvalError::State("anchor content"))?),
        state: state_of(state),
        bits: LocalizationOutcomeV1::count_bits(&values),
        entrants: values,
        result_bits: p.counted_bits,
        cases: p.result.cases,
        identify_bits: p.result.params.quant.identify_bits,
        budget_factor: p.result.params.quant.budget_factor,
    };
    record.validate().map_err(|_| EvalError::State("localization record"))?;
    record.check_result(p.result).map_err(|_| EvalError::State("localization record against the result"))?;
    let drift = match (block.anchor_drift, anchor_complete) {
        (Some(d), Some(a)) => {
            let out = |v: Option<u64>, r: [u32; 2]| v.is_some_and(|v| v < u64::from(r[0]) || v > u64::from(r[1]));
            Some(json!({"f1": out(a.agg.mean_f1_ppb(), d.f1_ppb), "in_context": out(a.agg.in_context_ppb(), d.in_context_ppb),
                "humans": u64::from(a.agg.flagged) < u64::from(d.humans[0]) || u64::from(a.agg.flagged) > u64::from(d.humans[1])}))
        }
        _ => None,
    };
    let components = json!({
        "rule": se::GATE_RULE, "eval_rule": se::EVAL_RULE, "mode": block.mode, "state": state.code(),
        "waiver": waiver.map(Waiver::code), "void": void, "k_span": k_span, "margins": block.margins,
        "anchor": {"job": anchor_job, "content": block.anchor_content, "stats": anchor.map(|a| agg_json(&a.agg)),
            "missing": anchor.map(|a| a.missing), "rows": anchor.map(|a| a.rows),
            "failed": anchor_job.and_then(|j| stats.get(&j)).map(|a| a.failed), "drift_alert": drift},
        "format_faults": format_faults,
        "population_sha256": population, "population_ok": population_ok,
        "trivial": trivial.as_ref().map(|t| json!({"policies": t.policies.len(), "passes": trivial_passes,
            "best_within_anchor": anchor_complete.and_then(|a| a.agg.in_context_ppb())
                .and_then(|c| t.best_f1_within(c.saturating_add(margins.map_or(0, |m| u64::from(m.in_context_ppb)))))
                .map(|(p, a)| json!({"policy": p.name(), "stats": agg_json(&a)}))})),
        "entrants": private,
        "record_bits": record.bits, "result_bits": record.result_bits,
    });
    Ok(LocalizationBuild { record: Some(record), components })
}

#[cfg(test)]
mod tests;
