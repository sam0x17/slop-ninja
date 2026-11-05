//! Spec v2: spec v1 stays exactly as published; v2 serves the span class and
//! admits heads v2 span-head artifacts, binds them with the scorer's shared
//! load guard, and bounds the JSON role files together.
//!
//! The artifact tests need private files and pass with a notice without them:
//! `A_R0_MODEL_DIR` (R0's six files), `A_SPAN_MODEL_DIR` (a span-head artifact:
//! the span export of R0's weights as `a-fp32band.onnx`, with the S0+ span head
//! package's heads, contract, calibration and export manifest re-encoded in the
//! canonical layouts by `slop_ninja_artifact_check canonical`, and the pinned
//! tokenizer) and `A_EXPORT_SPAN` (the span export alone).

mod common;

use std::path::PathBuf;

use common::*;
use serde_json::Value;
use slop_ninja_artifact_check::mapping::{check_header_for, check_listing};
use slop_ninja_artifact_check::spec::{self, Version};
use slop_ninja_artifact_check::{LOAD_GUARD_SOURCE, Refusal, canonical, fs};
use slop_ninja_protocol::{ArtifactFile, ArtifactHeader, Digest};

/// Spec v1's digest as first published (docs/testnet/artifact-spec.md).
const SPEC_V1_SHA256: &str = "7fceee2c93436bda1c01a8123ec4ac05807f47d1017b31200d56df0705cb7147";

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
fn spec_v1_is_unchanged() {
    assert_eq!(spec::spec_sha256(), SPEC_V1_SHA256);
    assert_eq!(Version::V1.spec_sha256(), SPEC_V1_SHA256);
    assert_eq!(Version::V1.spec_json(), spec::spec_json());
    assert_eq!(Version::V1.id(), spec::SPEC_ID);
}

#[test]
fn spec_v2_changes_only_what_serving_span_heads_needs() {
    let (v1, v2) = (spec::spec_json(), spec::spec_json_v2());
    let (a, b) = (v1.as_object().unwrap(), v2.as_object().unwrap());
    let added: Vec<&str> = b
        .keys()
        .filter(|k| !a.contains_key(*k))
        .map(String::as_str)
        .collect();
    assert_eq!(
        added,
        [
            "forms",
            "json_value_cost",
            "max_json_bytes",
            "max_json_value_bytes",
            "scorer_load_guard_sha256"
        ]
    );
    for (k, v) in a {
        match k.as_str() {
            "schema" => assert_eq!(b[k], spec::SPEC_ID_V2),
            "graph" | "object" => {}
            _ => assert_eq!(&b[k], v, "{k} must be spec v1's"),
        }
    }
    for c in b["graph"]["classes"].as_array().unwrap() {
        assert_eq!(c["served"], true);
        assert!(c.get("span_head").is_none() && c["token_states"].is_boolean());
    }
    assert_eq!(b["max_json_bytes"], 4 * spec::HEADS_MAX_BYTES);
    assert_eq!(b["max_json_value_bytes"], spec::MAX_JSON_VALUE_BYTES_V2);
    assert_eq!(b["json_value_cost"]["id"], canonical::VALUE_COST_ID);
    assert_ne!(Version::V2.spec_sha256(), SPEC_V1_SHA256);
}

#[test]
fn the_value_budget_is_checked_before_any_role_file_is_parsed() {
    use slop_ninja_artifact_check::{RoleFiles, check_roles_with};
    // Canonical but schema-less files: parsed, they are refused `schema`.
    let compact = b"{\"a\":[{\"a\":0},{\"a\":0}]}\n".as_slice();
    let pretty = b"{}\n".as_slice();
    let files = RoleFiles {
        heads: compact,
        contract: pretty,
        calibration: pretty,
        manifest: compact,
        digests: slop_ninja_artifact_check::roles::RoleDigests { heads: "", contract: "", calibration: "" },
    };
    let refused = |budget| check_roles_with(&files, None, Version::V2, budget).unwrap_err();
    assert_eq!(refused(None).code(), "schema");
    match refused(Some(100)) {
        Refusal::JsonValueBudget { need, budget: 100 } => assert!(need > 100),
        other => panic!("{other}"),
    }
    // Spec v1 has no value budget of its own.
    assert_eq!(
        check_roles_with(&files, None, Version::V1, None)
            .unwrap_err()
            .code(),
        "schema"
    );
}

