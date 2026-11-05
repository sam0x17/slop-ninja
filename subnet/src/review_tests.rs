//! Review of finalized failed attempts (`review-failure`, settled refusals).
//! Offline: attempts are written into the journal in rtao's own schema; no
//! client, signer or RPC is created.

use super::*;
use crate::review::{
    FAILURE_DIAGNOSTIC_KEY, FAILURE_REVIEW_KEY, FailureRef, RETIRED_CONTRADICTION_REVIEWED, RETIRED_REVIEWED,
    RETIRED_SETTLED, SETTLED_REFUSALS_RULE,
};

/// Append a signed attempt with `outcome` in rtao's schema. `seed` makes its
/// bytes, and so its hash, unique within the operation.
fn push_attempt(record: &mut rtao_client::journal::OperationRecord, seed: u8, outcome: AttemptOutcome) {
    use blake2::{Blake2b, Digest as _, digest::consts::U32};
    let bytes = vec![seed, 2, 3];
    let mut attempt: rtao_client::journal::SignedAttempt = serde_json::from_value(serde_json::json!({
        "id": record.attempts.len(),
        "role": "Ordinary",
        "material_generation": 0,
        "hash": H256([0; 32]),
        "bytes": [],
        "nonce": 7 + u64::from(seed),
        "spec_version": 486,
        "transaction_version": 1,
        "start": 1000,
        "expiry": 1128,
        "era_anchor": H256([0x5b; 32]),
        "era": null,
        "metadata_hash": null,
        "material": null,
        "next": 1000,
        "finalized": null,
        "outcome": "Unresolved",
        "evidence_conflicts": [],
    }))
    .expect("synthetic attempt matches rtao's schema");
    attempt.hash = H256(Blake2b::<U32>::digest(&bytes).into());
    attempt.bytes = bytes;
    attempt.material = record.material.clone();
    attempt.material_generation = Some(record.material_generation);
    attempt.outcome = outcome;
    record.mode = rtao_client::journal::ExecutionMode::Ordinary;
    record.attempts.push(attempt);
}

fn failed(number: u64) -> AttemptOutcome {
    AttemptOutcome::Finalized {
        block: H256([number as u8; 32]),
        number,
        success: false,
        extensions: BTreeMap::new(),
    }
}

fn landed(number: u64) -> AttemptOutcome {
    AttemptOutcome::Finalized {
        block: H256([number as u8; 32]),
        number,
        success: true,
        extensions: BTreeMap::new(),
    }
}

/// Queue the work for `tempo` and give it one attempt per outcome.
fn operation_with(adapter: &mut NativeWeightAdapter, tempo: u64, outcomes: Vec<AttemptOutcome>) -> Result<String> {
    let work = WeightWork { tempo: Some(tempo), ..work() };
    let Enqueued::Created(id) = adapter.enqueue(&work, row())? else { panic!("created") };
    adapter.controls.journal.update(|image| {
        let record = image.operations.get_mut(&id).unwrap();
        for (seed, outcome) in outcomes.into_iter().enumerate() {
            push_attempt(record, seed as u8 + 1, outcome);
        }
        Ok(())
    })?;
    Ok(id)
}

fn ready(summary: &JournalSummary) -> bool {
    // Every other recovery condition holds; only the attempts decide.
    recovery_complete(summary, false, true, 0, true)
}

fn diagnostic(number: u64, kind: &str, pallet: Option<&str>, name: &str) -> FailureDiagnostic {
    FailureDiagnostic {
        schema: 1,
        block_number: number,
        block_hash: H256([number as u8; 32]),
        error: Some(DecodedError {
            kind: kind.into(),
            pallet: pallet.map(str::to_owned),
            name: name.into(),
        }),
        receipt_contradiction: None,
        receipt_unavailable: None,
    }
}

fn refusal(error: anyhow::Error) -> ReviewRefusal {
    *error.downcast_ref::<ReviewRefusal>().expect("a review refusal")
}

