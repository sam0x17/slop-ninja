//! R0 passes, and each mutation of it is refused.
//!
//! R0's files are private. Set `A_R0_MODEL_DIR` to a directory holding R0's
//! six served files; the tests print a notice and pass vacuously without it.
//! `A_EXPORT_PLAIN` and `A_EXPORT_SPAN` name graphs written by
//! `export_a_onnx.py` (plain and span variants) for the export checks.

mod common;

use std::path::PathBuf;

use common::*;
use serde_json::Value;
use slop_ninja_artifact_check::{Refusal, RoleFiles, canonical, check_roles, fs, onnx, spec};
use slop_ninja_protocol::{Digest, FileRole};

const R0_GRAPH_SHA256: &str = "08848d95261eea0e0791366298cede77033d92dcac88cfcb08f8367fd6ca4cb7";
const R0_GRAPH_PARAMETERS: u64 = 150_058_763;
const R0_HEAD_PARAMETERS: u64 = 244_069;

fn env_path(name: &str) -> Option<PathBuf> {
    let p = std::env::var_os(name).map(PathBuf::from);
    if p.is_none() {
        eprintln!("notice: {name} is unset; test passes without checking R0");
    }
    p
}

fn scan_file(path: &PathBuf) -> Result<onnx::GraphReport, Refusal> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).expect("graph");
    let len = f.metadata().unwrap().len();
    let mut s = onnx::GraphScan::new(len, spec::GRAPH_POLICY);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        s.update(&buf[..n])?;
    }
    s.finish()
}

#[test]
fn r0_passes_and_maps_to_its_deployed_package() {
    let Some(dir) = env_path("A_R0_MODEL_DIR") else {
        return;
    };
    let r = fs::check_dir(&dir).expect("R0 passes");
    assert_eq!(r.graph_class, "a-plain-f32band-v1");
    assert_eq!(
        std::fs::metadata(dir.join(spec::GRAPH_FILE)).unwrap().len(),
        spec::GRAPHS[0].bytes
    );
    assert_eq!(r.graph.structure_sha256, spec::PLAIN_STRUCTURE_SHA256);
    assert_eq!(r.graph.parameters, R0_GRAPH_PARAMETERS);
    assert_eq!(r.roles.heads_parameters, R0_HEAD_PARAMETERS);
    assert_eq!(r.parameters, R0_GRAPH_PARAMETERS + R0_HEAD_PARAMETERS);
    assert!(!r.roles.span_head);
    assert_eq!((r.graph.nodes, r.graph.initializers), (4712, 150));
    assert_eq!(r.graph.opsets, vec![(String::new(), 17)]);
    let header = fs::header_for_dir(&dir).unwrap();
    let graph = header.files.iter().find(|f| f.name == spec::GRAPH_FILE).unwrap();
    assert_eq!(hex::encode(graph.sha256.0), R0_GRAPH_SHA256);
    // Its files manifest v2 (the eval bucket's reference layout) builds and
    // parses back under its own package digest.
    let m = slop_ninja_artifact_check::files_manifest::FilesManifest::for_header(
        &header,
        slop_ninja_artifact_check::Version::V1,
        &r.profile_id,
        |_, sha| ("eval".into(), fs::reference_file_key(sha), None),
    )
    .unwrap();
    let bytes = m.to_bytes();
    let package = slop_ninja_artifact_check::files_manifest::FilesManifest::package_of(&bytes);
    assert!(slop_ninja_artifact_check::files_manifest::FilesManifest::parse(&bytes, &package, slop_ninja_artifact_check::Version::V1).is_ok());
    eprintln!("R0 content {} package v2 {package}", r.content_sha256);
}

fn slop_ninja_artifact_check_sha(b: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(b))
}

/// R0's JSON role files are in their canonical layouts: each is exactly the
/// canonical serialization of its parsed value (Python `repr` floats for the
/// heads and the export manifest, `ryu` floats for the contract and the
/// calibration).
#[test]
fn r0_json_files_are_in_their_canonical_layouts() {
    let Some(dir) = env_path("A_R0_MODEL_DIR") else {
        return;
    };
    for r in spec::ROLES.iter().filter(|r| r.layout.is_some()) {
        let bytes = std::fs::read(dir.join(r.name)).unwrap();
        let layout = r.layout.unwrap();
        canonical::check(&bytes, layout).unwrap_or_else(|e| panic!("{}: {e}", r.name));
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            canonical::encode(&v, layout) == bytes,
            "{} re-encodes",
            r.name
        );
        eprintln!("{}: {} bytes, {:?}", r.name, bytes.len(), layout);
    }
}

