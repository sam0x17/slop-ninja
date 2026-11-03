//! Prepare exact hosted-capture requests for another detector, without training admission.
#[path = "../src/bin/support/annotation_captures.rs"]
mod annotation_captures;
use anyhow::{Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    capture_review: PathBuf,
    #[arg(long)]
    expected_audit_sha256: String,
    #[arg(long)]
    prior_comparison: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
fn save(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::File::create_new(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new output directory");
    let captures = annotation_captures::load(&args.capture_review, &args.expected_audit_sha256)?;
    let prior_bytes = fs::read(&args.prior_comparison)?;
    let prior: Vec<Value> = serde_json::from_slice(&prior_bytes)?;
    ensure!(
        prior.len() == captures.len(),
        "Prior comparison coverage differs"
    );
    let mut ids = BTreeSet::new();
    let mut requests = vec![];
    let mut provenance = vec![];
    for capture in &captures {
        let hash = sha256(&capture.text);
        ensure!(ids.insert(hash.clone()), "Repeated exact capture");
        let matches: Vec<_> = prior.iter().filter(|p| p["text_sha256"] == hash).collect();
        ensure!(matches.len() == 1, "Missing or duplicate prior observation");
        let previous = matches[0];
        ensure!(
            previous["source_group"] == capture.reference["source_group"]
                && previous["requested_model"]
                    == capture.reference["generator_attribution"]["requested_model"]
                && previous["profile"]
                    == capture.reference["generator_attribution"]["prompt_profile"]["profile"]["id"],
            "Prior provenance differs"
        );
        serde_json::to_writer(&mut requests, &json!({"id":hash,"text":capture.text}))?;
        requests.push(b'\n');
        provenance.push(json!({"text_sha256":hash,"reference":capture.reference,"prior":previous}));
    }
    fs::create_dir_all(&args.out)?;
    save(&args.out.join("requests.jsonl"), &requests)?;
    save(&args.out.join("prior-comparison.json"), &prior_bytes)?;
    save(
        &args.out.join("provenance.json"),
        &serde_json::to_vec_pretty(&provenance)?,
    )?;
    let plan = json!({"schema":"slop_ninja_hosted_rescore_v1",
        "capture_audit_sha256":args.expected_audit_sha256,
        "capture_annotation_schema":annotation_captures::PLAN_SCHEMA,
        "requests_sha256":sha256(&requests),"prior_comparison_sha256":sha256(&prior_bytes),
        "records":captures.len(),"commercial_training_admitted":false,
        "artifact_id":"4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138",
        "v6_thresholds_strictly_greater_than":[0.8436493155285434,0.4287369114260753],
        "pangram_flag_at_least":0.1,"provider_calls":0,
        "policy":"Score every frozen request with the V6 bundled CPU runner. Preserve failures; do not truncate, replace inputs, refit, or tune thresholds. Source families are already in Train. This is an all-model diagnostic with no independent human false-positive estimate. Prior Pangram scores are reused only for these exact texts."});
    save(
        &args.out.join("plan.json"),
        &serde_json::to_vec_pretty(&plan)?,
    )?;
    println!(
        "{}",
        json!({"status":"prepared","records":captures.len(),"model_calls":0,"provider_calls":0})
    );
    Ok(())
}
