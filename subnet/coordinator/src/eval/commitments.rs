//! The commitment log: every finalized `Commitments.CommitmentOf` entry of
//! the subnet that admission can use, with its position and the committing
//! registration (`docs/testnet/admission-api.md`, section 2).
//!
//! The chain keeps only each hotkey's latest commitment, and ordinary RPC
//! nodes keep state for recent blocks only, so admission never reads a past
//! commitment from current state. The reader runs on the RPC-confirmed
//! watermark (the eval Worker's finalized cursor), from the cron tick, inside
//! the shared per-minute RPC budget (each call is reserved first):
//!
//! - **Poll.** While a cohort admits, at most once a minute, it lists the
//!   subnet's `CommitmentOf` keys at the confirmed block
//!   (`state_getKeysPaged`) and reads their values with `System.
//!   LastRuntimeUpgrade` in one `state_queryStorageAt`. A value carries the
//!   block that set it, so a changed `(block, payload)` is a new commitment.
//! - **Resolve.** For each new commitment at block `c`: `c`'s hash, then
//!   `System.Events` and `Uids(netuid, who)` at `c`, then
//!   `BlockAtRegistration(netuid, uid)` at `c`. The position is the index
//!   of the last extrinsic that emitted `Commitments.Commitment { netuid,
//!   who }` for the hotkey in `c`. These reads normally happen within
//!   minutes of the commitment; state the endpoints no longer keep makes the
//!   commitment unresolvable (session opening answers
//!   `commitment_unresolvable`, and the miner commits again), never
//!   substituted by later state.
//! - **Snapshot.** At the cutoff, for every hotkey with a verified session:
//!   `CommitmentOf`, `Uids`, `Owner` and then `BlockAtRegistration` at the hash of
//!   `cutoff_block - 1`, the state the last block before the cutoff left.
//!   That is exactly "the last commitment before the cutoff block" and the
//!   registration that held then; the freeze reads nothing else.
//!
//! Failures stay local. A value or event list that does not decode leaves
//! that hotkey's commitment unresolvable (an empty or unusual
//! `CommitmentInfo` opens no submission) and never aborts the round. State a
//! node no longer keeps (its "state discarded" error) ends a resolution as
//! unresolvable and restarts a pinned key listing at the current block; at
//! the snapshot it pauses the closing cohort ([`SNAPSHOT_DISCARDED`]) until
//! a read answers. The Worker reads such state again from `ARCHIVE_RPC_URL`
//! when one is configured before either applies. A commitment whose extrinsic also committed for
//! another hotkey on the subnet (a batch) has no position the protocol
//! orders and is unresolvable ([`SHARED_EXTRINSIC`]). Every read at a past
//! block also reads `LastRuntimeUpgrade` there: a resolution stage or a
//! snapshot read under another runtime than the stored one reads the
//! metadata at that block and is read again, decoded by name with it; a
//! runtime upgrade never makes a commitment unresolvable. A commitment that
//! is terminally unresolvable answers the miner `commitment_unresolvable`
//! with the reason ([`terminal`]), so it commits again.
//!
//! The reader has its own share of the per-minute RPC budget
//! (`READER_RPC_CALLS_PER_MINUTE`, by default the budget less the
//! heartbeat's floor and the epoch read), so a busy log never starves the
//! heartbeat; a round holds at most one large read, so its replies fit the
//! cohort object's body bound. The snapshot comes first, then resolutions
//! oldest first, then the poll.
//!
//! Overwrites between two polls are not logged: a commitment that a later one
//! replaced before it was seen counts for no cohort (only the latest counts
//! while a cohort admits, and only the last before the cutoff at the close,
//! which the snapshot reads exactly). Reads scale with commitments, not
//! sessions: about two calls a minute while a cohort admits, four per new
//! commitment and three per verified hotkey at the cutoff.
//!
//! Storage is decoded by name from the runtime's own V15 metadata
//! (`state_call Metadata_metadata_at_version`), kept beside the log and
//! re-read whenever `LastRuntimeUpgrade` reports another spec version. No
//! layout or version is compiled in.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)
)]

use crate::chain::{self, ChainRuntime, Value, as_account, as_bytes, as_list, as_u64, field};
use crate::eval::engine::{EvalError, R, i, opt_int, opt_text, s};
use crate::rpc;
use crate::sql::{Sql, SqlValue, bind_u64, int, text, uint};
use crate::wire::{BlockRef, Id};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use slop_ninja_protocol::AccountId;

pub const PALLET: &str = "Commitments";
pub const ITEM: &str = "CommitmentOf";
pub const EVENT: &str = "Commitment";
/// Keys per `state_getKeysPaged` page: the method's own maximum.
pub const KEYS_PAGE: u32 = 1000;
/// The note of a commitment whose extrinsic committed for several hotkeys.
pub const SHARED_EXTRINSIC: &str = "shared_extrinsic";
/// The fault of a closing cohort whose snapshot block's state the node no
/// longer keeps.
pub const SNAPSHOT_DISCARDED: &str = "snapshot_state_discarded";
/// Calls one runtime read takes (version, metadata, marker).
pub const RUNTIME_READS: usize = 3;
/// The calls a minute the shared budget always leaves the heartbeat (two
/// head reads' worth, as signing's share does) and the tick's epoch read.
pub const READER_FLOOR: u64 = 4 + 1;

/// The reader's share of the per-minute RPC budget: the configured
/// `READER_RPC_CALLS_PER_MINUTE`, else the budget less [`READER_FLOOR`];
/// never more than that. A share below [`RUNTIME_READS`] cannot read the
/// runtime, and the reader then plans nothing.
pub fn reader_share(per_minute: u64, configured: Option<u64>) -> u64 {
    let most = per_minute.saturating_sub(READER_FLOOR);
    configured.unwrap_or(most).min(most)
}
/// The poll's spacing while a cohort admits: the cron tick's.
pub const POLL_EVERY_MS: u64 = 60_000;

/// The log's tables (part of the eval cohort store's v10 migration).
pub const SCHEMA: &[&str] = &[
    "CREATE TABLE commit_log (
        hotkey TEXT NOT NULL, block INTEGER NOT NULL, block_hash TEXT NOT NULL, extrinsic INTEGER NOT NULL,
        payload TEXT NOT NULL, uid INTEGER, registration_block INTEGER, logged_ms INTEGER NOT NULL,
        PRIMARY KEY (hotkey, block)
    ) STRICT",
    "CREATE TABLE commit_seen (
        hotkey TEXT PRIMARY KEY, block INTEGER NOT NULL, payload TEXT NOT NULL, seen_block INTEGER NOT NULL,
        stage TEXT NOT NULL CHECK (stage IN ('hash','events','reg','logged','unresolvable')),
        block_hash TEXT, extrinsic INTEGER, uid INTEGER, note TEXT
    ) STRICT",
    "CREATE TABLE commit_reader (
        id INTEGER PRIMARY KEY CHECK (id = 1), keys_block INTEGER, keys_hash TEXT, keys TEXT NOT NULL DEFAULT '[]',
        keys_next TEXT, keys_done INTEGER NOT NULL DEFAULT 0, polled_ms INTEGER NOT NULL DEFAULT 0,
        observed_upgrade TEXT, polls INTEGER NOT NULL DEFAULT 0, reads INTEGER NOT NULL DEFAULT 0,
        last_error TEXT, refresh_hash TEXT, refresh_block INTEGER,
        budget_window_ms INTEGER NOT NULL DEFAULT 0, budget_used INTEGER NOT NULL DEFAULT 0,
        discarded INTEGER NOT NULL DEFAULT 0, undecodable INTEGER NOT NULL DEFAULT 0
    ) STRICT",
    "INSERT INTO commit_reader (id) VALUES (1)",
    "CREATE TABLE chain_runtime (
        id INTEGER PRIMARY KEY CHECK (id = 1), spec_version INTEGER NOT NULL, tx_version INTEGER NOT NULL,
        metadata TEXT NOT NULL, at_block INTEGER NOT NULL, upgrade TEXT NOT NULL
    ) STRICT",
];

/// One logged commitment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub hotkey: AccountId,
    pub block: u64,
    pub block_hash: String,
    pub extrinsic: u32,
    pub payload: Vec<u8>,
    /// The committing hotkey's UID and registration block at `block`;
    /// `None` when it held no UID there.
    pub uid: Option<u16>,
    pub registration_block: Option<u64>,
}

fn hk(h: &AccountId) -> SqlValue {
    s(hex::encode(h.0))
}

fn entry_of(r: &crate::sql::Row) -> R<LogEntry> {
    let hotkey = crate::eval::plan::parse_hex32(text(r, 0)?).ok_or(EvalError::State("log hotkey"))?;
    Ok(LogEntry {
        hotkey: AccountId(hotkey),
        block: uint(r, 1)?,
        block_hash: text(r, 2)?.to_owned(),
        extrinsic: u32::try_from(int(r, 3)?).map_err(|_| EvalError::State("log extrinsic"))?,
        payload: hex::decode(text(r, 4)?).map_err(|_| EvalError::State("log payload"))?,
        uid: opt_int(r, 5).and_then(|u| u16::try_from(u).ok()),
        registration_block: opt_int(r, 6).and_then(|b| u64::try_from(b).ok()),
    })
}

