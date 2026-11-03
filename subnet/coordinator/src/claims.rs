//! Durable claims (leases) for work that outlives one invocation, and the
//! shared RPC call budget.
//!
//! Each block delivery is its own Worker invocation, so a slow task started by
//! one block can overlap the next block's invocation. Due work is claimed here
//! under an idempotent operation ID before it starts. Overlapping invocations
//! see the claim and skip the work. A claim whose holder crashed expires and can
//! be re-claimed; re-claiming bumps the generation, which fences the stale
//! holder: its later results and completion are refused. Completion is durable,
//! so later invocations observe it instead of repeating the work.
//!
//! Every function runs inside the caller's storage transaction.

use crate::sql::{Sql, StoreError, bind_u64, text, uint};
use serde::{Deserialize, Serialize};

/// Proof of a held claim, carried through RPC plans and work results.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimToken {
    pub op_id: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed(ClaimToken),
    /// Another live holder has it until `expires_at_ms`.
    Held {
        expires_at_ms: u64,
    },
    /// The operation already completed; nothing to do.
    Done,
}

fn row(sql: &impl Sql, op_id: &str) -> Result<Option<(String, u64, u64)>, StoreError> {
    let rows = sql.exec(
        "SELECT state, generation, expires_at_ms FROM claims WHERE op_id = ?",
        &[op_id.into()],
    )?;
    rows.first()
        .map(|r| Ok((text(r, 0)?.to_owned(), uint(r, 1)?, uint(r, 2)?)))
        .transpose()
}

/// Claims `op_id` for `holder` until `now_ms + lease_ms`.
pub fn claim(
    sql: &impl Sql,
    op_id: &str,
    holder: &str,
    lease_ms: u64,
    now_ms: u64,
) -> Result<ClaimOutcome, StoreError> {
    let expires = now_ms.saturating_add(lease_ms);
    let generation = match row(sql, op_id)? {
        Some((state, _, _)) if state == "done" => return Ok(ClaimOutcome::Done),
        Some((state, _, expires_at)) if state == "claimed" && expires_at > now_ms => {
            return Ok(ClaimOutcome::Held {
                expires_at_ms: expires_at,
            });
        }
        Some((_, generation, _)) => generation + 1,
        None => 1,
    };
    sql.exec(
        "INSERT INTO claims
            (op_id, holder, generation, state, claimed_at_ms, expires_at_ms, finished_at_ms)
         VALUES (?, ?, ?, 'claimed', ?, ?, NULL)
         ON CONFLICT(op_id) DO UPDATE SET holder = excluded.holder,
            generation = excluded.generation, state = 'claimed',
            claimed_at_ms = excluded.claimed_at_ms, expires_at_ms = excluded.expires_at_ms,
            finished_at_ms = NULL",
        &[
            op_id.into(),
            holder.into(),
            bind_u64(generation)?,
            bind_u64(now_ms)?,
            bind_u64(expires)?,
        ],
    )?;
    if generation > 1 {
        bump_reclaims(sql)?;
    }
    Ok(ClaimOutcome::Claimed(ClaimToken {
        op_id: op_id.to_owned(),
        generation,
    }))
}

fn bump_reclaims(sql: &impl Sql) -> Result<(), StoreError> {
    sql.exec(
        "INSERT INTO counters (name, value) VALUES ('claim_generations', 1)
         ON CONFLICT(name) DO UPDATE SET value = value + 1",
        &[],
    )?;
    Ok(())
}

/// True while `token` is the current, unexpired claim.
pub fn is_current(sql: &impl Sql, token: &ClaimToken, now_ms: u64) -> Result<bool, StoreError> {
    Ok(matches!(
        row(sql, &token.op_id)?,
        Some((state, generation, expires)) if state == "claimed"
            && generation == token.generation
            && expires > now_ms
    ))
}

/// Extends a current claim; a fenced holder gets `false`.
pub fn renew(
    sql: &impl Sql,
    token: &ClaimToken,
    lease_ms: u64,
    now_ms: u64,
) -> Result<bool, StoreError> {
    if !is_current(sql, token, now_ms)? {
        return Ok(false);
    }
    sql.exec(
        "UPDATE claims SET expires_at_ms = ? WHERE op_id = ?",
        &[
            bind_u64(now_ms.saturating_add(lease_ms))?,
            token.op_id.as_str().into(),
        ],
    )?;
    Ok(true)
}

fn finish(
    sql: &impl Sql,
    token: &ClaimToken,
    state: &'static str,
    now_ms: u64,
) -> Result<bool, StoreError> {
    if !is_current(sql, token, now_ms)? {
        return Ok(false);
    }
    sql.exec(
        "UPDATE claims SET state = ?, finished_at_ms = ? WHERE op_id = ?",
        &[state.into(), bind_u64(now_ms)?, token.op_id.as_str().into()],
    )?;
    Ok(true)
}

/// Gives up a repeatable operation (for example one backfill round) so the
/// next invocation can claim it at once.
pub fn release(sql: &impl Sql, token: &ClaimToken, now_ms: u64) -> Result<bool, StoreError> {
    finish(sql, token, "released", now_ms)
}

/// Records completion of a one-shot operation. Later claims return `Done`.
pub fn complete(sql: &impl Sql, token: &ClaimToken, now_ms: u64) -> Result<bool, StoreError> {
    finish(sql, token, "done", now_ms)
}

/// Content-free view of one claim for `/status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ClaimStatus {
    pub state: String,
    pub generation: u64,
    pub expires_in_ms: Option<u64>,
}

