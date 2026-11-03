//! Operator configuration. Nothing chain-specific is defaulted: the subnet,
//! network, hotkey and endpoints are always explicit.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rtao_primitives::H256;
use serde::{Deserialize, Serialize};

/// Named public networks the validator accepts. Each has a pinned genesis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkName {
    Test,
    Finney,
}

impl NetworkName {
    /// rtao's preset label, which fixes the expected genesis and pacing.
    pub fn rtao_label(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Finney => "finney",
        }
    }

    /// Immutable genesis from the pinned rtao network table.
    pub fn genesis(self) -> Result<H256> {
        rtao_client::Network::resolve_with(self.rtao_label(), |_| None)?
            .genesis()?
            .context("named network has no pinned genesis")
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WalletConfig {
    /// Absolute wallet root, normally `~/.bittensor/wallets` expanded.
    pub path: PathBuf,
    pub name: String,
    pub hotkey: String,
}

impl WalletConfig {
    /// The hotkey keyfile path. Startup reads only its `pub.txt` companion.
    pub fn hotkey_file(&self) -> PathBuf {
        self.path
            .join(&self.name)
            .join("hotkeys")
            .join(&self.hotkey)
    }
}

fn default_user_agent() -> String {
    format!("slop_ninja_validator/{}", env!("CARGO_PKG_VERSION"))
}
const fn default_requests_per_minute() -> u32 {
    15
}
const fn default_startup_timeout() -> u64 {
    60
}
const fn default_signing_connect_quiet() -> u64 {
    60
}
const fn default_recovery_rounds() -> u32 {
    16
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcConfig {
    /// Primary first, then failover. Plain `wss://` URLs without credentials.
    pub endpoints: Vec<String>,
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
    /// Shared token-bucket budget for this process. Public pools reject
    /// sustained traffic near 25 requests per minute from one address.
    #[serde(default = "default_requests_per_minute")]
    pub max_requests_per_minute: u32,
    #[serde(default = "default_startup_timeout")]
    pub startup_timeout_secs: u64,
    /// Optional separate signing transport: a second connection, with its
    /// own budget (requests per minute, which is also its burst), that rtao's
    /// executor signs and submits through and the signing guard reads from.
    /// rtao allows a signature 30 s from nonce lease to pool admission; the
    /// 26 to 28 calls testnet measured in that window cannot queue behind a
    /// read budget of about 15 per minute. That window alone exceeds about
    /// 20 calls per minute, whatever the budgets.
    #[serde(default)]
    pub signing_requests_per_minute: Option<u32>,
    /// Seconds the read session makes no call before and after the signing
    /// transport connects. The connection spends about 16 calls of its burst
    /// at once; overlapping the read budget, a restart minute reached 28 to
    /// 30 calls on testnet. With a minute on each side, no minute holds both.
    #[serde(default = "default_signing_connect_quiet")]
    pub signing_connect_quiet_secs: u64,
    /// Optional runtime metadata cache; omitted disables the disk cache.
    #[serde(default)]
    pub runtime_cache_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryConfig {
    /// Bounded `recover_finalized` calls per startup before a retryable exit.
    #[serde(default = "default_recovery_rounds")]
    pub max_rounds: u32,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            max_rounds: default_recovery_rounds(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    #[default]
    Json,
    Text,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogConfig {
    #[serde(default)]
    pub format: LogFormat,
    #[serde(default)]
    pub level: LogLevel,
}

/// Public keys of the coordinator's application signing roles, as 32-byte hex.
/// They are distinct from every chain key.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedKeysConfig {
    pub package: String,
    pub receipt: String,
    pub authorization: String,
}

impl TrustedKeysConfig {
    pub fn keys(&self) -> Result<slop_ninja_protocol::TrustedKeys> {
        let parse = |value: &str, name: &str| -> Result<[u8; 32]> {
            let mut bytes = [0; 32];
            hex::decode_to_slice(value.trim_start_matches("0x"), &mut bytes)
                .with_context(|| format!("package.trusted_keys.{name} is not 32 bytes of hex"))?;
            Ok(bytes)
        };
        slop_ninja_protocol::TrustedKeys::new(
            parse(&self.package, "package")?,
            parse(&self.receipt, "receipt")?,
            parse(&self.authorization, "authorization")?,
        )
        .map_err(|error| anyhow::anyhow!("package.trusted_keys refused: {error}"))
    }
}

/// Testnet shortcut T12: a coordinator-signed fixture package read from a
/// local file, removed by M6 when packages come from the coordinator.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageConfig {
    /// Absolute path of the signed package bundle.
    pub path: PathBuf,
    pub trusted_keys: TrustedKeysConfig,
}

/// Live dispatch. Read only when `run --dispatch` is given; without that flag
/// the hotkey file is never opened.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchConfig {
    /// Absolute path of the hotkey keyfile that signs weight commits. Its
    /// `pub.txt` companion must name the same account as `wallet.hotkey`.
    pub hotkey_file: PathBuf,
    /// Finalized blocks to process before a dispatch run exits on its own
    /// (exit 0). Unset, the run is continuous: it ends only on a signal, a
    /// check, a deferral, or when a fetched successor target is installed.
    #[serde(default)]
    pub max_finalized_blocks: Option<u32>,
    /// Commit at this many blocks after each observed epoch boundary instead
    /// of rtao's per-hotkey jitter. Unset (the default) keeps the jitter. On
    /// a rate-limited public RPC endpoint, a late offset keeps rtao's reveal
    /// replay short; it must stay below the subnet's blocks per epoch
    /// (`Tempo + 1`, read live), or no commit ever falls due.
    #[serde(default)]
    pub epoch_offset_blocks: Option<u64>,
    /// `per_tempo` (default, plan decision 12) re-commits the verified target
    /// once per tempo until the run ends; `once` commits it once and exits
    /// after its reveal is read back.
    #[serde(default)]
    pub recommit: Recommit,
}

/// How often a dispatch run commits its verified target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recommit {
    /// Once per tempo, each with its own operation ID, until a successor exists.
    #[default]
    PerTempo,
    /// One commit; the run stops when its reveal is read back.
    Once,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidatorConfig {
    pub network: NetworkName,
    /// Required. The subnet is registered separately; no netuid is built in.
    pub netuid: u16,
    /// Weight version key the scoring implementation supports.
    pub weight_version: u64,
    /// Absolute path of the native journal created by `init`.
    pub journal_path: PathBuf,
    pub wallet: WalletConfig,
    pub rpc: RpcConfig,
    #[serde(default)]
    pub recovery: RecoveryConfig,
    #[serde(default)]
    pub log: LogConfig,
    /// Score package to verify before any row is enqueued. Without it, `run`
    /// stops after startup with nothing enqueued.
    #[serde(default)]
    pub package: Option<PackageConfig>,
    #[serde(default)]
    pub dispatch: Option<DispatchConfig>,
    /// The published-package fetcher. Without it, `[package] path` is a
    /// bundle the operator installs (testnet shortcut T12).
    #[serde(default)]
    pub fetch: Option<FetchConfig>,
}

const fn default_poll_secs() -> u64 {
    300
}
const fn default_stall_secs() -> u64 {
    60
}

/// Cache lifetime of the public host's index documents (`max-age=60`). A
/// check more often than this reads the same documents again.
pub const MIN_POLL_SECS: u64 = 60;

/// Fetches the coordinator's published records from the public status host
/// (layout `v1`, `docs/testnet/public-layout.md`), verifies them and installs
/// the bundle at `[package] path`. Requires `[package]`, whose trusted keys
/// verify everything fetched.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchConfig {
    /// Base URL of the public status host: `https://` (plain `http://` only
    /// to a loopback host), with no credentials, query or fragment. The
    /// layout's `v1/...` paths are resolved against it.
    pub base_url: String,
    /// Absolute directory of the resumable object cache; directories the
    /// fetcher creates are mode 700 and files mode 600. Defaults to
    /// `public-cache` beside the journal.
    #[serde(default)]
    pub cache_dir: Option<PathBuf>,
    /// Seconds between checks for a new target or authorization while a
    /// dispatch run is continuous. At least [`MIN_POLL_SECS`]. It also bounds
    /// each fetch session and each request in time: a session never outlives
    /// the interval to the next one.
    #[serde(default = "default_poll_secs")]
    pub poll_secs: u64,
    /// Seconds a request may go without receiving a byte before it is
    /// abandoned; the object resumes from its partial file at the next check.
    #[serde(default = "default_stall_secs")]
    pub stall_secs: u64,
}

impl FetchConfig {
    /// The base URL, normalized to end with `/`.
    pub fn base(&self) -> Result<url::Url> {
        let mut url = check_base_url(&self.base_url)?;
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }
        Ok(url)
    }

    pub fn cache_dir(&self, journal: &Path) -> Result<PathBuf> {
        match &self.cache_dir {
            Some(dir) => Ok(dir.clone()),
            None => Ok(journal
                .parent()
                .context("journal_path has no parent directory")?
                .join("public-cache")),
        }
    }
}

