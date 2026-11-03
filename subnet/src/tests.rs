use super::*;
use rtao_client::{UnsignedExtrinsic, journal::Incompatibility};
use rtao_neuron::ChainClock;

struct RefuseAll;
impl SemanticGuard for RefuseAll {
    fn check<'a>(
        &'a self,
        _: &'a Client,
        _: &'a OperationIntent,
        _: &'a UnsignedExtrinsic,
    ) -> rtao_signer::BoxFuture<'a, std::result::Result<(), Incompatibility>> {
        Box::pin(async {
            Err(Incompatibility {
                code: "test_guard_refusal".into(),
                descriptor: None,
                detail: "Offline test guard never authorizes signing".into(),
                extensions: Default::default(),
            })
        })
    }
}

fn identity() -> ChainIdentity {
    ChainIdentity {
        genesis: H256([1; 32]),
        netuid: NetUid(7),
        hotkey: AccountId([2; 32]),
    }
}

fn config() -> SchedulerConfig {
    SchedulerConfig {
        identity: identity(),
        semantic_contract: "slopninja.test-contract.v1".into(),
        weight_version: 3,
        cadence: None,
        shielding: Shielding::Unshielded,
    }
}

fn work() -> WeightWork {
    WeightWork {
        manifest_hash: [3; 32],
        cohort: "synthetic-cohort".into(),
        revision: 1,
        mechanism: MechId(0),
        tempo: None,
        v2_target: None,
    }
}

fn row() -> BTreeMap<Uid, f64> {
    BTreeMap::from([(Uid(5), 0.12345678901234568), (Uid(9), 0.8765432109876543)])
}

fn scratch() -> Result<tempfile::TempDir> {
    let root = std::env::var_os("SLOP_NINJA_TEST_TMPDIR")
        .context("set SLOP_NINJA_TEST_TMPDIR to the recorded task temporary root")?;
    Ok(tempfile::Builder::new()
        .prefix("native-adapter-")
        .tempdir_in(root)?)
}

fn open(path: &Path, config: SchedulerConfig) -> Result<NativeWeightAdapter> {
    NativeWeightAdapter::open(
        path,
        config,
        Arc::new(RefuseAll),
        Arc::new(ChainClock::new(1_000_000)),
        ClockGuard::default(),
    )
}

// These unit fixtures exercise storage only. No client, signer or RPC is created.
// Recovery itself is deliberately not fabricated as chain evidence.
fn enable_storage_fixture(adapter: &NativeWeightAdapter) -> Result<()> {
    adapter
        .controls
        .journal
        .control(JournalControl::default())?;
    Ok(())
}

#[test]
fn stable_id_binds_all_application_and_chain_coordinates() -> Result<()> {
    let original = work().operation_id(&identity())?;
    assert_eq!(
        original,
        "slopninja:weights:v1:c32bdbedef1c2eb2fe1d13fbb298d2df62e157cbff62b199a24b71c19980489d"
    );
    assert_eq!(original, work().operation_id(&identity())?);
    let mut changed = work();
    changed.cohort.push('x');
    assert_ne!(original, changed.operation_id(&identity())?);
    changed = work();
    changed.revision += 1;
    assert_ne!(original, changed.operation_id(&identity())?);
    changed = work();
    changed.manifest_hash[0] += 1;
    assert_ne!(original, changed.operation_id(&identity())?);
    changed = work();
    changed.mechanism = MechId(1);
    assert_ne!(original, changed.operation_id(&identity())?);
    let mut other = identity();
    other.genesis.0[0] += 1;
    assert_ne!(original, work().operation_id(&other)?);
    other = identity();
    other.netuid.0 += 1;
    assert_ne!(original, work().operation_id(&other)?);
    other = identity();
    other.hotkey.0[0] += 1;
    assert_ne!(original, work().operation_id(&other)?);
    assert!(
        WeightWork {
            cohort: String::new(),
            ..work()
        }
        .operation_id(&identity())
        .is_err()
    );
    Ok(())
}

#[test]
fn repeated_delivery_is_idempotent_and_changed_row_conflicts() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    assert!(adapter.enqueue(&work(), row()).is_err());
    enable_storage_fixture(&adapter)?;
    let id = work().operation_id(&identity())?;
    assert_eq!(
        adapter.enqueue(&work(), row())?,
        Enqueued::Created(id.clone())
    );
    assert_eq!(adapter.enqueue(&work(), row())?, Enqueued::Existing(id));
    let before = adapter.controls.journal.snapshot()?.revision;
    let mut changed = row();
    changed.insert(Uid(9), 0.9);
    assert!(adapter.enqueue(&work(), changed).is_err());
    assert_eq!(adapter.controls.journal.snapshot()?.revision, before);
    assert_eq!(adapter.controls.inspect()?.operations, 1);
    Ok(())
}

