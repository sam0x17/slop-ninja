//! The alert feed (`slop_ninja.alert_feed/v1`): every message the relay
//! delivers is written once per event id into its private bucket as one
//! content-free JSON object, and `feed/v1/latest.json` is rewritten every
//! tick. A reader outside Cloudflare lists the objects after its cursor and
//! reads them in order; `latest.json` is its dead-man.
//!
//! - **Objects** live under [`EVENTS_PREFIX`], keyed by the UTC of their
//!   write and their event id ([`object_key`]). Key times are strictly
//!   increasing within the relay's Durable Object ([`crate::ledger::Table::
//!   feed_stamp`]), so keys sort in write order. Two writes can still land
//!   out of that order (a post and a tick interleave at their awaits), but
//!   only within one put bound, which is shorter than one cron period: a
//!   reader that re-lists from one [`RELAY_CRON_PERIOD_MS`] before its cursor
//!   and skips the event ids it has read misses none.
//! - **Once per event id.** A write is claimed and settled in the sent-event
//!   ledger under [`ledger_key`] the way an email is, so a retried post
//!   writes nothing new. Only an acknowledged put counts as written
//!   ([`crate::ledger::settle_acknowledged`]): a put whose outcome is
//!   unknown (it timed out) is written again after each claim period until
//!   one is acknowledged, and its sender is not acknowledged before that. An
//!   event can therefore be written more than once, under keys with the
//!   same event id, and a reader keeps the first; it is never lost to a
//!   timeout.
//! - **Content-free.** An object carries the rule, the subject, the kind,
//!   the times and the detail the email would carry, never a spend figure
//!   ([`without_figures`]) and never a word holding an address or a URL
//!   ([`clean_body`]): rule names, states, subjects, ages and epochs.
//! - **`latest.json`** ([`Latest`]) states the tick's time and count, the
//!   firing set, the last write's result, the retention, the horizon past
//!   which the relay holds nothing naming an event ([`horizon_ms`], the
//!   reader's archive horizon) and the relay's write gap (the longest between two of its writes), within
//!   [`LATEST_BYTES`].
//! - **Memory.** Each feed step (one object's write, `latest.json`, one
//!   delete batch) reserves the slot of one message ([`STEP_BYTES`]) before
//!   it allocates, and builds what it writes only inside that reservation.
//! - **Retention** ([`retention_ms`]): objects older than two of the spend
//!   tracker's schedule periods plus one pull period are deleted, batch by
//!   batch, from the relay's own record of the keys it wrote; `latest.json`
//!   is never among them.

use serde::{Deserialize, Serialize};
use slop_ninja_alerts_wire::{Event, EventKind, MAX_STRING_BYTES, POST_EVENTS_BYTES};

use crate::ledger::{StoreError, Table};
use crate::mime::{self, RFC3339_MAX_BYTES, SUBJECT_LINE, rfc3339};
use crate::relay::{Failure, SendError, code_text};

/// The schema of one feed object.
pub const FEED_SCHEMA: &str = "slop_ninja.alert_feed/v1";
/// The schema of `latest.json`.
pub const LATEST_SCHEMA: &str = "slop_ninja.alert_feed.latest/v1";
/// Where the event objects live.
pub const EVENTS_PREFIX: &str = "feed/v1/events/";
/// The relay's heartbeat object, rewritten every tick.
pub const LATEST_KEY: &str = "feed/v1/latest.json";

/// The relay's cron period (`* * * * *` in `wrangler.toml`'s testnet
/// environment; a test checks the file).
pub const RELAY_CRON_PERIOD_MS: u64 = 60_000;
/// A reader runs once per relay cron period.
pub const PULL_PERIOD_MS: u64 = RELAY_CRON_PERIOD_MS;

/// How long an object is kept: two of the spend tracker's schedule periods
/// (`tracker_stale_ms`, the tracker's staleness rule) plus one pull period.
/// The relay passes the larger of the staleness bound its last upload
/// states and the configured one, so an upload stating a shorter schedule
/// shortens the staleness rule, never the retention.
///
/// The bound: every object stays at least as long as the relay waits
/// before it reports the tracker stale, plus one pull period. Objects
/// written before `latest.json`'s `retained_from_ms` may have been deleted.
pub const fn retention_ms(tracker_stale_ms: u64) -> u64 {
    tracker_stale_ms.saturating_add(PULL_PERIOD_MS)
}

