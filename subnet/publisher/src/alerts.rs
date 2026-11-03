//! Alert rules (`slop_ninja.publisher.alerts/v2`), their state across runs,
//! and the posts to the sink chosen under decision D18.
//!
//! Each run turns the observed status into a set of [`Condition`]s
//! ([`evaluate`]). The durable [`AlertState`] (kept in the publisher's
//! Durable Object) remembers when each condition was first seen and what was
//! notified, so a condition fires once it has held for its rule's hold time,
//! is re-posted as a reminder every renotify interval while it lasts, and
//! posts a resolution when it clears. A failed post leaves its events due, so
//! the next run retries them; a duplicate run posts nothing new.
//!
//! Details are content-free: codes, labels, epochs and ages, never spend
//! figures or private data.
//!
//! | Rule | Fires when |
//! | --- | --- |
//! | `status_unreachable` | a status source has not answered for two runs |
//! | `durable_fault` | a Worker's journal holds a durable fault, or signing is in `fault` |
//! | `fallback_too_long` | a Worker has been in fallback mode for a whole tempo |
//! | `watermark_stalled` | a Worker's RPC-confirmed watermark is older than the watchdog silence plus two cron periods, or absent |
//! | `phase_overrun` | the active cohort is still in a work phase at its package deadline (`K - transition` tempos after it opened) |
//! | `scorer_budget_fault` | the active cohort is paused by a scorer or budget fault |
//! | `cohort_fault` | the active cohort is paused by any other fault |
//! | `ledger_threshold` | the monthly ledger's spend plus reservations reach 80% of its cap |
//! | `commit_gap` | the coordinator has not committed weights for two or more tempos |
//! | `coordinator_reveal_mismatch` | a reveal of the coordinator's weights did not match its committed row |
//! | `publication_refused` | a closed cohort, an artifact's admission record or copy, or the authorization failed verification or the memory budget |
//! | `publication_pending` | a closed cohort, an artifact copy or the authorization has been incomplete for a whole tempo |
//! | `publication_unverifiable` | closed cohorts whose manifest does not open under the compiled trust roots (one alert for all) |
//! | `config_refused` | the publisher's configuration was refused; only the status fault snapshot is written |
//! | `run_interrupted` | every run for a whole tempo stopped before it finished (it never released its lease) |
//!
//! Not covered here (AR-13 gaps, listed in the README): commits by our other
//! validator (validator2's journal is not visible to the publisher), and a
//! divergence between the chain's consensus for the subnet and our row, which
//! needs chain reads.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::SinkFormat;
use crate::status::{Observed, WorkerView};

pub const RULES_ID: &str = "slop_ninja.publisher.alerts/v2";
pub const ALERT_SCHEMA: &str = "slop_ninja.publisher.alert/v1";

/// Work phases that must end by the package deadline.
pub const WORK_PHASES: &[&str] = &["generating", "sealing", "scoring", "packaging"];
/// Cohort faults that are scorer or budget faults (AR-13).
pub const SCORER_BUDGET_FAULTS: &[&str] = &[
    "scorer_unavailable",
    "scoring_budget",
    "generation_budget",
    "unresolved_infrastructure",
    "monthly_cap",
];

/// Thresholds, each derived from a deployment parameter or the readiness
/// criterion it implements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rules {
    /// One tempo: fallback that long means block webhook deliveries have
    /// stopped and need an operator.
    pub fallback_ms: u64,
    /// The watchdog's silence threshold plus two one-minute cron periods: by
    /// then a healthy Worker has confirmed a newer block.
    pub watermark_stall_ms: u64,
    /// AR-13: two or more tempos without a commit.
    pub commit_gap_tempos: u64,
    /// AR-13: the ledger passing 80%.
    pub ledger_permille: u64,
    /// Two cron periods.
    pub unreachable_hold_ms: u64,
    /// One tempo.
    pub publication_hold_ms: u64,
    pub renotify_ms: u64,
}

