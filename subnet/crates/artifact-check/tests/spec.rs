//! Spec consistency, the pins on the scorer's sources (heads runtime, load
//! guard, parts limits) and the parts mapping layout.

use slop_ninja_artifact_check::files_manifest::FilesManifest;
use slop_ninja_artifact_check::mapping::profile_id;
use slop_ninja_artifact_check::{HEADS_RUNTIME_SOURCES, ObjectCheck, Refusal, spec, text};
use slop_ninja_protocol::{ArtifactFile, ArtifactHeader, Digest, FileRole, Record};

fn sha(b: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(b))
}

#[test]
fn compiled_heads_runtime_is_the_spec_pin() {
    for ((name, bytes), (pin_name, pin)) in
        HEADS_RUNTIME_SOURCES.iter().zip(spec::HEADS_RUNTIME_SHA256)
    {
        assert_eq!(*name, pin_name);
        assert_eq!(
            sha(bytes),
            pin,
            "{name} differs from the spec's heads runtime pin"
        );
    }
}

/// The scorer's served profile must pin exactly the heads runtime the checker
/// compiles: a scorer refuses to start (`main.rs` `check_heads_runtime`) on
/// any other source, and the checker's heads parse would no longer be the
/// scorer's.
#[test]
fn the_scorer_pins_the_same_heads_runtime() {
    let profile: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../inference/a-container-serve/profiles/a-baseline-rc-s17-f32band.json"
    ))
    .unwrap();
    let pins = profile["heads_runtime_sha256"].as_object().unwrap();
    assert_eq!(pins.len(), spec::HEADS_RUNTIME_SHA256.len());
    for (name, pin) in spec::HEADS_RUNTIME_SHA256 {
        let scorer = pins[name].as_str().unwrap();
        assert_eq!(
            scorer, pin,
            "{name}: the scorer pins {scorer}, the checker compiles {pin}"
        );
    }
}

/// The scorer's `guard` source, from `pub fn guard(` to the first lone `}`.
fn scorer_guard_source() -> &'static str {
    let engine = include_str!("../../../../inference/a-container-serve/src/engine.rs");
    let start = engine.find("pub fn guard(").expect("guard");
    let end = start + engine[start..].find("\n}\n").expect("guard end") + 3;
    &engine[start..end]
}

#[test]
fn the_scorer_guard_is_the_one_the_mirror_repeats() {
    assert_eq!(
        sha(scorer_guard_source().as_bytes()),
        spec::SCORER_GUARD_SHA256,
        "the scorer's load guard changed: re-derive src/guard.rs from it, then update spec::SCORER_GUARD_SHA256"
    );
}

#[test]
fn the_size_bounds_are_the_scorer_and_heads_runtime_limits() {
    let artifact = include_str!("../../../../inference/a-container-serve/src/artifact.rs");
    // The scorer takes its transfer and per-file bounds from the spec, and
    // its manifest type and bound from this crate's files manifest v2.
    for line in [
        "pub const MAX_RANGE_BYTES: u64 = spec::PART_BYTES;",
        "pub const MAX_CHUNKS_PER_FILE: u64 = spec::MAX_PARTS_PER_FILE;",
        "FilesManifest, MAX_MANIFEST_BYTES};",
        "ensure!(f.bytes > 0 && chunks <= MAX_CHUNKS_PER_FILE",
    ] {
        assert!(
            artifact.contains(line),
            "artifact.rs no longer has {line:?}"
        );
    }
    assert_eq!(spec::PART_BYTES, 64 << 20);
    assert_eq!(spec::MAX_MANIFEST_BYTES, 1 << 20);
    assert_eq!(slop_ninja_artifact_check::files_manifest::MAX_MANIFEST_BYTES as u64, spec::MAX_MANIFEST_BYTES);
    assert_eq!(spec::MAX_PARTS_PER_FILE, 64);
    // The heads runtime (pinned by digest above) refuses larger heads.
    let full_a = std::str::from_utf8(HEADS_RUNTIME_SOURCES[0].1).unwrap();
    assert!(full_a.contains("bytes.len() <= 64 * 1024 * 1024"));
    assert_eq!(spec::HEADS_MAX_BYTES, 64 * 1024 * 1024);
}

