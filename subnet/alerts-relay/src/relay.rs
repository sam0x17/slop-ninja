//! The relay's request logic, independent of the Workers runtime: a post from
//! the publisher, the publisher's heartbeat and the cron tick. `cf.rs` feeds
//! it the Durable Object's tables and the bucket (the ledger and the alert
//! feed), the `send_email` binding (used only while email is on) and the
//! reads.

use serde::{Deserialize, Serialize};
use slop_ninja_alerts_wire::{
    AlertState, Event, Heartbeat, POST_EVENTS_BYTES, Post, RELAY_ANSWER_MS, Refusal, acknowledge,
    attempt, due, observe,
};

use crate::arena::{self, Budget};
use crate::config::Config;
use crate::feed::{self, Written};
use crate::ledger::{self, Claim, Outcome, StoreError, Table};
use crate::mime::{self, Addresses, Message, Source};
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
    /// Every event is settled now in every enabled sink: `written` feed
    /// objects and `emailed` events by this call (email only when it is
    /// on), `duplicate` written before, `unfired` resolutions of firings that
    /// never went out (nothing to resolve, so nothing is written).
    Delivered {
        written: usize,
        emailed: usize,
        duplicate: usize,
        unfired: usize,
    },
    /// Another sender holds a live claim on some events: retry later.
    Busy { written: usize, emailed: usize },
    /// A write or send failed or its outcome is unknown: retry later (the
    /// ledger keeps an email from going out more than twice, and writes a
    /// feed object until a put is acknowledged).
    Failed { failure: Failure },
}

/// What one sink did with a batch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SinkOutcome {
    done: usize,
    duplicate: usize,
    unfired: usize,
    busy: usize,
    failure: Option<Failure>,
}

/// Delivers the events not yet delivered, once per sink: the alert feed
/// always ([`crate::feed`]: one object per event, claimed and settled in the
/// ledger under [`feed::ledger_key`]), then email when it is on (one message
/// with the claimed events, under the bare event ids). A refused write or
/// send frees its claims. An email that timed out is retried once and then
/// counts as sent; a feed write that timed out is written again until a
/// put is acknowledged, so a lost write is never acknowledged to its sender
/// ([`crate::ledger`]). An unconfirmed resolution (its firing
/// post was never acknowledged to the sender) goes to a sink only when that
/// sink's ledger rows show its firing may have gone out there, or no longer
/// cover its episode.
#[allow(clippy::too_many_arguments)]
pub async fn send_events(
    table: &impl Table,
    mailer: &impl Mailer,
    budget: &Budget,
    cfg: &Config,
    source: Source<'_>,
    events: &[Event],
    now_ms: u64,
) -> Result<Delivery, StoreError> {
    let mut seen = std::collections::BTreeSet::new();
    let unique: Vec<&Event> = events
        .iter()
        .filter(|e| seen.insert(e.id.as_str()))
        .collect();
    let fed = feed_events(table, budget, cfg, source, &unique, now_ms).await?;
    let mailed = match &cfg.email {
        Some(addrs) => {
            email_events(table, mailer, budget, cfg, addrs, source, &unique, now_ms).await?
        }
        None => SinkOutcome::default(),
    };
    Ok(match (fed.failure, mailed.failure) {
        (Some(failure), _) | (None, Some(failure)) => Delivery::Failed { failure },
        (None, None) if fed.busy + mailed.busy > 0 => Delivery::Busy {
            written: fed.done,
            emailed: mailed.done,
        },
        (None, None) => Delivery::Delivered {
            written: fed.done,
            emailed: mailed.done,
            duplicate: fed.duplicate,
            unfired: fed.unfired,
        },
    })
}

/// The feed's part of a delivery: one object per event, each claimed,
/// written and settled on its own.
async fn feed_events(
    table: &impl Table,
    budget: &Budget,
    cfg: &Config,
    source: Source<'_>,
    events: &[&Event],
    now_ms: u64,
) -> Result<SinkOutcome, StoreError> {
    let mut o = SinkOutcome::default();
    for e in events {
        if o.failure.as_ref().is_some_and(|f| f.code == "no_time") {
            // The answer bound has passed: the rest stay due (unclaimed) for
            // the sender's retry.
            o.busy += 1;
            continue;
        }
        if e.unconfirmed()
            && !ledger::maybe_sent(
                table,
                &feed::ledger_key(&e.firing_id(source.rules)),
                e.since_ms,
            )?
        {
            // Its firing never reached the feed: the condition is clear, so
            // any firing-set row for it (an earlier rule set's) goes.
            feed::unfired(table, source.name, e, now_ms)?;
            o.unfired += 1;
            continue;
        }
        let key = feed::ledger_key(&e.id);
        let retry = match ledger::claim(table, &key, now_ms)? {
            Claim::Mine { retry } => retry,
            Claim::Sent => {
                o.duplicate += 1;
                continue;
            }
            Claim::Busy => {
                o.busy += 1;
                continue;
            }
        };
        // The step's reservation comes first: the object is built inside it.
        let Ok(_hold) = budget.reserve(feed::STEP_BYTES) else {
            o.failure.get_or_insert(feed::refuse_budget(table, now_ms)?);
            ledger::settle(table, &key, retry, Outcome::Refused, now_ms)?;
            continue;
        };
        let mut object = feed::Object::new(e, &cfg.network, source, now_ms);
        let outcome = match feed::write(table, &mut object, now_ms).await? {
            Written::Ok => Outcome::Sent,
            Written::TimedOut => {
                o.failure.get_or_insert_with(|| Failure::new("timeout", ""));
                Outcome::TimedOut
            }
            Written::Refused(f) => {
                o.failure.get_or_insert(f);
                Outcome::Refused
            }
        };
        // Only an acknowledged put counts: a reader dedups by event id,
        // so a retry that lands a second copy is harmless, and one that
        // never landed is written again at the sender's next retry.
        if ledger::settle_acknowledged(table, &key, retry, outcome, now_ms)? {
            feed::delivered(table, &object)?;
            o.done += 1;
        }
    }
    Ok(o)
}

