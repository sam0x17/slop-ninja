//! Synthetic fixtures only; production receipt validation remains at the CLI boundary.
use super::super::super::tests as base;
use super::*;

struct Fixture {
    temp: tempfile::TempDir,
    selection: Value,
    sb: Vec<u8>,
    dataset: Value,
    rb: Vec<u8>,
}
impl Fixture {
    fn path(&self, name: &str) -> PathBuf {
        self.temp.path().join(name)
    }
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let mut plan = String::from(
            "pg_id\tauthor_key\tunderlying_work_key\tsplit\trole\tenrollment\tauthor_partition\n",
        );
        for i in 0..16 {
            plan.push_str(&format!(
                "{}\tquery-writer-{i}\tquery-work-{i}\ttest\tquery\tknown\tknown\n",
                1000 + i
            ));
        }
        let mut records = Vec::new();
        let mut assignments = serde_json::Map::new();
        let source_hash = sha(b"synthetic acquisition manifest");
        for i in 1..=2 {
            let pg = 9_900_000 + i;
            let group = format!("reference-work-{i}");
            let human = format!("withheld-writer-{i}");
            plan.push_str(&format!(
                "{pg}\t{human}\t{group}\ttest_reference\treference\twithheld\ttest\n"
            ));
            assignments.insert(group.clone(), json!("test"));
            for fraction in [60, 20] {
                // Deliberately unsorted; requests must sort independently.
                let text = format!("Synthetic reference {pg} excerpt{fraction}.");
                let rights = json!({"commercial_training":true,"external_evaluation":true});
                records.push(json!({"id":format!("author-pg{pg}-p{fraction}"),"source_group":group,
                    "human_identity":human,"human_role_present":true,"ai_identity":null,"ai_role_present":false,
                    "origin":"human_only","share_target":0.,"share_evidence":{"basis":"synthetic historical proxy"},
                    "evidence":"synthetic fixture","exposed_to_grammar":true,"purpose":"reference","frozen_split":"test",
                    "text":text,"text_sha256":sha(text.as_bytes()),"rights":rights,
                    "source":{"source_manifest_sha256":source_hash,"record":{"pg_id":pg,"author_key":human,
                    "source_group":group,"preassigned_partition":"test_reference","purpose":"reference",
                    "enrollment":"withheld","author_partition":"test","rights":rights}}}));
            }
        }
        fs::write(temp.path().join("plan.tsv"), &plan).unwrap();
        let selection = json!({"schema":"slop_ninja_balanced_a_selection_freeze_v3",
            "source_plan":{"path":temp.path().join("plan.tsv"),"sha256":sha(plan.as_bytes())}});
        let sb = pretty(&selection).unwrap();
        let rb = jsonl(records.clone()).unwrap();
        let dataset = json!({"schema":"slop_ninja_a_supervised_dataset_v1","complete":true,
            "test_text_opened":true,"test_used_for_selection":false,"selection_receipt_sha256":sha(&sb),
            "requested_reference_records":4,"separate_reference_records":4,"reference_exclusions":[],
            "references_sha256":sha(&rb),"reference_ids":records.iter().map(|r|&r["id"]).collect::<Vec<_>>(),
            "test_ids":["query-id"],"source_manifest_sha256":source_hash,"split_assignments":assignments});
        fs::write(temp.path().join("dataset.json"), pretty(&dataset).unwrap()).unwrap();
        fs::write(temp.path().join("references.jsonl"), &rb).unwrap();
        Self {
            temp,
            selection,
            sb,
            dataset,
            rb,
        }
    }
    fn select(&self) -> Result<()> {
        admission::select(
            &self.selection,
            &self.sb,
            &self.path("dataset.json"),
            &self.path("references.jsonl"),
            &self.path("selected"),
        )
    }
}

#[test]
fn selection_preserves_exact_records_and_exports_only_sorted_label_free_requests() {
    let f = Fixture::new();
    f.select().unwrap();
    assert_eq!(fs::read(f.path("selected/records.jsonl")).unwrap(), f.rb);
    let requests = fs::read(f.path("selected/requests.jsonl")).unwrap();
    let requested = records(&requests).unwrap();
    let original = rows(&f.rb).unwrap();
    assert_eq!(requested.len(), 4);
    assert!(
        requested
            .windows(2)
            .all(|r| r[0]["id"].as_str() < r[1]["id"].as_str())
    );
    for row in requested {
        assert_eq!(row.as_object().unwrap().len(), 2);
        assert_eq!(row["text"], original[row["id"].as_str().unwrap()]["text"]);
    }
    let (_, m) = read(&f.path("selected/request-manifest.json")).unwrap();
    assert_eq!(m["requests_sha256"], sha(&requests));
    assert_eq!(m["reference_records_sha256"], sha(&f.rb));
    assert_eq!(
        m["query_dataset_manifest_sha256"],
        sha(&fs::read(f.path("dataset.json")).unwrap())
    );
    assert!(f.select().is_err()); // Never overwrite/resume by replacing evidence.
}

