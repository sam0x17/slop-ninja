//! Coordinator admission of a reward-v1 cohort's roster: contract
//! `slop_ninja.admission/v2` (`docs/testnet/admission-api.md`), as
//! storage-neutral functions over [`Sql`] that the `EvalCohort` Durable
//! Object runs, one transaction each, around the I/O the eval Worker does
//! (`cf_admission.rs`): R2 heads and header reads, the signer's mints, the
//! upload Worker's header check and the canary.
//!
//! **Hash once.** Miners upload each artifact object (the header bytes `H`,
//! then each file) straight to R2 under presigned URLs that bind the derived
//! key, the committed length, the committed SHA-256 and create-only; R2
//! verifies the digest as the bytes arrive. No byte reaches a SlopNinja
//! Worker, and nothing here hashes a model byte: an object is recorded from
//! R2's stored size, checksum, entity tag and attributes ([`Head`]). The
//! only SHA-256 computed here is over small records: a session-creation
//! body (one `SubmissionUpload` record), the files manifest (the package
//! digest), the canary body and the control document.
//!
//! - **Window.** A v3 cohort whose plan has an `admission` block admits from
//!   its opening boundary (block `open_block`) until the boundary at which
//!   admission closes (`cutoff_block`). Its validity window, roster cap,
//!   artifact spec and upload URL policy are plan parameters; the object
//!   bound is derived from the spec ([`SpecBounds`]).
//! - **Requests** are signed by the hotkey they concern ([`authenticate`]),
//!   bound to the chain, method, path, body digest and a block bound checked
//!   against the RPC-confirmed cursor. `urls`, `complete` and `repair`
//!   require the signer to be the session's hotkey.
//! - **Sessions** follow the contract's state machine: one session per
//!   hotkey and commitment, its objects (the header, then each distinct
//!   file of the recorded header) with their recorded entity tags, one
//!   constant-size mint aggregate per session (a count and the latest URL
//!   expiry), the last refused request per hotkey and cohort.
//! - **Settling the header** ([`HeaderJob`], [`apply_header`]): R2's `head()`
//!   of the header key must show the envelope's size and SHA-256 with
//!   allowed attributes; the bytes are read pinned to that entity tag and
//!   decoded by the key-free upload Worker; the content digest is recomputed
//!   here from the returned table (`rebuild_header`) and must be the
//!   committed one, the contract the cohort's, and
//!   `SubmissionUpload::admit_header` must pass against the logged
//!   commitment. Only then are file URLs minted, so an artifact doomed by
//!   its header is refused before any file is sent.
//! - **Recording files** ([`apply_heads`]): from the client's `complete`, the
//!   R2 object-create event ([`observe`]), the alarm's sweep ([`sweep_due`])
//!   or, once `closing` is persisted, the close sweep alone. An object
//!   present with other bytes or attributes is deleted and may be PUT
//!   again. When the header and every file are recorded the session
//!   verifies: the files manifest v2 names each object's key and entity tag,
//!   its digest is the package, and the scorer's admission run loads it.
//! - **Platform faults.** A mint the signer could not make
//!   (`signer_unavailable`), a failed end-to-end canary
//!   (`upload_path_unavailable`), or a header settle or link probe that
//!   failed for the platform's reason (`upload_front_unavailable`: R2's
//!   head or pinned read, the upload Worker's header route, or its
//!   registration read, which every signed route needs) is persisted and
//!   defers the close until a later success; the close begins with a fresh
//!   canary, which also runs the link probe. The upload path's outage never
//!   ends a miner's upload.
//! - **Verdicts** come from an admission run on the scorer Container: one
//!   run per hotkey and finalized commitment, bound to the scorer binary and
//!   the cohort's spec and recorded as reached on them (rule (b)),
//!   definitive only on a `refused <code>`; attributable load failures and a
//!   failed control document on fresh instances count toward the plan's
//!   `artifact_failures_before_model_error`; platform failures retry with a
//!   growing backoff, show in the status, and at every
//!   `failures_before_pause` of them pause the cohort (`scorer_unavailable`).
//!   A run that met a missing or changed object audits the session's
//!   objects: absent is the platform's (repair), present with another
//!   entity tag or a forbidden attribute is the miner's (`object_replaced`).
//! - **Cutoff.** At the boundary where the admission minimum is reached (and
//!   after a canary passed) the window closes: `closing` is persisted, the
//!   close sweep heads every unrecorded object of every open session once
//!   (batched, progress persisted per object), then the close is recorded
//!   and sessions still missing an object are refused. The roster freezes
//!   at that boundary, or at the boundary after its work is in: every
//!   completed upload verified, every verdict in, the commitment snapshot at
//!   `cutoff_block - 1` read. Sessions whose commitment a later one
//!   replaced, whose registration changed or whose commitment shares its
//!   extrinsic are refused, the rest are ranked by `(block, extrinsic)`, the
//!   first `roster_cap` admitted and the rest queued as overflow.
//! - **Retention.** Objects of sessions that end without admission are
//!   deleted at the freeze (superseded ones at once); a standing
//!   reconciliation lists `admission/v2/` and deletes any object no live
//!   session holds. Nothing ends on time; nothing here is a lifecycle rule.
//! - **Way out.** An operator can refuse one stalled session with a recorded
//!   reason ([`operator_refuse`]) and abandon a faulted admission cohort
//!   ([`abandoned`]); nothing is skipped silently.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)
)]
// `ApiError` carries the contract's whole error detail by value, as the
// wire crate's shapes do.
#![allow(clippy::result_large_err)]

use crate::eval::commitments::{self, LogEntry};
use crate::eval::engine::{self, Cohort, EvalConfig, EvalError, R, i, observed, opt_int, opt_text, s, set_phase};
use crate::eval::plan::{AdmissionPlan, ArtifactJson, CohortPlan, IdentityJson};
use crate::sql::{Sql, SqlValue, bind_u64, int, text, uint};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use slop_ninja_admission_wire::json::{
    Admitting, ArtifactSpecRef, Chain, CohortInfo, CommitmentView, Detail, ErrorBody, HotkeyView, LastRefusal,
    NextCohort, ObjectState, Position, Refusal, Registration, SCHEMA_COHORT, SCHEMA_HOTKEY, SCHEMA_SESSION, Session,
    SessionSummary, Upload, UploadPolicy,
};
use slop_ninja_admission_wire::{CONTRACT, ErrorCode, RequestScope, SessionState, request_message};
use slop_ninja_artifact_check::admission::{HeaderFile, HeaderOutcome, HeaderResult, rebuild_header};
use slop_ninja_artifact_check::files_manifest::FilesManifest;
use slop_ninja_artifact_check::spec::{self, Version};
use slop_ninja_protocol::submission::{FinalizedCommitment, SubmissionUpload, parse_chain_payload};
use slop_ninja_protocol::{
    AccountId, ChainBinding, Digest, Error as ProtocolError, NeuronIdentity, Record, VerifiedHeader,
};
use slop_ninja_r2_presign::layout::{ADMISSION_PREFIX, ObjectId, ObjectKind};
use slop_ninja_r2_presign::{policy, wire as mint};

/// The single integer a `COUNT(*)`-style query returns.
fn count(sql: &impl Sql, query: &str, params: &[SqlValue]) -> R<i64> {
    Ok(sql.exec(query, params)?.first().map_or(Ok(0), |r| int(r, 0))?)
}

// ------------------------------------------------------------ spec bounds

pub use slop_ninja_artifact_check::admission::{ADMISSION_SPECS, SpecBounds, files_bytes};

/// The upload URL policy a plan names (`admission.upload_policy`), as the
/// compiled policy of that id: the only policy is
/// `slop_ninja_upload_url_policy_v1`. A plan frozen before admission v2
/// names none and mints under that first policy.
pub fn upload_policy(p: &AdmissionPlan) -> UploadPolicy {
    UploadPolicy {
        id: p.upload_policy.as_ref().map_or_else(|| policy::POLICY_ID.to_owned(), |u| u.id.clone()),
        b_min: policy::B_MIN,
        skew_secs: policy::SKEW_SECS,
    }
}

/// The cohort's limits record (content-free), published by digest in the
/// cohort response; it changes only with the plan or the spec.
pub fn limits_json(p: &AdmissionPlan, b: &SpecBounds) -> Value {
    let u = upload_policy(p);
    json!({"schema": "slop_ninja.admission.limits/v2", "contract": CONTRACT, "artifact_spec": b.spec_id,
        "artifact_spec_sha256": b.spec_sha256, "graph_bytes": b.graph_bytes, "files_bytes": b.files_bytes,
        "max_object_len": b.object_bytes,
        "upload_policy": {"id": u.id, "b_min": u.b_min, "skew_secs": u.skew_secs},
        "object_attributes": OBJECT_ATTRIBUTES_RULE,
        "request_validity_blocks": p.request_validity_blocks, "roster_cap": p.roster_cap, "selection": p.selection})
}

pub fn limits_digest(p: &AdmissionPlan, b: &SpecBounds) -> String {
    hex::encode(
        slop_ninja_protocol::domain_hash(
            "slop_ninja.admission/limits/v2",
            limits_json(p, b).to_string().as_bytes(),
        )
        .0,
    )
}

// ---------------------------------------------------------------- schema

/// v10: admission sessions, parts, last refusals, verdicts, cohort windows,
/// the cutoff snapshot, the blobs each session copied, and the standing
/// template (the commitment log is v11, `commitments::SCHEMA`). Kept as it
/// was applied: v25 replaces the session tables.
pub const SCHEMA_V10: &[&str] = &[
    "CREATE TABLE adm_cohorts (
        seq INTEGER PRIMARY KEY, cohort INTEGER NOT NULL UNIQUE,
        state TEXT NOT NULL CHECK (state IN ('admitting','closing','frozen','abandoned')),
        open_block INTEGER NOT NULL, cutoff_block INTEGER, closed_at_epoch INTEGER, frozen_epoch INTEGER,
        ready_epoch INTEGER,
        outside INTEGER NOT NULL DEFAULT 0, admitted INTEGER NOT NULL DEFAULT 0, overflow INTEGER NOT NULL DEFAULT 0
    ) STRICT",
    "CREATE TABLE adm_sessions (
        session TEXT PRIMARY KEY, seq INTEGER NOT NULL, cohort INTEGER NOT NULL, hotkey TEXT NOT NULL,
        upload TEXT NOT NULL, commitment TEXT NOT NULL, content TEXT NOT NULL,
        artifact_sha256 TEXT NOT NULL, artifact_len INTEGER NOT NULL, part_size INTEGER NOT NULL,
        part_count INTEGER NOT NULL, attempt INTEGER NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('uploading','verifying','verified','refused','admitted','overflow','superseded','aborted')),
        stage TEXT, refusal TEXT, pos_block INTEGER, pos_extrinsic INTEGER, next_cohort INTEGER,
        object_key TEXT NOT NULL, upload_id TEXT, package_sha256 TEXT, manifest_key TEXT, model_sha256 TEXT,
        frame TEXT, created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL, cleaned INTEGER NOT NULL DEFAULT 0,
        step_tries INTEGER NOT NULL DEFAULT 0, step_after_ms INTEGER NOT NULL DEFAULT 0,
        manifest_created INTEGER NOT NULL DEFAULT 0
    ) STRICT",
    "CREATE INDEX adm_sessions_hotkey ON adm_sessions (cohort, hotkey)",
    "CREATE INDEX adm_sessions_state ON adm_sessions (seq, state)",
    "CREATE TABLE adm_blobs (
        session TEXT NOT NULL, sha256 TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY (session, sha256)
    ) STRICT",
    "CREATE INDEX adm_blobs_sha ON adm_blobs (sha256)",
    "CREATE TABLE adm_parts (
        session TEXT NOT NULL, attempt INTEGER NOT NULL, part INTEGER NOT NULL, len INTEGER NOT NULL,
        sha256 TEXT NOT NULL, etag TEXT NOT NULL, PRIMARY KEY (session, attempt, part)
    ) STRICT",
    "CREATE TABLE adm_refusals (cohort INTEGER NOT NULL, hotkey TEXT NOT NULL, body TEXT NOT NULL, PRIMARY KEY (cohort, hotkey)) STRICT",
    "CREATE TABLE adm_verdicts (
        hotkey TEXT NOT NULL, commitment TEXT NOT NULL, package_sha256 TEXT NOT NULL, manifest_key TEXT NOT NULL,
        label TEXT NOT NULL, scorer_binary TEXT NOT NULL, spec_sha256 TEXT NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('pending','admitted','refused')), code TEXT,
        tries INTEGER NOT NULL DEFAULT 0, artifact_failures INTEGER NOT NULL DEFAULT 0,
        claim_until_ms INTEGER NOT NULL DEFAULT 0, not_before_ms INTEGER NOT NULL DEFAULT 0, updated_ms INTEGER NOT NULL,
        reached_binary TEXT, reached_spec TEXT, platform_tries INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (hotkey, commitment)
    ) STRICT",
    "CREATE TABLE adm_snapshot_wanted (cutoff_block INTEGER NOT NULL, hotkey TEXT NOT NULL, PRIMARY KEY (cutoff_block, hotkey)) STRICT",
    "CREATE TABLE adm_snapshot_blocks (cutoff_block INTEGER PRIMARY KEY, block_hash TEXT NOT NULL) STRICT",
    "CREATE TABLE adm_snapshot (
        cutoff_block INTEGER NOT NULL, hotkey TEXT NOT NULL, block INTEGER, payload TEXT, uid INTEGER,
        registration_block INTEGER, done INTEGER NOT NULL, PRIMARY KEY (cutoff_block, hotkey)
    ) STRICT",
    "CREATE TABLE standing_template (
        id INTEGER PRIMARY KEY CHECK (id = 1), template TEXT NOT NULL, sha256 TEXT NOT NULL,
        installed_ms INTEGER NOT NULL, paused INTEGER NOT NULL DEFAULT 0, last_error TEXT
    ) STRICT",
];

/// v21: the v1 framing and blob steps' Queue dispatches. Kept as it was
/// applied: v25 drops what it added to the session tables.
pub const SCHEMA_V21: &[&str] = &[
    "ALTER TABLE adm_sessions ADD COLUMN dispatch INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE adm_sessions ADD COLUMN dispatch_state TEXT CHECK (dispatch_state IN ('pending','sent','taken'))",
    "ALTER TABLE adm_sessions ADD COLUMN dispatch_by INTEGER",
    "ALTER TABLE adm_sessions ADD COLUMN dispatch_takes INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE adm_sessions ADD COLUMN delivery_error TEXT",
    "CREATE TABLE adm_blob_deletes (key TEXT NOT NULL, session TEXT NOT NULL, PRIMARY KEY (key, session)) STRICT",
    "ALTER TABLE commit_reader ADD COLUMN maxima_cohort INTEGER",
    "UPDATE commit_reader SET plan_ms_max = 0, ingest_ms_max = 0",
];

/// v25: admission contract v2 (direct upload to R2, hash once), in place.
///
/// - It refuses (the store stays at its pre-deploy version, v23 or v24:
///   `sql::migrate` applies every pending version in one transaction) while
///   any v1 admission is live: a
///   session uploading, verifying or verified, a window admitting or
///   closing, or an admitted session whose cohort is not closed. The
///   deploy's step 0 (`TESTNET.md`) checks the same on the live status
///   first, so the refusal is a backstop, never the plan.
/// - The v1 session tables go: a v1 envelope (whole-object digest, parts)
///   has no v2 form, and no v2 code reads one. The v1 objects of sessions
///   not cleaned up and not admitted are queued for deletion
///   (`adm_object_deletes`, the existing delete loop); admitted v1 objects
///   of closed cohorts, and their eval-bucket blobs, are left in place for
///   the operator's recorded clean-up (`TESTNET.md`, step 0), since no v2
///   table names them. Pending v1 verdicts (never
///   claimable again: their sessions are gone) are dropped; reached ones
///   stay as history.
/// - `adm_sessions` v2: the envelope (header digest and length), the
///   recorded header's file table (`files`, at most the spec's six entries),
///   the package and manifest key once every object is recorded, the mint
///   aggregate (`mints`, `url_until_ms`: never a row per mint), and
///   `published` (the publisher reported the entrant published or withheld:
///   with the cohort closed, the end of the repair window and of the
///   admitted objects' retention).
/// - `adm_objects`: each object of a session (the header, each distinct
///   file), with R2's entity tag once recorded, `missing` once a recorded
///   object was found absent (repair), and `close_headed` once the close
///   sweep headed it (the sweep's per-object progress).
/// - `adm_platform`: the persisted upload-path faults (`signer_unavailable`,
///   `upload_path_unavailable`, `upload_front_unavailable`) and their last
///   successes, the alarm sweep's
///   and the reconciliation's last runs and the reconciliation's cursor.
/// - `adm_cohorts`: the boundary a close is wanted at (its cutoff block,
///   epoch and when it was first wanted: the canary must pass after it),
///   whether the close is recorded (the close sweep finished; `closing`
///   cohorts of v1 had recorded theirs), and the close sweep's backoff
///   after a head that errored.
pub const SCHEMA_V25: &[&str] = &[
    // The guard (review findings: an unconditional drop would strand a live
    // v1 cohort): the migration refuses, leaving the store at its pre-deploy
    // version (v23 or v24) for the previously deployed build, while any v1 session is uploading, verifying or
    // verified, any admission window is admitting or closing, or an
    // admitted v1 session's cohort has not closed (its v1 files manifest
    // cannot load in a v2 lane). Review round 2 adds: any cohort queued (a
    // plan frozen before v25 names a v1 reference package, which the v2
    // lanes and the seal refuse), any v1 blob delete still pending, and any
    // created eval-bucket blob of a v1 session not yet cleaned up (no v2
    // table names them, so dropping their rows would orphan the objects).
    // The insert fails its CHECK in that case, and the whole migration rolls
    // back with it.
    "CREATE TABLE adm_v25_guard (live INTEGER NOT NULL CHECK (live = 0)) STRICT",
    "INSERT INTO adm_v25_guard SELECT
        (SELECT COUNT(*) FROM adm_sessions WHERE state IN ('uploading','verifying','verified'))
      + (SELECT COUNT(*) FROM adm_cohorts WHERE state IN ('admitting','closing'))
      + (SELECT COUNT(*) FROM adm_sessions s WHERE s.state = 'admitted'
           AND COALESCE((SELECT c.state FROM adm_cohorts c WHERE c.seq = s.seq), '') != 'abandoned'
           AND COALESCE((SELECT k.phase FROM cohorts k WHERE k.seq = s.seq), '') != 'closed')
      + (SELECT COUNT(*) FROM cohorts WHERE phase = 'queued')
      + (SELECT COUNT(*) FROM adm_blob_deletes)
      + (SELECT COUNT(*) FROM adm_blobs b JOIN adm_sessions s ON s.session = b.session
           WHERE b.created = 1 AND s.cleaned = 0 AND s.state != 'admitted')",
    "DROP TABLE adm_v25_guard",
    "INSERT OR IGNORE INTO adm_object_deletes (key, upload_id, created_ms)
       SELECT object_key, upload_id, updated_ms FROM adm_sessions WHERE cleaned = 0 AND state != 'admitted'",
    "DELETE FROM adm_verdicts WHERE state = 'pending'",
    "DROP TABLE adm_parts",
    "DROP TABLE adm_blobs",
    "DROP TABLE adm_blob_deletes",
    "DROP TABLE adm_sessions",
    "CREATE TABLE adm_sessions (
        session TEXT PRIMARY KEY, seq INTEGER NOT NULL, cohort INTEGER NOT NULL, hotkey TEXT NOT NULL,
        upload TEXT NOT NULL, commitment TEXT NOT NULL, content TEXT NOT NULL,
        header_sha256 TEXT NOT NULL, header_len INTEGER NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('uploading','verifying','verified','refused','admitted','overflow','superseded')),
        stage TEXT, refusal TEXT, pos_block INTEGER, pos_extrinsic INTEGER, next_cohort INTEGER,
        files TEXT, package_sha256 TEXT, manifest_key TEXT, model_sha256 TEXT,
        mints INTEGER NOT NULL DEFAULT 0, url_until_ms INTEGER NOT NULL DEFAULT 0,
        created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL, cleaned INTEGER NOT NULL DEFAULT 0,
        step_tries INTEGER NOT NULL DEFAULT 0, step_after_ms INTEGER NOT NULL DEFAULT 0,
        published INTEGER NOT NULL DEFAULT 0, read_faults INTEGER NOT NULL DEFAULT 0
    ) STRICT",
    "CREATE INDEX adm_sessions_hotkey ON adm_sessions (cohort, hotkey)",
    "CREATE INDEX adm_sessions_state ON adm_sessions (seq, state)",
    "CREATE INDEX adm_sessions_commitment ON adm_sessions (cohort, commitment)",
    "CREATE TABLE adm_objects (
        session TEXT NOT NULL, kind TEXT NOT NULL CHECK (kind IN ('h','f')), sha256 TEXT NOT NULL,
        len INTEGER NOT NULL, etag TEXT, missing INTEGER NOT NULL DEFAULT 0, close_headed INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (session, kind, sha256)
    ) STRICT",
    "CREATE TABLE adm_platform (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        signer_fault_ms INTEGER, signer_error TEXT, mint_ok_ms INTEGER,
        canary_fault_ms INTEGER, canary_error TEXT, canary_ok_ms INTEGER, canary_after_ms INTEGER NOT NULL DEFAULT 0,
        front_fault_ms INTEGER, front_error TEXT, front_ok_ms INTEGER,
        sweep_ms INTEGER NOT NULL DEFAULT 0, reconcile_ms INTEGER NOT NULL DEFAULT 0, reconcile_cursor TEXT,
        reconcile_deleted INTEGER NOT NULL DEFAULT 0
    ) STRICT",
    "INSERT INTO adm_platform (id) VALUES (1)",
    "CREATE TABLE adm_deleting (key TEXT PRIMARY KEY, since_ms INTEGER NOT NULL) STRICT",
    "ALTER TABLE adm_cohorts ADD COLUMN want_cutoff INTEGER",
    "ALTER TABLE adm_cohorts ADD COLUMN want_epoch INTEGER",
    "ALTER TABLE adm_cohorts ADD COLUMN want_ms INTEGER",
    "ALTER TABLE adm_cohorts ADD COLUMN close_recorded INTEGER NOT NULL DEFAULT 1",
    "ALTER TABLE adm_cohorts ADD COLUMN close_after_ms INTEGER NOT NULL DEFAULT 0",
];

// ------------------------------------------------------- platform bounds

/// The documented Workers limit on subrequests per invocation (default
/// 10,000; `data/task-roots/testnet/fanout/LIMITS.md`, version
/// `workers-limits-2026-10-30`). R2 binding calls count against it.
pub const SUBREQUESTS_PER_INVOCATION: usize = 10_000;

/// The R2 calls one admission step makes at most (heads, header reads,
/// deletes, a list page): half the per-invocation limit, so the alarm's
/// other steps in the same invocation (each bounded by its own count) keep
/// the other half. Larger work is batched across steps with its progress
/// persisted (the close sweep's `close_headed`, the reconciliation's cursor).
pub const R2_CALLS_PER_STEP: usize = SUBREQUESTS_PER_INVOCATION / 2;

/// How often the alarm sweep, the platform-fault canary and the standing
/// reconciliation run: once per fallback tick (the coordinator's one-minute
/// cron, `engine::PROBE_EVERY_MS`), however many kicks arrive.
pub const TICK_MS: u64 = engine::PROBE_EVERY_MS;
// ------------------------------------------------------------ API errors

/// A refused request or session, with its contract code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    pub detail: Detail,
    /// `Retry-After` in seconds, when a useful wait is known.
    pub retry_after: Option<u64>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: Detail::default(),
            retry_after: None,
        }
    }
    pub fn with(mut self, detail: Detail) -> Self {
        self.detail = detail;
        self
    }
    /// The HTTP status of a request error (a session refusal is never one).
    pub fn status(&self) -> u16 {
        self.code.http_status().unwrap_or(500)
    }
    pub fn body(&self) -> ErrorBody {
        ErrorBody::new(self.code, self.message.clone(), self.detail.clone())
    }
    fn refusal(&self) -> Refusal {
        Refusal {
            code: self.code.as_str().into(),
            message: self.message.clone(),
            detail: self.detail.clone(),
        }
    }
}

impl From<EvalError> for ApiError {
    fn from(e: EvalError) -> Self {
        let _ = e;
        ApiError::new(ErrorCode::Internal, "store")
    }
}

impl From<crate::sql::StoreError> for ApiError {
    fn from(e: crate::sql::StoreError) -> Self {
        ApiError::from(EvalError::from(e))
    }
}

type A<T> = Result<T, ApiError>;

pub use slop_ninja_admission_wire::internal::{Forward, Reply, Signed};

fn reply_ok<T: Serialize>(status: u16, v: &T) -> Reply {
    Reply::new(status, serde_json::to_value(v).unwrap_or(Value::Null), None)
}

/// The reply of a refused request.
pub fn reply_error(e: &ApiError) -> Reply {
    Reply::new(e.status(), serde_json::to_value(e.body()).unwrap_or(Value::Null), e.retry_after)
}

// ----------------------------------------------------------------- window

/// The admission window of one cohort.
#[derive(Clone, Debug)]
pub struct Window {
    pub seq: i64,
    pub cohort: u64,
    pub label: String,
    pub opened_epoch: u64,
    pub open_block: u64,
    pub planned_cutoff_epoch: u64,
    pub state: String,
    pub cutoff_block: Option<u64>,
    pub plan: AdmissionPlan,
    pub output_contract: String,
    pub bounds: SpecBounds,
    pub k_tempos: u64,
    pub failures_before_refusal: u32,
    /// The plan's `fanout.score.failures_before_pause`: consecutive platform
    /// failures of one admission step (a verdict run that met a platform
    /// failure, a commitment resolution without state) after which the
    /// cohort pauses visibly (`scorer_unavailable`, `state_unavailable`), as
    /// lane start failures pause it while scoring.
    pub failures_before_pause: u32,
}

impl Window {
    pub fn admitting(&self) -> bool {
        self.state == "admitting"
    }
}

