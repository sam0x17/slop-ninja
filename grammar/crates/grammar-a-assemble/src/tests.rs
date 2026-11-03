//! Tests on synthetic rows only (no corpus text).
use super::*;
use crate::bundle;
use crate::consolidate;
use std::fs;
use std::path::{Path, PathBuf};

fn o() -> Origin<'static> {
    Origin { input: 0, line: 1, bytes: b"{}" }
}

fn human(id: &str, split: &str, group: &str, text: &str) -> Value {
    json!({"id": id, "frozen_split": split, "origin": "human_only", "text": text,
        "text_sha256": sha_hex(text.as_bytes()), "collection": "plos_historical", "domain": "science",
        "source_group": group, "writer_key": format!("w:{group}")})
}

fn local(id: &str, split: &str, parent: &str, group: &str, text: &str, kind: &str, voice: Option<&str>) -> Value {
    let (origin, share, spans) = match kind {
        "mirror" => ("model_only", 1.0, json!([])),
        _ => ("mixed", 0.5, json!([[0, text.len() / 2]])),
    };
    json!({"schema": LOCAL_RECORD_SCHEMA, "id": id, "frozen_split": split, "split": split, "domain": "plos",
        "collection": "plos_historical", "source_group": group, "parent_id": parent, "origin": origin,
        "cohort": if kind == "mirror" {"primary_mirror"} else {"primary_mixed"}, "kind": kind,
        "population": "primary", "text": text, "text_sha256": sha_hex(text.as_bytes()), "share_target": share,
        "ai_byte_spans": spans,
        "generation": {"generator_key": "qwen36", "generator": {"family": "qwen3.6"}, "operation": null,
            "style": {"style_kind": "voice", "style_policy": "p", "voice_id": voice, "brief_id": null}}})
}

fn frontier(id: &str, split: &str, parent: &str, group: &str, text: &str, label: &str, voice: &str, held: bool) -> Value {
    json!({"schema": FRONTIER_RECORD_SCHEMA, "id": id, "frozen_split": split, "domain": "plos",
        "collection": "plos_historical", "source_group": group, "parent_id": parent, "origin": "mixed",
        "text": text, "text_sha256": sha_hex(text.as_bytes()), "share_target": 1.0,
        "share_evidence": {"final_byte_span": [0, text.len()]},
        "ai_attribution_label": label, "cohort_id": "hosted_frontier_v1",
        "cohort_weight_policy": {"policy_id": FRONTIER_POLICY},
        "training_admission": {"schema": "slop_ninja_a_hosted_training_admission_v1"},
        "generation": {"kind": "full", "voice": {"id": voice, "kind": "voice_prompt", "origin": "invented",
            "axes": {"persona_axis": "occupation"}, "held_out_style": held}, "voice_ladder": {"ladder_id": "L1"}}})
}

fn legacy(id: &str, group: &str, text: &str, origin: u64, ai: Option<&str>) -> Value {
    let share = match origin { 0 => 0.0, 1 => 1.0, _ => 0.4 };
    let spans = if origin == 2 { json!([[0, 3]]) } else { Value::Null };
    json!({"id": id, "frozen_split": "train", "text": text, "text_sha256": sha_hex(text.as_bytes()),
        "source_group": group, "purpose": "query",
        "roles": {"ai": {"identity": ai, "known": ai.is_some(), "present": ai.is_some()}},
        "targets": {"origin": origin, "share": share, "ai_byte_spans": spans}})
}

fn fz(id: &str, kind: &str, part: &str, cluster: &str, origin: &str, axis: &str, text: &str) -> styles::Frozen {
    styles::Frozen { schema: styles::FROZEN_SCHEMA.into(), id: id.into(), kind: kind.into(), partition: part.into(),
        text_key: styles::text_key(text), cluster: Some(cluster.into()), origin: Some(origin.into()), axis: Some(axis.into()),
        basis: "snapshot".into(), history: BTreeMap::new() }
}

fn frozen_rows() -> Vec<styles::Frozen> {
    vec![
        fz("v-train", "voice_prompt", "train", "vc-1", "invented", "occupation", "sound like a lighthouse keeper"),
        fz("v-dev", "voice_prompt", "dev_iter", "vc-2", "human_derived", "regional", "sound like Jane Austen"),
        fz("v-cal", "voice_prompt", "cal", "vc-3", "invented", "occupation", "sound like a court reporter"),
        fz("b-1", "brief", "train", "bc-1", "invented", "occupation", "Write as a ship's log."),
    ]
}

fn styles() -> FrozenIndex {
    FrozenIndex::from_rows(frozen_rows(), "test".into()).unwrap()
}

fn write_frozen(p: &Path) {
    let rows: Vec<Value> = frozen_rows().iter().map(|r| serde_json::to_value(r).unwrap()).collect();
    write_jsonl(p, &rows);
}

#[test]
fn human_rows_bind_text_and_split() {
    let h = human("h1", "train", "g1", "A human text.");
    let r = human_row(&h, "train", &HashMap::new(), &BTreeMap::new(), &o()).unwrap();
    assert_eq!((r.domain.as_str(), r.genre.as_deref(), r.cohort_id.as_str()), ("plos", Some("science"), "human:plos_historical"));
    assert!(r.ai_byte_spans.is_empty() && r.share == 0.0 && r.generator_label.is_none());
    let mut bad = h.clone();
    bad["text"] = json!("Edited.");
    assert!(human_row(&bad, "train", &HashMap::new(), &BTreeMap::new(), &o()).is_err());
    assert!(human_row(&h, "dev_iter", &HashMap::new(), &BTreeMap::new(), &o()).is_err());
    let mut d = HashMap::new();
    d.insert("h1".to_string(), "custom".to_string());
    assert_eq!(human_row(&h, "train", &d, &BTreeMap::new(), &o()).unwrap().domain, "custom");
}

#[test]
fn local_positive_spans_labels_and_style() {
    let s = styles();
    let m = local_row(&local("p1", "train", "h1", "g1", "Model text.", "mirror", Some("v-train")), &s, &o()).unwrap();
    assert_eq!(m.ai_byte_spans, vec![[0, 11]]);
    assert_eq!((m.origin.as_str(), m.share), ("model_only", 1.0));
    assert_eq!(m.generator_label.as_deref(), Some("qwen3.6"));
    assert_eq!(m.voice_cluster_id.as_deref(), Some("vc-1"));
    assert!(!m.held_out_style);
    let x = local_row(&local("p2", "dev_iter", "h2", "g2", "Mixed text here.", "span", Some("v-dev")), &s, &o()).unwrap();
    assert!(x.held_out_style);
    assert_eq!((x.style_partition.as_deref(), x.voice_origin.as_deref()), (Some("dev_iter"), Some("human_derived")));
    assert_eq!(x.operation, "span");
    let u = local_row(&local("p3", "train", "h1", "g1", "Other text.", "span", Some("v-none")), &s, &o()).unwrap();
    assert_eq!(u.style_partition.as_deref(), Some("unknown"));
    // A mixed row whose span is not on a character boundary is refused.
    let mut bad = local("p4", "train", "h1", "g1", "é mixed", "span", None);
    bad["ai_byte_spans"] = json!([[1, 3]]);
    assert!(local_row(&bad, &s, &o()).is_err());
}

#[test]
fn frontier_rows_need_v2_and_carry_canonical_labels() {
    let s = styles();
    let f = frontier("f1", "train", "h1", "g1", "Frontier text.", "claude-opus-5-5", "v-train", false);
    let r = frontier_row(&f, "claude-opus", &s, &o()).unwrap();
    assert_eq!(r.generator_label.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(r.generator_key.as_deref(), Some("claude-opus"));
    assert_eq!(r.style_axes["persona_axis"], "occupation");
    assert_eq!(r.style_axes["voice_ladder"], "L1");
    assert_eq!(r.ai_byte_spans, vec![[0, 14]]);
    let mut v1 = f.clone();
    v1["schema"] = json!("slop_ninja_frontier_positive_v1");
    assert!(frontier_row(&v1, "claude-opus", &s, &o()).is_err());
    let held = frontier("f2", "train", "h1", "g1", "Frontier two.", "requested:gpt-6-astra", "v-unlisted", true);
    let h = frontier_row(&held, "gpt-6-astra", &s, &o()).unwrap();
    assert!(h.held_out_style && style_unknown(&h));
    // A pilot voice with its own id but the text of an evaluation voice is held out by text.
    let mut pilot = frontier("f3", "train", "h1", "g1", "Frontier three.", "requested:gpt-6-astra", "v-train", false);
    pilot["generation"]["voice"]["text"] = json!("Sound like  JANE austen!");
    let p = frontier_row(&pilot, "gpt-6-astra", &s, &o()).unwrap();
    assert!(p.held_out_style);
    assert_eq!(p.style_text_match.as_deref(), Some("v-dev"));
}

#[test]
fn legacy_rows_map_origin_order_and_families() {
    let mut fam = BTreeMap::new();
    fam.insert("Qwen/Qwen3.6-35B-A3B".to_string(), "qwen3.6".to_string());
    let m = legacy_row(&legacy("l1", "encoder_stage3_work_v1:jack_london:pg310", "Model.", 1, Some("Qwen/Qwen3.6-35B-A3B:abc")), &fam, &o()).unwrap();
    assert_eq!((m.origin.as_str(), m.operation.as_str()), ("model_only", "mirror"));
    assert_eq!(m.generator_label.as_deref(), Some("qwen3.6"));
    assert_eq!(m.writer_key.as_deref(), Some("author:jack_london"));
    assert_eq!(m.id, "stage3:l1");
    assert_eq!(m.domain, "project_gutenberg");
    let w = legacy_row(&legacy("l5", "wikinews:31209", "News model.", 1, Some("Qwen/Qwen3.6-35B-A3B:abc")), &fam, &o()).unwrap();
    assert_eq!((w.domain.as_str(), w.writer_key.as_deref()), ("wikinews", None));
    let a = legacy_row(&legacy("l6", "author_work_v2:bill_nye:sparks", "Book model.", 1, Some("Qwen/Qwen3.6-35B-A3B:abc")), &fam, &o()).unwrap();
    assert_eq!((a.domain.as_str(), a.writer_key.as_deref()), ("project_gutenberg", Some("author:bill_nye")));
    let x = legacy_row(&legacy("l2", "encoder_stage3_work_v1:jack_london:pg310", "Mixed one.", 2, Some("Qwen/Qwen3.6-35B-A3B:abc")), &fam, &o()).unwrap();
    assert_eq!((x.origin.as_str(), x.operation.as_str(), x.ai_byte_spans.clone()), ("mixed", "span", vec![[0, 3]]));
    let h = legacy_row(&legacy("l3", "encoder_stage3_work_v1:jack_london:pg310", "Human.", 0, None), &fam, &o()).unwrap();
    assert!(h.generator_label.is_none() && h.share == 0.0);
    assert!(legacy_row(&legacy("l4", "g", "Other.", 1, Some("Unknown/Model:x")), &fam, &o()).is_err());
    let mut k = EvalKeys::default();
    k.add_manifest_row("test", &json!({"family_key": "f", "group_key": "author:jack_griffith_london",
        "source_unit": {"pg_id": 999, "writer_key": "author:jack_griffith_london"}}));
    assert_eq!(k.collision(&m).unwrap().1, "pg_author_first_last_name");
    let mut k2 = EvalKeys::default();
    k2.add_manifest_row("cal", &json!({"family_key": "f", "source_unit": {"pg_id": 310}}));
    assert_eq!(k2.collision(&m).unwrap(), ("cal".to_string(), "pg_id"));
}

fn train_rows() -> (Vec<Row>, HashMap<String, Parent>) {
    let s = styles();
    let hs = [human("h1", "train", "g1", "Human one."), human("h2", "train", "g2", "Human two.")];
    let mut rows = Vec::new();
    let mut parents = HashMap::new();
    for h in &hs {
        let r = human_row(h, "train", &HashMap::new(), &BTreeMap::new(), &o()).unwrap();
        parents.insert(r.id.clone(), Parent::of(&r));
        rows.push(r);
    }
    rows.push(local_row(&local("p1", "train", "h1", "g1", "Model one.", "mirror", Some("v-train")), &s, &o()).unwrap());
    rows.push(local_row(&local("p2", "train", "h1", "g1", "Model two.", "mirror", Some("v-dev")), &s, &o()).unwrap());
    rows.push(local_row(&local("p3", "train", "hx", "g9", "Model three.", "mirror", None), &s, &o()).unwrap());
    rows.push(local_row(&local("p4", "train", "h2", "g2", "Model one.", "mirror", None), &s, &o()).unwrap());
    rows.push(frontier_row(&frontier("f1", "train", "h2", "g2", "Frontier A.", "claude-opus-5-5", "v-train", false), "o", &s, &o()).unwrap());
    rows.push(frontier_row(&frontier("f2", "train", "h2", "g2", "Frontier B.", "requested:gpt-6-astra", "v-train", false), "a", &s, &o()).unwrap());
    rows.push(frontier_row(&frontier("f3", "train", "h1", "g1", "Frontier C.", "requested:gpt-6-astra", "v-train", false), "a", &s, &o()).unwrap());
    rows.push(frontier_row(&frontier("f4", "train", "h1", "g1", "Frontier D.", "requested:gpt-6-astra", "v-train", false), "a", &s, &o()).unwrap());
    (rows, parents)
}

#[test]
fn train_selection_excludes_held_out_style_orphans_and_duplicates() {
    let (rows, parents) = train_rows();
    let (kept, ex) = select("train", rows, &parents, &EvalKeys::default(), &HashMap::new(), &HashSet::new(), &HashMap::new());
    let reason = |id: &str| ex.iter().find(|e| e.id == id).map(|e| e.reason.as_str());
    assert_eq!(reason("p2"), Some("held_out_style_in_train"));
    assert_eq!(reason("p3"), Some("parent_not_found"));
    assert_eq!(reason("p4"), Some("duplicate_text"));
    assert_eq!(kept.len(), 7);
    assert!(kept.windows(2).all(|w| w[0].id < w[1].id));
    // Positives inherit the parent's writer and genre.
    let f1 = kept.iter().find(|r| r.id == "f1").unwrap();
    assert_eq!((f1.writer_key.as_deref(), f1.genre.as_deref()), (Some("w:g2"), Some("science")));
}

#[test]
fn train_rows_in_evaluation_groups_or_texts_are_dropped_and_evaluation_keeps_held_out_style() {
    let (rows, parents) = train_rows();
    let mut k = EvalKeys::default();
    // A reservation group row (family list) closes its families like a manifest row.
    k.add_manifest_row("reserved_unassigned", &json!({"family_keys": ["g2"], "group_key": "w:none"}));
    assert_eq!(k.families["g2"], "reserved_unassigned");
    let mut texts = HashMap::new();
    texts.insert(sha_hex(b"Frontier C."), "cal".to_string());
    let (kept, ex) = select("train", rows, &parents, &k, &texts, &HashSet::new(), &HashMap::new());
    assert!(kept.iter().all(|r| r.source_group != "g2"));
    assert!(ex.iter().any(|e| e.id == "f3" && e.reason == "text_in_evaluation_split"));
    assert_eq!(ex.iter().filter(|e| e.reason == "train_row_in_evaluation_group").count(), 4);
    // Evaluation: held-out-style rows stay; a row of a Train group is dropped.
    let s = styles();
    let h = human_row(&human("e1", "dev_iter", "ge", "Eval human."), "dev_iter", &HashMap::new(), &BTreeMap::new(), &o()).unwrap();
    let mut ps = HashMap::new();
    ps.insert("e1".to_string(), Parent::of(&h));
    let p = local_row(&local("ep", "dev_iter", "e1", "ge", "Eval model.", "mirror", Some("v-dev")), &s, &o()).unwrap();
    let wrong = local_row(&local("eq", "dev_iter", "h1", "g1", "Wrong split.", "mirror", None), &s, &o()).unwrap();
    let mut tp = parents.clone();
    tp.extend(ps);
    let mut tf = HashSet::new();
    tf.insert("g1".to_string());
    let (kept, ex) = select("dev_iter", vec![h, p, wrong], &tp, &EvalKeys::default(), &HashMap::new(), &tf, &HashMap::new());
    assert!(kept.iter().any(|r| r.id == "ep" && r.held_out_style));
    assert_eq!(ex[0].reason, "parent_in_other_split");
}

fn row(id: &str, domain: &str, group: &str, origin: &str, label: Option<&str>, cohort: &str, kind: SourceKind) -> Row {
    let text = format!("text {id}");
    let mut r = human_row(&human(id, "train", group, &text), "train", &HashMap::new(), &BTreeMap::new(), &o()).unwrap();
    r.domain = domain.into();
    r.origin = origin.into();
    r.generator_label = label.map(String::from);
    r.cohort_id = cohort.into();
    r.source_kind = kind;
    r.population = if kind == SourceKind::Frontier { "frontier".into() } else { "primary".into() };
    r
}

fn mass(rows: &[Row], f: impl Fn(&Row) -> bool) -> f64 {
    let t: f64 = rows.iter().map(|r| r.weight).sum();
    rows.iter().filter(|r| f(r)).map(|r| r.weight).sum::<f64>() / t
}

#[test]
fn weights_give_declared_origin_shares_per_domain_over_pooled_groups() {
    use SourceKind::*;
    // Domain A: 4 human-only groups, one group with a mirror; domain B: 2 groups, humans only.
    let mut rows = vec![
        row("a1", "A", "g1", "human_only", None, "human:c", Human),
        row("a2", "A", "g2", "human_only", None, "human:c", Human),
        row("a3", "A", "g3", "human_only", None, "human:c", Human),
        row("a4", "A", "g4", "human_only", None, "human:c", Human),
        row("a5", "A", "g4", "human_only", None, "human:c", Human),
        row("m1", "A", "g1", "model_only", Some("qwen3.6"), "local:primary_mirror", LocalPositive),
        row("x1", "A", "g2", "mixed", Some("qwen3.6"), "local:primary_mixed", LocalPositive),
        row("x2", "A", "g2", "mixed", Some("qwen3.6"), "local:primary_mixed", LocalPositive),
        row("x3", "A", "g3", "mixed", Some("phi4"), "local:primary_mixed", LocalPositive),
        row("b1", "B", "g5", "human_only", None, "human:c", Human),
        row("b2", "B", "g6", "human_only", None, "human:c", Human),
    ];
    let w = assign_weights(&mut rows, true, &CohortWeights::default(), &Weighting::default()).unwrap();
    // Domain masses 4/6 and 2/6; A splits its mass equally over its three cells; B is all human.
    let a = 4.0 / 6.0;
    assert!((mass(&rows, |r| r.domain == "A" && r.origin == "human_only") - a / 3.0).abs() < 1e-12);
    assert!((mass(&rows, |r| r.origin == "model_only") - a / 3.0).abs() < 1e-12);
    assert!((mass(&rows, |r| r.origin == "mixed") - a / 3.0).abs() < 1e-12);
    assert!((mass(&rows, |r| r.domain == "B") - 2.0 / 6.0).abs() < 1e-12);
    // Inside the human cell of A each of 4 groups gets a quarter; g4's two documents an eighth each.
    let ha = a / 3.0;
    assert!((mass(&rows, |r| r.id == "a5") - ha / 8.0).abs() < 1e-12);
    // Inside the mixed cell: groups g2 and g3 get half each; x1, x2 a quarter.
    assert!((mass(&rows, |r| r.id == "x3") - ha / 2.0).abs() < 1e-12);
    assert!((mass(&rows, |r| r.id == "x1") - ha / 4.0).abs() < 1e-12);
    let mean: f64 = rows.iter().map(|r| r.weight).sum::<f64>() / rows.len() as f64;
    assert!((mean - 1.0).abs() < 1e-12);
    assert!((w["effective_mass"]["by_origin"]["human_only"]["mass_share"].as_f64().unwrap() - (a / 3.0 + 2.0 / 6.0)).abs() < 1e-12);
    // A declared human share of 1/2 splits the rest equally.
    let wt = Weighting { human_share: 0.5, ..Default::default() };
    assign_weights(&mut rows, true, &CohortWeights::default(), &wt).unwrap();
    assert!((mass(&rows, |r| r.domain == "A" && r.origin == "human_only") - a / 2.0).abs() < 1e-12);
    assert!((mass(&rows, |r| r.origin == "mixed") - a / 4.0).abs() < 1e-12);
    // Cohort weights move mass inside a cell, never between cells.
    let mut cw = CohortWeights::default();
    cw.cohorts.insert("local:primary_mixed".into(), 3.0);
    assign_weights(&mut rows, true, &cw, &Weighting::default()).unwrap();
    assert!((mass(&rows, |r| r.origin == "mixed") - a / 3.0).abs() < 1e-12);
    assert_eq!(rows.iter().find(|r| r.id == "x1").unwrap().cohort_weight, 3.0);
    // A removed cohort hands its cell's share to the domain's other cells.
    cw.cohorts.insert("local:primary_mixed".into(), 0.0);
    assign_weights(&mut rows, true, &cw, &Weighting::default()).unwrap();
    assert!(rows.iter().filter(|r| r.origin == "mixed").all(|r| r.weight == 0.0));
    assert!((mass(&rows, |r| r.domain == "A" && r.origin == "human_only") - a / 2.0).abs() < 1e-12);
    assert!(Weighting { human_share: 0.5, mixed_share: Some(0.6), ..Default::default() }.shares().is_err());
}

#[test]
fn frontier_labels_get_equal_final_weight_and_cells_keep_their_mass() {
    use SourceKind::*;
    let f = "hosted_frontier_v1";
    let mut rows = vec![
        row("h1", "A", "g1", "human_only", None, "human:c", Human),
        row("h2", "A", "g2", "human_only", None, "human:c", Human),
        row("h3", "A", "g3", "human_only", None, "human:c", Human),
        row("l1", "A", "g1", "mixed", Some("qwen3.6"), "local:x", LocalPositive),
        // Astra: 3 records over 3 groups; Opus: 1 record; one mixed and one model-only cell each.
        row("fa1", "A", "g1", "mixed", Some("requested:gpt-6-astra"), f, Frontier),
        row("fa2", "A", "g2", "mixed", Some("requested:gpt-6-astra"), f, Frontier),
        row("fa3", "A", "g3", "model_only", Some("requested:gpt-6-astra"), f, Frontier),
        row("fo1", "A", "g2", "model_only", Some("claude-opus-5-5"), f, Frontier),
        row("fs1", "A", "g3", "mixed", Some("claude-sonnet-5-5"), f, Frontier),
    ];
    let w = assign_weights(&mut rows, true, &CohortWeights::default(), &Weighting::default()).unwrap();
    fn lab(rows: &[Row], l: &str) -> f64 {
        mass(rows, |r| r.generator_label.as_deref() == Some(l))
    }
    let (a, o, s) = (lab(&rows, "requested:gpt-6-astra"), lab(&rows, "claude-opus-5-5"), lab(&rows, "claude-sonnet-5-5"));
    assert!((a - o).abs() < 1e-9 && (a - s).abs() < 1e-9, "{a} {o} {s}");
    for origin in ORIGINS {
        assert!((mass(&rows, |r| r.origin == origin) - 1.0 / 3.0).abs() < 1e-8, "{origin}");
    }
    assert!(w["label_fit"]["cohorts"][f]["max_relative_deviation_from_label_target"].as_f64().unwrap() < 1e-9);
    assert!(w["label_fit"]["max_relative_cell_deviation"].as_f64().unwrap() < 1e-8);
    // Declared relative label weights are honoured on the final weights.
    let mut cw = CohortWeights::default();
    cw.labels.entry(f.into()).or_default().insert("claude-opus-5-5".into(), 2.0);
    assign_weights(&mut rows, true, &cw, &Weighting::default()).unwrap();
    let (a, o) = (lab(&rows, "requested:gpt-6-astra"), lab(&rows, "claude-opus-5-5"));
    assert!((o / a - 2.0).abs() < 1e-8);
    // Evaluation splits: balanced within each population, no cohort factor; frontier
    // rows are primary documents and share the primary pool.
    let w = assign_weights(&mut rows, false, &cw, &Weighting::default()).unwrap();
    assert!(rows.iter().all(|r| r.cohort_weight == 1.0 && r.weight == r.balanced_weight));
    assert_eq!(w["populations"]["primary"], 9);
    assert!(w["populations"].get("frontier").is_none() && w["frontier_pooled_with_primary"].is_string());
}

fn write_jsonl(p: &Path, rows: &[Value]) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut b = Vec::new();
    for r in rows {
        serde_json::to_writer(&mut b, r).unwrap();
        b.push(b'\n');
    }
    fs::write(p, b).unwrap();
}

