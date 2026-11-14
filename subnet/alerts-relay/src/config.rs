//! Deployment configuration: vars and secrets, parsed once per request through
//! a getter so native tests feed a map. No address appears in tracked
//! configuration: the sender and recipient are deploy-time secrets read from
//! the operator's private files.
//!
//! Only `NETWORK` refuses the configuration. The alert feed is always on;
//! email is optional and off by default: it is on only while the var
//! `ALERT_EMAIL_ENABLED` is `"true"` and the secret `ALERT_EMAIL_TO` is set,
//! so a recipient secret deployed by accident sends nothing (it is named in a
//! warning instead). `ALERT_EMAIL_TO` absent means email off, with no
//! warning, whatever `ALERT_EMAIL_FROM` holds. Every other setting is
//! optional: one that does not parse (an address included) is ignored, its
//! default is used (for an address: email off) and it is named in
//! [`Config::warnings`], which the relay reports as `relay_config_warning`,
//! so a typo in an optional var never stops the relay.
//!
//! While email is on, `ALERT_EMAIL_RULES` (a comma list of rule names) and
//! `ALERT_EMAIL_MIN_INTERVAL_SECS` set its policy ([`crate::email`]); unset,
//! the defaults apply (the critical rules, and the publisher's default
//! renotify interval). A list that names a rule neither the publisher nor
//! the relay raises ([`crate::email::known`]) is kept as given, with a
//! warning, so a misspelt name is noticed and a rule newer than this
//! build still emails.

