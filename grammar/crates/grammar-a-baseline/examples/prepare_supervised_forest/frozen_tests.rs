//! Only fabricated data is used to exercise the final-Test boundary.
use super::super::tests::{fixture, jsonl, put, read, read_rows, run};
use super::*;

fn test_export(root: &Path, group: &str, purpose: &str) {
    let out = root.join("test_export");
    fs::create_dir(&out).unwrap();
    let raw = read_rows(&root.join("export/rows.jsonl"));
    let mut row = raw[2].clone();
    row["id"] = json!("synthetic_test");
    row["metadata"]["id"] = json!("synthetic_test");
    row["metadata"]["source_group"] = json!(group);
    row["metadata"]["frozen_split"] = json!("test");
    row["metadata"]["purpose"] = json!(purpose);
    row["metadata"]["human_identity"] = json!("test_only_writer");
    row["metadata"]["ai_identity"] = json!("test_only_model");
    let mut source_contract = read(&root.join("export/contract.json"));
    source_contract["input_sha256"] = json!(sha(&jsonl(&[row["metadata"].clone()])));
    put(&out.join("contract.json"), &source_contract);
    put(&out.join("00000.receipt.json"), &row["receipt"]);
    row["receipt_sha256"] = json!(sha(&fs::read(out.join("00000.receipt.json")).unwrap()));
    fs::write(out.join("rows.jsonl"), jsonl(&[row.clone()])).unwrap();
    put(
        &out.join("report.json"),
        &json!({"schema":"slop_ninja_document_export_report_v1",
        "engine_identity":"synthetic_engine","complete":true,"excluded_reserved_test_documents":0,
        "rows_sha256":sha(&jsonl(&[row.clone()])),
        "documents":[{"id":"synthetic_test","prefix":"00000","summary":row["receipt"]["summary"]}]}),
    );
    let mut splits = serde_json::Map::new();
    splits.insert(group.into(), json!("test"));
    put(
        &root.join("test_dataset.json"),
        &json!({"schema":"slop_ninja_a_supervised_dataset_v1",
        "records_sha256":source_contract["input_sha256"],"test_ids":["synthetic_test"],"non_test_ids":[],
        "split_assignments":splits,"sources":{"kind":"synthetic fixture only"}}),
    );
}

fn transform_fixture(root: &Path) -> Result<()> {
    transform(
        &root.join("numeric"),
        &root.join("test_export"),
        &root.join("test_dataset.json"),
        &root.join("transformed"),
    )
}

fn partition_reference_export(root: &Path, group: &str, purpose: &str, split: &str) {
    test_export(root, group, purpose);
    let out = root.join("test_export");
    let mut row = read_rows(&out.join("rows.jsonl")).remove(0);
    row["metadata"]["frozen_split"] = json!(split);
    let rows_bytes = jsonl(&[row.clone()]);
    fs::write(out.join("rows.jsonl"), &rows_bytes).unwrap();
    let mut source = read(&out.join("contract.json"));
    source["input_sha256"] = json!(sha(&jsonl(&[row["metadata"].clone()])));
    put(&out.join("contract.json"), &source);
    let mut report = read(&out.join("report.json"));
    report["rows_sha256"] = json!(sha(&rows_bytes));
    put(&out.join("report.json"), &report);
    let mut dataset = read(&root.join("test_dataset.json"));
    dataset["records_sha256"] = source["input_sha256"].clone();
    dataset["reference_transform_only"] = json!(true);
    dataset["non_test_ids"] = json!(["synthetic_test"]);
    dataset["test_ids"] = json!([]);
    dataset["split_assignments"][group] = json!(split);
    put(&root.join("test_dataset.json"), &dataset);
}

fn train_extension_export(root: &Path, group: &str, purpose: &str, split: &str) {
    partition_reference_export(root, group, purpose, split);
    let path = root.join("test_dataset.json");
    let mut dataset = read(&path);
    dataset["reference_transform_only"] = json!(false);
    put(&path, &dataset);
}

