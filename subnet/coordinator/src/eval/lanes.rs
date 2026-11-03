//! Fan-out lanes, on the cohort object's side (storage-neutral, one
//! transaction per call).
//!
//! Every bulk step is a fan-out job of lanes. A lane is a Durable Object whose
//! alarm runs the lane loop, so each lane has its own invocation limits;
//! scoring lanes are the container-owning `AScorer` objects, each driving its
//! own Container instance, and generation, summary and bundle lanes are
//! `EvalLane` objects. The cohort object stays the only authority:
//!
//! - **Planning.** Generation and scoring lanes are planned when generation
//!   starts (scoring lanes early, for the just-in-time pre-start), summary and
//!   bundle lanes when scoring ends. Each row records the lane's assignment.
//! - **Dispatch.** A sweep (every cohort alarm, about every 10 s while lanes
//!   live and heartbeats are fresh) gives each due lane `gen + 1`, records it,
//!   and only then returns the content-free kick to send. A lane is due when it
//!   is pending within the live cap, kicked but unseen for [`KICK_UNSEEN_MS`],
//!   silent past its staleness bound with work left, paused while heartbeats
//!   are fresh again, or done while its job has claimable work and no live lane.
//!   Lanes report at alarm entry and every 20 s of a cold path, so a slow start
//!   is never mistaken for a lost kick (the re-kick livelock of the review).
//! - **Claims** use the existing lease tables. A lane prefers its own chunk
//!   range, then steals any unclaimed chunk of its job. An expired claim whose
//!   attempt may have stored its objects is first handed out for
//!   reconciliation (no compute); only a failed reconciliation records it lost.
//!   A claim is fenced by its token: a commit lands only from the holder of the
//!   chunk's latest claim. An identical repeated commit answers `already`.
//! - **Receipts.** The lane builds and stores the receipt object; the commit
//!   rebuilds it from the committed outcomes, the claim and the cohort seed and
//!   refuses a mismatch, so a lane bug can waste an attempt but never record a
//!   receipt that disagrees with the stored predictions.
//! - **Crash isolation.** A request that crashed its container (a transport
//!   failure, timeout or exit after dispatch) gets infrastructure-error
//!   receipts, and the chunk's next claim runs in isolation: one document per
//!   request on a freshly started instance, restarted after every crash. A
//!   document that crashes alone, on an instance that then scores a control
//!   document, is a model error for that job's document only (utility 0 there);
//!   every other job and the cohort proceed.
//! - **Job-scoped faults.** A job whose chunk spends every attempt unresolved is
//!   faulted alone; the other lanes finish, and the cohort pauses at the end of
//!   scoring (`unresolved_infrastructure`), since packaging needs every job.
//! - **Silence.** No claim is granted unless a heartbeat arrived within
//!   `HEARTBEAT_FRESH_MS` (and the load instance's hold flag is off). Work
//!   already dispatched may finish and persist its evidence; lanes then stop
//!   their containers and do not re-arm, and the next heartbeat's sweep
//!   re-kicks them on the same cohort.

use crate::eval::cost;
use crate::eval::engine::{
    self, CallTiming, Cohort, EvalConfig, EvalError, GenItem, GenOutcome, HEARTBEAT_FRESH_MS, MixedSpec, R,
    ScratchSpec, Work, cohort, cohort_seed, fault, i, item_digest, opt_int, opt_text, parse_digest, s, set_phase,
};
use crate::eval::receipts::{self, ChunkFacts, TaskOutcome};
use crate::eval::textgen;
use crate::sql::{Row, Sql, SqlValue, bind_u64, int, text, uint};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const KICK_VERSION: u8 = 1;
/// A kicked lane that has not reported within this long is kicked again.
pub const KICK_UNSEEN_MS: u64 = 30_000;
/// Summary and bundle lanes silent this long are kicked again.
pub const PACK_STALE_MS: u64 = 120_000;
/// A lane waits for another lane's claim only if it expires within this long.
pub const WAIT_HORIZON_MS: u64 = 20_000;
/// Backoff of a chunk whose request never reached the model.
pub const NOT_DISPATCHED_BACKOFF_MS: u64 = 2_000;

pub const KINDS: [&str; 4] = ["score", "gen", "summary", "bundle"];
pub(crate) const LIVE: &str = "('kicked','starting','running','waiting')";

/// A content-free wakeup: identifiers and the lane's generation only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneKick {
    pub v: u8,
    pub instance: String,
    pub seq: i64,
    pub kind: String,
    pub lane: i64,
    #[serde(rename = "gen")]
    pub generation: i64,
    pub name: String,
    /// The named main authority that issued the kick (`config::Authority`):
    /// the lane's calls go back to that authority's object whatever the
    /// deployment names now. Absent for the original authority and the load
    /// instance, so their kicks are the bytes they were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
}

/// Who a lane request comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneRef {
    pub seq: i64,
    pub kind: String,
    pub lane: i64,
    #[serde(rename = "gen")]
    pub generation: i64,
}

/// The lane's Durable Object name. Names are stable across cohorts, so the
/// namespace stays bounded by the lane cap per instance and authority; a lane
/// orders its assignments by `(seq, gen)`. `scope` is the instance, with a
/// named authority's suffix (`EvalConfig::lane_scope`): a restarted
/// authority numbers its cohorts from its own first sequence, so it never
/// kicks a lane object that holds a later `(seq, gen)` of an earlier one.
pub fn lane_name(scope: &str, kind: &str, lane: i64) -> String {
    let net = crate::config::NETWORK;
    match kind {
        "score" => format!("a-scorer/{net}/{scope}/lane-{lane:04}"),
        "gen" => format!("eval-lane/{net}/{scope}/gen-{lane:03}"),
        other => format!("eval-lane/{net}/{scope}/{other}-{lane:04}"),
    }
}

#[derive(Clone, Debug)]
struct LaneRow {
    lane: i64,
    name: String,
    job: Option<i64>,
    generation: i64,
    status: String,
    kicked_ms: u64,
    seen_ms: u64,
    kicks: i64,
    prestart: bool,
}

const LANE_COLUMNS: &str = "lane, name, job, gen, status, kicked_ms, seen_ms, kicks, prestart";

fn lane_from(r: &Row) -> R<LaneRow> {
    Ok(LaneRow {
        lane: int(r, 0)?,
        name: text(r, 1)?.to_owned(),
        job: opt_int(r, 2),
        generation: int(r, 3)?,
        status: text(r, 4)?.to_owned(),
        kicked_ms: uint(r, 5)?,
        seen_ms: uint(r, 6)?,
        kicks: int(r, 7)?,
        prestart: int(r, 8)? != 0,
    })
}

fn lane_rows(sql: &impl Sql, seq: i64, kind: &str) -> R<Vec<LaneRow>> {
    sql.exec(
        &format!("SELECT {LANE_COLUMNS} FROM lanes WHERE seq = ? AND kind = ? ORDER BY lane"),
        &[i(seq), s(kind)],
    )?
    .iter()
    .map(lane_from)
    .collect()
}

/// The lane's row if `r` is its current generation; `None` means fenced.
fn fence(sql: &impl Sql, r: &LaneRef) -> R<Option<LaneRow>> {
    let rows = sql.exec(
        &format!("SELECT {LANE_COLUMNS} FROM lanes WHERE seq = ? AND kind = ? AND lane = ?"),
        &[i(r.seq), s(&r.kind), i(r.lane)],
    )?;
    match rows.first() {
        Some(row) => {
            let l = lane_from(row)?;
            Ok((l.generation == r.generation).then_some(l))
        }
        None => Ok(None),
    }
}

fn touch(sql: &impl Sql, r: &LaneRef, now: u64) -> R<()> {
    sql.exec(
        "UPDATE lanes SET seen_ms = ?, first_seen_ms = CASE WHEN first_seen_ms = 0 THEN ? ELSE first_seen_ms END
         WHERE seq = ? AND kind = ? AND lane = ?",
        &[bind_u64(now)?, bind_u64(now)?, i(r.seq), s(&r.kind), i(r.lane)],
    )?;
    Ok(())
}

fn event(sql: &impl Sql, seq: i64, kind: &str, lane: i64, ev: &str, ms: u64, now: u64) -> R<()> {
    sql.exec(
        "INSERT INTO lane_events (seq, kind, lane, event, ms, at_ms) VALUES (?, ?, ?, ?, ?, ?)",
        &[i(seq), s(kind), i(lane), s(ev), bind_u64(ms)?, bind_u64(now)?],
    )?;
    Ok(())
}

/// Heartbeats are fresh and no operator hold is set: claims may be granted.
pub fn live(sql: &impl Sql, now: u64) -> R<bool> {
    let o = engine::observed(sql)?;
    let hold = int(&sql.exec("SELECT hold FROM eval_obs WHERE id = 1", &[])?[0], 0)?;
    Ok(hold == 0 && o.kick_ms > 0 && now.saturating_sub(o.kick_ms) < HEARTBEAT_FRESH_MS)
}

/// Load instance: an operator hold that every claim treats as silence.
pub fn set_hold(sql: &impl Sql, hold: bool) -> R<()> {
    sql.exec("UPDATE eval_obs SET hold = ? WHERE id = 1", &[i(i64::from(hold))])?;
    Ok(())
}

fn fault_job(sql: &impl Sql, seq: i64, job: i64, code: &str, now: u64) -> R<()> {
    let changed = sql.exec(
        "UPDATE jobs SET status = 'faulted', fault = ? WHERE seq = ? AND job = ? AND status NOT IN ('done','faulted')
         RETURNING job",
        &[s(code), i(seq), i(job)],
    )?;
    if !changed.is_empty() {
        let o = engine::observed(sql)?;
        sql.exec(
            "INSERT INTO phase_log (seq, phase, at_ms, confirmed_number, epoch_index, note)
             SELECT seq, phase, ?, ?, ?, ? FROM cohorts WHERE seq = ?",
            &[
                bind_u64(now)?,
                o.confirmed.map_or(SqlValue::Null, |c| i(c.number as i64)),
                o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)),
                s(format!("job {job} fault:{code}; other jobs continue")),
                i(seq),
            ],
        )?;
    }
    Ok(())
}

// ------------------------------------------------------------------ planning

#[allow(clippy::too_many_arguments)]
fn insert_lane(sql: &impl Sql, cfg: &EvalConfig, seq: i64, kind: &str, lane: i64, job: Option<i64>, index: i64, lo: i64, hi: i64) -> R<()> {
    sql.exec(
        "INSERT INTO lanes (seq, kind, lane, name, job, lane_index, chunk_lo, chunk_hi, status)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'pending')",
        &[
            i(seq),
            s(kind),
            i(lane),
            s(lane_name(&cfg.lane_scope(), kind, lane)),
            job.map_or(SqlValue::Null, i),
            i(index),
            i(lo),
            i(hi),
        ],
    )?;
    Ok(())
}

/// Plans a cohort's scoring lanes for `n` documents (replacing any unkicked plan).
pub fn plan_score_lanes(sql: &impl Sql, cfg: &EvalConfig, c: &Cohort, n: u32) -> R<u32> {
    let f = c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    let jobs = c.plan.jobs() as u32;
    let budget = cost::budget_lanes(jobs, n, f.score.scoring_budget_microusd, &f.cost);
    let cap = f.score.max_lanes.min(cfg.max_score_lanes).min(budget).max(1);
    sql.exec("DELETE FROM lanes WHERE seq = ? AND kind = 'score'", &[i(c.seq)])?;
    let lanes = cost::plan_score_lanes(jobs, n, c.plan.scorer.chunk_docs, f.score.lane_docs, cap);
    for l in &lanes {
        insert_lane(sql, cfg, c.seq, "score", i64::from(l.lane), Some(i64::from(l.job)), i64::from(l.index),
            i64::from(l.chunk_lo), i64::from(l.chunk_hi))?;
    }
    Ok(lanes.len() as u32)
}

/// At generation start: each pending item's next generator, the generation
/// lanes, and the scoring lanes (for the pre-start).
pub fn plan_generation(sql: &impl Sql, cfg: &EvalConfig, seq: i64, now: u64) -> R<()> {
    let c = cohort(sql, seq)?;
    let f = c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    let seed = cohort_seed(&cfg.private_seed, &c.label);
    let pending =
        sql.exec("SELECT idx, key, attempts, kind, spec FROM docs WHERE seq = ? AND status = 'pending'", &[i(seq)])?;
    for r in &pending {
        let avoid = match (text(r, 3)?, opt_text(r, 4)) {
            ("mixed", Some(spec)) => serde_json::from_str::<MixedSpec>(&spec).map(|s| s.avoid).unwrap_or_default(),
            _ => Vec::new(),
        };
        let g = textgen::generator_avoiding(&c.plan.generators, &item_digest(&seed, text(r, 1)?), int(r, 2)? as u32, &avoid);
        sql.exec("UPDATE docs SET generator_next = ? WHERE seq = ? AND idx = ?", &[s(g), i(seq), i(int(r, 0)?)])?;
    }
    let p = pending.len() as u32;
    if p > 0 {
        let g = f.generation.max_lanes.min(p.div_ceil(f.generation.items_per_lane)).max(1);
        for lane in 0..g {
            insert_lane(sql, cfg, seq, "gen", i64::from(lane), None, i64::from(lane), 0, 0)?;
        }
    }
    plan_score_lanes(sql, cfg, &c, c.plan.n)?;
    sql.exec("UPDATE cohorts SET gen_started_ms = ? WHERE seq = ?", &[bind_u64(now)?, i(seq)])?;
    Ok(())
}

