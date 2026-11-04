//! The coordinator's status, read through bindings, and the redacted public
//! snapshot built from it.
//!
//! Three status documents are read each run, each behind its Worker's status
//! token:
//! the coordinator Worker's `/status` (heartbeat, journal and signing), the
//! eval Worker's `/status` (its own heartbeat and journal) and its
//! `/eval/status` (cohorts, the reward regime and the ledger). [`Observed`]
//! keeps only the typed fields the snapshot and the alert rules use; the
//! snapshot is then built from those fields alone, so nothing in a status
//! response reaches the public side unless named here. In particular the
//! snapshot never carries spend, generation, model, lane, job or document
//! data, fault details, or the ledger: faults are one boolean.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use slop_ninja_admission_wire::json::{Chain, NextCohort};
use slop_ninja_protocol::ChainBinding;

pub const STATUS_SCHEMA: &str = "slop_ninja.public.status/v1";

/// One Worker's `/status`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkerView {
    /// `primary` or `fallback`.
    pub mode: Option<String>,
    pub mode_age_ms: Option<u64>,
    pub confirmed_block: Option<u64>,
    pub confirmed_age_ms: Option<u64>,
    /// The active durable journal fault's kind.
    pub fault: Option<String>,
    pub signing: Option<SigningView>,
    /// `chain_journal.watchdog.subscription.state`: `delivering`, `silent`
    /// or `presumed_paused` (absent from Workers built before 2026-11-03).
    pub subscription: Option<String>,
    /// The status had no `chain_journal.journal` block, so `fault` (and the
    /// watermark) say nothing: the alert rules treat that read as unknown.
    pub journal_missing: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SigningView {
    pub state: String,
    pub reason: Option<String>,
    pub ops: Vec<OpView>,
    /// `signing.authorization` is present: a reward-v1 bundle is installed
    /// and the coordinator writes the private authorization pointer.
    pub authorization: bool,
    /// The pointer writer's last pass (`authorization.pointer_current` and
    /// the fields beside it); `None` from a coordinator built before they
    /// existed, or without an authorization.
    pub pointer: Option<PointerView>,
    /// The automatic target handoff's last failure
    /// (`signing.auto_target.error`, its label and its age), else a failed
    /// check of its authorization key (`key_error`); `None` while it has
    /// neither, or from a coordinator built before the handoff.
    pub auto_target_error: Option<AutoTargetError>,
}

/// A failure of the coordinator's automatic target handoff, as `/status`
/// reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutoTargetError {
    pub code: String,
    pub label: Option<String>,
    pub age_ms: u64,
}

/// The coordinator's authorization-pointer writer, as `/status` reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PointerView {
    /// The last pass confirmed the stored pointer equal to the plan.
    pub current: bool,
    /// Age of the last confirmation; `None` before the first.
    pub confirmed_age_ms: Option<u64>,
    /// The last pass's failure code, when it failed.
    pub error: Option<String>,
}

/// One weight-commit operation of the coordinator's signing journal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpView {
    pub epoch: u64,
    pub state: String,
    pub commit_block: Option<u64>,
    pub reveal_weights_match: Option<bool>,
    /// The operation's detail code (`inclusion_unverifiable`: its consumed
    /// nonce's inclusion could not be read, so whether its commit landed is
    /// unknown).
    pub detail: Option<String>,
}

/// One cohort of `/eval/status`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CohortView {
    pub cohort: u64,
    pub label: String,
    pub phase: String,
    pub fault: Option<String>,
    pub opened_epoch: Option<u64>,
    /// The coordinator's credited-tempo count when the cohort opened
    /// (`opened_active`; absent from coordinators that do not report it).
    pub opened_active: Option<u64>,
    pub k_tempos: Option<u64>,
    pub admission_tempos: Option<u64>,
    pub transition_tempos: Option<u64>,
    pub load: bool,
    /// The cohort's failing root step (`step_error`), when one is reported.
    pub step_error: Option<StepErrorView>,
    /// The first budget guard the cohort passed (`budget_alert`: tracking
    /// only, nothing paused); absent from coordinators that do not report it.
    pub budget_alert: Option<String>,
    /// Infrastructure attempt extensions past `max_attempts`
    /// (`attempt_extensions`).
    pub attempt_extensions: Option<u64>,
}

/// A cohort's failing root step: its code, the stored epoch at its first
/// failure (absent from coordinators that do not report it) and how many
/// times in a row it failed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct StepErrorView {
    pub code: String,
    pub since_epoch: Option<u64>,
    pub count: Option<u64>,
}

