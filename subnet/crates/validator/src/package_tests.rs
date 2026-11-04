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
/// and refuses a forged one and another chain's. Loading does not verify
/// signatures again: an entry journaled under a rotated key still loads, and
/// an altered entry is reported and applied from its summary while the
/// others load as before.
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
    assert_eq!(crate::revocations::load(&store, &chain())?, Default::default());
    let first = revocation(20, &authorization_key());
    assert!(crate::revocations::record(&store, &trusted(), &chain(), &first)?.new);
    assert!(!crate::revocations::record(&store, &trusted(), &chain(), &first)?.new);
    assert!(crate::revocations::record(&store, &trusted(), &chain(), &revocation(30, &authorization_key()))?.new);
    assert_eq!(crate::revocations::load(&store, &chain())?.records.len(), 2);
    assert_eq!(crate::revocations::entries(&store)?[0].issued_epoch, 20);
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(std::fs::metadata(&store)?.permissions().mode() & 0o777, 0o600);
    // Signed by another key, or for another chain: refused, nothing written.
    assert!(crate::revocations::record(&store, &trusted(), &chain(), &revocation(40, &package_key())).is_err());
    let other = ChainBinding { netuid: NETUID + 1, ..chain() };
    assert!(crate::revocations::record(&store, &trusted(), &other, &first).is_err());
    let loaded = crate::revocations::load(&store, &chain())?;
    assert_eq!((loaded.records.len(), loaded.problems.len()), (2, 0));
    // After the trust roots rotate, the journaled entries still load as
    // they were checked; nothing re-verifies them under the new keys.
    let rotated = TrustedKeys::new(
        package_key().verifying_key().to_bytes(),
        slop_ninja_protocol::fixtures::receipt_key().verifying_key().to_bytes(),
        SigningKey::from_bytes(&[0x77; 32]).verifying_key().to_bytes(),
    )
    .unwrap();
    assert!(crate::revocations::record(&store, &rotated, &chain(), &first).is_err(), "a new entry is checked");
    assert_eq!(crate::revocations::load(&store, &chain())?, loaded);
    // An altered entry is reported and applied from its recorded summary;
    // the other entry loads as before.
    let text = std::fs::read_to_string(&store)?;
    let entry = &crate::revocations::entries(&store)?[0].signed;
    std::fs::write(&store, text.replace(entry.as_str(), "zz"))?;
    let altered = crate::revocations::load(&store, &chain())?;
    assert_eq!(altered.problems.len(), 1, "{altered:?}");
    assert_eq!(altered.records.len(), 2);
    assert_eq!(altered.records[0].target, loaded.records[0].target);
    assert_eq!(altered.records[0].issued_epoch, 20);
    assert_eq!(altered.records[1], loaded.records[1]);
    // A store that is not readable as a whole is an error.
    std::fs::write(&store, b"{")?;
    assert!(crate::revocations::load(&store, &chain()).is_err());
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
            "/home/operator/.local/state/slop_ninja_validator/NAME/weights-journal.json",
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
    fn fixture_packages(&self) -> bool {
        true
    }
    fn trusted_keys(&self) -> Result<TrustedKeys> {
        Ok(self.keys.clone().unwrap_or_else(trusted))
    }
    fn chain(&self) -> ([u8; 32], u16) {
        self.served
    }
    fn revocations(&self) -> Result<crate::revocations::Loaded> {
        let Some(store) = &self.revocation_store else { return Ok(Default::default()) };
        let chain = ChainBinding {
            genesis: slop_ninja_protocol::Digest(self.served.0),
            netuid: self.served.1,
            mechanism: 0,
        };
        crate::revocations::load(store, &chain)
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
            "/home/operator/.local/state/slop_ninja_validator/NAME/weights-journal.json",
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

/// A coordinator-row bundle of the launch burn row: the launch authorization
/// issued at `issued_epoch` and the coordinator's signed row of it, every
/// unit to the owner's UID in [`registered`].
fn launch_bundle(issued_epoch: u64) -> Vec<u8> {
    let auth = slop_ninja_protocol::LaunchAuthorization::new(&chain(), 1, issued_epoch).unwrap();
    let row = slop_ninja_protocol::CoordinatorRow {
        chain: chain(),
        reward_rule: slop_ninja_protocol::RewardRule::RewardV1,
        source: slop_ninja_protocol::RowSource::LaunchBurn,
        target: auth.target,
        activated_epoch: None,
        authorization: slop_ninja_protocol::Record::digest(&auth).unwrap(),
        issued_epoch,
        epoch: issued_epoch,
        registration_block: 100,
        owner_hotkey: owner().hotkey,
        entries: vec![slop_ninja_protocol::CoordinatorRowEntry {
            uid: owner().uid,
            hotkey: owner().hotkey,
            registration_block: owner().registration_block,
            weight: u16::MAX,
        }],
    };
    let auth = slop_ninja_protocol::sign(auth, &authorization_key()).unwrap().to_bytes().unwrap();
    let row = slop_ninja_protocol::sign(row, &authorization_key()).unwrap().to_bytes().unwrap();
    serde_json::to_vec(&crate::package_v2::row_bundle("launch", &auth, None, &row)).unwrap()
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
    // The burn row queued at epoch 60 never signed: it is retirable now.
    let queued: Vec<String> = AppPorts::pending_operations(&app, 0)?
        .into_iter()
        .filter(|p| p.retirable())
        .map(|p| p.id)
        .collect();
    assert!(!queued.is_empty(), "the burn row queued at epoch 60 is still pending");
    app.epoch = 62;
    match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
        AppOutcome::Deferred { code, .. } => assert_eq!(code, "authorization_revoked"),
        other => panic!("expected the revocation deferral, got {other:?}"),
    }
    for id in &queued {
        assert_eq!(retired_reason(&app, id).as_deref(), Some("authorization_revoked"), "{id} is retired");
    }
    let pending = AppPorts::pending_operations(&app, 0)?;
    assert!(
        pending.iter().all(|p| !queued.contains(&p.id)),
        "never-signed work of the revoked target is no longer pending: {pending:?}"
    );
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
    forged["authorization"] = serde_json::json!(hex::encode(other));
    assert_eq!(
        crate::package_v2::verify(&serde_json::to_vec(&forged)?, &trusted(), &TESTNET_GENESIS.0, NETUID, &launch_held)
            .unwrap_err()
            .code,
        "bundle_unverified"
    );
    Ok(())
}