#[test]
fn the_pinned_tokenizer_has_the_profile_the_text_spec_records() {
    let Some(dir) = env_path("A_R0_MODEL_DIR") else {
        return;
    };
    let t: Value =
        serde_json::from_slice(&std::fs::read(dir.join("tokenizer.json")).unwrap()).unwrap();
    assert_eq!(t["normalizer"], serde_json::json!({"type": "NFC"}));
    assert!(t["truncation"].is_null() && t["padding"].is_null());
    assert_eq!(t["pre_tokenizer"]["type"], "ByteLevel");
    let single = &t["post_processor"]["single"];
    assert_eq!(single[0]["SpecialToken"]["id"], "[CLS]");
    assert_eq!(single[1]["Sequence"]["id"], "A");
    assert_eq!(single[2]["SpecialToken"]["id"], "[SEP]");
    assert_eq!(single.as_array().unwrap().len(), 3);
}

struct R0 {
    dir: PathBuf,
    digests: Vec<(String, (Digest, u64))>,
}

impl R0 {
    fn load(dir: PathBuf) -> Self {
        let digests = spec::ROLES
            .iter()
            .map(|r| (r.name.to_string(), Source::Path(dir.join(r.name)).digest()))
            .collect();
        Self { dir, digests }
    }

    fn files(&self) -> Vec<TestFile> {
        spec::ROLES
            .iter()
            .map(|r| TestFile {
                name: r.name.into(),
                role: r.file_role,
                source: Source::Path(self.dir.join(r.name)),
                digest: self
                    .digests
                    .iter()
                    .find(|(n, _)| n == r.name)
                    .map(|(_, d)| *d),
            })
            .collect()
    }

    fn json(&self, name: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.dir.join(name)).unwrap()).unwrap()
    }

    fn bytes(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.dir.join(name)).unwrap()
    }

    /// The files with the given JSON role files replaced, written in their
    /// canonical layouts, and the export manifest's digests of the heads and
    /// the calibration rebound to them.
    fn rebound(&self, replace: &[(&str, &Value)]) -> Vec<TestFile> {
        let mut files = self.files();
        let mut manifest = self.json("manifest.json");
        for (name, v) in replace {
            let b = enc(name, v);
            let key = match *name {
                "heads.json" => Some("heads"),
                "calibration.json" => Some("calibration"),
                _ => None,
            };
            if let Some(key) = key {
                manifest[key]["sha256"] = Value::from(slop_ninja_artifact_check_sha(&b));
            }
            let f = files.iter_mut().find(|f| f.name == *name).unwrap();
            f.source = Source::Bytes(b);
            f.digest = None;
        }
        let f = files
            .iter_mut()
            .find(|f| f.name == "manifest.json")
            .unwrap();
        f.source = Source::Bytes(enc("manifest.json", &manifest));
        f.digest = None;
        files
    }

    fn with(&self, name: &str, bytes: Vec<u8>) -> Vec<TestFile> {
        let mut files = self.files();
        let f = files.iter_mut().find(|f| f.name == name).unwrap();
        f.source = Source::Bytes(bytes);
        f.digest = None;
        files
    }
}

const CHUNK: usize = 999_983;

