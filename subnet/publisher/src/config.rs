//! Deployment configuration: Worker vars and secrets, parsed once per run
//! through a getter so native tests feed a map.
//!
//! The trust roots are not configuration: they are the tracked
//! `subnet/coordinator/testnet-trust.json`, compiled in, so the keys the
//! publisher verifies with and the file it publishes are the same bytes.

use std::collections::BTreeSet;

use serde::Deserialize;
use slop_ninja_protocol::{AccountId, ChainBinding, Digest, TESTNET_GENESIS, TrustedKeys};

use crate::alerts::Rules;

/// The trust file every verifier pins, published byte for byte.
pub const TRUST_FILE: &[u8] = include_bytes!("../../coordinator/testnet-trust.json");
pub const TRUST_SCHEMA: &str = "slop_ninja_testnet_trust_roots_v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Network,
    Netuid,
    Trust,
    Digest(&'static str),
    Number(&'static str),
    Url(&'static str),
    Format,
    /// `ARTIFACT_COPY` is neither `stream` nor `server`.
    Copy,
    /// `DASHBOARD_ROSTER` is not one of the roster levels.
    Roster,
    Prefix,
    /// Artifact publication is enabled without one of its guards.
    Guards(&'static str),
    /// A duration below its derived minimum (or one that overflows).
    Duration(&'static str, u64),
    /// A setting that would publish something the copy model keeps
    /// coordinator-private.
    Private(&'static str),
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Network => f.write_str("NETWORK must be testnet"),
            Self::Netuid => f.write_str("SUBNET_NETUID missing, invalid, or not the trust file's"),
            Self::Trust => f.write_str("the compiled trust file is invalid"),
            Self::Digest(v) => write!(f, "{v} must list 64-hex digests"),
            Self::Number(v) => write!(f, "{v} must be an unsigned integer"),
            Self::Url(v) => write!(f, "{v} must be an https URL without credentials"),
            Self::Format => f.write_str("ALERT_SINK_FORMAT must be json, slack or discord"),
            Self::Copy => f.write_str("ARTIFACT_COPY must be stream or server"),
            Self::Roster => f.write_str("DASHBOARD_ROSTER must be closed, scoring, frozen or sessions"),
            Self::Prefix => f.write_str("RECORD_LABEL_PREFIX must use the cohort label alphabet"),
            Self::Guards(v) => write!(f, "ARTIFACTS_FROM_EPOCH is set but {v}"),
            Self::Duration(v, min) => {
                write!(f, "{v} must be at least {min} and fit in milliseconds")
            }
            Self::Private(v) => write!(
                f,
                "{v} must be false: receipts are coordinator-private and never published"
            ),
        }
    }
}

/// How alert posts are shaped for an https sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkFormat {
    /// The publisher's own JSON document (`slop_ninja.publisher.alert/v2`),
    /// also the alerts relay's post.
    Json,
    /// `{"text": ...}` (Slack-style incoming webhooks).
    Slack,
    /// `{"content": ...}` (Discord-style webhooks).
    Discord,
}

/// Where alert posts go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sink {
    /// An https webhook, its posts shaped by `ALERT_SINK_FORMAT`.
    Url { url: String, format: SinkFormat },
    /// Decision D18: the private alerts relay Worker (email), through the
    /// service binding `ALERTS_RELAY_TESTNET` (`ALERT_RELAY = "true"`). Posts
    /// are the JSON document; each run whose alert phase finished also
    /// posts the relay's dead-man heartbeat. It wins over an
    /// `ALERT_SINK_URL` set at the same time, which is ignored and alerted
    /// (`config_warning`), never a refusal: the configuration also drives
    /// publication.
    Relay,
}

impl Sink {
    /// The body shape of this sink's posts.
    pub fn format(&self) -> SinkFormat {
        match self {
            Self::Url { format, .. } => *format,
            Self::Relay => SinkFormat::Json,
        }
    }
}

/// How publication at scoring start copies an object
/// (`ARTIFACT_COPY`). Neither hashes a byte on our side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyMode {
    /// `"stream"` (the default until a cross-bucket `CopyObject` is
    /// qualified, probe G7-x): a binding `get` from the admission bucket
    /// piped into a binding `put` on the public bucket with the object's
    /// SHA-256, which R2 verifies. The bytes stream inside the runtime and
    /// never enter Wasm memory; no extra credential is needed. The copy is
    /// checked by its stored SHA-256.
    Stream,
    /// `"server"` (configured once probe G7-x qualified a cross-bucket
    /// copy): an S3 `CopyObject` inside R2, presigned by
    /// the internal signer Worker with the separate copy credential (service
    /// binding `R2_SIGNER_TESTNET`, token `COPY_INTERFACE_TOKEN`), with the
    /// source etag pinned, the metadata directive `REPLACE` (no stored
    /// header of the source carries over) and the storage class `STANDARD`.
    /// `CopyObject` keeps the etag but never carries the stored SHA-256
    /// (qualification G7), so the copy is checked by its size and by etag
    /// equality with the admitted source, whose SHA-256 R2 verified at
    /// upload. An object whose length-derived wait does not fit a step, or
    /// whose copy was not confirmed and left nothing, is copied by stream
    /// ([`crate::copy_wait`]).
    Server,
}

