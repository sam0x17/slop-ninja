//! Verify every mapped tree and compressed blob without interpreting corpus text.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    process::Command,
};

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    mode: String,
    kind: String,
    oid: String,
}

fn git(repo: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(repo)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "git metadata read failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

fn tree(repo: &str, oid: &str) -> Result<BTreeMap<String, Entry>> {
    let mut entries = BTreeMap::new();
    let bytes = git(repo, &["ls-tree", "-z", oid])?;
    for record in bytes.split(|c| *c == 0).filter(|r| !r.is_empty()) {
        let (metadata, name) = std::str::from_utf8(record)?
            .split_once('\t')
            .context("tree record")?;
        let parts: Vec<_> = metadata.split_whitespace().collect();
        ensure!(parts.len() == 3, "tree metadata");
        entries.insert(
            name.into(),
            Entry {
                mode: parts[0].into(),
                kind: parts[1].into(),
                oid: parts[2].into(),
            },
        );
    }
    Ok(entries)
}

fn eligible(path: &str) -> bool {
    path.starts_with("datasets/")
        && (path.ends_with(".jsonl")
            || path.ends_with("/data_rights.json")
            || path.ends_with("/evidence-index.json"))
}

struct Audit<'a> {
    old: &'a str,
    new: &'a str,
    blobs: HashMap<String, String>,
    seen: HashSet<(String, String, String)>,
    payload_entries: usize,
}

impl Audit<'_> {
    fn trees(&mut self, old: &str, new: &str, prefix: &str) -> Result<()> {
        if !self.seen.insert((old.into(), new.into(), prefix.into())) {
            return Ok(());
        }
        let before = tree(self.old, old)?;
        let mut after = tree(self.new, new)?;
        for (name, prior) in before {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let packed = eligible(&path);
            let key = if packed { format!("{name}.zst") } else { name };
            let next = after
                .remove(&key)
                .with_context(|| format!("missing {path} at {old}"))?;
            ensure!(
                prior.mode == next.mode && prior.kind == next.kind,
                "mode/type changed: {path}"
            );
            if prior.kind == "tree" && (path == "datasets" || path.starts_with("datasets/")) {
                self.trees(&prior.oid, &next.oid, &path)?;
            } else if packed {
                ensure!(
                    self.blobs.get(&prior.oid) == Some(&next.oid),
                    "payload mapping differs: {path}"
                );
                self.payload_entries += 1;
            } else {
                ensure!(
                    prior.oid == next.oid,
                    "non-payload changed: {path} at {old}"
                );
            }
        }
        ensure!(
            after.is_empty(),
            "unexpected new tree entries at {prefix}: {:?}",
            after.keys()
        );
        Ok(())
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 5,
        "usage: history_audit OLD_GIT NEW_GIT COMMIT_MAP BLOB_MAP_JSONL OUTPUT"
    );
    let mut mappings = HashMap::new();
    let mut decoded_bytes = 0usize;
    let mut compressed_bytes = 0usize;
    for line in fs::read_to_string(&args[3])?.lines() {
        let row: Value = serde_json::from_str(line)?;
        let old = row["old_blob"].as_str().context("old blob")?;
        let new = row["new_blob"].as_str().context("new blob")?;
        let raw = git(&args[0], &["cat-file", "blob", old])?;
        let packed = git(&args[1], &["cat-file", "blob", new])?;
        let decoded = zstd::stream::decode_all(packed.as_slice())?;
        ensure!(raw == decoded, "historical roundtrip differs: {old}");
        ensure!(
            row["decoded_sha256"] == format!("{:x}", Sha256::digest(&raw)),
            "decoded SHA mismatch"
        );
        ensure!(
            row["compressed_sha256"] == format!("{:x}", Sha256::digest(&packed)),
            "compressed SHA mismatch"
        );
        ensure!(
            row["decoded_bytes"] == raw.len() && row["compressed_bytes"] == packed.len(),
            "size mismatch"
        );
        ensure!(
            mappings.insert(old.into(), new.into()).is_none(),
            "duplicate mapping"
        );
        decoded_bytes += raw.len();
        compressed_bytes += packed.len();
    }
    let blobs = mappings.len();
    let mut audit = Audit {
        old: &args[0],
        new: &args[1],
        blobs: mappings,
        seen: HashSet::new(),
        payload_entries: 0,
    };
    let mut commits = 0;
    for line in fs::read_to_string(&args[2])?.lines().skip(1) {
        let cols: Vec<_> = line.split_whitespace().collect();
        ensure!(
            cols.len() == 2 && cols[1] != "0000000000000000000000000000000000000000",
            "lost commit"
        );
        audit.trees(cols[0], cols[1], "")?;
        commits += 1;
    }
    let report = json!({"schema":"slop_ninja_dataset_history_audit_v1", "commits":commits,
        "distinct_tree_pairs":audit.seen.len(), "distinct_payload_blobs":blobs,
        "payload_tree_entries":audit.payload_entries,"decoded_bytes":decoded_bytes,
        "compressed_bytes":compressed_bytes,"every_transformed_blob_restores_exactly":true,
        "all_non_payload_tree_entries_identical":true});
    fs::write(
        &args[4],
        format!("{}\n", serde_json::to_string_pretty(&report)?),
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