/// The email part of a delivery (only while email is on): claims each id,
/// sends one message with the claimed ones, then settles them.
#[allow(clippy::too_many_arguments)]
async fn email_events(
    table: &impl Table,
    mailer: &impl Mailer,
    budget: &Budget,
    cfg: &Config,
    addrs: &Addresses,
    source: Source<'_>,
    events: &[&Event],
    now_ms: u64,
) -> Result<SinkOutcome, StoreError> {
    let mut out = SinkOutcome::default();
    let mut mine: Vec<Event> = Vec::new();
    let mut retries: Vec<bool> = Vec::new();
    for e in events {
        if e.unconfirmed() && !ledger::maybe_sent(table, &e.firing_id(source.rules), e.since_ms)? {
            out.unfired += 1;
            continue;
        }
        match ledger::claim(table, &e.id, now_ms)? {
            Claim::Mine { retry } => {
                mine.push((*e).clone());
                retries.push(retry);
            }
            Claim::Sent => out.duplicate += 1,
            Claim::Busy => out.busy += 1,
        }
    }
    if mine.is_empty() {
        return Ok(out);
    }
    let settle = |outcome: Outcome| -> Result<(), StoreError> {
        for (e, retry) in mine.iter().zip(&retries) {
            ledger::settle(table, &e.id, *retry, outcome, now_ms)?;
        }
        Ok(())
    };
    let Ok(_hold) = budget.reserve(arena::message_footprint(arena::text_bound())) else {
        settle(Outcome::Refused)?;
        out.failure = Some(Failure::new("budget", ""));
        return Ok(out);
    };
    let message = mime::build(addrs, &mine, &cfg.network, source, now_ms);
    match mailer.send(&message).await {
        Ok(()) => {
            settle(Outcome::Sent)?;
            out.done = mine.len();
        }
        Err(SendError::Timeout) => {
            settle(Outcome::TimedOut)?;
            out.failure = Some(Failure::new("timeout", ""));
        }
        Err(SendError::Refused(failure)) => {
            settle(Outcome::Refused)?;
            out.failure = Some(failure);
        }
    }
    Ok(out)
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

/// A publisher post. 200 only when every event in it is settled in every
/// enabled sink (written now or before, or an unconfirmed resolution with
/// nothing to resolve), so the publisher acknowledges exactly then.
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
    let (status, body) = match send_events(table, mailer, budget, cfg, source, &post.events, now_ms)
        .await
    {
        Ok(Delivery::Delivered {
            written,
            emailed,
            duplicate,
            unfired,
        }) => (
            200,
            format!(
                r#"{{"written":{written},"emailed":{emailed},"duplicate":{duplicate},"unfired":{unfired}}}"#
            ),
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
    /// The first tick of the current series whose deliveries failed (not
    /// busy): a write or send is failing. Cleared by a tick that finishes
    /// without a failed delivery.
    #[serde(default)]
    pub send_failing_since_ms: Option<u64>,
    /// The last drill id emailed (`DRILL_ID`): that drill is never sent
    /// again.
    #[serde(default)]
    pub drill_sent: Option<String>,
    /// The last drill id written to the alert feed: never written again.
    #[serde(default)]
    pub drill_fed: Option<String>,
    /// The status check after a token change ([`status::CheckState`]): the
    /// fingerprint of the tokens whose check settled, and the last check's
    /// outcome (stated in `latest.json` as `status_check`).
    #[serde(default)]
    pub status_check: status::CheckState,
    /// Ticks run (stated in `latest.json`).
    #[serde(default)]
    pub ticks: u64,
}

/// What one tick did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    pub due: usize,
    /// Feed objects written.
    pub written: usize,
    /// Events emailed (0 while email is off).
    pub sent: usize,
    pub undelivered: usize,
    pub failure: Option<Failure>,
    /// Another tick took the lease over: this one stopped before its next
    /// step.
    pub stopped: bool,
    /// The tick finished, and its deliveries have not been failing for the
    /// undelivered hold (two cron periods): the alert path works as far as
    /// the relay can tell, and the external dead-man may be pinged. An idle
    /// relay (nothing due) proves only that its cron runs.
    pub delivery_ok: bool,
    /// What happened to the deploy-time drill's email this tick, if one was
    /// due: `sent`, `timeout` (it may have gone out; recorded as sent, never
    /// repeated) or `refused` (retried by the next tick).
    pub drill: Option<&'static str>,
    /// The same for the drill's feed object: `written`, `timeout` or
    /// `refused`.
    pub drill_feed: Option<&'static str>,
    /// Feed objects the retention sweep deleted.
    pub swept: usize,
    /// How `latest.json`'s put ended: `written`, `timeout` or `refused`
    /// (`None` when the tick stopped first).
    pub latest: Option<&'static str>,
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

/// Sends the drill message for `drill` within the budget.
async fn send_drill(
    mailer: &impl Mailer,
    budget: &Budget,
    cfg: &Config,
    drill: &str,
    now_ms: u64,
) -> Result<(), SendError> {
    let Some(addrs) = &cfg.email else {
        return Err(SendError::Refused(Failure::new("email_off", "")));
    };
    let Ok(_hold) = budget.reserve(arena::message_footprint(arena::text_bound())) else {
        return Err(SendError::Refused(Failure::new("budget", "")));
    };
    let message = mime::build_drill(addrs, &cfg.network, drill, now_ms);
    mailer.send(&message).await
}

/// Everything one tick read before it evaluates.
pub struct TickReads<'a> {
    pub heartbeat: Option<&'a HeartbeatRecord>,
    pub intake: &'a Intake,
    pub tracker: &'a tracker::Read,
    pub workers: Option<&'a [status::Read; 2]>,
}

/// One cron tick over what the Durable Object read: evaluates the rules,
/// delivers what is due through the ledger (the feed, and email when it is
/// on), batch by batch under the renewed lease, acknowledges what went out,
/// sweeps the ledger and the feed, and rewrites `latest.json`.
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
    st.ticks = st.ticks.saturating_add(1);
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
        // Figures only ever go to email (and only when it is on).
        spend_figures: cfg.spend_figures && cfg.email.is_some(),
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
        // The batch's bound: one answer bound per feed object and one for
        // the email.
        if !hooks.renew(RELAY_ANSWER_MS.saturating_mul(batch.len() as u64 + 1)) {
            report.stopped = true;
            break;
        }
        attempt(&mut st.alerts, &batch, now_ms);
        hooks.save(st)?;
        match send_events(table, mailer, budget, cfg, source, &batch, now_ms).await? {
            Delivery::Delivered {
                written, emailed, ..
            } => {
                report.written += written;
                report.sent += emailed;
                acknowledge(&mut st.alerts, &batch, now_ms);
                hooks.save(st)?;
            }
            Delivery::Busy { written, emailed } => {
                report.written += written;
                report.sent += emailed;
                report.undelivered += batch.len() - written.max(emailed).min(batch.len());
            }
            Delivery::Failed { failure } => {
                report.undelivered += batch.len();
                report.failure = Some(failure);
            }
        }
    }
    drills(st, cfg, table, mailer, budget, hooks, now_ms, &mut report).await?;
    // The delivery path: a failed write or send starts (or continues) a
    // failing series, a tick that finished without one ends it; a stopped
    // tick says nothing.
    let failing = match (&report.failure, report.stopped) {
        (Some(_), _) => Some(st.send_failing_since_ms.unwrap_or(now_ms)),
        (None, true) => st.send_failing_since_ms,
        (None, false) => None,
    };
    if failing != st.send_failing_since_ms {
        st.send_failing_since_ms = failing;
        hooks.save(st)?;
    }
    report.delivery_ok = !report.stopped
        && failing.is_none_or(|since| now_ms.saturating_sub(since) < bounds.undelivered_hold_ms);
    // The feed's retention, then its heartbeat, each under the renewed
    // lease (each is one bucket call within the answer bound). The
    // retention never follows an upload's shorter schedule below the
    // configured one.
    let retention = feed::retention_ms(
        bounds
            .tracker_stale_ms
            .max(cfg.bounds(None, None).tracker_stale_ms),
    );
    table.feed_firing_sweep(now_ms.saturating_sub(feed::firing_ttl_ms(retention, renotify)))?;
    if !report.stopped && hooks.renew(RELAY_ANSWER_MS) {
        report.swept = feed::sweep(table, budget, now_ms, retention).await?;
    }
    if !report.stopped && hooks.renew(RELAY_ANSWER_MS) {
        let horizon = feed::horizon_ms(retention, renotify);
        report.latest =
            Some(latest(st, cfg, table, budget, &report, retention, horizon, now_ms).await?);
    }
    Ok(report)
}

