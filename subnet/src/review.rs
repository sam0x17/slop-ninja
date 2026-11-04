//! Review of finalized failed weight attempts.
//!
//! A signed weight call that the chain includes but whose dispatch fails is a
//! finalized failure. rtao records it and hands the operation back; nothing
//! was committed, and the same signed bytes can never land again.
//!
//! Finalized recovery first reads the failure's receipt and records its
//! decoded dispatch error ([`FAILURE_DIAGNOSTIC_KEY`]). Until that read
//! succeeds the failure awaits decoding: recovery is incomplete and retries
//! at the next round, but nothing halts, because a receipt read can fail for
//! a passing RPC reason (a timeout, a lost connection). A read that cannot
//! clear by itself (the endpoint discarded the block's state, does not have
//! the block, or the block or its events do not decode) is recorded instead
//! as a diagnostic naming its class (`receipt_unavailable`), which halts for
//! review; it is read again at each later recovery round (each start), so an
//! endpoint that has the state again clears it without a review. Once
//! decoded, recovery halts on the failure until it is reviewed, because an
//! error the adapter does not recognize can mean a misconfiguration (a wrong
//! weights version, a deregistered hotkey) that would fail every later tempo
//! too. A receipt that contradicts rtao's recorded outcome is recorded as
//! such and halts the same way; its review is a distinct kind
//! ([`ReviewKind::ContradictionReviewed`]) that the operator must ask for
//! explicitly, since the call may have landed.
//!
//! A review is a permanent marker for the attempt ([`FAILURE_REVIEW_KEY`]),
//! kept in the operation record's extensions under the attempt's number:
//! rtao keeps signed attempts immutable apart from their recovery fields.
//! Nothing is deleted: the attempt, its outcome and its signed bytes stay in
//! the journal as evidence. Two paths write it:
//!
//! - the offline journal tool's `review-failure`, after an operator has looked
//!   at the failure ([`ReviewKind::OperatorReviewed`]);
//! - finalized recovery, for a failure whose dispatch error, decoded by name
//!   through the inclusion block's own metadata, is an ordinary refusal of a
//!   weight call that clears by itself ([`SETTLED_REFUSALS`]):
//!   [`ReviewKind::Settled`], retained as a diagnostic.
//!
//! Recovery and resume count only failures without a marker. Once every
//! failure of a handed-back operation is reviewed, the operation is also
//! marked retired ([`crate::RETIRED_KEY`]), so the application moves on to
//! its next tempo instead of waiting for a read-back that cannot come.
//!
//! Builds before this review existed count every finalized failure,
//! reviewed or not, and halt on it with no way to review it, so a journal
//! that retains one is roll-forward only
//! ([`crate::JournalControls::rollback_blockers`] reports it).
//!
//! No runtime version is compared: the error is identified by its pallet and
//! variant names, whatever indices the runtime assigns them.

use rtao_client::journal::{AttemptOutcome, OperationRecord, SignedAttempt};
use rtao_primitives::H256;
use serde::{Deserialize, Serialize};

/// Operation record extension: an object from attempt number (decimal
/// string) to the permanent [`FailureReview`] of that finalized failed attempt.
pub const FAILURE_REVIEW_KEY: &str = "slopninja.failure-review.v1";

/// Operation record extension: an object from attempt number to the
/// [`FailureDiagnostic`] of that finalized failed attempt, read once from the
/// chain by finalized recovery. Informational; whether it settles the
/// failure is decided by [`SETTLED_REFUSALS`].
pub const FAILURE_DIAGNOSTIC_KEY: &str = "slopninja.failure-diagnostic.v1";

/// Identity of the rule that settles known refusals, recorded in every
/// automatic review. A change to [`SETTLED_REFUSALS`] gets a new identity.
pub const SETTLED_REFUSALS_RULE: &str = "slopninja.settled-weight-refusals.v1";

/// Pallet whose errors [`SETTLED_REFUSALS`] names, matched by name.
pub const SETTLED_PALLET: &str = "SubtensorModule";

