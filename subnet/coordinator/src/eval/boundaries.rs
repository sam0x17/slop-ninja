//! The boundary journal: one row per tempo boundary (subnet epoch) the
//! engine observed, with that boundary's first block, the time it was first
//! observed and whether that observation was live, so the public dashboard
//! can date every stage of every cohort by tempo boundaries
//! (`slop_ninja.dashboard.publisher/v2`).
//!
//! Only boundary facts enter: a boundary's block (`LastEpochBlock`, a public
//! chain fact) and the minute the coordinator first saw it. A work commit's
//! own time (`phase_log.at_ms` of `generating`, `sealing`, `scoring`,
//! `packaging` or the package-commit `transition`) is a time inside a tempo
//! and never reaches the journal: work stages are dated by the boundary at
//! which they are seen, never by when their work committed.
//!
//! [`resolve`] turns rows into points for any set of epochs: observed values
//! where a row has them, otherwise values derived from neighbouring rows
//! (linear in block between observed rows, or the nominal block time from
//! one side, or by epoch between observed rows across a tempo change), and
//! `None` where nothing supports a value, with the reason.
//!
//! An observation counts as the boundary's time only when the tick that
//! stored it read the boundary within one fallback sweep of it
//! ([`TICK_SWEEP_SECS`]): a first tick later in the tempo (after coordinator
//! silence ending inside a tempo) is dated back to the boundary by the
//! nominal block time and is derived, never observed, and never an anchor.

use crate::eval::engine::{R, i, opt_int, opt_text};
use crate::sql::{Row, Sql, SqlValue, bind_u64};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The coordinator's fallback sweep period: the eval Worker's cron trigger
/// fires every minute (`[env.eval-testnet.triggers] crons = ["* * * * *"]`
/// in `wrangler.toml`; checked by a test), so a live coordinator stores a
/// new epoch within one sweep of its boundary even without block webhooks.
pub const TICK_SWEEP_SECS: u64 = 60;

/// The most blocks a live first observation can trail its boundary: one
/// sweep at the nominal block time, rounded up.
pub fn sweep_blocks(nominal_block_ms: u64) -> u64 {
    if nominal_block_ms == 0 {
        return 0;
    }
    (TICK_SWEEP_SECS * 1000).div_ceil(nominal_block_ms)
}