/// The continuous dispatch loop's revocation path, the one a running
/// validator takes ([`LivePorts::revocation_head`] at each finalized head),
/// against the real journal and signing guard: a revocation journaled mid-run
/// withdraws the queued operation's signing grant (the guard refuses it as
/// `slop_ninja_authorization_revoked`), retires it because it never signed,
/// alerts once, and the re-commit path derives nothing for the target; a
/// later authorization fetched resumes commits with a fresh grant.
#[test]
fn the_dispatch_loop_withdraws_a_revoked_target_then_resumes_on_reauthorization() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    let mut app = TestApp::open(&config, &bundle())?;
    app.bundle = launch_bundle(10);
    app.epoch = 60;
    let first = match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
        AppOutcome::Enqueued { operation_id, .. } => operation_id,
        other => panic!("expected the burn row queued, got {other:?}"),
    };
    assert!(app.live.authorized_for_tests().get(&first).is_some(), "the queued burn row holds a grant");
    let launch_held = slop_ninja_protocol::ActiveTarget::launch(&chain()).unwrap();
    let current = crate::package_v2::verify(&launch_bundle(10), &trusted(), &TESTNET_GENESIS.0, NETUID, &launch_held).unwrap();
    let hotkey = identity().hotkey.0;
    let mut watch = crate::live::RevocationWatch::default();
    // No revocation journaled: the head changes nothing.
    assert_eq!(app.live.revocation_head(&current, &mut watch, 0)?, Default::default());
    assert!(current.check_epoch(hotkey, 61).is_ok());

    // A revocation journaled mid-run (by the operator or the fetcher).
    let launch = slop_ninja_protocol::sign(
        slop_ninja_protocol::LaunchAuthorization::new(&chain(), 1, 10).unwrap(),
        &authorization_key(),
    )
    .unwrap();
    let checked = slop_ninja_protocol::LaunchAuthorization::check(&launch, &trusted(), &chain(), &launch_held).unwrap();
    let revocation = slop_ninja_protocol::sign(
        slop_ninja_protocol::RewardIntentRevocation::new(&checked, 1, 61).unwrap(),
        &authorization_key(),
    )
    .unwrap();
    let store = crate::revocations::path(&config.journal_path);
    assert!(crate::revocations::record(&store, &trusted(), &chain(), &revocation.to_bytes()?)?.new);
    let revoked = app.live.revocation_head(&current, &mut watch, 0)?;
    assert_eq!(revoked.revoked, Some(slop_ninja_protocol::Record::digest(&revocation.record)?));
    assert!(revoked.alerted, "alerted at the first head");
    assert_eq!((revoked.withdrawn, revoked.retired), (1, 1), "{revoked:?}");
    let guard = app.live.authorized_for_tests();
    assert!(guard.is_withdrawn(&first) && guard.get(&first).is_none());
    assert_eq!(retired_reason(&app, &first).as_deref(), Some("authorization_revoked"));
    assert!(AppPorts::pending_operations(&app, 0)?.iter().all(|p| p.id != first));
    // The re-commit path derives no new operation for the revoked target.
    assert_eq!(current.check_epoch(hotkey, 62).unwrap_err().code, "authorization_revoked");
    // The next head finds it again without alerting again.
    let again = app.live.revocation_head(&current, &mut watch, 0)?;
    assert_eq!((again.revoked, again.alerted, again.withdrawn), (Some(slop_ninja_protocol::Record::digest(&revocation.record)?), false, 0));
    // A store that can no longer be read as a whole keeps the revocations
    // this process loaded in force (alerted once per distinct error by the
    // head path); a process with none loaded yet refuses instead.
    let saved = std::fs::read(&store)?;
    std::fs::write(&store, b"{ not json")?;
    let kept = app.live.revocation_head(&current, &mut watch, 0)?;
    assert_eq!(kept.revoked, again.revoked, "the loaded revocation stays in force");
    assert!(AppPorts::revocations(&app.live)?.store_error.is_some());
    assert!(current.check_epoch(hotkey, 62).is_err());
    let fresh = crate::live::LivePorts::new(config.clone(), identity());
    assert!(AppPorts::revocations(&fresh).is_err(), "nothing loaded yet: refused");
    std::fs::write(&store, &saved)?;
    assert!(AppPorts::revocations(&app.live)?.store_error.is_none());

    // A later authorization is fetched: the next head finds no revocation of
    // it, its operations derive again, and a new commitment gets a grant.
    let renewed = crate::package_v2::verify(&launch_bundle(63), &trusted(), &TESTNET_GENESIS.0, NETUID, &launch_held).unwrap();
    assert_eq!(app.live.revocation_head(&renewed, &mut watch, 0)?, Default::default());
    assert!(renewed.check_epoch(hotkey, 63).is_ok());
    app.revocation_store = Some(store);
    app.bundle = launch_bundle(63);
    app.epoch = 63;
    let resumed = match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
        AppOutcome::Enqueued { operation_id, .. } => operation_id,
        other => panic!("expected the re-authorized burn row queued, got {other:?}"),
    };
    assert_ne!(resumed, first);
    let guard = app.live.authorized_for_tests();
    assert!(guard.get(&resumed).is_some() && !guard.is_withdrawn(&resumed));
    Ok(())
}