#[test]
fn build_writes_bundles_seals_evaluation_splits_and_reruns_identically() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    write_jsonl(&d.join("train.jsonl"), &[human("h1", "train", "g1", "Human one."), human("h2", "train", "g2", "Human two.")]);
    write_jsonl(&d.join("cal.jsonl"), &[human("c1", "cal", "gc", "Cal human.")]);
    write_jsonl(&d.join("train-manifest.jsonl"), &[json!({"id": "h1", "domain": "plos"}), json!({"id": "h2", "domain": "plos"})]);
    write_jsonl(&d.join("mining-manifest.jsonl"), &[json!({"id": "m1", "split": "train_mining", "domain": "plos", "source_group": "gm", "writer_key": "w:gm"})]);
    write_jsonl(&d.join("test-manifest.jsonl"), &[json!({"family_key": "gt", "group_key": "w:gt", "split": "test"})]);
    write_frozen(&d.join("frozen.jsonl"));
    write_jsonl(&d.join("local.jsonl"), &[
        local("p1", "train", "h1", "g1", "Model one.", "mirror", Some("v-train")),
        local("p2", "train", "m1", "gm", "Mining child.", "mirror", Some("v-train")),
        local("p3", "cal", "c1", "gc", "Cal model.", "mirror", Some("v-cal")),
        local("p4", "dev_select", "x", "gx", "Unbuilt split.", "mirror", None),
    ]);
    write_jsonl(&d.join("fx/claude-opus/records.jsonl"), &[frontier("f1", "train", "h2", "g2", "Frontier A.", "claude-opus-5-5", "v-train", false)]);
    write_jsonl(&d.join("legacy.jsonl"), &[legacy("l1", "encoder_stage3_work_v1:a_b:pg1", "Legacy model.", 1, Some("Qwen/Qwen3.6-35B-A3B:x"))]);
    let cfg = json!({"schema": bundle::CONFIG_SCHEMA,
        "splits": {"train": {"humans": [d.join("train.jsonl")], "domain_manifests": [d.join("train-manifest.jsonl")]},
                   "cal": {"humans": [d.join("cal.jsonl")]}},
        "sealed": ["cal"], "train_side_manifests": [d.join("mining-manifest.jsonl")],
        "local_positives": [d.join("local.jsonl")], "frontier_exports": [d.join("fx")],
        "legacy_train": d.join("legacy.jsonl"), "legacy_families": {"Qwen/Qwen3.6-35B-A3B": "qwen3.6"},
        "frozen_partitions": d.join("frozen.jsonl"), "evaluation_manifests": {"test": [d.join("test-manifest.jsonl")]},
        "cohort_weights": {"default": 1.0, "cohorts": {"stage3_legacy_train": 0.5}}});
    fs::write(d.join("config.json"), serde_json::to_vec(&cfg).unwrap()).unwrap();
    let code = json!({"assembler_sha256": "test"});
    let out = d.join("bundles");
    let b1 = bundle::build(&d.join("config.json"), &out, &code).unwrap();
    assert_eq!(b1["unchanged"], false);
    assert_eq!(b1["splits"]["train"]["rows"], 6);
    assert_eq!(b1["splits"]["cal"]["rows"], 2);
    let dir = Path::new(b1["dir"].as_str().unwrap());
    assert!(dir.join("cal/SEALED.json").is_file() && !dir.join("train/SEALED.json").exists());
    assert!(fs::metadata(dir.join("cal/rows.jsonl")).unwrap().permissions().readonly());
    assert!(!fs::metadata(dir.join("train/rows.jsonl")).unwrap().permissions().readonly());
    let m: Value = serde_json::from_slice(&fs::read(dir.join("train/MANIFEST.json")).unwrap()).unwrap();
    assert_eq!(m["counts_domain_origin_generator"]["plos"]["model_only"]["qwen3.6"], 2);
    assert_eq!(m["counts_domain_origin_generator"]["project_gutenberg"]["model_only"]["qwen3.6"], 1);
    assert_eq!(m["checks"]["kept_rows_colliding_with_other_side"], 0);
    let w: Value = serde_json::from_slice(&fs::read(dir.join("train/weights.json")).unwrap()).unwrap();
    assert_eq!(w["cohorts"]["stage3_legacy_train"]["cohort_weight"], 0.5);
    assert_eq!(w["cohorts"]["stage3_legacy_train"]["default"], false);
    let cal: Vec<Value> = fs::read_to_string(dir.join("cal/rows.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(cal.iter().any(|r| r["id"] == "p3" && r["held_out_style"] == true));
    let rows: Vec<Value> = fs::read_to_string(dir.join("train/rows.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let p2 = rows.iter().find(|r| r["id"] == "p2").unwrap();
    assert_eq!(p2["writer_key"], "w:gm");
    for r in &rows {
        assert_eq!(r["text_sha256"], sha_hex(r["text"].as_str().unwrap().as_bytes()));
        assert!(r["provenance_sha256"].as_str().unwrap().len() == 64);
    }
    let b: Value = serde_json::from_slice(&fs::read(dir.join("BUILD.json")).unwrap()).unwrap();
    assert_eq!(b["ignored_rows_of_unbuilt_splits"]["LocalPositive:dev_select"], 1);
    // Unchanged inputs: same build, nothing rewritten.
    let b2 = bundle::build(&d.join("config.json"), &out, &code).unwrap();
    assert_eq!((b2["build_id"].clone(), b2["unchanged"].clone()), (b1["build_id"].clone(), json!(true)));
    // New positives: a new build, and CURRENT points at it.
    let mut more = fs::read(d.join("local.jsonl")).unwrap();
    serde_json::to_writer(&mut more, &local("p5", "train", "h2", "g2", "Model five.", "mirror", None)).unwrap();
    more.push(b'\n');
    fs::write(d.join("local.jsonl"), more).unwrap();
    let b3 = bundle::build(&d.join("config.json"), &out, &code).unwrap();
    assert_ne!(b3["build_id"], b1["build_id"]);
    assert_eq!(b3["splits"]["train"]["rows"], 7);
    let cur: Value = serde_json::from_slice(&fs::read(out.join("CURRENT.json")).unwrap()).unwrap();
    assert_eq!(cur["build_id"], b3["build_id"]);
    // Test is never assembled.
    let mut bad = cfg.clone();
    bad["splits"]["test"] = json!({"humans": []});
    fs::write(d.join("bad.json"), serde_json::to_vec(&bad).unwrap()).unwrap();
    assert!(bundle::build(&d.join("bad.json"), &out, &code).is_err());
}

#[test]
fn eval_human_merge_keeps_lines_and_refuses_duplicates() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    for (p, ids) in [("a", ["a1", "a2"]), ("b", ["b1", "b2"])] {
        for s in consolidate::EVAL_SPLIT_DIRS {
            let docs: Vec<Value> = ids.iter().map(|i| json!({"id": format!("{s}-{i}"), "text_sha256": format!("{s}-{i}")})).collect();
            write_jsonl(&d.join(p).join(s).join("documents.jsonl"), &docs);
            write_jsonl(&d.join(p).join(s).join("metadata.jsonl"), &[json!({"m": p})]);
            write_jsonl(&d.join(p).join(s).join("skips.jsonl"), &[]);
            fs::write(d.join(p).join(s).join("summary.json"), b"{}").unwrap();
        }
        fs::write(d.join(p).join("ACQUISITION.json"), b"{}").unwrap();
    }
    let r = consolidate::merge_eval_humans(&[&d.join("a"), &d.join("b")], &d.join("m")).unwrap();
    assert_eq!(r["splits"]["cal"]["documents"], 4);
    let merged = fs::read_to_string(d.join("m/cal/documents.jsonl")).unwrap();
    assert_eq!(merged.lines().count(), 4);
    for p in ["a", "b"] {
        let v = consolidate::verify_lines(&d.join(p).join("cal/documents.jsonl"), &d.join("m/cal/documents.jsonl")).unwrap();
        assert_eq!(v["complete"], true);
    }
    assert!(consolidate::merge_eval_humans(&[&d.join("a"), &d.join("a")], &d.join("m2")).is_err());
    let v = consolidate::verify_lines(&d.join("m/cal/documents.jsonl"), &d.join("a/cal/documents.jsonl")).unwrap();
    assert_eq!((v["complete"].clone(), v["base_lines_found"].clone()), (json!(false), json!(2)));
}

#[test]
fn frontier_migration_check_compares_with_the_sidecar() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let text = "Frontier text.";
    let v1 = json!({"schema": "slop_ninja_frontier_positive_v1", "id": "f1", "text": text, "text_sha256": sha_hex(text.as_bytes()),
        "ai_identity": "codex-cli:gpt-6-astra", "generation": {"kind": "full"}});
    write_jsonl(&d.join("old/gpt-6-astra/records.jsonl"), &[v1.clone()]);
    write_jsonl(&d.join("old/admission-labels.jsonl"), &[json!({"record_id": "f1", "text_sha256": sha_hex(text.as_bytes()),
        "cohort_id": "hosted_frontier_v1", "cohort_weight_policy": FRONTIER_POLICY, "training_admission": {"x": 1},
        "identity": {"attribution_label": "requested:gpt-6-astra", "identity_basis": "requested_selector", "recorded_ai_identity": "codex-cli:gpt-6-astra"}})]);
    let mut v2 = v1.clone();
    v2["schema"] = json!(FRONTIER_RECORD_SCHEMA);
    v2["ai_attribution_label"] = json!("requested:gpt-6-astra");
    v2["ai_identity_basis"] = json!("requested_selector");
    v2["training_admission"] = json!({"x": 1});
    v2["cohort_id"] = json!("hosted_frontier_v1");
    v2["cohort_weight_policy"] = json!({"policy_id": FRONTIER_POLICY});
    v2["ai_identity_detail"] = json!({"migrated_from": {"ai_identity": "codex-cli:gpt-6-astra"}});
    let extra = json!({"schema": FRONTIER_RECORD_SCHEMA, "id": "f2"});
    write_jsonl(&d.join("new/gpt-6-astra/records.jsonl"), &[v2.clone(), extra]);
    let r = consolidate::verify_frontier(&d.join("old"), &d.join("new")).unwrap();
    assert_eq!((r["complete"].clone(), r["labels"]["gpt-6-astra"]["collected_since"].clone()), (json!(true), json!(1)));
    let mut wrong = v2;
    wrong["ai_attribution_label"] = json!("requested:gpt-6-sol");
    write_jsonl(&d.join("new/gpt-6-astra/records.jsonl"), &[wrong]);
    let r = consolidate::verify_frontier(&d.join("old"), &d.join("new")).unwrap();
    assert_eq!(r["complete"], false);
}

#[test]
fn evaluation_closure_uses_reservation_contributor_keys_not_document_writers() {
    // A Federal Register-like case: documents carry a department writer key,
    // the reservation's contributor key is the office.
    let train = human_row(&human("t1", "train", "doc-t", "Train notice."), "train", &HashMap::new(), &BTreeMap::new(), &o()).unwrap();
    let mut eval = human_row(&human("e1", "cal", "doc-e", "Cal notice."), "cal", &HashMap::new(), &BTreeMap::new(), &o()).unwrap();
    eval.writer_key = train.writer_key.clone();
    let mut k = EvalKeys::default();
    k.add_human(&eval);
    assert!(k.collision(&train).is_none());
    k.add_manifest_row("cal", &json!({"family_key": "doc-e", "group_key": "w:doc-t"}));
    assert_eq!(k.collision(&train).unwrap(), ("cal".to_string(), "writer_key"));
    // A diagnostic-slice document manifest closes its source groups.
    let mut k = EvalKeys::default();
    k.add_manifest_row("diagnostic", &json!({"id": "x", "source_group": "doc-t"}));
    assert_eq!(k.collision(&train).unwrap(), ("diagnostic".to_string(), "source_group"));
}

#[test]
fn positives_take_the_parent_split_domain() {
    let s = styles();
    let h = human_row(&human("h1", "train", "g1", "Human one."), "train", &HashMap::new(), &BTreeMap::new(), &o()).unwrap();
    let mut ps = HashMap::new();
    ps.insert("h1".to_string(), Parent::of(&h));
    let mut rec = local("p1", "train", "h1", "g1", "Model one.", "mirror", None);
    rec["domain"] = json!("plos_blog");
    let p = local_row(&rec, &s, &o()).unwrap();
    let (kept, ex) = select("train", vec![h, p], &ps, &EvalKeys::default(), &HashMap::new(), &HashSet::new(), &HashMap::new());
    assert!(ex.is_empty());
    let p = kept.iter().find(|r| r.id == "p1").unwrap();
    assert_eq!((p.domain.as_str(), p.record_domain.as_deref()), ("plos", Some("plos_blog")));
}

#[test]
fn trainer_bundle_projects_the_trainer_fields_weights_and_selection() {
    use crate::trainer;
    use std::collections::BTreeSet;
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let groups = ["g1", "g2", "g3", "g4", "g5", "g6"];
    let humans: Vec<Value> = groups.iter().map(|g| human(&format!("h-{g}"), "train", g, &format!("Human of {g}."))).collect();
    write_jsonl(&d.join("train.jsonl"), &humans);
    write_jsonl(&d.join("dev.jsonl"), &[human("d1", "dev_iter", "gd", "Dev human.")]);
    write_frozen(&d.join("frozen.jsonl"));
    let locals: Vec<Value> = groups.iter().map(|g| local(&format!("p-{g}"), "train", &format!("h-{g}"), g, &format!("Model of {g}."), "mirror", Some("v-train"))).collect();
    write_jsonl(&d.join("local.jsonl"), &locals);
    let mut fr: Vec<Value> = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        let label = if i % 2 == 0 { "claude-opus-5-5" } else { "requested:gpt-6-astra" };
        let mut f = frontier(&format!("f-{g}"), "train", &format!("h-{g}"), g, &format!("Frontier of {g}."), label, "v-train", false);
        f["generation"]["voice"]["axes"]["inventor"] = json!("mistral7b");
        if i == 1 {
            f["generation"]["voice"]["id"] = json!("b-1");
            f["generation"]["voice"]["kind"] = json!("brief");
        }
        fr.push(f);
    }
    write_jsonl(&d.join("fx/all/records.jsonl"), &fr);
    let cfg = json!({"schema": bundle::CONFIG_SCHEMA,
        "splits": {"train": {"humans": [d.join("train.jsonl")]}, "dev_iter": {"humans": [d.join("dev.jsonl")]}},
        "local_positives": [d.join("local.jsonl")], "frontier_exports": [d.join("fx")],
        "frozen_partitions": d.join("frozen.jsonl")});
    fs::write(d.join("config.json"), serde_json::to_vec(&cfg).unwrap()).unwrap();
    let b = bundle::build(&d.join("config.json"), &d.join("bundles"), &json!({"assembler_sha256": "test"})).unwrap();
    assert_eq!(b["splits"]["train"]["rows"], 18);
    let build = Path::new(b["dir"].as_str().unwrap());
    let read = |p: &Path| -> Vec<Value> {
        fs::read_to_string(p).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    };
    let code = json!({"assembler_sha256": "test"});
    // Plain projection (declared cohort factors).
    let out = d.join("tb");
    let sel = trainer::Selection { weighting: trainer::TrainerWeighting::CohortFactor, ..Default::default() };
    trainer::trainer_bundle(build, &["train".into(), "dev-iter".into()], &sel, &out, &code).unwrap();
    let rows = read(&out.join("rows.jsonl"));
    assert_eq!(rows.len(), 19);
    let fields: BTreeSet<&str> = trainer::TRAINER_FIELDS.into_iter().collect();
    for r in &rows {
        assert_eq!(r.as_object().unwrap().keys().map(String::as_str).collect::<BTreeSet<_>>(), fields);
        if r["origin"] == "human_only" {
            assert!(r["operation"].is_null() && r["generator_label"].is_null());
        }
        assert!(r["voice_cluster_id"].is_null() || r["voice_id"].is_string());
    }
    assert_eq!(rows.last().unwrap()["split"], "dev_iter");
    let brief = rows.iter().find(|r| r["id"] == "f-g2").unwrap();
    assert_eq!((brief["voice_id"].clone(), brief["voice_cluster_id"].clone()), (json!("b-1"), json!("bc-1")));
    assert_eq!(brief["style_axes"], json!(["occupation"]));
    let w: Value = serde_json::from_slice(&fs::read(out.join("weights.json")).unwrap()).unwrap();
    assert_eq!(w["base_weighting"], "family_origin_balanced");
    let f = &w["cohorts"]["hosted_frontier_v1"];
    assert_eq!((f["policy"].clone(), f["default"].clone()), (json!(FRONTIER_POLICY), json!(true)));
    assert_eq!(f["labels"]["claude-opus-5-5"], json!({"records": 3, "weight": 1.0}));
    assert_eq!(w["cohorts"]["human:plos_historical"]["labels"]["human"]["records"], 6);
    let m: Value = serde_json::from_slice(&fs::read(out.join("MANIFEST.json")).unwrap()).unwrap();
    assert_eq!(m["files"]["rows.jsonl"], sha_hex(&fs::read(out.join("rows.jsonl")).unwrap()));
    assert_eq!(m["smoke_only"], false);
    assert!(trainer::trainer_bundle(build, &["train".into()], &sel, &out, &code).is_err(), "written once");
    // Final-weight contract: each Train row carries its build weight.
    let outf = d.join("tbf");
    trainer::trainer_bundle(build, &["train".into()], &trainer::Selection::default(), &outf, &code).unwrap();
    let wf: Value = serde_json::from_slice(&fs::read(outf.join("weights.json")).unwrap()).unwrap();
    assert_eq!(wf["base_weighting"], "assembler_row_weight_v1");
    let built: HashMap<String, f64> = read(&build.join("train/rows.jsonl")).iter().map(|r| (r["id"].as_str().unwrap().to_string(), r["weight"].as_f64().unwrap())).collect();
    for r in read(&outf.join("rows.jsonl")) {
        assert_eq!(r["cohort_weight"].as_f64().unwrap(), built[r["id"].as_str().unwrap()]);
    }
    let fl = &wf["cohorts"]["hosted_frontier_v1"]["labels"];
    let (a, o) = (fl["requested:gpt-6-astra"]["weight_sum"].as_f64().unwrap(), fl["claude-opus-5-5"]["weight_sum"].as_f64().unwrap());
    assert!((a - o).abs() < 1e-9, "equal final weight per frontier label");
    // Smoke carve and caps.
    let mut sel = trainer::Selection { seed: "7".into(), ..Default::default() };
    sel.parse_carve("1/2").unwrap();
    sel.parse_take("train:frontier=2").unwrap();
    sel.parse_take("dev_iter:carved_human=1").unwrap();
    let out2 = d.join("tb2");
    trainer::trainer_bundle(build, &["train".into(), "dev_iter".into()], &sel, &out2, &code).unwrap();
    let rows = read(&out2.join("rows.jsonl"));
    let train_groups: BTreeSet<&str> = rows.iter().filter(|r| r["split"] == "train").map(|r| r["source_group"].as_str().unwrap()).collect();
    let dev: Vec<&Value> = rows.iter().filter(|r| r["split"] == "dev_iter").collect();
    assert!(dev.len() > 1 && !train_groups.is_empty());
    for r in &dev {
        assert!(!train_groups.contains(r["source_group"].as_str().unwrap()));
        assert_eq!(r["cohort_weight"], 1.0);
    }
    assert!(rows.iter().filter(|r| r["split"] == "train" && r["cohort_id"] == "hosted_frontier_v1").count() <= 2);
    // Dev-iter's own human stays; at most one carved human joins it.
    assert!(dev.iter().any(|r| r["id"] == "d1"));
    assert!(dev.iter().filter(|r| r["origin"] == "human_only" && r["id"] != "d1").count() <= 1);
    let m: Value = serde_json::from_slice(&fs::read(out2.join("MANIFEST.json")).unwrap()).unwrap();
    assert_eq!(m["smoke_only"], true);
    // Test and unknown kinds are refused.
    assert!(sel.parse_take("test:human=1").is_err() && sel.parse_take("train:other=1").is_err());
}

