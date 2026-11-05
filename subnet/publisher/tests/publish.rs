//! End-to-end publisher runs over in-memory buckets: copies verify after the
//! copy, nothing private is published, refusals publish nothing, and
//! artifacts follow their rules.

mod common;

use common::*;
use slop_ninja_protocol::{CalculationTargetV2, object_sha256};
use slop_ninja_protocol::fixtures_reward_v1 as fx;
use slop_ninja_publisher::arena::Budget;
use slop_ninja_publisher::artifacts::StartRecord;
use slop_ninja_publisher::index::{ArtifactState, Publication};
use slop_ninja_publisher::layout;
use slop_ninja_publisher::leaderboard::LeaderboardDoc;
use slop_ninja_publisher::publish::{self, Memo, ReferenceDigests};
use slop_ninja_publisher::store::block_on;
use slop_ninja_publisher::summary::CohortSummary;
use slop_ninja_publisher::verify::{self, Options};

fn public_json<T: for<'d> serde::Deserialize<'d>>(x: &Buckets3, key: &str) -> T {
    serde_json::from_slice(
        &x.public
            .bytes(key)
            .unwrap_or_else(|| panic!("{key} missing")),
    )
    .unwrap()
}

/// A closed cohort is published as its allow-listed summary alone: no
/// signed record, index, component or receipt of the coordinator leaves the
/// private bucket, and no score value appears anywhere.
#[test]
fn publishes_a_closed_cohort_as_its_allow_listed_summary() {
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
    assert_eq!(entry.closed_epoch, b.c.closure.closed_epoch);
    assert_eq!(idx.active_target.digest, b.c.closure.target.unwrap().hex());

    // The cohort directory holds the summary and the publication record,
    // nothing else; the summary is the one the index and record name.
    let dir = format!("v1/cohorts/{}/", b.label);
    let mut held: Vec<String> = x
        .public
        .keys()
        .into_iter()
        .filter(|k| k.starts_with(&dir))
        .collect();
    held.sort();
    assert_eq!(
        held,
        vec![
            format!("{dir}publication.json"),
            format!("{dir}summary.json")
        ]
    );
    let summary_bytes = x.public.bytes(&format!("{dir}summary.json")).unwrap();
    let publication: Publication = public_json(&x, &layout::publication(&b.label));
    assert_eq!(publication.summary, object_sha256(&summary_bytes).hex());
    assert_eq!(entry.summary, publication.summary);
    let summary: CohortSummary = serde_json::from_slice(&summary_bytes).unwrap();
    let p = &b.c.package;
    assert_eq!(summary.entrants.len(), p.entrants.len());
    assert_eq!(
        summary.references.current.content,
        b.c.manifest.references.current.content.hex()
    );
    let result = b.result();
    let quantization = summary.quantization.as_ref().expect("a v3 summary carries the grid");
    assert_eq!(quantization.rule, "quant-v1");
    let w = result.width;
    assert_eq!(quantization.width, slop_ninja_publisher::summary::decimal(w, w));
    assert_eq!(
        summary.references.current.q.as_deref(),
        Some(slop_ninja_publisher::summary::decimal(result.current.q.get(), w).as_str())
    );
    for e in &summary.entrants {
        let s = p.entrants.iter().find(|s| s.identity.uid == e.uid).unwrap();
        let r = result.entrants.iter().find(|r| r.uid == e.uid).unwrap();
        assert_eq!(e.gate, slop_ninja_publisher::summary::gate_bit(&r.outcome));
        assert_eq!(e.copy_gate.as_deref() == Some("fail"), s.public_copy().is_some());
        assert_eq!(e.gate == "pass", s.gated());
        // The quantized score only, on the grid with the width's decimals.
        assert_eq!(e.q, r.outcome.q().map(|q| slop_ninja_publisher::summary::decimal(q.get(), w)));
        if let Some(q) = &e.q {
            slop_ninja_protocol::quant::parse_grid(q, w).unwrap();
        }
    }
    // No rank and no order by rank: the summary carries no `rank` field
    // and lists the entrants in roster order.
    let raw: serde_json::Value = serde_json::from_slice(&summary_bytes).unwrap();
    assert!(
        raw["entrants"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e.get("rank").is_none()),
        "no rank"
    );
    assert_eq!(
        summary.entrants.iter().map(|e| e.uid).collect::<Vec<_>>(),
        p.entrants
            .iter()
            .map(|e| e.identity.uid)
            .collect::<Vec<_>>(),
        "roster order"
    );

    // No private object, no score value, nothing outside the allow-list.
    for key in x.public.keys() {
        let bytes = x.public.bytes(&key).unwrap();
        slop_ninja_publisher::allowlist::check_object(&key, &bytes)
            .unwrap_or_else(|e| panic!("{e}"));
    }
    assert_no_score_values(&b, &x);

    // The trust roots are published byte for byte; the site and index exist.
    assert_eq!(
        x.public.bytes(layout::TRUST).unwrap(),
        slop_ninja_publisher::config::TRUST_FILE
    );
    let page = String::from_utf8(x.public.bytes(layout::SITE_INDEX).unwrap()).unwrap();
    assert!(page.contains(&b.label) && !page.contains("Brier utility"));
    // D12b: the testnet-only promotion label is on the page.
    assert!(page.contains("id=\"promotion-rule\"") && page.contains("without the lookup canary"));
    assert!(
        page.contains("mailto:security@slop.ninja"),
        "the security contact"
    );
    assert!(x.public.bytes(layout::SITE_STYLE).is_some());

    // A second run is a no-op.
    let puts = x.public.puts.get();
    let (report, _) = run(&cfg, &x, &mut Memo::default());
    assert!(report.published.is_empty() && report.refused.is_empty());
    assert_eq!(x.public.puts.get(), puts, "nothing rewritten");
}

