//! The published-row fetcher: reads the coordinator's signed row and the
//! authorization it was committed under from the public status host and
//! assembles the coordinator-row bundle that [`crate::package_v2::verify`]
//! checks (the copy model, user decision of 2026-11-04).
//!
//! The layout is the publisher's `v1` (`docs/testnet/public-layout.md`).
//! A session reads one document, `v1/authorizations/current.json` (schema
//! v2), and the content-addressed signed objects it names:
//!
//! - the revocation set, if any: the latest signed revocation of every
//!   revoked target, concatenated. Every entry is checked and journaled
//!   ([`crate::revocations`]) before anything else is read, so it takes
//!   effect whatever the rest of the session does, and a validator that
//!   missed earlier revocations still journals each of them. An entry that
//!   does not verify under the pinned keys is skipped, never journaled;
//! - the authorization validators commit under now (launch or cohort) and
//!   any renew-by hint refresh of it;
//! - `row`: the coordinator's signed row ([`slop_ninja_protocol::CoordinatorRow`]),
//!   signed when it committed under that authorization. Until the
//!   coordinator publishes one, the session reports `row_unpublished` and the
//!   installed bundle keeps committing.
//!
//! No cohort record, receipt or index is read: the coordinator keeps them
//! private, and nothing is recomputed. Every object is checked against its
//! listed SHA-256 and length, then authenticated by the protocol crate under
//! the pinned trust roots. A lying pointer can make a session fail; it cannot
//! change what is verified, and it cannot grow the process past the session
//! bound: the authorization, renewal and revocation objects are fixed-size
//! records whose listed length must be the schema's exact signed length (for
//! the revocation set a whole multiple of it), and the row's listed length
//! must be within the protocol's longest row. Nothing is cached on disk.
//!
//! A session whose pointer and held target are byte-identical to those of
//! the last session that produced a bundle reports [`Outcome::Unchanged`]
//! without reading or verifying any object again. One session at a time
//! holds the cache directory's lock (`<cache>/.lock`). What an earlier
//! layout cached there (`cohorts/`, `authorizations/`: cohort records and
//! the receipts an installed v3 bundle references, some of which the host
//! has since deleted under N+2) is never touched: it is what a rollback to
//! the earlier release needs to verify its installed bundle again. The
//! operator removes it once this release is accepted (validator README,
//! Install).

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use slop_ninja_protocol::{
    ActiveTarget, ChainBinding, CoordinatorRow, Digest, LaunchAuthorization, Record, RewardIntentAuthorization,
    RewardIntentRenewal, RewardIntentRevocation, RowSource, Signed, TrustedKeys,
};

use crate::package::{MAX_BUNDLE_BYTES, Refusal, protocol};
use crate::package_v2::{Authorized, HeldTargetFile};

pub const AUTHORIZATION_PATH: &str = "v1/authorizations/current.json";
pub const AUTHORIZATION_SCHEMA: &str = "slop_ninja.public.authorization/v2";

/// Bytes one session may read into memory: the validator's bundle file
/// bound, far above a pointer and its signed objects.
pub const SESSION_BYTES: u64 = MAX_BUNDLE_BYTES;

/// The cache's lock file; one session at a time holds it.
const LOCK_FILE: &str = ".lock";

/// The exact encoded length of a signed fixed-size record: every field of
/// the authorization records is fixed-width, so any instance has it.
fn signed_len<R: Record>(record: R) -> Result<u64> {
    let bytes = Signed {
        record,
        public_key: [0; 32],
        signature: [0; 64],
    }
    .to_bytes()
    .map_err(|e| anyhow::anyhow!("signed length: {e}"))?;
    Ok(bytes.len() as u64)
}

/// Schema-exact signed length of a revocation.
pub fn revocation_length(chain: &ChainBinding) -> Result<u64> {
    signed_len(RewardIntentRevocation {
        chain: chain.clone(),
        target: Digest([0; 32]),
        authorization: Digest([0; 32]),
        checkpoint_generation: 0,
        issued_epoch: 0,
    })
}

