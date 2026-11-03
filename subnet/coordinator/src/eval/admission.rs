//! Coordinator admission of a reward-v1 cohort's roster: contract
//! `slop_ninja.admission/v1` (`docs/testnet/admission-api.md`), as
//! storage-neutral functions over [`Sql`] that the `EvalCohort` Durable
//! Object runs, one transaction each, and the eval Worker's routes drive.
//!
//! - **Window.** A v3 cohort whose plan has an `admission` block admits from
//!   its opening boundary (block `open_block`) until the boundary at which
//!   admission closes (`cutoff_block`). Its object bound, part size, part
//!   count, validity window and roster cap are plan parameters; the object
//!   bound is derived from the plan's artifact spec ([`SpecBounds`]).
//! - **Requests** are signed by the hotkey they concern ([`authenticate`]),
//!   bound to the chain, method, path, body digest and a block bound checked
//!   against the RPC-confirmed cursor.
//! - **Sessions** follow the contract's state machine: one session per
//!   hotkey and commitment (so a hotkey's sessions are bounded by its
//!   commitments), attempts, parts recorded with their R2 tags, the last
//!   refused request per hotkey and cohort.
//! - **Verification** is a chain of durable steps of the cohort object, each
//!   resumed after an eviction or redeploy: the multipart completion
//!   (`complete`, re-issued until R2's outcome is recorded), framing
//!   (`frame`) and the blob copies (`blobs`). Framing ([`Framer`]) is the
//!   protocol `ArtifactReader` (the header record, decoded once and bounded
//!   by `MAX_ARTIFACT_HEADER_LEN`; object and file digests and lengths), the
//!   spec's header-only file-table checks, the content-addressed slices and
//!   parts manifest the scorer loads, and `SubmissionUpload::admit` against
//!   the logged commitment. Framing attempts are counted, and an upload whose
//!   framing keeps being cut off is refused. No graph or role-file parser of
//!   untrusted bytes runs here; the header record is the one decoded
//!   structure (see the README's open item on the key-holding Worker).
//! - **Verdicts** come from an admission run on the scorer Container: one
//!   run per hotkey and finalized commitment, bound to the scorer binary and
//!   the cohort's spec and recorded as reached on them (rule (b)),
//!   definitive only on a `refused <code>`; attributable load failures and a
//!   failed control document on fresh instances count toward the plan's
//!   `artifact_failures_before_model_error`; platform failures retry with a
//!   growing backoff and show in the status.
//! - **Cutoff.** At the boundary where the admission minimum is reached the
//!   window closes (no new sessions, parts or completions). The roster
//!   freezes at that boundary, or at the boundary after its work is in: every
//!   completed upload verified, every verdict in, the commitment snapshot at
//!   `cutoff_block - 1` read. Sessions whose commitment a later one replaced,
//!   whose registration changed or whose commitment shares its extrinsic are
//!   refused, the rest are ranked by `(block, extrinsic)`, the first
//!   `roster_cap` admitted and the rest queued as overflow. The admitted
//!   roster is written into the stored plan (its entrants, with their
//!   openings), which the seal binds. A frozen plan the engine refuses faults
//!   the cohort for the operator. Fresh stock is drawn only when the roster
//!   holds an admitted outside entrant with an artifact of its own.
//! - **Way out.** An operator can refuse one stalled session with a recorded
//!   reason ([`operator_refuse`]) and abandon a faulted admission cohort
//!   ([`abandoned`]); nothing is skipped silently.
//!
//! Objects and openings stay in the private bucket; the closure publishes the
//! admitted entrants' openings and names their objects after the close.
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
    NextCohort, PartAck, PartRecord, Position, Refusal, Registration, SCHEMA_COHORT, SCHEMA_HOTKEY, SCHEMA_SESSION,
    Session, SessionSummary,
};
use slop_ninja_admission_wire::{CONTRACT, ErrorCode, PartPlan, RequestScope, SessionState, request_message};
use slop_ninja_artifact_check::mapping::{self, Part};
use slop_ninja_artifact_check::spec::{self, Version};
use slop_ninja_protocol::submission::{FinalizedCommitment, SubmissionUpload, parse_chain_payload};
use slop_ninja_protocol::{
    AccountId, ArtifactReader, ChainBinding, Digest, Error as ProtocolError, MAX_ARTIFACT_HEADER_LEN, NeuronIdentity,
    Record, VerifiedArtifact,
};

/// The single integer a `COUNT(*)`-style query returns.
fn count(sql: &impl Sql, query: &str, params: &[SqlValue]) -> R<i64> {
    Ok(sql.exec(query, params)?.first().map_or(Ok(0), |r| int(r, 0))?)
}

// ------------------------------------------------------------ spec bounds

/// The bounds admission derives from an artifact spec the scorer admits by
/// (spec v2 or v3): the largest file total and the largest object any
/// artifact of the spec can have. Nothing here is tuned: the graph term is
/// the largest served class's fixed length, the tokenizer is pinned, the
/// JSON role files share the spec's own budget, and the header is bounded by
/// the protocol record's limit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SpecBounds {
    pub spec_id: &'static str,
    pub spec_sha256: String,
    /// Largest graph class length the spec serves.
    pub graph_bytes: u64,
    /// The six files' largest total: graph, tokenizer and the JSON budget.
    pub files_bytes: u64,
    /// `4 + MAX_ARTIFACT_HEADER_LEN + files_bytes`.
    pub object_bytes: u64,
}

impl SpecBounds {
    pub fn of(v: Version) -> Option<Self> {
        if !engine_admits(v) {
            return None;
        }
        let graph = graph_bytes(v)?;
        let files = files_bytes(v)?;
        let object = files.checked_add(4 + u64::from(MAX_ARTIFACT_HEADER_LEN))?;
        Some(Self {
            spec_id: v.id(),
            spec_sha256: v.spec_sha256(),
            graph_bytes: graph,
            files_bytes: files,
            object_bytes: object,
        })
    }

    pub fn of_id(id: &str) -> Option<Self> {
        Version::from_id(id).and_then(Self::of)
    }
}

/// Largest graph class length spec `v` serves.
fn graph_bytes(v: Version) -> Option<u64> {
    v.classes().iter().filter(|c| v.serves(c)).map(|c| c.bytes).max()
}

/// The six files' largest total under spec `v` (graph, tokenizer and the JSON
/// budget), from its class and JSON bounds alone: unlike the spec digest it
/// never rebuilds the spec record, so a lane's limits read it per report.
pub fn files_bytes(v: Version) -> Option<u64> {
    if !engine_admits(v) {
        return None;
    }
    graph_bytes(v)?.checked_add(spec::TOKENIZER_BYTES)?.checked_add(v.max_json_bytes()?)
}

/// The specs an admission run can load by: every run names its cohort's spec
/// to the scorer's probe route ([`LaneRun::body`]), as the cohort's scoring
/// loads do, so a spec v3 cohort admits the span tower class by spec v3.
pub const ADMISSION_SPECS: &[&str] = &[spec::SPEC_ID_V2, spec::SPEC_ID_V3];

/// Specs the scorer admits a load by: those that bound the JSON role files.
fn engine_admits(v: Version) -> bool {
    v.max_json_bytes().is_some() && v.max_json_value_bytes().is_some()
}

/// The cohort's limits record (content-free), published by digest in the
/// cohort response; it changes only with the plan or the spec.
pub fn limits_json(p: &AdmissionPlan, b: &SpecBounds) -> Value {
    json!({"schema": "slop_ninja.admission.limits/v1", "contract": CONTRACT, "artifact_spec": b.spec_id,
        "artifact_spec_sha256": b.spec_sha256, "graph_bytes": b.graph_bytes, "files_bytes": b.files_bytes,
        "max_object_len": b.object_bytes, "part_size": p.part_size, "max_parts": p.max_parts,
        "request_validity_blocks": p.request_validity_blocks, "roster_cap": p.roster_cap, "selection": p.selection})
}

pub fn limits_digest(p: &AdmissionPlan, b: &SpecBounds) -> String {
    hex::encode(
        slop_ninja_protocol::domain_hash(
            "slop_ninja.admission/limits/v1",
            limits_json(p, b).to_string().as_bytes(),
        )
        .0,
    )
}

// ---------------------------------------------------------------- schema

/// v10: admission sessions, parts, last refusals, verdicts, cohort windows,
/// the cutoff snapshot, the blobs each session copied, and the standing
/// template (the commitment log is v11, `commitments::SCHEMA`).
///
/// - `adm_sessions.step_tries`, `step_after_ms`: the durable completion and
///   framing steps' attempts that ended without an outcome, and when the
///   next one may run.
/// - `adm_verdicts.reached_binary`, `reached_spec`: the scorer build and spec
///   a verdict was reached on (rule (b)), apart from the build a pending run
///   is bound to; `platform_tries` counts platform failures (backoff, alert).
/// - `adm_blobs`: each slice a session copied, and whether its copy created
///   the blob, so the blobs of uploads that end without admission go too.
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