/// The most keys one sweep deletes: R2's limit for one `delete` call of
/// several keys. The rest wait for the next tick's sweep.
pub const DELETE_KEYS: usize = 1000;

/// What one feed step holds at most, reserved before it allocates: the slot
/// of one message ([`crate::arena::message_footprint`] of
/// [`crate::arena::text_bound`]), which the budget already counts for the
/// tick and for each post. A step is one object (the [`Object`], its key
/// and its serialized body), `latest.json` (the firing rows as read, their
/// decoded entries and the body) or one delete batch (the keys, their
/// vector and the names handed to the bucket); a test checks each fits.
pub const STEP_BYTES: u64 = crate::arena::message_footprint(crate::arena::text_bound());

/// The ledger id of an event's feed write. The email path keeps the bare
/// event id, so the rows it wrote before the feed existed keep their
/// meaning.
pub fn ledger_key(id: &str) -> String {
    format!("feed:{id}")
}

/// `20261102T134507123Z`: sortable, to the millisecond.
pub fn compact_utc(ms: u64) -> String {
    let s = rfc3339(ms);
    let mut out: String = s[..s.len() - 1]
        .chars()
        .filter(|c| *c != '-' && *c != ':')
        .collect();
    out.push_str(&format!("{:03}Z", ms % 1000));
    out
}

/// The key of the object for `event_id` written at `at_ms`.
pub fn object_key(at_ms: u64, event_id: &str) -> String {
    format!("{EVENTS_PREFIX}{}-{event_id}.json", compact_utc(at_ms))
}

/// The longest object key: the prefix, the widest compact time (the widest
/// RFC 3339 time without its four separators, with three digits of
/// milliseconds), a 64-digit event id and the extension (a test checks it).
pub const KEY_BYTES: usize = EVENTS_PREFIX.len() + (RFC3339_MAX_BYTES - 4 + 3) + 1 + 64 + 5;

/// What a spend alert's detail carries after the spend figures it may hold
/// (only when `SPEND_FIGURES_IN_EMAIL` is on): the feed cuts it there.
pub const FIGURES_LEAD: &str = crate::rules::FIGURES_LEAD;

/// The detail without spend figures, for the feed.
pub fn without_figures(detail: &str) -> &str {
    match detail.find(FIGURES_LEAD) {
        Some(at) => &detail[..at],
        None => detail,
    }
}

/// The feed's name for the kind (the email subjects' `STILL FIRING` is a
/// `REMINDER` here).
pub fn state_name(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Firing => "FIRING",
        EventKind::Reminder => "REMINDER",
        EventKind::Resolved => "RESOLVED",
    }
}

/// The feed's name for a sender.
pub fn source_name(source: &str) -> &'static str {
    if source == mime::SOURCE_PUBLISHER {
        "publisher"
    } else {
        "relay"
    }
}

/// One feed object (an event the relay delivered).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Object {
    pub schema: String,
    /// When it was written.
    pub utc: String,
    pub utc_ms: u64,
    pub event_id: String,
    pub network: String,
    /// `publisher`, `relay` or (drills only) `drill`.
    pub source: String,
    /// The sender's rule set.
    pub rules: String,
    pub rule: String,
    pub subject: String,
    /// `FIRING`, `REMINDER`, `RESOLVED` or (drills only) `DRILL`.
    pub state: String,
    /// A resolution of a firing that was never confirmed to its sender.
    pub unconfirmed: bool,
    /// The email subject line this event would have had.
    pub title: String,
    /// The event's detail, without spend figures.
    pub body: String,
    /// When the condition was first seen (the sender's clock).
    pub since_utc: String,
    pub since_ms: u64,
    pub notified_ms: Option<u64>,
    /// Seconds from `since_ms` to the write.
    pub age_s: u64,
}

/// An object with empty strings, no numbers and `null` options, in serde's
/// field order (a test checks it).
const OBJECT_SKELETON: &str = r#"{"schema":"","utc":"","utc_ms":,"event_id":"","network":"","source":"","rules":"","rule":"","subject":"","state":"","unconfirmed":,"title":"","body":"","since_utc":"","since_ms":,"notified_ms":,"age_s":}"#;
const U64_DIGITS: usize = 20;

