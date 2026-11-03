//! Test helpers: a minimal protobuf writer for synthetic ONNX graphs, and an
//! artifact-object builder over in-memory or on-disk files.
#![allow(dead_code)]

use std::io::Read;
use std::path::PathBuf;

use sha2::{Digest as _, Sha256};
use slop_ninja_artifact_check::{ObjectCheck, ObjectReport, Refusal, spec};
use slop_ninja_protocol::{ArtifactFile, ArtifactHeader, Digest, Record};

pub fn varint(out: &mut Vec<u8>, mut v: u64) {
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

pub fn tag(out: &mut Vec<u8>, field: u64, wire: u8) {
    varint(out, (field << 3) | u64::from(wire));
}

pub fn uint(out: &mut Vec<u8>, field: u64, v: u64) {
    tag(out, field, 0);
    varint(out, v);
}

pub fn bytes(out: &mut Vec<u8>, field: u64, b: &[u8]) {
    tag(out, field, 2);
    varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

pub fn f32_field(out: &mut Vec<u8>, field: u64, v: f32) {
    tag(out, field, 5);
    out.extend_from_slice(&v.to_le_bytes());
}

/// A FLOAT tensor with `raw_data`.
pub fn float_tensor(name: &str, dims: &[u64], values: &[f32]) -> Vec<u8> {
    let mut t = Vec::new();
    for &d in dims {
        uint(&mut t, 1, d);
    }
    uint(&mut t, 2, 1);
    bytes(&mut t, 8, name.as_bytes());
    let raw: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    bytes(&mut t, 9, &raw);
    t
}

/// An INT64 tensor with `raw_data`.
pub fn int_tensor(name: &str, dims: &[u64], values: &[i64]) -> Vec<u8> {
    let mut t = Vec::new();
    for &d in dims {
        uint(&mut t, 1, d);
    }
    uint(&mut t, 2, 7);
    bytes(&mut t, 8, name.as_bytes());
    let raw: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    bytes(&mut t, 9, &raw);
    t
}

pub struct Node<'a> {
    pub op: &'a str,
    pub domain: Option<&'a str>,
    pub inputs: &'a [&'a str],
    pub outputs: &'a [&'a str],
    /// Encoded attributes.
    pub attributes: Vec<Vec<u8>>,
}

pub fn node(n: &Node<'_>) -> Vec<u8> {
    let mut v = Vec::new();
    for i in n.inputs {
        bytes(&mut v, 1, i.as_bytes());
    }
    for o in n.outputs {
        bytes(&mut v, 2, o.as_bytes());
    }
    bytes(&mut v, 3, format!("/{}", n.op).as_bytes());
    bytes(&mut v, 4, n.op.as_bytes());
    for a in &n.attributes {
        bytes(&mut v, 5, a);
    }
    if let Some(d) = n.domain {
        bytes(&mut v, 7, d.as_bytes());
    }
    v
}

/// A `value` attribute holding a tensor (as on a Constant node).
pub fn tensor_attribute(name: &str, tensor: &[u8]) -> Vec<u8> {
    let mut a = Vec::new();
    bytes(&mut a, 1, name.as_bytes());
    bytes(&mut a, 5, tensor);
    uint(&mut a, 20, 4);
    a
}

pub fn int_attribute(name: &str, v: u64) -> Vec<u8> {
    let mut a = Vec::new();
    bytes(&mut a, 1, name.as_bytes());
    uint(&mut a, 3, v);
    uint(&mut a, 20, 2);
    a
}

/// A FLOAT value info with a symbolic batch dimension and fixed `dims`.
pub fn value_info(name: &str, elem: u64, dims: &[u64]) -> Vec<u8> {
    let mut shape = Vec::new();
    let mut d = Vec::new();
    bytes(&mut d, 2, b"batch");
    bytes(&mut shape, 1, &d);
    for &x in dims {
        let mut d = Vec::new();
        uint(&mut d, 1, x);
        bytes(&mut shape, 1, &d);
    }
    let mut tt = Vec::new();
    uint(&mut tt, 1, elem);
    bytes(&mut tt, 2, &shape);
    let mut ty = Vec::new();
    bytes(&mut ty, 1, &tt);
    let mut vi = Vec::new();
    bytes(&mut vi, 1, name.as_bytes());
    bytes(&mut vi, 2, &ty);
    vi
}

pub struct Graph {
    pub nodes: Vec<Vec<u8>>,
    pub initializers: Vec<Vec<u8>>,
    pub inputs: Vec<Vec<u8>>,
    pub outputs: Vec<Vec<u8>>,
    /// Extra encoded graph fields appended at the end.
    pub extra: Vec<u8>,
}

pub fn graph(g: &Graph) -> Vec<u8> {
    let mut v = Vec::new();
    for n in &g.nodes {
        bytes(&mut v, 1, n);
    }
    bytes(&mut v, 2, b"main_graph");
    for t in &g.initializers {
        bytes(&mut v, 5, t);
    }
    for i in &g.inputs {
        bytes(&mut v, 11, i);
    }
    for o in &g.outputs {
        bytes(&mut v, 12, o);
    }
    v.extend_from_slice(&g.extra);
    v
}

