//! A local mock of the coordinator's admission routes, contract
//! `slop_ninja.admission/v2` (`docs/testnet/admission-api.md`), and of R2's
//! presigned `PutObject`, on two loopback listeners.
//!
//! **The coordinator** (`url()`): signed requests; sessions keyed by the
//! envelope digest; a header URL at creation; `urls` settling the header
//! from the stub's stored object (size and checksum, then the header's
//! decode, contract and committed content) before it mints one URL per file
//! not yet recorded; `complete` heading every object and queueing the
//! asynchronous verification (`SubmissionUpload::admit_header` and an
//! optional artifact-check stage over the stored files); `repair` for
//! recorded objects that went missing; the one-artifact rule; the last
//! refused request per hotkey; and the cutoff with ordering, the roster cap
//! and overflow. URLs are signed with `slop_ninja_r2_presign` under a test
//! credential, with the versioned URL policy's life.
//!
//! **The R2 stub** (`r2_url()`, reached through the client's loopback
//! endpoint override; URLs name the S3 host [`S3_HOST`]): it enforces what
//! R2 is relied on for. The SigV4 signature is recomputed over the request's
//! `host`, `content-length`, `if-none-match` and `x-amz-checksum-sha256`
//! (any change, or a missing one, is 403 `SignatureDoesNotMatch`); an
//! unsigned `x-amz-*` header, `content-encoding` or chunked transfer
//! encoding is refused; an expired URL is 403 `ExpiredRequest` (against the
//! mock clock); a body whose SHA-256 is not the signed checksum is 400
//! `BadDigest` and stores nothing; an existing key is 412
//! `PreconditionFailed`. Faults inject what R2 or the network can do.
//!
//! Its "chain" is a commitment log the test fills: every finalized commitment
//! of a hotkey, in order, as the coordinator records them from its finalized
//! cursor. Session opening uses a hotkey's latest entry; the cutoff uses its
//! last entry before the cutoff block, so a later commitment (for the next
//! cohort, say) never replaces the one that counted.
//!
//! With `upload_gate` set, every coordinator request first passes the
//! key-free upload Worker's own front sequence
//! (`slop_ninja_admission_gate::decide`), before this mock decides as the
//! cohort authority.

#![allow(dead_code, clippy::result_large_err)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use slop_ninja_admission_wire::json::{
    Admitting, ArtifactSpecRef, Chain, CohortInfo, CommitmentView, Detail, ErrorBody, HotkeyView,
    LastRefusal, NextCohort, ObjectState, Position, Refusal, Registration, SCHEMA_COHORT,
    SCHEMA_HOTKEY, SCHEMA_SESSION, Session, SessionSummary, Upload, UploadPolicy,
};
use slop_ninja_admission_wire::{
    CONTRACT, ErrorCode, RequestScope, Route, SELECTION_EARLIEST_COMMITMENT_V1, SessionState,
    header, parse_hex32, parse_signature, parse_valid_until, request_message,
};
use slop_ninja_artifact_check::Version;
use slop_ninja_protocol::submission::{FinalizedCommitment, SubmissionUpload, parse_chain_payload};
use slop_ninja_protocol::{
    AccountId, ArtifactHeader, ChainBinding, Digest, Error as ProtocolError, NeuronIdentity,
    Record, VerifiedHeader, object_sha256,
};
use slop_ninja_r2_presign::layout::{ObjectId, ObjectKind};
use slop_ninja_r2_presign::policy;
use slop_ninja_r2_presign::presign::{Binding, Credentials, Target, presign_put};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The S3 host the mock's URLs name (an R2 S3 host shape).
pub const S3_HOST: &str = "mockacct.r2.cloudflarestorage.com";
/// The admission bucket the mock's URLs name.
pub const BUCKET: &str = "mock-admission-testnet";
const KEY_ID: &str = "MOCKKEYID0123456789ABCDEF0123456";
const SECRET: &[u8] = b"mock-secret-access-key-for-tests";
/// The mock clock's start (Unix seconds).
pub const CLOCK_START: u64 = 1_793_846_400;
const HEADER: &str = "header";

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
    pub request_validity_blocks: u64,
}

#[derive(Clone, Debug)]
pub struct CommitmentRow {
    pub block: u64,
    pub extrinsic: u32,
    pub payload: Vec<u8>,
}

/// The artifact-check stage of the admission run (and the header check).
#[derive(Clone, Debug)]
pub enum CheckHook {
    /// Run the artifact spec: the header-only checks at `urls`, then the
    /// hash-free files check over the stored files.
    Spec,
    /// Refuse every artifact at the admission run with this check code.
    Refuse(String),
}

/// Which PUT a fault applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    Header,
    File,
    Any,
}

/// What R2 or the network does to one matching PUT (once).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Answer this status and S3 code after reading the body; store nothing.
    Status(u16, &'static str),
    /// 403 `ExpiredRequest`.
    Expire,
    /// Flip a byte as if it changed in transit: R2's checksum refuses it.
    Corrupt,
    /// Close the connection without an answer.
    Drop,
}

/// A minted URL the coordinator damages, to test the client's checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BadUrl {
    /// Another host.
    Host(String),
    /// Another key (another commitment's prefix).
    Key,
    /// An extra query parameter (for example a multipart one).
    ExtraQuery,
    /// Another checksum header than the committed digest.
    Checksum,
}

#[derive(Clone, Debug)]
pub struct StoredObject {
    pub bytes: Vec<u8>,
    pub etag: u64,
}