fn window_of(sql: &impl Sql, c: &Cohort) -> R<Option<Window>> {
    let Some(a) = c.plan.admission.clone() else {
        return Ok(None);
    };
    let rows = sql.exec(
        "SELECT state, open_block, cutoff_block FROM adm_cohorts WHERE seq = ?",
        &[i(c.seq)],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    let bounds = SpecBounds::of_id(&a.artifact_spec).ok_or(EvalError::State("admission spec"))?;
    let v1 = c
        .plan
        .reward_v1
        .as_ref()
        .ok_or(EvalError::State("admission without reward_v1"))?;
    let failures = c
        .plan
        .fanout
        .as_ref()
        .and_then(|f| f.score.artifact_failures_before_model_error)
        .unwrap_or(1);
    Ok(Some(Window {
        seq: c.seq,
        cohort: c.plan.cohort,
        label: c.label.clone(),
        opened_epoch: c.opened_epoch.ok_or(EvalError::State("opened epoch"))?,
        open_block: uint(r, 1)?,
        planned_cutoff_epoch: c.opened_epoch.unwrap_or(0).saturating_add(c.plan.admission_tempos),
        state: text(r, 0)?.to_owned(),
        cutoff_block: opt_int(r, 2).map(|v| v as u64),
        plan: a,
        output_contract: v1.output_contract.clone(),
        bounds,
        k_tempos: c.plan.k_tempos,
        failures_before_refusal: failures.max(1),
        failures_before_pause: c
            .plan
            .fanout()
            .map_or(engine::SCORER_FAILURES_MAX, |f| i64::from(f.score.failures_before_pause))
            .clamp(1, i64::from(u32::MAX)) as u32,
    }))
}

/// The window of the cohort that admits or is closing, if any.
pub fn window(sql: &impl Sql) -> R<Option<Window>> {
    let rows = sql.exec(
        "SELECT seq FROM adm_cohorts WHERE state IN ('admitting','closing') ORDER BY seq DESC LIMIT 1",
        &[],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    let c = engine::cohort(sql, int(r, 0)?)?;
    window_of(sql, &c)
}

/// The window of cohort number `cohort`, in any state.
pub fn window_for(sql: &impl Sql, cohort: u64) -> R<Option<Window>> {
    let rows = sql.exec("SELECT seq FROM adm_cohorts WHERE cohort = ?", &[bind_u64(cohort)?])?;
    let Some(r) = rows.first() else { return Ok(None) };
    let c = engine::cohort(sql, int(r, 0)?)?;
    window_of(sql, &c)
}

/// The most recent admission plan known (the window's, else the last
/// cohort's, else the standing template's), for the validity bound of
/// signed reads while nothing admits.
fn recent_plan(sql: &impl Sql) -> R<Option<AdmissionPlan>> {
    if let Some(w) = window(sql)? {
        return Ok(Some(w.plan));
    }
    let rows = sql.exec("SELECT seq FROM adm_cohorts ORDER BY seq DESC LIMIT 1", &[])?;
    if let Some(r) = rows.first() {
        return Ok(engine::cohort(sql, int(r, 0)?)?.plan.admission);
    }
    Ok(crate::eval::calendar::template(sql)?.and_then(|t| t.admission))
}

/// Records a cohort's opening (the engine's `queued -> admission` move).
pub fn open(sql: &impl Sql, c: &Cohort, open_block: u64) -> R<()> {
    if c.plan.admission.is_none() {
        return Ok(());
    }
    if !sql.exec("SELECT 1 FROM adm_cohorts WHERE seq = ?", &[i(c.seq)])?.is_empty() {
        return Ok(());
    }
    sql.exec(
        "INSERT INTO adm_cohorts (seq, cohort, state, open_block) VALUES (?, ?, 'admitting', ?)",
        &[i(c.seq), bind_u64(c.plan.cohort)?, bind_u64(open_block)?],
    )?;
    // The commitment reader's call maxima measure each admission window on
    // its own (the upload load they are judged under is the window's).
    sql.exec(
        "UPDATE commit_reader SET plan_ms_max = 0, ingest_ms_max = 0, maxima_cohort = ? WHERE id = 1",
        &[bind_u64(c.plan.cohort)?],
    )?;
    Ok(())
}

// --------------------------------------------------------- authentication

/// The finalized cursor: the RPC-confirmed watermark.
pub fn cursor(sql: &impl Sql) -> R<Option<u64>> {
    Ok(observed(sql)?.confirmed.map(|c| c.number))
}

/// A request whose signature verified: the signer, the content digest and
/// `valid_until` (not yet checked against the cursor).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verified {
    pub signer: AccountId,
    pub content: [u8; 32],
    pub valid_until: u64,
}

/// The stateless part of authentication: the headers parse, the sr25519
/// signature verifies over the request message, and a body read whole
/// matches the content digest. The Worker runs it before forwarding, so a
/// request that fails it never reaches the cohort authority; the authority
/// runs it again with the cursor checks ([`authenticate`]).
pub fn verify_request(chain: &ChainBinding, r: &Signed) -> A<Verified> {
    let hotkey = r
        .hotkey
        .as_deref()
        .and_then(|h| crate::chain::public_from_ss58(h).ok())
        .ok_or_else(|| ApiError::new(ErrorCode::BadSignature, "x-sn-hotkey is not an SS58 account"))?;
    let valid_until = r
        .valid_until
        .as_deref()
        .and_then(slop_ninja_admission_wire::parse_valid_until)
        .ok_or_else(|| ApiError::new(ErrorCode::BadRequest, "x-sn-valid-until"))?;
    let content = r
        .content_sha256
        .as_deref()
        .and_then(slop_ninja_admission_wire::parse_hex32)
        .ok_or_else(|| ApiError::new(ErrorCode::BadRequest, "x-sn-content-sha256"))?;
    let signature = r
        .signature
        .as_deref()
        .and_then(slop_ninja_admission_wire::parse_signature)
        .ok_or_else(|| ApiError::new(ErrorCode::BadSignature, "x-sn-signature"))?;
    let account = AccountId(hotkey);
    let message = request_message(&RequestScope {
        chain,
        hotkey: &account,
        valid_until,
        method: &r.method,
        path: &r.path,
        content_sha256: &Digest(content),
    });
    if !crate::chain::verify_sr25519(&hotkey, &message, &signature) {
        return Err(ApiError::new(
            ErrorCode::BadSignature,
            "the signature does not verify for x-sn-hotkey",
        ));
    }
    if let Some(b) = &r.body_sha256
        && slop_ninja_admission_wire::parse_hex32(b) != Some(content)
    {
        return Err(ApiError::new(
            ErrorCode::ContentDigestMismatch,
            "the body does not match x-sn-content-sha256",
        ));
    }
    Ok(Verified {
        signer: account,
        content,
        valid_until,
    })
}

/// Verifies a signed request against the cursor and the validity bound.
/// Returns the signer and the request's content digest.
pub fn authenticate(chain: &ChainBinding, cursor: u64, validity: u64, r: &Signed) -> A<(AccountId, [u8; 32])> {
    let Verified {
        signer: account,
        content,
        valid_until,
    } = verify_request(chain, r)?;
    if valid_until < cursor {
        return Err(
            ApiError::new(ErrorCode::RequestExpired, "valid_until is below the finalized cursor").with(Detail {
                finalized_block: Some(cursor),
                ..Default::default()
            }),
        );
    }
    if valid_until > cursor.saturating_add(validity) {
        return Err(ApiError::new(
            ErrorCode::RequestValidityTooLong,
            "valid_until exceeds the allowed window",
        ));
    }
    Ok((account, content))
}

/// Authenticates against the store's cursor and the current validity bound.
pub fn signer(sql: &impl Sql, chain: &ChainBinding, r: &Signed) -> A<(AccountId, [u8; 32])> {
    let cursor = cursor(sql)?.ok_or_else(|| paused("no finalized cursor yet"))?;
    let validity = recent_plan(sql)?
        .map(|p| p.request_validity_blocks)
        .ok_or_else(|| ApiError::new(ErrorCode::NotAdmitting, "no cohort has admitted yet"))?;
    authenticate(chain, cursor, validity, r)
}

fn paused(why: &str) -> ApiError {
    let mut e = ApiError::new(ErrorCode::Paused, why);
    e.retry_after = Some(60);
    e
}


// ---------------------------------------------------------------- sessions

#[derive(Clone, Debug)]
struct Row {
    session: String,
    /// The eval cohort's sequence number (`cohorts.seq`).
    seq: i64,
    cohort: u64,
    hotkey: AccountId,
    upload: SubmissionUpload,
    state: SessionState,
    stage: Option<String>,
    refusal: Option<Refusal>,
    position: Option<Position>,
    next_cohort: Option<u64>,
    /// The recorded header's file table, once the header is recorded.
    files: Option<Vec<HeaderFile>>,
    published: bool,
}

const SESSION_COLUMNS: &str = "session, seq, cohort, hotkey, upload, state, stage, refusal, pos_block, pos_extrinsic, \
    next_cohort, files, published";

fn row_of(r: &crate::sql::Row) -> R<Row> {
    let upload_bytes = hex::decode(text(r, 4)?).map_err(|_| EvalError::State("session upload"))?;
    let upload = SubmissionUpload::from_bytes(&upload_bytes).map_err(|_| EvalError::State("session upload"))?;
    let hk = crate::eval::plan::parse_hex32(text(r, 3)?).ok_or(EvalError::State("session hotkey"))?;
    Ok(Row {
        session: text(r, 0)?.to_owned(),
        seq: int(r, 1)?,
        cohort: uint(r, 2)?,
        hotkey: AccountId(hk),
        upload,
        state: SessionState::parse(text(r, 5)?).ok_or(EvalError::State("session state"))?,
        stage: opt_text(r, 6),
        refusal: opt_text(r, 7).and_then(|t| serde_json::from_str(&t).ok()),
        position: match (opt_int(r, 8), opt_int(r, 9)) {
            (Some(b), Some(x)) => Some(Position {
                block: b as u64,
                extrinsic: x as u32,
            }),
            _ => None,
        },
        next_cohort: opt_int(r, 10).map(|v| v as u64),
        files: match opt_text(r, 11) {
            Some(t) => Some(serde_json::from_str(&t).map_err(|_| EvalError::State("session files"))?),
            None => None,
        },
        published: int(r, 12)? == 1,
    })
}

fn session_row(sql: &impl Sql, id: &str) -> R<Option<Row>> {
    let rows = sql.exec(
        &format!("SELECT {SESSION_COLUMNS} FROM adm_sessions WHERE session = ?"),
        &[s(id)],
    )?;
    rows.first().map(row_of).transpose()
}

fn ss58(a: &AccountId) -> String {
    crate::chain::ss58(a.0)
}

/// What one admission object holds, in the store's two-letter form.
fn kind_code(k: ObjectKind) -> &'static str {
    match k {
        ObjectKind::Header => "h",
        ObjectKind::File(_) => "f",
    }
}

/// One object of a session as recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Obj {
    kind: ObjectKind,
    sha256: [u8; 32],
    len: u64,
    /// R2's entity tag, once recorded present.
    etag: Option<String>,
    /// Recorded, then found absent: repair needed.
    missing: bool,
    close_headed: bool,
}

impl Obj {
    fn recorded(&self) -> bool {
        self.etag.is_some() && !self.missing
    }
}

/// The session's objects: the header first, then the files by digest.
fn objects_of(sql: &impl Sql, id: &str) -> R<Vec<Obj>> {
    sql.exec(
        "SELECT kind, sha256, len, etag, missing, close_headed FROM adm_objects WHERE session = ? ORDER BY kind DESC, sha256",
        &[s(id)],
    )?
    .iter()
    .map(|r| {
        let sha = crate::eval::plan::parse_hex32(text(r, 1)?).ok_or(EvalError::State("object digest"))?;
        Ok(Obj {
            kind: if text(r, 0)? == "h" { ObjectKind::Header } else { ObjectKind::File(sha) },
            sha256: sha,
            len: uint(r, 2)?,
            etag: opt_text(r, 3),
            missing: int(r, 4)? == 1,
            close_headed: int(r, 5)? == 1,
        })
    })
    .collect()
}

/// The R2 key of one object of a session (`slop_ninja_r2_presign::layout`).
pub fn object_key(cohort: u64, commitment: &Digest, kind: ObjectKind) -> String {
    ObjectId { cohort, commitment: commitment.0, kind }.key().as_str().to_owned()
}

fn key_of(r: &Row, kind: ObjectKind) -> String {
    object_key(r.cohort, &r.upload.commitment, kind)
}

/// The artifact's committed size: the header and every file it lists (the
/// URL policy's `L_total`); the header alone before it is recorded.
fn committed_len(r: &Row) -> u64 {
    let files = r.files.as_ref().map_or(0u64, |f| f.iter().fold(0u64, |a, x| a.saturating_add(x.len)));
    u64::from(r.upload.header_len).saturating_add(files)
}

/// The session's JSON (`slop_ninja.admission.session/v2`).
fn session_json(sql: &impl Sql, r: &Row, cursor: u64, uploads: Vec<Upload>) -> R<Session> {
    let objs = objects_of(sql, &r.session)?;
    let present = |kind: ObjectKind| objs.iter().any(|o| o.kind == kind && o.recorded());
    let mut objects = vec![ObjectState {
        kind: "header".into(),
        name: None,
        sha256: r.upload.header_sha256.hex(),
        len: u64::from(r.upload.header_len),
        present: present(ObjectKind::Header),
    }];
    for f in r.files.iter().flatten() {
        let sha = crate::eval::plan::parse_hex32(&f.sha256).unwrap_or([0; 32]);
        objects.push(ObjectState {
            kind: "file".into(),
            name: Some(f.name.clone()),
            sha256: f.sha256.clone(),
            len: f.len,
            present: present(ObjectKind::File(sha)),
        });
    }
    Ok(Session {
        schema: SCHEMA_SESSION.into(),
        session: r.session.clone(),
        cohort: r.cohort,
        hotkey: ss58(&r.hotkey),
        commitment: r.upload.commitment.hex(),
        header_sha256: r.upload.header_sha256.hex(),
        header_len: r.upload.header_len,
        objects,
        state: r.state.as_str().into(),
        refusal: r.refusal.clone(),
        position: r.position,
        next_cohort: r.next_cohort,
        finalized_block: cursor,
        uploads,
        repair_needed: repair_needed(&objs),
    })
}

fn repair_needed(objs: &[Obj]) -> Vec<String> {
    objs.iter().filter(|o| o.missing).map(|o| hex::encode(o.sha256)).collect()
}

fn set_state(sql: &impl Sql, id: &str, state: SessionState, refusal: Option<&ApiError>, now_ms: u64) -> R<()> {
    sql.exec(
        "UPDATE adm_sessions SET state = ?, refusal = ?, updated_ms = ? WHERE session = ?",
        &[
            s(state.as_str()),
            refusal.map_or(SqlValue::Null, |e| {
                s(serde_json::to_string(&e.refusal()).unwrap_or_default())
            }),
            bind_u64(now_ms)?,
            s(id),
        ],
    )?;
    Ok(())
}

/// Records the hotkey's last refusal for `status`, keyed (admitting cohort,
/// hotkey). Only for the cohort that is admitting, and only for a hotkey the
/// commitment reader has seen commit on the subnet (`commit_seen` or the
/// log, bounded by the chain's own commitments): signatures cost nothing, so
/// any other refusal is answered but never stored, and the table stays
/// bounded by the chain's own commitments, whatever cohort or key a request
/// names.
fn record_refusal(
    sql: &impl Sql,
    cursor: u64,
    signer: &AccountId,
    window: Option<&Window>,
    upload: Option<&SubmissionUpload>,
    e: &ApiError,
) -> R<()> {
    let opening_cohort = upload.map(|u| u.opening.cohort);
    let Some(cohort) = window.filter(|w| w.admitting()).map(|w| w.cohort) else {
        return Ok(());
    };
    let who = s(hex::encode(signer.0));
    if sql
        .exec(
            "SELECT 1 FROM commit_seen WHERE hotkey = ? UNION ALL SELECT 1 FROM commit_log WHERE hotkey = ? LIMIT 1",
            &[who.clone(), who],
        )?
        .is_empty()
    {
        return Ok(());
    }
    let last = LastRefusal {
        code: e.code.as_str().into(),
        message: e.message.clone(),
        detail: e.detail.clone(),
        finalized_block: cursor,
        session: upload
            .and_then(|u| u.digest().ok())
            .map_or_else(String::new, |d| d.hex()),
        opening_cohort: opening_cohort.unwrap_or(cohort),
    };
    sql.exec(
        "INSERT INTO adm_refusals (cohort, hotkey, body) VALUES (?, ?, ?)
         ON CONFLICT (cohort, hotkey) DO UPDATE SET body = excluded.body",
        &[
            bind_u64(cohort)?,
            s(hex::encode(signer.0)),
            s(serde_json::to_string(&last).unwrap_or_default()),
        ],
    )?;
    Ok(())
}

/// The hotkey's commitment that counts for the window: its latest logged one
/// while admitting, its last one before the cutoff once closed.
fn counted(sql: &impl Sql, w: &Window, hotkey: &AccountId) -> R<Option<LogEntry>> {
    match w.cutoff_block {
        Some(c) => commitments::last_before(sql, hotkey, c),
        None => commitments::latest(sql, hotkey),
    }
}

/// Whether a definitive commitment answer to this opening must still wait
/// for the commitment reader: the hotkey may have committed again after the
/// reader's last completed poll (a returning miner, or a replaced
/// commitment). The wait is tied to a fixed block, never to the moving
/// finalized cursor (the heartbeat advances it every block, the reader polls
/// less often). The first such answer for an opening records that block
/// (`adm_commit_waits`, one row per hotkey in the admitting cohort), and the
/// answer becomes definitive once a poll has completed at or after it. The
/// block is the latest of:
///
/// - the finalized cursor (a commitment the miner saw finalized before it
///   asked is at or below it, unless the coordinator lags the chain);
/// - the miner's own confirmed commitment block (`hint`, untrusted), which
///   covers a coordinator whose cursor lags the block holding the new
///   commitment (a heartbeat or webhook stall); a hint at or below the last
///   poll means the poll already holds that commitment, so the answer is
///   final at once. A later request with a higher hint for the same opening
///   moves the block up, never down;
/// - without a hint, one past the reader's last completed poll: a poll
///   planned after the answer, so a poll already at the cursor never makes
///   the first answer final (the miner may have committed past it).
///
/// The window's close ends every wait (only open or reopen waits).
fn commitment_wait(sql: &impl Sql, w: &Window, upload: &SubmissionUpload, hint: Option<u64>) -> R<bool> {
    let who = s(hex::encode(upload.opening.hotkey.0));
    let commitment = upload.commitment.hex();
    let polled = commitments::polled_block(sql)?;
    let rows = sql.exec(
        "SELECT commitment, at_block FROM adm_commit_waits WHERE cohort = ? AND hotkey = ?",
        &[bind_u64(w.cohort)?, who.clone()],
    )?;
    let at = match rows.first() {
        Some(r) if text(r, 0)? == commitment => {
            let at = uint(r, 1)?;
            match hint.filter(|h| *h > at) {
                Some(h) => {
                    sql.exec(
                        "UPDATE adm_commit_waits SET at_block = ? WHERE cohort = ? AND hotkey = ?",
                        &[bind_u64(h)?, bind_u64(w.cohort)?, who],
                    )?;
                    h
                }
                None => at,
            }
        }
        _ => {
            let at = cursor(sql)?.unwrap_or(0).max(match hint {
                Some(h) => h,
                None => polled.map_or(0, |p| p.saturating_add(1)),
            });
            // Rows of earlier cohorts are never read again.
            sql.exec("DELETE FROM adm_commit_waits WHERE cohort < ?", &[bind_u64(w.cohort)?])?;
            sql.exec(
                "INSERT INTO adm_commit_waits (cohort, hotkey, commitment, at_block) VALUES (?, ?, ?, ?)
                 ON CONFLICT (cohort, hotkey) DO UPDATE SET commitment = excluded.commitment, at_block = excluded.at_block",
                &[bind_u64(w.cohort)?, who, s(&commitment), bind_u64(at)?],
            )?;
            at
        }
    };
    Ok(polled.is_none_or(|b| b < at))
}

/// The `Retry-After` of a commitment answer that waits for the reader: its
/// shortest poll interval, the earliest a new poll can complete (a slower
/// poll, a stalled heartbeat or a lagging cursor answers the same code again).
fn commitment_retry_secs() -> u64 {
    commitments::POLL_EVERY_MS.div_ceil(1000)
}

/// Who asks for a commitment check: opening or reopening a session, where a
/// newer commitment the reader has not logged yet may still make the opening
/// valid (`hint`: the miner's confirmed commitment block, untrusted), or the
/// verify stage of a session already opened against the logged commitment,
/// where the logged answer is final.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckAt {
    Open { hint: Option<u64> },
    Verify,
}

/// The finalized-commitment check shared by opening, reopening and the
/// verify stage: window, registration and opening. Returns the position.
fn check_commitment(
    sql: &impl Sql,
    chain: &ChainBinding,
    w: &Window,
    upload: &SubmissionUpload,
    at: CheckAt,
) -> A<(Position, LogEntry)> {
    let opening = &upload.opening;
    // The hotkey's newest commitment can never be logged (a shared
    // extrinsic, an undecodable value, state no node keeps): the miner is
    // told to commit again, rather than to retry or that an older logged
    // commitment mismatches. Only while admitting: once closed, the
    // snapshot's last commitment before the cutoff counts.
    if w.cutoff_block.is_none()
        && let Some((block, reason)) = commitments::terminal(sql, &opening.hotkey)?
        && commitments::latest(sql, &opening.hotkey)?.is_none_or(|e| e.block < block)
    {
        return Err(ApiError::new(
            ErrorCode::CommitmentUnresolvable,
            "the hotkey's latest commitment cannot be resolved; commit again with a new nonce",
        )
        .with(Detail { reason: Some(reason), ..Default::default() }));
    }
    let Some(entry) = counted(sql, w, &opening.hotkey)? else {
        let msg = if commitments::unresolved(sql, &opening.hotkey)? {
            "the hotkey's latest commitment is not logged yet"
        } else {
            "no finalized commitment of this hotkey is logged"
        };
        let mut e = ApiError::new(ErrorCode::CommitmentNotFound, msg);
        e.retry_after = Some(commitment_retry_secs());
        return Err(e);
    };
    if commitments::unresolved(sql, &opening.hotkey)? && w.cutoff_block.is_none() {
        // A newer commitment was seen and is still being logged: the
        // opening's commitment may already be overwritten.
        let mut e = ApiError::new(
            ErrorCode::CommitmentNotFound,
            "the hotkey's latest commitment is not logged yet",
        );
        e.retry_after = Some(commitment_retry_secs());
        return Err(e);
    }
    let definitive = match definitive_check(chain, &entry, w, upload) {
        Ok(v) => return Ok(v),
        Err(e) => e,
    };
    // The logged commitment does not open this opening, but at open or
    // reopen the hotkey may have committed again since the reader's last
    // completed poll (a returning miner, or a replaced commitment): the
    // answer waits for a poll at or after the first such answer
    // ([`commitment_wait`]). A wrong chain is the opening's own fault and
    // never waits.
    if let CheckAt::Open { hint } = at
        && w.cutoff_block.is_none()
        && opening.cohort == w.cohort
        && definitive.code != ErrorCode::WrongChain
        && commitment_wait(sql, w, upload, hint)?
    {
        let mut e = ApiError::new(
            ErrorCode::CommitmentNotFound,
            "the hotkey's newest commitment may not be logged yet",
        );
        e.retry_after = Some(commitment_retry_secs());
        return Err(e);
    }
    Err(definitive)
}

/// The logged commitment's answer to the opening: its position, or the
/// definitive refusal code.
fn definitive_check(
    chain: &ChainBinding,
    entry: &LogEntry,
    w: &Window,
    upload: &SubmissionUpload,
) -> A<(Position, LogEntry)> {
    let opening = &upload.opening;
    if entry.block < w.open_block {
        return Err(ApiError::new(
            ErrorCode::CommitmentBeforeOpen,
            "the commitment was made before the cohort opened",
        ));
    }
    let (Some(uid), Some(registration_block)) = (entry.uid, entry.registration_block) else {
        return Err(ApiError::new(
            ErrorCode::NotRegistered,
            "the hotkey had no UID on the subnet at its commitment",
        ));
    };
    let committer = NeuronIdentity {
        uid,
        hotkey: opening.hotkey,
        registration_block,
    };
    let fc = FinalizedCommitment {
        chain,
        committer,
        block: entry.block,
        extrinsic_index: entry.extrinsic,
        payload: &entry.payload,
    };
    match opening.check_finalized(&fc) {
        Ok(r) => Ok((
            Position {
                block: r.commitment_block,
                extrinsic: r.extrinsic_index,
            },
            entry.clone(),
        )),
        Err(ProtocolError::Mismatch("commitment chain")) => {
            Err(ApiError::new(ErrorCode::WrongChain, "the opening names another chain"))
        }
        Err(
            ProtocolError::Mismatch("committing account") | ProtocolError::Invalid("registration after commitment"),
        ) => Err(ApiError::new(
            ErrorCode::RegistrationMismatch,
            "the opening names another registration generation",
        )),
        Err(_) => Err(ApiError::new(
            ErrorCode::CommitmentMismatch,
            "the opening does not open the hotkey's current commitment",
        )
        .with(Detail {
            payload_cohort: parse_chain_payload(&entry.payload).ok().map(|(c, _)| c),
            ..Default::default()
        })),
    }
}


/// The checks of opening a session, in contract order.
fn admission_checks(
    sql: &impl Sql,
    chain: &ChainBinding,
    w: Option<&Window>,
    signer: &AccountId,
    upload: &SubmissionUpload,
    hint: Option<u64>,
) -> A<Window> {
    let opening = &upload.opening;
    if opening.chain != *chain {
        return Err(ApiError::new(
            ErrorCode::WrongChain,
            "the opening names another chain or subnet",
        ));
    }
    let Some(w) = w.filter(|w| w.admitting()) else {
        let closed = window_for(sql, opening.cohort)?.is_some_and(|w| !w.admitting());
        return Err(if closed {
            ApiError::new(ErrorCode::AfterCutoff, "admission has closed for this cohort")
        } else {
            let mut e = ApiError::new(ErrorCode::NotAdmitting, "no cohort is admitting");
            e.retry_after = Some(60);
            e.with(Detail {
                next: crate::eval::calendar::next(sql)?,
                ..Default::default()
            })
        });
    };
    if opening.cohort != w.cohort {
        return Err(
            ApiError::new(ErrorCode::WrongCohort, "the opening names another cohort").with(Detail {
                admitting: Some(w.cohort),
                ..Default::default()
            }),
        );
    }
    if opening.hotkey != *signer {
        return Err(ApiError::new(
            ErrorCode::HotkeyMismatch,
            "the opening names another hotkey than the signer",
        ));
    }
    if upload.header_len == 0 || upload.header_len > slop_ninja_protocol::MAX_ARTIFACT_HEADER_LEN {
        return Err(ApiError::new(
            ErrorCode::HeaderTooLarge,
            "header_len is zero or exceeds MAX_ARTIFACT_HEADER_LEN",
        ));
    }
    check_commitment(sql, chain, w, upload, CheckAt::Open { hint })?;
    Ok(w.clone())
}

/// The one-artifact rule: one session per commitment (another envelope for
/// a commitment that already has a session, in any state, is refused: the
/// miner commits again), so a hotkey's sessions are bounded by its
/// commitments; live sessions of older commitments are superseded, and
/// their objects are queued for deletion at once.
fn one_artifact(sql: &impl Sql, w: &Window, signer: &AccountId, id: &str, upload: &SubmissionUpload, now_ms: u64) -> A<()> {
    let same = sql.exec(
        "SELECT session FROM adm_sessions WHERE cohort = ? AND commitment = ? AND session != ? LIMIT 1",
        &[bind_u64(w.cohort)?, s(upload.commitment.hex()), s(id)],
    )?;
    if let Some(r) = same.first() {
        return Err(ApiError::new(
            ErrorCode::SecondArtifact,
            "this commitment already has a session: commit again to submit another artifact",
        )
        .with(Detail {
            session: Some(text(r, 0)?.to_owned()),
            ..Default::default()
        }));
    }
    let live: Vec<Row> = sql
        .exec(
            &format!(
                "SELECT {SESSION_COLUMNS} FROM adm_sessions WHERE cohort = ? AND hotkey = ? AND session != ?
                AND state IN ('uploading','verifying','verified')"
            ),
            &[bind_u64(w.cohort)?, s(hex::encode(signer.0)), s(id)],
        )?
        .iter()
        .map(row_of)
        .collect::<R<_>>()?;
    for other in live {
        set_state(sql, &other.session, SessionState::Superseded, None, now_ms)?;
        queue_deletes(sql, &other, now_ms)?;
    }
    Ok(())
}

/// Queues every object of a session for deletion (the existing delete loop,
/// `adm_object_deletes`) and marks it cleaned.
fn queue_deletes(sql: &impl Sql, r: &Row, now_ms: u64) -> R<()> {
    for o in objects_of(sql, &r.session)? {
        sql.exec(
            "INSERT OR IGNORE INTO adm_object_deletes (key, upload_id, created_ms) VALUES (?, NULL, ?)",
            &[s(key_of(r, o.kind)), bind_u64(now_ms)?],
        )?;
    }
    sql.exec("UPDATE adm_sessions SET cleaned = 1 WHERE session = ?", &[s(&r.session)])?;
    Ok(())
}

// ---------------------------------------------------------- the steps' I/O

/// One object to head: its session, identity, key and recorded entity tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    pub session: String,
    pub kind: ObjectKind,
    pub sha256: [u8; 32],
    pub len: u64,
    pub key: String,
    /// The entity tag recorded for it (repair and audit heads).
    pub etag: Option<String>,
}

/// What R2's `head()` showed for one object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    /// A definite absence.
    Absent,
    Present(Stored),
    /// The head itself failed: never an absence.
    Error,
}

/// An object's stored metadata as `head()` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub size: u64,
    /// `checksums.sha256` (absent when R2 holds none).
    pub sha256: Option<[u8; 32]>,
    pub etag: String,
    /// The attributes are the allowed ones ([`attrs_clean`]).
    pub clean: bool,
}

/// The stored-attribute rule an admitted object meets, versioned and
/// published in the cohort's limits record ([`limits_json`]): no customer
/// encryption key (`ssecKeyMd5`), the standard storage class, empty
/// `customMetadata` and empty `httpMetadata` (no field of
/// [`HTTP_METADATA_FIELDS`], and no other, set to any value).
pub const OBJECT_ATTRIBUTES_RULE: &str = "slop_ninja_object_attributes_v1";

/// The `httpMetadata` fields R2 stores from the unsigned standard headers a
/// presigned PUT accepts (`Content-Type`, `Content-Encoding`,
/// `Content-Disposition`, `Content-Language`, `Cache-Control`, `Expires`;
/// qualification G9, 2026-11-05). A plain PUT stores none (G12).
pub const HTTP_METADATA_FIELDS: [&str; 6] =
    ["contentType", "contentEncoding", "contentDisposition", "contentLanguage", "cacheControl", "cacheExpiry"];