const COLUMNS: &str = "hotkey, block, block_hash, extrinsic, payload, uid, registration_block";

/// The hotkey's latest logged commitment.
pub fn latest(sql: &impl Sql, hotkey: &AccountId) -> R<Option<LogEntry>> {
    let rows = sql.exec(
        &format!("SELECT {COLUMNS} FROM commit_log WHERE hotkey = ? ORDER BY block DESC LIMIT 1"),
        &[hk(hotkey)],
    )?;
    rows.first().map(entry_of).transpose()
}

/// The hotkey's last logged commitment before `cutoff` (exclusive).
pub fn last_before(sql: &impl Sql, hotkey: &AccountId, cutoff: u64) -> R<Option<LogEntry>> {
    let rows = sql.exec(
        &format!("SELECT {COLUMNS} FROM commit_log WHERE hotkey = ? AND block < ? ORDER BY block DESC LIMIT 1"),
        &[hk(hotkey), bind_u64(cutoff)?],
    )?;
    rows.first().map(entry_of).transpose()
}

/// The hotkey's logged commitment at exactly `block`.
pub fn at_block(sql: &impl Sql, hotkey: &AccountId, block: u64) -> R<Option<LogEntry>> {
    let rows = sql.exec(
        &format!("SELECT {COLUMNS} FROM commit_log WHERE hotkey = ? AND block = ?"),
        &[hk(hotkey), bind_u64(block)?],
    )?;
    rows.first().map(entry_of).transpose()
}

/// The hotkey's latest seen commitment, when it is terminally unresolvable:
/// `(block, reason)`. Its opening can never be checked; the miner commits
/// again.
pub fn terminal(sql: &impl Sql, hotkey: &AccountId) -> R<Option<(u64, String)>> {
    let rows = sql.exec(
        "SELECT block, COALESCE(note, 'unresolvable') FROM commit_seen WHERE hotkey = ? AND stage = 'unresolvable'",
        &[hk(hotkey)],
    )?;
    rows.first().map(|r| Ok((uint(r, 0)?, text(r, 1)?.to_owned()))).transpose()
}

/// Whether the latest value a poll saw for the hotkey is not logged yet
/// (its position or registration is still being read).
pub fn unresolved(sql: &impl Sql, hotkey: &AccountId) -> R<bool> {
    Ok(!sql
        .exec(
            "SELECT 1 FROM commit_seen WHERE hotkey = ? AND stage IN ('hash','events','reg')",
            &[hk(hotkey)],
        )?
        .is_empty())
}

/// The block of the last completed poll: every commitment finalized at or
/// before it is either logged or seen (`unresolved`/`terminal`). `None`
/// until a poll completes.
pub fn polled_block(sql: &impl Sql) -> R<Option<u64>> {
    Ok(sql
        .exec("SELECT polled_block FROM commit_reader WHERE id = 1", &[])?
        .first()
        .and_then(|r| opt_int(r, 0))
        .and_then(|b| u64::try_from(b).ok()))
}

/// Logs one commitment (idempotent on `(hotkey, block)`).
pub fn record(sql: &impl Sql, e: &LogEntry, now_ms: u64) -> R<()> {
    sql.exec(
        "INSERT INTO commit_log (hotkey, block, block_hash, extrinsic, payload, uid, registration_block, logged_ms)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (hotkey, block) DO UPDATE SET block_hash = excluded.block_hash, extrinsic = excluded.extrinsic,
            payload = excluded.payload, uid = excluded.uid, registration_block = excluded.registration_block",
        &[
            hk(&e.hotkey),
            bind_u64(e.block)?,
            s(&e.block_hash),
            i(i64::from(e.extrinsic)),
            s(hex::encode(&e.payload)),
            e.uid.map_or(SqlValue::Null, |u| i(i64::from(u))),
            e.registration_block.map_or(Ok(SqlValue::Null), bind_u64)?,
            bind_u64(now_ms)?,
        ],
    )?;
    Ok(())
}

// ------------------------------------------------------------ runtime store

/// Stores the runtime the reader decodes with, with the
/// `System.LastRuntimeUpgrade` value read beside it (its upgrade marker).
pub fn store_runtime(sql: &impl Sql, spec: u32, tx: u32, metadata: &[u8], at_block: u64, upgrade: &str) -> R<()> {
    sql.exec(
        "INSERT INTO chain_runtime (id, spec_version, tx_version, metadata, at_block, upgrade) VALUES (1, ?, ?, ?, ?, ?)
         ON CONFLICT (id) DO UPDATE SET spec_version = excluded.spec_version, tx_version = excluded.tx_version,
            metadata = excluded.metadata, at_block = excluded.at_block, upgrade = excluded.upgrade",
        &[i(i64::from(spec)), i(i64::from(tx)), s(hex::encode(metadata)), bind_u64(at_block)?, s(upgrade)],
    )?;
    sql.exec(
        "UPDATE commit_reader SET observed_upgrade = ?, refresh_hash = NULL, refresh_block = NULL WHERE id = 1",
        &[s(upgrade)],
    )?;
    Ok(())
}

/// The stored runtime's `(spec, tx, metadata)`.
pub fn load_runtime(sql: &impl Sql) -> R<Option<(u32, u32, Vec<u8>)>> {
    let rows = sql.exec(
        "SELECT spec_version, tx_version, metadata FROM chain_runtime WHERE id = 1",
        &[],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    let spec = u32::try_from(int(r, 0)?).map_err(|_| EvalError::State("runtime spec"))?;
    let tx = u32::try_from(int(r, 1)?).map_err(|_| EvalError::State("runtime tx"))?;
    Ok(Some((
        spec,
        tx,
        hex::decode(text(r, 2)?).map_err(|_| EvalError::State("runtime metadata"))?,
    )))
}

/// The upgrade marker the stored runtime was read with.
fn stored_marker(sql: &impl Sql) -> R<Option<String>> {
    Ok(sql
        .exec("SELECT upgrade FROM chain_runtime WHERE id = 1", &[])?
        .first()
        .and_then(|r| opt_text(r, 0)))
}

/// `System.LastRuntimeUpgrade`'s key: a plain storage value, whose key is
/// `twox128(pallet) ++ twox128(item)` in every runtime, so it is read before
/// any metadata is known. Its raw value is the reader's upgrade marker: a
/// change means another runtime wrote the state, and the metadata is read
/// again. Versions are never compared.
pub fn marker_key() -> String {
    let mut k = crate::eval::chain::twox128(b"System").to_vec();
    k.extend_from_slice(&crate::eval::chain::twox128(b"LastRuntimeUpgrade"));
    key_hex(k)
}

/// The stored metadata's length, for the parse footprint, without reading it.
pub fn runtime_len(sql: &impl Sql) -> R<Option<usize>> {
    let rows = sql.exec("SELECT length(metadata) FROM chain_runtime WHERE id = 1", &[])?;
    Ok(rows
        .first()
        .and_then(|r| opt_int(r, 0))
        .and_then(|n| usize::try_from(n / 2).ok()))
}

// --------------------------------------------------------------- keys

fn netuid_value(netuid: u16) -> Value {
    Value::Int(i128::from(netuid))
}

fn account_value(a: &AccountId) -> Value {
    Value::Bytes(a.0.to_vec())
}

fn key_hex(k: Vec<u8>) -> String {
    format!("0x{}", hex::encode(k))
}

fn chain_err(e: chain::ChainError) -> EvalError {
    let _ = e;
    EvalError::State("chain decode")
}

/// The storage prefix of the subnet's `CommitmentOf` entries.
pub fn commitments_prefix(rt: &ChainRuntime, netuid: u16) -> R<String> {
    let entry = rt
        .core()
        .storage_entry(PALLET, ITEM)
        .ok_or(EvalError::State("Commitments.CommitmentOf absent"))?;
    rt.core()
        .storage_key(entry, &[netuid_value(netuid)])
        .map(key_hex)
        .map_err(|_| EvalError::State("commitment prefix"))
}

pub fn commitment_key(rt: &ChainRuntime, netuid: u16, who: &AccountId) -> R<String> {
    rt.storage_key(PALLET, ITEM, &[netuid_value(netuid), account_value(who)])
        .map(key_hex)
        .map_err(chain_err)
}

pub fn uids_key(rt: &ChainRuntime, netuid: u16, who: &AccountId) -> R<String> {
    rt.storage_key(chain::SUBTENSOR, "Uids", &[netuid_value(netuid), account_value(who)])
        .map(key_hex)
        .map_err(chain_err)
}

/// `SubtensorModule.Owner(hotkey)`: the coldkey that owns the hotkey.
pub fn owner_key(rt: &ChainRuntime, who: &AccountId) -> R<String> {
    rt.storage_key(chain::SUBTENSOR, "Owner", &[account_value(who)])
        .map(key_hex)
        .map_err(chain_err)
}

fn decode_owner(rt: &ChainRuntime, raw: Option<&[u8]>) -> Option<AccountId> {
    let v = rt.decode_storage(chain::SUBTENSOR, "Owner", raw).ok()?;
    as_account(&v).map(AccountId).filter(|a| a.0 != [0u8; 32])
}

pub fn registration_key(rt: &ChainRuntime, netuid: u16, uid: u16) -> R<String> {
    rt.storage_key(
        chain::SUBTENSOR,
        "BlockAtRegistration",
        &[netuid_value(netuid), Value::Int(i128::from(uid))],
    )
    .map(key_hex)
    .map_err(chain_err)
}

pub fn events_key(rt: &ChainRuntime) -> R<String> {
    rt.storage_key("System", "Events", &[]).map(key_hex).map_err(chain_err)
}

pub fn upgrade_key(rt: &ChainRuntime) -> R<String> {
    rt.storage_key("System", "LastRuntimeUpgrade", &[])
        .map(key_hex)
        .map_err(chain_err)
}

/// The marker a reply carries (`""` for an absent value).
fn marker_of(pairs: &[(String, Option<Vec<u8>>)], key: &str) -> String {
    value_of(pairs, key).map(hex::encode).unwrap_or_default()
}

/// The hotkey a full `CommitmentOf` key names.
pub fn hotkey_of_key(rt: &ChainRuntime, key: &str) -> R<AccountId> {
    let entry = rt
        .core()
        .storage_entry(PALLET, ITEM)
        .ok_or(EvalError::State("Commitments.CommitmentOf absent"))?;
    let raw = hex::decode(key.trim_start_matches("0x")).map_err(|_| EvalError::State("commitment key hex"))?;
    let params = rt
        .core()
        .decode_storage_key_params(entry, &raw, 1)
        .map_err(|_| EvalError::State("commitment key"))?;
    params
        .first()
        .and_then(as_account)
        .map(AccountId)
        .ok_or(EvalError::State("commitment key account"))
}

// ------------------------------------------------------------- decoding

/// A decoded `CommitmentOf` value: the block that set it and its first
/// field's raw bytes (the 44-byte submission payload, when it is one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Committed {
    pub block: u64,
    pub payload: Vec<u8>,
}

