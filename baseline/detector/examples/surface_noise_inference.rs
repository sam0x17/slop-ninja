//! Score the reviewed, frozen surface-noise panel without changing any detector.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const PREPARATION: &str = "e167257243e780d610d3f3215beaed7334cfc1b05eab144371d35b2cff1eb89e";
const REVIEW: &str = "dfb5ee93c75211b1baee1b93bcfa574c4a75bb0845d73517f2c4275c6ba5f6c9";
#[derive(Parser)]
struct Args {
    #[arg(long)]
    root: PathBuf,
}
fn read(path: impl AsRef<Path>) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn hash(path: impl AsRef<Path>) -> Result<String> {
    Ok(sha256(fs::read(path)?))
}
fn save(path: impl AsRef<Path>, v: &Value) -> Result<()> {
    let mut f = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn verify(bindings: &Value) -> Result<()> {
    for (path, expected) in bindings.as_object().context("Bindings")? {
        ensure!(
            hash(path)? == expected.as_str().context("Expected hash")?,
            "Changed bound input {path}"
        );
    }
    Ok(())
}
fn origin(row: &Value) -> usize {
    match row["origin"].as_str().unwrap() {
        "human_only" => 0,
        "model_only" => 1,
        "mixed" => 2,
        _ => unreachable!(),
    }
}
fn argmax(p: &[f64; 3]) -> usize {
    (1..3).fold(0, |best, i| if p[i] > p[best] { i } else { best })
}
fn measured(mut row: Value, p: [[f64; 3]; 2]) -> Value {
    let y = origin(&row);
    let ai = p.map(|v| v[1] + v[2]);
    row["probabilities"] = json!(p);
    row["ai"] = json!(ai);
    row["delta_ai"] = json!(ai[1] - ai[0]);
    row["binary_nll"] = json!(ai.map(|a| -(if y == 0 { 1.0 - a } else { a }).max(1e-15).ln()));
    row["class_nll"] = json!(p.map(|v| -v[y].max(1e-15).ln()));
    row["binary_accuracy_at_half"] =
        json!(ai.map(|a| if (a > 0.5) == (y != 0) { 1.0 } else { 0.0 }));
    row["class_accuracy"] = json!(p.map(|v| if argmax(&v) == y { 1.0 } else { 0.0 }));
    row
}
fn family_mean(rows: &[Value], value: impl Fn(&Value) -> f64) -> f64 {
    let mut groups = BTreeMap::<&str, (f64, usize)>::new();
    for row in rows {
        let e = groups.entry(row["group"].as_str().unwrap()).or_default();
        e.0 += value(row);
        e.1 += 1;
    }
    groups
        .values()
        .map(|(sum, n)| sum / (*n as f64))
        .sum::<f64>()
        / groups.len() as f64
}
fn summarize(rows: &[Value], thresholds: &[Value]) -> Value {
    let delta = |r: &Value| r["delta_ai"].as_f64().unwrap();
    let mut means = json!({});
    let mut equal_family_means = json!({});
    for field in [
        "ai",
        "binary_nll",
        "class_nll",
        "binary_accuracy_at_half",
        "class_accuracy",
    ] {
        means[field] = json!(
            (0..2)
                .map(|i| rows
                    .iter()
                    .map(|r| r[field][i].as_f64().unwrap())
                    .sum::<f64>()
                    / rows.len() as f64)
                .collect::<Vec<_>>()
        );
        equal_family_means[field] = json!(
            (0..2)
                .map(|i| family_mean(rows, |r| r[field][i].as_f64().unwrap()))
                .collect::<Vec<_>>()
        );
    }
    let cutoffs: Vec<_>=thresholds.iter().map(|c| {
        let t=c["threshold"].as_f64().unwrap(); let flag=|r:&Value,i:usize|r["ai"][i].as_f64().unwrap()>t;
        json!({"threshold":t,"baseline_flagged":rows.iter().filter(|r|flag(r,0)).count(),"candidate_flagged":rows.iter().filter(|r|flag(r,1)).count(),"flag_to_clear":rows.iter().filter(|r|flag(r,0)&&!flag(r,1)).count(),"clear_to_flag":rows.iter().filter(|r|!flag(r,0)&&flag(r,1)).count(),"human_denominator":rows.iter().filter(|r|origin(r)==0).count(),"model_involved_denominator":rows.iter().filter(|r|origin(r)!=0).count(),"false_positives":(0..2).map(|i|rows.iter().filter(|r|origin(r)==0&&flag(r,i)).count()).collect::<Vec<_>>(),"true_positives":(0..2).map(|i|rows.iter().filter(|r|origin(r)!=0&&flag(r,i)).count()).collect::<Vec<_>>()})
    }).collect();
    json!({"rows":rows.len(),"families":rows.iter().map(|r|r["group"].as_str().unwrap()).collect::<BTreeSet<_>>().len(),"mean_delta_ai":rows.iter().map(delta).sum::<f64>()/rows.len() as f64,"mean_absolute_delta_ai":rows.iter().map(|r|delta(r).abs()).sum::<f64>()/rows.len() as f64,"equal_family_mean_delta_ai":family_mean(rows,delta),"equal_family_mean_absolute_delta_ai":family_mean(rows,|r|delta(r).abs()),"max_absolute_delta_ai":rows.iter().map(|r|delta(r).abs()).fold(0.0,f64::max),"paired_means":means,"equal_family_paired_means":equal_family_means,"cutoffs":cutoffs})
}
fn slices(rows: &[Value], fields: &[&str], thresholds: &[Value]) -> Vec<Value> {
    let mut groups = BTreeMap::<Vec<String>, Vec<Value>>::new();
    for row in rows {
        groups
            .entry(
                fields
                    .iter()
                    .map(|k| row[*k].as_str().unwrap().to_owned())
                    .collect(),
            )
            .or_default()
            .push(row.clone());
    }
    groups
        .into_iter()
        .map(|(keys, values)| {
            let mut result = json!({"summary":summarize(&values,thresholds)});
            for (k, v) in fields.iter().zip(keys) {
                result[*k] = json!(v);
            }
            result
        })
        .collect()
}
fn main() -> Result<()> {
    let args = Args::parse();
    let root = args.root;
    ensure!(
        hash(root.join("preparation.json"))? == PREPARATION
            && hash(root.join("manual-review.json"))? == REVIEW,
        "Changed prepared or reviewed panel"
    );
    let plan = read(root.join("preparation.json"))?;
    let review = read(root.join("manual-review.json"))?;
    ensure!(
        review["remaining_unreviewed"] == 0
            && review["reviewed_pair_count"] == 386
            && review["ready_for_origin_diagnostic"] == true,
        "Review incomplete"
    );
    let preflight = read(root.join("inference-preflight.json"))?;
    ensure!(
        preflight["ready"] == true
            && preflight["runner_sha256"] == hash(std::env::current_exe()?)?
            && preflight["preparation_sha256"] == PREPARATION
            && preflight["manual_review_sha256"] == REVIEW,
        "Changed execution preparation"
    );
    let preflight_hash = hash(root.join("inference-preflight.json"))?;
    let out = root.join("inference");
    fs::create_dir(&out).context("Use a fresh inference directory; preserve earlier attempts")?;
    save(
        out.join("started.json"),
        &json!({"at":chrono::Utc::now(),"pid":std::process::id(),"preparation_sha256":PREPARATION,"manual_review_sha256":REVIEW,"preflight_sha256":preflight_hash,"source_sha256":sha256(include_bytes!("surface_noise_inference.rs"))}),
    )?;
    let outcome = (|| -> Result<Vec<Value>> {
        verify(&plan["artifact_bindings"])?;
        ensure!(
            hash(root.join("requests.jsonl"))? == plan["requests_sha256"],
            "Changed requests"
        );
        let requests: Vec<Value> = fs::read_to_string(root.join("requests.jsonl"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<serde_json::Result<_>>()?;
        ensure!(
            requests.len() == 512
                && requests
                    .iter()
                    .map(|r| r["id"].as_str())
                    .collect::<BTreeSet<_>>()
                    .len()
                    == 512,
            "Request coverage"
        );
        let pairs = plan["pairs"].as_array().context("Pairs")?;
        ensure!(pairs.len() == 386, "Pair coverage");
        let candidates = plan["candidates"].as_array().context("Candidates")?;
        ensure!(
            candidates
                .iter()
                .map(|c| c["role"].as_str())
                .collect::<Vec<_>>()
                == [Some("v6"), Some("control"), Some("consistency")],
            "Candidate order"
        );
        let mut results = Vec::new();
        for candidate in candidates {
            let role = candidate["role"].as_str().context("Role")?;
            let dir = out.join(role);
            fs::create_dir(&dir)?;
            let artifact = PathBuf::from(candidate["artifact"].as_str().context("Artifact")?);
            let status = Command::new(plan["python"].as_str().context("Python")?)
                .arg(artifact.join("runner/infer.py"))
                .arg("--artifact")
                .arg(&artifact)
                .env("HF_HUB_OFFLINE", "1")
                .env("TRANSFORMERS_OFFLINE", "1")
                .env("OMP_NUM_THREADS", "1")
                .env("OPENBLAS_NUM_THREADS", "1")
                .stdin(fs::File::open(root.join("requests.jsonl"))?)
                .stdout(fs::File::create_new(dir.join("responses.jsonl"))?)
                .stderr(Stdio::from(fs::File::create_new(
                    dir.join("inference.log"),
                )?))
                .status()?;
            save(
                dir.join("exit.json"),
                &json!({"code":status.code(),"success":status.success()}),
            )?;
            ensure!(status.success(), "Inference failed for {role}");
            let responses: Vec<Value> = fs::read_to_string(dir.join("responses.jsonl"))?
                .lines()
                .map(serde_json::from_str)
                .collect::<serde_json::Result<_>>()?;
            ensure!(responses.len() == requests.len(), "Response coverage");
            let mut by_id = BTreeMap::new();
            for (response, request) in responses.iter().zip(&requests) {
                ensure!(
                    response["id"] == request["id"]
                        && response["status"] == "ok"
                        && response["artifact_id"] == candidate["artifact_id"]
                        && response["runtime"] == "cpu_float32_eager"
                        && response["labels"] == json!(["human_only", "model_only", "mixed"])
                        && response["token_count"]
                            .as_u64()
                            .is_some_and(|n| n > 0 && n <= 2048),
                    "Changed or unsupported response"
                );
                let p: [f64; 3] = serde_json::from_value(response["probabilities"].clone())?;
                ensure!(
                    p.iter().all(|v| v.is_finite() && *v >= 0.0 && *v <= 1.0)
                        && (p.iter().sum::<f64>() - 1.0).abs() < 1e-8,
                    "Invalid distribution"
                );
                by_id.insert(response["id"].as_str().unwrap(), p);
            }
            let rows: Vec<Value> = pairs
                .iter()
                .map(|row| {
                    let ids = row["request_ids"].as_array().context("Pair requests")?;
                    let a = *by_id
                        .get(ids[0].as_str().context("ID")?)
                        .context("Missing baseline")?;
                    let b = *by_id
                        .get(ids[1].as_str().context("ID")?)
                        .context("Missing candidate")?;
                    Ok(measured(row.clone(), [a, b]))
                })
                .collect::<Result<_>>()?;
            let thresholds = candidate["thresholds"].as_array().context("Thresholds")?;
            let report = json!({"role":role,"artifact_id":candidate["artifact_id"],"temperature":candidate["temperature"],"responses_sha256":hash(dir.join("responses.jsonl"))?,"loss_probability_floor":1e-15,"view_order":["baseline","candidate"],"by_kind":slices(&rows,&["kind"],thresholds),"by_kind_origin":slices(&rows,&["kind","origin"],thresholds),"by_kind_origin_collection":slices(&rows,&["kind","origin","collection"],thresholds),"rows":rows});
            save(dir.join("report.json"), &report)?;
            results.push(json!({"role":role,"report":dir.join("report.json"),"sha256":hash(dir.join("report.json"))?}));
        }
        verify(&plan["artifact_bindings"])?;
        ensure!(
            hash(root.join("preparation.json"))? == PREPARATION
                && hash(root.join("manual-review.json"))? == REVIEW
                && hash(root.join("requests.jsonl"))? == plan["requests_sha256"]
                && hash(root.join("inference-preflight.json"))? == preflight_hash,
            "Inputs changed during inference"
        );
        Ok(results)
    })();
    save(
        out.join("completion.json"),
        &json!({"at":chrono::Utc::now(),"analysis_complete":outcome.is_ok(),"reports":outcome.as_ref().ok(),"error":outcome.as_ref().err().map(|e|format!("{e:#}")),"provider_calls":0,"reference_promoted":false,"fresh_confirmation":false}),
    )?;
    outcome.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(group: &str, origin: &str, a: f64, b: f64) -> Value {
        measured(
            json!({"group":group,"origin":origin}),
            [[1.0 - a, a, 0.0], [1.0 - b, b, 0.0]],
        )
    }
    #[test]
    fn constant_high_predictions_have_zero_delta_but_human_errors() {
        let rows = vec![
            row("a", "human_only", 0.8, 0.8),
            row("b", "model_only", 0.8, 0.8),
        ];
        let s = summarize(&rows, &[json!({"threshold":0.5})]);
        assert_eq!(s["mean_absolute_delta_ai"], 0.0);
        assert_eq!(
            s["paired_means"]["binary_accuracy_at_half"],
            json!([0.5, 0.5])
        );
        assert_eq!(s["cutoffs"][0]["false_positives"], json!([1, 1]));
        assert!(s["paired_means"]["binary_nll"][0].as_f64().unwrap() > 0.9);
    }
    #[test]
    fn opposite_changes_do_not_cancel_absolute_sensitivity() {
        let rows = vec![
            row("a", "human_only", 0.1, 0.9),
            row("b", "model_only", 0.9, 0.1),
        ];
        let s = summarize(&rows, &[json!({"threshold":0.5})]);
        assert!(s["mean_delta_ai"].as_f64().unwrap().abs() < 1e-12);
        assert!((s["mean_absolute_delta_ai"].as_f64().unwrap() - 0.8).abs() < 1e-12);
        assert_eq!(s["cutoffs"][0]["flag_to_clear"], 1);
        assert_eq!(s["cutoffs"][0]["clear_to_flag"], 1);
    }
}
