//! The coordinator's own timelocked weight commits: a durable signing
//! journal in the ChainJournal Durable Object (schema v4).
//!
//! # Authority and fencing
//!
//! One durable claim, [`SIGNER_CLAIM`], serializes every signing step. A step
//! is issued as a [`SignPlan`] that carries the claim token and is recorded as
//! the signer's pending step; the Worker performs the step's one I/O call and
//! posts the outcome back. The journal accepts an outcome only while the token
//! is the current, unexpired claim and the pending step is still the one it
//! issued. A delayed invocation whose lease lapsed is fenced: its outcome,
//! its pre-submission mark and its renewals are refused, so it can never
//! regain signing authority. A new holder that finds a submission mark
//! without an outcome treats that attempt as uncertain.
//!
//! # One commit per subnet epoch
//!
//! The current calculation target is re-committed once per subnet epoch
//! (tempo), as a new operation whose identity is the protocol's weight
//! operation for the target extended by the epoch index
//! ([`commit_op_id`]). The epoch is the column `commit_ops.epoch`, which is
//! unique, so no epoch can hold two operations. Before signing, the chain's
//! own queue for that epoch is read; an existing commit by this hotkey
//! refuses the operation instead of committing twice.
//!
//! # Only RPC-confirmed state
//!
//! Every decision and every chain read is made at the RPC-confirmed
//! watermark, or at a retained finalized block below it, by block hash. The
//! provisional cursor that webhook deliveries advance is never used. The
//! nonce, the schedule, the registrations and the timestamp that sets the
//! drand round all come from one confirmed block, which also anchors the
//! transaction's mortal era.
//!
//! # Attempts and reconciliation (no blind resubmission)
//!
//! An attempt (nonce, era, signed bytes) is journaled before anything is
//! sent. A one-shot mark (`signed` → `sending`) under the current claim
//! precedes the single `author_submitExtrinsic`. Nothing is ever sent twice:
//! a timeout, a transport error, a lost acknowledgement or an eviction
//! between mark and outcome leaves the attempt uncertain, and only finalized
//! state resolves it:
//!
//! - the hotkey's finalized nonce passed the attempt's nonce: a bounded
//!   binary search over retained finalized blocks finds the inclusion block,
//!   the block body must contain exactly these bytes, and the block's events
//!   give the dispatch outcome;
//! - the confirmed block reached the era's expiry with the nonce unconsumed:
//!   the attempt can never be included, so a new attempt may be signed.
//!
//! A nonce consumed by bytes this journal never signed is a durable fault
//! that pauses signing; so is a finalized nonce below the journal's floor.
//!
//! # Reveal
//!
//! Commit-reveal v4 reveals on chain. After inclusion the journal reads the
//! commit back from `TimelockedWeightCommits` at a confirmed block. After the
//! predicted reveal block it reads the queue, `LastUpdate` and the reveal
//! block's events and `Weights`, and records the `TimelockedWeightsRevealed`
//! event and whether the revealed weights equal the row.
//!
//! # Pauses
//!
//! Signing starts paused (`operator_hold`). A disabled commit/reveal flag
//! pauses new commitments and never falls back to direct weights. Existing
//! attempts keep reconciling during every pause except a fault, and the
//! chain's own reveal is unaffected.

use crate::chain::{
    self, ChainError, ChainRuntime, Hotkey, Value, as_account, as_bool, as_bytes, as_list, as_u64,
    field,
};
use crate::claims::{self, ClaimOutcome, ClaimToken};
use crate::journal;
use crate::rpc::RpcError;
use crate::sql::{Sql, SqlValue, StoreError, bind_u64, int, text, uint};
use crate::target::{self, RowSummary};
use crate::wire::{BlockRef, Id};
use rtao_core::timelock::epoch_schedule::EpochScheduleState;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

pub const SIGNER_CLAIM: &str = "signer";
pub const COMMIT_OP_PREFIX: &str = "slop_ninja:coordinator-commit:v1:";
const COMMIT_OP_DOMAIN: &[u8] = b"slop_ninja.coordinator-commit.v1\0";
/// The timelocked payload format the coordinator encrypts: rtao's
/// `WeightsTlockPayload` (hotkey, UIDs, values, version key), taken from
/// rtao's portable `commit_reveal::NATIVE_PAYLOAD_VERSION` (D-110), the value
/// the native validators' scheduler uses. Runtime code alone decides which
/// payload a reveal decodes; the stored `CommitRevealWeightsVersion` is a tag
/// each commit repeats, so the commit follows the stored tag whatever it is,
/// every epoch ([`cr_tag_unrevealed`] reports evidence against a tag; it
/// never stops a commit). That is rtao's own decision under its default
/// `Unreviewed::Follow` (D-122): `select_tag` follows the stored tag under
/// any code, and its one constraint after a refuted reveal, at most one
/// commit per subnet epoch, is what the coordinator does every epoch. The
/// Worker reads no `:code` hash, so `select_tag` would only report
/// `CodeUnknown`; a test checks that the two decisions agree.
pub const NATIVE_CR_PAYLOAD_VERSION: u16 = rtao_core::commit_reveal::NATIVE_PAYLOAD_VERSION;
/// `GLOBAL_MAX_SUBNET_COUNT`: the mechanism stride of storage indices.
const MECHANISM_STRIDE: u16 = 4096;
/// Retained sign-log rows on `/status`.
const STATUS_LOG: u64 = 16;

/// Static signing configuration from Worker vars.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignConfig {
    pub genesis: Id,
    pub netuid: u16,
    /// The coordinator hotkey's public key. The secret must derive it.
    pub hotkey: [u8; 32],
    pub version_key: u64,
    /// Mortal era length. The attempt's fate is known at most this many
    /// blocks after the confirmed block that anchors it.
    pub era_period: u64,
    /// Blocks after an epoch starts before its commit is prepared.
    pub commit_offset: u64,
    /// Blocks kept free before the next epoch beyond the era, so an attempt
    /// signed in the window is included in the epoch it was encrypted for.
    pub boundary_guard: u64,
    /// Blocks after the predicted reveal block before the reveal is checked.
    pub reveal_slack: u64,
    pub lease_ms: u64,
    pub rpc_calls_per_minute: u64,
    /// Signing's own share of the shared per-minute RPC budget, so it can
    /// never starve the heartbeat's anchoring, audits and fallback.
    pub sign_rpc_calls_per_minute: u64,
}

impl SignConfig {
    pub fn testnet(genesis: Id, netuid: u16, hotkey: [u8; 32]) -> Self {
        Self {
            genesis,
            netuid,
            hotkey,
            version_key: 0,
            era_period: 64,
            commit_offset: 12,
            boundary_guard: 8,
            reveal_slack: 2,
            lease_ms: 25_000,
            rpc_calls_per_minute: 12,
            sign_rpc_calls_per_minute: 8,
        }
    }
}

/// The source of the hotkey secret, read only while signing.
pub trait KeySource {
    fn hotkey(&self) -> Result<Hotkey, ChainError>;
}

/// Parsed runtimes, kept in memory across requests (at most
/// [`PARSED_RUNTIMES`]) and rebuilt from durable storage after an eviction.
///
/// The *head* runtime is the one the chain reported at the newest block the
/// signer asked about (`signer.runtime_block`); keys are planned and attempts
/// signed with it. A read is decoded with the runtime that *wrote* its block,
/// found by the spec in that block's own `LastRuntimeUpgrade`.
#[derive(Default)]
pub struct RuntimeCache {
    parsed: RefCell<Vec<(RuntimeKey, Rc<ChainRuntime>)>>,
}

/// The cache key of a stored runtime: the spec and transaction versions the
/// chain reported for it. It only selects which stored metadata decodes or
/// signs; no value is ever accepted or refused.
type RuntimeKey = (u32, u32);

/// Parsed runtimes kept in memory: the head and one older writer.
pub const PARSED_RUNTIMES: usize = 2;

impl RuntimeCache {
    /// The head runtime, if its metadata is stored.
    pub fn get(&self, sql: &impl Sql) -> Result<Option<Rc<ChainRuntime>>, SignError> {
        let Some((spec, tx)) = current_versions(sql)? else {
            return Ok(None);
        };
        self.load(sql, spec, tx)
    }

    /// A stored runtime with `spec` (the most recently fetched, if several).
    fn for_spec(&self, sql: &impl Sql, spec: u32) -> Result<Option<Rc<ChainRuntime>>, SignError> {
        let hit = self
            .parsed
            .borrow()
            .iter()
            .find(|(key, _)| key.0 == spec)
            .map(|(_, rt)| rt.clone());
        if hit.is_some() {
            return Ok(hit);
        }
        let rows = sql.exec(
            "SELECT tx_version FROM runtime_cache WHERE spec_version = ? ORDER BY fetched_at_ms DESC LIMIT 1",
            &[SqlValue::Integer(spec.into())],
        )?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let tx = u32::try_from(uint(row, 0)?).map_err(|_| StoreError("tx version"))?;
        self.load(sql, spec, tx)
    }

    pub(crate) fn load(
        &self,
        sql: &impl Sql,
        spec: u32,
        tx: u32,
    ) -> Result<Option<Rc<ChainRuntime>>, SignError> {
        let key: RuntimeKey = (spec, tx);
        let hit = self
            .parsed
            .borrow()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, rt)| rt.clone());
        if hit.is_some() {
            return Ok(hit);
        }
        let versions = [SqlValue::Integer(spec.into()), SqlValue::Integer(tx.into())];
        let rows = sql.exec(
            "SELECT metadata_hex, metadata_sha256 FROM runtime_cache WHERE spec_version = ? AND tx_version = ?",
            &versions,
        )?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let bytes =
            hex::decode(text(row, 0)?).map_err(|_| SignError::Store(StoreError("metadata hex")))?;
        if chain::sha256_hex(&bytes) != text(row, 1)? {
            return Err(SignError::Store(StoreError("metadata digest")));
        }
        // The parsed runtime outlives this request: it is parsed, and kept,
        // in the isolate's runtimes home (`arena_routes::home`). The cache is
        // trimmed to make room first, so the home holds at most
        // `PARSED_RUNTIMES - 1` resident runtimes beside the one parse; an
        // entry a caller still holds would stay live past its eviction, and a
        // parse the home cannot hold (its need scales with the metadata
        // length) is parsed here, in the request's scope, and not cached.
        use crate::arena_routes::home::{self, Home};
        let room = home::enter(Home::Runtimes, || {
            let mut parsed = self.parsed.borrow_mut();
            if parsed.iter().skip(PARSED_RUNTIMES - 1).any(|(_, rt)| Rc::strong_count(rt) > 1) {
                return false;
            }
            parsed.truncate(PARSED_RUNTIMES - 1);
            true
        })
        .unwrap_or(false);
        let cacheable = room && home::free_bytes(Home::Runtimes) >= crate::arena_routes::runtime_parse_bytes(bytes.len());
        let parse = || ChainRuntime::parse(&bytes, spec, tx).map(Rc::new);
        let (parsed, cacheable) = if cacheable {
            match home::enter(Home::Runtimes, parse) {
                Ok(p) => (p, true),
                Err(parse) => (parse(), false),
            }
        } else {
            (parse(), false)
        };
        let rt = match parsed {
            Ok(rt) => rt,
            Err(e) => {
                // Stored by a build whose parser accepted it: drop it so the
                // runtime is fetched again (after the failure's backoff).
                sql.exec(
                    "DELETE FROM runtime_cache WHERE spec_version = ? AND tx_version = ?",
                    &versions,
                )?;
                return Err(SignError::Chain(e));
            }
        };
        if cacheable {
            let _ = home::enter(Home::Runtimes, || {
                let mut parsed = self.parsed.borrow_mut();
                if parsed.capacity() < PARSED_RUNTIMES {
                    let more = PARSED_RUNTIMES - parsed.len();
                    parsed.reserve_exact(more);
                }
                parsed.truncate(PARSED_RUNTIMES - 1);
                parsed.insert(0, (key, rt.clone()));
            });
        }
        Ok(Some(rt))
    }

    /// Drops the in-memory copies (tests use this to model an eviction).
    pub fn forget(&self) {
        self.parsed.borrow_mut().clear();
    }
}

#[derive(Debug)]
pub enum SignError {
    Store(StoreError),
    Chain(ChainError),
}

impl From<StoreError> for SignError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

// ------------------------------------------------------------------ plans

/// What a chain read is for. The journal recomputes the keys from its own
/// state when the values come back, so a reply cannot choose its meaning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "purpose", rename_all = "snake_case")]
pub enum ReadPurpose {
    Preflight,
    Registration {
        op_id: String,
    },
    NonceCheck {
        op_id: String,
        attempt: u32,
    },
    NonceProbe {
        op_id: String,
        attempt: u32,
        mid: u64,
    },
    Events {
        op_id: String,
        attempt: u32,
    },
    /// D5: the events of the block that consumed an attempt's nonce, read
    /// before its body: a commit event of this hotkey, netuid and reveal
    /// round there is the attempt's inclusion.
    InclusionEvents {
        op_id: String,
        attempt: u32,
    },
    Readback {
        op_id: String,
    },
    RevealCheck {
        op_id: String,
    },
    /// Is the commit still queued at `mid`? (`lo` queued, `hi` not.)
    RevealProbe {
        op_id: String,
        lo: u64,
        hi: u64,
        mid: u64,
    },
    /// The events and `Weights` of the block that removed the commit.
    RevealEvents {
        op_id: String,
        block: u64,
    },
    /// The subnet's schedule and population at a finalized block, for the
    /// row other validators copy ([`rows`]), outside the commit pipeline.
    RowSchedule,
    /// The registrations and owner hotkey at the same block, for the row of
    /// commit epoch `epoch`.
    RowRegistration {
        epoch: u64,
        population: u16,
    },
}

/// Why a runtime version (and, once, its metadata) is fetched.
///
/// `state_getRuntimeVersion` and the metadata API at block `b` run the code in
/// `b`'s post-state, which executes `b + 1`. The block that enacts a runtime
/// upgrade was itself executed, and its storage written, by the previous
/// runtime, and its `LastRuntimeUpgrade` still names that runtime until the
/// next block's initialization. So the runtime that wrote `b` is always
/// learned at `b`'s parent, never at `b`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "for", rename_all = "snake_case")]
pub enum RuntimeFor {
    /// The head runtime, at the confirmed block.
    #[default]
    Head,
    /// The runtime with `spec` that wrote `block`, fetched at its parent, for
    /// the read that needs it.
    Writer {
        block: BlockRef,
        spec: u32,
        read: ReadPurpose,
    },
    /// The runtime that executes the blocks after the preflight block, which
    /// signs the attempt for `op_id` (signing reads versions fresh).
    Sign { op_id: String },
}

/// One signing step: a single RPC call for the Worker to make.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum SignPlan {
    RuntimeVersion {
        claim: ClaimToken,
        at: BlockRef,
        #[serde(default)]
        purpose: RuntimeFor,
    },
    Metadata {
        claim: ClaimToken,
        at: BlockRef,
        spec_version: u32,
        tx_version: u32,
        #[serde(default)]
        purpose: RuntimeFor,
    },
    Read {
        claim: ClaimToken,
        at: BlockRef,
        read: ReadPurpose,
        keys: Vec<String>,
    },
    /// `chain_getBlock` at `at`, matched in the Worker against `extrinsic`
    /// (the attempt's bytes) as it streams (`rpc::BlockScan`); only the
    /// match comes back, so a block of any size is readable.
    Block {
        claim: ClaimToken,
        at: BlockRef,
        op_id: String,
        attempt: u32,
        /// The attempt's extrinsic hex. Absent from a plan an earlier build
        /// stored; such a plan's outcome is refused (`apply_block`).
        #[serde(default)]
        extrinsic: String,
    },
    /// Mark first (`/sign-mark`), then `author_submitExtrinsic` exactly once.
    Submit {
        claim: ClaimToken,
        op_id: String,
        attempt: u32,
        extrinsic: String,
        drill: Option<String>,
    },
}

impl SignPlan {
    pub fn claim(&self) -> &ClaimToken {
        match self {
            Self::RuntimeVersion { claim, .. }
            | Self::Metadata { claim, .. }
            | Self::Read { claim, .. }
            | Self::Block { claim, .. }
            | Self::Submit { claim, .. } => claim,
        }
    }
}