impl Rules {
    pub fn derive(tempo_ms: u64, silence_ms: u64, renotify_ms: u64) -> Self {
        Self {
            fallback_ms: tempo_ms,
            watermark_stall_ms: silence_ms + 2 * 60_000,
            commit_gap_tempos: 2,
            ledger_permille: 800,
            unreachable_hold_ms: 2 * 60_000,
            publication_hold_ms: tempo_ms,
            renotify_ms,
        }
    }
}

/// One condition observed this run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Condition {
    pub rule: &'static str,
    pub subject: String,
    pub detail: String,
    pub hold_ms: u64,
}

/// What the publisher itself observed in its last publication pass.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublisherObs {
    /// Refused work (cohorts, artifacts, authorizations): subject, reason.
    pub refused: Vec<(String, String)>,
    /// Incomplete work: subject, reason.
    pub pending: Vec<(String, String)>,
    /// Closed cohorts whose manifest does not open under the trust roots.
    #[serde(default)]
    pub unverifiable: Vec<String>,
    /// The configuration error, when the configuration was refused.
    #[serde(default)]
    pub config: Option<String>,
    /// The previous run stopped before it finished (a CPU or wall limit, an
    /// eviction or a trap): it never released its lease.
    #[serde(default)]
    pub interrupted: bool,
}

fn cond(rule: &'static str, subject: &str, detail: String, hold_ms: u64) -> Condition {
    Condition {
        rule,
        subject: subject.to_owned(),
        detail,
        hold_ms,
    }
}

fn worker_conditions(name: &str, w: &WorkerView, r: &Rules, out: &mut Vec<Condition>) {
    if let Some(kind) = &w.fault {
        out.push(cond(
            "durable_fault",
            name,
            format!("journal fault {kind}"),
            0,
        ));
    }
    if let Some(s) = &w.signing
        && s.state == "fault"
    {
        let reason = s.reason.as_deref().unwrap_or("unspecified");
        out.push(cond(
            "durable_fault",
            &format!("{name}.signing"),
            format!("signing fault {reason}"),
            0,
        ));
    }
    if w.mode.as_deref() == Some("fallback") && w.mode_age_ms.unwrap_or(0) >= r.fallback_ms {
        out.push(cond(
            "fallback_too_long",
            name,
            format!("fallback for {} s", w.mode_age_ms.unwrap_or(0) / 1000),
            0,
        ));
    }
    match w.confirmed_age_ms {
        Some(age) if age < r.watermark_stall_ms => {}
        Some(age) => out.push(cond(
            "watermark_stalled",
            name,
            format!("confirmed watermark {} s old", age / 1000),
            0,
        )),
        None => out.push(cond(
            "watermark_stalled",
            name,
            "no confirmed watermark".into(),
            0,
        )),
    }
}

