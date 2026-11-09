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
/// never count: they back off and, past the plan's
/// `fanout.generation.failures_before_pause`, pause the cohort.
pub const MAX_GEN_ATTEMPTS: i64 = 4;
const GEN_BACKOFF_BASE_MS: u64 = 5_000;
const GEN_BACKOFF_MAX_MS: u64 = 120_000;
/// Consecutive scorer failures that never reached the model (a slot that does
/// not start or load, a refused batch) that pause the cohort (`scorer_unavailable`).
pub const SCORER_FAILURES_MAX: i64 = 6;
/// Backoff of a chunk whose request never reached the model.
pub const SCORER_BACKOFF_MS: u64 = 30_000;
/// A heartbeat at most this old counts the coordinator as alive: claims are
/// granted and the alarm re-arms itself. It never credits a tempo
/// ([`ACTIVE_TEMPO_RULE`] reads chain epochs only).
pub const HEARTBEAT_FRESH_MS: u64 = 600_000;
/// The active-tempo rule, by chain epochs observed and never by wall-clock
/// time: an observation is live when its epoch is the chain head's epoch at
/// the time it is made (the tick reads the epoch index at the RPC-confirmed
/// watermark and at the RPC finalized head). An increase of the epoch index
/// by exactly one credits one active tempo when the earlier epoch was
/// observed live at least once and the new observation is live. Every other
/// increase (a jump of several, or replayed epochs while a lagging watermark
/// catches up after an outage) credits none: those tempos passed while the
/// coordinator was not observing them, are counted as uncredited and never
/// consume a minimum, so the same cohort resumes at later boundaries. A
/// tick whose head epoch is unknown (a failed or budget-deferred head read)
/// proves no lag: it never moves the stored epoch to a new index, so the
/// next tick with a head reading decides that increment.
pub const ACTIVE_TEMPO_RULE: &str = "live_epoch_single_increment_v3";
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

/// v16: stuck-state recovery. Whether the stored epoch observation's epoch
/// was ever observed while it was the chain head's (`epoch_live`, the
/// [`ACTIVE_TEMPO_RULE`]); each cohort's failing root step (its code, since
/// when, how many times, and when it may run again, so a closure that fails
/// yields to other work: [`note_step_error`]); and the spend measured when
/// an operator cleared a budget fault, from which the plan's budget counts
/// again ([`clear_fault`]).
pub const SCHEMA_V16: &[&str] = &[
    "ALTER TABLE eval_obs ADD COLUMN epoch_live INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN step_error TEXT",
    "ALTER TABLE cohorts ADD COLUMN step_error_since_ms INTEGER",
    "ALTER TABLE cohorts ADD COLUMN step_errors INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN step_retry_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN score_base_nanousd INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN gen_base_microusd INTEGER NOT NULL DEFAULT 0",
];

/// v18: the stored epoch index at a cohort's first failure of its failing
/// root step ([`note_step_error`]), so a transient step error that persists
/// is judged (and alerted) by chain progress, never by wall time.
pub const SCHEMA_V18: &[&str] = &["ALTER TABLE cohorts ADD COLUMN step_error_since_epoch INTEGER"];

/// v19: stuck-states fix round 2. A chunk's attempt allowance once extended
/// past the plan's `max_attempts` for infrastructure failures
/// (`chunks.attempt_limit`, `lanes::extend_attempts`) with the cohort's
/// extension count and the receipts those extensions may add; the first
/// budget guard a cohort passed, which alerts and pauses nothing
/// (`cohorts.budget_alert`, [`note_budget`]); and the epoch index at the
/// confirmed watermark as the last tick read it (`eval_obs.watermark_epoch`),
/// so an increment deferred for an unknown head epoch is visible.
pub const SCHEMA_V19: &[&str] = &[
    "ALTER TABLE chunks ADD COLUMN attempt_limit INTEGER",
    "ALTER TABLE cohorts ADD COLUMN attempt_extensions INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN extension_receipts INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN budget_alert TEXT",
    "ALTER TABLE eval_obs ADD COLUMN watermark_epoch INTEGER",
];

/// v24: the rule floor of this store. Records sealed from here are
/// `reward-v2` / `quant-v1` (manifest v5, package v5, closure v2, target v3)
/// and no earlier build decodes them, so an earlier build refuses to open
/// this store (`NewerStoredVersion`) rather than run against cohorts it
/// cannot package or close. The table records the floor for operators; the
/// version number is what enforces it.
pub const SCHEMA_V24: &[&str] = &[
    "CREATE TABLE rule_floor (
        id INTEGER PRIMARY KEY CHECK (id = 1), reward_rule TEXT NOT NULL, quant_rule TEXT NOT NULL
    ) STRICT",
    "INSERT INTO rule_floor (id, reward_rule, quant_rule) VALUES (1, 'reward-v2', 'quant-v1')",
];

/// v20: the automatic resume probe of an outage fault ([`probe_due`],
/// [`probe_begin`], [`probe_done`]: when the next probe may run, how many
/// ran, consecutive failures, their measured time and spend, the bound
/// charged for a probe in flight, and the last outcome), the cause recorded
/// with a fault (`fault_cause`, and `scorer_cause` while scorer failures
/// count), whether the cohort was cleared automatically with no progress
/// since (`auto_cleared`: how many in a row, which backs the probe interval
/// off, [`probe_interval_ms`]), which fault a probe was started for
/// (`fault_n`, counted per cohort, and `probe_fault_n`) and until when it
/// may still run (`probe_until_ms`), when the signed abandonment record of
/// an abandoned cohort was written or may be retried
/// ([`Work::AbandonMarker`]), and the jobs whose attempts no further
/// extension fits under the receipt bound (`receipt_bound_reached`), and the
/// retry time of a bookkeeping step that failed (`bookkeeping_retry`,
/// [`bookkeeping_failed`]).
pub const SCHEMA_V20: &[&str] = &[
    "ALTER TABLE cohorts ADD COLUMN probe_after_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN probes INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN probe_failures INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN probe_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN probe_nanousd INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN probe_last TEXT",
    "ALTER TABLE cohorts ADD COLUMN abandon_marker_ms INTEGER",
    "ALTER TABLE cohorts ADD COLUMN abandon_marker_retry_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN abandon_marker_error TEXT",
    "ALTER TABLE cohorts ADD COLUMN fault_cause TEXT",
    "ALTER TABLE cohorts ADD COLUMN scorer_cause TEXT",
    "ALTER TABLE cohorts ADD COLUMN auto_cleared INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN probe_started_ms INTEGER",
    "ALTER TABLE cohorts ADD COLUMN probe_pending_nanousd INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE cohorts ADD COLUMN probe_until_ms INTEGER",
    "ALTER TABLE cohorts ADD COLUMN probe_fault_n INTEGER",
    "ALTER TABLE cohorts ADD COLUMN fault_n INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE jobs ADD COLUMN receipt_bound_reached INTEGER NOT NULL DEFAULT 0",
    "CREATE TABLE bookkeeping_retry (kind TEXT PRIMARY KEY, after_ms INTEGER NOT NULL, code TEXT NOT NULL)",
];

/// A plan names fresh stock without an admitted external entrant.
pub const FRESH_STOCK_REFUSED: &str =
    "pool (fresh stock is drawn only for a cohort with an admitted external entrant; list exposed pools in EXPOSED_POOLS)";

/// The draw's fault code for a fresh pool without an admitted outside entrant.
pub const FRESH_STOCK_REFUSED_CODE: &str = "fresh_stock_refused";

/// A plan names a further parent pool the deployment does not list as
/// exposed (`reward_v1.parent_pools`: exposed stock only).
pub const PARENT_POOL_REFUSED: &str =
    "reward_v1.parent_pools (parent pools are exposed stock only; list them in EXPOSED_POOLS)";

/// A plan names the testnet-only exposed-pool reuse rule on another chain.
pub const EXPOSED_REUSE_TESTNET_ONLY: &str = "reward_v1.reuse.exposed (testnet only)";

/// Fault codes an operator clear can resume, because the cohort's work can
/// still finish: outages, refused scorer slots, platform refusals (an arena
/// or object bound, which a corrected deployment lifts), a root step that
/// failed the same way under the same inputs ([`STEP_REFUSED`]), a roster
/// freeze the engine refused ([`crate::eval::admission::FREEZE_FAULT`]: the
/// clear runs the freeze again on the same roster), and
/// [`UNRESOLVED_INFRASTRUCTURE`]. Every other fault is terminal under the
/// plan (no human or case is left) and needs [`abandon`]. `package_refused`
/// resumes once a corrected build is deployed: the frozen inputs are
/// unchanged and packaging runs again from them. `reference_failure` is
/// terminal by cause: it names a reference that answered invalidly (an
/// answer that does not parse or holds no valid prediction). A reference's
/// unanswered document (a scorer error, a crash reproduced on a proven
/// instance) is never attributed to it: its receipt keeps the model error,
/// marked unattributed, and the document stays unresolved
/// (`lane_logic::unanswered`). The budget
/// codes are kept for stores that paused under the earlier guard: the
/// guards now only alert ([`note_budget`]), and a clear of an old budget
/// fault resumes the cohort as before. The outage faults among these
/// ([`PROBED_FAULTS`], [`EVIDENCE_CLEARED_FAULTS`]) also clear by themselves
/// once the work they paused succeeds again ([`auto_clear`]).
pub const RESUMABLE_FAULTS: &[&str] = &[
    GENERATION_UNAVAILABLE,
    SCORER_UNAVAILABLE,
    crate::eval::admission::FREEZE_FAULT,
    UNRESOLVED_INFRASTRUCTURE,
    "package_refused",
    crate::eval::commitments::SNAPSHOT_DISCARDED,
    crate::eval::admission::FRAMING_UNAVAILABLE,
    crate::eval::commitments::STATE_UNAVAILABLE,
    "arena_capacity",
    "arena_defect",
    "object_over_bound",
    STEP_REFUSED,
    SCORING_BUDGET,
    GENERATION_BUDGET,
];

/// Generation's outage fault: an item's consecutive outages reached the
/// plan's `fanout.generation.failures_before_pause`.
pub const GENERATION_UNAVAILABLE: &str = "generation_unavailable";
/// The scorer's outage fault: scorer slots that never reached the model
/// reached `fanout.score.failures_before_pause`, or (admission) a verdict's
/// platform failures did.
pub const SCORER_UNAVAILABLE: &str = "scorer_unavailable";
/// Outage faults of a work phase that an automatic probe may clear
/// ([`probe_due`]): while the cohort is paused by one whose recorded cause
/// the probe exercises ([`probed_cause`]), each tick runs one bounded probe
/// of the paused platform, with no count or time limit, and the first that
/// succeeds resumes the same cohort ([`probe_done`]). A probe that keeps
/// failing only keeps the cohort paused. A cohort that paused again with no
/// progress since its last automatic clear is still probed, after a wait
/// that doubles with each such clear ([`probe_interval_ms`]). A cause the
/// probe cannot see (a store failure, a refused commit, a definitive
/// refusal) is left to an operator clear, paused and visible.
pub const PROBED_FAULTS: &[&str] = &[GENERATION_UNAVAILABLE, SCORER_UNAVAILABLE];

/// Admission's outage faults, cleared once the subject that raised them
/// succeeds and no other subject still meets its pause condition
/// ([`crate::eval::admission`]'s `outage_passed`): the session whose framing
/// kept being cut off framed, the commitment whose block's state was missing
/// was served, or the verdict that kept meeting platform failures reached
/// the scorer. The attempts go on with their backoff while the cohort is
/// paused, so they are the probe; nothing extra is spent.
pub const EVIDENCE_CLEARED_FAULTS: &[&str] = &[
    crate::eval::admission::FRAMING_UNAVAILABLE,
    crate::eval::commitments::STATE_UNAVAILABLE,
    SCORER_UNAVAILABLE,
];

/// How often a paused cohort's outage probe runs: once per fallback tick
/// (the coordinator's one-minute cron), however many kicks arrive, counted
/// from the end of the previous probe.
pub const PROBE_EVERY_MS: u64 = 60_000;

/// The most times the probe interval doubles after automatic clears that
/// saw no progress ([`probe_interval_ms`]): the longest wait, 64 one-minute
/// ticks, stays under one default subnet tempo (360 blocks at the 12 s
/// block time, `watchdog::BLOCK_TIME_MS`), so a platform that recovers is
/// probed within the tempo it recovers in. There is no count or time limit.
pub const PROBE_BACKOFF_DOUBLINGS: i64 = 6;

/// The wait between a paused cohort's probes after `no_progress_clears`
/// automatic clears in a row that saw no progress (`auto_cleared`):
/// [`PROBE_EVERY_MS`] doubled once per clear, at most
/// [`PROBE_BACKOFF_DOUBLINGS`] times. Progress resets the count
/// ([`note_progress`]).
pub fn probe_interval_ms(no_progress_clears: i64) -> u64 {
    PROBE_EVERY_MS << no_progress_clears.clamp(0, PROBE_BACKOFF_DOUBLINGS)
}

/// What caused a scorer failure counted toward `scorer_unavailable`
/// ([`note_scorer_failure`]), recorded with the fault (`fault_cause`) so only
/// a cause the automatic probe exercises is cleared by it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScorerCause {
    /// A scorer slot that did not start, load or take the request, or an
    /// isolation instance that could not be restarted: what the probe
    /// exercises (a fresh instance of the artifact scoring a document).
    Slot,
    /// A bucket read or write of the lane (batch shards, results or receipt
    /// objects): the probe touches no bucket.
    Store,
    /// The cohort object refused a lane's commit: the probe commits nothing.
    Commit,
    /// A failure the lane did not classify (a request from an older lane
    /// build, a receipt build error): never cleared automatically.
    #[default]
    Unclassified,
}

impl ScorerCause {
    pub fn code(self) -> &'static str {
        match self {
            Self::Slot => "slot",
            Self::Store => "store",
            Self::Commit => "commit",
            Self::Unclassified => "unclassified",
        }
    }
}

/// Whether the recorded cause of a `scorer_unavailable` fault is one the
/// probe exercises: slot failures of the reference (`slot`) or of the
/// previous reference (`slot:previous_reference`), each probed with its own
/// artifact.
pub fn probed_cause(cause: Option<&str>) -> bool {
    matches!(cause, Some("slot" | "slot:previous_reference"))
}

