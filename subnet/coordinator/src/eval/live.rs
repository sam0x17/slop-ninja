//! Live values while a cohort is scored (`reward_v1.live`, rule
//! [`slop_ninja_protocol::live::LIVE_RULE`]): pre-finalized scores and
//! `live-v1` segment readings, served on the internal `GET /eval/live`
//! (report rule [`REPORT_RULE`]) and stopped or resumed by the operator's
//! `POST /eval/live/stop`.
//!
//! - **Setup after the seal** ([`prepare`]). Once the seal has committed, the
//!   scoring tick derives `L` from the sealed roster
//!   (`live::derive_segments`, every rule's counted bits included), computes
//!   the partition of the batch from the task positions and the lane plan
//!   (`live::partition`) and stores both (`live_cohorts`,
//!   `docs.live_segment`, batch rows only) in one transaction. Any error
//!   there rolls it back and a second transaction stores `segments = 0` with
//!   the fault ([`note_fault`]): the cohort runs on without readings. The
//!   seal and its transaction are never touched, so a live defect cannot
//!   hold the seal, scoring, packaging or any phase.
//! - **Releases** ([`release_due`]). Each call releases at most one value of
//!   one subject (an executed entrant or `R_k`), insert-once, keyed with a
//!   revision, never recomputed. A reading of checkpoint `s` is released when
//!   every batch task of segments `1..=s` has a final outcome for the
//!   subject (a prediction or an attributable model error; an
//!   infrastructure retry keeps it pending). A pre-finalized value is
//!   released when its subject's whole job is done and `R_k` finished the
//!   batch without a failure, computed with the package's own function
//!   (`reward_v1::preview_q`). Releases happen only between the seal and the
//!   close, never in a paused or faulted cohort, never while an operator stop
//!   covers the cohort, and a failure records a fault that stops further live
//!   values for that cohort only.
//! - **Report** ([`report`]): read from the tables only. No timestamp, no
//!   exact value, no segment membership, nothing per task, chunk or document,
//!   nothing from the probe or audits.
//! - **Stop** ([`set_stop`]): global or per cohort, journaled with its reason
//!   (`live_stop`, `live_stop_log`), no expiry; it holds nothing else.
//! - **Package cross-check** ([`cross_check`]): the package wins; a shown
//!   value that differs is recorded as a `prefinal_mismatch` defect and
//!   counted as one more release in the cohort's private terms, never in a
//!   signed record, and never holds the package. An excess over the bound
//!   sets the global stop.

use crate::eval::engine::{Cohort, EvalConfig, EvalError, R, cohort, cohort_seed, i, observed, opt_int, opt_text, s};
use crate::eval::plan::LivePlan;
use crate::eval::v3;
use crate::sql::{Sql, SqlValue, int, text};
use serde::Deserialize;
use serde_json::{Value, json};
use slop_ninja_protocol::live;
use slop_ninja_protocol::quant::{self, CohortBinding, CountedTerms, Subject};
use slop_ninja_protocol::reward::brier_utility;
use slop_ninja_protocol::reward_v1::{self, PreviewInputs};
use slop_ninja_protocol::{Probability, Score};

#[cfg(test)]
thread_local! {
    /// Test hook: the setup fails (a checked-arithmetic or shape error).
    pub static FAIL_PREPARE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The report's rule.
pub const REPORT_RULE: &str = "slop_ninja.eval.live_scores/v1";
/// Fault codes: the setup, a release, or the private recount over the bound.
pub const FAULT_PREPARE: &str = "prepare";
pub const FAULT_RELEASE: &str = "release";
pub const FAULT_OVER_BOUND: &str = "over_bound";
/// The defect a package records for a shown value it does not carry.
pub const PREFINAL_MISMATCH: &str = "prefinal_mismatch";
/// The global stop's scope.
pub const GLOBAL: &str = "*";
/// The most rows of a report: one per entrant the protocol's roster can hold.
pub const ROWS: usize = slop_ninja_protocol::MAX_ROSTER as usize;
/// The longest stop reason, in bytes.
pub const REASON_BYTES: usize = 200;
/// The phases between the seal and the close.
const RUNNING: &str = "('scoring','packaging','transition')";

/// v28: live values while a cohort is scored.
/// - `live_cohorts`: one row per cohort once the setup ran (`segments` 0
///   means no readings), with the derived counts and the private recount
///   (`terms`, JSON), the quantization width and margin line once `R_k`'s
///   value is released, the fault, and the prediction count the last full
///   release check saw (`checked_preds`, so a kick finds due work cheaply).
/// - `live_releases`: every released value, insert-once, keyed with its
///   revision; `released_epoch` is private and tempo-level.
/// - `live_stop`, `live_stop_log`: the operator's stops and resumes.
/// - `live_defects`: the package cross-check's defects (values private).
/// - `docs.live_segment`: a batch task's segment; null on every other row.
pub const SCHEMA_V28: &[&str] = &[
    "CREATE TABLE live_cohorts (
        seq INTEGER PRIMARY KEY, segments INTEGER NOT NULL, segments_max INTEGER NOT NULL,
        eps_live_milli INTEGER NOT NULL, prefinal INTEGER NOT NULL, executed INTEGER NOT NULL,
        predecessor INTEGER NOT NULL, n INTEGER NOT NULL, width_units TEXT,
        counted INTEGER NOT NULL, live_bits INTEGER NOT NULL, bound INTEGER NOT NULL,
        terms TEXT NOT NULL DEFAULT '{}', quant_width TEXT, margin_line TEXT, fault TEXT,
        checked_preds INTEGER NOT NULL DEFAULT -1
    ) STRICT",
    "CREATE TABLE live_releases (
        seq INTEGER NOT NULL, job INTEGER NOT NULL, kind TEXT NOT NULL CHECK (kind IN ('reading','prefinal')),
        checkpoint INTEGER NOT NULL, value_units TEXT,
        outcome TEXT NOT NULL CHECK (outcome IN ('value','withheld','none')),
        revision INTEGER NOT NULL DEFAULT 0, released_epoch INTEGER,
        PRIMARY KEY (seq, job, kind, checkpoint, revision)
    ) STRICT",
    "CREATE TABLE live_stop (
        scope TEXT PRIMARY KEY, stopped INTEGER NOT NULL, reason TEXT NOT NULL, set_epoch INTEGER
    ) STRICT",
    "CREATE TABLE live_stop_log (
        id INTEGER PRIMARY KEY, scope TEXT NOT NULL, stopped INTEGER NOT NULL, reason TEXT NOT NULL, set_epoch INTEGER
    ) STRICT",
    "CREATE TABLE live_defects (
        seq INTEGER NOT NULL, job INTEGER NOT NULL, rule TEXT NOT NULL, shown TEXT, signed TEXT,
        PRIMARY KEY (seq, job, rule)
    ) STRICT",
    "ALTER TABLE docs ADD COLUMN live_segment INTEGER",
];

