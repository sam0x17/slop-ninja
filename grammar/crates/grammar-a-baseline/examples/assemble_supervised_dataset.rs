//! Bind already selected non-Test records before parsing or model fitting.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() >= 3,
        "assemble_supervised_dataset NEW_OUT RESERVED_ID_MANIFEST RECORDS..."
    );
    let out = Path::new(&a[0]);
    ensure!(!out.exists(), "new output required");
    let reserved_bytes = fs::read(&a[1])?;
    let reserved: Value = serde_json::from_slice(&reserved_bytes)?;
    let test_ids = reserved["test_ids"]
        .as_array()
        .context("Test IDs only required")?;
    let mut split_assignments = BTreeMap::new();
    let mut texts = BTreeMap::<String, BTreeSet<String>>::new();
    let mut ids = BTreeSet::new();
    let mut records = Vec::new();
    let mut inputs = Vec::new();
    for path in &a[2..] {
        let bytes = fs::read(path)?;
        inputs.push(json!({"path":path,"sha256":sha(&bytes)}));
        for line in std::str::from_utf8(&bytes)?.lines() {
            let row: Value = serde_json::from_str(line)?;
            let id = row["id"].as_str().context("id")?;
            ensure!(ids.insert(id.to_owned()), "duplicate row ID");
            ensure!(!test_ids.iter().any(|x| x == id), "reserved Test ID");
            let split = row["frozen_split"].as_str().context("split")?;
            ensure!(
                matches!(split, "train" | "development" | "calibration"),
                "nonfitting split"
            );
            let group = row["source_group"].as_str().context("source family")?;
            if let Some(prior) = split_assignments.insert(group.to_owned(), split.to_owned()) {
                ensure!(prior == split, "family crosses splits");
            }
            let text = row["text"].as_str().context("text")?;
            ensure!(row["text_sha256"] == sha(text.as_bytes()), "text changed");
            let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
            texts
                .entry(sha(normalized.as_bytes()))
                .or_default()
                .insert(split.to_owned());
            ensure!(
                row["rights"]["commercial_training"] == true,
                "source not admitted"
            );
            ensure!(row["exposed_to_grammar"].is_boolean(), "exposure missing");
            ensure!(
                row["purpose"] == "query" || row["purpose"] == "reference",
                "purpose missing"
            );
            records.push(row);
        }
    }
    // Independent model-only prompts can collide under deterministic decoding.
    // Drop every copy of such a cross-split duplicate, not just evaluation copies.
    // A collision involving source text or a descendant remains a hard error.
    let requested = records.len();
    let mut excluded = Vec::new();
    let mut retained = Vec::new();
    for row in records {
        let normalized = row["text"]
            .as_str()
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let digest = sha(normalized.as_bytes());
        if texts[&digest].len() > 1 {
            ensure!(
                row["origin"] == "model_only" && row["generation"]["mode"] == "from_scratch",
                "source/descendant duplicate crosses splits: {}",
                row["id"]
            );
            excluded.push(json!({"id":row["id"],"source_group":row["source_group"],"split":row["frozen_split"],"text_sha256":row["text_sha256"],"normalized_sha256":digest,"reason":"all_copies_of_cross_split_from_scratch_duplicate_excluded"}));
        } else {
            retained.push(row);
        }
    }
    let mut records = retained;
    records.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let mut bytes = Vec::new();
    for row in &records {
        serde_json::to_writer(&mut bytes, row)?;
        bytes.push(b'\n');
    }
    fs::create_dir(out)?;
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out.join("records.jsonl"))?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    let manifest = json!({"schema":"slop_ninja_a_supervised_dataset_v1","records_sha256":sha(&bytes),"input_sha256":sha(&bytes),
        "non_test_ids":records.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"test_ids":test_ids,
        "split_assignments":split_assignments,"sources":inputs,"reserved_manifest_sha256":sha(&reserved_bytes),
        "requested_records":requested,"records":records.len(),"excluded":excluded,"source_families":split_assignments.len(),"test_text_opened":false,
        "clean_holdout":false,"exposure":"historic detector corpus exposed; fresh author source grammar exposure conservatively unknown; no clean-holdout claim",
        "labels_are_not_inputs":true,"parser_frozen":"grammar/a-document-input-v1.json",
        "generation_failures":"see source generation report; never substituted with labels",
        "rights_scope":"per-record public corpus permissions or pinned public-domain work plus local generated descendants; not private correspondence"});
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out.join("manifest.json"))?;
    serde_json::to_writer_pretty(&mut f, &manifest)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!(
        "{}",
        json!({"records":records.len(),"families":split_assignments.len()})
    );
    Ok(())
}
