//! Complete the frozen v6 evaluation after its original frontier closes.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PLAN_SHA: &str = "c6b5b1d2db43b74aafe175c6f509e02992d0a5e2caeac2b553380b35b802d424";
const V5_ID: &str = "da3a604d7bd1708f5ba1c4a4a4be946d77670492b702e41e267fb86237dfb8c9";

#[derive(Parser)]
struct Args {
    #[arg(long)]
    plan: PathBuf,
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    producer_pid: u32,
    /// Exact output of ps -p PID -o lstart=,command=, captured under LC_ALL=C.
    #[arg(long)]
    producer_identity: PathBuf,
    #[arg(long)]
    control_dir: PathBuf,
    /// Check static inputs and the live producer without waiting or inference.
    #[arg(long)]
    check_only: bool,
    /// Short host name (as printed by `hostname -s`) of the machine that holds
    /// the frozen archive and the producer. Falls back to the
    /// REVISION_HANDOFF_HOST environment variable; with neither set the
    /// handoff refuses to run.
    #[arg(long)]
    required_host: Option<String>,
}

const REQUIRED_HOST_ENV: &str = "REVISION_HANDOFF_HOST";

/// The configured execution host, from `--required-host` or the environment.
fn required_host(args: &Args) -> Result<String> {
    let host = match &args.required_host {
        Some(host) => host.clone(),
        None => std::env::var(REQUIRED_HOST_ENV).with_context(|| {
            format!("Set --required-host or {REQUIRED_HOST_ENV} to the execution host's short name")
        })?,
    };
    let host = host.trim().to_owned();
    ensure!(!host.is_empty(), "The required execution host is empty");
    Ok(host)
}