/// The public host is read anonymously: `https://`, or `http://` to a
/// loopback host only, with no credentials, query or fragment.
fn check_base_url(text: &str) -> Result<url::Url> {
    let url = url::Url::parse(text).context("fetch.base_url is not a URL")?;
    let loopback_http = url.scheme() == "http"
        && match url.host() {
            Some(url::Host::Domain(host)) => host == "localhost",
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
    ensure!(
        url.scheme() == "https" || loopback_http,
        "fetch.base_url must use https:// (http:// only to a loopback host)"
    );
    ensure!(url.host_str().is_some(), "fetch.base_url has no host");
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "fetch.base_url must not embed credentials"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "fetch.base_url must not carry a query or fragment"
    );
    Ok(url)
}

/// Fields safe for logs: no paths, addresses, endpoint paths or credentials.
#[derive(Debug, Serialize)]
pub struct ConfigSummary {
    pub network: NetworkName,
    pub netuid: u16,
    pub weight_version: u64,
    pub wallet: String,
    pub hotkey: String,
    pub endpoint_hosts: Vec<String>,
    pub user_agent: String,
    pub max_requests_per_minute: u32,
    pub package_configured: bool,
    pub dispatch_configured: bool,
    /// Host of the public status host, when the fetcher is configured.
    pub fetch_host: Option<String>,
    /// `continuous`, or the configured block bound of a dispatch run.
    pub dispatch_blocks: Option<String>,
}