/// The conditions that hold this run.
pub fn evaluate(o: &Observed, p: &PublisherObs, r: &Rules) -> Vec<Condition> {
    let mut out = Vec::new();
    for (name, present) in [
        ("coordinator", o.coordinator.is_some()),
        ("eval_worker", o.eval_worker.is_some()),
        ("eval", o.eval.is_some()),
    ] {
        if !present {
            out.push(cond(
                "status_unreachable",
                name,
                "status did not answer".into(),
                r.unreachable_hold_ms,
            ));
        }
    }
    if let Some(w) = &o.coordinator {
        worker_conditions("coordinator", w, r, &mut out);
    }
    if let Some(w) = &o.eval_worker {
        worker_conditions("eval_worker", w, r, &mut out);
    }
    let epoch = o.eval.as_ref().and_then(|e| e.epoch);
    if let Some(e) = &o.eval {
        if let Some(c) = e.active() {
            if let Some(code) = &c.fault {
                let rule = if SCORER_BUDGET_FAULTS.contains(&code.as_str()) {
                    "scorer_budget_fault"
                } else {
                    "cohort_fault"
                };
                out.push(cond(
                    rule,
                    &c.label,
                    format!("fault {code} in {}", c.phase),
                    0,
                ));
            }
            if let (true, Some(now), Some(opened), Some(k)) = (
                WORK_PHASES.contains(&c.phase.as_str()),
                epoch,
                c.opened_epoch,
                c.k_tempos,
            ) {
                let deadline = k.saturating_sub(c.transition_tempos.unwrap_or(0));
                let elapsed = now.saturating_sub(opened);
                if elapsed >= deadline {
                    out.push(cond(
                        "phase_overrun",
                        &c.label,
                        format!(
                            "{} after {elapsed} tempos; package deadline {deadline}",
                            c.phase
                        ),
                        0,
                    ));
                }
            }
        }
        if e.ledger_permille.is_some_and(|p| p >= r.ledger_permille) {
            out.push(cond(
                "ledger_threshold",
                "ledger",
                format!(
                    "spend and reservations at or above {}% of the monthly cap",
                    r.ledger_permille / 10
                ),
                0,
            ));
        }
    }
    if let (Some(signing), Some(now)) = (
        o.coordinator.as_ref().and_then(|w| w.signing.as_ref()),
        epoch,
    ) {
        let last = signing
            .ops
            .iter()
            .filter(|op| op.commit_block.is_some())
            .map(|op| op.epoch)
            .max();
        match last {
            Some(last) if now.saturating_sub(last) < r.commit_gap_tempos => {}
            Some(last) => out.push(cond(
                "commit_gap",
                "coordinator",
                format!("last commit at epoch {last}, now {now}"),
                0,
            )),
            None => out.push(cond(
                "commit_gap",
                "coordinator",
                "no commit among recent operations".into(),
                0,
            )),
        }
        for op in signing
            .ops
            .iter()
            .filter(|op| op.reveal_weights_match == Some(false))
        {
            out.push(cond(
                "coordinator_reveal_mismatch",
                &format!("coordinator.epoch-{}", op.epoch),
                format!(
                    "reveal at epoch {} differs from the committed row",
                    op.epoch
                ),
                0,
            ));
        }
    }
    if let Some(why) = &p.config {
        out.push(cond("config_refused", "publisher", why.clone(), 0));
    }
    if p.interrupted {
        out.push(cond(
            "run_interrupted",
            "publisher",
            "the previous run stopped before it finished".into(),
            r.publication_hold_ms,
        ));
    }
    for (label, reason) in &p.refused {
        out.push(cond("publication_refused", label, reason.clone(), 0));
    }
    if let Some(newest) = p.unverifiable.iter().max() {
        out.push(cond(
            "publication_unverifiable",
            "manifests",
            format!(
                "{} closed cohorts have a manifest that does not open under the compiled trust \
                 roots (newest label {newest}); none is read again by this build. Set \
                 RECORD_LABEL_PREFIX to leave them outside the record.",
                p.unverifiable.len()
            ),
            0,
        ));
    }
    for (label, reason) in &p.pending {
        out.push(cond(
            "publication_pending",
            label,
            reason.clone(),
            r.publication_hold_ms,
        ));
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub rule: String,
    pub subject: String,
    pub detail: String,
    pub hold_ms: u64,
    pub first_seen_ms: u64,
    /// The last firing or reminder post, once it fired.
    pub notified_ms: Option<u64>,
    /// Cleared after it fired; the resolution is not yet posted.
    pub resolved: bool,
}

/// Alert state across runs, keyed by `rule/subject`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertState {
    pub rules: String,
    pub entries: BTreeMap<String, Entry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Firing,
    Reminder,
    Resolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub state: EventKind,
    pub rule: String,
    pub subject: String,
    pub detail: String,
    pub since_ms: u64,
}

fn key(rule: &str, subject: &str) -> String {
    format!("{rule}/{subject}")
}

