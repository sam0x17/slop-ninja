//! Verifies one closed `reward-v1` cohort from its stored objects, as a
//! validator would, before anything is published and again on the copies.
//!
//! Every signed record is authenticated under the pinned trust roots before
//! it is decoded ([`Signed::open`]). The checks, in order:
//!
//! - the manifest is this chain's and names this cohort's label;
//! - the package is bound to the manifest; the target, when stored, to the
//!   package;
//! - the closure is checked against the manifest's frozen schedule, the
//!   package and its target ([`CohortClosure::check`]);
//! - the receipt index is bound to the package's root and count, every chunk
//!   object in the chunk map has its listed digest and length, and every
//!   receipt cut from a chunk verifies against its index entry, each entry
//!   exactly once (when receipts are published, decision D7);
//! - the published index at the seal matches the manifest's binding, and the
//!   outcome recomputed from the package equals the package's own
//!   ([`reward_v1::check_outcome`]);
//! - the published index after the closure equals its rebuild from the
//!   closure's fingerprints.
//!
//! Only the objects named here are ever copied: the private batch, results,
//! summaries, probe vectors and plans in the same buckets are never read.
//! Binary records decode canonically and completely, so a verified record
//! carries nothing beyond its schema. The one diagnostic JSON object,
//! `components.json`, is published only when it parses as its typed schema,
//! agrees with the manifest and package field for field, and passes the
//! redaction scan ([`crate::redact`]). The receipt index is authenticated and
//! walked in place ([`RawIndex`]), never decoded into memory.

use serde::{Deserialize, Serialize};
use slop_ninja_protocol::reward_v1::{self, Evaluation};
use slop_ninja_protocol::submission::CohortClosure;
use slop_ninja_protocol::{
    AccountId, AttemptReceipt, CalculationTargetV2, Canonical, ChainBinding, CohortManifestV2,
    Decoder, Digest, EntrantFingerprint, FingerprintIndex, MAX_SEQUENCE_LEN, PublishedContents,
    PublishedIndex, ReceiptIndex, ReceiptIndexEntry, Record, ScorePackageV2, Signed, TrustedKeys,
    domain_hash, object_sha256,
};

use crate::arena::{self, Budget, Hold, Refused};
use crate::layout;
use crate::store::{Store, StoreError};

pub const MANIFEST: &str = "manifest.signed";
pub const PACKAGE: &str = "package.signed";
pub const TARGET: &str = "target.signed";
pub const RECEIPT_INDEX: &str = "receipt-index.signed";
pub const CHUNK_MAP: &str = "chunk-map.json";
pub const CONTENTS: &str = "published-contents.bin";
pub const FINGERPRINTS: &str = "fingerprint-index.bin";
pub const CLOSURE: &str = "closure.signed";
pub const CLOSURE_FINGERPRINTS: &str = "closure-fingerprints.json";
pub const CONTENTS_AFTER: &str = "published-contents-after.bin";
pub const FINGERPRINTS_AFTER: &str = "fingerprint-index-after.bin";
pub const COMPONENTS: &str = "components.json";

/// The receipt chunk format the chunk map names.
pub const CHUNK_FORMAT: &str = "slop_ninja_receipt_chunk_v1";
/// The components schema the publisher knows how to check.
pub const COMPONENTS_SCHEMA: &str = "slop_ninja_eval_score_components_v2";

/// Why a cohort was not verified. Content-free: object names, codes and
/// protocol error texts only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A required object is not stored (yet): retried next run.
    Missing(String),
    /// The store failed: retried next run.
    Store(String, StoreError),
    /// The object fits its schema bound but not the run's memory budget.
    Budget(String, Refused),
    /// The cohort failed verification: never published.
    Invalid(String, String),
}

impl Refusal {
    pub fn retryable(&self) -> bool {
        match self {
            Self::Missing(_) => true,
            // A write the allow-list refuses is refused again next run.
            Self::Store(_, e) => *e != StoreError::NotAllowed,
            _ => false,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Missing(_) => "missing",
            Self::Store(..) => "store",
            Self::Budget(..) => "budget",
            Self::Invalid(..) => "invalid",
        }
    }
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Missing(rel) => write!(f, "{rel}: not stored"),
            Self::Store(rel, e) => write!(f, "{rel}: {e}"),
            Self::Budget(rel, r) => write!(
                f,
                "{rel}: needs {} bytes with {} held of {}",
                r.want, r.held, r.limit
            ),
            Self::Invalid(rel, why) => write!(f, "{rel}: {why}"),
        }
    }
}