/// At a reward-v1 seal: drop the lanes of jobs that do not exist (entrants
/// never executed) and fit each remaining lane's affinity range to its own
/// job's chunk count (references score the batch, entrants every task).
pub fn fit_score_ranges_per_job(sql: &impl Sql, seq: i64) -> R<()> {
    sql.exec(
        "DELETE FROM lanes WHERE seq = ? AND kind = 'score' AND job NOT IN (SELECT job FROM jobs WHERE seq = ?)",
        &[i(seq), i(seq)],
    )?;
    let jobs = sql.exec(
        "SELECT l.job, COUNT(*), j.chunks FROM lanes l JOIN jobs j ON j.seq = l.seq AND j.job = l.job
         WHERE l.seq = ? AND l.kind = 'score' GROUP BY l.job",
        &[i(seq)],
    )?;
    for r in &jobs {
        let (job, w, chunks) = (int(r, 0)?, int(r, 1)? as u32, int(r, 2)? as u32);
        for k in 0..w {
            let (lo, hi) = cost::lane_range(k, w, chunks);
            sql.exec(
                "UPDATE lanes SET chunk_lo = ?, chunk_hi = ? WHERE seq = ? AND kind = 'score' AND job = ? AND lane_index = ?",
                &[i(i64::from(lo)), i(i64::from(hi)), i(seq), i(job), i(i64::from(k))],
            )?;
        }
    }
    Ok(())
}

/// At seal: fit every scoring lane's affinity range to the sealed chunk count.
pub fn fit_score_ranges(sql: &impl Sql, seq: i64, chunks: u32) -> R<()> {
    let jobs = sql.exec("SELECT job, COUNT(*) FROM lanes WHERE seq = ? AND kind = 'score' GROUP BY job", &[i(seq)])?;
    for j in &jobs {
        let (job, w) = (int(j, 0)?, int(j, 1)? as u32);
        for k in 0..w {
            let (lo, hi) = cost::lane_range(k, w, chunks);
            sql.exec(
                "UPDATE lanes SET chunk_lo = ?, chunk_hi = ? WHERE seq = ? AND kind = 'score' AND job = ? AND lane_index = ?",
                &[i(i64::from(lo)), i(i64::from(hi)), i(seq), i(job), i(i64::from(k))],
            )?;
        }
    }
    Ok(())
}

fn plan_pack_lanes(sql: &impl Sql, cfg: &EvalConfig, seq: i64) -> R<()> {
    let jobs = sql.exec("SELECT job FROM jobs WHERE seq = ? ORDER BY job", &[i(seq)])?;
    for j in &jobs {
        let job = int(j, 0)?;
        for kind in ["summary", "bundle"] {
            insert_lane(sql, cfg, seq, kind, job, Some(job), 0, 0, 0)?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ dispatch

fn kick(sql: &impl Sql, cfg: &EvalConfig, seq: i64, kind: &str, l: &LaneRow, now: u64, out: &mut Vec<LaneKick>) -> R<()> {
    let generation = l.generation + 1;
    let rekick = l.kicks > 0 && !matches!(l.status.as_str(), "pending");
    sql.exec(
        "UPDATE lanes SET gen = ?, status = 'kicked', kicked_ms = ?, kicks = kicks + 1, rekicks = rekicks + ?
         WHERE seq = ? AND kind = ? AND lane = ?",
        &[i(generation), bind_u64(now)?, i(i64::from(rekick)), i(seq), s(kind), i(l.lane)],
    )?;
    event(sql, seq, kind, l.lane, if rekick { "rekick" } else { "kick" }, 0, now)?;
    out.push(LaneKick {
        v: KICK_VERSION,
        instance: cfg.instance.clone(),
        seq,
        kind: kind.into(),
        lane: l.lane,
        generation,
        name: l.name.clone(),
        authority: cfg.kick_authority(),
    });
    Ok(())
}

/// The lanes of `kinds` that are due a kick, each recorded with `gen + 1`
/// before its kick is returned.
pub fn lanes_due(sql: &impl Sql, cfg: &EvalConfig, c: &Cohort, kinds: &[&str], now: u64) -> R<Vec<LaneKick>> {
    let mut out = Vec::new();
    if !live(sql, now)? {
        return Ok(out);
    }
    let f = c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    for kind in kinds {
        let (cap, stale) = match *kind {
            // A lane is silent at most one call: a chunk (within its lease) or one
            // isolation request (within the declared per-document limit).
            "score" => (f.score.max_lanes.min(cfg.max_score_lanes), 3 * f.score.lease_ms.max(f.score.limits().doc_timeout_ms)),
            "gen" => (f.generation.max_lanes, 3 * f.generation.lease_ms),
            _ => (f.package.max_lanes, PACK_STALE_MS),
        };
        let rows = lane_rows(sql, c.seq, kind)?;
        let mut live_n = rows.iter().filter(|l| matches!(l.status.as_str(), "kicked" | "starting" | "running" | "waiting")).count() as u32;
        // Jobs (score) or work (gen) that a live lane already covers.
        let mut covered: BTreeSet<i64> = rows
            .iter()
            .filter(|l| matches!(l.status.as_str(), "kicked" | "starting" | "running" | "waiting"))
            .filter_map(|l| l.job)
            .collect();
        let gen_live = live_n > 0;
        for l in &rows {
            // Before the seal no job rows exist yet (pre-started lanes): open.
            let open_job = |job: Option<i64>| -> R<bool> {
                let Some(job) = job else { return Ok(false) };
                let st = sql.exec("SELECT status FROM jobs WHERE seq = ? AND job = ?", &[i(c.seq), i(job)])?;
                Ok(st.first().is_none_or(|r| text(r, 0).is_ok_and(|t| t != "done" && t != "faulted")))
            };
            let due = match l.status.as_str() {
                "pending" | "paused" => {
                    let ok = live_n < cap && (kind != &"score" || open_job(l.job)?);
                    if ok {
                        live_n += 1;
                    }
                    ok
                }
                "kicked" => now.saturating_sub(l.kicked_ms) > KICK_UNSEEN_MS && l.seen_ms < l.kicked_ms,
                "starting" | "running" | "waiting" => {
                    now.saturating_sub(l.seen_ms) > stale && (kind != &"score" || open_job(l.job)?)
                }
                // Done or failed: kick one lane again only for work no live lane covers.
                _ => match *kind {
                    "score" => {
                        let job = l.job.unwrap_or(-1);
                        let claimable = !covered.contains(&job)
                            && open_job(l.job)?
                            && int(
                                &sql.exec(
                                    "SELECT COUNT(*) FROM chunks WHERE seq = ? AND job = ? AND status = 'pending'
                                       AND claim_until_ms <= ?",
                                    &[i(c.seq), i(job), bind_u64(now)?],
                                )?[0],
                                0,
                            )? > 0;
                        if claimable && live_n < cap {
                            covered.insert(job);
                            live_n += 1;
                            true
                        } else {
                            false
                        }
                    }
                    "gen" => {
                        let pending = int(
                            &sql.exec(
                                "SELECT COUNT(*) FROM docs WHERE seq = ? AND status = 'pending' AND claim_until_ms <= ?",
                                &[i(c.seq), bind_u64(now)?],
                            )?[0],
                            0,
                        )?;
                        let ok = pending > 0 && !gen_live && live_n == 0;
                        if ok {
                            live_n += 1;
                        }
                        ok
                    }
                    _ => false,
                },
            };
            if due {
                kick(sql, cfg, c.seq, kind, l, now, &mut out)?;
            }
        }
    }
    Ok(out)
}

/// Names of a cohort's lanes of `kinds` that may still hold a container.
pub fn live_lane_names(sql: &impl Sql, seq: i64, kinds: &[&str]) -> R<Vec<String>> {
    let mut out = Vec::new();
    for kind in kinds {
        let rows = sql.exec(
            &format!(
                "SELECT name FROM lanes WHERE seq = ? AND kind = ? AND (status IN {LIVE} OR status = 'paused'
                   OR (container_up = 1 AND final = 0)) ORDER BY lane"
            ),
            &[i(seq), s(*kind)],
        )?;
        for r in &rows {
            out.push(text(r, 0)?.to_owned());
        }
    }
    Ok(out)
}

/// The generating phase's sweep: generation lanes, and the just-in-time
/// pre-start of scoring lanes once the generation tail is projected to end
/// within the plan's lead. Each scoring lane is pre-started at most once.
pub fn generation_sweep(sql: &impl Sql, cfg: &EvalConfig, c: &Cohort, now: u64) -> R<Work> {
    let mut kicks = lanes_due(sql, cfg, c, &["gen"], now)?;
    let f = c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    let row = &sql.exec("SELECT prestarted_ms, gen_started_ms FROM cohorts WHERE seq = ?", &[i(c.seq)])?[0];
    if f.score.prestart_lead_ms > 0
        && opt_int(row, 0).is_none()
        && live(sql, now)?
        && let Some(eta) = generation_eta_ms(sql, c.seq, opt_int(row, 1).map(|v| v as u64), now)?
        && eta <= f.score.prestart_lead_ms
    {
        sql.exec("UPDATE cohorts SET prestarted_ms = ? WHERE seq = ?", &[bind_u64(now)?, i(c.seq)])?;
        sql.exec("UPDATE lanes SET prestart = 1 WHERE seq = ? AND kind = 'score' AND status = 'pending'", &[i(c.seq)])?;
        kicks.extend(lanes_due(sql, cfg, c, &["score"], now)?);
    }
    Ok(Work::Dispatch { seq: c.seq, kicks, stop: Vec::new() })
}

/// Projected time to the end of generation from its rate so far: pending items
/// times the mean time per resolved item. `None` before any item resolved.
pub fn generation_eta_ms(sql: &impl Sql, seq: i64, started: Option<u64>, now: u64) -> R<Option<u64>> {
    let Some(started) = started else { return Ok(None) };
    let r = &sql.exec(
        "SELECT SUM(status = 'pending'), SUM(status != 'pending') FROM docs WHERE seq = ? AND kind != 'human_only'",
        &[i(seq)],
    )?[0];
    let (pending, done) = (opt_int(r, 0).unwrap_or(0) as u64, opt_int(r, 1).unwrap_or(0) as u64);
    if done == 0 {
        return Ok(None);
    }
    Ok(Some(pending * now.saturating_sub(started) / done))
}

/// The scoring phase's sweep: the budget check, the end of scoring, or kicks.
pub fn scoring_sweep(sql: &impl Sql, cfg: &EvalConfig, c: &Cohort, now: u64) -> R<Work> {
    let seq = c.seq;
    let f = c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    let open = int(&sql.exec("SELECT COUNT(*) FROM jobs WHERE seq = ? AND status NOT IN ('done','faulted')", &[i(seq)])?[0], 0)?;
    if open == 0 {
        let stop = live_lane_names(sql, seq, &["score"])?;
        let faulted = int(&sql.exec("SELECT COUNT(*) FROM jobs WHERE seq = ? AND status = 'faulted'", &[i(seq)])?[0], 0)?;
        if faulted > 0 {
            // Packaging needs every job: the missing evidence pauses the cohort.
            fault(sql, seq, "unresolved_infrastructure", now)?;
            return Ok(Work::Dispatch { seq, kicks: Vec::new(), stop });
        }
        sql.exec("UPDATE cohorts SET scoring_finished_ms = ? WHERE seq = ?", &[bind_u64(now)?, i(seq)])?;
        set_phase(sql, seq, "packaging", now, "scoring complete")?;
        plan_pack_lanes(sql, cfg, seq)?;
        let c2 = cohort(sql, seq)?;
        let kicks = lanes_due(sql, cfg, &c2, &["summary", "bundle"], now)?;
        return Ok(Work::Dispatch { seq, kicks, stop });
    }
    // Spend so far, plus what the remaining work may still cost, must fit.
    let sp = spend(sql, c, &["score"], now, false)?;
    let unresolved = unresolved_tasks(sql, seq)?;
    let unstarted = int(&sql.exec("SELECT COUNT(*) FROM lanes WHERE seq = ? AND kind = 'score' AND kicks = 0", &[i(seq)])?[0], 0)?;
    let remaining = u128::from(unresolved) * u128::from(f.cost.doc_ub_nanousd) + unstarted as u128 * u128::from(f.cost.cold_ub_nanousd);
    if cost::nano_to_micro_ceil(sp.measured_nanousd + sp.unreported_nanousd + remaining) > f.score.scoring_budget_microusd {
        fault(sql, seq, "scoring_budget", now)?;
        let stop = live_lane_names(sql, seq, &["score"])?;
        return Ok(Work::Dispatch { seq, kicks: Vec::new(), stop });
    }
    let kicks = lanes_due(sql, cfg, c, &["score"], now)?;
    Ok(Work::Dispatch { seq, kicks, stop: Vec::new() })
}

fn unresolved_tasks(sql: &impl Sql, seq: i64) -> R<u64> {
    let r = &sql.exec(
        "SELECT (SELECT COALESCE(SUM(COALESCE(j.tasks, c.task_count, 0)), 0) FROM jobs j
                   WHERE j.seq = c.seq AND j.status NOT IN ('done','faulted'))
              - (SELECT COUNT(*) FROM preds p JOIN jobs j ON j.seq = p.seq AND j.job = p.job
                 WHERE p.seq = ? AND j.status NOT IN ('done','faulted'))
         FROM cohorts c WHERE c.seq = ?",
        &[i(seq), i(seq)],
    )?[0];
    Ok(opt_int(r, 0).unwrap_or(0).max(0) as u64)
}

/// The packaging phase's sweep: summary and bundle lanes until every job has
/// both, then the packaging root step.
pub fn packaging_sweep(sql: &impl Sql, cfg: &EvalConfig, c: &Cohort, now: u64) -> R<Work> {
    let missing = int(
        &sql.exec(
            "SELECT COUNT(*) FROM jobs WHERE seq = ? AND (summary_sha256 IS NULL OR bundle_sha256 IS NULL)",
            &[i(c.seq)],
        )?[0],
        0,
    )?;
    if missing == 0 {
        return Ok(Work::Package { seq: c.seq });
    }
    let kicks = lanes_due(sql, cfg, c, &["summary", "bundle"], now)?;
    Ok(Work::Dispatch { seq: c.seq, kicks, stop: Vec::new() })
}

// ------------------------------------------------------------------- reports

/// A lane's report: its state and cumulative metering (since its first start in
/// this cohort), sent at alarm entry, every 20 s of a cold path, and at the end.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneReport {
    /// `starting`, `running`, `waiting`, `done`, `paused`, `yield`,
    /// `start_failed` or `failed`.
    pub state: String,
    #[serde(default)]
    pub cold_ms: Option<u64>,
    /// Container CPU spent by the cold path (start, artifact stream, load).
    #[serde(default)]
    pub cold_cpu_ms: Option<u64>,
    #[serde(default)]
    pub started: bool,
    #[serde(default)]
    pub active_ms: u64,
    #[serde(default)]
    pub cpu_ms: u64,
    #[serde(default)]
    pub do_ms: u64,
    #[serde(default)]
    pub container_up: bool,
    #[serde(default)]
    pub code: Option<String>,
    /// The container host's CPU model (content-free), reported with a cold path.
    #[serde(default)]
    pub host: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub id: String,
    pub package_sha256: String,
    pub manifest_key: String,
}