/// How one `author_submitExtrinsic` ended, as the Worker saw it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "submit", rename_all = "snake_case")]
pub enum SubmitOutcome {
    /// The node returned the extrinsic hash.
    Accepted { hash: String },
    /// A JSON-RPC error proving the node did not take it into its pool.
    Rejected { code: i64, message: String },
    /// Anything else: the bytes may or may not be in a pool.
    Uncertain { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SignOutcome {
    RuntimeVersion {
        result: Result<(u32, u32), RpcError>,
    },
    /// Hex of the `Option<OpaqueMetadata>` the runtime API returned.
    Metadata {
        result: Result<String, RpcError>,
    },
    /// `(key, value)` hex pairs from `state_queryStorageAt`.
    Read {
        result: Result<Vec<(String, Option<String>)>, RpcError>,
    },
    /// How many extrinsics the block holds, and the index of the plan's.
    Block {
        result: Result<crate::rpc::BlockMatch, RpcError>,
    },
    Submit {
        result: SubmitOutcome,
    },
}

/// A step that follows one outcome directly, at a block the outcome names.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Chained {
    /// The runtime reported at `at` is not stored: fetch its metadata.
    Metadata {
        at: BlockRef,
        spec: u32,
        tx: u32,
        purpose: RuntimeFor,
    },
    /// `block` was written by runtime `spec`, which is not stored: learn it
    /// at the block's parent.
    LearnWriter {
        block: BlockRef,
        spec: u32,
        read: ReadPurpose,
    },
    /// Read `read` at `at` again with the keys runtime `spec` derives.
    Read {
        at: BlockRef,
        spec: u32,
        read: ReadPurpose,
    },
    /// The version of the runtime that will execute the attempt for `op_id`.
    SignRuntime { op_id: String, at: BlockRef },
    /// The registration read a preflight asks for, at the same block.
    Registration { op_id: String, at: BlockRef },
    /// Narrow the block where the queued commit disappeared.
    RevealProbe { op_id: String, lo: u64, hi: u64 },
    /// Read the events of the block that removed it.
    RevealEvents { op_id: String, block: u64 },
    /// The row snapshot's registration read, at the block of its schedule.
    RowRegistration { at: BlockRef, epoch: u64, population: u16 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignStep {
    pub fenced: bool,
    pub plan: Option<SignPlan>,
}

// ------------------------------------------------------------ identities

/// `slop_ninja:coordinator-commit:v1:` + hex SHA-256 of the domain, the
/// protocol weight operation ID for (target, hotkey) and the big-endian
/// subnet epoch. One identity per epoch.
pub fn commit_op_id(base_op: &str, epoch: u64) -> String {
    let mut hash = Sha256::new();
    hash.update(COMMIT_OP_DOMAIN);
    hash.update((base_op.len() as u64).to_be_bytes());
    hash.update(base_op.as_bytes());
    hash.update(epoch.to_be_bytes());
    format!("{COMMIT_OP_PREFIX}{}", hex::encode(hash.finalize()))
}

fn storage_index(netuid: u16, mechanism: u8) -> Option<u16> {
    u16::from(mechanism)
        .checked_mul(MECHANISM_STRIDE)
        .and_then(|m| m.checked_add(netuid))
}

// ---------------------------------------------------------------- signer

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Signer {
    pub state: String,
    pub reason: Option<String>,
    pub nonce_floor: u64,
    pub runtime: Option<(u32, u32)>,
    /// The block the head runtime was reported at.
    pub runtime_block: Option<u64>,
    pending_plan: Option<String>,
    pending_generation: Option<u64>,
    /// No step is planned before this time (RPC-error backoff).
    pub backoff_until_ms: Option<u64>,
    /// Consecutive RPC errors, reset by any answered call.
    pub rpc_failures: u64,
    /// Runtime re-learns since the last decoded read.
    relearns: u64,
    pub last_error: Option<String>,
    pub last_error_at_ms: Option<u64>,
    budget_window_ms: u64,
    budget_used: u64,
}

fn signer(sql: &impl Sql) -> Result<Signer, StoreError> {
    let rows = sql.exec(
        "SELECT state, reason, nonce_floor, runtime_spec, runtime_tx, pending_plan, pending_generation,
                runtime_block, backoff_until_ms, rpc_failures, relearns, last_error, last_error_at_ms,
                budget_window_ms, budget_used
         FROM signer WHERE id = 1",
        &[],
    )?;
    let r = rows.first().ok_or(StoreError("signer row"))?;
    let opt_text = |i| match r.get(i) {
        Some(SqlValue::Text(t)) => Some(t.clone()),
        _ => None,
    };
    let opt_int = |i| match r.get(i) {
        Some(SqlValue::Integer(v)) => Some(*v),
        _ => None,
    };
    let opt_u = |i| opt_int(i).and_then(|v| u64::try_from(v).ok());
    Ok(Signer {
        state: text(r, 0)?.to_owned(),
        reason: opt_text(1),
        nonce_floor: uint(r, 2)?,
        runtime: match (opt_int(3), opt_int(4)) {
            (Some(s), Some(t)) => match (u32::try_from(s), u32::try_from(t)) {
                (Ok(s), Ok(t)) => Some((s, t)),
                _ => None,
            },
            _ => None,
        },
        pending_plan: opt_text(5),
        pending_generation: opt_u(6),
        runtime_block: opt_u(7),
        backoff_until_ms: opt_u(8),
        rpc_failures: uint(r, 9)?,
        relearns: uint(r, 10)?,
        last_error: opt_text(11),
        last_error_at_ms: opt_u(12),
        budget_window_ms: uint(r, 13)?,
        budget_used: uint(r, 14)?,
    })
}

fn current_versions(sql: &impl Sql) -> Result<Option<(u32, u32)>, StoreError> {
    Ok(signer(sql)?.runtime)
}

/// Sets (or clears) the head runtime, reported at `block`.
fn set_runtime(
    sql: &impl Sql,
    versions: Option<(u32, u32)>,
    block: Option<u64>,
) -> Result<(), StoreError> {
    let (s, t) = match versions {
        Some((s, t)) => (SqlValue::Integer(s.into()), SqlValue::Integer(t.into())),
        None => (SqlValue::Null, SqlValue::Null),
    };
    sql.exec(
        "UPDATE signer SET runtime_spec = ?, runtime_tx = ?, runtime_block = ? WHERE id = 1",
        &[s, t, block.map_or(Ok(SqlValue::Null), bind_u64)?],
    )?;
    Ok(())
}

/// Moves the head runtime forward to a runtime reported at `block`; a report
/// at an older block never moves it back.
fn advance_head(sql: &impl Sql, versions: (u32, u32), block: u64) -> Result<(), StoreError> {
    let s = signer(sql)?;
    if s.runtime.is_none() || s.runtime_block.is_none_or(|b| block >= b) {
        set_runtime(sql, Some(versions), Some(block))?;
    }
    Ok(())
}

fn set_pending(sql: &impl Sql, plan: Option<&SignPlan>) -> Result<(), StoreError> {
    let (json, generation) = match plan {
        Some(p) => (
            SqlValue::Text(serde_json::to_string(p).map_err(|_| StoreError("plan json"))?),
            bind_u64(p.claim().generation)?,
        ),
        None => (SqlValue::Null, SqlValue::Null),
    };
    sql.exec(
        "UPDATE signer SET pending_plan = ?, pending_generation = ? WHERE id = 1",
        &[json, generation],
    )?;
    Ok(())
}

/// Pauses new signing with a durable reason.
pub fn pause(sql: &impl Sql, reason: &str, now_ms: u64) -> Result<(), StoreError> {
    sql.exec(
        "UPDATE signer SET state = 'paused', reason = ?, updated_at_ms = ? WHERE id = 1",
        &[reason.into(), bind_u64(now_ms)?],
    )?;
    log(sql, now_ms, None, None, None, "paused", Some(reason))
}

/// Operator resume. Refused while a durable signing fault is unresolved.
pub fn resume(sql: &impl Sql, now_ms: u64) -> Result<bool, StoreError> {
    let s = signer(sql)?;
    if s.state == "active" {
        return Ok(true);
    }
    if s.reason.as_deref().is_some_and(|r| r.starts_with("fault:")) {
        return Ok(false);
    }
    sql.exec(
        "UPDATE signer SET state = 'active', reason = NULL, updated_at_ms = ? WHERE id = 1",
        &[bind_u64(now_ms)?],
    )?;
    log(sql, now_ms, None, None, None, "resumed", None)?;
    Ok(true)
}

/// Operator clear of a signing fault after review; signing stays paused.
pub fn clear_fault(sql: &impl Sql, now_ms: u64) -> Result<bool, StoreError> {
    let s = signer(sql)?;
    if !s.reason.as_deref().is_some_and(|r| r.starts_with("fault:")) {
        return Ok(false);
    }
    sql.exec(
        "UPDATE signer SET reason = 'operator_hold', updated_at_ms = ? WHERE id = 1",
        &[bind_u64(now_ms)?],
    )?;
    // An attempt that settled into a fault is retried from scratch.
    sql.exec(
        "DELETE FROM sign_failures WHERE subject LIKE 'attempt:%'",
        &[],
    )?;
    log(
        sql,
        now_ms,
        None,
        None,
        None,
        "fault_cleared",
        s.reason.as_deref(),
    )?;
    Ok(true)
}

fn fault(
    sql: &impl Sql,
    code: &str,
    op_id: Option<&str>,
    attempt: Option<u32>,
    now_ms: u64,
) -> Result<(), StoreError> {
    let reason = format!("fault:{code}");
    sql.exec(
        "UPDATE signer SET state = 'paused', reason = ?, updated_at_ms = ? WHERE id = 1",
        &[reason.as_str().into(), bind_u64(now_ms)?],
    )?;
    journal::bump(sql, "signing_faults")?;
    log(sql, now_ms, None, op_id, attempt, "fault", Some(code))
}

fn log(
    sql: &impl Sql,
    now_ms: u64,
    block: Option<u64>,
    op_id: Option<&str>,
    attempt: Option<u32>,
    event: &str,
    detail: Option<&str>,
) -> Result<(), StoreError> {
    sql.exec(
        "INSERT INTO sign_log (at_ms, block_number, op_id, attempt, event, detail) VALUES (?, ?, ?, ?, ?, ?)",
        &[
            bind_u64(now_ms)?,
            block.map_or(Ok(SqlValue::Null), bind_u64)?,
            op_id.map_or(SqlValue::Null, |s| s.into()),
            attempt.map_or(SqlValue::Null, |a| SqlValue::Integer(a.into())),
            event.into(),
            detail.map_or(SqlValue::Null, |s| s.into()),
        ],
    )?;
    Ok(())
}

// --------------------------------------------------------------- failures
//
// A step can fail in two ways. An RPC error (transport, timeout, HTTP 429) is
// transient: it ends the round and backs the whole signer off in time, and
// never settles anything. A deterministic failure (a value, event list or
// metadata that does not decode, a block hash the journal does not retain, a
// runtime that cannot be resolved) is recorded durably against the step's
// *subject* (the head runtime, the preflight, one attempt, one operation's
// read-back or its reveal), backs that subject off by confirmed blocks, and
// after `DIAG_CAP` failures settles into a durable diagnostic or fault. A
// blocked subject never blocks another one: diagnostics cannot starve new
// commits, and a decode failure never rolls the journal back into a retry
// loop. A reply too large for its read ([`REPLY_TOO_LARGE`]) is a property
// of that pinned read, not of the endpoint's health: it counts against the
// step's subject like a deterministic failure, but never settles.

/// Deterministic failures after which a subject settles.
pub const DIAG_CAP: u64 = 5;
/// Longest block backoff of one subject.
const MAX_BACKOFF_BLOCKS: u64 = 60;
/// The code of a historical read of the live attempt answered with a
/// JSON-RPC error (`on_result_within`).
pub const STATE_UNAVAILABLE: &str = "state_unavailable";
/// The code of a step whose reply exceeded its read's bound
/// (`RpcError::TooLarge`). The same pinned read answers the same bytes, so
/// a signer-wide time backoff would retry it forever and hold every other
/// subject (it held the row snapshot and every commit after epoch 137 on
/// testnet, 2026-11-05/06, when the 488 upgrade block was read whole). It is
/// recorded against the step's subject instead, backs that subject off by
/// blocks and never settles: an oversized answer is no evidence about the
/// chain, so it never becomes a verdict (an attempt is never `displaced`,
/// `unverifiable` or faulted by it), and a build that can read it carries
/// on from the same state.
pub const REPLY_TOO_LARGE: &str = "reply_too_large";
/// Failures after which a [`STATE_UNAVAILABLE`] subject settles: the first
/// count whose backoffs before it span [`MAX_BACKOFF_BLOCKS`] confirmed
/// blocks (never fewer than [`DIAG_CAP`]). An endpoint that answers errors
/// for a shorter span (an overload or an internal error rather than pruned
/// state) is retried and recovers with no settle.
pub const UNAVAILABLE_SETTLE_FAILURES: u64 = {
    let (mut n, mut span) = (1u64, 0u64);
    while span < MAX_BACKOFF_BLOCKS {
        span += backoff_blocks(n);
        n += 1;
    }
    if n < DIAG_CAP { DIAG_CAP } else { n }
};

/// The block backoff after a subject's `failures`-th failure: 1, 2, 4, ...
/// up to [`MAX_BACKOFF_BLOCKS`].
const fn backoff_blocks(failures: u64) -> u64 {
    let shift = failures.saturating_sub(1);
    if shift >= 63 {
        return MAX_BACKOFF_BLOCKS;
    }
    let b = 1u64 << shift;
    if b < MAX_BACKOFF_BLOCKS { b } else { MAX_BACKOFF_BLOCKS }
}
/// Runtime re-learns for one read before it counts as a failure.
const RELEARN_CAP: u64 = 4;
const RPC_BACKOFF_MS: u64 = 5_000;
const RPC_RATE_LIMITED_BACKOFF_MS: u64 = 30_000;
const MAX_RPC_BACKOFF_MS: u64 = 120_000;

const HEAD: &str = "head";
const PREFLIGHT: &str = "preflight";
/// The row snapshot step's subject: its failures back off at the capped
/// rate and never settle (it is not a diagnostic: other validators' rows
/// depend on it).
const ROW_SNAPSHOT: &str = "row_snapshot";

fn attempt_subject(op_id: &str, n: u32) -> String {
    format!("attempt:{n}:{op_id}")
}

fn read_subject(read: &ReadPurpose) -> String {
    match read {
        ReadPurpose::Preflight | ReadPurpose::Registration { .. } => PREFLIGHT.to_owned(),
        ReadPurpose::NonceCheck { op_id, attempt }
        | ReadPurpose::NonceProbe { op_id, attempt, .. }
        | ReadPurpose::Events { op_id, attempt }
        | ReadPurpose::InclusionEvents { op_id, attempt } => attempt_subject(op_id, *attempt),
        ReadPurpose::Readback { op_id } => format!("readback:{op_id}"),
        ReadPurpose::RevealCheck { op_id }
        | ReadPurpose::RevealProbe { op_id, .. }
        | ReadPurpose::RevealEvents { op_id, .. } => format!("reveal:{op_id}"),
        ReadPurpose::RowSchedule | ReadPurpose::RowRegistration { .. } => ROW_SNAPSHOT.to_owned(),
    }
}

/// The subject a plan's failures count against (`None` for a submission,
/// whose outcome is never a failure to retry).
fn plan_subject(plan: &SignPlan) -> Option<String> {
    match plan {
        SignPlan::RuntimeVersion { purpose, .. } | SignPlan::Metadata { purpose, .. } => {
            Some(match purpose {
                RuntimeFor::Head => HEAD.to_owned(),
                RuntimeFor::Writer { read, .. } => read_subject(read),
                RuntimeFor::Sign { .. } => PREFLIGHT.to_owned(),
            })
        }
        SignPlan::Read { read, .. } => Some(read_subject(read)),
        SignPlan::Block { op_id, attempt, .. } => Some(attempt_subject(op_id, *attempt)),
        SignPlan::Submit { .. } => None,
    }
}

/// Whether `subject` waits out a backoff at confirmed block `c`, or settled.
fn blocked(sql: &impl Sql, subject: &str, c: u64) -> Result<bool, StoreError> {
    Ok(sql
        .exec(
            "SELECT retry_block, settled FROM sign_failures WHERE subject = ?",
            &[subject.into()],
        )?
        .first()
        .map(|r| Ok::<_, StoreError>(uint(r, 0)? > c || int(r, 1)? == 1))
        .transpose()?
        .unwrap_or(false))
}

fn clear_failure(sql: &impl Sql, subject: &str) -> Result<(), StoreError> {
    sql.exec(
        "DELETE FROM sign_failures WHERE subject = ? AND settled = 0",
        &[subject.into()],
    )?;
    Ok(())
}

/// Records one deterministic failure of `subject` at confirmed block `c`,
/// backs it off and settles it at the cap.
fn record_failure(
    sql: &impl Sql,
    subject: &str,
    code: &str,
    c: u64,
    now_ms: u64,
) -> Result<(), StoreError> {
    let prior = sql
        .exec(
            "SELECT failures, settled FROM sign_failures WHERE subject = ?",
            &[subject.into()],
        )?
        .first()
        .map(|r| Ok::<_, StoreError>((uint(r, 0)?, int(r, 1)? == 1)))
        .transpose()?;
    if prior.is_some_and(|(_, settled)| settled) {
        return Ok(());
    }
    let failures = prior.map_or(0, |(f, _)| f).saturating_add(1);
    let backoff = backoff_blocks(failures);
    sql.exec(
        "INSERT INTO sign_failures (subject, failures, code, first_at_ms, last_at_ms, retry_block, settled)
         VALUES (?, ?, ?, ?, ?, ?, 0)
         ON CONFLICT(subject) DO UPDATE SET failures = excluded.failures, code = excluded.code,
            last_at_ms = excluded.last_at_ms, retry_block = excluded.retry_block",
        &[
            subject.into(),
            bind_u64(failures)?,
            code.into(),
            bind_u64(now_ms)?,
            bind_u64(now_ms)?,
            bind_u64(c.saturating_add(backoff))?,
        ],
    )?;
    note_error(sql, &format!("{subject}: {code}"), now_ms)?;
    journal::bump(sql, "sign_step_failures")?;
    log(
        sql,
        now_ms,
        Some(c),
        None,
        None,
        "step_failed",
        Some(&format!("{} {code} #{failures}", short_subject(subject))),
    )?;
    let cap = if code == STATE_UNAVAILABLE { UNAVAILABLE_SETTLE_FAILURES } else { DIAG_CAP };
    if failures >= cap && code != REPLY_TOO_LARGE {
        settle(sql, subject, code, now_ms)?;
    }
    Ok(())
}

/// A subject for logs and status: kind plus the last 12 characters of the
/// operation ID.
fn short_subject(subject: &str) -> String {
    match subject.split_once(":slop_ninja:") {
        Some((kind, rest)) => {
            let tail: String = rest.chars().rev().take(12).collect();
            format!("{kind}:{}", tail.chars().rev().collect::<String>())
        }
        None => subject.to_owned(),
    }
}

/// The last 12 characters of an operation ID, for status.
fn short_op(op_id: &str) -> String {
    let tail: String = op_id.chars().rev().take(12).collect();
    tail.chars().rev().collect()
}

fn note_error(sql: &impl Sql, error: &str, now_ms: u64) -> Result<(), StoreError> {
    let error: String = error.chars().take(200).collect();
    sql.exec(
        "UPDATE signer SET last_error = ?, last_error_at_ms = ? WHERE id = 1",
        &[error.as_str().into(), bind_u64(now_ms)?],
    )?;
    Ok(())
}

/// Turns a subject that keeps failing into a durable state.
///
/// - `reveal:OP`: the operation keeps state `committed` with diagnostic
///   `reveal_unverifiable:CODE`, and its reveal is no longer checked (the
///   chain reveals on its own).
/// - `readback:OP`: read-back `unverifiable`; the reveal is still tracked.
/// - `attempt:N:OP` of an included attempt (its events do not decode): the
///   attempt's dispatch is `unverifiable`, the nonce floor still advances
///   past its nonce, and the operation is `committed` with diagnostic
///   `events_unverifiable:CODE`; the read-back then checks the chain's queue.
/// - `attempt:N:OP` whose nonce the head showed consumed (an inclusion
///   search) but whose historical state and bodies the endpoint keeps
///   refusing ([`STATE_UNAVAILABLE`]): its inclusion cannot be decided, so
///   the attempt's dispatch is `unverifiable`, the nonce floor advances past
///   its nonce (consumed at the head, so later nonces are safe), and the
///   operation fails as `inclusion_unverifiable`. The signer is not paused:
///   the chain reveals an included commit on its own, and the next epoch
///   commits with no operator clear.
/// - `attempt:N:OP` otherwise (its nonce cannot be read): a signer fault,
///   because no later nonce can be allocated safely.
/// - `head` and `preflight` never settle: they keep their capped backoff,
///   and status names the error.
fn settle(sql: &impl Sql, subject: &str, code: &str, now_ms: u64) -> Result<(), StoreError> {
    let (kind, rest) = subject.split_once(':').unwrap_or((subject, ""));
    let mark = || {
        sql.exec(
            "UPDATE sign_failures SET settled = 1 WHERE subject = ?",
            &[subject.into()],
        )
    };
    match kind {
        "reveal" => {
            let diagnostic = format!("reveal_unverifiable:{code}");
            sql.exec(
                "UPDATE commit_ops SET diagnostic = ?, next_check_block = NULL, updated_at_ms = ? WHERE op_id = ?",
                &[diagnostic.as_str().into(), bind_u64(now_ms)?, rest.into()],
            )?;
            mark()?;
            log(
                sql,
                now_ms,
                None,
                Some(rest),
                None,
                "diagnostic",
                Some(&diagnostic),
            )?;
        }
        "readback" => {
            let diagnostic = format!("readback_unverifiable:{code}");
            sql.exec(
                "UPDATE commit_ops SET readback = 'unverifiable', diagnostic = ?, updated_at_ms = ? WHERE op_id = ?",
                &[diagnostic.as_str().into(), bind_u64(now_ms)?, rest.into()],
            )?;
            mark()?;
            log(
                sql,
                now_ms,
                None,
                Some(rest),
                None,
                "diagnostic",
                Some(&diagnostic),
            )?;
        }
        "attempt" => {
            let Some((n, op_id)) = rest.split_once(':') else {
                return Ok(());
            };
            let n: u32 = n.parse().map_err(|_| StoreError("attempt subject"))?;
            let Some(a) = attempt(sql, op_id, n)? else {
                return Ok(());
            };
            mark()?;
            if a.state == "included" {
                let diagnostic = format!("events_unverifiable:{code}");
                set_attempt(
                    sql,
                    &a,
                    "dispatch = 'unverifiable', resolved_at_ms = ?",
                    vec![bind_u64(now_ms)?],
                )?;
                sql.exec(
                    "UPDATE signer SET nonce_floor = MAX(nonce_floor, ?) WHERE id = 1",
                    &[bind_u64(a.nonce.saturating_add(1))?],
                )?;
                let block = a.included_block.map_or(Ok(SqlValue::Null), bind_u64)?;
                let hash = sql
                    .exec(
                        "SELECT included_hash FROM sign_attempts WHERE op_id = ? AND attempt = ?",
                        &[op_id.into(), SqlValue::Integer(n.into())],
                    )?
                    .first()
                    .and_then(|r| match r.first() {
                        Some(SqlValue::Text(t)) => Some(SqlValue::Text(t.clone())),
                        _ => None,
                    })
                    .unwrap_or(SqlValue::Null);
                sql.exec(
                    "UPDATE commit_ops SET state = 'committed', commit_block = ?, commit_block_hash = ?,
                        commit_index = ?, diagnostic = ?, updated_at_ms = ? WHERE op_id = ?",
                    &[
                        block,
                        hash,
                        a.included_index
                            .map_or(SqlValue::Null, |i| SqlValue::Integer(i.into())),
                        diagnostic.as_str().into(),
                        bind_u64(now_ms)?,
                        op_id.into(),
                    ],
                )?;
                log(
                    sql,
                    now_ms,
                    a.included_block,
                    Some(op_id),
                    Some(n),
                    "diagnostic",
                    Some(&diagnostic),
                )?;
            } else if code == STATE_UNAVAILABLE && a.search_lo.is_some() && a.search_hi.is_some() {
                let diagnostic = format!("inclusion_unverifiable:{code}");
                set_attempt(
                    sql,
                    &a,
                    "dispatch = 'unverifiable', resolved_at_ms = ?",
                    vec![bind_u64(now_ms)?],
                )?;
                sql.exec(
                    "UPDATE signer SET nonce_floor = MAX(nonce_floor, ?) WHERE id = 1",
                    &[bind_u64(a.nonce.saturating_add(1))?],
                )?;
                journal::bump(sql, "sign_inclusions_unverifiable")?;
                sql.exec(
                    "UPDATE commit_ops SET diagnostic = ?, updated_at_ms = ? WHERE op_id = ?",
                    &[diagnostic.as_str().into(), bind_u64(now_ms)?, op_id.into()],
                )?;
                log(sql, now_ms, a.search_hi, Some(op_id), Some(n), "diagnostic", Some(&diagnostic))?;
                fail_op(sql, op_id, INCLUSION_UNVERIFIABLE, now_ms)?;
            } else {
                fault(sql, "attempt_unverifiable", Some(op_id), Some(n), now_ms)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// One RPC error: the round ends and the signer backs off in time.
fn rpc_failed(sql: &impl Sql, e: RpcError, now_ms: u64) -> Result<(), StoreError> {
    journal::bump(sql, "sign_rpc_failures")?;
    if e == RpcError::RateLimited {
        journal::bump(sql, "rpc_rate_limited")?;
    }
    let k = signer(sql)?.rpc_failures.saturating_add(1);
    let base = if e == RpcError::RateLimited {
        RPC_RATE_LIMITED_BACKOFF_MS
    } else {
        RPC_BACKOFF_MS
    };
    let backoff = base
        .saturating_mul(1u64 << k.saturating_sub(1).min(6))
        .min(MAX_RPC_BACKOFF_MS);
    sql.exec(
        "UPDATE signer SET rpc_failures = ?, backoff_until_ms = ? WHERE id = 1",
        &[bind_u64(k)?, bind_u64(now_ms.saturating_add(backoff))?],
    )?;
    note_error(sql, &format!("rpc: {e:?}"), now_ms)
}

fn rpc_answered(sql: &impl Sql) -> Result<(), StoreError> {
    sql.exec("UPDATE signer SET rpc_failures = 0 WHERE id = 1", &[])?;
    Ok(())
}

/// Reserves one call from signing's own sub-budget and the shared budget.
fn reserve_sign_rpc(sql: &impl Sql, cfg: &SignConfig, now_ms: u64) -> Result<bool, StoreError> {
    let s = signer(sql)?;
    let (mut start, mut used) = (s.budget_window_ms, s.budget_used);
    if now_ms.saturating_sub(start) >= claims::BUDGET_WINDOW_MS || now_ms < start {
        start = now_ms;
        used = 0;
    }
    if used.saturating_add(1) > cfg.sign_rpc_calls_per_minute {
        return Ok(false);
    }
    if !claims::reserve_rpc(sql, 1, cfg.rpc_calls_per_minute, now_ms)? {
        return Ok(false);
    }
    sql.exec(
        "UPDATE signer SET budget_window_ms = ?, budget_used = ? WHERE id = 1",
        &[bind_u64(start)?, bind_u64(used.saturating_add(1))?],
    )?;
    Ok(true)
}

// ---------------------------------------------------------------- targets

/// An installed target's verified records: a `testnet-reward-v0` package
/// (T12 v1 bundle) or a `reward-v1` bundle.
pub enum Verified {
    V1(Box<target::VerifiedTarget>),
    V2(Box<slop_ninja_protocol::VerifiedTarget>),
}

/// An installed target, re-verified from its stored bundle on each use.
pub struct ActiveTarget {
    pub digest: String,
    pub verified: Verified,
    /// The journaled revocation of this target with the latest issue epoch,
    /// if any ([`authority::revocation_of`]), that every reward-v1 operation
    /// is checked against: it withdraws whatever an earlier one does.
    pub revocations: Vec<slop_ninja_protocol::RewardIntentRevocation>,
    /// v1: the protocol weight operation of (target, hotkey); each epoch's
    /// identity is [`commit_op_id`]. v2: the target digest; each epoch's
    /// identity is the protocol `WeightOperationV2`, issued only under the
    /// bundle's checked authorization ([`ActiveTarget::op_id`]).
    pub base_op: String,
    pub mechanism: u8,
}

impl ActiveTarget {
    /// This validator's operation identity at `epoch`. A reward-v1 target
    /// re-checks its authorization here: the held active target, its
    /// activation, the authorization's issue and the journaled revocations
    /// (`authorization_revoked`). There is no end epoch.
    pub fn op_id(
        &self,
        keys: &slop_ninja_protocol::TrustedKeys,
        hotkey: [u8; 32],
        epoch: u64,
    ) -> Result<String, target::Refusal> {
        match &self.verified {
            Verified::V1(_) => Ok(commit_op_id(&self.base_op, epoch)),
            Verified::V2(v) => v
                .operation(keys, &self.revocations, slop_ninja_protocol::AccountId(hotkey), epoch)
                .map(|op| op.id())
                .map_err(|e| target::Refusal {
                    code: if matches!(e, slop_ninja_protocol::Error::Revoked(_)) {
                        "authorization_revoked"
                    } else {
                        "authorization_refused"
                    },
                    detail: format!("{e} ({})", e.code()),
                }),
        }
    }
}

/// Installs `bundle` (already parsed) as the single active target.
pub fn install_target(
    sql: &impl Sql,
    cfg: &SignConfig,
    keys: &slop_ninja_protocol::TrustedKeys,
    bundle_json: &str,
    now_ms: u64,
) -> Result<Result<String, target::Refusal>, StoreError> {
    if target::is_v2(bundle_json.as_bytes()) {
        // P1-R `launch_needs_restart`: after the first reward-v1 install, a
        // launch bundle installs only through the restart decision, so every later
        // launch row is a restart whose floor is its issue epoch (P1-V). A
        // byte-identical re-post of the active bundle is an idempotent
        // replay that writes nothing.
        let is_launch = target::parse_v2(bundle_json.as_bytes()).is_ok_and(|b| b.source.is_none());
        if is_launch && !sql.exec("SELECT 1 FROM sign_targets WHERE reward_rule = 'reward-v1' LIMIT 1", &[])?.is_empty() {
            let active = sql.exec("SELECT target, bundle FROM sign_targets WHERE active = 1", &[])?;
            if let Some(r) = active.first()
                && text(r, 1)? == bundle_json
            {
                let digest = text(r, 0)?.to_owned();
                // A withdrawn authorization stays refused, replay or not.
                if !authority::revocation_of(sql, &digest)?.is_empty() {
                    return Ok(Err(target::Refusal {
                        code: "authorization_revoked",
                        detail: "a journaled revocation withdraws this authorization".into(),
                    }));
                }
                return Ok(Ok(digest));
            }
            return Ok(Err(target::Refusal {
                code: "launch_needs_restart",
                detail: "a launch bundle after the first install is installed only by a restart decision".into(),
            }));
        }
        return install_target_v2(sql, cfg, keys, bundle_json, now_ms);
    }
    // After a restart only the restarted record's bundles install
    // ([`restart_floor_ok`]); a T12 bundle carries no issued or opened epoch
    // to hold to the floor, so the earlier record's, signed by the same keys,
    // would follow it. None installs after any restart.
    if restart_floor(sql)?.is_some() {
        return Ok(Err(target::Refusal {
            code: "v1_after_restart",
            detail: "a T12 bundle cannot be held to the restarted record's first epoch".into(),
        }));
    }
    let bundle = match target::parse(bundle_json.as_bytes()) {
        Ok(b) => b,
        Err(r) => return Ok(Err(r)),
    };
    let verified = match target::verify(&bundle, keys, &cfg.genesis.0, cfg.netuid) {
        Ok(v) => v,
        Err(r) => return Ok(Err(r)),
    };
    let digest = verified.target_digest.to_string();
    let t = &verified.target;
    sql.exec("UPDATE sign_targets SET active = 0", &[])?;
    sql.exec(
        "INSERT INTO sign_targets (target, bundle, cohort, cohort_label, revision, mechanism, manifest,
            package, reward_rule, installed_at_ms, active)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)
         ON CONFLICT(target) DO UPDATE SET active = 1",
        &[
            digest.as_str().into(),
            bundle_json.into(),
            bind_u64(t.cohort)?,
            t.cohort_label.as_str().into(),
            bind_u64(t.revision)?,
            SqlValue::Integer(t.chain.mechanism.into()),
            t.cohort_manifest.to_string().into(),
            t.score_package.to_string().into(),
            verified.package.reward_rule.name().into(),
            bind_u64(now_ms)?,
        ],
    )?;
    log(
        sql,
        now_ms,
        None,
        None,
        None,
        "target_installed",
        Some(&digest),
    )?;
    Ok(Ok(digest))
}

/// The reward-v1 target this coordinator holds active: its own record of the
/// checked closures it accepted, the launch burn row before any.
fn held_target(sql: &impl Sql, cfg: &SignConfig) -> Result<slop_ninja_protocol::ActiveTarget, StoreError> {
    let chain = slop_ninja_protocol::ChainBinding {
        genesis: slop_ninja_protocol::Digest(cfg.genesis.0),
        netuid: cfg.netuid,
        mechanism: 0,
    };
    let rows = sql.exec("SELECT target, activated_epoch FROM sign_held_target WHERE id = 1", &[])?;
    match rows.first() {
        None => slop_ninja_protocol::ActiveTarget::launch(&chain).map_err(|_| StoreError("held target")),
        Some(r) => {
            let digest: [u8; 32] = hex::decode(text(r, 0)?)
                .ok()
                .and_then(|b| b.try_into().ok())
                .ok_or(StoreError("held target digest"))?;
            let activated = opt_u(r, 1);
            slop_ninja_protocol::ActiveTarget::restore(&chain, slop_ninja_protocol::Digest(digest), activated)
                .map_err(|_| StoreError("held target"))
        }
    }
}

/// Records the target active after a verified bundle, forward only: the same
/// target, or one a later checked closure activated.
fn record_held(
    sql: &impl Sql,
    held: &slop_ninja_protocol::ActiveTarget,
    next: &slop_ninja_protocol::ActiveTarget,
    now_ms: u64,
) -> Result<Result<(), target::Refusal>, StoreError> {
    if next == held {
        return Ok(Ok(()));
    }
    let forward = match (held.activated_epoch(), next.activated_epoch()) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(a), Some(b)) => b > a,
    };
    if !forward {
        return Ok(Err(target::Refusal { code: "held_target_regressed", detail: String::new() }));
    }
    sql.exec(
        "INSERT INTO sign_held_target (id, target, activated_epoch, updated_at_ms) VALUES (1, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET target = excluded.target, activated_epoch = excluded.activated_epoch,
            updated_at_ms = excluded.updated_at_ms",
        &[
            hex::encode(next.digest().0).into(),
            next.activated_epoch().map_or(SqlValue::Null, |e| SqlValue::Integer(e as i64)),
            bind_u64(now_ms)?,
        ],
    )?;
    log(sql, now_ms, None, None, None, "held_target_recorded", Some(&next.digest().to_string()))?;
    // Cohorts the automatic handoff checked from the earlier held target
    // are checked again from this one.
    auto::clear_pending(sql)?;
    Ok(Ok(()))
}

/// The first epoch of the chain record the latest restart installed
/// ([`restart_target`]), if any: every bundle installed after it must come
/// from that record ([`restart_floor_ok`]).
fn restart_floor(sql: &impl Sql) -> Result<Option<u64>, StoreError> {
    let rows = sql.exec(
        "SELECT from_epoch FROM sign_held_target_resets ORDER BY seq DESC LIMIT 1",
        &[],
    )?;
    Ok(rows.first().and_then(|r| opt_u(r, 0)))
}

/// The highest epoch any reward-v1 bundle installed here was issued (a
/// launch bundle's authorization) or opened (a cohort bundle's cohort) at,
/// whatever record it came from; `None` before the first such install under
/// schema v8. A restart's first epoch must exceed it ([`restart_target`]).
fn record_epoch(sql: &impl Sql) -> Result<Option<u64>, StoreError> {
    let rows = sql.exec("SELECT epoch FROM sign_record_epoch WHERE id = 1", &[])?;
    Ok(rows.first().and_then(|r| opt_u(r, 0)))
}

/// Raises [`record_epoch`] to `epoch` (never lowers it).
fn note_record_epoch(sql: &impl Sql, epoch: u64, now_ms: u64) -> Result<(), StoreError> {
    sql.exec(
        "INSERT INTO sign_record_epoch (id, epoch, updated_at_ms) VALUES (1, ?, ?)
         ON CONFLICT(id) DO UPDATE SET epoch = MAX(epoch, excluded.epoch), updated_at_ms = excluded.updated_at_ms",
        &[bind_u64(epoch)?, bind_u64(now_ms)?],
    )?;
    Ok(())
}

/// Whether a bundle may follow a restart whose record starts at `floor`: a
/// launch bundle's authorization was issued at or after it, and a cohort
/// bundle's cohort opened at or after it. The earlier record's bundles,
/// signed by the same keys, were issued or opened before it, so none of them
/// can be installed again after the restart.
pub fn restart_floor_ok(
    floor: u64,
    launch_issued: Option<u64>,
    cohort_opened: Option<u64>,
) -> Result<(), &'static str> {
    match (launch_issued, cohort_opened) {
        (Some(issued), None) if issued >= floor => Ok(()),
        (None, Some(opened)) if opened >= floor => Ok(()),
        _ => Err("bundle_before_restarted_record"),
    }
}

/// P1-R: the unsigned `/signing/restart` route refuses; a restart is the
/// signed operator decision `decide::restart`.
pub const RESTART_NEEDS_DECISION: bool = true;

/// What an operator restart of the held target did ([`restart_target`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Restarted {
    /// The held record set aside (its target digest and activation epoch);
    /// `None` when the launch burn row was held.
    pub previous: Option<(String, Option<u64>)>,
    /// The installed target it replaced, if any.
    pub replaced: Option<String>,
    /// The restarted chain's launch target, now installed and active.
    pub installed: String,
    /// The restarted record's first epoch.
    pub from_epoch: u64,
}

/// Operator restart of the held target for a chain record restarted under
/// the same trust roots (the eval Worker's new main authority, R_0 with no
/// predecessor): the coordinator's own counterpart of the native validator's
/// `reset-target`. One transaction, refused unless signing is paused: the
/// restarted record's first epoch `from_epoch` (the restart record's
/// `PUBLISH_FROM_EPOCH`) must be after the epoch the held target activated
/// and not before an earlier restart's, and the supplied bundle must be a
/// launch bundle that verifies against the launch burn row and was issued at
/// or after `from_epoch`; then the held
/// record is moved to the reset history (never deleted), the bundle is
/// installed as the only active target, and `from_epoch` becomes the floor
/// every later bundle must meet ([`restart_floor_ok`]). A refusal changes
/// nothing, so this action is never a probe.
///
/// `from_epoch` must also exceed every epoch an installed bundle was issued
/// or opened at ([`record_epoch`], `restart_regressed`), the earlier
/// restarts' own launch bundles included. So a second restart can never
/// reuse the first one's floor while the launch burn row is held (no
/// activation to bound it): every bundle of a record set aside was issued or
/// opened before its restart's floor, or installed since, and so below the
/// next one.
///
/// Operations, attempts and reveals are left alone: a live attempt keeps
/// reconciling from finalized state and is never sent again, committed
/// operations keep their read-back and reveal checks, and an operation that
/// never reached the chain is never signed under another target (an epoch
/// holds one operation, `commit_ops.epoch` being unique, so the restarted
/// target's first commit takes the next free epoch).
pub fn restart_target(
    sql: &impl Sql,
    cfg: &SignConfig,
    keys: &slop_ninja_protocol::TrustedKeys,
    bundle_json: &str,
    from_epoch: u64,
    now_ms: u64,
) -> Result<Result<Restarted, target::Refusal>, StoreError> {
    let refuse = |code: &'static str, detail: &str| {
        Ok(Err(target::Refusal {
            code,
            detail: detail.to_owned(),
        }))
    };
    if signer(sql)?.state != "paused" {
        return refuse("signing_active", "pause signing first");
    }
    if restart_floor(sql)?.is_some_and(|f| from_epoch < f) {
        return refuse(
            "restart_regressed",
            "from_epoch is before the latest restart's",
        );
    }
    // The record set aside activated its held target at that epoch, after
    // every cohort it installed opened (a cohort's target activates at its
    // closure), and its launch bundle was issued before any of them: a floor
    // above it leaves none of that record's bundles a way back in, so a low
    // or mistyped `from_epoch` is refused instead of reopening them.
    if let Some(activated) = held_target(sql, cfg)?.activated_epoch()
        && from_epoch <= activated
    {
        return refuse(
            "restart_before_held_record",
            &format!("from_epoch must be after epoch {activated}, when the held target activated"),
        );
    }
    // Every bundle installed so far, from any record (the earlier restarts'
    // own launch bundles included), was issued or opened at or below the
    // high-water mark: a floor above it lets none of them back in, also while
    // the launch burn row is held and no activation bounds the floor.
    if let Some(high) = record_epoch(sql)?
        && from_epoch <= high
    {
        return refuse(
            "restart_regressed",
            &format!(
                "from_epoch must be after epoch {high}, the latest an installed bundle was issued or opened at"
            ),
        );
    }
    let chain = slop_ninja_protocol::ChainBinding {
        genesis: slop_ninja_protocol::Digest(cfg.genesis.0),
        netuid: cfg.netuid,
        mechanism: 0,
    };
    let launch = slop_ninja_protocol::ActiveTarget::launch(&chain)
        .map_err(|_| StoreError("launch target"))?;
    let verified = match target::parse_v2(bundle_json.as_bytes())
        .and_then(|b| target::verify_v2(&b, keys, &cfg.genesis.0, cfg.netuid, &launch))
    {
        Ok(v) => v,
        Err(r) => return Ok(Err(r)),
    };
    if verified.source.is_some() {
        return refuse(
            "not_a_launch_bundle",
            "a restart installs the restarted record's launch bundle",
        );
    }
    let issued = match verified.checked_terms(keys) {
        Ok(terms) => terms.issued_epoch,
        Err(e) => return refuse("authorization_refused", &e.to_string()),
    };
    if let Err(code) = restart_floor_ok(from_epoch, Some(issued), None) {
        return refuse(
            code,
            "the launch authorization was issued before from_epoch",
        );
    }
    let held = held_target(sql, cfg)?;
    let previous =
        (!held.is_launch_burn()).then(|| (held.digest().to_string(), held.activated_epoch()));
    let replaced = sql
        .exec("SELECT target FROM sign_targets WHERE active = 1", &[])?
        .first()
        .map(|r| text(r, 0).map(str::to_owned))
        .transpose()?;
    sql.exec(
        "INSERT INTO sign_held_target_resets (held_target, held_activated_epoch, from_epoch, launch_issued_epoch, at_ms)
         VALUES (?, ?, ?, ?, ?)",
        &[
            previous.as_ref().map_or(SqlValue::Null, |(d, _)| d.as_str().into()),
            previous
                .as_ref()
                .and_then(|(_, e)| *e)
                .map_or(SqlValue::Null, |e| SqlValue::Integer(e as i64)),
            bind_u64(from_epoch)?,
            bind_u64(issued)?,
            bind_u64(now_ms)?,
        ],
    )?;
    sql.exec("DELETE FROM sign_held_target WHERE id = 1", &[])?;
    auto::clear_pending(sql)?;
    // Verified above against the same launch row and floor: a refusal here
    // rolls the whole restart back.
    let installed = match install_target_v2(sql, cfg, keys, bundle_json, now_ms)? {
        Ok(d) => d,
        Err(_) => return Err(StoreError("restart install")),
    };
    let detail = match &previous {
        Some((digest, epoch)) => format!(
            "{digest}@{} from {from_epoch}",
            epoch.map_or("none".into(), |e| e.to_string())
        ),
        None => format!("launch from {from_epoch}"),
    };
    journal::bump(sql, "held_target_resets")?;
    log(
        sql,
        now_ms,
        None,
        None,
        None,
        "held_target_reset",
        Some(&detail),
    )?;
    Ok(Ok(Restarted {
        previous,
        replaced,
        installed,
        from_epoch,
    }))
}