/// Whether fault `code` of a cohort in `phase`, raised by `cause`, clears
/// without an operator: a probed outage in a work phase ([`probe_due`]:
/// a generation outage, or a scorer outage of a [`probed_cause`]), or an
/// admission outage while the cohort admits (its subjects re-read each
/// admission step, [`EVIDENCE_CLEARED_FAULTS`]). Every other fault waits for
/// an operator clear or an abandonment.
pub fn self_clearing(phase: &str, code: &str, cause: Option<&str>) -> bool {
    if WORK_PHASES.contains(&phase) {
        code == GENERATION_UNAVAILABLE || (code == SCORER_UNAVAILABLE && probed_cause(cause))
    } else {
        phase == "admission" && EVIDENCE_CLEARED_FAULTS.contains(&code)
    }
}

/// A cohort's root step (draw, seal, package or closure) failed in a way no
/// wait changes: a content check (a digest or object that does not match its
/// record), a missing object or key, or an engine refusal under the same
/// stored inputs ([`step_error_is_deterministic`]). The cohort pauses with
/// its work and evidence kept; the failing code is in its phase log and its
/// status (`step_error`). An operator clear runs the step again, after the
/// cause is corrected (an object restored, a key or build deployed).
pub const STEP_REFUSED: &str = "step_refused";
/// The scoring and generation budget guards ([`note_budget`]).
pub const SCORING_BUDGET: &str = "scoring_budget";
pub const GENERATION_BUDGET: &str = "generation_budget";

/// A job's chunk still has unresolved documents and no further attempt fits:
/// its attempt allowance is at the attempt number's range, or one more
/// round of the plan's `max_attempts` per unresolved document would take the
/// cohort's receipts past one signed receipt index
/// (`lanes::extend_attempts`). Every attempt that leaves a document
/// unresolved is infrastructure-caused (a model failure resolves its
/// document as a model error), so until those bounds a chunk's attempts are
/// extended by themselves and no outage, eviction or expired lease ends
/// one. The job faults alone; the cohort pauses at the end of scoring.
/// Resumable: a clear returns the faulted jobs to scoring, and their next
/// claims extend again under the deployed build's receipt bound, or fault
/// again (visibly) if it still holds no further attempt.
pub const UNRESOLVED_INFRASTRUCTURE: &str = "unresolved_infrastructure";

/// The delay before a root step that failed runs again (the alarm's re-arm
/// after a step error), during which a failed closure yields to other work.
pub const STEP_RETRY_MS: u64 = 30_000;

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
    /// Ticks: the epoch index at the RPC finalized head when `epoch` was
    /// read at the confirmed watermark; `epoch` is live when they are equal
    /// ([`ACTIVE_TEMPO_RULE`]). Absent (a failed or refused head read, no
    /// stored head, conflicting hashes), the head epoch is unknown: the
    /// observation is not live, and an `epoch` at a new index is not
    /// recorded, so it neither credits nor counts as uncredited.
    #[serde(default)]
    pub head_epoch: Option<u64>,
}

/// Where a tick takes the head epoch from ([`head_epoch_source`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadEpoch {
    /// The watermark is the head: the epoch already read there.
    Known(u64),
    /// The head is ahead of the watermark: one more epoch read, at this block.
    ReadAt(BlockRef),
    /// No usable head (none stored, behind the watermark, or a different
    /// block at the watermark's height): unknown, never lagging.
    Unknown,
}

