//! Replay declared saved responses without generation or corpus admission.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use slop_ninja_detector::generation::{ResponseFormat, final_prose};
use std::{fs, path::PathBuf};

fn digest(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Parser)]
struct Args {
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new result path");
    let bytes = fs::read(&args.manifest)?;
    let manifest: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        manifest["schema"] == "slop_ninja_harmony_response_packet_v1",
        "Wrong packet schema"
    );
    let mut rows = Vec::new();
    for case in manifest["cases"].as_array().context("Missing cases")? {
        let response = fs::read(case["response"].as_str().context("Missing response path")?)?;
        ensure!(
            digest(&response) == case["response_sha256"],
            "Changed response"
        );
        let v: Value = serde_json::from_slice(&response)?;
        ensure!(
            v["choices"][0]["finish_reason"] == "stop",
            "Incomplete response"
        );
        let raw = v["choices"][0]["message"]["content"]
            .as_str()
            .context("Missing content")?;
        let old = final_prose(raw, Some(ResponseFormat::HarmonyTextV1));
        ensure!(
            Some(old.is_ok()) == case["v1_expected_success"].as_bool(),
            "Changed frozen v1 outcome"
        );
        let new = final_prose(raw, Some(ResponseFormat::HarmonyTextV2))?;
        ensure!(
            digest(new) == case["expected_text_sha256"],
            "Changed final bytes"
        );
        if let Ok(text) = &old {
            ensure!(*text == new, "Changed earlier accepted final bytes");
        }
        rows.push(json!({"id":case["id"],"response_sha256":case["response_sha256"],"v1_success":old.is_ok(),"v1_error":old.err().map(|e|format!("{e:#}")),"v2_success":true,"final_text_sha256":digest(new),"final_words_unicode_whitespace":new.split_whitespace().count()}));
    }
    let report = json!({"schema":"slop_ninja_harmony_response_review_v1","manifest_sha256":digest(bytes),"generation_source_sha256":digest(include_bytes!("../src/generation.rs")),"runner_source_sha256":digest(include_bytes!("harmony_response_review.rs")),"cargo_lock_sha256":digest(include_bytes!("../Cargo.lock")),"binary_sha256":digest(fs::read(std::env::current_exe()?)?),"rows":rows,"provider_calls":0,"training_records_admitted":0,"historical_records_changed":false});
    fs::write(&args.out, serde_json::to_vec_pretty(&report)?)?;
    println!("{} saved responses checked", rows.len());
    Ok(())
}