/// Module errors of a weight call that settle automatically. Each is a
/// refusal the chain makes before any effect, and each is expected to clear
/// without operator action, so retrying at a later tempo is the right
/// response:
///
/// - `CommittingWeightsTooFast`, `SettingWeightsTooFast`: the hotkey's weights
///   rate limit; a later tempo is outside it.
/// - `NotEnoughStakeToSetWeights`: the hotkey's stake is below the weights
///   minimum, ordinary for a new validator until its stake arrives.
///
/// A settled refusal is still reported: recovery returns it in
/// [`SettleReport::refusals`] and the validator logs it as an alert, so a
/// refusal that never clears stays visible.
///
/// Every other error needs review, including `TooManyUnrevealedCommits` and
/// `NeuronNoValidatorPermit` (a commit queue that never drains or a permit the
/// stake rank never earns can be a lasting misconfiguration), a wrong weights
/// version, a disabled commit-reveal flag, a deregistered hotkey, an invalid
/// row and a system error.
pub const SETTLED_REFUSALS: &[&str] = &[
    "CommittingWeightsTooFast",
    "SettingWeightsTooFast",
    "NotEnoughStakeToSetWeights",
];

/// Retirement reason of an operation whose failures an operator reviewed.
pub const RETIRED_REVIEWED: &str = "finalized_failure_reviewed";
/// Retirement reason of an operation whose failures all settled automatically.
pub const RETIRED_SETTLED: &str = "finalized_failure_settled";
/// Retirement reason of an operation with a reviewed receipt contradiction
/// ([`ReviewKind::ContradictionReviewed`]): not a reviewed failure.
pub const RETIRED_CONTRADICTION_REVIEWED: &str = "receipt_contradiction_reviewed";

/// A dispatch error decoded by name from the inclusion block's metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodedError {
    /// `Module`, `System` or `MevShield`.
    pub kind: String,
    /// Pallet name of a module error.
    #[serde(default)]
    pub pallet: Option<String>,
    /// Error variant name.
    pub name: String,
}

impl DecodedError {
    /// `Pallet.Name`, or the name alone for a non-module error.
    pub fn label(&self) -> String {
        match &self.pallet {
            Some(pallet) => format!("{pallet}.{}", self.name),
            None => self.name.clone(),
        }
    }

    /// Whether [`SETTLED_REFUSALS`] settles this error.
    pub fn settles(&self) -> bool {
        self.kind == "Module"
            && self.pallet.as_deref() == Some(SETTLED_PALLET)
            && SETTLED_REFUSALS.contains(&self.name.as_str())
    }
}

/// The decoded dispatch result of one finalized failed attempt
/// ([`FAILURE_DIAGNOSTIC_KEY`]). Decodes tolerantly so a later build's fields
/// do not hide it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureDiagnostic {
    pub schema: u32,
    pub block_number: u64,
    pub block_hash: H256,
    /// `None` when the receipt reported failure without a decodable error,
    /// or contradicted the recorded outcome.
    #[serde(default)]
    pub error: Option<DecodedError>,
    /// What the receipt contradicted in rtao's recorded outcome
    /// (`receipt_reports_success`, `receipt_block_differs`), when it did.
    /// Such a failure never settles and needs review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_contradiction: Option<String>,
    /// The class of a receipt read that cannot clear by itself
    /// ([`ReadFailure::lasting`]), such as `state_discarded`, when no read
    /// has succeeded. Such a failure needs review; it is read again at each
    /// later recovery round and replaced by the first read that succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_unavailable: Option<String>,
}

/// Why one receipt read failed: a fixed class and whether it can clear by
/// itself. The error text is kept for a debug-level log only.
#[derive(Clone, PartialEq, Eq)]
pub struct ReadFailure {
    /// Fixed class, such as `timeout`, `transport` or `state_discarded`.
    pub class: String,
    /// The read cannot succeed again on its own: the endpoint discarded the
    /// inclusion block's state or lacks the block, or the block or its
    /// events do not decode under this build.
    pub lasting: bool,
    /// The read's error text. May carry endpoint detail: never logged at an
    /// alert level, never serialized.
    pub detail: String,
}

impl std::fmt::Debug for ReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadFailure")
            .field("class", &self.class)
            .field("lasting", &self.lasting)
            .finish_non_exhaustive()
    }
}

/// The diagnostic recorded for `failure` when its receipt read failed with
/// a lasting `class`.
pub fn unavailable(failure: &FailureRef, class: &str) -> FailureDiagnostic {
    FailureDiagnostic {
        schema: 1,
        block_number: failure.block_number,
        block_hash: failure.block_hash,
        error: None,
        receipt_contradiction: None,
        receipt_unavailable: Some(class.to_owned()),
    }
}

/// One receipt read of a finalized failed attempt, as recovery needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptRead {
    pub hash: H256,
    pub block_hash: Option<H256>,
    pub block_number: Option<u64>,
    pub success: bool,
    pub error: Option<DecodedError>,
}

