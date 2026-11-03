//! Token counts per text and tokenizer (special tokens excluded), resumable.

use crate::archive::{BlobCache, hash_file};
use anyhow::{Result, anyhow};
use rayon::prelude::*;
use rusqlite::{Connection, params};
use std::path::Path;

pub fn run(db: &Connection, store: &Path, tokenizer: &Path) -> Result<String> {
    let (tsha, _) = hash_file(tokenizer)?;
    let tok = tokenizers::Tokenizer::from_file(tokenizer).map_err(|e| anyhow!(e.to_string()))?;
    let mut cache = BlobCache::new(&store.join("packs"));
    let mut total = 0u64;
    let mut last = String::new();
    loop {
        // Keyset scan in digest order, so each batch starts where the last ended.
        let batch: Vec<String> = {
            let mut st = db.prepare("SELECT text_sha256 FROM text WHERE text_sha256 > ?1 ORDER BY text_sha256 LIMIT 20000")?;
            st.query_map([&last], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        let Some(end) = batch.last().cloned() else { break };
        last = end;
        let mut shas = Vec::with_capacity(batch.len());
        for h in batch {
            let done: bool = db
                .prepare_cached("SELECT 1 FROM text_tokens WHERE text_sha256 = ?1 AND tokenizer_sha256 = ?2")?
                .exists(rusqlite::params![h, tsha])?;
            if !done {
                shas.push(h);
            }
        }
        if shas.is_empty() {
            continue;
        }
        let mut texts = Vec::with_capacity(shas.len());
        for h in &shas {
            let mut b = [0u8; 32];
            hex::decode_to_slice(h, &mut b)?;
            texts.push(cache.text(db, &b)?);
        }
        let counts: Vec<usize> = texts
            .par_iter()
            .map(|t| tok.encode(t.as_str(), false).map(|e| e.get_ids().len()).unwrap_or(usize::MAX))
            .collect();
        db.execute_batch("BEGIN")?;
        for (h, n) in shas.iter().zip(counts) {
            let n = if n == usize::MAX { -1 } else { n as i64 };
            db.prepare_cached("INSERT OR REPLACE INTO text_tokens(text_sha256, tokenizer_sha256, n_tokens) VALUES (?1, ?2, ?3)")?
                .execute(params![h, tsha, n])?;
        }
        db.execute_batch("COMMIT")?;
        total += shas.len() as u64;
        eprintln!("tokens: {total}");
    }
    db.execute("INSERT OR REPLACE INTO meta(key, value) VALUES ('tokenizer_sha256', ?1)", [&tsha])?;
    Ok(serde_json::json!({"tokenizer_sha256": tsha, "counted": total}).to_string())
}
