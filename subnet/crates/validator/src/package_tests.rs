//! Offline tests of the T12 package path: verification before enqueue, row
//! derivation, the signing guard's application checks, and restart behavior
//! at every application fault boundary with a real file journal.
//!
//! Signing keys are public deterministic dev seeds. No client, signer, RPC
//! endpoint or chain is contacted, and no real key is read.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use rtao_client::journal::{OperationIntent, OperationJournal};
use rtao_primitives::{AccountId, H256, NetUid, Uid};
use slop_ninja_protocol::{
    ChainBinding, NeuronIdentity, RegistrationSnapshot, SigningKey, TESTNET_GENESIS, TrustedKeys,
};
use slop_ninja_subnet::{ChainIdentity, Enqueued, WeightWork};

use crate::app::{AppOutcome, AppPorts, DispatchReport, Mode, apply};
use crate::config::ValidatorConfig;
use crate::faults::{Faults, InjectedFault, Phase};
use crate::fixture::{FixtureParams, build};
use crate::guard::{Authorized, PACKAGE_CONTRACT, check_call_name, check_intent};
use crate::live::{
    LivePorts, account_from_value, commits_from_value, init_journal, weights_from_value,
};
use crate::package::{self, Bundle, Derived};
use crate::payload::DryRunReport;
use crate::startup::FinalizedHead;

const NETUID: u16 = 601;

/// Public dev seeds for the three application key roles. Test data only.
fn dev_key(byte: u8) -> SigningKey {
    SigningKey::from_bytes(&[byte; 32])
}

fn package_key() -> SigningKey {
    dev_key(0x61)
}
fn receipt_key() -> SigningKey {
    dev_key(0x62)
}
fn authorization_key() -> SigningKey {
    dev_key(0x63)
}

fn trusted() -> TrustedKeys {
    TrustedKeys::new(
        package_key().verifying_key().to_bytes(),
        receipt_key().verifying_key().to_bytes(),
        authorization_key().verifying_key().to_bytes(),
    )
    .unwrap()
}

fn neuron(uid: u16, hotkey: u8, block: u64) -> NeuronIdentity {
    NeuronIdentity {
        uid,
        hotkey: slop_ninja_protocol::AccountId([hotkey; 32]),
        registration_block: block,
    }
}

fn owner() -> NeuronIdentity {
    neuron(0, 0x0f, 8_331_000)
}
fn validator2() -> NeuronIdentity {
    neuron(1, 0x22, 8_340_000)
}
fn miner1() -> NeuronIdentity {
    neuron(2, 0x33, 8_340_010)
}

fn params() -> FixtureParams {
    FixtureParams {
        netuid: NETUID,
        mechanism: 0,
        cohort: 1,
        cohort_label: "testnet-t12-cohort-1".into(),
        revision: 1,
        opened_block: 8_340_120,
        burn_identity: owner(),
        miners: vec![miner1()],
    }
}

fn bundle_for(params: &FixtureParams) -> Bundle {
    build(params, &package_key(), &receipt_key()).unwrap()
}

fn bundle() -> Bundle {
    bundle_for(&params())
}

fn chain() -> ChainBinding {
    params().chain()
}

fn registered() -> RegistrationSnapshot {
    package::snapshot(
        &chain(),
        8_340_500,
        [0x44; 32],
        vec![miner1(), owner(), validator2()],
    )
    .unwrap()
}

fn verify(bundle: &Bundle) -> std::result::Result<package::VerifiedPackage, package::Refusal> {
    package::verify(bundle, &trusted(), &TESTNET_GENESIS.0, NETUID)
}

fn code(bundle: &Bundle) -> &'static str {
    verify(bundle).map(|_| "ok").unwrap_or_else(|r| r.code)
}

/// Flip one bit in the middle of a hex field's bytes.
fn flip(hex_field: &str) -> String {
    let mut bytes = hex::decode(hex_field).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 0x01;
    hex::encode(bytes)
}

#[test]
fn fixture_bundle_verifies_and_derives_the_miner_row() -> Result<()> {
    let bundle = bundle();
    // The JSON form round-trips and the builder is deterministic.
    assert_eq!(Bundle::parse(bundle.to_json().as_bytes()).unwrap(), bundle);
    assert_eq!(bundle_for(&params()), bundle);
    let verified = verify(&bundle).unwrap();
    let summary = verified.summary();
    assert_eq!(
        (summary.cohort, summary.revision, summary.mechanism),
        (1, 1, 0)
    );
    // Four tasks for the baseline and four for the miner, one receipt each.
    assert_eq!(summary.receipt_count, 8);
    let derived = package::derive(&verified, &registered()).unwrap();
    assert_eq!(derived.entries(), vec![(2, 65535)]);
    assert_eq!(derived.input, BTreeMap::from([(Uid(2), 65535.0)]));
    assert_eq!(derived.work.cohort, "testnet-t12-cohort-1");
    assert_eq!(derived.work.revision, 1);
    assert_eq!(
        derived.work.manifest_hash,
        verified.target.cohort_manifest.0
    );
    Ok(())
}

#[test]
fn two_miners_share_credit_by_their_improvement() -> Result<()> {
    let mut two = params();
    two.miners.push(neuron(3, 0x34, 8_340_020));
    let verified = verify(&bundle_for(&two)).unwrap();
    let snapshot = package::snapshot(
        &chain(),
        8_340_500,
        [0x44; 32],
        vec![owner(), validator2(), miner1(), neuron(3, 0x34, 8_340_020)],
    )
    .unwrap();
    let row = package::derive(&verified, &snapshot).unwrap().entries();
    assert_eq!(row.len(), 2);
    assert_eq!(row[0], (2, 65535));
    assert!(row[1].0 == 3 && row[1].1 > 0 && row[1].1 < 65535);
    Ok(())
}

