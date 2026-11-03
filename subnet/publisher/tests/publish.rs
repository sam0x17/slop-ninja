//! End-to-end publisher runs over in-memory buckets: copies verify after the
//! copy, nothing private is published, refusals publish nothing, and
//! artifacts follow their rules.

mod common;

use common::*;
use slop_ninja_protocol::fixtures::trusted_keys;
use slop_ninja_protocol::submission::CohortClosure;
use slop_ninja_protocol::{
    CalculationTargetV2, CohortManifestV2, ROW_TOTAL, ReceiptIndex, ScorePackageV2, Signed,
    object_sha256,
};
use slop_ninja_publisher::arena::Budget;
use slop_ninja_publisher::index::{ArtifactState, Publication};
use slop_ninja_publisher::layout::{self, Cache};
use slop_ninja_publisher::leaderboard::LeaderboardDoc;
use slop_ninja_publisher::publish::{self, Memo, ReferenceDigests};
use slop_ninja_publisher::store::block_on;
use slop_ninja_publisher::verify::{self, Options};

fn public_json<T: for<'d> serde::Deserialize<'d>>(x: &Buckets3, key: &str) -> T {
    serde_json::from_slice(
        &x.public
            .bytes(key)
            .unwrap_or_else(|| panic!("{key} missing")),
    )
    .unwrap()
}

#[test]
fn publishes_a_closed_cohort_and_the_copies_verify() {
    let b = build(0);
    let x = buckets(&b);
    let cfg = config(Some(0), false);
    let (report, idx) = run(&cfg, &x, &mut Memo::default());
    let idx = idx.unwrap();
    assert_eq!(report.published, vec![b.label.clone()]);
    assert!(
        report.refused.is_empty() && report.pending.is_empty(),
        "{report:?}"
    );
    assert_eq!(idx.cohorts.len(), 2, "the seed and the fixture cohort");
    let entry = entry_of(&idx, &b.label);
    assert_eq!(entry.cohort, b.c.manifest.cohort);
    assert_eq!(entry.receipts, b.c.package.receipt_count);
    assert_eq!(entry.closed_epoch, b.c.closure.closed_epoch);
    assert_eq!(idx.active_target.digest, b.c.closure.target.unwrap().hex());

    // Every copied object is byte-identical to the private one, under the
    // documented path mapping, and its digest is the one recorded.
    let publication: Publication = public_json(&x, &layout::publication(&b.label));
    assert!(publication.objects.len() >= 12);
    for o in &publication.objects {
        let public = x
            .public
            .bytes(&format!("v1/cohorts/{}/{}", b.label, o.rel))
            .unwrap();
        let private = x
            .receipts
            .bytes(&format!("cohorts/{}/{}", b.label, o.rel))
            .unwrap();
        assert_eq!(public, private, "{}", o.rel);
        assert_eq!(object_sha256(&public).hex(), o.sha256);
    }
    assert!(
        publication
            .objects
            .iter()
            .any(|o| o.rel == "components.json")
    );
    assert_eq!(publication.receipts_verified, b.c.package.receipt_count);

    // Signatures verify after the copy, independently of the publisher.
    let keys = trusted_keys();
    let get = |rel: &str| {
        x.public
            .bytes(&format!("v1/cohorts/{}/{rel}", b.label))
            .unwrap()
    };
    let manifest = Signed::<CohortManifestV2>::open(&get("manifest.signed"), &keys).unwrap();
    let package = Signed::<ScorePackageV2>::open(&get("package.signed"), &keys).unwrap();
    let target = Signed::<CalculationTargetV2>::open(&get("target.signed"), &keys).unwrap();
    let closure = Signed::<CohortClosure>::open(&get("closure.signed"), &keys).unwrap();
    let index = Signed::<ReceiptIndex>::open(&get("receipt-index.signed"), &keys).unwrap();
    assert_eq!(manifest, b.c.manifest);
    assert_eq!(package, b.c.package);
    assert_eq!(target, b.c.target);
    assert_eq!(closure, b.c.closure);
    package.check_receipts(&index).unwrap();
    closure.check(&manifest, &package, Some(&target)).unwrap();
    // And the publisher's own verifier agrees on the public copies.
    let opts = Options {
        keys: &keys,
        chain: &cfg.chain,
        receipts: true,
    };
    let again = block_on(verify::verify_cohort(
        &x.public,
        &layout::cohort_dir(&b.label),
        &b.label,
        &opts,
        &Budget::worker(),
    ))
    .unwrap();
    assert_eq!(again.receipts, b.c.package.receipt_count);

    // The trust roots are published byte for byte; the site and index exist.
    assert_eq!(
        x.public.bytes(layout::TRUST).unwrap(),
        slop_ninja_publisher::config::TRUST_FILE
    );
    let page = String::from_utf8(x.public.bytes(layout::SITE_INDEX).unwrap()).unwrap();
    assert!(page.contains(&b.label) && page.contains("Brier utility"));
    assert!(x.public.bytes(layout::SITE_STYLE).is_some());

    // A second run is a no-op.
    let puts = x.public.puts.get();
    let (report, _) = run(&cfg, &x, &mut Memo::default());
    assert!(report.published.is_empty() && report.refused.is_empty());
    assert_eq!(x.public.puts.get(), puts, "nothing rewritten");
}

#[test]
fn leaderboard_and_references_come_from_the_verified_outcome() {
    let b = build(0);
    let x = buckets(&b);
    run(&config(Some(0), false), &x, &mut Memo::default())
        .1
        .unwrap();
    let doc: LeaderboardDoc = public_json(&x, layout::LEADERBOARD);
    let p = &b.c.package;
    assert_eq!(doc.entrants.len(), p.entrants.len());
    assert_eq!(doc.mode, slop_ninja_publisher::index::mode_name(p.mode));
    let units: u32 = doc.entrants.iter().map(|e| u32::from(e.units)).sum();
    if p.mode != slop_ninja_protocol::CutoffMode::NoTarget {
        assert_eq!(units, u32::from(ROW_TOTAL));
    }
    // Ranks are 1..=gated in order, gate failures unranked and last.
    let ranked: Vec<u32> = doc.entrants.iter().filter_map(|e| e.rank).collect();
    assert_eq!(ranked, (1..=ranked.len() as u32).collect::<Vec<_>>());
    assert!(
        doc.entrants
            .iter()
            .skip(ranked.len())
            .all(|e| e.rank.is_none())
    );
    for row in &doc.entrants {
        let e = p
            .entrants
            .iter()
            .find(|e| e.identity.uid == row.uid)
            .unwrap();
        assert_eq!(row.units, e.units);
        assert_eq!(
            row.gates,
            e.gate_failures
                .iter()
                .map(|g| g.reason.clone())
                .collect::<Vec<_>>()
        );
        if e.units > 0 {
            assert!(
                row.cluster.is_some() && row.band.is_some() && row.credit.is_some(),
                "{row:?}"
            );
        }
        if !row.gates.is_empty() {
            assert!(row.credit.is_none());
        }
    }
    // The initial reference is never offered as an entrant artifact; with
    // no staged package the pointer says so.
    let current: slop_ninja_publisher::index::ReferencePointer =
        public_json(&x, layout::REFERENCE_CURRENT);
    if p.promoted.is_none() {
        assert!(current.path.is_none() && current.package.is_none() && current.licence.is_none());
        assert_eq!(
            current.withheld.as_deref(),
            Some(slop_ninja_publisher::index::REFERENCE_NOT_STAGED)
        );
    }
    assert_eq!(doc.current_reference, current);
}

