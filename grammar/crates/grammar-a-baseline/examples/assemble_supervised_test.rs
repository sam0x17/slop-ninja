//! Final-Test assembly after an explicit saved model-selection freeze.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn rows(bytes: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(bytes)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn normalized(row: &Value) -> Result<String> {
    Ok(sha(row["text"]
        .as_str()
        .context("text")?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .as_bytes()))
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() >= 5,
        "assemble_supervised_test FITTING_RECORDS SELECTION_FREEZE NEW_OUT TEST_RECORDS..."
    );
    let previous = fs::read(&a[0])?;
    let freeze_bytes = fs::read(&a[1])?;
    let freeze: Value = serde_json::from_slice(&freeze_bytes)?;
    ensure!(
        freeze["schema"] == "slop_ninja_a_selection_freeze_v1"
            && freeze["test_used_for_selection"] == false,
        "selection freeze required"
    );
    ensure!(
        freeze["fitting_records_sha256"] == sha(&previous),
        "fitting population freeze mismatch"
    );
    let mut old_groups = BTreeSet::new();
    let mut old_ids = BTreeSet::new();
    let mut old_texts = BTreeSet::new();
    for row in rows(&previous)? {
        old_groups.insert(row["source_group"].as_str().context("family")?.to_owned());
        old_ids.insert(row["id"].as_str().context("id")?.to_owned());
        old_texts.insert(normalized(&row)?);
    }
    let mut all = Vec::new();
    let mut sources = Vec::new();
    let mut ids = BTreeSet::new();
    let mut groups = BTreeMap::new();
    for path in &a[3..] {
        let b = fs::read(path)?;
        sources.push(json!({"path":path,"sha256":sha(&b)}));
        for row in rows(&b)? {
            let id = row["id"].as_str().context("id")?;
            let g = row["source_group"].as_str().context("family")?;
            ensure!(
                row["frozen_split"] == "test" && row["purpose"] == "query",
                "only Test queries"
            );
            ensure!(
                !old_groups.contains(g) && !old_ids.contains(id) && ids.insert(id.to_owned()),
                "family/id leakage"
            );
            ensure!(
                row["text_sha256"] == sha(row["text"].as_str().context("text")?.as_bytes()),
                "text hash"
            );
            ensure!(
                row["rights"]["commercial_training"] == true,
                "source rights"
            );
            groups.insert(g.to_owned(), "test");
            all.push(row);
        }
    }
    let requested = all.len();
    let mut excluded = Vec::new();
    let mut accepted = Vec::new();
    let mut test_texts = BTreeSet::new();
    all.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    for row in all {
        let h = normalized(&row)?;
        if old_texts.contains(&h) || test_texts.contains(&h) {
            ensure!(
                row["origin"] == "model_only" && row["generation"]["mode"] == "from_scratch",
                "source/descendant duplicate in Test requires source-family audit: {}",
                row["id"]
            );
            excluded.push(json!({"id":row["id"],"source_group":row["source_group"],"reason":"from_scratch_duplicate_of_prior_or_retained_Test_text","normalized_sha256":h}));
        } else {
            test_texts.insert(h);
            accepted.push(row);
        }
    }
    let mut bytes = Vec::new();
    for r in &accepted {
        serde_json::to_writer(&mut bytes, r)?;
        bytes.push(b'\n');
    }
    let out = Path::new(&a[2]);
    fs::create_dir(out)?;
    fs::write(out.join("records.jsonl"), &bytes)?;
    let manifest = json!({"schema":"slop_ninja_a_supervised_dataset_v1","records_sha256":sha(&bytes),"test_records_sha256":sha(&bytes),"input_sha256":sha(&bytes),"non_test_ids":old_ids,"test_ids":accepted.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"split_assignments":groups,"sources":sources,"selection_freeze_sha256":sha(&freeze_bytes),"fitting_records_sha256":sha(&previous),"requested_records":requested,"records":accepted.len(),"excluded":excluded,"test_text_opened":true,"test_used_for_selection":false,"clean_holdout":false,"exposure":"public historical detector Test is prior research exposed; author works were reserved for this model; prior grammar exposure conservatively unknown"});
    fs::write(
        out.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!(
        "{}",
        json!({"requested":requested,"retained":accepted.len(),"excluded":excluded.len()})
    );
    Ok(())
}