fn hand_off(adapter: &NativeWeightAdapter, id: &str) -> Result<()> {
    adapter.controls.journal.update(|image| {
        image
            .operations
            .get_mut(id)
            .unwrap()
            .extensions
            .insert("scheduler_handed_off".into(), serde_json::Value::Bool(true));
        Ok(())
    })?;
    Ok(())
}

#[test]
fn per_tempo_recommits_have_distinct_ids_and_queue_one_at_a_time() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    enable_storage_fixture(&adapter)?;
    let base = work().operation_id(&identity())?;
    // The first commit keeps the pre-tempo identity and exact correlation.
    assert_eq!(
        base,
        slop_ninja_protocol::weight_operation_id(
            &[1; 32],
            7,
            &[2; 32],
            &[3; 32],
            "synthetic-cohort",
            1,
            0
        )?
    );
    let correlation = serde_json::to_string(&WeightCorrelation {
        schema: 1,
        work: work(),
        row_hash: weight_row_hash(row())?,
    })?;
    assert!(!correlation.contains("tempo"));
    let first = work().for_tempo(40);
    let second = work().for_tempo(41);
    let first_id = first.operation_id(&identity())?;
    assert_eq!(
        first_id,
        slop_ninja_protocol::weight_tempo_operation_id(&base, 40)?
    );
    assert_ne!(first_id, second.operation_id(&identity())?);
    assert!(work().same_target(&second) && first.same_target(&second));
    assert!(!work().same_target(&WeightWork {
        revision: 2,
        ..work()
    }));

    adapter.enqueue(&work(), row())?;
    // While the first commit is pending, no re-commit can be queued.
    assert!(adapter.enqueue(&first, row()).is_err());
    hand_off(&adapter, &base)?;
    assert_eq!(
        adapter.enqueue(&first, row())?,
        Enqueued::Created(first_id.clone())
    );
    assert_eq!(
        adapter.enqueue(&first, row())?,
        Enqueued::Existing(first_id.clone())
    );
    assert!(adapter.enqueue(&second, row()).is_err());
    // A re-commit with another row conflicts with its retained identity.
    let mut changed = row();
    changed.insert(Uid(9), 0.9);
    assert!(adapter.enqueue(&first, changed).is_err());
    hand_off(&adapter, &first_id)?;
    adapter.enqueue(&second, row())?;

    // Another target's work is never listed with this one.
    let listed = adapter.controls.target_operations(&work())?;
    let tempos: Vec<_> = listed.iter().map(|op| op.tempo).collect();
    assert_eq!(tempos, vec![None, Some(40), Some(41)]);
    assert_eq!(
        listed.iter().map(|op| op.handed_off).collect::<Vec<_>>(),
        vec![true, true, false]
    );
    assert!(
        listed
            .iter()
            .all(|op| op.row_hash == weight_row_hash(row()).unwrap())
    );
    assert!(
        adapter
            .controls
            .target_operations(&WeightWork {
                revision: 2,
                ..work()
            })?
            .is_empty()
    );
    Ok(())
}

#[test]
fn completed_redelivery_remains_idempotent_with_later_pending_work() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    enable_storage_fixture(&adapter)?;
    adapter.enqueue(&work(), row())?;
    let id = work().operation_id(&identity())?;
    adapter.controls.journal.update(|image| {
        image
            .operations
            .get_mut(&id)
            .unwrap()
            .extensions
            .insert("scheduler_handed_off".into(), serde_json::Value::Bool(true));
        Ok(())
    })?;
    let next = WeightWork {
        revision: 2,
        ..work()
    };
    adapter.enqueue(&next, row())?;
    assert_eq!(adapter.enqueue(&work(), row())?, Enqueued::Existing(id));
    assert_eq!(adapter.controls.inspect()?.operations, 2);
    Ok(())
}