/// Installs a reward-v1 bundle (verified, outcome recomputed) as the single
/// active target. The bundle is checked against the target this coordinator
/// holds active, never one rebuilt from the bundle: only the held target or
/// its successor (activated by the bundle's checked closure) is accepted, and
/// the launch bundle only while no cohort target has activated.
fn install_target_v2(
    sql: &impl Sql,
    cfg: &SignConfig,
    keys: &slop_ninja_protocol::TrustedKeys,
    bundle_json: &str,
    now_ms: u64,
) -> Result<Result<String, target::Refusal>, StoreError> {
    let held = held_target(sql, cfg)?;
    Ok(install_target_v2_from(sql, cfg, keys, bundle_json, now_ms, &held, false)?.map(|(digest, _)| digest))
}

/// [`install_target_v2`] against `held`: the journal's held target, or (the
/// automatic handoff, `auto::handle`) the target the pending intermediates
/// it checked from the journal's held target in this transaction leave
/// held. With `install_revoked` a bundle whose authorization a journaled
/// revocation withdraws is still installed (its checked closure activated
/// the target), as revoked: nothing commits for it, as when the revocation
/// comes after the install; the reply says so. Every refusal comes before
/// any write.
pub(crate) fn install_target_v2_from(
    sql: &impl Sql,
    cfg: &SignConfig,
    keys: &slop_ninja_protocol::TrustedKeys,
    bundle_json: &str,
    now_ms: u64,
    held: &slop_ninja_protocol::ActiveTarget,
    install_revoked: bool,
) -> Result<Result<(String, bool), target::Refusal>, StoreError> {
    // Every later signing step parses and verifies the stored bundle again
    // within the journal's footprint: a larger one is never installed.
    if bundle_json.len() > auto::SIGNED_BUNDLE_BYTES {
        return Ok(Err(target::Refusal {
            code: "bundle_exceeds_signing_footprint",
            detail: format!("{} B, at most {} B", bundle_json.len(), auto::SIGNED_BUNDLE_BYTES),
        }));
    }
    let verified = match target::parse_v2(bundle_json.as_bytes())
        .and_then(|b| target::verify_v2(&b, keys, &cfg.genesis.0, cfg.netuid, held))
    {
        Ok(v) => v,
        Err(r) => return Ok(Err(r)),
    };
    // The epoch the bundle was issued (launch) or opened (cohort) at.
    let launch_issued = match &verified.source {
        None => verified.checked_terms(keys).ok().map(|terms| terms.issued_epoch),
        Some(_) => None,
    };
    let opened = verified.source.as_ref().map(|s| s.manifest.opened_epoch);
    // After a restart, only the restarted record's bundles follow it.
    if let Some(floor) = restart_floor(sql)?
        && let Err(code) = restart_floor_ok(floor, launch_issued, opened)
    {
        return Ok(Err(target::Refusal {
            code,
            detail: format!("the restarted record starts at epoch {floor}"),
        }));
    }
    // A bundle whose authorization a journaled revocation withdraws would
    // commit nothing; a newer authorization of the target is needed.
    let revocations = authority::revocation_of(sql, &verified.target_digest.to_string())?;
    let revoked = match verified.revoked_by(keys, &revocations) {
        Ok(None) => false,
        Ok(Some(_)) if install_revoked => true,
        Ok(Some(revocation)) => {
            return Ok(Err(target::Refusal {
                code: "authorization_revoked",
                detail: format!("revocation {revocation} withdraws this authorization"),
            }));
        }
        Err(e) => {
            return Ok(Err(target::Refusal {
                code: "authorization_refused",
                detail: format!("{e} ({})", e.code()),
            }));
        }
    };
    if let Err(r) = record_held(sql, held, &verified.active, now_ms)? {
        return Ok(Err(r));
    }
    let digest = verified.target_digest.to_string();
    let (cohort, label, revision, manifest, package) = match &verified.source {
        Some(s) => (
            s.package.cohort,
            s.package.cohort_label.clone(),
            s.package.revision,
            slop_ninja_protocol::Record::digest(&s.manifest).map_err(|_| StoreError("manifest digest"))?.to_string(),
            slop_ninja_protocol::Record::digest(&s.package).map_err(|_| StoreError("package digest"))?.to_string(),
        ),
        None => (0, "launch-burn".to_owned(), 0, digest.clone(), digest.clone()),
    };
    sql.exec("UPDATE sign_targets SET active = 0", &[])?;
    sql.exec(
        "INSERT INTO sign_targets (target, bundle, cohort, cohort_label, revision, mechanism, manifest,
            package, reward_rule, installed_at_ms, active)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1)
         ON CONFLICT(target) DO UPDATE SET active = 1, bundle = excluded.bundle",
        &[
            digest.as_str().into(),
            bundle_json.into(),
            bind_u64(cohort)?,
            label.as_str().into(),
            bind_u64(revision)?,
            SqlValue::Integer(verified.chain.mechanism.into()),
            manifest.into(),
            package.into(),
            "reward-v1".into(),
            bind_u64(now_ms)?,
        ],
    )?;
    if let Some(epoch) = launch_issued.or(opened) {
        note_record_epoch(sql, epoch, now_ms)?;
    }
    log(sql, now_ms, None, None, None, if revoked { "target_installed_revoked" } else { "target_installed" }, Some(&digest))?;
    Ok(Ok((digest, revoked)))
}

fn active_target(
    sql: &impl Sql,
    cfg: &SignConfig,
    keys: &slop_ninja_protocol::TrustedKeys,
) -> Result<Option<Result<ActiveTarget, target::Refusal>>, StoreError> {
    let rows = sql.exec(
        "SELECT target, bundle FROM sign_targets WHERE active = 1",
        &[],
    )?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    let digest = text(r, 0)?.to_owned();
    if target::is_v2(text(r, 1)?.as_bytes()) {
        let held = held_target(sql, cfg)?;
        let verified = match target::parse_v2(text(r, 1)?.as_bytes())
            .and_then(|b| target::verify_v2(&b, keys, &cfg.genesis.0, cfg.netuid, &held))
        {
            Ok(v) => v,
            Err(e) => return Ok(Some(Err(e))),
        };
        // A bundle installed before the held record existed records it now.
        if let Err(e) = record_held(sql, &held, &verified.active, 0)? {
            return Ok(Some(Err(e)));
        }
        if verified.target_digest.to_string() != digest {
            return Ok(Some(Err(target::Refusal { code: "target_digest_changed", detail: String::new() })));
        }
        // A journaled signed revocation withdraws the installed
        // authorization: nothing of its target is planned or signed until a
        // newer authorization is installed. Time alone never refuses it.
        // Only this target's latest revocation is read (one row), and it is
        // not verified again, so neither the journal's length nor a rotated
        // trust root changes what this costs or whether it succeeds.
        let revocations = authority::revocation_of(sql, &digest)?;
        match verified.revoked_by(keys, &revocations) {
            Ok(None) => {}
            Ok(Some(revocation)) => {
                return Ok(Some(Err(target::Refusal {
                    code: "authorization_revoked",
                    detail: format!("revocation {revocation}"),
                })));
            }
            Err(e) => {
                return Ok(Some(Err(target::Refusal {
                    code: "authorization_refused",
                    detail: format!("{e} ({})", e.code()),
                })));
            }
        }
        let mechanism = verified.chain.mechanism;
        return Ok(Some(Ok(ActiveTarget {
            base_op: digest.clone(),
            digest,
            verified: Verified::V2(Box::new(verified)),
            revocations,
            mechanism,
        })));
    }
    let bundle = match target::parse(text(r, 1)?.as_bytes()) {
        Ok(b) => b,
        Err(e) => return Ok(Some(Err(e))),
    };
    let verified = match target::verify(&bundle, keys, &cfg.genesis.0, cfg.netuid) {
        Ok(v) => v,
        Err(e) => return Ok(Some(Err(e))),
    };
    if verified.target_digest.to_string() != digest {
        return Ok(Some(Err(target::Refusal {
            code: "target_digest_changed",
            detail: String::new(),
        })));
    }
    let op = slop_ninja_protocol::WeightOperation::for_target(
        &verified.target,
        slop_ninja_protocol::AccountId(cfg.hotkey),
    );
    let base_op = match op.id() {
        Ok(id) => id,
        Err(e) => {
            return Ok(Some(Err(target::Refusal {
                code: "operation_id",
                detail: e.to_string(),
            })));
        }
    };
    let mechanism = verified.target.chain.mechanism;
    Ok(Some(Ok(ActiveTarget {
        digest,
        verified: Verified::V1(Box::new(verified)),
        revocations: Vec::new(),
        base_op,
        mechanism,
    })))
}

// --------------------------------------------------------- op and attempt

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preflight {
    pub block: BlockRef,
    pub timestamp_ms: u64,
    pub nonce: u64,
    pub uid: u16,
    pub population: u16,
    pub tempo: u16,
    pub last_epoch_block: u64,
    pub pending_epoch_at: u64,
    pub subnet_epoch_index: u64,
    pub blocks_since_last_step: u64,
    pub reveal_period: u64,
    pub commit_reveal_enabled: bool,
    pub commit_reveal_version: u16,
    pub required_version_key: u64,
    pub rate_limit: u64,
    pub last_update: u64,
    pub min_allowed_weights: Option<u64>,
    pub max_weights_limit: Option<u64>,
    pub spec_version: u32,
}

impl Preflight {
    fn schedule(&self) -> EpochScheduleState {
        chain::schedule(
            self.tempo,
            self.last_epoch_block,
            self.pending_epoch_at,
            self.subnet_epoch_index,
            self.blocks_since_last_step,
            self.block.number,
        )
    }
}

#[derive(Clone, Debug)]
struct Op {
    op_id: String,
    epoch: u64,
    state: String,
    preflight: Option<Preflight>,
    uid: Option<u16>,
    row: Option<RowSummary>,
    predicted_reveal_block: Option<u64>,
    commit_block: Option<u64>,
    readback: Option<String>,
    next_check_block: Option<u64>,
    /// The reveal search: `lo` known queued, `hi` known gone.
    reveal_search: Option<(u64, u64)>,
}

const OP_COLUMNS: &str = "op_id, epoch, state, preflight_json, uid, row_json, reveal_round,
    predicted_reveal_block, commit_block, readback, next_check_block, reveal_lo, reveal_hi";

fn op_from(r: &crate::sql::Row) -> Result<Op, StoreError> {
    let opt_text = |i| match r.get(i) {
        Some(SqlValue::Text(t)) => Some(t.clone()),
        _ => None,
    };
    let opt_u64 = |i| match r.get(i) {
        Some(SqlValue::Integer(v)) => Some(*v as u64),
        _ => None,
    };
    Ok(Op {
        op_id: text(r, 0)?.to_owned(),
        epoch: uint(r, 1)?,
        state: text(r, 2)?.to_owned(),
        preflight: opt_text(3)
            .map(|j| serde_json::from_str(&j))
            .transpose()
            .map_err(|_| StoreError("preflight json"))?,
        uid: opt_u64(4).map(|u| u as u16),
        row: opt_text(5)
            .map(|j| serde_json::from_str(&j))
            .transpose()
            .map_err(|_| StoreError("row json"))?,
        predicted_reveal_block: opt_u64(7),
        commit_block: opt_u64(8),
        readback: opt_text(9),
        next_check_block: opt_u64(10),
        reveal_search: opt_u64(11).zip(opt_u64(12)),
    })
}

fn op(sql: &impl Sql, op_id: &str) -> Result<Option<Op>, StoreError> {
    sql.exec(
        &format!("SELECT {OP_COLUMNS} FROM commit_ops WHERE op_id = ?"),
        &[op_id.into()],
    )?
    .first()
    .map(op_from)
    .transpose()
}

fn ops_where(sql: &impl Sql, clause: &str) -> Result<Vec<Op>, StoreError> {
    sql.exec(
        &format!("SELECT {OP_COLUMNS} FROM commit_ops WHERE {clause} ORDER BY epoch"),
        &[],
    )?
    .iter()
    .map(op_from)
    .collect()
}

fn set_op_state(
    sql: &impl Sql,
    op_id: &str,
    state: &str,
    detail: Option<&str>,
    now_ms: u64,
) -> Result<(), StoreError> {
    sql.exec(
        "UPDATE commit_ops SET state = ?, detail = COALESCE(?, detail), updated_at_ms = ? WHERE op_id = ?",
        &[state.into(), detail.map_or(SqlValue::Null, |d| d.into()), bind_u64(now_ms)?, op_id.into()],
    )?;
    log(
        sql,
        now_ms,
        None,
        Some(op_id),
        None,
        &format!("op_{state}"),
        detail,
    )
}

/// An operation's failure detail when its consumed nonce's inclusion could
/// not be read (`settle`): failed for this signer, its inclusion unknown.
pub const INCLUSION_UNVERIFIABLE: &str = "inclusion_unverifiable";

fn fail_op(sql: &impl Sql, op_id: &str, code: &str, now_ms: u64) -> Result<(), StoreError> {
    journal::bump(sql, "commit_ops_failed")?;
    set_op_state(sql, op_id, "failed", Some(code), now_ms)
}

#[derive(Clone, Debug)]
struct Attempt {
    op_id: String,
    attempt: u32,
    nonce: u64,
    expiry: u64,
    floor_block: u64,
    reveal_round: u64,
    extrinsic_hex: String,
    state: String,
    search_lo: Option<u64>,
    search_hi: Option<u64>,
    checked_block: Option<u64>,
    included_block: Option<u64>,
    included_index: Option<u32>,
    ciphertext_sha256: String,
    /// The inclusion search reads block bodies one by one instead of the
    /// nonce at retained blocks ([`attempt_state_unavailable`]).
    search_scan: bool,
    /// The inclusion block's events were read (D5) and held no commit event
    /// of this hotkey, so its body decides.
    events_checked: bool,
}

const ATTEMPT_COLUMNS: &str =
    "op_id, attempt, nonce, expiry, floor_block, reveal_round, extrinsic_hex,
    state, search_lo, search_hi, checked_block, included_block, included_index, ciphertext_sha256, search_scan,
    events_checked";