/// v23: the boundary journal, seeded once from the records the store already
/// keeps.
///
/// - `seed_open`: each cohort's opening (`opened_epoch`, `opened_block`,
///   `opened_ms`), except load cohorts and probe builds, which do not open
///   in a boundary step.
/// - `seed_cutoff`: each coordinator admission window's close
///   (`closed_at_epoch`, `cutoff_block`), with the time of that close's own
///   phase-log entry.
/// - `seed_phase`: the phase-log entries the engine writes in its boundary
///   step (an opening, a window close, the end of admission without
///   coordinator admission, a close), found by their fixed notes. Notes the
///   engine logs with the cohort's phase at other times (faults, clears,
///   budget guards) are not boundary steps and never seed.
/// - `seed_obs`: the stored epoch's block, without a time (`epoch_ms` is the
///   latest observation of that epoch, not its first).
///
/// `seen_block` is the watermark block of the tick that stored the epoch
/// ([`record`]); seeds have none.
///
/// Several seeds of one epoch keep the earliest time and any block.
pub const SCHEMA_V23: &[&str] = &[
    "CREATE TABLE boundary_log (
        epoch_index INTEGER PRIMARY KEY,
        boundary_block INTEGER,
        observed_ms INTEGER,
        live INTEGER,
        credited INTEGER,
        active_tempos INTEGER,
        seen_block INTEGER,
        source TEXT NOT NULL CHECK (source IN ('observe','seed_open','seed_phase','seed_cutoff','seed_obs'))
    ) STRICT",
    "INSERT INTO boundary_log (epoch_index, boundary_block, observed_ms, source)
        SELECT opened_epoch, opened_block, opened_ms, 'seed_open' FROM cohorts
        WHERE opened_epoch IS NOT NULL AND json_extract(plan, '$.load') IS NULL
          AND COALESCE(json_extract(plan, '$.schema'), '') != 'slop_ninja_eval_probe_plan_v1'
        ON CONFLICT (epoch_index) DO UPDATE SET
          boundary_block = COALESCE(boundary_log.boundary_block, excluded.boundary_block),
          observed_ms = CASE WHEN boundary_log.observed_ms IS NULL THEN excluded.observed_ms
            WHEN excluded.observed_ms IS NULL THEN boundary_log.observed_ms
            ELSE MIN(boundary_log.observed_ms, excluded.observed_ms) END",
    "INSERT INTO boundary_log (epoch_index, boundary_block, observed_ms, source)
        SELECT a.closed_at_epoch, a.cutoff_block,
          (SELECT MIN(p.at_ms) FROM phase_log p WHERE p.seq = a.seq AND p.phase = 'admission'
             AND p.epoch_index = a.closed_at_epoch AND p.note LIKE 'admission closed at a boundary%'),
          'seed_cutoff'
        FROM adm_cohorts a WHERE a.closed_at_epoch IS NOT NULL
        ON CONFLICT (epoch_index) DO UPDATE SET
          boundary_block = COALESCE(boundary_log.boundary_block, excluded.boundary_block),
          observed_ms = CASE WHEN boundary_log.observed_ms IS NULL THEN excluded.observed_ms
            WHEN excluded.observed_ms IS NULL THEN boundary_log.observed_ms
            ELSE MIN(boundary_log.observed_ms, excluded.observed_ms) END",
    "INSERT INTO boundary_log (epoch_index, observed_ms, source)
        SELECT epoch_index, at_ms, 'seed_phase' FROM phase_log
        WHERE epoch_index IS NOT NULL AND (
          (phase = 'admission' AND (note LIKE 'opened at an epoch boundary%' OR note LIKE 'admission closed at a boundary%'))
          OR (phase = 'generating' AND note = 'admission minimum reached')
          OR (phase = 'closed' AND (note LIKE 'closed at a boundary%' OR note = 'target active at this boundary')))
        ON CONFLICT (epoch_index) DO UPDATE SET
          observed_ms = CASE WHEN boundary_log.observed_ms IS NULL THEN excluded.observed_ms
            WHEN excluded.observed_ms IS NULL THEN boundary_log.observed_ms
            ELSE MIN(boundary_log.observed_ms, excluded.observed_ms) END",
    "INSERT INTO boundary_log (epoch_index, boundary_block, source)
        SELECT epoch_index, last_epoch_block, 'seed_obs' FROM eval_obs WHERE id = 1 AND epoch_index IS NOT NULL
        ON CONFLICT (epoch_index) DO UPDATE SET
          boundary_block = COALESCE(boundary_log.boundary_block, excluded.boundary_block)",
];

/// Where a journal row came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Written by [`record`] when the engine stored a new epoch.
    Observe,
    /// The engine's own boundary-step records, seeded by v23.
    SeedOpen,
    SeedPhase,
    SeedCutoff,
    /// The stored epoch at v23: a block, no time.
    SeedObs,
}

impl Source {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "observe" => Self::Observe,
            "seed_open" => Self::SeedOpen,
            "seed_phase" => Self::SeedPhase,
            "seed_cutoff" => Self::SeedCutoff,
            "seed_obs" => Self::SeedObs,
            _ => return None,
        })
    }

    /// A seed from the engine's boundary step: its time is that step's
    /// first observation of the boundary.
    fn step(self) -> bool {
        matches!(self, Self::SeedOpen | Self::SeedPhase | Self::SeedCutoff)
    }
}