#[test]
fn the_exporter_records_the_spec_class_digests() {
    let script = include_str!("../../../../grammar/crates/grammar-eval/python/export_a_onnx.py");
    assert!(script.contains(&format!(
        "STRUCTURE_TAG = b\"{}\\0\"",
        slop_ninja_artifact_check::onnx::STRUCTURE_DIGEST_ID
    )));
    for (variant, class) in [("plain", &spec::GRAPHS[0]), ("span", &spec::GRAPHS[1])] {
        assert!(
            script.contains(&format!("\"{variant}\": \"{}\"", class.structure_sha256)),
            "{variant}"
        );
    }
}

#[test]
fn spec_values_are_consistent() {
    let mut ops = spec::OPS.to_vec();
    ops.sort();
    ops.dedup();
    assert_eq!(ops, spec::OPS, "operator allow-list sorted and unique");
    let names: Vec<_> = spec::ROLES.iter().map(|r| r.name).collect();
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), names.len());
    assert!(names.contains(&spec::GRAPH_FILE));
    assert_ne!(
        spec::GRAPHS[0].structure_sha256,
        spec::GRAPHS[1].structure_sha256
    );
    assert_ne!(spec::GRAPHS[0].bytes, spec::GRAPHS[1].bytes);
    assert!(spec::GRAPHS[0].served && !spec::GRAPHS[0].span_head);
    // The span class is allow-listed but not served until the scorer serves
    // span heads.
    assert!(!spec::GRAPHS[1].served && spec::GRAPHS[1].span_head);
    assert!(
        spec::GRAPHS.iter().all(
            |g| g.bytes <= spec::MAX_FILE_BYTES && spec::graph_class_by_len(g.bytes) == Some(g)
        )
    );
    // Only the JSON role files have a layout; the heads bound is the runtime's.
    for r in &spec::ROLES {
        assert_eq!(
            r.layout.is_some(),
            r.name.ends_with(".json") && r.name != "tokenizer.json"
        );
        assert!(r.max_bytes <= spec::MAX_FILE_BYTES);
    }
    assert_eq!(
        spec::role_by_name("heads.json").unwrap().max_bytes,
        spec::HEADS_MAX_BYTES
    );
    assert!(
        spec::GRAPHS
            .iter()
            .all(|g| g.outputs.ends_with(&["h"]) || g.outputs.contains(&"h"))
    );
    // Every inference role is used once; manifest.json is owner metadata.
    for role in [
        FileRole::Graph,
        FileRole::Weights,
        FileRole::Tokenizer,
        FileRole::Calibration,
        FileRole::OutputSchema,
    ] {
        assert_eq!(
            spec::ROLES.iter().filter(|r| r.file_role == role).count(),
            1
        );
    }
    assert_eq!(spec::spec_sha256().len(), 64);
    assert_eq!(spec::spec_json()["text_profile"], text::profile_json());
}

fn header(files: &[(&str, FileRole, u64, [u8; 32])]) -> ArtifactHeader {
    let mut files: Vec<_> = files
        .iter()
        .map(|(n, r, len, d)| ArtifactFile {
            name: (*n).into(),
            role: *r,
            sha256: Digest(*d),
            len: *len,
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    ArtifactHeader {
        contract: spec::CONTRACT_ID.into(),
        files,
    }
}

fn tokenizer_digest() -> [u8; 32] {
    hex::decode(spec::TOKENIZER_SHA256)
        .unwrap()
        .try_into()
        .unwrap()
}

fn table() -> Vec<(&'static str, FileRole, u64, [u8; 32])> {
    spec::ROLES
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let (len, d) = if r.file_role == FileRole::Tokenizer {
                (spec::TOKENIZER_BYTES, tokenizer_digest())
            } else if r.name == spec::GRAPH_FILE {
                (spec::GRAPHS[0].bytes, [1; 32])
            } else {
                (10 + i as u64, [i as u8 + 2; 32])
            };
            (r.name, r.file_role, len, d)
        })
        .collect()
}