/// Decodes a `CommitmentOf` value by name. A first field that is not a raw
/// byte variant, or no field at all (`set_commitment` accepts an empty
/// list), yields an empty payload: it opens no submission.
pub fn decode_commitment(rt: &ChainRuntime, raw: &[u8]) -> R<Committed> {
    let v = rt.decode_storage(PALLET, ITEM, Some(raw)).map_err(chain_err)?;
    let block = field(&v, "block")
        .and_then(as_u64)
        .ok_or(EvalError::State("commitment block"))?;
    let first = field(&v, "info")
        .and_then(|i| field(i, "fields"))
        .and_then(as_list)
        .and_then(|l| l.first());
    let payload = match first {
        Some(Value::Dict(entries)) if entries.len() == 1 => match entries.first() {
            Some((Value::Str(name), v)) if name.starts_with("Raw") => as_bytes(v).unwrap_or_default(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    Ok(Committed { block, payload })
}

/// Where a hotkey's commitment sits in its block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    /// The index of the last extrinsic that emitted the hotkey's
    /// `Commitments.Commitment { netuid, who }`.
    At(u32),
    /// That extrinsic also committed for another hotkey on the subnet (a
    /// batch call): two commitments at one position, which the
    /// protocol cannot order.
    Shared(u32),
    /// No such event in the block.
    Absent,
}

/// The position of `who`'s commitment on `netuid` in a block's
/// `System.Events`.
pub fn commitment_extrinsic(rt: &ChainRuntime, events: &[u8], netuid: u16, who: &AccountId) -> R<Position> {
    let events = rt.events(Some(events)).map_err(chain_err)?;
    let ours: Vec<(u32, [u8; 32])> = events
        .iter()
        .filter(|e| e.pallet == PALLET && e.name == EVENT && e.phase == "ApplyExtrinsic")
        .filter_map(|e| {
            let args = e.args();
            if args.first().and_then(|v| as_u64(v)) != Some(u64::from(netuid)) {
                return None;
            }
            Some((e.extrinsic_index?, args.get(1).and_then(|v| as_account(v))?))
        })
        .collect();
    let Some(x) = ours.iter().filter(|(_, a)| *a == who.0).map(|(x, _)| *x).next_back() else {
        return Ok(Position::Absent);
    };
    Ok(if ours.iter().any(|(y, a)| *y == x && *a != who.0) {
        Position::Shared(x)
    } else {
        Position::At(x)
    })
}

fn decode_uid(rt: &ChainRuntime, raw: Option<&[u8]>) -> R<Option<u16>> {
    let v = rt.decode_storage(chain::SUBTENSOR, "Uids", raw).map_err(chain_err)?;
    Ok(match v {
        Value::Null => None,
        other => Some(
            as_u64(&other)
                .and_then(|u| u16::try_from(u).ok())
                .ok_or(EvalError::State("uid"))?,
        ),
    })
}

fn decode_registration(rt: &ChainRuntime, raw: Option<&[u8]>) -> R<u64> {
    let v = rt
        .decode_storage(chain::SUBTENSOR, "BlockAtRegistration", raw)
        .map_err(chain_err)?;
    as_u64(&v).ok_or(EvalError::State("registration block"))
}

// ---------------------------------------------------------------- rounds

/// One RPC read the Worker performs for the reader: a JSON-RPC body, tagged
/// with what its reply feeds. `large`: the reply may be up to
/// `rpc::MAX_LARGE_RESPONSE_BYTES` (metadata, events, key pages).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Read {
    pub tag: String,
    pub id: u64,
    pub body: String,
    pub large: bool,
    /// Read from the archive endpoint when the deployment configures one
    /// (`ARCHIVE_RPC_URL`): an earlier answer for this resolution stage
    /// served no state for its block.
    pub archive: bool,
}

/// A read's reply as the Worker received it (`None`: transport failure,
/// rate limit or a refused budget reservation).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadResult {
    pub tag: String,
    pub id: u64,
    pub reply: Option<String>,
}

/// v14: the reader's round claim (`round_until_ms`: a planned round's reads
/// are in flight until its ingest or the planning invocation's end) and the
/// rounds skipped for an overlapping claim or for want of invocation time.
pub const SCHEMA_V14: &[&str] = &[
    "ALTER TABLE commit_reader ADD COLUMN round_until_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE commit_reader ADD COLUMN rounds_overlapped INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE commit_reader ADD COLUMN rounds_starved INTEGER NOT NULL DEFAULT 0",
];

/// Plans one round ([`plan`]) under the reader's round claim: while an
/// earlier round's reads are in flight (planned by an overlapping
/// invocation: each block's delivery drives the reader), nothing is planned
/// and the skip is counted, so two invocations never send the same reads.
/// A planned round holds the claim for `lease_ms`, the planning invocation's
/// remaining time; its ingest releases it ([`ingest`]).
#[allow(clippy::too_many_arguments)]
pub fn plan_claimed(
    sql: &impl Sql,
    rt: Option<&ChainRuntime>,
    netuid: u16,
    confirmed: Option<BlockRef>,
    needs: &Needs,
    per_minute: u64,
    now_ms: u64,
    lease_ms: u64,
) -> R<Round> {
    let held = sql.exec("SELECT round_until_ms FROM commit_reader WHERE id = 1", &[])?;
    if held.first().map(|r| uint(r, 0)).transpose()?.unwrap_or(0) > now_ms {
        sql.exec("UPDATE commit_reader SET rounds_overlapped = rounds_overlapped + 1 WHERE id = 1", &[])?;
        return Ok(Round::default());
    }
    let round = plan(sql, rt, netuid, confirmed, needs, per_minute, now_ms)?;
    if !round.reads.is_empty() {
        sql.exec(
            "UPDATE commit_reader SET round_until_ms = ? WHERE id = 1",
            &[bind_u64(now_ms.saturating_add(lease_ms))?],
        )?;
    }
    Ok(round)
}

/// An invocation that drives the reader had no time left for one round
/// (the per-block path runs it after signing, in the same budget).
pub fn note_starved(sql: &impl Sql) -> R<()> {
    sql.exec("UPDATE commit_reader SET rounds_starved = rounds_starved + 1 WHERE id = 1", &[])?;
    Ok(())
}

/// What the reader needs this round: independent reads, done in any order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Round {
    pub reads: Vec<Read>,
}

/// What the reader is for right now: the poll runs only while a cohort
/// admits; the snapshot is requested by a closing cohort.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Needs {
    pub poll: bool,
    /// `(cutoff_block, hotkeys)` of a closing cohort whose snapshot is due.
    pub snapshot: Option<(u64, Vec<AccountId>)>,
}

fn reader_row(sql: &impl Sql) -> R<crate::sql::Row> {
    sql.exec(
        "SELECT keys_block, keys_hash, keys, keys_next, keys_done, polled_ms, observed_upgrade FROM commit_reader WHERE id = 1",
        &[],
    )?
    .into_iter()
    .next()
    .ok_or(EvalError::State("commit_reader row"))
}

/// The snapshot table (`adm_snapshot`) is the admission module's; the
/// reader fills it.
/// A snapshot row: the hotkey, its UID at the cutoff, and whether its
/// registration block is read.
type SnapshotRow = (AccountId, Option<u16>, bool);
/// A commitment being resolved: hotkey, block, stage, block hash, UID.
type Pending = (AccountId, u64, String, Option<String>, Option<u16>, bool);