/// One finalized failed commit halts recovery and resume. After the
/// operator reviews it, recovery is complete again, the marker names the
/// block, and the handed-back operation is retired so the next tempo
/// follows. A second, unreviewed failure halts again.
#[test]
fn a_reviewed_failure_stops_halting_and_a_new_one_halts_again() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    enable_storage_fixture(&adapter)?;
    let controls = adapter.controls();
    let first = operation_with(&mut adapter, 40, vec![failed(1200)])?;
    hand_off(&adapter, &first)?;

    // Its receipt is not read yet: recovery is incomplete but nothing halts.
    let summary = controls.inspect()?;
    assert_eq!((summary.failures_awaiting_decode, summary.finalized_failures), (1, 0));
    assert!(summary.review_required.is_empty());
    assert!(!ready(&summary));
    // Decoded, an error that does not settle halts for review.
    let at = controls.undecoded_failures()?.remove(0);
    let version = diagnostic(1200, "Module", Some("SubtensorModule"), "IncorrectWeightVersionKey");
    assert_eq!(controls.record_failure_decodings(&[(at, version)])?.decoded, 1);
    let summary = controls.inspect()?;
    assert_eq!((summary.failures_awaiting_decode, summary.finalized_failures, summary.reviewed_failures), (0, 1, 0));
    assert_eq!(
        summary.review_required,
        vec![FailedAttempt {
            operation: first.clone(),
            attempt: 0,
            block_number: 1200,
            error: Some("SubtensorModule.IncorrectWeightVersionKey".into()),
            receipt_contradiction: None,
            receipt_unavailable: None,
        }]
    );
    assert!(!ready(&summary));
    assert!(adapter.resume_after_recovery().is_err());
    assert_eq!(controls.retired(&first)?, None);

    let review = controls.review_failure(&first, 0, false)?;
    assert_eq!(review.kind, ReviewKind::OperatorReviewed);
    assert_eq!((review.block_number, review.block_hash), (1200, H256([1200u64 as u8; 32])));
    assert_eq!(review.error.map(|e| e.name).as_deref(), Some("IncorrectWeightVersionKey"));
    assert_eq!(review.rule, None);
    let summary = controls.inspect()?;
    assert_eq!((summary.finalized_failures, summary.reviewed_failures), (0, 1));
    assert!(summary.review_required.is_empty());
    assert!(ready(&summary));
    assert!(keeps_recovery_after_error(summary.attempts, &summary));
    let status = controls.operation(&first)?.unwrap();
    assert_eq!((status.finalized_failures, status.reviewed_failures), (0, 1));
    // The application's tempo loop treats it like other retired work.
    assert_eq!(controls.retired(&first)?.as_deref(), Some(RETIRED_REVIEWED));
    // Nothing of the attempt changed but its marker.
    let record = &adapter.controls.journal.snapshot()?.operations[&first];
    assert_eq!(record.attempts.len(), 1);
    assert_eq!(record.attempts[0].outcome, failed(1200));
    assert!(record.attempts[0].extensions.is_empty());
    assert!(record.extensions[FAILURE_REVIEW_KEY].get("0").is_some());

    // A second failure waits again; an operator may review it undecoded
    // (a receipt that can never be read).
    let second = operation_with(&mut adapter, 41, vec![failed(1300)])?;
    let summary = controls.inspect()?;
    assert_eq!((summary.failures_awaiting_decode, summary.reviewed_failures), (1, 1));
    assert_eq!(summary.awaiting_decode[0].operation, second);
    assert!(!ready(&summary));
    assert!(adapter.resume_after_recovery().is_err());
    assert_eq!(controls.review_failure(&second, 0, false)?.error, None);
    assert!(ready(&controls.inspect()?));
    Ok(())
}