/// Load-mode fault hooks that apply to one lane.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneFaults {
    pub kill: bool,
    pub crash_after_put: bool,
    pub throw_after_claim: bool,
    pub crash_task: Option<u32>,
    /// More tasks of this job that kill the container whenever requested.
    #[serde(default)]
    pub crash_tasks: Vec<u32>,
    /// Every request of this job kills the container, the control included.
    #[serde(default)]
    pub crash_all: bool,
    /// This task's first single-document request on the lane kills the container once.
    #[serde(default)]
    pub flaky_task: Option<u32>,
    /// The artifact load reports failure on every fresh instance.
    #[serde(default)]
    pub load_fail: bool,
    /// Batch-shard reads fail until this time (lane clock, ms since the epoch).
    #[serde(default)]
    pub r2_outage_until_ms: u64,
}

/// What a lane is told in reply to a report: whether to go on, and the
/// parameters of its work. Never any text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    /// `go`, `wait` (scoring lanes before the seal), `done`, `paused` or `fenced`.
    pub status: String,
    pub phase: String,
    pub label: String,
    pub job: Option<i64>,
    pub artifact: Option<ArtifactRef>,
    pub manifest: Option<String>,
    pub lease_ms: u64,
    pub isolation_lease_ms: u64,
    pub inactivity_ms: u64,
    pub prestart_wait_ms: u64,
    pub generators: Vec<String>,
    pub inflight: u32,
    pub gen_lease_ms: u64,
    pub faults: LaneFaults,
    /// Scoring lanes: the plan's lane limits ([`crate::eval::plan::LaneLimits`]).
    #[serde(default)]
    pub step_budget_ms: u64,
    #[serde(default)]
    pub doc_timeout_ms: u64,
    #[serde(default)]
    pub load_wait_ms: u64,
    #[serde(default)]
    pub artifact_max_bytes: u64,
    /// Consecutive start failures after which a lane reports `failed`.
    #[serde(default)]
    pub failures_before_pause: u32,
    /// The job's artifact failed (`jobs.artifact_failed`): its unresolved
    /// documents are model errors and need no container.
    #[serde(default)]
    pub artifact_failed: bool,
    /// Scoring lanes: the digest of the artifact spec the cohort names
    /// (`fanout.score.artifact_spec`), which every load names
    /// (`spec=<digest>`); absent for a cohort that names none, whose loads
    /// are admitted by the scorer's unnamed-load spec (spec v2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_sha256: Option<String>,
}

fn assignment_for(sql: &impl Sql, c: &Cohort, kind: &str, l: &LaneRow, now: u64) -> R<Assignment> {
    let f = c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    let status = if c.fault.is_some() || !live(sql, now)? {
        "paused"
    } else {
        match (kind, c.phase.as_str()) {
            ("score", "scoring") => "go",
            ("score", "generating" | "sealing") => "wait",
            ("gen", "generating") => "go",
            ("summary" | "bundle", "packaging") => "go",
            _ => "done",
        }
    };
    let lim = f.score.limits();
    let mut a = Assignment {
        status: status.into(),
        phase: c.phase.clone(),
        label: c.label.clone(),
        job: l.job,
        lease_ms: f.score.lease_ms,
        isolation_lease_ms: f.score.isolation_lease_ms,
        inactivity_ms: f.score.inactivity_ms,
        prestart_wait_ms: f.score.prestart_wait_ms,
        manifest: c.manifest_digest.clone(),
        step_budget_ms: lim.step_budget_ms,
        doc_timeout_ms: lim.doc_timeout_ms,
        load_wait_ms: lim.load_wait_ms,
        artifact_max_bytes: lim.artifact_max_bytes,
        failures_before_pause: f.score.failures_before_pause,
        ..Default::default()
    };
    if kind == "score" {
        // Only scoring lanes load an artifact.
        // The digest the plan froze, never one rebuilt per report.
        a.spec_sha256 = c.plan.load_spec_sha256().map(str::to_owned);
    }
    if kind == "score"
        && let Some(job) = l.job
    {
        let r = sql.exec("SELECT artifact_failed FROM jobs WHERE seq = ? AND job = ?", &[i(c.seq), i(job)])?;
        a.artifact_failed = r.first().is_some_and(|r| opt_text(r, 0).is_some());
    }
    if kind == "gen" {
        a.generators = c.plan.generators.clone();
        a.inflight = f.generation.inflight_per_lane;
        a.gen_lease_ms = f.generation.lease_ms;
    }
    if let Some(job) = l.job {
        let idx = job as usize;
        let roster: Vec<&crate::eval::plan::ArtifactJson> = std::iter::once(&c.plan.reference)
            .chain(c.plan.previous_reference.iter())
            .chain(c.plan.entrants.iter())
            .collect();
        if let Some(art) = roster.get(idx) {
            a.artifact = Some(ArtifactRef {
                id: art.id().map_err(|e| EvalError::Plan(e.0))?.to_string(),
                package_sha256: art.package_sha256.clone(),
                manifest_key: art.manifest_key.clone(),
            });
        }
        if let Some(load) = &c.plan.load
            && kind == "score"
        {
            let lane = l.lane as u32;
            let started = opt_int(&sql.exec("SELECT scoring_started_ms FROM cohorts WHERE seq = ?", &[i(c.seq)])?[0], 0);
            let jb = job as u32;
            a.faults = LaneFaults {
                kill: load.faults.kill_lanes.contains(&lane),
                crash_after_put: load.faults.crash_after_put.contains(&lane),
                throw_after_claim: load.faults.throw_after_claim.contains(&lane),
                crash_task: load.faults.crash_task.filter(|(j, _)| *j == jb).map(|(_, t)| t),
                crash_tasks: load.faults.crash_tasks.iter().filter(|(j, _)| *j == jb).map(|(_, t)| *t).collect(),
                crash_all: load.faults.crash_jobs.contains(&jb),
                flaky_task: load.faults.flaky_task.filter(|(j, _)| *j == jb).map(|(_, t)| t),
                load_fail: load.faults.load_fail_jobs.contains(&jb),
                r2_outage_until_ms: match (load.faults.r2_outage_ms, started) {
                    (0, _) => 0,
                    (ms, Some(t)) => t as u64 + ms,
                    // Before scoring starts the outage window has not opened.
                    (ms, None) => now + ms,
                },
            };
        }
    }
    Ok(a)
}

/// Records a lane's report and answers with its assignment.
pub fn lane_report(sql: &impl Sql, r: &LaneRef, rep: &LaneReport, now: u64) -> R<Assignment> {
    let Some(l) = fence(sql, r)? else {
        return Ok(Assignment { status: "fenced".into(), ..Default::default() });
    };
    let c = cohort(sql, r.seq)?;
    touch(sql, r, now)?;
    sql.exec(
        "UPDATE lanes SET active_ms = MAX(active_ms, ?), cpu_ms = MAX(cpu_ms, ?), do_ms = MAX(do_ms, ?),
            container_up = ?, starts = starts + ? WHERE seq = ? AND kind = ? AND lane = ?",
        &[
            bind_u64(rep.active_ms)?,
            bind_u64(rep.cpu_ms)?,
            bind_u64(rep.do_ms)?,
            i(i64::from(rep.container_up)),
            i(i64::from(rep.started)),
            i(r.seq),
            s(&r.kind),
            i(r.lane),
        ],
    )?;
    if let Some(ms) = rep.cold_ms {
        event(sql, r.seq, &r.kind, r.lane, "cold", ms, now)?;
    }
    if let Some(ms) = rep.cold_cpu_ms {
        event(sql, r.seq, &r.kind, r.lane, "cold_cpu", ms, now)?;
    }
    if let Some(h) = &rep.host {
        event(sql, r.seq, &r.kind, r.lane, &format!("host:{}", textgen_safe(h)), 0, now)?;
    }
    // A report without a code keeps the lane's last failure note.
    let set = |status: &str, last: bool| -> R<()> {
        sql.exec(
            "UPDATE lanes SET status = ?, final = ?, note = COALESCE(?, note) WHERE seq = ? AND kind = ? AND lane = ?",
            &[
                s(status),
                i(i64::from(last)),
                rep.code.as_deref().map_or(SqlValue::Null, |c| s(textgen_safe(c))),
                i(r.seq),
                s(&r.kind),
                i(r.lane),
            ],
        )?;
        Ok(())
    };
    match rep.state.as_str() {
        "starting" | "running" | "waiting" => set(&rep.state, false)?,
        "done" => set("done", !rep.container_up)?,
        "paused" => set("paused", !rep.container_up)?,
        "yield" => {
            // A pre-started lane that gave up: kicked again when scoring starts.
            set("pending", !rep.container_up)?;
            sql.exec("UPDATE lanes SET prestart = 0 WHERE seq = ? AND kind = ? AND lane = ?", &[i(r.seq), s(&r.kind), i(r.lane)])?;
        }
        "start_failed" => {
            sql.exec(
                "UPDATE lanes SET start_failures = start_failures + 1, note = ? WHERE seq = ? AND kind = ? AND lane = ?",
                &[s(textgen_safe(rep.code.as_deref().unwrap_or("start_failed"))), i(r.seq), s(&r.kind), i(r.lane)],
            )?;
            event(sql, r.seq, &r.kind, r.lane, "start_failed", 0, now)?;
            if c.phase == "scoring" {
                engine::note_scorer_failure(sql, r.seq, now)?;
            }
        }
        _ => set("failed", !rep.container_up)?,
    }
    let c = cohort(sql, r.seq)?;
    let _ = l.prestart;
    assignment_for(sql, &c, &r.kind, &l, now)
}

