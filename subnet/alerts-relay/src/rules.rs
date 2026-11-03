//! The relay's own rules (`slop_ninja.alerts_relay.rules/v1`), evaluated by
//! its one-minute cron, outside the coordinator and the publisher. They read
//! and alert only; nothing here advances protocol state.
//!
//! | Rule | Subject | Fires when |
//! | --- | --- | --- |
//! | `publisher_silent` | `publisher` | no publisher heartbeat for longer than one run lease plus one cron period, held for two cron periods (the publisher's own hold for an unanswered source) |
//! | `publisher_alerts_undelivered` | `publisher` | held two cron periods: the publisher's last heartbeat reports due alert events it could not deliver, or names a post schema this relay does not read, or the relay refused the publisher's posts since its last accepted one |
//! | `coordinator_unwatched` | `coordinator`, `eval_worker` | while the publisher is silent past that hold: the Worker's `/status` does not answer (held two cron periods), holds a journal or signing fault, or its confirmed watermark is older than the watchdog silence plus two cron periods (the publisher's `watermark_stalled` bound) |
//! | `spend_alert` | the month | the tracker's upload lists the month with an active, unacknowledged alert |
//! | `spend_tracker_stale` | `tracker` | the tracker's newest output is older than two of its schedule periods, no readable upload exists, or the bucket has not answered for two schedule periods |
//! | `spend_tracker_blind` | `tracker` | the tracker's newest run could not authenticate to Cloudflare |
//! | `spend_tracker_incomplete` | `tracker` | held two of the tracker's schedule periods: its newest readable runs did not price everything (a non-zero exit, `complete` false, or alert state `undetermined`), so its totals are lower bounds |
//! | `relay_config_warning` | `relay` | an optional setting of the relay did not parse and its default is in force (names only) |
//!
//! Every bound is derived from the publisher's compiled cron period, lease and
//! hold ([`slop_ninja_alerts_wire`], which the publisher builds with), the
//! publisher's watermark bound (in its heartbeat; the relay's
//! `WATCHDOG_SILENCE_SECS` only until the first one) and the tracker's
//! schedule period (in its upload; `TRACKER_PERIOD_SECS` only until the first
//! one); none is tuned here. Reminders follow the publisher's renotify
//! interval (sent in its heartbeat, never below one cron period). Unknown is
//! never a resolution: while the tracker's upload is missing or unreadable,
//! the alerts it raised stay open, and while the bucket does not answer
//! nothing it raised changes.

use serde::{Deserialize, Serialize};
use slop_ninja_alerts_wire::{
    AlertState, Condition, DEFAULT_RENOTIFY_MS, MIN_RENOTIFY_MS, POST_SCHEMA,
    PUBLISHER_CRON_PERIOD_MS, PUBLISHER_LEASE_MS, PUBLISHER_UNREACHABLE_HOLD_MS,
};

use crate::mime::rfc3339;
use crate::status::{self, WorkerView};
use crate::tracker::{self, Upload};

pub const RULES_ID: &str = "slop_ninja.alerts_relay.rules/v1";

pub const PUBLISHER_SILENT: &str = "publisher_silent";
pub const PUBLISHER_UNDELIVERED: &str = "publisher_alerts_undelivered";
pub const COORDINATOR_UNWATCHED: &str = "coordinator_unwatched";
pub const SPEND_ALERT: &str = "spend_alert";
pub const TRACKER_STALE: &str = "spend_tracker_stale";
pub const TRACKER_BLIND: &str = "spend_tracker_blind";
pub const TRACKER_INCOMPLETE: &str = "spend_tracker_incomplete";
pub const RELAY_CONFIG_WARNING: &str = "relay_config_warning";

/// The relay counts the publisher as down once it has been silent this long:
/// past one run lease and one cron period (a run killed mid-step blocks the
/// next runs for one lease, and the next run starts within one period), held
/// for two cron periods (the publisher's own hold for an unanswered source).
pub const PUBLISHER_DOWN_MS: u64 =
    PUBLISHER_LEASE_MS + PUBLISHER_CRON_PERIOD_MS + PUBLISHER_UNREACHABLE_HOLD_MS;

/// The Workers whose `/status` the relay reads while the publisher is
/// silent, by subject.
pub const WORKERS: [&str; 2] = ["coordinator", "eval_worker"];

/// The rule bounds, each derived from a deployment parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// A run killed mid-step blocks the next runs' alert phase for one lease;
    /// the next run starts within one cron period of its end.
    pub silent_after_ms: u64,
    /// Two missed runs: the publisher's hold for an unanswered source.
    pub silent_hold_ms: u64,
    /// Two runs whose posts were not accepted: the same hold, for the
    /// publisher's alert sink (a single failure is retried by the next run).
    pub undelivered_hold_ms: u64,
    /// The publisher's `watermark_stalled` bound (its watchdog silence plus
    /// two cron periods), from its last heartbeat.
    pub watermark_stall_ms: u64,
    pub unreachable_hold_ms: u64,
    /// Two of the tracker's schedule periods.
    pub tracker_stale_ms: u64,
    pub tracker_period_ms: u64,
}

impl Bounds {
    pub fn derive(watermark_stall_ms: u64, tracker_period_ms: u64) -> Self {
        Self {
            silent_after_ms: PUBLISHER_LEASE_MS + PUBLISHER_CRON_PERIOD_MS,
            silent_hold_ms: PUBLISHER_UNREACHABLE_HOLD_MS,
            undelivered_hold_ms: PUBLISHER_UNREACHABLE_HOLD_MS,
            watermark_stall_ms,
            unreachable_hold_ms: PUBLISHER_UNREACHABLE_HOLD_MS,
            tracker_stale_ms: tracker_period_ms.saturating_mul(2),
            tracker_period_ms,
        }
    }
}

/// The last heartbeat the relay received (relay clock).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatRecord {
    pub received_ms: u64,
    pub run_started_ms: u64,
    pub renotify_ms: u64,
    /// The publisher's own `watermark_stalled` bound.
    pub watermark_stall_ms: u64,
    pub config_refused: bool,
    pub rules: String,
    pub post_schema: String,
    pub undelivered: u64,
    pub undelivered_since_ms: Option<u64>,
}