/// The largest object: its skeleton; the schema; one event's rule, subject,
/// id and detail within the post's events bound (an event's serialized
/// length already counts their escaping); the rule set and network at the
/// contract's string bound; the longest source and state; the subject line;
/// two times; four numbers and the longer boolean.
pub const OBJECT_BYTES: usize = OBJECT_SKELETON.len()
    + FEED_SCHEMA.len()
    + POST_EVENTS_BYTES
    + 2 * MAX_STRING_BYTES
    + "publisher".len()
    + "RESOLVED".len()
    + SUBJECT_LINE
    + 2 * RFC3339_MAX_BYTES
    + 4 * U64_DIGITS
    + "false".len();

impl Object {
    /// The object for `e` from `source` (a [`mime::Source`]), written at
    /// `at_ms`.
    pub fn new(e: &Event, network: &str, source: mime::Source<'_>, at_ms: u64) -> Self {
        Self {
            schema: FEED_SCHEMA.into(),
            utc: rfc3339(at_ms),
            utc_ms: at_ms,
            event_id: e.id.clone(),
            network: network.into(),
            source: source_name(source.name).into(),
            rules: source.rules.into(),
            rule: e.rule.clone(),
            subject: e.subject.clone(),
            state: state_name(e.state).into(),
            unconfirmed: e.unconfirmed(),
            title: mime::subject(std::slice::from_ref(e), network),
            body: clean_body(without_figures(&e.detail)),
            since_utc: rfc3339(e.since_ms),
            since_ms: e.since_ms,
            notified_ms: e.notified_ms,
            age_s: at_ms.saturating_sub(e.since_ms) / 1000,
        }
    }

    /// The drill object for `drill` (a deploy-time `DRILL_ID`): it tests the
    /// feed, raises no rule and has no ledger row.
    pub fn drill(drill: &str, network: &str, at_ms: u64) -> Self {
        Self {
            schema: FEED_SCHEMA.into(),
            utc: rfc3339(at_ms),
            utc_ms: at_ms,
            event_id: drill_id(drill),
            network: network.into(),
            source: "drill".into(),
            rules: crate::rules::RULES_ID.into(),
            rule: "drill".into(),
            subject: drill.into(),
            state: "DRILL".into(),
            unconfirmed: false,
            title: mime::drill_subject(network),
            body: format!(
                "a drill of the alerts relay's feed ({drill}); it needs no action. Written once per \
                 drill id because the deploy-time setting DRILL_ID was set."
            ),
            since_utc: rfc3339(at_ms),
            since_ms: at_ms,
            notified_ms: None,
            age_s: 0,
        }
    }

    /// The serialized object, in one buffer of [`OBJECT_BYTES`].
    pub fn body(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(OBJECT_BYTES);
        if serde_json::to_writer(&mut out, self).is_err() {
            out.clear();
        }
        out
    }
}

/// A drill's event id: 64 hex digits from its id alone.
pub fn drill_id(drill: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"slop_ninja.alert_feed_drill/v1");
    h.update((drill.len() as u64).to_be_bytes());
    h.update(drill.as_bytes());
    hex::encode(h.finalize())
}

/// A detail for the feed: one line, without any word that holds an address
/// (`@`) or a URL (`://`), the filter the relay's log lines use
/// ([`crate::relay::message_text`]), so free text a sender passes through
/// (a refusal's reason) never puts one in the bucket or the inbox.
pub fn clean_body(s: &str) -> String {
    s.split_whitespace()
        .filter(|w| !w.contains('@') && !w.contains("://"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One entry of the firing set: a condition whose last written object was
/// a firing or a reminder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiringEntry {
    pub source: String,
    pub rule: String,
    pub subject: String,
    /// `FIRING` or `REMINDER`.
    pub state: String,
    pub since_utc: String,
    pub since_ms: u64,
    /// The event id of the last object written for it.
    pub event_id: String,
    pub at_utc: String,
}

impl FiringEntry {
    /// The firing set's key of an object.
    pub fn key(source: &str, rule: &str, subject: &str) -> String {
        format!("{source}/{rule}/{subject}")
    }

    fn of(o: &Object) -> Self {
        Self {
            source: o.source.clone(),
            rule: o.rule.clone(),
            subject: o.subject.clone(),
            state: o.state.clone(),
            since_utc: o.since_utc.clone(),
            since_ms: o.since_ms,
            event_id: o.event_id.clone(),
            at_utc: o.utc.clone(),
        }
    }
}

/// The last event write's outcome.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteNote {
    pub utc: String,
    pub utc_ms: u64,
    /// `written`, `timeout` or `refused`.
    pub result: String,
    /// The refusal's code (content-free), within the contract's string
    /// bound.
    pub code: String,
}

impl WriteNote {
    fn new(at_ms: u64, result: &str, code: &str) -> Self {
        let mut code = code_text(code);
        code.truncate(MAX_STRING_BYTES);
        Self {
            utc: rfc3339(at_ms),
            utc_ms: at_ms,
            result: result.into(),
            code,
        }
    }
}

/// How one object's write ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Written {
    Ok,
    TimedOut,
    Refused(Failure),
}