#[derive(Debug, Default)]
pub struct R2 {
    pub objects: BTreeMap<String, StoredObject>,
    /// Faults, applied in order to the first matching PUT each.
    pub faults: Vec<(Match, Fault)>,
    /// PUT requests per key that reached the stub.
    pub puts: BTreeMap<String, u32>,
    /// The request header names of every PUT, in order.
    pub put_headers: Vec<Vec<String>>,
    /// Every refused PUT: key, status, S3 code.
    pub refused: Vec<(String, u16, String)>,
    /// Body bytes received in all.
    pub bytes_received: u64,
    next_etag: u64,
}

#[derive(Clone, Debug)]
struct MockSession {
    id: String,
    upload: SubmissionUpload,
    hotkey: AccountId,
    /// The recorded header, once settled.
    header: Option<ArtifactHeader>,
    /// Recorded objects (`header` or a file digest) and their entity tags.
    recorded: BTreeMap<String, u64>,
    /// Objects a repair URL was minted for (their next entity tag is taken).
    repairing: BTreeSet<String>,
    state: SessionState,
    refusal: Option<Refusal>,
    position: Option<Position>,
    next_cohort: Option<u64>,
    /// Verification result applied on the next read, so completion is
    /// observed as `verifying` first.
    pending: Option<Result<Position, Refusal>>,
    /// Publication recorded: repair answers `not_needed`.
    published: bool,
    /// Constant-size mint aggregate: URLs minted.
    mints: u32,
}

#[derive(Debug, Default)]
pub struct Counters {
    pub requests: u32,
    /// Requests per route kind, for example `cohort` or `urls`.
    pub routes: BTreeMap<&'static str, u32>,
    /// Requests the upload Worker's gate refused (`upload_gate`).
    pub gate_refused: u32,
    /// URLs minted.
    pub minted: u32,
}

pub struct MockState {
    pub config: MockConfig,
    /// This mock's base URL, which the public snapshot names as the coordinator.
    pub base: String,
    /// The coordinator the public snapshot advertises (default: `base`).
    pub advertised: Option<String>,
    pub finalized_block: u64,
    /// The wall clock URLs are signed by and the stub checks expiry against.
    pub now: u64,
    pub registrations: HashMap<AccountId, NeuronIdentity>,
    /// Each hotkey's finalized commitments, in chain order.
    pub commitments: HashMap<AccountId, Vec<CommitmentRow>>,
    sessions: BTreeMap<String, MockSession>,
    last_refusals: HashMap<(AccountId, u64), LastRefusal>,
    /// The cohort's cutoff block, once admission has closed.
    pub cutoff_block: Option<u64>,
    pub artifact_spec: Option<ArtifactSpecRef>,
    /// Answer `paused` to this many signed requests.
    pub paused_requests: u32,
    /// Answer `rate_limited` to this many signed requests, with this
    /// `Retry-After`.
    pub rate_limited_requests: u32,
    pub retry_after_secs: u64,
    /// The artifact-check stage; `None` stands for checks that pass.
    pub check_hook: Option<CheckHook>,
    /// Answer `urls` with a redirect to this URL.
    pub redirect_urls_to: Option<String>,
    /// Damage every minted URL this way.
    pub bad_urls: Option<BadUrl>,
    /// Pad the cohort response with this many bytes in an unknown field.
    pub pad_cohort_bytes: usize,
    /// Answer this many signed requests with an edge error page (502, HTML,
    /// no contract body), as a Cloudflare gateway failure would.
    pub edge_errors: u32,
    /// Whether `request_expired` carries the cursor (`detail.finalized_block`).
    pub expired_with_cursor: bool,
    /// Lose these stored objects at the next completion request, as a lost
    /// object would, so completion answers `objects_missing`.
    pub drop_before_complete: Vec<Match>,
    /// The block of the commitment reader's last completed poll; `None`
    /// stands for a reader that is always caught up (see `wait_for_reader`).
    pub reader_polled: Option<u64>,
    commit_waits: HashMap<AccountId, (String, u64)>,
    /// The `x-sn-commitment-block` of each session creation, in order.
    pub commitment_block_hints: Vec<Option<u64>>,
    /// Front every request with the upload Worker's gate.
    pub upload_gate: bool,
    pub counters: Counters,
    pub r2: R2,
}

pub struct Mock {
    pub addr: SocketAddr,
    pub r2_addr: SocketAddr,
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
    /// 0: close the connection without an answer.
    status: u16,
    body: String,
    headers: Vec<(&'static str, String)>,
    content_type: &'static str,
}

fn error(code: ErrorCode, message: &str, detail: Detail) -> Reply {
    let status = code.http_status().expect("request error code");
    Reply {
        status,
        body: serde_json::to_string(&ErrorBody::new(code, message, detail)).unwrap(),
        headers: Vec::new(),
        content_type: "application/json",
    }
}

fn ok<T: serde::Serialize>(status: u16, value: &T) -> Reply {
    Reply {
        status,
        body: serde_json::to_string(value).unwrap(),
        headers: Vec::new(),
        content_type: "application/json",
    }
}

fn s3_error(status: u16, code: &str) -> Reply {
    Reply {
        status,
        body: format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code><Message>mock</Message></Error>"
        ),
        headers: Vec::new(),
        content_type: "application/xml",
    }
}

fn ss58(account: &AccountId) -> String {
    rtao_primitives::AccountId(account.0).to_string()
}

/// A stand-in block hash for a block number.
pub fn block_hash(block: u64) -> String {
    object_sha256(&block.to_le_bytes()).hex()
}

/// Unix seconds of a SigV4 `yyyymmddThhmmssZ` stamp.
fn unix_of_stamp(stamp: &str) -> Option<u64> {
    let b = stamp.as_bytes();
    if b.len() != 16 || b[8] != b'T' || b[15] != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| stamp.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(4..6)?, num(6..8)?);
    let (hh, mm, ss) = (num(9..11)?, num(11..13)?, num(13..15)?);
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

