//! Deployment configuration, parsed from Worker vars and secrets.
//!
//! Nothing chain- or subnet-specific is compiled in except the testnet guard
//! below. The netuid, subscription ID and signing secret come from deployment
//! configuration.

use crate::heartbeat::Settings;
use crate::rpc::{DEFAULT_TESTNET_RPC_URL, MAX_HASHES_PER_CALL};
use crate::signing::SignConfig;
use crate::webhook::{ConfigError, ReceiverConfig, SigningKey};
use crate::wire::Id;
use sha2::{Digest as _, Sha256};
use slop_ninja_protocol::Digest;
use subtle::ConstantTimeEq;

/// Bittensor testnet (`test.finney`) genesis hash in the configuration form.
/// It must equal the shared protocol crate's `TESTNET_GENESIS`; a unit test pins it.
pub const TESTNET_GENESIS: &str =
    "0x8f9cf856bf558a14440e75569c9e58594757048d7b3a84b5d25f6bd978263105";

/// TESTNET-ONLY RESTRICTION (`testnet_only_genesis`): the skeleton refuses any
/// configured chain other than testnet, because nothing here is qualified for
/// Finney. Removed when the coordinator passes Finney qualification after
/// milestone M7 of the testnet plan.
pub const TESTNET_ONLY_GENESIS: bool = true;

pub const DEFAULT_KEY_ID: &str = "pilot-v1";

/// The only network this build serves. Every Cloudflare resource, binding and
/// Durable Object instance name carries it, so testnet state cannot be
/// mistaken for a later mainnet deployment.
pub const NETWORK: &str = "testnet";

/// Shortest accepted bearer token secret (`COORDINATOR_STATUS_TOKEN`,
/// `COORDINATOR_OPERATOR_TOKEN`).
pub const MIN_TOKEN_BYTES: usize = 32;

/// Checks the `NETWORK` var. Testnet-only, like the genesis guard.
pub fn network(value: Option<&str>) -> Result<&'static str, ConfigError> {
    match value.map(str::trim) {
        Some(NETWORK) => Ok(NETWORK),
        _ => Err(ConfigError::Network),
    }
}

/// Which deployment of this crate a Worker is. The testnet evaluation Worker
/// (`EVAL_ROLE = "eval"`, shortcut T13) runs beside the coordinator with its
/// own ChainJournal namespace and cron watchdog, and binds the same
/// evaluation bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Coordinator,
    Eval,
}

/// Reads the `EVAL_ROLE` var: exactly `eval` is the evaluation Worker; any
/// other value, or none, is the coordinator.
pub fn role(value: Option<&str>) -> Role {
    if value == Some("eval") {
        Role::Eval
    } else {
        Role::Coordinator
    }
}

/// Binding name for `base` on `network`, for example `CHAIN_JOURNAL_TESTNET`.
pub fn binding(base: &str, network: &str) -> String {
    format!("{base}_{}", network.to_ascii_uppercase())
}

fn setting(
    get: &impl Fn(&str) -> Option<String>,
    name: &str,
    default: u64,
    max: u64,
) -> Result<u64, ConfigError> {
    match get(name).as_deref().map(str::trim) {
        None | Some("") => Ok(default),
        Some(v) => match v.parse::<u64>() {
            Ok(n) if n > 0 && n <= max => Ok(n),
            _ => Err(ConfigError::Setting),
        },
    }
}

/// Heartbeat and watchdog settings from Worker vars, with defaults.
pub fn settings(get: impl Fn(&str) -> Option<String>) -> Result<Settings, ConfigError> {
    let d = Settings::default();
    let silence_secs = setting(&get, "WATCHDOG_SILENCE_SECS", d.silence_ms / 1000, 86_400)?;
    let s = Settings {
        silence_ms: silence_secs * 1000,
        behind_blocks: setting(&get, "WATCHDOG_BEHIND_BLOCKS", d.behind_blocks, 100_000)?,
        // The public endpoint throttles near 25 calls a minute per source.
        rpc_calls_per_minute: setting(&get, "RPC_CALLS_PER_MINUTE", d.rpc_calls_per_minute, 20)?,
        hashes_per_call: setting(
            &get,
            "RPC_HASHES_PER_CALL",
            d.hashes_per_call,
            MAX_HASHES_PER_CALL,
        )?,
        ..d
    };
    if s.hashes_per_call < 2 || s.rpc_calls_per_minute < 2 {
        return Err(ConfigError::Setting);
    }
    Ok(s)
}

/// The RPC endpoint: HTTPS, no credentials or query.
pub fn rpc_url(value: Option<&str>) -> Result<String, ConfigError> {
    let url = match value.map(str::trim) {
        None | Some("") => DEFAULT_TESTNET_RPC_URL,
        Some(v) => v,
    };
    let rest = url.strip_prefix("https://").ok_or(ConfigError::RpcUrl)?;
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty()
        || url.contains(['@', '?', '#', ' '])
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return Err(ConfigError::RpcUrl);
    }
    Ok(url.to_owned())
}

/// Checks a bearer token secret's length.
pub fn token(secret: &str) -> Result<(), ConfigError> {
    if secret.len() < MIN_TOKEN_BYTES {
        return Err(ConfigError::Token);
    }
    Ok(())
}

/// Constant-time comparison of an `Authorization: Bearer` header with the
/// token secret. Both sides are hashed first so lengths do not leak.
pub fn bearer_matches(header: Option<&str>, secret: &str) -> bool {
    if token(secret).is_err() {
        return false;
    }
    let Some(presented) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
        return false;
    };
    let a = Sha256::digest(presented.as_bytes());
    let b = Sha256::digest(secret.as_bytes());
    bool::from(a.ct_eq(&b))
}

/// Public, content-free settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainConfig {
    pub genesis: Id,
    /// `None` until the operator sets the registered subnet's netuid.
    pub netuid: Option<u16>,
}

pub fn chain_config(genesis: &str, netuid: Option<&str>) -> Result<ChainConfig, ConfigError> {
    let genesis = Id::parse(genesis.trim()).map_err(|_| ConfigError::Chain)?;
    if genesis.is_zero() {
        return Err(ConfigError::Chain);
    }
    if TESTNET_ONLY_GENESIS && Digest::from(genesis) != slop_ninja_protocol::TESTNET_GENESIS {
        return Err(ConfigError::TestnetOnlyGenesis);
    }
    let netuid = match netuid.map(str::trim) {
        None | Some("") => None,
        Some(value) if value.starts_with("REPLACE_") => None,
        Some(value) => Some(value.parse::<u16>().map_err(|_| ConfigError::Netuid)?),
    };
    Ok(ChainConfig { genesis, netuid })
}

/// Builds the webhook receiver's expectations. All inputs are required.
pub fn receiver_config(
    chain: &ChainConfig,
    key_id: &str,
    signing_secret_hex: &str,
    subscription: &str,
) -> Result<ReceiverConfig, ConfigError> {
    let key = SigningKey::from_hex(key_id.trim(), signing_secret_hex)?;
    let subscription = Id::parse(subscription.trim()).map_err(|_| ConfigError::Subscription)?;
    if subscription.is_zero() {
        return Err(ConfigError::Subscription);
    }
    Ok(ReceiverConfig {
        key,
        subscription,
        chain: chain.genesis,
    })
}

/// The signing journal's configuration: the coordinator hotkey's public key
/// (`COORDINATOR_HOTKEY_SS58`; the secret is the separate Worker secret
/// `COORDINATOR_HOTKEY_SEED`), the T12 package trust roots and the commit
/// schedule settings. The netuid is required.
pub fn sign_config(
    get: impl Fn(&str) -> Option<String>,
    chain: &ChainConfig,
    settings: &Settings,
) -> Result<(SignConfig, slop_ninja_protocol::TrustedKeys), ConfigError> {
    let netuid = chain.netuid.ok_or(ConfigError::Netuid)?;
    let hotkey = get("COORDINATOR_HOTKEY_SS58")
        .and_then(|a| crate::chain::public_from_ss58(a.trim()).ok())
        .ok_or(ConfigError::Hotkey)?;
    let key = |name: &str| get(name).unwrap_or_default();
    let trusted = crate::target::trusted_keys(
        &key("PACKAGE_PUBLIC_KEY"),
        &key("RECEIPT_PUBLIC_KEY"),
        &key("AUTHORIZATION_PUBLIC_KEY"),
    )
    .map_err(|_| ConfigError::TrustedKeys)?;
    let mut cfg = SignConfig::testnet(chain.genesis, netuid, hotkey);
    let version_key = match get("WEIGHTS_VERSION_KEY").as_deref().map(str::trim) {
        None | Some("") => 0,
        Some(v) => v.parse::<u64>().map_err(|_| ConfigError::Setting)?,
    };
    cfg.version_key = version_key;
    cfg.era_period = setting(&get, "SIGN_ERA_PERIOD", cfg.era_period, 256)?;
    cfg.commit_offset = setting(&get, "SIGN_COMMIT_OFFSET", cfg.commit_offset, 10_000)?;
    cfg.lease_ms = settings.lease_ms;
    cfg.rpc_calls_per_minute = settings.rpc_calls_per_minute;
    // Signing's share leaves the heartbeat at least four calls a minute (two
    // head reads' worth) however busy signing is.
    let share = settings.rpc_calls_per_minute.saturating_sub(4).max(1);
    cfg.sign_rpc_calls_per_minute = setting(&get, "SIGN_RPC_CALLS_PER_MINUTE", share, share)?;
    if cfg.era_period < 16 || !cfg.era_period.is_power_of_two() {
        return Err(ConfigError::Setting);
    }
    Ok((cfg, trusted))
}

/// Durable Object instance name for a chain's observation journal.
pub fn chain_journal_name(chain: &ChainConfig) -> String {
    format!("chain-journal/{NETWORK}/{}", chain.genesis.to_hex())
}

/// Durable Object instance name for a subnet's cohort authority: the original
/// main authority's, and the base of the load instance's and every named
/// authority's ([`authority_cohort_name`]).
pub fn eval_cohort_name(chain: &ChainConfig) -> Option<String> {
    chain
        .netuid
        .map(|netuid| format!("eval-cohort/{NETWORK}/{}/{netuid}", chain.genesis.to_hex()))
}

/// The original main authority's name. Its Durable Object, lane names and
/// probe keys keep the form they had before authorities were named.
pub const ORIGINAL_AUTHORITY: &str = "main";

/// The cohort label rule's length bound: plan validation (`eval::plan`)
/// takes 1 to 96 bytes of `[A-Za-z0-9-]`, and so does the scorer's cohort id.
pub const LABEL_MAX_BYTES: usize = 96;

/// The longest authority name: its label prefix `<network>-<name>-` leaves
/// at least one byte of a label.
pub const AUTHORITY_NAME_MAX_BYTES: usize = LABEL_MAX_BYTES - NETWORK.len() - 3;

/// Whether `name` can name an authority other than the original: lowercase
/// ASCII letters and digits, starting with a letter, never `main` or `load`.
/// No hyphen, so no authority's label prefix is a prefix of another's.
pub fn authority_name_ok(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=AUTHORITY_NAME_MAX_BYTES).contains(&b.len())
        && b.first().is_some_and(u8::is_ascii_lowercase)
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name != ORIGINAL_AUTHORITY
        && name != "load"
}

/// Durable Object instance name of the main authority `name`: the original's
/// ([`ORIGINAL_AUTHORITY`]) is [`eval_cohort_name`]; a named authority's adds
/// `/authority/<name>`, so it never equals the load instance's (`/load`).
/// `None` for a name no authority can have, or before the netuid is set.
pub fn authority_cohort_name(chain: &ChainConfig, name: &str) -> Option<String> {
    let base = eval_cohort_name(chain)?;
    if name == ORIGINAL_AUTHORITY {
        Some(base)
    } else if authority_name_ok(name) {
        Some(format!("{base}/authority/{name}"))
    } else {
        None
    }
}

