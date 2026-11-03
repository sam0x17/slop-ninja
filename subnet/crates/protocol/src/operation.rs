//! Stable identity of one native weight operation.
//!
//! [`weight_operation_id`] is the native adapter's existing journal identity
//! for `testnet-reward-v0`, so journals written before this crate existed keep
//! their operation IDs. Its `slopninja` spelling is a retained compatibility
//! identifier. [`weight_tempo_operation_id`] names the per-tempo re-commit of
//! a `testnet-reward-v0` target. [`WeightOperationV2`] is the per-tempo
//! `reward-v1` identity: it binds the active target and the chain's epoch
//! index, so each tempo's re-commit of one target is a distinct operation.

use sha2::{Digest as _, Sha256};

use crate::codec::{Canonical as _, Encoder, canonical_struct};
use crate::hash::{domain_hash, hex};
use crate::records::{CalculationTarget, validate_label};
use crate::records_v2::CalculationTargetV2;
use crate::{AccountId, Digest, Record as _, Result};

pub const WEIGHT_OPERATION_DOMAIN: &[u8] = b"slopninja.weight-operation.v1\0";
pub const WEIGHT_OPERATION_PREFIX: &str = "slopninja:weights:v1:";
/// Domain of a per-tempo re-commit of one weight operation's target.
pub const WEIGHT_TEMPO_OPERATION_DOMAIN: &str = "slop_ninja.protocol/weight-tempo-operation/v1";
pub const WEIGHT_TEMPO_OPERATION_PREFIX: &str = "slop_ninja:weights:tempo:v1:";

/// Immutable application meaning of a weight operation. The row is excluded,
/// so changed input under the same identity conflicts with the original work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeightOperation {
    pub genesis: Digest,
    pub netuid: u16,
    pub hotkey: AccountId,
    /// Digest of the frozen cohort manifest, not an operational build hash.
    pub manifest: Digest,
    pub cohort_label: String,
    pub revision: u64,
    pub mechanism: u8,
}

impl WeightOperation {
    /// The operation a validator `hotkey` performs for a calculation target.
    pub fn for_target(target: &CalculationTarget, hotkey: AccountId) -> Self {
        Self {
            genesis: target.chain.genesis,
            netuid: target.chain.netuid,
            hotkey,
            manifest: target.cohort_manifest,
            cohort_label: target.cohort_label.clone(),
            revision: target.revision,
            mechanism: target.chain.mechanism,
        }
    }

    pub fn id(&self) -> Result<String> {
        weight_operation_id(
            &self.genesis.0,
            self.netuid,
            &self.hotkey.0,
            &self.manifest.0,
            &self.cohort_label,
            self.revision,
            self.mechanism,
        )
    }
}

/// `slopninja:weights:v1:` followed by the hex SHA-256 of the domain, genesis,
/// big-endian netuid, hotkey, manifest digest, big-endian `u64` cohort label
/// length, label bytes, big-endian revision and mechanism byte.
pub fn weight_operation_id(
    genesis: &[u8; 32],
    netuid: u16,
    hotkey: &[u8; 32],
    manifest: &[u8; 32],
    cohort_label: &str,
    revision: u64,
    mechanism: u8,
) -> Result<String> {
    validate_label(cohort_label)?;
    let mut hash = Sha256::new();
    hash.update(WEIGHT_OPERATION_DOMAIN);
    hash.update(genesis);
    hash.update(netuid.to_be_bytes());
    hash.update(hotkey);
    hash.update(manifest);
    hash.update((cohort_label.len() as u64).to_be_bytes());
    hash.update(cohort_label.as_bytes());
    hash.update(revision.to_be_bytes());
    hash.update([mechanism]);
    Ok(format!(
        "{WEIGHT_OPERATION_PREFIX}{}",
        hex(&hash.finalize())
    ))
}

/// Identity of the re-commit of a target in one subnet epoch (plan decision
/// 12: re-commit the current target once per tempo until a successor exists).
///
/// `base` is the target's [`weight_operation_id`], and `epoch` is the
/// `SubnetEpochIndex` the re-commit is scheduled for. The identity is
/// `slop_ninja:weights:tempo:v1:` followed by the hex
/// `domain_hash(WEIGHT_TEMPO_OPERATION_DOMAIN, u64_be(len(base)) || base || u64_be(epoch))`.
/// Distinct epochs of one target, and the same epoch of distinct targets or
/// validators, never share an identity; the row stays excluded as in the base.
pub fn weight_tempo_operation_id(base: &str, epoch: u64) -> Result<String> {
    let digest = base.strip_prefix(WEIGHT_OPERATION_PREFIX).filter(|hash| {
        hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    });
    if digest.is_none() {
        return Err(crate::Error::Invalid("base_operation_id"));
    }
    let mut payload = Vec::with_capacity(8 + base.len() + 8);
    payload.extend_from_slice(&(base.len() as u64).to_be_bytes());
    payload.extend_from_slice(base.as_bytes());
    payload.extend_from_slice(&epoch.to_be_bytes());
    Ok(format!(
        "{WEIGHT_TEMPO_OPERATION_PREFIX}{}",
        domain_hash(WEIGHT_TEMPO_OPERATION_DOMAIN, &payload).hex()
    ))
}

/// Prefix of a per-tempo `reward-v1` weight operation identity.
pub const WEIGHT_OPERATION_V2_PREFIX: &str = "slop_ninja:weights:v2:";
const WEIGHT_OPERATION_V2_DOMAIN: &str = "slop_ninja.protocol/weight-operation/v2";

canonical_struct! {
    /// One validator's commitment of the active target's row in one tempo.
    /// The epoch index makes each tempo's re-commit of the same target a
    /// distinct operation; the row is excluded, so a changed row under the
    /// same identity conflicts with the original work.
    pub struct WeightOperationV2 {
        pub genesis: Digest,
        pub netuid: u16,
        pub mechanism: u8,
        pub hotkey: AccountId,
        /// Digest of the active calculation target, including the launch burn row.
        pub target: Digest,
        /// The chain's epoch index of the tempo.
        pub epoch: u64,
    }
}

impl WeightOperationV2 {
    /// The identity of the operation a validator `hotkey` performs for
    /// `target` at `epoch`. It authorizes nothing: a new commitment goes
    /// through [`crate::CheckedAuthorization::operation`], which needs an
    /// authorization checked against its carrying cohort and checks the
    /// target's activation and the reward-intent lease. Reconciling an
    /// already-submitted commitment recomputes its identity here.
    pub fn for_target(target: &CalculationTargetV2, hotkey: AccountId, epoch: u64) -> Result<Self> {
        Ok(Self {
            genesis: target.chain.genesis,
            netuid: target.chain.netuid,
            mechanism: target.chain.mechanism,
            hotkey,
            target: target.digest()?,
            epoch,
        })
    }

    /// `slop_ninja:weights:v2:` followed by the hex domain hash of the
    /// canonical encoding.
    pub fn id(&self) -> String {
        let mut e = Encoder::new();
        self.encode_to(&mut e);
        format!(
            "{WEIGHT_OPERATION_V2_PREFIX}{}",
            domain_hash(WEIGHT_OPERATION_V2_DOMAIN, &e.into_bytes())
        )
    }
}