fn snapshot_stage(sql: &impl Sql, cutoff: u64) -> R<(Option<String>, Vec<SnapshotRow>)> {
    let hash = sql
        .exec(
            "SELECT block_hash FROM adm_snapshot_blocks WHERE cutoff_block = ?",
            &[bind_u64(cutoff)?],
        )?
        .first()
        .and_then(|r| opt_text(r, 0));
    let rows = sql.exec(
        "SELECT hotkey, uid, done FROM adm_snapshot WHERE cutoff_block = ? ORDER BY hotkey",
        &[bind_u64(cutoff)?],
    )?;
    let mut out = Vec::new();
    for r in &rows {
        let h = crate::eval::plan::parse_hex32(text(r, 0)?).ok_or(EvalError::State("snapshot hotkey"))?;
        out.push((
            AccountId(h),
            opt_int(r, 1).and_then(|u| u16::try_from(u).ok()),
            int(r, 2)? != 0,
        ));
    }
    Ok((hash, out))
}

/// The reader's calls left in the current one-minute window of its share.
fn reader_left(sql: &impl Sql, per_minute: u64, now_ms: u64) -> R<(u64, u64, u64)> {
    let r = sql.exec(
        "SELECT budget_window_ms, budget_used FROM commit_reader WHERE id = 1",
        &[],
    )?;
    let r = r.first().ok_or(EvalError::State("commit_reader row"))?;
    let (mut start, mut used) = (uint(r, 0)?, uint(r, 1)?);
    if now_ms.saturating_sub(start) >= crate::claims::BUDGET_WINDOW_MS || now_ms < start {
        start = now_ms;
        used = 0;
    }
    Ok((start, used, per_minute.saturating_sub(used)))
}

/// Plans one round within the reader's share of the RPC budget
/// (`per_minute` calls a minute): the runtime first when it is missing or
/// stale (three calls, alone), else the snapshot, then pending resolutions
/// oldest first, then the poll, with at most one large read. `confirmed` is
/// the RPC-confirmed watermark; `rt` the stored runtime, parsed (`None` until
/// one is stored). The calls planned are charged to the share now.
pub fn plan(
    sql: &impl Sql,
    rt: Option<&ChainRuntime>,
    netuid: u16,
    confirmed: Option<BlockRef>,
    needs: &Needs,
    per_minute: u64,
    now_ms: u64,
) -> R<Round> {
    let mut round = Round::default();
    let Some(at) = confirmed else { return Ok(round) };
    let pending: Vec<Pending> = sql
        .exec(
            "SELECT hotkey, block, stage, block_hash, uid, absent_tries FROM commit_seen
             WHERE stage IN ('hash','events','reg') AND retry_ms <= ? ORDER BY block, hotkey",
            &[bind_u64(now_ms)?],
        )?
        .iter()
        .map(|r| {
            let h = crate::eval::plan::parse_hex32(text(r, 0)?).ok_or(EvalError::State("seen hotkey"))?;
            Ok((
                AccountId(h),
                uint(r, 1)?,
                text(r, 2)?.to_owned(),
                opt_text(r, 3),
                opt_int(r, 4).and_then(|u| u16::try_from(u).ok()),
                int(r, 5)? > 0,
            ))
        })
        .collect::<R<_>>()?;
    if !needs.poll && needs.snapshot.is_none() && pending.is_empty() {
        return Ok(round);
    }
    let (start, used, left) = reader_left(sql, per_minute, now_ms)?;
    let left = usize::try_from(left).unwrap_or(usize::MAX);
    let reader = reader_row(sql)?;
    let observed = opt_text(&reader, 6);
    let refresh = sql.exec(
        "SELECT refresh_hash, refresh_block FROM commit_reader WHERE id = 1",
        &[],
    )?;
    let refresh = refresh
        .first()
        .and_then(|r| Some((opt_text(r, 0)?, opt_int(r, 1)? as u64)));
    // The runtime first: nothing decodes without it. An upgrade marker that
    // a read saw and the stored runtime's differ in needs the new metadata:
    // at the block a past read named (`refresh_hash`), else at the head.
    let stored = stored_marker(sql)?;
    let stale = rt.is_none() || refresh.is_some() || observed.is_some_and(|o| stored.as_deref() != Some(o.as_str()));
    let Some(rt) = rt.filter(|_| !stale) else {
        if left < RUNTIME_READS {
            return Ok(round);
        }
        let (number, hash) = match &refresh {
            Some((h, n)) => (*n, Id::parse(h).map_err(|_| EvalError::State("refresh hash"))?),
            None => (at.number, at.hash),
        };
        round.reads.push(Read {
            tag: format!("version@{number}"),
            id: 11,
            body: rpc::runtime_version_request(11, &hash),
            large: false,
            archive: false,
        });
        round.reads.push(Read {
            tag: format!("metadata@{number}"),
            id: 12,
            body: rpc::metadata_request(12, &hash),
            large: true,
            archive: false,
        });
        round.reads.push(Read {
            tag: format!("marker@{number}"),
            id: 13,
            body: rpc::query_storage_request(13, &[marker_key()], &hash),
            large: false,
            archive: false,
        });
        charge(sql, start, used, round.reads.len())?;
        return Ok(round);
    };
    let mut next_id = 20u64;
    let mut large_left = 1usize;
    let mut push = |round: &mut Round, tag: String, body_of: &dyn Fn(u64) -> String, large: bool| -> bool {
        if round.reads.len() >= left || (large && large_left == 0) {
            return false;
        }
        if large {
            large_left -= 1;
        }
        let id = next_id;
        next_id += 1;
        round.reads.push(Read {
            tag,
            id,
            body: body_of(id),
            large,
            archive: false,
        });
        true
    };
    // The snapshot at the cutoff (its state is the most perishable).
    if let Some((cutoff, hotkeys)) = &needs.snapshot {
        let (hash, rows) = snapshot_stage(sql, *cutoff)?;
        match hash {
            None => {
                let before = cutoff.saturating_sub(1);
                push(
                    &mut round,
                    format!("snaphash@{cutoff}"),
                    &|id| rpc::request(id, "chain_getBlockHash", json!([before])),
                    false,
                );
            }
            Some(h) => {
                let at_hash = Id::parse(&h).map_err(|_| EvalError::State("snapshot hash"))?;
                let have: Vec<&AccountId> = rows.iter().map(|r| &r.0).collect();
                let missing: Vec<&AccountId> = hotkeys.iter().filter(|h| !have.contains(h)).collect();
                if !missing.is_empty() {
                    let mut keys = Vec::new();
                    for who in &missing {
                        keys.push(commitment_key(rt, netuid, who)?);
                        keys.push(uids_key(rt, netuid, who)?);
                        keys.push(owner_key(rt, who)?);
                    }
                    keys.push(marker_key());
                    push(
                        &mut round,
                        format!("snapvals@{cutoff}"),
                        &|id| rpc::query_storage_request(id, &keys, &at_hash),
                        true,
                    );
                } else {
                    let regs: Vec<u16> = rows.iter().filter(|r| !r.2).filter_map(|r| r.1).collect();
                    if !regs.is_empty() {
                        let mut keys = regs
                            .iter()
                            .map(|u| registration_key(rt, netuid, *u))
                            .collect::<R<Vec<_>>>()?;
                        keys.push(marker_key());
                        push(
                            &mut round,
                            format!("snapregs@{cutoff}"),
                            &|id| rpc::query_storage_request(id, &keys, &at_hash),
                            false,
                        );
                    }
                }
            }
        }
    }
    // Resolution of new commitments, oldest first.
    for (who, block, stage, hash, uid, absent) in &pending {
        let who_hex = hex::encode(who.0);
        let before = round.reads.len();
        match (stage.as_str(), hash) {
            ("hash", _) => {
                push(
                    &mut round,
                    format!("hash@{who_hex}@{block}"),
                    &|id| rpc::request(id, "chain_getBlockHash", json!([block])),
                    false,
                );
            }
            ("events", Some(h)) => {
                let keys = vec![events_key(rt)?, uids_key(rt, netuid, who)?, marker_key()];
                let at_hash = Id::parse(h).map_err(|_| EvalError::State("seen hash"))?;
                push(
                    &mut round,
                    format!("events@{who_hex}@{block}"),
                    &|id| rpc::query_storage_request(id, &keys, &at_hash),
                    true,
                );
            }
            ("reg", Some(h)) => {
                let Some(uid) = uid else { continue };
                let keys = vec![registration_key(rt, netuid, *uid)?, marker_key()];
                let at_hash = Id::parse(h).map_err(|_| EvalError::State("seen hash"))?;
                push(
                    &mut round,
                    format!("reg@{who_hex}@{block}"),
                    &|id| rpc::query_storage_request(id, &keys, &at_hash),
                    false,
                );
            }
            _ => {}
        }
        if *absent && round.reads.len() > before && let Some(r) = round.reads.last_mut() {
            r.archive = true;
        }
    }
    // The poll.
    let polled = uint(&reader, 5)?;
    if needs.poll && now_ms.saturating_sub(polled) >= POLL_EVERY_MS {
        let keys_block = opt_int(&reader, 0).map(|v| v as u64);
        let keys_done = int(&reader, 4)? != 0;
        let prefix = commitments_prefix(rt, netuid)?;
        match keys_block {
            // A listing in progress continues at its own block (a node that
            // no longer keeps that block's state restarts it, `ingest_keys`).
            Some(b) if !keys_done => {
                let h = opt_text(&reader, 1).ok_or(EvalError::State("keys hash"))?;
                let start = opt_text(&reader, 3);
                push(
                    &mut round,
                    format!("keys@{b}"),
                    &|id| keys_paged_request(id, &prefix, start.as_deref(), &h),
                    true,
                );
            }
            Some(b) if keys_done => {
                let h = opt_text(&reader, 1).ok_or(EvalError::State("keys hash"))?;
                let at_hash = Id::parse(&h).map_err(|_| EvalError::State("keys hash"))?;
                let listed: Vec<String> =
                    serde_json::from_str(text(&reader, 2)?).map_err(|_| EvalError::State("keys"))?;
                let mut keys = listed;
                keys.push(upgrade_key(rt)?);
                push(
                    &mut round,
                    format!("values@{b}"),
                    &|id| rpc::query_storage_request(id, &keys, &at_hash),
                    true,
                );
            }
            _ => {
                let h = at.hash.to_hex();
                if push(
                    &mut round,
                    format!("keys@{}", at.number),
                    &|id| keys_paged_request(id, &prefix, None, &h),
                    true,
                ) {
                    sql.exec(
                        "UPDATE commit_reader SET keys_block = ?, keys_hash = ?, keys = '[]', keys_next = NULL, keys_done = 0 WHERE id = 1",
                        &[bind_u64(at.number)?, s(at.hash.to_hex())],
                    )?;
                }
            }
        }
    }
    charge(sql, start, used, round.reads.len())?;
    Ok(round)
}