#[test]
fn the_shared_load_guard_is_the_pinned_source() {
    assert_eq!(
        sha(LOAD_GUARD_SOURCE),
        spec::SCORER_LOAD_GUARD_SHA256,
        "load_guard.rs changed: review it, then update spec::SCORER_LOAD_GUARD_SHA256 (a spec v2 change)"
    );
}

#[test]
fn versions_are_named_by_id_and_digest() {
    for v in Version::ALL {
        assert_eq!(Version::from_id(v.id()), Some(v));
        assert_eq!(Version::named(v.id(), &v.spec_sha256()), Some(v));
        assert_eq!(Version::named(v.id(), &"0".repeat(64)), None);
    }
    // Spec v4 (tests/spec_v4.rs) is the newest version.
    assert_eq!(Version::from_id(spec::SPEC_ID_V2), Some(Version::V2));
    assert_eq!(Version::LATEST, Version::V4);
}

/// A header with the six files: the graph at `graph_len`, the pinned
/// tokenizer, and small JSON files except `calibration.json` at `cal_len`.
fn header(graph_len: u64, cal_len: u64) -> ArtifactHeader {
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
                "calibration.json" => (Digest([2; 32]), cal_len),
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
fn the_span_class_is_served_under_v2_only() {
    let span = spec::GRAPHS[1].bytes;
    assert_eq!(
        check_header_for(&header(span, 1000), Version::V1),
        Err(Refusal::GraphClassNotServed("a-span-f32band-v1"))
    );
    check_header_for(&header(span, 1000), Version::V2).unwrap();
    for v in Version::ALL {
        check_header_for(&header(spec::GRAPHS[0].bytes, 1000), v).unwrap();
        assert_eq!(
            check_header_for(&header(span - 1, 1000), v),
            Err(Refusal::GraphSize(span - 1))
        );
    }
}

#[test]
fn spec_v2_bounds_the_json_role_files_together() {
    let plain = spec::GRAPHS[0].bytes;
    // Three small files of 1,000 bytes: the calibration may take the rest.
    let room = spec::MAX_JSON_BYTES_V2 - 3000;
    check_header_for(&header(plain, room), Version::V2).unwrap();
    assert_eq!(
        check_header_for(&header(plain, room + 1), Version::V2),
        Err(Refusal::JsonBudget {
            need: spec::MAX_JSON_BYTES_V2 + 1,
            budget: spec::MAX_JSON_BYTES_V2
        })
    );
    // Spec v1 has no such bound.
    check_header_for(&header(plain, room + 1), Version::V1).unwrap();
}

#[test]
fn a_parts_manifest_listing_is_checked_like_a_header() {
    let h = header(spec::GRAPHS[1].bytes, 1000);
    let hexes: Vec<String> = h.files.iter().map(|f| hex::encode(f.sha256.0)).collect();
    let listing: Vec<(&str, &str, u64)> = h
        .files
        .iter()
        .zip(&hexes)
        .map(|(f, d)| (f.name.as_str(), d.as_str(), f.len))
        .collect();
    check_listing(spec::CONTRACT_ID, &listing, Version::V2).unwrap();
    assert_eq!(
        check_listing(spec::CONTRACT_ID, &listing, Version::V1),
        Err(Refusal::GraphClassNotServed("a-span-f32band-v1"))
    );
    let mut extra = listing.clone();
    extra.push(("notes.txt", hexes[0].as_str(), 10));
    assert_eq!(
        check_listing(spec::CONTRACT_ID, &extra, Version::V2),
        Err(Refusal::UndeclaredFile("notes.txt".into()))
    );
    let mut twice = listing.clone();
    twice.push(listing[0]);
    assert!(check_listing(spec::CONTRACT_ID, &twice, Version::V2).is_err());
    assert!(matches!(
        check_listing("another_contract", &listing, Version::V2),
        Err(Refusal::Contract(_))
    ));
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
                manifest = (*v).clone();
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
fn r0_passes_spec_v2_with_its_plain_class() {
    let Some(dir) = env_path("A_R0_MODEL_DIR") else {
        return;
    };
    let v1 = fs::check_dir(&dir).unwrap();
    let v2 = fs::check_dir_for(&dir, Version::V2).unwrap();
    assert_eq!(v2.spec, spec::SPEC_ID_V2);
    assert_eq!(v2.graph_class, "a-plain-f32band-v1");
    assert!(!v2.roles.span_head && !v2.roles.token_head);
    // The content digest a miner commits does not depend on the spec version;
    // the package's profile id does.
    assert_eq!(v1.content_sha256, v2.content_sha256);
    assert_eq!(v2.profile_id, "slop_ninja_a_artifact_spec_v2/a-plain-f32band-v1");
    assert_ne!(v1.profile_id, v2.profile_id);
}

/// R0's export manifest with `extra` set to a canonical array of `n` copies
/// of `item`, rebound into R0's files.
fn r0_with_manifest_key(art: &Art, key: &str, item: Value, n: usize) -> Vec<TestFile> {
    let mut m = art.json("manifest.json");
    m[key] = Value::Array(vec![item; n]);
    art.rebound(&[("manifest.json", &m)])
}

#[test]
fn hostile_json_within_the_byte_budget_is_refused_before_it_is_parsed() {
    let Some(dir) = env_path("A_R0_MODEL_DIR") else {
        return;
    };
    let art = Art { dir };
    let r0 = fs::check_dir_for(&art.dir, Version::V2).unwrap();
    let r0_cost = r0.roles.json_value_bytes.unwrap();
    eprintln!(
        "R0: JSON value cost {r0_cost}, 1/{} of the budget",
        spec::MAX_JSON_VALUE_BYTES_V2 / r0_cost
    );
    assert!(fs::check_dir(&art.dir).unwrap().roles.json_value_bytes.is_none());
    // `{"a":0},` is 8 bytes and costs a map node: past the value budget at
    // about 20 MB, far inside the 256 MiB byte budget.
    let per_item = canonical::value_cost::OBJECT + 2 * (canonical::value_cost::TEXT + 1);
    let n = (spec::MAX_JSON_VALUE_BYTES_V2 / per_item) as usize + 1;
    let hostile = r0_with_manifest_key(&art, "diagnostic_train_subset", serde_json::json!({"a": 0}), n);
    let len: u64 = hostile
        .iter()
        .filter(|f| f.name.ends_with(".json") && f.name != "tokenizer.json")
        .map(|f| match &f.source {
            Source::Bytes(b) => b.len() as u64,
            Source::Path(p) => std::fs::metadata(p).unwrap().len(),
        })
        .sum();
    assert!(len < spec::MAX_JSON_BYTES_V2, "inside the byte budget");
    assert_eq!(code(check_files_for(&hostile, CHUNK, Version::V2)), "json_value_budget");
    // Under the value budget the same key is parsed and refused by its schema.
    let under = r0_with_manifest_key(&art, "diagnostic_train_subset", serde_json::json!({"a": 0}), 10_000);
    assert_eq!(code(check_files_for(&under, CHUNK, Version::V2)), "schema");
}

#[test]
fn the_span_head_artifact_passes_spec_v2_and_spec_v1_refuses_it() {
    let Some(dir) = env_path("A_SPAN_MODEL_DIR") else {
        return;
    };
    let r = fs::check_dir_for(&dir, Version::V2).unwrap();
    assert_eq!(r.graph_class, "a-span-f32band-v1");
    assert!(r.roles.token_head && !r.roles.span_head);
    assert!(r.parameters <= spec::MAX_PARAMETERS);
    assert_eq!(code(fs::check_dir(&dir)), "graph_class_not_served");
}

#[test]
fn the_graph_class_and_the_heads_must_agree_under_v2() {
    if let (Some(r0), Some(span)) = (env_path("A_R0_MODEL_DIR"), env_path("A_EXPORT_SPAN")) {
        // Document heads with a graph that outputs token states nobody reads.
        let files = Art { dir: r0 }.with_graph(span);
        assert_eq!(
            code(check_files_for(&files, CHUNK, Version::V2)),
            "graph_class"
        );
    }
    if let (Some(span_dir), Some(r0)) = (env_path("A_SPAN_MODEL_DIR"), env_path("A_R0_MODEL_DIR")) {
        // A token head with a graph that has no token states.
        let files = Art { dir: span_dir }.with_graph(r0.join(spec::GRAPH_FILE));
        assert_eq!(
            code(check_files_for(&files, CHUNK, Version::V2)),
            "graph_class"
        );
    }
}

#[test]
fn span_head_mutations_are_refused_under_v2() {
    let Some(dir) = env_path("A_SPAN_MODEL_DIR") else {
        return;
    };
    let art = Art { dir };
    let (heads, cal, manifest) = (
        art.json("heads.json"),
        art.json("calibration.json"),
        art.json("manifest.json"),
    );
    let refuse = |files: Vec<TestFile>, want: &str, what: &str| {
        assert_eq!(
            code(check_files_for(&files, CHUNK, Version::V2)),
            want,
            "{what}"
        );
    };
    // The unchanged files, rebound, pass: the mutations below are the only change.
    assert_eq!(
        code(check_files_for(
            &art.rebound(&[("heads.json", &heads)]),
            CHUNK,
            Version::V2
        )),
        "accepted"
    );

    let mut m = manifest.clone();
    m["token_head"]["hidden"] = Value::from(255);
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "binding",
        "manifest token head differs from the heads'",
    );

    let mut c = cal.clone();
    c["spans"]["decoder"]["tau_hi"] = Value::from(0.75);
    refuse(
        art.rebound(&[("calibration.json", &c)]),
        "binding",
        "calibration span spec differs from the heads'",
    );

    let mut c = cal.clone();
    c.as_object_mut().unwrap().remove("spans");
    refuse(
        art.rebound(&[("calibration.json", &c)]),
        "schema",
        "calibration v3 without its span spec",
    );

    let mut h = heads.clone();
    h["supervision"]
        .as_object_mut()
        .unwrap()
        .remove("token_head");
    refuse(
        art.rebound(&[("heads.json", &h)]),
        "schema",
        "heads v2 supervision without token_head",
    );

    let mut m = manifest.clone();
    m["fit_binding"]
        .as_object_mut()
        .unwrap()
        .remove("spans_rule_sha256");
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "schema",
        "export v3 fit binding without spans_rule_sha256",
    );

    let mut m = manifest.clone();
    m["fit_binding"]["arm_definition"]["token_head"] = Value::Null;
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "schema",
        "a frozen document model with no token head",
    );

    let mut m = manifest.clone();
    m["diagnostic_train_subset"] = Value::from("smoke");
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "schema",
        "a diagnostic Train-subset export",
    );

    let mut m = manifest.clone();
    m["span_tower"] = Value::Null;
    refuse(
        art.rebound(&[("manifest.json", &m)]),
        "schema",
        "a span tower key in an export v3",
    );

    // The same span-head files under spec v1: the span class is not served.
    assert_eq!(
        code(check_files_for(&art.files(), CHUNK, Version::V1)),
        "graph_class_not_served"
    );
}
