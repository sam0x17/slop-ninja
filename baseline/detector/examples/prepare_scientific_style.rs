//! Allocate admitted scientific roots before any new model request.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{self, Evidence, Origin, Split, sha256},
    prompt_profiles,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const POLICY: &str = include_str!("../SCIENTIFIC_STYLE_TRAIN_V1.md");
const RECORDS: &str = "99d0f65bc5e129a0d2824beab7e685820de9fb41d55425b50d96fc411b77f164";
const TRAIN: &str = "023d6e5428236502e877bac0afe0e9e9ee4aa9d10ab38eb940ca8e915cf7502e";
const PROFILES: [&str; 4] = ["source-matched", "plain", "informal", "fix-slop"];

#[derive(Parser)]
struct Args {
    #[arg(long)]
    admission: PathBuf,
    #[arg(long)]
    active_train: PathBuf,
    #[arg(long)]
    output: PathBuf,
}
fn save(p: &Path, v: &impl serde::Serialize) -> Result<()> {
    let mut f = fs::File::create_new(p)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.write_all(b"\n")?;
    Ok(())
}
fn digest(p: &Path) -> Result<String> {
    Ok(sha256(fs::read(p)?))
}

fn main() -> Result<()> {
    let a = Args::parse();
    ensure!(!a.output.exists(), "Use a new allocation directory");
    let source = a.admission.join("human.jsonl");
    let report_path = a.admission.join("report.json");
    let report: Value = serde_json::from_slice(&fs::read(&report_path)?)?;
    ensure!(
        digest(&source)? == RECORDS
            && report["records_sha256"] == RECORDS
            && report["admitted_records"] == 13
            && report["split_assigned"] == false,
        "Changed admission"
    );
    ensure!(
        digest(&a.active_train)? == TRAIN,
        "Changed active Train shard"
    );
    let existing = dataset::read_records(&a.active_train)?;
    let groups: BTreeSet<_> = existing.iter().map(|r| r.source_group.as_str()).collect();
    let texts: BTreeSet<_> = existing
        .iter()
        .map(|r| sha256(grammar_core::features::words(&r.text).join(" ")))
        .collect();
    let mut roots = dataset::read_records(&source)?;
    ensure!(roots.len() == 13, "Changed source count");
    let aliases = report["family_aliases"]
        .as_object()
        .context("Missing admitted family aliases")?;
    ensure!(
        aliases.keys().all(|g| !groups.contains(g.as_str())),
        "Related work already in Train"
    );
    for root in &mut roots {
        ensure!(
            root.split.is_none()
                && root.origin == Origin::HumanOnly
                && root.evidence == Evidence::HistoricalProxy
                && root.parent_id.is_none()
                && root.source.collection == "plos-introductions-historical-v1",
            "Unexpected source record"
        );
        ensure!(
            !groups.contains(root.source_group.as_str())
                && !texts.contains(&sha256(grammar_core::features::words(&root.text).join(" "))),
            "Overlapping training source"
        );
        root.split = Some(Split::Train);
    }
    roots.sort_by_key(|r| {
        (
            sha256(
                serde_json::to_vec(&json!([
                    "slop-ninja-scientific-style-pilot-v1",
                    r.source_group
                ]))
                .unwrap(),
            ),
            r.source_group.clone(),
        )
    });
    let pilot = roots[..3].to_vec();
    let remaining = roots[3..].to_vec();
    fs::create_dir(&a.output)?;
    fs::write(a.output.join("SCIENTIFIC_STYLE_TRAIN_V1.md"), POLICY)?;
    let mut outputs = BTreeMap::new();
    for (name, records) in [
        ("all-roots", &roots),
        ("pilot-roots", &pilot),
        ("remaining-roots", &remaining),
    ] {
        let file = a.output.join(format!("{name}.jsonl"));
        dataset::write_records(&file, records)?;
        outputs.insert(format!("{name}.jsonl"), digest(&file)?);
    }
    for (phase, records) in [("pilot", &pilot), ("remaining", &remaining)] {
        for id in PROFILES {
            let selection = prompt_profiles::Selection::new("style-mix-v1", Some(&[id.into()]))?;
            let assignments = prompt_profiles::assign(
                records,
                &outputs[&format!("{phase}-roots.jsonl")],
                &selection,
            )?;
            let name = format!("{phase}-{id}-profiles.json");
            save(&a.output.join(&name), &assignments)?;
            outputs.insert(name.clone(), digest(&a.output.join(name))?);
        }
    }
    let allocations: Vec<_> = roots
        .iter()
        .enumerate()
        .map(|(i, r)| {
            json!({"id":r.id,"source_group":r.source_group,
        "text_sha256":r.text_sha256,"split":r.split,"role":if i<3{"pilot"}else{"remaining"}})
        })
        .collect();
    let report = json!({"schema":"slop_ninja_scientific_style_allocation_v1","policy_sha256":sha256(POLICY),
        "source_records_sha256":RECORDS,"admission_report_sha256":digest(&report_path)?,
        "active_train_sha256":TRAIN,"output_sha256":outputs,"family_aliases":aliases,
        "source_families":roots.len(),"pilot_source_families":pilot.len(),"remaining_source_families":remaining.len(),
        "profiles":PROFILES,"generator_revisions":[
            "mlx-community/Qwen3-30B-A3B-Instruct-2507-4bit@e9675aa3ca5f900ccef55267914466d55ab325fa",
            "mlx-community/Mistral-Small-3.2-24B-Instruct-2506-4bit@2a1d5eabfc504747bdc24178394821a1efc0edde"],
        "pilot_call_cap":24,"remaining_call_cap":80,"remaining_launch_requires_pilot_review":true,
        "allocations":allocations,"generation_calls":0,"detector_calls":0,"active_training_inputs_changed":false});
    save(&a.output.join("allocation.json"), &report)?;
    println!(
        "{}",
        json!({"source_families":roots.len(),"pilot":pilot.len(),"remaining":remaining.len(),"split":"train","model_calls":0})
    );
    Ok(())
}
