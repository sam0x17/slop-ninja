//! Assemble public v6 evidence from the original completed evaluation, without inference.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use slop_ninja_detector::{
    dataset::sha256,
    metrics::{self, OperatingPoint, ProbabilityRow},
};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const PLAN: &[u8] = include_bytes!("../manifests/v6-postfit-commands-v1.json");
const PLAN_SHA: &str = "c6b5b1d2db43b74aafe175c6f509e02992d0a5e2caeac2b553380b35b802d424";
const V5: &str = "da3a604d7bd1708f5ba1c4a4a4be946d77670492b702e41e267fb86237dfb8c9";
const CREDITS_SHA: &str = "fb1c81a6b655c696bc18da175cc9f1dd04ab90912409d8d849b1baad30e9a493";

#[derive(Parser)]
struct Args {
    #[arg(long)]
    run_root: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
}

fn hash(path: &Path) -> Result<String> {
    let mut input = fs::File::open(path).with_context(|| format!("Read {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
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
            "Input hash differs: {}",
            path.display()
        );
        Ok(())
    }
    fn json(&mut self, path: &Path) -> Result<Value> {
        let bytes = fs::read(path).with_context(|| format!("Read {}", path.display()))?;
        self.check(path, &sha256(&bytes))?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    fn finish(&self, root: &Path) -> Result<BTreeMap<String, String>> {
        let mut relative = BTreeMap::new();
        for (path, digest) in &self.0 {
            ensure!(hash(path)? == *digest, "Input changed: {}", path.display());
            relative.insert(
                path.strip_prefix(root)?
                    .to_str()
                    .context("Non-UTF-8 path")?
                    .to_owned(),
                digest.clone(),
            );
        }
        Ok(relative)
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("Missing {key}"))
}
fn save(path: &Path, value: &Value) -> Result<String> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let mut file = fs::File::create_new(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(sha256(bytes))
}

// Known per-record score/threshold fields are omitted. Unexpected text or private
// paths cause failure instead of being silently accepted in public metadata.
fn project(value: &mut Value, root: &str) -> Result<()> {
    match value {
        Value::Object(map) => {
            for key in [
                "predictions",
                "ids",
                "calibration_ids",
                "calibration_groups",
                "calibration_text_hashes",
            ] {
                map.remove(key);
            }
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
                            | "probability_rows"
                    ),
                    "Unexpected raw field: {key}"
                );
                ensure!(
                    !key.contains("/Users/") && !key.contains("/home/"),
                    "Absolute path key in public metadata"
                );
                project(child, root)?;
            }
        }
        Value::Array(values) => {
            for item in values {
                project(item, root)?;
            }
        }
        Value::String(text) => {
            if let Some(relative) = text.strip_prefix(&format!("{root}/")) {
                *text = format!("${{RUN_ROOT}}/{relative}");
            }
            ensure!(
                !["/Users/", "/home/", "grammar-private", "Bearer ", "file://"]
                    .iter()
                    .any(|s| text.contains(s)),
                "Unexpected private path or credential marker"
            );
        }
        _ => {}
    }
    Ok(())
}

fn required_frozen(path: &Path, frozen: &serde_json::Map<String, Value>) -> Result<()> {
    ensure!(
        frozen.contains_key(path.to_str().context("Non-UTF-8 path")?),
        "Input was not frozen before Test: {}",
        path.display()
    );
    Ok(())
}