/// The tick's head-epoch decision: `confirmed` is the RPC-confirmed
/// watermark whose epoch index is `confirmed_index`, `head` the stored RPC
/// finalized head.
pub fn head_epoch_source(confirmed: BlockRef, confirmed_index: u64, head: Option<BlockRef>) -> HeadEpoch {
    match head {
        Some(h) if h == confirmed => HeadEpoch::Known(confirmed_index),
        Some(h) if h.number > confirmed.number => HeadEpoch::ReadAt(h),
        _ => HeadEpoch::Unknown,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Observed {
    pub confirmed: Option<BlockRef>,
    pub epoch: Option<EpochObservation>,
    pub kick_ms: u64,
    pub kicks: u64,
    /// Active tempos under [`ACTIVE_TEMPO_RULE`]; the phase minimums count these.
    pub active_tempos: u64,
    /// Epoch-index increments that passed unobserved, during silence or in
    /// a catch-up replay.
    pub uncredited_tempos: u64,
    /// The stored epoch was observed while it was the chain head's epoch.
    pub epoch_live: bool,
}

pub fn observed(sql: &impl Sql) -> R<Observed> {
    let rows = sql.exec(
        "SELECT kick_ms, kicks, confirmed_number, confirmed_hash, epoch_index, epoch_block,
                epoch_block_hash, last_epoch_block, active_tempos, uncredited_tempos, epoch_live FROM eval_obs WHERE id = 1",
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
        epoch_live: int(r, 10)? != 0,
    })
}

/// Records a heartbeat's observations (monotonic) and advances boundary phases.
///
/// Tempos are credited by [`ACTIVE_TEMPO_RULE`]: an increase of the epoch
/// index by one credits one active tempo when the earlier epoch was observed
/// live and the new observation is live; replayed historical epochs and
/// jumps credit none. The phase minimums count active tempos, so coordinator
/// silence and the catch-up after it consume no scheduling budget, and
/// recovery resumes the same cohort at a later boundary without charging
/// missed ones. An observation at a new index whose head epoch is unknown
/// is left for the next tick with a head reading: only a proven lag (a
/// head epoch ahead of it) settles an increment as uncredited.
pub fn observe(sql: &impl Sql, kick: &Kick, now_ms: u64) -> R<Vec<String>> {
    let before = observed(sql)?;
    let before_live = before.epoch_live;
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
        // The watermark's epoch as read, recorded even when the increment
        // below is deferred, so a stored epoch held back by unknown head
        // epochs shows against it in the status.
        sql.exec(
            "UPDATE eval_obs SET watermark_epoch = ? WHERE id = 1 AND (watermark_epoch IS NULL OR watermark_epoch < ?)",
            &[bind_u64(e.index)?, bind_u64(e.index)?],
        )?;
        match before.epoch {
            Some(b) if e.block.number <= b.block.number => {}
            Some(b) if e.index < b.index => {
                sql.exec("UPDATE eval_obs SET epoch_regressions = epoch_regressions + 1 WHERE id = 1", &[])?;
            }
            _ if kick.head_epoch.is_none() && before.epoch.is_none_or(|b| e.index > b.index) => {}
            _ => {
                let live = kick.head_epoch == Some(e.index);
                let same = before.epoch.is_some_and(|b| b.index == e.index);
                let mut credited = None;
                if let Some(b) = before.epoch
                    && e.index > b.index
                {
                    let credit = u64::from(e.index == b.index + 1 && before_live && live);
                    credited = Some(credit == 1);
                    sql.exec(
                        "UPDATE eval_obs SET active_tempos = active_tempos + ?,
                            uncredited_tempos = uncredited_tempos + ? WHERE id = 1",
                        &[bind_u64(credit)?, bind_u64(e.index - b.index - credit)?],
                    )?;
                }
                // The first storing of a new epoch: the boundary journal
                // records its block, this observation's time and liveness.
                if before.epoch.is_none_or(|b| e.index > b.index) {
                    let active_after = before.active_tempos + u64::from(credited == Some(true));
                    crate::eval::boundaries::record(sql, e.index, e.last_epoch_block, now_ms, live, credited, active_after, e.block.number)?;
                }
                // An epoch observed live once stays so for later observations
                // of the same epoch (a watermark a few blocks behind the head
                // at a boundary tick); a new epoch starts from this one's.
                sql.exec(
                    "UPDATE eval_obs SET epoch_index = ?, epoch_block = ?, epoch_block_hash = ?,
                        last_epoch_block = ?, epoch_ms = ?, epoch_live = ? WHERE id = 1",
                    &[
                        bind_u64(e.index)?,
                        bind_u64(e.block.number)?,
                        s(e.block.hash.to_hex()),
                        bind_u64(e.last_epoch_block)?,
                        bind_u64(now_ms)?,
                        i(i64::from(live || (same && before_live))),
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
    /// An operator abandon is draining the cohort's lanes
    /// ([`crate::eval::decide`]): no new claim, renewal or attempt extension,
    /// and the abandon completes once every outstanding claim has passed.
    pub draining: bool,
}

/// Every column but the plan text, which is read and parsed once per plan
/// digest ([`plan_for`]): lanes reach the cohort object hundreds of times a
/// second, and a wide roster's plan is over 100 KB of JSON.
const COHORT_COLUMNS: &str = "seq, label, plan_sha256, plan_sha256, phase, queued_epoch, opened_epoch, opened_block,
    packaged_epoch, manifest_digest, task_count, fault, gen_cost_microusd, opened_active, packaged_active,
    scorer_failures, draining_ms";

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
        draining: opt_int(r, 16).is_some(),
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

/// Records that cohort `seq` passed a budget guard (`code`: [`SCORING_BUDGET`]
/// or [`GENERATION_BUDGET`]): measured spend plus the bound of the work left
/// is past the plan's budget. Spend is tracked, never capped: nothing pauses
/// or is refused, the cohort keeps running, and the status's `budget_alert`
/// (the first guard passed, kept for the cohort) raises the publisher's
/// alert. The phase log names it once.
pub fn note_budget(sql: &impl Sql, seq: i64, code: &str, now_ms: u64) -> R<()> {
    if sql
        .exec("UPDATE cohorts SET budget_alert = ? WHERE seq = ? AND budget_alert IS NULL RETURNING seq", &[s(code), i(seq)])?
        .is_empty()
    {
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
            s(format!("budget:{code} passed; tracking only, nothing paused")),
            i(seq),
        ],
    )?;
    Ok(())
}

/// Adds a content-free note to cohort `seq`'s phase log (an operator
/// record that changes nothing).
pub(crate) fn log_note(sql: &impl Sql, seq: i64, note: &str, now_ms: u64) -> R<()> {
    let o = observed(sql)?;
    sql.exec(
        "INSERT INTO phase_log (seq, phase, at_ms, confirmed_number, epoch_index, note)
         SELECT seq, phase, ?, ?, ?, ? FROM cohorts WHERE seq = ?",
        &[
            bind_u64(now_ms)?,
            o.confirmed.map_or(SqlValue::Null, |c| i(c.number as i64)),
            o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)),
            s(note),
            i(seq),
        ],
    )?;
    Ok(())
}

/// Pauses a cohort with a content-free fault code; its work and evidence stay.
pub fn fault(sql: &impl Sql, seq: i64, code: &str, now_ms: u64) -> R<()> {
    fault_with_cause(sql, seq, code, None, now_ms).map(|_| ())
}

/// [`fault`] with the fault's cause (`fault_cause`: what raised it, a short
/// content-free code such as a scorer failure class or the admission
/// subject), which decides whether the fault clears without an operator
/// ([`probe_due`], [`auto_clear`]). Returns whether this call set the fault
/// (a cohort already faulted keeps its first fault and cause).
pub fn fault_with_cause(sql: &impl Sql, seq: i64, code: &str, cause: Option<&str>, now_ms: u64) -> R<bool> {
    // Logged once: a cohort already faulted keeps its first fault and entry.
    if sql
        .exec(
            "UPDATE cohorts SET fault = ?, fault_cause = ?, fault_n = fault_n + 1 WHERE seq = ? AND fault IS NULL RETURNING seq",
            &[s(code), cause.map_or(SqlValue::Null, s), i(seq)],
        )?
        .is_empty()
    {
        return Ok(false);
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
    Ok(true)
}

/// Whether a root step's error code names a failure no wait changes: a
/// content check against a record (`... digest`), an object the step needs
/// that is not there (`... missing`), stored bytes that do not parse
/// (`... json`, `... utf-8`, `... row`, a probe record's version or size), a
/// signing key or deployment variable that is absent or does not match its
/// trust root, a sealed document the scorer refuses
/// ([`crate::eval::v3::UNSCORABLE_DOCUMENT`]), a pool with no qualifying
/// token bound ([`crate::eval::v3::POOL_TOKEN_BOUND_REFUSED`]), or another
/// fixed fact the step derives from its inputs. Every
/// other code (a bucket read or write, a storage transaction, an arena
/// retry) is transient: the step runs again after [`STEP_RETRY_MS`].
pub fn step_error_is_deterministic(code: &str) -> bool {
    const SUFFIXES: &[&str] = &[" digest", " missing", " json", " utf-8", " row", "trust root", "must differ", " version or size"];
    const CODES: &[&str] = &[
        "probe version",
        "probe task",
        "probe text",
        "shard info",
        "netuid",
        "receipt root",
        "unknown_authority",
        crate::eval::v3::UNSCORABLE_DOCUMENT,
        crate::eval::v3::POOL_TOKEN_BOUND_REFUSED,
    ];
    code.starts_with("EVAL_") || SUFFIXES.iter().any(|x| code.ends_with(x)) || CODES.contains(&code)
}

/// Records that a root step of cohort `seq` (draw, seal, package, closure)
/// failed with `code`: the code, since when and how many times in a row, and
/// when it may run again ([`STEP_RETRY_MS`]; a closure that waits yields to
/// every other step, [`crate::eval::v3::closure_due`]). The stored epoch of
/// the first failure (`step_error_since_epoch`) is kept until a step
/// succeeds ([`note_step`]), whatever code later failures carry, so a step
/// failing on alternating transient codes is still judged by the chain
/// progress since it first failed; a row without one (written before v18)
/// takes the current stored epoch. A deterministic code
/// ([`step_error_is_deterministic`]) also pauses the cohort ([`refuse_step`]).
/// Returns whether the cohort is paused by a fault.
pub fn note_step_error(sql: &impl Sql, seq: i64, code: &str, now_ms: u64) -> R<bool> {
    let epoch = observed(sql)?.epoch.map_or(SqlValue::Null, |e| i(e.index as i64));
    sql.exec(
        "UPDATE cohorts SET step_errors = CASE WHEN step_error IS ? THEN step_errors + 1 ELSE 1 END,
            step_error_since_ms = CASE WHEN step_error IS ? THEN step_error_since_ms ELSE ? END,
            step_error_since_epoch = COALESCE(step_error_since_epoch, ?),
            step_error = ?, step_retry_ms = ? WHERE seq = ?",
        &[
            s(code),
            s(code),
            bind_u64(now_ms)?,
            epoch,
            s(code),
            bind_u64(now_ms.saturating_add(STEP_RETRY_MS))?,
            i(seq),
        ],
    )?;
    if step_error_is_deterministic(code) {
        refuse_step(sql, seq, code, now_ms)?;
    }
    Ok(!sql.exec("SELECT 1 FROM cohorts WHERE seq = ? AND fault IS NOT NULL", &[i(seq)])?.is_empty())
}

/// Pauses cohort `seq` as [`STEP_REFUSED`] (resumable, alerted once as a
/// cohort fault) because its root step failed with `code` in a way no wait
/// changes; the phase log names the code. A cohort already paused keeps its
/// first fault.
pub fn refuse_step(sql: &impl Sql, seq: i64, code: &str, now_ms: u64) -> R<()> {
    if sql.exec("SELECT 1 FROM cohorts WHERE seq = ? AND fault IS NULL", &[i(seq)])?.is_empty() {
        return Ok(());
    }
    fault(sql, seq, STEP_REFUSED, now_ms)?;
    let o = observed(sql)?;
    sql.exec(
        "INSERT INTO phase_log (seq, phase, at_ms, confirmed_number, epoch_index, note)
         SELECT seq, phase, ?, ?, ?, ? FROM cohorts WHERE seq = ?",
        &[
            bind_u64(now_ms)?,
            o.confirmed.map_or(SqlValue::Null, |c| i(c.number as i64)),
            o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)),
            s(format!("step_refused:{code}")),
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
/// it stopped, with fresh outage allowances. A budget fault resumes with a
/// fresh allowance of the plan's budget, counted from the spend measured now:
/// the guard stops a runaway again, never the cohort for good. A failed root
/// step runs again at once. A terminal fault (attempts, humans or cases
/// exhausted under the frozen plan) is refused: it would fault again at once,
/// so the cohort can only be abandoned.
pub fn clear_fault(sql: &impl Sql, label: &str, now_ms: u64) -> R<Cleared> {
    let rows = sql.exec("SELECT seq, fault FROM cohorts WHERE label = ? AND fault IS NOT NULL", &[s(label)])?;
    let Some(r) = rows.first() else {
        return Ok(Cleared { cleared: false, refused: Some("no_fault") });
    };
    let seq = int(r, 0)?;
    let code = text(r, 1)?.to_owned();
    if !RESUMABLE_FAULTS.contains(&code.as_str()) {
        return Ok(Cleared { cleared: false, refused: Some("abandon_required") });
    }
    match code.as_str() {
        UNRESOLVED_INFRASTRUCTURE => {
            // Back to scoring: each faulted job's next claim extends its
            // exhausted chunks under the deployed receipt bound, or faults
            // the job again if none fits. No attempt or receipt is reset.
            // A job that reached the receipt bound (`receipt_bound_reached`,
            // shown in the status) faults again unless the deployed bound
            // grew: the mark is set again in that case.
            sql.exec(
                "UPDATE jobs SET status = 'running', fault = NULL, receipt_bound_reached = 0
                 WHERE seq = ? AND status = 'faulted' AND fault = ?",
                &[i(seq), s(UNRESOLVED_INFRASTRUCTURE)],
            )?;
        }
        SCORING_BUDGET => {
            let c = cohort(sql, seq)?;
            let measured = crate::eval::lanes::spend(sql, &c, &["score"], now_ms, false)?.measured_nanousd;
            sql.exec(
                "UPDATE cohorts SET score_base_nanousd = ? WHERE seq = ?",
                &[i(i64::try_from(measured).unwrap_or(i64::MAX)), i(seq)],
            )?;
        }
        GENERATION_BUDGET => {
            sql.exec("UPDATE cohorts SET gen_base_microusd = gen_cost_microusd WHERE seq = ?", &[i(seq)])?;
        }
        _ => {}
    }
    resume(sql, seq, now_ms, "fault_cleared", false)?;
    Ok(Cleared { cleared: true, refused: None })
}

/// Clears the outage fault `code` of cohort `seq` without an operator, once
/// the work it paused succeeded again: an automatic probe ([`probe_done`]),
/// or the success of the admission subject that raised it
/// ([`EVIDENCE_CLEARED_FAULTS`]). `code` must still be the cohort's fault, so
/// a later or different fault is never cleared this way, and no terminal
/// fault ever is. In a work phase only a cause the probe exercises clears
/// ([`probed_cause`]); a fault the probe cannot see (a store failure, a
/// refused commit, a definitive refusal) stays for an operator. A cohort
/// that pauses again with no progress since its last automatic clear (a
/// committed chunk or a generation answer resets `auto_cleared`) is still
/// probed and cleared, never held for good: each such clear doubles the
/// wait before its next probe ([`probe_interval_ms`]), so a partly recovered
/// platform cannot make a tight fault/clear loop, and the same cohort
/// resumes by itself once it fully recovers. The phase log records
/// `fault_auto_cleared:<code>` and how (`how`, a short content-free code).
/// Returns whether it cleared.
pub fn auto_clear(sql: &impl Sql, seq: i64, code: &str, how: &str, now_ms: u64) -> R<bool> {
    if !(PROBED_FAULTS.contains(&code) || EVIDENCE_CLEARED_FAULTS.contains(&code)) {
        return Ok(false);
    }
    let rows = sql.exec("SELECT phase, fault_cause FROM cohorts WHERE seq = ? AND fault = ?", &[i(seq), s(code)])?;
    let Some(r) = rows.first() else { return Ok(false) };
    let work_phase = WORK_PHASES.contains(&text(r, 0)?);
    if work_phase {
        if !PROBED_FAULTS.contains(&code) {
            return Ok(false);
        }
        if code == SCORER_UNAVAILABLE && !probed_cause(opt_text(r, 1).as_deref()) {
            return Ok(false);
        }
    }
    resume(sql, seq, now_ms, &format!("fault_auto_cleared:{code} ({how})"), work_phase)?;
    Ok(true)
}

/// The phases whose outage faults the automatic probe handles.
pub const WORK_PHASES: &[&str] = &["generating", "sealing", "scoring", "packaging"];

/// The cohort made progress (a committed chunk, a generation answer): the
/// probe interval's back-off starts again ([`probe_interval_ms`]), and a
/// later pause is probed within one [`PROBE_EVERY_MS`] of now.
pub fn note_progress(sql: &impl Sql, seq: i64, now_ms: u64) -> R<()> {
    sql.exec(
        "UPDATE cohorts SET auto_cleared = 0, probe_after_ms = MIN(probe_after_ms, ?) WHERE seq = ? AND auto_cleared != 0",
        &[bind_u64(now_ms.saturating_add(PROBE_EVERY_MS))?, i(seq)],
    )?;
    Ok(())
}

/// Resumes paused cohort `seq`: its fault, its cause and outage counters
/// cleared, its claims released so they are taken again with fresh outage
/// allowances. No attempt number, answer or receipt is reset, and nothing is
/// skipped. An automatic clear (`automatic`) counts one more clear without
/// progress (`auto_cleared`, reset by [`note_progress`]) and sets the next
/// probe's time from that count ([`probe_interval_ms`]), so a cohort that
/// pauses again waits at least a full, backed-off probe interval; an
/// operator clear probes at once and resets the count.
fn resume(sql: &impl Sql, seq: i64, now_ms: u64, note: &str, automatic: bool) -> R<()> {
    sql.exec(
        "UPDATE cohorts SET fault = NULL, fault_cause = NULL, scorer_cause = NULL, scorer_failures = 0,
            step_retry_ms = 0, probe_failures = 0,
            auto_cleared = CASE WHEN ? THEN auto_cleared + 1 ELSE 0 END,
            probe_after_ms = CASE WHEN ? THEN probe_after_ms ELSE 0 END WHERE seq = ?",
        &[i(i64::from(automatic)), i(i64::from(automatic)), i(seq)],
    )?;
    if automatic {
        let n = int(&sql.exec("SELECT auto_cleared FROM cohorts WHERE seq = ?", &[i(seq)])?[0], 0)?;
        sql.exec(
            "UPDATE cohorts SET probe_after_ms = MAX(probe_after_ms, ?) WHERE seq = ?",
            &[bind_u64(now_ms.saturating_add(probe_interval_ms(n)))?, i(seq)],
        )?;
    }
    sql.exec(
        "UPDATE docs SET claim_until_ms = 0, infra_failures = 0, not_before_ms = 0, throttle_since_ms = 0
         WHERE seq = ? AND status = 'pending'",
        &[i(seq)],
    )?;
    sql.exec("UPDATE chunks SET claim_until_ms = 0 WHERE seq = ? AND status = 'pending'", &[i(seq)])?;
    let c = cohort(sql, seq)?;
    set_phase(sql, seq, &c.phase, now_ms, note)
}

/// Operator action: end a faulted cohort without a target, so later cohorts can
/// open. `reason` is a short content-free code. The phase log, the used set
/// (drawn humans are never reused), the sealed batch in the eval bucket and
/// the cohort row with its receipt root and count stay; its detailed receipts
/// follow the N+2 rule like any other cohort's (`eval::retention`). Private
/// texts leave Durable Object storage as at a close. The calculation target
/// stays the previous cohort's. The main instance then writes the signed,
/// content-free abandonment record ([`Work::AbandonMarker`],
/// [`abandonment_record`]) beside the cohort's other signed records, so the
/// automatic target handoff can move past it.
pub fn abandon(sql: &impl Sql, label: &str, reason: &str, now_ms: u64) -> R<bool> {
    let code_ok = !reason.is_empty()
        && reason.len() <= slop_ninja_protocol::submission::ABANDONMENT_CODE_MAX
        && reason.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.:".contains(&b));
    if !code_ok {
        return Err(EvalError::Plan("abandon reason (a-z 0-9 _ - . :, at most ABANDONMENT_CODE_MAX bytes)"));
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
    if !abandon_reason_listed(reason, &fault_code) {
        return Err(EvalError::Plan("abandon reason (one of ABANDON_REASONS, or the cohort's fault code)"));
    }
    abandon_now(sql, seq, reason, Some(&fault_code), now_ms)?;
    Ok(true)
}

/// Ends cohort `seq` as abandoned, now: the shared effect of an abandonment
/// of a faulted cohort ([`abandon`]) and of an operator's
/// ([`crate::eval::decide::abandon`]). `fault_code` is the fault it was
/// paused by, `None` for a cohort with none.
pub(crate) fn abandon_now(sql: &impl Sql, seq: i64, reason: &str, fault_code: Option<&str>, now_ms: u64) -> R<()> {
    let o = observed(sql)?;
    sql.exec(
        "UPDATE cohorts SET abandoned_reason = ?, abandoned_fault = ?, fault = NULL, closed_epoch = ?, draining_ms = NULL
         WHERE seq = ?",
        &[s(reason), fault_code.map_or(SqlValue::Null, s), o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)), i(seq)],
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
    let note = match fault_code {
        Some(f) => format!("abandoned:{reason} after fault:{f}; no target"),
        None => format!("abandoned:{reason} by operator decision; no target"),
    };
    set_phase(sql, seq, "abandoned", now_ms, &note)
}

/// The reason codes an abandonment may give, besides the cohort's own fault
/// code (which the engine wrote). The signed abandonment record carries the
/// reason to readers outside the eval store, so it is a public-safe code
/// from this fixed vocabulary or an engine fault code, never a figure, an
/// account or a provider name; the legacy budget fault codes are published
/// as [`RESOURCE_GUARD`] ([`published_code`]).
pub const ABANDON_REASONS: &[&str] = &[
    // The fault is terminal under the frozen plan (no attempt, human or case left).
    "terminal_fault",
    // The frozen plan cannot be completed as written.
    "plan_defect",
    // The cohort's reference cannot be scored by any deployable scorer.
    "reference_unusable",
    // The pool cannot supply the plan's documents.
    "pool_shortfall",
    // The chain record restarts under a new authority.
    "authority_restart",
    // A rehearsal or fault drill.
    "drill",
];

/// Whether `reason` may be given for abandoning a cohort paused by
/// `fault_code`: a listed reason ([`ABANDON_REASONS`]) or the fault code.
pub fn abandon_reason_listed(reason: &str, fault_code: &str) -> bool {
    ABANDON_REASONS.contains(&reason) || reason == fault_code
}

/// The signed abandonment record's key under the cohort's receipts prefix:
/// the name the publisher reads, from the shared receipts-bucket contract.
pub use slop_ninja_admission_wire::receipts::ABANDONMENT_RECORD as ABANDONMENT_KEY;

/// The neutral code the signed abandonment record gives, as its fault and
/// reason, for the legacy budget fault codes ([`SCORING_BUDGET`],
/// [`GENERATION_BUDGET`]): stores that paused under the earlier budget
/// guard keep their codes, but the published record names no budget.
pub const RESOURCE_GUARD: &str = "resource_guard";

/// A fault or reason code as the signed abandonment record publishes it:
/// the legacy budget codes as [`RESOURCE_GUARD`], every other code as it is.
pub fn published_code(code: &str) -> &str {
    match code {
        SCORING_BUDGET | GENERATION_BUDGET => RESOURCE_GUARD,
        c => c,
    }
}

/// An abandoned cohort of this instance whose signed abandonment record is
/// not written yet (main instance only: a load cohort is never in the chain
/// record), and whose retry time has passed by `due_by` (`None`: any). The
/// record is a later consistency aid for the target handoff, so it never
/// holds other work: it is chosen only where receipt deletion is, once
/// admission and the active cohort have nothing to send ([`next_work`]), and
/// a write that fails waits out [`STEP_RETRY_MS`] ([`abandon_marker_failed`]).
pub fn abandon_marker_due(sql: &impl Sql, cfg: &EvalConfig, due_by: Option<u64>) -> R<Option<Work>> {
    if cfg.is_load() {
        return Ok(None);
    }
    let rows = sql.exec(
        "SELECT seq FROM cohorts WHERE (phase = 'abandoned'
                OR (phase = 'closed' AND closure_digest = 'probe-build' AND json_extract(plan, '$.reward_v1.probe_cut') = 1))
            AND abandon_marker_ms IS NULL AND abandon_marker_retry_ms <= ?
         ORDER BY seq LIMIT 1",
        &[match due_by {
            Some(n) => bind_u64(n)?,
            None => i(i64::MAX),
        }],
    )?;
    Ok(rows.first().map(|r| int(r, 0)).transpose()?.map(|seq| Work::AbandonMarker { seq }))
}

/// The abandonment record of `seq` could not be written (`code`, a short
/// content-free code): it is retried after [`STEP_RETRY_MS`], and every
/// other step goes on meanwhile.
pub fn abandon_marker_failed(sql: &impl Sql, seq: i64, code: &str, now_ms: u64) -> R<()> {
    sql.exec(
        "UPDATE cohorts SET abandon_marker_retry_ms = ?, abandon_marker_error = ? WHERE seq = ? AND abandon_marker_ms IS NULL",
        &[bind_u64(now_ms.saturating_add(STEP_RETRY_MS))?, s(code), i(seq)],
    )?;
    Ok(())
}

/// The abandonment record of abandoned cohort `seq` and its key: content-free
/// (the chain, cohort number and label, its manifest digest when it was
/// sealed, the fault it was paused by, the operator's reason code, and the
/// epoch observed when it was abandoned). A cohort abandoned before reasons
/// were recorded carries `unrecorded`. Legacy budget codes are published as
/// [`RESOURCE_GUARD`] ([`published_code`]).
pub fn abandonment_record(sql: &impl Sql, cfg: &EvalConfig, seq: i64) -> R<(String, AbandonmentRecord)> {
    let c = cohort(sql, seq)?;
    if c.phase == "closed" && c.plan.is_probe_cut() {
        // D2: a probe cut consumed its number and closed with no target; its
        // record lets the coordinator's handoff pass the number.
        let r = &sql.exec("SELECT closed_epoch FROM cohorts WHERE seq = ?", &[i(seq)])?[0];
        let record = slop_ninja_protocol::decisions::ProbeCutRecord {
            chain: cfg.chain.clone(),
            cohort: c.plan.cohort,
            cohort_label: c.label.clone(),
            probe_version: c.plan.reward_v1.as_ref().and_then(|v| v.probe_version).unwrap_or(0),
            cut_epoch: opt_int(r, 0).map_or(0, |e| e as u64),
        };
        let key = format!("{}/{}", receipt_prefix(&c.label), slop_ninja_admission_wire::receipts::PROBE_CUT_RECORD);
        return Ok((key, AbandonmentRecord::ProbeCut(record)));
    }
    if c.phase != "abandoned" {
        return Err(EvalError::State("abandonment record of a cohort not abandoned"));
    }
    let r = &sql.exec(
        "SELECT abandoned_reason, abandoned_fault, closed_epoch, abandon_operator, abandon_phase, abandon_decision,
            abandon_statement, package_digest, target_digest FROM cohorts WHERE seq = ?",
        &[i(seq)],
    )?[0];
    let fault = opt_text(r, 1);
    let key = format!("{}/{ABANDONMENT_KEY}", receipt_prefix(&c.label));
    let digest = |idx: usize| opt_text(r, idx).map(|d| parse_digest(&d).ok_or(EvalError::State("abandon digest"))).transpose();
    if opt_int(r, 3).is_some_and(|o| o != 0) {
        // By operator decision (`decide::abandon`): version 2, fault `none`
        // when the cohort had none.
        let result = crate::eval::v3::state(sql, seq)?.result.map(|h| h.0);
        let record = slop_ninja_protocol::decisions::CohortAbandonmentV2 {
            chain: cfg.chain.clone(),
            cohort: c.plan.cohort,
            cohort_label: c.label.clone(),
            cohort_manifest: digest_opt(c.manifest_digest.as_deref())?,
            fault: fault.as_deref().map_or("none", published_code).to_owned(),
            reason: opt_text(r, 0).as_deref().map_or("operator_decision", published_code).to_owned(),
            abandoned_epoch: opt_int(r, 2).map(|e| e as u64),
            phase_at_abandon: opt_text(r, 4).unwrap_or_else(|| "unrecorded".into()),
            score_package: digest(7)?,
            result,
            target: digest(8)?,
            operator: true,
            decision: digest(5)?,
            statement: digest(6)?,
        };
        return Ok((key, AbandonmentRecord::V2(record)));
    }
    let record = slop_ninja_protocol::submission::CohortAbandonment {
        chain: cfg.chain.clone(),
        cohort: c.plan.cohort,
        cohort_label: c.label.clone(),
        cohort_manifest: c.manifest_digest.as_deref().map(|d| parse_digest(d).ok_or(EvalError::State("manifest digest"))).transpose()?,
        fault: fault.as_deref().map_or("unrecorded", published_code).to_owned(),
        // A reason stored before the vocabulary (an older free code) is
        // published as `unlisted`; the store keeps the original.
        reason: match opt_text(r, 0) {
            Some(x) if abandon_reason_listed(&x, fault.as_deref().unwrap_or("")) => published_code(&x).to_owned(),
            Some(_) => "unlisted".into(),
            None => "unrecorded".into(),
        },
        abandoned_epoch: opt_int(r, 2).map(|e| e as u64),
    };
    Ok((key, AbandonmentRecord::V1(record)))
}

fn digest_opt(d: Option<&str>) -> R<Option<Digest>> {
    d.map(|d| parse_digest(d).ok_or(EvalError::State("manifest digest"))).transpose()
}

/// The abandonment record a cohort's end writes: version 1 for an
/// abandonment of a faulted cohort, version 2 for one by operator decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbandonmentRecord {
    V1(slop_ninja_protocol::submission::CohortAbandonment),
    V2(slop_ninja_protocol::decisions::CohortAbandonmentV2),
    /// A probe cut's end (D2), written by the same step.
    ProbeCut(slop_ninja_protocol::decisions::ProbeCutRecord),
}

/// The abandonment record of `seq` is durable in the receipts bucket.
pub fn abandon_marker_written(sql: &impl Sql, seq: i64, now_ms: u64) -> R<()> {
    sql.exec(
        "UPDATE cohorts SET abandon_marker_ms = ? WHERE seq = ? AND phase IN ('abandoned','closed') AND abandon_marker_ms IS NULL",
        &[bind_u64(now_ms)?, i(seq)],
    )?;
    Ok(())
}

// --------------------------------------------------------- outage probes

/// One bounded probe of the platform an outage fault paused ([`probe_due`]).
/// A probe runs on a fixed control text (never cohort text) and touches no
/// job. Its time and spend are recorded with the cohort and charged at its
/// settle ([`probe_begin`], [`probe_done`]), and its runs, failures, time
/// and spend are in the cohort's status. It writes no R2 receipt and is not
/// bound into the signed score package: an open exception to the rule that
/// every inference attempt has one, recorded for a decision (the existing
/// `/eval/probe`, `/eval/scorer-probe` and admission runs are the same).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Probe {
    /// One generation on a fixed synthetic prompt (never cohort text) by
    /// `item.generator`, a generator of the paused items, within the lane's
    /// call timeout.
    Generation { item: GenItem, timeout_ms: u64 },
    /// The control document on a freshly started scorer instance of the
    /// artifact whose slots failed (the cohort's reference, or its previous
    /// reference), under the cohort's label, spec and lane limits (the
    /// admission route's single-use run): `body` is the scorer lane's
    /// `/admit` request, or its `/run` request for a plan that names no
    /// artifact spec (`admit` false). `lane` is one stable name per
    /// instance and authority ([`probe_lane_name`]). It never touches an
    /// entrant's job.
    Scorer { lane: String, body: Value, admit: bool },
}

/// The scorer lane object a probe runs on: one per instance and authority
/// (`EvalConfig::lane_scope`), stable across cohorts, so probes add no lane
/// objects or container identities. A probe destroys any running instance
/// before it starts and stops its own when done.
pub fn probe_lane_name(cfg: &EvalConfig) -> String {
    format!("a-scorer/{}/{}/fault-probe", crate::config::NETWORK, cfg.lane_scope())
}

/// A work-phase cohort paused by an outage fault ([`PROBED_FAULTS`]) whose
/// next probe is due: one at a time (never while an earlier probe may still
/// run, `probe_until_ms`), at most one per [`probe_interval_ms`] after the
/// previous one ended, and only while the coordinator is alive (a fresh
/// heartbeat), so silence costs nothing and never counts against anything.
/// There is no count or time limit: a cohort is never skipped or abandoned
/// because its probes keep failing. Only a cause the probe exercises is
/// probed (a `scorer_unavailable` with a `fault_cause` of [`probed_cause`];
/// a probe that met a definitive refusal records `definitive:probe`, which
/// is not): the others wait for an operator clear.
pub fn probe_due(sql: &impl Sql, cfg: &EvalConfig, now_ms: u64) -> R<Option<Work>> {
    if !crate::eval::lanes::live(sql, now_ms)? {
        return Ok(None);
    }
    let rows = sql.exec(
        "SELECT seq, fault, fault_cause FROM cohorts
          WHERE (fault = ? OR (fault = ? AND fault_cause IN ('slot', 'slot:previous_reference')))
            AND phase IN ('generating','sealing','scoring','packaging')
            AND probe_after_ms <= ? AND (probe_started_ms IS NULL OR COALESCE(probe_until_ms, 0) <= ?)
          ORDER BY seq LIMIT 1",
        &[s(GENERATION_UNAVAILABLE), s(SCORER_UNAVAILABLE), bind_u64(now_ms)?, bind_u64(now_ms)?],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    let (seq, code, cause) = (int(r, 0)?, text(r, 1)?.to_owned(), opt_text(r, 2));
    let c = cohort(sql, seq)?;
    let f = c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    let probe = if code == GENERATION_UNAVAILABLE {
        // The generator of the paused item with the most outages in a row.
        let g = sql.exec(
            "SELECT generator_next FROM docs WHERE seq = ? AND status = 'pending' AND generator_next IS NOT NULL
               AND generator_next LIKE '@cf/%'
             ORDER BY infra_failures DESC, idx LIMIT 1",
            &[i(seq)],
        )?;
        let generator = match g.first().and_then(|r| opt_text(r, 0)) {
            Some(g) => g,
            None => c.plan.generators.first().cloned().ok_or(EvalError::Plan("generators"))?,
        };
        let item = GenItem {
            idx: -1,
            kind: "probe".into(),
            generator,
            prompt: textgen::scratch_prompt("fineweb", "a short walk along a canal in early spring", 80),
            attempt: 0,
            claim: 0,
            call: None,
        };
        Probe::Generation { item, timeout_ms: crate::eval::lanes::gen_call_timeout_ms(f.generation.lease_ms) }
    } else {
        // The artifact whose slots failed: the previous reference's own
        // load is probed with its artifact, every other cause with the
        // reference's.
        let a = match (cause.as_deref(), &c.plan.previous_reference) {
            (Some("slot:previous_reference"), Some(p)) => p,
            _ => &c.plan.reference,
        };
        let lane = probe_lane_name(cfg);
        let control = json!([{"id": "0", "text": crate::eval::lane_logic::CONTROL_TEXT}]);
        match (c.plan.load_spec_sha256(), crate::eval::admission::AdmitLimits::of(&c.plan)) {
            (Some(spec), Some(limits)) => Probe::Scorer {
                lane,
                body: json!({"package_sha256": a.package_sha256, "manifest_key": a.manifest_key, "label": c.label,
                    "spec_sha256": spec, "limits": limits, "docs": control}),
                admit: true,
            },
            _ => Probe::Scorer {
                lane,
                body: json!({"package_sha256": a.package_sha256, "manifest_key": a.manifest_key, "docs": control}),
                admit: false,
            },
        }
    };
    Ok(Some(Work::Probe { seq, code, probe }))
}

/// The most probe `p` of cohort `seq` can spend (nano-dollars), charged if
/// its outcome is never recorded (a step cut off mid-call): a generation's
/// prompt bytes (each at most one token) and its completion allowance at
/// the generator's price; a scorer probe's instance and lane object held
/// for every allowance the run is given (two loads, the inactivity bound
/// and the control document's limit; a plan without limits, its lease),
/// with every vCPU busy.
pub fn probe_bound_nanousd(c: &Cohort, p: &Probe) -> R<u64> {
    let f = c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    Ok(match p {
        Probe::Generation { item, .. } => {
            let prompt = (item.prompt.system.len() + item.prompt.user.len()) as u64;
            textgen::cost_microusd(&item.generator, prompt, u64::from(item.prompt.max_tokens)).saturating_mul(1_000)
        }
        Probe::Scorer { .. } => u64::try_from(cost::unreported_bound_nanousd(probe_run_ms(c, p)?, &f.cost)).unwrap_or(u64::MAX),
    })
}

/// The longest probe `p` of cohort `seq`'s run is given: a generation's
/// call timeout, or every allowance the scorer run is given (two loads, the
/// inactivity bound and the control document's limit; a plan without
/// limits, its score lease), the same allowances [`probe_bound_nanousd`]
/// charges for.
pub fn probe_run_ms(c: &Cohort, p: &Probe) -> R<u64> {
    let f = c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    Ok(match p {
        Probe::Generation { timeout_ms, .. } => *timeout_ms,
        Probe::Scorer { .. } => match crate::eval::admission::AdmitLimits::of(&c.plan) {
            Some(l) => l.load_wait_ms.saturating_mul(2).saturating_add(l.inactivity_ms).saturating_add(l.doc_timeout_ms),
            None => f.score.lease_ms,
        },
    })
}

/// A probe that started ([`probe_begin`]): when (its id), which fault it
/// was started for (the cohort's `fault_n` then), and the deadline its
/// call is bounded by (`until_ms`: [`probe_run_ms`] plus one Durable Object
/// call's allowance, `liveness::TIMEOUT_MS`). [`probe_done`] applies an
/// outcome only to the probe and the fault it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeTicket {
    pub started_ms: u64,
    pub fault_n: i64,
    pub until_ms: u64,
}

/// Records, before probe `p` of cohort `seq` runs, that it started: its
/// start, the fault it is for and its deadline ([`ProbeTicket`]), so no
/// second probe starts while it may still run ([`probe_due`]) and a probe
/// cut off mid-call is not run again before its deadline and a probe
/// interval have passed; and its bound ([`probe_bound_nanousd`]) as pending
/// spend, charged with the cohort unless [`probe_done`] replaces it with
/// the measured spend. A probe still pending from an earlier cut-off step
/// is charged its bound now.
pub fn probe_begin(sql: &impl Sql, seq: i64, p: &Probe, now_ms: u64) -> R<ProbeTicket> {
    let c = cohort(sql, seq)?;
    let bound = probe_bound_nanousd(&c, p)?;
    let until_ms = now_ms.saturating_add(probe_run_ms(&c, p)?).saturating_add(crate::liveness::TIMEOUT_MS);
    let r = &sql.exec("SELECT fault_n, auto_cleared FROM cohorts WHERE seq = ?", &[i(seq)])?[0];
    let (fault_n, cleared) = (int(r, 0)?, int(r, 1)?);
    sql.exec(
        "UPDATE cohorts SET probe_nanousd = probe_nanousd + probe_pending_nanousd,
            probes = probes + CASE WHEN probe_started_ms IS NULL THEN 0 ELSE 1 END,
            probe_last = CASE WHEN probe_started_ms IS NULL THEN probe_last ELSE 'cut_off' END,
            probe_pending_nanousd = ?, probe_started_ms = ?, probe_until_ms = ?, probe_fault_n = ?,
            probe_after_ms = ? WHERE seq = ?",
        &[
            bind_u64(bound)?,
            bind_u64(now_ms)?,
            bind_u64(until_ms)?,
            i(fault_n),
            bind_u64(until_ms.saturating_add(probe_interval_ms(cleared)))?,
            i(seq),
        ],
    )?;
    Ok(ProbeTicket { started_ms: now_ms, fault_n, until_ms })
}

/// Whether a generation probe's answer shows the provider serving again: a
/// model answer (accepted or rejected for its content), or a model the
/// provider no longer serves (the work path then uses an attempt and rotates
/// the item's generator, `apply_generation`), never an outage, a throttle or
/// a budget refusal.
pub fn probe_answered(g: &GenOutcome) -> bool {
    !is_outage(g) && !matches!(g.output, Err(Reject::Transport | Reject::Throttled | Reject::Budget))
}

/// How a probe ended ([`probe_done`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeOutcome {
    pub ok: bool,
    /// The scorer refused the artifact definitively (a deterministic check
    /// that no retry passes): no further probe runs until an operator clear.
    pub definitive: bool,
    pub ms: u64,
    pub nanousd: u64,
    /// `ok`, or a short content-free code for the failure.
    pub note: String,
}

