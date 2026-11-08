//! Each entrant's scoring progress in one cohort, for the status
//! (`admission.cohorts[].progress`, rule [`PROGRESS_RULE`]): the counts
//! the publisher's running roster shows beside an admitted entrant
//! ("142 / 312 challenges", or "challenges pending").
//!
//! Counts only (the 2026-11-06 decision to show challenge progress
//! while scoring runs). A row names an entrant's hotkey (64
//! lowercase hex), whether its scoring has started, and how many of its
//! documents are resolved out of how many it is scored on. Nothing else
//! leaves this function: no prediction, outcome, score, utility, job or
//! chunk number, lane, attempt, crash, fault, retry, timing or roster
//! position. The rows are in hotkey order, never roster order.
//!
//! - **Rows.** One per entrant job of the cohort (`jobs.role =
//!   'entrant'`): references are not miners and are left out, and an
//!   entrant the seal does not execute (its content was already
//!   published) has no job and so no row. At most [`PROGRESS_ROWS`]
//!   (the protocol's roster bound); more refuses the report whole
//!   (`over_bound`), counted before any row is read.
//! - **`sealed`.** Whether the cohort's jobs exist (the seal inserts
//!   them all in one transaction). Before that every row is absent and
//!   each admitted entrant is pending.
//! - **`done` / `total`.** The documents of the job's chunks that
//!   committed, of all its chunks' documents (the fan-out lanes commit a
//!   chunk whole, so `done` moves in steps of `scorer.chunk_docs`). A
//!   document that resolved as a model error counts as done, so the count
//!   does not reveal outcomes. A faulted job keeps its count, so no fault
//!   shows either.
//! - **`started`.** The job's first claim was granted (`started_ms`).
//! - **Freshness.** Read with the status; the publisher reads the status
//!   about once a minute.

use crate::eval::engine::{R, i, opt_int};
use crate::sql::{Sql, int, text};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The rule of a cohort's `progress` in the admission status.
pub const PROGRESS_RULE: &str = "slop_ninja.eval.scoring_progress/v1";

/// The most rows: one per entrant the protocol's roster can hold.
pub const PROGRESS_ROWS: usize = slop_ninja_protocol::MAX_ROSTER as usize;

/// The cohort's `progress` report (`seq`, numbered `cohort`).
pub(super) fn report(sql: &impl Sql, seq: i64, cohort: u64) -> R<Value> {
    let refused = |why: &str| json!({"rule": PROGRESS_RULE, "cohort": cohort, "sealed": null, "rows": null, "refused": why});
    let jobs = super::count(sql, "SELECT COUNT(*) FROM jobs WHERE seq = ?", &[i(seq)])?;
    let entrants = super::count(sql, "SELECT COUNT(*) FROM jobs WHERE seq = ? AND role = 'entrant'", &[i(seq)])?;
    if usize::try_from(entrants).ok().is_none_or(|n| n > PROGRESS_ROWS) {
        return Ok(refused("over_bound"));
    }
    let found = sql.exec(
        "SELECT j.owner, j.started_ms,
                COALESCE((SELECT SUM(c.count) FROM chunks c WHERE c.seq = j.seq AND c.job = j.job AND c.status = 'done'), 0),
                COALESCE((SELECT SUM(c.count) FROM chunks c WHERE c.seq = j.seq AND c.job = j.job), 0)
         FROM jobs j WHERE j.seq = ? AND j.role = 'entrant'",
        &[i(seq)],
    )?;
    let mut rows: Vec<(String, Value)> = Vec::with_capacity(found.len());
    let mut keys = BTreeSet::new();
    for r in &found {
        let Some(hotkey) = owner_hotkey(text(r, 0)?) else {
            return Ok(refused("invalid_owner"));
        };
        let started = opt_int(r, 1).is_some();
        let (done, total) = (int(r, 2)?, int(r, 3)?);
        if total < 1 || done < 0 || done > total || (done > 0 && !started) {
            return Ok(refused("invalid_counts"));
        }
        if !keys.insert(hotkey.clone()) {
            return Ok(refused("repeated_hotkey"));
        }
        let row = json!({"hotkey": hotkey, "started": started, "done": done, "total": total});
        rows.push((hotkey, row));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let rows: Vec<Value> = rows.into_iter().map(|(_, v)| v).collect();
    Ok(json!({"rule": PROGRESS_RULE, "cohort": cohort, "sealed": jobs > 0, "rows": rows}))
}

/// The 64 lowercase hex characters of a job owner's hotkey
/// (`plan::IdentityJson`, `0x` + 64 lowercase hex).
fn owner_hotkey(owner: &str) -> Option<String> {
    let v: Value = serde_json::from_str(owner).ok()?;
    let hex = v.get("hotkey")?.as_str()?.strip_prefix("0x")?;
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))).then(|| hex.to_owned())
}