/// Schema-exact signed lengths of a launch authorization, a reward-intent
/// authorization and a renewal.
pub fn authorization_lengths(chain: &ChainBinding) -> Result<(u64, u64, u64)> {
    let launch = LaunchAuthorization::new(chain, 0, 0)
        .map_err(|e| anyhow::anyhow!("launch authorization: {e}"))?;
    let cohort = RewardIntentAuthorization {
        chain: chain.clone(),
        cohort: 0,
        cohort_manifest: Digest([0; 32]),
        score_package: Digest([0; 32]),
        target: Digest([0; 32]),
        checkpoint_generation: 0,
        issued_epoch: 0,
        renew_by_epoch: 1,
    };
    let renewal = RewardIntentRenewal {
        authorization: Digest([0; 32]),
        checkpoint_generation: 0,
        issued_epoch: 0,
        renew_by_epoch: 1,
    };
    Ok((signed_len(launch)?, signed_len(cohort)?, signed_len(renewal)?))
}

/// The longest signed coordinator row the pointer may name.
pub fn row_bound() -> Result<u64> {
    CoordinatorRow::max_signed_len().map_err(|e| anyhow::anyhow!("row bound: {e}"))
}

// ---------------------------------------------------------------------------
// The pointer. Unknown fields are ignored: the publisher may add some.

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainJson {
    pub genesis: String,
    pub netuid: u16,
    pub mechanism: u8,
}

impl ChainJson {
    fn check(&self, chain: &ChainBinding, what: &str) -> Result<()> {
        let genesis = self.genesis.trim_start_matches("0x");
        ensure!(
            genesis.eq_ignore_ascii_case(&hex::encode(chain.genesis.0))
                && self.netuid == chain.netuid
                && self.mechanism == chain.mechanism,
            "{what} names another chain or subnet"
        );
        Ok(())
    }
}

/// A content-addressed object named by a document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectRef {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

/// `v1/authorizations/current.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizationPointer {
    pub schema: String,
    pub chain: ChainJson,
    /// `launch` or `cohort`.
    pub kind: String,
    pub authorization: ObjectRef,
    #[serde(default)]
    pub renewal: Option<ObjectRef>,
    /// The revocation set: the latest signed revocation of every revoked
    /// target, concatenated, each of [`revocation_length`]; absent when there
    /// is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocations: Option<ObjectRef>,
    /// The coordinator's signed row under this authorization; absent until
    /// the coordinator publishes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<ObjectRef>,
}

/// A label or path segment the layout allows: lowercase ASCII letters,
/// digits, `.`, `_` and `-`, never `.` or `..`, within the protocol's string
/// bound.
pub fn safe_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= slop_ninja_protocol::MAX_STRING_LEN as usize
        && segment != "."
        && segment != ".."
        && segment.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// A relative path of safe segments.
pub fn safe_rel(rel: &str) -> bool {
    !rel.is_empty() && rel.split('/').all(safe_segment)
}

fn hex32(value: &str, what: &str) -> Result<[u8; 32]> {
    let mut out = [0; 32];
    hex::decode_to_slice(value.trim_start_matches("0x"), &mut out)
        .with_context(|| format!("{what} is not 32 bytes of hex"))?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Transport.

/// How a GET was answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Got {
    /// The whole body, from byte 0.
    Full,
    /// The body continues from the requested offset (HTTP 206).
    Partial,
    /// The object does not exist (HTTP 404 or 410).
    NotFound,
}

/// Receives one response body.
pub trait Sink: Send {
    /// Called once before the body, with its declared length and whether it
    /// continues from the requested offset. Refusing here reads nothing.
    fn begin(&mut self, declared: Option<u64>, continues: bool) -> Result<()>;
    fn write(&mut self, chunk: &[u8]) -> Result<()>;
}

/// GETs paths relative to the public host's base URL.
pub trait Transport: Send + Sync {
    /// GET `path`; when `offset > 0`, ask for the bytes from `offset` on.
    fn get<'a, S: Sink>(
        &'a self,
        path: &'a str,
        offset: u64,
        sink: &'a mut S,
    ) -> impl Future<Output = Result<Got>> + Send + 'a;
}

/// HTTPS through reqwest with rustls (ring) and the system trust roots. No
/// redirect is followed, no credential is sent, a request that receives
/// nothing for `stall` is abandoned, and one that is still open after
/// `total` is abandoned however slowly bytes trickle in.
pub struct HttpTransport {
    client: reqwest::Client,
    base: url::Url,
}