/// Charges `n` planned calls to the reader's window.
fn charge(sql: &impl Sql, start: u64, used: u64, n: usize) -> R<()> {
    sql.exec(
        "UPDATE commit_reader SET budget_window_ms = ?, budget_used = ? WHERE id = 1",
        &[bind_u64(start)?, bind_u64(used.saturating_add(n as u64))?],
    )?;
    Ok(())
}

/// `state_getKeysPaged(prefix, KEYS_PAGE, start, at)`.
pub fn keys_paged_request(id: u64, prefix: &str, start: Option<&str>, at: &str) -> String {
    rpc::request(id, "state_getKeysPaged", json!([prefix, KEYS_PAGE, start, at]))
}

fn reply_result(r: &ReadResult) -> Option<Json> {
    let body = r.reply.as_deref()?;
    let v: Json = serde_json::from_str(body).ok()?;
    if v.get("jsonrpc").and_then(Json::as_str) != Some("2.0")
        || v.get("id").and_then(Json::as_u64) != Some(r.id)
        || v.get("error").is_some()
    {
        return None;
    }
    v.get("result").cloned()
}

/// The largest reply [`discarded`] parses: a JSON-RPC error object is small,
/// and a large successful reply is never one.
const ERROR_REPLY_BYTES: usize = 64 * 1024;

/// Whether a reply is a node's refusal of state it no longer keeps (a
/// pruning node's "State already discarded" or unknown-block error).
pub fn discarded(r: &ReadResult) -> bool {
    let Some(b) = r.reply.as_deref() else { return false };
    if b.len() > ERROR_REPLY_BYTES || !b.contains("\"error\"") {
        return false;
    }
    let Ok(v) = serde_json::from_str::<Json>(b) else {
        return false;
    };
    let Some(e) = v.get("error") else { return false };
    let text = |k: &str| match e.get(k) {
        Some(Json::String(t)) => t.to_ascii_lowercase(),
        Some(other) => other.to_string().to_ascii_lowercase(),
        None => String::new(),
    };
    let (m, d) = (text("message"), text("data"));
    [m, d]
        .iter()
        .any(|t| t.contains("discarded") || t.contains("unknownblock") || t.contains("unknown block"))
}

fn query_values(r: &ReadResult, keys: &[String], at: &Id) -> Option<Vec<(String, Option<Vec<u8>>)>> {
    let body = r.reply.as_deref()?;
    let pairs = rpc::parse_query_storage(r.id, body.as_bytes(), keys, at).ok()?;
    pairs
        .into_iter()
        .map(|(k, v)| {
            Some((
                k,
                match v {
                    Some(h) => Some(hex::decode(h.trim_start_matches("0x")).ok()?),
                    None => None,
                },
            ))
        })
        .collect()
}

fn value_of<'a>(pairs: &'a [(String, Option<Vec<u8>>)], key: &str) -> Option<&'a [u8]> {
    pairs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .and_then(|(_, v)| v.as_deref())
}

fn note_error(sql: &impl Sql, code: &str) -> R<()> {
    sql.exec("UPDATE commit_reader SET last_error = ? WHERE id = 1", &[s(code)])?;
    Ok(())
}

/// A runtime the reader read: `(spec, tx, prefixed metadata, at_block,
/// upgrade marker)`.
pub type NewRuntime = (u32, u32, Vec<u8>, u64, String);

/// A round's outcome.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Ingested {
    pub applied: u32,
    pub failed: u32,
    /// New metadata to store.
    #[serde(skip)]
    pub runtime: Option<NewRuntime>,
}

/// Applies a round's replies. A missing or malformed reply leaves its stage
/// for the next round; a value that does not decode, or state the node no
/// longer keeps, ends only its own item. Returns the runtime to store when
/// the metadata read completed (the caller stores it, so a parse happens
/// once).
pub fn ingest(
    sql: &impl Sql,
    rt: Option<&ChainRuntime>,
    netuid: u16,
    results: &[ReadResult],
    now_ms: u64,
) -> R<Ingested> {
    let mut out = Ingested::default();
    // The round's reads are back (or were never sent): its claim ends.
    sql.exec(
        "UPDATE commit_reader SET reads = reads + ?, round_until_ms = 0 WHERE id = 1",
        &[i(results.len() as i64)],
    )?;
    let version = results.iter().find(|r| r.tag.starts_with("version@"));
    let metadata = results.iter().find(|r| r.tag.starts_with("metadata@"));
    let marker = results.iter().find(|r| r.tag.starts_with("marker@"));
    if let (Some(v), Some(m), Some(k)) = (version, metadata, marker) {
        let at = m.tag.trim_start_matches("metadata@").parse::<u64>().unwrap_or(0);
        let mk = marker_key();
        let marker = k.reply.as_deref().and_then(|b| {
            let v: Json = serde_json::from_str(b).ok()?;
            let at = Id::parse(v.get("result")?.get(0)?.get("block")?.as_str()?).ok()?;
            query_values(k, std::slice::from_ref(&mk), &at).map(|pairs| marker_of(&pairs, &mk))
        });
        let ver = v
            .reply
            .as_deref()
            .and_then(|b| rpc::parse_runtime_version(v.id, b.as_bytes()).ok());
        let meta = m
            .reply
            .as_deref()
            .and_then(|b| rpc::parse_large_hex(m.id, b.as_bytes()).ok())
            .and_then(|h| hex::decode(h.trim_start_matches("0x")).ok())
            .and_then(|scale| chain::opaque_metadata(&scale).ok());
        match (ver, meta, marker) {
            (Some((spec, tx)), Some(prefixed), Some(marker)) => {
                out.runtime = Some((spec, tx, prefixed, at, marker));
                out.applied += 3;
            }
            _ => {
                if [v, m, k].iter().any(|r| discarded(r)) {
                    // The block a past read named is gone (from the archive
                    // endpoint too, when one is configured): read the head's.
                    // Its pending resolutions can never be decoded with
                    // their own runtime, so they end as `state_discarded`
                    // (never decoded with another); a snapshot at that block
                    // pauses its cohort until a read answers.
                    let refresh = sql.exec("SELECT refresh_block FROM commit_reader WHERE id = 1", &[])?;
                    let refresh = refresh.first().and_then(|r| opt_int(r, 0)).and_then(|b| u64::try_from(b).ok());
                    sql.exec(
                        "UPDATE commit_reader SET refresh_hash = NULL, refresh_block = NULL WHERE id = 1",
                        &[],
                    )?;
                    if let Some(b) = refresh {
                        let gone = sql.exec(
                            "SELECT hotkey FROM commit_seen WHERE block = ? AND stage IN ('events','reg')",
                            &[bind_u64(b)?],
                        )?;
                        for r in &gone {
                            if let Some(h) = crate::eval::plan::parse_hex32(text(r, 0)?) {
                                unresolvable(sql, &AccountId(h), b, "state_discarded")?;
                            }
                        }
                        snapshot_discarded(sql, b.saturating_add(1), now_ms)?;
                    }
                }
                note_error(sql, "runtime_read")?;
                out.failed += 3;
            }
        }
        return Ok(out);
    }
    let Some(rt) = rt else { return Ok(out) };
    for r in results {
        let (kind, rest) = r.tag.split_once('@').unwrap_or((r.tag.as_str(), ""));
        let ok = match kind {
            "keys" => ingest_keys(sql, r)?,
            "values" => ingest_values(sql, rt, r, now_ms)?,
            "hash" => ingest_hash(sql, r, rest)?,
            "events" => ingest_events(sql, rt, netuid, r, rest, now_ms)?,
            "reg" => ingest_reg(sql, rt, netuid, r, rest, now_ms)?,
            "snaphash" => ingest_snap_hash(sql, r, rest)?,
            "snapvals" => ingest_snap_values(sql, rt, netuid, r, rest, now_ms)?,
            "snapregs" => ingest_snap_regs(sql, rt, netuid, r, rest, now_ms)?,
            _ => false,
        };
        if ok {
            out.applied += 1;
        } else {
            out.failed += 1;
            note_error(sql, kind)?;
        }
    }
    Ok(out)
}