/// `v` in the canonical layout of role file `name`.
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
fn r0_mutations_are_refused() {
    let Some(dir) = env_path("A_R0_MODEL_DIR") else {
        return;
    };
    let r0 = R0::load(dir);
    assert_eq!(code(check_files(&r0.files(), CHUNK)), "accepted");

    // Extra files: notes, an external-data sidecar.
    for (name, role) in [
        ("notes.txt", FileRole::OwnerMetadata),
        ("a-fp32band.onnx.data", FileRole::Weights),
    ] {
        let mut files = r0.files();
        files.push(TestFile {
            name: name.into(),
            role,
            source: Source::Bytes(b"x".to_vec()),
            digest: None,
        });
        assert_eq!(
            code(check_files(&files, CHUNK)),
            "undeclared_file",
            "{name}"
        );
    }
    // A missing file and a wrong role.
    let mut files = r0.files();
    files.retain(|f| f.name != "calibration.json");
    assert_eq!(code(check_files(&files, CHUNK)), "missing_file");
    let mut files = r0.files();
    files
        .iter_mut()
        .find(|f| f.name == "heads.json")
        .unwrap()
        .role = FileRole::Graph;
    assert_eq!(code(check_files(&files, CHUNK)), "file_role");

    // The tokenizer one byte off.
    let mut tok = std::fs::read(r0.dir.join("tokenizer.json")).unwrap();
    let mid = tok.len() / 2;
    tok[mid] ^= 1;
    assert_eq!(
        code(check_files(&r0.with("tokenizer.json", tok), CHUNK)),
        "tokenizer_mismatch"
    );

    // Padding: the same values in another layout, trailing whitespace, a
    // respelled number.
    let mut cal = r0.bytes("calibration.json");
    cal.extend(std::iter::repeat_n(b' ', 1 << 20));
    assert_eq!(
        code(check_files(&r0.with("calibration.json", cal), CHUNK)),
        "json_layout"
    );
    let compact = serde_json::to_vec(&r0.json("contract.json")).unwrap();
    assert_eq!(
        code(check_files(&r0.with("contract.json", compact), CHUNK)),
        "json_layout"
    );
    let heads = String::from_utf8(r0.bytes("heads.json")).unwrap();
    // The same value with a trailing zero.
    let respelled = heads.replacen(
        "\"base_temperature\":1.8776105792643432,",
        "\"base_temperature\":1.87761057926434320,",
        1,
    );
    assert_ne!(respelled, heads);
    assert_eq!(
        code(check_files(
            &r0.with("heads.json", respelled.into_bytes()),
            CHUNK
        )),
        "json_layout"
    );
    // Declared lengths past the runtimes' bounds are refused from the
    // header, before a byte of the file is read.
    for (name, len) in [
        ("heads.json", spec::HEADS_MAX_BYTES + 1),
        ("calibration.json", spec::MAX_FILE_BYTES + 1),
    ] {
        let mut files = r0.files();
        let f = files.iter_mut().find(|f| f.name == name).unwrap();
        f.source = Source::Bytes(Vec::new());
        f.digest = Some((Digest([7; 32]), len));
        assert_eq!(code(check_files(&files, CHUNK)), "file_size", "{name}");
    }

    // Schema violations: a broken calibration knot, an unknown contract
    // field, an unknown heads field, a changed calibration policy.
    let mut cal = r0.json("calibration.json");
    cal["involvement"]["selected"]["knots"][0]
        .as_object_mut()
        .unwrap()
        .remove("rows");
    let files = r0.with("calibration.json", enc("calibration.json", &cal));
    assert_eq!(code(check_files(&files, CHUNK)), "schema");
    let mut cal = r0.json("calibration.json");
    cal["involvement"]["selected"]["knots"][3]["score"] = Value::from(2.0);
    let files = r0.with("calibration.json", enc("calibration.json", &cal));
    assert_eq!(code(check_files(&files, CHUNK)), "schema");
    let mut cal = r0.json("calibration.json");
    cal["involvement"]["report"]["policy"]["fallback"] = Value::from("any");
    let files = r0.with("calibration.json", enc("calibration.json", &cal));
    assert_eq!(code(check_files(&files, CHUNK)), "schema");
    let mut contract = r0.json("contract.json");
    contract["notes"] = Value::from("padding");
    let files = r0.with("contract.json", enc("contract.json", &contract));
    assert_eq!(code(check_files(&files, CHUNK)), "schema");
    let mut contract = r0.json("contract.json");
    contract["embedding_normalization"]["runs"][0]["shard"] = Value::from("0");
    let files = r0.with("contract.json", enc("contract.json", &contract));
    assert_eq!(code(check_files(&files, CHUNK)), "schema");
    let mut contract = r0.json("contract.json");
    contract["maximum_tokens_including_specials"] = Value::from(4096);
    let files = r0.with("contract.json", enc("contract.json", &contract));
    assert_eq!(code(check_files(&files, CHUNK)), "schema");
    let mut heads = r0.json("heads.json");
    heads["notes"] = Value::from("padding");
    let files = r0.with("heads.json", enc("heads.json", &heads));
    assert_eq!(code(check_files(&files, CHUNK)), "schema");
    let mut manifest = r0.json("manifest.json");
    manifest["fit_binding"]["arm_definition"]["batching"]["notes"] = Value::from("padding");
    let files = r0.with("manifest.json", enc("manifest.json", &manifest));
    assert_eq!(code(check_files(&files, CHUNK)), "schema");

    // A binding: heads bound to another contract.
    let mut heads = r0.json("heads.json");
    heads["contract_sha256"] = Value::from("0".repeat(64));
    let files = r0.rebound(&[("heads.json", &heads)]);
    assert_eq!(code(check_files(&files, CHUNK)), "binding");

    // The scorer's guard compares thresholds as JSON values: the integer 1
    // in the calibration is not the float 1.0 in the heads, so the scorer
    // refuses the artifact and admission does too. Both floats pass.
    let mut cal = r0.json("calibration.json");
    let mut heads = r0.json("heads.json");
    cal["roles"]["ai"]["threshold"] = Value::from(1);
    heads["thresholds"]["ai"] = Value::from(1.0);
    let files = r0.rebound(&[("heads.json", &heads), ("calibration.json", &cal)]);
    assert_eq!(code(check_files(&files, CHUNK)), "binding");
    cal["roles"]["ai"]["threshold"] = Value::from(1.0);
    let files = r0.rebound(&[("heads.json", &heads), ("calibration.json", &cal)]);
    assert_eq!(code(check_files(&files, CHUNK)), "accepted");

    // A plain-class check of R0's role files against the span class: the
    // heads carry no span head.
    let (h, c, k, m) = (
        r0.bytes("heads.json"),
        r0.bytes("contract.json"),
        r0.bytes("calibration.json"),
        r0.bytes("manifest.json"),
    );
    let hex = [slop_ninja_artifact_check_sha(&h), slop_ninja_artifact_check_sha(&c), slop_ninja_artifact_check_sha(&k)];
    let roles = RoleFiles {
        heads: &h,
        contract: &c,
        calibration: &k,
        manifest: &m,
        digests: slop_ninja_artifact_check::roles::RoleDigests::of(&hex),
    };
    assert!(check_roles(&roles, Some(&spec::GRAPHS[0])).is_ok());
    assert_eq!(
        check_roles(&roles, Some(&spec::GRAPHS[1]))
            .map(|_| ())
            .map_err(|e| e.code()),
        Err("graph_class")
    );

    // Graph mutations of R0's own length: an operator outside the allow-list
    // and a custom operator domain, each one field of the first Add node.
    let mut graph = r0.bytes(spec::GRAPH_FILE);
    let at = graph
        .windows(5)
        .position(|w| w == b"\x22\x03Add")
        .expect("an Add node");
    for (patch, want) in [
        (&b"\x22\x03Abs"[..], "graph_op_not_allowed"),
        (&b"\x3a\x03Add"[..], "graph_custom_domain"),
    ] {
        let saved: Vec<u8> = graph[at..at + 5].to_vec();
        graph[at..at + 5].copy_from_slice(patch);
        let files = r0.with(spec::GRAPH_FILE, std::mem::take(&mut graph));
        assert_eq!(code(check_files(&files, CHUNK)), want);
        let Source::Bytes(b) = files
            .into_iter()
            .find(|f| f.name == spec::GRAPH_FILE)
            .unwrap()
            .source
        else {
            unreachable!()
        };
        graph = b;
        graph[at..at + 5].copy_from_slice(&saved);
    }
    drop(graph);

    // Graphs of other lengths are refused from the header: a custom
    // operator domain, external data, a graph of allow-listed operators that
    // is not an allow-listed class, and more parameters than the bound (no
    // file of a class's length can hold them; the walk's own bound is tested
    // on synthetic graphs).
    let custom = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| {
            g.nodes.push(node(&Node {
                op: "Add",
                domain: Some("com.example"),
                inputs: &["y", "y"],
                outputs: &["z"],
                attributes: vec![],
            }))
        },
        &[("", 17)],
        &[],
    );
    assert_eq!(
        code(check_files(&r0.with(spec::GRAPH_FILE, custom), CHUNK)),
        "graph_size"
    );
    let mut t = Vec::new();
    uint(&mut t, 1, 4);
    uint(&mut t, 2, 1);
    bytes(&mut t, 8, b"w");
    uint(&mut t, 14, 1);
    let external = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| g.initializers = vec![t.clone()],
        &[("", 17)],
        &[],
    );
    assert_eq!(
        code(check_files(&r0.with(spec::GRAPH_FILE, external), CHUNK)),
        "graph_size"
    );
    let wrong = small_graph([1., 2., 3., 4.], 0.5);
    assert_eq!(
        code(check_files(&r0.with(spec::GRAPH_FILE, wrong), CHUNK)),
        "graph_size"
    );
    let mut files = r0.files();
    let (prefix, total) = huge_graph_prefix(spec::MAX_PARAMETERS + 1);
    let g = files
        .iter_mut()
        .find(|f| f.name == spec::GRAPH_FILE)
        .unwrap();
    g.source = Source::Bytes(prefix);
    g.digest = Some((Digest([7; 32]), total));
    assert_eq!(code(check_files(&files, CHUNK)), "graph_size");

    // The span-head graph: allow-listed, but the scorer does not serve span
    // heads yet, so admission refuses it from its length.
    if let Some(span) = std::env::var_os("A_EXPORT_SPAN").map(PathBuf::from) {
        let mut files = r0.files();
        let g = files
            .iter_mut()
            .find(|f| f.name == spec::GRAPH_FILE)
            .unwrap();
        g.source = Source::Path(span);
        g.digest = None;
        assert_eq!(code(check_files(&files, CHUNK)), "graph_class_not_served");
    }
}

