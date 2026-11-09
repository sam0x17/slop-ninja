//! A token-bounded manifest for an existing testnet human pool
//! (`pool-token-bound`).
//!
//! The eval Worker's seeded task order draws only from a pool whose manifest
//! records a per-row token bound inside the scorer's 2,048-token contract
//! (`selection.max_tokens`, the coordinator's `v3::pool_token_bound_ok`).
//! Pool v1 (the Dev-iter humans, `pool` above) records none. This command
//! reads such a pool's manifest and shards, checks every shard and row by
//! digest, counts each row's tokens with the scorer's own compiled-in count
//! (`slop_ninja_scorer_tokens`, special tokens excluded) and writes a new
//! manifest for the same pool id and the same shard files: the rows within
//! `--max-tokens`, the bound and its basis in `selection`, the counts
//! recomputed, and the digest of the manifest it was derived from. Nothing
//! else changes, so the shards already stored beside the original manifest
//! serve the new one, and the new manifest holds no text.
//!
//! The bound is the public-candidate export's (1,000 tokens): a mixed
//! rewrite of the row stays inside the scorer's contract.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::Path;

pub const POOL_SCHEMA: &str = "slop_ninja_testnet_human_pool_v1";
pub const SHARD_SCHEMA: &str = "slop_ninja_testnet_human_pool_row_v1";
pub const RULE: &str = "scorer_tokens_excluding_specials_v1";
pub const BASIS: &str = "a mixed rewrite stays inside the scorer's 2,048-token contract";
/// The largest manifest or shard file read.
const MAX_FILE_BYTES: u64 = 64 << 20;

fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn read_bounded(p: &Path) -> Result<Vec<u8>> {
    let len = fs::metadata(p).with_context(|| format!("read {}", p.display()))?.len();
    ensure!(len <= MAX_FILE_BYTES, "{} is larger than {MAX_FILE_BYTES} bytes", p.display());
    fs::read(p).with_context(|| format!("read {}", p.display()))
}

/// A row's tokens under the scorer's count, the frame (`[CLS]`, `[SEP]`)
/// excluded.
pub fn row_tokens(text: &str) -> Result<u64> {
    let mut admit = |_: usize, _: usize| true;
    let framed = slop_ninja_scorer_tokens::count(text, &mut admit).map_err(|e| anyhow::anyhow!("token count: {e:?}"))?;
    let frame = slop_ninja_scorer_tokens::frame_tokens().map_err(|e| anyhow::anyhow!("token frame: {e:?}"))?;
    Ok(framed.saturating_sub(frame))
}