fn state_err(what: &'static str) -> EvalError {
    EvalError::State(what)
}

fn live_plan(c: &Cohort) -> Option<&LivePlan> {
    c.plan.reward_v1.as_ref()?.live.as_ref()
}

fn epoch(sql: &impl Sql) -> R<SqlValue> {
    Ok(observed(sql)?.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)))
}

fn count(sql: &impl Sql, q: &str, p: &[SqlValue]) -> R<i64> {
    Ok(int(sql.exec(q, p)?.first().ok_or(state_err("count"))?, 0)?)
}

// ------------------------------------------------------------------ setup

/// Sealed cohorts between the seal and the close whose plan has a live block
/// and whose setup has not run.
pub fn pending_setup(sql: &impl Sql) -> R<Vec<i64>> {
    let rows = sql.exec(
        &format!(
            "SELECT c.seq FROM cohorts c WHERE c.phase IN {RUNNING}
               AND json_extract(c.plan, '$.reward_v1.live') IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM live_cohorts l WHERE l.seq = c.seq)
               AND EXISTS (SELECT 1 FROM jobs j WHERE j.seq = c.seq)
             ORDER BY c.seq"
        ),
        &[],
    )?;
    rows.iter().map(|r| Ok(int(r, 0)?)).collect()
}

/// The setup right after the seal: `L` from the sealed `X` and reference
/// shape, the partition, and the cohort's row, in the caller's
/// transaction. Idempotent: a stored row means done.
pub fn prepare(sql: &impl Sql, seq: i64) -> R<()> {
    if count(sql, "SELECT COUNT(*) FROM live_cohorts WHERE seq = ?", &[i(seq)])? > 0 {
        return Ok(());
    }
    #[cfg(test)]
    if FAIL_PREPARE.with(std::cell::Cell::get) {
        return Err(state_err("injected setup failure"));
    }
    let c = cohort(sql, seq)?;
    let lp = live_plan(&c).ok_or(state_err("live block"))?.clone();
    let v1 = c.plan.reward_v1.as_ref().ok_or(state_err("reward_v1 block"))?;
    let q = v1.quant_params().map_err(|e| EvalError::Plan(e.0))?;
    let st = v3::state(sql, seq)?;
    let n = st.batch_tasks;
    let executed = u16::try_from(count(sql, "SELECT COUNT(*) FROM jobs WHERE seq = ? AND role = 'entrant'", &[i(seq)])?)
        .map_err(|_| state_err("roster"))?;
    let predecessor = c.plan.previous_reference.is_some();
    let params = lp.params();
    // Every other rule's counted terms: none today. A rule that publishes
    // hidden-data bits adds them here, which lowers L without live code.
    let d = live::derive_segments(n, executed, predecessor, &q, &params, &CountedTerms::NONE)
        .map_err(|_| state_err("live derivation"))?;
    let mut width = SqlValue::Null;
    if d.segments >= live::MIN_SEGMENTS {
        width = s(live::width_live(n, d.segments, executed, params.eps_live_milli)
            .map_err(|_| state_err("live width"))?
            .to_string());
        let batch = sql.exec(
            "SELECT idx, task FROM docs WHERE seq = ? AND docset = 'batch' AND task IS NOT NULL ORDER BY task",
            &[i(seq)],
        )?;
        if batch.len() != n as usize {
            return Err(state_err("batch tasks"));
        }
        let positions =
            batch.iter().map(|r| u32::try_from(int(r, 1)?).map_err(|_| state_err("task"))).collect::<R<Vec<u32>>>()?;
        let (lanes, chunks) = entrant_shape(sql, &c)?;
        let segs = live::partition(&positions, c.plan.scorer.chunk_docs, lanes, chunks, d.segments)
            .map_err(|_| state_err("live partition"))?;
        for (r, seg) in batch.iter().zip(&segs) {
            sql.exec(
                "UPDATE docs SET live_segment = ? WHERE seq = ? AND idx = ? AND docset = 'batch'",
                &[i(i64::from(*seg)), i(seq), i(int(r, 0)?)],
            )?;
        }
    }
    sql.exec(
        "INSERT INTO live_cohorts (seq, segments, segments_max, eps_live_milli, prefinal, executed, predecessor, n,
            width_units, counted, live_bits, bound) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            i(seq),
            i(i64::from(d.segments)),
            i(i64::from(lp.segments_max)),
            i(i64::from(lp.eps_live_milli)),
            i(i64::from(lp.prefinal)),
            i(i64::from(executed)),
            i(i64::from(predecessor)),
            i(i64::from(n)),
            width,
            i(i64::from(d.counted)),
            i(i64::from(d.live)),
            i(i64::try_from(d.bound).map_err(|_| state_err("bound"))?),
        ],
    )?;
    Ok(())
}