/// The cohort's reference `R_k` (the fixture's `R_1`, content tag
/// `0xe0 + k`): its artifact digest and the object with that SHA-256, to stage
/// as a parts manifest.
fn reference_artifact(b: &Built) -> (slop_ninja_protocol::Digest, Vec<u8>) {
    use slop_ninja_protocol::fixtures_reward_v1 as fx;
    let r = &b.c.manifest.references;
    let tag = 0xe0 + r.index as u8;
    let bytes = fx::artifact_object(tag, tag);
    assert_eq!(object_sha256(&bytes), r.current.artifact);
    (r.current.artifact, bytes)
}

/// The published reference pointer (current or previous) that names
/// `artifact`, and whether it is the current one.
fn pointer_of(
    x: &Buckets3,
    artifact: &slop_ninja_protocol::Digest,
) -> (slop_ninja_publisher::index::ReferencePointer, bool) {
    for (key, current) in [
        (layout::REFERENCE_CURRENT, true),
        (layout::REFERENCE_PREVIOUS, false),
    ] {
        if let Some(bytes) = x.public.bytes(key) {
            let p: slop_ninja_publisher::index::ReferencePointer =
                serde_json::from_slice(&bytes).unwrap();
            if p.artifact == artifact.hex() {
                return (p, current);
            }
        }
    }
    panic!("no reference pointer names {}", artifact.hex());
}

#[test]
fn a_staged_reference_package_is_linked() {
    let b = build(0);
    let x = buckets(&b);
    let (artifact, bytes) = reference_artifact(&b);
    x.public
        .insert(&layout::reference_package_manifest(&artifact), bytes);
    run(&config(Some(0), false), &x, &mut Memo::default())
        .1
        .unwrap();
    let (pointer, current) = pointer_of(&x, &artifact);
    assert_eq!(pointer.package, Some(layout::reference_package(&artifact)));
    assert_eq!(pointer.licence.as_deref(), Some("CC-BY-SA-4.0"));
    assert!(pointer.path.is_none() && pointer.withheld.is_none());
    let doc: LeaderboardDoc = public_json(&x, layout::LEADERBOARD);
    if current {
        assert_eq!(doc.current_reference, pointer);
        let site = String::from_utf8(x.public.bytes(layout::SITE_INDEX).unwrap()).unwrap();
        assert!(site.contains(&format!(
            "{}package.json",
            layout::reference_package(&artifact)
        )));
    } else {
        assert_eq!(doc.previous_reference, Some(pointer));
    }
    // The publisher never writes under the packages prefix.
    let staged: Vec<String> = x
        .public
        .keys()
        .into_iter()
        .filter(|k| k.starts_with("v1/reference/packages/"))
        .collect();
    assert_eq!(staged, [layout::reference_package_manifest(&artifact)]);
}

#[test]
fn a_staged_manifest_with_another_digest_is_not_linked() {
    let b = build(0);
    let x = buckets(&b);
    let (artifact, mut bytes) = reference_artifact(&b);
    bytes.push(b'\n');
    x.public
        .insert(&layout::reference_package_manifest(&artifact), bytes);
    run(&config(Some(0), false), &x, &mut Memo::default())
        .1
        .unwrap();
    let (pointer, _) = pointer_of(&x, &artifact);
    assert!(pointer.package.is_none() && pointer.path.is_none());
    assert_eq!(
        pointer.withheld.as_deref(),
        Some(slop_ninja_publisher::index::REFERENCE_NOT_STAGED)
    );
    // Past the scorer's manifest bound it is not read at all.
    let x = buckets(&b);
    x.public.insert(
        &layout::reference_package_manifest(&artifact),
        vec![b' '; layout::REFERENCE_PACKAGE_MANIFEST_BOUND as usize + 1],
    );
    run(&config(Some(0), false), &x, &mut Memo::default())
        .1
        .unwrap();
    assert!(pointer_of(&x, &artifact).0.package.is_none());
}

#[test]
fn the_package_manifest_bound_is_the_specs() {
    let spec = include_str!("../../crates/artifact-check/src/spec.rs");
    assert!(spec.contains("pub const MAX_MANIFEST_BYTES: u64 = 1 << 20;"));
    assert_eq!(layout::REFERENCE_PACKAGE_MANIFEST_BOUND, 1 << 20);
}

#[test]
fn public_text_carries_no_rights_hold_wording() {
    let sources = [
        include_str!("../src/index.rs"),
        include_str!("../src/publish.rs"),
        include_str!("../src/leaderboard.rs"),
        include_str!("../src/config.rs"),
        include_str!("../src/lib.rs"),
        include_str!("../README.md"),
        include_str!("../wrangler.toml"),
        include_str!("../../../site/index.html"),
    ];
    for text in sources {
        let lower = text.to_lowercase();
        for phrase in [
            "rights question",
            "counsel",
            "stays private",
            "stay private until",
            "weights not published",
            "private reference",
        ] {
            assert!(!lower.contains(phrase), "{phrase:?} remains");
        }
    }
}

