//! Check model directories and release packages against the tracked served profile.
//!
//! slop_ninja_a_served_profile show
//! slop_ninja_a_served_profile sums
//! slop_ninja_a_served_profile verify [--model-dir DIR] [--release-dir DIR]
//! slop_ninja_a_served_profile pack [--spec v2|v3|v4] --model-dir DIR --out DIR
//!
//! `sums` prints `sha256sum -c` lines for the served files. `verify --model-dir` hashes
//! every served file in DIR, including the graph. `verify --release-dir` checks the
//! release package's `SHA256SUMS` digest and lines against the profile; run
//! `sha256sum -c SHA256SUMS` in the package to check its own files.
//!
//! `pack` packs an artifact for a cohort: it checks the model directory with the
//! artifact spec `--spec` names (`v2`, `v3`, `v4` or a full spec id; the spec the cohort
//! names, which the scorer admits the cohort's loads by), else with spec v2, the spec
//! a load that names none is admitted by (`engine::UNNAMED_LOAD_SPEC`). It then
//! writes the reference files layout v2 (each file once under `OUT/files/<sha256>` and
//! the unpinned files manifest v2 `OUT/artifact-files.json`, naming the admission
//! bucket key `reference/v2/files/<sha256>`) and prints that manifest's SHA-256. The
//! operator stages it with `slop_ninja_reference_stage` (`slop_ninja_reference_staging_v1`): each
//! file goes up under a presigned PUT that binds its SHA-256, so R2 verifies it, and
//! the eval Worker pins each file's entity tag and writes the package manifest, whose
//! digest (printed by the stage tool) is the package a template names.
//! The manifest's `profile_id` is that spec's (`<spec id>/<graph class id>`), except
//! for the served profile's own files (R0), which keep the served profile's id under
//! any spec. Nothing leaves the host.
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
            let (header, mut report) = slop_ninja_artifact_check::fs::check_dir_with_header(&model, spec)
                .map_err(|e| anyhow::anyhow!("refused {}: {e}", e.code()))?;
            if profile.check_dir(&model).is_ok() {
                report.profile_id = profile.profile_id.clone();
            }
            let (path, package) = slop_ninja_artifact_check::fs::write_files(&model, &header, &report, spec, &out)
                .map_err(|e| anyhow::anyhow!("refused {}: {e}", e.code()))?;
            ensure!(sha256_hex(&std::fs::read(&path)?) == package, "manifest digest");
            println!("unpinned_manifest_sha256 {package}");
            println!("files {}", header.files.len());
            println!("graph_class {} ({})", report.graph_class, report.spec);
            println!("content_sha256 {}", report.content_sha256);
            let graph = header.files.iter().find(|f| f.name == slop_ninja_artifact_check::spec::GRAPH_FILE).context("graph")?;
            println!("model_sha256 {}", hex::encode(graph.sha256.0));
        }
        _ => bail!("usage: slop_ninja_a_served_profile show | sums | verify [--model-dir DIR] [--release-dir DIR] | pack [--spec v2|v3|v4] --model-dir DIR --out DIR"),
    }
    Ok(())
}
