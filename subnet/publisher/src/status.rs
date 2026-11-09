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

use std::collections::BTreeMap;

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
    /// The status gate's `access` report (`None` from a Worker built before
    /// the gate). Private: alert rules only, never the public snapshot.
    pub access: Option<AccessView>,
    /// The coordinator's registry read (`chain_journal.registry`, rule
    /// [`REGISTRY_RULE`]): `None` when the status carries no such field (a
    /// coordinator built before it), `Some(None)` when the field is there
    /// but null, refused or not whole and well formed.
    pub registry: Option<Option<RegistryView>>,
}

/// The coordinator's registry rule this publisher reads.
pub const REGISTRY_RULE: &str = "slop_ninja.coordinator.registry/v1";

/// Every registered UID of the subnet as the coordinator's latest signing
/// operation read them (`Keys` and `BlockAtRegistration` below
/// `SubnetworkN`) at the confirmed block `block` of epoch `epoch`: rows in
/// strictly ascending UID order, each hotkey once, at most
/// `contract::registry::ROWS` rows, each registered at or before `block`.
/// Public chain facts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegistryView {
    pub epoch: u64,
    pub block: u64,
    /// `(uid, hotkey, registration_block)`.
    pub rows: Vec<(u16, [u8; 32], u64)>,
}

/// The eval Worker's per-hotkey admission rule this publisher reads.
pub const ADMISSION_HOTKEYS_RULE: &str = "slop_ninja.eval.admission_hotkeys/v1";

/// The per-hotkey admission states the eval Worker reports, in the order a
/// hotkey moves through them.
pub const ADMISSION_STATES: &[&str] = &[
    "none",
    "committed",
    "uploading",
    "verifying",
    "verified",
    "refused",
    "overflow",
    "admitted",
];

/// The closed refusal classes (the eval Worker maps each refusal code to
/// one; no code or detail leaves it).
pub const REFUSAL_CLASSES: &[&str] = &["commitment", "artifact", "cutoff", "registration"];

/// One admission cohort as `/eval/status` reports it
/// (`admission.cohorts[]`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdmissionCohortView {
    pub cohort: u64,
    /// The epoch its roster froze at; `None` before the freeze.
    pub frozen_epoch: Option<u64>,
    /// Its per-hotkey states (`hotkeys`, rule [`ADMISSION_HOTKEYS_RULE`]):
    /// `None` when absent, refused or not whole and well formed.
    pub hotkeys: Option<HotkeysView>,
    /// Its entrants' scoring counts (`progress`, rule
    /// [`SCORING_PROGRESS_RULE`]): `None` when absent, refused, for another
    /// cohort, or not whole and well formed.
    pub progress: Option<ScoringProgressView>,
}

/// The eval Worker's per-entrant scoring progress rule this publisher reads.
pub const SCORING_PROGRESS_RULE: &str = "slop_ninja.eval.scoring_progress/v1";

/// A frozen cohort's per-entrant scoring counts, read with the status:
/// whether its jobs exist yet (`sealed`), and for each entrant job its
/// hotkey, whether its scoring started, and its resolved and total
/// documents. Counts only: the eval Worker reports nothing else, and
/// anything else in a row is not read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScoringProgressView {
    pub sealed: bool,
    /// Ascending hotkeys, each once, at most `contract::roster::ENTRANTS`.
    pub rows: Vec<ScoringProgressRow>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScoringProgressRow {
    pub hotkey: [u8; 32],
    pub started: bool,
    pub done: u32,
    /// At least 1 and at least `done`.
    pub total: u32,
}

impl ScoringProgressView {
    pub fn row(&self, hotkey: &[u8; 32]) -> Option<&ScoringProgressRow> {
        self.rows.binary_search_by(|r| r.hotkey.cmp(hotkey)).ok().and_then(|i| self.rows.get(i))
    }
}

/// A cohort's per-hotkey admission states as of the tempo boundary `epoch`
/// (first block `block`): each hotkey of the UID map at that boundary and
/// each hotkey with a session of the cohort, once, at most
/// `contract::roster::ROWS` rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HotkeysView {
    pub epoch: u64,
    pub block: u64,
    pub rows: Vec<HotkeyRow>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HotkeyRow {
    pub hotkey: [u8; 32],
    /// Its UID at the boundary, when it holds one.
    pub uid: Option<u16>,
    /// The block of its logged commitment naming the cohort.
    pub committed_block: Option<u64>,
    /// One of [`ADMISSION_STATES`].
    pub state: String,
    /// One of [`REFUSAL_CLASSES`], exactly when `state` is `refused`.
    pub refusal_class: Option<String>,
}

/// A Worker's `access` report: which status tokens it has installed (never
/// their values) and whether they all differ.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessView {
    pub tokens_distinct: bool,
    pub publisher_token: bool,
    pub alerts_token: bool,
    pub shared_status_token: bool,
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
    /// The signed coordinator row validators copy
    /// (`signing.row_record`): no record covering the installed
    /// authorization yet, or a signing failure; `None` while the record is
    /// current, or from a coordinator built before it.
    pub row_record: Option<RowRecordStall>,
    /// The installed authorization's issue epoch
    /// (`signing.authorization.issued_epoch`).
    pub issued_epoch: Option<u64>,
    /// `signing.health`: `ok`, `backing_off`, `failing`, `diagnostic`,
    /// `paused` or `fault`.
    pub health: Option<String>,
    /// The signer-wide backoff still to wait (`signing.backoff_ms`).
    pub backoff_ms: Option<u64>,
    /// The signer's last error code and its age (`signing.last_error`,
    /// `signing.last_error_age_ms`).
    pub last_error: Option<String>,
    pub last_error_age_ms: Option<u64>,
    /// Age of the newest row of the sign log (`signing.log[].age_ms`): the
    /// signer's last recorded progress. `None` when the log is empty or
    /// absent.
    pub last_event_age_ms: Option<u64>,
}