fn attempt_from(r: &crate::sql::Row) -> Result<Attempt, StoreError> {
    let opt = |i| match r.get(i) {
        Some(SqlValue::Integer(v)) => Some(*v as u64),
        _ => None,
    };
    Ok(Attempt {
        op_id: text(r, 0)?.to_owned(),
        attempt: uint(r, 1)? as u32,
        nonce: uint(r, 2)?,
        expiry: uint(r, 3)?,
        floor_block: uint(r, 4)?,
        reveal_round: uint(r, 5)?,
        extrinsic_hex: text(r, 6)?.to_owned(),
        state: text(r, 7)?.to_owned(),
        search_lo: opt(8),
        search_hi: opt(9),
        checked_block: opt(10),
        included_block: opt(11),
        included_index: opt(12).map(|v| v as u32),
        ciphertext_sha256: text(r, 13)?.to_owned(),
        search_scan: opt(14).is_some_and(|v| v != 0),
        events_checked: opt(15).is_some_and(|v| v != 0),
    })
}

fn attempt(sql: &impl Sql, op_id: &str, n: u32) -> Result<Option<Attempt>, StoreError> {
    sql.exec(
        &format!("SELECT {ATTEMPT_COLUMNS} FROM sign_attempts WHERE op_id = ? AND attempt = ?"),
        &[op_id.into(), SqlValue::Integer(n.into())],
    )?
    .first()
    .map(attempt_from)
    .transpose()
}

/// The single attempt whose fate is not settled, if any.
fn live_attempt(sql: &impl Sql) -> Result<Option<Attempt>, StoreError> {
    let rows = sql.exec(
        &format!(
            "SELECT {ATTEMPT_COLUMNS} FROM sign_attempts
             WHERE state IN ('signed', 'sending', 'submitted', 'uncertain', 'included')
               AND dispatch IS NULL
             ORDER BY signed_at_ms"
        ),
        &[],
    )?;
    if rows.len() > 1 {
        return Err(StoreError("more than one live attempt"));
    }
    rows.first().map(attempt_from).transpose()
}

fn set_attempt(
    sql: &impl Sql,
    a: &Attempt,
    sets: &str,
    params: Vec<SqlValue>,
) -> Result<(), StoreError> {
    let mut all = params;
    all.push(a.op_id.as_str().into());
    all.push(SqlValue::Integer(a.attempt.into()));
    sql.exec(
        &format!("UPDATE sign_attempts SET {sets} WHERE op_id = ? AND attempt = ?"),
        &all,
    )?;
    Ok(())
}

// --------------------------------------------------------------- planning

/// The context one planning or result call needs.
pub struct Ctx<'a> {
    pub cfg: &'a SignConfig,
    pub trusted: &'a slop_ninja_protocol::TrustedKeys,
    pub runtime: &'a RuntimeCache,
    pub keys: &'a dyn KeySource,
}

/// The next step, or `None`. Skips at once while another holder's claim is
/// live or an RPC-error backoff runs; otherwise takes the claim (a new
/// fencing generation), recovers what an earlier generation abandoned,
/// plans, and releases the claim again when there is nothing to do.
pub fn plan(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    holder: &str,
    now_ms: u64,
) -> Result<Option<SignPlan>, SignError> {
    plan_within(sql, ctx, holder, None, now_ms)
}

/// The stale-invocation drill: how long the stalled holder sleeps before it
/// tries to mark.
pub const DRILL_STALL_SLEEP_MS: u64 = 15_000;

/// The worst-case wall time of running `plan` and reporting its outcome,
/// with `journal_call_ms` for each journal call (the allowance the Worker's
/// platform timeout enforces): the step's RPC call at its own timeout (the
/// metadata fetch's or the streamed block read's, else one call's), a
/// submission's mark before it (and, for the stale-holder drill, the
/// stall), and the result report. The
/// Worker runs a step only when it fits its budget, and the journal plans
/// only a step that fits the budget it is given ([`plan_within`]).
pub fn step_worst_ms(plan: &SignPlan, journal_call_ms: u64) -> u64 {
    let call = match plan {
        SignPlan::Metadata { .. } => crate::rpc::METADATA_TIMEOUT_MS,
        SignPlan::Block { .. } => crate::rpc::BLOCK_TIMEOUT_MS,
        SignPlan::Submit { drill, .. } => {
            let stall = if drill.as_deref() == Some("stall_before_mark") {
                journal_call_ms.saturating_add(DRILL_STALL_SLEEP_MS)
            } else {
                0
            };
            stall.saturating_add(journal_call_ms).saturating_add(crate::rpc::CALL_TIMEOUT_MS)
        }
        _ => crate::rpc::CALL_TIMEOUT_MS,
    };
    call.saturating_add(journal_call_ms)
}

/// [`plan`] for a caller with `budget_ms` left for the step (`None`: no
/// bound). A step whose worst case ([`step_worst_ms`]) does not fit is not
/// handed out: the claim is released at once with nothing pending, no RPC
/// call is charged and no drill is taken, so a short wakeup (a delivery's
/// 30 s `waitUntil`) never holds the signer for a step only a longer one
/// (the cron tick) can finish.
pub fn plan_within(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    holder: &str,
    budget_ms: Option<u64>,
    now_ms: u64,
) -> Result<Option<SignPlan>, SignError> {
    if let Some(status) = claims::status(sql, SIGNER_CLAIM, now_ms)?
        && status.state == "claimed"
        && status.expires_in_ms.is_some()
    {
        journal::bump(sql, "sign_claim_held_skips")?;
        return Ok(None);
    }
    if journal::active_fault(sql)?.is_some() {
        return Ok(None);
    }
    if signer(sql)?.backoff_until_ms.is_some_and(|t| now_ms < t) {
        journal::bump(sql, "sign_backoff_skips")?;
        return Ok(None);
    }
    let Some(confirmed) = journal::confirmed(sql)? else {
        return Ok(None);
    };
    let token = match claims::claim(sql, SIGNER_CLAIM, holder, ctx.cfg.lease_ms, now_ms)? {
        ClaimOutcome::Claimed(token) => token,
        _ => return Ok(None),
    };
    recover_abandoned(sql, &token, now_ms)?;
    let next = next_step(sql, ctx, &token, confirmed, now_ms)?;
    finish_round(sql, ctx, &token, next, budget_ms, now_ms)
}

/// Runs when a new generation takes the claim. A step an earlier holder
/// never reported back is dropped, and a submission mark without an outcome
/// makes that attempt uncertain: its bytes may be in a pool, so they are
/// never sent again and only finalized state resolves them.
fn recover_abandoned(sql: &impl Sql, token: &ClaimToken, now_ms: u64) -> Result<(), SignError> {
    let s = signer(sql)?;
    if s.pending_plan.is_some() && s.pending_generation != Some(token.generation) {
        journal::bump(sql, "sign_abandoned_steps")?;
        set_pending(sql, None)?;
    }
    for r in sql.exec(
        "SELECT op_id, attempt FROM sign_attempts WHERE state = 'sending'",
        &[],
    )? {
        let (op_id, n) = (text(&r, 0)?.to_owned(), uint(&r, 1)? as u32);
        if let Some(a) = attempt(sql, &op_id, n)? {
            set_attempt(
                sql,
                &a,
                "state = 'uncertain', submit_detail = 'abandoned_after_mark'",
                vec![],
            )?;
            journal::bump(sql, "sign_recovered_uncertain")?;
            log(
                sql,
                now_ms,
                None,
                Some(&op_id),
                Some(n),
                "recovered_uncertain",
                Some("mark without outcome"),
            )?;
        }
    }
    Ok(())
}

/// Records `next` as pending and keeps the claim, or releases it. Every
/// step is one RPC call, charged to the shared budget; a step the budget
/// defers is planned again by a later block.
fn finish_round(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    token: &ClaimToken,
    next: Option<SignPlan>,
    budget_ms: Option<u64>,
    now_ms: u64,
) -> Result<Option<SignPlan>, SignError> {
    let Some(mut p) = next else {
        set_pending(sql, None)?;
        claims::release(sql, token, now_ms)?;
        return Ok(None);
    };
    if let Some(budget) = budget_ms {
        // A submission that would take the stall drill costs the stall too.
        let mut worst = p.clone();
        if let SignPlan::Submit { drill, .. } = &mut worst
            && drill_armed(sql, "stall_before_mark")?
        {
            *drill = Some("stall_before_mark".into());
        }
        if step_worst_ms(&worst, crate::liveness::TIMEOUT_MS) > budget {
            journal::bump(sql, "sign_budget_declines")?;
            set_pending(sql, None)?;
            claims::release(sql, token, now_ms)?;
            return Ok(None);
        }
    }
    if !reserve_sign_rpc(sql, ctx.cfg, now_ms)? {
        journal::bump(sql, "sign_budget_deferrals")?;
        set_pending(sql, None)?;
        claims::release(sql, token, now_ms)?;
        return Ok(None);
    }
    if let SignPlan::Submit { drill, .. } = &mut p {
        // One drill per submission step, in this order, so a stalled holder
        // can be followed by an evicted one within one epoch.
        *drill = take_drill(
            sql,
            &[
                "stall_before_mark",
                "never_send",
                "evict_after_send",
                "uncertain_after_send",
            ],
        )?;
        if drill.is_some() {
            log(
                sql,
                now_ms,
                None,
                None,
                None,
                "drill_taken",
                drill.as_deref(),
            )?;
        }
    }
    set_pending(sql, Some(&p))?;
    Ok(Some(p))
}

fn plan_read(
    token: &ClaimToken,
    at: BlockRef,
    purpose: ReadPurpose,
    keys: Vec<Vec<u8>>,
) -> SignPlan {
    SignPlan::Read {
        claim: token.clone(),
        at,
        read: purpose,
        keys: keys
            .iter()
            .map(|k| format!("0x{}", hex::encode(k)))
            .collect(),
    }
}

fn at(sql: &impl Sql, number: u64) -> Result<Option<BlockRef>, StoreError> {
    Ok(journal::finalized_hash(sql, number)?.map(|hash| BlockRef { number, hash }))
}

/// A retained finalized block; one the journal does not hold is a
/// deterministic failure of the step that needs it.
fn retained(sql: &impl Sql, number: u64) -> Result<BlockRef, SignError> {
    at(sql, number)?.ok_or(SignError::Chain(ChainError::new(
        "block_hash_unretained",
        number.to_string(),
    )))
}

/// Runs one candidate's planning; a deterministic failure (for example a
/// storage item live metadata no longer declares) is recorded against its
/// subject and lets the next candidate plan.
fn guarded(
    sql: &impl Sql,
    subject: &str,
    c: BlockRef,
    now_ms: u64,
    f: impl FnOnce() -> Result<Option<SignPlan>, SignError>,
) -> Result<Option<SignPlan>, SignError> {
    match f() {
        Err(SignError::Chain(e)) => {
            record_failure(sql, subject, e.code, c.number, now_ms)?;
            Ok(None)
        }
        other => other,
    }
}