/// The diagnostic recorded for `failure` from its receipt `read`: the
/// decoded error, or the contradiction when the receipt disagrees with
/// rtao's recorded inclusion or outcome.
pub fn diagnose(failure: &FailureRef, read: &ReceiptRead) -> FailureDiagnostic {
    let contradiction = if read.hash != failure.hash
        || read.block_hash != Some(failure.block_hash)
        || read.block_number != Some(failure.block_number)
    {
        Some("receipt_block_differs")
    } else if read.success {
        Some("receipt_reports_success")
    } else {
        None
    };
    FailureDiagnostic {
        schema: 1,
        block_number: failure.block_number,
        block_hash: failure.block_hash,
        error: if contradiction.is_some() { None } else { read.error.clone() },
        receipt_contradiction: contradiction.map(str::to_owned),
        receipt_unavailable: None,
    }
}

/// Who settled a finalized failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewKind {
    /// An operator ran `review-failure`.
    OperatorReviewed,
    /// Finalized recovery matched [`SETTLED_REFUSALS`].
    Settled,
    /// An operator ran `review-failure --accept-contradiction` on a failure
    /// whose receipt contradicts rtao's recorded outcome (it reports success
    /// or another inclusion), so the call may have landed. Never counted as
    /// a reviewed failure; the operation retires as
    /// [`RETIRED_CONTRADICTION_REVIEWED`].
    ContradictionReviewed,
}

/// The permanent review marker of one finalized failed attempt
/// ([`FAILURE_REVIEW_KEY`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureReview {
    pub schema: u32,
    pub kind: ReviewKind,
    /// Finalized inclusion of the failed attempt.
    pub block_number: u64,
    pub block_hash: H256,
    /// The decoded dispatch error, when recovery had read it.
    #[serde(default)]
    pub error: Option<DecodedError>,
    /// [`SETTLED_REFUSALS_RULE`] for an automatic review.
    #[serde(default)]
    pub rule: Option<String>,
    /// Journal revision the marker was written over.
    pub journal_revision: u64,
}

/// A finalized failed attempt awaiting decoding or review. Redacted:
/// identifiers, the inclusion height and the decoded error name only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FailedAttempt {
    pub operation: String,
    pub attempt: u64,
    pub block_number: u64,
    /// `Pallet.Name` when recovery has decoded the error.
    pub error: Option<String>,
    /// What the receipt contradicted, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt_contradiction: Option<String>,
    /// The lasting class of the failed receipt read, when no read succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt_unavailable: Option<String>,
}

/// A finalized failure settled under [`SETTLED_REFUSALS`] in one decoding
/// pass: the chain refused the weight call and the validator commits again
/// at a later tempo. Reported so a refusal that keeps recurring is visible.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SettledRefusal {
    pub operation: String,
    pub attempt: u64,
    /// The operation's tempo label, when its correlation names one.
    pub tempo: Option<u64>,
    pub block_number: u64,
    /// `Pallet.Name` of the refusal.
    pub error: String,
}

/// A finalized failure whose receipt could not be read in one decoding
/// pass. A passing failure awaits decoding and is read again at the next
/// round; a lasting one is recorded as `receipt_unavailable` and halts for
/// review. Debug and serialized forms omit the error text.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct UnreadableReceipt {
    pub operation: String,
    pub attempt: u64,
    pub block_number: u64,
    /// Fixed class of the read failure ([`ReadFailure::class`]).
    pub class: String,
    /// Whether the failure cannot clear by itself ([`ReadFailure::lasting`]).
    pub lasting: bool,
    /// The read's error text, for a debug-level log only.
    #[serde(skip)]
    pub detail: String,
}

impl std::fmt::Debug for UnreadableReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnreadableReceipt")
            .field("operation", &self.operation)
            .field("attempt", &self.attempt)
            .field("block_number", &self.block_number)
            .field("class", &self.class)
            .field("lasting", &self.lasting)
            .finish_non_exhaustive()
    }
}

/// One finalized failed attempt whose dispatch error recovery has not read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureRef {
    pub operation: String,
    pub attempt: u64,
    pub hash: H256,
    pub block_hash: H256,
    pub block_number: u64,
}

