//! Spec v3: spec v1 and v2 stay exactly as published; v3 keeps every v2 value
//! for the plain and span classes and adds the span tower class, heads v3 in
//! export v4, and per-class parameter and JSON value bounds.
//!
//! The artifact tests need private files and pass with a notice without them:
//! `A_R0_MODEL_DIR` (R0's six files), `A_SPAN_MODEL_DIR` (the S0+ span-head
//! artifact, as for `tests/spec_v2.rs`), `A_TOWER_MODEL_DIR` (the S2c span
//! package: the tower export as `a-fp32band.onnx`, with the calibrated S2c
//! export's heads, contract, calibration and export manifest re-encoded in the
//! canonical layouts by `slop_ninja_artifact_check canonical`, and the pinned
//! tokenizer), `A_EXPORT_SPAN` (the span export alone) and `A_EXPORT_TOWER`
//! (the tower export alone).

mod common;

use std::path::PathBuf;

use common::*;
use serde_json::{Value, json};
use slop_ninja_artifact_check::mapping::{check_header_for, check_listing};
use slop_ninja_artifact_check::onnx::GraphScan;
use slop_ninja_artifact_check::spec::{self, Version};
use slop_ninja_artifact_check::{Refusal, TOWER_RUNTIME_SOURCE, canonical, fs};
use slop_ninja_protocol::{ArtifactFile, ArtifactHeader, Digest};

/// Spec v1's and v2's digests as published (docs/testnet/artifact-spec.md).
const SPEC_V1_SHA256: &str = "7fceee2c93436bda1c01a8123ec4ac05807f47d1017b31200d56df0705cb7147";
const SPEC_V2_SHA256: &str = "0f25e636ba52b45269c0a4d0520c2f01550b51325bb6399d4ddae418c8384b50";

fn sha(b: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(b))
}

fn env_path(name: &str) -> Option<PathBuf> {
    let p = std::env::var_os(name).map(PathBuf::from);
    if p.is_none() {
        eprintln!("notice: {name} is unset; test passes without checking it");
    }
    p
}

#[test]
fn spec_v1_and_v2_are_unchanged() {
    assert_eq!(Version::V1.spec_sha256(), SPEC_V1_SHA256);
    assert_eq!(Version::V2.spec_sha256(), SPEC_V2_SHA256);
    assert_eq!(Version::V2.spec_json(), spec::spec_json_v2());
    assert_eq!(Version::V2.classes(), &spec::GRAPHS);
    assert_eq!(Version::V1.classes(), &spec::GRAPHS);
}

#[test]
fn spec_v3_changes_only_what_the_tower_class_needs() {
    let (v2, v3) = (spec::spec_json_v2(), spec::spec_json_v3());
    let (a, b) = (v2.as_object().unwrap(), v3.as_object().unwrap());
    let added: Vec<&str> = b
        .keys()
        .filter(|k| !a.contains_key(*k))
        .map(String::as_str)
        .collect();
    assert_eq!(added, ["tower_runtime_sha256"]);
    let removed: Vec<&str> = a
        .keys()
        .filter(|k| !b.contains_key(*k))
        .map(String::as_str)
        .collect();
    assert_eq!(removed, ["max_json_value_bytes", "max_parameters"]);
    for (k, v) in b {
        match k.as_str() {
            "schema" => assert_eq!(v, spec::SPEC_ID_V3),
            "graph" | "forms" | "json_value_cost" | "tower_runtime_sha256" => {}
            _ => assert_eq!(v, &a[k], "{k} must be spec v2's"),
        }
    }
    assert_eq!(b["tower_runtime_sha256"], spec::TOWER_RUNTIME_SHA256);
    // The value cost model is v2's; only its bound moved into the classes.
    for (k, v) in b["json_value_cost"].as_object().unwrap() {
        if k != "checked" {
            assert_eq!(v, &a["json_value_cost"][k], "json_value_cost {k}");
        }
    }
    // Graph: v2's pre-parse, digest and allow-lists; the classes gain their
    // bounds and the tower flag, and the tower class is added.
    for (k, v) in b["graph"].as_object().unwrap() {
        match k.as_str() {
            "classes" | "token_states_rule" => {}
            _ => assert_eq!(v, &a["graph"][k], "graph {k} must be spec v2's"),
        }
    }
    let (c2, c3) = (
        a["graph"]["classes"].as_array().unwrap(),
        b["graph"]["classes"].as_array().unwrap(),
    );
    assert_eq!(c3.len(), c2.len() + 1);
    for (old, new) in c2.iter().zip(c3) {
        let mut stripped = new.clone();
        let o = stripped.as_object_mut().unwrap();
        assert_eq!(o.remove("tower_states"), Some(json!(false)));
        assert_eq!(
            o.remove("max_parameters"),
            Some(a["max_parameters"].clone())
        );
        assert_eq!(
            o.remove("max_json_value_bytes"),
            Some(a["max_json_value_bytes"].clone())
        );
        assert_eq!(&stripped, old, "a spec v2 class changed");
    }
    let tower = &c3[2];
    assert_eq!(tower["id"], spec::TOWER_GRAPH.id);
    assert_eq!(tower["token_states"], false);
    assert_eq!(tower["tower_states"], true);
    assert_eq!(tower["span_tower"], spec::tower::declaration());
    assert_eq!(tower["max_parameters"], spec::TOWER_MAX_PARAMETERS);
    assert_eq!(
        tower["max_json_value_bytes"],
        spec::MAX_JSON_VALUE_BYTES_TOWER
    );
    // Forms: v2's two, unchanged, then export v4.
    let (f2, f3) = (
        a["forms"].as_array().unwrap(),
        b["forms"].as_array().unwrap(),
    );
    assert_eq!(&f3[..2], &f2[..]);
    assert_eq!(f3.len(), 3);
    assert_eq!(f3[2]["export"], spec::EXPORT_SCHEMA_V4);
    assert_eq!(f3[2]["heads"], spec::HEADS_SCHEMA_V3);
    assert_eq!(f3[2]["token_head"]["input"], spec::TOKEN_HEAD_INPUT_V3);
    assert_ne!(Version::V3.spec_sha256(), SPEC_V2_SHA256);
}

