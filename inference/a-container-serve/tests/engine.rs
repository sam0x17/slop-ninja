//! Engine tests on synthetic artifacts: a tiny runnable ONNX graph with the
//! scorer's graph contract (inputs `input_ids` and `attention_mask`, outputs `h`
//! and, by variant, the token states), a word-level tokenizer and small heads of
//! each schema. The graph's outputs are known in closed form, so every engine
//! output is compared with the heads runtime run directly on the same values:
//! the document outputs, the token head on the declared states, and the span
//! emission.
//!
//! The ONNX Runtime tests need `A_ORT_LIB` (the ONNX Runtime 1.31.1 shared
//! library); without it each prints a notice and passes without loading a
//! graph. The admission test needs no library. No model is loaded and nothing
//! leaves the host.
use a_container_serve::engine::{Model, TOKEN_STATES, TOWER_STATES, content_mask, word_mask};
use a_container_serve::full_a_spans;
use a_container_serve::full_a_tower::{self, SpanTowerHeads, TokenStates};
use a_container_serve::profile::sha256_hex;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const N: usize = 768;
const GRAPH: &str = "a-fp32band.onnx";
const TOWER_GOLDEN: &str =
    include_str!("../../../grammar/crates/grammar-a-baseline/tests/golden/span-tower-golden-v1.json");

fn lib() -> Option<String> {
    let lib = std::env::var("A_ORT_LIB").ok().filter(|v| !v.is_empty());
    if lib.is_none() {
        eprintln!("A_ORT_LIB is not set: ONNX Runtime engine test skipped");
    }
    lib
}