/// One `boundary_log` row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundaryRow {
    pub epoch: u64,
    pub block: Option<u64>,
    pub observed_ms: Option<u64>,
    /// `Some(true)`: observed while it was the head's epoch; `Some(false)`:
    /// stored while catching up; `None`: a seed.
    pub live: Option<bool>,
    pub credited: Option<bool>,
    /// The credited-tempo count after this boundary (`eval_obs.active_tempos`).
    pub active: Option<u64>,
    /// The watermark block the storing tick read the epoch at.
    pub seen_block: Option<u64>,
    pub source: Source,
}

impl BoundaryRow {
    /// The boundary's time from this row: a boundary-step seed's time, or a
    /// live observation's time when its tick read the boundary within one
    /// sweep (observed). A live observation later in the tempo is dated back
    /// by its block lag at the nominal block time (derived). A caught-up
    /// observation gives nothing.
    fn own_time(&self, nominal_block_ms: u64) -> Option<(u64, Basis)> {
        let ms = self.observed_ms?;
        if self.source.step() {
            return Some((ms, Basis::Observed));
        }
        if self.live != Some(true) {
            return None;
        }
        let lag = self.seen_block.zip(self.block).map(|(s, b)| s.saturating_sub(b));
        match lag {
            Some(l) if l > sweep_blocks(nominal_block_ms) => {
                Some((ms.checked_sub(l.checked_mul(nominal_block_ms)?)?, Basis::Derived))
            }
            _ => Some((ms, Basis::Observed)),
        }
    }

    /// The row's observed time, if any: the only times that anchor others.
    fn observed_time(&self, nominal_block_ms: u64) -> Option<u64> {
        self.own_time(nominal_block_ms)
            .and_then(|(t, b)| (b == Basis::Observed).then_some(t))
    }
}

/// How a value is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    Observed,
    Derived,
}

impl Basis {
    pub fn code(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Derived => "derived",
        }
    }
}

/// A resolved boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundaryPoint {
    pub epoch: u64,
    pub block: Option<u64>,
    pub block_basis: Option<Basis>,
    pub at_ms: Option<u64>,
    pub at_basis: Option<Basis>,
    /// The journal's credited-tempo count at this boundary, when its row
    /// records one.
    pub active: Option<u64>,
    /// Why `at_ms` is `None`.
    pub at_reason: Option<Unresolved>,
}

/// Why a boundary has no time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unresolved {
    /// Below the journal's first row (or no rows at all): the engine holds no
    /// record of the boundary.
    BeforeTracking,
    /// The journal has rows around it, but none supports a time.
    NotDerivable,
}

impl Unresolved {
    pub fn code(self) -> &'static str {
        match self {
            Self::BeforeTracking => "before_tracking",
            Self::NotDerivable => "not_derivable",
        }
    }
}

impl BoundaryPoint {
    pub fn json(&self) -> Value {
        json!({
            "epoch": self.epoch, "block": self.block, "block_basis": self.block_basis.map(Basis::code),
            "at_ms": self.at_ms, "at_basis": self.at_basis.map(Basis::code), "active": self.active,
            "at_reason": self.at_reason.map(Unresolved::code),
        })
    }
}

/// Blocks per epoch between two rows, when it is a whole number.
fn stride(e1: u64, b1: u64, e2: u64, b2: u64) -> Option<u64> {
    let (de, db) = (e2.checked_sub(e1)?, b2.checked_sub(b1)?);
    (de > 0 && db % de == 0).then_some(db / de)
}

/// `a + (x - xa) * (b - a) / (xb - xa)` for `xa <= x <= xb`, floored.
fn lerp(xa: u64, a: u64, xb: u64, b: u64, x: u64) -> Option<u64> {
    if xb <= xa || x < xa || x > xb || b < a {
        return None;
    }
    let v = u128::from(a) + u128::from(x - xa) * u128::from(b - a) / u128::from(xb - xa);
    u64::try_from(v).ok()
}

