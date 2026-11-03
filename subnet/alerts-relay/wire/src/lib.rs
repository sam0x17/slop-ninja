//! The alert contract between the Slop Ninja testnet publisher and the
//! private alerts relay (two separate Workers joined by a service binding),
//! and the hold/renotify state machine both of them run.
//!
//! - [`Post`]: one batch of alert events, posted to the relay's
//!   [`ALERTS_PATH`]. Each [`Event`] carries an [`event_id`] derived from the
//!   sender's alert state (and the fields it is derived from), so a retried
//!   post (its earlier answer was lost) carries the same ids, the relay sends
//!   it once, and the relay can recompute every id it is sent.
//! - [`Heartbeat`]: posted to [`HEARTBEAT_PATH`] after every publisher run
//!   whose alert phase finished; the relay's dead-man rule watches it, and
//!   its delivery fields tell the relay when the publisher's alert posts keep
//!   failing.
//! - [`state`]: conditions, holds, reminders and resolutions
//!   (`observe`, `due`, `attempt`, `attempt_resolutions`, `acknowledge`).
//! - The publisher's compiled timing ([`PUBLISHER_CRON_PERIOD_MS`],
//!   [`PUBLISHER_LEASE_MS`], [`PUBLISHER_UNREACHABLE_HOLD_MS`],
//!   [`STATUS_TIMEOUT_MS`], [`OUTBOUND_TIMEOUT_MS`], [`watermark_stall_ms`]):
//!   the publisher builds with these constants and the relay derives its
//!   bounds from them, so the two cannot drift apart.
//!
//! Every input has a bound fixed by this contract ([`post_bound`],
//! [`HEARTBEAT_BYTES`]), so the relay refuses a larger body before reading
//! it. The bounds derive from the contract's fields: the publisher's
//! splitter bound for the events ([`POST_EVENTS_BYTES`]), the protocol's
//! string bound for names and subjects ([`MAX_STRING_BYTES`]), and the
//! fields' own widths. Changing one is a contract change (a new schema).
//! Nothing here carries an address, a key, a URL or a spend figure.

pub mod state;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use state::{
    AlertState, Condition, Entry, acknowledge, attempt, attempt_resolutions, due, observe,
};

/// The schema of one post of alert events (v3: each event carries its id
/// and the last-notified time the id is derived from).
pub const POST_SCHEMA: &str = "slop_ninja.publisher.alert/v3";
/// The schema of the publisher's heartbeat (v3: the publisher's watermark
/// bound; v2 added the post schema and the run's undelivered events).
pub const HEARTBEAT_SCHEMA: &str = "slop_ninja.publisher.heartbeat/v3";
/// Domain of [`event_id`].
pub const EVENT_ID_DOMAIN: &str = "slop_ninja.alert_event_id/v2";

/// The relay's routes (reached only through service bindings).
pub const ALERTS_PATH: &str = "/alerts";
pub const HEARTBEAT_PATH: &str = "/heartbeat";