/// Review refuses, writing nothing, an unknown operation or attempt, an
/// unresolved attempt, an expired or landed one, and an operation with
/// contradictory evidence. A repeated review returns the same marker and
/// writes nothing.
#[test]
fn review_refuses_anything_but_a_classified_failure() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    enable_storage_fixture(&adapter)?;
    let controls = adapter.controls();
    let unresolved = operation_with(&mut adapter, 40, vec![AttemptOutcome::ExpiredUnexecuted, AttemptOutcome::Unresolved])?;
    hand_off(&adapter, &unresolved)?;
    let before = adapter.controls.journal.snapshot()?.revision;
    let code = |result: Result<FailureReview>| refusal(result.unwrap_err());
    assert_eq!(code(controls.review_failure("unknown", 0, false)), ReviewRefusal::UnknownOperation);
    assert_eq!(code(controls.review_failure(&unresolved, 9, false)), ReviewRefusal::UnknownAttempt);
    assert_eq!(code(controls.review_failure(&unresolved, 1, false)), ReviewRefusal::Unresolved);
    assert_eq!(code(controls.review_failure(&unresolved, 0, false)), ReviewRefusal::NotAFailure);
    assert_eq!(adapter.controls.journal.snapshot()?.revision, before);
    assert_eq!(controls.inspect()?.unresolved_attempts, 1);

    // A landed commit is not a failure; its operation is never retired.
    let landed_op = operation_with(&mut adapter, 41, vec![landed(1250)])?;
    hand_off(&adapter, &landed_op)?;
    assert_eq!(code(controls.review_failure(&landed_op, 0, false)), ReviewRefusal::NotAFailure);
    assert_eq!(controls.retired(&landed_op)?, None);

    let conflicted = operation_with(&mut adapter, 42, vec![failed(1300)])?;
    adapter.controls.journal.update(|image| {
        image.operations.get_mut(&conflicted).unwrap().attempts[0]
            .evidence_conflicts
            .push(landed(1301));
        Ok(())
    })?;
    assert_eq!(code(controls.review_failure(&conflicted, 0, false)), ReviewRefusal::ConflictingEvidence);
    assert_eq!(controls.inspect()?.failures_awaiting_decode, 1);
    hand_off(&adapter, &conflicted)?;
    // Handed back, it is still never retired: its failure is unreviewed.
    assert_eq!(controls.record_failure_decodings(&[])?.retired, 0);

    let failure = operation_with(&mut adapter, 43, vec![failed(1400)])?;
    let marker = controls.review_failure(&failure, 0, false)?;
    let after = adapter.controls.journal.snapshot()?.revision;
    assert_eq!(controls.review_failure(&failure, 0, false)?, marker);
    assert_eq!(adapter.controls.journal.snapshot()?.revision, after);
    // Not handed back yet: reviewed, but rtao still owns it, so not retired.
    assert_eq!(controls.retired(&failure)?, None);
    // Once rtao hands it back, the next decoding pass retires it.
    hand_off(&adapter, &failure)?;
    assert_eq!(controls.record_failure_decodings(&[])?.retired, 1);
    assert_eq!(controls.retired(&failure)?.as_deref(), Some(RETIRED_REVIEWED));
    // A pass with nothing to do writes nothing.
    let quiet = adapter.controls.journal.snapshot()?.revision;
    assert_eq!(controls.record_failure_decodings(&[])?, SettleReport::default());
    assert_eq!(adapter.controls.journal.snapshot()?.revision, quiet);
    Ok(())
}

