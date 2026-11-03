//! Select complete Train families for budgeted annotation without opening scores.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{self, Origin, OriginRecord, Split, sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

const DOMAIN: &str = "slop-ninja-pangram-family-sampling-v1";

#[derive(Parser)]
struct Args {
    /// Frozen, complete Train ancestry records with independent origin evidence.
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
    #[arg(long, default_value_t = 1)]
    families_per_stratum: usize,
    /// Earlier cohort inputs; exclude their entire families, including human controls.
    #[arg(long)]
    exclude_cohort: Vec<PathBuf>,
    /// Optional collection/profile strata selected from a separately recorded pilot.
    #[arg(long)]
    stratum: Vec<String>,
}

fn select(
    records: &[OriginRecord],
    per_stratum: usize,
    excluded: &BTreeSet<String>,
    allowed: &BTreeSet<String>,
) -> Result<(Vec<OriginRecord>, Value)> {
    ensure!(
        (1..=100).contains(&per_stratum),
        "Use 1..100 families per stratum"
    );
    dataset::validate_records(records)?;
    ensure!(
        records
            .iter()
            .all(|r| r.split == Some(Split::Train) && r.rights.external_evaluation),
        "Only externally admitted Train records may enter this annotation collection"
    );
    let mut families: BTreeMap<&str, Vec<&OriginRecord>> = BTreeMap::new();
    for row in records {
        families.entry(&row.source_group).or_default().push(row);
    }
    let mut strata: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (group, family) in &families {
        let roots: Vec<_> = family
            .iter()
            .filter(|r| r.origin == Origin::HumanOnly)
            .collect();
        let drafts: Vec<_> = family
            .iter()
            .filter(|r| {
                r.generation
                    .as_ref()
                    .is_some_and(|g| g.operation == "draft")
            })
            .collect();
        ensure!(
            roots.len() == 1 && drafts.len() == 1,
            "Require one human root and original draft per family"
        );
        let draft = drafts[0];
        ensure!(
            draft.origin == Origin::ModelOnly,
            "Draft has an incompatible origin"
        );
        let profile = serde_json::to_value(&draft.generation.as_ref().unwrap().prompt_profile)?;
        let profile = profile["profile"]["id"]
            .as_str()
            .context("Missing recorded draft profile")?;
        let key = format!("{}/{profile}", roots[0].source.collection);
        // Declare each stratum even if earlier cohorts have exhausted it.
        let members = strata.entry(key).or_default();
        if !excluded.contains(*group) {
            let rank = sha256(serde_json::to_vec(&(DOMAIN, group))?);
            members.push((rank, (*group).to_owned()));
        }
    }
    ensure!(
        allowed.iter().all(|s| strata.contains_key(s)),
        "Unknown requested stratum"
    );
    let mut selected_groups = BTreeSet::new();
    let mut counts = BTreeMap::new();
    for (key, members) in &mut strata {
        members.sort();
        if !allowed.is_empty() && !allowed.contains(key) {
            continue;
        }
        let chosen: Vec<_> = members
            .iter()
            .take(per_stratum)
            .map(|(_, group)| group.clone())
            .collect();
        selected_groups.extend(chosen.iter().cloned());
        counts.insert(
            key.clone(),
            json!({"available_families":members.len(),"selected_families":chosen}),
        );
    }
    ensure!(
        !selected_groups.is_empty(),
        "No unannotated families remain in the requested strata"
    );
    // Preserve original record order and every ancestor/sibling in each chosen family.
    let selected: Vec<_> = records
        .iter()
        .filter(|r| selected_groups.contains(&r.source_group))
        .cloned()
        .collect();
    dataset::validate_records(&selected)?;
    let report = json!({
        "rank_domain":DOMAIN,"rank_rule":"Ascending SHA256 of JSON [domain, source_group], then source_group",
        "strata":"human-root collection / original-draft prompt profile",
        "families_per_stratum":per_stratum,"requested_strata":allowed,"selection":counts,
        "selected_families":selected_groups.len(),"excluded_families":excluded.len(),
        "summary":dataset::summarize(&selected),
        "policy":"Keep whole families and original records; origin evidence is independent of detector observations. Scores do not enter this selector. If strata were chosen from earlier scores, record that adaptive choice separately; this collection is not a fresh benchmark."
    });
    Ok((selected, report))
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new cohort directory");
    let input_hash = sha256(fs::read(&args.input)?);
    let records = dataset::read_records(&args.input)?;
    let mut excluded = BTreeSet::new();
    let mut exclusion_inputs = Vec::new();
    for path in &args.exclude_cohort {
        let hash = sha256(fs::read(path)?);
        let previous = dataset::read_records(path)?;
        ensure!(
            previous.iter().all(|r| r.split == Some(Split::Train)),
            "Exclusion cohort is not Train-only"
        );
        ensure!(
            hash == sha256(fs::read(path)?),
            "Exclusion input changed during selection"
        );
        excluded.extend(previous.into_iter().map(|r| r.source_group));
        exclusion_inputs.push(json!({"path":path,"sha256":hash}));
    }
    let (selected, selection) = select(
        &records,
        args.families_per_stratum,
        &excluded,
        &args.stratum.into_iter().collect(),
    )?;
    ensure!(
        input_hash == sha256(fs::read(&args.input)?),
        "Corpus changed during selection"
    );
    fs::create_dir(&args.output_dir)?;
    let output = args.output_dir.join("records.jsonl");
    dataset::write_records(&output, &selected)?;
    let report = json!({
        "schema":"slop_ninja_annotation_cohort_v1","input":{"path":args.input,"sha256":input_hash},
        "exclusion_inputs":exclusion_inputs,"records_sha256":sha256(fs::read(output)?),"selection":selection,
        "source_sha256":sha256(include_bytes!("select_annotation_cohort.rs")),
        "executable_sha256":sha256(fs::read(std::env::current_exe()?)?),
        "model_calls":0,"detector_calls":0,"pangram_calls":0
    });
    fs::write(
        args.output_dir.join("selection.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report["selection"]["summary"])?
    );
    Ok(())
}