impl CopyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stream => "stream",
            Self::Server => "server",
        }
    }
}

/// The settings layout v1 used (closure-time copies from the eval bucket).
/// Retired: a deployment that still sets one is warned (`config_warning`),
/// never refused, since the configuration also drives the row copy.
pub const RETIRED_ARTIFACT_KEYS: [&str; 2] = ["ARTIFACT_SOURCE_KEY", "ADMISSION_RECORD_KEY"];
pub const RETIRED_PUBLISH_IGNORED: &str =
    "PUBLISH_ARTIFACTS is retired and ignored: the tracked ARTIFACTS_FROM_EPOCH enables publication at scoring start";
pub const RETIRED_KEY_IGNORED: &str =
    "ARTIFACT_SOURCE_KEY or ADMISSION_RECORD_KEY is set: retired with closure-time copies, ignored (publication at scoring start uses ARTIFACTS_FROM_EPOCH)";

#[derive(Clone, Debug)]
pub struct Config {
    pub chain: ChainBinding,
    pub keys: TrustedKeys,
    /// Blocks per tempo and seconds per block, for durations in tempos.
    pub tempo_blocks: u64,
    pub block_secs: u64,
    /// The restarted public record holds the cohorts whose manifest opened at
    /// or after this chain epoch (epochs only increase, whatever the
    /// coordinator's cohort numbering). Unset: no cohort is published (the
    /// status snapshot still is).
    pub publish_from_epoch: Option<u64>,
    /// When set, only labels starting with it can be in the record; others
    /// are skipped without a read (rehearsal, load and rotated-root cohorts).
    pub record_label_prefix: Option<String>,
    /// Inference-content digests and artifact SHA-256s that are never
    /// published as entrant artifacts, whatever cohort names them (the
    /// references, which are published as reference packages instead).
    pub private_digests: BTreeSet<Digest>,
    /// Hotkeys whose artifacts are never published: the owner, our own
    /// miners and validators, and synthetic rehearsal identities.
    pub operator_hotkeys: BTreeSet<AccountId>,
    /// Publication at scoring start, on exactly when
    /// [`Config::artifacts_from_epoch`] is set: each admitted entrant's
    /// header and files are copied from the private admission bucket to the
    /// public bucket once its cohort's roster is sealed (`crate::artifacts`).
    /// Off: nothing is copied and closed cohorts list their artifacts as
    /// pending.
    pub publish_artifacts: bool,
    /// The first chain epoch whose cohorts publish their artifacts at
    /// scoring start (`ARTIFACTS_FROM_EPOCH`, tracked in `wrangler.toml` like
    /// the public record's `PUBLISH_FROM_EPOCH`, which it never moves; a
    /// cohort must be inside both). Tracked, so a routine deploy keeps it:
    /// with it set, a deploy without the deploy-time guards
    /// (`PRIVATE_DIGESTS`, `OPERATOR_HOTKEYS`) is refused (`config_refused`
    /// alerts) instead of silently stopping publication, which would keep
    /// admitted objects in the admission bucket with no alert (review round
    /// 2). Empty or unset: publication at scoring start is off.
    pub artifacts_from_epoch: Option<u64>,
    /// How the copy is made (`ARTIFACT_COPY`): `stream` (unset, empty or
    /// `stream`, the tracked default until probe G7-x passes and the copy
    /// token is installed) or `server` (configuration only).
    pub artifact_copy: CopyMode,
    /// How long a server-side copy is waited for, and which objects stream
    /// instead ([`crate::copy_wait`]). Not configurable: always
    /// [`crate::copy_wait::CopyWait::V1`] when parsed.
    pub copy_wait: crate::copy_wait::CopyWait,
    /// The coordinator's admission origin the snapshot advertises.
    pub admission_origin: Option<String>,
    /// Extra operator text for the snapshot's notice.
    pub notice: Option<String>,
    pub rules: Rules,
    pub sink: Option<Sink>,
    /// Settings that were ignored (raised as `config_warning`).
    pub warnings: Vec<String>,
    /// The publisher's own renotify interval, sent in the relay heartbeat.
    pub renotify_ms: u64,
    /// The external dead-man URL; pinged only when enabled.
    pub heartbeat: Option<String>,
    /// Wall time after which a run starts no new step (a cohort
    /// verification or an artifact copy). Default: one cron period.
    pub run_wall_budget_ms: u64,
    /// How much of a running cohort's per-hotkey status the dashboard
    /// roster documents carry (`DASHBOARD_ROSTER`, tracked in
    /// `wrangler.toml`; unset or empty: `closed`). The held-out exposure
    /// review picks the level ([`crate::dashboard::RosterLevel`]).
    pub dashboard_roster: crate::dashboard::RosterLevel,
}

