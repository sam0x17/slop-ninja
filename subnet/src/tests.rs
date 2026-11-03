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
        submit_deadline: None,
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
    // A task-owned root when SLOP_NINJA_TEST_TMPDIR is set, otherwise the
    // system temporary directory (TMPDIR). Each TempDir removes itself.
    let root = std::env::var_os("SLOP_NINJA_TEST_TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
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

/// Owner-row policy (rtao D-093) through the adapter: `Submit` work records
/// `"owner_rows":"submit"` in its immutable effects, `Hold` work keeps the
/// exact effects earlier builds wrote, and the same operation ID under
/// another policy is refused like changed input, without writing anything.
#[test]
fn owner_row_policy_is_journaled_with_the_work_and_immutable() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    let mut adapter = open(&path, config())?;
    enable_storage_fixture(&adapter)?;
    let controls = adapter.controls();
    let held = WeightWork { v2_target: Some([7; 32]), tempo: Some(47), ..work() };
    let Enqueued::Created(first) = adapter.enqueue(&held, row())? else { panic!("created") };
    let effects = controls.journal.snapshot()?.operations[&first].intent.effects.clone();
    // Byte-for-byte the projection this adapter wrote at the previous pin.
    assert_eq!(
        serde_json::to_string(&effects)?,
        r#"{"netuid":7,"mechanism":0,"input":{"ByUid":{"5":0.12345678901234568,"9":0.8765432109876543}},"version_key":3}"#
    );
    assert_eq!(controls.owner_row_policy(&first)?, Some(OwnerRowPolicy::Hold));
    // The same ID under Submit is refused and leaves the journal unchanged.
    let before = controls.journal.snapshot()?.revision;
    assert!(adapter.enqueue_with_owner_rows(&held, row(), OwnerRowPolicy::Submit).is_err());
    assert_eq!(controls.journal.snapshot()?.revision, before);
    assert!(controls.retire_superseded(&first, "superseded")?);

    let burn = BTreeMap::from([(Uid(2), 65535.0)]);
    let submit = WeightWork { tempo: Some(48), ..held.clone() };
    let Enqueued::Created(second) =
        adapter.enqueue_with_owner_rows(&submit, burn.clone(), OwnerRowPolicy::Submit)?
    else {
        panic!("created")
    };
    let effects = controls.journal.snapshot()?.operations[&second].intent.effects.clone();
    assert_eq!(effects["owner_rows"], serde_json::json!("submit"));
    assert_eq!(controls.owner_row_policy(&second)?, Some(OwnerRowPolicy::Submit));
    // A retry under the same policy joins; under Hold it is refused.
    assert_eq!(
        adapter.enqueue_with_owner_rows(&submit, burn.clone(), OwnerRowPolicy::Submit)?,
        Enqueued::Existing(second.clone())
    );
    assert!(adapter.enqueue(&submit, burn.clone()).is_err());
    // The policy survives a restart: the reopened scheduler reads the effects.
    drop(controls);
    drop(adapter);
    let mut adapter = open(&path, config())?;
    enable_storage_fixture(&adapter)?;
    assert_eq!(adapter.controls().owner_row_policy(&second)?, Some(OwnerRowPolicy::Submit));
    assert_eq!(
        adapter.enqueue_with_owner_rows(&submit, burn, OwnerRowPolicy::Submit)?,
        Enqueued::Existing(second)
    );
    assert_eq!(adapter.controls().owner_row_policy("unknown")?, None);
    Ok(())
}