/// What the relay refused from the publisher since it last accepted a post
/// or a heartbeat (its Durable Object's `intake` row). A refusal is a 4xx
/// answer for the request's content (size, schema, network, ids), never a
/// relay-side failure, which the publisher retries and reports in its next
/// heartbeat.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intake {
    pub posts_refused: u64,
    pub posts_refused_since_ms: Option<u64>,
    pub post_refusal: Option<String>,
    pub heartbeats_refused: u64,
    pub heartbeats_refused_since_ms: Option<u64>,
    pub heartbeat_refusal: Option<String>,
}

impl Intake {
    pub fn post(&mut self, refusal: Option<&str>, now_ms: u64) {
        match refusal {
            Some(code) => {
                self.posts_refused = self.posts_refused.saturating_add(1);
                self.posts_refused_since_ms.get_or_insert(now_ms);
                self.post_refusal = Some(code.to_owned());
            }
            None => {
                self.posts_refused = 0;
                self.posts_refused_since_ms = None;
                self.post_refusal = None;
            }
        }
    }

    pub fn heartbeat(&mut self, refusal: Option<&str>, now_ms: u64) {
        match refusal {
            Some(code) => {
                self.heartbeats_refused = self.heartbeats_refused.saturating_add(1);
                self.heartbeats_refused_since_ms.get_or_insert(now_ms);
                self.heartbeat_refusal = Some(code.to_owned());
            }
            None => {
                self.heartbeats_refused = 0;
                self.heartbeats_refused_since_ms = None;
                self.heartbeat_refusal = None;
            }
        }
    }
}

/// The renotify interval the relay's own alerts follow: the publisher's,
/// never below one cron period (a smaller value in a heartbeat is raised to
/// it, as the publisher's own configuration refuses it).
pub fn renotify_ms(h: Option<&HeartbeatRecord>) -> u64 {
    h.map_or(DEFAULT_RENOTIFY_MS, |h| h.renotify_ms)
        .max(MIN_RENOTIFY_MS)
}

/// Milliseconds since the publisher was last heard from, or since the relay
/// started watching.
pub fn silence_ms(h: Option<&HeartbeatRecord>, watch_since_ms: u64, now_ms: u64) -> u64 {
    now_ms.saturating_sub(h.map_or(watch_since_ms, |h| h.received_ms))
}

/// The publisher is down: silent for [`PUBLISHER_DOWN_MS`]. Only then does
/// the relay read the coordinator's status itself.
pub fn publisher_down(h: Option<&HeartbeatRecord>, watch_since_ms: u64, now_ms: u64) -> bool {
    silence_ms(h, watch_since_ms, now_ms) >= PUBLISHER_DOWN_MS
}

const _: () = assert!(
    PUBLISHER_DOWN_MS
        == PUBLISHER_LEASE_MS + PUBLISHER_CRON_PERIOD_MS + PUBLISHER_UNREACHABLE_HOLD_MS
);

/// Everything one tick observed.
pub struct Inputs<'a> {
    pub now_ms: u64,
    pub watch_since_ms: u64,
    pub heartbeat: Option<&'a HeartbeatRecord>,
    pub intake: &'a Intake,
    pub tracker: &'a tracker::Read,
    /// The last tick the bucket answered (this one when it did), or when the
    /// relay started watching.
    pub tracker_answered_ms: u64,
    /// Read only while the publisher is down: one per [`WORKERS`] entry.
    pub workers: Option<&'a [status::Read; 2]>,
    pub spend_figures: bool,
    /// The relay's optional settings that were ignored (their rule text,
    /// names only).
    pub config_warnings: &'a [String],
}

fn cond(rule: &'static str, subject: &str, detail: String, hold_ms: u64) -> Condition {
    Condition {
        rule,
        subject: subject.to_owned(),
        detail,
        hold_ms,
    }
}

fn worker_summary(name: &str, r: &status::Read, b: &Bounds) -> (String, Option<(String, u64)>) {
    match r {
        status::Read::NoToken => (format!("{name} status not read (no status token)"), None),
        status::Read::Unanswered => {
            let d = "status did not answer".to_owned();
            (format!("{name} {d}"), Some((d, b.unreachable_hold_ms)))
        }
        status::Read::Ok(w) => {
            let issues = worker_issues(w, b);
            let summary = match w.confirmed_age_ms {
                Some(age) => format!("{name} confirmed watermark {} s old", age / 1000),
                None => format!("{name} has no confirmed watermark"),
            };
            if issues.is_empty() {
                (summary, None)
            } else {
                let d = issues.join("; ");
                (format!("{name} {d}"), Some((d, 0)))
            }
        }
    }
}

fn worker_issues(w: &WorkerView, b: &Bounds) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(kind) = &w.fault {
        out.push(format!("journal fault {kind}"));
    }
    if w.signing_fault {
        out.push("signing fault".into());
    }
    match w.confirmed_age_ms {
        Some(age) if age < b.watermark_stall_ms => {}
        Some(age) => out.push(format!("confirmed watermark {} s old", age / 1000)),
        None => out.push("no confirmed watermark".into()),
    }
    out
}

/// Where a spend alert's figures start in its detail (only when
/// `SPEND_FIGURES_IN_EMAIL` is on): the alert feed cuts the detail there, so
/// no figure reaches it.
pub const FIGURES_LEAD: &str = " Whole account month to date";

fn spend_detail(month: &str, u: &Upload, figures: bool) -> String {
    let mut d = format!(
        "{month} is at or above the private spend alert threshold. Investigate (reports/{month}.md on \
         the tracker host), then acknowledge with track.sh --ack {month} and run upload-latest.sh."
    );
    if figures && u.month == month {
        let f = |v: Option<u64>| v.map_or_else(|| "unknown".to_owned(), tracker::usd);
        d.push_str(&format!(
            "{FIGURES_LEAD} {} (projected {}); testnet {} (projected {}); list-price \
             estimates as of {}.",
            f(u.account.mtd_microusd),
            f(u.account.projected_microusd),
            f(u.testnet.mtd_microusd),
            f(u.testnet.projected_microusd),
            u.generated_at
        ));
    }
    d
}

/// What a stale tracker's email suggests: the tracker uploads with the same
/// Cloudflare login it prices with, so an expired login usually shows here,
/// not as `spend_tracker_blind`.
const TRACKER_STALE_HINT: &str = "Check its timer on the tracker host (a reboot removes the \
     transient timer) and its Cloudflare login (wrangler whoami): an expired login stops the \
     uploads too.";