impl ValidatorConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).context("cannot read the configuration file")?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text).context("invalid configuration")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(self.netuid != 0, "netuid 0 is the root network");
        ensure!(
            self.journal_path.is_absolute(),
            "journal_path must be absolute"
        );
        ensure!(
            self.wallet.path.is_absolute(),
            "wallet.path must be absolute"
        );
        check_component(&self.wallet.name, "wallet.name")?;
        check_component(&self.wallet.hotkey, "wallet.hotkey")?;
        ensure!(
            !self.rpc.endpoints.is_empty() && self.rpc.endpoints.len() <= 8,
            "rpc.endpoints needs between one and eight endpoints"
        );
        let mut seen = Vec::new();
        for endpoint in &self.rpc.endpoints {
            let url = check_endpoint(endpoint)?;
            ensure!(!seen.contains(&url), "rpc.endpoints repeats an endpoint");
            seen.push(url);
        }
        check_user_agent(&self.rpc.user_agent)?;
        ensure!(
            (1..=60).contains(&self.rpc.max_requests_per_minute),
            "rpc.max_requests_per_minute must be between 1 and 60"
        );
        if let Some(signing) = self.rpc.signing_requests_per_minute {
            ensure!(
                (1..=60).contains(&signing),
                "rpc.signing_requests_per_minute must be between 1 and 60"
            );
        }
        ensure!(
            self.rpc.signing_connect_quiet_secs <= 300,
            "rpc.signing_connect_quiet_secs must be at most 300"
        );
        ensure!(
            (5..=600).contains(&self.rpc.startup_timeout_secs),
            "rpc.startup_timeout_secs must be between 5 and 600"
        );
        if let Some(dir) = &self.rpc.runtime_cache_dir {
            ensure!(dir.is_absolute(), "rpc.runtime_cache_dir must be absolute");
        }
        ensure!(
            (1..=1024).contains(&self.recovery.max_rounds),
            "recovery.max_rounds must be between 1 and 1024"
        );
        if let Some(package) = &self.package {
            ensure!(package.path.is_absolute(), "package.path must be absolute");
            package.trusted_keys.keys()?;
        }
        if let Some(dispatch) = &self.dispatch {
            ensure!(
                dispatch.hotkey_file.is_absolute(),
                "dispatch.hotkey_file must be absolute"
            );
            if let Some(blocks) = dispatch.max_finalized_blocks {
                ensure!(
                    (1..=100_000).contains(&blocks),
                    "dispatch.max_finalized_blocks must be between 1 and 100000 (unset for a continuous run)"
                );
            }
            if let Some(offset) = dispatch.epoch_offset_blocks {
                ensure!(
                    offset <= 50_400,
                    "dispatch.epoch_offset_blocks must be at most 50400"
                );
            }
        }
        if let Some(fetch) = &self.fetch {
            ensure!(
                self.package.is_some(),
                "[fetch] requires [package]: its path receives the fetched bundle and its keys verify it"
            );
            fetch.base()?;
            if let Some(dir) = &fetch.cache_dir {
                ensure!(dir.is_absolute(), "fetch.cache_dir must be absolute");
            }
            ensure!(
                fetch.poll_secs >= MIN_POLL_SECS,
                "fetch.poll_secs must be at least {MIN_POLL_SECS}, the index documents' cache lifetime"
            );
            ensure!(fetch.stall_secs >= 1, "fetch.stall_secs must be at least 1");
        }
        Ok(())
    }

    pub fn summary(&self) -> ConfigSummary {
        ConfigSummary {
            network: self.network,
            netuid: self.netuid,
            weight_version: self.weight_version,
            wallet: self.wallet.name.clone(),
            hotkey: self.wallet.hotkey.clone(),
            endpoint_hosts: self
                .rpc
                .endpoints
                .iter()
                .map(|endpoint| rtao_rpc::policy::safe_endpoint(endpoint))
                .collect(),
            user_agent: self.rpc.user_agent.clone(),
            max_requests_per_minute: self.rpc.max_requests_per_minute,
            package_configured: self.package.is_some(),
            dispatch_configured: self.dispatch.is_some(),
            fetch_host: self
                .fetch
                .as_ref()
                .and_then(|fetch| fetch.base().ok())
                .and_then(|url| url.host_str().map(str::to_owned)),
            dispatch_blocks: self.dispatch.as_ref().map(|dispatch| {
                dispatch
                    .max_finalized_blocks
                    .map_or_else(|| "continuous".to_owned(), |blocks| blocks.to_string())
            }),
        }
    }
}

