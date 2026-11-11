//! The email policy end to end: the relay's post handler driven by the
//! publisher's own alert state machine (the wire crate's observe, due,
//! attempt and acknowledge; one run a minute, renotify 6 h) under the
//! default policy. Each sent message is read back for the events it carries
//! (the "; event <id prefix>" line of each event).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use slop_ninja_alerts_relay::arena::{self, Budget};
use slop_ninja_alerts_relay::block_on;
use slop_ninja_alerts_relay::config::{self, Config};
use slop_ninja_alerts_relay::email;
use slop_ninja_alerts_relay::ledger::{self, MemTable, Table};
use slop_ninja_alerts_relay::mime::Message;
use slop_ninja_alerts_relay::relay::{self, Failure, Mailer, SendError};
use slop_ninja_alerts_wire::{
    AlertState, Condition, Event, EventKind, POST_SCHEMA, Post, acknowledge, attempt, due, observe,
};

const V3: &str = "slop_ninja.publisher.alerts/v3";
const V4: &str = "slop_ninja.publisher.alerts/v4";
const MIN: u64 = 60_000;
const H: u64 = 3_600_000;
const DAY: u64 = 86_400_000;
const RENOTIFY: u64 = 6 * H;
const MIDNIGHT: u64 = 1_793_627_107_000 / DAY * DAY;
const FIRST: &str = "(first email on record: its firing was deferred";

/// A mailer that records what it sent, answering from `script` first.
#[derive(Default)]
struct Rec {
    script: RefCell<Vec<Result<(), SendError>>>,
    now: Cell<u64>,
    sent: RefCell<Vec<(u64, String)>>,
}

impl Mailer for Rec {
    async fn send(&self, m: &Message) -> Result<(), SendError> {
        let next = {
            let mut s = self.script.borrow_mut();
            if s.is_empty() { Ok(()) } else { s.remove(0) }
        };
        if next.is_ok() {
            self.sent.borrow_mut().push((self.now.get(), m.raw.clone()));
        }
        next
    }
}

fn cfg() -> Config {
    let vars = [
        ("NETWORK", "testnet"),
        ("ALERT_EMAIL_ENABLED", "true"),
        ("ALERT_EMAIL_FROM", "alerts@example.invalid"),
        ("ALERT_EMAIL_TO", "ops@example.invalid"),
    ];
    config::parse(|k| {
        vars.iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| (*v).to_owned())
    })
    .unwrap()
}

fn cond(rule: &'static str, subject: &str) -> Condition {
    Condition {
        rule,
        subject: subject.into(),
        detail: "d".into(),
        hold_ms: 2 * MIN,
    }
}

type Episode = (String, String, u64);

fn ep(e: &Event) -> Episode {
    (e.rule.clone(), e.subject.clone(), e.since_ms)
}

/// The publisher, the relay and its mailbox.
struct World {
    cfg: Config,
    table: MemTable,
    mail: Rec,
    budget: Budget,
    st: AlertState,
    rules: String,
    posted: BTreeMap<String, Event>,
    statuses: Vec<u16>,
}

impl World {
    fn new() -> Self {
        Self {
            cfg: cfg(),
            table: MemTable::default(),
            mail: Rec::default(),
            budget: Budget::new(arena::READBACK_BUDGET),
            st: AlertState::default(),
            rules: V3.into(),
            posted: BTreeMap::new(),
            statuses: Vec::new(),
        }
    }

    fn post(&mut self, rules: &str, events: Vec<Event>, now: u64) -> u16 {
        for e in &events {
            self.posted.insert(e.id[..12].to_owned(), e.clone());
        }
        let body = Post {
            schema: POST_SCHEMA.into(),
            rules: rules.into(),
            network: "testnet".into(),
            events,
        }
        .body();
        self.mail.now.set(now);
        let a = block_on(relay::handle_post(
            &body,
            &self.cfg,
            &self.table,
            &self.mail,
            &self.budget,
            now,
        ));
        self.statuses.push(a.status);
        a.status
    }

    /// One publisher run: observe, post the due events (marked attempted
    /// first), acknowledge them on 200.
    fn run(&mut self, conds: &[Condition], now: u64) {
        let rules = self.rules.clone();
        observe(&mut self.st, &rules, conds, now);
        let ev = due(&self.st, now, RENOTIFY);
        if ev.is_empty() {
            return;
        }
        attempt(&mut self.st, &ev, now);
        if self.post(&rules, ev.clone(), now) == 200 {
            acknowledge(&mut self.st, &ev, now);
        }
    }

    /// Runs every minute from `from` while `until` holds, then three runs
    /// with nothing observed; returns the next minute.
    fn runs(&mut self, from: u64, until: impl Fn(u64) -> Option<Vec<Condition>>) -> u64 {
        let mut t = from;
        while let Some(c) = until(t) {
            self.run(&c, t);
            t += MIN;
        }
        for _ in 0..3 {
            self.run(&[], t);
            t += MIN;
        }
        t
    }

