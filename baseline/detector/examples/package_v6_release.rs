//! Bundle completed v6 evidence and an unchanged encoder; no inference or publishing.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const CREDITS: &str = "fb1c81a6b655c696bc18da175cc9f1dd04ab90912409d8d849b1baad30e9a493";
const RECORDS: &str = "1b06538d11fc770fbf6c68b5b9cdb95f12463ebbc6f37e7d518ad8aa08859475";
const PANGRAM_PLAN: &str = "5699a1348650f14d5d5747428c25bcc1760c76eb40651ddb139d789d63db6df3";
const EVALUATION_PLAN: &str = "c6b5b1d2db43b74aafe175c6f509e02992d0a5e2caeac2b553380b35b802d424";
const V5: &str = "da3a604d7bd1708f5ba1c4a4a4be946d77670492b702e41e267fb86237dfb8c9";

#[derive(Parser)]
struct Args {
    #[arg(long)]
    run_root: PathBuf,
    /// Completed synthetic-only CPU replay receipt for the selected encoder.
    #[arg(long)]
    runtime_verification: PathBuf,
    /// README.md, NOTICE.md and RESULTS.md written after reviewing all comparisons.
    #[arg(long)]
    notes_dir: PathBuf,
    /// Public repository commit containing the reviewed reports and results prose.
    #[arg(long)]
    source_commit: String,
    #[arg(long)]
    output_dir: PathBuf,
}

fn hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("Read {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("Missing {key}"))
}

fn relative(text: &str) -> Result<&Path> {
    let path = Path::new(text);
    ensure!(
        !text.is_empty() && path.components().all(|c| matches!(c, Component::Normal(_))),
        "Expected a relative member path"
    );
    Ok(path)
}

fn public(value: &mut Value, root: &str) -> Result<()> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                ensure!(
                    !matches!(
                        key.as_str(),
                        "text"
                            | "prompt"
                            | "raw_path"
                            | "raw_request"
                            | "raw_response"
                            | "response_text"
                            | "source_text"
                            | "predictions"
                            | "probability_rows"
                            | "calibration_ids"
                            | "calibration_groups"
                            | "calibration_text_hashes"
                            | "ids"
                    ),
                    "Unexpected individual text or score field: {key}"
                );
                ensure!(
                    !key.contains("/Users/") && !key.contains("/home/"),
                    "Private path key"
                );
                public(child, root)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                public(child, root)?;
            }
        }
        Value::String(text) => {
            if let Some(member) = text.strip_prefix(&format!("{root}/")) {
                *text = format!("${{RUN_ROOT}}/{member}");
            }
            ensure!(
                !["/Users/", "/home/", "grammar-private", "Bearer ", "file://"]
                    .iter()
                    .any(|marker| text.contains(marker)),
                "Unexpected private path or credential marker"
            );
        }
        _ => {}
    }
    Ok(())
}