#[test]
fn nothing_private_is_published() {
    let b = build(0);
    let x = buckets(&b);
    run(&config(Some(0), true), &x, &mut Memo::default())
        .1
        .unwrap();
    let keys = x.public.keys();
    for key in &keys {
        for forbidden in [
            "/batch/",
            "/results/",
            "/summaries/",
            "probes/",
            "pools/",
            "keys.json",
            "/job-",
        ] {
            assert!(!key.contains(forbidden), "{key} published");
        }
        let bytes = x.public.bytes(key).unwrap();
        let hay = String::from_utf8_lossy(&bytes);
        assert!(
            !hay.contains("PRIVATE-DOCUMENT-TEXT"),
            "{key} carries private text"
        );
        assert!(
            !hay.contains("PRIVATE-PREDICTION-VECTOR"),
            "{key} carries a prediction vector"
        );
        assert!(
            !hay.contains("0.4242424242") && !hay.contains("0.1717171717"),
            "{key} carries a per-document prediction"
        );
        if key.ends_with(".json") {
            slop_ninja_publisher::redact::scan_json(&bytes)
                .unwrap_or_else(|e| panic!("{key}: {e}"));
        }
    }
    // Only the documented layout.
    for key in &keys {
        assert!(
            key == layout::SITE_INDEX || key == layout::SITE_STYLE || key.starts_with("v1/"),
            "{key} outside the layout"
        );
    }
}

#[test]
fn a_tampered_record_is_refused_and_nothing_is_indexed() {
    let b = build(0);
    let x = buckets(&b);
    let key = format!("cohorts/{}/package.signed", b.label);
    let mut bytes = x.receipts.bytes(&key).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x40;
    x.receipts.insert(&key, bytes);
    let (report, idx) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert!(report.published.is_empty());
    assert_eq!(report.refused.len(), 1, "{report:?}");
    assert!(report.refused[0].1.starts_with("package.signed"));
    assert!(!idx.unwrap().contains(&b.label));
    assert!(
        x.public.keys().iter().all(|k| !k.contains(&b.label)),
        "nothing of the cohort copied"
    );
}

#[test]
fn a_source_changed_after_verification_is_refused() {
    let b = build(0);
    let x = buckets(&b);
    x.receipts
        .tamper_on_copy
        .borrow_mut()
        .insert(format!("cohorts/{}/closure.signed", b.label));
    let (report, idx) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert_eq!(report.refused.len(), 1, "{report:?}");
    assert!(
        report.refused[0].1.contains("changed after verification"),
        "{report:?}"
    );
    assert!(!idx.unwrap().contains(&b.label));
    assert!(
        x.public
            .bytes(&format!("v1/cohorts/{}/closure.signed", b.label))
            .is_none()
    );
}

#[test]
fn receipt_chunks_must_hold_exactly_the_indexed_receipts() {
    let b = build(0);
    // A receipt listed twice, another missing, the chunk map adjusted so its
    // totals still match the index.
    let x = buckets(&b);
    let d = format!("cohorts/{}", b.label);
    let first = format!("{d}/receipts/bundle-job-000.bin");
    let second = format!("{d}/receipts/bundle-job-001.bin");
    let mut one = x.receipts.bytes(&first).unwrap();
    let two = x.receipts.bytes(&second).unwrap();
    let len = u32::from_le_bytes(two[..4].try_into().unwrap()) as usize;
    let repeated = two[..4 + len].to_vec();
    let mut tail = two[4 + len..].to_vec();
    let next = u32::from_le_bytes(tail[..4].try_into().unwrap()) as usize;
    assert_eq!(next, len, "fixture receipts have one encoded length");
    tail.drain(..4 + next);
    one.extend_from_slice(&repeated);
    let new_two = [repeated, tail].concat();
    x.receipts.insert(&first, one.clone());
    x.receipts.insert(&second, new_two.clone());
    let map_key = format!("{d}/chunk-map.json");
    let mut chunk_map: serde_json::Value =
        serde_json::from_slice(&x.receipts.bytes(&map_key).unwrap()).unwrap();
    let objs = chunk_map["objects"].as_array_mut().unwrap();
    objs[0]["sha256"] = object_sha256(&one).hex().into();
    objs[0]["bytes"] = one.len().into();
    objs[0]["receipts"] = (objs[0]["receipts"].as_u64().unwrap() + 1).into();
    objs[1]["sha256"] = object_sha256(&new_two).hex().into();
    objs[1]["bytes"] = new_two.len().into();
    objs[1]["receipts"] = (objs[1]["receipts"].as_u64().unwrap() - 1).into();
    x.receipts
        .insert(&map_key, serde_json::to_vec(&chunk_map).unwrap());
    let (report, _) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert_eq!(report.refused.len(), 1, "{report:?}");
    assert!(
        report.refused[0].1.contains("receipt listed twice"),
        "{report:?}"
    );
    // A chunk map whose sizes do not add up to the index.
    let x = buckets(&b);
    let d = format!("cohorts/{}", b.label);
    let mut chunk_map: serde_json::Value =
        serde_json::from_slice(&x.receipts.bytes(&format!("{d}/chunk-map.json")).unwrap()).unwrap();
    chunk_map["objects"][0]["bytes"] =
        (chunk_map["objects"][0]["bytes"].as_u64().unwrap() + 1).into();
    x.receipts.insert(
        &format!("{d}/chunk-map.json"),
        serde_json::to_vec(&chunk_map).unwrap(),
    );
    let (report, _) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert!(
        report.refused[0].1.contains("exactly the indexed receipts"),
        "{report:?}"
    );
}

#[test]
fn an_incomplete_closure_waits_then_publishes() {
    let b = build(0);
    let x = buckets(&b);
    let key = format!("cohorts/{}/published-contents-after.bin", b.label);
    let saved = x.receipts.bytes(&key).unwrap();
    x.receipts.remove(&key);
    let cfg = config(Some(0), false);
    let (report, idx) = run(&cfg, &x, &mut Memo::default());
    assert!(report.refused.is_empty());
    assert_eq!(report.pending.len(), 1);
    assert!(!idx.unwrap().contains(&b.label));
    x.receipts.insert(&key, saved);
    let (report, idx) = run(&cfg, &x, &mut Memo::default());
    assert_eq!(report.published, vec![b.label.clone()]);
    assert!(idx.unwrap().contains(&b.label));
}

