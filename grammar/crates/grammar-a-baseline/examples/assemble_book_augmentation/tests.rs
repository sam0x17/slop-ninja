use super::*;
fn parent() -> Value {
    let t = (0..30)
        .map(|i| format!("Élan synthetic sentence {i} has five more words."))
        .collect::<Vec<_>>()
        .join(" ");
    json!({"schema":"slop_ninja_a_supervised_record_v1","id":"synthetic-book-root","text":t,"text_sha256":sha(t.as_bytes()),
        "source_group":"synthetic-book-family","frozen_split":"train","purpose":"query","origin":"human_only",
        "evidence":"historical_public_domain_author_proxy","documented_human_only":false,"human_identity":"henry_james",
        "human_role_present":true,"ai_identity":null,"ai_role_present":false,"share_target":0.0,"share_evidence":{"basis":"historical_proxy"},
        "source":{"fixture":"synthetic"},"rights":{"commercial_training":true,"external_evaluation":true},
        "exposed_to_grammar":true,"grammar_exposure":"unknown","metadata":{"fixture":true},"parent_id":null})
}
fn config() -> Value {
    json!({"schema":"slop_ninja_local_generator_v1","identity":MODELS[0],"sha256":MODELS[0].split(':').next_back().unwrap(),
        "repo":"synthetic","revision":"pinned","filename":"synthetic.gguf","model_path":"/synthetic/model","runtime_path":"/synthetic/runtime",
        "runtime_sha256":"b".repeat(64),"runtime_libraries":[{"path":"/synthetic/library","sha256":"c".repeat(64)}],
        "alias":"synthetic","endpoint":"http://127.0.0.1:18086","allowed_splits":["train"],"rights":{"generation_permitted":true,"model_license":"Apache-2.0"}})
}
fn binding() -> Value {
    json!({"schema":"slop_ninja_controlled_generation_v2","input_sha256":"a".repeat(64),"generator":MODELS[0],
        "generator_config":config(),"generator_config_sha256":compact_sha(&config()).unwrap(),"producer_sha256":"e".repeat(64),
        "share_word_policy":"unicode_split_whitespace_v1","requested_span_percentages":[20,60,100],
        "span_policy":"nearest_sentence_start_middle_end_by_source_digest_v2","test_opened":false})
}
fn plan(p: &Value, b: &Value, percent: u64) -> Value {
    let text = str_at(p, "text").unwrap();
    let id = str_at(p, "id").unwrap();
    let pos = balanced::position(id, percent);
    let (s, e) = if percent == 0 {
        (0, 0)
    } else {
        recipe::span(
            text,
            percent as usize,
            ["start", "middle", "end"]
                .iter()
                .position(|x| *x == pos)
                .unwrap(),
        )
    };
    let prompt = if percent == 0 {
        recipe::scratch_prompt(id)
    } else {
        format!(
            "Rewrite the following English narrative passage in your own words, keeping its events, perspective, and approximate length. Return only the replacement prose in one paragraph: no title, preface, analysis, quotation delimiters, or explanation.\n\n{}",
            &text[s..e]
        )
    };
    json!({"id":balanced::task_id(id,MODELS[0],percent),"parent_id":id,"source_sha256":p["text_sha256"],
        "source_record_sha256":compact_sha(p).unwrap(),"binding_sha256":compact_sha(b).unwrap(),"position":pos,
        "mode":if percent==0{"from_scratch"}else{"controlled_rewrite"},"requested_source_span_percent":percent,"source_byte_span":[s,e],"sham_reassembly_exact":true,
        "request":{"model":"synthetic","messages":[{"role":"user","content":prompt}],"seed":1729,"temperature":0.7,"top_p":0.9,"max_tokens":1024,"stream":false,
        "chat_template_kwargs":{"enable_thinking":false},"reasoning_format":"none"}})
}
fn write_json(path: &Path, v: &Value) {
    fs::write(path, serde_json::to_vec(v).unwrap()).unwrap();
}
fn response() -> Value {
    let body = json!({"choices":[{"finish_reason":"stop","message":{"content":"A wholly synthetic replacement with enough words for the fixture."}}]});
    json!({"status":200,"raw_body":serde_json::to_string(&body).unwrap(),"body":body,"elapsed_ms":1})
}
fn fixture(dir: &Path, accepted: bool) -> (Value, Value, Vec<Value>) {
    let p = parent();
    let b = binding();
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    write_json(&dir.join("binding.json"), &b);
    for percent in PERCENTAGES {
        let plan = plan(&p, &b, percent);
        let id = str_at(&plan, "id").unwrap();
        let task = dir.join(id);
        fs::create_dir(&task).unwrap();
        write_json(&task.join("plan.json"), &plan);
        write_json(
            &task.join("context.json"),
            &json!({"request_sha256":compact_sha(&plan["request"]).unwrap(),"rendered_prompt_sha256":"f".repeat(64),
            "prompt_tokens":300,"max_output_tokens":1024,"context_capacity":8192,"fits":true}),
        );
        for n in 0..if accepted { 1 } else { 2 } {
            write_json(
                &task.join(format!("attempt-{n}.intent.json")),
                &json!({"request":plan["request"],"plan_sha256":sha(&bytes(&task.join("plan.json")).unwrap()),"attempt":n}),
            );
            write_json(
                &task.join(format!("attempt-{n}.json")),
                &if accepted {
                    response()
                } else {
                    json!({"status":0,"error":"synthetic transport failure","elapsed_ms":1})
                },
            );
        }
        if accepted {
            let response = response();
            let replacement =
                str_at(&response["body"]["choices"][0]["message"], "content").unwrap();
            let text = str_at(&p, "text").unwrap();
            let s = plan["source_byte_span"][0].as_u64().unwrap() as usize;
            let e = plan["source_byte_span"][1].as_u64().unwrap() as usize;
            let final_text = if percent == 0 {
                replacement.to_owned()
            } else {
                format!("{}{}{}", &text[..s], replacement, &text[e..])
            };
            let share = replacement.split_whitespace().count() as f64
                / final_text.split_whitespace().count() as f64;
            let mut r = p.clone();
            r["id"] = plan["id"].clone();
            r["parent_id"] = p["id"].clone();
            r["text"] = json!(final_text);
            r["text_sha256"] = json!(sha(final_text.as_bytes()));
            r["origin"] = json!(if percent == 0 { "model_only" } else { "mixed" });
            r["human_role_present"] = json!(percent != 0);
            if percent == 0 {
                r["human_identity"] = Value::Null;
            }
            r["ai_role_present"] = json!(true);
            r["ai_identity"] = json!(MODELS[0]);
            r["share_target"] = json!(share);
            r["generation"] = json!({"generator":MODELS[0],"binding":b,"request":plan["request"],"position":plan["position"],"mode":plan["mode"],"attempt":0,
                "response_path":task.join("attempt-0.json"),"response_sha256":sha(&bytes(&task.join("attempt-0.json")).unwrap())});
            r["share_evidence"] = json!({"schema":"slop_ninja_realized_span_share_v1","source_text":text,"source_sha256":p["text_sha256"],"source_byte_span":[s,e],"replacement":replacement,
                "final_byte_span":[s,s+replacement.len()],"final_sha256":r["text_sha256"],"replacement_words":replacement.split_whitespace().count(),"final_words":final_text.split_whitespace().count(),
                "word_policy":"unicode_split_whitespace_v1","realized_share":share,"requested_percent_not_target":percent,"full_rewrite_is_mixed_history":percent==100});
            write_json(&task.join("record.json"), &r);
            write_json(
                &task.join("record.sha256.json"),
                &json!({"sha256":sha(&bytes(&task.join("record.json")).unwrap()),"plan_sha256":sha(&bytes(&task.join("plan.json")).unwrap())}),
            );
            rows.push(r);
        } else {
            failures.push(json!({"id":id,"parent_id":p["id"],"mode":plan["mode"],"requested_span_percent":percent,"reason":"no_accepted_response_after_bounded_attempts"}));
        }
    }
    let bytes = jsonl(&rows).unwrap();
    fs::write(dir.join("records.jsonl"), &bytes).unwrap();
    write_json(
        &dir.join("report.json"),
        &json!({"schema":"slop_ninja_controlled_generation_report_v2","complete":true,"test_opened":false,
        "input_sha256":b["input_sha256"],"binding_sha256":compact_sha(&b).unwrap(),"records_sha256":sha(&bytes),"accepted":rows.len(),"failures":failures}),
    );
    (p, b, rows)
}
fn validate(dir: &Path, p: &Value) -> Result<(Vec<Value>, Value)> {
    balanced::generator_for_roster(
        dir,
        0,
        &config(),
        &compact_sha(&config())?,
        &[p.clone()],
        &"a".repeat(64),
        &mut Vec::new(),
        &mut BTreeMap::new(),
        &balanced::GenerationPolicy {
            requested_tasks: 4,
            producer_sha256: &"e".repeat(64),
            slot_check: generation::check_slot,
            plan_check: generation::check_plan,
        },
    )
}
#[test]
fn v2_complete_synthetic_generation_reuses_lineage_and_realized_share() {
    let d = tempfile::tempdir().unwrap();
    let (p, _, _) = fixture(d.path(), true);
    let (rows, r) = validate(d.path(), &p).unwrap();
    assert_eq!(r["requested"], 4);
    assert_eq!(r["failed"], 0);
    assert_eq!(
        rows.iter()
            .filter(|r| r["human_identity"] == "henry_james")
            .count(),
        3
    );
    let full = rows
        .iter()
        .find(|r| r["share_evidence"]["requested_percent_not_target"] == 100)
        .unwrap();
    assert_eq!(full["origin"], "mixed");
    assert_eq!(full["share_target"], 1.0);
    let mut bad = full.clone();
    bad["share_evidence"]["final_byte_span"] = json!([0, 1]);
    assert!(supervision::Supervision::parse(&bad).is_err());
    let mut bad = full.clone();
    bad["human_role_present"] = json!(false);
    assert!(supervision::Supervision::parse(&bad).is_err());
}
#[test]
fn all_failed_tasks_preserve_denominator_and_incomplete_is_rejected() {
    let d = tempfile::tempdir().unwrap();
    let (p, _, _) = fixture(d.path(), false);
    let (rows, r) = validate(d.path(), &p).unwrap();
    assert!(rows.is_empty());
    assert_eq!(r["requested"], 4);
    assert_eq!(r["failed"], 4);
    let path = d.path().join("report.json");
    let mut report: Value = serde_json::from_slice(&bytes(&path).unwrap()).unwrap();
    report["failures"].as_array_mut().unwrap().pop();
    write_json(&path, &report);
    assert!(validate(d.path(), &p).is_err());
}
#[test]
fn fixed_recipe_rejects_scratch_prompt_sampler_and_span_substitution() {
    let p = parent();
    let b = binding();
    for pct in PERCENTAGES {
        let q = plan(&p, &b, pct);
        generation::check_plan(&q, &p, &b, pct, str_at(&q, "id").unwrap()).unwrap();
    }
    let q = plan(&p, &b, 0);
    let mut bad = q.clone();
    bad["request"]["messages"][0]["content"] = json!("Changed topic");
    assert!(generation::check_plan(&bad, &p, &b, 0, str_at(&q, "id").unwrap()).is_err());
    let mut bad = q.clone();
    bad["request"]["top_k"] = json!(1);
    assert!(generation::check_plan(&bad, &p, &b, 0, str_at(&q, "id").unwrap()).is_err());
    let q = plan(&p, &b, 20);
    let mut bad = q.clone();
    bad["source_byte_span"] = json!([0, 5]);
    assert!(generation::check_plan(&bad, &p, &b, 20, str_at(&q, "id").unwrap()).is_err());
}
#[test]
fn known_slots_and_train_scope_cannot_expand() {
    generation::check_slot(0, &config(), "train").unwrap();
    for split in ["development", "calibration", "test", "test_reference"] {
        assert!(generation::check_slot(0, &config(), split).is_err());
    }
    assert!(generation::check_slot(1, &config(), "train").is_err());
    assert!(generation::check_slot(2, &config(), "train").is_err());
    let mut c = config();
    c["allowed_splits"] = json!(["train", "test"]);
    assert!(generation::check_slot(0, &c, "train").is_err());
}
#[test]
fn invalid_terminal_failure_and_skipped_success_rejected_uncertainty_retained() {
    let d = tempfile::tempdir().unwrap();
    let (p, b, _) = fixture(d.path(), false);
    let q = plan(&p, &b, 20);
    let task = d.path().join(str_at(&q, "id").unwrap());
    generation::attempts(&task, None, &mut Vec::new()).unwrap();
    write_json(&task.join("attempt-0.json"), &response());
    assert!(generation::attempts(&task, None, &mut Vec::new()).is_err());
    let row = json!({"generation":{"attempt":1}});
    write_json(&task.join("attempt-1.json"), &response());
    assert!(generation::attempts(&task, Some(&row), &mut Vec::new()).is_err());
    fs::remove_file(task.join("attempt-0.json")).unwrap();
    let mut evidence = Vec::new();
    generation::attempts(&task, Some(&row), &mut evidence).unwrap();
    assert_eq!(
        evidence.last().unwrap()["uncertain_attempt_indices"],
        json!([0])
    );
    let mut r = response();
    r["raw_body"] = json!("{}");
    assert!(generation::replacement(&r).is_err());
}
#[test]
fn every_duplicate_copy_is_excluded_and_protected_family_is_fatal() {
    let p = parent();
    let mut a = p.clone();
    a["id"] = json!("child-a");
    a["parent_id"] = p["id"].clone();
    a["text"] = json!("duplicate generated words");
    let mut b = a.clone();
    b["id"] = json!("child-b");
    b["text"] = json!("duplicate\n generated words");
    let (rows, excluded) =
        exclude_duplicates(&[p.clone()], vec![a.clone(), b], &Protected::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(excluded.len(), 2);
    let mut protected = Protected::default();
    protected
        .texts
        .insert(normalized("duplicate generated words"));
    assert_eq!(
        exclude_duplicates(&[p.clone()], vec![a], &protected)
            .unwrap()
            .1
            .len(),
        1
    );
    protected
        .families
        .insert(str_at(&p, "source_group").unwrap().into());
    assert!(exclude_duplicates(&[p], vec![], &protected).is_err());
}
#[test]
fn exact_pg_excerpt_is_utf8_bound_and_not_arbitrary_220_words() {
    let raw = format!(
        "Synthetic metadata\n*** START OF SYNTHETIC ***\n{}\n*** END OF SYNTHETIC ***\n",
        (0..500)
            .map(|i| format!("Élan synthetic sentence {i} ends here."))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let (a, b) = recipe::excerpt(&raw, 20).unwrap();
    let (c, d) = recipe::excerpt(&raw, 60).unwrap();
    assert!(raw.is_char_boundary(a) && raw.is_char_boundary(b) && a < b && b < c && c < d);
    assert!(raw[a..b].split_whitespace().count() >= 220);
    assert!(raw[a..b].ends_with('.'));
    assert!(recipe::excerpt("missing PG markers", 20).is_err());
}
#[test]
fn hash_pins_and_preflight_incomplete_block_without_corpus_reads() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("meta.json");
    write_json(&f, &json!({"a":1}));
    let p = pin(&f).unwrap();
    pinned_json(&p, &mut Vec::new()).unwrap();
    write_json(&f, &json!({"a":2}));
    assert!(pinned_json(&p, &mut Vec::new()).is_err());
    assert!(admission::root_manifest(&json!({"schema":"slop_ninja_train_book_roots_v1","complete":true,"test_text_opened":true,"test_ids":[]})).is_err());
    let mut source = json!({"split":"test","preassigned_partition":"test","role":"query","purpose":"query","enrollment":"known"});
    assert!(admission::source_scope(&source, &Value::Null, &BTreeSet::new()).is_err());
    source["split"] = json!("test_reference");
    assert!(admission::source_scope(&source, &Value::Null, &BTreeSet::new()).is_err());
}
#[test]
fn default_template_and_runtime_sampler_cannot_change() {
    let p = json!({"model_path":"/synthetic/model","total_slots":1,"default_generation_settings":{"n_ctx":8192,"params":{"top_k":20}},
        "chat_template":"synthetic template","chat_template_caps":{},"build_info":"synthetic-runtime","bos_token":"start","eos_token":"end"});
    generation::check_server(&p, &p, &config()).unwrap();
    let mut bad = p.clone();
    bad["chat_template"] = json!("different");
    assert!(generation::check_server(&bad, &p, &config()).is_err());
    let mut bad = p.clone();
    bad["default_generation_settings"]["params"]["top_k"] = json!(40);
    assert!(generation::check_server(&bad, &p, &config()).is_err());
}