/// The deploy-time drill: one feed object per drill id, and one email per
/// drill id while email is on, after the due events and under the same
/// lease. It tests the delivery path only (no rule, no ledger row): each
/// sink records the id in the state once it went out, or once its attempt
/// timed out, so it is never repeated; a refused one counts as a failed
/// delivery and is retried by the next tick.
#[allow(clippy::too_many_arguments)]
async fn drills(
    st: &mut TickState,
    cfg: &Config,
    table: &impl Table,
    mailer: &impl Mailer,
    budget: &Budget,
    hooks: &impl TickHooks,
    now_ms: u64,
    report: &mut TickReport,
) -> Result<(), StoreError> {
    let Some(drill) = &cfg.drill else {
        return Ok(());
    };
    if !report.stopped && st.drill_fed.as_deref() != Some(drill.as_str()) {
        if !hooks.renew(RELAY_ANSWER_MS) {
            report.stopped = true;
        } else {
            let written = match budget.reserve(feed::STEP_BYTES) {
                Ok(_hold) => {
                    let mut o = feed::Object::drill(drill, &cfg.network, now_ms);
                    feed::write(table, &mut o, now_ms).await?
                }
                Err(_) => Written::Refused(feed::refuse_budget(table, now_ms)?),
            };
            match written {
                Written::Ok | Written::TimedOut => {
                    report.drill_feed = Some(if written == Written::Ok {
                        "written"
                    } else {
                        "timeout"
                    });
                    st.drill_fed = Some(drill.clone());
                    hooks.save(st)?;
                }
                Written::Refused(failure) => {
                    report.drill_feed = Some("refused");
                    report.failure = Some(failure);
                }
            }
        }
    }
    if cfg.email.is_some() && !report.stopped && st.drill_sent.as_deref() != Some(drill.as_str()) {
        if !hooks.renew(RELAY_ANSWER_MS) {
            report.stopped = true;
        } else {
            match send_drill(mailer, budget, cfg, drill, now_ms).await {
                Ok(()) => {
                    st.drill_sent = Some(drill.clone());
                    hooks.save(st)?;
                    report.drill = Some("sent");
                }
                Err(SendError::Timeout) => {
                    st.drill_sent = Some(drill.clone());
                    hooks.save(st)?;
                    report.drill = Some("timeout");
                }
                Err(SendError::Refused(failure)) => {
                    report.drill = Some("refused");
                    report.failure = Some(failure);
                }
            }
        }
    }
    Ok(())
}

