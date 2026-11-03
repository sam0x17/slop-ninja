//! The testnet human pool refilled from the public-candidate corpus export.
//!
//! **Testnet only (`testnet_public_candidate_refill`).** The rows are human
//! texts of the redistributable public-candidate export whose every
//! experiment role is a consumed Test, held or diagnostic role: none was ever
//! a Train, CAL, Dev-select or Dev row, so no released model trained or
//! calibrated on them. Every text is public and can be recognized, so this is
//! a stated pilot limit and the pool is never a mainnet evaluation source.
//!
//! The export's decoded `texts.jsonl`, `records.jsonl` and `splits.jsonl` are
//! read (the operator decodes the zstd tables into ignored storage first).
//! A text is kept when it is `human`, every role it held is allowed, its
//! records agree on one domain and source group, and its token count is at
//! most `max_tokens`: the bound that keeps a mixed rewrite of it inside the
//! scorer's 2,048-token input contract, since a rewrite replaces less than the
//! whole text and the generation check bounds its length at 1.8 times the
//! replaced words. The pool is written in the v1 pool format; the cohort
//! engine's draw and its durable used ledger apply unchanged, with reward-v1's
//! rewrite-source reuse on top.

use crate::{POOL_SCHEMA, POOL_SHARD_SCHEMA, sha256_hex};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, Write};
use std::path::Path;

pub const PURPOSE: &str = "testnet_public_candidate_refill";

/// Whether an experiment role keeps a text eligible.
pub fn role_allowed(role: &str) -> bool {
    role == "test_consumed" || role == "held" || role.starts_with("diagnostic_")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub row_id: String,
    pub text_sha256: String,
    pub domain: String,
    pub source_group: String,
    pub text: String,
}

fn lines(path: &Path) -> Result<impl Iterator<Item = Result<Value>>> {
    let f = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    Ok(std::io::BufReader::new(f).lines().map(|l| Ok(serde_json::from_str(&l?)?)))
}

/// Selects the eligible human texts (sorted by domain, group and row id) and
/// the counts of every exclusion reason.
pub fn select(texts: &Path, records: &Path, splits: &Path, max_tokens: u64) -> Result<(Vec<Candidate>, BTreeMap<String, u64>)> {
    let mut roles: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for v in lines(splits)? {
        let v = v?;
        let sha = v["text_sha256"].as_str().context("split text_sha256")?.to_owned();
        roles.entry(sha).or_default().insert(v["role"].as_str().context("split role")?.to_owned());
    }
    let mut meta: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    for v in lines(records)? {
        let v = v?;
        if v["origin"] != "human_only" {
            continue;
        }
        let sha = v["text_sha256"].as_str().context("record text_sha256")?.to_owned();
        let domain = v["domain"].as_str().unwrap_or_default().to_owned();
        let group = v["source_group"].as_str().unwrap_or_default().to_owned();
        meta.entry(sha).or_default().insert((domain, group));
    }
    let mut out = Vec::new();
    let mut excluded: BTreeMap<String, u64> = BTreeMap::new();
    let mut skip = |why: &str| *excluded.entry(why.to_owned()).or_default() += 1;
    for v in lines(texts)? {
        let v = v?;
        if v["kind"] != "human" {
            continue;
        }
        let sha = v["text_sha256"].as_str().context("text_sha256")?.to_owned();
        let text = v["text"].as_str().context("text")?.to_owned();
        ensure!(sha256_hex(text.as_bytes()) == sha, "a text does not match its digest");
        let Some(rs) = roles.get(&sha) else {
            skip("no experiment role");
            continue;
        };
        if !rs.iter().all(|r| role_allowed(r)) {
            skip("a train, calibration or development role");
            continue;
        }
        let Some(m) = meta.get(&sha).filter(|m| m.len() == 1) else {
            skip("no single human record domain and group");
            continue;
        };
        let Some((domain, group)) = m.iter().next().cloned() else { continue };
        let ok_label = |s: &str| !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b));
        if !ok_label(&domain) || group.is_empty() {
            skip("domain or group label");
            continue;
        }
        if v["n_tokens"].as_u64().is_none_or(|t| t > max_tokens) {
            skip("over the token bound");
            continue;
        }
        out.push(Candidate {
            row_id: format!("pc1-{}", &sha[..24]),
            text_sha256: sha,
            domain,
            source_group: format!("pc1:{group}"),
            text,
        });
    }
    out.sort_by(|a, b| (&a.domain, &a.source_group, &a.row_id).cmp(&(&b.domain, &b.source_group, &b.row_id)));
    let mut ids = BTreeSet::new();
    for c in &out {
        ensure!(ids.insert(c.row_id.clone()), "two rows share a row id prefix");
    }
    Ok((out, excluded))
}