/// Writes one object (the feed's part of a delivery): stamps its key,
/// records it for the retention sweep before the put, puts it, and notes
/// the outcome. The caller holds a [`STEP_BYTES`] reservation from before
/// it built the object until after [`delivered`], which it calls on a
/// write that counts as delivered; a refused reservation is
/// [`refuse_budget`].
pub async fn write(
    t: &impl Table,
    object: &mut Object,
    now_ms: u64,
) -> Result<Written, StoreError> {
    let at = t.feed_stamp(now_ms)?;
    object.utc = rfc3339(at);
    object.utc_ms = at;
    object.age_s = at.saturating_sub(object.since_ms) / 1000;
    let key = object_key(at, &object.event_id);
    t.feed_record(&key, at)?;
    let outcome = match t.feed_put(&key, object.body()).await {
        Ok(()) => Written::Ok,
        Err(SendError::Timeout) => Written::TimedOut,
        Err(SendError::Refused(f)) => Written::Refused(f),
    };
    let note = match &outcome {
        Written::Ok => WriteNote::new(at, "written", ""),
        Written::TimedOut => WriteNote::new(at, "timeout", "timeout"),
        Written::Refused(f) => WriteNote::new(at, "refused", &f.code),
    };
    t.feed_note(&note)?;
    Ok(outcome)
}

/// A write refused before it allocated (the budget had no room for its
/// step): noted as refused (code `budget`); the caller frees its claim and
/// the event stays due.
pub fn refuse_budget(t: &impl Table, now_ms: u64) -> Result<Failure, StoreError> {
    let f = Failure::new("budget", "");
    t.feed_note(&WriteNote::new(t.feed_stamp(now_ms)?, "refused", &f.code))?;
    Ok(f)
}

/// After an object counts as delivered: a firing or reminder enters the
/// firing set (at its write time), a resolution leaves it.
pub fn delivered(t: &impl Table, o: &Object) -> Result<(), StoreError> {
    let key = FiringEntry::key(&o.source, &o.rule, &o.subject);
    match o.state.as_str() {
        "FIRING" | "REMINDER" => t.feed_firing(&key, Some(&FiringEntry::of(o)), o.utc_ms),
        "RESOLVED" => t.feed_firing(&key, None, o.utc_ms),
        _ => Ok(()),
    }
}

/// A resolution settled without being written (an unconfirmed resolution
/// whose firing never reached the feed, for example one fired under an
/// earlier rule set): its condition is clear, so it leaves the firing set.
pub fn unfired(t: &impl Table, source: &str, e: &Event, now_ms: u64) -> Result<(), StoreError> {
    if e.state != EventKind::Resolved {
        return Ok(());
    }
    t.feed_firing(
        &FiringEntry::key(source_name(source), &e.rule, &e.subject),
        None,
        now_ms,
    )
}

/// How long a firing-set row lasts without being refreshed: the retention
/// plus one renotify interval. An open condition is restated every renotify
/// interval, which refreshes its row; a row older than that belongs to a
/// condition whose resolution was never delivered, or whose sender has
/// been gone longer than the retention (it comes back with its next
/// reminder).
pub const fn firing_ttl_ms(retention_ms: u64, renotify_ms: u64) -> u64 {
    retention_ms.saturating_add(renotify_ms)
}

