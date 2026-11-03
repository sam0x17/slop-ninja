//! Alert rules (`slop_ninja.publisher.alerts/v3`), their state across runs,
//! and the posts to the sink: under decision D18 the private alerts relay
//! (email, through a service binding), or an https sink.
//!
//! Each run turns the observed status into a set of [`Condition`]s
//! ([`evaluate`]). The durable [`AlertState`] (kept in the publisher's
//! Durable Object; the state machine is `slop_ninja_alerts_wire::state`,
//! shared with the relay) remembers when each condition was first seen and
//! what was notified, so a condition fires once it has held for its rule's
//! hold time, is re-posted as a reminder every renotify interval while it
//! lasts, and posts a resolution when it clears. A failed post leaves its
//! events due with the same ids ([`event_id`]), so the next run retries them
//! and the relay sends them once; a duplicate run posts nothing new. The run
//! saves its observed state, with these marks, before it posts anything
//! (`cf.rs`), so a run that dies after posting, or whose save after posting
//! fails, leaves the next run the same first-seen times, hence the same ids.
//!
//! For the alerts relay, which keeps a ledger of what it sent, a firing post
//! that was attempted but never acknowledged is resolved when its condition
//! clears (the relay emails that resolution only if the firing may have gone
//! out), so a lost answer never leaves a firing email unresolved. An https
//! sink has no such ledger: its firings are not marked, and an unconfirmed
//! resolution is never posted to it (the entry is dropped, as before the
//! relay existed). For every sink, a resolution that was attempted marks
//! its entry, so a condition that comes back afterwards fires again as a
//! new episode.
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
//! | `config_warning` | the configuration was accepted with a setting ignored (an `ALERT_SINK_URL` while `ALERT_RELAY` is on) |
//! | `run_interrupted` | every run for a whole tempo stopped before it finished (it never released its lease) |
//!
//! Not covered here (AR-13 gaps, listed in the README): commits by our other
//! validator (validator2's journal is not visible to the publisher), and a
//! divergence between the chain's consensus for the subnet and our row, which
//! needs chain reads.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::SinkFormat;
use crate::status::{Observed, WorkerView};

pub use slop_ninja_alerts_wire::{
    AlertState, Condition, Entry, Event, EventKind, acknowledge, attempt, due, event_id,
};

/// v3: `config_warning`, and resolutions of attempted firings.
pub const RULES_ID: &str = "slop_ninja.publisher.alerts/v3";
/// The post schema (each event carries its id and the last-notified time
/// it is derived from, so the alerts relay sends a retried post once).
pub const ALERT_SCHEMA: &str = slop_ninja_alerts_wire::POST_SCHEMA;

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
    /// The cron period and the unanswered-source hold are the shared wire
    /// crate's, so the alerts relay's dead-man bounds derive from the same
    /// values.
    pub fn derive(tempo_ms: u64, silence_ms: u64, renotify_ms: u64) -> Self {
        Self {
            fallback_ms: tempo_ms,
            watermark_stall_ms: slop_ninja_alerts_wire::watermark_stall_ms(silence_ms),
            commit_gap_tempos: 2,
            ledger_permille: 800,
            unreachable_hold_ms: slop_ninja_alerts_wire::PUBLISHER_UNREACHABLE_HOLD_MS,
            publication_hold_ms: tempo_ms,
            renotify_ms,
        }
    }
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
    /// Settings this run's accepted configuration ignored.
    #[serde(default)]
    pub warnings: Vec<String>,
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
    if !p.warnings.is_empty() {
        out.push(cond(
            "config_warning",
            "publisher",
            p.warnings.join("; "),
            0,
        ));
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

/// Records this run's conditions under [`RULES_ID`] (the state machine is
/// shared with the alerts relay: `slop_ninja_alerts_wire::state`).
pub fn observe(state: &mut AlertState, conditions: &[Condition], now_ms: u64) {
    slop_ninja_alerts_wire::observe(state, RULES_ID, conditions, now_ms);
}

/// The largest message one post carries, by sink: Discord refuses a
/// `content` over 2,000 characters; Slack truncates a `text` over 40,000;
/// the generic JSON body (also the alerts relay's) uses Slack's bound for
/// its serialized events (`slop_ninja_alerts_wire::POST_EVENTS_BYTES`).
pub fn post_limit(format: SinkFormat) -> usize {
    match format {
        SinkFormat::Discord => 2_000,
        SinkFormat::Slack | SinkFormat::Json => slop_ninja_alerts_wire::POST_EVENTS_BYTES,
    }
}

fn line(e: &Event, network: &str) -> String {
    format!(
        "[{}] slop-ninja {network} {} {}: {}",
        e.state.tag(),
        e.rule,
        e.subject,
        e.detail
    )
}

/// The size an event adds to a post of `format`.
fn event_size(e: &Event, format: SinkFormat, network: &str) -> usize {
    match format {
        SinkFormat::Json => e.wire_len() + 1,
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
            return slop_ninja_alerts_wire::Post {
                schema: ALERT_SCHEMA.into(),
                rules: RULES_ID.into(),
                network: network.into(),
                events: events.to_vec(),
            }
            .body();
        }
        SinkFormat::Slack => json!({"text": text}),
        SinkFormat::Discord => json!({"content": text}),
    };
    serde_json::to_vec(&body).unwrap_or_default()
}

