//! Realised slice minimums of the Test bundle (protocol
//! `gated_slices.realised_minimum`), from pipeline counts only, after Test
//! acquisition and generation and before any Test scoring.
//!
//! ```text
//! a_comp_tierc_slice_minimums --seal SEAL.json --seal-sha256 SHA --bundle TEST_BUNDLE_DIR \
//!   --held-out-generator olmo2 --out RECORD.json
//! ```
//!
//! Membership and status are the evaluator's own (`tierc::gated_slice_members`,
//! `tierc::realised_status`): a listed slice below 15 source groups or 150
//! human documents (frontier: below 150 frontier positives) is reported
//! ungated with its shortfall. Reads metadata only (`comp_eval::parse_rows`).
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_a_baseline::comp_eval::parse_rows;
use grammar_a_baseline::tierc::{gated_slice_members, realised_status};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, BTreeSet}, fs, io::Write, path::PathBuf};

#[derive(Parser)]
#[command(about = "Record the realised Test slice minimums from pipeline counts (no network)")]
struct Args {
    #[arg(long)]
    seal: PathBuf,
    #[arg(long)]
    seal_sha256: String,
    #[arg(long)]
    bundle: PathBuf,
    #[arg(long = "held-out-generator", required = true)]
    held_out_generators: Vec<String>,
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    let a = Args::parse();
    ensure!(!a.out.exists(), "{} exists: the record is written once", a.out.display());
    ensure!(a.seal.is_file(), "the seal {} does not exist: Test is sealed", a.seal.display());
    let seal = fs::read(&a.seal)?;
    let seal_sha = hex::encode(Sha256::digest(&seal));
    ensure!(seal_sha == a.seal_sha256, "SEAL.json hashes to {seal_sha}, not the stated digest");
    let raw = fs::read(a.bundle.join("rows.jsonl")).context("bundle rows")?;
    let rows_sha = hex::encode(Sha256::digest(&raw));
    let manifest: Value = serde_json::from_slice(&fs::read(a.bundle.join("MANIFEST.json"))?)?;
    ensure!(manifest["files"]["rows.jsonl"] == rows_sha.as_str(), "rows.jsonl differs from the bundle manifest");
    ensure!(manifest["seal"]["sha256"] == seal_sha.as_str(), "the bundle was built under another seal");
    let rows: Vec<_> = parse_rows(std::str::from_utf8(&raw)?)?.into_iter().filter(|r| r.split == "test").collect();
    ensure!(!rows.is_empty(), "no Test rows");
    let held: BTreeSet<String> = a.held_out_generators.iter().cloned().collect();
    let mut slices = Vec::new();
    let mut ungated = Vec::new();
    for (name, idx) in gated_slice_members(&rows, &held) {
        let s = realised_status(&rows, &name, &idx);
        if s["status"] != "gated" {
            ungated.push(json!({"slice": name, "shortfall": s["shortfall"]}));
        }
        slices.push(json!({"slice": name, "realised": s}));
    }
    let mut pops: BTreeMap<String, (usize, BTreeSet<&str>)> = BTreeMap::new();
    for r in &rows {
        let e = pops.entry(r.population.clone()).or_default();
        e.0 += 1;
        e.1.insert(r.group.as_str());
    }
    let rec = json!({"schema": "slop_ninja_a_comp_tierc_realised_slice_minimums_v1",
        "rule": "protocol gated_slices.realised_minimum: after Test acquisition and generation and before any Test scoring, a listed slice below 15 groups or 150 human documents (frontier: below 150 frontier positives) by pipeline counts is reported ungated with its shortfall; nothing else removes a slice",
        "membership": "tierc::gated_slice_members over the primary Test rows (domain slices; held_out_generator and frontier: their positives plus every primary Test human)",
        "held_out_generators": held, "seal_sha256": seal_sha, "bundle_rows_sha256": rows_sha,
        "slices": slices, "reported_ungated": ungated,
        "populations": pops.iter().map(|(k, (n, g))| (k.clone(), json!({"rows": n, "source_groups": g.len()}))).collect::<serde_json::Map<_, _>>(),
        "reads": "bundle metadata only (comp_eval::parse_rows); no text, no score", "network_calls": 0});
    let mut f = fs::OpenOptions::new().create_new(true).write(true).open(&a.out)?;
    f.write_all(&serde_json::to_vec_pretty(&rec)?)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!("{}", json!({"slices": rec["slices"].as_array().map(Vec::len), "reported_ungated": rec["reported_ungated"]}));
    Ok(())
}
