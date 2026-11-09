//! Spec v4: spec v1 to v3 stay exactly as published; v4 keeps every v3 value
//! and states the graph class rule as a byte comparison of the structure
//! stream with each class's pinned reference stream (decision D5).
//!
//! The artifact tests need private files and pass with a notice without them:
//! `A_R0_MODEL_DIR`, `A_SPAN_MODEL_DIR` and `A_TOWER_MODEL_DIR`, as for
//! `tests/spec_v3.rs`. Each is checked under every version that defines its
//! class, with the same results, and a structure-byte mutation of its graph is
//! refused under each while a weight change is admitted.

mod common;

use std::path::PathBuf;

use common::*;
use serde_json::json;
use slop_ninja_artifact_check::admission::SpecBounds;
use slop_ninja_artifact_check::spec::{self, Version};
use slop_ninja_artifact_check::{Refusal, fs, mapping};
use slop_ninja_protocol::FileRole;

/// The published digests of spec v1 to v3 (docs/testnet/artifact-spec.md).
const SPEC_V1_SHA256: &str = "7fceee2c93436bda1c01a8123ec4ac05807f47d1017b31200d56df0705cb7147";
const SPEC_V2_SHA256: &str = "0f25e636ba52b45269c0a4d0520c2f01550b51325bb6399d4ddae418c8384b50";
const SPEC_V3_SHA256: &str = "0bb326a93b1f4669018dadac3186f08bcc0ac527983eed0c75ce1e889e970d25";
/// Spec v4's digest.
const SPEC_V4_SHA256: &str = "7d5a105aa23968bcbc67eb1cbe18a89e3a4d3d272b69845e7bdedcda46b97ae8";

fn env_path(name: &str) -> Option<PathBuf> {
    let p = std::env::var_os(name).map(PathBuf::from);
    if p.is_none() {
        eprintln!("notice: {name} is unset; test passes without checking it");
    }
    p
}

#[test]
fn spec_v1_to_v3_are_unchanged_and_v4_has_its_digest() {
    assert_eq!(Version::V1.spec_sha256(), SPEC_V1_SHA256);
    assert_eq!(Version::V2.spec_sha256(), SPEC_V2_SHA256);
    assert_eq!(Version::V3.spec_sha256(), SPEC_V3_SHA256);
    assert_eq!(Version::V4.spec_sha256(), SPEC_V4_SHA256);
    assert_eq!(Version::V4.id(), spec::SPEC_ID_V4);
    assert_eq!(Version::LATEST, Version::V4);
    assert!(Version::V1 < Version::V2 && Version::V3 < Version::V4);
}

#[test]
fn spec_v4_changes_only_how_the_graph_class_rule_is_stated() {
    let (v3, v4) = (spec::spec_json_v3(), spec::spec_json_v4());
    let (a, b) = (v3.as_object().unwrap(), v4.as_object().unwrap());
    assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
    for (k, v) in b {
        match k.as_str() {
            "schema" => assert_eq!(v, spec::SPEC_ID_V4),
            "graph" => {}
            _ => assert_eq!(v, &a[k], "{k} must be spec v3's"),
        }
    }
    let (g3, g4) = (a["graph"].as_object().unwrap(), b["graph"].as_object().unwrap());
    for (k, v) in g4 {
        match k.as_str() {
            "classes" => {}
            "structure_stream" => {
                assert!(!g3.contains_key(k));
                assert_eq!(v["id"], slop_ninja_artifact_check::onnx::STRUCTURE_STREAM_ID);
            }
            _ => assert_eq!(v, &g3[k], "graph {k} must be spec v3's"),
        }
    }
    assert_eq!(g3["structure_digest"], slop_ninja_artifact_check::onnx::STRUCTURE_DIGEST_ID);
    assert!(!g4.contains_key("structure_digest"));
    assert_eq!(g4.len(), g3.len());
    let (c3, c4) = (g3["classes"].as_array().unwrap(), g4["classes"].as_array().unwrap());
    assert_eq!(c4.len(), c3.len());
    for ((old, new), class) in c3.iter().zip(c4).zip(spec::GRAPHS_V3.iter()) {
        let mut stripped = new.clone();
        let o = stripped.as_object_mut().unwrap();
        assert_eq!(
            o.remove("reference_structure"),
            Some(json!({"bytes": class.structure_bytes, "sha256": class.structure_stream_sha256}))
        );
        let mut old = old.clone();
        assert_eq!(old.as_object_mut().unwrap().remove("structure_sha256"), Some(json!(class.structure_sha256)));
        assert_eq!(stripped, old, "a spec v3 class changed beyond its structure rule");
    }
}

