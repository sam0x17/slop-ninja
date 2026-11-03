//! Allocate the two closed long-source cohorts before generating or scoring.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{self, Origin, OriginRecord, Split, sha256},
    prompt_profiles,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const POLICY: &str = include_str!("../LONG_STYLE_COMPARISON_V1.md");
const COHORTS: [(&str, &str, usize); 2] = [
    (
        "long-sources-v1",
        "5ff946daf1b33d57971103c41b87ea857cef120e71ce488c273086d75df66596",
        47,
    ),
    (
        "long-sources-v2",
        "ecaad984a0dc5d9e9d6252cf795bff870c6b73ba5585b623652eec83953a6ebe",
        30,
    ),
];
#[derive(Parser)]
struct Args {
    #[arg(long)]
    cohorts: PathBuf,
    #[arg(long)]
    train: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
fn object(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn save(p: &Path, v: &impl serde::Serialize) -> Result<()> {
    let mut file = fs::File::create_new(p)?;
    serde_json::to_writer_pretty(&mut file, v)?;
    file.write_all(b"\n")?;
    Ok(())
}
fn digest(p: &Path) -> Result<String> {
    Ok(sha256(fs::read(p)?))
}
fn key(r: &OriginRecord) -> String {
    sha256(grammar_core::features::words(&r.text).join(" "))
}
fn main() -> Result<()> {
    let a = Args::parse();
    ensure!(!a.out.exists(), "Use a new allocation directory");
    ensure!(
        digest(&a.train)? == "023d6e5428236502e877bac0afe0e9e9ee4aa9d10ab38eb940ca8e915cf7502e",
        "Changed active Train archive"
    );
    let train = dataset::read_records(&a.train)?;
    let mut profiles = BTreeMap::<String, usize>::new();
    let mut generators = BTreeMap::<String, usize>::new();
    let mut old_groups = BTreeSet::new();
    let mut old_text = BTreeSet::new();
    for r in &train {
        old_groups.insert(&r.source_group);
        old_text.insert(key(r));
        if let Some(g) = &r.generation {
            *generators
                .entry(format!("{}@{}", g.model_id, g.model_revision))
                .or_default() += 1;
            if let Some(p) = &g.prompt_profile {
                *profiles.entry(p.profile.id.clone()).or_default() += 1;
            }
        }
    }
    ensure!(
        !profiles.contains_key("formal") && !profiles.contains_key("editorial"),
        "Requested profile was used in Train"
    );
    ensure!(
        !generators
            .keys()
            .any(|g| g.to_lowercase().contains("phi-4")),
        "Phi-4 was used in Train"
    );
    let mut all = vec![];
    let mut allocation = vec![];
    let mut groups = BTreeSet::new();
    let mut texts = BTreeSet::new();
    let mut bindings = BTreeMap::new();
    bindings.insert(a.train.canonicalize()?, digest(&a.train)?);
    for (name, expected, count) in COHORTS {
        let root = a.cohorts.join(name);
        let corpus = root.join("admitted/human.jsonl");
        ensure!(digest(&corpus)? == expected, "Changed {name} corpus");
        let done = object(&root.join("completion.json"))?;
        ensure!(
            done["exit_code"] == 0
                && done["phase"] == "sources_admitted_audited_unassigned_unscored",
            "Source cohort is not closed"
        );
        let admission = object(&root.join("admitted/summary.json"))?;
        let audit = object(&root.join("token-audit.json"))?;
        ensure!(
            admission["corpus_sha256"] == expected
                && audit["shards"]["unassigned"]["sha256"] == expected
                && audit["max_tokens"] == 2048
                && audit["model_executed"] == false,
            "Changed admission or token audit"
        );
        let rows = dataset::read_records(&corpus)?;
        ensure!(rows.len() == count, "Changed cohort count");
        let tokens = audit["shards"]["unassigned"]["record_token_counts"]
            .as_array()
            .context("Missing token counts")?;
        let mut seen_tokens = BTreeSet::new();
        for (index, mut r) in rows.into_iter().enumerate() {
            ensure!(
                r.split.is_none() && r.origin == Origin::HumanOnly && r.parent_id.is_none(),
                "Expected unassigned human roots"
            );
            ensure!(
                groups.insert(r.source_group.clone())
                    && texts.insert(key(&r))
                    && !old_groups.contains(&r.source_group)
                    && !old_text.contains(&key(&r)),
                "Overlapping source or text"
            );
            let token = tokens
                .iter()
                .find(|t| t["id"] == r.id)
                .context("Missing root token count")?;
            ensure!(
                token["text_sha256"] == r.text_sha256 && seen_tokens.insert(r.id.clone()),
                "Changed token audit identity"
            );
            let n = token["token_count"]
                .as_u64()
                .context("Missing token length")?;
            let pilot = index < 3;
            r.split = Some(if pilot {
                Split::Development
            } else {
                Split::Test
            });
            allocation.push(
                json!({"id":r.id,"source_group":r.source_group,"text_sha256":r.text_sha256,
                "cohort":name,"admission_index":index,"role":if pilot {"pilot"}else{"confirmation"},
                "split":r.split,"token_count":n,"supported_by_current_A":n<=2048}),
            );
            all.push(r);
        }
        ensure!(
            seen_tokens.len() == tokens.len(),
            "Extra or duplicate token audit entries"
        );
        for relative in [
            "admitted/human.jsonl",
            "admitted/summary.json",
            "token-audit.json",
            "completion.json",
        ] {
            let p = root.join(relative);
            bindings.insert(p.canonicalize()?, digest(&p)?);
        }
    }
    dataset::validate_records(&all)?;
    let pilot: Vec<_> = all
        .iter()
        .filter(|r| r.split == Some(Split::Development))
        .cloned()
        .collect();
    let confirmation: Vec<_> = all
        .iter()
        .filter(|r| r.split == Some(Split::Test))
        .cloned()
        .collect();
    ensure!(
        pilot.len() == 6 && confirmation.len() == 71,
        "Changed allocation"
    );
    fs::create_dir_all(&a.out)?;
    let mut output_hashes = BTreeMap::new();
    for (name, records) in [
        ("all-roots.jsonl", &all),
        ("pilot-roots.jsonl", &pilot),
        ("confirmation-roots.jsonl", &confirmation),
    ] {
        dataset::write_records(&a.out.join(name), records)?;
        output_hashes.insert(name, digest(&a.out.join(name))?);
    }
    fs::write(a.out.join("LONG_STYLE_COMPARISON_V1.md"), POLICY)?;
    for id in ["source-matched", "formal", "editorial"] {
        let selection = prompt_profiles::Selection::new("style-mix-v1", Some(&[id.into()]))?;
        let assignments =
            prompt_profiles::assign(&pilot, &output_hashes["pilot-roots.jsonl"], &selection)?;
        save(
            &a.out.join(format!("pilot-{id}-profiles.json")),
            &assignments,
        )?;
    }
    let report = json!({"schema":"slop_ninja_long_style_allocation_v1","policy_sha256":sha256(POLICY),
        "executable_sha256":digest(&std::env::current_exe()?)?,"input_bindings":bindings,"output_sha256":output_hashes,
        "allocations":allocation,"pilot_sources":6,"confirmation_sources":71,
        "confirmation_supported_sources":allocation.iter().filter(|r|r["role"]=="confirmation" && r["supported_by_current_A"]==true).count(),
        "training_profiles":profiles,"training_generators":generators,"optimizer_training_sources_added":0,
        "new_provider_calls":0,"detector_scores_opened":false,"pilot_generation_call_cap":36,
        "confirmation_generation_authorized_by_this_preparer":false});
    save(&a.out.join("allocation.json"), &report)?;
    println!(
        "{}",
        json!({"status":"allocated_unscored","roots":77,"pilot":6,"confirmation":71,
        "confirmation_supported":report["confirmation_supported_sources"],"generation_calls":0})
    );
    Ok(())
}
