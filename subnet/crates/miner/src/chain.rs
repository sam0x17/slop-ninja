//! The on-chain commitment through rtao.
//!
//! [`commitment_call`] builds `Commitments.set_commitment(netuid,
//! CommitmentInfo { fields: [Raw44(payload)] })` with rtao's generated
//! builder; composition resolves it by name in the metadata the chain serves.
//! Tests compose it against recorded metadata and never submit it.
//!
//! The live functions read finalized state and submit with the hotkey only.
//! They are not exercised by automated tests: no test connects to a chain.

use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use rtao_client::{BlockRef, Client, ClientConfig, Network, ViewExt as _};
use rtao_primitives::NetUid;
use rtao_runtime::models::{CommitmentInfo, CommitmentRegistration, Data};
use rtao_runtime::storage::commitments::COMMITMENT_OF;
use rtao_runtime::storage::subtensor_module as st;
use rtao_runtime::{Call, CallBytes, RuntimeHandle};
use rtao_signer::SignerRole;
use slop_ninja_protocol::submission::SUBMISSION_PAYLOAD_LEN;
use slop_ninja_protocol::{AccountId, ChainBinding, Digest, NeuronIdentity};

use crate::opening::Submitted;

/// The commitment field: the 44 payload bytes as the length-indexed raw
/// variant, so finalized state holds exactly those bytes.
pub fn commitment_info(payload: &[u8; SUBMISSION_PAYLOAD_LEN]) -> CommitmentInfo {
    CommitmentInfo {
        fields: vec![Data::Raw(payload.to_vec())],
    }
}

/// `Commitments.set_commitment` for `payload` on `netuid`, built without
/// encoding or I/O.
pub fn commitment_call(netuid: u16, payload: &[u8; SUBMISSION_PAYLOAD_LEN]) -> Call {
    rtao_runtime::calls::commitments::set_commitment(NetUid(netuid), commitment_info(payload))
}

/// Composes the call against `runtime`'s metadata.
pub fn compose(call: &Call, runtime: &RuntimeHandle) -> Result<CallBytes> {
    call.compose(runtime)
        .map_err(|error| anyhow::anyhow!("compose set_commitment: {error}"))
}

/// The committed bytes of a finalized registration, when it holds exactly one
/// raw field.
pub fn registration_payload(registration: &CommitmentRegistration) -> Option<&[u8]> {
    match registration.info.fields.as_slice() {
        [Data::Raw(bytes)] => Some(bytes),
        _ => None,
    }
}

/// How to reach the chain.
#[derive(Clone, Debug)]
pub struct ChainConfig {
    /// rtao network label, `test` or `finney`.
    pub network: String,
    /// Explicit RPC endpoints; empty uses the network's defaults.
    pub endpoints: Vec<url::Url>,
    /// Client-side request budget, when the endpoint publishes one.
    pub requests_per_minute: Option<u32>,
    pub startup_timeout: Duration,
}

pub async fn connect(config: &ChainConfig) -> Result<Client> {
    let network = Network::resolve_with(&config.network, |_| None)
        .with_context(|| format!("unknown network {:?}", config.network))?;
    let mut client = ClientConfig::new(network)?;
    let mut urls = config.endpoints.iter().cloned();
    if let Some(primary) = urls.next() {
        client.session.pool = rtao_rpc::EndpointPool::new(primary).with_fallbacks(urls);
    }
    if let Some(per_minute) = config.requests_per_minute {
        client.session.rate_limit = Some(rtao_rpc::RateLimit {
            per_second: f64::from(per_minute) / 60.0,
            burst: per_minute.clamp(1, 3),
        });
    }
    client.session.plaintext_ws = rtao_rpc::PlaintextPolicy::LoopbackOnly;
    client.config_store = None;
    client.track_heads = false;
    client.startup_timeout = config.startup_timeout;
    Ok(Client::connect(client).await?)
}

/// This chain's binding for `netuid` and `mechanism`.
pub fn chain_binding(client: &Client, netuid: u16, mechanism: u8) -> ChainBinding {
    ChainBinding {
        genesis: Digest(client.genesis().0),
        netuid,
        mechanism,
    }
}

