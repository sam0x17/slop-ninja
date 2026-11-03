//! Freeze fresh source families and combine completed generation cohorts.
use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use serde_json::json;
use slop_ninja_detector::dataset::{self, Evidence, OriginRecord, Split};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Keep one representative per already frozen family, ranked without scores.
    SelectRoots {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output_dir: PathBuf,
    },
    /// Exclude previously used sources, freeze partitions and balance teachers.
    Plan {
        #[arg(long)]
        input: PathBuf,
        #[arg(long, required = true)]
        prior: Vec<PathBuf>,
        #[arg(long)]
        output_dir: PathBuf,
        #[arg(long, default_value = "slop-ninja-style-pilot-v2")]
        seed: String,
    },
    /// Combine closed admitted cohorts; identical shared roots are deduplicated.
    Merge {
        #[arg(long, required = true)]
        input: Vec<PathBuf>,
        #[arg(long)]
        output: PathBuf,
    },
}

fn text_key(record: &OriginRecord) -> String {
    dataset::sha256(grammar_core::features::words(&record.text).join(" "))
}

fn plan(input: PathBuf, prior: Vec<PathBuf>, output: PathBuf, seed: String) -> Result<()> {
    ensure!(!output.exists(), "Use a new immutable plan directory");
    let candidates = dataset::read_records(&input)?;
    ensure!(
        candidates.iter().all(|r| r.split.is_none()
            && r.parent_id.is_none()
            && r.evidence == Evidence::HistoricalProxy),
        "Expected unsplit historical source roots"
    );
    let mut groups = BTreeSet::new();
    let mut source_urls = BTreeSet::new();
    let mut hashes = BTreeSet::new();
    let mut prior_hashes = Vec::new();
    for path in &prior {
        prior_hashes.push(dataset::sha256(fs::read(path)?));
        for record in dataset::read_records(path)? {
            groups.insert(record.source_group.clone());
            source_urls.insert(record.source.url.clone());
            hashes.insert(record.text_sha256.clone());
            hashes.insert(text_key(&record));
        }
    }
    let mut excluded = Vec::new();
    let mut records = Vec::new();
    for record in candidates {
        let overlaps = groups.contains(&record.source_group)
            || source_urls.contains(&record.source.url)
            || hashes.contains(&record.text_sha256)
            || hashes.contains(&text_key(&record));
        if overlaps {
            excluded.push(json!({"id":record.id,"source_group":record.source_group,"reason":"previous_source_or_exact_normalized_text"}));
        } else {
            records.push(record);
        }
    }
    ensure!(!records.is_empty(), "No fresh sources remain");
    let split_report = dataset::assign_splits(&mut records, &seed)?;
    ensure!(
        [
            Split::Train,
            Split::Development,
            Split::Calibration,
            Split::Test
        ]
        .iter()
        .all(|split| records.iter().any(|r| r.split == Some(*split))),
        "A frozen split is empty; amend protocol instead of rerolling silently"
    );
    let mut family_collections = BTreeMap::<String, (Split, BTreeSet<String>)>::new();
    for r in &records {
        let family = family_collections
            .entry(r.source_group.clone())
            .or_insert((r.split.unwrap(), BTreeSet::new()));
        family.1.insert(r.source.collection.clone());
    }
    let mut buckets = BTreeMap::<(Split, Vec<String>), Vec<(String, String)>>::new();
    for (family, (split, collections)) in family_collections {
        let collections = collections.into_iter().collect::<Vec<_>>();
        let rank = dataset::sha256(serde_json::to_vec(&(
            "style-pilot-teacher-v1",
            &seed,
            &family,
        ))?);
        buckets
            .entry((split, collections))
            .or_default()
            .push((rank, family));
    }
    let mut assignments = BTreeMap::new();
    for ((split, collections), mut families) in buckets {
        families.sort();
        let digest = dataset::sha256(serde_json::to_vec(&(
            "style-pilot-teacher-offset-v1",
            &seed,
            split,
            &collections,
        ))?);
        let offset = usize::from_str_radix(&digest[..2], 16)? % 2;
        for (rank, (_, family)) in families.into_iter().enumerate() {
            assignments.insert(
                family,
                if (rank + offset) % 2 == 0 {
                    "qwen"
                } else {
                    "mistral"
                },
            );
        }
    }
    fs::create_dir_all(&output)?;
    dataset::write_records(&output.join("frozen-roots.jsonl"), &records)?;
    let mut files = BTreeMap::new();
    let mut summaries = BTreeMap::new();
    for teacher in ["qwen", "mistral"] {
        let selected = records
            .iter()
            .filter(|r| assignments[&r.source_group] == teacher)
            .cloned()
            .collect::<Vec<_>>();
        let name = format!("{teacher}-roots.jsonl");
        dataset::write_records(&output.join(&name), &selected)?;
        files.insert(name.clone(), dataset::sha256(fs::read(output.join(&name))?));
        summaries.insert(teacher, dataset::summarize(&selected));
    }
    let report = json!({"schema":"slop_ninja_style_pilot_plan_v1","input_sha256":dataset::sha256(fs::read(input)?),
        "prior_manifest_sha256":prior_hashes,"seed":seed,"split_report":split_report,"excluded":excluded,
        "frozen_roots_sha256":dataset::sha256(fs::read(output.join("frozen-roots.jsonl"))?),
        "teacher_assignment":"SHA256-ranked round robin within split/register; source families kept whole; no scores, origins or text content used",
        "teacher_assignments":assignments,"files":files,"teacher_summaries":summaries,
        "limitations":["Excludes previous source URLs/families and exact/normalized text, not all shared authors/events or near-duplicates.","Generator and style effects are not isolated by this mixed-recipe experiment."]});
    fs::write(
        output.join("plan.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"summary":dataset::summarize(&records),"teachers":summaries,"excluded":report["excluded"]})
        )?
    );
    Ok(())
}

