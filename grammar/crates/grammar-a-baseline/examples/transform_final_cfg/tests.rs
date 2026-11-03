//! Synthetic exports only. No parser, model, provider or retained corpus reads.
use super::super::super::tests as base;
use super::*;
use tempfile::TempDir;

struct Fixture {
    tmp: TempDir,
    selection: Value,
    sb: Vec<u8>,
}
impl Fixture {
    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }
    fn run(&self, name: &str) -> Result<()> {
        prepare_validated(
            &self.selection,
            &self.sb,
            name,
            &self.path("records.jsonl"),
            &self.path("test-dataset.json"),
            &self.path("test-export"),
            &self.path(&format!("out-{name}")),
        )
    }
    fn new(failed: usize, reverse: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        base::fixture(root, 2.);
        for (name, ablation) in [
            ("cfg_all", Ablation::All),
            ("cfg_leaf", Ablation::LeafGrammar),
        ] {
            super::super::super::prepare(
                &root.join("export"),
                &root.join("dataset.json"),
                &root.join(name),
                ablation,
            )
            .unwrap();
        }
        let leaf_path = root.join("cfg_leaf");
        let leaf = read(&leaf_path.join("contract.json")).unwrap();
        let lm = read(&leaf_path.join("manifest.json")).unwrap();
        assert_eq!(leaf["coordinates"].as_array().unwrap().len(), 1);
        for (name, variant) in [("ordered_cfg", "ordered"), ("global_cfg_control", "global")] {
            let dir = root.join(name);
            fs::create_dir(&dir).unwrap();
            let c = json!({"schema":"slop_ninja_shared_a_input_v1","feature_policy":"slop_ninja_four_bin_cfg_leaves_v1",
                "variant":variant,"bins":4,"input_dimension":8,"coordinates":leaf["coordinates"],
                "normalization":{"fit":"Train queries only, equal source family, masks explicit","scale_floor":0.01,
                    "mean":[0.1,0.2,0.3,0.4],"scale":[1.,2.,3.,4.],"mask_center":[0.25,0.5,0.75,1.]},
                "layout":"bin-major standardized log1p counts divided by document tokens, then centered masks",
                "position":"each parser window assigned by token midpoint; no within-window token order",
                "global_control":"one-quarter of document counts broadcast into each bin; same dimension",
                "human_profiles":leaf["human_profiles"],"ai_profiles":leaf["ai_profiles"],"origin_order":leaf["origin_order"],
                "grammar_identities":leaf["grammar_identities"],"document_policy":leaf["document_policy"],
                "split_assignments":leaf["split_assignments"],"parent_contract_sha256":lm["contract_sha256"],
                "parent_manifest_sha256":sha(&fs::read(leaf_path.join("manifest.json")).unwrap()),
                "source_sha256":sha(include_bytes!("../prepare_ordered_cfg.rs")),"final_test_opened":false});
            let ch = sha(&serde_json::to_vec(&c).unwrap());
            base::put(&dir.join("contract.json"), &c);
            fs::copy(
                leaf_path.join("population.json"),
                dir.join("population.json"),
            )
            .unwrap();
            let mut m = lm.clone();
            m["contract_sha256"] = json!(ch);
            m["contract_file_sha256"] = json!(sha(&fs::read(dir.join("contract.json")).unwrap()));
            m["feature_dimension"] = json!(8);
            for split in m["splits"].as_array_mut().unwrap() {
                let file = format!("{}.jsonl", split["split"].as_str().unwrap());
                let mut rr = indexed(&fs::read(leaf_path.join(&file)).unwrap()).unwrap();
                for r in rr.values_mut() {
                    r["input"] = json!({"contract_sha256":ch,"x":vec![0.;8]});
                }
                let bytes = jsonl(rr.into_values()).unwrap();
                split["sha256"] = json!(sha(&bytes));
                fs::write(dir.join(file), bytes).unwrap();
            }
            base::put(&dir.join("manifest.json"), &m);
        }
        let mut candidates = Vec::new();
        let mut bindings = Vec::new();
        for name in ["cfg_all", "cfg_leaf", "ordered_cfg", "global_cfg_control"] {
            let dir = root.join(name);
            let m = read(&dir.join("manifest.json")).unwrap();
            let mh = sha(&fs::read(dir.join("manifest.json")).unwrap());
            candidates.push(json!({"candidate":name,"numeric_path":dir,"numeric_manifest_sha256":mh,
                "contract_file_sha256":m["contract_file_sha256"],"contract_sha256":m["contract_sha256"]}));
            for file in ["manifest.json", "contract.json"] {
                bindings.push(
                    json!({"path":dir.join(file),"sha256":sha(&fs::read(dir.join(file)).unwrap())}),
                );
            }
        }
        let selection = json!({"schema":"slop_ninja_balanced_a_selection_freeze_v3",
            "candidates":candidates,"binding_files":bindings});
        let sb = serde_json::to_vec_pretty(&selection).unwrap();
        let ex = root.join("test-export");
        fs::create_dir(&ex).unwrap();
        let original = base::read_rows(&root.join("export/rows.jsonl"));
        let mut raw = Vec::new();
        let mut records = Vec::new();
        let mut entries = Vec::new();
        let key = leaf["coordinates"][0].as_str().unwrap();
        for i in 0..2 {
            let id = format!("test-{i}");
            let mut r = original[i + 2].clone();
            r["id"] = json!(id);
            r["metadata"]["id"] = json!(id);
            r["metadata"]["frozen_split"] = json!("test");
            r["metadata"]["source_group"] = json!(format!("new-work-{i}"));
            r["metadata"]["human_identity"] = json!("withheld-test-writer");
            r["metadata"]["ai_identity"] = json!("withheld-test-model");
            r["receipt"]["summary"]["windows"] = json!(2);
            r["receipt"]["features"]["counts"]["availability/window/NativeComplete"] = json!(2.);
            if i < failed {
                r["receipt"]["summary"]["execution_failures"] = json!(1);
                r["receipt"]["features"]["counts"]["availability/window/NativeComplete"] =
                    json!(1.);
                r["receipt"]["features"]["counts"]["availability/window/ExecutionFailure"] =
                    json!(1.);
                r["receipt"]["features"]["projection_available"] = json!(false);
            }
            let prefix = format!("{:05}", 9 - i); // Use report prefixes, never row indices.
            let counts = if reverse { [0., 2.] } else { [2., 0.] };
            let windows = jsonl(counts.into_iter().map(|n| {
                json!({"tokens":[{},{}],
                "features":{"counts":{key:n},"projection_available":true}})
            }))
            .unwrap();
            fs::write(ex.join(format!("{prefix}.windows.jsonl")), &windows).unwrap();
            r["windows_sha256"] = json!(sha(&windows));
            base::put(&ex.join(format!("{prefix}.receipt.json")), &r["receipt"]);
            r["receipt_sha256"] = json!(sha(
                &fs::read(ex.join(format!("{prefix}.receipt.json"))).unwrap()
            ));
            records.push(r["metadata"].clone());
            entries.push(json!({"id":id,"prefix":prefix,"summary":r["receipt"]["summary"]}));
            raw.push(r);
        }
        let rb = jsonl(records).unwrap();
        fs::write(root.join("records.jsonl"), &rb).unwrap();
        let mut ec = read(&root.join("export/contract.json")).unwrap();
        ec["input_sha256"] = json!(sha(&rb));
        base::put(&ex.join("contract.json"), &ec);
        let bytes = jsonl(raw).unwrap();
        fs::write(ex.join("rows.jsonl"), &bytes).unwrap();
        base::put(
            &ex.join("report.json"),
            &json!({"schema":"slop_ninja_document_export_report_v1",
            "complete":true,"engine_identity":"synthetic_engine","rows_sha256":sha(&bytes),"documents":entries}),
        );
        base::put(
            &root.join("test-dataset.json"),
            &json!({"schema":"slop_ninja_a_supervised_dataset_v1",
            "complete":true,"test_text_opened":true,"test_used_for_selection":false,"selection_receipt_sha256":sha(&sb),
            "records_sha256":sha(&rb),"test_records_sha256":sha(&rb),"non_test_ids":[],"test_ids":["test-0","test-1"],
            "split_assignments":{"new-work-0":"test","new-work-1":"test"},"sources":["synthetic fixtures"],
            "records":2,"requested_records":3,"excluded":[],"generation_reports":[{"complete":true,
                "requested":1,"accepted":0,"failed":1,"failures":[{"id":"terminal","reason":"synthetic terminal failure"}]}]}),
        );
        Self { tmp, selection, sb }
    }
}