#[test]
fn the_tower_runtime_pin_is_the_compiled_source_and_the_scorers() {
    assert_eq!(
        sha(TOWER_RUNTIME_SOURCE),
        spec::TOWER_RUNTIME_SHA256,
        "full_a_tower.rs changed: review it, then update spec::TOWER_RUNTIME_SHA256 (a spec v3 change)"
    );
    let scorer = include_str!("../../../../inference/a-container-serve/src/lib.rs");
    assert!(
        scorer.contains(&format!(
            "pub const TOWER_RUNTIME_SHA256: &str = \"{}\";",
            spec::TOWER_RUNTIME_SHA256
        )),
        "the scorer pins another span tower runtime"
    );
}

#[test]
fn the_exporter_records_the_tower_class_digest() {
    let script = include_str!("../../../../grammar/crates/grammar-eval/python/export_a_onnx.py");
    assert!(script.contains(&format!(
        "\"tower\": \"{}\"",
        spec::TOWER_GRAPH.structure_sha256
    )));
    assert!(script.contains("TOWER_OUTPUTS = PLAIN_OUTPUTS + [\"span_tower_token_states\"]"));
    assert_eq!(
        spec::TOWER_GRAPH.outputs.last(),
        Some(&"span_tower_token_states")
    );
}

#[test]
fn versions_are_named_by_id_and_digest() {
    for v in Version::ALL {
        assert_eq!(Version::from_id(v.id()), Some(v));
        assert_eq!(Version::named(v.id(), &v.spec_sha256()), Some(v));
        assert_eq!(Version::named(v.id(), &"0".repeat(64)), None);
    }
    assert_eq!(Version::from_id("slop_ninja_a_artifact_spec_v4"), None);
    assert_eq!(Version::LATEST, Version::V3);
    assert_eq!(Version::ALL.len(), 3);
}

