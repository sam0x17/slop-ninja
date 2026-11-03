use super::*;

fn put(p: &Path, v: &Value) {
    fs::write(p, serde_json::to_vec(v).unwrap()).unwrap();
}
fn pin(p: &Path) -> Value {
    json!({"path":p,"sha256":sha(&fs::read(p).unwrap())})
}
fn text() -> String {
    (0..30)
        .map(|i| format!("The synthetic visitor numbered {i} carried eight blue stones."))
        .collect::<Vec<_>>()
        .join(" ")
}
fn parent() -> Value {
    let t = text();
    json!({"schema":"slop_ninja_a_supervised_record_v1","id":"author-pg1-p20","text":t,"text_sha256":sha(t.as_bytes()),
        "frozen_split":"test","source_group":"synthetic-family","purpose":"query","origin":"human_only","human_role_present":true,
        "human_identity":"synthetic-writer","ai_role_present":false,"ai_identity":null,"parent_id":null,"share_target":0.,"share_evidence":{},
        "evidence":"historical_public_domain_author_proxy","documented_human_only":false,"source":{},"rights":{"commercial_training":true},
        "exposed_to_grammar":true,"grammar_exposure":"unknown"})
}
fn config(root: &Path, slot: usize) -> Value {
    let runtime = root.join("runtime");
    let library = root.join("library");
    fs::write(&runtime, b"synthetic runtime").unwrap();
    fs::write(&library, b"synthetic library").unwrap();
    json!({"schema":"slop_ninja_local_generator_v1","identity":MODELS[slot],"sha256":MODELS[slot].rsplit(':').next().unwrap(),
        "allowed_splits":["test"],"rights":{"generation_permitted":true,"model_license":"synthetic notice"},
        "repo":"synthetic","revision":"synthetic-revision","filename":"synthetic.gguf","model_path":"/synthetic/model.gguf",
        "runtime_path":runtime,"runtime_sha256":sha(b"synthetic runtime"),"runtime_libraries":[pin(&library)],"alias":format!("model-{slot}"),"endpoint":"http://127.0.0.1:18086"})
}
fn task(parent: &Value, c: &Value, binding: &Value, percent: u64) -> (Value, Value) {
    let id = balanced::task_id(
        str_at(parent, "id").unwrap(),
        str_at(c, "identity").unwrap(),
        percent,
    );
    let pos = Sha256::digest(format!("{}:{percent}", str_at(parent, "id").unwrap()).as_bytes())[0]
        as usize
        % 3;
    let t = str_at(parent, "text").unwrap();
    let (s, e) = if percent == 0 {
        (0, 0)
    } else {
        recipe::span(t, percent as usize, pos)
    };
    let prompt = if percent == 0 {
        recipe::scratch_prompt(str_at(parent, "id").unwrap())
    } else {
        format!(
            "Rewrite the following English narrative passage in your own words, keeping its events, perspective, and approximate length. Return only the replacement prose in one paragraph: no title, preface, analysis, quotation delimiters, or explanation.\n\n{}",
            &t[s..e]
        )
    };
    let request = json!({"model":c["alias"],"messages":[{"role":"user","content":prompt}],"temperature":0.7,"top_p":0.9,
        "seed":1729,"max_tokens":1024,"stream":false,"chat_template_kwargs":{"enable_thinking":false},"reasoning_format":"none"});
    let mode = if percent == 0 {
        "from_scratch"
    } else {
        "controlled_rewrite"
    };
    let plan = json!({"id":id,"parent_id":parent["id"],"source_sha256":parent["text_sha256"],"source_record_sha256":compact_sha(parent).unwrap(),
        "binding_sha256":compact_sha(binding).unwrap(),"mode":mode,"requested_source_span_percent":percent,
        "position":(["start","middle","end"][pos]),"source_byte_span":[s,e],"sham_reassembly_exact":true,"request":request});
    let replacement =
        "A different synthetic traveller carried seven bright stones toward the quiet harbor.";
    let final_text = if percent == 0 {
        replacement.to_owned()
    } else {
        format!("{}{}{}", &t[..s], replacement, &t[e..])
    };
    let words = replacement.split_whitespace().count();
    let total = final_text.split_whitespace().count();
    let share = words as f64 / total as f64;
    let mut r = parent.clone();
    r["id"] = json!(id);
    r["parent_id"] = parent["id"].clone();
    r["text"] = json!(final_text);
    r["text_sha256"] = json!(sha(final_text.as_bytes()));
    r["origin"] = json!(if percent == 0 { "model_only" } else { "mixed" });
    r["human_role_present"] = json!(percent != 0);
    if percent == 0 {
        r["human_identity"] = Value::Null;
    }
    r["ai_role_present"] = json!(true);
    r["ai_identity"] = c["identity"].clone();
    r["share_target"] = json!(share);
    r["evidence"] = json!(if percent == 0 {
        "recorded_local_generation_from_scratch"
    } else {
        "recorded_local_rewrite_of_historical_human_proxy"
    });
    r["generation"] = json!({"generator":c["identity"],"binding":binding,"request":request,"position":plan["position"],"mode":mode,
        "attempt":0,"response_format_policy":"strip_verified_empty_leading_think_block_then_trim_v1"});
    r["share_evidence"] = json!({"schema":"slop_ninja_realized_span_share_v1","source_text":t,"source_sha256":parent["text_sha256"],
        "source_byte_span":[s,e],"replacement":replacement,"final_byte_span":[s,s+replacement.len()],"final_sha256":r["text_sha256"],
        "replacement_words":words,"final_words":total,"word_policy":"unicode_split_whitespace_v1","realized_share":share,
        "requested_percent_not_target":percent,"full_rewrite_is_mixed_history":percent==100});
    (plan, r)
}
#[test]
fn exact_test_roster_and_generator_scope() {
    let w = sources::works().unwrap();
    assert_eq!(w.len(), 18);
    assert_eq!(
        w.values().filter(|w| w["purpose"] == "reference").count(),
        2
    );
    let temp = tempfile::tempdir().unwrap();
    for slot in 0..3 {
        let c = config(temp.path(), slot);
        generation::check_slot(slot, &c, "test").unwrap();
        assert!(generation::check_slot(slot, &c, "calibration").is_err());
    }
    let mut c = config(temp.path(), 2);
    c["identity"] = json!(balanced::MODELS[2]);
    assert!(generation::check_slot(2, &c, "test").is_err());
}
#[test]
fn exact_recipe_and_full_rewrite_preserve_mixed_history() {
    let temp = tempfile::tempdir().unwrap();
    let p = parent();
    let c = config(temp.path(), 2);
    let binding = json!({"generator_config":c});
    for percent in PERCENTAGES {
        let (plan, row) = task(&p, &c, &binding, percent);
        generation::check_plan(&plan, &p, &binding, percent, str_at(&plan, "id").unwrap()).unwrap();
        balanced::validate_descendant_with_slot(
            &row,
            &p,
            &c,
            &binding,
            &plan,
            2,
            percent,
            generation::check_slot,
        )
        .unwrap();
        if percent == 100 {
            assert_eq!(row["origin"], "mixed");
            assert_eq!(row["share_target"], 1.);
            assert_eq!(row["human_role_present"], true);
        }
        if percent == 20 {
            assert_ne!(row["share_target"], 0.2);
        }
        let mut bad = plan.clone();
        bad["request"]["messages"][0]["content"] = json!("Changed synthetic request");
        assert!(
            generation::check_plan(&bad, &p, &binding, percent, str_at(&plan, "id").unwrap())
                .is_err()
        );
        let mut bad = row.clone();
        bad["source_group"] = json!("different-work");
        assert!(
            balanced::validate_descendant_with_slot(
                &bad,
                &p,
                &c,
                &binding,
                &plan,
                2,
                percent,
                generation::check_slot
            )
            .is_err()
        );
    }
}
#[test]
fn all_four_tasks_require_exact_intents_responses_and_terminal_accounting() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let run = root.join("run");
    fs::create_dir(&run).unwrap();
    let c = config(root, 2);
    put(&root.join("config.json"), &c);
    let config_hash = sha(&bytes(&root.join("config.json")).unwrap());
    let p = parent();
    let hs = sha(&jsonl(&[p.clone()]).unwrap());
    let producer = sha(b"synthetic producer");
    let binding = json!({"schema":"slop_ninja_controlled_generation_v2","test_opened":true,"input_sha256":hs,"generator":MODELS[2],
        "generator_config":c,"generator_config_sha256":config_hash,"producer_sha256":producer,"endpoint":c["endpoint"],
        "requested_span_percentages":[20,60,100],"span_policy":"nearest_sentence_start_middle_end_by_source_digest_v2",
        "share_word_policy":"unicode_split_whitespace_v1","seed":1729,"temperature":0.7,"top_p":0.9,"max_tokens":1024,"thinking":false,"input_schema":"slop_ninja_a_supervised_record_v1"});
    put(&run.join("binding.json"), &binding);
    let props = json!({"model_path":c["model_path"],"total_slots":1,"default_generation_settings":{"n_ctx":8192},
        "chat_template":"synthetic template","chat_template_caps":{},"build_info":"synthetic","bos_token":"bos","eos_token":"eos"});
    put(&root.join("props.json"), &props);
    put(&run.join("server-properties.json"), &props);
    let spec = json!({"run_directory":run,"config":pin(&root.join("config.json")),"server_properties":pin(&root.join("props.json"))});
    let mut all = Vec::new();
    for percent in PERCENTAGES {
        let (plan, mut row) = task(&p, &c, &binding, percent);
        let dir = run.join(str_at(&plan, "id").unwrap());
        fs::create_dir(&dir).unwrap();
        put(&dir.join("plan.json"), &plan);
        let ph = sha(&bytes(&dir.join("plan.json")).unwrap());
        put(
            &dir.join("context.json"),
            &json!({"request_sha256":compact_sha(&plan["request"]).unwrap(),"rendered_prompt_sha256":sha(b"rendered"),
            "prompt_tokens":100,"max_output_tokens":1024,"context_capacity":8192,"fits":true}),
        );
        put(
            &dir.join("attempt-0.intent.json"),
            &json!({"request":plan["request"],"plan_sha256":ph,"attempt":0}),
        );
        let body = json!({"choices":[{"finish_reason":"stop","message":{"content":row["share_evidence"]["replacement"]}}]});
        put(
            &dir.join("attempt-0.json"),
            &json!({"status":200,"elapsed_ms":1,"body":body,"raw_body":serde_json::to_string(&body).unwrap()}),
        );
        row["generation"]["response_path"] = json!(dir.join("attempt-0.json"));
        row["generation"]["response_sha256"] =
            json!(sha(&bytes(&dir.join("attempt-0.json")).unwrap()));
        put(&dir.join("record.json"), &row);
        put(
            &dir.join("record.sha256.json"),
            &json!({"sha256":sha(&bytes(&dir.join("record.json")).unwrap()),"plan_sha256":ph}),
        );
        all.push(row);
    }
    let rb = jsonl(&all).unwrap();
    fs::write(run.join("records.jsonl"), &rb).unwrap();
    let report = json!({"schema":"slop_ninja_controlled_generation_report_v2","complete":true,"test_opened":true,"input_sha256":hs,
        "binding_sha256":compact_sha(&binding).unwrap(),"records_sha256":sha(&rb),"accepted":4,"failures":[]});
    put(&run.join("report.json"), &report);
    let (_, summary) = generation::read(
        &spec,
        2,
        &c,
        &[p.clone()],
        &hs,
        &producer,
        &mut Vec::new(),
        &mut BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(summary["requested"], 4);
    assert_eq!(summary["planned_requested"], 128);
    let mut bad = report;
    bad["failures"] = json!([{"id":all[0]["id"],"parent_id":p["id"],"reason":"no_accepted_response_after_bounded_attempts"}]);
    put(&run.join("report.json"), &bad);
    assert!(
        generation::read(
            &spec,
            2,
            &c,
            &[p],
            &hs,
            &producer,
            &mut Vec::new(),
            &mut BTreeMap::new()
        )
        .is_err()
    );
}
fn ledger() -> Value {
    json!({"protected_ids":[],"protected_source_groups":[],"protected_work_aliases":[],"protected_text_sha256":[],"protected_normalized_text_sha256":[],"protected_scratch_prompt_sha256":[]})
}
#[test]
fn cumulative_collisions_stop_instead_of_dropping_test_rows() {
    let p = parent();
    for key in [
        "protected_ids",
        "protected_source_groups",
        "protected_text_sha256",
        "protected_normalized_text_sha256",
    ] {
        let mut v = ledger();
        v[key] = json!([match key {
            "protected_ids" => str_at(&p, "id").unwrap().to_owned(),
            "protected_source_groups" => str_at(&p, "source_group").unwrap().to_owned(),
            "protected_text_sha256" => str_at(&p, "text_sha256").unwrap().to_owned(),
            _ => normalized(str_at(&p, "text").unwrap()),
        }]);
        assert!(
            overlap::synthetic_ledger(&v)
                .unwrap()
                .check(&BTreeMap::new(), &[p.clone()], &[], &BTreeMap::new())
                .is_err()
        );
    }
    let w = json!({"pg_id":7,"source_group":"new-work","purpose":"query","selection_row":{"expected_title":"The Copper Door: A Tale (Revised)"}});
    let aliases = overlap::aliases(&w).unwrap();
    assert!(aliases.contains("title:copperdoor"));
    assert!(aliases.contains("title:thecopperdoor"));
    let mut v = ledger();
    v["protected_work_aliases"] = json!(["title:copperdoor"]);
    assert!(
        overlap::synthetic_ledger(&v)
            .unwrap()
            .check(
                &BTreeMap::from([(7, w.clone())]),
                &[],
                &[],
                &BTreeMap::new()
            )
            .is_err()
    );
    let mut v = ledger();
    v["protected_scratch_prompt_sha256"] =
        json!([sha(recipe::scratch_prompt("author-pg7-p20").as_bytes())]);
    assert!(
        overlap::synthetic_ledger(&v)
            .unwrap()
            .check(&BTreeMap::from([(7, w)]), &[], &[], &BTreeMap::new())
            .is_err()
    );
}
#[test]
fn same_family_duplicates_keep_original_and_retain_all_generated_evidence() {
    let p = parent();
    let mut a = p.clone();
    a["id"] = json!("generated-a");
    a["origin"] = json!("mixed");
    let mut b = a.clone();
    b["id"] = json!("generated-b");
    let (kept, excluded, raw) = overlap::within_test(vec![p.clone(), a.clone(), b], &[]).unwrap();
    assert_eq!(kept, vec![p.clone()]);
    assert_eq!(excluded.len(), 2);
    assert_eq!(raw.len(), 2);
    a["source_group"] = json!("another-family");
    assert!(overlap::within_test(vec![p.clone(), a], &[]).is_err());
    let mut r = p.clone();
    r["id"] = json!("reference");
    r["purpose"] = json!("reference");
    assert!(overlap::within_test(vec![p], &[r]).is_err());
}