    fn texts(&self) -> Vec<(u64, String)> {
        self.mail
            .sent
            .borrow()
            .iter()
            .map(|(at, raw)| (*at, raw.replace("=\r\n", "")))
            .collect()
    }

    /// Every emailed event, in send order.
    fn emailed(&self) -> Vec<(u64, Event)> {
        let mut out = Vec::new();
        for (at, text) in self.texts() {
            for (i, _) in text.match_indices("; event ") {
                let id = &text[i + 8..i + 20];
                out.push((at, self.posted[id].clone()));
            }
        }
        out
    }

    fn kinds(&self) -> Vec<EventKind> {
        self.emailed().iter().map(|(_, e)| e.state).collect()
    }

    fn first_notes(&self) -> usize {
        self.texts()
            .iter()
            .map(|(_, t)| t.matches(FIRST).count())
            .sum()
    }
}

/// The emailed openings and resolutions, checking that every emailed
/// resolution belongs to an episode opened earlier and is emailed once,
/// and that no episode is opened twice.
fn pairs(em: &[(u64, Event)]) -> (BTreeSet<Episode>, BTreeSet<Episode>) {
    let (mut opened, mut resolved) = (BTreeSet::new(), BTreeSet::new());
    for (_, e) in em {
        if e.state == EventKind::Resolved {
            assert!(
                opened.contains(&ep(e)),
                "resolution of an unopened episode: {e:?}"
            );
            assert!(resolved.insert(ep(e)), "resolution emailed twice: {e:?}");
        } else {
            assert!(opened.insert(ep(e)), "episode opened twice: {e:?}");
        }
    }
    (opened, resolved)
}

fn openings_per_day(em: &[(u64, Event)]) -> BTreeMap<u64, u64> {
    let mut per_day = BTreeMap::new();
    for (at, e) in em {
        if e.state != EventKind::Resolved {
            *per_day.entry(at / DAY - MIDNIGHT / DAY).or_default() += 1;
        }
    }
    per_day
}

#[test]
fn defaults() {
    let c = cfg();
    let p = &c.email_policy;
    assert_eq!(
        p.rules,
        [
            "cohort_fault",
            "commit_gap",
            "coordinator_unwatched",
            "publisher_alerts_undelivered",
            "signing_backing_off",
            "watermark_stalled"
        ]
    );
    assert_eq!((p.min_interval_ms, p.daily_limit()), (RENOTIFY, 24));
    assert!(c.warnings.is_empty());
}

/// One subject flapping (5 minutes on, 5 off) for three days: openings at
/// least the interval apart, each with its resolution, every post answered.
#[test]
fn a_flapping_subject_is_emailed_once_per_interval() {
    let mut w = World::new();
    let start = MIDNIGHT + 30 * MIN;
    let c = cond("commit_gap", "coordinator");
    w.runs(start, |t| {
        (t < start + 3 * DAY).then(|| {
            if ((t - start) / MIN) % 10 < 5 {
                vec![c.clone()]
            } else {
                vec![]
            }
        })
    });
    let em = w.emailed();
    let (opened, resolved) = pairs(&em);
    let times: Vec<u64> = em
        .iter()
        .filter(|(_, e)| e.state != EventKind::Resolved)
        .map(|(at, _)| *at)
        .collect();
    assert!(times.windows(2).all(|p| p[1] - p[0] >= RENOTIFY));
    assert_eq!(opened, resolved);
    assert!((12..=13).contains(&opened.len()), "{}", opened.len());
    assert!(w.statuses.iter().all(|s| *s == 200));
}

/// 30 cohort faults for 40 hours: at most the daily limit opened per UTC
/// day, the rest opened by a reminder the next day (with the first-email
/// note), one resolution per episode.
#[test]
fn a_storm_stays_within_the_daily_limit() {
    let mut w = World::new();
    let start = MIDNIGHT + H;
    let conds: Vec<Condition> = (0..30)
        .map(|i| cond("cohort_fault", &format!("testnet-r1-{i}")))
        .collect();
    w.runs(start, |t| (t < start + 40 * H).then(|| conds.clone()));
    let em = w.emailed();
    let (opened, resolved) = pairs(&em);
    assert_eq!((opened.len(), resolved.len()), (30, 30));
    assert_eq!(openings_per_day(&em), BTreeMap::from([(0, 24), (1, 6)]));
    let reminders = em
        .iter()
        .filter(|(_, e)| e.state == EventKind::Reminder)
        .count();
    assert_eq!((reminders, w.first_notes()), (6, 6));
}

/// The publisher moves to a new rule set while an emailed episode lasts:
/// its reminders under the new rule set are not emailed again (and say
/// nothing about a first email), and its resolution under the new rule set
/// is emailed, whether it clears before a reminder or after.
#[test]
fn a_rule_set_change_keeps_the_episode() {
    for last in [H, 8 * H] {
        let mut w = World::new();
        let c = cond("commit_gap", "coordinator");
        let t0 = MIDNIGHT + H;
        let switch = t0 + 10 * MIN;
        let mut t = t0;
        while t < switch {
            w.run(std::slice::from_ref(&c), t);
            t += MIN;
        }
        w.rules = V4.into();
        w.runs(t, |t| (t < switch + last).then(|| vec![c.clone()]));
        assert_eq!(
            w.kinds(),
            [EventKind::Firing, EventKind::Resolved],
            "{last}"
        );
        let resolution = &w.emailed()[1].1;
        assert!(resolution.id_matches(V4), "resolved under the new rule set");
        assert_eq!(w.first_notes(), 0);
        assert!(w.statuses.iter().all(|s| *s == 200));
        assert_eq!(email::usage(&w.table, t0).unwrap().opened, 1);
    }
}