/// A reply the Worker sends as is: HTTP status and JSON body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
    #[serde(default)]
    pub retry_after: Option<u64>,
}

impl Reply {
    fn ok<T: Serialize>(status: u16, v: &T) -> Self {
        Self {
            status,
            body: serde_json::to_value(v).unwrap_or(Value::Null),
            retry_after: None,
        }
    }
    pub fn error(e: &ApiError) -> Self {
        Self {
            status: e.status(),
            body: serde_json::to_value(e.body()).unwrap_or(Value::Null),
            retry_after: e.retry_after,
        }
    }
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
}

impl Window {
    pub fn admitting(&self) -> bool {
        self.state == "admitting"
    }
    pub fn part_plan(&self, len: u64) -> Option<PartPlan> {
        PartPlan::new(len, self.plan.part_size, self.plan.max_parts).ok()
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
    sql.exec(
        "INSERT OR IGNORE INTO adm_cohorts (seq, cohort, state, open_block) VALUES (?, ?, 'admitting', ?)",
        &[i(c.seq), bind_u64(c.plan.cohort)?, bind_u64(open_block)?],
    )?;
    Ok(())
}

// --------------------------------------------------------- authentication

/// A signed request as the Worker forwards it: method and path exactly as
/// sent, the four signature headers, and for a body read whole, its SHA-256
/// (a part's digest is the claimed one; the part streams to R2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signed {
    pub method: String,
    pub path: String,
    pub hotkey: Option<String>,
    pub valid_until: Option<String>,
    pub content_sha256: Option<String>,
    pub signature: Option<String>,
    /// SHA-256 of a body the Worker read whole (hex); `None` for a part.
    #[serde(default)]
    pub body_sha256: Option<String>,
    /// A small body, hex (session creation only).
    #[serde(default)]
    pub body: Option<String>,
}

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
    cohort: u64,
    hotkey: AccountId,
    upload: SubmissionUpload,
    attempt: u32,
    state: SessionState,
    refusal: Option<Refusal>,
    position: Option<Position>,
    next_cohort: Option<u64>,
    part_size: u64,
    part_count: u32,
    object_key: String,
    upload_id: Option<String>,
    stage: Option<String>,
}

const SESSION_COLUMNS: &str = "session, seq, cohort, hotkey, upload, attempt, state, refusal, pos_block, pos_extrinsic, \
    next_cohort, part_size, part_count, object_key, upload_id, stage";

fn row_of(r: &crate::sql::Row) -> R<Row> {
    let upload_bytes = hex::decode(text(r, 4)?).map_err(|_| EvalError::State("session upload"))?;
    let upload = SubmissionUpload::from_bytes(&upload_bytes).map_err(|_| EvalError::State("session upload"))?;
    let hk = crate::eval::plan::parse_hex32(text(r, 3)?).ok_or(EvalError::State("session hotkey"))?;
    Ok(Row {
        session: text(r, 0)?.to_owned(),
        cohort: uint(r, 2)?,
        hotkey: AccountId(hk),
        upload,
        attempt: u32::try_from(int(r, 5)?).map_err(|_| EvalError::State("attempt"))?,
        state: SessionState::parse(text(r, 6)?).ok_or(EvalError::State("session state"))?,
        refusal: opt_text(r, 7).and_then(|t| serde_json::from_str(&t).ok()),
        position: match (opt_int(r, 8), opt_int(r, 9)) {
            (Some(b), Some(x)) => Some(Position {
                block: b as u64,
                extrinsic: x as u32,
            }),
            _ => None,
        },
        next_cohort: opt_int(r, 10).map(|v| v as u64),
        part_size: uint(r, 11)?,
        part_count: u32::try_from(int(r, 12)?).map_err(|_| EvalError::State("part count"))?,
        object_key: text(r, 13)?.to_owned(),
        upload_id: opt_text(r, 14),
        stage: opt_text(r, 15),
    })
}

fn session_row(sql: &impl Sql, id: &str) -> R<Option<Row>> {
    let rows = sql.exec(
        &format!("SELECT {SESSION_COLUMNS} FROM adm_sessions WHERE session = ?"),
        &[s(id)],
    )?;
    rows.first().map(row_of).transpose()
}

fn parts_of(sql: &impl Sql, id: &str, attempt: u32) -> R<Vec<(u32, u64, String, String)>> {
    sql.exec(
        "SELECT part, len, sha256, etag FROM adm_parts WHERE session = ? AND attempt = ? ORDER BY part",
        &[s(id), i(i64::from(attempt))],
    )?
    .iter()
    .map(|r| {
        Ok((
            u32::try_from(int(r, 0)?).map_err(|_| EvalError::State("part"))?,
            uint(r, 1)?,
            text(r, 2)?.to_owned(),
            text(r, 3)?.to_owned(),
        ))
    })
    .collect()
}

fn ss58(a: &AccountId) -> String {
    crate::chain::ss58(a.0)
}

