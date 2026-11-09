use super::*;
use crate::eval::plan::tests::{artifact, identity};
use crate::eval::receipts::{self, ChunkFacts, TaskOutcome};
use serde_json::json;

pub fn block(anchor: ArtifactJson) -> LocalizationPlan {
    LocalizationPlan {
        rule: se::GATE_RULE.into(),
        eval_rule: se::EVAL_RULE.into(),
        decoder_rule: se::DECODER_RULE.into(),
        floor: 24,
        mode: MODE_REPORT.into(),
        anchor,
        anchor_content: "ab".repeat(32),
        margins: Some(MarginsPlan { f1_ppb: se::PPB as u32 / 10, in_context_ppb: se::PPB as u32 / 20, humans: 3, n_humans: 4 }),
        emission_max_failed: 2,
        metric: METRIC_CHAR_F1.into(),
        anchor_drift: None,
        in_scope_min: None,
        population_sha256: None,
        record_sha256: None,
        basis: Some("synthetic test margins".into()),
    }
}

#[test]
fn roles_split_batch_scoring_from_attribution() {
    assert!(batch_only("reference") && batch_only("previous_reference") && batch_only(ANCHOR_ROLE));
    assert!(!batch_only("entrant"));
    assert!(attributable("entrant") && attributable(ANCHOR_ROLE));
    assert!(!attributable("reference") && !attributable("previous_reference"));
}

#[test]
fn the_block_validates_report_mode_only() {
    let a = artifact(identity(3, 0x31), 7);
    let b = block(a.clone());
    b.validate(20).unwrap();
    let mut enforce = b.clone();
    enforce.mode = MODE_ENFORCE.into();
    assert_eq!(enforce.validate(20).unwrap_err().0, ENFORCE_REFUSAL);
    for (f, what) in [
        (Box::new(|p: &mut LocalizationPlan| p.rule = "x".into()) as Box<dyn Fn(&mut LocalizationPlan)>, "reward_v1.localization.rule"),
        (Box::new(|p: &mut LocalizationPlan| p.floor = 23), "reward_v1.localization.floor (24)"),
        (Box::new(|p: &mut LocalizationPlan| p.metric = "span_f1_iou50".into()), "reward_v1.localization.metric (char_f1)"),
        (Box::new(|p: &mut LocalizationPlan| p.emission_max_failed = 20), "reward_v1.localization.emission_max_failed (below n)"),
        (Box::new(|p: &mut LocalizationPlan| p.anchor_content = "zz".into()), "reward_v1.localization.anchor_content"),
        (Box::new(|p: &mut LocalizationPlan| p.basis = Some("x".repeat(201))), "reward_v1.localization.basis"),
        (Box::new(|p: &mut LocalizationPlan| p.population_sha256 = Some("00".into())), "reward_v1.localization digests"),
    ] {
        let mut p = b.clone();
        f(&mut p);
        assert_eq!(p.validate(20).unwrap_err().0, what);
    }
    // The anchor job: only when the anchor is not the reference's package.
    assert!(!b.anchor_job(&a));
    assert!(b.anchor_job(&artifact(identity(0, 0xb0), 1)));
    // An older build's plan (no block) keeps its bytes: the field is not serialized.
    let v = serde_json::to_value(&b).unwrap();
    assert_eq!(serde_json::from_value::<LocalizationPlan>(v).unwrap(), b);
    assert!(serde_json::from_value::<LocalizationPlan>(json!({"rule": se::GATE_RULE, "unknown": 1})).is_err());
}

#[test]
fn the_population_signature_follows_the_pool_and_the_generators() {
    let mut p = crate::eval::plan::tests::plan(20);
    let pool = p.pool.clone();
    let a = population_signature(&p, &pool);
    assert_eq!(a, population_signature(&p, &pool));
    let mut other = pool.clone();
    other.pool_id = "fresh".into();
    assert_ne!(a, population_signature(&p, &other));
    p.generators.push("another-model".into());
    assert_ne!(a, population_signature(&p, &pool));
}

