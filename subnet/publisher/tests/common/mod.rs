//! A synthetic closed `reward-v1` cohort laid out exactly as the coordinator
//! stores it, with private objects beside it that must never be published.
#![allow(dead_code)]

use serde_json::json;
use slop_ninja_protocol::fixtures::{package_key, receipt_key, trusted_keys};
use slop_ninja_protocol::fixtures_reward_v1 as fx;
use slop_ninja_protocol::submission::SubmissionUpload;
use slop_ninja_protocol::{
    AccountId, AttemptReceipt, CalculationTargetV2, Canonical, Digest, OutcomeClass, ReceiptIndex,
    ReceiptIndexEntry, Record, domain_hash, sign,
};
use slop_ninja_publisher::arena::Budget;
use slop_ninja_publisher::config::{self, Config};
use slop_ninja_publisher::index::CohortIndex;
use slop_ninja_publisher::publish::{Buckets, Memo, NoLease, Report, Run};
use slop_ninja_publisher::store::{MemStore, block_on};

pub const PRIVATE_TEXT: &str =
    "PRIVATE-DOCUMENT-TEXT: a private passage that must never leave the eval bucket";
pub const PRIVATE_VECTOR: &[u8] = b"PRIVATE-PREDICTION-VECTOR";
pub const ARTIFACT_TEMPLATE: &str = "admission/objects/{sha256}.bin";
pub const ADMISSION_TEMPLATE: &str = "admission/cohorts/{label}/{hotkey}.upload";

/// The eval-bucket key of an entrant's stored admission record.
pub fn admission_key(label: &str, hotkey: &AccountId) -> String {
    ADMISSION_TEMPLATE
        .replace("{label}", label)
        .replace("{hotkey}", &hex::encode(hotkey.0))
}

pub struct Built {
    pub c: fx::Computed,
    pub label: String,
    pub receipts: Vec<Vec<u8>>,
    pub index: ReceiptIndex,
}

/// The computed fixture cohort with `receipts` attempt receipts (at least
/// the package's assignments; 0 keeps the fixture's count) bound into its
/// package, and its target and closure rebuilt over that package.
pub fn build(receipts: u64) -> Built {
    build_cohort(fx::computed().expect("computed fixture"), receipts)
}

/// [`build`] for cohort number `cohort` (the fixture's is 3): its label and
/// opening epoch follow the number, so several closed cohorts can be stored
/// side by side in order.
pub fn build_at(cohort: u64) -> Built {
    build_cohort(fx::computed_at(cohort).expect("computed fixture"), 0)
}

/// [`build_at`] with reference `R_0` and no predecessor: a cohort that can
/// start the record.
pub fn build_r0_at(cohort: u64) -> Built {
    build_cohort(fx::computed_r0_at(cohort).expect("computed fixture"), 0)
}

fn build_cohort(mut c: fx::Computed, receipts: u64) -> Built {
    let n = if receipts == 0 {
        c.package.receipt_count
    } else {
        receipts
    };
    let entrants: Vec<_> = c.package.entrants.iter().map(|e| e.artifact).collect();
    let mut bytes = Vec::with_capacity(n as usize);
    let mut entries = Vec::with_capacity(n as usize);
    for i in 0..n {
        let tag = i.to_le_bytes();
        let r = AttemptReceipt {
            attempt: domain_hash("publisher-test/attempt", &tag),
            assignment: domain_hash("publisher-test/assignment", &tag),
            artifact: entrants[(i as usize) % entrants.len()],
            number: 1,
            outcome: if i % 17 == 0 {
                OutcomeClass::ModelError
            } else {
                OutcomeClass::Prediction
            },
            result_commitment: domain_hash("publisher-test/result", &tag),
        };
        entries.push(ReceiptIndexEntry::for_receipt(&r).unwrap());
        bytes.push(r.to_bytes().unwrap());
    }
    entries.sort_by_key(|a| a.attempt);
    let index = ReceiptIndex {
        cohort_manifest: c.package.cohort_manifest,
        entries,
    };
    c.package.receipt_root = index.digest().unwrap();
    c.package.receipt_count = n;
    let launch = CalculationTargetV2::launch_burn(fx::chain())
        .digest()
        .unwrap();
    c.target = CalculationTargetV2::for_package(&c.package, Some(launch)).unwrap();
    c.closure = fx::closure(&c.manifest, &c.package, Some(&c.target), &c.specs).unwrap();
    let label = c.manifest.cohort_label.clone();
    Built {
        c,
        label,
        receipts: bytes,
        index,
    }
}