/// A model with opset `("" or domain, version)` pairs and `extra` model fields.
pub fn model(graph: &[u8], opsets: &[(&str, u64)], extra: &[u8]) -> Vec<u8> {
    let mut m = Vec::new();
    uint(&mut m, 1, 8);
    bytes(&mut m, 2, b"pytorch");
    bytes(&mut m, 3, b"2.9.1");
    bytes(&mut m, 7, graph);
    for (d, v) in opsets {
        let mut o = Vec::new();
        if !d.is_empty() {
            bytes(&mut o, 1, d.as_bytes());
        }
        uint(&mut o, 2, *v);
        bytes(&mut m, 8, &o);
    }
    m.extend_from_slice(extra);
    m
}

/// `y = x * w + b` with FLOAT weights: a small graph of allow-listed operators.
pub fn small_graph(w: [f32; 4], b: f32) -> Vec<u8> {
    small_graph_with(w, b, |_| {}, &[("", 17)], &[])
}

pub fn small_graph_with(
    w: [f32; 4],
    b: f32,
    edit: impl FnOnce(&mut Graph),
    opsets: &[(&str, u64)],
    model_extra: &[u8],
) -> Vec<u8> {
    let mut g = Graph {
        nodes: vec![
            node(&Node {
                op: "Constant",
                domain: None,
                inputs: &[],
                outputs: &["b"],
                attributes: vec![tensor_attribute("value", &float_tensor("", &[1], &[b]))],
            }),
            node(&Node {
                op: "MatMul",
                domain: None,
                inputs: &["x", "w"],
                outputs: &["xw"],
                attributes: vec![],
            }),
            node(&Node {
                op: "Add",
                domain: None,
                inputs: &["xw", "b"],
                outputs: &["y"],
                attributes: vec![],
            }),
        ],
        initializers: vec![float_tensor("w", &[2, 2], &w)],
        inputs: vec![value_info("x", 1, &[2])],
        outputs: vec![value_info("y", 1, &[2])],
        extra: Vec::new(),
    };
    edit(&mut g);
    model(&graph(&g), opsets, model_extra)
}

/// A file of an artifact under test.
#[derive(Clone)]
pub enum Source {
    Bytes(Vec<u8>),
    Path(PathBuf),
}

impl Source {
    pub fn digest(&self) -> (Digest, u64) {
        match self {
            Source::Bytes(b) => (Digest(Sha256::digest(b).into()), b.len() as u64),
            Source::Path(p) => {
                let mut f = std::fs::File::open(p).expect("open");
                let mut h = Sha256::new();
                let mut buf = vec![0u8; 1 << 20];
                let mut n = 0u64;
                loop {
                    let k = f.read(&mut buf).expect("read");
                    if k == 0 {
                        break;
                    }
                    h.update(&buf[..k]);
                    n += k as u64;
                }
                (Digest(h.finalize().into()), n)
            }
        }
    }
}

/// One file: name, role and source, with an optional precomputed digest.
pub struct TestFile {
    pub name: String,
    pub role: slop_ninja_protocol::FileRole,
    pub source: Source,
    pub digest: Option<(Digest, u64)>,
}

/// Builds the object for `files` and streams it through [`ObjectCheck`] in
/// chunks of `chunk` bytes, stopping at the first refusal (spec v1).
pub fn check_files(files: &[TestFile], chunk: usize) -> Result<ObjectReport, Refusal> {
    check_files_for(files, chunk, slop_ninja_artifact_check::Version::V1)
}

/// [`check_files`] under `version`.
pub fn check_files_for(
    files: &[TestFile],
    chunk: usize,
    version: slop_ninja_artifact_check::Version,
) -> Result<ObjectReport, Refusal> {
    let mut sorted: Vec<&TestFile> = files.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let header = ArtifactHeader {
        contract: spec::CONTRACT_ID.into(),
        files: sorted
            .iter()
            .map(|f| {
                let (sha256, len) = f.digest.unwrap_or_else(|| f.source.digest());
                ArtifactFile {
                    name: f.name.clone(),
                    role: f.role,
                    sha256,
                    len,
                }
            })
            .collect(),
    };
    let hb = header.to_bytes().expect("header encodes");
    let mut check = ObjectCheck::for_spec(version);
    let mut feed = |b: &[u8]| -> Result<(), Refusal> {
        for c in b.chunks(chunk.max(1)) {
            check.update(c)?;
        }
        Ok(())
    };
    feed(&(hb.len() as u32).to_le_bytes())?;
    feed(&hb)?;
    for f in sorted {
        match &f.source {
            Source::Bytes(b) => feed(b)?,
            Source::Path(p) => {
                let mut file = std::fs::File::open(p).expect("open");
                let mut buf = vec![0u8; chunk.max(1)];
                loop {
                    let k = file.read(&mut buf).expect("read");
                    if k == 0 {
                        break;
                    }
                    feed(&buf[..k])?;
                }
            }
        }
    }
    check.finish()
}
