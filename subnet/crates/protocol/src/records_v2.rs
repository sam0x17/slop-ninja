//! Version-2 and later records: the `reward-v1` comparison of whitepaper 0.56,
//! with copy policy B + D (no credit for copies of published artifacts, and a
//! near-copy relation that ignores rank-preserving recalibration).
//!
//! These records sit beside the version-1 `testnet-reward-v0` family. Each
//! `reward-v1` record requires [`RewardRule::RewardV1`], and each version-1
//! record requires `testnet-reward-v0`, so neither calculation can read the
//! other's inputs. Encoding and decoding validate structure; [`crate::reward_v1`]
//! recomputes the reward outcome itself.

use std::collections::{BTreeMap, BTreeSet};

use crate::bigint::{BigUint, Fraction};
use crate::codec::{Canonical, Decoder, Encoder, canonical_struct};
use crate::records::{bounded, strictly_increasing, validate_label};
use crate::sign::KeyRole;
use crate::{
    AccountId, ChainBinding, Digest, Error, FINNEY_GENESIS, NeuronIdentity, ReceiptIndex, Record,
    Result, RewardRule, RowEntry, SCORE_SCALE, Score,
};

/// Hard bound on a cohort roster. The chain's UID space holds at most 256
/// registrations per subnet; this bound also keeps exact credit arithmetic small.
pub const MAX_ROSTER: u16 = 256;
/// Hard bound on batch strata (domain x origin cells).
/// Strata are distinct (domain, origin) keys, so at most three per domain;
/// their count is bounded only by the canonical sequence encoding.
pub const MAX_STRATA: usize = crate::codec::MAX_SEQUENCE_LEN as usize;
/// Longest gate reason code or stratum domain label.
pub const MAX_CODE_LEN: usize = 64;
const MAX_CONTRACT_LEN: usize = 128;
/// Smallest mainnet batch: the whitepaper's 2,000-case floor.
pub const MAINNET_MIN_CASES: u32 = 2000;
/// The launch origin mix (decision of 2026-10-30): 20% human-only, 55% mixed,
/// 25% model-only, self-weighting. The mix is a manifest parameter: each
/// manifest freezes its own, and [`OriginMix::validate`] only requires whole
/// percentages that sum to 100 with every origin present.
pub const LAUNCH_ORIGIN_MIX: OriginMix = OriginMix {
    human_only: 20,
    mixed: 55,
    model_only: 25,
};

macro_rules! record {
    ($name:ident, $kind:literal, $version:literal, $signer:expr, |$s:ident| $validate:block) => {
        impl Record for $name {
            const KIND: &'static str = $kind;
            const VERSION: u16 = $version;
            const SIGNER: Option<KeyRole> = $signer;
            fn validate(&self) -> Result<()> {
                let $s = self;
                $validate
            }
        }
    };
}

/// Defines a field-less enum with stable one-byte tags.
macro_rules! tagged_enum {
    ($(#[$meta:meta])* pub enum $name:ident ($field:literal) { $($(#[$vmeta:meta])* $variant:ident = $tag:literal),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name { $($(#[$vmeta])* $variant),* }
        impl $name {
            pub fn tag(self) -> u8 {
                match self { $(Self::$variant => $tag),* }
            }
        }
        impl $crate::codec::Canonical for $name {
            fn encode_to(&self, e: &mut $crate::codec::Encoder) {
                e.u8(self.tag());
            }
            fn decode_from(d: &mut $crate::codec::Decoder<'_>) -> $crate::Result<Self> {
                match d.u8()? {
                    $($tag => Ok(Self::$variant),)*
                    tag => Err($crate::Error::InvalidTag { field: $field, tag }),
                }
            }
        }
    };
}
pub(crate) use tagged_enum;

/// Reason codes and domain labels: 1 to 64 bytes of `[a-z0-9_.-]`.
pub(crate) fn validate_code(code: &str, field: &'static str) -> Result<()> {
    if code.is_empty()
        || code.len() > MAX_CODE_LEN
        || !code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
    {
        return Err(Error::Invalid(field));
    }
    Ok(())
}

pub(crate) fn validate_contract(contract: &str) -> Result<()> {
    if contract.is_empty() || contract.len() > MAX_CONTRACT_LEN {
        return Err(Error::Invalid("contract"));
    }
    Ok(())
}

/// A secret 256-bit value, such as a submission commitment nonce.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Nonce(pub [u8; 32]);

impl core::fmt::Debug for Nonce {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&crate::hash::hex(&self.0))
    }
}

impl Canonical for Nonce {
    fn encode_to(&self, e: &mut Encoder) {
        e.raw(&self.0);
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(d.bytes32()?))
    }
}

// ---------------------------------------------------------------------------
// Cohort schedule, batch design and parameters
// ---------------------------------------------------------------------------

canonical_struct! {
    /// Cohort length and phase minimums, in subnet tempos counted as finalized
    /// increments of the chain's epoch index. Generation, scoring and
    /// publication advance on completion and have no minimum.
    #[derive(Copy)]
    pub struct CohortSchedule {
        /// `K`: the cohort cannot end before this many tempos (20 at launch,
        /// 4 for rehearsals).
        pub tempos: u16,
        /// The admission phase lasts at least this many tempos (at least 1).
        pub admission_min: u16,
        /// The transition lasts at least this many tempos (at least 2).
        pub transition_min: u16,
        /// Distance of the reward-intent authorization's non-binding renew-by
        /// hint from its issue: at least two cohort lengths, `2K` tempos. The
        /// field keeps its name for the frozen manifest format; no
        /// authorization expires (`authorization` module docs).
        pub lease_tempos: u32,
    }
}

impl CohortSchedule {
    pub fn validate(&self) -> Result<()> {
        if self.admission_min < 1 {
            return Err(Error::Invalid("admission minimum"));
        }
        if self.transition_min < 2 {
            return Err(Error::Invalid("transition minimum"));
        }
        if u32::from(self.admission_min) + u32::from(self.transition_min) > u32::from(self.tempos) {
            return Err(Error::Invalid("phase minimums exceed cohort length"));
        }
        if self.lease_tempos < 2 * u32::from(self.tempos) {
            return Err(Error::Invalid("renew-by hint below two cohorts"));
        }
        Ok(())
    }
}

tagged_enum! {
    /// Recorded production history of a batch document.
    pub enum Origin ("origin") {
        HumanOnly = 1,
        Mixed = 2,
        ModelOnly = 3,
    }
}

canonical_struct! {
    /// Origin shares of the batch in whole percent.
    #[derive(Copy)]
    pub struct OriginMix {
        pub human_only: u8,
        pub mixed: u8,
        pub model_only: u8,
    }
}

impl OriginMix {
    /// Whole percentages summing to 100, every origin present.
    pub fn validate(&self) -> Result<()> {
        let parts = [self.human_only, self.mixed, self.model_only];
        if parts.contains(&0) || parts.iter().map(|&p| u16::from(p)).sum::<u16>() != 100 {
            return Err(Error::Invalid("origin mix"));
        }
        Ok(())
    }

    pub fn percent(&self, origin: Origin) -> u8 {
        match origin {
            Origin::HumanOnly => self.human_only,
            Origin::Mixed => self.mixed,
            Origin::ModelOnly => self.model_only,
        }
    }
}

canonical_struct! {
    /// One domain x origin cell of the self-weighting batch.
    pub struct Stratum {
        pub domain: String,
        pub origin: Origin,
        pub cases: u32,
    }
}

canonical_struct! {
    /// The cohort batch design. Whole source groups are sampled and every
    /// document carries equal weight. Documents and labels stay private; the
    /// sealed batch is bound by digest.
    pub struct BatchDesign {
        /// `n`: documents every entrant and both references score.
        pub cases: u32,
        pub source_groups: u32,
        pub mix: OriginMix,
        /// Sorted by `(domain, origin)`.
        pub strata: Vec<Stratum>,
        pub batch: Digest,
    }
}

