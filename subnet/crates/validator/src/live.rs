//! Live ports over the native adapter and an rtao client.
//!
//! Startup and the dry run load no private key: the hotkey identity comes from
//! the wallet's public `pub.txt` companion, recovery is signer-free, and the
//! dry run prepares an unsigned payload. Only `run --dispatch` opens the
//! configured hotkey file, after startup and package verification passed.

use std::collections::BTreeMap;
use std::{path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use futures::{FutureExt as _, StreamExt as _};
use rtao_client::{
    Backfill, BlockHeader, BlockRef, BlockStream, Client, ClientConfig, Finality, Mortality,
    Network, PublicPrepareOptions, SubmitOptions, ViewExt,
};
use rtao_core::codec::Value;
use rtao_intents::weights::{
    CommitSizeLimit, ConformState, TimelockCommitRequest, TimelockCommitter, conform,
};
use rtao_neuron::weights::{NATIVE_CR_PAYLOAD_VERSION, WeightPreflight};
use rtao_neuron::{ChainClock, ClockGuard, EpochClock, EpochStatus, Metrics, NeuronCx};
use rtao_primitives::{AccountId, H256, IntoCoreValue as _, NetUid, Uid};
use rtao_rpc::{EndpointPool, RateLimit};
use rtao_runtime::storage::subtensor_module as st;
use rtao_signer::KeyView as _;
use slop_ninja_protocol::{ChainBinding, NeuronIdentity, RegistrationSnapshot, TrustedKeys};
use slop_ninja_subnet::{
    ChainIdentity, Enqueued, NativeWeightAdapter, OwnerRowPolicy, SchedulerConfig, Shielding,
    WeightWork,
};

use crate::app::{AppPorts, CommitRecord, DispatchReport, Mode, OwnerRowChoice, WeightLimits};
use crate::config::{Recommit, ValidatorConfig};
use crate::fetch::{BundleInfo, HttpTransport, Outcome, Session, Staged};
use crate::reveals::{Check, Completed, OpState, RevealLedger};
use crate::supersede::{Candidate, Decision, FetchState, InstallPorts, LoopPorts, LoopStep, Pending};
use crate::faults::{Faults, Phase};
use crate::guard::{Authorized, PACKAGE_CONTRACT, PackageGuard};
use crate::package::{self, Derived, Refusal};
use crate::payload::{self, DryRunReport};
use crate::startup::{FinalizedHead, RecoveryCounts, StartupPorts, SubnetState, SurfaceStatus};

/// Largest subnet population read into a registration snapshot.
const MAX_SNAPSHOT_UIDS: u128 = 4096;
/// Mortal era of weight commits, as rtao's scheduler signs them.
const COMMIT_ERA_PERIOD: u64 = 128;

/// Key arguments of the `SubtensorModule` storage items read by name. Live
/// metadata supplies each key's type and hasher; these supply only the values,
/// in the dynamic encoder's canonical integer form (`Value::Int`, which it
/// narrows to the key's declared width and unwraps through newtypes such as
/// `NetUid` and `NetUidStorageIndex`). `storage_tests` pins every key built
/// here against storage keys built independently of rtao.
pub mod keys {
    use anyhow::{Context, Result};
    use rtao_core::codec::Value;
    use rtao_primitives::{AccountId, IntoCoreValue, NetUid};

    /// `NetUidStorageIndex` of a mechanism, as subtensor's
    /// `get_mechanism_storage_index` computes it: `mechanism * 4096 + netuid`.
    pub fn storage_index(netuid: NetUid, mechanism: u8) -> Result<u16> {
        u16::from(mechanism)
            .checked_mul(4096)
            .and_then(|base| base.checked_add(netuid.0))
            .context("storage index overflows")
    }

    /// `(netuid)`: `NetworksAdded`, `CommitRevealWeightsEnabled`,
    /// `SubnetworkN`, `SubnetOwnerHotkey`, `RevealPeriodEpochs`.
    pub fn subnet(netuid: NetUid) -> Vec<Value> {
        vec![netuid.into_core_value()]
    }

    /// `(netuid, hotkey)`: `Uids`.
    pub fn hotkey(netuid: NetUid, hotkey: AccountId) -> Vec<Value> {
        vec![netuid.into_core_value(), hotkey.into_core_value()]
    }

    /// `(netuid, uid)`: `Keys`, `BlockAtRegistration`.
    pub fn neuron(netuid: NetUid, uid: u16) -> Vec<Value> {
        vec![netuid.into_core_value(), uid.into_core_value()]
    }

    /// `(storage index, uid)`: `Weights`.
    pub fn weights(netuid: NetUid, mechanism: u8, uid: u16) -> Result<Vec<Value>> {
        Ok(vec![
            storage_index(netuid, mechanism)?.into_core_value(),
            uid.into_core_value(),
        ])
    }

    /// `(storage index, commit epoch)`: `TimelockedWeightCommits`.
    pub fn timelocked_commits(netuid: NetUid, mechanism: u8, epoch: u64) -> Result<Vec<Value>> {
        Ok(vec![
            storage_index(netuid, mechanism)?.into_core_value(),
            epoch.into_core_value(),
        ])
    }
}

/// Read the hotkey address from `<hotkey>pub.txt` only. Unlike rtao's wallet
/// fallback, a missing companion is an error: the keyfile itself is never read.
pub fn public_hotkey(config: &ValidatorConfig) -> Result<AccountId> {
    let mut path = config.wallet.hotkey_file().into_os_string();
    path.push("pub.txt");
    let text = std::fs::read_to_string(&path)
        .context("cannot read the hotkey's public companion <hotkey>pub.txt")?;
    let fields: serde_json::Value =
        serde_json::from_str(&text).context("hotkey public companion is not JSON")?;
    let address: AccountId = fields
        .get("ss58Address")
        .and_then(serde_json::Value::as_str)
        .context("hotkey public companion has no ss58Address")?
        .parse()
        .context("hotkey public companion has an invalid ss58Address")?;
    if let Some(public) = fields.get("publicKey").and_then(serde_json::Value::as_str) {
        let mut bytes = [0; 32];
        hex::decode_to_slice(public.trim_start_matches("0x"), &mut bytes)
            .context("hotkey public companion has an invalid publicKey")?;
        ensure!(
            bytes == address.0,
            "hotkey public companion fields disagree"
        );
    }
    Ok(address)
}

/// Resolve the journal identity from configuration and the public hotkey file.
pub fn chain_identity(config: &ValidatorConfig) -> Result<ChainIdentity> {
    let hotkey = public_hotkey(config)?;
    Ok(ChainIdentity {
        genesis: config.network.genesis()?,
        netuid: NetUid(config.netuid),
        hotkey,
    })
}

/// Open the adapter with the package guard. Opening persists pause.
///
/// `clock` is rtao's timelock clock. It starts unanchored. rtao's scheduler
/// anchors it before each timelocked commit from `Timestamp.Now` at the
/// commit's preflight block (D-094); the dispatch loop also observes the best
/// head's timestamp on every iteration. The clock only moves forward.
pub fn open_adapter(
    config: &ValidatorConfig,
    identity: ChainIdentity,
    authorized: Authorized,
    clock: ChainClock,
) -> Result<NativeWeightAdapter> {
    let netuid = identity.netuid;
    NativeWeightAdapter::open(
        &config.journal_path,
        SchedulerConfig {
            identity,
            semantic_contract: PACKAGE_CONTRACT.into(),
            weight_version: config.weight_version,
            cadence: config
                .dispatch
                .as_ref()
                .and_then(|dispatch| dispatch.epoch_offset_blocks)
                .map(|offset_blocks| rtao_neuron::WeightCadence::OncePerEpoch { offset_blocks }),
            shielding: Shielding::Unshielded,
            submit_deadline: Some(weight_submit_deadline(config)),
        },
        Arc::new(PackageGuard { authorized, netuid }),
        Arc::new(clock),
        ClockGuard::default(),
    )
}

/// Calls the signing transport made in rtao's admission window, the most
/// measured on subnet 601 (TESTNET-M2.md section 8): 26 to 28 at the seven
/// testnet commits at rtao `b5553a76` (2026-10-30), counted from the nonce
/// lease with the signing guards' reads and the watch's reads after
/// `author_submitAndWatchExtrinsic`, and 21 at the first burn-row commit at
/// rtao `0c128160` (AR-3, 2026-11-03), counted from the signing transport's
/// first call of the attempt to the submission (20 reads and the submission,
/// in 3.0 s). The larger count is kept; [`UNCOUNTED_ADMISSION_CALLS`] covers
/// paths neither measurement exercised.
pub const MEASURED_ADMISSION_CALLS: u32 = 28;

/// An allowance on top of [`MEASURED_ADMISSION_CALLS`] for calls a window at
/// this pin can make beyond the measured ones: one each for signer
/// resolution, the D-094 `Timestamp.Now` anchor read at the preflight block,
/// the commit-reveal tag-following runtime-code read and a by-name fallback
/// read under descriptor drift (4), and one reconnect inside the window,
/// which rtao retries within the session under `retry_forever` and which
/// makes about 16 calls at once on the signing connection. The first
/// burn-row commit at rtao `0c128160` (AR-3, 2026-11-03) made 21 calls with
/// no reconnect, under the 28 measured; the allowance is kept rather than
/// cut to that count, because a reconnect or a fallback can still fall in
/// any window.
pub const UNCOUNTED_ADMISSION_CALLS: u32 = 4 + 16;

/// rtao's weight admission window for this configuration (D-097).
///
/// The window's calls go through the transport that signs: the separate
/// signing transport when `rpc.signing_requests_per_minute` is set, else the
/// read session, whose budget it then has to itself because the dispatch loop
/// makes no other call while rtao dispatches. In the worst case the window
/// starts with an empty token bucket, which then admits one call every
/// `60 / per_minute` seconds: the window needs one interval for each of the
/// [`MEASURED_ADMISSION_CALLS`] and [`UNCOUNTED_ADMISSION_CALLS`] and one
/// more for the pool's acknowledgment of the last (the measured responses
/// took well under one interval at these budgets). rtao's 30 s default is the
/// floor. At the testnet budgets, 20 calls a minute on the signing transport
/// give 147 s, and 13 a minute on the read session alone 227 s; both fit in
/// the about 492 s between a 320-block offset and the end of a 361-block
/// epoch. A window that proves too short costs no journal state: an attempt
/// that signed nothing is reported `Failed` and retried in the same epoch
/// (D-095), which the dispatch loop alerts on, and one that signed goes
/// uncertain and is recovered. A longer window never shortens the wait for
/// inclusion (a 128-block era after the window); a commit that lands in the
/// next epoch is followed there by rtao's reveal watcher.
pub fn weight_submit_deadline(config: &ValidatorConfig) -> Duration {
    let per_minute = config
        .rpc
        .signing_requests_per_minute
        .unwrap_or(config.rpc.max_requests_per_minute)
        .max(1);
    let calls = u64::from(MEASURED_ADMISSION_CALLS + UNCOUNTED_ADMISSION_CALLS) + 1;
    Duration::from_secs((calls * 60).div_ceil(u64::from(per_minute)))
        .max(rtao_neuron::weights::DEFAULT_WEIGHT_SUBMIT_DEADLINE)
}

/// How the dispatch loop follows finalized heads: the newest head only
/// (rtao D-096). The loop needs one finalized head in each epoch's commit
/// window, not every height: rtao's `OncePerEpoch` cadence is ready at any
/// head at or past the epoch's offset that is in a later epoch than the last
/// attempt, and it reads the epoch from that head's state. The reveal
/// watcher and finalized recovery follow history from their own cursors, so
/// they see every block that can change a pending reveal whichever heads the
/// loop is given. `Latest` delivers the newest head as soon as an iteration
/// ends and makes no request for a skipped height, so it reaches each commit
/// window at least as early as `Complete` or `Bounded(n)` would, which first
/// replay the gap at two requests a height (an hour-long call left about 400
/// heights, 25 minutes at the read budget, on testnet). An epoch's window is
/// missed only if one iteration outlasts it, under any policy.
pub const DISPATCH_BACKFILL: Backfill = Backfill::Latest;

/// The dispatch loop's finalized-head stream ([`DISPATCH_BACKFILL`]).
pub fn finalized_heads(client: &Client) -> Result<BlockStream> {
    Ok(client.blocks_with(Finality::Finalized, DISPATCH_BACKFILL)?)
}

/// Create a new journal. Refuses an existing path so a typo never replaces history.
pub fn init_journal(config: &ValidatorConfig, identity: ChainIdentity) -> Result<()> {
    ensure!(
        !config.journal_path.exists(),
        "journal_path already exists; init never adopts or replaces a journal"
    );
    let parent = config
        .journal_path
        .parent()
        .context("journal_path has no parent directory")?;
    ensure!(
        parent.is_dir(),
        "journal_path's parent directory does not exist"
    );
    drop(open_adapter(
        config,
        identity,
        Authorized::default(),
        ChainClock::unanchored(),
    )?);
    Ok(())
}

pub struct LivePorts {
    config: ValidatorConfig,
    identity: ChainIdentity,
    adapter: Option<NativeWeightAdapter>,
    client: Option<Client>,
    header: Option<BlockHeader>,
    metrics: Metrics,
    authorized: Authorized,
    /// rtao's timelock clock, shared with the adapter; dispatch anchors it.
    clock: ChainClock,
    /// Separate signing transport, when configured (dispatch only).
    signing_client: Option<Client>,
    /// Subscribe to finalized heads; needed only by dispatch.
    track_heads: bool,
    /// SHA-256 of the bundle this process last read or installed.
    installed_sha: Option<String>,
    /// A dry run: never writes the held-target record or the bundle file; a
    /// fetched bundle it would install is staged in memory instead.
    dry_run: bool,
    /// A dry run's fetched bundle, read in place of `[package] path`.
    staged_bundle: Option<Vec<u8>>,
    /// The fetcher, shared by the startup session and the dispatch run's
    /// background task (one session at a time).
    fetcher: Option<Arc<tokio::sync::Mutex<FetchSetup>>>,
    /// Cohorts the installed bundle was built from, which the cache keeps.
    keep_cohorts: Arc<std::sync::Mutex<Vec<String>>>,
    /// A successor the startup fetch found eligible while work was in
    /// flight: the dispatch loop starts waiting for it.
    startup_wait: Option<(Box<Staged>, Decision, u64)>,
    /// The installed bundle in a dispatch run, for supersession decisions.
    loop_installed: Option<InstalledBundle>,
    /// `Tempo(netuid)` read when the signing context was built (dispatch
    /// only); sizes the missed-commit-window alert.
    tempo: Option<u64>,
    /// The revocations last loaded from the store: a store that later
    /// cannot be read as a whole keeps them in force ([`AppPorts::revocations`]).
    /// Entries are only ever added, so they are a subset of the store's.
    revocations_loaded: std::sync::Mutex<Option<Vec<slop_ninja_protocol::RewardIntentRevocation>>>,
}

/// The installed bundle's facts in a dispatch run.
#[derive(Clone, Debug)]
struct InstalledBundle {
    sha256: String,
    info: BundleInfo,
    authorized: Arc<crate::package_v2::Authorized>,
}

/// Seconds between finalized-epoch reads while a continuous run waits out a
/// deferred tempo without `[fetch]`: one read a minute, well inside the
/// default read budget of 15 requests a minute.
const EPOCH_POLL_SECS: u64 = 60;

impl LivePorts {
    pub fn new(config: ValidatorConfig, identity: ChainIdentity) -> Self {
        Self {
            config,
            identity,
            adapter: None,
            client: None,
            header: None,
            metrics: Metrics::default(),
            authorized: Authorized::default(),
            clock: ChainClock::unanchored(),
            signing_client: None,
            track_heads: false,
            installed_sha: None,
            dry_run: false,
            staged_bundle: None,
            fetcher: None,
            keep_cohorts: Arc::new(std::sync::Mutex::new(Vec::new())),
            startup_wait: None,
            loop_installed: None,
            tempo: None,
            revocations_loaded: std::sync::Mutex::new(None),
        }
    }

    /// A dry run: nothing durable is written outside the journal's own
    /// pause, and fetched bundles are staged in memory.
    pub fn with_dry_run(mut self) -> Self {
        self.dry_run = true;
        self
    }

    /// Whether a dry run staged a fetched bundle in memory.
    pub fn has_staged_bundle(&self) -> bool {
        self.staged_bundle.is_some()
    }

    /// Whether a dispatch run is continuous (no `max_finalized_blocks`).
    pub fn continuous(&self) -> bool {
        self.config
            .dispatch
            .as_ref()
            .is_some_and(|dispatch| dispatch.max_finalized_blocks.is_none())
    }

    /// Reads the finalized head again, for an application stage that follows
    /// a switch or a wait within the same process.
    pub async fn refresh_head(&mut self) -> Result<FinalizedHead> {
        StartupPorts::finalized_head(self).await
    }

    /// The finalized head startup read, used for the registration snapshot.
    pub fn finalized(&self) -> Option<FinalizedHead> {
        self.header.as_ref().map(|header| FinalizedHead {
            number: header.number,
            hash: header.hash,
        })
    }

    /// Track finalized heads, for a dispatch run.
    pub fn with_head_tracking(mut self) -> Self {
        self.track_heads = true;
        self
    }

    /// Persist pause if the journal is open. Opening already persisted pause,
    /// so this matters only after a resume.
    pub fn pause(&self) -> Result<()> {
        if let Some(adapter) = &self.adapter {
            adapter.controls().pause()?;
        }
        Ok(())
    }

    pub async fn close(&mut self) {
        if let Some(client) = self.signing_client.take() {
            client.close().await;
        }
        if let Some(client) = self.client.take() {
            client.close().await;
        }
    }

    fn client(&self) -> Result<&Client> {
        self.client.as_ref().context("RPC client is not connected")
    }

    fn adapter_mut(&mut self) -> Result<&mut NativeWeightAdapter> {
        self.adapter.as_mut().context("journal is not open")
    }

    fn header_for(&self, head: &FinalizedHead) -> Result<&BlockHeader> {
        self.header
            .as_ref()
            .filter(|header| header.hash == head.hash)
            .context("finalized head was not read in this process")
    }
}

pub(crate) fn client_config(config: &ValidatorConfig, track_heads: bool) -> Result<ClientConfig> {
    let network = Network::resolve_with(config.network.rtao_label(), |_| None)?;
    let mut client = ClientConfig::new(network)?;
    let mut urls = config
        .rpc
        .endpoints
        .iter()
        .map(|endpoint| url::Url::parse(endpoint))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter();
    let primary = urls.next().context("no RPC endpoint configured")?;
    client.session.pool = EndpointPool::new(primary).with_fallbacks(urls);
    // Keep dialing through endpoint losses and through HTTP 403/429 refusals
    // of a reconnect handshake, each after rtao's bounded wait (D-097): a
    // validator's session must outlast a refusal of any length. Without it a
    // refusal becomes terminal after `max_retries` further refusals.
    client.session.retry_forever = true;
    client.session.rate_limit = Some(RateLimit {
        per_second: f64::from(config.rpc.max_requests_per_minute) / 60.0,
        burst: 3,
    });
    // Plaintext only to a loopback proxy (configuration enforces it too).
    client.session.plaintext_ws = rtao_rpc::PlaintextPolicy::LoopbackOnly;
    client.session.headers.insert(
        http::header::USER_AGENT,
        http::HeaderValue::from_str(&config.rpc.user_agent)?,
    );
    client.runtime_cache_dir = config.rpc.runtime_cache_dir.clone();
    client.config_store = None;
    client.track_heads = track_heads;
    client.startup_timeout = Duration::from_secs(config.rpc.startup_timeout_secs);
    Ok(client)
}

/// The separate signing transport: the same endpoints and headers with its
/// own budget, burst equal to a minute of it, tracking heads so best and
/// finalized reads inside the admission window come from its subscriptions.
pub(crate) fn signing_client_config(config: &ValidatorConfig, per_minute: u32) -> Result<ClientConfig> {
    let mut client = client_config(config, true)?;
    client.session.rate_limit = Some(RateLimit {
        per_second: f64::from(per_minute) / 60.0,
        burst: per_minute,
    });
    Ok(client)
}

fn parse_hash(value: &str) -> Result<H256> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(
        value.strip_prefix("0x").context("block hash lacks 0x")?,
        &mut bytes,
    )
    .context("block hash is not 32 bytes of hex")?;
    Ok(H256(bytes))
}

