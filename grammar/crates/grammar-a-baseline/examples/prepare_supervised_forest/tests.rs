//! Synthetic fixtures only; no corpus, parser, detector or provider invocation.
use super::*;

const SYMBOL: &str = r#"native_complete/symbol/{"category":"Noun","features":{}}"#;
const LEAF: &str =
    r#"native_complete/leaf/["inventory","NOUN",{"category":"Noun","features":{}},null]"#;
const HELDOUT: &str = r#"native_complete/symbol/{"category":"Verb","features":{}}"#;

fn metadata(id: &str, group: &str, split: &str) -> Value {
    json!({"id":id,"text":"One two. Three four five.","source_group":group,"frozen_split":split,
        "origin":"human_only","evidence":"synthetic historical human proxy",
        "exposed_to_grammar":false,"share_target":0.0,
        "share_evidence":{"basis":"historical_human_proxy_endpoint"},
        "human_identity":"writer_a","human_role_present":true,
        "ai_identity":null,"ai_role_present":false,"purpose":"query"})
}

fn generated(mut row: Value, mode: &str, full: bool) -> Value {
    let source = row["text"].as_str().unwrap().to_owned();
    let replacement = "A longer replacement with words.";
    let end = if mode == "from_scratch" {
        0
    } else if full {
        source.len()
    } else {
        8
    };
    let text = if mode == "from_scratch" {
        replacement.into()
    } else {
        format!("{replacement}{}", &source[end..])
    };
    let share =
        replacement.split_whitespace().count() as f64 / text.split_whitespace().count() as f64;
    row["text"] = json!(text);
    row["text_sha256"] = json!(sha(text.as_bytes()));
    row["origin"] = json!(if mode == "from_scratch" {
        "model_only"
    } else {
        "mixed"
    });
    row["share_target"] = json!(share);
    row["ai_identity"] = json!("model_a");
    row["ai_role_present"] = json!(true);
    if mode == "from_scratch" {
        row["human_identity"] = Value::Null;
        row["human_role_present"] = json!(false);
    }
    row["generation"] = json!({"mode":mode});
    row["share_evidence"] = json!({"schema":"slop_ninja_realized_span_share_v1",
        "source_text":source,"source_sha256":sha(source.as_bytes()),"source_byte_span":[0,end],
        "replacement":replacement,"final_byte_span":[0,replacement.len()],"final_sha256":sha(text.as_bytes()),
        "replacement_words":replacement.split_whitespace().count(),"final_words":text.split_whitespace().count(),
        "word_policy":"unicode_split_whitespace_v1","realized_share":share});
    row
}

fn manifest(rows: &[Value]) -> Value {
    let mut assignments = BTreeMap::<String, Value>::new();
    for row in rows {
        assignments.insert(
            row["source_group"].as_str().unwrap().into(),
            row["frozen_split"].clone(),
        );
    }
    assignments.insert("reserved_family".into(), json!("test"));
    json!({"schema":"slop_ninja_a_supervised_dataset_v1","records_sha256":sha(&jsonl(rows)),
        "non_test_ids":rows.iter().map(|r| &r["id"]).collect::<Vec<_>>(),
        "test_ids":["reserved_test"],"split_assignments":assignments,
        "sources":{"description":"synthetic test fixtures only"}})
}

