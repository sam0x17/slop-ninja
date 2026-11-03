//! Reverse overlap check: stream JSONL rows of other pools (for example
//! concurrent candidate files too large to add to a prior index) against a
//! protected index built from this pool's candidates, and report every row
//! whose text or family key overlaps. Rows are read, hashed and shingled
//! only; no text is written to the output.
use crate::{
    io::{self, JsonlWriter},
    protected::Index,
};
use anyhow::Result;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = "slop_ninja_a_pool_overlap_scan_v1";

pub fn scan(index_dir: &Path, inputs: &[PathBuf], out: &Path) -> Result<Value> {
    io::fresh_dir(out)?;
    let index = Index::load(index_dir)?;
    let mut hits = JsonlWriter::create(&out.join("hits.jsonl"))?;
    let mut files = Vec::new();
    for p in inputs {
        let (sha, bytes) = io::file_sha(p)?;
        let (mut rows, mut with_text, mut bad, mut n_hits) = (0u64, 0u64, 0u64, 0u64);
        for row in io::jsonl_rows(p)? {
            let Ok(row) = row else {
                bad += 1;
                continue;
            };
            rows += 1;
            let mut reasons = index.family_conflicts(&row);
            if let Some(t) = row["text"].as_str() {
                with_text += 1;
                reasons.extend(index.text_conflicts(t));
            }
            if !reasons.is_empty() {
                n_hits += 1;
                hits.write(&json!({"input": p, "id": row["id"], "collection": row["collection"], "source_group": row["source_group"],
                                   "text_sha256": row["text_sha256"], "reasons": reasons}))?;
            }
        }
        files.push(json!({"path": p, "sha256_at_scan": sha, "bytes_at_scan": bytes, "rows": rows, "rows_with_text": with_text,
                          "unparsable_lines": bad, "hits": n_hits}));
    }
    let (hsha, hrows) = hits.finish()?;
    let report = json!({"schema": SCHEMA, "finished_utc": io::now_utc(), "index_dir": index_dir, "index_sha256": index.sha256,
                        "inputs": files, "hits": hrows, "hits_sha256": hsha,
                        "rule": "a row overlaps when it shares an exact, whitespace-normalised or folded text hash, a 50-word sequence, or a family key with the indexed pool",
                        "limits": ["files still being written are scanned as they stood at scan time (sha256_at_scan); later rows are not covered",
                                   "unparsable (partially written) lines are counted, not scanned"]});
    io::write_json(&out.join("report.json"), &report)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn prose(seed: &str, n: usize) -> String {
        (0..n).map(|i| format!("{seed}{i}")).collect::<Vec<_>>().join(" ")
    }
    #[test]
    fn reports_rows_sharing_text_with_the_index() {
        let d = tempfile::tempdir().unwrap();
        let shared = prose("w", 60);
        std::fs::create_dir_all(d.path().join("snap")).unwrap();
        std::fs::write(d.path().join("snap/a.jsonl"), format!("{}\n", json!({"text": shared}))).unwrap();
        std::fs::write(d.path().join("reg.json"), json!({"schema": "slop_ninja_a_pool_prior_registry_v1", "entries": [{"label": "s", "path": "snap", "names": ["*.jsonl"]}]}).to_string()).unwrap();
        crate::protected::build(&d.path().join("reg.json"), d.path(), &d.path().join("ix")).unwrap();
        let other = d.path().join("other.jsonl");
        std::fs::write(&other, format!("{}\n{}\n{{\"trunc", json!({"id": "x1", "text": shared}), json!({"id": "x2", "text": prose("z", 60)}))).unwrap();
        let r = scan(&d.path().join("ix"), &[other], &d.path().join("out")).unwrap();
        assert_eq!(r["hits"], 1);
        assert_eq!(r["inputs"][0]["rows"], 2);
        assert_eq!(r["inputs"][0]["unparsable_lines"], 1);
        let h: Vec<Value> = io::jsonl_rows(&d.path().join("out/hits.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(h[0]["id"], "x1");
        assert!(h[0].get("text").is_none());
    }
}
