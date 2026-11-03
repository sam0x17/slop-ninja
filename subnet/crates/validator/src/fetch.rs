//! The published-package fetcher: reads the coordinator's signed records from
//! the public status host and assembles the `reward-v1` bundle that
//! [`crate::package_v2::verify`] checks.
//!
//! The layout is the publisher's `v1` (`docs/testnet/public-layout.md`):
//!
//! - `v1/authorizations/current.json` (schema v2) names the authorization
//!   that lets validators commit now (launch or cohort), any renew-by hint
//!   refresh of it, and the revocation set, if any: the latest signed
//!   revocation of every revoked target, concatenated, each a
//!   content-addressed object with its SHA-256 and length. Every entry of
//!   the set is checked and journaled ([`crate::revocations`]) before
//!   anything else is fetched, so it takes effect whatever the rest of the
//!   session does, and a validator that missed earlier revocations (offline,
//!   or holding an older target) still journals each of them. An entry that
//!   does not verify under the pinned keys is skipped, never journaled. A
//!   validator built for schema v1 refuses a v2 pointer by its schema and
//!   cannot see revocations; it must be upgraded;
//! - `v1/cohorts/index.json` lists every published cohort with its target
//!   digest and whether its detailed receipts are still published;
//! - each cohort's `publication.json` lists every object copied, with its
//!   SHA-256 and length; `chunk-map.json` does the same for receipt bundles.
//!
//! The documents are unsigned and serve only to locate objects. Every object
//! is checked against its listed SHA-256 and length before it is used, and
//! every signed record is then authenticated by the protocol crate under the
//! pinned trust roots. A lying document can make a fetch fail; it cannot
//! change what is verified, and it cannot grow the cache or the process past
//! the session bound:
//!
//! - authorization, renewal and revocation objects are fixed-size records: their listed
//!   length must be the schema's exact signed length (for the revocation
//!   set, a whole multiple of it), they are read into memory and their
//!   signature is checked before anything else is fetched; they are never
//!   cached;
//! - every receipt bundle the chunk map names must also be listed in
//!   `publication.json`, the chunk map's receipt root and count must be the
//!   signed receipt index's, and the bundles' total length must be exactly
//!   what the signed index's entries declare (`u32` length prefix plus each
//!   receipt object's signed length), all checked before a bundle is read;
//! - every object read into memory counts against the session bound once,
//!   whether it is read from the host or from the cache; receipt bundles a
//!   v3 bundle names by reference are only streamed to the cache, and the
//!   signed index bounds their total before any is read;
//! - the session ends by pruning the cache to the cohorts it named and the
//!   ones the installed bundle was built from, whatever its outcome.
//!
//! A fetch session is bounded: it reads at most [`SESSION_BYTES`] into
//! memory (the validator's bundle file bound, [`MAX_BUNDLE_BYTES`]). The
//! bundle embeds every object hex-encoded (bundle schema v2) when all of
//! them fit the file bound; otherwise it embeds the records and names the
//! receipt indexes and receipt bundles by their absolute paths in the cache
//! (bundle schema v3), and a cohort whose listed records alone could not fit
//! is refused before any object is read. It refuses a declared length before reading the body,
//! abandons a request that receives nothing for the configured stall time,
//! and the caller bounds the whole session in time. It is resumable: verified
//! objects are kept in the cache, and a partial object is kept as
//! `<name>.part` and continued with an HTTP range request at the next session.
//! A digest mismatch deletes the partial file. One session at a time holds the
//! cache (`<cache>/.lock`); another process finds it busy and fails fast.
//!
//! A session whose documents (pointer, index and publication records) and
//! held target are byte-identical to those of the last session that produced
//! a bundle reports [`Outcome::Unchanged`] without reading, hashing or
//! verifying any object again.
//!
//! Receipt retention (the N+2 rule): a cohort's receipt index, chunk map and
//! bundles are deleted from the host once two later cohorts are published and
//! its target is superseded. A carrying cohort produces no target, so its
//! receipts are deleted two cohorts later even while the current
//! authorization still names it. A validator that fetched them keeps them in
//! its cache while its installed bundle needs them; one that was offline
//! longer, or starts fresh, may find them gone. New commitments need retained
//! receipts, so the session then reports [`Outcome::ReceiptsExpired`] and the
//! installed bundle stays; commitments already submitted recover from the
//! journal without them. Intermediate targets on the way from the held
//! target to the current one ([`advance`]) are checked without receipts,
//! because no commitment is made for them.

use std::collections::BTreeSet;
use std::future::Future;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use slop_ninja_protocol::submission::CohortClosure;
use slop_ninja_protocol::{
    ActiveTarget, CalculationTargetV2, ChainBinding, CohortManifestV2, Digest, FingerprintIndex,
    LaunchAuthorization, PublishedContents, PublishedIndex, ReceiptIndex, Record,
    RewardIntentAuthorization, RewardIntentRenewal, RewardIntentRevocation, ScorePackageV2, Signed,
    TrustedKeys,
};

use crate::package::{MAX_BUNDLE_BYTES, Refusal, protocol};
use crate::package_v2::{
    AuthorizationHex, Authorized, BUNDLE_SCHEMA_V2, BUNDLE_SCHEMA_V3, BundleV2, CarrierHex, HeldTargetFile,
    ObjectRef as BundleObject, ReceiptsRef, SourceHex,
};

pub const AUTHORIZATION_PATH: &str = "v1/authorizations/current.json";
pub const INDEX_PATH: &str = "v1/cohorts/index.json";
pub const AUTHORIZATION_SCHEMA: &str = "slop_ninja.public.authorization/v2";
pub const INDEX_SCHEMA: &str = "slop_ninja.public.cohort_index/v1";
pub const PUBLICATION_SCHEMA: &str = "slop_ninja.public.publication/v1";
pub const CHUNK_FORMAT: &str = "slop_ninja_receipt_chunk_v1";