fn invalid(rel: &str, why: impl core::fmt::Display) -> Refusal {
    Refusal::Invalid(rel.to_owned(), why.to_string())
}

/// One object to publish: its path relative to the cohort directory and the
/// digest and length verified.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicObject {
    pub rel: String,
    pub sha256: String,
    pub bytes: u64,
}

impl PublicObject {
    fn of(rel: &str, bytes: &[u8]) -> Self {
        Self {
            rel: rel.to_owned(),
            sha256: object_sha256(bytes).hex(),
            bytes: bytes.len() as u64,
        }
    }

    pub fn digest(&self) -> Option<Digest> {
        let b: [u8; 32] = hex::decode(&self.sha256).ok()?.try_into().ok()?;
        Some(Digest(b))
    }
}

/// What happened to `components.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Components {
    Absent,
    Published(PublicObject),
    Withheld(String),
}

/// A verified cohort and the objects to publish, in read order.
#[derive(Clone, Debug)]
pub struct VerifiedCohort {
    pub label: String,
    pub manifest: CohortManifestV2,
    pub package: ScorePackageV2,
    pub target: Option<CalculationTargetV2>,
    pub closure: CohortClosure,
    pub evaluation: Evaluation,
    pub objects: Vec<PublicObject>,
    pub components: Components,
    /// Receipts verified against the index (0 when receipts are not published).
    pub receipts: u64,
}

impl VerifiedCohort {
    /// Every object to publish, `components.json` included when it passed.
    pub fn public_objects(&self) -> Vec<PublicObject> {
        let mut all = self.objects.clone();
        if let Components::Published(o) = &self.components {
            all.push(o.clone());
        }
        all
    }
}

pub struct Options<'a> {
    pub keys: &'a TrustedKeys,
    pub chain: &'a ChainBinding,
    /// Verify (and so publish) the receipt index and chunks.
    pub receipts: bool,
}

struct Reader<'s, S> {
    store: &'s S,
    dir: &'s str,
    budget: &'s Budget,
    keys: &'s TrustedKeys,
}

impl<'s, S: Store> Reader<'s, S> {
    /// Reads `rel` when its stored size is within `bound` (its schema bound)
    /// and the remaining budget, and reserves its bytes while they are held.
    async fn read(&self, rel: &str, bound: u64) -> Result<(Vec<u8>, Hold<'s>), Refusal> {
        let key = layout::join(self.dir, rel);
        let limit = bound.min(self.budget.remaining());
        let bytes = match self.store.get(&key, limit).await {
            Ok(Some(b)) => b,
            Ok(None) => return Err(Refusal::Missing(rel.to_owned())),
            Err(StoreError::TooLarge { size, .. }) if size > bound => {
                return Err(invalid(
                    rel,
                    format!("{size} bytes exceed the schema bound of {bound}"),
                ));
            }
            Err(StoreError::TooLarge { size, .. }) => {
                return Err(Refusal::Budget(
                    rel.to_owned(),
                    Refused {
                        want: size,
                        held: self.budget.held(),
                        limit: self.budget.held() + self.budget.remaining(),
                    },
                ));
            }
            Err(e) => return Err(Refusal::Store(rel.to_owned(), e)),
        };
        let hold = self
            .budget
            .reserve(bytes.len() as u64)
            .map_err(|r| Refusal::Budget(rel.to_owned(), r))?;
        Ok((bytes, hold))
    }