#[test]
fn every_tampered_or_mismatched_bundle_is_refused_before_a_row_exists() {
    let good = bundle();
    let mut cases: Vec<(&str, Bundle)> = Vec::new();
    for field in ["manifest", "package", "target", "receipt_index"] {
        let mut bad = good.clone();
        let slot = match field {
            "manifest" => &mut bad.manifest,
            "package" => &mut bad.package,
            "target" => &mut bad.target,
            _ => &mut bad.receipt_index,
        };
        *slot = flip(slot);
        let expected = match field {
            "manifest" => "manifest_unverified",
            "package" => "package_unverified",
            "target" => "target_unverified",
            _ => "receipt_index_unverified",
        };
        cases.push((expected, bad));
    }
    let mut bad = good.clone();
    bad.receipts[3] = flip(&bad.receipts[3]);
    cases.push(("receipt_object_mismatch", bad));
    let mut bad = good.clone();
    bad.receipts.pop();
    cases.push(("receipts_incomplete", bad));
    let mut bad = good.clone();
    bad.receipts.swap(0, 1);
    cases.push(("receipt_object_mismatch", bad));
    // Records from another cohort, each validly signed.
    let mut other = params();
    other.cohort = 2;
    other.cohort_label = "testnet-t12-cohort-2".into();
    let other = bundle_for(&other);
    let mut bad = good.clone();
    bad.package = other.package.clone();
    cases.push(("package_manifest_mismatch", bad));
    let mut bad = good.clone();
    bad.target = other.target.clone();
    cases.push(("target_package_mismatch", bad));
    let mut bad = good.clone();
    bad.receipt_index = other.receipt_index.clone();
    cases.push(("receipt_index_mismatch", bad));
    // Records signed under the wrong role or by an untrusted key.
    let mut bad = good.clone();
    std::mem::swap(&mut bad.package, &mut bad.receipt_index);
    cases.push(("package_unverified", bad));
    let rogue = build(&params(), &dev_key(0x70), &receipt_key()).unwrap();
    let mut bad = good.clone();
    bad.package = rogue.package;
    cases.push(("package_unverified", bad));
    for (expected, bundle) in &cases {
        assert_eq!(code(bundle), *expected);
    }
    assert_eq!(code(&good), "ok");
}

#[test]
fn a_package_for_another_subnet_or_chain_is_refused() {
    let bundle = bundle();
    let refused = |genesis: &[u8; 32], netuid| {
        package::verify(&bundle, &trusted(), genesis, netuid)
            .unwrap_err()
            .code
    };
    assert_eq!(refused(&TESTNET_GENESIS.0, 602), "package_chain_mismatch");
    assert_eq!(refused(&[0x2f; 32], NETUID), "package_chain_mismatch");
    // A correctly signed package built for another subnet is not this one's.
    let mut elsewhere = params();
    elsewhere.netuid = 602;
    assert_eq!(code(&bundle_for(&elsewhere)), "package_chain_mismatch");
}

#[test]
fn bundle_format_is_strict() {
    let text = bundle().to_json();
    for bad in [
        text.replace(package::BUNDLE_SCHEMA, "slop_ninja.fixture_package/v2"),
        text.replace("\"T12\"", "\"T13\""),
        text.replacen('{', "{\"extra\": 1,", 1),
        "not json".to_owned(),
    ] {
        assert!(Bundle::parse(bad.as_bytes()).is_err());
    }
}

#[test]
fn registration_changes_move_credit_or_refuse_the_row() {
    let verified = verify(&bundle()).unwrap();
    // The miner's UID was reused by a new registration: credit goes to UID 0.
    let reused = package::snapshot(
        &chain(),
        8_340_600,
        [0x45; 32],
        vec![owner(), validator2(), neuron(2, 0x99, 8_340_590)],
    )
    .unwrap();
    assert_eq!(
        package::derive(&verified, &reused).unwrap().entries(),
        vec![(0, 65535)]
    );
    // Someone else holds UID 0: no row is derived at all.
    let foreign = package::snapshot(
        &chain(),
        8_340_600,
        [0x45; 32],
        vec![neuron(0, 0xee, 1), validator2(), miner1()],
    )
    .unwrap();
    assert_eq!(
        package::derive(&verified, &foreign).unwrap_err().code,
        "row_refused"
    );
    // Duplicate UIDs are an invalid snapshot.
    assert!(package::snapshot(&chain(), 1, [0; 32], vec![owner(), owner()]).is_err());
}

fn intent(id: &str, row_hash: &str, contract: &str) -> OperationIntent {
    OperationIntent {
        id: id.into(),
        correlation_id: Some(
            serde_json::json!({
                "schema": 1,
                "work": WeightWork {
                    manifest_hash: [3; 32],
                    cohort: "c".into(),
                    revision: 1,
                    mechanism: rtao_primitives::MechId(0),
                    tempo: None,
                    v2_target: None,
                },
                "row_hash": row_hash,
            })
            .to_string(),
        ),
        genesis: H256(TESTNET_GENESIS.0),
        account: AccountId([2; 32]),
        scope: "weights".into(),
        effects: serde_json::json!({}),
        obligation: false,
        semantic_policy: Some(contract.into()),
        extensions: BTreeMap::new(),
    }
}

#[test]
fn guard_signs_only_verified_rows_as_timelocked_commits() {
    let authorized = Authorized::default();
    let refusal = |result: std::result::Result<(), rtao_client::journal::Incompatibility>| {
        result.unwrap_err().code
    };
    assert_eq!(
        refusal(check_intent(
            &authorized,
            &intent("op", "h", PACKAGE_CONTRACT)
        )),
        "slop_ninja_package_unverified"
    );
    authorized.insert("op", "h");
    assert!(check_intent(&authorized, &intent("op", "h", PACKAGE_CONTRACT)).is_ok());
    assert_eq!(
        refusal(check_intent(
            &authorized,
            &intent("op", "other", PACKAGE_CONTRACT)
        )),
        "slop_ninja_row_mismatch"
    );
    assert_eq!(
        refusal(check_intent(
            &authorized,
            &intent("op", "h", "slopninja.validator-skeleton.refuse-all.v1")
        )),
        "slop_ninja_contract_mismatch"
    );
    let mut missing = intent("op", "h", PACKAGE_CONTRACT);
    missing.correlation_id = None;
    assert_eq!(
        refusal(check_intent(&authorized, &missing)),
        "slop_ninja_correlation_missing"
    );
    for call in [
        "commit_timelocked_mechanism_weights",
        "commit_timelocked_weights",
    ] {
        assert!(check_call_name("SubtensorModule", call).is_ok());
    }
    for (pallet, call) in [
        ("SubtensorModule", "set_mechanism_weights"),
        ("SubtensorModule", "set_weights"),
        ("SubtensorModule", "commit_weights"),
        ("Utility", "commit_timelocked_mechanism_weights"),
    ] {
        assert_eq!(
            refusal(check_call_name(pallet, call)),
            "slop_ninja_direct_weights_refused"
        );
    }
}

