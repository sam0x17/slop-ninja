//! Versioned protocol records.
//!
//! A record's bytes are `str(kind) || u16(version) || fields`, and its digest
//! is [`domain_hash`] under `slop_ninja.protocol/<kind>/v<version>`. Encoding and
//! decoding both validate, so an accepted record always satisfies its invariants.

use core::fmt;

use crate::codec::{Canonical, Decoder, Encoder, MAX_SEQUENCE_LEN, canonical_struct};
use crate::hash::{domain_hash, hex, object_sha256};
use crate::sign::KeyRole;
use crate::{Digest, Error, Result};

/// Fixed-point scale of [`Score`]: 10^18 represents a utility of 1.
pub const SCORE_SCALE: u64 = 1_000_000_000_000_000_000;
/// Fixed-point scale of [`Probability`]: 10^9 represents probability 1.
pub const PROBABILITY_SCALE: u32 = 1_000_000_000;
/// Longest cohort label; matches the native adapter's journal limit.
pub const MAX_LABEL_LEN: usize = 256;
const MAX_CONTRACT_LEN: usize = 128;
/// Genesis hash of the Bittensor testnet (`test.finney.opentensor.ai`),
/// `0x8f9cf856...263105`. `testnet-reward-v0` is valid on this chain only.
pub const TESTNET_GENESIS: Digest = Digest([
    0x8f, 0x9c, 0xf8, 0x56, 0xbf, 0x55, 0x8a, 0x14, 0x44, 0x0e, 0x75, 0x56, 0x9c, 0x9e, 0x58, 0x59,
    0x47, 0x57, 0x04, 0x8d, 0x7b, 0x3a, 0x84, 0xb5, 0xd2, 0x5f, 0x6b, 0xd9, 0x78, 0x26, 0x31, 0x05,
]);

/// Genesis hash of Bittensor mainnet (Finney), `0x2f0555cc...ab36c03`.
/// `reward-v1` manifests on this chain must meet the mainnet batch floor.
pub const FINNEY_GENESIS: Digest = Digest([
    0x2f, 0x05, 0x55, 0xcc, 0x76, 0xfc, 0x28, 0x40, 0xa2, 0x5a, 0x6e, 0xa3, 0xb9, 0x63, 0x71, 0x46,
    0x80, 0x6f, 0x1f, 0x44, 0xb0, 0x90, 0xc1, 0x75, 0xff, 0xde, 0x2a, 0x7e, 0x5a, 0xb3, 0x6c, 0x03,
]);

/// A versioned, self-describing protocol record.
pub trait Record: Canonical {
    const KIND: &'static str;
    const VERSION: u16;
    /// Key role allowed to sign this record, if it is ever signed.
    const SIGNER: Option<KeyRole>;

    fn validate(&self) -> Result<()>;

    fn domain() -> String {
        format!("slop_ninja.protocol/{}/v{}", Self::KIND, Self::VERSION)
    }

    fn to_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut e = Encoder::new();
        e.str(Self::KIND);
        e.u16(Self::VERSION);
        self.encode_to(&mut e);
        Ok(e.into_bytes())
    }

    fn decode_record(d: &mut Decoder<'_>) -> Result<Self> {
        if d.str()? != Self::KIND {
            return Err(Error::WrongKind {
                expected: Self::KIND,
            });
        }
        let version = d.u16()?;
        if version != Self::VERSION {
            return Err(Error::WrongVersion {
                kind: Self::KIND,
                version,
            });
        }
        let record = Self::decode_from(d)?;
        record.validate()?;
        Ok(record)
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(bytes);
        let record = Self::decode_record(&mut d)?;
        d.finish()?;
        Ok(record)
    }

    fn digest(&self) -> Result<Digest> {
        Ok(domain_hash(&Self::domain(), &self.to_bytes()?))
    }
}

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

/// A 32-byte public account key (an sr25519 hotkey on chain).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct AccountId(pub [u8; 32]);

impl fmt::Debug for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex(&self.0))
    }
}