#[test]
fn spec_v4_has_spec_v3s_classes_and_bounds() {
    assert_eq!(Version::V4.classes(), Version::V3.classes());
    for g in Version::V4.classes() {
        assert!(Version::V4.serves(g) && Version::V3.serves(g));
        assert_eq!(Version::V4.max_json_value_bytes_for(g), Version::V3.max_json_value_bytes_for(g));
        assert_eq!(Version::V4.graph_class_by_len(g.bytes), Some(g));
        assert_eq!(
            mapping::profile_id_for(g, Version::V4),
            format!("{}/{}", spec::SPEC_ID_V4, g.id)
        );
    }
    assert_eq!(Version::V4.max_json_bytes(), Version::V3.max_json_bytes());
    assert_eq!(Version::V4.max_json_value_bytes(), Version::V3.max_json_value_bytes());
    assert!(Version::V4.has_tower() && Version::V3.has_tower() && !Version::V2.has_tower());
    let (b3, b4) = (SpecBounds::of(Version::V3).unwrap(), SpecBounds::of(Version::V4).unwrap());
    assert_eq!((b4.graph_bytes, b4.files_bytes, b4.object_bytes), (b3.graph_bytes, b3.files_bytes, b3.object_bytes));
    assert!(slop_ninja_artifact_check::admission::ADMISSION_SPECS.contains(&spec::SPEC_ID_V4));
}

/// The versions up to v4 that define the class of the artifact in `dir`
/// (spec v5 adds span requirements of its own: `tests/spec_v5.rs`).
fn versions_of(class: &str) -> Vec<Version> {
    up_to_v4().filter(|v| v.classes().iter().any(|g| g.id == class && v.serves(g))).collect()
}

fn up_to_v4() -> impl Iterator<Item = Version> {
    Version::ALL.into_iter().filter(|v| *v <= Version::V4)
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

const CHUNK: usize = 999_983;

/// The six files of `dir`, the graph replaced by `graph`.
fn with_graph(dir: &std::path::Path, graph: Vec<u8>) -> Vec<TestFile> {
    spec::ROLES
        .iter()
        .map(|r| TestFile {
            name: r.name.into(),
            role: r.file_role,
            source: if r.file_role == FileRole::Graph {
                Source::Bytes(graph.clone())
            } else {
                Source::Path(dir.join(r.name))
            },
            digest: None,
        })
        .collect()
}

#[test]
fn every_reference_artifact_has_the_same_results_under_each_version_of_its_class() {
    for var in ["A_R0_MODEL_DIR", "A_SPAN_MODEL_DIR", "A_TOWER_MODEL_DIR"] {
        let Some(dir) = env_path(var) else {
            continue;
        };
        let newest = fs::check_dir_for(&dir, Version::V4).unwrap();
        let class = spec::GRAPHS_V3.iter().find(|g| g.id == newest.graph_class).unwrap();
        assert_eq!(newest.graph.structure_match, Some(true), "{var}");
        assert_eq!(newest.graph.structure_bytes, class.structure_bytes, "{var}");
        for v in up_to_v4() {
            let r = fs::check_dir_for(&dir, v);
            if !versions_of(class.id).contains(&v) {
                assert!(r.is_err(), "{var} {v:?}");
                continue;
            }
            let r = r.unwrap();
            assert_eq!((r.spec, r.profile_id.clone()), (v.id(), mapping::profile_id_for(class, v)), "{var}");
            assert_eq!(r.content_sha256, newest.content_sha256, "{var} {v:?}");
            assert_eq!(r.graph_class, newest.graph_class, "{var} {v:?}");
            assert_eq!(r.graph, newest.graph, "{var} {v:?}");
            assert_eq!(r.parameters, newest.parameters, "{var} {v:?}");
        }
    }
}

#[test]
fn a_structure_byte_mutation_is_refused_and_a_weight_change_admitted_under_each_version() {
    for var in ["A_R0_MODEL_DIR", "A_SPAN_MODEL_DIR", "A_TOWER_MODEL_DIR"] {
        let Some(dir) = env_path(var) else {
            continue;
        };
        let class_id = fs::check_dir_for(&dir, Version::V4).unwrap().graph_class;
        let mut graph = std::fs::read(dir.join(spec::GRAPH_FILE)).unwrap();
        // A node's op_type changed to another allow-listed operator of the
        // same length: the walk passes and only the structure differs.
        let at = graph.windows(5).position(|w| w == b"\x22\x03Add").expect("an Add node");
        graph[at + 2..at + 5].copy_from_slice(b"Sub");
        // The producer name, a structure byte the walk does not interpret.
        let producer = graph[..64].windows(7).position(|w| w == b"pytorch").expect("the producer name");
        for (what, edit) in [("op_type", None), ("producer name", Some(producer))] {
            let mut g = graph.clone();
            if let Some(i) = edit {
                g[at + 2..at + 5].copy_from_slice(b"Add");
                g[i] = b'P';
            }
            for v in versions_of(class_id) {
                match check_files_for(&with_graph(&dir, g.clone()), CHUNK, v) {
                    Err(Refusal::GraphNotAllowed { class, difference }) => {
                        assert_eq!(class, Some(class_id), "{var} {what} {v:?}");
                        assert!(difference.is_some(), "{var} {what} {v:?}");
                    }
                    other => panic!("{var} {what} {v:?}: {}", code(other)),
                }
            }
        }
        // One bit of a weight in the middle of the file (inside an encoder
        // weight's payload): admitted under every version of the class.
        graph[at + 2..at + 5].copy_from_slice(b"Add");
        let mid = graph.len() / 2;
        graph[mid] ^= 0x01;
        for v in versions_of(class_id) {
            let r = check_files_for(&with_graph(&dir, graph.clone()), CHUNK, v).unwrap();
            assert_eq!(r.graph_class, class_id, "{var} {v:?}");
            assert_eq!(r.graph.structure_match, Some(true), "{var} {v:?}");
        }
    }
}