impl BatchDesign {
    pub fn validate(&self, chain: &ChainBinding) -> Result<()> {
        if self.cases == 0 {
            return Err(Error::Invalid("batch cases"));
        }
        if chain.genesis == FINNEY_GENESIS && self.cases < MAINNET_MIN_CASES {
            return Err(Error::Invalid("batch below mainnet floor"));
        }
        if self.source_groups == 0 || self.source_groups > self.cases {
            return Err(Error::Invalid("source groups"));
        }
        self.mix.validate()?;
        if self.strata.is_empty() || self.strata.len() > MAX_STRATA {
            return Err(Error::LengthLimit {
                field: "strata",
                len: self.strata.len() as u64,
            });
        }
        let keys: Vec<_> = self
            .strata
            .iter()
            .map(|s| (s.domain.as_str(), s.origin))
            .collect();
        strictly_increasing(&keys, "strata")?;
        let mut by_origin = [0u64; 3];
        for s in &self.strata {
            validate_code(&s.domain, "stratum domain")?;
            if s.cases == 0 {
                return Err(Error::Invalid("empty stratum"));
            }
            by_origin[usize::from(s.origin.tag() - 1)] += u64::from(s.cases);
        }
        if by_origin.iter().sum::<u64>() != u64::from(self.cases) {
            return Err(Error::Mismatch("strata cases"));
        }
        for origin in [Origin::HumanOnly, Origin::Mixed, Origin::ModelOnly] {
            let total = by_origin[usize::from(origin.tag() - 1)];
            if total * 100 != u64::from(self.cases) * u64::from(self.mix.percent(origin)) {
                return Err(Error::Mismatch("origin mix cases"));
            }
        }
        Ok(())
    }
}

/// `delta` floor: 0.01 of headroom, scaled by [`SCORE_SCALE`].
pub const MARGIN_FLOOR: u64 = SCORE_SCALE / 100;
/// `delta = max(0.01, 2 sigma)`.
pub const MARGIN_SIGMAS: u8 = 2;
/// `w = 2 sigma (1 - S_Rk)`.
pub const BAND_SIGMAS: u8 = 2;
/// `epsilon = w / 250`.
pub const NEAR_EQUAL_DIVISOR: u16 = 250;
/// `r = 0.85`, across and within bands, in basis points.
pub const DECAY_BPS: u16 = 8500;
/// The fallback cutoff removes the bottom 15% of the would-be scoring range.
pub const FALLBACK_BPS: u16 = 1500;
/// Near-copy distance `d`: after the best rank-preserving recalibration fitted
/// in either direction, involvement probabilities differ by RMS at most 0.025,
/// in parts per billion.
pub const NEAR_COPY_PPB: u32 = 25_000_000;
/// The near-copy score clause: a model within distance `d` of another is a
/// near copy unless it scores more than `w / 2` above it.
pub const IMPROVEMENT_DIVISOR: u8 = 2;
/// Largest fingerprint probe, in documents.
pub const MAX_PROBE_SIZE: u32 = 1 << 16;
/// Units apportioned by largest remainder.
pub const ROW_TOTAL: u16 = u16::MAX;

canonical_struct! {
    /// Every `reward-v1` parameter, frozen and signed in the manifest. `sigma`
    /// is measured per manifest; the others are the rule's frozen constants
    /// and must equal them, so the parameter rule cannot drift silently.
    #[derive(Copy)]
    pub struct RewardV1Params {
        /// Pooled paired standard error of `q` at the manifest's batch size,
        /// in headroom units scaled by [`SCORE_SCALE`].
        pub sigma: u64,
        pub margin_floor: u64,
        pub margin_sigmas: u8,
        pub band_sigmas: u8,
        pub near_equal_divisor: u16,
        pub decay_bps: u16,
        pub fallback_bps: u16,
        /// `d` in parts per billion of involvement probability.
        pub near_copy_ppb: u32,
        /// The score clause's `w / improvement_divisor`.
        pub improvement_divisor: u8,
        pub row_total: u16,
    }
}

impl RewardV1Params {
    /// The frozen rule with a measured `sigma`.
    pub fn new(sigma: u64) -> Self {
        Self {
            sigma,
            margin_floor: MARGIN_FLOOR,
            margin_sigmas: MARGIN_SIGMAS,
            band_sigmas: BAND_SIGMAS,
            near_equal_divisor: NEAR_EQUAL_DIVISOR,
            decay_bps: DECAY_BPS,
            fallback_bps: FALLBACK_BPS,
            near_copy_ppb: NEAR_COPY_PPB,
            improvement_divisor: IMPROVEMENT_DIVISOR,
            row_total: ROW_TOTAL,
        }
    }

    /// `sigma` is measured for each manifest. The near-copy distance `d` is a
    /// manifest parameter that a later manifest may lower before independent
    /// models approach it, never raise: `0 < d <= NEAR_COPY_PPB`, the frozen
    /// launch value. Every other value is fixed by the rule itself and must
    /// equal its constant: the margin floor and multiple (decision: `delta =
    /// max(0.01, 2 sigma)`), the band multiple, the subnet 78 near-equal
    /// ratio `w / 250` and decay 0.85, the 15% fallback fraction, the score
    /// clause divisor and the 65,535 row total. Changing any of those is a new
    /// reward rule, not a manifest change.
    pub fn validate(&self) -> Result<()> {
        if self.sigma == 0 || self.sigma > SCORE_SCALE {
            return Err(Error::OutOfRange("sigma"));
        }
        if self.near_copy_ppb == 0 || self.near_copy_ppb > NEAR_COPY_PPB {
            return Err(Error::OutOfRange("near_copy_ppb"));
        }
        let fixed = Self {
            near_copy_ppb: self.near_copy_ppb,
            ..Self::new(self.sigma)
        };
        if *self != fixed {
            return Err(Error::Mismatch("reward_v1 parameters"));
        }
        Ok(())
    }

    /// `delta = max(0.01, 2 sigma)`, scaled by [`SCORE_SCALE`]. It may exceed 1.
    pub fn margin(&self) -> u128 {
        u128::from(self.margin_floor).max(u128::from(self.margin_sigmas) * u128::from(self.sigma))
    }
}

canonical_struct! {
    /// A public reference model.
    #[derive(Copy)]
    pub struct ReferenceModel {
        /// Canonical inference-content digest.
        pub content: Digest,
        /// Digest of the hosted artifact object.
        pub artifact: Digest,
    }
}

canonical_struct! {
    /// The references `R_k` and `R_{k-1}` in force for a cohort.
    #[derive(Copy)]
    pub struct ReferenceSet {
        /// `k`. `R_0` is the released baseline and has no predecessor.
        pub index: u64,
        pub current: ReferenceModel,
        pub predecessor: Option<ReferenceModel>,
    }
}

impl ReferenceModel {
    /// Checks a hosted reference artifact: the object is the one named, and
    /// its derived inference content is the reference's.
    pub fn check_artifact(&self, artifact: &crate::VerifiedArtifact) -> Result<()> {
        if artifact.sha256 != self.artifact {
            return Err(Error::Mismatch("reference artifact"));
        }
        if artifact.content != self.content {
            return Err(Error::Mismatch("reference artifact content"));
        }
        Ok(())
    }
}

impl ReferenceSet {
    pub fn validate(&self) -> Result<()> {
        if (self.index == 0) != self.predecessor.is_none() {
            return Err(Error::Invalid("reference predecessor"));
        }
        Ok(())
    }

    /// The operator's one permitted change outside promotion: while `R_0` is
    /// the reference, a new manifest may replace it with Slop Ninja's own
    /// candidate, which takes `R_0`'s position with no predecessor.
    pub fn replace_r0(&self, candidate: ReferenceModel) -> Result<Self> {
        if self.index != 0 {
            return Err(Error::Invalid("only R_0 can be replaced"));
        }
        Ok(Self {
            index: 0,
            current: candidate,
            predecessor: None,
        })
    }
}

canonical_struct! {
    /// One admitted roster entry: a finalized on-chain digest commitment with
    /// a matching private upload, as [`crate::submission::SubmissionUpload::admit`]
    /// returns it.
    pub struct RosterEntry {
        /// Credited identity: the committing hotkey's registration, with the
        /// UID it held at the commitment block.
        pub identity: NeuronIdentity,
        /// The on-chain submission commitment digest.
        pub commitment: Digest,
        pub commitment_block: u64,
        pub extrinsic_index: u32,
    }
}

impl RosterEntry {
    /// Admission order: finalized commitment block, then extrinsic index.
    pub fn position(&self) -> (u64, u32) {
        (self.commitment_block, self.extrinsic_index)
    }
}

