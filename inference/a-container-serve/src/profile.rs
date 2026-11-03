//! Served profile: the tracked identity of everything the Container serves.
//!
//! The release package's `SHA256SUMS` covers the release model files but not the
//! f32 ONNX graph, which is exported from the release weights outside the package.
//! The served profile binds that graph's SHA-256 and size next to the release
//! files it was derived from, the release `SHA256SUMS` digest, the heads runtime
//! sources and the runtime images. The server checks every served file against it
//! before reporting ready.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::Path;

pub const SCHEMA: &str = "slop_ninja_a_served_profile_v1";
/// The profile compiled into the server.
pub const RELEASE_CANDIDATE_JSON: &str = include_str!("../profiles/a-baseline-rc-s17-f32band.json");

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServedProfile {
    pub schema: String,
    pub profile_id: String,
    pub status: String,
    pub release: Release,
    pub files: Vec<ServedFile>,
    pub graph: Graph,
    pub heads_runtime_sha256: BTreeMap<String, String>,
    pub heads_base_temperature: f64,
    pub runtime: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub schema: String,
    pub arm: String,
    pub selected_arm: String,
    pub seed: u64,
    pub release_json_sha256: String,
    pub sha256sums_sha256: String,
    pub replay_receipt_sha256: String,
    pub encoder_safetensors_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServedFile {
    pub name: String,
    pub role: String,
    pub bytes: u64,
    pub sha256: String,
    /// Path inside the release package, or null for a file derived outside it.
    pub release_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    pub file: String,
    pub format: String,
    pub opset: u32,
    pub precision: String,
    pub attention: String,
    pub pad_block: usize,
    pub max_tokens_including_specials: usize,
    pub inputs: Vec<String>,
    pub outputs_used: Vec<String>,
    pub derived_from: Vec<String>,
    pub export: Value,
}

fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Streaming SHA-256 and byte count of a file.
pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut f = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        h.update(&buf[..n]);
    }
    Ok((hex::encode(h.finalize()), total))
}

impl ServedProfile {
    pub fn parse(json: &str) -> Result<Self> {
        let p: Self = serde_json::from_str(json).context("served profile JSON")?;
        p.validate()?;
        Ok(p)
    }

    pub fn release_candidate() -> Result<Self> {
        Self::parse(RELEASE_CANDIDATE_JSON)
    }

    fn validate(&self) -> Result<()> {
        ensure!(self.schema == SCHEMA, "served profile schema {}", self.schema);
        ensure!(!self.profile_id.is_empty(), "empty profile id");
        let r = &self.release;
        for d in [
            &r.release_json_sha256,
            &r.sha256sums_sha256,
            &r.replay_receipt_sha256,
            &r.encoder_safetensors_sha256,
        ] {
            ensure!(is_digest(d), "release digest is not lowercase SHA-256 hex");
        }
        let mut names = BTreeSet::new();
        for f in &self.files {
            ensure!(
                !f.name.is_empty() && !f.name.contains('/') && f.name != "." && f.name != "..",
                "served file name must be a bare file name"
            );
            ensure!(names.insert(f.name.as_str()), "duplicate served file {}", f.name);
            ensure!(is_digest(&f.sha256) && f.bytes > 0, "{}: bad digest or size", f.name);
            if let Some(p) = &f.release_path {
                ensure!(!p.starts_with('/') && !p.split('/').any(|c| c == ".."), "{}: bad release path", f.name);
            }
        }
        let g = self.graph_file()?;
        ensure!(g.release_path.is_none(), "the graph is derived outside the release package");
        ensure!(
            self.files.iter().filter(|f| f.name != g.name).all(|f| f.release_path.is_some()),
            "every non-graph served file must come from the release package"
        );
        ensure!(self.graph.format == "onnx" && self.graph.precision == "f32", "only the f32 ONNX profile is served");
        ensure!(self.graph.outputs_used == ["h"], "only the graph's h output feeds the heads");
        let want: BTreeSet<&str> = ["full_a.rs", "full_a_spans.rs", "involvement_calibration.rs", "shared.rs"].into();
        let got: BTreeSet<&str> = self.heads_runtime_sha256.keys().map(String::as_str).collect();
        ensure!(got == want, "heads runtime sources must be exactly {want:?}");
        ensure!(self.heads_runtime_sha256.values().all(|d| is_digest(d)), "heads runtime digest format");
        ensure!(self.heads_base_temperature.is_finite() && self.heads_base_temperature > 0., "base temperature");
        Ok(())
    }

    pub fn graph_file(&self) -> Result<&ServedFile> {
        self.file(&self.graph.file)
    }

    pub fn file(&self, name: &str) -> Result<&ServedFile> {
        self.files.iter().find(|f| f.name == name).with_context(|| format!("{name} is not in the served profile"))
    }

    /// The non-graph files, small enough to hash before loading anything.
    pub fn support_files(&self) -> impl Iterator<Item = &ServedFile> {
        self.files.iter().filter(move |f| f.name != self.graph.file)
    }

