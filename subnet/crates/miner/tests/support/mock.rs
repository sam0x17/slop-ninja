//! A local mock of the coordinator's admission routes
//! (`docs/testnet/admission-api.md`): signed requests, sessions with
//! attempts, parts, asynchronous verification through the protocol's
//! `ArtifactReader`, `SubmissionUpload::admit` and an optional artifact-check
//! stage, the one-artifact rule, the last refused request per hotkey, and the
//! cutoff with ordering, the roster cap and overflow.
//!
//! Its "chain" is a commitment log the test fills: every finalized commitment
//! of a hotkey, in order, as the coordinator records them from its finalized
//! cursor. Session opening uses a hotkey's latest entry; the cutoff uses its
//! last entry before the cutoff block, so a later commitment (for the next
//! cohort, say) never replaces the one that counted.
//!
//! Modes inject what a real coordinator or network can do: `paused`,
//! `rate_limited` with `Retry-After`, a part corrupted in transit, the
//! fallback that records claimed part digests without verifying them, an
//! artifact-check refusal, a redirect, and an oversized response.

#![allow(dead_code, clippy::result_large_err)]

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use slop_ninja_admission_wire::json::{
    Admitting, ArtifactSpecRef, Chain, CohortInfo, CommitmentView, Detail, ErrorBody, HotkeyView,
    LastRefusal, NextCohort, PartAck, PartRecord, Position, Refusal, Registration, SCHEMA_COHORT,
    SCHEMA_HOTKEY, SCHEMA_SESSION, Session, SessionSummary,
};
use slop_ninja_admission_wire::{
    CONTRACT, ErrorCode, PartPlan, RequestScope, Route, SELECTION_EARLIEST_COMMITMENT_V1,
    SessionState, header, parse_hex32, parse_signature, parse_valid_until, request_message,
};
use slop_ninja_protocol::submission::{FinalizedCommitment, SubmissionUpload, parse_chain_payload};
use slop_ninja_protocol::{
    AccountId, ChainBinding, Digest, Error as ProtocolError, NeuronIdentity, Record,
    VerifiedArtifact, object_sha256,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Debug)]
pub struct MockConfig {
    pub chain: ChainBinding,
    pub cohort: u64,
    pub opened_epoch: u64,
    pub open_block: u64,
    pub admission_min: u64,
    pub output_contract: String,
    pub roster_cap: u16,
    pub max_object_len: u64,
    pub part_size: u64,
    pub max_parts: u32,
    pub request_validity_blocks: u64,
}

#[derive(Clone, Debug)]
pub struct CommitmentRow {
    pub block: u64,
    pub extrinsic: u32,
    pub payload: Vec<u8>,
}

/// The artifact-check stage at completion.
#[derive(Clone, Debug)]
pub enum CheckHook {
    /// Run the artifact spec's `ObjectCheck` over the assembled object.
    Spec,
    /// Refuse every object with this check code.
    Refuse(String),
}

#[derive(Clone, Debug)]
struct MockSession {
    id: String,
    upload: SubmissionUpload,
    hotkey: AccountId,
    attempt: u32,
    /// Part number to (recorded digest, stored bytes).
    parts: BTreeMap<u32, (String, Vec<u8>)>,
    state: SessionState,
    refusal: Option<Refusal>,
    position: Option<Position>,
    next_cohort: Option<u64>,
    /// Verification result applied on the next read, so completion is
    /// observed as `verifying` first.
    pending: Option<Result<Position, Refusal>>,
}

#[derive(Debug, Default)]
pub struct Counters {
    /// Part number to the number of PUT requests that reached the handler.
    pub part_puts: BTreeMap<u32, u32>,
    pub requests: u32,
    /// Requests per route kind, for example `cohort` or `abort`.
    pub routes: BTreeMap<&'static str, u32>,
}

pub struct MockState {
    pub config: MockConfig,
    /// This mock's base URL, which the public snapshot names as the coordinator.
    pub base: String,
    /// The coordinator the public snapshot advertises (default: `base`).
    pub advertised: Option<String>,
    pub finalized_block: u64,
    pub registrations: HashMap<AccountId, NeuronIdentity>,
    /// Each hotkey's finalized commitments, in chain order.
    pub commitments: HashMap<AccountId, Vec<CommitmentRow>>,
    sessions: BTreeMap<String, MockSession>,
    last_refusals: HashMap<(AccountId, u64), LastRefusal>,
    /// The cohort's cutoff block, once admission has closed.
    pub cutoff_block: Option<u64>,
    pub artifact_spec: Option<ArtifactSpecRef>,
    /// Answer `unavailable` once for each part number listed.
    pub fail_part_once: Vec<u32>,
    /// Flip a byte of each listed part once, as if it changed in transit.
    pub corrupt_part_once: Vec<u32>,
    /// Record the claimed part digest without hashing the part (the
    /// contract's fallback where the digest-stream path is not qualified).
    pub fallback_digests: bool,
    /// Answer `paused` to this many signed requests.
    pub paused_requests: u32,
    /// Answer `rate_limited` to this many signed requests, with this
    /// `Retry-After`.
    pub rate_limited_requests: u32,
    pub retry_after_secs: u64,
    /// The artifact-check stage at completion; `None` stands for checks that pass.
    pub check_hook: Option<CheckHook>,
    /// Answer part uploads with a redirect to this URL.
    pub redirect_parts_to: Option<String>,
    /// Pad the cohort response with this many bytes in an unknown field.
    pub pad_cohort_bytes: usize,
    /// Whether `request_expired` carries the cursor (`detail.finalized_block`).
    pub expired_with_cursor: bool,
    /// Lose these part records at the next completion request, as a lost
    /// record would, so completion answers `parts_missing`.
    pub drop_parts_before_complete: Vec<u32>,
    pub counters: Counters,
}

