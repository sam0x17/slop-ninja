//! The coordinator heartbeat: what one block delivery, one cron tick or one RPC
//! result does inside the ChainJournal's storage transaction.
//!
//! Each entry point runs in a single transaction and returns at most one
//! [`RpcPlan`]. The Worker executes the plan through the read-only transport
//! after it has acknowledged the delivery, then hands the [`RpcResult`] back to
//! [`on_rpc_result`], which may plan one more bounded round. RPC work is held
//! under the durable `rpc-backfill` claim, so overlapping invocations never
//! fetch the same range twice, and every call is charged to the shared
//! per-minute budget first.
//!
//! RPC use: none for a delivery that extends the cursor; one hash-list call per
//! gap round (at most [`Settings::delivery_rounds`] per delivery); two calls
//! (finalized head and its header) to anchor; two calls for the once-per-tick
//! head read; and in fallback at most [`Settings::tick_rounds`] minus one
//! hash-list calls per tick.
//!
//! Scheduling reads the RPC-confirmed watermark ([`journal::confirmed`]), not
//! the provisional cursor that deliveries advance, so no work acts on a block
//! until RPC evidence independent of the webhook provider covers it.

use crate::claims::{self, ClaimOutcome, ClaimToken};
use crate::journal::{self, DeliveryOutcome, Halt, HashesOutcome, IngestError, Progress};
use crate::rpc::{MAX_HASHES_PER_CALL, RpcError};
use crate::schedule;
use crate::sql::{Sql, StoreError};
use crate::watchdog::{self, Mode};
use crate::webhook::BlockNotification;
use crate::wire::{BlockRef, FinalizedHeader, Id};
use serde::{Deserialize, Serialize};