#[test]
fn sealed_splits_open_only_once_for_their_protocol_use() {
    use crate::trainer;
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    write_jsonl(&d.join("train.jsonl"), &[human("h1", "train", "g1", "Human one.")]);
    write_jsonl(&d.join("cal.jsonl"), &[human("c1", "cal", "gc", "Cal human.")]);
    write_jsonl(&d.join("ds.jsonl"), &[human("s1", "dev_select", "gs", "Select human.")]);
    write_frozen(&d.join("frozen.jsonl"));
    let cfg = json!({"schema": bundle::CONFIG_SCHEMA, "frozen_partitions": d.join("frozen.jsonl"),
        "splits": {"train": {"humans": [d.join("train.jsonl")]}, "cal": {"humans": [d.join("cal.jsonl")]},
                   "dev_select": {"humans": [d.join("ds.jsonl")]}}, "sealed": ["cal", "dev_select"]});
    fs::write(d.join("config.json"), serde_json::to_vec(&cfg).unwrap()).unwrap();
    let code = json!({"assembler_sha256": "test"});
    let b = bundle::build(&d.join("config.json"), &d.join("bundles"), &code).unwrap();
    let build = Path::new(b["dir"].as_str().unwrap());
    let sel = trainer::Selection::default();
    assert!(trainer::trainer_bundle(build, &["train".into(), "cal".into()], &sel, &d.join("a"), &code).is_err());
    let wrong = trainer::Selection { unseal_for: vec!["final_selection".into()], ..Default::default() };
    assert!(trainer::trainer_bundle(build, &["train".into(), "cal".into()], &wrong, &d.join("b"), &code).is_err());
    let bogus = trainer::Selection { unseal_for: vec!["peek".into()], ..Default::default() };
    assert!(trainer::trainer_bundle(build, &["train".into(), "cal".into()], &bogus, &d.join("c"), &code).is_err());
    let ok = trainer::Selection { unseal_for: vec!["calibration_fit".into()], ..Default::default() };
    trainer::trainer_bundle(build, &["train".into(), "cal".into()], &ok, &d.join("e"), &code).unwrap();
    assert!(build.join("cal/UNSEALED-calibration_fit.json").is_file());
    assert!(fs::metadata(d.join("e/rows.jsonl")).unwrap().permissions().readonly());
    // One use only.
    assert!(trainer::trainer_bundle(build, &["train".into(), "cal".into()], &ok, &d.join("f"), &code).is_err());
    // Dev-select still sealed.
    assert!(trainer::trainer_bundle(build, &["train".into(), "dev_select".into()], &ok, &d.join("g"), &code).is_err());
}

#[test]
fn freeze_moves_styles_one_way_by_history_text_and_cluster() {
    let lib = |id: &str, part: &str, cluster: &str, text: &str| json!({"id": id, "type": "voice", "partition": part, "cluster": cluster, "text": text, "origin": "invented"});
    let snap = vec![
        lib("a", "train", "c1", "sound like a sailor"),
        lib("b", "train", "c1", "sound like a buccaneer"),
        lib("c", "dev_iter", "c2", "sound like Jane Austen"),
        lib("d", "train", "c3", "sound like a nurse"),
        lib("e", "train", "c4", "sound like a farmer"),
    ];
    // In an earlier snapshot "d" was in CAL; a generation record saw "gone" in Dev-select.
    let hist = vec![
        ("snap0".to_string(), vec![lib("d", "cal", "c9", "sound like a nurse")]),
        ("used".to_string(), vec![json!({"id": "gone", "kind": "voice_prompt", "partition_at_generation": "dev_select", "text": "sound like a monk"})]),
    ];
    let pilot = vec![("pilot".to_string(), vec![
        json!({"id": "p1", "kind": "voice_prompt", "text": "Sound like JANE austen.", "origin": "pilot"}),
        json!({"id": "p2", "kind": "voice_prompt", "text": "sound like a lighthouse keeper", "origin": "pilot"}),
    ])];
    let (rows, moves) = styles::freeze(&snap, &hist, &pilot, false).unwrap();
    let p = |id: &str| rows.iter().find(|r| r.id == id).unwrap().partition.clone();
    assert_eq!((p("a"), p("c"), p("d"), p("gone"), p("p1"), p("p2")),
        ("train".into(), "dev_iter".into(), "cal".into(), "dev_select".into(), "dev_iter".into(), "train".into()));
    assert_eq!(moves["was_evaluation"], 1);
    assert_eq!(moves["text_match"], 1);
    // Closing clusters moves the cluster-mates of a moved item.
    let mut snap2 = snap.clone();
    snap2.push(lib("f", "train", "c3", "sound like a midwife"));
    let (rows, _) = styles::freeze(&snap2, &hist, &pilot, true).unwrap();
    assert_eq!(rows.iter().find(|r| r.id == "f").unwrap().partition, "cal");
    assert_eq!(rows.iter().find(|r| r.id == "e").unwrap().partition, "train");
    assert_eq!(styles::normalize_text("  Sound like: a  “Sailor”! "), "sound like a sailor");
}

#[test]
fn hosted_captures_need_admission_and_a_train_family() {
    let text = "A model draft.";
    let adm = json!({"schema": captures::ADMISSION_SCHEMA, "cohorts": {"frontier-captures-v1": {"records": [
        {"text_sha256": sha_hex(text.as_bytes()), "record_id": "cli-capture:r1", "origin": "model_only",
         "identity": {"attribution_label": "claude-opus-5"}}]}}});
    let a = captures::admitted(&adm).unwrap();
    let rev = json!({"schema": "slop_ninja_cli_capture_review_v1", "text": text, "text_sha256": sha_hex(text.as_bytes()),
        "request_sha256": "r1", "operation": "source_conditioned_draft", "source_group": "doi:1", "source": {"collection": "plos_one"}});
    let r = captures::review_row(&rev, &a, &o()).unwrap().unwrap();
    assert_eq!((r.generator_label.as_deref(), r.cohort_id.as_str(), r.share), (Some("claude-opus-5"), "frontier-captures-v1", 1.0));
    let mut other = rev.clone();
    other["text"] = json!("Unlisted.");
    other["text_sha256"] = json!(sha_hex(b"Unlisted."));
    assert!(captures::review_row(&other, &a, &o()).unwrap().is_none());
    let mut groups = HashMap::new();
    let (kept, ex) = select("train", vec![r.clone()], &HashMap::new(), &EvalKeys::default(), &HashMap::new(), &HashSet::new(), &groups);
    assert!(kept.is_empty() && ex[0].reason == "capture_source_not_train_family");
    groups.insert("doi:1".to_string(), "plos".to_string());
    let (kept, _) = select("train", vec![r.clone()], &HashMap::new(), &EvalKeys::default(), &HashMap::new(), &HashSet::new(), &groups);
    assert_eq!(kept[0].domain, "plos");
    let mut k = EvalKeys::default();
    k.add_manifest_row("test", &json!({"family_key": "doi:1"}));
    let (kept, _) = select("train", vec![r], &HashMap::new(), &k, &HashMap::new(), &HashSet::new(), &groups);
    assert!(kept.is_empty());
}

#[test]
fn ocr_measures_and_private_use_screen() {
    assert!(successor::case_flip("tHe") && successor::case_flip("thE"));
    assert!(!successor::case_flip("McDonald") && !successor::case_flip("The") && !successor::case_flip("NASA"));
    let lex = grammar_a_pools::ocr::Lexicon::from_words(["the", "information", "was", "sent", "to", "town"]);
    let m = successor::measure("The infor- mation was sent to tHe town", &lex);
    assert_eq!((m.case_flips, m.broken_hyphens), (1, 1));
    assert!(m.nonword_rate > 0.0);
    assert!(successor::has_private_use("ab\u{E012}c") && successor::has_private_use("x\u{FFFD}"));
    assert!(!successor::has_private_use("plain é text"));
    let p: std::collections::BTreeSet<String> = ["p:a", "p:b", "p:c", "p:d", "p:e"].iter().map(|s| s.to_string()).collect();
    let mut ev = HashSet::new();
    let t1 = successor::ellipse_train_prompts(&p, "salt", &ev);
    assert_eq!(t1.len(), 3);
    ev.insert(t1.iter().next().unwrap().clone());
    let t2 = successor::ellipse_train_prompts(&p, "salt", &ev);
    assert_eq!(t2.len(), 2);
    assert!(t2.is_subset(&t1));
}