/// Resolves every epoch of `epochs` against the journal `rows` (any order).
///
/// 1. Block: the row's own block (observed); otherwise linear between the
///    nearest rows with blocks below and above when their block span is a
///    whole number of strides; from one side, the nearest row and the
///    stride of the latest two rows with blocks (derived); otherwise none.
/// 2. Time: the row's own time ([`BoundaryRow::own_time`]: observed, or
///    derived for a live observation late in its tempo); otherwise, with a
///    block, linear in block between the nearest observed times below and
///    above, or from one side the nearest observed time plus the block
///    distance at `nominal_block_ms`; without a block (a tempo change
///    between the rows), linear in epoch between the nearest observed times
///    below and above (derived); otherwise none, with the reason. A row
///    stored while catching up has an observed block and a derived time.
///
/// An epoch below the first row ("before tracking began") resolves only by
/// arithmetic from the rows above it, and to nothing without them.
pub fn resolve(rows: &[BoundaryRow], epochs: &BTreeSet<u64>, nominal_block_ms: u64) -> Vec<BoundaryPoint> {
    let mut rows: Vec<BoundaryRow> = rows.to_vec();
    rows.sort_by_key(|r| r.epoch);
    let with_block: Vec<(u64, u64)> = rows.iter().filter_map(|r| Some((r.epoch, r.block?))).collect();
    let latest_stride = match with_block.as_slice() {
        [.., (e1, b1), (e2, b2)] => stride(*e1, *b1, *e2, *b2),
        _ => None,
    };
    let block_of = |e: u64| -> (Option<u64>, Option<Basis>) {
        if let Ok(k) = with_block.binary_search_by_key(&e, |(x, _)| *x) {
            return (Some(with_block[k].1), Some(Basis::Observed));
        }
        let below = with_block.iter().rev().find(|(x, _)| *x < e).copied();
        let above = with_block.iter().find(|(x, _)| *x > e).copied();
        let v = match (below, above) {
            (Some((e1, b1)), Some((e2, b2))) => {
                stride(e1, b1, e2, b2).and_then(|st| b1.checked_add((e - e1).checked_mul(st)?))
            }
            (Some((e1, b1)), None) => latest_stride.and_then(|st| b1.checked_add((e - e1).checked_mul(st)?)),
            (None, Some((e2, b2))) => latest_stride.and_then(|st| b2.checked_sub((e2 - e).checked_mul(st)?)),
            (None, None) => None,
        };
        (v, v.map(|_| Basis::Derived))
    };
    // Time anchors: rows with an observed time and a block (their own or
    // derived), ascending by block.
    let anchors: Vec<(u64, u64)> = rows
        .iter()
        .filter_map(|r| Some((block_of(r.epoch).0?, r.observed_time(nominal_block_ms)?)))
        .collect();
    // The same rows by epoch, for boundaries without a block.
    let by_epoch: Vec<(u64, u64)> = rows
        .iter()
        .filter_map(|r| Some((r.epoch, r.observed_time(nominal_block_ms)?)))
        .collect();
    let first = rows.first().map(|r| r.epoch);
    epochs
        .iter()
        .map(|&e| {
            let row = rows.iter().find(|r| r.epoch == e);
            let (block, block_basis) = block_of(e);
            let (at_ms, at_basis) = match row.and_then(|r| r.own_time(nominal_block_ms)) {
                Some((t, basis)) => (Some(t), Some(basis)),
                None => {
                    let v = match block {
                        Some(b) => {
                            let below = anchors.iter().rev().find(|(x, _)| *x <= b).copied();
                            let above = anchors.iter().find(|(x, _)| *x >= b).copied();
                            match (below, above) {
                                (Some((b1, t1)), Some((b2, t2))) if b1 == b2 => Some(t1.min(t2)),
                                (Some((b1, t1)), Some((b2, t2))) => lerp(b1, t1, b2, t2, b),
                                (Some((b1, t1)), None) => (b - b1).checked_mul(nominal_block_ms).and_then(|d| t1.checked_add(d)),
                                (None, Some((b2, t2))) => (b2 - b).checked_mul(nominal_block_ms).and_then(|d| t2.checked_sub(d)),
                                (None, None) => None,
                            }
                        }
                        None => {
                            let below = by_epoch.iter().rev().find(|(x, _)| *x < e).copied();
                            let above = by_epoch.iter().find(|(x, _)| *x > e).copied();
                            match (below, above) {
                                (Some((e1, t1)), Some((e2, t2))) => lerp(e1, t1, e2, t2, e),
                                _ => None,
                            }
                        }
                    };
                    (v, v.map(|_| Basis::Derived))
                }
            };
            let at_reason = at_ms.is_none().then(|| {
                if first.is_none_or(|f| e < f) { Unresolved::BeforeTracking } else { Unresolved::NotDerivable }
            });
            BoundaryPoint {
                epoch: e,
                block,
                block_basis,
                at_ms,
                at_basis,
                active: row.and_then(|r| r.active),
                at_reason,
            }
        })
        .collect()
}