/// Content-free code text for storage: short, code characters only.
fn textgen_safe(code: &str) -> String {
    crate::eval::limiter::sanitize(code).chars().take(64).collect()
}

// -------------------------------------------------------------------- claims

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkClaim {
    pub job: i64,
    pub chunk: i64,
    /// The attempt run; for a reconciliation, the expired attempt to check.
    pub attempt: u8,
    pub lost: Vec<u8>,
    pub record_only: bool,
    /// The claim token that fences the commit.
    pub claim: i64,
    /// Check the expired attempt's stored objects instead of computing.
    pub reconcile: bool,
    /// The previous attempt crashed: one document per request, fresh instance.
    pub isolate: bool,
    pub lease_ms: u64,
    pub tasks: Vec<u32>,
    pub digests: Vec<String>,
    /// Batch shards (key, SHA-256) holding the tasks' texts.
    pub shards: Vec<(String, String)>,
    /// Outcomes this attempt already resolved (an isolation attempt handed
    /// back unfinished): kept, never recomputed.
    #[serde(default)]
    pub done: Vec<TaskOutcome>,
    /// The job's artifact failed: every other task is a model error, scored by
    /// no container.
    #[serde(default)]
    pub model_error: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaimReply {
    Chunk(ChunkClaim),
    Wait { ms: u64 },
    Done,
    Paused,
    Fenced,
}

fn unresolved(sql: &impl Sql, seq: i64, job: i64, first: i64, count: i64) -> R<Vec<(u32, String)>> {
    sql.exec(
        "SELECT d.task, d.text_sha256 FROM docs d
         WHERE d.seq = ? AND d.task >= ? AND d.task < ?
           AND NOT EXISTS (SELECT 1 FROM preds p WHERE p.seq = d.seq AND p.job = ? AND p.task = d.task)
         ORDER BY d.task",
        &[i(seq), i(first), i(first + count), i(job)],
    )?
    .iter()
    .map(|r| Ok((int(r, 0)? as u32, text(r, 1)?.to_owned())))
    .collect()
}

fn shards_for(sql: &impl Sql, seq: i64, lo: u32, hi: u32) -> R<Vec<(String, String)>> {
    sql.exec(
        "SELECT key, sha256 FROM batch_shards WHERE seq = ? AND first_task <= ? AND first_task + docs > ? ORDER BY shard",
        &[i(seq), i(i64::from(hi)), i(i64::from(lo))],
    )?
    .iter()
    .map(|r| Ok((text(r, 0)?.to_owned(), text(r, 1)?.to_owned())))
    .collect()
}

/// The outcomes an isolation attempt has resolved so far, in task order.
fn iso_done(sql: &impl Sql, seq: i64, job: i64, chunk: i64, attempt: i64) -> R<Vec<TaskOutcome>> {
    sql.exec(
        "SELECT outcome FROM iso_done WHERE seq = ? AND job = ? AND chunk = ? AND attempt = ? ORDER BY task",
        &[i(seq), i(job), i(chunk), i(attempt)],
    )?
    .iter()
    .map(|r| serde_json::from_str(text(r, 0)?).map_err(|_| EvalError::State("iso_done outcome")))
    .collect()
}

/// A scoring lane's claim: its own range first, then any unclaimed chunk of
/// its job. An isolation attempt handed back unfinished is continued under the
/// same attempt number; an expired attempt that may have stored objects is
/// reconciled before any new attempt is allocated.
pub fn claim_chunk(sql: &impl Sql, r: &LaneRef, name: &str, now: u64) -> R<ClaimReply> {
    let Some(l) = fence(sql, r)? else { return Ok(ClaimReply::Fenced) };
    touch(sql, r, now)?;
    let c = cohort(sql, r.seq)?;
    if c.fault.is_some() || !live(sql, now)? {
        return Ok(ClaimReply::Paused);
    }
    match c.phase.as_str() {
        "generating" | "sealing" => return Ok(ClaimReply::Wait { ms: 2_000 }),
        "scoring" => {}
        _ => return Ok(ClaimReply::Done),
    }
    let f = c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    let job = l.job.ok_or(EvalError::State("scoring lane without a job"))?;
    let js = sql.exec("SELECT status, artifact_failed FROM jobs WHERE seq = ? AND job = ?", &[i(r.seq), i(job)])?;
    if js.first().is_none_or(|j| text(j, 0).is_ok_and(|t| t == "done" || t == "faulted")) {
        return Ok(ClaimReply::Done);
    }
    let artifact_failed = opt_text(&js[0], 1).is_some();
    let range = sql.exec("SELECT chunk_lo, chunk_hi FROM lanes WHERE seq = ? AND kind = 'score' AND lane = ?", &[i(r.seq), i(r.lane)])?;
    let (lo, hi) = (int(&range[0], 0)?, int(&range[0], 1)?);
    let candidates = sql.exec(
        "SELECT chunk, first_task, count, attempt, recorded, inflight, reconcile_checked, crashes, iso_yield FROM chunks
         WHERE seq = ? AND job = ? AND status = 'pending' AND claim_until_ms <= ?
         ORDER BY (chunk >= ? AND chunk < ?) DESC, chunk LIMIT 64",
        &[i(r.seq), i(job), bind_u64(now)?, i(lo), i(hi)],
    )?;
    for ch in &candidates {
        let (chunk, first, count) = (int(ch, 0)?, int(ch, 1)?, int(ch, 2)?);
        let (allocated, recorded, inflight, checked, crashes) = (int(ch, 3)?, int(ch, 4)?, int(ch, 5)?, int(ch, 6)?, int(ch, 7)?);
        let iso_yield = int(ch, 8)?;
        let tasks = unresolved(sql, r.seq, job, first, count)?;
        if tasks.is_empty() {
            continue;
        }
        let stamp = |attempt: i64, inflight: i64, lease: u64, kind: &str| -> R<i64> {
            let claim = sql.exec(
                "UPDATE chunks SET attempt = ?, inflight = ?, claims = claims + 1, claim_until_ms = ?, holder = ?,
                    claim_gen = ?, claimed_ms = ?, claim_kind = ?, iso_yield = 0
                 WHERE seq = ? AND job = ? AND chunk = ? RETURNING claims",
                &[
                    i(attempt),
                    i(inflight),
                    bind_u64(now + lease)?,
                    s(name),
                    i(r.generation),
                    bind_u64(now)?,
                    s(kind),
                    i(r.seq),
                    i(job),
                    i(chunk),
                ],
            )?;
            Ok(int(&claim[0], 0)?)
        };
        let lo_t = tasks.first().map(|t| t.0).unwrap_or(0);
        let hi_t = tasks.last().map(|t| t.0).unwrap_or(0);
        let (task_ids, digests): (Vec<u32>, Vec<String>) = tasks.into_iter().unzip();
        let isolate = crashes > 0;
        let lease = if isolate { f.score.isolation_lease_ms } else { f.score.lease_ms };
        let claim_of = |attempt: i64, lost: Vec<u8>, record_only: bool, claim: i64, reconcile: bool, done: Vec<TaskOutcome>| -> R<ClaimReply> {
            Ok(ClaimReply::Chunk(ChunkClaim {
                job,
                chunk,
                attempt: attempt as u8,
                lost,
                record_only,
                claim,
                reconcile,
                isolate: isolate && !record_only,
                lease_ms: lease,
                tasks: task_ids.clone(),
                digests: digests.clone(),
                shards: shards_for(sql, r.seq, lo_t, hi_t)?,
                done,
                model_error: artifact_failed && !record_only && !reconcile,
            }))
        };
        if inflight > 0 && iso_yield == inflight {
            // An isolation attempt handed back unfinished: the same attempt
            // continues, keeping every outcome it already resolved.
            let done = iso_done(sql, r.seq, job, chunk, inflight)?;
            let claim = stamp(allocated, inflight, lease, "continue")?;
            return claim_of(inflight, ((recorded + 1)..inflight).map(|a| a as u8).collect(), false, claim, false, done);
        }
        if inflight > 0 && inflight > checked {
            // An expired attempt: its objects may be durable. Check before recomputing.
            let claim = stamp(allocated, inflight, lease, "reconcile")?;
            return claim_of(inflight, ((recorded + 1)..inflight).map(|a| a as u8).collect(), false, claim, true, Vec::new());
        }
        let lost: Vec<u8> = ((recorded + 1)..=allocated).map(|a| a as u8).collect();
        let record_only = allocated >= i64::from(c.plan.max_attempts);
        if record_only && lost.is_empty() {
            // Every allowed attempt has a receipt and none resolved these tasks.
            fault_job(sql, r.seq, job, "unresolved_infrastructure", now)?;
            return Ok(ClaimReply::Done);
        }
        let attempt = if record_only { allocated } else { allocated + 1 };
        let claim = stamp(attempt, if record_only { 0 } else { attempt }, lease, if record_only { "record" } else { "fresh" })?;
        sql.exec(
            "UPDATE jobs SET status = 'running', started_ms = COALESCE(started_ms, ?), attempts = attempts + ?
             WHERE seq = ? AND job = ?",
            &[bind_u64(now)?, i(i64::from(!record_only)), i(r.seq), i(job)],
        )?;
        return claim_of(attempt, lost, record_only, claim, false, Vec::new());
    }
    let next = sql.exec(
        "SELECT MIN(claim_until_ms) FROM chunks WHERE seq = ? AND job = ? AND status = 'pending' AND claim_until_ms > ?",
        &[i(r.seq), i(job), bind_u64(now)?],
    )?;
    match next.first().and_then(|row| opt_int(row, 0)) {
        Some(t) if (t as u64).saturating_sub(now) <= WAIT_HORIZON_MS => Ok(ClaimReply::Wait { ms: (t as u64 - now) + 50 }),
        _ => Ok(ClaimReply::Done),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkCommit {
    pub job: i64,
    pub chunk: i64,
    pub attempt: u8,
    pub claim: i64,
    pub record_only: bool,
    pub outcomes: Vec<TaskOutcome>,
    pub object_key: String,
    pub object_sha256: String,
    pub object_len: u64,
    #[serde(default)]
    pub timing: CallTiming,
    /// The request crashed its container: the next claim isolates.
    #[serde(default)]
    pub crashed: bool,
    /// Committed from durable objects of an expired attempt, with no compute.
    #[serde(default)]
    pub reconciled: bool,
    /// The lane's cumulative metering at the commit.
    #[serde(default)]
    pub meter: Meter,
}

/// A lane's cumulative metering in this cohort: container provisioned time,
/// container CPU, and the lane Durable Object's active time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meter {
    pub active_ms: u64,
    pub cpu_ms: u64,
    pub do_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitReply {
    Committed { job_done: bool, scoring_done: bool },
    Already,
    Fenced,
}

/// The facts a chunk's receipts are rebuilt from, read under the claim.
pub struct ClaimFacts {
    pub manifest: slop_ninja_protocol::Digest,
    pub artifact: slop_ninja_protocol::Digest,
    pub seed: [u8; 32],
    pub prefix: String,
    pub lost: Vec<u8>,
    pub tasks: Vec<u32>,
}

fn claim_facts(sql: &impl Sql, cfg: &EvalConfig, c: &Cohort, job: i64, chunk: i64, attempt: u8, record_only: bool) -> R<ClaimFacts> {
    let manifest = c.manifest_digest.as_deref().and_then(parse_digest).ok_or(EvalError::State("manifest digest"))?;
    let a = sql.exec("SELECT artifact FROM jobs WHERE seq = ? AND job = ?", &[i(c.seq), i(job)])?;
    let artifact = parse_digest(text(a.first().ok_or(EvalError::State("job"))?, 0)?).ok_or(EvalError::State("artifact"))?;
    let ch = sql.exec("SELECT first_task, count, recorded FROM chunks WHERE seq = ? AND job = ? AND chunk = ?", &[i(c.seq), i(job), i(chunk)])?;
    let ch = ch.first().ok_or(EvalError::State("chunk"))?;
    let (first, count, recorded) = (int(ch, 0)?, int(ch, 1)?, int(ch, 2)?);
    let top = i64::from(attempt) - i64::from(!record_only);
    Ok(ClaimFacts {
        manifest,
        artifact,
        seed: cohort_seed(&cfg.private_seed, &c.label),
        prefix: engine::receipt_prefix(&c.label),
        lost: ((recorded + 1)..=top).map(|a| a as u8).collect(),
        tasks: unresolved(sql, c.seq, job, first, count)?.into_iter().map(|t| t.0).collect(),
    })
}

/// Commits a chunk attempt whose receipt object the lane already stored.
pub fn commit_chunk(sql: &impl Sql, cfg: &EvalConfig, r: &LaneRef, name: &str, cm: &ChunkCommit, now: u64) -> R<CommitReply> {
    let seq = r.seq;
    // An exact repeat (the lane lost the reply to its own commit) comes first:
    // after the first commit the chunk may be done and its claim cleared.
    let prior = sql.exec("SELECT sha256 FROM receipt_objects WHERE seq = ? AND key = ?", &[i(seq), s(&cm.object_key)])?;
    if let Some(p) = prior.first() {
        return if text(p, 0)? == cm.object_sha256 {
            Ok(CommitReply::Already)
        } else {
            Err(EvalError::State("another object is committed at this key"))
        };
    }
    let ch = sql.exec(
        "SELECT claims, status, holder, attempt, crashes FROM chunks WHERE seq = ? AND job = ? AND chunk = ?",
        &[i(seq), i(cm.job), i(cm.chunk)],
    )?;
    let ch = ch.first().ok_or(EvalError::State("chunk"))?;
    if int(ch, 0)? != cm.claim
        || text(ch, 1)? != "pending"
        || opt_text(ch, 2).as_deref() != Some(name)
        || int(ch, 3)? != i64::from(cm.attempt)
    {
        sql.exec("UPDATE lanes SET fenced = fenced + 1 WHERE seq = ? AND kind = ? AND lane = ?", &[i(seq), s(&r.kind), i(r.lane)])?;
        return Ok(CommitReply::Fenced);
    }
    let crashes = int(ch, 4)?;
    let c = cohort(sql, seq)?;
    let facts = claim_facts(sql, cfg, &c, cm.job, cm.chunk, cm.attempt, cm.record_only)?;
    let isolated = cm.outcomes.iter().filter(|o| o.isolated_crash).count() as i64;
    if isolated > 0 && (crashes == 0 || cm.outcomes.iter().any(|o| o.isolated_crash && o.class != 2)) {
        return Err(EvalError::State("an isolated crash outside an isolation claim"));
    }
    if cm.outcomes.iter().any(|o| o.artifact_failed) {
        let flagged = sql.exec("SELECT artifact_failed FROM jobs WHERE seq = ? AND job = ?", &[i(seq), i(cm.job)])?;
        if flagged.first().is_none_or(|r| opt_text(r, 0).is_none()) || cm.outcomes.iter().any(|o| o.artifact_failed && o.class != 2) {
            return Err(EvalError::State("an artifact failure the job does not record"));
        }
    }
    // An isolation attempt's recorded outcomes are kept exactly.
    for kept in iso_done(sql, seq, cm.job, cm.chunk, i64::from(cm.attempt))? {
        if !cm.outcomes.contains(&kept) {
            return Err(EvalError::State("a commit differs from the attempt's recorded outcomes"));
        }
    }
    let built = receipts::build_chunk(
        &ChunkFacts {
            manifest: facts.manifest,
            artifact: facts.artifact,
            seed: &facts.seed,
            prefix: &facts.prefix,
            job: cm.job,
            chunk: cm.chunk,
            attempt: cm.attempt,
            lost: &facts.lost,
            record_only: cm.record_only,
            tasks: &facts.tasks,
        },
        &cm.outcomes,
    )
    .map_err(|e| EvalError::State(e.0))?;
    if built.object_key != cm.object_key
        || engine::sha_hex(&built.object) != cm.object_sha256
        || built.object.len() as u64 != cm.object_len
    {
        return Err(EvalError::State("receipt object differs from the committed outcomes"));
    }
    sql.exec(
        "INSERT INTO receipt_objects (seq, key, sha256, bytes, receipts) VALUES (?, ?, ?, ?, ?)",
        &[i(seq), s(&cm.object_key), s(&cm.object_sha256), i(built.object.len() as i64), i(built.receipts.len() as i64)],
    )?;
    for (attempt, task, number, class, bytes) in &built.receipts {
        sql.exec(
            "INSERT INTO receipts (seq, attempt_id, job, task, number, outcome, object_sha256, object_len, object_key)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                i(seq),
                s(attempt.to_string()),
                i(cm.job),
                i(i64::from(*task)),
                i(i64::from(*number)),
                i(receipts::outcome_tag(*class)),
                s(engine::sha_hex(bytes)),
                i(bytes.len() as i64),
                s(&cm.object_key),
            ],
        )?;
    }
    for o in cm.outcomes.iter().filter(|o| o.class != 5) {
        sql.exec(
            "INSERT INTO preds (seq, job, task, attempt, outcome, inv_ppb, share_ppb, human_profile, ai_profile, token_count)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                i(seq),
                i(cm.job),
                i(i64::from(o.task)),
                i(i64::from(cm.attempt)),
                i(i64::from(o.class)),
                o.inv_ppb.map_or(SqlValue::Null, |v| i(i64::from(v))),
                o.share_ppb.map_or(SqlValue::Null, |v| i(i64::from(v))),
                o.human.clone().map_or(SqlValue::Null, s),
                o.ai.clone().map_or(SqlValue::Null, s),
                o.tokens.map_or(SqlValue::Null, |t| i(i64::from(t))),
            ],
        )?;
    }
    let left = int(
        &sql.exec(
            "SELECT COUNT(*) FROM docs d, chunks c WHERE d.seq = ? AND c.seq = d.seq AND c.job = ? AND c.chunk = ?
               AND d.task >= c.first_task AND d.task < c.first_task + c.count
               AND NOT EXISTS (SELECT 1 FROM preds p WHERE p.seq = d.seq AND p.job = c.job AND p.task = d.task)",
            &[i(seq), i(cm.job), i(cm.chunk)],
        )?[0],
        0,
    )?;
    let done = left == 0;
    sql.exec(
        "DELETE FROM iso_done WHERE seq = ? AND job = ? AND chunk = ? AND attempt = ?",
        &[i(seq), i(cm.job), i(cm.chunk), i(i64::from(cm.attempt))],
    )?;
    if cm.outcomes.iter().any(|o| o.class == 1) {
        // The artifact demonstrably scores: its failure count starts again.
        sql.exec("UPDATE jobs SET artifact_failures = 0 WHERE seq = ? AND job = ?", &[i(seq), i(cm.job)])?;
    }
    sql.exec(
        "UPDATE chunks SET inflight = 0, claim_until_ms = 0, holder = NULL, recorded = ?, status = ?,
            crashes = crashes + ?, claim_kind = 'none', iso_yield = 0 WHERE seq = ? AND job = ? AND chunk = ?",
        &[
            i(i64::from(built.recorded)),
            s(if done { "done" } else { "pending" }),
            i(i64::from(cm.crashed)),
            i(seq),
            i(cm.job),
            i(cm.chunk),
        ],
    )?;
    sql.exec("UPDATE cohorts SET scorer_failures = 0, r2_puts = r2_puts + ? WHERE seq = ?", &[i(if cm.record_only { 1 } else { 2 }), i(seq)])?;
    let t = &cm.timing;
    sql.exec(
        "UPDATE jobs SET done_chunks = done_chunks + ?, call_ms = call_ms + ?, server_ms = server_ms + ?,
            load_ms = load_ms + ?, cpu_ms = cpu_ms + ?, isolated_crashes = isolated_crashes + ? WHERE seq = ? AND job = ?",
        &[
            i(i64::from(done)),
            bind_u64(t.call_ms)?,
            bind_u64(t.server_ms)?,
            bind_u64(t.load_ms)?,
            bind_u64(t.cpu_ms)?,
            i(isolated),
            i(seq),
            i(cm.job),
        ],
    )?;
    let job_done = !sql
        .exec(
            "UPDATE jobs SET status = 'done', finished_ms = ? WHERE seq = ? AND job = ? AND done_chunks = chunks
               AND status != 'faulted' RETURNING job",
            &[bind_u64(now)?, i(seq), i(cm.job)],
        )?
        .is_empty();
    if cm.record_only && !done {
        fault_job(sql, seq, cm.job, "unresolved_infrastructure", now)?;
    }
    sql.exec(
        "UPDATE lanes SET chunks = chunks + 1, lost = lost + ?, reconciled = reconciled + ?,
            active_ms = MAX(active_ms, ?), cpu_ms = MAX(cpu_ms, ?), do_ms = MAX(do_ms, ?)
         WHERE seq = ? AND kind = ? AND lane = ?",
        &[
            i(facts.lost.len() as i64),
            i(i64::from(cm.reconciled)),
            bind_u64(cm.meter.active_ms)?,
            bind_u64(cm.meter.cpu_ms)?,
            bind_u64(cm.meter.do_ms)?,
            i(seq),
            s(&r.kind),
            i(r.lane),
        ],
    )?;
    touch(sql, r, now)?;
    if !cm.record_only && !cm.reconciled {
        event(sql, seq, &r.kind, r.lane, "chunk", t.call_ms, now)?;
    }
    if cm.reconciled {
        event(sql, seq, &r.kind, r.lane, "reconciled", 0, now)?;
    }
    let open = int(&sql.exec("SELECT COUNT(*) FROM jobs WHERE seq = ? AND status NOT IN ('done','faulted')", &[i(seq)])?[0], 0)?;
    Ok(CommitReply::Committed { job_done, scoring_done: open == 0 })
}

fn held(sql: &impl Sql, seq: i64, job: i64, chunk: i64, claim: i64, name: &str) -> R<bool> {
    let ch = sql.exec("SELECT claims, holder, status FROM chunks WHERE seq = ? AND job = ? AND chunk = ?", &[i(seq), i(job), i(chunk)])?;
    Ok(ch.first().is_some_and(|c| {
        int(c, 0).is_ok_and(|v| v == claim) && opt_text(c, 1).as_deref() == Some(name) && text(c, 2).is_ok_and(|t| t == "pending")
    }))
}

/// A reconciliation found no stored results object for the expired attempt.
/// An isolation attempt that recorded outcomes is continued (none of them is
/// lost); otherwise the attempt is lost, and the lane's next claim computes
/// `attempt + 1`, recording it.
#[allow(clippy::too_many_arguments)]
pub fn chunk_lost(sql: &impl Sql, r: &LaneRef, name: &str, job: i64, chunk: i64, claim: i64, attempt: u8, now: u64) -> R<bool> {
    touch(sql, r, now)?;
    if !held(sql, r.seq, job, chunk, claim, name)? {
        return Ok(false);
    }
    if !iso_done(sql, r.seq, job, chunk, i64::from(attempt))?.is_empty() {
        sql.exec(
            "UPDATE chunks SET iso_yield = inflight, claim_until_ms = 0, holder = NULL, claim_kind = 'none'
             WHERE seq = ? AND job = ? AND chunk = ? AND inflight = ?",
            &[i(r.seq), i(job), i(chunk), i(i64::from(attempt))],
        )?;
        event(sql, r.seq, &r.kind, r.lane, "iso_resumed", 0, now)?;
        return Ok(true);
    }
    sql.exec(
        "UPDATE chunks SET reconcile_checked = MAX(reconcile_checked, ?), claim_until_ms = 0, holder = NULL,
            claim_kind = 'none' WHERE seq = ? AND job = ? AND chunk = ?",
        &[i(i64::from(attempt)), i(r.seq), i(job), i(chunk)],
    )?;
    event(sql, r.seq, &r.kind, r.lane, "lost", 0, now)?;
    Ok(true)
}

/// Releases a claim after its request was dispatched but its objects could
/// not be stored: the attempt stays allocated, so the next claim reconciles it
/// (recovering any results object that was stored). `failure` counts a scorer
/// failure (an R2 outage pauses the cohort instead of spending attempts).
#[allow(clippy::too_many_arguments)]
pub fn chunk_release(sql: &impl Sql, r: &LaneRef, name: &str, job: i64, chunk: i64, claim: i64, failure: bool, now: u64) -> R<bool> {
    touch(sql, r, now)?;
    if !held(sql, r.seq, job, chunk, claim, name)? {
        return Ok(false);
    }
    let backoff = if failure { NOT_DISPATCHED_BACKOFF_MS } else { 0 };
    sql.exec(
        "UPDATE chunks SET claim_until_ms = ?, holder = NULL, claim_kind = 'none' WHERE seq = ? AND job = ? AND chunk = ?",
        &[bind_u64(now + backoff)?, i(r.seq), i(job), i(chunk)],
    )?;
    if failure {
        engine::note_scorer_failure(sql, r.seq, now)?;
    }
    Ok(true)
}

/// Extends a held claim (an isolation run restarting its instance).
pub fn chunk_renew(sql: &impl Sql, r: &LaneRef, name: &str, job: i64, chunk: i64, claim: i64, now: u64) -> R<bool> {
    touch(sql, r, now)?;
    if !held(sql, r.seq, job, chunk, claim, name)? {
        return Ok(false);
    }
    let c = cohort(sql, r.seq)?;
    let f = c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    let crashes = int(&sql.exec("SELECT crashes FROM chunks WHERE seq = ? AND job = ? AND chunk = ?", &[i(r.seq), i(job), i(chunk)])?[0], 0)?;
    let lease = if crashes > 0 { f.score.isolation_lease_ms } else { f.score.lease_ms };
    sql.exec("UPDATE chunks SET claim_until_ms = ? WHERE seq = ? AND job = ? AND chunk = ?", &[bind_u64(now + lease)?, i(r.seq), i(job), i(chunk)])?;
    Ok(true)
}

/// Hands back a claim before any of its documents reached the model (a batch
/// shard that could not be read or did not match, a missing cohort manifest,
/// a scorer that refused the request, an instance that could not be restarted
/// before the first document). Nothing is recorded and no attempt is spent: a
/// fresh attempt's number goes back unused, a continuation stays continued, a
/// reconciliation or record-only claim stays as it was. `failure` counts a
/// scorer failure, so repeated failures pause the cohort (`scorer_unavailable`,
/// resumable by a clear). Returns whether the cohort paused.
#[allow(clippy::too_many_arguments)]
pub fn chunk_give_back(sql: &impl Sql, r: &LaneRef, name: &str, job: i64, chunk: i64, claim: i64, failure: bool, now: u64) -> R<bool> {
    touch(sql, r, now)?;
    let ch = sql.exec(
        "SELECT claims, holder, status, attempt, inflight, claim_kind FROM chunks WHERE seq = ? AND job = ? AND chunk = ?",
        &[i(r.seq), i(job), i(chunk)],
    )?;
    let Some(row) = ch.first() else { return Ok(false) };
    if int(row, 0)? != claim || opt_text(row, 1).as_deref() != Some(name) || text(row, 2)? != "pending" {
        return Ok(false);
    }
    let until = bind_u64(now + if failure { NOT_DISPATCHED_BACKOFF_MS } else { 0 })?;
    match text(row, 5)? {
        "fresh" if int(row, 4)? == int(row, 3)? && int(row, 4)? > 0 => {
            sql.exec(
                "UPDATE chunks SET attempt = attempt - 1, inflight = 0, claim_until_ms = ?, holder = NULL, claim_kind = 'none'
                 WHERE seq = ? AND job = ? AND chunk = ?",
                &[until, i(r.seq), i(job), i(chunk)],
            )?;
            sql.exec("UPDATE jobs SET attempts = attempts - 1 WHERE seq = ? AND job = ?", &[i(r.seq), i(job)])?;
        }
        "continue" => {
            sql.exec(
                "UPDATE chunks SET iso_yield = inflight, claim_until_ms = ?, holder = NULL, claim_kind = 'none'
                 WHERE seq = ? AND job = ? AND chunk = ?",
                &[until, i(r.seq), i(job), i(chunk)],
            )?;
        }
        _ => {
            sql.exec(
                "UPDATE chunks SET claim_until_ms = ?, holder = NULL, claim_kind = 'none' WHERE seq = ? AND job = ? AND chunk = ?",
                &[until, i(r.seq), i(job), i(chunk)],
            )?;
        }
    }
    event(sql, r.seq, &r.kind, r.lane, "given_back", 0, now)?;
    if failure { engine::note_scorer_failure(sql, r.seq, now) } else { Ok(false) }
}

/// The scorer refused a request before inference (not ready, or busy): the
/// claim goes back with no attempt spent ([`chunk_give_back`], counted).
#[allow(clippy::too_many_arguments)]
pub fn chunk_not_dispatched(sql: &impl Sql, r: &LaneRef, name: &str, job: i64, chunk: i64, claim: i64, now: u64) -> R<bool> {
    chunk_give_back(sql, r, name, job, chunk, claim, true, now)
}

/// The reply to an isolation run's progress: whether the claim is still held,
/// whether the cohort paused (start no further document), and whether the
/// job's artifact failed meanwhile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsoReply {
    pub held: bool,
    pub paused: bool,
    pub artifact_failed: bool,
}

