//! Who may read the detailed status routes (`GET /status`, `GET
//! /eval/status`, `GET /eval/load/status`, `GET /eval/admission/link` and
//! `GET /eval/authority/<name>/status`; [`status_route`] maps a request to
//! one).
//!
//! Those documents are content-free, but they are a real-time timing and
//! aggregate feed (per-job call and CPU times, chunk-attempt histograms,
//! per-kind document counts, the draw summary, spend), so they are served
//! only to:
//!
//! - the operator, with `COORDINATOR_OPERATOR_TOKEN`, from any host (the
//!   operator's own calls reach the `workers.dev` name);
//! - the operator's unattended read-only jobs (deploy idle and schema
//!   checks, the spend tracker, watchers), with `STATUS_TOKEN_MONITOR`, from
//!   any host: every status route, and nothing else. The gate guards only
//!   these GET routes; every route that changes state checks the operator
//!   token alone, so the monitor token never installs, revokes, restarts,
//!   abandons or clears anything. It is the one token besides the
//!   operator's that opens the feed from a public host, held only on the
//!   operator's host, never by a Worker;
//! - the publisher and the alerts relay through their service bindings: a
//!   request addressed to [`STATUS_INTERNAL_HOST`], which no public route of
//!   either Worker carries, presenting that consumer's own token
//!   (`STATUS_TOKEN_PUBLISHER`, `STATUS_TOKEN_ALERTS`). A consumer token
//!   presented on a public host is refused. That rests on Cloudflare never
//!   delivering a public request whose URL names that host (TESTNET.md,
//!   "Status token split", probes it after each gate deploy).
//!
//! Each consumer reads only the routes it needs: the publisher the Workers'
//! `/status` and `/eval/status`, the alerts relay only `/status`, and its
//! `/status` is the liveness subset its rules read ([`alerts_journal`]: no
//! cohort status, no arena or liveness record); the load, audit and link
//! reads are the operator's and the monitor's. `COORDINATOR_STATUS_TOKEN`,
//! the one token every consumer shared before, is still accepted while it is
//! installed, only on the internal host and with the publisher's routes, so
//! the consumers can move to their own tokens without a gap; `/status`
//! reports it under `access` until the operator deletes it.
//!
//! `/hook` (the rtao-rpc webhook, authenticated by its signature) and the
//! upload Worker's internal admission interface are unaffected.

use crate::config::{self, bearer_matches};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq;

/// The host the publisher and the alerts relay address their status reads
/// to (`https://status.internal/...`) through their service bindings. No
/// public route of the coordinator or the eval Worker carries it: a public
/// request arrives on the `workers.dev` name.
pub const STATUS_INTERNAL_HOST: &str = "status.internal";

/// The publisher's publication report ([`StatusRoute::Publication`], POST).
pub const PUBLICATION_PATH: &str = "/eval/admission/published";

/// The operator's token: every operator route, and every status route from
/// any host.
pub const OPERATOR_TOKEN: &str = "COORDINATOR_OPERATOR_TOKEN";
/// The operator's read-only monitor token: every status route from any
/// host, never a route that changes state.
pub const MONITOR_TOKEN: &str = "STATUS_TOKEN_MONITOR";
/// The publisher's status token (service binding only).
pub const PUBLISHER_TOKEN: &str = "STATUS_TOKEN_PUBLISHER";
/// The alerts relay's status token (service binding only).
pub const ALERTS_TOKEN: &str = "STATUS_TOKEN_ALERTS";
/// The token the publisher and the alerts relay shared before each had its
/// own: accepted only on the internal host, with the publisher's routes,
/// until the operator deletes the secret.
pub const SHARED_TOKEN: &str = "COORDINATOR_STATUS_TOKEN";