/// The publisher's cron period (`* * * * *` in its `wrangler.toml`; a
/// publisher test checks the file).
pub const PUBLISHER_CRON_PERIOD_MS: u64 = 60_000;
/// The publisher's run lease: two cron periods. A run killed mid-step blocks
/// the next runs' alert phase for one lease at most.
pub const PUBLISHER_LEASE_MS: u64 = 2 * PUBLISHER_CRON_PERIOD_MS;
/// The publisher's hold for an unanswered status source: two cron periods
/// (two missed runs).
pub const PUBLISHER_UNREACHABLE_HOLD_MS: u64 = 2 * PUBLISHER_CRON_PERIOD_MS;
/// The publisher's bound on one status read (and the relay's, for the same
/// reads and for its tracker object).
pub const STATUS_TIMEOUT_MS: u64 = 10_000;
/// The publisher's bound on one outbound request (an alert post, a
/// heartbeat).
pub const OUTBOUND_TIMEOUT_MS: u64 = 10_000;
/// The relay answers a post within this of starting to handle it: half the
/// publisher's request bound. The other half is left for the hops the relay
/// cannot time (the publisher's service-binding call, the forward to the
/// relay's Durable Object, and the answer back), so a send the relay records
/// as sent is answered before the publisher stops waiting. The relay's send
/// bound is what remains of it after the body read and the claims.
pub const RELAY_ANSWER_MS: u64 = OUTBOUND_TIMEOUT_MS / 2;
const _: () = assert!(RELAY_ANSWER_MS > 0 && RELAY_ANSWER_MS < OUTBOUND_TIMEOUT_MS);
/// The publisher's default `ALERT_RENOTIFY_SECS`, in milliseconds.
pub const DEFAULT_RENOTIFY_MS: u64 = 6 * 3600 * 1000;
/// The shortest renotify interval: one cron period (a shorter one would
/// remind on every run, the same as one period).
pub const MIN_RENOTIFY_MS: u64 = PUBLISHER_CRON_PERIOD_MS;

/// The publisher's `watermark_stalled` bound for a watchdog silence: by
/// two cron periods after the silence, a healthy Worker has confirmed a newer
/// block.
pub const fn watermark_stall_ms(silence_ms: u64) -> u64 {
    silence_ms.saturating_add(2 * PUBLISHER_CRON_PERIOD_MS)
}

/// The serialized events one post carries at most: the publisher's splitter
/// bound, shared by all of its sinks (Slack's 40,000-character message
/// limit, which the publisher's splitter applies to its JSON body as well).
/// The relay accepts exactly what that splitter produces. Widening it is a
/// contract change: a new [`POST_SCHEMA`], and a new relay arena rule, since
/// the relay's memory derives from it.
pub const POST_EVENTS_BYTES: usize = 40_000;

/// The longest string of this contract other than an event's detail: the
/// protocol's string bound (`slop_ninja_protocol::MAX_STRING_LEN`, the
/// largest cohort label; a test checks they agree). Every subject the
/// publisher names stays within it, and so does every name
/// ([`valid_name`]).
pub const MAX_STRING_BYTES: usize = 4096;

/// The digits of the largest `u64`.
const U64_DIGITS: usize = 20;
/// An event id's length ([`valid_id`]).
const ID_BYTES: usize = 64;

/// A post with empty names and no events, compact and in serde's field
/// order (a test checks it).
const POST_SKELETON: &str = r#"{"schema":"","rules":"","network":"","events":[]}"#;

/// A post's envelope around its events, derived: the skeleton, the post
/// schema, and the rule set and network names at their bound.
pub const ENVELOPE_BYTES: usize = POST_SKELETON.len() + POST_SCHEMA.len() + 2 * MAX_STRING_BYTES;

/// A heartbeat with empty names and no numbers, in serde's field order.
const HEARTBEAT_SKELETON: &str = r#"{"schema":"","network":"","rules":"","post_schema":"","run_started_ms":,"renotify_ms":,"watermark_stall_ms":,"config_refused":,"undelivered":,"undelivered_since_ms":}"#;

/// A heartbeat's bound, derived: its skeleton, its schema, three names at
/// their bound, five numbers at the largest `u64` and the longer boolean
/// (a test checks that a maximal heartbeat is exactly this long).
pub const HEARTBEAT_BYTES: usize = HEARTBEAT_SKELETON.len()
    + HEARTBEAT_SCHEMA.len()
    + 3 * MAX_STRING_BYTES
    + 5 * U64_DIGITS
    + "false".len();

/// An event with empty strings and no numbers, in serde's field order.
const EVENT_SKELETON: &str =
    r#"{"id":"","state":"","rule":"","subject":"","detail":"","since_ms":,"notified_ms":}"#;

