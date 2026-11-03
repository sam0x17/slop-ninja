//! The cohort engine: durable phases, idempotent claims and resumable steps,
//! as storage-neutral functions over [`Sql`] (each call runs inside one
//! Durable Object transaction; `cf` performs the I/O between them).
//!
//! Phases: `queued -> admission -> generating -> sealing -> scoring -> packaging
//! -> transition -> closed`, or `abandoned` by an explicit operator action on a
//! faulted cohort. Boundary-driven moves (open, end of admission, close) happen
//! only on increments of the subnet epoch index observed at RPC-confirmed blocks
//! ([`observe`]); the phase minimums count active tempos, which advance only on
//! increments observed while heartbeats are fresh, so tempos that pass during
//! silence never count. Work-driven moves happen when the last piece of a
//! phase's work commits. A fault pauses the cohort with its evidence intact;
//! time passing never skips or expires it.
//!
//! Work is claimed with a lease. A claim that expires (its invocation died) is
//! taken again; every scoring attempt number allocated but never committed is
//! recorded as an infrastructure-error receipt by the next commit, so every
//! inference attempt has a receipt. A scorer request that provably never
//! reached the model returns its attempt number unused.
//!
//! Bulk work (generation, scoring, receipt bundles, score summaries) runs as
//! fan-out lanes ([`crate::eval::lanes`]): this object plans the lanes, records
//! each lane's assignment before its wakeup is sent, and remains the only
//! authority. Lanes claim and commit through it; root steps (draw, seal,
//! package) and signing stay here, inside phase transactions.

use crate::eval::chain::EpochObservation;
use crate::eval::cost;
use crate::eval::textgen::{self, Prompt, Reject};
use crate::eval::package::{self, JobScore, PackageInputs};
use crate::eval::plan::{ArtifactJson, Ceilings, CohortPlan, LoadPlan};
use crate::eval::pool::{self, Draw};
use crate::eval::receipts;
use crate::eval::score::{self, TaskResult};
use crate::sql::{Row, Sql, SqlValue, StoreError, bind_u64, int, text, uint};
use crate::wire::{BlockRef, Id};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use slop_ninja_protocol::{
    ChainBinding, CohortManifest, Digest, ReceiptIndex, ReceiptIndexEntry, Record, ScorePackage,
    CalculationTarget, OutcomeClass,
};
use std::collections::{BTreeMap, BTreeSet};

/// Model answers (accepted or rejected for content) per item before it is
/// dropped from the batch. Outages (transport failures, or no output at all)
/// never count: they back off and, past [`GEN_INFRA_MAX`], pause the cohort.
pub const MAX_GEN_ATTEMPTS: i64 = 4;
/// Consecutive outage failures of one item that pause the cohort
/// (`generation_unavailable`): with the backoff below, about 10 minutes.
pub const GEN_INFRA_MAX: i64 = 10;
const GEN_BACKOFF_BASE_MS: u64 = 5_000;
const GEN_BACKOFF_MAX_MS: u64 = 120_000;
/// Consecutive scorer failures that never reached the model (a slot that does
/// not start or load, a refused batch) that pause the cohort (`scorer_unavailable`).
pub const SCORER_FAILURES_MAX: i64 = 6;
/// Backoff of a chunk whose request never reached the model.
pub const SCORER_BACKOFF_MS: u64 = 30_000;
/// A heartbeat at most this old counts the coordinator as observing: an epoch
/// increment then credits an active tempo, and the alarm re-arms itself.
pub const HEARTBEAT_FRESH_MS: u64 = 600_000;
/// The active-tempo rule: each observed increase of the epoch index credits one
/// active tempo if the previous heartbeat is fresh, else none. A jump of several
/// tempos credits at most one; missed tempos are never charged.
pub const ACTIVE_TEMPO_RULE: &str = "fresh_heartbeat_single_increment_v1";
/// Upper bound on prompt tokens assumed when checking a wave against the budget.
pub(crate) const BUDGET_PROMPT_TOKENS: u64 = 1500;

pub const SCHEMA_V2: &[&str] = &[
    "DROP TABLE cohort_state",
    "CREATE TABLE eval_obs (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        kick_ms INTEGER NOT NULL, kicks INTEGER NOT NULL,
        confirmed_number INTEGER, confirmed_hash TEXT,
        epoch_index INTEGER, epoch_block INTEGER, epoch_block_hash TEXT, last_epoch_block INTEGER,
        epoch_ms INTEGER, epoch_regressions INTEGER NOT NULL DEFAULT 0
    ) STRICT",
    "INSERT INTO eval_obs (id, kick_ms, kicks) VALUES (1, 0, 0)",
    "CREATE TABLE cohorts (
        seq INTEGER PRIMARY KEY, label TEXT NOT NULL UNIQUE, cohort INTEGER NOT NULL UNIQUE,
        plan TEXT NOT NULL, plan_sha256 TEXT NOT NULL,
        phase TEXT NOT NULL CHECK (phase IN ('queued','admission','generating','sealing','scoring','packaging','transition','closed')),
        phase_since_ms INTEGER NOT NULL, queued_ms INTEGER NOT NULL, queued_epoch INTEGER,
        opened_epoch INTEGER, opened_block INTEGER, opened_ms INTEGER, packaged_epoch INTEGER, closed_epoch INTEGER,
        manifest_digest TEXT, evaluation_set TEXT, batch_index_sha256 TEXT, task_count INTEGER,
        receipt_root TEXT, receipt_count INTEGER, package_digest TEXT, target_digest TEXT,
        fault TEXT, gen_cost_microusd INTEGER NOT NULL DEFAULT 0,
        steps INTEGER NOT NULL DEFAULT 0, step_ms INTEGER NOT NULL DEFAULT 0, r2_puts INTEGER NOT NULL DEFAULT 0,
        draw TEXT
    ) STRICT",
    "CREATE TABLE phase_log (
        seq INTEGER NOT NULL, phase TEXT NOT NULL, at_ms INTEGER NOT NULL,
        confirmed_number INTEGER, epoch_index INTEGER, note TEXT
    ) STRICT",
    "CREATE TABLE pool_used (
        row_id TEXT PRIMARY KEY, text_sha256 TEXT NOT NULL UNIQUE, source_group TEXT NOT NULL, seq INTEGER NOT NULL
    ) STRICT",
    "CREATE TABLE pool_groups_used (source_group TEXT PRIMARY KEY, seq INTEGER NOT NULL) STRICT",
    "CREATE TABLE docs (
        seq INTEGER NOT NULL, idx INTEGER NOT NULL, key TEXT NOT NULL, kind TEXT NOT NULL,
        domain TEXT NOT NULL, source_group TEXT NOT NULL, parent INTEGER,
        status TEXT NOT NULL CHECK (status IN ('pending','ready','failed')),
        attempts INTEGER NOT NULL DEFAULT 0, claim_until_ms INTEGER NOT NULL DEFAULT 0,
        generator TEXT, text TEXT, text_sha256 TEXT, share_ppb INTEGER, span_start INTEGER, span_end INTEGER,
        words_total INTEGER, words_ai INTEGER, spec TEXT, rejects TEXT NOT NULL DEFAULT '', task INTEGER,
        PRIMARY KEY (seq, idx)
    ) STRICT",
    "CREATE TABLE gen_usage (
        seq INTEGER NOT NULL, model TEXT NOT NULL, calls INTEGER NOT NULL, accepted INTEGER NOT NULL,
        rejected INTEGER NOT NULL, errors INTEGER NOT NULL, prompt_tokens INTEGER NOT NULL,
        completion_tokens INTEGER NOT NULL, cost_microusd INTEGER NOT NULL, ms INTEGER NOT NULL,
        PRIMARY KEY (seq, model)
    ) STRICT",
    "CREATE TABLE jobs (
        seq INTEGER NOT NULL, job INTEGER NOT NULL, role TEXT NOT NULL, artifact TEXT NOT NULL,
        package_sha256 TEXT NOT NULL, manifest_key TEXT NOT NULL, owner TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('pending','running','done')),
        chunks INTEGER NOT NULL, done_chunks INTEGER NOT NULL DEFAULT 0, started_ms INTEGER, finished_ms INTEGER,
        call_ms INTEGER NOT NULL DEFAULT 0, server_ms INTEGER NOT NULL DEFAULT 0, load_ms INTEGER NOT NULL DEFAULT 0,
        cpu_ms INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (seq, job)
    ) STRICT",
    "CREATE TABLE chunks (
        seq INTEGER NOT NULL, job INTEGER NOT NULL, chunk INTEGER NOT NULL,
        first_task INTEGER NOT NULL, count INTEGER NOT NULL,
        attempt INTEGER NOT NULL DEFAULT 0, inflight INTEGER NOT NULL DEFAULT 0,
        claim_until_ms INTEGER NOT NULL DEFAULT 0,
        status TEXT NOT NULL CHECK (status IN ('pending','done')),
        PRIMARY KEY (seq, job, chunk)
    ) STRICT",
    "CREATE TABLE preds (
        seq INTEGER NOT NULL, job INTEGER NOT NULL, task INTEGER NOT NULL, attempt INTEGER NOT NULL,
        outcome INTEGER NOT NULL, inv_ppb INTEGER, share_ppb INTEGER, human_profile TEXT, ai_profile TEXT,
        token_count INTEGER, PRIMARY KEY (seq, job, task)
    ) STRICT",
    "CREATE TABLE receipts (
        seq INTEGER NOT NULL, attempt_id TEXT NOT NULL, job INTEGER NOT NULL, task INTEGER NOT NULL,
        number INTEGER NOT NULL, outcome INTEGER NOT NULL, object_sha256 TEXT NOT NULL,
        object_len INTEGER NOT NULL, object_key TEXT NOT NULL, PRIMARY KEY (seq, attempt_id)
    ) STRICT",
    "CREATE TABLE receipt_objects (
        seq INTEGER NOT NULL, key TEXT NOT NULL, sha256 TEXT NOT NULL, bytes INTEGER NOT NULL,
        receipts INTEGER NOT NULL, PRIMARY KEY (seq, key)
    ) STRICT",
    "CREATE TABLE eval_targets (
        seq INTEGER PRIMARY KEY, target_digest TEXT NOT NULL, activated_epoch INTEGER NOT NULL
    ) STRICT",
];

/// v3: the active-tempo clock, the `abandoned` phase, outage counters, and
/// per-chunk claim fences with the highest attempt whose receipts committed.
pub const SCHEMA_V3: &[&str] = &[
    "ALTER TABLE eval_obs ADD COLUMN active_tempos INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE eval_obs ADD COLUMN uncredited_tempos INTEGER NOT NULL DEFAULT 0",
    "CREATE TABLE cohorts_v3 (
        seq INTEGER PRIMARY KEY, label TEXT NOT NULL UNIQUE, cohort INTEGER NOT NULL UNIQUE,
        plan TEXT NOT NULL, plan_sha256 TEXT NOT NULL,
        phase TEXT NOT NULL CHECK (phase IN ('queued','admission','generating','sealing','scoring','packaging','transition','closed','abandoned')),
        phase_since_ms INTEGER NOT NULL, queued_ms INTEGER NOT NULL, queued_epoch INTEGER,
        opened_epoch INTEGER, opened_block INTEGER, opened_ms INTEGER, packaged_epoch INTEGER, closed_epoch INTEGER,
        manifest_digest TEXT, evaluation_set TEXT, batch_index_sha256 TEXT, task_count INTEGER,
        receipt_root TEXT, receipt_count INTEGER, package_digest TEXT, target_digest TEXT,
        fault TEXT, gen_cost_microusd INTEGER NOT NULL DEFAULT 0,
        steps INTEGER NOT NULL DEFAULT 0, step_ms INTEGER NOT NULL DEFAULT 0, r2_puts INTEGER NOT NULL DEFAULT 0,
        draw TEXT,
        opened_active INTEGER, packaged_active INTEGER, scorer_failures INTEGER NOT NULL DEFAULT 0,
        counts TEXT, abandoned_reason TEXT, abandoned_fault TEXT
    ) STRICT",
    "INSERT INTO cohorts_v3 (seq, label, cohort, plan, plan_sha256, phase, phase_since_ms, queued_ms, queued_epoch,
        opened_epoch, opened_block, opened_ms, packaged_epoch, closed_epoch, manifest_digest, evaluation_set,
        batch_index_sha256, task_count, receipt_root, receipt_count, package_digest, target_digest, fault,
        gen_cost_microusd, steps, step_ms, r2_puts, draw)
     SELECT seq, label, cohort, plan, plan_sha256, phase, phase_since_ms, queued_ms, queued_epoch,
        opened_epoch, opened_block, opened_ms, packaged_epoch, closed_epoch, manifest_digest, evaluation_set,
        batch_index_sha256, task_count, receipt_root, receipt_count, package_digest, target_digest, fault,
        gen_cost_microusd, steps, step_ms, r2_puts, draw FROM cohorts",
    "DROP TABLE cohorts",
    "ALTER TABLE cohorts_v3 RENAME TO cohorts",
    // A cohort open at the upgrade counts its minimums from here (none was open on testnet).
    "UPDATE cohorts SET opened_active = 0 WHERE opened_epoch IS NOT NULL AND phase != 'closed'",
    "UPDATE cohorts SET packaged_active = 0 WHERE packaged_epoch IS NOT NULL AND phase != 'closed'",
    "ALTER TABLE docs ADD COLUMN infra_failures INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE chunks ADD COLUMN recorded INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE chunks ADD COLUMN claims INTEGER NOT NULL DEFAULT 0",
    // v2 recorded at most one lost attempt (the in-flight one) per chunk.
    "UPDATE chunks SET recorded = CASE WHEN status = 'done' OR inflight = 0 THEN attempt ELSE inflight - 1 END",
];