#[test]
fn additional_training_queries_use_frozen_features_without_changing_existing_inputs() {
    for ablation in [Ablation::All, Ablation::LeafGrammar] {
        let tmp = tempfile::tempdir().unwrap();
        fixture(tmp.path(), 1e8);
        run(tmp.path(), ablation).unwrap();
        train_extension_export(tmp.path(), "new_train_work", "query", "train");
        transform_train_extension(
            &tmp.path().join("numeric"),
            &tmp.path().join("test_export"),
            &tmp.path().join("test_dataset.json"),
            &tmp.path().join("additional"),
        )
        .unwrap();
        for file in [
            "contract.json",
            "population.json",
            "train.jsonl",
            "development.jsonl",
            "calibration.jsonl",
            "reference.jsonl",
        ] {
            assert_eq!(
                fs::read(tmp.path().join("numeric").join(file)).unwrap(),
                fs::read(tmp.path().join("additional").join(file)).unwrap()
            );
        }
        let rows = read_rows(&tmp.path().join("additional/train_extension.jsonl"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["frozen_split"], "train");
        assert_eq!(rows[0]["roles"]["human"]["known"], false);
        assert!(rows[0]["targets"]["human"].is_null());
        assert!(rows[0]["targets"]["origin"].is_number());
        let m = read(&tmp.path().join("additional/manifest.json"));
        assert_eq!(m["final_test_opened"], false);
        assert_eq!(
            m["training_extension_transform"]["normalization_fitted"],
            false
        );
        assert!(!tmp.path().join("additional/test.jsonl").exists());
    }
}

#[test]
fn training_extension_rejects_protected_families_references_and_evaluation_splits() {
    for (group, purpose, split) in [
        ("new_work", "query", "development"),
        ("new_work", "query", "calibration"),
        ("new_work", "query", "test"),
        ("new_work", "reference", "train"),
        ("train_a", "query", "train"),
        ("reference_family", "query", "train"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        fixture(tmp.path(), 1.);
        run(tmp.path(), Ablation::All).unwrap();
        train_extension_export(tmp.path(), group, purpose, split);
        assert!(
            transform_train_extension(
                &tmp.path().join("numeric"),
                &tmp.path().join("test_export"),
                &tmp.path().join("test_dataset.json"),
                &tmp.path().join("additional")
            )
            .is_err()
        );
        assert!(!tmp.path().join("additional").exists());
    }
}

#[test]
fn partition_references_preserve_frozen_contract_and_real_partitions() {
    for ablation in [Ablation::All, Ablation::LeafGrammar] {
        for split in ["development", "calibration"] {
            let tmp = tempfile::tempdir().unwrap();
            fixture(tmp.path(), 1e8);
            run(tmp.path(), ablation).unwrap();
            partition_reference_export(
                tmp.path(),
                "independent_reference_work",
                "reference",
                split,
            );
            transform_references(
                &tmp.path().join("numeric"),
                &tmp.path().join("test_export"),
                &tmp.path().join("test_dataset.json"),
                &tmp.path().join("references"),
            )
            .unwrap();
            for file in [
                "contract.json",
                "population.json",
                "train.jsonl",
                "development.jsonl",
                "calibration.jsonl",
                "reference.jsonl",
            ] {
                assert_eq!(
                    fs::read(tmp.path().join("numeric").join(file)).unwrap(),
                    fs::read(tmp.path().join("references").join(file)).unwrap()
                );
            }
            let m = read(&tmp.path().join("references/manifest.json"));
            assert_eq!(m["final_test_opened"], false);
            assert_eq!(
                m["splits"],
                read(&tmp.path().join("numeric/manifest.json"))["splits"]
            );
            assert_eq!(m["partition_reference_transform"]["fit_performed"], false);
            let rows = read_rows(&tmp.path().join("references/partition_references.jsonl"));
            assert_eq!(rows[0]["frozen_split"], split);
            assert_eq!(rows[0]["roles"]["human"]["known"], false);
            assert_eq!(rows[0]["input"]["contract_sha256"], m["contract_sha256"]);
            assert!(
                rows[0]["targets"]
                    .as_object()
                    .unwrap()
                    .values()
                    .all(Value::is_null)
            );
            assert!(
                Dataset::parse(&fs::read(tmp.path().join("test_dataset.json")).unwrap()).is_err()
            );
        }
    }
}

#[test]
fn partition_references_reject_test_queries_and_frozen_family_overlap() {
    for (group, purpose, split) in [
        ("new", "reference", "test"),
        ("new", "query", "development"),
        ("new", "reference", "train"),
        ("train_a", "reference", "development"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        fixture(tmp.path(), 5.);
        run(tmp.path(), Ablation::All).unwrap();
        partition_reference_export(tmp.path(), group, purpose, split);
        assert!(
            transform_references(
                &tmp.path().join("numeric"),
                &tmp.path().join("test_export"),
                &tmp.path().join("test_dataset.json"),
                &tmp.path().join("references")
            )
            .is_err()
        );
        assert!(!tmp.path().join("references").exists());
    }
}

#[test]
fn frozen_test_transform_preserves_every_fitted_binding_and_masks_unknowns() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), 99999.0);
    run(tmp.path(), Ablation::All).unwrap();
    test_export(tmp.path(), "synthetic_test_family", "query");
    transform_fixture(tmp.path()).unwrap();
    let original = tmp.path().join("numeric");
    let output = tmp.path().join("transformed");
    for file in [
        "contract.json",
        "population.json",
        "train.jsonl",
        "development.jsonl",
        "calibration.jsonl",
        "reference.jsonl",
    ] {
        assert_eq!(
            fs::read(original.join(file)).unwrap(),
            fs::read(output.join(file)).unwrap(),
            "modified {file}"
        );
    }
    let before = read(&original.join("manifest.json"));
    let after = read(&output.join("manifest.json"));
    assert_eq!(before["contract_sha256"], after["contract_sha256"]);
    assert_eq!(
        before["splits"],
        json!(&after["splits"].as_array().unwrap()[..4])
    );
    assert_eq!(after["test_transform"]["fit_performed"], false);
    let rows = read_rows(&output.join("test.jsonl"));
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["input"]["contract_sha256"],
        before["contract_sha256"]
    );
    assert!(rows[0]["targets"]["human"].is_null() && rows[0]["targets"]["ai"].is_null());
    assert_eq!(
        rows[0]["roles"]["human"],
        json!({"present":true,"identity":"test_only_writer","known":false})
    );
    // Test was cloned from Dev with only label/ID/family changes. Extreme counts
    // use exactly the old frozen transform, without influencing any parameter.
    let dev = read_rows(&original.join("development.jsonl"));
    assert_eq!(rows[0]["input"]["x"], dev[0]["input"]["x"]);
    assert_eq!(
        after["splits"][4]["sha256"],
        sha(&fs::read(output.join("test.jsonl")).unwrap())
    );
}

