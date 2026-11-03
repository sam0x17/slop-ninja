//! Bounded development-only word/grammar comparison; no provider or Test calls.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::sha256,
    features::Coordinate,
    model::{DetectorArtifact, TrainingReport},
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    features: PathBuf,
    #[arg(long)]
    detector_cli: PathBuf,
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    source_commit: String,
    #[arg(long)]
    output_dir: PathBuf,
}

fn digest(path: &Path) -> Result<String> {
    Ok(sha256(fs::read(path)?))
}
fn write(path: &Path, value: &Value) -> Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.write_all(b"\n")?;
    Ok(())
}
fn subset_hash(coordinates: &[Coordinate], word: bool) -> Result<String> {
    Ok(sha256(serde_json::to_vec(
        &coordinates
            .iter()
            .filter(|c| c.key.is_word() == word)
            .collect::<Vec<_>>(),
    )?))
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        !args.output_dir.exists(),
        "Use a new immutable probe directory"
    );
    ensure!(
        args.source_commit.len() == 40 && args.source_commit.bytes().all(|x| x.is_ascii_hexdigit()),
        "Expected full source commit"
    );
    let input = fs::read(&args.features)?;
    let mut counts = BTreeMap::<String, usize>::new();
    for line in input.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        let row: Value = serde_json::from_slice(line)?;
        let split = row["split"].as_str().context("Missing feature split")?;
        ensure!(
            ["train", "development", "calibration"].contains(&split),
            "Probe refuses Test/unassigned features"
        );
        ensure!(
            row["evidence"] != "synthetic_fixture",
            "Synthetic fixtures cannot enter this probe"
        );
        *counts.entry(split.into()).or_default() += 1;
    }
    ensure!(
        counts.len() == 3,
        "All three fitting partitions are required"
    );
    let bindings = vec![
        (args.features.clone(), sha256(&input)),
        (args.detector_cli.clone(), digest(&args.detector_cli)?),
        (args.protocol.clone(), digest(&args.protocol)?),
        (std::env::current_exe()?, digest(&std::env::current_exe()?)?),
    ];
    let verify = || -> Result<()> {
        for (path, expected) in &bindings {
            ensure!(
                digest(path)? == *expected,
                "Frozen input changed: {}",
                path.display()
            );
        }
        Ok(())
    };
    fs::create_dir_all(args.output_dir.join("logs"))?;
    fs::create_dir(args.output_dir.join("runs"))?;
    write(
        &args.output_dir.join("plan.json"),
        &json!({
            "schema":"slop_ninja_linear_style_probe_v1", "source_commit":args.source_commit,
            "feature_sha256":bindings[0].1, "detector_executable_sha256":bindings[1].1,
            "protocol_sha256":bindings[2].1, "runner_executable_sha256":bindings[3].1,
            "row_counts":counts, "learning_rates":[0.0003,0.001], "modes":["word","grammar","combined"],
            "test_predictions_opened":false, "selection":"positive development recall in all classes and trained checkpoint, then development log loss; unrestricted winner also reported"
        }),
    )?;
    let mut results = BTreeMap::new();
    let mut spaces = BTreeMap::new();
    for rate in ["0.0003", "0.001"] {
        for mode in ["word", "grammar", "combined"] {
            verify()?;
            let name = format!("{mode}-{rate}");
            let output = args.output_dir.join("runs").join(&name);
            let log = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(args.output_dir.join("logs").join(format!("{name}.log")))?;
            let mut command = Command::new(&args.detector_cli);
            command
                .args(["train", "--features"])
                .arg(&args.features)
                .args(["--mode", mode, "--output-dir"])
                .arg(&output)
                .args([
                    "--learning-rate",
                    rate,
                    "--epochs",
                    "300",
                    "--patience",
                    "25",
                    "--l2",
                    "0.01",
                    "--min-document-frequency",
                    "2",
                    "--max-coordinates",
                    if mode == "combined" { "16384" } else { "8192" },
                ]);
            if mode == "combined" {
                command.args(["--word-coordinate-cap", "8192"]);
            }
            eprintln!("Fitting {name}");
            let status = command
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log))
                .status()?;
            ensure!(
                status.success(),
                "Candidate {name} failed; inspect its retained log"
            );
            verify()?;
            let artifact: DetectorArtifact =
                serde_json::from_slice(&fs::read(output.join("model.json"))?)?;
            artifact.validate()?;
            let report: TrainingReport =
                serde_json::from_slice(&fs::read(output.join("training_report.json"))?)?;
            ensure!(
                report.artifact_sha256 == artifact.content_sha256,
                "Report/model identity mismatch"
            );
            ensure!(
                report.epoch_history.len() == report.epochs_run + 1,
                "Incomplete epoch trace"
            );
            let coordinates = &artifact.content.feature_space.coordinates;
            let word = subset_hash(coordinates, true)?;
            let grammar = subset_hash(coordinates, false)?;
            if mode == "combined" {
                ensure!(
                    spaces.get("word") == Some(&word) && spaces.get("grammar") == Some(&grammar),
                    "Combined model changed the standalone coordinate spaces"
                );
            } else {
                let hash = if mode == "word" { &word } else { &grammar };
                if let Some(previous) = spaces.insert(mode, hash.clone()) {
                    ensure!(
                        previous == *hash,
                        "Coordinates changed across learning rates"
                    );
                }
            }
            let eligible = report.best_epoch > 0
                && report
                    .development_uncalibrated
                    .classes
                    .iter()
                    .all(|c| c.recall.is_some_and(|r| r > 0.0));
            eprintln!(
                "{name}: epoch {}, development loss {:.6}, eligible {eligible}",
                report.best_epoch, report.development_uncalibrated.log_loss
            );
            results.insert(
                name,
                json!({"artifact_id":artifact.content_sha256, "best_epoch":report.best_epoch,
                "epochs_run":report.epochs_run,"dimensions":report.dimensions,"eligible":eligible,
                "word_coordinates_sha256":word,"grammar_coordinates_sha256":grammar,
                "development":report.development_uncalibrated,
                "model_file_sha256":digest(&output.join("model.json"))?,
                "training_report_sha256":digest(&output.join("training_report.json"))?,
                "runtime_sha256":digest(&output.join("runtime.json"))?}),
            );
        }
    }
    let best = |eligible_only: bool| {
        results
            .iter()
            .filter(|(_, v)| !eligible_only || v["eligible"] == true)
            .min_by(|a, b| {
                a.1["development"]["log_loss"]
                    .as_f64()
                    .unwrap()
                    .total_cmp(&b.1["development"]["log_loss"].as_f64().unwrap())
            })
            .map(|(name, _)| name.clone())
    };
    let result = json!({"schema":"slop_ninja_linear_style_probe_results_v1","status":"complete",
        "source_commit":args.source_commit,"plan_sha256":digest(&args.output_dir.join("plan.json"))?,
        "selected_eligible_candidate":best(true),"unrestricted_loss_winner":best(false),
        "test_predictions_opened":false,"calibration_used_for_selection":false,"candidates":results});
    verify()?;
    write(&args.output_dir.join("results.json"), &result)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