/// The delivery problems of the publisher's alert posts, if any.
fn undelivered_detail(h: Option<&HeartbeatRecord>, intake: &Intake) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(h) = h {
        if h.post_schema != POST_SCHEMA {
            parts.push(format!(
                "the publisher sends posts of schema {}, and this relay reads {POST_SCHEMA}",
                h.post_schema
            ));
        }
        if h.undelivered > 0 {
            let since = h
                .undelivered_since_ms
                .map(|t| format!(", in every run since {}", rfc3339(t)))
                .unwrap_or_default();
            parts.push(format!(
                "the publisher's run of {} left {} due alert events undelivered{since}",
                rfc3339(h.run_started_ms),
                h.undelivered
            ));
        }
    }
    if intake.posts_refused > 0 {
        parts.push(format!(
            "the relay refused {} of the publisher's posts since {} (last: {})",
            intake.posts_refused,
            intake
                .posts_refused_since_ms
                .map_or_else(|| "its last accepted post".to_owned(), rfc3339),
            intake.post_refusal.as_deref().unwrap_or("unknown")
        ));
    }
    (!parts.is_empty()).then(|| {
        format!(
            "{}. The publisher's own alerts are not being delivered. Check the relay's live log \
             (wrangler tail) and deploy both Workers from one commit.",
            parts.join("; ")
        )
    })
}

/// The conditions that hold this tick. `state` supplies the open tracker
/// alerts carried over while the upload is unknown.
pub fn conditions(i: &Inputs<'_>, b: &Bounds, state: &AlertState) -> Vec<Condition> {
    let mut out = Vec::new();
    let silence = silence_ms(i.heartbeat, i.watch_since_ms, i.now_ms);
    if silence > b.silent_after_ms {
        let mut detail = match i.heartbeat {
            Some(h) => format!(
                "no publisher heartbeat for {} s (last at {}): its alert phase has not finished since, \
                 so its own alerts may not be sent.",
                silence / 1000,
                rfc3339(h.received_ms)
            ),
            None => format!(
                "no publisher heartbeat since the relay started watching at {}.",
                rfc3339(i.watch_since_ms)
            ),
        };
        if i.intake.heartbeats_refused > 0 {
            detail.push_str(&format!(
                " The relay refused {} heartbeats since {} (last: {}): deploy both Workers from one \
                 commit.",
                i.intake.heartbeats_refused,
                i.intake
                    .heartbeats_refused_since_ms
                    .map_or_else(|| "its last accepted one".to_owned(), rfc3339),
                i.intake.heartbeat_refusal.as_deref().unwrap_or("unknown")
            ));
        }
        if let Some(reads) = i.workers {
            let summaries: Vec<String> = WORKERS
                .iter()
                .zip(reads.iter())
                .map(|(name, r)| worker_summary(name, r, b).0)
                .collect();
            detail.push_str(&format!(" Coordinator status: {}.", summaries.join("; ")));
        }
        out.push(cond(
            PUBLISHER_SILENT,
            "publisher",
            detail,
            b.silent_hold_ms,
        ));
    }
    if let Some(detail) = undelivered_detail(i.heartbeat, i.intake) {
        out.push(cond(
            PUBLISHER_UNDELIVERED,
            "publisher",
            detail,
            b.undelivered_hold_ms,
        ));
    }
    if let Some(reads) = i.workers {
        for (name, r) in WORKERS.iter().zip(reads.iter()) {
            if let (_, Some((issue, hold))) = worker_summary(name, r, b) {
                out.push(cond(
                    COORDINATOR_UNWATCHED,
                    name,
                    format!("the publisher is silent, and {name}: {issue}"),
                    hold,
                ));
            }
        }
    }
    if !i.config_warnings.is_empty() {
        out.push(cond(
            RELAY_CONFIG_WARNING,
            "relay",
            format!(
                "the relay ignored these settings and uses their defaults: {}. Fix the deploy-time \
                 vars or secrets (wrangler secret list --env testnet on the relay) and redeploy.",
                i.config_warnings.join("; ")
            ),
            0,
        ));
    }
    tracker_conditions(i, b, state, &mut out);
    out
}

/// A readable run that did not price everything: its totals are lower
/// bounds (the tracker's `undetermined`), whatever its alert fields say.
fn incomplete(u: &Upload) -> bool {
    u.exit_code != 0 || !u.complete || u.alert_state.as_deref() == Some("undetermined")
}