/// Decode an `AccountId32` storage value read by name. rtao renders byte-like
/// values as hex or SS58 strings, raw bytes, or a newtype around either.
pub fn account_from_value(value: &Value) -> Result<[u8; 32]> {
    match value {
        Value::Str(text) if text.starts_with("0x") => {
            let mut bytes = [0; 32];
            hex::decode_to_slice(&text[2..], &mut bytes).context("account hex is not 32 bytes")?;
            Ok(bytes)
        }
        Value::Str(text) => Ok(text.parse::<AccountId>().context("account is not SS58")?.0),
        Value::Bytes(bytes) => <[u8; 32]>::try_from(bytes.as_slice())
            .ok()
            .context("account bytes are not 32 long"),
        Value::List(items) | Value::Tuple(items) if items.len() == 1 => {
            account_from_value(&items[0])
        }
        Value::List(items) | Value::Tuple(items) if items.len() == 32 => {
            let mut bytes = [0; 32];
            for (slot, item) in bytes.iter_mut().zip(items) {
                *slot = match item {
                    Value::Int(n) => u8::try_from(*n).ok(),
                    Value::Uint(n) => u8::try_from(*n).ok(),
                    _ => None,
                }
                .context("account byte is not an integer in 0..=255")?;
            }
            Ok(bytes)
        }
        _ => bail!("account value has an unexpected shape"),
    }
}

/// Decode a `Vec<(u16, u16)>` weight row read by name.
pub fn weights_from_value(value: &Value) -> Result<Vec<(u16, u16)>> {
    let integer = |value: &Value| -> Option<u16> {
        match value {
            Value::Int(n) => u16::try_from(*n).ok(),
            Value::Uint(n) => u16::try_from(*n).ok(),
            Value::List(items) | Value::Tuple(items) if items.len() == 1 => match &items[0] {
                Value::Int(n) => u16::try_from(*n).ok(),
                Value::Uint(n) => u16::try_from(*n).ok(),
                _ => None,
            },
            _ => None,
        }
    };
    match value {
        Value::Null => Ok(Vec::new()),
        Value::List(items) => items
            .iter()
            .map(|item| match item {
                Value::List(pair) | Value::Tuple(pair) if pair.len() == 2 => {
                    Some((integer(&pair[0])?, integer(&pair[1])?))
                }
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
            .context("weights entry is not a (u16, u16) pair"),
        _ => bail!("weights value is not a list"),
    }
}

/// Decode `TimelockedWeightCommits` entries read by name into
/// `(hotkey, commit_block, reveal_round)`; the ciphertext is skipped.
pub fn commits_from_value(value: &Value) -> Result<Vec<([u8; 32], u64, u64)>> {
    let integer = |value: &Value| -> Option<u64> {
        match value {
            Value::Int(n) => u64::try_from(*n).ok(),
            Value::Uint(n) => u64::try_from(*n).ok(),
            _ => None,
        }
    };
    match value {
        Value::Null => Ok(Vec::new()),
        Value::List(items) => items
            .iter()
            .map(|item| match item {
                Value::List(fields) | Value::Tuple(fields) if fields.len() == 4 => Ok((
                    account_from_value(&fields[0])?,
                    integer(&fields[1]).context("commit block is not an integer")?,
                    integer(&fields[3]).context("reveal round is not an integer")?,
                )),
                _ => bail!("commit entry is not a 4-tuple"),
            })
            .collect(),
        _ => bail!("commits value is not a list"),
    }
}

pub(crate) fn u128_of(value: &rtao_core::DynValue, name: &str) -> Result<u128> {
    value
        .as_u128()
        .with_context(|| format!("{name} is not an unsigned integer"))
}

/// Decode a boolean storage value read by name. A value that is no longer a
/// boolean is an error, never a silent pass.
pub fn flag_from_value(value: &rtao_core::DynValue, item: &str) -> Result<bool> {
    value
        .as_bool()
        .with_context(|| format!("SubtensorModule.{item} is no longer a boolean"))
}

/// Decode `Uids(netuid, hotkey)` read by name: `None` when the hotkey has no
/// UID (an absent `OptionQuery` entry decodes as null).
pub fn uid_from_value(value: &rtao_core::DynValue) -> Result<Option<u16>> {
    if value.to_json().is_null() {
        return Ok(None);
    }
    Ok(Some(u16::try_from(u128_of(value, "Uids")?)?))
}

/// Pair `Keys(netuid, uid)` and `BlockAtRegistration(netuid, uid)` values read
/// by name for UIDs `0..n`, in UID order.
pub fn neurons_from_values(
    hotkeys: &[rtao_core::DynValue],
    blocks: &[rtao_core::DynValue],
) -> Result<Vec<NeuronIdentity>> {
    ensure!(
        hotkeys.len() == blocks.len() && hotkeys.len() <= MAX_SNAPSHOT_UIDS as usize,
        "registration reads returned the wrong number of values"
    );
    hotkeys
        .iter()
        .zip(blocks)
        .enumerate()
        .map(|(uid, (hotkey, block))| {
            Ok(NeuronIdentity {
                uid: u16::try_from(uid)?,
                hotkey: slop_ninja_protocol::AccountId(account_from_value(hotkey.as_core())?),
                registration_block: u64::try_from(u128_of(block, "BlockAtRegistration")?)?,
            })
        })
        .collect()
}

impl StartupPorts for LivePorts {
    fn open_journal(&mut self) -> Result<H256> {
        ensure!(
            Path::new(&self.config.journal_path).is_file(),
            "journal does not exist; create it once with `init`"
        );
        let adapter = open_adapter(
            &self.config,
            self.identity.clone(),
            self.authorized.clone(),
            self.clock.clone(),
        )?;
        let genesis = adapter.controls().identity().genesis;
        self.adapter = Some(adapter);
        Ok(genesis)
    }

    async fn connect(&mut self) -> Result<H256> {
        let client = Client::connect(client_config(&self.config, self.track_heads)?).await?;
        let genesis = client.genesis();
        self.client = Some(client);
        Ok(genesis)
    }

    async fn finalized_head(&mut self) -> Result<FinalizedHead> {
        let client = self.client()?;
        let hash: String = client
            .request("chain_getFinalizedHead", serde_json::json!([]))
            .await?;
        let hash = parse_hash(&hash)?;
        let raw: serde_json::Value = client
            .request(
                "chain_getHeader",
                serde_json::json!([format!("0x{}", hex::encode(hash.0))]),
            )
            .await?;
        ensure!(!raw.is_null(), "finalized header is unavailable");
        let header = BlockHeader::from_json_with_hash(raw, hash)?;
        let head = FinalizedHead {
            number: header.number,
            hash: header.hash,
        };
        self.header = Some(header);
        Ok(head)
    }

    async fn recover_finalized(&mut self, head: &FinalizedHead) -> Result<RecoveryCounts> {
        let header = self.header_for(head)?.clone();
        let client = self.client()?.clone();
        let metrics = self.metrics.clone();
        let progress = self
            .adapter_mut()?
            .recover_finalized(&client, &header, &metrics)
            .await?;
        Ok(RecoveryCounts {
            ready_to_resume: progress.ready_to_resume,
            reveal_history_caught_up: progress.reveal_history_caught_up,
            unresolved_attempts: progress.unresolved_attempts,
            finalized_failures: progress.finalized_failures,
            semantic_contract_mismatches: progress.semantic_contract_mismatches,
            outstanding_reveals: progress.outstanding_reveals.len(),
            paused_during_recovery: progress.paused_during_recovery,
        })
    }

    async fn subnet_state(&mut self, head: &FinalizedHead) -> Result<SubnetState> {
        let snapshot = self.client()?.at(BlockRef::Hash(head.hash)).await?;
        let netuid = self.identity.netuid;
        // One coalesced read. A missing `CommitRevealWeightsEnabled` key decodes
        // as `true`, so startup also requires `NetworksAdded` (default `false`).
        match snapshot
            .fetch((
                st::NETWORKS_ADDED.at((netuid,)),
                st::UIDS.at((netuid, self.identity.hotkey)),
                st::COMMIT_REVEAL_WEIGHTS_ENABLED.at((netuid,)),
            ))
            .await
        {
            Ok((subnet_exists, uid, commit_reveal_enabled)) => Ok(SubnetState {
                subnet_exists,
                hotkey_registered: uid.is_some(),
                commit_reveal_enabled,
            }),
            Err(typed) => {
                // The compiled storage descriptors no longer match the live
                // metadata. Read the same items by name with live key types,
                // hashers and defaults; a value that is no longer a boolean is
                // an error for this read only, never a silent pass.
                tracing::warn!(event = "typed_storage_refused", error = %typed);
                let net = keys::subnet(netuid);
                let pair = keys::hotkey(netuid, self.identity.hotkey);
                let read = |item: &'static str, keys: Vec<_>| {
                    let snapshot = &snapshot;
                    async move {
                        snapshot
                            .query_batch_dyn("SubtensorModule", item, vec![keys])
                            .await
                            .with_context(|| format!("SubtensorModule.{item} read by name failed"))?
                            .into_iter()
                            .next()
                            .with_context(|| format!("SubtensorModule.{item} returned no value"))
                    }
                };
                let exists =
                    flag_from_value(&read("NetworksAdded", net.clone()).await?, "NetworksAdded")?;
                let uid = uid_from_value(&read("Uids", pair).await?)?;
                let enabled = flag_from_value(
                    &read("CommitRevealWeightsEnabled", net).await?,
                    "CommitRevealWeightsEnabled",
                )?;
                Ok(SubnetState {
                    subnet_exists: exists,
                    hotkey_registered: uid.is_some(),
                    commit_reveal_enabled: enabled,
                })
            }
        }
    }

    async fn runtime_surface(&mut self, head: &FinalizedHead) -> Result<SurfaceStatus> {
        // New calls are composed and signed against the head runtime, so that
        // is the metadata whose operations are evaluated. The commit-reveal
        // version is read by name at the best head, as rtao reads it.
        let client = self.client()?.clone();
        let adapter = self.adapter.as_ref().context("journal is not open")?;
        let (summary, observed) = adapter.evaluate_live(&client, head.number).await?;
        Ok(SurfaceStatus {
            deferred: summary
                .deferred
                .into_iter()
                .map(|deferred| (deferred.operation, deferred.code))
                .collect(),
            adapted: summary.adapted,
            observed_spec_version: Some(observed.spec_version),
        })
    }

    fn resume(&mut self) -> Result<()> {
        self.adapter_mut()?.resume_after_recovery()
    }
}

/// [`LivePorts::chain_check_inputs`].
#[derive(Clone, Copy, Debug, Default)]
struct ChainCheckInputs {
    max_allowed_validators: Option<u16>,
    stake_threshold: Option<u64>,
    own_alpha_stake: Option<u64>,
}

impl LivePorts {
    /// The inputs of the chain's stake and permit checks that the weight
    /// preflight does not carry, read by name at the best head for the dry
    /// run's report. Best effort: a failed read reports `None`.
    async fn chain_check_inputs(&self) -> ChainCheckInputs {
        use rtao_primitives::IntoCoreValue;
        let Ok(client) = self.client() else { return ChainCheckInputs::default() };
        let Ok(best) = client.at(BlockRef::Best).await else { return ChainCheckInputs::default() };
        let read = |item: &'static str, key: Vec<rtao_core::codec::Value>| {
            let best = &best;
            async move {
                let value = best.query_batch_dyn("SubtensorModule", item, vec![key]).await.ok()?.into_iter().next()?;
                u128_of(&value, item).ok()
            }
        };
        let netuid = self.identity.netuid;
        ChainCheckInputs {
            max_allowed_validators: read("MaxAllowedValidators", keys::subnet(netuid))
                .await
                .and_then(|value| u16::try_from(value).ok()),
            stake_threshold: read("StakeThreshold", Vec::new()).await.and_then(|value| u64::try_from(value).ok()),
            own_alpha_stake: read(
                "TotalHotkeyAlpha",
                vec![self.identity.hotkey.into_core_value(), netuid.into_core_value()],
            )
            .await
            .and_then(|value| u64::try_from(value).ok()),
        }
    }
}