/// Records outcomes an isolation attempt resolved (one document each, as soon
/// as it is resolved) under the held claim, and renews its lease. A document
/// is recorded once; the commit must carry these outcomes unchanged.
#[allow(clippy::too_many_arguments)]
pub fn iso_progress(sql: &impl Sql, r: &LaneRef, name: &str, job: i64, chunk: i64, claim: i64, attempt: u8, outcomes: &[TaskOutcome], now: u64) -> R<IsoReply> {
    touch(sql, r, now)?;
    let ch = sql.exec("SELECT inflight, first_task, count FROM chunks WHERE seq = ? AND job = ? AND chunk = ?", &[i(r.seq), i(job), i(chunk)])?;
    if !held(sql, r.seq, job, chunk, claim, name)? || ch.first().is_none_or(|c| int(c, 0).is_ok_and(|v| v != i64::from(attempt))) {
        return Ok(IsoReply::default());
    }
    let (first, count) = (int(&ch[0], 1)?, int(&ch[0], 2)?);
    let open: BTreeSet<u32> = unresolved(sql, r.seq, job, first, count)?.into_iter().map(|t| t.0).collect();
    for o in outcomes {
        if !open.contains(&o.task) || o.artifact_failed || (o.isolated_crash && o.class != 2) {
            return Err(EvalError::State("isolation progress for a task the claim does not hold"));
        }
        let body = serde_json::to_string(o).map_err(|_| EvalError::State("iso_done outcome"))?;
        let prior = sql.exec(
            "SELECT outcome FROM iso_done WHERE seq = ? AND job = ? AND chunk = ? AND attempt = ? AND task = ?",
            &[i(r.seq), i(job), i(chunk), i(i64::from(attempt)), i(i64::from(o.task))],
        )?;
        match prior.first() {
            Some(p) if text(p, 0)? == body => {}
            Some(_) => return Err(EvalError::State("a document's isolation outcome is recorded already")),
            None => {
                sql.exec(
                    "INSERT INTO iso_done (seq, job, chunk, attempt, task, outcome) VALUES (?, ?, ?, ?, ?, ?)",
                    &[i(r.seq), i(job), i(chunk), i(i64::from(attempt)), i(i64::from(o.task)), s(body)],
                )?;
            }
        }
    }
    chunk_renew(sql, r, name, job, chunk, claim, now)?;
    let c = cohort(sql, r.seq)?;
    let flagged = sql.exec("SELECT artifact_failed FROM jobs WHERE seq = ? AND job = ?", &[i(r.seq), i(job)])?;
    Ok(IsoReply {
        held: true,
        paused: c.fault.is_some() || !live(sql, now)?,
        artifact_failed: flagged.first().is_some_and(|f| opt_text(f, 0).is_some()),
    })
}

