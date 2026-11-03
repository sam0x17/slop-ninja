use super::*;
use tempfile::TempDir;

fn put(p: &Path, v: &Value) {
    fs::write(p, serde_json::to_vec(v).unwrap()).unwrap();
}
struct Fixture {
    temp: TempDir,
    selection: Value,
    selection_sha: String,
}
impl Fixture {
    fn path(&self, p: &str) -> PathBuf {
        self.temp.path().join(p)
    }
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir(root.join("parent")).unwrap();
        let contract =
            json!({"final_test_opened":false,"split_assignments":{"old-family":"train"}});
        for p in ["contract.json", "parent/contract.json"] {
            put(&root.join(p), &contract);
        }
        put(
            &root.join("parent/manifest.json"),
            &json!({"final_test_opened":false}),
        );
        let parent_sha = sha(&root.join("parent/manifest.json")).unwrap();
        let csha = digest(&serde_json::to_vec(&contract).unwrap());
        let file_sha = sha(&root.join("contract.json")).unwrap();
        let selection = json!({"candidates":[{"candidate":"fixture","numeric_path":root.join("parent"),
            "numeric_manifest_sha256":parent_sha,"contract_sha256":csha,"contract_file_sha256":file_sha}],
            "binding_files":[{"path":root.join("parent/manifest.json"),"sha256":parent_sha},
                {"path":root.join("parent/contract.json"),"sha256":file_sha}]});
        let selection_sha = digest(&serde_json::to_vec(&selection).unwrap());
        let raw=[("a","human_only",json!(0.)),("b","mixed",Value::Null)].map(|(id,origin,share)|
            json!({"id":id,"group":format!("new-{id}"),"split":"test","purpose":"query","available":id=="a",
                "metadata":{"id":id,"source_group":format!("new-{id}"),"frozen_split":"test","purpose":"query",
                    "origin":origin,"share_target":share,"text_sha256":digest(id.as_bytes())}}));
        let mut row = labelled(&raw[0]).unwrap();
        row["input"] = json!({"contract_sha256":csha});
        fs::write(root.join("test.jsonl"), format!("{}\n", row)).unwrap();
        let records_hash = digest(b"synthetic records binding");
        let source = json!({"schema":"slop_ninja_a_supervised_dataset_v1","complete":true,
            "test_text_opened":true,"test_used_for_selection":false,"selection_receipt_sha256":selection_sha,
            "records_sha256":records_hash,"non_test_ids":[],"test_ids":["a","b"],"records":2,
            "requested_records":4,"split_assignments":{"new-a":"test","new-b":"test"},
            "excluded":[{"id":"collision","reason":"duplicate_text"}],
            "generation_reports":[{"complete":true,"requested":2,"accepted":1,"failed":1,
                "failures":[{"id":"terminal","reason":"terminal_generation_failure"}]}]});
        put(&root.join("test_dataset_manifest.json"), &source);
        let dsha = sha(&root.join("test_dataset_manifest.json")).unwrap();
        put(
            &root.join("test_population.json"),
            &json!({"rows":raw,"requested":2,"available":1,
            "excluded":[{"id":"b","reason":"frozen_input_unavailable","reasons":[{"stage":"v6","reason":"unsupported_length:2049>2048"}]}],
            "fit_performed":false,"final_test_opened":true,"contract_sha256":csha,
            "frozen_manifest_sha256":parent_sha,"dataset_manifest_sha256":dsha,"records_sha256":records_hash}),
        );
        put(
            &root.join("manifest.json"),
            &json!({"schema":"slop_ninja_shared_a_dataset_v1","final_test_opened":true,
            "test_used_for_selection":false,"contract_file_sha256":file_sha,"contract_sha256":csha,
            "splits":[{"split":"test","rows":1,"sha256":sha(&root.join("test.jsonl")).unwrap()}],
            "test_transform":{"fit_performed":false,"selection_receipt_sha256":selection_sha,
                "frozen_manifest_sha256":parent_sha,"dataset_manifest_sha256":dsha,
                "audit_file":"test_population.json","audit_file_sha256":sha(&root.join("test_population.json")).unwrap(),
                "requested":2,"available":1,"excluded":1}}),
        );
        Self {
            temp,
            selection,
            selection_sha,
        }
    }
    fn load(&self) -> Result<TestData> {
        load_data(self.temp.path(), &self.selection, &self.selection_sha)
    }
    fn refresh(&self) {
        let mut m = read(&self.path("manifest.json")).unwrap();
        m["test_transform"]["audit_file_sha256"] =
            json!(sha(&self.path("test_population.json")).unwrap());
        m["splits"][0]["sha256"] = json!(sha(&self.path("test.jsonl")).unwrap());
        put(&self.path("manifest.json"), &m);
    }
}

