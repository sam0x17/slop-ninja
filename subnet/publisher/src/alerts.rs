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
//! | `scorer_budget_fault` | a cohort not yet closed (admitting, closing or evaluating) is paused by a scorer or budget fault, or has passed a budget guard (`budget_alert`: tracking only, the cohort keeps running) |
//! | `attempts_extended` | a cohort not yet closed has a chunk whose infrastructure failures (lost leases, crashes, failed requests) used the plan's `max_attempts` and got another round (`attempt_extensions`); nothing is paused |
//! | `epoch_deferred` | the epoch index at the eval status's confirmed watermark is two or more past its stored epoch: increments have waited for a head-epoch reading through a whole tempo of watermark progress (failed or refused head reads), so the cohort schedule is not advancing |
//! | `inclusion_unknown` | a recent coordinator commit operation settled `inclusion_unverifiable`: its nonce was consumed but the inclusion could not be read, so whether that epoch's commit landed is unknown (the signer keeps running; one alert per operation) |
//! | `operator_clear_needed` | a cohort the other fault rules report is paused by a fault the coordinator says does not clear by itself (`operator_clear_needed`: not a probed outage in a work phase, nor an admission outage whose subjects it re-reads; for example a store-caused or definitive scorer pause, `unresolved_infrastructure` with jobs at the receipt bound, or a terminal fault): it stays paused until an operator clears or abandons it. The detail names the fault, its cause and any jobs at the receipt bound |
//! | `cohort_fault` | a cohort not yet closed is paused by any other fault (for example `framing_unavailable` while admission waits on a framing step, or `step_refused` when a root step fails in a way no wait changes), or a closed cohort's closure step is paused; or such a cohort's root step has kept failing on a retried (transient) code through a whole tempo of chain progress (its stored epoch two past the one it first failed in), which pauses nothing |
//! | `ledger_threshold` | the spend ledger's billing month (settled spend plus open reservations) is past the deployment's private alert threshold; tracking only, nothing is refused |
//! | `retention_stalled` | the eval status reports a due cohort whose detailed receipts remain after a further completion, or a predecessor authority not yet finished after one (judged by completions, never a clock): deletion is not keeping up with the N+2 rule (a failing step or unreadable authorization pointer, named by its last error); or (subject `public_receipts`) the publisher's own N+2 reading of the public record disagrees with the coordinator's retirement markers, which alone delete public receipts: a marker whose due point the record reached while the public reading still holds the cohort, or a cohort the public reading found due with no marker after two further completions |
//! | `ledger_threshold_unset` | the eval status's ledger standing names no alert threshold (the deploy did not pass `SPEND_ALERT_MICROUSD`), so `ledger_threshold` cannot fire |
//! | `commit_gap` | the coordinator has not committed weights for two or more tempos |
//! | `commit_stalled` | the coordinator holds an authorization and its signer is active, yet two whole epochs passed without a weight commit: the confirmed epoch is three or more past its last committed epoch (or its authorization's issue epoch, when later). Each epoch's commit window, from the commit offset after its boundary to before the next boundary, lies inside the epoch, so the current epoch never counts |
//! | `signing_backing_off` | for a whole tempo of observation, the active signer's last error has been newer than its last recorded progress (its newest sign-log row): it keeps failing and backing off (health `backing_off`, or `failing`) without committing, revealing or reading back anything |
//! | `pointer_stale` | the coordinator's authorization-pointer writer has not confirmed the private pointer (its last pass failed, or its last confirmation is older than two cron periods) for a whole tempo: re-authorizations and revocations are not reaching validators |
//! | `auto_target_stalled` | the coordinator's automatic target handoff has reported the same failure for a whole tempo (a closed cohort it cannot install and authorize yet, such as a refused bundle, retired or missing receipts, an earlier cohort that never closed, a cohort-number gap, a manifest this build cannot decode, a pass that cannot advance within its deadline, or no main record configured; the cohorts after it wait and none is skipped), or its authorization key's check has failed for a tempo (it holds no cohort until one needs a signature) |
//! | `row_record_stalled` | for a whole tempo, no signed coordinator row covers the installed authorization (waiting on its first derived row, its issue epoch, or a signing failure: validators keep their installed row and a fresh validator waits), or signing a new row keeps failing while the old one still covers it |
//! | `coordinator_reveal_mismatch` | a reveal of the coordinator's weights did not match its committed row |
//! | `publication_refused` | a closed cohort, an artifact's admission record or copy, or the authorization failed verification or the memory budget |
//! | `publication_pending` | a closed cohort, an artifact copy or the authorization has been incomplete for a whole tempo |
//! | `publication_unverifiable` | closed cohorts whose manifest does not open under the compiled trust roots (one alert for all) |
//! | `revocation_unverifiable` | entries of the copied revocation set that do not open under the compiled trust roots (copied unchanged; validators skip them) |
//! | `config_refused` | the publisher's configuration was refused; only the status fault snapshot is written |
//! | `config_warning` | the configuration was accepted with a setting ignored (an `ALERT_SINK_URL` while `ALERT_RELAY` is on) |
//! | `status_access_warning` | a Worker's status gate (`access`) reports two of its status tokens equal (a consumer holding another's routes, or the operator's), or, for a whole tempo, the shared pre-split status token still installed beside both consumer tokens (the split's last step not done) |
//! | `run_interrupted` | every run for a whole tempo stopped before it finished (it never released its lease) |
//! | `live_fault` | at the `live` roster level, the eval Worker reports a live fault for the running cohort (`prepare`: its live setup failed, so it runs without readings; `release`: a release failed, so its live values stopped); scoring, packaging and phases are not held |
//! | `live_over_bound` | the eval Worker's private recount of a cohort's counted bits, with a superseded pre-finalized value, passed the bound: it set the global live stop until the operator resumes |
//! | `prefinal_mismatch` | a pre-finalized value shown differs from the package's (the eval Worker's defect, subject the cohort and hotkey), or from the published result at close (the publisher's own close check); values stay private |
//! | `live_value_changed` | a live value would have replaced one already published: the roster was written at `sessions` without its live block |
//! | `live_dropped` | a roster with its live block failed the dashboard check: it was written at `sessions` without it, keeping the counts |
//!
//! Not covered here (AR-13 gaps, listed in the README): commits by our other
//! validator (validator2's journal is not visible to the publisher), and a
//! divergence between the chain's consensus for the subnet and our row, which
//! needs chain reads.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::SinkFormat;
use crate::status::{CohortView, EvalView, Observed, SigningView, WorkerView};

