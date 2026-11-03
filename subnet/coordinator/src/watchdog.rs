//! Watchdog mode: primary (webhook heartbeat) or fallback (cron-driven RPC).
//!
//! The webhook delivery of every finalized block is the coordinator's clock.
//! The one-minute cron is only a watchdog: it enters fallback when no valid
//! delivery has arrived for the silence threshold, or when the cursor lags the
//! RPC finalized head by more than the behind threshold. In fallback each tick
//! walks the cursor toward the RPC head through the same per-block path. A
//! valid delivery returns the coordinator to primary once it is not behind; a
//! tick returns it once deliveries are fresh and it has caught up. Every mode
//! change is recorded durably in `mode_transitions`.

use crate::sql::{Sql, StoreError, bind_u64, text, uint};
use crate::wire::BlockRef;
use serde::Serialize;

/// Testnet block time, used only to express the default behind threshold.
pub const BLOCK_TIME_MS: u64 = 12_000;
pub const DEFAULT_SILENCE_MS: u64 = 300_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Primary,
    Fallback,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// No valid delivery for the silence threshold.
    WebhookSilent,
    /// The cursor lags the RPC finalized head by more than the threshold.
    Behind,
    /// A valid delivery arrived while not behind.
    WebhookResumed,
    /// A tick saw fresh deliveries and a cursor within the threshold.
    CaughtUp,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Self::WebhookSilent => "webhook_silent",
            Self::Behind => "behind",
            Self::WebhookResumed => "webhook_resumed",
            Self::CaughtUp => "caught_up",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct State {
    pub mode: Mode,
    pub reason: Option<String>,
    pub mode_since_ms: u64,
    pub last_delivery_ms: Option<u64>,
    pub first_tick_ms: Option<u64>,
    pub last_tick_ms: Option<u64>,
    pub last_head_read_ms: Option<u64>,
    pub behind: bool,
    pub lag_blocks: Option<u64>,
}

impl State {
    /// True when no valid delivery has arrived for `silence_ms`. Before any
    /// delivery, silence counts from the first tick.
    pub fn silent(&self, silence_ms: u64, now_ms: u64) -> bool {
        self.last_delivery_ms
            .or(self.first_tick_ms)
            .is_some_and(|since| now_ms.saturating_sub(since) >= silence_ms)
    }
}

pub fn load(sql: &impl Sql) -> Result<State, StoreError> {
    let rows = sql.exec(
        "SELECT mode, reason, mode_since_ms, last_delivery_ms, first_tick_ms, last_tick_ms,
                last_head_read_ms, behind, lag_blocks
         FROM watchdog WHERE id = 1",
        &[],
    )?;
    let r = rows.first().ok_or(StoreError("watchdog row"))?;
    let opt = |i| crate::journal::opt_int(r, i);
    Ok(State {
        mode: match text(r, 0)? {
            "fallback" => Mode::Fallback,
            _ => Mode::Primary,
        },
        reason: match r.get(1) {
            Some(crate::sql::SqlValue::Text(v)) => Some(v.clone()),
            _ => None,
        },
        mode_since_ms: uint(r, 2)?,
        last_delivery_ms: opt(3)?,
        first_tick_ms: opt(4)?,
        last_tick_ms: opt(5)?,
        last_head_read_ms: opt(6)?,
        behind: uint(r, 7)? == 1,
        lag_blocks: opt(8)?,
    })
}

fn transition(
    sql: &impl Sql,
    from: Mode,
    to: Mode,
    reason: Reason,
    head: Option<u64>,
    now_ms: u64,
) -> Result<(), StoreError> {
    let cursor = crate::journal::cursor(sql)?;
    let opt = |v: Option<u64>| v.map_or(Ok(crate::sql::SqlValue::Null), bind_u64);
    sql.exec(
        "INSERT INTO mode_transitions
            (at_ms, from_mode, to_mode, reason, cursor_number, head_number)
         VALUES (?, ?, ?, ?, ?, ?)",
        &[
            bind_u64(now_ms)?,
            from.as_str().into(),
            to.as_str().into(),
            reason.as_str().into(),
            opt(cursor.map(|c| c.number))?,
            opt(head)?,
        ],
    )?;
    sql.exec(
        "UPDATE watchdog SET mode = ?, reason = ?, mode_since_ms = ? WHERE id = 1",
        &[
            to.as_str().into(),
            reason.as_str().into(),
            bind_u64(now_ms)?,
        ],
    )?;
    Ok(())
}