/// The main instance's cohort authority (`EVAL_MAIN_AUTHORITY`, with
/// `EVAL_MAIN_FIRST_COHORT` and `EVAL_MAIN_PREVIOUS`, all tracked in
/// `wrangler.toml`): a chain-record restart without deleting anything. The
/// original authority keeps every name and key it had. A named authority is a
/// fresh Durable Object, so it starts its own reference chain, published
/// index, fingerprints, probe versions and reuse ledger, while the earlier
/// authorities' state stays where it was, readable. Its lane objects and
/// probe objects sit under its own names, its labels start with
/// `<network>-<name>-` and its cohort numbers start at the configured first
/// cohort, so its batches, receipts, packages, closures and openings never
/// share a label or a cohort number with an earlier authority's.
///
/// Succession is durable: a named authority takes its first plan (or
/// template) only after every earlier authority (the original, then each one
/// `EVAL_MAIN_PREVIOUS` lists) recorded in its own store that it is
/// superseded, which each does only while it holds no cohort that is not
/// closed or abandoned and no label or number the successor's rule would
/// share. A superseded authority admits no plan again, whatever a later
/// deployment names, so a deploy that reverts the vars cannot re-admit
/// numbers or labels of the restarted chain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Authority {
    named: Option<NamedAuthority>,
    /// The deployment names another authority, or a successor superseded
    /// this one: it keeps its lane names, probe keys and kicks for work it
    /// still holds, and admits no plan ([`Authority::retired`]).
    retired: bool,
    /// The successor that superseded this authority, when one did.
    superseded_by: Option<String>,
    /// Earlier named authorities (`EVAL_MAIN_PREVIOUS`), oldest first.
    previous: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NamedAuthority {
    name: String,
    first_cohort: u64,
}

impl Authority {
    /// Reads `EVAL_MAIN_AUTHORITY` and `EVAL_MAIN_FIRST_COHORT`. Unset, empty
    /// or `main` is the original authority, which takes no first cohort. A
    /// named authority needs one (at least 1). Anything else is refused, so a
    /// misconfigured deployment reaches no authority at all.
    pub fn parse(name: Option<&str>, first_cohort: Option<&str>) -> Result<Self, ConfigError> {
        Self::parse_with(name, first_cohort, None)
    }

    /// [`Authority::parse`] with `EVAL_MAIN_PREVIOUS`: the earlier named
    /// authorities, comma separated, oldest first. Each is a valid name,
    /// listed once, never this authority's own; the original authority has
    /// none (it has no predecessor).
    pub fn parse_with(
        name: Option<&str>,
        first_cohort: Option<&str>,
        previous: Option<&str>,
    ) -> Result<Self, ConfigError> {
        let name = name.map(str::trim).filter(|n| !n.is_empty());
        let first = first_cohort.map(str::trim).filter(|v| !v.is_empty());
        let mut listed: Vec<String> = Vec::new();
        for p in previous
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            if !authority_name_ok(p) || listed.iter().any(|l| l == p) {
                return Err(ConfigError::Authority);
            }
            listed.push(p.to_owned());
        }
        match name {
            None | Some(ORIGINAL_AUTHORITY) => match (first, listed.is_empty()) {
                (None, true) => Ok(Self::default()),
                _ => Err(ConfigError::Authority),
            },
            Some(n) if authority_name_ok(n) && !listed.iter().any(|l| l == n) => {
                let first_cohort = first
                    .and_then(|v| v.parse::<u64>().ok())
                    .filter(|c| *c >= 1)
                    .ok_or(ConfigError::Authority)?;
                Ok(Self {
                    named: Some(NamedAuthority {
                        name: n.to_owned(),
                        first_cohort,
                    }),
                    retired: false,
                    superseded_by: None,
                    previous: listed,
                })
            }
            Some(_) => Err(ConfigError::Authority),
        }
    }

    /// A named authority (for tests and tools; the Worker reads [`Authority::parse_with`]).
    pub fn named(name: &str, first_cohort: u64) -> Result<Self, ConfigError> {
        Self::parse(Some(name), Some(&first_cohort.to_string()))
    }

    /// A named authority after earlier named ones (`previous`, oldest first).
    pub fn named_after(
        name: &str,
        first_cohort: u64,
        previous: &[&str],
    ) -> Result<Self, ConfigError> {
        Self::parse_with(
            Some(name),
            Some(&first_cohort.to_string()),
            Some(&previous.join(",")),
        )
    }

    /// Authority `name` when the deployment names another: a cohort object
    /// keeps its own names for the work it holds and is never queued again.
    pub fn retired(name: &str) -> Self {
        let named = authority_name_ok(name).then(|| NamedAuthority {
            name: name.to_owned(),
            first_cohort: u64::MAX,
        });
        Self {
            named,
            retired: true,
            superseded_by: None,
            previous: Vec::new(),
        }
    }

    pub fn is_original(&self) -> bool {
        self.named.is_none()
    }

    pub fn is_retired(&self) -> bool {
        self.retired
    }

    /// The successor that superseded this authority ([`own_authority`]).
    pub fn superseded_by(&self) -> Option<&str> {
        self.superseded_by.as_deref()
    }

    pub fn name(&self) -> &str {
        self.named
            .as_ref()
            .map_or(ORIGINAL_AUTHORITY, |n| n.name.as_str())
    }

    /// The first cohort number a named authority takes (0: the original
    /// authority's numbers are bounded only by its own history).
    pub fn first_cohort(&self) -> u64 {
        self.named.as_ref().map_or(0, |n| n.first_cohort)
    }

    /// Every label of a named authority starts with this.
    pub fn label_prefix(&self) -> Option<String> {
        self.named
            .as_ref()
            .map(|n| format!("{NETWORK}-{}-", n.name))
    }

    /// Every authority before this one, oldest first: the original and each
    /// earlier named one (`EVAL_MAIN_PREVIOUS`) for a named authority, none
    /// for the original.
    pub fn predecessors(&self) -> Vec<&str> {
        if self.named.is_none() {
            return Vec::new();
        }
        std::iter::once(ORIGINAL_AUTHORITY)
            .chain(self.previous.iter().map(String::as_str))
            .collect()
    }

    /// The authorities the deployment knows: this one and its predecessors.
    /// Only these are read on the audit route and kicked by the Worker, so no
    /// request creates an object for any other name.
    pub fn known(&self, name: &str) -> bool {
        name == self.name() || self.predecessors().contains(&name)
    }

    /// The known authority ([`Authority::known`]) whose cohort `label` is:
    /// the named one whose prefix ([`Authority::label_prefix`]) the label
    /// carries, else the original authority when the deployment knows it.
    /// Names hold no `-`, so at most one named prefix matches, and a
    /// succession refuses a successor whose prefix an earlier label carries
    /// ([`Authority::follows`]). Whether that authority holds the label is
    /// its own store's answer. A report about a cohort (the publisher's
    /// publication report) so reaches the authority that ran it, also after
    /// a successor took over.
    pub fn label_authority(&self, label: &str) -> Option<&str> {
        std::iter::once(self.name())
            .chain(self.predecessors())
            .find(|n| {
                *n != ORIGINAL_AUTHORITY
                    && label
                        .strip_prefix(NETWORK)
                        .and_then(|l| l.strip_prefix('-'))
                        .and_then(|l| l.strip_prefix(*n))
                        .is_some_and(|l| l.starts_with('-'))
            })
            .or_else(|| self.known(ORIGINAL_AUTHORITY).then_some(ORIGINAL_AUTHORITY))
    }

    /// The main authority's Durable Object name ([`authority_cohort_name`]).
    pub fn cohort_name(&self, chain: &ChainConfig) -> Option<String> {
        authority_cohort_name(chain, self.name())
    }

    /// The scope lane object names carry for `instance` (`eval::lanes::lane_name`):
    /// the instance alone for the original authority, so its lane objects are
    /// the ones they were; `<instance>/authority/<name>` for a named one,
    /// whose lanes order their own assignments from its first sequence number.
    pub fn lane_scope(&self, instance: &str) -> String {
        match &self.named {
            None => instance.to_owned(),
            Some(n) => format!("{instance}/authority/{}", n.name),
        }
    }

    /// Where probe version `version`'s objects live in both buckets: the
    /// original authority's `probes/v<version>`, a named authority's
    /// `probes/authority/<name>/v<version>`.
    pub fn probe_prefix(&self, version: u32) -> String {
        match &self.named {
            None => format!("probes/v{version}"),
            Some(n) => format!("probes/authority/{}/v{version}", n.name),
        }
    }

    /// Whether this authority takes a plan's label and cohort number.
    pub fn admits(&self, label: &str, cohort: u64) -> Result<(), &'static str> {
        if self.superseded_by.is_some() {
            return Err("authority superseded (a later main authority restarted the chain record)");
        }
        if self.retired {
            return Err("authority retired (the deployment names another, EVAL_MAIN_AUTHORITY)");
        }
        let Some(n) = &self.named else { return Ok(()) };
        let prefix = format!("{NETWORK}-{}-", n.name);
        if !label.starts_with(&prefix) || label.len() == prefix.len() {
            return Err("label (a named authority's labels start with <network>-<authority>-)");
        }
        if cohort < n.first_cohort {
            return Err(
                "cohort number (below the authority's first cohort, EVAL_MAIN_FIRST_COHORT)",
            );
        }
        Ok(())
    }

    /// Checks a named authority against the cohorts `(label, cohort number)`
    /// an earlier authority holds: none of its labels may start with this
    /// authority's prefix, and every number must be below its first cohort.
    pub fn follows<'a>(
        &self,
        earlier: impl IntoIterator<Item = (&'a str, u64)>,
    ) -> Result<(), &'static str> {
        let Some(n) = &self.named else { return Ok(()) };
        let prefix = format!("{NETWORK}-{}-", n.name);
        for (label, cohort) in earlier {
            if label.starts_with(&prefix) {
                return Err("an earlier authority's label has this authority's prefix");
            }
            if cohort >= n.first_cohort {
                return Err(
                    "an earlier authority's cohort number is not below EVAL_MAIN_FIRST_COHORT",
                );
            }
        }
        Ok(())
    }
}

/// `(name, action)` of `/eval/authority/<name>/<action>`: a name a main
/// authority can have (the original's or a valid one) and one of `status`,
/// `clear` or `abandon`. Whether the deployment knows the name is the
/// caller's check ([`Authority::known`]).
pub fn authority_route(path: &str) -> Option<(&str, &str)> {
    let (name, action) = path.strip_prefix("/eval/authority/")?.split_once('/')?;
    ((name == ORIGINAL_AUTHORITY || authority_name_ok(name))
        && matches!(action, "status" | "clear" | "abandon"))
    .then_some((name, action))
}

/// What a successor asks an earlier authority to record (`/succession/supersede`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Supersede {
    /// The successor's name.
    pub by: String,
    /// Its first cohort number.
    pub first_cohort: u64,
}

impl Supersede {
    /// The request a named authority sends each predecessor.
    pub fn of(successor: &Authority) -> Option<Self> {
        let n = successor.named.as_ref()?;
        Some(Self {
            by: n.name.clone(),
            first_cohort: n.first_cohort,
        })
    }

    /// Whether the earlier authority holding `cohorts` (label, number, phase)
    /// may record this successor: every cohort closed or abandoned (none
    /// queued, admitting, working or in transition), and none of its labels
    /// or numbers within the successor's ([`Authority::follows`]).
    pub fn check<'a>(
        &self,
        cohorts: impl IntoIterator<Item = (&'a str, u64, &'a str)> + Clone,
    ) -> Result<(), &'static str> {
        let successor =
            Authority::named(&self.by, self.first_cohort).map_err(|_| "successor_name")?;
        if cohorts
            .clone()
            .into_iter()
            .any(|(_, _, phase)| !matches!(phase, "closed" | "abandoned"))
        {
            return Err("predecessor_busy");
        }
        successor
            .follows(cohorts.into_iter().map(|(l, n, _)| (l, n)))
            .map_err(|_| "predecessor_numbers")
    }
}