/// An attempt decided and claimed an opening, then stopped before its send
/// settled (an isolate that ended mid-send); the retry after the claim
/// period is refused. The next retry still emails the firing (once), under
/// the opening's reservation, and its resolution follows.
#[test]
fn a_crashed_then_refused_opening_is_still_emailed() {
    let mut w = World::new();
    let t0 = MIDNIGHT + H;
    let f = Event::new(
        V3,
        EventKind::Firing,
        "commit_gap",
        "coordinator",
        "d",
        t0 - 2 * MIN,
        None,
    );
    let d = email::decide(&w.table, &w.cfg.email_policy, &f, t0).unwrap();
    assert!(matches!(d, email::Decision::Send(Some(_))), "{d:?}");
    assert_eq!(
        ledger::claim(&w.table, &f.id, t0).unwrap(),
        ledger::Claim::Mine { retry: false }
    );
    let t1 = t0 + ledger::CLAIM_MS + MIN;
    w.mail
        .script
        .borrow_mut()
        .push(Err(SendError::Refused(Failure::new("E_TEST", "refused"))));
    for (i, want) in [503, 200, 200].into_iter().enumerate() {
        assert_eq!(w.post(V3, vec![f.clone()], t1 + i as u64 * MIN), want);
    }
    assert_eq!(w.kinds(), [EventKind::Firing]);
    let z = Event::new(
        V3,
        EventKind::Resolved,
        "commit_gap",
        "coordinator",
        "d",
        t0 - 2 * MIN,
        Some(t1 + MIN),
    );
    assert_eq!(w.post(V3, vec![z], t1 + H), 200);
    assert_eq!(w.kinds(), [EventKind::Firing, EventKind::Resolved]);
    assert_eq!(email::usage(&w.table, t0).unwrap().opened, 1);
}

/// A refused send gives its opening back: the publisher's retry is emailed
/// once, and the day counts one opening.
#[test]
fn a_refused_send_gives_its_opening_back() {
    let mut w = World::new();
    w.mail
        .script
        .borrow_mut()
        .push(Err(SendError::Refused(Failure::new("E_TEST", "refused"))));
    let c = cond("commit_gap", "coordinator");
    let t0 = MIDNIGHT + H;
    for i in 0..10 {
        w.run(std::slice::from_ref(&c), t0 + i * MIN);
    }
    assert_eq!(w.kinds(), [EventKind::Firing]);
    assert_eq!(email::usage(&w.table, t0).unwrap().opened, 1);
    assert_eq!(w.statuses.iter().filter(|s| **s == 503).count(), 1);
}

/// An emailed episode whose sender is silent past the email rows'
/// retention loses its row: its next reminder is emailed as the first
/// email on record about it, and its resolution follows.
#[test]
fn an_episode_swept_in_a_silence_is_emailed_again() {
    let mut w = World::new();
    let c = cond("commit_gap", "coordinator");
    let t0 = MIDNIGHT + H;
    let mut t = t0;
    while t < t0 + 10 * MIN {
        w.run(std::slice::from_ref(&c), t);
        t += MIN;
    }
    t += 40 * H;
    let keep = email::retention_ms(RENOTIFY, w.cfg.email_policy.min_interval_ms);
    assert!(40 * H > keep);
    email::sweep(&w.table, t, keep).unwrap();
    ledger::sweep(&w.table, t, ledger::retention_ms(RENOTIFY)).unwrap();
    assert!(w.table.email_cutoff().unwrap() > 0);
    let resume = t;
    w.runs(resume, |t| (t < resume + H).then(|| vec![c.clone()]));
    assert_eq!(
        w.kinds(),
        [EventKind::Firing, EventKind::Reminder, EventKind::Resolved]
    );
    assert_eq!(w.first_notes(), 1);
}

/// Unlisted rules with email on send nothing; every post is answered.
#[test]
fn unlisted_rules_go_to_the_feed_alone() {
    let mut w = World::new();
    let start = MIDNIGHT + H;
    let cs = [
        cond("status_unreachable", "eval"),
        cond("publisher_silent", "publisher"),
        cond("spend_alert", "month"),
    ];
    w.runs(start, |t| {
        (t < start + DAY).then(|| {
            if ((t - start) / MIN) % 10 < 5 {
                cs.to_vec()
            } else {
                vec![]
            }
        })
    });
    assert!(w.mail.sent.borrow().is_empty());
    assert!(!w.statuses.is_empty() && w.statuses.iter().all(|s| *s == 200));
}