    async fn exists(&self, rel: &str) -> Result<bool, Refusal> {
        self.store
            .head(&layout::join(self.dir, rel))
            .await
            .map(|m| m.is_some())
            .map_err(|e| Refusal::Store(rel.to_owned(), e))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChunkMap {
    format: String,
    cohort_manifest: String,
    receipt_root: String,
    receipt_count: u64,
    objects: Vec<ChunkObject>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChunkObject {
    key: String,
    sha256: String,
    bytes: u64,
    receipts: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosureFingerprints {
    fingerprints: Vec<ClosureFingerprint>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClosureFingerprint {
    hotkey: String,
    version: u32,
    fingerprint: String,
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || s.bytes().any(|b| b.is_ascii_uppercase()) {
        return None;
    }
    hex::decode(s).ok()?.try_into().ok()
}

/// What opening a cohort's manifest alone says, before anything else of the
/// cohort is read: enough to place it inside or outside the public record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestCheck {
    Opened(Box<CohortManifestV2>),
    /// The manifest does not open under the compiled trust roots (another
    /// signer, an older record format, or damaged bytes).
    Unverifiable(String),
    /// Signed by our key for another chain or subnet.
    OtherChain,
    /// Not a `reward-v1` cohort manifest at all (a load or older cohort):
    /// the envelope's record kind or version says so.
    OtherKind,
    /// Its label differs from its directory's.
    OtherLabel,
}

async fn read_manifest<S: Store>(
    r: &Reader<'_, S>,
    label: &str,
    chain: &ChainBinding,
) -> Result<(ManifestCheck, PublicObject), Refusal> {
    let (raw, _hold) = r.read(MANIFEST, arena::READBACK_BUDGET).await?;
    let object = PublicObject::of(MANIFEST, &raw);
    let check = match Signed::<CohortManifestV2>::open(&raw, r.keys) {
        Err(_) if !manifest_kind(&raw) => ManifestCheck::OtherKind,
        Err(e) => ManifestCheck::Unverifiable(e.to_string()),
        Ok(m) if m.chain != *chain => ManifestCheck::OtherChain,
        Ok(m) if m.cohort_label != label => ManifestCheck::OtherLabel,
        Ok(m) => ManifestCheck::Opened(Box::new(m)),
    };
    Ok((check, object))
}

/// Whether a signed envelope's record is a `reward-v1` cohort manifest by its
/// kind and version: only the envelope header and the record's kind string
/// are read, through the protocol's bounded decoder, to route a manifest
/// that does not open. Nothing else of unauthenticated bytes is decoded, and
/// a wrong answer can only leave a cohort outside the record.
fn manifest_kind(raw: &[u8]) -> bool {
    let mut d = Decoder::new(raw);
    let header = (|| -> slop_ninja_protocol::Result<(String, u16)> {
        d.str()?;
        d.u16()?;
        d.u8()?;
        d.u32()?;
        Ok((d.str()?.to_owned(), d.u16()?))
    })();
    match header {
        Ok((kind, version)) => {
            kind == CohortManifestV2::KIND && version == CohortManifestV2::VERSION
        }
        // Too damaged to say: treat as a manifest that does not open.
        Err(_) => true,
    }
}

/// Opens only the manifest of the cohort stored under `dir`.
pub async fn open_manifest<S: Store>(
    store: &S,
    dir: &str,
    label: &str,
    opts: &Options<'_>,
    budget: &Budget,
) -> Result<ManifestCheck, Refusal> {
    if !layout::safe_label(label) {
        return Err(invalid(label, "unsafe cohort label"));
    }
    let r = Reader {
        store,
        dir,
        budget,
        keys: opts.keys,
    };
    Ok(read_manifest(&r, label, opts.chain).await?.0)
}

/// Verifies the cohort stored under `dir` (`cohorts/<label>` in the private
/// bucket, `v1/cohorts/<label>` in the public one).
pub async fn verify_cohort<S: Store>(
    store: &S,
    dir: &str,
    label: &str,
    opts: &Options<'_>,
    budget: &Budget,
) -> Result<VerifiedCohort, Refusal> {
    if !layout::safe_label(label) {
        return Err(invalid(label, "unsafe cohort label"));
    }
    let r = Reader {
        store,
        dir,
        budget,
        keys: opts.keys,
    };
    let keys = opts.keys;
    let mut objects = Vec::new();

    // Records. Each raw object is held only while it is opened.
    let record_bound = arena::READBACK_BUDGET;
    let manifest = match read_manifest(&r, label, opts.chain).await? {
        (ManifestCheck::Opened(m), object) => {
            objects.push(object);
            *m
        }
        (ManifestCheck::Unverifiable(why), _) => return Err(invalid(MANIFEST, why)),
        (ManifestCheck::OtherChain, _) => return Err(invalid(MANIFEST, "another chain or subnet")),
        (ManifestCheck::OtherKind, _) => {
            return Err(invalid(MANIFEST, "not a reward-v1 cohort manifest"));
        }
        (ManifestCheck::OtherLabel, _) => {
            return Err(invalid(MANIFEST, "label differs from its directory"));
        }
    };
    let package = {
        let (raw, _hold) = r.read(PACKAGE, record_bound).await?;
        objects.push(PublicObject::of(PACKAGE, &raw));
        Signed::<ScorePackageV2>::open(&raw, keys).map_err(|e| invalid(PACKAGE, e))?
    };
    package
        .check_manifest(&manifest)
        .map_err(|e| invalid(PACKAGE, e))?;
    let target = if r.exists(TARGET).await? {
        let (raw, _hold) = r.read(TARGET, record_bound).await?;
        objects.push(PublicObject::of(TARGET, &raw));
        let t = Signed::<CalculationTargetV2>::open(&raw, keys).map_err(|e| invalid(TARGET, e))?;
        t.check_package(&package).map_err(|e| invalid(TARGET, e))?;
        Some(t)
    } else {
        None
    };
    let closure = {
        let (raw, _hold) = r.read(CLOSURE, record_bound).await?;
        let closure = Signed::<CohortClosure>::open(&raw, keys).map_err(|e| invalid(CLOSURE, e))?;
        // Listed after the receipts and the index, as a reader fetches them.
        (closure, PublicObject::of(CLOSURE, &raw))
    };
    let (closure, closure_object) = closure;
    if closure.target.is_some() != target.is_some() {
        return Err(invalid(CLOSURE, "closure and stored target disagree"));
    }
    closure
        .check(&manifest, &package, target.as_ref())
        .map_err(|e| invalid(CLOSURE, e))?;

    // Receipts.
    let receipts = if opts.receipts {
        verify_receipts(&r, label, &manifest, &package, &mut objects).await?
    } else {
        0
    };

    // The published index at the seal, and the recomputed outcome.
    let at_seal = {
        let (raw, _hold) = r
            .read(
                CONTENTS,
                arena::contents_bound(manifest.published.content_count),
            )
            .await?;
        objects.push(PublicObject::of(CONTENTS, &raw));
        let contents = PublishedContents::from_bytes(&raw).map_err(|e| invalid(CONTENTS, e))?;
        drop(raw);
        let (raw, _hold) = r
            .read(
                FINGERPRINTS,
                arena::fingerprints_bound(manifest.published.fingerprint_count),
            )
            .await?;
        objects.push(PublicObject::of(FINGERPRINTS, &raw));
        let fingerprints =
            FingerprintIndex::from_bytes(&raw).map_err(|e| invalid(FINGERPRINTS, e))?;
        PublishedIndex {
            contents,
            fingerprints,
        }
    };
    at_seal.validate().map_err(|e| invalid(CONTENTS, e))?;
    if at_seal.binding().map_err(|e| invalid(CONTENTS, e))? != manifest.published {
        return Err(invalid(
            CONTENTS,
            "published index differs from the manifest's binding",
        ));
    }
    let evaluation = reward_v1::check_outcome(&package, &at_seal)
        .map_err(|e| invalid(PACKAGE, format!("recomputed outcome differs: {e}")))?;

    // The closure and the index after it.
    objects.push(closure_object);
    let entrants = package.entrants.len() as u64;
    let probes = package.probes.len() as u64;
    let fps = {
        let (raw, _hold) = r
            .read(
                CLOSURE_FINGERPRINTS,
                arena::closure_fingerprints_bound(entrants.saturating_mul(probes)),
            )
            .await?;
        objects.push(PublicObject::of(CLOSURE_FINGERPRINTS, &raw));
        let parsed: ClosureFingerprints =
            serde_json::from_slice(&raw).map_err(|e| invalid(CLOSURE_FINGERPRINTS, e))?;
        parsed
            .fingerprints
            .iter()
            .map(|f| {
                Some(EntrantFingerprint {
                    hotkey: AccountId(hex32(&f.hotkey)?),
                    version: f.version,
                    fingerprint: Digest(hex32(&f.fingerprint)?),
                })
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| invalid(CLOSURE_FINGERPRINTS, "not 64-hex digests"))?
    };
    let rebuilt = at_seal
        .after_closure(&package, fps)
        .map_err(|e| invalid(CLOSURE_FINGERPRINTS, e))?;
    drop(at_seal);
    let after = {
        let (raw, _hold) = r
            .read(
                CONTENTS_AFTER,
                arena::contents_bound(manifest.published.content_count.saturating_add(entrants)),
            )
            .await?;
        objects.push(PublicObject::of(CONTENTS_AFTER, &raw));
        let contents =
            PublishedContents::from_bytes(&raw).map_err(|e| invalid(CONTENTS_AFTER, e))?;
        drop(raw);
        let (raw, _hold) = r
            .read(
                FINGERPRINTS_AFTER,
                arena::fingerprints_bound(
                    manifest
                        .published
                        .fingerprint_count
                        .saturating_add(entrants.saturating_mul(probes)),
                ),
            )
            .await?;
        objects.push(PublicObject::of(FINGERPRINTS_AFTER, &raw));
        let fingerprints =
            FingerprintIndex::from_bytes(&raw).map_err(|e| invalid(FINGERPRINTS_AFTER, e))?;
        PublishedIndex {
            contents,
            fingerprints,
        }
    };
    if rebuilt != after {
        return Err(invalid(
            CONTENTS_AFTER,
            "index after the closure differs from its rebuild",
        ));
    }

    let components = check_components(&r, label, &manifest, &package).await?;
    Ok(VerifiedCohort {
        label: label.to_owned(),
        manifest,
        package,
        target,
        closure,
        evaluation,
        objects,
        components,
        receipts,
    })
}

/// A signed receipt index read in place: the authenticated record bytes, its
/// header, and its entries as fixed-width canonical encodings. Nothing is
/// decoded into memory beyond one entry at a time.
pub struct RawIndex<'a> {
    pub cohort_manifest: Digest,
    entries: &'a [u8],
    count: usize,
}

impl<'a> RawIndex<'a> {
    /// Applies `ReceiptIndex`'s decoding rules to authenticated record bytes
    /// (`Signed::open_raw`): kind and version, the manifest digest, a length
    /// prefix within the sequence bound, exactly that many canonical entries
    /// and nothing after them, attempts strictly increasing.
    pub fn parse(record: &'a [u8]) -> slop_ninja_protocol::Result<Self> {
        use slop_ninja_protocol::Error;
        let mut d = Decoder::new(record);
        if d.str()? != ReceiptIndex::KIND {
            return Err(Error::WrongKind {
                expected: ReceiptIndex::KIND,
            });
        }
        let version = d.u16()?;
        if version != ReceiptIndex::VERSION {
            return Err(Error::WrongVersion {
                kind: ReceiptIndex::KIND,
                version,
            });
        }
        let cohort_manifest = Digest::decode_from(&mut d)?;
        let count = d.len_prefix("sequence", MAX_SEQUENCE_LEN)?;
        let width = arena::receipt_entry_len() as usize;
        let entries = d.take(
            count
                .checked_mul(width)
                .ok_or(Error::Overflow("receipt entries"))?,
        )?;
        d.finish()?;
        let index = Self {
            cohort_manifest,
            entries,
            count,
        };
        let mut previous: Option<Digest> = None;
        for i in 0..count {
            let attempt = index.entry(i)?.attempt;
            if let Some(p) = previous {
                match p.cmp(&attempt) {
                    core::cmp::Ordering::Less => {}
                    core::cmp::Ordering::Equal => return Err(Error::Duplicate("receipt attempt")),
                    core::cmp::Ordering::Greater => {
                        return Err(Error::Unsorted("receipt attempt"));
                    }
                }
            }
            previous = Some(attempt);
        }
        Ok(index)
    }

    pub fn count(&self) -> usize {
        self.count
    }

    fn raw(&self, i: usize) -> &'a [u8] {
        let width = arena::receipt_entry_len() as usize;
        &self.entries[i * width..(i + 1) * width]
    }

    /// Entry `i`, decoded canonically and completely.
    pub fn entry(&self, i: usize) -> slop_ninja_protocol::Result<ReceiptIndexEntry> {
        if i >= self.count {
            return Err(slop_ninja_protocol::Error::OutOfRange("receipt position"));
        }
        let mut d = Decoder::new(self.raw(i));
        let entry = ReceiptIndexEntry::decode_from(&mut d)?;
        d.finish()?;
        Ok(entry)
    }

    /// The position of `attempt`, by binary search over the sorted entries
    /// (an entry's encoding starts with its attempt ID).
    pub fn position(&self, attempt: &Digest) -> Option<usize> {
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.raw(mid)[..32].cmp(&attempt.0[..]) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    /// `ReceiptIndex::verify_object` for entry `i`, through the protocol's own
    /// check on a one-entry index.
    pub fn verify_object(
        &self,
        i: usize,
        bytes: &[u8],
    ) -> slop_ninja_protocol::Result<AttemptReceipt> {
        let one = ReceiptIndex {
            cohort_manifest: self.cohort_manifest,
            entries: vec![self.entry(i)?],
        };
        one.verify_object(0, bytes)
    }
}

async fn verify_receipts<S: Store>(
    r: &Reader<'_, S>,
    label: &str,
    manifest: &CohortManifestV2,
    package: &ScorePackageV2,
    objects: &mut Vec<PublicObject>,
) -> Result<u64, Refusal> {
    let count = package.receipt_count;
    let root = package.receipt_root.hex();
    // The chunk map first: it names every bundle's length, so the largest one
    // is known before the index is read. It is unsigned; its totals are
    // checked against the signed index below, so a wrong map only refuses.
    let (map, map_object): (ChunkMap, PublicObject) = {
        let jobs = manifest.roster.len() as u64 + 2;
        let (raw, _hold) = r
            .read(CHUNK_MAP, arena::chunk_map_bound(jobs, label.len() as u64))
            .await?;
        let map = serde_json::from_slice(&raw).map_err(|e| invalid(CHUNK_MAP, e))?;
        (map, PublicObject::of(CHUNK_MAP, &raw))
    };
    if map.format != CHUNK_FORMAT
        || map.cohort_manifest != package.cohort_manifest.hex()
        || map.receipt_root != root
        || map.receipt_count != count
    {
        return Err(invalid(
            CHUNK_MAP,
            "format, manifest, root or count differs from the signed index",
        ));
    }
    let mut keys = std::collections::BTreeSet::new();
    for o in &map.objects {
        if o.receipts == 0 || o.bytes == 0 {
            return Err(invalid(CHUNK_MAP, "a chunk object holds no receipt"));
        }
        if !keys.insert(o.key.as_str()) {
            return Err(invalid(CHUNK_MAP, "a chunk object is listed twice"));
        }
    }
    let largest = map.objects.iter().map(|o| o.bytes).max().unwrap_or(0);
    let peak = arena::receipt_verification_peak(count, largest);
    if peak > r.budget.remaining() {
        return Err(Refusal::Budget(
            RECEIPT_INDEX.into(),
            Refused {
                want: peak,
                held: r.budget.held(),
                limit: r.budget.held() + r.budget.remaining(),
            },
        ));
    }
    let (raw_index, _index_hold) = r
        .read(RECEIPT_INDEX, arena::receipt_index_bound(count))
        .await?;
    objects.push(PublicObject::of(RECEIPT_INDEX, &raw_index));
    let record = Signed::<ReceiptIndex>::open_raw(&raw_index, r.keys)
        .map_err(|e| invalid(RECEIPT_INDEX, e))?;
    let index = RawIndex::parse(record).map_err(|e| invalid(RECEIPT_INDEX, e))?;
    // `ScorePackageV2::check_receipts`, with the digest taken over the
    // authenticated raw record (canonical decoding makes it the record's).
    if index.cohort_manifest != package.cohort_manifest
        || domain_hash(&ReceiptIndex::domain(), record) != package.receipt_root
        || index.count() as u64 != count
    {
        return Err(invalid(
            RECEIPT_INDEX,
            "receipt root or count differs from the package",
        ));
    }
    objects.push(map_object);
    // The one-bit-per-entry bitmap, held until every receipt is seen.
    let _seen_hold = r
        .budget
        .reserve(arena::receipt_bitmap_bytes(count))
        .map_err(|e| Refusal::Budget(RECEIPT_INDEX.into(), e))?;
    // The chunk objects hold exactly the indexed receipts, each framed by a
    // 4-byte length: their total size is known before any is read.
    let mut want_bytes = 0u64;
    for i in 0..index.count() {
        let e = index.entry(i).map_err(|e| invalid(RECEIPT_INDEX, e))?;
        want_bytes += 4 + u64::from(e.object_len);
    }
    let listed_bytes: u64 = map.objects.iter().map(|o| o.bytes).sum();
    let listed_receipts: u64 = map.objects.iter().map(|o| o.receipts).sum();
    if listed_bytes != want_bytes || listed_receipts != count {
        return Err(invalid(
            CHUNK_MAP,
            "chunk objects do not hold exactly the indexed receipts",
        ));
    }
    let mut seen = vec![0u64; index.count().div_ceil(64)];
    let mut verified = 0u64;
    for object in &map.objects {
        let rel = layout::rel_of_private_key(label, &object.key)
            .ok_or_else(|| invalid(CHUNK_MAP, "chunk key outside the cohort directory"))?
            .to_owned();
        if !rel.starts_with("receipts/") {
            return Err(invalid(CHUNK_MAP, "chunk key outside receipts/"));
        }
        let (raw, _hold) = r.read(&rel, object.bytes).await?;
        if raw.len() as u64 != object.bytes || object_sha256(&raw).hex() != object.sha256 {
            return Err(invalid(&rel, "digest or length differs from the chunk map"));
        }
        let mut pos = 0usize;
        let mut in_object = 0u64;
        while pos < raw.len() {
            let len_bytes: [u8; 4] = raw
                .get(pos..pos + 4)
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| invalid(&rel, "truncated receipt frame"))?;
            let len = u32::from_le_bytes(len_bytes) as usize;
            let body = raw
                .get(pos + 4..pos + 4 + len)
                .ok_or_else(|| invalid(&rel, "truncated receipt"))?;
            let receipt = AttemptReceipt::from_bytes(body).map_err(|e| invalid(&rel, e))?;
            let position = index
                .position(&receipt.attempt)
                .ok_or_else(|| invalid(&rel, "receipt not in the index"))?;
            index
                .verify_object(position, body)
                .map_err(|e| invalid(&rel, e))?;
            let (word, bit) = (position / 64, position % 64);
            if seen[word] >> bit & 1 == 1 {
                return Err(invalid(&rel, "receipt listed twice"));
            }
            seen[word] |= 1 << bit;
            verified += 1;
            in_object += 1;
            pos += 4 + len;
        }
        if in_object != object.receipts {
            return Err(invalid(&rel, "receipt count differs from the chunk map"));
        }
        objects.push(PublicObject::of(&rel, &raw));
    }
    if verified != count {
        return Err(invalid(
            CHUNK_MAP,
            "not every indexed receipt is in a chunk",
        ));
    }
    Ok(verified)
}

/// `components.json` (`slop_ninja_eval_score_components_v2`) as the
/// coordinator writes it: every field typed, unknown fields refused at every
/// level, so a field the publisher does not know is never published.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComponentsV2 {
    schema: String,
    cohort: u64,
    label: String,
    reward_rule: String,
    cohort_manifest: String,
    mode: String,
    promoted: Option<String>,
    reference_score: String,
    predecessor_score: Option<String>,
    gate_policy: GateThresholds,
    canary_rule: String,
    fpr_rule: String,
    reuse_rule: String,
    /// The frozen gate policy document; checked against the manifest's
    /// `gate_policy` digest.
    policy: Option<serde_json::Value>,
    /// Roster positions never executed.
    unexecuted: Vec<u64>,
    entrants: Vec<EntrantComponents>,
    crash_rule: String,
    artifact_rule: String,
    /// The limits record the manifest binds by digest (`limits`), in
    /// components written after it was added; checked against the manifest.
    #[serde(default)]
    limits: Option<serde_json::Value>,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct GateThresholds {
    operating_ppb: u32,
    loose_fpr_max_permille: u32,
    strict_fpr_max_permille: u32,
    share_mae_max_ppb: u32,
    attribution_min_permille: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntrantComponents {
    hotkey: String,
    uid: u16,
    score: Option<String>,
    gates: Vec<String>,
    cluster_head: Option<String>,
    units: u16,
}

/// The domain the coordinator hashes the gate policy document under; the
/// manifest binds the digest.
pub const GATE_POLICY_DOMAIN: &str = "slop_ninja.eval/gate-policy/v2";

/// The domain the coordinator hashes the v2 limits record under (a cohort
/// whose plan names an artifact spec: the spec its scorer loads name, by id
/// and frozen digest); the manifest binds the digest. The v1 record (no spec)
/// is not published.
pub const LIMITS_DOMAIN_V2: &str = "slop_ninja.eval/limits/v2";

/// A short version identifier: `[a-z0-9_.-]`, at most 64 bytes.
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
        })
}

/// A rule text the coordinator writes from a constant: printable ASCII, no
/// longer than any public string.
fn rule_text(s: &str) -> bool {
    s.len() <= crate::redact::MAX_PUBLIC_STRING && s.bytes().all(|b| (0x20..0x7f).contains(&b))
}

/// Why `c` may not be published, or `None` when it agrees with the signed
/// records field for field.
fn components_disagreement(
    c: &ComponentsV2,
    label: &str,
    manifest: &CohortManifestV2,
    manifest_digest: &str,
    package: &ScorePackageV2,
) -> Option<&'static str> {
    let hex32 = |d: &Digest| d.hex();
    if c.schema != COMPONENTS_SCHEMA
        || c.cohort != manifest.cohort
        || c.label != label
        || c.reward_rule != "reward-v1"
        || c.cohort_manifest != manifest_digest
    {
        return Some("schema, cohort, label or manifest differs");
    }
    if c.mode != format!("{:?}", package.mode)
        || c.promoted != package.promoted.map(|h| hex::encode(h.0))
        || c.reference_score != package.reference.score.get().to_string()
        || c.predecessor_score
            != package
                .predecessor
                .as_ref()
                .map(|p| p.score.get().to_string())
    {
        return Some("mode, promotion or reference scores differ from the package");
    }
    if ![&c.canary_rule, &c.fpr_rule, &c.reuse_rule]
        .iter()
        .all(|r| identifier(r))
        || !rule_text(&c.crash_rule)
        || !rule_text(&c.artifact_rule)
    {
        return Some("a rule is not a known identifier or rule text");
    }
    if let Some(policy) = &c.policy {
        let digest = domain_hash(GATE_POLICY_DOMAIN, policy.to_string().as_bytes());
        if digest != manifest.gate_policy {
            return Some("the policy is not the one the manifest binds");
        }
        let gates_agree =
            serde_json::to_value(&c.gate_policy).is_ok_and(|g| policy.get("gates") == Some(&g));
        if !gates_agree {
            return Some("the gate thresholds differ from the bound policy");
        }
    }
    if let Some(limits) = &c.limits {
        // Only the v2 record (a cohort that names an artifact spec) is published.
        if limits.get("schema").and_then(serde_json::Value::as_str)
            != Some("slop_ninja_eval_limits_v2")
        {
            return Some("the limits record has an unknown schema");
        }
        if domain_hash(LIMITS_DOMAIN_V2, limits.to_string().as_bytes()) != manifest.limits {
            return Some("the limits record is not the one the manifest binds");
        }
    }
    let n = package.entrants.len();
    if c.entrants.len() != n
        || c.unexecuted.len() > n
        || !c.unexecuted.windows(2).all(|w| w[0] < w[1])
        || c.unexecuted.iter().any(|&p| {
            usize::try_from(p).map_or(true, |p| p >= n || package.entrants[p].score.is_some())
        })
    {
        return Some("entrant list or unexecuted positions differ from the package");
    }
    for (v, e) in c.entrants.iter().zip(&package.entrants) {
        let gates: Vec<&str> = e.gate_failures.iter().map(|g| g.reason.as_str()).collect();
        if v.hotkey != hex::encode(e.identity.hotkey.0)
            || v.uid != e.identity.uid
            || v.units != e.units
            || v.score != e.score.map(|s| s.get().to_string())
            || v.gates != gates
            || v.cluster_head != e.cluster_head.map(|h| hex32(&Digest(h.0)))
        {
            return Some("an entrant differs from the package");
        }
    }
    None
}