/// A failure whose decoded error is a known refusal of a weight call (rate
/// limit, too soon, not enough stake) settles as a recorded diagnostic,
/// is reported once, and never halts. Any other error (a full commit queue
/// and a missing permit included), the same name from another pallet or as
/// a system error, and a failure with no decodable error still halt and
/// keep the decoded name for review.
#[test]
fn known_refusals_settle_and_other_errors_still_halt() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    enable_storage_fixture(&adapter)?;
    let controls = adapter.controls();
    let cases = [
        ("Module", Some("SubtensorModule"), "CommittingWeightsTooFast", true),
        ("Module", Some("SubtensorModule"), "SettingWeightsTooFast", true),
        ("Module", Some("SubtensorModule"), "NotEnoughStakeToSetWeights", true),
        ("Module", Some("SubtensorModule"), "TooManyUnrevealedCommits", false),
        ("Module", Some("SubtensorModule"), "NeuronNoValidatorPermit", false),
        ("Module", Some("SubtensorModule"), "IncorrectWeightVersionKey", false),
        ("Module", Some("SubtensorModule"), "CommitRevealDisabled", false),
        ("Module", Some("SubtensorModule"), "HotKeyNotRegisteredInSubNet", false),
        ("Module", Some("OtherPallet"), "CommittingWeightsTooFast", false),
        ("System", None, "CommittingWeightsTooFast", false),
    ];
    let mut ids = Vec::new();
    for (tempo, _) in cases.iter().enumerate() {
        let id = operation_with(&mut adapter, 50 + tempo as u64, vec![failed(2000 + tempo as u64)])?;
        hand_off(&adapter, &id)?;
        ids.push(id);
    }
    let undecodable = operation_with(&mut adapter, 70, vec![failed(2100)])?;
    hand_off(&adapter, &undecodable)?;
    let undecoded = controls.undecoded_failures()?;
    assert_eq!(undecoded.len(), cases.len() + 1);
    let mut decoded: Vec<(FailureRef, FailureDiagnostic)> = undecoded
        .iter()
        .filter_map(|at| {
            let index = ids.iter().position(|id| id == &at.operation)?;
            let (kind, pallet, name, _) = cases[index];
            Some((at.clone(), diagnostic(at.block_number, kind, pallet, name)))
        })
        .collect();
    let at = undecoded.iter().find(|at| at.operation == undecodable).unwrap().clone();
    decoded.push((at.clone(), FailureDiagnostic { error: None, ..diagnostic(at.block_number, "Module", None, "") }));

    let settling = cases.iter().filter(|case| case.3).count();
    let report = controls.record_failure_decodings(&decoded)?;
    assert_eq!((report.decoded, report.settled, report.retired), (cases.len() + 1, settling, settling));
    let mut refused: Vec<_> = report.refusals.iter().map(|r| (r.tempo, r.error.as_str())).collect();
    refused.sort();
    assert_eq!(
        refused,
        vec![
            (Some(50), "SubtensorModule.CommittingWeightsTooFast"),
            (Some(51), "SubtensorModule.SettingWeightsTooFast"),
            (Some(52), "SubtensorModule.NotEnoughStakeToSetWeights"),
        ]
    );
    assert!(report.unreadable.is_empty());
    let summary = controls.inspect()?;
    assert_eq!(summary.reviewed_failures, settling);
    assert_eq!(summary.finalized_failures, cases.len() + 1 - settling);
    assert!(!ready(&summary));
    for (id, (_, pallet, name, settles)) in ids.iter().zip(cases) {
        let record = &adapter.controls.journal.snapshot()?.operations[id];
        assert!(record.extensions[FAILURE_DIAGNOSTIC_KEY].get("0").is_some());
        if settles {
            let review = crate::review::review_of(record, 0).expect("a settled review");
            assert_eq!(review.kind, ReviewKind::Settled);
            assert_eq!(review.rule.as_deref(), Some(SETTLED_REFUSALS_RULE));
            assert_eq!(review.error.as_ref().map(|e| e.name.as_str()), Some(name));
            assert_eq!(controls.retired(id)?.as_deref(), Some(RETIRED_SETTLED));
        } else {
            let label = pallet.map_or_else(|| name.to_owned(), |pallet| format!("{pallet}.{name}"));
            let listed = summary.review_required.iter().find(|f| &f.operation == id).unwrap();
            assert_eq!(listed.error.as_deref(), Some(label.as_str()));
            assert_eq!(controls.retired(id)?, None);
        }
    }
    // Decoded once: nothing is read again, and a repeat pass writes nothing.
    assert!(controls.undecoded_failures()?.is_empty());
    let revision = adapter.controls.journal.snapshot()?.revision;
    assert_eq!(controls.record_failure_decodings(&decoded)?, SettleReport::default());
    assert_eq!(adapter.controls.journal.snapshot()?.revision, revision);

    // The operator reviews the rest; the marker keeps the decoded error.
    for id in ids.iter().chain([&undecodable]) {
        let review = controls.review_failure(id, 0, false)?;
        if review.kind == ReviewKind::OperatorReviewed {
            assert_eq!(controls.retired(id)?.as_deref(), Some(RETIRED_REVIEWED));
        }
    }
    let version = controls.review_failure(&ids[5], 0, false)?;
    assert_eq!(cases[5].2, "IncorrectWeightVersionKey");
    assert_eq!(version.error.map(|e| e.name), Some("IncorrectWeightVersionKey".into()));
    let summary = controls.inspect()?;
    assert_eq!((summary.finalized_failures, summary.reviewed_failures), (0, cases.len() + 1));
    assert!(ready(&summary));
    Ok(())
}