/// Claim that serializes RPC anchoring and backfill across invocations.
pub const BACKFILL_OP: &str = "rpc-backfill";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Settings {
    /// Silence before fallback (default 300 s).
    pub silence_ms: u64,
    /// Lag beyond which a tick flags the cursor as behind (default 25 blocks,
    /// the silence threshold at 12 s blocks).
    pub behind_blocks: u64,
    /// Shared ceiling on the Worker's RPC calls per minute.
    pub rpc_calls_per_minute: u64,
    /// Heights per `chain_getBlockHash` list call, cursor height included.
    pub hashes_per_call: u64,
    /// Lease on claimed work; below the 30 s `waitUntil` allowance.
    pub lease_ms: u64,
    /// Minimum spacing of watchdog head reads (one per tick).
    pub head_read_interval_ms: u64,
    /// Blocks one transaction applies at most.
    pub max_drain: u64,
    /// RPC rounds one delivery invocation may run.
    pub delivery_rounds: u64,
    /// RPC rounds one cron tick may run, the head read included.
    pub tick_rounds: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            silence_ms: watchdog::DEFAULT_SILENCE_MS,
            behind_blocks: watchdog::DEFAULT_SILENCE_MS.div_ceil(watchdog::BLOCK_TIME_MS),
            rpc_calls_per_minute: 12,
            hashes_per_call: MAX_HASHES_PER_CALL,
            lease_ms: 25_000,
            head_read_interval_ms: 50_000,
            max_drain: 512,
            delivery_rounds: 2,
            tick_rounds: 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Delivery,
    Tick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadPurpose {
    /// First cursor position.
    Anchor,
    /// The tick's lag check and cursor audit.
    Watchdog,
}

/// One bounded round of read-only RPC for the Worker to run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "call", rename_all = "snake_case")]
pub enum RpcPlan {
    /// `chain_getFinalizedHead`, then `chain_getHeader` on that hash.
    FinalizedHead {
        purpose: HeadPurpose,
        origin: Origin,
        claim: Option<ClaimToken>,
    },
    /// `chain_getBlockHash` list for `from..=to`; `from` is the cursor height.
    BlockHashes {
        from: u64,
        to: u64,
        origin: Origin,
        claim: ClaimToken,
    },
}

impl RpcPlan {
    pub fn calls(&self) -> u64 {
        match self {
            Self::FinalizedHead { .. } => 2,
            Self::BlockHashes { .. } => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "call", rename_all = "snake_case")]
pub enum RpcResult {
    FinalizedHead {
        purpose: HeadPurpose,
        origin: Origin,
        claim: Option<ClaimToken>,
        head: Result<FinalizedHeader, RpcError>,
    },
    BlockHashes {
        from: u64,
        to: u64,
        origin: Origin,
        claim: ClaimToken,
        hashes: Result<Vec<Option<Id>>, RpcError>,
    },
}

impl RpcResult {
    fn claim(&self) -> Option<&ClaimToken> {
        match self {
            Self::FinalizedHead { claim, .. } => claim.as_ref(),
            Self::BlockHashes { claim, .. } => Some(claim),
        }
    }

    fn origin(&self) -> Origin {
        match self {
            Self::FinalizedHead { origin, .. } | Self::BlockHashes { origin, .. } => *origin,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryReport {
    pub outcome: DeliveryOutcome,
    pub progress: Progress,
    /// Work this invocation claimed in the scheduling step.
    pub claimed_work: Vec<ClaimToken>,
    pub plan: Option<RpcPlan>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub progress: Progress,
    /// The result came from a fenced (expired and re-claimed) holder and was
    /// discarded.
    pub fenced: bool,
    pub plan: Option<RpcPlan>,
}

/// One authenticated block delivery, inside the transaction that persists it.
/// The acknowledgment follows the commit; the plan, if any, runs after it.
pub fn on_delivery(
    sql: &impl Sql,
    settings: &Settings,
    n: &BlockNotification,
    now_ms: u64,
) -> Result<DeliveryReport, IngestError> {
    let outcome = journal::ingest_delivery(sql, n, now_ms)?;
    watchdog::note_delivery(sql, now_ms)?;
    if outcome == DeliveryOutcome::DuplicateDelivery {
        // A provider retry: the first attempt already drove its step.
        journal::bump(sql, "duplicate_deliveries")?;
        return Ok(DeliveryReport {
            outcome,
            progress: Progress {
                cursor: journal::cursor(sql)?,
                ..Progress::default()
            },
            claimed_work: Vec::new(),
            plan: None,
        });
    }
    let progress = journal::drain(sql, settings.max_drain, now_ms)?;
    let holder = format!("delivery:{}", n.delivery_id.to_hex());
    // Due work is judged at the RPC-confirmed watermark, never at the
    // provisional cursor this delivery may just have advanced.
    let due = schedule::due_operations(progress.confirmed);
    let claimed_work = schedule::step(sql, &due, &holder, settings.lease_ms, now_ms)?;
    let plan = plan(
        sql,
        settings,
        &progress,
        Origin::Delivery,
        None,
        &holder,
        now_ms,
    )?;
    Ok(DeliveryReport {
        outcome,
        progress,
        claimed_work,
        plan,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickReport {
    pub fallback: bool,
    pub paused: bool,
    pub plan: Option<RpcPlan>,
}

/// One cron tick: a cheap read of the delivery time and cursor, plus at most
/// one RPC head read per tick.
pub fn on_tick(sql: &impl Sql, settings: &Settings, now_ms: u64) -> Result<TickReport, StoreError> {
    let state = watchdog::begin_tick(sql, settings.silence_ms, now_ms)?;
    let fallback = state.mode == Mode::Fallback;
    if journal::active_fault(sql)?.is_some() {
        return Ok(TickReport {
            fallback,
            paused: true,
            plan: None,
        });
    }
    if !watchdog::take_head_read(sql, settings.head_read_interval_ms, now_ms)? {
        journal::bump(sql, "duplicate_ticks")?;
        return Ok(TickReport {
            fallback,
            paused: false,
            plan: None,
        });
    }
    let plan = if claims::reserve_rpc(sql, 2, settings.rpc_calls_per_minute, now_ms)? {
        Some(RpcPlan::FinalizedHead {
            purpose: HeadPurpose::Watchdog,
            origin: Origin::Tick,
            claim: None,
        })
    } else {
        journal::bump(sql, "rpc_budget_deferrals")?;
        None
    };
    Ok(TickReport {
        fallback,
        paused: false,
        plan,
    })
}

/// Applies one RPC round's result and plans the next round when `more` rounds
/// remain in this invocation. Releases the backfill claim when no round follows.
pub fn on_rpc_result(
    sql: &impl Sql,
    settings: &Settings,
    result: &RpcResult,
    more: bool,
    holder: &str,
    now_ms: u64,
) -> Result<Step, StoreError> {
    let held = result.claim().cloned();
    if let Some(token) = &held
        && !claims::is_current(sql, token, now_ms)?
    {
        journal::bump(sql, "fenced_results")?;
        return Ok(Step {
            progress: Progress {
                cursor: journal::cursor(sql)?,
                ..Progress::default()
            },
            fenced: true,
            plan: None,
        });
    }
    let confirmed_before = journal::confirmed(sql)?;
    match result {
        RpcResult::FinalizedHead { head: Err(e), .. }
        | RpcResult::BlockHashes { hashes: Err(e), .. } => {
            note_rpc_error(sql, *e)?;
        }
        RpcResult::FinalizedHead {
            purpose,
            head: Ok(h),
            ..
        } => {
            journal::note_rpc_head(sql, h.block_ref(), now_ms)?;
            journal::anchor(sql, h.block_ref(), now_ms)?;
            if *purpose == HeadPurpose::Watchdog {
                let progress = journal::drain(sql, settings.max_drain, now_ms)?;
                watchdog::after_head(
                    sql,
                    settings.silence_ms,
                    settings.behind_blocks,
                    h.block_ref(),
                    progress.cursor,
                    now_ms,
                )?;
            }
        }
        RpcResult::BlockHashes {
            from,
            hashes: Ok(list),
            ..
        } => match journal::apply_hashes(sql, *from, list, now_ms)? {
            HashesOutcome::Unusable => journal::bump(sql, "rpc_unusable_lists")?,
            HashesOutcome::Recorded { .. } | HashesOutcome::Fault => {}
        },
    }
    let progress = journal::drain(sql, settings.max_drain, now_ms)?;
    if progress.confirmed != confirmed_before {
        // RPC evidence moved the confirmed watermark (an anchor, a passed head
        // audit, or fallback blocks), so work due there may now start. In
        // fallback no delivery drives scheduling, so this round does.
        let due = schedule::due_operations(progress.confirmed);
        schedule::step(sql, &due, holder, settings.lease_ms, now_ms)?;
    }
    let next = if more {
        plan(
            sql,
            settings,
            &progress,
            result.origin(),
            held.clone(),
            holder,
            now_ms,
        )?
    } else {
        None
    };
    if next.is_none()
        && let Some(token) = &held
    {
        claims::release(sql, token, now_ms)?;
    }
    Ok(Step {
        progress,
        fenced: false,
        plan: next,
    })
}

fn note_rpc_error(sql: &impl Sql, e: RpcError) -> Result<(), StoreError> {
    journal::bump(sql, "rpc_failures")?;
    if e == RpcError::RateLimited {
        journal::bump(sql, "rpc_rate_limited")?;
    }
    Ok(())
}

/// Plans the next RPC round for `progress`, under the backfill claim and the
/// shared budget. `held` continues a claim this invocation already holds.
fn plan(
    sql: &impl Sql,
    settings: &Settings,
    progress: &Progress,
    origin: Origin,
    held: Option<ClaimToken>,
    holder: &str,
    now_ms: u64,
) -> Result<Option<RpcPlan>, StoreError> {
    let range = |cursor: BlockRef, to: u64| {
        let width = settings.hashes_per_call.clamp(2, MAX_HASHES_PER_CALL);
        (cursor.number, to.min(cursor.number + width - 1))
    };
    let want = match (progress.halt, progress.cursor) {
        (Some(Halt::NotAnchored), _) => Some(None),
        (Some(Halt::Gap { to, .. }), Some(c)) => Some(Some(range(c, to))),
        (None | Some(Halt::Bound), Some(c)) if origin == Origin::Tick => {
            let fallback = watchdog::load(sql)?.mode == Mode::Fallback;
            match journal::rpc_finalized_head(sql)? {
                Some(head) if fallback && head.number > c.number => {
                    Some(Some(range(c, head.number)))
                }
                _ => None,
            }
        }
        _ => None,
    };
    let Some(want) = want else {
        return Ok(None);
    };
    let token = match held {
        Some(token) => {
            if !claims::renew(sql, &token, settings.lease_ms, now_ms)? {
                return Ok(None);
            }
            token
        }
        None => match claims::claim(sql, BACKFILL_OP, holder, settings.lease_ms, now_ms)? {
            ClaimOutcome::Claimed(token) => token,
            ClaimOutcome::Held { .. } | ClaimOutcome::Done => {
                journal::bump(sql, "backfill_claim_skips")?;
                return Ok(None);
            }
        },
    };
    let plan = match want {
        None => RpcPlan::FinalizedHead {
            purpose: HeadPurpose::Anchor,
            origin,
            claim: Some(token.clone()),
        },
        Some((from, to)) => RpcPlan::BlockHashes {
            from,
            to,
            origin,
            claim: token.clone(),
        },
    };
    if !claims::reserve_rpc(sql, plan.calls(), settings.rpc_calls_per_minute, now_ms)? {
        journal::bump(sql, "rpc_budget_deferrals")?;
        claims::release(sql, &token, now_ms)?;
        return Ok(None);
    }
    Ok(Some(plan))
}

/// Operator action: clears the active fault and runs the path again (no RPC).
pub fn clear_fault(
    sql: &impl Sql,
    settings: &Settings,
    fault_id: u64,
    reanchor: bool,
    now_ms: u64,
) -> Result<(journal::ClearOutcome, Progress), StoreError> {
    let outcome = journal::clear_fault(sql, fault_id, reanchor, now_ms)?;
    let progress = journal::drain(sql, settings.max_drain, now_ms)?;
    Ok((outcome, progress))
}

/// Content-free `/status` view of the ChainJournal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Status {
    pub schema_version: u32,
    pub mode: Mode,
    pub mode_reason: Option<String>,
    pub mode_age_ms: u64,
    pub last_delivery_age_ms: Option<u64>,
    pub last_tick_age_ms: Option<u64>,
    /// Behind flag and lag from the last watchdog head read.
    pub behind: bool,
    pub watchdog_lag_blocks: Option<u64>,
    pub transitions: u64,
    pub recent_transitions: Vec<watchdog::Transition>,
    pub journal: journal::JournalStatus,
    pub backfill_claim: Option<claims::ClaimStatus>,
    pub rpc_calls_this_minute: u64,
    pub settings: Settings,
    pub counters: std::collections::BTreeMap<String, u64>,
}

pub fn status(
    sql: &impl Sql,
    settings: &Settings,
    schema_version: u32,
    now_ms: u64,
) -> Result<Status, StoreError> {
    let w = watchdog::load(sql)?;
    let age = |at: Option<u64>| at.map(|at| now_ms.saturating_sub(at));
    Ok(Status {
        schema_version,
        mode: w.mode,
        mode_reason: w.reason.clone(),
        mode_age_ms: now_ms.saturating_sub(w.mode_since_ms),
        last_delivery_age_ms: age(w.last_delivery_ms),
        last_tick_age_ms: age(w.last_tick_ms),
        behind: w.behind,
        watchdog_lag_blocks: w.lag_blocks,
        transitions: watchdog::transition_count(sql)?,
        recent_transitions: watchdog::transitions(sql, 5)?,
        journal: journal::status(sql, now_ms)?,
        backfill_claim: claims::status(sql, BACKFILL_OP, now_ms)?,
        rpc_calls_this_minute: claims::rpc_used(sql, now_ms)?,
        settings: *settings,
        counters: journal::counters(sql)?.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests;