pub struct Mock {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<MockState>>,
}

fn refusal(code: ErrorCode, message: &str, detail: Detail) -> Refusal {
    Refusal {
        code: code.as_str().into(),
        message: message.into(),
        detail,
    }
}

struct Reply {
    status: u16,
    body: String,
    headers: Vec<(&'static str, String)>,
}

fn error(code: ErrorCode, message: &str, detail: Detail) -> Reply {
    let status = code.http_status().expect("request error code");
    Reply {
        status,
        body: serde_json::to_string(&ErrorBody::new(code, message, detail)).unwrap(),
        headers: Vec::new(),
    }
}

fn ok<T: serde::Serialize>(status: u16, value: &T) -> Reply {
    Reply {
        status,
        body: serde_json::to_string(value).unwrap(),
        headers: Vec::new(),
    }
}

fn ss58(account: &AccountId) -> String {
    rtao_primitives::AccountId(account.0).to_string()
}

/// A stand-in block hash for a block number.
pub fn block_hash(block: u64) -> String {
    object_sha256(&block.to_le_bytes()).hex()
}

impl Mock {
    pub async fn start(config: MockConfig, finalized_block: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(MockState {
            config,
            base: format!("http://{addr}/"),
            advertised: None,
            finalized_block,
            registrations: HashMap::new(),
            commitments: HashMap::new(),
            sessions: BTreeMap::new(),
            last_refusals: HashMap::new(),
            cutoff_block: None,
            artifact_spec: None,
            fail_part_once: Vec::new(),
            corrupt_part_once: Vec::new(),
            fallback_digests: false,
            paused_requests: 0,
            rate_limited_requests: 0,
            retry_after_secs: 0,
            check_hook: None,
            redirect_parts_to: None,
            pad_cohort_bytes: 0,
            expired_with_cursor: true,
            drop_parts_before_complete: Vec::new(),
            counters: Counters::default(),
        }));
        let shared = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let state = shared.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, state).await;
                });
            }
        });
        Self { addr, state }
    }

    pub fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut MockState) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }

    pub fn register(&self, identity: NeuronIdentity) {
        self.with(|s| s.registrations.insert(identity.hotkey, identity));
    }

    /// A finalized commitment by `hotkey` at `block`, extrinsic `extrinsic`,
    /// appended to the hotkey's commitment log.
    pub fn commit(&self, hotkey: AccountId, block: u64, extrinsic: u32, payload: &[u8]) {
        self.with(|s| {
            let rows = s.commitments.entry(hotkey).or_default();
            rows.push(CommitmentRow {
                block,
                extrinsic,
                payload: payload.to_vec(),
            });
            rows.sort_by_key(|r| (r.block, r.extrinsic));
            s.finalized_block = s.finalized_block.max(block + 2);
        });
    }

    /// Closes admission with every commitment finalized so far inside the
    /// window (section 7 of the contract).
    pub fn close(&self) {
        self.with(|s| {
            let cutoff = s.finalized_block + 1;
            s.close_at(cutoff)
        });
    }

    /// Closes admission at `cutoff_block`; commitments at or after it do not
    /// count for this cohort, even when they are already finalized.
    pub fn close_at(&self, cutoff_block: u64) {
        self.with(|s| s.close_at(cutoff_block));
    }

    /// Opens the next cohort after a close.
    pub fn open_next(&self, open_block: u64) {
        self.with(|s| {
            s.config.cohort += 1;
            s.config.opened_epoch += 20;
            s.config.open_block = open_block;
            s.finalized_block = s.finalized_block.max(open_block);
            s.cutoff_block = None;
        });
    }

    pub fn session_state(&self, id: &str) -> Option<String> {
        self.with(|s| s.sessions.get(id).map(|x| x.state.as_str().to_owned()))
    }

    pub fn session_attempt(&self, id: &str) -> Option<u32> {
        self.with(|s| s.sessions.get(id).map(|x| x.attempt))
    }
}