#[derive(Deserialize)]
struct TrustFile {
    schema: String,
    network: String,
    netuid: u16,
    package_public_key: String,
    receipt_public_key: String,
    authorization_public_key: String,
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() != 64 || s.bytes().any(|b| b.is_ascii_uppercase()) {
        return None;
    }
    hex::decode(s).ok()?.try_into().ok()
}

/// The pinned trust roots, and the netuid the file names.
pub fn trust() -> Result<(TrustedKeys, u16), ConfigError> {
    let file: TrustFile = serde_json::from_slice(TRUST_FILE).map_err(|_| ConfigError::Trust)?;
    if file.schema != TRUST_SCHEMA || file.network != "testnet" {
        return Err(ConfigError::Trust);
    }
    let k = |s: &str| hex32(s).ok_or(ConfigError::Trust);
    let keys = TrustedKeys::new(
        k(&file.package_public_key)?,
        k(&file.receipt_public_key)?,
        k(&file.authorization_public_key)?,
    )
    .map_err(|_| ConfigError::Trust)?;
    Ok((keys, file.netuid))
}

fn https(name: &'static str, value: Option<String>) -> Result<Option<String>, ConfigError> {
    match value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => {
            let rest = v.strip_prefix("https://").ok_or(ConfigError::Url(name))?;
            let host = rest.split(['/', '?', '#']).next().unwrap_or("");
            if host.is_empty() || host.contains('@') || v.chars().any(char::is_whitespace) {
                return Err(ConfigError::Url(name));
            }
            Ok(Some(v))
        }
    }
}

fn number(
    get: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    default: u64,
) -> Result<u64, ConfigError> {
    match get(name) {
        None => Ok(default),
        Some(v) => v.trim().parse().map_err(|_| ConfigError::Number(name)),
    }
}

fn flag(
    get: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    default: bool,
) -> Result<bool, ConfigError> {
    match get(name).as_deref().map(str::trim) {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(_) => Err(ConfigError::Number(name)),
    }
}

/// The cron runs once a minute (`wrangler.toml`); the alerts relay derives its
/// dead-man bound from the same constant.
pub const CRON_PERIOD_MS: u64 = slop_ninja_alerts_wire::PUBLISHER_CRON_PERIOD_MS;
const DEFAULT_TEMPO_BLOCKS: u64 = 360;
const DEFAULT_BLOCK_SECS: u64 = 12;
const DEFAULT_SILENCE_SECS: u64 = 300;
const DEFAULT_RENOTIFY_SECS: u64 = slop_ninja_alerts_wire::DEFAULT_RENOTIFY_MS / 1000;