/// Rewrites `latest.json` within [`feed::LATEST_BYTES`]: the tick's count
/// and time, the firing set, the last write's outcome, the retention and
/// the write gap. Inside one [`feed::STEP_BYTES`] reservation, taken before
/// the firing rows are read.
#[allow(clippy::too_many_arguments)]
async fn latest(
    st: &TickState,
    cfg: &Config,
    table: &impl Table,
    budget: &Budget,
    report: &TickReport,
    retention: u64,
    horizon: u64,
    now_ms: u64,
) -> Result<&'static str, StoreError> {
    let Ok(_hold) = budget.reserve(feed::STEP_BYTES) else {
        return Ok("refused");
    };
    let at = table.feed_stamp(now_ms)?;
    let (entries, total) = table.feed_firing_entries()?;
    let mut l = feed::Latest {
        schema: feed::LATEST_SCHEMA.into(),
        network: cfg.network.clone(),
        utc: mime::rfc3339(at),
        utc_ms: at,
        tick: st.ticks,
        cron_period_s: feed::RELAY_CRON_PERIOD_MS / 1000,
        pull_period_s: feed::PULL_PERIOD_MS / 1000,
        tick_reads_s: TICK_READS_MS / 1000,
        write_gap_s: WRITE_GAP_MS / 1000,
        retention_s: retention / 1000,
        retained_from_ms: now_ms.saturating_sub(retention),
        horizon_s: horizon / 1000,
        object_bytes: feed::OBJECT_BYTES as u64,
        email: if cfg.email.is_some() { "on" } else { "off" }.into(),
        delivery_ok: report.delivery_ok,
        due: report.due as u64,
        written: report.written as u64,
        emailed: report.sent as u64,
        undelivered: report.undelivered as u64,
        config_warnings: cfg.warnings.len() as u64,
        last_write: table.feed_last_note()?,
        status_check: st.status_check.last.as_ref().map(feed::CheckNote::new),
        ..feed::Latest::default()
    };
    l.fill(entries, total);
    Ok(match table.feed_put(feed::LATEST_KEY, l.body()).await {
        Ok(()) => "written",
        Err(SendError::Timeout) => "timeout",
        Err(SendError::Refused(_)) => "refused",
    })
}

/// A tick's reads before it evaluates: at most two status reads (while the
/// publisher is down, or for the status check after a token change) and the
/// tracker object, each within the status
/// read bound. The tick takes its lease for that long, then renews it
/// before each step by that step's bound ([`RELAY_ANSWER_MS`]), so the lease
/// always covers the step under way, whatever the number of batches.
pub const TICK_READS_MS: u64 = 3 * slop_ninja_alerts_wire::STATUS_TIMEOUT_MS;