/// Every journal row, ascending by epoch.
pub fn rows(sql: &impl Sql) -> R<Vec<BoundaryRow>> {
    let u = |r: &Row, k: usize| opt_int(r, k).and_then(|v| u64::try_from(v).ok());
    Ok(sql
        .exec(
            "SELECT epoch_index, boundary_block, observed_ms, live, credited, active_tempos, seen_block, source
             FROM boundary_log ORDER BY epoch_index",
            &[],
        )?
        .iter()
        .filter_map(|r| {
            Some(BoundaryRow {
                epoch: u(r, 0)?,
                block: u(r, 1),
                observed_ms: u(r, 2),
                live: opt_int(r, 3).map(|v| v != 0),
                credited: opt_int(r, 4).map(|v| v != 0),
                active: u(r, 5),
                seen_block: u(r, 6),
                source: Source::parse(&opt_text(r, 7)?)?,
            })
        })
        .collect())
}

/// Records the engine's first storing of epoch `epoch` (`engine::observe`,
/// in its transaction): the boundary's block, the observation time, whether
/// it was live, whether the increment credited a tempo, the credited count
/// after it and the watermark block the tick read the epoch at (`seen`). The first observation wins: an observed row is never
/// overwritten, and a seeded row is replaced by an observation. Then drops
/// the rows nothing needs any more ([`prune`]).
#[allow(clippy::too_many_arguments)]
pub fn record(
    sql: &impl Sql,
    epoch: u64,
    block: u64,
    now_ms: u64,
    live: bool,
    credited: Option<bool>,
    active_after: u64,
    seen: u64,
) -> R<()> {
    sql.exec(
        "INSERT INTO boundary_log (epoch_index, boundary_block, observed_ms, live, credited, active_tempos, seen_block, source)
            VALUES (?, ?, ?, ?, ?, ?, ?, 'observe')
         ON CONFLICT (epoch_index) DO UPDATE SET boundary_block = excluded.boundary_block,
            observed_ms = excluded.observed_ms, live = excluded.live, credited = excluded.credited,
            active_tempos = excluded.active_tempos, seen_block = excluded.seen_block, source = 'observe'
         WHERE boundary_log.source != 'observe'",
        &[
            bind_u64(epoch)?,
            bind_u64(block)?,
            bind_u64(now_ms)?,
            i(i64::from(live)),
            credited.map_or(SqlValue::Null, |c| i(i64::from(c))),
            bind_u64(active_after)?,
            bind_u64(seen)?,
        ],
    )?;
    prune(sql)
}

