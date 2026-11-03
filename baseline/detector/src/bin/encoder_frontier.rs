//! Execute a frozen encoder frontier and select using Development alone.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{Split, sha256},
    evaluation_view,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    python: PathBuf,
    #[arg(long)]
    runner_dir: PathBuf,
    #[arg(long)]
    checkpoint: PathBuf,
    #[arg(long)]
    shards: PathBuf,
    /// Frozen Development observations over the complete ancestry shard.
    #[arg(long, requires = "calibration_view")]
    development_view: Option<PathBuf>,
    /// Frozen Calibration observations; both view flags must be declared together.
    #[arg(long, requires = "development_view")]
    calibration_view: Option<PathBuf>,
    #[arg(long)]
    whitespace_train: PathBuf,
    #[arg(long)]
    token_audit: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
    #[arg(long, default_value = "cpu", value_parser = ["cpu", "mps", "cuda"])]
    device: String,
    /// Freeze all rates before fitting. Lower rates win exact loss ties.
    #[arg(long, value_delimiter = ',', default_value = "0.00001,0.00002")]
    learning_rates: Vec<f64>,
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..=20))]
    epochs: u32,
    #[arg(long, default_value_t = 1024, value_parser = clap::value_parser!(u32).range(1..=8192))]
    max_tokens: u32,
    #[arg(long, default_value = "three_class", value_parser = ["three_class", "human_model"])]
    selection_objective: String,
}

fn freeze_evaluation_views(
    args: &Args,
    hashes: &serde_json::Map<String, Value>,
) -> Result<Option<Value>> {
    match (&args.development_view, &args.calibration_view) {
        (None, None) => Ok(None),
        (Some(development), Some(calibration)) => {
            let mut bindings = serde_json::Map::new();
            for (name, split, path) in [
                ("development", Split::Development, development),
                ("calibration", Split::Calibration, calibration),
            ] {
                let loaded = evaluation_view::load_view(
                    path,
                    &args.shards.join(format!("{name}.jsonl")),
                    split,
                )?;
                ensure!(
                    hashes[name] == loaded.view.records_sha256,
                    "Evaluation view archive differs from the audited shard"
                );
                bindings.insert(name.into(), serde_json::to_value(loaded.binding())?);
            }
            Ok(Some(Value::Object(bindings)))
        }
        _ => anyhow::bail!("Development and Calibration views must be supplied together"),
    }
}

fn check_training_views(training: &Value, frozen: Option<&Value>) -> Result<()> {
    ensure!(
        training.get("evaluation_views") == frozen,
        "Training evaluation views differ from the frozen frontier"
    );
    Ok(())
}

