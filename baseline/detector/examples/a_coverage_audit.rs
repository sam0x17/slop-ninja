//! Reuse frozen V6 evidence to audit coverage before another fit. No model calls.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{Split, read_records, sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::PathBuf,
};

const ARTIFACT: &str = "4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138";

#[derive(Parser)]
struct Args {
    #[arg(long)]
    train: PathBuf,
    #[arg(long)]
    token_audit: PathBuf,
    #[arg(long)]
    evaluation: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new output");
    let audit_bytes = fs::read(&args.token_audit)?;
    let audit: Value = serde_json::from_slice(&audit_bytes)?;
    let train_hash = sha256(fs::read(&args.train)?);
    ensure!(
        audit["shards"]["train"]["sha256"] == train_hash,
        "Training shard differs from the token audit"
    );
    let records = read_records(&args.train)?;
    let mut groups = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut generations = BTreeMap::<(String, String, String), usize>::new();
    let mut collection_groups = BTreeMap::<String, BTreeSet<String>>::new();
    for record in &records {
        record.validate()?;
        ensure!(
            record.split == Some(Split::Train) && ids.insert(&record.id),
            "Wrong split or duplicate record"
        );
        groups.insert(&record.source_group);
        collection_groups
            .entry(record.source.collection.clone())
            .or_default()
            .insert(record.source_group.clone());
        if let Some(generation) = &record.generation {
            let profile = generation
                .prompt_profile
                .as_ref()
                .map(|p| p.profile.id.clone())
                .unwrap_or_else(|| "unprofiled".to_owned());
            *generations
                .entry((
                    generation.model_revision.clone(),
                    generation.operation.clone(),
                    profile,
                ))
                .or_default() += 1;
        }
    }
    ensure!(
        audit["shards"]["train"]["records"] == records.len()
            && audit["shards"]["train"]["source_groups"] == groups.len(),
        "Training counts differ from audit"
    );
    let evaluation_bytes = fs::read(&args.evaluation)?;
    let evaluation: Value = serde_json::from_slice(&evaluation_bytes)?;
    let mut profiles = vec![];
    for route in ["primary", "phi4"] {
        for stage in ["R0", "R1", "R2"] {
            let report = &evaluation["reports"][format!("{route}-{stage}-v6")];
            ensure!(report["artifact_id"] == ARTIFACT, "Wrong V6 artifact");
            let slices = report["human_model_slices"]
                .as_object()
                .context("Missing binary slices")?;
            let mut rows = 0u64;
            let mut count = 0;
            for (key, metrics) in slices {
                if let Some(profile) = key.strip_prefix("initial-profile:") {
                    rows += metrics["rows"].as_u64().context("Missing rows")?;
                    count += 1;
                    let three = &report["slices"][key];
                    profiles.push(json!({"route":route,"stage":stage,"profile":profile,
                        "binary":metrics,"three_class_accuracy":three["model"]["accuracy"],
                        "three_class_log_loss":three["model"]["log_loss"],
                        "operating_points":three["operating_points"]}));
                }
            }
            ensure!(
                count == 6 && report["human_model"]["rows"] == rows,
                "Profiles do not partition this view"
            );
        }
    }
    let lengths: BTreeMap<_, _> = ["train", "development", "calibration", "test"]
        .into_iter()
        .map(|split| {
            let shard = &audit["shards"][split];
            (
                split,
                json!({"records":shard["records"],"source_groups":shard["source_groups"],
            "class_counts":shard["class_counts"],
            "token_lengths":shard["token_lengths_including_special_tokens"],
            "token_lengths_by_class":shard["token_lengths_by_class"]}),
            )
        })
        .collect();
    ensure!(
        sha256(fs::read(&args.train)?) == train_hash,
        "Training input changed"
    );
    let result = json!({"schema":"slop_ninja_a_coverage_audit_v1","artifact_id":ARTIFACT,
        "inputs":{"train_sha256":train_hash,"token_audit_sha256":sha256(&audit_bytes),
            "evaluation_sha256":sha256(&evaluation_bytes)},
        "training_collection_groups":collection_groups.into_iter().map(|(collection, groups)|
            json!({"collection":collection,"source_groups":groups.len()})).collect::<Vec<_>>(),
        "training_generation_cells":generations.into_iter().map(|((model, operation, profile), rows)|
            json!({"last_generator":model,"operation":operation,"profile":profile,"rows":rows})).collect::<Vec<_>>(),
        "length_coverage":lengths,"profile_slices":profiles,
        "model_calls":0,"provider_calls":0,"new_training":false,
        "limits":["Previously opened evaluation; diagnostic only.",
            "Profiles are requested prompts, not independently verified styles.",
            "Collection groups, routes and revision-stage views overlap; do not sum them as independent samples.",
            "Last generator counts do not replace full revision-chain provenance.",
            "Supported context length does not establish training or evaluation coverage.",
            "No new Test set, retraining or detector-parity claim."]});
    let mut output = fs::File::create_new(&args.out)?;
    serde_json::to_writer_pretty(&mut output, &result)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    println!(
        "{}",
        json!({"status":"audited","training_rows":records.len(),
        "training_source_groups":groups.len(),"profile_slices":profiles.len(),"model_calls":0,"provider_calls":0})
    );
    Ok(())
}
