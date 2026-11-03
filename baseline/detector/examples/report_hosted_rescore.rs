//! Compare frozen V6 CPU responses with exact-text V5/Pangram observations.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    prepared: PathBuf,
    #[arg(long)]
    responses: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
fn probability(v: &Value) -> Result<f64> {
    let p = v.as_f64().context("Missing probability")?;
    ensure!(
        p.is_finite() && (0.0..=1.0).contains(&p),
        "Invalid probability"
    );
    Ok(p)
}
fn jsonl(bytes: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(bytes)?
        .lines()
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}
fn save(path: &Path, value: &Value) -> Result<()> {
    let mut file = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
fn counts(rows: &[&Value]) -> Value {
    let count = |key: &str| rows.iter().filter(|r| r[key] == true).count();
    let mut groups = BTreeSet::new();
    for row in rows {
        groups.insert(row["source_group"].as_str().unwrap());
    }
    json!({"records":rows.len(),"source_groups":groups.len(),
        "pangram_flags":count("pangram_flag"),"v5_strict_flags":count("v5_strict"),
        "v5_loose_flags":count("v5_loose"),"v6_strict_flags":count("v6_strict"),
        "v6_loose_flags":count("v6_loose"),"v6_flags_at_half":count("v6_half"),
        "pangram_flagged_but_v6_below_10":rows.iter().filter(|r| r["pangram_flag"] == true && r["v6_probability"].as_f64().unwrap() < 0.1).count(),
        "paired_v6_strict":{
            "both":rows.iter().filter(|r| r["pangram_flag"] == true && r["v6_strict"] == true).count(),
            "pangram_only":rows.iter().filter(|r| r["pangram_flag"] == true && r["v6_strict"] == false).count(),
            "v6_only":rows.iter().filter(|r| r["pangram_flag"] == false && r["v6_strict"] == true).count(),
            "neither":rows.iter().filter(|r| r["pangram_flag"] == false && r["v6_strict"] == false).count()},
        "human_false_positive_rate":null})
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new output directory");
    let plan_bytes = fs::read(args.prepared.join("plan.json"))?;
    let plan: Value = serde_json::from_slice(&plan_bytes)?;
    ensure!(
        plan["schema"] == "slop_ninja_hosted_rescore_v1"
            && plan["artifact_id"]
                == "4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138"
            && plan["v6_thresholds_strictly_greater_than"]
                == json!([0.8436493155285434, 0.4287369114260753])
            && plan["pangram_flag_at_least"] == 0.1,
        "Changed evaluation policy"
    );
    let requests = fs::read(args.prepared.join("requests.jsonl"))?;
    let previous = fs::read(args.prepared.join("prior-comparison.json"))?;
    ensure!(
        plan["requests_sha256"] == sha256(&requests)
            && plan["prior_comparison_sha256"] == sha256(&previous),
        "Changed prepared inputs"
    );
    let prior: Vec<Value> = serde_json::from_slice(&previous)?;
    let requests = jsonl(&requests)?;
    let response_bytes = fs::read(&args.responses)?;
    let scores = jsonl(&response_bytes)?;
    ensure!(
        plan["records"] == requests.len()
            && prior.len() == requests.len()
            && scores.len() == requests.len(),
        "Incomplete response coverage"
    );
    let mut index = BTreeMap::new();
    for request in &requests {
        let id = request["id"].as_str().context("Missing ID")?;
        ensure!(
            sha256(request["text"].as_str().context("Missing text")?) == id
                && index.insert(id, request).is_none(),
            "Changed or duplicate request"
        );
    }
    let mut seen = BTreeSet::new();
    let mut rows = vec![];
    for score in &scores {
        let id = score["id"].as_str().context("Missing response ID")?;
        ensure!(
            index.contains_key(id) && seen.insert(id),
            "Unexpected or duplicate response"
        );
        ensure!(
            score["status"] == "ok"
                && score["artifact_id"] == plan["artifact_id"]
                && score["runtime"] == "cpu_float32_eager"
                && score["labels"] == json!(["human_only", "model_only", "mixed"])
                && score["token_count"]
                    .as_u64()
                    .is_some_and(|n| (1..=2048).contains(&n)),
            "Failed, unsupported or noncanonical response; keep its original evidence"
        );
        let p = probability(&score["model_or_assistance_probability"])?;
        let probs: Vec<f64> = score["probabilities"]
            .as_array()
            .context("Missing classes")?
            .iter()
            .map(probability)
            .collect::<Result<_>>()?;
        ensure!(
            probs.len() == 3
                && (probs.iter().sum::<f64>() - 1.0).abs() < 1e-6
                && (p - probs[1] - probs[2]).abs() < 1e-6,
            "Inconsistent probabilities"
        );
        let matching: Vec<_> = prior.iter().filter(|r| r["text_sha256"] == id).collect();
        ensure!(matching.len() == 1, "Missing or repeated prior score");
        let old = matching[0];
        let v5 = probability(&old["v5_combined_probability"])?;
        let prior_combined = old["pangram_combined_fraction"]
            .as_f64()
            .context("Missing prior Pangram sum")?;
        let human = probability(&old["fraction_human"])?;
        // Provider float32 components can sum slightly above one. Retain that
        // raw sum, validate it, and score using the bounded human complement.
        let pangram = 1.0 - human;
        let ai = probability(&old["fraction_ai"])?;
        let assisted = probability(&old["fraction_ai_assisted"])?;
        ensure!(
            prior_combined.is_finite()
                && (ai + assisted + human - 1.0).abs() < 1e-5
                && (prior_combined - pangram).abs() < 1e-5,
            "Inconsistent Pangram fractions"
        );
        for key in ["source_group", "requested_model", "profile", "collection"] {
            ensure!(
                old[key].as_str().is_some_and(|s| !s.is_empty()),
                "Missing prior provenance"
            );
        }
        rows.push(json!({"text_sha256":id,"source_group":old["source_group"],
            "requested_model":old["requested_model"],"profile":old["profile"],"collection":old["collection"],
            "v6_probability":p,"v5_probability":v5,"pangram_flagged_fraction":pangram,
            "pangram_prior_component_sum":prior_combined,
            "pangram_flag":pangram >= 0.1,"v5_strict":v5 > 0.9052925262799454,
            "v5_loose":v5 > 0.8884499969916239,"v6_strict":p > 0.8436493155285434,
            "v6_loose":p > 0.4287369114260753,"v6_half":p > 0.5}));
    }
    let mut slices = BTreeMap::new();
    for key in ["requested_model", "profile", "collection"] {
        let mut groups = BTreeMap::<&str, Vec<&Value>>::new();
        for row in &rows {
            groups
                .entry(row[key].as_str().unwrap())
                .or_default()
                .push(row);
        }
        for (value, group) in groups {
            slices.insert(format!("{key}:{value}"), counts(&group));
        }
    }
    let result = json!({"schema":"slop_ninja_hosted_v6_diagnostic_v1","artifact_id":plan["artifact_id"],
        "plan_sha256":sha256(&plan_bytes),"responses_sha256":sha256(&response_bytes),
        "prior_comparison_sha256":plan["prior_comparison_sha256"],
        "overall":counts(&rows.iter().collect::<Vec<_>>()),"slices":slices,
        "v6_thresholds":plan["v6_thresholds_strictly_greater_than"],"v5_thresholds":[0.9052925262799454,0.8884499969916239],
        "provider_calls":0,"new_training":false,"commercial_training_admitted":false,
        "pangram_fraction_policy":"One minus fraction_human, with the prior raw component sum retained and checked within 1e-5.",
        "limits":["All-model texts on 24 previously trained source families; no fresh-source or false-positive estimate.",
            "Different operating thresholds are not matched population false-positive rates.",
            "Pangram content fractions and A probabilities have different meanings.",
            "Profile slices use different source families; this is not a causal style comparison."]});
    fs::create_dir_all(&args.out)?;
    save(&args.out.join("per-record.json"), &json!(rows))?;
    save(&args.out.join("summary.json"), &result)?;
    println!("{}", result["overall"]);
    Ok(())
}