fn query_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v)
}

impl Mock {
    pub async fn start(config: MockConfig, finalized_block: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let r2_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let r2_addr = r2_listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(MockState {
            config,
            base: format!("http://{addr}/"),
            advertised: None,
            finalized_block,
            now: CLOCK_START,
            registrations: HashMap::new(),
            commitments: HashMap::new(),
            sessions: BTreeMap::new(),
            last_refusals: HashMap::new(),
            cutoff_block: None,
            artifact_spec: None,
            paused_requests: 0,
            rate_limited_requests: 0,
            retry_after_secs: 0,
            check_hook: None,
            redirect_urls_to: None,
            bad_urls: None,
            pad_cohort_bytes: 0,
            edge_errors: 0,
            expired_with_cursor: true,
            drop_before_complete: Vec::new(),
            reader_polled: None,
            commit_waits: HashMap::new(),
            commitment_block_hints: Vec::new(),
            upload_gate: false,
            counters: Counters::default(),
            r2: R2::default(),
        }));
        for (listener, is_r2) in [(listener, false), (r2_listener, true)] {
            let shared = state.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    let state = shared.clone();
                    tokio::spawn(async move {
                        let _ = serve(stream, state, is_r2).await;
                    });
                }
            });
        }
        Self {
            addr,
            r2_addr,
            state,
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }

    /// The R2 stub's loopback origin, for the client's endpoint override.
    pub fn r2_url(&self) -> String {
        format!("http://{}/", self.r2_addr)
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut MockState) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }

    /// The commitment reader completes a poll at the current cursor.
    pub fn poll(&self) {
        self.with(|s| s.reader_polled = Some(s.finalized_block));
    }

    /// Advances the finalized cursor by `blocks` (the heartbeat), with no poll.
    pub fn advance(&self, blocks: u64) {
        self.with(|s| s.finalized_block += blocks);
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

    /// Deletes the stored objects of a session whose keys `keep` rejects
    /// (a platform loss after admission recorded them).
    pub fn lose_objects(&self, id: &str, lose: impl Fn(&str) -> bool) -> usize {
        self.with(|s| {
            let keys: Vec<String> = s.sessions[id]
                .recorded
                .keys()
                .map(|k| s.key_of(id, k))
                .filter(|key| lose(key))
                .collect();
            for key in &keys {
                s.r2.objects.remove(key);
            }
            keys.len()
        })
    }

    /// Marks a session's publication as recorded.
    pub fn publish(&self, id: &str) {
        self.with(|s| s.sessions.get_mut(id).unwrap().published = true);
    }

    /// PUT requests that reached the stub, in all.
    pub fn puts(&self) -> u32 {
        self.with(|s| s.r2.puts.values().sum())
    }
}

async fn serve(
    mut stream: TcpStream,
    state: Arc<Mutex<MockState>>,
    is_r2: bool,
) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 65_536];
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
    let target = parts.next().unwrap_or_default().to_owned();
    let mut names = Vec::new();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| {
            let k = k.trim().to_ascii_lowercase();
            names.push(k.clone());
            (k, v.trim().to_owned())
        })
        .collect();
    let chunked = headers
        .get("transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let reply = if is_r2 && chunked {
        // R2 is never sent a chunked body by this client; refuse it.
        state.lock().unwrap().r2.put_headers.push(names);
        s3_error(411, "MissingContentLength")
    } else {
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
        let mut state = state.lock().unwrap();
        if is_r2 {
            state.r2.put_headers.push(names);
            state.r2_put(&method, &target, &headers, body)
        } else {
            state.handle(&method, &target, &headers, &body)
        }
    };
    if reply.status == 0 {
        return stream.shutdown().await;
    }
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
        "HTTP/1.1 {} {reason}\r\ncontent-type: {}\r\ncontent-length: {}\r\n{extra}connection: close\r\n\r\n{}",
        reply.status,
        reply.content_type,
        reply.body.len(),
        reply.body
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

enum Settle {
    /// No header object in R2.
    Missing,
    /// The header failed; the session is refused with this (boxed: a
    /// refusal is far larger than the other variant).
    Refused(Box<Refusal>),
}

impl MockState {
    // ------------------------------------------------------------ the R2 stub

    /// The stored key of a session's recorded object (`header` or a digest).
    fn key_of(&self, id: &str, object: &str) -> String {
        let s = &self.sessions[id];
        let kind = if object == HEADER {
            ObjectKind::Header
        } else {
            ObjectKind::File(parse_hex32(object).unwrap())
        };
        ObjectId {
            cohort: s.upload.opening.cohort,
            commitment: s.upload.commitment.0,
            kind,
        }
        .key()
        .as_str()
        .to_owned()
    }

    /// The SHA-256 a key's content is fixed to: a file key's own digest, or
    /// the header digest of the session whose commitment the key names.
    fn committed_sha(&self, id: &ObjectId) -> Option<[u8; 32]> {
        match id.kind {
            ObjectKind::File(sha) => Some(sha),
            ObjectKind::Header => self
                .sessions
                .values()
                .find(|s| {
                    s.upload.commitment.0 == id.commitment && s.upload.opening.cohort == id.cohort
                })
                .map(|s| s.upload.header_sha256.0),
        }
    }

    fn r2_put(
        &mut self,
        method: &str,
        target: &str,
        headers: &HashMap<String, String>,
        body: Vec<u8>,
    ) -> Reply {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let key = path
            .strip_prefix('/')
            .and_then(|p| p.strip_prefix(BUCKET))
            .and_then(|p| p.strip_prefix('/'));
        let key = key.unwrap_or_default().to_owned();
        *self.r2.puts.entry(key.clone()).or_default() += 1;
        self.r2.bytes_received += body.len() as u64;
        let reply = self.r2_decide(method, &key, query, headers, body);
        if reply.status >= 300 || reply.status == 0 {
            let code = slop_ninja_miner::direct::s3_error_code(reply.body.as_bytes())
                .unwrap_or("")
                .to_owned();
            self.r2.refused.push((key, reply.status, code));
        }
        reply
    }