/// Whether an object's stored attributes meet [`OBJECT_ATTRIBUTES_RULE`]
/// (skeptic K1; qualifications G9 and G12). R2 refuses every unsigned
/// `x-amz-*` header on a presigned PUT (SSE-C, storage class, user metadata
/// included), but stores the six standard headers in `httpMetadata`; the
/// bytes and checksum stay the committed ones, but the stored headers would
/// change how a reader decodes them (`Content-Encoding`), so an object with
/// any is not admitted: it is the miner's (`object_mismatch`, or
/// `object_replaced` once recorded), never the platform's. `http` is whether
/// `httpMetadata` holds any field at all.
pub fn attrs_clean(ssec: bool, storage_class: Option<&str>, custom: bool, http: bool) -> bool {
    !ssec && storage_class.is_none_or(|c| c == "Standard") && !custom && !http
}

/// How a head compares with the object's committed identity.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Seen {
    Absent,
    /// The committed size and SHA-256 with allowed attributes.
    Match(String),
    /// Present, the committed size, allowed attributes, but no stored
    /// checksum: never a pass (qualification G2), the platform's.
    NoChecksum,
    /// Present with the committed size and SHA-256 but a stored attribute
    /// a presigned PUT does not sign (G9: an unsigned `Content-Type` and
    /// the like): its entity tag. The bytes are the committed ones; the
    /// object is deleted (pinned to this tag, and only while still
    /// unclean) so the miner can PUT it again cleanly.
    Dirty(String),
    /// Present with other bytes: its entity tag.
    Mismatch(String),
    Error,
}

fn seen(head: &Head, sha256: &[u8; 32], len: u64) -> Seen {
    match head {
        Head::Absent => Seen::Absent,
        Head::Error => Seen::Error,
        Head::Present(st) => match st.sha256 {
            Some(d) if d == *sha256 && st.size == len && st.clean => Seen::Match(st.etag.clone()),
            Some(d) if d == *sha256 && st.size == len => Seen::Dirty(st.etag.clone()),
            None if st.size == len && st.clean => Seen::NoChecksum,
            _ => Seen::Mismatch(st.etag.clone()),
        },
    }
}

/// A presigned URL the signer minted (one reply entry of the mint).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Minted {
    pub url: String,
    /// The base64 SHA-256 the client sends as `x-amz-checksum-sha256`.
    pub checksum: String,
    pub signed_at: u64,
    pub valid_secs: u32,
}

/// The signer's reply to `entries` (`slop_ninja_r2_presign::wire`), one
/// [`Minted`] per entry in order: refused unless every entry is `Ok`, for
/// the entry's length and with a nonzero life.
pub fn minted_from_reply(reply: &[u8], entries: &[mint::Entry]) -> Result<Vec<Minted>, &'static str> {
    let decoded = mint::decode_reply(reply, entries.len()).ok_or("signer reply")?;
    let mut out = Vec::with_capacity(entries.len());
    for (e, got) in entries.iter().zip(decoded) {
        let got = got.ok_or("signer reply")?;
        if got.status != mint::Status::Ok || got.content_length != e.len || got.valid_secs == 0 {
            return Err("signer refused an entry");
        }
        out.push(Minted { url: got.url.to_owned(), checksum: got.checksum.to_owned(), signed_at: got.signed_at, valid_secs: got.valid_secs });
    }
    Ok(out)
}

/// One signer request (`slop_ninja_r2_presign::wire`): at most one header
/// and the spec's file table, so one `urls` call is one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintJob {
    pub session: String,
    pub entries: Vec<mint::Entry>,
    /// The reply's status once minted.
    pub status: u16,
    /// Answer with `header_missing` (the session and its fresh header URL
    /// beside the error) instead of the session.
    pub header_missing: bool,
}

/// The header object of a session to settle: its key, its envelope digest
/// and length, and the cohort's spec the upload Worker checks it under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderJob {
    pub session: String,
    pub key: String,
    pub sha256: [u8; 32],
    pub len: u32,
    pub spec_id: &'static str,
}

/// What the header settle's I/O found.
#[derive(Clone, Debug, PartialEq)]
pub enum HeaderSeen {
    Absent,
    /// The envelope's size and checksum with a forbidden stored attribute:
    /// its entity tag (deleted, the session keeps uploading).
    Dirty(String),
    /// Present but not the envelope's size or checksum: its entity tag
    /// (the conditional delete).
    Mismatch(String),
    /// The envelope's object, read pinned to `etag` and checked by the
    /// upload Worker: its outcome.
    Checked { etag: String, outcome: HeaderOutcome },
    /// The platform's (R2 or the upload Worker failed, no stored checksum,
    /// the pinned read refused): retried, never the miner's.
    Platform(&'static str),
}

/// The header head's verdict before the header is read: `Ok(etag)` to read
/// it pinned to, or what the settle found instead.
pub fn header_head(job: &HeaderJob, head: &Head) -> Result<String, HeaderSeen> {
    match seen(head, &job.sha256, u64::from(job.len)) {
        Seen::Match(etag) => Ok(etag),
        Seen::Absent => Err(HeaderSeen::Absent),
        Seen::Dirty(etag) => Err(HeaderSeen::Dirty(etag)),
        Seen::Mismatch(etag) => Err(HeaderSeen::Mismatch(etag)),
        Seen::NoChecksum => Err(HeaderSeen::Platform("stored checksum absent")),
        Seen::Error => Err(HeaderSeen::Platform("header head")),
    }
}

/// What applying a header settle did.
#[derive(Clone, Debug, PartialEq)]
pub enum Settled {
    /// The header is recorded (now or before): its files are next.
    Recorded,
    /// R2 holds no header object yet.
    Absent,
    /// The session ended refused; make the delete when given.
    Refused(Option<Delete>),
    /// The header object has the envelope's bytes but a forbidden stored
    /// attribute: it is deleted and the session keeps uploading (a fresh
    /// header URL is answered, as for an absent header).
    Unclean(Delete),
    /// A refusal that waits (the hotkey's latest commitment is still being
    /// logged): answered, nothing recorded.
    Wait(Box<ApiError>),
    /// The platform's failure: answered `unavailable`, nothing recorded.
    Platform(&'static str),
    /// The session no longer settles (ended, closing, recorded elsewhere).
    Ignored,
}

/// One step of a session operation: answer now, or the I/O it needs first.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Reply(Reply),
    Mint(MintJob),
    Header(HeaderJob),
    Heads(Vec<Probe>),
}

/// Who records a head: the client's request, the R2 event and the alarm
/// sweep record presence only while the window admits; once `closing` is
/// persisted only the close sweep does; repair and audit heads judge
/// objects already recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorder {
    Upload,
    Close,
    Recorded,
}

/// A conditional delete of an object found unclean or wrong: made only
/// while a fresh head still shows entity tag `etag` AND the object is not
/// a clean copy of the committed `sha256` and `len`. A clean re-upload of
/// the same bytes has the same tag (R2's single-part tag is the bytes'
/// MD5), so the tag alone cannot tell it from the dirty copy (review
/// finding: a delete aimed at a dirty object must never remove its clean
/// re-upload).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delete {
    pub key: String,
    pub etag: String,
    pub sha256: [u8; 32],
    pub len: u64,
}

impl Delete {
    /// Whether a fresh `head` of the key still allows this delete.
    pub fn allows(&self, head: &Head) -> bool {
        match head {
            Head::Present(st) => st.etag == self.etag && !matches!(seen(head, &self.sha256, self.len), Seen::Match(_)),
            _ => false,
        }
    }
}

/// What applying heads did: the conditional deletes to make ([`Delete`]),
/// and whether a head was the platform's (an error, no stored checksum).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub deletes: Vec<Delete>,
    pub platform: bool,
}

fn refusal_error(code: ErrorCode, message: &str) -> ApiError {
    ApiError::new(code, message)
}

/// `POST /admission/v2/sessions`: opens (or returns) the session, then its
/// header URL is minted ([`MintJob`], [`finish_mint`]).
pub fn create(sql: &impl Sql, chain: &ChainBinding, req: &Signed, now_ms: u64) -> Step {
    create_inner(sql, chain, req, now_ms).unwrap_or_else(|e| Step::Reply(reply_error(&e)))
}

fn create_inner(sql: &impl Sql, chain: &ChainBinding, req: &Signed, now_ms: u64) -> A<Step> {
    let (signer, content) = signer(sql, chain, req)?;
    let cursor = cursor(sql)?.unwrap_or(0);
    let w = window(sql)?;
    let body = req
        .body
        .as_deref()
        .and_then(|b| hex::decode(b).ok())
        .unwrap_or_default();
    // The key-holding side hashes the record it decodes itself (one
    // `SubmissionUpload` record, a small record): a forwarded creation must
    // carry its body and the upload Worker's digest of it, and both must be
    // the signed content digest.
    let digest: [u8; 32] = sha2::Sha256::digest(&body).into();
    if req.body.is_none() || req.body_sha256.is_none() || digest != content {
        return Err(ApiError::new(
            ErrorCode::ContentDigestMismatch,
            "the body does not match x-sn-content-sha256",
        ));
    }
    let Ok(upload) = SubmissionUpload::from_bytes(&body) else {
        let e = ApiError::new(ErrorCode::BadRequest, "the body is not a submission-upload record");
        record_refusal(sql, cursor, &signer, w.as_ref(), None, &e)?;
        return Err(e);
    };
    let id = upload
        .digest()
        .map_err(|_| ApiError::new(ErrorCode::Internal, "digest"))?
        .hex();
    // The same envelope again (a retried creation, a lost answer) is
    // answered with its session before any admission check: the checks ran
    // when it was created. While its header is still to upload it gets a
    // fresh header URL.
    if let Some(existing) = session_row(sql, &id)?
        && existing.hotkey == signer
    {
        if existing.state == SessionState::Uploading
            && existing.files.is_none()
            && w.as_ref().is_some_and(|w| w.admitting() && w.cohort == existing.cohort)
        {
            return header_mint(&existing, 200, false);
        }
        return Ok(Step::Reply(reply_ok(200, &session_json(sql, &existing, cursor, Vec::new())?)));
    }
    let fail = |e: ApiError| -> A<Step> {
        record_refusal(sql, cursor, &signer, w.as_ref(), Some(&upload), &e)?;
        Err(e)
    };
    let w = match admission_checks(sql, chain, w.as_ref(), &signer, &upload, req.commitment_block) {
        Ok(w) => w,
        Err(e) => return fail(e),
    };
    if let Err(e) = one_artifact(sql, &w, &signer, &id, &upload, now_ms) {
        return fail(e);
    }
    let bytes = upload
        .to_bytes()
        .map_err(|_| ApiError::new(ErrorCode::Internal, "upload bytes"))?;
    sql.exec(
        "INSERT INTO adm_sessions (session, seq, cohort, hotkey, upload, commitment, content, header_sha256, header_len,
            state, created_ms, updated_ms)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'uploading', ?, ?)",
        &[
            s(&id),
            i(w.seq),
            bind_u64(w.cohort)?,
            s(hex::encode(signer.0)),
            s(hex::encode(bytes)),
            s(upload.commitment.hex()),
            s(upload.opening.content.hex()),
            s(upload.header_sha256.hex()),
            i(i64::from(upload.header_len)),
            bind_u64(now_ms)?,
            bind_u64(now_ms)?,
        ],
    )?;
    sql.exec(
        "INSERT INTO adm_objects (session, kind, sha256, len) VALUES (?, 'h', ?, ?)",
        &[s(&id), s(upload.header_sha256.hex()), i(i64::from(upload.header_len))],
    )?;
    let row = session_row(sql, &id)?.ok_or_else(|| ApiError::new(ErrorCode::Internal, "session"))?;
    header_mint(&row, 201, false)
}

/// The URL validity of a session's objects under the upload URL policy,
/// for its whole committed size (R2 checks expiry when a PUT starts, so no
/// transfer rate enters it).
fn valid_secs(r: &Row) -> A<u32> {
    policy::valid_secs(committed_len(r)).map_err(|_| ApiError::new(ErrorCode::Internal, "url policy"))
}

fn header_mint(r: &Row, status: u16, header_missing: bool) -> A<Step> {
    let valid = valid_secs(r)?;
    Ok(Step::Mint(MintJob {
        session: r.session.clone(),
        entries: vec![mint::Entry {
            op: mint::Op::Header,
            cohort: r.cohort,
            commitment: r.upload.commitment.0,
            sha256: r.upload.header_sha256.0,
            len: u64::from(r.upload.header_len),
            valid_secs: valid,
        }],
        status,
        header_missing,
    }))
}

/// The mint of the given objects of a session (each distinct object once).
fn files_mint(r: &Row, objs: &[&Obj], status: u16) -> A<Step> {
    let valid = valid_secs(r)?;
    let entries = objs
        .iter()
        .map(|o| mint::Entry {
            op: match o.kind {
                ObjectKind::Header => mint::Op::Header,
                ObjectKind::File(_) => mint::Op::File,
            },
            cohort: r.cohort,
            commitment: r.upload.commitment.0,
            sha256: o.sha256,
            len: o.len,
            valid_secs: valid,
        })
        .collect();
    Ok(Step::Mint(MintJob { session: r.session.clone(), entries, status, header_missing: false }))
}

/// Records a mint's outcome and answers. A mint the signer could not make
/// (unreachable, a 5xx, a reply that does not decode, an entry it refused)
/// is the platform's: the cohort's upload path takes the persisted fault
/// `signer_unavailable`, which cancels any earlier success and defers the
/// close until a later mint succeeds. A success records the session's mint
/// aggregate (its count and latest URL expiry; never a row per mint) and
/// clears the fault.
pub fn finish_mint(sql: &impl Sql, job: &MintJob, minted: Result<Vec<Minted>, &str>, now_ms: u64) -> R<Reply> {
    let urls = match minted {
        Ok(u) if u.len() == job.entries.len() => u,
        Ok(_) => return mint_failed(sql, "signer reply", now_ms),
        Err(code) => return mint_failed(sql, code, now_ms),
    };
    mint_ok(sql, now_ms)?;
    let until = urls
        .iter()
        .map(|u| u.signed_at.saturating_add(u64::from(u.valid_secs)).saturating_mul(1000))
        .max()
        .unwrap_or(0);
    sql.exec(
        "UPDATE adm_sessions SET mints = mints + ?, url_until_ms = MAX(url_until_ms, ?) WHERE session = ?",
        &[i(urls.len() as i64), bind_u64(until)?, s(&job.session)],
    )?;
    let Some(r) = session_row(sql, &job.session)? else {
        return Ok(reply_error(&ApiError::new(ErrorCode::NotFound, "unknown session")));
    };
    // The window may have closed while the signer minted (`closing` is
    // persisted by another invocation): an uploading session's URLs could
    // no longer count, so none is handed out. The signer worked (`mint_ok`
    // above). A repair's URLs (a session past uploading) are valid in any
    // phase.
    let repairing = matches!(r.state, SessionState::Verifying | SessionState::Verified | SessionState::Admitted);
    if !repairing && !(r.state == SessionState::Uploading && window_for(sql, r.cohort)?.is_some_and(|w| w.admitting())) {
        if r.state == SessionState::Uploading {
            return Ok(reply_error(&after_cutoff()));
        }
        // Ended meanwhile (the close sweep refused it): the session, no URL.
        return Ok(reply_ok(200, &session_json(sql, &r, cursor(sql)?.unwrap_or(0), Vec::new())?));
    }
    let uploads = job
        .entries
        .iter()
        .zip(urls)
        .map(|(e, u)| Upload {
            kind: if e.op == mint::Op::Header { "header".into() } else { "file".into() },
            sha256: hex::encode(e.sha256),
            len: e.len,
            url: u.url,
            content_length: e.len.to_string(),
            if_none_match: slop_ninja_r2_presign::presign::IF_NONE_MATCH.into(),
            checksum_sha256: u.checksum,
            signed_at: u.signed_at,
            valid_secs: u.valid_secs,
        })
        .collect();
    let session = session_json(sql, &r, cursor(sql)?.unwrap_or(0), uploads)?;
    if job.header_missing {
        let mut e = ApiError::new(ErrorCode::HeaderMissing, "R2 holds no header object yet: PUT it with the fresh URL");
        e.retry_after = Some(1);
        let mut reply = reply_error(&e);
        if let Value::Object(m) = &mut reply.body {
            m.insert("session".into(), serde_json::to_value(&session).unwrap_or(Value::Null));
        }
        return Ok(reply);
    }
    Ok(reply_ok(job.status, &session))
}

fn mint_failed(sql: &impl Sql, code: &str, now_ms: u64) -> R<Reply> {
    platform_fault(sql, Fault::Signer, code, now_ms)?;
    let mut e = ApiError::new(ErrorCode::Unavailable, "the URL signer is unavailable");
    e.retry_after = Some(TICK_MS / 1000);
    Ok(reply_error(&e))
}

/// Whether a forwarded message's route fields are the ones its signed
/// method and path name, for interface operation `op` (the key-holding
/// side re-derives what the upload Worker parsed, so a defect there cannot
/// apply one signed request to another session).
pub fn forward_matches(op: &str, f: &Forward) -> bool {
    use slop_ninja_admission_wire::Route;
    use slop_ninja_admission_wire::internal::op as o;
    let Some(route) = Route::parse(&f.signed.method, &f.signed.path) else {
        return false;
    };
    match (op, route) {
        (o::CREATE, Route::CreateSession) => true,
        (o::READ, Route::ReadSession { session })
        | (o::URLS, Route::Urls { session })
        | (o::COMPLETE, Route::Complete { session })
        | (o::REPAIR, Route::Repair { session }) => session == f.session,
        (o::HOTKEY, Route::ReadHotkey { cohort, hotkey }) => cohort == f.cohort && hotkey == f.hotkey,
        _ => false,
    }
}

/// The refusal of a forwarded message whose fields are not its signed
/// request's ([`forward_matches`]).
pub fn forward_mismatch() -> Reply {
    reply_error(&ApiError::new(
        ErrorCode::BadRequest,
        "the forwarded fields do not match the signed request",
    ))
}

/// The session `id`, owned by `signer` (session ownership: only the
/// session's hotkey may act on it).
fn owned(sql: &impl Sql, signer: &AccountId, id: &str) -> A<Row> {
    let Some(r) = session_row(sql, id)? else {
        return Err(ApiError::new(ErrorCode::NotFound, "unknown session"));
    };
    if r.hotkey != *signer {
        return Err(ApiError::new(
            ErrorCode::Forbidden,
            "the signer does not own the session",
        ));
    }
    Ok(r)
}

fn after_cutoff() -> ApiError {
    ApiError::new(ErrorCode::AfterCutoff, "admission has closed for this cohort")
}

/// `GET /admission/v2/sessions/{session}`.
pub fn read(sql: &impl Sql, chain: &ChainBinding, req: &Signed, id: &str) -> Reply {
    let go = || -> A<Reply> {
        let (signer, _) = signer(sql, chain, req)?;
        let r = owned(sql, &signer, id)?;
        Ok(reply_ok(200, &session_json(sql, &r, cursor(sql)?.unwrap_or(0), Vec::new())?))
    };
    go().unwrap_or_else(|e| reply_error(&e))
}

fn header_job(r: &Row, w: &Window) -> HeaderJob {
    HeaderJob {
        session: r.session.clone(),
        key: key_of(r, ObjectKind::Header),
        sha256: r.upload.header_sha256.0,
        len: r.upload.header_len,
        spec_id: w.bounds.spec_id,
    }
}

fn probe(r: &Row, o: &Obj) -> Probe {
    Probe {
        session: r.session.clone(),
        kind: o.kind,
        sha256: o.sha256,
        len: o.len,
        key: key_of(r, o.kind),
        etag: o.etag.clone(),
    }
}

/// An uploading session of an admitting window, owned by the signer: the
/// shared start of `urls` and `complete`.
fn uploading(sql: &impl Sql, chain: &ChainBinding, req: &Signed, id: &str) -> A<Result<(Row, Window), Reply>> {
    let (signer, _) = signer(sql, chain, req)?;
    let r = owned(sql, &signer, id)?;
    let w = window_for(sql, r.cohort)?.ok_or_else(|| ApiError::new(ErrorCode::NotFound, "unknown cohort"))?;
    if !w.admitting() {
        return Err(after_cutoff());
    }
    match r.state {
        SessionState::Uploading => Ok(Ok((r, w))),
        SessionState::Verifying | SessionState::Verified => {
            Ok(Err(reply_ok(202, &session_json(sql, &r, cursor(sql)?.unwrap_or(0), Vec::new())?)))
        }
        _ => Err(ApiError::new(
            ErrorCode::SessionNotUploading,
            "the session has ended and takes no URL request or completion",
        )),
    }
}

/// `POST /admission/v2/sessions/{session}/urls`: settles the header first
/// when it is not recorded ([`Step::Header`]), then mints a fresh URL for
/// every object not yet recorded present. Every call mints anew; only
/// protocol state refuses.
pub fn urls(sql: &impl Sql, chain: &ChainBinding, req: &Signed, id: &str) -> Step {
    let go = || -> A<Step> {
        let (r, w) = match uploading(sql, chain, req, id)? {
            Ok(v) => v,
            Err(reply) => return Ok(Step::Reply(reply)),
        };
        if r.files.is_none() {
            return Ok(Step::Header(header_job(&r, &w)));
        }
        let objs = objects_of(sql, id)?;
        let missing: Vec<&Obj> = objs.iter().filter(|o| !o.recorded()).collect();
        if missing.is_empty() {
            return Ok(Step::Reply(reply_ok(200, &session_json(sql, &r, cursor(sql)?.unwrap_or(0), Vec::new())?)));
        }
        files_mint(&r, &missing, 200)
    };
    go().unwrap_or_else(|e| Step::Reply(reply_error(&e)))
}

/// The answer of `urls` after its header settle: the files' URLs once the
/// header is recorded, `header_missing` with a fresh header URL while R2
/// holds none, the ended session after a refusal.
pub fn urls_after(sql: &impl Sql, chain: &ChainBinding, req: &Signed, id: &str, settled: &Settled) -> Step {
    let go = || -> A<Step> {
        match settled {
            Settled::Recorded => Ok(urls(sql, chain, req, id)),
            Settled::Absent | Settled::Unclean(_) => {
                let (r, _) = match uploading(sql, chain, req, id)? {
                    Ok(v) => v,
                    Err(reply) => return Ok(Step::Reply(reply)),
                };
                header_mint(&r, 409, true)
            }
            other => settled_reply(sql, id, other).map(Step::Reply),
        }
    };
    go().unwrap_or_else(|e| Step::Reply(reply_error(&e)))
}

/// The answer to a settle that recorded nothing more: the session (ended,
/// or recorded elsewhere), the waiting refusal, or `unavailable`.
fn settled_reply(sql: &impl Sql, id: &str, settled: &Settled) -> A<Reply> {
    match settled {
        Settled::Wait(e) => Err((**e).clone()),
        Settled::Platform(_) => {
            let mut e = ApiError::new(ErrorCode::Unavailable, "the header could not be checked now");
            e.retry_after = Some(TICK_MS / 1000);
            Err(e)
        }
        _ => {
            let r = session_row(sql, id)?.ok_or_else(|| ApiError::new(ErrorCode::NotFound, "unknown session"))?;
            Ok(reply_ok(200, &session_json(sql, &r, cursor(sql)?.unwrap_or(0), Vec::new())?))
        }
    }
}

/// `POST /admission/v2/sessions/{session}/complete`: settles the header if
/// needed, then heads every file not yet recorded ([`Step::Heads`]); the
/// answer is [`complete_reply`].
pub fn complete(sql: &impl Sql, chain: &ChainBinding, req: &Signed, id: &str) -> Step {
    let go = || -> A<Step> {
        let (r, w) = match uploading(sql, chain, req, id)? {
            Ok(v) => v,
            Err(reply) => return Ok(Step::Reply(reply)),
        };
        if r.files.is_none() {
            return Ok(Step::Header(header_job(&r, &w)));
        }
        let probes: Vec<Probe> = objects_of(sql, id)?.iter().filter(|o| !o.recorded()).map(|o| probe(&r, o)).collect();
        if probes.is_empty() {
            return complete_reply(sql, id).map(Step::Reply);
        }
        Ok(Step::Heads(probes))
    };
    go().unwrap_or_else(|e| Step::Reply(reply_error(&e)))
}

/// The answer of `complete` once its heads are applied: 202 when the
/// session verifies, else 409 `objects_missing` with the missing digests.
pub fn complete_reply(sql: &impl Sql, id: &str) -> A<Reply> {
    let r = session_row(sql, id)?.ok_or_else(|| ApiError::new(ErrorCode::NotFound, "unknown session"))?;
    let cursor = cursor(sql)?.unwrap_or(0);
    match r.state {
        SessionState::Verifying | SessionState::Verified => Ok(reply_ok(202, &session_json(sql, &r, cursor, Vec::new())?)),
        SessionState::Uploading => {
            let mut missing: Vec<String> =
                objects_of(sql, id)?.iter().filter(|o| !o.recorded()).map(|o| hex::encode(o.sha256)).collect();
            if r.files.is_none() && missing.is_empty() {
                missing.push(r.upload.header_sha256.hex());
            }
            let mut e = ApiError::new(ErrorCode::ObjectsMissing, "completion before every object is in R2 as committed")
                .with(Detail { missing: Some(missing), ..Default::default() });
            e.retry_after = Some(1);
            Err(e)
        }
        _ => Ok(reply_ok(200, &session_json(sql, &r, cursor, Vec::new())?)),
    }
}

/// The answer of `complete` after its header settle.
pub fn complete_after(sql: &impl Sql, chain: &ChainBinding, req: &Signed, id: &str, settled: &Settled) -> Step {
    match settled {
        Settled::Recorded => complete(sql, chain, req, id),
        Settled::Absent | Settled::Unclean(_) => {
            complete_reply(sql, id).map_or_else(|e| Step::Reply(reply_error(&e)), Step::Reply)
        }
        other => settled_reply(sql, id, other).map_or_else(|e| Step::Reply(reply_error(&e)), Step::Reply),
    }
}

/// `POST /admission/v2/sessions/{session}/repair`: for a recorded object
/// that went missing (a platform fault, never the miner's), a fresh URL for
/// exactly that recorded key, digest and length, so R2 again fixes the
/// content. Valid in any phase while the object is still needed (before its
/// publication is recorded); the recorded objects are headed first
/// ([`Step::Heads`], then [`repair_after`]).
pub fn repair(sql: &impl Sql, chain: &ChainBinding, req: &Signed, id: &str) -> Step {
    let go = || -> A<Step> {
        let (signer, _) = signer(sql, chain, req)?;
        let r = owned(sql, &signer, id)?;
        if !needed(sql, &r)? {
            return Err(not_needed());
        }
        let probes: Vec<Probe> = objects_of(sql, id)?.iter().filter(|o| o.etag.is_some()).map(|o| probe(&r, o)).collect();
        Ok(Step::Heads(probes))
    };
    go().unwrap_or_else(|e| Step::Reply(reply_error(&e)))
}

/// Whether a session's recorded objects are still needed: verifying,
/// verified, or admitted until its cohort is closed and its publication
/// recorded.
fn needed(sql: &impl Sql, r: &Row) -> R<bool> {
    Ok(match r.state {
        SessionState::Verifying | SessionState::Verified => true,
        // An admitted entrant's objects are what its scoring lanes read
        // (the files manifest names the admission bucket) and what
        // publication copies: needed until the cohort is closed AND its
        // publication is recorded (the clean-up's own condition,
        // `cleanup_enqueue`), or until the cohort is abandoned.
        SessionState::Admitted => {
            let rows = sql.exec(
                "SELECT (SELECT phase FROM cohorts WHERE seq = ?), (SELECT state FROM adm_cohorts WHERE seq = ?)",
                &[i(r.seq), i(r.seq)],
            )?;
            let row = rows.first();
            let closed = row.and_then(|x| opt_text(x, 0)).as_deref() == Some("closed");
            let abandoned = row.and_then(|x| opt_text(x, 1)).as_deref() == Some("abandoned");
            !abandoned && !(closed && r.published)
        }
        _ => false,
    })
}

fn not_needed() -> ApiError {
    ApiError::new(ErrorCode::NotNeeded, "no recorded object of this session is missing or still needed")
}

