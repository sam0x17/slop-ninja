//! The email path's policy (`slop_ninja.alerts_relay.email/v1`): which
//! events are emailed, and how often. Email is for the critical rules the
//! operator lists; the alert feed carries every event whatever this says,
//! and none of this applies while email is off.
//!
//! - **Rule list.** Only events of the rules in `ALERT_EMAIL_RULES` are
//!   emailed ([`DEFAULT_RULES`] when unset); every other rule goes to the
//!   feed alone. The list covers the publisher's rules and the relay's own;
//!   a listed name that neither sender raises ([`known`]) is kept, with a
//!   warning.
//! - **One email per episode, and one at its resolution.** An episode is
//!   one condition from when it is first seen to its resolution: its rule,
//!   its subject and when it was first seen, whatever the sender's rule
//!   set, so a rule set change in the middle of an episode neither opens it
//!   again nor loses its resolution. It is *opened* by its firing's email,
//!   or, when the firing was deferred (below) or came while email was off,
//!   by the first reminder that passes the limits. Later reminders are not
//!   emailed. Its resolution is emailed when the episode was opened (and
//!   never counts against the limits: it is bounded by the openings); the
//!   resolution of an episode that was never opened is not.
//! - **Per-subject minimum interval.** An opening is deferred while the
//!   last opening of the same rule and subject is younger than the minimum
//!   interval (`ALERT_EMAIL_MIN_INTERVAL_SECS`, default
//!   [`DEFAULT_MIN_INTERVAL_MS`]: the publisher's default renotify interval,
//!   so by default email speaks about one subject no more often than the
//!   feed restates an open condition). The episode stays unopened and its
//!   next reminder (one renotify interval later) is considered again: a
//!   condition that keeps firing is emailed within about one renotify
//!   interval of the limit allowing it, and one that flaps is emailed at
//!   most once per interval.
//! - **Daily limit.** At most [`Policy::daily_limit`] openings per UTC day:
//!   the number of listed rules times the openings one subject can have in
//!   a day at the minimum interval. It binds only when more subjects open
//!   than one per listed rule (a storm of cohort faults, say), and defers
//!   openings the same way.
//! - **Reservations.** An opening is reserved (the episode, the opening
//!   event, the subject's time and the day's count) in the same synchronous
//!   step that claims its event in the ledger, and given back when the send
//!   is refused (nothing went out), so the sender's retry is not deferred
//!   by its own refused attempt. A send that timed out keeps its
//!   reservation: it may have gone out, and its retry goes through the
//!   ledger ([`crate::relay`]: an event with a ledger row is never decided
//!   again). An attempt that stopped before its send settled (an isolate
//!   that ended mid-send) also keeps it; the opening event's own retry is
//!   then sent under it ([`decide`]), however its earlier retries ended;
//!   any other event of the episode is still skipped as opened.
//!
//! The rows live in the Durable Object's `email_limits` table
//! ([`crate::ledger::Table`]): `episode:<hash of rule, subject and first
//! seen>` (opened or deferred; the last time an event of the episode was
//! seen), `opening:<hash of the opening event>` (until it is sent),
//! `subject:<hash of rule and subject>` (the last opening), and per UTC day
//! `opened:<day>` and `deferred:<day>` (episodes first deferred that day).
//! No key depends on the sender's rule set. [`sweep`] deletes rows
//! untouched for [`retention_ms`]: a whole day or the interval (the day's
//! counts and the subject's last opening), plus two ledger retentions (an
//! open episode's row is touched by each of its reminders, one renotify
//! interval apart, so it survives one missed reminder). A sweep that
//! deletes a row records its cutoff; a resolution whose episode has no row
//! is emailed when the episode began before the cutoff (its row may have
//! been swept), so an episode whose firing was emailed is never left
//! without its resolution. An emailed episode whose sender is silent past
//! the retention loses its row, and its next reminder opens it again (the
//! message says it is the first email on record about the condition).

use sha2::{Digest, Sha256};
use slop_ninja_alerts_wire::{DEFAULT_RENOTIFY_MS, Event, EventKind};

use crate::ledger::{self, StoreError, Table};
use crate::rules;

pub const EMAIL_RULE: &str = "slop_ninja.alerts_relay.email/v1";

