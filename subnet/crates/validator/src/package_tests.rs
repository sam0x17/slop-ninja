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
use slop_ninja_subnet::{ChainIdentity, Enqueued, OwnerRowPolicy, WeightWork};

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

/// A revocation withdraws an operation's grant: every later attempt is
/// refused, until a newer authorization grants the operation again.
#[test]
fn guard_refuses_attempts_of_a_withdrawn_operation() {
    let authorized = Authorized::default();
    authorized.insert("op", "h");
    assert!(check_intent(&authorized, &intent("op", "h", PACKAGE_CONTRACT)).is_ok());
    assert!(authorized.withdraw("op"));
    assert_eq!(
        check_intent(&authorized, &intent("op", "h", PACKAGE_CONTRACT)).unwrap_err().code,
        "slop_ninja_authorization_revoked"
    );
    assert!(!authorized.withdraw("op"), "no grant left to withdraw");
    // Withdrawn without a grant (work recovered from an earlier process).
    assert!(!authorized.withdraw("other"));
    assert_eq!(
        check_intent(&authorized, &intent("other", "h", PACKAGE_CONTRACT)).unwrap_err().code,
        "slop_ninja_authorization_revoked"
    );
    authorized.insert("op", "h");
    assert!(check_intent(&authorized, &intent("op", "h", PACKAGE_CONTRACT)).is_ok());
}