#[test]
fn storage_values_read_by_name_decode_in_every_rendering() -> Result<()> {
    use rtao_core::codec::Value;
    let account = [0x5a; 32];
    let ss58 = AccountId(account).to_string();
    for value in [
        Value::Str(format!("0x{}", hex::encode(account))),
        Value::Str(ss58),
        Value::Bytes(account.to_vec()),
        Value::Tuple(vec![Value::Bytes(account.to_vec())]),
        Value::List(account.iter().map(|b| Value::Int((*b).into())).collect()),
    ] {
        assert_eq!(account_from_value(&value)?, account);
    }
    assert!(account_from_value(&Value::Bytes(vec![1; 31])).is_err());
    assert!(account_from_value(&Value::Int(1)).is_err());
    let pairs = Value::List(vec![
        Value::Tuple(vec![Value::Int(2), Value::Int(65535)]),
        Value::List(vec![Value::Uint(3), Value::Int(17)]),
    ]);
    assert_eq!(weights_from_value(&pairs)?, vec![(2, 65535), (3, 17)]);
    assert_eq!(weights_from_value(&Value::Null)?, Vec::new());
    assert!(weights_from_value(&Value::List(vec![Value::Int(70_000)])).is_err());
    let commits = Value::List(vec![Value::Tuple(vec![
        Value::Str(format!("0x{}", hex::encode(account))),
        Value::Int(8_340_620),
        Value::Bytes(vec![1, 2, 3]),
        Value::Uint(27_123_456),
    ])]);
    assert_eq!(
        commits_from_value(&commits)?,
        vec![(account, 8_340_620, 27_123_456)]
    );
    assert!(commits_from_value(&Value::List(vec![Value::Int(1)])).is_err());
    Ok(())
}

// Application fault boundaries with the real native journal.

fn scratch() -> Result<tempfile::TempDir> {
    let root = std::env::var_os("SLOP_NINJA_TEST_TMPDIR")
        .context("set SLOP_NINJA_TEST_TMPDIR to the recorded task temporary root")?;
    Ok(tempfile::Builder::new()
        .prefix("m2-app-")
        .tempdir_in(root)?)
}

fn config(root: &Path) -> Result<ValidatorConfig> {
    let text = include_str!("../validator.example.toml")
        .replace("netuid = 0", &format!("netuid = {NETUID}"))
        .replace(
            "/home/operator/.local/state/slop_ninja_validator/weights-journal.json",
            &root.join("journal.json").display().to_string(),
        )
        .replace(
            "/home/operator/.bittensor/wallets",
            &root.join("wallets").display().to_string(),
        );
    ValidatorConfig::parse(&text)
}

fn identity() -> ChainIdentity {
    ChainIdentity {
        genesis: H256(TESTNET_GENESIS.0),
        netuid: NetUid(NETUID),
        hotkey: AccountId(validator2().hotkey.0),
    }
}

/// The real journal behind `LivePorts`, with a fixed finalized registration
/// snapshot standing in for the chain.
struct TestApp {
    live: LivePorts,
    bundle: Vec<u8>,
    snapshot: RegistrationSnapshot,
    /// Own hotkey and owner hotkey, as `hold_hotkeys` reads them.
    hold: Vec<[u8; 32]>,
    epoch: u64,
    dry_runs: usize,
    dispatches: usize,
    /// The durable held-target record, kept in memory here.
    held: Option<crate::package_v2::HeldTargetFile>,
    /// Chain served (genesis, netuid).
    served: ([u8; 32], u16),
    /// Trusted application keys, when not the dev keys.
    keys: Option<TrustedKeys>,
}

impl TestApp {
    fn open(config: &ValidatorConfig, bundle: &Bundle) -> Result<Self> {
        let mut live = LivePorts::new(config.clone(), identity());
        crate::startup::StartupPorts::open_journal(&mut live)?;
        live.unpause_for_offline_tests()?;
        Ok(Self {
            live,
            bundle: bundle.to_json().into_bytes(),
            snapshot: registered(),
            hold: vec![validator2().hotkey.0, owner().hotkey.0],
            epoch: 0,
            dry_runs: 0,
            dispatches: 0,
            held: None,
            served: (TESTNET_GENESIS.0, NETUID),
            keys: None,
        })
    }

    fn summary(&self) -> Result<slop_ninja_subnet::JournalSummary> {
        self.live
            .controls_for_tests()
            .context("journal is not open")?
            .inspect()
    }
}

impl AppPorts for TestApp {
    fn read_bundle(&mut self) -> Result<Vec<u8>> {
        Ok(self.bundle.clone())
    }
    fn trusted_keys(&self) -> Result<TrustedKeys> {
        Ok(self.keys.clone().unwrap_or_else(trusted))
    }
    fn chain(&self) -> ([u8; 32], u16) {
        self.served
    }
    async fn registration_snapshot(
        &mut self,
        _: &FinalizedHead,
        _: &ChainBinding,
    ) -> Result<RegistrationSnapshot> {
        Ok(self.snapshot.clone())
    }
    async fn hold_hotkeys(&mut self, _: &FinalizedHead) -> Result<Vec<[u8; 32]>> {
        Ok(self.hold.clone())
    }
    async fn epoch_index(&mut self, _: &FinalizedHead) -> Result<u64> {
        Ok(self.epoch)
    }
    fn operation_id(&self, work: &WeightWork) -> Result<String> {
        self.live.operation_id(work)
    }
    fn retained(&self, operation_id: &str) -> Result<bool> {
        self.live.retained(operation_id)
    }
    fn enqueue(&mut self, work: &WeightWork, input: BTreeMap<Uid, f64>) -> Result<Enqueued> {
        self.live.enqueue(work, input)
    }
    fn authorize(&mut self, operation_id: &str, input: &BTreeMap<Uid, f64>) -> Result<()> {
        self.live.authorize(operation_id, input)
    }
    fn held_target(&self) -> Result<Option<crate::package_v2::HeldTargetFile>> {
        Ok(self.held.clone())
    }
    fn record_target(&mut self, held: &crate::package_v2::HeldTargetFile) -> Result<()> {
        self.held = Some(held.clone());
        Ok(())
    }
    fn pending_operations(&self, mechanism: u8) -> Result<Vec<slop_ninja_subnet::PendingOperation>> {
        self.live.pending_operations(mechanism)
    }
    fn retire(&mut self, operation_id: &str, reason: &str) -> Result<bool> {
        self.live.retire(operation_id, reason)
    }
    fn retained_row(&self, operation_id: &str) -> Result<Option<(String, bool)>> {
        self.live.retained_row(operation_id)
    }
    async fn dry_run(&mut self, operation_id: &str, derived: &Derived) -> Result<DryRunReport> {
        self.dry_runs += 1;
        Ok(DryRunReport {
            operation_id: operation_id.into(),
            row_digest: derived.row_digest(),
            entries: derived.entries(),
            signing_uid: 1,
            reveal_round: 0,
            commit_bytes: 0,
            call_pallet_index: 7,
            call_index: 118,
            call_sha256: String::new(),
            signing_payload_bytes: 0,
            signing_payload_sha256: String::new(),
            nonce: 0,
            era_expiry: None,
            ciphertext_randomized: true,
            observed_spec_version: 0,
            signed: false,
        })
    }
    /// Offline stand-in: no signer, client or chain. It only counts calls, so
    /// the enqueue side of `--dispatch` runs against the real journal.
    async fn dispatch(&mut self, _: &str, _: &Derived, _: &Faults) -> Result<DispatchReport> {
        self.dispatches += 1;
        Ok(DispatchReport {
            stop: "offline_stand_in".into(),
            ..DispatchReport::default()
        })
    }
}