/// Journal migration from an existing journal written at the previous rtao
/// pin, such as a live validator's (copied read-only to a scratch path; it is
/// private and never checked in). Set `SLOP_NINJA_MIGRATION_JOURNAL` to the
/// copy. The journal opens under its recorded identity, every retained
/// operation reads back as `Hold` with its effects bytes unchanged, every one
/// is recognized as existing work when queued again, and nothing but the
/// pause control and pause generation changes.
#[test]
#[ignore = "needs SLOP_NINJA_MIGRATION_JOURNAL (a private journal copy)"]
fn an_existing_journal_from_the_previous_pin_opens_and_replays_unchanged() -> Result<()> {
    let source = std::env::var_os("SLOP_NINJA_MIGRATION_JOURNAL")
        .context("set SLOP_NINJA_MIGRATION_JOURNAL")?;
    let bytes = std::fs::read(&source)?;
    let before: serde_json::Value = serde_json::from_slice(&bytes)?;
    let root = scratch()?;
    let path = root.path().join("journal.json");
    write_journal(&path, &bytes)?;
    let identity: ChainIdentity =
        serde_json::from_value(before[APPLICATION_KEY]["chain"].clone())?;
    let mut config = config();
    config.identity = identity;
    // The weight version and contract the work was queued under, as recorded
    // with it.
    config.weight_version = before["operations"]
        .as_object()
        .and_then(|ops| ops.values().next())
        .and_then(|op| op["intent"]["effects"]["version_key"].as_u64())
        .unwrap_or(0);
    config.semantic_contract = before["operations"]
        .as_object()
        .and_then(|ops| ops.values().next())
        .and_then(|op| op["intent"]["semantic_policy"].as_str())
        .unwrap_or("slop_ninja.validator.package-row.v1")
        .to_owned();
    let summary_before = JournalControls::open_existing(&path)?.inspect()?;
    let mut adapter = open(&path, config)?;
    let controls = adapter.controls();
    let image = controls.journal.snapshot()?;
    let operations = before["operations"].as_object().context("operations")?;
    assert_eq!(image.operations.len(), operations.len());
    let mut replayed = 0;
    for (id, old) in operations {
        let record = &image.operations[id];
        assert_eq!(
            serde_json::to_string(&record.intent.effects)?,
            serde_json::to_string(&old["intent"]["effects"])?,
            "{id}: effects changed"
        );
        assert_eq!(controls.owner_row_policy(id)?, Some(OwnerRowPolicy::Hold), "{id}");
        assert_eq!(serde_json::to_value(&record.attempts)?, old["attempts"], "{id}: attempts changed");
        let correlation: WeightCorrelation =
            serde_json::from_str(record.intent.correlation_id.as_deref().context("correlation")?)?;
        let WeightsInput::ByUid(row) = serde_json::from_value(record.intent.effects["input"].clone())? else {
            panic!("{id}: input is not by UID")
        };
        enable_storage_fixture(&adapter)?;
        assert_eq!(adapter.enqueue(&correlation.work, row)?, Enqueued::Existing(id.clone()), "{id}");
        replayed += 1;
    }
    let after = controls.inspect()?;
    assert_eq!(
        (after.operations, after.attempts, after.unresolved_attempts, after.finalized_failures),
        (
            summary_before.operations,
            summary_before.attempts,
            summary_before.unresolved_attempts,
            summary_before.finalized_failures
        )
    );
    // Checkpoints (slots and reveal watcher) load into the reopened scheduler
    // unchanged; only the pause generation and control moved.
    let reread: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    assert_eq!(reread["checkpoints"], before["checkpoints"]);
    // Nothing in it is refused by the previous pin's strict decoding.
    let blockers = controls.rollback_blockers()?;
    assert!(blockers.is_empty(), "rollback blockers: {blockers:?}");
    eprintln!(
        "replayed {replayed} operations; pending {}; rollback blockers {}",
        controls.pending_operations(MechId(0))?.len(),
        blockers.len()
    );
    Ok(())
}

