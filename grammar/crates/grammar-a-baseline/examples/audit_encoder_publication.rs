//! Audit exact retained Train coverage in rights-reviewed published snapshots.
//! REPO STAGE NEW_PRIVATE_REPORT. No corpus mutation, fitting or network calls.
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
fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn rows(b: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(b)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
pub(crate) fn matches(label: &Value, published: &Value) -> bool {
    let origin = match published["origin"].as_str() {
        Some("human_only") => 0,
        Some("model_only") => 1,
        Some("mixed") => 2,
        _ => return false,
    };
    label["id"] == published["id"]
        && label["text_sha256"] == published["text_sha256"]
        && label["source_group"] == published["source_group"]
        && label["targets"]["origin"] == origin
        && published["frozen_split"] == "train"
        && (published["split"].is_null() || published["split"] == "train")
        && published["purpose"] == "query"
        && label["targets"]["share"] == published["share_target"]
        && label["targets"]["ai_involvement"] == (origin != 0)
        && ["human", "ai"].iter().all(|role| {
            label["roles"][*role]["present"] == published[format!("{role}_role_present")]
                && label["roles"][*role]["identity"] == published[format!("{role}_identity")]
        })
}
fn run(repo: &Path, stage: &Path, out: &Path) -> Result<Value> {
    ensure!(
        !out.exists()
            && out
                .parent()
                .context("report parent")?
                .canonicalize()?
                .starts_with(repo.join("data/grammar-private").canonicalize()?),
        "new private report required"
    );
    let root = stage.join("retained-train-requests-v1");
    let m = read(&root.join("manifest.json"))?;
    ensure!(
        m["schema"] == "slop_ninja_retained_train_encoder_requests_v1"
            && m["complete"] == true
            && m["permitted_split"] == "train"
            && m["test_text_opened"] == false,
        "retained Train scope"
    );
    let label_bytes = fs::read(root.join("labels.jsonl"))?;
    let request_bytes = fs::read(root.join("requests.jsonl"))?;
    ensure!(
        m["labels_sha256"] == sha(&label_bytes) && m["requests_sha256"] == sha(&request_bytes),
        "retained inputs changed"
    );
    let mut texts = BTreeMap::new();
    for r in rows(&request_bytes)? {
        ensure!(
            texts
                .insert(s(&r, "id")?.to_owned(), sha(s(&r, "text")?.as_bytes()))
                .is_none(),
            "duplicate request"
        );
    }
    let labels = rows(&label_bytes)?;
    let mut seen = BTreeSet::new();
    for r in &labels {
        ensure!(
            r["frozen_split"] == "train"
                && r["purpose"] == "query"
                && seen.insert(s(r, "id")?.to_owned())
                && texts
                    .get(s(r, "id")?)
                    .is_some_and(|h| r["text_sha256"] == *h),
            "label/request scope"
        );
    }
    ensure!(
        labels.len() == texts.len() && m["records"] == labels.len(),
        "retained roster count"
    );
    let cm = read(&repo.join("datasets/compression-manifest.json"))?;
    let mut inventory = BTreeMap::<String, Vec<(String, Value)>>::new();
    let mut inputs = Vec::new();
    // Fixed already-published Train inputs only, never another split's prose.
    let mut files = vec![
        "datasets/a-balanced-20261026-v1/train.jsonl",
        "datasets/a-train-augmentation-20261026-v1/records.jsonl",
        "datasets/a-supervised-20261026-v1/train.jsonl",
        "datasets/detector-v6/train.jsonl",
    ];
    let successor = "datasets/a-retained-train-20261026-v1/records.jsonl";
    if cm["files"]
        .as_array()
        .context("compression files")?
        .iter()
        .any(|e| e["decoded_path"] == successor)
    {
        files.push(successor);
    }
    for file in files {
        let b = fs::read(repo.join(file))?;
        let binding = cm["files"]
            .as_array()
            .context("compression manifest")?
            .iter()
            .find(|e| e["decoded_path"] == file)
            .context("unbound published payload")?;
        ensure!(
            binding["decoded_sha256"] == sha(&b),
            "published decoded bytes differ"
        );
        for r in rows(&b)? {
            ensure!(
                r["text_sha256"] == sha(s(&r, "text")?.as_bytes()),
                "published text changed"
            );
            inventory
                .entry(s(&r, "id")?.to_owned())
                .or_default()
                .push((file.into(), r));
        }
        inputs.push(json!({"logical_path":file,"decoded_sha256":sha(&b),"compressed_sha256":binding["compressed_sha256"]}));
    }
    let mut present = Vec::new();
    let mut missing = Vec::new();
    let mut counts = BTreeMap::<String, usize>::new();
    for r in &labels {
        let found = inventory
            .get(s(r, "id")?)
            .and_then(|items| items.iter().find(|(_, p)| matches(r, p)));
        if let Some((file, p)) = found {
            *counts.entry(file.clone()).or_default() += 1;
            present.push(json!({"id":r["id"],"text_sha256":r["text_sha256"],"logical_path":file,"published_row_sha256":sha(&serde_json::to_vec(p)?)}));
        } else {
            missing.push(json!({"id":r["id"],"text_sha256":r["text_sha256"],"source_group":r["source_group"]}));
        }
    }
    let summary = json!({"requested":labels.len(),"published_exact_matches":present.len(),"missing":missing.len(),"by_published_file":counts});
    let report = json!({"schema":"slop_ninja_retained_train_publication_audit_v1","complete":true,"all_retained_train_published":missing.is_empty(),
        "summary":summary,"missing":missing,"present":present,"retained_manifest_sha256":sha(&fs::read(root.join("manifest.json"))?),"published_inputs":inputs,
        "match":"exact ID, text hash, source family, origin, involvement, share including missingness, both role identities/presence, and Train/query scope",
        "original_training_and_publications_unchanged":true});
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)?;
    serde_json::to_writer_pretty(&mut f, &report)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(summary)
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 3, "REPO STAGE NEW_PRIVATE_REPORT");
    println!(
        "{}",
        run(Path::new(&a[0]), Path::new(&a[1]), Path::new(&a[2]))?
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matching_text_cannot_hide_wrong_history_family_or_split() {
        let l = json!({"id":"synthetic","text_sha256":"hash","source_group":"work","targets":{"origin":2,"share":0.8,"ai_involvement":true},"roles":{"human":{"present":true,"identity":"writer"},"ai":{"present":true,"identity":"generator"}}});
        let p = json!({"id":"synthetic","text_sha256":"hash","source_group":"work","origin":"mixed","frozen_split":"train","purpose":"query","share_target":0.8,"human_role_present":true,"human_identity":"writer","ai_role_present":true,"ai_identity":"generator"});
        assert!(matches(&l, &p));
        for (k, v) in [
            ("source_group", json!("other")),
            ("origin", json!("model_only")),
            ("frozen_split", json!("test")),
            ("share_target", json!(0.5)),
            ("share_target", Value::Null),
            ("human_identity", json!("different_writer")),
            ("ai_role_present", json!(false)),
            ("split", json!("test")),
        ] {
            let mut b = p.clone();
            b[k] = v;
            assert!(!matches(&l, &b));
        }
    }
}