#[test]
fn the_tower_class_is_a_spec_v3_class_with_its_own_bounds() {
    let t = &spec::TOWER_GRAPH;
    assert_eq!(Version::V3.classes(), &spec::GRAPHS_V3);
    assert_eq!(&Version::V3.classes()[..2], &spec::GRAPHS);
    for v in [Version::V1, Version::V2] {
        assert_eq!(v.graph_class_by_len(t.bytes), None);
        assert_eq!(v.graph_class(t.structure_sha256), None);
        assert!(!v.serves(t));
    }
    assert_eq!(Version::V3.graph_class_by_len(t.bytes), Some(t));
    assert_eq!(Version::V3.graph_class(t.structure_sha256), Some(t));
    assert!(Version::V3.classes().iter().all(|g| Version::V3.serves(g)));
    assert!(t.tower_states && !t.span_head);
    assert!(spec::GRAPHS.iter().all(|g| !g.tower_states));
    // The unversioned lookups are spec v1 and v2's.
    assert_eq!(spec::graph_class_by_len(t.bytes), None);
    assert_eq!(spec::graph_class(t.structure_sha256), None);
    // Lengths are distinct across the classes, so a header fixes the class.
    let mut lens: Vec<u64> = spec::GRAPHS_V3.iter().map(|g| g.bytes).collect();
    lens.dedup();
    assert_eq!(lens.len(), 3);
    assert!(t.bytes <= spec::MAX_FILE_BYTES);

    // Parameters: the plain and span classes keep v2's bound; the tower's is
    // its graph plus the largest heads its form admits.
    assert!(
        spec::GRAPHS
            .iter()
            .all(|g| g.max_parameters == spec::MAX_PARAMETERS)
    );
    assert_eq!(spec::token_head_parameters(256), 197_121);
    // The tower's weights file: four copied layers of 5,015,040 and the norm.
    assert_eq!(spec::tower::LAYER_PARAMETERS, 5_015_040);
    assert_eq!(
        (spec::tower::PARAMETERS, spec::tower::TENSORS),
        (20_060_928, 25)
    );
    assert_eq!(
        spec::token_head_parameters(spec::TOKEN_HEAD_MAX_HIDDEN),
        3_153_921
    );
    assert_eq!(
        spec::TOWER_MAX_PARAMETERS,
        spec::TOWER_GRAPH_PARAMETERS + spec::DOCUMENT_HEAD_PARAMETERS + 3_153_921
    );
    assert_eq!(t.max_parameters, spec::TOWER_MAX_PARAMETERS);
    assert_eq!(
        spec::graph_policy(t).max_parameters,
        spec::TOWER_MAX_PARAMETERS
    );
    let plain = spec::graph_policy(&spec::GRAPHS[0]);
    assert_eq!(plain.max_parameters, spec::GRAPH_POLICY.max_parameters);
    assert_eq!(
        (plain.ops, plain.domains, plain.dtypes, plain.attr_types),
        (spec::OPS, spec::DOMAINS, spec::DTYPES, spec::ATTR_TYPES)
    );
    assert_eq!(tower_policy_ops(), spec::OPS);

    // JSON value budgets: v2's for the plain and span classes under v2 and
    // v3; the tower's own; the class-free bound is the smallest.
    for g in &spec::GRAPHS {
        for v in [Version::V2, Version::V3] {
            assert_eq!(
                v.max_json_value_bytes_for(g),
                Some(spec::MAX_JSON_VALUE_BYTES_V2)
            );
        }
        assert_eq!(Version::V1.max_json_value_bytes_for(g), None);
    }
    assert_eq!(
        Version::V3.max_json_value_bytes_for(t),
        Some(spec::MAX_JSON_VALUE_BYTES_TOWER)
    );
    assert_eq!(
        Version::V2.max_json_value_bytes(),
        Some(spec::MAX_JSON_VALUE_BYTES_V2)
    );
    assert_eq!(
        Version::V3.max_json_value_bytes(),
        Some(spec::MAX_JSON_VALUE_BYTES_V2.min(spec::MAX_JSON_VALUE_BYTES_TOWER))
    );
    assert_eq!(Version::V3.max_json_bytes(), Version::V2.max_json_bytes());
}

fn tower_policy_ops() -> &'static [&'static str] {
    spec::graph_policy(&spec::TOWER_GRAPH).ops
}

#[test]
fn the_tower_bound_applies_to_the_graph_walk() {
    // A graph prefix declaring one initializer of n floats: refused at the
    // start of its payload when n passes the policy's bound.
    fn prefix(n: u64) -> (Vec<u8>, u64) {
        let payload = n * 4;
        let mut t = Vec::new();
        uint(&mut t, 1, n);
        uint(&mut t, 2, 1);
        bytes(&mut t, 8, b"w");
        tag(&mut t, 9, 2);
        varint(&mut t, payload);
        let tensor_len = t.len() as u64 + payload;
        let mut g = Vec::new();
        tag(&mut g, 5, 2);
        varint(&mut g, tensor_len);
        let graph_len = g.len() as u64 + t.len() as u64 + payload;
        let mut m = Vec::new();
        uint(&mut m, 1, 8);
        tag(&mut m, 7, 2);
        varint(&mut m, graph_len);
        let total = m.len() as u64 + g.len() as u64 + t.len() as u64 + payload;
        m.extend_from_slice(&g);
        m.extend_from_slice(&t);
        (m, total)
    }
    let tower = spec::graph_policy(&spec::TOWER_GRAPH);
    let (p, total) = prefix(spec::TOWER_MAX_PARAMETERS + 1);
    assert_eq!(
        GraphScan::new(total, tower).update(&p),
        Err(Refusal::TooManyParameters {
            count: spec::TOWER_MAX_PARAMETERS + 1,
            max: spec::TOWER_MAX_PARAMETERS
        })
    );
    let (p, total) = prefix(spec::TOWER_MAX_PARAMETERS);
    GraphScan::new(total, tower).update(&p).unwrap();
    // The same count is past the plain and span classes' bound.
    assert!(
        GraphScan::new(total, spec::GRAPH_POLICY)
            .update(&p)
            .is_err()
    );
}