/// Restarts the key listing at the next poll (its block's state is gone).
fn restart_listing(sql: &impl Sql) -> R<()> {
    sql.exec(
        "UPDATE commit_reader SET keys_block = NULL, keys_hash = NULL, keys = '[]', keys_next = NULL, keys_done = 0,
            discarded = discarded + 1 WHERE id = 1",
        &[],
    )?;
    Ok(())
}

fn ingest_keys(sql: &impl Sql, r: &ReadResult) -> R<bool> {
    if discarded(r) {
        restart_listing(sql)?;
        return Ok(true);
    }
    let Some(Json::Array(items)) = reply_result(r) else {
        return Ok(false);
    };
    let page: Vec<String> = items
        .iter()
        .filter_map(|v| v.as_str().map(str::to_ascii_lowercase))
        .collect();
    if page.len() != items.len() {
        return Ok(false);
    }
    let reader = reader_row(sql)?;
    let mut keys: Vec<String> = serde_json::from_str(text(&reader, 2)?).map_err(|_| EvalError::State("keys"))?;
    let done = page.len() < KEYS_PAGE as usize;
    let next = page.last().cloned();
    keys.extend(page);
    sql.exec(
        "UPDATE commit_reader SET keys = ?, keys_next = ?, keys_done = ? WHERE id = 1",
        &[
            s(serde_json::to_string(&keys).map_err(|_| EvalError::State("keys"))?),
            next.map_or(SqlValue::Null, s),
            i(i64::from(done)),
        ],
    )?;
    Ok(true)
}

fn undecodable(sql: &impl Sql) -> R<()> {
    sql.exec(
        "UPDATE commit_reader SET undecodable = undecodable + 1 WHERE id = 1",
        &[],
    )?;
    Ok(())
}

fn ingest_values(sql: &impl Sql, rt: &ChainRuntime, r: &ReadResult, now_ms: u64) -> R<bool> {
    if discarded(r) {
        restart_listing(sql)?;
        return Ok(true);
    }
    let reader = reader_row(sql)?;
    let (Some(block), Some(h)) = (opt_int(&reader, 0), opt_text(&reader, 1)) else {
        return Ok(false);
    };
    let at = Id::parse(&h).map_err(|_| EvalError::State("keys hash"))?;
    let mut keys: Vec<String> = serde_json::from_str(text(&reader, 2)?).map_err(|_| EvalError::State("keys"))?;
    let upgrade = upgrade_key(rt)?;
    keys.push(upgrade.clone());
    let Some(pairs) = query_values(r, &keys, &at) else {
        return Ok(false);
    };
    let marker = marker_of(&pairs, &upgrade);
    sql.exec(
        "UPDATE commit_reader SET observed_upgrade = ? WHERE id = 1",
        &[s(&marker)],
    )?;
    if stored_marker(sql)?.as_deref() != Some(marker.as_str()) {
        // Another runtime wrote this state: read its metadata before decoding.
        return Ok(true);
    }
    for k in keys.iter().filter(|k| **k != upgrade) {
        let Some(raw) = value_of(&pairs, k) else { continue };
        // A key or value that does not decode is that hotkey's alone: it is
        // recorded once as unresolvable and the round goes on.
        let Ok(who) = hotkey_of_key(rt, k) else {
            undecodable(sql)?;
            continue;
        };
        let (c, stage, note) = match decode_commitment(rt, raw) {
            Ok(c) => (c, None, None),
            Err(_) => (
                Committed {
                    block: 0,
                    payload: Vec::new(),
                },
                Some("unresolvable"),
                Some("undecodable"),
            ),
        };
        let seen = sql.exec("SELECT block, payload FROM commit_seen WHERE hotkey = ?", &[hk(&who)])?;
        let same = seen.first().is_some_and(|row| {
            uint(row, 0).ok() == Some(c.block) && text(row, 1).ok() == Some(hex::encode(&c.payload).as_str())
        });
        if same {
            continue;
        }
        if note.is_some() {
            undecodable(sql)?;
        }
        let logged = stage.is_none() && at_block(sql, &who, c.block)?.is_some_and(|e| e.payload == c.payload);
        let stage = stage.unwrap_or(if logged { "logged" } else { "hash" });
        sql.exec(
            "INSERT INTO commit_seen (hotkey, block, payload, seen_block, stage, note) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (hotkey) DO UPDATE SET block = excluded.block, payload = excluded.payload,
                seen_block = excluded.seen_block, stage = excluded.stage, block_hash = NULL, extrinsic = NULL, uid = NULL,
                note = excluded.note, absent_tries = 0, retry_ms = 0",
            &[hk(&who), bind_u64(c.block)?, s(hex::encode(&c.payload)), i(block), s(stage), note.map_or(SqlValue::Null, s)],
        )?;
    }
    sql.exec(
        "UPDATE commit_reader SET polled_ms = ?, polled_block = ?, polls = polls + 1, keys_block = NULL, keys_hash = NULL,
            keys = '[]', keys_next = NULL, keys_done = 0, last_error = NULL WHERE id = 1",
        &[bind_u64(now_ms)?, i(block)],
    )?;
    Ok(true)
}

fn seen_target(rest: &str) -> Option<(AccountId, u64)> {
    let (h, b) = rest.split_once('@')?;
    Some((AccountId(crate::eval::plan::parse_hex32(h)?), b.parse().ok()?))
}

/// The resumable cohort fault of an admitting or closing cohort whose
/// verifying session waits on a commitment whose block's state no endpoint
/// has served yet ([`state_absent`]).
pub const STATE_UNAVAILABLE: &str = "commitment_state_unavailable";

/// A resolution read (events or registration) whose answer served no state
/// for its block (no `LastRuntimeUpgrade` marker): a lagging or
/// load-balanced node's empty answer is never final, as the snapshot's
/// absent state pauses only until a read answers. The stage stays pending
/// and is read again after a backoff that grows with these answers
/// (`absent_tries`, `retry_ms`; every read is still charged to the reader's
/// per-minute share), from the archive endpoint when the deployment
/// configures one ([`Read::archive`]). Only an explicit discarded-state
/// error (from the archive endpoint too, when configured) ends it, as
/// `state_discarded`.
///
/// When the hotkey has a session verifying in the admitting or closing
/// cohort (its framing waits for the resolution, so the freeze does), the
/// cohort faults at every multiple of the plan's pause bound
/// ([`STATE_UNAVAILABLE`], resumable): a visible pause of the same cohort,
/// which the operator clears once an endpoint serves the state (for example
/// an archive endpoint configured) or after refusing the session.
fn state_absent(sql: &impl Sql, who: &AccountId, block: u64, now_ms: u64) -> R<()> {
    let rows = sql.exec(
        "UPDATE commit_seen SET absent_tries = absent_tries + 1 WHERE hotkey = ? AND block = ? AND stage IN ('events','reg')
         RETURNING absent_tries",
        &[hk(who), bind_u64(block)?],
    )?;
    let Some(r) = rows.first() else { return Ok(()) };
    let tries = u32::try_from(int(r, 0)?).unwrap_or(u32::MAX);
    sql.exec(
        "UPDATE commit_seen SET retry_ms = ? WHERE hotkey = ? AND block = ?",
        &[bind_u64(now_ms.saturating_add(crate::eval::admission::backoff_ms(tries)))?, hk(who), bind_u64(block)?],
    )?;
    crate::eval::admission::waiting_on_state(sql, who, tries, now_ms)
}

fn unresolvable(sql: &impl Sql, who: &AccountId, block: u64, note: &str) -> R<()> {
    sql.exec(
        "UPDATE commit_seen SET stage = 'unresolvable', note = ? WHERE hotkey = ? AND block = ? AND stage IN ('hash','events','reg')",
        &[s(note), hk(who), bind_u64(block)?],
    )?;
    if note == "state_discarded" {
        sql.exec("UPDATE commit_reader SET discarded = discarded + 1 WHERE id = 1", &[])?;
    } else if note == "undecodable" {
        undecodable(sql)?;
    }
    Ok(())
}

/// What a past read's `LastRuntimeUpgrade` says about the runtime that
/// wrote its state.
#[derive(Debug, PartialEq, Eq)]
enum Writer {
    /// The stored runtime's marker.
    Stored,
    /// Another runtime's marker: read that block's metadata first.
    Other(String),
    /// No marker at all: the node served no state for the block (a live
    /// chain always holds the value), never a runtime change.
    Absent,
}

fn other_runtime(sql: &impl Sql, pairs: &[(String, Option<Vec<u8>>)]) -> R<Writer> {
    let mk = marker_key();
    if value_of(pairs, &mk).is_none() {
        return Ok(Writer::Absent);
    }
    let seen = marker_of(pairs, &mk);
    Ok(if stored_marker(sql)?.as_deref() == Some(seen.as_str()) { Writer::Stored } else { Writer::Other(seen) })
}