/// v4 (fan-out): the lane table and its event samples, the batch shard table
/// lanes read texts through, chunk holders and claim generations, crash and
/// reconciliation bookkeeping, generation fences, backoff, throttling and
/// budget reservations, job-scoped faults, summaries and bundles, and the
/// load instance's hold flag.
pub const SCHEMA_V4: &[&str] = &[
    "ALTER TABLE eval_obs ADD COLUMN hold INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE gen_usage ADD COLUMN throttled INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE gen_usage ADD COLUMN fenced INTEGER NOT NULL DEFAULT 0",
    "CREATE TABLE jobs_v4 (
        seq INTEGER NOT NULL, job INTEGER NOT NULL, role TEXT NOT NULL, artifact TEXT NOT NULL,
        package_sha256 TEXT NOT NULL, manifest_key TEXT NOT NULL, owner TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('pending','running','done','faulted')),
        chunks INTEGER NOT NULL, done_chunks INTEGER NOT NULL DEFAULT 0, started_ms INTEGER, finished_ms INTEGER,
        call_ms INTEGER NOT NULL DEFAULT 0, server_ms INTEGER NOT NULL DEFAULT 0, load_ms INTEGER NOT NULL DEFAULT 0,
        cpu_ms INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0,
        fault TEXT, isolated_crashes INTEGER NOT NULL DEFAULT 0,
        summary_key TEXT, summary_sha256 TEXT, summary_count INTEGER,
        bundle_key TEXT, bundle_sha256 TEXT, bundle_bytes INTEGER, bundle_receipts INTEGER,
        PRIMARY KEY (seq, job)
    ) STRICT",
    "INSERT INTO jobs_v4 (seq, job, role, artifact, package_sha256, manifest_key, owner, status, chunks, done_chunks,
        started_ms, finished_ms, call_ms, server_ms, load_ms, cpu_ms, attempts)
     SELECT seq, job, role, artifact, package_sha256, manifest_key, owner, status, chunks, done_chunks,
        started_ms, finished_ms, call_ms, server_ms, load_ms, cpu_ms, attempts FROM jobs",
    "DROP TABLE jobs",
    "ALTER TABLE jobs_v4 RENAME TO jobs",
    "ALTER TABLE chunks ADD COLUMN holder TEXT",
    "ALTER TABLE chunks ADD COLUMN claim_gen INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE chunks ADD COLUMN reconcile_checked INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE chunks ADD COLUMN crashes INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE chunks ADD COLUMN claimed_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE docs ADD COLUMN generator_next TEXT",
    "ALTER TABLE docs ADD COLUMN claim_gen INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE docs ADD COLUMN holder TEXT",
    "ALTER TABLE docs ADD COLUMN not_before_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE docs ADD COLUMN throttles INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE docs ADD COLUMN throttle_since_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE docs ADD COLUMN reserved_microusd INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN gen_reserved_microusd INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN gen_started_ms INTEGER",
    "ALTER TABLE cohorts ADD COLUMN scoring_started_ms INTEGER",
    "ALTER TABLE cohorts ADD COLUMN scoring_finished_ms INTEGER",
    "ALTER TABLE cohorts ADD COLUMN prestarted_ms INTEGER",
    "ALTER TABLE cohorts ADD COLUMN reserved_microusd INTEGER",
    "ALTER TABLE cohorts ADD COLUMN settled_microusd INTEGER",
    "CREATE TABLE lanes (
        seq INTEGER NOT NULL, kind TEXT NOT NULL CHECK (kind IN ('score','gen','summary','bundle')),
        lane INTEGER NOT NULL, name TEXT NOT NULL, job INTEGER, lane_index INTEGER NOT NULL DEFAULT 0,
        chunk_lo INTEGER NOT NULL DEFAULT 0, chunk_hi INTEGER NOT NULL DEFAULT 0,
        gen INTEGER NOT NULL DEFAULT 0,
        status TEXT NOT NULL CHECK (status IN ('pending','kicked','starting','running','waiting','paused','done','failed')),
        kicked_ms INTEGER NOT NULL DEFAULT 0, seen_ms INTEGER NOT NULL DEFAULT 0, first_seen_ms INTEGER NOT NULL DEFAULT 0,
        kicks INTEGER NOT NULL DEFAULT 0, rekicks INTEGER NOT NULL DEFAULT 0, starts INTEGER NOT NULL DEFAULT 0,
        start_failures INTEGER NOT NULL DEFAULT 0, chunks INTEGER NOT NULL DEFAULT 0, lost INTEGER NOT NULL DEFAULT 0,
        reconciled INTEGER NOT NULL DEFAULT 0, fenced INTEGER NOT NULL DEFAULT 0,
        active_ms INTEGER NOT NULL DEFAULT 0, cpu_ms INTEGER NOT NULL DEFAULT 0, do_ms INTEGER NOT NULL DEFAULT 0,
        container_up INTEGER NOT NULL DEFAULT 0, final INTEGER NOT NULL DEFAULT 0, prestart INTEGER NOT NULL DEFAULT 0,
        note TEXT,
        PRIMARY KEY (seq, kind, lane)
    ) STRICT",
    "CREATE TABLE lane_events (
        seq INTEGER NOT NULL, kind TEXT NOT NULL, lane INTEGER NOT NULL, event TEXT NOT NULL,
        ms INTEGER NOT NULL, at_ms INTEGER NOT NULL
    ) STRICT",
    "CREATE INDEX lane_events_metric ON lane_events (seq, event, ms)",
    "CREATE TABLE batch_shards (
        seq INTEGER NOT NULL, shard INTEGER NOT NULL, key TEXT NOT NULL, sha256 TEXT NOT NULL,
        first_task INTEGER NOT NULL, docs INTEGER NOT NULL, PRIMARY KEY (seq, shard)
    ) STRICT",
    "CREATE INDEX chunks_claim ON chunks (seq, job, status, claim_until_ms)",
    "CREATE INDEX docs_gen ON docs (seq, status, generator_next, not_before_ms)",
    "CREATE INDEX docs_task ON docs (seq, task)",
];

/// v5: an index for a job's receipt rows (bundle lanes page through them).
pub const SCHEMA_V5: &[&str] = &["CREATE INDEX receipts_job ON receipts (seq, job)"];

/// v6: when a finished cohort's private results objects and summaries were
/// deleted from the eval bucket.
pub const SCHEMA_V6: &[&str] = &["ALTER TABLE cohorts ADD COLUMN purged_ms INTEGER"];

/// v7: resumable crash isolation and artifact attribution.
/// - `chunks.claim_kind`: what the current claim is (`fresh`, `continue`,
///   `reconcile`, `record`), so a claim handed back before dispatch returns its
///   attempt number unused only when it was a fresh attempt.
/// - `chunks.iso_yield`: an isolation attempt handed back unfinished (its step
///   budget ran out, or its instance could not be restarted); the next claim
///   continues the same attempt.
/// - `iso_done`: the outcomes an isolation attempt has resolved so far, one row
///   per document, so a continuation never recomputes a durable answer.
/// - `jobs.artifact_failures` and `jobs.artifact_failed`: consecutive
///   attributable artifact failures on fresh instances, and the code that made
///   an entrant's unresolved documents model errors.
pub const SCHEMA_V7: &[&str] = &[
    "ALTER TABLE chunks ADD COLUMN claim_kind TEXT NOT NULL DEFAULT 'none'",
    "ALTER TABLE chunks ADD COLUMN iso_yield INTEGER NOT NULL DEFAULT 0",
    "CREATE TABLE iso_done (
        seq INTEGER NOT NULL, job INTEGER NOT NULL, chunk INTEGER NOT NULL, attempt INTEGER NOT NULL,
        task INTEGER NOT NULL, outcome TEXT NOT NULL, PRIMARY KEY (seq, job, chunk, attempt, task)
    ) STRICT",
    "ALTER TABLE jobs ADD COLUMN artifact_failures INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE jobs ADD COLUMN artifact_failed TEXT",
];

/// Fault codes an operator clear can resume, because the frozen plan still
/// allows the work: outages and refused scorer slots. Every other fault is
/// terminal under the plan (no attempt, budget, human or case is left) and
/// needs [`abandon`].
/// `package_refused` resumes once a corrected build is deployed: the frozen
/// inputs are unchanged and packaging runs again from them.
/// A plan names fresh stock without an admitted external entrant.
pub const FRESH_STOCK_REFUSED: &str =
    "pool (fresh stock is drawn only for a cohort with an admitted external entrant; list exposed pools in EXPOSED_POOLS)";

/// The draw's fault code for a fresh pool without an admitted outside entrant.
pub const FRESH_STOCK_REFUSED_CODE: &str = "fresh_stock_refused";

pub const RESUMABLE_FAULTS: &[&str] = &[
    "generation_unavailable",
    "scorer_unavailable",
    "package_refused",
    crate::eval::commitments::SNAPSHOT_DISCARDED,
    crate::eval::admission::FRAMING_UNAVAILABLE,
    crate::eval::commitments::STATE_UNAVAILABLE,
];

/// The crash-isolation rule, as published in `components.json`.
pub const CRASH_RULE: &str = "crash isolation: after a request kills its container (the container stops or stops \
answering, or a single-document request outlasts fanout.score.doc_timeout_ms), each unresolved document runs \
alone; a document that kills its instance is run again alone on a freshly restarted instance that has just scored \
a control document, and is a model_error for that job's document only if this second run also kills the instance. \
A failed request whose container stays up is an infrastructure error, a refused or undispatched request uses no \
attempt, and an isolation attempt cut short by the lane's step budget continues on the next claim";

/// The artifact-attribution rule, as published in `components.json`.
pub const ARTIFACT_RULE: &str = "an entrant's unresolved documents are model_errors after \
fanout.score.artifact_failures_before_model_error consecutive artifact failures on fresh instances (a load that \
fails or outlasts its allowance, or a control document that kills a freshly restarted instance) with no prediction \
of that job committed in between; an artifact manifest over fanout.score.artifact_max_mib or not matching its \
package digest is one at once. Reference artifacts are never attributed: their failures pause the cohort \
(scorer_unavailable)";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvalError {
    Store(StoreError),
    Plan(&'static str),
    State(&'static str),
}

impl From<StoreError> for EvalError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl EvalError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Store(e) => e.0,
            Self::Plan(c) | Self::State(c) => c,
        }
    }
}

pub(crate) type R<T> = Result<T, EvalError>;

pub(crate) fn s(v: impl Into<String>) -> SqlValue {
    SqlValue::Text(v.into())
}

pub(crate) fn i(v: i64) -> SqlValue {
    SqlValue::Integer(v)
}

pub(crate) fn opt_int(row: &Row, idx: usize) -> Option<i64> {
    match row.get(idx) {
        Some(SqlValue::Integer(v)) => Some(*v),
        _ => None,
    }
}

pub(crate) fn opt_text(row: &Row, idx: usize) -> Option<String> {
    match row.get(idx) {
        Some(SqlValue::Text(v)) => Some(v.clone()),
        _ => None,
    }
}

pub(crate) fn parse_digest(s: &str) -> Option<Digest> {
    crate::eval::plan::parse_hex32(s).map(Digest)
}

pub(crate) fn sha_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// The authoritative instance, and the separate load-rehearsal instance.
pub const INSTANCE_MAIN: &str = "main";
pub const INSTANCE_LOAD: &str = "load";

/// Static configuration of one cohort authority.
#[derive(Clone, Debug)]
pub struct EvalConfig {
    pub chain: ChainBinding,
    /// Private 32-byte seed (Worker secret) from which every cohort seed derives.
    pub private_seed: [u8; 32],
    /// `main` (authoritative) or `load` (rehearsals only; never a target).
    pub instance: String,
    /// The deployment's ceiling on live scoring lanes: `MAX_SCORE_LANES`, which
    /// must equal the Container application's `max_instances`.
    pub max_score_lanes: u32,
    /// The deployment's roster cap for this instance.
    pub ceilings: Ceilings,
    /// Human pools the deployment lists as already exposed (`EXPOSED_POOLS`):
    /// testnet stock that operator-only cohorts may draw, and draw again.
    /// Any other pool is fresh stock, drawn only for a cohort with an
    /// admitted external entrant.
    pub exposed_pools: Vec<String>,
    /// The scorer build admission verdicts are bound to
    /// (`SCORER_BINARY_SHA256`); empty when the deployment pins none.
    pub scorer_binary: String,
    /// The main instance's authority (`EVAL_MAIN_AUTHORITY`): its lane names,
    /// probe keys, labels and first cohort number. The load instance is
    /// always the original authority's.
    pub authority: crate::config::Authority,
}

impl EvalConfig {
    pub fn is_load(&self) -> bool {
        self.instance == INSTANCE_LOAD
    }

    /// The scope of this instance's lane object names.
    pub fn lane_scope(&self) -> String {
        self.authority.lane_scope(&self.instance)
    }

    /// The authority a lane's calls go back to: `None` for the original
    /// authority (and the load instance), so kicks from it are the same bytes.
    pub fn kick_authority(&self) -> Option<String> {
        (!self.authority.is_original()).then(|| self.authority.name().to_owned())
    }
}

/// The cohort's private seed: ranks, spans, topics, generator rotation and salts.
pub fn cohort_seed(private: &[u8; 32], label: &str) -> [u8; 32] {
    let mut payload = private.to_vec();
    payload.extend_from_slice(label.as_bytes());
    slop_ninja_protocol::domain_hash("slop_ninja.eval/cohort-seed/v1", &payload).0
}

pub(crate) fn item_digest(seed: &[u8; 32], key: &str) -> [u8; 32] {
    pool::rank(seed, "item", key)
}

// ------------------------------------------------------------------ observation

/// What a heartbeat (delivery or tick) tells the cohort authority.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kick {
    pub confirmed: Option<BlockRef>,
    pub epoch: Option<EpochObservation>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Observed {
    pub confirmed: Option<BlockRef>,
    pub epoch: Option<EpochObservation>,
    pub kick_ms: u64,
    pub kicks: u64,
    /// Active tempos under [`ACTIVE_TEMPO_RULE`]; the phase minimums count these.
    pub active_tempos: u64,
    /// Epoch-index increments that passed unobserved or during silence.
    pub uncredited_tempos: u64,
}

pub fn observed(sql: &impl Sql) -> R<Observed> {
    let rows = sql.exec(
        "SELECT kick_ms, kicks, confirmed_number, confirmed_hash, epoch_index, epoch_block,
                epoch_block_hash, last_epoch_block, active_tempos, uncredited_tempos FROM eval_obs WHERE id = 1",
        &[],
    )?;
    let r = rows.first().ok_or(StoreError("eval_obs row"))?;
    let block = |n: Option<i64>, h: Option<String>| -> Option<BlockRef> {
        Some(BlockRef {
            number: u64::try_from(n?).ok()?,
            hash: Id::parse(&h?).ok()?,
        })
    };
    let confirmed = block(opt_int(r, 2), opt_text(r, 3));
    let epoch = match (block(opt_int(r, 5), opt_text(r, 6)), opt_int(r, 4), opt_int(r, 7)) {
        (Some(b), Some(index), Some(last)) => Some(EpochObservation {
            block: b,
            index: index as u64,
            last_epoch_block: last as u64,
        }),
        _ => None,
    };
    Ok(Observed {
        confirmed,
        epoch,
        kick_ms: uint(r, 0)?,
        kicks: uint(r, 1)?,
        active_tempos: uint(r, 8)?,
        uncredited_tempos: uint(r, 9)?,
    })
}

/// Records a heartbeat's observations (monotonic) and advances boundary phases.
///
/// An increase of the epoch index credits one active tempo when the previous
/// heartbeat arrived within [`HEARTBEAT_FRESH_MS`]; after silence it credits
/// none, and a jump of several tempos never credits more than one. The phase
/// minimums count active tempos, so coordinator silence consumes no scheduling
/// budget and recovery resumes at a later boundary without charging missed ones.
pub fn observe(sql: &impl Sql, kick: &Kick, now_ms: u64) -> R<Vec<String>> {
    let before = observed(sql)?;
    sql.exec(
        "UPDATE eval_obs SET kick_ms = ?, kicks = kicks + 1 WHERE id = 1",
        &[bind_u64(now_ms)?],
    )?;
    if let Some(c) = kick.confirmed
        && before.confirmed.is_none_or(|b| c.number > b.number)
    {
        sql.exec(
            "UPDATE eval_obs SET confirmed_number = ?, confirmed_hash = ? WHERE id = 1",
            &[bind_u64(c.number)?, s(c.hash.to_hex())],
        )?;
    }
    if let Some(e) = kick.epoch {
        match before.epoch {
            Some(b) if e.block.number <= b.block.number => {}
            Some(b) if e.index < b.index => {
                sql.exec("UPDATE eval_obs SET epoch_regressions = epoch_regressions + 1 WHERE id = 1", &[])?;
            }
            _ => {
                if let Some(b) = before.epoch
                    && e.index > b.index
                {
                    let fresh = before.kick_ms > 0 && now_ms.saturating_sub(before.kick_ms) <= HEARTBEAT_FRESH_MS;
                    let credit = u64::from(fresh);
                    sql.exec(
                        "UPDATE eval_obs SET active_tempos = active_tempos + ?,
                            uncredited_tempos = uncredited_tempos + ? WHERE id = 1",
                        &[bind_u64(credit)?, bind_u64(e.index - b.index - credit)?],
                    )?;
                }
                sql.exec(
                    "UPDATE eval_obs SET epoch_index = ?, epoch_block = ?, epoch_block_hash = ?,
                        last_epoch_block = ?, epoch_ms = ? WHERE id = 1",
                    &[
                        bind_u64(e.index)?,
                        bind_u64(e.block.number)?,
                        s(e.block.hash.to_hex()),
                        bind_u64(e.last_epoch_block)?,
                        bind_u64(now_ms)?,
                    ],
                )?;
            }
        }
    }
    advance(sql, now_ms)
}

// ------------------------------------------------------------------- cohorts

#[derive(Clone, Debug)]
pub struct Cohort {
    pub seq: i64,
    pub label: String,
    pub plan: CohortPlan,
    pub plan_sha256: String,
    pub phase: String,
    pub queued_epoch: Option<u64>,
    pub opened_epoch: Option<u64>,
    pub opened_block: Option<u64>,
    pub packaged_epoch: Option<u64>,
    pub manifest_digest: Option<String>,
    pub task_count: Option<u32>,
    pub fault: Option<String>,
    pub gen_cost_microusd: u64,
    pub opened_active: Option<u64>,
    pub packaged_active: Option<u64>,
    pub scorer_failures: i64,
}

/// Every column but the plan text, which is read and parsed once per plan
/// digest ([`plan_for`]): lanes reach the cohort object hundreds of times a
/// second, and a wide roster's plan is over 100 KB of JSON.
const COHORT_COLUMNS: &str = "seq, label, plan_sha256, plan_sha256, phase, queued_epoch, opened_epoch, opened_block,
    packaged_epoch, manifest_digest, task_count, fault, gen_cost_microusd, opened_active, packaged_active,
    scorer_failures";

/// Parsed plans kept in memory: the active cohorts of the main and load
/// instances and the next queued one of each. A miss re-reads and re-parses
/// the stored plan, so the bound changes only CPU, never results.
pub const PLAN_CACHE_PLANS: usize = 4;

