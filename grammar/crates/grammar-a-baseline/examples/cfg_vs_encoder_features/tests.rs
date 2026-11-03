//! Synthetic-text tests only: no corpus, model, detector or network access.
use crate::{cfg, common, encoder, select, stylo};
use serde_json::json;
use std::collections::BTreeSet;

#[test]
fn role_hash_bucket_and_fold_follow_the_frozen_formula() {
    let h = select::role_hash("synthetic_domain", "synthetic:writer-1");
    assert_eq!(
        h,
        "18e2dc2a8a0942af39b91c604bdb1399e39908d10c55ac8ceeb3690d389bdb2e"
    );
    assert_eq!(select::bucket(&h), 682);
    assert_eq!(select::role(select::bucket(&h)), "eval");
    assert_eq!(select::fold(&h), 3);
    assert_eq!(select::role(499), "train");
    assert_eq!(select::role(989), "eval");
    assert_eq!(select::role(990), "feasibility_only");
    assert_eq!(
        select::doc_rank("synthetic-doc-1"),
        "0670c4756311dca8dfadebf2e20e29d7f86577f82ec0264b88192581c564c0d4"
    );
}

fn docs(spec: &[(&str, &str)]) -> Vec<select::RankedDoc> {
    spec.iter().map(|(id, g)| select::RankedDoc::new(id, g)).collect()
}

#[test]
fn queries_and_references_never_share_a_source_group() {
    let mut spec = Vec::new();
    let ids: Vec<String> = (0..24).map(|i| format!("synthetic-doc-{i}")).collect();
    for (i, id) in ids.iter().enumerate() {
        spec.push((id.as_str(), ["g-a", "g-b", "g-c", "g-d", "g-e", "g-f"][i % 6]));
    }
    let d = docs(&spec);
    let split = select::split_author(&d);
    assert_eq!(split.groups, 6);
    assert_eq!(split.queries.len(), 3);
    assert_eq!(split.references.len(), 5);
    assert!(split.eligible());
    let group = |id: &str| d.iter().find(|x| x.id == id).unwrap().group.clone();
    let qg: BTreeSet<_> = split.queries.iter().map(|q| group(q)).collect();
    assert_eq!(qg.len(), 3, "one query per group");
    for r in &split.references {
        assert!(!qg.contains(&group(r)));
    }
    // Ranked groups: queries are the first document of the three lowest groups.
    let ranked = select::ranked_groups(&d);
    for (i, q) in split.queries.iter().enumerate() {
        assert_eq!(ranked[i][0].id, *q);
    }
    // Round-robin: the first three references come from three distinct groups.
    let rg: Vec<_> = split.references.iter().map(|r| group(r)).collect();
    assert_eq!(rg[0], ranked[3][0].group);
    assert_eq!(rg[1], ranked[4][0].group);
    assert_eq!(rg[2], ranked[5][0].group);
    assert_eq!(split.references[3], ranked[3][1].id);
}

#[test]
fn query_count_is_bounded_by_group_count_minus_one() {
    let one = docs(&[("synthetic-a", "g1"), ("synthetic-b", "g1"), ("synthetic-c", "g1")]);
    let s = select::split_author(&one);
    assert!(s.queries.is_empty());
    assert!(!s.eligible());
    let two = docs(&[
        ("synthetic-a", "g1"),
        ("synthetic-b", "g2"),
        ("synthetic-c", "g2"),
        ("synthetic-d", "g2"),
        ("synthetic-e", "g2"),
        ("synthetic-f", "g2"),
        ("synthetic-g", "g1"),
    ]);
    let s = select::split_author(&two);
    assert_eq!(s.queries.len(), 1);
    // Five references must come from the single remaining group.
    let qgroup = two.iter().find(|d| d.id == s.queries[0]).unwrap().group.clone();
    let remaining = two.iter().filter(|d| d.group != qgroup).count();
    assert_eq!(s.eligible(), remaining >= 5);
}

#[test]
fn t3_profile_excludes_voice_source_groups() {
    let d = docs(&[
        ("synthetic-1", "work-a"),
        ("synthetic-2", "work-a"),
        ("synthetic-3", "work-b"),
        ("synthetic-4", "work-b"),
        ("synthetic-5", "work-c"),
        ("synthetic-6", "work-c"),
        ("synthetic-7", "work-c"),
    ]);
    let excluded: BTreeSet<String> = ["work-a".to_string()].into();
    let p = select::t3_profile(&d, &excluded);
    assert_eq!(p.len(), 5);
    assert!(!p.contains(&"synthetic-1".to_string()) && !p.contains(&"synthetic-2".to_string()));
    let both: BTreeSet<String> = ["work-a".to_string(), "work-b".to_string()].into();
    assert_eq!(select::t3_profile(&d, &both).len(), 3);
}