pub fn status(sql: &impl Sql, op_id: &str, now_ms: u64) -> Result<Option<ClaimStatus>, StoreError> {
    Ok(
        row(sql, op_id)?.map(|(state, generation, expires)| ClaimStatus {
            expires_in_ms: (state == "claimed" && expires > now_ms).then(|| expires - now_ms),
            state,
            generation,
        }),
    )
}

/// Length of the RPC budget window.
pub const BUDGET_WINDOW_MS: u64 = 60_000;

/// Reserves `calls` RPC calls in the current one-minute window, shared by every
/// invocation. Returns `false`, reserving nothing, when the window is full.
pub fn reserve_rpc(
    sql: &impl Sql,
    calls: u64,
    per_minute: u64,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let rows = sql.exec(
        "SELECT window_start_ms, used FROM rpc_budget WHERE id = 1",
        &[],
    )?;
    let r = rows.first().ok_or(StoreError("rpc budget row"))?;
    let (mut start, mut used) = (uint(r, 0)?, uint(r, 1)?);
    if now_ms.saturating_sub(start) >= BUDGET_WINDOW_MS || now_ms < start {
        start = now_ms;
        used = 0;
    }
    let granted = used.saturating_add(calls) <= per_minute;
    if granted {
        used += calls;
    }
    sql.exec(
        "UPDATE rpc_budget SET window_start_ms = ?, used = ? WHERE id = 1",
        &[bind_u64(start)?, bind_u64(used)?],
    )?;
    Ok(granted)
}

/// Calls used in the window containing `now_ms`.
pub fn rpc_used(sql: &impl Sql, now_ms: u64) -> Result<u64, StoreError> {
    let rows = sql.exec(
        "SELECT window_start_ms, used FROM rpc_budget WHERE id = 1",
        &[],
    )?;
    let r = rows.first().ok_or(StoreError("rpc budget row"))?;
    let start = uint(r, 0)?;
    Ok(if now_ms.saturating_sub(start) >= BUDGET_WINDOW_MS {
        0
    } else {
        uint(r, 1)?
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::{CHAIN_JOURNAL_SCHEMA, migrate};
    use crate::testing::{Sqlite, tx};

    fn db() -> Sqlite {
        let db = Sqlite::open();
        tx(&db, |sql| migrate(sql, &CHAIN_JOURNAL_SCHEMA)).unwrap();
        db
    }

    #[test]
    fn overlapping_holders_skip_and_stale_claims_are_fenced() {
        let db = db();
        let first = match tx(&db, |s| claim(s, "work/7", "block-100", 25_000, 1_000)).unwrap() {
            ClaimOutcome::Claimed(t) => t,
            other => panic!("{other:?}"),
        };
        assert_eq!(first.generation, 1);
        // The next block's invocation overlaps and skips the work.
        assert_eq!(
            tx(&db, |s| claim(s, "work/7", "block-101", 25_000, 13_000)).unwrap(),
            ClaimOutcome::Held {
                expires_at_ms: 26_000
            }
        );
        // The first holder crashed; a later block re-claims after expiry.
        let second = match tx(&db, |s| claim(s, "work/7", "block-103", 25_000, 37_000)).unwrap() {
            ClaimOutcome::Claimed(t) => t,
            other => panic!("{other:?}"),
        };
        assert_eq!(second.generation, 2);
        // The stale holder wakes up: its renewal and completion are refused.
        assert!(!tx(&db, |s| renew(s, &first, 25_000, 38_000)).unwrap());
        assert!(!tx(&db, |s| complete(s, &first, 38_000)).unwrap());
        assert!(tx(&db, |s| renew(s, &second, 25_000, 38_000)).unwrap());
        assert!(tx(&db, |s| complete(s, &second, 40_000)).unwrap());
        // Later invocations observe completion from durable state.
        assert_eq!(
            tx(&db, |s| claim(s, "work/7", "block-110", 25_000, 120_000)).unwrap(),
            ClaimOutcome::Done
        );
        let st = tx(&db, |s| status(s, "work/7", 120_000)).unwrap().unwrap();
        assert_eq!((st.state.as_str(), st.generation), ("done", 2));
    }

    #[test]
    fn released_claims_are_reclaimable_at_once() {
        let db = db();
        let ClaimOutcome::Claimed(t) = tx(&db, |s| claim(s, "rpc", "a", 25_000, 0)).unwrap() else {
            panic!()
        };
        assert!(tx(&db, |s| release(s, &t, 1)).unwrap());
        assert!(!tx(&db, |s| is_current(s, &t, 2)).unwrap());
        let ClaimOutcome::Claimed(t2) = tx(&db, |s| claim(s, "rpc", "b", 25_000, 2)).unwrap()
        else {
            panic!()
        };
        assert_eq!(t2.generation, 2);
    }

    #[test]
    fn rpc_budget_is_shared_per_minute() {
        let db = db();
        let t = 1_793_000_000_000;
        assert!(tx(&db, |s| reserve_rpc(s, 10, 12, t)).unwrap());
        assert!(!tx(&db, |s| reserve_rpc(s, 3, 12, t + 30_000)).unwrap());
        assert!(tx(&db, |s| reserve_rpc(s, 2, 12, t + 30_000)).unwrap());
        assert_eq!(tx(&db, |s| rpc_used(s, t + 59_999)).unwrap(), 12);
        assert!(!tx(&db, |s| reserve_rpc(s, 1, 12, t + 59_999)).unwrap());
        assert!(tx(&db, |s| reserve_rpc(s, 12, 12, t + 60_000)).unwrap());
        assert_eq!(tx(&db, |s| rpc_used(s, t + 200_000)).unwrap(), 0);
    }
}
