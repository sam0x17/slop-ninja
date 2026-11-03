//! The relay's request logic, independent of the Workers runtime: a post from
//! the publisher, the publisher's heartbeat and the cron tick. `cf.rs` feeds
//! it the Durable Object's table, the `send_email` binding and the reads.

use serde::{Deserialize, Serialize};
use slop_ninja_alerts_wire::{
    AlertState, Event, Heartbeat, POST_EVENTS_BYTES, Post, RELAY_ANSWER_MS, Refusal, acknowledge,
    attempt, due, observe,
};

use crate::arena::{self, Budget};
use crate::config::Config;
use crate::ledger::{self, Claim, Outcome, StoreError, Table};
use crate::mime::{self, Message, Source};
use crate::rules::{self, HeartbeatRecord, Inputs, Intake};
use crate::{status, tracker};

pub use crate::mime::{SOURCE_PUBLISHER, SOURCE_RELAY};

/// A failed send, content-free: the error's code and its message with every
/// word that holds an address or a URL removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Failure {
    /// The error's `code` (for example `E_RECIPIENT_NOT_ALLOWED` or
    /// `RCPT_NOT_ALLOWED`), else its name, else `send_error`.
    pub code: String,
    pub message: String,
}

impl Failure {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code_text(code),
            message: message_text(message),
        }
    }
}

/// Why a send did not go out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendError {
    /// Nothing was sent: the binding refused it, or no time was left to send
    /// it within the answer bound.
    Refused(Failure),
    /// No answer within the bound: it may still go out.
    Timeout,
}

/// The `send_email` binding, or a recorder in tests.
#[allow(async_fn_in_trait)]
pub trait Mailer {
    async fn send(&self, m: &Message) -> Result<(), SendError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Every event is settled now: `sent` by this call, `duplicate` before,
    /// `unfired` resolutions of firings that never went out (nothing to
    /// resolve, so nothing is emailed).
    Delivered {
        sent: usize,
        duplicate: usize,
        unfired: usize,
    },
    /// Another sender holds a live claim on some events: retry later.
    Busy { sent: usize },
    /// The send failed or its outcome is unknown: retry later (the ledger
    /// keeps a timed-out send from going out more than twice).
    Failed { failure: Failure },
}

/// Emails the events not yet sent, once: claims each id, sends one message
/// with the claimed ones, then settles them in the ledger. A refused send
/// frees its claims; a send that timed out is retried once and then counts
/// as sent ([`crate::ledger`]). An unconfirmed resolution (its firing post
/// was never acknowledged to the sender) is emailed only when the ledger
/// shows its firing may have gone out, or no longer covers its episode.
pub async fn send_events(
    table: &impl Table,
    mailer: &impl Mailer,
    budget: &Budget,
    cfg: &Config,
    source: Source<'_>,
    events: &[Event],
    now_ms: u64,
) -> Result<Delivery, StoreError> {
    let mut mine: Vec<Event> = Vec::new();
    let mut retries: Vec<bool> = Vec::new();
    let (mut duplicate, mut busy, mut unfired) = (0, 0, 0);
    let mut seen = std::collections::BTreeSet::new();
    for e in events {
        if !seen.insert(e.id.as_str()) {
            continue;
        }
        if e.unconfirmed() && !ledger::maybe_sent(table, &e.firing_id(source.rules), e.since_ms)? {
            unfired += 1;
            continue;
        }
        match ledger::claim(table, &e.id, now_ms)? {
            Claim::Mine { retry } => {
                mine.push(e.clone());
                retries.push(retry);
            }
            Claim::Sent => duplicate += 1,
            Claim::Busy => busy += 1,
        }
    }
    if mine.is_empty() {
        return Ok(if busy > 0 {
            Delivery::Busy { sent: 0 }
        } else {
            Delivery::Delivered {
                sent: 0,
                duplicate,
                unfired,
            }
        });
    }
    let settle = |outcome: Outcome| -> Result<(), StoreError> {
        for (e, retry) in mine.iter().zip(&retries) {
            ledger::settle(table, &e.id, *retry, outcome, now_ms)?;
        }
        Ok(())
    };
    let Ok(_hold) = budget.reserve(arena::message_footprint(arena::text_bound())) else {
        settle(Outcome::Refused)?;
        return Ok(Delivery::Failed {
            failure: Failure::new("budget", ""),
        });
    };
    let message = mime::build(&cfg.addresses, &mine, &cfg.network, source, now_ms);
    match mailer.send(&message).await {
        Ok(()) => {
            settle(Outcome::Sent)?;
            let sent = mine.len();
            Ok(if busy > 0 {
                Delivery::Busy { sent }
            } else {
                Delivery::Delivered {
                    sent,
                    duplicate,
                    unfired,
                }
            })
        }
        Err(SendError::Timeout) => {
            settle(Outcome::TimedOut)?;
            Ok(Delivery::Failed {
                failure: Failure::new("timeout", ""),
            })
        }
        Err(SendError::Refused(failure)) => {
            settle(Outcome::Refused)?;
            Ok(Delivery::Failed { failure })
        }
    }
}

/// An error code for logs and answers: ASCII letters, digits and `_`.
pub fn code_text(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}