/// Records probe `t` of cohort `seq`'s outage fault `code`: its measured
/// time and spend (`nanousd`, charged with the cohort's spend at its
/// settle, in place of the bound [`probe_begin`] held), and its outcome
/// (`ok`, or a short content-free code). The next probe waits a probe
/// interval from now ([`probe_interval_ms`]). A success resumes the same
/// cohort with nothing reset or skipped ([`auto_clear`]); a failure keeps
/// it paused until the next probe; a definitive refusal records
/// `definitive:probe` as the fault's cause, so no further probe runs (a
/// deterministic check fails the same way on every fresh instance) until an
/// operator clears it after deploying a scorer that admits the artifact.
/// The outcome applies only to the fault the probe was started for: when
/// the cohort's fault is another one now (an operator cleared it and the
/// cohort paused again), the probe only records its spend. A probe whose
/// start is no longer the recorded one (a later probe began after this one
/// was taken for cut off) leaves that probe's pending record alone.
/// Returns whether the cohort resumed.
pub fn probe_done(sql: &impl Sql, seq: i64, code: &str, t: &ProbeTicket, o: &ProbeOutcome, now_ms: u64) -> R<bool> {
    let last = if o.ok { "ok".to_owned() } else { crate::eval::limiter::sanitize(&o.note) };
    let r = &sql.exec(
        "SELECT probe_started_ms = ?, fault = ? AND fault_n = ?, auto_cleared FROM cohorts WHERE seq = ?",
        &[bind_u64(t.started_ms)?, s(code), i(t.fault_n), i(seq)],
    )?[0];
    let (current, same_fault, cleared) = (opt_int(r, 0) == Some(1), opt_int(r, 1) == Some(1), int(r, 2)?);
    if current {
        sql.exec(
            "UPDATE cohorts SET probes = probes + 1, probe_ms = probe_ms + ?, probe_nanousd = probe_nanousd + ?,
                probe_pending_nanousd = 0, probe_started_ms = NULL, probe_until_ms = NULL, probe_fault_n = NULL,
                probe_last = ?, probe_after_ms = ?,
                probe_failures = CASE WHEN ? THEN 0 ELSE probe_failures + 1 END
             WHERE seq = ?",
            &[
                bind_u64(o.ms)?,
                bind_u64(o.nanousd)?,
                s(last),
                bind_u64(now_ms.saturating_add(probe_interval_ms(cleared)))?,
                i(i64::from(o.ok)),
                i(seq),
            ],
        )?;
    } else {
        // The probe was charged its bound as cut off when the later one
        // began; its measured spend replaces nothing.
        sql.exec(
            "UPDATE cohorts SET probe_ms = probe_ms + ? WHERE seq = ?",
            &[bind_u64(o.ms)?, i(seq)],
        )?;
    }
    if !same_fault {
        return Ok(false);
    }
    if o.definitive && !o.ok {
        sql.exec(
            "UPDATE cohorts SET fault_cause = 'definitive:probe' WHERE seq = ? AND fault = ? AND fault_n = ?",
            &[i(seq), s(code), i(t.fault_n)],
        )?;
        return Ok(false);
    }
    if !o.ok {
        return Ok(false);
    }
    auto_clear(sql, seq, code, "probe", now_ms)
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
    // Further parent pools are exposed stock only, and the exposed-pool
    // reuse rule is testnet's (`plan::ExposedReuse`).
    if let Some(v) = &plan.reward_v1 {
        if v.parent_pools.iter().any(|p| !cfg.exposed_pools.contains(&p.pool_id)) {
            return Err(EvalError::Plan(PARENT_POOL_REFUSED));
        }
        if v.reuse.exposed.is_some() && cfg.chain.genesis != slop_ninja_protocol::TESTNET_GENESIS {
            return Err(EvalError::Plan(EXPOSED_REUSE_TESTNET_ONLY));
        }
    }
    let f = plan.fanout().map_err(|e| EvalError::Plan(e.0))?;
    // The generation pause bound is part of what the plan's seal binds, so a
    // new cohort plan declares it; only plans stored before it was required
    // fall back to the bound those builds compiled in
    // (`plan::GEN_FAILURES_BEFORE_PAUSE_COMPILED`). A load plan generates
    // nothing.
    if !plan.is_load() && f.generation.failures_before_pause.is_none() {
        return Err(EvalError::Plan("fanout.generation.failures_before_pause (required in a new plan)"));
    }
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
    if plan.is_probe_cut() {
        crate::eval::calendar::check_probe_cut(sql, &plan, &existing)?;
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
    // A same-authority probe cut (`calendar::check_probe_cut`) has no roster
    // and pays nothing: it starts at once, whatever the cohorts before it
    // are doing. Its work yields to every other cohort's (`active`): it runs
    // only when no other cohort's work is runnable, earlier or later, so it
    // never takes a queued or running cohort's turn or holds one back.
    for c in cohorts(sql)? {
        if c.phase == "queued" && c.plan.is_probe_cut() {
            let active_now = observed(sql)?.active_tempos;
            sql.exec(
                "UPDATE cohorts SET queued_epoch = COALESCE(queued_epoch, ?), opened_epoch = ?, opened_block = ?,
                    opened_ms = ?, opened_active = ?, admission_closed_epoch = ? WHERE seq = ?",
                &[bind_u64(e.index)?, bind_u64(e.index)?, bind_u64(e.last_epoch_block)?, bind_u64(now_ms)?,
                    bind_u64(active_now)?, bind_u64(e.index)?, i(c.seq)],
            )?;
            set_phase(sql, c.seq, "generating", now_ms, "probe cut: drawn at once beside the open cohorts; no roster, no target")?;
            moved.push(format!("{}:generating", c.label));
        }
    }
    for c in cohorts(sql)? {
        let p = &c.plan;
        if p.is_load() || p.is_probe_cut() {
            // Load cohorts and probe cuts never wait on a boundary.
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

/// The cohort whose work is runnable now, if any: the first in seq order,
/// except that a same-authority probe cut (`calendar::check_probe_cut`)
/// yields to every other cohort with runnable work, earlier or later, so
/// it never holds a cohort back. A yielding cut keeps its state and
/// resumes when no other cohort's work is runnable.
pub fn active(sql: &impl Sql) -> R<Option<Cohort>> {
    let mut cut = None;
    for c in cohorts(sql)? {
        if !matches!(c.phase.as_str(), "generating" | "sealing" | "scoring" | "packaging") || c.fault.is_some() || c.draining {
            continue;
        }
        if !c.plan.is_probe_cut() {
            return Ok(Some(c));
        }
        if cut.is_none() {
            cut = Some(c);
        }
    }
    Ok(cut)
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
    /// Generation v2: the vendor call of the item's current cell (built by
    /// the claim from the cohort's bound manifest); `prompt` is then unused.
    /// Absent for every v1 item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<Box<slop_ninja_gen_core::outcome::GenCall>>,
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
    /// The cohort's spend under its own key, and (generation v2) each
    /// frontier vendor's under `gen:<vendor>:<cohort key>`, settled first.
    Settle { seq: i64, key: String, microusd: u64, vendors: Vec<(String, u64)> },
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
    /// Admission: a verifying session's durable step: write its files
    /// manifest (`manifest`) or audit its recorded objects after its run met
    /// a missing or changed one (`audit`).
    AdmissionVerify { session: String, stage: String },
    /// Admission: run the claimable verdicts on the scorer.
    AdmissionLanes,
    /// Admission: delete the queued objects (sessions that ended without
    /// admission, superseded ones, refused objects).
    AdmissionCleanup,
    /// Admission: the upload path's end-to-end canary (the close's first
    /// step, and the retry while an upload-path fault is outstanding).
    AdmissionCanary,
    /// Admission: one batch of the closing cohort's close sweep.
    AdmissionClose,
    /// Admission: the alarm sweep of unrecorded objects (once per tick).
    AdmissionSweep,
    /// Admission: one page of the standing reconciliation of `admission/v2/`.
    AdmissionReconcile,
    /// The calendar: queue the next cohort from the standing template.
    OpenNext { label: String, plan: Vec<u8> },
    /// Admission: charge one run's measured instance time to the tracking
    /// ledger under its own key; marked settled only after the ledger
    /// answers (`admission::charge_settled`).
    AdmissionSettle { key: String, microusd: u64 },
    /// Run one outage probe of paused cohort `seq` ([`probe_due`]) and
    /// record it ([`probe_done`]).
    Probe { seq: i64, code: String, probe: Probe },
    /// Write abandoned cohort `seq`'s signed abandonment record
    /// ([`abandonment_record`]) to the receipts bucket, then mark it written.
    AbandonMarker { seq: i64 },
    /// Write one queued operator decision record
    /// (`decide::record_of`), then mark it written.
    DecisionRecord { id: i64 },
}

/// The next root step or sweep, if any. Phase moves that need no I/O (all
/// generation finished, all scoring finished) happen here, in the same
/// transaction. Bulk work is never done here: it is dispatched to lanes.
pub fn next_work(sql: &impl Sql, cfg: &EvalConfig, now_ms: u64) -> R<Option<Work>> {
    // A finished cohort's spend is charged to the ledger when no work runs, so
    // a ledger outage never delays evaluation. The purge of private
    // per-document predictions comes first, so a failing ledger (a settle
    // that keeps returning an error) never holds private results in the
    // bucket. A due closure comes first; one that failed waits out its retry
    // time (or its fault) without holding anything else (`v3::closure_due`).
    // An operator abandon whose lanes hold no claim any longer completes
    // first (`decide::finish_drains`).
    crate::eval::decide::finish_drains(sql, now_ms)?;
    if let Some(seq) = crate::eval::v3::closure_due(sql, Some(now_ms))? {
        return Ok(Some(Work::Close { seq }));
    }
    if !cfg.is_load()
        && let Some(w) = crate::eval::admission::next_work(sql, cfg, now_ms)?
    {
        return Ok(Some(w));
    }
    let Some(c) = active(sql)? else {
        if bookkeeping_due(sql, BOOKKEEPING_PURGE, now_ms)?
            && let Some(w) = crate::eval::lanes::purge_due(sql, now_ms)?
        {
            return Ok(Some(w));
        }
        if bookkeeping_due(sql, BOOKKEEPING_SETTLE, now_ms)?
            && let Some(w) = crate::eval::lanes::settle_due(sql, cfg, now_ms)?
        {
            return Ok(Some(w));
        }
        if bookkeeping_due(sql, BOOKKEEPING_CHARGE, now_ms)?
            && let Some(w) = crate::eval::admission::charge_due(sql)?
        {
            return Ok(Some(w));
        }
        if let Some(w) = retention_work(sql, cfg, now_ms)? {
            return Ok(Some(w));
        }
        // A cohort paused by an outage probes it once a probe interval
        // (`probe_due`), last: the bookkeeping above is quick and a probe
        // can take minutes, so the purge of private predictions, settles,
        // receipt deletion and the abandonment record never wait behind an
        // outage's probes; and a bookkeeping step that fails waits out its
        // retry time ([`bookkeeping_failed`]), so a ledger or bucket outage
        // never holds the probe either.
        return probe_due(sql, cfg, now_ms);
    };
    let work = active_work(sql, cfg, c, now_ms)?;
    // While the active cohort only waits on its lanes (a sweep with nothing
    // to send or stop), receipt deletion and predecessor notices run, so
    // back-to-back cohorts never leave the N+2 rule waiting for an idle gap.
    if let Some(Work::Dispatch { kicks, stop, .. }) = &work
        && kicks.is_empty()
        && stop.is_empty()
        && let Some(w) = retention_work(sql, cfg, now_ms)?
    {
        return Ok(Some(w));
    }
    Ok(work)
}

/// The next receipt deletion step, else a due predecessor notice
/// (`eval::retention`), else a due abandonment record
/// ([`abandon_marker_due`]): bookkeeping that runs when admission and the
/// active cohort have nothing to send, so none of it holds them up.
fn retention_work(sql: &impl Sql, cfg: &EvalConfig, now_ms: u64) -> R<Option<Work>> {
    if bookkeeping_due(sql, BOOKKEEPING_RETENTION, now_ms)? {
        if let Some(w) = crate::eval::retention::next_work(sql, cfg)? {
            return Ok(Some(w));
        }
        if let Some(w) = crate::eval::retention::notice_due(sql, cfg)? {
            return Ok(Some(w));
        }
    }
    if let Some(w) = abandon_marker_due(sql, cfg, Some(now_ms))? {
        return Ok(Some(w));
    }
    crate::eval::decide::record_due(sql, now_ms)
}

/// Bookkeeping kinds with their own retry time ([`bookkeeping_failed`]): the
/// purge of a finished cohort's private predictions ([`Work::Purge`]).
pub const BOOKKEEPING_PURGE: &str = "purge";
/// A finished cohort's spend settle ([`Work::Settle`]).
pub const BOOKKEEPING_SETTLE: &str = "settle";
/// An admission run's charge settle ([`Work::AdmissionSettle`]).
pub const BOOKKEEPING_CHARGE: &str = "charge";
/// Receipt deletion and predecessor notices ([`Work::RetireReceipts`],
/// [`Work::RetentionNotice`]).
pub const BOOKKEEPING_RETENTION: &str = "retention";

/// The bookkeeping kind of `w`, if it is one ([`bookkeeping_failed`]).
pub fn bookkeeping_kind(w: &Work) -> Option<&'static str> {
    Some(match w {
        Work::Purge { .. } => BOOKKEEPING_PURGE,
        Work::Settle { .. } => BOOKKEEPING_SETTLE,
        Work::AdmissionSettle { .. } => BOOKKEEPING_CHARGE,
        Work::RetireReceipts { .. } | Work::RetentionNotice { .. } => BOOKKEEPING_RETENTION,
        _ => return None,
    })
}

/// A bookkeeping step of `kind` failed (`code`, a short content-free code):
/// that kind waits out [`STEP_RETRY_MS`] while every other step, the outage
/// probe included, goes on.
pub fn bookkeeping_failed(sql: &impl Sql, kind: &str, code: &str, now_ms: u64) -> R<()> {
    sql.exec(
        "INSERT INTO bookkeeping_retry (kind, after_ms, code) VALUES (?, ?, ?)
         ON CONFLICT (kind) DO UPDATE SET after_ms = excluded.after_ms, code = excluded.code",
        &[s(kind), bind_u64(now_ms.saturating_add(STEP_RETRY_MS))?, s(crate::eval::lanes::textgen_safe(code))],
    )?;
    Ok(())
}

/// A bookkeeping step of `kind` succeeded: an earlier failure is over.
pub fn bookkeeping_done(sql: &impl Sql, kind: &str) -> R<()> {
    sql.exec("DELETE FROM bookkeeping_retry WHERE kind = ?", &[s(kind)])?;
    Ok(())
}

fn bookkeeping_due(sql: &impl Sql, kind: &str, now_ms: u64) -> R<bool> {
    Ok(sql.exec("SELECT 1 FROM bookkeeping_retry WHERE kind = ? AND after_ms > ?", &[s(kind), bind_u64(now_ms)?])?.is_empty())
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
                    if !screen_generated_lengths(sql, cfg, &c, now_ms)?.clean() {
                        return crate::eval::lanes::generation_sweep(sql, cfg, &c, now_ms).map(Some);
                    }
                    set_phase(sql, seq, "sealing", now_ms, "generation complete")?;
                    return Ok(Some(Work::Seal { seq }));
                }
                return crate::eval::lanes::generation_sweep(sql, cfg, &c, now_ms).map(Some);
            }
            if pending == 0 && !screen_generated_lengths(sql, cfg, &c, now_ms)?.clean() {
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
        "sealing" => {
            if let Some(l) = &c.plan.load {
                return Ok(Some(Work::LoadSeal { seq, source: l.source.clone() }));
            }
            // A cohort that entered `sealing` under a build without the
            // generation token check (no clean screen in its phase log,
            // [`token_screen_done`]) is screened here, before its seal step:
            // one over the limit sends the cohort back to `generating`,
            // where the next generator replaces it, since nothing is exposed
            // before the seal commits. The seal runs only after a complete
            // screen that sends back none, and a cohort with one recorded is
            // not counted again here (the seal's own backstop count stays).
            if token_screen_done(sql, seq)? {
                return Ok(Some(Work::Seal { seq }));
            }
            let screen = screen_generated_lengths(sql, cfg, &c, now_ms)?;
            if screen.sent_back > 0 {
                set_phase(sql, seq, "generating", now_ms, "token screen sent back generated documents")?;
                let c = cohort(sql, seq)?;
                return crate::eval::lanes::generation_sweep(sql, cfg, &c, now_ms).map(Some);
            }
            if screen.refused.is_some() {
                return Ok(Some(Work::Dispatch { seq, kicks: Vec::new(), stop: Vec::new() }));
            }
            Ok(Some(Work::Seal { seq }))
        }
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
    /// Generation v2: the vendor's content-free reply (with the answer's
    /// visible text). Absent for every v1 result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<Box<slop_ninja_gen_core::outcome::GenReply>>,
    /// Generation v2: the effort level the call ran at (its claim's cell),
    /// so a stale result is recorded and charged on the cell it actually
    /// ran, not the document's current one. Absent for every v1 result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<slop_ninja_gen_core::vendor::Effort>,
}

impl GenOutcome {
    /// A v2 result from the vendor's reply: `output` mirrors the outcome
    /// for the lane's events and the limiter's feedback.
    pub fn from_reply(it: &GenItem, reply: slop_ninja_gen_core::outcome::GenReply) -> Self {
        use slop_ninja_gen_core::outcome::OutcomeCode as O;
        let output = match reply.outcome {
            O::Answer => reply.text.clone().ok_or(Reject::Empty),
            O::Throttle => Err(Reject::Throttled),
            O::CellRefused => Err(Reject::ModelUnavailable),
            O::Outage | O::InterfaceMismatch | O::FlexUnavailable | O::SpendCap | O::KeyUnavailable => Err(Reject::Transport),
            O::Refusal | O::LengthTokensOut | O::LengthBytes | O::ModelMismatch => Err(Reject::Empty),
        };
        GenOutcome {
            idx: it.idx,
            generator: it.generator.clone(),
            output,
            prompt_tokens: reply.usage.input_tokens + reply.usage.cached_input_tokens,
            completion_tokens: reply.usage.output_tokens,
            ms: reply.latency_ms,
            claim: it.claim,
            reply: Some(Box::new(reply)),
            effort: it.call.as_deref().map(|c| c.request.effort),
        }
    }
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

pub(crate) fn gen_backoff_ms(failures: i64) -> u64 {
    let shift = (failures.clamp(1, 16) - 1) as u32;
    GEN_BACKOFF_BASE_MS.saturating_mul(1 << shift).min(GEN_BACKOFF_MAX_MS)
}

/// Throttle backoff: min(60 s, 2 s x 2^t) plus a deterministic jitter under 1 s.
pub fn throttle_backoff_ms(idx: i64, throttles: i64) -> u64 {
    let base = 2_000u64.saturating_mul(1 << (throttles.clamp(0, 5) as u32)).min(60_000);
    base + (idx.unsigned_abs().wrapping_mul(7_919).wrapping_add(throttles.unsigned_abs().wrapping_mul(104_729))) % 1_000
}

/// The wait before a throttled item's next call: the backoff, or the
/// provider's hint when longer, but never past the plan's throttle bound
/// (`since + max_throttle_ms`, plus 1 ms so the next throttle reply is past
/// it). A long hint (an endpoint may ask for up to an hour) therefore
/// reaches the outage path, and the fallback, at the bound rather than after
/// the full hint.
pub fn throttle_wait_ms(idx: i64, throttles: i64, retry_after_ms: Option<u64>, since_ms: u64, max_throttle_ms: u64, now_ms: u64) -> u64 {
    let wait = throttle_backoff_ms(idx, throttles).max(retry_after_ms.unwrap_or(0));
    let left = since_ms.saturating_add(max_throttle_ms).saturating_sub(now_ms).saturating_add(1);
    wait.min(left)
}

/// Validates and records generation results from lanes.
///
/// A model answer (accepted, or rejected for its content) uses one of the
/// item's [`MAX_GEN_ATTEMPTS`] and rotates the generator, as does a model the
/// provider no longer serves. An outage uses none: the item backs off
/// exponentially, and its consecutive outages reaching the plan's
/// `fanout.generation.failures_before_pause` pause the cohort
/// (`generation_unavailable`, cleared by the first probe that the provider
/// answers, [`probe_due`], or by an operator), so an outage never drops items
/// from the batch. A throttle (rate limit or capacity) uses no
/// attempt and keeps the generator: the item backs off, until its accumulated
/// throttling passes the plan's `max_throttle_ms`, after which a throttle counts
/// as an outage. A result whose claim is no longer the item's latest records
/// its usage (the spend was real) and is otherwise discarded.
pub fn apply_generation(sql: &impl Sql, cfg: &EvalConfig, seq: i64, results: &[GenOutcome], now_ms: u64) -> R<Value> {
    let c = cohort(sql, seq)?;
    if c.phase != "generating" {
        return late_generation(sql, cfg, &c, results);
    }
    let gl = &c.plan.fanout().map_err(|e| EvalError::Plan(e.0))?.generation;
    let (max_throttle, pause_after) = (gl.max_throttle_ms, i64::from(gl.failures_before_pause()));
    let seed = cohort_seed(&cfg.private_seed, &c.label);
    let (mut accepted, mut rejected, mut outages, mut throttled, mut fenced) = (0, 0, 0, 0, 0);
    let mut pause = false;
    let mut cost_total = 0u64;
    let mut released = 0u64;
    // Whether the commit's last token count finished (`None`: no count ran).
    let mut last_count_ok = None;
    // Generation v2 (a plan naming `generation`): the cohort's bound
    // manifest, read once per commit when a v2 document is in it.
    let mut v2: Option<crate::eval::gen_v2::Ctx> = None;
    for g in results {
        let rows = sql.exec(
            "SELECT kind, parent, spec, attempts, status, infra_failures, claim_gen, reserved_microusd, throttles,
                    throttle_since_ms, key, gen_record, domain
             FROM docs WHERE seq = ? AND idx = ?",
            &[i(seq), i(g.idx)],
        )?;
        let r = rows.first().ok_or(EvalError::State("generated doc"))?;
        if opt_text(r, 11).is_some() {
            if v2.is_none() {
                v2 = crate::eval::gen_v2::Ctx::load(sql, &c, &cfg.private_seed)?;
            }
            let ctx = v2.as_ref().ok_or(EvalError::State("generation v2 record in a v1 cohort"))?;
            let a = crate::eval::gen_v2::apply_one(sql, ctx, &c, max_throttle, pause_after, r, g, now_ms)?;
            accepted += a.accepted;
            rejected += a.rejected;
            outages += a.outages;
            throttled += a.throttled;
            fenced += a.fenced;
            cost_total += a.cost_microusd;
            released += a.released_microusd;
            pause |= a.pause;
            if a.count_ok.is_some() {
                last_count_ok = a.count_ok;
            }
            continue;
        }
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
        // The final document must fit the scorer's token limit (finding L9):
        // one over it could be scored by no artifact, so it is rejected like
        // an answer outside the length window and the item retries on the
        // next generator (`length_tokens`). A count the arena refuses decides
        // nothing about the answer: the item is handled as an outage (no
        // attempt used, back-off, counted toward the plan's pause), the
        // refusal is recorded by its class, and the commit's other results
        // still apply.
        let mut refused = None;
        let validated = match validated {
            Ok(a) if current => match crate::eval::v3::scorer_length(&a.0) {
                Ok(fits) => {
                    last_count_ok = Some(true);
                    if fits { Ok(a) } else { Err(Reject::TokenLimit) }
                }
                Err(r) => {
                    last_count_ok = Some(false);
                    refused = Some(r);
                    Err(Reject::Transport)
                }
            },
            v => v,
        };
        if let Some(r) = refused {
            r.record(sql, seq, now_ms)?;
        }
        let outage = current && (is_outage(g) || refused.is_some());
        let (ok, err, rej, thr) = match &validated {
            _ if !current => (0, 0, 0, 0),
            _ if outage => (0, 1, 0, 0),
            _ if is_throttle => (0, 0, 0, 1),
            Ok(_) => (1, 0, 0, 0),
            Err(_) => (0, 0, 1, 0),
        };
        record_gen_usage(sql, seq, &g.generator, (ok, rej, err, thr, i64::from(!current)), g, cost)?;
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
            // and the plan's `failures_before_pause`th in a row pauses the cohort.
            outages += 1;
            let failures = int(r, 5)? + 1;
            pause |= failures >= pause_after;
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
                let item = RejectedItem { idx: g.idx, kind: text(r, 0)?, spec: text(r, 2)?, key: text(r, 10)? };
                reject_answer(sql, &c, &seed, &item, int(r, 3)? + 1, e, Answers::Counted)?;
            }
        }
    }
    sql.exec(
        "UPDATE cohorts SET gen_cost_microusd = gen_cost_microusd + ?,
            gen_reserved_microusd = MAX(0, gen_reserved_microusd - ?) WHERE seq = ?",
        &[bind_u64(cost_total)?, bind_u64(released)?, i(seq)],
    )?;
    if accepted + rejected > 0 {
        // The provider answered: the probe's back-off starts again.
        note_progress(sql, seq, now_ms)?;
    }
    if last_count_ok == Some(true) {
        // A count finished after any refusal: a busy pool's step error is
        // over (the refused item already backed off as an outage), so the
        // status shows no stale error through the rest of generation.
        clear_token_scratch_error(sql, seq)?;
    }
    if pause {
        fault(sql, seq, GENERATION_UNAVAILABLE, now_ms)?;
    }
    Ok(json!({"accepted": accepted, "rejected": rejected, "outages": outages, "throttled": throttled,
        "fenced": fenced, "cost_microusd": cost_total}))
}

/// A generation commit after the cohort left `generating`: nothing of it
/// applies to a document. A generation v2 result is still a vendor call
/// (one that outlived its claim's lease, whose item another lane finished):
/// its spend is recorded once (fenced by its claim token, `gen_results`)
/// on the cell it ran, and added to the cohort's generation cost, while
/// the cohort's spend is not yet settled (`settled_microusd`, the ledger's
/// one charge of the cohort). A v1 result (Workers AI, reported with the
/// cohort's Cloudflare spend), or any result after the settle, is refused
/// as before: a stale call that returns only after its cohort settled (the
/// settle runs once the cohort reaches transition, closes or is abandoned)
/// stays out of the cohort's spend.
fn late_generation(sql: &impl Sql, cfg: &EvalConfig, c: &Cohort, results: &[GenOutcome]) -> R<Value> {
    let outside = EvalError::State("generation outside generating");
    if c.plan.generation.is_none() {
        return Err(outside);
    }
    let settled = sql.exec("SELECT settled_microusd IS NOT NULL FROM cohorts WHERE seq = ?", &[i(c.seq)])?;
    if settled.first().is_none_or(|r| int(r, 0).unwrap_or(1) != 0) {
        return Err(outside);
    }
    let mut v2: Option<crate::eval::gen_v2::Ctx> = None;
    let (mut late, mut fenced, mut cost_total) = (0u32, 0u32, 0u64);
    for g in results {
        let rows = sql.exec(
            "SELECT kind, parent, spec, attempts, status, infra_failures, claim_gen, reserved_microusd, throttles,
                    throttle_since_ms, key, gen_record, domain
             FROM docs WHERE seq = ? AND idx = ?",
            &[i(c.seq), i(g.idx)],
        )?;
        let Some(r) = rows.first() else { continue };
        if opt_text(r, 11).is_none() {
            continue;
        }
        if v2.is_none() {
            v2 = crate::eval::gen_v2::Ctx::load(sql, c, &cfg.private_seed)?;
        }
        let ctx = v2.as_ref().ok_or(EvalError::State("generation v2 record in a v1 cohort"))?;
        // Outside `generating` no result is current: `apply_one` records
        // the call's spend (or fences a redelivery) and applies nothing.
        let a = crate::eval::gen_v2::apply_one(sql, ctx, c, 0, i64::MAX, r, g, 0)?;
        late += 1;
        fenced += a.fenced;
        cost_total += a.cost_microusd;
    }
    sql.exec("UPDATE cohorts SET gen_cost_microusd = gen_cost_microusd + ? WHERE seq = ?", &[bind_u64(cost_total)?, i(c.seq)])?;
    Ok(json!({"accepted": 0, "rejected": 0, "outages": 0, "throttled": 0, "fenced": fenced, "late": late, "cost_microusd": cost_total}))
}

/// Adds one call to the cohort's per-model generation usage: `(accepted,
/// rejected, errors, throttled, fenced)` and the call's tokens, time and cost.
pub(crate) fn record_gen_usage(sql: &impl Sql, seq: i64, model: &str, n: (i64, i64, i64, i64, i64), g: &GenOutcome, cost: u64) -> R<()> {
    let (ok, rej, err, thr, fenced) = n;
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
            s(model),
            i(ok),
            i(rej),
            i(err),
            bind_u64(g.prompt_tokens)?,
            bind_u64(g.completion_tokens)?,
            bind_u64(cost)?,
            bind_u64(g.ms)?,
            i(thr),
            i(fenced),
        ],
    )?;
    Ok(())
}