/// The answer of `repair` once its heads are applied: URLs for the
/// recorded objects found absent, else `not_needed`.
pub fn repair_after(sql: &impl Sql, id: &str) -> Step {
    let go = || -> A<Step> {
        let r = session_row(sql, id)?.ok_or_else(|| ApiError::new(ErrorCode::NotFound, "unknown session"))?;
        if !needed(sql, &r)? {
            return Err(not_needed());
        }
        let objs = objects_of(sql, id)?;
        let missing: Vec<&Obj> = objs.iter().filter(|o| o.missing).collect();
        if missing.is_empty() {
            return Err(not_needed());
        }
        files_mint(&r, &missing, 200)
    };
    go().unwrap_or_else(|e| Step::Reply(reply_error(&e)))
}

// ----------------------------------------------------------- the header

/// Applies a header settle (design A1-A3): R2 held the envelope's header
/// (size, stored SHA-256, allowed attributes), the upload Worker decoded and
/// checked it, its content digest is recomputed here from the returned
/// table and must be the outcome's, the committed one, under the cohort's
/// contract, and `admit_header` passes against the logged commitment. Then
/// the header and its file table are recorded and each distinct file gets
/// its object. Runs only for an uploading session while the window admits.
pub fn apply_header(sql: &impl Sql, chain: &ChainBinding, job: &HeaderJob, seen: HeaderSeen, now_ms: u64) -> R<Settled> {
    let Some(r) = session_row(sql, &job.session)? else {
        return Ok(Settled::Ignored);
    };
    let Some(w) = window_for(sql, r.cohort)? else {
        return Ok(Settled::Ignored);
    };
    if r.state != SessionState::Uploading || !w.admitting() {
        return Ok(Settled::Ignored);
    }
    if r.files.is_some() {
        return Ok(Settled::Recorded);
    }
    let refuse = |e: ApiError, delete: Option<Delete>| -> R<Settled> {
        set_state(sql, &r.session, SessionState::Refused, Some(&e), now_ms)?;
        Ok(Settled::Refused(delete))
    };
    let delete = |etag: String| Delete { key: job.key.clone(), etag, sha256: job.sha256, len: u64::from(job.len) };
    let (etag, outcome) = match seen {
        HeaderSeen::Absent => return Ok(Settled::Absent),
        HeaderSeen::Platform(code) => {
            // Persisted (review finding): the close waits for a later
            // settle the upload Worker answers, or a passing link probe.
            platform_fault(sql, Fault::Front, code, now_ms)?;
            return Ok(Settled::Platform(code));
        }
        // The committed bytes with a stray stored attribute (one unsigned
        // `Content-Type` from a third-party client): deleted like an
        // unclean file, so the miner PUTs it again under a fresh URL; the
        // session and its commitment stay (slop_ninja_object_attributes_v1).
        HeaderSeen::Dirty(etag) => return Ok(Settled::Unclean(delete(etag))),
        HeaderSeen::Mismatch(etag) => {
            return refuse(
                refusal_error(
                    ErrorCode::ObjectMismatch,
                    "R2's stored size or checksum of the header is not the envelope's",
                ),
                Some(delete(etag)),
            );
        }
        HeaderSeen::Checked { etag, outcome } => (etag, outcome),
    };
    if outcome.schema != slop_ninja_artifact_check::admission::HEADER_OUTCOME_SCHEMA {
        platform_fault(sql, Fault::Front, "header outcome schema", now_ms)?;
        return Ok(Settled::Platform("header outcome schema"));
    }
    let (contract, claimed, files) = match outcome.result {
        HeaderResult::Checked { contract, content, files } => (contract, content, files),
        HeaderResult::Invalid { reason } => {
            // The upload Worker answered a well-formed refusal: the front works.
            front_ok(sql, now_ms)?;
            return refuse(
                refusal_error(ErrorCode::ArtifactInvalid, "the header bytes do not decode or validate")
                    .with(Detail { reason: Some(reason), ..Default::default() }),
                None,
            );
        }
        HeaderResult::Check { code } => {
            front_ok(sql, now_ms)?;
            return refuse(
                refusal_error(ErrorCode::CheckFailed, "an artifact check refused it")
                    .with(Detail { check: Some(code), ..Default::default() }),
                None,
            );
        }
    };
    // Skeptic K6: the table the key-free upload Worker returned is a small
    // record whose content digest is recomputed here; a table that does not
    // rebuild, recompute to the outcome's own digest or pass the spec's
    // header-only checks is the platform's fault, never the miner's. Each
    // is persisted like an unanswered settle (review round 2: version skew
    // between the two Workers, or a faulty upload Worker, must defer the
    // close, never end the miner's upload), and only a fully consistent
    // outcome clears the front fault.
    let inconsistent = |code: &'static str| -> R<Settled> {
        platform_fault(sql, Fault::Front, code, now_ms)?;
        Ok(Settled::Platform(code))
    };
    let Some((header, content)) = rebuild_header(&contract, &files) else {
        return inconsistent("header outcome table");
    };
    if content.hex() != claimed {
        return inconsistent("header outcome content");
    }
    let version = Version::from_id(w.bounds.spec_id).ok_or(EvalError::State("spec"))?;
    if slop_ninja_artifact_check::mapping::check_header_for(&header, version).is_err()
        || files.iter().enumerate().any(|(n, a)| files.iter().skip(n + 1).any(|b| b.sha256 == a.sha256 && b.len != a.len))
    {
        return inconsistent("header outcome checks");
    }
    // The upload Worker answered a consistent outcome: the front works.
    front_ok(sql, now_ms)?;
    if contract != w.output_contract {
        return refuse(
            refusal_error(ErrorCode::ContractMismatch, "the header's contract is not the cohort's output contract"),
            None,
        );
    }
    if content != r.upload.opening.content {
        return refuse(
            refusal_error(ErrorCode::ContentMismatch, "the header's derived content is not the committed content"),
            None,
        );
    }
    let (_, entry) = match check_commitment(sql, chain, &w, &r.upload, CheckAt::Verify) {
        Ok(v) => v,
        // The hotkey's latest commitment is still being logged: retryable,
        // never a refusal.
        Err(e) if e.code == ErrorCode::CommitmentNotFound && commitments::unresolved(sql, &r.hotkey)? => {
            return Ok(Settled::Wait(Box::new(e)));
        }
        Err(e) => return refuse(e, None),
    };
    let (Some(uid), Some(registration_block)) = (entry.uid, entry.registration_block) else {
        return refuse(refusal_error(ErrorCode::NotRegistered, "not registered"), None);
    };
    let fc = FinalizedCommitment {
        chain,
        committer: NeuronIdentity { uid, hotkey: r.hotkey, registration_block },
        block: entry.block,
        extrinsic_index: entry.extrinsic,
        payload: &entry.payload,
    };
    let verified = VerifiedHeader { sha256: Digest(job.sha256), len: job.len, header, content };
    let pos = match r.upload.admit_header(w.cohort, &fc, &verified) {
        Ok(p) => p,
        Err(_) => {
            return refuse(
                refusal_error(ErrorCode::ContentMismatch, "the header does not open the logged commitment"),
                None,
            );
        }
    };
    let model = files.iter().find(|f| f.name == spec::GRAPH_FILE).map(|f| f.sha256.clone());
    sql.exec(
        "UPDATE adm_sessions SET files = ?, model_sha256 = ?, pos_block = ?, pos_extrinsic = ?, updated_ms = ?
         WHERE session = ? AND files IS NULL",
        &[
            s(serde_json::to_string(&files).map_err(|_| EvalError::State("files"))?),
            model.map_or(SqlValue::Null, s),
            bind_u64(pos.commitment_block)?,
            i(i64::from(pos.extrinsic_index)),
            bind_u64(now_ms)?,
            s(&r.session),
        ],
    )?;
    sql.exec(
        "UPDATE adm_objects SET etag = ?, missing = 0 WHERE session = ? AND kind = 'h'",
        &[s(&etag), s(&r.session)],
    )?;
    for f in &files {
        sql.exec(
            "INSERT OR IGNORE INTO adm_objects (session, kind, sha256, len) VALUES (?, 'f', ?, ?)",
            &[s(&r.session), s(&f.sha256), bind_u64(f.len)?],
        )?;
    }
    maybe_complete(sql, &r.session, now_ms)?;
    Ok(Settled::Recorded)
}

// ----------------------------------------------------------- the objects

/// Applies heads (design A4). An unrecorded object present with its
/// committed size and SHA-256 and allowed attributes is recorded with its
/// entity tag; one present otherwise is deleted (it may be PUT again). A
/// recorded object found absent needs repair (the platform's); one present
/// with its digest but another entity tag or a forbidden attribute refuses
/// its session `object_replaced` (the miner's, never a cohort pause). A
/// head that errored, or showed no stored checksum, records nothing.
///
/// Presence is recorded only by the recorder entitled to it (skeptic K7):
/// [`Recorder::Upload`] for an uploading session while its window admits,
/// [`Recorder::Close`] for one while its cohort is closing (the close
/// sweep, which marks each object headed), [`Recorder::Recorded`] for the
/// recorded objects of a session still needing them (repair and audit).
pub fn apply_heads(sql: &impl Sql, heads: &[(Probe, Head)], who: Recorder, now_ms: u64) -> R<Applied> {
    let mut out = Applied::default();
    for (p, head) in heads {
        let Some(r) = session_row(sql, &p.session)? else { continue };
        let Some(w) = window_for(sql, r.cohort)? else { continue };
        let Some(o) = objects_of(sql, &p.session)?.into_iter().find(|o| o.kind == p.kind && o.sha256 == p.sha256) else {
            continue;
        };
        let entitled = match who {
            Recorder::Upload => r.state == SessionState::Uploading && w.admitting() && o.etag.is_none(),
            Recorder::Close => r.state == SessionState::Uploading && w.state == "closing" && o.etag.is_none(),
            Recorder::Recorded => o.etag.is_some() && needed(sql, &r)?,
        };
        if !entitled {
            continue;
        }
        let kind = kind_code(o.kind);
        let sha = hex::encode(o.sha256);
        let exec = |q: &str, a: &[SqlValue]| -> R<()> {
            sql.exec(q, a)?;
            Ok(())
        };
        let mark = || -> R<()> {
            if who == Recorder::Close {
                exec(
                    "UPDATE adm_objects SET close_headed = 1 WHERE session = ? AND kind = ? AND sha256 = ?",
                    &[s(&r.session), s(kind), s(&sha)],
                )?;
            }
            Ok(())
        };
        let delete = |etag: String| Delete { key: p.key.clone(), etag, sha256: o.sha256, len: o.len };
        let set_missing = || -> R<()> {
            exec(
                "UPDATE adm_objects SET missing = 1 WHERE session = ? AND kind = ? AND sha256 = ?",
                &[s(&r.session), s(kind), s(&sha)],
            )?;
            hold_for_repair(sql, &r, now_ms)
        };
        let replaced = || -> R<()> {
            let e = refusal_error(
                ErrorCode::ObjectReplaced,
                "a recorded object is present with other bytes or another entity tag",
            );
            set_state(sql, &r.session, SessionState::Refused, Some(&e), now_ms)?;
            exec("UPDATE adm_sessions SET stage = NULL WHERE session = ?", &[s(&r.session)])?;
            refuse_verdict(sql, &r, "object_replaced", now_ms)
        };
        match (seen(head, &o.sha256, o.len), &o.etag) {
            (Seen::Error, _) => {
                out.platform = true;
                if who == Recorder::Close {
                    // A head that errored defers the close: only a definite
                    // absence counts as missing.
                    return Ok(out);
                }
            }
            (Seen::NoChecksum, _) => out.platform = true,
            (Seen::Absent, None) => mark()?,
            (Seen::Match(etag), None) => {
                exec(
                    "UPDATE adm_objects SET etag = ?, missing = 0 WHERE session = ? AND kind = ? AND sha256 = ?",
                    &[s(&etag), s(&r.session), s(kind), s(&sha)],
                )?;
                mark()?;
                maybe_complete(sql, &r.session, now_ms)?;
            }
            (Seen::Dirty(etag) | Seen::Mismatch(etag), None) => {
                mark()?;
                out.deletes.push(delete(etag));
            }
            (Seen::Absent, Some(_)) => set_missing()?,
            (Seen::Match(etag), Some(recorded)) if etag == *recorded => {
                if o.missing {
                    exec(
                        "UPDATE adm_objects SET missing = 0 WHERE session = ? AND kind = ? AND sha256 = ?",
                        &[s(&r.session), s(kind), s(&sha)],
                    )?;
                    repaired(sql, &r.session, now_ms)?;
                }
            }
            // The committed bytes with a forbidden stored attribute (a
            // repair PUT carrying an unsigned `Content-Type`, G9), in any
            // state: deleted (only while still unclean) and listed missing,
            // so the miner can PUT it again cleanly (skeptic K1). Never
            // `object_replaced`: the bytes are the committed ones.
            (Seen::Dirty(etag), Some(_)) => {
                out.deletes.push(delete(etag));
                set_missing()?;
            }
            // Other bytes, or a clean object with another entity tag: the
            // miner's before admission (`object_replaced`, never a cohort
            // pause). After admission the roster is frozen and the files
            // manifest pins the recorded tag, so the object is deleted and
            // listed missing: a re-PUT of the committed bytes restores the
            // recorded tag (R2's single-part tag is the bytes' MD5).
            (Seen::Match(etag) | Seen::Mismatch(etag), Some(_)) => {
                if matches!(r.state, SessionState::Verifying | SessionState::Verified) {
                    replaced()?;
                } else {
                    out.deletes.push(delete(etag));
                    set_missing()?;
                }
            }
        }
    }
    Ok(out)
}

/// A recorded object of `r` was found missing (absent, or deleted as
/// unclean): a verifying session whose run or audit met it waits for the
/// repair (stage `repair`, its claim released); an admitted session's
/// scoring lanes and publication hold (stage `repair`) until
/// [`repaired`] clears it.
fn hold_for_repair(sql: &impl Sql, r: &Row, now_ms: u64) -> R<()> {
    // A real loss: the read path's earlier intact audits are not its fault.
    sql.exec("UPDATE adm_sessions SET read_faults = 0 WHERE session = ?", &[s(&r.session)])?;
    match r.state {
        SessionState::Verifying if matches!(r.stage.as_deref(), Some("verdict" | "audit")) => {
            sql.exec(
                "UPDATE adm_sessions SET stage = 'repair', updated_ms = ? WHERE session = ?",
                &[bind_u64(now_ms)?, s(&r.session)],
            )?;
            release_claim(sql, r)?;
        }
        SessionState::Admitted => {
            sql.exec(
                "UPDATE adm_sessions SET stage = 'repair', updated_ms = ? WHERE session = ? AND state = 'admitted'",
                &[bind_u64(now_ms)?, s(&r.session)],
            )?;
        }
        _ => {}
    }
    Ok(())
}

/// A pending verdict's claim released (its run waits for the objects).
fn release_claim(sql: &impl Sql, r: &Row) -> R<()> {
    sql.exec(
        "UPDATE adm_verdicts SET claim_until_ms = 0 WHERE hotkey = ? AND commitment = ? AND state = 'pending'",
        &[s(hex::encode(r.hotkey.0)), s(r.upload.commitment.hex())],
    )?;
    Ok(())
}

/// A pending verdict ended with the session's own refusal `code`.
fn refuse_verdict(sql: &impl Sql, r: &Row, code: &str, now_ms: u64) -> R<()> {
    sql.exec(
        "UPDATE adm_verdicts SET state = 'refused', code = ?, claim_until_ms = 0, updated_ms = ?
         WHERE hotkey = ? AND commitment = ? AND state = 'pending'",
        &[s(code), bind_u64(now_ms)?, s(hex::encode(r.hotkey.0)), s(r.upload.commitment.hex())],
    )?;
    Ok(())
}

/// Every missing object of a session in repair is back: its verdict run
/// resumes.
fn repaired(sql: &impl Sql, id: &str, now_ms: u64) -> R<()> {
    let left = count(sql, "SELECT COUNT(*) FROM adm_objects WHERE session = ? AND missing = 1", &[s(id)])?;
    if left == 0 {
        sql.exec(
            "UPDATE adm_sessions SET stage = 'verdict', updated_ms = ? WHERE session = ? AND state = 'verifying' AND stage = 'repair'",
            &[bind_u64(now_ms)?, s(id)],
        )?;
        // An admitted entrant's held lanes and publication resume.
        sql.exec(
            "UPDATE adm_sessions SET stage = NULL, updated_ms = ? WHERE session = ? AND state = 'admitted' AND stage = 'repair'",
            &[bind_u64(now_ms)?, s(id)],
        )?;
    }
    Ok(())
}

/// The profile id a checked header loads under: the spec and its graph
/// class (a class fixes its file length).
fn profile_of(header: &slop_ninja_protocol::ArtifactHeader, version: Version) -> Option<String> {
    let graph = header.files.iter().find(|f| f.name == spec::GRAPH_FILE)?;
    let class = version.graph_class_by_len(graph.len)?;
    Some(slop_ninja_artifact_check::mapping::profile_id_for(class, version))
}

/// The files manifest v2 of a session whose header and every file are
/// recorded: each file's key in the admission bucket and its recorded
/// entity tag. Its exact bytes' SHA-256 (a small record) is the package.
fn files_manifest(sql: &impl Sql, r: &Row, w: &Window) -> R<Option<FilesManifest>> {
    let Some(files) = &r.files else { return Ok(None) };
    let (header, _) = rebuild_header(&w.output_contract, files).ok_or(EvalError::State("recorded header"))?;
    let version = Version::from_id(w.bounds.spec_id).ok_or(EvalError::State("spec"))?;
    let objs = objects_of(sql, &r.session)?;
    let Some(profile) = profile_of(&header, version) else {
        return Ok(None);
    };
    let m = FilesManifest::for_header(&header, version, &profile, |_, sha| {
        let d = crate::eval::plan::parse_hex32(sha).unwrap_or([0; 32]);
        let etag = objs.iter().find(|o| o.kind == ObjectKind::File(d)).and_then(|o| o.etag.clone());
        ("admission".into(), key_of(r, ObjectKind::File(d)), etag)
    });
    Ok(m.ok())
}

/// The package of a files manifest: the SHA-256 of its exact bytes (a
/// small record, the one the scorer binds).
pub fn package_of(bytes: &[u8]) -> String {
    FilesManifest::package_of(bytes)
}

/// The eval-bucket key of a package's files manifest.
pub fn manifest_key(package: &str) -> String {
    format!("artifacts/packages/{package}.json")
}

/// When the header and every file of an uploading session are recorded,
/// the session verifies: its files manifest v2 and package are fixed (the
/// manifest is written to the eval bucket by the `manifest` step, then the
/// admission run follows).
fn maybe_complete(sql: &impl Sql, id: &str, now_ms: u64) -> R<()> {
    let Some(r) = session_row(sql, id)? else { return Ok(()) };
    if r.state != SessionState::Uploading || r.files.is_none() {
        return Ok(());
    }
    if objects_of(sql, id)?.iter().any(|o| !o.recorded()) {
        return Ok(());
    }
    let Some(w) = window_for(sql, r.cohort)? else { return Ok(()) };
    let Some(m) = files_manifest(sql, &r, &w)? else {
        let e = refusal_error(ErrorCode::CheckFailed, "an artifact check refused it")
            .with(Detail { check: Some("files_manifest".into()), ..Default::default() });
        return set_state(sql, id, SessionState::Refused, Some(&e), now_ms);
    };
    let package = package_of(&m.to_bytes());
    sql.exec(
        "UPDATE adm_sessions SET state = 'verifying', stage = 'manifest', package_sha256 = ?, manifest_key = ?,
            step_tries = 0, step_after_ms = 0, updated_ms = ? WHERE session = ? AND state = 'uploading'",
        &[s(&package), s(manifest_key(&package)), bind_u64(now_ms)?, s(id)],
    )?;
    Ok(())
}

/// The session whose files manifest is due to be written (stage
/// `manifest`): `(session, key, exact bytes)`, the bytes rebuilt from the
/// recorded table and entity tags and checked to be the recorded package.
pub fn manifest_due(sql: &impl Sql, id: &str) -> R<Option<(String, Vec<u8>)>> {
    let Some(r) = session_row(sql, id)? else { return Ok(None) };
    if r.state != SessionState::Verifying || r.stage.as_deref() != Some("manifest") {
        return Ok(None);
    }
    let Some(w) = window_for(sql, r.cohort)? else { return Ok(None) };
    let Some(m) = files_manifest(sql, &r, &w)? else { return Ok(None) };
    let bytes = m.to_bytes();
    let rows = sql.exec("SELECT package_sha256, manifest_key FROM adm_sessions WHERE session = ?", &[s(id)])?;
    let row = rows.first().ok_or(EvalError::State("session"))?;
    if opt_text(row, 0).as_deref() != Some(package_of(&bytes).as_str()) {
        return Err(EvalError::State("files manifest package"));
    }
    Ok(Some((text(row, 1)?.to_owned(), bytes)))
}

/// A durable step of a session (the manifest write, an audit) that met a
/// platform failure: it runs again after a backoff that grows with its
/// failures, never ending the session.
pub fn step_failed(sql: &impl Sql, id: &str, now_ms: u64) -> R<()> {
    let rows = sql.exec("SELECT step_tries FROM adm_sessions WHERE session = ?", &[s(id)])?;
    let Some(r) = rows.first() else { return Ok(()) };
    let tries = u32::try_from(int(r, 0)?).unwrap_or(u32::MAX).saturating_add(1);
    sql.exec(
        "UPDATE adm_sessions SET step_tries = ?, step_after_ms = ?, updated_ms = ? WHERE session = ?",
        &[i(i64::from(tries)), bind_u64(now_ms.saturating_add(backoff_ms(tries)))?, bind_u64(now_ms)?, s(id)],
    )?;
    Ok(())
}

/// After the files manifest is in the eval bucket: the verdict (the
/// admission run) is next, under one run per hotkey and commitment
/// (rule (a)).
pub fn manifest_written(sql: &impl Sql, cfg: &AdmissionEnv, id: &str, now_ms: u64) -> R<()> {
    let Some(r) = session_row(sql, id)? else { return Ok(()) };
    if r.state != SessionState::Verifying || r.stage.as_deref() != Some("manifest") {
        return Ok(());
    }
    let rows = sql.exec("SELECT package_sha256, manifest_key FROM adm_sessions WHERE session = ?", &[s(id)])?;
    let row = rows.first().ok_or(EvalError::State("session"))?;
    let (package, key) = (text(row, 0)?.to_owned(), text(row, 1)?.to_owned());
    let Some(w) = window_for(sql, r.cohort)? else {
        return Ok(());
    };
    let existing = sql.exec(
        "SELECT package_sha256 FROM adm_verdicts WHERE hotkey = ? AND commitment = ?",
        &[s(hex::encode(r.hotkey.0)), s(r.upload.commitment.hex())],
    )?;
    if let Some(e) = existing.first() {
        if text(e, 0)? != package {
            // Rule (a): one admission run per hotkey and commitment.
            let e = ApiError::new(
                ErrorCode::CheckFailed,
                "this commitment's admission run judged another object; commit again to submit another artifact",
            )
            .with(Detail {
                check: Some("admission_run_used".into()),
                ..Default::default()
            });
            set_state(sql, id, SessionState::Refused, Some(&e), now_ms)?;
            sql.exec("UPDATE adm_sessions SET stage = NULL WHERE session = ?", &[s(id)])?;
            return Ok(());
        }
    } else {
        sql.exec(
            "INSERT INTO adm_verdicts (hotkey, commitment, package_sha256, manifest_key, label, scorer_binary, spec_sha256, state, updated_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', ?)",
            &[
                s(hex::encode(r.hotkey.0)),
                s(r.upload.commitment.hex()),
                s(&package),
                s(&key),
                s(&w.label),
                s(&cfg.scorer_binary),
                s(&w.bounds.spec_sha256),
                bind_u64(now_ms)?,
            ],
        )?;
    }
    sql.exec(
        "UPDATE adm_sessions SET stage = 'verdict', step_tries = 0, step_after_ms = 0, updated_ms = ? WHERE session = ?",
        &[bind_u64(now_ms)?, s(id)],
    )?;
    apply_verdicts(sql, cfg, now_ms)
}

/// The recorded objects a session in audit heads (stage `audit`, after its
/// admission run, a scoring lane or publication met a missing or changed
/// object: [`object_fault`]).
pub fn audit_probes(sql: &impl Sql, id: &str) -> R<Vec<Probe>> {
    let Some(r) = session_row(sql, id)? else { return Ok(Vec::new()) };
    if !matches!(r.state, SessionState::Verifying | SessionState::Admitted) || r.stage.as_deref() != Some("audit") {
        return Ok(Vec::new());
    }
    Ok(objects_of(sql, id)?.iter().filter(|o| o.etag.is_some()).map(|o| probe(&r, o)).collect())
}

/// An audit's heads are applied: a session still in audit (nothing absent,
/// nothing replaced) runs its verdict again; a head that failed retries
/// the audit after a backoff.
///
/// A session still in audit here had every recorded object present with
/// its recorded entity tag, although a load reported one missing or
/// changed: the read path failed (an etag-conditional read the platform
/// refused, say), not the objects. Review round 2: that is counted, never a
/// silent loop. A verifying session's verdict takes a platform try
/// ([`platform_wait`]: the growing backoff, the status, and the cohort's
/// `scorer_unavailable` pause at the plan's bound); an admitted entrant's
/// count ([`OBJECT_READ_FAULT`] in the status) makes its next
/// [`object_fault`] answer "not held", so the lane counts that load as a
/// start failure under its own backoff and pause bound.
pub fn audit_done(sql: &impl Sql, id: &str, applied: &Applied, now_ms: u64) -> R<()> {
    if applied.platform {
        return step_failed(sql, id, now_ms);
    }
    let Some(r) = session_row(sql, id)? else { return Ok(()) };
    if r.stage.as_deref() != Some("audit") {
        return Ok(());
    }
    match r.state {
        SessionState::Verifying => {
            let (hotkey, commitment) = (hex::encode(r.hotkey.0), r.upload.commitment.hex());
            let rows = sql.exec(
                "SELECT platform_tries FROM adm_verdicts WHERE hotkey = ? AND commitment = ? AND state = 'pending'",
                &[s(&hotkey), s(&commitment)],
            )?;
            if let Some(v) = rows.first() {
                let tries = u32::try_from(int(v, 0)?).unwrap_or(u32::MAX);
                platform_wait(sql, &hotkey, &commitment, OBJECT_READ_FAULT, tries, now_ms)?;
            }
            sql.exec(
                "UPDATE adm_sessions SET stage = 'verdict', step_tries = 0, step_after_ms = 0, updated_ms = ?
                 WHERE session = ? AND state = 'verifying' AND stage = 'audit'",
                &[bind_u64(now_ms)?, s(id)],
            )?;
        }
        // An admitted entrant found whole again: its lanes and publication
        // resume (a missing object moved it to `repair` in `apply_heads`).
        SessionState::Admitted => {
            sql.exec(
                "UPDATE adm_sessions SET stage = NULL, step_tries = 0, step_after_ms = 0, read_faults = read_faults + 1,
                    updated_ms = ?
                 WHERE session = ? AND state = 'admitted' AND stage = 'audit'",
                &[bind_u64(now_ms)?, s(id)],
            )?;
        }
        _ => {}
    }
    Ok(())
}

/// The code a load that reported an object missing or changed takes when
/// the audit found every recorded object intact: the read path's failure
/// (a verdict's platform code, and the status's count of admitted entrants
/// whose loads met it).
pub const OBJECT_READ_FAULT: &str = "artifact object read failed with every object intact";

/// A scoring lane's load (`cf_lanes`, `scorer_client::OBJECT_CODES`) or
/// publication at scoring start met an object of an admitted entrant
/// absent or changed since admission recorded it: the coordinator's fault
/// (design R4, skeptic K2), never the scorer's or the artifact's. The
/// session is audited (its recorded objects headed by
/// [`Recorder::Recorded`], so an absent one is listed `repair_needed` and
/// an unclean one deleted for a clean PUT), and the caller holds, uncounted,
/// while the session is in audit or waits for its repair. `true` while held;
/// `false` when no admitted session still needing its objects matches, or
/// when an earlier audit of this entrant found every object intact
/// ([`audit_done`]): the read path's failure, which the caller counts
/// (the session is still audited, so a real loss is still found).
pub fn object_fault(sql: &impl Sql, seq: i64, by: FaultBy<'_>, now_ms: u64) -> R<bool> {
    let (q, arg) = match by {
        FaultBy::Package(p) => ("package_sha256", p),
        FaultBy::Commitment(c) => ("commitment", c),
    };
    let rows = sql.exec(
        &format!("SELECT {SESSION_COLUMNS} FROM adm_sessions WHERE seq = ? AND {q} = ? AND state = 'admitted' LIMIT 1"),
        &[i(seq), s(arg)],
    )?;
    let Some(r) = rows.first().map(row_of).transpose()? else { return Ok(false) };
    if !needed(sql, &r)? {
        return Ok(false);
    }
    if r.stage.is_none() {
        sql.exec(
            "UPDATE adm_sessions SET stage = 'audit', step_tries = 0, step_after_ms = 0, updated_ms = ?
             WHERE session = ? AND state = 'admitted' AND stage IS NULL",
            &[bind_u64(now_ms)?, s(&r.session)],
        )?;
    }
    if r.stage.as_deref() == Some("repair") {
        return Ok(true);
    }
    let faults = count(sql, "SELECT read_faults FROM adm_sessions WHERE session = ?", &[s(&r.session)])?;
    Ok(faults == 0)
}