/// Audit B5: in per-tempo mode the dispatch loop queues the next tempo only
/// once the tracked operation is settled. A journaled revocation applied at
/// a finalized head (the loop's own `revocation_head`) retires the tracked
/// operation before it signs; it can never land, so it has nothing to read
/// back. Before, the loop waited for that read-back forever, so even a
/// refreshed authorization never queued another tempo.
#[test]
fn a_tracked_operation_retired_before_signing_needs_no_read_back() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    let mut app = TestApp::open(&config, &bundle())?;
    app.bundle = launch_bundle(10);
    app.epoch = 60;
    let first = match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
        AppOutcome::Enqueued { operation_id, .. } => operation_id,
        other => panic!("expected the burn row queued, got {other:?}"),
    };
    // Queued and not yet signed: it may still land, so it waits.
    assert!(!app.live.tracked_settled(&first, false)?);
    assert!(app.live.tracked_settled(&first, true)?, "a read-back settles it");
    let launch_held = slop_ninja_protocol::ActiveTarget::launch(&chain()).unwrap();
    let current = crate::package_v2::verify(&launch_bundle(10), &trusted(), &TESTNET_GENESIS.0, NETUID, &launch_held).unwrap();
    // The target's work, derived while it is still authorized.
    let snap = crate::package_v2::snapshot(&registered(), owner().hotkey.0).unwrap();
    let work = crate::package_v2::derive(
        std::sync::Arc::new(
            crate::package_v2::verify(&launch_bundle(10), &trusted(), &TESTNET_GENESIS.0, NETUID, &launch_held).unwrap(),
        ),
        &snap,
        validator2().hotkey.0,
        60,
    )
    .unwrap()
    .work;
    assert!(app.live.per_tempo_gate(&work, &first, false)?.is_none(), "queued: the gate is shut");
    let launch = slop_ninja_protocol::sign(
        slop_ninja_protocol::LaunchAuthorization::new(&chain(), 1, 10).unwrap(),
        &authorization_key(),
    )
    .unwrap();
    let checked = slop_ninja_protocol::LaunchAuthorization::check(&launch, &trusted(), &chain(), &launch_held).unwrap();
    let revocation = slop_ninja_protocol::sign(
        slop_ninja_protocol::RewardIntentRevocation::new(&checked, 1, 61).unwrap(),
        &authorization_key(),
    )
    .unwrap();
    let store = crate::revocations::path(&config.journal_path);
    assert!(crate::revocations::record(&store, &trusted(), &chain(), &revocation.to_bytes()?)?.new);
    let mut watch = crate::live::RevocationWatch::default();
    let revoked = app.live.revocation_head(&current, &mut watch, 0)?;
    assert_eq!(revoked.retired, 1);
    assert_eq!(retired_reason(&app, &first).as_deref(), Some("authorization_revoked"));
    // Retired before it signed: settled without a read-back, so the loop's
    // per-tempo block runs again (and a refreshed authorization resumes).
    assert!(app.live.tracked_settled(&first, false)?, "retired before signing: nothing to read back");
    // Fix round 1: the loop's whole per-tempo gate, not only the helper.
    // Before the revocation the queued operation held it shut; retired, it
    // is handed back and the gate opens with no read-back.
    let retained = app.live.per_tempo_gate(&work, &first, false)?.expect("the gate opens without a read-back");
    assert!(retained.iter().any(|op| op.id == first && op.handed_off), "{retained:?}");
    // A refreshed authorization then queues the next tempo's operation.
    app.revocation_store = Some(store);
    app.bundle = launch_bundle(63);
    app.epoch = 63;
    let next = match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
        AppOutcome::Enqueued { operation_id, .. } => operation_id,
        other => panic!("expected the next tempo queued after the refresh, got {other:?}"),
    };
    assert_ne!(next, first);
    assert!(app.live.authorized_for_tests().get(&next).is_some(), "the next tempo holds a fresh grant");
    Ok(())
}