/// A header with the six files: the graph at `graph_len`, the pinned
/// tokenizer, and small JSON files.
fn header(graph_len: u64) -> ArtifactHeader {
    let mut files: Vec<ArtifactFile> = spec::ROLES
        .iter()
        .map(|r| {
            let (sha256, len) = match r.name {
                spec::GRAPH_FILE => (Digest([1; 32]), graph_len),
                "tokenizer.json" => {
                    let mut d = [0u8; 32];
                    hex::decode_to_slice(spec::TOKENIZER_SHA256, &mut d).unwrap();
                    (Digest(d), spec::TOKENIZER_BYTES)
                }
                _ => (Digest([3; 32]), 1000),
            };
            ArtifactFile {
                name: r.name.into(),
                role: r.file_role,
                sha256,
                len,
            }
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    ArtifactHeader {
        contract: spec::CONTRACT_ID.into(),
        files,
    }
}

#[test]
fn the_tower_length_is_a_graph_size_refusal_before_spec_v3() {
    let t = spec::TOWER_GRAPH.bytes;
    for v in [Version::V1, Version::V2] {
        assert_eq!(check_header_for(&header(t), v), Err(Refusal::GraphSize(t)));
    }
    check_header_for(&header(t), Version::V3).unwrap();
    for g in &spec::GRAPHS {
        check_header_for(&header(g.bytes), Version::V3).unwrap();
    }
    assert_eq!(
        check_header_for(&header(t + 1), Version::V3),
        Err(Refusal::GraphSize(t + 1))
    );
    let h = header(t);
    let hexes: Vec<String> = h.files.iter().map(|f| hex::encode(f.sha256.0)).collect();
    let listing: Vec<(&str, &str, u64)> = h
        .files
        .iter()
        .zip(&hexes)
        .map(|(f, d)| (f.name.as_str(), d.as_str(), f.len))
        .collect();
    check_listing(spec::CONTRACT_ID, &listing, Version::V3).unwrap();
    assert_eq!(
        check_listing(spec::CONTRACT_ID, &listing, Version::V2),
        Err(Refusal::GraphSize(t))
    );
}

// ----------------------------------------------------------- private files

const CHUNK: usize = 999_983;

struct Art {
    dir: PathBuf,
}

impl Art {
    fn files(&self) -> Vec<TestFile> {
        spec::ROLES
            .iter()
            .map(|r| TestFile {
                name: r.name.into(),
                role: r.file_role,
                source: Source::Path(self.dir.join(r.name)),
                digest: None,
            })
            .collect()
    }

    fn json(&self, name: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.dir.join(name)).unwrap()).unwrap()
    }

    /// The files with JSON role files replaced (in their canonical layouts) and
    /// the export manifest's heads and calibration digests rebound.
    fn rebound(&self, replace: &[(&str, &Value)]) -> Vec<TestFile> {
        let mut files = self.files();
        let mut manifest = self.json("manifest.json");
        for (name, v) in replace {
            let b = enc(name, v);
            if let Some(key) = match *name {
                "heads.json" => Some("heads"),
                "calibration.json" => Some("calibration"),
                _ => None,
            } {
                manifest[key]["sha256"] = Value::from(sha(&b));
            }
            if *name == "manifest.json" {
                let heads = manifest["heads"].clone();
                let cal = manifest["calibration"].clone();
                manifest = (*v).clone();
                manifest["heads"] = heads;
                manifest["calibration"] = cal;
                continue;
            }
            let f = files.iter_mut().find(|f| f.name == *name).unwrap();
            f.source = Source::Bytes(b);
        }
        let f = files
            .iter_mut()
            .find(|f| f.name == "manifest.json")
            .unwrap();
        f.source = Source::Bytes(enc("manifest.json", &manifest));
        files
    }

    /// The files with the graph taken from `graph`.
    fn with_graph(&self, graph: PathBuf) -> Vec<TestFile> {
        let mut files = self.files();
        files
            .iter_mut()
            .find(|f| f.name == spec::GRAPH_FILE)
            .unwrap()
            .source = Source::Path(graph);
        files
    }
}