fn head() -> FinalizedHead {
    FinalizedHead {
        number: 8_340_500,
        hash: H256([0x44; 32]),
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

fn enqueued(outcome: &AppOutcome) -> (String, bool) {
    match outcome {
        AppOutcome::Enqueued {
            operation_id,
            created,
            ..
        } => (operation_id.clone(), *created),
        other => panic!("expected enqueued work, got {other:?}"),
    }
}

#[test]
fn dry_run_verifies_derives_and_never_signs_or_writes_the_journal() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    let mut app = TestApp::open(&config, &bundle())?;
    let revision = app.summary()?.generation;
    let outcome = block_on(apply(&mut app, &head(), Mode::DryRun, &Faults::none()))?;
    assert_eq!(app.dry_runs, 1);
    let AppOutcome::DryRun {
        operation_id,
        retained,
        entries,
        dry_run: report,
        ..
    } = &outcome
    else {
        panic!("dry run report expected, got {outcome:?}")
    };
    assert_eq!(entries, &vec![(2, 65535)]);
    assert!(!report.signed && !retained);
    // Nothing was enqueued or authorized: the journal is unchanged.
    let summary = app.summary()?;
    assert_eq!((summary.operations, summary.attempts), (0, 0));
    assert_eq!(summary.generation, revision);
    let expected = WeightWork {
        manifest_hash: verify(&bundle()).unwrap().target.cohort_manifest.0,
        cohort: "testnet-t12-cohort-1".into(),
        revision: 1,
        mechanism: rtao_primitives::MechId(0),
        tempo: None,
        v2_target: None,
    }
    .operation_id(&identity())?;
    assert_eq!(operation_id, &expected);
    // After dispatch enqueued it, a dry run reports the retained operation.
    block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?;
    let outcome = block_on(apply(&mut app, &head(), Mode::DryRun, &Faults::none()))?;
    assert!(matches!(outcome, AppOutcome::DryRun { retained: true, .. }));
    assert_eq!(app.summary()?.operations, 1);
    Ok(())
}

#[test]
fn a_mistaken_package_checked_by_dry_run_never_blocks_the_corrected_one() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    {
        let mut app = TestApp::open(&config, &bundle())?;
        block_on(apply(&mut app, &head(), Mode::DryRun, &Faults::none()))?;
    }
    let mut corrected = params();
    corrected.revision = 2;
    let mut app = TestApp::open(&config, &bundle_for(&corrected))?;
    let outcome = block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?;
    assert!(enqueued(&outcome).1);
    assert_eq!(app.summary()?.operations, 1);
    Ok(())
}

#[test]
fn a_row_rtao_would_hold_is_refused_before_the_journal() -> Result<()> {
    // The owner hotkey sits at the miner's UID, as a mistyped registration
    // that maps the credit onto UID 0 would: rtao would hold the row forever.
    for mode in [Mode::DryRun, Mode::Dispatch] {
        let root = scratch()?;
        let config = config(root.path())?;
        init_journal(&config, identity())?;
        let mut app = TestApp::open(&config, &bundle())?;
        app.hold = vec![validator2().hotkey.0, miner1().hotkey.0];
        let outcome = block_on(apply(&mut app, &head(), mode, &Faults::none()))?;
        assert!(
            matches!(
                outcome,
                AppOutcome::Refused {
                    code: "row_would_be_held",
                    ..
                }
            ),
            "{mode:?}: {outcome:?}"
        );
        assert_eq!(app.summary()?.operations, 0);
        assert_eq!((app.dry_runs, app.dispatches), (0, 0));
    }
    let snapshot = registered();
    let hold = [validator2().hotkey.0, owner().hotkey.0];
    assert!(crate::app::row_would_be_held(
        &[(0, 65535)],
        &snapshot,
        &hold
    ));
    assert!(crate::app::row_would_be_held(
        &[(1, 65535), (2, 0)],
        &snapshot,
        &hold
    ));
    assert!(!crate::app::row_would_be_held(
        &[(0, 9), (2, 65535)],
        &snapshot,
        &hold
    ));
    // A UID outside the snapshot is not held here; prepare_commit refuses it.
    assert!(!crate::app::row_would_be_held(
        &[(7, 65535)],
        &snapshot,
        &hold
    ));
    Ok(())
}

#[test]
fn a_crash_at_any_application_boundary_never_duplicates_the_operation() -> Result<()> {
    for phase in [Phase::PackageVerified, Phase::RowDerived, Phase::Enqueued] {
        let root = scratch()?;
        let config = config(root.path())?;
        init_journal(&config, identity())?;
        {
            let mut app = TestApp::open(&config, &bundle())?;
            let error = block_on(apply(
                &mut app,
                &head(),
                Mode::Dispatch,
                &Faults::at(phase, 1),
            ))
            .unwrap_err();
            assert!(error.downcast_ref::<InjectedFault>().is_some(), "{phase:?}");
            assert_eq!(app.dispatches, 0);
            let expected_ops = usize::from(phase == Phase::Enqueued);
            assert_eq!(app.summary()?.operations, expected_ops, "{phase:?}");
        }
        // Restart: open persists pause; the same package yields the same work.
        let mut ids = Vec::new();
        for restart in 0..3 {
            let mut app = TestApp::open(&config, &bundle())?;
            let outcome = block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?;
            let (id, created) = enqueued(&outcome);
            assert_eq!(
                created,
                restart == 0 && phase != Phase::Enqueued,
                "{phase:?} restart {restart}"
            );
            ids.push(id);
            let summary = app.summary()?;
            assert_eq!((summary.operations, summary.attempts), (1, 0), "{phase:?}");
        }
        assert!(ids.windows(2).all(|pair| pair[0] == pair[1]));
    }
    Ok(())
}