/// Fix round 1 (B5): the per-tempo gate stays shut while the tracked
/// operation is queued and may still land.
#[test]
fn the_per_tempo_gate_waits_for_a_queued_tracked_operation() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    let mut app = TestApp::open(&config, &bundle())?;
    app.bundle = launch_bundle(10);
    app.epoch = 60;
    let first = match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
        AppOutcome::Enqueued { operation_id, .. } => operation_id,
        other => panic!("expected the burn row queued, got {other:?}"),
    };
    let launch_held = slop_ninja_protocol::ActiveTarget::launch(&chain()).unwrap();
    let a = std::sync::Arc::new(
        crate::package_v2::verify(&launch_bundle(10), &trusted(), &TESTNET_GENESIS.0, NETUID, &launch_held).unwrap(),
    );
    let snap = crate::package_v2::snapshot(&registered(), owner().hotkey.0).unwrap();
    let work = crate::package_v2::derive(a, &snap, validator2().hotkey.0, 60).unwrap().work;
    assert!(app.live.per_tempo_gate(&work, &first, false)?.is_none(), "queued, unsigned: it may still land");
    assert!(app.live.per_tempo_gate(&work, &first, true)?.is_none(), "read back but not yet handed back by rtao");
    Ok(())
}

/// Lane journal-review: the tracked commit was signed, included and refused
/// by the chain with a refusal that settles (the weights rate limit). The
/// first receipt read fails: the failure waits for decoding and nothing
/// halts or retires. The next read settles it and retires the operation, so
/// the per-tempo loop's `tracked_settled` follows it without a read-back,
/// and the next application pass skips its label and queues another.
#[test]
fn a_settled_refusal_of_the_tracked_commit_lets_the_next_tempo_follow() -> Result<()> {
    use slop_ninja_subnet::{DecodedError, ReceiptRead, review::FailureRef};
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
    let (first, created) = enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?);
    assert!(created);
    drop(app);
    OperationJournal::open(&config.journal_path)?.update(|image| {
        let record = image.operations.get_mut(&first).expect("operation retained");
        push_lost_response(record);
        record.attempts[0].outcome = rtao_client::journal::AttemptOutcome::Finalized {
            block: H256([0x61; 32]),
            number: 8_340_480,
            success: false,
            extensions: BTreeMap::new(),
        };
        record.extensions.insert("scheduler_handed_off".into(), serde_json::Value::Bool(true));
        Ok(())
    })?;
    let mut app = open()?;
    assert!(!app.live.tracked_settled(&first, false)?);
    let controls = app.live.controls_for_tests().context("journal")?;

    let fail = |_: FailureRef| async {
        Err::<ReceiptRead, _>(slop_ninja_subnet::ReadFailure {
            class: "timeout".into(),
            lasting: false,
            detail: "rpc request failed".into(),
        })
    };
    let report = block_on(controls.decode_failures_with(fail))?;
    assert_eq!((report.unreadable.len(), report.settled), (1, 0));
    let summary = controls.inspect()?;
    assert_eq!((summary.failures_awaiting_decode, summary.finalized_failures), (1, 0));
    assert_eq!(retired_reason(&app, &first), None);
    assert!(!app.live.tracked_settled(&first, false)?);

    let refuse = |at: FailureRef| async move {
        Ok(ReceiptRead {
            hash: at.hash,
            block_hash: Some(at.block_hash),
            block_number: Some(at.block_number),
            success: false,
            error: Some(DecodedError {
                kind: "Module".into(),
                pallet: Some("SubtensorModule".into()),
                name: "CommittingWeightsTooFast".into(),
            }),
        })
    };
    let report = block_on(controls.decode_failures_with(refuse))?;
    assert_eq!((report.settled, report.retired), (1, 1));
    assert_eq!(report.refusals[0].operation, first);
    assert!(report.refusals[0].tempo.is_some());
    assert_eq!(retired_reason(&app, &first).as_deref(), Some(slop_ninja_subnet::review::RETIRED_SETTLED));
    let summary = controls.inspect()?;
    assert_eq!((summary.failures_awaiting_decode, summary.finalized_failures, summary.reviewed_failures), (0, 0, 1));
    assert!(app.live.tracked_settled(&first, false)?, "settled: nothing to read back");
    let (next, created) = enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?);
    assert!(created && next != first, "the retired label is skipped and the next queued");
    assert_eq!(app.summary()?.operations, 2);
    Ok(())
}