#[test]
fn cohorts_before_the_public_record_are_skipped_once() {
    let b = build(0);
    let x = buckets(&b);
    let cfg = config(Some(b.c.manifest.opened_epoch + 1), false);
    let mut memo = Memo::default();
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert!(report.published.is_empty() && report.refused.is_empty());
    assert_eq!(report.outside, 1);
    assert!(!idx.unwrap().contains(&b.label));
    let outside = memo.outside_record.get(&b.label).unwrap();
    assert_eq!(outside.opened_epoch, Some(b.c.manifest.opened_epoch));
    // Its initial reference is remembered all the same.
    let initial = slop_ninja_publisher::index::initial_reference(&b.c.manifest.references).unwrap();
    assert!(memo.references.contains(&ReferenceDigests::of(&initial)));
    let read = x.receipts.bytes_read.get();
    let (report, _) = run(&cfg, &x, &mut memo);
    assert_eq!(report.outside, 1);
    assert_eq!(x.receipts.bytes_read.get(), read, "never read again");
    // Moving the boundary back reconsiders it.
    let (report, idx) = run(&config(Some(0), false), &x, &mut memo);
    assert_eq!(report.published, vec![b.label.clone()]);
    assert!(idx.unwrap().contains(&b.label));
    // Publication disabled: nothing is published, the trust file and page are.
    let x = buckets(&b);
    let (report, idx) = run(&config(None, false), &x, &mut Memo::default());
    assert!(report.publication_disabled && report.published.is_empty());
    assert!(!idx.unwrap().contains(&b.label));
    assert!(
        x.public.bytes(layout::TRUST).is_some() && x.public.bytes(layout::SITE_INDEX).is_some()
    );
}

/// A label outside `RECORD_LABEL_PREFIX` is never read; a cohort whose
/// manifest does not open (rotated roots, an older format) is remembered
/// once, never read again, and raised as one aggregated condition.
#[test]
fn labels_outside_the_record_and_unverifiable_manifests() {
    let b = build(0);
    let x = buckets(&b);
    let mut cfg = config(Some(0), false);
    cfg.record_label_prefix = Some("sn601-r1-".into());
    let mut memo = Memo::default();
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert_eq!(x.receipts.bytes_read.get(), 0, "nothing read");
    assert!(report.outside >= 1 && report.published.is_empty());
    assert!(!idx.unwrap().contains(&b.label));
    // Rotated trust roots: the manifest no longer opens.
    let mut cfg = config(Some(0), false);
    cfg.keys = slop_ninja_protocol::TrustedKeys::new([7; 32], [8; 32], [9; 32]).unwrap();
    let mut memo = Memo::default();
    let (report, _) = run(&cfg, &x, &mut memo);
    assert!(report.refused.is_empty(), "{report:?}");
    assert_eq!(report.unverifiable, vec![b.label.clone()]);
    assert!(memo.unverifiable[&b.label].reason.contains("untrusted_key"));
    let read = x.receipts.bytes_read.get();
    for _ in 0..2 {
        let (report, _) = run(&cfg, &x, &mut memo);
        assert_eq!(report.unverifiable, vec![b.label.clone()]);
        assert!(report.refused.is_empty() && report.pending.is_empty());
    }
    assert_eq!(x.receipts.bytes_read.get(), read, "never read again");
    // A prefix that leaves it outside the record clears the condition.
    cfg.record_label_prefix = Some("sn601-r1-".into());
    let (report, _) = run(&cfg, &x, &mut memo);
    assert!(report.unverifiable.is_empty());
}

/// The record starts at `R_0`: with no cohort listed, a cohort whose
/// reference is `R_1` waits (and alerts after a tempo), and is never
/// refused for it.
#[test]
fn the_record_starts_at_r0() {
    let b = build(0);
    assert_eq!(b.c.manifest.references.index, 1, "the fixture is R_1");
    let x = buckets_unseeded(&b);
    let (report, idx) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert!(report.refused.is_empty() && report.published.is_empty());
    assert!(report.pending[0].1.contains("first cohort"), "{report:?}");
    assert!(idx.unwrap().cohorts.is_empty());
}

/// Cohort numbers must order like opening epochs: a restarted authority
/// that numbers from 1 again cannot mix into the record.
#[test]
fn conflicting_cohort_numbers_are_refused() {
    let b = build(0);
    let x = buckets(&b);
    let mut idx: slop_ninja_publisher::index::CohortIndex = public_json(&x, layout::COHORT_INDEX);
    // A listed cohort with a higher number that opened earlier.
    idx.cohorts[0].cohort = b.c.manifest.cohort + 5;
    x.public
        .insert(layout::COHORT_INDEX, serde_json::to_vec(&idx).unwrap());
    let mut memo = Memo::default();
    let (report, idx) = run(&config(Some(0), false), &x, &mut memo);
    assert!(report.refused[0].1.contains("conflicts"), "{report:?}");
    assert!(!idx.unwrap().contains(&b.label));
    // Remembered: the next run reads nothing of it and still raises it.
    let read = x.receipts.bytes_read.get();
    let (report, _) = run(&config(Some(0), false), &x, &mut memo);
    assert_eq!(report.refused.len(), 1);
    assert_eq!(x.receipts.bytes_read.get(), read);
}

/// A record that is not a `reward-v1` manifest (a load or older cohort) is
/// outside the record, not an unverifiable manifest.
#[test]
fn a_record_of_another_kind_is_outside_the_record() {
    let b = build(0);
    let x = buckets(&b);
    let d = format!("cohorts/{}", b.label);
    let package = x.receipts.bytes(&format!("{d}/package.signed")).unwrap();
    x.receipts.insert(&format!("{d}/manifest.signed"), package);
    let mut memo = Memo::default();
    let (report, _) = run(&config(Some(0), false), &x, &mut memo);
    assert!(
        report.unverifiable.is_empty() && report.refused.is_empty(),
        "{report:?}"
    );
    assert_eq!(report.outside, 1);
    assert_eq!(
        memo.outside_record[&b.label].reason,
        "not a reward-v1 cohort manifest"
    );
}

/// A manifest signed for another chain or subnet is outside the record.
#[test]
fn a_manifest_for_another_chain_is_outside_the_record() {
    let b = build(0);
    let x = buckets(&b);
    let mut cfg = config(Some(0), false);
    cfg.chain.netuid += 1;
    let mut memo = Memo::default();
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert!(report.published.is_empty() && report.refused.is_empty());
    assert_eq!(report.outside, 1);
    assert!(!idx.unwrap().contains(&b.label));
    assert_eq!(memo.outside_record[&b.label].opened_epoch, None);
}

#[test]
fn receipts_stay_private_when_d7_says_so() {
    let b = build(0);
    let x = buckets(&b);
    let mut cfg = config(Some(0), false);
    cfg.publish_receipts = false;
    let (report, _) = run(&cfg, &x, &mut Memo::default());
    assert_eq!(report.published.len(), 1);
    assert!(
        x.public
            .keys()
            .iter()
            .all(|k| !k.contains("receipt") && !k.ends_with("chunk-map.json"))
    );
}