/// Roster-shaped sequences are in admission order, with unique positions and
/// one entry per hotkey.
fn validate_admission_order<'a>(
    entries: impl Iterator<Item = ((u64, u32), &'a AccountId)>,
    field: &'static str,
) -> Result<()> {
    let mut hotkeys = BTreeSet::new();
    let mut last: Option<(u64, u32)> = None;
    for (position, hotkey) in entries {
        if let Some(prev) = last {
            match prev.cmp(&position) {
                core::cmp::Ordering::Less => {}
                core::cmp::Ordering::Equal => return Err(Error::Duplicate(field)),
                core::cmp::Ordering::Greater => return Err(Error::Unsorted(field)),
            }
        }
        last = Some(position);
        if !hotkeys.insert(*hotkey) {
            return Err(Error::Duplicate("roster hotkey"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Published artifacts and the fingerprint probe
// ---------------------------------------------------------------------------

canonical_struct! {
    /// Binds the published-artifact index a cohort is checked against: the
    /// digest and size of the sorted set of every published inference-content
    /// digest, and of the sorted fingerprint index.
    #[derive(Copy, Default)]
    pub struct PublishedIndexBinding {
        /// Digest of the [`PublishedContents`] record.
        pub content_root: Digest,
        pub content_count: u64,
        /// Digest of the [`FingerprintIndex`] record.
        pub fingerprint_root: Digest,
        pub fingerprint_count: u64,
    }
}

canonical_struct! {
    /// One fingerprint probe version in use. The probe documents, labels and
    /// every fingerprint stay private; the digest covers the canonical private
    /// probe record, including a fresh 256-bit nonce, so it reveals nothing
    /// about membership.
    #[derive(Copy, PartialOrd, Ord)]
    pub struct ProbeBinding {
        pub version: u32,
        /// `P`, the probe's documents.
        pub size: u32,
        pub digest: Digest,
    }
}

/// Probe versions in use: at least one, in strictly increasing version order,
/// each with 1 to [`MAX_PROBE_SIZE`] documents.
fn validate_probes(probes: &[ProbeBinding]) -> Result<()> {
    if probes.is_empty() {
        return Err(Error::Invalid("no probe version in use"));
    }
    let versions: Vec<u32> = probes.iter().map(|p| p.version).collect();
    strictly_increasing(&versions, "probe versions")?;
    if probes
        .iter()
        .any(|p| p.size == 0 || p.size > MAX_PROBE_SIZE)
    {
        return Err(Error::OutOfRange("probe size"));
    }
    Ok(())
}

canonical_struct! {
    /// The inference-content digests of every artifact ever published: every
    /// entrant of every closed cohort and every `R_0` the operator has
    /// published, sorted. Its digest is the content root. Validators rebuild it
    /// from the packages they retain.
    #[derive(Default)]
    pub struct PublishedContents {
        pub contents: Vec<Digest>,
    }
}

record!(PublishedContents, "published-contents", 1, None, |c| {
    strictly_increasing(&c.contents, "published contents")
});

canonical_struct! {
    /// One archived artifact's fingerprint on one probe version: the digest of
    /// its private probe predictions.
    #[derive(Copy, PartialOrd, Ord)]
    pub struct Fingerprint {
        pub content: Digest,
        pub version: u32,
        pub fingerprint: Digest,
    }
}

canonical_struct! {
    /// One executed entrant's probe predictions on one probe version, as its
    /// cohort's closure records them: the digest of the entrant's private
    /// probe predictions. [`PublishedIndex::after_closure`] turns these into
    /// one canonical [`Fingerprint`] per content and version
    /// ([`canonical_fingerprints`]).
    #[derive(Copy, PartialOrd, Ord)]
    pub struct EntrantFingerprint {
        pub hotkey: AccountId,
        pub version: u32,
        pub fingerprint: Digest,
    }
}

/// The canonical fingerprints a closed `package` adds to the index, from the
/// per-entrant fingerprints its closure records.
///
/// `fingerprints` must hold exactly one fingerprint of each scored entrant on
/// each probe version the package binds, gate-failed and low-scoring entrants
/// included, and none of an entrant that was never executed. Entrants with
/// identical inference content can return different probe predictions, for
/// example when one fails on a probe document. The canonical fingerprint of a
/// `(content, probe version)` is then the earliest-committed entrant's, by
/// finalized commitment block, then extrinsic index, then hotkey bytes, so
/// every validator that rebuilds the index records the same one. The others
/// are not recorded.
pub fn canonical_fingerprints(
    package: &ScorePackageV2,
    fingerprints: &[EntrantFingerprint],
) -> Result<Vec<Fingerprint>> {
    let mut given = BTreeMap::<(AccountId, u32), Digest>::new();
    for f in fingerprints {
        if given.insert((f.hotkey, f.version), f.fingerprint).is_some() {
            return Err(Error::Mismatch("closure fingerprints"));
        }
    }
    let mut scored: Vec<&EntrantScore> = package
        .entrants
        .iter()
        .filter(|e| e.score.is_some())
        .collect();
    scored.sort_by_key(|e| (e.commitment_block, e.extrinsic_index, e.identity.hotkey));
    let expected: BTreeSet<(AccountId, u32)> = scored
        .iter()
        .flat_map(|e| {
            package
                .probes
                .iter()
                .map(|p| (e.identity.hotkey, p.version))
        })
        .collect();
    if !given.keys().copied().eq(expected.iter().copied()) {
        return Err(Error::Mismatch("closure fingerprints"));
    }
    let mut canonical = BTreeMap::<(Digest, u32), Digest>::new();
    for entrant in scored {
        for probe in &package.probes {
            canonical
                .entry((entrant.content, probe.version))
                .or_insert(given[&(entrant.identity.hotkey, probe.version)]);
        }
    }
    Ok(canonical
        .into_iter()
        .map(|((content, version), fingerprint)| Fingerprint {
            content,
            version,
            fingerprint,
        })
        .collect())
}

canonical_struct! {
    /// Every recorded fingerprint, sorted by content, version and fingerprint
    /// digest, with one fingerprint per content and version. Its digest is the
    /// fingerprint root; the fingerprints themselves stay private.
    #[derive(Default)]
    pub struct FingerprintIndex {
        pub entries: Vec<Fingerprint>,
    }
}

record!(FingerprintIndex, "fingerprint-index", 1, None, |i| {
    strictly_increasing(&i.entries, "fingerprints")?;
    if i.entries
        .windows(2)
        .any(|w| (w[0].content, w[0].version) == (w[1].content, w[1].version))
    {
        return Err(Error::Duplicate("fingerprint content and version"));
    }
    Ok(())
});

/// The published-artifact index a validator rebuilds before checking a
/// package: the published content set and the fingerprint index.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PublishedIndex {
    pub contents: PublishedContents,
    pub fingerprints: FingerprintIndex,
}

impl PublishedIndex {
    /// Builds an index from unsorted parts; repeated content digests and
    /// repeated identical fingerprints merge, while two different
    /// fingerprints of one content on one version are refused.
    pub fn new(
        contents: impl IntoIterator<Item = Digest>,
        fingerprints: impl IntoIterator<Item = Fingerprint>,
    ) -> Result<Self> {
        let mut contents: Vec<Digest> = contents.into_iter().collect();
        contents.sort_unstable();
        contents.dedup();
        let mut entries: Vec<Fingerprint> = fingerprints.into_iter().collect();
        entries.sort_unstable();
        entries.dedup();
        let index = Self {
            contents: PublishedContents { contents },
            fingerprints: FingerprintIndex { entries },
        };
        index.validate()?;
        Ok(index)
    }

    /// Both records are canonical and every fingerprinted artifact is
    /// published.
    pub fn validate(&self) -> Result<()> {
        self.contents.validate()?;
        self.fingerprints.validate()?;
        if self
            .fingerprints
            .entries
            .iter()
            .any(|f| !self.contains(&f.content))
        {
            return Err(Error::Mismatch("fingerprint of an unpublished artifact"));
        }
        Ok(())
    }

    pub fn binding(&self) -> Result<PublishedIndexBinding> {
        Ok(PublishedIndexBinding {
            content_root: self.contents.digest()?,
            content_count: self.contents.contents.len() as u64,
            fingerprint_root: self.fingerprints.digest()?,
            fingerprint_count: self.fingerprints.entries.len() as u64,
        })
    }

    /// Whether an artifact with this inference content has been published.
    pub fn contains(&self, content: &Digest) -> bool {
        self.contents.contents.binary_search(content).is_ok()
    }

    /// Whether the index records a fingerprint of `content` on probe `version`.
    pub fn fingerprinted(&self, content: &Digest, version: u32) -> bool {
        self.fingerprints
            .entries
            .iter()
            .any(|f| f.content == *content && f.version == version)
    }

    /// The index after a cohort closes: every entrant's content joins the
    /// published set, and every executed entrant's probe predictions join the
    /// fingerprint index, gate-failed entrants included. `fingerprints` are
    /// the closure's per-entrant fingerprints: exactly one of each scored
    /// entrant on each probe version the package binds, and none of an
    /// entrant that was never executed. Entrants with identical content
    /// record one fingerprint per version, the earliest-committed entrant's
    /// ([`canonical_fingerprints`]). A validator that rebuilds the next index
    /// this way therefore detects an executed artifact left unfingerprinted,
    /// and every validator records the same fingerprint for each content.
    pub fn after_closure(
        &self,
        package: &ScorePackageV2,
        fingerprints: impl IntoIterator<Item = EntrantFingerprint>,
    ) -> Result<Self> {
        let fingerprints: Vec<EntrantFingerprint> = fingerprints.into_iter().collect();
        let canonical = canonical_fingerprints(package, &fingerprints)?;
        Self::new(
            self.contents
                .contents
                .iter()
                .copied()
                .chain(package.entrants.iter().map(|e| e.content)),
            self.fingerprints.entries.iter().copied().chain(canonical),
        )
    }
}

canonical_struct! {
    /// Frozen `reward-v1` comparison for one cohort, sealed with its roster
    /// and batch at the end of generation.
    pub struct CohortManifestV2 {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub cohort_label: String,
        /// Epoch index of the cohort's opening tempo boundary.
        pub opened_epoch: u64,
        pub reward_rule: RewardRule,
        pub schedule: CohortSchedule,
        pub batch: BatchDesign,
        pub params: RewardV1Params,
        pub references: ReferenceSet,
        /// The published-artifact index as of this cohort's seal.
        pub published: PublishedIndexBinding,
        /// The fingerprint probe versions in use, in version order.
        pub probes: Vec<ProbeBinding>,
        /// Raw-text input profile and tokenizer contract.
        pub input_profile: Digest,
        /// Frozen output contract identifier.
        pub output_contract: String,
        pub reference_gallery: Digest,
        /// Qualification, share, attribution and span gate thresholds.
        pub gate_policy: Digest,
        /// Whether the span gate is enforced in this cohort.
        pub span_gate: bool,
        /// Per-artifact size and compute limits and numerical tolerances.
        pub limits: Digest,
        /// Attempts per assignment; the earliest non-infrastructure outcome is final.
        pub max_attempts: u8,
        pub roster_cap: u16,
        /// Admission order.
        pub roster: Vec<RosterEntry>,
    }
}

record!(
    CohortManifestV2,
    "cohort-manifest",
    4,
    Some(KeyRole::Package),
    |m| {
        validate_label(&m.cohort_label)?;
        m.reward_rule.check_reward_v1(&m.chain)?;
        m.schedule.validate()?;
        m.batch.validate(&m.chain)?;
        m.params.validate()?;
        m.references.validate()?;
        validate_probes(&m.probes)?;
        validate_contract(&m.output_contract)?;
        if m.max_attempts == 0 {
            return Err(Error::Invalid("max_attempts"));
        }
        if m.roster_cap == 0 || m.roster_cap > MAX_ROSTER {
            return Err(Error::OutOfRange("roster_cap"));
        }
        if m.roster.len() > usize::from(m.roster_cap) {
            return Err(Error::LengthLimit {
                field: "roster",
                len: m.roster.len() as u64,
            });
        }
        validate_admission_order(
            m.roster.iter().map(|r| (r.position(), &r.identity.hotkey)),
            "roster",
        )
    }
);

impl CohortManifestV2 {
    /// Earliest epoch index at which admission may close: the admission
    /// minimum after the opening boundary.
    pub fn earliest_admission_close_epoch(&self) -> Result<u64> {
        self.opened_epoch
            .checked_add(u64::from(self.schedule.admission_min))
            .ok_or(Error::Overflow("admission close epoch"))
    }

    /// Earliest epoch index at which the cohort may close: `K` tempos after
    /// the opening boundary.
    pub fn earliest_close_epoch(&self) -> Result<u64> {
        self.opened_epoch
            .checked_add(u64::from(self.schedule.tempos))
            .ok_or(Error::Overflow("close epoch"))
    }
}

// ---------------------------------------------------------------------------
// Score package
// ---------------------------------------------------------------------------

tagged_enum! {
    /// A qualification gate. Gates decide eligibility and carry no reward weight.
    pub enum Gate ("gate") {
        Qualification = 1,
        Share = 2,
        Attribution = 3,
        Span = 4,
        /// The public-copy gate: the entrant is a copy of a published artifact.
        PublicCopy = 5,
        /// The loose, per-entrant tier of the pure-human false-positive gate,
        /// measured on the entrant's own cohort humans
        /// ([`HUMAN_FPR_LOOSE_REASON`]). The strict tier is a promotion
        /// condition ([`PromotionAudit`]), not a gate.
        HumanFalsePositive = 6,
    }
}

/// Reason code of a loose-tier pure-human false-positive failure.
pub const HUMAN_FPR_LOOSE_REASON: &str = "human_fpr.loose";

canonical_struct! {
    pub struct GateFailure {
        pub gate: Gate,
        /// Stable reason code, `[a-z0-9_.-]`.
        pub reason: String,
    }
}

/// Why an entrant failed the public-copy gate. Each has a fixed reason code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PublicCopy {
    /// `public.content`: its inference content equals that of a published
    /// artifact. Found at admission; the entrant is never executed.
    Content,
    /// `public.near.current`: a near copy of `R_k` on the batch.
    Current,
    /// `public.near.predecessor`: a near copy of `R_k-1` on the batch.
    Predecessor,
    /// `public.near.probe`: a near copy of a fingerprinted published artifact
    /// on the fingerprint probe.
    Probe,
}

impl PublicCopy {
    pub const ALL: [Self; 4] = [Self::Content, Self::Current, Self::Predecessor, Self::Probe];

    pub fn reason(self) -> &'static str {
        match self {
            Self::Content => "public.content",
            Self::Current => "public.near.current",
            Self::Predecessor => "public.near.predecessor",
            Self::Probe => "public.near.probe",
        }
    }

    pub fn from_reason(reason: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.reason() == reason)
    }

    pub fn failure(self) -> GateFailure {
        GateFailure {
            gate: Gate::PublicCopy,
            reason: self.reason().into(),
        }
    }
}

canonical_struct! {
    /// Exact near-copy statistics of a model `x` against a model `y` over the
    /// `common` documents on which both returned a prediction, in parts per
    /// billion squared (the `10^-18` grid of the Brier utilities).
    /// `rss_fwd = RSS(x | y)` is the residual of `x` after the best
    /// non-decreasing function of `y` (isotonic regression), and
    /// `rss_rev = RSS(y | x)` ([`crate::reward_v1::aligned_rss`]). The aligned
    /// distance is `A_sym = sqrt(min(rss_fwd, rss_rev) / common)`.
    #[derive(Copy)]
    pub struct NearStats {
        pub common: u32,
        pub rss_fwd: u128,
        pub rss_rev: u128,
    }
}

impl NearStats {
    /// `min(RSS(x|y), RSS(y|x))`.
    pub fn min_rss(&self) -> u128 {
        self.rss_fwd.min(self.rss_rev)
    }

    /// `A_sym <= d`: at least one common document and
    /// `min(rss_fwd, rss_rev) <= common * d^2`, without a square root. With no
    /// common document the pair is not near.
    pub fn within(&self, d_ppb: u32) -> Result<bool> {
        if self.common == 0 {
            return Ok(false);
        }
        let d = u128::from(d_ppb);
        let bound = u128::from(self.common)
            .checked_mul(d * d)
            .ok_or(Error::Overflow("near-copy bound"))?;
        Ok(self.min_rss() <= bound)
    }

    /// `floor(A_sym)` in parts per billion; `None` with no common document.
    pub fn distance_ppb(&self) -> Option<u32> {
        if self.common == 0 {
            return None;
        }
        let squared = self.min_rss() / u128::from(self.common);
        u32::try_from(squared.isqrt()).ok()
    }

    /// Whether `A_sym(self) < A_sym(other)`, compared exactly; a pair with no
    /// common document is never closer.
    pub fn closer_than(&self, other: &Self) -> bool {
        match (self.common, other.common) {
            (0, _) => false,
            (_, 0) => true,
            (a, b) => self.min_rss() * u128::from(b) < other.min_rss() * u128::from(a),
        }
    }

    /// `common <= max_common`, and each residual at most `common * 10^18`,
    /// the largest squared difference of two probabilities over `common`
    /// documents.
    fn check_bounds(&self, max_common: u32) -> Result<()> {
        if self.common > max_common {
            return Err(Error::Mismatch("near-copy common documents"));
        }
        let bound = u128::from(self.common) * u128::from(crate::PROBABILITY_SCALE).pow(2);
        if self.rss_fwd > bound || self.rss_rev > bound {
            return Err(Error::OutOfRange("near-copy residual"));
        }
        Ok(())
    }
}

canonical_struct! {
    /// One probe comparison of an entrant with a fingerprinted published
    /// artifact on one probe version. As a [`PublicCheck::probe_match`] it is
    /// the near copy with the smallest aligned distance among every
    /// fingerprint compared, `R_k` and `R_k-1` included, ties broken by
    /// content digest bytes and then version.
    pub struct ProbeMatch {
        pub content: Digest,
        pub version: u32,
        /// Entrant (`x`) against the archived fingerprint (`y`) on the probe.
        pub stats: NearStats,
        /// Both models' scores on the probe, failures at zero; they serve only
        /// the score clause.
        pub entrant_probe_score: Score,
        pub public_probe_score: Score,
    }
}

canonical_struct! {
    /// The public-copy test of one scored entrant: its exact statistics
    /// against `R_k` and `R_k-1` on the batch and against their fingerprints
    /// on every probe version in use, and the matched published artifact
    /// when only the probe found a near copy.
    pub struct PublicCheck {
        pub current: NearStats,
        /// Present exactly when `k >= 1`.
        pub predecessor: Option<NearStats>,
        /// The entrant against `R_k`'s and then `R_k-1`'s fingerprint on each
        /// probe version in use, in version order
        /// ([`ScorePackageV2::reference_probe_keys`]). The probe comparison
        /// always includes both references, and validators check that it
        /// does: none of these may be a near copy unless the entrant fails
        /// the gate.
        pub reference_probes: Vec<ProbeMatch>,
        /// Present exactly when the entrant fails `public.near.probe`.
        pub probe_match: Option<ProbeMatch>,
    }
}

canonical_struct! {
    /// Aggregate distances that show independent models drifting toward `d`,
    /// as `floor(A_sym)` in parts per billion.
    #[derive(Copy, Default)]
    pub struct DistanceMonitor {
        /// The smallest distance between two cluster heads on the batch, a
        /// coordinator measurement; `None` with fewer than two clusters.
        pub min_entrant_pair_ppb: Option<u32>,
        /// The smallest distance to `R_k` among gated entrants, recomputed
        /// from their public-copy statistics; `None` without a gated entrant
        /// with a common document.
        pub min_gated_vs_current_ppb: Option<u32>,
        /// The smallest distance on the probe between a gated entrant and
        /// any fingerprinted published artifact, `R_k` and `R_k-1` included,
        /// on any probe version in use: a coordinator measurement that
        /// validators bound by the published reference comparisons. `None`
        /// without a gated entrant with a common probe document.
        pub min_gated_vs_archive_ppb: Option<u32>,
    }
}

canonical_struct! {
    /// Public score components of one roster entry.
    pub struct EntrantScore {
        pub identity: NeuronIdentity,
        pub commitment: Digest,
        pub commitment_block: u64,
        pub extrinsic_index: u32,
        /// Canonical inference-content digest (see [`InferenceContent`]).
        pub content: Digest,
        /// SHA-256 of the uploaded artifact object, published after the cohort.
        pub artifact: Digest,
        /// Sorted by gate; empty when every enforced gate passed.
        pub gate_failures: Vec<GateFailure>,
        /// Mean binary Brier utility over the batch. `None` only for an entrant
        /// that was never executed: it failed qualification, or its content is
        /// that of a published artifact (`public.content`).
        pub score: Option<Score>,
        pub assigned: u32,
        pub succeeded: u32,
        /// Attributable failures: zero utility, weight retained.
        pub failed: u32,
        /// The public-copy test, present exactly for scored entrants,
        /// gate-failed ones included.
        pub public: Option<PublicCheck>,
        /// Head of the entrant's near-copy cluster, a coordinator measurement.
        /// Set exactly for gated entrants at or above the cutoff.
        pub cluster_head: Option<AccountId>,
        /// The member's statistics against its head, for every member joined
        /// by distance rather than by identical content.
        pub cluster_stats: Option<NearStats>,
        /// Units of the 65,535 pool apportioned to this entrant.
        pub units: u16,
    }
}

impl EntrantScore {
    /// Gated: every enforced gate passed and the entrant was scored.
    pub fn gated(&self) -> bool {
        self.gate_failures.is_empty() && self.score.is_some()
    }

    pub fn position(&self) -> (u64, u32) {
        (self.commitment_block, self.extrinsic_index)
    }

    /// The recorded public-copy failure, if any.
    pub fn public_copy(&self) -> Option<PublicCopy> {
        self.gate_failures
            .iter()
            .find(|g| g.gate == Gate::PublicCopy)
            .and_then(|g| PublicCopy::from_reason(&g.reason))
    }

    fn validate(&self, cases: u32, span_gate: bool) -> Result<()> {
        let gates: Vec<_> = self.gate_failures.iter().map(|g| g.gate).collect();
        strictly_increasing(&gates, "gate failures")?;
        for failure in &self.gate_failures {
            validate_code(&failure.reason, "gate reason")?;
            if failure.gate == Gate::Span && !span_gate {
                return Err(Error::Invalid("span gate not enforced"));
            }
            if failure.gate == Gate::PublicCopy
                && PublicCopy::from_reason(&failure.reason).is_none()
            {
                return Err(Error::Invalid("public-copy reason"));
            }
        }
        let copy = self.public_copy();
        match self.score {
            Some(_) => {
                if copy == Some(PublicCopy::Content) {
                    return Err(Error::Invalid("published content executed"));
                }
                if self.assigned != cases {
                    return Err(Error::Mismatch("entrant assignments"));
                }
                let outcomes = self
                    .succeeded
                    .checked_add(self.failed)
                    .ok_or(Error::Overflow("outcome count"))?;
                if outcomes != self.assigned {
                    return Err(Error::Mismatch("outcome count"));
                }
            }
            None => {
                if !gates.contains(&Gate::Qualification) && copy != Some(PublicCopy::Content) {
                    return Err(Error::Invalid("unscored entrant passed qualification"));
                }
                if (self.assigned, self.succeeded, self.failed) != (0, 0, 0) {
                    return Err(Error::Mismatch("outcome count"));
                }
                if self.public.is_some() {
                    return Err(Error::Invalid("public check of an unscored entrant"));
                }
            }
        }
        match (&self.public, copy) {
            (None, Some(PublicCopy::Current | PublicCopy::Predecessor | PublicCopy::Probe)) => {
                return Err(Error::Invalid("near-copy failure without statistics"));
            }
            (Some(public), _)
                if public.probe_match.is_some() != (copy == Some(PublicCopy::Probe)) =>
            {
                return Err(Error::Invalid("probe match without a probe failure"));
            }
            _ => {}
        }
        if self.units > 0 && self.cluster_head.is_none() {
            return Err(Error::Invalid("units outside a cluster"));
        }
        if self.cluster_head.is_some() && !self.gated() {
            return Err(Error::Invalid("cluster member not gated"));
        }
        if self.cluster_stats.is_some()
            && self
                .cluster_head
                .is_none_or(|head| head == self.identity.hotkey)
        {
            return Err(Error::Invalid("cluster statistics outside a member"));
        }
        Ok(())
    }
}

canonical_struct! {
    /// A reference model's score on the cohort batch. A reference that fails
    /// to return any prediction holds the package for a signed correction, so
    /// a valid record has no failure.
    pub struct ReferenceScore {
        pub model: ReferenceModel,
        pub score: Score,
        pub assigned: u32,
        pub succeeded: u32,
        pub failed: u32,
    }
}

impl ReferenceScore {
    fn validate(&self, cases: u32) -> Result<()> {
        if self.failed != 0 {
            return Err(Error::Invalid("reference failure holds the package"));
        }
        if self.assigned != cases || self.succeeded != cases {
            return Err(Error::Mismatch("reference assignments"));
        }
        Ok(())
    }
}

tagged_enum! {
    /// Which cutoff scored the cohort.
    pub enum CutoffMode ("cutoff_mode") {
        /// Some gated entrant reached `q >= delta`; the cutoff is
        /// `T = S_Rk + delta (1 - S_Rk)` and the top entrant is promoted.
        Margin = 1,
        /// Nobody reached the margin; the cutoff is
        /// `C_N = S_Rk-1 + 0.15 (max_N - S_Rk-1)` and the reference stays.
        Fallback = 2,
        /// The cohort produces no new target; the previous target stays live,
        /// or the launch burn row if none ever existed.
        NoTarget = 3,
    }
}

canonical_struct! {
    /// One retained cluster's exact credit `c = num / den`, in lowest terms as
    /// [`crate::reward_v1::band_credits`] computes it: with `r = p / q` in
    /// lowest terms the numerator is a product of powers of `p` and `q` and
    /// the denominator is coprime to both.
    pub struct ClusterCredit {
        /// The cluster's head.
        pub head: AccountId,
        pub num: BigUint,
        pub den: BigUint,
    }
}

impl ClusterCredit {
    /// The credit as an exact fraction.
    pub fn fraction(&self) -> Fraction {
        Fraction {
            num: self.num.clone(),
            den: self.den.clone(),
        }
    }

    /// `0 < c <= 1`, within the recordable size.
    fn validate(&self) -> Result<()> {
        self.num.check_recordable("credit numerator")?;
        self.den.check_recordable("credit denominator")?;
        if self.num.is_zero() || self.num > self.den {
            return Err(Error::Invalid("cluster credit"));
        }
        Ok(())
    }
}

canonical_struct! {
    /// One canary audit of a promotion candidate: the paired comparison with
    /// a frozen `R_0` on a fresh draw, minus the same comparison on the
    /// anchors. The draw and its labels stay private; `draw` is the digest of
    /// the private draw record.
    pub struct CanaryAudit {
        pub draw: Digest,
        /// The flag's reason code when the audit flagged the candidate
        /// (`canary.statistic`, or `canary.drill` for a testnet drill).
        pub flag: Option<String>,
    }
}

canonical_struct! {
    /// The promotion conditions of one candidate of a margin cohort, beyond
    /// the margin itself: the strict tier of the pure-human false-positive
    /// gate, measured on the first canary draw, and the lookup canary. A
    /// first flag is followed by an immediate re-audit on a fresh,
    /// independent draw; two flags block the candidate. These are coordinator
    /// measurements on hidden predictions, trusted like the scores; the
    /// structure and the promotion they imply are checked.
    pub struct PromotionAudit {
        /// The candidate: a retained cluster head.
        pub head: AccountId,
        pub strict_fpr_passed: bool,
        /// One audit, or two when the first flagged.
        pub canary: Vec<CanaryAudit>,
    }
}

impl PromotionAudit {
    /// One audit without a flag, or a flagged audit and its re-audit.
    pub fn validate(&self) -> Result<()> {
        match self.canary.as_slice() {
            [first] if first.flag.is_none() => {}
            // A re-audit runs on a fresh, independent draw.
            [first, second] if first.flag.is_some() => {
                if second.draw == first.draw {
                    return Err(Error::Invalid("canary re-audit reuses its draw"));
                }
            }
            _ => return Err(Error::Invalid("canary audits")),
        }
        for audit in &self.canary {
            if let Some(reason) = &audit.flag {
                validate_code(reason, "canary reason")?;
            }
        }
        Ok(())
    }

    /// Two flags: the first audit and its re-audit both flagged.
    pub fn blocked(&self) -> bool {
        self.canary.len() == 2 && self.canary.iter().all(|a| a.flag.is_some())
    }

    /// Passes every promotion condition beyond the margin.
    pub fn passes(&self) -> bool {
        self.strict_fpr_passed && !self.blocked()
    }
}

canonical_struct! {
    /// Signed `reward-v1` score package: public score components and the
    /// coordinator's outcome, which every validator recomputes.
    pub struct ScorePackageV2 {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub cohort_label: String,
        pub cohort_manifest: Digest,
        pub revision: u64,
        pub reward_rule: RewardRule,
        pub params: RewardV1Params,
        pub span_gate: bool,
        /// `n`, the batch size.
        pub cases: u32,
        /// `k` of the reference `R_k` in force.
        pub reference_index: u64,
        /// `S_Rk` on this batch.
        pub reference: ReferenceScore,
        /// `S_Rk-1` on this batch; present exactly when `k > 0`.
        pub predecessor: Option<ReferenceScore>,
        /// The manifest's published-artifact index binding.
        pub published: PublishedIndexBinding,
        /// The manifest's probe versions in use.
        pub probes: Vec<ProbeBinding>,
        /// Roster order (admission order).
        pub entrants: Vec<EntrantScore>,
        pub mode: CutoffMode,
        /// `R_k+1`: in a margin cohort, the first retained cluster head in
        /// rank order that passes every promotion condition; `None` when no
        /// candidate passes.
        pub promoted: Option<AccountId>,
        /// The promotion audits of a margin cohort: the retained cluster
        /// heads in rank order, up to and including the promoted one, or all
        /// of them when none passes. Empty in any other mode.
        pub promotion_audits: Vec<PromotionAudit>,
        /// Each retained cluster's exact credit `c`, in head rank order; each
        /// of a cluster's `m` members receives `c / m` before apportionment.
        /// Empty when the cohort produced no target.
        pub credits: Vec<ClusterCredit>,
        pub distance_monitor: DistanceMonitor,
        pub receipt_root: Digest,
        pub receipt_count: u64,
    }
}

impl ScorePackageV2 {
    /// The probe version `version`, if it is in use.
    pub fn probe(&self, version: u32) -> Option<&ProbeBinding> {
        self.probes.iter().find(|p| p.version == version)
    }

    /// The `(content, probe version)` of every reference fingerprint each
    /// scored entrant is compared with on the probe, in the order of
    /// [`PublicCheck::reference_probes`]: for each probe version in use, in
    /// version order, `R_k` and then `R_k-1` when `k >= 1`.
    pub fn reference_probe_keys(&self) -> Vec<(Digest, u32)> {
        self.probes
            .iter()
            .flat_map(|p| {
                core::iter::once(&self.reference)
                    .chain(&self.predecessor)
                    .map(|r| (r.model.content, p.version))
            })
            .collect()
    }

    /// Structure of the calculation inputs, the public-copy records and the
    /// cluster assignment, excluding the outcome fields (`units`, `mode`,
    /// `promoted`, `credits`) that [`crate::reward_v1`] derives:
    ///
    /// - every scored entrant, whatever its score and other gates, carries
    ///   public-copy statistics, with predecessor statistics exactly when
    ///   `k >= 1`, and an unscored entrant carries none;
    /// - batch statistics cover exactly the documents the entrant succeeded
    ///   on, since references never fail; probe statistics name a probe in
    ///   use and cover at most its documents; cluster statistics cover at
    ///   most the documents both members succeeded on;
    /// - every scored entrant carries its comparison with each reference
    ///   fingerprint on each probe version in use, in the order of
    ///   [`Self::reference_probe_keys`], and a probe match that names a
    ///   reference repeats that comparison exactly;
    /// - probe scores agree: an entrant's own probe score is the same in all
    ///   its comparisons on one version, and a reference's probe score is the
    ///   same for every entrant.
    pub fn validate_inputs(&self) -> Result<()> {
        validate_label(&self.cohort_label)?;
        self.reward_rule.check_reward_v1(&self.chain)?;
        self.params.validate()?;
        validate_probes(&self.probes)?;
        if self.cases == 0 {
            return Err(Error::Invalid("cases"));
        }
        if (self.reference_index == 0) != self.predecessor.is_none() {
            return Err(Error::Invalid("reference predecessor"));
        }
        self.reference.validate(self.cases)?;
        if let Some(p) = &self.predecessor {
            p.validate(self.cases)?;
        }
        if self.entrants.len() > usize::from(MAX_ROSTER) {
            return Err(Error::LengthLimit {
                field: "entrants",
                len: self.entrants.len() as u64,
            });
        }
        validate_admission_order(
            self.entrants
                .iter()
                .map(|e| (e.position(), &e.identity.hotkey)),
            "entrants",
        )?;
        let mut assigned = u64::from(self.reference.assigned)
            + self
                .predecessor
                .as_ref()
                .map_or(0, |p| u64::from(p.assigned));
        let reference_keys = self.reference_probe_keys();
        let mut reference_scores = BTreeMap::<(Digest, u32), Score>::new();
        for entrant in &self.entrants {
            entrant.validate(self.cases, self.span_gate)?;
            assigned += u64::from(entrant.assigned);
            if entrant.public.is_some() != entrant.score.is_some() {
                return Err(Error::Mismatch("public check coverage"));
            }
            if let Some(public) = &entrant.public {
                if public.predecessor.is_some() != self.predecessor.is_some() {
                    return Err(Error::Mismatch("public predecessor statistics"));
                }
                for stats in core::iter::once(&public.current).chain(&public.predecessor) {
                    stats.check_bounds(self.cases)?;
                    if stats.common != entrant.succeeded {
                        return Err(Error::Mismatch("near-copy common documents"));
                    }
                }
                if !public
                    .reference_probes
                    .iter()
                    .map(|m| (m.content, m.version))
                    .eq(reference_keys.iter().copied())
                {
                    return Err(Error::Mismatch("reference probe comparisons"));
                }
                let mut own_scores = BTreeMap::<u32, Score>::new();
                for found in public.reference_probes.iter().chain(&public.probe_match) {
                    let probe = self
                        .probe(found.version)
                        .ok_or(Error::Mismatch("probe version not in use"))?;
                    found.stats.check_bounds(probe.size)?;
                    if *own_scores
                        .entry(found.version)
                        .or_insert(found.entrant_probe_score)
                        != found.entrant_probe_score
                    {
                        return Err(Error::Mismatch("entrant probe score"));
                    }
                }
                for found in &public.reference_probes {
                    if *reference_scores
                        .entry((found.content, found.version))
                        .or_insert(found.public_probe_score)
                        != found.public_probe_score
                    {
                        return Err(Error::Mismatch("reference probe score"));
                    }
                }
                if let Some(found) = &public.probe_match
                    && let Some(reference) = public
                        .reference_probes
                        .iter()
                        .find(|m| (m.content, m.version) == (found.content, found.version))
                    && reference != found
                {
                    return Err(Error::Mismatch("probe match differs from its reference"));
                }
            }
            if let Some(head) = &entrant.cluster_head {
                let head = self
                    .entrant(head)
                    .ok_or(Error::Mismatch("cluster head not an entrant"))?;
                if head.cluster_head != Some(head.identity.hotkey) {
                    return Err(Error::Mismatch("cluster head not a head"));
                }
                if let Some(stats) = &entrant.cluster_stats {
                    stats.check_bounds(head.succeeded.min(entrant.succeeded))?;
                }
            }
        }
        if self.receipt_count < assigned {
            return Err(Error::Mismatch("receipt_count below assignments"));
        }
        Ok(())
    }

    /// The entrant credited to `hotkey`.
    pub fn entrant(&self, hotkey: &AccountId) -> Option<&EntrantScore> {
        self.entrants.iter().find(|e| e.identity.hotkey == *hotkey)
    }

    /// Binds a fully retained receipt index to this package. Validators check
    /// it, and each indexed object, before committing a newly calculated row.
    pub fn check_receipts(&self, index: &ReceiptIndex) -> Result<()> {
        if index.cohort_manifest != self.cohort_manifest {
            return Err(Error::Mismatch("receipt index cohort"));
        }
        if index.digest()? != self.receipt_root || index.count() != self.receipt_count {
            return Err(Error::Mismatch("receipt root or count"));
        }
        Ok(())
    }

    /// Binds this package to the signed manifest it scores: identities, the
    /// complete roster in admission order, parameters and references.
    pub fn check_manifest(&self, manifest: &CohortManifestV2) -> Result<()> {
        let pred_matches = match (&self.predecessor, &manifest.references.predecessor) {
            (None, None) => true,
            (Some(s), Some(m)) => s.model == *m,
            _ => false,
        };
        if self.cohort_manifest != manifest.digest()?
            || self.chain != manifest.chain
            || self.cohort != manifest.cohort
            || self.cohort_label != manifest.cohort_label
            || self.reward_rule != manifest.reward_rule
            || self.params != manifest.params
            || self.published != manifest.published
            || self.probes != manifest.probes
            || self.span_gate != manifest.span_gate
            || self.cases != manifest.batch.cases
            || self.reference_index != manifest.references.index
            || self.reference.model != manifest.references.current
            || !pred_matches
        {
            return Err(Error::Mismatch("package manifest"));
        }
        if self.entrants.len() != manifest.roster.len()
            || self.entrants.iter().zip(&manifest.roster).any(|(e, r)| {
                e.identity != r.identity
                    || e.commitment != r.commitment
                    || e.position() != r.position()
            })
        {
            return Err(Error::Mismatch("package roster"));
        }
        Ok(())
    }
}

record!(
    ScorePackageV2,
    "score-package",
    4,
    Some(KeyRole::Package),
    |p| {
        p.validate_inputs()?;
        let total: u32 = p.entrants.iter().map(|e| u32::from(e.units)).sum();
        match p.mode {
            CutoffMode::NoTarget => {
                if total != 0
                    || p.entrants.iter().any(|e| e.cluster_head.is_some())
                    || !p.credits.is_empty()
                {
                    return Err(Error::Invalid("no-target package allocates"));
                }
            }
            CutoffMode::Margin | CutoffMode::Fallback => {
                if total != u32::from(p.params.row_total) {
                    return Err(Error::Mismatch("allocated units"));
                }
                if p.credits.is_empty() {
                    return Err(Error::Invalid("package without credits"));
                }
            }
        }
        let mut credited = BTreeSet::new();
        for credit in &p.credits {
            credit.validate()?;
            let head = p
                .entrant(&credit.head)
                .ok_or(Error::Mismatch("credited cluster head"))?;
            if head.cluster_head != Some(credit.head) {
                return Err(Error::Mismatch("credited cluster head"));
            }
            if !credited.insert(credit.head) {
                return Err(Error::Duplicate("cluster credit"));
            }
        }
        let mut audited = BTreeSet::new();
        for audit in &p.promotion_audits {
            audit.validate()?;
            if !audited.insert(audit.head) {
                return Err(Error::Duplicate("promotion audit"));
            }
            if !credited.contains(&audit.head) {
                return Err(Error::Mismatch("promotion audit of an uncredited head"));
            }
        }
        match (&p.promoted, p.mode) {
            (Some(hotkey), CutoffMode::Margin) => {
                let promoted = p
                    .entrant(hotkey)
                    .ok_or(Error::Mismatch("promoted entrant"))?;
                if promoted.cluster_head != Some(*hotkey) {
                    return Err(Error::Mismatch("promoted entrant"));
                }
                match p.promotion_audits.split_last() {
                    Some((last, earlier))
                        if last.head == *hotkey
                            && last.passes()
                            && earlier.iter().all(|a| !a.passes()) =>
                    {
                        Ok(())
                    }
                    _ => Err(Error::Mismatch("promotion audits")),
                }
            }
            (None, CutoffMode::Margin) => {
                if p.promotion_audits.is_empty() || p.promotion_audits.iter().any(|a| a.passes()) {
                    return Err(Error::Mismatch("promotion audits"));
                }
                Ok(())
            }
            (None, CutoffMode::Fallback | CutoffMode::NoTarget) => {
                if !p.promotion_audits.is_empty() {
                    return Err(Error::Invalid("promotion audits outside a margin cohort"));
                }
                Ok(())
            }
            _ => Err(Error::Mismatch("promotion and cutoff mode")),
        }
    }
);

canonical_struct! {
    /// A signed operator review that clears a candidate the lookup canary
    /// blocked. It has no reward effect: it changes no credit and no past
    /// promotion, and a cleared candidate is promoted only by a later
    /// cohort's own promotion rule.
    pub struct CanaryClearance {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub cohort_manifest: Digest,
        pub score_package: Digest,
        /// The blocked candidate.
        pub head: AccountId,
        /// Digest of the private review record.
        pub review: Digest,
        /// Epoch index at which the review cleared the candidate.
        pub cleared_epoch: u64,
    }
}

record!(
    CanaryClearance,
    "canary-clearance",
    1,
    Some(KeyRole::Package),
    |_c| { Ok(()) }
);

impl CanaryClearance {
    /// The clearance names `package` and a candidate that package records as
    /// blocked by two canary flags.
    pub fn check(&self, package: &ScorePackageV2) -> Result<()> {
        if self.chain != package.chain
            || self.cohort != package.cohort
            || self.cohort_manifest != package.cohort_manifest
            || self.score_package != package.digest()?
        {
            return Err(Error::Mismatch("clearance package"));
        }
        match package
            .promotion_audits
            .iter()
            .find(|a| a.head == self.head)
        {
            Some(audit) if audit.blocked() => Ok(()),
            _ => Err(Error::Mismatch("clearance of a candidate not blocked")),
        }
    }
}

// ---------------------------------------------------------------------------
// Targets, registration snapshots and rows
// ---------------------------------------------------------------------------

/// What an active calculation target pays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetSource {
    /// The launch row: the whole pool to the owner hotkey's UID, active until
    /// some cohort first produces a target.
    LaunchBurn,
    /// A cohort's score package.
    Cohort {
        cohort: u64,
        cohort_label: String,
        cohort_manifest: Digest,
        revision: u64,
        score_package: Digest,
        receipt_root: Digest,
        receipt_count: u64,
    },
}