/// The token-bounded manifest of the pool in `pool_dir` (its
/// `manifest.json` and shards), keeping the rows of at most `max_tokens`
/// tokens. Refuses a manifest that is not a human pool, already records a
/// bound, or lists a shard or row whose digest, schema or listing differs.
pub fn bound_manifest(pool_dir: &Path, max_tokens: u64) -> Result<Value> {
    ensure!(
        (1..=2046).contains(&max_tokens),
        "--max-tokens must leave room for the frame inside the scorer's 2,048 tokens"
    );
    let raw = read_bounded(&pool_dir.join("manifest.json"))?;
    let mut m: Value = serde_json::from_slice(&raw).context("manifest json")?;
    ensure!(m["schema"] == POOL_SCHEMA, "not a {POOL_SCHEMA} manifest");
    ensure!(m["pool_id"].as_str().is_some_and(|p| !p.is_empty()), "pool_id");
    ensure!(m.get("selection").is_none_or(|s| s.get("max_tokens").is_none()), "the manifest already records a token bound");
    let rows = m["rows"].as_array().context("rows")?.clone();
    let shards = m["shards"].as_array().context("shards")?.clone();
    // Every shard by digest, and every row in it by schema and text digest.
    let mut texts: BTreeMap<String, (String, String)> = BTreeMap::new();
    for sh in &shards {
        let file = sh["file"].as_str().context("shard file")?;
        ensure!(
            !file.is_empty() && !file.contains('/') && !file.contains(".."),
            "shard file name {file:?}"
        );
        let body = read_bounded(&pool_dir.join(file))?;
        ensure!(sha256_hex(&body) == sh["sha256"].as_str().context("shard sha256")?, "shard {file} digest");
        let index = sh["index"].as_u64().context("shard index")?;
        for line in std::str::from_utf8(&body).context("shard utf-8")?.lines() {
            let r: Value = serde_json::from_str(line).context("shard row json")?;
            ensure!(r["schema"] == SHARD_SCHEMA, "shard {file}: row schema");
            let id = r["row_id"].as_str().context("row_id")?.to_owned();
            let text = r["text"].as_str().context("text")?;
            let sha = r["text_sha256"].as_str().context("text_sha256")?;
            ensure!(sha256_hex(text.as_bytes()) == sha, "row {id}: text digest");
            ensure!(
                texts.insert(id.clone(), (format!("{index}:{sha}"), text.to_owned())).is_none(),
                "row {id} appears twice"
            );
        }
    }
    let mut kept = Vec::new();
    let mut over = 0u64;
    let mut longest = 0u64;
    for r in &rows {
        let id = r["row_id"].as_str().context("manifest row_id")?;
        let (listed, text) = texts.get(id).with_context(|| format!("row {id}: not in its shard"))?;
        let want = format!("{}:{}", r["shard"].as_u64().context("row shard")?, r["text_sha256"].as_str().context("row text_sha256")?);
        ensure!(*listed == want, "row {id}: shard or digest differs from the manifest");
        let n = row_tokens(text)?;
        if n > max_tokens {
            over += 1;
            continue;
        }
        longest = longest.max(n);
        kept.push(r.clone());
    }
    if kept.is_empty() {
        bail!("no row within {max_tokens} tokens");
    }
    let mut by_domain: BTreeMap<String, u64> = BTreeMap::new();
    let mut groups = BTreeSet::new();
    for r in &kept {
        *by_domain.entry(r["domain"].as_str().context("domain")?.to_owned()).or_default() += 1;
        groups.insert(r["source_group"].as_str().context("source_group")?.to_owned());
    }
    let obj = m.as_object_mut().context("manifest object")?;
    obj.insert("rows".into(), Value::Array(kept.clone()));
    obj.insert("counts".into(), json!({"rows": kept.len(), "source_groups": groups.len(), "by_domain": by_domain}));
    obj.insert(
        "selection".into(),
        json!({"max_tokens": max_tokens, "max_tokens_basis": BASIS, "token_rule": RULE,
            "tokenizer_table": slop_ninja_scorer_tokens::TABLE_ID, "longest_kept_tokens": longest,
            "excluded": {"over the token bound": over}}),
    );
    obj.insert("token_bound_of".into(), json!({"manifest_sha256": sha256_hex(&raw), "rows": rows.len()}));
    obj.insert(
        "reuse".into(),
        json!("exposed testnet stock: drawn again under the plan's reuse rule, never a mainnet evaluation source"),
    );
    Ok(m)
}