#[test]
fn splits_successor_links_evaluation_files_and_rewrites_train_side() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let base = d.join("v4");
    let doc = |id: &str, split: &str, coll: &str, text: &str, extra: Value| {
        let mut v = json!({"id": id, "frozen_split": split, "collection": coll, "text": text, "text_sha256": sha_hex(text.as_bytes())});
        for (k, x) in extra.as_object().unwrap() { v[k] = x.clone(); }
        v
    };
    let man = |id: &str, split: &str, domain: &str| json!({"id": id, "split": split, "domain": domain, "source_group": format!("g:{id}")});
    let clean = "The town was sent the information";
    let noisy = "tHe tOwn wAs sEnt thE infor- mation";
    write_jsonl(&base.join("train/documents.jsonl"), &[doc("t1", "train", "pg", clean, json!({})), doc("t2", "train", "as", noisy, json!({"ocr_tier": "b"}))]);
    write_jsonl(&base.join("train/manifest.jsonl"), &[man("t1", "train", "project_gutenberg"), man("t2", "train", "american_stories")]);
    write_jsonl(&base.join("train-mining/documents.jsonl"), &[
        doc("m1", "train_mining", "as", clean, json!({"ocr_tier": "b"})),
        doc("m2", "train_mining", "as", clean, json!({"ocr_tier": "a"})),
        doc("m3", "train_mining", "ws", "A page \u{E001} glyph", json!({})),
    ]);
    write_jsonl(&base.join("train-mining/manifest.jsonl"), &[man("m1", "train_mining", "american_stories"),
        man("m2", "train_mining", "american_stories"), man("m3", "train_mining", "wikisource")]);
    let ell: Vec<Value> = (0..6).map(|i| doc(&format!("e{i}"), "diagnostic_ellipse", "ellipse", "An essay text.", json!({"provenance": {"prompt_key": format!("p:{}", i % 3)}}))).collect();
    write_jsonl(&base.join("diagnostics/ell/documents.jsonl"), &ell);
    write_jsonl(&base.join("diagnostics/ell/manifest.jsonl"), &(0..6).map(|i| man(&format!("e{i}"), "diagnostic_ellipse", "ellipse")).collect::<Vec<_>>());
    fs::write(base.join("diagnostics/ell/SLICE.json"), br#"{"spec": {}}"#).unwrap();
    write_jsonl(&base.join("cal/manifest.jsonl"), &[json!({"id": "c1", "domain": "x", "source_group": "p:1", "split": "cal"})]);
    let mut sums = String::new();
    for f in ["train/documents.jsonl", "train/manifest.jsonl", "train-mining/documents.jsonl", "train-mining/manifest.jsonl",
        "diagnostics/ell/documents.jsonl", "diagnostics/ell/manifest.jsonl", "diagnostics/ell/SLICE.json", "cal/manifest.jsonl"] {
        sums.push_str(&format!("{}  ./{f}\n", bundle::file_sha(&base.join(f)).unwrap()));
    }
    fs::write(base.join("SHA256SUMS"), sums).unwrap();
    fs::write(d.join("lex.txt"), "the\ntown\nwas\nsent\ninformation\n").unwrap();
    let cfg = json!({"schema": successor::CONFIG_SCHEMA, "base": base, "version": "v5", "lexicon": d.join("lex.txt"),
        "ocr": {"domains": ["american_stories"], "max_case_flip_rate": 0.05, "max_broken_hyphen_rate": 0.05,
                "max_nonword_rate": 0.2, "max_odd_token_rate": 0.05, "cap_train_mining": 1, "salt": "s"},
        "pua": {"domains": ["wikisource"]}, "ellipse": {"slice": "ell", "salt": "e"}});
    fs::write(d.join("succ.json"), serde_json::to_vec(&cfg).unwrap()).unwrap();
    let r = successor::successor(&d.join("succ.json"), &d.join("v5"), &json!({})).unwrap();
    let v5 = d.join("v5");
    let ids = |p: &str| -> Vec<String> { fs::read_to_string(v5.join(p)).unwrap().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()["id"].as_str().unwrap().to_string()).collect() };
    // Noisy Train row dropped; the cap keeps the tier-a mining row; the private-use row is dropped.
    let train = ids("train/documents.jsonl");
    assert!(train.contains(&"t1".to_string()) && !train.contains(&"t2".to_string()));
    assert_eq!(ids("train-mining/documents.jsonl"), vec!["m2".to_string()]);
    assert_eq!(ids("train-mining/manifest.jsonl"), vec!["m2".to_string()]);
    // ELLIPSE: prompt p:1 is a CAL group key and stays held; Train gets whole prompts only.
    let held = ids("diagnostics/ell/documents.jsonl");
    assert!(held.contains(&"e1".to_string()) && held.contains(&"e4".to_string()));
    let moved: Vec<&String> = train.iter().filter(|i| i.starts_with('e')).collect();
    assert_eq!(moved.len() + held.len(), 6);
    assert_eq!(ids("train/manifest.jsonl").len(), train.len());
    // Evaluation files are the same inode; SHA256SUMS verifies.
    use std::os::unix::fs::MetadataExt;
    assert_eq!(fs::metadata(base.join("cal/manifest.jsonl")).unwrap().ino(), fs::metadata(v5.join("cal/manifest.jsonl")).unwrap().ino());
    for line in fs::read_to_string(v5.join("SHA256SUMS")).unwrap().lines() {
        let (s, p) = line.split_once("  ").unwrap();
        assert_eq!(bundle::file_sha(&v5.join(p.trim_start_matches("./"))).unwrap(), s, "{p}");
    }
    assert_eq!(r["ellipse"]["evaluation_prompts_held"], json!(["p:1"]));
    assert!(successor::successor(&d.join("succ.json"), &d.join("v5"), &json!({})).is_err(), "written once");
}

fn ext(id: &str, cohort: &str, group: &str, text: &str, label: Option<&str>, rep: f64) -> Value {
    let model = label.is_some();
    json!({"schema": EXTERNAL_POOL_DOCUMENT_SCHEMA, "id": id, "pool_status": "admitted", "intended_use": "train_only",
        "split_assigned": false, "rights": {"commercial_training": true}, "text": text,
        "text_sha256": sha_hex(text.as_bytes()), "origin": if model { "model_only" } else { "human_only" },
        "ai_byte_spans": if model { json!([[0, text.len()]]) } else { json!([]) },
        "share_target": if model { 1.0 } else { 0.0 }, "generator_label": label, "generator_key": label.map(|l| format!("ext:{l}")),
        "operation": if model { "title_conditioned_generation" } else { "human_reference" },
        "cohort_id": cohort, "source_group": group, "writer_key": null, "domain": "ext_books", "genre": "books",
        "collection": "ext_benchmark", "format": {"repeated_word_8gram_share": rep}})
}

#[test]
fn external_rows_keep_pool_labels_and_refuse_unadmitted_documents() {
    let m = external_row(&ext("x1", "paired", "src:1", "A model text.", Some("gen-x"), 0.0), "ext", &o()).unwrap();
    assert_eq!((m.origin.as_str(), m.share, m.ai_byte_spans.clone()), ("model_only", 1.0, vec![[0, 13]]));
    assert_eq!((m.generator_label.as_deref(), m.cohort_id.as_str(), m.domain.as_str()), (Some("gen-x"), "paired", "ext_books"));
    assert_eq!((m.source_kind, m.split.as_str(), m.population.as_str()), (SourceKind::ExternalPool, "train", "external:ext"));
    let h = external_row(&ext("x2", "paired", "src:1", "A human text.", None, 0.0), "ext", &o()).unwrap();
    assert!(h.generator_label.is_none() && h.share == 0.0 && h.operation == "human");
    for (k, v) in [("intended_use", json!("evaluation")), ("split_assigned", json!(true)), ("text_sha256", json!("00"))] {
        let mut bad = ext("x3", "paired", "src:1", "Text.", Some("gen-x"), 0.0);
        bad[k] = v;
        assert!(external_row(&bad, "ext", &o()).is_err(), "{k}");
    }
    let mut partial = ext("x4", "paired", "src:1", "Two words.", Some("gen-x"), 0.0);
    partial["ai_byte_spans"] = json!([[0, 3]]);
    assert!(external_row(&partial, "ext", &o()).is_err());
    let mut no_rights = ext("x5", "paired", "src:1", "Text.", Some("gen-x"), 0.0);
    no_rights["rights"]["commercial_training"] = json!(false);
    assert!(external_row(&no_rights, "ext", &o()).is_err());
    assert_eq!(per_mille("s", "k"), per_mille("s", "k"));
    assert!(per_mille("s", "k") < 1000);
}

#[test]
fn external_pools_filter_cohorts_repetition_and_sample_deterministically() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    write_jsonl(&d.join("train.jsonl"), &[human("h1", "train", "g1", "Human one.")]);
    write_frozen(&d.join("frozen.jsonl"));
    let mut pool = vec![
        ext("e-h1", "paired", "src:1", "Pool human one.", None, 0.0),
        ext("e-m1", "paired", "src:1", "Pool model one.", Some("gen-x"), 0.1),
        ext("e-m2", "paired", "src:1", "Pool model two.", Some("gen-x"), 0.7),
        ext("e-u1", "unpaired", "src:2", "Unpaired model.", Some("gen-x"), 0.0),
        ext("e-p1", "paraphrase", "src:3", "Paraphrased human.", Some("para"), 0.0),
    ];
    for i in 0..40 {
        pool.push(ext(&format!("e-s{i}"), "paired", &format!("src:s{i}"), &format!("Sampled model {i}."), Some("gen-y"), 0.0));
    }
    write_jsonl(&d.join("pool.jsonl"), &pool);
    let mut cfg = json!({"schema": bundle::CONFIG_SCHEMA,
        "splits": {"train": {"humans": [d.join("train.jsonl")]}}, "frozen_partitions": d.join("frozen.jsonl"),
        "external_train_pools": [{"name": "ext", "documents": d.join("pool.jsonl"),
            "include_cohorts": ["paired", "paraphrase"], "keep_all_cohorts": ["paraphrase"],
            "max_repeated_word_8gram_share": 0.5, "group_per_mille": 1000, "positive_per_mille": 1000, "seed": "s"}]});
    fs::write(d.join("config.json"), serde_json::to_vec(&cfg).unwrap()).unwrap();
    let code = json!({"assembler_sha256": "test"});
    let b = bundle::build(&d.join("config.json"), &d.join("out"), &code).unwrap();
    // h1 + e-h1 + e-m1 + e-p1 + 40 sampled: e-m2 is degenerate, e-u1 is outside the included cohorts.
    assert_eq!(b["splits"]["train"]["rows"], 44);
    let dir = Path::new(b["dir"].as_str().unwrap());
    let bj: Value = serde_json::from_slice(&fs::read(dir.join("BUILD.json")).unwrap()).unwrap();
    let ig = &bj["ignored_rows_of_unbuilt_splits"];
    assert_eq!(ig["external:ext:cohort_not_included"], 1);
    assert_eq!(ig["external:ext:repeated_word_8gram_share_at_or_above_bound"], 1);
    // Sampling half the groups keeps the paraphrase cohort whole and is deterministic.
    cfg["external_train_pools"][0]["group_per_mille"] = json!(500);
    fs::write(d.join("config.json"), serde_json::to_vec(&cfg).unwrap()).unwrap();
    let b2 = bundle::build(&d.join("config.json"), &d.join("out2"), &code).unwrap();
    let b3 = bundle::build(&d.join("config.json"), &d.join("out3"), &code).unwrap();
    assert_eq!(b2["splits"]["train"]["rows"], b3["splits"]["train"]["rows"]);
    let n = b2["splits"]["train"]["rows"].as_u64().unwrap();
    assert!(n > 3 && n < 44, "{n}");
    let rows: Vec<Value> = fs::read_to_string(Path::new(b2["dir"].as_str().unwrap()).join("train/rows.jsonl")).unwrap()
        .lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(rows.iter().any(|r| r["id"] == "e-p1" && r["source_kind"] == "external_pool"));
    cfg["external_train_pools"][0]["positive_per_mille"] = json!(1001);
    fs::write(d.join("config.json"), serde_json::to_vec(&cfg).unwrap()).unwrap();
    assert!(bundle::build(&d.join("config.json"), &d.join("out4"), &code).is_err());
}

fn seal_file(d: &Path, binds: &str) -> (PathBuf, String) {
    let p = d.join("SEAL.json");
    let b = serde_json::to_vec(&json!({"sealed": true, "items": [{"sha256": binds}]})).unwrap();
    fs::write(&p, &b).unwrap();
    (p, sha_hex(&b))
}

/// A synthetic Test input set: 2 domains x 3 groups x 2 humans, one primary
/// mirror and one primary span per human (one generator held out), one
/// robustness continuation per group, one frontier full rewrite per group,
/// one diagnostic human, one Train group colliding human.
fn test_inputs(d: &Path) -> Value {
    let mut humans = Vec::new();
    let mut locals = Vec::new();
    let mut fx = Vec::new();
    for (di, dom) in ["plos", "wikipedia"].iter().enumerate() {
        for g in 0..3 {
            let group = format!("g{di}{g}");
            for k in 0..2 {
                let id = format!("h{di}{g}{k}");
                let mut h = human(&id, "test", &group, &format!("Human text {id} with words."));
                h["collection"] = json!(format!("{dom}_historical"));
                h["rights"] = json!({"external_evaluation": true});
                h["aliases"] = json!([format!("a:{id}")]);
                if k == 0 {
                    h["provenance"] = json!({"writer_demographics": {"ell_status": "Yes", "grade": null}});
                    h["ocr_tier"] = json!("b");
                }
                humans.push(h);
                let mut m = local(&format!("m{id}"), "test", &id, &group, &format!("Model mirror {id}."), "mirror", Some("v-cal"));
                m["domain"] = json!(dom);
                m["generation"]["generator"]["family"] = json!(if k == 0 { "olmo2" } else { "qwen3.6" });
                m["generation"]["style"]["voice_text"] = json!("sound like a court reporter");
                locals.push(m);
                let mut s = local(&format!("s{id}"), "test", &id, &group, &format!("Span rewrite text {id}."), "span", None);
                s["domain"] = json!(dom);
                locals.push(s);
            }
            let pid = format!("h{di}{g}0");
            let mut r = local(&format!("r{pid}"), "test", &pid, &group, &format!("Robust continuation {pid}."), "continuation", None);
            r["cohort"] = json!("test_robustness");
            r["population"] = json!("robustness_test_only_outside_primary");
            r["generation"]["generator"]["family"] = json!("olmo2");
            r["generation"]["style"] = Value::Null;
            locals.push(r);
            fx.push(frontier(&format!("f{pid}"), "test", &pid, &group, &format!("Frontier rewrite {pid}."), "claude-opus-5-5", "v-test-1", true));
        }
    }
    // A Test human whose group is a Train group: dropped by the Train closure.
    let mut tr = human("htrain", "test", "gtrain", "Train group human.");
    tr["rights"] = json!({"external_evaluation": true});
    humans.push(tr);
    write_jsonl(&d.join("test-humans.jsonl"), &humans);
    write_jsonl(&d.join("test-local.jsonl"), &locals);
    write_jsonl(&d.join("fx-test/claude-opus/records.jsonl"), &fx);
    write_jsonl(&d.join("train-groups.jsonl"), &[json!({"id": "x", "source_group": "gtrain"})]);
    let mut diag = human("dg1", "diagnostic_sbcsae_spoken_conversation", "sbcsae_recording:S1", "Spoken diagnostic text.");
    diag["collection"] = json!("sbcsae_conversation_transcripts");
    diag["extraction"] = json!({"unit": {"normalization": {"removed_markup_per_100_words": 12.5}}});
    write_jsonl(&d.join("diag.jsonl"), &[diag]);
    let mut fz_rows: Vec<Value> = frozen_rows().iter().map(|r| serde_json::to_value(r).unwrap()).collect();
    fz_rows.push(serde_json::to_value(fz("v-test-1", "voice_prompt", "test", "vc-9", "invented", "occupation", "sound like a ferry pilot")).unwrap());
    write_jsonl(&d.join("frozen-test.jsonl"), &fz_rows);
    json!({"schema": bundle::CONFIG_SCHEMA,
        "splits": {"test": {"humans": [d.join("test-humans.jsonl")]}},
        "local_positives": [d.join("test-local.jsonl")], "frontier_exports": [d.join("fx-test")],
        "frozen_partitions": d.join("frozen-test.jsonl"), "train_group_manifests": [d.join("train-groups.jsonl")],
        "diagnostic_humans": [{"name": "sbcsae_spoken_conversation", "documents": d.join("diag.jsonl"), "split": "test"}]})
}

