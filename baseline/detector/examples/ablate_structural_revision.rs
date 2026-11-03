//! Enumerate recorded components or an explicit manual probe with saved controls.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::edits::{self, Edit};
use serde::Deserialize;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{Origin, OriginRecord, Split, sha256};
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    path: PathBuf,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Choice {
    id: String,
    before: String,
    after: String,
}
#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    CapturedComponents,
    ManualProbe,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    source_case: Binding,
    prior_scores: Binding,
    artifact_id: String,
    #[serde(default)]
    mode: Mode,
    expected_combined_text_sha256: Option<String>,
    expected_new_inputs: usize,
    choices: Vec<Choice>,
}
fn bound(binding: &Binding) -> Result<Vec<u8>> {
    let bytes = fs::read(&binding.path)?;
    ensure!(sha256(&bytes) == binding.sha256, "Changed bound input");
    Ok(bytes)
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new output directory");
    let config_bytes = fs::read(&args.config)?;
    let config: Config = serde_json::from_slice(&config_bytes)?;
    ensure!(
        (1..=4).contains(&config.choices.len()),
        "Expected one to four components"
    );
    if config.mode == Mode::ManualProbe {
        ensure!(
            config.choices.len() == 1 && config.expected_combined_text_sha256.is_none(),
            "Manual probes require one edit and no captured-candidate hash"
        );
    }
    let case: Value = serde_json::from_slice(&bound(&config.source_case)?)?;
    let source: OriginRecord = serde_json::from_value(case["source"]["parent"].clone())?;
    source.validate()?;
    ensure!(
        source.origin == Origin::ModelOnly && source.split == Some(Split::Train),
        "Requires recorded Train model parent"
    );
    let score_bytes = bound(&config.prior_scores)?;
    let mut scores = BTreeMap::new();
    for line in std::str::from_utf8(&score_bytes)?.lines() {
        let row: Value = serde_json::from_str(line)?;
        let id = row["id"].as_str().context("Missing score ID")?.to_owned();
        ensure!(scores.insert(id, row).is_none(), "Duplicate score ID");
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut patches = Vec::new();
    for choice in &config.choices {
        ensure!(
            !choice.id.is_empty() && ids.insert(&choice.id),
            "Invalid component ID"
        );
        ensure!(
            !choice.before.is_empty()
                && !choice.after.trim().is_empty()
                && choice.before != choice.after,
            "Empty or unchanged component"
        );
        let matches: Vec<_> = source.text.match_indices(&choice.before).collect();
        ensure!(matches.len() == 1, "Expected unique source span");
        let start = matches[0].0;
        patches.push(Edit {
            start_byte: start,
            end_byte: start + choice.before.len(),
            expected: choice.before.clone(),
            replacement: choice.after.clone(),
        });
    }
    let combined = edits::apply(&source.text, &patches)?;
    if config.mode == Mode::CapturedComponents {
        let expected = config
            .expected_combined_text_sha256
            .as_ref()
            .context("Captured components require the combined text hash")?;
        ensure!(
            sha256(&combined) == *expected,
            "Components do not reproduce the captured candidate"
        );
        ensure!(
            case["proposal"]["states"]
                .as_array()
                .context("Missing original states")?
                .iter()
                .any(|s| s["text"] == combined && s["text_sha256"] == *expected),
            "Combined state absent from original experiment"
        );
    }
    let mut rows = Vec::new();
    let mut requests = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for mask in 0..(1usize << patches.len()) {
        let selected: Vec<_> = patches
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, p)| p.clone())
            .collect();
        let text = edits::apply(&source.text, &selected)?;
        let id = sha256(&text);
        ensure!(
            seen.insert(id.clone()),
            "Distinct subsets produced duplicate text"
        );
        let cached = scores.get(&id);
        if let Some(score) = cached {
            ensure!(
                score["artifact_id"] == config.artifact_id
                    && score["runtime"] == "cpu_float32_eager"
                    && score["status"] == "ok"
                    && score["labels"] == json!(["human_only", "model_only", "mixed"]),
                "Unsupported cached score"
            );
        } else {
            requests.push(json!({"id":id,"text":text}));
        }
        rows.push(json!({"mask":mask,"id":id,"text":text,"edits":selected,"cached_response":cached,"review":"pending"}));
    }
    ensure!(
        requests.len() == config.expected_new_inputs,
        "Unexpected new inference count"
    );
    ensure!(
        !rows[0]["cached_response"].is_null(),
        "Parent must have a saved response"
    );
    if config.mode == Mode::CapturedComponents {
        ensure!(
            !rows.last().unwrap()["cached_response"].is_null(),
            "Captured endpoint must have a saved response"
        );
    }
    bound(&config.source_case)?;
    bound(&config.prior_scores)?;
    ensure!(
        fs::read(&args.config)? == config_bytes,
        "Config changed during preparation"
    );
    fs::create_dir(&args.out)?;
    let mut file = fs::File::create_new(args.out.join("requests.jsonl"))?;
    for request in &requests {
        serde_json::to_writer(&mut file, request)?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    fs::write(args.out.join("config.json"), &config_bytes)?;
    let mut result = json!({"schema":"slop_ninja_a_structural_components_v1","config_sha256":sha256(config_bytes),"source_case_sha256":config.source_case.sha256,"prior_scores_sha256":config.prior_scores.sha256,"artifact_id":config.artifact_id,"source_record_id":source.id,"source_group":source.source_group,"source_origin":source.origin,"split":source.split,"component_ids":config.choices.iter().map(|c|&c.id).collect::<Vec<_>>(),"states":rows,"new_inputs":requests.len(),"requests_sha256":sha256(fs::read(args.out.join("requests.jsonl"))?),"training_admission":false,"provider_calls":0});
    if config.mode == Mode::ManualProbe {
        result["mode"] = json!("manual_probe");
        result["candidate_provenance"] = json!("manually_specified_exact_edit");
    }
    let mut file = fs::File::create_new(args.out.join("plan.json"))?;
    serde_json::to_writer_pretty(&mut file, &result)?;
    file.write_all(b"\n")?;
    println!(
        "{}",
        json!({"states":rows.len(),"new_inputs":requests.len(),"model_calls":0})
    );
    Ok(())
}