/// How long the relay holds anything that names an event: an object for
/// the retention, then a ledger row for the ledger's retention after its
/// last touch ([`crate::ledger::retention_ms`] of the renotify interval,
/// never below it), and a firing-set row for [`firing_ttl_ms`], which this
/// covers.
pub const fn horizon_ms(retention_ms: u64, renotify_ms: u64) -> u64 {
    retention_ms.saturating_add(crate::ledger::retention_ms(renotify_ms))
}
const _: () = assert!(horizon_ms(0, 0) >= firing_ttl_ms(0, 0));

/// Deletes one batch of objects older than the retention: the keys the
/// relay recorded before `now_ms - retention`, at most [`DELETE_KEYS`],
/// inside one [`STEP_BYTES`] reservation (refused: nothing is read, the
/// next tick's sweep tries again). Only keys under [`EVENTS_PREFIX`] are
/// ever deleted, never [`LATEST_KEY`]. Returns how many were deleted.
pub async fn sweep(
    t: &impl Table,
    budget: &crate::arena::Budget,
    now_ms: u64,
    retention: u64,
) -> Result<usize, StoreError> {
    let Ok(_hold) = budget.reserve(STEP_BYTES) else {
        return Ok(0);
    };
    let cutoff = now_ms.saturating_sub(retention);
    let keys: Vec<String> = t
        .feed_expired(cutoff, DELETE_KEYS)?
        .into_iter()
        .filter(|k| k.starts_with(EVENTS_PREFIX) && k.as_str() != LATEST_KEY)
        .collect();
    if keys.is_empty() {
        return Ok(0);
    }
    match t.feed_delete(&keys).await {
        Ok(()) => {
            t.feed_forget(&keys)?;
            Ok(keys.len())
        }
        // Not deleted: the next tick's sweep retries them.
        Err(_) => Ok(0),
    }
}

/// `latest.json`: the relay's heartbeat for a reader.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Latest {
    pub schema: String,
    pub network: String,
    /// When the tick wrote it.
    pub utc: String,
    pub utc_ms: u64,
    /// Ticks this relay state has run.
    pub tick: u64,
    pub cron_period_s: u64,
    pub pull_period_s: u64,
    /// The longest a tick reads before it evaluates (its lease before the
    /// first batch).
    pub tick_reads_s: u64,
    /// The longest between two of the relay's writes while it runs
    /// ([`crate::relay::WRITE_GAP_MS`]): a reader's dead-man bound, with
    /// one pull period of hold.
    pub write_gap_s: u64,
    pub retention_s: u64,
    /// Objects written before this may have been deleted.
    pub retained_from_ms: u64,
    /// How long the relay holds anything that names an event
    /// ([`horizon_ms`]): the retention plus the ledger's row retention, at
    /// least the firing set's row lifetime. Past it no object, firing-set
    /// row or ledger row names an older event, so a reader can archive
    /// items older than this and forget their ids.
    pub horizon_s: u64,
    /// The largest object the relay writes ([`OBJECT_BYTES`]): a reader
    /// skips a larger one.
    pub object_bytes: u64,
    /// `on` or `off`.
    pub email: String,
    /// The tick finished and its deliveries have not been failing for the
    /// undelivered hold.
    pub delivery_ok: bool,
    pub due: u64,
    pub written: u64,
    pub emailed: u64,
    pub undelivered: u64,
    /// Ignored optional settings (a count; `relay_config_warning` names
    /// them).
    pub config_warnings: u64,
    pub last_write: Option<WriteNote>,
    /// Conditions whose last object was a firing or a reminder.
    pub firing_count: u64,
    /// Entries of the firing set not listed (past [`LATEST_BYTES`]).
    pub firing_omitted: u64,
    pub firing: Vec<FiringEntry>,
}

/// `latest.json` with empty strings, no numbers and no firing entries, in
/// serde's field order (a test checks it).
const LATEST_SKELETON: &str = r#"{"schema":"","network":"","utc":"","utc_ms":,"tick":,"cron_period_s":,"pull_period_s":,"tick_reads_s":,"write_gap_s":,"retention_s":,"retained_from_ms":,"horizon_s":,"object_bytes":,"email":"","delivery_ok":,"due":,"written":,"emailed":,"undelivered":,"config_warnings":,"last_write":{"utc":"","utc_ms":,"result":"","code":""},"firing_count":,"firing_omitted":,"firing":[]}"#;