#[test]
fn native_reopen_retains_work_and_pause() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    {
        let mut adapter = open(&path, config())?;
        enable_storage_fixture(&adapter)?;
        adapter.enqueue(&work(), row())?;
        adapter.controls.pause()?;
    }
    let mut adapter = open(&path, config())?;
    assert!(adapter.controls.inspect()?.paused);
    assert_eq!(adapter.controls.inspect()?.operations, 1);
    assert_eq!(
        adapter.enqueue(&work(), row())?,
        Enqueued::Existing(work().operation_id(&identity())?)
    );
    assert!(adapter.resume_after_recovery().is_err());
    let record = adapter
        .controls
        .journal
        .snapshot()?
        .operations
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(
        record.intent.semantic_policy.as_deref(),
        Some("slopninja.test-contract.v1")
    );
    let correlation: WeightCorrelation =
        serde_json::from_str(record.intent.correlation_id.as_deref().unwrap())?;
    assert_eq!(correlation.work, work());
    assert_eq!(correlation.row_hash, row_hash(&row()));
    Ok(())
}

#[test]
fn restart_pauses_a_previously_resumed_journal() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    {
        let adapter = open(&path, config())?;
        enable_storage_fixture(&adapter)?;
    }
    assert!(open(&path, config())?.controls.inspect()?.paused);
    Ok(())
}

#[test]
fn wrong_identity_preserves_existing_bytes() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    {
        let _adapter = open(&path, config())?;
    }
    let before = std::fs::read(&path)?;
    for field in 0..3 {
        let mut other = config();
        match field {
            0 => other.identity.genesis.0[0] += 1,
            1 => other.identity.netuid.0 += 1,
            _ => other.identity.hotkey.0[0] += 1,
        }
        assert!(open(&path, other).is_err());
        assert_eq!(std::fs::read(&path)?, before);
    }
    Ok(())
}

#[test]
fn one_native_owner_and_shared_handle_retains_lock() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    let adapter = open(&path, config())?;
    let controls = adapter.controls();
    assert!(open(&path, config()).is_err());
    drop(adapter);
    assert!(JournalControls::open_existing(&path).is_err());
    drop(controls);
    assert!(open(&path, config()).is_ok());
    Ok(())
}

#[test]
fn missing_contract_and_shielded_requests_fail_before_creating_storage() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    let mut missing = config();
    missing.semantic_contract = "  ".into();
    assert!(open(&path, missing).is_err());
    for shielding in [Shielding::Required, Shielding::Automatic] {
        let mut shielded = config();
        shielded.shielding = shielding;
        assert!(open(&path, shielded).is_err());
    }
    assert!(!path.exists());
    Ok(())
}

#[test]
fn pause_invalidates_a_previous_recovery_generation() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    // Inject only the storage gate for this race regression; no chain is contacted.
    adapter.recovered = true;
    adapter.recovered_generation = Some(adapter.controls.inspect()?.generation);
    adapter.controls().pause()?;
    assert!(adapter.resume_after_recovery().is_err());
    assert!(adapter.controls.inspect()?.paused);
    Ok(())
}

#[test]
fn external_recovery_entry_persists_pause_before_observation() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    enable_storage_fixture(&adapter)?;
    adapter.recovered = true;
    adapter.recovered_generation = Some(adapter.controls.inspect()?.generation);
    let previous_pause = pause_epoch(&adapter.controls.journal.snapshot()?)?;
    assert!(adapter.begin_recovery()? > previous_pause);
    assert!(adapter.controls.inspect()?.paused);
    assert!(!adapter.recovered);
    assert!(adapter.recovered_generation.is_none());
    assert!(adapter.resume_after_recovery().is_err());
    Ok(())
}

#[test]
fn unknown_nonempty_journal_is_not_adopted() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    {
        let journal = OperationJournal::open(&path)?;
        journal.update(|image| {
            image.checkpoints.insert(
                "other-application".into(),
                serde_json::json!({"secret": "synthetic"}),
            );
            Ok(())
        })?;
    }
    let before = std::fs::read(&path)?;
    assert!(open(&path, config()).is_err());
    assert_eq!(std::fs::read(&path)?, before);
    Ok(())
}

#[test]
fn invalid_rows_never_create_an_operation() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    enable_storage_fixture(&adapter)?;
    for value in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
        assert!(
            adapter
                .enqueue(&work(), BTreeMap::from([(Uid(1), value)]))
                .is_err()
        );
    }
    assert_eq!(adapter.controls.inspect()?.operations, 0);
    Ok(())
}