    fn r2_decide(
        &mut self,
        method: &str,
        key: &str,
        query: &str,
        headers: &HashMap<String, String>,
        mut body: Vec<u8>,
    ) -> Reply {
        if method != "PUT" {
            return s3_error(405, "MethodNotAllowed");
        }
        // A multipart or version parameter is answered 501 before any
        // authentication, as R2 does (qualification G11); nothing is stored.
        if ["partNumber", "uploadId", "versionId"]
            .iter()
            .any(|p| query_param(query, p).is_some())
        {
            return s3_error(501, "NotImplemented");
        }
        let Some(id) = ObjectId::parse(key) else {
            return s3_error(403, "AccessDenied");
        };
        // An unsigned x-amz header, or a content encoding, is refused.
        if headers
            .keys()
            .any(|h| h.starts_with("x-amz-") && h != "x-amz-checksum-sha256")
            || headers.contains_key("content-encoding")
        {
            return s3_error(403, "SignatureDoesNotMatch");
        }
        let get = |name: &str| headers.get(name).map(String::as_str);
        let (Some(host), Some(length), Some(if_none_match), Some(checksum)) = (
            get("host"),
            get("content-length"),
            get("if-none-match"),
            get("x-amz-checksum-sha256"),
        ) else {
            return s3_error(403, "SignatureDoesNotMatch");
        };
        let (Some(stamp), Some(expires), Some(signature), Some(credential)) = (
            query_param(query, "X-Amz-Date"),
            query_param(query, "X-Amz-Expires").and_then(|e| e.parse::<u32>().ok()),
            query_param(query, "X-Amz-Signature"),
            query_param(query, "X-Amz-Credential"),
        ) else {
            return s3_error(403, "AuthorizationQueryParametersError");
        };
        if !credential.starts_with(KEY_ID) {
            return s3_error(403, "InvalidAccessKeyId");
        }
        let Some(signed_at) = unix_of_stamp(stamp) else {
            return s3_error(403, "AuthorizationQueryParametersError");
        };
        let Some(sha) = self.committed_sha(&id) else {
            return s3_error(403, "AccessDenied");
        };
        let Ok(len) = length.parse::<u64>() else {
            return s3_error(400, "InvalidArgument");
        };
        // Recompute the signature over what the request carries: the URL
        // signs host, content-length, if-none-match: * and the checksum the
        // key's content is fixed to.
        let expected = presign_put(
            &Credentials {
                access_key_id: KEY_ID,
                secret_access_key: SECRET,
            },
            &Target {
                host,
                bucket: BUCKET,
                key,
            },
            &Binding { len, sha256: sha },
            signed_at,
            expires,
        );
        let expected_signature = expected.as_ref().ok().and_then(|p| {
            query_param(p.url.as_str().split_once('?')?.1, "X-Amz-Signature").map(str::to_owned)
        });
        let checksum_ok = expected
            .as_ref()
            .is_ok_and(|p| p.checksum.as_str() == checksum);
        let query_ok = expected
            .as_ref()
            .is_ok_and(|p| p.url.as_str().split_once('?').map(|(_, q)| q) == Some(query));
        if expected_signature.as_deref() != Some(signature)
            || !checksum_ok
            || if_none_match != "*"
            || !query_ok
        {
            return s3_error(403, "SignatureDoesNotMatch");
        }
        if self.now > signed_at + u64::from(expires) {
            return s3_error(403, "ExpiredRequest");
        }
        let is_header = id.kind == ObjectKind::Header;
        let fault = self.r2.faults.iter().position(|(m, _)| match m {
            Match::Any => true,
            Match::Header => is_header,
            Match::File => !is_header,
        });
        if let Some(i) = fault {
            match self.r2.faults.remove(i).1 {
                Fault::Status(status, code) => return s3_error(status, code),
                Fault::Expire => return s3_error(403, "ExpiredRequest"),
                Fault::Drop => {
                    return Reply {
                        status: 0,
                        body: String::new(),
                        headers: vec![],
                        content_type: "",
                    };
                }
                Fault::Corrupt => {
                    if let Some(b) = body.first_mut() {
                        *b ^= 0xff;
                    }
                }
            }
        }
        if body.len() as u64 != len {
            return s3_error(400, "IncompleteBody");
        }
        // R2 hashes the body as it arrives (the stub stands in for R2).
        if object_sha256(&body).0 != sha {
            return s3_error(400, "BadDigest");
        }
        if self.r2.objects.contains_key(key) {
            return s3_error(412, "PreconditionFailed");
        }
        self.r2.next_etag += 1;
        let etag = self.r2.next_etag;
        self.r2
            .objects
            .insert(key.to_owned(), StoredObject { bytes: body, etag });
        Reply {
            status: 200,
            body: String::new(),
            headers: vec![("etag", format!("\"{etag:032x}\""))],
            content_type: "application/xml",
        }
    }

    /// `head()`: the stored length, SHA-256 and entity tag of a key.
    fn head(&self, key: &str) -> Option<(u64, [u8; 32], u64)> {
        self.r2
            .objects
            .get(key)
            .map(|o| (o.bytes.len() as u64, object_sha256(&o.bytes).0, o.etag))
    }