/// D-095: a dispatch that provably signed nothing returns its cadence slot
/// and retries in the same epoch, reported as `Failed`, never `Unresolved`.
/// rtao checkpoints the retry as the slot's `unsigned_retry`. The adapter
/// opens such a checkpoint (written here as rtao writes it) unchanged, counts
/// no unresolved attempt for it, and needs no external resolution: the slot
/// is not uncertain, and nothing here calls `resolve_uncertainty`.
#[test]
fn an_unsigned_retry_checkpoint_loads_unchanged_and_needs_no_resolution() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    drop(open(&path, config())?);
    let id = identity();
    let scope = format!("weights:{}:{}:{}", hex::encode(id.genesis.0), id.netuid, id.hotkey);
    let checkpoint = serde_json::json!({
        "schema": 1,
        "slots": {"0": {
            "last_attempt_epoch": 61,
            "last_attempt_block": 8_354_100,
            "uncertain": false,
            "unsigned_retry": [8_354_104, 3]
        }},
        "reveals": {"pending": [], "unlocated": [], "outcomes": [], "last_observed_block": 8_354_101}
    });
    {
        let journal = OperationJournal::open(&path)?;
        journal.update(|image| {
            image.checkpoints.insert(scope.clone(), checkpoint.clone());
            Ok(())
        })?;
    }
    let adapter = open(&path, config())?;
    let image = adapter.controls.journal.snapshot()?;
    assert_eq!(image.checkpoints[&scope], checkpoint);
    let summary = adapter.controls.inspect()?;
    assert_eq!((summary.attempts, summary.unresolved_attempts, summary.finalized_failures), (0, 0, 0));
    assert!(adapter.last_attempt(MechId(0)).is_none());
    assert!(!adapter.is_recovered());
    drop(adapter);
    assert_eq!(
        OperationJournal::open(&path)?.snapshot()?.checkpoints[&scope],
        checkpoint
    );
    Ok(())
}

/// D-095 with durable work: after a journaled attempt released its slot
/// because it signed nothing, rtao checkpoints the slot as `uncertain` with
/// its `unsigned_retry`, and clears that itself in its next finalized
/// recovery (`a_journaled_release_survives_restart_and_commits_in_the_same_epoch`
/// upstream). The adapter opens that shape unchanged, counts no unresolved
/// attempt or failure, keeps in-process recovery after a call that signed
/// nothing, and has no pending work that would need external resolution.
#[test]
fn an_uncertain_unsigned_retry_checkpoint_needs_no_external_resolution() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    drop(open(&path, config())?);
    let id = identity();
    let scope = format!("weights:{}:{}:{}", hex::encode(id.genesis.0), id.netuid, id.hotkey);
    let checkpoint = serde_json::json!({
        "schema": 1,
        "slots": {"0": {
            "last_attempt_epoch": 61,
            "last_attempt_block": 8_354_100,
            "uncertain": true,
            "unsigned_retry": [8_354_104, 1]
        }},
        "reveals": {"pending": [], "unlocated": [], "outcomes": [], "last_observed_block": 8_354_101}
    });
    {
        let journal = OperationJournal::open(&path)?;
        journal.update(|image| {
            image.checkpoints.insert(scope.clone(), checkpoint.clone());
            Ok(())
        })?;
    }
    let adapter = open(&path, config())?;
    assert_eq!(adapter.controls.journal.snapshot()?.checkpoints[&scope], checkpoint);
    let summary = adapter.controls.inspect()?;
    assert_eq!((summary.attempts, summary.unresolved_attempts, summary.finalized_failures), (0, 0, 0));
    assert!(keeps_recovery_after_error(summary.attempts, &summary));
    assert!(adapter.controls.pending_operations(MechId(0))?.is_empty());
    assert!(adapter.last_attempt(MechId(0)).is_none());
    // The previous pin reads this checkpoint too (its slot is not strict).
    assert!(adapter.controls.rollback_blockers()?.is_empty());
    drop(adapter);
    assert_eq!(OperationJournal::open(&path)?.snapshot()?.checkpoints[&scope], checkpoint);
    Ok(())
}