/// The session's JSON (`slop_ninja.admission.session/v1`).
fn session_json(sql: &impl Sql, r: &Row, cursor: u64) -> R<Session> {
    let parts = parts_of(sql, &r.session, r.attempt)?;
    Ok(Session {
        schema: SCHEMA_SESSION.into(),
        session: r.session.clone(),
        cohort: r.cohort,
        hotkey: ss58(&r.hotkey),
        commitment: r.upload.commitment.hex(),
        artifact_sha256: r.upload.artifact_sha256.hex(),
        artifact_len: r.upload.artifact_len,
        part_size: r.part_size,
        part_count: r.part_count,
        attempt: r.attempt,
        parts: parts
            .into_iter()
            .map(|(part, len, sha256, _)| PartRecord { part, len, sha256 })
            .collect(),
        state: r.state.as_str().into(),
        refusal: r.refusal.clone(),
        position: r.position,
        next_cohort: r.next_cohort,
        finalized_block: cursor,
    })
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

fn record_refusal(
    sql: &impl Sql,
    cursor: u64,
    signer: &AccountId,
    window: Option<&Window>,
    upload: Option<&SubmissionUpload>,
    e: &ApiError,
) -> R<()> {
    let opening_cohort = upload.map(|u| u.opening.cohort);
    let cohort = match window {
        Some(w) if w.admitting() => w.cohort,
        _ => match opening_cohort {
            Some(c) => c,
            None => return Ok(()),
        },
    };
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

/// The finalized-commitment check shared by opening, reopening and
/// completion: window, registration and opening. Returns the position.
fn check_commitment(
    sql: &impl Sql,
    chain: &ChainBinding,
    w: &Window,
    upload: &SubmissionUpload,
) -> A<(Position, LogEntry)> {
    let opening = &upload.opening;
    let Some(entry) = counted(sql, w, &opening.hotkey)? else {
        let msg = if commitments::unresolved(sql, &opening.hotkey)? {
            "the hotkey's latest commitment is not logged yet"
        } else {
            "no finalized commitment of this hotkey is logged"
        };
        let mut e = ApiError::new(ErrorCode::CommitmentNotFound, msg);
        e.retry_after = Some(60);
        return Err(e);
    };
    if commitments::unresolved(sql, &opening.hotkey)? && w.cutoff_block.is_none() {
        // A newer commitment was seen and is still being logged: the
        // opening's commitment may already be overwritten.
        let mut e = ApiError::new(
            ErrorCode::CommitmentNotFound,
            "the hotkey's latest commitment is not logged yet",
        );
        e.retry_after = Some(60);
        return Err(e);
    }
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
            entry,
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

/// The checks shared by opening and reopening a session, in contract order.
fn admission_checks(
    sql: &impl Sql,
    chain: &ChainBinding,
    w: Option<&Window>,
    signer: &AccountId,
    upload: &SubmissionUpload,
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
                next: crate::eval::calendar::next(sql)?.map(|(cohort, earliest_open_epoch)| NextCohort {
                    cohort,
                    earliest_open_epoch,
                }),
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
    if upload.artifact_len > w.bounds.object_bytes || w.part_plan(upload.artifact_len).is_none() {
        return Err(ApiError::new(
            ErrorCode::ObjectTooLarge,
            "artifact_len exceeds max_object_len or max_parts parts",
        ));
    }
    check_commitment(sql, chain, w, upload)?;
    Ok(w.clone())
}

/// The one-artifact rule: one session per commitment (another envelope for
/// a commitment that already has a session, in any state, is refused: the
/// miner reopens that session or commits again), so a hotkey's sessions are
/// bounded by its commitments; live sessions of older commitments are
/// superseded. Returns the object keys of superseded attempts (their uploads
/// are aborted).
fn one_artifact(
    sql: &impl Sql,
    w: &Window,
    signer: &AccountId,
    id: &str,
    upload: &SubmissionUpload,
    now_ms: u64,
) -> A<Vec<(String, Option<String>)>> {
    let same = sql.exec(
        "SELECT session FROM adm_sessions WHERE cohort = ? AND hotkey = ? AND commitment = ? AND session != ?
           AND state NOT IN ('uploading','verifying','verified') LIMIT 1",
        &[
            bind_u64(w.cohort)?,
            s(hex::encode(signer.0)),
            s(upload.commitment.hex()),
            s(id),
        ],
    )?;
    if let Some(r) = same.first() {
        return Err(ApiError::new(
            ErrorCode::SecondArtifact,
            "this commitment already has a session: reopen it, or commit again to submit another artifact",
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
    if let Some(other) = live.iter().find(|r| r.upload.commitment == upload.commitment) {
        return Err(ApiError::new(
            ErrorCode::SecondArtifact,
            "another live session exists for this hotkey and commitment",
        )
        .with(Detail {
            session: Some(other.session.clone()),
            ..Default::default()
        }));
    }
    let mut cleanup = Vec::new();
    for other in live {
        set_state(sql, &other.session, SessionState::Superseded, None, now_ms)?;
        sql.exec("DELETE FROM adm_parts WHERE session = ?", &[s(&other.session)])?;
        cleanup.push((other.object_key, other.upload_id));
    }
    Ok(cleanup)
}

/// R2 work a session operation leaves for the Worker or the Durable Object,
/// done after its transaction commits (create, abort, complete a multipart
/// upload, delete an object).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effects {
    /// Create a multipart upload at this key for (session, attempt).
    pub create: Option<(String, String, u32)>,
    /// Abort these multipart uploads `(key, upload_id)` and delete the keys.
    pub abort: Vec<(String, Option<String>)>,
    /// Complete this multipart upload.
    pub complete: Option<Completion>,
}

/// A multipart completion: `(key, upload_id, parts (number, etag))`.
pub type Completion = (String, String, Vec<(u16, String)>);

/// The object key of a session attempt (private; never served).
pub fn object_key(label: &str, session: &str, attempt: u32) -> String {
    format!("admission/{label}/{session}/a{attempt}.object")
}

/// `POST /admission/v1/sessions`.
pub fn create(sql: &impl Sql, chain: &ChainBinding, req: &Signed, now_ms: u64) -> (Reply, Effects) {
    match create_inner(sql, chain, req, now_ms) {
        Ok(v) => v,
        Err(e) => (Reply::error(&e), Effects::default()),
    }
}

fn create_inner(sql: &impl Sql, chain: &ChainBinding, req: &Signed, now_ms: u64) -> A<(Reply, Effects)> {
    let (signer, _) = signer(sql, chain, req)?;
    let cursor = cursor(sql)?.unwrap_or(0);
    let w = window(sql)?;
    let body = req
        .body
        .as_deref()
        .and_then(|b| hex::decode(b).ok())
        .unwrap_or_default();
    let Ok(upload) = SubmissionUpload::from_bytes(&body) else {
        let e = ApiError::new(ErrorCode::BadRequest, "the body is not a submission-upload record");
        record_refusal(sql, cursor, &signer, w.as_ref(), None, &e)?;
        return Err(e);
    };
    let id = upload
        .digest()
        .map_err(|_| ApiError::new(ErrorCode::Internal, "digest"))?
        .hex();
    // The same envelope again (a retried creation) is answered with its
    // session before any admission check: the checks ran when it was
    // created, and a later commitment changes nothing about this one.
    if let Some(existing) = session_row(sql, &id)?
        && existing.hotkey == signer
    {
        return Ok((
            Reply::ok(200, &session_json(sql, &existing, cursor)?),
            Effects::default(),
        ));
    }
    let fail = |e: ApiError| -> A<(Reply, Effects)> {
        record_refusal(sql, cursor, &signer, w.as_ref(), Some(&upload), &e)?;
        Err(e)
    };
    let w = match admission_checks(sql, chain, w.as_ref(), &signer, &upload) {
        Ok(w) => w,
        Err(e) => return fail(e),
    };
    let abort = match one_artifact(sql, &w, &signer, &id, &upload, now_ms) {
        Ok(a) => a,
        Err(e) => return fail(e),
    };
    let plan = w
        .part_plan(upload.artifact_len)
        .ok_or_else(|| ApiError::new(ErrorCode::ObjectTooLarge, "parts"))?;
    let key = object_key(&w.label, &id, 1);
    let bytes = upload
        .to_bytes()
        .map_err(|_| ApiError::new(ErrorCode::Internal, "upload bytes"))?;
    sql.exec(
        "INSERT INTO adm_sessions (session, seq, cohort, hotkey, upload, commitment, content, artifact_sha256, artifact_len,
            part_size, part_count, attempt, state, object_key, created_ms, updated_ms)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 'uploading', ?, ?, ?)",
        &[
            s(&id),
            i(w.seq),
            bind_u64(w.cohort)?,
            s(hex::encode(signer.0)),
            s(hex::encode(bytes)),
            s(upload.commitment.hex()),
            s(upload.opening.content.hex()),
            s(upload.artifact_sha256.hex()),
            bind_u64(upload.artifact_len)?,
            bind_u64(plan.part_size())?,
            i(i64::from(plan.count())),
            s(&key),
            bind_u64(now_ms)?,
            bind_u64(now_ms)?,
        ],
    )?;
    let row = session_row(sql, &id)?.ok_or_else(|| ApiError::new(ErrorCode::Internal, "session"))?;
    Ok((
        Reply::ok(201, &session_json(sql, &row, cursor)?),
        Effects {
            create: Some((key, id, 1)),
            abort,
            complete: None,
        },
    ))
}

/// A session attempt whose multipart upload is not created yet (the
/// creation after its transaction failed): `(key, attempt)`.
pub fn upload_missing(sql: &impl Sql, id: &str) -> R<Option<(String, u32)>> {
    let rows = sql.exec(
        "SELECT object_key, attempt FROM adm_sessions WHERE session = ? AND state = 'uploading' AND upload_id IS NULL",
        &[s(id)],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    Ok(Some((
        text(r, 0)?.to_owned(),
        u32::try_from(int(r, 1)?).map_err(|_| EvalError::State("attempt"))?,
    )))
}

/// Stores the multipart upload id of a session attempt, once created.
pub fn set_upload_id(sql: &impl Sql, id: &str, attempt: u32, upload_id: &str) -> R<()> {
    sql.exec(
        "UPDATE adm_sessions SET upload_id = ? WHERE session = ? AND attempt = ? AND upload_id IS NULL",
        &[s(upload_id), s(id), i(i64::from(attempt))],
    )?;
    Ok(())
}

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

fn closed_for(sql: &impl Sql, r: &Row) -> R<bool> {
    Ok(window_for(sql, r.cohort)?.is_none_or(|w| !w.admitting()))
}

/// `GET /admission/v1/sessions/{session}`.
pub fn read(sql: &impl Sql, chain: &ChainBinding, req: &Signed, id: &str) -> Reply {
    let go = || -> A<Reply> {
        let (signer, _) = signer(sql, chain, req)?;
        let r = owned(sql, &signer, id)?;
        Ok(Reply::ok(200, &session_json(sql, &r, cursor(sql)?.unwrap_or(0))?))
    };
    go().unwrap_or_else(|e| Reply::error(&e))
}

/// What the Worker needs to stream one part: the object key, the multipart
/// upload id and the part's exact length.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartTarget {
    pub key: String,
    pub upload_id: String,
    pub attempt: u32,
    pub part: u32,
    pub len: u64,
    pub sha256: String,
}

/// `PUT .../parts/{n}`, before the body streams: authenticates and checks the
/// session, the part number and the declared length.
pub fn part_begin(
    sql: &impl Sql,
    chain: &ChainBinding,
    req: &Signed,
    id: &str,
    part: u32,
    content_length: Option<u64>,
) -> Result<PartTarget, Reply> {
    let go = || -> A<PartTarget> {
        let (signer, content) = signer(sql, chain, req)?;
        let r = owned(sql, &signer, id)?;
        if closed_for(sql, &r)? {
            return Err(ApiError::new(
                ErrorCode::AfterCutoff,
                "admission has closed for this cohort",
            ));
        }
        if r.state != SessionState::Uploading {
            return Err(ApiError::new(
                ErrorCode::SessionNotUploading,
                "the session no longer accepts parts",
            ));
        }
        let plan = PartPlan::new(r.upload.artifact_len, r.part_size, r.part_count.max(1))
            .map_err(|_| ApiError::new(ErrorCode::Internal, "plan"))?;
        let Some((_, len)) = plan.range(part) else {
            return Err(ApiError::new(ErrorCode::PartOutOfRange, "no such part"));
        };
        if content_length != Some(len) {
            return Err(ApiError::new(
                ErrorCode::PartLengthMismatch,
                "the part is not exactly its expected length",
            ));
        }
        let upload_id = r.upload_id.clone().ok_or_else(|| {
            let mut e = ApiError::new(ErrorCode::Unavailable, "the upload is being created");
            e.retry_after = Some(2);
            e
        })?;
        Ok(PartTarget {
            key: r.object_key,
            upload_id,
            attempt: r.attempt,
            part,
            len,
            sha256: hex::encode(content),
        })
    };
    go().map_err(|e| Reply::error(&e))
}

/// Records a streamed part (after R2 accepted it). The latest upload of a
/// part counts. Refused with `after_cutoff` when the close came first.
pub fn part_record(sql: &impl Sql, id: &str, t: &PartTarget, etag: &str) -> Reply {
    let go = || -> A<Reply> {
        let r = session_row(sql, id)?.ok_or_else(|| ApiError::new(ErrorCode::NotFound, "unknown session"))?;
        if closed_for(sql, &r)? {
            return Err(ApiError::new(
                ErrorCode::AfterCutoff,
                "admission has closed for this cohort",
            ));
        }
        if r.state != SessionState::Uploading || r.attempt != t.attempt {
            return Err(ApiError::new(
                ErrorCode::SessionNotUploading,
                "the session no longer accepts parts",
            ));
        }
        sql.exec(
            "INSERT INTO adm_parts (session, attempt, part, len, sha256, etag) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (session, attempt, part) DO UPDATE SET len = excluded.len, sha256 = excluded.sha256, etag = excluded.etag",
            &[s(id), i(i64::from(t.attempt)), i(i64::from(t.part)), bind_u64(t.len)?, s(&t.sha256), s(etag)],
        )?;
        Ok(Reply::ok(
            200,
            &PartAck {
                part: t.part,
                len: t.len,
                sha256: t.sha256.clone(),
            },
        ))
    };
    go().unwrap_or_else(|e| Reply::error(&e))
}

/// `POST .../complete`: requires every part, then hands the multipart
/// completion to the caller and moves the session to `verifying`.
pub fn complete(sql: &impl Sql, chain: &ChainBinding, req: &Signed, id: &str, now_ms: u64) -> (Reply, Effects) {
    let go = || -> A<(Reply, Effects)> {
        let (signer, _) = signer(sql, chain, req)?;
        let r = owned(sql, &signer, id)?;
        let cursor = cursor(sql)?.unwrap_or(0);
        if closed_for(sql, &r)? {
            return Err(ApiError::new(
                ErrorCode::AfterCutoff,
                "admission has closed for this cohort",
            ));
        }
        match r.state {
            SessionState::Uploading => {}
            // A completion not yet confirmed is issued again (R2's complete
            // of the same parts is idempotent in effect: a completed upload
            // is recognised by its object, `completion_target`).
            SessionState::Verifying if r.stage.as_deref() == Some("complete") => {
                let fx = Effects {
                    complete: completion_target(sql, id)?.map(|(c, _)| c),
                    ..Default::default()
                };
                return Ok((Reply::ok(202, &session_json(sql, &r, cursor)?), fx));
            }
            SessionState::Verifying | SessionState::Verified => {
                return Ok((Reply::ok(202, &session_json(sql, &r, cursor)?), Effects::default()));
            }
            _ => {
                return Err(ApiError::new(
                    ErrorCode::SessionNotUploading,
                    "the session no longer accepts a completion",
                ));
            }
        }
        let parts = parts_of(sql, id, r.attempt)?;
        let plan = PartPlan::new(r.upload.artifact_len, r.part_size, r.part_count.max(1))
            .map_err(|_| ApiError::new(ErrorCode::Internal, "plan"))?;
        let missing: Vec<u32> = plan.missing(|n| parts.iter().any(|p| p.0 == n)).collect();
        if !missing.is_empty() {
            let mut e = ApiError::new(ErrorCode::PartsMissing, "completion before every part").with(Detail {
                missing: Some(missing),
                ..Default::default()
            });
            e.retry_after = Some(1);
            return Err(e);
        }
        let upload_id = r
            .upload_id
            .clone()
            .ok_or_else(|| ApiError::new(ErrorCode::Unavailable, "upload"))?;
        let tags = parts
            .iter()
            .map(|p| {
                Ok((
                    u16::try_from(p.0).map_err(|_| ApiError::new(ErrorCode::Internal, "part"))?,
                    p.3.clone(),
                ))
            })
            .collect::<A<Vec<_>>>()?;
        sql.exec(
            "UPDATE adm_sessions SET state = 'verifying', stage = 'complete', updated_ms = ? WHERE session = ?",
            &[bind_u64(now_ms)?, s(id)],
        )?;
        let r = session_row(sql, id)?.ok_or_else(|| ApiError::new(ErrorCode::Internal, "session"))?;
        Ok((
            Reply::ok(202, &session_json(sql, &r, cursor)?),
            Effects {
                complete: Some((r.object_key, upload_id, tags)),
                ..Default::default()
            },
        ))
    };
    go().unwrap_or_else(|e| (Reply::error(&e), Effects::default()))
}

/// The multipart completion of a session whose stage is `complete` (the
/// completion is durable work until its outcome is recorded), and the
/// envelope's object length.
pub fn completion_target(sql: &impl Sql, id: &str) -> R<Option<(Completion, u64)>> {
    let Some(r) = session_row(sql, id)? else {
        return Ok(None);
    };
    if r.state != SessionState::Verifying || r.stage.as_deref() != Some("complete") {
        return Ok(None);
    }
    let Some(upload_id) = r.upload_id.clone() else {
        return Ok(None);
    };
    let tags = parts_of(sql, id, r.attempt)?
        .into_iter()
        .map(|p| Ok((u16::try_from(p.0).map_err(|_| EvalError::State("part"))?, p.3)))
        .collect::<R<Vec<_>>>()?;
    Ok(Some(((r.object_key, upload_id, tags), r.upload.artifact_len)))
}

/// After the multipart completion succeeded (or its object is found at the
/// envelope's length): framing is due.
pub fn completed(sql: &impl Sql, id: &str, now_ms: u64) -> R<()> {
    sql.exec(
        "UPDATE adm_sessions SET stage = 'frame', step_tries = 0, step_after_ms = 0, updated_ms = ?
         WHERE session = ? AND state = 'verifying' AND stage = 'complete'",
        &[bind_u64(now_ms)?, s(id)],
    )?;
    Ok(())
}

/// A multipart completion that R2 refused definitively (a part changed or
/// went missing, or the upload no longer exists and no object stands in for
/// it): the object does not match its envelope.
pub fn completion_failed(sql: &impl Sql, id: &str, now_ms: u64) -> R<()> {
    let e = ApiError::new(
        ErrorCode::ObjectMismatch,
        "the uploaded parts do not assemble the envelope's object",
    );
    if session_row(sql, id)?
        .is_some_and(|r| r.state == SessionState::Verifying && r.stage.as_deref() == Some("complete"))
    {
        set_state(sql, id, SessionState::Refused, Some(&e), now_ms)?;
        sql.exec("UPDATE adm_sessions SET stage = NULL WHERE session = ?", &[s(id)])?;
    }
    Ok(())
}

/// Whether R2's refusal of a multipart completion is definitive: a part it
/// does not hold or that changed (`InvalidPart`, 10025), parts out of order
/// or too small, or an upload that no longer exists (`NoSuchUpload`, 10024;
/// the caller has already looked for the completed object). Anything else
/// (transport, 5xx, rate limits) is retried.
pub fn completion_refused(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "(10024)",
        "(10025)",
        "(10011)",
        "nosuchupload",
        "invalidpart",
        "entitytoosmall",
        "no such upload",
        "invalid part",
    ]
    .iter()
    .any(|k| m.contains(k))
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

/// A completion or framing step that met a transport or platform failure:
/// it never ends the session, and the step runs again after a backoff that
/// grows with the session's step attempts. A framing attempt that met one
/// is not counted against the upload (`uncount`).
pub fn step_retry(sql: &impl Sql, id: &str, uncount: bool, now_ms: u64) -> R<()> {
    let rows = sql.exec("SELECT step_tries FROM adm_sessions WHERE session = ?", &[s(id)])?;
    let Some(r) = rows.first() else { return Ok(()) };
    let tries = u32::try_from(int(r, 0)?).unwrap_or(u32::MAX);
    let kept = if uncount { tries.saturating_sub(1) } else { tries };
    sql.exec(
        "UPDATE adm_sessions SET step_tries = ?, step_after_ms = ?, updated_ms = ? WHERE session = ? AND state = 'verifying'",
        &[i(i64::from(kept)), bind_u64(now_ms.saturating_add(backoff_ms(tries.max(1))))?, bind_u64(now_ms)?, s(id)],
    )?;
    Ok(())
}

/// Starts a completion step: counts the attempt and returns the completion.
pub fn completion_begin(sql: &impl Sql, id: &str, now_ms: u64) -> R<Option<(Completion, u64)>> {
    let t = completion_target(sql, id)?;
    if t.is_some() {
        sql.exec(
            "UPDATE adm_sessions SET step_tries = step_tries + 1, updated_ms = ? WHERE session = ?",
            &[bind_u64(now_ms)?, s(id)],
        )?;
    }
    Ok(t)
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
    sql.exec(
        "UPDATE adm_verdicts SET state = 'refused', code = ?, claim_until_ms = 0, updated_ms = ?
         WHERE hotkey = ? AND commitment = ? AND state = 'pending'",
        &[
            s(&code),
            bind_u64(now_ms)?,
            s(hex::encode(r.hotkey.0)),
            s(r.upload.commitment.hex()),
        ],
    )?;
    Ok(true)
}

/// `DELETE .../attempts/{attempt}`.
pub fn abort(
    sql: &impl Sql,
    chain: &ChainBinding,
    req: &Signed,
    id: &str,
    attempt: u32,
    now_ms: u64,
) -> (Reply, Effects) {
    let go = || -> A<(Reply, Effects)> {
        let (signer, _) = signer(sql, chain, req)?;
        let r = owned(sql, &signer, id)?;
        let cursor = cursor(sql)?.unwrap_or(0);
        if closed_for(sql, &r)? {
            return Err(ApiError::new(
                ErrorCode::AfterCutoff,
                "admission has closed for this cohort",
            ));
        }
        if attempt != r.attempt {
            return Err(stale(id, r.attempt));
        }
        let mut fx = Effects::default();
        match r.state {
            SessionState::Aborted => {}
            SessionState::Uploading | SessionState::Verified | SessionState::Refused => {
                set_state(sql, id, SessionState::Aborted, None, now_ms)?;
                sql.exec("DELETE FROM adm_parts WHERE session = ?", &[s(id)])?;
                fx.abort.push((r.object_key.clone(), r.upload_id.clone()));
            }
            _ => {
                return Err(ApiError::new(
                    ErrorCode::SessionNotUploading,
                    "the session cannot be aborted in this state",
                ));
            }
        }
        let r = session_row(sql, id)?.ok_or_else(|| ApiError::new(ErrorCode::Internal, "session"))?;
        Ok((Reply::ok(200, &session_json(sql, &r, cursor)?), fx))
    };
    go().unwrap_or_else(|e| (Reply::error(&e), Effects::default()))
}

fn stale(id: &str, current: u32) -> ApiError {
    ApiError::new(
        ErrorCode::StaleAttempt,
        "the request names another attempt than the current one",
    )
    .with(Detail {
        session: Some(id.into()),
        attempt: Some(current),
        ..Default::default()
    })
}

/// `POST .../attempts/{attempt}`: reopens an aborted session, empty.
pub fn reopen(
    sql: &impl Sql,
    chain: &ChainBinding,
    req: &Signed,
    id: &str,
    attempt: u32,
    now_ms: u64,
) -> (Reply, Effects) {
    let go = || -> A<(Reply, Effects)> {
        let (signer, _) = signer(sql, chain, req)?;
        let r = owned(sql, &signer, id)?;
        let cursor = cursor(sql)?.unwrap_or(0);
        if attempt == r.attempt && r.state != SessionState::Aborted {
            return Ok((Reply::ok(200, &session_json(sql, &r, cursor)?), Effects::default()));
        }
        if attempt != r.attempt.saturating_add(1) || r.state != SessionState::Aborted {
            return Err(stale(id, r.attempt));
        }
        let w = window(sql)?;
        let fail = |e: ApiError| -> A<(Reply, Effects)> {
            record_refusal(sql, cursor, &signer, w.as_ref(), Some(&r.upload), &e)?;
            Err(e)
        };
        let w = match admission_checks(sql, chain, w.as_ref(), &signer, &r.upload) {
            Ok(w) => w,
            Err(e) => return fail(e),
        };
        let abort = match one_artifact(sql, &w, &signer, id, &r.upload, now_ms) {
            Ok(a) => a,
            Err(e) => return fail(e),
        };
        let key = object_key(&w.label, id, attempt);
        sql.exec(
            "UPDATE adm_sessions SET attempt = ?, state = 'uploading', stage = NULL, refusal = NULL, pos_block = NULL,
                pos_extrinsic = NULL, object_key = ?, upload_id = NULL, frame = NULL, updated_ms = ? WHERE session = ?",
            &[i(i64::from(attempt)), s(&key), bind_u64(now_ms)?, s(id)],
        )?;
        let r = session_row(sql, id)?.ok_or_else(|| ApiError::new(ErrorCode::Internal, "session"))?;
        Ok((
            Reply::ok(200, &session_json(sql, &r, cursor)?),
            Effects {
                create: Some((key, id.into(), attempt)),
                abort,
                complete: None,
            },
        ))
    };
    go().unwrap_or_else(|e| (Reply::error(&e), Effects::default()))
}

/// `GET /admission/v1/cohorts/{cohort}/hotkeys/{hotkey}`.
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
            sessions: sessions
                .iter()
                .map(|r| SessionSummary {
                    session: r.session.clone(),
                    state: r.state.as_str().into(),
                    attempt: Some(r.attempt),
                    refusal: r.refusal.clone(),
                })
                .collect(),
            last_refusal: last,
        };
        // The contract's response limit: a view that would exceed it lists
        // the live sessions and the newest ended sessions that fit.
        let limit = slop_ninja_admission_wire::response_limit(w.as_ref().map(|w| w.plan.max_parts));
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
        Ok(Reply::ok(200, &view))
    };
    go().unwrap_or_else(|e| Reply::error(&e))
}