/// What one decoding pass read and wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SettleReport {
    /// Diagnostics recorded.
    pub decoded: usize,
    /// Failures settled under [`SETTLED_REFUSALS`].
    pub settled: usize,
    /// Operations retired because every failure is reviewed.
    pub retired: usize,
    /// The failures settled, one entry each (`settled` of them).
    pub refusals: Vec<SettledRefusal>,
    /// Receipts that could not be read; their failures await decoding.
    pub unreadable: Vec<UnreadableReceipt>,
}

/// Why `review-failure` refused. Its text is fixed and safe to print.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewRefusal {
    UnknownOperation,
    UnknownAttempt,
    /// Recovery has not classified the attempt yet.
    Unresolved,
    /// The attempt expired unexecuted or landed successfully.
    NotAFailure,
    /// rtao retained contradictory terminal evidence for the operation.
    ConflictingEvidence,
    /// The failure's receipt contradicts rtao's recorded outcome, and the
    /// operator did not pass `--accept-contradiction`.
    ReceiptContradiction,
}

impl ReviewRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::UnknownOperation => "unknown_operation",
            Self::UnknownAttempt => "unknown_attempt",
            Self::Unresolved => "attempt_unresolved",
            Self::NotAFailure => "not_a_finalized_failure",
            Self::ConflictingEvidence => "conflicting_evidence",
            Self::ReceiptContradiction => "receipt_contradiction",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Self::UnknownOperation => "The journal retains no operation with that ID.",
            Self::UnknownAttempt => "The operation has no attempt with that number.",
            Self::Unresolved => {
                "The attempt is unresolved: run the validator so finalized recovery classifies it first."
            }
            Self::NotAFailure => {
                "The attempt is not a finalized failure (it expired unexecuted or landed); nothing to review."
            }
            Self::ConflictingEvidence => {
                "The operation retains contradictory finalized evidence; it cannot be reviewed away."
            }
            Self::ReceiptContradiction => {
                "The failure's receipt contradicts the journal (it reports success or another inclusion), so the call may have landed. Check the chain (observe), then run again with --accept-contradiction to record a reviewed contradiction, which is not a reviewed failure."
            }
        }
    }
}

impl std::fmt::Display for ReviewRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.detail())
    }
}

impl std::error::Error for ReviewRefusal {}

/// The minimal shape of any review marker: a later build's marker with more
/// fields still counts as reviewed.
#[derive(Deserialize)]
struct MarkerShape {
    #[allow(dead_code)]
    schema: u32,
}

/// The inclusion of a finalized failed attempt, or `None` for any other outcome.
pub(crate) fn failed_at(attempt: &SignedAttempt) -> Option<(H256, u64)> {
    match &attempt.outcome {
        AttemptOutcome::Finalized {
            block,
            number,
            success: false,
            ..
        } => Some((*block, *number)),
        _ => None,
    }
}

fn entry<'a>(record: &'a OperationRecord, key: &str, attempt: u64) -> Option<&'a serde_json::Value> {
    record.extensions.get(key)?.get(attempt.to_string())
}

/// Store `value` for `attempt` under `key`. Refuses (false) to replace a
/// value under `key` that is not an object, which this build cannot read.
fn insert(record: &mut OperationRecord, key: &str, attempt: u64, value: serde_json::Value) -> bool {
    let map = record
        .extensions
        .entry(key.to_owned())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    match map.as_object_mut() {
        Some(map) => {
            map.insert(attempt.to_string(), value);
            true
        }
        None => false,
    }
}

/// Whether the attempt has a review marker.
pub(crate) fn reviewed(record: &OperationRecord, attempt: &SignedAttempt) -> bool {
    entry(record, FAILURE_REVIEW_KEY, attempt.id)
        .is_some_and(|value| serde_json::from_value::<MarkerShape>(value.clone()).is_ok())
}

/// A finalized failure without a review marker.
pub(crate) fn unreviewed(record: &OperationRecord, attempt: &SignedAttempt) -> bool {
    failed_at(attempt).is_some() && !reviewed(record, attempt)
}

/// An unreviewed finalized failure whose receipt recovery has not read yet:
/// recovery is incomplete, but it does not halt.
pub(crate) fn awaits_decode(record: &OperationRecord, attempt: &SignedAttempt) -> bool {
    unreviewed(record, attempt) && diagnostic(record, attempt).is_none()
}

/// An unreviewed finalized failure whose receipt recovery reads (again):
/// none read yet, or only a lasting read failure recorded.
pub(crate) fn needs_read(record: &OperationRecord, attempt: &SignedAttempt) -> bool {
    unreviewed(record, attempt)
        && diagnostic(record, attempt).is_none_or(|known| known.receipt_unavailable.is_some())
}