impl HttpTransport {
    pub fn new(base: url::Url, stall: Duration, total: Duration, user_agent: &str) -> Result<Self> {
        let mut roots = rustls::RootCertStore::empty();
        let native = rustls_native_certs::load_native_certs();
        let (added, _) = roots.add_parsable_certificates(native.certs);
        ensure!(added > 0, "no usable system TLS trust roots");
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let client = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(stall)
            .read_timeout(stall)
            .timeout(total)
            .user_agent(user_agent)
            .build()?;
        Ok(Self { client, base })
    }
}

/// `bytes <start>-<end>/<total>` of a 206 answer.
fn content_range_start(value: Option<&reqwest::header::HeaderValue>) -> Option<u64> {
    let text = value?.to_str().ok()?;
    let range = text.strip_prefix("bytes ")?;
    range.split('-').next()?.trim().parse().ok()
}

impl Transport for HttpTransport {
    async fn get<'a, S: Sink>(
        &'a self,
        path: &'a str,
        offset: u64,
        sink: &'a mut S,
    ) -> Result<Got> {
        {
            ensure!(safe_rel(path), "unsafe object path");
            let url = self.base.join(path).context("object path")?;
            let mut request = self
                .client
                .get(url)
                .header(reqwest::header::CACHE_CONTROL, "no-cache");
            if offset > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
            }
            // Errors drop the URL; logs name objects, never endpoint paths.
            let mut response = request
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("request failed: {}", e.without_url()))?;
            let status = response.status();
            let got = match status.as_u16() {
                404 | 410 => return Ok(Got::NotFound),
                200 => {
                    sink.begin(response.content_length(), false)?;
                    Got::Full
                }
                206 if offset > 0
                    && content_range_start(
                        response.headers().get(reqwest::header::CONTENT_RANGE),
                    ) == Some(offset) =>
                {
                    sink.begin(response.content_length(), true)?;
                    Got::Partial
                }
                _ if status.is_redirection() => {
                    bail!("answered {status}; redirects are never followed")
                }
                _ => bail!("answered {status}"),
            };
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| anyhow::anyhow!("body read failed: {}", e.without_url()))?
            {
                sink.write(&chunk)?;
            }
            Ok(got)
        }
    }
}

/// A local mirror of the public layout (`fetch.base_url = "file:///<dir>/"`):
/// each path is a file under the directory. It serves an operator's own copy
/// of the public host's objects, for example while the public host is not
/// reachable; whatever fills the directory is outside this crate (none is
/// installed on testnet). Every object is checked exactly
/// as from the HTTPS host (listed digest and length, then signature), so a
/// mirror can make a fetch fail but cannot change what is verified. A
/// missing file is [`Got::NotFound`]; a continued read starts at the
/// requested offset when the file is longer than it, and otherwise from byte
/// 0 (the session then restarts the object, as for a host that ignores a
/// range request).
pub struct DirTransport {
    base: PathBuf,
}

impl DirTransport {
    pub fn new(base: PathBuf) -> Result<Self> {
        ensure!(base.is_absolute(), "the mirror directory must be absolute");
        Ok(Self { base })
    }
}

/// Bytes read from a mirror file per sink write.
const DIR_READ_CHUNK: usize = 64 * 1024;

impl Transport for DirTransport {
    async fn get<'a, S: Sink>(&'a self, path: &'a str, offset: u64, sink: &'a mut S) -> Result<Got> {
        use std::io::{Read as _, Seek as _};
        ensure!(safe_rel(path), "unsafe object path");
        let full = self.base.join(path);
        let mut file = match std::fs::File::open(&full) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Got::NotFound),
            Err(e) => bail!("mirror read failed: {}", e.kind()),
        };
        let len = file.metadata().map_err(|e| anyhow::anyhow!("mirror read failed: {}", e.kind()))?.len();
        let got = if offset > 0 && offset < len {
            file.seek(std::io::SeekFrom::Start(offset)).map_err(|e| anyhow::anyhow!("mirror read failed: {}", e.kind()))?;
            sink.begin(Some(len - offset), true)?;
            Got::Partial
        } else {
            sink.begin(Some(len), false)?;
            Got::Full
        };
        let mut buf = vec![0u8; DIR_READ_CHUNK];
        loop {
            let n = file.read(&mut buf).map_err(|e| anyhow::anyhow!("mirror read failed: {}", e.kind()))?;
            if n == 0 {
                break;
            }
            sink.write(&buf[..n])?;
            // The runtime is single-threaded: let the finalized-head stream
            // and signing run between chunks of a large local file.
            tokio::task::yield_now().await;
        }
        Ok(got)
    }
}