/// The cohort snapshot's repair list (`internal::CohortSnapshot::repair`):
/// the distinct hotkeys (hex) of sessions waiting for the miner's repair
/// (stage `repair`), most recent cohort first, and whether more than
/// `internal::REPAIR_LIST_MAX` wait (then the list is empty and the flag
/// set: every signer may read and repair, as if listed).
pub fn repair_list(sql: &impl Sql) -> R<(Vec<String>, bool)> {
    let max = slop_ninja_admission_wire::internal::REPAIR_LIST_MAX;
    let rows = sql.exec(
        "SELECT hotkey FROM adm_sessions WHERE stage = 'repair' AND state IN ('verifying','admitted')
         GROUP BY hotkey ORDER BY MAX(seq) DESC, hotkey LIMIT ?",
        &[i(i64::try_from(max + 1).map_err(|_| EvalError::State("repair list"))?)],
    )?;
    if rows.len() > max {
        return Ok((Vec::new(), true));
    }
    let mut list = Vec::with_capacity(rows.len());
    for r in &rows {
        list.push(text(r, 0)?.to_owned());
    }
    Ok((list, false))
}

/// The header digest admission recorded for the admitted entrant whose
/// files manifest is `package` (the committed digest R2 verified at
/// ingest), for the seal's artifact digest; `None` for any other artifact.
pub fn recorded_header_sha256(sql: &impl Sql, package: &str) -> R<Option<Digest>> {
    let rows = sql.exec(
        "SELECT header_sha256 FROM adm_sessions WHERE package_sha256 = ? AND state = 'admitted' ORDER BY seq DESC LIMIT 1",
        &[s(package)],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    crate::eval::plan::parse_hex32(text(r, 0)?).map(|d| Some(Digest(d))).ok_or(EvalError::State("header digest"))
}

/// Whether a scoring lane's job (an entrant, by its package) is held for
/// its objects: its admitted session is in audit or waits for the miner's
/// repair. The lane asks before it starts a scorer Container again
/// (review round 2: no cold start per admission tick while the miner has
/// not repaired), so a held lane costs one cohort call per tick.
pub fn object_held(sql: &impl Sql, seq: i64, package: &str) -> R<bool> {
    let rows = sql.exec(
        "SELECT 1 FROM adm_sessions WHERE seq = ? AND package_sha256 = ? AND state = 'admitted' AND stage IN ('audit','repair')
         LIMIT 1",
        &[i(seq), s(package)],
    )?;
    Ok(!rows.is_empty())
}

/// How [`object_fault`] names the entrant: a scoring lane by its job's
/// package, publication by the roster entry's commitment (hex).
#[derive(Clone, Copy, Debug)]
pub enum FaultBy<'a> {
    Package(&'a str),
    Commitment(&'a str),
}

/// The publication report's body bound: the largest roster the protocol
/// admits (`MAX_ROSTER`, which bounds `admission.roster_cap`), each entry
/// at its longest (a 64-hex commitment and the longest state), the cohort
/// number as a `u64` and its label at its longest
/// (`config::LABEL_MAX_BYTES`, safe characters only) in its object.
pub const PUBLICATION_REPORT_BYTES: usize = r#"{"cohort":18446744073709551615,"label":"","entrants":[]}"#.len()
    + crate::config::LABEL_MAX_BYTES
    + slop_ninja_protocol::MAX_ROSTER as usize * (r#"{"commitment":"","state":"published"},"#.len() + 64);

/// One roster entry of the publisher's report ([`publication_report`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationEntry {
    /// The on-chain commitment (hex), which keys the entrant's uploads.
    pub commitment: String,
    /// `published` or `withheld` (decided for good), or `missing` (the
    /// admission object is absent: the platform's, audited).
    pub state: String,
}

/// The publisher's report on one cohort's publication at scoring start
/// (`POST /eval/admission/published`, the publisher's binding and token
/// only): each decided entrant's publication is recorded ([`published`]:
/// the end, once the cohort is closed, of its repair window and of its
/// objects' retention), and each missing one audited ([`object_fault`]).
/// Only admitted sessions of the cohort number `cohort` labelled `label`,
/// past the seal (the publisher reads the signed manifest the seal writes),
/// are touched. Answers how many were recorded and how many are held for
/// repair, or `known: false` when this store holds no such cohort: the
/// Worker routes the report to the authority the label names
/// (`config::Authority::label_authority`) and answers that one 404, so the
/// publisher keeps the report pending rather than taking it as recorded.
pub fn publication_report(
    sql: &impl Sql,
    cohort: u64,
    label: &str,
    entries: &[PublicationEntry],
    now_ms: u64,
) -> R<Value> {
    let rows = sql.exec(
        "SELECT a.seq FROM adm_cohorts a JOIN cohorts c ON c.seq = a.seq WHERE a.cohort = ? AND c.label = ?",
        &[bind_u64(cohort)?, s(label)],
    )?;
    let Some(seq) = rows.first().map(|r| int(r, 0)).transpose()? else {
        return Ok(json!({"recorded": 0, "held": 0, "known": false}));
    };
    let (mut recorded, mut held) = (0u64, 0u64);
    for e in entries {
        if e.commitment.len() != 64 || !e.commitment.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let c = e.commitment.to_ascii_lowercase();
        match e.state.as_str() {
            "published" | "withheld" => {
                let changed = sql.exec(
                    "UPDATE adm_sessions SET published = 1 WHERE seq = ? AND commitment = ? AND state = 'admitted'
                       AND published = 0 RETURNING session",
                    &[i(seq), s(&c)],
                )?;
                recorded += changed.len() as u64;
            }
            "missing" if object_fault(sql, seq, FaultBy::Commitment(&c), now_ms)? => held += 1,
            _ => {}
        }
    }
    Ok(json!({"recorded": recorded, "held": held, "known": true}))
}

// --------------------------------------------- events, sweep, reconcile

/// What an object-create event (a wakeup only: its payload carries no
/// checksum, and Queues deliver at least once) leads to.
#[derive(Clone, Debug, PartialEq)]
pub enum Observed {
    Nothing,
    /// The object belongs to no live session: delete it (a stale URL
    /// recreated it after its session ended).
    Delete(String),
    Header(HeaderJob),
    Heads(Vec<Probe>),
}

/// The live session holding `key` (uploading, verifying, verified, or
/// admitted and not yet published), and that object's record.
fn live_holder(sql: &impl Sql, id: &ObjectId) -> R<Option<(Row, Obj)>> {
    let rows = sql.exec(
        &format!("SELECT {SESSION_COLUMNS} FROM adm_sessions WHERE cohort = ? AND commitment = ? LIMIT 1"),
        &[bind_u64(id.cohort)?, s(hex::encode(id.commitment))],
    )?;
    let Some(r) = rows.first().map(row_of).transpose()? else { return Ok(None) };
    if !(r.state == SessionState::Uploading || needed(sql, &r)?) {
        return Ok(None);
    }
    let sha = match id.kind {
        ObjectKind::Header => r.upload.header_sha256.0,
        ObjectKind::File(d) => d,
    };
    let o = objects_of(sql, &r.session)?.into_iter().find(|o| o.kind == id.kind && o.sha256 == sha);
    Ok(o.map(|o| (r, o)))
}

/// Whether a queued delete of `key` may run: never while a live session
/// holds the key (uploading, or still needing its recorded objects), since
/// a delete was queued only after a conditional one failed, and the key may
/// hold that session's clean object since. A v1 key always may.
pub fn queued_delete_allowed(sql: &impl Sql, key: &str) -> R<bool> {
    let Some(id) = ObjectId::parse(key) else { return Ok(true) };
    Ok(live_holder(sql, &id)?.is_none())
}

/// An object-create event for `key`: settles that object (the same step a
/// client request runs), or deletes an object no live session holds.
/// Nothing is recorded while its cohort is closing (only the close sweep
/// records then).
pub fn observe(sql: &impl Sql, key: &str) -> R<Observed> {
    let Some(id) = ObjectId::parse(key) else { return Ok(Observed::Nothing) };
    let Some((r, o)) = live_holder(sql, &id)? else {
        return Ok(Observed::Delete(key.to_owned()));
    };
    let Some(w) = window_for(sql, r.cohort)? else { return Ok(Observed::Nothing) };
    if r.state == SessionState::Uploading {
        if !w.admitting() || o.etag.is_some() {
            return Ok(Observed::Nothing);
        }
        if o.kind == ObjectKind::Header {
            return Ok(Observed::Header(header_job(&r, &w)));
        }
        return Ok(Observed::Heads(vec![probe(&r, &o)]));
    }
    if o.missing {
        return Ok(Observed::Heads(vec![probe(&r, &o)]));
    }
    Ok(Observed::Nothing)
}

/// The alarm sweep: every unrecorded object of every uploading session that
/// had at least one URL minted (headers settled, files headed), and every
/// object awaiting repair, once per tick, within the step's R2 calls.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sweep {
    pub headers: Vec<HeaderJob>,
    pub heads: Vec<Probe>,
}

/// Starts the alarm sweep when one is due ([`TICK_MS`] after the last), or
/// `None`. A header settle costs a head, a read and the upload Worker's
/// call; a file head one call.
pub fn sweep_begin(sql: &impl Sql, now_ms: u64) -> R<Option<Sweep>> {
    if !sweep_due(sql, now_ms)? {
        return Ok(None);
    }
    sql.exec("UPDATE adm_platform SET sweep_ms = ? WHERE id = 1", &[bind_u64(now_ms)?])?;
    let mut sweep = Sweep::default();
    let mut calls = 0usize;
    let w = window(sql)?.filter(Window::admitting);
    let rows = sql.exec(
        &format!(
            "SELECT {SESSION_COLUMNS} FROM adm_sessions WHERE (state = 'uploading' AND mints > 0 AND seq = ?)
               OR session IN (SELECT session FROM adm_objects WHERE missing = 1) ORDER BY created_ms"
        ),
        &[i(w.as_ref().map_or(-1, |w| w.seq))],
    )?;
    for row in &rows {
        let r = row_of(row)?;
        if r.state == SessionState::Uploading {
            let Some(w) = &w else { continue };
            if r.files.is_none() {
                if calls + 3 > R2_CALLS_PER_STEP {
                    break;
                }
                calls += 3;
                sweep.headers.push(header_job(&r, w));
                continue;
            }
        } else if !needed(sql, &r)? {
            continue;
        }
        for o in objects_of(sql, &r.session)? {
            let wanted = if r.state == SessionState::Uploading { !o.recorded() } else { o.missing };
            if wanted && calls < R2_CALLS_PER_STEP {
                calls += 1;
                sweep.heads.push(probe(&r, &o));
            }
        }
    }
    Ok(Some(sweep))
}

fn sweep_due(sql: &impl Sql, now_ms: u64) -> R<bool> {
    let p = sql.exec("SELECT sweep_ms FROM adm_platform WHERE id = 1", &[])?;
    let last = p.first().map_or(Ok(0), |r| uint(r, 0))?;
    if last.saturating_add(TICK_MS) > now_ms {
        return Ok(false);
    }
    let w = window(sql)?.filter(Window::admitting);
    Ok(count(
        sql,
        "SELECT COUNT(*) FROM adm_sessions s WHERE (s.state = 'uploading' AND s.mints > 0 AND s.seq = ?
            AND (s.files IS NULL OR EXISTS (SELECT 1 FROM adm_objects o WHERE o.session = s.session AND o.etag IS NULL)))
           OR s.session IN (SELECT session FROM adm_objects WHERE missing = 1)",
        &[i(w.as_ref().map_or(-1, |w| w.seq))],
    )? > 0)
}

/// Starts or continues the standing reconciliation of `admission/v2/`:
/// `Some(cursor)` of the next listing page when a pass is under way or due
/// ([`TICK_MS`] after the last pass ended), else `None`. It never ends on
/// time or on an empty list, and gates nothing.
pub fn reconcile_begin(sql: &impl Sql, now_ms: u64) -> R<Option<Option<String>>> {
    // A store that never opened an admission window minted no URL: its
    // bucket holds no admission object to reconcile.
    if sql.exec("SELECT 1 FROM adm_cohorts LIMIT 1", &[])?.is_empty() {
        return Ok(None);
    }
    let p = sql.exec("SELECT reconcile_ms, reconcile_cursor FROM adm_platform WHERE id = 1", &[])?;
    let Some(r) = p.first() else { return Ok(None) };
    let cursor = opt_text(r, 1);
    if cursor.is_some() || uint(r, 0)?.saturating_add(TICK_MS) <= now_ms {
        return Ok(Some(cursor));
    }
    Ok(None)
}

/// One listing page of the reconciliation: the keys no live session holds
/// (to delete), and the cursor kept for the next page (`None` ends the
/// pass).
///
/// Only keys of this store's own cohorts are deleted (review round 2): the
/// keys name no authority, and a retired predecessor (or a successor) shares
/// the bucket, so a key of a cohort this store never opened is another
/// authority's, never this one's to judge. Authorities never share a cohort
/// number (`config::Authority`). A key under the prefix that does not parse
/// as an admission object is no authority's and is deleted.
pub fn reconcile_page(sql: &impl Sql, keys: &[String], next: Option<&str>, now_ms: u64) -> R<Vec<String>> {
    let mut gone = Vec::new();
    for k in keys {
        let delete = match ObjectId::parse(k) {
            Some(id) => {
                !sql.exec("SELECT 1 FROM adm_cohorts WHERE cohort = ?", &[bind_u64(id.cohort)?])?.is_empty()
                    && live_holder(sql, &id)?.is_none()
            }
            None => true,
        };
        if delete && k.starts_with(ADMISSION_PREFIX) {
            gone.push(k.clone());
        }
    }
    sql.exec(
        "UPDATE adm_platform SET reconcile_cursor = ?, reconcile_ms = CASE WHEN ? IS NULL THEN ? ELSE reconcile_ms END,
            reconcile_deleted = reconcile_deleted + ? WHERE id = 1",
        &[next.map_or(SqlValue::Null, s), next.map_or(SqlValue::Null, s), bind_u64(now_ms)?, i(gone.len() as i64)],
    )?;
    Ok(gone)
}

/// The internal signer Worker's interface (`subnet/r2-signer`), as the eval
/// Worker calls it through its service binding `R2_SIGNER_TESTNET`: the
/// internal origin, the mint route, the token header (the shared Worker
/// secret `SIGNER_INTERFACE_TOKEN`) and the media type of the fixed-size
/// messages (`slop_ninja_r2_presign::wire`). The signer crate is a Worker of
/// its own; a test checks these equal its definitions.
pub mod signer {
    pub const ORIGIN: &str = "https://r2-signer.internal";
    pub const MINT_PATH: &str = "/internal/r2-signer/v1/mint";
    pub const TOKEN_HEADER: &str = "x-sn-interface-token";
    pub const MEDIA_TYPE: &str = "application/vnd.slop-ninja.r2-mint.v1";
    /// The eval Worker's service binding to the signer.
    pub const BINDING: &str = "R2_SIGNER_TESTNET";
    /// The Worker secret both hold.
    pub const TOKEN_SECRET: &str = "SIGNER_INTERFACE_TOKEN";
}

/// The Wrangler var naming the admission events Queue (R2 object-create
/// notifications on the admission bucket's `admission/v2/` prefix), consumed
/// by the eval Worker. The name is deployment configuration, not code: the
/// eval env's `[vars]` carries it with the same value as its queue
/// consumer's `queue` (a test checks both), so no resource name is compiled
/// in. Only the eval env consumes Queues. If the var is unset or empty the
/// `queue` export refuses every batch rather than guess its route.
pub const EVENTS_QUEUE_VAR: &str = "ADMISSION_EVENTS_QUEUE";

/// The events consumer's `max_concurrency` (tracked in `wrangler.toml`, a
/// test checks it): one batch at a time. An event is a wakeup only (a late
/// or lost one loses nothing: the alarm sweep and the close sweep record
/// every object), so one batch at a time costs only latency, and it is the
/// bound the memory fit counts (`arena_routes`: one batch's control and one
/// event delivery, each message handled in turn). Review round 2: before,
/// the fit counted the verdict claim width, which bounds nothing here.
pub const EVENTS_MAX_CONCURRENCY: usize = 1;

const _: () = assert!(slop_ninja_r2_presign::layout::MAX_KEY_LEN < EVENT_MESSAGE_BYTES);

/// The bound of one R2 event notification message as JSON, refused before
/// it is decoded (design section 4): its fixed fields (account id, action,
/// bucket name, event time, the object's size and entity tag) and at most
/// two R2 keys (the object's and a copy source's), each at most 1,024 bytes
/// and at most six JSON bytes per key byte (a `\u00XX` escape). Version
/// `admission-event-bound-2026-11-04`.
pub const EVENT_MESSAGE_BYTES: usize = 1024 + 2 * 6 * 1024;

/// The object key an R2 event notification names (`object.key`), for a
/// message within [`EVENT_MESSAGE_BYTES`]; `None` for anything else. The
/// payload is a wakeup only: nothing else in it is used.
pub fn event_key(message: &str) -> Option<String> {
    if message.len() > EVENT_MESSAGE_BYTES {
        return None;
    }
    #[derive(Deserialize)]
    struct Event {
        object: EventObject,
    }
    #[derive(Deserialize)]
    struct EventObject {
        key: String,
    }
    let e: Event = serde_json::from_str(message).ok()?;
    ObjectId::parse(&e.object.key).map(|_| e.object.key)
}

// ------------------------------------------------- the upload path's faults

/// The three persisted faults of the upload path. Each defers the close
/// until a later success clears it, so no platform outage ends a miner's
/// upload as `incomplete_at_cutoff`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// A mint the signer could not make (`signer_unavailable`).
    Signer,
    /// The end-to-end canary failed (`upload_path_unavailable`).
    Path,
    /// The front every session passes before any file URL exists failed
    /// for the platform's reason (`upload_front_unavailable`): a header
    /// settle that could not run (R2's head or pinned read, the upload
    /// Worker's binding, its header route or its answer), or the canary's
    /// link probe (the header route and the upload Worker's registration
    /// read, which every signed route needs). Cleared by a later header
    /// settle the upload Worker answered, or a passing link probe.
    Front,
}

/// The fault codes, as the status shows them.
pub const SIGNER_UNAVAILABLE: &str = "signer_unavailable";
pub const UPLOAD_PATH_UNAVAILABLE: &str = "upload_path_unavailable";
pub const UPLOAD_FRONT_UNAVAILABLE: &str = "upload_front_unavailable";

fn platform_fault(sql: &impl Sql, fault: Fault, code: &str, now_ms: u64) -> R<()> {
    // A fault cancels any earlier success: a close wanted at a boundary now
    // waits for the first boundary after a later success (skeptic R3).
    clear_want(sql)?;
    let code = crate::eval::lanes::textgen_safe(code);
    match fault {
        Fault::Signer => sql.exec(
            "UPDATE adm_platform SET signer_fault_ms = ?, signer_error = ? WHERE id = 1",
            &[bind_u64(now_ms)?, s(code)],
        )?,
        Fault::Path => sql.exec(
            "UPDATE adm_platform SET canary_fault_ms = ?, canary_error = ? WHERE id = 1",
            &[bind_u64(now_ms)?, s(code)],
        )?,
        Fault::Front => sql.exec(
            "UPDATE adm_platform SET front_fault_ms = ?, front_error = ? WHERE id = 1",
            &[bind_u64(now_ms)?, s(code)],
        )?,
    };
    Ok(())
}

/// The front worked (a header settle the upload Worker answered, or a
/// passing link probe): clears `upload_front_unavailable`.
fn front_ok(sql: &impl Sql, now_ms: u64) -> R<()> {
    sql.exec(
        "UPDATE adm_platform SET front_ok_ms = ?, front_fault_ms = NULL, front_error = NULL WHERE id = 1",
        &[bind_u64(now_ms)?],
    )?;
    Ok(())
}

fn mint_ok(sql: &impl Sql, now_ms: u64) -> R<()> {
    sql.exec(
        "UPDATE adm_platform SET mint_ok_ms = ?, signer_fault_ms = NULL, signer_error = NULL WHERE id = 1",
        &[bind_u64(now_ms)?],
    )?;
    Ok(())
}

/// The persisted upload-path state: the outstanding faults (since when)
/// and the last canary success.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Platform {
    signer_fault_ms: Option<u64>,
    canary_fault_ms: Option<u64>,
    front_fault_ms: Option<u64>,
    canary_ok_ms: Option<u64>,
    canary_after_ms: u64,
}

impl Platform {
    fn faulted(&self) -> bool {
        self.signer_fault_ms.is_some() || self.canary_fault_ms.is_some() || self.front_fault_ms.is_some()
    }
}

fn platform(sql: &impl Sql) -> R<Platform> {
    let rows = sql.exec(
        "SELECT signer_fault_ms, canary_fault_ms, canary_ok_ms, canary_after_ms, front_fault_ms FROM adm_platform WHERE id = 1",
        &[],
    )?;
    let Some(r) = rows.first() else { return Ok(Platform::default()) };
    Ok(Platform {
        signer_fault_ms: opt_int(r, 0).map(|v| v as u64),
        canary_fault_ms: opt_int(r, 1).map(|v| v as u64),
        front_fault_ms: opt_int(r, 4).map(|v| v as u64),
        canary_ok_ms: opt_int(r, 2).map(|v| v as u64),
        canary_after_ms: uint(r, 3)?,
    })
}

/// The cohort a canary is due for: when a close is wanted at a boundary and
/// no canary passed since it was wanted, or while an upload-path fault is
/// outstanding (retried once per tick until a success clears it). Starting
/// it arms the next attempt one tick later.
pub fn canary_begin(sql: &impl Sql, now_ms: u64) -> R<Option<u64>> {
    let Some(cohort) = canary_due(sql, now_ms)? else { return Ok(None) };
    sql.exec(
        "UPDATE adm_platform SET canary_after_ms = ? WHERE id = 1",
        &[bind_u64(now_ms.saturating_add(TICK_MS))?],
    )?;
    Ok(Some(cohort))
}

fn canary_due(sql: &impl Sql, now_ms: u64) -> R<Option<u64>> {
    let p = platform(sql)?;
    // The last attempt started one tick before `canary_after_ms`.
    let last_attempt = p.canary_after_ms.saturating_sub(TICK_MS);
    let wanted = sql.exec(
        "SELECT cohort, want_ms FROM adm_cohorts WHERE state = 'admitting' AND want_ms IS NOT NULL ORDER BY seq DESC LIMIT 1",
        &[],
    )?;
    if let Some(r) = wanted.first() {
        let want_ms = uint(r, 1)?;
        // A close newly wanted gets its canary at once; a failed one waits
        // out the tick like any retry.
        let unchecked = p.canary_ok_ms.is_none_or(|ok| ok < want_ms);
        if unchecked && (p.canary_after_ms == 0 || last_attempt < want_ms || p.canary_after_ms <= now_ms) {
            return Ok(Some(uint(r, 0)?));
        }
    }
    if p.canary_after_ms > now_ms {
        return Ok(None);
    }
    if p.faulted() {
        let w = window(sql)?;
        return Ok(Some(w.map_or(0, |w| w.cohort)));
    }
    Ok(None)
}