#[test]
fn cells_come_from_the_reply_or_a_failed_prediction() {
    let human = "We walked to the market. ".repeat(10);
    let ai = "The committee reviewed every budget line with care and noted the costs. ".repeat(3);
    let text = format!("{human}{ai}");
    let gold = (human.len() as u32, text.len() as u32);
    let label = SpanLabel { kind: "mixed".into(), gold: Some(gold) };
    let words = se::words(&text[gold.0 as usize..gold.1 as usize]);
    let reply = json!({"status": "ok", "span_status": "ok",
        "prediction": {"spans": [{"start_byte": gold.0, "end_byte": gold.1 - 1, "words": words}]}});
    let c = cell(&text, &label, Some(&reply), Some(24)).unwrap();
    assert_eq!(c.pop, se::Population::InScope.tag());
    assert_eq!((c.fp, c.spans, c.flags), (0, 1, 0));
    assert!(c.f1 > 990_000_000);
    let failed = cell(&text, &label, None, Some(24)).unwrap();
    assert_eq!((failed.f1, failed.fp, failed.spans), (0, 0, 0));
    assert_eq!(failed.human, c.human);
    // A malformed reply is a format fault, and decodes nothing.
    let bad = json!({"status": "ok", "span_status": "ok", "prediction": {"spans": "x"}});
    let b = cell(&text, &label, Some(&bad), Some(24)).unwrap();
    assert_eq!((b.f1, b.flags & 4), (0, 4));
    // A human document is flagged by a failed prediction or emission.
    let h = SpanLabel { kind: "human_only".into(), gold: None };
    assert_eq!(cell(&human, &h, None, Some(24)).unwrap().flags & 1, 1);
    let emission = json!({"status": "ok", "span_status": {"unsupported": "nonfinite token state"}});
    assert_eq!(cell(&human, &h, Some(&emission), Some(24)).unwrap().flags, 3);
    assert_eq!(cell(&human, &h, Some(&json!({"status": "ok"})), Some(24)).unwrap().flags, 0);
    // An unusable label gives no cell.
    let broken = SpanLabel { kind: "mixed".into(), gold: None };
    assert!(cell(&text, &broken, None, Some(24)).is_none());
}

/// Load facts from `/healthz`: a token head with no decoder field (a scorer
/// image older than the decoder fact) gives no facts, never "decoder under
/// the floor"; a `null` decoder is the fact "no decoder"; a load without a
/// token head is a fact on every image.
#[test]
fn load_facts_need_the_decoder_field_for_a_token_head() {
    let h = |release: Value| json!({"state": "ready", "load": {"release": release}});
    let old_tower = h(json!({"arm": "s2c", "token_head": true, "span_tower": true}));
    assert_eq!(SpanFacts::of_healthz(&old_tower), None);
    let d24 = SpanFacts::of_healthz(&h(json!({"token_head": true,
        "decoder": {"rule": se::DECODER_RULE, "min_words": 24}})))
    .unwrap();
    assert_eq!(se::structural(&d24.structure()), None);
    assert_eq!((d24.decoder_rule.as_deref(), d24.decoder_min), (Some(se::DECODER_RULE), Some(24)));
    let short = SpanFacts::of_healthz(&h(json!({"token_head": true,
        "decoder": {"rule": se::DECODER_RULE, "min_words": 10}})))
    .unwrap();
    assert_eq!(se::structural(&short.structure()), Some(se::Reason::Floor));
    let none = SpanFacts::of_healthz(&h(json!({"token_head": true, "decoder": null}))).unwrap();
    assert_eq!(se::structural(&none.structure()), Some(se::Reason::Floor));
    // R0's release guard reports neither field, on old and new images.
    let r0 = SpanFacts::of_healthz(&h(json!({"arm": "r0", "seed": 17}))).unwrap();
    assert_eq!(se::structural(&r0.structure()), Some(se::Reason::Missing));
    let plain = SpanFacts::of_healthz(&h(json!({"token_head": false}))).unwrap();
    assert!(!plain.token_head);
}

