mod common;
use common::*;
use grammar_a_baseline::*;

#[test]
fn fallback_pos_counts_are_explicit_checked_and_distinct_from_dictionary_leaves() {
    let (contract, all, _) = fixture();
    let mut input = all[0].input.clone();
    input.lexical_status.oov_tokens = 1;
    input.lexical_status.fallback_tokens = 1;
    input.lexical_status.fallback_by_pos[1] = 1; // known noun POS, OOV leaf
    input.validate(&contract).unwrap();
    assert_ne!(input.lexical_status, all[0].input.lexical_status);
    // A known surface can also need a generic fallback; do not require OOV.
    input.lexical_status.oov_tokens = 0;
    input.validate(&contract).unwrap();
    let valid = input.clone();
    input.lexical_status.fallback_by_pos.pop();
    assert!(input.validate(&contract).is_err());
    input = valid.clone();
    input.lexical_status.fallback_tokens = 2;
    assert!(input.validate(&contract).is_err());
    input = valid.clone();
    input.lexical_status.fallback_tokens = 11;
    input.lexical_status.fallback_by_pos[1] = 11;
    assert!(input.validate(&contract).is_err());
    input = valid.clone();
    input.lexical_status.fallback_by_pos = vec![1, 0];
    assert!(input.validate(&contract).is_err()); // unknown POS must be counted
    input.lexical_status.unknown_pos_tokens = 1;
    input.validate(&contract).unwrap();
    input.lexical_status.fallback_by_pos = vec![u64::MAX, 1];
    assert!(input.validate(&contract).is_err());

    // Only the fallback noun channel drives this synthetic numeric head;
    // dictionary leaf values and known POS alone cannot activate it.
    let (mut artifact, _) = fit_ready(&contract, &training(&all));
    let noun_channel = 2 * contract.lexical_dimensions + 8;
    for head in &mut artifact.content.heads {
        for weights in &mut head.weights {
            weights.fill(0.0);
        }
        head.bias.fill(0.0);
        head.temperature = 1.0;
        if head.mode != Mode::Grammar {
            head.weights[1][noun_channel] = 20.0;
        }
    }
    artifact.sha256 = StableId(digest(MODEL_SCHEMA, &artifact.content).unwrap());
    let Prediction::Available { heads: dictionary } = predict(&artifact, &all[0].input).unwrap()
    else {
        panic!("complete dictionary input")
    };
    let Prediction::Available { heads: fallback } = predict(&artifact, &valid).unwrap() else {
        panic!("complete fallback input")
    };
    assert!((dictionary[0].probabilities[1] - 1.0 / 3.0).abs() < 1e-12);
    assert!(fallback[0].probabilities[1] > dictionary[0].probabilities[1]);
    assert_eq!(fallback[1].probabilities, dictionary[1].probabilities);
    assert!(fallback[2].probabilities[1] > dictionary[2].probabilities[1]);
}

#[test]
fn frozen_contract_binds_meanings_layout_and_categorical_leaf_identity() {
    let (contract, _, _) = fixture();
    let identity = contract.identity().unwrap();
    let restored: Contract =
        serde_json::from_slice(&serde_json::to_vec(&contract).unwrap()).unwrap();
    assert_eq!(identity, restored.identity().unwrap());
    let leaf = StableId::leaf(
        "native-persistent-lemma-sense",
        Some("native-morphology-past"),
    )
    .unwrap();
    assert_ne!(
        leaf,
        StableId::leaf("native-persistent-lemma-sense", None).unwrap()
    );
    let mut extended = contract.clone();
    extended.inventory_identity = id("inventory-with-added-word");
    extended.parser_identity = id("grammar-with-added-rule");
    assert_ne!(identity, extended.identity().unwrap());
    assert_eq!(
        leaf,
        StableId::leaf(
            "native-persistent-lemma-sense",
            Some("native-morphology-past")
        )
        .unwrap()
    );
    let mut changed = contract.clone();
    changed.normalization = Normalization::Counts;
    assert_ne!(identity, changed.identity().unwrap());
    changed = contract.clone();
    changed.parser_identity = id("another-parser");
    assert_ne!(identity, changed.identity().unwrap());
    changed = contract.clone();
    changed.lexical_catalog[0] = Coordinate::Leaf {
        leaf_id: leaf,
        pos_id: 1,
    };
    assert_ne!(identity, changed.identity().unwrap());
    changed.lexical_dimensions += 1;
    assert!(changed.validate().is_err());
}