/// An error message for logs: printable ASCII, without any word that holds
/// an address (`@`) or a URL (`://`), so a recipient named in a refusal
/// never reaches a log line. Quotes become apostrophes (the message is
/// embedded in JSON answers).
pub fn message_text(message: &str) -> String {
    message
        .split_whitespace()
        .filter(|w| !w.contains('@') && !w.contains("://"))
        .map(|w| {
            w.chars()
                .map(|c| match c {
                    '"' | '\\' => '\'',
                    c if c.is_ascii_graphic() => c,
                    _ => '?',
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The relay's answer to one post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub body: String,
    /// The post was refused for its content (a 4xx): the relay records it
    /// ([`Intake`]), and its rules email when refusals persist.
    pub refusal: Option<&'static str>,
}

/// A publisher post. 200 only when every event in it is settled (sent now or
/// before, or an unconfirmed resolution with nothing to resolve), so the
/// publisher acknowledges exactly then.
pub async fn handle_post(
    body: &[u8],
    cfg: &Config,
    table: &impl Table,
    mailer: &impl Mailer,
    budget: &Budget,
    now_ms: u64,
) -> Answer {
    let post = match Post::parse(body, &cfg.network) {
        Ok(p) => p,
        Err(r) => {
            return Answer {
                status: if r == Refusal::TooLarge { 413 } else { 400 },
                body: format!(r#"{{"error":"{}"}}"#, r.code()),
                refusal: Some(r.code()),
            };
        }
    };
    let source = Source {
        name: SOURCE_PUBLISHER,
        rules: &post.rules,
    };
    let (status, body) =
        match send_events(table, mailer, budget, cfg, source, &post.events, now_ms).await {
            Ok(Delivery::Delivered {
                sent,
                duplicate,
                unfired,
            }) => (
                200,
                format!(r#"{{"sent":{sent},"duplicate":{duplicate},"unfired":{unfired}}}"#),
            ),
            Ok(Delivery::Busy { .. }) => (503, r#"{"error":"busy"}"#.into()),
            Ok(Delivery::Failed { failure }) => (
                503,
                format!(
                    r#"{{"error":"send_failed","code":"{}","message":"{}"}}"#,
                    failure.code, failure.message
                ),
            ),
            Err(StoreError(code)) => (503, format!(r#"{{"error":"store","code":"{code}"}}"#)),
        };
    Answer {
        status,
        body,
        refusal: None,
    }
}

/// A heartbeat, recorded at the relay's clock.
pub fn heartbeat(body: &[u8], network: &str, now_ms: u64) -> Result<HeartbeatRecord, Refusal> {
    let h = Heartbeat::parse(body, network)?;
    Ok(HeartbeatRecord {
        received_ms: now_ms,
        run_started_ms: h.run_started_ms,
        renotify_ms: h.renotify_ms,
        watermark_stall_ms: h.watermark_stall_ms,
        config_refused: h.config_refused,
        rules: h.rules,
        post_schema: h.post_schema,
        undelivered: h.undelivered,
        undelivered_since_ms: h.undelivered_since_ms,
    })
}

/// The relay's own alert state (its Durable Object's `state` row).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickState {
    pub alerts: AlertState,
    /// The relay's first tick: the publisher's silence counts from here
    /// until its first heartbeat, and the bucket's from here until it first
    /// answers.
    pub watch_since_ms: Option<u64>,
    /// The last tick whose tracker read got an answer from the bucket (an
    /// object, a missing object or an unreadable one).
    #[serde(default)]
    pub tracker_answered_ms: Option<u64>,
    /// The schedule period the tracker's last readable upload stated.
    #[serde(default)]
    pub tracker_period_ms: Option<u64>,
    /// The first tick of the current series whose sends failed (not busy):
    /// the email path is failing. Cleared by a tick that finishes without a
    /// failed send.
    #[serde(default)]
    pub send_failing_since_ms: Option<u64>,
}

/// What one tick did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    pub due: usize,
    pub sent: usize,
    pub undelivered: usize,
    pub failure: Option<Failure>,
    /// Another tick took the lease over: this one stopped before its next
    /// batch.
    pub stopped: bool,
    /// The tick finished, and its sends have not been failing for the
    /// undelivered hold (two cron periods): the email path works as far as
    /// the relay can tell, and the external dead-man may be pinged. An idle
    /// relay (nothing due) proves only that its cron runs.
    pub email_ok: bool,
}

/// What a tick needs from its Durable Object besides the ledger: its lease
/// and its state row.
pub trait TickHooks {
    /// Extends this tick's lease by `ms` from now, before a step bounded by
    /// `ms`; `false` when another tick holds the lease (stop).
    fn renew(&self, ms: u64) -> bool;
    /// Saves the state while this tick still holds the lease (an error
    /// otherwise). Called once the conditions are observed, before each
    /// batch (its attempt marks) and after each delivered batch, in the same
    /// synchronous step as the ledger's records, so a tick that dies later
    /// loses no acknowledgment.
    fn save(&self, st: &TickState) -> Result<(), StoreError>;
}

/// The relay's events split into posts' worth (each event fitted to a post),
/// so a tick's message stays within the budgeted bound.
pub fn batches(events: &[Event]) -> Vec<Vec<Event>> {
    let mut out: Vec<Vec<Event>> = Vec::new();
    let mut size = 0usize;
    for e in events {
        let mut e = e.clone();
        e.fit(POST_EVENTS_BYTES - 1);
        let n = e.wire_len() + 1;
        match out.last_mut() {
            Some(b) if size + n <= POST_EVENTS_BYTES => {
                b.push(e);
                size += n;
            }
            _ => {
                out.push(vec![e]);
                size = n;
            }
        }
    }
    out
}

/// Everything one tick read before it evaluates.
pub struct TickReads<'a> {
    pub heartbeat: Option<&'a HeartbeatRecord>,
    pub intake: &'a Intake,
    pub tracker: &'a tracker::Read,
    pub workers: Option<&'a [status::Read; 2]>,
}

/// One cron tick over what the Durable Object read: evaluates the rules,
/// emails what is due through the ledger, batch by batch under the renewed
/// lease, acknowledges what went out and sweeps the ledger.
#[allow(clippy::too_many_arguments)]
pub async fn tick(
    st: &mut TickState,
    cfg: &Config,
    reads: &TickReads<'_>,
    table: &impl Table,
    mailer: &impl Mailer,
    budget: &Budget,
    hooks: &impl TickHooks,
    now_ms: u64,
) -> Result<TickReport, StoreError> {
    let watch_since_ms = *st.watch_since_ms.get_or_insert(now_ms);
    if !matches!(reads.tracker, tracker::Read::Unavailable) {
        st.tracker_answered_ms = Some(now_ms);
    }
    if let tracker::Read::Ok { upload, .. } = reads.tracker
        && let Some(period) = upload.period_ms()
    {
        st.tracker_period_ms = Some(period);
    }
    // The publisher's watermark bound from its heartbeat, the tracker's
    // period from its upload; the configured fallbacks until they arrive.
    let bounds = cfg.bounds(
        reads.heartbeat.map(|h| h.watermark_stall_ms),
        st.tracker_period_ms,
    );
    let warnings: Vec<String> = cfg.warnings.iter().map(ToString::to_string).collect();
    let inputs = Inputs {
        now_ms,
        watch_since_ms,
        heartbeat: reads.heartbeat,
        intake: reads.intake,
        tracker: reads.tracker,
        tracker_answered_ms: st.tracker_answered_ms.unwrap_or(watch_since_ms),
        workers: reads.workers,
        spend_figures: cfg.spend_figures,
        config_warnings: &warnings,
    };
    let conditions = rules::conditions(&inputs, &bounds, &st.alerts);
    observe(&mut st.alerts, rules::RULES_ID, &conditions, now_ms);
    rules::resolution_notes(&mut st.alerts, &inputs);
    let renotify = rules::renotify_ms(reads.heartbeat);
    ledger::sweep(table, now_ms, ledger::retention_ms(renotify))?;
    let events = due(&st.alerts, now_ms, renotify);
    hooks.save(st)?;
    let mut report = TickReport {
        due: events.len(),
        ..TickReport::default()
    };
    let source = Source {
        name: SOURCE_RELAY,
        rules: rules::RULES_ID,
    };
    for batch in batches(&events) {
        if !hooks.renew(RELAY_ANSWER_MS) {
            report.stopped = true;
            break;
        }
        attempt(&mut st.alerts, &batch, now_ms);
        hooks.save(st)?;
        match send_events(table, mailer, budget, cfg, source, &batch, now_ms).await? {
            Delivery::Delivered { sent, .. } => {
                report.sent += sent;
                acknowledge(&mut st.alerts, &batch, now_ms);
                hooks.save(st)?;
            }
            Delivery::Busy { sent } => {
                report.sent += sent;
                report.undelivered += batch.len() - sent;
            }
            Delivery::Failed { failure } => {
                report.undelivered += batch.len();
                report.failure = Some(failure);
            }
        }
    }
    // The email path: a failed send starts (or continues) a failing series,
    // a tick that finished without one ends it; a stopped tick says nothing.
    let failing = match (&report.failure, report.stopped) {
        (Some(_), _) => Some(st.send_failing_since_ms.unwrap_or(now_ms)),
        (None, true) => st.send_failing_since_ms,
        (None, false) => None,
    };
    if failing != st.send_failing_since_ms {
        st.send_failing_since_ms = failing;
        hooks.save(st)?;
    }
    report.email_ok = !report.stopped
        && failing.is_none_or(|since| now_ms.saturating_sub(since) < bounds.undelivered_hold_ms);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::{BASE, vars};
    use crate::ledger::{CLAIM_MS, Mark, MemTable};
    use crate::mime::tests::{RULES, ev, qp_decode};
    use crate::rules::tests::{T0, beat, fresh};
    use slop_ninja_alerts_wire::{EventKind, POST_SCHEMA, post_bound};
    use std::cell::RefCell;

    /// Records each message; answers from `script` in order, then Ok.
    #[derive(Default)]
    struct Recorder {
        sent: RefCell<Vec<Message>>,
        script: RefCell<Vec<Result<(), SendError>>>,
    }

    impl Recorder {
        fn scripted(answers: Vec<Result<(), SendError>>) -> Self {
            Self {
                sent: RefCell::default(),
                script: RefCell::new(answers.into_iter().rev().collect()),
            }
        }
        fn count(&self) -> usize {
            self.sent.borrow().len()
        }
        fn subjects(&self) -> Vec<String> {
            self.sent
                .borrow()
                .iter()
                .map(|m| m.subject.clone())
                .collect()
        }
    }

    impl Mailer for Recorder {
        async fn send(&self, m: &Message) -> Result<(), SendError> {
            self.sent.borrow_mut().push(m.clone());
            self.script.borrow_mut().pop().unwrap_or(Ok(()))
        }
    }

    /// The tick's lease and state row, in memory: `lost` makes every renewal
    /// and save fail (another tick took the lease over).
    #[derive(Default)]
    struct Hooks {
        saved: RefCell<Option<TickState>>,
        saves: RefCell<usize>,
        lost: bool,
    }

    impl TickHooks for Hooks {
        fn renew(&self, _ms: u64) -> bool {
            !self.lost
        }
        fn save(&self, st: &TickState) -> Result<(), StoreError> {
            if self.lost {
                return Err(StoreError("lease lost"));
            }
            *self.saves.borrow_mut() += 1;
            *self.saved.borrow_mut() = Some(st.clone());
            Ok(())
        }
    }

    fn cfg() -> Config {
        crate::config::parse(vars(&BASE)).unwrap()
    }

    fn post(events: Vec<Event>) -> Vec<u8> {
        Post {
            schema: POST_SCHEMA.into(),
            rules: RULES.into(),
            network: "testnet".into(),
            events,
        }
        .body()
    }

    fn budget() -> Budget {
        Budget::new(arena::READBACK_BUDGET)
    }

    fn answer(a: Answer) -> (u16, String) {
        (a.status, a.body)
    }

    use crate::block_on;

    /// A retried post (its answer was lost) sends one email; a partly new
    /// post sends only its new events.
    #[test]
    fn dedupe_by_event_id() {
        let (t, m, b, c) = (MemTable::default(), Recorder::default(), budget(), cfg());
        let a = ev(EventKind::Firing, "commit_gap", "coordinator", "d");
        let body = post(vec![a.clone()]);
        assert_eq!(
            answer(block_on(handle_post(&body, &c, &t, &m, &b, T0))),
            (200, r#"{"sent":1,"duplicate":0,"unfired":0}"#.into())
        );
        assert_eq!(
            answer(block_on(handle_post(&body, &c, &t, &m, &b, T0 + 60_000))),
            (200, r#"{"sent":0,"duplicate":1,"unfired":0}"#.into())
        );
        assert_eq!(m.count(), 1, "one email for the retried post");
        let z = ev(EventKind::Resolved, "commit_gap", "coordinator", "d");
        let a2 = block_on(handle_post(
            &post(vec![a, z.clone(), z]),
            &c,
            &t,
            &m,
            &b,
            T0 + 120_000,
        ));
        assert_eq!(a2.status, 200);
        assert_eq!(m.count(), 2);
        let text = qp_decode(m.sent.borrow()[1].raw.split_once("\r\n\r\n").unwrap().1);
        assert!(text.contains("[RESOLVED]") && !text.contains("[FIRING]"));
        assert_eq!(b.held(), 0);
    }

    /// A refused send frees its claim, so the publisher's retry sends; a
    /// timed-out send keeps it (busy) until the claim expires, is retried
    /// once, and is recorded as sent after its second timeout.
    #[test]
    fn refused_and_timed_out_sends() {
        let (t, b, c) = (MemTable::default(), budget(), cfg());
        let body = post(vec![ev(EventKind::Firing, "r", "s", "d")]);
        let m = Recorder::scripted(vec![Err(SendError::Refused(Failure::new(
            "E_RECIPIENT_NOT_ALLOWED",
            "recipient ops@example.invalid is not a verified destination",
        )))]);
        let a = block_on(handle_post(&body, &c, &t, &m, &b, T0));
        assert_eq!((a.status, a.refusal), (503, None));
        assert!(a.body.contains("E_RECIPIENT_NOT_ALLOWED"));
        assert!(a.body.contains("not a verified destination"));
        assert!(!a.body.contains('@'), "{}", a.body);
        assert_eq!(
            block_on(handle_post(&body, &c, &t, &m, &b, T0 + 60_000)).status,
            200
        );
        assert_eq!(m.count(), 2);
        let body = post(vec![ev(EventKind::Reminder, "r", "s", "d")]);
        let m = Recorder::scripted(vec![Err(SendError::Timeout)]);
        assert_eq!(block_on(handle_post(&body, &c, &t, &m, &b, T0)).status, 503);
        assert_eq!(
            block_on(handle_post(&body, &c, &t, &m, &b, T0 + CLAIM_MS - 1)).body,
            r#"{"error":"busy"}"#
        );
        assert_eq!(
            block_on(handle_post(&body, &c, &t, &m, &b, T0 + CLAIM_MS)).status,
            200
        );
        assert_eq!(m.count(), 2, "at least once after an unknown outcome");
    }

    /// A send path that answers slowly but delivers (every send times out)
    /// emails an event at most twice, not once per cron period.
    #[test]
    fn timeouts_resend_at_most_once() {
        let (t, b, c) = (MemTable::default(), budget(), cfg());
        let body = post(vec![ev(
            EventKind::Firing,
            "commit_gap",
            "coordinator",
            "d",
        )]);
        let m = Recorder::scripted((0..60).map(|_| Err(SendError::Timeout)).collect());
        let mut answers = Vec::new();
        for n in 0..60 {
            answers.push(block_on(handle_post(&body, &c, &t, &m, &b, T0 + n * CLAIM_MS)).status);
        }
        assert_eq!(m.count(), 2, "{:?}", m.subjects());
        assert_eq!(&answers[..3], &[503, 503, 200]);
        assert!(answers[2..].iter().all(|s| *s == 200));
        let id = ev(EventKind::Firing, "commit_gap", "coordinator", "d").id;
        assert_eq!(t.get(&id).unwrap().map(|r| r.mark), Some(Mark::Sent));
    }

    /// An unconfirmed resolution (the publisher never had its firing
    /// acknowledged) is emailed when the firing went out, and settled
    /// without an email when it never did.
    #[test]
    fn unconfirmed_resolutions_follow_their_firing() {
        let (t, b, c) = (MemTable::default(), budget(), cfg());
        let f = Event::new(
            RULES,
            EventKind::Firing,
            "commit_gap",
            "coordinator",
            "d",
            5,
            None,
        );
        let z = Event::new(
            RULES,
            EventKind::Resolved,
            "commit_gap",
            "coordinator",
            "d",
            5,
            None,
        );
        // The firing went out (the publisher's wait ended before the answer).
        let m = Recorder::default();
        assert_eq!(
            block_on(handle_post(&post(vec![f]), &c, &t, &m, &b, T0)).status,
            200
        );
        let a = block_on(handle_post(
            &post(vec![z.clone()]),
            &c,
            &t,
            &m,
            &b,
            T0 + 60_000,
        ));
        assert_eq!(a.status, 200);
        assert_eq!(
            m.subjects(),
            vec![
                "[slop-ninja testnet] FIRING commit_gap coordinator",
                "[slop-ninja testnet] RESOLVED commit_gap coordinator"
            ]
        );
        // A firing that never went out: its resolution sends nothing.
        let (t, m) = (MemTable::default(), Recorder::default());
        assert_eq!(
            answer(block_on(handle_post(&post(vec![z]), &c, &t, &m, &b, T0))),
            (200, r#"{"sent":0,"duplicate":0,"unfired":1}"#.into())
        );
        assert_eq!(m.count(), 0);
    }

    /// Oversized, foreign or malformed posts are refused before any send,
    /// and say why (the relay records refusals).
    #[test]
    fn refusals_send_nothing() {
        let (t, m, b, c) = (MemTable::default(), Recorder::default(), budget(), cfg());
        let big = vec![b'x'; post_bound() + 1];
        let a = block_on(handle_post(&big, &c, &t, &m, &b, T0));
        assert_eq!((a.status, a.refusal), (413, Some("too_large")));
        let a = block_on(handle_post(b"{}", &c, &t, &m, &b, T0));
        assert_eq!((a.status, a.refusal), (400, Some("malformed")));
        let mut p: serde_json::Value =
            serde_json::from_slice(&post(vec![ev(EventKind::Firing, "r", "s", "d")])).unwrap();
        p["network"] = "finney".into();
        assert_eq!(
            answer(block_on(handle_post(
                &serde_json::to_vec(&p).unwrap(),
                &c,
                &t,
                &m,
                &b,
                T0
            ))),
            (400, r#"{"error":"network"}"#.into())
        );
        // An id that its event's fields do not derive.
        let mut forged = ev(EventKind::Firing, "r", "s", "d");
        forged.subject = "t".into();
        let a = block_on(handle_post(&post(vec![forged]), &c, &t, &m, &b, T0));
        assert_eq!((a.status, a.refusal), (400, Some("event_id")));
        assert_eq!(m.count(), 0);
        // A budget that cannot hold the message frees the claims.
        let tiny = Budget::new(1);
        let body = post(vec![ev(EventKind::Firing, "r", "s", "d")]);
        assert_eq!(
            block_on(handle_post(&body, &c, &t, &m, &tiny, T0)).status,
            503
        );
        assert_eq!(
            block_on(handle_post(&body, &c, &t, &m, &b, T0 + 1)).status,
            200
        );
    }

    #[test]
    fn heartbeats_are_recorded_at_the_relay_clock() {
        let h = Heartbeat {
            schema: slop_ninja_alerts_wire::HEARTBEAT_SCHEMA.into(),
            network: "testnet".into(),
            rules: RULES.into(),
            post_schema: POST_SCHEMA.into(),
            run_started_ms: 5,
            renotify_ms: 60_000,
            watermark_stall_ms: 420_000,
            config_refused: false,
            undelivered: 2,
            undelivered_since_ms: Some(4),
        };
        let r = heartbeat(&h.body(), "testnet", T0).unwrap();
        assert_eq!((r.received_ms, r.renotify_ms), (T0, 60_000));
        assert_eq!((r.undelivered, r.undelivered_since_ms), (2, Some(4)));
        assert_eq!(heartbeat(&h.body(), "finney", T0), Err(Refusal::Network));
    }

    fn reads<'a>(
        hb: Option<&'a HeartbeatRecord>,
        intake: &'a Intake,
        tr: &'a tracker::Read,
    ) -> TickReads<'a> {
        TickReads {
            heartbeat: hb,
            intake,
            tracker: tr,
            workers: None,
        }
    }

    /// Ticks: a spend alert emails once; a tick that overlaps it (the same
    /// starting state, the same due events) sends nothing more; the reminder
    /// follows the publisher's renotify interval from its heartbeat; the
    /// state is saved after each delivered batch.
    #[test]
    fn ticks_email_once_and_overlaps_are_deduplicated() {
        let (t, m, b, c) = (MemTable::default(), Recorder::default(), budget(), cfg());
        let h = Hooks::default();
        let mut hb = beat(T0);
        hb.renotify_ms = 3_600_000;
        let intake = Intake::default();
        let tr = fresh(&["2026-11"]);
        let mut st = TickState::default();
        let before = st.clone();
        let r = block_on(tick(
            &mut st,
            &c,
            &reads(Some(&hb), &intake, &tr),
            &t,
            &m,
            &b,
            &h,
            T0,
        ))
        .unwrap();
        assert_eq!((r.due, r.sent, r.undelivered), (1, 1, 0));
        assert_eq!(h.saved.borrow().as_ref(), Some(&st), "saved once delivered");
        assert_eq!(*h.saves.borrow(), 3, "observed, attempted, acknowledged");
        let msg = m.sent.borrow()[0].clone();
        assert_eq!(
            msg.subject,
            "[slop-ninja testnet] FIRING spend_alert 2026-11"
        );
        // An overlapping tick from the same starting state: already sent.
        let mut other = before;
        let r = block_on(tick(
            &mut other,
            &c,
            &reads(Some(&hb), &intake, &tr),
            &t,
            &m,
            &b,
            &h,
            T0,
        ))
        .unwrap();
        assert_eq!((r.due, r.sent), (1, 0));
        assert!(
            other
                .alerts
                .get("spend_alert", "2026-11")
                .unwrap()
                .notified_ms
                .is_some(),
            "acknowledged as sent"
        );
        assert_eq!(m.count(), 1);
        // Nothing new, then a reminder after the publisher's interval.
        let hb2 = HeartbeatRecord {
            received_ms: T0 + 3_600_000,
            ..hb.clone()
        };
        let r2 = reads(Some(&hb2), &intake, &tr);
        assert_eq!(
            block_on(tick(&mut st, &c, &r2, &t, &m, &b, &h, T0 + 60_000))
                .unwrap()
                .due,
            0
        );
        let r = block_on(tick(&mut st, &c, &r2, &t, &m, &b, &h, T0 + 3_600_000)).unwrap();
        assert_eq!((r.due, r.sent), (1, 1));
        assert!(m.sent.borrow()[1].subject.contains("STILL FIRING"));
        assert_eq!(st.watch_since_ms, Some(T0));
        assert_eq!(st.tracker_answered_ms, Some(T0 + 3_600_000));
        assert_eq!(b.held(), 0);
    }

    /// A failed tick send stays due and is retried by the next tick; a tick
    /// that lost its lease stops before sending.
    #[test]
    fn failed_tick_sends_are_retried() {
        let (t, b, c) = (MemTable::default(), budget(), cfg());
        let h = Hooks::default();
        let m = Recorder::scripted(vec![Err(SendError::Refused(Failure::new(
            "E_DELIVERY_FAILED",
            "",
        )))]);
        let tr = tracker::Read::Missing;
        let intake = Intake::default();
        let mut st = TickState::default();
        let r = block_on(tick(
            &mut st,
            &c,
            &reads(None, &intake, &tr),
            &t,
            &m,
            &b,
            &h,
            T0,
        ))
        .unwrap();
        assert_eq!((r.due, r.sent, r.undelivered), (1, 0, 1));
        assert_eq!(
            r.failure.map(|f| f.code).as_deref(),
            Some("E_DELIVERY_FAILED")
        );
        let r = block_on(tick(
            &mut st,
            &c,
            &reads(None, &intake, &tr),
            &t,
            &m,
            &b,
            &h,
            T0 + 60_000,
        ))
        .unwrap();
        assert_eq!((r.due, r.sent), (1, 1));
        let lost = Hooks {
            lost: true,
            ..Hooks::default()
        };
        let mut st = TickState::default();
        let m = Recorder::default();
        assert_eq!(
            block_on(tick(
                &mut st,
                &c,
                &reads(None, &intake, &tr),
                &t,
                &m,
                &b,
                &lost,
                T0
            )),
            Err(StoreError("lease lost"))
        );
        assert_eq!(m.count(), 0);
    }

    /// The relay's own alert whose send timed out, then clears, still gets
    /// its resolution emailed (the firing may have gone out).
    #[test]
    fn a_timed_out_tick_firing_is_resolved() {
        let (t, b, c) = (MemTable::default(), budget(), cfg());
        let h = Hooks::default();
        let m = Recorder::scripted(vec![Err(SendError::Timeout)]);
        let intake = Intake::default();
        let mut st = TickState::default();
        let missing = tracker::Read::Missing;
        let hb = beat(T0);
        let r = block_on(tick(
            &mut st,
            &c,
            &reads(Some(&hb), &intake, &missing),
            &t,
            &m,
            &b,
            &h,
            T0,
        ))
        .unwrap();
        assert_eq!((r.sent, r.undelivered), (0, 1));
        let back = fresh(&[]);
        let r = block_on(tick(
            &mut st,
            &c,
            &reads(Some(&hb), &intake, &back),
            &t,
            &m,
            &b,
            &h,
            T0 + 60_000,
        ))
        .unwrap();
        assert_eq!(r.sent, 1);
        assert_eq!(
            m.subjects(),
            vec![
                "[slop-ninja testnet] FIRING spend_tracker_stale tracker",
                "[slop-ninja testnet] RESOLVED spend_tracker_stale tracker"
            ]
        );
        assert!(st.alerts.entries.is_empty());
    }

    #[test]
    fn relay_batches_fit_a_post() {
        let many: Vec<Event> = (0..800)
            .map(|i| {
                ev(
                    EventKind::Firing,
                    "spend_alert",
                    &format!("m{i}"),
                    &"d".repeat(200),
                )
            })
            .collect();
        let b = batches(&many);
        assert!(b.len() > 1);
        assert_eq!(b.iter().map(Vec::len).sum::<usize>(), many.len());
        for batch in &b {
            assert!(batch.iter().map(|e| e.wire_len() + 1).sum::<usize>() <= POST_EVENTS_BYTES);
        }
    }

    #[test]
    fn log_text_drops_addresses_and_urls() {
        assert_eq!(
            message_text(
                "Recipient \"ops@example.invalid\" not verified; see https://x.invalid/a\nnow"
            ),
            "Recipient not verified; see now"
        );
        assert_eq!(
            code_text("E_RECIPIENT_NOT_ALLOWED\r\n"),
            "E_RECIPIENT_NOT_ALLOWED"
        );
        assert_eq!(message_text("a \"b\" c\\d é"), "a 'b' c'd ?");
    }

    fn cfg_with(extra: &[(&'static str, &'static str)]) -> Config {
        let mut p = BASE.to_vec();
        p.extend_from_slice(extra);
        crate::config::parse(vars(&p)).unwrap()
    }

    /// A typo in an optional setting never stops the relay: posts and ticks
    /// still send, and the tick emails which settings it ignored.
    #[test]
    fn bad_optional_settings_still_send() {
        let c = cfg_with(&[
            ("SPEND_FIGURES_IN_EMAIL", "yes"),
            ("HEARTBEAT_ENABLED", "1"),
            ("TRACKER_PERIOD_SECS", "1d"),
        ]);
        assert_eq!(c.warnings.len(), 3);
        let (t, m, b, h) = (
            MemTable::default(),
            Recorder::default(),
            budget(),
            Hooks::default(),
        );
        let body = post(vec![ev(
            EventKind::Firing,
            "commit_gap",
            "coordinator",
            "d",
        )]);
        assert_eq!(block_on(handle_post(&body, &c, &t, &m, &b, T0)).status, 200);
        let hb = beat(T0);
        let (intake, tr) = (Intake::default(), fresh(&[]));
        let mut st = TickState::default();
        let r = block_on(tick(
            &mut st,
            &c,
            &reads(Some(&hb), &intake, &tr),
            &t,
            &m,
            &b,
            &h,
            T0,
        ))
        .unwrap();
        assert_eq!((r.sent, r.email_ok), (1, true));
        assert_eq!(
            m.subjects(),
            vec![
                "[slop-ninja testnet] FIRING commit_gap coordinator",
                "[slop-ninja testnet] FIRING relay_config_warning relay"
            ]
        );
        let text = qp_decode(m.sent.borrow()[1].raw.split_once("\r\n\r\n").unwrap().1);
        let text = text.replace("\r\n", " ");
        for name in [
            "SPEND_FIGURES_IN_EMAIL",
            "HEARTBEAT_ENABLED",
            "TRACKER_PERIOD_SECS",
        ] {
            assert!(text.contains(name), "{text}");
        }
        assert!(
            !text.contains("yes") && !text.contains("1d"),
            "no values: {text}"
        );
    }

    /// A resolution whose send timed out (it may be in the mailbox), then
    /// the condition comes back: a new firing is emailed at once, not one
    /// reminder a renotify interval later.
    #[test]
    fn a_recurrence_after_a_lost_resolution_fires_again() {
        let (t, b, c, h) = (MemTable::default(), budget(), cfg(), Hooks::default());
        let m = Recorder::scripted(vec![Ok(()), Err(SendError::Timeout)]);
        let intake = Intake::default();
        let (missing, back) = (tracker::Read::Missing, fresh(&[]));
        let mut st = TickState::default();
        let mut at = T0;
        for tr in [&missing, &back] {
            let hb = beat(at);
            block_on(tick(
                &mut st,
                &c,
                &reads(Some(&hb), &intake, tr),
                &t,
                &m,
                &b,
                &h,
                at,
            ))
            .unwrap();
            at += 60_000;
        }
        let mut due_total = 0;
        for _ in 0..360 {
            let hb = beat(at);
            let r = block_on(tick(
                &mut st,
                &c,
                &reads(Some(&hb), &intake, &missing),
                &t,
                &m,
                &b,
                &h,
                at,
            ))
            .unwrap();
            due_total += r.due;
            at += 60_000;
        }
        assert_eq!(
            m.subjects(),
            vec![
                "[slop-ninja testnet] FIRING spend_tracker_stale tracker",
                "[slop-ninja testnet] RESOLVED spend_tracker_stale tracker",
                "[slop-ninja testnet] FIRING spend_tracker_stale tracker"
            ]
        );
        assert_eq!(
            due_total, 1,
            "one new firing, then quiet until the reminder"
        );
    }

    /// The external ping means the email path works: sends that keep
    /// failing for the undelivered hold withhold it, a tick that finishes
    /// without a failure restores it, and a stopped tick never pings.
    #[test]
    fn the_ping_follows_the_email_path() {
        let (t, b, c, h) = (MemTable::default(), budget(), cfg(), Hooks::default());
        let refused = || {
            Err(SendError::Refused(Failure::new(
                "E_RECIPIENT_NOT_ALLOWED",
                "",
            )))
        };
        let m = Recorder::scripted(vec![refused(), refused(), refused()]);
        let intake = Intake::default();
        let tr = tracker::Read::Missing;
        let mut st = TickState::default();
        let mut ok = Vec::new();
        for n in 0..4 {
            let now = T0 + n * 60_000;
            let hb = beat(now);
            let r = block_on(tick(
                &mut st,
                &c,
                &reads(Some(&hb), &intake, &tr),
                &t,
                &m,
                &b,
                &h,
                now,
            ))
            .unwrap();
            ok.push((r.email_ok, r.failure.is_some()));
        }
        assert_eq!(
            ok,
            vec![(true, true), (true, true), (false, true), (true, false)],
            "failing for two cron periods withholds the ping; the next send restores it"
        );
        assert_eq!(st.send_failing_since_ms, None);
        // Idle ticks (nothing due) ping.
        let hb = beat(T0 + 300_000);
        let r = block_on(tick(
            &mut st,
            &c,
            &reads(Some(&hb), &intake, &tr),
            &t,
            &m,
            &b,
            &h,
            T0 + 300_000,
        ))
        .unwrap();
        assert_eq!((r.due, r.email_ok), (0, true));
        // A tick that lost its lease before a batch does not ping.
        let lost = LeaseLostAtBatch;
        let mut fresh_state = TickState::default();
        let r = block_on(tick(
            &mut fresh_state,
            &c,
            &reads(Some(&hb), &intake, &tr),
            &MemTable::default(),
            &Recorder::default(),
            &b,
            &lost,
            T0,
        ))
        .unwrap();
        assert!(r.stopped && !r.email_ok);
    }

    /// Saves succeed, renewals fail: another tick took the lease between
    /// the observation and the first batch.
    struct LeaseLostAtBatch;

    impl TickHooks for LeaseLostAtBatch {
        fn renew(&self, _ms: u64) -> bool {
            false
        }
        fn save(&self, _st: &TickState) -> Result<(), StoreError> {
            Ok(())
        }
    }

    /// With the shortest renotify interval, a send that times out every
    /// time but delivers is still emailed at most twice: the ledger keeps
    /// unknown outcomes for the publisher's retry window.
    #[test]
    fn unknown_outcomes_survive_the_shortest_renotify() {
        let (t, b, c) = (MemTable::default(), budget(), cfg());
        let body = post(vec![ev(
            EventKind::Firing,
            "commit_gap",
            "coordinator",
            "d",
        )]);
        let m = Recorder::scripted((0..20).map(|_| Err(SendError::Timeout)).collect());
        for n in 0..20 {
            let now = T0 + n * 60_001;
            ledger::sweep(&t, now, ledger::retention_ms(60_000)).unwrap();
            block_on(handle_post(&body, &c, &t, &m, &b, now));
        }
        assert_eq!(m.count(), 2, "{:?}", m.subjects());
    }

    /// After an outage longer than the ledger's retention, the firing's row
    /// is gone; its unconfirmed resolution is still emailed (the ledger no
    /// longer covers the episode), with the note that the firing may not
    /// have gone out.
    #[test]
    fn a_resolution_after_a_long_outage_is_emailed() {
        let (t, b, c, m) = (MemTable::default(), budget(), cfg(), Recorder::default());
        let since = T0 - 60_000;
        let f = Event::new(
            RULES,
            EventKind::Firing,
            "commit_gap",
            "coordinator",
            "d",
            since,
            None,
        );
        let z = Event::new(
            RULES,
            EventKind::Resolved,
            "commit_gap",
            "coordinator",
            "d",
            since,
            None,
        );
        assert_eq!(
            block_on(handle_post(&post(vec![f]), &c, &t, &m, &b, T0)).status,
            200
        );
        let renotify = 6 * 3_600_000;
        let later = T0 + renotify + 120_000;
        ledger::sweep(&t, later, ledger::retention_ms(renotify)).unwrap();
        assert!(t.is_empty(), "the firing's row was swept");
        assert_eq!(
            answer(block_on(handle_post(&post(vec![z]), &c, &t, &m, &b, later))),
            (200, r#"{"sent":1,"duplicate":0,"unfired":0}"#.into())
        );
        let text = qp_decode(m.sent.borrow()[1].raw.split_once("\r\n\r\n").unwrap().1);
        assert!(
            text.replace("\r\n", " ").contains("may not have gone out"),
            "{text}"
        );
    }

    /// The bounds come from the senders: the publisher's watermark bound
    /// from its heartbeat, the tracker's period from its upload.
    #[test]
    fn bounds_follow_the_heartbeat_and_the_upload() {
        let (t, m, b, c, h) = (
            MemTable::default(),
            Recorder::default(),
            budget(),
            cfg(),
            Hooks::default(),
        );
        let intake = Intake::default();
        // The publisher went down with a 10-minute watermark bound; the
        // coordinator's watermark is 8 minutes old: healthy by that bound
        // (the relay's fallback, 420 s, would call it stalled).
        let mut hb = beat(T0 - 3_600_000);
        hb.watermark_stall_ms = 600_000;
        let view = |age| {
            status::Read::Ok(status::WorkerView {
                confirmed_age_ms: Some(age),
                ..Default::default()
            })
        };
        let workers = [view(480_000), view(1_000)];
        let tr = fresh(&[]);
        let tracker::Read::Ok { generated_ms, .. } = &tr else {
            unreachable!()
        };
        let now = *generated_ms + 60_000;
        let r = TickReads {
            heartbeat: Some(&hb),
            intake: &intake,
            tracker: &tr,
            workers: Some(&workers),
        };
        let mut st = TickState::default();
        block_on(tick(&mut st, &c, &r, &t, &m, &b, &h, now)).unwrap();
        assert!(
            st.alerts
                .get(rules::COORDINATOR_UNWATCHED, "coordinator")
                .is_none(),
            "{:?}",
            st.alerts.entries.keys().collect::<Vec<_>>()
        );
        // The upload states an hourly schedule: stale after two hours.
        let mut u = crate::tracker::tests::upload(&[]);
        u.period_secs = Some(3_600);
        let hourly = tracker::Read::Ok {
            upload: Box::new(u),
            generated_ms: *generated_ms,
            uploaded_ms: *generated_ms,
        };
        let hb = beat(now);
        let mut st = TickState::default();
        let at = generated_ms + 2 * 3_600_000 + 1;
        block_on(tick(
            &mut st,
            &c,
            &reads(Some(&hb), &intake, &hourly),
            &t,
            &m,
            &b,
            &h,
            at,
        ))
        .unwrap();
        assert_eq!(st.tracker_period_ms, Some(3_600_000));
        assert!(st.alerts.get(rules::TRACKER_STALE, "tracker").is_some());
    }
}