/// `components.json` is published only when it parses as the typed schema
/// (unknown fields refused at every level), agrees with the signed manifest
/// and package field for field, and passes the redaction scan.
async fn check_components<S: Store>(
    r: &Reader<'_, S>,
    label: &str,
    manifest: &CohortManifestV2,
    package: &ScorePackageV2,
) -> Result<Components, Refusal> {
    if !r.exists(COMPONENTS).await? {
        return Ok(Components::Absent);
    }
    let bound = arena::components_bound(package.entrants.len() as u64);
    let (raw, _hold) = match r.read(COMPONENTS, bound).await {
        Ok(x) => x,
        Err(e @ (Refusal::Invalid(..) | Refusal::Budget(..))) => {
            return Ok(Components::Withheld(e.to_string()));
        }
        Err(e) => return Err(e),
    };
    let typed: ComponentsV2 = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(_) => {
            return Ok(Components::Withheld(
                "not the known schema (unknown or missing fields)".into(),
            ));
        }
    };
    let manifest_digest = manifest.digest().map_err(|e| invalid(MANIFEST, e))?.hex();
    if let Some(why) = components_disagreement(&typed, label, manifest, &manifest_digest, package) {
        return Ok(Components::Withheld(why.into()));
    }
    // The bound policy carries only identifiers and thresholds; scan it all.
    if let Err(why) = crate::redact::scan_json(&raw) {
        return Ok(Components::Withheld(format!("redaction: {why}")));
    }
    Ok(Components::Published(PublicObject::of(COMPONENTS, &raw)))
}
