//! Chain observation journal: delivery deduplication, the per-block path, the
//! finalized height/hash cursor and durable faults.
//!
//! Every function here is one step of a logical operation and runs inside the
//! single storage transaction the caller supplies (`transactionSync` in the
//! Durable Object).
//!
//! # The per-block path
//!
//! Webhook deliveries and RPC backfill feed the same path. A block is first
//! recorded as a block event, deduplicated by (stream, block hash), whichever
//! source supplied it first. [`drain`] then applies pending events in height
//! order: the event at `cursor + 1` advances the cursor only when its parent
//! hash equals the cursor hash. Events at or below the cursor are compared with
//! the retained finalized chain. A missing height is a gap that RPC backfills;
//! a broken parent link or two hashes at one height is a conflict, persisted as
//! a durable fault that pauses the path until an operator clears it.
//!
//! # Trust
//!
//! Notifications alone establish neither finality nor authorization. An
//! authenticated finalized-stream delivery from the configured subscription
//! advances the cursor without an RPC call, but only along an unbroken parent
//! chain from a cursor anchored on an RPC-reported finalized head, and the
//! blocks it adds are **provisional**. The separate RPC-confirmed watermark
//! ([`confirmed`]) moves only on independent RPC evidence:
//!
//! - the anchor itself, taken from the RPC finalized head;
//! - a watchdog head read whose hash equals the retained block at its height,
//!   at once or when the cursor reaches it;
//! - a block applied from an RPC hash list at or below the last RPC finalized
//!   head (the heartbeat plans no list above it, so every listed block is
//!   RPC's own finalized chain).
//!
//! Because every block hash commits to its parent, confirming one retained
//! height confirms the whole retained chain below it. A mismatch is a fault.
//! Anything that acts on a block (cohort transitions, scoring, signing) reads
//! the confirmed watermark, never the provisional cursor.

use crate::sql::{Sql, SqlValue, StoreError, bind_u64, text, uint};
use crate::webhook::BlockNotification;
use crate::wire::{BlockRef, FinalizedHeader, Id};
use serde::{Deserialize, Serialize};

/// The only accepted stream. Best and reorg handling is unqualified.
pub const STREAM_FINALIZED: &str = "finalized";
/// RPC audit points kept while the cursor has not reached them.
pub const MAX_PENDING_AUDITS: u64 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Webhook,
    Rpc,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Self::Webhook => "webhook",
            Self::Rpc => "rpc",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryOutcome {
    /// New delivery of a new block: recorded for the per-block path.
    Accepted,
    /// Same delivery ID and identical body: a provider retry.
    DuplicateDelivery,
    /// New delivery ID for a block already recorded, by an earlier delivery or
    /// by RPC backfill: recorded, not processed again.
    DuplicateBlock,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IngestError {
    /// Delivery ID reused with different body bytes.
    DeliveryConflict,
    /// Event ID reused with a different block.
    EventConflict,
    Store(StoreError),
}

impl From<StoreError> for IngestError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

fn id(value: &str) -> Result<Id, StoreError> {
    Id::parse(value).map_err(|_| StoreError("stored identifier"))
}

fn opt_u64(row: &[SqlValue], index: usize) -> Result<Option<u64>, StoreError> {
    match row.get(index) {
        Some(SqlValue::Null) | None => Ok(None),
        Some(SqlValue::Integer(v)) => u64::try_from(*v)
            .map(Some)
            .map_err(|_| StoreError("negative integer column")),
        Some(SqlValue::Text(_)) => Err(StoreError("expected integer column")),
    }
}

fn opt_id(row: &[SqlValue], index: usize) -> Result<Option<Id>, StoreError> {
    match row.get(index) {
        Some(SqlValue::Null) | None => Ok(None),
        Some(SqlValue::Text(v)) => id(v).map(Some),
        Some(SqlValue::Integer(_)) => Err(StoreError("expected text column")),
    }
}

/// Records an authenticated delivery and its block event. The acknowledgment
/// waits for the enclosing transaction to commit.
pub fn ingest_delivery(
    sql: &impl Sql,
    n: &BlockNotification,
    now_ms: u64,
) -> Result<DeliveryOutcome, IngestError> {
    let delivery = n.delivery_id.to_hex();
    let existing = sql.exec(
        "SELECT body_sha256 FROM deliveries WHERE delivery_id = ?",
        &[delivery.clone().into()],
    )?;
    if let Some(row) = existing.first() {
        return if text(row, 0)? == n.body_sha256.to_hex() {
            Ok(DeliveryOutcome::DuplicateDelivery)
        } else {
            Err(IngestError::DeliveryConflict)
        };
    }
    let event = n.event_id.to_hex();
    let same_event = sql.exec(
        "SELECT block_number, block_hash FROM deliveries
         WHERE event_id = ? AND block_hash IS NOT NULL LIMIT 1",
        &[event.clone().into()],
    )?;
    if let Some(row) = same_event.first()
        && (uint(row, 0)? != n.header.number || id(text(row, 1)?)? != n.header.hash)
    {
        return Err(IngestError::EventConflict);
    }
    sql.exec(
        "INSERT INTO deliveries
            (delivery_id, subscription, event_id, body_sha256, first_attempt, received_at_ms,
             block_number, block_hash)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            delivery.into(),
            n.subscription.to_hex().into(),
            event.into(),
            n.body_sha256.to_hex().into(),
            SqlValue::Integer(n.attempt.into()),
            bind_u64(now_ms)?,
            bind_u64(n.header.number)?,
            n.header.hash.to_hex().into(),
        ],
    )?;
    note_head(sql, n.header.block_ref(), now_ms)?;
    let new = record_block(
        sql,
        &n.header,
        Source::Webhook,
        Some(&n.delivery_id),
        now_ms,
    )?;
    Ok(if new {
        DeliveryOutcome::Accepted
    } else {
        DeliveryOutcome::DuplicateBlock
    })
}