pub(crate) fn diagnostic(record: &OperationRecord, attempt: &SignedAttempt) -> Option<FailureDiagnostic> {
    entry(record, FAILURE_DIAGNOSTIC_KEY, attempt.id)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

pub(crate) fn review_kind(record: &OperationRecord, attempt: &SignedAttempt) -> Option<ReviewKind> {
    entry(record, FAILURE_REVIEW_KEY, attempt.id)
        .and_then(|value| value.get("kind"))
        .and_then(|kind| serde_json::from_value(kind.clone()).ok())
}

/// The review marker of `attempt`, when this build can read it.
pub fn review_of(record: &OperationRecord, attempt: u64) -> Option<FailureReview> {
    entry(record, FAILURE_REVIEW_KEY, attempt).and_then(|value| serde_json::from_value(value.clone()).ok())
}

pub(crate) fn failed_attempt(
    operation: &str,
    record: &OperationRecord,
    attempt: &SignedAttempt,
) -> Option<FailedAttempt> {
    let (_, number) = failed_at(attempt)?;
    let known = diagnostic(record, attempt);
    Some(FailedAttempt {
        operation: operation.to_owned(),
        attempt: attempt.id,
        block_number: number,
        error: known.as_ref().and_then(|d| d.error.as_ref()).map(DecodedError::label),
        receipt_contradiction: known.as_ref().and_then(|d| d.receipt_contradiction.clone()),
        receipt_unavailable: known.and_then(|d| d.receipt_unavailable),
    })
}

/// The marker an operator review writes over `revision` and whether it is
/// new, or the refusal. An attempt reviewed already returns its existing
/// marker. A failure whose receipt contradicts the recorded outcome is
/// refused unless `accept_contradiction`, and then marked
/// [`ReviewKind::ContradictionReviewed`].
pub(crate) fn operator_review(
    record: &OperationRecord,
    attempt: u64,
    revision: u64,
    accept_contradiction: bool,
) -> Result<(FailureReview, bool), ReviewRefusal> {
    if record.attempts.iter().any(|a| !a.evidence_conflicts.is_empty()) {
        return Err(ReviewRefusal::ConflictingEvidence);
    }
    let found = record
        .attempts
        .iter()
        .find(|a| a.id == attempt)
        .ok_or(ReviewRefusal::UnknownAttempt)?;
    let Some((block_hash, block_number)) = failed_at(found) else {
        return Err(match found.outcome {
            AttemptOutcome::Unresolved => ReviewRefusal::Unresolved,
            _ => ReviewRefusal::NotAFailure,
        });
    };
    if let Some(existing) = review_of(record, attempt) {
        return Ok((existing, false));
    }
    let known = diagnostic(record, found);
    let contradicted = known.as_ref().is_some_and(|d| d.receipt_contradiction.is_some());
    if contradicted && !accept_contradiction {
        return Err(ReviewRefusal::ReceiptContradiction);
    }
    Ok((
        FailureReview {
            schema: 1,
            kind: if contradicted {
                ReviewKind::ContradictionReviewed
            } else {
                ReviewKind::OperatorReviewed
            },
            block_number,
            block_hash,
            error: known.and_then(|d| d.error),
            rule: None,
            journal_revision: revision,
        },
        true,
    ))
}

/// Write an operator review marker. False when the record's review object
/// is unreadable to this build (nothing written).
pub(crate) fn write_review(record: &mut OperationRecord, attempt: u64, review: &FailureReview) -> bool {
    match serde_json::to_value(review) {
        Ok(value) => insert(record, FAILURE_REVIEW_KEY, attempt, value),
        Err(_) => false,
    }
}

/// Record `decoded` for the unreviewed failed attempt at `index`, and settle
/// it when [`SETTLED_REFUSALS`] names its recorded error. A decoding of
/// another inclusion than the attempt's is ignored. A diagnostic recorded
/// earlier is replaced only when it records a lasting read failure
/// (`receipt_unavailable`) and `decoded` differs from it. Returns whether
/// it recorded a diagnostic, and the settled error when it settled.
pub(crate) fn record_decoding(
    record: &mut OperationRecord,
    index: usize,
    decoded: Option<&FailureDiagnostic>,
    revision: u64,
) -> (bool, Option<DecodedError>) {
    let attempt = &record.attempts[index];
    let id = attempt.id;
    let Some((block_hash, block_number)) = failed_at(attempt) else {
        return (false, None);
    };
    if reviewed(record, attempt) {
        return (false, None);
    }
    let known = diagnostic(record, attempt);
    let mut wrote = false;
    if let Some(found) = decoded
        && replaces(known.as_ref(), found, block_hash, block_number)
        && let Ok(value) = serde_json::to_value(found)
    {
        wrote = insert(record, FAILURE_DIAGNOSTIC_KEY, id, value);
    }
    // A diagnostic recorded earlier is judged again, so a later rule
    // settles what an earlier one left for review.
    let error = if wrote {
        decoded.and_then(|d| d.error.clone())
    } else {
        known.and_then(|known| known.error)
    };
    let Some(error) = error.filter(DecodedError::settles) else {
        return (wrote, None);
    };
    let review = FailureReview {
        schema: 1,
        kind: ReviewKind::Settled,
        block_number,
        block_hash,
        error: Some(error.clone()),
        rule: Some(SETTLED_REFUSALS_RULE.into()),
        journal_revision: revision,
    };
    let settled = write_review(record, id, &review);
    (wrote, settled.then_some(error))
}

/// Whether `found`, a decoding of the inclusion `block_hash`/`block_number`,
/// is recorded over the attempt's `known` diagnostic: there is none, or it
/// records a lasting read failure and `found` differs from it.
fn replaces(known: Option<&FailureDiagnostic>, found: &FailureDiagnostic, block_hash: H256, block_number: u64) -> bool {
    found.block_hash == block_hash
        && found.block_number == block_number
        && known.is_none_or(|known| known.receipt_unavailable.is_some() && known != found)
}

/// Whether a decoding pass would change `record`: an unreviewed failure with
/// a supplied decoding it records, or a recorded diagnostic the rule
/// settles, or a retirement [`reviewed_retirement`] allows.
pub(crate) fn decoding_changes<'a>(
    record: &OperationRecord,
    supplied: impl Fn(u64, H256) -> Option<&'a FailureDiagnostic>,
) -> bool {
    record.attempts.iter().any(|attempt| {
        let Some((block_hash, block_number)) = failed_at(attempt) else {
            return false;
        };
        let known = diagnostic(record, attempt);
        unreviewed(record, attempt)
            && (known
                .as_ref()
                .and_then(|known| known.error.as_ref())
                .is_some_and(DecodedError::settles)
                || supplied(attempt.id, attempt.hash)
                    .is_some_and(|found| replaces(known.as_ref(), found, block_hash, block_number)))
    }) || reviewed_retirement(record).is_some()
}