/// Plans the next step at confirmed block `c`, in priority order:
///
/// 1. the head runtime, when none is stored;
/// 2. the live attempt, whose nonce decides everything else;
/// 3. new work (a preflight) while active, when no attempt is live;
/// 4. the row snapshot other validators' rows come from ([`rows`]), once a
///    tempo, in every signer state (paused or faulted too);
/// 5. diagnostics: read-back and reveal tracking of committed operations.
///
/// A subject that is backing off or settled is skipped, so a failing
/// diagnostic never delays a commit.
fn next_step(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    token: &ClaimToken,
    c: BlockRef,
    now_ms: u64,
) -> Result<Option<SignPlan>, SignError> {
    let s = signer(sql)?;
    // A signer fault stops the hotkey pipeline. The row snapshot, which
    // signs nothing with the hotkey, still runs: other validators' rows
    // never wait on the coordinator's own signing ([`rows`]).
    let faulted = s.reason.as_deref().is_some_and(|r| r.starts_with("fault:"));
    if faulted && !rows::snapshot_due(sql, c)? {
        return Ok(None);
    }
    if blocked(sql, HEAD, c.number)? {
        return Ok(None);
    }
    let rt = match ctx.runtime.get(sql) {
        Ok(Some(rt)) => rt,
        Ok(None) => {
            return Ok(Some(SignPlan::RuntimeVersion {
                claim: token.clone(),
                at: c,
                purpose: RuntimeFor::Head,
            }));
        }
        Err(SignError::Chain(e)) => {
            set_runtime(sql, None, None)?;
            record_failure(sql, HEAD, e.code, c.number, now_ms)?;
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let cfg = ctx.cfg;
    if faulted {
        return row_step(sql, ctx, &rt, token, c, now_ms);
    }
    // 2. The live attempt.
    let live = live_attempt(sql)?;
    if let Some(a) = live.clone() {
        let subject = attempt_subject(&a.op_id, a.attempt);
        if !blocked(sql, &subject, c.number)?
            && let Some(p) = guarded(sql, &subject, c, now_ms, || {
                attempt_step(sql, ctx, &rt, token, c, a, now_ms)
            })?
        {
            return Ok(Some(p));
        }
    } else if s.state == "active"
        && !blocked(sql, PREFLIGHT, c.number)?
        && let Some(p) = guarded(sql, PREFLIGHT, c, now_ms, || {
            new_work(sql, ctx, &rt, token, c)
        })?
    {
        // 3. New work only while active, with a target and no live attempt.
        return Ok(Some(p));
    }
    // 4. The row snapshot, in every signer state.
    if let Some(p) = row_step(sql, ctx, &rt, token, c, now_ms)? {
        return Ok(Some(p));
    }
    // 5. Committed operations: read-back, then reveal.
    for o in ops_where(sql, "state = 'committed'")? {
        let Some(commit_block) = o.commit_block else {
            continue;
        };
        if o.readback.is_none() {
            let subject = format!("readback:{}", o.op_id);
            if c.number >= commit_block
                && !blocked(sql, &subject, c.number)?
                && let Some(p) = guarded(sql, &subject, c, now_ms, || {
                    let read = ReadPurpose::Readback {
                        op_id: o.op_id.clone(),
                    };
                    let keys = keys_for(sql, cfg, &rt, &read)?;
                    Ok(Some(plan_read(token, c, read, keys)))
                })?
            {
                return Ok(Some(p));
            }
            continue;
        }
        let subject = format!("reveal:{}", o.op_id);
        if let Some((lo, hi)) = o.reveal_search {
            // A search a round's step limit cut short continues from its
            // stored bounds.
            if !blocked(sql, &subject, c.number)?
                && let Some(p) = guarded(sql, &subject, c, now_ms, || {
                    chained_step(sql, ctx, token, c, search_step(&o.op_id, lo, hi))
                })?
            {
                return Ok(Some(p));
            }
            continue;
        }
        let due = o.next_check_block.or(o
            .predicted_reveal_block
            .map(|p| p.saturating_add(cfg.reveal_slack)));
        if due.is_some_and(|d| c.number >= d)
            && !blocked(sql, &subject, c.number)?
            && let Some(p) = guarded(sql, &subject, c, now_ms, || {
                let read = ReadPurpose::RevealCheck {
                    op_id: o.op_id.clone(),
                };
                let keys = keys_for(sql, cfg, &rt, &read)?;
                Ok(Some(plan_read(token, c, read, keys)))
            })?
        {
            return Ok(Some(p));
        }
    }
    Ok(None)
}

/// The row snapshot's schedule read, when one is due ([`rows::snapshot_due`])
/// and its subject is not backing off.
fn row_step(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    rt: &ChainRuntime,
    token: &ClaimToken,
    c: BlockRef,
    now_ms: u64,
) -> Result<Option<SignPlan>, SignError> {
    if blocked(sql, ROW_SNAPSHOT, c.number)? || !rows::snapshot_due(sql, c)? {
        return Ok(None);
    }
    guarded(sql, ROW_SNAPSHOT, c, now_ms, || {
        let read = ReadPurpose::RowSchedule;
        let keys = keys_for(sql, ctx.cfg, rt, &read)?;
        Ok(Some(plan_read(token, c, read, keys)))
    })
}

/// A preflight, when the confirmed block opens a commit window or an
/// operation is preparing. Preflights back off by [`PREFLIGHT_RETRY`] blocks
/// so a deferral costs little RPC.
fn new_work(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    rt: &ChainRuntime,
    token: &ClaimToken,
    c: BlockRef,
) -> Result<Option<SignPlan>, SignError> {
    let Some(Ok(_)) = active_target(sql, ctx.cfg, ctx.trusted)? else {
        return Ok(None);
    };
    if retry_block(sql)?.is_some_and(|r| c.number < r) {
        return Ok(None);
    }
    if !ops_where(sql, "state = 'preparing'")?.is_empty() || schedule_due(sql, ctx.cfg, c)? {
        let keys = keys_for(sql, ctx.cfg, rt, &ReadPurpose::Preflight)?;
        return Ok(Some(plan_read(token, c, ReadPurpose::Preflight, keys)));
    }
    Ok(None)
}

/// The keys a read asks for under runtime `rt`, recomputed from the
/// journal's own state (never from a reply).
fn keys_for(
    sql: &impl Sql,
    cfg: &SignConfig,
    rt: &ChainRuntime,
    read: &ReadPurpose,
) -> Result<Vec<Vec<u8>>, SignError> {
    let op_of =
        |op_id: &str| -> Result<Op, SignError> { Ok(op(sql, op_id)?.ok_or(StoreError("op row"))?) };
    match read {
        ReadPurpose::Preflight => preflight_keys(rt, cfg, active_mechanism(sql)?),
        ReadPurpose::Registration { op_id } => {
            let o = op_of(op_id)?;
            let population = o.preflight.as_ref().map_or(0, |p| p.population);
            registration_keys(rt, cfg, active_mechanism(sql)?, population, o.epoch)
        }
        ReadPurpose::NonceCheck { .. } | ReadPurpose::NonceProbe { .. } => nonce_keys(rt, cfg),
        ReadPurpose::Events { .. } | ReadPurpose::InclusionEvents { .. } => Ok(vec![
            rt.storage_key("System", "Events", &[])
                .map_err(SignError::Chain)?,
            upgrade_key(rt)?,
        ]),
        ReadPurpose::Readback { op_id } => readback_keys(rt, cfg, &op_of(op_id)?),
        ReadPurpose::RevealCheck { op_id } => reveal_keys(rt, cfg, &op_of(op_id)?),
        ReadPurpose::RevealProbe { op_id, .. } => Ok(vec![
            upgrade_key(rt)?,
            commits_key(rt, cfg, 0, op_of(op_id)?.epoch)?,
        ]),
        ReadPurpose::RowSchedule => rows::ROW_SCHEDULE_ITEMS
            .iter()
            .map(|(p, i, k)| item_key(rt, cfg, 0, p, i, *k))
            .collect(),
        ReadPurpose::RowRegistration { population, .. } => {
            let mut keys = vec![upgrade_key(rt)?];
            keys.extend(neuron_keys(rt, cfg, *population)?);
            keys.push(owner_key(rt, cfg)?);
            Ok(keys)
        }
        ReadPurpose::RevealEvents { op_id, .. } => {
            let o = op_of(op_id)?;
            Ok(vec![
                upgrade_key(rt)?,
                rt.storage_key("System", "Events", &[])
                    .map_err(SignError::Chain)?,
                rt.storage_key(
                    chain::SUBTENSOR,
                    "Weights",
                    &[
                        index_value(cfg, 0)?,
                        Value::Int(i128::from(o.uid.unwrap_or(0))),
                    ],
                )
                .map_err(SignError::Chain)?,
            ])
        }
    }
}

fn active_mechanism(sql: &impl Sql) -> Result<u8, StoreError> {
    Ok(sql
        .exec("SELECT mechanism FROM sign_targets WHERE active = 1", &[])?
        .first()
        .map(|r| uint(r, 0))
        .transpose()?
        .and_then(|m| u8::try_from(m).ok())
        .unwrap_or(0))
}

/// Blocks between preflights that did not lead to a signed attempt.
pub const PREFLIGHT_RETRY: u64 = 10;

fn retry_block(sql: &impl Sql) -> Result<Option<u64>, StoreError> {
    Ok(sql
        .exec("SELECT retry_block FROM chain_schedule WHERE id = 1", &[])?
        .first()
        .and_then(|r| match r.first() {
            Some(SqlValue::Integer(v)) => Some(*v as u64),
            _ => None,
        }))
}

/// Whether the confirmed block opens a commit window no operation holds yet,
/// predicted from the last preflight's schedule. With no schedule, or one
/// older than a tempo, a preflight is due to refresh it.
fn schedule_due(sql: &impl Sql, cfg: &SignConfig, c: BlockRef) -> Result<bool, StoreError> {
    let rows = sql.exec(
        "SELECT block_number, tempo, last_epoch_block, pending_epoch_at, subnet_epoch_index,
                blocks_since_last_step FROM chain_schedule WHERE id = 1",
        &[],
    )?;
    let Some(r) = rows.first() else {
        return Ok(true);
    };
    let (at_block, tempo) = (uint(r, 0)?, uint(r, 1)? as u16);
    if c.number <= at_block {
        return Ok(false);
    }
    if c.number - at_block > u64::from(tempo).max(1) {
        return Ok(true);
    }
    let base = chain::schedule(
        tempo,
        uint(r, 2)?,
        uint(r, 3)?,
        uint(r, 4)?,
        uint(r, 5)?,
        at_block,
    );
    let state = rtao_core::timelock::epoch_schedule::advance_blocks(&base, at_block + 1, c.number);
    let window = commit_window(cfg, &state);
    let epoch = chain::commit_epoch(&state);
    Ok(window && op_for_epoch(sql, epoch)?.is_none())
}

/// Whether a commit prepared at `state.current_block` lands in its epoch with
/// room for the whole mortal era.
fn commit_window(cfg: &SignConfig, state: &EpochScheduleState) -> bool {
    let into = state.current_block.saturating_sub(state.last_epoch_block);
    let Some(next) = chain::next_epoch_block(state, u64::from(state.tempo) + 2) else {
        return false;
    };
    into >= cfg.commit_offset && next - state.current_block > cfg.era_period + cfg.boundary_guard
}

fn op_for_epoch(sql: &impl Sql, epoch: u64) -> Result<Option<String>, StoreError> {
    sql.exec(
        "SELECT op_id FROM commit_ops WHERE epoch = ?",
        &[bind_u64(epoch)?],
    )?
    .first()
    .map(|r| text(r, 0).map(str::to_owned))
    .transpose()
}

fn attempt_step(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    rt: &ChainRuntime,
    token: &ClaimToken,
    c: BlockRef,
    a: Attempt,
    now_ms: u64,
) -> Result<Option<SignPlan>, SignError> {
    let cfg = ctx.cfg;
    match a.state.as_str() {
        "signed" => {
            if c.number >= a.expiry {
                // Never sent, and now unincludable.
                set_attempt(
                    sql,
                    &a,
                    "state = 'expired', resolved_at_ms = ?",
                    vec![bind_u64(now_ms)?],
                )?;
                log(
                    sql,
                    now_ms,
                    Some(c.number),
                    Some(&a.op_id),
                    Some(a.attempt),
                    "attempt_expired_unsent",
                    None,
                )?;
                set_op_state(sql, &a.op_id, "preparing", Some("attempt_expired"), now_ms)?;
                return Ok(None);
            }
            Ok(Some(SignPlan::Submit {
                claim: token.clone(),
                op_id: a.op_id.clone(),
                attempt: a.attempt,
                extrinsic: a.extrinsic_hex.clone(),
                drill: None,
            }))
        }
        // This holder's own submission, whose outcome is in flight; an
        // earlier holder's was made uncertain when the claim changed hands.
        "sending" => Ok(None),
        "submitted" | "uncertain" => {
            if let (Some(lo), Some(hi)) = (a.search_lo, a.search_hi)
                && a.search_scan
            {
                // The body scan: the next block after the last one known not
                // to hold these bytes. A mortal attempt is included at or
                // before its era's expiry, so nothing later is read.
                let next = lo.saturating_add(1);
                if next > hi.min(a.expiry) {
                    // No block left where these bytes could be.
                    displaced(sql, &a, now_ms)?;
                    return Ok(None);
                }
                return Ok(Some(SignPlan::Block {
                    claim: token.clone(),
                    at: retained(sql, next)?,
                    op_id: a.op_id,
                    attempt: a.attempt,
                    extrinsic: a.extrinsic_hex,
                }));
            }
            if let (Some(lo), Some(hi)) = (a.search_lo, a.search_hi) {
                if hi <= lo.saturating_add(1) && !a.events_checked {
                    // D5: the inclusion block's events first; its body only
                    // when they hold no commit of this hotkey.
                    let read = ReadPurpose::InclusionEvents { op_id: a.op_id, attempt: a.attempt };
                    let keys = keys_for(sql, cfg, rt, &read)?;
                    return Ok(Some(plan_read(token, retained(sql, hi)?, read, keys)));
                }
                if hi <= lo.saturating_add(1) {
                    return Ok(Some(SignPlan::Block {
                        claim: token.clone(),
                        at: retained(sql, hi)?,
                        op_id: a.op_id,
                        attempt: a.attempt,
                        extrinsic: a.extrinsic_hex,
                    }));
                }
                let mid = lo + (hi - lo) / 2;
                let read = ReadPurpose::NonceProbe {
                    op_id: a.op_id,
                    attempt: a.attempt,
                    mid,
                };
                let keys = keys_for(sql, cfg, rt, &read)?;
                return Ok(Some(plan_read(token, retained(sql, mid)?, read, keys)));
            }
            if a.checked_block.is_some_and(|b| b >= c.number) || c.number <= a.floor_block {
                return Ok(None);
            }
            let read = ReadPurpose::NonceCheck {
                op_id: a.op_id,
                attempt: a.attempt,
            };
            let keys = keys_for(sql, cfg, rt, &read)?;
            Ok(Some(plan_read(token, c, read, keys)))
        }
        "included" => {
            let Some(n) = a.included_block else {
                return Ok(None);
            };
            let read = ReadPurpose::Events {
                op_id: a.op_id,
                attempt: a.attempt,
            };
            let keys = keys_for(sql, cfg, rt, &read)?;
            Ok(Some(plan_read(token, retained(sql, n)?, read, keys)))
        }
        _ => Ok(None),
    }
}

// ------------------------------------------------------------------ keys

fn upgrade_key(rt: &ChainRuntime) -> Result<Vec<u8>, SignError> {
    rt.storage_key("System", "LastRuntimeUpgrade", &[])
        .map_err(SignError::Chain)
}

fn net(cfg: &SignConfig) -> Value {
    Value::Int(i128::from(cfg.netuid))
}

fn hotkey_value(cfg: &SignConfig) -> Value {
    Value::Bytes(cfg.hotkey.to_vec())
}

fn index_value(cfg: &SignConfig, mechanism: u8) -> Result<Value, SignError> {
    storage_index(cfg.netuid, mechanism)
        .map(|i| Value::Int(i128::from(i)))
        .ok_or(SignError::Chain(ChainError::new(
            "mechanism_index",
            "overflow",
        )))
}

const PREFLIGHT_ITEMS: &[(&str, &str, u8)] = &[
    // (pallet, item, key kind): 0 none, 1 netuid, 2 netuid+hotkey, 3 hotkey, 4 storage index
    ("System", "LastRuntimeUpgrade", 0),
    ("System", "Account", 3),
    ("Timestamp", "Now", 0),
    (chain::SUBTENSOR, "NetworksAdded", 1),
    (chain::SUBTENSOR, "Uids", 2),
    (chain::SUBTENSOR, "CommitRevealWeightsEnabled", 1),
    (chain::SUBTENSOR, "CommitRevealWeightsVersion", 0),
    (chain::SUBTENSOR, "RevealPeriodEpochs", 1),
    (chain::SUBTENSOR, "WeightsVersionKey", 1),
    (chain::SUBTENSOR, "WeightsSetRateLimit", 1),
    (chain::SUBTENSOR, "LastUpdate", 4),
    (chain::SUBTENSOR, "Tempo", 1),
    (chain::SUBTENSOR, "LastEpochBlock", 1),
    (chain::SUBTENSOR, "PendingEpochAt", 1),
    (chain::SUBTENSOR, "SubnetEpochIndex", 1),
    (chain::SUBTENSOR, "BlocksSinceLastStep", 1),
    (chain::SUBTENSOR, "SubnetworkN", 1),
    (chain::SUBTENSOR, "MinAllowedWeights", 1),
    (chain::SUBTENSOR, "MaxWeightsLimit", 1),
];

/// Items whose absence from live metadata only drops an advisory check.
const OPTIONAL_ITEMS: &[&str] = &["MinAllowedWeights", "MaxWeightsLimit"];

fn item_key(
    rt: &ChainRuntime,
    cfg: &SignConfig,
    mechanism: u8,
    pallet: &str,
    item: &str,
    kind: u8,
) -> Result<Vec<u8>, SignError> {
    let params = match kind {
        0 => vec![],
        1 => vec![net(cfg)],
        2 => vec![net(cfg), hotkey_value(cfg)],
        3 => vec![hotkey_value(cfg)],
        _ => vec![index_value(cfg, mechanism)?],
    };
    rt.storage_key(pallet, item, &params)
        .map_err(SignError::Chain)
}

fn preflight_keys(
    rt: &ChainRuntime,
    cfg: &SignConfig,
    mechanism: u8,
) -> Result<Vec<Vec<u8>>, SignError> {
    PREFLIGHT_ITEMS
        .iter()
        .filter(|(p, i, _)| !OPTIONAL_ITEMS.contains(i) || rt.has_storage(p, i))
        .map(|(p, i, k)| item_key(rt, cfg, mechanism, p, i, *k))
        .collect()
}

fn nonce_keys(rt: &ChainRuntime, cfg: &SignConfig) -> Result<Vec<Vec<u8>>, SignError> {
    Ok(vec![
        item_key(rt, cfg, 0, "System", "Account", 3)?,
        upgrade_key(rt)?,
    ])
}

fn registration_keys(
    rt: &ChainRuntime,
    cfg: &SignConfig,
    mechanism: u8,
    population: u16,
    epoch: u64,
) -> Result<Vec<Vec<u8>>, SignError> {
    let mut keys = vec![upgrade_key(rt)?];
    keys.extend(neuron_keys(rt, cfg, population)?);
    keys.push(owner_key(rt, cfg)?);
    keys.push(commits_key(rt, cfg, mechanism, epoch)?);
    Ok(keys)
}

/// `Keys` and `BlockAtRegistration` of every UID below `population`.
fn neuron_keys(rt: &ChainRuntime, cfg: &SignConfig, population: u16) -> Result<Vec<Vec<u8>>, SignError> {
    let mut keys = Vec::with_capacity(usize::from(population) * 2);
    for uid in 0..population {
        let p = [net(cfg), Value::Int(i128::from(uid))];
        keys.push(rt.storage_key(chain::SUBTENSOR, "Keys", &p).map_err(SignError::Chain)?);
        keys.push(rt.storage_key(chain::SUBTENSOR, "BlockAtRegistration", &p).map_err(SignError::Chain)?);
    }
    Ok(keys)
}

/// `(uid, hotkey, registration block)` of each registered UID.
type Neurons = Vec<(u16, [u8; 32], u64)>;

/// The registrations `neuron_keys` read, or `None` when a UID's hotkey does
/// not decode.
fn read_neurons(
    rt: &ChainRuntime,
    cfg: &SignConfig,
    population: u16,
    values: &Values,
) -> Result<Option<Neurons>, SignError> {
    let mut neurons = Vec::with_capacity(usize::from(population));
    for uid in 0..population {
        let params = [net(cfg), Value::Int(i128::from(uid))];
        let k = rt.storage_key(chain::SUBTENSOR, "Keys", &params).map_err(SignError::Chain)?;
        let b = rt.storage_key(chain::SUBTENSOR, "BlockAtRegistration", &params).map_err(SignError::Chain)?;
        let Some(hotkey) = as_account(&values.get(rt, chain::SUBTENSOR, "Keys", &k)?) else {
            return Ok(None);
        };
        neurons.push((
            uid,
            hotkey,
            number(&values.get(rt, chain::SUBTENSOR, "BlockAtRegistration", &b)?, "BlockAtRegistration")?,
        ));
    }
    Ok(Some(neurons))
}

/// The paid row of reward-v2 target `v` at `snapshot`, with the hotkey and
/// registration block at each paid UID and the owner hotkey (what the
/// signed row other validators copy carries, [`rows`]), or the refusal code.
///
/// The row is derived from the target's public `QuantizedResult` alone (no
/// exact score enters it) and is signed for other validators ([`rows`]) as
/// soon as it is journaled.
fn paid_summary_v2(
    v: &slop_ninja_protocol::VerifiedTarget,
    snapshot: &slop_ninja_protocol::RegistrationSnapshotV2,
) -> Result<RowSummary, &'static str> {
    match v.derive_row(snapshot) {
        Ok(row) => match target::summary_v2(&row) {
            Ok(mut s) => {
                let at = |uid: u16| snapshot.neurons.iter().find(|n| n.uid == uid);
                s.hotkeys = s.entries.iter().map(|(uid, _)| at(*uid).map(|n| hex::encode(n.hotkey.0))).collect();
                s.registration_blocks = s.entries.iter().map(|(uid, _)| at(*uid).map(|n| n.registration_block)).collect();
                s.owner_hotkey = Some(hex::encode(snapshot.owner_hotkey.0));
                Ok(s)
            }
            Err(r) => Err(r.code),
        },
        // The owner hotkey holds no UID and units must burn: this tempo's
        // row is deferred, never sent to another UID.
        Err(slop_ninja_protocol::Error::Deferred(_)) => Err(ROW_DEFERRED_OWNER),
        Err(_) => Err("row_refused"),
    }
}

/// A row deferred because the owner hotkey holds no UID.
pub const ROW_DEFERRED_OWNER: &str = "row_deferred_owner_unregistered";

/// `SubnetOwnerHotkey(netuid)`: the burn destination of a reward-v1 row.
fn owner_key(rt: &ChainRuntime, cfg: &SignConfig) -> Result<Vec<u8>, SignError> {
    rt.storage_key(chain::SUBTENSOR, "SubnetOwnerHotkey", &[net(cfg)])
        .map_err(SignError::Chain)
}

fn commits_key(
    rt: &ChainRuntime,
    cfg: &SignConfig,
    mechanism: u8,
    epoch: u64,
) -> Result<Vec<u8>, SignError> {
    rt.storage_key(
        chain::SUBTENSOR,
        "TimelockedWeightCommits",
        &[index_value(cfg, mechanism)?, Value::Int(i128::from(epoch))],
    )
    .map_err(SignError::Chain)
}

fn readback_keys(rt: &ChainRuntime, cfg: &SignConfig, o: &Op) -> Result<Vec<Vec<u8>>, SignError> {
    Ok(vec![
        upgrade_key(rt)?,
        commits_key(rt, cfg, 0, o.epoch)?,
        commits_key(rt, cfg, 0, o.epoch + 1)?,
    ])
}

fn reveal_keys(rt: &ChainRuntime, cfg: &SignConfig, o: &Op) -> Result<Vec<Vec<u8>>, SignError> {
    let uid = Value::Int(i128::from(o.uid.unwrap_or(0)));
    Ok(vec![
        upgrade_key(rt)?,
        commits_key(rt, cfg, 0, o.epoch)?,
        item_key(rt, cfg, 0, chain::SUBTENSOR, "LastUpdate", 4)?,
        rt.storage_key(chain::SUBTENSOR, "Weights", &[index_value(cfg, 0)?, uid])
            .map_err(SignError::Chain)?,
        item_key(rt, cfg, 0, chain::SUBTENSOR, "SubnetEpochIndex", 1)?,
    ])
}

// ---------------------------------------------------------------- results

/// The Worker's pre-submission mark: `signed` → `sending`, once, under the
/// current claim. A refused mark means the Worker must not send.
pub fn mark(
    sql: &impl Sql,
    token: &ClaimToken,
    op_id: &str,
    n: u32,
    now_ms: u64,
) -> Result<bool, SignError> {
    if !claims::is_current(sql, token, now_ms)? || !pending_is(sql, token)? {
        journal::bump(sql, "sign_fenced_marks")?;
        log(sql, now_ms, None, Some(op_id), Some(n), "mark_fenced", None)?;
        return Ok(false);
    }
    let Some(a) = attempt(sql, op_id, n)? else {
        return Ok(false);
    };
    if a.state != "signed" {
        journal::bump(sql, "sign_refused_marks")?;
        return Ok(false);
    }
    set_attempt(
        sql,
        &a,
        "state = 'sending', sends = sends + 1, sent_at_ms = ?",
        vec![bind_u64(now_ms)?],
    )?;
    journal::bump(sql, "sign_submissions")?;
    log(
        sql,
        now_ms,
        None,
        Some(op_id),
        Some(n),
        "marked_sending",
        None,
    )?;
    Ok(true)
}

fn pending_is(sql: &impl Sql, token: &ClaimToken) -> Result<bool, StoreError> {
    let s = signer(sql)?;
    Ok(s.pending_generation == Some(token.generation) && s.pending_plan.is_some())
}

/// Processes one outcome for the pending step and plans the next.
///
/// A deterministic failure while applying it is recorded against the step's
/// subject (never rolled back into a retry loop), and an RPC error ends the
/// round under a time backoff instead of re-planning at once.
pub fn on_result(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    token: &ClaimToken,
    outcome: &SignOutcome,
    now_ms: u64,
) -> Result<SignStep, SignError> {
    on_result_more(sql, ctx, token, outcome, true, now_ms)
}

/// [`on_result`] for a round that may (`more`) or may not run another step.
/// The last step of a round plans nothing and releases the claim, so the
/// next wakeup continues at once instead of waiting out the lease.
pub fn on_result_more(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    token: &ClaimToken,
    outcome: &SignOutcome,
    more: bool,
    now_ms: u64,
) -> Result<SignStep, SignError> {
    on_result_within(sql, ctx, token, outcome, more, None, now_ms)
}

/// [`on_result_more`] for a caller with `budget_ms` left for the next step
/// (`None`: no bound): a next step that does not fit is not planned and the
/// claim is released ([`plan_within`]).
pub fn on_result_within(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    token: &ClaimToken,
    outcome: &SignOutcome,
    more: bool,
    budget_ms: Option<u64>,
    now_ms: u64,
) -> Result<SignStep, SignError> {
    if !claims::is_current(sql, token, now_ms)? || !pending_is(sql, token)? {
        journal::bump(sql, "sign_fenced_results")?;
        return Ok(SignStep {
            fenced: true,
            plan: None,
        });
    }
    let s = signer(sql)?;
    let pending: SignPlan = serde_json::from_str(s.pending_plan.as_deref().unwrap_or_default())
        .map_err(|_| StoreError("pending plan"))?;
    set_pending(sql, None)?;
    let rpc_error = rpc_error(outcome);
    if rpc_error.is_none() && !matches!(outcome, SignOutcome::Submit { .. }) {
        rpc_answered(sql)?;
    }
    let confirmed = journal::confirmed(sql)?;
    let subject = plan_subject(&pending);
    // A JSON-RPC error answer to a historical read of the live attempt (a
    // nonce probe of its inclusion search, a block body, its events): the
    // endpoint does not serve that block's state (pruned after a long
    // outage), or refuses it for a while (overload, an internal error).
    // Retried behind the signer-wide backoff, it would hold the live
    // attempt, and with it every new commit, forever and unseen. It counts
    // against the attempt instead (a failing subject in `/status`), and a
    // nonce probe's search goes on by block bodies, which need no state. A
    // later answer clears it. Only refusals spanning
    // `UNAVAILABLE_SETTLE_FAILURES` failures of block backoff settle it,
    // without pausing the signer: an included attempt's events as
    // `events_unverifiable`, a consumed nonce's inclusion as
    // `inclusion_unverifiable`.
    let historical = rpc_error == Some(RpcError::Remote)
        && subject.as_deref().is_some_and(|s| s.starts_with("attempt:"))
        && matches!(
            &pending,
            SignPlan::Block { .. }
                | SignPlan::Read { read: ReadPurpose::NonceProbe { .. } | ReadPurpose::Events { .. }, .. }
        );
    // A reply too large for its read: the same pinned read will answer the
    // same bytes, so it counts against its subject ([`REPLY_TOO_LARGE`]),
    // never as a signer-wide backoff that would hold every other subject.
    let too_large = rpc_error == Some(RpcError::TooLarge) && subject.is_some();
    // D5: an inclusion block whose events cannot be read is decided by its
    // body; one whose events held no commit of this hotkey and whose body
    // cannot be read either settles as `consumed_unverified`.
    let unverifiable = match (&pending, rpc_error) {
        (SignPlan::Read { read: ReadPurpose::InclusionEvents { op_id, attempt: n }, .. }, Some(RpcError::TooLarge | RpcError::Remote)) => {
            if let Some(a) = attempt(sql, op_id, *n)? {
                set_attempt(sql, &a, "events_checked = 1", vec![])?;
                log(sql, now_ms, a.search_hi, Some(op_id), Some(*n), "inclusion_events_unreadable", None)?;
            }
            true
        }
        // A body too large to read never answers differently; a refused
        // historical read keeps its own retry and settle
        // (`inclusion_unverifiable`).
        (SignPlan::Block { op_id, attempt: n, .. }, Some(RpcError::TooLarge)) => {
            match attempt(sql, op_id, *n)? {
                Some(a) if a.events_checked && !a.search_scan && matches!(a.state.as_str(), "submitted" | "uncertain") => {
                    consumed_unverified(sql, &a, now_ms)?;
                    true
                }
                _ => false,
            }
        }
        _ => false,
    };
    let chained = if unverifiable {
        if let Some(subject) = &subject {
            clear_failure(sql, subject)?;
        }
        None
    } else if historical {
        attempt_state_unavailable(sql, &pending, now_ms)?;
        let at = confirmed.map_or(0, |c| c.number);
        if let Some(subject) = &subject {
            record_failure(sql, subject, STATE_UNAVAILABLE, at, now_ms)?;
        }
        None
    } else if too_large {
        journal::bump(sql, "sign_replies_too_large")?;
        let at = confirmed.map_or(0, |c| c.number);
        if let Some(subject) = &subject {
            record_failure(sql, subject, REPLY_TOO_LARGE, at, now_ms)?;
        }
        None
    } else {
        match apply(sql, ctx, &pending, outcome, now_ms) {
            Ok(chained) => {
                // A subject's failures clear only when its work completes, not
                // at a handover to the next step of the same work.
                if let Some(subject) = &subject
                    && rpc_error.is_none()
                    && chained.is_none()
                {
                    clear_failure(sql, subject)?;
                }
                chained
            }
            Err(SignError::Chain(e)) => {
                let at = confirmed.map_or(0, |c| c.number);
                match &subject {
                    Some(subject) => record_failure(sql, subject, e.code, at, now_ms)?,
                    None => note_error(sql, &format!("submit: {}", e.code), now_ms)?,
                }
                None
            }
            Err(e) => return Err(e),
        }
    };
    let next = match confirmed {
        _ if rpc_error.is_some() || !more => None,
        Some(c) if claims::renew(sql, token, ctx.cfg.lease_ms, now_ms)? => {
            let chained_plan = match chained {
                Some(ch) => {
                    let subject = chained_subject(&ch);
                    guarded(sql, &subject, c, now_ms, || {
                        chained_step(sql, ctx, token, c, ch)
                    })?
                }
                None => None,
            };
            match chained_plan {
                Some(p) => Some(p),
                None => next_step(sql, ctx, token, c, now_ms)?,
            }
        }
        _ => None,
    };
    let plan = finish_round(sql, ctx, token, next, budget_ms, now_ms)?;
    Ok(SignStep {
        fenced: false,
        plan,
    })
}

/// A historical read of the live attempt answered with a JSON-RPC error
/// (`on_result_within`): a nonce probe's binary search switches to the body
/// scan, which reads each block after the last one known not to hold the
/// attempt's bytes, up to the block that consumed its nonce or its era's
/// expiry, and needs no state at those blocks.
fn attempt_state_unavailable(sql: &impl Sql, pending: &SignPlan, now_ms: u64) -> Result<(), StoreError> {
    let SignPlan::Read { read: ReadPurpose::NonceProbe { op_id, attempt: n, mid }, .. } = pending else {
        return Ok(());
    };
    let Some(a) = attempt(sql, op_id, *n)? else {
        return Ok(());
    };
    if a.search_scan {
        return Ok(());
    }
    set_attempt(sql, &a, "search_scan = 1", vec![])?;
    journal::bump(sql, "sign_search_scans")?;
    log(sql, now_ms, Some(*mid), Some(op_id), Some(*n), "search_scan", Some("nonce state unavailable"))
}

fn rpc_error(outcome: &SignOutcome) -> Option<RpcError> {
    match outcome {
        SignOutcome::RuntimeVersion { result: Err(e) }
        | SignOutcome::Metadata { result: Err(e) }
        | SignOutcome::Read { result: Err(e) }
        | SignOutcome::Block { result: Err(e) } => Some(*e),
        _ => None,
    }
}

fn chained_subject(ch: &Chained) -> String {
    match ch {
        Chained::Metadata { purpose, .. } => match purpose {
            RuntimeFor::Head => HEAD.to_owned(),
            RuntimeFor::Writer { read, .. } => read_subject(read),
            RuntimeFor::Sign { .. } => PREFLIGHT.to_owned(),
        },
        Chained::LearnWriter { read, .. } | Chained::Read { read, .. } => read_subject(read),
        Chained::SignRuntime { .. } | Chained::Registration { .. } => PREFLIGHT.to_owned(),
        Chained::RevealProbe { op_id, .. } | Chained::RevealEvents { op_id, .. } => {
            format!("reveal:{op_id}")
        }
        Chained::RowRegistration { .. } => ROW_SNAPSHOT.to_owned(),
    }
}

/// Counts one runtime re-learn for a read; past [`RELEARN_CAP`] the read
/// fails instead of learning again.
fn relearn(sql: &impl Sql) -> Result<(), SignError> {
    let n = signer(sql)?.relearns.saturating_add(1);
    if n > RELEARN_CAP {
        sql.exec("UPDATE signer SET relearns = 0 WHERE id = 1", &[])?;
        return Err(SignError::Chain(ChainError::new("runtime_relearn_cap", "")));
    }
    sql.exec(
        "UPDATE signer SET relearns = ? WHERE id = 1",
        &[bind_u64(n)?],
    )?;
    journal::bump(sql, "sign_runtime_changes")?;
    Ok(())
}

fn runtime_stored(sql: &impl Sql, spec: u32, tx: u32) -> Result<bool, StoreError> {
    Ok(!sql
        .exec(
            "SELECT 1 FROM runtime_cache WHERE spec_version = ? AND tx_version = ?",
            &[SqlValue::Integer(spec.into()), SqlValue::Integer(tx.into())],
        )?
        .is_empty())
}

/// What a newly known runtime is for, once it is stored.
fn runtime_known(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    at: BlockRef,
    versions: (u32, u32),
    purpose: &RuntimeFor,
    now_ms: u64,
) -> Result<Option<Chained>, SignError> {
    advance_head(sql, versions, at.number)?;
    match purpose {
        RuntimeFor::Head => Ok(None),
        RuntimeFor::Writer { block, spec, read } => Ok(Some(Chained::Read {
            at: *block,
            spec: *spec,
            read: read.clone(),
        })),
        RuntimeFor::Sign { op_id } => {
            let Some(rt) = ctx.runtime.load(sql, versions.0, versions.1)? else {
                return Ok(None);
            };
            sign_op(sql, ctx, &rt, op_id, at, now_ms)?;
            Ok(None)
        }
    }
}

fn apply(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    pending: &SignPlan,
    outcome: &SignOutcome,
    now_ms: u64,
) -> Result<Option<Chained>, SignError> {
    match (pending, outcome) {
        (SignPlan::RuntimeVersion { at, purpose, .. }, SignOutcome::RuntimeVersion { result }) => {
            match result {
                Err(e) => Ok(rpc_failed(sql, *e, now_ms).map(|_| None)?),
                Ok((spec, tx)) => {
                    if let RuntimeFor::Writer { spec: wanted, .. } = purpose
                        && spec != wanted
                    {
                        // The parent's runtime is not the one the block names.
                        return Err(SignError::Chain(ChainError::new(
                            "writer_runtime_mismatch",
                            format!("{spec} at parent, {wanted} named"),
                        )));
                    }
                    if runtime_stored(sql, *spec, *tx)? {
                        return runtime_known(sql, ctx, *at, (*spec, *tx), purpose, now_ms);
                    }
                    log(
                        sql,
                        now_ms,
                        Some(at.number),
                        None,
                        None,
                        "runtime_seen",
                        Some(&format!("{spec}/{tx}")),
                    )?;
                    Ok(Some(Chained::Metadata {
                        at: *at,
                        spec: *spec,
                        tx: *tx,
                        purpose: purpose.clone(),
                    }))
                }
            }
        }
        (
            SignPlan::Metadata {
                at,
                spec_version,
                tx_version,
                purpose,
                ..
            },
            SignOutcome::Metadata { result },
        ) => match result {
            Err(e) => Ok(rpc_failed(sql, *e, now_ms).map(|_| None)?),
            Ok(hex_scale) => {
                let scale = hex::decode(hex_scale.trim_start_matches("0x"))
                    .map_err(|_| SignError::Chain(ChainError::new("metadata_malformed", "hex")))?;
                let prefixed = chain::opaque_metadata(&scale).map_err(SignError::Chain)?;
                // Parse before storing: an unparsable runtime is never trusted.
                let rt = ChainRuntime::parse(&prefixed, *spec_version, *tx_version)
                    .map_err(SignError::Chain)?;
                sql.exec(
                    "DELETE FROM runtime_cache WHERE fetched_at_ms < ?",
                    &[bind_u64(now_ms.saturating_sub(7 * 86_400_000))?],
                )?;
                sql.exec(
                    "INSERT INTO runtime_cache (spec_version, tx_version, metadata_sha256, metadata_hex, block_number,
                        block_hash, fetched_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(spec_version, tx_version) DO UPDATE SET metadata_sha256 = excluded.metadata_sha256,
                        metadata_hex = excluded.metadata_hex, block_number = excluded.block_number,
                        block_hash = excluded.block_hash, fetched_at_ms = excluded.fetched_at_ms",
                    &[
                        SqlValue::Integer((*spec_version).into()),
                        SqlValue::Integer((*tx_version).into()),
                        hex::encode(rt.metadata_sha256()).into(),
                        hex::encode(&prefixed).into(),
                        bind_u64(at.number)?,
                        at.hash.to_hex().into(),
                        bind_u64(now_ms)?,
                    ],
                )?;
                log(
                    sql,
                    now_ms,
                    Some(at.number),
                    None,
                    None,
                    "metadata_stored",
                    Some(&format!(
                        "{spec_version}/{tx_version} {}",
                        hex::encode(rt.metadata_sha256())
                    )),
                )?;
                ctx.runtime.forget();
                runtime_known(sql, ctx, *at, (*spec_version, *tx_version), purpose, now_ms)
            }
        },
        (SignPlan::Read { at, read, keys, .. }, SignOutcome::Read { result }) => match result {
            Err(e) => Ok(rpc_failed(sql, *e, now_ms).map(|_| None)?),
            Ok(pairs) => {
                let Some(head) = ctx.runtime.get(sql)? else {
                    return Ok(None);
                };
                let Some(values) = Values::new(keys, pairs) else {
                    journal::bump(sql, "sign_malformed_reads")?;
                    return Err(SignError::Chain(ChainError::new("read_malformed", "")));
                };
                // Decode with the runtime that wrote this block, which its own
                // `LastRuntimeUpgrade` names.
                let head_key: RuntimeKey = (head.spec_version(), head.tx_version());
                let rt = match values.writer_spec(&head)? {
                    Some(spec) if spec != head_key.0 => match ctx.runtime.for_spec(sql, spec)? {
                        Some(rt) => rt,
                        None => {
                            relearn(sql)?;
                            log(
                                sql,
                                now_ms,
                                Some(at.number),
                                None,
                                None,
                                "runtime_needed",
                                Some(&format!("{spec} wrote this block")),
                            )?;
                            return Ok(Some(Chained::LearnWriter {
                                block: *at,
                                spec,
                                read: read.clone(),
                            }));
                        }
                    },
                    _ => head,
                };
                if !values.planned_all(&keys_for(sql, ctx.cfg, &rt, read)?) {
                    // Keys planned under another runtime: ask again with this
                    // block's own.
                    relearn(sql)?;
                    return Ok(Some(Chained::Read {
                        at: *at,
                        spec: rt.spec_version(),
                        read: read.clone(),
                    }));
                }
                sql.exec("UPDATE signer SET relearns = 0 WHERE id = 1", &[])?;
                apply_read(sql, ctx, &rt, *at, read, &values, now_ms)
            }
        },
        (
            SignPlan::Block {
                at,
                op_id,
                attempt: n,
                extrinsic,
                ..
            },
            SignOutcome::Block { result },
        ) => match result {
            Err(e) => Ok(rpc_failed(sql, *e, now_ms).map(|_| None)?),
            Ok(m) => apply_block(sql, *at, op_id, *n, extrinsic, *m, now_ms).map(|_| None),
        },
        (
            SignPlan::Submit {
                op_id, attempt: n, ..
            },
            SignOutcome::Submit { result },
        ) => apply_submit(sql, op_id, *n, result, now_ms).map(|_| None),
        _ => {
            journal::bump(sql, "sign_mismatched_outcomes")?;
            Ok(None)
        }
    }
}

/// Read values keyed by the hex keys the journal planned.
struct Values {
    map: BTreeMap<String, Option<Vec<u8>>>,
}

impl Values {
    fn new(keys: &[String], pairs: &[(String, Option<String>)]) -> Option<Self> {
        let mut map = BTreeMap::new();
        for (k, v) in pairs {
            let value = match v {
                Some(h) => Some(hex::decode(h.trim_start_matches("0x")).ok()?),
                None => None,
            };
            map.insert(k.to_ascii_lowercase(), value);
        }
        // Every planned key must be answered; absent values are `null`.
        for k in keys {
            map.entry(k.to_ascii_lowercase()).or_insert(None);
        }
        Some(Self { map })
    }

    /// A planned key's raw value (`None` when absent). A key the journal did
    /// not plan is refused rather than read as absent, so a key a runtime
    /// derives differently can never decode as a default.
    fn raw(&self, key: &[u8]) -> Result<Option<&[u8]>, SignError> {
        match self.map.get(&format!("0x{}", hex::encode(key))) {
            Some(v) => Ok(v.as_deref()),
            None => Err(SignError::Chain(ChainError::new("key_not_planned", ""))),
        }
    }

    fn get(
        &self,
        rt: &ChainRuntime,
        pallet: &str,
        item: &str,
        key: &[u8],
    ) -> Result<Value, SignError> {
        rt.decode_storage(pallet, item, self.raw(key)?)
            .map_err(SignError::Chain)
    }

    /// Whether every key in `keys` was planned (and so answered).
    fn planned_all(&self, keys: &[Vec<u8>]) -> bool {
        keys.iter()
            .all(|k| self.map.contains_key(&format!("0x{}", hex::encode(k))))
    }

    /// The spec of the runtime that wrote the block read, from its own
    /// `LastRuntimeUpgrade` (decoded with `head`; the item's layout is fixed).
    /// `None` when the read did not include it or the chain has none.
    fn writer_spec(&self, head: &ChainRuntime) -> Result<Option<u32>, SignError> {
        let key = upgrade_key(head)?;
        let Some(raw) = self.map.get(&format!("0x{}", hex::encode(&key))) else {
            return Ok(None);
        };
        let value = head
            .decode_storage("System", "LastRuntimeUpgrade", raw.as_deref())
            .map_err(SignError::Chain)?;
        Ok(field(&value, "spec_version")
            .and_then(as_u64)
            .and_then(|s| u32::try_from(s).ok()))
    }
}

fn number(v: &Value, what: &'static str) -> Result<u64, SignError> {
    as_u64(v).ok_or(SignError::Chain(ChainError::new("storage_shape", what)))
}

fn apply_read(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    rt: &ChainRuntime,
    at_block: BlockRef,
    purpose: &ReadPurpose,
    values: &Values,
    now_ms: u64,
) -> Result<Option<Chained>, SignError> {
    let cfg = ctx.cfg;
    match purpose {
        ReadPurpose::Preflight => apply_preflight(sql, ctx, rt, at_block, values, now_ms),
        ReadPurpose::Registration { op_id } => {
            apply_registration(sql, ctx, rt, at_block, op_id, values, now_ms)
        }
        ReadPurpose::NonceCheck { op_id, attempt: n } => {
            let Some(a) = attempt(sql, op_id, *n)? else {
                return Ok(None);
            };
            let nonce = account_nonce(rt, cfg, values)?;
            if nonce > a.nonce {
                log(
                    sql,
                    now_ms,
                    Some(at_block.number),
                    Some(op_id),
                    Some(*n),
                    "nonce_consumed",
                    None,
                )?;
                set_attempt(
                    sql,
                    &a,
                    "search_lo = ?, search_hi = ?, checked_block = ?",
                    vec![
                        bind_u64(a.floor_block)?,
                        bind_u64(at_block.number)?,
                        bind_u64(at_block.number)?,
                    ],
                )?;
            } else if nonce < a.nonce {
                fault(sql, "nonce_regressed", Some(op_id), Some(*n), now_ms)?;
            } else if at_block.number >= a.expiry {
                set_attempt(
                    sql,
                    &a,
                    "state = 'expired', checked_block = ?, resolved_at_ms = ?",
                    vec![bind_u64(at_block.number)?, bind_u64(now_ms)?],
                )?;
                journal::bump(sql, "sign_attempts_expired")?;
                log(
                    sql,
                    now_ms,
                    Some(at_block.number),
                    Some(op_id),
                    Some(*n),
                    "attempt_expired",
                    None,
                )?;
                set_op_state(sql, op_id, "preparing", Some("attempt_expired"), now_ms)?;
            } else {
                set_attempt(
                    sql,
                    &a,
                    "checked_block = ?",
                    vec![bind_u64(at_block.number)?],
                )?;
            }
            Ok(None)
        }
        ReadPurpose::NonceProbe {
            op_id,
            attempt: n,
            mid,
        } => {
            let Some(a) = attempt(sql, op_id, *n)? else {
                return Ok(None);
            };
            let nonce = account_nonce(rt, cfg, values)?;
            if nonce > a.nonce {
                set_attempt(sql, &a, "search_hi = ?", vec![bind_u64(*mid)?])?;
            } else {
                set_attempt(sql, &a, "search_lo = ?", vec![bind_u64(*mid)?])?;
            }
            Ok(None)
        }
        ReadPurpose::Events { op_id, attempt: n } => {
            apply_events(sql, ctx, rt, at_block, op_id, *n, values, now_ms).map(|_| None)
        }
        ReadPurpose::InclusionEvents { op_id, attempt: n } => {
            apply_inclusion_events(sql, ctx, rt, at_block, op_id, *n, values, now_ms).map(|_| None)
        }
        ReadPurpose::Readback { op_id } => {
            apply_readback(sql, ctx, rt, at_block, op_id, values, now_ms).map(|_| None)
        }
        ReadPurpose::RevealCheck { op_id } => {
            apply_reveal_check(sql, ctx, rt, at_block, op_id, values, now_ms)
        }
        ReadPurpose::RevealProbe { op_id, lo, hi, mid } => {
            apply_reveal_probe(sql, ctx, rt, op_id, (*lo, *hi, *mid), values, now_ms)
        }
        ReadPurpose::RevealEvents { op_id, block } => {
            apply_reveal_events(sql, ctx, rt, at_block, op_id, *block, values, now_ms)
        }
        ReadPurpose::RowSchedule => rows::apply_schedule(sql, ctx, rt, at_block, values, now_ms),
        ReadPurpose::RowRegistration { epoch, population } => {
            rows::apply_registration(sql, ctx, rt, at_block, *epoch, *population, values, now_ms)
        }
    }
}

fn account_nonce(rt: &ChainRuntime, cfg: &SignConfig, values: &Values) -> Result<u64, SignError> {
    let key = item_key(rt, cfg, 0, "System", "Account", 3)?;
    let info = values.get(rt, "System", "Account", &key)?;
    number(field(&info, "nonce").unwrap_or(&Value::Null), "nonce")
}

fn save_schedule(sql: &impl Sql, p: &Preflight, now_ms: u64) -> Result<(), StoreError> {
    sql.exec(
        "INSERT INTO chain_schedule (id, block_number, block_hash, tempo, last_epoch_block, pending_epoch_at,
            subnet_epoch_index, blocks_since_last_step, reveal_period, observed_at_ms, retry_block)
         VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET block_number = excluded.block_number, block_hash = excluded.block_hash,
            tempo = excluded.tempo, last_epoch_block = excluded.last_epoch_block,
            pending_epoch_at = excluded.pending_epoch_at, subnet_epoch_index = excluded.subnet_epoch_index,
            blocks_since_last_step = excluded.blocks_since_last_step, reveal_period = excluded.reveal_period,
            observed_at_ms = excluded.observed_at_ms, retry_block = excluded.retry_block",
        &[
            bind_u64(p.block.number)?,
            p.block.hash.to_hex().into(),
            SqlValue::Integer(p.tempo.into()),
            bind_u64(p.last_epoch_block)?,
            bind_u64(p.pending_epoch_at)?,
            bind_u64(p.subnet_epoch_index)?,
            bind_u64(p.blocks_since_last_step)?,
            bind_u64(p.reveal_period)?,
            bind_u64(now_ms)?,
            bind_u64(p.block.number + PREFLIGHT_RETRY)?,
        ],
    )?;
    Ok(())
}