/// The rules emailed when `ALERT_EMAIL_RULES` is unset: the coordinator's
/// commits, its confirmed watermark and signing, a paused cohort (the
/// publisher's rules), the relay's stand-in for the first three while the
/// publisher is silent, and the relay's rule for publisher alerts that do
/// not reach it (so a critical publisher alert the relay refuses, or the
/// publisher cannot deliver, still reaches email as that rule).
pub const DEFAULT_RULES: [&str; 6] = [
    "cohort_fault",
    "commit_gap",
    rules::COORDINATOR_UNWATCHED,
    rules::PUBLISHER_UNDELIVERED,
    "signing_backing_off",
    "watermark_stalled",
];

/// The rules the publisher raises (a test checks this list against its
/// source).
pub const PUBLISHER_RULES: &[&str] = &[
    "attempts_extended",
    "auto_target_stalled",
    "cohort_fault",
    "commit_gap",
    "commit_stalled",
    "config_refused",
    "config_warning",
    "coordinator_reveal_mismatch",
    "durable_fault",
    "epoch_deferred",
    "fallback_too_long",
    "inclusion_unknown",
    "ledger_threshold",
    "ledger_threshold_unset",
    "live_dropped",
    "live_fault",
    "live_over_bound",
    "live_value_changed",
    "operator_clear_needed",
    "phase_overrun",
    "pointer_stale",
    "prefinal_mismatch",
    "publication_pending",
    "publication_refused",
    "publication_unverifiable",
    "retention_stalled",
    "revocation_unverifiable",
    "row_record_stalled",
    "run_interrupted",
    "scorer_budget_fault",
    "signing_backing_off",
    "status_access_warning",
    "status_incomplete",
    "status_unreachable",
    "watermark_stalled",
    "webhook_paused",
];

/// The relay's own rules ([`crate::rules`]).
pub const RELAY_RULES: &[&str] = &[
    rules::COORDINATOR_UNWATCHED,
    rules::PUBLISHER_SILENT,
    rules::PUBLISHER_UNDELIVERED,
    rules::RELAY_CONFIG_WARNING,
    rules::SPEND_ALERT,
    rules::TRACKER_BLIND,
    rules::TRACKER_INCOMPLETE,
    rules::TRACKER_STALE,
];

/// Whether `rule` is one the publisher or the relay raises. A listed name
/// that is not is kept on the list (a newer publisher may raise it) and
/// named in a warning by its setting ([`crate::config`]), so a misspelt
/// name, which would never email, does not go unnoticed.
pub fn known(rule: &str) -> bool {
    PUBLISHER_RULES.contains(&rule) || RELAY_RULES.contains(&rule)
}

/// The default per-subject minimum interval: the publisher's default
/// renotify interval (6 hours).
pub const DEFAULT_MIN_INTERVAL_MS: u64 = DEFAULT_RENOTIFY_MS;

/// One UTC day.
pub const DAY_MS: u64 = 86_400_000;

/// The email path's rules and limits, from the configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// The rules emailed, sorted and without repeats.
    pub rules: Vec<String>,
    /// The per-subject minimum interval between openings (positive).
    pub min_interval_ms: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self::new(
            DEFAULT_RULES.iter().map(|r| (*r).to_owned()).collect(),
            DEFAULT_MIN_INTERVAL_MS,
        )
    }
}

impl Policy {
    /// The policy for `rules` (sorted and deduplicated here) and an
    /// interval (at least one millisecond).
    pub fn new(mut rules: Vec<String>, min_interval_ms: u64) -> Self {
        rules.sort();
        rules.dedup();
        Self {
            rules,
            min_interval_ms: min_interval_ms.max(1),
        }
    }

    /// Whether `rule` is on the email list.
    pub fn listed(&self, rule: &str) -> bool {
        self.rules
            .binary_search_by(|r| r.as_str().cmp(rule))
            .is_ok()
    }

    /// Openings per UTC day: the listed rules times the openings one
    /// subject can have in a day at the minimum interval (whole intervals
    /// in a day, rounded up, at least one).
    pub fn daily_limit(&self) -> u64 {
        (self.rules.len() as u64).saturating_mul(DAY_MS.div_ceil(self.min_interval_ms))
    }
}

/// One row of the limits table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitRow {
    /// An episode's state ([`OPENED`] or [`DEFERRED`]), or a day's count.
    pub n: u64,
    /// When it was last touched (an episode's last event, a subject's last
    /// opening, a day's last change).
    pub at_ms: u64,
}

/// An episode's row: opened by an email (its resolution is emailed).
pub const OPENED: u64 = 1;
/// An episode's row: seen, and its opening deferred so far.
pub const DEFERRED: u64 = 0;