#[test]
fn a_successor_package_cannot_queue_a_second_commit_while_one_is_pending() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    {
        let mut app = TestApp::open(&config, &bundle())?;
        block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?;
    }
    let mut successor = params();
    successor.revision = 2;
    let mut app = TestApp::open(&config, &bundle_for(&successor))?;
    let error = block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none())).unwrap_err();
    assert_eq!(error.to_string(), "enqueue refused");
    assert_eq!(app.summary()?.operations, 1);
    // A dry run of the successor still works and writes nothing.
    let outcome = block_on(apply(&mut app, &head(), Mode::DryRun, &Faults::none()))?;
    assert!(matches!(
        outcome,
        AppOutcome::DryRun {
            retained: false,
            ..
        }
    ));
    assert_eq!(app.summary()?.operations, 1);
    Ok(())
}

#[test]
fn a_refused_package_enqueues_nothing() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    let mut bad = bundle();
    bad.target = flip(&bad.target);
    let mut app = TestApp::open(&config, &bad)?;
    for mode in [Mode::DryRun, Mode::Dispatch] {
        let outcome = block_on(apply(&mut app, &head(), mode, &Faults::none()))?;
        assert!(matches!(
            outcome,
            AppOutcome::Refused {
                code: "target_unverified",
                ..
            }
        ));
    }
    assert_eq!(app.summary()?.operations, 0);
    assert_eq!((app.dry_runs, app.dispatches), (0, 0));
    Ok(())
}

/// Append a signed attempt persisted before network I/O with no response
/// recorded: the state a timeout or 429 after send, or a crash mid-submission,
/// leaves. It satisfies rtao's attempt-graph invariants.
fn push_lost_response(record: &mut rtao_client::journal::OperationRecord) {
    use blake2::{Blake2b, Digest as _, digest::consts::U32};
    let bytes = vec![1u8, 2, 3];
    let mut attempt: rtao_client::journal::SignedAttempt =
        serde_json::from_value(serde_json::json!({
            "id": record.attempts.len(), "role": "Ordinary", "material_generation": 0,
            "hash": H256([0x5a; 32]), "bytes": [1, 2, 3], "nonce": 7,
            "spec_version": 486, "transaction_version": 1, "start": 8_340_501,
            "expiry": 8_340_629, "era_anchor": H256([0x5b; 32]), "era": null,
            "metadata_hash": null, "material": null, "next": 8_340_501,
            "finalized": null, "outcome": "Unresolved", "evidence_conflicts": [],
        }))
        .unwrap();
    attempt.hash = H256(Blake2b::<U32>::digest(&bytes).into());
    attempt.bytes = bytes;
    attempt.material = record.material.clone();
    attempt.material_generation = Some(record.material_generation);
    record.mode = rtao_client::journal::ExecutionMode::Ordinary;
    record.attempts.push(attempt);
}

#[test]
fn a_lost_response_after_send_survives_restart_without_a_second_commit() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    let id = {
        let mut app = TestApp::open(&config, &bundle())?;
        let outcome = block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?;
        enqueued(&outcome).0
    };
    // rtao persisted the signed bytes, then the response was lost.
    OperationJournal::open(&config.journal_path)?.update(|image| {
        push_lost_response(image.operations.get_mut(&id).expect("operation retained"));
        Ok(())
    })?;
    for _restart in 0..3 {
        let mut app = TestApp::open(&config, &bundle())?;
        let outcome = block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?;
        assert_eq!(enqueued(&outcome), (id.clone(), false));
        let summary = app.summary()?;
        assert_eq!(
            (
                summary.operations,
                summary.attempts,
                summary.unresolved_attempts
            ),
            (1, 1, 1)
        );
        // Only finalized recovery may classify the retained bytes; the
        // adapter's resume gate, and therefore dispatch, stays closed.
        assert!(app.live.resume_for_tests().is_err());
    }
    let image = OperationJournal::open(&config.journal_path)?.snapshot()?;
    assert_eq!(image.operations[&id].attempts.len(), 1);
    Ok(())
}

#[test]
fn package_and_dispatch_configuration_is_explicit_and_checked() -> Result<()> {
    let root = scratch()?;
    let base = include_str!("../validator.example.toml")
        .replace("netuid = 0", &format!("netuid = {NETUID}"))
        .replace(
            "/home/operator/.local/state/slop_ninja_validator/weights-journal.json",
            &root.path().join("journal.json").display().to_string(),
        );
    let keys = trusted();
    let section = |path: &str, hotkey: &str, package: &[u8; 32]| {
        format!(
            "\n[package]\npath = \"{path}\"\n[package.trusted_keys]\npackage = \"{}\"\nreceipt = \"{}\"\nauthorization = \"{}\"\n[dispatch]\nhotkey_file = \"{hotkey}\"\nepoch_offset_blocks = 20\n",
            hex::encode(package),
            hex::encode(keys.key(slop_ninja_protocol::KeyRole::Receipt)),
            hex::encode(keys.key(slop_ninja_protocol::KeyRole::Authorization)),
        )
    };
    let package = *keys.key(slop_ninja_protocol::KeyRole::Package);
    let good = format!(
        "{base}{}",
        section("/abs/package.json", "/abs/hotkeys/v2", &package)
    );
    let config = ValidatorConfig::parse(&good)?;
    assert!(config.summary().package_configured && config.summary().dispatch_configured);
    assert_eq!(config.package.unwrap().trusted_keys.keys()?, keys);
    assert_eq!(config.dispatch.unwrap().epoch_offset_blocks, Some(20));
    for bad in [
        section("relative.json", "/abs/hotkeys/v2", &package),
        section("/abs/package.json", "hotkeys/v2", &package),
        // A package key equal to the receipt key is refused.
        section(
            "/abs/package.json",
            "/abs/hotkeys/v2",
            keys.key(slop_ninja_protocol::KeyRole::Receipt),
        ),
    ] {
        assert!(ValidatorConfig::parse(&format!("{base}{bad}")).is_err());
    }
    // The example itself has neither section, so it never dispatches.
    let plain = ValidatorConfig::parse(&base)?;
    assert!(plain.package.is_none() && plain.dispatch.is_none());
    Ok(())
}

