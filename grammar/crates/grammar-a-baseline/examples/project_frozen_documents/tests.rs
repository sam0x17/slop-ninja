//! Small synthetic unlabelled exports; no corpus, parser or provider execution.
use super::*;

const SYMBOL: &str = r#"native_complete/symbol/{"category":"Noun","features":{}}"#;
const LEAF: &str =
    r#"native_complete/leaf/["inventory","NOUN",{"category":"Noun","features":{}},null]"#;

fn contract() -> Value {
    json!({"schema":"slop_ninja_shared_a_numeric_contract_v1",
        "feature_policy":FEATURE_POLICY,"feature_policy_version":1,"ablation":"all",
        "projection":{"feature_schema":FEATURE_SCHEMA,"resolver":RESOLVER},
        "grammar_identities":{"document_engine":"synthetic-engine","native_parser":"synthetic-parser"},
        "document_policy":{"synthetic":true},
        "coordinates":[FIXED[0],LEAF,SYMBOL],"input_dimension":6,
        "normalization":{"transform":"log1p(count/denominator)","fit":"train_query_only_equal_source_family_weight",
            "mean":[0.2,0.3,0.4],"scale":[1.3,0.5,0.25],"scale_floor":0.01,"all_masked_mean":0.0,"all_masked_scale":1.0},
        "layout":"standardized_values_then_train_centered_availability_masks",
        "availability_mask_center":[1.0,0.25,0.75],
        "availability_mask_transform":"mask_minus_equal_source_family_Train_presence_mean; invert by adding frozen mean; do not rescale",
        "masked_standardized_value":0.0})
}