/// A detailed status route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusRoute {
    /// `GET /status` (either Worker).
    Worker,
    /// `GET /eval/status`.
    Eval,
    /// `GET /eval/load/status`, `GET /eval/admission/link` and `GET
    /// /eval/authority/<name>/status`: operator reads.
    Operator,
    /// `POST /eval/admission/published`: the publisher's report of a
    /// cohort's publication at scoring start (which admitted entrants are
    /// published or withheld for good, and which objects it found missing).
    /// The one route outside the status reads the gate guards: the operator
    /// and the publisher (internal host, its own token) only, never the
    /// monitor token, so the monitor still changes nothing. Its effect is
    /// bounded by what the publisher already decides: a recorded publication
    /// ends an admitted entrant's object retention only once its cohort is
    /// closed, and a reported loss only starts an audit.
    Publication,
}

/// Who presented an accepted token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Caller {
    Operator,
    /// The operator's read-only jobs ([`MONITOR_TOKEN`]).
    Monitor,
    Publisher,
    Alerts,
    /// The shared pre-split token ([`SHARED_TOKEN`]).
    Shared,
}

impl Caller {
    /// Whether this caller may read `route`.
    pub const fn may_read(self, route: StatusRoute) -> bool {
        match self {
            Self::Operator => true,
            Self::Monitor => !matches!(route, StatusRoute::Publication),
            Self::Publisher => matches!(route, StatusRoute::Worker | StatusRoute::Eval | StatusRoute::Publication),
            Self::Shared => matches!(route, StatusRoute::Worker | StatusRoute::Eval),
            Self::Alerts => matches!(route, StatusRoute::Worker),
        }
    }

    /// Whether this caller's `/status` carries the full document: the
    /// ChainJournal status as it is, the main cohort's status
    /// (`eval_cohort`, the same document `/eval/status` serves without the
    /// limiter and the ledger), the liveness record and the arena
    /// counters. The alerts relay's carries only [`alerts_journal`].
    pub const fn reads_detail(self) -> bool {
        !matches!(self, Self::Alerts)
    }
}

/// The status route a request reads, or `None` for every other route (each
/// of which checks the operator token alone). `path` is the request path,
/// exact: no trailing slash or other spelling maps to a status route.
pub fn status_route(get: bool, path: &str) -> Option<StatusRoute> {
    if !get {
        return (path == PUBLICATION_PATH).then_some(StatusRoute::Publication);
    }
    match path {
        "/status" => Some(StatusRoute::Worker),
        "/eval/status" => Some(StatusRoute::Eval),
        "/eval/load/status" | "/eval/admission/link" => Some(StatusRoute::Operator),
        _ if matches!(config::authority_route(path), Some((_, "status"))) => Some(StatusRoute::Operator),
        _ => None,
    }
}

/// The alerts relay's view of a ChainJournal `/status`: only what its
/// `coordinator_unwatched` rule reads (the confirmed watermark's age, the
/// durable fault's kind and the signing state), each kept only with its
/// expected type.
pub fn alerts_journal(journal: &serde_json::Value) -> serde_json::Value {
    use serde_json::{Map, Value};
    let mut j = Map::new();
    if let Some(age) = journal.pointer("/journal/confirmed_age_ms").and_then(Value::as_u64) {
        j.insert("confirmed_age_ms".into(), age.into());
    }
    if let Some(fault) = journal.pointer("/journal/fault").filter(|f| f.is_object()) {
        let kind = fault.get("kind").and_then(Value::as_str).map(Value::from);
        j.insert("fault".into(), serde_json::json!({ "kind": kind }));
    }
    let mut out = Map::new();
    if journal.get("journal").is_some_and(Value::is_object) {
        out.insert("journal".into(), Value::Object(j));
    }
    if let Some(signing) = journal.get("signing").filter(|s| s.is_object()) {
        let state = signing.get("state").and_then(Value::as_str).map(Value::from);
        out.insert("signing".into(), serde_json::json!({ "state": state }));
    }
    Value::Object(out)
}

/// The installed token secrets. A secret shorter than
/// [`config::MIN_TOKEN_BYTES`] counts as not installed.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tokens<'a> {
    pub operator: Option<&'a str>,
    pub monitor: Option<&'a str>,
    pub publisher: Option<&'a str>,
    pub alerts: Option<&'a str>,
    pub shared: Option<&'a str>,
}

