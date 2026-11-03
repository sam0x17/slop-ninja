//! Fixed-model paired boundary-space diagnostic on an exposed Train panel.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{Origin, Split, read_records, sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

const TRAIN_SHA: &str = "023d6e5428236502e877bac0afe0e9e9ee4aa9d10ab38eb940ca8e915cf7502e";
const FIT_PLAN_SHA: &str = "acd45ba5a1f7853e5460828973574f333cf4323cd94acde23615b979e1daca2a";
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    task: Task,
}
#[derive(Subcommand)]
enum Task {
    Prepare {
        #[arg(long)]
        train: PathBuf,
        #[arg(long)]
        fit_plan: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    Run {
        #[arg(long)]
        plan: PathBuf,
    },
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn save(path: &Path, v: &Value) -> Result<()> {
    let mut f = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("Missing {k}"))
}
fn verify(bindings: &Value) -> Result<()> {
    for (p, h) in bindings.as_object().context("Missing bindings")? {
        ensure!(
            sha256(fs::read(p)?) == h.as_str().context("Invalid binding")?,
            "Changed binding {p}"
        );
    }
    Ok(())
}
fn views(text: &str) -> [String; 2] {
    let trimmed = text.trim();
    [trimmed.to_owned(), format!(" {trimmed}")]
}
fn prepare(train: PathBuf, fit_plan: PathBuf, out: PathBuf) -> Result<()> {
    ensure!(!out.exists(), "Use a new study directory");
    ensure!(
        sha256(fs::read(&train)?) == TRAIN_SHA && sha256(fs::read(&fit_plan)?) == FIT_PLAN_SHA,
        "Changed frozen inputs"
    );
    let fit = read(&fit_plan)?;
    verify(&fit["bindings"])?;
    let mut records = read_records(&train)?;
    records.sort_by(|a, b| a.id.cmp(&b.id));
    ensure!(
        records.len() == 3833 && records.iter().all(|r| r.split == Some(Split::Train)),
        "Wrong Train coverage"
    );
    let mut roots = BTreeMap::<String, Vec<(String, String)>>::new();
    let mut collection = BTreeMap::new();
    for r in records.iter().filter(|r| r.origin == Origin::HumanOnly) {
        ensure!(
            collection
                .insert(r.source_group.clone(), r.source.collection.clone())
                .is_none(),
            "Multiple roots"
        );
        let rank = sha256(serde_json::to_vec(&(
            "slop-ninja-human-train-register-v1",
            &r.source.collection,
            &r.source_group,
            &r.id,
        ))?);
        roots
            .entry(r.source.collection.clone())
            .or_default()
            .push((rank, r.id.clone()));
    }
    ensure!(
        collection.len() == 471 && roots.len() == 4,
        "Wrong root coverage"
    );
    let mut selected_roots = BTreeSet::new();
    for entries in roots.values_mut() {
        entries.sort();
        ensure!(entries.len() >= 4, "Small collection");
        selected_roots.extend(entries.iter().take(4).map(|(_, id)| id.clone()));
    }
    let families: BTreeSet<_> = records
        .iter()
        .filter(|r| selected_roots.contains(&r.id))
        .map(|r| r.source_group.clone())
        .collect();
    ensure!(families.len() == 16, "Wrong panel");
    let mut requests = String::new();
    let mut pairs = Vec::new();
    for r in records
        .iter()
        .filter(|r| families.contains(&r.source_group))
    {
        let text = views(&r.text);
        ensure!(!text[0].is_empty(), "Empty probe");
        let ids = [format!("{}:trim", r.id), format!("{}:space", r.id)];
        for i in 0..2 {
            requests.push_str(&serde_json::to_string(
                &json!({"id":ids[i],"text":text[i]}),
            )?);
            requests.push('\n');
        }
        pairs.push(json!({"id":r.id,"group":r.source_group,"origin":r.origin,"collection":collection[&r.source_group],"source_text_sha256":r.text_sha256,"request_ids":ids,"text_sha256":text.map(sha256)}));
    }
    fs::create_dir(&out)?;
    fs::write(out.join("requests.jsonl"), &requests)?;
    let mut bindings = fit["bindings"].as_object().context("Bindings")?.clone();
    bindings.insert(
        out.join("requests.jsonl").to_string_lossy().into_owned(),
        json!(sha256(&requests)),
    );
    bindings.insert(fit_plan.to_string_lossy().into_owned(), json!(FIT_PLAN_SHA));
    let plan = json!({"schema":"slop_ninja_boundary_space_plan_v1","output":out,"python":fit["python"],"candidates":fit["candidates"].as_array().context("Candidates")?.iter().map(|c|json!({"role":c["role"],"artifact":c["artifact"],"artifact_id":c["artifact_id"],"temperature":c["temperature"],"thresholds":c["thresholds"]})).collect::<Vec<_>>(),"pairs":pairs,"selected_root_ids":selected_roots,"bindings":bindings,"source_sha256":sha256(include_bytes!("boundary_space_probe.rs")),"protocol_sha256":sha256(include_bytes!("../BOUNDARY_SPACE_PROBE_V1.md")),"binary_sha256":sha256(fs::read(std::env::current_exe()?)?),"train_sha256":TRAIN_SHA,"new_training_records":0,"provider_calls":0});
    save(&out.join("plan.json"), &plan)?;
    println!("{} paired records from 16 families", pairs.len());
    Ok(())
}
fn summarize(rows: &[Value], thresholds: &[Value]) -> Value {
    let delta = |r: &Value| r["delta_ai"].as_f64().unwrap();
    let cells = rows
        .iter()
        .fold(BTreeMap::<(&str, &str), Vec<f64>>::new(), |mut a, r| {
            a.entry((r["group"].as_str().unwrap(), r["origin"].as_str().unwrap()))
                .or_default()
                .push(delta(r));
            a
        });
    json!({"rows":rows.len(),"family_origin_cells":cells.len(),"mean_delta_ai":rows.iter().map(delta).sum::<f64>()/rows.len() as f64,"mean_absolute_delta_ai":rows.iter().map(|r|delta(r).abs()).sum::<f64>()/rows.len() as f64,"equal_family_origin_mean_delta_ai":cells.values().map(|v|v.iter().sum::<f64>()/v.len() as f64).sum::<f64>()/cells.len() as f64,"equal_family_origin_mean_absolute_delta_ai":cells.values().map(|v|v.iter().map(|d|d.abs()).sum::<f64>()/v.len() as f64).sum::<f64>()/cells.len() as f64,"max_absolute_delta_ai":rows.iter().map(|r|delta(r).abs()).fold(0.0,f64::max),"cutoffs":thresholds.iter().map(|c|{let t=c["threshold"].as_f64().unwrap();let a=|r:&Value|r["ai"][0].as_f64().unwrap()>t;let b=|r:&Value|r["ai"][1].as_f64().unwrap()>t;json!({"threshold":t,"trim_flagged":rows.iter().filter(|r|a(r)).count(),"space_flagged":rows.iter().filter(|r|b(r)).count(),"flag_to_clear":rows.iter().filter(|r|a(r)&&!b(r)).count(),"clear_to_flag":rows.iter().filter(|r|!a(r)&&b(r)).count()})}).collect::<Vec<_>>()})
}
fn run(path: PathBuf) -> Result<()> {
    let plan_hash = sha256(fs::read(&path)?);
    let plan = read(&path)?;
    ensure!(
        plan["schema"] == "slop_ninja_boundary_space_plan_v1",
        "Wrong plan"
    );
    verify(&plan["bindings"])?;
    ensure!(
        plan["source_sha256"] == sha256(include_bytes!("boundary_space_probe.rs"))
            && plan["binary_sha256"] == sha256(fs::read(std::env::current_exe()?)?),
        "Changed probe implementation"
    );
    let out = PathBuf::from(string(&plan, "output")?);
    let pairs = plan["pairs"].as_array().context("Pairs")?;
    save(
        &out.join("started.json"),
        &json!({"pid":std::process::id(),"at":chrono::Utc::now(),"plan_sha256":sha256(fs::read(&path)?)}),
    )?;
    let result = (|| -> Result<Vec<Value>> {
        let candidates = plan["candidates"].as_array().context("Candidates")?;
        ensure!(
            candidates
                .iter()
                .map(|c| c["role"].as_str())
                .collect::<Vec<_>>()
                == [Some("v6"), Some("control"), Some("consistency")],
            "Wrong candidates"
        );
        let mut reports = Vec::new();
        for c in candidates {
            let role = string(c, "role")?;
            let dir = out.join(role);
            fs::create_dir(&dir)?;
            let artifact = PathBuf::from(string(c, "artifact")?);
            let status = Command::new(string(&plan, "python")?)
                .arg(artifact.join("runner/infer.py"))
                .arg("--artifact")
                .arg(&artifact)
                .env("HF_HUB_OFFLINE", "1")
                .env("TRANSFORMERS_OFFLINE", "1")
                .env("OMP_NUM_THREADS", "1")
                .env("OPENBLAS_NUM_THREADS", "1")
                .stdin(fs::File::open(out.join("requests.jsonl"))?)
                .stdout(fs::File::create_new(dir.join("responses.jsonl"))?)
                .stderr(fs::File::create_new(dir.join("inference.log"))?)
                .status()?;
            save(
                &dir.join("exit.json"),
                &json!({"code":status.code(),"success":status.success()}),
            )?;
            ensure!(
                status.success(),
                "Inference failed, preserve partial responses"
            );
            let responses = fs::read_to_string(dir.join("responses.jsonl"))?;
            let parsed: Vec<Value> = responses
                .lines()
                .map(serde_json::from_str)
                .collect::<serde_json::Result<_>>()?;
            ensure!(parsed.len() == pairs.len() * 2, "Response coverage");
            let mut rows = Vec::new();
            for (pair, values) in pairs.iter().zip(parsed.as_chunks::<2>().0) {
                let mut probs = Vec::new();
                for i in 0..2 {
                    let v = &values[i];
                    ensure!(
                        v["id"] == pair["request_ids"][i]
                            && v["artifact_id"] == c["artifact_id"]
                            && v["status"] == "ok"
                            && v["runtime"] == "cpu_float32_eager"
                            && v["token_count"]
                                .as_u64()
                                .is_some_and(|n| n > 0 && n <= 2048),
                        "Unsupported or changed observation"
                    );
                    let p: [f64; 3] = serde_json::from_value(v["probabilities"].clone())?;
                    ensure!(
                        p.iter().all(|x| x.is_finite() && *x >= 0.0 && *x <= 1.0)
                            && (p.iter().sum::<f64>() - 1.0).abs() < 1e-8,
                        "Invalid probabilities"
                    );
                    probs.push(p);
                }
                let ai = [probs[0][1] + probs[0][2], probs[1][1] + probs[1][2]];
                rows.push(json!({"id":pair["id"],"group":pair["group"],"origin":pair["origin"],"collection":pair["collection"],"text_sha256":pair["text_sha256"],"probabilities":probs,"ai":ai,"delta_ai":ai[1]-ai[0]}));
            }
            let keys: BTreeSet<_> = rows
                .iter()
                .map(|r| {
                    (
                        r["collection"].as_str().unwrap(),
                        r["origin"].as_str().unwrap(),
                    )
                })
                .collect();
            let thresholds = c["thresholds"].as_array().context("Thresholds")?;
            let slices:Vec<_>=keys.iter().map(|(collection,origin)|{let selected:Vec<_>=rows.iter().filter(|r|r["collection"]==*collection&&r["origin"]==*origin).cloned().collect();json!({"collection":collection,"origin":origin,"summary":summarize(&selected,thresholds)})}).collect();
            save(
                &dir.join("report.json"),
                &json!({"role":role,"artifact_id":c["artifact_id"],"temperature":c["temperature"],"responses_sha256":sha256(responses),"all":summarize(&rows,thresholds),"slices":slices,"rows":rows}),
            )?;
            reports.push(json!({"role":role,"report":dir.join("report.json"),"sha256":sha256(fs::read(dir.join("report.json"))?)}));
        }
        verify(&plan["bindings"])?;
        ensure!(
            sha256(fs::read(&path)?) == plan_hash,
            "Plan changed during run"
        );
        Ok(reports)
    })();
    save(
        &out.join("completion.json"),
        &json!({"at":chrono::Utc::now(),"analysis_complete":result.is_ok(),"reports":result.as_ref().ok(),"error":result.as_ref().err().map(|e|format!("{e:#}")),"provider_calls":0,"reference_promoted":false,"fresh_confirmation":false}),
    )?;
    result.map(|_| ())
}
fn main() -> Result<()> {
    match Args::parse().task {
        Task::Prepare {
            train,
            fit_plan,
            out,
        } => prepare(train, fit_plan, out),
        Task::Run { plan } => run(plan),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefix_intervention_preserves_all_interior_bytes() {
        for text in [
            " One\n\nparagraph.",
            "\u{a0}Café\tinside.\u{202f}",
            "No prefix.",
        ] {
            let pair = views(text);
            assert_eq!(pair[1].strip_prefix(' '), Some(pair[0].as_str()));
            assert_eq!(pair[0], text.trim());
        }
    }
    #[test]
    fn opposite_cutoff_crossings_do_not_cancel_absolute_change() {
        let rows = vec![
            json!({"group":"g","origin":"model_only","ai":[0.9,0.1],"delta_ai":-0.8}),
            json!({"group":"g","origin":"model_only","ai":[0.1,0.9],"delta_ai":0.8}),
        ];
        let s = summarize(&rows, &[json!({"threshold":0.5})]);
        assert_eq!(s["mean_delta_ai"], 0.0);
        assert_eq!(s["mean_absolute_delta_ai"], 0.8);
        assert_eq!(s["cutoffs"][0]["flag_to_clear"], 1);
        assert_eq!(s["cutoffs"][0]["clear_to_flag"], 1);
    }
}