#[test]
fn the_files_manifest_v2_has_the_scorers_order_and_binds_its_package() {
    let h = header(&table());
    let m = FilesManifest::for_header(&h, slop_ninja_artifact_check::Version::V1, &profile_id(&spec::GRAPHS[0]), |name, sha| {
        ("admission".into(), format!("admission/v2/c1/{}/f/{sha}", "ab".repeat(32)), Some(format!("\"etag-{name}\"")))
    })
    .unwrap();
    let names: Vec<_> = m.files.iter().map(|f| f.name.as_str()).collect();
    let table_order: Vec<_> = spec::ROLES.iter().map(|r| r.name).collect();
    assert_eq!(names, table_order, "scorer file order, not header order");
    assert_eq!(m.profile_id, "slop_ninja_a_artifact_spec_v1/a-plain-f32band-v1");
    let bytes = m.to_bytes();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.starts_with("{\n  \"schema\": \"slop_ninja_a_artifact_files_v2\",\n  \"profile_id\""));
    assert!(text.ends_with("}\n"));
    let package = FilesManifest::package_of(&bytes);
    let back = FilesManifest::parse(&bytes, &package, slop_ninja_artifact_check::Version::V1).unwrap();
    assert_eq!(back, m);
    assert_eq!(back.header().unwrap(), h);
    // Another digest, a changed byte, a reordered table and an unknown
    // bucket are refused.
    assert!(FilesManifest::parse(&bytes, &"0".repeat(64), slop_ninja_artifact_check::Version::V1).is_err());
    let mut swapped = m.clone();
    swapped.files.swap(0, 1);
    let sb = swapped.to_bytes();
    assert!(FilesManifest::parse(&sb, &FilesManifest::package_of(&sb), slop_ninja_artifact_check::Version::V1).is_err());
    let mut bucket = m.clone();
    bucket.files[0].bucket = "public".into();
    let bb = bucket.to_bytes();
    assert!(FilesManifest::parse(&bb, &FilesManifest::package_of(&bb), slop_ninja_artifact_check::Version::V1).is_err());
}

#[test]
fn header_checks_refuse_contract_names_roles_and_tokenizer() {
    use slop_ninja_artifact_check::mapping::check_header;
    assert!(check_header(&header(&table())).is_ok());
    let mut h = header(&table());
    h.contract = "other_contract".into();
    assert!(matches!(check_header(&h), Err(Refusal::Contract(_))));
    let mut t = table();
    t.push(("extra.bin", FileRole::ReferenceVectors, 1, [9; 32]));
    assert!(matches!(
        check_header(&header(&t)),
        Err(Refusal::UndeclaredFile(_))
    ));
    let mut t = table();
    t.retain(|f| f.0 != "manifest.json");
    assert_eq!(
        check_header(&header(&t)),
        Err(Refusal::MissingFile("manifest.json"))
    );
    let mut t = table();
    t.iter_mut().find(|f| f.0 == "tokenizer.json").unwrap().3[0] ^= 1;
    assert_eq!(check_header(&header(&t)), Err(Refusal::Tokenizer));
    let mut t = table();
    t.iter_mut().find(|f| f.0 == "manifest.json").unwrap().1 = FileRole::Weights;
    assert!(matches!(
        check_header(&header(&t)),
        Err(Refusal::Role { .. })
    ));
}

fn with_len(name: &str, len: u64) -> ArtifactHeader {
    let mut t = table();
    t.iter_mut().find(|f| f.0 == name).unwrap().2 = len;
    header(&t)
}