impl Canonical for AccountId {
    fn encode_to(&self, e: &mut Encoder) {
        e.raw(&self.0);
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(d.bytes32()?))
    }
}

/// Utility in `[0, 1]` scaled by [`SCORE_SCALE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Score(u64);

impl Score {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(SCORE_SCALE);
    pub fn new(value: u64) -> Result<Self> {
        if value > SCORE_SCALE {
            return Err(Error::OutOfRange("score"));
        }
        Ok(Self(value))
    }
    pub fn get(self) -> u64 {
        self.0
    }
}

impl Canonical for Score {
    fn encode_to(&self, e: &mut Encoder) {
        e.u64(self.0);
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        Self::new(d.u64()?)
    }
}

/// Probability in `[0, 1]` scaled by [`PROBABILITY_SCALE`] (parts per billion).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Probability(u32);

impl Probability {
    pub fn new(ppb: u32) -> Result<Self> {
        if ppb > PROBABILITY_SCALE {
            return Err(Error::OutOfRange("probability"));
        }
        Ok(Self(ppb))
    }
    /// Converts a model output. Non-finite and out-of-range values are invalid
    /// output, never clamped. `p * 10^9` is one correctly rounded IEEE-754
    /// multiplication, then rounded half to even, so every target agrees.
    pub fn from_f64(p: f64) -> Result<Self> {
        if !p.is_finite() || !(0.0..=1.0).contains(&p) {
            return Err(Error::OutOfRange("probability"));
        }
        Self::new((p * f64::from(PROBABILITY_SCALE)).round_ties_even() as u32)
    }
    pub fn ppb(self) -> u32 {
        self.0
    }
}

impl Canonical for Probability {
    fn encode_to(&self, e: &mut Encoder) {
        e.u32(self.0);
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        Self::new(d.u32()?)
    }
}

canonical_struct! {
    /// Chain coordinates. The netuid is configuration, never a protocol constant.
    pub struct ChainBinding {
        pub genesis: Digest,
        pub netuid: u16,
        pub mechanism: u8,
    }
}

canonical_struct! {
    /// One registration generation. A reused UID with a new hotkey or
    /// registration block is a different identity.
    #[derive(Copy, PartialOrd, Ord)]
    pub struct NeuronIdentity {
        pub uid: u16,
        pub hotkey: AccountId,
        pub registration_block: u64,
    }
}

/// Frozen reward calculation selected by a cohort.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewardRule {
    /// Testnet shortcut T8, permitted on the testnet only; Finney uses
    /// `reward-v1`. See [`crate::reward`].
    TestnetRewardV0,
    /// The frozen `reward-v1` band mechanism of whitepaper 0.56, permitted on
    /// every chain, Finney included. See [`crate::reward_v1`].
    RewardV1,
}

impl RewardRule {
    pub fn name(self) -> &'static str {
        match self {
            Self::TestnetRewardV0 => "testnet-reward-v0",
            Self::RewardV1 => "reward-v1",
        }
    }

    /// Fails closed unless the rule is permitted on `chain`.
    /// `testnet-reward-v0` (shortcut T8) is permitted only on [`TESTNET_GENESIS`],
    /// so no record using it can be encoded, decoded or scored for Finney.
    /// `reward-v1` is permitted on every chain.
    pub fn check_chain(self, chain: &ChainBinding) -> Result<()> {
        match self {
            Self::TestnetRewardV0 if chain.genesis == TESTNET_GENESIS => Ok(()),
            Self::TestnetRewardV0 => Err(Error::Mismatch("testnet_reward_v0 genesis")),
            Self::RewardV1 => Ok(()),
        }
    }

    /// Version-1 records carry the `testnet-reward-v0` calculation only;
    /// `reward-v1` uses the version-2 and later records, so neither calculation can be
    /// applied to the other's inputs.
    pub(crate) fn check_testnet_reward_v0(self, chain: &ChainBinding) -> Result<()> {
        if self != Self::TestnetRewardV0 {
            return Err(Error::Mismatch("reward rule record version"));
        }
        self.check_chain(chain)
    }

    /// Version-2 and later records carry the `reward-v1` calculation only.
    pub(crate) fn check_reward_v1(self, chain: &ChainBinding) -> Result<()> {
        if self != Self::RewardV1 {
            return Err(Error::Mismatch("reward rule record version"));
        }
        self.check_chain(chain)
    }
}