// reward-v1 coordinator-row bundles against the real journal: the held
// target, a target switch, a missed tempo, rows the snapshot never moves and
// rollbacks.

pub(crate) mod rv1 {
    use super::*;
    use slop_ninja_protocol::fixtures_reward_v1::{self as fx, Spec, bp};
    use slop_ninja_protocol::{
        CalculationTargetV2, CohortManifestV2, CoordinatorRow, CoordinatorRowEntry, Digest, PublishedIndex, Record,
        RewardRule, RowSource, ScorePackageV2,
    };

    pub const RV1_NETUID: u16 = 4242;

    pub struct Cohort {
        pub manifest: CohortManifestV2,
        pub package: ScorePackageV2,
        pub target: CalculationTargetV2,
        pub closure: slop_ninja_protocol::submission::CohortClosure,
        pub index: PublishedIndex,
        pub authorization: Vec<u8>,
    }

    /// A fixture cohort with its target, closure and authorization.
    pub fn bound(
        manifest: CohortManifestV2,
        package: ScorePackageV2,
        index: PublishedIndex,
        predecessor: Digest,
        specs: &[Spec],
    ) -> Cohort {
        let target = CalculationTargetV2::for_package(&package, Some(predecessor)).unwrap();
        let closure = fx::closure(&manifest, &package, Some(&target), specs).unwrap();
        let authorization = fx::signed(
            fx::authorization(&manifest, &package, &target, 1, manifest.opened_epoch + fx::PUBLISHED_TEMPO).unwrap(),
        )
        .unwrap()
        .to_bytes()
        .unwrap();
        Cohort { manifest, package, target, closure, index, authorization }
    }

    /// The coordinator's signed row of `c` as its own snapshot `snapshot`
    /// (with the fixture owner hotkey) maps it: what the coordinator commits
    /// and validators copy.
    pub fn row(c: &Cohort, snapshot: &RegistrationSnapshot) -> Vec<u8> {
        row_with(c, &c.authorization, snapshot)
    }

    /// [`row`] committed under `authorization` (a re-authorization of `c`'s
    /// target).
    pub fn row_with(c: &Cohort, authorization: &[u8], snapshot: &RegistrationSnapshot) -> Vec<u8> {
        let auth = slop_ninja_protocol::Signed::<slop_ninja_protocol::RewardIntentAuthorization>::from_bytes(authorization)
            .unwrap()
            .record;
        row_at(c, authorization, snapshot, c.closure.closed_epoch.max(auth.issued_epoch))
    }