#[test]
fn full_requested_population_retains_all_stages_and_unknown_share() {
    let f = Fixture::new();
    let d = f.load().unwrap();
    assert_eq!(d.rows.len(), 2);
    assert_eq!(d.numeric.len(), 1);
    assert_eq!(d.upstream["requested"], 4);
    assert_eq!(d.upstream["assembled"], 2);
    assert!(d.rows["b"]["targets"]["share"].is_null());
    assert_eq!(d.projection_excluded[0]["reasons"][0]["stage"], "v6");
}
#[test]
fn rehashed_numeric_supervision_change_is_rejected() {
    let f = Fixture::new();
    let mut rows = jsonl(&f.path("test.jsonl")).unwrap();
    rows[0]["targets"]["origin"] = json!(1);
    fs::write(f.path("test.jsonl"), format!("{}\n", rows[0])).unwrap();
    f.refresh();
    assert!(f.load().is_err());
}
#[test]
fn projection_rejects_cross_split_and_reserved_family() {
    for family in [false, true] {
        let f = Fixture::new();
        let mut p = read(&f.path("test_population.json")).unwrap();
        if family {
            p["rows"][1]["group"] = json!("old-family");
            p["rows"][1]["metadata"]["source_group"] = json!("old-family");
        } else {
            p["rows"][1]["split"] = json!("calibration");
        }
        put(&f.path("test_population.json"), &p);
        f.refresh();
        assert!(f.load().is_err());
    }
}
#[test]
fn changed_contract_cannot_be_self_rehashed_into_frozen_parent() {
    let f = Fixture::new();
    let mut c = read(&f.path("contract.json")).unwrap();
    c["normalization"] = json!("refitted");
    put(&f.path("contract.json"), &c);
    let mut m = read(&f.path("manifest.json")).unwrap();
    m["contract_file_sha256"] = json!(sha(&f.path("contract.json")).unwrap());
    m["contract_sha256"] = json!(digest(&serde_json::to_vec(&c).unwrap()));
    put(&f.path("manifest.json"), &m);
    assert!(f.load().is_err());
}
#[test]
fn upstream_and_projection_denominators_must_reconcile() {
    let f = Fixture::new();
    let source = read(&f.path("test_dataset_manifest.json")).unwrap();
    let accepted = ["a".into(), "b".into()].into_iter().collect();
    for bad in ["count", "duplicate", "failures", "crosssplit"] {
        let mut v = source.clone();
        match bad {
            "count" => v["requested_records"] = json!(3),
            "duplicate" => v["excluded"][0]["id"] = json!("a"),
            "failures" => v["generation_reports"][0]["failures"] = json!([]),
            _ => v["non_test_ids"] = json!(["a"]),
        }
        assert!(upstream_accounting(&v, &accepted).is_err());
    }
    let mut p = read(&f.path("test_population.json")).unwrap();
    p["excluded"][0]["id"] = json!("a");
    put(&f.path("test_population.json"), &p);
    f.refresh();
    assert!(f.load().is_err());
}
#[test]
fn predictions_bind_exact_run_and_account_for_every_numeric_id() {
    let f = Fixture::new();
    let d = f.load().unwrap();
    let run = json!({"weights_sha256":digest(b"weights"),"fit_sha256":digest(b"fit")});
    put(
        &f.path("predictions.json"),
        &json!([{"id":"a","prediction":{"ai_confidence_percent":10.,"ai_share_percent":4.}}]),
    );
    let receipt = json!({"schema":"slop_ninja_final_test_predictions_v1","complete":true,"split":"test","fit_performed":false,
        "selection_receipt_sha256":d.selection_sha,"numeric_manifest_sha256":d.manifest_sha,"query_sha256":d.query_sha,
        "contract_sha256":d.contract_sha,"weights_sha256":run["weights_sha256"],"fit_sha256":run["fit_sha256"],
        "predictions_file":"predictions.json","predictions_sha256":sha(&f.path("predictions.json")).unwrap(),
        "requested":1,"supported":1,"excluded":[]});
    put(&f.path("receipt.json"), &receipt);
    assert_eq!(
        predictions(&f.path("receipt.json"), &d, &run)
            .unwrap()
            .values
            .len(),
        1
    );
    let mut changed = receipt.clone();
    changed["fit_sha256"] = json!(digest(b"other fit"));
    put(&f.path("receipt.json"), &changed);
    assert!(predictions(&f.path("receipt.json"), &d, &run).is_err());
    put(&f.path("predictions.json"), &json!([]));
    let mut failed = receipt;
    failed["predictions_sha256"] = json!(sha(&f.path("predictions.json")).unwrap());
    failed["supported"] = json!(0);
    failed["excluded"] = json!([{"id":"a","reason":"runtime_failure"}]);
    put(&f.path("receipt.json"), &failed);
    assert_eq!(
        predictions(&f.path("receipt.json"), &d, &run)
            .unwrap()
            .excluded
            .len(),
        1
    );
    failed["excluded"] = json!([]);
    put(&f.path("receipt.json"), &failed);
    assert!(predictions(&f.path("receipt.json"), &d, &run).is_err());
}