/// Deletes the rows no cohort's dates need. A row is kept while a cohort's
/// opening, window close, publication or close, or any phase-log entry's
/// epoch or the boundary after it, names it (a referenced epoch); every row
/// from the opening of the oldest cohort not yet closed on is kept, and the
/// latest two rows (the stride of [`resolve`]). For each referenced epoch
/// the nearest rows below and above it with a block, and with an observed
/// time, are kept too: they are the anchors [`resolve`] derives its values
/// from, so a closed cohort's derived dates never move when unreferenced
/// rows go. The journal is so bounded by the records the store keeps, with
/// no count cap.
pub fn prune(sql: &impl Sql) -> R<()> {
    let all = rows(sql)?;
    let Some(max) = all.last().map(|r| r.epoch) else { return Ok(()) };
    let u = |r: &Row| opt_int(r, 0).and_then(|v| u64::try_from(v).ok());
    let col = |q: &str| -> R<Vec<u64>> { Ok(sql.exec(q, &[])?.iter().filter_map(u).collect()) };
    let floor = col("SELECT MIN(opened_epoch) FROM cohorts WHERE phase NOT IN ('closed','abandoned') AND opened_epoch IS NOT NULL")?
        .first()
        .copied()
        .unwrap_or(max)
        .min(max.saturating_sub(1));
    let mut referenced: BTreeSet<u64> = BTreeSet::new();
    for q in [
        "SELECT opened_epoch FROM cohorts WHERE opened_epoch IS NOT NULL",
        "SELECT closed_epoch FROM cohorts WHERE closed_epoch IS NOT NULL",
        "SELECT published_epoch FROM cohorts WHERE published_epoch IS NOT NULL",
        "SELECT closed_at_epoch FROM adm_cohorts WHERE closed_at_epoch IS NOT NULL",
        "SELECT DISTINCT epoch_index FROM phase_log WHERE epoch_index IS NOT NULL",
    ] {
        referenced.extend(col(q)?);
    }
    let next: Vec<u64> = referenced.iter().filter_map(|e| e.checked_add(1)).collect();
    referenced.extend(next);
    let keep = kept(&all, &referenced, floor);
    for r in all.iter().filter(|r| !keep.contains(&r.epoch)) {
        sql.exec("DELETE FROM boundary_log WHERE epoch_index = ?", &[bind_u64(r.epoch)?])?;
    }
    Ok(())
}

/// The epochs of `all` (ascending) that [`prune`] keeps: every row from
/// `floor` on, the referenced rows, and for a referenced epoch whose own row
/// lacks a block (or a time), the nearest rows on each side with a block
/// (with an observed time): the anchors its value is derived from.
fn kept(all: &[BoundaryRow], referenced: &BTreeSet<u64>, floor: u64) -> BTreeSet<u64> {
    let ms = crate::eval::calendar::NOMINAL_BLOCK_SECS * 1000;
    let mut keep: BTreeSet<u64> = all.iter().map(|r| r.epoch).filter(|e| *e >= floor || referenced.contains(e)).collect();
    for &e in referenced {
        let own = all.iter().find(|r| r.epoch == e);
        let mut nearest = |pred: &dyn Fn(&BoundaryRow) -> bool| {
            if let Some(r) = all.iter().rev().filter(|r| r.epoch < e).find(|r| pred(r)) {
                keep.insert(r.epoch);
            }
            if let Some(r) = all.iter().filter(|r| r.epoch > e).find(|r| pred(r)) {
                keep.insert(r.epoch);
            }
        };
        if own.is_none_or(|r| r.block.is_none()) {
            nearest(&|r| r.block.is_some());
        }
        if own.is_none_or(|r| r.own_time(ms).is_none()) {
            nearest(&|r| r.observed_time(ms).is_some());
        }
    }
    keep
}

