//! Alert rules (`slop_ninja.publisher.alerts/v4`), their state across runs,
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
//! Unknown is never a resolution ([`conditions`]): a status read that is
//! unanswered, times out or fails, or answers without a field a rule reads,
//! decides nothing. The open entries of the status-dependent rules it would
//! decide are carried as they are (neither fired nor resolved), and they
//! resolve only on a later read that answers and shows the condition clear.
//! The unknown read itself alerts after the same hold (two cron periods): a
//! source that does not answer is `status_unreachable`, one that answers
//! without a field a carried entry needs is `status_incomplete`. One missed
//! read sends nothing.
//!
//! | Rule | Fires when |
//! | --- | --- |
//! | `status_unreachable` | a status source has not answered for two runs |
//! | `status_incomplete` | a status source answered without a field an open status rule reads, for two runs |
//! | `durable_fault` | a Worker's journal holds a durable fault, or signing is in `fault` |
//! | `fallback_too_long` | a Worker has been in fallback mode for a whole tempo |
//! | `webhook_paused` | a Worker reports its webhook subscription presumed paused: no delivery for longer than the provider can retry one, while blocks are owed (paused, or a provider-side stall: an operator checks the subscription's state and resumes it only when paused) |
//! | `watermark_stalled` | a Worker's RPC-confirmed watermark is older than the watchdog silence plus two cron periods, or absent |
//! | `phase_overrun` | the active cohort is still in a work phase, or in `admission` with its window no longer admitting (the roster freeze waiting; the detail names on what), at its package deadline (`K - transition` tempos after it opened, in the tempos the coordinator credited: uncredited tempos (an outage, a catch-up jump) extend the cohort's schedule and are not an overrun; epochs only from coordinators that do not report credited tempos) |
//! | `scorer_budget_fault` | a cohort not yet closed (admitting, closing or evaluating) is paused by a scorer or budget fault |
//! | `cohort_fault` | a cohort not yet closed is paused by any other fault (for example `framing_unavailable` while admission waits on a framing step, or `step_refused` when a root step fails in a way no wait changes), or a closed cohort's closure step is paused |
//! | `ledger_threshold` | the spend ledger's billing month (settled spend plus open reservations) is past the deployment's private alert threshold; tracking only, nothing is refused |
//! | `retention_stalled` | the eval status reports a due cohort whose detailed receipts remain after a further completion, or a predecessor authority not yet finished after one (judged by completions, never a clock): deletion is not keeping up with the N+2 rule (a failing step or unreadable authorization pointer, named by its last error) |
//! | `ledger_threshold_unset` | the eval status's ledger standing names no alert threshold (the deploy did not pass `SPEND_ALERT_MICROUSD`), so `ledger_threshold` cannot fire |
//! | `commit_gap` | the coordinator has not committed weights for two or more tempos |
//! | `pointer_stale` | the coordinator's authorization-pointer writer has not confirmed the private pointer (its last pass failed, or its last confirmation is older than two cron periods) for a whole tempo: re-authorizations and revocations are not reaching validators |
//! | `coordinator_reveal_mismatch` | a reveal of the coordinator's weights did not match its committed row |
//! | `publication_refused` | a closed cohort, an artifact's admission record or copy, or the authorization failed verification or the memory budget |
//! | `publication_pending` | a closed cohort, an artifact copy or the authorization has been incomplete for a whole tempo |
//! | `publication_unverifiable` | closed cohorts whose manifest does not open under the compiled trust roots (one alert for all) |
//! | `revocation_unverifiable` | entries of the copied revocation set that do not open under the compiled trust roots (copied unchanged; validators skip them) |
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
use crate::status::{CohortView, EvalView, Observed, WorkerView};

pub use slop_ninja_alerts_wire::{
    AlertState, Condition, Entry, Event, EventKind, acknowledge, attempt, due, event_id,
};

/// v8: `phase_overrun` also covers a cohort still in `admission` at its
/// package deadline with its window closed (its roster freeze waiting), and
/// counts a cohort's age in the coordinator's credited tempos
/// (`observed.active_tempos - opened_active`) rather than epochs, so an
/// outage that extends a cohort's schedule is not an overrun
/// (v7: `cohort_fault` also reads closed cohorts (a paused closure step) and
/// the eval Worker's `step_refused`; v6: `pointer_stale` and
/// `revocation_unverifiable`; v5: `webhook_paused`
/// and `retention_stalled`; v4: an unknown status read, unanswered or answered
/// without a field an open entry needs, carries the open entries it would
/// decide, and alerts after its hold, `status_unreachable` and
/// `status_incomplete`; v3: `config_warning`, and resolutions of attempted
/// firings).
pub const RULES_ID: &str = "slop_ninja.publisher.alerts/v8";
/// The post schema (each event carries its id and the last-notified time
/// it is derived from, so the alerts relay sends a retried post once).
pub const ALERT_SCHEMA: &str = slop_ninja_alerts_wire::POST_SCHEMA;

/// Work phases that must end by the package deadline. A cohort in
/// `admission` whose window no longer admits is bounded by it too
/// ([`deadline_phase`]).
pub const WORK_PHASES: &[&str] = &["generating", "sealing", "scoring", "packaging"];