/// Why no signed coordinator row covers the installed authorization, as
/// `/status` reports it (`signing.row_record.pending_*` or `error`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowRecordStall {
    /// The waiting or failure code.
    pub code: String,
    /// How long it has waited or failed.
    pub age_ms: u64,
    /// No record covers the installed authorization (as opposed to a
    /// failure while the record still covers it).
    pub uncovered: bool,
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
    /// The commit extrinsic's index in `commit_block` (a public chain fact;
    /// the coordinator's type, narrowed where it is published).
    pub commit_index: Option<u64>,
    /// The block whose events carry this operation's
    /// `TimelockedWeightsRevealed` (a public chain fact).
    pub reveal_block: Option<u64>,
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
    /// What raised the cohort's fault (`fault_cause`: a scorer failure
    /// class, a definitive refusal, an admission subject), when reported.
    pub fault_cause: Option<String>,
    /// Whether the coordinator reports that the cohort's fault does not
    /// clear by itself (`operator_clear_needed`): an operator clear or an
    /// abandonment is needed. Absent from coordinators that do not report it.
    pub operator_clear_needed: Option<bool>,
    /// Jobs whose attempts no further extension fits under the receipt bound
    /// (`receipt_bound_reached_jobs`).
    pub receipt_bound_reached_jobs: Vec<u64>,
    /// What the public dashboard reads of the cohort ([`crate::dashboard`]).
    pub dash: CohortDash,
}

/// The fields of one `/eval/status` cohort the dashboard reads: the phase
/// log without its notes, the epochs and credited marks of its boundaries,
/// and the aggregate work counts (E1). Each is `None` (or empty) from
/// coordinators that do not report it. The counts are private: the
/// dashboard publishes only a 10% grid percent of them, read at tempo
/// boundaries ([`crate::dashboard`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CohortDash {
    pub opened_block: Option<u64>,
    pub packaged_epoch: Option<u64>,
    pub closed_epoch: Option<u64>,
    /// The cohort runs under reward-v1 (`reward_v1` is not null): its
    /// results are published in a boundary step after the package commits.
    pub reward_v1: bool,
    /// `reward_v1.probe_build`: a probe build, never part of the record.
    pub probe_build: bool,
    /// `reward_v1.published_epoch`: the boundary its results were published at.
    pub published_epoch: Option<u64>,
    /// The epoch the coordinator admission window of this cohort closed at
    /// (`window_closed_epoch`, E1).
    pub window_closed_epoch: Option<u64>,
    /// The credited-tempo count at publication (`published_active`, E1).
    pub published_active: Option<u64>,
    /// The plan's roster cap (`roster_cap`, E1).
    pub roster_cap: Option<u64>,
    /// Aggregate work counts (`progress`, E1).
    pub work: Option<WorkCounts>,
    /// `self_clearing`: the cohort's fault clears by itself.
    pub self_clearing: Option<bool>,
    /// The phase log (`log[]`), in order, without notes.
    pub log: Vec<PhaseMark>,
    /// The time the cohort opened (`opened_ms`, the opening boundary step).
    pub opened_ms: Option<u64>,
    /// The admission cutoff's block (`cutoff_block`).
    pub cutoff_block: Option<u64>,
    /// The tempo boundaries the cohort's dates use, by epoch
    /// (`boundaries[]`, resolved from the engine's boundary journal). Empty
    /// from coordinators that do not report them.
    pub boundaries: BTreeMap<u64, BoundaryView>,
}

/// One resolved tempo boundary of `boundaries[]`: its block and time, each
/// with how it is known (`observed` or `derived`), and the credited-tempo
/// count the journal records at it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoundaryView {
    pub block: Option<u64>,
    pub block_basis: Option<String>,
    pub at_ms: Option<u64>,
    pub at_basis: Option<String>,
    pub active: Option<u64>,
    /// Why `at_ms` is `None`: `before_tracking` (no journal row at or below
    /// it) or `not_derivable` (rows around it, none supporting a time).
    pub at_reason: Option<String>,
}

/// One `log[]` entry: the phase the cohort was in, the stored epoch and the
/// commit time. The note is never parsed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PhaseMark {
    pub phase: String,
    pub epoch: Option<u64>,
    pub at_ms: Option<u64>,
}

/// A cohort's aggregate work counts (`progress`, E1): generation documents
/// ready of those not failed, scoring chunks done of those planned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkCounts {
    pub generation: Option<(u64, u64)>,
    pub scoring: Option<(u64, u64)>,
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
    /// The block of the last observed tempo boundary
    /// (`observed.epoch.last_epoch_block`).
    pub boundary_block: Option<u64>,
    /// The stored epoch was observed while it was the chain head's epoch
    /// (`observed.epoch_live`).
    pub epoch_live: Option<bool>,
    /// Admitted entrants per cohort (`admission.cohorts[].admitted`).
    pub admitted: Vec<(u64, u64)>,
    /// The subnet's UID-to-hotkey map as of a tempo boundary (`uid_map`,
    /// rule [`UID_MAP_RULE`]): `None` when absent, refused by the
    /// coordinator, or not whole and well formed.
    pub uid_map: Option<UidMapView>,
    /// The admission cohorts (`admission.cohorts[]`) with their freeze and,
    /// when reported, per-hotkey states.
    pub admission: Vec<AdmissionCohortView>,
}