/// The revocation store keeps each checked revocation once, across reloads,
/// and refuses a forged one, another chain's, and an altered file.
#[test]
fn the_revocation_store_journals_checked_revocations_once() -> Result<()> {
    let root = scratch()?;
    let store = root.path().join("journal.json.revocations.json");
    let launch = slop_ninja_protocol::sign(
        slop_ninja_protocol::LaunchAuthorization::new(&chain(), 1, 10).unwrap(),
        &authorization_key(),
    )
    .unwrap();
    let held = slop_ninja_protocol::ActiveTarget::launch(&chain()).unwrap();
    let checked = slop_ninja_protocol::LaunchAuthorization::check(&launch, &trusted(), &chain(), &held).unwrap();
    let revocation = |epoch: u64, key: &SigningKey| {
        slop_ninja_protocol::sign(slop_ninja_protocol::RewardIntentRevocation::new(&checked, 1, epoch).unwrap(), key)
            .unwrap()
            .to_bytes()
            .unwrap()
    };
    assert!(crate::revocations::load(&store, &trusted(), &chain())?.is_empty());
    let first = revocation(20, &authorization_key());
    assert!(crate::revocations::record(&store, &trusted(), &chain(), &first)?.new);
    assert!(!crate::revocations::record(&store, &trusted(), &chain(), &first)?.new);
    assert!(crate::revocations::record(&store, &trusted(), &chain(), &revocation(30, &authorization_key()))?.new);
    assert_eq!(crate::revocations::load(&store, &trusted(), &chain())?.len(), 2);
    assert_eq!(crate::revocations::entries(&store)?[0].issued_epoch, 20);
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(std::fs::metadata(&store)?.permissions().mode() & 0o777, 0o600);
    // Signed by another key, or for another chain: refused, nothing written.
    assert!(crate::revocations::record(&store, &trusted(), &chain(), &revocation(40, &package_key())).is_err());
    let other = ChainBinding { netuid: NETUID + 1, ..chain() };
    assert!(crate::revocations::record(&store, &trusted(), &other, &first).is_err());
    assert_eq!(crate::revocations::load(&store, &trusted(), &chain())?.len(), 2);
    // An altered entry fails the whole load: the operator must look.
    let text = std::fs::read_to_string(&store)?;
    let entry = &crate::revocations::entries(&store)?[0].signed;
    let mut flipped = entry.clone();
    let last = flipped.pop().unwrap();
    flipped.push(if last == '0' { '1' } else { '0' });
    std::fs::write(&store, text.replace(entry.as_str(), &flipped))?;
    assert!(crate::revocations::load(&store, &trusted(), &chain()).is_err());
    Ok(())
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
    // The owner-row policy journaled with the work must be the one this
    // process chose: a Submit intent authorized as Hold is refused, and so
    // is a Hold intent authorized as Submit.
    let submit = |row: serde_json::Value| {
        let mut intent = intent("op", "h", PACKAGE_CONTRACT);
        intent.effects = serde_json::json!({"input": {"ByUid": row}, "owner_rows": "submit"});
        intent
    };
    let burn = submit(serde_json::json!({"0": 65535.0}));
    assert_eq!(refusal(check_intent(&authorized, &burn)), "slop_ninja_owner_rows_mismatch");
    let choice = crate::app::OwnerRowChoice::Submit { owner_uid: 0, owner_validator: false };
    authorized.grant("op", "h", choice);
    assert_eq!(
        refusal(check_intent(&authorized, &intent("op", "h", PACKAGE_CONTRACT))),
        "slop_ninja_owner_rows_mismatch"
    );
    assert!(check_intent(&authorized, &burn).is_ok());
    // A zero entry elsewhere is not a payment.
    assert!(check_intent(&authorized, &submit(serde_json::json!({"0": 65535.0, "2": 0.0}))).is_ok());
    // A Submit row that pays any other UID is refused, whatever chose it.
    for row in [serde_json::json!({"0": 65535.0, "2": 1.0}), serde_json::json!({"2": 65535.0}), serde_json::json!({"0": 0.0})] {
        assert_eq!(refusal(check_intent(&authorized, &submit(row))), "slop_ninja_owner_rows_unexpected");
    }
    let mut unknown = burn.clone();
    unknown.effects["owner_rows"] = serde_json::json!("sometimes");
    assert_eq!(refusal(check_intent(&authorized, &unknown)), "slop_ninja_owner_rows_mismatch");
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
    // A task-owned root when SLOP_NINJA_TEST_TMPDIR is set, otherwise the
    // system temporary directory (TMPDIR). Each TempDir removes itself.
    let root = std::env::var_os("SLOP_NINJA_TEST_TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
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
    /// Weight limits `weight_limits` returns, and how often it was read.
    limits: crate::app::WeightLimits,
    limit_reads: usize,
    /// The journaled revocation store, when the test uses one.
    revocation_store: Option<std::path::PathBuf>,
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
            limits: crate::app::WeightLimits {
                min_allowed_weights: 1,
                max_weights_limit: u16::MAX,
                population: 3,
                own_permit: true,
                null_consensus: false,
            },
            limit_reads: 0,
            revocation_store: None,
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
    fn revocations(&self) -> Result<Vec<slop_ninja_protocol::Signed<slop_ninja_protocol::RewardIntentRevocation>>> {
        let Some(store) = &self.revocation_store else { return Ok(Vec::new()) };
        let chain = ChainBinding {
            genesis: slop_ninja_protocol::Digest(self.served.0),
            netuid: self.served.1,
            mechanism: 0,
        };
        crate::revocations::load(store, &AppPorts::trusted_keys(self)?, &chain)
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
    async fn weight_limits(&mut self, _: u8) -> Result<crate::app::WeightLimits> {
        self.limit_reads += 1;
        Ok(self.limits)
    }
    fn enqueue(
        &mut self,
        work: &WeightWork,
        input: BTreeMap<Uid, f64>,
        owner_rows: OwnerRowPolicy,
    ) -> Result<Enqueued> {
        self.live.enqueue(work, input, owner_rows)
    }
    fn authorize(
        &mut self,
        operation_id: &str,
        input: &BTreeMap<Uid, f64>,
        owner_rows: crate::app::OwnerRowChoice,
    ) -> Result<()> {
        self.live.authorize(operation_id, input, owner_rows)
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
    fn retained_policy(&self, operation_id: &str) -> Result<Option<OwnerRowPolicy>> {
        self.live.retained_policy(operation_id)
    }
    fn retired(&self, operation_id: &str) -> Result<Option<String>> {
        self.live.retired(operation_id)
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
            commit_reveal_version: 4,
            weight_version: 0,
            required_version_key: 0,
            validator_permit: true,
            null_consensus: false,
            max_allowed_validators: None,
            stake_threshold: None,
            own_alpha_stake: None,
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
    // rtao's predicate, decided as its scheduler decides: this validator's
    // own UID is the owner's only when its hotkey is the owner hotkey.
    use crate::app::row_held;
    use OwnerRowPolicy::{Hold, Submit};
    let snapshot = registered();
    let (own, owner_hotkey) = (validator2().hotkey.0, owner().hotkey.0);
    let held = |policy, row: &[(u16, u16)]| row_held(policy, row, &snapshot, own, owner_hotkey);
    assert!(held(Hold, &[(0, 65535)]));
    assert!(held(Hold, &[(1, 65535), (2, 0)]));
    assert!(!held(Hold, &[(0, 9), (2, 65535)]));
    // A UID outside the snapshot is not held here; prepare_commit refuses it.
    assert!(!held(Hold, &[(7, 65535)]));
    // Under Submit an owner-only burn row is submitted; self-only,
    // self-plus-owner and all-zero rows keep holding.
    assert!(!held(Submit, &[(0, 65535)]));
    assert!(!held(Submit, &[(0, 65535), (2, 0)]));
    assert!(held(Submit, &[(1, 65535)]));
    assert!(held(Submit, &[(0, 1), (1, 1)]));
    assert!(held(Submit, &[(0, 0)]));
    assert!(held(Submit, &[]));
    assert!(!held(Submit, &[(1, 1), (2, 1)]));
    // An owner validator: its own UID is the owner's UID.
    assert!(row_held(Hold, &[(0, 65535)], &snapshot, owner_hotkey, owner_hotkey));
    assert!(!row_held(Submit, &[(0, 65535)], &snapshot, owner_hotkey, owner_hotkey));
    // A hotkey without a UID in the snapshot has no own entry.
    assert!(!row_held(Submit, &[(0, 65535)], &snapshot, [0x99; 32], owner_hotkey));
    Ok(())
}

#[test]
fn burn_rows_meet_the_chain_limits_before_they_are_queued() {
    use crate::app::{WeightLimits, check_burn_row};
    let snapshot = registered();
    let (own, owner_hotkey) = (validator2().hotkey.0, owner().hotkey.0);
    let limits = |min_allowed_weights, max_weights_limit, population| WeightLimits {
        min_allowed_weights,
        max_weights_limit,
        population,
        own_permit: true,
        null_consensus: false,
    };
    let burn = [(0, 65535)];
    assert_eq!(check_burn_row(&burn, &snapshot, own, owner_hotkey, &limits(1, 65535, 3)), Ok(()));
    // The effective minimum is capped at the population.
    assert_eq!(check_burn_row(&burn, &snapshot, own, owner_hotkey, &limits(8, 65535, 1)), Ok(()));
    for refused in [limits(2, 65535, 3), limits(8, 65535, 3)] {
        assert_eq!(
            check_burn_row(&burn, &snapshot, own, owner_hotkey, &refused).unwrap_err().code,
            "burn_row_min_allowed_weights"
        );
    }
    // An owner validator's burn row is a lone self-weight, which conform exempts.
    assert_eq!(check_burn_row(&burn, &snapshot, owner_hotkey, owner_hotkey, &limits(8, 65535, 3)), Ok(()));
    // Under a MaxWeightsLimit below 65535 only the owner UID may be listed.
    assert_eq!(check_burn_row(&burn, &snapshot, own, owner_hotkey, &limits(1, 4096, 3)), Ok(()));
    assert_eq!(
        check_burn_row(&[(0, 65535), (2, 0)], &snapshot, own, owner_hotkey, &limits(1, 4096, 3)).unwrap_err().code,
        "burn_row_weights_limit"
    );
    assert_eq!(check_burn_row(&[(0, 65535), (2, 0)], &snapshot, own, owner_hotkey, &limits(1, 65535, 3)), Ok(()));
    // The chain's validator-permit check, applied again at reveal: a
    // non-owner validator's burn row needs the permit under Yuma; the owner
    // validator is exempt under Yuma; under Null consensus nobody is.
    let no_permit = WeightLimits { own_permit: false, ..limits(1, 65535, 3) };
    assert_eq!(
        check_burn_row(&burn, &snapshot, own, owner_hotkey, &no_permit).unwrap_err().code,
        "burn_row_no_permit"
    );
    assert_eq!(check_burn_row(&burn, &snapshot, owner_hotkey, owner_hotkey, &no_permit), Ok(()));
    let null = WeightLimits { null_consensus: true, ..no_permit };
    for signer in [own, owner_hotkey] {
        assert_eq!(check_burn_row(&burn, &snapshot, signer, owner_hotkey, &null).unwrap_err().code, "burn_row_no_permit");
    }
    let null_permit = WeightLimits { own_permit: true, ..null };
    assert_eq!(check_burn_row(&burn, &snapshot, own, owner_hotkey, &null_permit), Ok(()));
    assert!(crate::app::is_row_deferral("burn_row_no_permit"));
    for code in ["burn_row_no_permit", "burn_row_min_allowed_weights", "burn_row_weights_limit", "row_would_be_held"] {
        assert!(crate::app::alerts_on_deferral(code), "{code}");
    }
    for code in ["row_deferred", "authorization_refused", "pending_operation_unsettled"] {
        assert!(!crate::app::alerts_on_deferral(code), "{code}");
    }
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
        receipts: None,
        carrier_receipts: None,
    })
    .unwrap()
}

#[test]
fn a_reward_v1_launch_bundle_queues_the_burn_row_under_submit() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    let mut app = TestApp::open(&config, &bundle())?;
    app.bundle = launch_bundle(10);
    app.epoch = 12;
    // The burn row: every unit at the owner hotkey's UID, under the launch
    // authorization. It is queued under OwnerRowPolicy::Submit, which rtao
    // journals as "owner_rows":"submit" and commits.
    match block_on(apply(&mut app, &head(), Mode::DryRun, &Faults::none()))? {
        AppOutcome::DryRun { entries, retained, .. } => {
            assert_eq!(entries, vec![(owner().uid, 65535)]);
            assert!(!retained);
        }
        other => panic!("expected a dry run, got {other:?}"),
    }
    assert_eq!(app.summary()?.operations, 0);
    let outcome = block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?;
    let (id, created) = enqueued(&outcome);
    assert!(created);
    assert_eq!(app.retained_policy(&id)?, Some(OwnerRowPolicy::Submit));
    // The journal file itself records the policy in the work's effects.
    let image: serde_json::Value = serde_json::from_slice(&std::fs::read(&config.journal_path)?)?;
    assert_eq!(image["operations"][&id]["intent"]["effects"]["owner_rows"], serde_json::json!("submit"));
    // The signing guard accepts the journaled intent under the choice the
    // application recorded (Submit, paying the owner UID), and would refuse
    // it under Hold.
    let intent: rtao_client::journal::OperationIntent =
        serde_json::from_value(image["operations"][&id]["intent"].clone())?;
    let authorized = app.live.authorized_for_tests();
    assert_eq!(
        authorized.get(&id).map(|grant| grant.owner_rows),
        Some(crate::app::OwnerRowChoice::Submit { owner_uid: owner().uid, owner_validator: false })
    );
    assert!(check_intent(authorized, &intent).is_ok());
    let hold = Authorized::default();
    hold.insert(&id, &authorized.row_hash(&id).context("authorized")?);
    assert_eq!(check_intent(&hold, &intent).unwrap_err().code, "slop_ninja_owner_rows_mismatch");
    // A retry joins the same operation under the same policy.
    let (again, created) = enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?);
    assert_eq!((again.as_str(), created), (id.as_str(), false));
    assert_eq!(app.summary()?.operations, 1);
    assert_eq!(app.limit_reads, 3);
    Ok(())
}