/// `latest.json` without firing entries at its widest: the skeleton, the
/// schema, the network the configuration admits, two times, the longer
/// email state and write result, a refusal code at the contract's string
/// bound (code text needs no escaping), eighteen numbers and the longer
/// boolean.
pub const LATEST_FIXED_BYTES: usize = LATEST_SKELETON.len()
    + LATEST_SCHEMA.len()
    + crate::config::NETWORK.len()
    + 2 * RFC3339_MAX_BYTES
    + "off".len()
    + "refused".len()
    + MAX_STRING_BYTES
    + 18 * U64_DIGITS
    + "false".len();
/// `latest.json`'s bound: the fixed part and the firing entries within one
/// post's events bound (the same splitter bound every alert batch has).
pub const LATEST_BYTES: usize = LATEST_FIXED_BYTES + POST_EVENTS_BYTES;

impl Latest {
    /// Adds firing entries in order while they fit [`LATEST_BYTES`]; counts
    /// the rest as omitted.
    pub fn fill(&mut self, entries: impl IntoIterator<Item = FiringEntry>, total: u64) {
        let mut room = POST_EVENTS_BYTES;
        for e in entries {
            let n = serde_json::to_vec(&e).map_or(usize::MAX, |b| b.len() + 1);
            if n <= room {
                room -= n;
                self.firing.push(e);
            }
        }
        self.firing_count = total.max(self.firing.len() as u64);
        self.firing_omitted = self.firing_count - self.firing.len() as u64;
    }

