//! Inspect one encountered compilation against the frozen, already opened shards.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::features::words;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{OriginRecord, Split, read_records, sha256};
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};

const N: usize = 8;
const PACKET: &str = "6d6542888c3a7eb3146e5e0de10518b144043a0efe9cf2b9f36c2230f4b6e15e";
const TARGET: &str = "wikinews:1445:1389307:c309c2f796c5a3d2";
const SHARDS: [(&str, Split, usize, &str); 4] = [
    (
        "train",
        Split::Train,
        3833,
        "023d6e5428236502e877bac0afe0e9e9ee4aa9d10ab38eb940ca8e915cf7502e",
    ),
    (
        "development",
        Split::Development,
        530,
        "c9ee825909a37b7aee0863c00437f2375d5569f702de80c12f515af1df29a78b",
    ),
    (
        "calibration",
        Split::Calibration,
        540,
        "e26ed741c1fbe2d64dc7e8d49211573eaa44b11589524ce76f2b487a9b711031",
    ),
    (
        "test",
        Split::Test,
        560,
        "9d49faf9f9415352d49f286c523f24af0677128ac149478d7e422190b5c896b4",
    ),
];

#[derive(Parser)]
struct Args {
    #[arg(long)]
    packet: PathBuf,
    #[arg(long)]
    shards: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new report");
    let packet_bytes = fs::read(&args.packet)?;
    ensure!(sha256(&packet_bytes) == PACKET, "Changed review packet");
    let packet: Value = serde_json::from_slice(&packet_bytes)?;
    let target: OriginRecord = serde_json::from_value(
        packet["selected"]
            .as_array()
            .context("Missing selection")?
            .iter()
            .find(|r| r["record"]["id"] == TARGET)
            .context("Digest was not selected")?["record"]
            .clone(),
    )?;
    target.validate()?;
    let query = words(&target.text);
    ensure!(query.len() >= N, "Insufficient query text");
    let mut index = BTreeMap::<Vec<String>, Vec<usize>>::new();
    for (i, w) in query.windows(N).enumerate() {
        index.entry(w.to_vec()).or_default().push(i);
    }
    let mut counts = BTreeMap::new();
    let mut matches = vec![];
    let mut target_found = false;
    for (name, split, expected_count, expected_hash) in SHARDS {
        let path = args.shards.join(format!("{name}.jsonl"));
        ensure!(
            sha256(fs::read(&path)?) == expected_hash,
            "Changed {name} shard"
        );
        let records = read_records(&path)?;
        ensure!(records.len() == expected_count, "Changed {name} count");
        let mut origins = BTreeMap::new();
        let mut matched_rows = 0;
        for record in records {
            ensure!(record.split == Some(split), "Wrong split");
            *origins.entry(record.origin).or_insert(0_usize) += 1;
            if record.id == TARGET {
                ensure!(
                    split == Split::Train
                        && record.text == target.text
                        && record.source_group == target.source_group
                        && !target_found,
                    "Changed target"
                );
                target_found = true;
            }
            let candidate = words(&record.text);
            let mut runs = vec![];
            for (j, window) in candidate.windows(N).enumerate() {
                if let Some(starts) = index.get(window) {
                    for &i in starts {
                        if i > 0 && j > 0 && query[i - 1] == candidate[j - 1] {
                            continue;
                        }
                        let mut length = N;
                        while i + length < query.len()
                            && j + length < candidate.len()
                            && query[i + length] == candidate[j + length]
                        {
                            length += 1;
                        }
                        runs.push(json!({"digest_range":[i,i+length],"record_range":[j,j+length],"words":length}));
                    }
                }
            }
            if !runs.is_empty() {
                matched_rows += 1;
                matches.push(json!({"split":split,"id":record.id,"source_group":record.source_group,
                    "origin":record.origin,"evidence":record.evidence,"text_sha256":record.text_sha256,
                    "same_family":record.source_group==target.source_group,"normalized_words":candidate.len(),
                    "review_at_least_20_words":runs.iter().any(|r|r["words"].as_u64().unwrap()>=20),"runs":runs}));
            }
        }
        counts.insert(name,json!({"records":expected_count,"origins":origins,"records_with_runs_at_least_8":matched_rows,"sha256":expected_hash}));
    }
    ensure!(target_found, "Target absent from Train");
    for (name, _, _, hash) in SHARDS {
        ensure!(
            sha256(fs::read(args.shards.join(format!("{name}.jsonl")))?) == hash,
            "Shard changed during audit"
        );
    }
    ensure!(
        sha256(fs::read(&args.packet)?) == PACKET,
        "Packet changed during audit"
    );
    let cross_family: Vec<_> = matches
        .iter()
        .filter(|r| r["same_family"] == false)
        .collect();
    let result = json!({"schema":"slop_ninja_train_digest_overlap_v1","packet_sha256":PACKET,
        "source_sha256":sha256(include_bytes!("train_digest_overlap.rs")),
        "protocol_sha256":sha256(include_bytes!("../TRAIN_DIGEST_OVERLAP_V1.md")),
        "target_id":TARGET,"target_text_sha256":target.text_sha256,"target_normalized_words":query.len(),
        "counts":counts,"minimum_shared_normalized_words":N,"review_contiguous_words":20,
        "matched_records":matches.len(),"cross_family_matched_records":cross_family.len(),
        "cross_family_review_records":cross_family.iter().filter(|r|r["review_at_least_20_words"]==true).count(),
        "matches":matches,"inference_calls":0,"provider_calls":0,"data_changed":false,
        "limitation":"One digest and exact normalized contiguous runs only; no corpus-wide independence or semantic-overlap claim."});
    let mut file = fs::File::create_new(&args.out)?;
    serde_json::to_writer_pretty(&mut file, &result)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    println!(
        "{}",
        json!({"matched_records":result["matched_records"],"cross_family_matched_records":result["cross_family_matched_records"],"cross_family_review_records":result["cross_family_review_records"]})
    );
    Ok(())
}