/// A reward-v1 launch bundle: the burn row under a launch authorization.
fn launch_bundle(issued_epoch: u64) -> Vec<u8> {
    let auth = slop_ninja_protocol::LaunchAuthorization::new(&chain(), 1, issued_epoch).unwrap();
    let signed = slop_ninja_protocol::sign(auth, &authorization_key()).unwrap().to_bytes().unwrap();
    serde_json::to_vec(&crate::package_v2::BundleV2 {
        schema: crate::package_v2::BUNDLE_SCHEMA_V2.into(),
        shortcut: "T12".into(),
        source: None,
        authorization: crate::package_v2::AuthorizationHex {
            kind: "launch".into(),
            authorization: hex::encode(signed),
            renewal: None,
            carrier: None,
        },
        receipt_chunks: Vec::new(),
        carrier_receipt_chunks: Vec::new(),
    })
    .unwrap()
}

#[test]
fn a_reward_v1_launch_bundle_derives_the_burn_row_and_rtao_would_hold_it() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    let mut app = TestApp::open(&config, &bundle())?;
    app.bundle = launch_bundle(10);
    app.epoch = 12;
    // The burn row: every unit at the owner hotkey's UID. rtao at this pin
    // holds a row whose positive entries are only the owner's (or this
    // validator's) UID, so it is refused before the journal.
    // That is a per-tempo deferral (exit 75), not a halt.
    for mode in [Mode::DryRun, Mode::Dispatch] {
        let outcome = block_on(apply(&mut app, &head(), mode, &Faults::none()))?;
        match &outcome {
            AppOutcome::Deferred { code, .. } => assert_eq!(*code, "row_would_be_held"),
            other => panic!("expected the held-row deferral, got {other:?}"),
        }
    }
    assert_eq!(app.summary()?.operations, 0);
    // A lease that does not cover the epoch defers too.
    app.epoch = 60;
    match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
        AppOutcome::Deferred { code, .. } => assert_eq!(code, "authorization_refused"),
        other => panic!("expected the lease deferral, got {other:?}"),
    }
    // The derivation itself, and the authorization's epoch checks.
    let launch_held = slop_ninja_protocol::ActiveTarget::launch(&chain()).unwrap();
    let a = std::sync::Arc::new(
        crate::package_v2::verify(&launch_bundle(10), &trusted(), &TESTNET_GENESIS.0, NETUID, &launch_held).unwrap(),
    );
    let snap = crate::package_v2::snapshot(&registered(), owner().hotkey.0).unwrap();
    let d = crate::package_v2::derive(a.clone(), &snap, validator2().hotkey.0, 12).unwrap();
    assert_eq!(d.entries(), vec![(owner().uid, 65535)]);
    assert!(d.work.operation_id(&identity())?.starts_with(slop_ninja_protocol::WEIGHT_OPERATION_V2_PREFIX));
    assert_eq!(d.work_for_tempo(validator2().hotkey.0, 13).unwrap().tempo, Some(13));
    assert_eq!(d.work_for_tempo(validator2().hotkey.0, 51).unwrap_err().code, "authorization_refused");
    assert_eq!(crate::package_v2::derive(a, &snap, validator2().hotkey.0, 9).unwrap_err().code, "authorization_refused");
    // Under another key the bundle is refused before any row.
    let mut forged: serde_json::Value = serde_json::from_slice(&launch_bundle(10))?;
    let other = slop_ninja_protocol::sign(slop_ninja_protocol::LaunchAuthorization::new(&chain(), 1, 10).unwrap(), &package_key())
        .unwrap()
        .to_bytes()
        .unwrap();
    forged["authorization"]["authorization"] = serde_json::json!(hex::encode(other));
    assert_eq!(
        crate::package_v2::verify(&serde_json::to_vec(&forged)?, &trusted(), &TESTNET_GENESIS.0, NETUID, &launch_held)
            .unwrap_err()
            .code,
        "bundle_unverified"
    );
    Ok(())
}


// reward-v1 cohort bundles against the real journal: the held target, a
// target switch, a missed tempo, a re-derived row and rollbacks.

pub(crate) mod rv1 {
    use super::*;
    use slop_ninja_protocol::fixtures_reward_v1::{self as fx, Spec, bp};
    use slop_ninja_protocol::{
        AttemptReceipt, CalculationTargetV2, CohortManifestV2, Digest, OutcomeClass, PublishedIndex, ReceiptIndex,
        ReceiptIndexEntry, Record, ScorePackageV2, sign,
    };

    pub const RV1_NETUID: u16 = 4242;

    pub struct Cohort {
        pub manifest: CohortManifestV2,
        pub package: ScorePackageV2,
        pub target: CalculationTargetV2,
        pub closure: slop_ninja_protocol::submission::CohortClosure,
        pub index: PublishedIndex,
        pub receipt_index: ReceiptIndex,
        pub chunk: Vec<u8>,
        pub authorization: Vec<u8>,
    }

    /// Binds a fixture package to real receipts (one object per counted
    /// receipt, in one chunk) and rebuilds its target, closure and
    /// authorization for the new package digest.
    pub fn bound(
        manifest: CohortManifestV2,
        mut package: ScorePackageV2,
        index: PublishedIndex,
        predecessor: Digest,
        specs: &[Spec],
    ) -> Cohort {
        let mut receipts: Vec<AttemptReceipt> = (0..package.receipt_count)
            .map(|i| AttemptReceipt {
                attempt: slop_ninja_protocol::domain_hash("validator-test-attempt", &i.to_le_bytes()),
                assignment: Digest([0x10; 32]),
                artifact: Digest([0x11; 32]),
                number: 1,
                outcome: OutcomeClass::Prediction,
                result_commitment: Digest([0x12; 32]),
            })
            .collect();
        receipts.sort_by_key(|r| r.attempt);
        let receipt_index = ReceiptIndex {
            cohort_manifest: manifest.digest().unwrap(),
            entries: receipts.iter().map(|r| ReceiptIndexEntry::for_receipt(r).unwrap()).collect(),
        };
        package.receipt_root = receipt_index.digest().unwrap();
        package.receipt_count = receipt_index.count();
        let mut chunk = Vec::new();
        for r in &receipts {
            let bytes = r.to_bytes().unwrap();
            chunk.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            chunk.extend_from_slice(&bytes);
        }
        let target = CalculationTargetV2::for_package(&package, Some(predecessor)).unwrap();
        let closure = fx::closure(&manifest, &package, Some(&target), specs).unwrap();
        let authorization = fx::signed(
            fx::authorization(&manifest, &package, &target, 1, manifest.opened_epoch + fx::PUBLISHED_TEMPO).unwrap(),
        )
        .unwrap()
        .to_bytes()
        .unwrap();
        Cohort { manifest, package, target, closure, index, receipt_index, chunk, authorization }
    }