pub use slop_ninja_alerts_wire::{
    AlertState, Condition, Entry, Event, EventKind, acknowledge, attempt, due, event_id,
};

/// v17: the live-value rules (`live_fault`, `live_over_bound`,
/// `prefinal_mismatch`, `live_value_changed`, `live_dropped`), decided by
/// the publisher's own pass at the `live` roster level; v16: `commit_stalled` and `signing_backing_off` (the coordinator's
/// signer stalled 14 hours from epoch 137 with health `backing_off`, and
/// only `commit_gap` told); v15: `row_record_stalled`; v14: `operator_clear_needed` for a paused cohort whose fault the
/// coordinator says does not clear by itself, and the fault rules' detail
/// names the fault's cause and any jobs at the receipt bound; the retired
/// `monthly_cap` fault code is dropped from [`SCORER_BUDGET_FAULTS`]
/// (nothing raises it; budgets only alert); v13:
/// `retention_stalled` for `public_receipts`, the publisher's own
/// cross-check of the coordinator's retirement markers, which now alone
/// delete public detailed receipts; a pass that did not finish its
/// retention step keeps the previous reading ([`retention_after_pass`])
/// (v12: `status_access_warning`; v11: `scorer_budget_fault` also fires
/// for a cohort past a budget guard that pauses nothing (`budget_alert`),
/// and `attempts_extended`, `epoch_deferred` and `inclusion_unknown`; v10:
/// `cohort_fault` also fires for a root step that keeps failing on a
/// retried code through a whole tempo of chain progress; v9:
/// `auto_target_stalled`; v8: `phase_overrun` also covers a cohort still in `admission` at its
/// package deadline with its window closed (its roster freeze waiting), and
/// counts a cohort's age in the coordinator's credited tempos
/// (`observed.active_tempos - opened_active`) rather than epochs, so an
/// outage that extends a cohort's schedule is not an overrun;
/// v7: `cohort_fault` also reads closed cohorts (a paused closure step) and
/// the eval Worker's `step_refused`; v6: `pointer_stale` and
/// `revocation_unverifiable`; v5: `webhook_paused`
/// and `retention_stalled`; v4: an unknown status read, unanswered or answered
/// without a field an open entry needs, carries the open entries it would
/// decide, and alerts after its hold, `status_unreachable` and
/// `status_incomplete`; v3: `config_warning`, and resolutions of attempted
/// firings).
pub const RULES_ID: &str = "slop_ninja.publisher.alerts/v17";
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
/// A cohort's failing root step alerts (`cohort_fault`) once the stored
/// epoch is this many past the one it first failed in: it has failed
/// through at least one whole tempo of chain progress.
pub const STEP_ERROR_EPOCHS: u64 = 2;

/// `epoch_deferred` fires once the confirmed watermark's epoch is this many
/// past the stored epoch: a whole tempo of watermark progress without the
/// stored epoch advancing (judged by chain blocks, never wall time).
pub const DEFERRED_EPOCHS: u64 = 2;

/// The coordinator's detail for a commit operation whose inclusion could
/// not be read (`inclusion_unknown`).
pub const INCLUSION_UNVERIFIABLE: &str = "inclusion_unverifiable";

/// Cohort faults that are scorer or budget faults (AR-13).
pub const SCORER_BUDGET_FAULTS: &[&str] = &[
    "scorer_unavailable",
    "scoring_budget",
    "generation_budget",
    "unresolved_infrastructure",
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
    /// `commit_stalled`: epochs whose commit window closed without a commit
    /// while the coordinator was authorized and active. An epoch's window
    /// opens the coordinator's commit offset (`SIGN_COMMIT_OFFSET`) after
    /// its boundary and closes before the next one (its mortal era and
    /// boundary guard), so it lies inside the epoch: an epoch is missed once
    /// the confirmed epoch index is past it, and the current epoch never
    /// counts. Two missed epochs: the confirmed epoch is three past the
    /// last committed (or authorized) one.
    pub commit_stall_epochs: u64,
    /// `signing_backing_off`: one tempo, the cadence of the signer's own
    /// progress (each epoch logs a commit, then a reveal).
    pub signing_stall_ms: u64,
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
            commit_stall_epochs: 2,
            signing_stall_ms: tempo_ms,
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
    /// Disagreements between the coordinator's receipt retirement markers
    /// and the publisher's own N+2 reading (`publish::Report`), one
    /// content-free line each.
    #[serde(default)]
    pub retention_mismatch: Vec<String>,
    /// The live-value alerts of the last dashboard pass, as (rule,
    /// subject): rule names and subjects (cohort, hotkey) only.
    #[serde(default)]
    pub live: Vec<(String, String)>,
}

/// The live-value rules, decided by the publisher's own pass.
pub const LIVE_RULES: &[&str] = &["live_fault", "live_over_bound", "prefinal_mismatch", "live_value_changed", "live_dropped"];

/// The `retention_stalled` subject of the publisher's own cross-check
/// (decided by its own pass, never by a status read).
pub const PUBLIC_RECEIPTS: &str = "public_receipts";