/// Append a signed attempt as rtao persists it before network dispatch, with
/// no response recorded: the shape a timeout, a 429 or a crash after send
/// leaves. It satisfies rtao's attempt-graph invariants.
fn push_sent_without_response(record: &mut rtao_client::journal::OperationRecord) {
    use blake2::{Blake2b, Digest as _, digest::consts::U32};
    let bytes = vec![1u8, 2, 3];
    let mut attempt: rtao_client::journal::SignedAttempt =
        serde_json::from_value(serde_json::json!({
            "id": record.attempts.len(),
            "role": "Ordinary",
            "material_generation": 0,
            "hash": H256([0x5a; 32]),
            "bytes": [1, 2, 3],
            "nonce": 7,
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
    record.mode = rtao_client::journal::ExecutionMode::Ordinary;
    record.attempts.push(attempt);
}

#[test]
fn lost_response_after_send_is_never_signed_again_after_restart() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    let id = work().operation_id(&identity())?;
    {
        let mut adapter = open(&path, config())?;
        enable_storage_fixture(&adapter)?;
        adapter.enqueue(&work(), row())?;
        adapter.controls.journal.update(|image| {
            let record = image.operations.get_mut(&id).unwrap();
            push_sent_without_response(record);
            Ok(())
        })?;
        // The process dies here: no outcome, no reveal, no pause from the app.
    }
    for _restart in 0..3 {
        let mut adapter = open(&path, config())?;
        let summary = adapter.controls.inspect()?;
        assert!(summary.paused);
        assert_eq!(
            (
                summary.operations,
                summary.attempts,
                summary.unresolved_attempts
            ),
            (1, 1, 1)
        );
        // Redelivering the same verified row joins the retained operation.
        assert_eq!(
            adapter.enqueue(&work(), row())?,
            Enqueued::Existing(id.clone())
        );
        // A changed row under the same identity is refused, not queued anew.
        let mut changed = row();
        changed.insert(Uid(9), 0.5);
        assert!(adapter.enqueue(&work(), changed).is_err());
        // No signer can be attached until finalized history resolves the
        // retained bytes; only rtao's recovery may classify them.
        assert!(adapter.resume_after_recovery().is_err());
        let status = adapter.controls.operation(&id)?.unwrap();
        assert_eq!((status.attempts, status.unresolved), (1, 1));
        assert_eq!(status.finalized_success_block, None);
    }
    let snapshot = OperationJournal::open(&path)?.snapshot()?;
    let record = &snapshot.operations[&id];
    assert_eq!(record.attempts.len(), 1);
    assert_eq!(record.attempts[0].bytes, vec![1, 2, 3]);
    assert_eq!(record.attempts[0].outcome, AttemptOutcome::Unresolved);
    Ok(())
}

#[test]
fn row_hash_is_public_and_canonical() -> Result<()> {
    let mut negative_zero = row();
    negative_zero.insert(Uid(11), -0.0);
    let mut zero = row();
    zero.insert(Uid(11), 0.0);
    assert_eq!(weight_row_hash(negative_zero)?, weight_row_hash(zero)?);
    assert_eq!(weight_row_hash(row())?, row_hash(&row()));
    assert!(weight_row_hash(BTreeMap::new()).is_err());
    Ok(())
}

#[path = "surface_tests.rs"]
mod surface_tests;

/// A journal written by this adapter at rtao `aa8627ba` (journal schema 2), with
/// one handed-off operation, one operation holding an unresolved attempt, and a
/// retired testnet runtime-qualification record. See the fixture README.
const SCHEMA2_JOURNAL: &[u8] = include_bytes!("../fixtures/journal-schema2/journal.json");

/// Journals are owner-only files; rtao refuses group or other access.
fn write_journal(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn schema2_config() -> SchedulerConfig {
    let mut config = config();
    config.identity.genesis = H256(slop_ninja_protocol::TESTNET_GENESIS.0);
    config.identity.netuid = NetUid(4242);
    config
}

#[test]
fn schema2_journal_migrates_in_place_and_retains_work() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    write_journal(&path, SCHEMA2_JOURNAL)?;
    let before: serde_json::Value = serde_json::from_slice(SCHEMA2_JOURNAL)?;
    assert_eq!(before["schema_version"], 2);
    {
        let controls = JournalControls::open_existing(&path)?;
        assert_eq!(controls.identity(), &schema2_config().identity);
        let summary = controls.inspect()?;
        assert_eq!(summary.schema, rtao_client::journal::JOURNAL_SCHEMA);
        assert_eq!(summary.operations, 2);
        assert_eq!(summary.attempts, 1);
        assert_eq!(summary.unresolved_attempts, 1);
        assert!(summary.paused);
        let image = controls.journal.snapshot()?;
        // Unknown application evidence survives migration unchanged and gates nothing.
        assert_eq!(
            image.extensions["slopninja.testnet-runtime-qualification.v1"],
            before["slopninja.testnet-runtime-qualification.v1"]
        );
        let id = WeightWork {
            revision: 2,
            ..work()
        }
        .operation_id(&schema2_config().identity)?;
        let attempt = &image.operations[&id].attempts[0];
        let old = &before["operations"][&id]["attempts"][0];
        assert_eq!(serde_json::to_value(attempt.hash.0)?, old["hash"]);
        assert_eq!(serde_json::to_value(&attempt.bytes)?, old["bytes"]);
        assert_eq!(attempt.outcome, AttemptOutcome::Unresolved);
    }
    // The original operation identities still hold after the migration: the same
    // cohort work is recognized rather than duplicated.
    let mut adapter = open(&path, schema2_config())?;
    enable_storage_fixture(&adapter)?;
    assert!(matches!(
        adapter.enqueue(&work(), row())?,
        Enqueued::Existing(_)
    ));
    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    assert_eq!(
        after["schema_version"],
        rtao_client::journal::JOURNAL_SCHEMA
    );
    assert_eq!(after["operations"].as_object().map(|o| o.len()), Some(2));
    Ok(())
}

#[test]
fn unsupported_or_lossy_journals_are_refused_without_reset() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    let mut future: serde_json::Value = serde_json::from_slice(SCHEMA2_JOURNAL)?;
    future["schema_version"] = serde_json::json!(99);
    let mut colliding: serde_json::Value = serde_json::from_slice(SCHEMA2_JOURNAL)?;
    let id = WeightWork {
        revision: 2,
        ..work()
    }
    .operation_id(&schema2_config().identity)?;
    // A legacy field already named like a schema-3 field cannot be migrated safely.
    colliding["operations"][&id]["attempts"][0]["role"] = serde_json::json!("Carrier");
    for image in [future, colliding] {
        let bytes = serde_json::to_vec(&image)?;
        write_journal(&path, &bytes)?;
        let error = JournalControls::open_existing(&path)
            .err()
            .context("refused")?;
        assert!(
            error.to_string().contains("schema") || error.to_string().contains("retained"),
            "{error}"
        );
        assert!(open(&path, schema2_config()).is_err());
        assert_eq!(std::fs::read(&path)?, bytes);
    }
    Ok(())
}

/// A pending operation that never landed is retired with a journaled reason,
/// after which another operation of the mechanism can be queued: a successor
/// target, the tempo after a missed one, or a re-derived row. An operation
/// rtao handed back, or an unknown one, is left alone.
#[test]
fn a_never_signed_pending_operation_is_retired_so_the_queue_moves_on() -> Result<()> {
    let root = scratch()?;
    let mut adapter = open(&root.path().join("journal.json"), config())?;
    enable_storage_fixture(&adapter)?;
    let controls = adapter.controls();
    let t1 = WeightWork { v2_target: Some([7; 32]), tempo: Some(47), ..work() };
    let Enqueued::Created(first) = adapter.enqueue(&t1, row())? else { panic!("created") };
    let pending = controls.pending_operations(MechId(0))?;
    assert_eq!(pending.len(), 1);
    assert!(pending[0].retirable());
    assert_eq!(pending[0].work.as_ref().and_then(|w| w.tempo), Some(47));
    // The successor is refused while the first is pending (rtao's queue rule).
    let t2 = WeightWork { manifest_hash: [8; 32], v2_target: Some([9; 32]), tempo: Some(47), ..work() };
    assert!(format!("{:#}", adapter.enqueue(&t2, row()).unwrap_err()).contains("reconcile the existing mechanism operation"));
    assert!(controls.retire_superseded(&first, "Bad Reason").is_err());
    assert!(controls.retire_superseded(&first, "superseded")?);
    assert_eq!(controls.retired(&first)?.as_deref(), Some("superseded"));
    assert_eq!(controls.retained_row(&first)?.map(|(_, handed_off)| handed_off), Some(true));
    assert!(controls.pending_operations(MechId(0))?.is_empty());
    // Retiring twice, or an unknown operation, writes nothing.
    assert!(!controls.retire_superseded(&first, "superseded")?);
    assert!(!controls.retire_superseded("unknown", "superseded")?);
    let Enqueued::Created(second) = adapter.enqueue(&t2, row())? else { panic!("created") };
    assert_ne!(first, second);
    // The retired operation's immutable row is still retained: the same
    // identity with another row is refused, so a re-derived row needs a new label.
    let mut rederived = row();
    rederived.insert(Uid(11), 0.5);
    assert!(adapter.enqueue(&t1, rederived).is_err());
    Ok(())
}