/// `GET /admission/v1/cohort` (unsigned, `no-store`).
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
        part_size: w.plan.part_size,
        max_parts: w.plan.max_parts,
        limits: limits_digest(&w.plan, &w.bounds),
        artifact_spec: Some(ArtifactSpecRef {
            id: w.bounds.spec_id.into(),
            sha256: w.bounds.spec_sha256.clone(),
        }),
    });
    let next = match &w {
        Some(w) => Some(NextCohort {
            cohort: w.cohort + 1,
            earliest_open_epoch: Some(w.opened_epoch.saturating_add(w.k_tempos)),
        }),
        None => crate::eval::calendar::next(sql)?.map(|(cohort, earliest_open_epoch)| NextCohort {
            cohort,
            earliest_open_epoch,
        }),
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
    })
}

// ----------------------------------------------------------------- framing

/// The scorer parts and manifest of an uploaded object, derived while it
/// streams. The protocol reader is the only parser: it decodes the header
/// record once (the length prefix, then at most `MAX_ARTIFACT_HEADER_LEN`
/// bytes) and checks the object and every file against it; the framer takes
/// the header and the files' offset from the reader and cuts each file into
/// the scorer's content-addressed slices. A header listing more files than
/// the spec's file table is refused before any slice is cut.
pub struct Framer {
    /// The whole object's SHA-256 and length, kept to the end even after a
    /// refusal, so a changed part reports `object_mismatch`.
    object: Sha256,
    len: u64,
    failed: Option<FrameError>,
    reader: ArtifactReader,
    /// Per header file: its length; filled once the reader has the header.
    files: Vec<u64>,
    /// The object offset of the first file (`None` until the header is read).
    files_at: Option<u64>,
    slices: Vec<Vec<Part>>,
    slice: Sha256,
    slice_len: u64,
    file: usize,
    file_left: u64,
}