/// The decision for one status request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Granted(Caller),
    /// No accepted token for this request (401).
    Unauthorized,
    /// No token that could grant this request is installed (503).
    NotConfigured,
}

fn installed(secret: Option<&str>) -> Option<&str> {
    secret.filter(|s| config::token(s).is_ok())
}

impl<'a> Tokens<'a> {
    /// Drops secrets that are too short to accept.
    pub fn new(
        operator: Option<&'a str>,
        monitor: Option<&'a str>,
        publisher: Option<&'a str>,
        alerts: Option<&'a str>,
        shared: Option<&'a str>,
    ) -> Self {
        Self {
            operator: installed(operator),
            monitor: installed(monitor),
            publisher: installed(publisher),
            alerts: installed(alerts),
            shared: installed(shared),
        }
    }

    /// The candidates for a request, operator first: an internal request
    /// may present any token; a public one only the operator's or the
    /// monitor's.
    fn candidates(&self, internal: bool) -> [(Caller, Option<&'a str>); 5] {
        let consumer = |s: Option<&'a str>| if internal { s } else { None };
        [
            (Caller::Operator, self.operator),
            (Caller::Monitor, self.monitor),
            (Caller::Publisher, consumer(self.publisher)),
            (Caller::Alerts, consumer(self.alerts)),
            (Caller::Shared, consumer(self.shared)),
        ]
    }

    /// Whether every installed token differs from every other. Equal
    /// tokens cannot tell their consumers apart (an equal operator token
    /// would give a consumer the operator's routes).
    pub fn distinct(&self) -> bool {
        let all = [self.operator, self.monitor, self.publisher, self.alerts, self.shared];
        let digests: Vec<_> = all.iter().flatten().map(|s| Sha256::digest(s.as_bytes())).collect();
        digests
            .iter()
            .enumerate()
            .all(|(i, a)| digests.iter().skip(i + 1).all(|b| !bool::from(a.ct_eq(b))))
    }

    /// The content-free account of which tokens are installed, for `/status`.
    pub fn report(&self, caller: Caller) -> Report {
        Report {
            caller,
            operator_token: self.operator.is_some(),
            monitor_token: self.monitor.is_some(),
            publisher_token: self.publisher.is_some(),
            alerts_token: self.alerts.is_some(),
            shared_status_token: self.shared.is_some(),
            tokens_distinct: self.distinct(),
            internal_host: STATUS_INTERNAL_HOST,
        }
    }
}

/// Decides one status request. `internal` is whether it was addressed to
/// [`STATUS_INTERNAL_HOST`]; `header` is its `Authorization` header. Every
/// candidate is compared (constant time each), so the time taken does not
/// depend on which token matched.
pub fn decide(tokens: &Tokens<'_>, internal: bool, header: Option<&str>, route: StatusRoute) -> Access {
    let candidates = tokens.candidates(internal);
    if candidates.iter().all(|(_, s)| s.is_none()) {
        return Access::NotConfigured;
    }
    let mut granted = None;
    for (caller, secret) in candidates {
        let matched = secret.is_some_and(|s| bearer_matches(header, s));
        if matched && granted.is_none() {
            granted = Some(caller);
        }
    }
    match granted {
        Some(caller) if caller.may_read(route) => Access::Granted(caller),
        _ => Access::Unauthorized,
    }
}

/// Whether a request URL's host is the internal status host.
pub fn is_internal_host(host: Option<&str>) -> bool {
    host == Some(STATUS_INTERNAL_HOST)
}

/// `/status` `access`: which caller read it and which tokens are installed
/// (never their values).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    pub caller: Caller,
    pub operator_token: bool,
    pub monitor_token: bool,
    pub publisher_token: bool,
    pub alerts_token: bool,
    /// The shared pre-split token is still installed: delete it once the
    /// publisher and the alerts relay present their own.
    pub shared_status_token: bool,
    pub tokens_distinct: bool,
    pub internal_host: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(c: char) -> String {
        c.to_string().repeat(40)
    }

    fn bearer(s: &str) -> String {
        format!("Bearer {s}")
    }