    // --------------------------------------------------------- the coordinator

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
            Route::Urls { .. } => "urls",
            Route::Complete { .. } => "complete",
            Route::Repair { .. } => "repair",
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
        if self.edge_errors > 0 {
            self.edge_errors -= 1;
            return Reply {
                status: 502,
                body: "<html><body>502 Bad Gateway</body></html>".into(),
                headers: vec![],
                content_type: "text/html",
            };
        }
        if self.upload_gate
            && let Err(reply) = self.gate(method, path, headers, body)
        {
            self.counters.gate_refused += 1;
            return reply;
        }
        let signer = match self.authenticate(method, path, headers, body) {
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
        let hint = headers
            .get(header::COMMITMENT_BLOCK)
            .and_then(|v| v.trim().parse::<u64>().ok());
        match route {
            Route::CreateSession => self.create(signer, body, hint),
            Route::ReadSession { session } => self.read(signer, session),
            Route::Urls { session } => {
                if let Some(target) = &self.redirect_urls_to {
                    return Reply {
                        status: 307,
                        body: String::new(),
                        headers: vec![("location", target.clone())],
                        content_type: "text/plain",
                    };
                }
                self.urls(signer, session)
            }
            Route::Complete { session } => self.complete(signer, session),
            Route::Repair { session } => self.repair(signer, session),
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
                upload_policy: UploadPolicy {
                    id: policy::POLICY_ID.into(),
                    b_min: policy::B_MIN,
                    skew_secs: policy::SKEW_SECS,
                },
                limits: object_sha256(b"mock limits").hex(),
                artifact_spec: self.artifact_spec.clone(),
            }),
            // The next cohort's plan values are the mock cohort's own.
            next: Some(NextCohort {
                cohort: c.cohort + 1,
                earliest_open_epoch: Some(c.opened_epoch + 20),
                admission_tempos: None,
                output_contract: Some(c.output_contract.clone()),
                max_object_len: Some(c.max_object_len),
                artifact_spec: self.artifact_spec.clone(),
            }),
            confirmed_epoch: Some(c.opened_epoch),
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