/// How a canary failed: at the signer's mint, or on the upload path (the
/// PUT to the S3 host, the head that confirms it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanaryFailure {
    Signer(&'static str),
    Path(&'static str),
    /// The mint and the PUT passed; the link probe of the upload Worker's
    /// header route and registration read failed.
    Front(&'static str),
}

/// Records a canary's outcome. A success clears both faults (its mint was a
/// successful mint) and, when a close is wanted at the boundary still
/// observed and no fault was recorded since it was wanted, closes the
/// window at that boundary now. A failure persists its fault; the close
/// then waits for the first boundary after a later success.
pub fn canary_done(sql: &impl Sql, outcome: Result<(), CanaryFailure>, now_ms: u64) -> R<bool> {
    let before = platform(sql)?;
    match outcome {
        Err(CanaryFailure::Signer(code)) => {
            platform_fault(sql, Fault::Signer, code, now_ms)?;
            return Ok(false);
        }
        Err(CanaryFailure::Path(code)) => {
            mint_ok(sql, now_ms)?;
            platform_fault(sql, Fault::Path, code, now_ms)?;
            return Ok(false);
        }
        Err(CanaryFailure::Front(code)) => {
            mint_ok(sql, now_ms)?;
            sql.exec("UPDATE adm_platform SET canary_fault_ms = NULL, canary_error = NULL WHERE id = 1", &[])?;
            platform_fault(sql, Fault::Front, code, now_ms)?;
            return Ok(false);
        }
        Ok(()) => {
            mint_ok(sql, now_ms)?;
            front_ok(sql, now_ms)?;
            sql.exec(
                "UPDATE adm_platform SET canary_ok_ms = ?, canary_fault_ms = NULL, canary_error = NULL WHERE id = 1",
                &[bind_u64(now_ms)?],
            )?;
        }
    }
    let Some(w) = window(sql)?.filter(Window::admitting) else { return Ok(false) };
    let rows = sql.exec("SELECT want_cutoff, want_epoch, want_ms FROM adm_cohorts WHERE seq = ?", &[i(w.seq)])?;
    let Some(r) = rows.first() else { return Ok(false) };
    let (Some(cutoff), Some(epoch), Some(want_ms)) = (opt_int(r, 0), opt_int(r, 1), opt_int(r, 2)) else {
        return Ok(false);
    };
    let current = observed(sql)?.epoch.map(|e| e.index);
    // A fault outstanding before this success: the close waits for the
    // first boundary after the clear (a fault since the want cleared it).
    let _ = want_ms;
    if before.faulted() || current != Some(epoch as u64) {
        return Ok(false);
    }
    close(sql, &w, epoch as u64, cutoff as u64, now_ms)?;
    Ok(true)
}

/// A content-free summary of the upload path's state, for the status.
fn platform_status(sql: &impl Sql) -> R<Value> {
    let rows = sql.exec(
        "SELECT signer_fault_ms, signer_error, mint_ok_ms, canary_fault_ms, canary_error, canary_ok_ms, sweep_ms,
            reconcile_ms, reconcile_cursor IS NOT NULL, reconcile_deleted, front_fault_ms, front_error, front_ok_ms
         FROM adm_platform WHERE id = 1",
        &[],
    )?;
    let Some(r) = rows.first() else { return Ok(Value::Null) };
    let mut faults = Vec::new();
    if opt_int(r, 0).is_some() {
        faults.push(SIGNER_UNAVAILABLE);
    }
    if opt_int(r, 3).is_some() {
        faults.push(UPLOAD_PATH_UNAVAILABLE);
    }
    if opt_int(r, 10).is_some() {
        faults.push(UPLOAD_FRONT_UNAVAILABLE);
    }
    Ok(json!({"faults": faults, "front_fault_ms": opt_int(r, 10), "front_error": opt_text(r, 11),
        "front_ok_ms": opt_int(r, 12), "signer_fault_ms": opt_int(r, 0), "signer_error": opt_text(r, 1),
        "mint_ok_ms": opt_int(r, 2), "canary_fault_ms": opt_int(r, 3), "canary_error": opt_text(r, 4),
        "canary_ok_ms": opt_int(r, 5), "sweep_ms": opt_int(r, 6), "reconcile_ms": opt_int(r, 7),
        "reconcile_in_pass": int(r, 8)? == 1, "reconcile_deleted": opt_int(r, 9)}))
}

/// `GET /admission/v2/cohorts/{cohort}/hotkeys/{hotkey}`.
pub fn hotkey_view(sql: &impl Sql, chain: &ChainBinding, req: &Signed, cohort: u64, hotkey: &str) -> Reply {
    let go = || -> A<Reply> {
        let (signer, _) = signer(sql, chain, req)?;
        if hotkey != ss58(&signer) {
            return Err(ApiError::new(
                ErrorCode::Forbidden,
                "the signer is not the hotkey the path names",
            ));
        }
        let cursor = cursor(sql)?.unwrap_or(0);
        let w = window_for(sql, cohort)?;
        let cutoff = w.as_ref().and_then(|w| w.cutoff_block);
        let entry = match (&w, cutoff) {
            (_, Some(c)) => commitments::last_before(sql, &signer, c)?,
            _ => commitments::latest(sql, &signer)?,
        };
        let open = w.as_ref().map_or(u64::MAX, |w| w.open_block);
        let sessions = sql
            .exec(
                &format!(
                    "SELECT {SESSION_COLUMNS} FROM adm_sessions WHERE cohort = ? AND hotkey = ? ORDER BY created_ms"
                ),
                &[bind_u64(cohort)?, s(hex::encode(signer.0))],
            )?
            .iter()
            .map(row_of)
            .collect::<R<Vec<_>>>()?;
        let last = sql
            .exec(
                "SELECT body FROM adm_refusals WHERE cohort = ? AND hotkey = ?",
                &[bind_u64(cohort)?, s(hex::encode(signer.0))],
            )?
            .first()
            .and_then(|r| opt_text(r, 0))
            .and_then(|t| serde_json::from_str::<LastRefusal>(&t).ok());
        let mut summaries = Vec::with_capacity(sessions.len());
        for r in &sessions {
            summaries.push(SessionSummary {
                session: r.session.clone(),
                state: r.state.as_str().into(),
                refusal: r.refusal.clone(),
                repair_needed: repair_needed(&objects_of(sql, &r.session)?),
            });
        }
        let mut view = HotkeyView {
            schema: SCHEMA_HOTKEY.into(),
            cohort,
            hotkey: hotkey.into(),
            finalized_block: cursor,
            registration: entry.as_ref().and_then(|e| {
                Some(Registration {
                    uid: e.uid?,
                    registration_block: e.registration_block?,
                })
            }),
            commitment: entry.as_ref().map(|e| CommitmentView {
                block: e.block,
                block_hash: Some(e.block_hash.trim_start_matches("0x").to_owned()),
                extrinsic: e.extrinsic,
                payload: hex::encode(&e.payload),
                payload_cohort: parse_chain_payload(&e.payload).ok().map(|(c, _)| c),
                in_window: e.block >= open && cutoff.is_none_or(|c| e.block < c),
            }),
            cutoff_block: cutoff,
            sessions: summaries,
            last_refusal: last,
        };
        // The contract's response limit: a view that would exceed it lists
        // the live sessions and the newest ended sessions that fit.
        let limit = slop_ninja_admission_wire::response_limit();
        while serde_json::to_vec(&view).map_or(0, |b| b.len()) > limit {
            let oldest_ended = view
                .sessions
                .iter()
                .position(|x| SessionState::parse(&x.state).is_some_and(|st| st.ended()));
            match oldest_ended {
                Some(n) => {
                    view.sessions.remove(n);
                }
                None => break,
            }
        }
        Ok(reply_ok(200, &view))
    };
    go().unwrap_or_else(|e| reply_error(&e))
}

/// `GET /admission/v2/cohort` (unsigned, `no-store`).
pub fn cohort_info(sql: &impl Sql, chain: &ChainBinding, paused: bool) -> R<CohortInfo> {
    let w = window(sql)?;
    let validity = recent_plan(sql)?.map_or(0, |p| p.request_validity_blocks);
    let admitting = w.as_ref().map(|w| Admitting {
        cohort: w.cohort,
        label: w.label.clone(),
        opened_epoch: w.opened_epoch,
        open_block: w.open_block,
        planned_cutoff_epoch: w.planned_cutoff_epoch,
        closing: !w.admitting(),
        output_contract: w.output_contract.clone(),
        roster_cap: w.plan.roster_cap,
        selection: w.plan.selection.clone(),
        max_object_len: w.bounds.object_bytes,
        upload_policy: upload_policy(&w.plan),
        limits: limits_digest(&w.plan, &w.bounds),
        artifact_spec: Some(ArtifactSpecRef {
            id: w.bounds.spec_id.into(),
            sha256: w.bounds.spec_sha256.clone(),
        }),
    });
    // The calendar's next cohort (a queued one, or the template's next),
    // else, for an operator plan's window, the standing cohort after it (a
    // probe cut's number is skipped: it admits no one) at the earliest
    // boundary it can open.
    let next = match crate::eval::calendar::next(sql)? {
        Some(mut n) => {
            next_limits(sql, &mut n)?;
            Some(n)
        }
        None => match w.as_ref() {
            Some(w) => Some(NextCohort {
                cohort: crate::eval::calendar::standing_after(sql, w.cohort)?,
                earliest_open_epoch: Some(w.opened_epoch.saturating_add(w.k_tempos)),
                admission_tempos: None,
                ..Default::default()
            }),
            None => None,
        },
    };
    Ok(CohortInfo {
        schema: SCHEMA_COHORT.into(),
        contract: CONTRACT.into(),
        chain: Chain::from_binding(chain),
        finalized_block: cursor(sql)?.unwrap_or(0),
        request_validity_blocks: validity,
        paused,
        admitting,
        next,
        confirmed_epoch: observed(sql)?.epoch.map(|e| e.index),
    })
}

/// The next cohort's output contract, object bound and artifact spec, from
/// the plan it opens with: its queued plan, else the unpaused standing
/// template's (the calendar's next cohort is the template's then). Lets
/// `check` compare an object with them between windows.
fn next_limits(sql: &impl Sql, n: &mut NextCohort) -> R<()> {
    let queued = sql.exec(
        "SELECT seq FROM cohorts WHERE cohort = ? AND phase = 'queued' ORDER BY seq DESC LIMIT 1",
        &[bind_u64(n.cohort)?],
    )?;
    let plan = match queued.first() {
        Some(r) => Some(engine::cohort(sql, int(r, 0)?)?.plan),
        None => crate::eval::calendar::active_template(sql)?,
    };
    let Some(plan) = plan else { return Ok(()) };
    let Some(bounds) = plan.admission.as_ref().and_then(|a| SpecBounds::of_id(&a.artifact_spec)) else {
        return Ok(());
    };
    n.output_contract = plan.reward_v1.as_ref().map(|v| v.output_contract.clone());
    n.max_object_len = Some(bounds.object_bytes);
    n.artifact_spec = Some(ArtifactSpecRef {
        id: bounds.spec_id.into(),
        sha256: bounds.spec_sha256.clone(),
    });
    Ok(())
}

/// The resumable cohort fault of v1 framing (`framing_unavailable`). Kept so
/// a store paused under it resumes: nothing raises it any more, and it
/// clears on the next admission step (`outage_recheck`).
pub const FRAMING_UNAVAILABLE: &str = "framing_unavailable";

/// Whether `failures` consecutive platform failures of one step reach the
/// pause bound: at the bound and at every multiple of it, so after an
/// operator clears the fault the step gets a full bound of attempts again.
fn pause_due(failures: u32, bound: u32) -> bool {
    failures > 0 && failures.is_multiple_of(bound.max(1))
}

/// A commitment resolution of `hotkey` met its `tries`-th answer without
/// state ([`commitments`]'s `state_absent`): when the hotkey has a session
/// uploading or verifying in the admitting or closing cohort (its header
/// settle waits for the resolution, and the freeze for the settle), the
/// cohort faults at every multiple of the plan's pause bound
/// ([`commitments::STATE_UNAVAILABLE`], resumable).
pub fn waiting_on_state(sql: &impl Sql, hotkey: &AccountId, block: u64, tries: u32, now_ms: u64) -> R<()> {
    let Some(w) = window(sql)? else { return Ok(()) };
    if !pause_due(tries, w.failures_before_pause) {
        return Ok(());
    }
    let waiting = count(
        sql,
        "SELECT COUNT(*) FROM adm_sessions WHERE seq = ? AND hotkey = ? AND state IN ('uploading','verifying')",
        &[i(w.seq), s(hex::encode(hotkey.0))],
    )?;
    if waiting > 0 {
        let subject = format!("state:{}@{block}", hex::encode(hotkey.0));
        engine::fault_with_cause(sql, w.seq, commitments::STATE_UNAVAILABLE, Some(&subject), now_ms)?;
    }
    Ok(())
}

/// The closing cohort at `cutoff` met its snapshot's `tries`-th answer in a
/// row that served no state (a lagging node's not-served refusal, or an
/// answer without state; [`commitments`]'s `snapshot_absent`): whether that
/// reaches the plan's pause bound, at which the cohort pauses
/// (`snapshot_state_discarded`, resumable), as a resolution stage's
/// ([`waiting_on_state`]). `false` when no cohort is closing at `cutoff`.
pub fn snapshot_pause_due(sql: &impl Sql, cutoff: u64, tries: u32) -> R<bool> {
    let rows = sql.exec(
        "SELECT seq FROM adm_cohorts WHERE cutoff_block = ? AND state = 'closing'",
        &[bind_u64(cutoff)?],
    )?;
    let Some(r) = rows.first() else { return Ok(false) };
    let c = engine::cohort(sql, int(r, 0)?)?;
    Ok(window_of(sql, &c)?.is_some_and(|w| pause_due(tries, w.failures_before_pause)))
}

/// The admitting or closing cohort's sequence number (0 when none), the
/// cohort an admission step's arena refusal faults.
pub fn current_seq(sql: &impl Sql) -> R<i64> {
    let rows = sql.exec(
        "SELECT seq FROM adm_cohorts WHERE state IN ('admitting','closing') ORDER BY seq DESC LIMIT 1",
        &[],
    )?;
    rows.first().map_or(Ok(0), |r| Ok(int(r, 0)?))
}

/// The wait before a durable step that met a platform failure runs again:
/// doubling from [`LANE_RETRY_MS`] up to [`LANE_LEASE_MS`].
pub fn backoff_ms(failures: u32) -> u64 {
    LANE_RETRY_MS
        .saturating_mul(1u64 << failures.saturating_sub(1).min(16))
        .min(LANE_LEASE_MS)
}

/// An operator's recorded way out of a stalled session: a session still
/// uploading, verifying or verified is refused with the operator's reason
/// (`check_failed`, `operator:<reason>`), and its pending verdict with it.
/// `false` when the session is unknown or already ended.
pub fn operator_refuse(sql: &impl Sql, id: &str, reason: &str, now_ms: u64) -> R<bool> {
    let ok = !reason.is_empty()
        && reason.len() <= 64
        && reason
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.:".contains(&b));
    if !ok {
        return Err(EvalError::Plan("refusal reason (a-z 0-9 _ - . :, at most 64)"));
    }
    let Some(r) = session_row(sql, id)? else {
        return Ok(false);
    };
    if !matches!(
        r.state,
        SessionState::Uploading | SessionState::Verifying | SessionState::Verified
    ) {
        return Ok(false);
    }
    let code = format!("operator:{reason}");
    let e = ApiError::new(ErrorCode::CheckFailed, "the operator refused this session").with(Detail {
        check: Some(code.clone()),
        ..Default::default()
    });
    set_state(sql, id, SessionState::Refused, Some(&e), now_ms)?;
    sql.exec("UPDATE adm_sessions SET stage = NULL WHERE session = ?", &[s(id)])?;
    refuse_verdict(sql, &r, &code, now_ms)?;
    Ok(true)
}

// ------------------------------------------------------------- the close

/// Closes an admitting cohort's window at the epoch boundary that reached
/// its admission minimum (`engine::advance`); `cutoff_block` is the
/// boundary's first block. The canary is the close's first step (skeptic
/// K4): the close is recorded as wanted at this boundary, and the window
/// closes once a canary passed after that (here, or in [`canary_done`]
/// while the boundary is still the observed one). While an upload-path
/// fault (`signer_unavailable`, `upload_path_unavailable`) is outstanding
/// the close waits for the first boundary after a later success, so the
/// platform's outage never ends a miner's upload. `true` when it closed.
pub fn close_window(sql: &impl Sql, c: &Cohort, epoch: u64, cutoff_block: u64, now_ms: u64) -> R<bool> {
    let Some(w) = window_of(sql, c)? else {
        return Err(EvalError::State("admission window"));
    };
    if !w.admitting() {
        return Ok(false);
    }
    let p = platform(sql)?;
    if p.faulted() {
        clear_want(sql)?;
        return Ok(false);
    }
    let rows = sql.exec("SELECT want_cutoff, want_ms FROM adm_cohorts WHERE seq = ?", &[i(w.seq)])?;
    let (want_cutoff, want_ms) = rows
        .first()
        .map(|r| (opt_int(r, 0).map(|v| v as u64), opt_int(r, 1).map(|v| v as u64)))
        .unwrap_or((None, None));
    match (want_cutoff, want_ms) {
        (Some(c), Some(ms)) if c == cutoff_block && p.canary_ok_ms.is_some_and(|ok| ok >= ms) => {
            close(sql, &w, epoch, cutoff_block, now_ms)?;
            Ok(true)
        }
        (Some(c), Some(_)) if c == cutoff_block => Ok(false),
        _ => {
            sql.exec(
                "UPDATE adm_cohorts SET want_cutoff = ?, want_epoch = ?, want_ms = ? WHERE seq = ?",
                &[bind_u64(cutoff_block)?, bind_u64(epoch)?, bind_u64(now_ms)?, i(w.seq)],
            )?;
            Ok(false)
        }
    }
}

/// A fault since a close was wanted: the close waits for a later boundary.
fn clear_want(sql: &impl Sql) -> R<()> {
    sql.exec(
        "UPDATE adm_cohorts SET want_cutoff = NULL, want_epoch = NULL, want_ms = NULL WHERE state = 'admitting'",
        &[],
    )?;
    Ok(())
}

/// Persists `closing` (design section 6, step 1): from here `create` and
/// `urls` answer `after_cutoff`, and only the close sweep records presence.
fn close(sql: &impl Sql, w: &Window, epoch: u64, cutoff: u64, now_ms: u64) -> R<()> {
    sql.exec(
        "UPDATE adm_cohorts SET state = 'closing', cutoff_block = ?, closed_at_epoch = ?, close_recorded = 0,
            close_after_ms = 0, want_cutoff = NULL, want_epoch = NULL, want_ms = NULL
         WHERE seq = ? AND state = 'admitting'",
        &[bind_u64(cutoff)?, bind_u64(epoch)?, i(w.seq)],
    )?;
    set_phase(
        sql,
        w.seq,
        "admission",
        now_ms,
        "admission closing at a boundary: the close sweep heads every unrecorded object, then the close is recorded",
    )
}

/// The closing cohort whose close sweep has objects left to head and is
/// due (its backoff after a failed head passed): `(seq, probes)` within the
/// step's R2 calls. A closing cohort with nothing left records its close
/// here ([`close_finish`]) and gives `None`.
pub fn close_begin(sql: &impl Sql, now_ms: u64) -> R<Option<(i64, Vec<Probe>)>> {
    let rows = sql.exec(
        "SELECT seq, close_after_ms FROM adm_cohorts WHERE state = 'closing' AND close_recorded = 0 ORDER BY seq LIMIT 1",
        &[],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    let seq = int(r, 0)?;
    if uint(r, 1)? > now_ms {
        return Ok(None);
    }
    let left = sql.exec(
        &format!(
            "SELECT {SESSION_COLUMNS} FROM adm_sessions s WHERE s.seq = ? AND s.state = 'uploading' AND s.files IS NOT NULL
               AND EXISTS (SELECT 1 FROM adm_objects o WHERE o.session = s.session AND o.etag IS NULL AND o.close_headed = 0)
             ORDER BY s.session"
        ),
        &[i(seq)],
    )?;
    let mut probes = Vec::new();
    for row in &left {
        let r = row_of(row)?;
        for o in objects_of(sql, &r.session)? {
            if o.etag.is_none() && !o.close_headed && probes.len() < R2_CALLS_PER_STEP {
                probes.push(probe(&r, &o));
            }
        }
    }
    if probes.is_empty() {
        close_finish(sql, seq, now_ms)?;
        return Ok(None);
    }
    Ok(Some((seq, probes)))
}

/// Whether a closing cohort's close sweep has a step due (or its close to
/// record), without changing anything.
fn close_due(sql: &impl Sql, now_ms: u64) -> R<bool> {
    Ok(count(
        sql,
        "SELECT COUNT(*) FROM adm_cohorts WHERE state = 'closing' AND close_recorded = 0 AND close_after_ms <= ?",
        &[bind_u64(now_ms)?],
    )? > 0)
}

/// After a close sweep step: a head that errored (or showed no stored
/// checksum) defers the sweep after a backoff; its progress is kept.
pub fn close_step_done(sql: &impl Sql, seq: i64, applied: &Applied, now_ms: u64) -> R<()> {
    if applied.platform {
        sql.exec(
            "UPDATE adm_cohorts SET close_after_ms = ? WHERE seq = ?",
            &[bind_u64(now_ms.saturating_add(LANE_RETRY_MS))?, i(seq)],
        )?;
    }
    Ok(())
}

/// Records the close (design section 6, steps 3-4): every uploading session
/// whose header and files are all recorded verifies; every other is
/// refused `incomplete_at_cutoff`; the commitment snapshot covers every
/// hotkey whose upload completed in time.
///
/// A session whose header was never settled cannot have sent a file (file
/// URLs exist only after the settle), so its refusal is the miner's state,
/// never a platform outage's: while the window admits, the alarm sweep
/// settles every present header each tick, and any settle the platform
/// could not run persists `upload_front_unavailable`, which defers this
/// close until a settle or link probe works again.
fn close_finish(sql: &impl Sql, seq: i64, now_ms: u64) -> R<()> {
    let rows = sql.exec("SELECT session FROM adm_sessions WHERE seq = ? AND state = 'uploading'", &[i(seq)])?;
    for r in &rows {
        maybe_complete(sql, text(r, 0)?, now_ms)?;
    }
    let e = ApiError::new(
        ErrorCode::IncompleteAtCutoff,
        "the session was not completed before the close",
    );
    for r in sql.exec("SELECT session FROM adm_sessions WHERE seq = ? AND state = 'uploading'", &[i(seq)])? {
        set_state(sql, text(&r, 0)?, SessionState::Refused, Some(&e), now_ms)?;
    }
    let cutoff = sql
        .exec("SELECT cutoff_block FROM adm_cohorts WHERE seq = ?", &[i(seq)])?
        .first()
        .and_then(|r| opt_int(r, 0))
        .ok_or(EvalError::State("cutoff"))?;
    sql.exec(
        "INSERT OR IGNORE INTO adm_snapshot_wanted (cutoff_block, hotkey)
         SELECT DISTINCT ?, hotkey FROM adm_sessions WHERE seq = ? AND state IN ('verifying','verified')",
        &[i(cutoff), i(seq)],
    )?;
    sql.exec("UPDATE adm_cohorts SET close_recorded = 1 WHERE seq = ?", &[i(seq)])?;
    set_phase(
        sql,
        seq,
        "admission",
        now_ms,
        "admission closed at a boundary; the roster freezes once verdicts and the commitment snapshot are in",
    )
}

/// Queues the objects of sessions that will never be needed again for
/// deletion: sessions that ended without admission once their cohort froze
/// or was abandoned, admitted ones of an abandoned cohort, and admitted
/// ones whose publication is recorded once their cohort closed.
fn cleanup_enqueue(sql: &impl Sql, now_ms: u64) -> R<()> {
    let rows = sql.exec(
        &format!(
            "SELECT {SESSION_COLUMNS} FROM adm_sessions s WHERE s.cleaned = 0 AND (
               (s.state IN ('refused','overflow','superseded')
                  AND (SELECT state FROM adm_cohorts c WHERE c.seq = s.seq) IN ('frozen','abandoned'))
               OR (s.state = 'admitted' AND (SELECT state FROM adm_cohorts c WHERE c.seq = s.seq) = 'abandoned')
               OR (s.state = 'admitted' AND s.published = 1
                  AND (SELECT phase FROM cohorts k WHERE k.seq = s.seq) = 'closed'))
             LIMIT ?"
        ),
        &[i(R2_CALLS_PER_STEP as i64)],
    )?;
    for row in &rows {
        queue_deletes(sql, &row_of(row)?, now_ms)?;
    }
    Ok(())
}

/// The queued object deletes whose backoff after a failed delete has
/// passed: `(key, upload_id)`, oldest first.
pub fn object_deletes_due(sql: &impl Sql, limit: i64, now_ms: u64) -> R<Vec<(String, Option<String>)>> {
    sql.exec(
        "SELECT key, upload_id FROM adm_object_deletes WHERE not_before_ms <= ? ORDER BY created_ms, key LIMIT ?",
        &[bind_u64(now_ms)?, i(limit)],
    )?
    .iter()
    .map(|r| Ok((text(r, 0)?.to_owned(), opt_text(r, 1))))
    .collect()
}

/// Records that the bucket confirmed a queued delete.
pub fn object_deleted(sql: &impl Sql, key: &str) -> R<()> {
    sql.exec("DELETE FROM adm_object_deletes WHERE key = ?", &[s(key)])?;
    Ok(())
}

/// Takes the one in-flight conditional delete of `key` ([`Delete`]):
/// `false` while another is in flight (review round 2: two paths that both
/// headed the same unclean object must not both delete, or the second can
/// remove the clean copy the miner PUT after the first). One deleter at a
/// time suffices: while the object it headed exists, no PUT can replace it
/// (each URL signs `if-none-match: *`), no miner can delete it, and no
/// other delete touches a key a live session holds. A marker left by an
/// invocation that died is taken over once it is older than the longest
/// scope of a cohort-object request (`arena_routes::UNBOUNDED_FETCH_MS`).
pub fn delete_begin(sql: &impl Sql, key: &str, now_ms: u64) -> R<bool> {
    sql.exec(
        "DELETE FROM adm_deleting WHERE key = ? AND since_ms <= ?",
        &[s(key), bind_u64(now_ms.saturating_sub(crate::arena_routes::UNBOUNDED_FETCH_MS))?],
    )?;
    let taken = sql.exec(
        "INSERT INTO adm_deleting (key, since_ms) VALUES (?, ?) ON CONFLICT (key) DO NOTHING RETURNING key",
        &[s(key), bind_u64(now_ms)?],
    )?;
    Ok(!taken.is_empty())
}

/// Releases the in-flight delete of `key` ([`delete_begin`]).
pub fn delete_end(sql: &impl Sql, key: &str) -> R<()> {
    sql.exec("DELETE FROM adm_deleting WHERE key = ?", &[s(key)])?;
    Ok(())
}

/// A queued delete that the bucket did not confirm: it stays queued and is
/// tried again after a growing backoff.
pub fn object_delete_failed(sql: &impl Sql, key: &str, now_ms: u64) -> R<()> {
    let rows = sql.exec("SELECT tries FROM adm_object_deletes WHERE key = ?", &[s(key)])?;
    let Some(r) = rows.first() else {
        sql.exec(
            "INSERT OR IGNORE INTO adm_object_deletes (key, upload_id, created_ms, tries, not_before_ms) VALUES (?, NULL, ?, 1, ?)",
            &[s(key), bind_u64(now_ms)?, bind_u64(now_ms.saturating_add(backoff_ms(1)))?],
        )?;
        return Ok(());
    };
    let tries = u32::try_from(int(r, 0)?).unwrap_or(u32::MAX).saturating_add(1);
    sql.exec(
        "UPDATE adm_object_deletes SET tries = ?, not_before_ms = ? WHERE key = ?",
        &[i(i64::from(tries)), bind_u64(now_ms.saturating_add(backoff_ms(tries)))?, s(key)],
    )?;
    Ok(())
}

/// The session whose durable step (the manifest write, an audit) is due:
/// `(session, stage)`.
pub fn verify_due(sql: &impl Sql, now_ms: u64) -> R<Option<(String, String)>> {
    let rows = sql.exec(
        "SELECT session, stage FROM adm_sessions WHERE ((state = 'verifying' AND stage IN ('manifest','audit'))
             OR (state = 'admitted' AND stage = 'audit'))
           AND step_after_ms <= ? ORDER BY updated_ms LIMIT 1",
        &[bind_u64(now_ms)?],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    Ok(Some((text(r, 0)?.to_owned(), text(r, 1)?.to_owned())))
}
// ---------------------------------------------------------------- verdicts

/// The deployment facts a verdict is bound to (rule (b)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdmissionEnv {
    /// `SCORER_BINARY_SHA256`: the scorer build a verdict was reached on.
    pub scorer_binary: String,
    /// The deployment's ceiling on live Container instances
    /// (`MAX_SCORE_LANES`): admission runs share it with scoring (rule (d)).
    pub max_instances: u32,
}

/// One admission run to perform: a fresh scorer instance loads the package
/// for the cohort's label and admits it by the cohort's spec.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneRun {
    pub hotkey: String,
    pub commitment: String,
    pub package_sha256: String,
    pub manifest_key: String,
    pub label: String,
    pub spec_sha256: String,
    /// The scorer build the run was claimed under: the verdict it reaches is
    /// recorded as reached on this build (rule (b)).
    pub scorer_binary: String,
    /// The try number: each try runs on its own fresh instance.
    pub try_no: u32,
    pub claim_until_ms: u64,
    /// The cohort's lane limits the run loads under (its spec's size bound
    /// and load allowance, its inactivity and document time limits).
    pub limits: AdmitLimits,
}

/// What an admission run takes from its cohort's plan: the same limits a
/// scoring lane of that cohort loads under (`ScoreLanes::limits`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmitLimits {
    pub artifact_max_bytes: u64,
    /// The stream allowance and the model-load allowance, each.
    pub load_wait_ms: u64,
    pub inactivity_ms: u64,
    /// The control document's time limit.
    pub doc_timeout_ms: u64,
}

impl AdmitLimits {
    /// Whether every limit is set: the plan's validated lane limits are the
    /// only source of these numbers, so a zero means a malformed request and
    /// the scorer lane refuses it rather than substituting a figure.
    pub fn complete(&self) -> bool {
        self.artifact_max_bytes > 0 && self.load_wait_ms > 0 && self.inactivity_ms > 0 && self.doc_timeout_ms > 0
    }

    pub fn of(plan: &CohortPlan) -> Option<Self> {
        let f = plan.fanout.as_ref()?;
        let l = f.score.limits();
        Some(Self {
            artifact_max_bytes: l.artifact_max_bytes,
            load_wait_ms: l.load_wait_ms,
            inactivity_ms: f.score.inactivity_ms,
            doc_timeout_ms: l.doc_timeout_ms,
        })
    }
}

/// The scorer lane's admission route body (`AScorer` `POST /admit`).
pub fn admit_body(run: &LaneRun) -> Value {
    json!({"package_sha256": run.package_sha256, "manifest_key": run.manifest_key, "label": run.label,
        "spec_sha256": run.spec_sha256, "limits": run.limits,
        "docs": [{"id": "0", "text": crate::eval::lane_logic::CONTROL_TEXT}]})
}

impl LaneRun {
    /// The scorer lane's object name: unique per try, so every try starts a
    /// fresh, single-use instance.
    pub fn lane_name(&self, network: &str) -> String {
        format!(
            "a-scorer/{network}/admit/{}/{}/{}",
            self.label, self.hotkey, self.try_no
        )
    }
}

/// How long a claimed run may take before another invocation takes it.
pub const LANE_LEASE_MS: u64 = 900_000;
/// The wait before an attributable failure is tried again on a fresh
/// instance, and the first wait after a platform failure ([`backoff_ms`]).
pub const LANE_RETRY_MS: u64 = 60_000;

