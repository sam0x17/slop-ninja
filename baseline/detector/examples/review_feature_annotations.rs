//! Reparse a declared Train family for inspection and compare with cached features.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::features::{Features, extract};
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{self, Origin, Split, sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
};

const BRIDGE: &str = include_str!("../../../grammar/crates/grammar-spacy/python/syntax_bridge.py");
#[derive(Parser)]
struct Args {
    #[arg(long)]
    records: PathBuf,
    #[arg(long)]
    features: PathBuf,
    #[arg(long)]
    python: String,
    #[arg(long)]
    source_group: String,
    #[arg(long)]
    out: PathBuf,
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new review directory");
    let records = dataset::read_records(&args.records)?;
    let mut selected = Vec::new();
    for origin in [Origin::HumanOnly, Origin::ModelOnly, Origin::Mixed] {
        let row = records
            .iter()
            .filter(|r| {
                r.split == Some(Split::Train)
                    && r.source_group == args.source_group
                    && r.origin == origin
            })
            .min_by_key(|r| &r.id)
            .context("Missing declared Train family/origin")?;
        selected.push(row);
    }
    let mut cached = BTreeMap::<String, Features>::new();
    for line in BufReader::new(fs::File::open(&args.features)?).lines() {
        let row: Value = serde_json::from_str(&line?)?;
        let id = row["id"].as_str().context("Missing feature ID")?;
        if selected.iter().any(|r| r.id == id) {
            let features: Features = serde_json::from_value(row["features"].clone())?;
            ensure!(
                row["features_sha256"] == sha256(serde_json::to_vec(&features)?),
                "Changed cache payload"
            );
            ensure!(
                cached.insert(id.into(), features).is_none(),
                "Duplicate cached ID"
            );
        }
    }
    ensure!(cached.len() == selected.len(), "Incomplete feature cache");
    let mut bridge = tempfile::NamedTempFile::new()?;
    bridge.write_all(BRIDGE.as_bytes())?;
    bridge.flush()?;
    let texts: Vec<_> = selected.iter().map(|r| r.text.as_str()).collect();
    let docs = grammar_spacy::parse_batch_with_bridge(&texts, &args.python, bridge.path())?;
    fs::create_dir_all(&args.out)?;
    let mut summary = Vec::new();
    for (index, (row, doc)) in selected.iter().zip(docs).enumerate() {
        let doc = doc.map_err(anyhow::Error::msg)?;
        let mut features = extract(&doc)?;
        features
            .parser_identity
            .push_str(&format!(";detector-bridge-sha256={}", sha256(BRIDGE)));
        ensure!(
            features == cached[&row.id],
            "Reparsed features differ from cache for {}",
            row.id
        );
        let bytes = serde_json::to_vec_pretty(&doc)?;
        fs::write(args.out.join(format!("{index}.annotation.json")), &bytes)?;
        summary.push(
            json!({"index":index,"id":row.id,"origin":row.origin,"evidence":row.evidence,
            "text_sha256":row.text_sha256,"annotation_sha256":sha256(&bytes),
            "features_match_cache":true,"sentences":doc.sentences.len(),"tokens":doc.tokens.len()}),
        );
    }
    let report = json!({"schema":"slop_ninja_feature_annotation_review_preparation_v1",
        "source_group":args.source_group,"records_sha256":sha256(fs::read(&args.records)?),
        "features_file_sha256":sha256(fs::read(&args.features)?),"bridge_sha256":sha256(BRIDGE),
        "rows":summary,"selection":"lexicographically first ID per origin in the declared Train family",
        "human_review_complete":false,"parse_accuracy":null});
    fs::write(
        args.out.join("preparation.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}
