//! Validate exact-input V6 CPU responses for a frozen, fully enumerated pilot.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{collections::BTreeMap, fs, io::Write, path::Path};

pub const ARTIFACT: &str = "4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138";
pub const THRESHOLDS: [f64; 2] = [0.8436493155285434, 0.4287369114260753];
pub struct Policy {
    pub plan_schema: &'static str,
    pub report_schema: &'static str,
    pub expected_rows: usize,
    pub limitations: &'static str,
}
fn string(v: &Value) -> Result<&str> {
    v.as_str().context("Expected string")
}
fn array(v: &Value) -> Result<&Vec<Value>> {
    v.as_array().context("Expected array")
}
fn save(path: &Path, value: &Value) -> Result<()> {
    let mut f = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
pub fn report(prepared: &Path, responses: &Path, out: &Path, policy: &Policy) -> Result<()> {
    let plan_bytes = fs::read(prepared.join("plan.json"))?;
    let plan: Value = serde_json::from_slice(&plan_bytes)?;
    ensure!(
        plan["schema"] == policy.plan_schema
            && plan["artifact_id"] == ARTIFACT
            && plan["thresholds_strictly_greater_than"] == json!(THRESHOLDS),
        "Wrong evaluation policy"
    );
    for (path, hash) in plan["bindings"].as_object().context("Missing bindings")? {
        ensure!(
            sha256(fs::read(path)?) == string(hash)?,
            "Changed input {path}"
        );
    }
    if let Some(paths) = plan["absent_files"].as_array() {
        for path in paths {
            ensure!(
                !Path::new(string(path)?).exists(),
                "Previously absent capture appeared"
            );
        }
    }
    let request_bytes = fs::read(prepared.join("requests.jsonl"))?;
    ensure!(
        sha256(&request_bytes) == plan["requests_sha256"],
        "Changed requests"
    );
    let response_bytes = fs::read(responses)?;
    let mut scores = BTreeMap::new();
    for line in std::str::from_utf8(&response_bytes)?.lines() {
        let s: Value = serde_json::from_str(line)?;
        let id = string(&s["id"])?.to_owned();
        ensure!(scores.insert(id, s).is_none(), "Repeated response");
    }
    let mut rows = array(&plan["rows"])?.clone();
    ensure!(
        rows.len() == policy.expected_rows && scores.len() == rows.len(),
        "Incomplete response coverage"
    );
    for row in &mut rows {
        let s = scores
            .remove(string(&row["text_sha256"])?)
            .context("Missing response")?;
        ensure!(
            s["artifact_id"] == ARTIFACT
                && s["runtime"] == "cpu_float32_eager"
                && s["labels"] == json!(["human_only", "model_only", "mixed"]),
            "Wrong artifact/runtime"
        );
        let n = row["expected_tokens"]
            .as_u64()
            .context("Missing token count")?;
        row["status"] = s["status"].clone();
        if n > 2048 {
            ensure!(
                s["status"] == "unsupported_input"
                    && s["reason"] == format!("unsupported_length:{n}>2048")
                    && s["probabilities"].is_null()
                    && s["model_or_assistance_probability"].is_null(),
                "Unexpected unsupported response"
            );
            row["reason"] = s["reason"].clone();
            continue;
        }
        ensure!(
            s["status"] == "ok" && s["token_count"] == n,
            "Unexpected failed response/token count"
        );
        let p = s["model_or_assistance_probability"]
            .as_f64()
            .context("Missing probability")?;
        let probs = array(&s["probabilities"])?
            .iter()
            .map(|v| v.as_f64().context("Invalid class probability"))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            p.is_finite()
                && (0.0..=1.0).contains(&p)
                && probs.len() == 3
                && probs
                    .iter()
                    .all(|p| p.is_finite() && (0.0..=1.0).contains(p))
                && (probs.iter().sum::<f64>() - 1.0).abs() < 1e-6
                && (probs[1] + probs[2] - p).abs() < 1e-6,
            "Invalid probabilities"
        );
        row["probability"] = json!(p);
        row["strict_flag"] = json!(p > THRESHOLDS[0]);
        row["loose_flag"] = json!(p > THRESHOLDS[1]);
    }
    let mut groups = BTreeMap::<String, Vec<&Value>>::new();
    for row in &rows {
        groups
            .entry(format!(
                "{}/{}",
                string(&row["generator"])?,
                string(&row["profile"])?
            ))
            .or_default()
            .push(row);
    }
    let summary: BTreeMap<_,_>=groups.into_iter().map(|(key,rs)|{
        let ps: Vec<_>=rs.iter().filter_map(|r|r["probability"].as_f64()).collect();
        (key,json!({"total":rs.len(),"supported":ps.len(),"unsupported":rs.len()-ps.len(),"strict_flags":rs.iter().filter(|r|r["strict_flag"]==true).count(),"loose_flags":rs.iter().filter(|r|r["loose_flag"]==true).count(),"min_probability":ps.iter().copied().reduce(f64::min),"max_probability":ps.iter().copied().reduce(f64::max)}))
    }).collect();
    let mut result = json!({"schema":policy.report_schema,"artifact_id":ARTIFACT,"plan_sha256":sha256(plan_bytes),"responses_sha256":sha256(response_bytes),"thresholds_strictly_greater_than":THRESHOLDS,"summary":summary,"rows":rows,"provider_calls":0,"training_admission":false,"limitations":policy.limitations});
    if let Some(attempts) = plan.get("unscored_attempts") {
        result["unscored_attempts"] = attempts.clone();
    }
    save(out, &result)?;
    println!("{}", json!({"status":"reported","summary":summary}));
    Ok(())
}
