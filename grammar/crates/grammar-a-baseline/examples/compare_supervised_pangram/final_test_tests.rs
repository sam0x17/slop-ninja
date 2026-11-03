use super::*;

fn description() -> Value {
    let pairs = vec![
        ("a".into(), 0.2, 0., 0),
        ("a".into(), 0.2, 1., 1),
        ("b".into(), 0.8, 1., 2),
    ];
    Isotonic::fit("calibration", &pairs, true)
        .unwrap()
        .description(true)
}
fn mapping() -> FrozenMapping {
    let d = description();
    FrozenMapping {
        model: stored_isotonic(&d).unwrap(),
        description: d,
        selector: "pangram-4".into(),
        version: "synthetic-version".into(),
        file_sha: b::digest(b"mapping"),
        requested: 3,
        paired: 3,
    }
}
#[test]
fn frozen_ties_weights_interpolation_and_clamped_ends_are_preserved() {
    let d = description();
    let model = stored_isotonic(&d).unwrap();
    assert_eq!(model.knots.len(), 2);
    assert_eq!(model.knots[0].rows, 2);
    assert_eq!(model.knots[0].weight, 0.5);
    assert_eq!(model.predict(0.), 0.5);
    assert!((model.predict(0.5) - 0.75).abs() < 1e-12);
    assert_eq!(model.predict(1.), 1.);
    for n in 0..100 {
        assert!(model.predict(n as f64 / 100.) <= model.predict((n + 1) as f64 / 100.));
    }
    assert_eq!(d, model.description(true));
}
#[test]
fn changed_fit_split_blocks_and_semantics_are_rejected() {
    for field in ["split", "block", "order", "interpolation"] {
        let mut d = description();
        match field {
            "split" => d["fit_split"] = json!("test"),
            "block" => d["blocks"][0]["weighted_positives"] = json!(0.),
            "order" => d["knots"][1]["fraction"] = json!(0.2),
            _ => d["interpolation"] = json!("test_fitted"),
        }
        assert!(stored_isotonic(&d).is_err());
    }
    assert!(Isotonic::fit("test", &[("x".into(), 0.2, 1., 1)], true).is_err());
}
#[test]
fn observations_separate_missing_failure_and_binding_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let map = mapping();
    let row = json!({"id":"synthetic","source_group":"family"});
    let hash = b::digest(b"synthetic text");
    assert!(test_observation(root, &row, &hash, &map).unwrap().is_err());
    fs::create_dir(root.join("synthetic")).unwrap();
    let path = root.join("synthetic/result.json");
    let v = json!({"id":"synthetic","source_group":"family","split":"test","text_sha256":hash,"model":"pangram-4",
        "response":{"stage":"STAGE_SUCCESS","version":"synthetic-version","fraction_ai":0.1,"fraction_ai_assisted":0.2}});
    fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(test_observation(root, &row, &hash, &map).unwrap().is_ok());
    for key in ["version", "hash", "split"] {
        let mut bad = v.clone();
        match key {
            "version" => bad["response"]["version"] = json!("changed"),
            "hash" => bad["text_sha256"] = json!(b::digest(b"changed")),
            _ => bad["split"] = json!("development"),
        }
        fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(test_observation(root, &row, &hash, &map).is_err());
    }
    let mut failed = v;
    failed["response"] = json!({"stage":"STAGE_FAILED"});
    fs::write(&path, serde_json::to_vec(&failed).unwrap()).unwrap();
    assert_eq!(
        test_observation(root, &row, &hash, &map)
            .unwrap()
            .unwrap_err()["reason"],
        "retained_provider_failure"
    );
}
#[test]
fn shared_mapping_is_anchored_to_its_original_producer_not_selected_fit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for dir in ["numeric", "fit"] {
        fs::create_dir(root.join(dir)).unwrap();
    }
    let parent = b::digest(b"numeric");
    let fit_sha = b::digest(b"producer fit");
    let weights = b::digest(b"weights");
    let contract = b::digest(b"contract");
    let pop = b::digest(b"population");
    let cal = b::digest(b"calibration");
    fs::write(
        root.join("numeric/manifest.json"),
        serde_json::to_vec(
            &json!({"final_test_opened":false,"population_file_sha256":pop,
        "splits":[{"split":"calibration","rows":3,"sha256":cal}]}),
        )
        .unwrap(),
    )
    .unwrap();
    fs::write(
        root.join("fit/fit.json"),
        br#"{"query_weighting":"family_origin_balanced"}"#,
    )
    .unwrap();
    let mut desc = description();
    desc["provenance"] = json!({"development_used_for_fitting":false,"dataset_manifest_sha256":parent,
        "fit_report_sha256":fit_sha,"population_sha256":pop,"calibration_numeric_sha256":cal});
    let report = json!({"schema":"slop_ninja_a_supervised_pangram_comparison_v1","split":"development","test_used_for_selection":false,
        "weights_sha256":weights,"contract_sha256":contract,"calibration":desc,"calibration_requested":3,"calibration_paired":3,
        "excluded":[],"records":[{"selector":"pangram-4","response_version":"synthetic-version"}]});
    let path = root.join("mapping.json");
    fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
    let hash = sha(&path).unwrap();
    let mut selection = json!({"selected_candidate":"another-candidate","pangram_mapping":{"path":path,"sha256":hash,"method":"isotonic_v1","fit_split":"calibration"},
        "binding_files":[{"path":path,"sha256":hash}],"candidates":[{"candidate":"original-producer","numeric_path":root.join("numeric"),
            "numeric_manifest_sha256":parent,"contract_sha256":contract,"runs":[{"model_path":root.join("fit"),"fit_sha256":fit_sha,"weights_sha256":weights}]}]});
    assert_eq!(frozen_mapping(&selection).unwrap().paired, 3);
    selection["candidates"][0]["runs"][0]["fit_sha256"] = json!(b::digest(b"other producer"));
    assert!(frozen_mapping(&selection).is_err());
}