/// Why an event is not emailed (it is settled for the email sink without a
/// send; the feed has it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// A reminder (or a repeated firing) of an episode already opened.
    Opened,
    /// A resolution of an episode never opened.
    NotOpened,
    /// Deferred: the subject's last opening is younger than the minimum
    /// interval.
    Interval,
    /// Deferred: the day's openings reached the daily limit.
    Limit,
}

/// What to do with one event of a listed rule that has no ledger row yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Skip(Skip),
    /// Claim and send it; a new opening carries its reservation (a
    /// resolution takes none, and nor does the opening event's own retry,
    /// which is sent under the reservation it already holds).
    Send(Option<Reservation>),
}

/// An opening's reserved rows and what they held before, so a refused
/// send gives them back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reservation {
    episode: String,
    episode_before: Option<LimitRow>,
    opening: String,
    subject: String,
    subject_before: Option<LimitRow>,
    day: String,
    at_ms: u64,
}

const EPISODE: &str = "episode:";
const OPENING: &str = "opening:";
const SUBJECT: &str = "subject:";
const OPENED_DAY: &str = "opened:";
const DEFERRED_DAY: &str = "deferred:";

/// `prefix` and the SHA-256 of `domain` and `parts`, each prefixed with
/// its length (a subject may be as long as the wire contract's string
/// bound).
fn hashed(prefix: &str, domain: &str, parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for part in std::iter::once(domain.as_bytes()).chain(parts.iter().copied()) {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part);
    }
    format!("{prefix}{}", hex::encode(h.finalize()))
}

/// The episode's key: its rule, its subject and when it was first seen,
/// whatever the sender's rule set (which also scopes the event ids), so
/// an episode keeps its row across a rule set change.
fn episode_key(e: &Event) -> String {
    hashed(
        EPISODE,
        "slop_ninja.alerts_relay.email_episode/v1",
        &[
            e.rule.as_bytes(),
            e.subject.as_bytes(),
            &e.since_ms.to_be_bytes(),
        ],
    )
}

/// The key of the event that opened an episode: the episode and the
/// event's kind and last-notified time (which tell its events apart),
/// whatever the sender's rule set.
fn opening_key(e: &Event) -> String {
    let kind: u8 = match e.state {
        EventKind::Firing => 1,
        EventKind::Reminder => 2,
        EventKind::Resolved => 3,
    };
    let notified = e.notified_ms.map(u64::to_be_bytes);
    hashed(
        OPENING,
        "slop_ninja.alerts_relay.email_opening/v1",
        &[
            e.rule.as_bytes(),
            e.subject.as_bytes(),
            &e.since_ms.to_be_bytes(),
            &[kind],
            notified.as_ref().map_or(&[][..], |n| &n[..]),
        ],
    )
}

/// The subject's key: a hash of the rule and the subject, whatever the
/// sender's rule set, so a rule set change does not reset the interval.
fn subject_key(rule: &str, subject: &str) -> String {
    hashed(
        SUBJECT,
        "slop_ninja.alerts_relay.email_subject/v1",
        &[rule.as_bytes(), subject.as_bytes()],
    )
}

fn day_key(prefix: &str, now_ms: u64) -> String {
    format!("{prefix}{}", now_ms / DAY_MS)
}

fn count(t: &impl Table, key: &str) -> Result<u64, StoreError> {
    Ok(t.email_get(key)?.map_or(0, |r| r.n))
}

fn bump(t: &impl Table, key: &str, now_ms: u64) -> Result<(), StoreError> {
    let n = count(t, key)?.saturating_add(1);
    t.email_put(key, LimitRow { n, at_ms: now_ms })
}