/// One content-addressed slice to copy from the object to the blob store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slice {
    pub sha256: String,
    pub offset: u64,
    pub len: u64,
}

/// Why framing refused an object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The protocol reader refused it (`artifact_invalid`, the reason).
    Reader(String),
    /// A header-only check of the spec refused it (`check_failed`, the code).
    Check(String),
}

/// What framing derived.
#[derive(Clone, Debug)]
pub struct Framed {
    pub artifact: VerifiedArtifact,
    pub manifest: Vec<u8>,
    pub package_sha256: String,
    /// The graph file's SHA-256 (the record's model digest).
    pub model_sha256: String,
    pub slices: Vec<Slice>,
}

impl Default for Framer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framer {
    pub fn new() -> Self {
        Self {
            object: Sha256::new(),
            len: 0,
            failed: None,
            reader: ArtifactReader::new(),
            files: Vec::new(),
            files_at: None,
            slices: Vec::new(),
            slice: Sha256::new(),
            slice_len: 0,
            file: 0,
            file_left: 0,
        }
    }

    /// Feeds the next chunk. A refusal is kept for [`Framer::finish`]; the
    /// object's digest is computed to the end either way. Every call consumes
    /// its whole chunk.
    pub fn update(&mut self, chunk: &[u8]) {
        let start = self.len;
        self.object.update(chunk);
        self.len = self.len.saturating_add(chunk.len() as u64);
        if self.failed.is_some() {
            return;
        }
        if let Err(e) = self.reader.update(chunk) {
            self.failed = Some(FrameError::Reader(format!("{e:?}")));
            return;
        }
        if self.files_at.is_none() {
            let Some((h, at)) = self.reader.header() else { return };
            // The spec's file table names every file an artifact may hold;
            // a longer list has an undeclared one (the code the header check
            // would give), refused before any slice is cut.
            if h.files.len() > spec::ROLES.len() {
                self.failed = Some(FrameError::Check("undeclared_file".into()));
                return;
            }
            self.files = h.files.iter().map(|f| f.len).collect();
            self.slices = vec![Vec::new(); self.files.len()];
            self.file_left = self.files.first().copied().unwrap_or(0);
            self.files_at = Some(at);
        }
        let at = self.files_at.unwrap_or(u64::MAX);
        // The part of this chunk past the header (the reader refuses
        // trailing bytes, so every byte here belongs to a file).
        let skip = usize::try_from(at.saturating_sub(start))
            .unwrap_or(usize::MAX)
            .min(chunk.len());
        let mut rest = chunk.get(skip..).unwrap_or_default();
        while !rest.is_empty() && self.file < self.files.len() {
            let room = (spec::PART_BYTES - self.slice_len).min(self.file_left);
            let take = usize::try_from(room).unwrap_or(usize::MAX).min(rest.len());
            let (head, tail) = rest.split_at(take);
            self.slice.update(head);
            self.slice_len += take as u64;
            self.file_left -= take as u64;
            rest = tail;
            if self.slice_len == spec::PART_BYTES || self.file_left == 0 {
                self.close_slice();
            }
            if self.file_left == 0 {
                self.file += 1;
                self.file_left = self.files.get(self.file).copied().unwrap_or(0);
            }
        }
    }