    const ROUTES: [StatusRoute; 3] = [StatusRoute::Worker, StatusRoute::Eval, StatusRoute::Operator];

    #[test]
    fn the_operator_and_the_monitor_read_every_status_route_from_any_host() {
        let (op, mon, publ, al) = (secret('o'), secret('m'), secret('p'), secret('a'));
        let t = Tokens::new(Some(&op), Some(&mon), Some(&publ), Some(&al), None);
        for internal in [false, true] {
            for route in ROUTES {
                assert_eq!(decide(&t, internal, Some(&bearer(&op)), route), Access::Granted(Caller::Operator));
                assert_eq!(decide(&t, internal, Some(&bearer(&mon)), route), Access::Granted(Caller::Monitor));
            }
        }
        // The monitor alone is enough configuration for a public read.
        let t = Tokens::new(None, Some(&mon), None, None, None);
        assert_eq!(decide(&t, false, Some(&bearer(&mon)), StatusRoute::Operator), Access::Granted(Caller::Monitor));
        assert_eq!(decide(&t, false, Some(&bearer(&op)), StatusRoute::Worker), Access::Unauthorized);
    }

    #[test]
    fn consumer_tokens_open_nothing_on_a_public_host() {
        let (op, mon, publ, al, shared) = (secret('o'), secret('m'), secret('p'), secret('a'), secret('s'));
        let t = Tokens::new(Some(&op), Some(&mon), Some(&publ), Some(&al), Some(&shared));
        for token in [&publ, &al, &shared] {
            for route in ROUTES {
                assert_eq!(decide(&t, false, Some(&bearer(token)), route), Access::Unauthorized);
            }
        }
        assert_eq!(decide(&t, false, None, StatusRoute::Worker), Access::Unauthorized);
    }

    #[test]
    fn each_consumer_reads_only_its_routes_through_its_binding() {
        let (op, publ, al, shared) = (secret('o'), secret('p'), secret('a'), secret('s'));
        let t = Tokens::new(Some(&op), None, Some(&publ), Some(&al), Some(&shared));
        let d = |token: &str, route| decide(&t, true, Some(&bearer(token)), route);
        assert_eq!(d(&publ, StatusRoute::Worker), Access::Granted(Caller::Publisher));
        assert_eq!(d(&publ, StatusRoute::Eval), Access::Granted(Caller::Publisher));
        assert_eq!(d(&publ, StatusRoute::Operator), Access::Unauthorized);
        assert_eq!(d(&al, StatusRoute::Worker), Access::Granted(Caller::Alerts));
        assert_eq!(d(&al, StatusRoute::Eval), Access::Unauthorized);
        assert_eq!(d(&al, StatusRoute::Operator), Access::Unauthorized);
        // The shared pre-split token keeps the publisher's routes while it is installed.
        assert_eq!(d(&shared, StatusRoute::Worker), Access::Granted(Caller::Shared));
        assert_eq!(d(&shared, StatusRoute::Eval), Access::Granted(Caller::Shared));
        assert_eq!(d(&shared, StatusRoute::Operator), Access::Unauthorized);
        assert_eq!(d(&secret('x'), StatusRoute::Worker), Access::Unauthorized);
        assert_eq!(decide(&t, true, None, StatusRoute::Worker), Access::Unauthorized);
    }

    #[test]
    fn only_the_alerts_relay_reads_the_reduced_status() {
        for c in [Caller::Operator, Caller::Monitor, Caller::Publisher, Caller::Shared] {
            assert!(c.reads_detail(), "{c:?}");
        }
        assert!(!Caller::Alerts.reads_detail());
    }

