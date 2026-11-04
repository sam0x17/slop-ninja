//! Verifies a cohort package from its stored receipts-bucket objects, an
//! operator check of the coordinator's own output. It writes no validator
//! bundle: validators commit the coordinator's signed row and refuse the T12
//! fixture package (`bundle_schema_retired`).
//!
//! eval_verify DIR PACKAGE_PUBLIC_KEY_HEX RECEIPT_PUBLIC_KEY_HEX [--snapshot UID:HOTKEY_HEX:REG_BLOCK ...]
//!
//! DIR holds the cohort's receipts-bucket objects at their key suffixes:
//! `manifest.signed`, `package.signed`, `target.signed`, `receipt-index.signed`,
//! `chunk-map.json` and every chunk object under `receipts/`. Every signed record
//! is authenticated before it is decoded; the package is bound to the manifest,
//! the target to the package and the index to the package root and count; every
//! chunk object must match the chunk map's digest and length, and every receipt
//! cut from a chunk must match its index entry, covering every entry once. With
//! `--snapshot`, the canonical row is derived. Prints a JSON report; no text.
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use slop_ninja_coordinator::eval::receipts;
use slop_ninja_protocol::{
    AccountId, CalculationTarget, CohortManifest, NeuronIdentity, OutcomeClass, ReceiptIndex, Record,
    RegistrationSnapshot, ScorePackage, Signed, TrustedKeys, reward,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn key(hex: &str) -> [u8; 32] {
    hex::decode(hex.trim_start_matches("0x")).expect("key hex").try_into().expect("32-byte key")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    assert!(args.len() >= 3, "usage: eval_verify DIR PACKAGE_KEY RECEIPT_KEY [--snapshot UID:HOTKEY:REG ...]");
    let dir = PathBuf::from(&args[0]);
    // The authorization role is unused here; any distinct key fills it.
    let keys = TrustedKeys::new(key(&args[1]), key(&args[2]), [0xa5; 32]).expect("distinct trusted keys");
    let read = |name: &str| std::fs::read(dir.join(name)).unwrap_or_else(|_| panic!("read {name}"));
    let manifest = Signed::<CohortManifest>::open(&read("manifest.signed"), &keys).expect("manifest signature");
    let package = Signed::<ScorePackage>::open(&read("package.signed"), &keys).expect("package signature");
    let target = Signed::<CalculationTarget>::open(&read("target.signed"), &keys).expect("target signature");
    let index = Signed::<ReceiptIndex>::open(&read("receipt-index.signed"), &keys).expect("index signature");
    package.check_manifest(&manifest).expect("package binds the manifest");
    target.check_package(&package).expect("target binds the package");
    package.check_receipts(&index).expect("index is the package root and count");

    let map: Value = serde_json::from_slice(&read("chunk-map.json")).expect("chunk map");
    assert_eq!(map["receipt_root"], index.digest().unwrap().to_string());
    let prefix = format!("cohorts/{}/", manifest.cohort_label);
    let mut chunks = Vec::new();
    let mut bytes_total = 0u64;
    for o in map["objects"].as_array().expect("objects") {
        let k = o["key"].as_str().unwrap();
        let rel = k.strip_prefix(&prefix).expect("object under the cohort prefix");
        let b = read(rel);
        assert_eq!(hex::encode(Sha256::digest(&b)), o["sha256"].as_str().unwrap(), "{rel} digest");
        assert_eq!(b.len() as u64, o["bytes"].as_u64().unwrap(), "{rel} length");
        bytes_total += b.len() as u64;
        chunks.push(b);
    }
    let verified = receipts::verify_chunks(&index, &chunks).expect("every receipt matches the index once");
    let mut by_artifact: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for r in &verified {
        assert!(
            r.artifact == manifest.baseline_artifact || manifest.artifacts.binary_search(&r.artifact).is_ok(),
            "receipt for an unlisted artifact"
        );
        let class = match r.outcome {
            OutcomeClass::Prediction => "prediction",
            OutcomeClass::ModelError => "model_error",
            OutcomeClass::InvalidOutput => "invalid_output",
            OutcomeClass::ParseFailure => "parse_failure",
            OutcomeClass::InfrastructureError => "infrastructure_error",
        };
        *by_artifact.entry(r.artifact.to_string()).or_default().entry(class.into()).or_default() += 1;
    }


    let mut row = Value::Null;
    let snap: Vec<NeuronIdentity> = args[3..]
        .iter()
        .filter(|a| a.as_str() != "--snapshot")
        .map(|a| {
            let p: Vec<&str> = a.split(':').collect();
            NeuronIdentity { uid: p[0].parse().unwrap(), hotkey: AccountId(key(p[1])), registration_block: p[2].parse().unwrap() }
        })
        .collect();
    if !snap.is_empty() {
        let mut neurons = snap;
        neurons.sort_by_key(|n| n.uid);
        let s = RegistrationSnapshot {
            chain: package.chain.clone(),
            block: 0,
            block_hash: slop_ninja_protocol::Digest([0; 32]),
            neurons,
        };
        let r = reward::derive_row(&target, &package, &s).expect("row derivation");
        row = json!({"entries": r.entries.iter().map(|e| json!([e.uid, e.weight])).collect::<Vec<_>>(),
            "row_digest": r.digest().unwrap().to_string()});
    }
    let report = json!({
        "verified": true,
        "cohort": manifest.cohort, "label": manifest.cohort_label,
        "manifest": manifest.digest().unwrap().to_string(),
        "package": package.digest().unwrap().to_string(),
        "target": target.digest().unwrap().to_string(),
        "target_predecessor": target.predecessor.map(|d| d.to_string()),
        "receipt_root": package.receipt_root.to_string(),
        "receipt_count": package.receipt_count,
        "task_count": manifest.task_count,
        "chunk_objects": chunks.len(), "chunk_bytes": bytes_total,
        "receipts_by_artifact": by_artifact,
        "baseline_utility": package.baseline.get().to_string(),
        "scores": package.scores.iter().map(|s| json!({"uid": s.identity.uid, "artifact": s.artifact.to_string(),
            "utility": s.score.get().to_string(), "assigned": s.assigned, "succeeded": s.succeeded, "failed": s.failed})).collect::<Vec<_>>(),
        "row": row,
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
