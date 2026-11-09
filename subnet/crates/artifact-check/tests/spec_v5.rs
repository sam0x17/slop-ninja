//! Spec v5: spec v1 to v4 stay exactly as published; v5 keeps every v4 value
//! and adds two refusals, `token_head_required` and `span_decoder_floor`
//! (the span decoder rule with a minimum of at least 24 words). It is defined
//! and admitted where a cohort names it, and it is not the default.
//!
//! The artifact tests need private files and pass with a notice without them:
//! `A_R0_MODEL_DIR` (R0's six files: document heads only),
//! `A_TOWER_MODEL_DIR` (the S2c span package, decoder minimum 10),
//! `A_S0P_MODEL_DIR` (an S0+ span-class package, decoder minimum 5) and
//! `A_D24_MODEL_DIR` (SlopNinja's first span package: the S2c tower with the
//! 24-word decoder).

mod common;

use std::path::PathBuf;

use common::*;
use serde_json::{Value, json};
use slop_ninja_artifact_check::admission::{ADMISSION_SPECS, SpecBounds};
use slop_ninja_artifact_check::spec::{self, Version};
use slop_ninja_artifact_check::{Refusal, canonical, fs};

/// The published digests of spec v1 to v4 (docs/testnet/artifact-spec.md).
const SPEC_V1_SHA256: &str = "7fceee2c93436bda1c01a8123ec4ac05807f47d1017b31200d56df0705cb7147";
const SPEC_V2_SHA256: &str = "0f25e636ba52b45269c0a4d0520c2f01550b51325bb6399d4ddae418c8384b50";
const SPEC_V3_SHA256: &str = "0bb326a93b1f4669018dadac3186f08bcc0ac527983eed0c75ce1e889e970d25";
const SPEC_V4_SHA256: &str = "7d5a105aa23968bcbc67eb1cbe18a89e3a4d3d272b69845e7bdedcda46b97ae8";
/// Spec v5's digest.
const SPEC_V5_SHA256: &str = "3bc7560344eca8cba27e44b81b2f7683478e6ef23036de6a4f474d65fa8814a1";

fn env_path(name: &str) -> Option<PathBuf> {
    let p = std::env::var_os(name).map(PathBuf::from);
    if p.is_none() {
        eprintln!("notice: {name} is unset; test passes without checking it");
    }
    p
}

fn sha(b: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(b))
}

#[test]
fn spec_v1_to_v4_are_unchanged_and_v5_has_its_digest() {
    assert_eq!(Version::V1.spec_sha256(), SPEC_V1_SHA256);
    assert_eq!(Version::V2.spec_sha256(), SPEC_V2_SHA256);
    assert_eq!(Version::V3.spec_sha256(), SPEC_V3_SHA256);
    assert_eq!(Version::V4.spec_sha256(), SPEC_V4_SHA256);
    assert_eq!(Version::V5.spec_sha256(), SPEC_V5_SHA256);
    assert_eq!(Version::V5.id(), spec::SPEC_ID_V5);
    assert_eq!(Version::from_id(spec::SPEC_ID_V5), Some(Version::V5));
    // Defined and admitted, not the default.
    assert_eq!(Version::LATEST, Version::V4);
    assert!(ADMISSION_SPECS.contains(&spec::SPEC_ID_V5));
    assert!(Version::V5.requires_token_head() && !Version::V4.requires_token_head());
    assert_eq!(Version::V5.span_decoder_floor(), Some(("slop_ninja_span_decoder_v1", 24)));
    assert_eq!(Version::V4.span_decoder_floor(), None);
}

#[test]
fn spec_v5_adds_only_the_span_requirements() {
    let (v4, v5) = (spec::spec_json_v4(), spec::spec_json_v5());
    let (a, b) = (v4.as_object().unwrap(), v5.as_object().unwrap());
    for (k, v) in b {
        match k.as_str() {
            "schema" => assert_eq!(v, spec::SPEC_ID_V5),
            "span_requirements" => {
                assert!(!a.contains_key(k));
                assert_eq!(v["span_decoder_floor"]["rule"], "slop_ninja_span_decoder_v1");
                assert_eq!(v["span_decoder_floor"]["min_words"], 24);
                assert!(v["token_head_required"].is_string());
            }
            _ => assert_eq!(v, &a[k], "{k} must be spec v4's"),
        }
    }
    assert_eq!(b.len(), a.len() + 1);
    // The same classes, bounds and object sizes as v4.
    assert_eq!(Version::V5.classes(), Version::V4.classes());
    for g in Version::V5.classes() {
        assert!(Version::V5.serves(g));
        assert_eq!(Version::V5.max_json_value_bytes_for(g), Version::V4.max_json_value_bytes_for(g));
    }
    let (b4, b5) = (SpecBounds::of(Version::V4).unwrap(), SpecBounds::of(Version::V5).unwrap());
    assert_eq!((b5.graph_bytes, b5.files_bytes, b5.object_bytes), (b4.graph_bytes, b4.files_bytes, b4.object_bytes));
    assert_eq!(SpecBounds::of_id(spec::SPEC_ID_V5).unwrap().spec_sha256, SPEC_V5_SHA256);
}

#[test]
fn the_new_refusals_have_stable_codes() {
    assert_eq!(Refusal::TokenHeadRequired.code(), "token_head_required");
    assert_eq!(Refusal::SpanDecoderFloor(String::new()).code(), "span_decoder_floor");
}