/// Whether the active cohort `c`, `elapsed` tempos after it opened, is in a
/// phase its package deadline bounds: a work phase, or `admission` once its
/// window no longer admits (closing, its roster freeze waiting, or a cohort
/// without coordinator admission). Without the calendar's window flags for
/// it, the window counts as closed once its `admission_tempos` elapsed.
pub fn deadline_phase(e: &EvalView, c: &CohortView, elapsed: u64) -> bool {
    if WORK_PHASES.contains(&c.phase.as_str()) {
        return true;
    }
    if c.phase != "admission" {
        return false;
    }
    match e.window_of(c.cohort).and_then(|w| w.window_open) {
        Some(open) => !open,
        None => c.admission_tempos.is_some_and(|a| elapsed >= a),
    }
}
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
    /// Revocation-set entries that do not open under the trust roots.
    #[serde(default)]
    pub revocations_unopened: u32,
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
    if w.subscription.as_deref() == Some("presumed_paused") {
        out.push(cond(
            "webhook_paused",
            name,
            "no webhook delivery for longer than the provider retries one: paused, or a provider-side \
             stall; check the subscription's state (GET) before resuming"
                .into(),
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
        // Every cohort not yet closed (an admitting cohort can be paused
        // while an earlier one is still evaluating), and a closed one whose
        // closure step is paused (`step_refused`, or an arena or object
        // bound): its closure is never written until an operator clears it.
        for c in e
            .cohorts
            .iter()
            .filter(|c| c.active() || (!c.load && c.phase == "closed"))
        {
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
        }
        if let Some(c) = e.active()
            && let (Some((elapsed, credited)), Some(k)) = (e.cohort_age(c), c.k_tempos)
        {
            let deadline = k.saturating_sub(c.transition_tempos.unwrap_or(0));
            let unit = if credited {
                "credited tempos"
            } else {
                "epochs"
            };
            if elapsed >= deadline && deadline_phase(e, c, elapsed) {
                let phase = match (c.phase.as_str(), e.window_of(c.cohort)) {
                    ("admission", w) => format!(
                        "admission with its window closed (roster freeze: {})",
                        w.and_then(|w| w.freeze_wait.as_deref())
                            .unwrap_or("not reported")
                    ),
                    (p, _) => p.to_owned(),
                };
                out.push(cond(
                    "phase_overrun",
                    &c.label,
                    format!("{phase} after {elapsed} {unit}; package deadline {deadline}"),
                    0,
                ));
            }
        }
        if let Some(stalled) = e.retention_stalled.as_ref().filter(|v| !v.is_empty()) {
            out.push(cond(
                "retention_stalled",
                "receipts",
                format!(
                    "{}; last error: {}",
                    stalled.join("; "),
                    e.retention_error.as_deref().unwrap_or("none")
                ),
                0,
            ));
        }
        if e.ledger_over_threshold == Some(true) {
            out.push(cond(
                "ledger_threshold",
                "ledger",
                "the billing month's settled spend and open reservations are past the private alert threshold (tracking only)"
                    .to_string(),
                0,
            ));
        }
        if e.ledger_threshold_set == Some(false) {
            out.push(cond(
                "ledger_threshold_unset",
                "ledger",
                "the ledger reports no alert threshold (SPEND_ALERT_MICROUSD was not passed at deploy): ledger_threshold cannot fire"
                    .to_string(),
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
    if let Some(pointer) = o
        .coordinator
        .as_ref()
        .and_then(|w| w.signing.as_ref())
        .and_then(|s| s.pointer.as_ref())
    {
        let stale = pointer
            .confirmed_age_ms
            .is_none_or(|age| age > r.unreachable_hold_ms);
        if !pointer.current || stale {
            out.push(cond(
                "pointer_stale",
                "coordinator",
                format!(
                    "authorization pointer last confirmed {}; last failure: {}",
                    pointer
                        .confirmed_age_ms
                        .map_or_else(|| "never".into(), |a| format!("{} s ago", a / 1000)),
                    pointer.error.as_deref().unwrap_or("none")
                ),
                r.publication_hold_ms,
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
    if p.revocations_unopened > 0 {
        out.push(cond(
            "revocation_unverifiable",
            "authorizations",
            format!(
                "{} revocation set entries do not open under the compiled trust roots; copied \
                 unchanged, validators skip them (a trust-root rotation not yet deployed here?)",
                p.revocations_unopened
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

/// The rules whose conditions a status read decides. Their open entries are
/// carried while that read is unknown ([`conditions`]).
pub const STATUS_RULES: &[&str] = &[
    "status_incomplete",
    "durable_fault",
    "fallback_too_long",
    "webhook_paused",
    "watermark_stalled",
    "phase_overrun",
    "scorer_budget_fault",
    "cohort_fault",
    "ledger_threshold",
    "retention_stalled",
    "ledger_threshold_unset",
    "commit_gap",
    "coordinator_reveal_mismatch",
    "pointer_stale",
];

/// What a status-dependent rule's decision lacks this run: a source that
/// did not answer, or a field an answered source's status omits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    Unanswered(&'static str),
    Field(&'static str, &'static str),
}

/// A field of a Worker's status: its name and whether a read holds it.
type WorkerField = (&'static str, fn(&WorkerView) -> bool);

/// The coordinator's signing view (`commit_gap`, the reveal check and a
/// `.signing` subject's `durable_fault`).
const SIGNING: WorkerField = ("signing", |w| w.signing.is_some());
/// The pointer writer's fields `pointer_stale` reads (needed only while an
/// authorization is installed).
const POINTER: WorkerField = ("signing.authorization.pointer_current", |w| {
    w.signing
        .as_ref()
        .is_some_and(|s| !s.authorization || s.pointer.is_some())
});
/// The journal block a Worker's `durable_fault` reads.
const JOURNAL: WorkerField = ("chain_journal", |w| !w.journal_missing);
/// The mode `fallback_too_long` reads, and its age while in fallback.
const MODE: WorkerField = ("mode", |w| w.mode.is_some());
const MODE_AGE: WorkerField = ("mode_age_ms", |w| {
    w.mode.as_deref() != Some("fallback") || w.mode_age_ms.is_some()
});
/// The subscription state `webhook_paused` reads.
const SUBSCRIPTION: WorkerField = ("watchdog.subscription", |w| w.subscription.is_some());

fn need_worker(o: &Observed, name: &str, fields: &[WorkerField], n: &mut Vec<Need>) {
    let (source, view) = match name {
        "coordinator" => ("coordinator", o.coordinator.as_ref()),
        "eval_worker" => ("eval_worker", o.eval_worker.as_ref()),
        _ => return,
    };
    match view {
        None => n.push(Need::Unanswered(source)),
        Some(w) => n.extend(
            fields
                .iter()
                .filter(|(_, present)| !present(w))
                .map(|(field, _)| Need::Field(source, field)),
        ),
    }
}

/// A field of the eval status: its name and whether a read holds it.
type EvalField = (&'static str, fn(&EvalView) -> bool);

fn need_eval(o: &Observed, field: Option<EvalField>, n: &mut Vec<Need>) {
    match (&o.eval, field) {
        (None, _) => n.push(Need::Unanswered("eval")),
        (Some(e), Some((name, present))) if !present(e) => n.push(Need::Field("eval", name)),
        _ => {}
    }
}

/// The eval status's cohort list, complete (every cohort with its number,
/// label and phase).
const COHORTS: EvalField = ("cohorts", |e| !e.cohorts_missing);

/// What this run's reads lack to decide `rule/subject` (a status-dependent
/// rule): `commit_gap` and the reveal check need the coordinator's signing
/// view and the eval status's epoch; the cohort rules the eval status's
/// complete cohort list, and `phase_overrun` also its clock (credited tempos
/// `observed.active_tempos`, else the epoch) and, while its cohort is the
/// active one in a work phase or in `admission`, that cohort's opening on
/// the same clock (`opened_active`, else `opened_epoch`) and `k_tempos`;
/// `ledger_threshold` the eval status's ledger flag,
/// `ledger_threshold_unset` its ledger standing, `retention_stalled` its
/// `receipt_retention.stalled` list; a
/// Worker's `durable_fault` its journal block (a `.signing` subject the
/// signing view instead), `fallback_too_long` its mode (and the mode's age
/// in fallback), `webhook_paused` its subscription state, `watermark_stalled` its answer (a missing watermark fires);
/// `status_incomplete` its source's answer.
pub fn needs(rule: &str, subject: &str, o: &Observed) -> Vec<Need> {
    let mut n = Vec::new();
    match rule {
        "durable_fault" => match subject.strip_suffix(".signing") {
            Some(name) => need_worker(o, name, &[SIGNING], &mut n),
            None => need_worker(o, subject, &[JOURNAL], &mut n),
        },
        "fallback_too_long" => need_worker(o, subject, &[MODE, MODE_AGE], &mut n),
        "webhook_paused" => need_worker(o, subject, &[SUBSCRIPTION], &mut n),
        "watermark_stalled" => need_worker(o, subject, &[], &mut n),
        "scorer_budget_fault" | "cohort_fault" => need_eval(o, Some(COHORTS), &mut n),
        "phase_overrun" => {
            need_eval(
                o,
                Some(("epoch", |e| e.active_tempos.is_some() || e.epoch.is_some())),
                &mut n,
            );
            if o.eval.as_ref().is_some_and(|e| e.cohorts_missing) {
                n.push(Need::Field("eval", "cohorts"));
            }
            if let Some(c) = o.eval.as_ref().and_then(|e| e.active())
                && c.label == subject
                && (WORK_PHASES.contains(&c.phase.as_str()) || c.phase == "admission")
            {
                let credited = o.eval.as_ref().is_some_and(|e| e.active_tempos.is_some());
                if credited && c.opened_active.is_none() {
                    n.push(Need::Field("eval", "opened_active"));
                }
                if !credited && c.opened_epoch.is_none() {
                    n.push(Need::Field("eval", "opened_epoch"));
                }
                if c.k_tempos.is_none() {
                    n.push(Need::Field("eval", "k_tempos"));
                }
            }
        }
        "ledger_threshold" => need_eval(
            o,
            Some(("ledger flag", |e| e.ledger_over_threshold.is_some())),
            &mut n,
        ),
        "retention_stalled" => need_eval(
            o,
            Some(("receipt_retention.stalled", |e| {
                e.retention_stalled.is_some()
            })),
            &mut n,
        ),
        "ledger_threshold_unset" => need_eval(
            o,
            Some(("ledger standing", |e| e.ledger_threshold_set.is_some())),
            &mut n,
        ),
        "commit_gap" | "coordinator_reveal_mismatch" => {
            need_worker(o, "coordinator", &[SIGNING], &mut n);
            need_eval(o, Some(("epoch", |e| e.epoch.is_some())), &mut n);
        }
        "pointer_stale" => need_worker(o, "coordinator", &[SIGNING, POINTER], &mut n),
        "status_incomplete" => match subject {
            "eval" => need_eval(o, None, &mut n),
            name => need_worker(o, name, &[], &mut n),
        },
        _ => {}
    }
    n
}

/// Whether this run's reads decide `rule/subject`: the reads it depends on
/// answered and hold the fields it reads ([`needs`]).
pub fn decided(rule: &str, subject: &str, o: &Observed) -> bool {
    needs(rule, subject, o).is_empty()
}

/// The conditions that hold this run, given the alert state the earlier
/// runs left: [`evaluate`]'s, plus every open entry of a
/// [`STATUS_RULES`] rule that this run's reads do not decide ([`decided`]),
/// carried with its detail and hold. An unknown read therefore neither
/// fires (no new entry comes from it) nor resolves (the open entry stays as
/// it was); a resolution needs a read that answered and shows the condition
/// clear. An entry already resolving (a successful read cleared it) is not
/// revived. A source that did not answer is `status_unreachable`'s, which
/// fires after its hold; a source that answered without a field a carried
/// entry needs raises `status_incomplete <source>` with the same hold,
/// naming the fields and the entries they leave undecided. A carried
/// entry's detail is its last decided one with a note of how long its read
/// has been unknown ([`UNKNOWN_NOTE`]), so a reminder sent meanwhile does
/// not read as a fresh observation; the note never changes an event id.
pub fn conditions(
    o: &Observed,
    p: &PublisherObs,
    r: &Rules,
    state: &AlertState,
    now_ms: u64,
) -> Vec<Condition> {
    let mut out = evaluate(o, p, r);
    let mut incomplete: std::collections::BTreeMap<
        &'static str,
        std::collections::BTreeSet<String>,
    > = std::collections::BTreeMap::new();
    for e in state.entries.values().filter(|e| !e.resolved) {
        let Some(rule) = STATUS_RULES.iter().copied().find(|x| *x == e.rule) else {
            continue;
        };
        let lacks = needs(rule, &e.subject, o);
        if lacks.is_empty() || out.iter().any(|c| c.rule == rule && c.subject == e.subject) {
            continue;
        }
        out.push(Condition {
            rule,
            subject: e.subject.clone(),
            detail: carried_detail(&e.detail, &lacks, state, now_ms),
            hold_ms: e.hold_ms,
        });
        for need in lacks {
            if let Need::Field(source, field) = need {
                incomplete
                    .entry(source)
                    .or_default()
                    .insert(format!("{field} ({rule} {})", e.subject));
            }
        }
    }
    for (source, what) in incomplete {
        if out
            .iter()
            .any(|c| c.rule == "status_incomplete" && c.subject == source)
        {
            out.retain(|c| !(c.rule == "status_incomplete" && c.subject == source));
        }
        out.push(cond(
            "status_incomplete",
            source,
            format!(
                "status answered without {}",
                what.into_iter().collect::<Vec<_>>().join(", ")
            ),
            r.unreachable_hold_ms,
        ));
    }
    out
}

/// Where a carried entry's note starts: `<detail> [unknown for <age> s:
/// <what>]`, the age counted from the first run of the unknown reads'
/// `status_unreachable` or `status_incomplete` entry (this run when there is
/// none yet).
pub const UNKNOWN_NOTE: &str = " [unknown for ";

/// A carried entry's detail: its last decided detail (any earlier note cut)
/// and a note naming what is unknown and for how long.
fn carried_detail(detail: &str, lacks: &[Need], state: &AlertState, now_ms: u64) -> String {
    let decided = detail.find(UNKNOWN_NOTE).map_or(detail, |i| &detail[..i]);
    let mut since = now_ms;
    let mut what = Vec::new();
    for need in lacks {
        let (rule, source, text) = match need {
            Need::Unanswered(source) => (
                "status_unreachable",
                *source,
                format!("{source} status did not answer"),
            ),
            Need::Field(source, field) => (
                "status_incomplete",
                *source,
                format!("{source} status without {field}"),
            ),
        };
        if let Some(e) = state.get(rule, source).filter(|e| !e.resolved) {
            since = since.min(e.first_seen_ms);
        }
        what.push(text);
    }
    format!(
        "{decided}{UNKNOWN_NOTE}{} s: {}]",
        now_ms.saturating_sub(since) / 1000,
        what.join(", ")
    )
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
    use crate::config::CRON_PERIOD_MS;
    use crate::status::tests::{eval_status, worker_status};
    use crate::status::{OpView, SigningView, parse_eval, parse_worker};
    use serde_json::Value;

    const TEMPO: u64 = 360 * 12 * 1000;

    fn rules() -> Rules {
        Rules::derive(TEMPO, 300_000, 6 * 3600 * 1000)
    }

    /// A healthy status, the ledger below its alert threshold.
    fn healthy() -> Observed {
        let mut eval = parse_eval(&eval_status("admission"));
        eval.ledger_over_threshold = Some(false);
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

    /// At the daily shape with a 17-tempo window (K 20, transition 2) the
    /// freeze has no spare tempo: a cohort still in `admission` with its
    /// window closed overruns at the package deadline (18 tempos after it
    /// opened), and the detail names what the freeze waits on. An open
    /// window (extended past uncredited tempos) does not.
    #[test]
    fn a_stalled_roster_freeze_overruns_the_package_deadline() {
        let (r, p) = (rules(), PublisherObs::default());
        let at = |elapsed: u64, calendar: Value| -> Observed {
            let mut j = eval_status("admission");
            j["cohorts"][1]["admission_tempos"] = json!(17);
            j["observed"]["epoch"]["index"] = json!(23_169 + elapsed);
            if !calendar.is_null() {
                j["calendar"] = calendar;
            }
            let mut e = parse_eval(&j);
            e.ledger_over_threshold = Some(false);
            let mut o = healthy();
            o.coordinator.as_mut().unwrap().signing = None;
            o.eval = Some(e);
            o
        };
        let window = |open: bool, wait: Value| {
            json!({"current": {"cohort": 7, "window_open": open, "window_closing": !open, "freeze_wait": wait},
                   "next": {"cohort": 8, "earliest_open_epoch": 23_189, "admission_tempos": 17}})
        };
        let closing = window(false, json!("commitment snapshot pending"));
        assert!(evaluate(&at(17, closing.clone()), &p, &r).is_empty());
        let c = evaluate(&at(18, closing.clone()), &p, &r);
        assert_eq!(rules_of(&c), vec!["phase_overrun"]);
        assert_eq!(c[0].subject, "testnet-c7");
        assert_eq!(
            c[0].detail,
            "admission with its window closed (roster freeze: commitment snapshot pending) after 18 epochs; package deadline 18"
        );
        // Still admitting at the deadline: not a freeze stall.
        assert!(evaluate(&at(18, window(true, Value::Null)), &p, &r).is_empty());
        // No calendar flags: the window counts as closed once its 17
        // tempos elapsed.
        let c = evaluate(&at(18, Value::Null), &p, &r);
        assert_eq!(rules_of(&c), vec!["phase_overrun"]);
        assert!(c[0].detail.contains("roster freeze: not reported"));
        // The rule reads the admitting cohort's opening: without it the
        // read decides nothing.
        let mut o = at(18, closing);
        o.eval.as_mut().unwrap().cohorts[1].opened_epoch = None;
        assert!(!decided("phase_overrun", "testnet-c7", &o));
    }

    /// The package deadline is counted in the tempos the coordinator
    /// credited, the clock it schedules the cohort by: after an uncredited
    /// tempo (an outage, a stale heartbeat, a catch-up jump) the window
    /// closes one epoch later than planned, and neither its freeze nor the
    /// work after it is an overrun until the credited deadline.
    #[test]
    fn an_uncredited_tempo_moves_the_package_deadline_with_the_schedule() {
        let (r, p) = (rules(), PublisherObs::default());
        // Cohort 7 opened at epoch 23,169 with 100 tempos credited; `epochs`
        // after it, `uncredited` of them not credited.
        let at = |phase: &str,
                  epochs: u64,
                  uncredited: u64,
                  admission: u64,
                  open: Option<bool>|
         -> Observed {
            let mut j = eval_status(phase);
            j["cohorts"][1]["admission_tempos"] = json!(admission);
            j["cohorts"][1]["opened_active"] = json!(100);
            j["observed"]["epoch"]["index"] = json!(23_169 + epochs);
            j["observed"]["active_tempos"] = json!(100 + epochs - uncredited);
            if let Some(open) = open {
                j["calendar"] = json!({"current": {"cohort": 7, "window_open": open, "window_closing": !open,
                    "freeze_wait": if open { Value::Null } else { json!("verification or verdicts pending") }}});
            }
            let mut e = parse_eval(&j);
            e.ledger_over_threshold = Some(false);
            let mut o = healthy();
            o.coordinator.as_mut().unwrap().signing = None;
            o.eval = Some(e);
            o
        };
        for admission in [17, 16] {
            // One uncredited tempo during admission: the window closes at
            // epoch 18 (17 credited), its freeze still verifying. Counted in
            // epochs this was an overrun; in credited tempos it is not.
            let closing = at("admission", 18, 1, admission, Some(false));
            let e = closing.eval.as_ref().unwrap();
            assert_eq!(e.cohort_age(&e.cohorts[1]), Some((17, true)));
            assert!(
                evaluate(&closing, &p, &r).is_empty(),
                "admission {admission}"
            );
            assert!(decided("phase_overrun", "testnet-c7", &closing));
            // Still in admission at the credited deadline: the freeze stalls.
            let c = evaluate(&at("admission", 19, 1, admission, Some(false)), &p, &r);
            assert_eq!(rules_of(&c), vec!["phase_overrun"]);
            assert_eq!(
                c[0].detail,
                "admission with its window closed (roster freeze: verification or verdicts pending) after 18 credited tempos; package deadline 18"
            );
            // Without the calendar flags the window counts as closed once its
            // tempos were credited, still on the credited clock.
            assert!(evaluate(&at("admission", 18, 1, admission, None), &p, &r).is_empty());
        }
        // Three uncredited tempos (an outage): every work phase is on its
        // credited schedule until 18 credited tempos.
        for phase in WORK_PHASES {
            assert!(
                evaluate(&at(phase, 20, 3, 16, None), &p, &r).is_empty(),
                "{phase}"
            );
            let c = evaluate(&at(phase, 21, 3, 16, None), &p, &r);
            assert_eq!(rules_of(&c), vec!["phase_overrun"], "{phase}");
            assert_eq!(
                c[0].detail,
                format!("{phase} after 18 credited tempos; package deadline 18")
            );
        }
        // The credited count reported without the cohort's credited opening
        // decides nothing, and never falls back to epochs (which would fire).
        let mut o = at("scoring", 21, 3, 16, None);
        o.eval.as_mut().unwrap().cohorts[1].opened_active = None;
        assert!(evaluate(&o, &p, &r).is_empty());
        assert!(!decided("phase_overrun", "testnet-c7", &o));
        assert_eq!(
            needs("phase_overrun", "testnet-c7", &o),
            vec![Need::Field("eval", "opened_active")]
        );
        // The credited clock needs no epoch.
        let mut o = at("scoring", 21, 0, 16, None);
        o.eval.as_mut().unwrap().epoch = None;
        assert!(decided("phase_overrun", "testnet-c7", &o));
        assert_eq!(rules_of(&evaluate(&o, &p, &r)), vec!["phase_overrun"]);
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
        // A retention stall the coordinator reports (completions, not a
        // clock), with its last error.
        let mut o = healthy();
        let mut j = eval_status("admission");
        j["receipt_retention"]["stalled"] = json!([
            {"kind": "cohort", "cohort": 3, "label": "testnet-c3", "due_after_cohort": 5, "completions_since": 1},
            {"kind": "predecessor", "predecessor": "main", "after_cohort": 14, "completions_since": 2}
        ]);
        j["receipt_retention"]["last_error"] = json!("authorization_pointer");
        let mut e = parse_eval(&j);
        e.ledger_over_threshold = Some(false);
        o.eval = Some(e);
        let c = evaluate(&o, &p, &r);
        assert_eq!(rules_of(&c), vec!["retention_stalled"]);
        assert_eq!(
            c[0].detail,
            "cohort 3 (testnet-c3) due after cohort 5, 1 completion(s) since; predecessor main not \
             finished after cohort 14, 2 completion(s) since; last error: authorization_pointer"
        );
        // Fix round 2, finding 5: the next cohort paused in admission
        // (`framing_unavailable`) while the earlier one still evaluates.
        let mut o = healthy();
        let e = o.eval.as_mut().unwrap();
        e.cohorts[1].phase = "scoring".into();
        e.cohorts[2].phase = "admission".into();
        e.cohorts[2].fault = Some("framing_unavailable".into());
        let c = evaluate(&o, &p, &r);
        assert_eq!(rules_of(&c), vec!["cohort_fault"]);
        assert_eq!(c[0].subject, "testnet-c8");
        // Audit B1: a closed cohort whose closure step is paused (it cannot
        // be written until an operator clears it) alerts too.
        let mut o = healthy();
        o.eval.as_mut().unwrap().cohorts[0].fault = Some("step_refused".into());
        let c = evaluate(&o, &p, &r);
        assert_eq!(rules_of(&c), vec!["cohort_fault"]);
        assert_eq!(
            (c[0].subject.as_str(), c[0].detail.as_str()),
            ("testnet-c6", "fault step_refused in closed")
        );
        // Fix round 2, findings 2 and 4: a ledger standing without a
        // threshold is reported, never read as "not over".
        let mut j = eval_status("admission");
        j["ledger"]["standing"]["alert_microusd"] = serde_json::Value::Null;
        j["ledger"]["standing"]["over_threshold"] = json!(false);
        j["ledger"]["over_threshold"] = json!(false);
        let mut o = healthy();
        o.eval = Some(parse_eval(&j));
        let c = evaluate(&o, &p, &r);
        assert_eq!(rules_of(&c), vec!["ledger_threshold_unset"]);
        assert!(
            !c[0].detail.contains("1050") && !c[0].detail.contains("900"),
            "no figures"
        );
        // Past the alert threshold: the fixture's status carries the flag.
        let mut o = healthy();
        o.eval.as_mut().unwrap().ledger_over_threshold =
            parse_eval(&eval_status("admission")).ledger_over_threshold;
        let c = evaluate(&o, &p, &r);
        assert_eq!(rules_of(&c), vec!["ledger_threshold"]);
        assert!(
            !c[0].detail.contains("1050") && !c[0].detail.contains("150"),
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
            ..SigningView::default()
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
            revocations_unopened: 0,
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

    /// One run of the alert phase over `o`, as `cf.rs` runs it with the
    /// relay (every post accepted): the due events, acknowledged.
    fn run(st: &mut AlertState, o: &Observed, now: u64) -> Vec<Event> {
        let r = rules();
        let c = conditions(o, &PublisherObs::default(), &r, st, now);
        observe(st, &c, now);
        let ev = due(st, now, r.renotify_ms);
        mark(st, &ev, now, true);
        acknowledge(st, &ev, now);
        ev
    }

    /// The coordinator has not committed for two tempos (the testnet's
    /// standing condition while its signing is deferred).
    fn gap() -> Observed {
        let mut o = healthy();
        o.eval.as_mut().unwrap().epoch = Some(23_169 + 2);
        o
    }

    fn kinds(ev: &[Event]) -> Vec<(EventKind, &str, &str)> {
        ev.iter()
            .map(|e| (e.state, e.rule.as_str(), e.subject.as_str()))
            .collect()
    }

    /// 2026-11-02 18:52Z: `commit_gap coordinator` was firing; one run's
    /// coordinator status read timed out, the rule (evaluated only on an
    /// answer) dropped the condition, the state machine resolved it, and the
    /// next run fired it again: two messages for one missed read. Now the
    /// unanswered read carries the entry: no message at all, and the missed
    /// read clears inside `status_unreachable`'s hold.
    #[test]
    fn a_timed_out_status_read_is_not_a_resolution() {
        let t = 1_793_645_520_000; // 2026-11-02T18:52:00Z
        let mut st = AlertState::default();
        let fired = run(&mut st, &gap(), t - 20 * 60_000);
        assert_eq!(
            kinds(&fired),
            vec![(EventKind::Firing, "commit_gap", "coordinator")]
        );
        assert!(run(&mut st, &gap(), t - 60_000).is_empty());
        let mut timed_out = gap();
        timed_out.coordinator = None;
        // The defect, reproduced with the old evaluation (no carry): a
        // resolution, then a new firing.
        let mut old = st.clone();
        let mut old_posts = Vec::new();
        for (o, now) in [(&timed_out, t), (&gap(), t + 52_000)] {
            observe(
                &mut old,
                &evaluate(o, &PublisherObs::default(), &rules()),
                now,
            );
            let ev = due(&old, now, rules().renotify_ms);
            acknowledge(&mut old, &ev, now);
            old_posts.extend(ev);
        }
        assert_eq!(
            kinds(&old_posts),
            vec![
                (EventKind::Resolved, "commit_gap", "coordinator"),
                (EventKind::Firing, "commit_gap", "coordinator")
            ]
        );
        // The fix: the same two runs post nothing.
        assert!(run(&mut st, &timed_out, t).is_empty());
        let e = st.get("commit_gap", "coordinator").unwrap();
        assert!(!e.resolved && e.notified_ms == Some(t - 20 * 60_000));
        assert!(
            st.get("status_unreachable", "coordinator")
                .is_some_and(|e| e.notified_ms.is_none()),
            "the missed read is counted"
        );
        assert!(run(&mut st, &gap(), t + 52_000).is_empty());
        assert!(st.get("status_unreachable", "coordinator").is_none());
        assert_eq!(
            st.get("commit_gap", "coordinator").unwrap().first_seen_ms,
            t - 20 * 60_000,
            "the same episode"
        );
    }

    /// A reminder sent while the read stays unknown restates the last
    /// decided detail with a note of how long the read has been unknown and
    /// why, never as a fresh observation; the note does not pile up, leaves
    /// the event id alone and goes once a read decides again.
    #[test]
    fn a_carried_reminder_says_its_read_is_unknown() {
        let t = 1_793_645_520_000;
        let r = rules();
        let mut st = AlertState::default();
        let fired = run(&mut st, &gap(), t);
        assert_eq!(fired[0].detail, "last commit at epoch 23169, now 23171");
        let mut silent = gap();
        silent.coordinator = None;
        let runs = r.renotify_ms / CRON_PERIOD_MS;
        let mut reminders = Vec::new();
        for i in 1..=runs {
            reminders.extend(
                run(&mut st, &silent, t + i * CRON_PERIOD_MS)
                    .into_iter()
                    .filter(|e| e.rule == "commit_gap"),
            );
        }
        assert_eq!(
            kinds(&reminders),
            vec![(EventKind::Reminder, "commit_gap", "coordinator")]
        );
        let d = &reminders[0].detail;
        let age = (runs - 1) * CRON_PERIOD_MS / 1000;
        assert_eq!(
            d,
            &format!(
                "last commit at epoch 23169, now 23171{UNKNOWN_NOTE}{age} s: coordinator status did not answer]"
            ),
            "counted from the first unknown run (status_unreachable's first sight)"
        );
        assert_eq!(d.matches(UNKNOWN_NOTE).count(), 1);
        assert_eq!(
            reminders[0].id,
            event_id(
                RULES_ID,
                EventKind::Reminder,
                "commit_gap",
                "coordinator",
                t,
                Some(t)
            ),
            "the note leaves the id alone"
        );
        // A field missing instead: named as such.
        let mut partial = gap();
        partial.coordinator.as_mut().unwrap().signing = None;
        let c = conditions(
            &partial,
            &PublisherObs::default(),
            &r,
            &st,
            t + (runs + 1) * CRON_PERIOD_MS,
        );
        let carried = c.iter().find(|c| c.rule == "commit_gap").unwrap();
        assert!(
            carried
                .detail
                .ends_with("coordinator status without signing]")
                && carried.detail.matches(UNKNOWN_NOTE).count() == 1,
            "{}",
            carried.detail
        );
        // A decided read: the fresh detail, no note.
        run(&mut st, &gap(), t + (runs + 2) * CRON_PERIOD_MS);
        assert_eq!(
            st.get("commit_gap", "coordinator").unwrap().detail,
            "last commit at epoch 23169, now 23171"
        );
    }

    /// One message per real transition: a firing, nothing while unknown
    /// reads come and go, and a resolution only on an answered read that
    /// shows the condition clear.
    #[test]
    fn one_message_per_real_transition() {
        let t = 1_793_644_320_000;
        let mut st = AlertState::default();
        let mut unknown = gap();
        unknown.coordinator = None;
        let mut no_eval = gap();
        no_eval.eval = None;
        let mut no_signing = gap();
        no_signing.coordinator.as_mut().unwrap().signing = None;
        let script = [
            gap(),
            unknown.clone(),
            gap(),
            no_eval,
            no_signing,
            unknown.clone(),
            gap(),
            healthy(),
            healthy(),
            unknown,
            healthy(),
        ];
        let mut posts = Vec::new();
        for (i, o) in script.iter().enumerate() {
            posts.extend(run(&mut st, o, t + i as u64 * 60_000));
        }
        assert_eq!(
            kinds(&posts),
            vec![
                (EventKind::Firing, "commit_gap", "coordinator"),
                (EventKind::Resolved, "commit_gap", "coordinator")
            ]
        );
        assert!(st.entries.is_empty());
    }

    /// An unknown read never fires: a condition no read has shown stays
    /// unseen, and the missed read alerts only after `status_unreachable`'s
    /// hold, once, then resolves when the source answers.
    #[test]
    fn unknown_reads_never_fire_and_alert_after_the_hold() {
        let t = 1_793_644_320_000;
        let r = rules();
        let mut st = AlertState::default();
        let mut silent = healthy();
        silent.coordinator = None;
        let mut posts = Vec::new();
        let runs = r.unreachable_hold_ms / CRON_PERIOD_MS + 3;
        for i in 0..runs {
            posts.extend(run(&mut st, &silent, t + i * CRON_PERIOD_MS));
        }
        assert_eq!(
            kinds(&posts),
            vec![(EventKind::Firing, "status_unreachable", "coordinator")]
        );
        assert_eq!(posts[0].since_ms, t);
        assert!(st.get("commit_gap", "coordinator").is_none());
        let back = run(&mut st, &gap(), t + runs * CRON_PERIOD_MS);
        assert_eq!(
            kinds(&back),
            vec![
                (EventKind::Firing, "commit_gap", "coordinator"),
                (EventKind::Resolved, "status_unreachable", "coordinator")
            ]
        );
    }

    /// Every status-dependent rule is carried while the read it depends on
    /// is unknown, and resolves on an answered read that shows it clear; an
    /// unknown read of another source does not keep it open; a resolving
    /// entry is not revived by an unknown read.
    #[test]
    fn each_status_rule_waits_for_its_own_read() {
        let r = rules();
        let p = PublisherObs::default();
        type Edit = fn(&mut Observed);
        let firing: [(&str, &str, Edit); 12] = [
            ("durable_fault", "coordinator", |o| {
                o.coordinator = Some(parse_worker(&worker_status("primary", true)))
            }),
            ("durable_fault", "eval_worker.signing", |o| {
                o.eval_worker
                    .as_mut()
                    .unwrap()
                    .signing
                    .as_mut()
                    .unwrap()
                    .state = "fault".into()
            }),
            ("fallback_too_long", "coordinator", |o| {
                let w = o.coordinator.as_mut().unwrap();
                w.mode = Some("fallback".into());
                w.mode_age_ms = Some(TEMPO);
            }),
            ("watermark_stalled", "eval_worker", |o| {
                o.eval_worker.as_mut().unwrap().confirmed_age_ms = None
            }),
            ("webhook_paused", "coordinator", |o| {
                o.coordinator.as_mut().unwrap().subscription = Some("presumed_paused".into())
            }),
            ("phase_overrun", "testnet-c7", |o| {
                o.coordinator.as_mut().unwrap().signing = None;
                let e = o.eval.as_mut().unwrap();
                e.cohorts[1].phase = "scoring".into();
                e.epoch = Some(23_169 + 18);
            }),
            ("scorer_budget_fault", "testnet-c7", |o| {
                o.eval.as_mut().unwrap().cohorts[1].fault = Some("scorer_unavailable".into())
            }),
            ("cohort_fault", "testnet-c7", |o| {
                o.eval.as_mut().unwrap().cohorts[1].fault = Some("draw_shortfall".into())
            }),
            ("ledger_threshold", "ledger", |o| {
                o.eval.as_mut().unwrap().ledger_over_threshold = Some(true)
            }),
            ("ledger_threshold_unset", "ledger", |o| {
                o.eval.as_mut().unwrap().ledger_threshold_set = Some(false)
            }),
            ("commit_gap", "coordinator", |o| {
                o.eval.as_mut().unwrap().epoch = Some(23_169 + 2)
            }),
            ("retention_stalled", "receipts", |o| {
                o.eval.as_mut().unwrap().retention_stalled = Some(vec!["cohort 1".into()])
            }),
        ];
        for (rule, subject, edit) in firing {
            let mut bad = healthy();
            edit(&mut bad);
            let mut st = AlertState::default();
            let t = 1_000_000;
            let c = conditions(&bad, &p, &r, &st, t);
            observe(&mut st, &c, t);
            let ev = due(&st, t, r.renotify_ms);
            acknowledge(&mut st, &ev, t);
            assert!(st.get(rule, subject).is_some(), "{rule} {subject} fires");
            // Every way its read can be unknown keeps it open.
            let unknown: Vec<Observed> = {
                let mut v = Vec::new();
                let mut o = bad.clone();
                match rule {
                    "durable_fault" | "fallback_too_long" | "watermark_stalled"
                    | "webhook_paused" => {
                        let name = subject.trim_end_matches(".signing");
                        if name == "coordinator" {
                            o.coordinator = None;
                        } else {
                            o.eval_worker = None;
                        }
                        v.push(o);
                        fn view<'a>(o: &'a mut Observed, name: &str) -> &'a mut WorkerView {
                            if name == "coordinator" {
                                o.coordinator.as_mut().unwrap()
                            } else {
                                o.eval_worker.as_mut().unwrap()
                            }
                        }
                        if subject.ends_with(".signing") {
                            let mut o = bad.clone();
                            view(&mut o, name).signing = None;
                            v.push(o);
                        } else if rule == "durable_fault" {
                            // A status without its journal block (the
                            // coordinator answers `chain_journal: null` when
                            // its journal object does not answer).
                            let mut o = bad.clone();
                            *view(&mut o, name) = parse_worker(&json!({"chain_journal": null}));
                            v.push(o);
                            let mut o = bad.clone();
                            *view(&mut o, name) = parse_worker(&json!({}));
                            v.push(o);
                        } else if rule == "fallback_too_long" {
                            let mut o = bad.clone();
                            view(&mut o, name).mode = None;
                            v.push(o);
                            let mut o = bad.clone();
                            view(&mut o, name).mode_age_ms = None;
                            v.push(o);
                        } else if rule == "webhook_paused" {
                            let mut o = bad.clone();
                            view(&mut o, name).subscription = None;
                            v.push(o);
                        }
                    }
                    "phase_overrun" => {
                        o.eval.as_mut().unwrap().epoch = None;
                        v.push(o);
                        let mut o = bad.clone();
                        o.eval = None;
                        v.push(o);
                        // A partial read of its cohort decides nothing either.
                        let mut o = bad.clone();
                        o.eval.as_mut().unwrap().cohorts[1].opened_epoch = None;
                        v.push(o);
                        let mut o = bad.clone();
                        o.eval.as_mut().unwrap().cohorts[1].k_tempos = None;
                        v.push(o);
                        let mut o = bad.clone();
                        o.eval.as_mut().unwrap().cohorts_missing = true;
                        v.push(o);
                    }
                    "scorer_budget_fault" | "cohort_fault" => {
                        o.eval = None;
                        v.push(o);
                        // An answered status without its cohort list, or
                        // with the cohort's phase missing (dropped).
                        let mut o = bad.clone();
                        let mut j = eval_status("admission");
                        j.as_object_mut().unwrap().remove("cohorts");
                        o.eval = Some(parse_eval(&j));
                        v.push(o);
                        let mut o = bad.clone();
                        let mut j = eval_status("admission");
                        j["cohorts"][1].as_object_mut().unwrap().remove("phase");
                        j["cohorts"][1]["fault"] = json!("draw_shortfall");
                        o.eval = Some(parse_eval(&j));
                        v.push(o);
                    }
                    "ledger_threshold" => {
                        o.eval.as_mut().unwrap().ledger_over_threshold = None;
                        v.push(o);
                        let mut o = bad.clone();
                        o.eval = None;
                        v.push(o);
                    }
                    "retention_stalled" => {
                        // A coordinator built before the rule answers
                        // without the list.
                        o.eval.as_mut().unwrap().retention_stalled = None;
                        v.push(o);
                        let mut o = bad.clone();
                        o.eval = None;
                        v.push(o);
                    }
                    "ledger_threshold_unset" => {
                        o.eval.as_mut().unwrap().ledger_threshold_set = None;
                        v.push(o);
                        let mut o = bad.clone();
                        o.eval = None;
                        v.push(o);
                    }
                    _ => {
                        o.coordinator = None;
                        v.push(o);
                        let mut o = bad.clone();
                        o.eval = None;
                        v.push(o);
                        let mut o = bad.clone();
                        o.coordinator.as_mut().unwrap().signing = None;
                        v.push(o);
                    }
                }
                v
            };
            for (n, o) in unknown.iter().enumerate() {
                assert!(!decided(rule, subject, o), "{rule} {subject} #{n}");
                let mut s2 = st.clone();
                let c = conditions(o, &p, &r, &s2, t + 60_000);
                observe(&mut s2, &c, t + 60_000);
                let e = s2.get(rule, subject).unwrap();
                assert!(
                    !e.resolved,
                    "{rule} {subject} #{n}: unknown is not a resolution"
                );
                assert!(
                    !due(&s2, t + 60_000, r.renotify_ms)
                        .iter()
                        .any(|ev| ev.rule == rule),
                    "{rule} {subject} #{n}"
                );
            }
            // An answered read that shows it clear resolves it, even while
            // an unrelated source is unknown.
            let mut clear = healthy();
            let other_missing = match rule {
                "durable_fault" | "fallback_too_long" | "watermark_stalled" | "webhook_paused"
                    if subject.starts_with("coordinator") =>
                {
                    clear.eval_worker = None;
                    true
                }
                "durable_fault" | "fallback_too_long" | "watermark_stalled" | "webhook_paused" => {
                    clear.coordinator = None;
                    true
                }
                _ => false,
            };
            assert!(decided(rule, subject, &clear), "{rule} {subject}");
            let mut s3 = st.clone();
            let c = conditions(&clear, &p, &r, &s3, t + 60_000);
            observe(&mut s3, &c, t + 60_000);
            let ev = due(&s3, t + 60_000, r.renotify_ms);
            assert!(
                ev.iter()
                    .any(|e| e.rule == rule && e.state == EventKind::Resolved),
                "{rule} {subject} resolves (other source missing: {other_missing})"
            );
            // A resolving entry stays resolving through an unknown read.
            let mut o = unknown[0].clone();
            if other_missing {
                o.coordinator = None;
                o.eval_worker = None;
            }
            let c = conditions(&o, &p, &r, &s3, t + 120_000);
            observe(&mut s3, &c, t + 120_000);
            assert!(
                s3.get(rule, subject).is_some_and(|e| e.resolved),
                "{rule} {subject} stays resolving"
            );
        }
        // The publisher's own rules are not status reads: never carried.
        let mut st = AlertState::default();
        let warned = PublisherObs {
            warnings: vec!["w".into()],
            ..PublisherObs::default()
        };
        let mut silent = healthy();
        silent.coordinator = None;
        let c = conditions(&healthy(), &warned, &r, &st, 1);
        observe(&mut st, &c, 1);
        let ev = due(&st, 1, r.renotify_ms);
        acknowledge(&mut st, &ev, 1);
        let c = conditions(&silent, &p, &r, &st, 2);
        observe(&mut st, &c, 2);
        assert!(st.get("config_warning", "publisher").unwrap().resolved);
    }

    /// A source that answers without a field an open entry needs: the entry
    /// is carried, and `status_incomplete <source>` fires after the same hold
    /// as `status_unreachable`, once, naming the fields; both resolve when
    /// the field is back and shows the condition clear. A missing field with
    /// nothing open raises nothing.
    #[test]
    fn missing_fields_are_unknown_and_alert_after_the_hold() {
        let t = 1_793_644_320_000;
        let r = rules();
        let runs = r.unreachable_hold_ms / CRON_PERIOD_MS + 3;
        type Edit = fn(&mut Observed);
        let cases: [(&str, &str, Edit, Edit, &str); 8] = [
            (
                "durable_fault",
                "coordinator",
                |o| o.coordinator = Some(parse_worker(&worker_status("primary", true))),
                |o| o.coordinator = Some(parse_worker(&json!({"chain_journal": null}))),
                "coordinator",
            ),
            (
                "fallback_too_long",
                "eval_worker",
                |o| {
                    let w = o.eval_worker.as_mut().unwrap();
                    w.mode = Some("fallback".into());
                    w.mode_age_ms = Some(TEMPO);
                },
                |o| o.eval_worker.as_mut().unwrap().mode = None,
                "eval_worker",
            ),
            (
                "cohort_fault",
                "testnet-c7",
                |o| o.eval.as_mut().unwrap().cohorts[1].fault = Some("draw_shortfall".into()),
                |o| {
                    let e = o.eval.as_mut().unwrap();
                    e.cohorts.clear();
                    e.cohorts_missing = true;
                },
                "eval",
            ),
            (
                "commit_gap",
                "coordinator",
                |o| o.eval.as_mut().unwrap().epoch = Some(23_169 + 2),
                |o| o.coordinator.as_mut().unwrap().signing = None,
                "coordinator",
            ),
            (
                "ledger_threshold",
                "ledger",
                |o| o.eval.as_mut().unwrap().ledger_over_threshold = Some(true),
                |o| o.eval.as_mut().unwrap().ledger_over_threshold = None,
                "eval",
            ),
            (
                "retention_stalled",
                "receipts",
                |o| o.eval.as_mut().unwrap().retention_stalled = Some(vec!["cohort 1".into()]),
                |o| o.eval.as_mut().unwrap().retention_stalled = None,
                "eval",
            ),
            (
                "phase_overrun",
                "testnet-c7",
                |o| {
                    let e = o.eval.as_mut().unwrap();
                    e.cohorts[1].phase = "scoring".into();
                    e.epoch = Some(23_169 + 18);
                },
                |o| o.eval.as_mut().unwrap().cohorts[1].k_tempos = None,
                "eval",
            ),
            (
                "commit_gap",
                "coordinator",
                |o| o.eval.as_mut().unwrap().epoch = Some(23_169 + 2),
                |o| o.eval.as_mut().unwrap().epoch = None,
                "eval",
            ),
        ];
        for (rule, subject, fire, strip, source) in cases {
            let mut st = AlertState::default();
            let mut bad = healthy();
            fire(&mut bad);
            let first = run(&mut st, &bad, t);
            assert!(first.iter().any(|e| e.rule == rule), "{rule} fires");
            let mut partial = bad.clone();
            strip(&mut partial);
            let mut posts = Vec::new();
            let mut first_post = None;
            for i in 1..=runs {
                let ev = run(&mut st, &partial, t + i * CRON_PERIOD_MS);
                if ev.iter().any(|e| e.rule == "status_incomplete") && first_post.is_none() {
                    first_post = Some(i);
                }
                posts.extend(ev);
            }
            // A missing journal block also leaves the watermark absent, which
            // `watermark_stalled` fires on by definition (absent = stalled).
            if rule == "durable_fault" {
                assert!(
                    kinds(&posts).contains(&(EventKind::Firing, "watermark_stalled", subject)),
                    "an absent watermark fires"
                );
                posts.retain(|e| e.rule != "watermark_stalled");
            }
            assert_eq!(
                kinds(&posts),
                vec![(EventKind::Firing, "status_incomplete", source)],
                "{rule}: carried, and the missing field alerts once"
            );
            assert_eq!(
                first_post,
                Some(1 + r.unreachable_hold_ms / CRON_PERIOD_MS),
                "{rule}: only after the hold"
            );
            assert_eq!(posts[0].since_ms, t + CRON_PERIOD_MS, "{rule}");
            assert!(
                posts[0].detail.contains(&format!("({rule} {subject})")),
                "{}",
                posts[0].detail
            );
            assert!(!st.get(rule, subject).unwrap().resolved);
            // The field is back and shows the condition clear: both resolve.
            let back = run(&mut st, &healthy(), t + (runs + 1) * CRON_PERIOD_MS);
            // (Other rules the edit raised, such as a commit gap beside an
            // overrun, resolve too; only these two are checked.)
            let mut k: Vec<_> = kinds(&back)
                .into_iter()
                .filter(|(_, r, _)| *r == rule || *r == "status_incomplete")
                .collect();
            k.sort();
            let mut want = vec![
                (EventKind::Resolved, rule, subject),
                (EventKind::Resolved, "status_incomplete", source),
            ];
            want.sort();
            assert_eq!(k, want, "{rule}");
        }
        // Nothing open: a missing field raises nothing (the rule cannot fire
        // until the field is back).
        let mut st = AlertState::default();
        let mut partial = healthy();
        partial.coordinator.as_mut().unwrap().signing = None;
        partial.eval.as_mut().unwrap().ledger_over_threshold = None;
        for i in 0..runs {
            assert!(run(&mut st, &partial, t + i * CRON_PERIOD_MS).is_empty());
        }
        // A carried status_incomplete stays open while its source does not
        // answer at all (status_unreachable covers that read).
        let mut st = AlertState::default();
        let mut bad = healthy();
        bad.eval.as_mut().unwrap().ledger_over_threshold = Some(true);
        run(&mut st, &bad, t);
        let mut partial = bad.clone();
        partial.eval.as_mut().unwrap().ledger_over_threshold = None;
        for i in 1..=runs {
            run(&mut st, &partial, t + i * CRON_PERIOD_MS);
        }
        let mut silent = healthy();
        silent.eval = None;
        run(&mut st, &silent, t + (runs + 1) * CRON_PERIOD_MS);
        assert!(!st.get("status_incomplete", "eval").unwrap().resolved);
        assert!(!st.get("ledger_threshold", "ledger").unwrap().resolved);
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

    /// The pointer writer: a failed last pass, or a confirmation older than
    /// two cron periods, holds `pointer_stale` for a tempo before it fires;
    /// a coordinator without the fields (older build) or without an
    /// authorization raises nothing, and the field is needed only then.
    #[test]
    fn a_stale_authorization_pointer_alerts_after_a_tempo() {
        let r = rules();
        let p = PublisherObs::default();
        let with = |pointer: Option<crate::status::PointerView>, authorization: bool| {
            let mut o = healthy();
            let s = o.coordinator.as_mut().unwrap().signing.as_mut().unwrap();
            s.authorization = authorization;
            s.pointer = pointer;
            o
        };
        let fresh = crate::status::PointerView {
            current: true,
            confirmed_age_ms: Some(60_000),
            error: None,
        };
        assert!(evaluate(&with(Some(fresh.clone()), true), &p, &r).is_empty());
        assert!(evaluate(&with(None, false), &p, &r).is_empty());
        assert!(evaluate(&with(None, true), &p, &r).is_empty());
        for stale in [
            crate::status::PointerView {
                current: false,
                error: Some("object_put".into()),
                ..fresh.clone()
            },
            crate::status::PointerView {
                confirmed_age_ms: Some(r.unreachable_hold_ms + 1),
                ..fresh.clone()
            },
            crate::status::PointerView {
                current: false,
                confirmed_age_ms: None,
                error: None,
            },
        ] {
            let c = evaluate(&with(Some(stale), true), &p, &r);
            assert_eq!(rules_of(&c), vec!["pointer_stale"]);
            assert_eq!(c[0].hold_ms, TEMPO);
        }
        assert!(decided(
            "pointer_stale",
            "coordinator",
            &with(Some(fresh), true)
        ));
        assert!(!decided("pointer_stale", "coordinator", &with(None, true)));
        assert!(decided("pointer_stale", "coordinator", &with(None, false)));
        // Revocation entries that do not open are one alert.
        let unopened = PublisherObs {
            revocations_unopened: 2,
            ..PublisherObs::default()
        };
        let c = evaluate(&healthy(), &unopened, &r);
        assert_eq!(rules_of(&c), vec!["revocation_unverifiable"]);
        assert!(c[0].detail.starts_with("2 revocation set entries"));
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