/// The coordinator's UID map rule this publisher reads.
pub const UID_MAP_RULE: &str = "slop_ninja.eval.uid_map/v1";

/// The subnet's UID-to-hotkey map (public chain facts from the
/// coordinator's commitment log) as of the tempo boundary `epoch`, whose
/// first block is `block`: rows in strictly ascending UID order, each
/// hotkey once, at most `contract::uid_map::ROWS` rows. Commitments after
/// `block` are left out, but one at or before it that the coordinator logs
/// later can still change the map under the same `epoch` and `block`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UidMapView {
    pub epoch: u64,
    pub block: u64,
    pub rows: Vec<(u16, [u8; 32])>,
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
    /// The standing template is paused (`template.paused`).
    pub template_paused: Option<bool>,
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
    /// The epoch the coordinator admission window closed at
    /// (`window_closed_epoch`, E1).
    pub window_closed_epoch: Option<u64>,
    /// The credit-adjusted earliest cutoff while the window is open
    /// (`earliest_cutoff_epoch`, E1).
    pub earliest_cutoff_epoch: Option<u64>,
    /// The credit-adjusted earliest close (`earliest_close_epoch_credited`,
    /// E1).
    pub earliest_close_epoch_credited: Option<u64>,
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
    /// The running cohort's live values (`/eval/live`), read only at the
    /// `live` roster level; `None` otherwise or when not whole.
    pub live: Option<LiveScoresView>,
}

// The status documents as typed views: only the fields named here are
// parsed; every other field is skipped without building a JSON tree, so a
// status body's memory is its own bytes (see `arena::status_body_bound`).

