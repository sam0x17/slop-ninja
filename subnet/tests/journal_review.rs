//! `slop-ninja-journal review-failure`: the offline operator path for a
//! finalized failed attempt. The journal is written in rtao's own schema; the
//! tool is run as a process, as an operator runs it with the validator
//! stopped.

use std::{path::Path, process::Command};

use blake2::{Blake2b, Digest as _, digest::consts::U32};
use rtao_client::journal::OperationJournal;
use rtao_primitives::{AccountId, H256, NetUid};
use serde_json::{Value, json};
use slop_ninja_subnet::{ChainIdentity, JournalControls, ReviewKind};

const FAILED: &str = "slopninja:weights:v1:synthetic-failed";
const UNRESOLVED: &str = "slopninja:weights:v1:synthetic-unresolved";

fn scratch() -> tempfile::TempDir {
    let root = std::env::var_os("SLOP_NINJA_TEST_TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    tempfile::Builder::new()
        .prefix("journal-review-")
        .tempdir_in(root)
        .expect("scratch directory")
}

fn attempt(seed: u8, outcome: Value) -> Value {
    let bytes = vec![seed, 2, 3];
    let hash = H256(Blake2b::<U32>::digest(&bytes).into());
    json!({
        "id": 0, "role": "Ordinary", "material_generation": 0, "hash": hash, "bytes": bytes,
        "nonce": 7, "spec_version": 486, "transaction_version": 1, "start": 1000, "expiry": 1128,
        "era_anchor": H256([0x5b; 32]), "era": null, "metadata_hash": null, "material": null,
        "next": 1000, "finalized": null, "outcome": outcome, "evidence_conflicts": [],
    })
}

fn record(id: &str, identity: &ChainIdentity, attempt: Value) -> Value {
    json!({
        "mode": "Ordinary",
        "shield_pairs": [],
        "intent": {
            "id": id, "correlation_id": null, "genesis": identity.genesis, "account": identity.hotkey,
            "scope": "weights:synthetic", "effects": { "netuid": identity.netuid, "mechanism": 0 },
            "obligation": false, "semantic_policy": "slopninja.test-contract.v1",
        },
        "material": null,
        "material_generation": 0,
        "attempts": [attempt],
        "incompatibility": null,
        "scheduler_handed_off": true,
    })
}

/// A Slop Ninja journal holding one handed-back operation whose commit
/// finalized as a dispatch failure, and one whose attempt is unresolved.
fn write_journal(path: &Path) {
    let identity = ChainIdentity {
        genesis: H256([1; 32]),
        netuid: NetUid(7),
        hotkey: AccountId([2; 32]),
    };
    let journal = OperationJournal::open(path).expect("journal");
    journal
        .update(|image| {
            image.extensions.insert(
                "slopninja.native-weights.v1".into(),
                json!({ "schema": 1, "chain": identity, "pause_epoch": 1 }),
            );
            image.control.submissions_paused = true;
            let failed = json!({ "Finalized": { "block": H256([9; 32]), "number": 1200, "success": false } });
            for (id, attempt) in [(FAILED, attempt(1, failed)), (UNRESOLVED, attempt(2, json!("Unresolved")))] {
                image.operations.insert(
                    id.into(),
                    serde_json::from_value(record(id, &identity, attempt)).expect("record matches rtao's schema"),
                );
            }
            Ok(())
        })
        .expect("seed journal");
}

fn run(path: &Path, args: &[&str]) -> (bool, Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_slop-ninja-journal"))
        .arg("--journal")
        .arg(path)
        .args(args)
        .output()
        .expect("run slop-ninja-journal");
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let value = if stdout.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&stdout).expect("one JSON document")
    };
    (out.status.success(), value, String::from_utf8(out.stderr).expect("utf-8 stderr"))
}

#[test]
fn review_failure_records_a_permanent_marker_and_refuses_unresolved_attempts() {
    let root = scratch();
    let path = root.path().join("journal.json");
    write_journal(&path);

    let (ok, inspect, _) = run(&path, &["inspect"]);
    assert!(ok);
    // Recovery has not read its receipt: it awaits decoding (recovery waits
    // without halting), and an operator may still review it.
    assert_eq!(inspect["finalized_failures"], 0);
    assert_eq!(inspect["failures_awaiting_decode"], 1);
    assert_eq!(inspect["unresolved_attempts"], 1);
    assert_eq!(inspect["review_required"], json!([]));
    assert_eq!(
        inspect["awaiting_decode"],
        json!([{ "operation": FAILED, "attempt": 0, "block_number": 1200, "error": null }])
    );

    // An unresolved attempt, an unknown operation and an unknown attempt
    // are refused with a fixed reason; nothing is written.
    let before = OperationJournal::open(&path).unwrap().snapshot().unwrap().revision;
    for (args, code) in [
        (["review-failure", "--operation", UNRESOLVED, "--attempt", "0"], "attempt_unresolved"),
        (["review-failure", "--operation", "unknown", "--attempt", "0"], "unknown_operation"),
        (["review-failure", "--operation", FAILED, "--attempt", "4"], "unknown_attempt"),
    ] {
        let (ok, stdout, stderr) = run(&path, &args);
        assert!(!ok, "{args:?}");
        assert_eq!(stdout, Value::Null);
        assert!(stderr.contains(code), "{stderr}");
    }
    assert_eq!(OperationJournal::open(&path).unwrap().snapshot().unwrap().revision, before);

    let (ok, out, _) = run(&path, &["review-failure", "--operation", FAILED, "--attempt", "0"]);
    assert!(ok);
    assert_eq!(out["review"]["kind"], "operator_reviewed");
    assert_eq!(out["review"]["block_number"], 1200);
    assert_eq!(out["summary"]["finalized_failures"], 0);
    assert_eq!(out["summary"]["failures_awaiting_decode"], 0);
    assert_eq!(out["summary"]["awaiting_decode"], json!([]));
    assert_eq!(out["summary"]["reviewed_failures"], 1);
    assert_eq!(out["summary"]["review_required"], json!([]));

    // Permanent: a reopen keeps it, a repeat changes nothing, and the
    // handed-back operation is retired so the next tempo follows.
    let controls = JournalControls::open_existing(&path).unwrap();
    assert_eq!(controls.inspect().unwrap().reviewed_failures, 1);
    assert_eq!(controls.retired(FAILED).unwrap().as_deref(), Some("finalized_failure_reviewed"));
    drop(controls);
    let revision = OperationJournal::open(&path).unwrap().snapshot().unwrap().revision;
    let (ok, again, _) = run(&path, &["review-failure", "--operation", FAILED, "--attempt", "0"]);
    assert!(ok);
    assert_eq!(again["review"], out["review"]);
    assert_eq!(OperationJournal::open(&path).unwrap().snapshot().unwrap().revision, revision);
    let review: slop_ninja_subnet::FailureReview = serde_json::from_value(again["review"].clone()).unwrap();
    assert_eq!(review.kind, ReviewKind::OperatorReviewed);
    // The unresolved attempt still holds recovery.
    assert_eq!(again["summary"]["unresolved_attempts"], 1);
}