/// The rules at the default durations, for a run whose configuration was
/// refused.
pub fn default_rules() -> Rules {
    Rules::derive(
        DEFAULT_TEMPO_BLOCKS * DEFAULT_BLOCK_SECS * 1000,
        DEFAULT_SILENCE_SECS * 1000,
        DEFAULT_RENOTIFY_SECS * 1000,
    )
}

fn url_sink(get: &impl Fn(&str) -> Option<String>) -> Result<Option<Sink>, ConfigError> {
    Ok(match https("ALERT_SINK_URL", get("ALERT_SINK_URL"))? {
        None => None,
        Some(url) => Some(Sink::Url {
            url,
            format: match get("ALERT_SINK_FORMAT").as_deref().map(str::trim) {
                None | Some("json") => SinkFormat::Json,
                Some("slack") => SinkFormat::Slack,
                Some("discord") => SinkFormat::Discord,
                Some(_) => return Err(ConfigError::Format),
            },
        }),
    })
}

/// The warning for an `ALERT_SINK_URL` set while the relay is on.
pub const SINK_URL_IGNORED: &str =
    "ALERT_SINK_URL is set but ignored while ALERT_RELAY is true; remove the secret";

/// The sink, and a warning when a URL sink is set beside the relay (the
/// relay wins; whether the URL parses does not matter then).
fn sink(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<(Option<Sink>, Option<&'static str>), ConfigError> {
    if flag(get, "ALERT_RELAY", false)? {
        let ignored = get("ALERT_SINK_URL").is_some_and(|v| !v.trim().is_empty());
        return Ok((Some(Sink::Relay), ignored.then_some(SINK_URL_IGNORED)));
    }
    Ok((url_sink(get)?, None))
}

/// A duration in seconds as milliseconds, at least `min_ms`.
fn duration_ms(name: &'static str, secs: u64, min_ms: u64) -> Result<u64, ConfigError> {
    secs.checked_mul(1000)
        .filter(|&ms| ms >= min_ms)
        .ok_or(ConfigError::Duration(name, min_ms / 1000))
}

/// The alert sink alone, when it parses: a refused configuration still
/// alerts through it. The relay wins when it is enabled (it needs no URL to
/// parse).
pub fn sink_only(get: impl Fn(&str) -> Option<String>) -> Option<Sink> {
    if flag(&get, "ALERT_RELAY", false) == Ok(true) {
        return Some(Sink::Relay);
    }
    url_sink(&get).ok().flatten()
}

/// Parses the configuration. `get` returns a var or secret by name.
pub fn parse(get: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
    if get("NETWORK").as_deref() != Some("testnet") {
        return Err(ConfigError::Network);
    }
    let (keys, trust_netuid) = trust()?;
    let netuid: u16 = get("SUBNET_NETUID")
        .and_then(|v| v.trim().parse().ok())
        .ok_or(ConfigError::Netuid)?;
    if netuid != trust_netuid {
        return Err(ConfigError::Netuid);
    }
    let chain = ChainBinding {
        genesis: TESTNET_GENESIS,
        netuid,
        mechanism: 0,
    };
    let mut private_digests = BTreeSet::new();
    for item in get("PRIVATE_DIGESTS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
    {
        if !item.is_empty() {
            private_digests.insert(Digest(
                hex32(item).ok_or(ConfigError::Digest("PRIVATE_DIGESTS"))?,
            ));
        }
    }
    let mut operator_hotkeys = BTreeSet::new();
    for item in get("OPERATOR_HOTKEYS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
    {
        if !item.is_empty() {
            operator_hotkeys.insert(AccountId(
                hex32(item).ok_or(ConfigError::Digest("OPERATOR_HOTKEYS"))?,
            ));
        }
    }
    let artifacts_from_epoch = match get("ARTIFACTS_FROM_EPOCH") {
        Some(v) if !v.trim().is_empty() => Some(
            v.trim()
                .parse()
                .map_err(|_| ConfigError::Number("ARTIFACTS_FROM_EPOCH"))?,
        ),
        _ => None,
    };
    let publish_artifacts = artifacts_from_epoch.is_some();
    // Retired: enablement is the tracked ARTIFACTS_FROM_EPOCH, which a
    // routine deploy cannot drop.
    let retired_publish = get("PUBLISH_ARTIFACTS").is_some();
    let artifact_copy = match get("ARTIFACT_COPY").as_deref().map(str::trim) {
        None | Some("") | Some("stream") => CopyMode::Stream,
        Some("server") => CopyMode::Server,
        Some(_) => return Err(ConfigError::Copy),
    };
    // Fail closed: artifact copies need every guard.
    if publish_artifacts {
        if private_digests.is_empty() {
            return Err(ConfigError::Guards(
                "PRIVATE_DIGESTS lists no reference digest",
            ));
        }
        if operator_hotkeys.is_empty() {
            return Err(ConfigError::Guards("OPERATOR_HOTKEYS lists no hotkey"));
        }
    }
    let publish_from_epoch = match get("PUBLISH_FROM_EPOCH") {
        Some(v) if !v.trim().is_empty() => Some(
            v.trim()
                .parse()
                .map_err(|_| ConfigError::Number("PUBLISH_FROM_EPOCH"))?,
        ),
        _ => None,
    };
    let record_label_prefix = match get("RECORD_LABEL_PREFIX").map(|v| v.trim().to_owned()) {
        Some(p) if !p.is_empty() => {
            if !crate::layout::safe_label(&p) {
                return Err(ConfigError::Prefix);
            }
            Some(p)
        }
        _ => None,
    };
    let tempo_blocks = number(&get, "TEMPO_BLOCKS", DEFAULT_TEMPO_BLOCKS)?;
    let block_secs = number(&get, "BLOCK_SECS", DEFAULT_BLOCK_SECS)?;
    let silence_secs = number(&get, "WATCHDOG_SILENCE_SECS", DEFAULT_SILENCE_SECS)?;
    let tempo_secs = tempo_blocks
        .checked_mul(block_secs)
        .ok_or(ConfigError::Duration("TEMPO_BLOCKS", 0))?;
    // A reminder cannot come more often than the runs do: a shorter
    // interval (0 included) is refused rather than meaning "every run".
    let renotify_ms = duration_ms(
        "ALERT_RENOTIFY_SECS",
        number(&get, "ALERT_RENOTIFY_SECS", DEFAULT_RENOTIFY_SECS)?,
        slop_ninja_alerts_wire::MIN_RENOTIFY_MS,
    )?;
    let rules = Rules::derive(
        duration_ms("TEMPO_BLOCKS", tempo_secs, 0)?,
        duration_ms("WATCHDOG_SILENCE_SECS", silence_secs, 0)?,
        renotify_ms,
    );
    let (sink, sink_warning) = sink(&get)?;
    let retired = RETIRED_ARTIFACT_KEYS
        .iter()
        .any(|k| get(k).is_some_and(|v| !v.trim().is_empty()));
    let heartbeat = if flag(&get, "HEARTBEAT_ENABLED", false)? {
        https("HEARTBEAT_URL", get("HEARTBEAT_URL"))?
    } else {
        None
    };
    let admission_origin = https("ADMISSION_ORIGIN", get("ADMISSION_ORIGIN"))?;
    if admission_origin
        .as_deref()
        .is_some_and(|o| o.trim_start_matches("https://").contains('/'))
    {
        return Err(ConfigError::Url("ADMISSION_ORIGIN"));
    }
    // Receipts, the receipt index, chunk maps and bundles are
    // coordinator-private audit evidence (the copy model, 2026-11-04).
    if flag(&get, "PUBLISH_RECEIPTS", false)? {
        return Err(ConfigError::Private("PUBLISH_RECEIPTS"));
    }
    Ok(Config {
        chain,
        keys,
        tempo_blocks,
        block_secs,
        publish_from_epoch,
        record_label_prefix,
        private_digests,
        operator_hotkeys,
        publish_artifacts,
        artifacts_from_epoch,
        artifact_copy,
        copy_wait: crate::copy_wait::CopyWait::V1,
        admission_origin,
        notice: get("NOTICE")
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty()),
        rules,
        sink,
        warnings: sink_warning
            .into_iter()
            .chain(retired.then_some(RETIRED_KEY_IGNORED))
            .chain(retired_publish.then_some(RETIRED_PUBLISH_IGNORED))
            .map(str::to_owned)
            .collect(),
        renotify_ms,
        heartbeat,
        run_wall_budget_ms: number(&get, "RUN_WALL_BUDGET_MS", CRON_PERIOD_MS)?,
        dashboard_roster: match get("DASHBOARD_ROSTER").as_deref().map(str::trim) {
            None | Some("") => crate::dashboard::RosterLevel::Closed,
            Some(v) => crate::dashboard::RosterLevel::parse(v).ok_or(ConfigError::Roster)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    pub fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn minimal_testnet_config() {
        let c = parse(vars(&[("NETWORK", "testnet"), ("SUBNET_NETUID", "601")])).unwrap();
        assert_eq!(c.chain.netuid, 601);
        assert_eq!(c.publish_from_epoch, None);
        assert_eq!(c.record_label_prefix, None);
        assert_eq!(c.run_wall_budget_ms, CRON_PERIOD_MS);
        assert!(c.sink.is_none() && c.heartbeat.is_none() && !c.publish_artifacts);
        assert_eq!(c.artifact_copy, CopyMode::Stream, "stream until G7-x qualifies a cross-bucket copy");
        assert_eq!(c.rules.fallback_ms, 360 * 12 * 1000);
        assert_eq!(c.dashboard_roster, crate::dashboard::RosterLevel::Closed, "closed cohorts only by default");
    }

    /// The tracked testnet setting is the approved intake level: each
    /// miner's admission state while a cohort runs, no result before close.
    #[test]
    fn the_tracked_roster_level_is_sessions() {
        let toml = include_str!("../wrangler.toml");
        let values: Vec<&str> = toml
            .lines()
            .filter_map(|l| l.trim().strip_prefix("DASHBOARD_ROSTER"))
            .filter_map(|rest| rest.trim().strip_prefix('='))
            .map(|v| v.trim().trim_matches('"'))
            .collect();
        assert_eq!(values, ["sessions"], "one tracked value");
        assert_eq!(crate::dashboard::RosterLevel::parse(values[0]), Some(crate::dashboard::RosterLevel::Sessions));
    }

    #[test]
    fn refusals() {
        assert_eq!(
            parse(vars(&[("NETWORK", "finney"), ("SUBNET_NETUID", "601")])).unwrap_err(),
            ConfigError::Network
        );
        assert_eq!(
            parse(vars(&[("NETWORK", "testnet"), ("SUBNET_NETUID", "602")])).unwrap_err(),
            ConfigError::Netuid
        );
        let base = [("NETWORK", "testnet"), ("SUBNET_NETUID", "601")];
        let with = |k: &'static str, v: &'static str| {
            let mut p = base.to_vec();
            p.push((k, v));
            parse(vars(&p))
        };
        assert!(with("PRIVATE_DIGESTS", "zz").is_err());
        assert!(with("OPERATOR_HOTKEYS", "zz").is_err());
        assert!(with("RECORD_LABEL_PREFIX", "Testnet/").is_err());
        assert!(with("PUBLISH_FROM_EPOCH", "soon").is_err());
        assert_eq!(with("ARTIFACT_COPY", "binding").unwrap_err(), ConfigError::Copy);
        assert!(with("ARTIFACTS_FROM_EPOCH", "yes").is_err());
        assert!(with("ALERT_SINK_URL", "http://example.invalid/hook").is_err());
        assert!(with("ALERT_SINK_URL", "https://user:pw@example.invalid/hook").is_err());
        assert!(with("ADMISSION_ORIGIN", "https://example.invalid/admission").is_err());
        // Receipts are coordinator-private: only "false" (or nothing) is
        // accepted.
        assert!(with("PUBLISH_RECEIPTS", "yes").is_err());
        assert_eq!(
            with("PUBLISH_RECEIPTS", "true").unwrap_err(),
            ConfigError::Private("PUBLISH_RECEIPTS")
        );
        assert!(with("PUBLISH_RECEIPTS", "false").is_ok());
        assert!(with("ALERT_RELAY", "yes").is_err());
        assert_eq!(with("DASHBOARD_ROSTER", "everything").unwrap_err(), ConfigError::Roster);
        assert_eq!(with("DASHBOARD_ROSTER", "Frozen").unwrap_err(), ConfigError::Roster);
        for (v, level) in [
            ("", crate::dashboard::RosterLevel::Closed),
            ("closed", crate::dashboard::RosterLevel::Closed),
            ("scoring", crate::dashboard::RosterLevel::Scoring),
            ("frozen", crate::dashboard::RosterLevel::Frozen),
            ("sessions", crate::dashboard::RosterLevel::Sessions),
        ] {
            assert_eq!(with("DASHBOARD_ROSTER", v).unwrap().dashboard_roster, level, "{v:?}");
        }
    }

    /// The relay sink (decision D18) and the https sinks: one at a time;
    /// the URL sinks keep working; a URL set beside the relay is ignored
    /// with a warning, never a refusal (the configuration also drives
    /// publication).
    #[test]
    fn alert_sinks() {
        let base = [("NETWORK", "testnet"), ("SUBNET_NETUID", "601")];
        let with = |extra: &[(&'static str, &'static str)]| {
            let mut p = base.to_vec();
            p.extend_from_slice(extra);
            p
        };
        let relay = with(&[("ALERT_RELAY", "true")]);
        assert_eq!(parse(vars(&relay)).unwrap().sink, Some(Sink::Relay));
        assert_eq!(Sink::Relay.format(), SinkFormat::Json);
        let url = with(&[
            ("ALERT_SINK_URL", "https://example.invalid/hook"),
            ("ALERT_SINK_FORMAT", "discord"),
        ]);
        assert_eq!(
            parse(vars(&url)).unwrap().sink,
            Some(Sink::Url {
                url: "https://example.invalid/hook".into(),
                format: SinkFormat::Discord
            })
        );
        let off = with(&[("ALERT_RELAY", "false")]);
        assert_eq!(parse(vars(&off)).unwrap().sink, None);
        let both = with(&[
            ("ALERT_RELAY", "true"),
            ("ALERT_SINK_URL", "https://example.invalid/hook"),
        ]);
        let c = parse(vars(&both)).unwrap();
        assert_eq!(c.sink, Some(Sink::Relay));
        assert_eq!(c.warnings, vec![SINK_URL_IGNORED.to_owned()]);
        assert!(parse(vars(&relay)).unwrap().warnings.is_empty());
        // Even a URL that would not parse is only ignored beside the relay.
        let bad = with(&[("ALERT_RELAY", "true"), ("ALERT_SINK_URL", "http://x")]);
        assert_eq!(parse(vars(&bad)).unwrap().warnings.len(), 1);
        assert_eq!(sink_only(vars(&both)), Some(Sink::Relay));
        assert_eq!(sink_only(vars(&url)), parse(vars(&url)).unwrap().sink);
        assert_eq!(
            parse(vars(&with(&[("ALERT_RENOTIFY_SECS", "60")])))
                .unwrap()
                .renotify_ms,
            60_000
        );
    }

    /// The renotify interval is at least one cron period, and no duration
    /// overflows into milliseconds.
    #[test]
    fn durations_are_bounded() {
        let base = [("NETWORK", "testnet"), ("SUBNET_NETUID", "601")];
        let with = |k: &'static str, v: &'static str| {
            let mut p = base.to_vec();
            p.push((k, v));
            parse(vars(&p))
        };
        for v in ["0", "59"] {
            assert_eq!(
                with("ALERT_RENOTIFY_SECS", v).unwrap_err(),
                ConfigError::Duration("ALERT_RENOTIFY_SECS", 60)
            );
        }
        assert!(with("ALERT_RENOTIFY_SECS", "18446744073709551615").is_err());
        assert!(with("WATCHDOG_SILENCE_SECS", "18446744073709551615").is_err());
        assert!(with("TEMPO_BLOCKS", "18446744073709551615").is_err());
        assert_eq!(
            default_rules().renotify_ms,
            slop_ninja_alerts_wire::DEFAULT_RENOTIFY_MS
        );
    }

    /// Publication at scoring start is enabled only with every guard: the
    /// reference digests and the operator hotkeys. The copy mode defaults to
    /// the binding stream; the retired layout-v1 keys only warn.
    #[test]
    fn artifact_publication_fails_closed() {
        let d = "ab".repeat(32);
        let full = [
            ("NETWORK", "testnet"),
            ("SUBNET_NETUID", "601"),
            ("ARTIFACTS_FROM_EPOCH", "80"),
            ("PRIVATE_DIGESTS", d.as_str()),
            ("OPERATOR_HOTKEYS", d.as_str()),
        ];
        let c = parse(vars(&full)).unwrap();
        assert!(c.publish_artifacts && c.artifacts_from_epoch == Some(80));
        assert_eq!(c.artifact_copy, CopyMode::Stream);
        assert_eq!(c.operator_hotkeys.len(), 1);
        for missing in ["PRIVATE_DIGESTS", "OPERATOR_HOTKEYS"] {
            let partial: Vec<_> = full
                .iter()
                .copied()
                .filter(|(k, _)| *k != missing)
                .collect();
            assert!(
                matches!(parse(vars(&partial)), Err(ConfigError::Guards(_))),
                "{missing}"
            );
        }
        let mut server = full.to_vec();
        server.push(("ARTIFACT_COPY", "server"));
        assert_eq!(parse(vars(&server)).unwrap().artifact_copy, CopyMode::Server);
        let mut stream = full.to_vec();
        stream.push(("ARTIFACT_COPY", "stream"));
        assert_eq!(parse(vars(&stream)).unwrap().artifact_copy, CopyMode::Stream);
        // Off, the guards are optional: nothing is copied.
        let none = [("NETWORK", "testnet"), ("SUBNET_NETUID", "601")];
        assert!(!parse(vars(&none)).unwrap().publish_artifacts);
        // A retired layout-v1 key is ignored with a warning, never refused.
        let mut retired = none.to_vec();
        retired.push(("ARTIFACT_SOURCE_KEY", "admission/objects/{sha256}.bin"));
        let c = parse(vars(&retired)).unwrap();
        assert_eq!(c.warnings, vec![RETIRED_KEY_IGNORED.to_owned()]);
        // PUBLISH_ARTIFACTS no longer enables anything: the tracked
        // boundary does, so a routine deploy cannot silently drop it.
        let mut old = none.to_vec();
        old.push(("PUBLISH_ARTIFACTS", "true"));
        let c = parse(vars(&old)).unwrap();
        assert!(!c.publish_artifacts);
        assert_eq!(c.warnings, vec![RETIRED_PUBLISH_IGNORED.to_owned()]);
        let mut empty = none.to_vec();
        empty.push(("ARTIFACTS_FROM_EPOCH", ""));
        assert!(!parse(vars(&empty)).unwrap().publish_artifacts);
        let mut bad = none.to_vec();
        bad.push(("ARTIFACTS_FROM_EPOCH", "soon"));
        assert!(parse(vars(&bad)).is_err());
    }

    #[test]
    fn heartbeat_needs_explicit_enable() {
        let c = parse(vars(&[
            ("NETWORK", "testnet"),
            ("SUBNET_NETUID", "601"),
            ("HEARTBEAT_URL", "https://example.invalid/ping"),
        ]))
        .unwrap();
        assert_eq!(c.heartbeat, None);
        let c = parse(vars(&[
            ("NETWORK", "testnet"),
            ("SUBNET_NETUID", "601"),
            ("HEARTBEAT_URL", "https://example.invalid/ping"),
            ("HEARTBEAT_ENABLED", "true"),
        ]))
        .unwrap();
        assert_eq!(c.heartbeat.as_deref(), Some("https://example.invalid/ping"));
    }
}