#[test]
fn admission_rejects_changed_text_rights_identity_scope_or_reference_roster() {
    let f = Fixture::new();
    admission::validate(&f.selection, &f.sb, &f.dataset, &f.rb).unwrap();
    let changes = [
        ("/text", json!("Changed synthetic text.")),
        ("/frozen_split", json!("development")),
        ("/purpose", json!("query")),
        ("/human_identity", json!("known-writer")),
        ("/source/record/enrollment", json!("known")),
        ("/rights/external_evaluation", json!(false)),
        (
            "/source/source_manifest_sha256",
            json!(sha(b"substitution")),
        ),
        ("/source_group", json!("query-work-0")),
        ("/ai_role_present", json!(true)),
    ];
    for (pointer, value) in changes {
        let mut rr = records(&f.rb).unwrap();
        *rr[0].pointer_mut(pointer).unwrap() = value;
        let bytes = jsonl(rr).unwrap();
        let mut d = f.dataset.clone();
        d["references_sha256"] = json!(sha(&bytes));
        assert!(
            admission::validate(&f.selection, &f.sb, &d, &bytes).is_err(),
            "{pointer}"
        );
    }
    let mut d = f.dataset.clone();
    d["reference_ids"][0] = d["reference_ids"][1].clone();
    assert!(admission::validate(&f.selection, &f.sb, &d, &f.rb).is_err());
    fs::write(f.path("plan.tsv"), b"different metadata").unwrap();
    assert!(admission::validate(&f.selection, &f.sb, &f.dataset, &f.rb).is_err());
}

#[test]
fn reference_mode_is_explicit_and_cannot_enter_old_query_or_reference_routes() {
    let f = Fixture::new();
    f.select().unwrap();
    let b = fs::read(f.path("selected/manifest.json")).unwrap();
    let d = Dataset::parse_final_references(&b).unwrap();
    assert!(Dataset::parse(&b).is_err());
    assert!(Dataset::parse_test(&b).is_err());
    assert!(Dataset::parse_references(&b).is_err());
    let profiles = Profiles::from_contract(
        &json!({"human_profiles":["known-a","known-b"],"ai_profiles":["model-a","model-b"]}),
    )
    .unwrap();
    for r in records(&f.rb).unwrap() {
        d.validate_metadata(r["id"].as_str().unwrap(), &r).unwrap();
        let s = d.supervision(&r).unwrap();
        assert_eq!(profiles.roles(&s)["human"]["known"], false);
        assert!(
            profiles
                .targets(&s)
                .as_object()
                .unwrap()
                .values()
                .all(Value::is_null)
        );
        assert!(Supervision::parse(&r).is_err());
        let mut bad = r.clone();
        bad["frozen_split"] = json!("calibration");
        assert!(d.supervision(&bad).is_err());
        bad = r.clone();
        bad["purpose"] = json!("query");
        assert!(d.supervision(&bad).is_err());
    }
    let mut m: Value = serde_json::from_slice(&b).unwrap();
    m.as_object_mut()
        .unwrap()
        .remove("final_test_reference_transform_only");
    assert!(Dataset::parse_final_references(&pretty(&m).unwrap()).is_err());
}

