//! Assemble completed generation cohorts with balanced held-out origin classes.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{self, Origin, OriginRecord, Split, sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    prior: PathBuf,
    /// Exact prior corpus declared before assembly; defaults to the v4 experiment's v2 input.
    #[arg(
        long,
        default_value = "6377d431b570a53cb9e5cfa97adb28627c24eb8fa68be263df6c86f94065fa06"
    )]
    expected_prior_sha256: String,
    #[arg(long, required = true)]
    generation_dir: Vec<PathBuf>,
    #[arg(long)]
    output_dir: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new assembly directory");
    ensure!(
        sha256(fs::read(&args.prior)?) == args.expected_prior_sha256,
        "Prior corpus does not match its declared SHA256"
    );
    let prior = dataset::read_records(&args.prior)?;
    let retired: BTreeSet<_> = prior
        .iter()
        .filter(|r| r.split == Some(Split::Test))
        .map(|r| r.source_group.clone())
        .collect();
    let mut all: BTreeMap<String, OriginRecord> =
        prior.into_iter().map(|r| (r.id.clone(), r)).collect();
    let mut inputs = Vec::new();
    for dir in &args.generation_dir {
        let summary_bytes = fs::read(dir.join("summary.json"))?;
        let summary: Value = serde_json::from_slice(&summary_bytes)?;
        ensure!(
            summary["status"] == "complete"
                && summary["complete_export_ready"] == true
                && summary["unattempted"]
                    .as_array()
                    .is_some_and(|a| a.is_empty()),
            "Generation cohort is not terminal and complete: {}",
            dir.display()
        );
        let path = dir.join("records.jsonl");
        let records = dataset::read_records(&path)?;
        ensure!(
            records.len()
                == summary["summary"]["records"]
                    .as_u64()
                    .context("Missing cohort row count")? as usize,
            "Cohort count mismatch"
        );
        inputs.push(json!({"directory":dir,"records_sha256":sha256(fs::read(&path)?),"summary_sha256":sha256(&summary_bytes),
            "requested":summary["requested"],"completed":summary["completed"],"excluded_root_count":summary["excluded_root_count"]}));
        for record in records {
            if let Some(previous) = all.get(&record.id) {
                ensure!(
                    serde_json::to_value(previous)? == serde_json::to_value(&record)?,
                    "Conflicting shared source record"
                );
            } else {
                all.insert(record.id.clone(), record);
            }
        }
    }
    let all: Vec<_> = all.into_values().collect();
    dataset::validate_records(&all)?;
    let roots: BTreeMap<_, _> = all
        .iter()
        .filter(|r| r.parent_id.is_none())
        .map(|r| (r.id.as_str(), r))
        .collect();
    ensure!(
        roots
            .values()
            .map(|r| &r.source_group)
            .collect::<BTreeSet<_>>()
            .len()
            == roots.len(),
        "Use one representative per source family"
    );
    let mut pairs: BTreeMap<(&str, &str), [Option<&OriginRecord>; 2]> = BTreeMap::new();
    for record in all.iter().filter(|r| r.parent_id.is_some()) {
        let parent = record.parent_id.as_deref().unwrap();
        ensure!(
            roots.contains_key(parent),
            "Expected direct root descendants"
        );
        let generator = &record
            .generation
            .as_ref()
            .context("Missing generator provenance")?
            .model_revision;
        let class = match record.origin {
            Origin::ModelOnly => 0,
            Origin::Mixed => 1,
            _ => anyhow::bail!("Unexpected human descendant"),
        };
        let pair = pairs.entry((parent, generator.as_str())).or_default();
        ensure!(
            pair[class].replace(record).is_none(),
            "Duplicate generator/origin for one source; choose a declared cohort before merging"
        );
    }
    ensure!(
        pairs.values().all(|p| p.iter().all(Option::is_some)),
        "Incomplete generator pair"
    );
    let mut assembled = Vec::new();
    let mut selected = Vec::new();
    let mut generator_counts = BTreeMap::<String, BTreeMap<String, usize>>::new();
    for (id, root) in roots {
        if retired.contains(&root.source_group) {
            continue;
        }
        let mut candidates: Vec<_> = pairs
            .iter()
            .filter(|((parent, _), _)| *parent == id)
            .collect();
        ensure!(
            !candidates.is_empty(),
            "Root has no complete generator pair"
        );
        candidates.sort_by_cached_key(|((_, generator), _)| {
            sha256(
                serde_json::to_vec(&(
                    "slop-ninja-expansion-evaluation-provider-v1",
                    &root.source_group,
                    generator,
                ))
                .unwrap(),
            )
        });
        // Fit on every admitted variant; Development/Calibration/Test get one
        // provider pair per root, selected before any detector prediction.
        if root.split != Some(Split::Train) {
            candidates.truncate(1);
        }
        assembled.push(root.clone());
        for ((_, generator), pair) in candidates {
            let split = serde_json::to_value(root.split)?
                .as_str()
                .context("Unfrozen split")?
                .to_owned();
            *generator_counts
                .entry(split.clone())
                .or_default()
                .entry((*generator).to_owned())
                .or_default() += 1;
            selected.push(
                json!({"source_group":root.source_group,"split":split,"model_revision":generator}),
            );
            assembled.extend(pair.iter().map(|r| r.unwrap().clone()));
        }
    }
    dataset::validate_records(&assembled)?;
    for split in [
        Split::Train,
        Split::Development,
        Split::Calibration,
        Split::Test,
    ] {
        ensure!(
            assembled.iter().any(|r| r.split == Some(split)),
            "An assembled partition is empty"
        );
    }
    fs::create_dir(&args.output_dir)?;
    dataset::write_records(&args.output_dir.join("all-variants.jsonl"), &all)?;
    dataset::write_records(&args.output_dir.join("records.jsonl"), &assembled)?;
    let summary = json!({"schema":"slop_ninja_expansion_assembly_v1","prior_sha256":sha256(fs::read(args.prior)?),"generation_inputs":inputs,
        "records_sha256":sha256(fs::read(args.output_dir.join("records.jsonl"))?),"all_variants_sha256":sha256(fs::read(args.output_dir.join("all-variants.jsonl"))?),
        "retired_prior_test_families":retired.len(),"summary":dataset::summarize(&assembled),"generator_pairs_by_split":generator_counts,
        "training":"all complete provider pairs; use source_origin loss weights so each family-origin has equal total mass",
        "evaluation_selection":"one complete provider pair per root, smallest SHA256(domain, source_group, model_revision); same provider for draft/edit; no scores",
        "selection":selected,"test_predictions_opened":false});
    fs::write(
        args.output_dir.join("assembly.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary["summary"])?);
    Ok(())
}