impl CohortView {
    /// Opened and not yet closed or abandoned.
    pub fn active(&self) -> bool {
        !self.load && !matches!(self.phase.as_str(), "queued" | "closed" | "abandoned")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RewardView {
    pub active_target: Option<String>,
    pub launch_burn: Option<bool>,
    pub reference_index: Option<u64>,
    pub current_content: Option<String>,
}

/// `/eval/status`. The ledger enters only as its `over_threshold` flag for
/// the alert rule, never as an amount.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvalView {
    pub confirmed_block: Option<u64>,
    pub epoch: Option<u64>,
    /// The epoch index at the confirmed watermark as the last tick read it
    /// (`watermark_epoch`): ahead of `epoch` while an increment waits for a
    /// head reading. Absent from coordinators that do not report it.
    pub watermark_epoch: Option<u64>,
    /// Tempos the coordinator has credited (`observed.active_tempos`): its
    /// scheduling clock, which uncredited tempos (silence, a stale or
    /// non-live observation, a catch-up jump) do not advance. Absent from
    /// coordinators that do not report it.
    pub active_tempos: Option<u64>,
    pub cohorts: Vec<CohortView>,
    /// The status had no `cohorts` array, or a cohort without its number,
    /// label or phase (dropped from `cohorts`): the cohort rules treat that
    /// read as unknown.
    pub cohorts_missing: bool,
    pub reward: Option<RewardView>,
    /// The spend ledger's billing month is past the deployment's private
    /// alert threshold (`false` without a threshold); `None` when the status
    /// carried no ledger standing.
    pub ledger_over_threshold: Option<bool>,
    /// `receipt_retention.stalled`, each entry as a content-free line: a due
    /// cohort whose detailed receipts remain after a further completion, or
    /// a predecessor not yet finished after one (absent from coordinators
    /// built before the rule).
    pub retention_stalled: Option<Vec<String>>,
    /// `receipt_retention.last_error`: the last failed deletion step's code.
    pub retention_error: Option<String>,
    /// Whether the ledger standing names an alert threshold
    /// (`alert_microusd` non-null); `None` when the status carried no
    /// ledger standing. `Some(false)` means `ledger_threshold` can never
    /// fire, which `ledger_threshold_unset` reports.
    pub ledger_threshold_set: Option<bool>,
    /// The coordinator's cohort calendar (`calendar`), when it serves one.
    pub calendar: Option<CalendarView>,
}

/// The coordinator's calendar view (`/eval/status` `calendar`): the current
/// cohort's admission window state and the next cohort the calendar opens
/// (queued, or the standing template's next number).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CalendarView {
    /// The current cohort's admission window state.
    pub current: Option<CalendarCurrent>,
    /// The next cohort as the calendar reports it (`cohort`,
    /// `earliest_open_epoch`, and `admission_tempos` from coordinators that
    /// report it).
    pub next: Option<NextCohort>,
}

/// The calendar's current cohort (`calendar.current`): the window flags
/// are `None` from coordinators that do not report them, and `freeze_wait`
/// (what a closing window's roster freeze waits on) is set only while the
/// window is closing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CalendarCurrent {
    pub cohort: u64,
    pub window_open: Option<bool>,
    pub window_closing: Option<bool>,
    pub freeze_wait: Option<String>,
}

impl EvalView {
    /// The calendar's window state for `cohort`, when it reports one.
    pub fn window_of(&self, cohort: u64) -> Option<&CalendarCurrent> {
        self.calendar
            .as_ref()
            .and_then(|k| k.current.as_ref())
            .filter(|c| c.cohort == cohort)
    }

    /// The main instance's active cohort.
    pub fn active(&self) -> Option<&CohortView> {
        self.cohorts.iter().find(|c| c.active())
    }

    /// Tempos since cohort `c` opened on the clock the coordinator schedules
    /// it by: its credited tempos (`observed.active_tempos - opened_active`)
    /// when the status reports its count, and epochs (`observed.epoch -
    /// opened_epoch`) only from coordinators that do not. With the count
    /// reported but the cohort's `opened_active` absent, `None`: the two
    /// clocks are never mixed. The flag is true for credited tempos.
    pub fn cohort_age(&self, c: &CohortView) -> Option<(u64, bool)> {
        match self.active_tempos {
            Some(now) => Some((now.saturating_sub(c.opened_active?), true)),
            None => Some((self.epoch?.saturating_sub(c.opened_epoch?), false)),
        }
    }
}

/// Everything read this run; `None` where a source did not answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observed {
    pub coordinator: Option<WorkerView>,
    pub eval_worker: Option<WorkerView>,
    pub eval: Option<EvalView>,
}

// The status documents as typed views: only the fields named here are
// parsed; every other field is skipped without building a JSON tree, so a
// status body's memory is its own bytes (see `arena::status_body_bound`).