#[test]
fn a_burn_row_the_chain_limits_refuse_is_deferred_before_the_journal() -> Result<()> {
    for (min_allowed_weights, max_weights_limit, code) in
        [(2, u16::MAX, Some("burn_row_min_allowed_weights")), (1, 4096, None), (9, u16::MAX, Some("burn_row_min_allowed_weights"))]
    {
        let root = scratch()?;
        let config = config(root.path())?;
        init_journal(&config, identity())?;
        let mut app = TestApp::open(&config, &bundle())?;
        app.bundle = launch_bundle(10);
        app.epoch = 12;
        app.limits.min_allowed_weights = min_allowed_weights;
        app.limits.max_weights_limit = max_weights_limit;
        for mode in [Mode::DryRun, Mode::Dispatch] {
            let outcome = block_on(apply(&mut app, &head(), mode, &Faults::none()))?;
            match (code, &outcome) {
                (Some(code), AppOutcome::Deferred { code: got, .. }) => assert_eq!(got, &code),
                (None, AppOutcome::DryRun { .. } | AppOutcome::Enqueued { .. }) => {}
                other => panic!("unexpected outcome {other:?}"),
            }
        }
        assert_eq!(app.summary()?.operations, usize::from(code.is_none()));
    }
    Ok(())
}

#[test]
fn a_burn_row_queued_under_hold_by_an_earlier_build_is_retired_for_submit() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    let mut app = TestApp::open(&config, &bundle())?;
    app.bundle = launch_bundle(10);
    app.epoch = 12;
    // An earlier build's held operation of this tempo, with the same row.
    let launch_held = slop_ninja_protocol::ActiveTarget::launch(&chain()).unwrap();
    let a = std::sync::Arc::new(
        crate::package_v2::verify(&launch_bundle(10), &trusted(), &TESTNET_GENESIS.0, NETUID, &launch_held).unwrap(),
    );
    let snap = crate::package_v2::snapshot(&registered(), owner().hotkey.0).unwrap();
    let d = crate::package_v2::derive(a, &snap, validator2().hotkey.0, 12).unwrap();
    let Enqueued::Created(old) = app.enqueue(&d.work, d.input.clone(), OwnerRowPolicy::Hold)? else {
        panic!("created")
    };
    assert_eq!(app.retained_policy(&old)?, Some(OwnerRowPolicy::Hold));
    // The same ID under another policy is refused, like changed input.
    assert!(app.enqueue(&d.work, d.input.clone(), OwnerRowPolicy::Submit).is_err());
    let (id, created) = enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?);
    assert!(created);
    assert_ne!(id, old);
    assert_eq!(app.live.controls_for_tests().context("journal")?.retired(&old)?.as_deref(), Some("owner_row_policy_changed"));
    assert_eq!(app.retained_policy(&id)?, Some(OwnerRowPolicy::Submit));
    assert_eq!(app.summary()?.operations, 2);
    Ok(())
}