#[test]
fn word_tokens_are_lowercased_alphabetic_runs() {
    let w = stylo::word_tokens("Don't STOP\u{2014}\u{dc}n\u{ef}code 42 words_here, ok?");
    assert_eq!(w, ["don", "t", "stop", "\u{fc}n\u{ef}code", "words", "here", "ok"]);
    let fw: Vec<String> = ["the", "ok", "t"].iter().map(|s| s.to_string()).collect();
    assert_eq!(stylo::function_word_counts(&w, &fw), vec![0, 1, 1]);
}

#[test]
fn char_trigrams_collapse_whitespace_runs_without_trimming() {
    let (total, grams) = stylo::char3_counts("Ab \t\n c\n");
    // "ab c " -> "ab ", "b c", " c "
    assert_eq!(total, 3);
    assert_eq!(grams.get("ab "), Some(&1));
    assert_eq!(grams.get("b c"), Some(&1));
    assert_eq!(grams.get(" c "), Some(&1));
    let (total, grams) = stylo::char3_counts("aaaa");
    assert_eq!(total, 2);
    assert_eq!(grams.get("aaa"), Some(&2));
    assert_eq!(stylo::char3_counts("ab").0, 0);
}

fn window(start: usize, end: usize, tier: &str, tokens: usize) -> String {
    let toks: Vec<_> = (0..tokens).map(|i| json!({"byte_span":[i, i + 1]})).collect();
    json!({"source":{"start":start,"end":end,"boundary":"SentenceProposal"},"tier":tier,
        "native_status":"ambiguous","limit_reason":if tier == "resource_limit" {json!("alternatives")} else {json!(null)},
        "execution_error":null,"projection_error":null,"forest":{"alternatives":[]},"tokens":toks})
    .to_string()
}

#[test]
fn window_index_and_restricted_text_use_complete_windows_in_source_order() {
    let text = "One short line. Two more words here. Third part stays out. Fourth ends.";
    let spans = [(0, 15), (16, 36), (37, 58), (59, text.len())];
    let lines = [
        window(spans[2].0, spans[2].1, "partial", 4),
        window(spans[0].0, spans[0].1, "native_complete", 4),
        window(spans[3].0, spans[3].1, "lexical_fallback_complete", 3),
        window(spans[1].0, spans[1].1, "resource_limit", 5),
    ]
    .join("\n");
    let row = cfg::index_windows("synthetic", "00000", "digest", lines.as_bytes()).unwrap();
    assert_eq!(row.windows.len(), 4);
    assert_eq!(row.window_tokens, 16);
    assert_eq!(row.complete_tokens(), 7);
    assert_eq!(row.limit_reasons.get("alternatives"), Some(&1));
    let r = cfg::restricted_text(text, &row).unwrap();
    assert_eq!(r, "One short line.\nFourth ends.");
    let round: cfg::IndexRow = serde_json::from_str(&serde_json::to_string(&row).unwrap()).unwrap();
    assert_eq!(round, row);
}

#[test]
fn restricted_text_rejects_spans_inside_a_character() {
    let text = "caf\u{e9} au lait";
    let lines = window(0, 4, "native_complete", 1);
    let row = cfg::index_windows("synthetic", "00000", "digest", lines.as_bytes()).unwrap();
    assert!(cfg::restricted_text(text, &row).is_err());
}