/// The shortest serialized event, derived: its id, the shortest kind
/// (`firing`), empty strings and one-digit times. A post holds at most one
/// event per this many bytes (a test checks the shortest event is exactly
/// this long).
pub const MIN_EVENT_BYTES: usize = EVENT_SKELETON.len() + ID_BYTES + "firing".len() + 2;

/// The largest post body the relay accepts.
pub const fn post_bound() -> usize {
    POST_EVENTS_BYTES + ENVELOPE_BYTES
}

/// A name of this contract (a schema, a rule set, a network): 1 to
/// [`MAX_STRING_BYTES`] bytes of lowercase ASCII letters, digits and
/// `._/-`. A name never needs JSON escaping, so the bounds above hold for
/// every body whose names are valid; a body with other names is refused.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_STRING_BYTES
        && s.bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'/' | b'-'))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Firing,
    Reminder,
    Resolved,
}

impl EventKind {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Firing => "FIRING",
            Self::Reminder => "STILL FIRING",
            Self::Resolved => "RESOLVED",
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::Firing => 1,
            Self::Reminder => 2,
            Self::Resolved => 3,
        }
    }
}

/// One notification. `id` is [`event_id`] of the post's rule set and the
/// other fields (all but `detail`), so the same due event always has the
/// same id until it is acknowledged, and the relay recomputes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub id: String,
    pub state: EventKind,
    pub rule: String,
    pub subject: String,
    pub detail: String,
    /// When the condition was first seen.
    pub since_ms: u64,
    /// When its firing or latest reminder was acknowledged; `None` before
    /// any was. A resolution with `None` clears a condition whose firing post
    /// was attempted but never acknowledged ([`Event::unconfirmed`]).
    pub notified_ms: Option<u64>,
}

fn field(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_be_bytes());
    h.update(bytes);
}

/// The id of one due event: SHA-256 over the domain, the rule set, the kind,
/// the rule, the subject, when the condition was first seen and when it was
/// last notified (tagged, so `None` and `Some(0)` differ). A retry of an
/// unacknowledged event repeats every input, so it repeats the id; the next
/// reminder has a new `notified_ms`, so a new id.
pub fn event_id(
    rules: &str,
    state: EventKind,
    rule: &str,
    subject: &str,
    since_ms: u64,
    notified_ms: Option<u64>,
) -> String {
    let mut h = Sha256::new();
    field(&mut h, EVENT_ID_DOMAIN.as_bytes());
    field(&mut h, rules.as_bytes());
    h.update([state.code()]);
    field(&mut h, rule.as_bytes());
    field(&mut h, subject.as_bytes());
    h.update(since_ms.to_be_bytes());
    match notified_ms {
        None => h.update([0]),
        Some(t) => {
            h.update([1]);
            h.update(t.to_be_bytes());
        }
    }
    hex::encode(h.finalize())
}