/// A decoding for another inclusion than the attempt's is not recorded;
/// a diagnostic recorded earlier is judged again by the current rule.
#[test]
fn decodings_bind_to_the_attempts_inclusion() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    enable_storage_fixture(&adapter)?;
    let controls = adapter.controls();
    let id = operation_with(&mut adapter, 40, vec![AttemptOutcome::ExpiredUnexecuted, failed(1200)])?;
    hand_off(&adapter, &id)?;
    let at = controls.undecoded_failures()?.remove(0);
    assert_eq!((at.attempt, at.block_number), (1, 1200));
    let elsewhere = diagnostic(1201, "Module", Some("SubtensorModule"), "SettingWeightsTooFast");
    assert_eq!(controls.record_failure_decodings(&[(at.clone(), elsewhere)])?, SettleReport::default());
    assert_eq!(controls.inspect()?.failures_awaiting_decode, 1);
    // A diagnostic already on the attempt (an earlier build's rule left it
    // for review) settles under the current rule without another read.
    adapter.controls.journal.update(|image| {
        let record = image.operations.get_mut(&id).unwrap();
        record.extensions.insert(
            FAILURE_DIAGNOSTIC_KEY.into(),
            serde_json::json!({
                "1": diagnostic(1200, "Module", Some("SubtensorModule"), "SettingWeightsTooFast"),
            }),
        );
        Ok(())
    })?;
    assert!(controls.undecoded_failures()?.is_empty());
    let report = controls.record_failure_decodings(&[])?;
    assert_eq!((report.decoded, report.settled, report.retired), (0, 1, 1));
    assert_eq!(report.refusals[0].error, "SubtensorModule.SettingWeightsTooFast");
    assert!(ready(&controls.inspect()?));
    Ok(())
}

/// The markers survive reopening, by the adapter (which pauses first) and
/// by the offline controls. Every earlier build halts on a retained
/// failure, reviewed or not, so the rollback check reports each as a
/// non-retirable blocker and retires nothing.
#[test]
fn review_markers_survive_reopen() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    let (reviewed, settled) = {
        let mut adapter = open(&path, config())?;
        enable_storage_fixture(&adapter)?;
        let controls = adapter.controls();
        let reviewed = operation_with(&mut adapter, 40, vec![failed(1200)])?;
        hand_off(&adapter, &reviewed)?;
        controls.review_failure(&reviewed, 0, false)?;
        let settled = operation_with(&mut adapter, 41, vec![failed(1300)])?;
        hand_off(&adapter, &settled)?;
        let at = controls.undecoded_failures()?.remove(0);
        controls.record_failure_decodings(&[(
            at,
            diagnostic(1300, "Module", Some("SubtensorModule"), "NotEnoughStakeToSetWeights"),
        )])?;
        (reviewed, settled)
    };
    for _ in 0..2 {
        let adapter = open(&path, config())?;
        let summary = adapter.controls.inspect()?;
        assert!(summary.paused);
        assert_eq!((summary.attempts, summary.finalized_failures, summary.reviewed_failures), (2, 0, 2));
        assert!(ready(&summary));
        assert_eq!(adapter.controls.retired(&reviewed)?.as_deref(), Some(RETIRED_REVIEWED));
        assert_eq!(adapter.controls.retired(&settled)?.as_deref(), Some(RETIRED_SETTLED));
        let blockers = adapter.controls.rollback_blockers()?;
        assert_eq!(blockers.len(), 2);
        assert!(blockers.iter().all(|b| {
            b.field == REVIEWED_FAILURE_BLOCKER_FIELD && !b.retirable && b.finalized_failure()
        }));
    }
    let offline = JournalControls::open_existing(&path)?;
    assert_eq!(offline.inspect()?.reviewed_failures, 2);
    drop(offline);
    let raw = OperationJournal::open(&path)?.snapshot()?;
    assert!(raw.operations[&reviewed].extensions[FAILURE_REVIEW_KEY].get("0").is_some());
    assert!(raw.operations[&settled].extensions[FAILURE_DIAGNOSTIC_KEY].get("0").is_some());
    Ok(())
}

