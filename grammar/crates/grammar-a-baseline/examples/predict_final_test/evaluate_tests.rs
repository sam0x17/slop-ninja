use super::*;
#[path = "synthetic.rs"]
mod synthetic;
fn predictions(rows: &[Value]) -> BTreeMap<String, Value> {
    let model = grammar_a_baseline::warm_shared::NumericSharedModel::from_json(
        &serde_json::to_vec(&synthetic::weights()).unwrap(),
    )
    .unwrap();
    rows.iter()
        .map(|r| {
            let input: SharedInput = serde_json::from_value(r["input"].clone()).unwrap();
            (
                r["id"].as_str().unwrap().to_owned(),
                serde_json::to_value(model.predict(&input).unwrap()).unwrap(),
            )
        })
        .collect()
}
#[test]
fn bridge_metrics_equal_existing_helpers_without_threshold_or_weight_changes() {
    let mut second = synthetic::row("b");
    second["targets"]["origin"] = json!(2);
    second["targets"]["ai_involvement"] = json!(true);
    second["targets"]["share"] = json!(0.2);
    second["roles"]["human"] = json!({"present":true,"identity":"heldout-writer","known":false});
    second["targets"]["human"] = Value::Null;
    let rows = vec![synthetic::row("a"), second];
    let pred = predictions(&rows);
    let weights = synthetic::weights();
    let contract = synthetic::contract();
    let before = weights.clone();
    let fit = json!({"training_status":{"human":"trained","ai":"trained"},"conditional_named_calibration":{}});
    let result = score(&rows, &pred, &weights, &contract, &fit, None).unwrap();
    assert_eq!(
        result["model"],
        origin_share_metrics(&rows, &pred, None).unwrap()
    );
    assert_eq!(
        result["family_origin_balanced"],
        balanced_origin_metrics(&rows, &pred, None).unwrap()
    );
    let mut human = attribution_metrics(
        &rows,
        &pred,
        "human",
        &roster(&contract, "human").unwrap(),
        0.6,
    )
    .unwrap();
    human["training_status"] = json!("trained");
    human["calibration"] = Value::Null;
    assert_eq!(result["attribution"]["human"], human);
    assert_eq!(weights, before);
}
#[test]
fn no_supported_predictions_produce_null_scores_and_no_invented_success() {
    let result = score(
        &[],
        &BTreeMap::new(),
        &synthetic::weights(),
        &synthetic::contract(),
        &json!({}),
        None,
    )
    .unwrap();
    assert_eq!(result["model"]["documents"], 0);
    assert!(result["model"]["involvement_brier"].is_null());
    assert!(result["family_origin_balanced"]["involvement_brier"].is_null());
    assert_eq!(result["attribution"]["human"]["documented_present"], 0);
}
#[test]
fn frozen_fallback_rules_are_still_enforced_by_original_metric_helper() {
    let rows = vec![synthetic::row("a")];
    let mut pred = predictions(&rows);
    pred.get_mut("a").unwrap()["attribution"]["human"]["profile"] = json!("human-a");
    assert!(
        score(
            &rows,
            &pred,
            &synthetic::weights(),
            &synthetic::contract(),
            &json!({}),
            None
        )
        .is_err()
    );
}