#[derive(Default)]
struct Inputs(BTreeMap<PathBuf, String>);
impl Inputs {
    fn bind(&mut self, path: &Path) -> Result<String> {
        let digest = hash(path)?;
        if let Some(old) = self.0.insert(path.to_owned(), digest.clone()) {
            ensure!(old == digest, "Input changed: {}", path.display());
        }
        Ok(digest)
    }
    fn check(&mut self, path: &Path, expected: &str) -> Result<()> {
        ensure!(
            self.bind(path)? == expected,
            "Input binding differs: {}",
            path.display()
        );
        Ok(())
    }
    fn json(&mut self, path: &Path) -> Result<Value> {
        let bytes = fs::read(path).with_context(|| format!("Read {}", path.display()))?;
        self.check(path, &sha256(&bytes))?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    fn finish(&self) -> Result<()> {
        for (path, expected) in &self.0 {
            ensure!(
                hash(path)? == *expected,
                "Input changed: {}",
                path.display()
            );
        }
        Ok(())
    }
}

fn save(
    root: &Path,
    member: &str,
    bytes: &[u8],
    sums: &mut BTreeMap<String, String>,
) -> Result<()> {
    let target = root.join(relative(member)?);
    fs::create_dir_all(target.parent().context("Missing parent")?)?;
    let mut file = fs::File::create_new(&target)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    let digest = sha256(bytes);
    ensure!(hash(&target)? == digest, "Output differs after writing");
    ensure!(
        sums.insert(member.to_owned(), digest).is_none(),
        "Duplicate member"
    );
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new release directory");
    ensure!(
        args.source_commit.len() == 40 && args.source_commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "Expected a full source commit"
    );
    let root = args.run_root.canonicalize()?;
    let root_string = root.to_str().context("Non-UTF-8 root")?;
    let mut inputs = Inputs::default();
    // Stop before looking for release material if the original evaluation is incomplete.
    let control = root.join("postfit-evaluation-v1/guarded-handoff-v2");
    let done = inputs.json(&control.join("completion.json"))?;
    let status = inputs.json(&control.join("status.json"))?;
    ensure!(
        done["success"] == true
            && done["check_only"] == false
            && status["stage"] == "evaluation_complete_results_require_review"
            && status["test_predictions_opened"] == true,
        "Original evaluation must finish first"
    );

    let report_path = root.join("release-report-v1/report.json");
    let report = inputs.json(&report_path)?;
    let verification = inputs.json(&root.join("release-report-v1/verification.json"))?;
    ensure!(
        report["schema"] == "slop_ninja_revision_v6_release_evidence_v1"
            && report["final_test_opened"] == true
            && report["provenance"]["plan_sha256"] == EVALUATION_PLAN,
        "Unexpected evaluation evidence"
    );
    ensure!(
        verification["report_sha256"] == inputs.0[&report_path]
            && verification["evaluation_commands_checked"] == 22
            && verification["reports"] == 14
            && verification["paired_comparisons"] == 7
            && verification["overall_metrics_recomputed_from_saved_probabilities"] == true,
        "Incomplete report verification"
    );
    let id = field(&report["selection"]["selected"], "artifact_id")?;
    ensure!(
        id != V5
            && report["selection"]["final_test_opened"] == false
            && report["selection"]["calibration_used_to_select_candidate"] == false,
        "Unexpected selection"
    );
    let member = field(&report["selection"]["selected"], "path")?
        .strip_prefix("${RUN_ROOT}/")
        .context("Selected path was not projected")?;
    let artifact = root.join(relative(member)?);
    ensure!(
        artifact.parent() == Some(root.join("encoder-frontier-v6").as_path()),
        "Artifact is outside frontier"
    );
    let selection_path = root.join("encoder-frontier-v6/selection.json");
    inputs.check(
        &selection_path,
        field(
            &report["provenance"]["input_sha256"],
            "encoder-frontier-v6/selection.json",
        )?,
    )?;
    let mut selection = inputs.json(&selection_path)?;
    public(&mut selection, root_string)?;
    ensure!(
        selection == report["selection"],
        "Selection projection differs"
    );
    let manifest_path = artifact.join("manifest.json");
    inputs.check(
        &manifest_path,
        field(&report["detectors"]["v6"], "manifest_sha256")?,
    )?;
    let manifest = inputs.json(&manifest_path)?;
    ensure!(
        manifest["artifact_id"] == id && manifest["max_tokens"] == 2048,
        "Unexpected encoder"
    );

    let runtime = inputs.json(&args.runtime_verification)?;
    ensure!(
        runtime["schema"] == "slop_ninja_encoder_cli_runtime_verification_v1"
            && runtime["artifact_id"] == id
            && runtime["manifest_sha256"] == inputs.0[&manifest_path]
            && runtime["artifact"] == artifact.to_str().context("Non-UTF-8 artifact")?,
        "Runtime receipt refers to another artifact"
    );
    ensure!(
        runtime["bundled_cpu_reference_replay_passed"] == true
            && runtime["reference_vector_count"] == 2
            && runtime["exit_code"] == 0
            && runtime["stdin"] == "/dev/null"
            && runtime["model_forward_partitions"] == json!([])
            && runtime["checked_reference_runtime"] == manifest["reference_runtime"],
        "Synthetic CPU replay did not pass"
    );

    let confirmation = root.join("pangram-confirmation-v1");
    let anchor_path = confirmation.join("comparison-v1/summary.json");
    let anchor = inputs.json(&anchor_path)?;
    ensure!(
        anchor["schema"] == "slop_ninja_pangram_revision_comparison_v1"
            && anchor["records_sha256"] == RECORDS
            && anchor["plan_sha256"] == PANGRAM_PLAN
            && anchor["unique_texts"]["all"]["rows"] == 162,
        "Fresh Pangram comparison is incomplete or differs"
    );
    ensure!(
        anchor["views"]
            .as_object()
            .context("Missing Pangram views")?
            .len()
            == 6,
        "Missing Pangram stage view"
    );
    for (index, version, expected) in [(0, "v5", V5), (1, "v6", id)] {
        let thresholds = &report["detectors"][version]["thresholds"];
        ensure!(
            anchor["detector_bindings"][index]["artifact_id"] == expected
                && anchor["detector_bindings"][index]["thresholds_sha256"]
                    == thresholds["source_file_sha256"]
                && anchor["detector_bindings"][index]["points"]
                    == thresholds["content_projection"]["points"],
            "Pangram comparison uses different detector or thresholds"
        );
    }
    let subsets_path = confirmation.join("encoder-subsets-v1/summary.json");
    let subsets = inputs.json(&subsets_path)?;
    ensure!(
        subsets["schema"] == "slop_ninja_revision_subset_comparisons_v1"
            && subsets["selected_families"] == 18
            && subsets["unique_texts"] == 162,
        "Unexpected subset evidence"
    );
    let pairs = subsets["comparisons"]
        .as_object()
        .context("Missing subset comparisons")?;
    ensure!(pairs.len() == 6, "Missing subset comparison");
    for route in ["primary", "phi4"] {
        for stage in ["R0", "R1", "R2"] {
            let pair = &subsets["comparisons"][format!("{route}-{stage}")];
            ensure!(
                pair["baseline_artifact_id"] == V5
                    && pair["candidate_artifact_id"] == id
                    && pair["records_sha256"] == RECORDS
                    && pair["human_model_comparison"]["source_groups"] == 18
                    && pair["human_model_comparison"]["bootstrap_replicates"] == 512,
                "Subset comparison identity or coverage differs"
            );
        }
    }
    let credits_path = root.join("source-credits-v1/source-credits.json");
    inputs.check(&credits_path, CREDITS)?;
    let credits = inputs.json(&credits_path)?;
    ensure!(
        credits["summary"]["records"] == 4903
            && credits["summary"]["source_works"] == 685
            && report["source_credits"]["sha256"] == CREDITS,
        "Source credit scope differs"
    );

    // Assemble small public metadata before any output is created. Preserve original
    // input hashes separately: a projected JSON file has its own output checksum.
    let mut metadata = BTreeMap::<String, Vec<u8>>::new();
    let mut bindings = BTreeMap::<String, Value>::new();
    for (name, mut value, path) in [
        (
            "results/encoder-revision-v6.json",
            report.clone(),
            report_path,
        ),
        ("results/pangram-revision-v6.json", anchor, anchor_path),
        (
            "results/pangram-revision-v6-paired.json",
            subsets,
            subsets_path,
        ),
        (
            "provenance/runtime-verification.json",
            runtime,
            args.runtime_verification.clone(),
        ),
        ("provenance/source-credits.json", credits, credits_path),
    ] {
        public(&mut value, root_string)?;
        let mut bytes = serde_json::to_vec_pretty(&value)?;
        bytes.push(b'\n');
        metadata.insert(name.into(), bytes);
        bindings.insert(name.into(), json!({"original_sha256":inputs.0[&path],"projection":"Paths under the run root use ${RUN_ROOT}; content otherwise retained."}));
    }
    let thresholds = &report["detectors"]["v6"]["thresholds"];
    let operating_points = json!({
        "schema":"slop_ninja_released_operating_points_v1", "artifact_id":id,
        "original_threshold_file_sha256":thresholds["source_file_sha256"],
        "original_threshold_file_content_sha256":thresholds["original_content_sha256"],
        "points":thresholds["content_projection"]["points"],
        "temperature_refitted":false,
        "score":"P(model_only) + P(mixed); strictly greater than the cutoff"
    });
    let mut bytes = serde_json::to_vec_pretty(&operating_points)?;
    bytes.push(b'\n');
    metadata.insert("operating-points.json".into(), bytes);
    for name in ["README.md", "NOTICE.md", "RESULTS.md"] {
        let path = args.notes_dir.join(name);
        let bytes = fs::read(&path).with_context(|| format!("Read final release prose {name}"))?;
        ensure!(!bytes.is_empty(), "Empty release prose");
        let mut prose = json!(std::str::from_utf8(&bytes)?);
        public(&mut prose, root_string)?;
        ensure!(
            prose.as_str().unwrap().as_bytes() == bytes,
            "Release prose contains a private run path"
        );
        inputs.check(&path, &sha256(&bytes))?;
        bindings.insert(
            name.into(),
            json!({"original_sha256":inputs.0[&path],"projection":"none"}),
        );
        metadata.insert(name.into(), bytes);
    }
    for (name, bytes) in [
        ("LICENSE.code", include_bytes!("../LICENSE").as_slice()),
        (
            "provenance/SHARE_ALIKE_POLICY.md",
            include_bytes!("../SHARE_ALIKE_POLICY.md").as_slice(),
        ),
        (
            "provenance/ENCODER_REVISION_V6.md",
            include_bytes!("../ENCODER_REVISION_V6.md").as_slice(),
        ),
        (
            "provenance/PANGRAM_REVISION_V6.md",
            include_bytes!("../PANGRAM_REVISION_V6.md").as_slice(),
        ),
        (
            "provenance/prompts/fix-slop-v1.txt",
            include_bytes!("../prompts/fix-slop-v1.txt").as_slice(),
        ),
        (
            "provenance/prompts/LICENSE.fix-slop",
            include_bytes!("../prompts/LICENSE.fix-slop").as_slice(),
        ),
    ] {
        bindings.insert(
            name.into(),
            json!({"original_sha256":sha256(bytes),"projection":"none; compiled source asset"}),
        );
        metadata.insert(name.into(), bytes.to_vec());
    }
    let mut encoder_files = BTreeMap::new();
    encoder_files.insert("manifest.json".to_owned(), inputs.0[&manifest_path].clone());
    for (name, digest) in manifest["files"]
        .as_object()
        .context("Missing encoder members")?
    {
        relative(name)?;
        ensure!(
            !encoder_files.contains_key(name),
            "Duplicate encoder member"
        );
        let path = artifact.join(name);
        ensure!(
            path.canonicalize()?.starts_with(artifact.canonicalize()?),
            "Encoder member escapes artifact"
        );
        ensure!(
            !fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "Symlink encoder member"
        );
        let digest = digest.as_str().context("Invalid member digest")?;
        inputs.check(&path, digest)?;
        if name == "reference_vectors.json" {
            let vectors = inputs.json(&path)?;
            ensure!(
                vectors
                    .as_array()
                    .context("Missing reference vectors")?
                    .len()
                    == 2
                    && vectors[0]["text"] == "."
                    && vectors[1]["text"]
                        == "This synthetic sentence checks the reference runtime.",
                "Unexpected reference text"
            );
        } else if path.extension().is_some_and(|s| s == "json")
            && name != "classifier/tokenizer.json"
        {
            let mut value = inputs.json(&path)?;
            let before = value.clone();
            public(&mut value, root_string)?;
            ensure!(value == before, "Immutable encoder contains a private path");
        }
        encoder_files.insert(name.clone(), digest.to_owned());
    }
    for required in [
        "LICENSE",
        "UPSTREAM_LICENSE",
        "DATA_RIGHTS.json",
        "MODEL_CARD.json",
        "runner/infer.py",
        "runner/requirements.lock",
    ] {
        ensure!(
            encoder_files.contains_key(required),
            "Required encoder component missing: {required}"
        );
    }
    inputs.finish()?;
    fs::create_dir(&args.output_dir)?;
    let mut sums = BTreeMap::new();
    for (name, bytes) in metadata {
        save(&args.output_dir, &name, &bytes, &mut sums)?;
    }
    for (name, expected) in &encoder_files {
        let member = format!("encoder/{name}");
        let target = args.output_dir.join(&member);
        fs::create_dir_all(target.parent().context("Missing output parent")?)?;
        ensure!(!target.exists(), "Encoder output already exists");
        fs::copy(artifact.join(name), &target)?;
        ensure!(hash(&target)? == *expected, "Copied encoder member differs");
        sums.insert(member, expected.clone());
    }
    inputs.finish()?;
    let release = json!({
        "schema":"slop_ninja_research_release_v2", "version":"v0.6.0",
        "status":"unqualified_research_candidate", "artifact_id":id,
        "source_commit":args.source_commit, "repository":"https://github.com/sam0x17/slop-ninja",
        "encoder_manifest_sha256":inputs.0[&manifest_path], "artifact_bytes_preserved":true,
        "source_credits_original_sha256":CREDITS, "source_bindings":bindings,
        "packaging_helper_sha256":sha256(include_bytes!("package_v6_release.rs")),
        "packaging_executable_sha256":hash(&std::env::current_exe()?)?,
        "encoder_member_count":encoder_files.len(), "model_calls":0,"pangram_calls":0,
        "test_corpus_read_for_packaging":false,
        "limits":"Packaging verifies artifact and evidence bindings. It does not judge the results prose, establish detector quality, confer subnet qualification or promote this artifact to the reference. Review all final reports, including unfavorable and failed observations, before publication."
    });
    let mut bytes = serde_json::to_vec_pretty(&release)?;
    bytes.push(b'\n');
    save(
        &args.output_dir,
        "provenance/release.json",
        &bytes,
        &mut sums,
    )?;
    let checksums: String = sums
        .iter()
        .map(|(name, digest)| format!("{digest}  {name}\n"))
        .collect();
    let mut file = fs::File::create_new(args.output_dir.join("SHA256SUMS"))?;
    file.write_all(checksums.as_bytes())?;
    file.sync_all()?;
    println!(
        "{}",
        json!({"artifact_id":id,"files_in_checksums":sums.len(),"ready_for_archive":true,"status":"research_bundle_requires_final_review"})
    );
    Ok(())
}