/// One decoded preflight item; every non-optional item is decoded.
fn pick<'a>(v: &'a BTreeMap<&str, Value>, item: &'static str) -> Result<&'a Value, SignError> {
    v.get(item)
        .ok_or(SignError::Chain(ChainError::new("storage_absent", item)))
}

fn decode_preflight(
    rt: &ChainRuntime,
    cfg: &SignConfig,
    mechanism: u8,
    at_block: BlockRef,
    values: &Values,
) -> Result<Result<Preflight, &'static str>, SignError> {
    let mut v: BTreeMap<&str, Value> = BTreeMap::new();
    for (p, i, k) in PREFLIGHT_ITEMS {
        if OPTIONAL_ITEMS.contains(i) && !rt.has_storage(p, i) {
            continue;
        }
        let key = item_key(rt, cfg, mechanism, p, i, *k)?;
        v.insert(*i, values.get(rt, p, i, &key)?);
    }
    if !as_bool(pick(&v, "NetworksAdded")?).unwrap_or(false) {
        return Ok(Err("subnet_not_registered"));
    }
    let Some(uid) = as_u64(pick(&v, "Uids")?) else {
        return Ok(Err("hotkey_not_registered"));
    };
    let last_updates = as_list(pick(&v, "LastUpdate")?).unwrap_or_default();
    let uid16 = u16::try_from(uid)
        .map_err(|_| SignError::Chain(ChainError::new("storage_shape", "uid")))?;
    let tempo = number(pick(&v, "Tempo")?, "Tempo")?;
    Ok(Ok(Preflight {
        block: at_block,
        timestamp_ms: number(pick(&v, "Now")?, "Timestamp")?,
        nonce: number(
            field(pick(&v, "Account")?, "nonce").unwrap_or(&Value::Null),
            "nonce",
        )?,
        uid: uid16,
        population: u16::try_from(number(pick(&v, "SubnetworkN")?, "SubnetworkN")?)
            .unwrap_or(u16::MAX),
        tempo: u16::try_from(tempo)
            .map_err(|_| SignError::Chain(ChainError::new("storage_shape", "tempo")))?,
        last_epoch_block: number(pick(&v, "LastEpochBlock")?, "LastEpochBlock")?,
        pending_epoch_at: number(pick(&v, "PendingEpochAt")?, "PendingEpochAt")?,
        subnet_epoch_index: number(pick(&v, "SubnetEpochIndex")?, "SubnetEpochIndex")?,
        blocks_since_last_step: number(pick(&v, "BlocksSinceLastStep")?, "BlocksSinceLastStep")?,
        reveal_period: number(pick(&v, "RevealPeriodEpochs")?, "RevealPeriodEpochs")?,
        commit_reveal_enabled: as_bool(pick(&v, "CommitRevealWeightsEnabled")?).unwrap_or(false),
        commit_reveal_version: match u16::try_from(number(
            pick(&v, "CommitRevealWeightsVersion")?,
            "CommitRevealWeightsVersion",
        )?) {
            Ok(tag) => tag,
            // No u16 commit tag can carry it: no encoding exists.
            Err(_) => return Ok(Err("commit_reveal_version_unencodable")),
        },
        required_version_key: number(pick(&v, "WeightsVersionKey")?, "WeightsVersionKey")?,
        rate_limit: number(pick(&v, "WeightsSetRateLimit")?, "WeightsSetRateLimit")?,
        last_update: last_updates
            .get(usize::from(uid16))
            .and_then(as_u64)
            .unwrap_or(0),
        min_allowed_weights: v.get("MinAllowedWeights").and_then(as_u64),
        max_weights_limit: v.get("MaxWeightsLimit").and_then(as_u64),
        spec_version: rt.spec_version(),
    }))
}

/// The latest verdict on a commitment carrying `tag`, when it is against the
/// tag: the epoch of our latest commitment with this tag that either revealed
/// or left the chain's queue with no reveal event (`reveal_dropped`), if it is
/// the latter, and the runtime spec its preflight recorded (an observation,
/// never a key or a comparison).
///
/// Evidence, never a deferral. `reveal_dropped` also covers weights the chain
/// refused when it set them (a deregistered or re-registered UID, a raised
/// `MinAllowedWeights`, the version key), which says nothing about the tag,
/// and a commit that is not made can never reveal and clear it. So the
/// coordinator keeps committing with the stored tag: one operation per epoch
/// is the probe's only bound, and a dropped commit costs that epoch's weights,
/// what a skipped commit costs and what a native-tag drop already costs. A
/// queued commitment past its reveal epoch (`reveal_missed`, a late drand
/// pulse) is explained and is not reported.
fn cr_tag_unrevealed(sql: &impl Sql, tag: u16) -> Result<Option<(u64, Option<u64>)>, StoreError> {
    let latest = sql.exec(
        "SELECT epoch, state,
                json_extract(preflight_json, '$.spec_version')
         FROM commit_ops
         WHERE (state = 'revealed' OR (state = 'failed' AND detail = 'reveal_dropped'))
           AND json_extract(preflight_json, '$.commit_reveal_version') = ?
         ORDER BY epoch DESC LIMIT 1",
        &[SqlValue::Integer(tag.into())],
    )?;
    let Some(row) = latest.first() else {
        return Ok(None);
    };
    if text(row, 1)? != "failed" {
        return Ok(None);
    }
    Ok(Some((uint(row, 0)?, opt_u(row, 2))))
}

fn apply_preflight(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    rt: &ChainRuntime,
    at_block: BlockRef,
    values: &Values,
    now_ms: u64,
) -> Result<Option<Chained>, SignError> {
    let cfg = ctx.cfg;
    let Some(Ok(t)) = active_target(sql, cfg, ctx.trusted)? else {
        return Ok(None);
    };
    let p = match decode_preflight(rt, cfg, t.mechanism, at_block, values)? {
        Ok(p) => p,
        Err(code) => {
            journal::bump(sql, "sign_preflight_refusals")?;
            log(
                sql,
                now_ms,
                Some(at_block.number),
                None,
                None,
                "preflight_refused",
                Some(code),
            )?;
            // Counted against the preflight, which backs off.
            return Err(SignError::Chain(ChainError::new(code, "")));
        }
    };
    save_schedule(sql, &p, now_ms)?;
    journal::bump(sql, "sign_preflights")?;
    if !p.commit_reveal_enabled {
        // Never fall back to direct weights.
        pause(sql, "commit_reveal_disabled", now_ms)?;
        return Ok(None);
    }
    let state = p.schedule();
    let epoch = chain::commit_epoch(&state);
    let preparing = ops_where(sql, "state = 'preparing'")?;
    let existing = op_for_epoch(sql, epoch)?;
    // A preparing operation from an earlier epoch can no longer be committed
    // for the epoch it names.
    for o in &preparing {
        if o.epoch != epoch {
            // A rejected attempt whose runtime never changed in its window
            // stays a pool rejection.
            let code = if trailing_rejections(sql, &o.op_id)? > 0 { POOL_REJECTED } else { "epoch_passed" };
            fail_op(sql, &o.op_id, code, now_ms)?;
        }
    }
    if !commit_window(cfg, &state) {
        return Ok(None);
    }
    if p.commit_reveal_version != NATIVE_CR_PAYLOAD_VERSION {
        // The commit follows the stored tag (D-086), every epoch. Our latest
        // commitment with this tag dropping unrevealed is reported beside it,
        // never acted on (see `cr_tag_unrevealed`).
        let followed = format!("tag {} under spec {}", p.commit_reveal_version, p.spec_version);
        let detail = match cr_tag_unrevealed(sql, p.commit_reveal_version)? {
            None => followed,
            Some((epoch, spec)) => format!(
                "{followed}; epoch {epoch} (spec {}) dropped unrevealed",
                spec.map_or_else(|| "unrecorded".to_owned(), |s| s.to_string())
            ),
        };
        log(
            sql,
            now_ms,
            Some(at_block.number),
            None,
            None,
            "commit_reveal_tag_followed",
            Some(&detail),
        )?;
    }
    if cfg.version_key < p.required_version_key {
        log(
            sql,
            now_ms,
            Some(at_block.number),
            None,
            None,
            "deferred",
            Some("weights_version_key"),
        )?;
        return Ok(None);
    }
    if p.last_update > 0 && at_block.number.saturating_sub(p.last_update) < p.rate_limit {
        log(
            sql,
            now_ms,
            Some(at_block.number),
            None,
            None,
            "deferred",
            Some("rate_limit"),
        )?;
        return Ok(None);
    }
    let op_id = match t.op_id(ctx.trusted, cfg.hotkey, epoch) {
        Ok(id) => id,
        Err(refusal) => {
            log(sql, now_ms, Some(at_block.number), None, None, "deferred", Some(refusal.code))?;
            return Ok(None);
        }
    };
    match existing {
        Some(id) if id != op_id => return Ok(None),
        Some(_) => {
            let o = op(sql, &op_id)?.ok_or(StoreError("op row"))?;
            if o.state != "preparing" {
                return Ok(None);
            }
        }
        None => {
            sql.exec(
                "INSERT INTO commit_ops (op_id, epoch, target, base_op, state, created_at_ms, updated_at_ms)
                 VALUES (?, ?, ?, ?, 'preparing', ?, ?)",
                &[op_id.as_str().into(), bind_u64(epoch)?, t.digest.as_str().into(), t.base_op.as_str().into(), bind_u64(now_ms)?, bind_u64(now_ms)?],
            )?;
            journal::bump(sql, "commit_ops_created")?;
            log(
                sql,
                now_ms,
                Some(at_block.number),
                Some(&op_id),
                None,
                "op_created",
                Some(&format!("epoch {epoch}")),
            )?;
        }
    }
    sql.exec(
        "UPDATE commit_ops SET preflight_block = ?, preflight_hash = ?, preflight_nonce = ?, preflight_json = ?,
            uid = ?, reveal_period = ?, updated_at_ms = ? WHERE op_id = ?",
        &[
            bind_u64(at_block.number)?,
            at_block.hash.to_hex().into(),
            bind_u64(p.nonce)?,
            serde_json::to_string(&p).map_err(|_| StoreError("preflight json"))?.into(),
            SqlValue::Integer(p.uid.into()),
            bind_u64(p.reveal_period)?,
            bind_u64(now_ms)?,
            op_id.as_str().into(),
        ],
    )?;
    Ok(Some(Chained::Registration {
        op_id,
        at: at_block,
    }))
}

fn apply_registration(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    rt: &ChainRuntime,
    at_block: BlockRef,
    op_id: &str,
    values: &Values,
    now_ms: u64,
) -> Result<Option<Chained>, SignError> {
    let cfg = ctx.cfg;
    let Some(o) = op(sql, op_id)? else {
        return Ok(None);
    };
    let Some(p) = o.preflight.clone() else {
        return Ok(None);
    };
    if o.state != "preparing" || p.block != at_block {
        return Ok(None);
    }
    let failed = |code: &str| fail_op(sql, op_id, code, now_ms).map(|_| None);
    let t = match active_target(sql, cfg, ctx.trusted)? {
        Some(Ok(t)) => t,
        Some(Err(r)) => return Ok(failed(r.code)?),
        None => return Ok(failed("no_target")?),
    };
    if t.mechanism != 0 {
        return Ok(failed("mechanism_unsupported")?);
    }
    // The chain's own queue for this epoch: never commit twice.
    let commits = values.get(
        rt,
        chain::SUBTENSOR,
        "TimelockedWeightCommits",
        &commits_key(rt, cfg, t.mechanism, o.epoch)?,
    )?;
    if as_list(&commits)
        .unwrap_or_default()
        .iter()
        .any(|e| as_list(e).and_then(|f| f.first()).and_then(as_account) == Some(cfg.hotkey))
    {
        journal::bump(sql, "sign_duplicate_refusals")?;
        return Ok(failed("commit_already_present")?);
    }
    let Some(neurons) = read_neurons(rt, cfg, p.population, values)? else {
        return Ok(failed("registration_unreadable")?);
    };
    registry::save(sql, o.epoch, at_block.number, p.population, &neurons, now_ms)?;
    let summary = match &t.verified {
        Verified::V1(v) => {
            let snapshot = match target::snapshot(v.chain(), at_block.number, at_block.hash.0, neurons) {
                Ok(s) => s,
                Err(r) => return Ok(failed(r.code)?),
            };
            match target::derive_row(v, &snapshot).and_then(|r| target::summary(&r)) {
                Ok(s) => s,
                Err(r) => return Ok(failed(r.code)?),
            }
        }
        Verified::V2(v) => {
            let ok = owner_key(rt, cfg)?;
            let Some(owner) = as_account(&values.get(rt, chain::SUBTENSOR, "SubnetOwnerHotkey", &ok)?) else {
                return Ok(failed("owner_hotkey_unreadable")?);
            };
            let snapshot = match target::snapshot_v2(&v.chain, at_block.number, at_block.hash.0, owner, neurons) {
                Ok(s) => s,
                Err(r) => return Ok(failed(r.code)?),
            };
            match paid_summary_v2(v, &snapshot) {
                Ok(s) => s,
                Err(code) => return Ok(failed(code)?),
            }
        }
    };
    if let Some(min) = p.min_allowed_weights
        && (summary.entries.len() as u64) < min.min(u64::from(p.population))
        && !(summary.entries.len() == 1 && summary.entries.first().is_some_and(|e| e.0 == p.uid))
    {
        return Ok(failed("row_below_min_allowed_weights")?);
    }
    sql.exec(
        "UPDATE commit_ops SET row_json = ?, row_digest = ?, updated_at_ms = ? WHERE op_id = ?",
        &[
            serde_json::to_string(&summary)
                .map_err(|_| StoreError("row json"))?
                .into(),
            summary.digest.as_str().into(),
            bind_u64(now_ms)?,
            op_id.into(),
        ],
    )?;
    // Signing reads the version of the runtime that executes the attempt
    // fresh, at the preflight block.
    Ok(Some(Chained::SignRuntime {
        op_id: op_id.to_owned(),
        at: at_block,
    }))
}