/// The configured public host: HTTPS (or loopback HTTP), or a local mirror.
pub enum PublicTransport {
    Http(HttpTransport),
    Dir(DirTransport),
}

impl PublicTransport {
    /// The transport for `base` (`fetch.base_url`, already checked).
    pub fn new(base: url::Url, stall: Duration, total: Duration, user_agent: &str) -> Result<Self> {
        if base.scheme() == "file" {
            let dir = base.to_file_path().map_err(|()| anyhow::anyhow!("fetch.base_url file:// is not an absolute path"))?;
            return Ok(Self::Dir(DirTransport::new(dir)?));
        }
        Ok(Self::Http(HttpTransport::new(base, stall, total, user_agent)?))
    }
}

impl Transport for PublicTransport {
    async fn get<'a, S: Sink>(&'a self, path: &'a str, offset: u64, sink: &'a mut S) -> Result<Got> {
        match self {
            Self::Http(t) => t.get(path, offset, sink).await,
            Self::Dir(t) => t.get(path, offset, sink).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Bounded reads.

/// The bytes a session may still read.
#[derive(Debug)]
pub struct Budget {
    left: u64,
}

impl Budget {
    pub fn new(bytes: u64) -> Self {
        Self { left: bytes }
    }

    pub fn left(&self) -> u64 {
        self.left
    }

    fn reserve(&mut self, bytes: u64, what: &str) -> Result<()> {
        ensure!(
            bytes <= self.left,
            "{what} needs {bytes} bytes; the session's read bound has {} left",
            self.left
        );
        self.left -= bytes;
        Ok(())
    }
}

/// A document read into memory within a limit.
struct MemorySink {
    buf: Vec<u8>,
    limit: u64,
}

impl Sink for MemorySink {
    fn begin(&mut self, declared: Option<u64>, continues: bool) -> Result<()> {
        ensure!(!continues, "a document was answered as a range");
        if let Some(len) = declared {
            ensure!(
                len <= self.limit,
                "a document declares {len} bytes, over its bound"
            );
        }
        self.buf.clear();
        Ok(())
    }

    fn write(&mut self, chunk: &[u8]) -> Result<()> {
        ensure!(
            self.buf.len() as u64 + chunk.len() as u64 <= self.limit,
            "a document exceeds its bound"
        );
        self.buf.extend_from_slice(chunk);
        Ok(())
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .context("cache directory")
}

/// Counts for the report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub documents: u32,
    pub fetched: u32,
    pub bytes_read: u64,
}

struct Reader<'a, T: Transport> {
    transport: &'a T,
    budget: Budget,
    stats: Stats,
    /// Every document's path and bytes, in reading order.
    documents: Sha256,
}

/// An object read into memory.
enum InMemory {
    Bytes(Vec<u8>),
    Missing,
    Mismatch,
}

impl<'a, T: Transport> Reader<'a, T> {
    fn new(transport: &'a T, bytes: u64) -> Self {
        Self {
            transport,
            budget: Budget::new(bytes),
            stats: Stats::default(),
            documents: Sha256::new(),
        }
    }

    /// The documents read so far and the held target, as one digest.
    fn fingerprint(&self, held: &ActiveTarget) -> String {
        let mut hasher = self.documents.clone();
        hasher.update(held.digest().0);
        hasher.update(held.activated_epoch().unwrap_or(u64::MAX).to_be_bytes());
        hex::encode(hasher.finalize())
    }

    /// A document, or `None` when the host does not have it.
    async fn document<D: serde::de::DeserializeOwned>(&mut self, path: &str) -> Result<Option<D>> {
        let mut sink = MemorySink {
            buf: Vec::new(),
            limit: self.budget.left(),
        };
        let got = self
            .transport
            .get(path, 0, &mut sink)
            .await
            .with_context(|| format!("GET {path}"))?;
        if got == Got::NotFound {
            return Ok(None);
        }
        self.budget.reserve(sink.buf.len() as u64, path)?;
        self.stats.documents += 1;
        self.stats.bytes_read += sink.buf.len() as u64;
        self.documents.update((path.len() as u64).to_be_bytes());
        self.documents.update(path.as_bytes());
        self.documents.update((sink.buf.len() as u64).to_be_bytes());
        self.documents.update(&sink.buf);
        Ok(Some(
            serde_json::from_slice(&sink.buf).with_context(|| format!("{path} is malformed"))?,
        ))
    }

    /// A small object read into memory only, checked against its listed
    /// SHA-256 and length; never cached.
    async fn memory_object(&mut self, remote: &str, sha256: &str, bytes: u64) -> Result<InMemory> {
        self.budget.reserve(bytes, remote)?;
        let mut sink = MemorySink {
            buf: Vec::new(),
            limit: bytes,
        };
        let got = self
            .transport
            .get(remote, 0, &mut sink)
            .await
            .with_context(|| format!("GET {remote}"))?;
        if got == Got::NotFound {
            return Ok(InMemory::Missing);
        }
        self.stats.fetched += 1;
        self.stats.bytes_read += sink.buf.len() as u64;
        if sink.buf.len() as u64 != bytes || sha256_hex(&sink.buf) != sha256.to_ascii_lowercase() {
            return Ok(InMemory::Mismatch);
        }
        Ok(InMemory::Bytes(sink.buf))
    }
}

// ---------------------------------------------------------------------------
// Sessions.

/// What a session produced.
#[derive(Debug)]
pub enum Outcome {
    /// The host names no authorization yet.
    Unpublished,
    /// The pointer and the held target are byte-identical to those of the
    /// last session that produced a bundle: that bundle is still current.
    Unchanged,
    /// A complete bundle, verified against the held target.
    Ready(Box<Staged>),
    /// The records are not all published yet (the coordinator's row among
    /// them), or an object failed its digest; the installed bundle stays and
    /// a later session continues.
    Incomplete {
        cohort: Option<String>,
        code: &'static str,
        detail: String,
    },
    /// The records verified as published but the protocol refused them.
    Refused(Refusal),
}

impl Outcome {
    /// A content-free code for logs and reports.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unpublished => "unpublished",
            Self::Unchanged => "unchanged",
            Self::Ready(_) => "ready",
            Self::Incomplete { code, .. } => code,
            Self::Refused(refusal) => refusal.code,
        }
    }

    /// Whether the host served records that failed a check, as opposed to
    /// records that are only not complete yet: a refusal by the protocol or
    /// an object that failed its listed digest. These are the
    /// validator-side form of a published discrepancy and are logged with
    /// `alert = true`.
    pub fn alert(&self) -> bool {
        match self {
            Self::Refused(_) => true,
            Self::Incomplete { code, .. } => *code == "object_digest_mismatch",
            _ => false,
        }
    }

    /// Whether a later session may complete what this one could not
    /// (`fetch` exits 75 for these).
    pub fn retryable(&self) -> bool {
        match self {
            Self::Unpublished => true,
            Self::Incomplete { .. } => !self.alert(),
            _ => false,
        }
    }
}