fn enc(name: &str, v: &Value) -> Vec<u8> {
    canonical::encode(v, spec::role_by_name(name).unwrap().layout.unwrap())
}

fn code<T>(r: Result<T, Refusal>) -> String {
    match r {
        Ok(_) => "accepted".into(),
        Err(e) => {
            eprintln!("  refused {}: {e}", e.code());
            e.code().into()
        }
    }
}

#[test]
fn spec_v3_admits_what_spec_v2_admits_with_the_same_results() {
    for var in ["A_R0_MODEL_DIR", "A_SPAN_MODEL_DIR"] {
        let Some(dir) = env_path(var) else {
            continue;
        };
        let v2 = fs::check_dir_for(&dir, Version::V2).unwrap();
        let v3 = fs::check_dir_for(&dir, Version::V3).unwrap();
        assert_eq!(v3.spec, spec::SPEC_ID_V3);
        assert_eq!(v3.content_sha256, v2.content_sha256, "{var}");
        assert_eq!(v3.graph_class, v2.graph_class, "{var}");
        assert_eq!(v3.parameters, v2.parameters, "{var}");
        assert_eq!(v3.roles, v2.roles, "{var}");
        assert_eq!(v3.graph, v2.graph, "{var}");
        assert_eq!(v3.profile_id, format!("{}/{}", spec::SPEC_ID_V3, v2.graph_class));
        assert_ne!(v3.profile_id, v2.profile_id);
    }
}

#[test]
fn an_arm_s2_field_is_refused_outside_export_v4() {
    let Some(dir) = env_path("A_SPAN_MODEL_DIR") else {
        return;
    };
    let art = Art { dir };
    let mut m = art.json("manifest.json");
    m["fit_binding"]["arm_definition"]["s2"] =
        json!({"mechanism": "span_tower", "layers": 4, "learning_rate": 3e-5});
    for v in [Version::V2, Version::V3] {
        assert_eq!(
            code(check_files_for(
                &art.rebound(&[("manifest.json", &m)]),
                CHUNK,
                v
            )),
            "schema",
            "{v:?}"
        );
    }
}

#[test]
fn the_tower_package_passes_spec_v3_and_earlier_versions_refuse_it() {
    let Some(dir) = env_path("A_TOWER_MODEL_DIR") else {
        return;
    };
    let r = fs::check_dir_for(&dir, Version::V3).unwrap();
    assert_eq!(r.graph_class, spec::TOWER_GRAPH.id);
    assert!(r.roles.token_head && r.roles.span_tower && !r.roles.span_head);
    assert_eq!(r.graph.parameters, spec::TOWER_GRAPH_PARAMETERS);
    assert_eq!(
        r.roles.heads_parameters,
        spec::DOCUMENT_HEAD_PARAMETERS + spec::token_head_parameters(256)
    );
    assert!(r.parameters > spec::MAX_PARAMETERS && r.parameters <= spec::TOWER_MAX_PARAMETERS);
    assert!(r.roles.json_value_bytes.unwrap() <= spec::MAX_JSON_VALUE_BYTES_TOWER);
    assert_eq!(r.profile_id, "slop_ninja_a_artifact_spec_v3/a-tower-f32band-v1");
    for v in [Version::V1, Version::V2] {
        assert_eq!(code(fs::check_dir_for(&dir, v)), "graph_size", "{v:?}");
    }
}

