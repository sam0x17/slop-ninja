use super::*;
#[path = "synthetic.rs"]
mod synthetic;
fn admitted() -> binding::Admitted {
    let numeric = BTreeMap::from([
        ("a".into(), synthetic::row("a")),
        ("b".into(), synthetic::row("b")),
    ]);
    let hash = "a".repeat(64);
    let w = synthetic::weights();
    let wb = serde_json::to_vec(&w).unwrap();
    let data = test_binding::TestData {
        manifest: json!({}),
        manifest_sha: hash.clone(),
        contract_sha: hash.clone(),
        parent_sha: hash.clone(),
        selection_sha: hash.clone(),
        query_sha: hash.clone(),
        population_sha: hash.clone(),
        dataset_sha: hash.clone(),
        rows: numeric.clone(),
        numeric,
        hashes: BTreeMap::new(),
        projection_excluded: vec![],
        upstream: json!({"requested":2,"assembled":2}),
    };
    binding::Admitted {
        selection: json!({}),
        candidate: json!({"candidate":"synthetic"}),
        run: json!({"seed":17,"weights_sha256":test_binding::digest(&wb),"fit_sha256":hash}),
        fit: json!({}),
        weights: w,
        weight_bytes: wb,
        contract: synthetic::contract(),
        data,
    }
}
#[test]
fn dispatcher_exports_all_heads_and_the_same_frozen_query_encoding() {
    let a = admitted();
    let model = NumericSharedModel::from_json(&a.weight_bytes).unwrap();
    let (prediction, encoding_error) = predict_row(&model, &synthetic::row("a")).unwrap();
    let input: SharedInput = serde_json::from_value(synthetic::row("a")["input"].clone()).unwrap();
    assert_eq!(prediction["encoding"], json!(model.encode(&input).unwrap()));
    assert!(encoding_error.is_none());
    for role in ["human", "ai"] {
        assert!(prediction["attribution"][role]["conditional_named_scores"].is_object());
        assert_eq!(
            prediction["attribution"][role]["fallback_asserts_presence"],
            false
        );
    }
    assert!((prediction["ai_confidence_percent"].as_f64().unwrap() - 200. / 3.).abs() < 1e-10);
    assert_eq!(prediction["ai_share_percent"], 50.);
}
#[test]
fn partial_all_failed_and_empty_batches_retain_exact_receipt_accounting() {
    for mode in ["partial", "all", "empty"] {
        let mut a = admitted();
        if mode == "empty" {
            a.data.numeric.clear();
        }
        let result = infer(&a.data.numeric, |r| {
            if mode == "all" || r["id"] == "b" {
                anyhow::bail!("synthetic runtime failure");
            }
            Ok((
                json!({"ai_confidence_percent":50.,"ai_share_percent":25.}),
                None,
            ))
        });
        let temp = tempfile::tempdir().unwrap();
        let pb = serde_json::to_vec_pretty(&result.rows).unwrap();
        fs::write(temp.path().join("predictions.json"), &pb).unwrap();
        let r = receipt(&a, &result, &pb, 0, None);
        test_binding::write_new(&temp.path().join("receipt.json"), &r).unwrap();
        let loaded =
            test_binding::predictions(&temp.path().join("receipt.json"), &a.data, &a.run).unwrap();
        assert_eq!(
            loaded.values.len() + loaded.excluded.len(),
            a.data.numeric.len()
        );
        assert_eq!(r["all_failed"], mode == "all");
        if mode == "partial" {
            assert_eq!(loaded.values.len(), 1);
        }
    }
}
#[test]
fn diagnostic_controls_or_missing_head_supervision_cannot_enter_actual_a() {
    let w = synthetic::weights();
    let c = synthetic::contract();
    binding::all_heads(&w, &c).unwrap();
    for control in ["origin_only", "attribution_only"] {
        let mut bad = w.clone();
        bad["control"] = json!(control);
        assert!(binding::all_heads(&bad, &c).is_err());
    }
    for role in ["origin", "share", "mixed_fractional_share", "human", "ai"] {
        let mut bad = w.clone();
        bad["supervision"][role] = json!(0);
        assert!(binding::all_heads(&bad, &c).is_err());
    }
}
#[test]
fn numeric_dimension_failures_are_per_query_not_silent_drops() {
    let a = admitted();
    let model = NumericSharedModel::from_json(&a.weight_bytes).unwrap();
    let mut rows = a.data.numeric;
    rows.get_mut("b").unwrap()["input"]["x"] = json!([1.]);
    let outcome = infer(&rows, |r| predict_row(&model, r));
    assert_eq!(outcome.rows.len(), 1);
    assert_eq!(outcome.excluded[0]["id"], "b");
}
#[test]
fn role_targets_are_bound_to_metadata_and_unknown_identities_stay_masked() {
    let mut row = synthetic::row("a");
    let c = synthetic::contract();
    let m = json!({"human_role_present":true,"human_identity":"human-a","ai_role_present":false,"ai_identity":null});
    binding::roles(&row, &m, &c).unwrap();
    row["targets"]["human"] = json!(1);
    assert!(binding::roles(&row, &m, &c).is_err());
    let m = json!({"human_role_present":true,"human_identity":null,"ai_role_present":false,"ai_identity":null});
    row["targets"]["human"] = Value::Null;
    row["roles"]["human"] = json!({"present":true,"identity":null,"known":false});
    binding::roles(&row, &m, &c).unwrap();
}
#[test]
fn output_cannot_mutate_any_fit_or_numeric_parent() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for dir in ["numeric", "fit", "other-fit", "test"] {
        fs::create_dir(root.join(dir)).unwrap();
    }
    let selection = json!({"candidates":[{"numeric_path":root.join("numeric"),"runs":[{"model_path":root.join("fit")},{"model_path":root.join("other-fit")}]}]});
    for dir in ["numeric", "fit", "other-fit", "test"] {
        assert!(
            binding::new_output(&root.join(dir).join("new"), &selection, &root.join("test"))
                .is_err()
        );
    }
    binding::new_output(&root.join("new"), &selection, &root.join("test")).unwrap();
}