#[derive(Deserialize, Default)]
#[serde(default)]
struct ServiceJson {
    chain_journal: Option<ChainJournalJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ChainJournalJson {
    mode: Option<String>,
    mode_age_ms: Option<u64>,
    journal: Option<JournalJson>,
    signing: Option<SigningJson>,
    watchdog: Option<WatchdogJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WatchdogJson {
    subscription: Option<SubscriptionJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SubscriptionJson {
    state: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JournalJson {
    confirmed: Option<BlockJson>,
    confirmed_age_ms: Option<u64>,
    fault: Option<FaultJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct BlockJson {
    number: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FaultJson {
    kind: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SigningJson {
    state: Option<String>,
    reason: Option<String>,
    ops: Vec<OpJson>,
    authorization: Option<AuthorizationJson>,
    auto_target: Option<AutoTargetJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AutoTargetJson {
    error: Option<String>,
    error_label: Option<String>,
    error_age_ms: Option<u64>,
    key_error: Option<String>,
    key_age_ms: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AuthorizationJson {
    pointer_current: Option<bool>,
    pointer_confirmed_age_ms: Option<u64>,
    pointer_error: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OpJson {
    epoch: Option<u64>,
    state: Option<String>,
    commit_block: Option<u64>,
    reveal_weights_match: Option<bool>,
    detail: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EvalJson {
    observed: ObservedJson,
    watermark_epoch: Option<u64>,
    cohorts: Option<Vec<CohortJson>>,
    reward: Option<RewardJson>,
    /// `null` when the coordinator's ledger call failed: that read is
    /// unknown, and the rest of the status still parses.
    ledger: Option<LedgerJson>,
    receipt_retention: RetentionJson,
    calendar: Option<CalendarJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RetentionJson {
    stalled: Option<Vec<StalledJson>>,
    last_error: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CalendarJson {
    current: Option<CalendarCurrentJson>,
    next: Option<CalendarNextJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StalledJson {
    kind: Option<String>,
    cohort: Option<u64>,
    label: Option<String>,
    due_after_cohort: Option<u64>,
    predecessor: Option<String>,
    after_cohort: Option<u64>,
    completions_since: Option<u64>,
}

impl StalledJson {
    fn line(&self) -> String {
        let n = self.completions_since.unwrap_or(0);
        match self.kind.as_deref() {
            Some("predecessor") => format!(
                "predecessor {} not finished after cohort {}, {n} completion(s) since",
                self.predecessor.as_deref().unwrap_or("?"),
                self.after_cohort.map_or("?".into(), |c| c.to_string()),
            ),
            _ => format!(
                "cohort {} ({}) due after cohort {}, {n} completion(s) since",
                self.cohort.map_or("?".into(), |c| c.to_string()),
                self.label.as_deref().unwrap_or("?"),
                self.due_after_cohort.map_or("?".into(), |c| c.to_string()),
            ),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CalendarCurrentJson {
    cohort: Option<u64>,
    window_open: Option<bool>,
    window_closing: Option<bool>,
    freeze_wait: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CalendarNextJson {
    cohort: Option<u64>,
    earliest_open_epoch: Option<u64>,
    admission_tempos: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ObservedJson {
    confirmed: Option<BlockJson>,
    epoch: Option<EpochJson>,
    active_tempos: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EpochJson {
    index: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CohortJson {
    cohort: Option<u64>,
    label: Option<String>,
    phase: Option<String>,
    fault: Option<String>,
    opened_epoch: Option<u64>,
    opened_active: Option<u64>,
    k_tempos: Option<u64>,
    admission_tempos: Option<u64>,
    transition_tempos: Option<u64>,
    load: Option<bool>,
    step_error: Option<StepErrorView>,
    budget_alert: Option<String>,
    attempt_extensions: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RewardJson {
    active_target: Option<String>,
    launch_burn: Option<bool>,
    references: ReferencesJson,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReferencesJson {
    k: Option<u64>,
    current_content: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LedgerJson {
    standing: Option<StandingJson>,
    over_threshold: Option<bool>,
}

/// The coordinator's `ledger::Standing`: the tracking ledger's
/// `alert_microusd` and `over_threshold`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct StandingJson {
    alert_microusd: Option<u64>,
    over_threshold: Option<bool>,
    spent_microusd: Option<u64>,
    reserved_microusd: Option<u64>,
}

impl StandingJson {
    /// The flag when the status carries it, else derived from the
    /// threshold the same way the ledger derives it: settled plus reserved
    /// past it. No threshold: never over.
    fn over_threshold(&self, outer: Option<bool>) -> Option<bool> {
        if let Some(f) = self.over_threshold.or(outer) {
            return Some(f);
        }
        let spent = self.spent_microusd?;
        let total = spent.saturating_add(self.reserved_microusd.unwrap_or(0));
        Some(self.alert_microusd.is_some_and(|a| total > a))
    }
}

fn worker_view(j: ServiceJson) -> WorkerView {
    let c = j.chain_journal.unwrap_or_default();
    let journal_missing = c.journal.is_none();
    let journal = c.journal.unwrap_or_default();
    WorkerView {
        mode: c.mode,
        mode_age_ms: c.mode_age_ms,
        confirmed_block: journal.confirmed.and_then(|b| b.number),
        confirmed_age_ms: journal.confirmed_age_ms,
        fault: journal
            .fault
            .map(|f| f.kind.unwrap_or_else(|| "fault".into())),
        journal_missing,
        subscription: c
            .watchdog
            .and_then(|w| w.subscription)
            .and_then(|s| s.state),
        signing: c.signing.and_then(|s| {
            // A cohort's failure first; else the pass's check of the
            // authorization key, which holds no cohort until one needs a
            // signature but must not wait that long to be seen.
            let auto_target_error = s.auto_target.and_then(|a| match a.error {
                Some(code) => Some(AutoTargetError {
                    code,
                    label: a.error_label,
                    age_ms: a.error_age_ms.unwrap_or(0),
                }),
                None => a.key_error.map(|k| AutoTargetError {
                    code: format!("authorization key {k}"),
                    label: None,
                    age_ms: a.key_age_ms.unwrap_or(0),
                }),
            });
            Some(SigningView {
                auto_target_error,
                authorization: s.authorization.is_some(),
                pointer: s.authorization.and_then(|a| {
                    Some(PointerView {
                        current: a.pointer_current?,
                        confirmed_age_ms: a.pointer_confirmed_age_ms,
                        error: a.pointer_error,
                    })
                }),
                state: s.state?,
                reason: s.reason,
                ops: s
                    .ops
                    .into_iter()
                    .filter_map(|o| {
                        Some(OpView {
                            epoch: o.epoch?,
                            state: o.state.unwrap_or_default(),
                            commit_block: o.commit_block,
                            reveal_weights_match: o.reveal_weights_match,
                            detail: o.detail,
                        })
                    })
                    .collect(),
            })
        }),
    }
}

fn eval_view(j: EvalJson) -> EvalView {
    let listed = j.cohorts.as_ref().map(Vec::len);
    let cohorts: Vec<CohortView> = j
        .cohorts
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| {
            Some(CohortView {
                cohort: c.cohort?,
                label: c.label?,
                phase: c.phase?,
                fault: c.fault,
                opened_epoch: c.opened_epoch,
                opened_active: c.opened_active,
                k_tempos: c.k_tempos,
                admission_tempos: c.admission_tempos,
                transition_tempos: c.transition_tempos,
                load: c.load.unwrap_or(false),
                step_error: c.step_error,
                budget_alert: c.budget_alert,
                attempt_extensions: c.attempt_extensions,
            })
        })
        .collect();
    let cohorts_missing = listed != Some(cohorts.len());
    let ledger_over_threshold = j.ledger.as_ref().and_then(|l| {
        l.standing
            .as_ref()
            .and_then(|s| s.over_threshold(l.over_threshold))
    });
    let ledger_threshold_set = j
        .ledger
        .as_ref()
        .and_then(|l| l.standing.as_ref())
        .map(|s| s.alert_microusd.is_some());
    EvalView {
        confirmed_block: j.observed.confirmed.and_then(|b| b.number),
        epoch: j.observed.epoch.and_then(|e| e.index),
        watermark_epoch: j.watermark_epoch,
        active_tempos: j.observed.active_tempos,
        cohorts,
        cohorts_missing,
        reward: j.reward.map(|r| RewardView {
            active_target: r.active_target,
            launch_burn: r.launch_burn,
            reference_index: r.references.k,
            current_content: r.references.current_content,
        }),
        ledger_over_threshold,
        retention_stalled: j
            .receipt_retention
            .stalled
            .map(|v| v.iter().map(StalledJson::line).collect()),
        retention_error: j.receipt_retention.last_error,
        ledger_threshold_set,
        calendar: j.calendar.map(|c| CalendarView {
            current: c.current.and_then(|x| {
                Some(CalendarCurrent {
                    cohort: x.cohort?,
                    window_open: x.window_open,
                    window_closing: x.window_closing,
                    freeze_wait: x.freeze_wait,
                })
            }),
            next: c.next.and_then(|n| {
                Some(NextCohort {
                    cohort: n.cohort?,
                    earliest_open_epoch: n.earliest_open_epoch,
                    admission_tempos: n.admission_tempos,
                    ..Default::default()
                })
            }),
        }),
    }
}

/// Parses a Worker's `/status` (`ServiceStatus`) body; `None` when it is not
/// the expected shape.
pub fn parse_worker_bytes(body: &[u8]) -> Option<WorkerView> {
    serde_json::from_slice(body).ok().map(worker_view)
}

/// Parses an `/eval/status` body.
pub fn parse_eval_bytes(body: &[u8]) -> Option<EvalView> {
    serde_json::from_slice(body).ok().map(eval_view)
}

/// [`parse_worker_bytes`] over a JSON value (tests and tools).
pub fn parse_worker(v: &Value) -> WorkerView {
    serde_json::from_value(v.clone())
        .map(worker_view)
        .unwrap_or_default()
}

/// [`parse_eval_bytes`] over a JSON value (tests and tools).
pub fn parse_eval(v: &Value) -> EvalView {
    serde_json::from_value(v.clone())
        .map(eval_view)
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confirmed {
    pub block: u64,
    pub epoch: Option<u64>,
    pub age_secs: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Admitting {
    pub cohort: u64,
    pub label: Option<String>,
    pub opened_epoch: u64,
    pub planned_cutoff_epoch: u64,
}

/// Which status sources answered this run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sources {
    pub coordinator: bool,
    pub eval: bool,
}

/// The public snapshot (`v1/status.json`), the shape the admission contract
/// documents (`docs/testnet/admission-api.md`, section 10) plus fields the
/// miner client ignores.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicStatus {
    pub schema: String,
    pub network: String,
    pub chain: Chain,
    pub confirmed: Option<Confirmed>,
    pub phase: Option<String>,
    /// `burn` while the launch burn row is the active target, else `target`.
    pub mode: Option<String>,
    pub fault: bool,
    pub notice: Option<String>,
    pub admitting: Option<Admitting>,
    pub next: Option<NextCohort>,
    pub coordinator: Option<String>,
    pub active_target: Option<String>,
    /// The current reference `R_k`'s inference-content digest.
    pub reference: Option<String>,
    pub reference_index: Option<u64>,
    pub generated_at: u64,
    pub sources: Sources,
    pub last_published_cohort: Option<u64>,
}

pub const BURN_NOTICE: &str = "Launch burn: every unit of the weight row goes to the subnet owner's UID until an \
    entrant beats the reference R_k by the margin delta and its cohort's target activates at the cohort's close.";
pub const TARGET_NOTICE: &str = "A cohort target is active: validators pay its credited clusters every tempo; \
    units of a hotkey that is no longer registered under the same registration go to the owner's UID.";
pub const TESTNET_NOTICE: &str = "Testnet: scores test the evaluation plumbing. Evaluation documents may be reused \
    across cohorts and may be exposed; testnet scores say nothing about quality on unseen text.";
pub const CONFIG_NOTICE: &str = "The publisher's configuration was refused; publication is paused until the \
    operator fixes it.";
pub const UNREACHABLE_NOTICE: &str = "The publisher could not read the coordinator's status this run; the fields \
    below may be missing until it can.";

fn hex_digest(s: &Option<String>) -> Option<String> {
    s.as_ref()
        .map(|d| d.trim_start_matches("0x").to_owned())
        .filter(|d| {
            d.len() == 64
                && d.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
}

pub struct SnapshotInputs<'a> {
    pub chain: &'a ChainBinding,
    pub observed: &'a Observed,
    pub admission_origin: Option<&'a str>,
    pub operator_notice: Option<&'a str>,
    pub last_published_cohort: Option<u64>,
    pub now_ms: u64,
}

/// Builds the snapshot from typed fields only.
pub fn snapshot(i: &SnapshotInputs<'_>) -> PublicStatus {
    let o = i.observed;
    let eval = o.eval.as_ref();
    let worker = o.eval_worker.as_ref();
    let active = eval.and_then(EvalView::active);
    let confirmed = eval.and_then(|e| e.confirmed_block).map(|block| Confirmed {
        block,
        epoch: eval.and_then(|e| e.epoch),
        age_secs: worker.and_then(|w| w.confirmed_age_ms).map(|ms| ms / 1000),
    });
    let fault = [o.coordinator.as_ref(), worker]
        .into_iter()
        .flatten()
        .any(|w| w.fault.is_some() || w.signing.as_ref().is_some_and(|s| s.state == "fault"))
        || active.is_some_and(|c| c.fault.is_some())
        || o.eval.is_none()
        || o.coordinator.is_none();
    // The calendar's window state for the active cohort, when reported: a
    // closed window (closing, waiting for the freeze) or a cohort without
    // coordinator admission admits nothing, although its phase is still
    // `admission`.
    let calendar = eval.and_then(|e| e.calendar.as_ref());
    let window_open = |cohort: u64| {
        eval.and_then(|e| e.window_of(cohort))
            .and_then(|w| w.window_open)
            .unwrap_or(true)
    };
    let admitting = active
        .filter(|c| c.phase == "admission" && window_open(c.cohort))
        .and_then(|c| {
            let opened = c.opened_epoch?;
            Some(Admitting {
                cohort: c.cohort,
                label: Some(c.label.clone()),
                opened_epoch: opened,
                planned_cutoff_epoch: opened + c.admission_tempos?,
            })
        });
    // The calendar's next cohort when it serves one (a queued cohort, or the
    // standing template's next number, with its earliest opening epoch and
    // its admission window); otherwise only a queued cohort.
    let from_calendar = calendar.and_then(|k| k.next.clone());
    let next = from_calendar.or_else(|| {
        eval.and_then(|e| {
            let queued = e
                .cohorts
                .iter()
                .filter(|c| !c.load && c.phase == "queued")
                .min_by_key(|c| c.cohort)?;
            let earliest = match active {
                Some(a) => a.opened_epoch.zip(a.k_tempos).map(|(o, k)| o + k),
                None => e.epoch.map(|x| x + 1),
            };
            Some(NextCohort {
                cohort: queued.cohort,
                earliest_open_epoch: earliest,
                admission_tempos: None,
                ..Default::default()
            })
        })
    });
    let reward = eval.and_then(|e| e.reward.as_ref());
    let launch_burn = reward.and_then(|r| r.launch_burn);
    let mut notice = vec![];
    if o.eval.is_none() || o.coordinator.is_none() {
        notice.push(UNREACHABLE_NOTICE);
    }
    match launch_burn {
        Some(true) => notice.push(BURN_NOTICE),
        Some(false) => notice.push(TARGET_NOTICE),
        None => {}
    }
    notice.push(TESTNET_NOTICE);
    if let Some(extra) = i.operator_notice {
        notice.push(extra);
    }
    PublicStatus {
        schema: STATUS_SCHEMA.into(),
        network: "testnet".into(),
        chain: Chain::from_binding(i.chain),
        confirmed,
        phase: Some(match (active, eval) {
            (Some(c), _) => c.phase.clone(),
            (None, Some(e)) if e.cohorts.iter().any(|c| !c.load && c.phase == "queued") => {
                "queued".into()
            }
            (None, Some(_)) => "idle".into(),
            (None, None) => "unknown".into(),
        }),
        mode: launch_burn.map(|b| if b { "burn" } else { "target" }.to_owned()),
        fault,
        notice: Some(notice.join(" ")),
        admitting,
        next,
        coordinator: i.admission_origin.map(str::to_owned),
        active_target: reward.and_then(|r| hex_digest(&r.active_target)),
        reference: reward.and_then(|r| hex_digest(&r.current_content)),
        reference_index: reward.and_then(|r| r.reference_index),
        generated_at: i.now_ms / 1000,
        sources: Sources {
            coordinator: o.coordinator.is_some(),
            eval: o.eval.is_some() && o.eval_worker.is_some(),
        },
        last_published_cohort: i.last_published_cohort,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub const SECRET_MARKERS: &[&str] = &[
        "PRIVATE-DOCUMENT-TEXT",
        "cost_microusd",
        "budget_microusd",
        "spent_microusd",
        "@cf/",
        "prompt_tokens",
        "extrinsic_hash",
        "fault-detail-hash",
        "rejects",
        "pool_rows_used",
    ];

    /// Status documents shaped like the coordinator's, with private fields a
    /// pass-through would leak.
    pub fn eval_status(phase: &str) -> Value {
        json!({
            "observed": {"confirmed": {"number": 8_340_012, "hash": "0xab"}, "epoch": {"index": 23_170, "block": {"number": 8_340_000}}},
            "kick_age_ms": 4000,
            "pool_rows_used": 500,
            "cohorts": [
                {"cohort": 6, "label": "testnet-c6", "phase": "closed", "fault": null, "opened_epoch": 23_150,
                 "k_tempos": 20, "admission_tempos": 1, "transition_tempos": 2, "load": false},
                {"cohort": 7, "label": "testnet-c7", "phase": phase, "fault": null, "opened_epoch": 23_169,
                 "k_tempos": 20, "admission_tempos": 1, "transition_tempos": 2, "load": false,
                 "generation": [{"model": "@cf/x", "cost_microusd": 123, "prompt_tokens": 9}],
                 "budget_microusd": 99, "rejects": {"PRIVATE-DOCUMENT-TEXT": 1},
                 "docs": [{"kind": "mixed", "status": "ready", "count": 3}]},
                {"cohort": 8, "label": "testnet-c8", "phase": "queued", "load": false},
                {"cohort": 900, "label": "load-x", "phase": "scoring", "load": true}
            ],
            "reward": {"rule": "reward-v1", "active_target": "11".repeat(32), "launch_burn": true,
                       "references": {"k": 0, "current_content": "22".repeat(32), "predecessor_content": null},
                       "note": "PRIVATE-DOCUMENT-TEXT"},
            "ledger": {"standing": {"cycle": "2026-11-01", "alert_microusd": 1000, "spent_microusd": 900, "reserved_microusd": 150,
                                    "over_threshold": true},
                       "cycle_total_microusd": 1050, "over_threshold": true,
                       "reservations": [{"key": "main:testnet-c8", "cycle": "2026-11-01", "microusd": 150, "at_ms": 1}],
                       "recent_charges": []},
            "receipt_retention": {"rule": "receipts_after_two_later_completions_v2", "stalled": [], "last_error": null},
        })
    }

    pub fn worker_status(mode: &str, fault: bool) -> Value {
        json!({
            "service": "slop-ninja-coordinator", "version": "0.1.0", "deployment": "testnet",
            "config": {"rpc_host": "rpc.example.invalid"},
            "chain_journal": {
                "mode": mode, "mode_age_ms": 1000,
                "watchdog": {"source": "webhook", "subscription": {"state": "delivering", "silent_ms": 9000}},
                "journal": {"confirmed": {"number": 8_340_010, "hash": "0xcd"}, "confirmed_age_ms": 40_000,
                            "fault": if fault { json!({"id": 1, "kind": "parent_conflict", "expected_hash": "fault-detail-hash"}) } else { Value::Null }},
                "signing": {"state": "active", "reason": null, "ops": [
                    {"epoch": 23_169, "state": "committed", "commit_block": 8_339_800, "reveal_weights_match": true,
                     "attempts": [{"extrinsic_hash": "0xextrinsic_hash"}]}
                ]}
            }
        })
    }

    fn observed(phase: &str) -> Observed {
        Observed {
            coordinator: Some(parse_worker(&worker_status("primary", false))),
            eval_worker: Some(parse_worker(&worker_status("primary", false))),
            eval: Some(parse_eval(&eval_status(phase))),
        }
    }

    fn chain() -> ChainBinding {
        ChainBinding {
            genesis: slop_ninja_protocol::TESTNET_GENESIS,
            netuid: 601,
            mechanism: 0,
        }
    }

    /// The coordinator's ledger `/status` (the shared golden fixture, which
    /// the coordinator's ledger test checks byte for byte against what it
    /// serializes), embedded as `ledger` in `/eval/status`, sets the flag
    /// the `ledger_threshold` rule keys on; a deployment without a threshold
    /// never does, and a `cap_microusd` field is not a threshold.
    #[test]
    fn parses_the_coordinator_ledger_status() {
        let ledger: Value =
            serde_json::from_str(include_str!("../tests/fixtures/eval-ledger-status.json"))
                .unwrap();
        let mut j = eval_status("admission");
        j["ledger"] = ledger.clone();
        assert_eq!(parse_eval(&j).ledger_over_threshold, Some(true));
        let mut under = ledger.clone();
        under["standing"]["over_threshold"] = json!(false);
        under["over_threshold"] = json!(false);
        j["ledger"] = under;
        assert_eq!(parse_eval(&j).ledger_over_threshold, Some(false));
        // No threshold: the coordinator serializes `alert_microusd: null`
        // and `over_threshold: false`.
        let mut none = ledger.clone();
        none["standing"]["alert_microusd"] = Value::Null;
        none["standing"]["over_threshold"] = json!(false);
        none["over_threshold"] = json!(false);
        j["ledger"] = none;
        assert_eq!(parse_eval(&j).ledger_over_threshold, Some(false));
        // ... which is reported separately (`ledger_threshold_unset`).
        assert_eq!(parse_eval(&j).ledger_threshold_set, Some(false));
        // No flag: derived from the threshold; a cap field is not one.
        j["ledger"] = json!({"standing": {"cycle": "2026-11-01", "alert_microusd": 1000, "spent_microusd": 900, "reserved_microusd": 150}});
        assert_eq!(parse_eval(&j).ledger_over_threshold, Some(true));
        j["ledger"] = json!({"standing": {"cycle": "2026-11-01", "alert_microusd": 1000, "spent_microusd": 700, "reserved_microusd": 150}});
        assert_eq!(parse_eval(&j).ledger_over_threshold, Some(false));
        j["ledger"] = json!({"standing": {"cycle": "2026-11-01", "cap_microusd": 1000, "spent_microusd": 900, "reserved_microusd": 150}});
        assert_eq!(parse_eval(&j).ledger_over_threshold, Some(false));
        assert_eq!(parse_eval(&j).ledger_threshold_set, Some(false));
        // A ledger that did not answer (`ledger: null`): unknown.
        j["ledger"] = Value::Null;
        let e = parse_eval(&j);
        assert_eq!(e.ledger_over_threshold, None);
        assert_eq!(e.ledger_threshold_set, None);
        assert!(
            !e.cohorts.is_empty() && e.epoch.is_some(),
            "the rest still parses"
        );
    }

    /// `signing.auto_target`: a cohort's failure is reported with its label
    /// and age; with none, a failed key check is; with neither, nothing.
    #[test]
    fn parses_the_automatic_target_handoffs_failure_and_key_check() {
        let with = |auto: Value| {
            let mut j = worker_status("primary", false);
            j["chain_journal"]["signing"]["auto_target"] = auto;
            parse_worker(&j).signing.unwrap().auto_target_error
        };
        assert_eq!(
            with(json!({"error": null, "key_ok": true, "key_error": null})),
            None
        );
        assert_eq!(
            with(
                json!({"error": "receipts_retired", "error_label": "testnet-r0-20", "error_age_ms": 5,
                        "key_error": "authorization_key_missing", "key_age_ms": 9})
            ),
            Some(AutoTargetError {
                code: "receipts_retired".into(),
                label: Some("testnet-r0-20".into()),
                age_ms: 5
            })
        );
        assert_eq!(
            with(
                json!({"error": null, "key_ok": false, "key_error": "authorization_key_missing", "key_age_ms": 9})
            ),
            Some(AutoTargetError {
                code: "authorization key authorization_key_missing".into(),
                label: None,
                age_ms: 9
            })
        );
    }

    /// The views record a status that answered without the journal block or
    /// the cohort list (or with a cohort missing its label or phase), so the
    /// alert rules can tell "absent" from "clear".
    #[test]
    fn missing_blocks_are_recorded() {
        assert!(!parse_worker(&worker_status("primary", false)).journal_missing);
        for v in [
            json!({}),
            json!({"chain_journal": null}),
            json!({"chain_journal": {"mode": "primary"}}),
        ] {
            let w = parse_worker(&v);
            assert!(w.journal_missing && w.fault.is_none(), "{v}");
        }
        assert!(!parse_eval(&eval_status("admission")).cohorts_missing);
        let mut j = eval_status("admission");
        j.as_object_mut().unwrap().remove("cohorts");
        assert!(parse_eval(&j).cohorts_missing);
        let mut j = eval_status("admission");
        j["cohorts"][1].as_object_mut().unwrap().remove("label");
        let e = parse_eval(&j);
        assert!(e.cohorts_missing && e.cohorts.len() == 3);
        let mut j = eval_status("admission");
        j["cohorts"] = json!([]);
        assert!(
            !parse_eval(&j).cohorts_missing,
            "an empty list is a complete list"
        );
    }

    #[test]
    fn admission_snapshot_matches_the_contract() {
        let o = observed("admission");
        let snap = snapshot(&SnapshotInputs {
            chain: &chain(),
            observed: &o,
            admission_origin: Some("https://admission.example.invalid"),
            operator_notice: None,
            last_published_cohort: Some(6),
            now_ms: 1_793_500_000_000,
        });
        assert_eq!(snap.schema, STATUS_SCHEMA);
        assert_eq!(snap.phase.as_deref(), Some("admission"));
        assert_eq!(snap.mode.as_deref(), Some("burn"));
        assert!(!snap.fault);
        assert_eq!(
            snap.admitting,
            Some(Admitting {
                cohort: 7,
                label: Some("testnet-c7".into()),
                opened_epoch: 23_169,
                planned_cutoff_epoch: 23_170
            })
        );
        assert_eq!(
            snap.next,
            Some(NextCohort {
                cohort: 8,
                earliest_open_epoch: Some(23_189),
                admission_tempos: None,
                ..Default::default()
            })
        );
        assert_eq!(
            snap.confirmed,
            Some(Confirmed {
                block: 8_340_012,
                epoch: Some(23_170),
                age_secs: Some(40)
            })
        );
        assert_eq!(snap.reference.as_deref(), Some("22".repeat(32).as_str()));
        // The client's documented fields parse (admission-api.md section 10).
        let text = serde_json::to_string(&snap).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        for field in [
            "schema",
            "network",
            "chain",
            "confirmed",
            "phase",
            "mode",
            "fault",
            "notice",
            "admitting",
            "next",
            "coordinator",
            "active_target",
            "reference",
        ] {
            assert!(v.get(field).is_some(), "{field}");
        }
        let chain: Chain = serde_json::from_value(v["chain"].clone()).unwrap();
        assert_eq!(chain.netuid, 601);
    }

    /// Third W3 review, finding 3: the public `next` comes from the
    /// coordinator's calendar for the whole cohort (not only once the next
    /// cohort is queued), and `admitting` ends when the window closes even
    /// though the phase stays `admission` until the freeze.
    #[test]
    fn calendar_drives_next_and_a_closed_window_admits_nothing() {
        let snap_of = |v: &Value| {
            let o = Observed {
                eval: Some(parse_eval(v)),
                ..observed("admission")
            };
            snapshot(&SnapshotInputs {
                chain: &chain(),
                observed: &o,
                admission_origin: None,
                operator_notice: None,
                last_published_cohort: None,
                now_ms: 0,
            })
        };
        let mut v = eval_status("admission");
        // No cohort queued yet: the calendar still names the next one.
        v["cohorts"]
            .as_array_mut()
            .unwrap()
            .retain(|c| c["phase"] != "queued");
        assert_eq!(
            snap_of(&v).next,
            None,
            "an older coordinator: only a queued cohort"
        );
        v["calendar"] = json!({"rule": "back_to_back_on_tempo_boundaries/v1",
            "current": {"cohort": 7, "phase": "admission", "window_open": true, "window_closing": false},
            "next": {"cohort": 8, "earliest_open_epoch": 23_187},
            "template": {"sha256": "PRIVATE-TEMPLATE", "paused": false, "last_error": null}});
        let snap = snap_of(&v);
        assert_eq!(
            snap.next,
            Some(NextCohort {
                cohort: 8,
                earliest_open_epoch: Some(23_187),
                admission_tempos: None,
                ..Default::default()
            })
        );
        assert_eq!(snap.admitting.as_ref().map(|a| a.cohort), Some(7));
        // A coordinator that reports the next cohort's admission window: the
        // snapshot carries it as reported.
        v["calendar"]["next"]["admission_tempos"] = json!(17);
        let with_window = snap_of(&v);
        assert_eq!(
            with_window.next.as_ref().and_then(|n| n.admission_tempos),
            Some(17)
        );
        let text = serde_json::to_value(&with_window).unwrap();
        assert_eq!(text["next"]["admission_tempos"], 17);
        v["calendar"]["next"]
            .as_object_mut()
            .unwrap()
            .remove("admission_tempos");
        // The window closed, waiting for the freeze: nothing admits.
        v["calendar"]["current"]["window_open"] = json!(false);
        v["calendar"]["current"]["window_closing"] = json!(true);
        let snap = snap_of(&v);
        assert_eq!(snap.phase.as_deref(), Some("admission"));
        assert_eq!(snap.admitting, None);
        assert_eq!(snap.next.as_ref().map(|n| n.cohort), Some(8));
        assert!(!serde_json::to_string(&snap).unwrap().contains("PRIVATE"));
        // A calendar about another cohort says nothing about this window.
        v["calendar"]["current"]["cohort"] = json!(6);
        assert_eq!(snap_of(&v).admitting.map(|a| a.cohort), Some(7));
    }

    /// The snapshot carries none of the private fields in the status it was
    /// built from: no spend, generation, document, ledger or fault detail.
    #[test]
    fn snapshot_is_redacted() {
        let o = observed("scoring");
        let snap = snapshot(&SnapshotInputs {
            chain: &chain(),
            observed: &o,
            admission_origin: None,
            operator_notice: Some("Maintenance tonight."),
            last_published_cohort: None,
            now_ms: 0,
        });
        let text = serde_json::to_string(&snap).unwrap();
        for marker in SECRET_MARKERS {
            assert!(!text.contains(marker), "{marker} leaked: {text}");
        }
        assert!(
            !text.contains("1050") && !text.contains("1000\""),
            "no ledger figure"
        );
        crate::redact::scan_json(text.as_bytes()).unwrap();
        assert_eq!(o.eval.as_ref().unwrap().ledger_over_threshold, Some(true));
    }

    /// A reference or target in the status that is not a 64-hex digest never
    /// reaches the snapshot.
    #[test]
    fn non_digest_references_are_dropped() {
        let mut v = eval_status("admission");
        v["reward"]["references"]["current_content"] = json!("PRIVATE-DOCUMENT-TEXT");
        v["reward"]["active_target"] = json!("0x".to_owned() + &"AB".repeat(32));
        let o = Observed {
            eval: Some(parse_eval(&v)),
            ..observed("admission")
        };
        let snap = snapshot(&SnapshotInputs {
            chain: &chain(),
            observed: &o,
            admission_origin: None,
            operator_notice: None,
            last_published_cohort: None,
            now_ms: 0,
        });
        assert_eq!(snap.reference, None);
        assert_eq!(snap.active_target, None, "uppercase hex is not a digest");
        assert!(!serde_json::to_string(&snap).unwrap().contains("PRIVATE"));
    }

    /// The typed parse reads the same fields from bytes as from a value, and
    /// an unexpected shape is no status at all.
    #[test]
    fn typed_parse_from_bytes() {
        let bytes = serde_json::to_vec(&eval_status("scoring")).unwrap();
        assert_eq!(
            parse_eval_bytes(&bytes),
            Some(parse_eval(&eval_status("scoring")))
        );
        let w = serde_json::to_vec(&worker_status("primary", true)).unwrap();
        let view = parse_worker_bytes(&w).unwrap();
        assert_eq!(view.fault.as_deref(), Some("parent_conflict"));
        assert_eq!(view.signing.as_ref().unwrap().ops.len(), 1);
        assert_eq!(parse_eval_bytes(b"[1, 2]"), None);
        assert_eq!(parse_worker_bytes(b"not json"), None);
    }

    #[test]
    fn faults_and_unreachable_sources() {
        let mut o = observed("scoring");
        o.coordinator = Some(parse_worker(&worker_status("fallback", true)));
        let snap = snapshot(&SnapshotInputs {
            chain: &chain(),
            observed: &o,
            admission_origin: None,
            operator_notice: None,
            last_published_cohort: None,
            now_ms: 0,
        });
        assert!(snap.fault);
        assert!(
            !serde_json::to_string(&snap)
                .unwrap()
                .contains("parent_conflict")
        );
        let none = Observed::default();
        let snap = snapshot(&SnapshotInputs {
            chain: &chain(),
            observed: &none,
            admission_origin: None,
            operator_notice: None,
            last_published_cohort: None,
            now_ms: 0,
        });
        assert!(snap.fault);
        assert_eq!(snap.phase.as_deref(), Some("unknown"));
        assert!(snap.notice.unwrap().starts_with(UNREACHABLE_NOTICE));
        assert_eq!(
            snap.sources,
            Sources {
                coordinator: false,
                eval: false
            }
        );
    }
}