impl Canonical for TargetSource {
    fn encode_to(&self, e: &mut Encoder) {
        match self {
            Self::LaunchBurn => e.u8(1),
            Self::Cohort {
                cohort,
                cohort_label,
                cohort_manifest,
                revision,
                score_package,
                receipt_root,
                receipt_count,
            } => {
                e.u8(2);
                cohort.encode_to(e);
                cohort_label.encode_to(e);
                cohort_manifest.encode_to(e);
                revision.encode_to(e);
                score_package.encode_to(e);
                receipt_root.encode_to(e);
                receipt_count.encode_to(e);
            }
        }
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        match d.u8()? {
            1 => Ok(Self::LaunchBurn),
            2 => Ok(Self::Cohort {
                cohort: u64::decode_from(d)?,
                cohort_label: String::decode_from(d)?,
                cohort_manifest: Digest::decode_from(d)?,
                revision: u64::decode_from(d)?,
                score_package: Digest::decode_from(d)?,
                receipt_root: Digest::decode_from(d)?,
                receipt_count: u64::decode_from(d)?,
            }),
            tag => Err(Error::InvalidTag {
                field: "target_source",
                tag,
            }),
        }
    }
}

canonical_struct! {
    /// The coordinator's signed `reward-v1` calculation target. It changes
    /// only when a complete successor package with retained receipts exists;
    /// a cohort that produces no target leaves the previous one active. The
    /// burn destination is not part of it: each row reads the owner hotkey
    /// from finalized state, so an owner-hotkey rotation changes no target.
    pub struct CalculationTargetV2 {
        pub chain: ChainBinding,
        pub reward_rule: RewardRule,
        pub source: TargetSource,
        /// The target this one supersedes.
        pub predecessor: Option<Digest>,
    }
}

