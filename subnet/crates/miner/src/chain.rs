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

use anyhow::{Context, Result, bail};
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
    Ok(Client::connect(client_config(config)?).await?)
}

/// The rtao client configuration [`connect`] dials with. Its WebSocket
/// handshake names [`crate::client::USER_AGENT`]: RPC endpoints behind
/// Cloudflare's Browser Integrity Check (rtao.io) refuse a handshake with no
/// agent or a library default (error 1010).
pub fn client_config(config: &ChainConfig) -> Result<ClientConfig> {
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
    client.session.headers.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static(crate::client::USER_AGENT),
    );
    client.config_store = None;
    client.track_heads = false;
    client.startup_timeout = config.startup_timeout;
    Ok(client)
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
/// inclusion and reads the commitment back from finalized state (again each
/// block until it shows, within `deadline`). Once inclusion is finalized the
/// submission is returned whatever the read-back shows: when it gives up,
/// `finalized_commitment_block` stays `None` and the caller reports that,
/// but the inclusion record is never discarded.
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
    era_blocks: Option<u32>,
) -> Result<Submitted> {
    use rtao_signer::KeyView as _;
    let started = std::time::Instant::now();
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
    // A shorter mortal era than rtao's default when the caller bounds when
    // the call may still land (`resolve`, before a cutoff).
    let options = rtao_tx::ExecuteOptions {
        period: era_blocks.or(options.period),
        ..options
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
    // The node answering the finalized read can lag the one that reported
    // finalization: read again once a block until the commitment shows,
    // within what is left of the caller's deadline.
    loop {
        // A failed read is not yet an answer either.
        let read = finalized_commitment(client, netuid, &AccountId(account.0))
            .await
            .unwrap_or(None);
        match readback(read, payload) {
            Readback::Landed(block) => {
                submitted.finalized_commitment_block = Some(block);
                return Ok(submitted);
            }
            Readback::NotYet(why) => {
                if started.elapsed() + READBACK_SPACING >= deadline {
                    eprintln!("warning: {why} after inclusion, read back until the deadline");
                    return Ok(submitted);
                }
                tokio::time::sleep(READBACK_SPACING).await;
            }
        }
    }
}

/// The chain access `resolve` uses: rtao, connected on first use and again
/// after a failed read (RPC outage or lag), with the wallet's hotkey file
/// for the commitment.
pub struct RtaoChain {
    pub config: ChainConfig,
    pub hotkey: rtao_wallet::HotkeyFile,
    /// Bound on one commitment's signing, submission and finalization.
    pub deadline: Duration,
    /// The commitment's mortal era: it can land only within this many blocks
    /// of the finalized head it was signed at.
    pub era_blocks: u32,
    /// The subnet the coordinator serves: set from its cohort read, and
    /// nothing is read or submitted before it is known.
    netuid: std::sync::Mutex<Option<u16>>,
    client: std::sync::Mutex<Option<Client>>,
}

impl RtaoChain {
    pub fn new(
        config: ChainConfig,
        netuid: Option<u16>,
        hotkey: rtao_wallet::HotkeyFile,
        deadline: Duration,
        era_blocks: u32,
    ) -> Self {
        Self {
            config,
            hotkey,
            deadline,
            era_blocks,
            netuid: std::sync::Mutex::new(netuid),
            client: std::sync::Mutex::new(None),
        }
    }

    fn netuid(&self) -> Result<u16> {
        self.netuid
            .lock()
            .map_err(|_| anyhow::anyhow!("chain lock"))?
            .context("the subnet is not known until the coordinator's cohort read answers")
    }

    async fn client(&self) -> Result<Client> {
        if let Some(client) = self
            .client
            .lock()
            .map_err(|_| anyhow::anyhow!("chain lock"))?
            .clone()
        {
            return Ok(client);
        }
        let client = connect(&self.config).await?;
        *self
            .client
            .lock()
            .map_err(|_| anyhow::anyhow!("chain lock"))? = Some(client.clone());
        Ok(client)
    }

    fn forget(&self) {
        if let Ok(mut guard) = self.client.lock() {
            *guard = None;
        }
    }
}

impl crate::resolve::ChainAccess for RtaoChain {
    fn bind(&self, netuid: u16) {
        if let Ok(mut guard) = self.netuid.lock() {
            *guard = Some(netuid);
        }
    }

    async fn facts(&self, hotkey: &AccountId) -> Result<crate::intake::ChainFacts> {
        let netuid = self.netuid()?;
        let client = self.client().await?;
        let read = async {
            let (block, registration) = registration(&client, netuid, hotkey).await?;
            let commitment = finalized_commitment(&client, netuid, hotkey).await?;
            anyhow::Ok(crate::intake::ChainFacts {
                genesis: Some(Digest(client.genesis().0)),
                finalized_block: block,
                registration,
                commitment,
            })
        };
        match read.await {
            Ok(facts) => Ok(facts),
            Err(error) => {
                self.forget();
                Err(error)
            }
        }
    }

    async fn submit(&self, payload: &[u8; SUBMISSION_PAYLOAD_LEN]) -> Result<Submitted> {
        let netuid = self.netuid()?;
        let client = self.client().await?;
        submit_commitment(
            &client,
            &self.hotkey,
            netuid,
            payload,
            self.deadline,
            Some(self.era_blocks),
        )
        .await
    }
}

/// How close to the cutoff `resolve` may still commit, from its submission
/// deadline: `margin_blocks`, the deadline at the nominal block time (at
/// least 8), and the commitment's mortal era `era_blocks`, the largest power
/// of two at most half the margin (at least 4; rtao rounds an era up to a
/// power of two). `resolve` commits only while the cutoff's earliest block
/// is more than the margin past the finalized head, and the era is anchored
/// at or before the finalized head at signing, so the call expires before
/// the cutoff unless the head moves on by more than half the margin between
/// that check (made again just before signing) and the signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitWindow {
    pub margin_blocks: u64,
    pub era_blocks: u32,
}