    fn signed_hex<R: Record + Clone>(r: &R, key: &SigningKey) -> String {
        hex::encode(sign(r.clone(), key).unwrap().to_bytes().unwrap())
    }

    pub fn bundle(c: &Cohort) -> Vec<u8> {
        let pk = slop_ninja_protocol::fixtures::package_key();
        let rk = slop_ninja_protocol::fixtures::receipt_key();
        serde_json::to_vec(&crate::package_v2::BundleV2 {
            schema: crate::package_v2::BUNDLE_SCHEMA_V2.into(),
            shortcut: "T12".into(),
            source: Some(crate::package_v2::SourceHex {
                manifest: signed_hex(&c.manifest, &pk),
                package: signed_hex(&c.package, &pk),
                target: signed_hex(&c.target, &pk),
                closure: signed_hex(&c.closure, &pk),
                receipt_index: Some(signed_hex(&c.receipt_index, &rk)),
                published_contents: hex::encode(c.index.contents.to_bytes().unwrap()),
                fingerprint_index: hex::encode(c.index.fingerprints.to_bytes().unwrap()),
            }),
            authorization: crate::package_v2::AuthorizationHex {
                kind: "cohort".into(),
                authorization: hex::encode(&c.authorization),
                renewal: None,
                carrier: None,
            },
            receipt_chunks: vec![hex::encode(&c.chunk)],
            carrier_receipt_chunks: Vec::new(),
        })
        .unwrap()
    }

    pub fn launch(issued_epoch: u64) -> Vec<u8> {
        let auth = slop_ninja_protocol::LaunchAuthorization::new(&fx::chain(), 1, issued_epoch).unwrap();
        let signed = fx::signed(auth).unwrap().to_bytes().unwrap();
        serde_json::to_vec(&crate::package_v2::BundleV2 {
            schema: crate::package_v2::BUNDLE_SCHEMA_V2.into(),
            shortcut: "T12".into(),
            source: None,
            authorization: crate::package_v2::AuthorizationHex {
                kind: "launch".into(),
                authorization: hex::encode(signed),
                renewal: None,
                carrier: None,
            },
            receipt_chunks: Vec::new(),
            carrier_receipt_chunks: Vec::new(),
        })
        .unwrap()
    }

    /// The computed cohort bound to `receipts` receipts in one chunk, for
    /// sizing a bundle against its bound.
    pub fn sized(receipts: u64) -> Cohort {
        let c = fx::computed().unwrap();
        let launch = CalculationTargetV2::launch_burn(fx::chain()).digest().unwrap();
        let mut package = c.package.clone();
        package.receipt_count = receipts;
        bound(c.manifest.clone(), package, c.index.clone(), launch, &c.specs)
    }

    /// The computed cohort (target T1, activating at epoch 80) and its
    /// successor (T2, activating at epoch 100), bound to receipts.
    pub fn cohorts() -> (Cohort, Cohort, Vec<Spec>) {
        let c = fx::computed().unwrap();
        let launch = CalculationTargetV2::launch_burn(fx::chain()).digest().unwrap();
        let first = bound(c.manifest.clone(), c.package.clone(), c.index.clone(), launch, &c.specs);
        let s = fx::successor_of(&c).unwrap();
        let s_specs = vec![Spec::new(0x92, bp(9900))];
        let s_index = fx::default_index(&s.manifest.references);
        let second = bound(s.manifest, s.package, s_index, first.target.digest().unwrap(), &s_specs);
        let mut specs = c.specs.clone();
        specs.extend(s_specs);
        (first, second, specs)
    }

    /// Every fixture entrant registered under its own generation, plus the
    /// owner hotkey at UID 2, as a v1 snapshot.
    pub fn snapshot(specs: &[Spec]) -> RegistrationSnapshot {
        let v2 = fx::snapshot(fx::chain(), 2, specs);
        package::snapshot(&fx::chain(), v2.block, v2.block_hash.0, v2.neurons).unwrap()
    }
}

fn rv1_app(root: &Path) -> Result<TestApp> {
    let mut config = config(root)?;
    config.netuid = rv1::RV1_NETUID;
    let identity = ChainIdentity { netuid: NetUid(rv1::RV1_NETUID), ..identity() };
    init_journal(&config, identity.clone())?;
    let mut live = LivePorts::new(config.clone(), identity);
    crate::startup::StartupPorts::open_journal(&mut live)?;
    live.unpause_for_offline_tests()?;
    Ok(TestApp {
        live,
        bundle: Vec::new(),
        snapshot: registered(),
        hold: vec![validator2().hotkey.0, owner().hotkey.0],
        epoch: 0,
        dry_runs: 0,
        dispatches: 0,
        held: None,
        served: (TESTNET_GENESIS.0, rv1::RV1_NETUID),
        keys: Some(slop_ninja_protocol::fixtures::trusted_keys()),
    })
}

fn retired_reason(app: &TestApp, id: &str) -> Option<String> {
    app.live.controls_for_tests().unwrap().retired(id).unwrap()
}