/// Bytes one session may read into memory, documents and objects together,
/// whether from the host or from the cache. The assembled bundle holds the
/// objects it embeds hex-encoded (twice their size) within
/// [`MAX_BUNDLE_BYTES`], so they total at most half of this; the documents
/// that locate them and the signed receipt indexes of a v3 bundle fit in the
/// rest. Receipt bundles a v3 bundle names are streamed to the cache, not
/// read into memory, and are not counted.
pub const SESSION_BYTES: u64 = MAX_BUNDLE_BYTES;

/// Each receipt in a bundle object is `u32_le(len) || AttemptReceipt`.
const RECEIPT_LEN_PREFIX: u64 = 4;
/// The cache's lock file; one session at a time holds it.
const LOCK_FILE: &str = ".lock";
/// Cache directory of an earlier layout that cached authorization objects.
const LEGACY_AUTHORIZATIONS: &str = "authorizations";

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

/// The objects of a cohort target's records, by their published names.
const TARGET_OBJECTS: [&str; 7] = [
    "manifest.signed",
    "package.signed",
    "target.signed",
    "closure.signed",
    "receipt-index.signed",
    "published-contents.bin",
    "fingerprint-index.bin",
];
/// A carrying cohort's records (it produced no target and has no closure in
/// the bundle).
const CARRIER_OBJECTS: [&str; 5] = [
    "manifest.signed",
    "package.signed",
    "receipt-index.signed",
    "published-contents.bin",
    "fingerprint-index.bin",
];
/// An intermediate target's records, checked without receipts.
const STEP_OBJECTS: [&str; 6] = [
    "manifest.signed",
    "package.signed",
    "target.signed",
    "closure.signed",
    "published-contents.bin",
    "fingerprint-index.bin",
];
const CHUNK_MAP: &str = "chunk-map.json";
const RECEIPT_INDEX: &str = "receipt-index.signed";

// ---------------------------------------------------------------------------
// Layout documents. Unknown fields are ignored: the publisher may add some.

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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub cohort: u64,
    pub label: String,
    pub path: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default = "default_true")]
    pub receipts_retained: bool,
}

fn default_true() -> bool {
    true
}

/// `v1/cohorts/index.json`, cohorts ascending.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortIndex {
    pub schema: String,
    pub chain: ChainJson,
    pub cohorts: Vec<IndexEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedObject {
    /// Path relative to the cohort directory.
    pub rel: String,
    pub sha256: String,
    pub bytes: u64,
}

/// `v1/cohorts/<label>/publication.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publication {
    pub schema: String,
    pub cohort: u64,
    pub label: String,
    pub objects: Vec<PublishedObject>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkObject {
    /// The coordinator's private key, `cohorts/<label>/<rel>`.
    pub key: String,
    pub sha256: String,
    pub bytes: u64,
}

/// `chunk-map.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkMap {
    pub format: String,
    pub receipt_root: String,
    pub receipt_count: u64,
    pub objects: Vec<ChunkObject>,
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

/// An object streamed into its `.part` file, never past its listed length.
struct FileSink {
    file: std::fs::File,
    written: u64,
    expected: u64,
}

impl Sink for FileSink {
    fn begin(&mut self, declared: Option<u64>, continues: bool) -> Result<()> {
        if !continues {
            // The host ignored the range: start over.
            self.file.set_len(0)?;
            self.written = 0;
        }
        if let Some(len) = declared {
            ensure!(
                self.written.saturating_add(len) <= self.expected,
                "the host declares more bytes than the object's listed length"
            );
        }
        Ok(())
    }

    fn write(&mut self, chunk: &[u8]) -> Result<()> {
        ensure!(
            self.written + chunk.len() as u64 <= self.expected,
            "the object exceeds its listed length"
        );
        self.file.write_all(chunk)?;
        self.written += chunk.len() as u64;
        Ok(())
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn file_sha256(path: &Path) -> Result<(u64, String)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let len = std::io::copy(&mut file, &mut hasher)?;
    Ok((len, hex::encode(hasher.finalize())))
}

fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .context("cache directory")
}

/// How an object was obtained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Obtained {
    /// Already verified in the cache.
    Cached,
    /// Downloaded from byte 0.
    Fetched,
    /// Continued from a partial file.
    Resumed,
    /// The host does not have it.
    Missing,
    /// The downloaded bytes did not match the listed digest; the partial
    /// file was deleted.
    Mismatch,
}

/// Counts for the report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub documents: u32,
    pub cached: u32,
    pub fetched: u32,
    pub resumed: u32,
    pub bytes_read: u64,
}