/// A verified bundle ready to install.
#[derive(Debug)]
pub struct Staged {
    pub bundle: Vec<u8>,
    pub sha256: String,
    pub info: BundleInfo,
    /// Always `None` for a row bundle: the row names its target directly,
    /// so no intermediate target is checked on the way to it.
    pub advance: Option<HeldTargetFile>,
    pub stats: Stats,
    /// The verification itself, so a refresh can swap it in place.
    pub authorized: Arc<Authorized>,
    /// The pointer and held target this bundle was assembled from
    /// ([`Outcome::Unchanged`] compares the next session's with it).
    pub fingerprint: String,
    /// Cohorts the cache keeps for this bundle: none for a row bundle.
    pub cohorts: Vec<String>,
    /// Keeps [`Self::cohorts`] in the session's [`KeepSet`] while it lives.
    pub kept: KeepGuard,
}

/// The cohorts the fetch cache keeps besides those a session names: the
/// cohorts of the bundle files (the installed bundle, one staged on disk for
/// an interrupted install, a dry run's staged in memory; `None` while one of
/// them cannot be read), and those of every [`Staged`] bundle the process
/// still holds. A session reads it when it prunes, not when it starts.
///
/// A cohort enters it only from the session that names it (a [`Staged`]
/// bundle registers before the session prunes) or while a [`KeepGuard`]
/// already keeps it (an install writes its bundle file while it holds the
/// [`Staged`] value), so a snapshot taken at prune time never misses a
/// cohort a held or installed bundle needs.
#[derive(Clone, Debug)]
pub struct KeepSet(Arc<std::sync::Mutex<KeepState>>);