#[test]
fn four_candidates_preserve_parent_files_and_consumer_accounting() {
    let f = Fixture::new(1, false);
    for name in ["cfg_all", "cfg_leaf", "ordered_cfg", "global_cfg_control"] {
        f.run(name).unwrap();
        let out = f.path(&format!("out-{name}"));
        for file in [
            "contract.json",
            "population.json",
            "train.jsonl",
            "development.jsonl",
            "calibration.jsonl",
            "reference.jsonl",
        ] {
            assert_eq!(
                fs::read(f.path(name).join(file)).unwrap(),
                fs::read(out.join(file)).unwrap()
            );
        }
        let data = evaluation_binding::load_data(&out, &f.selection, &sha(&f.sb)).unwrap();
        assert_eq!(data.rows.len(), 2);
        assert_eq!(data.numeric.len(), 1);
        assert_eq!(data.projection_excluded.len(), 1);
        assert_eq!(data.upstream["requested"], 3);
        let row = &data.numeric["test-1"];
        assert_eq!(row["roles"]["human"]["known"], false);
        assert_eq!(row["roles"]["ai"]["known"], false);
        assert!(row["targets"]["human"].is_null());
        assert!(row["targets"]["ai"].is_null());
        assert_eq!(row["targets"]["share"], 1.);
        if matches!(name, "cfg_all" | "cfg_leaf") {
            let legacy = f.path(&format!("legacy-{name}"));
            super::super::transform(
                &f.path(name),
                &f.path("test-export"),
                &f.path("test-dataset.json"),
                &legacy,
            )
            .unwrap();
            assert_eq!(
                fs::read(out.join("test.jsonl")).unwrap(),
                fs::read(legacy.join("test.jsonl")).unwrap()
            );
        }
        assert!(f.run(name).is_err());
    }
}

