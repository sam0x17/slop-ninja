//! Artifact files manifest: how an admitted *A* artifact is stored and moved by digest.
//!
//! An artifact is the set of files the server loads (the f32 ONNX graph, heads,
//! contract, calibration, export manifest and tokenizer). Each file is stored as
//! content-addressed parts of at most [`MAX_PART_BYTES`], so no single object or
//! transfer has to carry the whole 601 MB graph. The manifest lists every file
//! with its SHA-256, size and parts. Its exact bytes are the artifact package:
//! their SHA-256 is the package digest an artifact record commits to
//! (`package_sha256`), and the scorer refuses a manifest whose bytes hash to
//! anything else.
use crate::profile::sha256_hex;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

pub const SCHEMA: &str = "slop_ninja_a_artifact_files_v1";
/// Largest part, and so the largest object a transfer carries.
pub const MAX_PART_BYTES: u64 = 64 << 20;
/// Largest manifest accepted.
pub const MAX_MANIFEST_BYTES: usize = 1 << 20;
const MAX_PARTS_PER_FILE: usize = 64;
/// Files every artifact of this contract must contain.
pub const REQUIRED: [&str; 5] = ["heads.json", "contract.json", "calibration.json", "manifest.json", "tokenizer.json"];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactFiles {
    pub schema: String,
    /// Served profile the files were checked against when packed, if any.
    pub profile_id: String,
    pub contract: String,
    /// Name of the encoder graph among `files`.
    pub graph: String,
    pub files: Vec<ArtifactFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactFile {
    pub name: String,
    pub role: String,
    pub bytes: u64,
    pub sha256: String,
    pub parts: Vec<Part>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Part {
    pub sha256: String,
    pub bytes: u64,
}

pub fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl ArtifactFiles {
    /// Parse manifest bytes and check that they hash to `package`.
    pub fn parse(bytes: &[u8], package: &str) -> Result<Self> {
        ensure!(bytes.len() <= MAX_MANIFEST_BYTES, "artifact manifest too large");
        ensure!(sha256_hex(bytes) == package, "artifact manifest bytes do not hash to the package digest");
        let m: Self = serde_json::from_slice(bytes).context("artifact manifest JSON")?;
        m.validate()?;
        Ok(m)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == SCHEMA, "artifact manifest schema {}", self.schema);
        ensure!(!self.contract.is_empty() && self.contract.len() <= 128, "contract id");
        let mut names = BTreeSet::new();
        for f in &self.files {
            ensure!(
                !f.name.is_empty() && f.name.len() <= 128 && !f.name.contains('/') && f.name != "." && f.name != "..",
                "artifact file name must be a bare file name"
            );
            ensure!(names.insert(f.name.as_str()), "duplicate artifact file {}", f.name);
            ensure!(is_digest(&f.sha256) && f.bytes > 0, "{}: digest or size", f.name);
            ensure!(!f.parts.is_empty() && f.parts.len() <= MAX_PARTS_PER_FILE, "{}: part count", f.name);
            let mut total = 0u64;
            for p in &f.parts {
                ensure!(is_digest(&p.sha256) && p.bytes > 0 && p.bytes <= MAX_PART_BYTES, "{}: part", f.name);
                total = total.checked_add(p.bytes).context("part sizes overflow")?;
            }
            ensure!(total == f.bytes, "{}: parts sum to {total}, file is {}", f.name, f.bytes);
        }
        ensure!(names.contains(self.graph.as_str()), "graph file missing");
        for r in REQUIRED {
            ensure!(names.contains(r), "artifact lacks {r}");
        }
        Ok(())
    }

    /// Every distinct part digest, in manifest order.
    pub fn parts(&self) -> Vec<&Part> {
        let mut seen = BTreeSet::new();
        self.files.iter().flat_map(|f| f.parts.iter()).filter(|p| seen.insert(p.sha256.as_str())).collect()
    }
}

/// One file to split: its name in the model directory and its scorer role.
pub struct PackFile<'a> {
    pub name: &'a str,
    pub role: &'a str,
}