/// An operation's failure detail when the pool rejected an attempt signed
/// from fresh chain state after an earlier rejection, or when its epoch
/// passed before the fresh attempt. While it waits for that fresh attempt
/// the preparing operation carries it as its detail.
pub const POOL_REJECTED: &str = "pool_rejected";

/// How many of the operation's latest attempts in a row the pool rejected.
fn trailing_rejections(sql: &impl Sql, op_id: &str) -> Result<u64, StoreError> {
    let rows = sql.exec(
        "SELECT state FROM sign_attempts WHERE op_id = ? ORDER BY attempt DESC",
        &[op_id.into()],
    )?;
    let mut n = 0;
    for r in &rows {
        if text(r, 0)? != "rejected" {
            break;
        }
        n += 1;
    }
    Ok(n)
}

/// Signs the attempt for a preparing operation whose row is derived, with
/// the runtime the chain reported at its preflight block.
fn sign_op(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    rt: &ChainRuntime,
    op_id: &str,
    at_block: BlockRef,
    now_ms: u64,
) -> Result<(), SignError> {
    let Some(o) = op(sql, op_id)? else {
        return Ok(());
    };
    let (Some(p), Some(row)) = (o.preflight.clone(), o.row.clone()) else {
        return Ok(());
    };
    if o.state != "preparing" || p.block != at_block || live_attempt(sql)?.is_some() {
        return Ok(());
    }
    // A revocation journaled since the row was derived: never sign.
    if let Some(Err(r)) = active_target(sql, ctx.cfg, ctx.trusted)?
        && r.code == "authorization_revoked"
    {
        fail_op(sql, op_id, r.code, now_ms)?;
        return Ok(());
    }
    // The fresh attempt after a pool rejection, from this preflight's state
    // and runtime (the runtime's spec is an observation in the log).
    if trailing_rejections(sql, op_id)? > 0 {
        journal::bump(sql, "sign_rejections_resigned")?;
        log(
            sql,
            now_ms,
            Some(at_block.number),
            Some(op_id),
            None,
            "resign_after_rejection",
            Some(&format!("runtime spec {}", rt.spec_version())),
        )?;
    }
    sign_attempt(sql, ctx, rt, &o, &p, &row, now_ms)
}

/// Allocates the nonce, encrypts, composes, signs and journals one attempt,
/// all in this transaction. Nothing is sent here.
fn sign_attempt(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    rt: &ChainRuntime,
    o: &Op,
    p: &Preflight,
    row: &RowSummary,
    now_ms: u64,
) -> Result<(), SignError> {
    let cfg = ctx.cfg;
    let s = signer(sql)?;
    if s.state != "active" {
        return Ok(());
    }
    if p.nonce < s.nonce_floor {
        fault(sql, "nonce_regressed", Some(&o.op_id), None, now_ms)?;
        return Ok(());
    }
    let hotkey = match ctx.keys.hotkey() {
        Ok(h) => h,
        Err(_) => {
            pause(sql, "hotkey_secret_unavailable", now_ms)?;
            return Ok(());
        }
    };
    if hotkey.public() != cfg.hotkey {
        fault(sql, "hotkey_mismatch", Some(&o.op_id), None, now_ms)?;
        return Ok(());
    }
    let state = p.schedule();
    let round = chain::reveal_round(p.timestamp_ms as f64 / 1000.0, &state, p.reveal_period)
        .map_err(SignError::Chain)?;
    let reveal_block =
        chain::first_reveal_block(&state, p.reveal_period).map_err(SignError::Chain)?;
    // The committed form: a reward-v1 row max-upscaled, as the chain keeps it.
    let uids: Vec<u16> = row.committed().iter().map(|e| e.0).collect();
    let weights: Vec<u16> = row.committed().iter().map(|e| e.1).collect();
    let payload = chain::commit_payload(cfg.hotkey, &uids, &weights, cfg.version_key);
    let ciphertext = chain::encrypt(&payload, round).map_err(SignError::Chain)?;
    if ciphertext.len() > chain::YUMA_COMMIT_LIMIT {
        return Ok(fail_op(sql, &o.op_id, "ciphertext_too_large", now_ms)?);
    }
    // The stored tag read at this operation's preflight (D-086).
    let call = match rt.commit_call(cfg.netuid, 0, &ciphertext, round, p.commit_reveal_version) {
        Ok(c) => c,
        Err(e) => {
            // No type-correct encoding under live metadata: defer this
            // operation alone; it is re-evaluated at the next preflight.
            log(
                sql,
                now_ms,
                Some(p.block.number),
                Some(&o.op_id),
                None,
                "deferred",
                Some(e.code),
            )?;
            return Ok(());
        }
    };
    let (era, birth, expiry) = chain::mortal_era(cfg.era_period, p.block.number);
    let Some(anchor) = at(sql, birth)? else {
        return Ok(());
    };
    let params = chain::tx_params(era, p.nonce, cfg.genesis.0, anchor.hash.0);
    let signing_payload = rt
        .signing_payload(&call, &params)
        .map_err(SignError::Chain)?;
    let signature = hotkey.sign(&signing_payload).map_err(SignError::Chain)?;
    if !chain::verify_sr25519(&cfg.hotkey, &signing_payload, &signature) {
        fault(sql, "signature_unverifiable", Some(&o.op_id), None, now_ms)?;
        return Ok(());
    }
    let (extrinsic, hash) = rt
        .signed_extrinsic(&call, cfg.hotkey, &signature, &params)
        .map_err(SignError::Chain)?;
    drop(hotkey);
    let n = sql
        .exec(
            "SELECT COALESCE(MAX(attempt), 0) FROM sign_attempts WHERE op_id = ?",
            &[o.op_id.as_str().into()],
        )?
        .first()
        .map(|r| uint(r, 0))
        .transpose()?
        .unwrap_or(0) as u32
        + 1;
    let generation = claims::status(sql, SIGNER_CLAIM, now_ms)?.map_or(0, |c| c.generation);
    sql.exec(
        "INSERT INTO sign_attempts (op_id, attempt, nonce, era_birth, era_period, expiry, era_hash, floor_block,
            spec_version, tx_version, reveal_round, ciphertext_sha256, call_sha256, payload_sha256,
            extrinsic_hash, extrinsic_hex, state, sends, signed_generation, signed_at_ms)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'signed', 0, ?, ?)",
        &[
            o.op_id.as_str().into(),
            SqlValue::Integer(n.into()),
            bind_u64(p.nonce)?,
            bind_u64(birth)?,
            bind_u64(cfg.era_period)?,
            bind_u64(expiry)?,
            anchor.hash.to_hex().into(),
            bind_u64(p.block.number)?,
            SqlValue::Integer(rt.spec_version().into()),
            SqlValue::Integer(rt.tx_version().into()),
            bind_u64(round)?,
            chain::sha256_hex(&ciphertext).into(),
            chain::sha256_hex(&call).into(),
            chain::sha256_hex(&signing_payload).into(),
            format!("0x{}", hex::encode(hash)).into(),
            format!("0x{}", hex::encode(&extrinsic)).into(),
            bind_u64(generation)?,
            bind_u64(now_ms)?,
        ],
    )?;
    sql.exec(
        "UPDATE commit_ops SET state = 'committing', reveal_round = ?, predicted_reveal_block = ?, updated_at_ms = ? WHERE op_id = ?",
        &[bind_u64(round)?, bind_u64(reveal_block)?, bind_u64(now_ms)?, o.op_id.as_str().into()],
    )?;
    journal::bump(sql, "sign_attempts_signed")?;
    log(
        sql,
        now_ms,
        Some(p.block.number),
        Some(&o.op_id),
        Some(n),
        "attempt_signed",
        Some(&format!("nonce {} expiry {expiry} round {round}", p.nonce)),
    )?;
    Ok(())
}

fn apply_submit(
    sql: &impl Sql,
    op_id: &str,
    n: u32,
    result: &SubmitOutcome,
    now_ms: u64,
) -> Result<(), SignError> {
    let Some(a) = attempt(sql, op_id, n)? else {
        return Ok(());
    };
    if a.state != "sending" {
        journal::bump(sql, "sign_late_submit_outcomes")?;
        return Ok(());
    }
    match result {
        SubmitOutcome::Accepted { hash } => {
            let ours = sql
                .exec(
                    "SELECT extrinsic_hash FROM sign_attempts WHERE op_id = ? AND attempt = ?",
                    &[op_id.into(), SqlValue::Integer(n.into())],
                )?
                .first()
                .map(|r| text(r, 0).map(|h| h.eq_ignore_ascii_case(hash)))
                .transpose()?
                .unwrap_or(false);
            let state = if ours { "submitted" } else { "uncertain" };
            set_attempt(
                sql,
                &a,
                &format!("state = '{state}', submit_detail = ?"),
                vec![
                    if ours {
                        "accepted"
                    } else {
                        "accepted_hash_mismatch"
                    }
                    .into(),
                ],
            )?;
            log(sql, now_ms, None, Some(op_id), Some(n), state, None)?;
        }
        SubmitOutcome::Rejected { code, message } => {
            let detail: String = format!("{code}: {message}").chars().take(160).collect();
            set_attempt(
                sql,
                &a,
                "state = 'rejected', submit_detail = ?, resolved_at_ms = ?",
                vec![detail.as_str().into(), bind_u64(now_ms)?],
            )?;
            journal::bump(sql, "sign_rejections")?;
            log(
                sql,
                now_ms,
                None,
                Some(op_id),
                Some(n),
                "rejected",
                Some(&detail),
            )?;
            // A rejected extrinsic consumed no nonce. Its signed bytes bind
            // the chain state of its preflight: the runtime's versions, the
            // nonce, the era. A runtime upgrade between signing and
            // submission is a rejection that fresh state cures, so the first
            // rejection returns the operation to preparing: its next
            // preflight re-reads that state and re-learns the runtime, and
            // the operation signs once more. A rejection of that fresh
            // attempt too shows fresh state does not cure it: final for the
            // epoch, as a lone rejection used to be.
            if trailing_rejections(sql, op_id)? >= 2 {
                fail_op(sql, op_id, POOL_REJECTED, now_ms)?;
            } else {
                set_op_state(sql, op_id, "preparing", Some(POOL_REJECTED), now_ms)?;
            }
        }
        SubmitOutcome::Uncertain { reason } => {
            let detail: String = reason.chars().take(160).collect();
            set_attempt(
                sql,
                &a,
                "state = 'uncertain', submit_detail = ?",
                vec![detail.as_str().into()],
            )?;
            journal::bump(sql, "sign_uncertain_submissions")?;
            log(
                sql,
                now_ms,
                None,
                Some(op_id),
                Some(n),
                "uncertain",
                Some(&detail),
            )?;
        }
    }
    Ok(())
}

/// A block body's answer for attempt `n` of `op_id`. The Worker matched the
/// body against the plan's `target` (`rpc::BlockScan`); a target that is not
/// this attempt's bytes (a plan stored by an earlier build carries none)
/// answers nothing about them, so it is a deterministic failure of the
/// attempt and the next round asks again. Only a complete body without the
/// bytes is "not here".
fn apply_block(
    sql: &impl Sql,
    at_block: BlockRef,
    op_id: &str,
    n: u32,
    target: &str,
    m: crate::rpc::BlockMatch,
    now_ms: u64,
) -> Result<(), SignError> {
    let Some(a) = attempt(sql, op_id, n)? else {
        return Ok(());
    };
    if target.is_empty() || !target.eq_ignore_ascii_case(&a.extrinsic_hex) {
        return Err(SignError::Chain(ChainError::new("block_target_mismatch", "")));
    }
    if m.index.is_some_and(|i| i >= m.count) {
        return Err(SignError::Chain(ChainError::new("block_match_malformed", "")));
    }
    let index = m.index;
    match index {
        Some(i) => {
            set_attempt(
                sql,
                &a,
                "state = 'included', included_block = ?, included_hash = ?, included_index = ?",
                vec![
                    bind_u64(at_block.number)?,
                    at_block.hash.to_hex().into(),
                    SqlValue::Integer(i as i64),
                ],
            )?;
            log(
                sql,
                now_ms,
                Some(at_block.number),
                Some(op_id),
                Some(n),
                "included",
                Some(&format!("index {i}")),
            )?;
        }
        None if a.search_scan && at_block.number < a.search_hi.unwrap_or(0).min(a.expiry) => {
            // The body scan goes on past a block without these bytes.
            set_attempt(sql, &a, "search_lo = ?", vec![bind_u64(at_block.number)?])?;
        }
        None => {
            // The nonce was consumed here by bytes this journal never signed
            // (or, for a body scan, nowhere in the attempt's era before the
            // block that consumed it).
            displaced(sql, &a, now_ms)?;
        }
    }
    Ok(())
}

/// The attempt's nonce was consumed by bytes this journal never signed: a
/// durable fault, and the epoch's operation fails.
fn displaced(sql: &impl Sql, a: &Attempt, now_ms: u64) -> Result<(), StoreError> {
    set_attempt(
        sql,
        a,
        "state = 'displaced', resolved_at_ms = ?",
        vec![bind_u64(now_ms)?],
    )?;
    fault(sql, "foreign_nonce_use", Some(&a.op_id), Some(a.attempt), now_ms)?;
    fail_op(sql, &a.op_id, "nonce_displaced", now_ms)
}

/// D5: the events of block `at_block`, which consumed attempt `n`'s nonce.
/// A `TimelockedWeightsCommitted` event whose account is this hotkey, whose
/// netuid index is this subnet's and whose reveal round is the attempt's is
/// its inclusion, at that event's extrinsic index (its dispatch is then read
/// as for any included attempt). A commit event of this hotkey with another
/// round or netuid is positive evidence of bytes this journal never signed
/// (`foreign_nonce_use`). No commit event of this hotkey: the body decides,
/// as before (our own extrinsic may have failed dispatch, which emits no
/// commit event).
#[allow(clippy::too_many_arguments)]
fn apply_inclusion_events(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    rt: &ChainRuntime,
    at_block: BlockRef,
    op_id: &str,
    n: u32,
    values: &Values,
    now_ms: u64,
) -> Result<(), SignError> {
    let cfg = ctx.cfg;
    let Some(a) = attempt(sql, op_id, n)? else {
        return Ok(());
    };
    if !matches!(a.state.as_str(), "submitted" | "uncertain") || a.search_hi != Some(at_block.number) {
        return Ok(());
    }
    let key = rt.storage_key("System", "Events", &[]).map_err(SignError::Chain)?;
    // Events this build cannot decode are no evidence: the body decides.
    let Ok(events) = rt.events(values.raw(&key)?) else {
        set_attempt(sql, &a, "events_checked = 1", vec![])?;
        log(sql, now_ms, Some(at_block.number), Some(op_id), Some(n), "inclusion_events_unreadable", None)?;
        return Ok(());
    };
    let index_here = storage_index(cfg.netuid, 0).map(u64::from);
    let ours: Vec<_> = events
        .iter()
        .filter(|e| e.pallet == chain::SUBTENSOR && e.name == "TimelockedWeightsCommitted")
        .filter(|e| e.args().first().and_then(|v| as_account(v)) == Some(cfg.hotkey))
        .collect();
    let matching = ours.iter().find(|e| {
        e.phase == "ApplyExtrinsic"
            && e.extrinsic_index.is_some()
            && e.args().get(1).and_then(|v| as_u64(v)) == index_here
            && e.args().get(3).and_then(|v| as_u64(v)) == Some(a.reveal_round)
    });
    if let Some(e) = matching {
        let i = e.extrinsic_index.unwrap_or_default();
        let hash = e.args().get(2).and_then(|v| as_bytes(v)).map(hex::encode).unwrap_or_default();
        set_attempt(
            sql,
            &a,
            "state = 'included', included_block = ?, included_hash = ?, included_index = ?, events_checked = 1",
            vec![bind_u64(at_block.number)?, at_block.hash.to_hex().into(), SqlValue::Integer(i64::from(i))],
        )?;
        journal::bump(sql, "sign_included_by_events")?;
        log(sql, now_ms, Some(at_block.number), Some(op_id), Some(n), "included", Some(&format!("index {i} by events, commit {hash}")))?;
        return Ok(());
    }
    if !ours.is_empty() {
        // A commit of this hotkey with another round or netuid consumed
        // the nonce: positive evidence of foreign bytes.
        displaced(sql, &a, now_ms)?;
        return Ok(());
    }
    set_attempt(sql, &a, "events_checked = 1", vec![])?;
    Ok(())
}

/// D5: an attempt whose nonce was consumed in a block with no commit event
/// of this hotkey and whose body cannot be read: no evidence either way. Its
/// epoch is never signed again (its operation fails, `commit_ops.epoch`
/// stays unique), the nonce floor moves past its nonce, the next epoch is
/// planned with the next nonce, and the signing status reports it
/// (`consumed_unverified`). Never a guess that it was included or not.
fn consumed_unverified(sql: &impl Sql, a: &Attempt, now_ms: u64) -> Result<(), StoreError> {
    set_attempt(sql, a, "state = 'consumed_unverified', resolved_at_ms = ?", vec![bind_u64(now_ms)?])?;
    sql.exec("UPDATE signer SET nonce_floor = MAX(nonce_floor, ?) WHERE id = 1", &[bind_u64(a.nonce + 1)?])?;
    journal::bump(sql, "sign_consumed_unverified")?;
    log(
        sql,
        now_ms,
        a.search_hi,
        Some(&a.op_id),
        Some(a.attempt),
        "consumed_unverified",
        Some(&format!("nonce {} block {}", a.nonce, a.search_hi.unwrap_or(0))),
    )?;
    fail_op(sql, &a.op_id, "consumed_unverified", now_ms)
}

#[allow(clippy::too_many_arguments)]
fn apply_events(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    rt: &ChainRuntime,
    at_block: BlockRef,
    op_id: &str,
    n: u32,
    values: &Values,
    now_ms: u64,
) -> Result<(), SignError> {
    let cfg = ctx.cfg;
    let Some(a) = attempt(sql, op_id, n)? else {
        return Ok(());
    };
    let Some(index) = a.included_index else {
        return Ok(());
    };
    if a.included_block != Some(at_block.number) {
        return Ok(());
    }
    let key = rt
        .storage_key("System", "Events", &[])
        .map_err(SignError::Chain)?;
    let events = rt.events(values.raw(&key)?).map_err(SignError::Chain)?;
    let mine: Vec<_> = events
        .iter()
        .filter(|e| e.phase == "ApplyExtrinsic" && e.extrinsic_index == Some(index))
        .collect();
    let success = mine
        .iter()
        .any(|e| e.pallet == "System" && e.name == "ExtrinsicSuccess");
    let failed = mine
        .iter()
        .find(|e| e.pallet == "System" && e.name == "ExtrinsicFailed");
    let committed = mine
        .iter()
        .find(|e| e.pallet == chain::SUBTENSOR && e.name == "TimelockedWeightsCommitted");
    let dispatch = if success {
        "success".to_owned()
    } else if failed.is_some() {
        "failed".to_owned()
    } else {
        "unknown".to_owned()
    };
    set_attempt(
        sql,
        &a,
        "dispatch = ?, resolved_at_ms = ?",
        vec![dispatch.as_str().into(), bind_u64(now_ms)?],
    )?;
    sql.exec(
        "UPDATE signer SET nonce_floor = MAX(nonce_floor, ?) WHERE id = 1",
        &[bind_u64(a.nonce + 1)?],
    )?;
    if success {
        let round = committed.and_then(|e| e.args().get(3).and_then(|v| as_u64(v)));
        let who = committed.and_then(|e| e.args().first().and_then(|v| as_account(v)));
        if who != Some(cfg.hotkey) || round != Some(a.reveal_round) {
            log(
                sql,
                now_ms,
                Some(at_block.number),
                Some(op_id),
                Some(n),
                "commit_event_mismatch",
                None,
            )?;
        }
        sql.exec(
            "UPDATE commit_ops SET state = 'committed', commit_block = ?, commit_block_hash = ?, commit_index = ?, updated_at_ms = ? WHERE op_id = ?",
            &[bind_u64(at_block.number)?, at_block.hash.to_hex().into(), SqlValue::Integer(index.into()), bind_u64(now_ms)?, op_id.into()],
        )?;
        journal::bump(sql, "commit_ops_committed")?;
        log(
            sql,
            now_ms,
            Some(at_block.number),
            Some(op_id),
            Some(n),
            "op_committed",
            Some(&format!("index {index}")),
        )?;
    } else {
        let detail = failed
            .map(|e| format!("{}", e.attributes))
            .unwrap_or_default();
        let detail: String = detail.chars().take(200).collect();
        log(
            sql,
            now_ms,
            Some(at_block.number),
            Some(op_id),
            Some(n),
            "dispatch_failed",
            Some(&detail),
        )?;
        fail_op(sql, op_id, "dispatch_failed", now_ms)?;
    }
    Ok(())
}

fn find_commit(
    commits: &Value,
    hotkey: [u8; 32],
    commit_block: u64,
    ciphertext_sha256: &str,
    round: u64,
) -> bool {
    as_list(commits).unwrap_or_default().iter().any(|entry| {
        let Some([who, block, ciphertext, reveal_round]) = as_list(entry) else {
            return false;
        };
        as_account(who) == Some(hotkey)
            && as_u64(block) == Some(commit_block)
            && as_bytes(ciphertext).is_some_and(|b| chain::sha256_hex(&b) == ciphertext_sha256)
            && as_u64(reveal_round) == Some(round)
    })
}

fn included_attempt(sql: &impl Sql, op_id: &str) -> Result<Option<Attempt>, StoreError> {
    sql.exec(
        &format!(
            "SELECT {ATTEMPT_COLUMNS} FROM sign_attempts WHERE op_id = ? AND state = 'included'
                  AND dispatch IN ('success', 'unverifiable')"
        ),
        &[op_id.into()],
    )?
    .first()
    .map(attempt_from)
    .transpose()
}

fn apply_readback(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    rt: &ChainRuntime,
    at_block: BlockRef,
    op_id: &str,
    values: &Values,
    now_ms: u64,
) -> Result<(), SignError> {
    let cfg = ctx.cfg;
    let Some(o) = op(sql, op_id)? else {
        return Ok(());
    };
    let Some(a) = included_attempt(sql, op_id)? else {
        return Ok(());
    };
    let Some(commit_block) = o.commit_block else {
        return Ok(());
    };
    let mut found = None;
    for epoch in [o.epoch, o.epoch + 1] {
        let v = values.get(
            rt,
            chain::SUBTENSOR,
            "TimelockedWeightCommits",
            &commits_key(rt, cfg, 0, epoch)?,
        )?;
        if find_commit(
            &v,
            cfg.hotkey,
            commit_block,
            &a.ciphertext_sha256,
            a.reveal_round,
        ) {
            found = Some(epoch);
        }
    }
    let readback = match found {
        Some(e) if e == o.epoch => "ok".to_owned(),
        Some(e) => format!("ok_epoch_{e}"),
        None => "missing".to_owned(),
    };
    sql.exec(
        "UPDATE commit_ops SET readback = ?, readback_block = ?, epoch = COALESCE(?, epoch), updated_at_ms = ? WHERE op_id = ?",
        &[readback.as_str().into(), bind_u64(at_block.number)?, found.map_or(Ok(SqlValue::Null), bind_u64)?, bind_u64(now_ms)?, op_id.into()],
    )?;
    log(
        sql,
        now_ms,
        Some(at_block.number),
        Some(op_id),
        Some(a.attempt),
        "readback",
        Some(&readback),
    )?;
    Ok(())
}

fn apply_reveal_check(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    rt: &ChainRuntime,
    at_block: BlockRef,
    op_id: &str,
    values: &Values,
    now_ms: u64,
) -> Result<Option<Chained>, SignError> {
    let cfg = ctx.cfg;
    let Some(o) = op(sql, op_id)? else {
        return Ok(None);
    };
    let Some(a) = included_attempt(sql, op_id)? else {
        return Ok(None);
    };
    let (Some(commit_block), Some(predicted), Some(uid)) =
        (o.commit_block, o.predicted_reveal_block, o.uid)
    else {
        return Ok(None);
    };
    let queue = values.get(
        rt,
        chain::SUBTENSOR,
        "TimelockedWeightCommits",
        &commits_key(rt, cfg, 0, o.epoch)?,
    )?;
    if find_commit(
        &queue,
        cfg.hotkey,
        commit_block,
        &a.ciphertext_sha256,
        a.reveal_round,
    ) {
        let epoch_now = number(
            &values.get(
                rt,
                chain::SUBTENSOR,
                "SubnetEpochIndex",
                &item_key(rt, cfg, 0, chain::SUBTENSOR, "SubnetEpochIndex", 1)?,
            )?,
            "SubnetEpochIndex",
        )?;
        let reveal_period = o.preflight.as_ref().map_or(1, |p| p.reveal_period);
        if epoch_now > o.epoch.saturating_add(reveal_period) {
            log(
                sql,
                now_ms,
                Some(at_block.number),
                Some(op_id),
                None,
                "reveal_check",
                Some(&format!("queued at epoch {epoch_now}")),
            )?;
            set_op_state(sql, op_id, "failed", Some("reveal_missed"), now_ms)?;
            return Ok(None);
        }
        let next = at_block.number.saturating_add(REVEAL_RECHECK);
        sql.exec(
            "UPDATE commit_ops SET next_check_block = ? WHERE op_id = ?",
            &[bind_u64(next)?, op_id.into()],
        )?;
        log(
            sql,
            now_ms,
            Some(at_block.number),
            Some(op_id),
            None,
            "reveal_check",
            Some(&format!("queued, next {next}")),
        )?;
        return Ok(None);
    }
    // The pallet takes the epoch's queue at the first reveal block and puts
    // back only commits whose drand pulse has not arrived, so the entry
    // disappears in exactly the block that revealed (or dropped) it. It was
    // queued before the predicted reveal block (or at the last check that
    // found it) and is gone now.
    let _ = uid;
    let lo = o
        .next_check_block
        .map(|n| n.saturating_sub(REVEAL_RECHECK))
        .unwrap_or(predicted.saturating_sub(1))
        .max(predicted.saturating_sub(1))
        .min(at_block.number.saturating_sub(1));
    log(
        sql,
        now_ms,
        Some(at_block.number),
        Some(op_id),
        None,
        "reveal_check",
        Some(&format!("gone, search {lo}..{}", at_block.number)),
    )?;
    Ok(Some(reveal_search(sql, op_id, lo, at_block.number)?))
}