/// A 64-character lowercase hex id.
pub fn valid_id(id: &str) -> bool {
    id.len() == ID_BYTES && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Event {
    /// The event of `kind` for these fields under `rules`, with its id.
    pub fn new(
        rules: &str,
        state: EventKind,
        rule: &str,
        subject: &str,
        detail: &str,
        since_ms: u64,
        notified_ms: Option<u64>,
    ) -> Self {
        Self {
            id: event_id(rules, state, rule, subject, since_ms, notified_ms),
            state,
            rule: rule.to_owned(),
            subject: subject.to_owned(),
            detail: detail.to_owned(),
            since_ms,
            notified_ms,
        }
    }

    /// Whether `id` is the event's id under `rules`.
    pub fn id_matches(&self, rules: &str) -> bool {
        self.id
            == event_id(
                rules,
                self.state,
                &self.rule,
                &self.subject,
                self.since_ms,
                self.notified_ms,
            )
    }

    /// A resolution of a condition that was never acknowledged as notified:
    /// its firing post was attempted, and may or may not have been sent.
    pub fn unconfirmed(&self) -> bool {
        self.state == EventKind::Resolved && self.notified_ms.is_none()
    }

    /// The id of this condition's firing event under `rules` (the firing of
    /// an [`Event::unconfirmed`] resolution).
    pub fn firing_id(&self, rules: &str) -> String {
        event_id(
            rules,
            EventKind::Firing,
            &self.rule,
            &self.subject,
            self.since_ms,
            None,
        )
    }

    /// The event's serialized length in a post.
    pub fn wire_len(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |b| b.len())
    }

    /// Shortens the detail (never the rule or subject, which key the
    /// sender's acknowledgment) until the serialized event is at most
    /// `limit` bytes. Returns whether it fits.
    pub fn fit(&mut self, limit: usize) -> bool {
        let mut len = self.wire_len();
        while len > limit && !self.detail.is_empty() {
            let over = len - limit;
            // JSON escaping can double a character, so cut at least what is
            // over and re-measure.
            let keep = self.detail.len().saturating_sub(over.max(1));
            let mut cut = keep;
            while !self.detail.is_char_boundary(cut) {
                cut -= 1;
            }
            self.detail.truncate(cut);
            len = self.wire_len();
        }
        len <= limit
    }
}

/// One post of events.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Post {
    pub schema: String,
    /// The sender's rule set.
    pub rules: String,
    pub network: String,
    pub events: Vec<Event>,
}

/// Why the relay refuses a post or a heartbeat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    TooLarge,
    Malformed,
    Schema,
    Network,
    EventId,
    Empty,
    /// A name ([`valid_name`]) outside the contract's alphabet or bound.
    Name,
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::Malformed => "malformed",
            Self::Schema => "schema",
            Self::Network => "network",
            Self::EventId => "event_id",
            Self::Empty => "empty",
            Self::Name => "name",
        }
    }
}

impl Post {
    /// Parses and checks a post body of at most [`post_bound`] bytes.
    pub fn parse(body: &[u8], network: &str) -> Result<Self, Refusal> {
        if body.len() > post_bound() {
            return Err(Refusal::TooLarge);
        }
        let p: Post = serde_json::from_slice(body).map_err(|_| Refusal::Malformed)?;
        if p.schema != POST_SCHEMA {
            return Err(Refusal::Schema);
        }
        if p.network != network {
            return Err(Refusal::Network);
        }
        if !valid_name(&p.rules) {
            return Err(Refusal::Name);
        }
        if p.events.is_empty() {
            return Err(Refusal::Empty);
        }
        // Every id is the one the event's own fields derive: only
        // state-derived ids are emailed.
        if p.events.iter().any(|e| !e.id_matches(&p.rules)) {
            return Err(Refusal::EventId);
        }
        Ok(p)
    }

    pub fn body(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

/// The publisher's heartbeat: its alert phase finished (status read, rules
/// evaluated, due posts attempted, state saved), and how its posts fared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heartbeat {
    pub schema: String,
    pub network: String,
    /// The publisher's rule set.
    pub rules: String,
    /// The post schema the publisher's build sends ([`POST_SCHEMA`] of its
    /// wire crate): the relay flags a skew between the two builds directly.
    pub post_schema: String,
    /// When the run started (publisher clock).
    pub run_started_ms: u64,
    /// The publisher's configured renotify interval; the relay's own alerts
    /// remind at the same interval.
    pub renotify_ms: u64,
    /// The publisher's `watermark_stalled` bound ([`watermark_stall_ms`] of
    /// its `WATCHDOG_SILENCE_SECS`): the relay's coordinator check, while
    /// the publisher is down, uses the bound the publisher last used.
    pub watermark_stall_ms: u64,
    /// The publisher's configuration was refused this run (it still alerts).
    pub config_refused: bool,
    /// Due events this run's alert phase left undelivered (0 when every post
    /// was accepted, or nothing was due).
    pub undelivered: u64,
    /// The start of the run that began the current series of runs leaving
    /// events undelivered (publisher clock); `None` once a run delivers
    /// everything due.
    pub undelivered_since_ms: Option<u64>,
}

impl Heartbeat {
    pub fn parse(body: &[u8], network: &str) -> Result<Self, Refusal> {
        if body.len() > HEARTBEAT_BYTES {
            return Err(Refusal::TooLarge);
        }
        let h: Heartbeat = serde_json::from_slice(body).map_err(|_| Refusal::Malformed)?;
        if h.schema != HEARTBEAT_SCHEMA {
            return Err(Refusal::Schema);
        }
        if h.network != network {
            return Err(Refusal::Network);
        }
        if !valid_name(&h.rules) || !valid_name(&h.post_schema) {
            return Err(Refusal::Name);
        }
        Ok(h)
    }