#[test]
fn learned_payload_rejects_prose_and_distinguishes_missing_from_measured_zero() {
    let (contract, rows, _) = fixture();
    let input = &rows[0].input;
    let mut object = serde_json::to_value(input).unwrap();
    object["text"] = serde_json::json!("not accepted");
    assert!(serde_json::from_value::<NumericInput>(object).is_err());
    let mut missing = input.clone();
    unavailable(&mut missing);
    missing.validate(&contract).unwrap();
    assert!(!missing.complete());
    missing.grammar.values[0] = 1.0;
    assert!(missing.validate(&contract).is_err());
    let mut zero = input.clone();
    zero.grammar.values.fill(0.0);
    zero.validate(&contract).unwrap();
    assert!(zero.complete());
    zero.lexical_status.oov_tokens = 11;
    assert!(zero.validate(&contract).is_err());
    zero = input.clone();
    zero.lexical_status.pos_available = false;
    assert!(zero.validate(&contract).is_err());
}

#[test]
fn export_rejects_exact_normalized_group_and_ancestry_leakage() {
    let (contract, all, _) = fixture();
    let base = training(&all);
    for normalized in [false, true] {
        let mut rows = base.clone();
        let first = rows
            .iter()
            .find(|r| r.split == Split::Train)
            .unwrap()
            .clone();
        let dev = rows
            .iter_mut()
            .find(|r| r.split == Split::Development)
            .unwrap();
        if normalized {
            dev.normalized_text_sha256 = first.normalized_text_sha256;
        } else {
            dev.text_sha256 = first.text_sha256;
        }
        assert!(validate_export(&contract, &rows, &audit(&contract, &rows)).is_err());
    }
    let mut rows = base.clone();
    let root = rows[0].lineage_root.clone();
    let dev = rows
        .iter_mut()
        .find(|r| r.split == Split::Development)
        .unwrap();
    dev.lineage_root = root.clone();
    dev.ancestor_ids = vec![root];
    assert!(validate_export(&contract, &rows, &audit(&contract, &rows)).is_err());
    rows = base.clone();
    rows[0].split = Split::Development;
    assert!(validate_export(&contract, &rows, &audit(&contract, &rows)).is_err());
    rows = base.clone();
    rows.push(rows[0].clone());
    assert!(validate_export(&contract, &rows, &audit(&contract, &rows)).is_err());
    let mut exposure = audit(&contract, &base);
    exposure.fitting_source_groups.push(
        contract
            .assignments
            .iter()
            .find(|a| a.split == Split::Test)
            .unwrap()
            .source_group
            .clone(),
    );
    assert!(validate_export(&contract, &base, &exposure).is_err());
}

#[test]
fn known_ancestry_order_and_cycles_are_rejected() {
    let (contract, all, _) = fixture();
    let mut rows = training(&all);
    let root = rows[0].id.clone();
    rows[0].lineage_root = root.clone();
    rows[0].ancestor_ids.clear();
    rows[1].lineage_root = root.clone();
    rows[1].ancestor_ids = vec![root.clone()];
    rows[2].lineage_root = root.clone();
    rows[2].ancestor_ids = vec![root, rows[1].id.clone()];
    validate_export(&contract, &rows, &audit(&contract, &rows)).unwrap();
    let ancestor = rows[2].id.clone();
    rows[1].ancestor_ids.push(ancestor);
    assert!(validate_export(&contract, &rows, &audit(&contract, &rows)).is_err());
}