/// An item whose answer was rejected.
struct RejectedItem<'a> {
    idx: i64,
    kind: &'a str,
    spec: &'a str,
    key: &'a str,
}

/// How a rejection counts toward the item's [`MAX_GEN_ATTEMPTS`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answers {
    /// An answer rejected when it arrived: at the item's last answer the
    /// item is dropped.
    Counted,
    /// An answer a build without the token check accepted, sent back by the
    /// pre-seal screen: that build's acceptance never costs the item its
    /// replacement, so the item always gets at least one further answer.
    SentBack,
}

/// Records a rejected answer of `item`: `attempts` answers used so far, at
/// most [`MAX_GEN_ATTEMPTS`] before the item is dropped (`failed`; a spare
/// or the plan's shortfall tolerance covers it, else the cohort faults as
/// the terminal `batch_shortfall`), except that a screen send-back
/// ([`Answers::SentBack`]) always leaves the item `pending`; the rejection
/// code; and the next generator in the rotation. The item's claim token
/// advances, so a redelivered commit of the rejected answer, or of any
/// earlier answer under the same claim, is fenced (usage only) and never
/// uses a second attempt.
fn reject_answer(
    sql: &impl Sql,
    c: &Cohort,
    seed: &[u8; 32],
    item: &RejectedItem<'_>,
    attempts: i64,
    e: Reject,
    answers: Answers,
) -> R<()> {
    let status = if attempts >= MAX_GEN_ATTEMPTS && answers == Answers::Counted { "failed" } else { "pending" };
    // The next answer comes from the next generator in the rotation.
    let avoid = if item.kind == "mixed" {
        serde_json::from_str::<MixedSpec>(item.spec).map(|s| s.avoid).unwrap_or_default()
    } else {
        Vec::new()
    };
    let v1_next = textgen::generator_avoiding(&c.plan.generators, &item_digest(seed, item.key), attempts as u32, &avoid);
    // A generation v2 item keeps its current cell (only the pre-seal screen
    // sends a v2 answer back here; the v2 commit picks retry cells itself).
    let v2_next = crate::eval::gen_v2::doc_gen(sql, c.seq, item.idx)?.map(|g| g.call.model);
    let next = v2_next.as_deref().unwrap_or(v1_next);
    sql.exec(
        "UPDATE docs SET attempts = ?, status = ?, infra_failures = 0, throttle_since_ms = 0,
            not_before_ms = 0, generator_next = ?, rejects = rejects || ?, claim_gen = claim_gen + 1
         WHERE seq = ? AND idx = ?",
        &[i(attempts), s(status), s(next), s(format!("{};", e.code())), i(c.seq), i(item.idx)],
    )?;
    Ok(())
}