    /// Refuse compiled heads runtime sources that differ from the profile.
    pub fn check_heads_runtime(&self, sources: &[(&str, &[u8])]) -> Result<()> {
        ensure!(sources.len() == self.heads_runtime_sha256.len(), "heads runtime source count");
        for (name, bytes) in sources {
            let want = self.heads_runtime_sha256.get(*name).with_context(|| format!("{name} not pinned"))?;
            ensure!(&sha256_hex(bytes) == want, "{name} differs from the release heads runtime");
        }
        Ok(())
    }

    /// Hash one served file in `dir` and compare digest and size.
    pub fn check_file(&self, dir: &Path, name: &str) -> Result<()> {
        let f = self.file(name)?;
        let (sha, bytes) = sha256_file(&dir.join(&f.name))?;
        ensure!(bytes == f.bytes, "{}: {} bytes, profile records {}", f.name, bytes, f.bytes);
        ensure!(sha == f.sha256, "{}: digest differs from the served profile", f.name);
        Ok(())
    }

    /// Hash every served file in `dir`.
    pub fn check_dir(&self, dir: &Path) -> Result<()> {
        for f in &self.files {
            self.check_file(dir, &f.name)?;
        }
        Ok(())
    }

    /// Check a release package against the profile without rehashing its large files:
    /// the package's `SHA256SUMS` must have the recorded digest, and its lines must
    /// give every served release file, `RELEASE.json` and the replay receipt the
    /// recorded digests. The package's own files are checked with `sha256sum -c`.
    pub fn check_release_sums(&self, sums: &[u8]) -> Result<()> {
        ensure!(sha256_hex(sums) == self.release.sha256sums_sha256, "release SHA256SUMS digest differs");
        let lines = parse_sums(sums)?;
        let mut want: Vec<(&str, &str)> = self
            .files
            .iter()
            .filter_map(|f| Some((f.release_path.as_deref()?, f.sha256.as_str())))
            .collect();
        want.push(("RELEASE.json", &self.release.release_json_sha256));
        want.push(("records/replay-receipt.json", &self.release.replay_receipt_sha256));
        want.push(("model/encoder.safetensors", &self.release.encoder_safetensors_sha256));
        for (path, sha) in want {
            let got = lines.get(path).with_context(|| format!("{path} is missing from release SHA256SUMS"))?;
            ensure!(got == sha, "{path}: release SHA256SUMS digest differs from the served profile");
        }
        let graph = self.graph_file()?;
        ensure!(
            !lines.values().any(|d| d == &graph.sha256),
            "the graph unexpectedly appears in release SHA256SUMS; update the profile"
        );
        Ok(())
    }

    /// `sha256sum -c` lines for the served files, relative to their directory.
    pub fn sums(&self) -> String {
        self.files.iter().map(|f| format!("{}  {}\n", f.sha256, f.name)).collect()
    }
}