/// The authority a main authority's object runs under: the deployment's when
/// it names this object's authority and no successor superseded it, else this
/// authority retired. `deployed` is `None` when the deployment's authority vars
/// are malformed.
pub fn own_authority(
    deployed: Option<&Authority>,
    own: &str,
    superseded_by: Option<&str>,
) -> Authority {
    let mut a = match deployed {
        Some(d) if d.name() == own && superseded_by.is_none() => d.clone(),
        _ => Authority::retired(own),
    };
    if let Some(by) = superseded_by {
        a.retired = true;
        a.superseded_by = Some(by.to_owned());
    }
    a
}

/// A cohort object's identity for one request: the authority (or instance)
/// it already stores, or the one the request names when it stores none and
/// the request may create it. Returns the identity and whether it must be
/// written. A read-only request (the audit route, a successor's supersede
/// call) to an object that stores nothing writes nothing (`unknown_authority`).
pub fn object_identity(
    stored: Option<&str>,
    asked: &str,
    may_create: bool,
) -> Result<(String, bool), &'static str> {
    match stored {
        Some(s) if s != asked => Err("authority_mismatch"),
        Some(s) => Ok((s.to_owned(), false)),
        None if may_create => Ok((asked.to_owned(), true)),
        None => Err("unknown_authority"),
    }
}

/// The authority a lane reports to: the one its kick named, the original when
/// the kick names none (the original authority, the load instance, or a kick
/// written before authorities were named).
pub fn callback_authority(kick: Option<&str>) -> &str {
    kick.unwrap_or(ORIGINAL_AUTHORITY)
}

/// The main authorities a heartbeat kicks: the deployment's own on every
/// delivery and tick, and on ticks its predecessors until its succession is
/// complete (`succession_complete`, which the deployed authority's kick reply
/// reports). Before that an earlier authority may still hold work (it records
/// succession only once idle), and these kicks advance its boundary phases
/// until it finishes; afterwards every predecessor is superseded, holds
/// nothing and takes nothing, so it is never kicked again.
pub fn kicked_authorities(
    deployed: &Authority,
    tick: bool,
    succession_complete: bool,
) -> Vec<String> {
    let mut v = vec![deployed.name().to_owned()];
    if tick && !succession_complete {
        v.extend(deployed.predecessors().into_iter().map(str::to_owned));
    }
    v
}

/// Whether a successor's store records `predecessor`'s succession for the
/// first cohort `first_cohort`: a record made for a first cohort at or below
/// it covers it (the predecessor's numbers were all below that one), and a
/// lower first cohort asks the predecessor again. `done` holds `(predecessor,
/// first cohort)` pairs as the successor stored them.
pub fn succession_recorded(done: &[(String, u64)], predecessor: &str, first_cohort: u64) -> bool {
    done.iter()
        .any(|(p, f)| p == predecessor && *f <= first_cohort)
}

/// Whether every predecessor of `authority` recorded it as successor for the
/// first cohort it has now ([`succession_recorded`]); the original authority,
/// which has none, is always complete.
pub fn succession_complete(authority: &Authority, done: &[(String, u64)]) -> bool {
    authority
        .predecessors()
        .iter()
        .all(|p| succession_recorded(done, p, authority.first_cohort()))
}

/// What a predecessor's `/succession/supersede` reply means for `successor`,
/// which asks its predecessors in order ([`Authority::predecessors`]). The
/// predecessor `predecessor` answers with the successor it recorded
/// (`superseded_by`), the first one ever to ask; the step is done only when
/// that is `successor` itself or an authority after `predecessor` in
/// `successor`'s own list, which is asked in turn (so the last one must have
/// recorded `successor`). Any other successor is an authority the deployment
/// leaves out of `EVAL_MAIN_PREVIOUS`, which may hold cohorts, labels and
/// numbers this successor never checked: `predecessor_superseded_elsewhere`.
/// So along the chain of recorded successors from the original, every
/// authority that ever admitted a plan is one the successor asked.
pub fn succession_reply(
    successor: &Authority,
    predecessor: &str,
    superseded_by: Option<&str>,
) -> Result<(), &'static str> {
    let listed = successor.predecessors();
    let Some(i) = listed.iter().position(|p| *p == predecessor) else {
        return Err("predecessor_unknown");
    };
    let later = listed.get(i + 1..).unwrap_or_default();
    match superseded_by {
        Some(by) if by == successor.name() || later.contains(&by) => Ok(()),
        Some(_) => Err("predecessor_superseded_elsewhere"),
        None => Err("predecessor_unreadable"),
    }
}

/// A main authority's own succession record, read only (`GET
/// /succession/record`): the predecessors' successions its store recorded
/// (`succession_recorded`, `(name, first cohort)` pairs), whether it
/// holds any cohort, and the retention notices it delivered
/// (`retention_notices`, `(predecessor, after cohort)` pairs, each recorded
/// only once that predecessor reported nothing left to delete:
/// `eval::retention::record_notice`). A record from a build before the
/// notices were reported reads as none delivered.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SuccessionRecord {
    pub succession_recorded: Vec<(String, u64)>,
    pub holds_cohorts: bool,
    #[serde(default)]
    pub retention_notices: Vec<(String, u64)>,
}

/// Whether `successor` may skip its own retention notice to `predecessor`
/// because `through`'s record shows the notice already delivered, with the
/// cohort after which `through` told it. That needs all of:
/// - `predecessor` is covered through `through`'s record
///   ([`succession_through`], checked again on the record read now);
/// - `through` recorded `predecessor`'s succession for a first cohort `f`
///   (so every cohort `predecessor` holds is below `f`: its highest is at
///   most `f - 1`), and
/// - `through` delivered its own notice to `predecessor` after a cohort at
///   least `f + 1`, so past `predecessor`'s highest cohort + 2, and
///   recorded it, which it does only once `predecessor` reported nothing
///   left to delete.
///
/// A superseded authority admits nothing again, so nothing it holds can
/// need a later notice. Otherwise (`None`) this successor tells
/// `predecessor` itself.
pub fn retention_covered(successor: &Authority, predecessor: &str, through: &str, record: &SuccessionRecord) -> Option<u64> {
    if !succession_through(successor, predecessor, through, record) {
        return None;
    }
    let first = record
        .succession_recorded
        .iter()
        .filter(|(p, _)| p == predecessor)
        .map(|(_, f)| *f)
        .max()?;
    record
        .retention_notices
        .iter()
        .filter(|(p, after)| p == predecessor && *after > first)
        .map(|(_, after)| *after)
        .max()
}

/// Whether `successor` counts `predecessor` as superseded through the record
/// of `through`, an authority it lists after `predecessor`, without opening
/// `predecessor`'s store (one that no longer opens, for example).
///
/// `through` recorded `predecessor` only after `predecessor` itself recorded,
/// in one transaction, a successor that was `through` or an authority
/// `through` listed after it ([`succession_reply`]), while it held no cohort
/// that was not closed or abandoned, no cohort number at or above `through`'s
/// first cohort and no blocking work. A superseded authority admits nothing
/// again, so all of that still holds. It covers this successor when:
/// - `through` recorded it for a first cohort at or below this successor's
///   ([`succession_recorded`]), so every number of `predecessor` is below
///   this successor's first cohort;
/// - `through` holds a cohort, so it took a plan, which it does only once its
///   succession is complete: its record names every authority it listed, and
///   so `predecessor`'s recorded successor too;
/// - every authority in `through`'s record is one this successor lists before
///   `through`, so that successor is one it handles itself.
///
/// `through` is then asked in turn (or covered by a later one), and the last
/// listed predecessor is always asked, so the chain of recorded successors
/// still ends at this successor. Not covered is the check of `predecessor`'s
/// labels against this successor's prefix: `through` checked them against
/// its own. A named predecessor's labels carry its own prefix, which no
/// other name's is; the original authority's are checked by the operator
/// before a deployment relies on this.
pub fn succession_through(
    successor: &Authority,
    predecessor: &str,
    through: &str,
    record: &SuccessionRecord,
) -> bool {
    let listed = successor.predecessors();
    let (Some(i), Some(j)) = (
        listed.iter().position(|p| *p == predecessor),
        listed.iter().position(|p| *p == through),
    ) else {
        return false;
    };
    let before = listed.get(..j).unwrap_or_default();
    i < j
        && record.holds_cohorts
        && succession_recorded(
            &record.succession_recorded,
            predecessor,
            successor.first_cohort(),
        )
        && record
            .succession_recorded
            .iter()
            .all(|(n, _)| before.contains(&n.as_str()))
}

/// A predecessor's answer to `/succession/supersede` ([`succeed_with`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SupersedeAnswer {
    /// 200, with the successor it recorded (`superseded_by`).
    Recorded(Option<String>),
    /// 404: no object stores that authority.
    Unknown,
    /// 409, with its refusal code.
    Refused(Option<String>),
    /// Any other reply: a store that does not open, for one.
    Unreadable,
    /// The call itself failed.
    Unreachable,
}

/// What [`succeed_with`] reads, asks and stores: a predecessor's own record
/// (read only, `None` when it does not answer), a predecessor's
/// `/succession/supersede`, and the successor's store of
/// `succession_recorded` and `succession_through`.
#[allow(async_fn_in_trait)]
pub trait SuccessionPeers {
    async fn record(&mut self, authority: &str) -> Option<SuccessionRecord>;
    async fn supersede(&mut self, predecessor: &str) -> SupersedeAnswer;
    async fn store(
        &mut self,
        done: &[(String, u64)],
        through: &[(String, String)],
    ) -> Result<(), &'static str>;
}

/// A named authority's succession before its plan or template (the cohort
/// object's `succeed`): each predecessor, in order, not yet recorded for its
/// first cohort ([`succession_recorded`]) is either covered through a later
/// predecessor's own record ([`succession_through`]; it is not asked, and
/// `through` keeps `(predecessor, through)`) or asked to record this
/// successor (`/succession/supersede`, [`succession_reply`]). `done` gets
/// `(predecessor, first cohort)` either way and is stored after each step.
/// A predecessor nothing covers that does not answer refuses, before any
/// later one is asked. The original authority and a retired one need
/// nothing.
pub async fn succeed_with(
    successor: &Authority,
    done: &mut Vec<(String, u64)>,
    through: &mut Vec<(String, String)>,
    peers: &mut impl SuccessionPeers,
) -> Result<(), &'static str> {
    if Supersede::of(successor).is_none() || successor.is_retired() {
        return Ok(());
    }
    let first = successor.first_cohort();
    let listed = successor.predecessors();
    let mut records: Vec<(&str, Option<SuccessionRecord>)> = Vec::new();
    for (i, p) in listed.iter().enumerate() {
        if succession_recorded(done, p, first) {
            continue;
        }
        let mut via = None;
        for q in listed.get(i + 1..).unwrap_or_default() {
            let record = match records.iter().find(|(n, _)| n == q) {
                Some((_, r)) => r.clone(),
                None => {
                    let r = peers.record(q).await;
                    records.push((q, r.clone()));
                    r
                }
            };
            if record
                .as_ref()
                .is_some_and(|r| succession_through(successor, p, q, r))
            {
                via = Some(*q);
                break;
            }
        }
        through.retain(|(d, _)| d != p);
        match via {
            Some(q) => through.push(((*p).to_owned(), q.to_owned())),
            None => match peers.supersede(p).await {
                SupersedeAnswer::Recorded(by) => succession_reply(successor, p, by.as_deref())?,
                SupersedeAnswer::Unknown => return Err("predecessor_unknown"),
                SupersedeAnswer::Refused(code) => {
                    return Err(match code.as_deref() {
                        Some("predecessor_busy") => "predecessor_busy",
                        Some("predecessor_numbers") => "predecessor_numbers",
                        Some("predecessor_work") => "predecessor_work",
                        _ => "predecessor_refused",
                    });
                }
                SupersedeAnswer::Unreadable => return Err("predecessor_unreadable"),
                SupersedeAnswer::Unreachable => return Err("predecessor_unreachable"),
            },
        }
        done.retain(|(d, _)| d != p);
        done.push(((*p).to_owned(), first));
        peers.store(done, through).await?;
    }
    Ok(())
}