#[test]
fn the_graph_class_and_the_heads_must_agree_under_v3() {
    if let (Some(tower), Some(span)) = (env_path("A_TOWER_MODEL_DIR"), env_path("A_EXPORT_SPAN")) {
        // Heads v3 with a graph that has the encoder's token states, not the tower's.
        let files = Art { dir: tower }.with_graph(span);
        assert_eq!(
            code(check_files_for(&files, CHUNK, Version::V3)),
            "graph_class"
        );
    }
    if let (Some(span_dir), Some(tower)) =
        (env_path("A_SPAN_MODEL_DIR"), env_path("A_EXPORT_TOWER"))
    {
        // A heads v2 token head with the tower graph.
        let files = Art { dir: span_dir }.with_graph(tower);
        assert_eq!(
            code(check_files_for(&files, CHUNK, Version::V3)),
            "graph_class"
        );
    }
    if let (Some(r0), Some(tower)) = (env_path("A_R0_MODEL_DIR"), env_path("A_EXPORT_TOWER")) {
        // Document heads with the tower graph.
        let files = Art { dir: r0 }.with_graph(tower);
        assert_eq!(
            code(check_files_for(&files, CHUNK, Version::V3)),
            "graph_class"
        );
    }
    if let (Some(tower), Some(plain)) = (env_path("A_TOWER_MODEL_DIR"), env_path("A_EXPORT_PLAIN"))
    {
        // Heads v3 with a graph that has no span tower: the plain class takes no heads v3.
        let files = Art { dir: tower }.with_graph(plain);
        assert_eq!(
            code(check_files_for(&files, CHUNK, Version::V3)),
            "graph_class"
        );
    }
}