/// The decision for event `e` of a listed rule that has no ledger row (an
/// event with one is a retry, and the ledger decides it), whatever rule
/// set it was sent under. A firing or reminder of an unopened episode is
/// opened when the limits allow (reserved now) and deferred otherwise; of
/// an opened episode it is skipped, unless it is the event that opened it
/// and has not been sent yet (an attempt stopped before its send settled,
/// and a retry was refused since): that one is sent under the reservation
/// it holds. A resolution is sent when its episode was opened, or may have
/// been (its row swept).
pub fn decide(
    t: &impl Table,
    policy: &Policy,
    e: &Event,
    now_ms: u64,
) -> Result<Decision, StoreError> {
    let episode = episode_key(e);
    let row = t.email_get(&episode)?;
    if e.state == EventKind::Resolved {
        let send = match row {
            Some(r) => r.n == OPENED,
            None => e.since_ms < t.email_cutoff()?,
        };
        return Ok(if send {
            Decision::Send(None)
        } else {
            Decision::Skip(Skip::NotOpened)
        });
    }
    let touch = |n: u64| t.email_put(&episode, LimitRow { n, at_ms: now_ms });
    let opening = opening_key(e);
    if row.is_some_and(|r| r.n == OPENED) {
        touch(OPENED)?;
        if t.email_get(&opening)?.is_some() {
            // The opening itself, never sent: its reservation is still held.
            t.email_put(
                &opening,
                LimitRow {
                    n: 0,
                    at_ms: now_ms,
                },
            )?;
            return Ok(Decision::Send(None));
        }
        return Ok(Decision::Skip(Skip::Opened));
    }
    let subject = subject_key(&e.rule, &e.subject);
    let subject_before = t.email_get(&subject)?;
    let day = day_key(OPENED_DAY, now_ms);
    let deferred = if subject_before
        .is_some_and(|r| now_ms.saturating_sub(r.at_ms) < policy.min_interval_ms)
    {
        Some(Skip::Interval)
    } else if count(t, &day)? >= policy.daily_limit() {
        Some(Skip::Limit)
    } else {
        None
    };
    if let Some(why) = deferred {
        if row.is_none() {
            bump(t, &day_key(DEFERRED_DAY, now_ms), now_ms)?;
        }
        touch(DEFERRED)?;
        return Ok(Decision::Skip(why));
    }
    touch(OPENED)?;
    let at_now = LimitRow {
        n: 0,
        at_ms: now_ms,
    };
    t.email_put(&opening, at_now)?;
    t.email_put(&subject, at_now)?;
    bump(t, &day, now_ms)?;
    Ok(Decision::Send(Some(Reservation {
        episode,
        episode_before: row,
        opening,
        subject,
        subject_before,
        day,
        at_ms: now_ms,
    })))
}

/// Gives back an opening whose send was refused (nothing went out): the
/// episode and the subject return to what they held (unless a later
/// opening has touched the subject since), the opening event is forgotten,
/// and the day's count drops.
pub fn release(t: &impl Table, r: &Reservation) -> Result<(), StoreError> {
    match r.episode_before {
        Some(row) => t.email_put(&r.episode, row)?,
        None => t.email_remove(&r.episode)?,
    }
    t.email_remove(&r.opening)?;
    if t.email_get(&r.subject)?.is_some_and(|s| s.at_ms == r.at_ms) {
        match r.subject_before {
            Some(row) => t.email_put(&r.subject, row)?,
            None => t.email_remove(&r.subject)?,
        }
    }
    if let Some(row) = t.email_get(&r.day)? {
        t.email_put(
            &r.day,
            LimitRow {
                n: row.n.saturating_sub(1),
                at_ms: row.at_ms,
            },
        )?;
    }
    Ok(())
}

/// Records that event `e` now counts as sent (its ledger row answers its
/// retries): if it opened its episode, it is no longer the opening's own
/// retry ([`decide`]), so a repost after its ledger row is swept is
/// skipped as opened.
pub fn sent(t: &impl Table, e: &Event) -> Result<(), StoreError> {
    if e.state == EventKind::Resolved {
        return Ok(());
    }
    t.email_remove(&opening_key(e))
}

/// How long a limits row is kept after it was last touched, for the
/// senders' renotify interval and the policy's minimum interval: a whole
/// day or the interval, whichever is longer (a day's counts, a subject's
/// last opening), plus two ledger retentions (an open episode's row,
/// touched by each reminder, survives one missed reminder).
pub fn retention_ms(renotify_ms: u64, min_interval_ms: u64) -> u64 {
    DAY_MS
        .max(min_interval_ms)
        .saturating_add(ledger::retention_ms(renotify_ms).saturating_mul(2))
}

/// Deletes rows untouched for `retain_ms` ([`retention_ms`]).
pub fn sweep(t: &impl Table, now_ms: u64, retain_ms: u64) -> Result<(), StoreError> {
    t.email_sweep(now_ms.saturating_sub(retain_ms))
}

/// Today's (UTC) openings and episodes first deferred.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub opened: u64,
    pub deferred: u64,
}