pub(super) fn jsonl(rows: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

pub(super) fn put(path: &Path, value: &Value) {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    fs::write(path, bytes).unwrap();
}

pub(super) fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

pub(super) fn read_rows(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub(super) fn fixture(root: &Path, heldout_count: f64) {
    fs::create_dir(root.join("export")).unwrap();
    let mut train_b = generated(
        metadata("t2", "train_b", "train"),
        "controlled_rewrite",
        false,
    );
    train_b["human_identity"] = json!("writer_b");
    train_b["ai_identity"] = json!("model_b");
    let mut dev = generated(
        metadata("dev", "dev_family", "development"),
        "controlled_rewrite",
        false,
    );
    dev["human_identity"] = json!("unseen_dev_writer");
    let mut cal = generated(
        metadata("cal", "cal_family", "calibration"),
        "controlled_rewrite",
        true,
    );
    cal["ai_identity"] = json!("unseen_cal_model");
    let mut reference = generated(
        metadata("ref", "reference_family", "train"),
        "controlled_rewrite",
        false,
    );
    reference["purpose"] = json!("reference");
    reference["human_identity"] = json!("reference_only_writer");
    reference["ai_identity"] = json!("reference_only_model");
    let records = vec![
        generated(
            metadata("t1", "train_a", "train"),
            "controlled_rewrite",
            false,
        ),
        train_b,
        dev,
        cal,
        reference,
    ];
    let dataset = manifest(&records);
    put(&root.join("dataset.json"), &dataset);
    put(
        &root.join("export/contract.json"),
        &json!({
        "schema":"slop_ninja_document_export_contract_v1","test_populations_excluded":false,
        "engine_identity":"synthetic_engine","parser_identity":"synthetic_parser","policy":{},
        "input_sha256":dataset["records_sha256"]}),
    );
    let mut raw = Vec::new();
    let mut entries = Vec::new();
    for (i, metadata) in records.into_iter().enumerate() {
        let text = metadata["text"].as_str().unwrap();
        let summary = json!({"tokens":4,"represented_tokens":4,"unresolved_tokens":0,
            "input_bytes":text.len(),"accounted_bytes":text.len(),"expected_fallback_leaves":0.0,
            "execution_failures":0,"windows":1});
        let mut counts = serde_json::Map::new();
        counts.insert(
            SYMBOL.into(),
            json!(if i < 2 {
                (i + 1) as f64 * 2.0
            } else {
                heldout_count
            }),
        );
        counts.insert(LEAF.into(), json!(2.0));
        counts.insert("availability/window/NativeComplete".into(), json!(1.0));
        if i >= 2 {
            counts.insert(HELDOUT.into(), json!(heldout_count));
        }
        let receipt = json!({"schema":"slop_ninja_cfg_document_fallback_v1",
            "feature_schema":"slop_ninja_cfg_forest_expectations_v1",
            "resolver":"acyclic-uniform-derivations-max-covered-min-islands-v1",
            "engine_identity":"synthetic_engine","parser_identity":"synthetic_parser",
            "source_sha256":sha(text.as_bytes()),"summary":summary,
            "features":{"token_opportunities":4,"represented_tokens":4,"unresolved_tokens":0,
                "expected_fallback_leaves":0.0,"projection_available":true,"search_exhaustive":true,"counts":counts}});
        let prefix = format!("{i:05}");
        let path = root.join(format!("export/{prefix}.receipt.json"));
        put(&path, &receipt);
        entries.push(json!({"id":metadata["id"],"prefix":prefix,"summary":summary}));
        raw.push(json!({"id":metadata["id"],"metadata":metadata,"receipt":receipt,
            "receipt_sha256":sha(&fs::read(path).unwrap()),"windows_sha256":sha(b"unopened windows")}));
    }
    let bytes = jsonl(&raw);
    fs::write(root.join("export/rows.jsonl"), &bytes).unwrap();
    put(
        &root.join("export/report.json"),
        &json!({"schema":"slop_ninja_document_export_report_v1",
        "complete":true,"engine_identity":"synthetic_engine","documents":entries,
        "rows_sha256":sha(&bytes),"excluded_reserved_test_documents":0}),
    );
}

pub(super) fn run(root: &Path, ablation: Ablation) -> Result<()> {
    prepare(
        &root.join("export"),
        &root.join("dataset.json"),
        &root.join("numeric"),
        ablation,
    )
}

#[test]
fn endpoint_targets_are_explicit_and_origin_consistent() {
    let mut row = metadata("a", "family", "train");
    row["share_target"] = Value::Null;
    assert_eq!(Supervision::parse(&row).unwrap().share, None);
    row["share_target"] = json!(0.2);
    assert!(Supervision::parse(&row).is_err());
    row["share_target"] = json!(-0.1);
    assert!(Supervision::parse(&row).is_err());
    row["share_target"] = json!(1.1);
    assert!(Supervision::parse(&row).is_err());
    row.as_object_mut().unwrap().remove("share_target");
    assert!(Supervision::parse(&row).is_err());
    let mut model = generated(metadata("m", "family", "train"), "from_scratch", false);
    assert_eq!(Supervision::parse(&model).unwrap().share, Some(1.0));
    model["share_target"] = json!(0.8);
    assert!(Supervision::parse(&model).is_err());
}

#[test]
fn measured_mixed_share_recomputes_words_assembly_and_spans() {
    let row = generated(
        metadata("a", "family", "train"),
        "controlled_rewrite",
        false,
    );
    assert_eq!(Supervision::parse(&row).unwrap().share, Some(5.0 / 8.0));
    for (field, value) in [
        ("replacement_words", json!(6)),
        ("final_words", json!(9)),
        ("realized_share", json!(0.75)),
        ("final_byte_span", json!([1, 32])),
        ("source_byte_span", json!([0, 7])),
        ("source_sha256", json!(sha(b"wrong"))),
        ("final_sha256", json!(sha(b"wrong"))),
        ("word_policy", json!("other")),
        ("replacement", json!("Other replacement.")),
    ] {
        let mut bad = row.clone();
        bad["share_evidence"][field] = value;
        assert!(
            Supervision::parse(&bad).is_err(),
            "accepted changed {field}"
        );
    }
    let mut masked = row.clone();
    masked["share_target"] = Value::Null;
    assert_eq!(Supervision::parse(&masked).unwrap().share, None);
    let mut no_evidence = row.clone();
    no_evidence["share_evidence"] = json!({});
    assert!(Supervision::parse(&no_evidence).is_err());
    let full = generated(metadata("a", "family", "train"), "controlled_rewrite", true);
    let target = Supervision::parse(&full).unwrap();
    assert_eq!((target.origin, target.share), (2, Some(1.0)));
}

#[test]
fn exposure_and_identity_truth_are_preserved_with_masked_losses() {
    let mut row = metadata("a", "family", "train");
    let a = Supervision::parse(&row).unwrap();
    assert!(!a.exposed && !a.exposure_was_unspecified);
    row["exposed_to_grammar"] = Value::Null;
    let unknown = Supervision::parse(&row).unwrap();
    assert!(unknown.exposed && unknown.exposure_was_unspecified);
    let one = Profiles::fit([&a].into_iter());
    assert!(one.human.is_empty());
    assert!(one.targets(&a)["human"].is_null());
    row["human_identity"] = json!("writer_b");
    let b = Supervision::parse(&row).unwrap();
    let profiles = Profiles::fit([&a, &b].into_iter());
    assert_eq!(profiles.targets(&a)["human"], 0);
    row["human_identity"] = json!("unseen");
    let unseen = Supervision::parse(&row).unwrap();
    assert!(profiles.targets(&unseen)["human"].is_null());
    assert_eq!(
        profiles.roles(&unseen)["human"],
        json!({"present":true,"identity":"unseen","known":false})
    );
    row["human_identity"] = json!("writer_a");
    row["human_role_present"] = Value::Null;
    assert!(profiles.targets(&Supervision::parse(&row).unwrap())["human"].is_null());
}

#[test]
fn opportunity_masks_distinguish_absent_counts_from_no_tokens() {
    let mut row = Row {
        id: "a".into(),
        group: "f".into(),
        split: "train".into(),
        supervision: Supervision::parse(&metadata("a", "f", "train")).unwrap(),
        available: true,
        tokens: 4.0,
        counts: BTreeMap::new(),
    };
    assert_eq!(measurement(&row, SYMBOL).unwrap(), (0.0, 1.0));
    row.tokens = 0.0;
    assert_eq!(measurement(&row, SYMBOL).unwrap(), (0.0, 0.0));
    assert_eq!(measurement(&row, FIXED[0]).unwrap(), (0.0, 1.0));
    row.counts.insert(SYMBOL.into(), 1.0);
    assert!(measurement(&row, SYMBOL).is_err());
}

#[test]
fn allow_list_rejects_test_unknown_family_crossing_and_reference_leakage() {
    let row = metadata("a", "family", "train");
    let value = manifest(&[row.clone()]);
    let dataset = Dataset::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(dataset.authorize_id("reserved_test").is_err());
    assert!(dataset.authorize_id("unknown").is_err());
    for (field, changed) in [
        ("frozen_split", "test"),
        ("frozen_split", "development"),
        ("source_group", "unknown"),
        ("id", "reserved_test"),
    ] {
        let mut bad = row.clone();
        bad[field] = json!(changed);
        assert!(
            dataset
                .check_headers(&jsonl(&[json!({"id":"a","metadata":bad})]))
                .is_err()
        );
    }
    let mut dev = metadata("d", "development_family", "development");
    dev["purpose"] = json!("reference");
    let dataset = Dataset::parse(&serde_json::to_vec(&manifest(&[dev.clone()])).unwrap()).unwrap();
    assert!(dataset.validate_metadata("d", &dev).is_err());
    let mut overlapping = value.clone();
    overlapping["test_ids"] = json!(["a"]);
    assert!(Dataset::parse(&serde_json::to_vec(&overlapping).unwrap()).is_err());
    let mut test_records = value;
    test_records["test_ids"] = json!([{"id":"reserved_test","text":"forbidden"}]);
    assert!(Dataset::parse(&serde_json::to_vec(&test_records).unwrap()).is_err());
}

#[test]
fn end_to_end_train_query_fit_unknown_roles_reference_and_hash_contract() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), 1000.0);
    run(tmp.path(), Ablation::All).unwrap();
    let out = tmp.path().join("numeric");
    let manifest = read(&out.join("manifest.json"));
    let contract = read(&out.join("contract.json"));
    assert_eq!(contract["human_profiles"], json!(["writer_a", "writer_b"]));
    assert_eq!(contract["ai_profiles"], json!(["model_a", "model_b"]));
    assert!(
        !contract["coordinates"]
            .as_array()
            .unwrap()
            .contains(&json!(HELDOUT))
    );
    assert_eq!(
        manifest["contract_sha256"],
        sha(&serde_json::to_vec(&contract).unwrap())
    );
    assert_eq!(
        manifest["contract_file_sha256"],
        sha(&fs::read(out.join("contract.json")).unwrap())
    );
    assert_eq!(
        manifest["population_file_sha256"],
        sha(&fs::read(out.join("population.json")).unwrap())
    );
    for binding in manifest["splits"].as_array().unwrap() {
        let path = out.join(format!("{}.jsonl", binding["split"].as_str().unwrap()));
        assert_eq!(binding["sha256"], sha(&fs::read(&path).unwrap()));
        for row in read_rows(&path) {
            assert_eq!(row["input"]["contract_sha256"], manifest["contract_sha256"]);
            assert_eq!(row["input"].as_object().unwrap().len(), 2);
        }
    }
    let train = read_rows(&out.join("train.jsonl"));
    assert_eq!(train.len(), 2);
    assert!(train.iter().all(|r| r["purpose"] == "query"));
    let dev = read_rows(&out.join("development.jsonl"));
    assert!(dev[0]["targets"]["human"].is_null());
    assert_eq!(dev[0]["roles"]["human"]["identity"], "unseen_dev_writer");
    assert_eq!(dev[0]["roles"]["human"]["known"], false);
    let cal = read_rows(&out.join("calibration.jsonl"));
    assert!(cal[0]["targets"]["ai"].is_null());
    let references = read_rows(&out.join("reference.jsonl"));
    assert_eq!(references.len(), 1);
    assert_eq!(references[0]["purpose"], "reference");
    assert!(
        references[0]["targets"]
            .as_object()
            .unwrap()
            .values()
            .all(Value::is_null)
    );
    let population = read(&out.join("population.json"));
    assert!(
        population["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["metadata"].get("text").is_none())
    );
    assert!(!out.join("test.jsonl").exists());
    let index = contract["coordinates"]
        .as_array()
        .unwrap()
        .iter()
        .position(|k| k == SYMBOL)
        .unwrap();
    let expected_mean = (0.5_f64.ln_1p() + 1.0_f64.ln_1p()) / 2.0;
    assert!(
        (contract["normalization"]["mean"][index].as_f64().unwrap() - expected_mean).abs() < 1e-12
    );
}