thread_local! {
    /// Parsed stored plans by (seq, plan digest), most recent first. A stored
    /// plan never changes. The entries live in the isolate's home scope
    /// (the plans home, `arena_routes::home`), never in the invocation that
    /// parsed them.
    static PLANS: std::cell::RefCell<Vec<((i64, String), CohortPlan)>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn plan_for(sql: &impl Sql, seq: i64, sha: &str) -> R<CohortPlan> {
    let hit = PLANS.with(|m| m.borrow().iter().find(|((s, d), _)| *s == seq && d == sha).map(|(_, p)| p.clone()));
    if let Some(p) = hit {
        return Ok(p);
    }
    let rows = sql.exec("SELECT plan FROM cohorts WHERE seq = ?", &[i(seq)])?;
    let plan: CohortPlan = serde_json::from_str(text(rows.first().ok_or(EvalError::State("cohort"))?, 0)?)
        .map_err(|_| EvalError::State("stored plan"))?;
    // The cached copy and its slot are allocated in the home scope; a full
    // cache drops its oldest entry first (freed into the home scope too).
    let _ = crate::arena_routes::home::enter(crate::arena_routes::home::Home::Plans, || {
        PLANS.with(|m| {
            let mut m = m.borrow_mut();
            if m.capacity() < PLAN_CACHE_PLANS {
                let more = PLAN_CACHE_PLANS - m.len();
                m.reserve_exact(more);
            }
            if m.len() >= PLAN_CACHE_PLANS {
                m.pop();
            }
            m.insert(0, ((seq, sha.to_owned()), plan.clone()));
        })
    });
    Ok(plan)
}

fn cohort_from(sql: &impl Sql, r: &Row) -> R<Cohort> {
    let seq = int(r, 0)?;
    let plan = plan_for(sql, seq, text(r, 3)?)?;
    let u = |idx| opt_int(r, idx).map(|v| v as u64);
    Ok(Cohort {
        seq,
        label: text(r, 1)?.to_owned(),
        plan,
        plan_sha256: text(r, 3)?.to_owned(),
        phase: text(r, 4)?.to_owned(),
        queued_epoch: u(5),
        opened_epoch: u(6),
        opened_block: u(7),
        packaged_epoch: u(8),
        manifest_digest: opt_text(r, 9),
        task_count: opt_int(r, 10).map(|v| v as u32),
        fault: opt_text(r, 11),
        gen_cost_microusd: uint(r, 12)?,
        opened_active: u(13),
        packaged_active: u(14),
        scorer_failures: int(r, 15)?,
    })
}

pub fn cohorts(sql: &impl Sql) -> R<Vec<Cohort>> {
    sql.exec(&format!("SELECT {COHORT_COLUMNS} FROM cohorts ORDER BY seq"), &[])?
        .iter()
        .map(|r| cohort_from(sql, r))
        .collect()
}

pub(crate) fn cohort(sql: &impl Sql, seq: i64) -> R<Cohort> {
    let rows = sql.exec(&format!("SELECT {COHORT_COLUMNS} FROM cohorts WHERE seq = ?"), &[i(seq)])?;
    cohort_from(sql, rows.first().ok_or(EvalError::State("no such cohort"))?)
}

pub(crate) fn set_phase(sql: &impl Sql, seq: i64, phase: &str, now_ms: u64, note: &str) -> R<()> {
    sql.exec(
        "UPDATE cohorts SET phase = ?, phase_since_ms = ? WHERE seq = ?",
        &[s(phase), bind_u64(now_ms)?, i(seq)],
    )?;
    let o = observed(sql)?;
    sql.exec(
        "INSERT INTO phase_log (seq, phase, at_ms, confirmed_number, epoch_index, note) VALUES (?, ?, ?, ?, ?, ?)",
        &[
            i(seq),
            s(phase),
            bind_u64(now_ms)?,
            o.confirmed.map_or(SqlValue::Null, |c| i(c.number as i64)),
            o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)),
            s(note),
        ],
    )?;
    Ok(())
}

/// Pauses a cohort with a content-free fault code; its work and evidence stay.
pub fn fault(sql: &impl Sql, seq: i64, code: &str, now_ms: u64) -> R<()> {
    // Logged once: a cohort already faulted keeps its first fault and entry.
    if sql.exec("UPDATE cohorts SET fault = ? WHERE seq = ? AND fault IS NULL RETURNING seq", &[s(code), i(seq)])?.is_empty() {
        return Ok(());
    }
    let o = observed(sql)?;
    sql.exec(
        "INSERT INTO phase_log (seq, phase, at_ms, confirmed_number, epoch_index, note)
         SELECT seq, phase, ?, ?, ?, ? FROM cohorts WHERE seq = ?",
        &[
            bind_u64(now_ms)?,
            o.confirmed.map_or(SqlValue::Null, |c| i(c.number as i64)),
            o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)),
            s(format!("fault:{code}")),
            i(seq),
        ],
    )?;
    Ok(())
}

/// The outcome of an operator clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Cleared {
    pub cleared: bool,
    /// Why nothing was cleared: `no_fault`, or `abandon_required` for a fault
    /// the frozen plan cannot resume.
    pub refused: Option<&'static str>,
}

/// Operator action: clear a resumable fault so the cohort's work resumes where
/// it stopped, with fresh outage allowances. A terminal fault (attempts,
/// budget, humans or cases exhausted under the frozen plan) is refused: it would
/// fault again at once, so the cohort can only be abandoned.
pub fn clear_fault(sql: &impl Sql, label: &str, now_ms: u64) -> R<Cleared> {
    let rows = sql.exec("SELECT seq, fault FROM cohorts WHERE label = ? AND fault IS NOT NULL", &[s(label)])?;
    let Some(r) = rows.first() else {
        return Ok(Cleared { cleared: false, refused: Some("no_fault") });
    };
    let seq = int(r, 0)?;
    if !RESUMABLE_FAULTS.contains(&text(r, 1)?) {
        return Ok(Cleared { cleared: false, refused: Some("abandon_required") });
    }
    sql.exec("UPDATE cohorts SET fault = NULL, scorer_failures = 0 WHERE seq = ?", &[i(seq)])?;
    // Released claims are taken again with fresh outage allowances; no attempt
    // number, answer or receipt is reset, and nothing is skipped.
    sql.exec(
        "UPDATE docs SET claim_until_ms = 0, infra_failures = 0, not_before_ms = 0, throttle_since_ms = 0
         WHERE seq = ? AND status = 'pending'",
        &[i(seq)],
    )?;
    sql.exec("UPDATE chunks SET claim_until_ms = 0 WHERE seq = ? AND status = 'pending'", &[i(seq)])?;
    let c = cohort(sql, seq)?;
    set_phase(sql, seq, &c.phase, now_ms, "fault_cleared")?;
    Ok(Cleared { cleared: true, refused: None })
}

/// Operator action: end a faulted cohort without a target, so later cohorts can
/// open. `reason` is a short content-free code. The phase log, the used set
/// (drawn humans are never reused), the sealed batch in the eval bucket and
/// the cohort row with its receipt root and count stay; its detailed receipts
/// follow the N+2 rule like any other cohort's (`eval::retention`). Private
/// texts leave Durable Object storage as at a close. The calculation target
/// stays the previous cohort's.
pub fn abandon(sql: &impl Sql, label: &str, reason: &str, now_ms: u64) -> R<bool> {
    let code_ok = !reason.is_empty()
        && reason.len() <= 64
        && reason.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.:".contains(&b));
    if !code_ok {
        return Err(EvalError::Plan("abandon reason (a-z 0-9 _ - . :, at most 64)"));
    }
    // A faulted admission-phase cohort (a coordinator-admitted roster that
    // cannot freeze) is abandoned like a faulted work phase.
    let rows = sql.exec(
        "SELECT seq, fault FROM cohorts WHERE label = ? AND fault IS NOT NULL
           AND (phase IN ('generating','sealing','scoring','packaging')
                OR (phase = 'admission' AND json_extract(plan, '$.admission') IS NOT NULL))",
        &[s(label)],
    )?;
    let Some(r) = rows.first() else { return Ok(false) };
    let (seq, fault_code) = (int(r, 0)?, text(r, 1)?.to_owned());
    let o = observed(sql)?;
    sql.exec(
        "UPDATE cohorts SET abandoned_reason = ?, abandoned_fault = ?, fault = NULL, closed_epoch = ? WHERE seq = ?",
        &[s(reason), s(&fault_code), o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)), i(seq)],
    )?;
    sql.exec("UPDATE docs SET text = NULL, claim_until_ms = 0 WHERE seq = ?", &[i(seq)])?;
    sql.exec("UPDATE chunks SET claim_until_ms = 0 WHERE seq = ?", &[i(seq)])?;
    sql.exec("DELETE FROM iso_done WHERE seq = ?", &[i(seq)])?;
    crate::eval::admission::abandoned(sql, seq, reason, now_ms)?;
    // A later cohort opens at the next boundary, not retroactively at this one.
    if let Some(e) = o.epoch {
        sql.exec(
            "UPDATE cohorts SET queued_epoch = MAX(COALESCE(queued_epoch, 0), ?) WHERE phase = 'queued'",
            &[bind_u64(e.index)?],
        )?;
    }
    set_phase(sql, seq, "abandoned", now_ms, &format!("abandoned:{reason} after fault:{fault_code}; no target"))?;
    Ok(true)
}

/// What a plan costs at most, checked before it is queued: the plan's
/// generation and scoring budgets are reserved in the deployment's monthly
/// ledger, and the scoring budget must cover the plan's own upper bound.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Admission {
    pub label: String,
    pub reserve_microusd: u64,
    pub scoring_upper_bound_microusd: u64,
    pub score_lanes: u32,
}

/// Parses and checks a plan for this instance (a cohort plan for `main`, a
/// load plan for `load`) without storing anything.
pub fn admit(cfg: &EvalConfig, bytes: &[u8]) -> R<(CohortPlan, Admission)> {
    let plan = if cfg.is_load() {
        LoadPlan::parse(bytes, cfg.ceilings).map_err(|e| EvalError::Plan(e.0))?.cohort_plan()
    } else {
        CohortPlan::parse(bytes, cfg.ceilings).map_err(|e| EvalError::Plan(e.0))?
    };
    // A named main authority (a restarted chain record) takes only its own
    // labels and cohort numbers, so nothing it writes shares a label or a
    // number with an earlier authority's (`config::Authority`).
    cfg.authority.admits(&plan.label, plan.cohort).map_err(EvalError::Plan)?;
    if cfg.max_score_lanes == 0 {
        return Err(EvalError::Plan("MAX_SCORE_LANES is not configured"));
    }
    // Fresh human stock is never consumed without an admitted external
    // entrant. Admission is not wired into reward-v1 cohorts yet, so no plan
    // has one: only pools the deployment lists as exposed may be drawn.
    if plan.is_v3() && !cfg.exposed_pools.contains(&plan.pool.pool_id) {
        return Err(EvalError::Plan(FRESH_STOCK_REFUSED));
    }
    let f = plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    let jobs = plan.jobs() as u32;
    let cap = f.score.max_lanes.min(cfg.max_score_lanes);
    let lanes = cost::plan_score_lanes(jobs, plan.n, plan.scorer.chunk_docs, f.score.lane_docs, cap).len() as u32;
    // reward-v1: references score the batch; every entrant also scores every
    // probe version in use and both canary audit draws.
    let docs = u32::try_from(plan.scored_docs()).map_err(|_| EvalError::Plan("documents to score"))?;
    let ub = cost::nano_to_micro_ceil(cost::scoring_upper_bound_nanousd(1, docs, lanes, &f.cost));
    if ub > f.score.scoring_budget_microusd {
        return Err(EvalError::Plan("scoring budget is below the plan's upper bound"));
    }
    let adm = Admission {
        label: plan.label.clone(),
        reserve_microusd: plan.generation_budget_microusd + f.score.scoring_budget_microusd,
        scoring_upper_bound_microusd: ub,
        score_lanes: lanes,
    };
    Ok((plan, adm))
}

/// Queues an operator plan. Returns the cohort's sequence number. A load plan
/// goes straight to `sealing`: it never waits on a boundary and never opens a
/// target.
pub fn queue(sql: &impl Sql, cfg: &EvalConfig, bytes: &[u8], now_ms: u64) -> R<i64> {
    // `cfg` was read when the request started; a successor's supersede may
    // have committed since (an operator plan awaits the ledger, a calendar
    // open its predecessors, between the two). Read here, in the queueing
    // transaction, a superseded authority queues nothing.
    if crate::eval::succession::superseded_by(sql)?.is_some() {
        return Err(EvalError::Plan("authority superseded (a later main authority restarted the chain record)"));
    }
    let (plan, adm) = admit(cfg, bytes)?;
    let existing = cohorts(sql)?;
    if let Some(c) = existing.iter().find(|c| c.label == plan.label) {
        // A repeated submission of the same bytes (a retry after a lost reply)
        // is the same cohort; other bytes under a queued label are refused.
        return if c.plan_sha256 == sha_hex(bytes) { Ok(c.seq) } else { Err(EvalError::Plan("label already queued")) };
    }
    if existing.iter().any(|c| c.plan.cohort >= plan.cohort) {
        return Err(EvalError::Plan("cohort number must increase"));
    }
    let o = observed(sql)?;
    let stored = if plan.is_load() {
        serde_json::to_string(&plan).map_err(|_| EvalError::Plan("plan json"))?
    } else {
        std::str::from_utf8(bytes).map_err(|_| EvalError::Plan("plan utf-8"))?.to_owned()
    };
    sql.exec(
        "INSERT INTO cohorts (label, cohort, plan, plan_sha256, phase, phase_since_ms, queued_ms, queued_epoch,
            reserved_microusd)
         VALUES (?, ?, ?, ?, 'queued', ?, ?, ?, ?)",
        &[
            s(&plan.label),
            bind_u64(plan.cohort)?,
            s(stored),
            s(sha_hex(bytes)),
            bind_u64(now_ms)?,
            bind_u64(now_ms)?,
            o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)),
            bind_u64(adm.reserve_microusd)?,
        ],
    )?;
    let seq = int(&sql.exec("SELECT MAX(seq) FROM cohorts", &[])?[0], 0)?;
    set_phase(sql, seq, "queued", now_ms, "queued")?;
    if plan.is_load() {
        let block = o.confirmed.map_or(1, |c| c.number.max(1));
        sql.exec(
            "UPDATE cohorts SET opened_block = ?, opened_ms = ?, opened_epoch = ? WHERE seq = ?",
            &[bind_u64(block)?, bind_u64(now_ms)?, o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)), i(seq)],
        )?;
        set_phase(sql, seq, "sealing", now_ms, "load: re-score of a sealed batch; verification only, never a target")?;
    }
    Ok(seq)
}

