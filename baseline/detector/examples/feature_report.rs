//! Summaries and fixed-operating-point diagnostics for the neural feature grid.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::sha256,
    metrics::{self, OperatingPoint, ProbabilityRow},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufWriter, Write},
    path::{Component, Path, PathBuf},
};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Grid {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    Thresholds {
        #[arg(long)]
        probabilities: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    Diagnostic {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        responses: PathBuf,
        #[arg(long)]
        thresholds: PathBuf,
        #[arg(long)]
        projections: Option<PathBuf>,
        #[arg(long)]
        output: PathBuf,
    },
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn lines(path: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(path)?
        .lines()
        .map(|s| Ok(serde_json::from_str(s)?))
        .collect()
}
fn digest(path: &Path) -> Result<String> {
    Ok(sha256(fs::read(path)?))
}
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn number(v: &Value, k: &str) -> Result<f64> {
    let n = v[k].as_f64().with_context(|| format!("missing {k}"))?;
    ensure!(n.is_finite(), "nonfinite {k}");
    Ok(n)
}
fn write(path: &Path, value: &Value) -> Result<()> {
    let mut writer = BufWriter::new(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?,
    );
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
fn probabilities(v: &Value) -> Result<[f64; 3]> {
    ensure!(v["status"] == "ok", "unavailable response");
    let p: [f64; 3] = serde_json::from_value(v["probabilities"].clone())?;
    ensure!(
        p.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x))
            && (p.iter().sum::<f64>() - 1.0).abs() < 1e-6,
        "invalid probabilities"
    );
    Ok(p)
}
fn response_map(path: &Path) -> Result<BTreeMap<String, Value>> {
    let mut map = BTreeMap::new();
    for row in lines(path)? {
        probabilities(&row)?;
        let id = text(&row, "id")?.to_owned();
        ensure!(map.insert(id, row).is_none(), "duplicate response");
    }
    ensure!(!map.is_empty(), "empty responses");
    Ok(map)
}
fn grid(root: &Path) -> Result<Value> {
    let completion = read(&root.join("completion.json"))?;
    ensure!(
        completion["phase"] == "feature_grid_closed" && completion["exit_code"] == 0,
        "grid not closed successfully"
    );
    for line in fs::read_to_string(root.join("bindings.sha256"))?.lines() {
        let (hash, path) = line.split_once("  ").context("invalid input binding")?;
        ensure!(digest(Path::new(path))? == hash, "changed grid input");
    }
    let mut rows = vec![];
    let mut best: BTreeMap<String, Value> = BTreeMap::new();
    let mut common = None;
    for (mode, widths) in [
        ("word", vec![0, 128, 256, 512, 1024]),
        ("grammar", vec![0, 128, 512]),
        ("combined", vec![0, 128, 512]),
    ] {
        for width in widths {
            let name = format!("{mode}-{width}");
            let dir = root.join("fits").join(&name);
            let c = read(&dir.join("completion.json"))?;
            let t = read(&dir.join("training.json"))?;
            ensure!(
                t["epochs_requested"] == 30
                    && t["seed"] == 17
                    && t["batch_size"] == 64
                    && t["learning_rate"] == 0.0003
                    && t["weight_decay"] == 0.001
                    && t["dtype"] == "float32"
                    && t["sample_weighting"] == "source_origin"
                    && t["formatting_augmentation"] == false
                    && t["origin_consistency_loss"] == false
                    && t["final_test_opened"] == false,
                "changed fitting recipe"
            );
            let shared = json!([
                t["source_partition_sha256"],
                t["evaluation_views"],
                t["source_code_sha256"]
            ]);
            if let Some(ref expected) = common {
                ensure!(&shared == expected, "grid inputs differ");
            } else {
                common = Some(shared);
            }
            let history = t["epoch_history"].as_array().context("missing history")?;
            ensure!(history.len() == 30, "incomplete history");
            let dimensions = if mode == "combined" { 16384 } else { 8192 };
            let parameters = if width == 0 {
                dimensions * 3 + 3
            } else {
                dimensions * width + 4 * width + 3
            };
            ensure!(
                t["config"]["dimensions"] == dimensions
                    && t["config"]["hidden_width"] == width
                    && t["parameters"] == parameters,
                "unexpected head shape or parameter count"
            );
            for (i, e) in history.iter().enumerate() {
                let recalls = e["development_human_model"]["recall_at_half"]
                    .as_array()
                    .context("missing recalls")?;
                ensure!(
                    recalls.len() == 2
                        && e["epoch"] == i + 1
                        && e["eligible"]
                            == recalls.iter().all(|r| r.as_f64().is_some_and(|x| x > 0.)),
                    "incorrect eligibility history"
                );
            }
            let selected = history
                .iter()
                .enumerate()
                .filter(|(_, r)| r["eligible"] == true)
                .map(|(i, r)| Ok((i, number(&r["development_human_model"], "log_loss")?)))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
            let Some((index, loss)) = selected else {
                ensure!(
                    c["status"] == "no_eligible_epoch" && t["selected_epoch"].is_null(),
                    "unexpected missing selection"
                );
                rows.push(json!({"candidate":name,"status":"no_eligible_epoch"}));
                continue;
            };
            ensure!(
                c["status"] == "fit_and_calibration_complete" && t["selected_epoch"] == index + 1,
                "incorrect selected epoch"
            );
            let manifest = read(&dir.join("manifest.json"))?;
            for (name, hash) in manifest["files"]
                .as_object()
                .context("missing artifact files")?
            {
                let path = Path::new(name);
                ensure!(
                    !path.is_absolute()
                        && path.components().all(|c| matches!(c, Component::Normal(_))),
                    "unsafe bound file"
                );
                ensure!(
                    digest(&dir.join(path))? == hash.as_str().context("invalid artifact hash")?,
                    "changed artifact file"
                );
            }
            let row = json!({"candidate":name,"representation":mode,"hidden_width":width,"parameters":t["parameters"],
                "selected_epoch":index+1,"development_binary_nll":loss,
                "selected_train_binary_nll":history[index]["train_human_model"]["log_loss"],
                "final_train_binary_nll":history[29]["train_human_model"]["log_loss"],
                "final_development_binary_nll":history[29]["development_human_model"]["log_loss"],
                "selected_development":history[index]["development_human_model"],
                "artifact_manifest_sha256":digest(&dir.join("manifest.json"))?,"training_report_sha256":digest(&dir.join("training.json"))?});
            if best
                .get(mode)
                .is_none_or(|r| loss < r["development_binary_nll"].as_f64().unwrap())
            {
                best.insert(mode.into(), row.clone());
            }
            rows.push(row);
        }
    }
    Ok(
        json!({"schema":"slop_ninja_feature_grid_summary_v1","bindings_sha256":digest(&root.join("bindings.sha256"))?,
        "candidates":rows,"best_per_representation_by_development_only":best,"diagnostic_selection":false,
        "qualification":"single-seed research comparison; no reference promotion or external transfer qualification"}),
    )
}
fn thresholds(path: &Path) -> Result<Value> {
    let responses = response_map(path)?;
    let artifact = text(responses.values().next().unwrap(), "artifact_id")?;
    let mut rows = vec![];
    for (id, r) in &responses {
        ensure!(r["artifact_id"] == artifact, "mixed artifact responses");
        let label = match text(r, "origin")? {
            "human_only" => 0,
            "model_only" => 1,
            "mixed" => 2,
            _ => anyhow::bail!("invalid origin"),
        };
        rows.push(ProbabilityRow {
            id: id.clone(),
            group: text(r, "source_group")?.into(),
            label,
            probabilities: probabilities(r)?,
        });
    }
    ensure!(
        rows.len() == 324 && rows.iter().filter(|r| r.label == 0).count() == 108,
        "expected complete Calibration R2 view"
    );
    let points = metrics::select_operating_points(&rows, &[0.01, 0.05])?;
    Ok(
        json!({"schema":"slop_ninja_feature_operating_points_v1","artifact_id":artifact,"calibration_probabilities_sha256":digest(path)?,"points":points}),
    )
}
fn dot(gradient: &[Value], vector: &Value) -> Result<f64> {
    let mut value = 0.;
    for pair in vector["vector"]["values"]
        .as_array()
        .context("missing sparse values")?
    {
        let i = pair[0].as_u64().context("invalid coordinate")? as usize;
        value += gradient
            .get(i)
            .and_then(Value::as_f64)
            .context("missing gradient coordinate")?
            * pair[1].as_f64().context("invalid feature")?;
    }
    ensure!(value.is_finite(), "nonfinite gradient estimate");
    Ok(value)
}
fn gradient_summary(details: &[Value]) -> Result<Value> {
    let sign = |x: f64| {
        if x.abs() <= 1e-8 {
            0
        } else if x > 0. {
            1
        } else {
            -1
        }
    };
    let edited: Vec<_> = details
        .iter()
        .filter(|r| {
            r["text_sha256"] != r["source_sha256"] && r.get("estimated_margin_change").is_some()
        })
        .collect();
    let mut direction = BTreeMap::<String, usize>::new();
    let mut rankings = BTreeMap::<String, usize>::new();
    let relation = |a: i32, b: i32| {
        if a == 0 && b == 0 {
            "both_tied"
        } else if a == 0 || b == 0 {
            "one_tied"
        } else if a == b {
            "agree"
        } else {
            "disagree"
        }
    };
    let mut absolute_error = 0.;
    for (i, r) in edited.iter().enumerate() {
        let estimate = number(r, "estimated_margin_change")?;
        let actual = number(r, "actual_margin_change")?;
        absolute_error += (estimate - actual).abs();
        *direction
            .entry(relation(sign(estimate), sign(actual)).into())
            .or_default() += 1;
        for other in &edited[i + 1..] {
            if r["source_sha256"] == other["source_sha256"] {
                let a = sign(estimate - number(other, "estimated_margin_change")?);
                let b = sign(actual - number(other, "actual_margin_change")?);
                *rankings.entry(relation(a, b).into()).or_default() += 1;
            }
        }
    }
    Ok(
        json!({"edited_states":edited.len(),"direction_counts":direction,"within_source_pair_counts":rankings,
        "mean_absolute_margin_error":if edited.is_empty(){None}else{Some(absolute_error/edited.len() as f64)},"tie_tolerance":1e-8,
        "meaning_preservation_inferred":false,"external_gradient_transfer_measured":false}),
    )
}
fn diagnostic(
    source: &Path,
    kind: &str,
    response_path: &Path,
    threshold_path: &Path,
    projections: Option<&Path>,
) -> Result<Value> {
    ensure!(
        matches!(kind, "hosted" | "grammar"),
        "unsupported diagnostic kind"
    );
    let plan = read(&source.join("plan.json"))?;
    let request_path = source.join("requests.jsonl");
    ensure!(
        plan["requests_sha256"] == digest(&request_path)?,
        "changed diagnostic requests"
    );
    let requests = lines(&request_path)?;
    let mut ids = BTreeSet::new();
    for r in &requests {
        let id = text(r, "id")?;
        ensure!(
            id == sha256(text(r, "text")?) && ids.insert(id.to_owned()),
            "changed or repeated source request"
        );
    }
    let meta = if kind == "hosted" {
        ensure!(
            plan["prior_comparison_sha256"] == digest(&source.join("prior-comparison.json"))?,
            "changed hosted metadata"
        );
        read(&source.join("prior-comparison.json"))?
    } else {
        plan["rows"].clone()
    };
    let responses = response_map(response_path)?;
    let baseline = response_map(&source.join("responses.jsonl"))?;
    ensure!(
        responses.keys().cloned().collect::<BTreeSet<_>>() == ids
            && baseline.keys().cloned().collect::<BTreeSet<_>>() == ids,
        "incomplete diagnostic responses"
    );
    let frozen = read(threshold_path)?;
    let artifact = text(&frozen, "artifact_id")?;
    let points: Vec<OperatingPoint> = serde_json::from_value(frozen["points"].clone())?;
    let baseline_points: Vec<f64> = serde_json::from_value(if kind == "hosted" {
        plan["v6_thresholds_strictly_greater_than"].clone()
    } else {
        plan["thresholds"].clone()
    })?;
    let mut projection_map = BTreeMap::new();
    if let Some(path) = projections {
        for p in lines(path)? {
            ensure!(p["status"] == "ok", "unsupported projection");
            let id = text(&p, "id")?.to_owned();
            ensure!(
                projection_map.insert(id, p).is_none(),
                "repeated projection"
            );
        }
        ensure!(
            projection_map.keys().cloned().collect::<BTreeSet<_>>() == ids,
            "incomplete projection coverage"
        );
    }
    let mut rows = vec![];
    let mut details = vec![];
    let mut slices: BTreeMap<String, Vec<ProbabilityRow>> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for m in meta.as_array().context("missing metadata rows")? {
        let id = text(m, "text_sha256")?;
        ensure!(seen.insert(id.to_owned()), "repeated metadata");
        let r = responses.get(id).context("missing response")?;
        let b = baseline.get(id).context("missing baseline")?;
        ensure!(
            r["artifact_id"] == artifact && r["text_sha256"] == id,
            "changed response identity"
        );
        ensure!(
            b["artifact_id"] == plan["artifact_id"],
            "changed baseline artifact"
        );
        let p = probabilities(r)?;
        let old = probabilities(b)?;
        let ai = p[1] + p[2];
        let old_ai = old[1] + old[2];
        let label = if kind == "hosted" {
            1
        } else {
            match text(m, "origin")? {
                "human_only" => 0,
                "model_only" => 1,
                _ => anyhow::bail!("invalid grammar origin"),
            }
        };
        let row = ProbabilityRow {
            id: id.into(),
            group: text(m, "source_group")?.into(),
            label,
            probabilities: p,
        };
        let mut d = json!({"text_sha256":id,"source_group":row.group,"origin_label":label,"candidate_probability":ai,"baseline_probability":old_ai,
            "candidate_flags":points.iter().map(|t|ai>t.threshold).collect::<Vec<_>>(),"baseline_flags":baseline_points.iter().map(|t|old_ai>*t).collect::<Vec<_>>()});
        if kind == "hosted" {
            let human = number(m, "fraction_human")?;
            let ai_fraction = number(m, "fraction_ai")?;
            let assisted = number(m, "fraction_ai_assisted")?;
            ensure!(
                [human, ai_fraction, assisted]
                    .iter()
                    .all(|x| (0.0..=1.0).contains(x))
                    && (human + ai_fraction + assisted - 1.).abs() < 1e-5,
                "invalid provider fraction"
            );
            d["pangram_fraction"] = json!(ai_fraction + assisted);
            d["pangram_flag"] = json!(ai_fraction + assisted >= 0.1);
            for k in ["profile", "requested_model"] {
                d[k] = m[k].clone();
                slices
                    .entry(format!("{k}:{}", text(m, k)?))
                    .or_default()
                    .push(row.clone());
            }
        } else {
            let source_id = text(m, "source_sha256")?;
            let control = responses
                .get(source_id)
                .context("missing unchanged source")?;
            let control_p = probabilities(control)?;
            let old_control =
                probabilities(baseline.get(source_id).context("missing old control")?)?;
            d["source_sha256"] = json!(source_id);
            d["mask"] = m["mask"].clone();
            d["judgment"] = m["judgment"].clone();
            d["candidate_edit_change_pp"] = json!(100. * (ai - control_p[1] - control_p[2]));
            d["baseline_edit_change_pp"] = json!(100. * (old_ai - old_control[1] - old_control[2]));
            d["actual_margin_change"] = json!(
                number(r, "uncalibrated_model_origin_margin")?
                    - number(control, "uncalibrated_model_origin_margin")?
            );
            if !projection_map.is_empty() {
                let g = control["gradient"]
                    .as_array()
                    .context("missing control gradient")?;
                ensure!(
                    projection_map[id]["feature_space_sha256"] == r["feature_space_sha256"]
                        && projection_map[id]["text_sha256"] == id,
                    "changed projected identity"
                );
                d["estimated_margin_change"] =
                    json!(dot(g, &projection_map[id])? - dot(g, &projection_map[source_id])?);
            }
            slices
                .entry(format!("judgment:{}", text(m, "judgment")?))
                .or_default()
                .push(row.clone());
        }
        rows.push(row);
        details.push(d);
    }
    ensure!(seen == ids, "incomplete diagnostic metadata");
    let slice_reports = slices
        .iter()
        .map(|(k, v)| {
            Ok((
                k.clone(),
                metrics::evaluate_probability_rows(artifact, v, [1. / 3.; 3], &points)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let hosted_counts = if kind == "hosted" {
        json!({"pangram_flagged":details.iter().filter(|r|r["pangram_flag"]==true).count(),
        "candidate_flagged":(0..points.len()).map(|i|details.iter().filter(|r|r["candidate_flags"][i]==true).count()).collect::<Vec<_>>(),
        "baseline_flagged":(0..baseline_points.len()).map(|i|details.iter().filter(|r|r["baseline_flags"][i]==true).count()).collect::<Vec<_>>(),
        "candidate_provider_flag_agreement":(0..points.len()).map(|i|details.iter().filter(|r|r["candidate_flags"][i]==r["pangram_flag"]).count()).collect::<Vec<_>>()})
    } else {
        Value::Null
    };
    Ok(
        json!({"schema":"slop_ninja_feature_diagnostic_v1","kind":kind,"artifact_id":artifact,"requests_sha256":digest(&request_path)?,
        "responses_sha256":digest(response_path)?,"thresholds_sha256":digest(threshold_path)?,"baseline_responses_sha256":digest(&source.join("responses.jsonl"))?,
        "metrics":metrics::evaluate_probability_rows(artifact,&rows,[1./3.;3],&points)?,"slices":slice_reports,"hosted_counts":hosted_counts,
        "gradient_check":if kind=="grammar"{gradient_summary(&details)?}else{Value::Null},"details":details,
        "qualification":"previously inspected diagnostics; no provider calls, no new model or threshold selection"}),
    )
}
fn main() -> Result<()> {
    let (output, result) = match Cli::parse().command {
        Command::Grid { root, output } => (output, grid(&root)?),
        Command::Thresholds {
            probabilities,
            output,
        } => (output, thresholds(&probabilities)?),
        Command::Diagnostic {
            source,
            kind,
            responses,
            thresholds,
            projections,
            output,
        } => (
            output,
            diagnostic(
                &source,
                &kind,
                &responses,
                &thresholds,
                projections.as_deref(),
            )?,
        ),
    };
    write(&output, &result)
}