pub fn dir(label: &str) -> String {
    format!("cohorts/{label}")
}

/// Stores the cohort in the private receipts bucket, its artifact objects in
/// the eval bucket, and private material a careless copy would leak.
pub fn store(b: &Built, receipts: &MemStore, eval: &MemStore) {
    let c = &b.c;
    let d = dir(&b.label);
    let put = |rel: &str, bytes: Vec<u8>| receipts.insert(&format!("{d}/{rel}"), bytes);
    put(
        "manifest.signed",
        sign(c.manifest.clone(), &package_key())
            .unwrap()
            .to_bytes()
            .unwrap(),
    );
    put(
        "package.signed",
        sign(c.package.clone(), &package_key())
            .unwrap()
            .to_bytes()
            .unwrap(),
    );
    put(
        "target.signed",
        sign(c.target.clone(), &package_key())
            .unwrap()
            .to_bytes()
            .unwrap(),
    );
    put(
        "receipt-index.signed",
        sign(b.index.clone(), &receipt_key())
            .unwrap()
            .to_bytes()
            .unwrap(),
    );
    // Bundles: one per entrant job plus the reference job.
    let jobs = c.package.entrants.len() + 1;
    let mut objects = Vec::new();
    for j in 0..jobs {
        let mut bundle = Vec::new();
        let mut count = 0u64;
        for (i, r) in b.receipts.iter().enumerate() {
            if i % jobs == j {
                bundle.extend_from_slice(&(r.len() as u32).to_le_bytes());
                bundle.extend_from_slice(r);
                count += 1;
            }
        }
        let key = format!("{d}/receipts/bundle-job-{j:03}.bin");
        objects.push(
            json!({"key": key, "sha256": slop_ninja_protocol::object_sha256(&bundle).hex(),
            "bytes": bundle.len(), "receipts": count}),
        );
        receipts.insert(&key, bundle);
        // The per-chunk objects the bundles were cut from stay private.
        receipts.insert(
            &format!("{d}/receipts/job-{j:03}/chunk-0000-attempt-1.bin"),
            vec![j as u8; 8],
        );
    }
    let chunk_map = json!({"format": "slop_ninja_receipt_chunk_v1", "cohort_manifest": c.package.cohort_manifest.hex(),
        "receipt_root": c.package.receipt_root.hex(), "receipt_count": c.package.receipt_count, "objects": objects});
    put(
        "chunk-map.json",
        serde_json::to_vec_pretty(&chunk_map).unwrap(),
    );
    put(
        "published-contents.bin",
        c.index.contents.to_bytes().unwrap(),
    );
    put(
        "fingerprint-index.bin",
        c.index.fingerprints.to_bytes().unwrap(),
    );
    put(
        "closure.signed",
        sign(c.closure.clone(), &package_key())
            .unwrap()
            .to_bytes()
            .unwrap(),
    );
    let fps = fx::closure_fingerprints(c);
    let after = c.index.after_closure(&c.package, fps.clone()).unwrap();
    put(
        "published-contents-after.bin",
        after.contents.to_bytes().unwrap(),
    );
    put(
        "fingerprint-index-after.bin",
        after.fingerprints.to_bytes().unwrap(),
    );
    let fps_json: Vec<_> = fps
        .iter()
        .map(|f| json!({"hotkey": hex::encode(f.hotkey.0), "version": f.version, "fingerprint": f.fingerprint.hex()}))
        .collect();
    put(
        "closure-fingerprints.json",
        serde_json::to_vec_pretty(&json!({"fingerprints": fps_json})).unwrap(),
    );
    put(
        "components.json",
        serde_json::to_vec_pretty(&components(b)).unwrap(),
    );
    put(
        "keys.json",
        br#"{"instance":"main","package_public_key":"00"}"#.to_vec(),
    );
    // Private material in the same bucket and beside it.
    put(
        "batch/shard-0000.jsonl",
        format!("{{\"task\":0,\"kind\":\"mixed\",\"text\":\"{PRIVATE_TEXT}\"}}\n").into_bytes(),
    );
    put(
        "batch/index.json",
        br#"{"documents":[{"task":0,"kind":"mixed","share_ppb":400000000}]}"#.to_vec(),
    );
    put(
        "results/job-000/chunk-0000-attempt-1.json",
        br#"{"predictions":[0.4242424242,0.1717171717]}"#.to_vec(),
    );
    put(
        "summaries/job-000.json",
        br#"{"involvement":[0.4242424242,0.1717171717],"labels":[1,0]}"#.to_vec(),
    );
    receipts.insert(
        &format!("probes/v1/fp/{}/00.bin", b.label),
        PRIVATE_VECTOR.to_vec(),
    );
    receipts.insert("pools/p/manifest.json", PRIVATE_TEXT.as_bytes().to_vec());
    // A cohort that never closed, and a load cohort.
    receipts.insert("cohorts/testnet-c99/manifest.signed", vec![1, 2, 3]);
    receipts.insert("cohorts/load-x/package.signed", vec![1, 2, 3]);
    for spec in &c.specs {
        let object = fx::artifact_bytes(spec);
        let sha = slop_ninja_protocol::object_sha256(&object);
        // Every fixture entrant was admitted through the admission route: its
        // upload envelope is stored beside the object.
        let opening = fx::opening(&c.manifest.chain, c.manifest.cohort, spec);
        let hotkey = opening.hotkey;
        let upload = SubmissionUpload::new(opening, &object);
        eval.insert(
            &admission_key(&b.label, &hotkey),
            upload.to_bytes().unwrap(),
        );
        eval.insert(&ARTIFACT_TEMPLATE.replace("{sha256}", &sha.hex()), object);
    }
    eval.insert(
        "cohorts/x/batch/shard-0000.jsonl",
        PRIVATE_TEXT.as_bytes().to_vec(),
    );
}