struct Reader<'a, T: Transport> {
    transport: &'a T,
    budget: Budget,
    stats: Stats,
    /// Objects already counted against the budget (read twice, counted once).
    reserved: BTreeSet<PathBuf>,
    /// Every document's path and bytes, in reading order.
    documents: Sha256,
    /// Cohorts this session located, so the cache keeps them.
    named: Vec<String>,
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
            reserved: BTreeSet::new(),
            documents: Sha256::new(),
            named: Vec::new(),
        }
    }

    /// Counts an object read into memory against the session bound, once.
    fn reserve_once(&mut self, local: &Path, bytes: u64, what: &str) -> Result<()> {
        if self.reserved.insert(local.to_path_buf()) {
            self.budget.reserve(bytes, what)?;
        }
        Ok(())
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

    /// An object at `remote`, kept at `local` once its SHA-256 and length
    /// match. A partial file continues with a range request. The object
    /// counts against the session bound whether it is cached or fetched,
    /// because the caller reads it into memory.
    async fn object(
        &mut self,
        remote: &str,
        local: &Path,
        sha256: &str,
        bytes: u64,
    ) -> Result<Obtained> {
        // Reserve the whole object: it is read into memory, and a host may
        // ignore the range.
        self.reserve_once(local, bytes, remote)?;
        self.object_to_disk(remote, local, sha256, bytes).await
    }

    /// [`object`](Self::object) for an object that is only streamed to its
    /// cache file and never read into memory by this session (a receipt
    /// chunk a v3 bundle names by reference): it does not count against the
    /// session bound. The signed receipt index bounds what the chunks total
    /// before any is read.
    async fn object_to_disk(
        &mut self,
        remote: &str,
        local: &Path,
        sha256: &str,
        bytes: u64,
    ) -> Result<Obtained> {
        if local.is_file() {
            if file_sha256(local)? == (bytes, sha256.to_ascii_lowercase()) {
                self.stats.cached += 1;
                return Ok(Obtained::Cached);
            }
            std::fs::remove_file(local)?;
        }
        if let Some(parent) = local.parent() {
            private_dir(parent)?;
        }
        let mut part_name = local
            .file_name()
            .context("object file name")?
            .to_os_string();
        part_name.push(".part");
        let part = local.with_file_name(part_name);
        let mut offset = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if offset > bytes {
            std::fs::remove_file(&part)?;
            offset = 0;
        }
        let file = {
            use std::os::unix::fs::OpenOptionsExt as _;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&part)
                .context("partial object file")?
        };
        let mut sink = FileSink {
            file,
            written: offset,
            expected: bytes,
        };
        let got = if offset == bytes {
            Got::Partial
        } else {
            let result = self.transport.get(remote, offset, &mut sink).await;
            // Count what arrived, even when the request failed part-way.
            self.stats.bytes_read += sink.written.saturating_sub(offset);
            result.with_context(|| format!("GET {remote}"))?
        };
        sink.file.sync_all()?;
        drop(sink);
        if got == Got::NotFound {
            return Ok(Obtained::Missing);
        }
        if file_sha256(&part)? != (bytes, sha256.to_ascii_lowercase()) {
            std::fs::remove_file(&part)?;
            return Ok(Obtained::Mismatch);
        }
        std::fs::rename(&part, local)?;
        if let Some(parent) = local.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(if offset > 0 && got == Got::Partial {
            self.stats.resumed += 1;
            Obtained::Resumed
        } else {
            self.stats.fetched += 1;
            Obtained::Fetched
        })
    }
}

// ---------------------------------------------------------------------------
// Sessions.