/// No score the package carries (an entrant's, `R_k`'s or `R_k-1`'s, a probe
/// or near-copy statistic), at full precision or as the six-place decimal a
/// layout v1 leaderboard printed, appears in any public object.
fn assert_no_score_values(b: &Built, x: &Buckets3) {
    let p = &b.c.package;
    let mut values: Vec<String> = Vec::new();
    // Each exact score with its published q. The fixture's grid is fine and
    // its scores are round, so a q can equal its exact score's leading
    // digits: those digits are the q's, published by rule, and are skipped.
    let mut scores: Vec<(u64, Option<u64>)> = vec![(p.reference.score.get(), Some(p.quant.reference_q.get()))];
    scores.extend(p.predecessor.as_ref().map(|r| (r.score.get(), p.quant.predecessor_q.map(|q| q.get()))));
    scores.extend(p.entrants.iter().filter_map(|e| e.score.map(|s| (s.get(), e.q.map(|q| q.get())))));
    let micro = slop_ninja_protocol::SCORE_SCALE / 1_000_000;
    let published: std::collections::BTreeSet<u64> = scores.iter().filter_map(|(_, q)| q.map(|q| q / micro)).collect();
    let scores: Vec<u64> = scores
        .into_iter()
        .filter(|(s, _)| !published.contains(&(s / micro)))
        .map(|(s, _)| s)
        .collect();
    for raw in scores {
        values.push(raw.to_string());
        let scale = u128::from(slop_ninja_protocol::SCORE_SCALE);
        let raw = u128::from(raw);
        values.push(format!(
            "{}.{:06}",
            raw / scale,
            (raw % scale) / (scale / 1_000_000)
        ));
    }
    for e in &p.entrants {
        if let Some(c) = &e.public {
            for m in c.reference_probes.iter().chain(c.probe_match.iter()) {
                values.push(m.entrant_probe_score.get().to_string());
                values.push(m.public_probe_score.get().to_string());
            }
            values.push(c.current.rss_fwd.to_string());
        }
    }
    values.retain(|v| v.len() >= 6);
    for key in x.public.keys() {
        let hay = String::from_utf8_lossy(&x.public.bytes(&key).unwrap()).into_owned();
        for v in &values {
            assert!(!hay.contains(v.as_str()), "{key} carries score value {v}");
        }
    }
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
    // D12b: promotions of the testnet record run without the lookup canary,
    // labelled testnet-only in the leaderboard JSON.
    let rule = doc
        .promotion_rule
        .as_ref()
        .expect("a testnet record carries the label");
    assert!(!rule.lookup_canary);
    assert_eq!(rule.scope, "testnet-only");
    assert!(rule.note.contains("required before mainnet"));
    assert_eq!(doc.entrants.len(), p.entrants.len());
    // Rows in roster order, never rank order, and no rank anywhere.
    assert_eq!(
        doc.entrants
            .iter()
            .map(|e| e.entrant.uid)
            .collect::<Vec<_>>(),
        p.entrants
            .iter()
            .map(|e| e.identity.uid)
            .collect::<Vec<_>>()
    );
    let result = b.result();
    let ranked = slop_ninja_protocol::reward_v1::q_rank(&result.q_entrants());
    assert!(
        ranked.windows(2).any(|w| w[0] > w[1]),
        "the fixture's q-rank differs from its roster order, so the row order tests something"
    );
    let raw: serde_json::Value =
        serde_json::from_slice(&x.public.bytes(layout::LEADERBOARD).unwrap()).unwrap();
    assert!(
        raw["entrants"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e.get("rank").is_none())
    );
    let page = String::from_utf8(x.public.bytes(layout::SITE_INDEX).unwrap()).unwrap();
    assert!(!page.contains(">Rank<"), "no rank column on the page");
    let ev = slop_ninja_protocol::reward_v1::derive_units(&result).unwrap();
    assert_eq!(doc.quantization.as_ref().map(|q| q.rule.as_str()), Some("quant-v1"));
    // A cluster is named by its first member in roster order, never by its
    // head (its first member in q-rank), and every member carries that name.
    for c in &ev.clusters {
        let first = *c.members.iter().min().unwrap();
        for &m in &c.members {
            let row = doc
                .entrants
                .iter()
                .find(|r| r.entrant.uid == p.entrants[m].identity.uid)
                .unwrap();
            let label = row
                .entrant
                .cluster
                .as_ref()
                .expect("a member has a cluster");
            assert_eq!(label.uid, p.entrants[first].identity.uid);
        }
    }
    for row in &doc.entrants {
        let (i, e) = p
            .entrants
            .iter()
            .enumerate()
            .find(|(_, e)| e.identity.uid == row.entrant.uid)
            .unwrap();
        assert_eq!(row.entrant.gate == "pass", e.gated());
        // A band and a cluster for every paid entrant; the band is the
        // reward band's rank from the top, from 1.
        if e.units > 0 {
            assert!(
                row.entrant.cluster.is_some() && row.entrant.band.is_some(),
                "{row:?}"
            );
        }
        let placed = ev.clusters.iter().find(|c| c.members.contains(&i));
        assert_eq!(row.entrant.band, placed.map(|c| c.band_rank as u32 + 1));
        if row.entrant.gate != "pass" {
            assert!(row.entrant.band.is_none());
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
    // R_k and R_k-1 by identity only.
    assert_eq!(doc.references.index, b.c.manifest.references.index);
    assert_eq!(
        doc.references.current.artifact,
        b.c.manifest.references.current.artifact.hex()
    );
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
        for phrase in slop_ninja_publisher::redact::REFUSED_WORDING {
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
            key == layout::SITE_INDEX
                || key == layout::SITE_STYLE
                || key.starts_with("v1/")
                || key.starts_with("artifacts/v2/")
                || key.starts_with("blobs/sha256/"),
            "{key} outside the layout"
        );
    }
}

#[test]
fn a_tampered_record_is_refused_and_nothing_is_indexed() {
    let b = build(0);
    let x = buckets(&b);
    let key = format!("cohorts/{}/result.signed", b.label);
    let mut bytes = x.receipts.bytes(&key).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x40;
    x.receipts.insert(&key, bytes);
    let (report, idx) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert!(report.published.is_empty());
    assert_eq!(report.refused.len(), 1, "{report:?}");
    assert!(report.refused[0].1.starts_with("result.signed"));
    assert!(!idx.unwrap().contains(&b.label));
    assert!(
        x.public.keys().iter().all(|k| !k.contains(&b.label)),
        "nothing of the cohort copied"
    );
}

#[test]
fn an_incomplete_closure_waits_then_publishes() {
    let b = build(0);
    let x = buckets(&b);
    let key = format!("cohorts/{}/result.signed", b.label);
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

/// A record that is not a `reward-v2` manifest (a load or older cohort) is
/// outside the record, not an unverifiable manifest.
#[test]
fn a_record_of_another_kind_is_outside_the_record() {
    let b = build(0);
    let x = buckets(&b);
    let d = format!("cohorts/{}", b.label);
    let manifest = x.receipts.bytes(&format!("{d}/manifest.signed")).unwrap();
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
        "not a reward-v2 cohort manifest"
    );
    // Review 2026-11-05 (ops 3): only this build's decoding placed it
    // outside, so the placement is this build's. The same build keeps it...
    assert_eq!(memo.outside_record[&b.label].build, Some(publish::build_id()));
    x.receipts.insert(&format!("{d}/manifest.signed"), manifest);
    let (report, _) = run(&config(Some(0), false), &x, &mut memo);
    assert!(report.published.is_empty() && report.outside == 1, "{report:?}");
    // ...and a later build (one that reads the format) lists the cohort
    // instead of dropping it from the record for good.
    memo.outside_record.get_mut(&b.label).unwrap().build = Some("an-earlier-build".into());
    let (report, idx) = run(&config(Some(0), false), &x, &mut memo);
    assert_eq!(report.published, std::slice::from_ref(&b.label), "{report:?}");
    assert!(idx.unwrap().contains(&b.label));
    assert!(!memo.outside_record.contains_key(&b.label));
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

fn start_of(r: &StartRecord, uid: u16) -> &slop_ninja_publisher::artifacts::StartEntry {
    r.entrants.iter().find(|e| e.uid == uid).unwrap()
}

fn closure_key(b: &Built) -> String {
    format!("{}/{}", dir(&b.label), verify::CLOSURE)
}

/// Holds the cohort at scoring start: its closure is not written yet.
fn unclose(b: &Built, x: &Buckets3) -> Vec<u8> {
    let key = closure_key(b);
    let bytes = x.receipts.bytes(&key).unwrap();
    x.receipts.remove(&key);
    bytes
}

/// The direct uploads of entrant `uid`, as stored.
fn uploaded(b: &Built, uid: u16) -> Uploaded {
    let e = uid_of(b, uid);
    let spec = b.c.specs.iter().find(|s| fx::hotkey(s.tag) == e.identity.hotkey).unwrap();
    split(&fx::artifact_bytes(spec))
}

fn published_keys(x: &Buckets3) -> Vec<String> {
    x.public
        .keys()
        .into_iter()
        .filter(|k| k.starts_with("artifacts/") || k.starts_with("blobs/"))
        .collect()
}

/// Publication at scoring start, on a roster shaped like the rehearsal's
/// (cohort 8): before the cohort closes, every admitted entrant's header and
/// files are copied from the admission bucket to their digest keys, except
/// an operator hotkey's and an earlier manifest's `R_0` entered as an
/// ordinary entrant (with its identical copy), which are withheld; an
/// entrant whose file is not uploaded yet waits. No score-dependent rule
/// withholds anything (the scores do not exist yet). At closure the
/// publication record reports the same states, each published header
/// checked against the package's artifact digest, and copies nothing more.
#[test]
fn artifacts_are_published_at_scoring_start() {
    let b = build(0);
    let x = buckets(&b);
    let closure = unclose(&b, &x);
    let mut cfg = config(Some(0), true);
    // uid 19 is one of ours.
    cfg.operator_hotkeys.insert(uid_of(&b, 19).identity.hotkey);
    // uid 17's content was `R_0` in an earlier manifest (since replaced);
    // uid 22 is an identical copy of it.
    let mut memo = Memo::default();
    memo.references.insert(ReferenceDigests {
        content: uid_of(&b, 17).content.hex(),
        artifact: "00".repeat(32),
    });
    // uid 26's largest file is not uploaded yet.
    let cohort = b.c.manifest.cohort;
    let u26 = uploaded(&b, 26);
    let (late_sha, late_bytes) = u26.files.iter().max_by_key(|f| f.1.len()).unwrap().clone();
    let late_key = file_key(cohort, &uid_of(&b, 26).commitment, &late_sha);
    x.admission.remove(&late_key);

    let (report, idx) = run(&cfg, &x, &mut memo);
    assert!(report.published.is_empty(), "the cohort is not closed");
    assert!(idx.unwrap().cohorts.iter().all(|c| c.label != b.label));
    assert!(x.public.bytes(&layout::publication(&b.label)).is_none());
    let rec: StartRecord = public_json(&x, &layout::artifacts_record(&b.label));
    assert_eq!(rec.schema, slop_ninja_publisher::artifacts::ARTIFACTS_SCHEMA);
    assert_eq!(rec.copy, "stream");
    assert_eq!(rec.entrants.len(), b.c.manifest.roster.len());
    let withheld = [
        (17, publish::WITHHELD_INITIAL),
        (22, publish::WITHHELD_INITIAL),
        (19, publish::WITHHELD_OPERATOR),
    ];
    for (uid, why) in withheld {
        let s = start_of(&rec, uid);
        assert_eq!(s.state, ArtifactState::Withheld, "uid {uid}");
        assert_eq!(s.reason.as_deref(), Some(why), "uid {uid}");
        assert!(x.public.bytes(&layout::header(&uid_of(&b, uid).artifact)).is_none(), "uid {uid}");
    }
    let mut published = 0;
    for e in &b.c.package.entrants {
        let uid = e.identity.uid;
        if withheld.iter().any(|w| w.0 == uid) || uid == 26 {
            continue;
        }
        let s = start_of(&rec, uid);
        assert_eq!(s.state, ArtifactState::Published, "uid {uid}: {:?}", s.reason);
        assert_eq!(s.header.as_deref(), Some(e.artifact.hex().as_str()), "uid {uid}");
        assert_eq!(s.path.as_deref(), Some(layout::header(&e.artifact).as_str()));
        let u = uploaded(&b, uid);
        assert_eq!(x.public.bytes(&layout::header(&e.artifact)).unwrap(), u.header);
        for (sha, bytes) in &u.files {
            assert_eq!(&x.public.bytes(&layout::blob(sha)).unwrap(), bytes, "uid {uid}");
            let f = s.files.iter().find(|f| f.sha256 == sha.hex()).unwrap();
            assert_eq!((f.bytes, f.path.as_str(), f.check.as_str()), (bytes.len() as u64, layout::blob(sha).as_str(), "sha256"));
        }
        published += 1;
    }
    assert!(published >= 10, "{published}");
    let s26 = start_of(&rec, 26);
    assert_eq!(s26.state, ArtifactState::Pending);
    assert_eq!(s26.reason.as_deref(), Some(slop_ninja_publisher::artifacts::WAIT_FILE));
    assert!(x.public.bytes(&layout::header(&uid_of(&b, 26).artifact)).is_none(), "a header waits for its files");
    assert!(report.pending.iter().any(|(s, r)| s.contains("/artifact-") && r == slop_ninja_publisher::artifacts::WAIT_FILE), "{report:?}");
    assert!(!memo.artifacts_done.contains(&b.label));
    // Every public object is checked by R2 against its digest as it is
    // written (the store's stored checksum), never hashed on our side.
    for key in published_keys(&x) {
        assert!(x.public.objects.borrow()[&key].sha256.is_some(), "{key}");
    }

    // The upload lands: the next run publishes it, and the cohort is done.
    x.admission.insert_checked(&late_key, late_bytes, late_sha);
    let (report, _) = run(&cfg, &x, &mut memo);
    assert_eq!(report.artifacts_copied, 1, "{report:?}");
    let rec: StartRecord = public_json(&x, &layout::artifacts_record(&b.label));
    assert_eq!(start_of(&rec, 26).state, ArtifactState::Published);
    assert!(memo.artifacts_done.contains(&b.label));
    // Each run reported to the eval Worker: the decided entrants, and the
    // entrant whose file was absent as missing (the eval Worker audits it
    // and lists it for the miner's repair); the last report has every
    // entrant decided.
    let reports = x.reports.taken.borrow();
    let commitment_26 = start_of(&rec, 26).commitment.clone();
    assert!(
        reports[0].2.iter().any(|e| e.commitment == commitment_26 && e.state == "missing"),
        "{:?}",
        reports[0]
    );
    assert!(reports.iter().all(|r| r.1 == b.label), "each report names the cohort's label");
    let last = &reports.last().unwrap().2;
    assert_eq!(last.len(), rec.entrants.len());
    assert!(last.iter().all(|e| e.state == "published" || e.state == "withheld"));
    assert!(last.iter().any(|e| e.commitment == commitment_26 && e.state == "published"));
    drop(reports);
    let reads = x.admission.bytes_read.get();
    let (report, _) = run(&cfg, &x, &mut memo);
    assert_eq!(report.artifacts_copied, 0);
    assert_eq!(x.admission.bytes_read.get(), reads, "a decided cohort is not read again");

    // The cohort closes: its record reports what scoring start published,
    // checked against the package, and copies nothing.
    let before = published_keys(&x);
    x.receipts.insert(&closure_key(&b), closure);
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert_eq!(report.published, vec![b.label.clone()]);
    assert_eq!(report.artifacts_copied, 0);
    assert!(report.refused.is_empty(), "{report:?}");
    assert_eq!(published_keys(&x), before);
    let p: Publication = public_json(&x, &layout::publication(&b.label));
    for (uid, why) in withheld {
        assert_eq!(artifact_of(&p, uid).state, ArtifactState::Withheld, "uid {uid}");
        assert_eq!(artifact_of(&p, uid).reason.as_deref(), Some(why));
    }
    for a in p.artifacts.iter().filter(|a| a.state == ArtifactState::Published) {
        assert_eq!(a.path.as_deref(), Some(layout::header(&uid_of(&b, a.uid).artifact).as_str()));
    }
    let idx = idx.unwrap();
    let entry = entry_of(&idx, &b.label);
    assert_eq!((entry.artifacts_withheld, entry.artifacts_pending), (3, 0));
    let doc: LeaderboardDoc = public_json(&x, layout::LEADERBOARD);
    let row = doc.entrants.iter().find(|r| r.entrant.uid == 26).unwrap();
    assert_eq!(row.artifact.as_ref().unwrap().state, ArtifactState::Published);
    // The fixture manifest's own initial reference (`R_0`, the predecessor
    // of `R_1`) is recorded in the index for every later cohort.
    let idx: slop_ninja_publisher::index::CohortIndex = public_json(&x, layout::COHORT_INDEX);
    let initial = slop_ninja_publisher::index::initial_reference(&b.c.manifest.references).unwrap();
    assert!(idx.initial_references.iter().any(|r| r.content == initial.content.hex()));
}

/// Without `ARTIFACTS_FROM_EPOCH` nothing is copied, no scoring-start record is
/// written, and closed cohorts list their artifacts as pending (the static
/// checks still withhold what they can).
#[test]
fn without_publication_enabled_nothing_is_copied() {
    let b = build(0);
    let x = buckets(&b);
    let (report, _) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert_eq!(report.artifacts_copied, 0);
    assert!(published_keys(&x).is_empty());
    assert!(x.public.bytes(&layout::artifacts_record(&b.label)).is_none());
    assert_eq!(x.admission.bytes_read.get(), 0, "the admission bucket is not read");
    let p: Publication = public_json(&x, &layout::publication(&b.label));
    for a in &p.artifacts {
        match a.state {
            ArtifactState::Pending => assert_eq!(a.reason.as_deref(), Some(publish::WAIT_DISABLED)),
            ArtifactState::Withheld => {}
            ArtifactState::Published => panic!("uid {} published", a.uid),
        }
    }
}

/// A cohort before the public record is never published at scoring start.
#[test]
fn cohorts_before_the_record_are_not_published_at_scoring_start() {
    let b = build(0);
    let x = buckets(&b);
    unclose(&b, &x);
    let mut memo = Memo::default();
    let (report, _) = run(&config(Some(u64::MAX), true), &x, &mut memo);
    assert_eq!(report.artifacts_copied, 0);
    assert!(published_keys(&x).is_empty());
    assert!(memo.artifacts_done.contains(&b.label), "remembered, never read again");
}

/// Each integrity rule refuses with an alert and waits, and publishes
/// nothing of that entrant: a header object without R2's stored checksum (a
/// platform fault, never a pass), a file object whose stored digest or size
/// is not the header's, a header that declares more files than the spec's
/// table, and bytes R2 refuses against the digest during the stream.
#[test]
fn integrity_faults_are_refused_and_publish_nothing() {
    for fault in ["no_checksum", "file_digest", "file_size", "too_many_files", "tampered"] {
        let b = build(0);
        let x = buckets(&b);
        unclose(&b, &x);
        let cfg = config(Some(0), true);
        let e = uid_of(&b, 31).clone();
        let cohort = b.c.manifest.cohort;
        let u = uploaded(&b, 31);
        let hkey = header_key(cohort, &e.commitment);
        let (fsha, fbytes) = u.files[0].clone();
        let fkey = file_key(cohort, &e.commitment, &fsha);
        let want = match fault {
            "no_checksum" => {
                x.admission.insert(&hkey, u.header.clone());
                slop_ninja_publisher::artifacts::REFUSED_NO_CHECKSUM
            }
            "file_digest" => {
                x.admission.insert_checked(&fkey, fbytes.clone(), slop_ninja_protocol::Digest([1; 32]));
                slop_ninja_publisher::artifacts::REFUSED_FILE_MISMATCH
            }
            "file_size" => {
                let mut longer = fbytes.clone();
                longer.push(0);
                x.admission.insert_checked(&fkey, longer, fsha);
                slop_ninja_publisher::artifacts::REFUSED_FILE_MISMATCH
            }
            "too_many_files" => {
                let mut h = u.header.clone();
                let kind = u32::from_le_bytes(h[..4].try_into().unwrap()) as usize;
                let at = 4 + kind + 2;
                let contract = u32::from_le_bytes(h[at..at + 4].try_into().unwrap()) as usize;
                let count_at = at + 4 + contract;
                h[count_at..count_at + 4].copy_from_slice(&7u32.to_le_bytes());
                x.admission.insert_checked(&hkey, h, u.header_sha256);
                slop_ninja_publisher::artifacts::REFUSED_FILE_COUNT
            }
            _ => {
                x.admission.tamper_on_copy.borrow_mut().insert(fkey.clone());
                slop_ninja_publisher::artifacts::REFUSED_R2
            }
        };
        let (report, _) = run(&cfg, &x, &mut Memo::default());
        let rec: StartRecord = public_json(&x, &layout::artifacts_record(&b.label));
        let s = start_of(&rec, 31);
        assert_eq!(s.state, ArtifactState::Pending, "{fault}");
        assert_eq!(s.reason.as_deref(), Some(want), "{fault}");
        assert!(report.refused.iter().any(|(_, r)| r == want), "{fault}: {report:?}");
        assert!(x.public.bytes(&layout::header(&e.artifact)).is_none(), "{fault}");
        if fault != "tampered" {
            assert!(x.public.bytes(&layout::blob(&fsha)).is_none(), "{fault}");
        }
        // Other entrants are not held by it.
        assert_eq!(start_of(&rec, 35).state, ArtifactState::Published, "{fault}");
    }
}

/// The server-side copy (`ARTIFACT_COPY = "server"`, the default): each
/// object is copied inside R2 with the source etag pinned and the metadata
/// replaced; `CopyObject` keeps the etag but never the stored SHA-256
/// (qualification G7), so the copy is checked by its size and the source's
/// etag, even if a checksum showed up. Without the signer binding it waits;
/// a source replaced since its etag was read is not copied; R2's refusal
/// (403, or 501 like 403) is refused with an alert, never published.
#[test]
fn the_server_side_copy_is_checked_by_etag() {
    for carry in [true, false] {
        let b = build(0);
        let x = buckets(&b);
        unclose(&b, &x);
        let mut cfg = config(Some(0), true);
        cfg.artifact_copy = slop_ninja_publisher::config::CopyMode::Server;
        // No signer binding: everything waits.
        let (report, _) = run(&cfg, &x, &mut Memo::default());
        assert_eq!(report.artifacts_copied, 0);
        let rec: StartRecord = public_json(&x, &layout::artifacts_record(&b.label));
        assert_eq!(start_of(&rec, 31).reason.as_deref(), Some(slop_ninja_publisher::artifacts::WAIT_SIGNER));
        assert!(published_keys(&x).is_empty());

        let server = MemServerCopy {
            admission: &x.admission,
            public: &x.public,
            carry_checksum: carry,
            refuse: None,
            calls: std::cell::Cell::new(0),
        };
        let mut memo = Memo::default();
        let (report, _) = run_copying(&cfg, &x, &mut memo, &Budget::worker(), Some(&server));
        assert!(report.artifacts_copied >= 10, "{report:?}");
        assert!(server.calls.get() > 0);
        let rec: StartRecord = public_json(&x, &layout::artifacts_record(&b.label));
        assert_eq!(rec.copy, "server");
        let s = start_of(&rec, 31);
        assert_eq!(s.state, ArtifactState::Published);
        assert!(s.files.iter().all(|f| f.check == "etag"), "{carry}: {:?}", s.files);
        let u = uploaded(&b, 31);
        assert_eq!(x.public.bytes(&layout::header(&u.header_sha256)).unwrap(), u.header);
        assert!(memo.artifacts_done.contains(&b.label));
    }
    for status in [403u16, 501] {
        let b = build(0);
        let x = buckets(&b);
        unclose(&b, &x);
        let mut cfg = config(Some(0), true);
        cfg.artifact_copy = slop_ninja_publisher::config::CopyMode::Server;
        let server = MemServerCopy {
            admission: &x.admission,
            public: &x.public,
            carry_checksum: false,
            refuse: Some(status),
            calls: std::cell::Cell::new(0),
        };
        let (report, _) =
            run_copying(&cfg, &x, &mut Memo::default(), &Budget::worker(), Some(&server));
        assert_eq!(report.artifacts_copied, 0, "{status}");
        let want = format!("{} ({status})", slop_ninja_publisher::artifacts::REFUSED_SERVER_COPY);
        assert!(report.refused.iter().any(|(_, r)| *r == want), "{status}: {report:?}");
        let rec: StartRecord = public_json(&x, &layout::artifacts_record(&b.label));
        assert_eq!(start_of(&rec, 31).state, ArtifactState::Pending, "{status}");
        assert!(published_keys(&x).is_empty(), "{status}");
    }
}

/// At closure the published header must be the one the package binds: a
/// scoring-start record whose header for an entrant differs from the
/// package's artifact digest is refused with an alert, and the entrant is
/// left pending rather than reported published.
#[test]
fn the_closure_checks_the_published_header_against_the_package() {
    let b = build(0);
    let x = buckets(&b);
    let closure = unclose(&b, &x);
    let cfg = config(Some(0), true);
    let mut memo = Memo::default();
    run(&cfg, &x, &mut memo).1.unwrap();
    let key = layout::artifacts_record(&b.label);
    let mut rec: StartRecord = public_json(&x, &key);
    let hotkey = slop_ninja_publisher::ss58::encode(&uid_of(&b, 31).identity.hotkey);
    let e = rec.entrants.iter_mut().find(|e| e.hotkey == hotkey).unwrap();
    e.header = Some("ab".repeat(32));
    x.public.insert(&key, serde_json::to_vec(&rec).unwrap());
    x.receipts.insert(&closure_key(&b), closure);
    let (report, _) = run(&cfg, &x, &mut memo);
    assert_eq!(report.published, vec![b.label.clone()]);
    assert!(report.refused.iter().any(|(_, r)| r == publish::REFUSED_HEADER), "{report:?}");
    let p: Publication = public_json(&x, &layout::publication(&b.label));
    assert_eq!(artifact_of(&p, 31).state, ArtifactState::Pending);
    assert_eq!(artifact_of(&p, 35).state, ArtifactState::Published);
}

/// A pending artifact is checked again on every run: a digest listed
/// private, or an initial reference recorded in the public index, after
/// publication started withholds it for good, even once its upload lands;
/// the closed cohort's record follows.
#[test]
fn retries_repeat_the_static_checks() {
    for listed_in in ["config", "index"] {
        let b = build(0);
        let x = buckets(&b);
        let mut cfg = config(Some(0), true);
        let e = uid_of(&b, 26).clone();
        let hkey = header_key(b.c.manifest.cohort, &e.commitment);
        let saved = x.admission.bytes(&hkey).unwrap();
        x.admission.remove(&hkey);
        let mut memo = Memo::default();
        run(&cfg, &x, &mut memo).1.unwrap();
        let p: Publication = public_json(&x, &layout::publication(&b.label));
        assert_eq!(artifact_of(&p, 26).state, ArtifactState::Pending);
        assert_eq!(artifact_of(&p, 26).reason.as_deref(), Some(slop_ninja_publisher::artifacts::WAIT_HEADER));
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
        x.admission.insert_checked(&hkey, saved, e.artifact);
        let (report, _) = run(&cfg, &x, &mut memo);
        assert_eq!(report.artifacts_copied, 0, "{listed_in}");
        let p: Publication = public_json(&x, &layout::publication(&b.label));
        assert_eq!(artifact_of(&p, 26).state, ArtifactState::Withheld, "{listed_in}");
        assert!(x.public.bytes(&layout::header(&e.artifact)).is_none());
    }
}

/// The record's own initial reference is withheld under any entrant's name,
/// by content or by artifact digest, and so is a listed private digest.
#[test]
fn the_initial_reference_is_never_published() {
    let b = build(0);
    let x = buckets(&b);
    let cfg = config(Some(0), true);
    let opts = Options {
        keys: &cfg.keys,
        chain: &cfg.chain,
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
    let r = v.result.entrants.iter().find(|r| r.uid == 31).unwrap();
    let f = publish::EntrantFacts::of(r, &v);
    assert_eq!(publish::static_withhold(&f, &cfg, &refs), None);
    let mut by_content = f.clone();
    by_content.content = initial.content;
    assert_eq!(
        publish::static_withhold(&by_content, &cfg, &refs),
        Some(publish::WITHHELD_INITIAL)
    );
    let mut by_artifact = f.clone();
    by_artifact.artifact = initial.artifact;
    assert_eq!(
        publish::static_withhold(&by_artifact, &cfg, &refs),
        Some(publish::WITHHELD_INITIAL)
    );
    let mut listed = cfg.clone();
    listed.private_digests.insert(e.artifact);
    assert_eq!(
        publish::static_withhold(&f, &listed, &refs),
        Some(publish::WITHHELD_LISTED)
    );
}

/// The summary is a function of the public result alone: the publisher
/// never reads the private score package or the components (both made
/// unreadable here, and the cohort still publishes), and a package with
/// other exact scores, statistics and draws but the same result publishes
/// byte-identical public objects.
#[test]
fn the_publisher_never_reads_the_private_package_or_components() {
    let b = build(0);
    let cfg = config(Some(0), false);
    let x = buckets(&b);
    for rel in [verify::PACKAGE, verify::COMPONENTS] {
        let key = format!("{}/{rel}", dir(&b.label));
        assert!(x.receipts.bytes(&key).is_some(), "{rel} is stored");
        x.receipts.unavailable.borrow_mut().insert(key);
    }
    let (report, idx) = run(&cfg, &x, &mut Memo::default());
    assert_eq!(report.published, vec![b.label.clone()], "{report:?}");
    assert!(idx.unwrap().contains(&b.label));
    // Other private bytes behind the same result: every public byte is the
    // same as a run over the original objects.
    let y = buckets(&b);
    for rel in [verify::PACKAGE, verify::COMPONENTS] {
        y.receipts.insert(&format!("{}/{rel}", dir(&b.label)), b"another private object".to_vec());
    }
    run(&cfg, &y, &mut Memo::default()).1.unwrap();
    let public = |z: &Buckets3| {
        z.public
            .keys()
            .into_iter()
            .filter(|k| k != layout::STATUS)
            .map(|k| (k.clone(), z.public.bytes(&k)))
            .collect::<Vec<_>>()
    };
    let x2 = buckets(&b);
    run(&cfg, &x2, &mut Memo::default()).1.unwrap();
    assert_eq!(public(&y), public(&x2));
}

/// The summary's entrants are the result's, in roster order, whatever
/// order the q values put them in.
#[test]
fn the_summary_lists_the_result_in_roster_order() {
    let b = build(0);
    let x = buckets(&b);
    let cfg = config(Some(0), false);
    let opts = Options {
        keys: &cfg.keys,
        chain: &cfg.chain,
    };
    let v = block_on(verify::verify_cohort(
        &x.receipts,
        &dir(&b.label),
        &b.label,
        &opts,
        &Budget::worker(),
    ))
    .unwrap();
    assert_eq!(v.result, b.result());
    let entrants = slop_ninja_publisher::summary::entrants(&v);
    assert_eq!(
        entrants.iter().map(|e| e.uid).collect::<Vec<_>>(),
        v.result.entrants.iter().map(|e| e.uid).collect::<Vec<_>>()
    );
    let w = v.result.width;
    for (e, r) in entrants.iter().zip(&v.result.entrants) {
        assert_eq!(e.band, r.band_rank.map(|k| u32::from(k) + 1));
        assert_eq!(e.q, r.outcome.q().map(|q| slop_ninja_publisher::summary::decimal(q.get(), w)));
    }
    let summary = slop_ninja_publisher::summary::build(&v, slop_ninja_admission_wire::json::Chain::from_binding(&cfg.chain));
    assert_eq!(summary.quality.bands.len(), v.result.bands.len());
    assert_eq!(summary.quality.cutoff.is_some(), v.result.cutoff.is_some());
}

/// A result the closure does not name, or one off the manifest's roster, is
/// refused; nothing is published.
#[test]
fn a_result_the_closure_does_not_name_is_refused() {
    use slop_ninja_protocol::fixtures::package_key;
    let b = build(0);
    let x = buckets(&b);
    let mut other = b.result();
    other.revision += 1;
    x.receipts.insert(
        &format!("{}/{}", dir(&b.label), verify::RESULT),
        slop_ninja_protocol::sign(other, &package_key()).unwrap().to_bytes().unwrap(),
    );
    let (report, _) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert!(report.published.is_empty(), "{report:?}");
    assert!(x.public.bytes(&layout::join(&layout::cohort_dir(&b.label), "summary.json")).is_none());
}

/// Stores a signed authorization object in the receipts bucket at its
/// content-addressed key and returns its pointer entry.
fn target_digest(b: &Built) -> slop_ninja_protocol::Digest {
    use slop_ninja_protocol::Record;
    b.c.target.digest().unwrap()
}

/// A cohort authorization for the fixture cohort committing `target`, in the
/// private pointer.
fn point_at_target(x: &Buckets3, b: &Built, target: slop_ninja_protocol::Digest) {
    point_at(x, b, b.c.manifest.cohort, target);
}

/// [`point_at_target`] for cohort number `cohort` with the fixture's
/// manifest and package digests.
fn point_at(x: &Buckets3, b: &Built, cohort: u64, target: slop_ninja_protocol::Digest) {
    use slop_ninja_protocol::{Record, RewardIntentAuthorization};
    let m = &b.c.manifest;
    let auth = RewardIntentAuthorization {
        chain: m.chain.clone(),
        cohort,
        cohort_manifest: m.digest().unwrap(),
        score_package: b.c.package.digest().unwrap(),
        target,
        checkpoint_generation: 1,
        issued_epoch: m.opened_epoch + 16,
        renew_by_epoch: m.opened_epoch + 56,
    };
    let bytes = slop_ninja_protocol::fixtures_reward_v1::signed(auth)
        .unwrap()
        .to_bytes()
        .unwrap();
    let a = store_authorization(x, bytes);
    write_pointer(x, "cohort", a, None);
}

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
    write_pointer_revoked(x, kind, auth, renewal, None);
}

fn write_pointer_revoked(
    x: &Buckets3,
    kind: &str,
    auth: publish::ObjectRef,
    renewal: Option<publish::ObjectRef>,
    revocation: Option<publish::ObjectRef>,
) {
    write_pointer_row(x, kind, auth, renewal, revocation, None);
}

fn write_pointer_row(
    x: &Buckets3,
    kind: &str,
    auth: publish::ObjectRef,
    renewal: Option<publish::ObjectRef>,
    revocation: Option<publish::ObjectRef>,
    row: Option<publish::ObjectRef>,
) {
    let pointer = publish::AuthorizationPointer {
        schema: publish::AUTHORIZATION_SCHEMA.into(),
        chain: slop_ninja_admission_wire::json::Chain::from_binding(
            &slop_ninja_protocol::fixtures_reward_v1::chain(),
        ),
        kind: kind.into(),
        authorization: auth,
        renewal,
        revocations: revocation,
        row,
    };
    x.receipts.insert(
        layout::PRIVATE_AUTHORIZATION_POINTER,
        serde_json::to_vec_pretty(&pointer).unwrap(),
    );
}

/// Review fix round 2 (finding 2): the authorization pointer, the
/// coordinator's row and the revocation set are copied before, and
/// whatever happens in, the cohort phase: a public index this build
/// refuses (an unknown schema) fails the run's cohort phase, yet validators
/// still receive the new row and revocations.
#[test]
fn a_refused_cohort_phase_never_holds_the_row_or_the_revocations() {
    use slop_ninja_protocol::fixtures_reward_v1 as fx;
    use slop_ninja_protocol::{Record, RewardIntentAuthorization};
    let b = build(0);
    let m = &b.c.manifest;
    let auth = RewardIntentAuthorization {
        chain: m.chain.clone(),
        cohort: m.cohort,
        cohort_manifest: m.digest().unwrap(),
        score_package: b.c.package.digest().unwrap(),
        target: target_digest(&b),
        checkpoint_generation: 1,
        issued_epoch: m.opened_epoch + 16,
        renew_by_epoch: m.opened_epoch + 56,
    };
    let row = slop_ninja_protocol::CoordinatorRow {
        chain: m.chain.clone(),
        reward_rule: slop_ninja_protocol::RewardRule::RewardV2,
        source: slop_ninja_protocol::RowSource::Cohort {
            cohort: m.cohort,
            cohort_label: m.cohort_label.clone(),
            cohort_manifest: m.digest().unwrap(),
        },
        target: auth.target,
        result: Some(b.c.package.result),
        activated_epoch: Some(b.c.closure.closed_epoch),
        authorization: auth.digest().unwrap(),
        issued_epoch: auth.issued_epoch,
        epoch: b.c.closure.closed_epoch.max(auth.issued_epoch),
        registration_block: 77,
        owner_hotkey: slop_ninja_protocol::AccountId([3; 32]),
        entries: vec![slop_ninja_protocol::CoordinatorRowEntry {
            uid: 3,
            hotkey: slop_ninja_protocol::AccountId([3; 32]),
            registration_block: 5,
            weight: u16::MAX,
        }],
    };
    let x = buckets(&b);
    x.public.insert(
        layout::COHORT_INDEX,
        serde_json::to_vec(&serde_json::json!({"schema": "slop_ninja.public.cohort_index/v99"}))
            .unwrap(),
    );
    let a = store_authorization(&x, fx::signed(auth.clone()).unwrap().to_bytes().unwrap());
    let row_bytes = fx::signed(row).unwrap().to_bytes().unwrap();
    let w = store_authorization(&x, row_bytes.clone());
    let rv = store_authorization(&x, revocation_set_for(&auth));
    write_pointer_row(&x, "cohort", a, None, Some(rv), Some(w));
    let (report, idx) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert!(
        idx.unwrap_err().contains("unknown schema"),
        "the cohort phase is refused"
    );
    assert!(report.authorization_written, "{report:?}");
    let public: publish::AuthorizationPointer = public_json(&x, layout::AUTHORIZATION_CURRENT);
    assert_eq!(
        x.public.bytes(&public.row.unwrap().path).unwrap(),
        row_bytes
    );
    assert_eq!(
        x.public.bytes(&public.revocations.unwrap().path).unwrap(),
        revocation_set_for(&auth)
    );
}

/// The current authorization, its renewal and the coordinator's signed row
/// reach `v1/authorizations/current.json` (the validator fetcher's source)
/// as soon as they authenticate: validators commit the row and need no
/// cohort record, so nothing waits for the cohort's listing. A renewal of
/// another authorization, or a row of another authorization, is refused.
#[test]
fn authorizations_and_the_coordinator_row_are_published_as_they_verify() {
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
        target: target_digest(&b),
        checkpoint_generation: 1,
        issued_epoch: m.opened_epoch + 16,
        renew_by_epoch: m.opened_epoch + 56,
    };
    let renewal = RewardIntentRenewal {
        authorization: auth.digest().unwrap(),
        checkpoint_generation: 2,
        issued_epoch: m.opened_epoch + 30,
        renew_by_epoch: m.opened_epoch + 70,
    };
    let auth_bytes = fx::signed(auth.clone()).unwrap().to_bytes().unwrap();
    let renewal_bytes = fx::signed(renewal).unwrap().to_bytes().unwrap();

    // Nothing stored: nothing published, nothing raised.
    let x = buckets(&b);
    let (report, _) = run(&config(None, false), &x, &mut Memo::default());
    assert!(report.pending.is_empty() && report.refused.is_empty());
    assert!(x.public.bytes(layout::AUTHORIZATION_CURRENT).is_none());

    // The coordinator's row of a commitment under that authorization.
    let row = slop_ninja_protocol::CoordinatorRow {
        chain: m.chain.clone(),
        reward_rule: slop_ninja_protocol::RewardRule::RewardV2,
        source: slop_ninja_protocol::RowSource::Cohort {
            cohort: m.cohort,
            cohort_label: m.cohort_label.clone(),
            cohort_manifest: m.digest().unwrap(),
        },
        target: auth.target,
        result: Some(b.c.package.result),
        activated_epoch: Some(b.c.closure.closed_epoch),
        authorization: auth.digest().unwrap(),
        issued_epoch: auth.issued_epoch,
        epoch: b.c.closure.closed_epoch.max(auth.issued_epoch),
        registration_block: 77,
        owner_hotkey: slop_ninja_protocol::AccountId([3; 32]),
        entries: vec![slop_ninja_protocol::CoordinatorRowEntry {
            uid: 3,
            hotkey: slop_ninja_protocol::AccountId([3; 32]),
            registration_block: 5,
            weight: u16::MAX,
        }],
    };
    let row_bytes = fx::signed(row.clone()).unwrap().to_bytes().unwrap();

    // Stored: published with public paths, byte for byte, before (and
    // without) any cohort listing.
    let a = store_authorization(&x, auth_bytes.clone());
    let r = store_authorization(&x, renewal_bytes.clone());
    let w = store_authorization(&x, row_bytes.clone());
    write_pointer_row(&x, "cohort", a.clone(), Some(r.clone()), None, Some(w));
    let (report, _) = run(&config(None, false), &x, &mut Memo::default());
    assert!(report.authorization_written, "{report:?}");
    let public: publish::AuthorizationPointer = public_json(&x, layout::AUTHORIZATION_CURRENT);
    assert_eq!(public.kind, "cohort");
    for (obj, bytes) in [
        (&public.authorization, &auth_bytes),
        (public.renewal.as_ref().unwrap(), &renewal_bytes),
        (public.row.as_ref().unwrap(), &row_bytes),
    ] {
        assert!(obj.path.starts_with("v1/authorizations/") && obj.path.ends_with(".signed"));
        assert_eq!(&x.public.bytes(&obj.path).unwrap(), bytes);
        assert_eq!(obj.sha256, object_sha256(bytes).hex());
        assert_eq!(obj.bytes, bytes.len() as u64);
    }
    // Idempotent.
    let (report, _) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert!(!report.authorization_written);
    for key in x.public.keys() {
        slop_ninja_publisher::allowlist::check_object(&key, &x.public.bytes(&key).unwrap())
            .unwrap_or_else(|e| panic!("{e}"));
    }

    // A row of another authorization, or not signed by the authorization
    // key, is refused and left out of the public pointer, which is still
    // published (with its revocation set): validators keep their installed
    // row (review fix round 1, finding 4).
    let mut stray = row.clone();
    stray.authorization = slop_ninja_protocol::domain_hash("publisher-test/other", b"");
    let w = store_authorization(&x, fx::signed(stray).unwrap().to_bytes().unwrap());
    let rv = store_authorization(&x, revocation_set_for(&auth));
    write_pointer_row(&x, "cohort", a.clone(), None, Some(rv.clone()), Some(w));
    let (report, _) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert!(
        report
            .refused
            .iter()
            .any(|(s, why)| s == "authorizations row"
                && why.contains("another chain or authorization")),
        "{report:?}"
    );
    assert!(
        report.authorization_written && !report.refused.iter().any(|(s, _)| s == "authorizations"),
        "{report:?}"
    );
    let without: publish::AuthorizationPointer = public_json(&x, layout::AUTHORIZATION_CURRENT);
    assert!(without.row.is_none(), "the failed row is left out");
    assert_eq!(
        &x.public.bytes(&without.revocations.unwrap().path).unwrap(),
        &revocation_set_for(&auth)
    );
    // A named row that is not stored yet: the pointer is published without
    // it and the row is pending.
    let missing = publish::ObjectRef {
        path: rv.path.replace(&rv.sha256, &"ab".repeat(32)),
        sha256: "ab".repeat(32),
        bytes: 300,
    };
    write_pointer_row(&x, "cohort", a.clone(), None, None, Some(missing));
    let (report, _) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert!(
        report
            .pending
            .iter()
            .any(|(s, _)| s == "authorizations row"),
        "{report:?}"
    );
    assert!(
        public_json::<publish::AuthorizationPointer>(&x, layout::AUTHORIZATION_CURRENT)
            .row
            .is_none()
    );
    let forged = slop_ninja_protocol::sign(row, &slop_ninja_protocol::fixtures::package_key())
        .unwrap()
        .to_bytes()
        .unwrap();
    let w = store_authorization(&x, forged);
    write_pointer_row(&x, "cohort", a.clone(), None, None, Some(w));
    let (report, _) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert!(
        report
            .refused
            .iter()
            .any(|(s, why)| s == "authorizations row" && why.contains("coordinator row")),
        "{report:?}"
    );

    // A renewal of another authorization is refused.
    let other = RewardIntentRenewal {
        authorization: slop_ninja_protocol::domain_hash("publisher-test/other", b""),
        checkpoint_generation: 2,
        issued_epoch: m.opened_epoch + 30,
        renew_by_epoch: m.opened_epoch + 70,
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
        renew_by_epoch: 50,
    };
    let launch_bytes = fx::signed(launch.clone()).unwrap().to_bytes().unwrap();
    let a = store_authorization(&x, launch_bytes.clone());
    write_pointer(&x, "launch", a.clone(), None);
    let (report, _) = run(&config(None, false), &x, &mut Memo::default());
    assert!(report.authorization_written, "{report:?}");
    // The revocation set the pointer names is copied with it, byte for
    // byte, every entry checked; a set with an entry for another chain, or
    // that is not whole revocations, is refused.
    let revocation = slop_ninja_protocol::RewardIntentRevocation {
        chain: m.chain.clone(),
        target: launch.target,
        authorization: launch.digest().unwrap(),
        checkpoint_generation: 1,
        issued_epoch: 12,
    };
    let mut older = revocation.clone();
    older.target = slop_ninja_protocol::Digest([0x5a; 32]);
    let mut set = fx::signed(older).unwrap().to_bytes().unwrap();
    set.extend(fx::signed(revocation.clone()).unwrap().to_bytes().unwrap());
    let rv = store_authorization(&x, set.clone());
    write_pointer_revoked(&x, "launch", a.clone(), None, Some(rv));
    let (report, _) = run(&config(None, false), &x, &mut Memo::default());
    assert!(report.authorization_written, "{report:?}");
    let public: publish::AuthorizationPointer = public_json(&x, layout::AUTHORIZATION_CURRENT);
    assert_eq!(public.schema, "slop_ninja.public.authorization/v2");
    let obj = public.revocations.as_ref().unwrap();
    assert!(obj.path.starts_with("v1/authorizations/"));
    assert_eq!(&x.public.bytes(&obj.path).unwrap(), &set);
    assert_eq!(report.revocations_unopened, 0);
    // An entry signed under another key (a trust root rotated on the
    // coordinator's side first) is copied unchanged and counted for an
    // alert, never a refusal: validators skip it, and newer authorizations
    // keep reaching them.
    let mut other = revocation.clone();
    other.target = slop_ninja_protocol::Digest([0x6b; 32]);
    let mut rotated = set.clone();
    rotated.extend(
        slop_ninja_protocol::sign(other, &slop_ninja_protocol::fixtures::package_key())
            .unwrap()
            .to_bytes()
            .unwrap(),
    );
    let rv_rotated = store_authorization(&x, rotated.clone());
    write_pointer_revoked(&x, "launch", a.clone(), None, Some(rv_rotated));
    let (report, _) = run(&config(None, false), &x, &mut Memo::default());
    assert!(
        report.authorization_written && report.refused.is_empty(),
        "{report:?}"
    );
    assert_eq!(report.revocations_unopened, 1);
    let copied: publish::AuthorizationPointer = public_json(&x, layout::AUTHORIZATION_CURRENT);
    assert_eq!(
        &x.public.bytes(&copied.revocations.unwrap().path).unwrap(),
        &rotated
    );
    let rv = store_authorization(&x, set[1..].to_vec());
    write_pointer_revoked(&x, "launch", a.clone(), None, Some(rv));
    let (report, _) = run(&config(None, false), &x, &mut Memo::default());
    assert!(
        report
            .refused
            .iter()
            .any(|(s, why)| s == "authorizations" && why.contains("not whole revocations")),
        "{report:?}"
    );
    let mut foreign = revocation;
    foreign.chain.netuid += 1;
    let mut set = fx::signed(foreign).unwrap().to_bytes().unwrap();
    set.extend(&x.public.bytes(&obj.path).unwrap()[..set.len()]);
    let rv = store_authorization(&x, set);
    write_pointer_revoked(&x, "launch", a, None, Some(rv));
    let (report, _) = run(&config(None, false), &x, &mut Memo::default());
    assert!(
        report.refused.iter().any(|(s, why)| s == "authorizations" && why.contains("revocation names another chain")),
        "{report:?}"
    );
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

/// A chain-record restart (a new `RECORD_LABEL_PREFIX`) over a non-empty
/// index starts a fresh record: the previous index, leaderboard and
/// reference pointers move under `v1/records/<first label>/`, and the
/// restarted record starts again at `R_0`; it never appends to the earlier
/// one. A prefix every listed label already has is adopted instead.
#[test]
fn a_new_record_prefix_moves_the_previous_record_aside() {
    let b = build(0);
    let x = buckets(&b);
    // The live pointer and leaderboard as an earlier build wrote them, with
    // the retired wording public text no longer carries.
    let held_wording = format!(
        "{{\"withheld\": \"x {} y\"}}",
        slop_ninja_publisher::redact::REFUSED_WORDING[0]
    );
    x.public
        .insert(layout::LEADERBOARD, held_wording.clone().into_bytes());
    x.public
        .insert(layout::REFERENCE_CURRENT, held_wording.into_bytes());
    let mut cfg = config(Some(0), false);
    cfg.record_label_prefix = Some("testnet-seed-".into());
    let (report, idx) = run(&cfg, &x, &mut Memo::default());
    assert_eq!(report.record_restarted, None);
    let idx = idx.unwrap();
    assert_eq!(idx.record_prefix.as_deref(), Some("testnet-seed-"));
    assert_eq!(idx.cohorts.len(), 1);
    // The restarted chain's prefix: the listed record moves aside.
    cfg.record_label_prefix = Some(b.label.clone());
    let mut memo = Memo::default();
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert_eq!(report.record_restarted.as_deref(), Some(SEED_LABEL));
    let idx = idx.unwrap();
    assert!(idx.cohorts.is_empty() && idx.initial_references.is_empty());
    assert_eq!(idx.record_prefix.as_deref(), Some(b.label.as_str()));
    let archived: slop_ninja_publisher::index::CohortIndex = public_json(
        &x,
        &layout::record_archive(SEED_LABEL, "cohorts/index.json"),
    );
    assert_eq!(archived.cohorts[0].label, SEED_LABEL);
    // Only the index is archived: the leaderboard and pointers, with the
    // wording of the build that wrote them, are removed, never archived, and
    // no object under the archive carries refused wording.
    for rel in [
        "leaderboard.json",
        "reference/current.json",
        "reference/previous.json",
    ] {
        assert!(
            x.public
                .bytes(&layout::record_archive(SEED_LABEL, rel))
                .is_none(),
            "{rel} archived"
        );
    }
    let archive_dir = layout::record_archive(SEED_LABEL, "");
    let archived_keys: Vec<String> = x
        .public
        .keys()
        .into_iter()
        .filter(|k| k.starts_with(&archive_dir))
        .collect();
    assert_eq!(
        archived_keys,
        [layout::record_archive(SEED_LABEL, "cohorts/index.json")]
    );
    for k in x.public.keys() {
        let bytes = x.public.bytes(&k).unwrap();
        assert!(
            slop_ninja_publisher::redact::scan_wording(&bytes).is_ok(),
            "{k} carries refused wording"
        );
    }
    assert!(x.public.bytes(layout::LEADERBOARD).is_none());
    assert!(x.public.bytes(layout::REFERENCE_CURRENT).is_none());
    // The earlier record's initial references stay withheld in the new
    // record, whatever the memo holds.
    let first: slop_ninja_publisher::index::CohortIndex = archived.clone();
    assert_eq!(
        idx.previous_records[0].initial_references,
        first.initial_references
    );
    // The fixture cohort (R_1) waits for the restarted record's R_0.
    assert!(report.published.is_empty());
    assert!(
        report
            .pending
            .iter()
            .any(|(l, why)| *l == b.label && why.contains("first cohort")),
        "{report:?}"
    );
    // Later runs keep the fresh record, which records the earlier one.
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert_eq!(report.record_restarted, None);
    let idx = idx.unwrap();
    assert!(idx.cohorts.is_empty());
    assert_eq!(idx.previous_records.len(), 1);
    assert_eq!(
        idx.previous_records[0].prefix.as_deref(),
        Some("testnet-seed-")
    );
    assert_eq!(idx.previous_records[0].first_label, SEED_LABEL);
    // A deploy from a tree that still tracks the earlier record's prefix is
    // refused: nothing is moved, adopted or written, and the run reports it.
    let puts = x.public.puts.get();
    let mut stale = cfg.clone();
    stale.record_label_prefix = Some("testnet-seed-".into());
    let (report, idx) = run(&stale, &x, &mut memo);
    let e = idx.unwrap_err();
    assert!(e.contains("earlier public record"), "{e}");
    assert_eq!(report.record_restarted, None);
    assert_eq!(x.public.puts.get(), puts, "nothing written");
    let live: slop_ninja_publisher::index::CohortIndex = public_json(&x, layout::COHORT_INDEX);
    assert_eq!(live.record_prefix.as_deref(), Some(b.label.as_str()));
    // The pre-deploy check refuses the same tree, and a restart only to a new
    // prefix.
    use slop_ninja_publisher::index::record_check;
    assert!(record_check(&live, "testnet-seed-", false).is_err());
    assert!(record_check(&live, "testnet-seed-", true).is_err());
    assert!(record_check(&live, &b.label, true).is_err());
    assert_eq!(
        record_check(&live, &b.label, false),
        Ok(format!("same record: {}", b.label))
    );
    assert!(record_check(&live, "testnet-r0c-", false).is_err());
    assert!(record_check(&live, "testnet-r0c-", true).is_ok());
}

/// An archive object is written once: a record restart that would replace
/// an existing archive with other bytes is refused, and the live record is
/// left as it was.
#[test]
fn a_record_restart_never_replaces_an_earlier_archive() {
    let b = build(0);
    let x = buckets(&b);
    x.public
        .insert(layout::LEADERBOARD, b"{\"old\":true}".to_vec());
    let mut cfg = config(Some(0), false);
    cfg.record_label_prefix = Some("testnet-seed-".into());
    run(&cfg, &x, &mut Memo::default()).1.unwrap();
    let held = layout::record_archive(SEED_LABEL, "cohorts/index.json");
    x.public.insert(&held, b"{\"other\":true}".to_vec());
    cfg.record_label_prefix = Some("testnet-r0b-".into());
    let (report, idx) = run(&cfg, &x, &mut Memo::default());
    let e = idx.unwrap_err();
    assert!(e.contains("never replaced"), "{e}");
    assert_eq!(report.record_restarted, None);
    assert_eq!(x.public.bytes(&held).unwrap(), b"{\"other\":true}".to_vec());
    assert_eq!(
        x.public.bytes(layout::LEADERBOARD).unwrap(),
        b"{\"old\":true}".to_vec()
    );
    let live: slop_ninja_publisher::index::CohortIndex = public_json(&x, layout::COHORT_INDEX);
    assert_eq!(live.record_prefix.as_deref(), Some("testnet-seed-"));
    // The same bytes are an interrupted run repeating the step.
    let live_bytes = x.public.bytes(layout::COHORT_INDEX).unwrap();
    x.public.insert(&held, live_bytes);
    let (report, idx) = run(&cfg, &x, &mut Memo::default());
    assert_eq!(report.record_restarted.as_deref(), Some(SEED_LABEL));
    assert_eq!(idx.unwrap().record_prefix.as_deref(), Some("testnet-r0b-"));
}

/// A record archive is written once for good, so it is scanned for wording
/// public text never carries before it is written: an index carrying it
/// refuses the restart, nothing is archived, and the live record is left as
/// it was.
#[test]
fn a_record_archive_never_carries_refused_wording() {
    let b = build(0);
    let x = buckets(&b);
    let mut cfg = config(Some(0), false);
    cfg.record_label_prefix = Some("testnet-seed-".into());
    run(&cfg, &x, &mut Memo::default()).1.unwrap();
    let mut raw: serde_json::Value = public_json(&x, layout::COHORT_INDEX);
    raw["note"] = serde_json::json!(format!(
        "x {} y",
        slop_ninja_publisher::redact::REFUSED_WORDING[0]
    ));
    let tainted = serde_json::to_vec(&raw).unwrap();
    x.public.insert(layout::COHORT_INDEX, tainted.clone());
    cfg.record_label_prefix = Some("testnet-r0b-".into());
    let puts = x.public.puts.get();
    let (report, idx) = run(&cfg, &x, &mut Memo::default());
    let e = idx.unwrap_err();
    assert!(e.contains("refused wording"), "{e}");
    assert_eq!(report.record_restarted, None);
    assert_eq!(x.public.puts.get(), puts, "nothing written");
    assert!(
        x.public
            .bytes(&layout::record_archive(SEED_LABEL, "cohorts/index.json"))
            .is_none()
    );
    assert_eq!(x.public.bytes(layout::COHORT_INDEX).unwrap(), tainted);
}

/// A record restart keeps the earlier record's initial references withheld:
/// the new index records them with the earlier record, so they stay
/// withheld after the manifest chain stops naming them, with a fresh memo,
/// and across a second restart.
#[test]
fn a_record_restart_keeps_the_earlier_initial_references_withheld() {
    let b = build(0);
    let x = buckets(&b);
    let mut cfg = config(Some(0), false);
    let before = run(&cfg, &x, &mut Memo::default()).1.unwrap();
    assert!(!before.initial_references.is_empty());
    cfg.record_label_prefix = Some("testnet-r0b-".into());
    let after = run(&cfg, &x, &mut Memo::default()).1.unwrap();
    assert!(after.initial_references.is_empty(), "the new record's own");
    let withheld: Vec<_> = after.withheld_initial_references().cloned().collect();
    assert_eq!(withheld, before.initial_references);
    let again = run(&cfg, &x, &mut Memo::default()).1.unwrap();
    assert_eq!(
        again
            .withheld_initial_references()
            .cloned()
            .collect::<Vec<_>>(),
        before.initial_references
    );
    cfg.record_label_prefix = Some("testnet-r0c-".into());
    let third = run(&cfg, &x, &mut Memo::default()).1.unwrap();
    assert_eq!(
        third.previous_records.len(),
        1,
        "an empty record moves nothing aside"
    );
    assert_eq!(
        third
            .withheld_initial_references()
            .cloned()
            .collect::<Vec<_>>(),
        before.initial_references
    );
}

/// Receipts are coordinator-private: the publisher never reads them, so a
/// cohort whose detailed receipts the coordinator already retired under its
/// N+2 rule (marker or not) is still published as its summary.
#[test]
fn a_cohort_whose_receipts_are_retired_is_still_published() {
    let b = build(0);
    let x = buckets(&b);
    for k in x.receipts.keys() {
        if k.contains("/receipts/") || k.ends_with("receipt-index.signed") || k.ends_with("chunk-map.json") {
            x.receipts.remove(&k);
        }
    }
    let marker = serde_json::json!({
        "schema": publish::RETIRED_MARKER_SCHEMA, "label": b.label, "cohort": b.c.manifest.cohort,
        "rule": "receipts_after_two_later_completions_v2", "due_after_cohort": 9,
    });
    x.receipts.insert(
        &format!("cohorts/{}/{}", b.label, publish::RETIRED_MARKER),
        serde_json::to_vec(&marker).unwrap(),
    );
    let reads = x.receipts.bytes_read.get();
    let (report, idx) = run(&config(Some(0), false), &x, &mut Memo::default());
    assert_eq!(report.published, vec![b.label.clone()], "{report:?}");
    let idx = idx.unwrap();
    assert!(idx.contains(&b.label) && idx.retired_unpublished.is_empty());
    assert!(x.receipts.bytes_read.get() > reads);
    assert!(
        x.public
            .keys()
            .iter()
            .all(|k| !k.contains("receipt") && !k.ends_with("chunk-map.json"))
    );
}

/// A record whose first cohort (reference `R_0`) was retired before
/// publication still starts: later cohorts are listed after it, since it
/// will never be. Without such a retired start, a later cohort waits.
#[test]
fn the_record_starts_after_a_retired_first_cohort() {
    let b = build(0);
    assert_eq!(b.c.manifest.references.index, 1, "the fixture is R_1");
    let m = &b.c.manifest;
    for starts_record in [false, true] {
        let x = buckets_unseeded(&b);
        let mut idx = slop_ninja_publisher::index::CohortIndex::empty(
            &slop_ninja_protocol::fixtures_reward_v1::chain(),
        );
        idx.retired_unpublished
            .push(slop_ninja_publisher::index::RetiredCohort {
                label: "retired-start".into(),
                cohort: m.cohort - 1,
                opened_epoch: m.opened_epoch - 20,
                due_after_cohort: m.cohort + 1,
                starts_record,
            });
        x.public
            .insert(layout::COHORT_INDEX, serde_json::to_vec(&idx).unwrap());
        let (report, idx) = run(&config(Some(0), false), &x, &mut Memo::default());
        if starts_record {
            assert_eq!(report.published, vec![b.label.clone()], "{report:?}");
            assert!(idx.unwrap().contains(&b.label));
        } else {
            assert!(report.pending[0].1.contains("first cohort"), "{report:?}");
        }
    }
}

/// The fixture cohort N and N+1, both closed, in the private bucket, with a
/// public index whose record already started.
fn two_cohorts() -> (Built, Built, Buckets3) {
    let b = build(0);
    let next = build_at(b.c.manifest.cohort + 1);
    let x = buckets(&b);
    store(&next, &x.receipts, &x.admission);
    (b, next, x)
}

fn listed(idx: &slop_ninja_publisher::index::CohortIndex) -> Vec<String> {
    idx.cohorts.iter().map(|c| c.label.clone()).collect()
}

/// Cohorts are listed strictly in order: while N is incomplete or
/// unreadable for now (a store error on a cohort object, an object not
/// stored yet, a closure or manifest that does not answer during the scan),
/// N+1 is never listed before it, however many runs pass; once N answers,
/// N lists, then N+1.
#[test]
fn a_later_cohort_never_lists_before_an_earlier_one() {
    let cfg = config(Some(0), false);
    for (rel, how) in [
        (verify::RESULT, "unavailable"),
        (verify::RESULT, "missing"),
        (verify::CLOSURE, "unavailable"),
        (verify::MANIFEST, "unavailable"),
    ] {
        let (b, next, x) = two_cohorts();
        let key = format!("{}/{rel}", dir(&b.label));
        let saved = x.receipts.bytes(&key).unwrap();
        match how {
            "missing" => x.receipts.remove(&key),
            _ => {
                x.receipts.unavailable.borrow_mut().insert(key.clone());
            }
        }
        let mut memo = Memo::default();
        for _ in 0..3 {
            let (report, idx) = run(&cfg, &x, &mut memo);
            let idx = idx.unwrap();
            assert!(report.published.is_empty(), "{rel} {how}: {report:?}");
            assert!(report.refused.is_empty(), "{rel} {how}: {report:?}");
            assert!(
                report.pending.iter().any(|(s, _)| s == &b.label),
                "{rel} {how}: {report:?}"
            );
            assert_eq!(listed(&idx), vec![SEED_LABEL.to_owned()], "{rel} {how}");
            assert!(idx.refused.is_empty());
            assert!(x.public.bytes(&layout::publication(&next.label)).is_none());
        }
        x.receipts.unavailable.borrow_mut().clear();
        x.receipts.insert(&key, saved);
        let (report, _) = run(&cfg, &x, &mut memo);
        assert_eq!(report.published, vec![b.label.clone()], "{rel} {how}");
        let (report, idx) = run(&cfg, &x, &mut memo);
        assert_eq!(report.published, vec![next.label.clone()], "{rel} {how}");
        assert_eq!(
            listed(&idx.unwrap()),
            vec![SEED_LABEL.to_owned(), b.label.clone(), next.label.clone()]
        );
    }
}

/// A cohort the publisher refuses appears in the public index's `refused`
/// list (label, number, opening epoch, reason, build), and the next cohort
/// is listed after it with its authorization copied; no receipt deletion
/// point moves. A later build reconsiders it: once it verifies, it is
/// listed in its place by number and its refused entry goes.
#[test]
fn a_refused_cohort_is_indexed_and_the_next_one_follows() {
    let (b, next, x) = two_cohorts();
    let cfg = config(Some(0), false);
    let key = format!("{}/{}", dir(&b.label), verify::RESULT);
    let good = x.receipts.bytes(&key).unwrap();
    let mut bad = good.clone();
    let mid = bad.len() / 2;
    bad[mid] ^= 0x40;
    x.receipts.insert(&key, bad);
    point_at_target(&x, &next, target_digest(&next));
    let mut memo = Memo::default();
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert!(
        report.refused.iter().any(|(s, _)| s == &b.label),
        "{report:?}"
    );
    let idx = idx.unwrap();
    assert_eq!(listed(&idx), vec![SEED_LABEL.to_owned()]);
    let want = slop_ninja_publisher::index::RefusedCohort {
        label: b.label.clone(),
        cohort: Some(b.c.manifest.cohort),
        opened_epoch: Some(b.c.manifest.opened_epoch),
        starts_record: publish::starts_record(&b.c.manifest.references),
        reason: "result.signed: does not verify".into(),
        build: publish::build_id(),
    };
    // The public entry names the object and the kind of failure only; the
    // alert keeps the full reason.
    assert!(
        memo.refused[&b.label]
            .reason
            .starts_with("result.signed: ")
    );
    assert_ne!(memo.refused[&b.label].reason, want.reason);
    assert_eq!(idx.refused, vec![want.clone()]);
    let public: slop_ninja_publisher::index::CohortIndex = public_json(&x, layout::COHORT_INDEX);
    assert_eq!(public.refused, vec![want.clone()]);
    // The next run lists N+1 after the refused N. Its authorization is
    // already public: under the copy model it is copied as soon as it
    // verifies, without waiting for the listing.
    assert!(x.public.bytes(layout::AUTHORIZATION_CURRENT).is_some());
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert_eq!(report.published, vec![next.label.clone()], "{report:?}");
    let idx = idx.unwrap();
    assert_eq!(
        listed(&idx),
        vec![SEED_LABEL.to_owned(), next.label.clone()]
    );
    assert_eq!(idx.refused, vec![want.clone()], "recorded once");
    assert_eq!(idx.active_target.cohort, Some(next.c.manifest.cohort));
    // Still refused by this build: not reconsidered, the entry unchanged.
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert!(report.published.is_empty(), "{report:?}");
    assert_eq!(idx.unwrap().refused, vec![want]);
    // A later build (simulated: the memory names another) after the source
    // was corrected: listed in its place, the entry dropped.
    x.receipts.insert(&key, good);
    memo.refused.get_mut(&b.label).unwrap().build = "an-earlier-build".into();
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert_eq!(report.published, vec![b.label.clone()], "{report:?}");
    let idx = idx.unwrap();
    assert_eq!(
        listed(&idx),
        vec![SEED_LABEL.to_owned(), b.label.clone(), next.label.clone()]
    );
    assert!(idx.refused.is_empty());
    assert_eq!(idx.active_target.cohort, Some(next.c.manifest.cohort));
}

/// A closed cohort numbered before the record's first listed cohort (it
/// waited while a later one started the record) is outside the record:
/// never listed after it.
#[test]
fn a_cohort_before_the_records_first_listed_one_stays_outside() {
    let b = build(0);
    let x = buckets(&b);
    let mut idx: slop_ninja_publisher::index::CohortIndex = public_json(&x, layout::COHORT_INDEX);
    idx.cohorts[0].cohort = b.c.manifest.cohort + 1;
    idx.cohorts[0].opened_epoch = b.c.manifest.opened_epoch + 20;
    x.public
        .insert(layout::COHORT_INDEX, serde_json::to_vec(&idx).unwrap());
    let mut memo = Memo::default();
    let (report, idx) = run(&config(Some(0), false), &x, &mut memo);
    assert!(
        report.published.is_empty() && report.refused.is_empty() && report.pending.is_empty(),
        "{report:?}"
    );
    assert!(!idx.unwrap().contains(&b.label));
    assert!(memo.outside_record[&b.label].reason.contains("precedes"));
}

/// Corrupts cohort `b`'s signed package in the private bucket; returns the
/// good bytes.
fn tamper_package(x: &Buckets3, b: &Built) -> Vec<u8> {
    let key = format!("{}/{}", dir(&b.label), verify::RESULT);
    let good = x.receipts.bytes(&key).unwrap();
    let mut bad = good.clone();
    let mid = bad.len() / 2;
    bad[mid] ^= 0x40;
    x.receipts.insert(&key, bad);
    good
}

/// A packaged cohort whose closure the coordinator has not written yet (its
/// closure paused or retrying while a later cohort closes) holds the later
/// cohort, however many runs pass: N+1 is listed only after N's closure is
/// written and N is listed. The coordinator's abandonment record beside a
/// packaged cohort without a closure releases the hold; a cohort without a
/// package holds nothing.
#[test]
fn a_packaged_cohort_without_its_closure_holds_later_ones() {
    let cfg = config(Some(0), false);
    let (b, next, x) = two_cohorts();
    let key = format!("{}/{}", dir(&b.label), verify::CLOSURE);
    let closure = x.receipts.bytes(&key).unwrap();
    x.receipts.remove(&key);
    let mut memo = Memo::default();
    for _ in 0..3 {
        let (report, idx) = run(&cfg, &x, &mut memo);
        let idx = idx.unwrap();
        assert!(report.published.is_empty(), "{report:?}");
        assert!(report.refused.is_empty(), "{report:?}");
        assert!(
            report.pending.iter().any(|(s, why)| s == &next.label
                && why.contains(&format!("cohort {}", b.c.manifest.cohort))),
            "{report:?}"
        );
        assert_eq!(listed(&idx), vec![SEED_LABEL.to_owned()]);
        assert!(x.public.bytes(&layout::publication(&next.label)).is_none());
    }
    x.receipts.insert(&key, closure);
    let (report, _) = run(&cfg, &x, &mut memo);
    assert_eq!(report.published, vec![b.label.clone()], "{report:?}");
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert_eq!(report.published, vec![next.label.clone()], "{report:?}");
    assert_eq!(
        listed(&idx.unwrap()),
        vec![SEED_LABEL.to_owned(), b.label.clone(), next.label.clone()]
    );
    // Abandoned (its signed record beside it): no closure follows, N+1 is
    // listed. A record that does not verify (unsigned bytes, another
    // label, another chain) holds as if absent.
    let (b, next, x) = two_cohorts();
    x.receipts
        .remove(&format!("{}/{}", dir(&b.label), verify::CLOSURE));
    let rec = format!("{}/{}", dir(&b.label), publish::ABANDONMENT);
    let record = |label: &str, chain: slop_ninja_protocol::ChainBinding| {
        let a = slop_ninja_protocol::submission::CohortAbandonment {
            chain,
            cohort: b.c.manifest.cohort,
            cohort_label: label.to_owned(),
            cohort_manifest: None,
            fault: "generation_unavailable".into(),
            reason: "drill".into(),
            abandoned_epoch: Some(1),
        };
        slop_ninja_protocol::sign(a, &slop_ninja_protocol::fixtures::package_key())
            .unwrap()
            .to_bytes()
            .unwrap()
    };
    let mut other_chain = cfg.chain.clone();
    other_chain.netuid += 1;
    let mut memo = Memo::default();
    for bad in [
        vec![1],
        record("testnet-other", cfg.chain.clone()),
        record(&b.label, other_chain),
    ] {
        x.receipts.insert(&rec, bad);
        let (report, _) = run(&cfg, &x, &mut memo);
        assert!(report.published.is_empty(), "{report:?}");
    }
    x.receipts.insert(&rec, record(&b.label, cfg.chain.clone()));
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert_eq!(report.published, vec![next.label.clone()], "{report:?}");
    assert!(!idx.unwrap().contains(&b.label));
    // Abandoned with no abandonment record (none is written yet), but the
    // coordinator's retirement marker beside it (written only after a
    // closure or an abandonment): no closure follows, N+1 is listed. Before
    // the marker it holds; a marker that does not answer or parse holds.
    let (b, next, x) = two_cohorts();
    x.receipts
        .remove(&format!("{}/{}", dir(&b.label), verify::CLOSURE));
    let mut memo = Memo::default();
    let (report, _) = run(&cfg, &x, &mut memo);
    assert!(report.published.is_empty(), "{report:?}");
    let marker = format!("{}/{}", dir(&b.label), publish::RETIRED_MARKER);
    x.receipts.insert(&marker, b"not json".to_vec());
    let (report, _) = run(&cfg, &x, &mut memo);
    assert!(report.published.is_empty(), "{report:?}");
    assert!(
        report.pending.iter().any(|(s, _)| s == &b.label),
        "{report:?}"
    );
    write_marker(&x, &b.label, b.c.manifest.cohort, b.c.manifest.cohort + 2);
    x.receipts.unavailable.borrow_mut().insert(marker.clone());
    let (report, _) = run(&cfg, &x, &mut memo);
    assert!(report.published.is_empty(), "{report:?}");
    x.receipts.unavailable.borrow_mut().clear();
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert_eq!(report.published, vec![next.label.clone()], "{report:?}");
    let idx = idx.unwrap();
    assert!(!idx.contains(&b.label));
    assert_eq!(
        listed(&idx),
        vec![SEED_LABEL.to_owned(), next.label.clone()]
    );
    // Not packaged (never reaches a closure, as the fixture's never-closed
    // cohort): holds nothing.
    let (b, next, x) = two_cohorts();
    for rel in [verify::CLOSURE, verify::RESULT] {
        x.receipts.remove(&format!("{}/{rel}", dir(&b.label)));
    }
    let (report, _) = run(&cfg, &x, &mut Memo::default());
    assert_eq!(report.published, vec![next.label.clone()], "{report:?}");
}

/// The record's first cohort (`R_0`, no predecessor) refused, the next `R_0`
/// cohort starts the record; a later build that accepts the first lists it
/// in its place, before the next, and its refused entry goes.
#[test]
fn a_refused_first_cohort_is_listed_in_its_place_by_a_later_build() {
    let first = build_r0_at(3);
    let second = build_r0_at(4);
    assert_eq!(first.c.manifest.references.index, 0);
    assert!(first.c.manifest.references.predecessor.is_none());
    let x = buckets_unseeded(&first);
    store(&second, &x.receipts, &x.admission);
    let good = tamper_package(&x, &first);
    let cfg = config(Some(0), false);
    let mut memo = Memo::default();
    let (report, _) = run(&cfg, &x, &mut memo);
    assert!(
        report.refused.iter().any(|(s, _)| s == &first.label),
        "{report:?}"
    );
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert_eq!(report.published, vec![second.label.clone()], "{report:?}");
    let idx = idx.unwrap();
    assert_eq!(listed(&idx), vec![second.label.clone()]);
    assert_eq!(idx.refused.len(), 1);
    x.receipts
        .insert(&format!("{}/{}", dir(&first.label), verify::RESULT), good);
    memo.refused.get_mut(&first.label).unwrap().build = "an-earlier-build".into();
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert_eq!(report.published, vec![first.label.clone()], "{report:?}");
    let idx = idx.unwrap();
    assert_eq!(
        listed(&idx),
        vec![first.label.clone(), second.label.clone()]
    );
    assert!(idx.refused.is_empty(), "{idx:?}");
    assert!(!memo.outside_record.contains_key(&first.label));
    let public: slop_ninja_publisher::index::CohortIndex = public_json(&x, layout::COHORT_INDEX);
    assert!(public.refused.is_empty());
}

/// A record restart under the same build leaves the earlier record's
/// refusals out of the new record: its public `refused` list and its
/// completions stay empty.
#[test]
fn an_earlier_records_refusals_stay_out_of_a_restarted_record() {
    let b = build(0);
    let x = buckets(&b);
    tamper_package(&x, &b);
    let mut cfg = config(Some(0), false);
    let mut memo = Memo::default();
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert!(
        report.refused.iter().any(|(s, _)| s == &b.label),
        "{report:?}"
    );
    assert_eq!(idx.unwrap().refused.len(), 1);
    cfg.record_label_prefix = Some("testnet-r0b-".into());
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert_eq!(report.record_restarted.as_deref(), Some(SEED_LABEL));
    let idx = idx.unwrap();
    assert!(idx.refused.is_empty(), "{idx:?}");
    assert!(idx.completions().is_empty());
    assert!(memo.refused.is_empty());
    let public: slop_ninja_publisher::index::CohortIndex = public_json(&x, layout::COHORT_INDEX);
    assert!(public.refused.is_empty());
    let (_, idx) = run(&cfg, &x, &mut memo);
    assert!(idx.unwrap().refused.is_empty());
}

/// The record's first cohort (`R_0`, no predecessor) refused while the next
/// carries a promoted reference (index 1): the refusal starts the record, so
/// the next is listed after it instead of waiting for a new build; a later
/// build that accepts the refused cohort lists it in its place.
#[test]
fn a_refused_first_cohort_starts_the_record_for_a_promoted_successor() {
    let first = build_r0_at(3);
    let second = build_at(4);
    assert!(publish::starts_record(&first.c.manifest.references));
    assert!(!publish::starts_record(&second.c.manifest.references));
    let x = buckets_unseeded(&first);
    store(&second, &x.receipts, &x.admission);
    let good = tamper_package(&x, &first);
    let cfg = config(Some(0), false);
    let mut memo = Memo::default();
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert!(
        report.refused.iter().any(|(s, _)| s == &first.label),
        "{report:?}"
    );
    let idx = idx.unwrap();
    assert!(memo.refused[&first.label].starts_record);
    assert_eq!(idx.refused.len(), 1);
    assert!(idx.refused[0].starts_record);
    let public: slop_ninja_publisher::index::CohortIndex = public_json(&x, layout::COHORT_INDEX);
    assert!(public.refused[0].starts_record);
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert_eq!(report.published, vec![second.label.clone()], "{report:?}");
    assert_eq!(listed(&idx.unwrap()), vec![second.label.clone()]);
    x.receipts
        .insert(&format!("{}/{}", dir(&first.label), verify::RESULT), good);
    memo.refused.get_mut(&first.label).unwrap().build = "an-earlier-build".into();
    let (report, idx) = run(&cfg, &x, &mut memo);
    assert_eq!(report.published, vec![first.label.clone()], "{report:?}");
    let idx = idx.unwrap();
    assert_eq!(
        listed(&idx),
        vec![first.label.clone(), second.label.clone()]
    );
    assert!(idx.refused.is_empty(), "{idx:?}");
}

/// A refused cohort that does not start the record (or whose manifest did
/// not open) does not start it: a later cohort with a promoted reference
/// still waits for the record's first cohort.
#[test]
fn a_refused_cohort_that_cannot_start_the_record_does_not_start_it() {
    let first = build_at(3);
    let second = build_at(4);
    let x = buckets_unseeded(&first);
    store(&second, &x.receipts, &x.admission);
    tamper_package(&x, &first);
    let cfg = config(Some(0), false);
    let mut memo = Memo::default();
    for _ in 0..3 {
        let (report, idx) = run(&cfg, &x, &mut memo);
        assert!(report.published.is_empty(), "{report:?}");
        assert!(listed(&idx.unwrap()).is_empty());
    }
    let (report, _) = run(&cfg, &x, &mut memo);
    assert!(
        report
            .pending
            .iter()
            .any(|(s, why)| s == &second.label && why.contains("record's first cohort")),
        "{report:?}"
    );
}

/// A revocation set of one revocation of `auth`'s target.
fn revocation_set_for(auth: &slop_ninja_protocol::RewardIntentAuthorization) -> Vec<u8> {
    let revocation = slop_ninja_protocol::RewardIntentRevocation {
        chain: auth.chain.clone(),
        target: auth.target,
        authorization: slop_ninja_protocol::Record::digest(auth).unwrap(),
        checkpoint_generation: auth.checkpoint_generation,
        issued_epoch: auth.issued_epoch + 1,
    };
    slop_ninja_protocol::fixtures_reward_v1::signed(revocation)
        .unwrap()
        .to_bytes()
        .unwrap()
}

/// Review fix round 1 (findings 12, 14): the first run over a layout-v1
/// index removes what v1 wrote that v2 never rewrites (the leaderboard and
/// reference pointers with their score fields, the archived record index of
/// an earlier record) and rewrites the page without a leaderboard before the
/// v2 index is written; the earlier record keeps its index entry. Every key
/// left passes the allow-list's content check, the page included.
#[test]
fn a_v1_index_migration_removes_what_v1_wrote_and_rewrites_the_page() {
    let b = build(0);
    let x = buckets(&b);
    let chain = slop_ninja_admission_wire::json::Chain::from_binding(&b.c.manifest.chain);
    let v1 = serde_json::json!({
        "schema": slop_ninja_publisher::index::INDEX_SCHEMA_V1,
        "network": "testnet",
        "chain": chain,
        "cohorts": [{"cohort": 1, "label": "old-c1", "receipt_count": 9}],
        "previous_records": [{"prefix": "older-", "first_label": "older-c1"}],
    });
    x.public
        .insert(layout::COHORT_INDEX, serde_json::to_vec(&v1).unwrap());
    let scored = serde_json::json!({"schema": "slop_ninja.public.leaderboard/v1", "entrants": [{"brier_utility": 0.1, "credit": 3}]});
    x.public
        .insert(layout::LEADERBOARD, serde_json::to_vec(&scored).unwrap());
    x.public
        .insert(layout::REFERENCE_CURRENT, b"{\"schema\":\"old\"}".to_vec());
    x.public
        .insert(layout::REFERENCE_PREVIOUS, b"{\"schema\":\"old\"}".to_vec());
    let archive = layout::record_archive("older-c1", "cohorts/index.json");
    x.public.insert(&archive, serde_json::to_vec(&v1).unwrap());
    x.public.insert(
        layout::SITE_INDEX,
        b"<th>Brier utility</th><th>Credit</th><th>Units</th>".to_vec(),
    );
    let (report, idx) = run(&config(None, false), &x, &mut Memo::default());
    let idx = idx.unwrap();
    assert!(report.index_migrated, "{report:?}");
    assert_eq!(
        idx.previous_records.len(),
        1,
        "the earlier record keeps its entry"
    );
    for key in [
        layout::LEADERBOARD,
        layout::REFERENCE_CURRENT,
        layout::REFERENCE_PREVIOUS,
        archive.as_str(),
    ] {
        assert!(x.public.bytes(key).is_none(), "{key} removed");
    }
    let page = String::from_utf8(x.public.bytes(layout::SITE_INDEX).unwrap()).unwrap();
    assert_eq!(page, slop_ninja_publisher::leaderboard::render(None));
    for key in ["Brier utility", "Credit", "Units"] {
        assert!(!page.contains(key), "{key}");
    }
    for key in x.public.keys() {
        slop_ninja_publisher::allowlist::check_object(&key, &x.public.bytes(&key).unwrap())
            .unwrap_or_else(|e| panic!("{e}"));
    }
}

/// Review fix round 1 (finding 13): the check every public write runs
/// refuses a document with a field outside its list, and that refusal is
/// not retried as a transient store failure (it is reported as refused).
#[test]
fn every_public_write_runs_the_allow_list_check() {
    let mut doc = serde_json::json!({
        "schema": slop_ninja_publisher::index::REFERENCE_SCHEMA,
        "role": "current",
        "score": 0.5,
    });
    let bytes = serde_json::to_vec(&doc).unwrap();
    assert!(
        slop_ninja_publisher::allowlist::check_object(layout::REFERENCE_CURRENT, &bytes).is_err()
    );
    doc.as_object_mut().unwrap().remove("score");
    let bytes = serde_json::to_vec(&doc).unwrap();
    slop_ninja_publisher::allowlist::check_object(layout::REFERENCE_CURRENT, &bytes).unwrap();
    assert!(
        !slop_ninja_publisher::verify::Refusal::Store(
            "k".into(),
            slop_ninja_publisher::store::StoreError::NotAllowed
        )
        .retryable()
    );
}

/// A cohort whose every entrant is decided at scoring start counts as done
/// only once the eval Worker took the publication report (it records the
/// publication, which later ends the admitted objects' retention); while
/// the report is refused, every run sends it again.
#[test]
fn scoring_start_publication_is_done_only_once_the_eval_worker_took_the_report() {
    let b = build(0);
    let x = buckets(&b);
    let _closure = unclose(&b, &x);
    let cfg = config(Some(0), true);
    let mut memo = Memo::default();
    x.reports.refuse.set(true);
    let (report, _) = run(&cfg, &x, &mut memo);
    assert!(!memo.artifacts_done.contains(&b.label));
    assert!(
        report.pending.iter().any(|(_, r)| r == slop_ninja_publisher::artifacts::WAIT_REPORT),
        "{report:?}"
    );
    assert!(x.reports.taken.borrow().is_empty());
    x.reports.refuse.set(false);
    let (report, _) = run(&cfg, &x, &mut memo);
    assert_eq!(report.artifacts_copied, 0, "the copies were made by the first run");
    assert!(memo.artifacts_done.contains(&b.label));
    let taken = x.reports.taken.borrow();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].0, b.c.manifest.cohort);
    assert_eq!(taken[0].1, b.label);
    assert_eq!(taken[0].2.len(), b.c.manifest.roster.len());
}