/// The test's own digest of an artifact's `contract.json` (in production the
/// header carries it, verified by the object store at upload).
fn contract_sha(dir: &Path) -> String {
    sha256_hex(&fs::read(dir.join("contract.json")).unwrap())
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("a-engine-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

// ------------------------------------------------------------------ protobuf

fn varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}
fn uint(out: &mut Vec<u8>, field: u64, v: u64) {
    varint(out, field << 3);
    varint(out, v);
}
fn bytes(out: &mut Vec<u8>, field: u64, b: &[u8]) {
    varint(out, (field << 3) | 2);
    varint(out, b.len() as u64);
    out.extend_from_slice(b);
}
fn node(op: &str, inputs: &[&str], outputs: &[&str], attrs: &[(&str, i64)]) -> Vec<u8> {
    let mut n = Vec::new();
    for i in inputs {
        bytes(&mut n, 1, i.as_bytes());
    }
    for o in outputs {
        bytes(&mut n, 2, o.as_bytes());
    }
    bytes(&mut n, 4, op.as_bytes());
    for (name, v) in attrs {
        let mut a = Vec::new();
        bytes(&mut a, 1, name.as_bytes());
        uint(&mut a, 3, *v as u64);
        uint(&mut a, 20, 2); // INT
        bytes(&mut n, 5, &a);
    }
    n
}
fn float_init(name: &str, dims: &[u64], v: &[f32]) -> Vec<u8> {
    let mut t = Vec::new();
    for d in dims {
        uint(&mut t, 1, *d);
    }
    uint(&mut t, 2, 1);
    bytes(&mut t, 8, name.as_bytes());
    bytes(&mut t, 9, &v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>());
    t
}
fn int_init(name: &str, v: &[i64]) -> Vec<u8> {
    let mut t = Vec::new();
    uint(&mut t, 1, v.len() as u64);
    uint(&mut t, 2, 7);
    bytes(&mut t, 8, name.as_bytes());
    bytes(&mut t, 9, &v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>());
    t
}
/// `dims`: Some(n) a fixed size, None the symbolic sequence length.
fn value_info(name: &str, elem: u64, dims: &[Option<u64>]) -> Vec<u8> {
    let mut shape = Vec::new();
    for d in dims {
        let mut dim = Vec::new();
        match d {
            Some(n) => uint(&mut dim, 1, *n),
            None => bytes(&mut dim, 2, b"seq"),
        }
        bytes(&mut shape, 1, &dim);
    }
    let mut tensor = Vec::new();
    uint(&mut tensor, 1, elem);
    bytes(&mut tensor, 2, &shape);
    let mut ty = Vec::new();
    bytes(&mut ty, 1, &tensor);
    let mut v = Vec::new();
    bytes(&mut v, 1, name.as_bytes());
    bytes(&mut v, 2, &ty);
    v
}

/// The synthetic graph's weights.
struct Weights {
    /// Subtracted from the masked id sum before scaling (0 for exact outputs).
    shift: f32,
    scale: f32,
    w: Vec<f32>,
    w3: Vec<f32>,
    w4: Vec<f32>,
}

fn weights(scale: f32) -> Weights {
    let row = |k: f32| (0..N).map(|j| ((j as f32 * 0.37 + k).sin()) * 0.5).collect::<Vec<f32>>();
    Weights { shift: 0.0, scale, w: row(0.1), w3: row(1.3), w4: row(2.9) }
}

/// h = (sum of masked ids - shift) * scale * W; token_states_i = id_i * mask_i * scale * W3;
/// span_tower_token_states_i = id_i * mask_i * scale * W4.
fn graph(wt: &Weights, token_states: bool, tower: bool) -> Vec<u8> {
    let mut g = Vec::new();
    let nodes = [
        node("Cast", &["input_ids"], &["xf"], &[("to", 1)]),
        node("Cast", &["attention_mask"], &["mf"], &[("to", 1)]),
        node("Mul", &["xf", "mf"], &["xm"], &[]),
        node("ReduceSum", &["xm", "axes1"], &["s0"], &[("keepdims", 1)]),
        node("Sub", &["s0", "shift"], &["s"], &[]),
        node("Mul", &["s", "scale"], &["ss"], &[]),
        node("Mul", &["ss", "w"], &["h"], &[]),
        node("Unsqueeze", &["xm", "axes2"], &["u"], &[]),
        node("Mul", &["u", "scale"], &["us"], &[]),
        node("Mul", &["us", "w3"], &[TOKEN_STATES], &[]),
        node("Mul", &["us", "w4"], &[TOWER_STATES], &[]),
    ];
    for n in &nodes {
        bytes(&mut g, 1, n);
    }
    bytes(&mut g, 2, b"synthetic");
    for t in [
        float_init("shift", &[1], &[wt.shift]),
        float_init("scale", &[1], &[wt.scale]),
        float_init("w", &[1, N as u64], &wt.w),
        float_init("w3", &[1, 1, N as u64], &wt.w3),
        float_init("w4", &[1, 1, N as u64], &wt.w4),
        int_init("axes1", &[1]),
        int_init("axes2", &[2]),
    ] {
        bytes(&mut g, 5, &t);
    }
    for i in ["input_ids", "attention_mask"] {
        bytes(&mut g, 11, &value_info(i, 7, &[Some(1), None]));
    }
    bytes(&mut g, 12, &value_info("h", 1, &[Some(1), Some(N as u64)]));
    if token_states {
        bytes(&mut g, 12, &value_info(TOKEN_STATES, 1, &[Some(1), None, Some(N as u64)]));
    }
    if tower {
        bytes(&mut g, 12, &value_info(TOWER_STATES, 1, &[Some(1), None, Some(N as u64)]));
    }
    let mut m = Vec::new();
    uint(&mut m, 1, 8);
    let mut opset = Vec::new();
    bytes(&mut opset, 1, b"");
    uint(&mut opset, 2, 17);
    bytes(&mut m, 8, &opset);
    bytes(&mut m, 7, &g);
    m
}

// ----------------------------------------------------------------- artifact

const WORDS: [&str; 8] = ["alpha", "beta", "gamma", "delta", "eps", "zeta", "eta", "theta"];

fn tokenizer() -> Value {
    let special = |id: u64, content: &str| json!({"id": id, "content": content, "single_word": false,
        "lstrip": false, "rstrip": false, "normalized": false, "special": true});
    let mut vocab = serde_json::Map::new();
    for (i, t) in ["[CLS]", "[SEP]", "[UNK]"].iter().chain(WORDS.iter()).enumerate() {
        vocab.insert(t.to_string(), json!(i));
    }
    let tok = |id: &str| json!({"SpecialToken": {"id": id, "type_id": 0}});
    json!({"version": "1.0", "truncation": null, "padding": null,
        "added_tokens": [special(0, "[CLS]"), special(1, "[SEP]"), special(2, "[UNK]")],
        "normalizer": null, "pre_tokenizer": {"type": "Whitespace"},
        "post_processor": {"type": "TemplateProcessing",
            "single": [tok("[CLS]"), {"Sequence": {"id": "A", "type_id": 0}}, tok("[SEP]")],
            "pair": [tok("[CLS]"), {"Sequence": {"id": "A", "type_id": 0}}, tok("[SEP]"),
                {"Sequence": {"id": "B", "type_id": 1}}, {"SpecialToken": {"id": "[SEP]", "type_id": 1}}],
            "special_tokens": {"[CLS]": {"id": "[CLS]", "ids": [0], "tokens": ["[CLS]"]},
                "[SEP]": {"id": "[SEP]", "ids": [1], "tokens": ["[SEP]"]}}},
        "decoder": null,
        "model": {"type": "WordLevel", "vocab": vocab, "unk_token": "[UNK]"}})
}

fn linear(rows: usize, cols: usize, seed: u64) -> (Value, Value) {
    let mut state = seed;
    let mut next = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((state >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 0.2
    };
    let w = (0..rows * cols).map(|_| next()).collect::<Vec<_>>();
    let b = (0..rows).map(|_| next()).collect::<Vec<_>>();
    (json!({"shape": [rows, cols], "values": w}), json!({"shape": [rows], "values": b}))
}

#[derive(Clone, Copy, PartialEq)]
enum Form {
    /// R0's form: export v2, heads v1, no token head.
    Plain,
    /// S0+'s form: export v3, heads v2, token head on the final-norm states.
    TokenHead,
    /// S2c's form: export v4, heads v3, token head on the span tower's states.
    Tower,
}

fn span_emission() -> Value {
    json!({"rule": full_a_spans::EMISSION_RULE, "word_rule": full_a_spans::WORD_RULE,
        "word_probability_rule": full_a_spans::WORD_PROBABILITY_RULE, "tilt_rule": full_a_spans::TILT_RULE,
        "splitter": full_a_spans::SPLITTER_ID,
        "decoder": {"rule": full_a_spans::DECODER_RULE, "tau_hi": 0.5, "delta": 0.0, "min_words": 1, "gap": 0,
            "snap": null},
        "sentence_confidence": {"method": "identity_v1"}, "span_confidence": {"method": "identity_v1"}})
}

/// Writes the five non-graph files of an artifact of `form` into `dir`.
fn role_files(dir: &Path, form: Form) {
    let contract = json!({"schema": "slop_ninja_full_a_contract_v2", "input_mode": "text", "raw_dimension": N,
        "maximum_tokens_including_specials": 2048, "token_truncation": false, "human_profiles": [],
        "ai_profiles": ["a1", "a2"]});
    let cb = serde_json::to_vec_pretty(&contract).unwrap();
    let ch = sha256_hex(&cb);
    let mut weights = serde_json::Map::new();
    for (name, rows, cols, seed) in [
        ("residual.0", 3, N + 2, 1),
        ("residual.2", N, 3, 2),
        ("origin", 3, N, 3),
        ("mixed_share", 1, N, 4),
        ("human", 0, N, 5),
        ("ai", 2, N, 6),
    ] {
        let (w, b) = linear(rows, cols, seed);
        weights.insert(format!("{name}.weight"), w);
        weights.insert(format!("{name}.bias"), b);
    }
    let (schema, export, calibration) = match form {
        Form::Plain => ("slop_ninja_full_a_heads_v1", "slop_ninja_full_a_export_v2", "slop_ninja_full_a_calibration_v2"),
        Form::TokenHead => ("slop_ninja_full_a_heads_v2", "slop_ninja_full_a_export_v3", "slop_ninja_full_a_calibration_v3"),
        Form::Tower => ("slop_ninja_full_a_heads_v3", "slop_ninja_full_a_export_v4", "slop_ninja_full_a_calibration_v3"),
    };
    let mut heads = json!({"schema": schema,
        "architecture": {"dimension": N + 2, "raw_text_dimension": N, "hidden": 3, "embedding": N,
            "human_profiles": 0, "ai_profiles": 2},
        "span_head": null, "share_mode": "head", "contract_sha256": ch, "initialization_sha256": "b".repeat(64),
        "base_temperature": 1.5, "text_mean": vec![0.01; N], "text_scale": vec![1.5; N],
        "profiles": {"human": [], "ai": ["a1", "a2"]}, "thresholds": {"human": 1.0, "ai": 0.6},
        "involvement_calibration": {"method": "identity_v1"}, "supervision": {"origin": 1},
        "origin_order": ["human_only", "model_only", "mixed"]});
    let tower: Value = serde_json::from_str::<Value>(TOWER_GOLDEN).unwrap()["tower"].clone();
    if form != Form::Plain {
        let (w1, b1) = linear(4, N, 7);
        let (w2, b2) = linear(1, 4, 8);
        weights.insert("token_head.first.weight".into(), w1);
        weights.insert("token_head.first.bias".into(), b1);
        weights.insert("token_head.second.weight".into(), w2);
        weights.insert("token_head.second.bias".into(), b2);
        let input = if form == Form::Tower { full_a_tower::TOWER_INPUT } else { full_a_tower::FINAL_NORM_INPUT };
        heads["token_head"] = json!({"hidden": 4, "activation": "gelu_tanh", "rule": full_a_spans::TOKEN_HEAD_RULE,
            "input": input, "label_policy": full_a_spans::LABEL_POLICY, "feeds_document_heads": false});
        heads["span_emission"] = span_emission();
    }
    if form == Form::Tower {
        heads["span_tower"] = tower.clone();
    }
    heads["weights"] = Value::Object(weights);
    let hb = serde_json::to_vec(&heads).unwrap();
    let mut manifest = json!({"schema": export, "complete": true, "calibrated": true, "arm": "synthetic", "seed": 1,
        "selected_checkpoint_sha256": "c".repeat(64)});
    let mut cal = json!({"schema": calibration, "complete": true, "fit_split": "cal", "arm": "synthetic", "seed": 1,
        "selected_checkpoint_sha256": "c".repeat(64), "contract_sha256": ch,
        "involvement": {"selected": {"method": "identity_v1"}},
        "roles": {"human": {"roster_empty": true}, "ai": {"qualified": true, "threshold": 0.6}}});
    if form != Form::Plain {
        manifest["token_head"] = heads["token_head"].clone();
        manifest["span_emission"] = heads["span_emission"].clone();
        cal["spans"] = heads["span_emission"].clone();
    }
    if form == Form::Tower {
        manifest["span_tower"] = json!({"file": "tower.safetensors", "sha256": "d".repeat(64), "declaration": tower});
    }
    fs::write(dir.join("contract.json"), cb).unwrap();
    fs::write(dir.join("heads.json"), hb).unwrap();
    fs::write(dir.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
    fs::write(dir.join("calibration.json"), serde_json::to_vec(&cal).unwrap()).unwrap();
    fs::write(dir.join("tokenizer.json"), serde_json::to_vec(&tokenizer()).unwrap()).unwrap();
}

fn artifact(tag: &str, form: Form, wt: &Weights, token_states: bool, tower: bool) -> PathBuf {
    let dir = scratch(tag);
    role_files(&dir, form);
    fs::write(dir.join(GRAPH), graph(wt, token_states, tower)).unwrap();
    dir
}

// ---------------------------------------------------------------- reference

/// The token ids the artifact's tokenizer gives `text`.
fn ids(model: &Model, text: &str) -> Vec<u32> {
    model.tokenizer.encode(text, true).unwrap().get_ids().to_vec()
}

/// The graph's outputs for `ids`, in float32 as ONNX Runtime computes them, widened.
fn reference(wt: &Weights, ids: &[u32]) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let sum: f32 = ids.iter().map(|&i| i as f32).sum::<f32>() - wt.shift;
    let h = wt.w.iter().map(|&w| ((sum * wt.scale) * w) as f64).collect();
    let rows = |w: &[f32]| {
        ids.iter().flat_map(|&i| w.iter().map(move |&x| (((i as f32) * wt.scale) * x) as f64)).collect::<Vec<_>>()
    };
    (h, rows(&wt.w3), rows(&wt.w4))
}

fn heads(dir: &Path) -> SpanTowerHeads {
    SpanTowerHeads::from_json(&fs::read(dir.join("heads.json")).unwrap()).unwrap()
}

const DOCS: [&str; 4] = [
    "alpha beta gamma. delta eps zeta.",
    "theta eta zeta eps delta gamma beta alpha.",
    "gamma gamma gamma unknown words here.",
    "beta. alpha!",
];

// -------------------------------------------------------------------- tests

#[test]
fn admission_refuses_a_synthetic_graph_before_onnx_runtime() {
    // Not an allow-listed class: refused by length, with no library loaded at all.
    let dir = artifact("admission", Form::Plain, &weights(1e-3), false, false);
    let spec = a_container_serve::engine::NEWEST_SPEC;
    let header = a_container_serve::engine::header_of_dir(&dir).unwrap();
    let e = Model::load(&dir, &header, spec, GRAPH, 1, 1, "/nonexistent/libonnxruntime.so").err().unwrap();
    let msg = format!("{e:#}");
    assert!(msg.starts_with("refused "), "{msg}");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn plain_document_outputs_equal_the_heads_on_the_graph_outputs_and_ignore_batching() {
    let Some(lib) = lib() else { return };
    let wt = weights(1e-3);
    let dir = artifact("plain", Form::Plain, &wt, false, false);
    let model = Model::load_without_admission(&dir, &contract_sha(&dir), GRAPH, 3, 1, &lib).unwrap();
    assert!(!model.emits_spans());
    let h = heads(&dir);
    let alone: Vec<Value> = DOCS.iter().enumerate().map(|(i, d)| model.score(&i.to_string(), d)).collect();
    for (i, d) in DOCS.iter().enumerate() {
        let r = &alone[i];
        assert_eq!(r["status"], "ok", "{r}");
        let want = h.predict(&reference(&wt, &ids(&model, d)).0, &[], &[], &[]).unwrap();
        assert_eq!(r["prediction"], serde_json::to_value(&want.prediction).unwrap());
        assert_eq!(r["output_sha256"], sha256_hex(&serde_json::to_vec(&r["prediction"]).unwrap()));
        // Spans asked of an artifact with no token head change nothing.
        let s = model.score_with(&i.to_string(), d, true);
        assert_eq!(s["prediction"], r["prediction"]);
        assert!(s.get("span_status").is_none());
    }
    // Batch invariance: the same documents scored concurrently, in reverse, over
    // three sessions, give the same outputs.
    let concurrent: Vec<Value> = std::thread::scope(|s| {
        let hs: Vec<_> = DOCS.iter().enumerate().rev().map(|(i, d)| {
            let m = &model;
            s.spawn(move || (i, m.score(&i.to_string(), d)))
        }).collect();
        let mut v: Vec<(usize, Value)> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        v.sort_by_key(|x| x.0);
        v.into_iter().map(|x| x.1).collect()
    });
    for (a, b) in alone.iter().zip(&concurrent) {
        assert_eq!(a["output_sha256"], b["output_sha256"]);
    }
    // Inputs the scorer refuses are the text's, not the artifact's.
    assert_eq!(model.score("e", " \n ")["reason"], "empty_text");
    let long = vec!["alpha"; 2047].join(" ");
    assert_eq!(model.score("l", &long)["reason"], "unsupported_length");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_artifact_that_overflows_on_the_warm_up_document_fails_to_load() {
    let Some(lib) = lib() else { return };
    let mut wt = weights(3.0e38);
    wt.w = vec![3.0e38; N];
    let dir = artifact("overflow", Form::Plain, &wt, false, false);
    // Warm-up runs a document too, so the load itself fails.
    let e = Model::load_without_admission(&dir, &contract_sha(&dir), GRAPH, 1, 1, &lib).err().unwrap();
    assert!(format!("{e:#}").contains("invalid h"), "{e:#}");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_artifact_that_loads_and_overflows_on_every_document_fails_each_as_a_model_error() {
    let Some(lib) = lib() else { return };
    // The warm-up document's ids sum to exactly the shift, so its h is 0 and the
    // load succeeds; every real document's h overflows.
    let warm: f32 = (50281 + 50282 + (1000..1062).sum::<u32>()) as f32;
    let mut wt = weights(3.0e38);
    wt.shift = warm;
    let dir = artifact("overflow-docs", Form::Plain, &wt, false, false);
    let model = Model::load_without_admission(&dir, &contract_sha(&dir), GRAPH, 2, 1, &lib).unwrap();
    for (i, d) in DOCS.iter().enumerate() {
        let r = model.score(&i.to_string(), d);
        assert_eq!((r["status"].as_str(), r["reason"].as_str()), (Some("error"), Some("model")), "{r}");
        assert!(r["prediction"].is_null() && r["token_count"].as_u64().is_some());
    }
    // A text the scorer refuses is still the text's, not the artifact's.
    assert_eq!(model.score("e", "")["status"], "unsupported_input");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_heads_v2_token_head_reads_the_final_norm_states_and_emits_spans() {
    let Some(lib) = lib() else { return };
    let wt = weights(1e-3);
    let dir = artifact("token-head", Form::TokenHead, &wt, true, false);
    let model = Model::load_without_admission(&dir, &contract_sha(&dir), GRAPH, 2, 1, &lib).unwrap();
    assert!(model.emits_spans());
    assert!(model.needs.token_states && !model.needs.tower_states);
    let h = heads(&dir);
    for (i, d) in DOCS.iter().enumerate() {
        let plain = model.score_with(&i.to_string(), d, false);
        let spans = model.score_with(&i.to_string(), d, true);
        // The document outputs and their digest do not depend on the span request.
        assert_eq!(plain["output_sha256"], spans["output_sha256"]);
        assert!(plain.get("span_status").is_none() && plain["prediction"].get("spans").is_none());
        assert_eq!(spans["span_status"], "ok", "{spans}");
        // The reference: the heads runtime on the graph's closed-form outputs.
        let ids = ids(&model, d);
        let (hv, tokens, tower) = reference(&wt, &ids);
        let pred = h.predict(&hv, &[], &[], &[]).unwrap();
        let enc = model.tokenizer.encode_char_offsets(*d, true).unwrap();
        assert_eq!(enc.get_ids(), ids.as_slice());
        let offsets = enc.get_offsets().to_vec();
        let words = word_mask(d, &offsets).unwrap();
        let states = TokenStates { final_norm: &tokens, span_tower: None };
        let logits = h.token_logits(&states, &words).unwrap().unwrap();
        let x = h.emit_spans(d, &offsets, &words, &logits, &pred.prediction).unwrap();
        assert_eq!(spans["prediction"]["sentences"], serde_json::to_value(&x.sentences).unwrap());
        assert_eq!(spans["prediction"]["spans"], serde_json::to_value(&x.spans).unwrap());
        // The tower's states would give other logits: the head reads the declared states.
        let other = h.heads().token_logits(&tower, &words).unwrap().unwrap();
        assert_ne!(other, logits);
        assert_eq!(content_mask(ids.len()).iter().filter(|c| **c).count(), ids.len() - 2);
    }
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_heads_v3_token_head_reads_the_span_tower_states() {
    let Some(lib) = lib() else { return };
    let wt = weights(1e-3);
    let dir = artifact("tower", Form::Tower, &wt, true, true);
    let model = Model::load_without_admission(&dir, &contract_sha(&dir), GRAPH, 1, 1, &lib).unwrap();
    assert!(model.needs.tower_states && !model.needs.token_states);
    let h = heads(&dir);
    for (i, d) in DOCS.iter().enumerate() {
        let r = model.score_with(&i.to_string(), d, true);
        assert_eq!(r["span_status"], "ok", "{r}");
        let ids = ids(&model, d);
        let (hv, tokens, tower) = reference(&wt, &ids);
        let pred = h.predict(&hv, &[], &[], &[]).unwrap();
        let enc = model.tokenizer.encode_char_offsets(*d, true).unwrap();
        let offsets = enc.get_offsets().to_vec();
        let words = word_mask(d, &offsets).unwrap();
        let logits = h.token_logits(&TokenStates { final_norm: &tokens, span_tower: Some(&tower) }, &words).unwrap().unwrap();
        let x = h.emit_spans(d, &offsets, &words, &logits, &pred.prediction).unwrap();
        assert_eq!(r["prediction"]["spans"], serde_json::to_value(&x.spans).unwrap());
        assert_eq!(r["prediction"]["sentences"], serde_json::to_value(&x.sentences).unwrap());
        assert_eq!(r["prediction"]["ai_confidence_percent"], json!(pred.prediction.ai_confidence_percent));
    }
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_graph_without_the_states_its_heads_read_is_refused_at_load() {
    let Some(lib) = lib() else { return };
    let wt = weights(1e-3);
    for (tag, form, token_states, tower, missing) in [
        ("no-token-states", Form::TokenHead, false, true, TOKEN_STATES),
        ("no-tower-states", Form::Tower, true, false, TOWER_STATES),
    ] {
        let dir = artifact(tag, form, &wt, token_states, tower);
        let e = Model::load_without_admission(&dir, &contract_sha(&dir), GRAPH, 1, 1, &lib).err().unwrap();
        assert!(format!("{e:#}").contains(missing), "{e:#}");
        fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn the_load_guard_binds_the_export_to_its_heads() {
    // Guard refusals come before ONNX Runtime: no library needed.
    let wt = weights(1e-3);
    type Mutate = Box<dyn Fn(&mut Value, &mut Value, &mut Value)>;
    let cases: [(&str, Form, Mutate, &str); 5] = [
        ("tok-decl", Form::TokenHead, Box::new(|m, _, _| m["token_head"]["hidden"] = json!(5)), "token head declaration"),
        ("cal-spans", Form::TokenHead, Box::new(|_, c, _| c["spans"]["decoder"]["tau_hi"] = json!(0.6)),
            "calibration/checkpoint/involvement map mismatch"),
        ("v3-tower", Form::TokenHead, Box::new(|m, _, _| m["span_tower"] = json!({})), "only a v4 export"),
        ("v4-decl", Form::Tower, Box::new(|m, _, _| m["span_tower"]["declaration"]["layers"] = json!([19, 20, 21])),
            "span tower declaration"),
        ("schema", Form::TokenHead, Box::new(|m, _, _| m["schema"] = json!("slop_ninja_full_a_export_v4")),
            "heads schema does not match"),
    ];
    for (tag, form, edit, want) in cases {
        let dir = artifact(tag, form, &wt, true, form == Form::Tower);
        let mut m: Value = serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let mut c: Value = serde_json::from_slice(&fs::read(dir.join("calibration.json")).unwrap()).unwrap();
        let mut w: Value = serde_json::from_slice(&fs::read(dir.join("heads.json")).unwrap()).unwrap();
        edit(&mut m, &mut c, &mut w);
        fs::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        fs::write(dir.join("calibration.json"), serde_json::to_vec(&c).unwrap()).unwrap();
        let e = Model::load_without_admission(&dir, &contract_sha(&dir), GRAPH, 1, 1, "/nonexistent/libonnxruntime.so").err().unwrap();
        assert!(format!("{e:#}").contains(want), "{tag}: {e:#}");
        fs::remove_dir_all(&dir).unwrap();
    }
}

/// Every string in `v`, at any depth.
fn strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(o) => o.iter().for_each(|(k, x)| {
            out.push(k.clone());
            strings(x, out)
        }),
        _ => {}
    }
}

#[test]
fn per_document_results_are_content_free() {
    let Some(lib) = lib() else { return };
    let id = "sentinel-id-7c1d";
    // Unknown words, punctuation and digits, and a text whose characters are all
    // Python whitespace but not Rust's (no words, so span emission fails).
    let texts = ["alpha zyxwvut-sentinel 41c7 beta, gamma.", "\u{1c}\u{1d}\u{1e}\u{1f}\u{1c}\u{1d}\u{1e}\u{1f}"];
    let base = ["id", "ms", "output_sha256", "prediction", "status", "text_sha256", "token_count"];
    let wt = weights(1e-3);
    for (tag, form, token_states) in [("cf-plain", Form::Plain, false), ("cf-token", Form::TokenHead, true)] {
        let dir = artifact(tag, form, &wt, token_states, false);
        let model = Model::load_without_admission(&dir, &contract_sha(&dir), GRAPH, 1, 1, &lib).unwrap();
        for text in texts {
            for spans in [false, true] {
                let r = model.score_with(id, text, spans);
                assert_eq!(r["status"], "ok", "{r}");
                let mut keys: Vec<&str> = r.as_object().unwrap().keys().map(String::as_str).collect();
                keys.sort();
                let mut want = base.to_vec();
                if spans && form == Form::TokenHead {
                    want.push("span_status");
                    if r["span_status"] == "ok" {
                        want.push("span_output_sha256");
                    }
                }
                want.sort();
                assert_eq!(keys, want, "{r}");
                // No string anywhere carries the text or a piece of it, and the
                // id appears as the id only.
                let mut rest = r.clone();
                rest.as_object_mut().unwrap().remove("id");
                let mut all = Vec::new();
                strings(&rest, &mut all);
                for s in &all {
                    assert!(!s.contains("sentinel") && !s.contains(text) && !s.contains('\u{1c}'), "{s:?} in {r}");
                }
                assert_eq!(r["id"], id);
            }
        }
        // The text with no words: span emission fails as `unsupported`, content-free.
        if form == Form::TokenHead {
            let r = model.score_with(id, texts[1], true);
            assert!(r["span_status"]["unsupported"].is_string(), "{r}");
            assert!(r["prediction"]["spans"].is_null() && r["prediction"]["sentences"].is_null());
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}

/// The scorer's load path takes the header's digest of `contract.json`
/// (`guard_verified`) instead of hashing it; the release guard every spec pins
/// by source (`guard`) stays unchanged. The two differ in exactly the
/// signature and the digest's source, so their checks are the same.
#[test]
fn the_load_guard_differs_from_the_pinned_guard_only_in_the_digest_source() {
    let src = include_str!("../src/engine.rs");
    let body = |sig: &str| -> Vec<&str> {
        let at = src.find(sig).unwrap();
        let mut lines = Vec::new();
        for l in src[at..].lines() {
            lines.push(l);
            if l == "}" {
                break;
            }
        }
        lines
    };
    let pinned = body("pub fn guard(dir: &Path)");
    let verified = body("pub fn guard_verified(dir: &Path, contract_sha256: &str)");
    assert_eq!(pinned.len(), verified.len());
    let differ: Vec<(&str, &str)> = pinned.iter().zip(&verified).filter(|(a, b)| a != b).map(|(a, b)| (*a, *b)).collect();
    assert_eq!(
        differ,
        vec![
            ("pub fn guard(dir: &Path) -> Result<(Vec<u8>, Value)> {", "pub fn guard_verified(dir: &Path, contract_sha256: &str) -> Result<(Vec<u8>, Value)> {"),
            ("    let ch = sha(&cb);", "    let ch = contract_sha256.to_owned();"),
        ]
    );
}