fn put(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn jsonl(rows: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

fn fixture(root: &Path) {
    fs::create_dir(root.join("export")).unwrap();
    put(&root.join("contract.json"), &contract());
    put(
        &root.join("export/contract.json"),
        &json!({"schema":"slop_ninja_document_export_contract_v1",
        "engine_identity":"synthetic-engine","parser_identity":"synthetic-parser","policy":{"synthetic":true},
        "test_populations_excluded":false,"input_sha256":sha(b"arbitrary formatted unlabelled input")}),
    );
    let text = "Café waits. More words.";
    let mut counts = serde_json::Map::new();
    counts.insert(SYMBOL.into(), json!(2.0));
    counts.insert(LEAF.into(), json!(1.0));
    counts.insert("availability/window/NativeComplete".into(), json!(1.0));
    let summary = json!({"tokens":4,"represented_tokens":4,"unresolved_tokens":0,
        "input_bytes":text.len(),"accounted_bytes":text.len(),"expected_fallback_leaves":0.0,
        "execution_failures":0,"windows":1});
    let receipt = json!({"schema":"slop_ninja_cfg_document_fallback_v1","feature_schema":FEATURE_SCHEMA,
        "resolver":RESOLVER,"engine_identity":"synthetic-engine","parser_identity":"synthetic-parser",
        "source_sha256":sha(text.as_bytes()),"summary":summary,
        "features":{"token_opportunities":4,"represented_tokens":4,"unresolved_tokens":0,
            "expected_fallback_leaves":0.0,"projection_available":false,"search_exhaustive":true,"counts":counts}});
    let row = json!({"id":"unlabelled","metadata":{"id":"unlabelled","text":text},
        "receipt":receipt,"receipt_sha256":"filled by rewrite","windows_sha256":sha(b"unopened windows")});
    rewrite_row(root, row);
}

fn rewrite_row(root: &Path, mut row: Value) {
    let path = root.join("export/00000.receipt.json");
    put(&path, &row["receipt"]);
    row["receipt_sha256"] = json!(sha(&fs::read(path).unwrap()));
    let bytes = jsonl(&[row.clone()]);
    fs::write(root.join("export/rows.jsonl"), &bytes).unwrap();
    put(
        &root.join("export/report.json"),
        &json!({"schema":"slop_ninja_document_export_report_v1",
        "complete":true,"engine_identity":"synthetic-engine","rows_sha256":sha(&bytes),
        "documents":[{"id":row["id"],"prefix":"00000","summary":row["receipt"]["summary"]}]}),
    );
}

fn run(root: &Path) -> Result<usize> {
    project_export(
        &root.join("contract.json"),
        &root.join("export"),
        &root.join("numeric.jsonl"),
    )
}

#[test]
fn unlabelled_projection_is_numeric_only_and_exact_formula() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    assert_eq!(run(tmp.path()).unwrap(), 1);
    let row = read(&tmp.path().join("numeric.jsonl"));
    assert_eq!(
        row.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["id", "input"]
    );
    assert_eq!(
        row["input"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["contract_sha256", "x"]
    );
    assert_eq!(row["id"], "unlabelled");
    assert_eq!(
        row["input"]["contract_sha256"],
        sha(&serde_json::to_vec(&contract()).unwrap())
    );
    let expected = [
        -0.2 / 1.3,
        (0.25_f64.ln_1p() - 0.3) / 0.5,
        (0.5_f64.ln_1p() - 0.4) / 0.25,
        0.0,
        0.75,
        0.25,
    ];
    for (value, expected) in row["input"]["x"].as_array().unwrap().iter().zip(expected) {
        assert_eq!(value.as_f64().unwrap().to_bits(), expected.to_bits());
    }
}

#[test]
fn arbitrary_metadata_never_changes_numeric_inputs() {
    let a = tempfile::tempdir().unwrap();
    fixture(a.path());
    run(a.path()).unwrap();
    let b = tempfile::tempdir().unwrap();
    fixture(b.path());
    let mut row = read(&b.path().join("export/rows.jsonl"));
    // Even invalid label types are ignored: this is not a supervised loader.
    row["metadata"]["origin"] = json!({"arbitrary":"not a label"});
    row["metadata"]["share_target"] = json!(-9);
    row["metadata"]["human_identity"] = json!([true]);
    row["metadata"]["ai_identity"] = json!(17);
    row["metadata"]["purpose"] = json!("unrelated");
    row["metadata"]["frozen_split"] = json!("unspecified");
    rewrite_row(b.path(), row);
    run(b.path()).unwrap();
    assert_eq!(
        fs::read(a.path().join("numeric.jsonl")).unwrap(),
        fs::read(b.path().join("numeric.jsonl")).unwrap()
    );
}

#[test]
fn frozen_policy_and_parameters_are_checked_before_rows_open() {
    for (field, value) in [
        ("feature_policy", json!("unknown")),
        ("feature_policy_version", json!(2)),
        ("layout", json!("other")),
        ("input_dimension", json!(5)),
        ("ablation", json!("lexical")),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        fixture(tmp.path());
        let mut bad = contract();
        bad[field] = value;
        put(&tmp.path().join("contract.json"), &bad);
        fs::remove_file(tmp.path().join("export/rows.jsonl")).unwrap();
        assert!(run(tmp.path()).is_err());
        assert!(!tmp.path().join("numeric.jsonl").exists());
    }
    for value in [
        json!([0.0, 1.0, 1.0]),
        json!([1.0]),
        json!(["invalid", 1.0, 1.0]),
    ] {
        let mut bad = contract();
        bad["normalization"]["scale"] = value;
        assert!(Frozen::parse(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
    let mut bad = contract();
    bad["availability_mask_center"][1] = json!(1.1);
    assert!(Frozen::parse(&serde_json::to_vec(&bad).unwrap()).is_err());
    let mut bad = contract();
    bad["projection"]["resolver"] = json!("changed");
    assert!(Frozen::parse(&serde_json::to_vec(&bad).unwrap()).is_err());
}

#[test]
fn export_engine_parser_policy_and_completion_are_frozen() {
    for (field, value) in [
        ("engine_identity", json!("changed")),
        ("parser_identity", json!("changed")),
        ("policy", json!({})),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        fixture(tmp.path());
        let path = tmp.path().join("export/contract.json");
        let mut c = read(&path);
        c[field] = value;
        put(&path, &c);
        let error = run(tmp.path()).unwrap_err().to_string();
        assert!(error.contains("mismatch"), "{error}");
        assert!(!tmp.path().join("numeric.jsonl").exists());
    }
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let path = tmp.path().join("export/report.json");
    let mut report = read(&path);
    report["complete"] = json!(false);
    put(&path, &report);
    assert!(
        run(tmp.path())
            .unwrap_err()
            .to_string()
            .contains("incomplete")
    );
}

#[test]
fn receipt_identity_text_and_accounting_changes_reject() {
    for (section, field, value) in [
        ("", "resolver", json!("changed")),
        ("", "parser_identity", json!("changed")),
        ("", "engine_identity", json!("changed")),
        ("", "source_sha256", json!(sha(b"wrong"))),
        ("summary", "input_bytes", json!(999)),
        ("summary", "accounted_bytes", json!(999)),
        ("summary", "windows", json!(2)),
        ("features", "token_opportunities", json!(5)),
        ("features", "represented_tokens", json!(3)),
        ("features", "expected_fallback_leaves", json!(1.0)),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        fixture(tmp.path());
        let mut row = read(&tmp.path().join("export/rows.jsonl"));
        if section.is_empty() {
            row["receipt"][field] = value;
        } else {
            row["receipt"][section][field] = value;
        }
        rewrite_row(tmp.path(), row);
        assert!(run(tmp.path()).is_err(), "accepted {section}/{field}");
        assert!(!tmp.path().join("numeric.jsonl").exists());
    }
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut row = read(&tmp.path().join("export/rows.jsonl"));
    row["metadata"]["text"] = json!("Changed source text.");
    rewrite_row(tmp.path(), row);
    assert!(
        run(tmp.path())
            .unwrap_err()
            .to_string()
            .contains("source text hash")
    );
}

#[test]
fn rows_and_receipt_digests_and_report_population_are_checked() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let path = tmp.path().join("export/report.json");
    let mut report = read(&path);
    report["rows_sha256"] = json!(sha(b"wrong"));
    put(&path, &report);
    assert!(
        run(tmp.path())
            .unwrap_err()
            .to_string()
            .contains("rows digest")
    );
    assert!(!tmp.path().join("numeric.jsonl").exists());
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    fs::write(tmp.path().join("export/00000.receipt.json"), b"{}").unwrap();
    assert!(
        run(tmp.path())
            .unwrap_err()
            .to_string()
            .contains("receipt file digest")
    );
    for prefix in ["../escape", "", "00000/extra"] {
        let tmp = tempfile::tempdir().unwrap();
        fixture(tmp.path());
        let path = tmp.path().join("export/report.json");
        let mut report = read(&path);
        report["documents"][0]["prefix"] = json!(prefix);
        put(&path, &report);
        assert!(run(tmp.path()).is_err());
    }
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let path = tmp.path().join("export/report.json");
    let mut report = read(&path);
    let duplicate = report["documents"][0].clone();
    report["documents"].as_array_mut().unwrap().push(duplicate);
    put(&path, &report);
    assert!(
        run(tmp.path())
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
}

#[test]
fn empty_document_opportunity_masks_and_execution_failures_are_distinct() {
    let frozen = Frozen::parse(&serde_json::to_vec(&contract()).unwrap()).unwrap();
    let empty = Measurements {
        tokens: 0.0,
        counts: BTreeMap::new(),
    };
    assert_eq!(
        frozen.project(&empty).unwrap(),
        vec![-0.2 / 1.3, 0.0, 0.0, 0.0, -0.25, -0.75]
    );
    let nonempty = Measurements {
        tokens: 4.0,
        counts: BTreeMap::new(),
    };
    let values = frozen.project(&nonempty).unwrap();
    assert_eq!(&values[3..], &[0.0, 0.75, 0.25]);
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut row = read(&tmp.path().join("export/rows.jsonl"));
    row["receipt"]["summary"]["execution_failures"] = json!(1);
    row["receipt"]["features"]["counts"]["availability/window/NativeComplete"] = json!(0.0);
    row["receipt"]["features"]["counts"]["availability/window/ExecutionFailure"] = json!(1.0);
    rewrite_row(tmp.path(), row);
    assert!(
        run(tmp.path())
            .unwrap_err()
            .to_string()
            .contains("document execution failure")
    );
    assert!(!tmp.path().join("numeric.jsonl").exists());
}

#[test]
fn unsupported_keys_are_rejected_even_outside_frozen_vocabulary() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut row = read(&tmp.path().join("export/rows.jsonl"));
    row["receipt"]["features"]["counts"]["label/human_only"] = json!(1.0);
    rewrite_row(tmp.path(), row);
    assert!(run(tmp.path()).is_err());
    assert!(!tmp.path().join("numeric.jsonl").exists());
}

#[test]
fn semantic_hash_ignores_contract_layout_and_existing_output_is_untouched() {
    let value = contract();
    let a = Frozen::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    let b = Frozen::parse(&serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    assert_eq!(a.hash, b.hash);
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    fs::write(tmp.path().join("numeric.jsonl"), b"existing").unwrap();
    assert!(run(tmp.path()).unwrap_err().to_string().contains("new"));
    assert_eq!(
        fs::read(tmp.path().join("numeric.jsonl")).unwrap(),
        b"existing"
    );
}