fn main() -> Result<()> {
    match Args::parse().command {
        Command::SelectRoots { input, output_dir } => {
            ensure!(!output_dir.exists(), "Use a new selection directory");
            let mut records = dataset::read_records(&input)?;
            let input_count = records.len();
            ensure!(
                records.iter().all(|r| r.split.is_some()),
                "Expected already frozen records"
            );
            records.retain(|r| r.parent_id.is_none());
            ensure!(
                !records.is_empty()
                    && records
                        .iter()
                        .all(|r| r.evidence == Evidence::HistoricalProxy),
                "Expected historical source roots"
            );
            let input_roots = records.len();
            records.sort_by_cached_key(|r| (dataset::sha256(&r.id), r.id.clone()));
            let mut seen = BTreeSet::new();
            let mut omitted = Vec::new();
            records.retain(|r| {
                if seen.insert(r.source_group.clone()) {
                    true
                } else {
                    omitted.push(json!({"id":r.id,"source_group":r.source_group}));
                    false
                }
            });
            fs::create_dir(&output_dir)?;
            let output = output_dir.join("frozen-roots.jsonl");
            dataset::write_records(&output, &records)?;
            let report = json!({"schema":"slop_ninja_family_representatives_v1","input_sha256":dataset::sha256(fs::read(input)?),
                "input_rows":input_count,"input_roots":input_roots,"descendants_ignored":input_count-input_roots,
                "selection":"one record per frozen source family, ascending SHA256(record ID), then ID; retain existing splits",
                "omitted":omitted,"output_sha256":dataset::sha256(fs::read(output)?),"summary":dataset::summarize(&records)});
            fs::write(
                output_dir.join("selection.json"),
                serde_json::to_vec_pretty(&report)?,
            )?;
            println!("{}", serde_json::to_string_pretty(&report["summary"])?);
            Ok(())
        }
        Command::Plan {
            input,
            prior,
            output_dir,
            seed,
        } => plan(input, prior, output_dir, seed),
        Command::Merge { input, output } => {
            ensure!(!output.exists(), "Use a new cohort output");
            let mut all = BTreeMap::<String, OriginRecord>::new();
            for path in input {
                for r in dataset::read_records(&path)? {
                    if let Some(previous) = all.get(&r.id) {
                        ensure!(
                            serde_json::to_value(previous)? == serde_json::to_value(&r)?,
                            "Conflicting duplicate ID {}",
                            r.id
                        );
                    } else {
                        all.insert(r.id.clone(), r);
                    }
                }
            }
            let records = all.into_values().collect::<Vec<_>>();
            dataset::write_records(&output, &records)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&dataset::summarize(&records))?
            );
            Ok(())
        }
    }
}