/// The outcome of a [`screen_generated_lengths`] pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LengthScreen {
    /// Documents sent back (over the limit).
    pub sent_back: u32,
    /// The first count the arena refused, if any: the documents it held were
    /// skipped for this pass, and the refusal is recorded by its class.
    pub refused: Option<crate::eval::v3::TokenRefusal>,
}

impl LengthScreen {
    /// A complete pass that sent back nothing: the cohort may seal.
    pub fn clean(&self) -> bool {
        self.sent_back == 0 && self.refused.is_none()
    }
}

/// The pre-seal length screen (finding L9): before a cohort leaves
/// `generating`, and before each seal step of a cohort in `sealing`, every
/// ready generated batch document (mixed or model-only; spares too, since a
/// spare keeps docset `batch` until the seal relabels it) is counted under
/// the scorer's tokenizer ([`crate::eval::v3::scorer_length`]).
/// [`apply_generation`] already rejects an over-limit answer when it arrives,
/// so this finds only one accepted by a build without that check. Such a
/// document's answer is reclassified as rejected (`length_tokens`; its
/// attempt was counted when it was accepted, and the model's usage moves
/// from accepted to rejected), and the item goes back to `pending` for the
/// next generator, even after its last counted answer: the earlier build's
/// acceptance never costs the item its replacement ([`Answers::SentBack`]),
/// so the screen alone never drops an item into the spares or the plan's
/// shortfall rule. A count the arena
/// refuses skips that document for this pass and is recorded by its class
/// ([`crate::eval::v3::TokenRefusal::record`]: a transient one as a step
/// error with a retry time, so `/eval/status` shows why the cohort waits);
/// it never fails the caller's transaction. The cohort seals only after a
/// complete pass that sends back none ([`LengthScreen::clean`]), which also
/// clears a step error an earlier refused pass recorded and is noted once in
/// the phase log ([`token_screen_done`]), so a seal step retried in
/// `sealing` does not count the batch again. Reads one document at a time.
pub fn screen_generated_lengths(sql: &impl Sql, cfg: &EvalConfig, c: &Cohort, now_ms: u64) -> R<LengthScreen> {
    let seed = cohort_seed(&cfg.private_seed, &c.label);
    let idxs = sql
        .exec(
            "SELECT idx FROM docs WHERE seq = ? AND docset = 'batch' AND status = 'ready' AND kind != 'human_only'
             ORDER BY idx",
            &[i(c.seq)],
        )?
        .iter()
        .map(|r| int(r, 0))
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = LengthScreen::default();
    for idx in idxs {
        let rows = sql.exec("SELECT text, kind, spec, attempts, key, generator FROM docs WHERE seq = ? AND idx = ?", &[i(c.seq), i(idx)])?;
        let r = rows.first().ok_or(EvalError::State("generated doc"))?;
        match crate::eval::v3::scorer_length(text(r, 0)?) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(e) => {
                out.refused.get_or_insert(e);
                continue;
            }
        }
        out.sent_back += 1;
        if let Some(model) = opt_text(r, 5) {
            sql.exec(
                "UPDATE gen_usage SET accepted = accepted - 1, rejected = rejected + 1
                 WHERE seq = ? AND model = ? AND accepted > 0",
                &[i(c.seq), s(model)],
            )?;
        }
        let item = RejectedItem { idx, kind: text(r, 1)?, spec: text(r, 2)?, key: text(r, 4)? };
        reject_answer(sql, c, &seed, &item, int(r, 3)?, Reject::TokenLimit, Answers::SentBack)?;
    }
    match out.refused {
        Some(e) => e.record(sql, c.seq, now_ms)?,
        None => {
            // A complete pass: a refused pass's step error is over.
            clear_token_scratch_error(sql, c.seq)?;
            if out.sent_back == 0 && !token_screen_done(sql, c.seq)? {
                note_token_screen_done(sql, c.seq, now_ms)?;
            }
        }
    }
    Ok(out)
}