#[test]
fn frozen_cfg_reference_transform_preserves_parent_and_null_targets_even_when_all_fail() {
    for all_fail in [false, true] {
        let f = Fixture::new();
        f.select().unwrap();
        base::fixture(f.temp.path(), 2.);
        super::super::super::prepare(
            &f.path("export"),
            &f.path("dataset.json"),
            &f.path("numeric"),
            Ablation::LeafGrammar,
        )
        .unwrap();
        let original = fs::read(f.path("numeric/contract.json")).unwrap();
        let old_manifest = fs::read(f.path("numeric/manifest.json")).unwrap();
        let original_export = base::read_rows(&f.path("export/rows.jsonl"));
        fs::create_dir(f.path("ref-export")).unwrap();
        let mut rr = Vec::new();
        let mut entries = Vec::new();
        for (i, r) in records(&f.rb).unwrap().into_iter().enumerate() {
            let prefix = format!("{i:05}");
            let mut row = original_export[0].clone();
            row["id"] = r["id"].clone();
            row["metadata"] = r.clone();
            row["receipt"]["source_sha256"] = r["text_sha256"].clone();
            let len = r["text"].as_str().unwrap().len();
            row["receipt"]["summary"]["input_bytes"] = json!(len);
            row["receipt"]["summary"]["accounted_bytes"] = json!(len);
            if all_fail {
                row["receipt"]["summary"]["execution_failures"] = json!(1);
                row["receipt"]["features"]["projection_available"] = json!(false);
                row["receipt"]["features"]["counts"]["availability/window/NativeComplete"] =
                    json!(0.);
                row["receipt"]["features"]["counts"]["availability/window/ExecutionFailure"] =
                    json!(1.);
            }
            let path = f.path(&format!("ref-export/{prefix}.receipt.json"));
            base::put(&path, &row["receipt"]);
            row["receipt_sha256"] = json!(sha(&fs::read(path).unwrap()));
            entries.push(json!({"id":r["id"],"prefix":prefix,"summary":row["receipt"]["summary"]}));
            rr.push(row);
        }
        let bytes = jsonl(rr).unwrap();
        fs::write(f.path("ref-export/rows.jsonl"), &bytes).unwrap();
        let mut c = base::read(&f.path("export/contract.json"));
        c["input_sha256"] = json!(sha(&f.rb));
        base::put(&f.path("ref-export/contract.json"), &c);
        base::put(
            &f.path("ref-export/report.json"),
            &json!({"schema":"slop_ninja_document_export_report_v1",
            "complete":true,"engine_identity":"synthetic_engine","rows_sha256":sha(&bytes),"documents":entries}),
        );
        super::super::transform_scope(
            &f.path("numeric"),
            &f.path("ref-export"),
            &f.path("selected/manifest.json"),
            &f.path("projection"),
            true,
            false,
            true,
        )
        .unwrap();
        let output =
            records(&fs::read(f.path("projection/partition_references.jsonl")).unwrap()).unwrap();
        assert_eq!(output.len(), if all_fail { 0 } else { 4 });
        for r in output {
            assert_eq!(r["purpose"], "reference");
            assert_eq!(r["frozen_split"], "test");
            assert!(
                r["targets"]
                    .as_object()
                    .unwrap()
                    .values()
                    .all(Value::is_null)
            );
        }
        let (_, audit) = read(&f.path("projection/reference_population.json")).unwrap();
        assert_eq!(audit["requested"], 4);
        assert_eq!(audit["final_test_opened"], true);
        assert_eq!(
            audit["excluded"].as_array().unwrap().len(),
            if all_fail { 4 } else { 0 }
        );
        assert_eq!(
            fs::read(f.path("projection/contract.json")).unwrap(),
            original
        );
        assert_eq!(
            fs::read(f.path("numeric/manifest.json")).unwrap(),
            old_manifest
        );
    }
}

#[test]
fn text_support_ignores_cfg_failure_but_combined_and_cfg_do_not() {
    let cp = json!({"exclusion_reason":"document_execution_failure"});
    let ok = json!({"status":"ok"});
    let failed = json!({"status":"unsupported_input","reason":"unsupported_length:2049>2048","token_count":2049});
    assert!(project::exclusion_reasons(false, false, &cp, Some(&ok)).is_empty());
    assert_eq!(
        project::exclusion_reasons(true, false, &cp, Some(&ok)).len(),
        1
    );
    assert_eq!(project::exclusion_reasons(true, false, &cp, None).len(), 1);
    assert_eq!(
        project::exclusion_reasons(true, false, &cp, Some(&failed)).len(),
        2
    );
    assert_eq!(
        project::exclusion_reasons(false, true, &cp, Some(&failed)).len(),
        1
    );
}

fn numeric_reference(i: usize, part: &str) -> Value {
    let known = part == "train";
    json!({"id":format!("{part}-ref-{i}"),"source_group":format!("{part}-work-{}",i/2),"frozen_split":part,"purpose":"reference",
        "roles":{"human":{"identity":format!("{part}-writer-{}",i/2),"present":true,"known":known},"ai":{"identity":null,"present":false,"known":false}},
        "targets":{"origin":null,"ai_involvement":null,"share":null,"human":null,"ai":null},
        "input":{"contract_sha256":"a".repeat(64),"x":[4.,-3.,0.5]}})
}
#[test]
fn gallery_rejects_partition_query_family_and_label_leakage() {
    let c =
        json!({"human_profiles":(0..6).map(|i|format!("train-writer-{i}")).collect::<Vec<_>>()});
    let known = (0..12)
        .map(|i| numeric_reference(i, "train"))
        .collect::<Vec<_>>();
    let withheld = (0..4)
        .map(|i| numeric_reference(i, "test"))
        .collect::<Vec<_>>();
    let empty = BTreeSet::new();
    encode::numeric_refs(&known, &c, "train", &empty, &empty).unwrap();
    encode::numeric_refs(&withheld, &c, "test", &empty, &empty).unwrap();
    assert!(encode::numeric_refs(&known[..11], &c, "train", &empty, &empty).is_err());
    assert!(encode::numeric_refs(&withheld, &c, "calibration", &empty, &empty).is_err());
    assert!(
        encode::numeric_refs(
            &withheld,
            &c,
            "test",
            &BTreeSet::from(["test-work-0".into()]),
            &empty
        )
        .is_err()
    );
    assert!(
        encode::numeric_refs(
            &withheld,
            &c,
            "test",
            &empty,
            &BTreeSet::from(["test-ref-0".into()])
        )
        .is_err()
    );
    let mut bad = withheld.clone();
    bad[0]["targets"]["origin"] = json!(0);
    assert!(encode::numeric_refs(&bad, &c, "test", &empty, &empty).is_err());
    bad = withheld.clone();
    bad[0]["roles"]["human"]["identity"] = json!("train-writer-0");
    assert!(encode::numeric_refs(&bad, &c, "test", &empty, &empty).is_err());
}