/// A journal retaining a finalized failure is roll-forward only: every
/// build before failure review halts on it and cannot review it. An
/// unreviewed failure is reported under its own field, a review does not
/// clear it, and `retire_for_rollback` retires nothing, so labels never
/// shift for a rollback that cannot happen. Queued `Submit` work, which the
/// previous pin reads, is neither reported nor retired.
#[test]
fn a_retained_failure_makes_the_journal_roll_forward_only() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    enable_storage_fixture(&adapter)?;
    let controls = adapter.controls();
    let id = operation_with(&mut adapter, 40, vec![failed(1200)])?;
    hand_off(&adapter, &id)?;
    let burn = BTreeMap::from([(Uid(2), 65535.0)]);
    let next = WeightWork { v2_target: Some([7; 32]), tempo: Some(41), ..work() };
    let Enqueued::Created(queued) = adapter.enqueue_with_owner_rows(&next, burn, OwnerRowPolicy::Submit)? else {
        panic!("created")
    };
    let failure = RollbackBlocker { operation_id: Some(id.clone()), field: FAILURE_BLOCKER_FIELD.into(), retirable: false };
    assert_eq!(controls.rollback_blockers()?, vec![failure.clone()]);
    assert!(failure.roll_forward_only());
    assert!(controls.retire_for_rollback()?.is_empty());
    assert_eq!(controls.retired(&queued)?, None);
    controls.review_failure(&id, 0, false)?;
    let reviewed = RollbackBlocker { operation_id: Some(id), field: REVIEWED_FAILURE_BLOCKER_FIELD.into(), retirable: false };
    assert_eq!(controls.rollback_blockers()?, vec![reviewed]);
    assert!(controls.retire_for_rollback()?.is_empty());
    assert_eq!(controls.retired(&queued)?, None);
    Ok(())
}

fn passing(class: &str) -> crate::ReadFailure {
    crate::ReadFailure { class: class.into(), lasting: false, detail: format!("{class} detail") }
}

fn lasting(class: &str) -> crate::ReadFailure {
    crate::ReadFailure { class: class.into(), lasting: true, detail: format!("{class} detail") }
}

fn receipt(at: &FailureRef, name: &str) -> crate::ReceiptRead {
    crate::ReceiptRead {
        hash: at.hash,
        block_hash: Some(at.block_hash),
        block_number: Some(at.block_number),
        success: false,
        error: Some(DecodedError { kind: "Module".into(), pallet: Some("SubtensorModule".into()), name: name.into() }),
    }
}

/// The reads here are ready at once and the journal is synchronous, so a
/// polling loop with a no-op waker drives the pass without a runtime.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
    }
}

/// A receipt read that fails leaves the refusal awaiting decoding: it is
/// reported, recovery stays incomplete without halting, and the next round
/// reads it again and settles it.
#[test]
fn an_unreadable_receipt_is_read_again_and_then_settles() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    enable_storage_fixture(&adapter)?;
    let controls = adapter.controls();
    let id = operation_with(&mut adapter, 40, vec![failed(1200)])?;
    hand_off(&adapter, &id)?;
    let reads = std::cell::Cell::new(0);
    let read = |at: FailureRef| {
        reads.set(reads.get() + 1);
        let first = reads.get() == 1;
        async move {
            if first {
                Err(passing("transport"))
            } else {
                Ok(receipt(&at, "CommittingWeightsTooFast"))
            }
        }
    };
    let revision = adapter.controls.journal.snapshot()?.revision;
    let report = block_on(controls.decode_failures_with(&read))?;
    assert_eq!((report.decoded, report.settled), (0, 0));
    assert_eq!(report.unreadable.len(), 1);
    assert_eq!((report.unreadable[0].attempt, report.unreadable[0].block_number), (0, 1200));
    assert_eq!((report.unreadable[0].class.as_str(), report.unreadable[0].lasting), ("transport", false));
    // The error text is kept for a debug log only: neither form shows it.
    assert!(!format!("{:?}", report.unreadable[0]).contains("detail"));
    assert!(!serde_json::to_string(&report.unreadable[0])?.contains("detail"));
    // Nothing written; it waits for decoding, which never halts.
    assert_eq!(adapter.controls.journal.snapshot()?.revision, revision);
    let summary = controls.inspect()?;
    assert_eq!((summary.failures_awaiting_decode, summary.finalized_failures), (1, 0));
    assert!(!ready(&summary));
    assert!(!keeps_recovery_after_error(summary.attempts, &summary));

    let report = block_on(controls.decode_failures_with(&read))?;
    assert_eq!((report.decoded, report.settled, report.retired), (1, 1, 1));
    assert!(report.unreadable.is_empty());
    assert_eq!(report.refusals[0].operation, id);
    assert_eq!(report.refusals[0].error, "SubtensorModule.CommittingWeightsTooFast");
    assert_eq!(report.refusals[0].tempo, Some(40));
    assert_eq!(reads.get(), 2);
    assert!(ready(&controls.inspect()?));
    assert_eq!(controls.retired(&id)?.as_deref(), Some(RETIRED_SETTLED));
    // Settled: never read again.
    assert_eq!(block_on(controls.decode_failures_with(&read))?, SettleReport::default());
    assert_eq!(reads.get(), 2);
    Ok(())
}