/// A deployed main authority as an eval status reports it (`GET
/// /eval/status`, or the Worker's `/status` that carries it as `eval_cohort`;
/// an earlier authority's at `GET /eval/authority/<name>/status`), for the
/// pre-deploy check ([`deploy_check`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeployedAuthority {
    pub name: String,
    pub first_cohort: u64,
    pub predecessors: Vec<String>,
    /// `(label, cohort number, phase)` of every cohort it holds.
    pub cohorts: Vec<(String, u64, String)>,
    /// Whether its calendar can open a cohort (a standing template installed
    /// and not paused); `None` when the status reports no calendar.
    pub calendar_open: Option<bool>,
    /// The successor its store recorded (`superseded_by`): it admits nothing
    /// again, whatever a deployment names.
    pub superseded_by: Option<String>,
    /// The predecessors whose succession its store recorded
    /// (`succession_recorded`): each of them is superseded.
    pub succession_recorded: Vec<String>,
    /// Whether it still has work (`work`, `engine::blocking_work`): a
    /// closure, a spend settlement, a purge, an admission clean-up or
    /// admitted objects awaiting the publisher's report. A due
    /// receipt deletion is not included (`retention_work`): it finishes under
    /// the successor's notices.
    pub work: bool,
}

/// The eval status in `status`: itself, or the Worker `/status`'s
/// `eval_cohort` object. An error reply, `eval_cohort: null` or anything else
/// without a `schema_version` is no status.
fn eval_status(status: &serde_json::Value) -> Result<&serde_json::Value, String> {
    let st = match status.get("eval_cohort") {
        None => status,
        Some(v) if v.is_object() => v,
        Some(_) => {
            return Err(
                "the Worker's status carries no eval status (eval_cohort is not an object)".into(),
            );
        }
    };
    if !st
        .get("schema_version")
        .is_some_and(serde_json::Value::is_u64)
    {
        return Err("not an eval status (no schema_version): an error reply or no status".into());
    }
    Ok(st)
}

/// `(label, number, phase)` of every cohort an eval status lists.
fn status_cohorts(st: &serde_json::Value) -> Result<Vec<(String, u64, String)>, String> {
    let cohorts = st
        .get("cohorts")
        .and_then(serde_json::Value::as_array)
        .ok_or("the status has no cohorts array")?;
    let field = |c: &serde_json::Value, k: &str| c.get(k).cloned().unwrap_or_default();
    cohorts
        .iter()
        .map(|c| {
            let label = field(c, "label").as_str().map(str::to_owned);
            let number = field(c, "cohort").as_u64();
            let phase = field(c, "phase").as_str().map(str::to_owned);
            match (label, number, phase) {
                (Some(l), Some(n), Some(p)) => Ok((l, n, p)),
                _ => Err("a cohort without a label, number or phase".to_owned()),
            }
        })
        .collect()
}