#[test]
fn header_checks_refuse_lengths_no_runtime_reads() {
    use slop_ninja_artifact_check::mapping::check_header;
    let code = |h: ArtifactHeader| check_header(&h).map_err(|e| e.code());
    // Past the scorer's 64 parts of 64 MiB, or empty.
    assert_eq!(
        code(with_len("calibration.json", spec::MAX_FILE_BYTES + 1)),
        Err("file_size")
    );
    assert_eq!(
        code(with_len("calibration.json", spec::MAX_FILE_BYTES)),
        Ok(())
    );
    assert_eq!(code(with_len("contract.json", 0)), Err("file_size"));
    // Past the heads runtime's limit.
    assert_eq!(
        code(with_len("heads.json", spec::HEADS_MAX_BYTES + 1)),
        Err("file_size")
    );
    assert_eq!(code(with_len("heads.json", spec::HEADS_MAX_BYTES)), Ok(()));
    // A graph length is a class's exact length; the span class is not served.
    assert_eq!(
        code(with_len(spec::GRAPH_FILE, spec::GRAPHS[0].bytes + 1)),
        Err("graph_size")
    );
    assert_eq!(code(with_len(spec::GRAPH_FILE, 4 << 30)), Err("graph_size"));
    assert_eq!(
        code(with_len(spec::GRAPH_FILE, spec::GRAPHS[1].bytes)),
        Err("graph_class_not_served")
    );
}

#[test]
fn a_json_budget_refuses_the_header_before_any_file_byte() {
    let h = with_len("calibration.json", 3 << 30);
    let need = slop_ninja_artifact_check::mapping::json_buffer_bytes(&h);
    assert_eq!(
        need,
        h.files
            .iter()
            .filter(|f| spec::role_by_name(&f.name).unwrap().layout.is_some())
            .map(|f| f.len)
            .sum::<u64>()
    );
    let hb = h.to_bytes().unwrap();
    let mut object = (hb.len() as u32).to_le_bytes().to_vec();
    object.extend_from_slice(&hb);
    let mut check = ObjectCheck::with_json_budget(need - 1);
    assert_eq!(
        check.update(&object),
        Err(Refusal::JsonBudget {
            need,
            budget: need - 1
        })
    );
    let mut check = ObjectCheck::with_json_budget(need);
    check.update(&object).unwrap();
    assert!(check.header().is_some());
}

/// `docs/testnet/artifact-spec.md` quotes the spec and text-profile digests
/// that `slop_ninja_artifact_check spec` prints, and lists the AI profile
/// names whose compact JSON array is the pinned roster digest. A spec change
/// fails here until the document is updated.
#[test]
fn the_spec_document_matches_the_compiled_spec() {
    let doc = include_str!("../../../../docs/testnet/artifact-spec.md");
    for (name, digest) in [
        ("spec", spec::spec_sha256()),
        ("spec v2", spec::Version::V2.spec_sha256()),
        ("spec v3", spec::Version::V3.spec_sha256()),
        ("text profile", text::profile_sha256()),
    ] {
        assert!(
            doc.contains(&format!("`{digest}`")),
            "docs/testnet/artifact-spec.md does not quote the {name} digest {digest}"
        );
    }
    let heading = doc
        .find("### AI profile names")
        .expect("the AI profile names section");
    let rest = &doc[heading..];
    let open = rest.find("```json\n").expect("the roster block") + "```json\n".len();
    let close = rest[open..]
        .find("```")
        .expect("the end of the roster block");
    let names: Vec<String> = serde_json::from_str(&rest[open..open + close]).unwrap();
    assert_eq!(names.len(), spec::contract::AI_PROFILES);
    assert_eq!(
        sha(serde_json::to_string(&names).unwrap().as_bytes()),
        spec::contract::AI_PROFILES_SHA256,
        "the document's AI profile names are not the pinned roster"
    );
}