    /// The upload Worker's front sequence, the very function the Worker
    /// runs (`slop_ninja_admission_gate::decide`).
    fn gate(
        &self,
        method: &str,
        path: &str,
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> Result<(), Reply> {
        use slop_ninja_admission_gate as gate;
        struct MockIo<'a> {
            registrations: &'a HashMap<AccountId, NeuronIdentity>,
            /// The snapshot's repair list: hotkeys with a session whose
            /// recorded objects went missing.
            repair: Vec<AccountId>,
            body: &'a [u8],
        }
        impl gate::Io for MockIo<'_> {
            async fn registered(&mut self, who: &AccountId) -> Result<bool, ()> {
                Ok(self.registrations.contains_key(who))
            }
            async fn repair_listed(&mut self, who: &AccountId) -> Result<bool, ()> {
                Ok(self.repair.contains(who))
            }
            async fn read_body(&mut self, bound: usize) -> Result<Vec<u8>, gate::BodyFailure> {
                if self.body.len() > bound {
                    return Err(gate::BodyFailure::TooLarge);
                }
                Ok(self.body.to_vec())
            }
        }
        let to_reply = |r: gate::Refusal| {
            let w = r.reply();
            Reply {
                status: w.status,
                body: w.body.to_string(),
                headers: w
                    .retry_after
                    .map(|s| vec![("retry-after", s.to_string())])
                    .unwrap_or_default(),
                content_type: "application/json",
            }
        };
        let get = |name: &str| headers.get(name).cloned();
        let request = gate::Request {
            method,
            path,
            headers: gate::Headers {
                hotkey: get(header::HOTKEY),
                valid_until: get(header::VALID_UNTIL),
                content_sha256: get(header::CONTENT_SHA256),
                signature: get(header::SIGNATURE),
                commitment_block: get(header::COMMITMENT_BLOCK),
            },
            content_length: headers.get("content-length").map(String::as_str),
            content_encoding: headers.get("content-encoding").map(String::as_str),
            content_type: headers.get("content-type").map(String::as_str),
        };
        let repair = self.sessions.values().filter(|s| !self.repair_needed(s).is_empty()).map(|s| s.hotkey).collect();
        let mut io = MockIo {
            registrations: &self.registrations,
            repair,
            body,
        };
        let decision = gate::ready(gate::decide(
            &mut io,
            &gate::Access::Open,
            &self.config.chain,
            &request,
        ))
        .expect("the mock's I/O is ready");
        match decision {
            gate::Decision::Refuse(r) => Err(to_reply(r)),
            gate::Decision::Cohort | gate::Decision::Forward { .. } => Ok(()),
        }
    }

    fn authenticate(
        &self,
        method: &str,
        path: &str,
        headers: &HashMap<String, String>,
        body: &[u8],
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
        // The largest body is one SubmissionUpload record.
        if body.len() > slop_ninja_admission_wire::UPLOAD_RECORD_BYTES {
            return Err(error(
                ErrorCode::BadRequest,
                "body too large",
                Detail::default(),
            ));
        }
        if object_sha256(body).0 != content {
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

    /// The objects of a session, header first, then each distinct file once
    /// the header is recorded.
    fn objects(&self, s: &MockSession) -> Vec<ObjectState> {
        let mut out = vec![ObjectState {
            kind: HEADER.into(),
            name: None,
            sha256: s.upload.header_sha256.hex(),
            len: u64::from(s.upload.header_len),
            present: s.recorded.contains_key(HEADER),
        }];
        if let Some(header) = &s.header {
            let mut seen = BTreeSet::new();
            for f in &header.files {
                let sha = f.sha256.hex();
                if seen.insert(sha.clone()) {
                    out.push(ObjectState {
                        kind: "file".into(),
                        name: Some(f.name.clone()),
                        present: s.recorded.contains_key(&sha),
                        sha256: sha,
                        len: f.len,
                    });
                }
            }
        }
        out
    }

    /// Recorded objects that went missing, while still needed: digests (the
    /// header by its digest).
    fn repair_needed(&self, s: &MockSession) -> Vec<String> {
        if s.published {
            return Vec::new();
        }
        s.recorded
            .keys()
            .filter(|k| self.head(&self.key_of(&s.id, k)).is_none())
            .map(|k| {
                if k == HEADER {
                    s.upload.header_sha256.hex()
                } else {
                    k.clone()
                }
            })
            .collect()
    }

    fn session_json(&self, s: &MockSession, uploads: Vec<Upload>) -> Session {
        Session {
            schema: SCHEMA_SESSION.into(),
            session: s.id.clone(),
            cohort: s.upload.opening.cohort,
            hotkey: ss58(&s.hotkey),
            commitment: s.upload.commitment.hex(),
            header_sha256: s.upload.header_sha256.hex(),
            header_len: s.upload.header_len,
            objects: self.objects(s),
            state: s.state.as_str().into(),
            refusal: s.refusal.clone(),
            position: s.position,
            next_cohort: s.next_cohort,
            finalized_block: self.finalized_block,
            uploads,
            repair_needed: self.repair_needed(s),
        }
    }

    /// Mints a URL for one object of session `id` (`header` or a digest),
    /// valid per the URL policy for the artifact's committed size.
    fn mint(&mut self, id: &str, object: &str) -> Upload {
        let key = self.key_of(id, object);
        let s = &self.sessions[id];
        let (sha, len) = if object == HEADER {
            (s.upload.header_sha256.0, u64::from(s.upload.header_len))
        } else {
            let f = s
                .header
                .as_ref()
                .unwrap()
                .files
                .iter()
                .find(|f| f.sha256.hex() == object)
                .unwrap();
            (f.sha256.0, f.len)
        };
        let l_total = u64::from(s.upload.header_len)
            + s.header.as_ref().map_or(0, |h| h.files_len().unwrap());
        let valid = policy::valid_secs(l_total).unwrap();
        let p = presign_put(
            &Credentials {
                access_key_id: KEY_ID,
                secret_access_key: SECRET,
            },
            &Target {
                host: S3_HOST,
                bucket: BUCKET,
                key: &key,
            },
            &Binding { len, sha256: sha },
            self.now,
            valid,
        )
        .unwrap();
        let mut upload = Upload {
            kind: if object == HEADER {
                HEADER.into()
            } else {
                "file".into()
            },
            sha256: hex::encode(sha),
            len,
            url: p.url.as_str().into(),
            content_length: len.to_string(),
            if_none_match: "*".into(),
            checksum_sha256: p.checksum.as_str().into(),
            signed_at: p.signed_at,
            valid_secs: p.valid_secs,
        };
        match &self.bad_urls {
            None => {}
            Some(BadUrl::Host(host)) => upload.url = upload.url.replacen(S3_HOST, host, 1),
            Some(BadUrl::Key) => {
                let other = ObjectId {
                    cohort: 999,
                    commitment: [7; 32],
                    kind: ObjectKind::Header,
                }
                .key();
                upload.url = upload.url.replacen(&key, other.as_str(), 1);
            }
            Some(BadUrl::ExtraQuery) => upload.url.push_str("&partNumber=1"),
            Some(BadUrl::Checksum) => {
                upload.checksum_sha256 = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into()
            }
        }
        let s = self.sessions.get_mut(id).unwrap();
        s.mints += 1;
        self.counters.minted += 1;
        upload
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

    /// At open, a definitive commitment answer waits for the reader: the
    /// first such answer for an opening records the cursor, and until a poll
    /// completed at or after it the answer is `commitment_not_found`.
    fn wait_for_reader(
        &mut self,
        upload: &SubmissionUpload,
        r: Refusal,
        hint: Option<u64>,
    ) -> Refusal {
        let Some(polled) = self.reader_polled else {
            return r;
        };
        if self.cutoff_block.is_some() || r.code == ErrorCode::CommitmentNotFound.as_str() {
            return r;
        }
        let commitment = upload.commitment.hex();
        let first = self
            .finalized_block
            .max(hint.unwrap_or(polled.saturating_add(1)));
        let entry = self
            .commit_waits
            .entry(upload.opening.hotkey)
            .or_insert((commitment.clone(), first));
        if entry.0 != commitment {
            *entry = (commitment, first);
        } else if let Some(h) = hint {
            entry.1 = entry.1.max(h);
        }
        if polled < entry.1 {
            return refusal(
                ErrorCode::CommitmentNotFound,
                "the hotkey's newest commitment may not be logged yet",
                Detail::default(),
            );
        }
        r
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

    fn admission_checks(
        &mut self,
        signer: AccountId,
        upload: &SubmissionUpload,
        hint: Option<u64>,
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
        if upload.header_len == 0
            || upload.header_len > slop_ninja_protocol::MAX_ARTIFACT_HEADER_LEN
        {
            return Err(self.refused(
                signer,
                Some(upload),
                ErrorCode::HeaderTooLarge,
                "header length",
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
            let r = self.wait_for_reader(upload, r, hint);
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
            let keys: Vec<String> = self.sessions[&other]
                .recorded
                .keys()
                .map(|k| self.key_of(&other, k))
                .collect();
            for key in keys {
                self.r2.objects.remove(&key);
            }
            let session = self.sessions.get_mut(&other).unwrap();
            session.state = SessionState::Superseded;
            session.recorded.clear();
        }
        Ok(())
    }

    fn create(&mut self, signer: AccountId, body: &[u8], hint: Option<u64>) -> Reply {
        self.commitment_block_hints.push(hint);
        let Ok(upload) = SubmissionUpload::from_bytes(body) else {
            return self.refused(
                signer,
                None,
                ErrorCode::BadRequest,
                "not a submission-upload record",
                Detail::default(),
            );
        };
        if let Err(reply) = self.admission_checks(signer, &upload, hint) {
            return reply;
        }
        let id = upload.digest().unwrap().hex();
        let status = if self.sessions.contains_key(&id) {
            200
        } else {
            if let Err(reply) = self.one_artifact(signer, &id, &upload) {
                return reply;
            }
            let session = MockSession {
                id: id.clone(),
                hotkey: signer,
                upload,
                header: None,
                recorded: BTreeMap::new(),
                repairing: BTreeSet::new(),
                state: SessionState::Uploading,
                refusal: None,
                position: None,
                next_cohort: None,
                pending: None,
                published: false,
                mints: 0,
            };
            self.sessions.insert(id.clone(), session);
            201
        };
        // While the header is not recorded, the answer carries its URL.
        let s = &self.sessions[&id];
        let uploads = if s.state == SessionState::Uploading && !s.recorded.contains_key(HEADER) {
            vec![self.mint(&id, HEADER)]
        } else {
            Vec::new()
        };
        ok(status, &self.session_json(&self.sessions[&id], uploads))
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

    /// Applies a pending verdict, and takes the entity tag of a repaired
    /// object.
    fn refresh(&mut self, id: &str) {
        let repaired: Vec<(String, u64)> = {
            let s = &self.sessions[id];
            s.repairing
                .iter()
                .filter_map(|k| {
                    self.head(&self.key_of(id, k))
                        .map(|(_, _, etag)| (k.clone(), etag))
                })
                .collect()
        };
        let session = self.sessions.get_mut(id).unwrap();
        for (key, etag) in repaired {
            session.recorded.insert(key.clone(), etag);
            session.repairing.remove(&key);
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
    }

    fn read(&mut self, signer: AccountId, id: &str) -> Reply {
        if let Err(reply) = self.owned(signer, id) {
            return reply;
        }
        self.refresh(id);
        ok(200, &self.session_json(&self.sessions[id], Vec::new()))
    }

    /// Settles the header from R2: its size and stored checksum, then its
    /// decode, contract and committed content (and, with the spec hook, the
    /// spec's header-only checks).
    fn settle_header(&mut self, id: &str) -> Result<(), Settle> {
        if self.sessions[id].recorded.contains_key(HEADER) {
            return Ok(());
        }
        let key = self.key_of(id, HEADER);
        let upload = self.sessions[id].upload.clone();
        let Some((len, sha, etag)) = self.head(&key) else {
            return Err(Settle::Missing);
        };
        if len != u64::from(upload.header_len) || sha != upload.header_sha256.0 {
            self.r2.objects.remove(&key);
            return Err(Settle::Refused(Box::new(refusal(
                ErrorCode::ObjectMismatch,
                "the stored header is not the envelope's",
                Detail::default(),
            ))));
        }
        let bytes = self.r2.objects[&key].bytes.clone();
        let verified = match VerifiedHeader::from_stored(&bytes, upload.header_sha256) {
            Ok(v) => v,
            Err(e) => {
                return Err(Settle::Refused(Box::new(refusal(
                    ErrorCode::ArtifactInvalid,
                    "the header does not decode",
                    Detail {
                        reason: Some(format!("{e:?}")),
                        ..Default::default()
                    },
                ))));
            }
        };
        if verified.header.contract != self.config.output_contract {
            return Err(Settle::Refused(Box::new(refusal(
                ErrorCode::ContractMismatch,
                "contract",
                Detail::default(),
            ))));
        }
        if verified.content != upload.opening.content {
            return Err(Settle::Refused(Box::new(refusal(
                ErrorCode::ContentMismatch,
                "content",
                Detail::default(),
            ))));
        }
        if matches!(self.check_hook, Some(CheckHook::Spec))
            && let Err(r) = slop_ninja_artifact_check::mapping::check_header_for(
                &verified.header,
                Version::LATEST,
            )
        {
            return Err(Settle::Refused(Box::new(refusal(
                ErrorCode::CheckFailed,
                "a header check refused it",
                Detail {
                    check: Some(r.code().into()),
                    ..Default::default()
                },
            ))));
        }
        let s = self.sessions.get_mut(id).unwrap();
        s.header = Some(verified.header);
        s.recorded.insert(HEADER.into(), etag);
        Ok(())
    }

    fn end_refused(&mut self, id: &str, r: Refusal) {
        let s = self.sessions.get_mut(id).unwrap();
        s.state = SessionState::Refused;
        s.refusal = Some(r);
    }

    fn urls(&mut self, signer: AccountId, id: &str) -> Reply {
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
        if session.state != SessionState::Uploading {
            return error(
                ErrorCode::SessionNotUploading,
                "not uploading",
                Detail::default(),
            );
        }
        match self.settle_header(id) {
            Ok(()) => {}
            Err(Settle::Missing) => {
                return error(
                    ErrorCode::HeaderMissing,
                    "PUT the header first",
                    Detail::default(),
                );
            }
            Err(Settle::Refused(r)) => {
                self.end_refused(id, *r);
                return ok(200, &self.session_json(&self.sessions[id], Vec::new()));
            }
        }
        let missing: Vec<String> = self
            .objects(&self.sessions[id])
            .into_iter()
            .filter(|o| o.kind != HEADER && !o.present)
            .map(|o| o.sha256)
            .collect();
        let uploads = missing
            .iter()
            .map(|sha| self.mint(id, sha))
            .collect();
        ok(200, &self.session_json(&self.sessions[id], uploads))
    }

    /// The admission run: the commitment, the header binding and the
    /// artifact-check stage over the stored files (no hash: the files check
    /// takes the header's digests).
    fn verify(&self, id: &str) -> Result<Position, Refusal> {
        let s = &self.sessions[id];
        let upload = &s.upload;
        let header = s.header.clone().unwrap();
        let position = self.check_commitment(upload, None)?;
        let row = self.counted(&upload.opening.hotkey, None).unwrap();
        let commitment = FinalizedCommitment {
            chain: &self.config.chain,
            committer: self.registrations[&upload.opening.hotkey],
            block: row.block,
            extrinsic_index: row.extrinsic,
            payload: &row.payload,
        };
        let verified = VerifiedHeader {
            sha256: upload.header_sha256,
            len: upload.header_len,
            content: header.content().unwrap().digest().unwrap(),
            header: header.clone(),
        };
        upload
            .admit_header(self.config.cohort, &commitment, &verified)
            .map_err(|_| refusal(ErrorCode::ContentMismatch, "content", Detail::default()))?;
        let check = match &self.check_hook {
            None => None,
            Some(CheckHook::Refuse(code)) => Some(code.clone()),
            Some(CheckHook::Spec) => {
                let result = slop_ninja_artifact_check::check_files_with(
                    &header,
                    Version::LATEST,
                    None,
                    |f| {
                        let key = self.key_of(id, &f.sha256.hex());
                        let bytes = self.r2.objects[&key].bytes.clone();
                        let len = bytes.len() as u64;
                        Ok((std::io::Cursor::new(bytes), len))
                    },
                );
                result.err().map(|r| r.code().to_owned())
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
        let dropped = std::mem::take(&mut self.drop_before_complete);
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
                return ok(202, &self.session_json(&self.sessions[id], Vec::new()));
            }
            _ => {
                return error(
                    ErrorCode::SessionNotUploading,
                    "not uploading",
                    Detail::default(),
                );
            }
        }
        // Lost objects (a test fault): never recorded, so heads miss them.
        for m in dropped {
            let objects = self.objects(&self.sessions[id]);
            let lost = objects.iter().find(|o| {
                !o.present && (m == Match::Any || (m == Match::File) == (o.kind != HEADER))
            });
            if let Some(o) = lost {
                let key = self.key_of(id, if o.kind == HEADER { HEADER } else { &o.sha256 });
                self.r2.objects.remove(&key);
            }
        }
        match self.settle_header(id) {
            Ok(()) => {}
            Err(Settle::Missing) => {
                let header = self.sessions[id].upload.header_sha256.hex();
                return error(
                    ErrorCode::ObjectsMissing,
                    "objects missing",
                    Detail {
                        missing: Some(vec![header]),
                        ..Default::default()
                    },
                );
            }
            Err(Settle::Refused(r)) => {
                self.end_refused(id, *r);
                return ok(200, &self.session_json(&self.sessions[id], Vec::new()));
            }
        }
        let mut missing = Vec::new();
        for o in self.objects(&self.sessions[id]) {
            if o.kind == HEADER || o.present {
                continue;
            }
            let key = self.key_of(id, &o.sha256);
            match self.head(&key) {
                Some((len, sha, etag)) if len == o.len && hex::encode(sha) == o.sha256 => {
                    self.sessions
                        .get_mut(id)
                        .unwrap()
                        .recorded
                        .insert(o.sha256.clone(), etag);
                }
                Some(_) => {
                    self.r2.objects.remove(&key);
                    missing.push(o.sha256);
                }
                None => missing.push(o.sha256),
            }
        }
        if !missing.is_empty() {
            return error(
                ErrorCode::ObjectsMissing,
                "objects missing",
                Detail {
                    missing: Some(missing),
                    ..Default::default()
                },
            );
        }
        self.sessions.get_mut(id).unwrap().state = SessionState::Verifying;
        let result = self.verify(id);
        self.sessions.get_mut(id).unwrap().pending = Some(result);
        ok(202, &self.session_json(&self.sessions[id], Vec::new()))
    }

    fn repair(&mut self, signer: AccountId, id: &str) -> Reply {
        if let Err(reply) = self.owned(signer, id) {
            return reply;
        }
        self.refresh(id);
        let s = &self.sessions[id];
        let needed: Vec<String> = s
            .recorded
            .keys()
            .filter(|k| !s.published && self.head(&self.key_of(id, k)).is_none())
            .cloned()
            .collect();
        if needed.is_empty() {
            return error(ErrorCode::NotNeeded, "nothing to repair", Detail::default());
        }
        let uploads = needed.iter().map(|k| self.mint(id, k)).collect();
        self.sessions.get_mut(id).unwrap().repairing.extend(needed);
        ok(200, &self.session_json(&self.sessions[id], uploads))
    }

    fn hotkey_view(&mut self, signer: AccountId, cohort: u64, hotkey: &str) -> Reply {
        if hotkey != ss58(&signer) {
            return error(ErrorCode::Forbidden, "another hotkey", Detail::default());
        }
        let ids: Vec<String> = self.sessions.keys().cloned().collect();
        for id in &ids {
            self.refresh(id);
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
                    refusal: s.refusal.clone(),
                    repair_needed: self.repair_needed(s),
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
            if self.sessions[id].upload.opening.cohort != cohort {
                continue;
            }
            self.refresh(id);
            if self.sessions[id].state == SessionState::Uploading {
                self.end_refused(
                    id,
                    refusal(
                        ErrorCode::IncompleteAtCutoff,
                        "not completed before the close",
                        Detail::default(),
                    ),
                );
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
                    self.end_refused(id, code);
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
            }
        }
    }
}