#[test]
fn separate_gallery_accepts_only_frozen_known_and_reserved_test_references() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let parent = root.join("parent");
    let numeric = root.join("test");
    fs::create_dir(&parent).unwrap();
    fs::create_dir(&numeric).unwrap();
    let contract_hash = "a".repeat(64);
    let fit_hash = "b".repeat(64);
    let weights_hash = "c".repeat(64);
    let raw_hash = "d".repeat(64);
    let profiles = (0..6).map(|i| format!("known-{i}")).collect::<Vec<_>>();
    let mut known = Vec::new();
    let mut all = Vec::new();
    for i in 0..6 {
        for part in 0..2 {
            let r = json!({"id":format!("known-{i}-{part}"),"source_group":format!("known-ref-{i}"),"frozen_split":"train","purpose":"reference",
            "input":{"contract_sha256":contract_hash,"x":[0.,0.]},"targets":{"origin":null,"ai_involvement":null,"share":null,"human":null,"ai":null},
            "roles":{"human":{"present":true,"identity":format!("known-{i}"),"known":true},"ai":{"present":false,"identity":null,"known":false}}});
            known.push(r.clone());
            let mut e = r;
            e["encoding"] = json!([1., 0.]);
            all.push(e);
        }
    }
    let serialize = |rows: &[Value]| {
        let mut b = Vec::new();
        for r in rows {
            serde_json::to_writer(&mut b, r).unwrap();
            b.push(b'\n');
        }
        b
    };
    let known_bytes = serialize(&known);
    fs::write(parent.join("reference.jsonl"), &known_bytes).unwrap();
    let plan = "split\tpg_id\tunderlying_work_key\tauthor_key\ntest_reference\t7\ttest-ref-7\theldout-7\ntest_reference\t8\ttest-ref-8\theldout-8\n";
    fs::write(root.join("sources.tsv"), plan).unwrap();
    let mut extra = Vec::new();
    for pg in [7, 8] {
        for fraction in [20, 60] {
            let r = json!({"id":format!("author-pg{pg}-p{fraction}"),"source_group":format!("test-ref-{pg}"),"frozen_split":"test","purpose":"reference",
            "input":{"contract_sha256":contract_hash,"x":[0.,0.]},"targets":{"origin":null,"ai_involvement":null,"share":null,"human":null,"ai":null},
            "roles":{"human":{"present":true,"identity":format!("heldout-{pg}"),"known":false},"ai":{"present":false,"identity":null,"known":false}}});
            extra.push(r.clone());
            let mut e = r;
            e["encoding"] = json!([0., 1.]);
            all.push(e);
        }
    }
    extra.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let source = json!({"references_sha256":raw_hash,"separate_reference_records":4,"reference_ids":extra.iter().map(|r|r["id"].clone()).collect::<Vec<_>>()});
    fs::write(
        numeric.join("test_dataset_manifest.json"),
        serde_json::to_vec(&source).unwrap(),
    )
    .unwrap();
    let selection = json!({"source_plan":{"path":root.join("sources.tsv")},"binding_files":[{"path":root.join("sources.tsv"),"sha256":b::digest(plan.as_bytes())},
        {"path":parent.join("reference.jsonl"),"sha256":b::digest(&known_bytes)}]});
    let d = b::TestData {
        manifest: json!({}),
        manifest_sha: contract_hash.clone(),
        contract_sha: contract_hash.clone(),
        parent_sha: contract_hash.clone(),
        selection_sha: contract_hash.clone(),
        query_sha: contract_hash.clone(),
        population_sha: contract_hash.clone(),
        dataset_sha: sha(&numeric.join("test_dataset_manifest.json")).unwrap(),
        rows: BTreeMap::from([("q".into(), synthetic::row("q"))]),
        numeric: BTreeMap::new(),
        hashes: BTreeMap::new(),
        projection_excluded: vec![],
        upstream: json!({}),
    };
    let a = prediction_binding::Admitted {
        selection,
        candidate: json!({"numeric_path":parent}),
        run: json!({"weights_sha256":weights_hash,"fit_sha256":fit_hash}),
        fit: json!({}),
        weights: json!({"architecture":{"embedding":2}}),
        weight_bytes: vec![],
        contract: json!({"input_dimension":2,"human_profiles":profiles,"ai_profiles":[],"split_assignments":{}}),
        data: d,
    };
    let bundle = json!({"schema":"slop_ninja_final_test_reference_encodings_v1","complete":true,"selection_receipt_sha256":contract_hash,"frozen_manifest_sha256":contract_hash,
        "contract_sha256":contract_hash,"weights_sha256":weights_hash,"fit_sha256":fit_hash,"fit_performed":false,"normalization_fitted":false,"profiles_changed":false,
        "test_used_for_selection":false,"known_reference_sha256":b::digest(&known_bytes),"reference_records_sha256":raw_hash,
        "query_dataset_manifest_sha256":a.data.dataset_sha,
        "partition_reference_sha256":b::digest(&serialize(&extra)),"rows":all});
    let path = root.join("gallery.json");
    fs::write(&path, serde_json::to_vec(&bundle).unwrap()).unwrap();
    assert_eq!(
        gallery::load(&path, &numeric, &a).unwrap().0["human"].len(),
        8
    );
    for change in ["known_input", "withheld_writer", "partition", "weights"] {
        let mut v = bundle.clone();
        match change {
            "known_input" => v["rows"][0]["input"]["x"] = json!([1., 0.]),
            "withheld_writer" => {
                v["rows"][12]["roles"]["human"]["identity"] = json!("another-writer")
            }
            "partition" => v["rows"][12]["frozen_split"] = json!("calibration"),
            _ => v["weights_sha256"] = json!("e".repeat(64)),
        }
        fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(gallery::load(&path, &numeric, &a).is_err());
    }
}