record!(
    CalculationTargetV2,
    "calculation-target",
    2,
    Some(KeyRole::Package),
    |t| {
        t.reward_rule.check_reward_v1(&t.chain)?;
        if let TargetSource::Cohort { cohort_label, .. } = &t.source {
            validate_label(cohort_label)?;
        }
        Ok(())
    }
);

impl CalculationTargetV2 {
    /// The launch burn row, active while no cohort has produced a target. It
    /// names no owner hotkey: each row burns through the owner hotkey of the
    /// validator's finalized snapshot.
    pub fn launch_burn(chain: ChainBinding) -> Self {
        Self {
            chain,
            reward_rule: RewardRule::RewardV1,
            source: TargetSource::LaunchBurn,
            predecessor: None,
        }
    }

    /// The target of a package that produced one.
    pub fn for_package(package: &ScorePackageV2, predecessor: Option<Digest>) -> Result<Self> {
        if package.mode == CutoffMode::NoTarget {
            return Err(Error::Invalid("cohort produced no target"));
        }
        Ok(Self {
            chain: package.chain.clone(),
            reward_rule: package.reward_rule,
            source: TargetSource::Cohort {
                cohort: package.cohort,
                cohort_label: package.cohort_label.clone(),
                cohort_manifest: package.cohort_manifest,
                revision: package.revision,
                score_package: package.digest()?,
                receipt_root: package.receipt_root,
                receipt_count: package.receipt_count,
            },
            predecessor,
        })
    }