/// The [`WeightLimits`] of a weight preflight.
pub fn limits_of(preflight: &WeightPreflight) -> WeightLimits {
    WeightLimits {
        min_allowed_weights: preflight.min_allowed_weights,
        max_weights_limit: preflight.max_weights_limit,
        population: preflight.population,
        own_permit: preflight
            .uid
            .and_then(|uid| preflight.permits.get(usize::from(uid.0)).copied())
            .unwrap_or(false),
        null_consensus: preflight.null_consensus,
    }
}

impl LivePorts {
    /// Read and conform the weight-commit inputs at the best head, exactly as
    /// rtao's scheduler would, and refuse a row the chain's limits would alter.
    async fn prepare_commit(
        &self,
        derived: &Derived,
    ) -> Result<(
        u16,
        (Vec<Uid>, Vec<u16>),
        rtao_intents::weights::PreparedWeightCommit,
        u16,
        WeightPreflight,
    )> {
        let client = self.client()?;
        let netuid = self.identity.netuid;
        let mechanism = derived.work.mechanism;
        let best = client.at(BlockRef::Best).await?;
        let preflight =
            WeightPreflight::fetch(&best, netuid, mechanism, self.identity.hotkey).await?;
        let uid = preflight
            .uid
            .context("hotkey_not_registered: the validator hotkey has no UID on this subnet")?;
        ensure!(
            preflight.commit_reveal,
            "commit_reveal_disabled: refusing to prepare direct weights"
        );
        // The commit tag rtao's scheduler would use (D-086): the stored tag
        // when it is the native payload version or the live runtime code is
        // one rtao reviewed. The scheduler also follows a tag under code that
        // revealed one of its own commitments; that evidence lives in the
        // dispatch journal, so the dry run reports it as a pause instead.
        let tag = if preflight.commit_reveal_version == NATIVE_CR_PAYLOAD_VERSION {
            NATIVE_CR_PAYLOAD_VERSION
        } else if preflight
            .runtime_code
            .and_then(rtao_neuron::weights::reviewed_cr_runtime)
            .is_some()
        {
            preflight.commit_reveal_version
        } else {
            bail!(
                "commit_reveal_version_paused: the chain stores commit-reveal version {} under runtime code this rtao build has not reviewed; rtao's scheduler pauses timelocked commits unless one of its commitments revealed under this code",
                preflight.commit_reveal_version
            );
        };
        // rtao refuses a version key below the runtime's before signing
        // (`check_except_cr`); the chain checks it again at reveal.
        ensure!(
            preflight.required_version_key == 0
                || self.config.weight_version >= preflight.required_version_key,
            "weights_version_key_refused: the subnet requires weights version key {} and this validator commits {}",
            preflight.required_version_key,
            self.config.weight_version
        );
        let (uids, weights): (Vec<Uid>, Vec<f64>) = derived
            .input
            .iter()
            .map(|(uid, weight)| (*uid, *weight))
            .unzip();
        ensure!(
            uids.iter().all(|dest| dest.0 < preflight.population),
            "row_destination_unregistered: a row UID is outside the subnet population"
        );
        let conformed = conform(
            &uids,
            &weights,
            ConformState {
                uid,
                min_allowed_weights: preflight.min_allowed_weights.min(preflight.population),
                max_weight_limit: preflight.max_weights_limit,
            },
            netuid,
            false,
        )
        .map_err(|error| anyhow::anyhow!("row_refused_by_chain_limits: {error}"))?;
        // The committed form: the canonical row max-upscaled, a fixed point
        // of rtao's own max-upscale and of the chain's.
        let expected: (Vec<Uid>, Vec<u16>) = derived
            .chain_entries
            .iter()
            .map(|(uid, weight)| (Uid(*uid), *weight))
            .unzip();
        ensure!(
            conformed == expected,
            "row_not_preserved: the subnet's weight limits would change the derived row"
        );
        let limit = if preflight.null_consensus {
            let (count,) = best
                .fetch((st::MECHANISM_COUNT_CURRENT.at((netuid,)),))
                .await?;
            CommitSizeLimit::Null {
                mechanisms: u16::from(count.0),
            }
        } else {
            CommitSizeLimit::Yuma
        };
        let slot = best
            .constant(&rtao_runtime::constants::aura::SLOT_DURATION)
            .await?;
        ensure!(slot > 0, "Aura slot duration is zero");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs_f64();
        let commit = TimelockCommitter::new(limit)?
            .commit_at(TimelockCommitRequest {
                now,
                hotkey: self.identity.hotkey,
                row: expected.clone(),
                version_key: self.config.weight_version,
                state: preflight.schedule.clone(),
                reveal_period_epochs: preflight.reveal_period,
                block_time: slot as f64 / 1000.0,
            })
            .await?;
        Ok((uid.0, expected, commit, tag, preflight))
    }

    /// `Weights(netuid, uid)` at `at`, read by name.
    async fn read_weights_at(&self, uid: u16, mechanism: u8, at: BlockRef) -> Result<Vec<(u16, u16)>> {
        let snapshot = self.client()?.at(at).await?;
        let value = snapshot
            .query_batch_dyn(
                "SubtensorModule",
                "Weights",
                vec![keys::weights(self.identity.netuid, mechanism, uid)?],
            )
            .await?
            .into_iter()
            .next()
            .context("Weights returned no value")?;
        weights_from_value(value.as_core())
    }

    /// `RevealPeriodEpochs(netuid)` at finality, read by name.
    async fn reveal_period(&self) -> Result<u64> {
        let snapshot = self.client()?.at(BlockRef::Finalized).await?;
        let period = snapshot
            .query_batch_dyn(
                "SubtensorModule",
                "RevealPeriodEpochs",
                vec![keys::subnet(self.identity.netuid)],
            )
            .await?
            .into_iter()
            .next()
            .context("RevealPeriodEpochs returned no value")?;
        Ok(u64::try_from(u128_of(&period, "RevealPeriodEpochs")?)?)
    }

    /// This hotkey's entries in `TimelockedWeightCommits(index, commit_epoch)`
    /// at finality, as `(commit_block, reveal_round)`.
    async fn hotkey_commits(&self, mechanism: u8, commit_epoch: u64) -> Result<Vec<(u64, u64)>> {
        let snapshot = self.client()?.at(BlockRef::Finalized).await?;
        self.hotkey_commits_at(&snapshot, mechanism, commit_epoch)
            .await
    }

    /// This hotkey's entries in the commit epoch's queue before its reveal
    /// drains it: at finality while the commit epoch is current, else at the
    /// last block before the following epoch began. `None` once more than one
    /// epoch has passed. Returns the block read and the entries.
    async fn hotkey_commits_before_reveal(
        &self,
        mechanism: u8,
        commit_epoch: u64,
    ) -> Result<Option<(u64, Vec<(u64, u64)>)>> {
        let client = self.client()?;
        let finalized = client.at(BlockRef::Finalized).await?;
        let netuid = self.identity.netuid;
        let read = |item: &'static str| {
            let finalized = &finalized;
            async move {
                let value = finalized
                    .query_batch_dyn("SubtensorModule", item, vec![keys::subnet(netuid)])
                    .await?
                    .into_iter()
                    .next()
                    .with_context(|| format!("{item} returned no value"))?;
                Ok::<_, anyhow::Error>(u64::try_from(u128_of(&value, item)?)?)
            }
        };
        let index = read("SubnetEpochIndex").await?;
        let snapshot = if index == commit_epoch {
            finalized
        } else if index == commit_epoch.saturating_add(1) {
            let boundary = read("LastEpochBlock").await?;
            client
                .at(BlockRef::Number(boundary.saturating_sub(1)))
                .await?
        } else {
            return Ok(None);
        };
        let at = snapshot.block().number;
        Ok(Some((
            at,
            self.hotkey_commits_at(&snapshot, mechanism, commit_epoch)
                .await?,
        )))
    }

    async fn hotkey_commits_at(
        &self,
        snapshot: &rtao_client::Snapshot,
        mechanism: u8,
        commit_epoch: u64,
    ) -> Result<Vec<(u64, u64)>> {
        let value = snapshot
            .query_batch_dyn(
                "SubtensorModule",
                "TimelockedWeightCommits",
                vec![keys::timelocked_commits(
                    self.identity.netuid,
                    mechanism,
                    commit_epoch,
                )?],
            )
            .await?
            .into_iter()
            .next()
            .context("TimelockedWeightCommits returned no value")?;
        Ok(commits_from_value(value.as_core())?
            .into_iter()
            .filter(|(account, _, _)| *account == self.identity.hotkey.0)
            .map(|(_, block, round)| (block, round))
            .collect())
    }

    /// `SubnetEpochIndex(netuid)` at finality, read by name.
    async fn finalized_epoch_index(&self) -> Result<u64> {
        self.epoch_index_ref(BlockRef::Finalized).await
    }

    /// `SubnetEpochIndex(netuid)` in the state after finalized block `block`:
    /// the epoch a commit included in that block was recorded under.
    async fn epoch_index_at(&self, block: u64) -> Result<u64> {
        self.epoch_index_ref(BlockRef::Number(block)).await
    }

    async fn epoch_index_ref(&self, at: BlockRef) -> Result<u64> {
        let snapshot = self.client()?.at(at).await?;
        let value = snapshot
            .query_batch_dyn(
                "SubtensorModule",
                "SubnetEpochIndex",
                vec![keys::subnet(self.identity.netuid)],
            )
            .await?
            .into_iter()
            .next()
            .context("SubnetEpochIndex returned no value")?;
        Ok(u64::try_from(u128_of(&value, "SubnetEpochIndex")?)?)
    }

    /// The latest epoch this hotkey committed in for `mechanism`, from rtao's
    /// reveal watcher: outstanding commits (`reveal_epoch - period`) and the
    /// retained completed outcomes. `None` when the watcher holds neither.
    fn last_commit_epoch(&self, mechanism: rtao_primitives::MechId, period: u64) -> Option<u64> {
        let reveals = self.adapter.as_ref()?.reveals();
        let outstanding = reveals
            .outstanding()
            .into_iter()
            .filter(|reveal| reveal.mechanism == mechanism)
            .filter_map(|reveal| reveal.reveal_epoch?.checked_sub(period));
        let completed = reveals
            .outcomes()
            .iter()
            .filter(|outcome| outcome.mechanism == mechanism)
            .map(|outcome| outcome.commit_epoch);
        outstanding.chain(completed).max()
    }

    async fn own_uid(&self) -> Result<Option<u16>> {
        let snapshot = self.client()?.at(BlockRef::Finalized).await?;
        let value = snapshot
            .query_batch_dyn(
                "SubtensorModule",
                "Uids",
                vec![keys::hotkey(self.identity.netuid, self.identity.hotkey)],
            )
            .await?
            .into_iter()
            .next()
            .context("Uids returned no value")?;
        uid_from_value(&value)
    }

    /// Build the hotkey-only context rtao's scheduler signs with. This is the
    /// only place a private key is read.
    async fn signing_context(&mut self) -> Result<NeuronCx> {
        let dispatch = self
            .config
            .dispatch
            .as_ref()
            .context("dispatch is not configured: add [dispatch] hotkey_file")?;
        let client = self.client()?.clone();
        let netuid = self.identity.netuid;
        let hotkey = rtao_wallet::HotkeyFile::new(&dispatch.hotkey_file);
        ensure!(
            hotkey.hotkey_address()? == self.identity.hotkey,
            "dispatch.hotkey_file names another account than wallet.hotkey"
        );
        let signer = hotkey.unlock().await?;
        let shutdown = tokio_util::sync::CancellationToken::new();
        let (metagraph, _) = rtao_metagraph::MetagraphSync::spawn(
            client.clone(),
            netuid,
            rtao_primitives::MechId(0),
            rtao_metagraph::SyncPolicy::validator(),
            shutdown.clone(),
        )
        .await?;
        // Keep the finalized snapshot and stop the refresh worker. It reads a
        // block and the epoch schedule for every finalized block (about 15
        // calls a minute on testnet), which alone exceeds a public read
        // budget and starved rtao's dispatch and reveal replay. rtao's
        // scheduler only `load()`s the graph (UID hotkeys for its hold check
        // and stake advisories); a restart takes a fresh snapshot.
        metagraph.shutdown().await;
        tracing::info!(
            event = "metagraph_snapshot",
            at = metagraph.load().at.number,
            neurons = metagraph.load().len(),
            worker = "stopped"
        );
        let finalized = client.at(BlockRef::Finalized).await?;
        let (index, last_epoch_block, tempo) = finalized
            .fetch((
                st::SUBNET_EPOCH_INDEX.at((netuid,)),
                st::LAST_EPOCH_BLOCK.at((netuid,)),
                st::TEMPO.at((netuid,)),
            ))
            .await?;
        tracing::info!(
            event = "epoch_schedule",
            finalized = finalized.block().number,
            last_epoch_block,
            tempo,
            epoch_offset_blocks = dispatch.epoch_offset_blocks
        );
        self.tempo = Some(u64::from(tempo));
        // An epoch spans `Tempo + 1` blocks; a larger offset never falls due.
        if let Some(offset) = dispatch.epoch_offset_blocks
            && offset > u64::from(tempo)
        {
            tracing::warn!(
                event = "epoch_offset_beyond_tempo",
                epoch_offset_blocks = offset,
                tempo,
                alert = true
            );
        }
        // Last, after every startup read: the dispatch loop's reads follow.
        let signing = self.connect_signing_transport(&client).await?;
        let executor = rtao_intents::executor(signing).with_policy(rtao_tx::Policy {
            max_spend: Some(rtao_primitives::Balance::<rtao_primitives::Tao>::from_rao(
                0,
            )),
            allowed_netuids: Some(std::collections::BTreeSet::from([netuid])),
            allow_raw_calls: false,
            ..Default::default()
        });
        Ok(NeuronCx {
            client,
            executor,
            netuid,
            hotkey,
            metagraph,
            epochs: EpochClock::new(EpochStatus {
                block: finalized.block().number,
                index,
                last_epoch_block,
                tempo,
            }),
            http: rtao_btauth::client::SignedClient::new(signer)?,
            shutdown,
            metrics: self.metrics.clone(),
            state_dir: self
                .config
                .journal_path
                .parent()
                .context("journal_path has no parent")?
                .to_path_buf(),
        })
    }
}

impl LivePorts {
    /// Connect the separate signing transport, when configured, in a quiet
    /// period of the read session, so that it never consumes a signature's
    /// admission window and no minute holds both its connection and reads.
    /// The connection spends about 16 calls of its burst at once (handshake,
    /// runtime, head subscriptions and rtao's `TokenSymbol` scan). Connected
    /// while the read session ran at its budget, every testnet restart minute
    /// reached 28 to 30 calls. Nothing else reads in this process meanwhile:
    /// the metagraph worker is stopped and the head stream starts afterwards.
    async fn connect_signing_transport(&mut self, read: &Client) -> Result<Client> {
        let Some(per_minute) = self.config.rpc.signing_requests_per_minute else {
            tracing::info!(event = "signing_transport_ready", separate = false);
            return Ok(read.clone());
        };
        // A run that continues on a successor keeps its signing connection;
        // a failed one ends the process (`transport_failed`) instead.
        if let Some(signing) = &self.signing_client {
            tracing::info!(event = "signing_transport_ready", separate = true, reused = true);
            return Ok(signing.clone());
        }
        let quiet = Duration::from_secs(self.config.rpc.signing_connect_quiet_secs);
        tracing::info!(
            event = "signing_transport_quiet",
            phase = "before_connect",
            secs = quiet.as_secs()
        );
        tokio::time::sleep(quiet).await;
        let signing = Client::connect(signing_client_config(&self.config, per_minute)?)
            .await
            .context("signing transport connect failed")?;
        ensure!(
            signing.genesis() == read.genesis(),
            "signing transport serves another chain"
        );
        self.signing_client = Some(signing.clone());
        tracing::info!(event = "signing_transport_ready", separate = true);
        tokio::time::sleep(quiet).await;
        tracing::info!(
            event = "signing_transport_quiet",
            phase = "after_connect",
            secs = quiet.as_secs()
        );
        Ok(signing)
    }
}