impl Canonical for RewardRule {
    fn encode_to(&self, e: &mut Encoder) {
        e.u8(match self {
            Self::TestnetRewardV0 => 1,
            Self::RewardV1 => 2,
        });
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        match d.u8()? {
            1 => Ok(Self::TestnetRewardV0),
            2 => Ok(Self::RewardV1),
            tag => Err(Error::InvalidTag {
                field: "reward_rule",
                tag,
            }),
        }
    }
}

/// Public, content-free class of an attempt outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OutcomeClass {
    Prediction,
    ModelError,
    InvalidOutput,
    ParseFailure,
    /// Operator-side failure. It must be reconciled under the same cohort and
    /// is never converted into a miner failure.
    InfrastructureError,
}

impl OutcomeClass {
    fn tag(self) -> u8 {
        match self {
            Self::Prediction => 1,
            Self::ModelError => 2,
            Self::InvalidOutput => 3,
            Self::ParseFailure => 4,
            Self::InfrastructureError => 5,
        }
    }
    fn from_tag(tag: u8) -> Result<Self> {
        Ok(match tag {
            1 => Self::Prediction,
            2 => Self::ModelError,
            3 => Self::InvalidOutput,
            4 => Self::ParseFailure,
            5 => Self::InfrastructureError,
            tag => {
                return Err(Error::InvalidTag {
                    field: "outcome",
                    tag,
                });
            }
        })
    }
}

impl Canonical for OutcomeClass {
    fn encode_to(&self, e: &mut Encoder) {
        e.u8(self.tag());
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        Self::from_tag(d.u8()?)
    }
}

/// Private outcome of one attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Prediction {
        /// Calibrated probability of any AI involvement.
        involvement: Probability,
        /// Expected AI-written or rewritten share; at most `involvement`.
        share: Probability,
    },
    ModelError,
    InvalidOutput,
    ParseFailure,
    InfrastructureError,
}

impl Outcome {
    pub fn class(&self) -> OutcomeClass {
        match self {
            Self::Prediction { .. } => OutcomeClass::Prediction,
            Self::ModelError => OutcomeClass::ModelError,
            Self::InvalidOutput => OutcomeClass::InvalidOutput,
            Self::ParseFailure => OutcomeClass::ParseFailure,
            Self::InfrastructureError => OutcomeClass::InfrastructureError,
        }
    }
}

impl Canonical for Outcome {
    fn encode_to(&self, e: &mut Encoder) {
        self.class().encode_to(e);
        if let Self::Prediction { involvement, share } = self {
            involvement.encode_to(e);
            share.encode_to(e);
        }
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(match OutcomeClass::decode_from(d)? {
            OutcomeClass::Prediction => Self::Prediction {
                involvement: Probability::decode_from(d)?,
                share: Probability::decode_from(d)?,
            },
            OutcomeClass::ModelError => Self::ModelError,
            OutcomeClass::InvalidOutput => Self::InvalidOutput,
            OutcomeClass::ParseFailure => Self::ParseFailure,
            OutcomeClass::InfrastructureError => Self::InfrastructureError,
        })
    }
}

pub(crate) fn validate_label(label: &str) -> Result<()> {
    if label.is_empty() || label.len() > MAX_LABEL_LEN {
        return Err(Error::Invalid("cohort_label"));
    }
    Ok(())
}

/// Rejects at encode time any sequence that decoding would refuse.
pub(crate) fn bounded<T>(items: &[T], field: &'static str) -> Result<()> {
    if items.len() > MAX_SEQUENCE_LEN as usize {
        return Err(Error::LengthLimit {
            field,
            len: items.len() as u64,
        });
    }
    Ok(())
}