    #[test]
    fn missing_or_short_secrets_are_not_installed() {
        let op = secret('o');
        let short = "p".repeat(config::MIN_TOKEN_BYTES - 1);
        // Only consumer tokens: a public request has nothing that could grant it.
        let publ = secret('p');
        let t = Tokens::new(None, None, Some(&publ), None, None);
        assert_eq!(decide(&t, false, Some(&bearer(&op)), StatusRoute::Worker), Access::NotConfigured);
        assert_eq!(decide(&t, true, Some(&bearer(&publ)), StatusRoute::Worker), Access::Granted(Caller::Publisher));
        let t = Tokens::new(Some(&op), Some(&short), Some(&short), None, None);
        assert!(t.publisher.is_none() && t.monitor.is_none());
        assert_eq!(decide(&t, true, Some(&bearer(&short)), StatusRoute::Worker), Access::Unauthorized);
        assert_eq!(decide(&Tokens::default(), true, Some(&bearer(&op)), StatusRoute::Worker), Access::NotConfigured);
    }

    #[test]
    fn equal_tokens_are_reported_and_resolve_to_the_operator() {
        let (op, mon, publ) = (secret('o'), secret('m'), secret('p'));
        assert!(Tokens::new(Some(&op), Some(&mon), Some(&publ), Some(&secret('a')), Some(&secret('s'))).distinct());
        assert!(Tokens::new(Some(&op), None, None, None, None).distinct());
        let same = Tokens::new(Some(&op), None, Some(&op), None, None);
        assert!(!same.distinct());
        assert!(!Tokens::new(Some(&op), None, Some(&publ), Some(&publ), None).distinct());
        assert!(!Tokens::new(None, None, Some(&publ), None, Some(&publ)).distinct());
        assert!(!Tokens::new(Some(&op), Some(&op), None, None, None).distinct());
        assert!(!Tokens::new(None, Some(&mon), Some(&mon), None, None).distinct());
        assert_eq!(decide(&same, true, Some(&bearer(&op)), StatusRoute::Worker), Access::Granted(Caller::Operator));
        let r = same.report(Caller::Operator);
        assert!(!r.tokens_distinct && r.operator_token && r.publisher_token && !r.alerts_token && !r.monitor_token);
    }

    #[test]
    fn the_report_names_no_secret() {
        let (op, mon, publ, shared) = (secret('o'), secret('m'), secret('p'), secret('s'));
        let t = Tokens::new(Some(&op), Some(&mon), Some(&publ), None, Some(&shared));
        let v = serde_json::to_string(&t.report(Caller::Publisher)).unwrap();
        assert!(!v.contains(&op) && !v.contains(&publ) && !v.contains(&mon));
        assert_eq!(
            v,
            r#"{"caller":"publisher","operator_token":true,"monitor_token":true,"publisher_token":true,"alerts_token":false,"shared_status_token":true,"tokens_distinct":true,"internal_host":"status.internal"}"#
        );
    }

    #[test]
    fn only_the_internal_host_is_internal() {
        assert!(is_internal_host(Some("status.internal")));
        for h in [None, Some("status.internal.example"), Some("STATUS.INTERNAL"), Some("x.workers.dev"), Some("")] {
            assert!(!is_internal_host(h), "{h:?}");
        }
    }

    #[test]
    fn status_routes_are_exactly_the_documented_reads() {
        assert_eq!(status_route(true, "/status"), Some(StatusRoute::Worker));
        assert_eq!(status_route(true, "/eval/status"), Some(StatusRoute::Eval));
        for p in ["/eval/load/status", "/eval/admission/link", "/eval/authority/main/status"] {
            assert_eq!(status_route(true, p), Some(StatusRoute::Operator), "{p}");
        }
        // Writes and every other spelling check the operator token alone.
        for p in [
            "/status",
            "/eval/status",
            "/eval/load/status",
            "/eval/admission/link",
            "/eval/authority/main/status",
            "/eval/authority/main/clear",
            "/eval/authority/main/abandon",
            "/eval/plan",
            "/eval/abandon",
            "/fault/clear",
            "/signing/target",
            "/signing/control",
            "/signing/revoke",
        ] {
            assert_eq!(status_route(false, p), None, "POST {p}");
        }
        // The publisher's publication report: the one POST the gate guards,
        // open to the operator and the publisher, never the monitor.
        assert_eq!(status_route(false, PUBLICATION_PATH), Some(StatusRoute::Publication));
        assert_eq!(status_route(true, PUBLICATION_PATH), None);
        assert_eq!(status_route(false, "/eval/admission/published/"), None);
        assert!(Caller::Operator.may_read(StatusRoute::Publication));
        assert!(Caller::Publisher.may_read(StatusRoute::Publication));
        for c in [Caller::Monitor, Caller::Alerts, Caller::Shared] {
            assert!(!c.may_read(StatusRoute::Publication), "{c:?}");
        }
        for p in [
            "/status/",
            "/eval/status/",
            "/eval/load/status/",
            "/eval/load/plan",
            "/eval/authority/main/clear",
            "/eval/authority/main/status/",
            "/eval/authority/../status",
            "/eval/admission/link/",
            "/Status",
            "",
        ] {
            assert_eq!(status_route(true, p), None, "GET {p}");
        }
    }