/// Records this run's conditions: new ones start their hold, cleared ones that
/// fired await their resolution post, cleared ones that never fired vanish.
pub fn observe(state: &mut AlertState, conditions: &[Condition], now_ms: u64) {
    state.rules = RULES_ID.into();
    let mut present = std::collections::BTreeSet::new();
    for c in conditions {
        let k = key(c.rule, &c.subject);
        present.insert(k.clone());
        let e = state.entries.entry(k).or_insert_with(|| Entry {
            rule: c.rule.into(),
            subject: c.subject.clone(),
            first_seen_ms: now_ms,
            ..Entry::default()
        });
        e.detail = c.detail.clone();
        e.hold_ms = c.hold_ms;
        if e.resolved {
            // It came back before its resolution was posted: still firing.
            e.resolved = false;
        }
    }
    state.entries.retain(|k, e| {
        if present.contains(k) {
            return true;
        }
        if e.notified_ms.is_some() {
            e.resolved = true;
            true
        } else {
            false
        }
    });
}

/// The posts due now.
pub fn due(state: &AlertState, now_ms: u64, renotify_ms: u64) -> Vec<Event> {
    let mut out = Vec::new();
    for e in state.entries.values() {
        let kind = match (e.resolved, e.notified_ms) {
            (true, _) => Some(EventKind::Resolved),
            (false, None) if now_ms.saturating_sub(e.first_seen_ms) >= e.hold_ms => {
                Some(EventKind::Firing)
            }
            (false, Some(at)) if now_ms.saturating_sub(at) >= renotify_ms => {
                Some(EventKind::Reminder)
            }
            _ => None,
        };
        if let Some(state) = kind {
            out.push(Event {
                state,
                rule: e.rule.clone(),
                subject: e.subject.clone(),
                detail: e.detail.clone(),
                since_ms: e.first_seen_ms,
            });
        }
    }
    out
}

/// Marks `events` as posted.
pub fn acknowledge(state: &mut AlertState, events: &[Event], now_ms: u64) {
    for ev in events {
        let k = key(&ev.rule, &ev.subject);
        match ev.state {
            EventKind::Resolved => {
                state.entries.remove(&k);
            }
            EventKind::Firing | EventKind::Reminder => {
                if let Some(e) = state.entries.get_mut(&k) {
                    e.notified_ms = Some(now_ms);
                }
            }
        }
    }
}

/// The largest message one post carries, by sink: Discord refuses a
/// `content` over 2,000 characters; Slack truncates a `text` over 40,000;
/// the generic JSON body uses Slack's bound for its serialized events.
pub fn post_limit(format: SinkFormat) -> usize {
    match format {
        SinkFormat::Discord => 2_000,
        SinkFormat::Slack | SinkFormat::Json => 40_000,
    }
}

fn line(e: &Event, network: &str) -> String {
    let tag = match e.state {
        EventKind::Firing => "FIRING",
        EventKind::Reminder => "STILL FIRING",
        EventKind::Resolved => "RESOLVED",
    };
    format!(
        "[{tag}] slop-ninja {network} {} {}: {}",
        e.rule, e.subject, e.detail
    )
}

/// The size an event adds to a post of `format`.
fn event_size(e: &Event, format: SinkFormat, network: &str) -> usize {
    match format {
        SinkFormat::Json => serde_json::to_vec(e).map_or(0, |b| b.len()) + 1,
        _ => line(e, network).chars().count() + 1,
    }
}