/// The error rtao ended a client's native session with, if it did: since
/// D-097 only a JSON-RPC policy error, a policy close (1008) or a refused
/// initial connect (the client's own connect error). No RPC call.
async fn terminal_session_failure(client: &Client) -> Option<String> {
    let session = client.native_author_session().await.ok()?;
    session_failure(&session)
}

/// [`terminal_session_failure`] for one session.
pub fn session_failure(session: &rtao_rpc::Session) -> Option<String> {
    let state = session.state();
    match &*state.borrow() {
        rtao_rpc::SessionState::Failed { error } => Some(error.to_string()),
        _ => None,
    }
}

/// `Timestamp.Now` at the best head, in milliseconds, read by name.
async fn best_timestamp_ms(client: &Client) -> Result<u64> {
    let value = client
        .at(BlockRef::Best)
        .await?
        .query_batch_dyn("Timestamp", "Now", vec![Vec::new()])
        .await?
        .into_iter()
        .next()
        .context("Timestamp.Now returned no value")?;
    let timestamp = u64::try_from(u128_of(&value, "Timestamp.Now")?)?;
    ensure!(timestamp > 0, "Timestamp.Now is unset");
    Ok(timestamp)
}

impl AppPorts for LivePorts {
    fn read_bundle(&mut self) -> Result<Vec<u8>> {
        let package = self
            .config
            .package
            .as_ref()
            .context("no [package] is configured")?;
        let bytes = match &self.staged_bundle {
            // A dry run's fetched bundle, verified within the bound.
            Some(staged) => staged.clone(),
            None => {
                let meta = std::fs::metadata(&package.path).context("package.path is unreadable")?;
                ensure!(
                    meta.len() <= package::MAX_BUNDLE_BYTES,
                    "package bundle exceeds 64 MiB"
                );
                std::fs::read(&package.path)?
            }
        };
        self.installed_sha = Some(crate::fetch::sha256_hex(&bytes));
        Ok(bytes)
    }

    fn trusted_keys(&self) -> Result<TrustedKeys> {
        self.config
            .package
            .as_ref()
            .context("no [package] is configured")?
            .trusted_keys
            .keys()
    }

    fn chain(&self) -> ([u8; 32], u16) {
        (self.identity.genesis.0, self.identity.netuid.0)
    }

    async fn registration_snapshot(
        &mut self,
        head: &FinalizedHead,
        chain: &ChainBinding,
    ) -> Result<RegistrationSnapshot> {
        let snapshot = self.client()?.at(BlockRef::Hash(head.hash)).await?;
        let netuid = self.identity.netuid;
        let count = snapshot
            .query_batch_dyn("SubtensorModule", "SubnetworkN", vec![keys::subnet(netuid)])
            .await?
            .into_iter()
            .next()
            .context("SubnetworkN returned no value")?;
        let count = u128_of(&count, "SubnetworkN")?;
        ensure!(
            count <= MAX_SNAPSHOT_UIDS,
            "subnet population exceeds the snapshot bound"
        );
        let neuron_keys: Vec<Vec<Value>> = (0..u16::try_from(count)?)
            .map(|uid| keys::neuron(netuid, uid))
            .collect();
        let hotkeys = snapshot
            .query_batch_dyn("SubtensorModule", "Keys", neuron_keys.clone())
            .await?;
        let blocks = snapshot
            .query_batch_dyn("SubtensorModule", "BlockAtRegistration", neuron_keys)
            .await?;
        ensure!(
            hotkeys.len() == count as usize,
            "registration reads returned the wrong number of values"
        );
        let neurons = neurons_from_values(&hotkeys, &blocks)?;
        Ok(package::snapshot(chain, head.number, head.hash.0, neurons)?)
    }

    async fn epoch_index(&mut self, _head: &FinalizedHead) -> Result<u64> {
        self.finalized_epoch_index().await
    }

    async fn hold_hotkeys(&mut self, head: &FinalizedHead) -> Result<Vec<[u8; 32]>> {
        let snapshot = self.client()?.at(BlockRef::Hash(head.hash)).await?;
        let owner = snapshot
            .query_batch_dyn(
                "SubtensorModule",
                "SubnetOwnerHotkey",
                vec![keys::subnet(self.identity.netuid)],
            )
            .await?
            .into_iter()
            .next()
            .context("SubnetOwnerHotkey returned no value")?;
        Ok(vec![
            self.identity.hotkey.0,
            account_from_value(owner.as_core())?,
        ])
    }

    fn operation_id(&self, work: &WeightWork) -> Result<String> {
        work.operation_id(&self.identity)
    }

    fn retained(&self, operation_id: &str) -> Result<bool> {
        let adapter = self.adapter.as_ref().context("journal is not open")?;
        Ok(adapter.controls().operation(operation_id)?.is_some())
    }

    async fn weight_limits(&mut self, mechanism: u8) -> Result<WeightLimits> {
        // rtao's own weight preflight at the best head, where its
        // preparation reads the same fields: one coalesced read, compiled
        // where the shapes match and by name where they do not.
        let best = self.client()?.at(BlockRef::Best).await?;
        let preflight = WeightPreflight::fetch(
            &best,
            self.identity.netuid,
            rtao_primitives::MechId(mechanism),
            self.identity.hotkey,
        )
        .await
        .context("weight preflight read failed")?;
        Ok(limits_of(&preflight))
    }

    fn enqueue(
        &mut self,
        work: &WeightWork,
        input: BTreeMap<Uid, f64>,
        owner_rows: OwnerRowPolicy,
    ) -> Result<Enqueued> {
        self.adapter_mut()?.enqueue_with_owner_rows(work, input, owner_rows)
    }

    fn authorize(&mut self, operation_id: &str, input: &BTreeMap<Uid, f64>, owner_rows: OwnerRowChoice) -> Result<()> {
        let hash = slop_ninja_subnet::weight_row_hash(input.clone())?;
        self.authorized.grant(operation_id, &hash, owner_rows);
        Ok(())
    }

    fn held_target(&self) -> Result<Option<crate::package_v2::HeldTargetFile>> {
        read_held(&held_target_path(&self.config.journal_path), &self.trusted_keys()?, !self.dry_run)
    }

    /// The journaled revocations. A store that cannot be read as a whole
    /// keeps the revocations this process last loaded in force and reports
    /// the error in [`crate::revocations::Loaded::store_error`] (alerted once
    /// per distinct error by the caller); with none loaded yet (at startup)
    /// it is an error, so nothing is committed on a check that did not run.
    fn revocations(&self) -> Result<crate::revocations::Loaded> {
        let (genesis, netuid) = AppPorts::chain(self);
        let chain = ChainBinding { genesis: slop_ninja_protocol::Digest(genesis), netuid, mechanism: 0 };
        let mut kept = self.revocations_loaded.lock().map_err(|_| anyhow::anyhow!("revocation cache poisoned"))?;
        match crate::revocations::load(&crate::revocations::path(&self.config.journal_path), &chain) {
            Ok(loaded) => {
                *kept = Some(loaded.records.clone());
                Ok(loaded)
            }
            Err(error) => match kept.as_ref() {
                Some(records) => Ok(crate::revocations::Loaded {
                    records: records.clone(),
                    problems: Vec::new(),
                    store_error: Some(format!("{error:#}")),
                }),
                None => Err(error),
            },
        }
    }

    fn record_target(&mut self, held: &crate::package_v2::HeldTargetFile) -> Result<()> {
        if self.dry_run {
            tracing::info!(event = "held_target_not_recorded", reason = "dry_run", target = %held.target);
            return Ok(());
        }
        let bound = held.clone().bound_to(&self.trusted_keys()?);
        crate::supersede::write_private_atomic(&held_target_path(&self.config.journal_path), &serde_json::to_vec_pretty(&bound)?)
    }

    fn pending_operations(&self, mechanism: u8) -> Result<Vec<slop_ninja_subnet::PendingOperation>> {
        let adapter = self.adapter.as_ref().context("journal is not open")?;
        adapter.controls().pending_operations(rtao_primitives::MechId(mechanism))
    }

    fn retire(&mut self, operation_id: &str, reason: &str) -> Result<bool> {
        let adapter = self.adapter.as_ref().context("journal is not open")?;
        adapter.controls().retire_superseded(operation_id, reason)
    }

    fn retained_row(&self, operation_id: &str) -> Result<Option<(String, bool)>> {
        let adapter = self.adapter.as_ref().context("journal is not open")?;
        adapter.controls().retained_row(operation_id)
    }

    fn retained_policy(&self, operation_id: &str) -> Result<Option<OwnerRowPolicy>> {
        let adapter = self.adapter.as_ref().context("journal is not open")?;
        adapter.controls().owner_row_policy(operation_id)
    }

    fn retired(&self, operation_id: &str) -> Result<Option<String>> {
        let adapter = self.adapter.as_ref().context("journal is not open")?;
        adapter.controls().retired(operation_id)
    }

