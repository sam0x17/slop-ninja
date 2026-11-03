//! Saved-model fit on the complete original Train and whitespace archives.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{OriginRecord, Split, read_records, sha256},
    metrics::{self, ProbabilityRow},
    model::CLASS_NAMES,
};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    task: Task,
}
#[derive(Subcommand)]
enum Task {
    Prepare {
        #[arg(long)]
        public_plan: PathBuf,
        #[arg(long)]
        reference_artifact: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    Run {
        #[arg(long)]
        plan: PathBuf,
    },
}
#[derive(Serialize, Deserialize)]
struct Candidate {
    role: String,
    artifact: PathBuf,
    artifact_id: String,
    temperature: f64,
    thresholds: Vec<Value>,
    training_history: Vec<Value>,
}
#[derive(Serialize, Deserialize)]
struct Plan {
    schema: String,
    output: PathBuf,
    python: PathBuf,
    views: BTreeMap<String, PathBuf>,
    candidates: Vec<Candidate>,
    bindings: BTreeMap<PathBuf, String>,
}
fn object(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}
fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
fn bind(path: &Path, map: &mut BTreeMap<PathBuf, String>) -> Result<String> {
    let path = path.canonicalize()?;
    let hash = sha256(fs::read(&path)?);
    if let Some(old) = map.insert(path, hash.clone()) {
        ensure!(old == hash, "Input changed");
    }
    Ok(hash)
}
fn verify(bindings: &BTreeMap<PathBuf, String>) -> Result<()> {
    for (path, hash) in bindings {
        ensure!(
            sha256(fs::read(path)?) == *hash,
            "Changed input {}",
            path.display()
        );
    }
    Ok(())
}
fn ordered(path: &Path) -> Result<Vec<OriginRecord>> {
    let mut rows = read_records(path)?;
    ensure!(
        rows.iter().all(|r| r.split == Some(Split::Train)),
        "Train diagnostic rejects other partitions"
    );
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(rows)
}
fn validate_views(raw: &[OriginRecord], whitespace: &[OriginRecord]) -> Result<()> {
    ensure!(
        raw.len() == 3833 && raw.len() == whitespace.len(),
        "Unexpected Train coverage"
    );
    let mut groups = BTreeMap::<&str, [usize; 3]>::new();
    for (a, b) in raw.iter().zip(whitespace) {
        ensure!(
            a.id == b.id
                && a.source_group == b.source_group
                && a.origin == b.origin
                && a.parent_id == b.parent_id
                && a.evidence == b.evidence,
            "Formatting view changed identity, origin or ancestry"
        );
        ensure!(
            a.text.split_whitespace().collect::<Vec<_>>().join(" ") == b.text,
            "Alternate view is not exact whitespace collapse"
        );
        groups.entry(&a.source_group).or_default()[a.origin.index()] += 1;
    }
    ensure!(
        groups.len() == 471 && groups.values().all(|c| c[0] == 1 && c[1] > 0 && c[2] > 0),
        "Missing family/origin coverage"
    );
    ensure!(
        (0..3)
            .map(|i| groups.values().map(|v| v[i]).sum::<usize>())
            .collect::<Vec<_>>()
            == [471, 2135, 1227],
        "Changed Train class counts"
    );
    Ok(())
}
fn prepare(public_plan: &Path, reference: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "Use a new diagnostic directory");
    let mut bindings = BTreeMap::new();
    bind(public_plan, &mut bindings)?;
    let public = object(public_plan)?;
    ensure!(
        public["schema"] == "slop_ninja_origin_comparison_v1",
        "Wrong public plan"
    );
    let public_root = PathBuf::from(text(&public, "output")?);
    let done_path = public_root.join("completion.json");
    bind(&done_path, &mut bindings)?;
    let done = object(&done_path)?;
    ensure!(
        done["public_diagnostics_complete"] == true && done["error"].is_null(),
        "Base public comparison must be closed"
    );
    let mut views = BTreeMap::new();
    for (view, key) in [("raw", "train"), ("whitespace", "whitespace")] {
        let path = PathBuf::from(text(&public["training_inputs"], key)?).canonicalize()?;
        bind(&path, &mut bindings)?;
        views.insert(view.to_string(), path);
    }
    let raw = ordered(&views["raw"])?;
    let whitespace = ordered(&views["whitespace"])?;
    validate_views(&raw, &whitespace)?;
    let mut candidates = Vec::new();
    for role in ["v6", "control", "consistency"] {
        let artifact = if role == "v6" {
            reference.canonicalize()?
        } else {
            PathBuf::from(text(&public, "fits")?)
                .join(role)
                .canonicalize()?
        };
        let manifest_path = artifact.join("manifest.json");
        bind(&manifest_path, &mut bindings)?;
        let manifest = object(&manifest_path)?;
        for (file, hash) in manifest["files"]
            .as_object()
            .context("Missing manifest files")?
        {
            let path = artifact.join(file).canonicalize()?;
            ensure!(path.starts_with(&artifact), "Artifact member escapes root");
            ensure!(
                bind(&path, &mut bindings)? == hash.as_str().context("Invalid file hash")?,
                "Artifact member differs"
            );
        }
        let training = object(&artifact.join("training.json"))?;
        ensure!(
            training["partition_sha256"]["train"] == bindings[&views["raw"]]
                && training["formatting_augmentation"]["alternate_train_sha256"]
                    == bindings[&views["whitespace"]],
            "Artifact was fitted on different Train views"
        );
        ensure!(
            training["sample_weight_policy"] == "source_origin"
                && manifest["reference_runtime"]["threads"] == 1
                && manifest["max_tokens"] == 2048,
            "Different fitted weighting/runtime"
        );
        let report_path = if role == "v6" {
            let case = public["cases"]
                .as_array()
                .context("Missing cases")?
                .iter()
                .find(|c| c["name"] == "primary-R0")
                .context("Missing primary case")?;
            PathBuf::from(text(case, "baseline")?)
        } else {
            public_root.join(role).join("primary-R0/report.json")
        };
        bind(&report_path, &mut bindings)?;
        let report = object(&report_path)?;
        ensure!(
            report["artifact_id"] == manifest["artifact_id"]
                && report["schema"] == "slop_ninja_frozen_encoder_evaluation_v1",
            "Wrong frozen threshold source"
        );
        let thresholds = report["report"]["operating_points"]
            .as_array()
            .context("Missing thresholds")?
            .iter()
            .map(|p| p["calibration"].clone())
            .collect::<Vec<_>>();
        ensure!(
            thresholds.len() == 2
                && thresholds
                    .iter()
                    .all(|p| p["comparison"] == "strictly_greater_than"
                        && p["threshold"]
                            .as_f64()
                            .is_some_and(|t| t.is_finite() && (0.0..=1.0).contains(&t))),
            "Invalid original operating points"
        );
        let temperature = object(&artifact.join("calibration.json"))?["temperature"]
            .as_f64()
            .context("Missing temperature")?;
        ensure!(
            temperature.is_finite() && temperature > 0.0,
            "Invalid temperature"
        );
        let history=training["epoch_history"].as_array().context("Missing history")?.iter().map(|e|json!({"epoch":e["epoch"],"selected":e["epoch"]==training["selected_epoch"],"online_training_loss":e["training_loss"],"online_pair_js":e["origin_pair_js_divergence"],"development_human_model":e["development_human_model"],"development_three_class":e["development"]})).collect();
        candidates.push(Candidate {
            role: role.into(),
            artifact,
            artifact_id: text(&manifest, "artifact_id")?.into(),
            temperature,
            thresholds,
            training_history: history,
        });
    }
    verify(&bindings)?;
    fs::create_dir_all(out)?;
    let output = out.canonicalize()?;
    for (view, rows) in [("raw", &raw), ("whitespace", &whitespace)] {
        let path = output.join(format!("{view}-requests.jsonl"));
        let mut file = fs::File::create_new(&path)?;
        for r in rows {
            serde_json::to_writer(&mut file, &json!({"id":r.id,"text":r.text}))?;
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        bind(&path, &mut bindings)?;
    }
    let plan = Plan {
        schema: "slop_ninja_train_fit_diagnostic_v1".into(),
        output: output.clone(),
        python: PathBuf::from(text(&public, "python")?),
        views,
        candidates,
        bindings,
    };
    save(&output.join("plan.json"), &plan)?;
    save(
        &output.join("preflight.json"),
        &json!({"records_per_view":3833,"families":471,"views":2,"models":3,"planned_inference_observations":22998,"training_admission":false,"provider_calls":0,"fresh_confirmation":false}),
    )
}
fn temperature_one(q: [f64; 3], temperature: f64) -> Result<[f64; 3]> {
    ensure!(
        temperature.is_finite()
            && temperature > 0.0
            && q.iter().all(|p| p.is_finite() && *p > 0.0 && *p <= 1.0)
            && (q.iter().sum::<f64>() - 1.0).abs() < 1e-6,
        "Cannot invert invalid or zero probabilities"
    );
    let logs = q.map(|p| temperature * p.ln());
    let max = logs.into_iter().fold(f64::NEG_INFINITY, f64::max);
    let v = logs.map(|x| (x - max).exp());
    let sum = v.iter().sum::<f64>();
    Ok(v.map(|x| x / sum))
}
fn bind_predictions(
    records: &[OriginRecord],
    values: &[Value],
    artifact_id: &str,
) -> Result<Vec<ProbabilityRow>> {
    ensure!(
        records.len() == values.len(),
        "Incomplete prediction coverage; retain partial output"
    );
    let mut result = Vec::new();
    for (r, v) in records.iter().zip(values) {
        ensure!(
            v["id"] == r.id
                && v["artifact_id"] == artifact_id
                && v["labels"] == json!(CLASS_NAMES)
                && v["status"] == "ok"
                && v["runtime"] == "cpu_float32_eager"
                && v["token_count"]
                    .as_u64()
                    .is_some_and(|n| n > 0 && n <= 2048),
            "Prediction failed or changed order/identity/runtime; retain every response"
        );
        result.push(ProbabilityRow {
            id: r.id.clone(),
            group: r.source_group.clone(),
            label: r.origin.index(),
            probabilities: serde_json::from_value(v["probabilities"].clone())?,
        });
    }
    metrics::summarize(&result)?;
    Ok(result)
}
fn balanced(rows: &[ProbabilityRow]) -> Result<Value> {
    metrics::summarize(rows)?;
    let mut cells = BTreeMap::<(&str, usize), Vec<ProbabilityRow>>::new();
    for r in rows {
        cells
            .entry((&r.group, r.label))
            .or_default()
            .push(r.clone());
    }
    let mut losses = [Vec::new(), Vec::new(), Vec::new()];
    let mut binary = [Vec::new(), Vec::new()];
    for ((_, label), cell) in cells {
        let summary = metrics::summarize(&cell)?;
        losses[label].push([summary.log_loss, summary.accuracy, summary.multiclass_brier]);
        if label < 2 {
            let summary = metrics::summarize_human_model(&cell)?.context("Missing binary cell")?;
            binary[label].push([
                summary.log_loss,
                summary.accuracy_at_half,
                summary.binary_brier,
            ]);
        }
    }
    let average = |v: Vec<[f64; 3]>| {
        if v.is_empty() {
            None
        } else {
            Some(std::array::from_fn(|i| {
                v.iter().map(|r| r[i]).sum::<f64>() / v.len() as f64
            }))
        }
    };
    let means: [Option<[f64; 3]>; 3] = losses.map(average);
    let binary_means: [Option<[f64; 3]>; 2] = binary.map(average);
    let render =
        |v: Option<[f64; 3]>| v.map(|a| json!({"log_loss":a[0],"accuracy":a[1],"brier":a[2]}));
    let total = |v: &[Option<[f64; 3]>]| {
        if v.iter().all(Option::is_some) {
            average(v.iter().flatten().copied().collect())
        } else {
            None
        }
    };
    Ok(
        json!({"three_class_by_origin":means.map(render),"three_class":render(total(&means)),"binary_by_origin":binary_means.map(render),"binary":render(total(&binary_means)),"class_order":CLASS_NAMES,"binary_class_order":["human_only","model_only"],"binary_mixed_policy":"excluded","binary_accuracy_rule":"combined probability strictly greater than 0.5"}),
    )
}
fn measures(rows: &[ProbabilityRow]) -> Result<Value> {
    Ok(
        json!({"three_class":metrics::summarize(rows)?,"human_model":metrics::summarize_human_model(rows)?,"equal_family_origin":balanced(rows)?}),
    )
}
fn summarize(rows: &[ProbabilityRow], records: &[OriginRecord], c: &Candidate) -> Result<Value> {
    let unscaled = rows
        .iter()
        .map(|r| {
            Ok(ProbabilityRow {
                probabilities: temperature_one(r.probabilities, c.temperature)?,
                ..r.clone()
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let roots = records
        .iter()
        .filter(|r| r.origin.index() == 0)
        .map(|r| (r.source_group.as_str(), r.source.collection.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut collections = BTreeMap::<&str, Vec<usize>>::new();
    for (i, row) in rows.iter().enumerate() {
        collections
            .entry(
                roots
                    .get(row.group.as_str())
                    .context("Missing human collection")?,
            )
            .or_default()
            .push(i);
    }
    let slices=collections.iter().map(|(collection,indices)|{let selected=indices.iter().map(|&i|rows[i].clone()).collect::<Vec<_>>();let raw=indices.iter().map(|&i|unscaled[i].clone()).collect::<Vec<_>>();Ok(json!({"collection":collection,"calibrated":measures(&selected)?,"temperature_one":measures(&raw)?,"original_cutoffs":c.thresholds.iter().map(|p|{let threshold=p["threshold"].as_f64().unwrap();json!({"calibration":p,"flagged_by_origin":(0..3).map(|label|selected.iter().filter(|r|r.label==label && r.probabilities[1]+r.probabilities[2]>threshold).count()).collect::<Vec<_>>()})}).collect::<Vec<_>>()}))}).collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"calibrated":measures(rows)?,"temperature_one":measures(&unscaled)?,"collections_by_human_root":slices,"predictions":rows}),
    )
}
fn run(path: &Path) -> Result<()> {
    let bytes = fs::read(path)?;
    let plan: Plan = serde_json::from_slice(&bytes)?;
    ensure!(
        plan.schema == "slop_ninja_train_fit_diagnostic_v1"
            && plan
                .candidates
                .iter()
                .map(|c| c.role.as_str())
                .collect::<Vec<_>>()
                == ["v6", "control", "consistency"],
        "Wrong diagnostic plan"
    );
    verify(&plan.bindings)?;
    let raw = ordered(&plan.views["raw"])?;
    let whitespace = ordered(&plan.views["whitespace"])?;
    validate_views(&raw, &whitespace)?;
    save(
        &plan.output.join("started.json"),
        &json!({"at":chrono::Utc::now(),"pid":std::process::id(),"plan_sha256":sha256(&bytes)}),
    )?;
    let result = (|| -> Result<Vec<Value>> {
        let mut completed = Vec::new();
        for c in &plan.candidates {
            for (view, records) in [("raw", &raw), ("whitespace", &whitespace)] {
                let out = plan.output.join(format!("{}-{view}", c.role));
                fs::create_dir(&out)?;
                let status = Command::new(&plan.python)
                    .arg(c.artifact.join("runner/infer.py"))
                    .arg("--artifact")
                    .arg(&c.artifact)
                    .env("HF_HUB_OFFLINE", "1")
                    .env("TRANSFORMERS_OFFLINE", "1")
                    .env("OMP_NUM_THREADS", "1")
                    .env("OPENBLAS_NUM_THREADS", "1")
                    .stdin(fs::File::open(
                        plan.output.join(format!("{view}-requests.jsonl")),
                    )?)
                    .stdout(fs::File::create_new(out.join("responses.jsonl"))?)
                    .stderr(fs::File::create_new(out.join("inference.log"))?)
                    .status()?;
                save(
                    &out.join("exit.json"),
                    &json!({"success":status.success(),"code":status.code()}),
                )?;
                ensure!(
                    status.success(),
                    "Bundled inference failed; partial files retained"
                );
                let response = fs::read_to_string(out.join("responses.jsonl"))?;
                let values = response
                    .lines()
                    .map(serde_json::from_str)
                    .collect::<serde_json::Result<Vec<Value>>>()?;
                let predictions = bind_predictions(records, &values, &c.artifact_id)?;
                let report = json!({"schema":"slop_ninja_train_fit_report_v1","role":c.role,"view":view,"split":"train","artifact_id":c.artifact_id,"temperature":c.temperature,"records_sha256":plan.bindings[&plan.views[view]],"requests_sha256":plan.bindings[&plan.output.join(format!("{view}-requests.jsonl"))],"responses_sha256":sha256(response),"metrics":summarize(&predictions,records,c)?,"training_history":c.training_history,"fresh_confirmation":false,"provider_calls":0,"threshold_or_temperature_fitted":false});
                save(&out.join("report.json"), &report)?;
                completed.push(json!({"role":c.role,"view":view,"report":out.join("report.json"),"sha256":sha256(fs::read(out.join("report.json"))?)}));
                eprintln!("{} {view}: 3833 Train predictions retained", c.role);
            }
        }
        verify(&plan.bindings)?;
        ensure!(
            sha256(fs::read(path)?) == sha256(&bytes),
            "Plan changed during execution"
        );
        Ok(completed)
    })();
    save(
        &plan.output.join("completion.json"),
        &json!({"at":chrono::Utc::now(),"analysis_complete":result.is_ok(),"reports":result.as_ref().ok(),"error":result.as_ref().err().map(|e|e.to_string()),"fresh_confirmation":false,"reference_promoted":false,"provider_calls":0,"new_training_records":0}),
    )?;
    result.map(|_| ())
}
fn main() -> Result<()> {
    match Args::parse().task {
        Task::Prepare {
            public_plan,
            reference_artifact,
            out,
        } => prepare(&public_plan, &reference_artifact, &out),
        Task::Run { plan } => run(&plan),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn family_origin_weighting_prevents_replication_from_changing_loss() -> Result<()> {
        let rows = (0..3)
            .map(|label| ProbabilityRow {
                id: format!("r{label}"),
                group: "g".into(),
                label,
                probabilities: [0.2, 0.3, 0.5],
            })
            .collect::<Vec<_>>();
        let mut duplicated = rows.clone();
        let mut extra = rows[1].clone();
        extra.id = "duplicate-model-view".into();
        duplicated.push(extra);
        assert_eq!(balanced(&rows)?, balanced(&duplicated)?);
        assert_ne!(
            metrics::summarize(&rows)?.log_loss,
            metrics::summarize(&duplicated)?.log_loss
        );
        Ok(())
    }
    #[test]
    fn reconstruction_rejects_invalid_probabilities() -> Result<()> {
        let result = temperature_one([0.2, 0.3, 0.5], 2.0)?;
        assert!((result[0] - 0.04 / 0.38).abs() < 1e-12);
        assert!(temperature_one([0.0, 0.5, 0.5], 1.0).is_err());
        assert!(temperature_one([0.2, 0.3, 0.5], 0.0).is_err());
        Ok(())
    }
}