struct SourceFixture {
    temp: tempfile::TempDir,
    plan: Value,
    selection_sha: String,
}
impl SourceFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let works = sources::works().unwrap();
        let selection_sha = sha(b"synthetic selection");
        let sources=works.values().map(|w|{let s=&w["selection_row"];json!({"pg_id":w["pg_id"],"selection_row":s,"split":"test","preassigned_partition":s["split"],
            "purpose":w["purpose"],"role":w["purpose"],"source_group":w["source_group"],"underlying_work_key":w["source_group"],"author_key":s["author_key"],
            "author":s["author"],"catalog_author":s["catalog_author"],"title":s["expected_title"],"enrollment":s["enrollment"],"author_partition":s["author_partition"],
            "catalog_evidence":{"sha256":s["metadata_sha256"]},"rights":{"commercial_training":true,"external_evaluation":true,"jurisdiction":"US","catalog_statement":"Public domain in the USA.","evidence_sha256":s["metadata_sha256"]},
            "evidence":"historical_public_domain_author_proxy","text_sha256":sha(b"synthetic whole work"),"text_bytes":100000,
            "raw_download":{"sha256":sha(b"synthetic whole work"),"bytes":100000,"status":200}})}).collect::<Vec<_>>();
        let m = json!({"schema":"slop_ninja.author_sources.v1","acquisition_schema":"slop_ninja_balanced_final_test_acquisition_v1","complete":true,"test_text_opened":true,
            "selection_receipt_sha256":selection_sha,"selection_sha256":TSV_SHA,"source_count":18,"query_count":16,"reference_count":2,"reserved_sources":[],"sources":sources});
        put(&root.join("sources.json"), &m);
        let msha = sha(&bytes(&root.join("sources.json")).unwrap());
        let (mut humans, mut refs, mut planned) = (Vec::new(), Vec::new(), Vec::new());
        for s in array(&m, "sources").unwrap() {
            for fraction in [20, 60] {
                let id = format!("author-pg{}-p{fraction}", s["pg_id"]);
                let mut r = parent();
                r["id"] = json!(id);
                r["source_group"] = s["source_group"].clone();
                r["purpose"] = s["purpose"].clone();
                r["human_identity"] = s["author_key"].clone();
                r["rights"] = s["rights"].clone();
                r["grammar_exposure"] = json!(
                    "conservative_unknown_prior_exposure;fresh_acquisition_is_not_an_exposure_audit"
                );
                r["source"] = json!({"record":s,"source_manifest_sha256":msha,"byte_span":[100,100+str_at(&r,"text").unwrap().len()],"extraction":"body_percent20_60_next_sentence_220words_end_sentence_max600_v1"});
                planned.push(json!({"id":id,"fraction":fraction,"source_group":s["source_group"],"frozen_split":"test","purpose":s["purpose"]}));
                if s["purpose"] == "query" {
                    humans.push(r);
                } else {
                    refs.push(r);
                }
            }
        }
        fs::write(root.join("humans.jsonl"), jsonl(&humans).unwrap()).unwrap();
        fs::write(root.join("references.jsonl"), jsonl(&refs).unwrap()).unwrap();
        let q = json!({"schema":"slop_ninja_author_queries_v2","mode":"fresh-balanced","test_mode":true,"test_text_opened":true,"source_manifest_sha256":msha,"non_test_ids":[],
            "planned_records":planned,"all_ids":planned.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"test_ids":planned.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),
            "test_query_ids":humans.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"test_reference_ids":refs.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),
            "emitted_human_ids":humans.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"emitted_reference_ids":refs.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),
            "records":humans.len(),"references":refs.len(),"records_sha256":sha(&jsonl(&humans).unwrap()),"references_sha256":sha(&jsonl(&refs).unwrap()),"failures":[]});
        put(&root.join("queries.json"), &q);
        let plan = json!({"source_manifest":pin(&root.join("sources.json")),"query_manifest":pin(&root.join("queries.json")),"humans":pin(&root.join("humans.jsonl")),"references":pin(&root.join("references.jsonl"))});
        Self {
            temp,
            plan,
            selection_sha,
        }
    }
}
#[test]
fn exact_32_query_and_four_reference_excerpts_and_source_ancestry() {
    let f = SourceFixture::new();
    let loaded = sources::read(&f.plan, &f.selection_sha, &mut Vec::new()).unwrap();
    assert_eq!(loaded.humans.len(), 32);
    assert_eq!(loaded.references.len(), 4);
    let mut w = sources::works().unwrap().into_values().next().unwrap();
    w["selection_row"]["author_key"] = json!("invented-writer");
    let s = &loaded.humans[0]["source"]["record"];
    assert!(sources::source(s, &w).is_err());
    let mut bad = s.clone();
    bad["rights"]["external_evaluation"] = json!(false);
    let works = sources::works().unwrap();
    assert!(sources::source(&bad, &works[&counter(s, "pg_id").unwrap()]).is_err());
}
#[test]
fn extraction_failures_preserve_416_requests_without_fabricated_attempts() {
    let mut f = SourceFixture::new();
    let root = f.temp.path();
    let mut h = records(&bytes(&root.join("humans.jsonl")).unwrap()).unwrap();
    let missing = h.pop().unwrap();
    fs::write(root.join("humans.jsonl"), jsonl(&h).unwrap()).unwrap();
    let (mut q, _) = load(&root.join("queries.json"), &mut Vec::new()).unwrap();
    q["records"] = json!(31);
    q["records_sha256"] = json!(sha(&jsonl(&h).unwrap()));
    q["emitted_human_ids"] = json!(h.iter().map(|r| r["id"].clone()).collect::<Vec<_>>());
    q["failures"] = json!([{"id":missing["id"],"frozen_split":"test","purpose":"query","reason":"synthetic extraction failure"}]);
    put(&root.join("queries.json"), &q);
    f.plan["query_manifest"] = pin(&root.join("queries.json"));
    f.plan["humans"] = pin(&root.join("humans.jsonl"));
    let s = sources::read(&f.plan, &f.selection_sha, &mut Vec::new()).unwrap();
    assert_eq!(s.query_exclusions.len(), 13);
    let mut accepted = h
        .iter()
        .map(|r| str_at(r, "id").unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    for r in &h {
        for model in MODELS {
            for p in PERCENTAGES {
                accepted.insert(balanced::task_id(str_at(r, "id").unwrap(), model, p));
            }
        }
    }
    let m = json!({"schema":"slop_ninja_a_supervised_dataset_v1","complete":true,"test_text_opened":true,"non_test_ids":[],"test_ids":accepted,
        "records":accepted.len(),"requested_records":416,"excluded":s.query_exclusions,
        "generation_reports":(0..3).map(|_|json!({"complete":true,"requested":124,"accepted":124,"failed":0,"failures":[]})).collect::<Vec<_>>()});
    assert_eq!(
        evaluation_binding::upstream_accounting(&m, &accepted).unwrap()["requested"],
        416
    );
}

#[test]
fn self_rehashed_reference_cannot_enter_query_or_generation_roster() {
    let mut f = SourceFixture::new();
    let root = f.temp.path();
    let mut humans = records(&bytes(&root.join("humans.jsonl")).unwrap()).unwrap();
    humans[0]["purpose"] = json!("reference");
    let hb = jsonl(&humans).unwrap();
    fs::write(root.join("humans.jsonl"), &hb).unwrap();
    let (mut q, _) = load(&root.join("queries.json"), &mut Vec::new()).unwrap();
    q["records_sha256"] = json!(sha(&hb));
    put(&root.join("queries.json"), &q);
    f.plan["query_manifest"] = pin(&root.join("queries.json"));
    f.plan["humans"] = pin(&root.join("humans.jsonl"));
    assert!(sources::read(&f.plan, &f.selection_sha, &mut Vec::new()).is_err());
    let c = config(root, 0);
    let mut p = parent();
    p["purpose"] = json!("reference");
    let binding = json!({"generator_config":c});
    let (plan, _) = task(&p, &c, &binding, 20);
    assert!(generation::check_plan(&plan, &p, &binding, 20, str_at(&plan, "id").unwrap()).is_err());
}