/// `components.json` exactly as the coordinator writes it for this package
/// (`subnet/coordinator/src/eval/v3.rs`), its policy document left out (the
/// fixture manifest's gate-policy digest is synthetic).
pub fn components(b: &Built) -> serde_json::Value {
    let p = &b.c.package;
    json!({
        "schema": "slop_ninja_eval_score_components_v2",
        "cohort": p.cohort, "label": b.label, "reward_rule": "reward-v1",
        "cohort_manifest": p.cohort_manifest.hex(),
        "mode": format!("{:?}", p.mode),
        "promoted": p.promoted.map(|h| hex::encode(h.0)),
        "reference_score": p.reference.score.get().to_string(),
        "predecessor_score": p.predecessor.as_ref().map(|x| x.score.get().to_string()),
        "gate_policy": {"operating_ppb": 500000000, "loose_fpr_max_permille": 100,
                        "strict_fpr_max_permille": 50, "share_mae_max_ppb": 300000000,
                        "attribution_min_permille": 0},
        "canary_rule": "testnet_drill_only_v1",
        "fpr_rule": "fpr_at_operating_point_failures_flagged_v1",
        "reuse_rule": "rewrite_source_ledger_v2",
        "policy": null,
        "unexecuted": p.entrants.iter().enumerate()
            .filter(|(_, e)| e.score.is_none()).map(|(i, _)| i).collect::<Vec<_>>(),
        "entrants": p.entrants.iter().map(|e| json!({
            "hotkey": hex::encode(e.identity.hotkey.0), "uid": e.identity.uid,
            "score": e.score.map(|s| s.get().to_string()),
            "gates": e.gate_failures.iter().map(|g| g.reason.clone()).collect::<Vec<_>>(),
            "cluster_head": e.cluster_head.map(|h| hex::encode(h.0)), "units": e.units,
        })).collect::<Vec<_>>(),
        "crash_rule": "crash isolation: a rule text",
        "artifact_rule": "an artifact rule text",
    })
}

/// A digest no fixture names, to satisfy the non-empty guards.
pub fn unrelated() -> Digest {
    domain_hash("publisher-test/unrelated", b"")
}

/// The configuration with the record from epoch `from`, and, with
/// `artifacts`, artifact copies and every guard they require.
pub fn config(from: Option<u64>, artifacts: bool) -> Config {
    let mut c = config::parse(|k| match k {
        "NETWORK" => Some("testnet".into()),
        "SUBNET_NETUID" => Some("601".into()),
        _ => None,
    })
    .unwrap();
    c.chain = fx::chain();
    c.keys = trusted_keys();
    c.publish_from_epoch = from;
    if artifacts {
        c.artifact_source = Some(ARTIFACT_TEMPLATE.to_owned());
        c.admission_record = Some(ADMISSION_TEMPLATE.to_owned());
        c.private_digests.insert(unrelated());
        c.operator_hotkeys.insert(AccountId(unrelated().0));
    }
    c
}

