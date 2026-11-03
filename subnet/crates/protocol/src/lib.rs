//! Shared Slop Ninja subnet protocol: versioned records, canonical encoding,
//! domain-separated digests and signatures, stable operation identities,
//! private submission records, artifact objects and their inference-content
//! identity, the published-artifact index and the near-copy relation, the
//! validators' launch and reward-intent authorizations and their revocation, and deterministic
//! reward arithmetic for `testnet-reward-v0` (testnet only) and `reward-v1`.
//!
//! The crate is pure computation. It performs no I/O, reads no clock or
//! randomness, and uses no floating-point arithmetic after input conversion, so
//! native and `wasm32-unknown-unknown` builds produce identical bytes. Chain
//! submission belongs to the chain adapters, not here.

pub mod artifact;
mod authorization;
pub mod bigint;
mod codec;
mod error;
mod hash;
mod operation;
mod records;
mod records_v2;
pub mod reward;
pub mod reward_v1;
mod sign;
pub mod submission;
pub mod target_bundle;

#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures;
#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures_reward_v1;

pub use artifact::{
    ArtifactFile, ArtifactHeader, ArtifactReader, FileRole, MAX_ARTIFACT_HEADER_LEN,
    VerifiedArtifact, artifact_object,
};
pub use authorization::{
    ActiveTarget, CheckedAuthorization, CheckedRenewal, CheckedRevocation, LAUNCH_RENEW_BY_TEMPOS,
    LaunchAuthorization, RewardIntentAuthorization, RewardIntentRenewal, RewardIntentRevocation,
};
pub use target_bundle::{AuthorizationTerms, Authorizing, Carrier, Source, VerifiedCarrier, VerifiedSource, VerifiedTarget};
pub use codec::{
    Canonical, Decoder, Encoder, MAX_PREALLOC_BYTES, MAX_SEQUENCE_LEN, MAX_STRING_LEN,
};
pub use ed25519_dalek::SigningKey;
pub use error::{Error, Result};
pub use hash::{Digest, domain_hash, object_sha256};
pub use operation::{
    WEIGHT_OPERATION_DOMAIN, WEIGHT_OPERATION_PREFIX, WEIGHT_OPERATION_V2_PREFIX,
    WEIGHT_TEMPO_OPERATION_DOMAIN, WEIGHT_TEMPO_OPERATION_PREFIX, WeightOperation,
    WeightOperationV2, weight_operation_id, weight_tempo_operation_id,
};
pub use records::{
    AccountId, ArtifactRecord, Assignment, Attempt, AttemptReceipt, AttemptResult,
    CalculationTarget, ChainBinding, CohortManifest, FINNEY_GENESIS, MinerScore, NeuronIdentity,
    Outcome, OutcomeClass, PROBABILITY_SCALE, Probability, ReceiptIndex, ReceiptIndexEntry, Record,
    RegistrationSnapshot, RewardRule, RowEntry, SCORE_SCALE, Score, ScorePackage, TESTNET_GENESIS,
    WeightRow, admission_commitment, result_commitment,
};
pub use records_v2::{
    BAND_SIGMAS, BatchDesign, CalculationTargetV2, CanaryAudit, CanaryClearance, ClusterCredit,
    CohortManifestV2, CohortSchedule, Component, ComponentRole, CutoffMode, DECAY_BPS,
    DistanceMonitor, EntrantFingerprint, EntrantScore, FALLBACK_BPS, Fingerprint, FingerprintIndex,
    Gate, GateFailure, HUMAN_FPR_LOOSE_REASON, IMPROVEMENT_DIVISOR, InferenceContent,
    LAUNCH_ORIGIN_MIX, MAINNET_MIN_CASES, MARGIN_FLOOR, MARGIN_SIGMAS, MAX_PROBE_SIZE, MAX_ROSTER,
    NEAR_COPY_PPB, NEAR_EQUAL_DIVISOR, NearStats, Nonce, Origin, OriginMix, ProbeBinding,
    ProbeMatch, PromotionAudit, PublicCheck, PublicCopy, PublishedContents, PublishedIndex,
    PublishedIndexBinding, ROW_TOTAL, ReferenceModel, ReferenceScore, ReferenceSet,
    RegistrationSnapshotV2, RewardV1Params, RosterEntry, ScorePackageV2, Stratum, TargetSource,
    WeightRowV2, canonical_fingerprints,
};
pub use sign::{KeyRole, MAX_SIGNED_RECORD_LEN, Signed, TrustedKeys, sign, signing_payload};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_reward_v1;
#[cfg(test)]
mod tests_review;