    async fn dry_run(&mut self, operation_id: &str, derived: &Derived) -> Result<DryRunReport> {
        let (signing_uid, _, commit, tag, preflight) = self.prepare_commit(derived).await?;
        let limits = limits_of(&preflight);
        let checks = self.chain_check_inputs().await;
        let client = self.client()?.clone();
        let runtime = client.refresh_runtime().await?;
        let call = payload::timelocked_commit_call(
            &runtime,
            self.identity.netuid,
            derived.work.mechanism,
            commit.ciphertext.clone(),
            commit.reveal_round,
            tag,
        )?;
        let unsigned = client
            .prepare_public(
                call.clone(),
                self.identity.hotkey,
                &PublicPrepareOptions {
                    submit: SubmitOptions {
                        mortality: Mortality::Mortal(rtao_client::Period::new(COMMIT_ERA_PERIOD)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await?;
        let bytes = call.as_bytes();
        Ok(DryRunReport {
            operation_id: operation_id.into(),
            row_digest: derived.row_digest(),
            entries: derived.entries(),
            signing_uid,
            reveal_round: commit.reveal_round,
            commit_bytes: commit.ciphertext.len(),
            call_pallet_index: bytes.first().copied().unwrap_or_default(),
            call_index: bytes.get(1).copied().unwrap_or_default(),
            call_sha256: payload::sha256_hex(bytes),
            signing_payload_bytes: unsigned.payload().len(),
            signing_payload_sha256: payload::sha256_hex(unsigned.payload()),
            nonce: unsigned.nonce(),
            era_expiry: unsigned.expiry(),
            ciphertext_randomized: true,
            observed_spec_version: runtime.spec_version(),
            commit_reveal_version: tag,
            weight_version: self.config.weight_version,
            required_version_key: preflight.required_version_key,
            validator_permit: limits.own_permit,
            null_consensus: limits.null_consensus,
            max_allowed_validators: checks.max_allowed_validators,
            stake_threshold: checks.stake_threshold,
            own_alpha_stake: checks.own_alpha_stake,
            signed: false,
        })
    }

    async fn dispatch(
        &mut self,
        operation_id: &str,
        derived: &Derived,
        faults: &Faults,
    ) -> Result<DispatchReport> {
        let settings = self
            .config
            .dispatch
            .clone()
            .context("dispatch is not configured: add [dispatch] hotkey_file")?;
        let per_tempo = settings.recommit == Recommit::PerTempo;
        let mechanism = derived.work.mechanism;
        let row_hash = slop_ninja_subnet::weight_row_hash(derived.input.clone())?;
        // Every retained operation of this target with the verified row is
        // signable in this process; the tracked one is the pending operation
        // if any, else the latest. (Journal only, no RPC call.)
        let mut tracked = self.adopt_target(derived, &row_hash, operation_id)?;
        // The committed row of each operation not yet revealed, this
        // process's and earlier processes' (see `reveals`). A pending
        // operation of this target with the verified row carries the derived
        // row; each per-tempo re-commit adds the row re-derived for it.
        let mut ledger = RevealLedger::load(crate::reveals::ledger_path(&self.config.journal_path));
        {
            let controls = self.adapter.as_ref().context("journal is not open")?.controls();
            for op in controls.target_operations(&derived.work)? {
                if op.row_hash == row_hash && !op.handed_off {
                    ledger.record(&op.id, &row_hash, derived.chain_entries());
                }
            }
        }
        ledger.persist()?;
        // A deferred per-tempo re-commit is retried once the finalized epoch
        // index passes this value.
        let mut retry_after_epoch: Option<u64> = None;
        // reward-v1 with [fetch]: the authorization each re-commit is derived
        // under (a refresh swaps it in place), fetched bundles' supersession
        // state, and whether new commitments are paused because the
        // authorization does not yield one (a journaled signed revocation, or
        // the target is no longer the held one). Nothing pauses by time: an
        // authorization has no end epoch.
        let mut authority = derived.reward_v1.clone();
        let mut background = match &authority {
            Some(_) => self.spawn_fetcher()?,
            None => None,
        };
        let mut fetch_state = match self.startup_wait.take() {
            Some((staged, decision, epoch)) => FetchState::seeded(staged, &decision, epoch),
            None => FetchState::default(),
        };
        if background.is_some()
            && let Some(current) = &authority
        {
            let bytes = AppPorts::read_bundle(self)?;
            self.loop_installed = Some(InstalledBundle {
                sha256: crate::fetch::sha256_hex(&bytes),
                info: BundleInfo::of(&bytes, current)?,
                authorized: current.clone(),
            });
        }
        let mut authorization_paused = false;
        // What the per-head revocation check alerted already.
        let mut revocation_watch = RevocationWatch::default();
        let mut last_decision_error: Option<String> = None;
        // What rtao reported for the tracked operation while it waits to be
        // signed: alerted once per distinct outcome kind ([`not_submitted`]).
        let mut attempt_alert: Option<String> = None;
        // The best height of the last unsigned failure counted, and the
        // failure a Submit burn row's limits were last re-read for.
        let mut counted_failure: Option<u64> = None;
        let mut limits_checked_for: Option<(String, u64)> = None;
        // The first head at which the tracked operation was seen waiting
        // with its current journal attempt count, that count, and whether a
        // passed commit window was alerted since.
        let mut tracked_since: Option<u64> = None;
        let mut tracked_attempts: Option<usize> = None;
        let mut window_alerted = false;
        let cx = self.signing_context().await?;
        let client = cx.client.clone();
        let mut heads = finalized_heads(&client)?;
        let mut report = DispatchReport {
            recommit: match settings.recommit {
                Recommit::PerTempo => "per_tempo".into(),
                Recommit::Once => "once".into(),
            },
            ..DispatchReport::default()
        };
        let result = async {
            let mut read_back = false;
            let mut period: Option<u64> = None;
            while settings
                .max_finalized_blocks
                .is_none_or(|max| report.finalized_blocks < max)
            {
                let Some(next) = heads.next().await else {
                    bail!("finalized head stream ended");
                };
                let mut header = next?;
                // `Latest` already coalesces; take any newer head that is
                // ready. Recovery and reveal cursors cover skipped blocks.
                while let Some(Some(Ok(newer))) = heads.next().now_or_never() {
                    header = newer;
                }
                report.finalized_blocks += 1;
                let metrics = self.metrics.clone();
                let adapter = self.adapter_mut()?;
                if !adapter.is_recovered() {
                    let progress = adapter
                        .recover_finalized(&client, &header, &metrics)
                        .await?;
                    tracing::info!(
                        event = "recovery_round",
                        height = header.number,
                        ready = progress.ready_to_resume,
                        unresolved = progress.unresolved_attempts,
                        outstanding_reveals = progress.outstanding_reveals.len()
                    );
                    if progress.finalized_failures != 0
                        || progress.semantic_contract_mismatches != 0
                    {
                        report.stop = "recovery_requires_review".into();
                        break;
                    }
                    if !progress.ready_to_resume {
                        faults.hit(Phase::Dispatched)?;
                        continue;
                    }
                    adapter.resume_after_recovery()?;
                    tracing::info!(event = "resumed_after_recovery", height = header.number);
                }
                // rtao retries a refused (HTTP 403/429) reconnect within the
                // session (D-097), and `retry_forever` keeps every loss
                // retryable, so a refusal no longer ends a session. What rtao
                // still ends a session on is a JSON-RPC policy error or a
                // policy close (1008), after which it needs a new client. A
                // failed signing session would otherwise fail every commit
                // before signing; exit instead, so the unit restarts with fresh
                // connections. Nothing is lost: the journal is the state.
                let transports: Vec<(&str, Client)> = std::iter::once(("read", client.clone()))
                    .chain(self.signing_client.clone().map(|signing| ("signing", signing)))
                    .collect();
                for (name, transport) in &transports {
                    if let Some(error) = terminal_session_failure(transport).await {
                        tracing::error!(event = "transport_failed", transport = name, error = %error);
                        bail!(
                            "transport_failed: rtao ended the {name} transport's session ({error}); restart to reconnect"
                        );
                    }
                }
                // Observe the best head's `Timestamp.Now`, read by name, on
                // rtao's timelock clock. Since D-094 rtao's scheduler also
                // anchors the clock itself at each commit's preflight block
                // (its read uses a compiled descriptor, with no by-name
                // fallback); this read keeps an anchor even if that one fails.
                // A failed read keeps the last anchor.
                match best_timestamp_ms(&client).await {
                    Ok(timestamp) => self.clock.observe(timestamp),
                    Err(error) => tracing::warn!(event = "clock_anchor_failed", error = %error),
                }
                let before = self.status(&tracked.0)?.attempts;
                let adapter = self.adapter_mut()?;
                match adapter.dispatch_finalized(&cx, &header).await {
                    Ok(summary) => tracing::info!(
                        event = "dispatch_finalized",
                        height = header.number,
                        attempts = summary.attempts,
                        unresolved = summary.unresolved_attempts,
                        alert = summary.readiness.alert
                    ),
                    Err(error) => tracing::warn!(
                        event = "dispatch_refused",
                        height = header.number,
                        error = %format!("{error:#}")
                    ),
                }
                faults.hit(Phase::Dispatched)?;
                let status = self.status(&tracked.0)?;
                if status.attempts > before {
                    tracing::info!(
                        event = "attempt_recorded",
                        operation_id = %tracked.0,
                        tempo = ?tracked.1,
                        attempts = status.attempts,
                        unresolved = status.unresolved,
                        expired = status.expired,
                        finalized_success_block = ?status.finalized_success_block
                    );
                    faults.hit(Phase::Submitted)?;
                }
                if let Some(code) = &status.semantic_refusal {
                    tracing::warn!(event = "guard_refused", code = %code);
                }
                // The tracked operation still waits to be signed: rtao
                // returned `Ok` whatever it recorded, so its last outcome is
                // read here. A pre-signing failure (D-095) adds no journal
                // attempt and is retried on its own; without these alerts a
                // validator that never commits would look healthy.
                let waiting = status.finalized_success_block.is_none()
                    && status.unresolved == 0
                    && self
                        .adapter
                        .as_ref()
                        .context("journal is not open")?
                        .controls()
                        .retained_row(&tracked.0)?
                        .is_some_and(|(_, handed_off)| !handed_off);
                if waiting {
                    if tracked_attempts != Some(status.attempts) {
                        tracked_attempts = Some(status.attempts);
                        tracked_since = Some(header.number);
                        window_alerted = false;
                    }
                    let since = tracked_since.unwrap_or(header.number);
                    let last = self
                        .adapter
                        .as_ref()
                        .context("journal is not open")?
                        .last_attempt(mechanism)
                        .cloned();
                    if let Some(attempt) = &last {
                        if let rtao_neuron::weights::WeightAttemptOutcome::Failed(_) = attempt.outcome
                            && counted_failure != Some(attempt.best_block)
                        {
                            counted_failure = Some(attempt.best_block);
                            report.unsigned_failures += 1;
                        }
                        match not_submitted(&attempt.outcome) {
                            Some(kind) if attempt_alert.as_deref() != Some(kind.as_str()) => {
                                tracing::warn!(
                                    event = "weight_attempt_not_submitted",
                                    operation_id = %tracked.0,
                                    tempo = ?tracked.1,
                                    outcome = %kind,
                                    best_block = attempt.best_block,
                                    alert = true
                                );
                                attempt_alert = Some(kind);
                            }
                            Some(_) => {}
                            None => attempt_alert = None,
                        }
                        // A Submit burn row that rtao refuses before signing:
                        // the chain limits or permit may have changed since
                        // it was queued, which nothing else in this loop
                        // re-reads. Re-read them for each distinct failure,
                        // at most once an epoch (`Tempo + 1` blocks), so a
                        // run of failures with changing text costs at most
                        // one preflight read an epoch.
                        let failure = match &attempt.outcome {
                            rtao_neuron::weights::WeightAttemptOutcome::Failed(text) => Some(text.clone()),
                            rtao_neuron::weights::WeightAttemptOutcome::Refused(refusal) => Some(refusal.name.to_owned()),
                            _ => None,
                        };
                        if let (Some(failure), Some(crate::guard::Grant { owner_rows: OwnerRowChoice::Submit { owner_uid, owner_validator }, .. })) =
                            (failure, self.authorized.get(&tracked.0))
                            && limits_checked_for.as_ref().is_none_or(|(text, at)| {
                                *text != failure
                                    && header.number >= at.saturating_add(self.tempo.map_or(0, |tempo| tempo + 1))
                            })
                        {
                            limits_checked_for = Some((failure, header.number));
                            match AppPorts::weight_limits(self, mechanism.0).await {
                                // A burn row lists only the owner UID, max-upscaled.
                                Ok(limits) => {
                                    if let Err(refusal) = crate::app::check_burn_limits(
                                        &[(owner_uid, u16::MAX)],
                                        Some(owner_uid),
                                        owner_validator,
                                        &limits,
                                    ) {
                                        let retired = self
                                            .adapter
                                            .as_ref()
                                            .context("journal is not open")?
                                            .controls()
                                            .retire_superseded(&tracked.0, "burn_row_limits_changed")?;
                                        tracing::warn!(
                                            event = "burn_row_limits_changed",
                                            operation_id = %tracked.0,
                                            tempo = ?tracked.1,
                                            code = refusal.code,
                                            retired,
                                            alert = true
                                        );
                                        if retired {
                                            // The next start re-derives the
                                            // tempo and defers it under the
                                            // same code until the limits allow
                                            // the row again (exit 75).
                                            report.stop = format!("tempo_deferred:{}", refusal.code);
                                            return Ok(());
                                        }
                                    }
                                }
                                Err(error) => tracing::warn!(
                                    event = "weight_limits_unavailable",
                                    error = %format!("{error:#}")
                                ),
                            }
                        }
                    }
                    // Queued work is due at the first head past the offset of
                    // an epoch after the last attempt, which is less than two
                    // epochs (`Tempo + 1` blocks each) after it was queued or
                    // its last signed attempt expired. Past that with no new
                    // journal attempt, at least one commit window went by
                    // unsigned.
                    if let Some(tempo) = self.tempo
                        && !window_alerted
                        && header.number >= since.saturating_add(2 * (tempo + 1))
                    {
                        tracing::warn!(
                            event = "commit_window_passed_without_attempt",
                            operation_id = %tracked.0,
                            tempo = ?tracked.1,
                            waiting_since = since,
                            height = header.number,
                            unsigned_failures = report.unsigned_failures,
                            alert = true
                        );
                        report.windows_without_attempt += 1;
                        window_alerted = true;
                    }
                } else {
                    attempt_alert = None;
                }
                let adapter = self.adapter.as_ref().context("journal is not open")?;
                let outstanding: Vec<_> = adapter
                    .reveals()
                    .outstanding()
                    .into_iter()
                    .filter(|reveal| reveal.mechanism == mechanism)
                    .collect();
                for reveal in &outstanding {
                    tracing::info!(
                        event = "reveal_pending",
                        inclusion_block = reveal.inclusion_block,
                        reveal_epoch = reveal.reveal_epoch
                    );
                }
                let caught_up = adapter.reveals().caught_up(header.number);

                // Read the tracked commit back at finality once rtao recorded
                // its finalized inclusion, and count this hotkey's entries.
                if !read_back && let Some(block) = status.finalized_success_block {
                    match outstanding
                        .iter()
                        .find(|reveal| reveal.inclusion_block == block)
                    {
                        Some(reveal) => {
                            if let Some(reveal_epoch) = reveal.reveal_epoch {
                                let period = match period {
                                    Some(period) => period,
                                    None => *period.insert(self.reveal_period().await?),
                                };
                                let commit_epoch = reveal_epoch
                                    .checked_sub(period)
                                    .context("reveal epoch precedes the reveal period")?;
                                match self
                                    .hotkey_commits_before_reveal(mechanism.0, commit_epoch)
                                    .await
                                {
                                    Ok(None) => {
                                        tracing::warn!(
                                            event = "commit_read_back_unavailable",
                                            commit_epoch,
                                            reason = "more than one epoch has passed"
                                        );
                                        read_back = true;
                                    }
                                    Ok(Some((read_at, entries))) => {
                                        tracing::info!(event = "commit_queue_read", commit_epoch, read_at);
                                        let (commit_block, reveal_round) =
                                            entries.first().copied().unwrap_or_default();
                                        tracing::info!(
                                            event = "commit_read_back",
                                            operation_id = %tracked.0,
                                            tempo = ?tracked.1,
                                            finalized_success_block = block,
                                            commit_epoch,
                                            commit_block,
                                            reveal_round,
                                            reveal_epoch,
                                            hotkey_entries = entries.len()
                                        );
                                        read_back = true;
                                        report.commit_epoch = Some(commit_epoch);
                                        report.reveal_epoch = Some(reveal_epoch);
                                        report.reveal_round = Some(reveal_round);
                                        report.commits.push(CommitRecord {
                                            operation_id: tracked.0.clone(),
                                            tempo: tracked.1,
                                            attempts: status.attempts,
                                            finalized_success_block: block,
                                            commit_epoch,
                                            reveal_epoch,
                                            commit_block,
                                            reveal_round,
                                            hotkey_entries: entries.len(),
                                            weights_match: None,
                                        });
                                        // A long run reports its recent commits.
                                        if report.commits.len() > crate::app::REPORT_RECENT {
                                            report.commits.remove(0);
                                        }
                                        if entries.len() != 1 {
                                            tracing::error!(
                                                event = "commit_count_unexpected",
                                                commit_epoch,
                                                hotkey_entries = entries.len()
                                            );
                                        }
                                        if entries.len() > 1 {
                                            report.stop = "duplicate_commit".into();
                                            return Ok(());
                                        }
                                    }
                                    Err(error) => tracing::warn!(
                                        event = "commit_read_back_failed",
                                        error = %format!("{error:#}")
                                    ),
                                }
                            }
                        }
                        None if caught_up => {
                            // Revealed before this process observed it (an
                            // earlier run read it back); nothing to read now.
                            tracing::info!(
                                event = "commit_read_back_skipped",
                                operation_id = %tracked.0,
                                finalized_success_block = block
                            );
                            read_back = true;
                        }
                        None => {}
                    }
                }

                // Every completed reveal the ledger attributes is compared
                // with that operation's own committed row, read at the block
                // that observed the reveal, whichever process committed it.
                if caught_up {
                    let period = match period {
                        Some(period) => period,
                        None => *period.insert(self.reveal_period().await?),
                    };
                    let controls = self.adapter.as_ref().context("journal is not open")?.controls();
                    ledger.settle(|entry| {
                        Ok(match controls.retained_row(&entry.operation_id)? {
                            Some((hash, handed_off)) if hash == entry.row_hash => {
                                match controls
                                    .operation(&entry.operation_id)?
                                    .and_then(|status| status.finalized_success_block)
                                {
                                    Some(block) => OpState::Landed(block),
                                    None if handed_off => OpState::Retired,
                                    None => OpState::Pending,
                                }
                            }
                            _ => OpState::Gone,
                        })
                    })?;
                    let pairs: Vec<(u64, Option<u64>)> = outstanding
                        .iter()
                        .map(|reveal| (reveal.inclusion_block, reveal.reveal_epoch))
                        .collect();
                    for block in ledger.assign_epochs(&pairs, period) {
                        match self.epoch_index_at(block).await {
                            Ok(epoch) => ledger.set_epoch(block, epoch),
                            Err(error) => tracing::warn!(
                                event = "reveal_epoch_unavailable",
                                inclusion_block = block,
                                error = %format!("{error:#}")
                            ),
                        }
                    }
                    let completed: Vec<Completed> = self
                        .adapter
                        .as_ref()
                        .context("journal is not open")?
                        .reveals()
                        .outcomes()
                        .iter()
                        .filter(|outcome| outcome.mechanism == mechanism)
                        .map(|outcome| Completed {
                            commit_epoch: outcome.commit_epoch,
                            reveal_epoch: outcome.reveal_epoch,
                            round: outcome.round,
                            observed_block: outcome.observed_block,
                            missed: outcome.missed.is_some(),
                        })
                        .collect();
                    let newest = completed.last().copied();
                    for check in ledger.checks(&completed) {
                        match check {
                            Check::Compare { entry, outcome } => {
                                let uid = self.own_uid().await?.context("hotkey lost its UID")?;
                                let chain = match self
                                    .read_weights_at(uid, mechanism.0, BlockRef::Number(outcome.observed_block))
                                    .await
                                {
                                    Ok(chain) => Some(chain),
                                    // A node that pruned the reveal block's
                                    // state: the newest reveal is still current.
                                    Err(_) if newest == Some(outcome) => self
                                        .read_weights_at(uid, mechanism.0, BlockRef::Hash(header.hash))
                                        .await
                                        .ok(),
                                    Err(_) => None,
                                };
                                let Some(chain) = chain else {
                                    tracing::warn!(
                                        event = "reveal_unverifiable",
                                        operation_id = %entry.operation_id,
                                        commit_epoch = outcome.commit_epoch,
                                        observed_block = outcome.observed_block,
                                        alert = true
                                    );
                                    continue;
                                };
                                let matches = chain == entry.entries;
                                tracing::info!(
                                    event = "reveal_completed",
                                    operation_id = %entry.operation_id,
                                    commit_epoch = outcome.commit_epoch,
                                    observed_block = outcome.observed_block,
                                    missed = false,
                                    height = header.number,
                                    weights_match = matches
                                );
                                report.reveals_compared += 1;
                                report.chain_weights_match = Some(matches);
                                report.chain_weights = chain;
                                if let Some(record) = report
                                    .commits
                                    .iter_mut()
                                    .find(|record| record.operation_id == entry.operation_id)
                                {
                                    record.weights_match = Some(matches);
                                }
                                if !matches {
                                    report.stop = "weights_mismatch".into();
                                    ledger.persist()?;
                                    return Ok(());
                                }
                                if !per_tempo && entry.operation_id == tracked.0 {
                                    report.stop = "revealed".into();
                                    ledger.persist()?;
                                    return Ok(());
                                }
                            }
                            Check::Missed { entry, outcome } => {
                                tracing::warn!(
                                    event = "reveal_missed",
                                    operation_id = %entry.operation_id,
                                    commit_epoch = outcome.commit_epoch,
                                    observed_block = outcome.observed_block,
                                    alert = true
                                );
                                report.reveals_missed += 1;
                            }
                            Check::Unattributed(outcome) => {
                                tracing::info!(
                                    event = "reveal_observed",
                                    commit_epoch = outcome.commit_epoch,
                                    missed = outcome.missed,
                                    attributed = false
                                );
                                report.reveals_unattributed += 1;
                            }
                        }
                    }
                    if let Some(newest) = newest {
                        for stale in ledger.expire(newest.reveal_epoch, period) {
                            tracing::warn!(
                                event = "reveal_unobserved",
                                operation_id = %stale.operation_id,
                                commit_epoch = ?stale.commit_epoch,
                                alert = true
                            );
                        }
                    }
                    ledger.persist()?;
                }

                // A fetched bundle: refresh the authorization in place, or
                // switch to a verified successor once nothing is in flight.
                // A failed decision (a chain read, an install write) is
                // retried with the next outcome; it never ends the run.
                let outcome = background.as_mut().and_then(Background::latest);
                let step = fetch_state.step(self, outcome).await;
                if step.is_ok() {
                    last_decision_error = None;
                }
                match step {
                    Ok(LoopStep::Refreshed) => {
                        if let Some(staged) = fetch_state.latest() {
                            authority = Some(staged.authorized.clone());
                            self.loop_installed = Some(InstalledBundle {
                                sha256: staged.sha256.clone(),
                                info: staged.info.clone(),
                                authorized: staged.authorized.clone(),
                            });
                        }
                        if authorization_paused {
                            tracing::info!(event = "authorization_resumed", height = header.number);
                            authorization_paused = false;
                            retry_after_epoch = None;
                        }
                    }
                    Ok(LoopStep::Switched) => {
                        report.last_fetch = fetch_state.last_code().to_owned();
                        report.stop = "superseded".into();
                        return Ok(());
                    }
                    Ok(LoopStep::Continue) => {}
                    Err(error) => {
                        // Retried with the next outcome on the installed
                        // bundle; alerted once per distinct failure.
                        let text = format!("{error:#}");
                        if last_decision_error.as_deref() != Some(text.as_str()) {
                            tracing::warn!(event = "fetch_decision_failed", error = %text, alert = true);
                            last_decision_error = Some(text);
                        }
                    }
                }
                report.last_fetch = fetch_state.last_code().to_owned();

                // Journaled revocations take effect at the next finalized
                // head: the withdrawn target's queued work loses its signing
                // grant (no new attempt is signed), work that never signed is
                // retired, and new commitments pause until a newer
                // authorization is installed.
                if let Some(current) = &authority
                    && self.revocation_head(current, &mut revocation_watch, mechanism.0)?.revoked.is_some()
                {
                    authorization_paused = true;
                }

                // Commitments whose reveal is not yet compared: rtao still
                // tracks one, its watcher is behind this head, or a landed
                // ledger entry still lacks its commit epoch.
                let awaiting_reveals = !outstanding.is_empty()
                    || !caught_up
                    || ledger
                        .entries()
                        .iter()
                        .any(|entry| entry.inclusion_block.is_some() && entry.commit_epoch.is_none());

                // Plan decision 12: once the tracked commit is finalized,
                // handed back by rtao and read back, queue the same target's
                // re-commit for the next epoch. rtao signs it at that epoch's
                // offset and never twice in one epoch. No commitment of the
                // old target is queued while a switch waits.
                if per_tempo && read_back && !fetch_state.waiting() {
                    let controls = self
                        .adapter
                        .as_ref()
                        .context("journal is not open")?
                        .controls();
                    let operations = controls.target_operations(&derived.work)?;
                    let tracked_done = operations
                        .iter()
                        .any(|op| op.id == tracked.0 && op.handed_off);
                    if tracked_done && operations.iter().all(|op| op.handed_off) {
                        let period = match period {
                            Some(period) => period,
                            None => *period.insert(self.reveal_period().await?),
                        };
                        let last = match self.last_commit_epoch(mechanism, period) {
                            Some(epoch) => epoch,
                            None => {
                                // The watcher holds no commit: fall back to
                                // finalized chain state.
                                let epoch = self.finalized_epoch_index().await?;
                                if self.hotkey_commits(mechanism.0, epoch).await?.is_empty() {
                                    epoch.saturating_sub(1)
                                } else {
                                    epoch
                                }
                            }
                        };
                        let retained: Vec<_> = operations.iter().map(|op| op.tempo).collect();
                        // The label follows the journal's last commit; after
                        // a deferral it is never behind the finalized epoch.
                        let finalized_epoch = self.finalized_epoch_index().await?;
                        // Without a fetcher, a paused authorization keeps
                        // the run only until its outstanding reveals compared.
                        let due = retry_after_epoch.is_none_or(|after| finalized_epoch > after)
                            || (authorization_paused && background.is_none() && !awaiting_reveals);
                        if due {
                            let label = crate::app::next_tempo(last.max(finalized_epoch.saturating_sub(1)), &retained)?;
                            // reward-v1: each tempo's row is derived afresh
                            // against a finalized snapshot and owner hotkey,
                            // under the bundle's authorization checked again.
                            match self.rederive(derived, authority.clone(), &header, label).await? {
                                Ok((next, choice, label)) => {
                                    let owner_rows = choice.policy();
                                    let id = match self.adapter_mut()?.enqueue_with_owner_rows(
                                        &next.work,
                                        next.input.clone(),
                                        owner_rows,
                                    )? {
                                        Enqueued::Created(id) | Enqueued::Existing(id) => id,
                                    };
                                    let hash = slop_ninja_subnet::weight_row_hash(next.input.clone())?;
                                    self.authorized.grant(&id, &hash, choice);
                                    ledger.record(&id, &hash, next.chain_entries());
                                    ledger.persist()?;
                                    tracing::info!(
                                        event = "tempo_enqueued",
                                        operation_id = %id,
                                        tempo = label,
                                        last_commit_epoch = last,
                                        registration_block = ?next.snapshot_block,
                                        row_digest = %next.row_digest,
                                        owner_rows = ?owner_rows
                                    );
                                    report.tempo_enqueued.push((label, id.clone()));
                                    if report.tempo_enqueued.len() > crate::app::REPORT_RECENT {
                                        report.tempo_enqueued.remove(0);
                                    }
                                    faults.hit(Phase::TempoEnqueued)?;
                                    tracked = (id, Some(label));
                                    tracked_since = None;
                                    tracked_attempts = None;
                                    window_alerted = false;
                                    attempt_alert = None;
                                    limits_checked_for = None;
                                    read_back = false;
                                    retry_after_epoch = None;
                                    authorization_paused = false;
                                }
                                Err(refusal) if crate::app::is_row_deferral(refusal.code) => {
                                    // Nothing to commit this tempo; a later
                                    // snapshot is tried afresh next epoch.
                                    crate::app::log_tempo_deferred(Some(label), refusal.code);
                                    report.tempo_deferred.push((label, refusal.code.to_owned()));
                                    if report.tempo_deferred.len() > crate::app::REPORT_RECENT {
                                        report.tempo_deferred.remove(0);
                                    }
                                    retry_after_epoch = Some(finalized_epoch);
                                }
                                Err(refusal)
                                    if matches!(refusal.code, "authorization_refused" | "authorization_revoked")
                                        && (background.is_some() || awaiting_reveals) =>
                                {
                                    // The authorization yields no commitment
                                    // (revoked, or its target is no longer
                                    // the held one): new commitments pause
                                    // here, reconciliation continues, and the
                                    // fetcher's newer authorization or
                                    // successor resumes them. Without a
                                    // fetcher the run stays only until this
                                    // target's outstanding reveals compared,
                                    // then ends to read the bundle again.
                                    if !authorization_paused {
                                        tracing::warn!(
                                            event = "authorization_paused",
                                            code = refusal.code,
                                            tempo = label,
                                            awaiting_reveals,
                                            fetcher = background.is_some(),
                                            alert = true
                                        );
                                    }
                                    authorization_paused = true;
                                    report.tempo_deferred.push((label, refusal.code.to_owned()));
                                    if report.tempo_deferred.len() > crate::app::REPORT_RECENT {
                                        report.tempo_deferred.remove(0);
                                    }
                                    retry_after_epoch = Some(finalized_epoch);
                                }
                                Err(refusal) => {
                                    // The authorization yields nothing or the
                                    // row cannot be derived: end the run,
                                    // which the caller retries after reading
                                    // the bundle and revocations again.
                                    tracing::warn!(event = "tempo_deferred", tempo = label, code = refusal.code);
                                    report.stop = format!("tempo_deferred:{}", refusal.code);
                                    return Ok(());
                                }
                            }
                        }
                    }
                }

                let controls = self
                    .adapter
                    .as_ref()
                    .context("journal is not open")?
                    .controls();
                let operations = controls.target_operations(&derived.work)?;
                report.attempts = operations.iter().map(|op| op.status.attempts).sum();
                report.unresolved_attempts = operations.iter().map(|op| op.status.unresolved).sum();
                report.finalized_success_block = operations
                    .iter()
                    .filter_map(|op| op.status.finalized_success_block)
                    .max();
                report.outstanding_reveals = outstanding.len();
            }
            if report.stop.is_empty() {
                report.stop = "max_finalized_blocks".into();
            }
            Ok(())
        }
        .await;
        // Stop the fetcher and the graph worker; the shared client stays
        // open for the caller.
        drop(background);
        self.loop_installed = None;
        cx.shutdown.cancel();
        cx.metagraph.shutdown().await;
        result?;
        Ok(report)
    }
}

/// The kind of an rtao outcome that leaves queued work unsigned and needs an
/// operator's attention, or `None`: `failed` (RPC, preparation or an
/// unsigned submission failure, including an admission-window overrun),
/// `refused:<check>` (a certain chain rejection before signing),
/// `hold_previous` (rtao's owner-row guard held the queued row),
/// `paused_surface` and `skipped`. Scheduling waits, unresolved work (which
/// recovery classifies) and submissions are `None`. Content-free: the
/// failure text, which rtao logs itself, is not included.
pub fn not_submitted(outcome: &rtao_neuron::weights::WeightAttemptOutcome) -> Option<String> {
    use rtao_neuron::weights::WeightAttemptOutcome as O;
    match outcome {
        O::Failed(_) => Some("failed".into()),
        O::Refused(refusal) => Some(format!("refused:{}", refusal.name)),
        O::HoldPrevious => Some("hold_previous".into()),
        O::PausedSurface { .. } => Some("paused_surface".into()),
        O::Skipped(_) => Some("skipped".into()),
        O::Scheduled | O::RateLimited { .. } | O::Unresolved(_) | O::Submitted { .. } => None,
    }
}

impl LivePorts {
    /// The next tempo's commitment, its owner-row policy and its label:
    /// reward-v1 derives the row afresh against the finalized registrations
    /// and owner hotkey at `header`, under the bundle's authorization checked
    /// for the label, and resolves it against the journal exactly as the
    /// first commitment does ([`crate::app::derive_queueable`]): a row that
    /// must burn while the owner hotkey holds no UID, that rtao would hold
    /// under its policy, or a burn row the chain limits refuse is deferred; a
    /// retained operation of the label is joined, or retired and the next
    /// label used, never re-queued under another policy. A
    /// `testnet-reward-v0` package keeps its one row under `Hold`.
    async fn rederive(
        &mut self,
        derived: &Derived,
        authority: Option<Arc<crate::package_v2::Authorized>>,
        header: &BlockHeader,
        label: u64,
    ) -> Result<std::result::Result<(Derived, OwnerRowChoice, u64), Refusal>> {
        let Some(authorized) = authority else {
            return Ok(derived
                .work_for_tempo(self.identity.hotkey.0, label)
                .map(|work| (Derived { work, ..derived.clone() }, OwnerRowChoice::Hold, label)));
        };
        let head = FinalizedHead {
            number: header.number,
            hash: header.hash,
        };
        let chain = authorized.verified.chain.clone();
        let v1 = self.registration_snapshot(&head, &chain).await?;
        let hold = self.hold_hotkeys(&head).await?;
        let (own, owner) = crate::app::own_and_owner(&hold)?;
        let snapshot = match crate::package_v2::snapshot(&v1, owner) {
            Ok(s) => s,
            Err(refusal) => return Ok(Err(refusal)),
        };
        Ok(crate::app::derive_queueable(self, authorized, &snapshot, &v1, own, owner, label, Mode::Dispatch)
            .await?
            .map(|(next, policy, label)| (next, OwnerRowChoice::of(policy, &v1, own, owner), label)))
    }

    fn status(&self, operation_id: &str) -> Result<slop_ninja_subnet::OperationStatus> {
        Ok(self
            .adapter
            .as_ref()
            .context("journal is not open")?
            .controls()
            .operation(operation_id)?
            .unwrap_or_default())
    }

    /// Authorize every retained operation of the derived target whose row is
    /// the verified row (work recovered from an earlier process waits for this),
    /// and choose the operation to track: the pending one if any, else the
    /// latest. A retained operation of this target with another row is never
    /// authorized.
    fn adopt_target(
        &mut self,
        derived: &Derived,
        row_hash: &str,
        operation_id: &str,
    ) -> Result<(String, Option<u64>)> {
        let operations = self
            .adapter
            .as_ref()
            .context("journal is not open")?
            .controls()
            .target_operations(&derived.work)?;
        let mut tracked = (operation_id.to_owned(), derived.work.tempo);
        // Every retained operation of this target with the verified row has
        // the same owner-row choice as the one the application authorized:
        // the choice follows from the row, the target and the owner.
        let owner_rows = self
            .authorized
            .get(operation_id)
            .map_or(OwnerRowChoice::Hold, |grant| grant.owner_rows);
        for operation in &operations {
            if operation.row_hash != row_hash {
                tracing::warn!(
                    event = "target_operation_row_differs",
                    operation_id = %operation.id,
                    tempo = ?operation.tempo
                );
                continue;
            }
            self.authorized.grant(&operation.id, row_hash, owner_rows);
        }
        let pending: Vec<_> = operations.iter().filter(|op| !op.handed_off).collect();
        // A retired operation (superseded before it signed) is never tracked:
        // it will never commit, so nothing could be read back from it.
        let controls = self.adapter.as_ref().context("journal is not open")?.controls();
        let mut live = Vec::new();
        for op in &operations {
            if controls.retired(&op.id)?.is_none() {
                live.push(op);
            }
        }
        if let Some(op) = pending.first() {
            tracked = (op.id.clone(), op.tempo);
        } else if let Some(op) = live.last() {
            tracked = (op.id.clone(), op.tempo);
        }
        tracing::info!(
            event = "target_adopted",
            operations = operations.len(),
            pending = pending.len(),
            tracked = %tracked.0,
            tempo = ?tracked.1
        );
        Ok(tracked)
    }
}

/// What a fetch did, for the run's log. Content-free.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct FetchReport {
    pub outcome: String,
    pub decision: Option<Decision>,
    /// The bundle file at `[package] path` was replaced.
    pub installed: bool,
    /// A dry run staged the bundle it would install in memory instead.
    pub staged_in_memory: bool,
    pub staged: Option<BundleInfo>,
    pub stats: Option<crate::fetch::Stats>,
    pub detail: Option<String>,
    /// The host served records that failed a check ([`Outcome::alert`]).
    pub alert: bool,
    /// A later session may complete what this one could not.
    pub retryable: bool,
}

/// The background fetcher of a dispatch run: one session per poll interval,
/// each outcome sent to the run, which decides at its next finalized block.
pub struct Background {
    handle: tokio::task::JoinHandle<()>,
    outcomes: tokio::sync::mpsc::Receiver<Outcome>,
}

impl Background {
    /// The latest outcome sent since the last call, if any.
    pub fn latest(&mut self) -> Option<Outcome> {
        let mut latest = None;
        while let Ok(outcome) = self.outcomes.try_recv() {
            latest = Some(outcome);
        }
        latest
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Everything one fetch session needs, owned, so it can run in a task.
struct FetchSetup {
    transport: HttpTransport,
    cache: std::path::PathBuf,
    keys: TrustedKeys,
    chain: ChainBinding,
    held_path: std::path::PathBuf,
    /// [`crate::revocations::path`] of the journal: revocations the pointer
    /// names are journaled here.
    revocation_store: std::path::PathBuf,
    /// A session never outlives the interval to the next one.
    deadline: Duration,
    /// [`Staged::fingerprint`] of the last bundle a session produced.
    previous: Option<String>,
    keep: Arc<std::sync::Mutex<Vec<String>>>,
    dry_run: bool,
}

impl FetchSetup {
    fn held(&self) -> Result<slop_ninja_protocol::ActiveTarget> {
        match read_held(&self.held_path, &self.keys, !self.dry_run)? {
            Some(file) => file.restore(&self.chain).map_err(|r| anyhow::anyhow!("{}: {}", r.code, r.detail)),
            None => slop_ninja_protocol::ActiveTarget::launch(&self.chain).map_err(|e| anyhow::anyhow!("{e}")),
        }
    }

    /// One session, bounded in time by the poll interval. A session that
    /// times out leaves its partial objects to resume next time.
    async fn session(&mut self) -> Result<Outcome> {
        let held = self.held()?;
        let keep = self.keep.lock().map(|k| k.clone()).unwrap_or_default();
        let outcome = {
            let session = Session {
                transport: &self.transport,
                cache: &self.cache,
                keys: &self.keys,
                chain: &self.chain,
                held,
                previous: self.previous.as_deref(),
                keep: &keep,
                revocations: Some(&self.revocation_store),
            };
            match tokio::time::timeout(self.deadline, session.run()).await {
                Ok(result) => result?,
                Err(_) => bail!(
                    "fetch_timed_out: the session did not finish within {} s",
                    self.deadline.as_secs()
                ),
            }
        };
        if let Outcome::Ready(staged) = &outcome {
            self.previous = Some(staged.fingerprint.clone());
        }
        Ok(outcome)
    }
}

/// Reads the held-target record at `path` for the authority `keys`. `None`
/// when there is none, or when another authority wrote it: that record is
/// set aside (renamed `<name>.authority-<digest prefix>`) when `migrate`, and
/// the validator starts again from the launch burn row. A record written
/// before records named their authority is bound to `keys` when `migrate`.
pub(crate) fn read_held(
    path: &std::path::Path,
    keys: &TrustedKeys,
    migrate: bool,
) -> Result<Option<crate::package_v2::HeldTargetFile>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("held target record is unreadable"),
    };
    let file: crate::package_v2::HeldTargetFile =
        serde_json::from_slice(&bytes).context("held target record is malformed")?;
    let current = crate::package_v2::authority_digest(keys);
    match &file.authority {
        Some(authority) if *authority == current => Ok(Some(file)),
        Some(authority) => {
            tracing::warn!(event = "held_target_authority_changed", set_aside = migrate, alert = true);
            if migrate {
                let mut aside = path.as_os_str().to_os_string();
                aside.push(format!(".authority-{}", authority.get(..16).unwrap_or(authority)));
                std::fs::rename(path, aside).context("held target set aside")?;
            }
            Ok(None)
        }
        None => {
            if migrate {
                let bound = file.clone().bound_to(keys);
                crate::supersede::write_private_atomic(path, &serde_json::to_vec_pretty(&bound)?)?;
                tracing::info!(event = "held_target_bound");
            }
            Ok(Some(file))
        }
    }
}

impl LivePorts {
    /// The fetcher, built once per process.
    fn fetch_setup(&mut self) -> Result<Option<Arc<tokio::sync::Mutex<FetchSetup>>>> {
        if let Some(setup) = &self.fetcher {
            return Ok(Some(setup.clone()));
        }
        let Some(fetch) = &self.config.fetch else {
            return Ok(None);
        };
        let deadline = Duration::from_secs(fetch.poll_secs);
        let setup = Arc::new(tokio::sync::Mutex::new(FetchSetup {
            transport: HttpTransport::new(
                fetch.base()?,
                Duration::from_secs(fetch.stall_secs),
                deadline,
                &self.config.rpc.user_agent,
            )?,
            cache: fetch.cache_dir(&self.config.journal_path)?,
            keys: self.trusted_keys()?,
            chain: self.chain_binding(),
            held_path: held_target_path(&self.config.journal_path),
            revocation_store: crate::revocations::path(&self.config.journal_path),
            deadline,
            previous: None,
            keep: self.keep_cohorts.clone(),
            dry_run: self.dry_run,
        }));
        self.refresh_keep_cohorts(None);
        self.fetcher = Some(setup.clone());
        Ok(Some(setup))
    }

    /// Records the installed bundle's cohorts (`bytes`, or the installed
    /// file), which the cache keeps.
    fn refresh_keep_cohorts(&self, bytes: Option<&[u8]>) {
        let Some(package) = &self.config.package else {
            return;
        };
        let cohorts = match bytes.or(self.staged_bundle.as_deref()) {
            Some(bytes) => crate::fetch::bundle_cohorts(bytes),
            None => match std::fs::read(&package.path) {
                Ok(bytes) if crate::package_v2::is_v2(&bytes) => crate::fetch::bundle_cohorts(&bytes),
                _ => Ok(Vec::new()),
            },
        };
        match cohorts {
            Ok(cohorts) => {
                if let Ok(mut keep) = self.keep_cohorts.lock() {
                    *keep = cohorts;
                }
            }
            Err(error) => tracing::warn!(event = "installed_bundle_unreadable", error = %format!("{error:#}")),
        }
    }

    fn chain_binding(&self) -> ChainBinding {
        ChainBinding {
            genesis: slop_ninja_protocol::Digest(self.identity.genesis.0),
            netuid: self.identity.netuid.0,
            mechanism: 0,
        }
    }

    /// Whether the fetcher is configured.
    pub fn fetches(&self) -> bool {
        self.config.fetch.is_some()
    }

    /// One fetch session without chain access, for the `fetch` command: the
    /// bundle is assembled and verified against the held target, never
    /// installed. Returns the report and the verified bundle.
    pub async fn fetch_only(&mut self) -> Result<(FetchReport, Option<Box<Staged>>)> {
        let setup = self.fetch_setup()?.context("no [fetch] is configured")?;
        let outcome = setup.lock().await.session().await?;
        Ok(report_of(outcome))
    }

    /// The target this validator holds active, from its own record.
    fn held_active(&self) -> Result<slop_ninja_protocol::ActiveTarget> {
        let chain = self.chain_binding();
        match AppPorts::held_target(self)? {
            Some(file) => file.restore(&chain).map_err(|r| anyhow::anyhow!("{}: {}", r.code, r.detail)),
            None => slop_ninja_protocol::ActiveTarget::launch(&chain).map_err(|e| anyhow::anyhow!("{e}")),
        }
    }

    /// Completes an install interrupted after its bundle was staged
    /// ([`InstallPorts::stage_bundle`]): a staged bundle that verifies
    /// against the held record (the record moved past the intermediate
    /// targets) is installed and its activation recorded; one that does not
    /// (the record never moved) is discarded, leaving the installed bundle.
    /// A dry run writes nothing and only stages a completing bundle in
    /// memory. Call it with the journal held, before the first verification.
    pub fn complete_install(&mut self) -> Result<Option<&'static str>> {
        let Some(package) = self.config.package.clone() else {
            return Ok(None);
        };
        let path = staged_bundle_path(&package.path);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("the staged bundle is unreadable"),
        };
        let held = self.held_active()?;
        let verified = if crate::package_v2::is_v2(&bytes) {
            crate::package_v2::verify(&bytes, &self.trusted_keys()?, &self.identity.genesis.0, self.identity.netuid.0, &held).ok()
        } else {
            None
        };
        let outcome = match verified {
            Some(authorized) => {
                self.write_bundle(&bytes)?;
                AppPorts::record_target(self, &crate::package_v2::HeldTargetFile::of(&authorized.verified.active))?;
                "completed"
            }
            None => {
                if !self.dry_run {
                    remove_if_present(&path)?;
                }
                "discarded"
            }
        };
        tracing::info!(event = "staged_install", outcome, dry_run = self.dry_run);
        Ok(Some(outcome))
    }

    /// The installed bundle as a [`Candidate`] at `epoch`, or `None` when
    /// there is none or it no longer verifies against the held target.
    fn installed_candidate(&self, epoch: u64) -> Result<Option<(Candidate, Arc<crate::package_v2::Authorized>)>> {
        let package = self.config.package.as_ref().context("no [package] is configured")?;
        let bytes = match &self.staged_bundle {
            Some(bytes) => bytes.clone(),
            None => match std::fs::read(&package.path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error).context("package.path is unreadable"),
            },
        };
        if !crate::package_v2::is_v2(&bytes) {
            return Ok(None);
        }
        let held = self.held_active()?;
        let Ok(authorized) =
            crate::package_v2::verify(&bytes, &self.trusted_keys()?, &self.identity.genesis.0, self.identity.netuid.0, &held)
        else {
            return Ok(None);
        };
        let info = BundleInfo::of(&bytes, &authorized)?;
        let covers = authorized.check_epoch(self.identity.hotkey.0, epoch).is_ok();
        Ok(Some((
            Candidate::of(&crate::fetch::sha256_hex(&bytes), &info, covers),
            Arc::new(authorized),
        )))
    }