    fn close_slice(&mut self) {
        let sha = hex::encode(std::mem::take(&mut self.slice).finalize());
        if let Some(v) = self.slices.get_mut(self.file) {
            v.push(Part {
                sha256: sha,
                bytes: self.slice_len,
            });
        }
        self.slice_len = 0;
    }

    /// Finishes framing under `version`: the object must verify, its header
    /// pass the spec's file-table checks, and its graph length name a class.
    /// The object's SHA-256 and length, and what framing derived from it.
    pub fn finish(self, version: Version) -> (Digest, u64, Result<Framed, FrameError>) {
        let digest = Digest(self.object.clone().finalize().into());
        let len = self.len;
        (digest, len, self.derive(version))
    }

    fn derive(self, version: Version) -> Result<Framed, FrameError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        let files_at = self.files_at.ok_or_else(|| FrameError::Reader("no header".into()))?;
        let artifact = self.reader.finish().map_err(|e| FrameError::Reader(format!("{e:?}")))?;
        let header = &artifact.header;
        // The header-only file-table checks first (contract, names, roles,
        // tokenizer, lengths): their codes are the spec's.
        mapping::check_header_for(header, version).map_err(|r| FrameError::Check(r.code().to_owned()))?;
        let graph = header
            .files
            .iter()
            .find(|f| f.name == spec::GRAPH_FILE)
            .ok_or_else(|| FrameError::Check("missing_file".into()))?;
        let class = version
            .graph_class_by_len(graph.len)
            .ok_or_else(|| FrameError::Check("graph_size".into()))?;
        let profile = mapping::profile_id_for(class, version);
        let m = mapping::parts_manifest_for(header, &self.slices, &profile, version)
            .map_err(|r| FrameError::Check(r.code().to_owned()))?;
        let manifest = m.to_bytes();
        let package_sha256 = hex::encode(Sha256::digest(&manifest));
        let model_sha256 = graph.sha256.hex();
        let mut slices = Vec::new();
        let mut at = files_at;
        for parts in &self.slices {
            for p in parts {
                slices.push(Slice {
                    sha256: p.sha256.clone(),
                    offset: at,
                    len: p.bytes,
                });
                at += p.bytes;
            }
        }
        Ok(Framed {
            model_sha256,
            artifact,
            manifest,
            package_sha256,
            slices,
        })
    }
}

/// The next durable verification step that is due: a multipart completion
/// not yet confirmed (`complete`), framing (`frame`; not while the hotkey's
/// latest commitment is still being logged) or blob copies (`blobs`).
pub fn verify_due(sql: &impl Sql, now_ms: u64) -> R<Option<(String, String)>> {
    let rows = sql.exec(
        "SELECT s.session, s.stage FROM adm_sessions s WHERE s.state = 'verifying' AND s.stage IN ('complete','frame','blobs')
           AND s.step_after_ms <= ?
           AND (s.stage != 'frame' OR NOT EXISTS (SELECT 1 FROM commit_seen c WHERE c.hotkey = s.hotkey AND c.stage IN ('hash','events','reg')))
         ORDER BY s.updated_ms LIMIT 1",
        &[bind_u64(now_ms)?],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    Ok(Some((text(r, 0)?.to_owned(), text(r, 1)?.to_owned())))
}

/// Starts a framing step: counts the attempt and returns the session's
/// object key, length and the cohort's spec. An attempt that ends without an
/// outcome (the step was cut off: CPU, eviction) stays counted, and a
/// session whose attempts reach the cohort's attributable-failure bound
/// (`artifact_failures_before_model_error`) is refused instead, so no upload
/// holds the cohort's alarm.
pub fn frame_begin(sql: &impl Sql, id: &str, now_ms: u64) -> R<Option<(String, u64, Version)>> {
    let Some(r) = session_row(sql, id)? else {
        return Ok(None);
    };
    if r.state != SessionState::Verifying || r.stage.as_deref() != Some("frame") {
        return Ok(None);
    }
    let Some(w) = window_for(sql, r.cohort)? else {
        return Ok(None);
    };
    let rows = sql.exec("SELECT step_tries FROM adm_sessions WHERE session = ?", &[s(id)])?;
    let tries = rows.first().map_or(Ok(0), |x| int(x, 0))?;
    if tries >= i64::from(w.failures_before_refusal) {
        let e = ApiError::new(ErrorCode::ArtifactInvalid, "framing the object did not complete").with(Detail {
            reason: Some(format!("framing_incomplete after {tries} attempts")),
            ..Default::default()
        });
        set_state(sql, id, SessionState::Refused, Some(&e), now_ms)?;
        sql.exec("UPDATE adm_sessions SET stage = NULL WHERE session = ?", &[s(id)])?;
        return Ok(None);
    }
    sql.exec(
        "UPDATE adm_sessions SET step_tries = step_tries + 1, updated_ms = ? WHERE session = ?",
        &[bind_u64(now_ms)?, s(id)],
    )?;
    let v = Version::from_id(w.bounds.spec_id).ok_or(EvalError::State("spec"))?;
    Ok(Some((r.object_key, r.upload.artifact_len, v)))
}

/// The framing record kept with the session until its blobs are written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRecord {
    pub package_sha256: String,
    pub manifest_key: String,
    pub manifest_hex: String,
    pub model_sha256: String,
    pub slices: Vec<Slice>,
}