/// The hotkey's registration at the finalized head, and that head's number.
pub async fn registration(
    client: &Client,
    netuid: u16,
    hotkey: &AccountId,
) -> Result<(u64, Option<NeuronIdentity>)> {
    let finalized = client.at(BlockRef::Finalized).await?;
    let account = rtao_primitives::AccountId(hotkey.0);
    let uid = finalized
        .storage(&st::UIDS, (NetUid(netuid), account))
        .await?;
    let block = finalized.block().number;
    let Some(uid) = uid else {
        return Ok((block, None));
    };
    let registration_block = finalized
        .storage(&st::BLOCK_AT_REGISTRATION, (NetUid(netuid), uid))
        .await?;
    Ok((
        block,
        Some(NeuronIdentity {
            uid,
            hotkey: *hotkey,
            registration_block,
        }),
    ))
}

/// The hotkey's commitment in finalized state: its block and payload.
pub async fn finalized_commitment(
    client: &Client,
    netuid: u16,
    hotkey: &AccountId,
) -> Result<Option<(u64, Vec<u8>)>> {
    let finalized = client.at(BlockRef::Finalized).await?;
    let account = rtao_primitives::AccountId(hotkey.0);
    let Some(registration) = finalized
        .storage(&COMMITMENT_OF, (NetUid(netuid), account))
        .await?
    else {
        return Ok(None);
    };
    let payload = registration_payload(&registration)
        .map(<[u8]>::to_vec)
        .unwrap_or_default();
    Ok(Some((u64::from(registration.block), payload)))
}

/// Signs and submits the commitment with the hotkey, waits for finalized
/// inclusion and reads the commitment back from finalized state.
///
/// rtao has no typed intent for `set_commitment`, so it goes out as a raw
/// call. rtao applies only its raw-call opt-in to raw calls: spend caps and
/// netuid allow-lists are not checked for them. What protects the miner is
/// that the call is composed here from the payload and pinned with
/// `expected_call`, so rtao signs exactly these bytes and nothing else. The
/// chain charges no fee for `set_commitment` (`Pays::No`); it limits each
/// account's commitment space per epoch (`UsedSpaceOf` against `MaxSpace`) and
/// fails a call past it with `SpaceLimitExceeded`.
pub async fn submit_commitment(
    client: &Client,
    hotkey: &rtao_wallet::HotkeyFile,
    netuid: u16,
    payload: &[u8; SUBMISSION_PAYLOAD_LEN],
    deadline: Duration,
) -> Result<Submitted> {
    use rtao_signer::KeyView as _;
    let account = hotkey.hotkey_address()?;
    let runtime = client.refresh_runtime().await?;
    let call = compose(&commitment_call(netuid, payload), &runtime)?;
    let executor = rtao_intents::executor(client.clone()).with_policy(rtao_tx::Policy {
        allow_raw_calls: true,
        ..Default::default()
    });
    let options = rtao_tx::ExecuteOptions {
        expected_call: Some(call.clone()),
        wait_for_inclusion: true,
        wait_for_finalization: true,
        deadline: Some(deadline),
        ..Default::default()
    };
    let result = executor
        .submit_call_dyn(call, hotkey, SignerRole::Hotkey, &options)
        .await?;
    if !result.success {
        bail!("set_commitment failed: {}", result.message);
    }
    let mut submitted = Submitted {
        extrinsic_hash: result
            .extrinsic_hash
            .map(|h| format!("0x{}", hex::encode(h.0))),
        block_hash: result.block_hash.map(|h| format!("0x{}", hex::encode(h.0))),
        extrinsic_id: result.extrinsic_id.clone(),
        finalized_commitment_block: None,
    };
    let readback = finalized_commitment(client, netuid, &AccountId(account.0)).await?;
    let (block, stored) = readback.context("no commitment in finalized state after inclusion")?;
    ensure!(
        stored == payload,
        "finalized state holds another commitment for this hotkey"
    );
    submitted.finalized_commitment_block = Some(block);
    Ok(submitted)
}