/// The events split into posts within the sink's limit, in order; each post
/// is delivered and acknowledged on its own, so one refused post never holds
/// back the others. A JSON event longer than the limit on its own has its
/// detail shortened to fit (its rule and subject, which key the
/// acknowledgment, are kept), so every JSON post stays within the relay's
/// body bound.
pub fn batches(events: &[Event], format: SinkFormat, network: &str) -> Vec<Vec<Event>> {
    let limit = post_limit(format);
    let mut out: Vec<Vec<Event>> = Vec::new();
    let mut size = 0usize;
    for e in events {
        let mut e = e.clone();
        if format == SinkFormat::Json {
            // One event plus its separator within the limit.
            e.fit(limit - 1);
        }
        let n = event_size(&e, format, network).min(limit);
        match out.last_mut() {
            Some(batch) if size + n <= limit => {
                batch.push(e);
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

/// How a sink answered one post.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Posted {
    /// A 2xx.
    Accepted,
    /// Any other answer (the sink refused it, or failed on its side).
    Refused,
    /// No answer within the outbound bound.
    NoAnswer,
}

/// One delivery channel: an https sink or the alerts relay's service
/// binding (`cf.rs`), or a recorder in tests.
#[allow(async_fn_in_trait)]
pub trait Poster {
    /// Delivers one post body.
    async fn post(&self, body: Vec<u8>) -> Posted;
    /// The receiver keeps a ledger of what it sent (the alerts relay), so it
    /// can tell whether an unconfirmed resolution's firing went out.
    fn ledger(&self) -> bool {
        false
    }
}

/// Marks the posts about to be sent to a receiver with a ledger
/// ([`attempt`]: firings and resolutions) or without one (resolutions
/// only). Idempotent; call it before saving the state that precedes the
/// posts.
pub fn mark(state: &mut AlertState, events: &[Event], now_ms: u64, ledger: bool) {
    if ledger {
        attempt(state, events, now_ms);
    } else {
        slop_ninja_alerts_wire::attempt_resolutions(state, events, now_ms);
    }
}

/// Posts `events` in batches within the sink's limit and acknowledges each
/// delivered batch on its own; an undelivered batch stays due, with the same
/// event ids, for the next run. Each post is marked attempted before it is
/// sent ([`mark`]). A receiver without a ledger is never sent an
/// unconfirmed resolution: those entries are dropped. A sink that does not
/// answer at all is not asked again this run (its remaining batches wait
/// for the next run), so a hung sink costs one outbound bound per run, not
/// one per batch. Returns the number of undelivered events.
pub async fn deliver(
    state: &mut AlertState,
    events: &[Event],
    format: SinkFormat,
    network: &str,
    now_ms: u64,
    poster: &impl Poster,
) -> usize {
    let ledger = poster.ledger();
    let (unverifiable, events): (Vec<Event>, Vec<Event>) = events
        .iter()
        .cloned()
        .partition(|e| !ledger && e.unconfirmed());
    acknowledge(state, &unverifiable, now_ms);
    let mut undelivered = 0;
    let mut answering = true;
    for batch in batches(&events, format, network) {
        if !answering {
            undelivered += batch.len();
            continue;
        }
        mark(state, &batch, now_ms, ledger);
        match poster.post(payload(&batch, format, network)).await {
            Posted::Accepted => acknowledge(state, &batch, now_ms),
            Posted::Refused => undelivered += batch.len(),
            Posted::NoAnswer => {
                undelivered += batch.len();
                answering = false;
            }
        }
    }
    undelivered
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
            warnings: vec![],
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
        let ev = vec![Event::new(
            RULES_ID,
            EventKind::Firing,
            "commit_gap",
            "coordinator",
            "last commit at epoch 1, now 4",
            5,
            None,
        )];
        let j: serde_json::Value =
            serde_json::from_slice(&payload(&ev, SinkFormat::Json, "testnet")).unwrap();
        assert_eq!(j["schema"], ALERT_SCHEMA);
        assert_eq!(j["events"][0]["state"], "firing");
        assert_eq!(j["events"][0]["id"], ev[0].id.as_str());
        // The JSON body is exactly the relay's post.
        let post = slop_ninja_alerts_wire::Post::parse(
            &payload(&ev, SinkFormat::Json, "testnet"),
            "testnet",
        )
        .unwrap();
        assert_eq!((post.rules.as_str(), post.events), (RULES_ID, ev.clone()));
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
            .map(|i| {
                Event::new(
                    RULES_ID,
                    EventKind::Firing,
                    "publication_refused",
                    &format!("testnet-c{i}"),
                    &"manifest.signed: untrusted_key".repeat(3),
                    i,
                    None,
                )
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

    /// A recording sink: answers each post in turn from `answers` (then
    /// accepted) and keeps every body it was sent. `new` is the relay (with
    /// a ledger), `plain` an https sink.
    struct Recorder {
        answers: std::cell::RefCell<Vec<Posted>>,
        bodies: std::cell::RefCell<Vec<Vec<u8>>>,
        ledger: bool,
    }

    impl Recorder {
        fn new(answers: &[Posted]) -> Self {
            Self {
                answers: std::cell::RefCell::new(answers.iter().rev().copied().collect()),
                bodies: std::cell::RefCell::new(Vec::new()),
                ledger: true,
            }
        }

        fn plain(answers: &[Posted]) -> Self {
            Self {
                ledger: false,
                ..Self::new(answers)
            }
        }
    }

    impl Poster for Recorder {
        async fn post(&self, body: Vec<u8>) -> Posted {
            self.bodies.borrow_mut().push(body);
            self.answers.borrow_mut().pop().unwrap_or(Posted::Accepted)
        }
        fn ledger(&self) -> bool {
            self.ledger
        }
    }

    /// An https sink (no ledger) that refused a firing, then the condition
    /// clears: nothing is posted for it, as before the relay sink existed.
    /// A resolution that was posted to it and then recurs fires again.
    #[test]
    fn plain_sinks_never_get_unconfirmed_resolutions() {
        use crate::store::block_on;
        let mut st = AlertState::default();
        let now = 1_000_000;
        observe(&mut st, &[held("coordinator")], now);
        let events = due(&st, now, 3_600_000);
        let slack = Recorder::plain(&[Posted::Refused]);
        let left = block_on(deliver(
            &mut st,
            &events,
            SinkFormat::Slack,
            "testnet",
            now,
            &slack,
        ));
        assert_eq!(left, 1);
        assert_eq!(
            st.get("commit_gap", "coordinator").unwrap().attempted_ms,
            None,
            "no firing mark for a sink without a ledger"
        );
        observe(&mut st, &[], now + 60_000);
        assert!(st.entries.is_empty(), "dropped silently");
        // A state carried over from the relay sink (a firing mark): its
        // unconfirmed resolution is dropped, never posted to the https sink.
        let mut st = AlertState::default();
        observe(&mut st, &[held("coordinator")], now);
        let events = due(&st, now, 3_600_000);
        attempt(&mut st, &events, now);
        observe(&mut st, &[], now + 60_000);
        let z = due(&st, now + 60_000, 3_600_000);
        assert!(z[0].unconfirmed());
        let slack = Recorder::plain(&[]);
        let left = block_on(deliver(
            &mut st,
            &z,
            SinkFormat::Slack,
            "testnet",
            now + 60_000,
            &slack,
        ));
        assert_eq!((left, slack.bodies.borrow().len()), (0, 0));
        assert!(st.entries.is_empty());
        // A delivered firing, a resolution whose answer was lost, and the
        // condition back: a new firing.
        let mut st = AlertState::default();
        observe(&mut st, &[held("coordinator")], now);
        let f = due(&st, now, 3_600_000);
        let ok = Recorder::plain(&[]);
        block_on(deliver(&mut st, &f, SinkFormat::Slack, "testnet", now, &ok));
        observe(&mut st, &[], now + 60_000);
        let z = due(&st, now + 60_000, 3_600_000);
        let lost = Recorder::plain(&[Posted::NoAnswer]);
        block_on(deliver(
            &mut st,
            &z,
            SinkFormat::Slack,
            "testnet",
            now + 60_000,
            &lost,
        ));
        observe(&mut st, &[held("coordinator")], now + 120_000);
        let again = due(&st, now + 120_000, 3_600_000);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].state, EventKind::Firing);
        assert_ne!(again[0].id, f[0].id);
    }

    fn held(subject: &str) -> Condition {
        Condition {
            rule: "commit_gap",
            subject: subject.into(),
            detail: "last commit at epoch 1, now 4".into(),
            hold_ms: 0,
        }
    }

    /// The relay sink: posts parse as the relay's contract, an undelivered
    /// post stays due with the same ids (so the relay, which already sent
    /// what it received, sends nothing twice), and a delivered one is
    /// acknowledged.
    #[test]
    fn relay_posts_retry_with_the_same_ids() {
        use crate::store::block_on;
        let mut st = AlertState::default();
        let now = 1_000_000;
        observe(&mut st, &[held("coordinator"), held("eval_worker")], now);
        let events = due(&st, now, 3_600_000);
        assert_eq!(events.len(), 2);
        // The first post's answer is lost.
        let lost = Recorder::new(&[Posted::NoAnswer]);
        let left = block_on(deliver(
            &mut st,
            &events,
            SinkFormat::Json,
            "testnet",
            now,
            &lost,
        ));
        assert_eq!(left, 2);
        let first =
            slop_ninja_alerts_wire::Post::parse(&lost.bodies.borrow()[0], "testnet").unwrap();
        // The next run posts the same events, ids unchanged, and is acknowledged.
        observe(
            &mut st,
            &[held("coordinator"), held("eval_worker")],
            now + 60_000,
        );
        let again = due(&st, now + 60_000, 3_600_000);
        let ok = Recorder::new(&[]);
        assert_eq!(
            block_on(deliver(
                &mut st,
                &again,
                SinkFormat::Json,
                "testnet",
                now + 60_000,
                &ok
            )),
            0
        );
        let second =
            slop_ninja_alerts_wire::Post::parse(&ok.bodies.borrow()[0], "testnet").unwrap();
        let ids = |p: &slop_ninja_alerts_wire::Post| {
            p.events.iter().map(|e| e.id.clone()).collect::<Vec<_>>()
        };
        assert_eq!(ids(&first), ids(&second));
        observe(
            &mut st,
            &[held("coordinator"), held("eval_worker")],
            now + 61_000,
        );
        assert!(due(&st, now + 61_000, 3_600_000).is_empty(), "acknowledged");
        // A reminder carries new ids.
        observe(
            &mut st,
            &[held("coordinator"), held("eval_worker")],
            now + 60_000 + 3_600_000,
        );
        let reminders = due(&st, now + 60_000 + 3_600_000, 3_600_000);
        assert!(reminders.iter().all(|e| e.state == EventKind::Reminder));
        assert!(reminders.iter().all(|e| !ids(&second).contains(&e.id)));
    }

    /// A firing whose post got no answer (the relay may have sent it) is
    /// resolved when the condition clears, with an unconfirmed resolution
    /// the relay emails only if the firing went out; a sink that does not
    /// answer is not asked again in the same run.
    #[test]
    fn unanswered_firings_resolve_and_stop_the_run() {
        use crate::store::block_on;
        let mut st = AlertState::default();
        let now = 1_000_000;
        let many: Vec<Condition> = (0..3)
            .map(|i| {
                let mut c = held(&format!("testnet-c{i}"));
                c.rule = "publication_refused";
                c.detail = "x".repeat(30_000);
                c
            })
            .collect();
        observe(&mut st, &many, now);
        let events = due(&st, now, 3_600_000);
        let rec = Recorder::new(&[Posted::NoAnswer]);
        let left = block_on(deliver(
            &mut st,
            &events,
            SinkFormat::Json,
            "testnet",
            now,
            &rec,
        ));
        assert_eq!(left, 3);
        assert_eq!(
            rec.bodies.borrow().len(),
            1,
            "one post per run to a hung sink"
        );
        // A refused post does not stop the others.
        let rec = Recorder::new(&[Posted::Refused]);
        let left = block_on(deliver(
            &mut st,
            &events,
            SinkFormat::Json,
            "testnet",
            now + 60_000,
            &rec,
        ));
        assert_eq!((left, rec.bodies.borrow().len()), (1, 3));
        // The first firing was attempted, never acknowledged; it clears.
        let rest: Vec<Condition> = many[1..].to_vec();
        observe(&mut st, &rest, now + 120_000);
        let ev = due(&st, now + 120_000, 3_600_000);
        assert_eq!(ev.len(), 1);
        assert!(ev[0].unconfirmed() && ev[0].subject == "testnet-c0");
        let post = slop_ninja_alerts_wire::Post::parse(
            &payload(&ev, SinkFormat::Json, "testnet"),
            "testnet",
        )
        .unwrap();
        assert_eq!(post.events[0].firing_id(RULES_ID), events[0].id);
    }

    /// A setting the configuration ignored is alerted, not refused.
    #[test]
    fn config_warnings_alert() {
        let p = PublisherObs {
            warnings: vec!["ALERT_SINK_URL is ignored".into()],
            ..PublisherObs::default()
        };
        let c = evaluate(&healthy(), &p, &rules());
        assert_eq!(rules_of(&c), vec!["config_warning"]);
        assert_eq!(c[0].detail, "ALERT_SINK_URL is ignored");
    }

    /// The thresholds the alerts relay derives its dead-man bounds from are
    /// the shared wire crate's values, and the cron is the wire crate's
    /// period.
    #[test]
    fn rule_timing_is_the_wire_crates() {
        let r = rules();
        assert_eq!(
            r.unreachable_hold_ms,
            slop_ninja_alerts_wire::PUBLISHER_UNREACHABLE_HOLD_MS
        );
        assert_eq!(
            r.watermark_stall_ms,
            slop_ninja_alerts_wire::watermark_stall_ms(300_000)
        );
        let toml = include_str!("../wrangler.toml");
        let testnet = toml.split("[env.testnet.triggers]").nth(1).unwrap();
        assert!(
            testnet.trim_start().starts_with("crons = [\"* * * * *\"]"),
            "the publisher's cron is once a minute, PUBLISHER_CRON_PERIOD_MS"
        );
        assert_eq!(slop_ninja_alerts_wire::PUBLISHER_CRON_PERIOD_MS, 60_000);
    }

    /// A JSON event whose detail alone exceeds the limit is shortened to fit,
    /// keeping its rule and subject, so the relay never refuses the post for
    /// its size and the acknowledgment still finds the entry.
    #[test]
    fn oversized_json_events_fit_the_relay_bound() {
        use crate::store::block_on;
        let mut st = AlertState::default();
        let mut c = held("testnet-c9");
        c.rule = "publication_refused";
        c.detail = "x".repeat(3 * slop_ninja_alerts_wire::POST_EVENTS_BYTES);
        observe(&mut st, &[c], 5);
        let events = due(&st, 5, 3_600_000);
        let rec = Recorder::new(&[]);
        assert_eq!(
            block_on(deliver(
                &mut st,
                &events,
                SinkFormat::Json,
                "testnet",
                5,
                &rec
            )),
            0
        );
        let body = rec.bodies.borrow()[0].clone();
        assert!(body.len() <= slop_ninja_alerts_wire::post_bound());
        let post = slop_ninja_alerts_wire::Post::parse(&body, "testnet").unwrap();
        assert_eq!(post.events[0].subject, "testnet-c9");
        assert_eq!(post.events[0].id, events[0].id);
        assert!(
            st.get("publication_refused", "testnet-c9")
                .unwrap()
                .notified_ms
                .is_some()
        );
    }
}