fn checkpoint_catalog_hash(runner: &Path) -> Result<Option<String>> {
    match fs::read(runner.join("checkpoints.py")) {
        Ok(bytes) => Ok(Some(sha256(bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn check_checkpoint_catalog(runner: &Path, frozen: &Option<String>) -> Result<()> {
    ensure!(
        checkpoint_catalog_hash(runner)? == *frozen,
        "Checkpoint catalog changed after the frontier was frozen"
    );
    Ok(())
}

fn append_training_partitions(command: &mut Command, args: &Args) {
    // Test remains an audit-only input, never a trainer argument.
    for split in ["train", "development", "calibration"] {
        command
            .arg(format!("--{split}-jsonl"))
            .arg(args.shards.join(format!("{split}.jsonl")));
    }
    if let (Some(development), Some(calibration)) = (&args.development_view, &args.calibration_view)
    {
        command
            .arg("--development-view")
            .arg(development)
            .arg("--calibration-view")
            .arg(calibration);
    }
}

fn main() -> Result<()> {
    let mut args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new frontier directory");
    ensure!(
        (1..=8).contains(&args.learning_rates.len())
            && args
                .learning_rates
                .iter()
                .all(|rate| rate.is_finite() && *rate > 0.0 && *rate <= 0.001),
        "Declare 1..8 finite learning rates in (0, 0.001]"
    );
    args.learning_rates.sort_by(f64::total_cmp);
    ensure!(
        args.learning_rates
            .windows(2)
            .all(|pair| pair[0] != pair[1]),
        "Duplicate learning rate"
    );
    let audit: Value = serde_json::from_slice(&fs::read(&args.token_audit)?)?;
    ensure!(
        audit["fits_configured_token_limit"] == true
            && audit["max_tokens"] == args.max_tokens
            && audit["model_executed"] == false
            && audit["records_dropped"] == 0,
        "Require a passing audit at the declared token limit without filtering or predictions"
    );
    let mut hashes = serde_json::Map::new();
    for split in ["train", "development", "calibration", "test"] {
        let hash = sha256(fs::read(args.shards.join(format!("{split}.jsonl")))?);
        ensure!(
            audit["shards"][split]["sha256"] == hash,
            "Audit shard mismatch"
        );
        hashes.insert(split.into(), json!(hash));
    }
    let checkpoint_hash = sha256(fs::read(args.checkpoint.join("checkpoint.json"))?);
    ensure!(
        audit["checkpoint_pin_sha256"] == checkpoint_hash,
        "Checkpoint audit mismatch"
    );
    let evaluation_views = freeze_evaluation_views(&args, &hashes)?;
    let checkpoint_catalog = checkpoint_catalog_hash(&args.runner_dir)?;
    let train_path = args.runner_dir.join("train.py");
    let mut protocol = json!({
        "schema":"slop_ninja_encoder_frontier_v1",
        "learning_rates":args.learning_rates,"epochs":args.epochs,"batch_size":4,"seed":17,
        "max_tokens":args.max_tokens,
        "sample_weighting":"source_origin","class_weights":"none",
        "selection_objective":args.selection_objective,
        "selection":"positive Development recall for each class in the selected objective, then minimum uncalibrated Development objective log loss; earlier epoch and lower learning rate break exact ties",
        "partition_sha256":hashes,"checkpoint_pin_sha256":checkpoint_hash,
        "whitespace_train_sha256":sha256(fs::read(&args.whitespace_train)?),
        "data_rights_sha256":sha256(fs::read(args.shards.join("data_rights.json"))?),
        "train_py_sha256":sha256(fs::read(&train_path)?),
        "common_py_sha256":sha256(fs::read(args.runner_dir.join("common.py"))?),
        "checkpoint_catalog_sha256":checkpoint_catalog,
        "token_audit_sha256":sha256(fs::read(&args.token_audit)?),
        "python":args.python,"device":args.device,"final_test_opened":false,
        "test_access":"count/length/hash audit only; no Test argument is passed to the trainer"
    });
    if let Some(views) = &evaluation_views {
        protocol["evaluation_views"] = views.clone();
    }
    fs::create_dir_all(&args.output_dir)?;
    fs::write(
        args.output_dir.join("protocol.json"),
        serde_json::to_vec_pretty(&protocol)?,
    )?;
    let mut candidates = Vec::new();
    let mut selected: Option<(f64, f64, PathBuf, String)> = None;
    for &rate in &args.learning_rates {
        check_checkpoint_catalog(&args.runner_dir, &checkpoint_catalog)?;
        if let Some(views) = &evaluation_views {
            for (name, path) in [
                ("development", &args.development_view),
                ("calibration", &args.calibration_view),
            ] {
                ensure!(
                    sha256(fs::read(
                        path.as_ref().context("Missing frozen view path")?
                    )?) == views[name]["sha256"]
                        && sha256(fs::read(args.shards.join(format!("{name}.jsonl")))?)
                            == views[name]["records_sha256"],
                    "Evaluation view or archive changed before fitting"
                );
            }
        }
        let name = format!("lr-{rate}");
        let output = args.output_dir.join(&name);
        let log_path = args.output_dir.join(format!("{name}.log"));
        let log = fs::File::create_new(&log_path)?;
        let mut command = Command::new(&args.python);
        command
            .arg(&train_path)
            .arg("--checkpoint")
            .arg(&args.checkpoint)
            .arg("--data-rights")
            .arg(args.shards.join("data_rights.json"))
            .arg("--training-whitespace-view")
            .arg(&args.whitespace_train)
            .arg("--output")
            .arg(&output)
            .arg("--device")
            .arg(&args.device)
            .arg("--max-tokens")
            .arg(args.max_tokens.to_string())
            .args([
                "--batch-size",
                "4",
                "--seed",
                "17",
                "--weight-decay",
                "0.01",
                "--class-weights",
                "none",
                "--sample-weighting",
                "source_origin",
                "--require-class-coverage",
                "--learning-rate",
            ])
            .arg(rate.to_string())
            .arg("--epochs")
            .arg(args.epochs.to_string())
            .env("HF_HUB_OFFLINE", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .env("PYTHONUNBUFFERED", "1");
        // The original frozen v4 runner predates this optional argument.
        if args.selection_objective != "three_class" {
            command
                .arg("--selection-objective")
                .arg(&args.selection_objective);
        }
        append_training_partitions(&mut command, &args);
        fs::write(
            args.output_dir.join(format!("{name}-invocation.json")),
            serde_json::to_vec_pretty(&json!({
                "program":command.get_program().to_string_lossy(),
                "args":command.get_args().map(|a| a.to_string_lossy()).collect::<Vec<_>>()
            }))?,
        )?;
        println!("Fitting {name}; log: {}", log_path.display());
        let status = command.stdout(log.try_clone()?).stderr(log).status()?;
        let diagnostic = args
            .output_dir
            .join(format!("{name}-no-eligible-epoch.json"));
        if !status.success() {
            ensure!(
                diagnostic.exists() && !output.exists(),
                "Training failed; inspect {}",
                log_path.display()
            );
            let failed: Value = serde_json::from_slice(&fs::read(&diagnostic)?)?;
            ensure!(
                failed["status"] == "no_eligible_epoch",
                "Unexpected failure diagnostic"
            );
            candidates.push(json!({"name":name,"learning_rate":rate,"eligible":false,
                "diagnostic_sha256":sha256(fs::read(diagnostic)?)}));
            continue;
        }
        let training: Value = serde_json::from_slice(&fs::read(output.join("training.json"))?)?;
        let manifest: Value = serde_json::from_slice(&fs::read(output.join("manifest.json"))?)?;
        ensure!(
            manifest["max_tokens"] == args.max_tokens,
            "Artifact token limit differs from the frozen frontier"
        );
        check_training_views(&training, evaluation_views.as_ref())?;
        if evaluation_views.is_some() {
            ensure!(
                manifest["files"]["training.json"]
                    == sha256(fs::read(output.join("training.json"))?),
                "Artifact training metadata is not bound by its manifest"
            );
        }
        let coverage_key = if args.selection_objective == "human_model" {
            "requires_nonzero_development_binary_recall"
        } else {
            "requires_nonzero_development_class_recall"
        };
        ensure!(
            training["final_test_opened"] == false
                && training[coverage_key] == true
                && training["selection_objective"]
                    .as_str()
                    .unwrap_or("three_class")
                    == args.selection_objective,
            "Unexpected training selection contract"
        );
        for split in ["train", "development", "calibration"] {
            ensure!(
                training["partition_sha256"][split] == protocol["partition_sha256"][split],
                "Training corpus changed after the frontier was frozen"
            );
        }
        ensure!(
            training["checkpoint_pin_sha256"] == protocol["checkpoint_pin_sha256"]
                && training["data_rights_sha256"] == protocol["data_rights_sha256"]
                && training["formatting_augmentation"]["alternate_train_sha256"]
                    == protocol["whitespace_train_sha256"]
                && manifest["files"]["runner/train.py"] == protocol["train_py_sha256"]
                && manifest["files"]["runner/common.py"] == protocol["common_py_sha256"]
                && manifest["files"]["runner/checkpoints.py"]
                    == protocol["checkpoint_catalog_sha256"],
            "Training inputs or runner changed after the frontier was frozen"
        );
        let epoch = training["epoch_history"]
            .as_array()
            .context("Missing epoch history")?
            .iter()
            .find(|e| e["epoch"] == training["selected_epoch"])
            .context("Missing selected epoch")?;
        ensure!(
            epoch["selection_eligible"] == true,
            "Selected epoch was ineligible"
        );
        let development_key = if args.selection_objective == "human_model" {
            "development_human_model"
        } else {
            "development"
        };
        let loss = epoch[development_key]["log_loss"]
            .as_f64()
            .context("Missing Development loss")?;
        ensure!(loss.is_finite(), "Non-finite Development loss");
        let artifact_id = manifest["artifact_id"]
            .as_str()
            .context("Missing artifact identity")?
            .to_owned();
        candidates.push(json!({"name":name,"learning_rate":rate,"eligible":true,
            "artifact_id":artifact_id,"selected_epoch":training["selected_epoch"],
            "selection_objective":args.selection_objective,
            "development_uncalibrated":epoch["development"],
            "development_selection_metrics":epoch[development_key],
            "training_sha256":sha256(fs::read(output.join("training.json"))?)}));
        if selected.as_ref().is_none_or(|(best, _, _, _)| loss < *best) {
            selected = Some((loss, rate, output, artifact_id));
        }
    }
    let winner = selected.map(|(loss, rate, path, id)| {
        json!({
            "path":path,"artifact_id":id,"selection_objective":args.selection_objective,
            "development_uncalibrated_log_loss":loss,"learning_rate":rate
        })
    });
    let report = json!({"schema":"slop_ninja_encoder_frontier_selection_v1",
        "protocol_sha256":sha256(fs::read(args.output_dir.join("protocol.json"))?),
        "candidates":candidates,"selected":winner,"final_test_opened":false,
        "calibration_used_to_select_candidate":false});
    fs::write(
        args.output_dir.join("selection.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_catalog_cannot_change_between_candidates() {
        let temporary = tempfile::tempdir().unwrap();
        let runner = temporary.path();
        let path = runner.join("checkpoints.py");
        let legacy = checkpoint_catalog_hash(runner).unwrap();
        assert_eq!(legacy, None);
        assert!(check_checkpoint_catalog(runner, &legacy).is_ok());
        fs::write(&path, b"declared model identities").unwrap();
        assert!(check_checkpoint_catalog(runner, &legacy).is_err());
        let frozen = checkpoint_catalog_hash(runner).unwrap();
        assert!(check_checkpoint_catalog(runner, &frozen).is_ok());
        fs::write(&path, b"different model identities").unwrap();
        assert!(check_checkpoint_catalog(runner, &frozen).is_err());
        fs::remove_file(path).unwrap();
        assert!(check_checkpoint_catalog(runner, &frozen).is_err());
    }

    fn arguments(extra: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(
            [
                "encoder_frontier",
                "--python",
                "fixture-python",
                "--runner-dir",
                "runner",
                "--checkpoint",
                "checkpoint",
                "--shards",
                "shards",
                "--whitespace-train",
                "whitespace.jsonl",
                "--token-audit",
                "audit.json",
                "--output-dir",
                "new-output",
            ]
            .into_iter()
            .chain(extra.iter().copied()),
        )
    }

    #[test]
    fn views_are_an_explicit_pair_and_no_test_argument_reaches_training() {
        assert!(arguments(&["--development-view", "dev.json"]).is_err());
        assert!(arguments(&["--calibration-view", "cal.json"]).is_err());
        for extra in [
            &[][..],
            &[
                "--development-view",
                "dev.json",
                "--calibration-view",
                "cal.json",
            ][..],
        ] {
            let args = arguments(extra).unwrap();
            let mut command = Command::new("never-executed");
            append_training_partitions(&mut command, &args);
            let values: Vec<_> = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                &values[..6],
                [
                    "--train-jsonl",
                    "shards/train.jsonl",
                    "--development-jsonl",
                    "shards/development.jsonl",
                    "--calibration-jsonl",
                    "shards/calibration.jsonl"
                ]
            );
            assert!(!values.iter().any(|value| value.contains("test")));
            assert_eq!(values.len(), 6 + extra.len());
            if !extra.is_empty() {
                assert_eq!(&values[6..], extra);
            }
        }
    }

    #[test]
    fn returned_artifact_must_keep_exact_frozen_view_bindings() {
        let frozen = json!({"development":{"sha256":sha256("development-view")},"calibration":{"sha256":sha256("calibration-view")}});
        assert!(check_training_views(&json!({}), None).is_ok());
        assert!(check_training_views(&json!({}), Some(&frozen)).is_err());
        let mut training = json!({"evaluation_views":frozen});
        assert!(check_training_views(&training, Some(&frozen)).is_ok());
        assert!(check_training_views(&training, None).is_err());
        training["evaluation_views"]["development"]["sha256"] = json!(sha256("changed"));
        assert!(check_training_views(&training, Some(&frozen)).is_err());
        assert!(check_training_views(&json!({"evaluation_views":null}), None).is_err());
    }

    #[test]
    fn token_limit_defaults_to_legacy_value_and_is_bounded() {
        assert_eq!(arguments(&[]).unwrap().max_tokens, 1024);
        assert_eq!(
            arguments(&["--max-tokens", "2048"]).unwrap().max_tokens,
            2048
        );
        assert!(arguments(&["--max-tokens", "0"]).is_err());
        assert!(arguments(&["--max-tokens", "8193"]).is_err());
    }
}