#[derive(Debug)]
struct KeepState {
    files: Option<Vec<String>>,
    held: BTreeMap<u64, Vec<String>>,
    next: u64,
}

impl KeepSet {
    /// A keep set with the bundle files' cohorts (`None`: unknown).
    pub fn new(files: Option<Vec<String>>) -> Self {
        Self(Arc::new(std::sync::Mutex::new(KeepState { files, held: BTreeMap::new(), next: 0 })))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, KeepState> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records the bundle files' cohorts (`None`: a file cannot be read).
    pub fn set_files(&self, files: Option<Vec<String>>) {
        self.state().files = files;
    }

    /// Whether the bundle files' cohorts are known.
    pub fn files_known(&self) -> bool {
        self.state().files.is_some()
    }

    /// Keeps `cohorts` until the guard is dropped.
    pub fn hold(&self, cohorts: &[String]) -> KeepGuard {
        let mut state = self.state();
        let id = state.next;
        state.next += 1;
        state.held.insert(id, cohorts.to_vec());
        KeepGuard { set: self.clone(), id }
    }

    /// Every cohort kept now, or `None` while the bundle files' are unknown.
    pub fn snapshot(&self) -> Option<Vec<String>> {
        let state = self.state();
        let mut all = state.files.clone()?;
        for c in state.held.values().flatten() {
            if !all.contains(c) {
                all.push(c.clone());
            }
        }
        Some(all)
    }
}

/// Keeps a [`Staged`] bundle's cohorts in its [`KeepSet`]; released on drop.
#[derive(Debug)]
pub struct KeepGuard {
    set: KeepSet,
    id: u64,
}

impl Drop for KeepGuard {
    fn drop(&mut self) {
        self.set.state().held.remove(&self.id);
    }
}

/// What a verified bundle holds. Content-free.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BundleInfo {
    /// `launch` or `cohort`.
    pub kind: String,
    pub target: String,
    pub activated_epoch: Option<u64>,
    /// Digest of the authorization record.
    pub authorization: String,
    /// First epoch at which the authorization yields a commitment.
    pub issued_epoch: u64,
    /// Its non-binding renew-by hint (or a refresh's). It never pauses
    /// anything.
    pub renew_by_epoch: u64,
    /// The cohort that carries the authorization, when it is a cohort's.
    pub cohort: Option<String>,
    /// The epoch of the coordinator commitment the row was signed for.
    pub row_epoch: u64,
    /// The finalized block the coordinator's row was derived at: under one
    /// authorization, rows are ordered by `(row_epoch, row_block)`.
    pub row_block: u64,
    /// Entries of the coordinator's row.
    pub row_entries: usize,
}

impl BundleInfo {
    /// Read from a bundle that [`crate::package_v2::verify`] accepted.
    pub fn of(_bytes: &[u8], authorized: &Authorized) -> Result<Self> {
        let v = &authorized.verified;
        let terms = v.terms();
        Ok(Self {
            kind: if v.is_launch() { "launch" } else { "cohort" }.into(),
            target: v.target_digest().to_string(),
            activated_epoch: v.active.activated_epoch(),
            authorization: terms.digest.to_string(),
            issued_epoch: terms.issued_epoch,
            renew_by_epoch: terms.renew_by_epoch,
            cohort: match &v.row.source {
                RowSource::Cohort { cohort_label, .. } => Some(cohort_label.clone()),
                RowSource::LaunchBurn => None,
            },
            row_epoch: v.row.epoch,
            row_block: v.row.registration_block,
            row_entries: v.row.entries.len(),
        })
    }
}

/// What a session reads and checks against.
pub struct Session<'a, T: Transport> {
    pub transport: &'a T,
    pub cache: &'a Path,
    pub keys: &'a TrustedKeys,
    pub chain: &'a ChainBinding,
    /// The target this validator's own durable record holds active.
    pub held: ActiveTarget,
    /// [`Staged::fingerprint`] of the last bundle a session produced.
    pub previous: Option<&'a str>,
    /// What the cache keeps besides what a session names (nothing, for row
    /// bundles); read when the session prunes.
    pub keep: &'a KeepSet,
    /// Where each revocation of the pointer's set is journaled
    /// ([`crate::revocations::record`]); `None` checks them without keeping
    /// them.
    pub revocations: Option<&'a Path>,
}

