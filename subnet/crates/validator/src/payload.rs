//! Weight-commit call and signing-payload construction without a private key.
//!
//! The call is composed with rtao's builder, exactly as its weight scheduler
//! composes it, and then checked by name against live metadata through the
//! adapter's `check_live_call` (the same check the signing guard runs). The
//! signing payload is `call ++ extra ++ additional` as the live runtime's
//! extensions declare it. Nothing here compares a runtime version: the spec
//! and transaction versions inside `additional` are whatever the runtime handle
//! carries, which rtao reads from the chain.

use anyhow::{Context, Result};
use rtao_core::extrinsic::TxParams;
use rtao_primitives::{MechId, NetUid};
use rtao_runtime::{CallBytes, RuntimeHandle};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Compose `SubtensorModule.commit_timelocked_mechanism_weights` and type-check
/// it against `runtime` by name.
pub fn timelocked_commit_call(
    runtime: &RuntimeHandle,
    netuid: NetUid,
    mechanism: MechId,
    commit: Vec<u8>,
    reveal_round: u64,
    commit_reveal_version: u16,
) -> Result<CallBytes> {
    let call = rtao_runtime::calls::subtensor_module::commit_timelocked_mechanism_weights(
        netuid,
        mechanism,
        commit,
        reveal_round,
        commit_reveal_version,
    )
    .compose(runtime)
    .map_err(|error| anyhow::anyhow!("upstream_descriptor_refused: {error}"))?;
    slop_ninja_subnet::check_live_call(runtime, &call)
        .map_err(|refusal| anyhow::anyhow!("{}: {}", refusal.code, refusal.detail))?;
    Ok(call)
}

/// The bytes a signer signs (before Substrate's hash-if-longer-than-256 rule).
pub fn signing_payload(runtime: &RuntimeHandle, call: &[u8], params: &TxParams) -> Result<Vec<u8>> {
    runtime
        .core()
        .signature_payload(call, params)
        .map_err(|error| anyhow::anyhow!("{error}"))
        .context("signing payload could not be built")
}

/// The commit tag a dry run prepares and the basis rtao's scheduler has for
/// it (D-086). The dry run follows the stored tag in every case and reports
/// the basis instead of refusing: under unreviewed code the outcome depends
/// on reveal evidence the dry run cannot see.
pub fn dry_run_commit_tag(stored: u16, runtime_code: Option<rtao_primitives::H256>) -> (u16, &'static str) {
    if stored == rtao_neuron::weights::NATIVE_CR_PAYLOAD_VERSION {
        (stored, "native")
    } else if runtime_code
        .and_then(rtao_neuron::weights::reviewed_cr_runtime)
        .is_some()
    {
        (stored, "reviewed_code")
    } else {
        (stored, "unproven_code")
    }
}

/// Content-free description of one prepared, unsigned weight commit.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DryRunReport {
    pub operation_id: String,
    pub row_digest: String,
    /// Canonical `(uid, weight)` entries; public, since the row is derivable.
    pub entries: Vec<(u16, u16)>,
    pub signing_uid: u16,
    pub reveal_round: u64,
    pub commit_bytes: usize,
    pub call_pallet_index: u8,
    pub call_index: u8,
    pub call_sha256: String,
    pub signing_payload_bytes: usize,
    pub signing_payload_sha256: String,
    pub nonce: u64,
    pub era_expiry: Option<u64>,
    /// Timelock encryption is randomized, so both hashes differ on every run.
    pub ciphertext_randomized: bool,
    /// Observation only; never compared.
    pub observed_spec_version: u32,
    /// The commit-reveal payload tag the commit carries: the stored
    /// `CommitRevealWeightsVersion`.
    pub commit_reveal_version: u16,
    /// Why rtao's scheduler would put that tag on the commit
    /// ([`dry_run_commit_tag`]): `native`, `reviewed_code`, or
    /// `unproven_code`, where rtao at this pin follows the tag only after one
    /// of this validator's commitments revealed under the live code and
    /// pauses timelocked commits until then (that evidence lives in rtao's
    /// dispatch journal, out of the dry run's reach).
    pub commit_reveal_tag_basis: &'static str,
    /// The weights version key the commit carries (`weight_version`).
    pub weight_version: u64,
    /// `WeightsVersionKey(netuid)`: 0, or at most `weight_version` (a larger
    /// one refuses the dry run, as rtao refuses it before signing).
    pub required_version_key: u64,
    /// `ValidatorPermit(netuid)` at the signing UID. The chain requires it of
    /// this row at commit and again at reveal unless the signer is the owner
    /// validator or the row is its lone self-weight under Yuma; a burn row
    /// that would need it and lacks it is deferred (`burn_row_no_permit`)
    /// before this report.
    pub validator_permit: bool,
    /// `SubnetEpochConsensus(netuid)` is `Null` (every signer needs a permit).
    pub null_consensus: bool,
    /// `MaxAllowedValidators(netuid)`, which bounds the permits; `None` when
    /// the read failed.
    pub max_allowed_validators: Option<u16>,
    /// `StakeThreshold`, the chain's minimum stake weight for a validator
    /// that is not the owner (`check_weights_min_stake`, again at reveal);
    /// `None` when the read failed.
    pub stake_threshold: Option<u64>,
    /// `TotalHotkeyAlpha(hotkey, netuid)`: the hotkey's own alpha on the
    /// subnet. The chain compares its stake weight, which also counts
    /// inherited alpha and weighted TAO, so this is context for
    /// `stake_threshold`, not the chain's figure. `None` when the read failed.
    pub own_alpha_stake: Option<u64>,
    pub signed: bool,
}