/// A graph whose single initializer declares `n` FLOAT elements; only the
/// bytes up to its payload, with the declared total length.
fn huge_graph_prefix(n: u64) -> (Vec<u8>, u64) {
    let payload = n * 4;
    let mut t = Vec::new();
    uint(&mut t, 1, n);
    uint(&mut t, 2, 1);
    tag(&mut t, 9, 2);
    varint(&mut t, payload);
    let mut g = Vec::new();
    tag(&mut g, 5, 2);
    varint(&mut g, t.len() as u64 + payload);
    let mut m = Vec::new();
    uint(&mut m, 1, 8);
    tag(&mut m, 7, 2);
    varint(&mut m, (g.len() + t.len()) as u64 + payload);
    m.extend_from_slice(&g);
    m.extend_from_slice(&t);
    let total = m.len() as u64 + payload;
    (m, total)
}

#[test]
fn the_export_reproduces_the_allow_listed_structures() {
    if let Some(p) = env_path("A_EXPORT_PLAIN") {
        let r = scan_file(&p).expect("plain export walks");
        assert_eq!(r.structure_sha256, spec::PLAIN_STRUCTURE_SHA256);
        assert_eq!(r.parameters, R0_GRAPH_PARAMETERS);
        assert_eq!(std::fs::metadata(&p).unwrap().len(), spec::GRAPHS[0].bytes);
        let (d, _) = Source::Path(p).digest();
        eprintln!(
            "plain export: structure matches R0; file sha256 {} ({} R0's bytes)",
            hex::encode(d.0),
            if hex::encode(d.0) == R0_GRAPH_SHA256 {
                "equals"
            } else {
                "differs from"
            }
        );
    }
    if let Some(p) = env_path("A_EXPORT_SPAN") {
        let r = scan_file(&p).expect("span export walks");
        assert_eq!(r.structure_sha256, spec::SPAN_STRUCTURE_SHA256);
        assert_eq!(r.outputs, 7);
        assert_eq!(r.parameters, R0_GRAPH_PARAMETERS);
        assert_eq!(std::fs::metadata(&p).unwrap().len(), spec::GRAPHS[1].bytes);
    }
}