#[test]
fn heldout_and_reference_measurements_cannot_change_fit() {
    let a = tempfile::tempdir().unwrap();
    fixture(a.path(), 10.0);
    run(a.path(), Ablation::All).unwrap();
    let b = tempfile::tempdir().unwrap();
    fixture(b.path(), 9000.0);
    run(b.path(), Ablation::All).unwrap();
    let ca = read(&a.path().join("numeric/contract.json"));
    let cb = read(&b.path().join("numeric/contract.json"));
    for field in [
        "coordinates",
        "normalization",
        "availability_mask_center",
        "human_profiles",
        "ai_profiles",
    ] {
        assert_eq!(ca[field], cb[field], "held-out data affected {field}");
    }
    let ta = read_rows(&a.path().join("numeric/train.jsonl"));
    let tb = read_rows(&b.path().join("numeric/train.jsonl"));
    assert_eq!(ta[0]["input"]["x"], tb[0]["input"]["x"]);
}

#[test]
fn ablations_and_feature_guards_are_explicit() {
    assert!(Ablation::parse("lexical").is_err());
    for mode in [
        Ablation::Structure,
        Ablation::LeafGrammar,
        Ablation::Availability,
    ] {
        let tmp = tempfile::tempdir().unwrap();
        fixture(tmp.path(), 10.0);
        run(tmp.path(), mode).unwrap();
        let contract = read(&tmp.path().join("numeric/contract.json"));
        assert!(
            contract["coordinates"]
                .as_array()
                .unwrap()
                .iter()
                .all(|k| mode.includes(k.as_str().unwrap()))
        );
    }
    for key in [
        "human_identity/writer_a",
        "source_group/train_a",
        "availability/window/unknown",
        r#"native_complete/symbol/{"category":"Noun","features":{"lemma":"writer_a"}}"#,
        r#"native_complete/symbol/{"category":"Noun","features":{},"identity":"writer_a"}"#,
    ] {
        assert!(validate_key(key).is_err());
    }
    assert!(validate_key(SYMBOL).is_ok());
    assert!(validate_key(LEAF).is_ok());
}