fn tracker_conditions(i: &Inputs<'_>, b: &Bounds, state: &AlertState, out: &mut Vec<Condition>) {
    let stale = |detail: String| cond(TRACKER_STALE, "tracker", detail, 0);
    let (why, carried): (Option<String>, &[&str]) = match i.tracker {
        tracker::Read::Ok {
            upload,
            generated_ms,
            uploaded_ms,
        } => {
            for m in &upload.alert_months {
                out.push(cond(
                    SPEND_ALERT,
                    m,
                    spend_detail(m, upload, i.spend_figures),
                    0,
                ));
            }
            // The output's age by the older of its own time and the bucket's
            // upload time, so a tracker clock ahead of Cloudflare's (or a bad
            // generated_at) never holds the age at zero.
            let fresh_ms = (*generated_ms).min(*uploaded_ms);
            let age = i.now_ms.saturating_sub(fresh_ms);
            if age > b.tracker_stale_ms {
                out.push(stale(format!(
                    "the spend tracker's newest output is {} h old (generated {}, uploaded {}); it runs \
                     every {} h. {TRACKER_STALE_HINT}",
                    age / 3_600_000,
                    upload.generated_at,
                    rfc3339(*uploaded_ms),
                    b.tracker_period_ms / 3_600_000
                )));
            }
            if incomplete(upload) {
                out.push(cond(
                    TRACKER_INCOMPLETE,
                    "tracker",
                    format!(
                        "the spend tracker's run of {} did not price everything (exit code {}, \
                         complete {}, {} failed queries, alert state {}); its totals are lower \
                         bounds, so a crossed threshold can go unreported. Read that run's log and \
                         the report's run failures on the tracker host.",
                        upload.generated_at,
                        upload.exit_code,
                        upload.complete,
                        upload.failures,
                        upload.alert_state.as_deref().unwrap_or("none")
                    ),
                    b.tracker_stale_ms,
                ));
            }
            if upload.auth != "ok" {
                out.push(cond(
                    TRACKER_BLIND,
                    "tracker",
                    format!(
                        "the spend tracker could not authenticate to Cloudflare (auth {}) in its run of {}; \
                         its usage figures are unknown until the login is refreshed.",
                        upload.auth, upload.generated_at
                    ),
                    0,
                ));
            }
            return;
        }
        tracker::Read::Missing => (
            Some("no spend tracker upload in the relay's bucket".to_owned()),
            &[SPEND_ALERT, TRACKER_BLIND, TRACKER_INCOMPLETE],
        ),
        tracker::Read::Unreadable(r) => (
            Some(format!("the spend tracker upload is unreadable ({r})")),
            &[SPEND_ALERT, TRACKER_BLIND, TRACKER_INCOMPLETE],
        ),
        // The bucket did not answer: nothing the tracker raised changes,
        // until it has not answered for as long as a stale tracker takes.
        tracker::Read::Unavailable
            if i.now_ms.saturating_sub(i.tracker_answered_ms) > b.tracker_stale_ms =>
        {
            (
                Some(format!(
                    "the relay's bucket has not answered since {}",
                    rfc3339(i.tracker_answered_ms)
                )),
                &[SPEND_ALERT, TRACKER_BLIND, TRACKER_INCOMPLETE],
            )
        }
        tracker::Read::Unavailable => (
            None,
            &[
                SPEND_ALERT,
                TRACKER_STALE,
                TRACKER_BLIND,
                TRACKER_INCOMPLETE,
            ],
        ),
    };
    if let Some(why) = why {
        out.push(stale(format!(
            "{why}; spend alerts cannot be checked. {TRACKER_STALE_HINT}"
        )));
    }
    // Unknown is not a resolution: what the tracker raised stays open.
    for e in state
        .entries
        .values()
        .filter(|e| carried.contains(&e.rule.as_str()) && !e.resolved)
    {
        let Some(rule) = carried.iter().copied().find(|r| *r == e.rule) else {
            continue;
        };
        out.push(Condition {
            rule,
            subject: e.subject.clone(),
            detail: e.detail.clone(),
            hold_ms: e.hold_ms,
        });
    }
}