fn ingest_hash(sql: &impl Sql, r: &ReadResult, rest: &str) -> R<bool> {
    let Some((who, block)) = seen_target(rest) else {
        return Ok(false);
    };
    if discarded(r) {
        unresolvable(sql, &who, block, "block_unknown")?;
        return Ok(true);
    }
    let Some(result) = reply_result(r) else {
        return Ok(false);
    };
    if result.is_null() {
        // The node knows no block at that height.
        unresolvable(sql, &who, block, "block_unknown")?;
        return Ok(true);
    }
    let Some(hash) = result.as_str().and_then(|s| Id::parse(s).ok()) else {
        return Ok(false);
    };
    sql.exec(
        "UPDATE commit_seen SET block_hash = ?, stage = 'events' WHERE hotkey = ? AND block = ? AND stage = 'hash'",
        &[s(hash.to_hex()), hk(&who), bind_u64(block)?],
    )?;
    Ok(true)
}

fn ingest_events(sql: &impl Sql, rt: &ChainRuntime, netuid: u16, r: &ReadResult, rest: &str, now_ms: u64) -> R<bool> {
    let Some((who, block)) = seen_target(rest) else {
        return Ok(false);
    };
    let rows = sql.exec(
        "SELECT block_hash, payload FROM commit_seen WHERE hotkey = ? AND block = ? AND stage = 'events'",
        &[hk(&who), bind_u64(block)?],
    )?;
    let Some(row) = rows.first() else { return Ok(true) };
    let Some(h) = opt_text(row, 0) else { return Ok(false) };
    if discarded(r) {
        // The node no longer holds the block's state: never substituted.
        unresolvable(sql, &who, block, "state_discarded")?;
        return Ok(true);
    }
    let at = Id::parse(&h).map_err(|_| EvalError::State("seen hash"))?;
    let (ek, uk) = (events_key(rt)?, uids_key(rt, netuid, &who)?);
    let keys = vec![ek.clone(), uk.clone(), marker_key()];
    let Some(pairs) = query_values(r, &keys, &at) else {
        return Ok(false);
    };
    match other_runtime(sql, &pairs)? {
        Writer::Stored => {}
        // Another runtime wrote this block's state: read the metadata at
        // this block, then the stage is read again (never terminal).
        Writer::Other(marker) => {
            refresh_at(sql, &at, block, &marker)?;
            return Ok(true);
        }
        Writer::Absent => {
            state_absent(sql, &who, block, now_ms)?;
            return Ok(true);
        }
    }
    let Some(events) = value_of(&pairs, &ek) else {
        unresolvable(sql, &who, block, "state_unavailable")?;
        return Ok(true);
    };
    let extrinsic = match commitment_extrinsic(rt, events, netuid, &who) {
        Ok(Position::At(x)) => x,
        Ok(Position::Shared(_)) => {
            unresolvable(sql, &who, block, SHARED_EXTRINSIC)?;
            return Ok(true);
        }
        Ok(Position::Absent) => {
            unresolvable(sql, &who, block, "no_commitment_event")?;
            return Ok(true);
        }
        Err(_) => {
            unresolvable(sql, &who, block, "undecodable")?;
            return Ok(true);
        }
    };
    let Ok(uid) = decode_uid(rt, value_of(&pairs, &uk)) else {
        unresolvable(sql, &who, block, "undecodable")?;
        return Ok(true);
    };
    if uid.is_none() {
        let payload = hex::decode(text(row, 1)?).map_err(|_| EvalError::State("seen payload"))?;
        record(
            sql,
            &LogEntry {
                hotkey: who,
                block,
                block_hash: h.clone(),
                extrinsic,
                payload,
                uid: None,
                registration_block: None,
            },
            now_ms,
        )?;
        sql.exec(
            "UPDATE commit_seen SET stage = 'logged', extrinsic = ? WHERE hotkey = ? AND block = ?",
            &[i(i64::from(extrinsic)), hk(&who), bind_u64(block)?],
        )?;
        return Ok(true);
    }
    sql.exec(
        "UPDATE commit_seen SET stage = 'reg', extrinsic = ?, uid = ?, note = NULL, absent_tries = 0, retry_ms = 0
         WHERE hotkey = ? AND block = ?",
        &[
            i(i64::from(extrinsic)),
            uid.map_or(SqlValue::Null, |u| i(i64::from(u))),
            hk(&who),
            bind_u64(block)?,
        ],
    )?;
    Ok(true)
}

fn ingest_reg(sql: &impl Sql, rt: &ChainRuntime, netuid: u16, r: &ReadResult, rest: &str, now_ms: u64) -> R<bool> {
    let Some((who, block)) = seen_target(rest) else {
        return Ok(false);
    };
    let rows = sql.exec(
        "SELECT block_hash, payload, extrinsic, uid FROM commit_seen WHERE hotkey = ? AND block = ? AND stage = 'reg'",
        &[hk(&who), bind_u64(block)?],
    )?;
    let Some(row) = rows.first() else { return Ok(true) };
    let (Some(h), Some(extrinsic), Some(uid)) = (opt_text(row, 0), opt_int(row, 2), opt_int(row, 3)) else {
        return Ok(false);
    };
    if discarded(r) {
        unresolvable(sql, &who, block, "state_discarded")?;
        return Ok(true);
    }
    let uid = u16::try_from(uid).map_err(|_| EvalError::State("seen uid"))?;
    let at = Id::parse(&h).map_err(|_| EvalError::State("seen hash"))?;
    let key = registration_key(rt, netuid, uid)?;
    let Some(pairs) = query_values(r, &[key.clone(), marker_key()], &at) else {
        return Ok(false);
    };
    match other_runtime(sql, &pairs)? {
        Writer::Stored => {}
        Writer::Other(marker) => {
            refresh_at(sql, &at, block, &marker)?;
            return Ok(true);
        }
        Writer::Absent => {
            state_absent(sql, &who, block, now_ms)?;
            return Ok(true);
        }
    }
    let Ok(registration) = decode_registration(rt, value_of(&pairs, &key)) else {
        unresolvable(sql, &who, block, "undecodable")?;
        return Ok(true);
    };
    let payload = hex::decode(text(row, 1)?).map_err(|_| EvalError::State("seen payload"))?;
    let extrinsic = u32::try_from(extrinsic).map_err(|_| EvalError::State("seen extrinsic"))?;
    record(
        sql,
        &LogEntry {
            hotkey: who,
            block,
            block_hash: h,
            extrinsic,
            payload,
            uid: Some(uid),
            registration_block: Some(registration),
        },
        now_ms,
    )?;
    sql.exec(
        "UPDATE commit_seen SET stage = 'logged' WHERE hotkey = ? AND block = ?",
        &[hk(&who), bind_u64(block)?],
    )?;
    Ok(true)
}

fn ingest_snap_hash(sql: &impl Sql, r: &ReadResult, rest: &str) -> R<bool> {
    let Ok(cutoff) = rest.parse::<u64>() else {
        return Ok(false);
    };
    let Some(hash) = reply_result(r).and_then(|v| v.as_str().and_then(|s| Id::parse(s).ok())) else {
        return Ok(false);
    };
    sql.exec(
        "INSERT OR IGNORE INTO adm_snapshot_blocks (cutoff_block, block_hash) VALUES (?, ?)",
        &[bind_u64(cutoff)?, s(hash.to_hex())],
    )?;
    Ok(true)
}

fn snapshot_hash(sql: &impl Sql, cutoff: u64) -> R<Option<Id>> {
    Ok(sql
        .exec(
            "SELECT block_hash FROM adm_snapshot_blocks WHERE cutoff_block = ?",
            &[bind_u64(cutoff)?],
        )?
        .first()
        .and_then(|r| opt_text(r, 0))
        .and_then(|h| Id::parse(&h).ok()))
}

/// The snapshot block's state is gone from every endpoint read (the RPC
/// endpoint and, when configured, the archive endpoint): the closing cohort
/// pauses (`snapshot_state_discarded`, resumable) and the snapshot keeps
/// being read; the first read that answers resumes the same cohort
/// ([`snapshot_answered`]). Nothing is substituted or expired.
fn snapshot_discarded(sql: &impl Sql, cutoff: u64, now_ms: u64) -> R<()> {
    sql.exec("UPDATE commit_reader SET discarded = discarded + 1 WHERE id = 1", &[])?;
    let rows = sql.exec(
        "SELECT seq FROM adm_cohorts WHERE cutoff_block = ? AND state = 'closing'",
        &[bind_u64(cutoff)?],
    )?;
    if let Some(r) = rows.first() {
        crate::eval::engine::fault(sql, int(r, 0)?, SNAPSHOT_DISCARDED, now_ms)?;
    }
    Ok(())
}

/// A snapshot read answered with the state (after an earlier read found it
/// discarded, for example once an archive endpoint is configured): the
/// closing cohort's `snapshot_state_discarded` pause ends by itself, and the
/// same cohort freezes. Nothing is skipped or substituted.
fn snapshot_answered(sql: &impl Sql, cutoff: u64, now_ms: u64) -> R<()> {
    let rows = sql.exec(
        "SELECT c.label FROM adm_cohorts a JOIN cohorts c ON c.seq = a.seq
         WHERE a.cutoff_block = ? AND a.state = 'closing' AND c.fault = ?",
        &[bind_u64(cutoff)?, s(SNAPSHOT_DISCARDED)],
    )?;
    if let Some(r) = rows.first() {
        crate::eval::engine::clear_fault(sql, text(r, 0)?, now_ms)?;
    }
    Ok(())
}