pub(crate) fn strictly_increasing<T: Ord>(items: &[T], field: &'static str) -> Result<()> {
    bounded(items, field)?;
    for pair in items.windows(2) {
        match pair[0].cmp(&pair[1]) {
            core::cmp::Ordering::Less => {}
            core::cmp::Ordering::Equal => return Err(Error::Duplicate(field)),
            core::cmp::Ordering::Greater => return Err(Error::Unsorted(field)),
        }
    }
    Ok(())
}

/// Allow-lists are sorted by `(uid, hotkey, registration_block)` and never
/// contain UID 0, which receives the unallocated pool.
pub(crate) fn validate_allow_list(list: &[NeuronIdentity]) -> Result<()> {
    strictly_increasing(list, "allow_list")?;
    if list.iter().any(|n| n.uid == 0) {
        return Err(Error::Invalid("allow_list contains uid 0"));
    }
    Ok(())
}

/// The burn identity is the owner registration expected at UID 0.
pub(crate) fn validate_burn_identity(burn: &NeuronIdentity) -> Result<()> {
    if burn.uid != 0 {
        return Err(Error::Invalid("burn identity uid"));
    }
    Ok(())
}

canonical_struct! {
    /// Frozen comparison for one cohort. The evaluation set is bound by digest;
    /// its documents and labels stay private.
    pub struct CohortManifest {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub cohort_label: String,
        /// Finalized tempo-boundary block at which the cohort opened.
        pub opened_block: u64,
        pub evaluation_set: Digest,
        pub task_count: u32,
        pub group_count: u32,
        pub baseline_artifact: Digest,
        pub reward_rule: RewardRule,
        /// Attempts allowed per assignment; the frozen attempt-order rule
        /// selects the earliest non-infrastructure outcome.
        pub max_attempts: u8,
        pub allow_list: Vec<NeuronIdentity>,
        /// Owner registration at UID 0 that receives the unallocated pool.
        pub burn_identity: NeuronIdentity,
        /// Admitted artifact IDs, sorted.
        pub artifacts: Vec<Digest>,
    }
}

record!(
    CohortManifest,
    "cohort-manifest",
    1,
    Some(KeyRole::Package),
    |m| {
        validate_label(&m.cohort_label)?;
        m.reward_rule.check_testnet_reward_v0(&m.chain)?;
        if m.task_count == 0 {
            return Err(Error::Invalid("task_count"));
        }
        if m.group_count == 0 || m.group_count > m.task_count {
            return Err(Error::Invalid("group_count"));
        }
        if m.max_attempts == 0 {
            return Err(Error::Invalid("max_attempts"));
        }
        validate_allow_list(&m.allow_list)?;
        validate_burn_identity(&m.burn_identity)?;
        strictly_increasing(&m.artifacts, "artifacts")
    }
);

canonical_struct! {
    /// A miner's numeric model artifact. Its digest is the artifact ID.
    pub struct ArtifactRecord {
        pub owner: NeuronIdentity,
        /// Frozen input/output contract identifier.
        pub contract: String,
        pub package_sha256: Digest,
        pub model_sha256: Digest,
        /// OCI image digest of the hosted runtime image.
        pub image_digest: Digest,
        /// Finalized block of the on-chain admission commitment.
        pub commitment_block: u64,
    }
}

record!(ArtifactRecord, "artifact", 1, None, |a| {
    if a.contract.is_empty() || a.contract.len() > MAX_CONTRACT_LEN {
        return Err(Error::Invalid("contract"));
    }
    Ok(())
});

/// The digest a miner commits on chain before revealing `artifact` and `nonce`.
pub fn admission_commitment(artifact: &Digest, nonce: &[u8; 32]) -> Digest {
    let mut payload = artifact.0.to_vec();
    payload.extend_from_slice(nonce);
    domain_hash("slop_ninja.protocol/artifact-commitment/v1", &payload)
}

canonical_struct! {
    /// One artifact assigned one private task.
    pub struct Assignment {
        pub cohort_manifest: Digest,
        pub artifact: Digest,
        pub task_index: u32,
        pub group: u32,
    }
}

record!(Assignment, "assignment", 1, None, |_a| { Ok(()) });

