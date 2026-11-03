use super::*;
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    selection: Value,
    sb: Vec<u8>,
}
fn put(p: &Path, v: &Value) {
    fs::write(p, pretty(v).unwrap()).unwrap();
}
fn origin_probabilities(temperature: f64) -> Vec<f64> {
    // Unequal, exactly representable logits make a changed temperature visible.
    let exp = [-1.5_f64, 0.25, 2.0].map(|l| ((l - 2.) / temperature).exp());
    let total: f64 = exp.iter().sum();
    exp.iter().map(|e| e / total).collect()
}
impl Fixture {
    fn path(&self, p: &str) -> std::path::PathBuf {
        self.temp.path().join(p)
    }
    fn run(&self) -> Result<()> {
        prepare_validated(
            &self.selection,
            &self.sb,
            "candidate",
            &self.path("records.jsonl"),
            &self.path("dataset.json"),
            &self.path("cfg"),
            &self.path("emb"),
            &self.path("out"),
        )
    }
    fn new(warm: bool, mode: &str, cfg_missing: Option<&str>, v6_missing: Option<&str>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for dir in ["parent", "cfg", "emb", "emb/contract"] {
            fs::create_dir(root.join(dir)).unwrap();
        }
        let cfg = json!({"input_dimension":2,"human_profiles":["known_a","known_b"],"ai_profiles":["model_a","model_b"],
            "origin_order":["human_only","model_only","mixed"]});
        let cb = pretty(&cfg).unwrap();
        let ch = sha(&serde_json::to_vec(&cfg).unwrap());
        let rep = json!({"schema":"slop_ninja_v6_penultimate_contract_v1","artifact_id":ARTIFACT,
            "dimension":TEXT,"embedding_dtype":"float32","runtime":{"device":"cpu"},
            "hook":{"module":"model.classifier","argument":0,"normalization":"none","projection":"none"},
            "input":{"truncation":false,"max_tokens_including_special_tokens":2048},
            "model":{"temperature":inputs::V6_TEMPERATURE}});
        let mut repb = serde_json::to_vec(&rep).unwrap();
        let reph = sha(&repb);
        repb.push(b'\n');
        let coordinates = (0..TEXT)
            .map(|i| {
                json!(format!(
                    "v6_penultimate_{}/{i}",
                    if warm { "raw" } else { "standardized" }
                ))
            })
            .chain((0..2).map(|i| json!(format!("frozen_cfg/x/{i}"))))
            .collect::<Vec<_>>();
        let mut c = json!({"schema":"slop_ninja_shared_a_numeric_contract_v1","fusion_schema":"slop_ninja_v6_fusion_numeric_contract_v1",
            "ablation":mode,"input_dimension":TEXT+2,"input_coordinates":coordinates,"final_test_opened":false,
            "feature_policy":"slop_ninja_v6_fusion_manifest_cohort_v1","feature_policy_version":1,
            "cfg_parent_contract":cfg,"cfg_parent_contract_sha256":ch,"cfg_parent_contract_file_sha256":sha(&cb),
            "representation_contract":rep,"parents":{"cfg":{"manifest_sha256":"original-cfg"},"representation_contract_sha256":reph},
            "embedding_normalization":{"dimension":TEXT,"mean":vec![2.;TEXT],"scale":vec![4.;TEXT],
                "transform":"(embedding[i]-mean[i])/scale[i]","fit":"train_query_only_equal_source_family_weight",
                "reference_used_for_fit":false,"development_used_for_fit":false,"calibration_used_for_fit":false,"final_test_opened":false},
            "human_profiles":cfg["human_profiles"],"ai_profiles":cfg["ai_profiles"],"origin_order":cfg["origin_order"],"split_assignments":{}});
        if warm {
            c["warm_start_schema"] = json!("slop_ninja_raw_v6_cfg_residual_input_v1");
            c["feature_policy"] =
                json!("raw_v6_classifier_input_plus_unchanged_train_frozen_cfg_v1");
            c["feature_policy_version"] = json!(2);
        }
        let mut parent_rows = Vec::new();
        for part in ["train", "development", "calibration", "reference"] {
            let split = if part == "reference" { "train" } else { part };
            let group = format!("old-{part}");
            c["split_assignments"][&group] = json!(split);
            parent_rows.push(json!({"id":format!("old-{part}"),"source_group":group,"purpose":if part=="reference"{"reference"}else{"query"},
                "frozen_split":split,"input":{"contract_sha256":"placeholder","x":vec![0.;TEXT+2]}}));
        }
        let pch = sha(&serde_json::to_vec(&c).unwrap());
        let pcb = pretty(&c).unwrap();
        let mut splits = Vec::new();
        let mut pop = Vec::new();
        for (part, mut row) in ["train", "development", "calibration", "reference"]
            .into_iter()
            .zip(parent_rows)
        {
            row["input"]["contract_sha256"] = json!(pch);
            let b = jsonl([row.clone()]).unwrap();
            fs::write(root.join(format!("parent/{part}.jsonl")), &b).unwrap();
            splits.push(json!({"split":part,"rows":1,"families":1,"sha256":sha(&b)}));
            pop.push(json!({"id":row["id"],"group":row["source_group"],"split":row["frozen_split"],"purpose":row["purpose"],"available":true}));
        }
        let pb = pretty(&json!({"rows":pop,"requested":4,"available":4,"excluded":[]})).unwrap();
        let m = json!({"schema":"slop_ninja_shared_a_dataset_v1","final_test_opened":false,"requested":4,"available":4,"excluded":0,
            "splits":splits,"contract_file_sha256":sha(&pcb),"contract_sha256":pch,"population_file_sha256":sha(&pb)});
        let mb = pretty(&m).unwrap();
        for (name, b) in [
            ("manifest.json", mb.clone()),
            ("contract.json", pcb.clone()),
            ("population.json", pb),
        ] {
            fs::write(root.join("parent").join(name), b).unwrap();
        }
        let selection = json!({"schema":"slop_ninja_balanced_a_selection_freeze_v3",
            "binding_files":[{"path":root.join("parent/manifest.json"),"sha256":sha(&mb)},
                {"path":root.join("parent/contract.json"),"sha256":sha(&pcb)}],"candidates":[{
            "candidate":"candidate","numeric_path":root.join("parent"),"numeric_manifest_sha256":sha(&mb),
            "contract_file_sha256":sha(&pcb),"contract_sha256":pch}]});
        let sb = pretty(&selection).unwrap();
        let records = ["a","b"].map(|letter|json!({"id":format!("eval-{letter}"),"text":format!("Synthetic paragraph {letter}."),
            "text_sha256":sha(format!("Synthetic paragraph {letter}.").as_bytes()),"source_group":format!("new-family-{letter}"),
            "frozen_split":"test","purpose":"query","origin":"human_only","evidence":"synthetic fixture",
            "share_target":0.,"share_evidence":{"basis":"synthetic"},"human_role_present":true,
            "human_identity":if letter=="a"{"known_a"}else{"withheld"},"ai_role_present":false,"ai_identity":null,
            "exposed_to_grammar":false}));
        let rb = jsonl(records.clone()).unwrap();
        let d = json!({"schema":"slop_ninja_a_supervised_dataset_v1","complete":true,"test_text_opened":true,"test_used_for_selection":false,
            "selection_receipt_sha256":sha(&sb),"records_sha256":sha(&rb),"test_records_sha256":sha(&rb),
            "requested_records":3,"records":2,"non_test_ids":[],"test_ids":["eval-a","eval-b"],"sources":["synthetic fixture"],
            "split_assignments":{"new-family-a":"test","new-family-b":"test"},"excluded":[],
            "generation_reports":[{"complete":true,"requested":1,"accepted":0,"failed":1,
                "failures":[{"id":"not-generated","reason":"synthetic terminal failure"}]}]});
        let db = pretty(&d).unwrap();
        fs::write(root.join("records.jsonl"), rb).unwrap();
        fs::write(root.join("dataset.json"), &db).unwrap();
        let dataset = supervision::Dataset::parse_test(&db).unwrap();
        let profiles = supervision::Profiles::from_contract(&c).unwrap();
        let mut cr = Vec::new();
        let mut ap = Vec::new();
        let mut excluded = Vec::new();
        for r in &records {
            let id = r["id"].as_str().unwrap();
            let available = cfg_missing != Some(id);
            ap.push(json!({"id":id,"group":r["source_group"],"split":"test","purpose":"query","available":available,
                "exclusion_reason":if available {Value::Null}else{json!("document_execution_failure")},
                "metadata":supervision::without_text(r),"metadata_sha256":sha(&serde_json::to_vec(r).unwrap()),"source_sha256":r["text_sha256"]}));
            if available {
                let s = dataset.supervision(r).unwrap();
                cr.push(json!({"id":id,"source_group":r["source_group"],"frozen_split":"test","purpose":"query","exposed_to_grammar":false,
                    "roles":profiles.roles(&s),"targets":profiles.targets(&s),"input":{"contract_sha256":ch,"x":[0.125,-0.25]}}));
            } else {
                excluded.push(json!({"id":id}));
            }
        }
        let crb = jsonl(cr).unwrap();
        let audit = json!({"schema":"slop_ninja_supervised_a_test_transform_v1","dataset_manifest_sha256":sha(&db),"records_sha256":d["records_sha256"],
            "contract_sha256":ch,"frozen_manifest_sha256":"original-cfg","fit_performed":false,"final_test_opened":true,
            "requested":2,"available":2-excluded.len(),"excluded":excluded,"rows":ap});
        let ab = pretty(&audit).unwrap();
        let cm = json!({"schema":"slop_ninja_shared_a_dataset_v1","final_test_opened":true,"contract_file_sha256":sha(&cb),"contract_sha256":ch,
            "test_transform":{"fit_performed":false,"dataset_manifest_sha256":sha(&db),"frozen_manifest_sha256":"original-cfg","audit_file_sha256":sha(&ab),
                "requested":2,"available":2-excluded.len(),"excluded":excluded.len()},
            "splits":[{"split":"test","rows":2-excluded.len(),"sha256":sha(&crb)}]});
        for (name, b) in [
            ("manifest.json", pretty(&cm).unwrap()),
            ("contract.json", cb),
            ("test_population.json", ab),
            ("test.jsonl", crb),
        ] {
            fs::write(root.join("cfg").join(name), b).unwrap();
        }
        // Reverse request order demonstrates that joins use IDs, not row position.
        let reqb = jsonl(
            records
                .iter()
                .rev()
                .map(|r| json!({"id":r["id"],"text":r["text"]})),
        )
        .unwrap();
        let raw = (0..TEXT).map(|i| i as f64 / 8.).collect::<Vec<_>>();
        let raw_bytes = raw
            .iter()
            .flat_map(|x| (*x as f32).to_le_bytes())
            .collect::<Vec<_>>();
        let probabilities = origin_probabilities(inputs::V6_TEMPERATURE);
        let er = reqb.split_inclusive(|b|*b==b'\n').enumerate().map(|(index,line)| {
            let r:Value=serde_json::from_slice(line).unwrap(); let ok=v6_missing!=r["id"].as_str();
            json!({"id":r["id"],"text_sha256":sha(r["text"].as_str().unwrap().as_bytes()),"artifact_id":ARTIFACT,
                "representation_contract_sha256":reph,"request_index":index+1,"request_sha256":sha(line),
                "runtime":"cpu_float32_eager","labels":["human_only","model_only","mixed"],
                "status":if ok{"ok"}else{"unsupported_input"},"reason":if ok{Value::Null}else{json!("unsupported_length:2049>2048")},
                "embedding":if ok{json!(raw)}else{Value::Null},"embedding_sha256":if ok{json!(sha(&raw_bytes))}else{Value::Null},
                "token_count":if ok{7}else{2049},"logits":if ok{json!([-1.5,0.25,2.0])}else{Value::Null},
                "probabilities":if ok{json!(probabilities)}else{Value::Null},
                "model_or_assistance_probability":if ok{json!(probabilities[1]+probabilities[2])}else{Value::Null}})
        });
        let erb = jsonl(er).unwrap();
        let em = json!({"schema":"slop_ninja_v6_embeddings_run_v1","status":"complete","requested":2,"processed":2,
            "dimension":TEXT,
            "supported":if v6_missing.is_some(){1}else{2},"unsupported":usize::from(v6_missing.is_some()),
            "representation_contract":rep,"representation_contract_sha256":reph,"requests_sha256":sha(&reqb),"rows_sha256":sha(&erb)});
        for (name, b) in [
            ("manifest.json", pretty(&em).unwrap()),
            ("contract/manifest.json", repb),
            ("requests.jsonl", reqb),
            ("rows.jsonl", erb),
        ] {
            fs::write(root.join("emb").join(name), b).unwrap();
        }
        Self {
            temp,
            selection,
            sb,
        }
    }
    fn mutate_cfg_row(&self, mutate: impl FnOnce(&mut Value)) {
        let mut rr = rows(&fs::read(self.path("cfg/test.jsonl")).unwrap()).unwrap();
        mutate(rr.get_mut("eval-a").unwrap());
        let b = jsonl(rr.into_values()).unwrap();
        fs::write(self.path("cfg/test.jsonl"), &b).unwrap();
        let (_, mut m) = read(&self.path("cfg/manifest.json")).unwrap();
        m["splits"][0]["sha256"] = json!(sha(&b));
        put(&self.path("cfg/manifest.json"), &m);
    }
    fn mutate_embedding(&self, mutate: impl FnOnce(&mut Rows)) {
        let mut rr = rows(&fs::read(self.path("emb/rows.jsonl")).unwrap()).unwrap();
        mutate(&mut rr);
        let b = jsonl(rr.into_values()).unwrap();
        fs::write(self.path("emb/rows.jsonl"), &b).unwrap();
        let (_, mut m) = read(&self.path("emb/manifest.json")).unwrap();
        m["rows_sha256"] = json!(sha(&b));
        put(&self.path("emb/manifest.json"), &m);
    }
    fn mutate_record(&self, mutate: impl FnOnce(&mut Value)) {
        let mut rr = rows(&fs::read(self.path("records.jsonl")).unwrap()).unwrap();
        mutate(rr.get_mut("eval-a").unwrap());
        let r = &rr["eval-a"];
        let group = r["source_group"].as_str().unwrap().to_owned();
        let b = jsonl(rr.into_values()).unwrap();
        fs::write(self.path("records.jsonl"), &b).unwrap();
        let (_, mut d) = read(&self.path("dataset.json")).unwrap();
        d["records_sha256"] = json!(sha(&b));
        d["test_records_sha256"] = json!(sha(&b));
        d["split_assignments"][&group] = json!("test");
        put(&self.path("dataset.json"), &d);
    }
}