/// Boundary-driven phase moves at the observed epoch index.
pub fn advance(sql: &impl Sql, now_ms: u64) -> R<Vec<String>> {
    let Some(e) = observed(sql)?.epoch else { return Ok(Vec::new()) };
    let mut moved = Vec::new();
    for c in cohorts(sql)? {
        let p = &c.plan;
        if p.is_load() {
            // Load cohorts never wait on a boundary.
            continue;
        }
        let active_now = observed(sql)?.active_tempos;
        match c.phase.as_str() {
            "closed" | "abandoned" => continue,
            "queued" => {
                let Some(q) = c.queued_epoch else {
                    // Queued before any epoch was observed: wait for the next increment.
                    sql.exec("UPDATE cohorts SET queued_epoch = ? WHERE seq = ?", &[bind_u64(e.index)?, i(c.seq)])?;
                    break;
                };
                if p.is_probe_build() {
                    // A probe build has no roster and pays nothing: it starts at once.
                    sql.exec(
                        "UPDATE cohorts SET opened_epoch = ?, opened_block = ?, opened_ms = ?, opened_active = ?,
                            admission_closed_epoch = ? WHERE seq = ?",
                        &[bind_u64(e.index)?, bind_u64(e.last_epoch_block)?, bind_u64(now_ms)?, bind_u64(active_now)?,
                            bind_u64(e.index)?, i(c.seq)],
                    )?;
                    set_phase(sql, c.seq, "generating", now_ms, "probe build: drawn at once; no roster, no target")?;
                    moved.push(format!("{}:generating", c.label));
                    break;
                }
                if e.index <= q {
                    break;
                }
                sql.exec(
                    "UPDATE cohorts SET opened_epoch = ?, opened_block = ?, opened_ms = ?, opened_active = ? WHERE seq = ?",
                    &[bind_u64(e.index)?, bind_u64(e.last_epoch_block)?, bind_u64(now_ms)?, bind_u64(active_now)?, i(c.seq)],
                )?;
                if p.admission.is_some() {
                    let opened = cohort(sql, c.seq)?;
                    crate::eval::admission::open(sql, &opened, e.last_epoch_block)?;
                    set_phase(sql, c.seq, "admission", now_ms, "opened at an epoch boundary; admitting")?;
                } else {
                    set_phase(sql, c.seq, "admission", now_ms, "opened at an epoch boundary; roster frozen")?;
                }
                moved.push(format!("{}:admission", c.label));
                break;
            }
            "admission" if p.admission.is_some() => {
                // Coordinator admission: the window closes at the boundary
                // that reaches the minimum; the roster freezes later, in the
                // cohort's own step (`admission::try_freeze`), which moves the
                // cohort to generating.
                if c.opened_active.is_some_and(|o| active_now >= o + p.admission_tempos)
                    && crate::eval::admission::close_window(sql, &c, e.index, e.last_epoch_block, now_ms)?
                {
                    moved.push(format!("{}:admission_closed", c.label));
                }
                break;
            }
            "admission" => {
                if c.opened_active.is_some_and(|o| active_now >= o + p.admission_tempos) {
                    sql.exec("UPDATE cohorts SET admission_closed_epoch = ? WHERE seq = ?", &[bind_u64(e.index)?, i(c.seq)])?;
                    set_phase(sql, c.seq, "generating", now_ms, "admission minimum reached")?;
                    moved.push(format!("{}:generating", c.label));
                }
                break;
            }
            "transition" if p.is_v3() => {
                // Publication completes at the first boundary after the package
                // is durable; the transition minimum counts from there.
                let row = sql.exec(
                    "SELECT packaged_epoch, published_epoch, published_active FROM cohorts WHERE seq = ?",
                    &[i(c.seq)],
                )?;
                let r = row.first().ok_or(StoreError("cohort"))?;
                let packaged = opt_int(r, 0).map(|v| v as u64);
                let (published, published_active) = match (opt_int(r, 1), opt_int(r, 2)) {
                    (Some(pe), Some(pa)) => (pe as u64, pa as u64),
                    _ => {
                        if packaged.is_some_and(|pk| e.index > pk) {
                            sql.exec(
                                "UPDATE cohorts SET published_epoch = ?, published_active = ? WHERE seq = ?",
                                &[bind_u64(e.index)?, bind_u64(active_now)?, i(c.seq)],
                            )?;
                            moved.push(format!("{}:published", c.label));
                        }
                        break;
                    }
                };
                let opened = c.opened_active.unwrap_or(u64::MAX / 2);
                if active_now >= opened + p.k_tempos
                    && active_now >= published_active + p.transition_tempos
                    && e.index > published
                {
                    sql.exec("UPDATE cohorts SET closed_epoch = ? WHERE seq = ?", &[bind_u64(e.index)?, i(c.seq)])?;
                    crate::eval::v3::close(sql, &c, e.index)?;
                    sql.exec("UPDATE docs SET text = NULL WHERE seq = ?", &[i(c.seq)])?;
                    set_phase(sql, c.seq, "closed", now_ms, "closed at a boundary; artifacts and openings publish now")?;
                    moved.push(format!("{}:closed", c.label));
                    continue;
                }
                break;
            }
            "transition" => {
                let opened = c.opened_active.unwrap_or(u64::MAX / 2);
                let packaged = c.packaged_active.unwrap_or(u64::MAX / 2);
                if active_now >= opened + p.k_tempos && active_now >= packaged + p.transition_tempos {
                    sql.exec("UPDATE cohorts SET closed_epoch = ? WHERE seq = ?", &[bind_u64(e.index)?, i(c.seq)])?;
                    let target = sql.exec("SELECT target_digest FROM cohorts WHERE seq = ?", &[i(c.seq)])?;
                    let t = text(&target[0], 0)?.to_owned();
                    sql.exec(
                        "INSERT INTO eval_targets (seq, target_digest, activated_epoch) VALUES (?, ?, ?)",
                        &[i(c.seq), s(t), bind_u64(e.index)?],
                    )?;
                    // Private texts leave Durable Object storage; the sealed batch stays in the eval bucket.
                    sql.exec("UPDATE docs SET text = NULL WHERE seq = ?", &[i(c.seq)])?;
                    set_phase(sql, c.seq, "closed", now_ms, "target active at this boundary")?;
                    moved.push(format!("{}:closed", c.label));
                    continue;
                }
                break;
            }
            // Work phases move when their work commits.
            _ => break,
        }
    }
    Ok(moved)
}

/// The cohort whose work is runnable now, if any.
pub fn active(sql: &impl Sql) -> R<Option<Cohort>> {
    Ok(cohorts(sql)?
        .into_iter()
        .find(|c| matches!(c.phase.as_str(), "generating" | "sealing" | "scoring" | "packaging") && c.fault.is_none()))
}

// ---------------------------------------------------------------------- draw

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MixedSpec {
    pub first: usize,
    pub last: usize,
    pub start: usize,
    pub end: usize,
    pub percent: u32,
    /// Generators of the parent's earlier children (reward-v1 reuse), which
    /// this child avoids when the plan offers another.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid: Vec<String>,
    /// reward-v1: the rewrite voice and operation of
    /// [`textgen::REWRITE_PROMPT_SET`], chosen at the draw avoiding the
    /// parent's earlier uses. Absent: the plain rewrite prompt (reward-v0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScratchSpec {
    pub topic: String,
    pub words: usize,
}

/// Creates the cohort's documents from a draw, and marks the pool rows and groups
/// used, in one transaction. `texts` maps drawn row ids to their checked texts;
/// `ledger` is the durable used ledger outside this Durable Object (already
/// excluded by the draw, and checked again here).
///
/// Returns false, with the cohort paused (`draw_shortfall`) and nothing marked
/// used, when the pool cannot supply the plan's humans, or their sentences the
/// plan's mixed cases, within the declared tolerance.
pub fn apply_draw(
    sql: &impl Sql,
    cfg: &EvalConfig,
    seq: i64,
    draw: &Draw,
    texts: &BTreeMap<String, String>,
    ledger: &pool::Used,
    now_ms: u64,
) -> R<bool> {
    let c = cohort(sql, seq)?;
    if c.phase != "generating" {
        return Err(EvalError::State("draw outside generating"));
    }
    if int(&sql.exec("SELECT COUNT(*) FROM docs WHERE seq = ?", &[i(seq)])?[0], 0)? > 0 {
        return Err(EvalError::State("already drawn"));
    }
    let seed = cohort_seed(&cfg.private_seed, &c.label);
    for r in &draw.rows {
        let used = sql.exec(
            "SELECT 1 FROM pool_used WHERE row_id = ? OR text_sha256 = ?
             UNION ALL SELECT 1 FROM pool_groups_used WHERE source_group = ?",
            &[s(&r.row_id), s(&r.text_sha256), s(&r.source_group)],
        )?;
        if !used.is_empty() || ledger.contains(r) {
            return Err(EvalError::State("pool row or group already used"));
        }
        let t = texts.get(&r.row_id).ok_or(EvalError::State("drawn row text missing"))?;
        if sha_hex(t.as_bytes()) != r.text_sha256 {
            return Err(EvalError::State("drawn row text digest"));
        }
    }
    let [_, n_mixed, n_scratch] = c.plan.counts();
    // Documents keyed by a private, stable key; ordered by a private rank.
    struct NewDoc {
        key: String,
        kind: &'static str,
        domain: String,
        group: String,
        parent_key: Option<String>,
        text: Option<String>,
        spec: Option<String>,
    }
    let mut docs = Vec::new();
    let mut mixed = 0u32;
    for r in &draw.rows {
        let t = &texts[&r.row_id];
        docs.push(NewDoc {
            key: format!("h:{}", r.row_id),
            kind: "human_only",
            domain: r.domain.clone(),
            group: r.source_group.clone(),
            parent_key: None,
            text: Some(t.clone()),
            spec: None,
        });
        if mixed < n_mixed {
            let key = format!("m:{}", r.row_id);
            let d = item_digest(&seed, &key);
            let sents = textgen::sentences(t);
            if let Some((first, last, percent)) = textgen::select_span(sents.len(), &d) {
                let spec = MixedSpec {
                    first,
                    last,
                    start: sents[first].0,
                    end: sents[last].1,
                    percent,
                    avoid: Vec::new(),
                    voice: None,
                    operation: None,
                };
                docs.push(NewDoc {
                    key,
                    kind: "mixed",
                    domain: r.domain.clone(),
                    group: r.source_group.clone(),
                    parent_key: Some(format!("h:{}", r.row_id)),
                    text: None,
                    spec: Some(serde_json::to_string(&spec).unwrap()),
                });
                mixed += 1;
            }
        }
    }
    // Scratch documents follow the drawn humans' domain mix.
    let mut weights: BTreeMap<String, u64> = BTreeMap::new();
    for r in &draw.rows {
        *weights.entry(r.domain.clone()).or_default() += 1;
    }
    for (domain, count) in pool::proportional(&weights, u64::from(n_scratch)) {
        for j in 0..count {
            let key = format!("s:{domain}:{j}");
            let d = item_digest(&seed, &key);
            let spec = ScratchSpec { topic: textgen::topic(&domain, &d).into(), words: textgen::scratch_words(&d) };
            docs.push(NewDoc {
                group: format!("scratch:{key}"),
                key,
                kind: "model_only",
                domain: domain.clone(),
                parent_key: None,
                text: None,
                spec: Some(serde_json::to_string(&spec).unwrap()),
            });
        }
    }
    let scratch = docs.iter().filter(|d| d.kind == "model_only").count() as u32;
    let have = [draw.rows.len() as u32, mixed, scratch];
    if let Some(short) = c.plan.excess_shortfall(have) {
        // Nothing is exposed: no document is created and no human is marked used.
        sql.exec(
            "UPDATE cohorts SET counts = ? WHERE seq = ?",
            &[s(counts_json(&c.plan, have, short).to_string()), i(seq)],
        )?;
        fault(sql, seq, "draw_shortfall", now_ms)?;
        return Ok(false);
    }
    docs.sort_by_key(|d| pool::rank(&seed, "order", &d.key));
    let index: BTreeMap<String, usize> = docs.iter().enumerate().map(|(n, d)| (d.key.clone(), n)).collect();
    for (n, d) in docs.iter().enumerate() {
        let (status, sha) = match &d.text {
            Some(t) => ("ready", s(sha_hex(t.as_bytes()))),
            None => ("pending", SqlValue::Null),
        };
        sql.exec(
            "INSERT INTO docs (seq, idx, key, kind, domain, source_group, parent, status, text, text_sha256,
                share_ppb, spec) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                i(seq),
                i(n as i64),
                s(&d.key),
                s(d.kind),
                s(&d.domain),
                s(&d.group),
                d.parent_key.as_ref().map_or(SqlValue::Null, |k| i(index[k] as i64)),
                s(status),
                d.text.clone().map_or(SqlValue::Null, s),
                sha,
                if d.kind == "human_only" { i(0) } else { SqlValue::Null },
                d.spec.clone().map_or(SqlValue::Null, s),
            ],
        )?;
    }
    for r in &draw.rows {
        sql.exec(
            "INSERT INTO pool_used (row_id, text_sha256, source_group, seq) VALUES (?, ?, ?, ?)",
            &[s(&r.row_id), s(&r.text_sha256), s(&r.source_group), i(seq)],
        )?;
    }
    for g in &draw.groups {
        sql.exec("INSERT INTO pool_groups_used (source_group, seq) VALUES (?, ?)", &[s(g), i(seq)])?;
    }
    let summary = json!({"rule": pool::DRAW_RULE, "humans": draw.rows.len(), "groups": draw.groups.len(),
        "quotas": draw.quotas, "shortfall": draw.shortfall, "available_before": draw.available_rows,
        "mixed": mixed, "scratch": n_scratch,
        "ledger": {"rows": ledger.rows.len(), "groups": ledger.groups.len()}});
    sql.exec("UPDATE cohorts SET draw = ? WHERE seq = ?", &[s(summary.to_string()), i(seq)])?;
    set_phase(sql, seq, "generating", now_ms, "humans drawn")?;
    // Generation lanes, and the scoring lanes (planned now so they can be
    // pre-started during the generation tail).
    crate::eval::lanes::plan_generation(sql, cfg, seq, now_ms)?;
    Ok(true)
}

/// The kind counts (plan, have, shortfall, tolerance) recorded on the cohort and
/// bound into the sealed batch index.
pub(crate) fn counts_json(plan: &CohortPlan, have: [u32; 3], short: [u32; 3]) -> Value {
    json!({"rule": "kind_shortfall_permille_v1", "kinds": ["human_only", "mixed", "model_only"],
        "plan": plan.counts(), "have": have, "shortfall": short,
        "tolerance_permille": plan.max_kind_shortfall_permille, "tolerance": plan.shortfall_tolerance()})
}

pub(crate) fn ready_counts(sql: &impl Sql, seq: i64) -> R<[u32; 3]> {
    let mut have = [0u32; 3];
    for r in sql.exec(
        "SELECT kind, COUNT(*) FROM docs WHERE seq = ? AND status = 'ready' AND docset = 'batch' GROUP BY kind",
        &[i(seq)],
    )? {
        let k = match text(&r, 0)? {
            "human_only" => 0,
            "mixed" => 1,
            _ => 2,
        };
        have[k] = int(&r, 1)? as u32;
    }
    Ok(have)
}

/// The SHA-256 of the cohort's exact plan bytes.
pub fn plan_sha256(sql: &impl Sql, seq: i64) -> R<String> {
    Ok(cohort(sql, seq)?.plan_sha256)
}

/// The content-free used set (row ids and digests are private but not text).
pub fn used_set(sql: &impl Sql) -> R<(BTreeSet<String>, BTreeSet<String>)> {
    let rows = sql.exec("SELECT row_id FROM pool_used", &[])?;
    let groups = sql.exec("SELECT source_group FROM pool_groups_used", &[])?;
    Ok((
        rows.iter().map(|r| text(r, 0).map(str::to_owned)).collect::<Result<_, _>>()?,
        groups.iter().map(|r| text(r, 0).map(str::to_owned)).collect::<Result<_, _>>()?,
    ))
}