/// A receipt that contradicts rtao's recorded outcome (it reports success,
/// or another inclusion) is recorded as a contradiction and halts for
/// review; it never settles, whatever error it names.
#[test]
fn a_contradicting_receipt_is_recorded_and_halts_for_review() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    enable_storage_fixture(&adapter)?;
    let controls = adapter.controls();
    let success = operation_with(&mut adapter, 40, vec![failed(1200)])?;
    hand_off(&adapter, &success)?;
    let moved = operation_with(&mut adapter, 41, vec![failed(1300)])?;
    let read = |at: FailureRef| {
        let mut read = receipt(&at, "CommittingWeightsTooFast");
        if at.block_number == 1200 {
            read.success = true;
        } else {
            read.block_number = Some(1301);
        }
        async move { Ok(read) }
    };
    let report = block_on(controls.decode_failures_with(read))?;
    assert_eq!((report.decoded, report.settled), (2, 0));
    let summary = controls.inspect()?;
    assert_eq!((summary.failures_awaiting_decode, summary.finalized_failures), (0, 2));
    let found = |id: &str| summary.review_required.iter().find(|f| f.operation == id).unwrap().clone();
    assert_eq!(found(&success).receipt_contradiction.as_deref(), Some("receipt_reports_success"));
    assert_eq!(found(&moved).receipt_contradiction.as_deref(), Some("receipt_block_differs"));
    assert_eq!(found(&success).error, None);
    assert!(!ready(&summary));
    // The call may have landed: a plain review is refused and writes
    // nothing; an explicit one is a reviewed contradiction, never a
    // reviewed failure, and retires the handed-back work under its own
    // reason.
    let revision = adapter.controls.journal.snapshot()?.revision;
    assert_eq!(refusal(controls.review_failure(&success, 0, false).unwrap_err()), ReviewRefusal::ReceiptContradiction);
    assert_eq!(refusal(controls.review_failure(&moved, 0, false).unwrap_err()), ReviewRefusal::ReceiptContradiction);
    assert_eq!(adapter.controls.journal.snapshot()?.revision, revision);
    assert_eq!(controls.review_failure(&success, 0, true)?.kind, ReviewKind::ContradictionReviewed);
    assert_eq!(controls.review_failure(&moved, 0, true)?.kind, ReviewKind::ContradictionReviewed);
    let summary = controls.inspect()?;
    assert_eq!((summary.reviewed_failures, summary.reviewed_contradictions), (0, 2));
    assert!(ready(&summary));
    assert_eq!(controls.retired(&success)?.as_deref(), Some(RETIRED_CONTRADICTION_REVIEWED));
    assert_eq!(controls.operation(&success)?.unwrap().reviewed_contradictions, 1);
    Ok(())
}