#[test]
fn report_test_rejection_precedes_any_row_or_receipt_read() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), 10.0);
    let path = tmp.path().join("export/report.json");
    let mut report = read(&path);
    report["documents"][0]["id"] = json!("reserved_test");
    put(&path, &report);
    fs::remove_file(tmp.path().join("export/rows.jsonl")).unwrap();
    let error = run(tmp.path(), Ablation::All).unwrap_err().to_string();
    assert!(error.contains("reserved Test ID"), "{error}");
    assert!(!tmp.path().join("numeric").exists());
}

#[test]
fn changed_record_binding_and_receipts_fail_before_publication() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), 10.0);
    let path = tmp.path().join("dataset.json");
    let mut dataset = read(&path);
    dataset["records_sha256"] = json!(sha(b"wrong"));
    put(&path, &dataset);
    assert!(
        run(tmp.path(), Ablation::All)
            .unwrap_err()
            .to_string()
            .contains("input hash mismatch")
    );
    assert!(!tmp.path().join("numeric").exists());
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), 10.0);
    fs::write(tmp.path().join("export/00000.receipt.json"), b"{}").unwrap();
    assert!(
        run(tmp.path(), Ablation::All)
            .unwrap_err()
            .to_string()
            .contains("receipt digest mismatch")
    );
    assert!(!tmp.path().join("numeric").exists());
}