/// A rollback to the previous pin: that build refuses `Submit` work it has
/// not handed back and checkpoint state from a followed non-native
/// commit-reveal tag. The check finds a queued per-tempo `Submit` burn row
/// that never signed, retires it for the rollback (the next tempo's
/// commitment, which in steady state is almost always queued), and then
/// reports nothing; it never retires `Hold` work or checkpoint state.
#[test]
fn a_rollback_check_finds_queued_submit_work_and_followed_tag_state() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    let mut adapter = open(&path, config())?;
    enable_storage_fixture(&adapter)?;
    let controls = adapter.controls();
    // Hold work blocks nothing.
    let held = WeightWork { v2_target: Some([7; 32]), tempo: Some(47), ..work() };
    let Enqueued::Created(first) = adapter.enqueue(&held, row())? else { panic!("created") };
    assert!(controls.rollback_blockers()?.is_empty());
    assert!(controls.retire_for_rollback()?.is_empty());
    assert!(controls.retire_superseded(&first, "superseded")?);
    // The next tempo's burn row, queued under Submit and not yet attempted.
    let burn = BTreeMap::from([(Uid(2), 65535.0)]);
    let next = WeightWork { tempo: Some(48), ..held.clone() };
    let Enqueued::Created(queued) = adapter.enqueue_with_owner_rows(&next, burn, OwnerRowPolicy::Submit)? else {
        panic!("created")
    };
    assert_eq!(
        controls.rollback_blockers()?,
        vec![RollbackBlocker { operation_id: Some(queued.clone()), field: "effects.owner_rows".into(), retirable: true }]
    );
    assert_eq!(controls.retire_for_rollback()?, vec![queued.clone()]);
    assert_eq!(controls.retired(&queued)?.as_deref(), Some("rollback_prepared"));
    assert!(controls.rollback_blockers()?.is_empty());
    drop(controls);
    drop(adapter);
    // Checkpoint state written only after following a non-native tag.
    let id = identity();
    let scope = format!("weights:{}:{}:{}", hex::encode(id.genesis.0), id.netuid, id.hotkey);
    {
        let journal = OperationJournal::open(&path)?;
        journal.update(|image| {
            image.checkpoints.insert(
                scope.clone(),
                serde_json::json!({
                    "schema": 1,
                    "slots": {},
                    "reveals": {
                        "pending": [{"netuid": 7, "followed_cr": {"tag": 5, "code": format!("0x{}", "11".repeat(32))}}],
                        "unlocated": [{"state": {"prepared_block": 1, "followed_cr": null}}],
                        "outcomes": [],
                        "last_observed_block": 9,
                        "revealed_code": [{"payload_version": 4, "code": format!("0x{}", "11".repeat(32)), "stored_tag": 5}]
                    }
                }),
            );
            Ok(())
        })?;
    }
    let controls = JournalControls::open_existing(&path)?;
    let fields: Vec<_> = controls.rollback_blockers()?.into_iter().map(|b| (b.operation_id, b.field, b.retirable)).collect();
    assert_eq!(
        fields,
        vec![
            (None, "checkpoint.reveals.pending.followed_cr".to_owned(), false),
            (None, "checkpoint.reveals.revealed_code".to_owned(), false),
            (None, "checkpoint.reveals.unlocated.state.followed_cr".to_owned(), false),
        ]
    );
    assert!(controls.retire_for_rollback()?.is_empty());
    assert_eq!(controls.rollback_blockers()?.len(), 3);
    Ok(())
}

/// D-095 and the journal's uncertainty rule: production code in the adapter
/// and the native validator never clears rtao's slot uncertainty by hand.
/// Uncertainty clears only through journaled recovery.
#[test]
fn production_code_never_calls_resolve_uncertainty() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut scanned = 0;
    for dir in ["src", "crates/validator/src"] {
        for entry in std::fs::read_dir(root.join(dir))? {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if path.extension().is_none_or(|e| e != "rs") || name.contains("tests") {
                continue;
            }
            let text = std::fs::read_to_string(&path)?;
            for (line, raw) in text.lines().enumerate() {
                if raw.trim_start().starts_with("#[cfg(test)]") {
                    break;
                }
                let code = raw.split("//").next().unwrap_or_default();
                assert!(
                    !code.contains("resolve_uncertainty"),
                    "{}:{}: resolve_uncertainty",
                    path.display(),
                    line + 1
                );
            }
            scanned += 1;
        }
    }
    assert!(scanned > 10, "scanned {scanned} files");
    Ok(())
}