pub fn usage(t: &impl Table, now_ms: u64) -> Result<Usage, StoreError> {
    Ok(Usage {
        opened: count(t, &day_key(OPENED_DAY, now_ms))?,
        deferred: count(t, &day_key(DEFERRED_DAY, now_ms))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::MemTable;
    use crate::mime::tests::RULES;

    const T0: u64 = 1_793_627_107_000;
    const H: u64 = 3_600_000;

    fn at(state: EventKind, rule: &str, subject: &str, since: u64, notified: Option<u64>) -> Event {
        Event::new(RULES, state, rule, subject, "d", since, notified)
    }

    fn opening(d: &Decision) -> Reservation {
        match d {
            Decision::Send(Some(r)) => r.clone(),
            other => panic!("not an opening: {other:?}"),
        }
    }

    #[test]
    fn defaults_and_cap() {
        let p = Policy::default();
        assert_eq!(
            p.rules,
            vec![
                "cohort_fault",
                "commit_gap",
                "coordinator_unwatched",
                "publisher_alerts_undelivered",
                "signing_backing_off",
                "watermark_stalled"
            ]
        );
        assert!(p.rules.iter().all(|r| known(r)));
        assert_eq!(p.min_interval_ms, 6 * H);
        // Six rules, four 6-hour intervals a day.
        assert_eq!(p.daily_limit(), 24);
        assert!(p.listed("commit_gap") && !p.listed("spend_alert") && !p.listed(""));
        // Whole intervals in a day, rounded up, at least one.
        let one = |ms| Policy::new(vec!["r".into()], ms).daily_limit();
        assert_eq!(one(7 * H), 4);
        assert_eq!(one(25 * H), 1);
        assert_eq!(one(1000), 86_400);
        assert_eq!(Policy::new(vec![], H).daily_limit(), 0);
        assert_eq!(
            Policy::new(vec!["b".into(), "a".into(), "b".into()], 0).rules,
            ["a", "b"]
        );
        assert_eq!(Policy::new(vec![], 0).min_interval_ms, 1);
    }

    /// One email when an episode fires and one when it resolves; its
    /// reminders, and the resolution of an episode never opened, are not
    /// emailed.
    #[test]
    fn one_email_per_episode_and_one_at_resolution() {
        let (t, p) = (MemTable::default(), Policy::default());
        let f = at(EventKind::Firing, "commit_gap", "coordinator", T0, None);
        let d = decide(&t, &p, &f, T0).unwrap();
        opening(&d);
        let r = at(
            EventKind::Reminder,
            "commit_gap",
            "coordinator",
            T0,
            Some(T0),
        );
        assert_eq!(
            decide(&t, &p, &r, T0 + 6 * H).unwrap(),
            Decision::Skip(Skip::Opened)
        );
        let z = at(
            EventKind::Resolved,
            "commit_gap",
            "coordinator",
            T0,
            Some(T0 + 6 * H),
        );
        assert_eq!(
            decide(&t, &p, &z, T0 + 7 * H).unwrap(),
            Decision::Send(None)
        );
        // An unconfirmed resolution of the opened episode goes too.
        let u = at(EventKind::Resolved, "commit_gap", "coordinator", T0, None);
        assert_eq!(
            decide(&t, &p, &u, T0 + 7 * H).unwrap(),
            Decision::Send(None)
        );
        // Never opened (never seen): its resolution is not emailed.
        let z2 = at(
            EventKind::Resolved,
            "commit_gap",
            "eval_worker",
            T0,
            Some(T0),
        );
        assert_eq!(
            decide(&t, &p, &z2, T0 + H).unwrap(),
            Decision::Skip(Skip::NotOpened)
        );
        assert_eq!(
            usage(&t, T0).unwrap(),
            Usage {
                opened: 1,
                deferred: 0
            }
        );
    }

    /// A second episode of a subject within the interval is deferred, and
    /// opened by its first reminder past the interval; other subjects are
    /// not deferred. A deferred episode's resolution is not emailed.
    #[test]
    fn the_interval_holds_back_a_subject_until_its_next_reminder() {
        let (t, p) = (MemTable::default(), Policy::default());
        let f1 = at(
            EventKind::Firing,
            "watermark_stalled",
            "coordinator",
            T0,
            None,
        );
        opening(&decide(&t, &p, &f1, T0).unwrap());
        let since = T0 + 600_000;
        let f2 = at(
            EventKind::Firing,
            "watermark_stalled",
            "coordinator",
            since,
            None,
        );
        assert_eq!(
            decide(&t, &p, &f2, since).unwrap(),
            Decision::Skip(Skip::Interval)
        );
        // The same decision again (a retried post) counts the episode once.
        assert_eq!(
            decide(&t, &p, &f2, since + 60_000).unwrap(),
            Decision::Skip(Skip::Interval)
        );
        assert_eq!(
            usage(&t, since).unwrap(),
            Usage {
                opened: 1,
                deferred: 1
            }
        );
        // Another subject, or another rule on the same subject, is free.
        let other = at(
            EventKind::Firing,
            "watermark_stalled",
            "eval_worker",
            since,
            None,
        );
        opening(&decide(&t, &p, &other, since).unwrap());
        let rule = at(EventKind::Firing, "commit_gap", "coordinator", since, None);
        opening(&decide(&t, &p, &rule, since).unwrap());
        // Its reminder one renotify interval later is past the interval.
        let r = at(
            EventKind::Reminder,
            "watermark_stalled",
            "coordinator",
            since,
            Some(since),
        );
        let d = decide(&t, &p, &r, since + 6 * H).unwrap();
        opening(&d);
        assert_eq!(
            usage(&t, since + 6 * H).unwrap(),
            Usage {
                opened: 4,
                deferred: 1
            }
        );
        // A deferred episode that resolves before any opening: not emailed.
        let f3 = at(
            EventKind::Firing,
            "watermark_stalled",
            "coordinator",
            T0 + 7 * H,
            None,
        );
        assert_eq!(
            decide(&t, &p, &f3, T0 + 7 * H).unwrap(),
            Decision::Skip(Skip::Interval)
        );
        let z3 = at(
            EventKind::Resolved,
            "watermark_stalled",
            "coordinator",
            T0 + 7 * H,
            Some(T0 + 7 * H),
        );
        assert_eq!(
            decide(&t, &p, &z3, T0 + 8 * H).unwrap(),
            Decision::Skip(Skip::NotOpened)
        );
    }

    /// The daily limit defers openings past the listed rules times the
    /// intervals in a day, until the next UTC day.
    #[test]
    fn the_daily_limit_holds_back_a_storm() {
        let t = MemTable::default();
        let p = Policy::new(vec!["cohort_fault".into()], 6 * H);
        assert_eq!(p.daily_limit(), 4);
        let day_start = (T0 / DAY_MS) * DAY_MS;
        let mut out = Vec::new();
        for i in 0..6u64 {
            let e = at(
                EventKind::Firing,
                "cohort_fault",
                &format!("testnet-r1-{i}"),
                day_start,
                None,
            );
            out.push(decide(&t, &p, &e, day_start + i).unwrap());
        }
        assert!(
            out[..4]
                .iter()
                .all(|d| matches!(d, Decision::Send(Some(_))))
        );
        assert_eq!(
            out[4..],
            [Decision::Skip(Skip::Limit), Decision::Skip(Skip::Limit)]
        );
        assert_eq!(
            usage(&t, day_start).unwrap(),
            Usage {
                opened: 4,
                deferred: 2
            }
        );
        // The next UTC day: the deferred episode's reminder opens it.
        let next = day_start + DAY_MS;
        let r = at(
            EventKind::Reminder,
            "cohort_fault",
            "testnet-r1-4",
            day_start,
            Some(day_start),
        );
        opening(&decide(&t, &p, &r, next).unwrap());
        assert_eq!(
            usage(&t, next).unwrap(),
            Usage {
                opened: 1,
                deferred: 0
            }
        );
    }

    /// A refused send gives its opening back: the retry is opened again,
    /// and the day counts it once.
    #[test]
    fn a_refused_opening_is_given_back() {
        let (t, p) = (MemTable::default(), Policy::default());
        let f = at(EventKind::Firing, "commit_gap", "coordinator", T0, None);
        let r = opening(&decide(&t, &p, &f, T0).unwrap());
        release(&t, &r).unwrap();
        assert_eq!(usage(&t, T0).unwrap(), Usage::default());
        assert_eq!(t.email_get(&episode_key(&f)).unwrap(), None);
        opening(&decide(&t, &p, &f, T0 + 60_000).unwrap());
        assert_eq!(
            usage(&t, T0).unwrap(),
            Usage {
                opened: 1,
                deferred: 0
            }
        );
        // A deferred episode opened by its reminder, then refused, is
        // deferred again (not forgotten), and the subject keeps the
        // earlier opening's time.
        let since = T0 + 120_000;
        let f2 = at(EventKind::Firing, "commit_gap", "coordinator", since, None);
        assert_eq!(
            decide(&t, &p, &f2, since).unwrap(),
            Decision::Skip(Skip::Interval)
        );
        let r2 = at(
            EventKind::Reminder,
            "commit_gap",
            "coordinator",
            since,
            Some(since),
        );
        let res = opening(&decide(&t, &p, &r2, T0 + 7 * H).unwrap());
        release(&t, &res).unwrap();
        assert_eq!(
            t.email_get(&episode_key(&f2)).unwrap().map(|r| r.n),
            Some(DEFERRED)
        );
        let subject = subject_key("commit_gap", "coordinator");
        assert_eq!(
            t.email_get(&subject).unwrap().map(|r| r.at_ms),
            Some(T0 + 60_000)
        );
    }

    /// Rows untouched for the retention are swept; an open episode's
    /// reminders keep its row; a resolution whose episode row may have been
    /// swept is emailed, and one the table still covers is not.
    #[test]
    fn sweeps_keep_open_episodes_and_resolve_swept_ones() {
        let (t, p) = (MemTable::default(), Policy::default());
        let renotify = 6 * H;
        let keep = retention_ms(renotify, p.min_interval_ms);
        assert_eq!(keep, DAY_MS + 12 * H);
        assert_eq!(
            retention_ms(60_000, 2 * DAY_MS),
            2 * DAY_MS + 2 * crate::rules::PUBLISHER_DOWN_MS
        );
        let f = at(EventKind::Firing, "commit_gap", "coordinator", T0, None);
        opening(&decide(&t, &p, &f, T0).unwrap());
        // Reminders every renotify interval for three days keep the row.
        let mut now = T0;
        for _ in 0..12 {
            now += renotify;
            let r = at(
                EventKind::Reminder,
                "commit_gap",
                "coordinator",
                T0,
                Some(now - renotify),
            );
            assert_eq!(
                decide(&t, &p, &r, now).unwrap(),
                Decision::Skip(Skip::Opened)
            );
            sweep(&t, now, keep).unwrap();
        }
        let z = at(
            EventKind::Resolved,
            "commit_gap",
            "coordinator",
            T0,
            Some(now),
        );
        assert_eq!(decide(&t, &p, &z, now + H).unwrap(), Decision::Send(None));
        // A fresh table covers every episode: no row means never opened.
        let t = MemTable::default();
        sweep(&t, now, keep).unwrap();
        assert_eq!(t.email_cutoff().unwrap(), 0, "nothing deleted, no cutoff");
        assert_eq!(
            decide(&t, &p, &z, now).unwrap(),
            Decision::Skip(Skip::NotOpened)
        );
        // An opened episode silent past the retention loses its row: its
        // resolution is still emailed (the episode began before the cutoff).
        opening(&decide(&t, &p, &f, T0).unwrap());
        sweep(&t, T0 + keep + 1, keep).unwrap();
        assert_eq!(t.email_cutoff().unwrap(), T0 + 1);
        assert_eq!(
            decide(&t, &p, &z, T0 + keep + 1).unwrap(),
            Decision::Send(None)
        );
        // An episode that began after the cutoff with no row: never opened.
        let late = at(
            EventKind::Resolved,
            "commit_gap",
            "coordinator",
            T0 + 2,
            Some(T0 + 3),
        );
        assert_eq!(
            decide(&t, &p, &late, T0 + keep + 1).unwrap(),
            Decision::Skip(Skip::NotOpened)
        );
    }

    /// An episode keeps its row across a change of the sender's rule set:
    /// its reminders under the new rule set are not emailed (no second
    /// opening), and its resolution under the new rule set is.
    #[test]
    fn a_rule_set_change_keeps_the_episode() {
        let (t, p) = (MemTable::default(), Policy::default());
        const NEXT: &str = "slop_ninja.publisher.alerts/v4";
        let under = |rules, state, notified| {
            Event::new(rules, state, "commit_gap", "coordinator", "d", T0, notified)
        };
        let f = under(RULES, EventKind::Firing, None);
        opening(&decide(&t, &p, &f, T0).unwrap());
        let r = under(NEXT, EventKind::Reminder, Some(T0));
        assert_ne!(r.firing_id(NEXT), f.id, "a new rule set, new ids");
        assert_eq!(
            decide(&t, &p, &r, T0 + 6 * H).unwrap(),
            Decision::Skip(Skip::Opened)
        );
        for z in [
            under(NEXT, EventKind::Resolved, Some(T0 + 6 * H)),
            under(NEXT, EventKind::Resolved, None),
        ] {
            assert_eq!(
                decide(&t, &p, &z, T0 + 7 * H).unwrap(),
                Decision::Send(None)
            );
        }
        assert_eq!(usage(&t, T0).unwrap().opened, 1);
    }

    /// An opening whose attempt stopped before its send settled keeps its
    /// reservation: the opening event's own retry is sent under it (once
    /// its ledger row is gone, after a refused retry), however often; the
    /// episode's other events are skipped, and once the opening counts as
    /// sent, so is a repost of it.
    #[test]
    fn the_opening_s_own_retry_is_sent_under_its_reservation() {
        let (t, p) = (MemTable::default(), Policy::default());
        let f = at(EventKind::Firing, "commit_gap", "coordinator", T0, None);
        opening(&decide(&t, &p, &f, T0).unwrap());
        for n in 1..4 {
            assert_eq!(
                decide(&t, &p, &f, T0 + n * 600_000).unwrap(),
                Decision::Send(None)
            );
        }
        assert_eq!(usage(&t, T0).unwrap().opened, 1, "one reservation");
        let r = at(
            EventKind::Reminder,
            "commit_gap",
            "coordinator",
            T0,
            Some(T0 + H),
        );
        assert_eq!(
            decide(&t, &p, &r, T0 + 7 * H).unwrap(),
            Decision::Skip(Skip::Opened)
        );
        sent(&t, &f).unwrap();
        assert_eq!(
            decide(&t, &p, &f, T0 + 8 * H).unwrap(),
            Decision::Skip(Skip::Opened)
        );
        // A refused opening forgets its opening event with the rest.
        let g = at(EventKind::Firing, "cohort_fault", "testnet-r1-3", T0, None);
        let res = opening(&decide(&t, &p, &g, T0).unwrap());
        release(&t, &res).unwrap();
        assert_eq!(t.email_get(&opening_key(&g)).unwrap(), None);
        opening(&decide(&t, &p, &g, T0 + 60_000).unwrap());
    }

    /// The keys ignore the rule set and tell an episode's events apart.
    #[test]
    fn keys_ignore_the_rule_set() {
        let a = at(EventKind::Firing, "commit_gap", "coordinator", T0, None);
        let b = Event::new(
            "slop_ninja.publisher.alerts/v4",
            EventKind::Firing,
            "commit_gap",
            "coordinator",
            "other detail",
            T0,
            None,
        );
        assert_eq!(
            (episode_key(&a), opening_key(&a)),
            (episode_key(&b), opening_key(&b))
        );
        let r0 = at(
            EventKind::Reminder,
            "commit_gap",
            "coordinator",
            T0,
            Some(0),
        );
        let later = at(EventKind::Firing, "commit_gap", "coordinator", T0 + 1, None);
        assert_eq!(episode_key(&r0), episode_key(&a));
        assert_ne!(opening_key(&r0), opening_key(&a));
        assert_ne!(episode_key(&later), episode_key(&a));
        // Length-prefixed parts: a rule and subject split differently differ.
        let x = at(EventKind::Firing, "ab", "c", T0, None);
        let y = at(EventKind::Firing, "a", "bc", T0, None);
        assert_ne!(episode_key(&x), episode_key(&y));
        assert_ne!(subject_key("ab", "c"), subject_key("a", "bc"));
    }

    /// The publisher's rules here are the ones its source raises: the
    /// literal rule of each condition it builds, and its live and
    /// status-read rule lists (outside its tests).
    #[test]
    fn the_publisher_rules_match_its_source() {
        let src = include_str!("../../publisher/src/alerts.rs");
        let src = &src[..src.find("#[cfg(test)]").expect("the publisher's tests")];
        let mut raised = std::collections::BTreeSet::new();
        let mut rest = src;
        while let Some(i) = rest.find("cond(") {
            let before = &rest[..i];
            rest = &rest[i + "cond(".len()..];
            let word = before
                .chars()
                .last()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            if word || before.ends_with("fn ") {
                continue;
            }
            if let Some(lit) = rest.trim_start().strip_prefix('"') {
                raised.insert(&lit[..lit.find('"').unwrap()]);
            }
        }
        for list in [
            "pub const LIVE_RULES: &[&str] = &[",
            "pub const STATUS_RULES: &[&str] = &[",
        ] {
            let start = src.find(list).expect(list) + list.len();
            let body = &src[start..start + src[start..].find("];").unwrap()];
            raised.extend(body.split('"').skip(1).step_by(2));
        }
        let listed: std::collections::BTreeSet<&str> = PUBLISHER_RULES.iter().copied().collect();
        assert_eq!(raised, listed);
        assert!(PUBLISHER_RULES.windows(2).all(|w| w[0] < w[1]));
        assert!(known("spend_alert") && known("live_fault"));
        assert!(!known("watermark_stall") && !known(""));
    }
}