/// Two successive reward-v1 targets, a missed tempo, a re-derived row and a
/// restart: retained work that never landed is retired (journaled), never
/// left to wedge the queue, and the held target refuses rollbacks.
#[test]
fn reward_v1_targets_switch_through_retained_work_and_refuse_rollbacks() -> Result<()> {
    let root = scratch()?;
    let (first, second, specs) = rv1::cohorts();
    let mut app = rv1_app(root.path())?;
    app.snapshot = rv1::snapshot(&specs);
    // T1 at its activation epoch.
    app.bundle = rv1::bundle(&first);
    app.epoch = 80;
    let (op80, created) = enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?);
    assert!(created);
    let held = app.held.clone().expect("held target recorded");
    assert_eq!(held.activated_epoch, Some(80));
    // A missed tempo: the epoch-80 operation never signed; at epoch 81 it is
    // retired as superseded and the epoch-81 operation is queued.
    app.epoch = 81;
    let (op81, created) = enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?);
    assert!(created && op81 != op80);
    assert_eq!(retired_reason(&app, &op80).as_deref(), Some("superseded"));
    // A restart in the same epoch after a registration change: a credited
    // entrant re-registered (a new generation), so its units burn. The
    // retained epoch-81 operation's row is immutable, so it is retired and
    // the re-derived row takes the next label.
    let credited = first.package.entrants.iter().find(|e| e.units > 0).expect("a credited entrant").identity.hotkey;
    let before = rv1::snapshot(&specs);
    let mut changed = before.clone();
    for n in &mut changed.neurons {
        if n.hotkey == credited {
            n.registration_block += 1;
        }
    }
    app.snapshot = changed;
    let outcome = block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?;
    let (op82, created) = enqueued(&outcome);
    assert!(created && op82 != op81);
    assert_eq!(retired_reason(&app, &op81).as_deref(), Some("row_rederived"));
    let before_entries = match block_on(apply(&mut app, &head(), Mode::DryRun, &Faults::none()))? {
        AppOutcome::DryRun { entries, .. } => entries,
        other => panic!("dry run expected, got {other:?}"),
    };
    app.snapshot = before.clone();
    let after_entries = match block_on(apply(&mut app, &head(), Mode::DryRun, &Faults::none()))? {
        AppOutcome::DryRun { entries, .. } => entries,
        other => panic!("dry run expected, got {other:?}"),
    };
    assert_ne!(before_entries, after_entries, "the registration change moved the row");
    // The successor target at its activation: the T1 work is retired and the
    // held target moves to T2.
    app.bundle = rv1::bundle(&second);
    app.epoch = 100;
    let (op100, created) = enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?);
    assert!(created);
    assert_eq!(retired_reason(&app, &op82).as_deref(), Some("superseded"));
    assert_eq!(app.held.as_ref().and_then(|h| h.activated_epoch), Some(100));
    assert_eq!(app.live.pending_operations(0)?.iter().map(|p| p.id.clone()).collect::<Vec<_>>(), vec![op100.clone()]);
    // Rollbacks are refused before the journal: the replaced target, and the
    // launch authorization after the first activation.
    let ops = app.summary()?.operations;
    app.epoch = 101;
    for (bundle, what) in [(rv1::bundle(&first), "replaced target"), (rv1::launch(95), "launch bundle")] {
        app.bundle = bundle;
        match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
            AppOutcome::Refused { code, .. } => assert_eq!(code, "bundle_unverified", "{what}"),
            other => panic!("{what}: expected a refusal, got {other:?}"),
        }
    }
    assert_eq!(app.summary()?.operations, ops);
    // A restart on the held target joins its retained operation.
    app.bundle = rv1::bundle(&second);
    app.epoch = 100;
    let (again, created) = enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?);
    assert_eq!((again, created), (op100, false));
    Ok(())
}

/// A multi-UID reward-v1 row sums to 65,535, but it is committed, and read
/// back after its reveal, in the max-upscaled form that rtao's preparation
/// and the chain's reveal both produce; that form is what reaches rtao and
/// survives its conformance unchanged.
#[test]
fn a_multi_uid_reward_v1_row_is_committed_in_its_max_upscaled_form() -> Result<()> {
    let root = scratch()?;
    let (first, _, specs) = rv1::cohorts();
    let mut app = rv1_app(root.path())?;
    app.snapshot = rv1::snapshot(&specs);
    app.bundle = rv1::bundle(&first);
    app.epoch = 80;
    let held = slop_ninja_protocol::ActiveTarget::launch(&slop_ninja_protocol::fixtures_reward_v1::chain())?;
    let authorized = std::sync::Arc::new(
        crate::package_v2::verify(
            &app.bundle,
            &slop_ninja_protocol::fixtures::trusted_keys(),
            &TESTNET_GENESIS.0,
            rv1::RV1_NETUID,
            &held,
        )
        .unwrap(),
    );
    let snap = crate::package_v2::snapshot(&app.snapshot, owner().hotkey.0).unwrap();
    let d = crate::package_v2::derive(authorized, &snap, validator2().hotkey.0, 80).unwrap();
    assert!(d.entries().len() >= 2, "the computed row pays several UIDs: {:?}", d.entries());
    assert_eq!(d.entries().iter().map(|e| u32::from(e.1)).sum::<u32>(), 65535);
    let top = d.chain_entries().iter().map(|e| e.1).max();
    assert_eq!(top, Some(65535));
    assert_ne!(d.chain_entries(), d.entries());
    // rtao's conformance (the step prepare_commit runs) keeps the committed form.
    let (uids, weights): (Vec<Uid>, Vec<f64>) = d.input.iter().map(|(u, w)| (*u, *w)).unzip();
    let conformed = rtao_intents::weights::conform(
        &uids,
        &weights,
        rtao_intents::weights::ConformState { uid: Uid(1), min_allowed_weights: 0, max_weight_limit: u16::MAX },
        NetUid(rv1::RV1_NETUID),
        false,
    )
    .unwrap();
    let expected: (Vec<Uid>, Vec<u16>) = d.chain_entries().iter().map(|(u, w)| (Uid(*u), *w)).unzip();
    assert_eq!(conformed, expected);
    // The canonical form would not survive it: rtao would upscale it.
    let (cu, cw): (Vec<Uid>, Vec<f64>) = d.entries().iter().map(|(u, w)| (Uid(*u), f64::from(*w))).unzip();
    let canonical = rtao_intents::weights::conform(
        &cu,
        &cw,
        rtao_intents::weights::ConformState { uid: Uid(1), min_allowed_weights: 0, max_weight_limit: u16::MAX },
        NetUid(rv1::RV1_NETUID),
        false,
    )
    .unwrap();
    assert_ne!(canonical.1, d.entries().iter().map(|e| e.1).collect::<Vec<_>>());
    assert_eq!(canonical, expected);
    // The dry run reports the canonical entries, which the coordinator's row compares.
    match block_on(apply(&mut app, &head(), Mode::DryRun, &Faults::none()))? {
        AppOutcome::DryRun { entries, .. } => assert_eq!(entries, d.entries()),
        other => panic!("expected a dry run, got {other:?}"),
    }
    Ok(())
}