/// Records a finalized block for the per-block path, once per (stream, hash).
/// Returns `false` when that block is already recorded.
pub fn record_block(
    sql: &impl Sql,
    header: &FinalizedHeader,
    source: Source,
    delivery: Option<&Id>,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let existing = sql.exec(
        "SELECT state FROM block_events WHERE stream = ? AND block_hash = ?",
        &[STREAM_FINALIZED.into(), header.hash.to_hex().into()],
    )?;
    if let Some(row) = existing.first() {
        // A block set aside by a fault the operator has since cleared is
        // judged again on its new evidence; any other repeat is a duplicate.
        if text(row, 0)? == "conflict" && active_fault(sql)?.is_none() {
            sql.exec(
                "UPDATE block_events SET block_number = ?, parent_hash = ?, source = ?,
                    delivery_id = ?, state = 'pending', received_at_ms = ?, resolved_at_ms = NULL
                 WHERE stream = ? AND block_hash = ?",
                &[
                    bind_u64(header.number)?,
                    header.parent_hash.to_hex().into(),
                    source.as_str().into(),
                    delivery.map_or(SqlValue::Null, |d| d.to_hex().into()),
                    bind_u64(now_ms)?,
                    STREAM_FINALIZED.into(),
                    header.hash.to_hex().into(),
                ],
            )?;
            bump(sql, "conflict_blocks_rejudged")?;
            return Ok(true);
        }
        bump(sql, &format!("duplicate_blocks_{}", source.as_str()))?;
        return Ok(false);
    }
    sql.exec(
        "INSERT INTO block_events
            (stream, block_hash, block_number, parent_hash, source, delivery_id, state,
             received_at_ms)
         VALUES (?, ?, ?, ?, ?, ?, 'pending', ?)",
        &[
            STREAM_FINALIZED.into(),
            header.hash.to_hex().into(),
            bind_u64(header.number)?,
            header.parent_hash.to_hex().into(),
            source.as_str().into(),
            delivery.map_or(SqlValue::Null, |d| d.to_hex().into()),
            bind_u64(now_ms)?,
        ],
    )?;
    Ok(true)
}