/// Clears cohort `seq`'s step error when it is a busy pool's refused token
/// count ([`crate::eval::v3::TOKEN_SCRATCH_REFUSED`]); any other is kept.
fn clear_token_scratch_error(sql: &impl Sql, seq: i64) -> R<()> {
    sql.exec(
        "UPDATE cohorts SET step_error = NULL, step_error_since_ms = NULL, step_error_since_epoch = NULL,
            step_errors = 0, step_retry_ms = 0 WHERE seq = ? AND step_error = ?",
        &[i(seq), s(crate::eval::v3::TOKEN_SCRATCH_REFUSED)],
    )?;
    Ok(())
}

/// The phase-log note of a complete pre-seal screen that sent back nothing,
/// naming the token table it counted under.
pub fn token_screen_note() -> String {
    format!("token screen clean ({})", slop_ninja_scorer_tokens::TABLE_ID)
}

/// Whether cohort `seq` recorded a complete, clean pre-seal screen under
/// this build's table ([`token_screen_note`]). After one, every generated
/// document it may seal was counted: by that screen, or on arrival
/// ([`apply_generation`]).
pub fn token_screen_done(sql: &impl Sql, seq: i64) -> R<bool> {
    Ok(!sql.exec("SELECT 1 FROM phase_log WHERE seq = ? AND note = ? LIMIT 1", &[i(seq), s(token_screen_note())])?.is_empty())
}