#[test]
fn test_bundle_needs_the_written_seal_binding_this_binary() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let cfg = test_inputs(d);
    fs::write(d.join("test.json"), serde_json::to_vec(&cfg).unwrap()).unwrap();
    let code = json!({"assembler_sha256": "asm-digest"});
    let out = d.join("bundles");
    // No seal: Test is refused.
    assert!(bundle::build(&d.join("test.json"), &out, &code).is_err());
    let missing = bundle::SealCheck { path: d.join("SEAL.json"), sha256: "0".repeat(64) };
    assert!(bundle::build_sealed(&d.join("test.json"), &out, &code, Some(&missing)).is_err());
    // A seal that does not bind this binary, or a wrong stated digest, is refused.
    let (p, sha) = seal_file(d, "other-binary");
    assert!(bundle::build_sealed(&d.join("test.json"), &out, &code, Some(&bundle::SealCheck { path: p.clone(), sha256: sha })).is_err());
    let (p, sha) = seal_file(d, "asm-digest");
    assert!(bundle::build_sealed(&d.join("test.json"), &out, &code, Some(&bundle::SealCheck { path: p.clone(), sha256: "f".repeat(64) })).is_err());
    assert!(!out.join("CURRENT.json").exists());
    let seal = bundle::SealCheck { path: p, sha256: sha };
    let b = bundle::build_sealed(&d.join("test.json"), &out, &code, Some(&seal)).unwrap();
    let dir = PathBuf::from(b["dir"].as_str().unwrap());
    let rows: Vec<Value> = fs::read_to_string(dir.join("test/rows.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let pop = |p: &str| rows.iter().filter(|r| r["population"] == p).count();
    // 12 humans + 24 local primary + 6 frontier; 6 robustness; 1 diagnostic; the Train-group human dropped.
    assert_eq!((pop("primary"), pop("frontier"), pop("robustness_test_only_outside_primary"), pop("sbcsae_spoken_conversation")), (36, 6, 6, 1));
    assert!(!rows.iter().any(|r| r["id"] == "htrain"));
    let ex = fs::read_to_string(dir.join("test/exclusions.jsonl")).unwrap();
    assert!(ex.contains("evaluation_row_in_train_group"));
    let m: Value = serde_json::from_slice(&fs::read(dir.join("test/MANIFEST.json")).unwrap()).unwrap();
    assert_eq!((m["test_opened"].clone(), m["seal"]["sha256"].clone()), (json!(true), json!(seal.sha256)));
    assert_eq!(m["rows_by_population"]["frontier"], 6);
    // Labels: robustness rows are continuations of OLMo-2 base, outside primary.
    let r = rows.iter().find(|r| r["id"] == "rh000").unwrap();
    assert_eq!((r["operation"].as_str(), r["origin"].as_str(), r["generator_label"].as_str()), (Some("continuation"), Some("mixed"), Some("olmo2")));
    // Frontier rows: canonical label, held-out style from the Test partition, the parent's collection.
    let f = rows.iter().find(|r| r["id"] == "fh000").unwrap();
    assert_eq!((f["generator_label"].as_str(), f["held_out_style"].as_bool(), f["style_partition"].as_str()), (Some("claude-opus-5-5"), Some(true), Some("test")));
    assert_eq!(f["collection"], "plos_historical");
    // Diagnostic and Test-only human fields.
    let h = rows.iter().find(|r| r["id"] == "h000").unwrap();
    assert_eq!((h["demographics"]["ell_status"].as_str(), h["ocr_tier"].as_str()), (Some("Yes"), Some("b")));
    assert!(rows.iter().find(|r| r["id"] == "h001").unwrap().get("demographics").is_none());
    let dg = rows.iter().find(|r| r["id"] == "dg1").unwrap();
    assert_eq!((dg["domain"].as_str(), dg["removed_markup_per_100_words"].as_f64()), (Some("sbcsae_spoken_conversation"), Some(12.5)));
    // Weights: frontier pooled with primary: equal group mass, equal observed cell mass, equal documents.
    let prim: Vec<&Value> = rows.iter().filter(|r| r["population"] == "primary" || r["population"] == "frontier").collect();
    let n = prim.len() as f64;
    let wsum: f64 = prim.iter().map(|r| r["weight"].as_f64().unwrap()).sum();
    assert!((wsum - n).abs() < 1e-9);
    let g000: f64 = prim.iter().filter(|r| r["source_group"] == "g00").map(|r| r["weight"].as_f64().unwrap()).sum();
    assert!((g000 - n / 6.0).abs() < 1e-9, "{g000}");
    // g00: 2 humans, 2 mirrors (model_only), 2 spans + 1 frontier (mixed): each cell n/18.
    let cell = |o: &str| prim.iter().filter(|r| r["source_group"] == "g00" && r["origin"] == o).map(|r| r["weight"].as_f64().unwrap()).sum::<f64>();
    for o in ["human_only", "model_only", "mixed"] {
        assert!((cell(o) - n / 18.0).abs() < 1e-9);
    }
    let fr = prim.iter().find(|r| r["id"] == "fh000").unwrap()["balanced_weight"].as_f64().unwrap();
    assert!((fr - n / 54.0).abs() < 1e-9);
    let w: Value = serde_json::from_slice(&fs::read(dir.join("test/weights.json")).unwrap()).unwrap();
    assert!(w["frontier_pooled_with_primary"].is_string());
    assert_eq!(w["populations"]["primary"], 42);

    // Scoring rows, collections and audit candidates need the same seal.
    let sr = d.join("scoring");
    assert!(test_split::scoring_rows(&dir, "test", None, &code, &sr).is_err());
    test_split::scoring_rows(&dir, "test", Some(&seal), &code, &sr).unwrap();
    let srows: Vec<Value> = fs::read_to_string(sr.join("rows.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(srows.len(), rows.len());
    assert!(srows.iter().all(|r| r.as_object().unwrap().len() == trainer::TRAINER_FIELDS.len() && r["cohort_weight"] == 1.0 && r["split"] == "test"));
    assert!(test_split::scoring_rows(&dir, "dev_select", Some(&seal), &code, &d.join("x")).is_err());
    let coll = d.join("collections.json");
    test_split::public_collections(&dir.join("test/rows.jsonl"), &[d.join("test-humans.jsonl")], &seal, "asm-digest", &coll).unwrap();
    let c: Value = serde_json::from_slice(&fs::read(&coll).unwrap()).unwrap();
    assert_eq!(c["collections"], json!(["plos_historical", "wikipedia_historical"]));
    let cands = d.join("cands");
    test_split::audit_candidates(&dir.join("test/rows.jsonl"), &[d.join("test-humans.jsonl")], &[d.join("test-local.jsonl")], &seal, "asm-digest", &cands).unwrap();
    let bc = fs::read_to_string(cands.join("test-bundle/candidates.jsonl")).unwrap();
    assert!(!bc.contains("\"dg1\"") && bc.lines().count() == rows.len() - 1);
    let pc = fs::read_to_string(cands.join("test-prompts/candidates.jsonl")).unwrap();
    assert_eq!(pc.lines().count(), 1);
    // A scan hit on one human and on the prompt excludes the groups; a rebuild drops them whole.
    let scan = d.join("scan");
    fs::create_dir_all(&scan).unwrap();
    fs::write(scan.join("scan-report.json"), serde_json::to_vec(&json!({"entries_without_files": [], "files_scanned": 3, "candidates": 49})).unwrap()).unwrap();
    let pid = serde_json::from_str::<Value>(pc.lines().next().unwrap()).unwrap()["id"].as_str().unwrap().to_string();
    write_jsonl(&scan.join("hits.jsonl"), &[json!({"candidate_id": "h010", "reasons": ["matches_prior_exact_text"], "label": "x", "path": "p"})]);
    let (rp, xp) = (d.join("receipt.json"), d.join("excl.jsonl"));
    let r = test_split::audit_receipt(&cands, &scan, &seal, "asm-digest", &rp, &xp).unwrap();
    assert_eq!(r["excluded_source_groups"], 1);
    let mut cfg2 = cfg.clone();
    cfg2["excluded_source_groups"] = json!([xp]);
    fs::write(d.join("test2.json"), serde_json::to_vec(&cfg2).unwrap()).unwrap();
    let b2 = bundle::build_sealed(&d.join("test2.json"), &out, &code, Some(&seal)).unwrap();
    let rows2 = fs::read_to_string(PathBuf::from(b2["dir"].as_str().unwrap()).join("test/rows.jsonl")).unwrap();
    assert!(!rows2.contains("\"source_group\":\"g01\"") && rows2.contains("\"source_group\":\"g02\""));
    // The prompt is used by every group's mirrors: a hit on it excludes all of them.
    write_jsonl(&scan.join("hits.jsonl"), &[json!({"candidate_id": pid, "reasons": ["matches_prior_exact_text"], "label": "x", "path": "p"})]);
    let r = test_split::audit_receipt(&cands, &scan, &seal, "asm-digest", &d.join("r2.json"), &d.join("x2.jsonl")).unwrap();
    assert_eq!(r["excluded_source_groups"], 6);
    // An incomplete registry is an error.
    fs::write(scan.join("scan-report.json"), serde_json::to_vec(&json!({"entries_without_files": ["e"]})).unwrap()).unwrap();
    assert!(test_split::audit_receipt(&cands, &scan, &seal, "asm-digest", &d.join("r3.json"), &d.join("x3.jsonl")).is_err());
}

#[test]
fn evaluation_weights_without_frontier_rows_are_unchanged() {
    // CAL-shaped split: primary and supplementary populations are separate pools.
    let mut rows = vec![
        row("a", "plos", "g1", "human_only", None, "human:x", SourceKind::Human),
        row("b", "plos", "g1", "model_only", Some("q"), "local:primary_mirror", SourceKind::LocalPositive),
        row("c", "plos", "g2", "human_only", None, "human:x", SourceKind::Human),
        row("d", "plos", "g2", "model_only", Some("q"), "local:diverse_mirror", SourceKind::LocalPositive),
    ];
    for r in rows.iter_mut() {
        r.population = if r.cohort_id == "local:diverse_mirror" { "supplementary".into() } else { "primary".into() };
    }
    let w = assign_weights(&mut rows, false, &CohortWeights::default(), &Weighting::default()).unwrap();
    assert!(w.get("frontier_pooled_with_primary").is_none());
    assert_eq!(w["populations"], json!({"primary": 3, "supplementary": 1}));
    let ws: Vec<f64> = rows.iter().map(|r| r.weight).collect();
    assert_eq!(ws, vec![0.75, 0.75, 1.5, 1.0]);
}

// ---------------------------------------------------------------------------
// Round 2 (B5a): supervision targets, covariate balance, ESS, cohort roles and
// block tags. Synthetic rows only.

/// A synthetic text of `n` whitespace words.
fn words_text(tag: &str, n: usize) -> String {
    let mut s = tag.to_string();
    for i in 1..n {
        s.push_str(if i % 7 == 0 { " stop." } else { " word" });
    }
    s
}

/// A two-domain Train split, a Dev-iter split, local mirrors and rewrites,
/// two frontier labels and a legacy row; returns the config JSON (written to
/// `d/config.json` by the caller after any change).
fn r2_fixture(d: &Path) -> Value {
    let plos = [("g1", 20usize), ("g2", 35), ("g3", 50), ("g4", 65), ("g5", 80)];
    let wiki = [("g6", 30usize), ("g7", 60), ("g8", 90)];
    let mut humans = Vec::new();
    let mut dm = Vec::new();
    for (g, n) in plos.iter().chain(wiki.iter()) {
        humans.push(human(&format!("h-{g}"), "train", g, &words_text(&format!("Human {g}"), *n)));
    }
    humans.push(human("h-g4b", "train", "g4", &words_text("Human g4 second", 120)));
    for (g, _) in &wiki {
        dm.push(json!({"id": format!("h-{g}"), "domain": "wikipedia"}));
    }
    write_jsonl(&d.join("train.jsonl"), &humans);
    write_jsonl(&d.join("train-domains.jsonl"), &dm);
    write_jsonl(&d.join("dev.jsonl"), &[human("d1", "dev_iter", "gd", &words_text("Dev human", 40))]);
    write_frozen(&d.join("frozen.jsonl"));
    let mut locals = Vec::new();
    for (i, (g, n)) in plos.iter().chain(wiki.iter()).enumerate() {
        let parent = format!("h-{g}");
        locals.push(local(&format!("m-{g}"), "train", &parent, g, &words_text(&format!("Mirror {g}"), n + 10 * (i % 3)), "mirror", Some("v-train")));
        if i % 2 == 0 {
            locals.push(local(&format!("x-{g}"), "train", &parent, g, &words_text(&format!("Rewrite {g}"), n + 5), "span", Some("v-train")));
        }
    }
    locals.push(local("m-d1", "dev_iter", "d1", "gd", &words_text("Dev mirror", 45), "mirror", None));
    write_jsonl(&d.join("local.jsonl"), &locals);
    let mut fr = Vec::new();
    for (i, (g, n)) in plos.iter().chain(wiki.iter()).enumerate() {
        let label = if i % 2 == 0 { "claude-opus-5-5" } else { "requested:gpt-6-astra" };
        fr.push(frontier(&format!("f-{g}"), "train", &format!("h-{g}"), g, &words_text(&format!("Frontier {g}"), n + 3), label, "v-train", false));
    }
    write_jsonl(&d.join("fx/all/records.jsonl"), &fr);
    write_jsonl(&d.join("legacy.jsonl"), &[legacy("l1", "encoder_stage3_work_v1:a_b:pg1", &words_text("Legacy model", 33), 1, Some("Qwen/Qwen3.6-35B-A3B:x"))]);
    json!({"schema": bundle::CONFIG_SCHEMA,
        "splits": {"train": {"humans": [d.join("train.jsonl")], "domain_manifests": [d.join("train-domains.jsonl")]},
                   "dev_iter": {"humans": [d.join("dev.jsonl")]}},
        "local_positives": [d.join("local.jsonl")], "frontier_exports": [d.join("fx")],
        "legacy_train": d.join("legacy.jsonl"), "legacy_families": {"Qwen/Qwen3.6-35B-A3B": "qwen3.6"},
        "frozen_partitions": d.join("frozen.jsonl"),
        "cohort_weights": {"default": 1.0, "cohorts": {"stage3_legacy_train": 0.5}}})
}

fn build_fixture(d: &Path, cfg: &Value, name: &str) -> Result<PathBuf> {
    fs::write(d.join(format!("{name}.json")), serde_json::to_vec(cfg).unwrap()).unwrap();
    let b = bundle::build(&d.join(format!("{name}.json")), &d.join(format!("bundles-{name}")), &json!({"assembler_sha256": "test"}))?;
    Ok(PathBuf::from(b["dir"].as_str().unwrap()))
}

fn file_digest(p: &Path) -> String {
    sha_hex(&fs::read(p).unwrap())
}

/// With no round-2 option the build and trainer-bundle bytes are those of the
/// assembler before B5a (digests pinned on origin/main f2b44852; D3 needs it).
#[test]
fn round2_options_off_keep_build_and_trainer_bundle_bytes() {
    use crate::trainer;
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let cfg = r2_fixture(d);
    let build = build_fixture(d, &cfg, "off").unwrap();
    let code = json!({"assembler_sha256": "test"});
    let tb = d.join("tb-final");
    trainer::trainer_bundle(&build, &["train".into(), "dev_iter".into()], &trainer::Selection::default(), &tb, &code).unwrap();
    let tbc = d.join("tb-factor");
    let sel = trainer::Selection { weighting: trainer::TrainerWeighting::CohortFactor, ..Default::default() };
    trainer::trainer_bundle(&build, &["train".into(), "dev_iter".into()], &sel, &tbc, &code).unwrap();
    let got = json!({
        "train/rows.jsonl": file_digest(&build.join("train/rows.jsonl")),
        "train/weights.json": file_digest(&build.join("train/weights.json")),
        "train/exclusions.jsonl": file_digest(&build.join("train/exclusions.jsonl")),
        "dev-iter/rows.jsonl": file_digest(&build.join("dev-iter/rows.jsonl")),
        "dev-iter/weights.json": file_digest(&build.join("dev-iter/weights.json")),
        "final/rows.jsonl": file_digest(&tb.join("rows.jsonl")),
        "final/weights.json": file_digest(&tb.join("weights.json")),
        "factor/rows.jsonl": file_digest(&tbc.join("rows.jsonl")),
        "factor/weights.json": file_digest(&tbc.join("weights.json")),
    });
    let pinned = json!({
        "dev-iter/rows.jsonl": "465f0f7833abd8abed00865742c68cecbd5571b2094bf90e755de16757dfc4fe",
        "dev-iter/weights.json": "5b3c7da57ddc823a13b385d15f654b111c21cea29802a2ea4bb27467afbbb496",
        "factor/rows.jsonl": "e2fdac0c16ae02ab33bee71f2700a39f0159d030c20cbd184c9638fdbcf088a8",
        "factor/weights.json": "22112068f09de146a9785c2a234636576d1620d1a3d0c38c618d2453cd16ee0f",
        "final/rows.jsonl": "fc6d62e38aee1495460f89b5f11d5d016e8b02c1d04ad6403f2d20eea611d377",
        "final/weights.json": "ce4df7df2277ab89bd8ed2bb0d2d05083d4c5f641a9bf872daf2703807aa7a46",
        "train/exclusions.jsonl": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "train/rows.jsonl": "7af713e5f3e443c90dd620b4a8d98fbda8afb58ab3d7896ed5498f891b561843",
        "train/weights.json": "1c1cff608dd94fab2481c85b50e3f7b71383bf51680b58becf221db1ba1e8c41",
    });
    assert_eq!(got, pinned, "{}", serde_json::to_string_pretty(&got).unwrap());
    // An empty round-2 block is the same as none: every option is off.
    let mut cfg2 = cfg.clone();
    cfg2["round2"] = json!({});
    let build2 = build_fixture(d, &cfg2, "off-empty").unwrap();
    for f in ["train/rows.jsonl", "train/weights.json", "dev-iter/rows.jsonl", "dev-iter/weights.json"] {
        assert_eq!(file_digest(&build2.join(f)), pinned[f].as_str().unwrap(), "{f}");
    }
}

/// The quantile loop the bins were first written as; `covariate::quantile_bins`
/// finds the same cuts per index.
fn reference_cut_index(dist: &[(usize, f64)], b: usize) -> Vec<usize> {
    let total: f64 = dist.iter().map(|x| x.1).sum();
    let last = dist.len() - 1;
    let mut cut = Vec::new();
    let mut j = 0usize;
    let mut cum = 0.0f64;
    for k in 1..b {
        while j < last && (cum + dist[j].1) * (b as f64) < (k as f64) * total * (1.0 - covariate::TIE_TOLERANCE) {
            cum += dist[j].1;
            j += 1;
        }
        if j < last && cut.last() != Some(&j) {
            cut.push(j);
        }
    }
    cut
}

#[test]
fn covariate_bins_are_equal_mass_quantiles_with_merged_ties_and_open_edges() {
    use crate::covariate::*;
    let d = [(10usize, 1.0), (20, 1.0), (30, 1.0), (40, 1.0)];
    assert_eq!(quantile_bins(&d, 1).cuts, Vec::<usize>::new());
    assert_eq!(quantile_bins(&d, 2).cuts, vec![20]);
    assert_eq!(quantile_bins(&d, 3).cuts, vec![20, 30]);
    assert_eq!(quantile_bins(&d, 4).cuts, vec![10, 20, 30]);
    assert_eq!(quantile_bins(&d, 4).cut_index, vec![0, 1, 2]);
    // Ties merge; a cut at the largest word count is dropped, so every bin holds human mass.
    assert_eq!(quantile_bins(&[(10, 3.0), (20, 1.0)], 4).cuts, vec![10]);
    assert_eq!(quantile_bins(&[(10, 1.0), (20, 3.0)], 2).cuts, Vec::<usize>::new());
    // Positives outside the human range go to the edge bins.
    let b = quantile_bins(&d, 3);
    assert_eq!([b.of(1), b.of(20), b.of(21), b.of(30), b.of(31), b.of(10_000)], [0, 0, 1, 1, 2, 2]);
    // Slot masses map onto the same bins as `of`.
    let cell = [(5usize, 1.0), (20, 2.0), (25, 4.0), (45, 8.0)];
    let slots = slot_masses(&d, cell.iter().copied());
    // Slot j: exactly j distinct human word counts below (20 has 10 below it).
    assert_eq!(slots, vec![1.0, 2.0, 4.0, 0.0, 8.0]);
    assert_eq!(b.masses(&slots), vec![3.0, 4.0, 8.0]);
    let hum: Vec<f64> = d.iter().map(|x| x.1).collect();
    assert_eq!(b.masses(&hum), vec![2.0, 1.0, 1.0]);
    // The per-index cuts equal the quantile loop's at every nominal b up to past the search
    // bound (ties, thirds and small masses included), and from the bound on every b gives the
    // finest partition.
    let dists: Vec<Vec<(usize, f64)>> = vec![
        d.to_vec(),
        vec![(10, 3.0), (20, 1.0)],
        vec![(150, 0.9), (200, 0.05), (300, 0.05)],
        (0..40).map(|i| (100 + i, 1.0 / 3.0)).collect(),
        (0..60).map(|i| (100 + 2 * i, 1.0 + ((i * 37) % 11) as f64 / 7.0)).collect(),
        (0..30).map(|i| (150 + i, if i % 9 == 0 { 1e-3 } else { 0.1 })).collect(),
        vec![(5, 1.0)],
    ];
    // An exact float tie: at b = 2 the first count's running mass times b equals the quantile
    // level k·T·(1 − tol) bit for bit, and reaching the level makes the cut.
    let g1 = 1.0f64 * (1.0 - TIE_TOLERANCE);
    let tie = vec![(10usize, g1 / 2.0), (20, 1.0 - g1 / 2.0)];
    assert!(tie[0].1 + tie[1].1 == 1.0 && tie[0].1 * 2.0 == g1);
    assert_eq!((quantile_bins(&tie, 2).cut_index, reference_cut_index(&tie, 2)), (vec![0], vec![0]));
    let dists: Vec<Vec<(usize, f64)>> = dists.into_iter().chain([tie]).collect();
    for dist in &dists {
        let bound = search_bound(dist);
        for b in 1..=bound + 2 {
            assert_eq!(quantile_bins(dist, b).cut_index, reference_cut_index(dist, b), "b {b} of {dist:?}");
        }
        let finest: Vec<usize> = (0..dist.len() - 1).collect();
        for b in [bound, bound + 1, 3 * bound + 5] {
            assert_eq!(quantile_bins(dist, b).cut_index, finest, "b {b}, bound {bound}");
        }
    }
}

#[test]
fn covariate_bins_take_the_most_realised_bins_within_r0_row_cap() {
    use crate::covariate::*;
    let d = [(10usize, 1.0), (20, 1.0), (30, 1.0), (40, 1.0)];
    let hum: Vec<f64> = d.iter().map(|x| x.1).collect();
    // b = 2 has factors 0.75 and 1.5; every finer partition leaves a bin without positives.
    let cell = [(12usize, 1.0), (15, 1.0), (35, 1.0)];
    let b = best_bins(&d, &hum, &slot_masses(&d, cell.iter().copied()));
    assert_eq!((b.nominal, b.len(), b.cuts.clone()), (2, 2, vec![20]));
    // A factor above 5x rules a partition out (5.5 here), so the cell falls back to b = 1.
    let d2 = [(10usize, 1.0), (20, 1.0)];
    let h2: Vec<f64> = d2.iter().map(|x| x.1).collect();
    assert_eq!(best_bins(&d2, &h2, &slot_masses(&d2, [(5, 10.0), (25, 1.0)])).nominal, 1);
    assert_eq!(best_bins(&d2, &h2, &slot_masses(&d2, [(5, 8.0), (25, 2.0)])).nominal, 2);
    // Exactly 5x is allowed; above it (beyond the tie tolerance) is not.
    let f = factors(&[1.0, 1.0], &[9.0, 1.0]).unwrap();
    assert!(f[1] == 5.0 && within_cap(&Some(f)));
    assert!(!within_cap(&Some(vec![1.0, 5.0 * (1.0 + 1e-9)])));
    // An exact equal-mass tie cuts in the same place at any weight scale.
    let eq = |w: f64| quantile_bins(&[(85usize, w), (100, w), (115, w), (130, w), (40, w), (55, w), (70, w), (145, w)]
        .iter().copied().collect::<BTreeMap<_, _>>().into_iter().collect::<Vec<_>>(), 2).cuts;
    for w in [1.0, 0.1, 1.0 / 3.0, std::f64::consts::FRAC_1_SQRT_2, 1e-7] {
        assert_eq!(eq(w), vec![85], "{w}");
    }
    // Qualifying is not monotone in b: here b = 3 puts 1/25 of the cell against 1/3 of the
    // humans (8.3x) while b = 4 passes and b = 5, 6 leave a bin without positives; b = 4's four
    // bins are the most any qualifying candidate has.
    let d3 = [(10usize, 1.0), (20, 1.0), (30, 1.0), (40, 1.0), (50, 1.0), (60, 1.0)];
    let h3: Vec<f64> = d3.iter().map(|x| x.1).collect();
    let s3 = slot_masses(&d3, [(15usize, 8.0), (25, 1.0), (45, 8.0), (55, 8.0)]);
    let pass = |b: usize| {
        let q = quantile_bins(&d3, b);
        within_cap(&factors(&q.masses(&h3), &q.masses(&s3)))
    };
    assert_eq!((1..=6).map(pass).collect::<Vec<_>>(), vec![true, true, false, true, false, false]);
    let best = best_bins(&d3, &h3, &s3);
    assert_eq!((best.nominal, best.len(), best.cuts), (4, 4, vec![20, 30, 50]));
    // Beyond D: 90% of the human mass at one of three word counts. No b <= D = 3 separates the
    // other two; b = 11 is the first to give the finest partition, which qualifies (factors
    // 1.5, 0.25, 0.25), so the cell takes three bins at nominal 11.
    let d4 = [(150usize, 0.9), (200, 0.05), (300, 0.05)];
    let h4: Vec<f64> = d4.iter().map(|x| x.1).collect();
    let s4 = vec![0.6, 0.2, 0.2, 0.0];
    assert!((1..=3).all(|b| quantile_bins(&d4, b).len() <= 2));
    assert_eq!((1..).find(|&b| quantile_bins(&d4, b).len() == 3), Some(11));
    assert!(11 <= search_bound(&d4));
    let best = best_bins(&d4, &h4, &s4);
    assert_eq!((best.nominal, best.len(), best.cuts.clone()), (11, 3, vec![150, 200]));
    let f = factors(&best.masses(&h4), &best.masses(&s4)).unwrap();
    for (a, b) in f.iter().zip([1.5, 0.25, 0.25]) {
        assert!((a - b).abs() < 1e-12, "{f:?}");
    }
    // Each cell of a domain chooses on its own: a cell whose finest partition has a 6.25x bin
    // keeps two bins, at the smallest nominal b that gives them.
    let both = choose_bins(&d4, &h4, &[s4.clone(), vec![0.802, 0.19, 0.008, 0.0]]);
    assert_eq!(both[0], best);
    assert_eq!((both[1].nominal, both[1].len(), both[1].cuts.clone()), (2, 2, vec![150]));
    // Ties in the realised count go to the smallest nominal b: b = 2 and b = 3 give different
    // two-bin partitions that both qualify (the finest leaves the middle bin without positives).
    let d5 = [(10usize, 0.4), (20, 0.2), (30, 0.4)];
    let h5: Vec<f64> = d5.iter().map(|x| x.1).collect();
    assert_eq!((quantile_bins(&d5, 2).cuts, quantile_bins(&d5, 3).cuts, quantile_bins(&d5, 4).len()), (vec![20], vec![10], 3));
    let s5 = vec![0.5, 0.0, 0.5, 0.0];
    for b in [2, 3] {
        let q = quantile_bins(&d5, b);
        assert!(within_cap(&factors(&q.masses(&h5), &q.masses(&s5))), "b {b}");
    }
    let best = best_bins(&d5, &h5, &s5);
    assert_eq!((best.nominal, best.cuts), (2, vec![20]));
}

fn texted(mut r: Row, words: usize) -> Row {
    r.text = words_text(&r.id, words);
    r
}

#[test]
fn raking_reproduces_target_bins_to_1e12_and_keeps_cell_masses() {
    use crate::bundle::Round2;
    use crate::covariate::{COVARIATE_RULE, R0_ROW_CAP, quantile_bins};
    use crate::weights::{Round2Train, assign_weights_with};
    use SourceKind::*;
    let _floor = covariate::test_floor::set(0.0);
    let mut rows = Vec::new();
    for (i, n) in [40usize, 55, 70, 85, 100, 115, 130, 145].iter().enumerate() {
        rows.push(texted(row(&format!("h{i}"), "A", &format!("g{i}"), "human_only", None, "human:c", Human), *n));
    }
    for (i, n) in [42usize, 44, 46, 60, 75, 140, 160, 30, 99, 101].iter().enumerate() {
        rows.push(texted(row(&format!("m{i}"), "A", &format!("g{}", i % 8), "model_only", Some("qwen3.6"), "local:m", LocalPositive), *n));
    }
    for (i, n) in [50usize, 52, 120, 125, 128].iter().enumerate() {
        rows.push(texted(row(&format!("x{i}"), "A", &format!("g{}", i % 8), "mixed", Some("phi4"), "local:x", LocalPositive), *n));
    }
    // Domain B has positives and no humans: it is not raked.
    rows.push(texted(row("b1", "B", "gb", "model_only", Some("qwen3.6"), "local:m", LocalPositive), 50));
    let mut off = rows.clone();
    assign_weights(&mut off, true, &CohortWeights::default(), &Weighting::default()).unwrap();
    let opts = Round2 { covariate_balance: Some(COVARIATE_RULE.into()), ..Default::default() };
    let r2 = Round2Train { opts: &opts, writer_keys: &[] };
    let w = assign_weights_with(&mut rows, true, &CohortWeights::default(), &Weighting::default(), Some(&r2)).unwrap();
    assert_eq!(w["scheme"], weights::TRAIN_SCHEME_R2);
    let cov = &w["round2"]["covariate_balance"];
    assert_eq!(cov["rule"], COVARIATE_RULE);
    assert!(cov["raked_max_abs_deviation"].as_f64().unwrap() <= 1e-12);
    assert_eq!(cov["unraked_cells"][0]["domain"], "B");
    // Cell masses and every human weight are unchanged.
    for origin in ORIGINS {
        for d in ["A", "B"] {
            let on: f64 = rows.iter().filter(|r| r.origin == origin && r.domain == d).map(|r| r.weight).sum();
            let of: f64 = off.iter().filter(|r| r.origin == origin && r.domain == d).map(|r| r.weight).sum();
            assert!((on - of).abs() <= 1e-12 * of.max(1.0), "{d} {origin}");
        }
    }
    for (a, b) in rows.iter().zip(&off) {
        if a.origin == "human_only" {
            assert!((a.weight - b.weight).abs() <= 1e-12);
        }
    }
    // Recompute the bins from the report: each cell's bin shares equal the humans' to 1e-12.
    let hum: Vec<&Row> = rows.iter().filter(|r| r.domain == "A" && r.origin == "human_only").collect();
    let hd = covariate::human_distribution(hum.iter().map(|r| (grammar_a_pools::text::words(&r.text), r.weight)));
    // The search bound is ⌈T / m_min⌉ + 1 of the step-3 masses (here 8 equal masses, so the
    // ratio sits at an integer and a rescaled copy may round one up); either way every nominal
    // b from it on gives the finest partition, so the choice does not depend on it.
    let rb = cov["domains"]["A"]["search_bound_b"].as_u64().unwrap() as usize;
    assert!(rb.abs_diff(covariate::search_bound(&hd)) <= 1 && rb >= hd.len());
    assert_eq!(quantile_bins(&hd, rb).len(), hd.len());
    for origin in ["mixed", "model_only"] {
        let c = &cov["domains"]["A"]["cells"][origin];
        let b = c["nominal_b"].as_u64().unwrap() as usize;
        let bins = quantile_bins(&hd, b);
        assert_eq!(json!(bins.cuts), c["cuts_words"]);
        assert_eq!(c["bins"], json!(bins.len()));
        assert!(c["max_factor"].as_f64().unwrap() <= R0_ROW_CAP);
        let share = |rs: Vec<&Row>| {
            let mut m = vec![0.0; bins.len()];
            for r in &rs {
                m[bins.of(grammar_a_pools::text::words(&r.text))] += r.weight;
            }
            let s: f64 = m.iter().sum();
            m.into_iter().map(|x| x / s).collect::<Vec<f64>>()
        };
        let pos = share(rows.iter().filter(|r| r.domain == "A" && r.origin == origin).collect());
        let hs = share(hum.clone());
        for (p, h) in pos.iter().zip(&hs) {
            assert!((p - h).abs() <= 1e-12, "{origin}: {p} vs {h}");
        }
        assert_eq!(cov["bin_counts_s0_14"]["A"][origin], json!({"bins": bins.len(), "nominal_b": b, "cuts_words": bins.cuts}));
        // The bin rule, recomputed over every candidate: no qualifying partition has more bins,
        // and none with as many comes at a smaller nominal b.
        let slots = covariate::slot_masses(&hd, off.iter().filter(|r| r.domain == "A" && r.origin == origin)
            .map(|r| (grammar_a_pools::text::words(&r.text), r.weight)));
        let hm: Vec<f64> = hd.iter().map(|x| x.1).collect();
        for bb in 1..=covariate::search_bound(&hd) {
            let q = quantile_bins(&hd, bb);
            let ok = covariate::within_cap(&covariate::factors(&q.masses(&hm), &q.masses(&slots)));
            assert!(!ok || q.len() < bins.len() || (q.len() == bins.len() && bb >= b), "{origin}: b {bb} beats the choice b {b}");
        }
    }
    // Hosted rows: the label step runs after raking, as in R0; label totals stay exact and
    // the final bin shares are reported next to the raked ones.
    let f = "hosted_frontier_v1";
    rows.push(texted(row("fa", "A", "g1", "mixed", Some("requested:gpt-6-astra"), f, Frontier), 64));
    rows.push(texted(row("fo", "A", "g2", "mixed", Some("claude-opus-5-5"), f, Frontier), 132));
    rows.push(texted(row("fo2", "A", "g3", "mixed", Some("claude-opus-5-5"), f, Frontier), 90));
    let w = assign_weights_with(&mut rows, true, &CohortWeights::default(), &Weighting::default(), Some(&r2)).unwrap();
    assert!(w["label_fit"]["cohorts"][f]["max_relative_deviation_from_label_target"].as_f64().unwrap() < 1e-9);
    let cov = &w["round2"]["covariate_balance"];
    assert!(cov["raked_max_abs_deviation"].as_f64().unwrap() <= 1e-12);
    assert!(cov["domains"]["A"]["cells"]["mixed"]["final_max_abs_deviation"].is_number());
    assert!(cov["domains"]["A"]["cells"]["mixed"]["trainer_weighting_max_abs_deviation"].is_number());
    // An unknown rule is refused.
    let bad = Round2 { covariate_balance: Some("a_covariate_balance_v0".into()), ..Default::default() };
    assert!(assign_weights_with(&mut rows, true, &CohortWeights::default(), &Weighting::default(),
        Some(&Round2Train { opts: &bad, writer_keys: &[] })).is_err());
}

#[test]
fn raking_targets_weigh_humans_by_step3_weight_and_skip_zero_weight_humans() {
    use crate::bundle::Round2;
    use crate::covariate::{COVARIATE_RULE, quantile_bins};
    use crate::weights::{Round2Train, assign_weights_with};
    use SourceKind::*;
    let _floor = covariate::test_floor::set(0.0);
    // Humans of unequal step-3 weight: groups g1 and g3 hold one human each, g2 three, so the
    // word counts 40 and 120 carry 1/3 of the human mass each and 60, 80, 100 carry 1/9.
    let mut rows = vec![
        texted(row("h1", "A", "g1", "human_only", None, "human:c", Human), 40),
        texted(row("h2", "A", "g2", "human_only", None, "human:c", Human), 60),
        texted(row("h3", "A", "g2", "human_only", None, "human:c", Human), 80),
        texted(row("h4", "A", "g2", "human_only", None, "human:c", Human), 100),
        texted(row("h5", "A", "g3", "human_only", None, "human:c", Human), 120),
        // Zero-weight humans (an ELLIPSE-like cohort at factor 0) outside the others' range.
        texted(row("z1", "A", "g6", "human_only", None, "human:z", Human), 200),
        texted(row("z2", "A", "g7", "human_only", None, "human:z", Human), 220),
    ];
    for (i, n) in [40usize, 60, 80, 100, 120].iter().enumerate() {
        rows.push(texted(row(&format!("m{i}"), "A", &format!("p{i}"), "model_only", Some("qwen3.6"), "local:m", LocalPositive), *n));
    }
    // A heavy mixed row (alone in its group, against 20 rows in another) for the trainer-cap report.
    rows.push(texted(row("x0", "A", "q0", "mixed", Some("phi4"), "local:x", LocalPositive), 40));
    for i in 0..20 {
        rows.push(texted(row(&format!("x{}", i + 1), "A", "q1", "mixed", Some("phi4"), "local:x", LocalPositive), 50 + 3 * i));
    }
    let mut cw = CohortWeights::default();
    cw.cohorts.insert("human:z".into(), 0.0);
    let opts = Round2 { covariate_balance: Some(COVARIATE_RULE.into()), ..Default::default() };
    let w = assign_weights_with(&mut rows, true, &cw, &Weighting::default(), Some(&Round2Train { opts: &opts, writer_keys: &[] })).unwrap();
    let cov = &w["round2"]["covariate_balance"];
    let dom = &cov["domains"]["A"];
    assert_eq!((dom["human_rows"].clone(), dom["distinct_human_word_counts"].clone()), (json!(5), json!(5)));
    assert_eq!(dom["human_word_count_range"], json!([40, 120]));
    // The model-only cell (one positive per human word count, 1/5 each) takes the finest
    // partition: factors 5/3 and 5/9 are within 5x.
    let m = &dom["cells"]["model_only"];
    assert_eq!(m["bins"], 5);
    assert_eq!(m["cuts_words"], json!([40, 60, 80, 100]));
    let close = |v: &Value, want: &[f64]| {
        let got: Vec<f64> = v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        assert!(got.len() == want.len() && got.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-12), "{got:?} vs {want:?}");
    };
    close(&m["human_target_shares"], &[1.0 / 3.0, 1.0 / 9.0, 1.0 / 9.0, 1.0 / 9.0, 1.0 / 3.0]);
    close(&m["factors"], &[5.0 / 3.0, 5.0 / 9.0, 5.0 / 9.0, 5.0 / 9.0, 5.0 / 3.0]);
    let hd = covariate::human_distribution(rows.iter().filter(|r| r.origin == "human_only")
        .map(|r| (grammar_a_pools::text::words(&r.text), r.weight)));
    assert_eq!(hd.len(), 5);
    assert_eq!(m["nominal_b"], json!((1..).find(|&b| quantile_bins(&hd, b).len() == 5).unwrap()));
    // Trainer caps on the final weights: the heavy row (1/3 of the mixed cell after raking,
    // 7x the mixed mean) is above its class cap; the report counts it, and under the trainer's
    // weighting the mixed cell's bin shares leave their targets.
    let live: Vec<&Row> = rows.iter().filter(|r| r.weight > 0.0).collect();
    let gm = live.iter().map(|r| r.weight).sum::<f64>() / live.len() as f64;
    let cm = |o: &str| {
        let v: Vec<f64> = live.iter().filter(|r| r.origin == o).map(|r| r.weight).collect();
        v.iter().sum::<f64>() / v.len() as f64
    };
    let above = |r: &Row, cap: f64| r.weight > cap * (1.0 + 1e-12);
    let class_rows = live.iter().filter(|r| above(r, 5.0 * cm(&r.origin))).count();
    let global_rows = live.iter().filter(|r| above(r, 5.0 * gm)).count();
    assert!(above(rows.iter().find(|r| r.id == "x0").unwrap(), 5.0 * cm("mixed")));
    let caps = &cov["above_trainer_caps"];
    assert_eq!((caps["all"]["rows_above_class_cap"].clone(), caps["all"]["rows_above_global_cap"].clone()), (json!(class_rows), json!(global_rows)));
    assert_eq!(dom["cells"]["mixed"]["above_trainer_caps"]["rows_above_class_cap"], 1);
    let excess = rows.iter().find(|r| r.id == "x0").unwrap().weight - 5.0 * cm("mixed");
    let total: f64 = live.iter().map(|r| r.weight).sum();
    assert!((dom["cells"]["mixed"]["above_trainer_caps"]["mass_above_class_cap"].as_f64().unwrap() - excess / total).abs() < 1e-12);
    assert!(dom["cells"]["mixed"]["trainer_weighting_max_abs_deviation"].as_f64().unwrap() > 1e-6);
    assert!(dom["cells"]["model_only"]["trainer_weighting_max_abs_deviation"].as_f64().unwrap() < 1e-12);
}

/// Synthetic Train weights (origin index in `ORIGINS`, cohort, weight): every class above
/// R0's caps somewhere, zero-weight humans, and RAID rows above 20% of the AI-involved mass.
fn ess_fixture_a() -> Vec<(usize, &'static str, f64)> {
    (0..3000usize)
        .map(|i| {
            let py = i % 3; // full_a origin ints: 0 human, 1 model-only, 2 mixed
            let o = [0, 2, 1][py];
            let mut w = 1.0 + ((i * 37) % 101) as f64 / 10.0;
            if i % 97 == 0 {
                w *= 40.0;
            }
            if i % 11 == 0 && py == 0 {
                w = 0.0;
            }
            let c = if py != 0 && i % 4 == 0 { "raid-v1-x" } else if py == 0 { "human:c" } else { "local:a" };
            (o, c, w)
        })
        .collect()
}

/// 24,000 humans and 10,000 model-only rows at weight 1, and 40 model-only rows at 20.
fn ess_fixture_b() -> Vec<(usize, &'static str, f64)> {
    let mut v = vec![(0usize, "human:c", 1.0); 24_000];
    v.extend(vec![(2usize, "local:a", 1.0); 10_000]);
    v.extend(vec![(2usize, "local:a", 20.0); 40]);
    v
}

#[test]
fn trainer_weighting_ports_the_trainer_recipe_and_its_caps_decide_the_refusal() {
    use crate::covariate::*;
    let digest = |t: &TrainerWeights, rows: &[(usize, &str, f64)]| {
        let mut b = Vec::new();
        for (x, r) in t.w.iter().zip(rows) {
            if r.2 > 0.0 {
                b.extend_from_slice(&x.to_le_bytes());
            }
        }
        sha_hex(&b)
    };
    // Expected values: full_a_bundle.train_weights under R0's train_weighting and the
    // trainer's mass-report statistics, run by CPython 3.12.15 on the same synthetic weights
    // (unit slop-a-comp-r2-b5a-fix-pyref). The port matches bit for bit.
    let a = ess_fixture_a();
    let t = trainer_weighting(&a).unwrap();
    assert_eq!(t.rows, 2909);
    assert_eq!(t.ess, 2111.055851927485);
    assert_eq!(t.ess_uncapped, 267.91418444068574);
    assert_eq!((t.max_over_class_mean, t.max_over_global_mean), (5.000000000000001, 5.000000000000001));
    assert_eq!(t.raid_share_of_ai_involved, 0.20000000000429508);
    assert_eq!(digest(&t, &a), "c44bbe694c464bf29ce9f4020e25c4be172edafaefe4149f93c8b47f40bbbe1c");
    assert!(a.iter().zip(&t.w).all(|(r, x)| (r.2 == 0.0) == (*x == 0.0)));
    // The caps decide the refusal: B's uncapped Kish ESS (24,220.8) is below R0's floor, its
    // ESS under the trainer's weighting (33,388.02) is above it, so B passes.
    let b = ess_fixture_b();
    let t = trainer_weighting(&b).unwrap();
    assert_eq!((t.rows, t.ess, t.ess_uncapped, t.max_over_global_mean), (34_040, 33388.02492254004, 24220.8, 5.0));
    assert_eq!(digest(&t, &b), "8f891a56b6f0c9fdd9a66e0e5ed833d6ab37c9d57e2bcb5a23d6a3ec4628805c");
    assert!(!passes_floor(t.ess_uncapped, R0_KISH_ESS_FLOOR) && !passes_floor(kish_ess(b.iter().map(|x| x.2)).1, R0_KISH_ESS_FLOOR));
    let v = ess_check(&t).unwrap();
    assert_eq!((v["passes_floor"].clone(), v["floor"].clone(), v["ess_rounded"].clone()), (json!(true), json!(32_827.0), json!(33_388.0)));
    assert_eq!(v["train_weighting"], trainer_weighting_json());
    // Below the floor the bundle is refused.
    let t = trainer_weighting(&b[..30_000]).unwrap();
    assert!((t.ess - 30_000.0).abs() < 1e-6);
    assert!(format!("{:#}", ess_check(&t).unwrap_err()).contains("refused"));
    // V24 at the precision it states R0's value: R0's own Train passes.
    assert_eq!(R0_KISH_ESS_FLOOR, 32_827.0);
    assert!(passes_floor(R0_KISH_ESS_MEASURED, R0_KISH_ESS_FLOOR));
    assert!(passes_floor(32_826.5, R0_KISH_ESS_FLOOR) && !passes_floor(32_826.499_999, R0_KISH_ESS_FLOOR));
    // Python's float sum (Neumaier): exact where plain addition is not.
    assert_eq!(py_sum([1e16, 1.0, -1e16]), 1.0);
    assert_eq!(py_sum(std::iter::empty()), 0.0);
    assert_eq!(kish_ess([1.0, 1.0, 1.0, 1.0]), (4, 4.0));
    assert_eq!(kish_ess([1.0, 3.0, 0.0]), (2, 1.6));
}

#[test]
fn round2_builds_always_refuse_below_r0_ess_and_carry_round2_schemas() {
    use crate::covariate::*;
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let base = r2_fixture(d);
    // Any one round-2 option makes a round-2 build, refused below R0's ESS (this fixture has ~30 rows).
    for (name, r2) in [
        ("cov", json!({"covariate_balance": COVARIATE_RULE})),
        ("blocks", json!({"block_tags": {"c_collections": ["cc_pilot_historical"]}})),
        ("roles", json!({"cohort_roles": [{"role": "base_model", "cohort_prefixes": ["local:base_"]}]})),
    ] {
        let mut cfg = base.clone();
        cfg["round2"] = r2;
        let e = format!("{:#}", build_fixture(d, &cfg, name).unwrap_err());
        assert!(e.contains("Kish ESS") && e.contains("refused"), "{name}: {e}");
    }
    // The refusal has no switch.
    let mut cfg = base.clone();
    cfg["round2"] = json!({"ess_floor_r0": true});
    assert!(build_fixture(d, &cfg, "switch").is_err());
    // With the floor lowered for this fixture: the report and the round-2 schemas.
    let _floor = test_floor::set(0.0);
    let mut cfg = base.clone();
    cfg["round2"] = json!({"covariate_balance": COVARIATE_RULE});
    let build = build_fixture(d, &cfg, "ess-report").unwrap();
    let w: Value = serde_json::from_slice(&fs::read(build.join("train/weights.json")).unwrap()).unwrap();
    assert_eq!(w["scheme"], weights::TRAIN_SCHEME_R2);
    let e = &w["round2"]["kish_ess"];
    let rows: Vec<Value> = fs::read_to_string(build.join("train/rows.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let tuples: Vec<(usize, &str, f64)> = rows.iter()
        .map(|r| (ORIGINS.iter().position(|o| r["origin"] == *o).unwrap(), r["cohort_id"].as_str().unwrap(), r["weight"].as_f64().unwrap()))
        .collect();
    let tw = trainer_weighting(&tuples).unwrap();
    assert_eq!((e["rows_positive_weight"].as_u64().unwrap() as usize, e["ess"].as_f64().unwrap()), (tw.rows, tw.ess));
    assert_eq!(e["ess_assembler_weights_uncapped"].as_f64().unwrap(), tw.ess_uncapped);
    assert_eq!((e["floor"].clone(), e["passes_floor"].clone()), (json!(0.0), json!(true)));
    assert!(w["round2"]["human_share_of_document_loss_mass"].as_f64().unwrap() > 0.0);
    assert!(w["round2"]["covariate_balance"]["above_trainer_caps"]["all"].is_object());
    for split in ["train", "dev-iter"] {
        let m: Value = serde_json::from_slice(&fs::read(build.join(split).join("MANIFEST.json")).unwrap()).unwrap();
        assert_eq!(m["row_schema"], ROW_SCHEMA_R2, "{split}");
        for l in fs::read_to_string(build.join(split).join("rows.jsonl")).unwrap().lines() {
            assert_eq!(serde_json::from_str::<Value>(l).unwrap()["schema"], ROW_SCHEMA_R2, "{split}");
        }
    }
    // The `ess` report of the split reads the same rows.
    let rep = supervision::train_ess(&build.join("train"), &json!({"assembler_sha256": "test"})).unwrap();
    assert_eq!(rep["kish_ess"]["ess"], e["ess"]);
    assert_eq!(rep["input"]["row_schema"], ROW_SCHEMA_R2);
}

#[test]
fn round2_report_gives_the_raid_share_of_ai_involved_mass() {
    use crate::bundle::Round2;
    use crate::weights::{Round2Train, assign_weights_with};
    use SourceKind::*;
    let _floor = covariate::test_floor::set(0.0);
    let mut rows = vec![
        row("h1", "A", "g1", "human_only", None, "human:c", Human),
        row("h2", "A", "g2", "human_only", None, "human:c", Human),
        row("r1", "A", "g1", "model_only", Some("gpt-4-0613"), "raid-v1-books", ExternalPool),
        row("m1", "A", "g2", "model_only", Some("qwen3.6"), "local:m", LocalPositive),
        row("x1", "A", "g1", "mixed", Some("phi4"), "local:x", LocalPositive),
    ];
    let opts = Round2::default();
    let w = assign_weights_with(&mut rows, true, &CohortWeights::default(), &Weighting::default(),
        Some(&Round2Train { opts: &opts, writer_keys: &[] })).unwrap();
    let raid = &w["round2"]["raid"];
    // The domain's three cells hold a third each and RAID half the model-only cell: 1/6 of all
    // mass, 1/4 of the AI-involved mass, above the 20% cap; the trainer holds it at 20%.
    assert!((raid["ai_involved_mass_share_before_trainer_caps"].as_f64().unwrap() - 0.25).abs() < 1e-12);
    assert_eq!(raid["trainer_cap_binds"], true);
    assert!((raid["ai_involved_mass_share_trainer_weighting"].as_f64().unwrap() - 0.2).abs() < 1e-9);
}

fn counts(h: u64, p: u64, frontier: &[(&str, u64)], gens: &[(&str, u64)], coll: &str) -> supervision::Counts {
    let mut c = supervision::Counts { humans: h, positives: p, ..Default::default() };
    for (l, n) in frontier {
        c.frontier += n;
        c.frontier_by_label.insert(l.to_string(), *n);
    }
    for (g, n) in gens {
        c.by_generator.insert(g.to_string(), *n);
    }
    if h > 0 {
        c.human_collections.insert(coll.into());
    }
    c
}

fn sup_config() -> Value {
    json!({"schema": supervision::CONFIG_SCHEMA, "domains": ["legal", "wiki", "enron_email"],
        "tier_c_test_mass": {"legal": 1.0, "wiki": 3.0, "fineweb": 4.0},
        "cc_shares": {"legal": 0.05, "news": 0.95}, "cc_stratum_domains": {"legal": "legal"},
        "enron_domains": ["enron_email"], "rule3_generators": ["qwen", "phi"],
        "planned_humans": {"enron_email": 764}})
}

#[test]
fn supervision_targets_follow_rules_1_to_4_to_the_least_fixed_point() {
    use supervision::*;
    let mut c = BTreeMap::new();
    c.insert("legal".to_string(), counts(100, 50, &[("requested:gpt-6-astra", 10)], &[("qwen", 40), ("requested:gpt-6-astra", 10)], "courtlistener_historical"));
    c.insert("wiki".to_string(), counts(300, 300, &[("claude-opus-5-5", 70)], &[("qwen", 230), ("claude-opus-5-5", 70)], "wikipedia_enwiki_historical"));
    c.insert("other".to_string(), counts(600, 400, &[], &[("qwen", 400)], "x"));
    let cfg: SupervisionConfig = serde_json::from_value(sup_config()).unwrap();
    let v = targets(&c, &cfg).unwrap();
    // Hand computation: m_legal = 1/8 / 2 + 0.05 / 2 = 0.0875, m_wiki = 0.1875, Enron 0.
    // Step A rows: legal 100 + max(77, 50 + 12 + 78) = 240; wiki 300 + 359; Enron 764 + 581; other 1000.
    // Rule 4 raises legal's humans 100 -> 136 -> 139 -> 140 over N = 3244, 3288, 3292, 3293.
    let l = &v["domains"]["legal"];
    assert_eq!(l["targets"], json!({"humans": 140, "positives": 149, "frontier_records": 31, "rows": 289,
        "rule1_min_positives": 107, "rule2_min_frontier": 31, "rule3_min_rows_per_generator": 59, "rule4_min_rows": 289,
        "positives_without_humans": false}));
    assert_eq!(l["add"]["humans"], 40);
    assert_eq!(l["add"]["frontier_records"], 21);
    // The frontier total only: its label allocation is B5b's top-up plan (amendment 5 item 5.3).
    assert!(l["add"].get("frontier_by_label").is_none());
    assert!(v["frontier_label_allocation"].as_str().unwrap().contains("B5b"));
    assert_eq!(l["current"]["frontier_by_label"], json!({"requested:gpt-6-astra": 10}));
    assert_eq!(l["add"]["local_positives"], 78);
    assert_eq!(l["add"]["local_for_generator_floor"], json!({"phi": 59, "qwen": 19}));
    assert_eq!(l["binding"], json!({"rule1": false, "rule2": true, "rule3": ["phi", "qwen"], "rule4": true}));
    assert!((l["eval_mass_share"].as_f64().unwrap() - 0.0875).abs() < 1e-15);
    let w = &v["domains"]["wiki"];
    assert_eq!((w["targets"]["humans"].clone(), w["targets"]["positives"].clone(), w["targets"]["rows"].clone()), (json!(300), json!(359), json!(659)));
    assert_eq!(w["binding"]["rule4"], false);
    // Enron: planned humans count as current, rule 2 is waived and the waiver recorded.
    let e = &v["domains"]["enron_email"];
    assert_eq!(e["targets"]["humans"], 764);
    assert_eq!(e["targets"]["positives"], 581);
    assert_eq!(e["targets"]["rule2_min_frontier"], Value::Null);
    assert_eq!(e["binding"]["rule2"], "waived");
    assert_eq!(e["rule2_waiver"], ENRON_WAIVER);
    assert_eq!(e["add"]["frontier_records"], 0);
    assert_eq!(v["rows"]["target_all_domains"], 3293);
    assert_eq!(v["rule4_passes"], 4);
    assert_eq!(v["tier_c_test_domains_not_covered"], json!(["fineweb"]));
    assert_eq!(v["cc_strata_not_mapped"], json!(["news"]));
    // Every rule holds at the targets, against the final N.
    let n = v["rows"]["target_all_domains"].as_f64().unwrap();
    for (d, x) in v["domains"].as_object().unwrap() {
        let t = &x["targets"];
        let (h, p, rows) = (t["humans"].as_u64().unwrap(), t["positives"].as_u64().unwrap(), t["rows"].as_u64().unwrap());
        assert!(p * RULE1.1 >= h * RULE1.0, "{d} rule 1");
        assert!(rows as f64 >= x["eval_mass_share"].as_f64().unwrap() * n, "{d} rule 4");
        if x["binding"]["rule2"] != "waived" {
            assert!(t["frontier_records"].as_u64().unwrap() * RULE2.1 >= h * RULE2.0, "{d} rule 2");
        }
    }
    // Refusals: Enron humans outside an Enron domain; other humans inside one; evaluation
    // shares that cannot be met; the removed per-label declaration.
    let mut c2 = c.clone();
    c2.get_mut("legal").unwrap().human_collections.insert(grammar_a_pools::pile::ENRON_COLLECTION.into());
    assert!(targets(&c2, &cfg).is_err());
    let mut c3 = c.clone();
    c3.insert("enron_email".into(), counts(5, 0, &[], &[], "plos_historical"));
    assert!(targets(&c3, &cfg).is_err());
    let mut full = sup_config();
    full["cc_shares"] = json!({"legal": 1.0});
    full["tier_c_test_mass"] = json!({"legal": 1.0});
    assert!(format!("{:#}", targets(&c, &serde_json::from_value(full).unwrap()).unwrap_err()).contains("rule 4"));
    let mut labels = sup_config();
    labels["frontier_labels"] = json!(["requested:gpt-6.1-terra"]);
    assert!(serde_json::from_value::<SupervisionConfig>(labels).is_err());
}

#[test]
fn supervision_counts_planned_positives_in_their_generator_cells() {
    use supervision::*;
    let mut c = BTreeMap::new();
    c.insert("legal".to_string(), counts(100, 50, &[], &[("qwen", 40), ("other", 10)], "courtlistener_historical"));
    c.insert("rest".to_string(), counts(600, 400, &[], &[("qwen", 400)], "x"));
    let cfg: SupervisionConfig = serde_json::from_value(json!({"schema": CONFIG_SCHEMA, "domains": ["legal"],
        "tier_c_test_mass": {"legal": 1.0, "rest": 99.0}, "cc_shares": {"news": 1.0},
        "rule3_generators": ["qwen", "phi"], "planned_positives": {"legal": {"phi": 30, "qwen": 5}}})).unwrap();
    let v = targets(&c, &cfg).unwrap();
    let l = &v["domains"]["legal"];
    // With the plan the cells hold qwen 40 + 5 and phi 0 + 30, so rule 3 adds 14 and 29 (43).
    assert_eq!(l["add"]["local_for_generator_floor"], json!({"phi": 29, "qwen": 14}));
    // Rule 2: ⌈100 × 10,713 / 49,179⌉ = 22 frontier records. Positives: max(⌈100 × 37,396 /
    // 49,179⌉ = 77, 50 current + 35 planned + 22 + 43) = 150.
    assert_eq!((l["targets"]["frontier_records"].clone(), l["targets"]["positives"].clone()), (json!(22), json!(150)));
    assert_eq!((l["add"]["positives"].clone(), l["add"]["local_positives"].clone()), (json!(65), json!(43)));
    assert_eq!(l["current"]["generator_rows"], json!({"phi": 0, "qwen": 40}));
    assert_eq!(l["planned"]["positives"], json!({"phi": 30, "qwen": 5}));
    assert_eq!(l["binding"]["rule4"], false);
}

#[test]
fn supervision_counts_only_frontier_source_rows_as_frontier_records() {
    let pos = |kind: &str, label: &str, w: f64| json!({"weight": w, "origin": "model_only", "generator_label": label, "source_kind": kind});
    let mut c = supervision::Counts::default();
    for r in [
        pos("frontier", "requested:gpt-6.1-terra", 1.0),
        pos("frontier", "requested:gpt-6-astra", 2.0),
        pos("frontier", "requested:gpt-6-astra", 0.0),
        pos("external_pool", "gpt-4-0613", 1.0),
        pos("stage3_legacy", "qwen3.6", 1.0),
        pos("hosted_capture", "claude-opus-5-5", 1.0),
        pos("local_positive", "qwen3.6", 1.0),
        json!({"weight": 1.0, "origin": "human_only", "collection": "plos_historical"}),
        json!({"weight": 0.0, "origin": "human_only", "collection": "ellipse_historical"}),
    ] {
        c.add(&r).unwrap();
    }
    // RAID (external pool), stage-3 and capture positives are positives, not frontier records.
    assert_eq!((c.humans, c.humans_zero_weight, c.positives, c.positives_zero_weight, c.frontier), (1, 1, 6, 1, 2));
    assert_eq!(c.frontier_by_label, BTreeMap::from([("requested:gpt-6-astra".to_string(), 1), ("requested:gpt-6.1-terra".to_string(), 1)]));
    assert_eq!(c.by_generator.get("qwen3.6"), Some(&2));
    assert_eq!(c.human_collections, ["plos_historical".to_string()].into_iter().collect());
}

#[test]
fn supervision_counts_positive_weight_train_rows_only_and_writes_once() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let mut cfg = r2_fixture(d);
    // An ELLIPSE-like cohort at weight 0 in the plos domain: it must count for nothing.
    let mut humans: Vec<Value> = fs::read_to_string(d.join("train.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    for i in 0..3 {
        let mut h = human(&format!("z{i}"), "train", &format!("gz{i}"), &words_text(&format!("Essay {i}"), 50));
        h["collection"] = json!("ellipse_historical");
        humans.push(h);
    }
    write_jsonl(&d.join("train.jsonl"), &humans);
    write_jsonl(&d.join("train-domains.jsonl"), &[json!({"id": "h-g6", "domain": "wikipedia"}), json!({"id": "h-g7", "domain": "wikipedia"}),
        json!({"id": "h-g8", "domain": "wikipedia"}), json!({"id": "z0", "domain": "plos"}), json!({"id": "z1", "domain": "plos"}), json!({"id": "z2", "domain": "plos"})]);
    cfg["cohort_weights"]["cohorts"]["human:ellipse_historical"] = json!(0.0);
    let build = build_fixture(d, &cfg, "sup").unwrap();
    let sc = json!({"schema": supervision::CONFIG_SCHEMA, "domains": ["plos", "wikipedia"],
        "tier_c_test_mass": {"plos": 1.0, "wikipedia": 1.0}, "cc_shares": {"news": 1.0},
        "rule3_generators": ["qwen3.6"]});
    fs::write(d.join("sup.json"), serde_json::to_vec(&sc).unwrap()).unwrap();
    let code = json!({"assembler_sha256": "test"});
    let out = d.join("S0-8.json");
    let r = supervision::run(&build.join("train"), &d.join("sup.json"), &out, &code).unwrap();
    let v: Value = serde_json::from_slice(&fs::read(&out).unwrap()).unwrap();
    assert_eq!(r["sha256"], sha_hex(&fs::read(&out).unwrap()));
    let p = &v["domains"]["plos"]["current"];
    // plos: h-g1..h-g5 and h-g4b with positive weight; the three essays at weight 0.
    assert_eq!((p["humans_positive_weight"].clone(), p["humans_zero_weight"].clone()), (json!(6), json!(3)));
    assert_eq!(p["frontier_records"], 5);
    assert_eq!(p["generator_rows"]["qwen3.6"], 8);
    assert_eq!(v["inputs"]["train"]["rows_sha256"], file_digest(&build.join("train/rows.jsonl")));
    assert!(fs::read_to_string(&out).unwrap().find("Human g1").is_none(), "no text in the output");
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // Written once; only an unsealed Train split directory is read.
    assert!(supervision::run(&build.join("train"), &d.join("sup.json"), &out, &code).is_err());
    assert!(supervision::run(&build.join("dev-iter"), &d.join("sup.json"), &d.join("x.json"), &code).is_err());
    assert!(!d.join("x.json").exists());
    // A Train manifest whose rows hold a non-Train row (digest and count updated to match):
    // the read stops at that row, and nothing is written.
    let bad = d.join("bad-train");
    fs::create_dir_all(&bad).unwrap();
    let train_rows = fs::read_to_string(build.join("train/rows.jsonl")).unwrap();
    let lines: Vec<&str> = train_rows.lines().collect();
    let dev_rows = fs::read_to_string(build.join("dev-iter/rows.jsonl")).unwrap();
    let mut mixed: Vec<&str> = lines[..2].to_vec();
    mixed.push(dev_rows.lines().next().unwrap());
    mixed.extend(&lines[2..]);
    fs::write(bad.join("rows.jsonl"), mixed.join("\n") + "\n").unwrap();
    let mut m: Value = serde_json::from_slice(&fs::read(build.join("train/MANIFEST.json")).unwrap()).unwrap();
    m["files"]["rows.jsonl"] = json!(file_digest(&bad.join("rows.jsonl")));
    m["rows"] = json!(lines.len() + 1);
    fs::write(bad.join("MANIFEST.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    let e = format!("{:#}", supervision::run(&bad, &d.join("sup.json"), &d.join("bad.json"), &code).unwrap_err());
    assert!(e.contains("line 3 is not a Train row"), "{e}");
    assert!(!d.join("bad.json").exists());
    assert!(format!("{:#}", supervision::train_ess(&bad, &code).unwrap_err()).contains("not a Train row"));
}

/// The fixture plus a CC cohort (humans, children and a hosted frontier child), Enron
/// Train-half humans with a child, and a base-model child.
fn r2_roles_fixture(d: &Path) -> Value {
    let mut cfg = r2_fixture(d);
    let mut humans: Vec<Value> = fs::read_to_string(d.join("train.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let mut locals: Vec<Value> = fs::read_to_string(d.join("local.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let mut fr: Vec<Value> = fs::read_to_string(d.join("fx/all/records.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    for i in 0..3 {
        let mut h = human(&format!("cc{i}"), "train", &format!("gc{i}"), &words_text(&format!("Page {i}"), 60 + 10 * i));
        h["collection"] = json!("cc_pilot_historical");
        humans.push(h);
        let mut m = local(&format!("ccm{i}"), "train", &format!("cc{i}"), &format!("gc{i}"), &words_text(&format!("Page child {i}"), 65), "mirror", Some("v-train"));
        m["cohort"] = json!("cc_n2_mirror");
        // The last child keeps a non-CC collection: it is C through its parent alone.
        if i < 2 {
            m["collection"] = json!("cc_pilot_historical");
        }
        locals.push(m);
    }
    // N2's hosted frontier children are self-weighting too (no relative label weights).
    let mut f = frontier("fcc0", "train", "cc0", "gc0", &words_text("Page frontier child", 66), "requested:gpt-6.1-terra", "v-train", false);
    f["cohort_id"] = json!("hosted_cc_n2_v1");
    f["collection"] = json!("cc_pilot_historical");
    fr.push(f);
    for i in 0..2 {
        let mut h = human(&format!("e{i}"), "train", &format!("ge{i}"), &words_text(&format!("Mail {i}"), 70));
        h["collection"] = json!(grammar_a_pools::pile::ENRON_COLLECTION);
        humans.push(h);
    }
    let mut em = local("em0", "train", "e0", "ge0", &words_text("Mail child", 72), "mirror", Some("v-train"));
    em["cohort"] = json!("enron_train_mirror");
    em["collection"] = json!(grammar_a_pools::pile::ENRON_COLLECTION);
    locals.push(em);
    let mut b = local("bm0", "train", "h-g2", "g2", &words_text("Base continuation", 40), "span", Some("v-train"));
    b["cohort"] = json!("base_continuation");
    locals.push(b);
    write_jsonl(&d.join("train.jsonl"), &humans);
    write_jsonl(&d.join("local.jsonl"), &locals);
    write_jsonl(&d.join("fx/all/records.jsonl"), &fr);
    fs::write(d.join("values.json"), serde_json::to_vec(&json!({"values": {"S0-9": {"train_half": {
        "writer_keys_in_plan_key_order": ["w:ge0", "w:ge1"]}}}})).unwrap()).unwrap();
    cfg["round2"] = json!({
        "cohort_roles": [
            {"role": "cc_n2", "cohort_prefixes": ["human:cc_pilot", "local:cc_n2", "hosted_cc_n2"]},
            {"role": "enron_train_half", "collections": [grammar_a_pools::pile::ENRON_COLLECTION],
             "writer_keys": {"file": d.join("values.json"), "pointer": "/values/S0-9/train_half/writer_keys_in_plan_key_order"}},
            {"role": "base_model", "cohort_prefixes": ["local:base_"]}],
        "block_tags": {"c_collections": ["cc_pilot_historical"]}});
    cfg
}

#[test]
fn round2_cohort_roles_report_self_weighting_writer_keys_and_block_tags() {
    use crate::trainer;
    let _floor = covariate::test_floor::set(0.0);
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let cfg = r2_roles_fixture(d);
    let build = build_fixture(d, &cfg, "roles").unwrap();
    let w: Value = serde_json::from_slice(&fs::read(build.join("train/weights.json")).unwrap()).unwrap();
    let roles = &w["round2"]["cohort_roles"];
    assert_eq!(roles["cc_n2"]["rows"], 7);
    assert_eq!(roles["cc_n2"]["self_weighting"], true);
    assert_eq!(roles["cc_n2"]["cohorts"]["local:cc_n2_mirror"]["declared_factor_max"], 1.0);
    assert_eq!(roles["cc_n2"]["cohorts"]["hosted_cc_n2_v1"]["rows"], 1);
    assert_eq!(roles["enron_train_half"]["rows"], 3);
    assert_eq!(roles["enron_train_half"]["writer_key_allow_list"], json!({"keys": 2, "rows_checked": 3}));
    assert_eq!(roles["base_model"]["rows"], 1);
    assert_eq!(roles["base_model"]["self_weighting"], false);
    assert!(roles["cc_n2"]["mass_share"].as_f64().unwrap() > 0.0);
    assert_eq!(w["round2"]["raid"]["prefix"], "raid-v1-");
    assert_eq!(w["round2"]["covariate_balance"], Value::Null);
    // Block tags: CC humans and their children are C (ccm2 through its parent), every other row T.
    let rows: Vec<Value> = fs::read_to_string(build.join("train/rows.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(rows.iter().find(|r| r["id"] == "ccm2").unwrap()["collection"], "plos_historical");
    for r in &rows {
        let c = r["id"].as_str().unwrap().starts_with("cc") || r["id"] == "fcc0";
        assert_eq!(r["block"], if c { "C" } else { "T" }, "{}", r["id"]);
        assert_eq!(r["schema"], ROW_SCHEMA_R2);
    }
    let m: Value = serde_json::from_slice(&fs::read(build.join("train/MANIFEST.json")).unwrap()).unwrap();
    assert_eq!(m["rows_by_block"]["C"], 7);
    assert_eq!(w["round2"]["blocks"]["C"]["rows"], 7);
    let dev: Vec<Value> = fs::read_to_string(build.join("dev-iter/rows.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(dev.iter().all(|r| r["block"] == "T"));
    // The trainer bundle carries the report and the ESS of its own Train rows under the
    // trainer's weighting; block tags are not trainer fields.
    let code = json!({"assembler_sha256": "test"});
    let tb = d.join("tb");
    trainer::trainer_bundle(&build, &["train".into(), "dev_iter".into()], &trainer::Selection::default(), &tb, &code).unwrap();
    let tw: Value = serde_json::from_slice(&fs::read(tb.join("weights.json")).unwrap()).unwrap();
    assert_eq!(tw["round2_build"], w["round2"]);
    let tuples: Vec<(usize, &str, f64)> = rows.iter()
        .map(|r| (ORIGINS.iter().position(|o| r["origin"] == *o).unwrap(), r["cohort_id"].as_str().unwrap(), r["weight"].as_f64().unwrap()))
        .collect();
    let ess = covariate::trainer_weighting(&tuples).unwrap().ess;
    assert_eq!(tw["round2_trainer_train"]["kish_ess"]["ess"].as_f64().unwrap(), ess);
    assert_eq!(tw["round2_trainer_train"]["kish_ess"], w["round2"]["kish_ess"]);
    let fields: std::collections::BTreeSet<&str> = trainer::TRAINER_FIELDS.into_iter().collect();
    for l in fs::read_to_string(tb.join("rows.jsonl")).unwrap().lines() {
        let r: Value = serde_json::from_str(l).unwrap();
        assert_eq!(r.as_object().unwrap().keys().map(String::as_str).collect::<std::collections::BTreeSet<_>>(), fields);
    }
    let tm: Value = serde_json::from_slice(&fs::read(tb.join("MANIFEST.json")).unwrap()).unwrap();
    assert!(tm["rows_by_split_block"].as_array().unwrap().iter().any(|x| x["split"] == "train" && x["block"] == "C" && x["rows"] == 7));
}

#[test]
fn round2_cohort_roles_refuse_multipliers_foreign_writers_and_overlaps() {
    let _floor = covariate::test_floor::set(0.0);
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    let cfg = r2_roles_fixture(d);
    let refused = |c: &Value, name: &str, needle: &str| {
        let e = format!("{:#}", build_fixture(d, c, name).unwrap_err());
        assert!(e.contains(needle), "{name}: {e}");
    };
    // N2's CC cohort is self-weighting: no cohort multiplier and no label factor, hosted cohorts included.
    let mut c = cfg.clone();
    c["cohort_weights"]["cohorts"]["local:cc_n2_mirror"] = json!(2.0);
    refused(&c, "cc-multiplier", "self-weighting");
    let mut c = cfg.clone();
    c["cohort_weights"]["labels"]["human:cc_pilot_historical"] = json!({"human": 0.5});
    refused(&c, "cc-label-factor", "self-weighting");
    let mut c = cfg.clone();
    c["cohort_weights"]["labels"]["hosted_cc_n2_v1"] = json!({"requested:gpt-6.1-terra": 2.0});
    refused(&c, "cc-hosted-label-factor", "self-weighting");
    // A label factor of 1 is no multiplier.
    let mut c = cfg.clone();
    c["cohort_weights"]["labels"]["hosted_cc_n2_v1"] = json!({"requested:gpt-6.1-terra": 1.0});
    build_fixture(d, &c, "cc-hosted-label-one").unwrap();
    // Enron rows only from the Train-half writers, and only inside the Enron role.
    fs::write(d.join("values-short.json"), serde_json::to_vec(&json!(["w:ge0"])).unwrap()).unwrap();
    let mut c = cfg.clone();
    c["round2"]["cohort_roles"][1]["writer_keys"] = json!({"file": d.join("values-short.json")});
    refused(&c, "foreign-writer", "allow-list");
    let mut c = cfg.clone();
    c["round2"]["cohort_roles"].as_array_mut().unwrap().remove(1);
    refused(&c, "no-enron-role", "outside a declared");
    let mut c = cfg.clone();
    c["round2"]["cohort_roles"][1].as_object_mut().unwrap().remove("writer_keys");
    refused(&c, "enron-without-keys", "allow-list");
    // Roles are exclusive, known, and stay clear of the inherited RAID cap prefix.
    let mut c = cfg.clone();
    c["round2"]["cohort_roles"][2]["cohort_prefixes"] = json!(["local:"]);
    refused(&c, "overlap", "matches round-2 cohort roles");
    let mut c = cfg.clone();
    c["round2"]["cohort_roles"][2]["cohort_prefixes"] = json!(["raid-v1-x"]);
    refused(&c, "raid", "RAID");
    let mut c = cfg.clone();
    c["round2"]["cohort_roles"][2]["role"] = json!("mystery");
    refused(&c, "unknown", "unknown round-2 cohort role");
    let mut c = cfg.clone();
    c["round2"]["block_tags"] = json!({});
    refused(&c, "empty-blocks", "block tags");
}