/// The epochs a cohort's dates use: its opening, window close, publication
/// and close, each phase-log entry's epoch and the boundary after it, and
/// the stored epoch while the cohort runs.
pub fn cohort_epochs(
    marks: &[Option<u64>],
    log_epochs: &[u64],
    current: Option<u64>,
) -> BTreeSet<u64> {
    let mut out: BTreeSet<u64> = marks.iter().flatten().copied().collect();
    for e in log_epochs {
        out.insert(*e);
        if let Some(n) = e.checked_add(1) {
            out.insert(n);
        }
    }
    out.extend(current);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRIDE: u64 = 361;
    const MS: u64 = 12_000;

    fn obs(epoch: u64, live: bool, ms: u64) -> BoundaryRow {
        BoundaryRow {
            epoch,
            block: Some(1000 + epoch * STRIDE),
            observed_ms: Some(ms),
            live: Some(live),
            credited: Some(live),
            active: Some(epoch),
            seen_block: Some(1000 + epoch * STRIDE + 2),
            source: Source::Observe,
        }
    }

    fn t(epoch: u64) -> u64 {
        1_790_000_000_000 + epoch * STRIDE * MS
    }

    fn one(rows: &[BoundaryRow], e: u64) -> BoundaryPoint {
        resolve(rows, &BTreeSet::from([e]), MS)[0]
    }

    #[test]
    fn observed_row_is_observed() {
        let p = one(&[obs(10, true, t(10) + 30_000)], 10);
        assert_eq!((p.block, p.block_basis), (Some(1000 + 10 * STRIDE), Some(Basis::Observed)));
        assert_eq!((p.at_ms, p.at_basis), (Some(t(10) + 30_000), Some(Basis::Observed)));
        assert_eq!(p.active, Some(10));
    }

    #[test]
    fn caught_up_row_has_observed_block_and_derived_time() {
        let rows = [obs(10, true, t(10)), obs(11, false, t(14)), obs(12, true, t(12))];
        let p = one(&rows, 11);
        assert_eq!(p.block_basis, Some(Basis::Observed));
        assert_eq!((p.at_ms, p.at_basis), (Some(t(11)), Some(Basis::Derived)));
    }

    #[test]
    fn missing_epoch_interpolates_between_observed_rows() {
        let rows = [obs(10, true, t(10)), obs(14, true, t(14) + 4_000)];
        let p = one(&rows, 12);
        assert_eq!((p.block, p.block_basis), (Some(1000 + 12 * STRIDE), Some(Basis::Derived)));
        assert_eq!((p.at_ms, p.at_basis), (Some(t(12) + 2_000), Some(Basis::Derived)));
        assert_eq!(p.active, None);
    }

    #[test]
    fn one_sided_anchor_uses_nominal_block_time() {
        let rows = [obs(10, true, t(10)), obs(11, true, t(11))];
        let p = one(&rows, 13);
        assert_eq!((p.block, p.block_basis), (Some(1000 + 13 * STRIDE), Some(Basis::Derived)));
        assert_eq!((p.at_ms, p.at_basis), (Some(t(13)), Some(Basis::Derived)));
        let p = one(&rows, 8);
        assert_eq!((p.block, p.at_ms), (Some(1000 + 8 * STRIDE), Some(t(8))));
    }

    #[test]
    fn tempo_change_between_rows_refuses_linear_block_but_dates_by_epoch() {
        let mut b = obs(13, true, t(10) + 3 * 300 * MS);
        b.block = Some(1000 + 10 * STRIDE + 3 * 301 + 1);
        b.seen_block = b.block;
        let rows = [obs(10, true, t(10)), b];
        let p = one(&rows, 12);
        assert_eq!((p.block, p.block_basis), (None, None), "no whole stride across the change");
        assert_eq!((p.at_ms, p.at_basis), (Some(t(10) + 2 * 300 * MS), Some(Basis::Derived)));
        assert_eq!(p.at_reason, None);
        // With an observed time on one side only, nothing supports a time,
        // and the reason says the journal has rows around it.
        let mut c = b;
        c.live = Some(false);
        let p = one(&[obs(10, true, t(10)), c], 12);
        assert_eq!((p.at_ms, p.at_reason), (None, Some(Unresolved::NotDerivable)));
    }

    /// Coordinator silence ending 40 minutes into a tempo: the first tick of
    /// the epoch is live, but its time is the tick's, not the boundary's.
    /// It is dated back by its block lag (derived) and anchors nothing.
    #[test]
    fn a_late_first_tick_is_dated_back_to_the_boundary_and_is_derived() {
        let mut late = obs(12, true, t(12) + 40 * 60_000);
        late.seen_block = Some(1000 + 12 * STRIDE + 200);
        let rows = [obs(10, true, t(10)), late];
        let p = one(&rows, 12);
        assert_eq!((p.at_ms, p.at_basis), (Some(t(12) + 40 * 60_000 - 200 * MS), Some(Basis::Derived)));
        // Epoch 11 interpolates from the observed row and the nominal block
        // time, never from the late tick.
        let p = one(&rows, 11);
        assert_eq!((p.at_ms, p.at_basis), (Some(t(11)), Some(Basis::Derived)));
        // A tick within one sweep is the boundary's observed time.
        let mut prompt = obs(12, true, t(12) + 50_000);
        prompt.seen_block = Some(1000 + 12 * STRIDE + sweep_blocks(MS));
        assert_eq!(one(&[prompt], 12).at_basis, Some(Basis::Observed));
    }

    #[test]
    fn the_sweep_is_the_eval_workers_cron() {
        let toml = include_str!("../../wrangler.toml");
        let section = toml.split("[env.eval-testnet.triggers]").nth(1).expect("eval-testnet triggers");
        let line = section.lines().find(|l| l.trim_start().starts_with("crons")).expect("crons");
        assert_eq!(line.trim(), r#"crons = ["* * * * *"]"#, "TICK_SWEEP_SECS follows the cron trigger");
        assert_eq!(TICK_SWEEP_SECS, 60);
        assert_eq!(sweep_blocks(12_000), 5);
    }

    /// Pruning keeps every referenced epoch's nearest anchors, so a derived
    /// value of an epoch without a row never moves once its anchors exist.
    #[test]
    fn prune_keeps_the_anchors_of_referenced_epochs() {
        let all: Vec<BoundaryRow> = [10, 11, 15, 16, 30, 31].iter().map(|&e| obs(e, true, t(e) + 1_000)).collect();
        // Epoch 13 is referenced (a phase-log entry) but has no row.
        let referenced = BTreeSet::from([13]);
        let keep = kept(&all, &referenced, 31);
        assert_eq!(keep, BTreeSet::from([11, 15, 31]));
        let before = one(&all, 13);
        let pruned: Vec<BoundaryRow> = all.iter().filter(|r| keep.contains(&r.epoch)).copied().collect();
        assert_eq!(one(&pruned, 13), before);
    }

    #[test]
    fn before_the_first_row_says_so() {
        let rows = [BoundaryRow { block: None, ..obs(20, true, t(20)) }];
        let p = one(&rows, 15);
        assert_eq!((p.at_ms, p.at_reason), (None, Some(Unresolved::BeforeTracking)));
    }

    #[test]
    fn before_tracking_is_null() {
        let seeded = BoundaryRow {
            epoch: 20,
            block: None,
            observed_ms: Some(t(20)),
            live: None,
            credited: None,
            active: None,
            seen_block: None,
            source: Source::SeedPhase,
        };
        // A seed without a block dates its own epoch only; nothing below it.
        assert_eq!(one(&[seeded], 20).at_basis, Some(Basis::Observed));
        let p = one(&[seeded], 15);
        assert_eq!((p.block, p.at_ms, p.at_basis), (None, None, None));
        assert_eq!(one(&[], 15).block, None);
    }

    #[test]
    fn a_seed_of_the_stored_epoch_dates_nothing_by_itself() {
        let seed = BoundaryRow {
            epoch: 30,
            block: Some(1000 + 30 * STRIDE),
            observed_ms: None,
            live: None,
            credited: None,
            active: None,
            seen_block: None,
            source: Source::SeedObs,
        };
        let p = one(&[seed], 30);
        assert_eq!((p.block_basis, p.at_ms), (Some(Basis::Observed), None));
    }

    #[test]
    fn a_caught_up_observation_time_is_never_an_anchor() {
        let rows = [obs(10, false, t(13))];
        let p = one(&rows, 10);
        assert_eq!((p.at_ms, p.at_basis), (None, None));
    }
}