    fn pending(&self) -> Result<Vec<Pending>> {
        Ok(self.pending_operations(0)?.iter().map(Pending::from).collect())
    }

    /// Decides and applies a verified bundle against the installed one at
    /// the finalized `epoch`; returns the decision and whether it installed.
    /// Without `journal_writes` (a dry run), a switch that would retire
    /// journaled work is kept for a dispatch run instead.
    pub(crate) fn settle_staged(
        &mut self,
        staged: &Staged,
        installed: Option<&Candidate>,
        epoch: u64,
        journal_writes: bool,
    ) -> Result<(Decision, bool)> {
        let covers = staged.authorized.check_epoch(self.identity.hotkey.0, epoch).is_ok();
        let candidate = Candidate::of(&staged.sha256, &staged.info, covers);
        let mut decision = crate::supersede::decide(installed, &candidate, epoch, &self.pending()?);
        if !journal_writes && matches!(&decision, Decision::Switch { retire } if !retire.is_empty()) {
            decision = Decision::Keep { reason: "switch_needs_dispatch" };
        }
        let installed_now = crate::supersede::install(self, staged, &decision)?;
        Ok((decision, installed_now))
    }

    /// The fetch before the application stage of `run`: one session, then
    /// the decision against the installed bundle, the journal and the
    /// finalized epoch. A failed session leaves the installed bundle. A dry
    /// run (`journal_writes` false) never retires journaled work and writes
    /// nothing: a bundle it would install is staged in memory for its own
    /// verification. A successor that must wait for in-flight work is handed
    /// to the dispatch loop, which starts waiting for it.
    pub async fn startup_fetch(&mut self, journal_writes: bool) -> Result<FetchReport> {
        let setup = self.fetch_setup()?.context("no [fetch] is configured")?;
        let outcome = setup.lock().await.session().await?;
        let (mut report, staged) = report_of(outcome);
        let Some(staged) = staged else {
            return Ok(report);
        };
        let epoch = self.finalized_epoch_index().await?;
        let installed = self.installed_candidate(epoch)?.map(|(c, _)| c);
        let (decision, installed_now) = self.settle_staged(&staged, installed.as_ref(), epoch, journal_writes)?;
        report.installed = installed_now && !self.dry_run;
        report.staged_in_memory = installed_now && self.dry_run;
        report.decision = Some(decision.clone());
        if matches!(decision, Decision::Wait { .. }) {
            self.startup_wait = Some((staged, decision, epoch));
        }
        Ok(report)
    }