/// An entrant job's planned lane count `W` and chunk count `C` (every
/// entrant job scores the same task list), with its windows checked to be
/// `k * chunk_docs`; `(1, C)` when no entrant was executed.
fn entrant_shape(sql: &impl Sql, c: &Cohort) -> R<(u32, u32)> {
    let per = i64::from(c.plan.scorer.chunk_docs.max(1));
    let total = i64::from(c.task_count.ok_or(state_err("task count"))?);
    let chunks = u32::try_from((total + per - 1) / per).map_err(|_| state_err("chunks"))?;
    let job = sql.exec("SELECT job, chunks FROM jobs WHERE seq = ? AND role = 'entrant' ORDER BY job LIMIT 1", &[i(c.seq)])?;
    let Some(j) = job.first() else { return Ok((1, chunks)) };
    let job = int(j, 0)?;
    if int(j, 1)? != i64::from(chunks) {
        return Err(state_err("entrant chunks"));
    }
    let off = count(
        sql,
        "SELECT COUNT(*) FROM chunks WHERE seq = ? AND job = ? AND first_task != chunk * ?",
        &[i(c.seq), i(job), i(per)],
    )?;
    if off != 0 {
        return Err(state_err("entrant windows"));
    }
    let lanes = count(sql, "SELECT COUNT(*) FROM lanes WHERE seq = ? AND kind = 'score' AND job = ?", &[i(c.seq), i(job)])?;
    Ok((u32::try_from(lanes.max(1)).map_err(|_| state_err("lanes"))?, chunks))
}

/// Records a live fault for `seq` (its own transaction, after the failing
/// one rolled back). A failed setup stores `segments = 0`: the cohort runs
/// on without readings, and its pre-finalized values, which need no
/// partition, still appear. A failed release (or the private recount over
/// the bound) stops every live value of the cohort. Never touches the
/// cohort's phase, fault or work.
pub fn note_fault(sql: &impl Sql, seq: i64, code: &'static str) -> R<()> {
    let exists = count(sql, "SELECT COUNT(*) FROM live_cohorts WHERE seq = ?", &[i(seq)])? > 0;
    if exists {
        sql.exec("UPDATE live_cohorts SET fault = COALESCE(fault, ?) WHERE seq = ?", &[s(code), i(seq)])?;
        return Ok(());
    }
    // What the pre-finalized values need, when it can be read; else none.
    let basics = (|| -> R<[i64; 4]> {
        let c = cohort(sql, seq)?;
        let prefinal = live_plan(&c).is_some_and(|l| l.prefinal);
        let executed = count(sql, "SELECT COUNT(*) FROM jobs WHERE seq = ? AND role = 'entrant'", &[i(seq)])?;
        Ok([i64::from(prefinal), executed, i64::from(c.plan.previous_reference.is_some()), i64::from(v3::state(sql, seq)?.batch_tasks)])
    })()
    .unwrap_or([0; 4]);
    sql.exec(
        "INSERT INTO live_cohorts (seq, segments, segments_max, eps_live_milli, prefinal, executed, predecessor, n,
            counted, live_bits, bound, fault) VALUES (?, 0, 0, 0, ?, ?, ?, ?, 0, 0, 0, ?)",
        &[i(seq), i(basics[0]), i(basics[1]), i(basics[2]), i(basics[3]), s(code)],
    )?;
    Ok(())
}

/// Whether a stored fault stops every live value (a failed setup stops
/// readings only).
fn stops_all(fault: Option<&str>) -> bool {
    fault.is_some_and(|f| f != FAULT_PREPARE)
}

/// The batch size `n` of cohort `seq`'s plan, for the step's footprint.
pub fn cohort_docs(sql: &impl Sql, seq: i64) -> R<u64> {
    let rows = sql.exec("SELECT json_extract(plan, '$.n') FROM cohorts WHERE seq = ?", &[i(seq)])?;
    Ok(rows.first().and_then(|r| opt_int(r, 0)).and_then(|n| u64::try_from(n).ok()).unwrap_or(0))
}

// ------------------------------------------------------------------- stop

/// Whether an operator stop covers `seq`.
pub fn stopped(sql: &impl Sql, seq: i64) -> R<bool> {
    Ok(count(sql, "SELECT COUNT(*) FROM live_stop WHERE stopped = 1 AND scope IN (?, ?)", &[s(GLOBAL), s(seq.to_string())])? > 0)
}

/// The operator's `POST /eval/live/stop` body.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopRequest {
    pub stopped: bool,
    /// The public cohort number; `None` is the global scope.
    pub cohort: Option<u64>,
    pub reason: String,
}

/// Stops or resumes live releases, globally or for one cohort, and journals
/// it with its reason and epoch. No expiry; it holds nothing else: scoring,
/// packaging, signing and phases go on. A resume releases what came due
/// during the stop, once each and in order, at the next tick.
pub fn set_stop(sql: &impl Sql, req: &StopRequest) -> R<Value> {
    let reason = req.reason.trim();
    if reason.is_empty() || reason.len() > REASON_BYTES || reason.chars().any(char::is_control) {
        return Err(EvalError::Plan("reason (1 to 200 bytes, no control characters)"));
    }
    let scope = match req.cohort {
        None => GLOBAL.to_owned(),
        Some(n) => {
            let rows = sql.exec("SELECT seq FROM cohorts WHERE cohort = ?", &[i(i64::try_from(n).map_err(|_| state_err("cohort"))?)])?;
            int(rows.first().ok_or(EvalError::Plan("cohort (unknown)"))?, 0)?.to_string()
        }
    };
    record_stop(sql, &scope, req.stopped, reason)?;
    Ok(json!({"scope": if scope == GLOBAL { Value::Null } else { json!(req.cohort) }, "stopped": req.stopped}))
}

fn record_stop(sql: &impl Sql, scope: &str, stopped: bool, reason: &str) -> R<()> {
    let e = epoch(sql)?;
    sql.exec(
        "INSERT INTO live_stop (scope, stopped, reason, set_epoch) VALUES (?, ?, ?, ?)
         ON CONFLICT (scope) DO UPDATE SET stopped = excluded.stopped, reason = excluded.reason, set_epoch = excluded.set_epoch",
        &[s(scope), i(i64::from(stopped)), s(reason), e.clone()],
    )?;
    sql.exec(
        "INSERT INTO live_stop_log (scope, stopped, reason, set_epoch) VALUES (?, ?, ?, ?)",
        &[s(scope), i(i64::from(stopped)), s(reason), e],
    )?;
    if !stopped {
        // Recheck every cohort at the next tick.
        sql.exec("UPDATE live_cohorts SET checked_preds = -1", &[])?;
    }
    Ok(())
}