/// A restart after the chain limits changed: an operation of this label
/// with the same row and policy is joined whatever the limits read now when
/// it has a signed attempt awaiting recovery or landed and was handed back
/// (its recovery and read-back settle it), while one that never signed is
/// retired (`burn_row_limits_changed`) rather than left to fail rtao's
/// preparation on every attempt, and the tempo is deferred.
#[test]
fn a_retained_burn_row_is_joined_or_retired_when_the_limits_change() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    let open = || -> Result<TestApp> {
        let mut app = TestApp::open(&config, &bundle())?;
        app.bundle = launch_bundle(10);
        app.epoch = 12;
        Ok(app)
    };
    let mut app = open()?;
    let (never_signed, created) = enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?);
    assert!(created);
    // MinAllowedWeights rose before it signed: retired, tempo deferred.
    app.limits.min_allowed_weights = 2;
    match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
        AppOutcome::Deferred { code, .. } => assert_eq!(code, "burn_row_min_allowed_weights"),
        other => panic!("expected the limits deferral, got {other:?}"),
    }
    assert_eq!(retired_reason(&app, &never_signed).as_deref(), Some("burn_row_limits_changed"));
    assert!(app.live.pending_operations(0)?.is_empty());
    // The limit returned: the retired label is skipped and the next queued.
    app.limits.min_allowed_weights = 1;
    let (signed, created) = enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?);
    assert!(created && signed != never_signed);
    drop(app);
    // It signed and its response was lost; then the limit rose again.
    OperationJournal::open(&config.journal_path)?.update(|image| {
        push_lost_response(image.operations.get_mut(&signed).expect("operation retained"));
        Ok(())
    })?;
    let mut app = open()?;
    app.limits.min_allowed_weights = 2;
    assert_eq!(enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?), (signed.clone(), false));
    assert_eq!(retired_reason(&app, &signed), None);
    assert_eq!(app.summary()?.unresolved_attempts, 1);
    drop(app);
    // It landed and rtao handed it back: joined for read-back with no limits read.
    OperationJournal::open(&config.journal_path)?.update(|image| {
        let record = image.operations.get_mut(&signed).expect("operation retained");
        record.extensions.insert("scheduler_handed_off".into(), serde_json::Value::Bool(true));
        Ok(())
    })?;
    let mut app = open()?;
    app.limits.min_allowed_weights = 2;
    assert_eq!(enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?), (signed, false));
    assert_eq!(app.limit_reads, 0);
    assert_eq!(app.summary()?.operations, 2);
    Ok(())
}