/// The longest between two of the relay's writes to the feed while it runs
/// (a reader's dead-man bound, stated in `latest.json`): a tick writes
/// `latest.json` at its end and its objects as it goes, and the next tick
/// starts at the first cron period after this one releases its lease. From
/// one write's key stamp: that put's answer bound, one cron period, the
/// next tick's read lease, and the answer bound of the step that writes
/// nothing visible before the next stamp (the sweep, before `latest.json`;
/// inside a tick, consecutive stamps are at most two answer bounds apart:
/// an object's put and an email). 5 + 60 + 30 + 5 = 100 s. A cron start
/// later than one cron period is outside it; a reader holds one pull
/// period on top.
pub const WRITE_GAP_MS: u64 =
    RELAY_ANSWER_MS + feed::RELAY_CRON_PERIOD_MS + TICK_READS_MS + RELAY_ANSWER_MS;

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
            (
                200,
                r#"{"written":1,"emailed":1,"duplicate":0,"unfired":0}"#.into()
            )
        );
        assert_eq!(
            answer(block_on(handle_post(&body, &c, &t, &m, &b, T0 + 60_000))),
            (
                200,
                r#"{"written":0,"emailed":0,"duplicate":1,"unfired":0}"#.into()
            )
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
            (
                200,
                r#"{"written":0,"emailed":0,"duplicate":0,"unfired":1}"#.into()
            )
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
        assert_eq!((r.sent, r.delivery_ok), (1, true));
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
            ok.push((r.delivery_ok, r.failure.is_some()));
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
        assert_eq!((r.due, r.delivery_ok), (0, true));
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
        assert!(r.stopped && !r.delivery_ok);
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
            (
                200,
                r#"{"written":1,"emailed":1,"duplicate":0,"unfired":0}"#.into()
            )
        );
        let text = qp_decode(m.sent.borrow()[1].raw.split_once("\r\n\r\n").unwrap().1);
        assert!(
            text.replace("\r\n", " ").contains("may not have gone out"),
            "{text}"
        );
    }

    /// Email off: only the network is set.
    fn cfg_off() -> Config {
        crate::config::parse(vars(&[("NETWORK", "testnet")])).unwrap()
    }

    fn states(t: &MemTable) -> Vec<(String, String, String)> {
        t.feed()
            .into_iter()
            .map(|o| (o.state, o.rule, o.subject))
            .collect()
    }

    fn s3(a: &str, b: &str, c: &str) -> (String, String, String) {
        (a.into(), b.into(), c.into())
    }

    /// With email off (no ALERT_EMAIL_TO), posts are delivered to the feed
    /// alone, one object per event id: a retried post writes nothing new,
    /// and the firing set follows the objects.
    #[test]
    fn the_feed_writes_once_per_event_id_with_email_off() {
        let (t, m, b, c) = (
            MemTable::default(),
            Recorder::default(),
            budget(),
            cfg_off(),
        );
        assert!(c.email.is_none() && c.warnings.is_empty());
        let f = ev(
            EventKind::Firing,
            "commit_gap",
            "coordinator",
            "last commit at epoch 50, now 53",
        );
        let body = post(vec![f.clone()]);
        assert_eq!(
            answer(block_on(handle_post(&body, &c, &t, &m, &b, T0))),
            (
                200,
                r#"{"written":1,"emailed":0,"duplicate":0,"unfired":0}"#.into()
            )
        );
        for n in 1..4 {
            assert_eq!(
                answer(block_on(handle_post(
                    &body,
                    &c,
                    &t,
                    &m,
                    &b,
                    T0 + n * 60_000
                ))),
                (
                    200,
                    r#"{"written":0,"emailed":0,"duplicate":1,"unfired":0}"#.into()
                )
            );
        }
        assert_eq!(m.count(), 0, "no email while it is off");
        let objects = t.feed();
        assert_eq!(objects.len(), 1);
        let o = &objects[0];
        assert_eq!(o.event_id, f.id);
        assert_eq!(
            (o.source.as_str(), o.state.as_str()),
            ("publisher", "FIRING")
        );
        assert_eq!(o.body, "last commit at epoch 50, now 53");
        assert_eq!(
            o.title,
            "[slop-ninja testnet] FIRING commit_gap coordinator"
        );
        assert_eq!(t.firing_set().len(), 1);
        assert_eq!(t.last_note().unwrap().result, "written");
        let z = ev(
            EventKind::Resolved,
            "commit_gap",
            "coordinator",
            "committed",
        );
        let a = block_on(handle_post(&post(vec![z]), &c, &t, &m, &b, T0 + 300_000));
        assert_eq!(a.status, 200);
        assert_eq!(
            states(&t),
            vec![
                s3("FIRING", "commit_gap", "coordinator"),
                s3("RESOLVED", "commit_gap", "coordinator")
            ]
        );
        assert!(t.firing_set().is_empty());
        assert_eq!(b.held(), 0);
    }

    /// A feed put that timed out may have landed, or not: its claim is held
    /// one claim period and written again, after every timeout, until a put
    /// is acknowledged; only then is the post acknowledged. Copies that
    /// landed carry the same event id (a reader keeps the first); later
    /// retries write nothing. A refused put frees its claim at once.
    #[test]
    fn feed_timeouts_are_retried_until_a_put_is_acknowledged() {
        let (t, m, b, c) = (
            MemTable::default(),
            Recorder::default(),
            budget(),
            cfg_off(),
        );
        t.script_feed(vec![Err(SendError::Timeout), Err(SendError::Timeout)]);
        let e = ev(EventKind::Firing, "commit_gap", "coordinator", "d");
        let body = post(vec![e.clone()]);
        let mut answers = Vec::new();
        for n in 0..6 {
            answers.push(block_on(handle_post(&body, &c, &t, &m, &b, T0 + n * CLAIM_MS)).status);
        }
        assert_eq!(answers, vec![503, 503, 200, 200, 200, 200]);
        let ids: Vec<String> = t.feed().into_iter().map(|o| o.event_id).collect();
        assert_eq!(
            ids,
            vec![e.id.clone(), e.id.clone(), e.id.clone()],
            "two copies that timed out but landed, and the acknowledged one"
        );
        assert_eq!(t.last_note().unwrap().result, "written");
        assert_eq!(
            t.firing_set().len(),
            1,
            "in the firing set once acknowledged"
        );
        // Timeouts that land nothing: the post is never acknowledged before
        // a put is, so the event is never lost, however many time out.
        let t = MemTable::default();
        t.feed_timeouts_lost.set(true);
        t.script_feed(vec![Err(SendError::Timeout); 4]);
        let mut answers = Vec::new();
        for n in 0..6 {
            answers.push(block_on(handle_post(&body, &c, &t, &m, &b, T0 + n * CLAIM_MS)).status);
        }
        assert_eq!(answers, vec![503, 503, 503, 503, 200, 200]);
        assert_eq!(t.feed().len(), 1, "exactly the acknowledged write landed");
        // A resolution whose put times out without landing is retried too:
        // the firing set keeps the condition until the resolution lands.
        let z = ev(EventKind::Resolved, "commit_gap", "coordinator", "d");
        t.script_feed(vec![Err(SendError::Timeout)]);
        let zb = post(vec![z.clone()]);
        assert_eq!(
            block_on(handle_post(&zb, &c, &t, &m, &b, T0 + 6 * CLAIM_MS)).status,
            503
        );
        assert_eq!(t.firing_set().len(), 1);
        assert_eq!(
            block_on(handle_post(&zb, &c, &t, &m, &b, T0 + 7 * CLAIM_MS)).status,
            200
        );
        assert!(t.firing_set().is_empty());
        assert_eq!(t.feed().iter().filter(|o| o.event_id == z.id).count(), 1);
        let t = MemTable::default();
        t.script_feed(vec![Err(SendError::Refused(Failure::new("r2_put", "")))]);
        assert_eq!(block_on(handle_post(&body, &c, &t, &m, &b, T0)).status, 503);
        assert!(t.feed().is_empty());
        assert_eq!(
            block_on(handle_post(&body, &c, &t, &m, &b, T0 + 1)).status,
            200
        );
        assert_eq!(t.feed().len(), 1);
    }

    /// An unconfirmed resolution goes to the feed when the feed holds its
    /// firing (or no longer covers its episode), and is settled without an
    /// object when the firing never reached the feed, whatever email did.
    #[test]
    fn unconfirmed_resolutions_follow_their_feed_firing() {
        let (b, m) = (budget(), Recorder::default());
        let fields = |kind| Event::new(RULES, kind, "commit_gap", "coordinator", "d", 5, None);
        let (f, z) = (fields(EventKind::Firing), fields(EventKind::Resolved));
        let (t, c) = (MemTable::default(), cfg_off());
        block_on(handle_post(&post(vec![f.clone()]), &c, &t, &m, &b, T0));
        block_on(handle_post(
            &post(vec![z.clone()]),
            &c,
            &t,
            &m,
            &b,
            T0 + 60_000,
        ));
        assert_eq!(t.feed().len(), 2);
        // The firing was emailed before the feed existed (a bare-id row
        // only): the feed has nothing to resolve, and a firing-set row for
        // the condition (written under an earlier rule set) goes.
        let t = MemTable::default();
        let row = feed::FiringEntry {
            source: "publisher".into(),
            rule: "commit_gap".into(),
            subject: "coordinator".into(),
            state: "FIRING".into(),
            since_utc: String::new(),
            since_ms: 5,
            event_id: "e".repeat(64),
            at_utc: String::new(),
        };
        t.feed_firing(
            &feed::FiringEntry::key("publisher", "commit_gap", "coordinator"),
            Some(&row),
            T0,
        )
        .unwrap();
        crate::ledger::claim(&t, &f.id, T0).unwrap();
        crate::ledger::settle(&t, &f.id, false, crate::ledger::Outcome::Sent, T0).unwrap();
        assert_eq!(
            answer(block_on(handle_post(
                &post(vec![z]),
                &c,
                &t,
                &m,
                &b,
                T0 + 60_000
            ))),
            (
                200,
                r#"{"written":0,"emailed":0,"duplicate":0,"unfired":1}"#.into()
            )
        );
        assert!(t.feed().is_empty());
        assert!(t.firing_set().is_empty());
    }

    /// The feed's retention never follows an upload that states a shorter
    /// schedule than the configured one; firing-set rows not refreshed for
    /// the retention plus one renotify interval go; the write gap and the
    /// horizon are stated.
    #[test]
    fn retention_floor_and_firing_rows() {
        let (t, m, b, c, h) = (
            MemTable::default(),
            Recorder::default(),
            budget(),
            cfg_off(),
            Hooks::default(),
        );
        let mut tr = fresh(&[]);
        if let tracker::Read::Ok { upload, .. } = &mut tr {
            upload.period_secs = Some(3600);
        }
        let hb = beat(T0);
        let retention = feed::retention_ms(2 * 86_400_000);
        let ttl = feed::firing_ttl_ms(retention, rules::renotify_ms(Some(&hb)));
        let row = |i: u64| feed::FiringEntry {
            source: "relay".into(),
            rule: "r".into(),
            subject: format!("s{i}"),
            state: "FIRING".into(),
            since_utc: String::new(),
            since_ms: 0,
            event_id: "e".repeat(64),
            at_utc: String::new(),
        };
        t.feed_firing("relay/r/s0", Some(&row(0)), T0 - ttl - 1)
            .unwrap();
        t.feed_firing("relay/r/s1", Some(&row(1)), T0 - ttl)
            .unwrap();
        let mut st = TickState::default();
        block_on(tick(
            &mut st,
            &c,
            &reads(Some(&hb), &Intake::default(), &tr),
            &t,
            &m,
            &b,
            &h,
            T0,
        ))
        .unwrap();
        let l: feed::Latest = serde_json::from_slice(&t.object(feed::LATEST_KEY).unwrap()).unwrap();
        assert_eq!(st.tracker_period_ms, Some(3_600_000));
        assert_eq!(l.retention_s, retention / 1000);
        assert_eq!(l.write_gap_s, 100);
        assert_eq!(WRITE_GAP_MS, 100_000);
        // The horizon: the retention plus the ledger's row retention, which
        // covers the firing-set row lifetime.
        let renotify = rules::renotify_ms(Some(&hb));
        assert_eq!(
            l.horizon_s,
            (retention + crate::ledger::retention_ms(renotify)) / 1000
        );
        assert!(l.horizon_s * 1000 >= ttl);
        assert_eq!(
            t.firing_set()
                .iter()
                .map(|e| e.subject.as_str())
                .collect::<Vec<_>>(),
            vec!["s1"]
        );
    }

    /// A feed step whose reservation is refused builds nothing, notes the
    /// refusal and frees its claim; the next attempt writes it.
    #[test]
    fn a_refused_step_reservation_frees_its_claim() {
        let (t, m, c) = (MemTable::default(), Recorder::default(), cfg_off());
        let b = budget();
        let e = ev(EventKind::Firing, "commit_gap", "coordinator", "d");
        let body = post(vec![e]);
        let hold = b.reserve(b.remaining() - feed::STEP_BYTES + 1).unwrap();
        assert_eq!(block_on(handle_post(&body, &c, &t, &m, &b, T0)).status, 503);
        assert!(t.feed().is_empty());
        let note = t.last_note().unwrap();
        assert_eq!(
            (note.result.as_str(), note.code.as_str()),
            ("refused", "budget")
        );
        drop(hold);
        assert_eq!(
            block_on(handle_post(&body, &c, &t, &m, &b, T0 + 1)).status,
            200
        );
        assert_eq!(t.feed().len(), 1);
    }

    /// Ticks write their own alerts to the feed and rewrite latest.json:
    /// the tick count, the firing set, the last write, email off.
    #[test]
    fn ticks_write_the_feed_and_latest() {
        let (t, m, b, c, h) = (
            MemTable::default(),
            Recorder::default(),
            budget(),
            cfg_off(),
            Hooks::default(),
        );
        let intake = Intake::default();
        let tr = fresh(&["2026-11"]);
        let mut st = TickState::default();
        let mut latest = Vec::new();
        for n in 0..3 {
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
            assert_eq!(r.latest, Some("written"));
            assert!(r.delivery_ok);
            latest.push(
                serde_json::from_slice::<feed::Latest>(&t.object(feed::LATEST_KEY).unwrap())
                    .unwrap(),
            );
        }
        assert_eq!(m.count(), 0);
        assert_eq!(states(&t), vec![s3("FIRING", "spend_alert", "2026-11")]);
        let l = &latest[2];
        assert_eq!(
            latest.iter().map(|l| l.tick).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(latest[0].utc_ms < latest[1].utc_ms && latest[1].utc_ms < l.utc_ms);
        assert_eq!(
            (l.schema.as_str(), l.email.as_str()),
            (feed::LATEST_SCHEMA, "off")
        );
        assert_eq!(
            (
                l.cron_period_s,
                l.pull_period_s,
                l.tick_reads_s,
                l.write_gap_s
            ),
            (60, 60, 30, 100)
        );
        assert_eq!(l.retention_s, (2 * 86_400_000 + 60_000) / 1000);
        assert_eq!(l.object_bytes, feed::OBJECT_BYTES as u64);
        assert_eq!((l.firing_count, l.firing_omitted), (1, 0));
        assert_eq!(l.firing[0].rule, "spend_alert");
        assert_eq!(l.firing[0].source, "relay");
        assert_eq!(latest[0].written, 1);
        assert_eq!(l.last_write.as_ref().unwrap().result, "written");
        // A tick that lost its lease writes no latest.json.
        let t2 = MemTable::default();
        let r = block_on(tick(
            &mut TickState::default(),
            &c,
            &reads(Some(&beat(T0)), &intake, &tr),
            &t2,
            &m,
            &b,
            &LeaseLostAtBatch,
            T0,
        ))
        .unwrap();
        assert!(r.stopped && r.latest.is_none() && t2.object(feed::LATEST_KEY).is_none());
        assert_eq!(b.held(), 0);
    }

    /// With email and figures on, a spend alert's email carries the month
    /// totals; its feed object never does.
    #[test]
    fn the_feed_never_carries_spend_figures() {
        let c = cfg_with(&[("SPEND_FIGURES_IN_EMAIL", "true")]);
        let (t, m, b, h) = (
            MemTable::default(),
            Recorder::default(),
            budget(),
            Hooks::default(),
        );
        let intake = Intake::default();
        let tr = fresh(&["2026-11"]);
        let hb = beat(T0);
        let mut st = TickState::default();
        block_on(tick(
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
        let text = qp_decode(m.sent.borrow()[0].raw.split_once("\r\n\r\n").unwrap().1);
        assert!(text.contains('$'), "the email has the figures: {text}");
        let o = &t.feed()[0];
        assert_eq!(o.rule, "spend_alert");
        assert!(
            !o.body.contains('$') && !o.body.contains("month to date"),
            "{}",
            o.body
        );
        let raw = String::from_utf8(t.object(&t.objects()[0]).unwrap()).unwrap();
        assert!(!raw.contains('$'), "{raw}");
    }

    /// The deploy-time drill writes one feed object per drill id, with email
    /// off too, and is never repeated.
    #[test]
    fn drills_go_to_the_feed_once() {
        let c = crate::config::parse(vars(&[("NETWORK", "testnet"), ("DRILL_ID", "20261102-f")]))
            .unwrap();
        let (t, m, b, h) = (
            MemTable::default(),
            Recorder::default(),
            budget(),
            Hooks::default(),
        );
        let intake = Intake::default();
        let tr = fresh(&[]);
        let mut st = TickState::default();
        let mut seen = Vec::new();
        for n in 0..3 {
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
            seen.push((r.drill_feed, r.drill));
        }
        assert_eq!(
            seen,
            vec![(Some("written"), None), (None, None), (None, None)]
        );
        assert_eq!(m.count(), 0);
        assert_eq!(states(&t), vec![s3("DRILL", "drill", "20261102-f")]);
        assert_eq!(t.feed()[0].event_id, feed::drill_id("20261102-f"));
        assert_eq!(st.drill_fed.as_deref(), Some("20261102-f"));
        assert!(t.firing_set().is_empty(), "a drill is not firing");
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

    /// The deploy-time drill: one email per drill id, beside the due
    /// events; a later tick with the same id sends nothing; a refused drill
    /// is a failed send retried by the next tick; a timed-out one is never
    /// repeated; a new id sends again; no drill without the setting.
    #[test]
    fn drills_send_once_per_id() {
        let (t, b, h) = (MemTable::default(), budget(), Hooks::default());
        let c = cfg_with(&[("DRILL_ID", "20261102-a")]);
        let intake = Intake::default();
        let tr = fresh(&[]);
        let refused = Err(SendError::Refused(Failure::new(
            "E_RECIPIENT_NOT_ALLOWED",
            "",
        )));
        let m = Recorder::scripted(vec![refused]);
        let mut st = TickState::default();
        let mut outcomes = Vec::new();
        for n in 0..3 {
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
            outcomes.push((r.drill, r.failure.is_some(), r.sent));
        }
        assert_eq!(
            outcomes,
            vec![
                (Some("refused"), true, 0),
                (Some("sent"), false, 0),
                (None, false, 0)
            ]
        );
        assert_eq!(
            m.subjects(),
            vec![
                "SlopNinja testnet alert drill",
                "SlopNinja testnet alert drill"
            ],
            "the refused send and its retry; nothing after"
        );
        assert_eq!(st.drill_sent.as_deref(), Some("20261102-a"));
        assert_eq!(h.saved.borrow().as_ref().unwrap().drill_sent, st.drill_sent);
        // A new id sends again; a timed-out drill is recorded as sent.
        let c2 = cfg_with(&[("DRILL_ID", "20261102-b")]);
        let m2 = Recorder::scripted(vec![Err(SendError::Timeout)]);
        for n in 3..5 {
            let now = T0 + n * 60_000;
            let hb = beat(now);
            block_on(tick(
                &mut st,
                &c2,
                &reads(Some(&hb), &intake, &tr),
                &t,
                &m2,
                &b,
                &h,
                now,
            ))
            .unwrap();
        }
        assert_eq!(m2.count(), 1, "a timed-out drill is not repeated");
        assert_eq!(st.drill_sent.as_deref(), Some("20261102-b"));
        // Without the setting, no drill; due events still go out first.
        let m3 = Recorder::default();
        let mut st = TickState::default();
        let spend = fresh(&["2026-11"]);
        let hb = beat(T0);
        let r = block_on(tick(
            &mut st,
            &cfg(),
            &reads(Some(&hb), &intake, &spend),
            &t,
            &m3,
            &b,
            &h,
            T0,
        ))
        .unwrap();
        assert_eq!((r.drill, r.sent), (None, 1));
        let m4 = Recorder::default();
        let mut st = TickState::default();
        let r = block_on(tick(
            &mut st,
            &c,
            &reads(Some(&hb), &intake, &spend),
            &MemTable::default(),
            &m4,
            &b,
            &h,
            T0,
        ))
        .unwrap();
        assert_eq!((r.drill, r.sent), (Some("sent"), 1));
        assert_eq!(
            m4.subjects(),
            vec![
                "[slop-ninja testnet] FIRING spend_alert 2026-11",
                "SlopNinja testnet alert drill"
            ]
        );
        // A tick that lost its lease sends no drill.
        let mut st = TickState::default();
        let r = block_on(tick(
            &mut st,
            &c,
            &reads(Some(&hb), &intake, &tr),
            &MemTable::default(),
            &Recorder::default(),
            &b,
            &LeaseLostAtBatch,
            T0,
        ))
        .unwrap();
        assert!(r.stopped && r.drill.is_none() && st.drill_sent.is_none());
        assert_eq!(b.held(), 0);
    }
}