fn check_component(value: &str, name: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 64
            && value != "."
            && value != ".."
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)),
        "{name} must be a plain file name of at most 64 ASCII letters, digits, '-', '_' or '.'"
    );
    Ok(())
}

/// Whether `url` is plaintext `ws://` to a loopback host (a local proxy,
/// such as the drill proxy, that itself connects onward with TLS).
pub fn is_loopback_ws(url: &url::Url) -> bool {
    url.scheme() == "ws"
        && match url.host() {
            Some(url::Host::Domain(host)) => host == "localhost",
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        }
}

/// Endpoints are logged by host and must not carry credentials in the URL.
/// Plaintext `ws://` is accepted only for a loopback host.
fn check_endpoint(endpoint: &str) -> Result<url::Url> {
    let url = url::Url::parse(endpoint).context("rpc endpoint is not a URL")?;
    ensure!(
        url.scheme() == "wss" || is_loopback_ws(&url),
        "rpc endpoints must use wss:// (ws:// only to a loopback host)"
    );
    ensure!(url.host_str().is_some(), "rpc endpoint has no host");
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "rpc endpoints must not embed credentials"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "rpc endpoints must not carry a query or fragment"
    );
    ensure!(
        matches!(url.path(), "" | "/"),
        "rpc endpoints must not carry a path, which can hold a credential"
    );
    Ok(url)
}

fn check_user_agent(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty()
            && value.len() <= 128
            && value.bytes().all(|byte| (0x20..0x7f).contains(&byte)),
        "rpc.user_agent must be 1-128 printable ASCII characters"
    );
    http::HeaderValue::from_str(value).context("rpc.user_agent is not a valid header value")?;
    Ok(())
}