/// A receipt read that cannot clear by itself (the endpoint discarded the
/// block's state) is recorded as `receipt_unavailable` and halts for review
/// at once, instead of retrying without end. Later rounds read it again
/// without rewriting the same diagnostic; the first read that succeeds
/// replaces it and settles a known refusal without any review. No retry is
/// counted.
#[test]
fn a_lasting_read_failure_halts_for_review_and_a_later_read_clears_it() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    enable_storage_fixture(&adapter)?;
    let controls = adapter.controls();
    let id = operation_with(&mut adapter, 40, vec![failed(1200)])?;
    hand_off(&adapter, &id)?;
    let discarded = |_: FailureRef| async { Err(lasting("state_discarded")) };
    let report = block_on(controls.decode_failures_with(discarded))?;
    assert_eq!((report.decoded, report.settled), (1, 0));
    assert_eq!((report.unreadable[0].class.as_str(), report.unreadable[0].lasting), ("state_discarded", true));
    let summary = controls.inspect()?;
    assert_eq!((summary.failures_awaiting_decode, summary.finalized_failures), (0, 1));
    assert_eq!(summary.review_required[0].receipt_unavailable.as_deref(), Some("state_discarded"));
    assert_eq!(summary.review_required[0].error, None);
    assert!(!ready(&summary));

    // Read again each round; the same lasting failure writes nothing.
    let revision = adapter.controls.journal.snapshot()?.revision;
    assert_eq!(controls.undecoded_failures()?.len(), 1);
    let report = block_on(controls.decode_failures_with(discarded))?;
    assert_eq!((report.decoded, report.unreadable.len()), (0, 1));
    assert_eq!(adapter.controls.journal.snapshot()?.revision, revision);
    // A passing failure leaves the recorded diagnostic as it is.
    block_on(controls.decode_failures_with(|_: FailureRef| async { Err(passing("timeout")) }))?;
    assert_eq!(adapter.controls.journal.snapshot()?.revision, revision);
    assert_eq!(controls.inspect()?.finalized_failures, 1);

    // An endpoint holding the state reads it: the refusal settles.
    let report = block_on(controls.decode_failures_with(|at: FailureRef| async move {
        Ok(receipt(&at, "CommittingWeightsTooFast"))
    }))?;
    assert_eq!((report.decoded, report.settled, report.retired), (1, 1, 1));
    let summary = controls.inspect()?;
    assert_eq!((summary.finalized_failures, summary.reviewed_failures), (0, 1));
    assert!(ready(&summary));
    assert!(controls.undecoded_failures()?.is_empty());
    assert_eq!(controls.retired(&id)?.as_deref(), Some(RETIRED_SETTLED));

    // An operator may instead review one that never becomes readable.
    let other = operation_with(&mut adapter, 41, vec![failed(1300)])?;
    hand_off(&adapter, &other)?;
    block_on(controls.decode_failures_with(discarded))?;
    assert_eq!(controls.review_failure(&other, 0, false)?.kind, ReviewKind::OperatorReviewed);
    assert!(ready(&controls.inspect()?));
    assert!(controls.undecoded_failures()?.is_empty());
    Ok(())
}

/// Read failures are classified by their typed cause, never their text:
/// pruned state, a missing block and an undecodable block are lasting;
/// timeouts, transport losses and gateway errors pass. A wrapped
/// (`Unresolved`) cause is classified by what it wraps.
#[test]
fn read_failures_are_classified_by_type() {
    use rtao_client::{ClientError, SubmitFailure};
    use rtao_rpc::RpcError;
    let rpc = |error: RpcError| SubmitFailure::Client(ClientError::Rpc(error));
    let cases = [
        (rpc(RpcError::StateDiscarded { block: "0x01".into() }), "state_discarded", true),
        (SubmitFailure::Client(ClientError::BlockNotFound(H256([1; 32]))), "block_not_found", true),
        (rpc(RpcError::Timeout { detail: "x".into() }), "timeout", false),
        (rpc(RpcError::Transport("x".into())), "transport", false),
        (rpc(RpcError::Closed), "transport", false),
        (SubmitFailure::Client(ClientError::Protocol("x".into())), "protocol", false),
        (
            SubmitFailure::Unresolved {
                hash: H256([2; 32]),
                cause: Box::new(rpc(RpcError::StateDiscarded { block: "0x01".into() })),
                reconciliation: None,
            },
            "state_discarded",
            true,
        ),
    ];
    for (error, class, lasting) in cases {
        let read = crate::read_failure(&error);
        assert_eq!((read.class.as_str(), read.lasting), (class, lasting), "{error}");
        // The text stays out of the Debug form (logged at info level).
        assert!(!format!("{read:?}").contains("detail"));
    }
}