/// Sets each resolving entry's detail to what resolved it.
pub fn resolution_notes(state: &mut AlertState, i: &Inputs<'_>) {
    let down = publisher_down(i.heartbeat, i.watch_since_ms, i.now_ms);
    let upload = match i.tracker {
        tracker::Read::Ok { upload, .. } => Some(upload),
        _ => None,
    };
    for e in state.resolving_mut() {
        e.detail = match e.rule.as_str() {
            PUBLISHER_SILENT => match i.heartbeat {
                Some(h) => format!(
                    "publisher heartbeat received at {}.",
                    rfc3339(h.received_ms)
                ),
                None => "the publisher is heard again.".into(),
            },
            PUBLISHER_UNDELIVERED => match i.heartbeat {
                Some(h) => format!(
                    "the publisher's alert posts are accepted again (heartbeat at {}).",
                    rfc3339(h.received_ms)
                ),
                None => "the publisher's alert posts are accepted again.".into(),
            },
            COORDINATOR_UNWATCHED if !down => {
                "the publisher's heartbeat is back; it watches this Worker again.".into()
            }
            COORDINATOR_UNWATCHED => "the Worker's status is healthy again.".into(),
            SPEND_ALERT => match upload.and_then(|u| tracker::acked_ms(u, &e.subject)) {
                Some(at) if at >= e.first_seen_ms => {
                    format!("acknowledged at {} (track.sh --ack).", rfc3339(at))
                }
                _ => "no longer an active alert month in the tracker's output.".into(),
            },
            TRACKER_STALE => match upload {
                Some(u) => format!(
                    "the spend tracker's output is current again (generated {}).",
                    u.generated_at
                ),
                None => "the spend tracker's output is current again.".into(),
            },
            TRACKER_BLIND => "the spend tracker authenticated again.".into(),
            TRACKER_INCOMPLETE => match upload {
                Some(u) => format!(
                    "the spend tracker's run of {} priced everything again.",
                    u.generated_at
                ),
                None => "the spend tracker priced everything again.".into(),
            },
            RELAY_CONFIG_WARNING => "every relay setting parses again.".into(),
            _ => "cleared.".into(),
        };
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tracker::tests::upload;
    use slop_ninja_alerts_wire::{EventKind, acknowledge, due, observe};

    pub const T0: u64 = 1_793_627_107_000;

    pub fn bounds() -> Bounds {
        Bounds::derive(
            slop_ninja_alerts_wire::watermark_stall_ms(300_000),
            86_400_000,
        )
    }

    pub fn beat(at: u64) -> HeartbeatRecord {
        HeartbeatRecord {
            received_ms: at,
            run_started_ms: at,
            renotify_ms: 6 * 3_600_000,
            watermark_stall_ms: 420_000,
            config_refused: false,
            rules: "slop_ninja.publisher.alerts/v3".into(),
            post_schema: POST_SCHEMA.into(),
            undelivered: 0,
            undelivered_since_ms: None,
        }
    }

    /// A readable upload, stored when it was generated.
    pub fn fresh(alerts: &[&str]) -> tracker::Read {
        at_upload(upload(alerts))
    }

    fn at_upload(u: Upload) -> tracker::Read {
        let generated_ms = crate::mime::parse_rfc3339(&u.generated_at).unwrap();
        tracker::Read::Ok {
            upload: Box::new(u),
            generated_ms,
            uploaded_ms: generated_ms,
        }
    }

    fn rules_of(c: &[Condition]) -> Vec<(&'static str, String)> {
        c.iter().map(|c| (c.rule, c.subject.clone())).collect()
    }

    const NO_INTAKE: Intake = Intake {
        posts_refused: 0,
        posts_refused_since_ms: None,
        post_refusal: None,
        heartbeats_refused: 0,
        heartbeats_refused_since_ms: None,
        heartbeat_refusal: None,
    };

    fn inputs<'a>(
        now: u64,
        hb: Option<&'a HeartbeatRecord>,
        tr: &'a tracker::Read,
        workers: Option<&'a [status::Read; 2]>,
    ) -> Inputs<'a> {
        Inputs {
            now_ms: now,
            watch_since_ms: T0 - 1,
            heartbeat: hb,
            intake: &NO_INTAKE,
            tracker: tr,
            tracker_answered_ms: now,
            workers,
            spend_figures: false,
            config_warnings: &[],
        }
    }

    #[test]
    fn bounds_derive_from_the_publisher() {
        let b = bounds();
        assert_eq!(b.silent_after_ms, 3 * 60_000);
        assert_eq!(b.silent_hold_ms, 2 * 60_000);
        assert_eq!(b.undelivered_hold_ms, 2 * 60_000);
        assert_eq!(b.watermark_stall_ms, 300_000 + 120_000);
        assert_eq!(b.tracker_stale_ms, 2 * 86_400_000);
    }

    /// A heartbeat's renotify interval below one cron period (the publisher
    /// refuses it in its configuration; a skewed or hand-made heartbeat
    /// might still carry it) is raised to one period, so 0 never means a
    /// reminder on every evaluation.
    #[test]
    fn renotify_is_never_below_a_cron_period() {
        let mut h = beat(T0);
        h.renotify_ms = 0;
        assert_eq!(renotify_ms(Some(&h)), MIN_RENOTIFY_MS);
        h.renotify_ms = 7_200_000;
        assert_eq!(renotify_ms(Some(&h)), 7_200_000);
        assert_eq!(renotify_ms(None), DEFAULT_RENOTIFY_MS);
        // An open alert, evaluated twice within one period (an overlapping
        // tick): one reminder, not two.
        let b = bounds();
        let mut st = AlertState::default();
        let tr = tracker::Read::Missing;
        h.renotify_ms = 0;
        let mut sent = 0;
        for now in [T0, T0 + 60_000, T0 + 90_000] {
            let i = inputs(now, Some(&h), &tr, None);
            let c = conditions(&i, &b, &st);
            observe(&mut st, RULES_ID, &c, now);
            let ev = due(&st, now, renotify_ms(Some(&h)));
            sent += ev.len();
            acknowledge(&mut st, &ev, now);
        }
        assert_eq!(sent, 2, "the firing, then one reminder a period later");
    }

    /// The dead-man: silent once the heartbeat is older than a lease plus a
    /// period, firing after two more periods, down (coordinator reads) from
    /// then, and resolved by the next heartbeat.
    #[test]
    fn publisher_silence_is_derived_and_resolves() {
        let b = bounds();
        let tr = fresh(&[]);
        let hb = beat(T0);
        let at = |now: u64, workers| inputs(now, Some(&hb), &tr, workers);
        let st = AlertState::default();
        assert!(conditions(&at(T0 + b.silent_after_ms, None), &b, &st).is_empty());
        let c = conditions(&at(T0 + b.silent_after_ms + 1, None), &b, &st);
        assert_eq!(rules_of(&c), vec![(PUBLISHER_SILENT, "publisher".into())]);
        assert_eq!(c[0].hold_ms, b.silent_hold_ms);
        assert_eq!(PUBLISHER_DOWN_MS, b.silent_after_ms + b.silent_hold_ms);
        assert!(!publisher_down(Some(&hb), 0, T0 + PUBLISHER_DOWN_MS - 1));
        assert!(publisher_down(Some(&hb), 0, T0 + PUBLISHER_DOWN_MS));
        // Never heard: counted from when the relay started watching.
        let none = Inputs {
            heartbeat: None,
            ..at(T0 + 10 * 60_000, None)
        };
        let c = conditions(&none, &b, &st);
        assert!(c[0].detail.contains("since the relay started watching"));
        // Heartbeats that arrive but are refused say so.
        let refused = Intake {
            heartbeats_refused: 4,
            heartbeats_refused_since_ms: Some(T0),
            heartbeat_refusal: Some("schema".into()),
            ..Intake::default()
        };
        let c = conditions(
            &Inputs {
                intake: &refused,
                ..none
            },
            &b,
            &st,
        );
        assert!(
            c[0].detail.contains("refused 4 heartbeats since"),
            "{}",
            c[0].detail
        );
        assert!(c[0].detail.contains("(last: schema)"));
        // Through the state machine: fires at bound + hold, then resolves.
        let mut st = AlertState::default();
        let start = T0 + b.silent_after_ms + 1;
        let c = conditions(&at(start, None), &b, &st);
        observe(&mut st, RULES_ID, &c, start);
        assert!(due(&st, start, renotify_ms(Some(&hb))).is_empty());
        let fire = start + b.silent_hold_ms;
        let reads = [status::Read::Unanswered, status::Read::NoToken];
        let c = conditions(&at(fire, Some(&reads)), &b, &st);
        observe(&mut st, RULES_ID, &c, fire);
        let ev = due(&st, fire, renotify_ms(Some(&hb)));
        assert_eq!(
            ev.len(),
            1,
            "the unanswered worker holds two periods: {ev:?}"
        );
        assert!(ev[0].detail.contains("Coordinator status: coordinator status did not answer; eval_worker status not read (no status token)"));
        acknowledge(&mut st, &ev, fire);
        let back = beat(fire + 1);
        let i = inputs(fire + 2, Some(&back), &tr, None);
        let c = conditions(&i, &b, &st);
        observe(&mut st, RULES_ID, &c, fire + 2);
        resolution_notes(&mut st, &i);
        let ev = due(&st, fire + 2, renotify_ms(Some(&back)));
        let resolved: Vec<_> = ev
            .iter()
            .filter(|e| e.state == EventKind::Resolved)
            .collect();
        assert_eq!(resolved.len(), 1);
        assert!(
            resolved[0]
                .detail
                .starts_with("publisher heartbeat received at")
        );
    }

    /// Heartbeats keep arriving while the publisher's alert posts are
    /// refused or keep failing (a wire skew between the two builds, an
    /// oversized event): the relay emails that, after two cron periods.
    #[test]
    fn undelivered_publisher_alerts_are_reported() {
        let b = bounds();
        let tr = fresh(&[]);
        let mut st = AlertState::default();
        let run = |st: &mut AlertState, hb: &HeartbeatRecord, intake: &Intake, now: u64| {
            let i = Inputs {
                intake,
                ..inputs(now, Some(hb), &tr, None)
            };
            let c = conditions(&i, &b, st);
            observe(st, RULES_ID, &c, now);
            resolution_notes(st, &i);
            let ev = due(st, now, renotify_ms(Some(hb)));
            acknowledge(st, &ev, now);
            ev
        };
        // One failed run is retried by the next: nothing.
        let mut hb = beat(T0);
        hb.undelivered = 3;
        hb.undelivered_since_ms = Some(T0);
        assert!(run(&mut st, &hb, &NO_INTAKE, T0).is_empty());
        let mut ok = beat(T0 + 60_000);
        ok.undelivered = 0;
        assert!(run(&mut st, &ok, &NO_INTAKE, T0 + 60_000).is_empty());
        assert!(st.entries.is_empty());
        // Failing for two cron periods while heartbeats arrive: emailed.
        for n in 0..3 {
            let now = T0 + 120_000 + n * 60_000;
            hb.received_ms = now;
            hb.run_started_ms = now;
            hb.undelivered_since_ms = Some(T0 + 120_000);
            let ev = run(&mut st, &hb, &NO_INTAKE, now);
            if n < 2 {
                assert!(ev.is_empty(), "inside the hold at {n}");
            } else {
                assert_eq!(ev.len(), 1);
                assert_eq!(
                    (ev[0].state, ev[0].rule.as_str()),
                    (EventKind::Firing, PUBLISHER_UNDELIVERED)
                );
                assert!(ev[0].detail.contains("left 3 due alert events undelivered"));
            }
        }
        // Delivered again: resolved.
        let mut ok = beat(T0 + 600_000);
        ok.undelivered = 0;
        let ev = run(&mut st, &ok, &NO_INTAKE, T0 + 600_000);
        assert_eq!(ev[0].state, EventKind::Resolved);
        assert!(
            ev[0]
                .detail
                .starts_with("the publisher's alert posts are accepted again")
        );
        // A schema skew, and posts the relay refused, are reported too.
        let mut skew = beat(T0);
        skew.post_schema = "slop_ninja.publisher.alert/v2".into();
        let refused = Intake {
            posts_refused: 7,
            posts_refused_since_ms: Some(T0),
            post_refusal: Some("event_id".into()),
            ..Intake::default()
        };
        let i = Inputs {
            intake: &refused,
            ..inputs(T0, Some(&skew), &tr, None)
        };
        let c = conditions(&i, &b, &AlertState::default());
        assert_eq!(
            rules_of(&c),
            vec![(PUBLISHER_UNDELIVERED, "publisher".into())]
        );
        assert!(
            c[0].detail
                .contains("sends posts of schema slop_ninja.publisher.alert/v2")
        );
        assert!(c[0].detail.contains("refused 7 of the publisher's posts"));
        assert!(c[0].detail.contains("(last: event_id)"));
        assert_eq!(c[0].hold_ms, b.undelivered_hold_ms);
        // The intake record: a refusal counts, an accepted post clears it.
        let mut intake = Intake::default();
        intake.post(Some("schema"), T0);
        intake.post(Some("too_large"), T0 + 5);
        assert_eq!(
            (intake.posts_refused, intake.posts_refused_since_ms),
            (2, Some(T0))
        );
        assert_eq!(intake.post_refusal.as_deref(), Some("too_large"));
        intake.post(None, T0 + 9);
        intake.heartbeat(Some("malformed"), T0 + 9);
        assert_eq!(intake.posts_refused, 0);
        assert_eq!(intake.heartbeats_refused, 1);
        intake.heartbeat(None, T0 + 10);
        assert_eq!(intake, Intake::default());
    }

    /// While the publisher is down, the relay checks the coordinator itself
    /// with the publisher's bounds.
    #[test]
    fn coordinator_checks_while_the_publisher_is_down() {
        let b = bounds();
        let tr = fresh(&[]);
        let stale = WorkerView {
            confirmed_age_ms: Some(b.watermark_stall_ms),
            ..Default::default()
        };
        let ok = WorkerView {
            confirmed_age_ms: Some(1000),
            ..Default::default()
        };
        let reads = [status::Read::Ok(stale), status::Read::Ok(ok)];
        let i = Inputs {
            watch_since_ms: T0,
            ..inputs(T0 + 3_600_000, None, &tr, Some(&reads))
        };
        let c = conditions(&i, &b, &AlertState::default());
        assert_eq!(
            rules_of(&c),
            vec![
                (PUBLISHER_SILENT, "publisher".into()),
                (COORDINATOR_UNWATCHED, "coordinator".into())
            ]
        );
        assert_eq!(c[1].hold_ms, 0);
        assert!(c[1].detail.contains("confirmed watermark 420 s old"));
        let faulty = WorkerView {
            confirmed_age_ms: Some(1),
            fault: Some("gap".into()),
            signing_fault: true,
        };
        let reads = [status::Read::Ok(faulty), status::Read::Unanswered];
        let c = conditions(
            &Inputs {
                workers: Some(&reads),
                ..i
            },
            &b,
            &AlertState::default(),
        );
        assert!(c[1].detail.ends_with("journal fault gap; signing fault"));
        assert_eq!(
            (c[2].subject.as_str(), c[2].hold_ms),
            ("eval_worker", b.unreachable_hold_ms)
        );
    }

    /// Spend alerts: one per active month; unknown keeps them open; an
    /// acknowledgment resolves with its time; figures only when enabled and
    /// never for another month than the run's.
    #[test]
    fn tracker_alert_transitions_and_acks() {
        let b = bounds();
        let now = T0 + 60_000;
        let hb = beat(now);
        let mut st = AlertState::default();
        let run = |st: &mut AlertState, tr: &tracker::Read, now: u64, figures: bool| {
            let i = Inputs {
                spend_figures: figures,
                ..inputs(now, Some(&hb), tr, None)
            };
            let c = conditions(&i, &b, st);
            observe(st, RULES_ID, &c, now);
            resolution_notes(st, &i);
            let ev = due(st, now, renotify_ms(Some(&hb)));
            acknowledge(st, &ev, now);
            ev
        };
        assert!(run(&mut st, &fresh(&[]), now, false).is_empty());
        // alert_any becomes true for 2026-11 (and the unacknowledged 2026-10).
        let ev = run(&mut st, &fresh(&["2026-10", "2026-11"]), now + 1, true);
        assert_eq!(ev.len(), 2);
        assert!(
            ev.iter()
                .all(|e| e.state == EventKind::Firing && e.rule == SPEND_ALERT)
        );
        let oct = ev.iter().find(|e| e.subject == "2026-10").unwrap();
        let nov = ev.iter().find(|e| e.subject == "2026-11").unwrap();
        assert!(
            !oct.detail.contains('$'),
            "figures only for the run's month"
        );
        assert!(nov.detail.contains("$12.34") && nov.detail.contains("track.sh --ack 2026-11"));
        assert!(
            nov.detail.contains("run upload-latest.sh") && !nov.detail.contains("ask the user")
        );
        // Without the figures flag, no amounts at all.
        let mut plain = AlertState::default();
        let ev = run(&mut plain, &fresh(&["2026-11"]), now, false);
        assert!(!ev[0].detail.contains('$'));
        // The same upload again: nothing new.
        assert!(run(&mut st, &fresh(&["2026-10", "2026-11"]), now + 2, true).is_empty());
        // The upload goes missing: a stale alert, the spend alerts stay open.
        let ev = run(&mut st, &tracker::Read::Missing, now + 3, true);
        assert_eq!(
            rules_of_events(&ev),
            vec![(TRACKER_STALE.into(), "tracker".into())]
        );
        // 2026-11 acknowledged (track.sh --ack), 2026-10 still active.
        let mut u = upload(&["2026-10"]);
        u.acked = vec![tracker::Acked {
            month: "2026-11".into(),
            acked_at: crate::mime::rfc3339(now + 2_000),
        }];
        let ev = run(&mut st, &at_upload(u), now + 5_000, true);
        let mut kinds: Vec<_> = ev
            .iter()
            .map(|e| (e.state, e.rule.as_str(), e.subject.as_str()))
            .collect();
        kinds.sort();
        assert_eq!(
            kinds,
            vec![
                (EventKind::Resolved, SPEND_ALERT, "2026-11"),
                (EventKind::Resolved, TRACKER_STALE, "tracker"),
            ]
        );
        let ack = ev.iter().find(|e| e.subject == "2026-11").unwrap();
        assert!(ack.detail.starts_with("acknowledged at"), "{}", ack.detail);
        // 2026-10 falls below without an acknowledgment.
        let ev = run(&mut st, &fresh(&[]), now + 6_000, true);
        assert_eq!(ev.len(), 1);
        assert_eq!(
            ev[0].detail,
            "no longer an active alert month in the tracker's output."
        );
        // A month that alerts again after its acknowledgment fires again.
        let ev = run(&mut st, &fresh(&["2026-11"]), now + 7_000, true);
        assert_eq!(
            (ev[0].state, ev[0].subject.as_str()),
            (EventKind::Firing, "2026-11")
        );
    }

    fn rules_of_events(ev: &[slop_ninja_alerts_wire::Event]) -> Vec<(String, String)> {
        ev.iter()
            .map(|e| (e.rule.clone(), e.subject.clone()))
            .collect()
    }

    /// Two schedule periods without fresh output, and a blind run.
    #[test]
    fn tracker_staleness_and_auth() {
        let b = bounds();
        let tr = fresh(&[]);
        let tracker::Read::Ok { generated_ms, .. } = &tr else {
            unreachable!()
        };
        let hb = beat(generated_ms + 2 * 86_400_000);
        let at = |now: u64, tr: &'_ tracker::Read| {
            rules_of(&conditions(
                &inputs(now, Some(&hb), tr, None),
                &b,
                &AlertState::default(),
            ))
        };
        assert!(at(generated_ms + b.tracker_stale_ms, &tr).is_empty());
        assert_eq!(
            at(generated_ms + b.tracker_stale_ms + 1, &tr),
            vec![(TRACKER_STALE, "tracker".into())]
        );
        let c = conditions(
            &inputs(generated_ms + b.tracker_stale_ms + 1, Some(&hb), &tr, None),
            &b,
            &AlertState::default(),
        );
        assert!(c[0].detail.contains("wrangler whoami"), "{}", c[0].detail);
        let mut u = upload(&[]);
        u.auth = "expired".into();
        let blind = at_upload(u);
        assert_eq!(
            at(generated_ms + 1, &blind),
            vec![(TRACKER_BLIND, "tracker".into())]
        );
        let bad = tracker::Read::Unreadable("schema");
        assert_eq!(
            at(generated_ms + 1, &bad),
            vec![(TRACKER_STALE, "tracker".into())]
        );
        // A bucket that does not answer raises nothing and resolves nothing.
        assert!(at(generated_ms + 1, &tracker::Read::Unavailable).is_empty());
        let mut st = AlertState::default();
        let i = inputs(generated_ms + 1, Some(&hb), &blind, None);
        let c = conditions(&i, &b, &st);
        observe(&mut st, RULES_ID, &c, generated_ms + 1);
        let ev = due(&st, generated_ms + 1, 1);
        acknowledge(&mut st, &ev, generated_ms + 1);
        let gone = Inputs {
            tracker: &tracker::Read::Unavailable,
            ..i
        };
        let c = conditions(&gone, &b, &st);
        observe(&mut st, RULES_ID, &c, generated_ms + 2);
        assert!(!st.get(TRACKER_BLIND, "tracker").unwrap().resolved);
    }

    /// A `generated_at` in the future (a tracker clock ahead of Cloudflare's,
    /// or a bad upload) does not hold the age at zero: the bucket's upload
    /// time bounds it.
    #[test]
    fn a_future_generated_at_still_goes_stale() {
        let b = bounds();
        let mut u = upload(&[]);
        u.generated_at = "2027-11-02T13:06:08Z".into();
        let tr = tracker::Read::Ok {
            upload: Box::new(u),
            generated_ms: crate::mime::parse_rfc3339("2027-11-02T13:06:08Z").unwrap(),
            uploaded_ms: T0,
        };
        let mut st = AlertState::default();
        let mut sent = Vec::new();
        for day in 0..10u64 {
            let now = T0 + day * 86_400_000;
            let hb = beat(now);
            let i = inputs(now, Some(&hb), &tr, None);
            let c = conditions(&i, &b, &st);
            observe(&mut st, RULES_ID, &c, now);
            let ev = due(&st, now, renotify_ms(Some(&hb)));
            acknowledge(&mut st, &ev, now);
            sent.extend(ev.into_iter().map(|e| (e.state, e.rule)));
        }
        assert_eq!(sent[0], (EventKind::Firing, TRACKER_STALE.to_owned()));
    }

    /// A bucket that stays unavailable (deleted, binding broken, persistent
    /// errors) is reported once it has not answered for as long as a stale
    /// tracker takes; until then nothing the tracker raised changes.
    #[test]
    fn a_bucket_that_never_answers_goes_stale() {
        let b = bounds();
        let tr = tracker::Read::Unavailable;
        let mut st = AlertState::default();
        let mut sent = Vec::new();
        for day in 0..10u64 {
            let now = T0 + day * 86_400_000;
            let hb = beat(now);
            let i = Inputs {
                tracker_answered_ms: T0,
                ..inputs(now, Some(&hb), &tr, None)
            };
            let c = conditions(&i, &b, &st);
            observe(&mut st, RULES_ID, &c, now);
            let ev = due(&st, now, renotify_ms(Some(&hb)));
            acknowledge(&mut st, &ev, now);
            sent.extend(ev.into_iter().map(|e| (e.state, e.rule, e.detail)));
        }
        assert_eq!(sent[0].0, EventKind::Firing);
        assert_eq!(sent[0].1, TRACKER_STALE);
        assert!(
            sent[0]
                .2
                .starts_with("the relay's bucket has not answered since 2026-11-02T")
        );
        assert!(sent.iter().all(|e| e.0 != EventKind::Resolved));
    }

    /// A missing or unreadable upload keeps a blind alert open (unknown is
    /// not a resolution); a readable, still-blind upload does not fire it
    /// again; a readable authenticated one resolves it.
    #[test]
    fn unknown_uploads_never_resolve_a_blind_tracker() {
        let b = bounds();
        let mut u = upload(&[]);
        u.auth = "expired".into();
        let blind = at_upload(u);
        let now = T0;
        let hb = beat(now);
        let mut st = AlertState::default();
        let mut run = |tr: &tracker::Read, now: u64| {
            let i = inputs(now, Some(&hb), tr, None);
            let c = conditions(&i, &b, &st);
            observe(&mut st, RULES_ID, &c, now);
            resolution_notes(&mut st, &i);
            let ev = due(&st, now, renotify_ms(Some(&hb)));
            acknowledge(&mut st, &ev, now);
            ev.into_iter()
                .map(|e| (e.state, e.rule))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            run(&blind, now),
            vec![(EventKind::Firing, TRACKER_BLIND.to_owned())]
        );
        assert_eq!(
            run(&tracker::Read::Unreadable("malformed"), now + 60_000),
            vec![(EventKind::Firing, TRACKER_STALE.to_owned())]
        );
        assert_eq!(run(&tracker::Read::Missing, now + 120_000), vec![]);
        let mut back = run(&blind, now + 180_000);
        back.sort();
        assert_eq!(
            back,
            vec![(EventKind::Resolved, TRACKER_STALE.to_owned())],
            "the blind alert stays open, never fired again"
        );
        let mut ok = run(&fresh(&[]), now + 240_000);
        ok.sort();
        assert_eq!(ok, vec![(EventKind::Resolved, TRACKER_BLIND.to_owned())]);
    }

    /// A tracker whose readable runs keep failing to price something (rate
    /// limits, scope or API changes) while it authenticates: emailed once
    /// that has lasted two schedule periods, with the failure count and no
    /// failure text; a complete run resolves it; unknown keeps it open.
    #[test]
    fn incomplete_tracker_runs_are_reported() {
        let b = bounds();
        let mut u = upload(&[]);
        u.exit_code = 2;
        u.complete = false;
        u.failures = 8;
        u.alert_state = Some("undetermined".into());
        let partial = at_upload(u);
        let mut st = AlertState::default();
        let hb = beat(T0);
        let mut run = |tr: &tracker::Read, now: u64| {
            let i = inputs(now, Some(&hb), tr, None);
            let c = conditions(&i, &b, &st);
            observe(&mut st, RULES_ID, &c, now);
            resolution_notes(&mut st, &i);
            let ev = due(&st, now, renotify_ms(Some(&hb)));
            acknowledge(&mut st, &ev, now);
            ev
        };
        // Generated at T0's upload time; read daily (within the stale bound).
        let start = T0 - 3_600_000;
        assert!(run(&partial, start).is_empty(), "inside its hold");
        assert!(run(&partial, start + b.tracker_stale_ms - 1).is_empty());
        let ev = run(&partial, start + b.tracker_stale_ms);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(
            (ev[0].state, ev[0].rule.as_str()),
            (EventKind::Firing, TRACKER_INCOMPLETE)
        );
        assert!(
            ev[0]
                .detail
                .contains("exit code 2, complete false, 8 failed queries")
        );
        assert!(ev[0].detail.contains("alert state undetermined"));
        // Unknown keeps it open; a complete run resolves it.
        let later = start + b.tracker_stale_ms + 60_000;
        let ev = run(&tracker::Read::Missing, later);
        assert!(ev.iter().all(|e| e.rule != TRACKER_INCOMPLETE));
        let ev = run(&fresh(&[]), later + 60_000);
        let z: Vec<_> = ev.iter().filter(|e| e.rule == TRACKER_INCOMPLETE).collect();
        assert_eq!(z.len(), 1);
        assert_eq!(z[0].state, EventKind::Resolved);
        assert!(z[0].detail.ends_with("priced everything again."));
        // An undetermined month alone (complete data elsewhere) counts too.
        let mut u = upload(&[]);
        u.alert_state = Some("undetermined".into());
        assert!(incomplete(&u));
        assert!(!incomplete(&upload(&[])));
    }

    /// An optional relay setting that did not parse is emailed by name.
    #[test]
    fn relay_config_warnings_are_reported() {
        let b = bounds();
        let tr = fresh(&[]);
        let hb = beat(T0);
        let warnings = vec!["SPEND_FIGURES_IN_EMAIL must be true or false".to_owned()];
        let i = Inputs {
            config_warnings: &warnings,
            ..inputs(T0, Some(&hb), &tr, None)
        };
        let c = conditions(&i, &b, &AlertState::default());
        assert_eq!(rules_of(&c), vec![(RELAY_CONFIG_WARNING, "relay".into())]);
        assert!(
            c[0].detail
                .contains("SPEND_FIGURES_IN_EMAIL must be true or false")
        );
        assert_eq!(c[0].hold_ms, 0);
    }
}
