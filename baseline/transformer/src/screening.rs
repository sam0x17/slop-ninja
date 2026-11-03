//! Optional accelerator feedback. Canonical A inference remains a separate step.
use super::{Sample, hash, json_lines, read, save, text, write_new};
use anyhow::{Context, Result, ensure};
use clap::{Args as ClapArgs, ValueEnum};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    process::{Command, Stdio},
};

const BRIDGE: &str = include_str!("../python/screen.py");
const SCHEMA: &str = "slop_ninja_b_screen_observation_v1";

#[derive(Clone, Copy, ValueEnum)]
pub enum Device {
    Mps,
    Cuda,
}

impl Device {
    fn name(self) -> &'static str {
        match self {
            Self::Mps => "mps",
            Self::Cuda => "cuda",
        }
    }
}

#[derive(ClapArgs)]
pub struct Args {
    #[arg(long)]
    requests: PathBuf,
    #[arg(long)]
    artifact: PathBuf,
    #[arg(long)]
    python: String,
    #[arg(long, value_enum)]
    device: Device,
    #[arg(long)]
    out: PathBuf,
}

fn check_row(row: &Value, artifact: &str, runtime: &str, max_tokens: u64) -> Result<()> {
    ensure!(
        row["schema"] == SCHEMA
            && row["canonical_evidence"] == false
            && row["weight_artifact_id"] == artifact
            && row.get("artifact_id").is_none()
            && row["runtime"] == runtime,
        "Wrong screening identity or evidence class"
    );
    ensure!(
        row["labels"] == json!(["human_only", "model_only", "mixed"]),
        "Wrong label order"
    );
    match row["status"].as_str() {
        Some("ok") => {
            ensure!(
                row["token_count"]
                    .as_u64()
                    .is_some_and(|n| n > 0 && n <= max_tokens),
                "Invalid token count"
            );
            let p = row["probabilities"]
                .as_array()
                .context("Missing probabilities")?;
            ensure!(p.len() == 3, "Expected three probabilities");
            let p = p
                .iter()
                .map(|p| p.as_f64().context("Invalid probability"))
                .collect::<Result<Vec<_>>>()?;
            ensure!(
                p.iter().all(|p| p.is_finite() && (0.0..=1.0).contains(p))
                    && (p.iter().sum::<f64>() - 1.0).abs() < 1e-6,
                "Invalid probability distribution"
            );
            let combined = row["model_or_assistance_probability"]
                .as_f64()
                .context("Missing combined probability")?;
            ensure!(
                combined.is_finite() && (combined - p[1] - p[2]).abs() < 1e-12,
                "Invalid combined probability"
            );
        }
        Some("unsupported_input") => ensure!(
            row["probabilities"].is_null()
                && row["model_or_assistance_probability"].is_null()
                && !text(row, "reason")?.is_empty(),
            "Unsupported input must remain unscored"
        ),
        _ => anyhow::bail!("Incomplete or failed accelerator observation"),
    }
    Ok(())
}