/// Hands back an isolation attempt unfinished (its step budget ran out, the
/// cohort paused, or its instance could not be restarted): the attempt number
/// stays, its recorded outcomes stay, and the next claim continues it.
#[allow(clippy::too_many_arguments)]
pub fn iso_yield(sql: &impl Sql, r: &LaneRef, name: &str, job: i64, chunk: i64, claim: i64, attempt: u8, failure: bool, now: u64) -> R<bool> {
    touch(sql, r, now)?;
    let ch = sql.exec("SELECT inflight FROM chunks WHERE seq = ? AND job = ? AND chunk = ?", &[i(r.seq), i(job), i(chunk)])?;
    if !held(sql, r.seq, job, chunk, claim, name)? || ch.first().is_none_or(|c| int(c, 0).is_ok_and(|v| v != i64::from(attempt))) {
        return Ok(false);
    }
    let until = bind_u64(now + if failure { NOT_DISPATCHED_BACKOFF_MS } else { 0 })?;
    sql.exec(
        "UPDATE chunks SET iso_yield = inflight, claim_until_ms = ?, holder = NULL, claim_kind = 'none'
         WHERE seq = ? AND job = ? AND chunk = ?",
        &[until, i(r.seq), i(job), i(chunk)],
    )?;
    // `iso_yield`: the step budget, a pause or an artifact failure ended the run;
    // `iso_yield_failure`: a restart or request failed (a scorer failure).
    event(sql, r.seq, &r.kind, r.lane, if failure { "iso_yield_failure" } else { "iso_yield" }, 0, now)?;
    if failure {
        engine::note_scorer_failure(sql, r.seq, now)?;
    }
    Ok(true)
}

/// An attributable artifact failure on a fresh instance: a load that failed or
/// outlasted its allowance, a control document that killed a freshly restarted
/// instance, or (with `definitive`) a manifest over the declared size bound,
/// not matching its package digest, or refused by the scorer's admission
/// (`refused <code>`, other than a code that can also be a host fault). For
/// an entrant it counts toward
/// `artifact_failures_before_model_error`, after which the job's unresolved
/// documents are model errors; a reference artifact is never attributed, and
/// its failure counts as a scorer failure instead. Returns whether the job's
/// artifact is now recorded as failed.
pub fn artifact_failure(sql: &impl Sql, r: &LaneRef, job: i64, code: &str, definitive: bool, now: u64) -> R<bool> {
    if fence(sql, r)?.is_none() {
        return Ok(false);
    }
    touch(sql, r, now)?;
    let c = cohort(sql, r.seq)?;
    let f = c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    let row = sql.exec("SELECT role, artifact_failed, status FROM jobs WHERE seq = ? AND job = ?", &[i(r.seq), i(job)])?;
    let Some(row) = row.first() else { return Ok(false) };
    if opt_text(row, 1).is_some() {
        return Ok(true);
    }
    event(sql, r.seq, &r.kind, r.lane, "artifact_failure", 0, now)?;
    if text(row, 0)? != "entrant" || matches!(text(row, 2)?, "done" | "faulted") {
        if c.phase == "scoring" {
            engine::note_scorer_failure(sql, r.seq, now)?;
        }
        return Ok(false);
    }
    let n = int(
        &sql.exec(
            "UPDATE jobs SET artifact_failures = artifact_failures + 1 WHERE seq = ? AND job = ? RETURNING artifact_failures",
            &[i(r.seq), i(job)],
        )?[0],
        0,
    )?;
    let bound = i64::from(f.score.limits().artifact_failures_before_model_error);
    if !definitive && n < bound {
        return Ok(false);
    }
    let code = textgen_safe(code);
    sql.exec("UPDATE jobs SET artifact_failed = ? WHERE seq = ? AND job = ?", &[s(&code), i(r.seq), i(job)])?;
    let o = engine::observed(sql)?;
    sql.exec(
        "INSERT INTO phase_log (seq, phase, at_ms, confirmed_number, epoch_index, note)
         SELECT seq, phase, ?, ?, ?, ? FROM cohorts WHERE seq = ?",
        &[
            bind_u64(now)?,
            o.confirmed.map_or(SqlValue::Null, |c| i(c.number as i64)),
            o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)),
            s(format!("job {job} artifact_failed:{code} after {n}; its unresolved documents are model errors")),
            i(r.seq),
        ],
    )?;
    Ok(true)
}

// ---------------------------------------------------------------- generation

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GenClaimReply {
    Items { items: Vec<GenItem>, wants: BTreeMap<String, u32> },
    Wait { ms: u64, wants: BTreeMap<String, u32> },
    Done,
    Paused,
    Fenced,
}

fn gen_item(sql: &impl Sql, c: &Cohort, seed: &[u8; 32], r: &Row) -> R<GenItem> {
    let idx = int(r, 0)?;
    let kind = text(r, 1)?.to_owned();
    let domain = text(r, 2)?.to_owned();
    let spec = text(r, 4)?;
    let prompt = if kind == "mixed" {
        let spec: MixedSpec = serde_json::from_str(spec).map_err(|_| EvalError::State("spec"))?;
        let pt = sql.exec("SELECT text FROM docs WHERE seq = ? AND idx = ?", &[i(c.seq), i(int(r, 3)?)])?;
        let pt = text(pt.first().ok_or(EvalError::State("parent"))?, 0)?.to_owned();
        let passage = pt.get(spec.start..spec.end).ok_or(EvalError::State("span"))?;
        match (&spec.voice, &spec.operation) {
            (Some(voice), Some(operation)) => textgen::rewrite_prompt_variant(&domain, passage, voice, operation),
            _ => textgen::rewrite_prompt(&domain, passage),
        }
    } else {
        let spec: ScratchSpec = serde_json::from_str(spec).map_err(|_| EvalError::State("spec"))?;
        textgen::scratch_prompt(&domain, &spec.topic, spec.words)
    };
    let _ = seed;
    Ok(GenItem { idx, kind, generator: text(r, 6)?.to_owned(), prompt, attempt: int(r, 5)?, claim: 0 })
}

fn gen_wants(sql: &impl Sql, seq: i64, now: u64) -> R<BTreeMap<String, u32>> {
    let mut out = BTreeMap::new();
    for r in sql.exec(
        "SELECT generator_next, COUNT(*) FROM docs WHERE seq = ? AND status = 'pending' AND claim_until_ms <= ?
           AND not_before_ms <= ? AND generator_next IS NOT NULL GROUP BY generator_next",
        &[i(seq), bind_u64(now)?, bind_u64(now)?],
    )? {
        out.insert(text(&r, 0)?.to_owned(), int(&r, 1)? as u32);
    }
    Ok(out)
}