#[test]
fn ordered_uses_frozen_midpoint_bins_while_global_ignores_window_order() {
    let a = Fixture::new(0, false);
    let b = Fixture::new(0, true);
    for name in ["ordered_cfg", "global_cfg_control"] {
        a.run(name).unwrap();
        b.run(name).unwrap();
        let rows = |f: &Fixture| {
            indexed(&fs::read(f.path(&format!("out-{name}/test.jsonl"))).unwrap()).unwrap()
        };
        let av = rows(&a);
        let bv = rows(&b);
        if name == "ordered_cfg" {
            assert_ne!(av["test-0"]["input"], bv["test-0"]["input"]);
            let x = av["test-0"]["input"]["x"].as_array().unwrap();
            let expected = [
                0.,
                (1.5_f64.ln() - 0.2) / 2.,
                0.,
                -0.1,
                -0.25,
                0.5,
                -0.75,
                0.,
            ];
            for (value, expected) in x.iter().zip(expected) {
                assert!((value.as_f64().unwrap() - expected).abs() < 1e-14);
            }
        } else {
            assert_eq!(av, bv);
        }
    }
}

#[test]
fn all_excluded_population_is_preserved_without_fabricated_vectors() {
    let f = Fixture::new(2, false);
    for name in ["cfg_all", "cfg_leaf", "ordered_cfg", "global_cfg_control"] {
        f.run(name).unwrap();
        let out = f.path(&format!("out-{name}"));
        let data = evaluation_binding::load_data(&out, &f.selection, &sha(&f.sb)).unwrap();
        assert!(data.numeric.is_empty());
        assert_eq!(data.rows.len(), 2);
        assert_eq!(data.projection_excluded.len(), 2);
        assert_eq!(
            read(&out.join("manifest.json")).unwrap()["evaluation_complete"],
            false
        );
    }
}