#[test]
fn no_test_rows_or_fabricated_missing_origin_classes_enter_fitting() {
    let (contract, all, _) = fixture();
    assert!(fit(&contract, &all, &audit(&contract, &all), &config()).is_err());
    let rows = training(&all)
        .into_iter()
        .filter(|r| r.label != Some(Origin::Mixed))
        .collect::<Vec<_>>();
    assert!(matches!(
        fit(&contract, &rows, &audit(&contract, &rows), &config()).unwrap(),
        FitOutcome::Unavailable { .. }
    ));
    let mut rows = training(&all);
    for r in &mut rows {
        r.label = None;
    }
    assert!(matches!(
        fit(&contract, &rows, &audit(&contract, &rows), &config()).unwrap(),
        FitOutcome::Unavailable { .. }
    ));
}

#[test]
fn unequal_family_sizes_receive_equal_total_loss_weight() {
    let (_, all, _) = fixture();
    let mut rows = training(&all);
    rows.retain(|r| r.split == Split::Train);
    rows.remove(0);
    let refs = rows.iter().collect::<Vec<_>>();
    let weights = source_family_weights(&refs).unwrap();
    let mut mass = std::collections::BTreeMap::new();
    for (r, w) in rows.iter().zip(weights) {
        *mass.entry(&r.source_group).or_insert(0.0) += w;
    }
    assert_eq!(mass.len(), 2);
    for w in mass.values() {
        assert!((w - 0.5_f64).abs() < 1e-12);
    }
}

#[test]
fn three_heads_are_reproducible_calibrated_and_report_common_rows() {
    let (contract, all, _) = fixture();
    let rows = training(&all);
    let (artifact, report) = fit_ready(&contract, &rows);
    let (again, second) = fit_ready(&contract, &rows);
    assert_eq!(artifact, again);
    assert_eq!(report, second);
    assert_eq!(report.grammar_evidence, GrammarEvidence::Synthetic);
    assert!(!report.semantic_admission);
    for head in &report.heads {
        assert_eq!(head.development_uncalibrated.rows, 6);
        assert_eq!(head.calibration_fitted.rows, 6);
        assert!(head.development_uncalibrated.row_accuracy > 0.99);
        assert!(
            head.calibration_fitted.family_log_loss
                <= head.calibration_uncalibrated.family_log_loss + 1e-12
        );
        assert_eq!(head.history[head.selected_epoch].epoch, head.selected_epoch);
    }
    let encoded = serde_json::to_vec(&artifact).unwrap();
    let restored: Artifact = serde_json::from_slice(&encoded).unwrap();
    restored.validate().unwrap();
    assert_eq!(
        predict(&artifact, &rows[0].input).unwrap(),
        predict(&restored, &rows[0].input).unwrap()
    );
    let mut reordered = rows.clone();
    reordered.reverse();
    let (same, _) = fit_ready(&contract, &reordered);
    assert_eq!(artifact, same);
}

#[test]
fn scales_are_train_only_and_calibration_cannot_choose_weights() {
    let (contract, all, _) = fixture();
    let rows = training(&all);
    let (original, _) = fit_ready(&contract, &rows);
    let mut dev = rows.clone();
    for row in dev.iter_mut().filter(|r| r.split == Split::Development) {
        row.input
            .lexical
            .values
            .iter_mut()
            .for_each(|v| *v *= 100.0);
    }
    let (changed_dev, _) = fit_ready(&contract, &dev);
    assert_eq!(original.content.scales, changed_dev.content.scales);
    let mut cal = rows.clone();
    for row in cal.iter_mut().filter(|r| r.split == Split::Calibration) {
        row.input.grammar.values.iter_mut().for_each(|v| *v *= 10.0);
    }
    let (changed_cal, _) = fit_ready(&contract, &cal);
    assert_eq!(original.content.scales, changed_cal.content.scales);
    for (a, b) in original
        .content
        .heads
        .iter()
        .zip(&changed_cal.content.heads)
    {
        assert_eq!(a.weights, b.weights);
        assert_eq!(a.bias, b.bias);
        assert_eq!(a.selected_epoch, b.selected_epoch);
    }
}

