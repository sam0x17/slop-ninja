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

/// Durable Object instance name for a subnet's cohort authority.
pub fn eval_cohort_name(chain: &ChainConfig) -> Option<String> {
    chain
        .netuid
        .map(|netuid| format!("eval-cohort/{NETWORK}/{}/{netuid}", chain.genesis.to_hex()))
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
}