// ----------------------------------------------------------- private files

const CHUNK: usize = 999_983;

fn code<T>(r: Result<T, Refusal>) -> String {
    match r {
        Ok(_) => "accepted".into(),
        Err(e) => {
            eprintln!("  refused {}: {e}", e.code());
            e.code().into()
        }
    }
}

fn enc(name: &str, v: &Value) -> Vec<u8> {
    canonical::encode(v, spec::role_by_name(name).unwrap().layout.unwrap())
}

/// The six files of `dir` with the decoder of the heads' span emission spec,
/// the export manifest's and the calibration record's set to `decoder`, and
/// the export manifest's heads and calibration digests rebound.
fn with_decoder(dir: &std::path::Path, decoder: &Value) -> Vec<TestFile> {
    let json = |n: &str| -> Value { serde_json::from_slice(&std::fs::read(dir.join(n)).unwrap()).unwrap() };
    let (mut heads, mut manifest, mut cal) = (json("heads.json"), json("manifest.json"), json("calibration.json"));
    heads["span_emission"]["decoder"] = decoder.clone();
    manifest["span_emission"]["decoder"] = decoder.clone();
    cal["spans"]["decoder"] = decoder.clone();
    let (hb, cb) = (enc("heads.json", &heads), enc("calibration.json", &cal));
    manifest["heads"]["sha256"] = json!(sha(&hb));
    manifest["calibration"]["sha256"] = json!(sha(&cb));
    let mb = enc("manifest.json", &manifest);
    spec::ROLES
        .iter()
        .map(|r| TestFile {
            name: r.name.into(),
            role: r.file_role,
            source: match r.name {
                "heads.json" => Source::Bytes(hb.clone()),
                "calibration.json" => Source::Bytes(cb.clone()),
                "manifest.json" => Source::Bytes(mb.clone()),
                n => Source::Path(dir.join(n)),
            },
            digest: None,
        })
        .collect()
}

#[test]
fn document_heads_alone_are_refused_under_v5_and_admitted_under_v4() {
    let Some(dir) = env_path("A_R0_MODEL_DIR") else {
        return;
    };
    fs::check_dir_for(&dir, Version::V4).unwrap();
    assert_eq!(code(fs::check_dir_for(&dir, Version::V5)), "token_head_required");
}

#[test]
fn decoders_under_the_floor_are_refused_under_v5() {
    for (var, min) in [("A_TOWER_MODEL_DIR", 10), ("A_S0P_MODEL_DIR", 5)] {
        let Some(dir) = env_path(var) else {
            continue;
        };
        let heads: Value = serde_json::from_slice(&std::fs::read(dir.join("heads.json")).unwrap()).unwrap();
        assert_eq!(heads["span_emission"]["decoder"]["min_words"], min, "{var}");
        fs::check_dir_for(&dir, Version::V4).unwrap();
        assert_eq!(code(fs::check_dir_for(&dir, Version::V5)), "span_decoder_floor", "{var}");
    }
}

#[test]
fn the_first_span_package_is_admitted_alike_under_v4_and_v5() {
    let Some(dir) = env_path("A_D24_MODEL_DIR") else {
        return;
    };
    let v4 = fs::check_dir_for(&dir, Version::V4).unwrap();
    let v5 = fs::check_dir_for(&dir, Version::V5).unwrap();
    assert_eq!(v5.spec, spec::SPEC_ID_V5);
    assert_eq!(v5.graph_class, spec::TOWER_GRAPH.id);
    assert_eq!(v5.graph_class, v4.graph_class);
    assert_eq!(v5.content_sha256, v4.content_sha256);
    assert_eq!(v5.parameters, v4.parameters);
    assert_eq!(v5.roles, v4.roles);
    assert_eq!(v5.graph, v4.graph);
    assert_eq!(v5.profile_id, format!("{}/{}", spec::SPEC_ID_V5, v4.graph_class));
}

#[test]
fn the_floor_is_checked_at_its_boundary_and_on_the_rule() {
    let Some(dir) = env_path("A_D24_MODEL_DIR") else {
        return;
    };
    let heads: Value = serde_json::from_slice(&std::fs::read(dir.join("heads.json")).unwrap()).unwrap();
    let d24 = heads["span_emission"]["decoder"].clone();
    assert_eq!(d24["min_words"], 24);
    let mut at = d24.clone();
    // Rebound but unchanged: admitted.
    assert_eq!(code(check_files_for(&with_decoder(&dir, &at), CHUNK, Version::V5)), "accepted");
    at["min_words"] = json!(23);
    assert_eq!(code(check_files_for(&with_decoder(&dir, &at), CHUNK, Version::V5)), "span_decoder_floor");
    assert_eq!(code(check_files_for(&with_decoder(&dir, &at), CHUNK, Version::V4)), "accepted");
    at["min_words"] = json!(25);
    assert_eq!(code(check_files_for(&with_decoder(&dir, &at), CHUNK, Version::V5)), "accepted");
    // A decoder the heads runtime does not run is refused before the floor.
    let mut other = d24.clone();
    other["rule"] = json!("slop_ninja_span_decoder_v3_segment");
    assert_ne!(code(check_files_for(&with_decoder(&dir, &other), CHUNK, Version::V5)), "accepted");
}