// ---------------------------------------------------------------------- work

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenItem {
    pub idx: i64,
    pub kind: String,
    pub generator: String,
    pub prompt: Prompt,
    pub attempt: i64,
    /// The claim fence: a result lands only while this is the item's latest claim.
    #[serde(default)]
    pub claim: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Work {
    Draw { seq: i64, plan: Box<CohortPlan> },
    Seal { seq: i64 },
    /// Load instance: index the source batch, sign the load manifest, plan lanes.
    LoadSeal { seq: i64, source: crate::eval::plan::LoadSource },
    Package { seq: i64 },
    /// Wake these lanes (their assignments are already recorded) and stop these
    /// lanes' containers, then sleep until the next sweep.
    Dispatch { seq: i64, kicks: Vec<crate::eval::lanes::LaneKick>, stop: Vec<String> },
    /// Charge a finished cohort's measured spend to the monthly ledger.
    Settle { seq: i64, key: String, microusd: u64 },
    /// Delete a finished cohort's private results objects and summaries (per-
    /// document predictions) from the eval bucket, as its texts are deleted.
    Purge { seq: i64, prefixes: Vec<String> },
    /// Delete one page of a cohort's detailed receipts from the receipts
    /// bucket under the N+2 rule (`eval::retention`), after reading the
    /// current authorization's hold: the content-free `marker` (key, JSON)
    /// first while it is not yet recorded, then objects under `prefix`, then
    /// the single `keys`, then its receipt rows.
    RetireReceipts { seq: i64, prefix: String, keys: Vec<String>, marker: Option<(String, String)> },
    /// Tell a superseded predecessor that this authority completed two
    /// cohorts, the second numbered `after_cohort` (`eval::retention`).
    RetentionNotice { predecessor: String, after_cohort: u64 },
    /// Publish a closed reward-v1 cohort's signed closure and index.
    Close { seq: i64 },
    /// Admission: frame a completed upload (`frame`) or copy its slices to
    /// the blob store (`blobs`).
    AdmissionVerify { session: String, stage: String },
    /// Admission: run the claimable verdicts on the scorer.
    AdmissionLanes,
    /// Admission: abort uploads and delete objects of sessions that ended
    /// without admission.
    AdmissionCleanup,
    /// The calendar: queue the next cohort from the standing template.
    OpenNext { label: String, plan: Vec<u8> },
    /// Admission: charge one run's measured instance time to the tracking
    /// ledger under its own key; marked settled only after the ledger
    /// answers (`admission::charge_settled`).
    AdmissionSettle { key: String, microusd: u64 },
}

/// The next root step or sweep, if any. Phase moves that need no I/O (all
/// generation finished, all scoring finished) happen here, in the same
/// transaction. Bulk work is never done here: it is dispatched to lanes.
pub fn next_work(sql: &impl Sql, cfg: &EvalConfig, now_ms: u64) -> R<Option<Work>> {
    // A finished cohort's spend is charged to the ledger when no work runs, so
    // a ledger outage never delays evaluation. The purge of private
    // per-document predictions comes first, so a failing ledger (a settle
    // that keeps returning an error) never holds private results in the
    // bucket.
    if let Some(seq) = crate::eval::v3::closure_due(sql)? {
        return Ok(Some(Work::Close { seq }));
    }
    if !cfg.is_load()
        && let Some(w) = crate::eval::admission::next_work(sql, cfg, now_ms)?
    {
        return Ok(Some(w));
    }
    let Some(c) = active(sql)? else {
        if let Some(w) = crate::eval::lanes::purge_due(sql, now_ms)? {
            return Ok(Some(w));
        }
        if let Some(w) = crate::eval::lanes::settle_due(sql, cfg, now_ms)? {
            return Ok(Some(w));
        }
        if let Some(w) = crate::eval::admission::charge_due(sql)? {
            return Ok(Some(w));
        }
        return retention_work(sql, cfg);
    };
    let work = active_work(sql, cfg, c, now_ms)?;
    // While the active cohort only waits on its lanes (a sweep with nothing
    // to send or stop), receipt deletion and predecessor notices run, so
    // back-to-back cohorts never leave the N+2 rule waiting for an idle gap.
    if let Some(Work::Dispatch { kicks, stop, .. }) = &work
        && kicks.is_empty()
        && stop.is_empty()
        && let Some(w) = retention_work(sql, cfg)?
    {
        return Ok(Some(w));
    }
    Ok(work)
}

/// The next receipt deletion step, else a due predecessor notice
/// (`eval::retention`).
fn retention_work(sql: &impl Sql, cfg: &EvalConfig) -> R<Option<Work>> {
    if let Some(w) = crate::eval::retention::next_work(sql, cfg)? {
        return Ok(Some(w));
    }
    crate::eval::retention::notice_due(sql, cfg)
}

/// The active cohort's next root step or sweep.
fn active_work(sql: &impl Sql, cfg: &EvalConfig, c: Cohort, now_ms: u64) -> R<Option<Work>> {
    let seq = c.seq;
    match c.phase.as_str() {
        "generating" => {
            let n = int(&sql.exec("SELECT COUNT(*) FROM docs WHERE seq = ?", &[i(seq)])?[0], 0)?;
            if n == 0 {
                return Ok(Some(Work::Draw { seq, plan: Box::new(c.plan) }));
            }
            let pending = int(&sql.exec("SELECT COUNT(*) FROM docs WHERE seq = ? AND status = 'pending'", &[i(seq)])?[0], 0)?;
            if c.plan.is_v3() {
                // Exact kinds: spares cover rejected items; the seal keeps the
                // planned count of each kind by private rank.
                if crate::eval::v3::batch_infeasible(sql, &c)? {
                    fault(sql, seq, "batch_shortfall", now_ms)?;
                    let stop = crate::eval::lanes::live_lane_names(sql, seq, &["score", "gen"])?;
                    return Ok(Some(Work::Dispatch { seq, kicks: Vec::new(), stop }));
                }
                if pending == 0 && crate::eval::v3::batch_complete(sql, &c)? {
                    set_phase(sql, seq, "sealing", now_ms, "generation complete")?;
                    return Ok(Some(Work::Seal { seq }));
                }
                return crate::eval::lanes::generation_sweep(sql, cfg, &c, now_ms).map(Some);
            }
            if pending == 0 {
                // The batch must match the plan's kind counts within the declared tolerance.
                let have = ready_counts(sql, seq)?;
                let want = c.plan.counts();
                let short = [0, 1, 2].map(|k| want[k].saturating_sub(have[k]));
                sql.exec(
                    "UPDATE cohorts SET counts = ? WHERE seq = ?",
                    &[s(counts_json(&c.plan, have, short).to_string()), i(seq)],
                )?;
                if c.plan.excess_shortfall(have).is_some() {
                    fault(sql, seq, "batch_shortfall", now_ms)?;
                    let stop = crate::eval::lanes::live_lane_names(sql, seq, &["score"])?;
                    return Ok(Some(Work::Dispatch { seq, kicks: Vec::new(), stop }));
                }
                set_phase(sql, seq, "sealing", now_ms, "generation complete")?;
                return Ok(Some(Work::Seal { seq }));
            }
            crate::eval::lanes::generation_sweep(sql, cfg, &c, now_ms).map(Some)
        }
        "sealing" => Ok(Some(match &c.plan.load {
            Some(l) => Work::LoadSeal { seq, source: l.source.clone() },
            None => Work::Seal { seq },
        })),
        "scoring" => crate::eval::lanes::scoring_sweep(sql, cfg, &c, now_ms).map(Some),
        "packaging" => crate::eval::lanes::packaging_sweep(sql, cfg, &c, now_ms).map(Some),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------- generation

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenOutcome {
    pub idx: i64,
    pub generator: String,
    /// Raw model text, or why there is none (transport, throttled, unavailable).
    pub output: Result<String, Reject>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub ms: u64,
    /// The claim this call ran under; a stale claim's result records usage only.
    #[serde(default)]
    pub claim: i64,
}

/// An accepted output: final text, model span, words, model words, share (ppb).
type Accepted = (String, Option<(usize, usize)>, usize, usize, u32);

/// Whether a generation result is an outage rather than the model's answer: a
/// transport failure, or a reply with no text and no generated tokens. An empty
/// answer after generated tokens (a reasoning model at its cap) is the model's.
/// Throttles are neither: they consume nothing, up to the plan's bound.
pub fn is_outage(g: &GenOutcome) -> bool {
    match &g.output {
        Err(Reject::Transport) => true,
        Err(Reject::Empty) => g.completion_tokens == 0,
        Ok(t) => t.trim().is_empty() && g.completion_tokens == 0,
        Err(_) => false,
    }
}

fn gen_backoff_ms(failures: i64) -> u64 {
    let shift = (failures.clamp(1, 16) - 1) as u32;
    GEN_BACKOFF_BASE_MS.saturating_mul(1 << shift).min(GEN_BACKOFF_MAX_MS)
}

/// Throttle backoff: min(60 s, 2 s x 2^t) plus a deterministic jitter under 1 s.
pub fn throttle_backoff_ms(idx: i64, throttles: i64) -> u64 {
    let base = 2_000u64.saturating_mul(1 << (throttles.clamp(0, 5) as u32)).min(60_000);
    base + (idx.unsigned_abs().wrapping_mul(7_919).wrapping_add(throttles.unsigned_abs().wrapping_mul(104_729))) % 1_000
}

/// Validates and records generation results from lanes.
///
/// A model answer (accepted, or rejected for its content) uses one of the
/// item's [`MAX_GEN_ATTEMPTS`] and rotates the generator, as does a model the
/// provider no longer serves. An outage uses none: the item backs off
/// exponentially, and its [`GEN_INFRA_MAX`]th consecutive outage pauses the
/// cohort (`generation_unavailable`, resumable by a clear), so an outage never
/// drops items from the batch. A throttle (rate limit or capacity) uses no
/// attempt and keeps the generator: the item backs off, until its accumulated
/// throttling passes the plan's `max_throttle_ms`, after which a throttle counts
/// as an outage. A result whose claim is no longer the item's latest records
/// its usage (the spend was real) and is otherwise discarded.
pub fn apply_generation(sql: &impl Sql, cfg: &EvalConfig, seq: i64, results: &[GenOutcome], now_ms: u64) -> R<Value> {
    let c = cohort(sql, seq)?;
    if c.phase != "generating" {
        return Err(EvalError::State("generation outside generating"));
    }
    let max_throttle = c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?.generation.max_throttle_ms;
    let seed = cohort_seed(&cfg.private_seed, &c.label);
    let (mut accepted, mut rejected, mut outages, mut throttled, mut fenced) = (0, 0, 0, 0, 0);
    let mut pause = false;
    let mut cost_total = 0u64;
    let mut released = 0u64;
    for g in results {
        let rows = sql.exec(
            "SELECT kind, parent, spec, attempts, status, infra_failures, claim_gen, reserved_microusd, throttles,
                    throttle_since_ms, key
             FROM docs WHERE seq = ? AND idx = ?",
            &[i(seq), i(g.idx)],
        )?;
        let r = rows.first().ok_or(EvalError::State("generated doc"))?;
        let cost = textgen::cost_microusd(&g.generator, g.prompt_tokens, g.completion_tokens);
        cost_total += cost;
        let current = text(r, 4)? == "pending" && int(r, 6)? == g.claim;
        let is_throttle = matches!(g.output, Err(Reject::Throttled));
        let validated: Result<Accepted, Reject> = match &g.output {
            _ if !current => Err(Reject::Transport),
            Err(e) => Err(*e),
            Ok(out) if text(r, 0)? == "mixed" => {
                let spec: MixedSpec = serde_json::from_str(text(r, 2)?).map_err(|_| EvalError::State("spec"))?;
                let pt = sql.exec("SELECT text FROM docs WHERE seq = ? AND idx = ?", &[i(seq), i(int(r, 1)?)])?;
                let parent = text(pt.first().ok_or(EvalError::State("parent"))?, 0)?.to_owned();
                textgen::validate_rewrite(&parent[spec.start..spec.end], out).map(|rep| {
                    let sp = textgen::splice(&parent, (spec.start, spec.end), &rep);
                    let share = sp.share_ppb();
                    (sp.text, Some(sp.span), sp.words_total, sp.words_ai, share)
                })
            }
            Ok(out) => {
                let spec: ScratchSpec = serde_json::from_str(text(r, 2)?).map_err(|_| EvalError::State("spec"))?;
                textgen::validate_scratch(out, spec.words).map(|t| {
                    let w = textgen::words(&t);
                    let len = t.len();
                    (t, Some((0, len)), w, w, 1_000_000_000)
                })
            }
        };
        let outage = current && is_outage(g);
        let (ok, err, rej, thr) = match &validated {
            _ if !current => (0, 0, 0, 0),
            _ if outage => (0, 1, 0, 0),
            _ if is_throttle => (0, 0, 0, 1),
            Ok(_) => (1, 0, 0, 0),
            Err(_) => (0, 0, 1, 0),
        };
        sql.exec(
            "INSERT INTO gen_usage (seq, model, calls, accepted, rejected, errors, prompt_tokens, completion_tokens,
                cost_microusd, ms, throttled, fenced) VALUES (?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (seq, model) DO UPDATE SET calls = calls + 1, accepted = accepted + excluded.accepted,
                rejected = rejected + excluded.rejected, errors = errors + excluded.errors,
                prompt_tokens = prompt_tokens + excluded.prompt_tokens,
                completion_tokens = completion_tokens + excluded.completion_tokens,
                cost_microusd = cost_microusd + excluded.cost_microusd, ms = ms + excluded.ms,
                throttled = throttled + excluded.throttled, fenced = fenced + excluded.fenced",
            &[
                i(seq),
                s(&g.generator),
                i(ok),
                i(rej),
                i(err),
                bind_u64(g.prompt_tokens)?,
                bind_u64(g.completion_tokens)?,
                bind_u64(cost)?,
                bind_u64(g.ms)?,
                i(thr),
                i(i64::from(!current)),
            ],
        )?;
        if !current {
            // A stale claim (expired and taken again, or already resolved).
            fenced += 1;
            continue;
        }
        // The live claim ends here: its reservation is replaced by the measured cost.
        released += uint(r, 7)?;
        sql.exec(
            "UPDATE docs SET reserved_microusd = 0, holder = NULL, claim_until_ms = 0 WHERE seq = ? AND idx = ?",
            &[i(seq), i(g.idx)],
        )?;
        let (throttles, since) = (int(r, 8)?, int(r, 9)?);
        if is_throttle {
            let since = if since == 0 { now_ms as i64 } else { since };
            if now_ms.saturating_sub(since as u64) <= max_throttle {
                throttled += 1;
                sql.exec(
                    "UPDATE docs SET throttles = ?, throttle_since_ms = ?, not_before_ms = ? WHERE seq = ? AND idx = ?",
                    &[i(throttles + 1), i(since), bind_u64(now_ms + throttle_backoff_ms(g.idx, throttles))?, i(seq), i(g.idx)],
                )?;
                continue;
            }
        }
        if outage || is_throttle {
            // An outage, or throttling past the plan's bound: no attempt, backoff,
            // and the GEN_INFRA_MAXth in a row pauses the cohort.
            outages += 1;
            let failures = int(r, 5)? + 1;
            pause |= failures >= GEN_INFRA_MAX;
            sql.exec(
                "UPDATE docs SET infra_failures = ?, not_before_ms = ? WHERE seq = ? AND idx = ?",
                &[i(failures), bind_u64(now_ms + gen_backoff_ms(failures))?, i(seq), i(g.idx)],
            )?;
            continue;
        }
        match validated {
            Ok((t, span, total, ai, share)) => {
                accepted += 1;
                // reward-v1: the rewrite-source ledger records the generator
                // of every accepted child, so later uses avoid it.
                if c.plan.is_v3() && text(r, 0)? == "mixed" {
                    crate::eval::v3::record_child_generator(sql, seq, int(r, 1)?, &g.generator)?;
                }
                let (a, b) = span.unwrap_or((0, t.len()));
                sql.exec(
                    "UPDATE docs SET status = 'ready', text = ?, text_sha256 = ?, generator = ?, share_ppb = ?,
                        span_start = ?, span_end = ?, words_total = ?, words_ai = ?, attempts = attempts + 1,
                        infra_failures = 0, throttle_since_ms = 0, not_before_ms = 0 WHERE seq = ? AND idx = ?",
                    &[
                        s(t.clone()),
                        s(sha_hex(t.as_bytes())),
                        s(&g.generator),
                        i(i64::from(share)),
                        i(a as i64),
                        i(b as i64),
                        i(total as i64),
                        i(ai as i64),
                        i(seq),
                        i(g.idx),
                    ],
                )?;
            }
            Err(e) => {
                rejected += 1;
                let attempts = int(r, 3)? + 1;
                let status = if attempts >= MAX_GEN_ATTEMPTS { "failed" } else { "pending" };
                // The next answer comes from the next generator in the rotation.
                let avoid = if text(r, 0)? == "mixed" {
                    serde_json::from_str::<MixedSpec>(text(r, 2)?).map(|s| s.avoid).unwrap_or_default()
                } else {
                    Vec::new()
                };
                let next = textgen::generator_avoiding(
                    &c.plan.generators,
                    &item_digest(&seed, text(r, 10)?),
                    attempts as u32,
                    &avoid,
                );
                sql.exec(
                    "UPDATE docs SET attempts = ?, status = ?, infra_failures = 0, throttle_since_ms = 0,
                        not_before_ms = 0, generator_next = ?, rejects = rejects || ? WHERE seq = ? AND idx = ?",
                    &[i(attempts), s(status), s(next), s(format!("{};", e.code())), i(seq), i(g.idx)],
                )?;
            }
        }
    }
    sql.exec(
        "UPDATE cohorts SET gen_cost_microusd = gen_cost_microusd + ?,
            gen_reserved_microusd = MAX(0, gen_reserved_microusd - ?) WHERE seq = ?",
        &[bind_u64(cost_total)?, bind_u64(released)?, i(seq)],
    )?;
    if pause {
        fault(sql, seq, "generation_unavailable", now_ms)?;
    }
    Ok(json!({"accepted": accepted, "rejected": rejected, "outages": outages, "throttled": throttled,
        "fenced": fenced, "cost_microusd": cost_total}))
}

// ---------------------------------------------------------------------- seal

#[derive(Clone, Debug)]
pub struct SealBuild {
    pub manifest: CohortManifest,
    pub manifest_digest: Digest,
    /// Private batch shards for the eval bucket: (key, bytes).
    pub shards: Vec<(String, Vec<u8>)>,
    /// Private, content-free batch index (labels, spans, digests; no text).
    pub index_key: String,
    pub index: Vec<u8>,
    pub task_map: Vec<(i64, u32)>,
}

pub const BATCH_SHARD_DOCS: usize = 64;

pub fn batch_prefix(label: &str) -> String {
    format!("cohorts/{label}/batch")
}

pub fn receipt_prefix(label: &str) -> String {
    format!("cohorts/{label}")
}

/// Builds the sealed batch, its private index and the cohort manifest.
pub fn prepare_seal(sql: &impl Sql, cfg: &EvalConfig, seq: i64) -> R<SealBuild> {
    let c = cohort(sql, seq)?;
    if c.phase != "sealing" {
        return Err(EvalError::State("seal outside sealing"));
    }
    let rows = sql.exec(
        "SELECT idx, kind, domain, source_group, text, text_sha256, share_ppb, span_start, span_end, generator,
                words_total, words_ai, parent
         FROM docs WHERE seq = ? AND status = 'ready' ORDER BY idx",
        &[i(seq)],
    )?;
    if rows.is_empty() {
        return Err(EvalError::State("empty batch"));
    }
    // The kind counts against the plan are bound into the batch index, and so
    // into the evaluation set the manifest carries.
    let have = ready_counts(sql, seq)?;
    if c.plan.excess_shortfall(have).is_some() {
        return Err(EvalError::State("batch shortfall"));
    }
    let want = c.plan.counts();
    let counts = counts_json(&c.plan, have, [0, 1, 2].map(|k| want[k].saturating_sub(have[k])));
    let mut task_map = Vec::new();
    let mut entries = Vec::new();
    let mut shard_lines: Vec<String> = Vec::new();
    for (task, r) in rows.iter().enumerate() {
        let idx = int(r, 0)?;
        task_map.push((idx, task as u32));
        let meta = json!({"task": task, "kind": text(r, 1)?, "domain": text(r, 2)?,
            "source_group_sha256": sha_hex(text(r, 3)?.as_bytes()), "text_sha256": text(r, 5)?,
            "share_ppb": opt_int(r, 6), "ai_byte_span": [opt_int(r, 7), opt_int(r, 8)],
            "generator": opt_text(r, 9), "words": opt_int(r, 10), "ai_words": opt_int(r, 11),
            "parent_task": Value::Null, "policy": match text(r, 1)? {
                "mixed" => textgen::SPLICE_POLICY, "model_only" => textgen::SCRATCH_POLICY, _ => "human" }});
        let mut line = meta.clone();
        line["text"] = json!(text(r, 4)?);
        shard_lines.push(line.to_string());
        entries.push(meta);
    }
    // Mixed documents name their human parent's task.
    let by_idx: BTreeMap<i64, u32> = task_map.iter().copied().collect();
    for (r, e) in rows.iter().zip(entries.iter_mut()) {
        if let Some(p) = opt_int(r, 12) {
            e["parent_task"] = json!(by_idx.get(&p));
        }
    }
    let prefix = batch_prefix(&c.label);
    let mut shards = Vec::new();
    let mut shard_index = Vec::new();
    for (k, chunk) in shard_lines.chunks(BATCH_SHARD_DOCS).enumerate() {
        let body: String = chunk.iter().map(|l| format!("{l}\n")).collect();
        let key = format!("{prefix}/shard-{k:04}.jsonl");
        shard_index.push(json!({"key": key, "docs": chunk.len(), "bytes": body.len(), "sha256": sha_hex(body.as_bytes())}));
        shards.push((key, body.into_bytes()));
    }
    let index = serde_json::to_vec_pretty(&json!({
        "schema": "slop_ninja_eval_batch_index_v1",
        "cohort": c.plan.cohort, "label": c.label, "plan_sha256": c.plan_sha256,
        "word_policy": textgen::WORD_POLICY, "prompt_version": textgen::PROMPT_VERSION,
        "temperature": textgen::TEMPERATURE, "top_p": textgen::TOP_P,
        "tasks": entries.len(), "counts": counts, "shards": shard_index, "documents": entries,
    }))
    .map_err(|_| EvalError::State("batch index"))?;
    let plan_sha: [u8; 32] = hex::decode(&c.plan_sha256)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(EvalError::State("plan digest"))?;
    let index_sha: [u8; 32] = Sha256::digest(&index).into();
    let artifacts = manifest_artifacts(&c.plan)?;
    let manifest = package::manifest(
        cfg.chain.clone(),
        c.plan.cohort,
        &c.label,
        c.opened_block.ok_or(EvalError::State("opened block"))?,
        package::evaluation_set(&plan_sha, &index_sha),
        rows.len() as u32,
        c.plan.reference.id().map_err(|e| EvalError::Plan(e.0))?,
        c.plan.max_attempts,
        c.plan.allow_list().map_err(|e| EvalError::Plan(e.0))?,
        c.plan.burn_identity.identity().map_err(|e| EvalError::Plan(e.0))?,
        artifacts,
    )
    .map_err(|e| EvalError::State(e.0))?;
    let manifest_digest = manifest.digest().map_err(|_| EvalError::State("manifest digest"))?;
    Ok(SealBuild {
        manifest,
        manifest_digest,
        shards,
        index_key: format!("{prefix}/index.json"),
        index,
        task_map,
    })
}

/// Commits the seal: task indices, manifest digest, scoring jobs and chunks.
pub fn apply_seal(sql: &impl Sql, seq: i64, b: &SealBuild, puts: u64, now_ms: u64) -> R<()> {
    let c = cohort(sql, seq)?;
    if c.phase != "sealing" {
        return Err(EvalError::State("seal outside sealing"));
    }
    for (idx, task) in &b.task_map {
        sql.exec("UPDATE docs SET task = ? WHERE seq = ? AND idx = ?", &[i(i64::from(*task)), i(seq), i(*idx)])?;
    }
    let n = b.task_map.len() as i64;
    sql.exec(
        "UPDATE cohorts SET manifest_digest = ?, evaluation_set = ?, batch_index_sha256 = ?, task_count = ?,
            r2_puts = r2_puts + ? WHERE seq = ?",
        &[
            s(b.manifest_digest.to_string()),
            s(b.manifest.evaluation_set.to_string()),
            s(sha_hex(&b.index)),
            i(n),
            bind_u64(puts)?,
            i(seq),
        ],
    )?;
    let chunks = insert_jobs(sql, seq, &c.plan, n)?;
    // Lanes read texts from the sealed shards, never through this object.
    for (k, (key, body)) in b.shards.iter().enumerate() {
        let docs = body.iter().filter(|c| **c == b'\n').count() as i64;
        sql.exec(
            "INSERT INTO batch_shards (seq, shard, key, sha256, first_task, docs) VALUES (?, ?, ?, ?, ?, ?)",
            &[i(seq), i(k as i64), s(key), s(sha_hex(body)), i((k * BATCH_SHARD_DOCS) as i64), i(docs)],
        )?;
    }
    crate::eval::lanes::fit_score_ranges(sql, seq, chunks as u32)?;
    sql.exec("UPDATE cohorts SET scoring_started_ms = ? WHERE seq = ?", &[bind_u64(now_ms)?, i(seq)])?;
    set_phase(sql, seq, "scoring", now_ms, "batch and roster sealed; manifest signed")?;
    Ok(())
}

/// Inserts a sealed cohort's scoring jobs and their chunks.
pub(crate) fn insert_jobs(sql: &impl Sql, seq: i64, plan: &CohortPlan, n: i64) -> R<i64> {
    let mut roster: Vec<(&str, &ArtifactJson)> = vec![("reference", &plan.reference)];
    if let Some(p) = &plan.previous_reference {
        roster.push(("previous_reference", p));
    }
    roster.extend(plan.entrants.iter().map(|e| ("entrant", e)));
    let per = i64::from(plan.scorer.chunk_docs);
    let chunks = (n + per - 1) / per;
    for (job, (role, a)) in roster.iter().enumerate() {
        let id = a.id().map_err(|e| EvalError::Plan(e.0))?;
        sql.exec(
            "INSERT INTO jobs (seq, job, role, artifact, package_sha256, manifest_key, owner, status, chunks)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', ?)",
            &[
                i(seq),
                i(job as i64),
                s(*role),
                s(id.to_string()),
                s(&a.package_sha256),
                s(&a.manifest_key),
                s(serde_json::to_string(&a.owner).unwrap()),
                i(chunks),
            ],
        )?;
        for k in 0..chunks {
            sql.exec(
                "INSERT INTO chunks (seq, job, chunk, first_task, count, status) VALUES (?, ?, ?, ?, ?, 'pending')",
                &[i(seq), i(job as i64), i(k), i(k * per), i(per.min(n - k * per))],
            )?;
        }
    }
    Ok(chunks)
}

/// A load cohort's seal: the source batch's text-free index re-bound into a
/// load manifest. Texts stay in the source's private shards.
#[derive(Clone, Debug)]
pub struct LoadSealBuild {
    pub manifest: CohortManifest,
    pub manifest_digest: Digest,
    pub index_sha256: String,
    /// (task, kind, domain, source group digest, text digest, share ppb).
    pub docs: Vec<(u32, String, String, String, String, Option<i64>)>,
    /// (key, SHA-256, documents), in task order.
    pub shards: Vec<(String, String, u32)>,
}

pub fn prepare_load_seal(sql: &impl Sql, cfg: &EvalConfig, seq: i64, index: &[u8]) -> R<LoadSealBuild> {
    let c = cohort(sql, seq)?;
    let load = c.plan.load.as_ref().ok_or(EvalError::State("not a load cohort"))?;
    if c.phase != "sealing" {
        return Err(EvalError::State("load seal outside sealing"));
    }
    let index_sha = sha_hex(index);
    if index_sha != load.source.index_sha256 {
        return Err(EvalError::State("source batch index digest"));
    }
    let v: Value = serde_json::from_slice(index).map_err(|_| EvalError::State("source batch index json"))?;
    if v["schema"] != "slop_ninja_eval_batch_index_v1" || v["tasks"].as_u64() != Some(u64::from(load.source.tasks)) {
        return Err(EvalError::State("source batch index schema or task count"));
    }
    let mut docs = Vec::new();
    for (k, d) in v["documents"].as_array().ok_or(EvalError::State("documents"))?.iter().enumerate() {
        let st = |f: &str| d[f].as_str().map(str::to_owned).ok_or(EvalError::State("document field"));
        if d["task"].as_u64() != Some(k as u64) {
            return Err(EvalError::State("documents out of task order"));
        }
        docs.push((k as u32, st("kind")?, st("domain")?, st("source_group_sha256")?, st("text_sha256")?, d["share_ppb"].as_i64()));
    }
    let mut shards = Vec::new();
    for sh in v["shards"].as_array().ok_or(EvalError::State("shards"))? {
        let key = sh["key"].as_str().ok_or(EvalError::State("shard key"))?.to_owned();
        let sha = sh["sha256"].as_str().ok_or(EvalError::State("shard digest"))?.to_owned();
        shards.push((key, sha, sh["docs"].as_u64().ok_or(EvalError::State("shard docs"))? as u32));
    }
    if docs.len() as u32 != load.source.tasks || shards.iter().map(|x| x.2).sum::<u32>() != load.source.tasks {
        return Err(EvalError::State("source batch index is incomplete"));
    }
    let plan_sha: [u8; 32] = hex::decode(&c.plan_sha256)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(EvalError::State("plan digest"))?;
    let index_digest: [u8; 32] = Sha256::digest(index).into();
    let manifest = package::manifest(
        cfg.chain.clone(),
        c.plan.cohort,
        &c.label,
        c.opened_block.ok_or(EvalError::State("opened block"))?,
        package::evaluation_set(&plan_sha, &index_digest),
        docs.len() as u32,
        c.plan.reference.id().map_err(|e| EvalError::Plan(e.0))?,
        c.plan.max_attempts,
        c.plan.allow_list().map_err(|e| EvalError::Plan(e.0))?,
        c.plan.burn_identity.identity().map_err(|e| EvalError::Plan(e.0))?,
        manifest_artifacts(&c.plan)?,
    )
    .map_err(|e| EvalError::State(e.0))?;
    let manifest_digest = manifest.digest().map_err(|_| EvalError::State("manifest digest"))?;
    Ok(LoadSealBuild { manifest, manifest_digest, index_sha256: index_sha, docs, shards })
}

pub fn apply_load_seal(sql: &impl Sql, cfg: &EvalConfig, seq: i64, b: &LoadSealBuild, now_ms: u64) -> R<()> {
    let c = cohort(sql, seq)?;
    if c.phase != "sealing" || !c.plan.is_load() {
        return Err(EvalError::State("load seal outside sealing"));
    }
    for (task, kind, domain, group, sha, share) in &b.docs {
        sql.exec(
            "INSERT INTO docs (seq, idx, key, kind, domain, source_group, status, text_sha256, share_ppb, task)
             VALUES (?, ?, ?, ?, ?, ?, 'ready', ?, ?, ?)",
            &[
                i(seq),
                i(i64::from(*task)),
                s(format!("load:{task}")),
                s(kind),
                s(domain),
                s(group),
                s(sha),
                share.map_or(SqlValue::Null, i),
                i(i64::from(*task)),
            ],
        )?;
    }
    let mut first = 0i64;
    for (k, (key, sha, docs)) in b.shards.iter().enumerate() {
        sql.exec(
            "INSERT INTO batch_shards (seq, shard, key, sha256, first_task, docs) VALUES (?, ?, ?, ?, ?, ?)",
            &[i(seq), i(k as i64), s(key), s(sha), i(first), i(i64::from(*docs))],
        )?;
        first += i64::from(*docs);
    }
    let n = b.docs.len() as i64;
    sql.exec(
        "UPDATE cohorts SET manifest_digest = ?, evaluation_set = ?, batch_index_sha256 = ?, task_count = ?,
            r2_puts = r2_puts + 1 WHERE seq = ?",
        &[s(b.manifest_digest.to_string()), s(b.manifest.evaluation_set.to_string()), s(&b.index_sha256), i(n), i(seq)],
    )?;
    let chunks = insert_jobs(sql, seq, &c.plan, n)?;
    let c = cohort(sql, seq)?;
    crate::eval::lanes::plan_score_lanes(sql, cfg, &c, n as u32)?;
    crate::eval::lanes::fit_score_ranges(sql, seq, chunks as u32)?;
    sql.exec("UPDATE cohorts SET scoring_started_ms = ? WHERE seq = ?", &[bind_u64(now_ms)?, i(seq)])?;
    set_phase(sql, seq, "scoring", now_ms, "load: source batch indexed; load manifest signed")?;
    Ok(())
}

// ------------------------------------------------------------------- scoring

/// Timing of one scorer request, for cost accounting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallTiming {
    pub call_ms: u64,
    pub server_ms: u64,
    pub load_ms: u64,
    pub cpu_ms: u64,
}

/// Counts a scorer failure that never reached the model (a lane whose
/// container did not start or load, or a refused batch); pauses the cohort
/// (`scorer_unavailable`) at the plan's bound. Any commit resets the count.
pub fn note_scorer_failure(sql: &impl Sql, seq: i64, now_ms: u64) -> R<bool> {
    sql.exec("UPDATE cohorts SET scorer_failures = scorer_failures + 1 WHERE seq = ?", &[i(seq)])?;
    let c = cohort(sql, seq)?;
    let bound = c.plan.fanout().map(|f| i64::from(f.score.failures_before_pause)).unwrap_or(SCORER_FAILURES_MAX);
    if c.scorer_failures >= bound {
        fault(sql, seq, "scorer_unavailable", now_ms)?;
        return Ok(true);
    }
    Ok(false)
}

// ----------------------------------------------------------------- packaging

#[derive(Clone, Debug)]
pub struct PackageBuild {
    pub index: ReceiptIndex,
    pub package: ScorePackage,
    pub target: CalculationTarget,
    pub components: Value,
    pub chunk_map: Value,
    pub manifest: CohortManifest,
}

pub const SUMMARY_SCHEMA: &str = "slop_ninja_eval_job_summary_v1";

/// A job's private score summary, written by its summary lane to the eval
/// bucket (never the receipts bucket) and deleted with the private texts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobSummary {
    pub schema: String,
    pub job: i64,
    pub artifact: String,
    /// reward-v1: share predictions and whether both role results were
    /// returned, per task in task order (absent in v2 summaries).
    #[serde(default)]
    pub share_ppb: Vec<Option<u32>>,
    #[serde(default)]
    pub roles: Vec<bool>,
    /// Prediction rows read, and the digest of their canonical lines
    /// ([`pred_line`]); both are checked against this object before signing.
    pub count: u32,
    pub rows_sha256: String,
    pub utility: String,
    pub assigned: u32,
    pub succeeded: u32,
    pub failed: u32,
    /// `score::components` of the job (content-free diagnostics).
    pub components: Value,
    /// Involvement per task in task order (ppb); `None` is an attributable failure.
    pub inv_ppb: Vec<Option<u32>>,
}

/// A job summary's bytes besides its per-task arrays: the scalar fields (the
/// schema, job index, artifact and row digests, the utility decimal and the
/// counts) and the components, whose maps hold one entry per task kind and
/// per AI profile of the job's documents. Checked against the widest summary
/// the native tests build (every task kind, 64 AI profiles of 96-byte names:
/// `the_summary_bound_holds_the_widest_summary`); the package footprint
/// counts its summary reads with it (`arena_routes::summary_bytes`).
pub const SUMMARY_FIXED_BYTES: usize = 16 * 1024;

/// One prediction row with its task's private label, as summary lanes read it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredRow {
    pub task: u32,
    /// The task's document set: `batch`, `probe`, `audit_a` or `audit_b`.
    #[serde(default = "batch_set")]
    pub set: String,
    pub kind: String,
    pub label_share_ppb: u32,
    pub outcome: i64,
    pub inv_ppb: Option<u32>,
    pub share_ppb: Option<u32>,
    pub human: Option<String>,
    pub ai: Option<String>,
}