fn note_token_screen_done(sql: &impl Sql, seq: i64, now_ms: u64) -> R<()> {
    let o = observed(sql)?;
    sql.exec(
        "INSERT INTO phase_log (seq, phase, at_ms, confirmed_number, epoch_index, note)
         SELECT seq, phase, ?, ?, ?, ? FROM cohorts WHERE seq = ?",
        &[
            bind_u64(now_ms)?,
            o.confirmed.map_or(SqlValue::Null, |c| i(c.number as i64)),
            o.epoch.map_or(SqlValue::Null, |e| i(e.index as i64)),
            s(token_screen_note()),
            i(seq),
        ],
    )?;
    Ok(())
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
/// container did not start or load, a refused batch, a bucket read or write,
/// a refused commit); pauses the cohort (`scorer_unavailable`) at the plan's
/// bound. Any commit resets the count. `cause` and, for a slot failure, the
/// role of job `job` are kept while failures count (`scorer_cause`) and
/// recorded with the fault (`fault_cause`), so the automatic probe clears
/// only what it exercises ([`probed_cause`]): one failure the probe cannot
/// see (a store failure, a refused commit, an unclassified one) in a run
/// leaves the pause to an operator clear.
pub fn note_scorer_failure(sql: &impl Sql, seq: i64, cause: ScorerCause, job: Option<i64>, now_ms: u64) -> R<bool> {
    let code = match (cause, job) {
        (ScorerCause::Slot, Some(job)) => {
            let role = sql.exec("SELECT role FROM jobs WHERE seq = ? AND job = ?", &[i(seq), i(job)])?;
            match role.first().map(|r| text(r, 0)).transpose()? {
                Some("previous_reference") => "slot:previous_reference",
                _ => "slot",
            }
        }
        (c, _) => c.code(),
    };
    // A cause the probe cannot see sticks for the run; a previous
    // reference's slot failure outranks a plain slot failure.
    sql.exec(
        "UPDATE cohorts SET scorer_failures = scorer_failures + 1,
            scorer_cause = CASE
                WHEN scorer_cause IS NOT NULL AND scorer_cause NOT IN ('slot', 'slot:previous_reference') THEN scorer_cause
                WHEN ? NOT IN ('slot', 'slot:previous_reference') THEN ?
                WHEN ? = 'slot:previous_reference' THEN ?
                ELSE COALESCE(scorer_cause, ?) END
         WHERE seq = ?",
        &[s(code), s(code), s(code), s(code), s(code), i(seq)],
    )?;
    let c = cohort(sql, seq)?;
    let bound = c.plan.fanout().map(|f| i64::from(f.score.failures_before_pause)).unwrap_or(SCORER_FAILURES_MAX);
    if c.scorer_failures >= bound {
        let recorded = sql.exec("SELECT scorer_cause FROM cohorts WHERE seq = ?", &[i(seq)])?;
        let recorded = recorded.first().and_then(|r| opt_text(r, 0));
        fault_with_cause(sql, seq, SCORER_UNAVAILABLE, recorded.as_deref(), now_ms)?;
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

/// A root step of cohort `seq` completed: its time is counted and any
/// failure it recorded before ([`note_step_error`]) clears.
pub fn note_step(sql: &impl Sql, seq: i64, ms: u64) -> R<()> {
    sql.exec(
        "UPDATE cohorts SET steps = steps + 1, step_ms = step_ms + ?, step_error = NULL, step_error_since_ms = NULL, step_error_since_epoch = NULL,
            step_errors = 0, step_retry_ms = 0 WHERE seq = ?",
        &[bind_u64(ms)?, i(seq)],
    )?;
    Ok(())
}

// -------------------------------------------------------------------- status

/// Content-free status: phases, counts, costs, timings and public digests.
pub fn status(sql: &impl Sql, now_ms: u64) -> R<Value> {
    let o = observed(sql)?;
    let held = crate::eval::retention::held_seqs(sql)?;
    let journal = crate::eval::boundaries::rows(sql)?;
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
                counts, abandoned_reason, abandoned_fault, step_error, step_error_since_ms, step_errors, step_retry_ms,
                score_base_nanousd, gen_base_microusd, step_error_since_epoch, attempt_extensions, extension_receipts,
                budget_alert, probes, probe_failures, probe_ms, probe_nanousd, probe_last, probe_after_ms,
                abandon_marker_ms, fault_cause, auto_cleared, probe_pending_nanousd, probe_started_ms,
                abandon_marker_retry_ms, abandon_marker_error, probe_until_ms FROM cohorts WHERE seq = ?")?[0];
        let bound_jobs: Vec<i64> = q("SELECT job FROM jobs WHERE seq = ? AND receipt_bound_reached = 1 ORDER BY job")?
            .iter()
            .map(|r| int(r, 0))
            .collect::<Result<_, _>>()?;
        let receipts = int(&q("SELECT COUNT(*) FROM receipts WHERE seq = ?")?[0], 0)?;
        // Aggregate work progress for the publisher's dashboard (two sums,
        // on this authenticated route only): documents ready of those not
        // failed, and scoring chunks done of those planned. The publisher
        // publishes only a 10% grid percent of them, read at tempo
        // boundaries; the counts never leave the private status.
        let generated = &q("SELECT COUNT(*), COALESCE(SUM(status = 'ready'), 0) FROM docs WHERE seq = ? AND status != 'failed'")?[0];
        let scored = &q("SELECT COALESCE(SUM(done_chunks), 0), COALESCE(SUM(chunks), 0) FROM jobs WHERE seq = ?")?[0];
        let published_active = opt_int(&q("SELECT published_active FROM cohorts WHERE seq = ?")?[0], 0);
        // The boundary its coordinator admission window closed at (the
        // dashboard's closing step), for every cohort, not only the
        // calendar's current one.
        let adm = q("SELECT closed_at_epoch, cutoff_block FROM adm_cohorts WHERE seq = ?")?;
        let window_closed_epoch = adm.first().and_then(|r| opt_int(r, 0));
        let cutoff_block = adm.first().and_then(|r| opt_int(r, 1));
        // The tempo boundaries this cohort's dates use, resolved against the
        // boundary journal (`eval::boundaries`): blocks and boundary times
        // only, never a work commit's own time.
        let marks = &q("SELECT opened_epoch, published_epoch, closed_epoch, opened_ms FROM cohorts WHERE seq = ?")?[0];
        let u = |v: Option<i64>| v.and_then(|v| u64::try_from(v).ok());
        let log_epochs: Vec<u64> = q("SELECT epoch_index FROM phase_log WHERE seq = ? AND epoch_index IS NOT NULL")?
            .iter()
            .filter_map(|r| u(opt_int(r, 0)))
            .collect();
        let running = !matches!(c.phase.as_str(), "closed" | "abandoned");
        let epochs = crate::eval::boundaries::cohort_epochs(
            &[u(opt_int(marks, 0)), u(window_closed_epoch), u(opt_int(marks, 1)), u(opt_int(marks, 2))],
            &log_epochs,
            o.epoch.map(|e| e.index).filter(|_| running),
        );
        let boundaries: Vec<Value> =
            crate::eval::boundaries::resolve(&journal, &epochs, crate::eval::calendar::NOMINAL_BLOCK_SECS * 1000)
                .iter()
                .map(crate::eval::boundaries::BoundaryPoint::json)
                .collect();
        let retention = crate::eval::retention::cohort_status(sql, c.seq, &held)?;
        let v3 = if c.plan.is_v3() {
            let st = crate::eval::v3::state(sql, c.seq)?;
            let e = &q("SELECT admission_closed_epoch, published_epoch, closure_digest FROM cohorts WHERE seq = ?")?[0];
            json!({"probe_build": c.plan.is_probe_build(), "probe_cut": c.plan.is_probe_cut(),
                "probe_version": c.plan.reward_v1.as_ref().and_then(|v| v.probe_version), "mode": st.mode, "promoted": st.promoted,
                "exposed_reuse": c.plan.reward_v1.as_ref().and_then(|v| v.reuse.exposed).map(crate::eval::plan::ExposedReuse::name),
                "parent_pools": c.plan.reward_v1.as_ref().map(|v| v.parent_pools.iter().map(|p| p.pool_id.as_str()).collect::<Vec<_>>()),
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
            "published_active": published_active, "window_closed_epoch": window_closed_epoch,
            "opened_ms": opt_int(marks, 3), "cutoff_block": cutoff_block, "boundaries": boundaries,
            "roster_cap": c.plan.admission.as_ref().map(|a| a.roster_cap),
            "progress": json!({"generation": {"ready": int(generated, 1)?, "total": int(generated, 0)?},
                "scoring": {"done": int(scored, 0)?, "total": int(scored, 1)?}}),
            "admission_tempos": c.plan.admission_tempos, "transition_tempos": c.plan.transition_tempos,
            "max_attempts": c.plan.max_attempts, "scorer_failures": c.scorer_failures,
            "counts": opt_text(row, 15).and_then(|d| serde_json::from_str::<Value>(&d).ok()),
            "abandoned": opt_text(row, 16).map(|r| json!({"reason": r, "fault": opt_text(row, 17)})),
            "chunk_attempts": chunk_attempts,
            "draw": opt_text(row, 10).and_then(|d| serde_json::from_str::<Value>(&d).ok()),
            "docs": docs, "rejects": rejects, "rejects_by_kind": rejects_by_kind, "generation": usage,
            "generation_cost_microusd": c.gen_cost_microusd,
            // By vendor (micro-USD): frontier vendors apart from Workers AI,
            // for the operator and the private spend tracker.
            "gen_cost_by_vendor": crate::eval::gen_v2::cost_by_vendor(sql, &c)?,
            "generation_v2": crate::eval::gen_v2::status(sql, &c)?,
            // The span gate's void state and journal (`null` without the block).
            "span_gate": crate::eval::span_gate::status(sql, &c)?,
            "budget_microusd": c.plan.generation_budget_microusd,
            "jobs": jobs, "job_count": job_rows.len(), "job_status": job_status, "lanes": lanes, "load": c.plan.is_load(),
            "receipts_recorded": receipts, "receipt_retention": retention,
            "manifest": opt_text(row, 0), "evaluation_set": opt_text(row, 1), "task_count": opt_int(row, 2),
            "receipt_root": opt_text(row, 3), "receipt_count": opt_int(row, 4),
            "package": opt_text(row, 5), "target": opt_text(row, 6),
            "steps": int(row, 7)?, "step_ms": int(row, 8)?, "r2_puts": int(row, 9)?,
            // A failing root step (`note_step_error`): its code, since when
            // (and since which stored epoch), how many times in a row, and
            // when it may run again.
            "step_error": opt_text(row, 18).map(|code| json!({"code": code, "since_ms": opt_int(row, 19),
                "since_epoch": opt_int(row, 24), "count": opt_int(row, 20), "retry_ms": opt_int(row, 21),
                "deterministic": step_error_is_deterministic(&code)})),
            // The spend each budget counts from since its last clear.
            "budget_base": json!({"scoring_microusd": opt_int(row, 22).map(|n| (n.max(0) as u64).div_ceil(1000)),
                "generation_microusd": opt_int(row, 23)}),
            // The first budget guard passed (`note_budget`): tracking only.
            "budget_alert": opt_text(row, 27),
            // Infrastructure attempt extensions past `max_attempts`
            // (`lanes::extend_attempts`) and the receipts they may add.
            "attempt_extensions": opt_int(row, 25), "extension_receipts": opt_int(row, 26),
            // What raised the fault (a scorer failure class, a definitive
            // refusal and its role, or an admission subject), and whether
            // the fault clears by itself (`self_clearing`: a probed outage in
            // a work phase, or an admission outage whose subjects are
            // re-read each step) or needs an operator clear or abandonment
            // (`operator_clear_needed`, which the publisher alerts on).
            "fault_cause": opt_text(row, 35),
            "self_clearing": c.fault.as_deref().is_some_and(|f| self_clearing(&c.phase, f, opt_text(row, 35).as_deref())),
            "operator_clear_needed": c.fault.as_deref().is_some_and(|f| !self_clearing(&c.phase, f, opt_text(row, 35).as_deref())),
            // The automatic outage probe (`probe_due`): runs, consecutive
            // failures, measured time and spend (with the bound held for a
            // probe in flight and its deadline), last outcome, next run,
            // whether one is due for this fault at all (`probed_cause`), and
            // the automatic clears in a row with no progress since, which
            // back the interval off (`probe_interval_ms`).
            "probes": json!({"runs": int(row, 28)?, "failures": int(row, 29)?, "ms": int(row, 30)?,
                "microusd": (int(row, 31)?.max(0) as u64).div_ceil(1000),
                "pending_bound_microusd": (int(row, 37)?.max(0) as u64).div_ceil(1000), "started_ms": opt_int(row, 38),
                "until_ms": opt_int(row, 41),
                "last": opt_text(row, 32),
                "probed": c.fault.as_deref().is_some_and(|f| WORK_PHASES.contains(&c.phase.as_str())
                    && self_clearing(&c.phase, f, opt_text(row, 35).as_deref())),
                "no_progress_clears": int(row, 36)?,
                "interval_ms": probe_interval_ms(int(row, 36)?),
                "next_ms": if c.fault.as_deref().is_some_and(|f| PROBED_FAULTS.contains(&f)) { opt_int(row, 33) } else { None }}),
            // Jobs whose attempts no further extension fits under the receipt
            // bound: a clear of `unresolved_infrastructure` runs them into it
            // again unless the deployed bound grew; abandonment is the way out.
            "receipt_bound_reached_jobs": bound_jobs,
            "abandonment_record_ms": opt_int(row, 34),
            "abandonment_record_retry": json!({"after_ms": opt_int(row, 39), "error": opt_text(row, 40)}),
            "reward_v1": v3,
            "log": log,
        }));
    }
    let used = int(&sql.exec("SELECT COUNT(*) FROM pool_used", &[])?[0], 0)?;
    let watermark_epoch = opt_int(&sql.exec("SELECT watermark_epoch FROM eval_obs WHERE id = 1", &[])?[0], 0);
    Ok(json!({
        // The epoch index at the confirmed watermark as last read: ahead of
        // `observed.epoch` while an increment waits for a head reading.
        "observed": o, "watermark_epoch": watermark_epoch, "kick_age_ms": now_ms.saturating_sub(o.kick_ms),
        "active_tempo_rule": ACTIVE_TEMPO_RULE, "resumable_faults": RESUMABLE_FAULTS,
        "auto_cleared_faults": {"probe": PROBED_FAULTS, "evidence": EVIDENCE_CLEARED_FAULTS, "probe_every_ms": PROBE_EVERY_MS,
            "probe_backoff_doublings": PROBE_BACKOFF_DOUBLINGS},
        // Bookkeeping kinds waiting out a retry after a failure
        // (`bookkeeping_failed`): kind, when it may run again, and its code.
        "bookkeeping_retry": sql.exec("SELECT kind, after_ms, code FROM bookkeeping_retry ORDER BY kind", &[])?
            .iter()
            .map(|r| Ok(json!({"kind": text(r, 0)?, "after_ms": int(r, 1)?, "code": text(r, 2)?})))
            .collect::<R<Vec<Value>>>()?,
        "pool_rows_used": used, "receipt_retention": crate::eval::retention::status(sql)?, "cohorts": out,
        // The subnet's UID-to-hotkey map as of the stored tempo boundary,
        // from the commitment log (public chain facts; the publisher's
        // dashboard source). `null` before a boundary is observed.
        "uid_map": match &o.epoch {
            Some(e) => crate::eval::commitments::uid_map(sql, e.index, e.last_epoch_block)?,
            None => Value::Null,
        },
    }))
}

/// Ensures there is a reason to keep stepping: [`blocking_work`], a due
/// receipt deletion (`eval::retention`), or an abandonment record not yet
/// written ([`abandon_marker_due`], which never blocks a succession). A predecessor notice is not work:
/// it is retried whenever a step runs, so an unreachable predecessor never
/// keeps this object's alarm armed on its own.
pub fn has_work(sql: &impl Sql, cfg: &EvalConfig) -> R<bool> {
    Ok(blocking_work(sql, cfg)?
        || crate::eval::retention::has_work(sql)?
        || abandon_marker_due(sql, cfg, None)?.is_some()
        || crate::eval::decide::record_pending(sql)?)
}

/// [`has_work`] counting admission's backed-off steps, clean-ups and object
/// deletes only once due at `now_ms`: what re-arms the step alarm.
pub fn has_due_work(sql: &impl Sql, cfg: &EvalConfig, now_ms: u64) -> R<bool> {
    Ok(work(sql, cfg, Some(now_ms))?
        || crate::eval::retention::has_work(sql)?
        || abandon_marker_due(sql, cfg, Some(now_ms))?.is_some()
        || crate::eval::decide::record_due(sql, now_ms)?.is_some()
        || probe_due(sql, cfg, now_ms)?.is_some()
        || crate::eval::live::due(sql)?)
}

/// What a kick arms the step alarm for: [`has_due_work`], or, on a tick (a
/// kick that carries the epoch), a due predecessor notice
/// (`eval::retention::notice_due`). A notice that came back with work left,
/// or failed, is so retried once a tick (the cron cadence), on an otherwise
/// idle store too, until the predecessor reports nothing left; the alarm
/// itself never re-arms for a notice, so an unreachable predecessor costs
/// one attempt a tick and holds nothing else.
pub fn kick_due(sql: &impl Sql, cfg: &EvalConfig, kick: &Kick, now_ms: u64) -> R<bool> {
    Ok(has_due_work(sql, cfg, now_ms)?
        || (kick.epoch.is_some() && crate::eval::retention::notice_due(sql, cfg)?.is_some()))
}

/// The work a succession or an authority switch waits for (`work` in the
/// status; `eval::succession::supersede`, `config::deploy_check`): an active
/// cohort with work, a closure, a finished cohort whose spend is not yet
/// charged to the ledger or whose private results are not yet purged,
/// admitted objects not yet released (`admission::objects_retained`: the
/// publisher's report not yet recorded, or their deletes not yet queued), or
/// admission or calendar work under `cfg` (a retired authority's calendar
/// opens nothing, so it is no reason to step). Retained objects alone never
/// re-arm the step alarm ([`has_due_work`]): only the publisher's report
/// releases them, and it arms the step itself. Due receipt deletions are not
/// included: a superseded authority finishes them under its successor's
/// notices (`eval::retention`), so waiting for them adds nothing, and a
/// storage fault there never holds up a restart.
pub fn blocking_work(sql: &impl Sql, cfg: &EvalConfig) -> R<bool> {
    work(sql, cfg, None)
}

fn work(sql: &impl Sql, cfg: &EvalConfig, due_by: Option<u64>) -> R<bool> {
    Ok(active(sql)?.is_some()
        // An operator abandon draining a cohort's lanes ends at a later step.
        || !sql.exec("SELECT 1 FROM cohorts WHERE draining_ms IS NOT NULL LIMIT 1", &[])?.is_empty()
        || crate::eval::admission::has_work(sql, cfg, due_by)?
        || (due_by.is_none() && crate::eval::admission::objects_retained(sql)?)
        || crate::eval::v3::closure_due(sql, due_by)?.is_some()
        || !sql
            .exec(
                "SELECT 1 FROM cohorts WHERE (phase IN ('transition','closed','abandoned')
                   AND reserved_microusd IS NOT NULL AND settled_microusd IS NULL)
                   OR (phase IN ('closed','abandoned') AND purged_ms IS NULL) LIMIT 1",
                &[],
            )?
            .is_empty())
}