    /// The serialized object, in one buffer of [`LATEST_BYTES`].
    pub fn body(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(LATEST_BYTES);
        if serde_json::to_writer(&mut out, self).is_err() {
            out.clear();
        }
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ledger::MemTable;
    use crate::mime::tests::{RULES, ev};

    #[test]
    fn keys_sort_by_time_and_are_bounded() {
        let a = object_key(1_793_627_107_000, &"a".repeat(64));
        let b = object_key(1_793_627_107_001, &"0".repeat(64));
        assert!(a < b, "{a} {b}");
        assert!(a.starts_with("feed/v1/events/20261102T"), "{a}");
        assert!(a.len() <= KEY_BYTES);
        assert_eq!(object_key(u64::MAX, &"f".repeat(64)).len(), KEY_BYTES);
        assert_eq!(compact_utc(1_793_627_107_042), "20261102T134507042Z");
        // Every event key sorts before latest.json, which is outside the
        // events prefix anyway.
        assert!(!LATEST_KEY.starts_with(EVENTS_PREFIX));
    }

    #[test]
    fn objects_are_content_free_and_bounded() {
        let mut e = ev(EventKind::Firing, "spend_alert", "2026-11", "");
        e.detail = format!(
            "2026-11 is at or above the private spend alert threshold.{FIGURES_LEAD} $12.34 \
             (projected $56.78)."
        );
        let o = Object::new(
            &e,
            "testnet",
            mime::Source {
                name: mime::SOURCE_RELAY,
                rules: crate::rules::RULES_ID,
            },
            e.since_ms + 61_000,
        );
        assert!(
            !o.body.contains('$') && !o.body.contains("12.34"),
            "{}",
            o.body
        );
        assert_eq!((o.state.as_str(), o.source.as_str()), ("FIRING", "relay"));
        assert_eq!(o.title, "[slop-ninja testnet] FIRING spend_alert 2026-11");
        assert_eq!(o.age_s, 61);
        let skeleton = Object {
            schema: String::new(),
            utc: String::new(),
            utc_ms: 0,
            event_id: String::new(),
            network: String::new(),
            source: String::new(),
            rules: String::new(),
            rule: String::new(),
            subject: String::new(),
            state: String::new(),
            unconfirmed: false,
            title: String::new(),
            body: String::new(),
            since_utc: String::new(),
            since_ms: 0,
            notified_ms: None,
            age_s: 0,
        };
        let json = String::from_utf8(serde_json::to_vec(&skeleton).unwrap()).unwrap();
        assert_eq!(
            json.replace(":0,", ":,")
                .replace(":0}", ":}")
                .replace(":null", ":")
                .replace(":false", ":"),
            OBJECT_SKELETON
        );
        // The largest event a post can carry, with the widest names.
        let mut big = Event::new(
            RULES,
            EventKind::Reminder,
            "publication_pending",
            &"s".repeat(MAX_STRING_BYTES),
            &"\u{1}\"".repeat(POST_EVENTS_BYTES),
            u64::MAX,
            Some(u64::MAX),
        );
        assert!(big.fit(POST_EVENTS_BYTES - 1));
        let rules = "r".repeat(MAX_STRING_BYTES);
        let mut o = Object::new(
            &big,
            &"n".repeat(MAX_STRING_BYTES),
            mime::Source {
                name: mime::SOURCE_PUBLISHER,
                rules: &rules,
            },
            u64::MAX,
        );
        o.unconfirmed = false;
        o.age_s = u64::MAX;
        o.utc_ms = u64::MAX;
        assert!(
            o.body().len() <= OBJECT_BYTES,
            "{} > {OBJECT_BYTES}",
            o.body().len()
        );
        // One write's step fits its reservation: the object's strings (at
        // most its serialized length), its body and its key.
        assert!((2 * OBJECT_BYTES + KEY_BYTES) as u64 <= STEP_BYTES);
        // Free text loses words with an address or a URL, and stays one line.
        let mut e = ev(
            EventKind::Firing,
            "durable_fault",
            "coordinator.signing",
            "",
        );
        e.detail =
            "signing fault bad key for ops@example.invalid at\nhttps://x.invalid/a ok".into();
        let o = Object::new(
            &e,
            "testnet",
            mime::Source {
                name: mime::SOURCE_PUBLISHER,
                rules: RULES,
            },
            0,
        );
        assert_eq!(o.body, "signing fault bad key for at ok");
    }

    #[test]
    fn latest_is_bounded_and_lists_what_fits() {
        let entry = |i: usize| FiringEntry {
            source: "publisher".into(),
            rule: "commit_gap".into(),
            subject: format!("s{i}{}", "x".repeat(MAX_STRING_BYTES)),
            state: "FIRING".into(),
            since_utc: rfc3339(u64::MAX),
            since_ms: u64::MAX,
            event_id: "f".repeat(64),
            at_utc: rfc3339(u64::MAX),
        };
        let empty = Latest {
            last_write: Some(WriteNote::default()),
            ..Latest::default()
        };
        let json = String::from_utf8(empty.body()).unwrap();
        assert_eq!(
            json.replace(":0,", ":,")
                .replace(":0}", ":}")
                .replace(":false", ":"),
            LATEST_SKELETON
        );
        let mut l = Latest {
            schema: LATEST_SCHEMA.into(),
            network: crate::config::NETWORK.into(),
            utc: rfc3339(u64::MAX),
            utc_ms: u64::MAX,
            tick: u64::MAX,
            cron_period_s: u64::MAX,
            pull_period_s: u64::MAX,
            tick_reads_s: u64::MAX,
            write_gap_s: u64::MAX,
            retention_s: u64::MAX,
            retained_from_ms: u64::MAX,
            horizon_s: u64::MAX,
            object_bytes: u64::MAX,
            email: "off".into(),
            delivery_ok: false,
            due: u64::MAX,
            written: u64::MAX,
            emailed: u64::MAX,
            undelivered: u64::MAX,
            config_warnings: u64::MAX,
            last_write: Some(WriteNote::new(
                u64::MAX,
                "refused",
                &"E".repeat(2 * MAX_STRING_BYTES),
            )),
            firing_count: u64::MAX,
            firing_omitted: u64::MAX,
            ..Latest::default()
        };
        assert_eq!(l.body().len(), LATEST_FIXED_BYTES);
        l.fill((0..100).map(entry), u64::MAX);
        assert!(!l.firing.is_empty() && l.firing.len() < 100);
        assert_eq!(l.firing_omitted, u64::MAX - l.firing.len() as u64);
        assert!(
            l.body().len() <= LATEST_BYTES,
            "{} > {LATEST_BYTES}",
            l.body().len()
        );
        // latest.json's step fits its reservation: the firing rows as read
        // and their decoded entries (each within one post's events bound) and
        // the body.
        assert!((2 * POST_EVENTS_BYTES + LATEST_BYTES) as u64 <= STEP_BYTES);
        // A delete batch's step: each key's bytes and its String, the vector
        // of keys and the vector of names handed to the bucket.
        let string = std::mem::size_of::<String>();
        let name = std::mem::size_of::<&str>();
        assert!((DELETE_KEYS * (KEY_BYTES + string + name)) as u64 <= STEP_BYTES);
    }

    /// The retention sweep deletes old event objects only, batch by batch,
    /// and never `latest.json`.
    #[test]
    fn retention_never_deletes_latest() {
        let t = MemTable::default();
        let b = crate::arena::Budget::new(crate::arena::READBACK_BUDGET);
        let retention = retention_ms(2 * 86_400_000);
        assert_eq!(retention, 2 * 86_400_000 + 60_000);
        let base = 1_793_627_107_000u64;
        crate::block_on(t.feed_put(LATEST_KEY, b"{}".to_vec())).unwrap();
        // A forged record of latest.json is ignored by the sweep.
        t.feed_record(LATEST_KEY, base).unwrap();
        for i in 0..5u64 {
            let e = ev(EventKind::Firing, "r", &format!("s{i}"), "d");
            let mut o = Object::new(
                &e,
                "testnet",
                mime::Source {
                    name: mime::SOURCE_RELAY,
                    rules: crate::rules::RULES_ID,
                },
                0,
            );
            crate::block_on(write(&t, &mut o, base + i * 3_600_000)).unwrap();
        }
        assert_eq!(t.objects().len(), 6);
        // Two hours past the retention of the second object: the first two go.
        let now = base + 3_600_000 + retention + 1;
        // A budget without room for the step reads nothing and deletes none.
        let full = crate::arena::Budget::new(STEP_BYTES - 1);
        assert_eq!(
            crate::block_on(sweep(&t, &full, now, retention)).unwrap(),
            0
        );
        assert_eq!(t.objects().len(), 6);
        assert_eq!(crate::block_on(sweep(&t, &b, now, retention)).unwrap(), 2);
        assert_eq!(b.peak(), STEP_BYTES);
        let left = t.objects();
        assert_eq!(left.len(), 4);
        assert!(left.contains(&LATEST_KEY.to_owned()));
        assert_eq!(crate::block_on(sweep(&t, &b, now, retention)).unwrap(), 0);
        // Long after everything: every event object goes, latest.json stays.
        crate::block_on(sweep(&t, &b, u64::MAX, retention)).unwrap();
        assert_eq!(t.objects(), vec![LATEST_KEY.to_owned()]);
        assert_eq!(b.held(), 0);
    }

    /// Keys are stamped strictly increasing, so a lister reads objects in
    /// write order even within one millisecond or a clock step back.
    #[test]
    fn writes_list_in_order() {
        let t = MemTable::default();
        let base = 1_793_627_107_000u64;
        let mut ids = Vec::new();
        for (i, now) in [base, base, base - 5, base + 10].into_iter().enumerate() {
            let e = ev(EventKind::Firing, "r", &format!("s{i}"), "d");
            ids.push(e.id.clone());
            let mut o = Object::new(
                &e,
                "testnet",
                mime::Source {
                    name: mime::SOURCE_RELAY,
                    rules: crate::rules::RULES_ID,
                },
                0,
            );
            crate::block_on(write(&t, &mut o, now)).unwrap();
        }
        let listed: Vec<String> = t
            .objects()
            .iter()
            .map(|k| {
                let o: Object = serde_json::from_slice(&t.object(k).unwrap()).unwrap();
                o.event_id
            })
            .collect();
        assert_eq!(listed, ids, "lexicographic key order is write order");
    }

    /// The cron line in `wrangler.toml` is the period the feed's bounds use.
    #[test]
    fn cron_period_is_the_wrangler_schedule() {
        let toml = include_str!("../wrangler.toml");
        let testnet = toml.split("[env.testnet.triggers]").nth(1).unwrap();
        let line = testnet.lines().find(|l| l.starts_with("crons")).unwrap();
        assert_eq!(line.trim(), r#"crons = ["* * * * *"]"#);
        assert_eq!(RELAY_CRON_PERIOD_MS, 60_000);
        assert_eq!(PULL_PERIOD_MS, RELAY_CRON_PERIOD_MS);
    }
}