#[derive(Deserialize, Default)]
#[serde(default)]
struct ServiceJson {
    chain_journal: Option<ChainJournalJson>,
    access: Option<AccessJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AccessJson {
    tokens_distinct: Option<bool>,
    publisher_token: Option<bool>,
    alerts_token: Option<bool>,
    shared_status_token: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ChainJournalJson {
    mode: Option<String>,
    mode_age_ms: Option<u64>,
    journal: Option<JournalJson>,
    signing: Option<SigningJson>,
    watchdog: Option<WatchdogJson>,
    /// Present (even `null`) only from a coordinator that reports it.
    #[serde(deserialize_with = "present")]
    registry: Option<Lenient<RegistryJson>>,
}

/// A field that may be malformed without failing the status it sits in:
/// anything that is not the expected shape reads as [`Lenient::Other`].
#[derive(Deserialize)]
#[serde(untagged)]
enum Lenient<T> {
    Ok(T),
    Other(serde::de::IgnoredAny),
}

impl<T> Lenient<T> {
    fn ok(self) -> Option<T> {
        match self {
            Lenient::Ok(t) => Some(t),
            Lenient::Other(_) => None,
        }
    }
}

/// A key that is present, whatever its value (`null` included).
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

#[derive(Deserialize)]
struct RegistryJson {
    rule: Option<String>,
    epoch: Option<u64>,
    block: Option<u64>,
    rows: Option<Vec<RegistryRowJson>>,
}

#[derive(Deserialize)]
struct RegistryRowJson {
    uid: u64,
    hotkey: String,
    registration_block: u64,
}

/// A 32-byte key as 64 lowercase hex characters.
fn key64(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    let mut key = [0u8; 32];
    hex::decode_to_slice(s, &mut key).ok()?;
    Some(key)
}

/// The registry only when it is whole: the known rule, its epoch and block,
/// rows within the contract's bound in strictly ascending UID order, each
/// hotkey once and registered at or before the block. Anything else drops
/// the whole read.
fn registry_view(j: RegistryJson) -> Option<RegistryView> {
    if j.rule.as_deref() != Some(REGISTRY_RULE) {
        return None;
    }
    let (epoch, block, rows) = (j.epoch?, j.block?, j.rows?);
    if rows.len() > crate::dashboard::contract::registry::ROWS {
        return None;
    }
    let mut out: Vec<(u16, [u8; 32], u64)> = Vec::with_capacity(rows.len());
    let mut keys = std::collections::BTreeSet::new();
    for r in rows {
        let uid = u16::try_from(r.uid).ok()?;
        let key = key64(&r.hotkey)?;
        if out.last().is_some_and(|(u, _, _)| *u >= uid) || !keys.insert(key) || r.registration_block > block {
            return None;
        }
        out.push((uid, key, r.registration_block));
    }
    Some(RegistryView { epoch, block, rows: out })
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
    row_record: Option<RowRecordJson>,
    health: Option<String>,
    backoff_ms: Option<u64>,
    last_error: Option<String>,
    last_error_age_ms: Option<u64>,
    log: Vec<SignLogJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SignLogJson {
    age_ms: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RowRecordJson {
    error: Option<String>,
    error_age_ms: Option<u64>,
    pending_age_ms: Option<u64>,
    pending_code: Option<String>,
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
    issued_epoch: Option<u64>,
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
    commit_index: Option<u64>,
    reveal_block: Option<u64>,
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
    admission: Option<AdmissionJson>,
    uid_map: Option<UidMapJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UidMapJson {
    rule: Option<String>,
    epoch: Option<u64>,
    block: Option<u64>,
    rows: Option<Vec<UidRowJson>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UidRowJson {
    uid: Option<u64>,
    hotkey: Option<String>,
}

/// The map only when it is whole: the known rule, its boundary, rows within
/// the contract's bound in strictly ascending UID order, each hotkey 64 hex
/// characters and named once. Anything else drops the whole map (a partial
/// map could resolve a UID to the wrong hotkey).
fn uid_map_view(j: UidMapJson) -> Option<UidMapView> {
    if j.rule.as_deref() != Some(UID_MAP_RULE) {
        return None;
    }
    let rows = j.rows?;
    if rows.len() > crate::dashboard::contract::uid_map::ROWS {
        return None;
    }
    let mut out: Vec<(u16, [u8; 32])> = Vec::with_capacity(rows.len());
    let mut keys = std::collections::BTreeSet::new();
    for r in rows {
        let uid = u16::try_from(r.uid?).ok()?;
        let mut key = [0u8; 32];
        let hex_key = r.hotkey?;
        if hex_key.len() != 64 || !hex_key.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return None;
        }
        hex::decode_to_slice(&hex_key, &mut key).ok()?;
        if out.last().is_some_and(|(u, _)| *u >= uid) || !keys.insert(key) {
            return None;
        }
        out.push((uid, key));
    }
    Some(UidMapView { epoch: j.epoch?, block: j.block?, rows: out })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AdmissionJson {
    cohorts: Vec<AdmissionCohortJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AdmissionCohortJson {
    cohort: Option<u64>,
    admitted: Option<u64>,
    frozen_epoch: Option<u64>,
    hotkeys: Option<Lenient<HotkeysJson>>,
    progress: Option<Lenient<ScoringProgressJson>>,
}

#[derive(Deserialize)]
struct ScoringProgressJson {
    rule: Option<String>,
    cohort: Option<u64>,
    sealed: Option<bool>,
    rows: Option<Vec<ScoringProgressRowJson>>,
}

#[derive(Deserialize)]
struct ScoringProgressRowJson {
    hotkey: String,
    started: bool,
    done: u64,
    total: u64,
}

/// The counts only when whole: the known rule and this cohort, rows within
/// the roster's entrant bound in strictly ascending hotkey order, none
/// before the seal, each with `1 <= total`, `done <= total` and no done
/// document before its scoring started. Anything else drops them all.
fn progress_view(j: ScoringProgressJson, cohort: u64) -> Option<ScoringProgressView> {
    if j.rule.as_deref() != Some(SCORING_PROGRESS_RULE) || j.cohort != Some(cohort) {
        return None;
    }
    let (sealed, rows) = (j.sealed?, j.rows?);
    if rows.len() > crate::dashboard::contract::roster::ENTRANTS || (!sealed && !rows.is_empty()) {
        return None;
    }
    let mut out: Vec<ScoringProgressRow> = Vec::with_capacity(rows.len());
    for r in rows {
        let hotkey = key64(&r.hotkey)?;
        let (done, total) = (u32::try_from(r.done).ok()?, u32::try_from(r.total).ok()?);
        if total == 0 || done > total || (done > 0 && !r.started) || out.last().is_some_and(|p| p.hotkey >= hotkey) {
            return None;
        }
        out.push(ScoringProgressRow { hotkey, started: r.started, done, total });
    }
    Some(ScoringProgressView { sealed, rows: out })
}

#[derive(Deserialize)]
struct HotkeysJson {
    rule: Option<String>,
    epoch: Option<u64>,
    block: Option<u64>,
    rows: Option<Vec<HotkeyRowJson>>,
}

#[derive(Deserialize)]
struct HotkeyRowJson {
    hotkey: String,
    uid: Option<u64>,
    committed_block: Option<u64>,
    state: String,
    refusal_class: Option<String>,
}

/// The per-hotkey states only when they are whole: the known rule, its
/// boundary, at most the roster bound's rows, each hotkey once with a state
/// from the closed list and a refusal class from the closed list exactly
/// when refused, distinct UIDs. Anything else drops them all.
fn hotkeys_view(j: HotkeysJson) -> Option<HotkeysView> {
    if j.rule.as_deref() != Some(ADMISSION_HOTKEYS_RULE) {
        return None;
    }
    let (epoch, block, rows) = (j.epoch?, j.block?, j.rows?);
    if rows.len() > crate::dashboard::contract::roster::ROWS {
        return None;
    }
    let mut keys = std::collections::BTreeSet::new();
    let mut uids = std::collections::BTreeSet::new();
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let hotkey = key64(&r.hotkey)?;
        let uid = match r.uid {
            Some(u) => Some(u16::try_from(u).ok()?),
            None => None,
        };
        let state = ADMISSION_STATES.iter().find(|s| **s == r.state)?;
        let class_ok = match r.refusal_class.as_deref() {
            Some(c) => *state == "refused" && REFUSAL_CLASSES.contains(&c),
            None => *state != "refused",
        };
        if !class_ok || !keys.insert(hotkey) || uid.is_some_and(|u| !uids.insert(u)) {
            return None;
        }
        out.push(HotkeyRow {
            hotkey,
            uid,
            committed_block: r.committed_block,
            state: (*state).to_owned(),
            refusal_class: r.refusal_class,
        });
    }
    Some(HotkeysView { epoch, block, rows: out })
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
    template: Option<TemplateJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TemplateJson {
    paused: Option<bool>,
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
    window_closed_epoch: Option<u64>,
    earliest_cutoff_epoch: Option<u64>,
    earliest_close_epoch_credited: Option<u64>,
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
    epoch_live: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EpochJson {
    index: Option<u64>,
    last_epoch_block: Option<u64>,
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
    fault_cause: Option<String>,
    operator_clear_needed: Option<bool>,
    receipt_bound_reached_jobs: Option<Vec<u64>>,
    opened_block: Option<u64>,
    packaged_epoch: Option<u64>,
    closed_epoch: Option<u64>,
    published_active: Option<u64>,
    roster_cap: Option<u64>,
    progress: Option<ProgressJson>,
    self_clearing: Option<bool>,
    reward_v1: Option<CohortRewardJson>,
    window_closed_epoch: Option<u64>,
    log: Vec<LogJson>,
    opened_ms: Option<u64>,
    cutoff_block: Option<u64>,
    boundaries: Vec<BoundaryJson>,
}

/// A `boundaries[]` entry; a basis outside `observed`/`derived` drops its
/// value.
#[derive(Deserialize, Default)]
#[serde(default)]
struct BoundaryJson {
    epoch: Option<u64>,
    block: Option<u64>,
    block_basis: Option<String>,
    at_ms: Option<u64>,
    at_basis: Option<String>,
    active: Option<u64>,
    at_reason: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ProgressJson {
    generation: Option<ReadyJson>,
    scoring: Option<DoneJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReadyJson {
    ready: Option<u64>,
    total: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DoneJson {
    done: Option<u64>,
    total: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CohortRewardJson {
    published_epoch: Option<u64>,
    probe_build: Option<bool>,
}

/// A phase log entry; its `note` is never parsed.
#[derive(Deserialize, Default)]
#[serde(default)]
struct LogJson {
    phase: Option<String>,
    at_ms: Option<u64>,
    epoch: Option<u64>,
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
    // Every flag is needed: a partial report decides nothing.
    let access = j.access.and_then(|a| {
        Some(AccessView {
            tokens_distinct: a.tokens_distinct?,
            publisher_token: a.publisher_token?,
            alerts_token: a.alerts_token?,
            shared_status_token: a.shared_status_token?,
        })
    });
    let c = j.chain_journal.unwrap_or_default();
    let journal_missing = c.journal.is_none();
    let journal = c.journal.unwrap_or_default();
    let registry = c.registry.map(|r| r.ok().and_then(registry_view));
    WorkerView {
        registry,
        access,
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
            // An uncovered authorization first (validators wait on it);
            // else a signing failure while the old record still covers it.
            let row_record = s
                .row_record
                .and_then(|r| match (r.pending_age_ms, r.error) {
                    (Some(age_ms), error) => Some(RowRecordStall {
                        code: r
                            .pending_code
                            .or(error)
                            .unwrap_or_else(|| "row_waiting".into()),
                        age_ms,
                        uncovered: true,
                    }),
                    (None, Some(code)) => Some(RowRecordStall {
                        code,
                        age_ms: r.error_age_ms.unwrap_or(0),
                        uncovered: false,
                    }),
                    (None, None) => None,
                });
            Some(SigningView {
                auto_target_error,
                row_record,
                issued_epoch: s.authorization.as_ref().and_then(|a| a.issued_epoch),
                health: s.health,
                backoff_ms: s.backoff_ms,
                last_error: s.last_error,
                last_error_age_ms: s.last_error_age_ms,
                last_event_age_ms: s.log.iter().filter_map(|l| l.age_ms).min(),
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
                            commit_index: o.commit_index,
                            reveal_block: o.reveal_block,
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
                fault_cause: c.fault_cause,
                operator_clear_needed: c.operator_clear_needed,
                receipt_bound_reached_jobs: c.receipt_bound_reached_jobs.unwrap_or_default(),
                dash: CohortDash {
                    opened_block: c.opened_block,
                    packaged_epoch: c.packaged_epoch,
                    closed_epoch: c.closed_epoch,
                    reward_v1: c.reward_v1.is_some(),
                    probe_build: c.reward_v1.as_ref().and_then(|r| r.probe_build) == Some(true),
                    published_epoch: c.reward_v1.as_ref().and_then(|r| r.published_epoch),
                    window_closed_epoch: c.window_closed_epoch,
                    published_active: c.published_active,
                    roster_cap: c.roster_cap,
                    work: c.progress.map(|p| WorkCounts {
                        generation: p.generation.and_then(|g| g.ready.zip(g.total)),
                        scoring: p.scoring.and_then(|g| g.done.zip(g.total)),
                    }),
                    self_clearing: c.self_clearing,
                    log: c
                        .log
                        .into_iter()
                        .filter_map(|l| {
                            Some(PhaseMark {
                                phase: l.phase?,
                                epoch: l.epoch,
                                at_ms: l.at_ms,
                            })
                        })
                        .collect(),
                    opened_ms: c.opened_ms,
                    cutoff_block: c.cutoff_block,
                    boundaries: c
                        .boundaries
                        .into_iter()
                        .filter_map(|b| {
                            let basis = |v: Option<u64>, k: Option<String>| match k.as_deref() {
                                Some("observed" | "derived") if v.is_some() => (v, k),
                                _ => (None, None),
                            };
                            let (block, block_basis) = basis(b.block, b.block_basis);
                            let (at_ms, at_basis) = basis(b.at_ms, b.at_basis);
                            let at_reason = b
                                .at_reason
                                .filter(|r| at_ms.is_none() && matches!(r.as_str(), "before_tracking" | "not_derivable"));
                            Some((b.epoch?, BoundaryView { block, block_basis, at_ms, at_basis, active: b.active, at_reason }))
                        })
                        .collect(),
                },
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
        boundary_block: j.observed.epoch.as_ref().and_then(|e| e.last_epoch_block),
        epoch_live: j.observed.epoch_live,
        admitted: j
            .admission
            .as_ref()
            .map(|a| a.cohorts.iter().filter_map(|c| c.cohort.zip(c.admitted)).collect())
            .unwrap_or_default(),
        admission: j
            .admission
            .map(|a| {
                a.cohorts
                    .into_iter()
                    .filter_map(|c| {
                        let cohort = c.cohort?;
                        Some(AdmissionCohortView {
                            cohort,
                            frozen_epoch: c.frozen_epoch,
                            hotkeys: c.hotkeys.and_then(Lenient::ok).and_then(hotkeys_view),
                            progress: c.progress.and_then(Lenient::ok).and_then(|p| progress_view(p, cohort)),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        epoch: j.observed.epoch.and_then(|e| e.index),
        uid_map: j.uid_map.and_then(uid_map_view),
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
                    window_closed_epoch: x.window_closed_epoch,
                    earliest_cutoff_epoch: x.earliest_cutoff_epoch,
                    earliest_close_epoch_credited: x.earliest_close_epoch_credited,
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
            template_paused: c.template.and_then(|t| t.paused),
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

/// The active cohort's phase as published. Admission opens at a tempo
/// boundary and ends at the roster freeze, and a reward cohort closes at a
/// tempo boundary; every phase in between (`generating`, `sealing`,
/// `scoring`, `packaging`, `transition`) moves when its work commits, so
/// publishing those names once a minute would time the hidden work (how
/// long generation and scoring took, which grows with the batch's documents
/// and an artifact's crashes). They are all published as `evaluating`. An
/// unknown phase is `evaluating` too, so a new work phase never reaches the
/// snapshot by name.
///
/// This hides only the phase name. Other public signals still move when
/// hidden work commits: a probe build closes from packaging, mid-tempo, so
/// its `evaluating` ends then; a cohort's records are published (and
/// `last_published_cohort` moves) once its package is durable, at the start
/// of `transition`; and `fault` turns on when a step fails.
pub fn public_phase(phase: &str) -> &'static str {
    match phase {
        "admission" => "admission",
        _ => "evaluating",
    }
}

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
            (Some(c), _) => public_phase(&c.phase).to_owned(),
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
                    {"epoch": 23_169, "state": "committed", "commit_block": 8_339_800, "commit_index": 7, "reveal_weights_match": true,
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
            live: None,
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

    /// `signing.row_record`: an uncovered authorization first (with its
    /// code and age), else a signing failure; a current record is nothing.
    #[test]
    fn parses_the_coordinator_row_records_wait_and_failure() {
        let with = |row: Value| {
            let mut j = worker_status("primary", false);
            j["chain_journal"]["signing"]["row_record"] = row;
            parse_worker(&j).signing.unwrap().row_record
        };
        assert_eq!(
            with(json!({"epoch": 12, "error": null, "pending_authorization": null})),
            None
        );
        let v = with(json!({"pending_authorization": "ab", "pending_age_ms": 5_000, "pending_code": "row_waiting_issue",
            "error": "authorization_key_missing", "error_age_ms": 9}))
        .unwrap();
        assert_eq!(
            (v.code.as_str(), v.age_ms, v.uncovered),
            ("row_waiting_issue", 5_000, true)
        );
        let v = with(json!({"error": "authorization_key_missing", "error_age_ms": 9})).unwrap();
        assert_eq!(
            (v.code.as_str(), v.age_ms, v.uncovered),
            ("authorization_key_missing", 9, false)
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

    /// Work phases move when their work commits, so the snapshot names none
    /// of them (`public_phase` lists the signals this does not hide).
    #[test]
    fn work_phases_are_published_as_evaluating() {
        for phase in [
            "generating",
            "sealing",
            "scoring",
            "packaging",
            "transition",
            "a_later_phase",
        ] {
            let o = observed(phase);
            let snap = snapshot(&SnapshotInputs {
                chain: &chain(),
                observed: &o,
                admission_origin: None,
                operator_notice: None,
                last_published_cohort: None,
                now_ms: 0,
            });
            assert_eq!(snap.phase.as_deref(), Some("evaluating"), "{phase}");
            let text = serde_json::to_string(&snap).unwrap();
            assert!(
                !text.contains(phase),
                "{phase} reached the snapshot: {text}"
            );
            // The alert rules still see the coordinator's own phase.
            assert_eq!(o.eval.as_ref().unwrap().active().unwrap().phase, phase);
        }
        assert_eq!(public_phase("admission"), "admission");
        let o = observed("admission");
        let snap = snapshot(&SnapshotInputs {
            chain: &chain(),
            observed: &o,
            admission_origin: None,
            operator_notice: None,
            last_published_cohort: None,
            now_ms: 0,
        });
        assert_eq!(snap.phase.as_deref(), Some("admission"));
    }

    /// The status gate's `access` report is parsed for the alert rules
    /// (a partial report is none) and never reaches the public snapshot.
    #[test]
    fn the_access_report_is_private() {
        let mut v = worker_status("primary", false);
        v["access"] = json!({"caller": "publisher", "operator_token": true, "monitor_token": true,
            "publisher_token": true, "alerts_token": false, "shared_status_token": true,
            "tokens_distinct": false, "internal_host": "status.internal"});
        let w = parse_worker(&v);
        assert_eq!(
            w.access,
            Some(AccessView {
                tokens_distinct: false,
                publisher_token: true,
                alerts_token: false,
                shared_status_token: true,
            })
        );
        let mut partial = v.clone();
        partial["access"] = json!({"tokens_distinct": true});
        assert_eq!(parse_worker(&partial).access, None);
        assert_eq!(parse_worker(&worker_status("primary", false)).access, None);
        let o = Observed {
            coordinator: Some(w.clone()),
            eval_worker: Some(w),
            ..observed("scoring")
        };
        let snap = snapshot(&SnapshotInputs {
            chain: &chain(),
            observed: &o,
            admission_origin: None,
            operator_notice: None,
            last_published_cohort: None,
            now_ms: 0,
        });
        let text = serde_json::to_string(&snap).unwrap();
        for field in ["access", "token", "tokens_distinct", "status.internal"] {
            assert!(
                !text.contains(field),
                "{field} reached the snapshot: {text}"
            );
        }
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

// ------------------------------------------------------------ live values

/// The eval Worker's live values report this publisher reads
/// (`GET /eval/live`), read only at the `live` roster level.
pub const LIVE_SCORES_RULE: &str = "slop_ninja.eval.live_scores/v1";

/// One running cohort's live values (`live-v1` readings and pre-finalized
/// scores), whole and checked ([`parse_live_bytes`]), or nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveScoresView {
    pub cohort: u64,
    /// The live block's parameters and counted bits (recomputed here).
    pub n: u32,
    pub predecessor: bool,
    pub eps_live_milli: u32,
    /// `L`: `0` or at least 2.
    pub segments: u8,
    /// The live grid (`None` without readings).
    pub width: Option<u64>,
    pub noise_sd: Option<String>,
    pub stopped: bool,
    pub bits: (u32, u32, u64),
    pub reference: Option<LiveReferenceView>,
    pub prefinal: Option<LivePrefinalView>,
    /// Ascending hotkeys, each once: one per executed entrant.
    pub rows: Vec<LiveRowView>,
    /// `prepare`, `release` or `over_bound`.
    pub fault: Option<String>,
    /// Hotkeys with a recorded `prefinal_mismatch` (values stay private).
    pub defects: Vec<[u8; 32]>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveReferenceView {
    /// Batch tasks of `R_k`'s completed segments.
    pub done: u32,
    pub total: u32,
    /// Grid decimals or `none`.
    pub checkpoints: Vec<String>,
    pub stopped: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LivePrefinalView {
    /// The `quant-v1` grid.
    pub width: u64,
    pub current: String,
    pub predecessor: Option<String>,
    pub margin_line: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveRowView {
    pub hotkey: [u8; 32],
    /// Grid decimals or `withheld`.
    pub checkpoints: Vec<String>,
    pub stopped: bool,
    /// A `quant-v1` grid decimal or `withheld`.
    pub prefinal: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveJson {
    rule: String,
    cohort: Option<u64>,
    sealed: Option<bool>,
    policy: Option<LivePolicyJson>,
    reference: Option<LiveReferenceJson>,
    prefinal: Option<LivePrefinalJson>,
    rows: Option<Vec<LiveRowJson>>,
    fault: Option<String>,
    defects: Vec<LiveDefectJson>,
    #[serde(default)]
    refused: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LivePolicyJson {
    rule: String,
    n: u32,
    predecessor: bool,
    eps_live_milli: u32,
    segments: u8,
    released_max: u8,
    width: Option<String>,
    noise_sd: Option<String>,
    stopped: bool,
    prefinal: bool,
    bits: LiveBitsJson,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveBitsJson {
    counted: u32,
    live: u32,
    bound: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveReferenceJson {
    done: u32,
    total: u32,
    checkpoints: Vec<String>,
    stopped: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LivePrefinalJson {
    quantization: LiveQuantJson,
    current: String,
    predecessor: Option<String>,
    margin_line: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveQuantJson {
    rule: String,
    width: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveRowJson {
    hotkey: String,
    checkpoints: Vec<String>,
    stopped: bool,
    prefinal: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveDefectJson {
    rule: String,
    hotkey: Option<String>,
}

/// A grid width printed as a decimal: the one-digit ladder's value whose
/// own decimal it is.
fn ladder_width(text: &str) -> Option<u64> {
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    if whole.len() != 1 || frac.len() > 18 || !text.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let mut v: u64 = u64::from(whole.as_bytes()[0] - b'0') * slop_ninja_protocol::SCORE_SCALE;
    let mut f: u64 = 0;
    for c in frac.bytes() {
        f = f * 10 + u64::from(c - b'0');
    }
    v += f * 10u64.pow(18 - frac.len() as u32);
    let w = slop_ninja_protocol::quant::on_ladder(v).then_some(v)?;
    // The decimal is exactly the width's own spelling.
    let mut buf = [0u8; slop_ninja_protocol::quant::MAX_DECIMAL_LEN];
    (slop_ninja_protocol::quant::format_grid(w, w, &mut buf).ok()? == text).then_some(w)
}

/// A value on grid `w` with exactly its decimals, or one of `symbols`.
fn on_grid(text: &str, w: u64, symbols: &[&str]) -> bool {
    symbols.contains(&text) || slop_ninja_protocol::quant::parse_grid(text, w).is_ok()
}

/// `/eval/live` whole and checked, or `None`: the rule; a sealed running
/// cohort's policy whose `width`, `noise_sd` and live bits this publisher
/// recomputes from `n`, `predecessor`, `segments`, `eps_live_milli` and
/// `X` = the rows with the protocol's own functions; counted plus live
/// bits within the bound; ascending distinct hotkeys within the roster's
/// entrant bound; at most `released_max` checkpoints, each on the live grid
/// with exactly its decimals or `withheld` (entrants) / `none` (`R_k`); no
/// value at all while stopped; pre-finalized values on the `quant-v1` grid,
/// an entrant's only with `R_k`'s, and a margin line at or above it. Any
/// failure drops the live view whole; the counts survive.
pub fn parse_live_bytes(body: &[u8]) -> Option<LiveScoresView> {
    let j: LiveJson = serde_json::from_slice(body).ok()?;
    if j.rule != LIVE_SCORES_RULE || j.refused.is_some() || j.sealed != Some(true) {
        return None;
    }
    let cohort = j.cohort?;
    let p = j.policy?;
    let rows = j.rows?;
    use slop_ninja_protocol::live;
    if p.rule != live::LIVE_RULE || rows.len() > crate::dashboard::contract::roster::ENTRANTS {
        return None;
    }
    let x = u16::try_from(rows.len()).ok()?;
    let readings = p.segments >= live::MIN_SEGMENTS;
    if (p.segments == 1) || p.released_max != p.segments.saturating_sub(1) {
        return None;
    }
    let (width, noise_sd) = if readings {
        let w = live::width_live(p.n, p.segments, x, p.eps_live_milli).ok()?;
        let sd = live::format_milli(live::noise_sd_milli(p.n, p.segments, x, p.eps_live_milli).ok()?);
        if p.width.as_deref().and_then(ladder_width) != Some(w) || p.noise_sd.as_deref() != Some(sd.as_str()) {
            return None;
        }
        if live::live_bits(x, p.segments, w).ok()? != p.bits.live {
            return None;
        }
        (Some(w), Some(sd))
    } else {
        if p.width.is_some() || p.noise_sd.is_some() || p.bits.live != 0 {
            return None;
        }
        (None, None)
    };
    if u64::from(p.bits.counted) + u64::from(p.bits.live) > p.bits.bound && readings {
        return None;
    }
    let silent = p.stopped;
    let cps_ok = |cps: &[String], symbol: &str| -> bool {
        cps.len() <= usize::from(p.released_max)
            && (!silent || cps.is_empty())
            && match width {
                Some(w) => cps.iter().all(|c| on_grid(c, w, &[symbol])),
                None => cps.is_empty(),
            }
    };
    let reference = match j.reference {
        Some(r) => {
            if !cps_ok(&r.checkpoints, live::NONE) || r.done > r.total || r.total != p.n {
                return None;
            }
            Some(LiveReferenceView { done: r.done, total: r.total, checkpoints: r.checkpoints, stopped: r.stopped })
        }
        None => None,
    };
    let prefinal = match j.prefinal {
        Some(pf) => {
            let w = ladder_width(&pf.quantization.width)?;
            if silent || pf.quantization.rule != slop_ninja_protocol::quant::QUANT_RULE {
                return None;
            }
            let cur = slop_ninja_protocol::quant::parse_grid(&pf.current, w).ok()?;
            if pf.predecessor.as_deref().is_some_and(|v| slop_ninja_protocol::quant::parse_grid(v, w).is_err()) {
                return None;
            }
            if let Some(m) = &pf.margin_line
                && slop_ninja_protocol::quant::parse_grid(m, w).ok()? < cur
            {
                return None;
            }
            Some(LivePrefinalView { width: w, current: pf.current, predecessor: pf.predecessor, margin_line: pf.margin_line })
        }
        None => None,
    };
    if !p.prefinal && prefinal.is_some() {
        return None;
    }
    let mut out_rows: Vec<LiveRowView> = Vec::with_capacity(rows.len());
    for r in rows {
        let hotkey = key64(&r.hotkey)?;
        if out_rows.last().is_some_and(|q| q.hotkey >= hotkey) || !cps_ok(&r.checkpoints, live::WITHHELD) {
            return None;
        }
        if let Some(v) = &r.prefinal {
            // An entrant's value only with R_k's, on its grid.
            let pf = prefinal.as_ref()?;
            if !on_grid(v, pf.width, &[live::WITHHELD]) {
                return None;
            }
        }
        out_rows.push(LiveRowView { hotkey, checkpoints: r.checkpoints, stopped: r.stopped, prefinal: r.prefinal });
    }
    let mut defects = Vec::with_capacity(j.defects.len());
    for d in j.defects {
        if d.rule != "prefinal_mismatch" {
            return None;
        }
        defects.push(key64(d.hotkey.as_deref()?)?);
    }
    if j.fault.as_deref().is_some_and(|f| !matches!(f, "prepare" | "release" | "over_bound")) {
        return None;
    }
    Some(LiveScoresView {
        cohort,
        n: p.n,
        predecessor: p.predecessor,
        eps_live_milli: p.eps_live_milli,
        segments: p.segments,
        width,
        noise_sd,
        stopped: p.stopped,
        bits: (p.bits.counted, p.bits.live, p.bits.bound),
        reference,
        prefinal,
        rows: out_rows,
        fault: j.fault,
        defects,
    })
}