async fn serve(mut stream: TcpStream, state: Arc<Mutex<MockState>>) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    let reply = state
        .lock()
        .unwrap()
        .handle(&method, &path, &headers, &body);
    let reason = match reply.status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        307 => "Temporary Redirect",
        _ => "Error",
    };
    let mut extra = String::new();
    for (name, value) in &reply.headers {
        extra.push_str(&format!("{name}: {value}\r\n"));
    }
    let response = format!(
        "HTTP/1.1 {} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{extra}connection: close\r\n\r\n{}",
        reply.status,
        reply.body.len(),
        reply.body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

impl MockState {
    fn handle(
        &mut self,
        method: &str,
        path: &str,
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> Reply {
        self.counters.requests += 1;
        if method == "GET" && path == "/v1/status.json" {
            return ok(200, &self.public_status());
        }
        let Some(route) = Route::parse(method, path) else {
            return error(ErrorCode::NotFound, "no such route", Detail::default());
        };
        let kind = match route {
            Route::ReadCohort => "cohort",
            Route::CreateSession => "create",
            Route::ReadSession { .. } => "read",
            Route::PutPart { .. } => "part",
            Route::Complete { .. } => "complete",
            Route::Abort { .. } => "abort",
            Route::Reopen { .. } => "reopen",
            Route::ReadHotkey { .. } => "hotkey",
        };
        *self.counters.routes.entry(kind).or_default() += 1;
        if route == Route::ReadCohort {
            let mut value = serde_json::to_value(self.cohort_info()).unwrap();
            if self.pad_cohort_bytes > 0 {
                value["padding"] = serde_json::Value::String("x".repeat(self.pad_cohort_bytes));
            }
            return Reply {
                headers: vec![("cache-control", "no-store".into())],
                ..ok(200, &value)
            };
        }
        let is_part = matches!(route, Route::PutPart { .. });
        let signer = match self.authenticate(method, path, headers, body, is_part) {
            Ok(signer) => signer,
            Err(reply) => return reply,
        };
        if self.paused_requests > 0 {
            self.paused_requests -= 1;
            return Reply {
                headers: vec![("retry-after", "0".into())],
                ..error(ErrorCode::Paused, "catching up", Detail::default())
            };
        }
        if self.rate_limited_requests > 0 {
            self.rate_limited_requests -= 1;
            return Reply {
                headers: vec![("retry-after", self.retry_after_secs.to_string())],
                ..error(ErrorCode::RateLimited, "slow down", Detail::default())
            };
        }
        match route {
            Route::CreateSession => self.create(signer, body),
            Route::ReadSession { session } => self.read(signer, session),
            Route::PutPart { session, part } => {
                if let Some(target) = &self.redirect_parts_to {
                    return Reply {
                        status: 307,
                        body: String::new(),
                        headers: vec![("location", target.clone())],
                    };
                }
                self.put_part(signer, session, part, headers, body)
            }
            Route::Complete { session } => self.complete(signer, session),
            Route::Abort { session, attempt } => self.abort(signer, session, attempt),
            Route::Reopen { session, attempt } => self.reopen(signer, session, attempt),
            Route::ReadHotkey { cohort, hotkey } => self.hotkey_view(signer, cohort, hotkey),
            Route::ReadCohort => unreachable!(),
        }
    }

    fn admitting(&self) -> bool {
        self.cutoff_block.is_none()
    }

    fn cohort_info(&self) -> CohortInfo {
        let c = &self.config;
        CohortInfo {
            schema: SCHEMA_COHORT.into(),
            contract: CONTRACT.into(),
            chain: Chain::from_binding(&c.chain),
            finalized_block: self.finalized_block,
            request_validity_blocks: c.request_validity_blocks,
            paused: self.paused_requests > 0,
            admitting: self.admitting().then(|| Admitting {
                cohort: c.cohort,
                label: format!("mock-c{}", c.cohort),
                opened_epoch: c.opened_epoch,
                open_block: c.open_block,
                planned_cutoff_epoch: c.opened_epoch + c.admission_min,
                closing: false,
                output_contract: c.output_contract.clone(),
                roster_cap: c.roster_cap,
                selection: SELECTION_EARLIEST_COMMITMENT_V1.into(),
                max_object_len: c.max_object_len,
                part_size: c.part_size,
                max_parts: c.max_parts,
                limits: object_sha256(b"mock limits").hex(),
                artifact_spec: self.artifact_spec.clone(),
            }),
            next: Some(NextCohort {
                cohort: c.cohort + 1,
                earliest_open_epoch: Some(c.opened_epoch + 20),
            }),
        }
    }

    /// The planned public status snapshot (contract section 10).
    fn public_status(&self) -> serde_json::Value {
        let c = &self.config;
        serde_json::json!({
            "schema": "slop_ninja.public.status/v1",
            "network": "testnet",
            "chain": Chain::from_binding(&c.chain),
            "confirmed": { "block": self.finalized_block, "epoch": c.opened_epoch, "age_secs": 12 },
            "phase": if self.admitting() { "admission" } else { "generation" },
            "mode": "burn",
            "fault": false,
            "notice": "mock notice",
            "admitting": self.admitting().then(|| serde_json::json!({
                "cohort": c.cohort,
                "label": format!("mock-c{}", c.cohort),
                "opened_epoch": c.opened_epoch,
                "planned_cutoff_epoch": c.opened_epoch + c.admission_min,
            })),
            "next": { "cohort": c.cohort + 1, "earliest_open_epoch": c.opened_epoch + 20 },
            "coordinator": self.advertised.clone().unwrap_or_else(|| self.base.clone()),
            "active_target": null,
            "reference": null,
            "a_later_field": [1, 2, 3],
        })
    }

    fn authenticate(
        &self,
        method: &str,
        path: &str,
        headers: &HashMap<String, String>,
        body: &[u8],
        is_part: bool,
    ) -> Result<AccountId, Reply> {
        let get = |name: &str| headers.get(name).map(String::as_str);
        let hotkey: rtao_primitives::AccountId =
            get(header::HOTKEY)
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| error(ErrorCode::BadSignature, "hotkey", Detail::default()))?;
        let valid_until = get(header::VALID_UNTIL)
            .and_then(parse_valid_until)
            .ok_or_else(|| error(ErrorCode::BadRequest, "valid-until", Detail::default()))?;
        let content = get(header::CONTENT_SHA256)
            .and_then(parse_hex32)
            .ok_or_else(|| error(ErrorCode::BadRequest, "content digest", Detail::default()))?;
        let signature = get(header::SIGNATURE)
            .and_then(parse_signature)
            .ok_or_else(|| error(ErrorCode::BadSignature, "signature", Detail::default()))?;
        if valid_until < self.finalized_block {
            return Err(error(
                ErrorCode::RequestExpired,
                "expired",
                Detail {
                    finalized_block: self.expired_with_cursor.then_some(self.finalized_block),
                    ..Default::default()
                },
            ));
        }
        if valid_until > self.finalized_block + self.config.request_validity_blocks {
            return Err(error(
                ErrorCode::RequestValidityTooLong,
                "too long",
                Detail::default(),
            ));
        }
        if !is_part && object_sha256(body).0 != content {
            return Err(error(
                ErrorCode::ContentDigestMismatch,
                "body",
                Detail::default(),
            ));
        }
        let account = AccountId(hotkey.0);
        let message = request_message(&RequestScope {
            chain: &self.config.chain,
            hotkey: &account,
            valid_until,
            method,
            path,
            content_sha256: &Digest(content),
        });
        let verified = rtao_core::keys::verify_strict(
            &message,
            &signature,
            &hotkey.to_string(),
            rtao_core::keys::CRYPTO_SR25519,
        )
        .unwrap_or(false);
        if !verified {
            return Err(error(
                ErrorCode::BadSignature,
                "signature does not verify",
                Detail::default(),
            ));
        }
        Ok(account)
    }

    fn session_json(&self, s: &MockSession) -> Session {
        let plan = PartPlan::new(
            s.upload.artifact_len,
            self.config.part_size,
            self.config.max_parts,
        )
        .unwrap();
        Session {
            schema: SCHEMA_SESSION.into(),
            session: s.id.clone(),
            cohort: s.upload.opening.cohort,
            hotkey: ss58(&s.hotkey),
            commitment: s.upload.commitment.hex(),
            artifact_sha256: s.upload.artifact_sha256.hex(),
            artifact_len: s.upload.artifact_len,
            part_size: plan.part_size(),
            part_count: plan.count(),
            attempt: s.attempt,
            parts: s
                .parts
                .iter()
                .map(|(n, (digest, bytes))| PartRecord {
                    part: *n,
                    len: bytes.len() as u64,
                    sha256: digest.clone(),
                })
                .collect(),
            state: s.state.as_str().into(),
            refusal: s.refusal.clone(),
            position: s.position,
            next_cohort: s.next_cohort,
            finalized_block: self.finalized_block,
        }
    }

    /// The hotkey's commitment that counts: its latest logged one, or with a
    /// cutoff its last one before the cutoff block.
    fn counted(&self, hotkey: &AccountId, cutoff: Option<u64>) -> Option<&CommitmentRow> {
        self.commitments
            .get(hotkey)?
            .iter()
            .rev()
            .find(|row| cutoff.is_none_or(|c| row.block < c))
    }

    /// The finalized commitment check of the contract: window, registration
    /// and opening. Returns the admission position.
    fn check_commitment(
        &self,
        upload: &SubmissionUpload,
        cutoff: Option<u64>,
    ) -> Result<Position, Refusal> {
        let opening = &upload.opening;
        let Some(row) = self.counted(&opening.hotkey, cutoff) else {
            return Err(refusal(
                ErrorCode::CommitmentNotFound,
                "no finalized commitment observed",
                Detail::default(),
            ));
        };
        if row.block < self.config.open_block {
            return Err(refusal(
                ErrorCode::CommitmentBeforeOpen,
                "committed before the cohort opened",
                Detail::default(),
            ));
        }
        let Some(identity) = self.registrations.get(&opening.hotkey) else {
            return Err(refusal(
                ErrorCode::NotRegistered,
                "not registered",
                Detail::default(),
            ));
        };
        let commitment = FinalizedCommitment {
            chain: &self.config.chain,
            committer: *identity,
            block: row.block,
            extrinsic_index: row.extrinsic,
            payload: &row.payload,
        };
        match opening.check_finalized(&commitment) {
            Ok(entry) => Ok(Position {
                block: entry.commitment_block,
                extrinsic: entry.extrinsic_index,
            }),
            Err(
                ProtocolError::Mismatch("committing account")
                | ProtocolError::Invalid("registration after commitment"),
            ) => Err(refusal(
                ErrorCode::RegistrationMismatch,
                "another registration generation",
                Detail::default(),
            )),
            Err(_) => Err(refusal(
                ErrorCode::CommitmentMismatch,
                "the opening does not open the current commitment",
                Detail {
                    payload_cohort: parse_chain_payload(&row.payload).ok().map(|(c, _)| c),
                    ..Default::default()
                },
            )),
        }
    }

    /// Records a refused session request and answers it.
    fn refused(
        &mut self,
        signer: AccountId,
        upload: Option<&SubmissionUpload>,
        code: ErrorCode,
        message: &str,
        detail: Detail,
    ) -> Reply {
        let opening_cohort = upload.map_or(self.config.cohort, |u| u.opening.cohort);
        let cohort = if self.admitting() {
            self.config.cohort
        } else {
            opening_cohort
        };
        let session = upload
            .and_then(|u| u.digest().ok())
            .map_or_else(String::new, |d| d.hex());
        self.last_refusals.insert(
            (signer, cohort),
            LastRefusal {
                code: code.as_str().into(),
                message: message.into(),
                detail: detail.clone(),
                finalized_block: self.finalized_block,
                session,
                opening_cohort,
            },
        );
        error(code, message, detail)
    }

    fn refused_with(&mut self, signer: AccountId, upload: &SubmissionUpload, r: Refusal) -> Reply {
        let code = ErrorCode::parse(&r.code).unwrap();
        self.refused(signer, Some(upload), code, &r.message, r.detail)
    }

    /// The checks shared by opening and reopening a session.
    fn admission_checks(
        &mut self,
        signer: AccountId,
        upload: &SubmissionUpload,
    ) -> Result<(), Reply> {
        let opening = &upload.opening;
        if opening.chain != self.config.chain {
            return Err(self.refused(
                signer,
                Some(upload),
                ErrorCode::WrongChain,
                "another chain",
                Detail::default(),
            ));
        }
        if !self.admitting() {
            let (code, message) = if opening.cohort == self.config.cohort {
                (ErrorCode::AfterCutoff, "admission closed")
            } else {
                (ErrorCode::NotAdmitting, "no cohort is admitting")
            };
            return Err(self.refused(signer, Some(upload), code, message, Detail::default()));
        }
        if opening.cohort != self.config.cohort {
            let detail = Detail {
                admitting: Some(self.config.cohort),
                ..Default::default()
            };
            return Err(self.refused(
                signer,
                Some(upload),
                ErrorCode::WrongCohort,
                "the opening names another cohort",
                detail,
            ));
        }
        if opening.hotkey != signer {
            return Err(self.refused(
                signer,
                Some(upload),
                ErrorCode::HotkeyMismatch,
                "the opening names another hotkey",
                Detail::default(),
            ));
        }
        let fits = PartPlan::new(
            upload.artifact_len,
            self.config.part_size,
            self.config.max_parts,
        )
        .is_ok();
        if upload.artifact_len > self.config.max_object_len || !fits {
            return Err(self.refused(
                signer,
                Some(upload),
                ErrorCode::ObjectTooLarge,
                "too large",
                Detail::default(),
            ));
        }
        if !self.registrations.contains_key(&signer) {
            return Err(self.refused(
                signer,
                Some(upload),
                ErrorCode::NotRegistered,
                "not registered",
                Detail::default(),
            ));
        }
        if let Err(r) = self.check_commitment(upload, None) {
            return Err(self.refused_with(signer, upload, r));
        }
        Ok(())
    }

    /// The one-artifact rule: refuses while another live session opens the
    /// same commitment; supersedes live sessions of older commitments.
    fn one_artifact(
        &mut self,
        signer: AccountId,
        id: &str,
        upload: &SubmissionUpload,
    ) -> Result<(), Reply> {
        let live: Vec<String> = self
            .sessions
            .values()
            .filter(|s| {
                s.hotkey == signer
                    && s.id != id
                    && s.upload.opening.cohort == self.config.cohort
                    && !s.state.ended()
            })
            .map(|s| s.id.clone())
            .collect();
        for other in &live {
            if self.sessions[other].upload.commitment == upload.commitment {
                let detail = Detail {
                    session: Some(other.clone()),
                    ..Default::default()
                };
                return Err(self.refused(
                    signer,
                    Some(upload),
                    ErrorCode::SecondArtifact,
                    "another live session for this commitment",
                    detail,
                ));
            }
        }
        for other in live {
            let session = self.sessions.get_mut(&other).unwrap();
            session.state = SessionState::Superseded;
            session.parts.clear();
        }
        Ok(())
    }

    fn create(&mut self, signer: AccountId, body: &[u8]) -> Reply {
        let Ok(upload) = SubmissionUpload::from_bytes(body) else {
            return self.refused(
                signer,
                None,
                ErrorCode::BadRequest,
                "not a submission-upload record",
                Detail::default(),
            );
        };
        if let Err(reply) = self.admission_checks(signer, &upload) {
            return reply;
        }
        let id = upload.digest().unwrap().hex();
        if let Some(existing) = self.sessions.get(&id) {
            // Returned in whatever state it is; reopening is its own request.
            return ok(200, &self.session_json(existing));
        }
        if let Err(reply) = self.one_artifact(signer, &id, &upload) {
            return reply;
        }
        let session = MockSession {
            id: id.clone(),
            hotkey: signer,
            upload,
            attempt: 1,
            parts: BTreeMap::new(),
            state: SessionState::Uploading,
            refusal: None,
            position: None,
            next_cohort: None,
            pending: None,
        };
        self.sessions.insert(id.clone(), session);
        ok(201, &self.session_json(&self.sessions[&id]))
    }

    fn owned(&mut self, signer: AccountId, id: &str) -> Result<&mut MockSession, Reply> {
        match self.sessions.get_mut(id) {
            None => Err(error(
                ErrorCode::NotFound,
                "no such session",
                Detail::default(),
            )),
            Some(s) if s.hotkey != signer => {
                Err(error(ErrorCode::Forbidden, "not yours", Detail::default()))
            }
            Some(s) => Ok(s),
        }
    }

    fn stale(id: &str, current: u32) -> Reply {
        error(
            ErrorCode::StaleAttempt,
            "the request names another attempt than the session's current one",
            Detail {
                session: Some(id.into()),
                attempt: Some(current),
                ..Default::default()
            },
        )
    }

    fn read(&mut self, signer: AccountId, id: &str) -> Reply {
        let session = match self.owned(signer, id) {
            Ok(s) => s,
            Err(reply) => return reply,
        };
        if let Some(result) = session.pending.take() {
            match result {
                Ok(position) => {
                    session.state = SessionState::Verified;
                    session.position = Some(position);
                }
                Err(r) => {
                    session.state = SessionState::Refused;
                    session.refusal = Some(r);
                }
            }
        }
        ok(200, &self.session_json(&self.sessions[id]))
    }

    fn abort(&mut self, signer: AccountId, id: &str, attempt: u32) -> Reply {
        let closed = !self.admitting();
        let session = match self.owned(signer, id) {
            Ok(s) => s,
            Err(reply) => return reply,
        };
        if closed {
            return error(
                ErrorCode::AfterCutoff,
                "admission closed",
                Detail::default(),
            );
        }
        if attempt != session.attempt {
            return Self::stale(id, session.attempt);
        }
        match session.state {
            SessionState::Aborted => {}
            SessionState::Uploading | SessionState::Verified | SessionState::Refused => {
                session.state = SessionState::Aborted;
                session.parts.clear();
                session.pending = None;
            }
            _ => {
                return error(
                    ErrorCode::SessionNotUploading,
                    "cannot abort in this state",
                    Detail::default(),
                );
            }
        }
        ok(200, &self.session_json(&self.sessions[id]))
    }

    fn reopen(&mut self, signer: AccountId, id: &str, attempt: u32) -> Reply {
        let (current, state, upload) = match self.owned(signer, id) {
            Ok(s) => (s.attempt, s.state, s.upload.clone()),
            Err(reply) => return reply,
        };
        if attempt == current && state != SessionState::Aborted {
            // A repeat of the reopen that started this attempt.
            return ok(200, &self.session_json(&self.sessions[id]));
        }
        if attempt != current.saturating_add(1) || state != SessionState::Aborted {
            return Self::stale(id, current);
        }
        if let Err(reply) = self.admission_checks(signer, &upload) {
            return reply;
        }
        if let Err(reply) = self.one_artifact(signer, id, &upload) {
            return reply;
        }
        let session = self.sessions.get_mut(id).unwrap();
        session.attempt = attempt;
        session.state = SessionState::Uploading;
        session.parts.clear();
        session.refusal = None;
        session.position = None;
        ok(200, &self.session_json(&self.sessions[id]))
    }

    fn put_part(
        &mut self,
        signer: AccountId,
        id: &str,
        n: u32,
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> Reply {
        *self.counters.part_puts.entry(n).or_default() += 1;
        if let Some(i) = self.fail_part_once.iter().position(|p| *p == n) {
            self.fail_part_once.remove(i);
            return error(
                ErrorCode::Unavailable,
                "injected failure",
                Detail::default(),
            );
        }
        let mut received = body.to_vec();
        if let Some(i) = self.corrupt_part_once.iter().position(|p| *p == n) {
            self.corrupt_part_once.remove(i);
            received[0] ^= 0xff;
        }
        let closed = !self.admitting();
        let fallback = self.fallback_digests;
        let (part_size, max_parts) = (self.config.part_size, self.config.max_parts);
        let session = match self.owned(signer, id) {
            Ok(s) => s,
            Err(reply) => return reply,
        };
        if closed {
            return error(
                ErrorCode::AfterCutoff,
                "admission closed",
                Detail::default(),
            );
        }
        if session.state != SessionState::Uploading {
            return error(
                ErrorCode::SessionNotUploading,
                "not uploading",
                Detail::default(),
            );
        }
        let plan = PartPlan::new(session.upload.artifact_len, part_size, max_parts).unwrap();
        let Some((_, expected)) = plan.range(n) else {
            return error(ErrorCode::PartOutOfRange, "no such part", Detail::default());
        };
        if received.len() as u64 != expected {
            return error(
                ErrorCode::PartLengthMismatch,
                "wrong length",
                Detail::default(),
            );
        }
        let claimed = headers
            .get(header::CONTENT_SHA256)
            .cloned()
            .unwrap_or_default();
        let digest = if fallback {
            claimed
        } else {
            let digest = object_sha256(&received).hex();
            if digest != claimed {
                return error(ErrorCode::PartDigestMismatch, "digest", Detail::default());
            }
            digest
        };
        session.parts.insert(n, (digest.clone(), received));
        ok(
            200,
            &PartAck {
                part: n,
                len: expected,
                sha256: digest,
            },
        )
    }

    fn verify(&self, upload: &SubmissionUpload, object: &[u8]) -> Result<Position, Refusal> {
        if object.len() as u64 != upload.artifact_len
            || object_sha256(object) != upload.artifact_sha256
        {
            return Err(refusal(
                ErrorCode::ObjectMismatch,
                "object digest",
                Detail::default(),
            ));
        }
        let artifact = VerifiedArtifact::from_bytes(object).map_err(|e| {
            refusal(
                ErrorCode::ArtifactInvalid,
                "artifact reader refused it",
                Detail {
                    reason: Some(format!("{e:?}")),
                    ..Default::default()
                },
            )
        })?;
        if artifact.header.contract != self.config.output_contract {
            return Err(refusal(
                ErrorCode::ContractMismatch,
                "contract",
                Detail::default(),
            ));
        }
        let position = self.check_commitment(upload, None)?;
        let row = self.counted(&upload.opening.hotkey, None).unwrap();
        let commitment = FinalizedCommitment {
            chain: &self.config.chain,
            committer: self.registrations[&upload.opening.hotkey],
            block: row.block,
            extrinsic_index: row.extrinsic,
            payload: &row.payload,
        };
        upload
            .admit(self.config.cohort, &commitment, &artifact)
            .map_err(|_| refusal(ErrorCode::ContentMismatch, "content", Detail::default()))?;
        let check = match &self.check_hook {
            None => None,
            Some(CheckHook::Refuse(code)) => Some(code.clone()),
            Some(CheckHook::Spec) => {
                let mut check = slop_ninja_artifact_check::ObjectCheck::new();
                match check.update(object) {
                    Err(refused) => Some(refused.code().to_owned()),
                    Ok(()) => check.finish().err().map(|r| r.code().to_owned()),
                }
            }
        };
        if let Some(code) = check {
            return Err(refusal(
                ErrorCode::CheckFailed,
                "an artifact check refused it",
                Detail {
                    check: Some(code),
                    ..Default::default()
                },
            ));
        }
        Ok(position)
    }

    fn complete(&mut self, signer: AccountId, id: &str) -> Reply {
        let closed = !self.admitting();
        let (part_size, max_parts) = (self.config.part_size, self.config.max_parts);
        let dropped = std::mem::take(&mut self.drop_parts_before_complete);
        let session = match self.owned(signer, id) {
            Ok(s) => s,
            Err(reply) => return reply,
        };
        if closed {
            return error(
                ErrorCode::AfterCutoff,
                "admission closed",
                Detail::default(),
            );
        }
        match session.state {
            SessionState::Uploading => {}
            SessionState::Verifying | SessionState::Verified => {
                return ok(202, &self.session_json(&self.sessions[id]));
            }
            _ => {
                return error(
                    ErrorCode::SessionNotUploading,
                    "not uploading",
                    Detail::default(),
                );
            }
        }
        for part in dropped {
            session.parts.remove(&part);
        }
        let plan = PartPlan::new(session.upload.artifact_len, part_size, max_parts).unwrap();
        let missing: Vec<u32> = plan.missing(|n| session.parts.contains_key(&n)).collect();
        if !missing.is_empty() {
            return error(
                ErrorCode::PartsMissing,
                "parts missing",
                Detail {
                    missing: Some(missing),
                    ..Default::default()
                },
            );
        }
        let object: Vec<u8> = session
            .parts
            .values()
            .flat_map(|(_, bytes)| bytes.iter().copied())
            .collect();
        let upload = session.upload.clone();
        session.state = SessionState::Verifying;
        let result = self.verify(&upload, &object);
        let session = self.sessions.get_mut(id).unwrap();
        session.pending = Some(result);
        ok(202, &self.session_json(&self.sessions[id]))
    }

    fn hotkey_view(&mut self, signer: AccountId, cohort: u64, hotkey: &str) -> Reply {
        if hotkey != ss58(&signer) {
            return error(ErrorCode::Forbidden, "another hotkey", Detail::default());
        }
        let cutoff = if cohort == self.config.cohort {
            self.cutoff_block
        } else {
            None
        };
        let open = self.config.open_block;
        let view = HotkeyView {
            schema: SCHEMA_HOTKEY.into(),
            cohort,
            hotkey: hotkey.into(),
            finalized_block: self.finalized_block,
            registration: self.registrations.get(&signer).map(|r| Registration {
                uid: r.uid,
                registration_block: r.registration_block,
            }),
            commitment: self.counted(&signer, cutoff).map(|row| CommitmentView {
                block: row.block,
                block_hash: Some(block_hash(row.block)),
                extrinsic: row.extrinsic,
                payload: hex::encode(&row.payload),
                payload_cohort: parse_chain_payload(&row.payload).ok().map(|(c, _)| c),
                in_window: row.block >= open && cutoff.is_none_or(|c| row.block < c),
            }),
            cutoff_block: cutoff,
            sessions: self
                .sessions
                .values()
                .filter(|s| s.hotkey == signer && s.upload.opening.cohort == cohort)
                .map(|s| SessionSummary {
                    session: s.id.clone(),
                    state: s.state.as_str().into(),
                    attempt: Some(s.attempt),
                    refusal: s.refusal.clone(),
                })
                .collect(),
            last_refusal: self.last_refusals.get(&(signer, cohort)).cloned(),
        };
        ok(200, &view)
    }

    fn close_at(&mut self, cutoff_block: u64) {
        self.cutoff_block = Some(cutoff_block);
        let cohort = self.config.cohort;
        let ids: Vec<String> = self.sessions.keys().cloned().collect();
        for id in &ids {
            let session = self.sessions.get_mut(id).unwrap();
            if session.upload.opening.cohort != cohort {
                continue;
            }
            if let Some(result) = session.pending.take() {
                match result {
                    Ok(position) => {
                        session.state = SessionState::Verified;
                        session.position = Some(position);
                    }
                    Err(r) => {
                        session.state = SessionState::Refused;
                        session.refusal = Some(r);
                    }
                }
            }
            if session.state == SessionState::Uploading {
                session.state = SessionState::Refused;
                session.refusal = Some(refusal(
                    ErrorCode::IncompleteAtCutoff,
                    "not completed before the close",
                    Detail::default(),
                ));
                session.parts.clear();
            }
        }
        let mut ranked = Vec::new();
        for id in &ids {
            let session = &self.sessions[id];
            if session.state != SessionState::Verified || session.upload.opening.cohort != cohort {
                continue;
            }
            match self.check_commitment(&session.upload.clone(), Some(cutoff_block)) {
                Ok(position) => ranked.push(((position.block, position.extrinsic), id.clone())),
                Err(r) => {
                    let code = if r.code == ErrorCode::CommitmentMismatch.as_str() {
                        refusal(
                            ErrorCode::CommitmentOverwritten,
                            "a later commitment replaced it",
                            Detail::default(),
                        )
                    } else {
                        r
                    };
                    let session = self.sessions.get_mut(id).unwrap();
                    session.state = SessionState::Refused;
                    session.refusal = Some(code);
                }
            }
        }
        ranked.sort();
        let cap = usize::from(self.config.roster_cap);
        let next = cohort + 1;
        for (i, (position, id)) in ranked.into_iter().enumerate() {
            let session = self.sessions.get_mut(&id).unwrap();
            session.position = Some(Position {
                block: position.0,
                extrinsic: position.1,
            });
            if i < cap {
                session.state = SessionState::Admitted;
            } else {
                session.state = SessionState::Overflow;
                session.next_cohort = Some(next);
                session.parts.clear();
            }
        }
    }
}