impl DeployedAuthority {
    /// Reads every field or refuses: a status without `authority_name`,
    /// `first_cohort`, `predecessors`, `cohorts`, `superseded_by` (a name or
    /// null), `succession_recorded` or `work` (an error reply, no eval
    /// status, a build before named authorities or before they reported
    /// their succession) is never taken for any authority, the original
    /// included.
    pub fn from_status(status: &serde_json::Value) -> Result<Self, String> {
        let st = eval_status(status)?;
        let name = st
            .get("authority_name")
            .and_then(serde_json::Value::as_str)
            .ok_or("the status reports no authority_name")?;
        let first_cohort = st
            .get("first_cohort")
            .and_then(serde_json::Value::as_u64)
            .ok_or("the status reports no first_cohort")?;
        let predecessors = st
            .get("predecessors")
            .and_then(serde_json::Value::as_array)
            .ok_or("the status reports no predecessors array")?
            .iter()
            .map(|p| {
                p.as_str()
                    .map(str::to_owned)
                    .ok_or("a predecessor that is not a name")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let calendar_open =
            st.get("calendar")
                .filter(|c| c.is_object())
                .map(|c| match c.get("template") {
                    Some(t) if t.is_object() => {
                        t.get("paused") != Some(&serde_json::Value::Bool(true))
                    }
                    _ => false,
                });
        let superseded_by = match st.get("superseded_by") {
            Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            _ => return Err("the status reports no superseded_by (a name or null)".into()),
        };
        let succession_recorded = st
            .get("succession_recorded")
            .and_then(serde_json::Value::as_array)
            .ok_or("the status reports no succession_recorded array")?
            .iter()
            .map(|r| {
                r.get("predecessor")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .ok_or("a recorded succession without a predecessor")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let work = st
            .get("work")
            .and_then(serde_json::Value::as_bool)
            .ok_or("the status reports no work flag")?;
        Ok(Self {
            name: name.to_owned(),
            first_cohort,
            predecessors,
            cohorts: status_cohorts(st)?,
            calendar_open,
            superseded_by,
            succession_recorded,
            work,
        })
    }
}

/// What an eval deploy is ([`deploy_check`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeployMode {
    /// Every eval deploy: the tree tracks the deployed authority, which no
    /// successor superseded.
    Same,
    /// A redeploy of the deployed authority after a successor superseded it
    /// (a rollback with `freeze` left it so): it stays frozen, admitting
    /// nothing, until a tree names that successor.
    Frozen,
    /// The one deploy of authority support over a build that reports no
    /// authority (it runs the original): the tree tracks the original.
    FirstDeploy,
    /// The switch commit (the tree names the deployed authority's successor)
    /// or a rollback commit. `freeze` lets a rollback retire an authority that
    /// already holds cohorts (its succession happened, so it only freezes).
    Switch { freeze: bool },
}

/// The pre-deploy check of an eval deploy (`slop_ninja_eval_ops
/// authority-check`): `Ok` with the line to print, or why a tree tracking
/// `tracked` must not be deployed over the authority `status` reports. It
/// fails closed: a status it cannot read refuses. Succession is read from
/// what the stores recorded (`superseded_by`, `succession_recorded`), never
/// inferred from the cohorts listed, since it is recorded before the first
/// plan queues or the first templated cohort opens.
///
/// - `Same`: the deployed name, first cohort and predecessors equal the
///   tree's, and no successor superseded it (it would admit nothing).
/// - `Frozen`: the same, over a deployed authority a successor superseded:
///   it stays frozen.
/// - `FirstDeploy`: the status is an eval status without `authority_name`
///   (a build before named authorities) and the tree tracks the original.
/// - `Switch` forward: the tree's predecessors are the deployed one's and then
///   the deployed one; the deployed authority holds no cohort that is not
///   closed or abandoned and has no work left, its calendar is reported and
///   cannot open one, none of its labels or numbers falls within the tree's
///   authority ([`Authority::follows`]), and if a successor already
///   superseded it, that successor is the tree's.
/// - `Switch` back (a rollback): the tree's authority is one of the deployed
///   one's predecessors, with the predecessors it had, and `target` is its
///   own status (`GET /eval/authority/<name>/status`). Refused once
///   succession happened (the tree's authority recorded a successor, or the
///   deployed one recorded a succession or lists a cohort), since the
///   rollback then only freezes it, unless `freeze`; without `freeze` the
///   deployed authority's calendar must also be unable to open a cohort, so
///   no succession starts between the check and the deploy.
pub fn deploy_check(
    tracked: &Authority,
    status: &serde_json::Value,
    target: Option<&serde_json::Value>,
    mode: DeployMode,
) -> Result<String, String> {
    let tracked_preds: Vec<String> = tracked
        .predecessors()
        .into_iter()
        .map(str::to_owned)
        .collect();
    if mode == DeployMode::FirstDeploy {
        let st = eval_status(status)?;
        if st.get("authority_name").is_some() {
            return Err(
                "the deployed build reports its authority: deploy without --first-deploy".into(),
            );
        }
        status_cohorts(st)?;
        if !tracked.is_original() {
            return Err(format!(
                "the first deploy of authority support tracks the original authority, not {}",
                tracked.name()
            ));
        }
        return Ok(
            "first deploy of authority support: the deployed build reports no authority \
                   (it runs the original); the tree tracks main"
                .into(),
        );
    }
    let d = DeployedAuthority::from_status(status)?;
    let same = d.name == tracked.name()
        && d.first_cohort == tracked.first_cohort()
        && d.predecessors == tracked_preds;
    let DeployMode::Switch { freeze } = mode else {
        if !same {
            return Err(format!(
                "deployed authority {} (first cohort {}, predecessors {:?}) differs from the tracked {} \
                 (first cohort {}, predecessors {:?})",
                d.name,
                d.first_cohort,
                d.predecessors,
                tracked.name(),
                tracked.first_cohort(),
                tracked_preds
            ));
        }
        return match (&d.superseded_by, mode == DeployMode::Frozen) {
            (None, false) => Ok(format!("same authority: {}", d.name)),
            (Some(by), true) => Ok(format!(
                "same authority: {} (frozen: superseded by {by}, it admits nothing)",
                d.name
            )),
            (Some(by), false) => Err(format!(
                "the deployed authority {} is superseded by {by} and admits nothing: deploy the tree \
                 that names {by} (--switch), or pass --freeze to redeploy it frozen",
                d.name
            )),
            (None, true) => Err(format!(
                "--freeze without --switch: the deployed authority {} is not superseded",
                d.name
            )),
        };
    };
    if same {
        return Err("--switch: the tree tracks the deployed authority".into());
    }
    let open: Vec<String> = d
        .cohorts
        .iter()
        .filter(|(_, _, p)| !matches!(p.as_str(), "closed" | "abandoned"))
        .map(|(l, _, p)| format!("{l} is {p}"))
        .collect();
    let mut chain = d.predecessors.clone();
    chain.push(d.name.clone());
    if tracked_preds == chain {
        if let Some(by) = d
            .superseded_by
            .as_deref()
            .filter(|by| *by != tracked.name())
        {
            return Err(format!(
                "the deployed authority {} is superseded by {by}: {} would wait on \
                 predecessor_superseded_elsewhere for good (deploy {by}, or a tree listing it)",
                d.name,
                tracked.name()
            ));
        }
        if !open.is_empty() {
            return Err(format!(
                "the deployed authority {} is not idle: {}",
                d.name,
                open.join("; ")
            ));
        }
        if d.work {
            return Err(format!(
                "the deployed authority {} still has work (a closure, a spend settlement, a purge, an \
                 admission clean-up or admitted objects awaiting the publisher's report; due receipt \
                 deletions never count): wait until its status reports work \
                 false",
                d.name
            ));
        }
        match d.calendar_open {
            Some(false) => {}
            Some(true) => {
                return Err(format!(
                    "the deployed authority {}'s standing template is not paused (POST /eval/template/pause)",
                    d.name
                ));
            }
            None => return Err("the status reports no calendar".into()),
        }
        tracked
            .follows(d.cohorts.iter().map(|(l, n, _)| (l.as_str(), *n)))
            .map_err(|e| format!("{e} ({})", tracked.first_cohort()))?;
        return Ok(format!(
            "switch: {} -> {} (first cohort {})",
            d.name,
            tracked.name(),
            tracked.first_cohort()
        ));
    }
    let back = d
        .predecessors
        .iter()
        .position(|p| p == tracked.name())
        .and_then(|i| d.predecessors.get(..i))
        .is_some_and(|before| before == tracked_preds.as_slice());
    if back {
        let t = DeployedAuthority::from_status(target.ok_or_else(|| {
            format!(
                "a rollback needs the status of {} itself (--target-status: GET /eval/authority/{}/status)",
                tracked.name(),
                tracked.name()
            )
        })?)
        .map_err(|e| format!("--target-status: {e}"))?;
        if t.name != tracked.name() {
            return Err(format!(
                "--target-status reports {}, not the tree's {}",
                t.name,
                tracked.name()
            ));
        }
        let mut happened = Vec::new();
        if let Some(by) = &t.superseded_by {
            happened.push(format!("{} recorded {by} as its successor", t.name));
        }
        if !d.succession_recorded.is_empty() {
            happened.push(format!(
                "{} recorded the succession of {}",
                d.name,
                d.succession_recorded.join(", ")
            ));
        }
        if !d.cohorts.is_empty() {
            happened.push(format!("{} holds {} cohorts", d.name, d.cohorts.len()));
        }
        if happened.is_empty() {
            if d.calendar_open != Some(false) {
                return Err(format!(
                    "the deployed authority {}'s standing template is not paused (POST /eval/template/pause), \
                     so a succession could start before the deploy",
                    d.name
                ));
            }
            return Ok(format!("rollback: {} -> {}", d.name, tracked.name()));
        }
        if !freeze {
            return Err(format!(
                "succession happened ({}), so a rollback to {} only freezes the record: neither admits a plan \
                 again (--freeze for a deliberate freeze)",
                happened.join("; "),
                tracked.name()
            ));
        }
        return Ok(format!(
            "rollback: {} -> {} (frozen, nothing admits: {}; {} holds {} cohorts, {} open)",
            d.name,
            tracked.name(),
            happened.join("; "),
            d.name,
            d.cohorts.len(),
            open.len()
        ));
    }
    Err(format!(
        "--switch needs the tree's authority {} to follow the deployed {} (predecessors {:?} then {}), \
         or to be one of its predecessors",
        tracked.name(),
        d.name,
        d.predecessors,
        d.name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_needs_the_public_hotkey_and_distinct_trust_roots() {
        let chain = chain_config(TESTNET_GENESIS, Some("601")).unwrap();
        let alice = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
        let vars = |extra: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                let base = [
                    ("COORDINATOR_HOTKEY_SS58", alice),
                    (
                        "PACKAGE_PUBLIC_KEY",
                        "1111111111111111111111111111111111111111111111111111111111111111",
                    ),
                    (
                        "RECEIPT_PUBLIC_KEY",
                        "2222222222222222222222222222222222222222222222222222222222222222",
                    ),
                    (
                        "AUTHORIZATION_PUBLIC_KEY",
                        "3333333333333333333333333333333333333333333333333333333333333333",
                    ),
                ];
                extra
                    .iter()
                    .chain(base.iter())
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            }
        };
        let s = Settings::default();
        let (cfg, _) = sign_config(vars(&[]), &chain, &s).unwrap();
        assert_eq!((cfg.netuid, cfg.era_period, cfg.version_key), (601, 64, 0));
        assert_eq!(
            hex::encode(cfg.hotkey),
            "d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d"
        );
        assert_eq!(
            sign_config(vars(&[("COORDINATOR_HOTKEY_SS58", "nope")]), &chain, &s).unwrap_err(),
            ConfigError::Hotkey
        );
        assert_eq!(
            sign_config(
                vars(&[(
                    "RECEIPT_PUBLIC_KEY",
                    "1111111111111111111111111111111111111111111111111111111111111111"
                )]),
                &chain,
                &s
            )
            .unwrap_err(),
            ConfigError::TrustedKeys
        );
        assert_eq!(
            sign_config(vars(&[("SIGN_ERA_PERIOD", "48")]), &chain, &s).unwrap_err(),
            ConfigError::Setting
        );
        let unset = chain_config(TESTNET_GENESIS, None).unwrap();
        assert_eq!(
            sign_config(vars(&[]), &unset, &s).unwrap_err(),
            ConfigError::Netuid
        );
    }

    #[test]
    fn netuid_is_configuration() {
        let c = chain_config(TESTNET_GENESIS, Some("REPLACE_WITH_NETUID")).unwrap();
        assert_eq!(c.netuid, None);
        assert_eq!(eval_cohort_name(&c), None);
        let c = chain_config(TESTNET_GENESIS, Some(" 17 ")).unwrap();
        assert_eq!(c.netuid, Some(17));
        assert!(eval_cohort_name(&c).unwrap().ends_with("/17"));
        assert_eq!(
            chain_config(TESTNET_GENESIS, Some("70000")),
            Err(ConfigError::Netuid)
        );
    }

    #[test]
    fn testnet_genesis_matches_the_shared_protocol_crate() {
        let configured = Id::parse(TESTNET_GENESIS).unwrap();
        assert_eq!(
            Digest::from(configured),
            slop_ninja_protocol::TESTNET_GENESIS
        );
        assert_eq!(Id::from(slop_ninja_protocol::TESTNET_GENESIS), configured);
    }

    #[test]
    fn testnet_only_genesis_refuses_other_chains() {
        let other_chain = format!("0x{}", "2f".repeat(32));
        assert_eq!(
            chain_config(&other_chain, None),
            Err(ConfigError::TestnetOnlyGenesis)
        );
        assert_eq!(chain_config("0x00", None), Err(ConfigError::Chain));
    }

    #[test]
    fn testnet_names_and_network_guard() {
        assert_eq!(network(Some("testnet")), Ok("testnet"));
        assert_eq!(network(Some("finney")), Err(ConfigError::Network));
        assert_eq!(network(None), Err(ConfigError::Network));
        assert_eq!(binding("CHAIN_JOURNAL", NETWORK), "CHAIN_JOURNAL_TESTNET");
        let c = chain_config(TESTNET_GENESIS, Some("601")).unwrap();
        assert!(chain_journal_name(&c).starts_with("chain-journal/testnet/0x8f9c"));
        assert!(
            eval_cohort_name(&c)
                .unwrap()
                .starts_with("eval-cohort/testnet/")
        );
    }

    #[test]
    fn the_original_authority_keeps_every_name_and_a_named_one_gets_its_own() {
        let c = chain_config(TESTNET_GENESIS, Some("601")).unwrap();
        let base = eval_cohort_name(&c).unwrap();
        // Unset, empty and `main` are the original authority, with no floor.
        for (name, first) in [(None, None), (Some(""), None), (Some(" main "), Some(""))] {
            let a = Authority::parse(name, first).unwrap();
            assert!(a.is_original());
            assert_eq!(a.name(), "main");
            assert_eq!(a.cohort_name(&c).unwrap(), base);
            assert_eq!(a.lane_scope("main"), "main");
            assert_eq!(a.probe_prefix(1), "probes/v1");
            assert_eq!(a.label_prefix(), None);
            assert_eq!(a.first_cohort(), 0);
            assert_eq!(a.admits("testnet-rv1-a", 1), Ok(()));
        }
        let a = Authority::parse(Some("r0b"), Some("13")).unwrap();
        assert_eq!(a, Authority::named("r0b", 13).unwrap());
        assert_eq!(a.name(), "r0b");
        let named = a.cohort_name(&c).unwrap();
        assert_eq!(named, format!("{base}/authority/r0b"));
        // Never the load instance's object, nor the original's.
        assert_ne!(named, format!("{base}/load"));
        assert_ne!(named, base);
        assert_eq!(a.lane_scope("main"), "main/authority/r0b");
        assert_eq!(a.probe_prefix(1), "probes/authority/r0b/v1");
        assert_eq!(a.label_prefix().as_deref(), Some("testnet-r0b-"));
        assert_eq!(a.first_cohort(), 13);
        // Labels and cohort numbers of the authority only.
        assert_eq!(a.admits("testnet-r0b-probe-v1", 13), Ok(()));
        assert_eq!(a.admits("testnet-r0b-14", 14), Ok(()));
        assert!(a.admits("testnet-rv1-g", 13).is_err());
        assert!(a.admits("testnet-r0b-", 13).is_err());
        assert!(a.admits("testnet-r0bx-1", 13).is_err());
        assert!(a.admits("testnet-r0b-probe-v1", 12).is_err());
        // The audit names: the original and any valid name; nothing else.
        assert_eq!(authority_cohort_name(&c, "main").unwrap(), base);
        assert_eq!(authority_cohort_name(&c, "r0b").unwrap(), named);
        for bad in [
            "load",
            "R0B",
            "r0-b",
            "0rb",
            "",
            "r0b/x",
            &"a".repeat(AUTHORITY_NAME_MAX_BYTES + 1),
        ] {
            assert_eq!(authority_cohort_name(&c, bad), None, "{bad}");
        }
        let unset = chain_config(TESTNET_GENESIS, None).unwrap();
        assert_eq!(a.cohort_name(&unset), None);
    }

    #[test]
    fn a_misconfigured_authority_is_refused() {
        for (name, first) in [
            (Some("r0b"), None),
            (Some("r0b"), Some("0")),
            (Some("r0b"), Some("-1")),
            (Some("r0b"), Some("x")),
            (Some("main"), Some("13")),
            (None, Some("13")),
            (Some("load"), Some("13")),
            (Some("r0-b"), Some("13")),
            (Some("R0b"), Some("13")),
            (Some("9r"), Some("13")),
        ] {
            assert_eq!(
                Authority::parse(name, first),
                Err(ConfigError::Authority),
                "{name:?} {first:?}"
            );
        }
        // The longest name leaves one byte of a label under the label bound.
        let long = "a".repeat(AUTHORITY_NAME_MAX_BYTES);
        let a = Authority::named(&long, 1).unwrap();
        let label = format!("{}x", a.label_prefix().unwrap());
        assert_eq!(label.len(), LABEL_MAX_BYTES);
        assert_eq!(a.admits(&label, 1), Ok(()));
    }

    #[test]
    fn a_retired_authority_keeps_its_names_and_admits_no_plan() {
        let c = chain_config(TESTNET_GENESIS, Some("601")).unwrap();
        let old = Authority::retired("main");
        assert!(old.is_retired() && old.is_original());
        assert_eq!(
            (old.lane_scope("main"), old.probe_prefix(1)),
            ("main".into(), "probes/v1".into())
        );
        assert_eq!(old.cohort_name(&c), eval_cohort_name(&c));
        assert!(old.admits("testnet-rv1-g", 13).is_err());
        let r0b = Authority::retired("r0b");
        assert_eq!(
            r0b.lane_scope("main"),
            Authority::named("r0b", 13).unwrap().lane_scope("main")
        );
        assert_eq!(r0b.probe_prefix(2), "probes/authority/r0b/v2");
        assert!(r0b.admits("testnet-r0b-20", 20).is_err());
        assert!(!Authority::named("r0b", 13).unwrap().is_retired());
    }

    #[test]
    fn a_named_authority_follows_its_predecessor_only_with_fresh_labels_and_numbers() {
        let earlier = [
            ("testnet-rv1-a", 7),
            ("testnet-rv1-f", 12),
            ("testnet-m4-rehearsal-1-k4-n200", 1),
        ];
        assert_eq!(
            Authority::named("r0b", 13).unwrap().follows(earlier),
            Ok(())
        );
        // A name whose prefix an earlier label has, or a floor at or below an
        // earlier number, is refused.
        assert!(
            Authority::named("rv1", 13)
                .unwrap()
                .follows(earlier)
                .is_err()
        );
        assert!(
            Authority::named("m4", 13)
                .unwrap()
                .follows(earlier)
                .is_err()
        );
        assert!(
            Authority::named("r0b", 12)
                .unwrap()
                .follows(earlier)
                .is_err()
        );
        assert_eq!(Authority::default().follows(earlier), Ok(()));
        // Two names never share a label: no name is another's prefix plus a hyphen.
        let (a, b) = (
            Authority::named("r0", 1).unwrap(),
            Authority::named("r0b", 1).unwrap(),
        );
        assert!(a.admits("testnet-r0b-1", 1).is_err());
        assert!(b.admits("testnet-r0-b1", 1).is_err());
    }

    #[test]
    fn earlier_authorities_parse_and_a_misconfiguration_is_refused() {
        let a = Authority::parse_with(Some("r0c"), Some("20"), Some(" r0b , ")).unwrap();
        assert_eq!(a, Authority::named_after("r0c", 20, &["r0b"]).unwrap());
        assert_eq!(a.predecessors(), vec!["main", "r0b"]);
        assert_eq!(
            Authority::named("r0b", 13).unwrap().predecessors(),
            vec!["main"]
        );
        for (name, first, previous) in [
            (Some("main"), None, Some("r0b")),
            (None, None, Some("r0b")),
            (Some("r0b"), Some("13"), Some("r0b")),
            (Some("r0c"), Some("20"), Some("r0b,r0b")),
            (Some("r0c"), Some("20"), Some("main")),
            (Some("r0c"), Some("20"), Some("load")),
            (Some("r0c"), Some("20"), Some("R0B")),
        ] {
            assert_eq!(
                Authority::parse_with(name, first, previous),
                Err(ConfigError::Authority),
                "{name:?} {previous:?}"
            );
        }
        // The tracked form of the original: empty first cohort and predecessors.
        assert_eq!(
            Authority::parse_with(Some("main"), Some(""), Some("")),
            Ok(Authority::default())
        );
    }

    #[test]
    fn a_predecessor_records_a_successor_only_while_idle_and_disjoint() {
        let req = Supersede::of(&Authority::named("r0b", 13).unwrap()).unwrap();
        assert_eq!(Supersede::of(&Authority::default()), None);
        let closed = [
            ("testnet-rv1-a", 7, "closed"),
            ("testnet-rv1-f", 12, "abandoned"),
        ];
        assert_eq!(req.check(closed.iter().copied()), Ok(()));
        assert_eq!(req.check(std::iter::empty()), Ok(()));
        for phase in [
            "queued",
            "admission",
            "generating",
            "scoring",
            "packaging",
            "transition",
        ] {
            let open = [("testnet-rv1-a", 7, "closed"), ("testnet-rv1-g", 12, phase)];
            assert_eq!(
                req.check(open.iter().copied()),
                Err("predecessor_busy"),
                "{phase}"
            );
        }
        let numbered = [("testnet-rv1-a", 13, "closed")];
        assert_eq!(
            req.check(numbered.iter().copied()),
            Err("predecessor_numbers")
        );
        let labelled = [("testnet-r0b-x", 1, "closed")];
        assert_eq!(
            req.check(labelled.iter().copied()),
            Err("predecessor_numbers")
        );
        let bad = Supersede {
            by: "R0B".into(),
            first_cohort: 13,
        };
        assert_eq!(bad.check(std::iter::empty()), Err("successor_name"));
    }

    /// A predecessor answers with the first successor it recorded; the
    /// successor accepts only itself or an authority it lists after that
    /// predecessor (asked in turn), so a deploy that leaves an earlier
    /// authority out of `EVAL_MAIN_PREVIOUS` never completes succession.
    #[test]
    fn a_predecessor_superseded_by_an_unlisted_authority_refuses_the_successor() {
        let r0b = Authority::named("r0b", 13).unwrap();
        assert_eq!(succession_reply(&r0b, "main", Some("r0b")), Ok(()));
        // main is superseded by r0b; r0x leaves r0b out of its list.
        let r0x = Authority::named("r0x", 14).unwrap();
        assert_eq!(
            succession_reply(&r0x, "main", Some("r0b")),
            Err("predecessor_superseded_elsewhere")
        );
        // r0c lists r0b after main: main's answer is accepted, and r0b, asked
        // next, must have recorded r0c.
        let r0c = Authority::named_after("r0c", 15, &["r0b"]).unwrap();
        assert_eq!(succession_reply(&r0c, "main", Some("r0b")), Ok(()));
        assert_eq!(succession_reply(&r0c, "r0b", Some("r0c")), Ok(()));
        assert_eq!(
            succession_reply(&r0c, "r0b", Some("r0x")),
            Err("predecessor_superseded_elsewhere")
        );
        // Never an authority listed before the predecessor, nor an unknown one.
        assert_eq!(
            succession_reply(&r0c, "r0b", Some("main")),
            Err("predecessor_superseded_elsewhere")
        );
        assert_eq!(
            succession_reply(&r0c, "r0z", Some("r0c")),
            Err("predecessor_unknown")
        );
        assert_eq!(
            succession_reply(&r0c, "main", None),
            Err("predecessor_unreadable")
        );
        // A predecessor the deployment skipped over that never recorded anyone
        // (r0b deployed, never used) leaves main recording r0c directly.
        assert_eq!(succession_reply(&r0c, "main", Some("r0c")), Ok(()));
    }

    /// A recorded succession covers its first cohort and any later one; a
    /// lower first cohort asks the predecessor again.
    #[test]
    fn a_recorded_succession_covers_only_its_first_cohort_and_later() {
        let done = vec![("main".to_owned(), 14)];
        assert!(succession_recorded(&done, "main", 14));
        assert!(succession_recorded(&done, "main", 20));
        assert!(!succession_recorded(&done, "main", 13));
        assert!(!succession_recorded(&done, "r0b", 14));
        let r0b = Authority::named("r0b", 14).unwrap();
        assert!(succession_complete(&r0b, &done));
        assert!(!succession_complete(
            &Authority::named("r0b", 13).unwrap(),
            &done
        ));
        let r0c = Authority::named_after("r0c", 20, &["r0b"]).unwrap();
        assert!(!succession_complete(&r0c, &done));
        let both = vec![("main".to_owned(), 20), ("r0b".to_owned(), 20)];
        assert!(succession_complete(&r0c, &both));
        assert!(succession_complete(&Authority::default(), &[]));
        // Ticks kick the predecessors only until succession is complete.
        assert_eq!(
            kicked_authorities(&r0c, true, false),
            ["r0c", "main", "r0b"]
        );
        assert_eq!(kicked_authorities(&r0c, true, true), ["r0c"]);
        assert_eq!(kicked_authorities(&r0c, false, false), ["r0c"]);
    }

    /// One store of `succeed_with`: `(done, through)`.
    type Stored = (Vec<(String, u64)>, Vec<(String, String)>);

    /// Peers for `succeed_with`: fixed records and supersede answers per
    /// authority, and every call made.
    #[derive(Default)]
    struct Table {
        records: Vec<(&'static str, Option<SuccessionRecord>)>,
        answers: Vec<(&'static str, SupersedeAnswer)>,
        read: Vec<String>,
        asked: Vec<String>,
        stored: Vec<Stored>,
    }

    impl SuccessionPeers for Table {
        async fn record(&mut self, authority: &str) -> Option<SuccessionRecord> {
            self.read.push(authority.to_owned());
            self.records
                .iter()
                .find(|(n, _)| *n == authority)
                .and_then(|(_, r)| r.clone())
        }
        async fn supersede(&mut self, predecessor: &str) -> SupersedeAnswer {
            self.asked.push(predecessor.to_owned());
            self.answers
                .iter()
                .find(|(n, _)| *n == predecessor)
                .map_or(SupersedeAnswer::Unknown, |(_, a)| a.clone())
        }
        async fn store(
            &mut self,
            done: &[(String, u64)],
            through: &[(String, String)],
        ) -> Result<(), &'static str> {
            self.stored.push((done.to_vec(), through.to_vec()));
            Ok(())
        }
    }

    /// Runs `succeed_with` to completion (the table answers at once).
    fn succeed_now(
        successor: &Authority,
        done: &mut Vec<(String, u64)>,
        through: &mut Vec<(String, String)>,
        table: &mut Table,
    ) -> Result<(), &'static str> {
        let mut f = std::pin::pin!(succeed_with(successor, done, through, table));
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match f.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(r) => r,
            std::task::Poll::Pending => panic!("the table never waits"),
        }
    }

    fn rec(recorded: &[(&str, u64)], holds_cohorts: bool) -> SuccessionRecord {
        SuccessionRecord {
            succession_recorded: recorded
                .iter()
                .map(|(n, f)| ((*n).to_owned(), *f))
                .collect(),
            holds_cohorts,
            retention_notices: Vec::new(),
        }
    }

    /// Skipping a covered predecessor's notice, on the 2026-11-09 testnet
    /// shape: r0 recorded `(main, 14)`, ran cohorts 14-21 and delivered its
    /// notice to main after cohort 16 (main's highest cohort is 13); r1
    /// (first cohort 22) covers main through r0. r1 skips main only while
    /// r0's record shows that notice; otherwise it tells main itself.
    #[test]
    fn a_covered_predecessor_needs_no_notice_once_its_successor_delivered_one() {
        let r1 = Authority::named_after("r1", 22, &["r0"]).unwrap();
        let mut r0 = rec(&[("main", 14)], true);
        // No notice recorded by r0 (or a record from an older build).
        assert_eq!(retention_covered(&r1, "main", "r0", &r0), None);
        // A notice after cohort 14 or 15 is not past main's highest (at most
        // 13) + 2 by the record alone: r1 tells main itself.
        r0.retention_notices = vec![("main".into(), 14)];
        assert_eq!(retention_covered(&r1, "main", "r0", &r0), None);
        r0.retention_notices = vec![("main".into(), 15)];
        assert_eq!(retention_covered(&r1, "main", "r0", &r0), Some(15));
        r0.retention_notices = vec![("other".into(), 30), ("main".into(), 16)];
        assert_eq!(retention_covered(&r1, "main", "r0", &r0), Some(16));
        // The cover itself must still hold on the record read now.
        let mut idle = r0.clone();
        idle.holds_cohorts = false;
        assert_eq!(retention_covered(&r1, "main", "r0", &idle), None, "through holds no cohort");
        let mut late = r0.clone();
        late.succession_recorded = vec![("main".into(), 23)];
        assert_eq!(retention_covered(&r1, "main", "r0", &late), None, "recorded for a later first cohort");
        // Only a predecessor listed before `through` is covered by it, and
        // never `through` itself.
        assert_eq!(retention_covered(&r1, "r0", "main", &r0), None);
        assert_eq!(retention_covered(&r1, "r0", "r0", &r0), None);
        // An unknown name is never covered.
        assert_eq!(retention_covered(&r1, "zz", "r0", &r0), None);
        // The record's JSON from a build before `retention_notices`.
        let old: SuccessionRecord =
            serde_json::from_str(r#"{"succession_recorded":[["main",14]],"holds_cohorts":true}"#).unwrap();
        assert!(old.retention_notices.is_empty());
        assert_eq!(retention_covered(&r1, "main", "r0", &old), None);
    }

    fn pairs(v: &[(&str, u64)]) -> Vec<(String, u64)> {
        v.iter().map(|(n, f)| ((*n).to_owned(), *f)).collect()
    }

    /// The 2026-11-06 testnet shape: main superseded by r0 on 11-02 (r0
    /// recorded `(main, 14)` and ran cohorts 14-21), main's store no longer
    /// opens, and r1 (first cohort 22, predecessors main, r0) succeeds r0.
    /// main is covered through r0's record and never asked; r0 is asked and
    /// records r1; nothing is asked twice, and a second run asks nothing.
    #[test]
    fn a_superseded_predecessor_is_covered_through_its_successors_record() {
        let r1 = Authority::named_after("r1", 22, &["r0"]).unwrap();
        let mut t = Table {
            records: vec![("r0", Some(rec(&[("main", 14)], true)))],
            answers: vec![
                ("main", SupersedeAnswer::Unreadable),
                ("r0", SupersedeAnswer::Recorded(Some("r1".into()))),
            ],
            ..Table::default()
        };
        let (mut done, mut through) = (Vec::new(), Vec::new());
        assert_eq!(succeed_now(&r1, &mut done, &mut through, &mut t), Ok(()));
        assert_eq!(t.asked, ["r0"]);
        assert_eq!(t.read, ["r0"]);
        assert_eq!(done, pairs(&[("main", 22), ("r0", 22)]));
        assert_eq!(through, [("main".to_owned(), "r0".to_owned())]);
        assert!(succession_complete(&r1, &done));
        // Stored after each step: main (with its cover), then r0.
        assert_eq!(t.stored.len(), 2);
        assert_eq!(t.stored[0].0, pairs(&[("main", 22)]));
        assert_eq!(t.stored[0].1, through);
        // Retried (another plan): nothing read, asked or stored again.
        let mut again = Table::default();
        assert_eq!(
            succeed_now(&r1, &mut done, &mut through, &mut again),
            Ok(())
        );
        assert!(again.read.is_empty() && again.asked.is_empty() && again.stored.is_empty());
        // r0 refusing (busy, say) after main was covered leaves main covered and
        // r0 unrecorded; the retry asks only r0, and main is never asked.
        let mut t = Table {
            records: vec![("r0", Some(rec(&[("main", 14)], true)))],
            answers: vec![(
                "r0",
                SupersedeAnswer::Refused(Some("predecessor_work".into())),
            )],
            ..Table::default()
        };
        let (mut done, mut through) = (Vec::new(), Vec::new());
        assert_eq!(
            succeed_now(&r1, &mut done, &mut through, &mut t),
            Err("predecessor_work")
        );
        assert_eq!(done, pairs(&[("main", 22)]));
        let mut retry = Table {
            answers: vec![("r0", SupersedeAnswer::Recorded(Some("r1".into())))],
            ..Table::default()
        };
        assert_eq!(
            succeed_now(&r1, &mut done, &mut through, &mut retry),
            Ok(())
        );
        assert_eq!(retry.asked, ["r0"]);
        assert!(retry.read.is_empty());
        // r0 already superseded by another authority still refuses r1.
        let mut t = Table {
            records: vec![("r0", Some(rec(&[("main", 14)], true)))],
            answers: vec![("r0", SupersedeAnswer::Recorded(Some("r9".into())))],
            ..Table::default()
        };
        let (mut done, mut through) = (Vec::new(), Vec::new());
        assert_eq!(
            succeed_now(&r1, &mut done, &mut through, &mut t),
            Err("predecessor_superseded_elsewhere")
        );
        assert!(!succession_complete(&r1, &done));
    }

    /// A predecessor no later record covers is asked, and one that does not
    /// answer refuses before any later predecessor is asked (so none records
    /// this successor while an earlier one may still hold work).
    #[test]
    fn an_unreadable_predecessor_no_record_covers_still_refuses() {
        let r1 = Authority::named_after("r1", 22, &["r0"]).unwrap();
        let unread = ("main", SupersedeAnswer::Unreadable);
        let r0_ok = ("r0", SupersedeAnswer::Recorded(Some("r1".into())));
        for (why, record) in [
            ("r0 does not answer", None),
            ("r0 never recorded main", Some(rec(&[], true))),
            (
                "r0 recorded main but took no plan",
                Some(rec(&[("main", 14)], false)),
            ),
            (
                "r0 recorded main for a later first cohort",
                Some(rec(&[("main", 23)], true)),
            ),
            (
                "r0 recorded an authority r1 does not list",
                Some(rec(&[("main", 14), ("rx", 14)], true)),
            ),
        ] {
            let mut t = Table {
                records: vec![("r0", record)],
                answers: vec![unread.clone(), r0_ok.clone()],
                ..Table::default()
            };
            let (mut done, mut through) = (Vec::new(), Vec::new());
            assert_eq!(
                succeed_now(&r1, &mut done, &mut through, &mut t),
                Err("predecessor_unreadable"),
                "{why}"
            );
            assert_eq!(t.asked, ["main"], "{why}");
            assert!(
                done.is_empty() && through.is_empty() && t.stored.is_empty(),
                "{why}"
            );
        }
        // Unreachable and unknown keep their own codes.
        for (answer, code) in [
            (SupersedeAnswer::Unreachable, "predecessor_unreachable"),
            (SupersedeAnswer::Unknown, "predecessor_unknown"),
        ] {
            let mut t = Table {
                answers: vec![("main", answer), r0_ok.clone()],
                ..Table::default()
            };
            let (mut done, mut through) = (Vec::new(), Vec::new());
            assert_eq!(succeed_now(&r1, &mut done, &mut through, &mut t), Err(code));
            assert_eq!(t.asked, ["main"]);
        }
        // The last listed predecessor has nothing after it: it is always asked.
        let r0 = Authority::named("r0", 14).unwrap();
        let mut t = Table {
            answers: vec![unread],
            ..Table::default()
        };
        let (mut done, mut through) = (Vec::new(), Vec::new());
        assert_eq!(
            succeed_now(&r0, &mut done, &mut through, &mut t),
            Err("predecessor_unreadable")
        );
        assert!(t.read.is_empty());
    }

    /// Without a covering record the predecessors are asked in order as
    /// before; a direct record replaces an earlier cover. The original and
    /// a retired authority need nothing.
    #[test]
    fn a_predecessor_without_a_cover_is_asked_as_before() {
        let r1 = Authority::named_after("r1", 22, &["r0"]).unwrap();
        let mut t = Table {
            records: vec![("r0", Some(rec(&[], false)))],
            answers: vec![
                ("main", SupersedeAnswer::Recorded(Some("r0".into()))),
                ("r0", SupersedeAnswer::Recorded(Some("r1".into()))),
            ],
            ..Table::default()
        };
        let mut done = Vec::new();
        // A cover kept for a higher first cohort no longer applies (main is
        // unrecorded for 22): asked directly, the cover goes.
        let mut through = vec![("main".to_owned(), "r0".to_owned())];
        done.push(("main".to_owned(), 30));
        assert_eq!(succeed_now(&r1, &mut done, &mut through, &mut t), Ok(()));
        assert_eq!(t.asked, ["main", "r0"]);
        assert_eq!(done, pairs(&[("main", 22), ("r0", 22)]));
        assert!(through.is_empty());
        let mut t = Table::default();
        for a in [Authority::default(), Authority::retired("r0")] {
            assert_eq!(
                succeed_now(&a, &mut Vec::new(), &mut Vec::new(), &mut t),
                Ok(())
            );
        }
        assert!(t.asked.is_empty() && t.read.is_empty());
    }

    /// `succession_through`'s conditions one at a time.
    #[test]
    fn a_cover_needs_a_later_listed_authority_that_took_a_plan() {
        let r2 = Authority::named_after("r2", 30, &["r0", "r1"]).unwrap();
        let ok = rec(&[("main", 14)], true);
        assert!(succession_through(&r2, "main", "r0", &ok));
        assert!(succession_through(
            &r2,
            "main",
            "r1",
            &rec(&[("main", 22), ("r0", 22)], true)
        ));
        assert!(succession_through(
            &r2,
            "r0",
            "r1",
            &rec(&[("main", 22), ("r0", 22)], true)
        ));
        // Not after the predecessor, not listed, or the successor itself.
        assert!(!succession_through(
            &r2,
            "r0",
            "r0",
            &rec(&[("r0", 14)], true)
        ));
        assert!(!succession_through(
            &r2,
            "r1",
            "r0",
            &rec(&[("r1", 14)], true)
        ));
        assert!(!succession_through(&r2, "main", "r9", &ok));
        assert!(!succession_through(&r2, "main", "r2", &ok));
        // A record listing an authority after `through` in r2's own list.
        assert!(!succession_through(
            &r2,
            "main",
            "r0",
            &rec(&[("main", 14), ("r1", 14)], true)
        ));
        // A first cohort above r2's.
        assert!(!succession_through(
            &r2,
            "main",
            "r0",
            &rec(&[("main", 31)], true)
        ));
        assert!(succession_through(
            &r2,
            "main",
            "r0",
            &rec(&[("main", 30)], true)
        ));
        // The original authority has no predecessors to cover.
        assert!(!succession_through(
            &Authority::default(),
            "main",
            "r0",
            &ok
        ));
    }

    fn status_of(
        name: Option<&str>,
        first: u64,
        preds: &[&str],
        cohorts: serde_json::Value,
    ) -> serde_json::Value {
        let mut st = serde_json::json!({"schema_version": 12, "cohorts": cohorts,
            "calendar": {"template": {"paused": true}}});
        if let Some(n) = name {
            st["authority_name"] = serde_json::json!(n);
            st["first_cohort"] = serde_json::json!(first);
            st["predecessors"] = serde_json::json!(preds);
            st["superseded_by"] = serde_json::Value::Null;
            st["succession_recorded"] = serde_json::json!([]);
            st["work"] = serde_json::json!(false);
        }
        st
    }

    /// The pre-deploy check fails closed: an error reply, a Worker status
    /// with no eval status, and a build that reports no authority are never
    /// taken for the original authority.
    #[test]
    fn the_deploy_check_refuses_a_status_it_cannot_read() {
        let main = Authority::default();
        let closed =
            serde_json::json!([{"label": "testnet-rv1-a", "cohort": 12, "phase": "closed"}]);
        for bad in [
            serde_json::json!({"error": "store_unavailable"}),
            serde_json::json!({"ok": true, "eval_cohort": null}),
            serde_json::json!({"ok": true, "eval_cohort": {"error": "store_unavailable"}}),
            serde_json::json!({"authority_name": "main", "first_cohort": 0, "predecessors": [], "cohorts": []}),
            status_of(None, 0, &[], closed.clone()),
        ] {
            for mode in [DeployMode::Same, DeployMode::Switch { freeze: false }] {
                assert!(
                    deploy_check(&main, &bad, None, mode).is_err(),
                    "{bad} {mode:?}"
                );
            }
        }
        let mut no_preds = status_of(Some("main"), 0, &[], closed.clone());
        no_preds.as_object_mut().unwrap().remove("predecessors");
        assert!(deploy_check(&main, &no_preds, None, DeployMode::Same).is_err());
        let ok = status_of(Some("main"), 0, &[], closed.clone());
        assert_eq!(
            deploy_check(&main, &ok, None, DeployMode::Same),
            Ok("same authority: main".into())
        );
        // The Worker's /status carries the same status as eval_cohort.
        let wrapped = serde_json::json!({"ok": true, "eval_cohort": ok});
        assert!(deploy_check(&main, &wrapped, None, DeployMode::Same).is_ok());
        // The first deploy of authority support: only over a build that
        // reports no authority, only for a tree that tracks the original.
        let old = status_of(None, 0, &[], closed);
        assert!(deploy_check(&main, &old, None, DeployMode::FirstDeploy).is_ok());
        assert!(deploy_check(&main, &wrapped, None, DeployMode::FirstDeploy).is_err());
        assert!(
            deploy_check(
                &Authority::named("r0b", 13).unwrap(),
                &old,
                None,
                DeployMode::FirstDeploy
            )
            .is_err()
        );
        assert!(
            deploy_check(
                &main,
                &serde_json::json!({"error": "x"}),
                None,
                DeployMode::FirstDeploy
            )
            .is_err()
        );
        // A status without the recorded succession state or the work flag
        // (or an unreadable succession record, null) refuses.
        for (key, bad) in [
            ("superseded_by", None),
            ("superseded_by", Some(serde_json::json!(7))),
            ("succession_recorded", None),
            ("succession_recorded", Some(serde_json::Value::Null)),
            (
                "succession_recorded",
                Some(serde_json::json!([{"first_cohort": 13}])),
            ),
            ("work", None),
            ("work", Some(serde_json::Value::Null)),
        ] {
            let mut st = status_of(Some("main"), 0, &[], serde_json::json!([]));
            match bad {
                None => {
                    st.as_object_mut().unwrap().remove(key);
                }
                Some(v) => st[key] = v,
            }
            for mode in [
                DeployMode::Same,
                DeployMode::Frozen,
                DeployMode::Switch { freeze: false },
            ] {
                assert!(
                    deploy_check(&main, &st, None, mode).is_err(),
                    "{key} {mode:?}"
                );
            }
        }
    }

    /// A switch needs an idle deployed authority with no work left, a paused
    /// calendar, numbers below the new first cohort and no other successor
    /// recorded; a rollback needs the tree authority's own status and, once
    /// succession happened (recorded on either side, or a cohort listed), an
    /// explicit freeze.
    #[test]
    fn the_deploy_check_passes_a_switch_only_from_an_idle_authority_and_a_rollback_only_before_succession()
     {
        let sw = DeployMode::Switch { freeze: false };
        let fr = DeployMode::Switch { freeze: true };
        let r0b = Authority::named("r0b", 13).unwrap();
        let idle = serde_json::json!([{"label": "testnet-rv1-f", "cohort": 12, "phase": "closed"}]);
        let st = status_of(Some("main"), 0, &[], idle.clone());
        assert_eq!(
            deploy_check(&r0b, &st, None, sw),
            Ok("switch: main -> r0b (first cohort 13)".into())
        );
        assert!(deploy_check(&r0b, &st, None, DeployMode::Same).is_err());
        let open = serde_json::json!([{"label": "testnet-rv1-f", "cohort": 12, "phase": "closed"},
            {"label": "testnet-rv1-g", "cohort": 13, "phase": "admission"}]);
        let e = deploy_check(&r0b, &status_of(Some("main"), 0, &[], open), None, sw).unwrap_err();
        assert!(e.contains("not idle"), "{e}");
        // Closed, but a closure, settlement, purge or clean-up is left.
        let mut busy = st.clone();
        busy["work"] = serde_json::json!(true);
        assert!(
            deploy_check(&r0b, &busy, None, sw)
                .unwrap_err()
                .contains("still has work")
        );
        let mut unpaused = st.clone();
        unpaused["calendar"]["template"]["paused"] = serde_json::json!(false);
        assert!(
            deploy_check(&r0b, &unpaused, None, sw)
                .unwrap_err()
                .contains("not paused")
        );
        let mut no_template = st.clone();
        no_template["calendar"]["template"] = serde_json::Value::Null;
        assert!(deploy_check(&r0b, &no_template, None, sw).is_ok());
        let mut no_calendar = st.clone();
        no_calendar.as_object_mut().unwrap().remove("calendar");
        assert!(deploy_check(&r0b, &no_calendar, None, sw).is_err());
        let high = serde_json::json!([{"label": "testnet-rv1-g", "cohort": 13, "phase": "closed"}]);
        assert!(deploy_check(&r0b, &status_of(Some("main"), 0, &[], high), None, sw).is_err());
        // A tree that leaves the deployed authority out of its predecessors.
        let r0c = Authority::named("r0c", 20).unwrap();
        let r0b_st = status_of(Some("r0b"), 13, &["main"], serde_json::json!([]));
        assert!(deploy_check(&r0c, &r0b_st, None, sw).is_err());
        let r0c = Authority::named_after("r0c", 20, &["r0b"]).unwrap();
        assert!(deploy_check(&r0c, &r0b_st, None, sw).is_ok());

        // A deployed authority a successor superseded (a frozen rollback, or
        // succession recorded while it was deployed): a routine deploy of it
        // is refused (it admits nothing), --freeze alone redeploys it frozen,
        // a switch to another successor is refused (it would wait on
        // predecessor_superseded_elsewhere), and a switch to the recorded
        // successor passes.
        let main = Authority::default();
        let mut sup = status_of(
            Some("main"),
            0,
            &[],
            serde_json::json!([{"label": "testnet-adm-13", "cohort": 13, "phase": "abandoned"}]),
        );
        sup["superseded_by"] = serde_json::json!("r0b");
        let e = deploy_check(&main, &sup, None, DeployMode::Same).unwrap_err();
        assert!(e.contains("superseded by r0b"), "{e}");
        assert_eq!(
            deploy_check(&main, &sup, None, DeployMode::Frozen),
            Ok("same authority: main (frozen: superseded by r0b, it admits nothing)".into())
        );
        assert!(deploy_check(&main, &st, None, DeployMode::Frozen).is_err());
        let r0c_after_main = Authority::named("r0c", 20).unwrap();
        let e = deploy_check(&r0c_after_main, &sup, None, sw).unwrap_err();
        assert!(e.contains("predecessor_superseded_elsewhere"), "{e}");
        assert!(deploy_check(&Authority::named("r0b", 14).unwrap(), &sup, None, sw).is_ok());

        // Rollback: it needs the tree authority's own status.
        let main_clean = status_of(Some("main"), 0, &[], idle.clone());
        let e = deploy_check(&main, &r0b_st, None, sw).unwrap_err();
        assert!(e.contains("--target-status"), "{e}");
        let e = deploy_check(&main, &r0b_st, Some(&r0b_st), sw).unwrap_err();
        assert!(e.contains("not the tree's main"), "{e}");
        // Before succession (nothing recorded on either side, no cohort, a
        // paused template) it passes.
        assert_eq!(
            deploy_check(&main, &r0b_st, Some(&main_clean), sw),
            Ok("rollback: r0b -> main".into())
        );
        let mut r0b_unpaused = r0b_st.clone();
        r0b_unpaused["calendar"]["template"]["paused"] = serde_json::json!(false);
        assert!(
            deploy_check(&main, &r0b_unpaused, Some(&main_clean), sw)
                .unwrap_err()
                .contains("not paused")
        );
        // Succession recorded before any cohort exists (a template installed
        // that opened nothing yet, or a plan whose queue failed): the named
        // authority lists no cohort, but the original recorded it, or it
        // recorded the original's succession. Only --freeze passes.
        for (deployed, target) in [
            (r0b_st.clone(), sup.clone()),
            (
                {
                    let mut d = r0b_st.clone();
                    d["succession_recorded"] =
                        serde_json::json!([{"predecessor": "main", "first_cohort": 13}]);
                    d
                },
                main_clean.clone(),
            ),
            (
                status_of(
                    Some("r0b"),
                    13,
                    &["main"],
                    serde_json::json!([{"label": "testnet-r0b-a", "cohort": 14, "phase": "scoring"}]),
                ),
                main_clean.clone(),
            ),
        ] {
            let e = deploy_check(&main, &deployed, Some(&target), sw).unwrap_err();
            assert!(e.contains("--freeze"), "{e}");
            let ok = deploy_check(&main, &deployed, Some(&target), fr).unwrap();
            assert!(
                ok.starts_with("rollback: r0b -> main (frozen, nothing admits"),
                "{ok}"
            );
        }
        // A rollback to a named predecessor keeps its own predecessors.
        let r0c_st = status_of(Some("r0c"), 20, &["main", "r0b"], serde_json::json!([]));
        let r0b_clean = status_of(Some("r0b"), 13, &["main"], serde_json::json!([]));
        assert!(
            deploy_check(
                &Authority::named("r0b", 13).unwrap(),
                &r0c_st,
                Some(&r0b_clean),
                sw
            )
            .is_ok()
        );
        assert!(
            deploy_check(
                &Authority::named_after("r0b", 13, &["r0x"]).unwrap(),
                &r0c_st,
                Some(&r0b_clean),
                sw
            )
            .is_err()
        );
        // The same authority is never a switch.
        assert!(deploy_check(&main, &st, None, sw).is_err());
    }

    #[test]
    fn only_the_exact_eval_role_var_selects_the_evaluation_worker() {
        assert_eq!(role(Some("eval")), Role::Eval);
        for other in [
            None,
            Some(""),
            Some("Eval"),
            Some(" eval"),
            Some("coordinator"),
        ] {
            assert_eq!(role(other), Role::Coordinator, "{other:?}");
        }
    }

    #[test]
    fn settings_default_and_bounds() {
        let none = |_: &str| None;
        let d = settings(none).unwrap();
        assert_eq!((d.silence_ms, d.behind_blocks), (300_000, 25));
        assert_eq!((d.rpc_calls_per_minute, d.hashes_per_call), (12, 32));
        let custom = settings(|k: &str| match k {
            "WATCHDOG_SILENCE_SECS" => Some("120".into()),
            "WATCHDOG_BEHIND_BLOCKS" => Some("10".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!((custom.silence_ms, custom.behind_blocks), (120_000, 10));
        for (k, v) in [
            ("WATCHDOG_SILENCE_SECS", "0"),
            ("WATCHDOG_SILENCE_SECS", "-5"),
            ("RPC_CALLS_PER_MINUTE", "21"),
            ("RPC_CALLS_PER_MINUTE", "1"),
            ("RPC_HASHES_PER_CALL", "33"),
            ("RPC_HASHES_PER_CALL", "1"),
        ] {
            let get = |name: &str| (name == k).then(|| v.to_owned());
            assert_eq!(settings(get), Err(ConfigError::Setting), "{k}={v}");
        }
    }

    #[test]
    fn rpc_url_is_https_without_credentials() {
        assert_eq!(rpc_url(None).unwrap(), "https://test.finney.opentensor.ai");
        assert!(rpc_url(Some("https://test.chain.opentensor.ai")).is_ok());
        for bad in [
            "http://test.finney.opentensor.ai",
            "https://user:pw@test.finney.opentensor.ai",
            "https://test.finney.opentensor.ai/?key=1",
            "wss://test.finney.opentensor.ai",
            "https://",
        ] {
            assert_eq!(rpc_url(Some(bad)), Err(ConfigError::RpcUrl), "{bad}");
        }
    }

    #[test]
    fn bearer_tokens_compare_exactly() {
        let secret = "s".repeat(40);
        assert!(bearer_matches(Some(&format!("Bearer {secret}")), &secret));
        assert!(!bearer_matches(Some(&format!("Bearer {secret}x")), &secret));
        assert!(!bearer_matches(Some(&format!("bearer {secret}")), &secret));
        assert!(!bearer_matches(Some(&secret), &secret));
        assert!(!bearer_matches(None, &secret));
        // A short secret is never accepted, even when presented exactly.
        assert!(!bearer_matches(Some("Bearer short"), "short"));
    }

    #[test]
    fn receiver_requires_canonical_inputs() {
        let c = chain_config(TESTNET_GENESIS, None).unwrap();
        let secret = "11".repeat(32);
        let sub = format!("0x{}", "ab".repeat(32));
        assert!(receiver_config(&c, DEFAULT_KEY_ID, &secret, &sub).is_ok());
        assert!(matches!(
            receiver_config(&c, DEFAULT_KEY_ID, &"00".repeat(32), &sub),
            Err(ConfigError::SigningSecret)
        ));
        assert!(matches!(
            receiver_config(&c, DEFAULT_KEY_ID, &secret[2..], &sub),
            Err(ConfigError::SigningSecret)
        ));
        assert!(matches!(
            receiver_config(&c, "bad key", &secret, &sub),
            Err(ConfigError::KeyId)
        ));
        assert!(matches!(
            receiver_config(&c, DEFAULT_KEY_ID, &secret, &sub.to_uppercase()),
            Err(ConfigError::Subscription)
        ));
    }

    /// A report about a cohort reaches the known authority whose label it
    /// is, the current one or a predecessor (the publisher's publication
    /// report after a succession).
    #[test]
    fn a_label_names_the_known_authority_that_ran_it() {
        let original = Authority::default();
        assert_eq!(original.label_authority("testnet-rv1-a"), Some("main"));
        assert_eq!(
            original.label_authority("testnet-r0b-14"),
            Some("main"),
            "not a known name"
        );
        let r0c = Authority::named_after("r0c", 20, &["r0b"]).unwrap();
        assert_eq!(r0c.label_authority("testnet-r0c-21"), Some("r0c"));
        assert_eq!(r0c.label_authority("testnet-r0b-14"), Some("r0b"));
        assert_eq!(r0c.label_authority("testnet-rv1-a"), Some("main"));
        // A prefix of a name is not that name, nor is another network's.
        assert_eq!(r0c.label_authority("testnet-r0bx-1"), Some("main"));
        assert_eq!(r0c.label_authority("testnet-r0b"), Some("main"));
        assert_eq!(r0c.label_authority("finney-r0c-21"), Some("main"));
        assert_eq!(r0c.label_authority(""), Some("main"));
    }
}