/// Parse `sha256sum` output (`<hex>  <path>`, optional `./` prefix, text or binary mode).
pub fn parse_sums(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    let text = std::str::from_utf8(bytes).context("SHA256SUMS is not UTF-8")?;
    let mut out = BTreeMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let Some((d, p)) = line.split_once(' ') else { bail!("malformed SHA256SUMS line") };
        let p = p.strip_prefix(' ').or_else(|| p.strip_prefix('*')).unwrap_or(p);
        let p = p.strip_prefix("./").unwrap_or(p);
        ensure!(is_digest(d) && !p.is_empty(), "malformed SHA256SUMS line");
        ensure!(out.insert(p.to_string(), d.to_string()).is_none(), "duplicate SHA256SUMS path {p}");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("a-served-profile-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A synthetic profile over small files, with the release candidate's structure.
    fn synthetic(dir: &Path) -> ServedProfile {
        let mut p = ServedProfile::release_candidate().unwrap();
        for f in &mut p.files {
            let body = format!("synthetic {}", f.name);
            fs::write(dir.join(&f.name), &body).unwrap();
            f.sha256 = sha256_hex(body.as_bytes());
            f.bytes = body.len() as u64;
        }
        p.validate().unwrap();
        p
    }

    #[test]
    fn tracked_profile_parses_and_binds_the_graph_outside_the_release() {
        let p = ServedProfile::release_candidate().unwrap();
        let g = p.graph_file().unwrap();
        assert_eq!(g.name, "a-fp32band.onnx");
        assert_eq!(g.sha256, "08848d95261eea0e0791366298cede77033d92dcac88cfcb08f8367fd6ca4cb7");
        assert!(g.release_path.is_none());
        assert_eq!(p.support_files().count(), 5);
        assert!(p.sums().lines().any(|l| l.ends_with("  a-fp32band.onnx")));
    }

    #[test]
    fn compiled_heads_runtime_matches_the_release_replay_bindings() {
        let p = ServedProfile::release_candidate().unwrap();
        p.check_heads_runtime(&crate::HEADS_RUNTIME_SOURCES).unwrap();
        let mut changed = crate::HEADS_RUNTIME_SOURCES;
        for i in 0..changed.len() {
            changed[i].1 = b"changed";
            assert!(p.check_heads_runtime(&changed).is_err(), "{}", changed[i].0);
            changed[i].1 = crate::HEADS_RUNTIME_SOURCES[i].1;
        }
        p.check_heads_runtime(&changed).unwrap();
        assert!(p.check_heads_runtime(&changed[..3]).is_err());
    }

    #[test]
    fn served_files_are_checked_by_digest_and_size() {
        let d = scratch("dir");
        let p = synthetic(&d);
        p.check_dir(&d).unwrap();
        fs::write(d.join("a-fp32band.onnx"), b"synthetic a-fp32band.onnY").unwrap();
        let e = p.check_dir(&d).unwrap_err().to_string();
        assert!(e.contains("a-fp32band.onnx") && e.contains("digest"), "{e}");
        fs::write(d.join("heads.json"), b"short").unwrap();
        assert!(p.check_file(&d, "heads.json").unwrap_err().to_string().contains("bytes"));
        fs::remove_file(d.join("tokenizer.json")).unwrap();
        assert!(p.check_file(&d, "tokenizer.json").is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn release_sums_must_bind_every_release_file_and_exclude_the_graph() {
        let d = scratch("sums");
        let mut p = synthetic(&d);
        let mut lines: Vec<String> = p
            .files
            .iter()
            .filter_map(|f| Some(format!("{}  ./{}", f.sha256, f.release_path.as_ref()?)))
            .collect();
        lines.push(format!("{}  ./RELEASE.json", p.release.release_json_sha256));
        lines.push(format!("{}  ./records/replay-receipt.json", p.release.replay_receipt_sha256));
        lines.push(format!("{}  ./model/encoder.safetensors", p.release.encoder_safetensors_sha256));
        let sums = lines.join("\n") + "\n";
        p.release.sha256sums_sha256 = sha256_hex(sums.as_bytes());
        p.check_release_sums(sums.as_bytes()).unwrap();

        // A changed SHA256SUMS file is refused by its own digest.
        let mut other = sums.clone();
        other.push('\n');
        assert!(p.check_release_sums(other.as_bytes()).is_err());

        // A release file whose sum line differs is refused.
        let heads = p.file("heads.json").unwrap().sha256.clone();
        let bad = sums.replace(&heads, &"0".repeat(64));
        p.release.sha256sums_sha256 = sha256_hex(bad.as_bytes());
        assert!(p.check_release_sums(bad.as_bytes()).unwrap_err().to_string().contains("model/heads.json"));

        // The graph must stay outside the package's sums, as recorded.
        let graph = p.graph_file().unwrap().sha256.clone();
        let with_graph = format!("{sums}{graph}  ./model/a-fp32band.onnx\n");
        p.release.sha256sums_sha256 = sha256_hex(with_graph.as_bytes());
        assert!(p.check_release_sums(with_graph.as_bytes()).is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn malformed_profiles_are_refused() {
        let base: Value = serde_json::from_str(RELEASE_CANDIDATE_JSON).unwrap();
        let refuse = |edit: &dyn Fn(&mut Value)| {
            let mut v = base.clone();
            edit(&mut v);
            assert!(ServedProfile::parse(&v.to_string()).is_err(), "{v}");
        };
        refuse(&|v| v["schema"] = "other".into());
        refuse(&|v| v["files"][0]["release_path"] = "model/a-fp32band.onnx".into());
        refuse(&|v| v["files"][1]["release_path"] = Value::Null);
        refuse(&|v| v["files"][1]["name"] = "../heads.json".into());
        refuse(&|v| v["files"][2]["name"] = "heads.json".into());
        refuse(&|v| v["files"][0]["sha256"] = "08848D95".into());
        refuse(&|v| v["graph"]["outputs_used"] = serde_json::json!(["h", "origin_logits"]));
        refuse(&|v| {
            v["heads_runtime_sha256"].as_object_mut().unwrap().remove("shared.rs");
        });
        refuse(&|v| {
            v["heads_runtime_sha256"].as_object_mut().unwrap().remove("full_a_spans.rs");
        });
        refuse(&|v| v["unexpected"] = true.into());
    }

    #[test]
    fn sums_parser_accepts_sha256sum_output_only() {
        let d = "a".repeat(64);
        let m = parse_sums(format!("{d}  ./x/y.json\n{d} *z.bin\n").as_bytes()).unwrap();
        assert_eq!(m.len(), 2);
        assert!(m.contains_key("x/y.json") && m.contains_key("z.bin"));
        assert!(parse_sums(b"nothex  a\n").is_err());
        assert!(parse_sums(format!("{d}  a\n{d}  ./a\n").as_bytes()).is_err());
    }
}