// --------------------------------------------------------------- releases

/// Cohorts whose releases a tick should check: set up, between the seal and
/// the close, unfaulted, unpaused, unstopped, with a live value possible.
fn release_cohorts(sql: &impl Sql) -> R<Vec<(i64, i64)>> {
    let rows = sql.exec(
        &format!(
            "SELECT l.seq, l.checked_preds FROM live_cohorts l JOIN cohorts c ON c.seq = l.seq
             WHERE c.phase IN {RUNNING} AND c.fault IS NULL AND (l.fault IS NULL OR l.fault = 'prepare')
               AND (l.segments >= 2 OR l.prefinal = 1)
               AND NOT EXISTS (SELECT 1 FROM live_stop t WHERE t.stopped = 1 AND t.scope IN ('*', CAST(l.seq AS TEXT)))
             ORDER BY l.seq"
        ),
        &[],
    )?;
    rows.iter().map(|r| Ok((int(r, 0)?, int(r, 1)?))).collect()
}

fn preds_count(sql: &impl Sql, seq: i64) -> R<i64> {
    count(sql, "SELECT COUNT(*) FROM preds WHERE seq = ?", &[i(seq)])
}

/// Whether a tick has live work: a setup to run, or a cohort whose
/// predictions changed since its last full release check. Cheap: counts.
pub fn due(sql: &impl Sql) -> R<bool> {
    if !pending_setup(sql)?.is_empty() {
        return Ok(true);
    }
    for (seq, checked) in release_cohorts(sql)? {
        if preds_count(sql, seq)? != checked {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The cohorts with possible releases, for the tick ([`release_due`] each).
pub fn due_cohorts(sql: &impl Sql) -> R<Vec<i64>> {
    let mut out = Vec::new();
    for (seq, checked) in release_cohorts(sql)? {
        if preds_count(sql, seq)? != checked {
            out.push(seq);
        }
    }
    Ok(out)
}

/// One cohort's live state as the setup stored it.
#[derive(Clone, Debug)]
struct LiveRow {
    segments: u8,
    eps_live_milli: u32,
    prefinal: bool,
    executed: u16,
    predecessor: bool,
    n: u32,
    fault: Option<String>,
    /// The `quant-v1` grid and the margin line, once `R_k`'s value is out.
    quant_width: Option<String>,
    margin_line: Option<String>,
}

fn live_row(sql: &impl Sql, seq: i64) -> R<Option<LiveRow>> {
    let rows = sql.exec(
        "SELECT segments, eps_live_milli, prefinal, executed, predecessor, n, fault, quant_width, margin_line
         FROM live_cohorts WHERE seq = ?",
        &[i(seq)],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    let bad = || state_err("live row");
    Ok(Some(LiveRow {
        segments: u8::try_from(int(r, 0)?).map_err(|_| bad())?,
        eps_live_milli: u32::try_from(int(r, 1)?).map_err(|_| bad())?,
        prefinal: int(r, 2)? != 0,
        executed: u16::try_from(int(r, 3)?).map_err(|_| bad())?,
        predecessor: int(r, 4)? != 0,
        n: u32::try_from(int(r, 5)?).map_err(|_| bad())?,
        fault: opt_text(r, 6),
        quant_width: opt_text(r, 7),
        margin_line: opt_text(r, 8),
    }))
}

/// A job of the cohort, as the releases read it.
#[derive(Clone, Debug)]
struct Job {
    job: i64,
    role: String,
    tasks: i64,
}

fn jobs(sql: &impl Sql, seq: i64) -> R<Vec<Job>> {
    sql.exec("SELECT job, role, COALESCE(tasks, 0) FROM jobs WHERE seq = ? ORDER BY job", &[i(seq)])?
        .iter()
        .map(|r| Ok(Job { job: int(r, 0)?, role: text(r, 1)?.to_owned(), tasks: int(r, 2)? }))
        .collect()
}

/// One resolved batch task: its utility (a model error scores 0) and
/// whether it is an attributable failure, as the job summary reads it
/// (`engine::summarize_job`: a prediction is outcome 1 with both values).
fn utility(kind: &str, outcome: i64, inv: Option<i64>, share: Option<i64>) -> R<(u64, bool)> {
    match (outcome, inv, share) {
        (1, Some(a), Some(_)) => {
            let p = Probability::new(u32::try_from(a).map_err(|_| state_err("probability"))?)
                .map_err(|_| state_err("probability"))?;
            Ok((brier_utility(p, kind != "human_only"), false))
        }
        _ => Ok((0, true)),
    }
}

/// Utilities and failures of `job` on the batch tasks of segment `seg`
/// (`Some`), or on the whole batch (`None`); `None` when a task is not yet
/// resolved. Reads only rows with `live_segment` set, or `docset = 'batch'`
/// for the whole batch: never a probe or audit task.
fn batch_outcomes(sql: &impl Sql, seq: i64, job: i64, seg: Option<u8>) -> R<Option<(Vec<u64>, u32)>> {
    let (filter, params): (&str, Vec<SqlValue>) = match seg {
        Some(sg) => ("d.live_segment = ?", vec![i(job), i(seq), i(i64::from(sg))]),
        None => ("d.docset = 'batch' AND d.task IS NOT NULL", vec![i(job), i(seq)]),
    };
    let rows = sql.exec(
        &format!(
            "SELECT d.kind, p.outcome, p.inv_ppb, p.share_ppb FROM docs d
             LEFT JOIN preds p ON p.seq = d.seq AND p.task = d.task AND p.job = ?
             WHERE d.seq = ? AND {filter} ORDER BY d.task"
        ),
        &params,
    )?;
    let mut u = Vec::with_capacity(rows.len());
    let mut failed = 0u32;
    for r in &rows {
        let Some(outcome) = opt_int(r, 1) else { return Ok(None) };
        let (v, f) = utility(text(r, 0)?, outcome, opt_int(r, 2), opt_int(r, 3))?;
        u.push(v);
        failed += u32::from(f);
    }
    if u.is_empty() {
        return Err(state_err("empty segment"));
    }
    Ok(Some((u, failed)))
}

fn stored(sql: &impl Sql, seq: i64, job: i64, kind: &str) -> R<Vec<(i64, String, Option<String>)>> {
    sql.exec(
        "SELECT checkpoint, outcome, value_units FROM live_releases WHERE seq = ? AND job = ? AND kind = ? AND revision = 0
         ORDER BY checkpoint",
        &[i(seq), i(job), s(kind)],
    )?
    .iter()
    .map(|r| Ok((int(r, 0)?, text(r, 1)?.to_owned(), opt_text(r, 2))))
    .collect()
}

fn insert(sql: &impl Sql, seq: i64, job: i64, kind: &str, cp: i64, value: Option<u64>, outcome: &str) -> R<()> {
    let e = epoch(sql)?;
    sql.exec(
        "INSERT INTO live_releases (seq, job, kind, checkpoint, value_units, outcome, revision, released_epoch)
         VALUES (?, ?, ?, ?, ?, ?, 0, ?)",
        &[i(seq), i(job), s(kind), i(cp), value.map_or(SqlValue::Null, |v| s(v.to_string())), s(outcome), e],
    )?;
    Ok(())
}

/// Content digests by job: `R_k`, `R_k-1` and each executed entrant.
fn subject_of(st: &v3::V3State, c: &Cohort, job: &Job) -> R<Subject> {
    let first_entrant = 1 + i64::from(c.plan.previous_reference.is_some());
    match job.role.as_str() {
        "reference" => Ok(Subject::Current(st.reference.as_ref().ok_or(state_err("reference digests"))?.content.0)),
        "previous_reference" => {
            Ok(Subject::Predecessor(st.predecessor.as_ref().ok_or(state_err("predecessor digests"))?.content.0))
        }
        "entrant" => {
            let pos = usize::try_from(job.job - first_entrant).map_err(|_| state_err("entrant job"))?;
            Ok(Subject::Entrant(st.entrants.get(pos).ok_or(state_err("entrant digests"))?.content.0))
        }
        _ => Err(state_err("job role")),
    }
}

fn binding(c: &Cohort) -> R<CohortBinding> {
    let hex = c.manifest_digest.as_deref().ok_or(state_err("manifest digest"))?;
    Ok(CohortBinding(crate::eval::engine::parse_digest(hex).ok_or(state_err("manifest digest"))?))
}

/// Releases at most one due value of one subject of cohort `seq`; `true`
/// when it released one (call again), `false` when nothing is due, which
/// also records the prediction count checked. Readings first, in job order,
/// then pre-finalized values.
pub fn release_due(sql: &impl Sql, cfg: &EvalConfig, seq: i64) -> R<bool> {
    let c = cohort(sql, seq)?;
    if !matches!(c.phase.as_str(), "scoring" | "packaging" | "transition") || c.fault.is_some() || stopped(sql, seq)? {
        return Ok(false);
    }
    let Some(lr) = live_row(sql, seq)? else { return Ok(false) };
    if stops_all(lr.fault.as_deref()) {
        return Ok(false);
    }
    let preds = preds_count(sql, seq)?;
    let js = jobs(sql, seq)?;
    let reference = js.iter().find(|j| j.role == "reference").ok_or(state_err("reference job"))?.clone();
    let st = v3::state(sql, seq)?;
    let seed = cohort_seed(&cfg.private_seed, &c.label);
    let k_n = c.plan.reward_v1.as_ref().and_then(|v| v.quant.as_ref()).map(|q| q.min_success_max_failed).ok_or(state_err("quant"))?;
    // Readings: each executed entrant and R_k, checkpoints 1..L-1 in order.
    if lr.segments >= live::MIN_SEGMENTS {
        for job in js.iter().filter(|j| j.role == "entrant" || j.role == "reference") {
            let done = stored(sql, seq, job.job, "reading")?;
            let cp = done.len() as i64 + 1;
            if cp >= i64::from(lr.segments) {
                continue;
            }
            let cp8 = u8::try_from(cp).map_err(|_| state_err("checkpoint"))?;
            // Every batch task of segments 1..=cp resolved for this job.
            let open = count(
                sql,
                "SELECT COUNT(*) FROM docs d WHERE d.seq = ? AND d.live_segment BETWEEN 1 AND ?
                   AND NOT EXISTS (SELECT 1 FROM preds p WHERE p.seq = d.seq AND p.job = ? AND p.task = d.task)",
                &[i(seq), i(cp), i(job.job)],
            )?;
            if open > 0 {
                continue;
            }
            // Attributable failures in segments 1..=cp.
            let mut failed = 0u32;
            let mut score = None;
            for sg in 1..=cp8 {
                let (u, f) = batch_outcomes(sql, seq, job.job, Some(sg))?.ok_or(state_err("segment pending"))?;
                failed += f;
                if sg == cp8 {
                    score = Some(live::segment_score(u).map_err(|_| state_err("segment score"))?);
                }
            }
            let score = score.ok_or(state_err("segment score"))?;
            let is_ref = job.role == "reference";
            let earlier_stop = done.iter().any(|(_, o, _)| o != "value");
            if is_ref && (failed > 0 || earlier_stop) {
                insert(sql, seq, job.job, "reading", cp, None, "none")?;
            } else if !is_ref && (failed > k_n || earlier_stop) {
                insert(sql, seq, job.job, "reading", cp, None, "withheld")?;
            } else {
                let subject = subject_of(&st, &c, job)?;
                let v = live::release(&seed, &binding(&c)?, &subject, cp8, 0, lr.n, lr.segments, lr.executed, lr.eps_live_milli, score)
                    .map_err(|_| state_err("live draw"))?;
                insert(sql, seq, job.job, "reading", cp, Some(v), "value")?;
            }
            return Ok(true);
        }
    }
    if lr.prefinal && release_prefinal(sql, &c, &st, &seed, &js, &reference, &lr, k_n)? {
        return Ok(true);
    }
    sql.exec("UPDATE live_cohorts SET checked_preds = ? WHERE seq = ?", &[i(preds), i(seq)])?;
    Ok(false)
}

fn job_done(sql: &impl Sql, seq: i64, job: &Job) -> R<bool> {
    Ok(job.tasks > 0 && count(sql, "SELECT COUNT(*) FROM preds WHERE seq = ? AND job = ?", &[i(seq), i(job.job)])? == job.tasks)
}

/// The pre-finalized values: `R_k`'s once it finished the batch without a
/// failure (with the grid width and the margin line), then each entrant's
/// once its whole job is done, then `R_k-1`'s once it and `R_k` finished
/// without a failure. One value per call.
#[allow(clippy::too_many_arguments)]
fn release_prefinal(
    sql: &impl Sql,
    c: &Cohort,
    st: &v3::V3State,
    seed: &[u8; 32],
    js: &[Job],
    reference: &Job,
    lr: &LiveRow,
    k_n: u32,
) -> R<bool> {
    let seq = c.seq;
    let have = |job: i64| -> R<bool> { Ok(!stored(sql, seq, job, "prefinal")?.is_empty()) };
    if !job_done(sql, seq, reference)? {
        return Ok(false);
    }
    let Some((ref_u, ref_failed)) = batch_outcomes(sql, seq, reference.job, None)? else { return Ok(false) };
    if ref_failed > 0 {
        // The package will wait for a signed correction: nothing is shown.
        return Ok(false);
    }
    let v1 = c.plan.reward_v1.as_ref().ok_or(state_err("reward_v1"))?;
    let q = v1.quant_params().map_err(|e| EvalError::Plan(e.0))?;
    let charged = quant::charged(lr.executed, lr.predecessor);
    let tau = quant::Tau::new(lr.n, charged, q.eps_total_milli).map_err(|_| state_err("tau"))?;
    let se = quant::SeMeasure::measure(ref_u.iter().copied()).map_err(|_| state_err("se"))?;
    let w = quant::width(se.se, &tau).map_err(|_| state_err("width"))?;
    let cohort = binding(c)?;
    let score_of = |u: &[u64]| -> R<Score> {
        let sum: u128 = u.iter().map(|x| u128::from(*x)).sum();
        Score::new((sum / u128::from(lr.n)) as u64).map_err(|_| state_err("score"))
    };
    let preview = |job: &Job, u: &[u64], failed: Option<(u32, u32)>| -> R<reward_v1::Preview> {
        reward_v1::preview_q(&PreviewInputs {
            seed,
            cohort,
            subject: subject_of(st, c, job)?,
            score: score_of(u)?,
            failed,
            w,
            tau: &tau,
        })
        .map_err(|_| state_err("preview"))
    };
    if !have(reference.job)? {
        if ref_u.len() != lr.n as usize {
            return Err(state_err("reference batch"));
        }
        let p = preview(reference, &ref_u, None)?;
        let params = crate::eval::v3::manifest_again(sql, c)?.params;
        let margin = reward_v1::margin_line(&params, p.q, w).map_err(|_| state_err("margin line"))?;
        sql.exec(
            "UPDATE live_cohorts SET quant_width = ?, margin_line = ? WHERE seq = ?",
            &[s(w.to_string()), margin.map_or(SqlValue::Null, |m| s(m.to_string())), i(seq)],
        )?;
        insert(sql, seq, reference.job, "prefinal", 0, Some(p.q), "value")?;
        return Ok(true);
    }
    for job in js.iter().filter(|j| j.role == "entrant") {
        if have(job.job)? || !job_done(sql, seq, job)? {
            continue;
        }
        let (u, failed) = batch_outcomes(sql, seq, job.job, None)?.ok_or(state_err("entrant batch"))?;
        if u.len() != lr.n as usize {
            return Err(state_err("entrant batch"));
        }
        let p = preview(job, &u, Some((failed, k_n)))?;
        match p.value() {
            reward_v1::PreviewValue::Q(v) => insert(sql, seq, job.job, "prefinal", 0, Some(v), "value")?,
            reward_v1::PreviewValue::Withheld => insert(sql, seq, job.job, "prefinal", 0, None, "withheld")?,
        }
        return Ok(true);
    }
    if let Some(pred) = js.iter().find(|j| j.role == "previous_reference")
        && !have(pred.job)?
        && job_done(sql, seq, pred)?
        && let Some((u, 0)) = batch_outcomes(sql, seq, pred.job, None)?
    {
        let p = preview(pred, &u, None)?;
        insert(sql, seq, pred.job, "prefinal", 0, Some(p.q), "value")?;
        return Ok(true);
    }
    Ok(false)
}

// ---------------------------------------------------------------- report

fn grid(value: u64, w: u64) -> R<String> {
    let mut buf = [0u8; quant::MAX_DECIMAL_LEN];
    Ok(quant::format_grid(value, w, &mut buf).map_err(|_| state_err("grid value"))?.to_owned())
}

fn shown(outcome: &str, value: Option<&str>, w: u64) -> R<Value> {
    Ok(match outcome {
        "value" => {
            let v: u64 = value.and_then(|v| v.parse().ok()).ok_or(state_err("stored value"))?;
            json!(grid(v, w)?)
        }
        "withheld" => json!(live::WITHHELD),
        "none" => json!(live::NONE),
        _ => return Err(state_err("stored outcome")),
    })
}

/// The 64 lowercase hex characters of a job owner's hotkey.
fn owner_hotkey(owner: &str) -> Option<String> {
    let v: Value = serde_json::from_str(owner).ok()?;
    let hex = v.get("hotkey")?.as_str()?.strip_prefix("0x")?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then(|| hex.to_owned())
}

fn empty_report() -> Value {
    json!({"rule": REPORT_RULE, "cohort": null, "sealed": false, "policy": null, "reference": null,
           "prefinal": null, "rows": [], "fault": null, "defects": []})
}

/// `GET /eval/live`: the running cohort's live values, from the tables only
/// (the newest cohort past admission that is neither closed nor abandoned,
/// probe builds left out). The whole report is refused (`refused` with a
/// reason) on any invalid stored value, as the progress report is.
pub fn report(sql: &impl Sql) -> R<Value> {
    let rows = sql.exec(
        "SELECT seq, cohort FROM cohorts WHERE phase IN ('generating','sealing','scoring','packaging','transition')
           AND json_extract(plan, '$.reward_v1.probe_version') IS NULL
         ORDER BY seq DESC LIMIT 1",
        &[],
    )?;
    let Some(r) = rows.first() else { return Ok(empty_report()) };
    let (seq, number) = (int(r, 0)?, int(r, 1)?);
    match cohort_report(sql, seq, number) {
        Ok(v) => Ok(v),
        Err(EvalError::Store(e)) => Err(EvalError::Store(e)),
        Err(e) => Ok(json!({"rule": REPORT_RULE, "cohort": number, "sealed": null, "policy": null, "reference": null,
                            "prefinal": null, "rows": null, "fault": null, "defects": [], "refused": e.code()})),
    }
}

fn cohort_report(sql: &impl Sql, seq: i64, number: i64) -> R<Value> {
    let c = cohort(sql, seq)?;
    let sealed = count(sql, "SELECT COUNT(*) FROM jobs WHERE seq = ?", &[i(seq)])? > 0;
    let mut out = empty_report();
    out["cohort"] = json!(number);
    out["sealed"] = json!(sealed);
    if live_plan(&c).is_none() {
        return Ok(out);
    }
    let Some(lr) = live_row(sql, seq)? else {
        // Sealed with its setup still to run, or not sealed: nothing yet.
        return Ok(out);
    };
    let stop = stopped(sql, seq)?;
    let closed = !matches!(c.phase.as_str(), "scoring" | "packaging" | "transition");
    let silent = stop || closed || stops_all(lr.fault.as_deref());
    let width = (lr.segments >= live::MIN_SEGMENTS)
        .then(|| live::width_live(lr.n, lr.segments, lr.executed, lr.eps_live_milli))
        .transpose()
        .map_err(|_| state_err("live width"))?;
    let noise = (lr.segments >= live::MIN_SEGMENTS)
        .then(|| live::noise_sd_milli(lr.n, lr.segments, lr.executed, lr.eps_live_milli))
        .transpose()
        .map_err(|_| state_err("noise sd"))?;
    let stored_bits = sql.exec("SELECT counted, live_bits, bound FROM live_cohorts WHERE seq = ?", &[i(seq)])?;
    let b = stored_bits.first().ok_or(state_err("live row"))?;
    let (counted, live_bits, bound) = (int(b, 0)?, int(b, 1)?, int(b, 2)?);
    if counted + live_bits > bound && lr.segments >= live::MIN_SEGMENTS {
        return Err(state_err("bits over the bound"));
    }
    out["policy"] = json!({
        "rule": live::LIVE_RULE, "n": lr.n, "predecessor": lr.predecessor, "eps_live_milli": lr.eps_live_milli,
        "segments": lr.segments, "released_max": lr.segments.saturating_sub(1),
        "width": width.map(|w| grid(w, w)).transpose()?, "noise_sd": noise.map(live::format_milli),
        "stopped": stop, "prefinal": lr.prefinal,
        "bits": {"counted": counted, "live": live_bits, "bound": bound},
    });
    out["fault"] = json!(lr.fault);
    let js = jobs(sql, seq)?;
    let reference = js.iter().find(|j| j.role == "reference");
    let checkpoints = |job: i64| -> R<Vec<Value>> {
        let Some(w) = width.filter(|_| !silent) else { return Ok(Vec::new()) };
        stored(sql, seq, job, "reading")?.iter().map(|(_, o, v)| shown(o, v.as_deref(), w)).collect()
    };
    if let Some(rj) = reference {
        // R_k's counts in segment steps: the batch tasks of its completed
        // leading segments (0 or n without segments).
        let mut done = 0i64;
        let segs = if lr.segments >= live::MIN_SEGMENTS { lr.segments } else { 0 };
        if segs == 0 {
            if job_done(sql, seq, rj)? {
                done = i64::from(lr.n);
            }
        } else {
            for sg in 1..=segs {
                let open = count(
                    sql,
                    "SELECT COUNT(*) FROM docs d WHERE d.seq = ? AND d.live_segment = ?
                       AND NOT EXISTS (SELECT 1 FROM preds p WHERE p.seq = d.seq AND p.job = ? AND p.task = d.task)",
                    &[i(seq), i(i64::from(sg)), i(rj.job)],
                )?;
                if open > 0 {
                    break;
                }
                let (a, b) = live::segment_bounds(lr.n, segs, sg - 1).map_err(|_| state_err("segment"))?;
                done += i64::from(b - a);
            }
        }
        let cps = stored(sql, seq, rj.job, "reading")?;
        let failed_at_end = job_done(sql, seq, rj)? && batch_outcomes(sql, seq, rj.job, None)?.is_some_and(|(_, f)| f > 0);
        out["reference"] = json!({
            "done": done, "total": lr.n, "checkpoints": checkpoints(rj.job)?,
            "stopped": cps.iter().any(|(_, o, _)| o == "none") || failed_at_end,
        });
    }
    // The pre-finalized block: R_k's value with the grid, the margin line and
    // R_k-1's value once released.
    let qw: Option<u64> = lr.quant_width.as_deref().map(str::parse).transpose().map_err(|_| state_err("quant width"))?;
    let prefinal_of = |job: i64| -> R<Value> {
        let Some(w) = qw.filter(|_| !silent) else { return Ok(Value::Null) };
        match stored(sql, seq, job, "prefinal")?.first() {
            Some((_, o, v)) => shown(o, v.as_deref(), w),
            None => Ok(Value::Null),
        }
    };
    if let (Some(rj), Some(w), false) = (reference, qw, silent) {
        let current = prefinal_of(rj.job)?;
        if !current.is_null() {
            let pred = js.iter().find(|j| j.role == "previous_reference").map(|j| prefinal_of(j.job)).transpose()?;
            let margin: Option<u64> = lr.margin_line.as_deref().map(str::parse).transpose().map_err(|_| state_err("margin"))?;
            let cur: u64 = quant::parse_grid(current.as_str().ok_or(state_err("current"))?, w).map_err(|_| state_err("current"))?;
            if margin.is_some_and(|m| m < cur) {
                return Err(state_err("margin line below the reference"));
            }
            out["prefinal"] = json!({
                "quantization": {"rule": quant::QUANT_RULE, "width": grid(w, w)?},
                "current": current, "predecessor": pred.unwrap_or(Value::Null),
                "margin_line": margin.map(|m| grid(m, w)).transpose()?,
            });
        }
    }
    // One row per entrant job, in hotkey order.
    let entrants = sql.exec("SELECT job, owner FROM jobs WHERE seq = ? AND role = 'entrant'", &[i(seq)])?;
    if entrants.len() > ROWS {
        return Err(state_err("over_bound"));
    }
    let mut rows: Vec<(String, Value)> = Vec::with_capacity(entrants.len());
    for r in &entrants {
        let job = int(r, 0)?;
        let hotkey = owner_hotkey(text(r, 1)?).ok_or(state_err("invalid_owner"))?;
        let stopped = stored(sql, seq, job, "reading")?.iter().any(|(_, o, _)| o == "withheld");
        rows.push((hotkey.clone(), json!({"hotkey": hotkey, "checkpoints": checkpoints(job)?, "stopped": stopped, "prefinal": prefinal_of(job)?})));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    if rows.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err(state_err("repeated_hotkey"));
    }
    out["rows"] = json!(rows.into_iter().map(|(_, v)| v).collect::<Vec<_>>());
    let defects = sql.exec(
        "SELECT d.rule, j.owner FROM live_defects d JOIN jobs j ON j.seq = d.seq AND j.job = d.job WHERE d.seq = ?
         ORDER BY d.job, d.rule",
        &[i(seq)],
    )?;
    out["defects"] = json!(defects
        .iter()
        .map(|r| Ok(json!({"rule": text(r, 0)?, "hotkey": owner_hotkey(text(r, 1)?)})))
        .collect::<R<Vec<_>>>()?);
    Ok(out)
}

// -------------------------------------------------------- package check

/// A pre-finalized value the package does not carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Defect {
    pub job: i64,
    pub shown: String,
    pub signed: String,
}

/// Compares every stored pre-finalized value of `seq` with the package's:
/// `(job, Some(q))` for a released value, `(job, None)` for withheld.
/// Returns the mismatches; reads only.
pub fn cross_check(sql: &impl Sql, seq: i64, package: &[(i64, Option<u64>)]) -> R<Vec<Defect>> {
    let mut out = Vec::new();
    for (job, signed) in package {
        for (_, outcome, value) in stored(sql, seq, *job, "prefinal")? {
            let shown_v = match outcome.as_str() {
                "value" => value.as_deref().and_then(|v| v.parse::<u64>().ok()),
                _ => None,
            };
            if shown_v != *signed {
                let txt = |v: Option<u64>| v.map_or_else(|| live::WITHHELD.to_owned(), |v| v.to_string());
                out.push(Defect { job: *job, shown: txt(shown_v), signed: txt(*signed) });
            }
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
/// Records the package's defects in the package's own transaction: each a
/// `prefinal_mismatch` row, counted as one more `quant-v1` symbol in the
/// cohort's private terms (never in the signed `BudgetRecord`); a private
/// total over `n h / F` records `over_bound` and sets the global stop until
/// the operator resumes. Never refuses: the package is signed regardless.
pub fn record_defects(sql: &impl Sql, seq: i64, defects: &[Defect], recount: u32, w: u64, n: u32, h: u16, f: u16) -> R<()> {
    if defects.is_empty() || count(sql, "SELECT COUNT(*) FROM live_cohorts WHERE seq = ?", &[i(seq)])? == 0 {
        return Ok(());
    }
    for d in defects {
        sql.exec(
            "INSERT INTO live_defects (seq, job, rule, shown, signed) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (seq, job, rule) DO NOTHING",
            &[i(seq), i(d.job), s(PREFINAL_MISMATCH), s(&d.shown), s(&d.signed)],
        )?;
    }
    let extra = u32::try_from(defects.len()).map_err(|_| state_err("defects"))?
        * quant::ceil_log2(quant::grid_values(w).map_err(|_| state_err("grid"))? + 1);
    let live_bits = count(sql, "SELECT live_bits FROM live_cohorts WHERE seq = ?", &[i(seq)])?;
    let total = u32::try_from(live_bits).map_err(|_| state_err("bits"))?.saturating_add(recount).saturating_add(extra);
    let over = !quant::within_bound(total, n, h, f);
    let terms = json!({"recount": recount, "live": live_bits, "extra_releases": defects.len(), "extra_bits": extra,
                       "total": total, "over_bound": over});
    sql.exec("UPDATE live_cohorts SET terms = ? WHERE seq = ?", &[s(terms.to_string()), i(seq)])?;
    if over {
        sql.exec("UPDATE live_cohorts SET fault = COALESCE(fault, ?) WHERE seq = ?", &[s(FAULT_OVER_BOUND), i(seq)])?;
        record_stop(sql, GLOBAL, true, FAULT_OVER_BOUND)?;
    }
    Ok(())
}

/// The value a subject's package entry carries, for [`cross_check`].
pub fn signed_value(q: Option<Score>, withheld: bool) -> Option<u64> {
    if withheld { None } else { q.map(|v| v.get()) }
}