    pub fn body(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: &str = "slop_ninja.publisher.alerts/v3";

    fn ev(detail: &str) -> Event {
        Event::new(
            RULES,
            EventKind::Firing,
            "commit_gap",
            "coordinator",
            detail,
            5,
            None,
        )
    }

    #[test]
    fn ids_repeat_for_a_retry_and_change_for_the_next_reminder() {
        let a = event_id("r/v1", EventKind::Reminder, "x", "y", 1, Some(2));
        assert_eq!(
            a,
            event_id("r/v1", EventKind::Reminder, "x", "y", 1, Some(2))
        );
        assert!(valid_id(&a));
        for other in [
            event_id("r/v2", EventKind::Reminder, "x", "y", 1, Some(2)),
            event_id("r/v1", EventKind::Firing, "x", "y", 1, Some(2)),
            event_id("r/v1", EventKind::Reminder, "xy", "", 1, Some(2)),
            event_id("r/v1", EventKind::Reminder, "x", "y", 2, Some(2)),
            event_id("r/v1", EventKind::Reminder, "x", "y", 1, Some(3)),
            event_id("r/v1", EventKind::Reminder, "x", "y", 1, None),
        ] {
            assert_ne!(a, other);
        }
        // Never notified and notified at 0 are different ids.
        assert_ne!(
            event_id("r/v1", EventKind::Resolved, "x", "y", 1, None),
            event_id("r/v1", EventKind::Resolved, "x", "y", 1, Some(0))
        );
        assert!(!valid_id(&a.to_uppercase()));
        assert!(!valid_id(&a[1..]));
    }

    #[test]
    fn unconfirmed_resolutions_name_their_firing() {
        let z = Event::new(
            RULES,
            EventKind::Resolved,
            "commit_gap",
            "coordinator",
            "d",
            5,
            None,
        );
        assert!(z.unconfirmed());
        assert_eq!(z.firing_id(RULES), ev("other detail").id);
        let notified = Event::new(
            RULES,
            EventKind::Resolved,
            "commit_gap",
            "coordinator",
            "d",
            5,
            Some(9),
        );
        assert!(!notified.unconfirmed());
    }

    #[test]
    fn fit_shortens_only_the_detail() {
        let mut e = ev(&"é\"\n".repeat(20_000));
        assert!(e.wire_len() > POST_EVENTS_BYTES);
        assert!(e.fit(POST_EVENTS_BYTES));
        assert!(e.wire_len() <= POST_EVENTS_BYTES);
        assert_eq!(
            (e.rule.as_str(), e.subject.as_str()),
            ("commit_gap", "coordinator")
        );
        assert!(e.id_matches(RULES), "the id does not cover the detail");
        // The largest subject with any detail still fits a post.
        let mut e = ev(&"d".repeat(100_000));
        e.subject = "s".repeat(MAX_STRING_BYTES);
        assert!(e.fit(POST_EVENTS_BYTES));
        let mut small = ev("short");
        let before = small.clone();
        assert!(small.fit(POST_EVENTS_BYTES));
        assert_eq!(small, before);
    }

    /// The shortest event is exactly [`MIN_EVENT_BYTES`] long; every other
    /// event is longer.
    #[test]
    fn min_event_bytes_is_the_shortest_event() {
        let shortest = Event {
            id: "0".repeat(64),
            state: EventKind::Firing,
            rule: String::new(),
            subject: String::new(),
            detail: String::new(),
            since_ms: 0,
            notified_ms: Some(0),
        };
        assert_eq!(shortest.wire_len(), MIN_EVENT_BYTES);
        for (state, notified_ms) in [
            (EventKind::Firing, None),
            (EventKind::Reminder, Some(0)),
            (EventKind::Resolved, Some(0)),
        ] {
            let e = Event {
                state,
                notified_ms,
                ..shortest.clone()
            };
            assert!(e.wire_len() > MIN_EVENT_BYTES);
        }
    }

    /// The skeletons the bounds are derived from are serde's output, and a
    /// maximal post envelope and heartbeat are exactly their bounds.
    #[test]
    fn bounds_are_the_maximal_bodies() {
        assert_eq!(U64_DIGITS, u64::MAX.to_string().len());
        let empty = Post {
            schema: String::new(),
            rules: String::new(),
            network: String::new(),
            events: vec![],
        };
        assert_eq!(empty.body(), POST_SKELETON.as_bytes());
        let name = "a".repeat(MAX_STRING_BYTES);
        let max = Post {
            schema: POST_SCHEMA.into(),
            rules: name.clone(),
            network: name.clone(),
            events: vec![],
        };
        assert_eq!(max.body().len(), ENVELOPE_BYTES);
        let h = Heartbeat {
            schema: HEARTBEAT_SCHEMA.into(),
            network: name.clone(),
            rules: name.clone(),
            post_schema: name,
            run_started_ms: u64::MAX,
            renotify_ms: u64::MAX,
            watermark_stall_ms: u64::MAX,
            config_refused: false,
            undelivered: u64::MAX,
            undelivered_since_ms: Some(u64::MAX),
        };
        assert_eq!(h.body().len(), HEARTBEAT_BYTES);
        let e = Event {
            id: String::new(),
            state: EventKind::Firing,
            rule: String::new(),
            subject: String::new(),
            detail: String::new(),
            since_ms: 0,
            notified_ms: None,
        };
        let json = String::from_utf8(serde_json::to_vec(&e).unwrap()).unwrap();
        assert_eq!(
            json.replace("firing", "")
                .replace(":0,", ":,")
                .replace(":null", ":"),
            EVENT_SKELETON
        );
    }

    /// The string bound is the protocol's, which bounds every label and
    /// subject the publisher names.
    #[test]
    fn the_string_bound_is_the_protocols() {
        assert_eq!(
            MAX_STRING_BYTES,
            slop_ninja_protocol::MAX_STRING_LEN as usize
        );
    }

    /// Every name in use is valid; names that would need escaping, or are
    /// empty or too long, are not.
    #[test]
    fn names() {
        for n in [
            POST_SCHEMA,
            HEARTBEAT_SCHEMA,
            EVENT_ID_DOMAIN,
            RULES,
            "slop_ninja.alerts_relay.rules/v1",
            "testnet",
        ] {
            assert!(valid_name(n), "{n}");
        }
        for n in [
            "",
            "A",
            "a b",
            "a\"b",
            "a\u{e9}",
            &"a".repeat(MAX_STRING_BYTES + 1),
        ] {
            assert!(!valid_name(n), "{n:?}");
        }
        assert!(valid_name(&"a".repeat(MAX_STRING_BYTES)));
    }

    fn post(events: Vec<Event>) -> Post {
        Post {
            schema: POST_SCHEMA.into(),
            rules: RULES.into(),
            network: "testnet".into(),
            events,
        }
    }

    #[test]
    fn posts_parse_within_their_bound() {
        let p = post(vec![ev("d")]);
        assert_eq!(Post::parse(&p.body(), "testnet").unwrap(), p);
        assert_eq!(Post::parse(&p.body(), "finney"), Err(Refusal::Network));
        let mut bad = p.clone();
        bad.events[0].id = "zz".into();
        assert_eq!(Post::parse(&bad.body(), "testnet"), Err(Refusal::EventId));
        // A well-formed id that the event's fields do not derive.
        let mut bad = p.clone();
        bad.events[0].since_ms = 6;
        assert_eq!(Post::parse(&bad.body(), "testnet"), Err(Refusal::EventId));
        let mut bad = p.clone();
        bad.rules = "slop_ninja.publisher.alerts/v9".into();
        assert_eq!(Post::parse(&bad.body(), "testnet"), Err(Refusal::EventId));
        // A rule set that is not a name is refused before its ids are read.
        let mut bad = p.clone();
        bad.rules = "Rules With Spaces".into();
        assert_eq!(Post::parse(&bad.body(), "testnet"), Err(Refusal::Name));
        let mut bad = p.clone();
        bad.schema = "slop_ninja.publisher.alert/v2".into();
        assert_eq!(Post::parse(&bad.body(), "testnet"), Err(Refusal::Schema));
        let mut bad = p.clone();
        bad.events.clear();
        assert_eq!(Post::parse(&bad.body(), "testnet"), Err(Refusal::Empty));
        let unknown = br#"{"schema":"slop_ninja.publisher.alert/v3","rules":"r","network":"testnet","events":[],"extra":1}"#;
        assert_eq!(Post::parse(unknown, "testnet"), Err(Refusal::Malformed));
        let big = vec![b' '; post_bound() + 1];
        assert_eq!(Post::parse(&big, "testnet"), Err(Refusal::TooLarge));
        // The envelope with no events fits its allowance, with room for the
        // array's punctuation.
        let mut empty = p.clone();
        empty.events.clear();
        assert!(empty.body().len() + 2 <= ENVELOPE_BYTES);
    }

    #[test]
    fn heartbeats_parse_within_their_bound() {
        let h = Heartbeat {
            schema: HEARTBEAT_SCHEMA.into(),
            network: "testnet".into(),
            rules: RULES.into(),
            post_schema: POST_SCHEMA.into(),
            run_started_ms: u64::MAX,
            renotify_ms: u64::MAX,
            watermark_stall_ms: u64::MAX,
            config_refused: true,
            undelivered: u64::MAX,
            undelivered_since_ms: Some(u64::MAX),
        };
        assert!(h.body().len() <= HEARTBEAT_BYTES);
        assert_eq!(Heartbeat::parse(&h.body(), "testnet").unwrap(), h);
        assert_eq!(Heartbeat::parse(&h.body(), "finney"), Err(Refusal::Network));
        assert_eq!(
            Heartbeat::parse(&vec![b' '; HEARTBEAT_BYTES + 1], "testnet"),
            Err(Refusal::TooLarge)
        );
        let mut old: serde_json::Value = serde_json::to_value(&h).unwrap();
        old["schema"] = "slop_ninja.publisher.heartbeat/v2".into();
        assert_eq!(
            Heartbeat::parse(&serde_json::to_vec(&old).unwrap(), "testnet"),
            Err(Refusal::Schema)
        );
        let mut skew = h.clone();
        skew.post_schema = "Post Schema".into();
        assert_eq!(
            Heartbeat::parse(&skew.body(), "testnet"),
            Err(Refusal::Name)
        );
    }

    #[test]
    fn derived_timing() {
        assert_eq!(PUBLISHER_LEASE_MS, 2 * PUBLISHER_CRON_PERIOD_MS);
        assert_eq!(watermark_stall_ms(300_000), 420_000);
        assert_eq!(MIN_RENOTIFY_MS, PUBLISHER_CRON_PERIOD_MS);
    }
}