#[test]
fn tower_mutations_are_refused_under_v3() {
    let Some(dir) = env_path("A_TOWER_MODEL_DIR") else {
        return;
    };
    let art = Art { dir };
    let (heads, manifest) = (art.json("heads.json"), art.json("manifest.json"));
    let refuse = |files: Vec<TestFile>, want: &str, what: &str| {
        assert_eq!(
            code(check_files_for(&files, CHUNK, Version::V3)),
            want,
            "{what}"
        );
    };
    // The unchanged files, rebound, pass: the mutations below are the only change.
    assert_eq!(
        code(check_files_for(
            &art.rebound(&[("heads.json", &heads)]),
            CHUNK,
            Version::V3
        )),
        "accepted"
    );
    let arm = |s2: Value| {
        let mut m = manifest.clone();
        m["fit_binding"]["arm_definition"]["s2"] = s2;
        m
    };
    for (s2, what) in [
        (
            json!({"mechanism": "anchor", "teacher": "init_fit", "teacher_precision": "fp32"}),
            "an anchor s2",
        ),
        (
            json!({"mechanism": "pair_contrast", "cohort": "spliced_rewrite_v1", "class_balance": "fixed_human_mass"}),
            "a pair-contrast s2",
        ),
        (
            json!({"mechanism": "span_tower", "layers": 3, "learning_rate": 3e-5}),
            "s2.layers unlike the declaration's depth",
        ),
        (
            json!({"mechanism": "span_tower", "layers": 4, "learning_rate": 0.0}),
            "a zero tower learning rate",
        ),
        (
            json!({"mechanism": "span_tower", "layers": 4.0, "learning_rate": 3e-5}),
            "a non-integer s2.layers",
        ),
        (Value::Null, "a null s2"),
    ] {
        refuse(art.rebound(&[("manifest.json", &arm(s2))]), "schema", what);
    }
    let mut m = manifest.clone();
    m["fit_binding"]["arm_definition"]
        .as_object_mut()
        .unwrap()
        .remove("s2");
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "schema",
        "an export v4 without the arm's s2",
    );

    let mut m = manifest.clone();
    m["fit_binding"]["s2"]["rule"] = Value::from("another rule");
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "schema",
        "a fit binding with another tower rule",
    );

    let mut m = manifest.clone();
    m["fit_hashes"]["encoder_equal_to_init_fit"] = Value::from(false);
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "schema",
        "an encoder unlike the init fit's",
    );

    let mut m = manifest.clone();
    m["fit_hashes"]["frozen_document_check"]["selected_dev_iter_document_raw_vs_init"]["document_raw_max_abs_error"] =
        Value::from(1e-9);
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "schema",
        "an inexact frozen document check",
    );

    let mut m = manifest.clone();
    m["fit_hashes"]["encoder_sha256"] = Value::from("0".repeat(64));
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "schema",
        "fit hashes unlike the manifest's encoder",
    );

    // Refused `schema` with exactly this detail, so each mutation reaches its
    // own branch of the export v4 checks.
    let refuse_at = |m: &Value, detail: &str, what: &str| {
        let files = art.rebound(&[("manifest.json", m)]);
        match check_files_for(&files, CHUNK, Version::V3) {
            Ok(_) => panic!("{what}: accepted"),
            Err(e) => {
                assert_eq!(e.code(), "schema", "{what}");
                assert_eq!(e.to_string(), format!("manifest.json: {detail}"), "{what}");
            }
        }
    };
    let mut m = manifest.clone();
    m["fit_binding"]["arm_definition"]["freeze"] = Value::from("none");
    refuse_at(
        &m,
        "arm definition: s2 span tower",
        "a span tower arm that trains the document model",
    );
    for (key, value, what) in [
        ("dtype", "F16", "a tower file of another dtype"),
        ("file", "tower.bin", "a tower file of another name"),
    ] {
        let mut m = manifest.clone();
        m["span_tower"][key] = Value::from(value);
        refuse_at(&m, "span tower: weights file", what);
    }
    let init_fit = "span tower: the encoder and document heads must be the init fit's";
    let mut m = manifest.clone();
    m["fit_hashes"]["document_heads_equal_to_init_fit"] = Value::from(false);
    refuse_at(&m, init_fit, "document heads unlike the init fit's");
    let mut m = manifest.clone();
    m["fit_hashes"]["frozen_document_check"]["encoder_and_document_heads_bitwise_equal_to_init"] =
        Value::from(false);
    refuse_at(&m, init_fit, "a frozen document check that is not bitwise");
    let paths: [&[&str]; 5] = [
        &["s2_module_sha256"],
        &["selected_checkpoint_sha256"],
        &["init_fit", "encoder_sha256"],
        &["init_fit", "fit_sha256"],
        &["init_fit", "selected_checkpoint_sha256"],
    ];
    for path in paths {
        let mut m = manifest.clone();
        let mut v = &mut m["fit_hashes"];
        for k in path {
            v = &mut v[*k];
        }
        assert!(v.is_string(), "{path:?}");
        *v = Value::from("0".repeat(64));
        refuse_at(
            &m,
            "span tower: fit_hashes differ from the manifest",
            &format!("fit hashes {path:?} unlike the manifest's"),
        );
    }

    let mut m = manifest.clone();
    m["span_tower"]["tensors"] = Value::from(24);
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "schema",
        "a tower file with another tensor count",
    );

    let mut m = manifest.clone();
    m["span_tower"]["parameters"] = Value::from(spec::tower::PARAMETERS - 1);
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "schema",
        "a tower file with another parameter count",
    );

    let mut m = manifest.clone();
    m["reference_tower_rotary_modules"] = Value::from(3);
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "schema",
        "rotary modules unlike the depth",
    );

    let mut m = manifest.clone();
    m.as_object_mut().unwrap().remove("fit_hashes");
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "schema",
        "an export v4 without fit hashes",
    );

    // The manifest's declaration unlike the heads': the load guard.
    let mut m = manifest.clone();
    m["span_tower"]["declaration"]["local_attention"] = Value::from(64);
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "binding",
        "the manifest's tower unlike the heads'",
    );

    // Both declarations agree, but not with the graph class's tower.
    let mut h = heads.clone();
    h["span_tower"]["local_attention"] = Value::from(64);
    let mut m = manifest.clone();
    m["span_tower"]["declaration"]["local_attention"] = Value::from(64);
    refuse(
        art.rebound(&[("heads.json", &h), ("manifest.json", &m)]),
        "graph_class",
        "a declaration unlike the class's",
    );

    // Heads v3 declaring an encoder depth from which the span tower runtime
    // would build a layer list of that length before comparing it (8 bytes per
    // layer): refused against the tower class's declaration before the runtime
    // reads the heads, so nothing of that size is allocated. The manifest's
    // declaration is left as it is.
    let refused_tower = format!(
        "graph class: graph class {} holds another span tower than the heads declare",
        spec::TOWER_GRAPH.id
    );
    assert_eq!(
        heads["span_tower"]["source_layer"],
        Value::from(spec::tower::SOURCE_LAYER)
    );
    for encoder_layers in [u64::MAX, 1_600_000_000] {
        let mut h = heads.clone();
        h["span_tower"]["encoder_layers"] = Value::from(encoder_layers);
        let files = art.rebound(&[("heads.json", &h)]);
        let t = std::time::Instant::now();
        match check_files_for(&files, CHUNK, Version::V3) {
            Ok(_) => panic!("heads v3 with {encoder_layers} encoder layers were accepted"),
            Err(e) => assert_eq!(e.to_string(), refused_tower, "{encoder_layers}"),
        }
        eprintln!(
            "  heads v3 with {encoder_layers} encoder layers refused in {:.1?}",
            t.elapsed()
        );
    }
    // Heads v3 without a span tower, absent or null, are refused before the
    // runtime too.
    for tower in [None, Some(Value::Null)] {
        let mut h = heads.clone();
        match &tower {
            None => {
                h.as_object_mut().unwrap().remove("span_tower");
            }
            Some(v) => h["span_tower"] = v.clone(),
        }
        match check_files_for(&art.rebound(&[("heads.json", &h)]), CHUNK, Version::V3) {
            Ok(_) => panic!("heads v3 without a span tower were accepted"),
            Err(e) => assert_eq!(
                e.to_string(),
                "heads.json: heads v3 declare a span tower",
                "{tower:?}"
            ),
        }
    }

    // Heads v3 read the tower's states; another token head input is refused.
    let mut h = heads.clone();
    h["token_head"]["input"] = Value::from(spec::TOKEN_HEAD_INPUT_V2);
    let mut m = manifest.clone();
    m["token_head"]["input"] = Value::from(spec::TOKEN_HEAD_INPUT_V2);
    refuse(
        art.rebound(&[("heads.json", &h), ("manifest.json", &m)]),
        "schema",
        "a heads v3 token head on the final-norm states",
    );

    // The span tower feeds a token head; heads v3 without one, absent or
    // null, are refused, and so is an export v4 manifest without one.
    for token_head in [None, Some(Value::Null)] {
        let mut h = heads.clone();
        match &token_head {
            None => {
                h.as_object_mut().unwrap().remove("token_head");
            }
            Some(v) => h["token_head"] = v.clone(),
        }
        match check_files_for(&art.rebound(&[("heads.json", &h)]), CHUNK, Version::V3) {
            Ok(_) => panic!("heads v3 without a token head were accepted"),
            Err(e) => assert_eq!(
                e.to_string(),
                "heads.json: heads v3 carry the token head the span tower feeds",
                "{token_head:?}"
            ),
        }
    }
    let mut h = heads.clone();
    h.as_object_mut().unwrap().remove("token_head");
    let mut m = manifest.clone();
    m.as_object_mut().unwrap().remove("token_head");
    refuse(
        art.rebound(&[("heads.json", &h), ("manifest.json", &m)]),
        "schema",
        "heads v3 and an export v4 manifest without a token head",
    );

    // A million unknown top-level members, well inside the tower class's
    // value budget: refused by the heads v3 members check in one pass over
    // the members, before the span tower runtime's repeated-member check
    // (which compares each member with every member before it) reads them.
    const UNKNOWN_MEMBERS: usize = 1_000_000;
    let mut h = heads.clone();
    let members = h.as_object_mut().unwrap();
    for i in 0..UNKNOWN_MEMBERS {
        members.insert(format!("z{i:07}"), Value::Null);
    }
    let files = art.rebound(&[("heads.json", &h)]);
    drop(h);
    let t = std::time::Instant::now();
    match check_files_for(&files, CHUNK, Version::V3) {
        Ok(_) => panic!("heads v3 with unknown members were accepted"),
        Err(e) => {
            assert_eq!(e.code(), "schema");
            let detail = e.to_string();
            assert!(
                detail.starts_with("heads.json: heads v3 members: unknown field"),
                "{}",
                &detail[..detail.len().min(200)]
            );
        }
    }
    eprintln!(
        "  {UNKNOWN_MEMBERS} unknown heads v3 members refused in {:.1?}",
        t.elapsed()
    );
}

#[test]
fn the_tower_export_walks_to_its_class() {
    let Some(path) = env_path("A_EXPORT_TOWER") else {
        return;
    };
    use std::io::Read;
    let len = std::fs::metadata(&path).unwrap().len();
    assert_eq!(len, spec::TOWER_GRAPH.bytes);
    let mut scan = GraphScan::new(len, spec::graph_policy(&spec::TOWER_GRAPH));
    let mut f = std::fs::File::open(&path).unwrap();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        scan.update(&buf[..n]).unwrap();
    }
    let r = scan.finish().unwrap();
    assert_eq!(r.structure_sha256, spec::TOWER_GRAPH.structure_sha256);
    assert_eq!(r.parameters, spec::TOWER_GRAPH_PARAMETERS);
    // Under the plain and span classes' bound the walk refuses it.
    let mut scan = GraphScan::new(len, spec::GRAPH_POLICY);
    let mut f = std::fs::File::open(&path).unwrap();
    let mut refused = false;
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        if let Err(e) = scan.update(&buf[..n]) {
            assert_eq!(e.code(), "too_many_parameters");
            refused = true;
            break;
        }
    }
    assert!(refused);
}