/// The retirement reason of a handed-back operation whose every finalized
/// failure is reviewed, or `None`: it never landed, no attempt awaits
/// classification, and its signed bytes can never land again.
pub(crate) fn reviewed_retirement(record: &OperationRecord) -> Option<&'static str> {
    let handed_off =
        record.extensions.get("scheduler_handed_off") == Some(&serde_json::Value::Bool(true));
    if !handed_off || record.extensions.contains_key(crate::RETIRED_KEY) {
        return None;
    }
    let mut failures = 0;
    let mut all_settled = true;
    let mut contradiction = false;
    for attempt in &record.attempts {
        match &attempt.outcome {
            AttemptOutcome::Unresolved | AttemptOutcome::Finalized { success: true, .. } => {
                return None;
            }
            AttemptOutcome::Finalized { success: false, .. } => {
                if !reviewed(record, attempt) {
                    return None;
                }
                failures += 1;
                let kind = review_kind(record, attempt);
                all_settled &= kind == Some(ReviewKind::Settled);
                contradiction |= kind == Some(ReviewKind::ContradictionReviewed);
            }
            AttemptOutcome::ExpiredUnexecuted => {}
        }
    }
    if failures == 0 {
        return None;
    }
    Some(if contradiction {
        RETIRED_CONTRADICTION_REVIEWED
    } else if all_settled {
        RETIRED_SETTLED
    } else {
        RETIRED_REVIEWED
    })
}

/// Mark the operation retired when [`reviewed_retirement`] allows it, in the
/// shape [`crate::JournalControls::retire_superseded`] writes. Returns
/// whether it wrote.
pub(crate) fn retire_reviewed(record: &mut OperationRecord) -> bool {
    let Some(reason) = reviewed_retirement(record) else {
        return false;
    };
    record.extensions.insert(
        crate::RETIRED_KEY.into(),
        serde_json::json!({ "schema": 1, "reason": reason, "attempts": record.attempts.len() }),
    );
    true
}