#[test]
fn key_validation_admits_structure_and_rejects_lexical_identity() {
    let sym = r#"{"category":"NP","features":{"agreement":"plural"}}"#;
    assert!(cfg::validate_key(&format!("partial/symbol/{sym}")).is_ok());
    assert!(cfg::validate_key(&format!(
        r#"native_complete/production/[{sym},[{sym},{{"category":"PP","features":{{}}}}],0]"#
    ))
    .is_ok());
    assert!(cfg::validate_key(
        r#"fallback_complete/leaf/["inventory","NOUN",{"category":"Noun","features":{"count":"mass"}},null]"#
    )
    .is_ok());
    assert!(cfg::validate_key("availability/window/NativeComplete").is_ok());
    assert!(cfg::validate_key("availability/no_real_reading_token").is_ok());
    // An unknown leaf marker or pool, or a free-text feature, must never pass.
    assert!(cfg::validate_key(r#"partial/leaf/["lexeme","NOUN",{"category":"Noun","features":{}},null]"#).is_err());
    assert!(cfg::validate_key(r#"partial/leaf/["inventory","NOUN",{"category":"Noun","features":{}},"two words"]"#).is_err());
    assert!(cfg::validate_key(&format!("words/symbol/{sym}")).is_err());
    assert!(cfg::validate_key(r#"partial/symbol/{"category":"NP","features":{"lemma":"cat"}}"#).is_err());
    assert!(cfg::validate_key(r#"partial/symbol/{"category":"NP","features":{},"text":"x"}"#).is_err());
    assert!(cfg::validate_key("availability/window/Unknown").is_err());
}

#[test]
fn encoder_rows_require_full_finite_vectors() {
    let ok = json!({"id":"synthetic","text_sha256":common::sha_hex(b"synthetic text"),"status":"ok",
        "token_count":5,"truncation":false,
        "diagnostics":{"h_raw":vec![0.5f64; 768],"z_shared_f64":vec![-0.25f64; 768],"token_ids":[1,2,3]}});
    let p: encoder::Prediction = serde_json::from_value(ok).unwrap();
    let (h, z) = encoder::vectors(&p).unwrap().unwrap();
    assert_eq!((h.len(), z.len()), (768, 768));
    let short = json!({"id":"s","text_sha256":"x","status":"ok","token_count":5,
        "diagnostics":{"h_raw":vec![0.5f64; 767],"z_shared_f64":vec![0.0f64; 768]}});
    let p: encoder::Prediction = serde_json::from_value(short).unwrap();
    assert!(encoder::vectors(&p).is_err());
    let unsupported = json!({"id":"s","text_sha256":"x","status":"unsupported_input",
        "reason":"unsupported_length","token_count":4000,"prediction":null});
    let p: encoder::Prediction = serde_json::from_value(unsupported).unwrap();
    assert!(encoder::vectors(&p).unwrap().is_none());
}

#[test]
fn restricted_output_keeps_only_qualifying_t12_documents() {
    let dir = tempfile::tempdir().unwrap();
    let long_a = "Alpha sentence here. ".repeat(10);
    let docs = [
        ("synthetic-q", "t12:eval:query", long_a.clone(), 120u64),
        ("synthetic-short", "t12:train:reference", "Too short. Still short.".to_string(), 99),
        ("synthetic-t3", "t3:primary:profile", long_a.clone(), 500),
    ];
    let mut inputs = String::new();
    let mut selected = std::collections::BTreeMap::new();
    let mut index = std::collections::HashMap::new();
    for (id, use_, text, tokens) in &docs {
        inputs.push_str(&serde_json::to_string(&json!({"id":id,"text":text})).unwrap());
        inputs.push('\n');
        selected.insert(id.to_string(), cfg::SelectedDoc {
            id: id.to_string(), kind: "human_document".into(), domain: "synthetic".into(),
            text_sha256: common::sha_hex(text.as_bytes()), chunk: "c0000".into(), uses: vec![use_.to_string()],
        });
        let lines = [window(0, 10, "native_complete", *tokens as usize), window(11, text.len(), "partial", 3)].join("\n");
        index.insert(id.to_string(), cfg::index_windows(id, "00000", "d", lines.as_bytes()).unwrap());
    }
    let inputs_path = dir.path().join("inputs.jsonl");
    std::fs::write(&inputs_path, &inputs).unwrap();
    let summary = json!({"inputs":{"sha256":common::sha_hex(inputs.as_bytes())}});
    let out = dir.path().join("restricted");
    let record = cfg::write_restricted(&selected, &index, &inputs_path, &summary, &out).unwrap();
    assert_eq!(record["t12_documents_considered"], 2);
    assert_eq!(record["rows"], 1);
    let written = std::fs::read_to_string(out.join("chunks/r0000.jsonl")).unwrap();
    let row: serde_json::Value = serde_json::from_str(written.trim()).unwrap();
    assert_eq!(row["id"], "synthetic-q");
    assert_eq!(row["text"], &long_a[..10]);
}