    /// [`row_with`] for the coordinator's commitment at `epoch` (a record
    /// it re-signed later under the same authorization).
    pub fn row_at(c: &Cohort, authorization: &[u8], snapshot: &RegistrationSnapshot, epoch: u64) -> Vec<u8> {
        let auth = slop_ninja_protocol::Signed::<slop_ninja_protocol::RewardIntentAuthorization>::from_bytes(authorization)
            .unwrap()
            .record;
        let v2 = crate::package_v2::snapshot(snapshot, fx::owner_hotkey().0).unwrap();
        let derived = slop_ninja_protocol::reward_v1::derive_row(&c.target, Some((&c.package, &c.index)), &v2).unwrap();
        let entries = derived
            .entries
            .iter()
            .map(|e| {
                let n = v2.neurons.iter().find(|n| n.uid == e.uid).unwrap();
                CoordinatorRowEntry { uid: e.uid, hotkey: n.hotkey, registration_block: n.registration_block, weight: e.weight }
            })
            .collect();
        let row = CoordinatorRow {
            chain: c.manifest.chain.clone(),
            reward_rule: RewardRule::RewardV1,
            source: RowSource::Cohort {
                cohort: auth.cohort,
                cohort_label: c.manifest.cohort_label.clone(),
                cohort_manifest: auth.cohort_manifest,
            },
            target: auth.target,
            activated_epoch: Some(c.closure.closed_epoch),
            authorization: auth.digest().unwrap(),
            issued_epoch: auth.issued_epoch,
            epoch,
            registration_block: v2.block,
            owner_hotkey: v2.owner_hotkey,
            entries,
        };
        fx::signed(row).unwrap().to_bytes().unwrap()
    }

    /// The coordinator-row bundle of `c` as the coordinator's `snapshot`
    /// maps it.
    pub fn bundle(c: &Cohort, snapshot: &RegistrationSnapshot) -> Vec<u8> {
        serde_json::to_vec(&crate::package_v2::row_bundle("cohort", &c.authorization, None, &row(c, snapshot))).unwrap()
    }

    /// The launch burn row's bundle, every unit to the owner's UID 2.
    pub fn launch(issued_epoch: u64) -> Vec<u8> {
        let auth = slop_ninja_protocol::LaunchAuthorization::new(&fx::chain(), 1, issued_epoch).unwrap();
        let row = CoordinatorRow {
            chain: fx::chain(),
            reward_rule: RewardRule::RewardV1,
            source: RowSource::LaunchBurn,
            target: auth.target,
            activated_epoch: None,
            authorization: auth.digest().unwrap(),
            issued_epoch,
            epoch: issued_epoch,
            registration_block: 100,
            owner_hotkey: fx::owner_hotkey(),
            entries: vec![CoordinatorRowEntry { uid: 2, hotkey: fx::owner_hotkey(), registration_block: 0, weight: u16::MAX }],
        };
        let auth = fx::signed(auth).unwrap().to_bytes().unwrap();
        let row = fx::signed(row).unwrap().to_bytes().unwrap();
        serde_json::to_vec(&crate::package_v2::row_bundle("launch", &auth, None, &row)).unwrap()
    }

    /// The computed cohort (target T1, activating at epoch 80) and its
    /// successor (T2, activating at epoch 100).
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