pub fn run(args: Args) -> Result<()> {
    ensure!(!args.out.exists(), "Refusing to overwrite a screening run");
    ensure!(
        fs::metadata(&args.requests)?.len() <= 64 * 1024 * 1024,
        "Screen at most 64 MiB per run"
    );
    let bytes = fs::read(&args.requests)?;
    let rows = json_lines(&args.requests)?;
    ensure!(
        (1..=512).contains(&rows.len()),
        "Screen 1..512 distinct requests per run"
    );
    let mut ids = BTreeSet::new();
    for row in rows {
        let request: Sample = serde_json::from_value(row)?;
        ensure!(
            request.id == hash(&request.text)
                && !request.text.trim().is_empty()
                && ids.insert(request.id),
            "Empty, duplicate or incorrectly bound request"
        );
    }
    let artifact = fs::canonicalize(args.artifact)?;
    let manifest = read(&artifact.join("manifest.json"))?;
    let artifact_id = text(&manifest, "artifact_id")?;
    let max_tokens = manifest["max_tokens"]
        .as_u64()
        .context("No artifact token limit")?;
    fs::create_dir(&args.out)?;
    let out = fs::canonicalize(&args.out)?;
    write_new(&out.join("requests.jsonl"), &bytes)?;
    write_new(&out.join("screen.py"), BRIDGE.as_bytes())?;
    let runtime = format!("{}_float32_eager_exploratory", args.device.name());
    save(
        &out.join("plan.json"),
        &json!({"schema":"slop_ninja_b_screen_plan_v1",
        "requests_sha256":hash(&bytes),"bridge_sha256":hash(BRIDGE),
        "manifest_sha256":hash(fs::read(artifact.join("manifest.json"))?),
        "weight_artifact_id":artifact_id,"request_count":ids.len(),"runtime":runtime,
        "canonical_evidence":false,"research_only":true}),
    )?;
    let status = Command::new(&args.python)
        .arg(out.join("screen.py"))
        .arg("--artifact")
        .arg(&artifact)
        .arg("--device")
        .arg(args.device.name())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("HF_HUB_OFFLINE", "1")
        .env("TRANSFORMERS_OFFLINE", "1")
        .env("PYTORCH_ENABLE_MPS_FALLBACK", "0")
        .stdin(Stdio::from(fs::File::open(out.join("requests.jsonl"))?))
        .stdout(Stdio::from(fs::File::create(out.join("raw.jsonl"))?))
        .stderr(Stdio::from(fs::File::create(out.join("inference.log"))?))
        .status()
        .context("Could not start accelerator bridge; retained plan")?;
    save(
        &out.join("execution.json"),
        &json!({"success":status.success(),"exit_code":status.code()}),
    )?;
    ensure!(
        status.success(),
        "Accelerator failed; inspect retained inference.log and partial raw.jsonl"
    );
    let raw = json_lines(&out.join("raw.jsonl"))?;
    let info = raw.first().context("Missing accelerator metadata")?;
    ensure!(
        info["schema"] == "slop_ninja_b_screen_runtime_v1"
            && info["weight_artifact_id"] == artifact_id
            && info["runtime"] == runtime
            && info["canonical_evidence"] == false
            && info["bridge_sha256"] == hash(BRIDGE),
        "Unbound accelerator metadata"
    );
    ensure!(raw.len() == ids.len() + 1, "Incomplete accelerator output");
    let mut unsupported = 0;
    for row in &raw[1..] {
        ensure!(
            ids.remove(text(row, "id")?),
            "Unexpected or duplicate result"
        );
        check_row(row, artifact_id, &runtime, max_tokens)?;
        unsupported += usize::from(row["status"] == "unsupported_input");
    }
    ensure!(ids.is_empty(), "Missing accelerator results");
    let output = raw[1..]
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join("\n")
        + "\n";
    write_new(&out.join("observations.jsonl"), output.as_bytes())?;
    save(
        &out.join("summary.json"),
        &json!({"schema":"slop_ninja_b_screen_summary_v1",
        "observations":raw.len()-1,"unsupported_inputs":unsupported,
        "raw_sha256":hash(fs::read(out.join("raw.jsonl"))?),"runtime_metadata":info,
        "canonical_evidence":false,"eligible_output":false,
        "next_step":"Confirm shortlisted exact texts with the pinned CPU A runner; review full candidates and document coverage separately."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn screening_cannot_claim_canonical_identity_or_hide_unsupported_input() {
        let mut row = json!({"schema":SCHEMA,"canonical_evidence":false,"weight_artifact_id":"weights",
            "runtime":"mps_float32_eager_exploratory","labels":["human_only","model_only","mixed"],
            "status":"ok","token_count":30,"probabilities":[0.8,0.15,0.05],"model_or_assistance_probability":0.2});
        let check = |r: &Value| check_row(r, "weights", "mps_float32_eager_exploratory", 2048);
        check(&row).unwrap();
        row["canonical_evidence"] = json!(true);
        assert!(check(&row).is_err());
        row["canonical_evidence"] = json!(false);
        row["artifact_id"] = json!("weights");
        assert!(check(&row).is_err());
        row.as_object_mut().unwrap().remove("artifact_id");
        row["status"] = json!("unsupported_input");
        row["reason"] = json!("unsupported_length");
        assert!(check(&row).is_err());
        row["probabilities"] = Value::Null;
        row["model_or_assistance_probability"] = Value::Null;
        check(&row).unwrap();
    }
}
