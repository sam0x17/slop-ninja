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
    pub signed: bool,
}