#[test]
fn changed_selection_records_window_or_parent_fails_before_publication() {
    for bad in ["selection", "records", "window", "parent"] {
        let f = Fixture::new(0, false);
        match bad {
            "selection" => {
                let mut d = read(&f.path("test-dataset.json")).unwrap();
                d["selection_receipt_sha256"] = json!("0".repeat(64));
                base::put(&f.path("test-dataset.json"), &d);
            }
            "records" => {
                fs::write(f.path("records.jsonl"), b"changed").unwrap();
            }
            "window" => {
                fs::write(f.path("test-export/00009.windows.jsonl"), b"changed").unwrap();
            }
            _ => {
                fs::write(f.path("ordered_cfg/contract.json"), b"changed").unwrap();
            }
        }
        assert!(f.run("ordered_cfg").is_err(), "{bad}");
        assert!(!f.path("out-ordered_cfg").exists());
    }
}

#[test]
fn window_counts_must_match_bound_document_receipt_even_after_rehashing() {
    let f = Fixture::new(0, false);
    let path = f.path("test-export/00009.windows.jsonl");
    let mut windows = base::read_rows(&path);
    let key = windows[0]["features"]["counts"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    windows[0]["features"]["counts"][key] = json!(5.);
    let bytes = jsonl(windows).unwrap();
    fs::write(&path, &bytes).unwrap();
    let mut raw = base::read_rows(&f.path("test-export/rows.jsonl"));
    raw[0]["windows_sha256"] = json!(sha(&bytes));
    let bytes = jsonl(raw).unwrap();
    fs::write(f.path("test-export/rows.jsonl"), &bytes).unwrap();
    let mut report = read(&f.path("test-export/report.json")).unwrap();
    report["rows_sha256"] = json!(sha(&bytes));
    base::put(&f.path("test-export/report.json"), &report);
    assert!(
        f.run("ordered_cfg")
            .unwrap_err()
            .to_string()
            .contains("window/document count mismatch")
    );
    assert!(!f.path("out-ordered_cfg").exists());
}

#[test]
fn references_fitting_families_and_missing_roster_ids_are_rejected() {
    for bad in ["reference", "family", "roster"] {
        let f = Fixture::new(0, false);
        let mut rr = indexed(&fs::read(f.path("records.jsonl")).unwrap()).unwrap();
        let mut d = read(&f.path("test-dataset.json")).unwrap();
        match bad {
            "reference" => rr.get_mut("test-0").unwrap()["purpose"] = json!("reference"),
            "family" => {
                rr.get_mut("test-0").unwrap()["source_group"] = json!("train_a");
                d["split_assignments"]["train_a"] = json!("test");
            }
            _ => {
                rr.remove("test-0");
            }
        }
        let bytes = jsonl(rr.into_values()).unwrap();
        d["records_sha256"] = json!(sha(&bytes));
        d["test_records_sha256"] = json!(sha(&bytes));
        fs::write(f.path("records.jsonl"), bytes).unwrap();
        base::put(&f.path("test-dataset.json"), &d);
        let error = f.run("cfg_leaf").unwrap_err().to_string();
        assert!(
            error.contains(match bad {
                "reference" => "reference must be Train",
                "family" => "overlaps frozen",
                _ => "population differs",
            }),
            "{bad}: {error}"
        );
        assert!(!f.path("out-cfg_leaf").exists());
    }
}