pub struct Buckets3 {
    pub receipts: MemStore,
    pub eval: MemStore,
    pub public: MemStore,
}

/// The label of the seeded first cohort of the record.
pub const SEED_LABEL: &str = "testnet-seed-r0";

/// The buckets with the cohort stored, and a public index whose record
/// already starts at `R_0`: the fixture cohort's reference is `R_1`, and the
/// record must start at a cohort whose reference is `R_0`. The seed entry is
/// the cohort before the fixture's, with no target and no receipts.
pub fn buckets(b: &Built) -> Buckets3 {
    let x = buckets_unseeded(b);
    let chain = fx::chain();
    let mut idx = CohortIndex::empty(&chain);
    let m = &b.c.manifest;
    idx.cohorts.push(slop_ninja_publisher::index::CohortEntry {
        cohort: m.cohort - 1,
        label: SEED_LABEL.into(),
        path: format!("v1/cohorts/{SEED_LABEL}/"),
        opened_epoch: m.opened_epoch - 20,
        admission_closed_epoch: m.opened_epoch - 19,
        published_epoch: m.opened_epoch - 2,
        closed_epoch: m.opened_epoch,
        mode: "no_target".into(),
        promoted: None,
        target: None,
        reference_index: 0,
        manifest: "00".repeat(32),
        package: "00".repeat(32),
        closure: "00".repeat(32),
        entrants: 0,
        receipts: 0,
        receipts_retained: false,
        artifacts_published: 0,
        artifacts_withheld: 0,
        artifacts_pending: 0,
    });
    x.public.insert(
        slop_ninja_publisher::layout::COHORT_INDEX,
        serde_json::to_vec(&idx).unwrap(),
    );
    x
}

/// The buckets with the cohort stored and an empty public bucket.
pub fn buckets_unseeded(b: &Built) -> Buckets3 {
    let x = Buckets3 {
        receipts: MemStore::new(),
        eval: MemStore::new(),
        public: MemStore::new(),
    };
    store(b, &x.receipts, &x.eval);
    x
}

/// The fixture cohort's entry in an index.
pub fn entry_of<'a>(
    idx: &'a CohortIndex,
    label: &str,
) -> &'a slop_ninja_publisher::index::CohortEntry {
    idx.cohorts.iter().find(|c| c.label == label).unwrap()
}

pub fn run_with(
    cfg: &Config,
    x: &Buckets3,
    memo: &mut Memo,
    budget: &Budget,
) -> (Report, Result<CohortIndex, String>) {
    let elapsed = || 0u64;
    let run = Run {
        buckets: Buckets {
            receipts: &x.receipts,
            eval: &x.eval,
            public: &x.public,
        },
        cfg,
        budget,
        elapsed_ms: &elapsed,
        keepalive: &NoLease,
        stopped: std::cell::Cell::new(false),
    };
    let mut report = Report::default();
    let idx = block_on(run.cohorts(memo, &mut report));
    (report, idx)
}

pub fn run(cfg: &Config, x: &Buckets3, memo: &mut Memo) -> (Report, Result<CohortIndex, String>) {
    run_with(cfg, x, memo, &Budget::worker())
}

pub fn encoded_len<T: Canonical>(value: &T) -> usize {
    let mut e = slop_ninja_protocol::Encoder::new();
    value.encode_to(&mut e);
    e.into_bytes().len()
}

/// Writes the coordinator's `receipts-retired.json` marker for `label`
/// (cohort `cohort`), due after cohort `after`.
pub fn write_marker(x: &Buckets3, label: &str, cohort: u64, after: u64) {
    let marker = json!({
        "schema": slop_ninja_publisher::publish::RETIRED_MARKER_SCHEMA, "label": label,
        "cohort": cohort, "rule": "receipts_after_two_later_completions_v2",
        "due_after_cohort": after,
    });
    x.receipts.insert(
        &format!(
            "cohorts/{label}/{}",
            slop_ninja_publisher::publish::RETIRED_MARKER
        ),
        serde_json::to_vec(&marker).unwrap(),
    );
}