/// A rollback past this pin: the queued burn row (in steady state the next
/// tempo's, waiting for its epoch's offset) blocks a build at the previous
/// pin until it is retired for the rollback with the validator stopped.
#[test]
fn the_rollback_check_finds_the_queued_submit_burn_row() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    let id = {
        let mut app = TestApp::open(&config, &bundle())?;
        app.bundle = launch_bundle(10);
        app.epoch = 12;
        enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?).0
    };
    let (report, safe) = crate::live::rollback_check(&config, false)?;
    assert!(!safe);
    assert_eq!(report["blockers"][0]["operation_id"], serde_json::json!(id));
    assert_eq!(report["blockers"][0]["field"], serde_json::json!("effects.owner_rows"));
    assert_eq!(report["next"], serde_json::json!("run again with --retire"));
    let (report, safe) = crate::live::rollback_check(&config, true)?;
    assert!(safe, "{report}");
    assert_eq!(report["retired"], serde_json::json!([id]));
    // A running validator holds the journal: the check refuses.
    let app = TestApp::open(&config, &bundle())?;
    assert!(crate::live::rollback_check(&config, false).is_err());
    drop(app);
    Ok(())
}

#[test]
fn a_reward_v1_launch_bundle_never_expires_and_a_revocation_stops_it() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    let mut app = TestApp::open(&config, &bundle())?;
    app.bundle = launch_bundle(10);
    // Past the renew-by hint (epoch 50) the burn row is still queued: an
    // authorization has no end epoch.
    app.epoch = 60;
    match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
        AppOutcome::Enqueued { .. } => {}
        other => panic!("expected the burn row queued past the hint, got {other:?}"),
    }
    app.epoch = 10_000;
    match block_on(apply(&mut app, &head(), Mode::DryRun, &Faults::none()))? {
        AppOutcome::DryRun { .. } => {}
        other => panic!("expected a dry run long past the hint, got {other:?}"),
    }
    // A signed revocation, journaled, stops it: the tempo defers
    // (`authorization_revoked`) and queued work that never signed is retired.
    let store = crate::revocations::path(&config.journal_path);
    let launch = slop_ninja_protocol::sign(
        slop_ninja_protocol::LaunchAuthorization::new(&chain(), 1, 10).unwrap(),
        &authorization_key(),
    )
    .unwrap();
    let launch_held = slop_ninja_protocol::ActiveTarget::launch(&chain()).unwrap();
    let checked = slop_ninja_protocol::LaunchAuthorization::check(&launch, &trusted(), &chain(), &launch_held).unwrap();
    let revocation = slop_ninja_protocol::sign(
        slop_ninja_protocol::RewardIntentRevocation::new(&checked, 1, 61).unwrap(),
        &authorization_key(),
    )
    .unwrap()
    .to_bytes()
    .unwrap();
    assert!(crate::revocations::record(&store, &trusted(), &chain(), &revocation)?.new);
    app.revocation_store = Some(store.clone());
    app.epoch = 62;
    match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
        AppOutcome::Deferred { code, .. } => assert_eq!(code, "authorization_revoked"),
        other => panic!("expected the revocation deferral, got {other:?}"),
    }
    let pending = AppPorts::pending_operations(&app, 0)?;
    assert!(pending.iter().all(|p| !p.retirable()), "never-signed work of the revoked target is retired: {pending:?}");
    // A later launch authorization re-authorizes the burn row.
    app.bundle = launch_bundle(63);
    app.epoch = 63;
    match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
        AppOutcome::Enqueued { .. } => {}
        other => panic!("expected the re-authorized burn row queued, got {other:?}"),
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
    // No end epoch: past the hint the next tempo's work is still derived.
    assert_eq!(d.work_for_tempo(validator2().hotkey.0, 51).unwrap().tempo, Some(51));
    assert_eq!(d.work_for_tempo(validator2().hotkey.0, 100_000).unwrap().tempo, Some(100_000));
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
            receipts: None,
            carrier_receipts: None,
        })
        .unwrap()
    }

    /// The bundle schema v3 form of [`bundle`]: the receipt index and the
    /// receipts (cut into `pieces` chunk objects at receipt boundaries)
    /// written to `dir` and named by absolute path, digest and length.
    pub fn bundle_v3(c: &Cohort, dir: &Path, pieces: usize) -> Vec<u8> {
        use sha2::{Digest as _, Sha256};
        let rk = slop_ninja_protocol::fixtures::receipt_key();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            crate::package_v2::ObjectRef {
                path: path.to_str().unwrap().to_owned(),
                sha256: hex::encode(Sha256::digest(bytes)),
                bytes: bytes.len() as u64,
            }
        };
        let index = write("receipt-index.signed", &sign(c.receipt_index.clone(), &rk).unwrap().to_bytes().unwrap());
        // Cut at receipt boundaries: walk the length prefixes.
        let mut cuts = vec![0usize];
        let mut at = 0usize;
        while at < c.chunk.len() {
            at += 4 + u32::from_le_bytes(c.chunk[at..at + 4].try_into().unwrap()) as usize;
            cuts.push(at);
        }
        let per = (cuts.len() - 1).div_ceil(pieces.max(1)).max(1);
        let chunks = cuts
            .iter()
            .step_by(per)
            .copied()
            .chain(std::iter::once(c.chunk.len()))
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|w| w[1] > w[0])
            .enumerate()
            .map(|(n, w)| write(&format!("chunk-{n}.bin"), &c.chunk[w[0]..w[1]]))
            .collect();
        let mut b: crate::package_v2::BundleV2 = serde_json::from_slice(&bundle(c)).unwrap();
        b.schema = crate::package_v2::BUNDLE_SCHEMA_V3.into();
        b.source.as_mut().unwrap().receipt_index = None;
        b.receipt_chunks.clear();
        b.receipts = Some(crate::package_v2::ReceiptsRef { index, chunks });
        serde_json::to_vec(&b).unwrap()
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
            receipts: None,
            carrier_receipts: None,
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

    /// A first cohort with one credited entrant `tag` and no predecessor,
    /// bound to receipts: its row pays only that entrant's UID.
    pub fn single(tag: u8) -> (Cohort, Vec<Spec>) {
        let specs = vec![Spec::new(tag, bp(9600))];
        let manifest = fx::manifest(fx::chain(), 7, 1000, fx::sigma_permille(40), 0, &specs);
        let (package, _) = fx::finalized(&manifest, bp(9100), None, &specs).unwrap();
        let index = fx::default_index(&manifest.references);
        let launch = CalculationTargetV2::launch_burn(fx::chain()).digest().unwrap();
        (bound(manifest, package, index, launch, &specs), specs)
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
        limits: crate::app::WeightLimits {
            min_allowed_weights: 1,
            max_weights_limit: u16::MAX,
            population: 64,
            own_permit: true,
            null_consensus: false,
        },
        limit_reads: 0,
        revocation_store: None,
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


/// The owner-row policy of reward-v1 cohort targets, decided from the row at
/// the first commitment ([`apply`]) and at the per-tempo re-derive
/// ([`crate::app::derive_queueable`], which `LivePorts::rederive` calls after
/// its snapshot reads): a target whose row pays only the owner UID (every
/// entrant unregistered, so every unit burns) is queued under `Submit`; one
/// paying this validator's own UID, alone or with the owner, is deferred as
/// `row_would_be_held` and nothing is journaled.
#[test]
fn reward_v1_cohort_targets_choose_their_owner_row_policy_from_the_row() -> Result<()> {
    use crate::app::derive_queueable;
    let (first, _, specs) = rv1::cohorts();
    let owner_hotkey = owner().hotkey.0;
    let authorized = |bundle: &[u8]| {
        let held = slop_ninja_protocol::ActiveTarget::launch(&slop_ninja_protocol::fixtures_reward_v1::chain()).unwrap();
        std::sync::Arc::new(
            crate::package_v2::verify(
                bundle,
                &slop_ninja_protocol::fixtures::trusted_keys(),
                &TESTNET_GENESIS.0,
                rv1::RV1_NETUID,
                &held,
            )
            .unwrap(),
        )
    };
    let rederive = |app: &mut TestApp, bundle: &[u8], own: [u8; 32], label: u64| {
        let v1 = app.snapshot.clone();
        let snap = crate::package_v2::snapshot(&v1, owner_hotkey).unwrap();
        block_on(derive_queueable(app, authorized(bundle), &snap, &v1, own, owner_hotkey, label, Mode::Dispatch))
    };

    // Every entrant unregistered: every unit burns to the owner UID.
    let root = scratch()?;
    let mut app = rv1_app(root.path())?;
    app.snapshot = rv1::snapshot(&[]);
    app.bundle = rv1::bundle(&first);
    app.epoch = 80;
    match block_on(apply(&mut app, &head(), Mode::DryRun, &Faults::none()))? {
        AppOutcome::DryRun { entries, owner_rows, .. } => {
            assert_eq!(entries, vec![(2, 65535)]);
            assert_eq!(owner_rows, OwnerRowPolicy::Submit);
        }
        other => panic!("expected a dry run, got {other:?}"),
    }
    let (id, created) = enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?);
    assert!(created);
    assert_eq!(app.retained_policy(&id)?, Some(OwnerRowPolicy::Submit));
    let image: serde_json::Value = serde_json::from_slice(&std::fs::read(root.path().join("journal.json"))?)?;
    assert_eq!(image["operations"][&id]["intent"]["effects"]["owner_rows"], serde_json::json!("submit"));
    // The next tempo's re-derive decides the same way.
    let bundle = app.bundle.clone();
    let (next, policy, label) = rederive(&mut app, &bundle, validator2().hotkey.0, 81)?.expect("queueable");
    assert_eq!((policy, label), (OwnerRowPolicy::Submit, 81));
    assert_eq!(next.entries(), vec![(2, 65535)]);

    // One credited entrant registered, and it is this validator's hotkey:
    // the row pays its own UID and the owner's.
    let credited = first.package.entrants.iter().find(|e| e.units > 0).expect("a credited entrant").identity.hotkey;
    let own_specs: Vec<_> =
        specs.iter().filter(|s| slop_ninja_protocol::fixtures_reward_v1::hotkey(s.tag) == credited).cloned().collect();
    assert_eq!(own_specs.len(), 1);
    let root = scratch()?;
    let mut app = rv1_app(root.path())?;
    app.snapshot = rv1::snapshot(&own_specs);
    app.hold = vec![credited.0, owner_hotkey];
    app.bundle = rv1::bundle(&first);
    app.epoch = 80;
    let row = crate::package_v2::derive(
        authorized(&app.bundle),
        &crate::package_v2::snapshot(&app.snapshot, owner_hotkey).unwrap(),
        credited.0,
        80,
    )
    .unwrap()
    .entries();
    assert_eq!(row.iter().map(|e| e.0).collect::<Vec<_>>(), vec![2, u16::from(own_specs[0].tag)], "{row:?}");
    for mode in [Mode::DryRun, Mode::Dispatch] {
        match block_on(apply(&mut app, &head(), mode, &Faults::none()))? {
            AppOutcome::Deferred { code, .. } => assert_eq!(code, "row_would_be_held", "{mode:?}"),
            other => panic!("{mode:?}: expected row_would_be_held, got {other:?}"),
        }
    }
    assert_eq!(app.summary()?.operations, 0);
    let bundle = app.bundle.clone();
    assert_eq!(rederive(&mut app, &bundle, credited.0, 81)?.unwrap_err().code, "row_would_be_held");
    assert_eq!(app.summary()?.operations, 0);

    // A target whose one credited entrant is this validator: own UID only.
    let (single, single_specs) = rv1::single(0x44);
    let own = slop_ninja_protocol::fixtures_reward_v1::hotkey(0x44).0;
    let root = scratch()?;
    let mut app = rv1_app(root.path())?;
    app.snapshot = rv1::snapshot(&single_specs);
    app.hold = vec![own, owner_hotkey];
    app.bundle = rv1::bundle(&single);
    let activated = authorized(&app.bundle).verified.active.activated_epoch().expect("activation epoch");
    app.epoch = activated;
    let row = crate::package_v2::derive(
        authorized(&app.bundle),
        &crate::package_v2::snapshot(&app.snapshot, owner_hotkey).unwrap(),
        own,
        activated,
    )
    .unwrap()
    .entries();
    assert_eq!(row, vec![(0x44, 65535)]);
    for mode in [Mode::DryRun, Mode::Dispatch] {
        match block_on(apply(&mut app, &head(), mode, &Faults::none()))? {
            AppOutcome::Deferred { code, .. } => assert_eq!(code, "row_would_be_held", "{mode:?}"),
            other => panic!("{mode:?}: expected row_would_be_held, got {other:?}"),
        }
    }
    assert_eq!(app.summary()?.operations, 0);
    let bundle = app.bundle.clone();
    assert_eq!(rederive(&mut app, &bundle, own, activated + 1)?.unwrap_err().code, "row_would_be_held");
    assert_eq!(app.summary()?.operations, 0);
    Ok(())
}