    /// A first cohort with one credited entrant `tag` and no predecessor:
    /// its row pays only that entrant's UID.
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

/// Two successive reward-v1 targets, a missed tempo, a registration change
/// and a restart: retained work that never landed is retired (journaled),
/// never left to wedge the queue; the coordinator's signed row is committed
/// whatever this validator's snapshot says; and the held target refuses
/// rollbacks.
#[test]
fn reward_v1_targets_switch_through_retained_work_and_refuse_rollbacks() -> Result<()> {
    let root = scratch()?;
    let (first, second, specs) = rv1::cohorts();
    let mut app = rv1_app(root.path())?;
    let before = rv1::snapshot(&specs);
    app.snapshot = before.clone();
    // T1 at its activation epoch.
    app.bundle = rv1::bundle(&first, &before);
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
    // entrant re-registered (a new registration), so its units burn through
    // the owner's UID (the copy rule, review fix round 1 finding 8). The
    // retained epoch-81 operation's row is immutable, so it is retired and
    // the re-derived row takes the next label.
    let credited = first.package.entrants.iter().find(|e| e.units > 0).expect("a credited entrant").identity.hotkey;
    let mut changed = before.clone();
    for n in &mut changed.neurons {
        if n.hotkey == credited {
            n.registration_block += 1;
        }
    }
    let before_entries = match block_on(apply(&mut app, &head(), Mode::DryRun, &Faults::none()))? {
        AppOutcome::DryRun { entries, .. } => entries,
        other => panic!("dry run expected, got {other:?}"),
    };
    app.snapshot = changed;
    let (op82, created) = enqueued(&block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))?);
    assert!(created && op82 != op81);
    assert_eq!(retired_reason(&app, &op81).as_deref(), Some("row_rederived"));
    let after_entries = match block_on(apply(&mut app, &head(), Mode::DryRun, &Faults::none()))? {
        AppOutcome::DryRun { entries, .. } => entries,
        other => panic!("dry run expected, got {other:?}"),
    };
    assert_ne!(before_entries, after_entries, "the registration change burned the entrant's units");
    app.snapshot = before.clone();
    // The successor target at its activation: the T1 work is retired and the
    // held target moves to T2.
    app.bundle = rv1::bundle(&second, &before);
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
    for (bundle, what) in [(rv1::bundle(&first, &before), "replaced target"), (rv1::launch(95), "launch bundle")] {
        app.bundle = bundle;
        match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
            AppOutcome::Refused { code, .. } => assert_eq!(code, "bundle_unverified", "{what}"),
            other => panic!("{what}: expected a refusal, got {other:?}"),
        }
    }
    assert_eq!(app.summary()?.operations, ops);
    // A restart on the held target joins its retained operation.
    app.bundle = rv1::bundle(&second, &before);
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
    app.bundle = rv1::bundle(&first, &app.snapshot);
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
    app.bundle = rv1::bundle(&first, &app.snapshot);
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
    app.bundle = rv1::bundle(&first, &app.snapshot);
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
    app.bundle = rv1::bundle(&single, &app.snapshot);
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

/// The copy model: the row committed is the coordinator's signed row. The
/// one change this validator's own snapshot makes is the score-free
/// registration rule (review fix round 1, finding 8): an entry whose
/// registration the snapshot no longer holds (re-registered, or the UID now
/// held by another hotkey) burns through the owner's UID instead of paying
/// its new holder, and with no owner UID the tempo is deferred. Nothing
/// else moves a UID or a weight.
#[test]
fn the_committed_row_is_the_coordinators_signed_row_with_moved_registrations_burned() -> Result<()> {
    let (first, _, specs) = rv1::cohorts();
    let coordinator = rv1::snapshot(&specs);
    let bundle = rv1::bundle(&first, &coordinator);
    let held = slop_ninja_protocol::ActiveTarget::launch(&slop_ninja_protocol::fixtures_reward_v1::chain())?;
    let keys = slop_ninja_protocol::fixtures::trusted_keys();
    let a = std::sync::Arc::new(crate::package_v2::verify(&bundle, &keys, &TESTNET_GENESIS.0, rv1::RV1_NETUID, &held).unwrap());
    let signed: Vec<(u16, u16)> = a.verified.row.entries.iter().map(|e| (e.uid, e.weight)).collect();
    let epoch = a.verified.active.activated_epoch().unwrap();
    let owner = slop_ninja_protocol::fixtures_reward_v1::owner_hotkey().0;
    let owner_uid = coordinator.neurons.iter().find(|n| n.hotkey.0 == owner).unwrap().uid;
    assert!(signed.len() >= 2, "the fixture row pays entrants besides the burn");
    // The coordinator's own snapshot: exactly the signed entries.
    let snap = crate::package_v2::snapshot(&coordinator, owner).unwrap();
    let d = crate::package_v2::derive(a.clone(), &snap, validator2().hotkey.0, epoch).unwrap();
    assert_eq!(d.entries(), signed);
    // Every entrant re-registered or replaced by another hotkey: their
    // units burn through the owner's UID; none is paid to a new holder.
    let mut moved = coordinator.clone();
    for n in &mut moved.neurons {
        if n.hotkey.0 != owner {
            n.registration_block += 1;
            n.hotkey.0[31] ^= 0x5a;
        }
    }
    let snap = crate::package_v2::snapshot(&moved, owner).unwrap();
    let d = crate::package_v2::derive(a.clone(), &snap, validator2().hotkey.0, epoch).unwrap();
    assert_eq!(d.entries(), vec![(owner_uid, u16::MAX)]);
    assert!(!a.verified.moved_uids(&snap).is_empty(), "the moved UIDs are reported for the logs");
    // The owner hotkey holds no UID: units must burn, so the tempo defers.
    let snap = crate::package_v2::snapshot(&moved, [0x99; 32]).unwrap();
    let err = crate::package_v2::derive(a.clone(), &snap, validator2().hotkey.0, epoch).unwrap_err();
    assert_eq!(err.code, "row_deferred");
    assert!(crate::app::is_deferral(err.code));
    // A tampered row (one weight moved between UIDs) is refused before any
    // row exists.
    let mut b: crate::package_v2::RowBundle = serde_json::from_slice(&bundle)?;
    let mut row = slop_ninja_protocol::Signed::<slop_ninja_protocol::CoordinatorRow>::from_bytes(&hex::decode(&b.row)?).unwrap();
    if row.record.entries.len() >= 2 {
        row.record.entries[0].weight += 1;
        row.record.entries[1].weight -= 1;
        b.row = hex::encode(row.to_bytes().unwrap());
        let err = crate::package_v2::verify(&serde_json::to_vec(&b)?, &keys, &TESTNET_GENESIS.0, rv1::RV1_NETUID, &held).unwrap_err();
        assert_eq!(err.code, "bundle_unverified");
    }
    Ok(())
}