impl Assignment {
    pub fn check_manifest(&self, manifest: &CohortManifest) -> Result<()> {
        if self.cohort_manifest != manifest.digest()? {
            return Err(Error::Mismatch("assignment cohort_manifest"));
        }
        if self.task_index >= manifest.task_count || self.group >= manifest.group_count {
            return Err(Error::OutOfRange("assignment task"));
        }
        if self.artifact != manifest.baseline_artifact
            && manifest.artifacts.binary_search(&self.artifact).is_err()
        {
            return Err(Error::Mismatch("assignment artifact"));
        }
        Ok(())
    }
}

canonical_struct! {
    /// One numbered execution of an assignment. Its digest is the stable
    /// attempt ID, so a reclaimed task never creates a second identity.
    pub struct Attempt {
        pub assignment: Digest,
        pub number: u8,
    }
}

record!(Attempt, "attempt", 1, None, |a| {
    if a.number == 0 {
        return Err(Error::Invalid("attempt number"));
    }
    Ok(())
});

canonical_struct! {
    /// Private result of one attempt. Never published.
    pub struct AttemptResult {
        pub attempt: Digest,
        pub outcome: Outcome,
    }
}

record!(AttemptResult, "result", 1, None, |r| {
    if let Outcome::Prediction { involvement, share } = r.outcome
        && share > involvement
    {
        return Err(Error::Invalid("share exceeds involvement"));
    }
    Ok(())
});

/// Salted commitment to a private result. The salt stays private.
pub fn result_commitment(result: &AttemptResult, salt: &[u8; 32]) -> Result<Digest> {
    let mut payload = salt.to_vec();
    payload.extend_from_slice(&result.digest()?.0);
    Ok(domain_hash(
        "slop_ninja.protocol/result-commitment/v1",
        &payload,
    ))
}

canonical_struct! {
    /// Content-free receipt stored as one object per attempt, failures included.
    pub struct AttemptReceipt {
        pub attempt: Digest,
        pub assignment: Digest,
        pub artifact: Digest,
        pub number: u8,
        pub outcome: OutcomeClass,
        pub result_commitment: Digest,
    }
}

record!(AttemptReceipt, "attempt-receipt", 1, None, |r| {
    if r.number == 0 {
        return Err(Error::Invalid("attempt number"));
    }
    Ok(())
});

impl AttemptReceipt {
    pub fn new(
        assignment: &Assignment,
        attempt: &Attempt,
        result: &AttemptResult,
        salt: &[u8; 32],
    ) -> Result<Self> {
        let assignment_id = assignment.digest()?;
        let attempt_id = attempt.digest()?;
        if attempt.assignment != assignment_id || result.attempt != attempt_id {
            return Err(Error::Mismatch("receipt linkage"));
        }
        Ok(Self {
            attempt: attempt_id,
            assignment: assignment_id,
            artifact: assignment.artifact,
            number: attempt.number,
            outcome: result.outcome.class(),
            result_commitment: result_commitment(result, salt)?,
        })
    }
}

canonical_struct! {
    pub struct ReceiptIndexEntry {
        pub attempt: Digest,
        pub outcome: OutcomeClass,
        /// Plain SHA-256 of the exact stored receipt object.
        pub object_sha256: Digest,
        pub object_len: u32,
    }
}

impl ReceiptIndexEntry {
    pub fn for_receipt(receipt: &AttemptReceipt) -> Result<Self> {
        let bytes = receipt.to_bytes()?;
        Ok(Self {
            attempt: receipt.attempt,
            outcome: receipt.outcome,
            object_sha256: object_sha256(&bytes),
            object_len: bytes.len() as u32,
        })
    }
}

canonical_struct! {
    /// Full receipt index for a cohort. Its digest is the receipt root bound
    /// into the score package; its entry count is the receipt count.
    pub struct ReceiptIndex {
        pub cohort_manifest: Digest,
        /// Sorted by attempt ID.
        pub entries: Vec<ReceiptIndexEntry>,
    }
}