/// Applies the framing outcome: the object's digest and length against the
/// envelope, the cohort contract, and `SubmissionUpload::admit` against the
/// logged commitment the session opened. Refusals end the session.
pub fn apply_frame(
    sql: &impl Sql,
    chain: &ChainBinding,
    id: &str,
    framed: (Digest, u64, Result<Framed, FrameError>),
    now_ms: u64,
) -> R<()> {
    let Some(r) = session_row(sql, id)? else { return Ok(()) };
    if r.state != SessionState::Verifying || r.stage.as_deref() != Some("frame") {
        return Ok(());
    }
    let Some(w) = window_for(sql, r.cohort)? else {
        return Ok(());
    };
    let refuse = |e: ApiError| -> R<()> {
        set_state(sql, id, SessionState::Refused, Some(&e), now_ms)?;
        sql.exec("UPDATE adm_sessions SET stage = NULL WHERE session = ?", &[s(id)])?;
        Ok(())
    };
    let (object_sha256, object_len, framed) = framed;
    if object_sha256 != r.upload.artifact_sha256 || object_len != r.upload.artifact_len {
        return refuse(ApiError::new(
            ErrorCode::ObjectMismatch,
            "the object's SHA-256 or length is not the envelope's",
        ));
    }
    let f = match framed {
        Ok(f) => f,
        Err(FrameError::Reader(reason)) => {
            return refuse(
                ApiError::new(ErrorCode::ArtifactInvalid, "the artifact reader refused the object").with(Detail {
                    reason: Some(reason),
                    ..Default::default()
                }),
            );
        }
        Err(FrameError::Check(code)) => {
            return refuse(
                ApiError::new(ErrorCode::CheckFailed, "an artifact check refused it").with(Detail {
                    check: Some(code),
                    ..Default::default()
                }),
            );
        }
    };
    if f.artifact.header.contract != w.output_contract {
        return refuse(ApiError::new(
            ErrorCode::ContractMismatch,
            "the header's contract is not the cohort's output contract",
        ));
    }
    let (_, entry) = match check_commitment(sql, chain, &w, &r.upload) {
        Ok(v) => v,
        // The hotkey's latest commitment is still being logged: retryable,
        // never a refusal; framing runs again once it is logged
        // (`verify_due` waits for it).
        Err(e) if e.code == ErrorCode::CommitmentNotFound && commitments::unresolved(sql, &r.hotkey)? => {
            sql.exec(
                "UPDATE adm_sessions SET step_tries = MAX(step_tries - 1, 0), updated_ms = ? WHERE session = ?",
                &[bind_u64(now_ms)?, s(id)],
            )?;
            return Ok(());
        }
        Err(e) => return refuse(e),
    };
    let (Some(uid), Some(registration_block)) = (entry.uid, entry.registration_block) else {
        return refuse(ApiError::new(ErrorCode::NotRegistered, "not registered"));
    };
    let fc = FinalizedCommitment {
        chain,
        committer: NeuronIdentity {
            uid,
            hotkey: r.hotkey,
            registration_block,
        },
        block: entry.block,
        extrinsic_index: entry.extrinsic,
        payload: &entry.payload,
    };
    let entry_pos = match r.upload.admit(w.cohort, &fc, &f.artifact) {
        Ok(e) => e,
        Err(_) => {
            return refuse(ApiError::new(
                ErrorCode::ContentMismatch,
                "the object's derived content is not the committed content",
            ));
        }
    };
    let rec = FrameRecord {
        package_sha256: f.package_sha256.clone(),
        manifest_key: format!("artifacts/packages/{}.json", f.package_sha256),
        manifest_hex: hex::encode(&f.manifest),
        model_sha256: f.model_sha256.clone(),
        slices: f.slices,
    };
    sql.exec(
        "UPDATE adm_sessions SET stage = 'blobs', frame = ?, package_sha256 = ?, manifest_key = ?, model_sha256 = ?,
            pos_block = ?, pos_extrinsic = ?, step_tries = 0, step_after_ms = 0, updated_ms = ? WHERE session = ?",
        &[
            s(serde_json::to_string(&rec).map_err(|_| EvalError::State("frame"))?),
            s(&rec.package_sha256),
            s(&rec.manifest_key),
            s(&rec.model_sha256),
            bind_u64(entry_pos.commitment_block)?,
            i(i64::from(entry_pos.extrinsic_index)),
            bind_u64(now_ms)?,
            s(id),
        ],
    )?;
    Ok(())
}

/// The framing record of a session whose blobs are due.
pub fn frame_record(sql: &impl Sql, id: &str) -> R<Option<(String, FrameRecord)>> {
    let rows = sql.exec(
        "SELECT object_key, frame FROM adm_sessions WHERE session = ? AND stage = 'blobs'",
        &[s(id)],
    )?;
    let Some(r) = rows.first() else { return Ok(None) };
    let rec = serde_json::from_str(text(r, 1)?).map_err(|_| EvalError::State("frame"))?;
    Ok(Some((text(r, 0)?.to_owned(), rec)))
}

/// A slice copy about to create its blob (the store held none): recorded
/// before the copy, so the clean-up of an upload that ends without admission
/// deletes it unless another session uses it.
pub fn blob_created(sql: &impl Sql, id: &str, sha256: &str) -> R<()> {
    sql.exec(
        "INSERT INTO adm_blobs (session, sha256, created) VALUES (?, ?, 1)
         ON CONFLICT (session, sha256) DO UPDATE SET created = 1",
        &[s(id), s(sha256)],
    )?;
    Ok(())
}

/// The parts manifest a session is about to write (the store held none).
pub fn manifest_created(sql: &impl Sql, id: &str) -> R<()> {
    sql.exec(
        "UPDATE adm_sessions SET manifest_created = 1 WHERE session = ?",
        &[s(id)],
    )?;
    Ok(())
}

/// After every slice and the manifest are in the blob store: the verdict
/// (the admission lane) is next, under one run per hotkey and commitment.
/// Every slice the session uses is recorded with it (those its copies
/// created were recorded as they were copied).
pub fn blobs_written(sql: &impl Sql, cfg: &AdmissionEnv, id: &str, now_ms: u64) -> R<()> {
    let Some(r) = session_row(sql, id)? else { return Ok(()) };
    if r.stage.as_deref() != Some("blobs") {
        return Ok(());
    }
    if let Some((_, rec)) = frame_record(sql, id)? {
        for sl in &rec.slices {
            sql.exec(
                "INSERT OR IGNORE INTO adm_blobs (session, sha256, created) VALUES (?, ?, 0)",
                &[s(id), s(&sl.sha256)],
            )?;
        }
    }
    let rows = sql.exec(
        "SELECT package_sha256, manifest_key FROM adm_sessions WHERE session = ?",
        &[s(id)],
    )?;
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
            sql.exec(
                "UPDATE adm_sessions SET stage = NULL, frame = NULL WHERE session = ?",
                &[s(id)],
            )?;
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
        "UPDATE adm_sessions SET stage = 'verdict', frame = NULL, step_after_ms = 0, updated_ms = ? WHERE session = ?",
        &[bind_u64(now_ms)?, s(id)],
    )?;
    apply_verdicts(sql, cfg, now_ms)
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
}

impl LaneRun {
    /// The probe-route request of this run: the package, the control
    /// document, and the cohort's spec digest (the load names it, so the
    /// scorer admits the package by the cohort's spec, not its unnamed-load
    /// spec).
    pub fn body(&self) -> Value {
        json!({"package_sha256": self.package_sha256, "manifest_key": self.manifest_key,
            "spec_sha256": self.spec_sha256,
            "docs": [{"id": "0", "text": crate::eval::lane_logic::CONTROL_TEXT}]})
    }

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
        "SELECT hotkey, commitment, package_sha256, manifest_key, label, spec_sha256, tries FROM adm_verdicts
         WHERE state = 'pending' AND claim_until_ms <= ? AND not_before_ms <= ?
           AND EXISTS (SELECT 1 FROM adm_sessions s WHERE s.hotkey = adm_verdicts.hotkey AND s.commitment = adm_verdicts.commitment
                       AND s.state = 'verifying' AND s.stage = 'verdict')
         ORDER BY updated_ms LIMIT ?",
        &[bind_u64(now_ms)?, bind_u64(now_ms)?, i(take as i64)],
    )?;
    let mut out = Vec::new();
    for r in &rows {
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
    /// coordinator's own manifest or blobs, a reused instance, a reply this
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
                sql.exec(
                    "UPDATE adm_verdicts SET artifact_failures = ?, code = ?, claim_until_ms = 0, not_before_ms = ?, updated_ms = ?
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
        LaneOutcome::Platform(code) => {
            let n = u32::try_from(int(r, 3)?).unwrap_or(u32::MAX).saturating_add(1);
            sql.exec(
                "UPDATE adm_verdicts SET code = ?, platform_tries = ?, claim_until_ms = 0, not_before_ms = ?, updated_ms = ?
                 WHERE hotkey = ? AND commitment = ?",
                &[
                    s(code),
                    i(i64::from(n)),
                    bind_u64(now_ms.saturating_add(backoff_ms(n)))?,
                    bind_u64(now_ms)?,
                    key[0].clone(),
                    key[1].clone(),
                ],
            )?;
        }
    }
    apply_verdicts(sql, cfg, now_ms)
}

