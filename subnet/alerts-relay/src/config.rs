//! Deployment configuration: vars and secrets, parsed once per request through
//! a getter so native tests feed a map. No address appears in tracked
//! configuration: the sender and recipient are deploy-time secrets read from
//! the operator's private files.
//!
//! Only the settings without which the relay cannot email refuse the
//! configuration: `NETWORK`, `ALERT_EMAIL_FROM` and `ALERT_EMAIL_TO`. Every
//! other setting is optional: one that does not parse is ignored (its
//! default is used) and named in [`Config::warnings`], which the relay
//! emails as `relay_config_warning`, so a typo in an optional var never
//! stops the relay.

use crate::mime::Addresses;
use crate::rules::Bounds;

/// The one network the relay serves.
pub const NETWORK: &str = "testnet";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Network,
    Address(&'static str),
    Number(&'static str),
    Flag(&'static str),
    Url(&'static str),
}

/// Names the setting and what it must be, never its value.
impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Network => f.write_str("NETWORK must be testnet"),
            Self::Address(v) => write!(f, "{v} must be one plain mailbox (local@domain)"),
            Self::Number(v) => write!(f, "{v} must be a positive integer"),
            Self::Flag(v) => write!(f, "{v} must be true or false"),
            Self::Url(v) => write!(f, "{v} must be an https URL without credentials"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub network: String,
    pub addresses: Addresses,
    /// Spend figures appear in the body of spend alerts only when the
    /// operator has confirmed that the recipient delivers to the user's
    /// mailbox alone. Subjects never carry figures.
    pub spend_figures: bool,
    /// The coordinator's watchdog silence, used until a publisher heartbeat
    /// carries the publisher's own watermark bound.
    pub silence_ms: u64,
    /// The tracker's schedule period, used until a tracker upload carries
    /// its own.
    pub tracker_period_ms: u64,
    /// An external dead-man URL the relay's cron pings (a GET) after a tick
    /// that finished with its email path working (see `cf.rs`), so a stopped
    /// relay, a stopped cron, an account-wide stop or a failing send path is
    /// caught outside Cloudflare. Pinged only when `HEARTBEAT_ENABLED` is
    /// `"true"`; the URL is a secret.
    pub heartbeat: Option<String>,
    /// Optional settings that did not parse and were ignored (their default
    /// is in force). Names and rules only, never values.
    pub warnings: Vec<ConfigError>,
}

impl Config {
    /// The rule bounds for this tick: the publisher's watermark bound from
    /// its last heartbeat and the tracker's period from its last upload,
    /// else the configured fallbacks.
    pub fn bounds(
        &self,
        watermark_stall_ms: Option<u64>,
        tracker_period_ms: Option<u64>,
    ) -> Bounds {
        Bounds::derive(
            watermark_stall_ms
                .unwrap_or(slop_ninja_alerts_wire::watermark_stall_ms(self.silence_ms)),
            tracker_period_ms.unwrap_or(self.tracker_period_ms),
        )
    }
}

/// A positive number of seconds, in milliseconds; `default` when unset.
fn positive_ms(
    get: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    default_secs: u64,
) -> Result<u64, ConfigError> {
    let secs = match get(name) {
        None => default_secs,
        Some(v) => v
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|&n| n > 0)
            .ok_or(ConfigError::Number(name))?,
    };
    secs.checked_mul(1000).ok_or(ConfigError::Number(name))
}

fn flag(get: &impl Fn(&str) -> Option<String>, name: &'static str) -> Result<bool, ConfigError> {
    match get(name).as_deref().map(str::trim) {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(ConfigError::Flag(name)),
    }
}

/// An https URL without credentials or whitespace.
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

/// The spend tracker's schedule: its systemd timer runs once a day.
pub const DEFAULT_TRACKER_PERIOD_SECS: u64 = 86_400;
/// The coordinator's watchdog silence, as the publisher's
/// `WATCHDOG_SILENCE_SECS` defaults it.
pub const DEFAULT_SILENCE_SECS: u64 = 300;

/// An optional setting's value, or its default with a warning.
fn optional<T>(warnings: &mut Vec<ConfigError>, value: Result<T, ConfigError>, default: T) -> T {
    value.unwrap_or_else(|e| {
        warnings.push(e);
        default
    })
}

/// Parses the configuration. `get` returns a var or secret by name. Only a
/// wrong network or a missing or invalid address refuses it.
pub fn parse(get: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
    if get("NETWORK").as_deref() != Some(NETWORK) {
        return Err(ConfigError::Network);
    }
    let from = get("ALERT_EMAIL_FROM").unwrap_or_default();
    let to = get("ALERT_EMAIL_TO").unwrap_or_default();
    if !crate::mime::valid_address(from.trim()) {
        return Err(ConfigError::Address("ALERT_EMAIL_FROM"));
    }
    let addresses = Addresses::new(&from, &to).ok_or(ConfigError::Address("ALERT_EMAIL_TO"))?;
    let mut warnings = Vec::new();
    let silence_ms = optional(
        &mut warnings,
        positive_ms(&get, "WATCHDOG_SILENCE_SECS", DEFAULT_SILENCE_SECS),
        DEFAULT_SILENCE_SECS * 1000,
    );
    let tracker_period_ms = optional(
        &mut warnings,
        positive_ms(&get, "TRACKER_PERIOD_SECS", DEFAULT_TRACKER_PERIOD_SECS),
        DEFAULT_TRACKER_PERIOD_SECS * 1000,
    );
    let spend_figures = optional(&mut warnings, flag(&get, "SPEND_FIGURES_IN_EMAIL"), false);
    let enabled = optional(&mut warnings, flag(&get, "HEARTBEAT_ENABLED"), false);
    let heartbeat = if enabled {
        let url = https("HEARTBEAT_URL", get("HEARTBEAT_URL"))
            .and_then(|u| u.ok_or(ConfigError::Url("HEARTBEAT_URL")));
        optional(&mut warnings, url.map(Some), None)
    } else {
        None
    };
    Ok(Config {
        network: NETWORK.into(),
        addresses,
        spend_figures,
        silence_ms,
        tracker_period_ms,
        heartbeat,
        warnings,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;

    pub fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        move |k| map.get(k).cloned()
    }

    pub const BASE: [(&str, &str); 3] = [
        ("NETWORK", "testnet"),
        ("ALERT_EMAIL_FROM", "alerts@example.invalid"),
        ("ALERT_EMAIL_TO", "ops@example.invalid"),
    ];

    #[test]
    fn parses_and_refuses() {
        let c = parse(vars(&BASE)).unwrap();
        assert!(!c.spend_figures);
        assert!(c.warnings.is_empty());
        assert_eq!(c.bounds(None, None).tracker_stale_ms, 2 * 86_400_000);
        assert_eq!(c.bounds(None, None).watermark_stall_ms, 420_000);
        // A heartbeat's and an upload's own values win over the fallbacks.
        assert_eq!(c.bounds(Some(9), Some(5)).watermark_stall_ms, 9);
        assert_eq!(c.bounds(Some(9), Some(5)).tracker_stale_ms, 10);
        let with = |k: &'static str, v: &'static str| {
            let mut p = BASE.to_vec();
            p.retain(|(n, _)| *n != k);
            p.push((k, v));
            parse(vars(&p))
        };
        // Required settings refuse.
        assert_eq!(with("NETWORK", "finney").unwrap_err(), ConfigError::Network);
        assert_eq!(
            with("ALERT_EMAIL_TO", "a@b.invalid, c@d.invalid").unwrap_err(),
            ConfigError::Address("ALERT_EMAIL_TO")
        );
        assert_eq!(
            with("ALERT_EMAIL_FROM", "").unwrap_err(),
            ConfigError::Address("ALERT_EMAIL_FROM")
        );
        let mut missing = BASE.to_vec();
        missing.retain(|(n, _)| *n != "ALERT_EMAIL_TO");
        assert!(parse(vars(&missing)).is_err());
        // Optional ones parse, or fall back to their default with a warning.
        assert!(
            with("SPEND_FIGURES_IN_EMAIL", "true")
                .unwrap()
                .spend_figures
        );
        let c = with("SPEND_FIGURES_IN_EMAIL", "yes").unwrap();
        assert!(!c.spend_figures);
        assert_eq!(
            c.warnings,
            vec![ConfigError::Flag("SPEND_FIGURES_IN_EMAIL")]
        );
        let c = with("TRACKER_PERIOD_SECS", "0").unwrap();
        assert_eq!(c.tracker_period_ms, 86_400_000);
        assert_eq!(c.warnings, vec![ConfigError::Number("TRACKER_PERIOD_SECS")]);
        assert_eq!(
            with("TRACKER_PERIOD_SECS", "3600")
                .unwrap()
                .bounds(None, None)
                .tracker_stale_ms,
            7_200_000
        );
        assert_eq!(
            with("TRACKER_PERIOD_SECS", "18446744073709551615")
                .unwrap()
                .warnings,
            vec![ConfigError::Number("TRACKER_PERIOD_SECS")]
        );
        assert_eq!(
            with("WATCHDOG_SILENCE_SECS", "5m").unwrap().silence_ms,
            300_000
        );
        // The external dead-man ping: only when enabled, and only https.
        assert_eq!(parse(vars(&BASE)).unwrap().heartbeat, None);
        assert_eq!(
            with("HEARTBEAT_URL", "https://example.invalid/ping")
                .unwrap()
                .heartbeat,
            None
        );
        let mut on = BASE.to_vec();
        on.push(("HEARTBEAT_ENABLED", "true"));
        let c = parse(vars(&on)).unwrap();
        assert_eq!(
            (c.heartbeat, c.warnings),
            (None, vec![ConfigError::Url("HEARTBEAT_URL")])
        );
        on.push(("HEARTBEAT_URL", "https://example.invalid/ping"));
        let c = parse(vars(&on)).unwrap();
        assert_eq!(c.heartbeat.as_deref(), Some("https://example.invalid/ping"));
        assert!(c.warnings.is_empty());
        on.pop();
        on.push(("HEARTBEAT_URL", "http://example.invalid/ping"));
        let c = parse(vars(&on)).unwrap();
        assert_eq!(
            (c.heartbeat, c.warnings),
            (None, vec![ConfigError::Url("HEARTBEAT_URL")])
        );
        assert_eq!(
            with("HEARTBEAT_ENABLED", "1").unwrap().warnings,
            vec![ConfigError::Flag("HEARTBEAT_ENABLED")]
        );
        // A warning names the setting, never its value.
        let c = with("SPEND_FIGURES_IN_EMAIL", "secret-looking-value").unwrap();
        assert!(!c.warnings[0].to_string().contains("secret"));
    }
}