pub fn commit_window(deadline_secs: u64) -> CommitWindow {
    let margin_blocks = deadline_secs
        .div_ceil(READBACK_SPACING.as_secs().max(1))
        .max(8);
    let half = margin_blocks / 2;
    let era = 1u64 << (63 - half.leading_zeros());
    CommitWindow {
        margin_blocks,
        era_blocks: u32::try_from(era.clamp(4, 65_536)).unwrap_or(65_536),
    }
}

/// The spacing of finalized read-backs after inclusion: Subtensor's block
/// time, so each read can see a newer finalized block.
pub const READBACK_SPACING: Duration = Duration::from_secs(12);

/// What one finalized read-back after inclusion shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readback {
    /// The finalized state holds this payload, set at that block.
    Landed(u64),
    /// Not yet visible to the node that answered: read again.
    NotYet(&'static str),
}

/// Judges one finalized read-back of the hotkey's commitment against the
/// submitted payload. Nothing, or another (older) commitment, is not yet a
/// failure: the answering node may not have reached the finalized block
/// that included it.
pub fn readback(read: Option<(u64, Vec<u8>)>, payload: &[u8]) -> Readback {
    match read {
        Some((block, stored)) if stored == payload => Readback::Landed(block),
        Some(_) => Readback::NotYet("finalized state holds another commitment for this hotkey"),
        None => Readback::NotYet("no commitment in finalized state"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A read-back right after inclusion that does not yet show the
    /// commitment (or still shows an older one) is read again, never a
    /// failure by itself (W4: a landed commitment was reported missing).
    #[test]
    fn a_lagging_finalized_readback_is_read_again() {
        let payload = [7u8; SUBMISSION_PAYLOAD_LEN];
        assert_eq!(
            readback(None, &payload),
            Readback::NotYet("no commitment in finalized state")
        );
        assert!(matches!(
            readback(Some((10, vec![1u8; SUBMISSION_PAYLOAD_LEN])), &payload),
            Readback::NotYet(_)
        ));
        assert_eq!(
            readback(Some((11, payload.to_vec())), &payload),
            Readback::Landed(11)
        );
        assert!(READBACK_SPACING >= Duration::from_secs(1));
    }

    #[test]
    fn the_chain_handshake_names_the_miner() {
        let config = ChainConfig {
            network: "test".into(),
            endpoints: vec![url::Url::parse("wss://test.finney.opentensor.ai:443").unwrap()],
            requests_per_minute: Some(15),
            startup_timeout: Duration::from_secs(5),
        };
        let client = client_config(&config).unwrap();
        let ua = client.session.headers.get(reqwest::header::USER_AGENT).unwrap().to_str().unwrap();
        assert_eq!(ua, crate::client::USER_AGENT);
        assert_eq!(ua, concat!("slop_ninja_miner/", env!("CARGO_PKG_VERSION")));
        // Not a library default that a Browser Integrity Check refuses.
        let lower = ua.to_ascii_lowercase();
        assert!(
            ["python", "urllib", "curl", "reqwest", "tungstenite"]
                .iter()
                .all(|l| !lower.contains(l))
        );
        // The default endpoints get the same header.
        let defaults = client_config(&ChainConfig { endpoints: Vec::new(), ..config }).unwrap();
        assert_eq!(defaults.session.headers.get(reqwest::header::USER_AGENT).unwrap(), ua);
    }
}