    /// The relay's `/status` keeps the three fields its rules read and
    /// nothing else: no per-op detail, reason, cohort or draw field.
    #[test]
    fn the_relay_view_carries_only_its_liveness_fields() {
        let full = serde_json::json!({
            "mode": "primary",
            "mode_age_ms": 12,
            "journal": {"confirmed": {"number": 5, "hash": "0xab"}, "confirmed_age_ms": 4000,
                        "fault": {"kind": "gap", "detail": "x", "id": 3}, "pending": [1, 2]},
            "signing": {"state": "fault", "reason": "r", "ops": [{"epoch": 9}],
                        "authorization": {"pointer_current": true}},
            "watchdog": {"subscription": {"state": "delivering"}},
            "eval_cohort": {"jobs": [{"call_ms": 3}], "draw": {"kinds": 2}},
        });
        assert_eq!(
            serde_json::to_string(&alerts_journal(&full)).unwrap(),
            r#"{"journal":{"confirmed_age_ms":4000,"fault":{"kind":"gap"}},"signing":{"state":"fault"}}"#
        );
        // Absent or mistyped fields stay absent; nothing else is copied.
        let sparse = serde_json::json!({"journal": {"confirmed_age_ms": "4000", "fault": null}, "signing": null});
        assert_eq!(serde_json::to_string(&alerts_journal(&sparse)).unwrap(), r#"{"journal":{}}"#);
        assert_eq!(serde_json::to_string(&alerts_journal(&serde_json::json!(null))).unwrap(), "{}");
        let kindless = serde_json::json!({"journal": {"fault": {"detail": "x"}}, "signing": {"ops": []}});
        assert_eq!(
            serde_json::to_string(&alerts_journal(&kindless)).unwrap(),
            r#"{"journal":{"fault":{"kind":null}},"signing":{"state":null}}"#
        );
    }

    /// The publisher and the alerts relay address their reads to this host,
    /// and read exactly the routes their tokens open: the publisher
    /// `/status` and `/eval/status`, the relay `/status`.
    #[test]
    fn the_consumers_address_the_internal_host() {
        let publisher = include_str!("../../publisher/src/cf.rs");
        let relay = include_str!("../../alerts-relay/src/cf.rs");
        let url = format!("https://{STATUS_INTERNAL_HOST}");
        assert!(publisher.contains(&format!("\"{url}{{path}}\"")), "publisher");
        // Every path the publisher passes to its status read.
        let mut paths: Vec<&str> = publisher
            .split("status_read(")
            .skip(1)
            .filter_map(|call| call.split('"').nth(1))
            .collect();
        // The function's own definition has no literal before its body.
        paths.retain(|p| p.starts_with('/'));
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths, ["/eval/status", "/status"], "publisher status paths");
        for p in &paths {
            let route = status_route(true, p).expect("a status route");
            assert!(Caller::Publisher.may_read(route), "{p}");
        }
        let relay_urls: Vec<&str> = relay.match_indices(&url).map(|(i, _)| &relay[i + url.len()..]).collect();
        assert!(!relay_urls.is_empty(), "alerts relay");
        for rest in relay_urls {
            let path = rest.split('"').next().unwrap_or_default();
            assert_eq!(path, "/status", "alerts relay status path");
            assert!(Caller::Alerts.may_read(status_route(true, path).expect("a status route")));
        }
    }
}
