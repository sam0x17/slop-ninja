//! Prepare exact text for the retained Train population only. No Dev/CAL/Test
//! numeric files are opened. Prefix computation is reusable model work, not fit.
//! OLD_NUMERIC_ROOT ORIGINAL_DATASET EXTENSION_DATASET NEW_OUTPUT
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn put(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn load_dataset(root: &Path) -> Result<(Vec<Value>, Value)> {
    let mb = fs::read(root.join("manifest.json"))?;
    let m: Value = serde_json::from_slice(&mb)?;
    ensure!(
        m["complete"] == true && m["test_text_opened"] == false,
        "non-Test complete dataset required"
    );
    let rb = fs::read(root.join("records.jsonl"))?;
    ensure!(m["records_sha256"] == sha(&rb), "dataset changed");
    let mut rows = Vec::new();
    for line in std::str::from_utf8(&rb)?.lines() {
        let v: Value = serde_json::from_str(line)?;
        ensure!(
            ["train", "development", "calibration"].contains(&string(&v, "frozen_split")?),
            "Test input forbidden"
        );
        if v["frozen_split"] == "train" && v["purpose"] == "query" {
            ensure!(
                m["split_assignments"][string(&v, "source_group")?] == "train",
                "family split mismatch"
            );
            ensure!(
                v["text_sha256"] == sha(string(&v, "text")?.as_bytes()),
                "text hash mismatch"
            );
            ensure!(
                v["rights"]["commercial_training"] == true,
                "rights unavailable"
            );
            rows.push(v);
        }
    }
    Ok((
        rows,
        json!({"path":root.canonicalize()?,"manifest_sha256":sha(&mb),"records_sha256":sha(&rb)}),
    ))
}
fn run(numeric: &Path, original: &Path, extension: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    let m = read(&numeric.join("manifest.json"))?;
    ensure!(
        m["final_test_opened"] == false && m["test_used_for_selection"] == false,
        "numeric Test flag"
    );
    let nb = fs::read(numeric.join("train.jsonl"))?;
    let split = m["splits"]
        .as_array()
        .context("numeric splits")?
        .iter()
        .find(|x| x["split"] == "train")
        .context("Train binding")?;
    ensure!(split["sha256"] == sha(&nb), "numeric Train changed");
    let mut wanted = BTreeMap::new();
    let mut families = BTreeSet::new();
    for line in std::str::from_utf8(&nb)?.lines() {
        let v: Value = serde_json::from_str(line)?;
        ensure!(
            v["frozen_split"] == "train" && v["purpose"] == "query",
            "non-Train numeric row"
        );
        families.insert(string(&v, "source_group")?.to_owned());
        ensure!(
            wanted.insert(string(&v, "id")?.to_owned(), v).is_none(),
            "duplicate numeric id"
        );
    }
    ensure!(
        split["rows"].as_u64() == Some(wanted.len() as u64)
            && split["families"].as_u64() == Some(families.len() as u64),
        "numeric roster differs"
    );
    let mut found = BTreeMap::new();
    let mut bindings = Vec::new();
    for root in [original, extension] {
        let (rows, binding) = load_dataset(root)?;
        bindings.push(binding);
        for r in rows {
            let id = string(&r, "id")?.to_owned();
            if let Some(n) = wanted.get(&id) {
                ensure!(
                    n["source_group"] == r["source_group"],
                    "source group changed"
                );
                let label = match string(&r, "origin")? {
                    "human_only" => 0,
                    "model_only" => 1,
                    "mixed" => 2,
                    _ => anyhow::bail!("unknown origin"),
                };
                ensure!(
                    n["targets"]["origin"] == label && n["targets"]["share"] == r["share_target"],
                    "origin/share target differs"
                );
                for role in ["human", "ai"] {
                    ensure!(
                        n["roles"][role]["present"] == r[format!("{role}_role_present")]
                            && n["roles"][role]["identity"] == r[format!("{role}_identity")],
                        "role supervision differs"
                    );
                }
                ensure!(found.insert(id, r).is_none(), "duplicate source id");
            }
        }
    }
    ensure!(
        found.keys().eq(wanted.keys()),
        "retained Train population incomplete"
    );
    let mut requests = Vec::new();
    let mut metadata = Vec::new();
    for (id, r) in &found {
        serde_json::to_writer(&mut requests, &json!({"id":id,"text":r["text"]}))?;
        requests.push(b'\n');
        let mut n = wanted[id].clone();
        n.as_object_mut().context("row")?.remove("input");
        n["text_sha256"] = r["text_sha256"].clone();
        serde_json::to_writer(&mut metadata, &n)?;
        metadata.push(b'\n');
    }
    fs::create_dir(out)?;
    put(&out.join("requests.jsonl"), &requests)?;
    put(&out.join("labels.jsonl"), &metadata)?;
    let report = json!({"schema":"slop_ninja_retained_train_encoder_requests_v1","complete":true,"records":found.len(),"source_families":families.len(),"requests_sha256":sha(&requests),"labels_sha256":sha(&metadata),"numeric_train_sha256":sha(&nb),"numeric_manifest_sha256":sha(&fs::read(numeric.join("manifest.json"))?),"sources":bindings,"permitted_split":"train","test_text_opened":false,"old_development_calibration_exported":false,"learned_inputs":"exact text only; all labels, IDs, provenance and family routing remain outside learned inputs","implementation_sha256":sha(include_bytes!("prepare_encoder_train_requests.rs"))});
    put(
        &out.join("manifest.json"),
        &serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "{}",
        json!({"records":found.len(),"families":families.len(),"split":"train"})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 4, "NUMERIC ORIGINAL EXTENSION OUTPUT");
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
    )
}