#[test]
fn the_memory_budget_refuses_before_reading() {
    let b = build(0);
    let x = buckets_unseeded(&b);
    let (report, idx) = run_with(
        &config(Some(0), false),
        &x,
        &mut Memo::default(),
        &Budget::new(512),
    );
    assert!(report.published.is_empty());
    assert_eq!(report.refused.len(), 1, "{report:?}");
    assert!(report.refused[0].1.contains("needs"), "{report:?}");
    assert_eq!(
        x.receipts.bytes_read.get(),
        0,
        "refused before any body was read"
    );
    assert!(idx.unwrap().cohorts.is_empty());
}

fn uid_of(b: &Built, uid: u16) -> &slop_ninja_protocol::EntrantScore {
    b.c.package
        .entrants
        .iter()
        .find(|e| e.identity.uid == uid)
        .unwrap()
}

fn artifact_of(p: &Publication, uid: u16) -> &slop_ninja_publisher::index::ArtifactEntry {
    p.artifacts.iter().find(|a| a.uid == uid).unwrap()
}

/// Artifact publication is default-deny, on a roster shaped like the
/// rehearsal's (cohort 8): an earlier manifest's `R_0` entered as an
/// ordinary entrant with an identical copy and a near copy, operator and
/// synthetic entrants without an admission record, an operator hotkey with
/// one, a mismatched record, and public-copy gate failures. Only admitted
/// outside entrants are published.
#[test]
fn artifact_publication_is_default_deny() {
    let b = build(0);
    let x = buckets(&b);
    let mut cfg = config(Some(0), true);
    // uid 19 is one of ours.
    cfg.operator_hotkeys.insert(uid_of(&b, 19).identity.hotkey);
    // uid 17's content was `R_0` in an earlier manifest (since replaced).
    let r0 = uid_of(&b, 17);
    let mut memo = Memo::default();
    memo.references.insert(ReferenceDigests {
        content: r0.content.hex(),
        artifact: "00".repeat(32),
    });
    // uid 20 entered from the operator's plan: no admission record.
    x.eval
        .remove(&admission_key(&b.label, &uid_of(&b, 20).identity.hotkey));
    // uid 21's stored record is another entrant's upload.
    let other = x
        .eval
        .bytes(&admission_key(&b.label, &uid_of(&b, 31).identity.hotkey))
        .unwrap();
    x.eval.insert(
        &admission_key(&b.label, &uid_of(&b, 21).identity.hotkey),
        other,
    );
    // uid 26's object is not uploaded yet.
    let missing = ARTIFACT_TEMPLATE.replace("{sha256}", &uid_of(&b, 26).artifact.hex());
    let saved = x.eval.bytes(&missing).unwrap();
    x.eval.remove(&missing);

    let (report, idx) = run(&cfg, &x, &mut memo);
    let idx = idx.unwrap();
    assert_eq!(report.published, vec![b.label.clone()]);
    let p: Publication = public_json(&x, &layout::publication(&b.label));
    let reason = |uid| artifact_of(&p, uid).reason.clone().unwrap_or_default();
    let state = |uid| artifact_of(&p, uid).state;
    let withheld = [
        (17, publish::WITHHELD_INITIAL),
        (22, publish::WITHHELD_INITIAL),
        (27, publish::WITHHELD_CLUSTER),
        (19, publish::WITHHELD_OPERATOR),
        (20, publish::WITHHELD_NOT_ADMITTED),
        (21, publish::WITHHELD_ADMISSION),
        (29, publish::WITHHELD_COPY),
        (30, publish::WITHHELD_COPY),
        (32, publish::WITHHELD_COPY),
        (34, publish::WITHHELD_COPY),
    ];
    for (uid, why) in withheld {
        assert_eq!(state(uid), ArtifactState::Withheld, "uid {uid}");
        assert_eq!(reason(uid), why, "uid {uid}");
        let e = uid_of(&b, uid);
        assert!(
            x.public.bytes(&layout::artifact(&e.artifact)).is_none(),
            "uid {uid} published"
        );
    }
    for uid in [18, 23, 28, 24, 25, 31, 35] {
        assert_eq!(state(uid), ArtifactState::Published, "uid {uid}");
        let e = uid_of(&b, uid);
        let published = x.public.bytes(&layout::artifact(&e.artifact)).unwrap();
        assert_eq!(object_sha256(&published), e.artifact);
    }
    assert_eq!(state(26), ArtifactState::Pending);
    assert_eq!(reason(26), publish::WAIT_SOURCE);
    // The mismatched record alerts; the pending copy alerts after a tempo.
    assert!(
        report
            .refused
            .iter()
            .any(|(s, r)| s.starts_with(&b.label) && r == publish::WITHHELD_ADMISSION),
        "{report:?}"
    );
    assert!(
        report
            .pending
            .iter()
            .any(|(s, r)| s.contains("/artifact-") && r == publish::WAIT_SOURCE),
        "{report:?}"
    );
    // The fixture manifest's own initial reference (`R_0`, the predecessor
    // of `R_1`) is recorded in the index for every later cohort.
    let initial = slop_ninja_publisher::index::initial_reference(&b.c.manifest.references).unwrap();
    assert!(
        idx.initial_references
            .iter()
            .any(|r| r.content == initial.content.hex() && r.artifact == initial.artifact.hex())
    );
    let entry = entry_of(&idx, &b.label);
    assert_eq!(entry.artifacts_pending, 1);
    assert_eq!(entry.artifacts_withheld, withheld.len() as u32);

    // The upload lands; a later run publishes it and refreshes the records.
    x.eval.insert(&missing, saved);
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert!(report.artifacts_copied >= 1, "{report:?}");
    assert_eq!(entry_of(&idx.unwrap(), &b.label).artifacts_pending, 0);
    let p: Publication = public_json(&x, &layout::publication(&b.label));
    assert_eq!(artifact_of(&p, 26).state, ArtifactState::Published);
    let doc: LeaderboardDoc = public_json(&x, layout::LEADERBOARD);
    let row = doc.entrants.iter().find(|r| r.uid == 26).unwrap();
    assert_eq!(
        row.artifact.as_ref().unwrap().state,
        ArtifactState::Published
    );
}