/// Review fix round 1 (findings 2, 7): a validator upgraded with a v1 or
/// v3 bundle still installed defers (`bundle_schema_retired`, exit 75 for
/// a one-shot run, a wait for the fetcher in a continuous one), never
/// halts, and commits nothing new; once a coordinator-row bundle is
/// installed the next pass queues its row.
#[test]
fn an_upgrade_over_a_retired_bundle_defers_then_commits_once_a_row_bundle_is_installed() -> Result<()> {
    let root = scratch()?;
    let config = config(root.path())?;
    init_journal(&config, identity())?;
    let mut app = TestApp::open(&config, &bundle())?;
    app.epoch = 60;
    for schema in [crate::package_v2::BUNDLE_SCHEMA_V2, crate::package_v2::BUNDLE_SCHEMA_V3] {
        app.bundle = serde_json::to_vec(&serde_json::json!({"schema": schema, "shortcut": "T12"}))?;
        match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
            AppOutcome::Deferred { code, .. } => assert_eq!(code, "bundle_schema_retired", "{schema}"),
            other => panic!("expected a deferral, got {other:?}"),
        }
        assert_eq!(app.summary()?.operations, 0, "nothing new is queued");
    }
    assert!(crate::app::is_deferral("bundle_schema_retired"));
    app.bundle = launch_bundle(10);
    match block_on(apply(&mut app, &head(), Mode::Dispatch, &Faults::none()))? {
        AppOutcome::Enqueued { .. } => {}
        other => panic!("expected the burn row queued, got {other:?}"),
    }
    assert_eq!(app.summary()?.operations, 1);
    Ok(())
}

/// Bundles of cohort records and receipts (schemas v1 and v3 of
/// `slop_ninja.reward_v1_bundle`), which validators recomputed, are refused
/// by schema: validators no longer receive them.
#[test]
fn bundles_of_cohort_records_and_receipts_are_retired() {
    let held = slop_ninja_protocol::ActiveTarget::launch(&slop_ninja_protocol::fixtures_reward_v1::chain()).unwrap();
    let keys = slop_ninja_protocol::fixtures::trusted_keys();
    for schema in [crate::package_v2::BUNDLE_SCHEMA_V2, crate::package_v2::BUNDLE_SCHEMA_V3] {
        let b = serde_json::json!({"schema": schema, "shortcut": "T12", "authorization": {"kind": "launch", "authorization": ""}});
        let bytes = serde_json::to_vec(&b).unwrap();
        assert!(crate::package_v2::is_v2(&bytes));
        let err = crate::package_v2::verify(&bytes, &keys, &TESTNET_GENESIS.0, rv1::RV1_NETUID, &held).unwrap_err();
        assert_eq!(err.code, "bundle_schema_retired", "{schema}");
    }
    let mut row: serde_json::Value = serde_json::from_slice(&rv1::launch(10)).unwrap();
    row["extra"] = serde_json::json!(1);
    let err = crate::package_v2::verify(&serde_json::to_vec(&row).unwrap(), &keys, &TESTNET_GENESIS.0, rv1::RV1_NETUID, &held).unwrap_err();
    assert_eq!(err.code, "bundle_malformed");
}