/// Strict journal structures of rtao `b5553a76` ([`PREVIOUS_RTAO_PIN`]),
/// copied from `crates/rtao-neuron/src/weights/durable.rs` and
/// `weights/reveal.rs` at that commit: the field names and serde attributes
/// are verbatim; a field whose own type that build does not decode strictly
/// is `IgnoredAny`. Independent of the field lists `rollback_blockers` uses.
mod previous_pin {
    #![allow(dead_code)]
    use serde::Deserialize;
    use serde::de::IgnoredAny;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Checkpoint {
        schema: IgnoredAny,
        slots: IgnoredAny,
        reveals: RevealWatcher,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Work {
        netuid: IgnoredAny,
        mechanism: IgnoredAny,
        input: IgnoredAny,
        version_key: IgnoredAny,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Material {
        row: IgnoredAny,
        commit: IgnoredAny,
        preparation: RevealPreparation,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Pending {
        netuid: IgnoredAny,
        mechanism: IgnoredAny,
        hotkey: IgnoredAny,
        uid: IgnoredAny,
        uids: IgnoredAny,
        version_key: IgnoredAny,
        commit_epoch: IgnoredAny,
        reveal_epoch: IgnoredAny,
        predicted_first_block: IgnoredAny,
        round: IgnoredAny,
        ciphertext: IgnoredAny,
        inclusion_block: IgnoredAny,
        first_check: IgnoredAny,
        first_checked: IgnoredAny,
        first_reveal_hash: IgnoredAny,
        last_round: IgnoredAny,
        notices: IgnoredAny,
        warned_permit: IgnoredAny,
        ambiguous_reveal: IgnoredAny,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Unlocated {
        netuid: IgnoredAny,
        mechanism: IgnoredAny,
        hotkey: IgnoredAny,
        row: IgnoredAny,
        version_key: IgnoredAny,
        state: RevealPreparation,
        commit: IgnoredAny,
        report: IgnoredAny,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RevealWatcher {
        pending: Vec<Pending>,
        unlocated: Vec<Unlocated>,
        outcomes: IgnoredAny,
        last_observed_block: IgnoredAny,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RevealPreparation {
        prepared_block: IgnoredAny,
        uid: IgnoredAny,
        reveal_period: IgnoredAny,
        schedule_fields: IgnoredAny,
    }

    /// Whether a build at the previous pin decodes every weight structure
    /// it reads from `image`: the effects and attempt material of each
    /// weight record not handed back, and each weight checkpoint.
    pub fn opens(image: &rtao_client::journal::JournalImage) -> bool {
        let records = image.operations.values().filter(|record| {
            record.intent.scope.starts_with("weights:")
                && record.extensions.get("scheduler_handed_off") != Some(&serde_json::Value::Bool(true))
        });
        for record in records {
            if serde_json::from_value::<Work>(record.intent.effects.clone()).is_err() {
                return false;
            }
            if record.attempts.iter().any(|a| serde_json::from_value::<Material>(a.material.clone()).is_err()) {
                return false;
            }
        }
        image
            .checkpoints
            .iter()
            .filter(|(scope, _)| scope.starts_with("weights:"))
            .all(|(_, checkpoint)| serde_json::from_value::<Checkpoint>(checkpoint.clone()).is_ok())
    }
}

/// `rollback_blockers` agrees with the previous pin's own strict structures
/// (copied verbatim above): a journal holding queued `Submit` work, or
/// checkpoint state from a followed non-native tag, does not decode there
/// and is reported; after `retire_for_rollback` the Submit work decodes;
/// a journal of Hold work decodes and reports nothing.
#[test]
fn the_rollback_check_agrees_with_the_previous_pins_strict_structures() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    let snapshot = |path: &Path| -> Result<rtao_client::journal::JournalImage> {
        Ok(OperationJournal::open(path)?.snapshot()?)
    };
    {
        let mut adapter = open(&path, config())?;
        enable_storage_fixture(&adapter)?;
        let held = WeightWork { v2_target: Some([7; 32]), tempo: Some(47), ..work() };
        adapter.enqueue(&held, row())?;
    }
    assert!(previous_pin::opens(&snapshot(&path)?));
    assert!(JournalControls::open_existing(&path)?.rollback_blockers()?.is_empty());
    {
        let mut adapter = open(&path, config())?;
        enable_storage_fixture(&adapter)?;
        let controls = adapter.controls();
        for pending in controls.pending_operations(MechId(0))? {
            assert!(controls.retire_superseded(&pending.id, "superseded")?);
        }
        let next = WeightWork { v2_target: Some([7; 32]), tempo: Some(48), ..work() };
        adapter.enqueue_with_owner_rows(&next, BTreeMap::from([(Uid(2), 65535.0)]), OwnerRowPolicy::Submit)?;
    }
    assert!(!previous_pin::opens(&snapshot(&path)?));
    let controls = JournalControls::open_existing(&path)?;
    assert!(!controls.rollback_blockers()?.is_empty());
    assert_eq!(controls.retire_for_rollback()?.len(), 1);
    assert!(controls.rollback_blockers()?.is_empty());
    drop(controls);
    assert!(previous_pin::opens(&snapshot(&path)?));
    // A complete pending reveal and an unlocated commit as the previous pin
    // writes them decode there; the same with a field this pin adds do not.
    let pending = serde_json::json!({
        "netuid": 7, "mechanism": 0, "hotkey": "x", "uid": 2, "uids": [0], "version_key": 0,
        "commit_epoch": 1, "reveal_epoch": 2, "predicted_first_block": 3, "round": 4,
        "ciphertext": [], "inclusion_block": 5, "first_check": null, "first_checked": false,
        "first_reveal_hash": null, "last_round": 4, "notices": [], "warned_permit": false,
        "ambiguous_reveal": false
    });
    let state = serde_json::json!({"prepared_block": 1, "uid": 2, "reveal_period": 1, "schedule_fields": [360, 0, 0, 0, 0, 0]});
    let unlocated = serde_json::json!({
        "netuid": 7, "mechanism": 0, "hotkey": "x", "row": [[0], [65535]], "version_key": 0,
        "state": state, "commit": [[], 4], "report": {}
    });
    let id = identity();
    let scope = format!("weights:{}:{}:{}", hex::encode(id.genesis.0), id.netuid, id.hotkey);
    let mut followed_pending = pending.clone();
    followed_pending["followed_cr"] = serde_json::json!({"tag": 5});
    let mut followed_unlocated = unlocated.clone();
    followed_unlocated["state"]["followed_cr"] = serde_json::json!(null);
    let cases = [
        (serde_json::json!({"pending": [pending], "unlocated": [unlocated], "outcomes": [], "last_observed_block": 9}), true),
        (serde_json::json!({"pending": [followed_pending], "unlocated": [], "outcomes": [], "last_observed_block": 9}), false),
        (serde_json::json!({"pending": [], "unlocated": [followed_unlocated], "outcomes": [], "last_observed_block": 9}), false),
        (serde_json::json!({"pending": [], "unlocated": [], "outcomes": [], "last_observed_block": 9, "revealed_code": []}), false),
    ];
    for (reveals, opens) in cases {
        OperationJournal::open(&path)?.update(|image| {
            image
                .checkpoints
                .insert(scope.clone(), serde_json::json!({"schema": 1, "slots": {}, "reveals": reveals.clone()}));
            Ok(())
        })?;
        assert_eq!(previous_pin::opens(&snapshot(&path)?), opens, "{reveals}");
        assert_eq!(JournalControls::open_existing(&path)?.rollback_blockers()?.is_empty(), opens, "{reveals}");
    }
    Ok(())
}

/// Every rtao dependency in the workspace and the coordinator pins
/// [`RTAO_PIN`] by full commit, and [`PREVIOUS_RTAO_PIN`] is another commit:
/// a re-pin that leaves the constants (and the rollback check's field lists
/// they document) behind fails here.
#[test]
fn every_manifest_pins_rtao_at_the_recorded_commit() -> Result<()> {
    assert_eq!(RTAO_PIN.len(), 40);
    assert_eq!(PREVIOUS_RTAO_PIN.len(), 40);
    assert_ne!(RTAO_PIN, PREVIOUS_RTAO_PIN);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut pinned = 0;
    for manifest in [
        "Cargo.toml",
        "crates/validator/Cargo.toml",
        "crates/miner/Cargo.toml",
        "coordinator/Cargo.toml",
        "coordinator/parity/Cargo.toml",
    ] {
        let text = std::fs::read_to_string(root.join(manifest))?;
        for line in text.lines().filter(|line| line.contains("\"https://github.com/sam0x17/rtao\"")) {
            let rev = line
                .split("rev = \"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .with_context(|| format!("{manifest}: rtao dependency without a rev: {line}"))?;
            assert_eq!(rev, RTAO_PIN, "{manifest}: {line}");
            pinned += 1;
        }
    }
    assert!(pinned >= 30, "only {pinned} rtao dependencies found");
    Ok(())
}