fn object(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("Missing {key}"))
}
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn save(path: &Path, value: &Value) -> Result<()> {
    let mut file = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
fn state(dir: &Path, stage: &str, opened: bool) -> Result<()> {
    let temporary = dir.join("status.new");
    save(
        &temporary,
        &json!({"at_unix":now()?,"stage":stage,"pid":std::process::id(),"test_predictions_opened":opened}),
    )?;
    fs::rename(temporary, dir.join("status.json"))?;
    Ok(())
}
fn child(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.envs([
        ("LC_ALL", "C"),
        ("HF_HUB_OFFLINE", "1"),
        ("TRANSFORMERS_OFFLINE", "1"),
        ("PYTHONDONTWRITEBYTECODE", "1"),
        ("TOKENIZERS_PARALLELISM", "false"),
        ("OMP_NUM_THREADS", "1"),
        ("MKL_NUM_THREADS", "1"),
    ]);
    c
}
fn producer_identity(pid: u32) -> Result<Option<String>> {
    let output = child("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "lstart=,command="])
        .output()?;
    if output.status.success() {
        return Ok(Some(String::from_utf8(output.stdout)?.trim().to_owned()));
    }
    ensure!(
        output.status.code() == Some(1) && output.stdout.is_empty(),
        "Producer observation failed"
    );
    Ok(None)
}
fn verify_map(base: &Path, value: &Value, bindings: &mut BTreeMap<PathBuf, String>) -> Result<()> {
    for (relative, expected) in value.as_object().context("Expected hash map")? {
        let relative = Path::new(relative);
        ensure!(
            relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
            "Unsafe relative binding path"
        );
        let path = base.join(relative);
        let hash = sha256(fs::read(&path).with_context(|| format!("Read {}", path.display()))?);
        ensure!(
            Some(hash.as_str()) == expected.as_str(),
            "Hash mismatch: {}",
            path.display()
        );
        bindings.insert(path, hash);
    }
    Ok(())
}
fn bind(path: &Path, bindings: &mut BTreeMap<PathBuf, String>) -> Result<()> {
    let hash = sha256(fs::read(path)?);
    if let Some(prior) = bindings.insert(path.to_owned(), hash.clone()) {
        ensure!(
            prior == hash,
            "Previously bound file changed: {}",
            path.display()
        );
    }
    Ok(())
}
fn verify_list(base: &Path, list: &Path, log: &Path) -> Result<()> {
    let output = child("/usr/bin/shasum")
        .current_dir(base)
        .args(["-a", "256", "-c"])
        .arg(list)
        .output()?;
    let mut bytes = output.stdout;
    bytes.extend(output.stderr);
    fs::File::create_new(log)?.write_all(&bytes)?;
    ensure!(
        output.status.success(),
        "Frozen hash-list check failed; inspect {}",
        log.display()
    );
    Ok(())
}
fn best_epoch(history: &Value) -> Result<Option<(f64, u64)>> {
    let epochs = history.as_array().context("Missing epoch history")?;
    ensure!(epochs.len() == 4, "Incomplete epoch history");
    let mut best = None;
    for (index, epoch) in epochs.iter().enumerate() {
        ensure!(
            epoch["epoch"] == index + 1 && epoch["selection_objective"] == "human_model",
            "Epoch order/objective differs"
        );
        let metrics = &epoch["development_human_model"];
        let loss = metrics["log_loss"]
            .as_f64()
            .context("Missing Development loss")?;
        let recall = metrics["recall_at_half"]
            .as_array()
            .context("Missing recall")?;
        ensure!(
            loss.is_finite() && loss >= 0.0 && recall.len() == 2,
            "Invalid selection metrics"
        );
        let recall: Vec<f64> = recall
            .iter()
            .map(|v| v.as_f64().context("Invalid recall"))
            .collect::<Result<_>>()?;
        ensure!(
            recall
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            "Invalid recall range"
        );
        let eligible = recall.iter().all(|v| *v > 0.0);
        ensure!(
            epoch["selection_eligible"] == eligible && epoch["selection_loss"] == loss,
            "Selection summary differs from objective"
        );
        if eligible && best.is_none_or(|(previous, _)| loss < previous) {
            best = Some((loss, (index + 1) as u64));
        }
    }
    Ok(best)
}
fn resolve_args(values: &Value, candidate: &Path) -> Result<Vec<String>> {
    values
        .as_array()
        .context("Missing argv")?
        .iter()
        .map(|v| {
            if let Some(s) = v.as_str() {
                return Ok(s.to_owned());
            }
            ensure!(
                v == &json!({"binding":"selected_candidate_artifact"}),
                "Unknown command binding"
            );
            Ok(candidate.to_string_lossy().into_owned())
        })
        .collect()
}
fn selected_artifact(root: &Path, bindings: &mut BTreeMap<PathBuf, String>) -> Result<PathBuf> {
    let frontier = root.join("encoder-frontier-v6");
    let selection_path = frontier.join("selection.json");
    let selection = object(&selection_path)?;
    ensure!(
        selection["schema"] == "slop_ninja_encoder_frontier_selection_v1"
            && selection["final_test_opened"] == false
            && selection["calibration_used_to_select_candidate"] == false,
        "Unexpected selection contract"
    );
    ensure!(
        selection["protocol_sha256"] == sha256(fs::read(frontier.join("protocol.json"))?),
        "Selection protocol differs"
    );
    let protocol = object(&frontier.join("protocol.json"))?;
    let candidates = selection["candidates"]
        .as_array()
        .context("Missing candidates")?;
    ensure!(candidates.len() == 2, "Incomplete frontier");
    let mut best: Option<(f64, u64, f64, PathBuf, String)> = None;
    for (candidate, rate) in candidates.iter().zip([0.0000075, 0.00001]) {
        let name = format!("lr-{rate}");
        ensure!(
            candidate["learning_rate"] == rate && candidate["name"] == name,
            "Unexpected rate/order"
        );
        let artifact = frontier.join(&name);
        if candidate["eligible"] == false {
            let path = frontier.join(format!("{name}-no-eligible-epoch.json"));
            let diagnostic = object(&path)?;
            ensure!(
                !artifact.exists()
                    && candidate["diagnostic_sha256"] == sha256(fs::read(&path)?)
                    && diagnostic["status"] == "no_eligible_epoch"
                    && diagnostic["final_test_opened"] == false
                    && best_epoch(&diagnostic["epoch_history"])?.is_none(),
                "Invalid ineligible candidate"
            );
            bind(&path, bindings)?;
            continue;
        }
        ensure!(
            candidate["eligible"] == true,
            "Missing candidate eligibility"
        );
        let training = object(&artifact.join("training.json"))?;
        let manifest = object(&artifact.join("manifest.json"))?;
        ensure!(
            training["final_test_opened"] == false
                && training["requires_nonzero_development_binary_recall"] == true
                && training["selection_objective"] == "human_model"
                && training["learning_rate"] == rate
                && training["epochs_requested"] == 4
                && manifest["max_tokens"] == 2048,
            "Candidate recipe differs"
        );
        for split in ["train", "development", "calibration"] {
            ensure!(
                training["partition_sha256"][split] == protocol["partition_sha256"][split],
                "Candidate partition differs"
            );
        }
        ensure!(
            training["evaluation_views"] == protocol["evaluation_views"]
                && training["checkpoint_pin_sha256"] == protocol["checkpoint_pin_sha256"]
                && training["data_rights_sha256"] == protocol["data_rights_sha256"],
            "Candidate source/view binding differs"
        );
        let (loss, epoch) =
            best_epoch(&training["epoch_history"])?.context("No eligible epoch in candidate")?;
        ensure!(
            training["selected_epoch"] == epoch
                && candidate["selected_epoch"] == epoch
                && candidate["development_selection_metrics"]["log_loss"] == loss
                && candidate["artifact_id"] == manifest["artifact_id"]
                && candidate["training_sha256"]
                    == sha256(fs::read(artifact.join("training.json"))?),
            "Selected epoch binding differs"
        );
        verify_map(&artifact, &manifest["files"], bindings)?;
        bind(&artifact.join("manifest.json"), bindings)?;
        let id = field(&manifest, "artifact_id")?.to_owned();
        let key = (loss, epoch, rate);
        if best.as_ref().is_none_or(|b| key < (b.0, b.1, b.2)) {
            best = Some((loss, epoch, rate, artifact, id));
        }
    }
    let (loss, _, rate, path, id) = best.context("No eligible frontier candidate")?;
    ensure!(
        selection["selected"]["path"] == path.to_string_lossy().as_ref()
            && selection["selected"]["artifact_id"] == id
            && selection["selected"]["learning_rate"] == rate
            && selection["selected"]["development_uncalibrated_log_loss"] == loss
            && selection["selected"]["selection_objective"] == "human_model",
        "Frontier winner differs from frozen selection/tie rules"
    );
    bind(&selection_path, bindings)?;
    Ok(path)
}

fn run(args: &Args) -> Result<()> {
    let host = required_host(args)?;
    let hostname = child("/bin/hostname").arg("-s").output()?;
    ensure!(
        hostname.status.success()
            && String::from_utf8(hostname.stdout)?
                .trim()
                .eq_ignore_ascii_case(&host),
        "Run only on the configured execution host"
    );
    let plan_bytes = fs::read(&args.plan)?;
    ensure!(sha256(&plan_bytes) == PLAN_SHA, "Command plan differs");
    let plan: Value = serde_json::from_slice(&plan_bytes)?;
    let root = Path::new(field(&plan, "run_root")?);
    let output = Path::new(field(&plan, "output_root")?);
    ensure!(
        !output.exists(),
        "Evaluation already attempted; inspect existing outputs"
    );
    let identity = fs::read_to_string(&args.producer_identity)?;
    let mut bindings = BTreeMap::new();
    verify_map(root, &plan["static_sha256"], &mut bindings)?;
    for route in ["primary", "phi4"] {
        let directory = root.join(format!("{route}-coverage-v1"));
        let coverage = object(&directory.join("coverage.json"))?;
        verify_map(&directory, &coverage["files_sha256"], &mut bindings)?;
        ensure!(
            coverage["excluded_family_count"] == 0 && coverage["excluded_record_count"] == 0,
            "Coverage exclusions differ"
        );
    }
    bind(&args.plan, &mut bindings)?;
    bind(&args.producer_identity, &mut bindings)?;
    bind(&std::env::current_exe()?, &mut bindings)?;
    save(
        &args.control_dir.join("launch-bindings.json"),
        &json!({"bindings":bindings,"source_sha256":sha256(include_bytes!("revision_handoff.rs")),"producer_pid":args.producer_pid,"producer_identity":identity.trim(),"plan_sha256":PLAN_SHA,"at_unix":now()?}),
    )?;
    if args.check_only {
        ensure!(
            producer_identity(args.producer_pid)?.as_deref() == Some(identity.trim()),
            "Live producer identity differs"
        );
        state(
            &args.control_dir,
            "static_preflight_passed_no_inference",
            false,
        )?;
        return Ok(());
    }
    let completion_path = root.join("fitting-handoff-v2/completion.json");
    loop {
        if completion_path.exists() {
            break;
        }
        ensure!(
            producer_identity(args.producer_pid)?.as_deref() == Some(identity.trim()),
            "Producer absent or changed before its completion receipt; inspect without retrying"
        );
        state(&args.control_dir, "waiting_for_original_frontier", false)?;
        thread::sleep(Duration::from_secs(30));
    }
    // The producer writes its receipt from an EXIT trap. Let that write and
    // exit finish before parsing it; observing a file name alone is insufficient.
    while producer_identity(args.producer_pid)?.as_deref() == Some(identity.trim()) {
        state(&args.control_dir, "waiting_for_producer_exit", false)?;
        thread::sleep(Duration::from_secs(1));
    }
    let completion = object(&completion_path)?;
    ensure!(
        completion["exit_code"] == 0
            && completion["stage"] == "frontier_closed_inspect_selection_before_test"
            && completion["test_predictions_opened"] == false,
        "Frontier did not close successfully"
    );
    state(&args.control_dir, "validating_closed_frontier", false)?;
    verify_map(root, &plan["static_sha256"], &mut bindings)?;
    let repo = root.join("repo");
    verify_list(
        &repo,
        &repo.join("data/baseline-detector/revision-expansion-v6/fitting-tools-v1/files.sha256"),
        &args.control_dir.join("tools.log"),
    )?;
    verify_list(
        root,
        &root.join("fitting-handoff-v2/pre-fit-bindings.sha256"),
        &args.control_dir.join("fit-bindings.log"),
    )?;
    bind(&completion_path, &mut bindings)?;
    let candidate = selected_artifact(root, &mut bindings)?;
    let v5 = repo.join(
        "data/baseline-detector/narrative-expansion-v5/encoder-frontier-v5-recovered/lr-0.0000075",
    );
    let v5_manifest = object(&v5.join("manifest.json"))?;
    ensure!(
        v5_manifest["artifact_id"] == V5_ID && v5_manifest["max_tokens"] == 1024,
        "V5 reference differs"
    );
    verify_map(&v5, &v5_manifest["files"], &mut bindings)?;
    bind(
        &root.join("assembly-execution-companion-v1.json"),
        &mut bindings,
    )?;
    let commands = plan["commands"].as_array().context("Missing commands")?;
    fs::create_dir(output)?;
    let mut opened = false;
    for (index, command) in commands.iter().enumerate() {
        if index == 1 {
            let thresholds_path = output.join("v6-thresholds/thresholds.json");
            let thresholds = object(&thresholds_path)?;
            let training = object(&candidate.join("training.json"))?;
            let manifest = object(&candidate.join("manifest.json"))?;
            let content = &thresholds["content"];
            ensure!(
                thresholds["content_sha256"] == sha256(serde_json::to_vec(content)?)
                    && content["artifact_id"] == manifest["artifact_id"]
                    && content["calibration_shard_sha256"]
                        == training["partition_sha256"]["calibration"]
                    && content["calibration_view"] == training["evaluation_views"]["calibration"]
                    && content["temperature_refitted"] == false
                    && content["test_predictions_opened"] == false,
                "Threshold freeze differs"
            );
            bind(&thresholds_path, &mut bindings)?;
            for (path, hash) in &bindings {
                ensure!(
                    sha256(fs::read(path)?) == *hash,
                    "Pre-Test file changed: {}",
                    path.display()
                );
            }
            let checksum_path = output.join("pre-test-bindings.sha256");
            let mut checksums = fs::File::create_new(&checksum_path)?;
            for (path, hash) in &bindings {
                let path = path.to_str().context("Non-UTF-8 checksum path")?;
                ensure!(
                    !path.contains(['\n', '\r', '\\']),
                    "Unsupported checksum path"
                );
                writeln!(checksums, "{hash}  {path}")?;
            }
            checksums.sync_all()?;
            save(
                &output.join("pre-test-bindings.json"),
                &json!({"schema":"slop_ninja_v6_final_test_bindings_v1","at_unix":now()?,"bindings":bindings,"candidate_artifact":candidate,"v5_artifact":v5,"plan_sha256":PLAN_SHA,"test_predictions_opened":false}),
            )?;
            save(
                &output.join("opening-receipt.json"),
                &json!({"at_unix":now()?,"pre_test_bindings_sha256":sha256(fs::read(output.join("pre-test-bindings.json"))?),"checksum_file_sha256":sha256(fs::read(checksum_path)?),"first_command":command["id"],"policy":"All commands retain the frozen selected candidate; no Test-based refit, candidate switch or automatic retry."}),
            )?;
            opened = true;
        }
        let id = field(command, "id")?;
        state(&args.control_dir, id, opened)?;
        let argv = resolve_args(&command["args"], &candidate)?;
        save(
            &args.control_dir.join(format!("{index:02}-invocation.json")),
            &json!({"at_unix":now()?,"program":command["program"],"args":argv}),
        )?;
        let status = child(field(command, "program")?)
            .args(&argv)
            .stdout(fs::File::create_new(
                args.control_dir.join(format!("{index:02}-stdout.log")),
            )?)
            .stderr(fs::File::create_new(
                args.control_dir.join(format!("{index:02}-stderr.log")),
            )?)
            .status()?;
        save(
            &args.control_dir.join(format!("{index:02}-completion.json")),
            &json!({"at_unix":now()?,"exit_code":status.code(),"command":id}),
        )?;
        ensure!(
            status.success(),
            "Command failed: {id}; partial outputs retained, no retry"
        );
    }
    for (path, hash) in &bindings {
        ensure!(
            sha256(fs::read(path)?) == *hash,
            "Post-Test file changed: {}",
            path.display()
        );
    }
    state(
        &args.control_dir,
        "evaluation_complete_results_require_review",
        opened,
    )?;
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    fs::create_dir(&args.control_dir).context("Use a new control directory")?;
    let result = run(&args);
    save(
        &args.control_dir.join("completion.json"),
        &json!({"at_unix":now()?,"success":result.is_ok(),"error":result.as_ref().err().map(|e|format!("{e:#}")),"check_only":args.check_only}),
    )?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn history() -> Value {
        json!((1..=4).map(|epoch|json!({"epoch":epoch,"selection_objective":"human_model","selection_loss":0.4,"selection_eligible":true,"development_human_model":{"log_loss":0.4,"recall_at_half":[0.5,0.5]}})).collect::<Vec<_>>())
    }
    #[test]
    fn epoch_ties_and_ineligible_epochs_preserve_frozen_selection() {
        let mut h = history();
        assert_eq!(best_epoch(&h).unwrap(), Some((0.4, 1)));
        h[0]["selection_eligible"] = json!(false);
        h[0]["development_human_model"]["recall_at_half"] = json!([0.0, 1.0]);
        assert_eq!(best_epoch(&h).unwrap(), Some((0.4, 2)));
        h[0]["selection_eligible"] = json!(true);
        assert!(best_epoch(&h).is_err());
        assert!(best_epoch(&json!([])).is_err());
    }
    #[test]
    fn only_the_declared_candidate_binding_is_resolved() {
        let path = Path::new("/candidate with spaces");
        let args = resolve_args(
            &json!(["literal $(text)",{"binding":"selected_candidate_artifact"}]),
            path,
        )
        .unwrap();
        assert_eq!(args, ["literal $(text)", "/candidate with spaces"]);
        assert!(resolve_args(&json!([{"binding":"another_candidate"}]), path).is_err());
    }
    #[test]
    fn execution_host_comes_from_configuration() {
        let parse = |host: &str| {
            Args::try_parse_from([
                "revision_handoff",
                "--plan",
                "commands.json",
                "--producer-pid",
                "1",
                "--producer-identity",
                "producer-identity.txt",
                "--control-dir",
                "control",
                "--required-host",
                host,
            ])
            .unwrap()
        };
        assert_eq!(required_host(&parse(" eval-host ")).unwrap(), "eval-host");
        assert!(required_host(&parse("  ")).is_err());
    }
}