#[test]
fn changed_engine_parser_policy_are_rejected_before_test_rows_open() {
    for (field, value, message) in [
        ("engine_identity", json!("changed"), "engine"),
        ("parser_identity", json!("changed"), "parser"),
        ("policy", json!({"changed":true}), "policy"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        fixture(tmp.path(), 10.0);
        run(tmp.path(), Ablation::All).unwrap();
        test_export(tmp.path(), "synthetic_test_family", "query");
        let path = tmp.path().join("test_export/contract.json");
        let mut contract = read(&path);
        contract[field] = value;
        put(&path, &contract);
        fs::remove_file(tmp.path().join("test_export/rows.jsonl")).unwrap();
        let error = transform_fixture(tmp.path()).unwrap_err().to_string();
        assert!(error.contains(message), "{error}");
        assert!(!tmp.path().join("transformed").exists());
    }
}

#[test]
fn test_family_and_reference_leakage_are_rejected() {
    for (group, purpose, message) in [
        ("train_a", "query", "family"),
        ("reference_family", "query", "family"),
        ("synthetic_test_family", "reference", "reference"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        fixture(tmp.path(), 10.0);
        run(tmp.path(), Ablation::All).unwrap();
        test_export(tmp.path(), group, purpose);
        let error = transform_fixture(tmp.path()).unwrap_err().to_string();
        assert!(error.contains(message), "{error}");
        assert!(!tmp.path().join("transformed").exists());
    }
}

#[test]
fn test_allow_list_and_frozen_hash_are_mandatory() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), 10.0);
    run(tmp.path(), Ablation::All).unwrap();
    test_export(tmp.path(), "synthetic_test_family", "query");
    let path = tmp.path().join("test_dataset.json");
    let mut dataset = read(&path);
    dataset["test_ids"] = json!(["different_id"]);
    put(&path, &dataset);
    assert!(
        transform_fixture(tmp.path())
            .unwrap_err()
            .to_string()
            .contains("allow-list")
    );
    assert!(!tmp.path().join("transformed").exists());
    let contract = tmp.path().join("numeric/contract.json");
    let mut changed = read(&contract);
    changed["normalization"]["mean"][0] = json!(123.0);
    put(&contract, &changed);
    assert!(
        transform_fixture(tmp.path())
            .unwrap_err()
            .to_string()
            .contains("contract file changed")
    );
}

#[test]
fn changed_receipt_resolver_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), 10.0);
    run(tmp.path(), Ablation::All).unwrap();
    test_export(tmp.path(), "synthetic_test_family", "query");
    let path = tmp.path().join("test_export/rows.jsonl");
    let mut rows = read_rows(&path);
    rows[0]["receipt"]["resolver"] = json!("another_resolver");
    let receipt_path = tmp.path().join("test_export/00000.receipt.json");
    put(&receipt_path, &rows[0]["receipt"]);
    rows[0]["receipt_sha256"] = json!(sha(&fs::read(receipt_path).unwrap()));
    fs::write(&path, jsonl(&rows)).unwrap();
    let report_path = tmp.path().join("test_export/report.json");
    let mut report = read(&report_path);
    report["rows_sha256"] = json!(sha(&jsonl(&rows)));
    put(&report_path, &report);
    assert!(
        transform_fixture(tmp.path())
            .unwrap_err()
            .to_string()
            .contains("resolver")
    );
    assert!(!tmp.path().join("transformed").exists());
}