/// The dispatch loop alerts on every rtao outcome that leaves queued work
/// unsigned, and never on a scheduling wait, unresolved work or a submission.
#[test]
fn outcomes_that_leave_queued_work_unsigned_are_alerted() {
    // `Refused` (reported as `refused:<check>`) needs rtao's catalog
    // error code, which this crate does not depend on.
    use crate::live::not_submitted;
    use rtao_neuron::weights::WeightAttemptOutcome as O;
    assert_eq!(not_submitted(&O::Failed("admission deadline".into())).as_deref(), Some("failed"));
    assert_eq!(not_submitted(&O::HoldPrevious).as_deref(), Some("hold_previous"));
    assert_eq!(
        not_submitted(&O::PausedSurface { descriptor: String::new(), detail: String::new() }).as_deref(),
        Some("paused_surface")
    );
    assert_eq!(not_submitted(&O::Skipped(String::new())).as_deref(), Some("skipped"));
    for quiet in [
        O::Scheduled,
        O::RateLimited { retry_after_block: 1, remediation: String::new() },
        O::Unresolved(String::new()),
        O::Submitted { block_hash: H256([0; 32]), block_number: 1 },
    ] {
        assert_eq!(not_submitted(&quiet), None, "{quiet:?}");
    }
}

/// The coordinator sizes a standing template's roster with the protocol's
/// `receipt_bundle_bytes`: each receipt grows the bundle this validator
/// assembles by exactly that many bytes, and the bound is this validator's.
#[test]
fn receipts_grow_the_bundle_by_the_protocol_receipt_length() {
    let size = |n: u64| rv1::bundle(&rv1::sized(n)).len() as u64;
    let grown = size(2_000) - size(1_000);
    assert_eq!(grown, slop_ninja_protocol::target_bundle::receipt_bundle_bytes(1_000).unwrap());
    assert_eq!(crate::package::MAX_BUNDLE_BYTES, 64 << 20);
}

