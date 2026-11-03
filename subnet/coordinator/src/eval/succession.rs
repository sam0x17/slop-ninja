//! Durable succession between main authorities (a chain-record restart).
//!
//! A named main authority takes its first plan or template only after each
//! earlier authority recorded, in its own store, that it is superseded
//! (`config::Supersede`). The earlier authority records it in one
//! transaction, and only while it holds no cohort that is not closed or
//! abandoned, none of its labels or numbers falls within the successor's
//! rule, and it has no work left (`engine::blocking_work`: a closure, a
//! spend settlement, a purge of private results and summaries, or an
//! admission clean-up; a due receipt deletion does not count). The Worker kicks a predecessor only until its successor's
//! succession is complete, so work left at that point would never run: it
//! refuses with `predecessor_work` instead, and finishes it under the kicks
//! that continue meanwhile. From then on it admits no plan whatever a
//! deployment names, and its calendar is paused, so a deploy that reverts the
//! authority vars cannot re-admit cohort numbers or labels of the restarted
//! chain record, and no new work reaches it. Its cohorts, references and
//! signed records stay readable; its detailed receipts are deleted under the
//! N+2 rule once its successor has completed two cohorts and tells it
//! (`eval::retention`, `POST /succession/retention`), and deletions already
//! due finish under those notices too.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

use crate::config::Supersede;
use crate::eval::engine::{EvalConfig, EvalError, R, s};
use crate::sql::{Sql, bind_u64, int, text};
use serde_json::{Value, json};

/// v12: the successor this authority recorded, if any.
pub const SCHEMA_V12: &[&str] = &["CREATE TABLE authority_succession (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        superseded_by TEXT NOT NULL,
        successor_first_cohort INTEGER NOT NULL,
        superseded_ms INTEGER NOT NULL
    ) STRICT"];

/// The successor that superseded this authority, if one did.
pub fn superseded_by(sql: &impl Sql) -> R<Option<String>> {
    let rows = sql.exec("SELECT superseded_by FROM authority_succession WHERE id = 1", &[])?;
    match rows.first() {
        Some(r) => Ok(Some(text(r, 0)?.to_owned())),
        None => Ok(None),
    }
}

/// Records `req`'s successor, or says why not: `predecessor_busy`,
/// `predecessor_numbers` ([`Supersede::check`]), or `predecessor_work` while
/// [`crate::eval::engine::blocking_work`] (due receipt deletions excluded)
/// under this authority's own
/// configuration `cfg` (a retired one, whose calendar is never due) still
/// finds work. Idempotent: an authority already superseded (by this
/// successor or an earlier one) answers with the successor it recorded,
/// after the same checks, since its cohorts can no longer change. The first
/// successor recorded stays.
pub fn supersede(
    sql: &impl Sql,
    cfg: &EvalConfig,
    req: &Supersede,
    now_ms: u64,
) -> R<std::result::Result<Value, &'static str>> {
    let rows = sql.exec("SELECT label, cohort, phase FROM cohorts ORDER BY seq", &[])?;
    let mut cohorts = Vec::with_capacity(rows.len());
    for r in &rows {
        let cohort = u64::try_from(int(r, 1)?).map_err(|_| EvalError::State("cohort number"))?;
        cohorts.push((text(r, 0)?.to_owned(), cohort, text(r, 2)?.to_owned()));
    }
    if let Err(code) = req.check(cohorts.iter().map(|(l, n, p)| (l.as_str(), *n, p.as_str()))) {
        return Ok(Err(code));
    }
    if crate::eval::engine::blocking_work(sql, cfg)? {
        return Ok(Err("predecessor_work"));
    }
    sql.exec(
        "INSERT INTO authority_succession (id, superseded_by, successor_first_cohort, superseded_ms)
         VALUES (1, ?, ?, ?) ON CONFLICT (id) DO NOTHING",
        &[s(&req.by), bind_u64(req.first_cohort)?, bind_u64(now_ms)?],
    )?;
    // The calendar opens nothing more here (a retired authority's calendar
    // is never due either).
    crate::eval::calendar::set_paused(sql, true)?;
    let by = superseded_by(sql)?.ok_or(EvalError::State("succession"))?;
    let highest = cohorts.iter().map(|(_, n, _)| *n).max();
    Ok(Ok(json!({"superseded_by": by, "cohorts": cohorts.len(), "highest_cohort": highest})))
}
