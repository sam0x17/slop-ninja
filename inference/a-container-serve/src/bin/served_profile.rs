//! Check model directories and release packages against the tracked served profile.
//!
//! slop_ninja_a_served_profile show
//! slop_ninja_a_served_profile sums
//! slop_ninja_a_served_profile verify [--model-dir DIR] [--release-dir DIR]
//! slop_ninja_a_served_profile pack [--spec v2|v3] --model-dir DIR --out DIR
//!
//! `sums` prints `sha256sum -c` lines for the served files. `verify --model-dir` hashes
//! every served file in DIR, including the graph. `verify --release-dir` checks the
//! release package's `SHA256SUMS` digest and lines against the profile; run
//! `sha256sum -c SHA256SUMS` in the package to check its own files.
//!
//! `pack` packs an artifact for a cohort: it checks the model directory with the
//! artifact spec `--spec` names (`v2`, `v3` or a full spec id; the spec the cohort
//! names, which the scorer admits the cohort's loads by), else with spec v2, the spec
//! a load that names none is admitted by (`engine::UNNAMED_LOAD_SPEC`). It then
//! writes the spec's parts layout (64 MiB parts under `OUT/blobs/` and the artifact
//! files manifest `OUT/artifact-files.json`) and prints the package digest (the
//! manifest's SHA-256). The manifest's `profile_id` is that spec's
//! (`<spec id>/<graph class id>`), so the digest depends on the spec, except for the
//! served profile's own files (R0), which keep the served profile's id under any
//! spec, so R0's deployed package digest is reproduced. Nothing leaves the host.
use a_container_serve::engine::{UNNAMED_LOAD_SPEC, spec_arg};
use a_container_serve::profile::{ServedProfile, sha256_hex};
use anyhow::{Context, Result, bail, ensure};
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let profile = ServedProfile::release_candidate()?;
    match args.first().map(String::as_str) {
        Some("show") => println!("{}", serde_json::to_string_pretty(&profile)?),
        Some("sums") => print!("{}", profile.sums()),
        Some("verify") => {
            let (mut model, mut release) = (None, None);
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                let v = it.next().with_context(|| format!("{a} needs a value"));
                match a.as_str() {
                    "--model-dir" => model = Some(PathBuf::from(v?)),
                    "--release-dir" => release = Some(PathBuf::from(v?)),
                    _ => bail!("unknown argument {a}"),
                }
            }
            ensure!(model.is_some() || release.is_some(), "verify needs --model-dir or --release-dir");
            if let Some(dir) = model {
                profile.check_dir(&dir)?;
                println!("model dir: {} served files match {}", profile.files.len(), profile.profile_id);
            }
            if let Some(dir) = release {
                let sums = std::fs::read(dir.join("SHA256SUMS")).context("release SHA256SUMS")?;
                profile.check_release_sums(&sums)?;
                println!("release SHA256SUMS binds every served release file of {}", profile.profile_id);
                println!("graph {} is bound by the served profile only", profile.graph.file);
            }
        }
        Some("pack") => {
            let (mut model, mut out, mut spec) = (None, None, UNNAMED_LOAD_SPEC);
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                let v = it.next().with_context(|| format!("{a} needs a value"))?;
                match a.as_str() {
                    "--model-dir" => model = Some(PathBuf::from(v)),
                    "--out" => out = Some(PathBuf::from(v)),
                    "--spec" => spec = spec_arg(v)?,
                    _ => bail!("unknown argument {a}"),
                }
            }
            let (model, out) = (model.context("--model-dir")?, out.context("--out")?);
            let mut report = slop_ninja_artifact_check::fs::check_dir_for(&model, spec)
                .map_err(|e| anyhow::anyhow!("refused {}: {e}", e.code()))?;
            if profile.check_dir(&model).is_ok() {
                report.parts_manifest.profile_id = profile.profile_id.clone();
            }
            let path = slop_ninja_artifact_check::fs::write_parts(&model, &report, &out)
                .map_err(|e| anyhow::anyhow!("refused {}: {e}", e.code()))?;
            let bytes = std::fs::read(&path)?;
            let m = &report.parts_manifest;
            let parts: std::collections::BTreeSet<&str> =
                m.files.iter().flat_map(|f| f.parts.iter().map(|p| p.sha256.as_str())).collect();
            println!("package_sha256 {}", sha256_hex(&bytes));
            println!("parts {} (files {})", parts.len(), m.files.len());
            println!("graph_class {} ({})", report.graph_class, report.spec);
            println!("content_sha256 {}", report.content_sha256);
            let graph = m.files.iter().find(|f| f.name == m.graph).context("graph")?;
            println!("model_sha256 {}", graph.sha256);
        }
        _ => bail!("usage: slop_ninja_a_served_profile show | sums | verify [--model-dir DIR] [--release-dir DIR] | pack [--spec v2|v3] --model-dir DIR --out DIR"),
    }
    Ok(())
}