/// Bundle schema v3 names the receipt index and chunks by reference: the
/// same cohort verifies with the same receipts as its embedded form,
/// whatever the chunking, and a cohort whose embedded receipts exceed the
/// 64 MiB file bound verifies by reference in a file of a few kilobytes.
#[test]
fn a_v3_bundle_verifies_receipts_by_reference_beyond_the_embedded_bound() -> Result<()> {
    use slop_ninja_protocol::target_bundle::{VALIDATOR_BUNDLE_BYTES, receipt_bundle_bytes};
    let root = scratch()?;
    let keys = slop_ninja_protocol::fixtures::trusted_keys();
    let held = slop_ninja_protocol::ActiveTarget::launch(&slop_ninja_protocol::fixtures_reward_v1::chain())?;
    let verify = |b: &[u8]| crate::package_v2::verify(b, &keys, &TESTNET_GENESIS.0, rv1::RV1_NETUID, &held);
    let small = rv1::sized(1_000);
    let embedded = verify(&rv1::bundle(&small)).unwrap();
    for pieces in [1, 3, 1_000] {
        let dir = root.path().join(format!("small-{pieces}"));
        std::fs::create_dir(&dir)?;
        let v3 = verify(&rv1::bundle_v3(&small, &dir, pieces)).unwrap();
        assert_eq!(v3.receipts_verified, embedded.receipts_verified);
        assert_eq!(v3.verified.target_digest, embedded.verified.target_digest);
    }
    // Past the embedded bound: the v2 form is refused, the v3 form holds.
    let per = receipt_bundle_bytes(1)?;
    let receipts = VALIDATOR_BUNDLE_BYTES / per + 1_000;
    let big = rv1::sized(receipts);
    let v1 = rv1::bundle(&big);
    assert!(v1.len() as u64 > VALIDATOR_BUNDLE_BYTES);
    assert_eq!(verify(&v1).unwrap_err().code, "bundle_too_large");
    let dir = root.path().join("big");
    std::fs::create_dir(&dir)?;
    let v3 = rv1::bundle_v3(&big, &dir, 7);
    assert!(v3.len() < 64 << 10, "{} bytes", v3.len());
    assert_eq!(verify(&v3).unwrap().receipts_verified, receipts);
    Ok(())
}