/// What a session produced.
#[derive(Debug)]
pub enum Outcome {
    /// The host names no authorization yet.
    Unpublished,
    /// The documents and the held target are byte-identical to those of the
    /// last session that produced a bundle: that bundle is still current.
    Unchanged,
    /// A complete bundle, verified against the held target (after any
    /// intermediate advance).
    Ready(Box<Staged>),
    /// The records are not all published yet, or an object failed its
    /// digest; the installed bundle stays and a later session continues.
    Incomplete {
        cohort: Option<String>,
        code: &'static str,
        detail: String,
    },
    /// Receipts a new commitment needs were deleted under the N+2 rule and
    /// are not in the cache.
    ReceiptsExpired { cohort: String },
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
            Self::ReceiptsExpired { .. } => "receipts_expired",
            Self::Refused(refusal) => refusal.code,
        }
    }

    /// Whether the host served records that failed a check, as opposed to
    /// records that are only not complete yet: a refusal by the protocol, an
    /// object that failed its listed digest, or documents that disagree with
    /// the signed records. These are the validator-side form of a published
    /// discrepancy and are logged with `alert = true`.
    pub fn alert(&self) -> bool {
        match self {
            Self::Refused(_) => true,
            Self::Incomplete { code, .. } => {
                matches!(*code, "object_digest_mismatch" | "chunk_map_mismatch")
            }
            _ => false,
        }
    }

    /// Whether a later session may complete what this one could not
    /// (`fetch` exits 75 for these).
    pub fn retryable(&self) -> bool {
        match self {
            Self::Unpublished | Self::ReceiptsExpired { .. } => true,
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
    /// Intermediate targets were checked on the way from the held target:
    /// the held-target record to write before the bundle is installed.
    pub advance: Option<HeldTargetFile>,
    pub stats: Stats,
    /// The verification itself, so a refresh can swap it in place.
    pub authorized: Arc<Authorized>,
    /// The documents and held target this bundle was assembled from
    /// ([`Outcome::Unchanged`] compares the next session's with it).
    pub fingerprint: String,
    /// Labels of the cohorts the bundle was built from (target and carrier),
    /// which the cache keeps while it is installed.
    pub cohorts: Vec<String>,
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
    pub cohort: Option<String>,
    pub carrier: Option<u64>,
    pub receipts_verified: u64,
}

impl BundleInfo {
    /// Read from a bundle that [`crate::package_v2::verify`] accepted.
    pub fn of(bytes: &[u8], authorized: &Authorized) -> Result<Self> {
        let b: BundleV2 = serde_json::from_slice(bytes)?;
        let terms = authorized
            .verified
            .checked_terms(&authorized.keys)
            .map_err(|e| anyhow::anyhow!("authorization: {e}"))?;
        Ok(Self {
            kind: b.authorization.kind.clone(),
            target: authorized.verified.target_digest.to_string(),
            activated_epoch: authorized.verified.active.activated_epoch(),
            authorization: terms.digest.to_string(),
            issued_epoch: terms.issued_epoch,
            renew_by_epoch: terms.renew_by_epoch,
            cohort: authorized
                .verified
                .source
                .as_ref()
                .map(|s| s.package.cohort_label.clone()),
            carrier: authorized.verified.carrier().map(|c| c.package.cohort),
            receipts_verified: authorized.receipts_verified,
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
    /// Cohorts of the installed bundle, which pruning keeps.
    pub keep: &'a [String],
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

/// One cohort's located objects.
struct CohortObjects<'e> {
    entry: &'e IndexEntry,
    publication: Publication,
}

impl CohortObjects<'_> {
    fn dir(&self) -> String {
        let path = self.entry.path.trim_end_matches('/');
        format!("{path}/")
    }

    fn listed(&self, rel: &str) -> Option<&PublishedObject> {
        self.publication.objects.iter().find(|o| o.rel == rel)
    }

    fn local(&self, cache: &Path, rel: &str) -> PathBuf {
        cache.join("cohorts").join(&self.entry.label).join(rel)
    }
}

/// The bytes of a cohort's cached receipt index and receipt bundles (0 for
/// what the cache does not hold).
fn cached_receipt_bytes(cohort_dir: &Path) -> u64 {
    let len = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len());
    let bundles = std::fs::read_dir(cohort_dir.join("receipts"))
        .map(|d| d.filter_map(|e| e.ok()).map(|e| len(&e.path())).fold(0u64, u64::saturating_add))
        .unwrap_or(0);
    len(&cohort_dir.join(RECEIPT_INDEX)).saturating_add(bundles)
}

/// One cohort's receipts as a bundle holds them: embedded (schema v2: the
/// signed index and the chunk objects), or by reference (schema v3).
enum Receipts {
    Embedded(Vec<u8>, Vec<Vec<u8>>),
    Referenced(ReceiptsRef),
}

impl Receipts {
    fn embedded_index(&self) -> Option<String> {
        match self {
            Self::Embedded(index, _) => Some(hex::encode(index)),
            Self::Referenced(_) => None,
        }
    }

    fn embedded_chunks(&self) -> Vec<String> {
        match self {
            Self::Embedded(_, chunks) => chunks.iter().map(hex::encode).collect(),
            Self::Referenced(_) => Vec::new(),
        }
    }

    fn referenced(self) -> Option<ReceiptsRef> {
        match self {
            Self::Embedded(..) => None,
            Self::Referenced(r) => Some(r),
        }
    }
}

/// A run's outcome while reading objects: continue, or stop with an outcome.
enum Step<T> {
    Go(T),
    Stop(Outcome),
}

/// Unwraps a [`Step`] in a function returning `Result<Outcome>`.
macro_rules! step {
    ($e:expr) => {
        match $e {
            Step::Go(value) => value,
            Step::Stop(outcome) => return Ok(outcome),
        }
    };
}

/// Unwraps a [`Step`] in a function returning `Result<Step<_>>`.
macro_rules! step_go {
    ($e:expr) => {
        match $e {
            Step::Go(value) => value,
            Step::Stop(outcome) => return Ok(Step::Stop(outcome)),
        }
    };
}

impl<T: Transport> Session<'_, T> {
    /// Fetches what the current authorization needs and assembles its bundle.
    /// Transport and disk errors are errors (retry at the next session);
    /// everything the host or the protocol decides is an [`Outcome`]. Holds
    /// the cache lock for the whole session (`cache_busy` when another holds
    /// it), and prunes the cache afterwards whatever the result.
    pub async fn run(&self) -> Result<Outcome> {
        private_dir(self.cache)?;
        let _lock = lock_cache(self.cache)?;
        let mut reader = Reader::new(self.transport, SESSION_BYTES);
        let result = self.run_locked(&mut reader).await;
        // Writes only ever land in located cohorts, so pruning to those and
        // the installed bundle's bounds the cache by one session's reads.
        if !reader.named.is_empty() {
            let mut keep: Vec<&str> = reader.named.iter().map(String::as_str).collect();
            keep.extend(self.keep.iter().map(String::as_str));
            prune(self.cache, &keep)?;
        }
        let legacy = self.cache.join(LEGACY_AUTHORIZATIONS);
        if legacy.is_dir() {
            std::fs::remove_dir_all(&legacy)?;
        }
        result
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
        let (launch_len, cohort_len, renewal_len) = authorization_lengths(self.chain)?;
        let authorization = match pointer.kind.as_str() {
            "launch" => step!(
                self.authorization_object::<LaunchAuthorization>(
                    reader,
                    &pointer.authorization,
                    launch_len
                )
                .await?
            ),
            "cohort" => step!(
                self.authorization_object::<RewardIntentAuthorization>(
                    reader,
                    &pointer.authorization,
                    cohort_len
                )
                .await?
            ),
            other => bail!("unknown authorization kind {other:?}"),
        };
        let renewal = match &pointer.renewal {
            Some(r) => Some(step!(
                self.authorization_object::<RewardIntentRenewal>(reader, r, renewal_len)
                    .await?
            )),
            None => None,
        };
        let (bundle, advance, cohorts) = match pointer.kind.as_str() {
            "launch" => {
                if self.previous == Some(reader.fingerprint(&self.held).as_str()) {
                    return Ok(Outcome::Unchanged);
                }
                let bundle = BundleV2 {
                    schema: BUNDLE_SCHEMA_V2.into(),
                    shortcut: "T12".into(),
                    source: None,
                    authorization: AuthorizationHex {
                        kind: "launch".into(),
                        authorization: hex::encode(&authorization),
                        renewal: renewal.as_ref().map(hex::encode),
                        carrier: None,
                    },
                    receipt_chunks: Vec::new(),
                    carrier_receipt_chunks: Vec::new(),
                    receipts: None,
                    carrier_receipts: None,
                };
                (bundle, None, Vec::new())
            }
            _ => step!(
                self.cohort_bundle(reader, &authorization, renewal.as_deref())
                    .await?
            ),
        };
        let fingerprint = reader.fingerprint(&self.held);
        let bytes = serde_json::to_vec(&bundle)?;
        drop(bundle);
        if bytes.len() as u64 > MAX_BUNDLE_BYTES {
            return Ok(Outcome::Refused(Refusal::new(
                "bundle_too_large",
                "the assembled bundle exceeds the file bound",
            )));
        }
        let held = match &advance {
            Some(file) => file
                .restore(self.chain)
                .map_err(|r| anyhow::anyhow!("{}: {}", r.code, r.detail))?,
            None => self.held.clone(),
        };
        // Hashing and verifying a large bundle takes seconds; keep it off the
        // runtime thread that drives the finalized-head stream and signing.
        let (keys, chain) = (self.keys.clone(), self.chain.clone());
        let (bytes, verified) = tokio::task::spawn_blocking(move || {
            let verified =
                crate::package_v2::verify(&bytes, &keys, &chain.genesis.0, chain.netuid, &held);
            let sha256 = sha256_hex(&bytes);
            (bytes, verified.map(|a| (a, sha256)))
        })
        .await
        .context("bundle verification task")?;
        let (authorized, sha256) = match verified {
            Ok(v) => v,
            Err(refusal) => return Ok(Outcome::Refused(refusal)),
        };
        let info = BundleInfo::of(&bytes, &authorized)?;
        Ok(Outcome::Ready(Box::new(Staged {
            sha256,
            bundle: bytes,
            info,
            advance,
            stats: reader.stats.clone(),
            authorized: Arc::new(authorized),
            fingerprint,
            cohorts,
        })))
    }

    /// A content-addressed authorization or renewal object: its listed
    /// length must be the record's exact signed length, and it is read into
    /// memory and its signature checked before anything else uses it. It is
    /// never cached: it is a few hundred bytes.
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

    async fn authorization_object<R: Record>(
        &self,
        reader: &mut Reader<'_, T>,
        r: &ObjectRef,
        exact_len: u64,
    ) -> Result<Step<Vec<u8>>> {
        ensure!(safe_rel(&r.path), "unsafe authorization path");
        let sha = hex::encode(hex32(&r.sha256, "authorization sha256")?);
        if r.bytes != exact_len {
            return Ok(Step::Stop(Outcome::Refused(Refusal::new(
                "authorization_malformed",
                "the listed length is not the authorization record's signed length",
            ))));
        }
        let bytes = match reader.memory_object(&r.path, &sha, r.bytes).await? {
            InMemory::Bytes(bytes) => bytes,
            InMemory::Missing => {
                return Ok(Step::Stop(incomplete(
                    None,
                    "authorization_missing",
                    "the named authorization object is not published",
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

    async fn locate<'e>(
        &self,
        reader: &mut Reader<'_, T>,
        entry: &'e IndexEntry,
    ) -> Result<Step<CohortObjects<'e>>> {
        ensure!(safe_segment(&entry.label), "unsafe cohort label");
        let dir = entry.path.trim_end_matches('/');
        ensure!(safe_rel(dir), "unsafe cohort path");
        if !reader.named.contains(&entry.label) {
            reader.named.push(entry.label.clone());
        }
        let path = format!("{dir}/publication.json");
        let Some(publication) = reader.document::<Publication>(&path).await? else {
            return Ok(Step::Stop(incomplete(
                Some(&entry.label),
                "publication_missing",
                "the cohort's publication record is not published",
            )));
        };
        ensure!(
            publication.schema == PUBLICATION_SCHEMA,
            "unknown publication schema"
        );
        ensure!(
            publication.cohort == entry.cohort && publication.label == entry.label,
            "a publication record names another cohort"
        );
        Ok(Step::Go(CohortObjects { entry, publication }))
    }

    /// Reads one listed object of a located cohort into the cache.
    async fn cohort_object(
        &self,
        reader: &mut Reader<'_, T>,
        c: &CohortObjects<'_>,
        rel: &str,
    ) -> Result<Step<PathBuf>> {
        let label = c.entry.label.as_str();
        let Some(listed) = c.listed(rel) else {
            return Ok(Step::Stop(incomplete(
                Some(label),
                "object_unlisted",
                format!("{rel} is not in the publication record"),
            )));
        };
        ensure!(safe_rel(rel), "unsafe object path");
        let local = c.local(self.cache, rel);
        let sha = hex::encode(hex32(&listed.sha256, "object sha256")?);
        match reader
            .object(&format!("{}{rel}", c.dir()), &local, &sha, listed.bytes)
            .await?
        {
            Obtained::Missing => Ok(Step::Stop(incomplete(
                Some(label),
                "object_missing",
                format!("{rel} is listed but not served"),
            ))),
            Obtained::Mismatch => Ok(Step::Stop(incomplete(
                Some(label),
                "object_digest_mismatch",
                format!("{rel} failed its digest"),
            ))),
            _ => Ok(Step::Go(local)),
        }
    }

    /// The receipt objects of one cohort: the signed index, then every
    /// bundle the chunk map names. Before any bundle is read, the index's
    /// signature is checked, the chunk map's receipt root and count must be
    /// the signed index's, and the bundles' total length must be exactly what
    /// the index's entries declare; each bundle must be listed in the
    /// publication record with the chunk map's digest and length, under
    /// `receipts/`. A cohort whose receipts were deleted is served from the
    /// cache only, under the same checks and digests.
    async fn receipts(
        &self,
        reader: &mut Reader<'_, T>,
        c: &CohortObjects<'_>,
        by_ref: bool,
    ) -> Result<Step<Receipts>> {
        let label = c.entry.label.as_str();
        let cached_only = !c.entry.receipts_retained;
        let expired = || {
            Ok(Step::Stop(Outcome::ReceiptsExpired {
                cohort: label.to_owned(),
            }))
        };
        let (index, map_bytes) = if cached_only {
            let mut read = Vec::with_capacity(2);
            for rel in [RECEIPT_INDEX, CHUNK_MAP] {
                let path = c.local(self.cache, rel);
                let Ok(meta) = std::fs::metadata(&path) else {
                    return expired();
                };
                reader.reserve_once(&path, meta.len(), rel)?;
                reader.stats.cached += 1;
                read.push(std::fs::read(&path)?);
            }
            let map = read.pop().context("chunk map")?;
            (read.pop().context("receipt index")?, map)
        } else {
            if c.listed(RECEIPT_INDEX).is_none() || c.listed(CHUNK_MAP).is_none() {
                return Ok(Step::Stop(incomplete(
                    Some(label),
                    "receipts_unpublished",
                    "the cohort's receipts are not published",
                )));
            }
            let index = step_go!(self.cohort_object(reader, c, RECEIPT_INDEX).await?);
            let map = step_go!(self.cohort_object(reader, c, CHUNK_MAP).await?);
            (std::fs::read(index)?, std::fs::read(map)?)
        };
        let receipt_index = match Signed::<ReceiptIndex>::open(&index, self.keys) {
            Ok(record) => record,
            Err(e) => {
                return Ok(Step::Stop(Outcome::Refused(protocol(
                    "receipt_index_unverified",
                )(e))));
            }
        };
        let map: ChunkMap = serde_json::from_slice(&map_bytes).context("chunk map is malformed")?;
        ensure!(map.format == CHUNK_FORMAT, "unknown chunk map format");
        let disagree = |detail: &str| {
            Ok(Step::Stop(incomplete(
                Some(label),
                "chunk_map_mismatch",
                detail.to_owned(),
            )))
        };
        let root = receipt_index
            .digest()
            .map_err(|e| anyhow::anyhow!("receipt index digest: {e}"))?;
        if hex32(&map.receipt_root, "receipt root").ok() != Some(root.0)
            || map.receipt_count != receipt_index.entries.len() as u64
        {
            return disagree("the chunk map names another receipt root or count than the signed index");
        }
        let declared = receipt_index
            .entries
            .iter()
            .try_fold(0u64, |sum, e| {
                sum.checked_add(RECEIPT_LEN_PREFIX + u64::from(e.object_len))
            })
            .context("receipt lengths overflow")?;
        let listed_total = map
            .objects
            .iter()
            .try_fold(0u64, |sum, o| sum.checked_add(o.bytes))
            .context("chunk lengths overflow")?;
        if listed_total != declared {
            return disagree("the chunk map's bundle lengths differ from the signed receipt index");
        }
        drop(receipt_index);
        let prefix = format!("cohorts/{label}/");
        let mut chunks = Vec::with_capacity(map.objects.len());
        let mut refs = Vec::with_capacity(if by_ref { map.objects.len() } else { 0 });
        let reference = |path: &Path, sha: &str, bytes: u64| -> Result<BundleObject> {
            let path = std::path::absolute(path).context("absolute object path")?;
            Ok(BundleObject {
                path: path.to_str().context("object path is not UTF-8")?.to_owned(),
                sha256: sha.to_owned(),
                bytes,
            })
        };
        for o in &map.objects {
            let rel = o
                .key
                .strip_prefix(&prefix)
                .context("a chunk lies outside its cohort")?;
            ensure!(
                safe_rel(rel) && rel.starts_with("receipts/"),
                "unsafe chunk path"
            );
            let sha = hex::encode(hex32(&o.sha256, "chunk sha256")?);
            let path = c.local(self.cache, rel);
            if cached_only {
                if !by_ref {
                    reader.reserve_once(&path, o.bytes, rel)?;
                }
                match file_sha256(&path) {
                    Ok(found) if found == (o.bytes, sha.clone()) => {
                        reader.stats.cached += 1;
                        if by_ref {
                            refs.push(reference(&path, &sha, o.bytes)?);
                        } else {
                            chunks.push(std::fs::read(&path)?)
                        }
                    }
                    _ => return expired(),
                }
                continue;
            }
            match c.listed(rel) {
                Some(listed)
                    if listed.sha256.eq_ignore_ascii_case(&sha) && listed.bytes == o.bytes => {}
                Some(_) => {
                    return disagree("the chunk map and the publication record disagree");
                }
                None => {
                    return Ok(Step::Stop(incomplete(
                        Some(label),
                        "object_unlisted",
                        format!("{rel} is not in the publication record"),
                    )));
                }
            }
            let remote = format!("{}{rel}", c.dir());
            let obtained = if by_ref {
                reader.object_to_disk(&remote, &path, &sha, o.bytes).await?
            } else {
                reader.object(&remote, &path, &sha, o.bytes).await?
            };
            match obtained {
                Obtained::Missing => {
                    return Ok(Step::Stop(incomplete(
                        Some(label),
                        "object_missing",
                        format!("{rel} is listed but not served"),
                    )));
                }
                Obtained::Mismatch => {
                    return Ok(Step::Stop(incomplete(
                        Some(label),
                        "object_digest_mismatch",
                        format!("{rel} failed its digest"),
                    )));
                }
                _ if by_ref => refs.push(reference(&path, &sha, o.bytes)?),
                _ => chunks.push(std::fs::read(&path)?),
            }
        }
        if by_ref {
            let path = c.local(self.cache, RECEIPT_INDEX);
            let index_ref = reference(&path, &sha256_hex(&index), index.len() as u64)?;
            return Ok(Step::Go(Receipts::Referenced(ReceiptsRef { index: index_ref, chunks: refs })));
        }
        Ok(Step::Go(Receipts::Embedded(index, chunks)))
    }

    /// The cohort bundle for an authorization: the target's cohort, any
    /// carrier, and the intermediate targets between the held one and it.
    async fn cohort_bundle(
        &self,
        reader: &mut Reader<'_, T>,
        authorization: &[u8],
        renewal: Option<&[u8]>,
    ) -> Result<Step<(BundleV2, Option<HeldTargetFile>, Vec<String>)>> {
        // Checked when it was read; opened again to read its target.
        let auth = match Signed::<RewardIntentAuthorization>::open(authorization, self.keys) {
            Ok(a) => a,
            Err(e) => {
                return Ok(Step::Stop(Outcome::Refused(protocol(
                    "authorization_unverified",
                )(e))));
            }
        };
        let Some(index) = reader.document::<CohortIndex>(INDEX_PATH).await? else {
            return Ok(Step::Stop(incomplete(
                None,
                "index_missing",
                "the cohort index is not published",
            )));
        };
        ensure!(index.schema == INDEX_SCHEMA, "unknown cohort index schema");
        index.chain.check(self.chain, "the cohort index")?;
        let target_hex = hex::encode(auth.target.0);
        let by_target = |digest: &str| {
            index.cohorts.iter().find(|e| {
                e.target
                    .as_deref()
                    .is_some_and(|t| t.trim_start_matches("0x").eq_ignore_ascii_case(digest))
            })
        };
        let Some(target_entry) = by_target(&target_hex) else {
            return Ok(Step::Stop(incomplete(
                None,
                "target_unlisted",
                "the authorized target's cohort is not published yet",
            )));
        };
        let carrier_entry = if auth.cohort == target_entry.cohort {
            None
        } else {
            match index.cohorts.iter().find(|e| e.cohort == auth.cohort) {
                Some(e) => Some(e),
                None => {
                    return Ok(Step::Stop(incomplete(
                        None,
                        "carrier_unlisted",
                        "the carrying cohort is not published yet",
                    )));
                }
            }
        };
        let target = step_go!(self.locate(reader, target_entry).await?);
        let carrier = match carrier_entry {
            Some(e) => Some(step_go!(self.locate(reader, e).await?)),
            None => None,
        };

        // Intermediate targets: walk predecessors back to the held target.
        let held_hex = hex::encode(self.held.digest().0);
        let mut steps: Vec<CohortObjects<'_>> = Vec::new();
        if held_hex != target_hex {
            let mut current = step_go!(self.target_record(reader, &target).await?);
            loop {
                let Some(predecessor) = current.predecessor else {
                    return Ok(Step::Stop(Outcome::Refused(Refusal::new(
                        "held_target_unreachable",
                        "the target chain ends before the held target",
                    ))));
                };
                let predecessor = hex::encode(predecessor.0);
                if predecessor == held_hex {
                    break;
                }
                let floor = steps.last().map_or(target_entry.cohort, |s| s.entry.cohort);
                let Some(entry) = by_target(&predecessor).filter(|e| e.cohort < floor) else {
                    return Ok(Step::Stop(Outcome::Refused(Refusal::new(
                        "held_target_unreachable",
                        "the held target is not an ancestor of the published target",
                    ))));
                };
                let located = step_go!(self.locate(reader, entry).await?);
                current = step_go!(self.target_record(reader, &located).await?);
                steps.push(located);
            }
            steps.reverse();
        }

        // Every document is read: the same documents and held target as the
        // last bundle's mean the same bundle.
        if self.previous == Some(reader.fingerprint(&self.held).as_str()) {
            return Ok(Step::Stop(Outcome::Unchanged));
        }

        // Refuse before reading any object when the bundle could not fit:
        // every object it holds, receipt bundles included, must be listed.
        // The records are embedded hex-encoded; the receipt indexes and
        // bundles are embedded too when everything fits the file bound
        // (bundle schema v2), else named by reference (bundle schema v3),
        // so then only the records count.
        let mut records_planned = 0u64;
        let mut receipts_planned = 0u64;
        for (c, names) in std::iter::once((&target, &TARGET_OBJECTS[..]))
            .chain(carrier.iter().map(|c| (c, &CARRIER_OBJECTS[..])))
        {
            for rel in names.iter().copied().chain(std::iter::once(CHUNK_MAP)) {
                let bytes = c.listed(rel).map_or(0, |o| o.bytes);
                if rel == RECEIPT_INDEX {
                    receipts_planned = receipts_planned.saturating_add(bytes);
                } else {
                    records_planned = records_planned.saturating_add(bytes);
                }
            }
            receipts_planned = receipts_planned.saturating_add(
                c.publication
                    .objects
                    .iter()
                    .filter(|o| o.rel.starts_with("receipts/"))
                    .map(|o| o.bytes)
                    .fold(0u64, u64::saturating_add),
            );
            if !c.entry.receipts_retained {
                // Deleted from the host under the N+2 rule: served from the
                // cache, whose files decide the form.
                receipts_planned = receipts_planned.saturating_add(cached_receipt_bytes(&c.local(self.cache, "")));
            }
        }
        for s in &steps {
            for rel in STEP_OBJECTS {
                records_planned = records_planned.saturating_add(s.listed(rel).map_or(0, |o| o.bytes));
            }
        }
        let by_ref = records_planned.saturating_add(receipts_planned).saturating_mul(2) > MAX_BUNDLE_BYTES;
        if records_planned.saturating_mul(2) > MAX_BUNDLE_BYTES {
            return Ok(Step::Stop(Outcome::Refused(Refusal::new(
                "bundle_too_large",
                "the listed records cannot fit the bundle file bound",
            ))));
        }

        // Advance through intermediates, checked without receipts.
        let mut held = self.held.clone();
        let mut advance = None;
        for s in &steps {
            let mut objects = Vec::with_capacity(STEP_OBJECTS.len());
            for rel in STEP_OBJECTS {
                objects.push(std::fs::read(step_go!(
                    self.cohort_object(reader, s, rel).await?
                ))?);
            }
            held = match crate::fetch::advance(self.chain, self.keys, &held, &objects) {
                Ok(next) => next,
                Err(refusal) => return Ok(Step::Stop(Outcome::Refused(refusal))),
            };
            advance = Some(HeldTargetFile::of(&held));
        }

        let mut records = Vec::with_capacity(TARGET_OBJECTS.len());
        for rel in TARGET_OBJECTS.iter().filter(|r| **r != RECEIPT_INDEX) {
            records.push(std::fs::read(step_go!(
                self.cohort_object(reader, &target, rel).await?
            ))?);
        }
        let receipts = step_go!(self.receipts(reader, &target, by_ref).await?);
        let (carrier_hex, carrier_receipts) = match &carrier {
            Some(c) => {
                let mut objects = Vec::new();
                for rel in CARRIER_OBJECTS.iter().filter(|r| **r != RECEIPT_INDEX) {
                    objects.push(std::fs::read(step_go!(
                        self.cohort_object(reader, c, rel).await?
                    ))?);
                }
                let receipts = step_go!(self.receipts(reader, c, by_ref).await?);
                let [manifest, package, contents, fingerprints] = <[Vec<u8>; 4]>::try_from(objects)
                    .map_err(|_| anyhow::anyhow!("carrier objects"))?;
                (
                    Some(CarrierHex {
                        manifest: hex::encode(manifest),
                        package: hex::encode(package),
                        receipt_index: receipts.embedded_index(),
                        published_contents: hex::encode(contents),
                        fingerprint_index: hex::encode(fingerprints),
                    }),
                    Some(receipts),
                )
            }
            None => (None, None),
        };
        let [
            manifest,
            package,
            target_rec,
            closure,
            contents,
            fingerprints,
        ] = <[Vec<u8>; 6]>::try_from(records).map_err(|_| anyhow::anyhow!("target objects"))?;
        Ok(Step::Go((
            BundleV2 {
                schema: if by_ref { BUNDLE_SCHEMA_V3 } else { BUNDLE_SCHEMA_V2 }.into(),
                shortcut: "T12".into(),
                source: Some(SourceHex {
                    manifest: hex::encode(manifest),
                    package: hex::encode(package),
                    target: hex::encode(target_rec),
                    closure: hex::encode(closure),
                    receipt_index: receipts.embedded_index(),
                    published_contents: hex::encode(contents),
                    fingerprint_index: hex::encode(fingerprints),
                }),
                authorization: AuthorizationHex {
                    kind: "cohort".into(),
                    authorization: hex::encode(authorization),
                    renewal: renewal.map(hex::encode),
                    carrier: carrier_hex,
                },
                receipt_chunks: receipts.embedded_chunks(),
                carrier_receipt_chunks: carrier_receipts.as_ref().map(Receipts::embedded_chunks).unwrap_or_default(),
                receipts: receipts.referenced(),
                carrier_receipts: carrier_receipts.and_then(|r| r.referenced()),
            },
            advance,
            std::iter::once(target.entry.label.clone())
                .chain(carrier.iter().map(|c| c.entry.label.clone()))
                .collect(),
        )))
    }

    /// A located cohort's calculation target, authenticated.
    async fn target_record(
        &self,
        reader: &mut Reader<'_, T>,
        c: &CohortObjects<'_>,
    ) -> Result<Step<CalculationTargetV2>> {
        let path = step_go!(self.cohort_object(reader, c, "target.signed").await?);
        match Signed::<CalculationTargetV2>::open(&std::fs::read(path)?, self.keys) {
            Ok(t) => Ok(Step::Go(t)),
            Err(e) => Ok(Step::Stop(Outcome::Refused(protocol("target_unverified")(
                e,
            )))),
        }
    }
}

/// Checks an intermediate target's records (`STEP_OBJECTS` order) and
/// returns the target its checked closure activates, which must be the held
/// target's successor. Every signed record is authenticated before it is
/// decoded; the package is bound to its manifest and the target to the
/// package; the published index must match the manifest's binding; the
/// outcome is recomputed; the closure is checked against the frozen
/// minimums. No receipt is checked and no commitment is made for it: the
/// validator only learns that the target was activated and superseded.
pub fn advance(
    chain: &ChainBinding,
    keys: &TrustedKeys,
    held: &ActiveTarget,
    objects: &[Vec<u8>],
) -> Result<ActiveTarget, Refusal> {
    let [manifest, package, target, closure, contents, fingerprints] = objects else {
        return Err(Refusal::new("bundle_malformed", "intermediate records"));
    };
    let manifest = Signed::<CohortManifestV2>::open(manifest, keys)
        .map_err(protocol("manifest_unverified"))?;
    let package =
        Signed::<ScorePackageV2>::open(package, keys).map_err(protocol("package_unverified"))?;
    let target =
        Signed::<CalculationTargetV2>::open(target, keys).map_err(protocol("target_unverified"))?;
    let closure =
        Signed::<CohortClosure>::open(closure, keys).map_err(protocol("closure_unverified"))?;
    if manifest.chain != *chain || package.chain != *chain || target.chain != *chain {
        return Err(Refusal::new(
            "package_chain_mismatch",
            "intermediate records name another chain",
        ));
    }
    package
        .check_manifest(&manifest)
        .map_err(protocol("package_manifest_mismatch"))?;
    target
        .check_package(&package)
        .map_err(protocol("target_package_mismatch"))?;
    let index = PublishedIndex {
        contents: PublishedContents::from_bytes(contents)
            .map_err(protocol("published_index_invalid"))?,
        fingerprints: FingerprintIndex::from_bytes(fingerprints)
            .map_err(protocol("published_index_invalid"))?,
    };
    index
        .validate()
        .map_err(protocol("published_index_invalid"))?;
    if index
        .binding()
        .map_err(protocol("published_index_invalid"))?
        != manifest.published
    {
        return Err(Refusal::new(
            "published_index_mismatch",
            "the published index does not match the manifest",
        ));
    }
    slop_ninja_protocol::reward_v1::check_outcome(&package, &index)
        .map_err(protocol("outcome_mismatch"))?;
    let checked = closure
        .check(&manifest, &package, Some(&target))
        .map_err(protocol("closure_refused"))?;
    let activation = ActiveTarget::activated(&checked).map_err(protocol("closure_refused"))?;
    if target.predecessor != Some(held.digest()) {
        return Err(Refusal::new(
            "held_target_unreachable",
            "the intermediate target does not succeed the held target",
        ));
    }
    if !crate::package_v2::advances(held, &activation) {
        return Err(Refusal::new(
            "held_target_regressed",
            "the intermediate target activates before the held target",
        ));
    }
    Ok(activation)
}

/// Removes cached cohorts other than `keep`, and the authorization objects
/// an earlier layout cached. The cache then holds at most the cohorts one
/// session named and those the installed bundle was built from.
pub fn prune(cache: &Path, keep: &[&str]) -> Result<()> {
    let dir = cache.join("cohorts");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if keep.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
        }
    }
    let legacy = cache.join(LEGACY_AUTHORIZATIONS);
    if legacy.is_dir() {
        std::fs::remove_dir_all(&legacy)?;
    }
    Ok(())
}

/// Labels of the cohorts an installed bundle was built from (its target's
/// and its carrier's), read from the bundle's own manifests.
pub fn bundle_cohorts(bytes: &[u8]) -> Result<Vec<String>> {
    let b: BundleV2 = serde_json::from_slice(bytes)?;
    let label = |hex_manifest: &str| -> Result<String> {
        Ok(
            Signed::<CohortManifestV2>::from_bytes(&hex::decode(hex_manifest)?)
                .map_err(|e| anyhow::anyhow!("manifest: {e}"))?
                .record
                .cohort_label,
        )
    };
    let mut cohorts = Vec::new();
    if let Some(source) = &b.source {
        cohorts.push(label(&source.manifest)?);
    }
    if let Some(carrier) = &b.authorization.carrier {
        cohorts.push(label(&carrier.manifest)?);
    }
    Ok(cohorts)
}