/// The cross-check lines the next alert phase reads: this pass's when its
/// retention step ran to its end (`this_pass`, from
/// `publish::Report::retention_checked`), otherwise the previous pass's, so
/// a pass that stopped earlier (a store error before or during retention)
/// neither resolves an open disagreement nor raises it again on the next
/// full pass.
pub fn retention_after_pass(previous: &[String], this_pass: Option<&[String]>) -> Vec<String> {
    this_pass.unwrap_or(previous).to_vec()
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
    if let Some(a) = &w.access {
        if !a.tokens_distinct {
            out.push(cond(
                "status_access_warning",
                name,
                "two of the Worker's status tokens are equal: one consumer can read another's \
                 routes; give each its own value"
                    .into(),
                0,
            ));
        } else if a.shared_status_token && a.publisher_token && a.alerts_token {
            out.push(cond(
                "status_access_warning",
                name,
                "the shared pre-split status token is still installed beside both consumer \
                 tokens: delete it once the publisher and the relay present their own"
                    .into(),
                r.publication_hold_ms,
            ));
        }
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
/// The signer's state for an alert detail: its health, backoff and last
/// error code.
fn signing_state(s: &SigningView) -> String {
    format!(
        "signing health {}, backoff {} s, last error {}",
        s.health.as_deref().unwrap_or("unknown"),
        s.backoff_ms.unwrap_or(0) / 1000,
        s.last_error.as_deref().unwrap_or("none"),
    )
}

/// `commit_stalled`: the coordinator holds an authorization and its signer
/// is active, yet [`Rules::commit_stall_epochs`] whole epochs passed since
/// its last committed epoch `last` (or, when later or with no commit among
/// the recent operations, the authorization's issue epoch) without a
/// commit, at the confirmed epoch `now`. Unlike `commit_gap`, which fires
/// one missed epoch in and also while unauthorized or paused, this is the
/// stall of the signer itself (2026-11-05/06: 14 hours from epoch 137).
fn commit_stalled(s: &SigningView, last: Option<u64>, now: u64, r: &Rules) -> Option<Condition> {
    if !s.authorization || s.state != "active" {
        return None;
    }
    let base = match (last, s.issued_epoch) {
        (Some(l), Some(i)) => l.max(i),
        (l, i) => l.or(i)?,
    };
    let missed = now.saturating_sub(base.saturating_add(1));
    if missed < r.commit_stall_epochs {
        return None;
    }
    let since = match last {
        Some(l) => format!("last committed epoch {l}"),
        None => "no commit among recent operations".into(),
    };
    Some(cond(
        "commit_stalled",
        "coordinator",
        format!(
            "authorized and active, but no weight commit for epochs {}..={} ({since}, now {now}); {}",
            base + 1,
            now - 1,
            signing_state(s)
        ),
        0,
    ))
}

/// `signing_backing_off`: the active signer's last error is newer than its
/// last recorded progress (its newest sign-log row), so it has been failing
/// and backing off since then; held for [`Rules::signing_stall_ms`] of
/// observation. Its health alternates between `backing_off` and `ok`
/// around each retry, so the error's recency against the log, which only
/// progress moves, is the condition rather than one health reading. A
/// signer with no log rows is read by its health alone.
fn signing_backing_off(s: &SigningView, r: &Rules) -> Option<Condition> {
    if s.state != "active" {
        return None;
    }
    let failing = match (s.last_error_age_ms, s.last_event_age_ms) {
        (Some(err), Some(event)) => s.last_error.is_some() && err < event,
        _ => matches!(s.health.as_deref(), Some("backing_off" | "failing")),
    };
    if !failing {
        return None;
    }
    let progress = s.last_event_age_ms.map_or_else(
        || "no recorded signing progress".into(),
        |a| format!("no recorded signing progress for {} s", a / 1000),
    );
    Some(cond(
        "signing_backing_off",
        "coordinator",
        format!("signer failing and backing off: {progress}; {}", signing_state(s)),
        r.signing_stall_ms,
    ))
}

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
                let mut detail = format!("fault {code} in {}", c.phase);
                if let Some(cause) = &c.fault_cause {
                    detail.push_str(&format!(" (cause {cause})"));
                }
                if !c.receipt_bound_reached_jobs.is_empty() {
                    detail.push_str(&format!(
                        "; jobs at the receipt bound: {:?}",
                        c.receipt_bound_reached_jobs
                    ));
                }
                out.push(cond(rule, &c.label, detail.clone(), 0));
                // The coordinator says this pause does not clear by itself:
                // an operator clear or abandonment is needed, unlike a
                // probed outage that resumes once the platform answers.
                if c.operator_clear_needed == Some(true) {
                    out.push(cond(
                        "operator_clear_needed",
                        &c.label,
                        format!("{detail}: no automatic clear; /eval/clear or /eval/abandon"),
                        0,
                    ));
                }
            } else if let Some(code) = c.budget_alert.as_ref().filter(|_| c.active()) {
                out.push(cond(
                    "scorer_budget_fault",
                    &c.label,
                    format!(
                        "budget guard {code} passed in {} (tracking only, nothing paused)",
                        c.phase
                    ),
                    0,
                ));
            }
            if let Some(n) = c.attempt_extensions.filter(|n| *n > 0 && c.active()) {
                out.push(cond(
                    "attempts_extended",
                    &c.label,
                    format!(
                        "{n} chunk attempt extensions past max_attempts in {} (infrastructure failures; nothing paused)",
                        c.phase
                    ),
                    0,
                ));
            }
            if c.fault.is_none()
                && let Some(err) = &c.step_error
                && let (Some(since), Some(now)) = (err.since_epoch, e.epoch)
                && now >= since.saturating_add(STEP_ERROR_EPOCHS)
            {
                // A root step retried on a transient code that has kept
                // failing for a whole tempo of chain progress: nothing is
                // paused, but no wait has fixed it (a refused write, a
                // binding that is not deployed).
                out.push(cond(
                    "cohort_fault",
                    &c.label,
                    format!(
                        "step error {} in {} since epoch {since} ({} failures), still retried",
                        err.code,
                        c.phase,
                        err.count.unwrap_or(0)
                    ),
                    0,
                ));
            }
        }
        if let (Some(w), Some(stored)) = (e.watermark_epoch, e.epoch)
            && w >= stored.saturating_add(DEFERRED_EPOCHS)
        {
            out.push(cond(
                "epoch_deferred",
                "eval",
                format!("confirmed watermark in epoch {w}, stored epoch {stored}: increments wait for a head-epoch reading"),
                0,
            ));
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
        if let Some(c) = commit_stalled(signing, last, now, r) {
            out.push(c);
        }
        for op in signing
            .ops
            .iter()
            .filter(|op| op.detail.as_deref() == Some(INCLUSION_UNVERIFIABLE))
        {
            out.push(cond(
                "inclusion_unknown",
                &format!("coordinator.epoch-{}", op.epoch),
                format!(
                    "commit at epoch {} settled with its inclusion unknown (nonce consumed, inclusion unreadable); the signer keeps running",
                    op.epoch
                ),
                0,
            ));
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
    if let Some(c) = o
        .coordinator
        .as_ref()
        .and_then(|w| w.signing.as_ref())
        .and_then(|s| signing_backing_off(s, r))
    {
        out.push(c);
    }
    if let Some(stall) = o
        .coordinator
        .as_ref()
        .and_then(|w| w.signing.as_ref())
        .and_then(|s| s.row_record.as_ref())
    {
        // Its own age counts toward the hold, as for the target handoff.
        let what = if stall.uncovered {
            "no signed coordinator row covers the installed authorization"
        } else {
            "signing a new coordinator row keeps failing"
        };
        out.push(cond(
            "row_record_stalled",
            "coordinator",
            format!("{what} for {} s: {}", stall.age_ms / 1000, stall.code),
            r.publication_hold_ms.saturating_sub(stall.age_ms),
        ));
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
    if let Some(e) = o
        .coordinator
        .as_ref()
        .and_then(|w| w.signing.as_ref())
        .and_then(|s| s.auto_target_error.as_ref())
    {
        // The failure's own age counts toward the hold, so a failure that
        // started before this publisher saw it is not held again.
        out.push(cond(
            "auto_target_stalled",
            "coordinator",
            format!(
                "automatic target handoff failing for {} s: {}{}",
                e.age_ms / 1000,
                e.code,
                e.label
                    .as_deref()
                    .map(|l| format!(" ({l})"))
                    .unwrap_or_default()
            ),
            r.publication_hold_ms.saturating_sub(e.age_ms),
        ));
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
    for (rule, subject) in &p.live {
        if let Some(rule) = LIVE_RULES.iter().copied().find(|r| r == rule) {
            out.push(cond(rule, subject, "live values (see the eval Worker's /eval/live)".into(), 0));
        }
    }
    if !p.retention_mismatch.is_empty() {
        out.push(cond(
            "retention_stalled",
            PUBLIC_RECEIPTS,
            p.retention_mismatch.join("; "),
            0,
        ));
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
    "attempts_extended",
    "epoch_deferred",
    "inclusion_unknown",
    "cohort_fault",
    "operator_clear_needed",
    "ledger_threshold",
    "retention_stalled",
    "ledger_threshold_unset",
    "commit_gap",
    "commit_stalled",
    "signing_backing_off",
    "coordinator_reveal_mismatch",
    "pointer_stale",
    "auto_target_stalled",
    "status_access_warning",
    "row_record_stalled",
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
/// The status gate's report `status_access_warning` reads.
const ACCESS: WorkerField = ("access", |w| w.access.is_some());
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
/// in fallback), `webhook_paused` its subscription state,
/// `status_access_warning` its `access` report, `watermark_stalled` its answer (a missing watermark fires);
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
        "status_access_warning" => need_worker(o, subject, &[ACCESS], &mut n),
        "watermark_stalled" => need_worker(o, subject, &[], &mut n),
        "scorer_budget_fault" | "cohort_fault" | "attempts_extended" | "operator_clear_needed" => {
            need_eval(o, Some(COHORTS), &mut n)
        }
        "epoch_deferred" => need_eval(
            o,
            Some(("watermark_epoch", |e| {
                e.watermark_epoch.is_some() && e.epoch.is_some()
            })),
            &mut n,
        ),
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
        "retention_stalled" if subject == PUBLIC_RECEIPTS => {}
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
        "commit_gap" | "commit_stalled" | "coordinator_reveal_mismatch" | "inclusion_unknown" => {
            need_worker(o, "coordinator", &[SIGNING], &mut n);
            need_eval(o, Some(("epoch", |e| e.epoch.is_some())), &mut n);
        }
        "pointer_stale" => need_worker(o, "coordinator", &[SIGNING, POINTER], &mut n),
        "auto_target_stalled" | "row_record_stalled" | "signing_backing_off" => {
            need_worker(o, "coordinator", &[SIGNING], &mut n)
        }
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
            live: None,
        }
    }

    fn rules_of(c: &[Condition]) -> Vec<&'static str> {
        c.iter().map(|c| c.rule).collect()
    }

    /// v14: a paused cohort whose fault the coordinator says does not
    /// clear by itself also raises `operator_clear_needed`, its detail
    /// naming the cause and any jobs at the receipt bound; a self-clearing
    /// pause (a probed outage) raises only its fault rule, and so does a
    /// coordinator that does not report the field.
    #[test]
    fn a_pause_that_needs_an_operator_is_told_apart_from_a_self_clearing_one() {
        let (r, p) = (rules(), PublisherObs::default());
        let at = |fault: &str, cause: Value, needed: Value, bound: Value| -> Vec<Condition> {
            let mut j = eval_status("scoring");
            j["cohorts"][1]["fault"] = json!(fault);
            j["cohorts"][1]["fault_cause"] = cause;
            j["cohorts"][1]["operator_clear_needed"] = needed;
            j["cohorts"][1]["receipt_bound_reached_jobs"] = bound;
            let mut e = parse_eval(&j);
            e.ledger_over_threshold = Some(false);
            let mut o = healthy();
            o.coordinator.as_mut().unwrap().signing = None;
            o.eval = Some(e);
            evaluate(&o, &p, &r)
        };
        let c = at("scorer_unavailable", json!("slot"), json!(false), json!([]));
        assert_eq!(rules_of(&c), vec!["scorer_budget_fault"]);
        assert_eq!(
            c[0].detail,
            "fault scorer_unavailable in scoring (cause slot)"
        );
        let c = at("scorer_unavailable", json!("store"), json!(true), json!([]));
        assert_eq!(
            rules_of(&c),
            vec!["scorer_budget_fault", "operator_clear_needed"]
        );
        assert!(
            c[1].detail.contains("cause store") && c[1].detail.contains("no automatic clear"),
            "{c:?}"
        );
        let c = at(
            "unresolved_infrastructure",
            Value::Null,
            json!(true),
            json!([2, 5]),
        );
        assert_eq!(
            rules_of(&c),
            vec!["scorer_budget_fault", "operator_clear_needed"]
        );
        assert!(
            c[1].detail.contains("jobs at the receipt bound: [2, 5]"),
            "{c:?}"
        );
        let c = at("step_refused", Value::Null, json!(true), Value::Null);
        assert_eq!(rules_of(&c), vec!["cohort_fault", "operator_clear_needed"]);
        let c = at("step_refused", Value::Null, Value::Null, Value::Null);
        assert_eq!(rules_of(&c), vec!["cohort_fault"]);
        assert!(STATUS_RULES.contains(&"operator_clear_needed"));
    }

    /// The coordinator's status during the 2026-11-05/06 signing stall, on
    /// the fixture's epochs: authorized (issued at `issued`), active, last
    /// committed epoch 23_169, health `backing_off` on a reply it cannot
    /// read, the error `err_age` old and the newest sign-log row
    /// `event_age` old; `now` the confirmed epoch.
    fn stalled_signer(
        now: u64,
        issued: Option<u64>,
        state: &str,
        health: &str,
        err_age: u64,
        event_age: u64,
    ) -> Observed {
        let mut j = worker_status("primary", false);
        let s = &mut j["chain_journal"]["signing"];
        s["state"] = json!(state);
        if let Some(i) = issued {
            s["authorization"] = json!({"kind": "launch", "issued_epoch": i,
                "pointer_current": true, "pointer_confirmed_age_ms": 1000});
        }
        s["health"] = json!(health);
        s["backoff_ms"] = json!(68_671);
        s["last_error"] = json!("rpc: TooLarge");
        s["last_error_age_ms"] = json!(err_age);
        s["log"] = json!([{"age_ms": event_age, "event": "nonce_consumed"},
                          {"age_ms": event_age + 900, "event": "submitted"}]);
        let mut o = healthy();
        o.coordinator = Some(parse_worker(&j));
        o.eval.as_mut().unwrap().epoch = Some(now);
        o
    }

    /// v16: `commit_stalled` fires once two whole epochs passed without a
    /// commit while authorized and active (the confirmed epoch three past
    /// the last committed one, or past a later issue epoch); never while
    /// paused or unauthorized, where `commit_gap` alone still reports one
    /// missed epoch.
    #[test]
    fn an_authorized_coordinator_two_epochs_without_a_commit_is_stalled() {
        let (r, p) = (rules(), PublisherObs::default());
        let c = |o: &Observed| -> Vec<(&'static str, String)> {
            evaluate(o, &p, &r)
                .into_iter()
                .filter(|c| c.rule.starts_with("commit_"))
                .map(|c| (c.rule, c.detail))
                .collect()
        };
        // Progress after the last error: only the commit rules are read.
        let at = |now, issued, state| stalled_signer(now, issued, state, "ok", 60_000, 1_000);
        assert!(c(&at(23_170, Some(23_100), "active")).is_empty());
        assert_eq!(rules_of(&evaluate(&at(23_171, Some(23_100), "active"), &p, &r)), vec!["commit_gap"]);
        let fired = c(&at(23_172, Some(23_100), "active"));
        assert_eq!(fired.iter().map(|x| x.0).collect::<Vec<_>>(), vec!["commit_gap", "commit_stalled"]);
        assert_eq!(
            fired[1].1,
            "authorized and active, but no weight commit for epochs 23170..=23171 (last committed epoch 23169, now 23172); signing health ok, backoff 68 s, last error rpc: TooLarge"
        );
        for o in [at(23_180, None, "active"), at(23_180, Some(23_100), "paused")] {
            assert_eq!(c(&o).iter().map(|x| x.0).collect::<Vec<_>>(), vec!["commit_gap"]);
        }
        // A later authorization counts from its issue epoch.
        assert_eq!(c(&at(23_172, Some(23_170), "active")).len(), 1);
        assert_eq!(c(&at(23_173, Some(23_170), "active"))[1].0, "commit_stalled");
        // No commit among the recent operations: from the issue epoch.
        let mut o = at(23_172, Some(23_170), "active");
        o.coordinator.as_mut().unwrap().signing.as_mut().unwrap().ops.clear();
        assert_eq!(c(&o).iter().map(|x| x.0).collect::<Vec<_>>(), vec!["commit_gap"]);
        let mut o = at(23_173, Some(23_170), "active");
        o.coordinator.as_mut().unwrap().signing.as_mut().unwrap().ops.clear();
        assert!(c(&o)[1].1.contains("no commit among recent operations"), "{:?}", c(&o));
        // Decided only with the signing view and the eval epoch.
        assert!(decided("commit_stalled", "coordinator", &at(23_172, Some(23_100), "active")));
        let mut o = at(23_172, Some(23_100), "active");
        o.eval.as_mut().unwrap().epoch = None;
        assert!(!decided("commit_stalled", "coordinator", &o));
    }

    /// v16: `signing_backing_off` holds a tempo while the active signer's
    /// last error is newer than its newest sign-log row, whatever its health
    /// reads at that moment (it is `ok` between retries); progress after the
    /// error clears it, and a paused signer raises nothing.
    #[test]
    fn a_signer_failing_without_progress_for_a_tempo_is_backing_off() {
        let (r, p) = (rules(), PublisherObs::default());
        let c = |o: &Observed| -> Vec<Condition> {
            evaluate(o, &p, &r)
                .into_iter()
                .filter(|c| c.rule == "signing_backing_off")
                .collect()
        };
        for health in ["backing_off", "ok", "failing"] {
            let got = c(&stalled_signer(23_170, Some(23_100), "active", health, 51_329, 53_651_800));
            assert_eq!(got.len(), 1, "{health}");
            assert_eq!(got[0].subject, "coordinator");
            assert_eq!(got[0].hold_ms, TEMPO);
            assert_eq!(
                got[0].detail,
                format!("signer failing and backing off: no recorded signing progress for 53651 s; signing health {health}, backoff 68 s, last error rpc: TooLarge")
            );
        }
        // Progress after the last error, or a paused signer: nothing.
        assert!(c(&stalled_signer(23_170, Some(23_100), "active", "ok", 60_000, 1_000)).is_empty());
        assert!(c(&stalled_signer(23_170, Some(23_100), "paused", "paused", 1_000, 60_000)).is_empty());
        // Without a sign log, the health alone.
        let mut o = stalled_signer(23_170, Some(23_100), "active", "backing_off", 1_000, 60_000);
        o.coordinator.as_mut().unwrap().signing.as_mut().unwrap().last_event_age_ms = None;
        assert_eq!(c(&o).len(), 1);
        o.coordinator.as_mut().unwrap().signing.as_mut().unwrap().health = Some("ok".into());
        assert!(c(&o).is_empty());
        // The alert fires only after a whole tempo of observation, and an
        // unknown read in between carries it.
        let mut st = AlertState::default();
        let stalled = stalled_signer(23_170, Some(23_100), "active", "ok", 1_000, 60_000);
        let fired = |st: &mut AlertState, o: &Observed, at: u64| {
            let conds = conditions(o, &p, &r, st, at);
            observe(st, &conds, at);
            due(st, at, r.renotify_ms)
                .into_iter()
                .filter(|e| e.rule == "signing_backing_off" && e.state == EventKind::Firing)
                .count()
        };
        assert_eq!(fired(&mut st, &stalled, 0), 0);
        let mut unread = stalled.clone();
        unread.coordinator = None;
        assert_eq!(fired(&mut st, &unread, TEMPO / 2), 0);
        assert_eq!(fired(&mut st, &stalled, TEMPO - 1), 0);
        assert_eq!(fired(&mut st, &stalled, TEMPO), 1);
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
        // Fix round 1: a closed cohort's closure retried on a transient
        // code alerts once it has failed through a whole tempo of chain
        // progress (the stored epoch two past its first failure's), and
        // pauses nothing; within that span, or with a fault, it adds none.
        for (since, fires) in [(23_168u64, true), (23_169, false), (23_170, false)] {
            let mut j = eval_status("admission");
            j["cohorts"][0]["step_error"] = json!({"code": "closure put", "since_ms": 1, "since_epoch": since, "count": 300, "retry_ms": 2, "deterministic": false});
            let mut o = healthy();
            let mut e = parse_eval(&j);
            e.ledger_over_threshold = Some(false);
            o.eval = Some(e);
            let c = evaluate(&o, &p, &r);
            if fires {
                assert_eq!(rules_of(&c), vec!["cohort_fault"], "since {since}");
                assert_eq!(c[0].subject, "testnet-c6");
                assert_eq!(
                    c[0].detail,
                    "step error closure put in closed since epoch 23168 (300 failures), still retried"
                );
            } else {
                assert!(c.is_empty(), "since {since}: {c:?}");
            }
        }
        let mut j = eval_status("admission");
        j["cohorts"][0]["step_error"] =
            json!({"code": "summary missing", "since_epoch": 23_100, "count": 1});
        j["cohorts"][0]["fault"] = json!("step_refused");
        let mut o = healthy();
        let mut e = parse_eval(&j);
        e.ledger_over_threshold = Some(false);
        o.eval = Some(e);
        let c = evaluate(&o, &p, &r);
        assert_eq!(
            rules_of(&c),
            vec!["cohort_fault"],
            "one condition for the paused cohort"
        );
        assert_eq!(c[0].detail, "fault step_refused in closed");
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
                detail: None,
                ..OpView::default()
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
            retention_mismatch: vec![],
            live: vec![],
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

    /// The publisher's own retention cross-check raises `retention_stalled`
    /// for `public_receipts` with its lines, at once; it is decided by the
    /// publisher's pass, not by any status read, so an unanswered eval
    /// status neither carries nor blocks it, and it resolves when the pass
    /// reports no disagreement.
    #[test]
    fn the_public_retention_cross_check_raises_retention_stalled() {
        let r = rules();
        let p = PublisherObs {
            retention_mismatch: vec!["cohort 3: a".into(), "cohort 4: b".into()],
            ..PublisherObs::default()
        };
        let c = evaluate(&healthy(), &p, &r);
        assert_eq!(rules_of(&c), vec!["retention_stalled"]);
        assert_eq!(c[0].subject, PUBLIC_RECEIPTS);
        assert_eq!(
            (c[0].detail.as_str(), c[0].hold_ms),
            ("cohort 3: a; cohort 4: b", 0)
        );
        let mut o = healthy();
        o.eval = None;
        assert!(decided("retention_stalled", PUBLIC_RECEIPTS, &o));
        assert!(!decided("retention_stalled", "receipts", &o));
        let mut st = AlertState::default();
        let c = conditions(&healthy(), &p, &r, &st, 1_000);
        observe(&mut st, &c, 1_000);
        let ev = due(&st, 1_000, r.renotify_ms);
        assert_eq!(ev.len(), 1);
        mark(&mut st, &ev, 1_000, true);
        acknowledge(&mut st, &ev, 1_000);
        // The eval status unanswered and the disagreement gone: resolved, not
        // carried.
        let c = conditions(&o, &PublisherObs::default(), &r, &st, 2_000);
        assert!(!c.iter().any(|c| c.rule == "retention_stalled"), "{c:?}");
    }

    /// A pass that stopped before its retention step finished keeps the
    /// previous pass's disagreement: the open alert stays raised (no
    /// resolution, so no second firing on the next full pass); a full pass
    /// with no disagreement resolves it.
    #[test]
    fn an_errored_pass_keeps_the_open_retention_disagreement() {
        let r = rules();
        let open = vec!["cohort 3: a".to_owned()];
        let mut st = AlertState::default();
        let p = PublisherObs {
            retention_mismatch: open.clone(),
            ..PublisherObs::default()
        };
        let c = conditions(&healthy(), &p, &r, &st, 1_000);
        observe(&mut st, &c, 1_000);
        let ev = due(&st, 1_000, r.renotify_ms);
        assert_eq!(ev.len(), 1);
        mark(&mut st, &ev, 1_000, true);
        acknowledge(&mut st, &ev, 1_000);
        // The errored pass: not evaluated, the open line carried.
        let carried = PublisherObs {
            retention_mismatch: retention_after_pass(&p.retention_mismatch, None),
            ..PublisherObs::default()
        };
        assert_eq!(carried.retention_mismatch, open);
        let c = conditions(&healthy(), &carried, &r, &st, 2_000);
        assert!(c.iter().any(|c| c.rule == "retention_stalled"), "{c:?}");
        observe(&mut st, &c, 2_000);
        assert!(due(&st, 2_000, r.renotify_ms).is_empty(), "no new firing");
        // A full pass with no disagreement: resolved.
        let resolved = PublisherObs {
            retention_mismatch: retention_after_pass(&carried.retention_mismatch, Some(&[])),
            ..PublisherObs::default()
        };
        let c = conditions(&healthy(), &resolved, &r, &st, 3_000);
        assert!(!c.iter().any(|c| c.rule == "retention_stalled"), "{c:?}");
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

    /// Stuck-states fix round 2 (finding 4): a closed cohort's root step
    /// failing on alternating transient codes keeps the epoch it first
    /// failed in (the coordinator no longer resets it on a code change), so
    /// it alerts once, as one episode, whatever code each read shows.
    #[test]
    fn a_step_failing_on_alternating_codes_alerts_once() {
        let t = 1_793_645_520_000;
        let mut st = AlertState::default();
        let read = |code: &str, since: u64| {
            let mut j = eval_status("admission");
            j["cohorts"][0]["step_error"] = json!({"code": code, "since_epoch": since, "count": 1});
            let mut e = parse_eval(&j);
            e.ledger_over_threshold = Some(false);
            let mut o = healthy();
            o.eval = Some(e);
            o
        };
        // Within the first tempo nothing fires, whichever code.
        assert!(run(&mut st, &read("summary read", 23_169), t).is_empty());
        assert!(run(&mut st, &read("closure put", 23_169), t + 60_000).is_empty());
        let fired = run(&mut st, &read("summary read", 23_168), t + 120_000);
        assert_eq!(
            kinds(&fired),
            vec![(EventKind::Firing, "cohort_fault", "testnet-c6")]
        );
        for (k, code) in ["closure put", "summary read", "closure put"]
            .iter()
            .enumerate()
        {
            let ev = run(
                &mut st,
                &read(code, 23_168),
                t + 180_000 + k as u64 * 60_000,
            );
            assert!(ev.is_empty(), "a code change is the same episode: {ev:?}");
        }
    }

    /// Stuck-states fix round 2 (findings 3, 1, 8, 9): the alert-only
    /// conditions. A budget guard passed, attempt extensions, a stored
    /// epoch held behind the watermark for a whole tempo, and a commit whose
    /// inclusion is unknown each raise their own rule and pause nothing.
    #[test]
    fn the_alert_only_conditions_fire_on_their_reads() {
        let (r, p) = (rules(), PublisherObs::default());
        let with = |f: &dyn Fn(&mut Value)| {
            let mut j = eval_status("admission");
            f(&mut j);
            let mut e = parse_eval(&j);
            e.ledger_over_threshold = Some(false);
            let mut o = healthy();
            o.eval = Some(e);
            o
        };
        let c = evaluate(
            &with(&|j| j["cohorts"][1]["budget_alert"] = json!("scoring_budget")),
            &p,
            &r,
        );
        assert_eq!(rules_of(&c), vec!["scorer_budget_fault"]);
        assert_eq!(
            (c[0].subject.as_str(), c[0].detail.as_str()),
            (
                "testnet-c7",
                "budget guard scoring_budget passed in admission (tracking only, nothing paused)"
            )
        );
        // Closed cohorts carry theirs for the record only.
        assert!(
            evaluate(
                &with(&|j| j["cohorts"][0]["budget_alert"] = json!("scoring_budget")),
                &p,
                &r
            )
            .is_empty()
        );
        let c = evaluate(
            &with(&|j| j["cohorts"][1]["attempt_extensions"] = json!(2)),
            &p,
            &r,
        );
        assert_eq!(rules_of(&c), vec!["attempts_extended"]);
        assert!(
            evaluate(
                &with(&|j| j["cohorts"][1]["attempt_extensions"] = json!(0)),
                &p,
                &r
            )
            .is_empty()
        );
        // The watermark one epoch ahead is an increment waiting a tick; two
        // ahead is a whole tempo of watermark progress without one.
        assert!(evaluate(&with(&|j| j["watermark_epoch"] = json!(23_171)), &p, &r).is_empty());
        let deferred = with(&|j| j["watermark_epoch"] = json!(23_172));
        let c = evaluate(&deferred, &p, &r);
        assert_eq!(rules_of(&c), vec!["epoch_deferred"]);
        assert_eq!(c[0].subject, "eval");
        assert!(decided("epoch_deferred", "eval", &deferred));
        // A coordinator that does not report the watermark epoch leaves it
        // undecided rather than clear.
        assert!(!decided("epoch_deferred", "eval", &healthy()));
        let mut o = healthy();
        o.coordinator.as_mut().unwrap().signing = Some(SigningView {
            state: "active".into(),
            reason: None,
            ops: vec![
                OpView {
                    epoch: 23_170,
                    state: "committed".into(),
                    commit_block: Some(2),
                    ..OpView::default()
                },
                OpView {
                    epoch: 23_169,
                    state: "failed".into(),
                    detail: Some(INCLUSION_UNVERIFIABLE.into()),
                    ..OpView::default()
                },
            ],
            ..SigningView::default()
        });
        let c = evaluate(&o, &p, &r);
        assert_eq!(rules_of(&c), vec!["inclusion_unknown"]);
        assert_eq!(c[0].subject, "coordinator.epoch-23169");
        assert!(decided("inclusion_unknown", "coordinator.epoch-23169", &o));
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

    /// Review fix round 1 (findings 3, 5, 9): no signed coordinator row
    /// covering the installed authorization, or a failing re-sign, holds
    /// `row_record_stalled` for a tempo less what it already waited; a
    /// current record raises nothing.
    #[test]
    fn a_coordinator_row_that_no_record_covers_alerts_after_a_tempo() {
        let r = rules();
        let p = PublisherObs::default();
        let with = |stall: Option<crate::status::RowRecordStall>| {
            let mut o = healthy();
            o.coordinator
                .as_mut()
                .unwrap()
                .signing
                .as_mut()
                .unwrap()
                .row_record = stall;
            o
        };
        assert!(evaluate(&with(None), &p, &r).is_empty());
        let waiting = crate::status::RowRecordStall {
            code: "row_waiting_derivation".into(),
            age_ms: 1_000,
            uncovered: true,
        };
        let c = evaluate(&with(Some(waiting)), &p, &r);
        assert_eq!(rules_of(&c), vec!["row_record_stalled"]);
        assert_eq!(c[0].hold_ms, TEMPO - 1_000);
        assert!(
            c[0].detail.contains("no signed coordinator row covers")
                && c[0].detail.contains("row_waiting_derivation")
        );
        let failing = crate::status::RowRecordStall {
            code: "authorization_key_missing".into(),
            age_ms: TEMPO + 1,
            uncovered: false,
        };
        let c = evaluate(&with(Some(failing)), &p, &r);
        assert_eq!(
            (rules_of(&c), c[0].hold_ms),
            (vec!["row_record_stalled"], 0)
        );
        assert!(decided("row_record_stalled", "coordinator", &with(None)));
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

    /// A failure of the automatic target handoff alerts once it has lasted a
    /// tempo (its own age counts); no failure, or a coordinator built
    /// before the handoff, raises nothing.
    #[test]
    fn a_stalled_automatic_target_handoff_alerts_after_a_tempo() {
        let r = rules();
        let p = PublisherObs::default();
        let with = |e: Option<crate::status::AutoTargetError>| {
            let mut o = healthy();
            o.coordinator
                .as_mut()
                .unwrap()
                .signing
                .as_mut()
                .unwrap()
                .auto_target_error = e;
            o
        };
        assert!(evaluate(&with(None), &p, &r).is_empty());
        let fresh = crate::status::AutoTargetError {
            code: "authorization_key_missing".into(),
            label: Some("testnet-r0-20".into()),
            age_ms: 0,
        };
        let c = evaluate(&with(Some(fresh.clone())), &p, &r);
        assert_eq!(rules_of(&c), vec!["auto_target_stalled"]);
        assert_eq!(c[0].hold_ms, TEMPO);
        assert!(
            c[0].detail
                .contains("authorization_key_missing (testnet-r0-20)")
        );
        let old = crate::status::AutoTargetError {
            age_ms: TEMPO + 1,
            ..fresh
        };
        assert_eq!(evaluate(&with(Some(old)), &p, &r)[0].hold_ms, 0);
        assert!(decided("auto_target_stalled", "coordinator", &with(None)));
    }

    /// The status gate's report alerts on equal tokens at once and on the
    /// shared pre-split token left beside both consumer tokens after a
    /// tempo; a Worker built before the gate (no `access`) raises nothing,
    /// and an open entry waits for a read that carries the report.
    #[test]
    fn an_unfinished_or_mixed_token_split_alerts() {
        use crate::status::AccessView;
        let r = rules();
        let p = PublisherObs::default();
        let with = |a: Option<AccessView>| {
            let mut o = healthy();
            o.eval_worker.as_mut().unwrap().access = a;
            o
        };
        let split = AccessView {
            tokens_distinct: true,
            publisher_token: true,
            alerts_token: true,
            shared_status_token: false,
        };
        assert!(evaluate(&with(None), &p, &r).is_empty());
        assert!(evaluate(&with(Some(split.clone())), &p, &r).is_empty());
        // Mid-split: the shared token beside only one consumer token.
        let midway = AccessView {
            alerts_token: false,
            shared_status_token: true,
            ..split.clone()
        };
        assert!(evaluate(&with(Some(midway)), &p, &r).is_empty());
        let lingering = AccessView {
            shared_status_token: true,
            ..split.clone()
        };
        let c = evaluate(&with(Some(lingering)), &p, &r);
        assert_eq!(rules_of(&c), vec!["status_access_warning"]);
        assert_eq!(
            (c[0].subject.as_str(), c[0].hold_ms),
            ("eval_worker", TEMPO)
        );
        let equal = AccessView {
            tokens_distinct: false,
            ..split.clone()
        };
        let c = evaluate(&with(Some(equal)), &p, &r);
        assert_eq!(rules_of(&c), vec!["status_access_warning"]);
        assert_eq!(c[0].hold_ms, 0);
        assert!(STATUS_RULES.contains(&"status_access_warning"));
        assert!(decided(
            "status_access_warning",
            "eval_worker",
            &with(Some(split))
        ));
        assert_eq!(
            needs("status_access_warning", "eval_worker", &with(None)),
            vec![Need::Field("eval_worker", "access")]
        );
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