/// Without the admission layout nothing is copied, and the static checks
/// still withhold what they can.
#[test]
fn without_the_admission_layout_artifacts_wait() {
    let b = build(0);
    let x = buckets(&b);
    run(&config(Some(0), false), &x, &mut Memo::default())
        .1
        .unwrap();
    let p: Publication = public_json(&x, &layout::publication(&b.label));
    for a in &p.artifacts {
        match a.state {
            ArtifactState::Pending => {
                assert_eq!(a.reason.as_deref(), Some(publish::WAIT_LAYOUT))
            }
            ArtifactState::Withheld => {}
            ArtifactState::Published => panic!("uid {} published", a.uid),
        }
    }
    assert!(
        x.public
            .keys()
            .iter()
            .all(|k| !k.starts_with("v1/artifacts/"))
    );
}

/// A pending artifact is checked again on every retry: a digest listed
/// private after publication started, or an initial reference recorded in
/// the public index since, withholds it for good, even once its object is
/// uploaded.
#[test]
fn retries_repeat_the_static_checks() {
    for listed_in in ["config", "index"] {
        let b = build(0);
        let x = buckets(&b);
        let mut cfg = config(Some(0), true);
        let e = uid_of(&b, 26).clone();
        let missing = ARTIFACT_TEMPLATE.replace("{sha256}", &e.artifact.hex());
        let saved = x.eval.bytes(&missing).unwrap();
        x.eval.remove(&missing);
        run(&cfg, &x, &mut Memo::default()).1.unwrap();
        let p: Publication = public_json(&x, &layout::publication(&b.label));
        assert_eq!(artifact_of(&p, 26).state, ArtifactState::Pending);
        match listed_in {
            "config" => {
                cfg.private_digests.insert(e.artifact);
            }
            _ => {
                let mut idx: slop_ninja_publisher::index::CohortIndex =
                    public_json(&x, layout::COHORT_INDEX);
                idx.initial_references
                    .push(slop_ninja_publisher::index::InitialReference {
                        content: e.content.hex(),
                        artifact: "00".repeat(32),
                        first_cohort: 1,
                    });
                x.public
                    .insert(layout::COHORT_INDEX, serde_json::to_vec(&idx).unwrap());
            }
        }
        x.eval.insert(&missing, saved);
        let (report, _) = run(&cfg, &x, &mut Memo::default());
        assert_eq!(report.artifacts_copied, 0, "{listed_in}");
        let p: Publication = public_json(&x, &layout::publication(&b.label));
        let a = artifact_of(&p, 26);
        assert_eq!(a.state, ArtifactState::Withheld, "{listed_in}");
        assert!(x.public.bytes(&layout::artifact(&e.artifact)).is_none());
    }
}

/// The record's own initial reference is withheld under any entrant's name,
/// by content or by artifact digest, and so is a listed private digest.
#[test]
fn the_initial_reference_is_never_published() {
    use slop_ninja_protocol::Record;
    let b = build(0);
    let x = buckets(&b);
    let cfg = config(Some(0), true);
    let opts = Options {
        keys: &cfg.keys,
        chain: &cfg.chain,
        receipts: true,
    };
    let v = block_on(verify::verify_cohort(
        &x.receipts,
        &dir(&b.label),
        &b.label,
        &opts,
        &Budget::worker(),
    ))
    .unwrap();
    let initial = slop_ninja_publisher::index::initial_reference(&v.manifest.references).unwrap();
    let refs = std::iter::once(ReferenceDigests::of(&initial)).collect();
    let e = uid_of(&b, 31);
    let f = publish::EntrantFacts::of(e, &v);
    assert_eq!(publish::static_withhold(&f, Some(e), &cfg, &refs), None);
    let mut by_content = f.clone();
    by_content.content = initial.content;
    assert_eq!(
        publish::static_withhold(&by_content, Some(e), &cfg, &refs),
        Some(publish::WITHHELD_INITIAL)
    );
    let mut by_artifact = f.clone();
    by_artifact.artifact = initial.artifact;
    assert_eq!(
        publish::static_withhold(&by_artifact, Some(e), &cfg, &refs),
        Some(publish::WITHHELD_INITIAL)
    );
    let mut listed = cfg.clone();
    listed.private_digests.insert(e.artifact);
    assert_eq!(
        publish::static_withhold(&f, Some(e), &listed, &refs),
        Some(publish::WITHHELD_LISTED)
    );
    // The admission record must be this entrant's own upload.
    assert!(
        f.admitted_by(
            &slop_ninja_protocol::submission::SubmissionUpload::from_bytes(
                &x.eval
                    .bytes(&admission_key(&b.label, &e.identity.hotkey))
                    .unwrap()
            )
            .unwrap(),
            &cfg
        )
    );
    let mut wrong_nonce = f.clone();
    wrong_nonce.nonce = Some(slop_ninja_protocol::Nonce([9; 32]));
    assert!(
        !wrong_nonce.admitted_by(
            &slop_ninja_protocol::submission::SubmissionUpload::from_bytes(
                &x.eval
                    .bytes(&admission_key(&b.label, &e.identity.hotkey))
                    .unwrap()
            )
            .unwrap(),
            &cfg
        )
    );
}

#[test]
fn components_are_withheld_unless_they_pass_redaction_and_agree() {
    let b = build(0);
    let key = format!("cohorts/{}/components.json", b.label);
    let published = |c: serde_json::Value| {
        let x = buckets(&b);
        x.receipts.insert(&key, serde_json::to_vec(&c).unwrap());
        run(&config(Some(0), false), &x, &mut Memo::default())
            .1
            .unwrap();
        let publication: Publication = public_json(&x, &layout::publication(&b.label));
        let present = x
            .public
            .bytes(&format!("v1/cohorts/{}/components.json", b.label))
            .is_some();
        assert_eq!(present, publication.components_withheld.is_none());
        present
    };
    // The coordinator's exact shape is published.
    assert!(published(components(&b)));
    // Per-document values or excerpts under keys the schema does not know,
    // at any level, are withheld (the review's probes).
    let mut probes = Vec::new();
    for (path, value) in [
        ("entrants.0.doc_brier", serde_json::json!([0.42, 0.17])),
        (
            "entrants.0.per_doc",
            serde_json::json!([{"id": 1, "p": 0.42}]),
        ),
        (
            "note",
            serde_json::json!("PRIVATE-DOCUMENT-TEXT short excerpt"),
        ),
        ("brier_by_task", serde_json::json!({"17": 0.42})),
        ("entrants.0.predictions", serde_json::json!([0.5])),
    ] {
        let mut c = components(&b);
        let mut target = &mut c;
        let parts: Vec<&str> = path.split('.').collect();
        for part in &parts[..parts.len() - 1] {
            target = match part.parse::<usize>() {
                Ok(i) => &mut target[i],
                Err(_) => &mut target[*part],
            };
        }
        target[parts[parts.len() - 1]] = value;
        probes.push(c);
    }
    // A policy document the manifest does not bind; a rule that is not an
    // identifier; disagreeing units; an unexecuted entrant that was scored.
    let mut c = components(&b);
    c["policy"] = serde_json::json!({"schema": "slop_ninja_eval_gate_policy_v2"});
    probes.push(c);
    let mut c = components(&b);
    c["fpr_rule"] = serde_json::json!("PRIVATE DOCUMENT TEXT");
    probes.push(c);
    let mut c = components(&b);
    c["entrants"][0]["units"] = serde_json::json!(12345);
    probes.push(c);
    let mut c = components(&b);
    c["unexecuted"] = serde_json::json!([0]);
    probes.push(c);
    for c in probes {
        assert!(!published(c.clone()), "published: {c}");
    }
}