/// Rule (b): binds the verdicts of every admitting or closing cohort to the
/// deployment's scorer build and the cohort's spec. A pending verdict is
/// re-bound in place (its next run claims the new build). A verdict reached
/// on another build or spec is reset to pending, and the sessions it
/// decided (verified, or refused by it) wait on it again, unless the hotkey
/// has another live session in that cohort. An operator's refusal is never
/// reset. Returns how many reached verdicts were reset.
pub fn rebind(sql: &impl Sql, binary: &str, now_ms: u64) -> R<usize> {
    let windows = sql.exec(
        "SELECT seq FROM adm_cohorts WHERE state IN ('admitting','closing')",
        &[],
    )?;
    let mut reset = 0;
    for w in &windows {
        let c = engine::cohort(sql, int(w, 0)?)?;
        let Some(win) = window_of(sql, &c)? else { continue };
        let spec = win.bounds.spec_sha256.clone();
        sql.exec(
            "UPDATE adm_verdicts SET scorer_binary = ?, spec_sha256 = ?, claim_until_ms = 0, updated_ms = ?
             WHERE label = ? AND state = 'pending' AND (scorer_binary != ? OR spec_sha256 != ?)",
            &[
                s(binary),
                s(&spec),
                bind_u64(now_ms)?,
                s(&win.label),
                s(binary),
                s(&spec),
            ],
        )?;
        let stale = sql.exec(
            "SELECT hotkey, commitment FROM adm_verdicts WHERE label = ? AND state != 'pending'
               AND (reached_binary IS NOT ? OR reached_spec IS NOT ?) AND (code IS NULL OR code NOT LIKE 'operator:%')",
            &[s(&win.label), s(binary), s(&spec)],
        )?;
        for v in &stale {
            let (hotkey, commitment) = (text(v, 0)?.to_owned(), text(v, 1)?.to_owned());
            sql.exec(
                "UPDATE adm_verdicts SET state = 'pending', code = NULL, artifact_failures = 0, platform_tries = 0,
                    scorer_binary = ?, spec_sha256 = ?, reached_binary = NULL, reached_spec = NULL,
                    claim_until_ms = 0, not_before_ms = 0, updated_ms = ?
                 WHERE hotkey = ? AND commitment = ?",
                &[s(binary), s(&spec), bind_u64(now_ms)?, s(&hotkey), s(&commitment)],
            )?;
            sql.exec(
                "UPDATE adm_sessions SET state = 'verifying', stage = 'verdict', refusal = NULL, updated_ms = ?
                 WHERE seq = ? AND hotkey = ? AND commitment = ?
                   AND (state = 'verified' OR (state = 'refused' AND stage = 'verdict'))
                   AND NOT EXISTS (SELECT 1 FROM adm_sessions o WHERE o.seq = adm_sessions.seq AND o.hotkey = adm_sessions.hotkey
                       AND o.session != adm_sessions.session AND o.state IN ('uploading','verifying','verified'))",
                &[bind_u64(now_ms)?, i(win.seq), s(&hotkey), s(&commitment)],
            )?;
            reset += 1;
        }
    }
    Ok(reset)
}

/// Scorer instances live now: admission runs under a claim, and scoring
/// lanes that may hold a container (rule (d): both share the deployment's
/// ceiling, which admission never raises).
fn live_instances(sql: &impl Sql, now_ms: u64) -> R<i64> {
    let runs = count(
        sql,
        "SELECT COUNT(*) FROM adm_verdicts WHERE state = 'pending' AND claim_until_ms > ?",
        &[bind_u64(now_ms)?],
    )?;
    let lanes = count(
        sql,
        &format!(
            "SELECT COUNT(*) FROM lanes WHERE kind = 'score' AND (status IN {} OR status = 'paused'
               OR (container_up = 1 AND final = 0))",
            crate::eval::lanes::LIVE
        ),
        &[],
    )?;
    Ok(runs.saturating_add(lanes))
}

/// Claims the due admission runs, at most `limit` (the caller's in-flight
/// bound), never more live instances (admission and scoring together) than
/// the deployment's ceiling. Verdicts are re-bound first (rule (b)).
pub fn claim_runs(sql: &impl Sql, cfg: &AdmissionEnv, limit: usize, now_ms: u64) -> R<Vec<LaneRun>> {
    rebind(sql, &cfg.scorer_binary, now_ms)?;
    let live = live_instances(sql, now_ms)?;
    let room = (cfg.max_instances as i64).saturating_sub(live).max(0) as usize;
    let take = limit.min(room);
    if take == 0 {
        return Ok(Vec::new());
    }
    let rows = sql.exec(
        "SELECT hotkey, commitment, package_sha256, manifest_key, label, spec_sha256, tries, platform_tries FROM adm_verdicts
         WHERE state = 'pending' AND claim_until_ms <= ? AND not_before_ms <= ?
           AND EXISTS (SELECT 1 FROM adm_sessions s WHERE s.hotkey = adm_verdicts.hotkey AND s.commitment = adm_verdicts.commitment
                       AND s.state = 'verifying' AND s.stage = 'verdict')
         ORDER BY updated_ms LIMIT ?",
        &[bind_u64(now_ms)?, bind_u64(now_ms)?, i(take as i64)],
    )?;
    let mut out = Vec::new();
    for r in &rows {
        let label = text(r, 4)?;
        let seq = sql.exec("SELECT seq FROM cohorts WHERE label = ?", &[s(label)])?;
        let limits = match seq.first().map(|r| int(r, 0)).transpose()? {
            Some(seq) => AdmitLimits::of(&engine::cohort(sql, seq)?.plan),
            None => None,
        };
        // A verdict whose cohort or lane limits cannot be read (never with
        // validated plans) leaves the claimable set as a platform failure
        // with a backoff, so the step never keeps choosing runs it cannot
        // claim and the status shows it.
        let Some(limits) = limits else {
            let n = u32::try_from(int(r, 7)?).unwrap_or(u32::MAX);
            platform_wait(sql, text(r, 0)?, text(r, 1)?, "admission_limits_unavailable", n, now_ms)?;
            continue;
        };
        let tries = u32::try_from(int(r, 6)?).unwrap_or(u32::MAX).saturating_add(1);
        let until = now_ms + LANE_LEASE_MS;
        sql.exec(
            "UPDATE adm_verdicts SET tries = ?, claim_until_ms = ? WHERE hotkey = ? AND commitment = ?",
            &[i(i64::from(tries)), bind_u64(until)?, s(text(r, 0)?), s(text(r, 1)?)],
        )?;
        out.push(LaneRun {
            hotkey: text(r, 0)?.to_owned(),
            commitment: text(r, 1)?.to_owned(),
            package_sha256: text(r, 2)?.to_owned(),
            manifest_key: text(r, 3)?.to_owned(),
            label: text(r, 4)?.to_owned(),
            spec_sha256: text(r, 5)?.to_owned(),
            scorer_binary: cfg.scorer_binary.clone(),
            try_no: tries,
            claim_until_ms: until,
            limits,
        });
    }
    Ok(out)
}

/// An admission run's outcome, classified by rule (c).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaneOutcome {
    /// The scorer admitted and loaded the artifact, and it scored the
    /// control document.
    Admitted,
    /// A definitive `refused <code>` on the fresh instance.
    Refused(String),
    /// A non-definitive failure attributable to the artifact on a fresh
    /// instance (a load that failed, died or outlasted its allowance, a host
    /// fault the checker files under `object`, a failed control document).
    ArtifactFailure(String),
    /// A platform failure (placement, `insufficient_memory`, transport, the
    /// coordinator's own manifest, a reused instance, a reply this
    /// coordinator does not know): retried with a growing backoff, never
    /// counted, raised in the status after repeated tries.
    Platform(String),
}

/// Records a run's outcome under its claim.
pub fn record_run(
    sql: &impl Sql,
    cfg: &AdmissionEnv,
    run: &LaneRun,
    outcome: &LaneOutcome,
    failures_before_refusal: u32,
    now_ms: u64,
) -> R<()> {
    let key = [s(&run.hotkey), s(&run.commitment)];
    let rows = sql.exec(
        "SELECT tries, artifact_failures, state, platform_tries FROM adm_verdicts WHERE hotkey = ? AND commitment = ?",
        &key,
    )?;
    let Some(r) = rows.first() else { return Ok(()) };
    if text(r, 2)? != "pending" || u32::try_from(int(r, 0)?).ok() != Some(run.try_no) {
        return Ok(());
    }
    let reached = |code: Option<String>, state: &str| -> R<()> {
        sql.exec(
            "UPDATE adm_verdicts SET state = ?, code = ?, reached_binary = ?, reached_spec = ?, claim_until_ms = 0, updated_ms = ?
             WHERE hotkey = ? AND commitment = ?",
            &[
                s(state),
                code.map_or(SqlValue::Null, s),
                s(&run.scorer_binary),
                s(&run.spec_sha256),
                bind_u64(now_ms)?,
                key[0].clone(),
                key[1].clone(),
            ],
        )?;
        Ok(())
    };
    match outcome {
        LaneOutcome::Admitted => reached(None, "admitted")?,
        LaneOutcome::Refused(code) => reached(Some(code.clone()), "refused")?,
        LaneOutcome::ArtifactFailure(code) => {
            let n = u32::try_from(int(r, 1)?).unwrap_or(u32::MAX).saturating_add(1);
            if n >= failures_before_refusal {
                sql.exec(
                    "UPDATE adm_verdicts SET artifact_failures = ? WHERE hotkey = ? AND commitment = ?",
                    &[i(i64::from(n)), key[0].clone(), key[1].clone()],
                )?;
                reached(Some(format!("load_failed:{code}")), "refused")?;
            } else {
                // The run reached the scorer: its platform failures in a row end.
                sql.exec(
                    "UPDATE adm_verdicts SET artifact_failures = ?, code = ?, claim_until_ms = 0, not_before_ms = ?, updated_ms = ?,
                        platform_tries = 0
                     WHERE hotkey = ? AND commitment = ?",
                    &[
                        i(i64::from(n)),
                        s(format!("load_failed:{code}")),
                        bind_u64(now_ms + LANE_RETRY_MS)?,
                        bind_u64(now_ms)?,
                        key[0].clone(),
                        key[1].clone(),
                    ],
                )?;
            }
        }
        LaneOutcome::Platform(code) if crate::eval::scorer_client::OBJECT_CODES.contains(&code.as_str()) => {
            // The run met an object missing or changed since admission
            // recorded it: the session's objects are audited (absent is
            // the platform's and waits for repair, replaced is the miner's),
            // never counted as a platform failure of the scorer.
            sql.exec(
                "UPDATE adm_verdicts SET code = ?, claim_until_ms = 0, updated_ms = ? WHERE hotkey = ? AND commitment = ?",
                &[s(code), bind_u64(now_ms)?, key[0].clone(), key[1].clone()],
            )?;
            sql.exec(
                "UPDATE adm_sessions SET stage = 'audit', step_tries = 0, step_after_ms = 0, updated_ms = ?
                 WHERE hotkey = ? AND commitment = ? AND state = 'verifying' AND stage = 'verdict'",
                &[bind_u64(now_ms)?, key[0].clone(), key[1].clone()],
            )?;
            return Ok(());
        }
        LaneOutcome::Platform(code) => {
            let n = u32::try_from(int(r, 3)?).unwrap_or(u32::MAX);
            platform_wait(sql, &run.hotkey, &run.commitment, code, n, now_ms)?;
        }
    }
    if !matches!(outcome, LaneOutcome::Platform(_)) {
        // The run reached the scorer and its outcome is applied: a cohort
        // paused by verdict platform failures (`platform_wait`) resumes once
        // no verdict still meets that pause condition (`outage_passed`).
        let seq = sql.exec(
            "SELECT seq FROM adm_sessions WHERE hotkey = ? AND commitment = ? ORDER BY seq DESC LIMIT 1",
            &key,
        )?;
        if let Some(r) = seq.first() {
            outage_passed(sql, int(r, 0)?, engine::SCORER_UNAVAILABLE, "verdict_reached_scorer", now_ms)?;
        }
    }
    apply_verdicts(sql, cfg, now_ms)
}

/// A platform failure on a pending verdict (`platform_tries` before it was
/// `tries_before`): recorded with its code, never counted against the
/// artifact, and the verdict waits out a backoff that grows with the
/// platform tries (raised in the status after repeated tries). At every
/// multiple of the plan's pause bound ([`Window::failures_before_pause`])
/// the session's cohort faults (`scorer_unavailable`, resumable), so a
/// verdict that keeps meeting platform failures is a visible pause of the
/// same cohort, never a silent wait of its freeze.
fn platform_wait(sql: &impl Sql, hotkey: &str, commitment: &str, code: &str, tries_before: u32, now_ms: u64) -> R<()> {
    let n = tries_before.saturating_add(1);
    let seq = sql.exec(
        "SELECT seq FROM adm_sessions WHERE hotkey = ? AND commitment = ? ORDER BY seq DESC LIMIT 1",
        &[s(hotkey), s(commitment)],
    )?;
    if let Some(r) = seq.first() {
        let seq = int(r, 0)?;
        if let Some(w) = window_of(sql, &engine::cohort(sql, seq)?)?
            && pause_due(n, w.failures_before_pause)
        {
            let subject = format!("verdict:{hotkey}:{commitment}");
            engine::fault_with_cause(sql, seq, engine::SCORER_UNAVAILABLE, Some(&subject), now_ms)?;
        }
    }
    sql.exec(
        "UPDATE adm_verdicts SET code = ?, platform_tries = ?, claim_until_ms = 0, not_before_ms = ?, updated_ms = ?
         WHERE hotkey = ? AND commitment = ?",
        &[
            s(code),
            i(i64::from(n)),
            bind_u64(now_ms.saturating_add(backoff_ms(n)))?,
            bind_u64(now_ms)?,
            s(hotkey),
            s(commitment),
        ],
    )?;
    Ok(())
}

/// Load failures attributable to the artifact on a fresh instance (the
/// lane client's non-definitive `Prep::Artifact` codes about the load).
const ARTIFACT_LOAD_FAILURES: &[&str] = &[
    "artifact load failed",
    "artifact load failed (load fault)",
    "container died during the load",
    "artifact load outlasted its allowance",
];

/// Classifies a scorer admission run's reply (`AScorer` `/run`: 200 with the
/// control document's result, or with `{"error"}` when scoring it failed;
/// 502 with the prepare code) by rule (c). Only a definitive refusal code
/// the scorer reports refuses; a host fault filed under `object` and the
/// load's own failures count on fresh instances; everything else, including
/// a code this coordinator does not know, is the platform's.
pub fn classify(status: Option<u16>, body: &Value) -> LaneOutcome {
    use crate::eval::scorer_client::{HOST_FAULT_CODES, REFUSALS, REFUSED};
    match status {
        Some(200) => {
            if body.get("error").is_some() {
                return LaneOutcome::ArtifactFailure("control_document_failed".into());
            }
            let control = sha_hex_of(crate::eval::lane_logic::CONTROL_TEXT.as_bytes());
            match body["results"].as_array().map(Vec::as_slice) {
                Some([one]) => match crate::eval::score::outcome(one, &control).0 {
                    slop_ninja_protocol::Outcome::Prediction { .. } => LaneOutcome::Admitted,
                    slop_ninja_protocol::Outcome::InfrastructureError => {
                        LaneOutcome::Platform("control_document_unmatched".into())
                    }
                    slop_ninja_protocol::Outcome::ParseFailure => {
                        LaneOutcome::ArtifactFailure("control_document:parse_failure".into())
                    }
                    slop_ninja_protocol::Outcome::ModelError => {
                        LaneOutcome::ArtifactFailure("control_document:model_error".into())
                    }
                    _ => LaneOutcome::ArtifactFailure("control_document:invalid_output".into()),
                },
                _ => LaneOutcome::Platform("control_document_reply".into()),
            }
        }
        Some(502) => {
            let code = body["error"].as_str().unwrap_or("");
            if HOST_FAULT_CODES.contains(&code) || ARTIFACT_LOAD_FAILURES.contains(&code) {
                LaneOutcome::ArtifactFailure(code.to_owned())
            } else if code == REFUSED || REFUSALS.iter().any(|(_, recorded)| *recorded == code) {
                LaneOutcome::Refused(code.trim_start_matches("refused ").to_owned())
            } else {
                LaneOutcome::Platform(if code.is_empty() {
                    "lane_error".into()
                } else {
                    code.to_owned()
                })
            }
        }
        Some(other) => LaneOutcome::Platform(format!("lane_status_{other}")),
        None => LaneOutcome::Platform("lane_unreachable".into()),
    }
}

/// Classifies the scorer lane's admission route reply (`AScorer` `POST
/// /admit`, always on a freshly started instance) by rule (c): `outcome`
/// `loaded` carries the control document's results (as `/run` does);
/// `artifact` the prepare's code with its `definitive` flag; `platform` the
/// platform's code. A definitive scorer refusal (`refused <code>`) refuses;
/// another definitive code is the coordinator's own manifest or bound (the
/// platform's); a non-definitive artifact failure counts only when the
/// instance was fresh. Anything else is the platform's.
pub fn classify_admit(status: Option<u16>, body: &Value) -> LaneOutcome {
    use crate::eval::scorer_client::{REFUSALS, REFUSED};
    match status {
        Some(200) => {}
        Some(other) => return LaneOutcome::Platform(format!("lane_status_{other}")),
        None => return LaneOutcome::Platform("lane_unreachable".into()),
    }
    let code = body["code"].as_str().unwrap_or("").to_owned();
    match body["outcome"].as_str() {
        Some("loaded") => classify(Some(200), body),
        Some("artifact") => {
            let definitive = body["definitive"].as_bool() == Some(true);
            let fresh = body["fresh"].as_bool() == Some(true);
            let scorer_refusal = code == REFUSED || REFUSALS.iter().any(|(_, recorded)| *recorded == code);
            match (definitive, scorer_refusal) {
                (true, true) => LaneOutcome::Refused(code.trim_start_matches("refused ").to_owned()),
                (true, false) => LaneOutcome::Platform(code),
                (false, _) if fresh => LaneOutcome::ArtifactFailure(code),
                (false, _) => LaneOutcome::Platform("artifact load failed on a reused instance".into()),
            }
        }
        Some("platform") => LaneOutcome::Platform(if code.is_empty() { "lane_error".into() } else { code }),
        _ => LaneOutcome::Platform("admit_reply".into()),
    }
}