/// A past read (a resolution stage or the snapshot) written by another
/// runtime than the stored one: the metadata is read at that block before
/// anything is decoded, and the read is planned again.
fn refresh_at(sql: &impl Sql, at: &Id, block: u64, marker: &str) -> R<()> {
    sql.exec(
        "UPDATE commit_reader SET refresh_hash = ?, refresh_block = ?, observed_upgrade = ? WHERE id = 1",
        &[s(at.to_hex()), bind_u64(block)?, s(marker)],
    )?;
    Ok(())
}

fn ingest_snap_values(
    sql: &impl Sql,
    rt: &ChainRuntime,
    netuid: u16,
    r: &ReadResult,
    rest: &str,
    now_ms: u64,
) -> R<bool> {
    let Ok(cutoff) = rest.parse::<u64>() else {
        return Ok(false);
    };
    let Some(at) = snapshot_hash(sql, cutoff)? else {
        return Ok(false);
    };
    if discarded(r) {
        snapshot_discarded(sql, cutoff, now_ms)?;
        return Ok(true);
    }
    // The request's keys, in request order: (CommitmentOf, Uids) per hotkey.
    let Some(body) = r.reply.as_deref() else {
        return Ok(false);
    };
    let Ok(v) = serde_json::from_str::<Json>(body) else {
        return Ok(false);
    };
    let Some(changes) = v
        .get("result")
        .and_then(|r| r.get(0))
        .and_then(|c| c.get("changes"))
        .and_then(Json::as_array)
    else {
        return Ok(false);
    };
    let keys: Vec<String> = changes
        .iter()
        .filter_map(|c| c.get(0).and_then(Json::as_str).map(str::to_ascii_lowercase))
        .collect();
    let Some(pairs) = query_values(r, &keys, &at) else {
        return Ok(false);
    };
    match other_runtime(sql, &pairs)? {
        Writer::Stored => {}
        Writer::Other(marker) => {
            refresh_at(sql, &at, cutoff.saturating_sub(1), &marker)?;
            return Ok(true);
        }
        // No state served for the snapshot block: pause, never decode
        // nothing as "no commitment".
        Writer::Absent => {
            snapshot_discarded(sql, cutoff, now_ms)?;
            return Ok(true);
        }
    }
    snapshot_answered(sql, cutoff, now_ms)?;
    let wanted: Vec<AccountId> = sql
        .exec(
            "SELECT hotkey FROM adm_snapshot_wanted WHERE cutoff_block = ?",
            &[bind_u64(cutoff)?],
        )?
        .iter()
        .filter_map(|r| text(r, 0).ok().and_then(crate::eval::plan::parse_hex32).map(AccountId))
        .collect();
    for who in &wanted {
        let ck = commitment_key(rt, netuid, who)?;
        let uk = uids_key(rt, netuid, who)?;
        if !keys.iter().any(|k| k.eq_ignore_ascii_case(&ck)) {
            continue;
        }
        // Under the snapshot block's own runtime, a value that does not
        // decode holds no submission (the session is refused at the freeze
        // as overwritten, or as unregistered), never a stalled freeze.
        let committed = value_of(&pairs, &ck).and_then(|raw| decode_commitment(rt, raw).ok());
        let uid = decode_uid(rt, value_of(&pairs, &uk)).unwrap_or(None);
        // The owning coldkey at the cutoff (unknown when it does not decode:
        // the freeze then counts no outside entrant for it).
        let coldkey = owner_key(rt, who).ok().and_then(|k| decode_owner(rt, value_of(&pairs, &k)));
        sql.exec(
            "INSERT OR REPLACE INTO adm_snapshot (cutoff_block, hotkey, block, payload, uid, registration_block, done, coldkey)
             VALUES (?, ?, ?, ?, ?, NULL, ?, ?)",
            &[
                bind_u64(cutoff)?,
                hk(who),
                committed.as_ref().map_or(Ok(SqlValue::Null), |c| bind_u64(c.block))?,
                committed
                    .as_ref()
                    .map_or(SqlValue::Null, |c| s(hex::encode(&c.payload))),
                uid.map_or(SqlValue::Null, |u| i(i64::from(u))),
                i(i64::from(uid.is_none())),
                coldkey.map_or(SqlValue::Null, |c| s(hex::encode(c.0))),
            ],
        )?;
    }
    Ok(true)
}

fn ingest_snap_regs(
    sql: &impl Sql,
    rt: &ChainRuntime,
    netuid: u16,
    r: &ReadResult,
    rest: &str,
    now_ms: u64,
) -> R<bool> {
    let Ok(cutoff) = rest.parse::<u64>() else {
        return Ok(false);
    };
    let Some(at) = snapshot_hash(sql, cutoff)? else {
        return Ok(false);
    };
    if discarded(r) {
        snapshot_discarded(sql, cutoff, now_ms)?;
        return Ok(true);
    }
    let rows = sql.exec(
        "SELECT hotkey, uid FROM adm_snapshot WHERE cutoff_block = ? AND done = 0 AND uid IS NOT NULL",
        &[bind_u64(cutoff)?],
    )?;
    let mut keys = Vec::new();
    let mut by_key = Vec::new();
    for row in &rows {
        let uid = u16::try_from(int(row, 1)?).map_err(|_| EvalError::State("snapshot uid"))?;
        let k = registration_key(rt, netuid, uid)?;
        keys.push(k.clone());
        by_key.push((text(row, 0)?.to_owned(), k));
    }
    keys.push(marker_key());
    let Some(pairs) = query_values(r, &keys, &at) else {
        return Ok(false);
    };
    match other_runtime(sql, &pairs)? {
        Writer::Stored => {}
        Writer::Other(marker) => {
            refresh_at(sql, &at, cutoff.saturating_sub(1), &marker)?;
            return Ok(true);
        }
        // No state served for the snapshot block: pause, never decode
        // nothing as "no commitment".
        Writer::Absent => {
            snapshot_discarded(sql, cutoff, now_ms)?;
            return Ok(true);
        }
    }
    snapshot_answered(sql, cutoff, now_ms)?;
    for (who, k) in by_key {
        // An undecodable registration block leaves it unknown: the freeze
        // refuses the session as a registration change.
        let reg = decode_registration(rt, value_of(&pairs, &k)).ok();
        sql.exec(
            "UPDATE adm_snapshot SET registration_block = ?, done = 1 WHERE cutoff_block = ? AND hotkey = ?",
            &[reg.map_or(Ok(SqlValue::Null), bind_u64)?, bind_u64(cutoff)?, s(who)],
        )?;
    }
    Ok(true)
}

/// Content-free reader status for `/eval/status`.
pub fn status(sql: &impl Sql) -> R<Json> {
    let r = reader_row(sql)?;
    let x = sql.exec(
        "SELECT polls, reads, last_error, discarded, undecodable, round_until_ms, rounds_overlapped, rounds_starved
         FROM commit_reader WHERE id = 1",
        &[],
    )?;
    let x = x.first().ok_or(EvalError::State("commit_reader row"))?;
    let stages: Vec<Json> = sql
        .exec(
            "SELECT stage, COUNT(*) FROM commit_seen GROUP BY stage ORDER BY stage",
            &[],
        )?
        .iter()
        .map(|row| Ok(json!({"stage": text(row, 0)?, "count": int(row, 1)?})))
        .collect::<R<_>>()?;
    let logged = int(
        &sql.exec("SELECT COUNT(*) FROM commit_log", &[])?
            .first()
            .cloned()
            .unwrap_or_default(),
        0,
    )
    .unwrap_or(0);
    let spec = sql.exec(
        "SELECT spec_version, at_block, upgrade FROM chain_runtime WHERE id = 1",
        &[],
    )?;
    Ok(json!({
        "polled_ms": uint(&r, 5)?, "polled_block": polled_block(sql)?, "polls": int(x, 0)?, "reads": int(x, 1)?, "last_error": opt_text(x, 2),
        "runtime_changed": opt_text(&r, 6).is_some_and(|o| spec.first().and_then(|r| opt_text(r, 2)).as_deref() != Some(o.as_str())),
        "runtime_spec": spec.first().and_then(|r| opt_int(r, 0)),
        "logged": logged, "seen": stages, "state_discarded": int(x, 3)?, "undecodable": int(x, 4)?,
        "round_until_ms": int(x, 5)?, "rounds_overlapped": int(x, 6)?, "rounds_starved": int(x, 7)?,
        // Pending resolutions whose reads have served no state yet (read
        // again after a backoff, never ended for it).
        "state_absent_pending": int(
            &sql.exec(
                "SELECT COUNT(*) FROM commit_seen WHERE stage IN ('events','reg') AND absent_tries > 0",
                &[],
            )?
            .first()
            .cloned()
            .unwrap_or_default(),
            0,
        )
        .unwrap_or(0),
    }))
}

#[cfg(test)]
#[path = "commitments_tests.rs"]
mod tests;