/// Every referenced object is checked before it is used: a missing, short,
/// altered, relative or extra reference, receipts in the other schema's
/// form, and an incomplete or duplicated chunk set are all refused.
#[test]
fn a_v3_bundle_refuses_objects_that_are_not_the_referenced_ones() -> Result<()> {
    use crate::package_v2::{BundleV2, ObjectRef};
    let root = scratch()?;
    let keys = slop_ninja_protocol::fixtures::trusted_keys();
    let held = slop_ninja_protocol::ActiveTarget::launch(&slop_ninja_protocol::fixtures_reward_v1::chain())?;
    let code = |b: &BundleV2| {
        crate::package_v2::verify(&serde_json::to_vec(b).unwrap(), &keys, &TESTNET_GENESIS.0, rv1::RV1_NETUID, &held)
            .map(|_| "ok")
            .unwrap_or_else(|r| r.code)
    };
    let c = rv1::sized(1_000);
    let fresh = |name: &str| -> BundleV2 {
        let dir = root.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        serde_json::from_slice(&rv1::bundle_v3(&c, &dir, 4)).unwrap()
    };
    assert_eq!(code(&fresh("ok")), "ok");
    // A missing chunk file, and a missing index file.
    let b = fresh("missing");
    std::fs::remove_file(&b.receipts.as_ref().unwrap().chunks[1].path)?;
    assert_eq!(code(&b), "receipts_incomplete");
    let b = fresh("missing-index");
    std::fs::remove_file(&b.receipts.as_ref().unwrap().index.path)?;
    assert_eq!(code(&b), "receipts_incomplete");
    // A file of another length, or the same length with other bytes.
    let b = fresh("short");
    let p = b.receipts.as_ref().unwrap().chunks[0].path.clone();
    let mut bytes = std::fs::read(&p)?;
    bytes.pop();
    std::fs::write(&p, &bytes)?;
    assert_eq!(code(&b), "receipt_object_mismatch");
    let b = fresh("altered");
    let p = b.receipts.as_ref().unwrap().chunks[2].path.clone();
    let mut bytes = std::fs::read(&p)?;
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&p, &bytes)?;
    assert_eq!(code(&b), "receipt_object_mismatch");
    // A reference whose digest names other bytes, even if they verify.
    let mut b = fresh("renamed");
    let r = b.receipts.as_mut().unwrap();
    r.chunks[0].sha256 = r.chunks[1].sha256.clone();
    assert_eq!(code(&b), "receipt_object_mismatch");
    // A relative path.
    let mut b = fresh("relative");
    let r = b.receipts.as_mut().unwrap();
    r.chunks[0].path = std::path::Path::new(&r.chunks[0].path).file_name().unwrap().to_str().unwrap().into();
    assert_eq!(code(&b), "bundle_malformed");
    // A chunk left out, and a chunk named twice (each receipt once).
    let mut b = fresh("dropped");
    b.receipts.as_mut().unwrap().chunks.pop();
    assert_eq!(code(&b), "receipt_object_mismatch");
    let mut b = fresh("twice");
    let r = b.receipts.as_mut().unwrap();
    let again: ObjectRef = r.chunks[0].clone();
    r.chunks.push(again);
    assert_eq!(code(&b), "receipt_object_mismatch");
    // Receipts in the other schema's form, or with nothing to belong to.
    let mut b = fresh("mixed");
    b.receipt_chunks = vec!["00".into()];
    assert_eq!(code(&b), "bundle_malformed");
    let mut b = fresh("v1-with-refs");
    b.schema = crate::package_v2::BUNDLE_SCHEMA_V2.into();
    assert_eq!(code(&b), "bundle_malformed");
    let mut b = fresh("no-receipts");
    b.receipts = None;
    assert_eq!(code(&b), "receipts_incomplete");
    let mut b = fresh("orphan");
    b.carrier_receipts = b.receipts.clone();
    assert_eq!(code(&b), "bundle_malformed");
    Ok(())
}