record!(
    ReceiptIndex,
    "receipt-index",
    1,
    Some(KeyRole::Receipt),
    |i| {
        bounded(&i.entries, "receipt entries")?;
        for pair in i.entries.windows(2) {
            match pair[0].attempt.cmp(&pair[1].attempt) {
                core::cmp::Ordering::Less => {}
                core::cmp::Ordering::Equal => return Err(Error::Duplicate("receipt attempt")),
                core::cmp::Ordering::Greater => return Err(Error::Unsorted("receipt attempt")),
            }
        }
        Ok(())
    }
);

impl ReceiptIndex {
    pub fn count(&self) -> u64 {
        self.entries.len() as u64
    }

    /// Verifies one retained object against its index entry.
    pub fn verify_object(&self, position: usize, bytes: &[u8]) -> Result<AttemptReceipt> {
        let entry = self
            .entries
            .get(position)
            .ok_or(Error::OutOfRange("receipt position"))?;
        if bytes.len() as u64 != u64::from(entry.object_len)
            || object_sha256(bytes) != entry.object_sha256
        {
            return Err(Error::Mismatch("receipt object digest"));
        }
        let receipt = AttemptReceipt::from_bytes(bytes)?;
        if receipt.attempt != entry.attempt || receipt.outcome != entry.outcome {
            return Err(Error::Mismatch("receipt object entry"));
        }
        Ok(receipt)
    }
}

canonical_struct! {
    /// Aggregate public score components for one credited identity.
    pub struct MinerScore {
        pub identity: NeuronIdentity,
        pub artifact: Digest,
        /// Source-balanced confidence Brier utility.
        pub score: Score,
        pub assigned: u32,
        pub succeeded: u32,
        /// Attributable failures; zero utility, weight retained.
        pub failed: u32,
    }
}

canonical_struct! {
    /// Signed public score package: per-miner components and the public inputs
    /// every validator needs to derive the native row independently.
    pub struct ScorePackage {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub cohort_label: String,
        pub cohort_manifest: Digest,
        pub revision: u64,
        pub reward_rule: RewardRule,
        pub baseline: Score,
        pub allow_list: Vec<NeuronIdentity>,
        /// Owner registration at UID 0 that receives the unallocated pool.
        pub burn_identity: NeuronIdentity,
        /// Sorted by identity.
        pub scores: Vec<MinerScore>,
        pub receipt_root: Digest,
        pub receipt_count: u64,
    }
}

record!(
    ScorePackage,
    "score-package",
    1,
    Some(KeyRole::Package),
    |p| {
        validate_label(&p.cohort_label)?;
        p.reward_rule.check_testnet_reward_v0(&p.chain)?;
        validate_allow_list(&p.allow_list)?;
        validate_burn_identity(&p.burn_identity)?;
        let identities: Vec<_> = p.scores.iter().map(|s| s.identity).collect();
        strictly_increasing(&identities, "scores")?;
        let mut artifacts: Vec<_> = p.scores.iter().map(|s| s.artifact).collect();
        artifacts.sort_unstable();
        strictly_increasing(&artifacts, "credited artifact")?;
        let mut assigned_total = 0u64;
        for s in &p.scores {
            if s.assigned == 0 {
                return Err(Error::Invalid("assigned"));
            }
            let outcomes = s
                .succeeded
                .checked_add(s.failed)
                .ok_or(Error::Overflow("outcome count"))?;
            if outcomes != s.assigned {
                return Err(Error::Mismatch("outcome count"));
            }
            assigned_total += u64::from(s.assigned);
        }
        if p.receipt_count < assigned_total {
            return Err(Error::Mismatch("receipt_count below assignments"));
        }
        Ok(())
    }
);

impl ScorePackage {
    /// Binds this package to the signed cohort manifest it claims to score.
    pub fn check_manifest(&self, manifest: &CohortManifest) -> Result<()> {
        if self.cohort_manifest != manifest.digest()?
            || self.chain != manifest.chain
            || self.cohort != manifest.cohort
            || self.cohort_label != manifest.cohort_label
            || self.reward_rule != manifest.reward_rule
            || self.allow_list != manifest.allow_list
            || self.burn_identity != manifest.burn_identity
        {
            return Err(Error::Mismatch("package manifest"));
        }
        if self
            .scores
            .iter()
            .any(|s| manifest.artifacts.binary_search(&s.artifact).is_err())
        {
            return Err(Error::Mismatch("package artifact"));
        }
        Ok(())
    }