/// Holds the cache for one session; released on drop.
struct CacheLock(std::fs::File);

impl Drop for CacheLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn lock_cache(cache: &Path) -> Result<CacheLock> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(cache.join(LOCK_FILE))
        .context("cache lock file")?;
    match file.try_lock() {
        Ok(()) => Ok(CacheLock(file)),
        Err(std::fs::TryLockError::WouldBlock) => {
            bail!("cache_busy: another fetch session holds the cache")
        }
        Err(std::fs::TryLockError::Error(error)) => Err(error).context("cache lock"),
    }
}

fn incomplete(cohort: Option<&str>, code: &'static str, detail: impl Into<String>) -> Outcome {
    Outcome::Incomplete {
        cohort: cohort.map(str::to_owned),
        code,
        detail: detail.into(),
    }
}

/// Stops a session with an outcome, or continues with a value.
enum Step<T> {
    Go(T),
    Stop(Outcome),
}

macro_rules! step {
    ($e:expr) => {
        match $e {
            Step::Go(v) => v,
            Step::Stop(outcome) => return Ok(outcome),
        }
    };
}

impl<T: Transport> Session<'_, T> {
    /// Fetches what the current authorization needs and assembles its
    /// bundle. Transport and disk errors are errors (retry at the next
    /// session); everything the host or the protocol decides is an
    /// [`Outcome`]. Holds the cache lock for the whole session
    /// (`cache_busy` when another holds it). Removes nothing: an earlier
    /// layout's cache stays for a rollback (module docs).
    pub async fn run(&self) -> Result<Outcome> {
        private_dir(self.cache)?;
        let _lock = lock_cache(self.cache)?;
        let mut reader = Reader::new(self.transport, SESSION_BYTES);
        self.run_locked(&mut reader).await
    }

    async fn run_locked(&self, reader: &mut Reader<'_, T>) -> Result<Outcome> {
        let Some(pointer) = reader
            .document::<AuthorizationPointer>(AUTHORIZATION_PATH)
            .await?
        else {
            return Ok(Outcome::Unpublished);
        };
        ensure!(
            pointer.schema == AUTHORIZATION_SCHEMA,
            "unknown authorization pointer schema"
        );
        pointer
            .chain
            .check(self.chain, "the authorization pointer")?;
        if let Some(r) = &pointer.revocations {
            let set = step!(self.revocation_set(reader, r).await?);
            let len = revocation_length(self.chain)? as usize;
            // One locked read of the store per session; only entries it
            // does not hold yet are verified and journaled, in one write.
            let recorded = crate::revocations::record_set(self.revocations, self.keys, self.chain, &set, len)?;
            drop(set);
            if self.revocations.is_some() {
                for entry in &recorded.new {
                    tracing::warn!(
                        event = "revocation_recorded",
                        source = "fetch",
                        revocation = %entry.digest,
                        target = %entry.target,
                        alert = true
                    );
                }
            }
            if recorded.skipped > 0 {
                tracing::warn!(
                    event = "revocation_skipped",
                    skipped = recorded.skipped,
                    reason = "does not verify under the pinned keys"
                );
            }
        }
        if self.previous == Some(reader.fingerprint(&self.held).as_str()) {
            return Ok(Outcome::Unchanged);
        }
        let (launch_len, cohort_len, renewal_len) = authorization_lengths(self.chain)?;
        let authorization = match pointer.kind.as_str() {
            "launch" => step!(
                self.signed_object::<LaunchAuthorization>(reader, &pointer.authorization, launch_len, launch_len)
                    .await?
            ),
            "cohort" => step!(
                self.signed_object::<RewardIntentAuthorization>(reader, &pointer.authorization, cohort_len, cohort_len)
                    .await?
            ),
            other => bail!("unknown authorization kind {other:?}"),
        };
        let renewal = match &pointer.renewal {
            Some(r) => Some(step!(
                self.signed_object::<RewardIntentRenewal>(reader, r, renewal_len, renewal_len)
                    .await?
            )),
            None => None,
        };
        let Some(row_ref) = &pointer.row else {
            return Ok(incomplete(
                None,
                "row_unpublished",
                "the coordinator has not published its signed row under this authorization yet",
            ));
        };
        let row = step!(self.signed_object::<CoordinatorRow>(reader, row_ref, 1, row_bound()?).await?);
        let fingerprint = reader.fingerprint(&self.held);
        let bundle = crate::package_v2::row_bundle(&pointer.kind, &authorization, renewal.as_deref(), &row);
        let bytes = serde_json::to_vec(&bundle)?;
        let authorized = match crate::package_v2::verify(&bytes, self.keys, &self.chain.genesis.0, self.chain.netuid, &self.held) {
            Ok(a) => a,
            Err(refusal) => return Ok(Outcome::Refused(refusal)),
        };
        let info = BundleInfo::of(&bytes, &authorized)?;
        Ok(Outcome::Ready(Box::new(Staged {
            sha256: sha256_hex(&bytes),
            bundle: bytes,
            info,
            advance: None,
            stats: reader.stats.clone(),
            authorized: Arc::new(authorized),
            fingerprint,
            kept: self.keep.hold(&[]),
            cohorts: Vec::new(),
        })))
    }

    /// The revocation set the pointer names, read into memory once its
    /// listed length is a non-zero whole multiple of the revocation's signed
    /// length (counted against the session bound like any object).
    async fn revocation_set(&self, reader: &mut Reader<'_, T>, r: &ObjectRef) -> Result<Step<Vec<u8>>> {
        ensure!(safe_rel(&r.path), "unsafe authorization path");
        let sha = hex::encode(hex32(&r.sha256, "revocation set sha256")?);
        let len = revocation_length(self.chain)?;
        if r.bytes == 0 || !r.bytes.is_multiple_of(len) {
            return Ok(Step::Stop(Outcome::Refused(Refusal::new(
                "authorization_malformed",
                "the revocation set's listed length is not whole revocations",
            ))));
        }
        match reader.memory_object(&r.path, &sha, r.bytes).await? {
            InMemory::Bytes(bytes) => Ok(Step::Go(bytes)),
            InMemory::Missing => Ok(Step::Stop(incomplete(
                None,
                "authorization_missing",
                "the named revocation set is not published",
            ))),
            InMemory::Mismatch => Ok(Step::Stop(incomplete(
                None,
                "object_digest_mismatch",
                "the revocation set failed its digest",
            ))),
        }
    }

    /// A content-addressed signed object (an authorization, a renewal or
    /// the coordinator's row): its listed length must be within
    /// `[min, max]` (an authorization's or renewal's exact signed length),
    /// and it is read into memory and its signature checked before anything
    /// else uses it. It is never cached.
    async fn signed_object<R: Record>(
        &self,
        reader: &mut Reader<'_, T>,
        r: &ObjectRef,
        min: u64,
        max: u64,
    ) -> Result<Step<Vec<u8>>> {
        ensure!(safe_rel(&r.path), "unsafe authorization path");
        let sha = hex::encode(hex32(&r.sha256, "authorization sha256")?);
        if r.bytes < min || r.bytes > max {
            return Ok(Step::Stop(Outcome::Refused(Refusal::new(
                "authorization_malformed",
                "the listed length is outside the record's signed length",
            ))));
        }
        let bytes = match reader.memory_object(&r.path, &sha, r.bytes).await? {
            InMemory::Bytes(bytes) => bytes,
            InMemory::Missing => {
                return Ok(Step::Stop(incomplete(
                    None,
                    "authorization_missing",
                    "a named authorization object is not published",
                )));
            }
            InMemory::Mismatch => {
                return Ok(Step::Stop(incomplete(
                    None,
                    "object_digest_mismatch",
                    "an authorization object failed its digest",
                )));
            }
        };
        if let Err(e) = Signed::<R>::open(&bytes, self.keys) {
            return Ok(Step::Stop(Outcome::Refused(protocol(
                "authorization_unverified",
            )(e))));
        }
        Ok(Step::Go(bytes))
    }
}

/// Labels of the cohorts an installed bundle keeps in the cache: none, for
/// a coordinator-row bundle (it names no cohort record). Kept for the
/// caller's keep set.
pub fn bundle_cohorts(_bytes: &[u8]) -> Result<Vec<String>> {
    Ok(Vec::new())
}