fn batch_set() -> String {
    "batch".into()
}

/// The canonical line of a prediction row, hashed in task order.
pub fn pred_line(r: &PredRow) -> String {
    let o = |v: Option<u32>| v.map_or("-".to_owned(), |x| x.to_string());
    format!(
        "{}|{}|{}|{}|{}|{}\n",
        r.task,
        r.outcome,
        o(r.inv_ppb),
        o(r.share_ppb),
        r.human.as_deref().unwrap_or("-"),
        r.ai.as_deref().unwrap_or("-")
    )
}

/// A page of a job's prediction rows after `after` (task index), with labels.
pub fn job_preds(sql: &impl Sql, seq: i64, job: i64, after: i64, limit: i64) -> R<Vec<PredRow>> {
    sql.exec(
        "SELECT p.task, d.kind, d.share_ppb, p.outcome, p.inv_ppb, p.share_ppb, p.human_profile, p.ai_profile, d.docset
         FROM preds p JOIN docs d ON d.seq = p.seq AND d.task = p.task
         WHERE p.seq = ? AND p.job = ? AND p.task > ? ORDER BY p.task LIMIT ?",
        &[i(seq), i(job), i(after), i(limit.clamp(1, 2000))],
    )?
    .iter()
    .map(|r| {
        Ok(PredRow {
            task: int(r, 0)? as u32,
            set: opt_text(r, 8).unwrap_or_else(batch_set),
            kind: text(r, 1)?.to_owned(),
            label_share_ppb: opt_int(r, 2).unwrap_or(0) as u32,
            outcome: int(r, 3)?,
            inv_ppb: opt_int(r, 4).map(|v| v as u32),
            share_ppb: opt_int(r, 5).map(|v| v as u32),
            human: opt_text(r, 6),
            ai: opt_text(r, 7),
        })
    })
    .collect()
}