/// Tracks the highest notified finalized head, as a hint for `/status`.
fn note_head(sql: &impl Sql, block: BlockRef, now_ms: u64) -> Result<(), StoreError> {
    match notified_head(sql)? {
        Some(head) if block.number < head.number => Ok(()),
        Some(head) if block.number == head.number => {
            if block.hash != head.hash {
                bump(sql, "head_conflicts")?;
            }
            Ok(())
        }
        _ => {
            sql.exec(
                "INSERT INTO notified_head (id, block_number, block_hash, updated_at_ms)
                 VALUES (1, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET block_number = excluded.block_number,
                    block_hash = excluded.block_hash, updated_at_ms = excluded.updated_at_ms",
                &[
                    bind_u64(block.number)?,
                    block.hash.to_hex().into(),
                    bind_u64(now_ms)?,
                ],
            )?;
            Ok(())
        }
    }
}

// ------------------------------------------------------------------ faults

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultKind {
    /// The block at `cursor + 1` does not name the cursor as its parent.
    ParentMismatch,
    /// Two different hashes for one finalized height.
    HashConflict,
    /// The RPC finalized head disagrees with the block the cursor holds there.
    RpcHeadConflict,
    /// An RPC hash list does not start at the block the cursor holds.
    RpcJunctionMismatch,
}

impl FaultKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ParentMismatch => "parent_mismatch",
            Self::HashConflict => "hash_conflict",
            Self::RpcHeadConflict => "rpc_head_conflict",
            Self::RpcJunctionMismatch => "rpc_junction_mismatch",
        }
    }
}

/// A durable fault. Only public chain identifiers are stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Fault {
    pub id: u64,
    pub kind: String,
    pub block_number: u64,
    pub expected_hash: Option<Id>,
    pub observed_hash: Option<Id>,
    pub source: String,
    pub raised_at_ms: u64,
}

fn raise(
    sql: &impl Sql,
    kind: FaultKind,
    number: u64,
    expected: Option<Id>,
    observed: Option<Id>,
    source: &str,
    now_ms: u64,
) -> Result<(), StoreError> {
    if active_fault(sql)?.is_some() {
        return Ok(());
    }
    let hex = |h: Option<Id>| h.map_or(SqlValue::Null, |h| h.to_hex().into());
    sql.exec(
        "INSERT INTO faults
            (kind, block_number, expected_hash, observed_hash, source, raised_at_ms)
         VALUES (?, ?, ?, ?, ?, ?)",
        &[
            kind.as_str().into(),
            bind_u64(number)?,
            hex(expected),
            hex(observed),
            source.into(),
            bind_u64(now_ms)?,
        ],
    )?;
    bump(sql, "faults_raised")
}

pub fn active_fault(sql: &impl Sql) -> Result<Option<Fault>, StoreError> {
    let rows = sql.exec(
        "SELECT id, kind, block_number, expected_hash, observed_hash, source, raised_at_ms
         FROM faults WHERE cleared_at_ms IS NULL ORDER BY id LIMIT 1",
        &[],
    )?;
    rows.first()
        .map(|r| {
            Ok(Fault {
                id: uint(r, 0)?,
                kind: text(r, 1)?.to_owned(),
                block_number: uint(r, 2)?,
                expected_hash: opt_id(r, 3)?,
                observed_hash: opt_id(r, 4)?,
                source: text(r, 5)?.to_owned(),
                raised_at_ms: uint(r, 6)?,
            })
        })
        .transpose()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum ClearOutcome {
    Cleared {
        reanchor: bool,
    },
    NoActiveFault,
    /// The request named another fault; nothing changed.
    WrongFault {
        active: u64,
    },
}

/// Operator action: clears the active fault `fault_id`. With `reanchor`, the
/// cursor and the retained chain are dropped, so the next delivery or watchdog
/// tick anchors again on the RPC finalized head.
pub fn clear_fault(
    sql: &impl Sql,
    fault_id: u64,
    reanchor: bool,
    now_ms: u64,
) -> Result<ClearOutcome, StoreError> {
    let Some(active) = active_fault(sql)? else {
        return Ok(ClearOutcome::NoActiveFault);
    };
    if active.id != fault_id {
        return Ok(ClearOutcome::WrongFault { active: active.id });
    }
    sql.exec(
        "UPDATE faults SET cleared_at_ms = ?, clear_action = ? WHERE id = ?",
        &[
            bind_u64(now_ms)?,
            (if reanchor { "reanchor" } else { "resume" }).into(),
            bind_u64(fault_id)?,
        ],
    )?;
    if reanchor {
        sql.exec("DELETE FROM finalized_cursor", &[])?;
        sql.exec("DELETE FROM finalized_blocks", &[])?;
        sql.exec("DELETE FROM rpc_audits", &[])?;
        sql.exec("DELETE FROM rpc_confirmed", &[])?;
    }
    bump(sql, "faults_cleared")?;
    Ok(ClearOutcome::Cleared { reanchor })
}

// ---------------------------------------------------------------- counters

pub(crate) fn bump(sql: &impl Sql, name: &str) -> Result<(), StoreError> {
    add(sql, name, 1)
}

pub(crate) fn add(sql: &impl Sql, name: &str, amount: u64) -> Result<(), StoreError> {
    sql.exec(
        "INSERT INTO counters (name, value) VALUES (?, ?)
         ON CONFLICT(name) DO UPDATE SET value = value + excluded.value",
        &[name.into(), bind_u64(amount)?],
    )?;
    Ok(())
}

pub fn counter(sql: &impl Sql, name: &str) -> Result<u64, StoreError> {
    let rows = sql.exec("SELECT value FROM counters WHERE name = ?", &[name.into()])?;
    rows.first().map_or(Ok(0), |row| uint(row, 0))
}

pub fn counters(sql: &impl Sql) -> Result<Vec<(String, u64)>, StoreError> {
    sql.exec("SELECT name, value FROM counters ORDER BY name", &[])?
        .iter()
        .map(|r| Ok((text(r, 0)?.to_owned(), uint(r, 1)?)))
        .collect()
}

// --------------------------------------------------------- confirmation

/// The independent RPC evidence that moved the confirmed watermark.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// The cursor's anchor, read from the RPC finalized head.
    Anchor,
    /// A watchdog RPC finalized head equal to the retained block.
    RpcHead,
    /// A block from an RPC hash list at or below the RPC finalized head.
    RpcList,
}