/// Split `files` of `model_dir` into parts under `out/blobs/` and write
/// `out/artifact-files.json`, with no admission check: the spec's packer is
/// `slop_ninja_artifact_check parts` (or `slop_ninja_a_served_profile pack`),
/// which admits the directory first. This one also lays out artifacts the spec
/// refuses, for the adversarial set and tests. Returns the manifest bytes.
pub fn pack(
    model_dir: &Path,
    out: &Path,
    files_in: &[PackFile<'_>],
    profile_id: &str,
    graph: &str,
    part_bytes: u64,
) -> Result<Vec<u8>> {
    ensure!(part_bytes > 0 && part_bytes <= MAX_PART_BYTES, "part size");
    let blobs = out.join("blobs");
    fs::create_dir_all(&blobs)?;
    let mut files = Vec::new();
    for f in files_in {
        let mut src = fs::File::open(model_dir.join(f.name))?;
        let mut parts = Vec::new();
        let mut whole = Sha256::new();
        loop {
            let mut buf = Vec::with_capacity(part_bytes as usize);
            (&mut src).take(part_bytes).read_to_end(&mut buf)?;
            if buf.is_empty() {
                break;
            }
            whole.update(&buf);
            let sha = sha256_hex(&buf);
            let path = blobs.join(&sha);
            if !path.exists() {
                let tmp = blobs.join(format!("{sha}.tmp"));
                let mut w = fs::File::create(&tmp)?;
                w.write_all(&buf)?;
                w.sync_all()?;
                fs::rename(&tmp, &path)?;
            }
            parts.push(Part { sha256: sha, bytes: buf.len() as u64 });
        }
        let bytes = parts.iter().map(|p| p.bytes).sum();
        files.push(ArtifactFile {
            name: f.name.into(),
            role: f.role.into(),
            bytes,
            sha256: hex::encode(whole.finalize()),
            parts,
        });
    }
    let manifest = ArtifactFiles {
        schema: SCHEMA.into(),
        profile_id: profile_id.into(),
        contract: "slop_ninja_full_a_contract_v2".into(),
        graph: graph.into(),
        files,
    };
    manifest.validate()?;
    let mut bytes = serde_json::to_vec_pretty(&manifest)?;
    bytes.push(b'\n');
    fs::write(out.join("artifact-files.json"), &bytes)?;
    Ok(bytes)
}

/// Concatenate a file's parts from `blobs` into `dest`, checking every part and the
/// whole file. Missing parts are an error naming the first missing digest.
pub fn assemble(file: &ArtifactFile, blobs: &Path, dest: &Path) -> Result<()> {
    let tmp = dest.with_extension("assembling");
    let mut w = fs::File::create(&tmp)?;
    let mut whole = Sha256::new();
    let mut total = 0u64;
    for p in &file.parts {
        let bytes = fs::read(blobs.join(&p.sha256)).with_context(|| format!("part {} missing", p.sha256))?;
        ensure!(bytes.len() as u64 == p.bytes && sha256_hex(&bytes) == p.sha256, "part {} differs", p.sha256);
        whole.update(&bytes);
        total += p.bytes;
        w.write_all(&bytes)?;
    }
    w.sync_all()?;
    drop(w);
    ensure!(total == file.bytes && hex::encode(whole.finalize()) == file.sha256, "{} differs after assembly", file.name);
    fs::rename(&tmp, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("a-artifact-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    const FILES: [PackFile<'static>; 6] = [
        PackFile { name: "a-fp32band.onnx", role: "encoder_graph" },
        PackFile { name: "heads.json", role: "heads" },
        PackFile { name: "contract.json", role: "contract" },
        PackFile { name: "calibration.json", role: "calibration" },
        PackFile { name: "manifest.json", role: "export_manifest" },
        PackFile { name: "tokenizer.json", role: "tokenizer" },
    ];

    fn synthetic(dir: &Path) {
        for f in &FILES {
            fs::write(dir.join(f.name), format!("synthetic {} {}", f.name, "x".repeat(40))).unwrap();
        }
    }

    fn pack_synthetic(model: &Path, out: &Path, part_bytes: u64) -> Result<Vec<u8>> {
        pack(model, out, &FILES, "synthetic", "a-fp32band.onnx", part_bytes)
    }

    #[test]
    fn pack_then_assemble_round_trips_and_binds_the_package_digest() {
        let model = scratch("model");
        let out = scratch("out");
        synthetic(&model);
        let bytes = pack_synthetic(&model, &out, 16).unwrap();
        let package = sha256_hex(&bytes);
        let m = ArtifactFiles::parse(&bytes, &package).unwrap();
        assert!(m.files.iter().all(|f| f.parts.len() >= 3));
        let dest = scratch("dest");
        for f in &m.files {
            assemble(f, &out.join("blobs"), &dest.join(&f.name)).unwrap();
            assert_eq!(fs::read(dest.join(&f.name)).unwrap(), fs::read(model.join(&f.name)).unwrap());
        }
        // Another digest, a changed byte and a missing part are refused.
        assert!(ArtifactFiles::parse(&bytes, &"0".repeat(64)).is_err());
        let mut edited = bytes.clone();
        let last = edited.len() - 2;
        edited[last] = b' ';
        assert!(ArtifactFiles::parse(&edited, &package).is_err());
        let first = &m.files[0];
        fs::remove_file(out.join("blobs").join(&first.parts[0].sha256)).unwrap();
        assert!(assemble(first, &out.join("blobs"), &dest.join("again")).is_err());
    }

    #[test]
    fn manifest_validation_refuses_inconsistent_parts_and_names() {
        let model = scratch("model2");
        let out = scratch("out2");
        synthetic(&model);
        let bytes = pack_synthetic(&model, &out, MAX_PART_BYTES).unwrap();
        let m: ArtifactFiles = serde_json::from_slice(&bytes).unwrap();
        let mut bad = m.clone();
        bad.files[0].parts[0].bytes += 1;
        assert!(bad.validate().is_err());
        let mut bad = m.clone();
        bad.files[1].name = "../heads.json".into();
        assert!(bad.validate().is_err());
        let mut bad = m.clone();
        bad.files.retain(|f| f.name != "calibration.json");
        assert!(bad.validate().is_err());
        let mut bad = m;
        bad.files[0].parts[0].bytes = MAX_PART_BYTES + 1;
        assert!(bad.validate().is_err());
    }
}