/// The sink's request body for `events`.
pub fn payload(events: &[Event], format: SinkFormat, network: &str) -> Vec<u8> {
    let limit = post_limit(format);
    let text = events
        .iter()
        .map(|e| {
            let l = line(e, network);
            // One line never exceeds the post on its own.
            l.chars().take(limit).collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let body = match format {
        SinkFormat::Json => {
            json!({"schema": ALERT_SCHEMA, "rules": RULES_ID, "network": network, "events": events})
        }
        SinkFormat::Slack => json!({"text": text}),
        SinkFormat::Discord => json!({"content": text}),
    };
    serde_json::to_vec(&body).unwrap_or_default()
}

/// The events split into posts within the sink's limit, in order; each post
/// is delivered and acknowledged on its own, so one refused post never holds
/// back the others.
pub fn batches(events: &[Event], format: SinkFormat, network: &str) -> Vec<Vec<Event>> {
    let limit = post_limit(format);
    let mut out: Vec<Vec<Event>> = Vec::new();
    let mut size = 0usize;
    for e in events {
        let n = event_size(e, format, network).min(limit);
        match out.last_mut() {
            Some(batch) if size + n <= limit => {
                batch.push(e.clone());
                size += n;
            }
            _ => {
                out.push(vec![e.clone()]);
                size = n;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::tests::{eval_status, worker_status};
    use crate::status::{OpView, SigningView, parse_eval, parse_worker};

    const TEMPO: u64 = 360 * 12 * 1000;

    fn rules() -> Rules {
        Rules::derive(TEMPO, 300_000, 6 * 3600 * 1000)
    }

    /// A healthy status, the ledger below its alert ratio.
    fn healthy() -> Observed {
        let mut eval = parse_eval(&eval_status("admission"));
        eval.ledger_permille = Some(500);
        Observed {
            coordinator: Some(parse_worker(&worker_status("primary", false))),
            eval_worker: Some(parse_worker(&worker_status("primary", false))),
            eval: Some(eval),
        }
    }

    fn rules_of(c: &[Condition]) -> Vec<&'static str> {
        c.iter().map(|c| c.rule).collect()
    }

    #[test]
    fn healthy_status_raises_nothing() {
        assert_eq!(
            evaluate(&healthy(), &PublisherObs::default(), &rules()),
            vec![]
        );
    }

    #[test]
    fn each_rule_fires_on_its_condition() {
        let r = rules();
        let p = PublisherObs::default();
        // Durable fault and signing fault.
        let mut o = healthy();
        o.coordinator = Some(parse_worker(&worker_status("primary", true)));
        o.eval_worker
            .as_mut()
            .unwrap()
            .signing
            .as_mut()
            .unwrap()
            .state = "fault".into();
        let c = evaluate(&o, &p, &r);
        assert!(
            c.iter()
                .any(|c| c.rule == "durable_fault" && c.subject == "coordinator")
        );
        assert!(
            c.iter()
                .any(|c| c.rule == "durable_fault" && c.subject == "eval_worker.signing")
        );
        // Fallback: not yet at one tempo, then at it.
        let mut o = healthy();
        let w = o.coordinator.as_mut().unwrap();
        w.mode = Some("fallback".into());
        w.mode_age_ms = Some(TEMPO - 1);
        assert!(!rules_of(&evaluate(&o, &p, &r)).contains(&"fallback_too_long"));
        o.coordinator.as_mut().unwrap().mode_age_ms = Some(TEMPO);
        assert!(rules_of(&evaluate(&o, &p, &r)).contains(&"fallback_too_long"));
        // Stalled watermark.
        let mut o = healthy();
        o.eval_worker.as_mut().unwrap().confirmed_age_ms = Some(r.watermark_stall_ms);
        assert_eq!(rules_of(&evaluate(&o, &p, &r)), vec!["watermark_stalled"]);
        o.eval_worker.as_mut().unwrap().confirmed_age_ms = None;
        assert_eq!(rules_of(&evaluate(&o, &p, &r)), vec!["watermark_stalled"]);
        // Phase overrun: scoring at tempo 17 of K = 20 with 2 transition tempos is
        // fine; at tempo 18 it overruns.
        let mut o = healthy();
        o.coordinator.as_mut().unwrap().signing = None;
        let e = o.eval.as_mut().unwrap();
        e.cohorts[1].phase = "scoring".into();
        e.epoch = Some(23_169 + 17);
        assert!(evaluate(&o, &p, &r).is_empty());
        o.eval.as_mut().unwrap().epoch = Some(23_169 + 18);
        let c = evaluate(&o, &p, &r);
        assert!(
            c.iter()
                .any(|c| c.rule == "phase_overrun" && c.subject == "testnet-c7")
        );
        // Scorer and other cohort faults.
        let mut o = healthy();
        o.eval.as_mut().unwrap().cohorts[1].fault = Some("scorer_unavailable".into());
        assert_eq!(rules_of(&evaluate(&o, &p, &r)), vec!["scorer_budget_fault"]);
        o.eval.as_mut().unwrap().cohorts[1].fault = Some("draw_shortfall".into());
        assert_eq!(rules_of(&evaluate(&o, &p, &r)), vec!["cohort_fault"]);
        // Ledger at 80%: the fixture's status reads 850 per mille.
        let mut o = healthy();
        o.eval.as_mut().unwrap().ledger_permille =
            parse_eval(&eval_status("admission")).ledger_permille;
        let c = evaluate(&o, &p, &r);
        assert_eq!(rules_of(&c), vec!["ledger_threshold"]);
        assert!(
            !c[0].detail.contains("850") && !c[0].detail.contains("700"),
            "no figures"
        );
        // Commit gap and divergence.
        let mut o = healthy();
        o.eval.as_mut().unwrap().epoch = Some(23_169 + 2);
        assert_eq!(rules_of(&evaluate(&o, &p, &r)), vec!["commit_gap"]);
        o.eval.as_mut().unwrap().epoch = Some(23_170);
        o.coordinator.as_mut().unwrap().signing = Some(SigningView {
            state: "active".into(),
            reason: None,
            ops: vec![OpView {
                epoch: 23_169,
                state: "revealed".into(),
                commit_block: Some(1),
                reveal_weights_match: Some(false),
            }],
        });
        assert_eq!(
            rules_of(&evaluate(&o, &p, &r)),
            vec!["coordinator_reveal_mismatch"]
        );
        // Unreachable sources and publication problems.
        let mut o = healthy();
        o.eval_worker = None;
        let p = PublisherObs {
            refused: vec![("c9".into(), "invalid".into())],
            pending: vec![("c10".into(), "missing".into())],
            unverifiable: vec!["old-a".into(), "old-b".into()],
            config: Some("NETWORK must be testnet".into()),
            interrupted: true,
        };
        let c = evaluate(&o, &p, &r);
        assert_eq!(
            rules_of(&c),
            vec![
                "status_unreachable",
                "config_refused",
                "run_interrupted",
                "publication_refused",
                "publication_unverifiable",
                "publication_pending"
            ]
        );
        assert_eq!(c[0].hold_ms, r.unreachable_hold_ms);
        assert_eq!(c[2].hold_ms, TEMPO);
        assert_eq!(c[5].hold_ms, TEMPO);
        // Every unverifiable cohort is one condition, naming the newest.
        assert_eq!(c[4].subject, "manifests");
        assert!(c[4].detail.starts_with("2 closed cohorts") && c[4].detail.contains("old-b"));
    }

    #[test]
    fn holds_reminders_resolutions_and_retries() {
        let mut st = AlertState::default();
        let held = Condition {
            rule: "status_unreachable",
            subject: "eval".into(),
            detail: "d".into(),
            hold_ms: 120_000,
        };
        let now = 1_000_000;
        observe(&mut st, std::slice::from_ref(&held), now);
        assert!(due(&st, now, 3_600_000).is_empty(), "inside its hold");
        // A failed post leaves the event due; the second run posts it.
        observe(&mut st, std::slice::from_ref(&held), now + 120_000);
        let ev = due(&st, now + 120_000, 3_600_000);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].state, EventKind::Firing);
        observe(&mut st, std::slice::from_ref(&held), now + 180_000);
        let ev = due(&st, now + 180_000, 3_600_000);
        assert_eq!(ev[0].state, EventKind::Firing);
        acknowledge(&mut st, &ev, now + 180_000);
        // A duplicate run posts nothing.
        observe(&mut st, std::slice::from_ref(&held), now + 181_000);
        assert!(due(&st, now + 181_000, 3_600_000).is_empty());
        // A reminder after the renotify interval.
        observe(
            &mut st,
            std::slice::from_ref(&held),
            now + 180_000 + 3_600_000,
        );
        let ev = due(&st, now + 180_000 + 3_600_000, 3_600_000);
        assert_eq!(ev[0].state, EventKind::Reminder);
        acknowledge(&mut st, &ev, now + 180_000 + 3_600_000);
        // Cleared: a resolution, then nothing.
        observe(&mut st, &[], now + 4_000_000);
        let ev = due(&st, now + 4_000_000, 3_600_000);
        assert_eq!(ev[0].state, EventKind::Resolved);
        acknowledge(&mut st, &ev, now + 4_000_000);
        assert!(st.entries.is_empty());
        // A condition that clears inside its hold never posts.
        observe(&mut st, std::slice::from_ref(&held), now);
        observe(&mut st, &[], now + 60_000);
        assert!(st.entries.is_empty());
    }

    #[test]
    fn payload_formats() {
        let ev = vec![Event {
            state: EventKind::Firing,
            rule: "commit_gap".into(),
            subject: "coordinator".into(),
            detail: "last commit at epoch 1, now 4".into(),
            since_ms: 5,
        }];
        let j: serde_json::Value =
            serde_json::from_slice(&payload(&ev, SinkFormat::Json, "testnet")).unwrap();
        assert_eq!(j["schema"], ALERT_SCHEMA);
        assert_eq!(j["events"][0]["state"], "firing");
        let s: serde_json::Value =
            serde_json::from_slice(&payload(&ev, SinkFormat::Slack, "testnet")).unwrap();
        assert!(
            s["text"]
                .as_str()
                .unwrap()
                .starts_with("[FIRING] slop-ninja testnet commit_gap")
        );
        let d: serde_json::Value =
            serde_json::from_slice(&payload(&ev, SinkFormat::Discord, "testnet")).unwrap();
        assert!(d["content"].is_string());
        crate::redact::scan_value(&j).unwrap();
    }

    /// Many events (a rotation leaves many refusals) are split into posts
    /// within each sink's limit, in order, losing none.
    #[test]
    fn posts_are_split_within_the_sink_limit() {
        let ev: Vec<Event> = (0..120)
            .map(|i| Event {
                state: EventKind::Firing,
                rule: "publication_refused".into(),
                subject: format!("testnet-c{i}"),
                detail: "manifest.signed: untrusted_key".repeat(3),
                since_ms: i,
            })
            .collect();
        for format in [SinkFormat::Discord, SinkFormat::Slack, SinkFormat::Json] {
            let batches = batches(&ev, format, "testnet");
            let flat: Vec<Event> = batches.iter().flatten().cloned().collect();
            assert_eq!(flat, ev, "{format:?}: all events, in order");
            for b in &batches {
                let body: serde_json::Value =
                    serde_json::from_slice(&payload(b, format, "testnet")).unwrap();
                match format {
                    SinkFormat::Discord => {
                        assert!(body["content"].as_str().unwrap().chars().count() <= 2_000)
                    }
                    SinkFormat::Slack => {
                        assert!(body["text"].as_str().unwrap().chars().count() <= 40_000)
                    }
                    SinkFormat::Json => {
                        assert!(serde_json::to_vec(&body["events"]).unwrap().len() <= 40_000 + 2)
                    }
                }
            }
            if format == SinkFormat::Discord {
                assert!(batches.len() > 1);
            }
        }
    }
}