fn weights(warm: bool) -> Value {
    let mut weights = serde_json::Map::new();
    for (name, n, m) in [
        (if warm { "residual.0" } else { "encoder.0" }, 2, 3),
        (if warm { "residual.2" } else { "encoder.2" }, 2, 2),
        ("origin", 3, 2),
        ("mixed_share", 1, 2),
        ("human", 2, 2),
        ("ai", 2, 2),
    ] {
        let values = if name.ends_with(".0") {
            vec![1., 0., 2., 0., 0., 0.]
        } else if name.ends_with(".2") {
            vec![2., 0., -1., 0.]
        } else {
            vec![0.; n * m]
        };
        weights.insert(
            format!("{name}.weight"),
            json!({"shape":[n,m],"values":values}),
        );
        weights.insert(
            format!("{name}.bias"),
            json!({"shape":[n],"values":vec![0.;n]}),
        );
    }
    let mut w = json!({"schema":if warm{"slop_ninja_v6_residual_a_weights_v1"}else{"slop_ninja_shared_a_weights_v1"},
        "architecture":{"dimension":3,"hidden":2,"embedding":2,"human_profiles":2,"ai_profiles":2},
        "contract_sha256":"a".repeat(64),"temperature":1.,"profiles":{"human":["a","b"],"ai":["c","d"]},
        "thresholds":{"human":0.6,"ai":0.6},"supervision":{"origin":3,"share":3,"mixed_fractional_share":1,"human":2,"ai":2},
        "origin_order":["human_only","model_only","mixed"],"control":"joint","weights":weights,
        "operations":["linear","tanh","softmax","sigmoid"]});
    if warm {
        w["architecture"]["raw_text_dimension"] = json!(2);
        w["base_temperature"] = json!(2.);
        w["input_mode"] = json!("combined");
        w["initialization_sha256"] = json!("b".repeat(64));
        w["text_mean"] = json!([2., 1.]);
        w["text_scale"] = json!([2., 4.]);
        w["operations"] = json!([
            "linear", "tanh", "add", "subtract", "divide", "softmax", "sigmoid"
        ]);
    }
    w
}

#[test]
fn shared_dispatcher_encodes_mlp_and_warm_without_changing_reference_rows() {
    for warm in [false, true] {
        let m = NumericSharedModel::from_json(&pretty(&weights(warm)).unwrap()).unwrap();
        let row = numeric_reference(0, "test");
        let mut output = encode::encode_rows(&m, [row.clone()]).unwrap().remove(0);
        let actual = output.as_object_mut().unwrap().remove("encoding").unwrap();
        assert_eq!(output, row);
        let expected = if warm {
            let t = 2f64.tanh();
            vec![4. + 2. * t, -3. - t]
        } else {
            let t = 5f64.tanh();
            vec![(2. * t).tanh(), (-t).tanh()]
        };
        for (a, e) in actual.as_array().unwrap().iter().zip(expected) {
            assert!((a.as_f64().unwrap() - e).abs() < 1e-14);
        }
        let mut bad = row;
        bad["input"]["contract_sha256"] = json!("c".repeat(64));
        assert!(encode::encode_rows(&m, [bad]).is_err());
    }
}

#[test]
fn gallery_requires_fixed_unambiguous_seed_and_protects_frozen_paths() {
    let c = json!({"runs":[{"seed":17},{"seed":29},{"seed":41}]});
    assert!(run_for(&c, 17).is_ok());
    assert!(run_for(&c, 42).is_err());
    let c = json!({"runs":[{"seed":17},{"seed":17}]});
    assert!(run_for(&c, 17).is_err());
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("numeric")).unwrap();
    fs::create_dir(tmp.path().join("fit")).unwrap();
    let s = json!({"candidates":[{"numeric_path":tmp.path().join("numeric"),"runs":[{"model_path":tmp.path().join("fit")}]}]});
    assert!(new_output(&s, &tmp.path().join("numeric/new")).is_err());
    assert!(new_output(&s, &tmp.path().join("fit/new")).is_err());
    assert!(new_output(&s, &tmp.path().join("new")).is_ok());
}