/// Writes the pool (shards and a content-free manifest) to a new directory.
pub fn export(rows: &[Candidate], excluded: &BTreeMap<String, u64>, pool_id: &str, shard_rows: usize, max_tokens: u64, inputs: Value, dir: &Path) -> Result<Value> {
    ensure!((1..=4096).contains(&shard_rows), "shard rows must be 1..=4096");
    ensure!(!rows.is_empty(), "no eligible rows");
    fs::create_dir(dir).with_context(|| format!("create {} (existing pools are never replaced)", dir.display()))?;
    let mut shards = Vec::new();
    let mut listed = Vec::new();
    let mut by_domain: BTreeMap<&str, usize> = BTreeMap::new();
    for (k, chunk) in rows.chunks(shard_rows).enumerate() {
        let file = format!("shard-{k:04}.jsonl");
        let mut body = String::new();
        for r in chunk {
            body.push_str(&serde_json::to_string(&json!({"schema": POOL_SHARD_SCHEMA, "row_id": r.row_id,
                "text_sha256": r.text_sha256, "domain": r.domain, "source_group": r.source_group, "text": r.text}))?);
            body.push('\n');
            listed.push(json!({"row_id": r.row_id, "text_sha256": r.text_sha256, "domain": r.domain,
                "source_group": r.source_group, "shard": k}));
            *by_domain.entry(r.domain.as_str()).or_default() += 1;
        }
        let mut f = fs::OpenOptions::new().write(true).create_new(true).open(dir.join(&file))?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        shards.push(json!({"index": k, "file": file, "rows": chunk.len(), "bytes": body.len(),
            "sha256": sha256_hex(body.as_bytes())}));
    }
    let groups: BTreeSet<&str> = rows.iter().map(|r| r.source_group.as_str()).collect();
    let manifest = json!({
        "schema": POOL_SCHEMA,
        "purpose": PURPOSE,
        "pool_id": pool_id,
        "evidence": "testnet only: public human texts whose every experiment role was consumed Test, held or diagnostic; every text can be recognized; never a mainnet evaluation source",
        "reuse": "a passage is scored as a pure-human negative at most once; reward-v1 cohorts may reuse it as a rewrite source under their plan's reuse rule",
        "selection": {"roles_allowed": ["test_consumed", "held", "diagnostic_*"], "max_tokens": max_tokens,
            "max_tokens_basis": "a mixed rewrite stays inside the scorer's 2,048-token contract", "excluded": excluded},
        "inputs": inputs,
        "shard_rows": shard_rows,
        "shards": shards,
        "counts": {"rows": rows.len(), "source_groups": groups.len(), "by_domain": by_domain},
        "rows": listed,
    });
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(dir.join("manifest.json"))?;
    f.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_texts_never_trained_or_calibrated_on_are_kept() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("pc-pool-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)?;
        let t = |s: &str| sha256_hex(s.as_bytes());
        let texts = ["Kept one. Second sentence.", "Trained on. Yes.", "Too long text here.", "Held text. Two."];
        let mut tj = String::new();
        let mut rj = String::new();
        let mut sj = String::new();
        for (i, x) in texts.iter().enumerate() {
            let tokens = if i == 2 { 5000 } else { 10 };
            tj.push_str(&format!("{}\n", json!({"kind": "human", "text": x, "text_sha256": t(x), "n_tokens": tokens})));
            rj.push_str(&format!("{}\n", json!({"origin": "human_only", "text_sha256": t(x), "domain": "essays", "source_group": format!("g{i}")})));
            let role = match i {
                1 => "train",
                3 => "held",
                _ => "test_consumed",
            };
            sj.push_str(&format!("{}\n", json!({"text_sha256": t(x), "role": role})));
            if i == 3 {
                sj.push_str(&format!("{}\n", json!({"text_sha256": t(x), "role": "diagnostic_x"})));
            }
        }
        fs::write(dir.join("t.jsonl"), tj)?;
        fs::write(dir.join("r.jsonl"), rj)?;
        fs::write(dir.join("s.jsonl"), sj)?;
        let (rows, excluded) = select(&dir.join("t.jsonl"), &dir.join("r.jsonl"), &dir.join("s.jsonl"), 1000)?;
        assert_eq!(rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), [texts[0], texts[3]]);
        assert_eq!(excluded["a train, calibration or development role"], 1);
        assert_eq!(excluded["over the token bound"], 1);
        let m = export(&rows, &excluded, "p", 1, 1000, json!({}), &dir.join("pool"))?;
        assert_eq!(m["counts"]["rows"], 2);
        assert!(export(&rows, &excluded, "p", 1, 1000, json!({}), &dir.join("pool")).is_err(), "never replaced");
        fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