use crate::email::{DEFAULT_MIN_INTERVAL_MS, Policy};
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
    Name(&'static str),
    List(&'static str),
    /// A recipient is set while email is not enabled.
    NotEnabled(&'static str),
    /// A rule list names a rule neither the publisher nor the relay raises
    /// (the list is kept as given).
    UnknownRule(&'static str),
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
            Self::Name(v) => write!(
                f,
                "{v} must be a name of lowercase letters, digits and ._/-"
            ),
            Self::List(v) => write!(
                f,
                "{v} must be a comma list of names of lowercase letters, digits and ._/-"
            ),
            Self::NotEnabled(v) => write!(
                f,
                "{v} is set while ALERT_EMAIL_ENABLED is not true: email stays off"
            ),
            Self::UnknownRule(v) => write!(
                f,
                "{v} names a rule that neither the publisher nor the relay raises: the list is \
                 kept as given (check the spelling)"
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub network: String,
    /// The email path's sender and recipient: `None` while email is off
    /// (the default: `ALERT_EMAIL_TO` absent). The alert feed is always on.
    pub email: Option<Addresses>,
    /// Spend figures appear in the body of spend alert emails only when the
    /// operator has confirmed that the recipient delivers to the user's
    /// mailbox alone, and only while email is on. Subjects and the alert
    /// feed never carry figures.
    pub spend_figures: bool,
    /// Which events email carries and how often ([`crate::email`]): the
    /// rules of `ALERT_EMAIL_RULES` and the per-subject minimum interval of
    /// `ALERT_EMAIL_MIN_INTERVAL_SECS`, else the defaults. Used only while
    /// email is on.
    pub email_policy: Policy,
    /// The coordinator's watchdog silence, used until a publisher heartbeat
    /// carries the publisher's own watermark bound.
    pub silence_ms: u64,
    /// The tracker's schedule period, used until a tracker upload carries
    /// its own.
    pub tracker_period_ms: u64,
    /// An external dead-man URL the relay's cron pings (a GET) after a tick
    /// that finished with its delivery path working (see `cf.rs`), so a stopped
    /// relay, a stopped cron, an account-wide stop or a failing send path is
    /// caught outside Cloudflare. Pinged only when `HEARTBEAT_ENABLED` is
    /// `"true"`; the URL is a secret.
    pub heartbeat: Option<String>,
    /// A deploy-time drill (`DRILL_ID`): the relay emails one drill message
    /// per id (subject `SlopNinja <network> alert drill`) and never again
    /// for the same id. Unset (the tracked default) sends none.
    pub drill: Option<String>,
    /// The legal stream's sender and recipient ([`crate::legal`]): `None`
    /// (the stream is off, and a legal post answers 503) while the secret
    /// `LEGAL_EMAIL_TO` is absent. The sender is `ALERT_EMAIL_FROM`. It does
    /// not depend on `ALERT_EMAIL_ENABLED`: the legal stream is not an alert.
    pub legal: Option<Addresses>,
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

/// A drill id: a name of the wire contract ([`slop_ninja_alerts_wire::valid_name`]),
/// so it never needs escaping and stays within the contract's string bound.
fn drill_id(value: Option<String>) -> Result<Option<String>, ConfigError> {
    match value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) if slop_ninja_alerts_wire::valid_name(&v) => Ok(Some(v)),
        Some(_) => Err(ConfigError::Name("DRILL_ID")),
    }
}

/// The email list: comma-separated rule names (blanks around a name and
/// empty entries ignored); unset or blank is the default list, and a list
/// with an invalid name, or none, is refused (the default applies, with a
/// warning).
fn email_rules(value: Option<String>) -> Result<Option<Vec<String>>, ConfigError> {
    let Some(v) = value.filter(|v| !v.trim().is_empty()) else {
        return Ok(None);
    };
    let rules: Vec<String> = v
        .split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .collect();
    if rules.is_empty() || !rules.iter().all(|n| slop_ninja_alerts_wire::valid_name(n)) {
        return Err(ConfigError::List("ALERT_EMAIL_RULES"));
    }
    Ok(Some(rules))
}

/// The spend tracker's schedule: it runs once a day.
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

/// The email path's addresses: `None` (email off) while `ALERT_EMAIL_TO` is
/// absent or blank; a recipient without `ALERT_EMAIL_ENABLED = "true"` is
/// email off with a warning; otherwise both must be one plain mailbox, or
/// email stays off and the bad one is named.
fn email(get: &impl Fn(&str) -> Option<String>) -> Result<Option<Addresses>, ConfigError> {
    let Some(to) = get("ALERT_EMAIL_TO").filter(|v| !v.trim().is_empty()) else {
        return Ok(None);
    };
    if !flag(get, "ALERT_EMAIL_ENABLED")? {
        return Err(ConfigError::NotEnabled("ALERT_EMAIL_TO"));
    }
    let from = get("ALERT_EMAIL_FROM").unwrap_or_default();
    if !crate::mime::valid_address(from.trim()) {
        return Err(ConfigError::Address("ALERT_EMAIL_FROM"));
    }
    Addresses::new(&from, &to)
        .map(Some)
        .ok_or(ConfigError::Address("ALERT_EMAIL_TO"))
}

/// The legal stream's addresses: `None` while `LEGAL_EMAIL_TO` is absent or
/// blank; otherwise it and `ALERT_EMAIL_FROM` must each be one plain
/// mailbox, or the stream stays off and the bad one is named.
fn legal(get: &impl Fn(&str) -> Option<String>) -> Result<Option<Addresses>, ConfigError> {
    let Some(to) = get("LEGAL_EMAIL_TO").filter(|v| !v.trim().is_empty()) else {
        return Ok(None);
    };
    if !crate::mime::valid_address(to.trim()) {
        return Err(ConfigError::Address("LEGAL_EMAIL_TO"));
    }
    let from = get("ALERT_EMAIL_FROM").unwrap_or_default();
    Addresses::new(&from, &to)
        .map(Some)
        .ok_or(ConfigError::Address("ALERT_EMAIL_FROM"))
}

/// Parses the configuration. `get` returns a var or secret by name. Only a
/// wrong network refuses it.
pub fn parse(get: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
    if get("NETWORK").as_deref() != Some(NETWORK) {
        return Err(ConfigError::Network);
    }
    let mut warnings = Vec::new();
    let email = optional(&mut warnings, email(&get), None);
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
    let default_policy = Policy::default();
    let listed = optional(&mut warnings, email_rules(get("ALERT_EMAIL_RULES")), None);
    if listed
        .as_ref()
        .is_some_and(|l| !l.iter().all(|r| crate::email::known(r)))
    {
        warnings.push(ConfigError::UnknownRule("ALERT_EMAIL_RULES"));
    }
    let rules = listed.unwrap_or(default_policy.rules);
    let min_interval_ms = optional(
        &mut warnings,
        positive_ms(
            &get,
            "ALERT_EMAIL_MIN_INTERVAL_SECS",
            DEFAULT_MIN_INTERVAL_MS / 1000,
        ),
        DEFAULT_MIN_INTERVAL_MS,
    );
    let email_policy = Policy::new(rules, min_interval_ms);
    let enabled = optional(&mut warnings, flag(&get, "HEARTBEAT_ENABLED"), false);
    let heartbeat = if enabled {
        let url = https("HEARTBEAT_URL", get("HEARTBEAT_URL"))
            .and_then(|u| u.ok_or(ConfigError::Url("HEARTBEAT_URL")));
        optional(&mut warnings, url.map(Some), None)
    } else {
        None
    };
    let drill = optional(&mut warnings, drill_id(get("DRILL_ID")), None);
    let legal = optional(&mut warnings, legal(&get), None);
    Ok(Config {
        network: NETWORK.into(),
        email,
        spend_figures,
        email_policy,
        silence_ms,
        tracker_period_ms,
        heartbeat,
        drill,
        legal,
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

    pub const BASE: [(&str, &str); 4] = [
        ("NETWORK", "testnet"),
        ("ALERT_EMAIL_ENABLED", "true"),
        ("ALERT_EMAIL_FROM", "alerts@example.invalid"),
        ("ALERT_EMAIL_TO", "ops@example.invalid"),
    ];

    #[test]
    fn the_legal_stream_has_its_own_recipient() {
        let with = |extra: &[(&'static str, &'static str)]| {
            let mut p = BASE.to_vec();
            p.retain(|(n, _)| *n != "ALERT_EMAIL_ENABLED" && *n != "ALERT_EMAIL_TO");
            p.extend_from_slice(extra);
            parse(vars(&p)).unwrap()
        };
        // Off without its recipient; on with it, whatever alert email does.
        assert_eq!(with(&[]).legal, None);
        let c = with(&[("LEGAL_EMAIL_TO", "legal-forward@example.invalid")]);
        assert_eq!(c.email, None);
        let a = c.legal.unwrap();
        assert_eq!(
            (a.from.as_str(), a.to.as_str()),
            ("alerts@example.invalid", "legal-forward@example.invalid")
        );
        // A bad address keeps it off and is named, never shown.
        let c = with(&[("LEGAL_EMAIL_TO", "Legal <legal-forward@example.invalid>")]);
        assert_eq!(
            (c.legal, c.warnings),
            (None, vec![ConfigError::Address("LEGAL_EMAIL_TO")])
        );
    }

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
        // Only the network refuses.
        assert_eq!(with("NETWORK", "finney").unwrap_err(), ConfigError::Network);
        assert!(c.email.is_some(), "both addresses set: email on");
        assert!(c.warnings.is_empty());
        // Email is optional: ALERT_EMAIL_TO absent (or blank) is email off,
        // with no warning, whatever ALERT_EMAIL_FROM holds.
        let mut missing = BASE.to_vec();
        missing.retain(|(n, _)| *n != "ALERT_EMAIL_TO");
        let off = parse(vars(&missing)).unwrap();
        assert_eq!((off.email.clone(), off.warnings.clone()), (None, vec![]));
        missing.retain(|(n, _)| *n != "ALERT_EMAIL_FROM");
        let bare = parse(vars(&missing)).unwrap();
        assert_eq!((bare.email, bare.warnings), (None, vec![]));
        assert_eq!(with("ALERT_EMAIL_TO", "  ").unwrap().email, None);
        // A recipient deployed without the enabling var sends nothing, and
        // says so (by name, never the value).
        for enabled in [None, Some("false")] {
            let mut p = BASE.to_vec();
            p.retain(|(n, _)| *n != "ALERT_EMAIL_ENABLED");
            if let Some(v) = enabled {
                p.push(("ALERT_EMAIL_ENABLED", v));
            }
            let c = parse(vars(&p)).unwrap();
            assert_eq!(
                (c.email, c.warnings.clone()),
                (None, vec![ConfigError::NotEnabled("ALERT_EMAIL_TO")])
            );
            assert!(!c.warnings[0].to_string().contains('@'));
        }
        let c = with("ALERT_EMAIL_ENABLED", "yes").unwrap();
        assert_eq!(
            (c.email, c.warnings),
            (None, vec![ConfigError::Flag("ALERT_EMAIL_ENABLED")])
        );
        // Enabled without a recipient: off, no warning.
        missing_to_enabled();
        // A bad address is a warning (email stays off), never a refusal.
        let c = with("ALERT_EMAIL_TO", "a@b.invalid, c@d.invalid").unwrap();
        assert_eq!(
            (c.email, c.warnings),
            (None, vec![ConfigError::Address("ALERT_EMAIL_TO")])
        );
        let c = with("ALERT_EMAIL_FROM", "").unwrap();
        assert_eq!(
            (c.email, c.warnings),
            (None, vec![ConfigError::Address("ALERT_EMAIL_FROM")])
        );
        // Figures without email change nothing (the feed never carries them).
        let mut figures = missing.clone();
        figures.push(("SPEND_FIGURES_IN_EMAIL", "true"));
        let c = parse(vars(&figures)).unwrap();
        assert!(c.spend_figures && c.email.is_none() && c.warnings.is_empty());
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
        // The deploy-time drill: off unless set, and only a contract name.
        assert_eq!(parse(vars(&BASE)).unwrap().drill, None);
        assert_eq!(with("DRILL_ID", " ").unwrap().drill, None);
        assert_eq!(
            with("DRILL_ID", " 20261102-a ").unwrap().drill.as_deref(),
            Some("20261102-a")
        );
        for bad in ["Drill", "a b", "a@b", "x\r\nBcc: y"] {
            let c = with("DRILL_ID", bad).unwrap();
            assert_eq!(
                (c.drill, c.warnings),
                (None, vec![ConfigError::Name("DRILL_ID")]),
                "{bad:?}"
            );
        }
        let long = "d".repeat(slop_ninja_alerts_wire::MAX_STRING_BYTES + 1);
        let mut p = BASE.to_vec();
        p.push(("DRILL_ID", &long));
        assert_eq!(parse(vars(&p)).unwrap().drill, None);
    }

    /// The email policy: the default list and interval unless set; a list
    /// or interval that does not parse is a warning (by name), and the
    /// default applies.
    #[test]
    fn email_policy_settings() {
        let with = |pairs: &[(&'static str, &'static str)]| {
            let mut p = BASE.to_vec();
            p.extend_from_slice(pairs);
            parse(vars(&p)).unwrap()
        };
        let c = parse(vars(&BASE)).unwrap();
        assert_eq!(c.email_policy, Policy::default());
        assert_eq!(c.email_policy.min_interval_ms, 21_600_000);
        assert_eq!(
            with(&[("ALERT_EMAIL_RULES", " ")]).email_policy,
            Policy::default()
        );
        let c = with(&[
            ("ALERT_EMAIL_RULES", " commit_gap, ,spend_alert,commit_gap,"),
            ("ALERT_EMAIL_MIN_INTERVAL_SECS", "3600"),
        ]);
        assert!(c.warnings.is_empty());
        assert_eq!(c.email_policy.rules, ["commit_gap", "spend_alert"]);
        assert_eq!(c.email_policy.min_interval_ms, 3_600_000);
        assert_eq!(c.email_policy.daily_limit(), 48);
        // A name no sender raises (misspelt): kept, and warned about by
        // the setting's name, never the value.
        let c = with(&[("ALERT_EMAIL_RULES", "commit_gap,watermark_stall")]);
        assert_eq!(c.email_policy.rules, ["commit_gap", "watermark_stall"]);
        assert_eq!(
            c.warnings,
            vec![ConfigError::UnknownRule("ALERT_EMAIL_RULES")]
        );
        assert!(!c.warnings[0].to_string().contains("watermark_stall"));
        // Every rule of either sender is known: no warning.
        let every = crate::email::PUBLISHER_RULES
            .iter()
            .chain(crate::email::RELAY_RULES)
            .copied()
            .collect::<Vec<_>>()
            .join(",");
        let c = parse(vars(&[
            BASE[0],
            BASE[1],
            BASE[2],
            BASE[3],
            ("ALERT_EMAIL_RULES", every.as_str()),
        ]))
        .unwrap();
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        for bad in [
            "Commit_gap",
            "commit_gap spend_alert",
            ",,",
            "a@b",
            "x\r\nBcc: y",
        ] {
            let c = with(&[("ALERT_EMAIL_RULES", bad)]);
            assert_eq!(c.email_policy, Policy::default(), "{bad:?}");
            assert_eq!(
                c.warnings,
                vec![ConfigError::List("ALERT_EMAIL_RULES")],
                "{bad:?}"
            );
            assert!(!c.warnings[0].to_string().contains("Commit"));
        }
        for bad in ["0", "-1", "1h", "18446744073709551615"] {
            let c = with(&[("ALERT_EMAIL_MIN_INTERVAL_SECS", bad)]);
            assert_eq!(c.email_policy.min_interval_ms, 21_600_000, "{bad:?}");
            assert_eq!(
                c.warnings,
                vec![ConfigError::Number("ALERT_EMAIL_MIN_INTERVAL_SECS")],
                "{bad:?}"
            );
        }
        // Email off: the policy still parses (and still warns), unused.
        let c = parse(vars(&[
            ("NETWORK", "testnet"),
            ("ALERT_EMAIL_RULES", "cohort_fault"),
        ]))
        .unwrap();
        assert_eq!((c.email.is_none(), c.email_policy.rules.len()), (true, 1));
    }

    fn missing_to_enabled() {
        let c = parse(vars(&[
            ("NETWORK", "testnet"),
            ("ALERT_EMAIL_ENABLED", "true"),
        ]))
        .unwrap();
        assert_eq!((c.email, c.warnings), (None, vec![]));
    }
}