fn sha_hex_of(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// Moves sessions waiting on a verdict to `verified` or `refused` (a
/// session refused by its verdict keeps the stage `verdict`, so a verdict
/// reset under rule (b) finds it).
fn apply_verdicts(sql: &impl Sql, cfg: &AdmissionEnv, now_ms: u64) -> R<()> {
    let _ = cfg;
    let rows = sql.exec(
        "SELECT s.session, v.state, v.code FROM adm_sessions s JOIN adm_verdicts v ON v.hotkey = s.hotkey AND v.commitment = s.commitment
         WHERE s.state = 'verifying' AND s.stage = 'verdict' AND v.state != 'pending'",
        &[],
    )?;
    for r in &rows {
        let id = text(r, 0)?;
        if text(r, 1)? == "admitted" {
            sql.exec(
                "UPDATE adm_sessions SET state = 'verified', stage = NULL, updated_ms = ? WHERE session = ?",
                &[bind_u64(now_ms)?, s(id)],
            )?;
        } else {
            let e = ApiError::new(ErrorCode::CheckFailed, "an artifact check refused it").with(Detail {
                check: opt_text(r, 2),
                ..Default::default()
            });
            set_state(sql, id, SessionState::Refused, Some(&e), now_ms)?;
        }
    }
    Ok(())
}

/// The Container cost model of the run's cohort (its plan's), at which the
/// run's instance time is settled.
pub fn run_cost_model(sql: &impl Sql, label: &str) -> R<Option<crate::eval::plan::CostModel>> {
    let rows = sql.exec("SELECT seq FROM cohorts WHERE label = ?", &[s(label)])?;
    let Some(r) = rows.first() else { return Ok(None) };
    Ok(engine::cohort(sql, int(r, 0)?)?.plan.fanout.map(|f| f.cost))
}

/// The ledger key of one admission run: the hotkey, commitment and try
/// tell runs apart, and a retried settle of the same run is the same key.
pub fn charge_key(run: &LaneRun) -> String {
    format!(
        "{}:{}:admission:{}:{}:{}",
        engine::INSTANCE_MAIN,
        run.label,
        run.hotkey,
        run.commitment,
        run.try_no
    )
}

/// Records a run's measured charge (instance time at the cohort's Container
/// rates) in the transaction that records the run, so a ledger outage or a
/// cut-off invocation never loses it. Idempotent on the run's key. Returns
/// the key and amount to settle, `None` when there is nothing to charge.
pub fn record_charge(sql: &impl Sql, run: &LaneRun, wall_ms: u64, now_ms: u64) -> R<Option<(String, u64)>> {
    let Some(model) = run_cost_model(sql, &run.label)? else { return Ok(None) };
    let micro = crate::eval::cost::probe_charge_microusd(&model, wall_ms);
    if micro == 0 {
        return Ok(None);
    }
    let key = charge_key(run);
    sql.exec(
        "INSERT INTO adm_charges (key, label, microusd, created_ms) VALUES (?, ?, ?, ?) ON CONFLICT (key) DO NOTHING",
        &[s(&key), s(&run.label), bind_u64(micro)?, bind_u64(now_ms)?],
    )?;
    Ok(Some((key, micro)))
}

/// The oldest admission charge the ledger has not confirmed.
pub fn charge_due(sql: &impl Sql) -> R<Option<engine::Work>> {
    let rows = sql.exec(
        "SELECT key, microusd FROM adm_charges ORDER BY created_ms, key LIMIT 1",
        &[],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    Ok(Some(engine::Work::AdmissionSettle {
        key: text(r, 0)?.to_owned(),
        microusd: uint(r, 1)?,
    }))
}

/// Drops a charge once the ledger answered for its key: the ledger's own
/// charges table holds the record from then on, so settled rows never
/// accumulate here.
pub fn charge_settled(sql: &impl Sql, key: &str) -> R<()> {
    sql.exec("DELETE FROM adm_charges WHERE key = ?", &[s(key)])?;
    Ok(())
}

/// The failures-before-refusal bound of the run's cohort.
pub fn failures_bound(sql: &impl Sql, label: &str) -> R<u32> {
    let rows = sql.exec("SELECT seq FROM cohorts WHERE label = ?", &[s(label)])?;
    let Some(r) = rows.first() else { return Ok(1) };
    let c = engine::cohort(sql, int(r, 0)?)?;
    Ok(window_of(sql, &c)?.map_or(1, |w| w.failures_before_refusal))
}

// ------------------------------------------------------------- cutoff, freeze

/// What a freeze attempt did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// Still waiting: verification, verdicts or the snapshot.
    Waiting(&'static str),
    /// The roster froze: the stored plan now names its entrants.
    Frozen {
        admitted: usize,
        overflow: usize,
        outside: bool,
    },
}

/// The fault a closing cohort takes when its roster cannot be frozen (a
/// frozen plan the engine refuses): the cohort pauses, never skipped, with
/// the refusal's code in its phase log (`roster_freeze_refused:<code>`).
/// Resumable: once the cause is corrected (a build that accepts the plan, a
/// session refused), an operator clear runs the freeze again on the same
/// roster.
pub const FREEZE_FAULT: &str = "roster_freeze_refused";

/// Freezes the roster of a closing cohort once it can, at a tempo boundary:
/// every completed upload verified or refused, every verdict in on the
/// deployment's build and the cohort's spec, the snapshot read. Ready within
/// the epoch the window closed in, it freezes at that boundary; ready later,
/// at the next boundary after it became ready (ruling (c)). The frozen cohort
/// moves to generating. A freeze the engine refuses faults the cohort
/// ([`FREEZE_FAULT`]) instead of failing every step.
pub fn try_freeze(sql: &impl Sql, cfg: &EvalConfig, now_ms: u64) -> R<Option<Boundary>> {
    let rows = sql.exec(
        "SELECT seq, closed_at_epoch, ready_epoch FROM adm_cohorts WHERE state = 'closing' AND close_recorded = 1 ORDER BY seq LIMIT 1",
        &[],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    let (closed_at, ready_at) = (opt_int(r, 1).map(|v| v as u64), opt_int(r, 2).map(|v| v as u64));
    let c = engine::cohort(sql, int(r, 0)?)?;
    if c.phase != "admission" || c.fault.is_some() {
        return Ok(None);
    }
    let Some(w) = window_of(sql, &c)? else { return Ok(None) };
    let epoch = observed(sql)?.epoch.map_or(0, |e| e.index);
    let decision = match ready(sql, cfg, &w, now_ms) {
        Ok(Err(why)) => {
            sql.exec("UPDATE adm_cohorts SET ready_epoch = NULL WHERE seq = ?", &[i(w.seq)])?;
            return Ok(Some(Boundary::Waiting(why)));
        }
        Ok(Ok(d)) => d,
        Err(EvalError::Store(e)) => return Err(EvalError::Store(e)),
        Err(e) => return fault_freeze(sql, c.seq, e.code(), now_ms),
    };
    let at_boundary = match (closed_at, ready_at) {
        (Some(cl), _) if epoch == cl => true,
        (_, Some(rd)) => epoch > rd,
        _ => false,
    };
    if !at_boundary {
        if ready_at.is_none() {
            sql.exec(
                "UPDATE adm_cohorts SET ready_epoch = ? WHERE seq = ?",
                &[bind_u64(epoch)?, i(w.seq)],
            )?;
        }
        return Ok(Some(Boundary::Waiting(
            "ready: the roster freezes at the next tempo boundary",
        )));
    }
    let b = match apply_freeze(sql, &c, &w, decision, epoch, now_ms) {
        Ok(b) => b,
        Err(EvalError::Store(e)) => return Err(EvalError::Store(e)),
        Err(e) => return fault_freeze(sql, c.seq, e.code(), now_ms),
    };
    if let Boundary::Frozen {
        admitted,
        overflow,
        outside,
    } = &b
    {
        let note = format!(
            "roster frozen: {admitted} admitted, {overflow} overflow, {}",
            if *outside {
                "an outside entrant (fresh stock allowed)"
            } else {
                "no outside entrant (exposed stock only)"
            }
        );
        set_phase(sql, c.seq, "generating", now_ms, &note)?;
    }
    Ok(Some(b))
}

/// What a closing window's freeze waits on, read without changing
/// anything (status and alerts; [`try_freeze`] decides): the reasons it
/// returns as [`Boundary::Waiting`], in its order. `None` unless the window
/// of `seq` is closing. A verdict reached on another scorer build shows as
/// verification pending once the freeze step has re-queued it.
pub fn freeze_wait(sql: &impl Sql, seq: i64) -> R<Option<&'static str>> {
    let rows = sql.exec(
        "SELECT cutoff_block, ready_epoch, close_recorded FROM adm_cohorts WHERE seq = ? AND state = 'closing'",
        &[i(seq)],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    if int(r, 2)? == 0 {
        return Ok(Some("the close sweep is heading the open sessions' objects"));
    }
    let verifying = count(
        sql,
        "SELECT COUNT(*) FROM adm_sessions WHERE seq = ? AND state = 'verifying'",
        &[i(seq)],
    )?;
    if verifying > 0 {
        return Ok(Some("verification or verdicts pending"));
    }
    let cutoff = opt_int(r, 0).map(|v| v as u64);
    if snapshot_needs(sql)?.is_some_and(|(c, _)| Some(c) == cutoff) {
        return Ok(Some("commitment snapshot pending"));
    }
    Ok(Some(if opt_int(r, 1).is_some() {
        "ready: the roster freezes at the next tempo boundary"
    } else {
        "the freeze step has not run since the window closed"
    }))
}

fn fault_freeze(sql: &impl Sql, seq: i64, why: &'static str, now_ms: u64) -> R<Option<Boundary>> {
    // Logged once with its code, as a refused root step is
    // (`engine::refuse_step`); a cohort already paused keeps its first fault.
    if engine::cohort(sql, seq)?.fault.is_none() {
        engine::fault(sql, seq, FREEZE_FAULT, now_ms)?;
        let phase = engine::cohort(sql, seq)?.phase;
        set_phase(sql, seq, &phase, now_ms, &format!("{FREEZE_FAULT}:{}", crate::eval::lanes::textgen_safe(why)))?;
    }
    Ok(Some(Boundary::Waiting("roster freeze refused: the cohort is paused (resumable)")))
}

/// The admitting or closing cohort `seq` paused by admission outage fault
/// `code` resumes now, after a success of the step that raises it, when the
/// subject recorded with the fault (`fault_cause`: the commitment
/// resolution, or the verdict) no longer waits on that step, and no other
/// subject still meets the fault's pause condition (a resolution of an
/// uploading or verifying hotkey at the bound of `absent_tries`, a pending
/// verdict at the bound of `platform_tries`). So another subject's success never clears a fault
/// whose subject keeps failing, and a fault with two failing subjects stays
/// until both pass (`engine::EVIDENCE_CLEARED_FAULTS`).
fn outage_passed(sql: &impl Sql, seq: i64, code: &str, how: &str, now_ms: u64) -> R<()> {
    let c = engine::cohort(sql, seq)?;
    if c.phase != "admission" || c.fault.as_deref() != Some(code) {
        return Ok(());
    }
    let Some(w) = window_of(sql, &c)? else { return Ok(()) };
    let cause = sql.exec("SELECT fault_cause FROM cohorts WHERE seq = ?", &[i(seq)])?;
    let cause = cause.first().and_then(|r| opt_text(r, 0));
    if outage_blocked(sql, seq, code, cause.as_deref(), w.failures_before_pause)? {
        return Ok(());
    }
    engine::auto_clear(sql, seq, code, how, now_ms)?;
    Ok(())
}

/// Re-reads the pause condition of every admission-phase cohort paused by
/// an admission outage fault ([`engine::EVIDENCE_CLEARED_FAULTS`]), on each
/// admission step: a fault whose recorded subject left without succeeding
/// (its session refused or withdrawn, its verdict no longer pending) while
/// no other subject meets the pause condition clears now (`outage_passed`),
/// instead of waiting for one particular success that will never come.
fn outage_recheck(sql: &impl Sql, now_ms: u64) -> R<()> {
    let rows = sql.exec(
        "SELECT seq, fault FROM cohorts WHERE phase = 'admission' AND fault IN (?, ?, ?) ORDER BY seq",
        &[
            s(FRAMING_UNAVAILABLE),
            s(commitments::STATE_UNAVAILABLE),
            s(engine::SCORER_UNAVAILABLE),
        ],
    )?;
    for r in &rows {
        outage_passed(sql, int(r, 0)?, text(r, 1)?, "subject_settled", now_ms)?;
    }
    Ok(())
}

/// Whether admission outage fault `code` of cohort `seq` still has a cause:
/// its recorded subject (`cause`) still waiting on the failing step, or any
/// subject at the pause bound (`bound`).
fn outage_blocked(sql: &impl Sql, seq: i64, code: &str, cause: Option<&str>, bound: u32) -> R<bool> {
    let bound = i64::from(bound.max(1));
    let pending = |q: &str, args: &[SqlValue]| -> R<bool> { Ok(!sql.exec(q, args)?.is_empty()) };
    let subject = cause.and_then(|c| c.split_once(':'));
    Ok(match code {
        // v1 framing is gone: nothing keeps that fault.
        FRAMING_UNAVAILABLE => false,
        commitments::STATE_UNAVAILABLE => {
            (match subject.and_then(|(k, rest)| (k == "state").then_some(rest)).and_then(|r| r.split_once('@')) {
                Some((hk, block)) => pending(
                    "SELECT 1 FROM commit_seen WHERE hotkey = ? AND block = ? AND stage IN ('hash','events','reg') AND absent_tries > 0",
                    &[s(hk), i(block.parse::<i64>().unwrap_or(-1))],
                )?,
                None => false,
            }) || pending(
                "SELECT 1 FROM commit_seen cs WHERE cs.stage IN ('hash','events','reg') AND cs.absent_tries >= ?
                   AND cs.hotkey IN (SELECT hotkey FROM adm_sessions WHERE seq = ? AND state IN ('uploading','verifying')) LIMIT 1",
                &[i(bound), i(seq)],
            )?
        }
        engine::SCORER_UNAVAILABLE => {
            (match subject.and_then(|(k, rest)| (k == "verdict").then_some(rest)).and_then(|r| r.split_once(':')) {
                Some((hk, cm)) => pending(
                    "SELECT 1 FROM adm_verdicts WHERE hotkey = ? AND commitment = ? AND state = 'pending' AND platform_tries > 0",
                    &[s(hk), s(cm)],
                )?,
                None => false,
            }) || pending(
                "SELECT 1 FROM adm_verdicts v WHERE v.state = 'pending' AND v.platform_tries >= ?
                   AND (SELECT MAX(x.seq) FROM adm_sessions x WHERE x.hotkey = v.hotkey AND x.commitment = v.commitment) = ?
                 LIMIT 1",
                &[i(bound), i(seq)],
            )?
        }
        _ => true,
    })
}

/// A commitment read served state for `who`'s resolution at `block`: its
/// absent answers in a row end, and the cohort paused by
/// [`commitments::STATE_UNAVAILABLE`] (an admitting or closing one) resumes
/// once no verifying hotkey's resolution still meets that pause condition
/// (`outage_passed`).
pub fn state_served(sql: &impl Sql, who: &AccountId, block: u64, now_ms: u64) -> R<()> {
    sql.exec(
        "UPDATE commit_seen SET absent_tries = 0 WHERE hotkey = ? AND block = ? AND absent_tries > 0",
        &[s(hex::encode(who.0)), bind_u64(block)?],
    )?;
    if let Some(w) = window(sql)? {
        outage_passed(sql, w.seq, commitments::STATE_UNAVAILABLE, "state_served", now_ms)?;
    }
    Ok(())
}

/// The commitment snapshot a closing cohort needs, if it is not complete.
pub fn snapshot_needs(sql: &impl Sql) -> R<Option<(u64, Vec<AccountId>)>> {
    let rows = sql.exec("SELECT cutoff_block FROM adm_cohorts WHERE state = 'closing' AND cutoff_block IS NOT NULL ORDER BY seq LIMIT 1", &[])?;
    let Some(r) = rows.first() else { return Ok(None) };
    let cutoff = uint(r, 0)?;
    let wanted: Vec<AccountId> = sql
        .exec(
            "SELECT hotkey FROM adm_snapshot_wanted WHERE cutoff_block = ? ORDER BY hotkey",
            &[bind_u64(cutoff)?],
        )?
        .iter()
        .filter_map(|r| text(r, 0).ok().and_then(crate::eval::plan::parse_hex32).map(AccountId))
        .collect();
    let done = count(
        sql,
        "SELECT COUNT(*) FROM adm_snapshot WHERE cutoff_block = ? AND done = 1",
        &[bind_u64(cutoff)?],
    )?;
    if wanted.is_empty() || done as usize >= wanted.len() {
        return Ok(None);
    }
    Ok(Some((cutoff, wanted)))
}

struct Candidate {
    row: Row,
    entry: LogEntry,
    package_sha256: String,
    manifest_key: String,
    model_sha256: String,
    /// The admitted artifact's header key in the admission bucket.
    header_key: String,
    /// The coldkey that owned the hotkey at the cutoff (the snapshot's
    /// `Owner`); `None` when unknown.
    coldkey: Option<[u8; 32]>,
}

/// What the freeze decided, before anything is written.
struct Decision {
    refused: Vec<(String, ApiError)>,
    admitted: Vec<Candidate>,
    overflow: Vec<Candidate>,
    plan: CohortPlan,
    outside: bool,
}

/// The freeze's decision when the roster can freeze now (`Err(why)` while
/// it waits). Nothing is written but the rule (b) re-binding, so a refused
/// plan leaves the store as it was.
fn ready(sql: &impl Sql, cfg: &EvalConfig, w: &Window, now_ms: u64) -> R<Result<Decision, &'static str>> {
    let cutoff = w.cutoff_block.ok_or(EvalError::State("cutoff"))?;
    // Rule (b): a verdict reached on another scorer build or under another
    // spec is admitted again before the roster freezes.
    if rebind(sql, &cfg.scorer_binary, now_ms)? > 0 {
        return Ok(Err("verdicts reached on another scorer build are admitted again"));
    }
    let waiting = count(
        sql,
        "SELECT COUNT(*) FROM adm_sessions WHERE seq = ? AND state = 'verifying'",
        &[i(w.seq)],
    )?;
    if waiting > 0 {
        return Ok(Err("verification or verdicts pending"));
    }
    if snapshot_needs(sql)?.is_some() {
        return Ok(Err("commitment snapshot pending"));
    }
    let c = engine::cohort(sql, w.seq)?;
    let verified: Vec<Row> = sql
        .exec(
            &format!("SELECT {SESSION_COLUMNS} FROM adm_sessions WHERE seq = ? AND state = 'verified'"),
            &[i(w.seq)],
        )?
        .iter()
        .map(row_of)
        .collect::<R<_>>()?;
    let mut refused = Vec::new();
    let mut ranked = Vec::new();
    for r in verified {
        let snap = sql.exec(
            "SELECT block, payload, uid, registration_block, coldkey FROM adm_snapshot WHERE cutoff_block = ? AND hotkey = ?",
            &[bind_u64(cutoff)?, s(hex::encode(r.hotkey.0))],
        )?;
        let overwritten = || {
            ApiError::new(
                ErrorCode::CommitmentOverwritten,
                "a later commitment before the cutoff block replaced the one this session opens",
            )
        };
        let Some(sn) = snap.first() else {
            refused.push((
                r.session.clone(),
                ApiError::new(
                    ErrorCode::CommitmentOverwritten,
                    "no commitment of the hotkey held at the cutoff",
                ),
            ));
            continue;
        };
        let entry = match commitments::last_before(sql, &r.hotkey, cutoff)? {
            Some(e)
                if r.position
                    .is_some_and(|p| p.block == e.block && p.extrinsic == e.extrinsic) =>
            {
                e
            }
            _ => {
                refused.push((r.session.clone(), overwritten()));
                continue;
            }
        };
        let same_commitment = opt_int(sn, 0).map(|b| b as u64) == Some(entry.block)
            && opt_text(sn, 1).as_deref() == Some(hex::encode(&entry.payload).as_str());
        if !same_commitment {
            refused.push((r.session.clone(), overwritten()));
            continue;
        }
        let snap_uid = opt_int(sn, 2).and_then(|u| u16::try_from(u).ok());
        let snap_reg = opt_int(sn, 3).map(|b| b as u64);
        match snap_uid {
            None => {
                refused.push((
                    r.session.clone(),
                    ApiError::new(ErrorCode::NotRegistered, "the hotkey held no UID at the cutoff"),
                ));
                continue;
            }
            Some(u) if Some(u) != entry.uid || snap_reg != entry.registration_block => {
                refused.push((
                    r.session.clone(),
                    ApiError::new(
                        ErrorCode::RegistrationMismatch,
                        "the hotkey's registration changed before the cutoff",
                    ),
                ));
                continue;
            }
            Some(_) => {}
        }
        let pk = sql.exec(
            "SELECT package_sha256, manifest_key, model_sha256 FROM adm_sessions WHERE session = ?",
            &[s(&r.session)],
        )?;
        let pk = pk.first().ok_or(EvalError::State("session"))?;
        ranked.push(Candidate {
            package_sha256: text(pk, 0)?.to_owned(),
            manifest_key: text(pk, 1)?.to_owned(),
            model_sha256: text(pk, 2)?.to_owned(),
            header_key: key_of(&r, ObjectKind::Header),
            coldkey: opt_text(sn, 4).and_then(|c| crate::eval::plan::parse_hex32(&c)),
            entry,
            row: r,
        });
    }
    ranked.sort_by_key(|c| (c.entry.block, c.entry.extrinsic));
    // Two commitments at one position (one extrinsic committing for several
    // hotkeys, which the commitment log refuses at resolution) have no
    // order the protocol accepts: both are refused.
    let mut tied = std::collections::BTreeSet::new();
    for pair in ranked.windows(2) {
        if let [a, b] = pair
            && (a.entry.block, a.entry.extrinsic) == (b.entry.block, b.entry.extrinsic)
        {
            tied.insert(a.row.session.clone());
            tied.insert(b.row.session.clone());
        }
    }
    let mut kept = Vec::new();
    for cand in ranked {
        if tied.contains(&cand.row.session) {
            refused.push((cand.row.session.clone(), shared_extrinsic()));
        } else {
            kept.push(cand);
        }
    }
    let cap = usize::from(w.plan.roster_cap).min(cfg.ceilings.roster_cap.unwrap_or(usize::MAX));
    let mut admitted = Vec::new();
    let mut overflow = Vec::new();
    for (n, cand) in kept.into_iter().enumerate() {
        if n < cap {
            admitted.push(cand);
        } else {
            overflow.push(cand);
        }
    }
    // An outside entrant unlocks fresh stock only with a working artifact of
    // its own: not one of the operator's hotkeys, not a hotkey an operator
    // coldkey owned at the cutoff (an owner that is unknown counts as the
    // operator's whenever the plan lists coldkeys), and not a copy of an
    // artifact already published (whose copy the seal leaves unexecuted).
    // Exact digests only: a near copy is caught by the copy gate at scoring,
    // which runs after the draw, so standing templates carry no fresh pool
    // (`calendar`).
    // The published index grows with every cohort, so each admitted
    // candidate (at most `roster_cap`) is looked up by its key rather than
    // loading the index (both sides are `Digest::hex`).
    let operator_owned = |a: &Candidate| match a.coldkey {
        Some(c) => w.plan.is_operator_coldkey(&c),
        None => !w.plan.operator_coldkeys.is_empty(),
    };
    let mut outside = false;
    for a in &admitted {
        if w.plan.is_operator(&a.row.hotkey.0) || operator_owned(a) {
            continue;
        }
        let published = !sql
            .exec(
                "SELECT 1 FROM published_contents WHERE content = ?",
                &[s(a.row.upload.opening.content.hex())],
            )?
            .is_empty();
        if !published {
            outside = true;
            break;
        }
    }
    let mut plan = c.plan.clone();
    let image = plan.reference.image_digest.clone();
    plan.entrants = admitted
        .iter()
        .map(|a| -> R<ArtifactJson> {
            Ok(ArtifactJson {
                owner: IdentityJson {
                    uid: a.entry.uid.ok_or(EvalError::State("uid"))?,
                    hotkey: format!("0x{}", hex::encode(a.row.hotkey.0)),
                    registration_block: a.entry.registration_block.ok_or(EvalError::State("registration"))?,
                },
                contract: spec::CONTRACT_ID.into(),
                package_sha256: a.package_sha256.clone(),
                model_sha256: a.model_sha256.clone(),
                image_digest: image.clone(),
                commitment_block: a.entry.block,
                manifest_key: a.manifest_key.clone(),
                opening_nonce: Some(hex::encode(a.row.upload.opening.nonce.0)),
                extrinsic_index: Some(a.entry.extrinsic),
                object_key: Some(a.header_key.clone()),
            })
        })
        .collect::<R<_>>()?;
    if outside && let Some(fresh) = &w.plan.fresh_pool {
        plan.pool = fresh.clone();
    }
    let bytes = serde_json::to_vec(&plan).map_err(|_| EvalError::State("frozen plan"))?;
    CohortPlan::parse(&bytes, cfg.ceilings).map_err(|e| EvalError::Plan(e.0))?;
    Ok(Ok(Decision {
        refused,
        admitted,
        overflow,
        plan,
        outside,
    }))
}

/// The refusal of a commitment sharing its extrinsic with another hotkey's.
pub fn shared_extrinsic() -> ApiError {
    ApiError::new(
        ErrorCode::CommitmentMismatch,
        "the commitment shares its extrinsic with another hotkey's commitment; commit again in an extrinsic of its own",
    )
    .with(Detail {
        reason: Some(commitments::SHARED_EXTRINSIC.into()),
        ..Default::default()
    })
}

fn apply_freeze(sql: &impl Sql, c: &Cohort, w: &Window, d: Decision, epoch: u64, now_ms: u64) -> R<Boundary> {
    let _ = c;
    for (id, e) in &d.refused {
        set_state(sql, id, SessionState::Refused, Some(e), now_ms)?;
    }
    for a in &d.admitted {
        sql.exec(
            "UPDATE adm_sessions SET state = 'admitted', pos_block = ?, pos_extrinsic = ?, updated_ms = ? WHERE session = ?",
            &[bind_u64(a.entry.block)?, i(i64::from(a.entry.extrinsic)), bind_u64(now_ms)?, s(&a.row.session)],
        )?;
    }
    // An overflowed miner commits again for the standing cohort after this
    // one, never a probe cut's number (`calendar::standing_after`).
    let next_cohort = if d.overflow.is_empty() { 0 } else { crate::eval::calendar::standing_after(sql, w.cohort)? };
    for o in &d.overflow {
        sql.exec(
            "UPDATE adm_sessions SET state = 'overflow', next_cohort = ?, pos_block = ?, pos_extrinsic = ?, updated_ms = ? WHERE session = ?",
            &[bind_u64(next_cohort)?, bind_u64(o.entry.block)?, i(i64::from(o.entry.extrinsic)), bind_u64(now_ms)?, s(&o.row.session)],
        )?;
    }
    let bytes = serde_json::to_vec(&d.plan).map_err(|_| EvalError::State("frozen plan"))?;
    let sha = engine::sha_hex(&bytes);
    sql.exec(
        "UPDATE cohorts SET plan = ?, plan_sha256 = ?, admission_closed_epoch = ? WHERE seq = ?",
        &[
            s(String::from_utf8(bytes).map_err(|_| EvalError::State("frozen plan"))?),
            s(&sha),
            bind_u64(epoch)?,
            i(w.seq),
        ],
    )?;
    sql.exec(
        "UPDATE adm_cohorts SET state = 'frozen', frozen_epoch = ?, outside = ?, admitted = ?, overflow = ? WHERE seq = ?",
        &[bind_u64(epoch)?, i(i64::from(d.outside)), i(d.admitted.len() as i64), i(d.overflow.len() as i64), i(w.seq)],
    )?;
    Ok(Boundary::Frozen {
        admitted: d.admitted.len(),
        overflow: d.overflow.len(),
        outside: d.outside,
    })
}

/// An admission cohort the operator abandoned (`engine::abandon`): its
/// window closes for good, every live session is refused with the reason,
/// and its uploads are cleaned up like any that ended without admission.
pub fn abandoned(sql: &impl Sql, seq: i64, reason: &str, now_ms: u64) -> R<()> {
    let changed = sql.exec(
        "UPDATE adm_cohorts SET state = 'abandoned' WHERE seq = ? AND state IN ('admitting','closing','frozen') RETURNING seq",
        &[i(seq)],
    )?;
    if changed.is_empty() {
        return Ok(());
    }
    let e = ApiError::new(ErrorCode::CheckFailed, "the operator abandoned this cohort").with(Detail {
        check: Some(format!("cohort_abandoned:{reason}")),
        ..Default::default()
    });
    for r in sql.exec(
        "SELECT session FROM adm_sessions WHERE seq = ? AND state IN ('uploading','verifying','verified')",
        &[i(seq)],
    )? {
        let id = text(&r, 0)?.to_owned();
        set_state(sql, &id, SessionState::Refused, Some(&e), now_ms)?;
        sql.exec("UPDATE adm_sessions SET stage = NULL WHERE session = ?", &[s(&id)])?;
    }
    sql.exec(
        "UPDATE adm_verdicts SET state = 'refused', code = 'cohort_abandoned', claim_until_ms = 0, updated_ms = ?
         WHERE state = 'pending' AND label = (SELECT label FROM cohorts WHERE seq = ?)",
        &[bind_u64(now_ms)?, i(seq)],
    )?;
    Ok(())
}

/// Whether a frozen cohort's roster holds an admitted outside entrant (the
/// fresh-stock gate).
pub fn has_outside_entrant(sql: &impl Sql, seq: i64) -> R<bool> {
    Ok(sql
        .exec(
            "SELECT outside FROM adm_cohorts WHERE seq = ? AND state = 'frozen'",
            &[i(seq)],
        )?
        .first()
        .is_some_and(|r| int(r, 0).unwrap_or(0) == 1))
}


// ------------------------------------------------------------- the step

/// Whether admission has work for the cohort object's step, or the calendar
/// has a cohort to open under `cfg` (`calendar::ready`: never for a retired
/// authority). A pure read: status calls it, so it never records an error
/// or pauses the calendar (the step's `calendar::due` does).
///
/// `due_by`: `None` counts every unfinished item (the deploy check and the
/// successor's retirement wait for all of them); `Some(now)` counts a
/// manifest write, an audit, a close sweep step, an object delete or a
/// clean-up only once its backoff has passed, and the paced work (the
/// canary, the alarm sweep, the reconciliation) only when due, so the step
/// alarm is not re-armed for work that is not due. The standing
/// reconciliation and the paced sweep are never unfinished work: they
/// gate nothing.
pub fn has_work(sql: &impl Sql, cfg: &EvalConfig, due_by: Option<u64>) -> R<bool> {
    let now = bind_u64(due_by.unwrap_or(crate::wire::MAX_SAFE_INTEGER))?;
    let unfinished = !sql
        .exec(
            "SELECT 1 FROM adm_sessions WHERE (state = 'verifying'
               AND (stage = 'verdict' OR (stage IN ('manifest','audit') AND step_after_ms <= ?)))
               OR (state = 'admitted' AND stage = 'audit' AND step_after_ms <= ?)
             UNION ALL SELECT 1 FROM adm_cohorts WHERE state = 'closing' AND (close_recorded = 1 OR close_after_ms <= ?)
             UNION ALL SELECT 1 FROM adm_charges
             UNION ALL SELECT 1 FROM adm_object_deletes WHERE not_before_ms <= ?
             UNION ALL SELECT 1 FROM adm_sessions s JOIN adm_cohorts c ON c.seq = s.seq
               WHERE s.cleaned = 0 AND ((s.state IN ('refused','overflow','superseded') AND c.state IN ('frozen','abandoned'))
                 OR (s.state = 'admitted' AND c.state = 'abandoned'))
             LIMIT 1",
            &[now.clone(), now.clone(), now.clone(), now],
        )?
        .is_empty();
    if unfinished || crate::eval::calendar::ready(sql, cfg)? {
        return Ok(true);
    }
    let Some(now) = due_by else { return Ok(false) };
    Ok(canary_due(sql, now)?.is_some() || sweep_due(sql, now)? || reconcile_begin(sql, now)?.is_some())
}

/// Whether an admitted entrant's objects are still retained in a cohort that
/// is not abandoned: its publication not yet recorded, or recorded and its
/// deletes not yet queued (`cleanup_enqueue`). Work a succession waits for
/// (`engine::blocking_work`): a superseded authority is no longer kicked, so
/// objects released after it retired would wait for its alarm alone.
pub fn objects_retained(sql: &impl Sql) -> R<bool> {
    Ok(!sql
        .exec(
            "SELECT 1 FROM adm_sessions s JOIN adm_cohorts c ON c.seq = s.seq
             WHERE s.state = 'admitted' AND s.cleaned = 0 AND c.state <> 'abandoned' LIMIT 1",
            &[],
        )?
        .is_empty())
}

/// The next admission step: the freeze when it can run, the canary, the
/// close sweep, manifest writes and audits, verdict runs, the alarm sweep,
/// deletes, the reconciliation, and the calendar's next cohort.
pub fn next_work(sql: &impl Sql, cfg: &EvalConfig, now_ms: u64) -> R<Option<engine::Work>> {
    outage_recheck(sql, now_ms)?;
    // A freeze moves its cohort to generating; the engine's own work for it
    // follows in the same step.
    if let Some(Boundary::Frozen { .. }) = try_freeze(sql, cfg, now_ms)? {
        return Ok(None);
    }
    cleanup_enqueue(sql, now_ms)?;
    if canary_due(sql, now_ms)?.is_some() {
        return Ok(Some(engine::Work::AdmissionCanary));
    }
    if close_due(sql, now_ms)? {
        return Ok(Some(engine::Work::AdmissionClose));
    }
    if let Some((session, stage)) = verify_due(sql, now_ms)? {
        return Ok(Some(engine::Work::AdmissionVerify { session, stage }));
    }
    let runs = sql.exec(
        "SELECT 1 FROM adm_verdicts v WHERE v.state = 'pending' AND v.claim_until_ms <= ? AND v.not_before_ms <= ?
           AND EXISTS (SELECT 1 FROM adm_sessions s WHERE s.hotkey = v.hotkey AND s.commitment = v.commitment
                       AND s.state = 'verifying' AND s.stage = 'verdict') LIMIT 1",
        &[bind_u64(now_ms)?, bind_u64(now_ms)?],
    )?;
    if !runs.is_empty() {
        return Ok(Some(engine::Work::AdmissionLanes));
    }
    if sweep_due(sql, now_ms)? {
        return Ok(Some(engine::Work::AdmissionSweep));
    }
    if !object_deletes_due(sql, 1, now_ms)?.is_empty() {
        return Ok(Some(engine::Work::AdmissionCleanup));
    }
    if let Some((label, plan)) = crate::eval::calendar::due(sql, cfg)? {
        return Ok(Some(engine::Work::OpenNext { label, plan }));
    }
    if reconcile_begin(sql, now_ms)?.is_some() {
        return Ok(Some(engine::Work::AdmissionReconcile));
    }
    Ok(None)
}

/// The admitted entrants' artifacts: each header key in the admission
/// bucket (its files are `f/<sha256>` beside it), the package and its
/// manifest, for the publisher (`closure-artifacts.json` beside the
/// closure).
pub fn closure_artifacts(plan: &CohortPlan) -> Value {
    let list: Vec<Value> = plan
        .entrants
        .iter()
        .filter_map(|a| {
            Some(
                json!({"hotkey": a.owner.hotkey.trim_start_matches("0x"), "header_key": a.object_key.as_deref()?,
                "package_sha256": a.package_sha256, "manifest_key": a.manifest_key}),
            )
        })
        .collect();
    json!({"schema": "slop_ninja.admission.closure_artifacts/v2", "artifacts": list})
}

/// Content-free admission status for `/eval/status`.
pub fn status(sql: &impl Sql) -> R<Value> {
    let cohorts: Vec<Value> = sql
        .exec("SELECT seq, cohort, state, open_block, cutoff_block, frozen_epoch, outside, admitted, overflow, close_recorded,
                want_cutoff FROM adm_cohorts ORDER BY seq DESC LIMIT 8", &[])?
        .iter()
        .map(|r| {
            let seq = int(r, 0)?;
            let states: Vec<Value> = sql
                .exec("SELECT state, COUNT(*) FROM adm_sessions WHERE seq = ? GROUP BY state ORDER BY state", &[i(seq)])?
                .iter()
                .map(|x| Ok(json!({"state": text(x, 0)?, "count": int(x, 1)?})))
                .collect::<R<_>>()?;
            Ok(json!({"cohort": int(r, 1)?, "state": text(r, 2)?, "open_block": int(r, 3)?, "cutoff_block": opt_int(r, 4),
                "frozen_epoch": opt_int(r, 5), "outside_entrant": int(r, 6)? == 1, "admitted": int(r, 7)?, "overflow": int(r, 8)?,
                "close_recorded": int(r, 9)? == 1, "close_wanted_at": opt_int(r, 10), "sessions": states}))
        })
        .collect::<R<_>>()?;
    let verdicts: Vec<Value> = sql
        .exec(
            "SELECT state, COUNT(*) FROM adm_verdicts GROUP BY state ORDER BY state",
            &[],
        )?
        .iter()
        .map(|r| Ok(json!({"state": text(r, 0)?, "count": int(r, 1)?})))
        .collect::<R<_>>()?;
    // Pending verdicts that met platform failures, for alerting: the most
    // tries one has taken, its latest code, and how many there are.
    let platform = sql.exec(
        "SELECT COUNT(*), COALESCE(MAX(platform_tries), 0) FROM adm_verdicts WHERE state = 'pending' AND platform_tries > 0",
        &[],
    )?;
    let worst = sql.exec(
        "SELECT code FROM adm_verdicts WHERE state = 'pending' AND platform_tries > 0 ORDER BY platform_tries DESC LIMIT 1",
        &[],
    )?;
    let stalled = json!({
        "verdicts": platform.first().map_or(Ok(0), |r| int(r, 0))?,
        "max_platform_tries": platform.first().map_or(Ok(0), |r| int(r, 1))?,
        "latest_code": worst.first().and_then(|r| opt_text(r, 0)),
        // Sessions whose manifest write or audit met a failure (only a
        // failure arms their backoff).
        "steps_retrying": count(
            sql,
            "SELECT COUNT(*) FROM adm_sessions WHERE state = 'verifying' AND stage IN ('manifest','audit') AND step_after_ms > 0",
            &[],
        )?,
    });
    // The uploaded objects the store still tracks (the pre-deploy check of a
    // bucket change: none may be left in progress or awaiting deletion), the
    // mint aggregates (spend attribution: URLs minted per session, never a
    // cap), and the objects awaiting repair.
    let objects = json!({
        "in_progress": count(
            sql,
            "SELECT COUNT(*) FROM adm_sessions WHERE state IN ('uploading','verifying','verified')",
            &[],
        )?,
        "uncleaned": count(sql, "SELECT COUNT(*) FROM adm_sessions WHERE cleaned = 0 AND state <> 'admitted'", &[])?,
        "pending_object_deletes": count(sql, "SELECT COUNT(*) FROM adm_object_deletes", &[])?,
        "repair_needed": count(sql, "SELECT COUNT(*) FROM adm_objects WHERE missing = 1", &[])?,
        // Admitted entrants whose loads reported an object missing or
        // changed while an audit found every object intact
        // (OBJECT_READ_FAULT: the read path's failure, counted by the lane).
        "object_read_faults": count(sql, "SELECT COUNT(*) FROM adm_sessions WHERE state = 'admitted' AND read_faults > 0", &[])?,
        "urls_minted": count(sql, "SELECT COALESCE(SUM(mints), 0) FROM adm_sessions", &[])?,
        "max_urls_one_session": count(sql, "SELECT COALESCE(MAX(mints), 0) FROM adm_sessions", &[])?,
    });
    Ok(
        json!({"contract": CONTRACT, "cohorts": cohorts, "verdicts": verdicts, "platform_retries": stalled,
        "objects": objects, "upload_path": platform_status(sql)?, "commitments": commitments::status(sql)?}),
    )
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