#[test]
fn warm_and_mlp_text_combined_match_frozen_math_and_preserve_parent_bytes() {
    for warm in [true, false] {
        for mode in ["text", "combined"] {
            let f = Fixture::new(warm, mode, None, None);
            f.run().unwrap();
            for file in [
                "contract.json",
                "train.jsonl",
                "development.jsonl",
                "calibration.jsonl",
                "reference.jsonl",
            ] {
                assert_eq!(
                    fs::read(f.path(&format!("parent/{file}"))).unwrap(),
                    fs::read(f.path(&format!("out/{file}"))).unwrap()
                );
            }
            let rr = rows(&fs::read(f.path("out/test.jsonl")).unwrap()).unwrap();
            for row in rr.values() {
                for i in 0..TEXT {
                    let raw = i as f64 / 8.;
                    assert_eq!(
                        row["input"]["x"][i],
                        if warm { raw } else { (raw - 2.) / 4. }
                    );
                }
                assert_eq!(
                    row["input"]["x"][TEXT],
                    if mode == "text" { 0. } else { 0.125 }
                );
                assert_eq!(
                    row["input"]["x"][TEXT + 1],
                    if mode == "text" { 0. } else { -0.25 }
                );
            }
            assert_eq!(rr["eval-b"]["roles"]["human"]["known"], false);
            assert!(rr["eval-b"]["targets"]["human"].is_null());
            let (_, m) = read(&f.path("out/manifest.json")).unwrap();
            assert_eq!(m["test_transform"]["requested"], 2);
            assert_eq!(m["test_transform"]["supported"], 2);
            assert_eq!(m["test_transform"]["upstream_requested_records"], 3);
            assert_eq!(m["test_transform"]["normalization_fitted"], false);
            let consumed =
                evaluation_binding::load_data(&f.path("out"), &f.selection, &sha(&f.sb)).unwrap();
            assert_eq!(consumed.numeric.len(), 2);
            assert_eq!(consumed.upstream["requested"], 3);
            assert_eq!(
                consumed.upstream["terminal_generation_failures"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert!(f.run().is_err());
        }
    }
}
#[test]
fn unsupported_and_cfg_failures_account_once_even_when_no_common_support() {
    for (cfg, v6, n) in [
        (None, Some("eval-a"), 1),
        (Some("eval-b"), None, 1),
        (Some("eval-b"), Some("eval-a"), 0),
        (Some("eval-a"), Some("eval-a"), 1),
    ] {
        let f = Fixture::new(true, "combined", cfg, v6);
        f.run().unwrap();
        let (_, m) = read(&f.path("out/manifest.json")).unwrap();
        assert_eq!(m["test_transform"]["supported"], n);
        assert_eq!(m["test_transform"]["excluded"], 2 - n);
        let (_, p) = read(&f.path("out/test_population.json")).unwrap();
        assert_eq!(p["rows"].as_array().unwrap().len(), 2);
        assert_eq!(p["excluded"].as_array().unwrap().len(), 2 - n);
        let consumed =
            evaluation_binding::load_data(&f.path("out"), &f.selection, &sha(&f.sb)).unwrap();
        assert_eq!(consumed.numeric.len(), n);
        assert_eq!(consumed.projection_excluded.len(), 2 - n);
    }
}

#[test]
fn cfg_failure_preserves_exact_text_rows_but_excludes_combined_rows() {
    for warm in [true, false] {
        let parsed = Fixture::new(warm, "text", None, None);
        let failed = Fixture::new(warm, "text", Some("eval-b"), None);
        parsed.run().unwrap();
        failed.run().unwrap();
        // Includes both the input bytes and every supervised target/role/mask.
        assert_eq!(
            fs::read(parsed.path("out/test.jsonl")).unwrap(),
            fs::read(failed.path("out/test.jsonl")).unwrap()
        );
        let data =
            evaluation_binding::load_data(&failed.path("out"), &failed.selection, &sha(&failed.sb))
                .unwrap();
        assert_eq!(data.numeric.len(), 2);
        assert!(data.projection_excluded.is_empty());
        let row = &data.numeric["eval-b"];
        assert_eq!(row["roles"]["human"]["present"], true);
        assert_eq!(row["roles"]["human"]["known"], false);
        assert!(row["targets"]["human"].is_null());
        assert!(row["targets"]["ai"].is_null());
        assert!(
            row["input"]["x"].as_array().unwrap()[TEXT..]
                .iter()
                .all(|v| *v == 0.)
        );
        let (_, manifest) = read(&failed.path("out/manifest.json")).unwrap();
        let t = &manifest["test_transform"];
        assert_eq!(t["supported"], 2);
        assert_eq!(t["excluded"], 0);
        assert_eq!(t["cfg_supported"], 1);
        assert_eq!(t["cfg_unavailable"], 1);
        assert_eq!(t["cfg_v6_common_supported"], 1);
        assert_eq!(t["cfg_required_for_model"], false);
        assert_eq!(manifest["evaluation_complete"], true);
        let (_, audit) = read(&failed.path("out/test_population.json")).unwrap();
        let failed_parse = audit["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "eval-b")
            .unwrap();
        assert_eq!(failed_parse["available"], true);
        assert!(failed_parse["exclusion_reason"].is_null());
        assert_eq!(failed_parse["evaluation_exclusions"], json!([]));
        assert_eq!(
            failed_parse["cfg_diagnostic"],
            json!({"available":false,
            "exclusion_reason":"document_execution_failure","required_for_model":false})
        );
        assert_eq!(
            fs::read(failed.path("cfg/test_population.json")).unwrap(),
            fs::read(failed.path("out/cfg-test-population.json")).unwrap()
        );

        let combined = Fixture::new(warm, "combined", Some("eval-b"), None);
        combined.run().unwrap();
        let data = evaluation_binding::load_data(
            &combined.path("out"),
            &combined.selection,
            &sha(&combined.sb),
        )
        .unwrap();
        assert_eq!(data.numeric.len(), 1);
        assert!(!data.numeric.contains_key("eval-b"));
        assert_eq!(data.projection_excluded.len(), 1);
        assert_eq!(data.projection_excluded[0]["id"], "eval-b");
        assert_eq!(data.projection_excluded[0]["reasons"][0]["stage"], "cfg");
        let (_, manifest) = read(&combined.path("out/manifest.json")).unwrap();
        assert_eq!(manifest["test_transform"]["cfg_required_for_model"], true);
        assert_eq!(manifest["test_transform"]["cfg_v6_common_supported"], 1);
        assert_eq!(manifest["evaluation_complete"], false);
    }
}

#[test]
fn text_support_requires_v6_even_when_cfg_has_no_common_support() {
    let f = Fixture::new(true, "text", Some("eval-b"), Some("eval-a"));
    f.run().unwrap();
    let data = evaluation_binding::load_data(&f.path("out"), &f.selection, &sha(&f.sb)).unwrap();
    assert_eq!(
        data.numeric.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["eval-b"]
    );
    assert_eq!(data.projection_excluded.len(), 1);
    assert_eq!(data.projection_excluded[0]["id"], "eval-a");
    assert_eq!(
        data.projection_excluded[0]["reasons"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(data.projection_excluded[0]["reasons"][0]["stage"], "v6");
    let (_, manifest) = read(&f.path("out/manifest.json")).unwrap();
    assert_eq!(manifest["test_transform"]["supported"], 1);
    assert_eq!(manifest["test_transform"]["cfg_v6_common_supported"], 0);
    assert_eq!(manifest["test_transform"]["cfg_unavailable"], 1);
}

#[test]
fn rejects_changed_selection_dataset_or_parent_before_output() {
    for bad in ["selection", "record", "parent"] {
        let f = Fixture::new(true, "text", None, None);
        match bad {
            "selection" => {
                let (_, mut d) = read(&f.path("dataset.json")).unwrap();
                d["selection_receipt_sha256"] = json!("0".repeat(64));
                put(&f.path("dataset.json"), &d);
            }
            "record" => {
                fs::write(f.path("records.jsonl"), b"changed").unwrap();
            }
            _ => {
                fs::write(f.path("parent/train.jsonl"), b"changed").unwrap();
            }
        }
        assert!(f.run().is_err());
        assert!(!f.path("out").exists());
    }
}
#[test]
fn rejects_fitting_family_reference_and_invalid_realized_span_before_join() {
    let f = Fixture::new(true, "text", None, None);
    f.mutate_record(|r| r["source_group"] = json!("old-train"));
    assert!(f.run().unwrap_err().to_string().contains("overlaps frozen"));
    assert!(!f.path("out").exists());
    let f = Fixture::new(true, "text", None, None);
    f.mutate_record(|r| r["purpose"] = json!("reference"));
    assert!(
        f.run()
            .unwrap_err()
            .to_string()
            .contains("reference must be Train")
    );
    assert!(!f.path("out").exists());
    let f = Fixture::new(true, "text", None, None);
    f.mutate_record(|r| {
        r["text"] = json!("New line."); r["text_sha256"] = json!(sha(b"New line."));
        r["origin"] = json!("mixed"); r["ai_role_present"] = json!(true); r["ai_identity"] = json!("model_a");
        r["share_target"] = json!(0.5);
        r["share_evidence"] = json!({"schema":"slop_ninja_realized_span_share_v1","word_policy":"unicode_split_whitespace_v1",
            "source_text":"Old line.","source_sha256":sha(b"Old line."),"replacement":"New","source_byte_span":[0,99],
            "final_sha256":sha(b"New line."),"final_byte_span":[0,3],"mode":"controlled_rewrite",
            "replacement_words":1,"final_words":2,"realized_share":0.5});
    });
    let error = f.run().unwrap_err().to_string();
    assert!(error.contains("span"), "{error}");
    assert!(!f.path("out").exists());
}
#[test]
fn incomplete_upstream_accounting_is_not_hidden_by_complete_feature_rows() {
    let f = Fixture::new(true, "text", None, None);
    let (_, mut d) = read(&f.path("dataset.json")).unwrap();
    d["generation_reports"][0]["failures"] = json!([]);
    put(&f.path("dataset.json"), &d);
    assert!(
        f.run()
            .unwrap_err()
            .to_string()
            .contains("generation report incomplete")
    );
    assert!(!f.path("out").exists());
}
#[test]
fn rehashed_cfg_cannot_change_targets_roles_scope_or_coordinates() {
    for bad in ["target", "role", "reference", "split", "dimension"] {
        let f = Fixture::new(true, "text", None, None);
        f.mutate_cfg_row(|r| match bad {
            "target" => r["targets"]["human"] = json!(1),
            "role" => r["roles"]["human"]["identity"] = json!("another"),
            "reference" => r["purpose"] = json!("reference"),
            "split" => r["frozen_split"] = json!("calibration"),
            _ => r["input"]["x"] = json!([0.]),
        });
        assert!(f.run().is_err());
        assert!(!f.path("out").exists());
    }
}
#[test]
fn rehashed_embeddings_cannot_substitute_text_model_requests_or_vectors() {
    for bad in [
        "text",
        "artifact",
        "request",
        "vector",
        "missing",
        "success_overlong",
        "failed_with_vector",
    ] {
        let f = Fixture::new(true, "text", None, None);
        f.mutate_embedding(|rr| {
            if bad == "missing" {
                rr.remove("eval-a");
                return;
            }
            let r = rr.get_mut("eval-a").unwrap();
            match bad {
                "text" => r["text_sha256"] = json!("0".repeat(64)),
                "artifact" => r["artifact_id"] = json!("changed"),
                "request" => r["request_index"] = json!(9),
                "vector" => r["embedding"][0] = json!(2.),
                "success_overlong" => r["token_count"] = json!(2049),
                _ => {
                    r["status"] = json!("unsupported_input");
                    r["reason"] = json!("failed");
                }
            }
        });
        assert!(f.run().is_err());
        assert!(!f.path("out").exists());
    }
}
#[test]
fn changed_embedding_snapshot_is_not_accepted_via_self_rehashing() {
    let f = Fixture::new(true, "text", None, None);
    let (_, mut m) = read(&f.path("emb/manifest.json")).unwrap();
    m["representation_contract"]["runtime"]["device"] = json!("cuda");
    let mut b = serde_json::to_vec(&m["representation_contract"]).unwrap();
    m["representation_contract_sha256"] = json!(sha(&b));
    b.push(b'\n');
    fs::write(f.path("emb/contract/manifest.json"), b).unwrap();
    put(&f.path("emb/manifest.json"), &m);
    assert!(f.run().is_err());
    assert!(!f.path("out").exists());
}

#[test]
fn rehashed_rows_must_keep_cpu_runtime_label_order_and_frozen_origin_head() {
    for (bad, expected) in [
        ("runtime", "runtime/labels"),
        ("missing_runtime", "runtime/labels"),
        ("labels", "runtime/labels"),
        ("missing_labels", "runtime/labels"),
        ("truncated", "runtime/labels"),
        ("temperature", "frozen calibration"),
        ("logits", "frozen calibration"),
        ("involvement", "involvement mismatch"),
        ("missing_involvement", "number"),
        ("probability_mass", "probability vector"),
    ] {
        let f = Fixture::new(true, "text", None, None);
        f.mutate_embedding(|rr| {
            let r = rr.get_mut("eval-a").unwrap();
            match bad {
                "runtime" => r["runtime"] = json!("cuda"),
                "missing_runtime" => {
                    r.as_object_mut().unwrap().remove("runtime");
                }
                "labels" => r["labels"] = json!(["model_only", "human_only", "mixed"]),
                "missing_labels" => {
                    r.as_object_mut().unwrap().remove("labels");
                }
                "truncated" => r["truncated"] = json!(true),
                "temperature" => {
                    let p = origin_probabilities(1.);
                    r["model_or_assistance_probability"] = json!(p[1] + p[2]);
                    r["probabilities"] = json!(p);
                }
                "logits" => r["logits"][0] = json!(0.),
                "involvement" => r["model_or_assistance_probability"] = json!(0.5),
                "missing_involvement" => {
                    r.as_object_mut()
                        .unwrap()
                        .remove("model_or_assistance_probability");
                }
                _ => {
                    // This used to pass the producer's looser 1e-6 mass check.
                    r["probabilities"][0] = json!(r["probabilities"][0].as_f64().unwrap() + 5e-7);
                }
            }
        });
        let error = f.run().unwrap_err().to_string();
        assert!(error.contains(expected), "{bad}: {error}");
        assert!(!f.path("out").exists());
    }
}

#[test]
fn unsupported_rows_require_exact_reason_tokens_and_absent_outputs() {
    for (bad, expected) in [
        ("reason", "reason/token mismatch"),
        ("length", "reason/token mismatch"),
        ("supported_length", "reason/token mismatch"),
        ("empty_nonzero", "reason/token mismatch"),
        ("zero_long", "reason/token mismatch"),
        ("involvement", "inferred values"),
        ("runtime", "runtime/labels"),
        ("labels", "runtime/labels"),
    ] {
        let f = Fixture::new(true, "text", None, Some("eval-a"));
        f.mutate_embedding(|rr| {
            let r = rr.get_mut("eval-a").unwrap();
            match bad {
                "reason" => r["reason"] = json!("unsupported"),
                "length" => r["token_count"] = json!(2050),
                "supported_length" => r["token_count"] = json!(2048),
                "empty_nonzero" => r["reason"] = json!("empty_text"),
                "zero_long" => r["token_count"] = json!(0),
                "involvement" => r["model_or_assistance_probability"] = json!(0.),
                "runtime" => r["runtime"] = json!("cuda"),
                _ => r["labels"] = json!(["mixed", "model_only", "human_only"]),
            }
        });
        let error = f.run().unwrap_err().to_string();
        assert!(error.contains(expected), "{bad}: {error}");
        assert!(!f.path("out").exists());
    }
}

#[test]
fn runtime_failures_preserve_both_comparator_count_conventions() {
    for status in ["error", "failed"] {
        for separate in [false, true] {
            let f = Fixture::new(true, "combined", None, Some("eval-a"));
            f.mutate_embedding(|rr| {
                let r = rr.get_mut("eval-a").unwrap();
                r["status"] = json!(status);
                r["reason"] = json!("synthetic execution failure");
                r["token_count"] = json!(7);
            });
            if separate {
                let (_, mut m) = read(&f.path("emb/manifest.json")).unwrap();
                m["unsupported"] = json!(0);
                m["failed"] = json!(1);
                put(&f.path("emb/manifest.json"), &m);
            }
            f.run().unwrap();
            let data =
                evaluation_binding::load_data(&f.path("out"), &f.selection, &sha(&f.sb)).unwrap();
            assert_eq!(data.rows.len(), 2);
            assert_eq!(data.numeric.len(), 1);
            assert_eq!(data.projection_excluded[0]["reasons"][0]["status"], status);
        }
    }
    let f = Fixture::new(true, "text", None, Some("eval-a"));
    let (_, mut m) = read(&f.path("emb/manifest.json")).unwrap();
    m["failed"] = json!(1); // Would count the same non-success row twice.
    put(&f.path("emb/manifest.json"), &m);
    assert!(
        f.run()
            .unwrap_err()
            .to_string()
            .contains("support counts differ")
    );
    assert!(!f.path("out").exists());
}

#[test]
fn synthetic_comparator_handoff_preserves_full_v6_evidence_without_cfg_support() {
    let f = Fixture::new(true, "combined", Some("eval-b"), Some("eval-a"));
    f.mutate_embedding(|rr| rr.get_mut("eval-b").unwrap()["token_count"] = json!(2048));
    f.run().unwrap();
    let data = evaluation_binding::load_data(&f.path("out"), &f.selection, &sha(&f.sb)).unwrap();
    assert_eq!(data.rows.len(), 2);
    assert!(data.numeric.is_empty());
    assert_eq!(data.projection_excluded.len(), 2);
    let (_, manifest) = read(&f.path("out/manifest.json")).unwrap();
    let (_, embeddings) = read(&f.path("emb/manifest.json")).unwrap();
    let binding = &manifest["test_transform"]["bindings"]["embeddings"];
    assert_eq!(
        binding["manifest_sha256"],
        sha(&fs::read(f.path("emb/manifest.json")).unwrap())
    );
    assert_eq!(binding["rows_sha256"], embeddings["rows_sha256"]);
    assert_eq!(
        binding["representation_contract_sha256"],
        embeddings["representation_contract_sha256"]
    );
    assert_eq!(
        fs::read(f.path("out/embedding-manifest.json")).unwrap(),
        fs::read(f.path("emb/manifest.json")).unwrap()
    );
    let retained =
        rows(&fs::read(f.path("out/test-baseline-origin-reference.jsonl")).unwrap()).unwrap();
    let original = rows(&fs::read(f.path("emb/rows.jsonl")).unwrap()).unwrap();
    assert!(retained.keys().eq(data.rows.keys()));
    for (id, r) in &retained {
        for field in [
            "status",
            "reason",
            "artifact_id",
            "logits",
            "probabilities",
            "token_count",
            "text_sha256",
        ] {
            assert_eq!(r[field], original[id][field], "{id}: {field}");
        }
        assert_eq!(r["text_sha256"], data.hashes[id]);
    }
    assert_eq!(retained["eval-b"]["token_count"], 2048);
    assert_eq!(retained["eval-b"]["status"], "ok");
    assert!(retained["eval-a"]["probabilities"].is_null());
}