    pub fn check_package(&self, package: &ScorePackageV2) -> Result<()> {
        if Self::for_package(package, self.predecessor)? != *self {
            return Err(Error::Mismatch("calculation target package"));
        }
        Ok(())
    }
}

canonical_struct! {
    /// Registrations and the subnet owner hotkey a validator read from one
    /// finalized block.
    pub struct RegistrationSnapshotV2 {
        pub chain: ChainBinding,
        pub block: u64,
        pub block_hash: Digest,
        /// The subnet owner hotkey in finalized state. Burned units go to the
        /// UID it holds in `neurons`.
        pub owner_hotkey: AccountId,
        /// Sorted by UID; each UID and each hotkey appears once.
        pub neurons: Vec<NeuronIdentity>,
    }
}

record!(
    RegistrationSnapshotV2,
    "registration-snapshot",
    2,
    None,
    |s| {
        let uids: Vec<_> = s.neurons.iter().map(|n| n.uid).collect();
        strictly_increasing(&uids, "registration uid")?;
        let mut hotkeys: Vec<_> = s.neurons.iter().map(|n| n.hotkey).collect();
        hotkeys.sort_unstable();
        strictly_increasing(&hotkeys, "registration hotkey")
    }
);

impl RegistrationSnapshotV2 {
    /// The registration currently held by `hotkey`, if any.
    pub fn by_hotkey(&self, hotkey: &AccountId) -> Option<&NeuronIdentity> {
        self.neurons.iter().find(|n| n.hotkey == *hotkey)
    }
}