    /// Waits outside the dispatch loop after a deferred tempo, until the
    /// finalized epoch passes the current one: a fetch session every poll
    /// interval with `[fetch]` (returning early once it installs a renewal or
    /// successor), else one epoch read a minute.
    pub async fn wait_after_deferral(&mut self) -> Result<()> {
        let start = self.finalized_epoch_index().await?;
        let poll = Duration::from_secs(self.config.fetch.as_ref().map_or(EPOCH_POLL_SECS, |f| f.poll_secs));
        tracing::info!(event = "deferral_wait", epoch = start, poll_secs = poll.as_secs());
        loop {
            tokio::time::sleep(poll).await;
            if self.fetches() {
                match self.startup_fetch(true).await {
                    Ok(report) => {
                        if report.outcome != "unchanged" {
                            tracing::info!(
                                event = "fetch_waiting",
                                report = %serde_json::to_string(&report).unwrap_or_default()
                            );
                        }
                        if report.installed {
                            return Ok(());
                        }
                    }
                    Err(error) => tracing::warn!(event = "fetch_failed", error = %format!("{error:#}")),
                }
            }
            match self.finalized_epoch_index().await {
                Ok(epoch) if epoch > start => return Ok(()),
                Ok(_) => {}
                Err(error) => tracing::warn!(event = "epoch_read_failed", error = %format!("{error:#}")),
            }
        }
    }

    /// Starts the background fetcher of a dispatch run.
    fn spawn_fetcher(&mut self) -> Result<Option<Background>> {
        let Some(setup) = self.fetch_setup()? else {
            return Ok(None);
        };
        let poll = Duration::from_secs(self.config.fetch.as_ref().map_or(crate::config::MIN_POLL_SECS, |f| f.poll_secs));
        let (sender, outcomes) = tokio::sync::mpsc::channel(1);
        let handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(poll).await;
                let result = setup.lock().await.session().await;
                match result {
                    Ok(outcome) => {
                        tracing::debug!(event = "fetch_session", outcome = outcome.code());
                        if sender.send(outcome).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => tracing::warn!(event = "fetch_failed", error = %format!("{error:#}")),
                }
            }
        });
        Ok(Some(Background { handle, outcomes }))
    }
}

/// The report of a session, and its verified bundle when it produced one.
fn report_of(outcome: Outcome) -> (FetchReport, Option<Box<Staged>>) {
    let code = outcome.code().to_owned();
    let (alert, retryable) = (outcome.alert(), outcome.retryable());
    let (mut report, staged) = match outcome {
        Outcome::Ready(staged) => (
            FetchReport {
                outcome: code,
                staged: Some(staged.info.clone()),
                stats: Some(staged.stats.clone()),
                ..FetchReport::default()
            },
            Some(staged),
        ),
        Outcome::Incomplete { cohort, detail, .. } => (
            FetchReport {
                outcome: code,
                detail: Some(match cohort {
                    Some(cohort) => format!("{cohort}: {detail}"),
                    None => detail,
                }),
                ..FetchReport::default()
            },
            None,
        ),
        Outcome::ReceiptsExpired { cohort } => (
            FetchReport { outcome: code, detail: Some(cohort), ..FetchReport::default() },
            None,
        ),
        Outcome::Refused(refusal) => (
            FetchReport { outcome: code, detail: Some(refusal.detail), ..FetchReport::default() },
            None,
        ),
        Outcome::Unpublished | Outcome::Unchanged => (FetchReport { outcome: code, ..FetchReport::default() }, None),
    };
    report.alert = alert;
    report.retryable = retryable;
    (report, staged)
}

impl InstallPorts for LivePorts {
    fn retire(&mut self, operation_id: &str, reason: &str) -> Result<bool> {
        AppPorts::retire(self, operation_id, reason)
    }

    fn record_target(&mut self, held: &crate::package_v2::HeldTargetFile) -> Result<()> {
        AppPorts::record_target(self, held)
    }

    fn stage_bundle(&mut self, bytes: &[u8]) -> Result<()> {
        if self.dry_run {
            return Ok(());
        }
        let package = self.config.package.as_ref().context("no [package] is configured")?;
        crate::supersede::write_private_atomic(&staged_bundle_path(&package.path), bytes)
    }