#[test]
fn availability_exclusions_are_shared_and_masked_runs_remain_explicit() {
    let (mut contract, all, _) = fixture();
    let mut rows = training(&all);
    unavailable(&mut rows[0].input);
    let (complete, report) = fit_ready(&contract, &rows);
    assert_eq!(report.partitions[0].eligible_rows, 5);
    assert_eq!(report.partitions[0].grammar_unavailable_rows, 1);
    assert!(matches!(
        predict(&complete, &rows[0].input).unwrap(),
        Prediction::Unavailable { .. }
    ));
    contract.row_policy = RowPolicy::MaskedPairedDocuments;
    let (masked, report) = fit_ready(&contract, &rows);
    assert_eq!(report.partitions[0].eligible_rows, 6);
    assert!(matches!(
        predict(&masked, &rows[0].input).unwrap(),
        Prediction::Available { .. }
    ));
    for row in &mut rows {
        unavailable(&mut row.input);
    }
    assert!(matches!(
        fit(&contract, &rows, &audit(&contract, &rows), &config()).unwrap(),
        FitOutcome::Unavailable { .. }
    ));
}

#[test]
fn heldout_evaluation_is_frozen_and_checks_the_complete_fit_ledger() {
    let (contract, all, _) = fixture();
    let (artifact, _) = fit_ready(&contract, &training(&all));
    let mut test = all
        .iter()
        .filter(|r| r.split == Split::Test)
        .cloned()
        .collect::<Vec<_>>();
    let before = artifact.clone();
    let result = evaluate(&artifact, &test, &audit(&contract, &test)).unwrap();
    assert!(result.available);
    assert_eq!(result.metrics.len(), 3);
    assert_eq!(artifact, before);
    test[0].normalized_text_sha256 = all[0].normalized_text_sha256.clone();
    assert!(evaluate(&artifact, &test, &audit(&contract, &test)).is_err());
}

#[test]
fn abstention_never_removes_rows_from_primary_metrics() {
    let (contract, all, _) = fixture();
    let mut rows = training(&all);
    // The ordinary fixture is separable, and calibrated softmax can round its
    // winning probability to exactly one. Instead use identical observations
    // for balanced classes: this has no class signal and stays at one-third.
    for row in &mut rows {
        row.input.lexical.values.fill(0.0);
        row.input.grammar.values.fill(0.0);
    }
    let mut config = config();
    config.abstain_below = Some(1.0);
    let FitOutcome::Ready { artifact, report } =
        fit(&contract, &rows, &audit(&contract, &rows), &config).unwrap()
    else {
        panic!("synthetic fit unavailable")
    };
    for head in report.heads {
        assert_eq!(head.calibration_fitted.rows, 6);
        assert_eq!(
            head.calibration_fitted
                .confusion
                .iter()
                .flatten()
                .sum::<usize>(),
            6
        );
        assert!((head.calibration_fitted.family_log_loss - 3.0_f64.ln()).abs() < 1e-10);
        assert_eq!(head.calibration_fitted.accepted_rows, 0);
        assert_eq!(head.calibration_fitted.selective_row_accuracy, None);
    }
    let Prediction::Available { heads } = predict(&artifact, &rows[0].input).unwrap() else {
        panic!("complete synthetic input must remain scoreable")
    };
    for head in heads {
        assert!(head.abstained);
        assert!(
            head.probabilities
                .iter()
                .all(|p| (*p - 1.0 / 3.0).abs() < 1e-10)
        );
    }
}