/// Writes `m` to `out`, which must not exist (a manifest is never replaced).
pub fn write_new(out: &Path, m: &Value) -> Result<String> {
    let body = serde_json::to_vec_pretty(m)?;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)
        .with_context(|| format!("create {} (an existing manifest is never replaced)", out.display()))?;
    f.write_all(&body)?;
    f.sync_all()?;
    Ok(sha256_hex(&body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A scratch pool directory removed on drop.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A pool of `texts` (row `r<i>`, one group each except rows 0 and 1,
    /// which share one), in shards of two rows.
    fn pool(tag: &str, texts: &[String]) -> Scratch {
        let dir = std::env::temp_dir().join(format!("a-testnet-token-bound-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut rows = Vec::new();
        let mut shards = Vec::new();
        for (k, chunk) in texts.chunks(2).enumerate() {
            let mut body = String::new();
            for (j, t) in chunk.iter().enumerate() {
                let i = k * 2 + j;
                let group = if i < 2 { "g0".to_owned() } else { format!("g{i}") };
                let domain = if i % 2 == 0 { "wikipedia" } else { "plos" };
                body.push_str(&json!({"schema": SHARD_SCHEMA, "row_id": format!("r{i}"), "text_sha256": sha256_hex(t.as_bytes()),
                    "domain": domain, "source_group": group, "text": t}).to_string());
                body.push('\n');
                rows.push(json!({"row_id": format!("r{i}"), "text_sha256": sha256_hex(t.as_bytes()), "domain": domain,
                    "source_group": group, "shard": k}));
            }
            let file = format!("shard-{k:04}.jsonl");
            fs::write(dir.join(&file), &body).unwrap();
            shards.push(json!({"index": k, "file": file, "rows": chunk.len(), "bytes": body.len(), "sha256": sha256_hex(body.as_bytes())}));
        }
        let m = json!({"schema": POOL_SCHEMA, "pool_id": "testnet-human-pool-x", "purpose": "testnet_plumbing_only",
            "reuse": "never", "rows": rows, "shards": shards, "counts": {"rows": texts.len()}});
        fs::write(dir.join("manifest.json"), serde_json::to_vec_pretty(&m).unwrap()).unwrap();
        Scratch(dir)
    }

    fn texts() -> Vec<String> {
        vec![
            "A short human note about the harbour.".to_owned(),
            "Another short note, written by hand.".to_owned(),
            "word ".repeat(1500),
            "The council met on Tuesday and agreed the budget.".to_owned(),
        ]
    }

    #[test]
    fn rows_over_the_bound_leave_and_the_rest_keep_their_shards() {
        let p = pool("keep", &texts());
        let raw = fs::read(p.0.join("manifest.json")).unwrap();
        let m = bound_manifest(&p.0, 1000).unwrap();
        let ids: Vec<&str> = m["rows"].as_array().unwrap().iter().map(|r| r["row_id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["r0", "r1", "r3"]);
        assert_eq!(m["counts"]["rows"], 3);
        assert_eq!(m["counts"]["source_groups"], 2);
        assert_eq!(m["counts"]["by_domain"], json!({"plos": 2, "wikipedia": 1}));
        assert_eq!(m["selection"]["max_tokens"], 1000);
        assert_eq!(m["selection"]["excluded"]["over the token bound"], 1);
        assert_eq!(m["selection"]["tokenizer_table"], slop_ninja_scorer_tokens::TABLE_ID);
        assert!(m["selection"]["longest_kept_tokens"].as_u64().unwrap() <= 1000);
        assert_eq!(m["token_bound_of"]["manifest_sha256"], sha256_hex(&raw));
        assert_eq!(m["pool_id"], "testnet-human-pool-x");
        // The shard list is the original's: the stored shards serve it.
        let orig: Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(m["shards"], orig["shards"]);
        // A count over the whole text, frame excluded.
        assert_eq!(row_tokens("").unwrap(), 0);
        assert!(row_tokens(&"word ".repeat(1500)).unwrap() > 1000);
        // Written once; never replaced.
        let out = p.0.join("manifest-tb1000.json");
        write_new(&out, &m).unwrap();
        assert!(write_new(&out, &m).is_err());
    }

    #[test]
    fn a_changed_shard_row_or_listing_is_refused() {
        let p = pool("refuse", &texts());
        // A manifest that already records a bound.
        let raw = fs::read(p.0.join("manifest.json")).unwrap();
        let mut m: Value = serde_json::from_slice(&raw).unwrap();
        m["selection"] = json!({"max_tokens": 1000});
        fs::write(p.0.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(bound_manifest(&p.0, 1000).unwrap_err().to_string().contains("already records"));
        // A listing whose text digest differs from its shard row.
        let mut m: Value = serde_json::from_slice(&raw).unwrap();
        m["rows"][0]["text_sha256"] = json!(sha256_hex(b"other"));
        fs::write(p.0.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(bound_manifest(&p.0, 1000).unwrap_err().to_string().contains("differs"));
        // A shard edited after the manifest.
        fs::write(p.0.join("manifest.json"), &raw).unwrap();
        let shard = p.0.join("shard-0000.jsonl");
        let body = fs::read_to_string(&shard).unwrap();
        fs::write(&shard, body.replace("harbour", "harbor")).unwrap();
        assert!(bound_manifest(&p.0, 1000).unwrap_err().to_string().contains("digest"));
        // Another schema, and a bound with no room for the frame.
        fs::write(&shard, &body).unwrap();
        let mut m: Value = serde_json::from_slice(&raw).unwrap();
        m["schema"] = json!("other");
        fs::write(p.0.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(bound_manifest(&p.0, 1000).is_err());
        fs::write(p.0.join("manifest.json"), &raw).unwrap();
        assert!(bound_manifest(&p.0, 2047).is_err());
        assert!(bound_manifest(&p.0, 1000).is_ok());
    }
}