    fn write_bundle(&mut self, bytes: &[u8]) -> Result<()> {
        if self.dry_run {
            self.staged_bundle = Some(bytes.to_vec());
        } else {
            let package = self.config.package.as_ref().context("no [package] is configured")?;
            crate::supersede::write_private_atomic(&package.path, bytes)?;
            remove_if_present(&staged_bundle_path(&package.path))?;
        }
        self.installed_sha = Some(crate::fetch::sha256_hex(bytes));
        self.refresh_keep_cohorts(Some(bytes));
        Ok(())
    }
}

impl LoopPorts for LivePorts {
    async fn finalized_epoch(&mut self) -> Result<u64> {
        self.finalized_epoch_index().await
    }

    fn pending(&self) -> Result<Vec<Pending>> {
        LivePorts::pending(self)
    }

    fn installed_sha(&self) -> Option<String> {
        self.installed_sha.clone()
    }

    fn installed(&mut self, epoch: u64) -> Result<Option<Candidate>> {
        let revocations = AppPorts::revocations(self)?.records;
        Ok(self.loop_installed.as_ref().map(|installed| {
            installed.authorized.set_revocations(revocations);
            let covers = installed.authorized.check_epoch(self.identity.hotkey.0, epoch).is_ok();
            Candidate::of(&installed.sha256, &installed.info, covers)
        }))
    }

    fn covers(&self, staged: &Staged, epoch: u64) -> bool {
        // A store that cannot be read counts as no cover: nothing is
        // installed on a revocation check that did not run.
        let Ok(loaded) = AppPorts::revocations(self) else { return false };
        staged.authorized.set_revocations(loaded.records);
        staged.authorized.check_epoch(self.identity.hotkey.0, epoch).is_ok()
    }
}

/// The held-target record beside the journal: `<journal>.held-target.json`.
pub fn held_target_path(journal: &std::path::Path) -> std::path::PathBuf {
    let mut name = journal.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".held-target.json");
    journal.with_file_name(name)
}

/// A successor staged for an install that moves the held record past
/// intermediate targets: `<package path>.next`.
pub fn staged_bundle_path(package: &std::path::Path) -> std::path::PathBuf {
    let mut name = package.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".next");
    package.with_file_name(name)
}

fn remove_if_present(path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            if let Some(dir) = path.parent() {
                std::fs::File::open(dir)?.sync_all()?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("remove the staged bundle"),
    }
}

/// `reset-target`: sets aside the held-target record and the installed
/// bundle, each renamed with a `.reset-<unix seconds>` suffix, so the next
/// run starts again from the launch burn row (for a restarted chain record
/// under the same trust roots). It holds the journal's lifetime lock while it
/// does so, so it refuses while a run holds the journal. The journal, its
/// retained work and the reveal ledger are untouched: work of the old target
/// that never landed is retired as superseded by the next dispatch run.
/// Journals a signed revocation from `file` ([`crate::revocations::record`]),
/// checked under the configured trust roots and this chain. Safe while a
/// run holds the journal: the store has its own lock, and the running loop
/// applies it at its next finalized head. Reads no chain and loads no key.
pub fn record_revocation(config: &ValidatorConfig, file: &std::path::Path) -> Result<serde_json::Value> {
    let keys = config.package.as_ref().context("no [package] is configured")?.trusted_keys.keys()?;
    let chain = ChainBinding {
        genesis: slop_ninja_protocol::Digest(config.network.genesis()?.0),
        netuid: config.netuid,
        mechanism: 0,
    };
    let bytes = std::fs::read(file).context("revocation file")?;
    let recorded = crate::revocations::record(&crate::revocations::path(&config.journal_path), &keys, &chain, &bytes)?;
    Ok(serde_json::json!({
        "revocation": recorded.entry.digest,
        "target": recorded.entry.target,
        "authorization": recorded.entry.authorization,
        "issued_epoch": recorded.entry.issued_epoch,
        "new": recorded.new,
    }))
}

pub fn reset_target(config: &ValidatorConfig) -> Result<serde_json::Value> {
    let _lock = slop_ninja_subnet::JournalControls::open_existing(&config.journal_path)
        .context("the journal is held by a running validator, or missing; stop the unit first")?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let set_aside = |path: &std::path::Path| -> Result<bool> {
        if !path.exists() {
            return Ok(false);
        }
        let mut aside = path.as_os_str().to_os_string();
        aside.push(format!(".reset-{stamp}"));
        std::fs::rename(path, &aside).context("rename")?;
        Ok(true)
    };
    let held = set_aside(&held_target_path(&config.journal_path))?;
    let bundle = match &config.package {
        Some(package) => {
            let staged = set_aside(&staged_bundle_path(&package.path))?;
            set_aside(&package.path)? || staged
        }
        None => false,
    };
    Ok(serde_json::json!({
        "held_target_set_aside": held,
        "bundle_set_aside": bundle,
        "suffix": format!(".reset-{stamp}"),
        "next": "the next run starts from the launch burn row and needs a launch bundle (fetched, or installed by the operator)",
    }))
}

/// The `rollback-check` command: with the unit stopped (the journal's
/// lifetime lock refuses a held journal), optionally retires the work a
/// build at the previous rtao pin would refuse and that never signed, then
/// reports what still blocks that build from opening the journal
/// ([`slop_ninja_subnet::JournalControls::rollback_blockers`]). Returns the
/// redacted report and whether nothing blocks the rollback.
pub fn rollback_check(config: &ValidatorConfig, retire: bool) -> Result<(serde_json::Value, bool)> {
    let controls = slop_ninja_subnet::JournalControls::open_existing(&config.journal_path)
        .context("the journal is held by a running validator, or missing; stop the unit first")?;
    let retired = if retire { controls.retire_for_rollback()? } else { Vec::new() };
    let blockers = controls.rollback_blockers()?;
    let safe = blockers.is_empty();
    let next = if safe {
        "a build at the previous pin can open this journal; install it before starting the unit again"
    } else if blockers.iter().any(|b| b.operation_id.is_none()) {
        "the weight checkpoint holds state from a followed non-native commit-reveal tag: roll forward only"
    } else if blockers.iter().any(|b| b.retirable) {
        "run again with --retire"
    } else {
        "run this build until the listed work is handed back, then stop the unit and check again"
    };
    Ok((
        serde_json::json!({
            "previous_pin": slop_ninja_subnet::PREVIOUS_RTAO_PIN,
            "retired": retired,
            "blockers": blockers,
            "rollback_safe": safe,
            "next": next,
        }),
        safe,
    ))
}

/// Read-only finalized observation for operators and drills: epoch state,
/// this hotkey's entries in `TimelockedWeightCommits` for the last `epochs`
/// epochs (and every hotkey's entry count), its `Weights` row, and the
/// subnet's `Incentive` and `LastUpdate` at its UID and at the row's UIDs.
/// Every item is read by name; no journal, key or signer is opened. Other
/// hotkeys appear only as counts. `at` reads a past finalized block instead
/// of the finalized head (for example a commit queue before its reveal).
pub async fn observe(
    config: &ValidatorConfig,
    epochs: u64,
    at: Option<u64>,
) -> Result<serde_json::Value> {
    let identity = chain_identity(config)?;
    let client = Client::connect(client_config(config, false)?).await?;
    let result = async {
        let finalized = client.at(BlockRef::Finalized).await?;
        // A past block must be finalized; the endpoint must still hold its state.
        let snapshot = match at {
            Some(number) => {
                ensure!(
                    number <= finalized.block().number,
                    "observe --at names a block that is not finalized"
                );
                client.at(BlockRef::Number(number)).await?
            }
            None => finalized,
        };
        let block = snapshot.block();
        let netuid = identity.netuid;
        let read = |item: &'static str, keys: Vec<Value>| {
            let snapshot = &snapshot;
            async move {
                snapshot
                    .query_batch_dyn("SubtensorModule", item, vec![keys])
                    .await
                    .with_context(|| format!("SubtensorModule.{item} read by name failed"))?
                    .into_iter()
                    .next()
                    .with_context(|| format!("SubtensorModule.{item} returned no value"))
            }
        };
        let integer = |value: rtao_core::DynValue, item: &str| -> Result<u64> {
            Ok(u64::try_from(u128_of(&value, item)?)?)
        };
        let epoch = integer(read("SubnetEpochIndex", keys::subnet(netuid)).await?, "SubnetEpochIndex")?;
        let last_epoch_block = integer(read("LastEpochBlock", keys::subnet(netuid)).await?, "LastEpochBlock")?;
        let tempo = integer(read("Tempo", keys::subnet(netuid)).await?, "Tempo")?;
        let period = integer(read("RevealPeriodEpochs", keys::subnet(netuid)).await?, "RevealPeriodEpochs")?;
        let uid = uid_from_value(&read("Uids", keys::hotkey(netuid, identity.hotkey)).await?)?;
        let first = epoch.saturating_sub(epochs.saturating_sub(1));
        let mut commits = Vec::new();
        for commit_epoch in first..=epoch {
            let value = read(
                "TimelockedWeightCommits",
                keys::timelocked_commits(netuid, 0, commit_epoch)?,
            )
            .await?;
            let entries = commits_from_value(value.as_core())?;
            let own: Vec<_> = entries
                .iter()
                .filter(|(account, _, _)| *account == identity.hotkey.0)
                .map(|(_, block, round)| serde_json::json!({"commit_block": block, "reveal_round": round}))
                .collect();
            commits.push(serde_json::json!({
                "commit_epoch": commit_epoch,
                "entries": entries.len(),
                "hotkey_entries": own.len(),
                "hotkey": own,
            }));
        }
        let index = vec![keys::storage_index(netuid, 0)?.into_core_value()];
        let list = |value: rtao_core::DynValue| -> Vec<u64> {
            match value.as_core() {
                Value::List(items) => items
                    .iter()
                    .map(|item| match item {
                        Value::Int(n) => u64::try_from(*n).unwrap_or(u64::MAX),
                        Value::Uint(n) => u64::try_from(*n).unwrap_or(u64::MAX),
                        _ => u64::MAX,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        };
        let incentive = list(read("Incentive", index.clone()).await?);
        let last_update = list(read("LastUpdate", index).await?);
        let weights = match uid {
            Some(uid) => weights_from_value(
                read("Weights", keys::weights(netuid, 0, uid)?).await?.as_core(),
            )?,
            None => Vec::new(),
        };
        let at = |values: &[u64], uid: u16| values.get(usize::from(uid)).copied();
        let row_uids: Vec<_> = weights
            .iter()
            .map(|(dest, weight)| {
                serde_json::json!({
                    "uid": dest,
                    "weight": weight,
                    "incentive": at(&incentive, *dest),
                })
            })
            .collect();
        Ok::<_, anyhow::Error>(serde_json::json!({
            "finalized": {"number": block.number, "hash": format!("0x{}", hex::encode(block.hash.0))},
            "netuid": netuid.0,
            "subnet_epoch_index": epoch,
            "last_epoch_block": last_epoch_block,
            "tempo": tempo,
            "reveal_period_epochs": period,
            "uid": uid,
            "last_update": uid.and_then(|uid| at(&last_update, uid)),
            "timelocked_commits": commits,
            "weights": weights,
            "row": row_uids,
        }))
    }
    .await;
    client.close().await;
    result
}

/// What [`LivePorts::revocation_head`] alerted already: the revocation that
/// withdrew the current authorization, and the revocation store's last read
/// failure or entry problems, each alerted once per distinct value.
#[derive(Debug, Default)]
pub(crate) struct RevocationWatch {
    revoked_alerted: Option<String>,
    store_alerted: Option<String>,
}

/// What one finalized head's revocation check found and did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RevocationHead {
    /// The journaled revocation withdrawing the current authorization.
    pub revoked: Option<slop_ninja_protocol::Digest>,
    /// Queued operations of its target whose signing grant was withdrawn.
    pub withdrawn: usize,
    /// Of those, the ones that never signed and were retired.
    pub retired: usize,
    /// Whether `authorization_revoked` was alerted at this head.
    pub alerted: bool,
}

impl LivePorts {
    /// The dispatch loop's per-head revocation check: journaled revocations
    /// take effect at the next finalized head. The store is read again
    /// (a read failure keeps the previous revocations in force; entry
    /// problems are applied from their summaries; each is alerted once per
    /// distinct text). When one withdraws `current`, every queued operation
    /// of its target on `mechanism` loses its signing grant (no new attempt
    /// is signed: `slop_ninja_authorization_revoked`), the ones that never
    /// signed are retired, and `authorization_revoked` is alerted once per
    /// revocation; the caller pauses new commitments until a newer
    /// authorization is fetched.
    pub(crate) fn revocation_head(
        &mut self,
        current: &crate::package_v2::Authorized,
        watch: &mut RevocationWatch,
        mechanism: u8,
    ) -> Result<RevocationHead> {
        match AppPorts::revocations(self) {
            Ok(loaded) => {
                if let Some(error) = &loaded.store_error {
                    if watch.store_alerted.as_deref() != Some(error.as_str()) {
                        tracing::warn!(
                            event = "revocation_store_unreadable",
                            error = %error,
                            kept = loaded.records.len(),
                            alert = true
                        );
                        watch.store_alerted = Some(error.clone());
                    }
                } else if loaded.problems.is_empty() {
                    watch.store_alerted = None;
                } else {
                    let text = loaded.problems.join("; ");
                    if watch.store_alerted.as_deref() != Some(text.as_str()) {
                        tracing::warn!(event = "revocation_entry_unreadable", problems = %text, alert = true);
                        watch.store_alerted = Some(text);
                    }
                }
                current.set_revocations(loaded.records);
            }
            Err(error) => {
                let text = format!("{error:#}");
                if watch.store_alerted.as_deref() != Some(text.as_str()) {
                    tracing::warn!(event = "revocation_store_unreadable", error = %text, alert = true);
                    watch.store_alerted = Some(text);
                }
            }
        }
        let Some(revocation) = current.revoked_by() else {
            watch.revoked_alerted = None;
            return Ok(RevocationHead::default());
        };
        let target = current.verified.target_digest.0;
        let mut head = RevocationHead { revoked: Some(revocation), ..RevocationHead::default() };
        for pending in AppPorts::pending_operations(self, mechanism)? {
            if pending.work.as_ref().and_then(|w| w.v2_target) != Some(target) {
                continue;
            }
            if self.authorized.withdraw(&pending.id) {
                head.withdrawn += 1;
            }
            if pending.retirable() && AppPorts::retire(self, &pending.id, "authorization_revoked")? {
                head.retired += 1;
            }
        }
        if watch.revoked_alerted.as_deref() != Some(revocation.hex().as_str()) {
            tracing::warn!(
                event = "authorization_revoked",
                target = %current.verified.target_digest,
                revocation = %revocation,
                withdrawn = head.withdrawn,
                retired = head.retired,
                alert = true
            );
            watch.revoked_alerted = Some(revocation.hex());
            head.alerted = true;
        }
        Ok(head)
    }
}

// Offline test helpers stay at the end of the file: the `no_runtime_gates`
// lint stops scanning a file at its first `#[cfg(test)]`.
#[cfg(test)]
impl LivePorts {
    /// Clear the opening pause so enqueue runs without a chain. Recovery stays
    /// incomplete, so resume and dispatch still refuse.
    pub(crate) fn unpause_for_offline_tests(&self) -> Result<()> {
        self.adapter
            .as_ref()
            .context("journal is not open")?
            .unpause_storage_for_offline_tests()
    }

    pub(crate) fn controls_for_tests(&self) -> Option<slop_ninja_subnet::JournalControls> {
        self.adapter.as_ref().map(NativeWeightAdapter::controls)
    }

    /// The signing guard's authorizations in this process.
    pub(crate) fn authorized_for_tests(&self) -> &Authorized {
        &self.authorized
    }

    /// Explicit resume through the adapter's real recovery gate.
    pub(crate) fn resume_for_tests(&mut self) -> Result<()> {
        self.adapter_mut()?.resume_after_recovery()
    }
}