    /// Binds a fully retained receipt index to this package.
    pub fn check_receipts(&self, index: &ReceiptIndex) -> Result<()> {
        if index.cohort_manifest != self.cohort_manifest {
            return Err(Error::Mismatch("receipt index cohort"));
        }
        if index.digest()? != self.receipt_root || index.count() != self.receipt_count {
            return Err(Error::Mismatch("receipt root or count"));
        }
        Ok(())
    }
}

canonical_struct! {
    /// The coordinator's signed calculation target. It changes only when a
    /// complete successor package with retained receipts exists.
    pub struct CalculationTarget {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub cohort_label: String,
        pub cohort_manifest: Digest,
        pub revision: u64,
        pub reward_rule: RewardRule,
        pub score_package: Digest,
        pub receipt_root: Digest,
        pub receipt_count: u64,
        pub predecessor: Option<Digest>,
    }
}

record!(
    CalculationTarget,
    "calculation-target",
    1,
    Some(KeyRole::Package),
    |t| {
        validate_label(&t.cohort_label)?;
        t.reward_rule.check_testnet_reward_v0(&t.chain)
    }
);

impl CalculationTarget {
    pub fn for_package(package: &ScorePackage, predecessor: Option<Digest>) -> Result<Self> {
        Ok(Self {
            chain: package.chain.clone(),
            cohort: package.cohort,
            cohort_label: package.cohort_label.clone(),
            cohort_manifest: package.cohort_manifest,
            revision: package.revision,
            reward_rule: package.reward_rule,
            score_package: package.digest()?,
            receipt_root: package.receipt_root,
            receipt_count: package.receipt_count,
            predecessor,
        })
    }

    pub fn check_package(&self, package: &ScorePackage) -> Result<()> {
        if Self::for_package(package, self.predecessor)? != *self {
            return Err(Error::Mismatch("calculation target package"));
        }
        Ok(())
    }
}

canonical_struct! {
    /// Registrations a validator read from one finalized block.
    pub struct RegistrationSnapshot {
        pub chain: ChainBinding,
        pub block: u64,
        pub block_hash: Digest,
        /// Sorted by UID; each UID appears once.
        pub neurons: Vec<NeuronIdentity>,
    }
}

record!(
    RegistrationSnapshot,
    "registration-snapshot",
    1,
    None,
    |s| {
        let uids: Vec<_> = s.neurons.iter().map(|n| n.uid).collect();
        strictly_increasing(&uids, "registration uid")
    }
);

canonical_struct! {
    #[derive(Copy)]
    pub struct RowEntry {
        pub uid: u16,
        pub weight: u16,
    }
}

canonical_struct! {
    /// Canonical native weight row. Equal inputs give equal digests on every
    /// target; validators compare rows by digest.
    pub struct WeightRow {
        pub chain: ChainBinding,
        pub calculation_target: Digest,
        pub reward_rule: RewardRule,
        pub registration_block: u64,
        pub registration: Digest,
        /// Sorted by UID, nonzero weights, maximum exactly `u16::MAX`.
        pub entries: Vec<RowEntry>,
    }
}

record!(WeightRow, "weight-row", 1, None, |r| {
    r.reward_rule.check_testnet_reward_v0(&r.chain)?;
    let uids: Vec<_> = r.entries.iter().map(|e| e.uid).collect();
    strictly_increasing(&uids, "row uid")?;
    if r.entries.iter().any(|e| e.weight == 0) {
        return Err(Error::Invalid("zero row weight"));
    }
    if r.entries.iter().map(|e| e.weight).max() != Some(u16::MAX) {
        return Err(Error::Invalid("row maximum"));
    }
    Ok(())
});