impl Basis {
    fn as_str(self) -> &'static str {
        match self {
            Self::Anchor => "anchor",
            Self::RpcHead => "rpc_head",
            Self::RpcList => "rpc_list",
        }
    }
}

/// The highest retained finalized block that independent RPC evidence covers.
/// Never above the cursor. Work that acts on a block waits for this watermark.
pub fn confirmed(sql: &impl Sql) -> Result<Option<BlockRef>, StoreError> {
    block_ref(
        sql,
        "SELECT block_number, block_hash FROM rpc_confirmed WHERE id = 1",
    )
}

/// Raises the confirmed watermark to the retained block at `number`. It never
/// moves down, and a height the retained chain does not hold changes nothing.
fn confirm(sql: &impl Sql, number: u64, basis: Basis, now_ms: u64) -> Result<(), StoreError> {
    if confirmed(sql)?.is_some_and(|c| c.number >= number) {
        return Ok(());
    }
    let Some(hash) = finalized_hash(sql, number)? else {
        return Ok(());
    };
    sql.exec(
        "INSERT INTO rpc_confirmed (id, block_number, block_hash, basis, updated_at_ms)
         VALUES (1, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET block_number = excluded.block_number,
            block_hash = excluded.block_hash, basis = excluded.basis,
            updated_at_ms = excluded.updated_at_ms",
        &[
            bind_u64(number)?,
            hash.to_hex().into(),
            basis.as_str().into(),
            bind_u64(now_ms)?,
        ],
    )?;
    bump(sql, &format!("confirmations_{}", basis.as_str()))
}

// ------------------------------------------------------------------ cursor

fn block_ref(sql: &impl Sql, query: &str) -> Result<Option<BlockRef>, StoreError> {
    let rows = sql.exec(query, &[])?;
    rows.first()
        .map(|row| {
            Ok(BlockRef {
                number: uint(row, 0)?,
                hash: id(text(row, 1)?)?,
            })
        })
        .transpose()
}

pub fn cursor(sql: &impl Sql) -> Result<Option<BlockRef>, StoreError> {
    block_ref(
        sql,
        "SELECT block_number, block_hash FROM finalized_cursor WHERE id = 1",
    )
}

pub fn notified_head(sql: &impl Sql) -> Result<Option<BlockRef>, StoreError> {
    block_ref(
        sql,
        "SELECT block_number, block_hash FROM notified_head WHERE id = 1",
    )
}

pub fn rpc_finalized_head(sql: &impl Sql) -> Result<Option<BlockRef>, StoreError> {
    block_ref(
        sql,
        "SELECT block_number, block_hash FROM rpc_finalized_head WHERE id = 1",
    )
}

/// The retained finalized hash at `number`, if the cursor has passed it.
pub fn finalized_hash(sql: &impl Sql, number: u64) -> Result<Option<Id>, StoreError> {
    let rows = sql.exec(
        "SELECT block_hash FROM finalized_blocks WHERE block_number = ?",
        &[bind_u64(number)?],
    )?;
    rows.first().map(|r| id(text(r, 0)?)).transpose()
}

fn write_cursor(
    sql: &impl Sql,
    block: BlockRef,
    source: &str,
    now_ms: u64,
) -> Result<(), StoreError> {
    sql.exec(
        "INSERT INTO finalized_cursor (id, block_number, block_hash, updated_at_ms)
         VALUES (1, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET block_number = excluded.block_number,
            block_hash = excluded.block_hash, updated_at_ms = excluded.updated_at_ms",
        &[
            bind_u64(block.number)?,
            block.hash.to_hex().into(),
            bind_u64(now_ms)?,
        ],
    )?;
    sql.exec(
        "INSERT INTO finalized_blocks (block_number, block_hash, source, applied_at_ms)
         VALUES (?, ?, ?, ?)",
        &[
            bind_u64(block.number)?,
            block.hash.to_hex().into(),
            source.into(),
            bind_u64(now_ms)?,
        ],
    )?;
    Ok(())
}

/// Sets the first cursor position from an RPC-reported finalized head. Returns
/// `false`, changing nothing, when a cursor already exists (a concurrent anchor
/// won) or a fault is active.
pub fn anchor(sql: &impl Sql, block: BlockRef, now_ms: u64) -> Result<bool, StoreError> {
    if cursor(sql)?.is_some() || active_fault(sql)?.is_some() {
        return Ok(false);
    }
    write_cursor(sql, block, "anchor", now_ms)?;
    confirm(sql, block.number, Basis::Anchor, now_ms)?;
    sql.exec(
        "DELETE FROM rpc_audits WHERE block_number <= ?",
        &[bind_u64(block.number)?],
    )?;
    bump(sql, "anchors")?;
    Ok(true)
}

/// Records the finalized head an RPC endpoint reported and audits the cursor
/// against it. The stored head never moves down; a different hash at its height
/// is counted. When the cursor already holds that height, the hashes are
/// compared now; otherwise the head is kept as an audit point that the cursor
/// checks when it reaches that height.
pub fn note_rpc_head(sql: &impl Sql, head: BlockRef, now_ms: u64) -> Result<(), StoreError> {
    match rpc_finalized_head(sql)? {
        Some(h) if head.number < h.number => {}
        Some(h) if head.number == h.number && head.hash != h.hash => {
            bump(sql, "rpc_head_conflicts")?;
        }
        _ => {
            sql.exec(
                "INSERT INTO rpc_finalized_head (id, block_number, block_hash, fetched_at_ms)
                 VALUES (1, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET block_number = excluded.block_number,
                    block_hash = excluded.block_hash, fetched_at_ms = excluded.fetched_at_ms",
                &[
                    bind_u64(head.number)?,
                    head.hash.to_hex().into(),
                    bind_u64(now_ms)?,
                ],
            )?;
        }
    }
    let Some(c) = cursor(sql)? else {
        return Ok(());
    };
    if head.number <= c.number {
        return match finalized_hash(sql, head.number)? {
            Some(held) if held == head.hash => {
                bump(sql, "rpc_audits_passed")?;
                confirm(sql, head.number, Basis::RpcHead, now_ms)
            }
            Some(held) => raise(
                sql,
                FaultKind::RpcHeadConflict,
                head.number,
                Some(head.hash),
                Some(held),
                "rpc",
                now_ms,
            ),
            None => bump(sql, "rpc_audits_unretained"),
        };
    }
    sql.exec(
        "INSERT INTO rpc_audits (block_number, block_hash, observed_at_ms) VALUES (?, ?, ?)
         ON CONFLICT(block_number) DO NOTHING",
        &[
            bind_u64(head.number)?,
            head.hash.to_hex().into(),
            bind_u64(now_ms)?,
        ],
    )?;
    sql.exec(
        "DELETE FROM rpc_audits WHERE block_number NOT IN
            (SELECT block_number FROM rpc_audits ORDER BY block_number DESC LIMIT ?)",
        &[bind_u64(MAX_PENDING_AUDITS)?],
    )?;
    Ok(())
}

// --------------------------------------------------------- per-block drain

/// Why [`drain`] stopped before every recorded block was resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "halt")]
pub enum Halt {
    /// No cursor yet: anchor on the RPC finalized head.
    NotAnchored,
    /// Heights `from..=to` are missing below a recorded block: backfill them.
    Gap { from: u64, to: u64 },
    /// A durable fault pauses the path.
    Fault { id: u64 },
    /// The per-transaction bound was reached; the next invocation continues.
    Bound,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub applied: u64,
    pub covered: u64,
    /// Provisional: may include webhook blocks no RPC evidence covers yet.
    pub cursor: Option<BlockRef>,
    /// The RPC-confirmed watermark, at or below the cursor.
    #[serde(default)]
    pub confirmed: Option<BlockRef>,
    pub halt: Option<Halt>,
}

struct Pending {
    number: u64,
    hash: Id,
    parent: Id,
    source: String,
}

fn pending(sql: &impl Sql, query: &str, number: u64) -> Result<Vec<Pending>, StoreError> {
    sql.exec(query, &[STREAM_FINALIZED.into(), bind_u64(number)?])?
        .iter()
        .map(|r| {
            Ok(Pending {
                number: uint(r, 0)?,
                hash: id(text(r, 1)?)?,
                parent: id(text(r, 2)?)?,
                source: text(r, 3)?.to_owned(),
            })
        })
        .collect()
}

fn set_state(sql: &impl Sql, hash: Id, state: &str, now_ms: u64) -> Result<(), StoreError> {
    sql.exec(
        "UPDATE block_events SET state = ?, resolved_at_ms = ? WHERE stream = ? AND block_hash = ?",
        &[
            state.into(),
            bind_u64(now_ms)?,
            STREAM_FINALIZED.into(),
            hash.to_hex().into(),
        ],
    )?;
    Ok(())
}

/// Resolves pending events at or below the cursor against the retained chain.
/// Returns `true` when it raised a fault.
fn resolve_covered(
    sql: &impl Sql,
    c: BlockRef,
    p: &mut Progress,
    now_ms: u64,
) -> Result<bool, StoreError> {
    let rows = pending(
        sql,
        "SELECT block_number, block_hash, parent_hash, source FROM block_events
         WHERE stream = ? AND state = 'pending' AND block_number <= ? ORDER BY block_number",
        c.number,
    )?;
    for e in rows {
        match finalized_hash(sql, e.number)? {
            Some(held) if held == e.hash => {
                set_state(sql, e.hash, "covered", now_ms)?;
                p.covered += 1;
            }
            Some(held) => {
                set_state(sql, e.hash, "conflict", now_ms)?;
                raise(
                    sql,
                    FaultKind::HashConflict,
                    e.number,
                    Some(held),
                    Some(e.hash),
                    &e.source,
                    now_ms,
                )?;
                return Ok(true);
            }
            None => {
                set_state(sql, e.hash, "below_anchor", now_ms)?;
                p.covered += 1;
            }
        }
    }
    Ok(false)
}

fn apply(sql: &impl Sql, e: &Pending, now_ms: u64) -> Result<(), StoreError> {
    write_cursor(
        sql,
        BlockRef {
            number: e.number,
            hash: e.hash,
        },
        &e.source,
        now_ms,
    )?;
    set_state(sql, e.hash, "applied", now_ms)?;
    bump(sql, &format!("applied_{}", e.source))?;
    let audit = sql.exec(
        "SELECT block_hash FROM rpc_audits WHERE block_number = ?",
        &[bind_u64(e.number)?],
    )?;
    if let Some(row) = audit.first() {
        let expected = id(text(row, 0)?)?;
        sql.exec(
            "DELETE FROM rpc_audits WHERE block_number <= ?",
            &[bind_u64(e.number)?],
        )?;
        if expected == e.hash {
            bump(sql, "rpc_audits_passed")?;
            confirm(sql, e.number, Basis::RpcHead, now_ms)?;
        } else {
            raise(
                sql,
                FaultKind::RpcHeadConflict,
                e.number,
                Some(expected),
                Some(e.hash),
                &e.source,
                now_ms,
            )?;
        }
    }
    // A hash-list block at or below the RPC finalized head is RPC's own
    // finalized chain, joined to the retained chain at the list's junction.
    if e.source == Source::Rpc.as_str()
        && active_fault(sql)?.is_none()
        && rpc_finalized_head(sql)?.is_some_and(|h| e.number <= h.number)
    {
        confirm(sql, e.number, Basis::RpcList, now_ms)?;
    }
    Ok(())
}

/// The per-block path: applies recorded blocks in height order, at most `max`
/// per call, and reports why it stopped.
pub fn drain(sql: &impl Sql, max: u64, now_ms: u64) -> Result<Progress, StoreError> {
    let mut p = Progress::default();
    loop {
        p.cursor = cursor(sql)?;
        p.confirmed = confirmed(sql)?;
        if let Some(fault) = active_fault(sql)? {
            p.halt = Some(Halt::Fault { id: fault.id });
            return Ok(p);
        }
        let Some(c) = p.cursor else {
            p.halt = Some(Halt::NotAnchored);
            return Ok(p);
        };
        if resolve_covered(sql, c, &mut p, now_ms)? {
            continue;
        }
        if p.applied >= max {
            p.halt = Some(Halt::Bound);
            return Ok(p);
        }
        let next_height = c.number + 1;
        let next = pending(
            sql,
            "SELECT block_number, block_hash, parent_hash, source FROM block_events
             WHERE stream = ? AND state = 'pending' AND block_number = ?",
            next_height,
        )?;
        match next.as_slice() {
            [] => {
                let rows = sql.exec(
                    "SELECT MIN(block_number) FROM block_events
                     WHERE stream = ? AND state = 'pending' AND block_number > ?",
                    &[STREAM_FINALIZED.into(), bind_u64(next_height)?],
                )?;
                let lowest = rows.first().map_or(Ok(None), |r| opt_u64(r, 0))?;
                p.halt = lowest.map(|l| Halt::Gap {
                    from: next_height,
                    to: l - 1,
                });
                return Ok(p);
            }
            [e] if e.parent == c.hash => {
                apply(sql, e, now_ms)?;
                p.applied += 1;
            }
            [e] => {
                set_state(sql, e.hash, "conflict", now_ms)?;
                raise(
                    sql,
                    FaultKind::ParentMismatch,
                    e.number,
                    Some(c.hash),
                    Some(e.parent),
                    &e.source,
                    now_ms,
                )?;
            }
            [a, b, ..] => {
                for e in &next {
                    set_state(sql, e.hash, "conflict", now_ms)?;
                }
                raise(
                    sql,
                    FaultKind::HashConflict,
                    next_height,
                    Some(a.hash),
                    Some(b.hash),
                    "mixed",
                    now_ms,
                )?;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HashesOutcome {
    /// `recorded` new blocks entered the per-block path. `complete` is false
    /// when the list stopped at a height the node did not serve yet.
    Recorded { recorded: u64, complete: bool },
    /// The list no longer joins the retained chain (for example after a
    /// re-anchor) or is empty; nothing was recorded.
    Unusable,
    /// The hash at the junction height disagrees with the retained chain.
    Fault,
}

/// Applies a `chain_getBlockHash` list for heights `from, from + 1, ...`. The
/// first entry must equal the retained finalized hash at `from` (the cursor
/// when the call was planned); later entries become blocks whose parent is the
/// previous entry. The list stops at the first height the node did not have.
pub fn apply_hashes(
    sql: &impl Sql,
    from: u64,
    hashes: &[Option<Id>],
    now_ms: u64,
) -> Result<HashesOutcome, StoreError> {
    let first = match hashes.first() {
        Some(Some(first)) => first,
        // The node does not serve the junction height yet.
        Some(None) => return Ok(HashesOutcome::Recorded { recorded: 0, complete: false }),
        None => return Ok(HashesOutcome::Unusable),
    };
    match finalized_hash(sql, from)? {
        None => return Ok(HashesOutcome::Unusable),
        Some(held) if held != *first => {
            raise(
                sql,
                FaultKind::RpcJunctionMismatch,
                from,
                Some(held),
                Some(*first),
                "rpc",
                now_ms,
            )?;
            return Ok(HashesOutcome::Fault);
        }
        Some(_) => {}
    }
    let mut recorded = 0;
    let mut complete = true;
    let mut parent = *first;
    for (offset, hash) in hashes.iter().enumerate().skip(1) {
        let Some(hash) = *hash else {
            complete = false;
            break;
        };
        let header = FinalizedHeader {
            number: from + offset as u64,
            hash,
            parent_hash: parent,
        };
        if record_block(sql, &header, Source::Rpc, None, now_ms)? {
            recorded += 1;
        }
        parent = hash;
    }
    Ok(HashesOutcome::Recorded { recorded, complete })
}

// ------------------------------------------------------------------ status

/// Content-free journal status: counts, heights and public block hashes only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct JournalStatus {
    /// Provisional cursor: webhook blocks advance it before RPC confirms them.
    pub cursor: Option<BlockRef>,
    pub cursor_age_ms: Option<u64>,
    /// RPC-confirmed watermark; work that acts on a block reads this.
    pub confirmed: Option<BlockRef>,
    pub confirmed_basis: Option<String>,
    pub confirmed_age_ms: Option<u64>,
    /// Cursor height minus confirmed height: blocks awaiting RPC evidence.
    pub provisional_blocks: Option<u64>,
    /// Highest finalized head recorded from RPC.
    pub rpc_finalized_head: Option<BlockRef>,
    pub rpc_head_age_ms: Option<u64>,
    /// RPC finalized height minus cursor height, when both exist.
    pub lag_blocks: Option<u64>,
    pub notified_head: Option<BlockRef>,
    pub notified_head_age_ms: Option<u64>,
    pub deliveries: u64,
    pub events_pending: u64,
    pub events_applied: u64,
    pub events_covered: u64,
    pub events_below_anchor: u64,
    pub events_conflict: u64,
    pub retained_blocks: u64,
    pub pending_audits: u64,
    pub fault: Option<Fault>,
    pub faults_total: u64,
}

fn scalar(sql: &impl Sql, query: &str) -> Result<Option<u64>, StoreError> {
    let rows = sql.exec(query, &[])?;
    rows.first().map_or(Ok(None), |r| opt_u64(r, 0))
}

pub fn status(sql: &impl Sql, now_ms: u64) -> Result<JournalStatus, StoreError> {
    let c = cursor(sql)?;
    let conf = confirmed(sql)?;
    let rpc_head = rpc_finalized_head(sql)?;
    let age = |at: Option<u64>| at.map(|at| now_ms.saturating_sub(at));
    let mut events = [0u64; 5];
    for row in &sql.exec(
        "SELECT state, COUNT(*) FROM block_events GROUP BY state",
        &[],
    )? {
        let slot = match text(row, 0)? {
            "pending" => 0,
            "applied" => 1,
            "covered" => 2,
            "below_anchor" => 3,
            _ => 4,
        };
        if let Some(count) = events.get_mut(slot) {
            *count = uint(row, 1)?;
        }
    }
    let count = |q| scalar(sql, q).map(|v| v.unwrap_or(0));
    Ok(JournalStatus {
        cursor: c,
        cursor_age_ms: age(scalar(
            sql,
            "SELECT updated_at_ms FROM finalized_cursor WHERE id = 1",
        )?),
        confirmed: conf,
        confirmed_basis: sql
            .exec("SELECT basis FROM rpc_confirmed WHERE id = 1", &[])?
            .first()
            .map(|r| text(r, 0).map(str::to_owned))
            .transpose()?,
        confirmed_age_ms: age(scalar(
            sql,
            "SELECT updated_at_ms FROM rpc_confirmed WHERE id = 1",
        )?),
        provisional_blocks: match (c, conf) {
            (Some(c), Some(k)) => Some(c.number.saturating_sub(k.number)),
            _ => None,
        },
        rpc_finalized_head: rpc_head,
        rpc_head_age_ms: age(scalar(
            sql,
            "SELECT fetched_at_ms FROM rpc_finalized_head WHERE id = 1",
        )?),
        lag_blocks: match (c, rpc_head) {
            (Some(c), Some(h)) => Some(h.number.saturating_sub(c.number)),
            _ => None,
        },
        notified_head: notified_head(sql)?,
        notified_head_age_ms: age(scalar(
            sql,
            "SELECT updated_at_ms FROM notified_head WHERE id = 1",
        )?),
        deliveries: count("SELECT COUNT(*) FROM deliveries")?,
        events_pending: events[0],
        events_applied: events[1],
        events_covered: events[2],
        events_below_anchor: events[3],
        events_conflict: events[4],
        retained_blocks: count("SELECT COUNT(*) FROM finalized_blocks")?,
        pending_audits: count("SELECT COUNT(*) FROM rpc_audits")?,
        fault: active_fault(sql)?,
        faults_total: count("SELECT COUNT(*) FROM faults")?,
    })
}

/// Integer helper re-exported for sibling modules.
pub(crate) fn opt_int(row: &[SqlValue], index: usize) -> Result<Option<u64>, StoreError> {
    opt_u64(row, index)
}

#[cfg(test)]
mod tests;
