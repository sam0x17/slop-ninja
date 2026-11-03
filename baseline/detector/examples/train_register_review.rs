//! Select a score-independent human Train packet for complete manual reading.
use anyhow::{Result, ensure};
use clap::Parser;
use serde_json::json;
use slop_ninja_detector::dataset::{Origin, OriginRecord, Split, read_records, sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::PathBuf,
};

const TRAIN_SHA: &str = "023d6e5428236502e877bac0afe0e9e9ee4aa9d10ab38eb940ca8e915cf7502e";
const DOMAIN: &str = "slop-ninja-human-train-register-v1";
const PER_COLLECTION: usize = 4;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    train: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new packet path");
    ensure!(
        sha256(fs::read(&args.train)?) == TRAIN_SHA,
        "Changed Train archive"
    );
    let records = read_records(&args.train)?;
    ensure!(records.len() == 3833, "Unexpected Train size");
    let mut all_groups = BTreeSet::new();
    let mut human_groups = BTreeSet::new();
    let mut collections = BTreeMap::<String, Vec<(String, &OriginRecord)>>::new();
    for r in &records {
        ensure!(r.split == Some(Split::Train), "Non-Train record");
        all_groups.insert(&r.source_group);
        if r.origin == Origin::HumanOnly {
            ensure!(
                r.parent_id.is_none() && r.generation.is_none(),
                "Human root has a parent or generation"
            );
            ensure!(
                human_groups.insert(&r.source_group),
                "Multiple human roots in a family"
            );
            let rank = sha256(serde_json::to_vec(&(
                DOMAIN,
                &r.source.collection,
                &r.source_group,
                &r.id,
            ))?);
            collections
                .entry(r.source.collection.clone())
                .or_default()
                .push((rank, r));
        }
    }
    ensure!(
        all_groups.len() == 471 && human_groups == all_groups,
        "Missing or changed human families"
    );
    ensure!(collections.len() == 4, "Changed collection coverage");
    let mut populations = BTreeMap::new();
    let mut selected = vec![];
    for (collection, rows) in &mut collections {
        ensure!(rows.len() >= PER_COLLECTION, "Insufficient source roots");
        populations.insert(collection, rows.len());
        rows.sort_by(|a, b| (&a.0, &a.1.id).cmp(&(&b.0, &b.1.id)));
        for (rank, record) in rows.iter().take(PER_COLLECTION) {
            selected.push(json!({"selection_sha256":rank, "record":record}));
        }
    }
    ensure!(selected.len() == 16, "Incomplete review panel");
    ensure!(
        sha256(fs::read(&args.train)?) == TRAIN_SHA,
        "Train changed during selection"
    );
    let packet = json!({
        "schema":"slop_ninja_human_train_register_packet_v1",
        "train_sha256":TRAIN_SHA,
        "source_sha256":sha256(include_bytes!("train_register_review.rs")),
        "protocol_sha256":sha256(include_bytes!("../TRAIN_REGISTER_REVIEW_V1.md")),
        "selection":{"domain":DOMAIN,"per_collection":PER_COLLECTION,"ordering":"SHA256(JSON tuple(domain, collection, source_group, id)); id tie-breaker","uses_detector_scores":false},
        "training_rows":records.len(),"training_families":all_groups.len(),
        "human_roots_by_collection":populations,"selected":selected,
        "review_complete":false,"model_calls":0,"provider_calls":0,
        "data_or_training_changed":false
    });
    let mut file = fs::File::create_new(&args.out)?;
    serde_json::to_writer_pretty(&mut file, &packet)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    println!(
        "{}",
        json!({"packet_sha256":sha256(fs::read(&args.out)?),"selected":selected.len(),"human_roots_by_collection":populations,"review_complete":false})
    );
    Ok(())
}