canonical_struct! {
    /// Canonical `reward-v1` native row: the largest-remainder split of 65,535
    /// units mapped to current UIDs. The chain max-upscales it on submission.
    pub struct WeightRowV2 {
        pub chain: ChainBinding,
        pub calculation_target: Digest,
        pub reward_rule: RewardRule,
        pub registration_block: u64,
        pub registration: Digest,
        /// The snapshot's finalized subnet owner hotkey. Any burned units are
        /// at the UID it held there.
        pub owner_hotkey: AccountId,
        /// Sorted by UID, nonzero weights, summing to exactly 65,535.
        pub entries: Vec<RowEntry>,
    }
}

impl WeightRowV2 {
    /// The row as committed on chain: the chain and rtao both max-upscale a
    /// committed row so that its largest entry is 65,535, so the canonical
    /// row (summing to 65,535) is committed in that form and every reveal is
    /// compared with it. Each entry is `w * 65535 / max`, rounded half to
    /// even in exact integer arithmetic; the largest entry becomes 65,535 and
    /// no entry becomes zero. A row of one entry is unchanged.
    pub fn chain_values(&self) -> Vec<(u16, u16)> {
        let max = self.entries.iter().map(|e| u64::from(e.weight)).max().unwrap_or(0);
        self.entries
            .iter()
            .map(|e| (e.uid, max_upscale(u64::from(e.weight), max)))
            .collect()
    }
}

