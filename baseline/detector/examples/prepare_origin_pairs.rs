//! Pair admitted Train revision chains without adding text or changing labels.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::json;
use slop_ninja_detector::dataset::{self, Evidence, Origin, Split, sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::PathBuf,
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    train: PathBuf,
    #[arg(long)]
    expected_sha256: String,
    #[arg(long)]
    whitespace: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new manifest path");
    let bytes = fs::read(&args.train)?;
    ensure!(
        sha256(&bytes) == args.expected_sha256,
        "Changed Train archive"
    );
    let records = dataset::read_records(&args.train)?;
    let alternate = dataset::read_records(&args.whitespace)?;
    let by_id: BTreeMap<_, _> = records.iter().map(|r| (r.id.as_str(), r)).collect();
    let alt: BTreeMap<_, _> = alternate.iter().map(|r| (r.id.as_str(), r)).collect();
    ensure!(
        by_id.len() == records.len() && alt.len() == records.len(),
        "Duplicate or incomplete rows"
    );
    let mut chains = BTreeMap::<&str, Vec<(&str, usize)>>::new();
    let mut pairs = BTreeMap::new();
    for row in &records {
        ensure!(
            row.split == Some(Split::Train),
            "Only Train may enter pairing"
        );
        let other = alt
            .get(row.id.as_str())
            .context("Missing formatting counterpart")?;
        ensure!(
            other.split == row.split
                && other.source_group == row.source_group
                && other.origin == row.origin
                && other.evidence == row.evidence
                && other.parent_id == row.parent_id
                && other
                    .text
                    .split_whitespace()
                    .eq(row.text.split_whitespace()),
            "Changed formatting counterpart"
        );
        if row.origin != Origin::ModelOnly {
            pairs.insert(row.id.as_str(), row.id.as_str());
            continue;
        }
        let mut parent = row;
        let mut seen = BTreeSet::new();
        let mut depth = 0;
        while parent.evidence == Evidence::RecordedModelRevision {
            ensure!(seen.insert(parent.id.as_str()), "Cyclic revision chain");
            parent = by_id
                .get(parent.parent_id.as_deref().context("Missing parent ID")?)
                .context("Missing revision parent")?;
            ensure!(
                parent.origin == Origin::ModelOnly && parent.source_group == row.source_group,
                "Changed revision origin/family"
            );
            depth += 1;
        }
        ensure!(
            parent.evidence == Evidence::RecordedModelGeneration,
            "Unrecorded model origin"
        );
        chains.entry(&parent.id).or_default().push((&row.id, depth));
    }
    let mut multi_record_chains = 0;
    for chain in chains.values_mut() {
        chain.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(b.0)));
        ensure!(
            chain.iter().enumerate().all(|(i, (_, depth))| i == *depth),
            "Branched or incomplete chain"
        );
        multi_record_chains += usize::from(chain.len() > 1);
        for (i, (id, _)) in chain.iter().enumerate() {
            pairs.insert(*id, chain[(i + 1) % chain.len()].0);
        }
    }
    ensure!(
        pairs.len() == records.len()
            && pairs.values().copied().collect::<BTreeSet<_>>().len() == records.len(),
        "Pairing is not a permutation"
    );
    let changed = pairs.iter().filter(|(a, b)| *a != *b).count();
    let manifest = json!({"schema":"slop_ninja_origin_pairs_v1",
        "train_sha256":sha256(&bytes),"whitespace_sha256":sha256(fs::read(&args.whitespace)?),
        "records":records.len(),"model_revision_pairs":changed,"multi_record_model_chains":multi_record_chains,
        "formatting_only_pairs":records.len()-changed,"pairs":pairs,
        "policy":"Cycle each unbranched recorded model-only lineage in depth order, returning its last revision to its original draft. Other records pair with themselves. The trainer exposes the opposite frozen formatting view for the peer. Every source record occurs once as anchor and once as peer, within its existing family and origin. No new text or label is introduced.",
        "meaning_equivalence_certified":false,"new_provider_calls":0});
    let mut file = fs::File::create_new(&args.out)?;
    serde_json::to_writer_pretty(&mut file, &manifest)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    println!(
        "{}",
        json!({"records":records.len(),"revision_pairs":changed,"multi_record_chains":multi_record_chains,"formatting_only_pairs":records.len()-changed})
    );
    Ok(())
}