/// A generation lane's claim, against the rate-limiter grants it holds: items
/// whose next generator is granted, each reserving its upper-bound cost. The
/// reply names the models with claimable work, for the lane's next acquire.
pub fn gen_claim(sql: &impl Sql, cfg: &EvalConfig, r: &LaneRef, name: &str, grants: &BTreeMap<String, u32>, now: u64) -> R<GenClaimReply> {
    if fence(sql, r)?.is_none() {
        return Ok(GenClaimReply::Fenced);
    }
    touch(sql, r, now)?;
    let c = cohort(sql, r.seq)?;
    if c.fault.is_some() || !live(sql, now)? {
        return Ok(GenClaimReply::Paused);
    }
    if c.phase != "generating" {
        return Ok(GenClaimReply::Done);
    }
    let f = c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    let seed = cohort_seed(&cfg.private_seed, &c.label);
    let reserved_row = &sql.exec("SELECT gen_reserved_microusd FROM cohorts WHERE seq = ?", &[i(r.seq)])?[0];
    let mut reserved = uint(reserved_row, 0)?;
    let mut items = Vec::new();
    let mut refused_budget = false;
    'models: for (model, n) in grants {
        let rows = sql.exec(
            "SELECT idx, kind, domain, parent, spec, attempts, generator_next, reserved_microusd FROM docs
             WHERE seq = ? AND status = 'pending' AND generator_next = ? AND claim_until_ms <= ? AND not_before_ms <= ?
             ORDER BY idx LIMIT ?",
            &[i(r.seq), s(model), bind_u64(now)?, bind_u64(now)?, i(i64::from(*n))],
        )?;
        for row in &rows {
            let mut it = gen_item(sql, &c, &seed, row)?;
            let (pi, po) = textgen::price(&it.generator).unwrap_or((1_000_000, 1_000_000));
            let bound = (engine::BUDGET_PROMPT_TOKENS * pi + u64::from(it.prompt.max_tokens) * po).div_ceil(1_000_000);
            // A stale claim's reservation is replaced, never added to.
            let stale = uint(row, 7)?;
            if c.gen_cost_microusd + reserved.saturating_sub(stale) + bound > c.plan.generation_budget_microusd {
                refused_budget = true;
                break 'models;
            }
            reserved = reserved.saturating_sub(stale) + bound;
            let claim = sql.exec(
                "UPDATE docs SET claim_gen = claim_gen + 1, claim_until_ms = ?, holder = ?, reserved_microusd = ?
                 WHERE seq = ? AND idx = ? RETURNING claim_gen",
                &[bind_u64(now + f.generation.lease_ms)?, s(name), bind_u64(bound)?, i(r.seq), i(it.idx)],
            )?;
            it.claim = int(&claim[0], 0)?;
            items.push(it);
        }
    }
    sql.exec("UPDATE cohorts SET gen_reserved_microusd = ? WHERE seq = ?", &[bind_u64(reserved)?, i(r.seq)])?;
    let wants = gen_wants(sql, r.seq, now)?;
    if !items.is_empty() {
        return Ok(GenClaimReply::Items { items, wants });
    }
    if refused_budget && reserved == 0 {
        // Not one more item fits and nothing in flight can release budget.
        fault(sql, r.seq, "generation_budget", now)?;
        return Ok(GenClaimReply::Paused);
    }
    let pending = int(&sql.exec("SELECT COUNT(*) FROM docs WHERE seq = ? AND status = 'pending'", &[i(r.seq)])?[0], 0)?;
    if pending == 0 {
        return Ok(GenClaimReply::Done);
    }
    if !wants.is_empty() && !refused_budget {
        // Work exists for models this lane holds no grant for.
        return Ok(GenClaimReply::Wait { ms: 0, wants });
    }
    let next = sql.exec(
        "SELECT MIN(MAX(claim_until_ms, not_before_ms)) FROM docs WHERE seq = ? AND status = 'pending'",
        &[i(r.seq)],
    )?;
    let at = next.first().and_then(|row| opt_int(row, 0)).unwrap_or(0) as u64;
    Ok(GenClaimReply::Wait { ms: at.saturating_sub(now).clamp(500, 30_000), wants })
}

/// Commits a generation lane's results (fenced per item by its claim token).
pub fn gen_commit(sql: &impl Sql, cfg: &EvalConfig, r: &LaneRef, results: &[GenOutcome], now: u64) -> R<Value> {
    let exists = !sql.exec("SELECT 1 FROM lanes WHERE seq = ? AND kind = 'gen' AND lane = ?", &[i(r.seq), i(r.lane)])?.is_empty();
    if !exists {
        return Err(EvalError::State("no such generation lane"));
    }
    touch(sql, r, now)?;
    for g in results {
        event(sql, r.seq, "gen", r.lane, if matches!(g.output, Err(textgen::Reject::Throttled)) { "gen_throttled" } else { "gen_call" }, g.ms, now)?;
    }
    sql.exec("UPDATE lanes SET chunks = chunks + ? WHERE seq = ? AND kind = 'gen' AND lane = ?", &[i(results.len() as i64), i(r.seq), i(r.lane)])?;
    engine::apply_generation(sql, cfg, r.seq, results, now)
}

// --------------------------------------------------------- summaries, bundles

/// A summary lane's report: the cohort recomputes the job's row count and
/// digest from its own rows before accepting the summary object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryReport {
    pub job: i64,
    pub key: String,
    pub sha256: String,
    pub count: u32,
    pub rows_sha256: String,
}

pub fn summary_done(sql: &impl Sql, r: &LaneRef, rep: &SummaryReport, now: u64) -> R<bool> {
    if fence(sql, r)?.is_none() {
        return Ok(false);
    }
    touch(sql, r, now)?;
    let c = cohort(sql, r.seq)?;
    if c.phase != "packaging" {
        return Ok(false);
    }
    let (count, digest) = engine::job_rows_digest(sql, r.seq, rep.job)?;
    let tasks = sql
        .exec("SELECT tasks FROM jobs WHERE seq = ? AND job = ?", &[i(r.seq), i(rep.job)])?
        .first()
        .and_then(|row| opt_int(row, 0))
        .map_or(c.task_count.unwrap_or(0), |t| t as u32);
    if count != rep.count || digest != rep.rows_sha256 || count != tasks {
        return Err(EvalError::State("summary does not match the job's rows"));
    }
    sql.exec(
        "UPDATE jobs SET summary_key = ?, summary_sha256 = ?, summary_count = ? WHERE seq = ? AND job = ?",
        &[s(&rep.key), s(&rep.sha256), i(i64::from(count)), i(r.seq), i(rep.job)],
    )?;
    sql.exec("UPDATE lanes SET status = 'done', final = 1 WHERE seq = ? AND kind = ? AND lane = ?", &[i(r.seq), s(&r.kind), i(r.lane)])?;
    Ok(true)
}

/// A job's referenced receipt objects (key, SHA-256, bytes, receipts), in key
/// order: a primary-key range over the job's key prefix (every committed
/// object of job `j` is `<prefix>/receipts/job-<jjj>/...`; bundles are never
/// committed objects).
pub fn job_objects(sql: &impl Sql, seq: i64, job: i64) -> R<Vec<(String, String, i64, i64)>> {
    let c = cohort(sql, seq)?;
    let lo = format!("{}/receipts/job-{job:03}/", engine::receipt_prefix(&c.label));
    let hi = format!("{}/receipts/job-{job:03}0", engine::receipt_prefix(&c.label));
    sql.exec(
        "SELECT key, sha256, bytes, receipts FROM receipt_objects WHERE seq = ? AND key >= ? AND key < ? ORDER BY key",
        &[i(seq), s(lo), s(hi)],
    )?
    .iter()
    .map(|r| Ok((text(r, 0)?.to_owned(), text(r, 1)?.to_owned(), int(r, 2)?, int(r, 3)?)))
    .collect()
}

