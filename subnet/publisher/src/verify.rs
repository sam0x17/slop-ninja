//! Verifies one closed `reward-v2` cohort from the objects the coordinator
//! stores for its publisher, before anything is published.
//!
//! The coordinator's score package (exact scores and every hidden
//! measurement) and its components never leave its storage, and this module
//! never reads them. A cohort is verified from three signed records only,
//! each authenticated under the pinned trust roots before it is decoded
//! ([`Signed::open`]):
//!
//! - the manifest, which must be this chain's and name this cohort's label;
//! - the public [`QuantizedResult`] (`result.signed`), read within its
//!   protocol bound ([`QuantizedResult::max_signed_len`]) and validated as it
//!   opens, so its units, bands, cutoff and bit budget are recomputed from
//!   its own `q` values, bits and membership; it must be the result the
//!   manifest frames ([`QuantizedResult::check_manifest`]);
//! - the closure, which must name that result and match its roster and
//!   openings ([`CohortClosure::check_result`]) and the manifest's frozen
//!   schedule ([`CohortClosure::check_schedule`]).
//!
//! Receipts, receipt indexes, chunk maps, published-content and fingerprint
//! indexes, closure fingerprints, targets and components are
//! coordinator-private, and none is read here. Binary records decode
//! canonically and completely, so a verified record carries nothing beyond
//! its schema. [`RawIndex`] (an authenticated receipt index walked in place)
//! remains for the operator's private tools.

use slop_ninja_protocol::submission::CohortClosure;
use slop_ninja_protocol::{
    AttemptReceipt, Canonical, ChainBinding, CohortManifestV2, Decoder, Digest, MAX_SEQUENCE_LEN,
    QuantizedResult, ReceiptIndex, ReceiptIndexEntry, Record, Signed, TrustedKeys,
};

use crate::arena::{self, Budget, Hold, Refused};
use crate::layout;
use crate::store::{Store, StoreError};

pub const MANIFEST: &str = "manifest.signed";
/// The coordinator's private score package: never read by the publisher,
/// named only so its existence marks a packaged cohort's private directory
/// in tests and documentation.
pub const PACKAGE: &str = "package.signed";
/// The public result of a scored cohort.
pub const RESULT: &str = "result.signed";
pub const CLOSURE: &str = "closure.signed";
/// The coordinator's private diagnostics: never read by the publisher.
pub const COMPONENTS: &str = "components.json";

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

/// A verified cohort: its manifest, its public result and its closure.
#[derive(Clone, Debug)]
pub struct VerifiedCohort {
    pub label: String,
    pub manifest: CohortManifestV2,
    pub result: QuantizedResult,
    pub closure: CohortClosure,
}

pub struct Options<'a> {
    pub keys: &'a TrustedKeys,
    pub chain: &'a ChainBinding,
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
    /// Not a `reward-v2` cohort manifest at all (a load or older cohort):
    /// the envelope's record kind or version says so.
    OtherKind,
    /// Its label differs from its directory's.
    OtherLabel,
}

async fn read_manifest<S: Store>(
    r: &Reader<'_, S>,
    label: &str,
    chain: &ChainBinding,
) -> Result<ManifestCheck, Refusal> {
    let (raw, _hold) = r.read(MANIFEST, arena::READBACK_BUDGET).await?;
    let check = match Signed::<CohortManifestV2>::open(&raw, r.keys) {
        Err(_) if !manifest_kind(&raw) => ManifestCheck::OtherKind,
        Err(e) => ManifestCheck::Unverifiable(e.to_string()),
        Ok(m) if m.chain != *chain => ManifestCheck::OtherChain,
        Ok(m) if m.cohort_label != label => ManifestCheck::OtherLabel,
        Ok(m) => ManifestCheck::Opened(Box::new(m)),
    };
    Ok(check)
}

/// Whether a signed envelope's record is a `reward-v2` cohort manifest by its
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
    read_manifest(&r, label, opts.chain).await
}

/// Verifies the cohort stored under `dir` (`cohorts/<label>` in the private
/// bucket) from its manifest, public result and closure.
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
    // Each raw object is held only while it is opened.
    let manifest = match read_manifest(&r, label, opts.chain).await? {
        ManifestCheck::Opened(m) => *m,
        ManifestCheck::Unverifiable(why) => return Err(invalid(MANIFEST, why)),
        ManifestCheck::OtherChain => return Err(invalid(MANIFEST, "another chain or subnet")),
        ManifestCheck::OtherKind => {
            return Err(invalid(MANIFEST, "not a reward-v2 cohort manifest"));
        }
        ManifestCheck::OtherLabel => {
            return Err(invalid(MANIFEST, "label differs from its directory"));
        }
    };
    let result = {
        let bound = QuantizedResult::max_signed_len().map_err(|e| invalid(RESULT, e))?;
        let (raw, _hold) = r.read(RESULT, bound).await?;
        Signed::<QuantizedResult>::open(&raw, keys).map_err(|e| invalid(RESULT, e))?
    };
    result
        .check_manifest(&manifest)
        .map_err(|e| invalid(RESULT, e))?;
    let closure = {
        let (raw, _hold) = r.read(CLOSURE, arena::READBACK_BUDGET).await?;
        Signed::<CohortClosure>::open(&raw, keys).map_err(|e| invalid(CLOSURE, e))?
    };
    closure
        .check_result(&result)
        .map_err(|e| invalid(CLOSURE, e))?;
    closure
        .check_schedule(&manifest)
        .map_err(|e| invalid(CLOSURE, e))?;
    Ok(VerifiedCohort {
        label: label.to_owned(),
        manifest,
        result,
        closure,
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