/// The count and digest of every prediction row of a job.
pub fn job_rows_digest(sql: &impl Sql, seq: i64, job: i64) -> R<(u32, String)> {
    let mut h = Sha256::new();
    let mut n = 0u32;
    let mut after = -1;
    loop {
        let page = job_preds(sql, seq, job, after, 2000)?;
        for r in &page {
            h.update(pred_line(r).as_bytes());
            n += 1;
        }
        match page.last() {
            Some(r) if page.len() == 2000 => after = i64::from(r.task),
            _ => break,
        }
    }
    Ok((n, hex::encode(h.finalize())))
}

/// Builds a job's summary from all of its rows (a summary lane's work).
pub fn summarize_job(job: i64, artifact: &str, rows: &[PredRow]) -> R<JobSummary> {
    let mut h = Sha256::new();
    let mut tasks = Vec::with_capacity(rows.len());
    let mut batch = Vec::with_capacity(rows.len());
    for r in rows {
        h.update(pred_line(r).as_bytes());
        batch.push(r.set == "batch");
        tasks.push(TaskResult {
            involved: r.kind != "human_only",
            kind: r.kind.clone(),
            source_group: String::new(),
            share_ppb: r.label_share_ppb,
            prediction: match (r.outcome, r.inv_ppb, r.share_ppb) {
                (1, Some(a), Some(b)) => Some((a, b)),
                _ => None,
            },
            human_profile: r.human.clone(),
            ai_profile: r.ai.clone(),
        });
    }
    // Diagnostics and the v2 utility cover the batch only; probe and audit
    // predictions serve the reward-v1 measurements.
    let batch_tasks: Vec<TaskResult> =
        tasks.iter().zip(&batch).filter(|(_, b)| **b).map(|(t, _)| t.clone()).collect();
    let comp = score::components(&batch_tasks).map_err(|_| EvalError::State("components"))?;
    Ok(JobSummary {
        schema: SUMMARY_SCHEMA.into(),
        job,
        artifact: artifact.into(),
        share_ppb: tasks.iter().map(|t| t.prediction.map(|p| p.1)).collect(),
        roles: tasks.iter().map(|t| t.human_profile.is_some() && t.ai_profile.is_some()).collect(),
        count: rows.len() as u32,
        rows_sha256: hex::encode(h.finalize()),
        utility: comp.utility_v0.clone(),
        assigned: comp.assigned,
        succeeded: comp.succeeded,
        failed: comp.failed,
        components: serde_json::to_value(&comp).map_err(|_| EvalError::State("components"))?,
        inv_ppb: tasks.iter().map(|t| t.prediction.map(|p| p.0)).collect(),
    })
}

/// The cohort's receipt index (every receipt row, sorted by the protocol) and
/// the chunk map of its jobs' verified bundles, which together hold every
/// indexed receipt.
pub fn receipt_index(sql: &impl Sql, seq: i64) -> R<(ReceiptIndex, Value)> {
    let c = cohort(sql, seq)?;
    let manifest_digest = match c.manifest_digest.as_deref().and_then(parse_digest) {
        Some(d) => d,
        // A probe build has no manifest: its receipts bind its plan digest.
        None => parse_digest(&c.plan_sha256).ok_or(EvalError::State("manifest digest"))?,
    };
    let mut entries = Vec::new();
    let mut after = 0i64;
    loop {
        let page = sql.exec(
            "SELECT rowid, attempt_id, outcome, object_sha256, object_len FROM receipts
             WHERE seq = ? AND rowid > ? ORDER BY rowid LIMIT 10000",
            &[i(seq), i(after)],
        )?;
        for r in &page {
            let class = match int(r, 2)? {
                1 => OutcomeClass::Prediction,
                2 => OutcomeClass::ModelError,
                3 => OutcomeClass::InvalidOutput,
                4 => OutcomeClass::ParseFailure,
                _ => OutcomeClass::InfrastructureError,
            };
            entries.push(ReceiptIndexEntry {
                attempt: parse_digest(text(r, 1)?).ok_or(EvalError::State("attempt id"))?,
                outcome: class,
                object_sha256: parse_digest(text(r, 3)?).ok_or(EvalError::State("object digest"))?,
                object_len: u32::try_from(int(r, 4)?).map_err(|_| EvalError::State("object length"))?,
            });
        }
        match page.last() {
            Some(r) if page.len() == 10_000 => after = int(r, 0)?,
            _ => break,
        }
    }
    let index = receipts::build_index(manifest_digest, entries).map_err(|e| EvalError::State(e.0))?;
    let receipt_root = index.digest().map_err(|_| EvalError::State("receipt root"))?;
    let mut objects = Vec::new();
    let mut bundled = 0u64;
    for j in sql.exec(
        "SELECT bundle_key, bundle_sha256, bundle_bytes, bundle_receipts FROM jobs WHERE seq = ? ORDER BY job",
        &[i(seq)],
    )? {
        let (Some(key), Some(sha), Some(bytes), Some(n_r)) =
            (opt_text(&j, 0), opt_text(&j, 1), opt_int(&j, 2), opt_int(&j, 3))
        else {
            return Err(EvalError::State("job bundle missing"));
        };
        bundled += n_r as u64;
        objects.push(json!({"key": key, "sha256": sha, "bytes": bytes, "receipts": n_r}));
    }
    if bundled != index.count() {
        return Err(EvalError::State("bundles do not hold every indexed receipt"));
    }
    let chunk_map = json!({
        "format": receipts::CHUNK_FORMAT,
        "cohort_manifest": manifest_digest.to_string(),
        "receipt_root": receipt_root.to_string(),
        "receipt_count": index.count(),
        "objects": objects,
    });
    Ok((index, chunk_map))
}

/// Builds the receipt index, package inputs, the v1 package (through the reward
/// policy), its calculation target and the content-free score components, from
/// the jobs' verified summaries and bundles. The summaries' row counts and
/// digests were checked against this object when their lanes reported them;
/// here every utility is recomputed from the summary's involvement vector and
/// the private labels, and the bundles must hold every indexed receipt.
pub fn prepare_package(sql: &impl Sql, cfg: &EvalConfig, seq: i64, summaries: &[JobSummary]) -> R<PackageBuild> {
    let c = cohort(sql, seq)?;
    if c.phase != "packaging" {
        return Err(EvalError::State("package outside packaging"));
    }
    let manifest_digest = c
        .manifest_digest
        .as_deref()
        .and_then(parse_digest)
        .ok_or(EvalError::State("manifest digest"))?;
    // The flat v1 index, read in pages.
    let mut entries = Vec::new();
    let mut after = 0i64;
    loop {
        let page = sql.exec(
            "SELECT rowid, attempt_id, outcome, object_sha256, object_len FROM receipts
             WHERE seq = ? AND rowid > ? ORDER BY rowid LIMIT 10000",
            &[i(seq), i(after)],
        )?;
        for r in &page {
            let class = match int(r, 2)? {
                1 => OutcomeClass::Prediction,
                2 => OutcomeClass::ModelError,
                3 => OutcomeClass::InvalidOutput,
                4 => OutcomeClass::ParseFailure,
                _ => OutcomeClass::InfrastructureError,
            };
            entries.push(ReceiptIndexEntry {
                attempt: parse_digest(text(r, 1)?).ok_or(EvalError::State("attempt id"))?,
                outcome: class,
                object_sha256: parse_digest(text(r, 3)?).ok_or(EvalError::State("object digest"))?,
                object_len: int(r, 4)? as u32,
            });
        }
        match page.last() {
            Some(r) if page.len() == 10_000 => after = int(r, 0)?,
            _ => break,
        }
    }
    let index = receipts::build_index(manifest_digest, entries).map_err(|e| EvalError::State(e.0))?;
    let receipt_root = index.digest().map_err(|_| EvalError::State("receipt root"))?;
    let jobs = sql.exec(
        "SELECT job, role, artifact, owner, package_sha256, call_ms, server_ms, load_ms, cpu_ms, attempts,
                started_ms, finished_ms, summary_sha256, summary_count, bundle_key, bundle_sha256, bundle_bytes,
                bundle_receipts, isolated_crashes, artifact_failed
         FROM jobs WHERE seq = ? ORDER BY job",
        &[i(seq)],
    )?;
    // Private labels once, in task order.
    let labels = sql.exec(
        "SELECT task, kind, source_group, share_ppb FROM docs WHERE seq = ? AND task IS NOT NULL ORDER BY task",
        &[i(seq)],
    )?;
    let n = c.task_count.unwrap_or(0) as usize;
    if labels.len() != n {
        return Err(EvalError::State("task labels"));
    }
    let mut results: Vec<Vec<TaskResult>> = Vec::new();
    let mut job_scores = Vec::new();
    for j in &jobs {
        let job = int(j, 0)?;
        let sm = summaries.iter().find(|x| x.job == job).ok_or(EvalError::State("job summary missing"))?;
        if sm.schema != SUMMARY_SCHEMA
            || sm.artifact != text(j, 2)?
            || sm.inv_ppb.len() != n
            || i64::from(sm.count) != opt_int(j, 13).unwrap_or(-1)
            || opt_text(j, 12).is_none()
        {
            return Err(EvalError::State("job summary does not match its verified report"));
        }
        let tasks: Vec<TaskResult> = labels
            .iter()
            .zip(&sm.inv_ppb)
            .map(|(l, inv)| {
                let kind = text(l, 1)?.to_owned();
                Ok(TaskResult {
                    involved: kind != "human_only",
                    kind,
                    source_group: text(l, 2)?.to_owned(),
                    share_ppb: opt_int(l, 3).unwrap_or(0) as u32,
                    prediction: inv.map(|v| (v, 0)),
                    human_profile: None,
                    ai_profile: None,
                })
            })
            .collect::<R<_>>()?;
        let utility = score::utility(&tasks).map_err(|_| EvalError::State("utility"))?;
        let succeeded = tasks.iter().filter(|t| t.prediction.is_some()).count() as u32;
        if utility.get().to_string() != sm.utility || succeeded != sm.succeeded || sm.assigned != n as u32 {
            return Err(EvalError::State("job summary utility differs from its predictions"));
        }
        job_scores.push(JobScore {
            artifact: parse_digest(text(j, 2)?).ok_or(EvalError::State("artifact"))?,
            score: utility,
            assigned: n as u32,
            succeeded,
            failed: n as u32 - succeeded,
        });
        results.push(tasks);
    }
    let mut reference = None;
    let mut previous = None;
    let mut entrants = Vec::new();
    for (k, j) in jobs.iter().enumerate() {
        match text(j, 1)? {
            "reference" => reference = Some(job_scores[k].clone()),
            "previous_reference" => previous = Some(job_scores[k].clone()),
            _ => {
                let owner: crate::eval::plan::IdentityJson =
                    serde_json::from_str(text(j, 3)?).map_err(|_| EvalError::State("owner"))?;
                entrants.push((owner.identity().map_err(|e| EvalError::Plan(e.0))?, job_scores[k].clone()));
            }
        }
    }
    let manifest = prepare_manifest_again(sql, cfg, &c)?;
    let inputs = PackageInputs {
        manifest: manifest.clone(),
        manifest_digest,
        revision: 0,
        reference: reference.ok_or(EvalError::State("reference job"))?,
        previous_reference: previous,
        entrants,
        receipt_root,
        receipt_count: index.count(),
    };
    let policy = package::policy(&c.plan.reward_rule).ok_or(EvalError::Plan("reward rule"))?;
    let pkg = policy.package(&inputs).map_err(|e| EvalError::State(e.0))?;
    let predecessor = if c.plan.is_load() {
        None
    } else {
        sql.exec("SELECT target_digest FROM eval_targets ORDER BY seq DESC LIMIT 1", &[])?
            .first()
            .map(|r| text(r, 0).map(str::to_owned))
            .transpose()?
            .and_then(|d| parse_digest(&d))
    };
    let target = package::target(&pkg, predecessor).map_err(|e| EvalError::State(e.0))?;

    // Content-free components, paired against R_k (job 0), and near-duplicate clusters.
    let mut per_job = Vec::new();
    for (k, j) in jobs.iter().enumerate() {
        let sm = summaries.iter().find(|x| x.job == int(j, 0).unwrap_or(-1)).ok_or(EvalError::State("summary"))?;
        let rms: Vec<Value> = (0..jobs.len())
            .filter(|m| *m != k)
            .map(|m| json!({"job": m, "rms": score::rms(&results[k], &results[m])}))
            .collect();
        per_job.push(json!({
            "job": k, "role": text(j, 1)?, "artifact": text(j, 2)?,
            "owner": serde_json::from_str::<Value>(text(j, 3)?).unwrap_or(Value::Null),
            "package_sha256": text(j, 4)?,
            "components": sm.components,
            "paired_vs_reference": score::paired(&results[0], &results[k]),
            "rms_vs": rms,
            "isolated_crashes": int(j, 18)?,
            "artifact_failed": opt_text(j, 19),
            "scoring": {"call_ms": uint(j, 5)?, "server_ms": uint(j, 6)?, "load_ms": uint(j, 7)?,
                "cpu_ms": uint(j, 8)?, "requests": uint(j, 9)?,
                "wall_ms": opt_int(j, 11).zip(opt_int(j, 10)).map(|(f, s)| f - s)},
        }));
    }
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    for k in 0..jobs.len() {
        let pkg_k = text(&jobs[k], 4)?;
        let home = clusters.iter_mut().find(|cl| {
            cl.iter().any(|m| {
                text(&jobs[*m], 4).is_ok_and(|p| p == pkg_k)
                    || score::rms(&results[k], &results[*m]) <= score::NEAR_DUPLICATE_RMS
            })
        });
        match home {
            Some(cl) => cl.push(k),
            None => clusters.push(vec![k]),
        }
    }
    // The chunk map lists one bundle per job; together they hold every receipt.
    let mut objects = Vec::new();
    let mut bundled = 0u64;
    for j in &jobs {
        let (Some(key), Some(sha), Some(bytes), Some(n_r)) =
            (opt_text(j, 14), opt_text(j, 15), opt_int(j, 16), opt_int(j, 17))
        else {
            return Err(EvalError::State("job bundle missing"));
        };
        bundled += n_r as u64;
        objects.push(json!({"key": key, "sha256": sha, "bytes": bytes, "receipts": n_r}));
    }
    if bundled != index.count() {
        return Err(EvalError::State("bundles do not hold every indexed receipt"));
    }
    let chunk_map = json!({
        "format": receipts::CHUNK_FORMAT,
        "cohort_manifest": manifest_digest.to_string(),
        "receipt_root": receipt_root.to_string(),
        "receipt_count": index.count(),
        "objects": objects,
    });
    let components = json!({
        "schema": "slop_ninja_eval_score_components_v1",
        "cohort": c.plan.cohort, "label": c.label,
        "cohort_manifest": manifest_digest.to_string(),
        "reward_rule": c.plan.reward_rule,
        "reward_note": "testnet-reward-v0 is the active rule; reward-v1 mapping is under review and plugs in through RewardPolicy",
        "load": c.plan.is_load(),
        "task_count": c.task_count, "receipt_root": receipt_root.to_string(), "receipt_count": index.count(),
        "near_duplicate_rule": {"exact_package": true, "rms_at_most": score::NEAR_DUPLICATE_RMS},
        "crash_rule": CRASH_RULE,
        "artifact_rule": ARTIFACT_RULE,
        "clusters": clusters,
        "jobs": per_job,
        "previous_reference": if c.plan.previous_reference.is_some() { json!("scored") } else { json!("absent: R_k is R_0") },
    });
    Ok(PackageBuild { index, package: pkg, target, components, chunk_map, manifest })
}