/// The chunk map may not list an object twice or an object without
/// receipts, and every object must be a bundle under `receipts/`.
#[test]
fn chunk_map_entries_are_distinct_bundles() {
    let b = build(0);
    let d = format!("cohorts/{}", b.label);
    let map_key = format!("{d}/chunk-map.json");
    let refused = |edit: &dyn Fn(&Buckets3, &mut serde_json::Value)| {
        let x = buckets(&b);
        let mut map: serde_json::Value =
            serde_json::from_slice(&x.receipts.bytes(&map_key).unwrap()).unwrap();
        edit(&x, &mut map);
        x.receipts
            .insert(&map_key, serde_json::to_vec(&map).unwrap());
        let (report, idx) = run(&config(Some(0), false), &x, &mut Memo::default());
        assert!(!idx.unwrap().contains(&b.label));
        report.refused[0].1.clone()
    };
    let empty = format!("{d}/receipts/empty.bin");
    let why = refused(&|x, map| {
        x.receipts.insert(&empty, Vec::new());
        let entry = serde_json::json!({"key": empty, "sha256": object_sha256(&[]).hex(),
            "bytes": 0, "receipts": 0});
        map["objects"].as_array_mut().unwrap().push(entry.clone());
        map["objects"].as_array_mut().unwrap().push(entry);
    });
    assert!(why.contains("holds no receipt"), "{why}");
    let why = refused(&|_, map| {
        let first = map["objects"][0].clone();
        map["objects"].as_array_mut().unwrap().push(first);
    });
    assert!(why.contains("listed twice"), "{why}");
    // The same bytes outside `receipts/` are refused by place.
    let why = refused(&|x, map| {
        let key = map["objects"][0]["key"].as_str().unwrap().to_owned();
        let moved = format!("{d}/elsewhere/bundle-job-000.bin");
        x.receipts.insert(&moved, x.receipts.bytes(&key).unwrap());
        map["objects"][0]["key"] = moved.into();
    });
    assert!(why.contains("outside receipts/"), "{why}");
}

/// Detailed receipts are cached one tempo at most, since they are deleted
/// after N+2; every other signed object is immutable.
#[test]
fn detailed_receipts_are_not_cached_as_immutable() {
    let b = build(0);
    let x = buckets(&b);
    let cfg = config(Some(0), false);
    run(&cfg, &x, &mut Memo::default()).1.unwrap();
    let publication: Publication = public_json(&x, &layout::publication(&b.label));
    let tempo = Cache::Bounded(cfg.tempo_blocks * cfg.block_secs);
    for o in &publication.objects {
        let key = format!("v1/cohorts/{}/{}", b.label, o.rel);
        let cache = x.public.objects.borrow()[&key].cache.unwrap();
        if publish::is_detailed_receipt(&o.rel) {
            assert_eq!(cache, tempo, "{}", o.rel);
            assert!(!cache.header().contains("immutable"));
        } else {
            assert_eq!(cache, Cache::Immutable, "{}", o.rel);
        }
    }
}

/// Detailed receipts leave the public host once N+1 and N+2 are published
/// and N's target is superseded; the signed records with the receipt root and
/// count stay.
#[test]
fn detailed_receipts_are_deleted_after_n_plus_2() {
    let b = build(0);
    let x = buckets(&b);
    let cfg = config(Some(0), false);
    run(&cfg, &x, &mut Memo::default()).1.unwrap();
    let dir = format!("v1/cohorts/{}", b.label);
    assert!(
        x.public
            .bytes(&format!("{dir}/receipt-index.signed"))
            .is_some()
    );
    // Two later cohorts appear in the index (their own records elsewhere).
    let mut idx: slop_ninja_publisher::index::CohortIndex = public_json(&x, layout::COHORT_INDEX);
    for k in 1..=2u64 {
        let mut later = entry_of(&idx, &b.label).clone();
        later.cohort += k;
        later.opened_epoch += 20 * k;
        later.label = format!("later-{k}");
        later.receipts_retained = false;
        idx.cohorts.push(later);
    }
    // Two later cohorts without a target do not supersede N's target.
    let no_target = {
        let mut i = idx.clone();
        for c in i
            .cohorts
            .iter_mut()
            .filter(|c| c.label.starts_with("later-"))
        {
            c.target = None;
        }
        i
    };
    x.public.insert(
        layout::COHORT_INDEX,
        serde_json::to_vec(&no_target).unwrap(),
    );
    let (report, _) = run(&cfg, &x, &mut Memo::default());
    assert_eq!(
        report.receipts_deleted, 0,
        "the active target keeps its receipts"
    );
    // One later cohort is not enough.
    let only_one = {
        let mut i = idx.clone();
        i.cohorts.pop();
        i
    };
    x.public
        .insert(layout::COHORT_INDEX, serde_json::to_vec(&only_one).unwrap());
    let (report, _) = run(&cfg, &x, &mut Memo::default());
    assert_eq!(report.receipts_deleted, 0);
    x.public
        .insert(layout::COHORT_INDEX, serde_json::to_vec(&idx).unwrap());
    let (report, after) = run(&cfg, &x, &mut Memo::default());
    assert_eq!(report.receipts_deleted, 1, "{report:?}");
    let after = after.unwrap();
    assert!(
        !after
            .cohorts
            .iter()
            .find(|c| c.label == b.label)
            .unwrap()
            .receipts_retained
    );
    let publication: Publication = public_json(&x, &layout::publication(&b.label));
    assert_eq!(
        publication.receipts_deleted_after,
        Some(b.c.manifest.cohort + 2)
    );
    for o in &publication.objects {
        let present = x.public.bytes(&format!("{dir}/{}", o.rel)).is_some();
        assert_eq!(present, !publish::is_detailed_receipt(&o.rel), "{}", o.rel);
    }
    assert!(x.public.bytes(&format!("{dir}/package.signed")).is_some());
    // The private buckets are never written or deleted from.
    assert_eq!((x.receipts.puts.get(), x.receipts.deletes.get()), (0, 0));
    assert_eq!((x.eval.puts.get(), x.eval.deletes.get()), (0, 0));
    // Idempotent.
    let (report, _) = run(&cfg, &x, &mut Memo::default());
    assert_eq!(report.receipts_deleted, 0);
}