/// Load failures attributable to the artifact on a fresh instance (the
/// lane client's non-definitive `Prep::Artifact` codes about the load).
const ARTIFACT_LOAD_FAILURES: &[&str] = &[
    "artifact load failed",
    "artifact load failed (load fault)",
    "container died during the load",
    "artifact load outlasted its allowance",
    "part rejected",
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

/// Closes an admitting cohort's window at the epoch boundary that reached
/// its admission minimum (`engine::advance`); `cutoff_block` is the
/// boundary's first block. `false` when it was already closed.
pub fn close_window(sql: &impl Sql, c: &Cohort, epoch: u64, cutoff_block: u64, now_ms: u64) -> R<bool> {
    let Some(w) = window_of(sql, c)? else {
        return Err(EvalError::State("admission window"));
    };
    if !w.admitting() {
        return Ok(false);
    }
    close(sql, &w, epoch, cutoff_block, now_ms)?;
    Ok(true)
}

/// The fault a closing cohort takes when its roster cannot be frozen (a
/// frozen plan the engine refuses): the cohort pauses, never skipped, until
/// the operator abandons it (`engine::abandon`) or refuses sessions.
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
        "SELECT seq, closed_at_epoch, ready_epoch FROM adm_cohorts WHERE state = 'closing' ORDER BY seq LIMIT 1",
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

fn fault_freeze(sql: &impl Sql, seq: i64, why: &'static str, now_ms: u64) -> R<Option<Boundary>> {
    let _ = why;
    engine::fault(sql, seq, FREEZE_FAULT, now_ms)?;
    Ok(Some(Boundary::Waiting("roster freeze refused: the cohort is faulted")))
}

/// Whether admission has work for the cohort object's step, or the calendar
/// has a cohort to open under `cfg` (`calendar::due`: never for a retired
/// authority).
pub fn has_work(sql: &impl Sql, cfg: &EvalConfig) -> R<bool> {
    Ok(!sql
        .exec(
            "SELECT 1 FROM adm_sessions WHERE state = 'verifying' AND stage IN ('complete','frame','blobs','verdict')
             UNION ALL SELECT 1 FROM adm_cohorts WHERE state = 'closing'
             UNION ALL SELECT 1 FROM adm_sessions s JOIN adm_cohorts c ON c.seq = s.seq
               WHERE c.state IN ('frozen','abandoned') AND s.cleaned = 0 AND s.state IN ('refused','overflow','superseded','aborted')
             LIMIT 1",
            &[],
        )?
        .is_empty()
        || crate::eval::calendar::due(sql, cfg)?.is_some())
}

/// The next admission step: the freeze when it can run, completions,
/// framing or blob copies, verdict runs, clean-up, and the calendar's next
/// cohort.
pub fn next_work(sql: &impl Sql, cfg: &EvalConfig, now_ms: u64) -> R<Option<engine::Work>> {
    // A freeze moves its cohort to generating; the engine's own work for it
    // follows in the same step.
    if let Some(Boundary::Frozen { .. }) = try_freeze(sql, cfg, now_ms)? {
        return Ok(None);
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
    if !cleanup_due(sql, 1)?.is_empty() {
        return Ok(Some(engine::Work::AdmissionCleanup));
    }
    if let Some((label, plan)) = crate::eval::calendar::due(sql, cfg)? {
        return Ok(Some(engine::Work::OpenNext { label, plan }));
    }
    Ok(None)
}

fn close(sql: &impl Sql, w: &Window, epoch: u64, cutoff: u64, now_ms: u64) -> R<()> {
    sql.exec(
        "UPDATE adm_cohorts SET state = 'closing', cutoff_block = ?, closed_at_epoch = ? WHERE seq = ? AND state = 'admitting'",
        &[bind_u64(cutoff)?, bind_u64(epoch)?, i(w.seq)],
    )?;
    // Every session not completed is refused; its upload is aborted later.
    let e = ApiError::new(
        ErrorCode::IncompleteAtCutoff,
        "the session was not completed before the close",
    );
    for r in sql.exec(
        "SELECT session FROM adm_sessions WHERE seq = ? AND state = 'uploading'",
        &[i(w.seq)],
    )? {
        let id = text(&r, 0)?.to_owned();
        set_state(sql, &id, SessionState::Refused, Some(&e), now_ms)?;
        sql.exec("DELETE FROM adm_parts WHERE session = ?", &[s(&id)])?;
    }
    // The snapshot covers every hotkey whose upload completed in time.
    sql.exec(
        "INSERT OR IGNORE INTO adm_snapshot_wanted (cutoff_block, hotkey)
         SELECT DISTINCT ?, hotkey FROM adm_sessions WHERE seq = ? AND state IN ('verifying','verified')",
        &[bind_u64(cutoff)?, i(w.seq)],
    )?;
    set_phase(
        sql,
        w.seq,
        "admission",
        now_ms,
        "admission closed at a boundary; the roster freezes once verdicts and the commitment snapshot are in",
    )
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
            "SELECT block, payload, uid, registration_block FROM adm_snapshot WHERE cutoff_block = ? AND hotkey = ?",
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
    // its own: not one of the operator's hotkeys, and not a copy of an
    // artifact already published (whose copy the seal leaves unexecuted).
    let published: std::collections::BTreeSet<String> = sql
        .exec("SELECT content FROM published_contents", &[])?
        .iter()
        .filter_map(|r| text(r, 0).ok().map(str::to_ascii_lowercase))
        .collect();
    let outside = admitted
        .iter()
        .any(|a| !w.plan.is_operator(&a.row.hotkey.0) && !published.contains(&a.row.upload.opening.content.hex()));
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
                object_key: Some(a.row.object_key.clone()),
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
    for o in &d.overflow {
        sql.exec(
            "UPDATE adm_sessions SET state = 'overflow', next_cohort = ?, pos_block = ?, pos_extrinsic = ?, updated_ms = ? WHERE session = ?",
            &[bind_u64(w.cohort + 1)?, bind_u64(o.entry.block)?, i(i64::from(o.entry.extrinsic)), bind_u64(now_ms)?, s(&o.row.session)],
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
        "UPDATE adm_cohorts SET state = 'abandoned' WHERE seq = ? AND state IN ('admitting','closing') RETURNING seq",
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

/// Objects of sessions that ended without admission (refused, overflow,
/// superseded, aborted) once their cohort froze or was abandoned, still to
/// delete.
pub fn cleanup_due(sql: &impl Sql, limit: i64) -> R<Vec<(String, String, Option<String>)>> {
    sql.exec(
        "SELECT s.session, s.object_key, s.upload_id FROM adm_sessions s JOIN adm_cohorts c ON c.seq = s.seq
         WHERE c.state IN ('frozen','abandoned') AND s.cleaned = 0 AND s.state IN ('refused','overflow','superseded','aborted') LIMIT ?",
        &[i(limit)],
    )?
    .iter()
    .map(|r| Ok((text(r, 0)?.to_owned(), text(r, 1)?.to_owned(), opt_text(r, 2))))
    .collect()
}

/// The blob-store keys a session's clean-up deletes besides its object: the
/// blobs its copies created and the parts manifest it wrote, unless another
/// session not yet cleaned up (or admitted) uses them.
pub fn cleanup_keys(sql: &impl Sql, id: &str) -> R<Vec<String>> {
    let mut keys: Vec<String> = sql
        .exec(
            "SELECT b.sha256 FROM adm_blobs b WHERE b.session = ? AND b.created = 1
               AND NOT EXISTS (SELECT 1 FROM adm_blobs o JOIN adm_sessions x ON x.session = o.session
                   WHERE o.sha256 = b.sha256 AND o.session != b.session AND (x.cleaned = 0 OR x.state = 'admitted'))",
            &[s(id)],
        )?
        .iter()
        .map(|r| Ok(format!("artifacts/blobs/{}", text(r, 0)?)))
        .collect::<R<_>>()?;
    let m = sql.exec(
        "SELECT s.manifest_key FROM adm_sessions s WHERE s.session = ? AND s.manifest_created = 1 AND s.manifest_key IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM adm_sessions o WHERE o.package_sha256 = s.package_sha256 AND o.session != s.session
               AND (o.cleaned = 0 OR o.state = 'admitted'))",
        &[s(id)],
    )?;
    if let Some(r) = m.first() {
        keys.push(text(r, 0)?.to_owned());
    }
    Ok(keys)
}

pub fn mark_cleaned(sql: &impl Sql, id: &str) -> R<()> {
    sql.exec("UPDATE adm_sessions SET cleaned = 1 WHERE session = ?", &[s(id)])?;
    Ok(())
}

/// The admitted entrants' published objects, named after the close
/// (`closure-artifacts.json` beside the closure).
pub fn closure_artifacts(plan: &CohortPlan) -> Value {
    let list: Vec<Value> = plan
        .entrants
        .iter()
        .filter_map(|a| {
            Some(
                json!({"hotkey": a.owner.hotkey.trim_start_matches("0x"), "object_key": a.object_key.as_deref()?,
                "package_sha256": a.package_sha256, "manifest_key": a.manifest_key}),
            )
        })
        .collect();
    json!({"schema": "slop_ninja.admission.closure_artifacts/v1", "artifacts": list})
}

/// Content-free admission status for `/eval/status`.
pub fn status(sql: &impl Sql) -> R<Value> {
    let cohorts: Vec<Value> = sql
        .exec("SELECT seq, cohort, state, open_block, cutoff_block, frozen_epoch, outside, admitted, overflow FROM adm_cohorts ORDER BY seq DESC LIMIT 8", &[])?
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
                "sessions": states}))
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
        "steps_retrying": count(sql, "SELECT COUNT(*) FROM adm_sessions WHERE state = 'verifying' AND step_after_ms > 0", &[])?,
    });
    Ok(
        json!({"contract": CONTRACT, "cohorts": cohorts, "verdicts": verdicts, "platform_retries": stalled,
        "commitments": commitments::status(sql)?}),
    )
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