/// The manifest's scored artifacts besides R_k: the entrants and, when present,
/// R_{k-1}. The v1 manifest has no separate predecessor field, so R_{k-1} is listed
/// as a scored artifact (its receipts then verify) but never credited: only
/// entrants appear in the package's scores.
pub(crate) fn manifest_artifacts(plan: &CohortPlan) -> R<Vec<Digest>> {
    plan.entrants
        .iter()
        .chain(plan.previous_reference.iter())
        .map(ArtifactJson::id)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| EvalError::Plan(e.0))
}

fn prepare_manifest_again(sql: &impl Sql, cfg: &EvalConfig, c: &Cohort) -> R<CohortManifest> {
    // The manifest is a pure function of the plan, the opening block and the
    // sealed batch index; rebuild it and check the stored digest.
    let rows = sql.exec(
        "SELECT evaluation_set, task_count, manifest_digest FROM cohorts WHERE seq = ?",
        &[i(c.seq)],
    )?;
    let r = rows.first().ok_or(EvalError::State("cohort"))?;
    let evaluation_set = parse_digest(text(r, 0)?).ok_or(EvalError::State("evaluation set"))?;
    let artifacts = manifest_artifacts(&c.plan)?;
    let m = package::manifest(
        cfg.chain.clone(),
        c.plan.cohort,
        &c.label,
        c.opened_block.ok_or(EvalError::State("opened block"))?,
        evaluation_set,
        int(r, 1)? as u32,
        c.plan.reference.id().map_err(|e| EvalError::Plan(e.0))?,
        c.plan.max_attempts,
        c.plan.allow_list().map_err(|e| EvalError::Plan(e.0))?,
        c.plan.burn_identity.identity().map_err(|e| EvalError::Plan(e.0))?,
        artifacts,
    )
    .map_err(|e| EvalError::State(e.0))?;
    if m.digest().map_err(|_| EvalError::State("manifest digest"))?.to_string() != text(r, 2)? {
        return Err(EvalError::State("manifest rebuild differs"));
    }
    Ok(m)
}

/// Commits the published package: digests, then the transition phase.
pub fn commit_package(sql: &impl Sql, seq: i64, b: &PackageBuild, puts: u64, now_ms: u64) -> R<()> {
    let c = cohort(sql, seq)?;
    if c.phase != "packaging" {
        return Err(EvalError::State("package outside packaging"));
    }
    let o = observed(sql)?;
    sql.exec(
        "UPDATE cohorts SET receipt_root = ?, receipt_count = ?, package_digest = ?, target_digest = ?,
            packaged_epoch = ?, packaged_active = ?, r2_puts = r2_puts + ? WHERE seq = ?",
        &[
            s(b.package.receipt_root.to_string()),
            bind_u64(b.package.receipt_count)?,
            s(b.package.digest().map_err(|_| EvalError::State("package digest"))?.to_string()),
            s(b.target.digest().map_err(|_| EvalError::State("target digest"))?.to_string()),
            o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)),
            bind_u64(o.active_tempos)?,
            bind_u64(puts)?,
            i(seq),
        ],
    )?;
    if c.plan.is_load() {
        // Load packages are for verification only: no transition, no target.
        sql.exec("UPDATE docs SET text = NULL WHERE seq = ?", &[i(seq)])?;
        set_phase(sql, seq, "closed", now_ms, "load: package stored for verification; never a target")?;
        return Ok(());
    }
    set_phase(sql, seq, "transition", now_ms, "package, target and receipts durable")?;
    advance(sql, now_ms)?;
    Ok(())
}

pub fn note_step(sql: &impl Sql, seq: i64, ms: u64) -> R<()> {
    sql.exec(
        "UPDATE cohorts SET steps = steps + 1, step_ms = step_ms + ? WHERE seq = ?",
        &[bind_u64(ms)?, i(seq)],
    )?;
    Ok(())
}

// -------------------------------------------------------------------- status

/// Content-free status: phases, counts, costs, timings and public digests.
pub fn status(sql: &impl Sql, now_ms: u64) -> R<Value> {
    let o = observed(sql)?;
    let held = crate::eval::retention::held_seqs(sql)?;
    let mut out = Vec::new();
    for c in cohorts(sql)? {
        let q = |query: &str| -> R<Vec<Row>> { Ok(sql.exec(query, &[i(c.seq)])?) };
        let docs: Vec<Value> = q("SELECT kind, status, COUNT(*), SUM(attempts), SUM(infra_failures) FROM docs
                WHERE seq = ? GROUP BY kind, status")?
            .iter()
            .map(|r| Ok(json!({"kind": text(r, 0)?, "status": text(r, 1)?, "count": int(r, 2)?,
                "attempts": opt_int(r, 3), "pending_outage_failures": opt_int(r, 4)})))
            .collect::<R<_>>()?;
        let chunk_attempts: Vec<Value> = q("SELECT attempt, recorded, status, COUNT(*) FROM chunks WHERE seq = ?
                GROUP BY attempt, recorded, status ORDER BY attempt, recorded, status")?
            .iter()
            .map(|r| Ok(json!({"allocated": int(r, 0)?, "recorded": int(r, 1)?, "status": text(r, 2)?,
                "chunks": int(r, 3)?})))
            .collect::<R<_>>()?;
        let rejects: BTreeMap<String, u64> = q("SELECT rejects FROM docs WHERE seq = ? AND rejects != ''")?
            .iter()
            .flat_map(|r| text(r, 0).unwrap_or("").split(';').filter(|x| !x.is_empty()).map(str::to_owned).collect::<Vec<_>>())
            .fold(BTreeMap::new(), |mut m, k| {
                *m.entry(k).or_default() += 1;
                m
            });
        // The same codes by document kind (rewrites are `mixed`, scratch
        // writing `model_only`), so a rejection window is checked per kind.
        let mut rejects_by_kind: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        for r in &q("SELECT kind, rejects FROM docs WHERE seq = ? AND rejects != ''")? {
            let m = rejects_by_kind.entry(text(r, 0)?.to_owned()).or_default();
            for k in text(r, 1)?.split(';').filter(|x| !x.is_empty()) {
                *m.entry(k.to_owned()).or_default() += 1;
            }
        }
        let usage: Vec<Value> = q("SELECT model, calls, accepted, rejected, errors, prompt_tokens, completion_tokens,
                cost_microusd, ms, throttled, fenced FROM gen_usage WHERE seq = ? ORDER BY model")?
            .iter()
            .map(|r| Ok(json!({"model": text(r, 0)?, "calls": int(r, 1)?, "accepted": int(r, 2)?,
                "rejected": int(r, 3)?, "errors": int(r, 4)?, "prompt_tokens": int(r, 5)?,
                "completion_tokens": int(r, 6)?, "cost_microusd": int(r, 7)?, "ms": int(r, 8)?,
                "throttled": int(r, 9)?, "fenced": int(r, 10)?})))
            .collect::<R<_>>()?;
        // Many jobs: a compact form (per-status counts plus any faulted job).
        let job_rows = q("SELECT job, role, artifact, status, chunks, done_chunks, call_ms, server_ms, load_ms,
                cpu_ms, attempts, started_ms, finished_ms, fault, isolated_crashes, artifact_failed, artifact_failures
                FROM jobs WHERE seq = ? ORDER BY job")?;
        let job_json = |r: &Row| -> R<Value> {
            Ok(json!({"job": int(r, 0)?, "role": text(r, 1)?, "artifact": text(r, 2)?,
                "status": text(r, 3)?, "chunks": int(r, 4)?, "done_chunks": int(r, 5)?, "call_ms": int(r, 6)?,
                "server_ms": int(r, 7)?, "load_ms": int(r, 8)?, "cpu_ms": int(r, 9)?, "requests": int(r, 10)?,
                "started_ms": opt_int(r, 11), "finished_ms": opt_int(r, 12), "fault": opt_text(r, 13),
                "isolated_crashes": int(r, 14)?, "artifact_failed": opt_text(r, 15), "artifact_failures": int(r, 16)?}))
        };
        let jobs: Vec<Value> = if job_rows.len() <= 8 {
            job_rows.iter().map(job_json).collect::<R<_>>()?
        } else {
            job_rows
                .iter()
                .filter(|r| {
                    opt_text(r, 13).is_some()
                        || opt_int(r, 14).unwrap_or(0) > 0
                        || opt_text(r, 15).is_some()
                        || int(r, 0).is_ok_and(|j| j < 2)
                })
                .map(job_json)
                .collect::<R<_>>()?
        };
        let mut job_status: BTreeMap<String, i64> = BTreeMap::new();
        for r in &job_rows {
            *job_status.entry(text(r, 3)?.to_owned()).or_default() += 1;
        }
        let lanes = if c.plan.fanout.is_some() { crate::eval::lanes::lane_status(sql, &c, now_ms)? } else { Value::Null };
        let log: Vec<Value> = q("SELECT phase, at_ms, confirmed_number, epoch_index, note FROM phase_log WHERE seq = ? ORDER BY rowid")?
            .iter()
            .map(|r| Ok(json!({"phase": text(r, 0)?, "at_ms": int(r, 1)?, "confirmed": opt_int(r, 2),
                "epoch": opt_int(r, 3), "note": opt_text(r, 4)})))
            .collect::<R<_>>()?;
        let row = &q("SELECT manifest_digest, evaluation_set, task_count, receipt_root, receipt_count, package_digest,
                target_digest, steps, step_ms, r2_puts, draw, opened_epoch, opened_block, packaged_epoch, closed_epoch,
                counts, abandoned_reason, abandoned_fault FROM cohorts WHERE seq = ?")?[0];
        let receipts = int(&q("SELECT COUNT(*) FROM receipts WHERE seq = ?")?[0], 0)?;
        let retention = crate::eval::retention::cohort_status(sql, c.seq, &held)?;
        let v3 = if c.plan.is_v3() {
            let st = crate::eval::v3::state(sql, c.seq)?;
            let e = &q("SELECT admission_closed_epoch, published_epoch, closure_digest FROM cohorts WHERE seq = ?")?[0];
            json!({"probe_build": c.plan.is_probe_build(), "mode": st.mode, "promoted": st.promoted,
                "unexecuted": st.unexecuted, "batch_tasks": st.batch_tasks, "probe_tasks": st.probe_tasks,
                "audit_tasks": st.audit_tasks, "reference_index": st.reference_index,
                "admission_closed_epoch": opt_int(e, 0), "published_epoch": opt_int(e, 1), "closure": opt_text(e, 2)})
        } else {
            Value::Null
        };
        out.push(json!({
            "cohort": c.plan.cohort, "label": c.label, "phase": c.phase, "fault": c.fault,
            "n": c.plan.n, "k_tempos": c.plan.k_tempos, "plan_sha256": c.plan_sha256,
            "opened_epoch": opt_int(row, 11), "opened_block": opt_int(row, 12),
            "packaged_epoch": opt_int(row, 13), "closed_epoch": opt_int(row, 14),
            "opened_active": c.opened_active, "packaged_active": c.packaged_active,
            "admission_tempos": c.plan.admission_tempos, "transition_tempos": c.plan.transition_tempos,
            "max_attempts": c.plan.max_attempts, "scorer_failures": c.scorer_failures,
            "counts": opt_text(row, 15).and_then(|d| serde_json::from_str::<Value>(&d).ok()),
            "abandoned": opt_text(row, 16).map(|r| json!({"reason": r, "fault": opt_text(row, 17)})),
            "chunk_attempts": chunk_attempts,
            "draw": opt_text(row, 10).and_then(|d| serde_json::from_str::<Value>(&d).ok()),
            "docs": docs, "rejects": rejects, "rejects_by_kind": rejects_by_kind, "generation": usage,
            "generation_cost_microusd": c.gen_cost_microusd,
            "budget_microusd": c.plan.generation_budget_microusd,
            "jobs": jobs, "job_count": job_rows.len(), "job_status": job_status, "lanes": lanes, "load": c.plan.is_load(),
            "receipts_recorded": receipts, "receipt_retention": retention,
            "manifest": opt_text(row, 0), "evaluation_set": opt_text(row, 1), "task_count": opt_int(row, 2),
            "receipt_root": opt_text(row, 3), "receipt_count": opt_int(row, 4),
            "package": opt_text(row, 5), "target": opt_text(row, 6),
            "steps": int(row, 7)?, "step_ms": int(row, 8)?, "r2_puts": int(row, 9)?,
            "reward_v1": v3,
            "log": log,
        }));
    }
    let used = int(&sql.exec("SELECT COUNT(*) FROM pool_used", &[])?[0], 0)?;
    Ok(json!({
        "observed": o, "kick_age_ms": now_ms.saturating_sub(o.kick_ms),
        "active_tempo_rule": ACTIVE_TEMPO_RULE, "resumable_faults": RESUMABLE_FAULTS,
        "pool_rows_used": used, "receipt_retention": crate::eval::retention::status(sql)?, "cohorts": out,
    }))
}

/// Ensures there is a reason to keep stepping: [`blocking_work`], or a due
/// receipt deletion (`eval::retention`). A predecessor notice is not work:
/// it is retried whenever a step runs, so an unreachable predecessor never
/// keeps this object's alarm armed on its own.
pub fn has_work(sql: &impl Sql, cfg: &EvalConfig) -> R<bool> {
    Ok(blocking_work(sql, cfg)? || crate::eval::retention::has_work(sql)?)
}

/// [`has_work`] counting admission's backed-off steps, clean-ups and object
/// deletes only once due at `now_ms`: what re-arms the step alarm.
pub fn has_due_work(sql: &impl Sql, cfg: &EvalConfig, now_ms: u64) -> R<bool> {
    Ok(work(sql, cfg, Some(now_ms))? || crate::eval::retention::has_work(sql)?)
}

/// The work a succession or an authority switch waits for (`work` in the
/// status; `eval::succession::supersede`, `config::deploy_check`): an active
/// cohort with work, a closure, a finished cohort whose spend is not yet
/// charged to the ledger or whose private results are not yet purged, or
/// admission or calendar work under `cfg` (a retired authority's calendar
/// opens nothing, so it is no reason to step). Due receipt deletions are not
/// included: a superseded authority finishes them under its successor's
/// notices (`eval::retention`), so waiting for them adds nothing, and a
/// storage fault there never holds up a restart.
pub fn blocking_work(sql: &impl Sql, cfg: &EvalConfig) -> R<bool> {
    work(sql, cfg, None)
}

fn work(sql: &impl Sql, cfg: &EvalConfig, due_by: Option<u64>) -> R<bool> {
    Ok(active(sql)?.is_some()
        || crate::eval::admission::has_work(sql, cfg, due_by)?
        || crate::eval::v3::closure_due(sql)?.is_some()
        || !sql
            .exec(
                "SELECT 1 FROM cohorts WHERE (phase IN ('transition','closed','abandoned')
                   AND reserved_microusd IS NOT NULL AND settled_microusd IS NULL)
                   OR (phase IN ('closed','abandoned') AND purged_ms IS NULL) LIMIT 1",
                &[],
            )?
            .is_empty())
}