/// A valid, authenticated block delivery (new or a provider retry) arrived.
pub fn note_delivery(sql: &impl Sql, now_ms: u64) -> Result<(), StoreError> {
    sql.exec(
        "UPDATE watchdog SET last_delivery_ms = ? WHERE id = 1",
        &[bind_u64(now_ms)?],
    )?;
    let state = load(sql)?;
    if state.mode == Mode::Fallback && !state.behind {
        transition(
            sql,
            Mode::Fallback,
            Mode::Primary,
            Reason::WebhookResumed,
            None,
            now_ms,
        )?;
    }
    Ok(())
}

/// Start of a cron tick: records it and enters fallback on silence.
pub fn begin_tick(sql: &impl Sql, silence_ms: u64, now_ms: u64) -> Result<State, StoreError> {
    sql.exec(
        "UPDATE watchdog SET first_tick_ms = COALESCE(first_tick_ms, ?), last_tick_ms = ?
         WHERE id = 1",
        &[bind_u64(now_ms)?, bind_u64(now_ms)?],
    )?;
    let state = load(sql)?;
    if state.mode == Mode::Primary && state.silent(silence_ms, now_ms) {
        transition(
            sql,
            Mode::Primary,
            Mode::Fallback,
            Reason::WebhookSilent,
            None,
            now_ms,
        )?;
        return load(sql);
    }
    Ok(state)
}

/// True when this tick may read the RPC head (at most once per tick, even if
/// the cron fires twice). Marks the read as started.
pub fn take_head_read(
    sql: &impl Sql,
    min_interval_ms: u64,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let state = load(sql)?;
    if state
        .last_head_read_ms
        .is_some_and(|at| now_ms.saturating_sub(at) < min_interval_ms)
    {
        return Ok(false);
    }
    sql.exec(
        "UPDATE watchdog SET last_head_read_ms = ? WHERE id = 1",
        &[bind_u64(now_ms)?],
    )?;
    Ok(true)
}

/// After the tick's RPC head read: records lag and applies the behind rule.
pub fn after_head(
    sql: &impl Sql,
    silence_ms: u64,
    behind_blocks: u64,
    head: BlockRef,
    cursor: Option<BlockRef>,
    now_ms: u64,
) -> Result<State, StoreError> {
    let lag = cursor.map(|c| head.number.saturating_sub(c.number));
    let behind = lag.is_some_and(|l| l > behind_blocks);
    let opt = lag.map_or(Ok(crate::sql::SqlValue::Null), bind_u64)?;
    sql.exec(
        "UPDATE watchdog SET behind = ?, lag_blocks = ? WHERE id = 1",
        &[i64::from(behind).into(), opt],
    )?;
    let state = load(sql)?;
    let silent = state.silent(silence_ms, now_ms);
    match state.mode {
        Mode::Primary if behind => transition(
            sql,
            Mode::Primary,
            Mode::Fallback,
            Reason::Behind,
            Some(head.number),
            now_ms,
        )?,
        Mode::Fallback if !behind && !silent => {
            let reason = if state.reason.as_deref() == Some(Reason::Behind.as_str()) {
                Reason::CaughtUp
            } else {
                Reason::WebhookResumed
            };
            transition(
                sql,
                Mode::Fallback,
                Mode::Primary,
                reason,
                Some(head.number),
                now_ms,
            )?
        }
        _ => return Ok(state),
    }
    load(sql)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Transition {
    pub seq: u64,
    pub at_ms: u64,
    pub from_mode: String,
    pub to_mode: String,
    pub reason: String,
    pub cursor_number: Option<u64>,
    pub head_number: Option<u64>,
}

pub fn transitions(sql: &impl Sql, limit: u64) -> Result<Vec<Transition>, StoreError> {
    sql.exec(
        "SELECT seq, at_ms, from_mode, to_mode, reason, cursor_number, head_number
         FROM mode_transitions ORDER BY seq DESC LIMIT ?",
        &[bind_u64(limit)?],
    )?
    .iter()
    .map(|r| {
        Ok(Transition {
            seq: uint(r, 0)?,
            at_ms: uint(r, 1)?,
            from_mode: text(r, 2)?.to_owned(),
            to_mode: text(r, 3)?.to_owned(),
            reason: text(r, 4)?.to_owned(),
            cursor_number: crate::journal::opt_int(r, 5)?,
            head_number: crate::journal::opt_int(r, 6)?,
        })
    })
    .collect()
}

pub fn transition_count(sql: &impl Sql) -> Result<u64, StoreError> {
    let rows = sql.exec("SELECT COUNT(*) FROM mode_transitions", &[])?;
    rows.first().map_or(Ok(0), |r| uint(r, 0))
}
