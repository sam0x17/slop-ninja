//! Freeze and run the public diagnostics of the declared two-candidate ablation.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::sha256,
    metrics::{self, OperatingPoint, ProbabilityRow},
    model::CLASS_NAMES,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

const BASELINE: &str = "4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138";
const POLICY: &str = include_str!("../ORIGIN_COMPARISON_V1.md");
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    task: Task,
}
#[derive(Subcommand)]
enum Task {
    Prepare(Preparation),
    Run {
        #[arg(long)]
        plan: PathBuf,
    },
}
#[derive(clap::Args)]
struct Preparation {
    #[arg(long)]
    v6: PathBuf,
    #[arg(long)]
    fits: PathBuf,
    #[arg(long)]
    diagnostics: PathBuf,
    #[arg(long)]
    python: PathBuf,
    #[arg(long)]
    tools: PathBuf,
    #[arg(long)]
    out: PathBuf,
    /// Explicit upstream checkpoint for the predeclared capacity comparison.
    /// Defaults to the original V6 checkpoint; other inputs remain unchanged.
    #[arg(long)]
    checkpoint: Option<PathBuf>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    records: PathBuf,
    view: Option<PathBuf>,
    baseline: PathBuf,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema: String,
    policy_sha256: String,
    executable_sha256: String,
    fits: PathBuf,
    python: PathBuf,
    output: PathBuf,
    calibration: PathBuf,
    calibration_view: PathBuf,
    hosted: PathBuf,
    grammar: PathBuf,
    cases: Vec<Case>,
    training_inputs: BTreeMap<String, PathBuf>,
    bindings: BTreeMap<PathBuf, String>,
}
fn object(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn save(p: &Path, value: &impl Serialize) -> Result<()> {
    let mut f = fs::File::create_new(p)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn digest(p: &Path) -> Result<String> {
    Ok(sha256(fs::read(p)?))
}
fn bind(map: &mut BTreeMap<PathBuf, String>, p: &Path) -> Result<()> {
    map.insert(p.canonicalize()?, digest(p)?);
    Ok(())
}
fn check(bindings: &BTreeMap<PathBuf, String>) -> Result<()> {
    for (p, h) in bindings {
        ensure!(digest(p)? == *h, "Changed bound file: {}", p.display());
    }
    Ok(())
}
fn jsonl(p: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(p)?
        .lines()
        .map(|s| Ok(serde_json::from_str(s)?))
        .collect()
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}
fn responses(requests: &[Value], p: &Path, artifact: &str) -> Result<BTreeMap<String, [f64; 3]>> {
    let mut ids = BTreeSet::new();
    for r in requests {
        let id = string(r, "id")?;
        ensure!(
            id == sha256(string(r, "text")?) && ids.insert(id),
            "Changed or duplicate diagnostic request"
        );
    }
    let mut scores = BTreeMap::new();
    for r in jsonl(p)? {
        let id = string(&r, "id")?;
        ensure!(
            ids.contains(id)
                && r["artifact_id"] == artifact
                && r["status"] == "ok"
                && r["runtime"] == "cpu_float32_eager"
                && r["labels"] == json!(CLASS_NAMES)
                && r["token_count"]
                    .as_u64()
                    .is_some_and(|n| (1..=2048).contains(&n)),
            "Unmatched, failed or noncanonical response"
        );
        let probs: [f64; 3] = serde_json::from_value(r["probabilities"].clone())?;
        ensure!(
            probs
                .iter()
                .all(|p| p.is_finite() && (0.0..=1.0).contains(p))
                && (probs.iter().sum::<f64>() - 1.0).abs() < 1e-6
                && (r["model_or_assistance_probability"]
                    .as_f64()
                    .context("Missing combined score")?
                    - probs[1]
                    - probs[2])
                    .abs()
                    < 1e-6,
            "Invalid probability vector"
        );
        ensure!(
            scores.insert(id.to_owned(), probs).is_none(),
            "Repeated response"
        );
    }
    ensure!(scores.len() == ids.len(), "Incomplete response coverage");
    Ok(scores)
}
fn metadata(directory: &Path, kind: &str, requests: &[Value]) -> Result<Vec<Value>> {
    let rows: Vec<Value> = if kind == "hosted" {
        serde_json::from_slice(&fs::read(directory.join("prior-comparison.json"))?)?
    } else {
        serde_json::from_value(object(&directory.join("plan.json"))?["rows"].clone())?
    };
    let ids: BTreeSet<_> = requests
        .iter()
        .map(|r| string(r, "id"))
        .collect::<Result<_>>()?;
    let mut seen = BTreeSet::new();
    for r in &rows {
        let id = string(r, "text_sha256")?;
        ensure!(
            ids.contains(id) && seen.insert(id) && !string(r, "source_group")?.is_empty(),
            "Changed diagnostic metadata coverage"
        );
        if kind == "hosted" {
            for key in ["requested_model", "profile"] {
                ensure!(!string(r, key)?.is_empty(), "Missing hosted provenance");
            }
            pangram_fraction(r)?;
        } else {
            let source = string(r, "source_sha256")?;
            let base = rows
                .iter()
                .find(|b| b["text_sha256"] == source)
                .context("Missing unmodified grammar source")?;
            ensure!(
                base["source_sha256"] == source
                    && base["source_group"] == r["source_group"]
                    && base["origin"] == r["origin"]
                    && matches!(string(r, "origin")?, "human_only" | "model_only")
                    && !string(r, "judgment")?.is_empty(),
                "Invalid grammar ancestry"
            );
        }
    }
    ensure!(seen == ids, "Incomplete diagnostic metadata");
    Ok(rows)
}
fn pangram_fraction(meta: &Value) -> Result<f64> {
    let human = meta["fraction_human"]
        .as_f64()
        .context("Missing saved Pangram fraction")?;
    let ai = meta["fraction_ai"]
        .as_f64()
        .context("Missing saved AI fraction")?;
    let assisted = meta["fraction_ai_assisted"]
        .as_f64()
        .context("Missing saved assisted fraction")?;
    ensure!(
        [human, ai, assisted]
            .iter()
            .all(|n| n.is_finite() && (0.0..=1.0).contains(n))
            && (human + ai + assisted - 1.0).abs() < 1e-5,
        "Invalid saved Pangram fractions"
    );
    Ok(1.0 - human)
}
fn runner_names(fits: &Path) -> Vec<&'static str> {
    let mut names = vec!["train.py", "common.py", "infer.py"];
    if fits.join("ml/checkpoints.py").exists() {
        names.push("checkpoints.py");
    }
    names
}
fn check_runner_bindings(
    fits: &Path,
    bindings: &BTreeMap<PathBuf, String>,
    manifest: &Value,
) -> Result<()> {
    let catalog = fits.join("ml/checkpoints.py");
    ensure!(
        catalog.exists() == bindings.contains_key(&catalog),
        "Checkpoint catalog presence changed"
    );
    for name in runner_names(fits) {
        let path = fits.join("ml").join(name);
        let frozen = bindings.get(&path).context("Unbound runner source")?;
        ensure!(
            digest(&path)? == *frozen && manifest["files"][format!("runner/{name}")] == *frozen,
            "Changed bundled runner"
        );
    }
    Ok(())
}
fn prepare(args: Preparation) -> Result<()> {
    let Preparation {
        v6,
        fits,
        diagnostics,
        python,
        tools,
        out,
        checkpoint,
    } = args;
    ensure!(
        !out.exists() && out.is_absolute(),
        "Use a new absolute output directory"
    );
    let (v6, fits, diagnostics) = (
        v6.canonicalize()?,
        fits.canonicalize()?,
        diagnostics.canonicalize()?,
    );
    let out = out
        .parent()
        .context("Missing output parent")?
        .canonicalize()?
        .join(out.file_name().context("Missing output name")?);
    ensure!(
        !out.starts_with(&fits) && !out.starts_with(&v6),
        "Keep comparison outputs outside frozen fits and V6"
    );
    // Preserve the venv symlink: canonicalizing Python would lose its environment.
    ensure!(
        python.is_absolute() && python.is_file(),
        "Use an absolute Python executable path"
    );
    let calibration = v6.join("shards-v1/calibration.jsonl");
    let calibration_view = v6.join("assembly-v1/primary/calibration-R2.json");
    let mut bindings = BTreeMap::new();
    for p in [&calibration, &calibration_view] {
        bind(&mut bindings, p)?;
    }
    let mut training_inputs: BTreeMap<String, PathBuf> = [
        ("train", "shards-v1/train.jsonl"),
        ("development", "shards-v1/development.jsonl"),
        ("whitespace", "whitespace-shards-v1/train.jsonl"),
        (
            "development_view",
            "assembly-v1/primary/development-R2.json",
        ),
        ("checkpoint", "checkpoint/checkpoint.json"),
        ("data_rights", "shards-v1/data_rights.json"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v6.join(v)))
    .collect();
    if let Some(checkpoint) = checkpoint {
        training_inputs.insert(
            "checkpoint".into(),
            checkpoint.canonicalize()?.join("checkpoint.json"),
        );
        bind(&mut bindings, &fits.join("CAPACITY_LARGE_V1.md"))?;
    }
    for input in training_inputs.values() {
        bind(&mut bindings, input)?;
    }
    for name in [
        "origin-pairs.json",
        "bindings.sha256",
        "run.sh",
        "ORIGIN_CONSISTENCY_V1.md",
    ] {
        bind(&mut bindings, &fits.join(name))?;
    }
    for name in runner_names(&fits) {
        bind(&mut bindings, &fits.join("ml").join(name))?;
    }
    let mut cases = vec![];
    for route in ["primary", "phi4"] {
        for stage in ["R0", "R1", "R2"] {
            cases.push(Case {
                name: format!("{route}-{stage}"),
                records: v6.join(format!("{route}-coverage-v1/common/records.jsonl")),
                view: Some(v6.join(format!("{route}-coverage-v1/common/{stage}.json"))),
                baseline: v6.join(format!(
                    "final-evaluation-v1/{route}-{stage}-v6/report.json"
                )),
            });
        }
    }
    cases.push(Case {
        name: "all-humans".into(),
        records: v6.join("primary-coverage-v1/humans/v6-supported.jsonl"),
        view: None,
        baseline: v6.join("final-evaluation-v1/all-humans-v6/report.json"),
    });
    for c in &cases {
        let report = object(&c.baseline)?;
        ensure!(
            report["artifact_id"] == BASELINE && report["records_sha256"] == digest(&c.records)?,
            "Baseline corpus or artifact differs"
        );
        if let Some(view) = &c.view {
            ensure!(
                report["evaluation_view"]["sha256"] == digest(view)?,
                "Baseline view differs"
            );
            bind(&mut bindings, view)?;
        } else {
            ensure!(
                report["observation_policy"] == "all_human_roots_v1",
                "Unexpected human-root selection"
            );
        }
        bind(&mut bindings, &c.records)?;
        bind(&mut bindings, &c.baseline)?;
    }
    let hosted = diagnostics.join("hosted-v6");
    let grammar = diagnostics.join("edit-stress-reviewed-v1");
    for (directory, count) in [(&hosted, 96), (&grammar, 26)] {
        let plan = object(&directory.join("plan.json"))?;
        ensure!(
            plan["artifact_id"] == BASELINE
                && plan["requests_sha256"] == digest(&directory.join("requests.jsonl"))?,
            "Changed diagnostic request plan"
        );
        let requests = jsonl(&directory.join("requests.jsonl"))?;
        ensure!(requests.len() == count, "Changed diagnostic coverage");
        responses(&requests, &directory.join("responses.jsonl"), BASELINE)?;
        metadata(
            directory,
            if count == 96 { "hosted" } else { "grammar" },
            &requests,
        )?;
        for name in ["plan.json", "requests.jsonl", "responses.jsonl"] {
            bind(&mut bindings, &directory.join(name))?;
        }
    }
    bind(&mut bindings, &hosted.join("prior-comparison.json"))?;
    ensure!(
        object(&hosted.join("plan.json"))?["prior_comparison_sha256"]
            == digest(&hosted.join("prior-comparison.json"))?,
        "Changed Pangram comparison"
    );
    fs::create_dir_all(out.join("tools"))?;
    for name in ["encoder_evaluation", "compare_encoder_reports"] {
        fs::copy(tools.join(name), out.join("tools").join(name))?;
        bind(&mut bindings, &out.join("tools").join(name))?;
    }
    save(
        &out.join("plan.json"),
        &Plan {
            schema: "slop_ninja_origin_comparison_v1".into(),
            policy_sha256: sha256(POLICY),
            executable_sha256: digest(&std::env::current_exe()?)?,
            fits,
            python,
            output: out.clone(),
            calibration,
            calibration_view,
            hosted,
            grammar,
            cases,
            training_inputs,
            bindings,
        },
    )?;
    println!(
        "{}",
        json!({"status":"prepared","candidates":2,"corpus_views":7,"hosted_requests":96,"grammar_requests":26,"inference_calls":0})
    );
    Ok(())
}
fn selected_epoch(training: &Value) -> Result<u64> {
    let epochs = training["epoch_history"]
        .as_array()
        .context("Missing epoch history")?;
    ensure!(epochs.len() == 4, "Incomplete four-epoch fit");
    let mut best: Option<(f64, u64)> = None;
    for (i, e) in epochs.iter().enumerate() {
        ensure!(
            e["epoch"] == i + 1 && e["selection_objective"] == "human_model",
            "Unexpected epoch identity"
        );
        let loss = e["selection_loss"]
            .as_f64()
            .context("Missing selection loss")?;
        ensure!(
            loss.is_finite() && e["development_human_model"]["log_loss"] == e["selection_loss"],
            "Invalid Development loss"
        );
        let recalls: Vec<f64> =
            serde_json::from_value(e["development_human_model"]["recall_at_half"].clone())?;
        ensure!(
            recalls.len() == 2
                && recalls
                    .iter()
                    .all(|r| r.is_finite() && (0.0..=1.0).contains(r)),
            "Invalid Development recall"
        );
        let eligible = recalls.iter().all(|r| *r > 0.0);
        ensure!(e["selection_eligible"] == eligible, "Changed eligibility");
        if eligible && best.is_none_or(|(l, _)| loss < l) {
            best = Some((loss, (i + 1) as u64));
        }
    }
    let epoch = best.context("No eligible epoch")?.1;
    ensure!(
        training["selected_epoch"] == epoch,
        "Selected epoch differs from earliest eligible minimum"
    );
    Ok(epoch)
}
fn seal_artifacts(p: &Plan) -> Result<BTreeMap<PathBuf, String>> {
    let done = object(&p.fits.join("completion.json"))?;
    ensure!(
        done == json!({"phase":"both_fits_closed_unevaluated","exit_code":0,"evaluation_complete":false}),
        "Both fits must close successfully before evaluation"
    );
    let mut bindings = BTreeMap::new();
    bind(&mut bindings, &p.fits.join("completion.json"))?;
    for (name, weight) in [("control", 0.0), ("consistency", 0.5)] {
        let root = p.fits.join(name);
        let m = object(&root.join("manifest.json"))?;
        let t = object(&root.join("training.json"))?;
        ensure!(
            m["labels"] == json!(CLASS_NAMES)
                && m["max_tokens"] == 2048
                && m["files"]["training.json"] == digest(&root.join("training.json"))?,
            "Changed artifact contract"
        );
        ensure!(
            t["status"] == "unqualified_research_candidate"
                && t["seed"] == 17
                && t["epochs_requested"] == 4
                && t["learning_rate"] == 0.00001
                && t["batch_size"] == 4
                && t["device"] == "mps"
                && t["dtype"] == "float32"
                && t["optimizer"] == "AdamW"
                && t["weight_decay"] == 0.01
                && t["sample_weight_policy"] == "source_origin"
                && t["gradient_norm_limit"] == 1.0
                && t["calibration_batch_size"] == 1
                && t["class_weight_policy"] == "none"
                && t["freeze_encoder"] == false
                && t["final_test_opened"] == false
                && t["selection_objective"] == "human_model"
                && t["selection_metric"] == "development_human_model_binary_log_loss"
                && t["source_group_counts"]["train"] == 471
                && t["requires_nonzero_development_binary_recall"] == true
                && t["class_counts"]["train"] == json!([471, 2135, 1227])
                && t["origin_pairing"]["consistency_weight"] == weight
                && t["origin_pairing"]["new_source_records"] == 0
                && t["origin_pairing"]["forward_passes_per_step"] == 2
                && t["origin_pairing"]["manifest_sha256"]
                    == digest(&p.fits.join("origin-pairs.json"))?
                && t["partition_sha256"]["calibration"] == digest(&p.calibration)?
                && t["evaluation_views"]["calibration"]["sha256"] == digest(&p.calibration_view)?,
            "Candidate does not match the frozen ablation"
        );
        for key in ["train", "development"] {
            ensure!(
                t["partition_sha256"][key] == digest(&p.training_inputs[key])?,
                "Changed {key} input"
            );
        }
        ensure!(
            t["checkpoint_pin_sha256"] == digest(&p.training_inputs["checkpoint"])?
                && t["data_rights_sha256"] == digest(&p.training_inputs["data_rights"])?
                && t["evaluation_views"]["development"]["sha256"]
                    == digest(&p.training_inputs["development_view"])?
                && t["formatting_augmentation"]["alternate_train_sha256"]
                    == digest(&p.training_inputs["whitespace"])?
                && t["formatting_augmentation"]["raw_rows"] == 3833
                && t["formatting_augmentation"]["alternate_rows"] == 3833,
            "Changed checkpoint, rights or paired views"
        );
        check_runner_bindings(&p.fits, &p.bindings, &m)?;
        ensure!(
            t["epoch_history"]
                .as_array()
                .context("Missing epochs")?
                .iter()
                .all(|e| e["training_forward_rows"] == 7666),
            "Changed training exposure"
        );
        selected_epoch(&t)?;
        for (name, hash) in m["files"].as_object().context("Missing artifact files")? {
            let rel = Path::new(name);
            ensure!(
                !rel.is_absolute()
                    && !rel
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir)),
                "Unsafe artifact path"
            );
            ensure!(
                digest(&root.join(rel))? == hash.as_str().context("Invalid artifact hash")?,
                "Changed artifact file"
            );
            bind(&mut bindings, &root.join(rel))?;
        }
        bind(&mut bindings, &root.join("manifest.json"))?;
    }
    Ok(bindings)
}
fn command(
    p: &Plan,
    id: &str,
    program: &Path,
    args: &[String],
    stdin: Option<&Path>,
    stdout: Option<&Path>,
) -> Result<()> {
    let logs = p.output.join("logs");
    save(
        &logs.join(format!("{id}.command.json")),
        &json!({"program":program,"argv":args,"stdin":stdin,"stdout":stdout}),
    )?;
    let output = stdout
        .map(PathBuf::from)
        .unwrap_or_else(|| logs.join(format!("{id}.stdout")));
    let mut c = Command::new(program);
    c.args(args)
        .env("HF_HUB_OFFLINE", "1")
        .env("TRANSFORMERS_OFFLINE", "1")
        .env("OMP_NUM_THREADS", "1")
        .env("OPENBLAS_NUM_THREADS", "1")
        .env("VECLIB_MAXIMUM_THREADS", "1")
        .env("MKL_NUM_THREADS", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("TOKENIZERS_PARALLELISM", "false")
        .stdout(fs::File::create_new(output)?)
        .stderr(fs::File::create_new(logs.join(format!("{id}.stderr")))?);
    if let Some(input) = stdin {
        c.stdin(fs::File::open(input)?);
    } else {
        c.stdin(std::process::Stdio::null());
    }
    let status = c.status()?;
    save(
        &logs.join(format!("{id}.exit.json")),
        &json!({"exit_code":status.code(),"success":status.success()}),
    )?;
    ensure!(
        status.success(),
        "Command {id} failed; partial evidence retained"
    );
    Ok(())
}
fn arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}
fn points(thresholds: &Value, artifact: &str) -> Result<Vec<OperatingPoint>> {
    let c = &thresholds["content"];
    ensure!(
        thresholds["content_sha256"] == sha256(serde_json::to_vec(c)?)
            && c["artifact_id"] == artifact,
        "Threshold binding differs"
    );
    let points: Vec<OperatingPoint> = serde_json::from_value(c["points"].clone())?;
    ensure!(points.len() == 2, "Expected two operating points");
    for (point, target) in points.iter().zip([0.01, 0.05]) {
        point.validate()?;
        ensure!(
            point.target_human_false_positive_rate == target,
            "Changed target FPR"
        );
    }
    Ok(points)
}
fn diagnostic(p: &Plan, name: &str, kind: &str, thresholds: &Value) -> Result<()> {
    let source = if kind == "hosted" {
        &p.hosted
    } else {
        &p.grammar
    };
    let directory = p.output.join(name).join(kind);
    fs::create_dir(&directory)?;
    let root = p.fits.join(name);
    let artifact = object(&root.join("manifest.json"))?;
    let id = string(&artifact, "artifact_id")?;
    let request = source.join("requests.jsonl");
    let response = directory.join("responses.jsonl");
    command(
        p,
        &format!("{name}-{kind}"),
        &p.python,
        &[
            arg(&root.join("runner/infer.py")),
            "--artifact".into(),
            arg(&root),
        ],
        Some(&request),
        Some(&response),
    )?;
    let requests = jsonl(&request)?;
    let scores = responses(&requests, &response, id)?;
    let old = responses(&requests, &source.join("responses.jsonl"), BASELINE)?;
    let metadata = metadata(source, kind, &requests)?;
    let operating = points(thresholds, id)?;
    let mut rows = vec![];
    let mut detail = vec![];
    let mut seen = BTreeSet::new();
    let mut slices: BTreeMap<String, Vec<ProbabilityRow>> = BTreeMap::new();
    for meta in metadata {
        let hash = string(&meta, "text_sha256")?;
        ensure!(
            seen.insert(hash.to_owned()),
            "Duplicate diagnostic metadata"
        );
        let prob = *scores.get(hash).context("Missing diagnostic text")?;
        let prior = *old.get(hash).context("Missing prior score")?;
        let label = if kind == "hosted" {
            1
        } else {
            match string(&meta, "origin")? {
                "human_only" => 0,
                "model_only" => 1,
                _ => anyhow::bail!("Unexpected grammar origin"),
            }
        };
        let row = ProbabilityRow {
            id: hash.into(),
            group: string(&meta, "source_group")?.into(),
            label,
            probabilities: prob,
        };
        let mut d = json!({"text_sha256":hash,"source_group":row.group,"origin_label":label,"baseline_probability":prior[1]+prior[2],"candidate_probability":prob[1]+prob[2],"candidate_flags":operating.iter().map(|t|prob[1]+prob[2]>t.threshold).collect::<Vec<_>>()});
        if kind == "hosted" {
            let fraction = pangram_fraction(&meta)?;
            d["pangram_fraction"] = json!(fraction);
            d["pangram_flag"] = json!(fraction >= 0.1);
            for key in ["requested_model", "profile"] {
                d[key] = meta[key].clone();
                slices
                    .entry(format!("{key}:{}", string(&meta, key)?))
                    .or_default()
                    .push(row.clone());
            }
        } else {
            let base = string(&meta, "source_sha256")?;
            let base_p = scores
                .get(base)
                .context("Missing unchanged grammar source")?;
            let base_old = old.get(base).context("Missing baseline grammar source")?;
            d["source_sha256"] = json!(base);
            d["mask"] = meta["mask"].clone();
            d["judgment"] = meta["judgment"].clone();
            d["candidate_edit_change_pp"] =
                json!(100.0 * (prob[1] + prob[2] - base_p[1] - base_p[2]));
            d["baseline_edit_change_pp"] =
                json!(100.0 * (prior[1] + prior[2] - base_old[1] - base_old[2]));
            slices
                .entry(format!("judgment:{}", string(&meta, "judgment")?))
                .or_default()
                .push(row.clone());
        }
        rows.push(row);
        detail.push(d);
    }
    ensure!(seen.len() == scores.len(), "Incomplete diagnostic metadata");
    let slice_reports = slices
        .iter()
        .map(|(k, v)| {
            Ok((
                k.clone(),
                metrics::evaluate_probability_rows(id, v, [1.0 / 3.0; 3], &operating)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    save(
        &directory.join("report.json"),
        &json!({"schema":"slop_ninja_origin_diagnostic_v1","kind":kind,"artifact_id":id,"baseline_artifact_id":BASELINE,
        "requests_sha256":digest(&request)?,"responses_sha256":digest(&response)?,"thresholds_sha256":sha256(serde_json::to_vec(thresholds)?),
        "report":metrics::evaluate_probability_rows(id,&rows,[1.0/3.0;3],&operating)?,"human_model":metrics::summarize_human_model(&rows)?,"slices":slice_reports,"details":detail,
        "previously_inspected_diagnostic":true,"fresh_confirmation":false,"provider_calls":0}),
    )?;
    Ok(())
}
fn run(plan: &Path) -> Result<()> {
    let p: Plan = serde_json::from_slice(&fs::read(plan)?)?;
    ensure!(
        p.schema == "slop_ninja_origin_comparison_v1"
            && p.policy_sha256 == sha256(POLICY)
            && p.executable_sha256 == digest(&std::env::current_exe()?)?,
        "Changed comparison recipe or executable"
    );
    ensure!(
        !p.output.join("artifacts.json").exists() && !p.output.join("completion.json").exists(),
        "Comparison already started; inspect existing evidence"
    );
    check(&p.bindings)?;
    let mut artifact_bindings = seal_artifacts(&p)?;
    bind(&mut artifact_bindings, plan)?;
    save(&p.output.join("artifacts.json"), &artifact_bindings)?;
    fs::create_dir(p.output.join("logs"))?;
    let result = (|| -> Result<()> {
        let evaluator = p.output.join("tools/encoder_evaluation");
        let comparer = p.output.join("tools/compare_encoder_reports");
        for name in ["control", "consistency"] {
            fs::create_dir(p.output.join(name))?;
            command(
                &p,
                &format!("{name}-thresholds"),
                &evaluator,
                &[
                    "--artifact".into(),
                    arg(&p.fits.join(name)),
                    "--python".into(),
                    arg(&p.python),
                    "--output-dir".into(),
                    arg(&p.output.join(name).join("calibration")),
                    "freeze-thresholds".into(),
                    "--calibration-jsonl".into(),
                    arg(&p.calibration),
                    "--calibration-view".into(),
                    arg(&p.calibration_view),
                ],
                None,
                None,
            )?;
        }
        for name in ["control", "consistency"] {
            let path = p.output.join(name).join("calibration/thresholds.json");
            let artifact = object(&p.fits.join(name).join("manifest.json"))?;
            points(&object(&path)?, string(&artifact, "artifact_id")?)?;
            bind(&mut artifact_bindings, &path)?;
        }
        save(
            &p.output.join("opening.json"),
            &json!({"bindings":artifact_bindings,"previously_inspected_diagnostics":true,"candidate_switching_permitted":false,"private_comparison_pending":true}),
        )?;
        for name in ["control", "consistency"] {
            let threshold = p.output.join(name).join("calibration/thresholds.json");
            for c in &p.cases {
                let out = p.output.join(name).join(&c.name);
                let mut args = vec![
                    "--artifact".into(),
                    arg(&p.fits.join(name)),
                    "--python".into(),
                    arg(&p.python),
                    "--output-dir".into(),
                    arg(&out),
                    "evaluate".into(),
                    "--records".into(),
                    arg(&c.records),
                    "--thresholds".into(),
                    arg(&threshold),
                    "--split".into(),
                    "test".into(),
                    "--open-final-test".into(),
                ];
                if let Some(view) = &c.view {
                    args.extend(["--evaluation-view".into(), arg(view)]);
                } else {
                    args.push("--human-roots-only".into());
                }
                command(
                    &p,
                    &format!("{name}-{}", c.name),
                    &evaluator,
                    &args,
                    None,
                    None,
                )?;
                command(
                    &p,
                    &format!("{name}-{}-comparison", c.name),
                    &comparer,
                    &[
                        "--baseline".into(),
                        arg(&c.baseline),
                        "--candidate".into(),
                        arg(&out.join("report.json")),
                        "--output".into(),
                        arg(&out.join("vs-v6.json")),
                    ],
                    None,
                    None,
                )?;
            }
            for kind in ["hosted", "grammar"] {
                diagnostic(&p, name, kind, &object(&threshold)?)?;
            }
        }
        for c in &p.cases {
            command(
                &p,
                &format!("treatment-vs-control-{}", c.name),
                &comparer,
                &[
                    "--baseline".into(),
                    arg(&p.output.join("control").join(&c.name).join("report.json")),
                    "--candidate".into(),
                    arg(&p
                        .output
                        .join("consistency")
                        .join(&c.name)
                        .join("report.json")),
                    "--output".into(),
                    arg(&p
                        .output
                        .join(format!("treatment-vs-control-{}.json", c.name))),
                ],
                None,
                None,
            )?;
        }
        check(&p.bindings)?;
        check(&artifact_bindings)?;
        Ok(())
    })();
    save(
        &p.output.join("completion.json"),
        &json!({"public_diagnostics_complete":result.is_ok(),"error":result.as_ref().err().map(|e|e.to_string()),"private_comparison_pending":true,"fresh_confirmation_complete":false,"reference_promoted":false,"provider_calls":0}),
    )?;
    result
}
fn main() -> Result<()> {
    match Args::parse().task {
        Task::Prepare(args) => prepare(args),
        Task::Run { plan } => run(&plan),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runner_binding_rejects_catalog_insertion_removal_and_substitution() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join("ml")).unwrap();
        let mut bindings = BTreeMap::new();
        let mut manifest = json!({"files":{}});
        for name in ["train.py", "common.py", "infer.py"] {
            let path = root.join("ml").join(name);
            fs::write(&path, name).unwrap();
            bind(&mut bindings, &path).unwrap();
            manifest["files"][format!("runner/{name}")] = json!(digest(&path).unwrap());
        }
        assert!(check_runner_bindings(&root, &bindings, &manifest).is_ok());
        let catalog = root.join("ml/checkpoints.py");
        fs::write(&catalog, b"fixed checkpoint catalog").unwrap();
        assert!(check_runner_bindings(&root, &bindings, &manifest).is_err());
        bind(&mut bindings, &catalog).unwrap();
        manifest["files"]["runner/checkpoints.py"] = json!(digest(&catalog).unwrap());
        assert!(check_runner_bindings(&root, &bindings, &manifest).is_ok());
        fs::write(&catalog, b"substituted checkpoint catalog").unwrap();
        assert!(check_runner_bindings(&root, &bindings, &manifest).is_err());
        fs::write(&catalog, b"fixed checkpoint catalog").unwrap();
        manifest["files"]["runner/checkpoints.py"] = json!("substituted bundle");
        assert!(check_runner_bindings(&root, &bindings, &manifest).is_err());
        fs::remove_file(&catalog).unwrap();
        assert!(check_runner_bindings(&root, &bindings, &manifest).is_err());
    }
    #[test]
    fn selection_requires_the_earliest_eligible_development_minimum() {
        let epochs:Vec<_>=[(0.5,true),(0.2,false),(0.3,true),(0.3,true)].iter().enumerate().map(|(i,(loss,eligible))|json!({"epoch":i+1,"selection_objective":"human_model","selection_loss":loss,"selection_eligible":eligible,"development_human_model":{"log_loss":loss,"recall_at_half":[1.0,if *eligible{1.0}else{0.0}]}})).collect();
        let mut t = json!({"epoch_history":epochs,"selected_epoch":3});
        assert_eq!(selected_epoch(&t).unwrap(), 3);
        t["selected_epoch"] = json!(4);
        assert!(selected_epoch(&t).is_err());
        t["selected_epoch"] = json!(2);
        assert!(selected_epoch(&t).is_err());
        t["selected_epoch"] = json!(3);
        t["epoch_history"][1]["selection_eligible"] = json!(true);
        assert!(selected_epoch(&t).is_err());
    }
    #[test]
    fn response_binding_rejects_wrong_text_artifact_and_missing_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scores.jsonl");
        let id = sha256("Synthetic text.");
        let requests = vec![json!({"id":id,"text":"Synthetic text."})];
        let good = json!({"id":id,"artifact_id":"synthetic","status":"ok","labels":CLASS_NAMES,"runtime":"cpu_float32_eager","token_count":5,"probabilities":[0.2,0.7,0.1],"model_or_assistance_probability":0.8});
        fs::write(&path, good.to_string()).unwrap();
        assert!(responses(&requests, &path, "synthetic").is_ok());
        assert!(responses(&requests, &path, "other").is_err());
        let mut changed = requests.clone();
        changed[0]["text"] = json!("Changed text.");
        assert!(responses(&changed, &path, "synthetic").is_err());
        fs::write(&path, "").unwrap();
        assert!(responses(&requests, &path, "synthetic").is_err());
    }
}