/// A page of a job's receipt rows: (attempt id, SHA-256, length).
pub fn job_receipts(sql: &impl Sql, seq: i64, job: i64, after: i64, limit: i64) -> R<Vec<(i64, String, String, i64)>> {
    sql.exec(
        "SELECT rowid, attempt_id, object_sha256, object_len FROM receipts WHERE seq = ? AND job = ? AND rowid > ?
         ORDER BY rowid LIMIT ?",
        &[i(seq), i(job), i(after), i(limit.clamp(1, 5000))],
    )?
    .iter()
    .map(|r| Ok((int(r, 0)?, text(r, 1)?.to_owned(), text(r, 2)?.to_owned(), int(r, 3)?)))
    .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleReport {
    pub job: i64,
    pub key: String,
    pub sha256: String,
    pub bytes: u64,
    pub receipts: u64,
}

pub fn bundle_done(sql: &impl Sql, r: &LaneRef, rep: &BundleReport, now: u64) -> R<bool> {
    if fence(sql, r)?.is_none() {
        return Ok(false);
    }
    touch(sql, r, now)?;
    let c = cohort(sql, r.seq)?;
    if c.phase != "packaging" {
        return Ok(false);
    }
    let n = int(&sql.exec("SELECT COUNT(*) FROM receipts WHERE seq = ? AND job = ?", &[i(r.seq), i(rep.job)])?[0], 0)?;
    let objects = job_objects(sql, r.seq, rep.job)?;
    let bytes: i64 = objects.iter().map(|o| o.2).sum();
    if n as u64 != rep.receipts || bytes as u64 != rep.bytes {
        return Err(EvalError::State("bundle does not hold the job's receipts"));
    }
    sql.exec(
        "UPDATE jobs SET bundle_key = ?, bundle_sha256 = ?, bundle_bytes = ?, bundle_receipts = ? WHERE seq = ? AND job = ?",
        &[s(&rep.key), s(&rep.sha256), bind_u64(rep.bytes)?, bind_u64(rep.receipts)?, i(r.seq), i(rep.job)],
    )?;
    sql.exec("UPDATE lanes SET status = 'done', final = 1 WHERE seq = ? AND kind = ? AND lane = ?", &[i(r.seq), s(&r.kind), i(r.lane)])?;
    Ok(true)
}

// --------------------------------------------------------------------- spend

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Spend {
    /// Reported metering, priced by the plan's cost model.
    pub measured_nanousd: u128,
    /// For lanes whose last report may not be their last: the bound on what
    /// they spent since, with every vCPU busy, up to the staleness bound plus
    /// the container's inactivity grace.
    pub unreported_nanousd: u128,
}

/// The cohort's lane spend of `kinds`. Lanes report cumulative metering with
/// every report and commit, and the remaining work's bound covers what live
/// lanes still spend. A lane presumed dead (silent past its staleness bound)
/// lost its unreported metering, so it is charged a bound from the cohort's
/// own records: provisioned, every vCPU busy, from its last report through the
/// staleness bound plus its container's inactivity grace. `settling` charges
/// that bound to every lane not known to have stopped.
pub fn spend(sql: &impl Sql, c: &Cohort, kinds: &[&str], now: u64, settling: bool) -> R<Spend> {
    let Ok(f) = c.plan.fanout() else { return Ok(Spend::default()) };
    let stale = 3 * f.score.lease_ms;
    let mut out = Spend::default();
    for kind in kinds {
        let rows = sql.exec(
            "SELECT active_ms, cpu_ms, do_ms, seen_ms, first_seen_ms, final, container_up FROM lanes
             WHERE seq = ? AND kind = ?",
            &[i(c.seq), s(*kind)],
        )?;
        for r in &rows {
            out.measured_nanousd += cost::lane_cost_nanousd(uint(r, 0)?, uint(r, 1)?, uint(r, 2)?, &f.cost);
            let open = *kind == "score" && uint(r, 4)? > 0 && int(r, 5)? == 0;
            let silent = now.saturating_sub(uint(r, 3)?);
            if open && (settling || silent > stale) {
                out.unreported_nanousd += cost::unreported_bound_nanousd(silent.min(stale) + f.score.inactivity_ms, &f.cost);
            }
        }
    }
    Ok(out)
}

/// A finished cohort whose measured spend has not been charged to the ledger.
pub fn settle_due(sql: &impl Sql, cfg: &EvalConfig, now: u64) -> R<Option<Work>> {
    let rows = sql.exec(
        "SELECT seq, label, gen_cost_microusd FROM cohorts WHERE phase IN ('transition','closed','abandoned')
           AND reserved_microusd IS NOT NULL AND settled_microusd IS NULL ORDER BY seq LIMIT 1",
        &[],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    let seq = int(r, 0)?;
    let c = cohort(sql, seq)?;
    let sp = spend(sql, &c, &KINDS, now, true)?;
    let total = uint(r, 2)? + cost::nano_to_micro_ceil(sp.measured_nanousd + sp.unreported_nanousd);
    Ok(Some(Work::Settle { seq, key: format!("{}:{}", cfg.instance, text(r, 1)?), microusd: total }))
}

/// A closed or abandoned cohort whose private results objects and summaries
/// are still in the eval bucket. Cohorts without lanes have none: they are
/// marked purged at once.
pub fn purge_due(sql: &impl Sql, now: u64) -> R<Option<Work>> {
    let rows = sql.exec(
        "SELECT seq, label FROM cohorts WHERE phase IN ('closed','abandoned') AND purged_ms IS NULL ORDER BY seq",
        &[],
    )?;
    for r in &rows {
        let seq = int(r, 0)?;
        if cohort(sql, seq)?.plan.fanout.is_none() {
            mark_purged(sql, seq, now)?;
            continue;
        }
        let label = text(r, 1)?;
        return Ok(Some(Work::Purge {
            seq,
            prefixes: vec![format!("cohorts/{label}/results/"), format!("cohorts/{label}/summaries/")],
        }));
    }
    Ok(None)
}

pub fn mark_purged(sql: &impl Sql, seq: i64, now: u64) -> R<()> {
    sql.exec("UPDATE cohorts SET purged_ms = ? WHERE seq = ?", &[bind_u64(now)?, i(seq)])?;
    Ok(())
}

pub fn mark_settled(sql: &impl Sql, seq: i64, microusd: u64) -> R<()> {
    sql.exec("UPDATE cohorts SET settled_microusd = ? WHERE seq = ?", &[bind_u64(microusd)?, i(seq)])?;
    Ok(())
}

// -------------------------------------------------------------------- status

fn percentiles(sql: &impl Sql, seq: i64, ev: &str) -> R<Value> {
    let rows = sql.exec("SELECT ms FROM lane_events WHERE seq = ? AND event = ? ORDER BY ms", &[i(seq), s(ev)])?;
    let v: Vec<i64> = rows.iter().filter_map(|r| int(r, 0).ok()).collect();
    if v.is_empty() {
        return Ok(json!({"n": 0}));
    }
    let q = |p: f64| v[((v.len() as f64 - 1.0) * p).round() as usize];
    Ok(json!({"n": v.len(), "p50": q(0.5), "p95": q(0.95), "max": v[v.len() - 1], "min": v[0]}))
}

/// Content-free lane status of a cohort: counts by kind and status, cold-path
/// and chunk-time percentiles, lost and reconciled attempts, kicks, throttles,
/// and spend against the budget.
pub fn lane_status(sql: &impl Sql, c: &Cohort, now: u64) -> R<Value> {
    let mut by_kind = serde_json::Map::new();
    for kind in KINDS {
        let rows = sql.exec(
            "SELECT status, COUNT(*), SUM(kicks), SUM(rekicks), SUM(starts), SUM(start_failures), SUM(chunks), SUM(lost),
                    SUM(reconciled), SUM(fenced), SUM(active_ms), SUM(cpu_ms), SUM(do_ms), SUM(container_up)
             FROM lanes WHERE seq = ? AND kind = ? GROUP BY status",
            &[i(c.seq), s(kind)],
        )?;
        if rows.is_empty() {
            continue;
        }
        let mut counts = serde_json::Map::new();
        let mut tot = [0i64; 12];
        for r in &rows {
            counts.insert(text(r, 0)?.to_owned(), json!(int(r, 1)?));
            for (k, t) in tot.iter_mut().enumerate() {
                *t += opt_int(r, k + 2).unwrap_or(0);
            }
        }
        by_kind.insert(kind.into(), json!({
            "lanes": rows.iter().map(|r| int(r, 1).unwrap_or(0)).sum::<i64>(), "by_status": counts,
            "kicks": tot[0], "rekicks": tot[1], "starts": tot[2], "start_failures": tot[3], "work_items": tot[4],
            "lost_attempts": tot[5], "reconciled": tot[6], "fenced_commits": tot[7], "active_ms": tot[8],
            "cpu_ms": tot[9], "do_ms": tot[10], "containers_up": tot[11],
        }));
    }
    let sp = spend(sql, c, &["score"], now, false)?;
    let all = spend(sql, c, &KINDS, now, true)?;
    let row = &sql.exec(
        "SELECT gen_reserved_microusd, gen_started_ms, scoring_started_ms, scoring_finished_ms, prestarted_ms,
                reserved_microusd, settled_microusd FROM cohorts WHERE seq = ?",
        &[i(c.seq)],
    )?[0];
    let errors: Vec<Value> = sql
        .exec(
            "SELECT kind, note, COUNT(*) FROM lanes WHERE seq = ? AND note IS NOT NULL GROUP BY kind, note ORDER BY 3 DESC LIMIT 10",
            &[i(c.seq)],
        )?
        .iter()
        .map(|r| json!({"kind": text(r, 0).unwrap_or(""), "note": text(r, 1).unwrap_or(""), "lanes": int(r, 2).unwrap_or(0)}))
        .collect();
    // M12: a lane's round time, the spacing of its successive chunk commits
    // (the call, the two puts, the claim and the commit).
    let mut rounds: Vec<i64> = sql
        .exec(
            "SELECT (MAX(at_ms) - MIN(at_ms)) / (COUNT(*) - 1) FROM lane_events
             WHERE seq = ? AND event = 'chunk' GROUP BY kind, lane HAVING COUNT(*) > 1",
            &[i(c.seq)],
        )?
        .iter()
        .filter_map(|r| int(r, 0).ok())
        .collect();
    rounds.sort_unstable();
    let round_ms = if rounds.is_empty() {
        json!({"n": 0})
    } else {
        let q = |p: f64| rounds[((rounds.len() as f64 - 1.0) * p).round() as usize];
        json!({"n": rounds.len(), "p50": q(0.5), "p95": q(0.95), "max": rounds[rounds.len() - 1], "min": rounds[0]})
    };
    let hosts: BTreeMap<String, i64> = sql
        .exec(
            "SELECT event, COUNT(*) FROM lane_events WHERE seq = ? AND event LIKE 'host:%' GROUP BY event",
            &[i(c.seq)],
        )?
        .iter()
        .filter_map(|r| Some((text(r, 0).ok()?.trim_start_matches("host:").to_owned(), int(r, 1).ok()?)))
        .collect();
    // Hand-backs and attribution, by event (review of 2026-10-31).
    let events: BTreeMap<String, i64> = sql
        .exec(
            "SELECT event, COUNT(*) FROM lane_events WHERE seq = ?
               AND event IN ('given_back','iso_yield','iso_yield_failure','iso_resumed','artifact_failure','lost','reconciled')
             GROUP BY event",
            &[i(c.seq)],
        )?
        .iter()
        .filter_map(|r| Some((text(r, 0).ok()?.to_owned(), int(r, 1).ok()?)))
        .collect();
    let mut by_job: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for r in &sql.exec(
        "SELECT e.event, l.job, COUNT(*) FROM lane_events e JOIN lanes l ON l.seq = e.seq AND l.kind = e.kind AND l.lane = e.lane
         WHERE e.seq = ? AND e.kind = 'score'
           AND e.event IN ('given_back','iso_yield','iso_yield_failure','iso_resumed','artifact_failure')
         GROUP BY e.event, l.job",
        &[i(c.seq)],
    )? {
        if let (Ok(ev), Some(job), Ok(n)) = (text(r, 0), opt_int(r, 1), int(r, 2)) {
            by_job.entry(job.to_string()).or_default().insert(ev.to_owned(), n);
        }
    }
    Ok(json!({
        "kinds": by_kind,
        "events": events,
        "events_by_job": by_job,
        "hosts": hosts,
        "cold_ms": percentiles(sql, c.seq, "cold")?,
        "cold_cpu_ms": percentiles(sql, c.seq, "cold_cpu")?,
        "chunk_ms": percentiles(sql, c.seq, "chunk")?,
        "round_ms": round_ms,
        "gen_call_ms": percentiles(sql, c.seq, "gen_call")?,
        "gen_throttles": int(&sql.exec("SELECT COUNT(*) FROM lane_events WHERE seq = ? AND event = 'gen_throttled'", &[i(c.seq)])?[0], 0)?,
        "notes": errors,
        "timeline_ms": {"generation_started": opt_int(row, 1), "prestarted": opt_int(row, 4),
            "scoring_started": opt_int(row, 2), "scoring_finished": opt_int(row, 3)},
        "spend": {
            "scoring_measured_microusd": cost::nano_to_micro_ceil(sp.measured_nanousd),
            "scoring_unreported_bound_microusd": cost::nano_to_micro_ceil(sp.unreported_nanousd),
            "scoring_budget_microusd": c.plan.fanout().map(|f| f.score.scoring_budget_microusd).ok(),
            "lanes_all_microusd": cost::nano_to_micro_ceil(all.measured_nanousd + all.unreported_nanousd),
            "generation_reserved_microusd": opt_int(row, 0),
            "ledger_reserved_microusd": opt_int(row, 5),
            "ledger_settled_microusd": opt_int(row, 6),
        },
    }))
}

// ------------------------------------------------------------ scoring routes

/// A scoring lane's claim reference in a route body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRef {
    pub lane: LaneRef,
    pub name: String,
    pub job: i64,
    pub chunk: i64,
    pub claim: i64,
    #[serde(default)]
    pub attempt: u8,
    /// Count a scorer failure (give-back, release, isolation yield).
    #[serde(default)]
    pub failure: bool,
    /// Isolation progress: the documents resolved since the last call.
    #[serde(default)]
    pub outcomes: Vec<TaskOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactFailure {
    pub lane: LaneRef,
    pub job: i64,
    pub code: String,
    #[serde(default)]
    pub definitive: bool,
}

#[derive(Deserialize)]
struct LaneName {
    lane: LaneRef,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct CommitBody {
    lane: LaneRef,
    name: String,
    commit: ChunkCommit,
}

/// The scoring lane's calls to its cohort object, by route, each one
/// transaction. The Worker routes and the native simulation share it, so both
/// read the same bodies. `None` for a route that is not a scoring route.
pub fn scoring_route(sql: &impl Sql, cfg: &EvalConfig, path: &str, body: &[u8], now: u64) -> R<Option<Value>> {
    fn parse<T: serde::de::DeserializeOwned>(b: &[u8]) -> R<T> {
        serde_json::from_slice(b).map_err(|_| EvalError::Plan("bad lane request"))
    }
    fn v<T: Serialize>(x: &T) -> Value {
        serde_json::to_value(x).unwrap_or(Value::Null)
    }
    let out = match path {
        "/lane/claim" => {
            let b: LaneName = parse(body)?;
            v(&claim_chunk(sql, &b.lane, &b.name, now)?)
        }
        "/lane/commit" => {
            let b: CommitBody = parse(body)?;
            v(&commit_chunk(sql, cfg, &b.lane, &b.name, &b.commit, now)?)
        }
        "/lane/lost" => {
            let b: ClaimRef = parse(body)?;
            json!(chunk_lost(sql, &b.lane, &b.name, b.job, b.chunk, b.claim, b.attempt, now)?)
        }
        "/lane/release" => {
            let b: ClaimRef = parse(body)?;
            json!(chunk_release(sql, &b.lane, &b.name, b.job, b.chunk, b.claim, b.failure, now)?)
        }
        "/lane/renew" => {
            let b: ClaimRef = parse(body)?;
            json!(chunk_renew(sql, &b.lane, &b.name, b.job, b.chunk, b.claim, now)?)
        }
        "/lane/not-dispatched" => {
            let b: ClaimRef = parse(body)?;
            json!(chunk_not_dispatched(sql, &b.lane, &b.name, b.job, b.chunk, b.claim, now)?)
        }
        "/lane/give-back" => {
            let b: ClaimRef = parse(body)?;
            json!(chunk_give_back(sql, &b.lane, &b.name, b.job, b.chunk, b.claim, b.failure, now)?)
        }
        "/lane/iso-progress" => {
            let b: ClaimRef = parse(body)?;
            v(&iso_progress(sql, &b.lane, &b.name, b.job, b.chunk, b.claim, b.attempt, &b.outcomes, now)?)
        }
        "/lane/iso-yield" => {
            let b: ClaimRef = parse(body)?;
            json!(iso_yield(sql, &b.lane, &b.name, b.job, b.chunk, b.claim, b.attempt, b.failure, now)?)
        }
        "/lane/artifact-failure" => {
            let b: ArtifactFailure = parse(body)?;
            json!(artifact_failure(sql, &b.lane, b.job, &b.code, b.definitive, now)?)
        }
        _ => return Ok(None),
    };
    Ok(Some(out))
}