/// `round_half_even(w * 65535 / max)` for `0 < w <= max <= 65535`.
fn max_upscale(w: u64, max: u64) -> u16 {
    if max == 0 {
        return 0;
    }
    let num = w * u64::from(u16::MAX);
    let (q, r) = (num / max, num % max);
    let up = match (2 * r).cmp(&max) {
        core::cmp::Ordering::Greater => true,
        core::cmp::Ordering::Equal => q % 2 == 1,
        core::cmp::Ordering::Less => false,
    };
    u16::try_from(q + u64::from(up)).unwrap_or(u16::MAX)
}

record!(WeightRowV2, "weight-row", 2, None, |r| {
    r.reward_rule.check_reward_v1(&r.chain)?;
    let uids: Vec<_> = r.entries.iter().map(|e| e.uid).collect();
    strictly_increasing(&uids, "row uid")?;
    if r.entries.iter().any(|e| e.weight == 0) {
        return Err(Error::Invalid("zero row weight"));
    }
    if r.entries.iter().map(|e| u32::from(e.weight)).sum::<u32>() != u32::from(ROW_TOTAL) {
        return Err(Error::Invalid("row total"));
    }
    Ok(())
});

// ---------------------------------------------------------------------------
// Inference-content identity
// ---------------------------------------------------------------------------

tagged_enum! {
    /// Role of one artifact file in the declared inference calculation.
    pub enum ComponentRole ("component_role") {
        Graph = 1,
        Weights = 2,
        Tokenizer = 3,
        Calibration = 4,
        OutputSchema = 5,
    }
}

canonical_struct! {
    #[derive(Copy, PartialOrd, Ord)]
    pub struct Component {
        pub role: ComponentRole,
        pub sha256: Digest,
        pub len: u64,
    }
}

canonical_struct! {
    /// The duplicate content rule: content identity is the frozen contract and
    /// the set of inference components by role, digest and length. File
    /// names, publisher signatures, owner metadata, reference test vectors and
    /// the commitment nonce are excluded, so renaming or re-signing an
    /// artifact cannot create new inference content. Its digest is the
    /// canonical inference-content digest. It is derived from an artifact
    /// object's header ([`crate::ArtifactHeader::content`]), so anyone holding
    /// the published object can recompute it.
    pub struct InferenceContent {
        pub contract: String,
        /// Sorted, without duplicates.
        pub components: Vec<Component>,
    }
}

record!(InferenceContent, "inference-content", 1, None, |c| {
    validate_contract(&c.contract)?;
    bounded(&c.components, "components")?;
    if !c.components.iter().any(|x| x.role == ComponentRole::Graph) {
        return Err(Error::Invalid("inference content has no graph"));
    }
    strictly_increasing(&c.components, "components")
});