/// Stores a signed authorization object in the receipts bucket at its
/// content-addressed key and returns its pointer entry.
fn store_authorization(x: &Buckets3, bytes: Vec<u8>) -> publish::ObjectRef {
    let sha = object_sha256(&bytes);
    let path = layout::private_authorization(&sha);
    let r = publish::ObjectRef {
        path: path.clone(),
        sha256: sha.hex(),
        bytes: bytes.len() as u64,
    };
    x.receipts.insert(&path, bytes);
    r
}

fn write_pointer(
    x: &Buckets3,
    kind: &str,
    auth: publish::ObjectRef,
    renewal: Option<publish::ObjectRef>,
) {
    let pointer = publish::AuthorizationPointer {
        schema: publish::AUTHORIZATION_SCHEMA.into(),
        chain: slop_ninja_admission_wire::json::Chain::from_binding(
            &slop_ninja_protocol::fixtures_reward_v1::chain(),
        ),
        kind: kind.into(),
        authorization: auth,
        renewal,
    };
    x.receipts.insert(
        layout::PRIVATE_AUTHORIZATION_POINTER,
        serde_json::to_vec_pretty(&pointer).unwrap(),
    );
}

/// The current authorization and its renewal reach
/// `v1/authorizations/current.json` (the validator fetcher's source) once
/// they authenticate and their cohort is listed; a renewal of another
/// authorization is refused.
#[test]
fn authorizations_are_published_once_their_cohort_is_listed() {
    use slop_ninja_protocol::fixtures_reward_v1 as fx;
    use slop_ninja_protocol::{
        LaunchAuthorization, Record, RewardIntentAuthorization, RewardIntentRenewal,
    };
    let b = build(0);
    let m = &b.c.manifest;
    let auth = RewardIntentAuthorization {
        chain: m.chain.clone(),
        cohort: m.cohort,
        cohort_manifest: m.digest().unwrap(),
        score_package: b.c.package.digest().unwrap(),
        target: b.c.target.digest().unwrap(),
        checkpoint_generation: 1,
        issued_epoch: m.opened_epoch + 16,
        expires_epoch: m.opened_epoch + 56,
    };
    let renewal = RewardIntentRenewal {
        authorization: auth.digest().unwrap(),
        checkpoint_generation: 2,
        issued_epoch: m.opened_epoch + 30,
        expires_epoch: m.opened_epoch + 70,
    };
    let auth_bytes = fx::signed(auth.clone()).unwrap().to_bytes().unwrap();
    let renewal_bytes = fx::signed(renewal).unwrap().to_bytes().unwrap();

    // Nothing stored: nothing published, nothing raised.
    let x = buckets(&b);
    let (report, _) = run(&config(None, false), &x, &mut Memo::default());
    assert!(report.pending.is_empty() && report.refused.is_empty());
    assert!(x.public.bytes(layout::AUTHORIZATION_CURRENT).is_none());

    // Stored before its cohort is listed: pending.
    let a = store_authorization(&x, auth_bytes.clone());
    let r = store_authorization(&x, renewal_bytes.clone());
    write_pointer(&x, "cohort", a.clone(), Some(r.clone()));
    let (report, _) = run(&config(None, false), &x, &mut Memo::default());
    assert!(
        report
            .pending
            .iter()
            .any(|(s, why)| s == "authorizations" && why.contains("not listed")),
        "{report:?}"
    );
    assert!(x.public.bytes(layout::AUTHORIZATION_CURRENT).is_none());

    // Listed: published with public paths, byte for byte.
    let (report, _) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert!(report.authorization_written, "{report:?}");
    let public: publish::AuthorizationPointer = public_json(&x, layout::AUTHORIZATION_CURRENT);
    assert_eq!(public.kind, "cohort");
    for (obj, bytes) in [
        (&public.authorization, &auth_bytes),
        (public.renewal.as_ref().unwrap(), &renewal_bytes),
    ] {
        assert!(obj.path.starts_with("v1/authorizations/") && obj.path.ends_with(".signed"));
        assert_eq!(&x.public.bytes(&obj.path).unwrap(), bytes);
        assert_eq!(obj.sha256, object_sha256(bytes).hex());
        assert_eq!(obj.bytes, bytes.len() as u64);
    }
    // Idempotent.
    let (report, _) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert!(!report.authorization_written);

    // A renewal of another authorization is refused.
    let other = RewardIntentRenewal {
        authorization: slop_ninja_protocol::domain_hash("publisher-test/other", b""),
        checkpoint_generation: 2,
        issued_epoch: m.opened_epoch + 30,
        expires_epoch: m.opened_epoch + 70,
    };
    let r = store_authorization(&x, fx::signed(other).unwrap().to_bytes().unwrap());
    write_pointer(&x, "cohort", a, Some(r));
    let (report, _) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert!(
        report
            .refused
            .iter()
            .any(|(s, why)| s == "authorizations" && why.contains("renews another")),
        "{report:?}"
    );

    // The launch authorization needs no listed cohort.
    let x = buckets(&b);
    let launch = LaunchAuthorization {
        chain: m.chain.clone(),
        target: CalculationTargetV2::launch_burn(m.chain.clone())
            .digest()
            .unwrap(),
        checkpoint_generation: 1,
        issued_epoch: 10,
        expires_epoch: 50,
    };
    let a = store_authorization(&x, fx::signed(launch).unwrap().to_bytes().unwrap());
    write_pointer(&x, "launch", a, None);
    let (report, _) = run(&config(None, false), &x, &mut Memo::default());
    assert!(report.authorization_written, "{report:?}");
    // Signed by another key: refused.
    let x = buckets(&b);
    let forged = slop_ninja_protocol::sign(auth, &slop_ninja_protocol::fixtures::package_key())
        .unwrap()
        .to_bytes()
        .unwrap();
    let a = store_authorization(&x, forged);
    write_pointer(&x, "cohort", a, None);
    let (report, _) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert!(
        report.refused.iter().any(|(s, _)| s == "authorizations"),
        "{report:?}"
    );
}