#[test]
fn shard_labels_read_batch_documents_only() {
    let lines = [
        json!({"task": 0, "set": "batch", "kind": "mixed", "ai_byte_span": [3, 9], "text": "x"}),
        json!({"task": 1, "set": "batch", "kind": "human_only", "ai_byte_span": [null, null], "text": "y"}),
        json!({"task": 2, "set": "probe", "kind": "mixed", "ai_byte_span": [1, 2], "text": "z"}),
    ]
    .iter()
    .map(|v| v.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    let l = shard_labels(lines.as_bytes()).unwrap();
    assert_eq!(l.len(), 2);
    assert_eq!(l[&0], SpanLabel { kind: "mixed".into(), gold: Some((3, 9)) });
    assert_eq!(l[&1].gold, None);
}

/// Receipts are a function of the document outcome alone: a span cell never
/// changes a receipt byte.
#[test]
fn receipt_bytes_are_equal_with_and_without_spans() {
    let seed = [7u8; 32];
    let facts = ChunkFacts {
        manifest: Digest([1; 32]),
        artifact: Digest([2; 32]),
        seed: &seed,
        prefix: "receipts/testnet-x",
        job: 3,
        chunk: 0,
        attempt: 1,
        lost: &[],
        record_only: false,
        tasks: &[0, 1],
    };
    let p = slop_ninja_protocol::Probability::new(700_000_000).unwrap();
    let s = slop_ninja_protocol::Probability::new(300_000_000).unwrap();
    let plain = vec![
        TaskOutcome::new(0, &slop_ninja_protocol::Outcome::Prediction { involvement: p, share: s }, None, None, Some(9)),
        TaskOutcome::new(1, &slop_ninja_protocol::Outcome::ModelError, None, None, None),
    ];
    let mut with = plain.clone();
    with[0].span = Some(SpanCell { pop: 1, f1: 812_000_000, fp: 31, human: 900, flags: 0, spans: 2 });
    with[1].span = Some(SpanCell { pop: 2, f1: 0, fp: 0, human: 0, flags: 1, spans: 0 });
    let (a, b) = (receipts::build_chunk(&facts, &plain).unwrap(), receipts::build_chunk(&facts, &with).unwrap());
    assert_eq!(a.object, b.object);
    assert_eq!(a.object_key, b.object_key);
    assert_eq!(a.receipts, b.receipts);
    // The results object (the reconciliation's source) keeps the cells.
    let json = serde_json::to_string(&with).unwrap();
    assert_eq!(serde_json::from_str::<Vec<TaskOutcome>>(&json).unwrap(), with);
    assert!(!serde_json::to_string(&plain).unwrap().contains("span"));
}

fn fit(x: &str) -> FitDigests {
    FitDigests { fit_sha256: x.into(), selected_checkpoint_sha256: "c".into(), encoder_sha256: "e".into(), heads_sha256: "h".into() }
}

#[test]
fn the_span_equivalent_decision_checks_in_order() {
    let r0 = fit("f");
    let same = fit("f");
    let v = vec![(1u32, vec![Some(100), None, Some(5)], vec![Some(130), None, Some(5)])];
    let ok = DecisionInputs {
        chain_index: 0,
        promotion_pending: false,
        staged: true,
        spec_admits: true,
        published: true,
        vectors: &v,
        probe_versions: &[1],
        candidate_fit: &same,
        r0_fit: &r0,
        byte_comparison: true,
        tolerance_ppb: 30,
    };
    assert_eq!(check_span_equivalent(&ok), Ok(()));
    let code = |d: &DecisionInputs<'_>| check_span_equivalent(d).unwrap_err().code();
    assert_eq!(code(&DecisionInputs { chain_index: 1, ..ok }), "not_at_first_reference");
    assert_eq!(code(&DecisionInputs { promotion_pending: true, ..ok }), "promotion_pending");
    assert_eq!(code(&DecisionInputs { staged: false, ..ok }), "reference_not_staged");
    assert_eq!(code(&DecisionInputs { spec_admits: false, ..ok }), "reference_not_admitted");
    assert_eq!(code(&DecisionInputs { published: false, ..ok }), "candidate_unpublished");
    assert_eq!(code(&DecisionInputs { probe_versions: &[1, 2], ..ok }), "candidate_unpublished");
    let other = fit("g");
    assert_eq!(code(&DecisionInputs { candidate_fit: &other, ..ok }), "static_identity_mismatch");
    assert_eq!(code(&DecisionInputs { byte_comparison: false, ..ok }), "static_identity_mismatch");
    assert_eq!(code(&DecisionInputs { tolerance_ppb: 29, ..ok }), "not_document_equivalent");
    // A failure on one side only, or another length, is never equivalent.
    assert!(!vectors_equivalent(&[Some(1), None], &[Some(1), Some(2)], 1_000_000_000));
    assert!(!vectors_equivalent(&[Some(1)], &[Some(1), Some(2)], 0));
    assert!(DECISION_REFUSAL_NO_KEY.starts_with("operator_key_unavailable"));
}