fn verify_report(
    report: &Value,
    artifact: &str,
    threshold_hash: &str,
    points: &[OperatingPoint],
    prior: &Value,
    groups: usize,
    human_only: bool,
) -> Result<()> {
    ensure!(
        report["schema"] == "slop_ninja_frozen_encoder_evaluation_v1"
            && report["artifact_id"] == artifact
            && report["report"]["artifact_sha256"] == artifact,
        "Report artifact or schema differs"
    );
    ensure!(
        report["split"] == "test"
            && report["final_test_opened"] == true
            && report["thresholds_sha256"] == threshold_hash,
        "Report partition or operating threshold binding differs"
    );
    ensure!(
        report["effective_training_class_prior"] == *prior,
        "Report training prior differs from the frozen thresholds"
    );
    let predictions: Vec<ProbabilityRow> =
        serde_json::from_value(report["report"]["predictions"].clone())?;
    let expected_rows = if human_only { groups } else { 3 * groups };
    ensure!(
        predictions.len() == expected_rows,
        "Unexpected report coverage"
    );
    ensure!(
        serde_json::to_value(metrics::summarize(&predictions)?)? == report["report"]["model"],
        "Saved overall metrics differ from probabilities"
    );
    ensure!(
        serde_json::to_value(metrics::summarize_human_model(&predictions)?)?
            == report["human_model"],
        "Saved human/model metrics differ from probabilities"
    );
    ensure!(
        report["report"]["model"]["source_groups"] == groups,
        "Unexpected source-family count"
    );
    let mut families = BTreeMap::<&str, [usize; 3]>::new();
    for row in &predictions {
        families.entry(&row.group).or_default()[row.label] += 1;
    }
    let expected = if human_only { [1, 0, 0] } else { [1, 1, 1] };
    ensure!(
        families.values().all(|counts| *counts == expected),
        "Report family origins differ"
    );
    if human_only {
        ensure!(
            report["observation_policy"] == "all_human_roots_v1"
                && report.get("evaluation_view").is_none(),
            "Human diagnostic policy differs"
        );
    } else {
        ensure!(
            report["evaluation_view"]["source_groups"] == groups
                && report["evaluation_view"]["selected_rows"] == expected_rows,
            "Stage view coverage differs"
        );
    }
    let operating = report["report"]["operating_points"]
        .as_array()
        .context("Missing operating points")?;
    ensure!(
        operating.len() == points.len(),
        "Operating point count differs"
    );
    for (evaluation, point) in operating.iter().zip(points) {
        ensure!(
            evaluation["calibration"] == serde_json::to_value(point)?,
            "Operating point differs from the pre-Test freeze"
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new output directory");
    let root = args.run_root.canonicalize()?;
    let root_string = root.to_str().context("Non-UTF-8 root")?;
    let mut inputs = Inputs::default();
    let control = root.join("postfit-evaluation-v1/guarded-handoff-v2");
    // This gate precedes reading any Test report or creating any output.
    let completion = inputs.json(&control.join("completion.json"))?;
    let status = inputs.json(&control.join("status.json"))?;
    ensure!(
        completion["success"] == true
            && completion["check_only"] == false
            && status["stage"] == "evaluation_complete_results_require_review"
            && status["test_predictions_opened"] == true,
        "Original evaluation must complete successfully first"
    );
    ensure!(sha256(PLAN) == PLAN_SHA, "Embedded command plan changed");
    let plan: Value = serde_json::from_slice(PLAN)?;
    ensure!(
        plan["run_root"] == root_string,
        "This evidence collector requires the declared experiment root"
    );
    let evaluation = root.join("final-evaluation-v1");
    let freeze_path = evaluation.join("pre-test-bindings.json");
    let freeze = inputs.json(&freeze_path)?;
    ensure!(
        freeze["schema"] == "slop_ninja_v6_final_test_bindings_v1"
            && freeze["plan_sha256"] == PLAN_SHA
            && freeze["test_predictions_opened"] == false,
        "Unexpected pre-Test freeze"
    );
    let opening = inputs.json(&evaluation.join("opening-receipt.json"))?;
    ensure!(
        opening["pre_test_bindings_sha256"] == inputs.0[&freeze_path]
            && opening["checksum_file_sha256"]
                == inputs.bind(&evaluation.join("pre-test-bindings.sha256"))?,
        "Opening receipt differs"
    );
    let frozen = freeze["bindings"]
        .as_object()
        .context("Missing pre-Test bindings")?;
    for (path, digest) in frozen {
        inputs.check(
            Path::new(path),
            digest.as_str().context("Invalid frozen digest")?,
        )?;
    }
    for (relative, digest) in plan["static_sha256"]
        .as_object()
        .context("Missing static bindings")?
    {
        inputs.check(
            &root.join(relative),
            digest.as_str().context("Invalid static digest")?,
        )?;
    }
    let candidate = PathBuf::from(field(&freeze, "candidate_artifact")?);
    ensure!(
        candidate.parent() == Some(root.join("encoder-frontier-v6").as_path()),
        "Selected artifact is outside the frontier"
    );
    let selection_path = root.join("encoder-frontier-v6/selection.json");
    required_frozen(&selection_path, frozen)?;
    let selection = inputs.json(&selection_path)?;
    ensure!(
        selection["final_test_opened"] == false
            && selection["calibration_used_to_select_candidate"] == false
            && selection["selected"]["path"]
                == candidate.to_str().context("Non-UTF-8 artifact path")?,
        "Selection differs from the pre-Test artifact"
    );
    let commands = plan["commands"].as_array().context("Missing commands")?;
    ensure!(commands.len() == 22, "Unexpected evaluation command count");
    for (index, command) in commands.iter().enumerate() {
        let receipt = inputs.json(&control.join(format!("{index:02}-completion.json")))?;
        ensure!(
            receipt["exit_code"] == 0 && receipt["command"] == command["id"],
            "Evaluation command did not complete: {index}"
        );
        let invocation = inputs.json(&control.join(format!("{index:02}-invocation.json")))?;
        let expected_args: Vec<Value> = command["args"]
            .as_array()
            .context("Missing argv")?
            .iter()
            .map(|arg| {
                if arg == &json!({"binding":"selected_candidate_artifact"}) {
                    json!(candidate)
                } else {
                    arg.clone()
                }
            })
            .collect();
        ensure!(
            invocation["program"] == command["program"]
                && invocation["args"] == json!(expected_args),
            "Executed argv differs: {index}"
        );
    }
    let mut detectors = BTreeMap::new();
    let mut detector_evidence = BTreeMap::new();
    for (name, key, thresholds_path) in [
        ("v5", "v5_artifact", root.join("repo/data/baseline-detector/narrative-expansion-v5/recovered-first-thresholds-v1/thresholds.json")),
        ("v6", "candidate_artifact", evaluation.join("v6-thresholds/thresholds.json")),
    ] {
        let artifact = PathBuf::from(field(&freeze, key)?);
        required_frozen(&artifact.join("manifest.json"), frozen)?;
        required_frozen(&thresholds_path, frozen)?;
        let manifest = inputs.json(&artifact.join("manifest.json"))?;
        let artifact_id = field(&manifest, "artifact_id")?.to_owned();
        ensure!((name != "v5" || artifact_id == V5) && (name != "v6" || selection["selected"]["artifact_id"] == artifact_id), "Artifact identity differs");
        let training = inputs.json(&artifact.join("training.json"))?;
        let calibration = inputs.json(&artifact.join("calibration.json"))?;
        let thresholds = inputs.json(&thresholds_path)?;
        let content = &thresholds["content"];
        ensure!(thresholds["content_sha256"] == sha256(serde_json::to_vec(content)?) && content["artifact_id"] == artifact_id && content["temperature_refitted"] == false && content["test_predictions_opened"] == false, "Threshold freeze differs");
        let points: Vec<OperatingPoint> = serde_json::from_value(content["points"].clone())?;
        ensure!(points.len() == 2, "Expected two operating points");
        for (point, rate) in points.iter().zip([0.01, 0.05]) { point.validate()?; ensure!(point.target_human_false_positive_rate == rate, "Unexpected target rate"); }
        detectors.insert(name, (artifact_id, inputs.0[&thresholds_path].clone(), points, content["effective_training_class_prior"].clone()));
        detector_evidence.insert(name, json!({"manifest_sha256":inputs.0[&artifact.join("manifest.json")],"training":training,"calibration":calibration,"thresholds":{"source_file_sha256":inputs.0[&thresholds_path],"original_content_sha256":thresholds["content_sha256"],"content_projection":content}}));
    }
    let mut coverage = BTreeMap::new();
    for (route, groups) in [("primary", 112), ("phi4", 104)] {
        let path = root.join(format!("{route}-coverage-v1/coverage.json"));
        required_frozen(&path, frozen)?;
        let value = inputs.json(&path)?;
        ensure!(
            value["full_route"]["source_groups"] == groups
                && value["common_route"]["source_groups"] == groups
                && value["full_route"]["records_sha256"] == value["common_route"]["records_sha256"]
                && value["human_roots"]["assigned_roots"] == 117
                && value["excluded_family_count"] == 0,
            "Coverage differs from the declared comparison"
        );
        coverage.insert(route, value);
    }
    let mut reports = BTreeMap::new();
    let mut comparisons = BTreeMap::new();
    for (key, groups, human_only) in [
        ("primary-R2", 112, false),
        ("primary-R0", 112, false),
        ("primary-R1", 112, false),
        ("phi4-R2", 104, false),
        ("phi4-R0", 104, false),
        ("phi4-R1", 104, false),
        ("all-humans", 117, true),
    ] {
        let mut originals = BTreeMap::new();
        for name in ["v5", "v6"] {
            let path = evaluation.join(format!("{key}-{name}/report.json"));
            let report = inputs.json(&path)?;
            let (artifact, threshold, points, prior) = &detectors[name];
            verify_report(
                &report, artifact, threshold, points, prior, groups, human_only,
            )?;
            let expected_archive = if human_only {
                &coverage["primary"]["human_roots"]["original_records_sha256"]
            } else {
                &coverage[if key.starts_with("primary") {
                    "primary"
                } else {
                    "phi4"
                }]["common_route"]["records_sha256"]
            };
            ensure!(
                report["records_sha256"] == *expected_archive,
                "Report archive differs from frozen coverage"
            );
            if !human_only {
                let (route, stage) = key.split_once('-').context("Invalid stage")?;
                let view_path = root.join(format!("{route}-coverage-v1/common/{stage}.json"));
                required_frozen(&view_path, frozen)?;
                ensure!(
                    report["evaluation_view"]["sha256"] == inputs.bind(&view_path)?,
                    "Report stage binding differs"
                );
            }
            originals.insert(name, (report.clone(), inputs.0[&path].clone()));
            reports.insert(format!("{key}-{name}"), report);
        }
        let pair = inputs.json(&evaluation.join(format!("{key}-comparison.json")))?;
        ensure!(
            pair["schema"] == "slop_ninja_paired_encoder_comparison_v1" && pair["split"] == "test",
            "Unexpected paired comparison"
        );
        for (prefix, name) in [("baseline", "v5"), ("candidate", "v6")] {
            let (report, digest) = &originals[name];
            ensure!(
                pair[format!("{prefix}_report_sha256")] == *digest
                    && pair[format!("{prefix}_artifact_id")] == detectors[name].0
                    && pair[format!("{prefix}_metrics")] == report["report"]["model"]
                    && pair[format!("{prefix}_operating_points")]
                        == report["report"]["operating_points"],
                "Paired comparison refers to a different report"
            );
        }
        ensure!(
            pair["human_model_comparison"]["source_groups"] == groups
                && pair["human_model_comparison"]["bootstrap_replicates"] == 512,
            "Paired coverage or bootstrap count differs"
        );
        for name in ["v5", "v6"] {
            let report = &originals[name].0;
            ensure!(
                pair["records_sha256"] == report["records_sha256"]
                    && pair.get("evaluation_view") == report.get("evaluation_view")
                    && pair.get("observation_policy") == report.get("observation_policy"),
                "Paired corpus or observation policy differs"
            );
        }
        let loss = |name: &str| -> Result<f64> {
            originals[name].0["human_model"]["log_loss"]
                .as_f64()
                .context("Missing human/model loss")
        };
        let delta =
            pair["human_model_comparison"]["deltas"]["log_loss"]["candidate_minus_baseline"]
                .as_f64()
                .context("Missing paired loss delta")?;
        ensure!(
            (delta - (loss("v6")? - loss("v5")?)).abs() < 1e-12,
            "Paired loss delta differs from the saved metrics"
        );
        comparisons.insert(key, pair);
    }
    let credits_path = root.join("source-credits-v1/source-credits.json");
    inputs.check(&credits_path, CREDITS_SHA)?;
    let credits = inputs.json(&credits_path)?;
    let bindings = inputs.finish(&root)?;
    let mut report = json!({
        "schema":"slop_ninja_revision_v6_release_evidence_v1","status":"completed_evaluation_requires_scientific_review",
        "final_test_opened":true,"selection":selection,"detectors":detector_evidence,
        "coverage":coverage,"reports":reports,"comparisons":comparisons,
        "primary_statistic":comparisons["primary-R2"]["human_model_comparison"]["deltas"]["log_loss"],
        "source_credits":{"sha256":CREDITS_SHA,"summary":credits["summary"]},
        "provenance":{"plan_sha256":PLAN_SHA,"pre_test_bindings_sha256":inputs.0[&freeze_path],"opening":opening,"input_sha256":bindings,"helper_sha256":sha256(include_bytes!("report_v6_release.rs")),"executable_sha256":hash(&std::env::current_exe()?)?},
        "model_calls":0,"pangram_calls":0,"paired_comparisons_reused":true,
        "projection_policy":"Known per-record predictions and cutoff/calibration identifiers are omitted; declared run-root paths are replaced by ${RUN_ROOT}. Source-file and original-content hashes refer to the unchanged input files, not these public projections.",
        "pangram_comparison_status":"separate_frozen_cohort_not_joined_by_this_helper",
        "limits":["Full/common stage views are byte-identical; one result covers each pair. Shared human and mixed texts across routes and stages are not independent samples.","The 117-root diagnostic includes generation failures but supplies no model sensitivity estimate.","Development selected the candidate. Test was opened only after artifacts, temperatures and operating points were frozen; report assembly does not select or promote a model.","Different frozen thresholds do not establish superiority at matched population false-positive rates. These source-conditioned genres and generators do not establish general authorship detection or Pangram parity."]
    });
    project(&mut report, root_string)?;
    fs::create_dir(&args.output_dir)?;
    let digest = save(&args.output_dir.join("report.json"), &report)?;
    save(
        &args.output_dir.join("verification.json"),
        &json!({"report_sha256":digest,"evaluation_commands_checked":22,"reports":14,"paired_comparisons":7,"overall_metrics_recomputed_from_saved_probabilities":true,"model_calls":0,"pangram_calls":0,"status":"requires_scientific_review"}),
    )?;
    println!(
        "Assembled fourteen reports and seven frozen paired comparisons; scientific review and the separate Pangram comparison remain required."
    );
    Ok(())
}