/// Blocks between reveal checks while the commit is still queued.
const REVEAL_RECHECK: u64 = 5;

/// Stores the search bounds durably and hands over its next step.
fn reveal_search(sql: &impl Sql, op_id: &str, lo: u64, hi: u64) -> Result<Chained, StoreError> {
    sql.exec(
        "UPDATE commit_ops SET reveal_lo = ?, reveal_hi = ? WHERE op_id = ?",
        &[bind_u64(lo)?, bind_u64(hi)?, op_id.into()],
    )?;
    Ok(search_step(op_id, lo, hi))
}

fn search_step(op_id: &str, lo: u64, hi: u64) -> Chained {
    if hi <= lo.saturating_add(1) {
        Chained::RevealEvents {
            op_id: op_id.to_owned(),
            block: hi,
        }
    } else {
        Chained::RevealProbe {
            op_id: op_id.to_owned(),
            lo,
            hi,
        }
    }
}

fn apply_reveal_probe(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    rt: &ChainRuntime,
    op_id: &str,
    (lo, hi, mid): (u64, u64, u64),
    values: &Values,
    now_ms: u64,
) -> Result<Option<Chained>, SignError> {
    let cfg = ctx.cfg;
    let Some(o) = op(sql, op_id)? else {
        return Ok(None);
    };
    let (Some(a), Some(commit_block)) = (included_attempt(sql, op_id)?, o.commit_block) else {
        return Ok(None);
    };
    let queue = values.get(
        rt,
        chain::SUBTENSOR,
        "TimelockedWeightCommits",
        &commits_key(rt, cfg, 0, o.epoch)?,
    )?;
    let queued = find_commit(
        &queue,
        cfg.hotkey,
        commit_block,
        &a.ciphertext_sha256,
        a.reveal_round,
    );
    log(
        sql,
        now_ms,
        Some(mid),
        Some(op_id),
        None,
        "reveal_probe",
        Some(if queued { "queued" } else { "gone" }),
    )?;
    Ok(Some(if queued {
        reveal_search(sql, op_id, mid, hi)?
    } else {
        reveal_search(sql, op_id, lo, mid)?
    }))
}

#[allow(clippy::too_many_arguments)]
fn apply_reveal_events(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    rt: &ChainRuntime,
    at_block: BlockRef,
    op_id: &str,
    block: u64,
    values: &Values,
    now_ms: u64,
) -> Result<Option<Chained>, SignError> {
    let cfg = ctx.cfg;
    let Some(o) = op(sql, op_id)? else {
        return Ok(None);
    };
    let (Some(uid), Some(row)) = (o.uid, o.row.clone()) else {
        return Ok(None);
    };
    let key = rt
        .storage_key("System", "Events", &[])
        .map_err(SignError::Chain)?;
    let events = rt.events(values.raw(&key)?).map_err(SignError::Chain)?;
    let revealed = events.iter().any(|e| {
        e.pallet == chain::SUBTENSOR
            && e.name == "TimelockedWeightsRevealed"
            && e.args().get(1).and_then(|v| as_account(v)) == Some(cfg.hotkey)
    });
    if revealed {
        let wkey = rt
            .storage_key(
                chain::SUBTENSOR,
                "Weights",
                &[index_value(cfg, 0)?, Value::Int(i128::from(uid))],
            )
            .map_err(SignError::Chain)?;
        let weights = values.get(rt, chain::SUBTENSOR, "Weights", &wkey)?;
        let on_chain: Vec<(u16, u16)> = as_list(&weights)
            .unwrap_or_default()
            .iter()
            .filter_map(|p| {
                let f = as_list(p)?;
                Some((
                    u16::try_from(as_u64(f.first()?)?).ok()?,
                    u16::try_from(as_u64(f.get(1)?)?).ok()?,
                ))
            })
            .collect();
        let matches = on_chain == row.committed();
        sql.exec(
            "UPDATE commit_ops SET state = 'revealed', reveal_block = ?, reveal_block_hash = ?, reveal_weights_match = ?, updated_at_ms = ? WHERE op_id = ?",
            &[bind_u64(at_block.number)?, at_block.hash.to_hex().into(), SqlValue::Integer(matches.into()), bind_u64(now_ms)?, op_id.into()],
        )?;
        journal::bump(sql, "commit_ops_revealed")?;
        log(
            sql,
            now_ms,
            Some(at_block.number),
            Some(op_id),
            None,
            "op_revealed",
            Some(if matches {
                "weights_match_row"
            } else {
                "weights_differ"
            }),
        )?;
        return Ok(None);
    }
    // Removed from the queue without a reveal event: the pallet dropped it
    // (undecryptable, or the weights were refused when set).
    let _ = block;
    set_op_state(sql, op_id, "failed", Some("reveal_dropped"), now_ms)?;
    Ok(None)
}

/// Turns a handover from one outcome into the next plan.
fn chained_step(
    sql: &impl Sql,
    ctx: &Ctx<'_>,
    token: &ClaimToken,
    c: BlockRef,
    chained: Chained,
) -> Result<Option<SignPlan>, SignError> {
    match chained {
        Chained::Metadata {
            at,
            spec,
            tx,
            purpose,
        } => Ok(Some(SignPlan::Metadata {
            claim: token.clone(),
            at,
            spec_version: spec,
            tx_version: tx,
            purpose,
        })),
        Chained::LearnWriter { block, spec, read } => {
            // The runtime that wrote `block` is the code in its parent's
            // post-state. Asking at `block` itself would report its own
            // post-state code, which at an upgrade's enactment block is
            // already the next runtime.
            let parent = block
                .number
                .checked_sub(1)
                .ok_or(SignError::Chain(ChainError::new(
                    "block_hash_unretained",
                    "genesis parent",
                )))?;
            Ok(Some(SignPlan::RuntimeVersion {
                claim: token.clone(),
                at: retained(sql, parent)?,
                purpose: RuntimeFor::Writer { block, spec, read },
            }))
        }
        Chained::Read { at, spec, read } => {
            let Some(rt) = ctx.runtime.for_spec(sql, spec)? else {
                return Ok(None);
            };
            let keys = keys_for(sql, ctx.cfg, &rt, &read)?;
            Ok(Some(plan_read(token, at, read, keys)))
        }
        Chained::SignRuntime { op_id, at } => Ok(Some(SignPlan::RuntimeVersion {
            claim: token.clone(),
            at,
            purpose: RuntimeFor::Sign { op_id },
        })),
        Chained::Registration { op_id, at } => {
            let Some(rt) = ctx.runtime.get(sql)? else {
                return Ok(None);
            };
            let read = ReadPurpose::Registration { op_id };
            let keys = keys_for(sql, ctx.cfg, &rt, &read)?;
            Ok(Some(plan_read(token, at, read, keys)))
        }
        Chained::RevealProbe { op_id, lo, hi } => {
            let mid = lo + (hi - lo) / 2;
            let Some(rt) = ctx.runtime.get(sql)? else {
                return Ok(None);
            };
            let read = ReadPurpose::RevealProbe { op_id, lo, hi, mid };
            let keys = keys_for(sql, ctx.cfg, &rt, &read)?;
            Ok(Some(plan_read(token, retained(sql, mid)?, read, keys)))
        }
        Chained::RevealEvents { op_id, block } => {
            if block > c.number {
                return Ok(None);
            }
            let Some(rt) = ctx.runtime.get(sql)? else {
                return Ok(None);
            };
            let read = ReadPurpose::RevealEvents { op_id, block };
            let keys = keys_for(sql, ctx.cfg, &rt, &read)?;
            Ok(Some(plan_read(token, retained(sql, block)?, read, keys)))
        }
        Chained::RowRegistration { at, epoch, population } => {
            let Some(rt) = ctx.runtime.get(sql)? else {
                return Ok(None);
            };
            let read = ReadPurpose::RowRegistration { epoch, population };
            let keys = keys_for(sql, ctx.cfg, &rt, &read)?;
            Ok(Some(plan_read(token, at, read, keys)))
        }
    }
}

// ------------------------------------------------------------------ drills

/// Arms a testnet drill for the next `count` matching steps.
pub fn arm_drill(sql: &impl Sql, name: &str, count: u64, now_ms: u64) -> Result<bool, StoreError> {
    if !DRILLS.contains(&name) {
        return Ok(false);
    }
    sql.exec(
        "INSERT INTO sign_drills (name, remaining, armed_at_ms) VALUES (?, ?, ?)
         ON CONFLICT(name) DO UPDATE SET remaining = excluded.remaining, armed_at_ms = excluded.armed_at_ms",
        &[name.into(), bind_u64(count)?, bind_u64(now_ms)?],
    )?;
    log(sql, now_ms, None, None, None, "drill_armed", Some(name))?;
    Ok(true)
}

pub const DRILLS: &[&str] = &[
    "uncertain_after_send",
    "evict_after_send",
    "stall_before_mark",
    "never_send",
    "ack_failure",
    "trap",
];

/// Whether drill `name` is armed (without taking it).
fn drill_armed(sql: &impl Sql, name: &str) -> Result<bool, StoreError> {
    let rows = sql.exec("SELECT remaining FROM sign_drills WHERE name = ?", &[name.into()])?;
    match rows.first() {
        Some(r) => Ok(int(r, 0)? > 0),
        None => Ok(false),
    }
}

/// Takes one armed drill among `names`, if any.
pub fn take_drill(sql: &impl Sql, names: &[&str]) -> Result<Option<String>, StoreError> {
    for name in names {
        let rows = sql.exec(
            "SELECT remaining FROM sign_drills WHERE name = ?",
            &[(*name).into()],
        )?;
        if let Some(r) = rows.first()
            && int(r, 0)? > 0
        {
            sql.exec(
                "UPDATE sign_drills SET remaining = remaining - 1 WHERE name = ?",
                &[(*name).into()],
            )?;
            return Ok(Some((*name).to_owned()));
        }
    }
    Ok(None)
}

/// Records a content-free signing-log event (drills taken outside a step).
pub fn note(
    sql: &impl Sql,
    now_ms: u64,
    block: Option<u64>,
    event: &str,
    detail: Option<&str>,
) -> Result<(), StoreError> {
    log(sql, now_ms, block, None, None, event, detail)
}

/// Shortens the current claim so a stalled holder's lease lapses (the stale
/// invocation drill).
pub fn shorten_claim(
    sql: &impl Sql,
    token: &ClaimToken,
    lease_ms: u64,
    now_ms: u64,
) -> Result<bool, StoreError> {
    claims::renew(sql, token, lease_ms, now_ms)
}

// ------------------------------------------------------------------ status

#[derive(Clone, Debug, Serialize)]
pub struct OpStatus {
    pub op_id: String,
    pub epoch: u64,
    pub state: String,
    pub detail: Option<String>,
    /// `unknown` for an operation that failed as `inclusion_unverifiable`:
    /// its nonce was consumed but its inclusion could not be read, so its
    /// commit may be on chain. A delivery audit reads such an epoch as
    /// unknown, never as a missing commit (the publisher alerts it once,
    /// `inclusion_unknown`). `None` for every other operation.
    pub inclusion: Option<&'static str>,
    pub preflight_block: Option<u64>,
    pub uid: Option<u64>,
    pub row: Option<serde_json::Value>,
    pub reveal_round: Option<u64>,
    pub predicted_reveal_block: Option<u64>,
    pub commit_block: Option<u64>,
    pub commit_block_hash: Option<String>,
    pub commit_index: Option<u64>,
    pub readback: Option<String>,
    pub readback_block: Option<u64>,
    pub reveal_block: Option<u64>,
    pub reveal_block_hash: Option<String>,
    pub reveal_weights_match: Option<bool>,
    pub diagnostic: Option<String>,
    /// The `CommitRevealWeightsVersion` tag the operation's last preflight
    /// read, which its commit carries (D-086): an observation, never a gate.
    pub commit_reveal_tag: Option<u16>,
    pub attempts: Vec<AttemptStatus>,
}

/// An attempt settled as `consumed_unverified` (D5).
#[derive(Clone, Debug, Serialize)]
pub struct UnverifiedAttempt {
    pub op: String,
    pub attempt: u64,
    pub nonce: u64,
    pub block: Option<u64>,
}

/// One subject's deterministic failures.
#[derive(Clone, Debug, Serialize)]
pub struct FailureStatus {
    pub subject: String,
    pub failures: u64,
    pub code: String,
    pub retry_block: u64,
    pub settled: bool,
    pub age_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct AttemptStatus {
    pub attempt: u64,
    pub nonce: u64,
    pub era_birth: u64,
    pub expiry: u64,
    pub state: String,
    pub sends: u64,
    pub submit_detail: Option<String>,
    pub extrinsic_hash: String,
    pub included_block: Option<u64>,
    pub included_index: Option<u64>,
    pub dispatch: Option<String>,
    pub spec_version: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SigningStatus {
    pub state: String,
    pub reason: Option<String>,
    /// `ok`, `blocked_attempt` (the live attempt's evidence cannot be read
    /// yet), `backing_off` (RPC errors), `failing` (a subject is backing
    /// off), `diagnostic` (a subject settled) or `paused`/`fault`.
    pub health: String,
    /// D5: attempts settled as `consumed_unverified` (newest first): their
    /// nonce was consumed in `block`, whose events hold no commit of this
    /// hotkey and whose body cannot be read. Their epochs are never signed
    /// again.
    pub attempts_unverified: Vec<UnverifiedAttempt>,
    pub hotkey: String,
    pub nonce_floor: u64,
    pub runtime: Option<(u32, u32)>,
    pub runtime_block: Option<u64>,
    /// Stored runtimes: (spec, tx, block fetched at).
    pub runtimes: Vec<(u64, u64, u64)>,
    pub backoff_ms: Option<u64>,
    pub rpc_failures: u64,
    pub last_error: Option<String>,
    pub last_error_age_ms: Option<u64>,
    pub failures: Vec<FailureStatus>,
    pub sign_rpc_this_minute: u64,
    pub target: Option<String>,
    /// The first epoch of the chain record the latest restart installed
    /// ([`restart_target`]): no bundle from before it installs.
    pub restarted_from_epoch: Option<u64>,
    /// The highest epoch an installed bundle was issued or opened at: a
    /// restart's `from_epoch` must exceed it.
    pub record_epoch: Option<u64>,
    /// The installed authorization, its revocation state and the pointer's
    /// last confirmed write ([`authority::view`]).
    pub authorization: Option<authority::AuthorizationView>,
    /// The signed coordinator row other validators copy, and its last
    /// signing failure ([`rows::view`]).
    pub row_record: Option<rows::RowRecordView>,
    /// Automatic target handoff: the closed cohorts handled, the next one
    /// waiting and the last failure ([`auto::view`]).
    pub auto_target: auto::AutoView,
    pub schedule_block: Option<u64>,
    pub claim: Option<claims::ClaimStatus>,
    pub ops: Vec<OpStatus>,
    pub drills: Vec<(String, u64)>,
    pub log: Vec<serde_json::Value>,
}

fn opt_u(r: &crate::sql::Row, i: usize) -> Option<u64> {
    match r.get(i) {
        Some(SqlValue::Integer(v)) => Some(*v as u64),
        _ => None,
    }
}

fn opt_t(r: &crate::sql::Row, i: usize) -> Option<String> {
    match r.get(i) {
        Some(SqlValue::Text(t)) => Some(t.clone()),
        _ => None,
    }
}

/// Content-free signing status: public chain identifiers, states and counts.
/// No signed bytes, ciphertexts or secrets.
pub fn status(
    sql: &impl Sql,
    cfg: &SignConfig,
    ops_limit: u64,
    now_ms: u64,
) -> Result<SigningStatus, StoreError> {
    let s = signer(sql)?;
    let mut ops = Vec::new();
    for r in sql.exec(
        "SELECT op_id, epoch, state, detail, preflight_block, uid, row_json, reveal_round, predicted_reveal_block,
                commit_block, commit_block_hash, commit_index, readback, readback_block, reveal_block,
                reveal_block_hash, reveal_weights_match, diagnostic,
                json_extract(preflight_json, '$.commit_reveal_version')
         FROM commit_ops ORDER BY epoch DESC LIMIT ?",
        &[bind_u64(ops_limit)?],
    )? {
        let op_id = text(&r, 0)?.to_owned();
        let attempts = sql
            .exec(
                "SELECT attempt, nonce, era_birth, expiry, state, sends, submit_detail, extrinsic_hash,
                        included_block, included_index, dispatch, spec_version
                 FROM sign_attempts WHERE op_id = ? ORDER BY attempt",
                &[op_id.as_str().into()],
            )?
            .iter()
            .map(|a| {
                Ok(AttemptStatus {
                    attempt: uint(a, 0)?,
                    nonce: uint(a, 1)?,
                    era_birth: uint(a, 2)?,
                    expiry: uint(a, 3)?,
                    state: text(a, 4)?.to_owned(),
                    sends: uint(a, 5)?,
                    submit_detail: opt_t(a, 6),
                    extrinsic_hash: text(a, 7)?.to_owned(),
                    included_block: opt_u(a, 8),
                    included_index: opt_u(a, 9),
                    dispatch: opt_t(a, 10),
                    spec_version: uint(a, 11)?,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        ops.push(OpStatus {
            op_id,
            epoch: uint(&r, 1)?,
            state: text(&r, 2)?.to_owned(),
            detail: opt_t(&r, 3),
            inclusion: (opt_t(&r, 3).as_deref() == Some(INCLUSION_UNVERIFIABLE)).then_some("unknown"),
            preflight_block: opt_u(&r, 4),
            uid: opt_u(&r, 5),
            row: opt_t(&r, 6).and_then(|j| serde_json::from_str(&j).ok()),
            reveal_round: opt_u(&r, 7),
            predicted_reveal_block: opt_u(&r, 8),
            commit_block: opt_u(&r, 9),
            commit_block_hash: opt_t(&r, 10),
            commit_index: opt_u(&r, 11),
            readback: opt_t(&r, 12),
            readback_block: opt_u(&r, 13),
            reveal_block: opt_u(&r, 14),
            reveal_block_hash: opt_t(&r, 15),
            reveal_weights_match: opt_u(&r, 16).map(|v| v == 1),
            diagnostic: opt_t(&r, 17),
            commit_reveal_tag: opt_u(&r, 18).and_then(|t| u16::try_from(t).ok()),
            attempts,
        });
    }
    let drills = sql
        .exec(
            "SELECT name, remaining FROM sign_drills WHERE remaining > 0 ORDER BY name",
            &[],
        )?
        .iter()
        .map(|r| Ok((text(r, 0)?.to_owned(), uint(r, 1)?)))
        .collect::<Result<Vec<_>, StoreError>>()?;
    let log = sql
        .exec(
            "SELECT seq, at_ms, block_number, op_id, attempt, event, detail FROM sign_log ORDER BY seq DESC LIMIT ?",
            &[bind_u64(STATUS_LOG)?],
        )?
        .iter()
        .map(|r| {
            Ok(serde_json::json!({
                "seq": uint(r, 0)?, "age_ms": now_ms.saturating_sub(uint(r, 1)?), "block": opt_u(r, 2),
                "op": opt_t(r, 3).map(|o| o.chars().rev().take(12).collect::<String>().chars().rev().collect::<String>()),
                "attempt": opt_u(r, 4), "event": text(r, 5)?, "detail": opt_t(r, 6),
            }))
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let failures = sql
        .exec(
            "SELECT subject, failures, code, retry_block, settled, last_at_ms FROM sign_failures
             ORDER BY last_at_ms DESC LIMIT 8",
            &[],
        )?
        .iter()
        .map(|r| {
            Ok(FailureStatus {
                subject: short_subject(text(r, 0)?),
                failures: uint(r, 1)?,
                code: text(r, 2)?.to_owned(),
                retry_block: uint(r, 3)?,
                settled: int(r, 4)? == 1,
                age_ms: now_ms.saturating_sub(uint(r, 5)?),
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let runtimes = sql
        .exec(
            "SELECT spec_version, tx_version, block_number FROM runtime_cache ORDER BY block_number",
            &[],
        )?
        .iter()
        .map(|r| Ok((uint(r, 0)?, uint(r, 1)?, uint(r, 2)?)))
        .collect::<Result<Vec<_>, StoreError>>()?;
    let backoff_ms = s
        .backoff_until_ms
        .filter(|t| *t > now_ms)
        .map(|t| t - now_ms);
    let health = if s.reason.as_deref().is_some_and(|r| r.starts_with("fault:")) {
        "fault"
    } else if s.state != "active" {
        "paused"
    } else if failures.iter().any(|f| !f.settled && f.subject.starts_with("attempt:")) {
        // D5: the live attempt's own evidence cannot be read yet; it
        // blocks the next commit until it settles, by evidence only.
        "blocked_attempt"
    } else if backoff_ms.is_some() {
        "backing_off"
    } else if failures.iter().any(|f| !f.settled) {
        "failing"
    } else if failures.iter().any(|f| f.settled) {
        "diagnostic"
    } else {
        "ok"
    };
    let sign_rpc_this_minute =
        if now_ms.saturating_sub(s.budget_window_ms) >= claims::BUDGET_WINDOW_MS {
            0
        } else {
            s.budget_used
        };
    let attempts_unverified = sql
        .exec(
            "SELECT op_id, attempt, nonce, search_hi FROM sign_attempts WHERE state = 'consumed_unverified'
             ORDER BY resolved_at_ms DESC LIMIT 8",
            &[],
        )?
        .iter()
        .map(|r| {
            Ok(UnverifiedAttempt {
                op: short_op(text(r, 0)?),
                attempt: uint(r, 1)?,
                nonce: uint(r, 2)?,
                block: match r.get(3) {
                    Some(SqlValue::Integer(b)) => Some(*b as u64),
                    _ => None,
                },
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    Ok(SigningStatus {
        attempts_unverified,
        health: health.to_owned(),
        runtime_block: s.runtime_block,
        runtimes,
        backoff_ms,
        rpc_failures: s.rpc_failures,
        last_error: s.last_error,
        last_error_age_ms: s.last_error_at_ms.map(|t| now_ms.saturating_sub(t)),
        failures,
        sign_rpc_this_minute,
        state: s.state,
        reason: s.reason,
        hotkey: chain::ss58(cfg.hotkey),
        nonce_floor: s.nonce_floor,
        runtime: s.runtime,
        target: sql
            .exec("SELECT target FROM sign_targets WHERE active = 1", &[])?
            .first()
            .map(|r| text(r, 0).map(str::to_owned))
            .transpose()?,
        restarted_from_epoch: restart_floor(sql)?,
        record_epoch: record_epoch(sql)?,
        authorization: authority::view(sql, now_ms)?,
        row_record: rows::view(sql, now_ms)?,
        auto_target: auto::view(sql, now_ms)?,
        schedule_block: sql
            .exec("SELECT block_number FROM chain_schedule WHERE id = 1", &[])?
            .first()
            .map(|r| uint(r, 0))
            .transpose()?,
        claim: claims::status(sql, SIGNER_CLAIM, now_ms)?,
        ops,
        drills,
        log,
    })
}

pub mod authority;
pub mod auto;
pub mod decide;
pub mod registry;
pub mod rows;
pub use authority::{
    PointerOutcome, PointerPlan, Revoked, install_revocation, pointer_object, pointer_outcome, pointer_plan, pointer_written,
};

/// The installed target's digest and stored bundle, if any.
fn signer_target_bundle(sql: &impl Sql) -> Result<Option<(String, String)>, StoreError> {
    let rows = sql.exec("SELECT target, bundle FROM sign_targets WHERE active = 1", &[])?;
    rows.first()
        .map(|r| Ok((text(r, 0)?.to_owned(), text(r, 1)?.to_owned())))
        .transpose()
}

#[cfg(test)]
mod tests;
