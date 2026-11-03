//! Export the store as plain, checksummed zstd JSONL tables.
//!
//! `all` writes every table, with exact texts. `redistributable` writes only
//! texts that already appear in a published snapshot (records whose source
//! root lies under `<repo_root>/datasets/`, an anchored prefix test rather than a
//! substring match), their records, views and split membership; it omits
//! detector observations, production events and every other text, which stay
//! private until separately cleared.

use crate::archive::BlobCache;
use anyhow::{Context, Result, bail, ensure};
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

pub const EXPORT_SCHEMA: &str = "slop_ninja_corpus_export_v1";

pub(crate) struct Out {
    w: zstd::stream::write::Encoder<'static, BufWriter<File>>,
    h: Sha256,
    pub(crate) rows: u64,
    raw_bytes: u64,
}

impl Out {
    pub(crate) fn new(path: &Path) -> Result<Out> {
        Ok(Out { w: zstd::stream::write::Encoder::new(BufWriter::new(File::create(path)?), 6)?, h: Sha256::new(), rows: 0, raw_bytes: 0 })
    }
    pub(crate) fn line(&mut self, v: &Value) -> Result<()> {
        let mut b = serde_json::to_vec(v)?;
        b.push(b'\n');
        self.h.update(&b);
        self.raw_bytes += b.len() as u64;
        self.w.write_all(&b)?;
        self.rows += 1;
        Ok(())
    }
    pub(crate) fn finish(self) -> Result<Value> {
        let mut inner = self.w.finish()?;
        inner.flush()?;
        Ok(json!({"rows": self.rows, "decoded_bytes": self.raw_bytes, "decoded_sha256": hex::encode(self.h.finalize())}))
    }
}

fn row_json(r: &rusqlite::Row, names: &[String]) -> rusqlite::Result<Value> {
    let mut m = Map::new();
    for (k, n) in names.iter().enumerate() {
        let v = match r.get_ref(k)? {
            rusqlite::types::ValueRef::Null => Value::Null,
            rusqlite::types::ValueRef::Integer(i) => json!(i),
            rusqlite::types::ValueRef::Real(f) => json!(f),
            rusqlite::types::ValueRef::Text(t) => {
                let s = String::from_utf8_lossy(t).to_string();
                if n.ends_with("_json") || n == "ai_byte_spans" {
                    serde_json::from_str(&s).unwrap_or(Value::String(s))
                } else {
                    Value::String(s)
                }
            }
            rusqlite::types::ValueRef::Blob(b) => json!(hex::encode(b)),
        };
        m.insert(n.clone(), v);
    }
    Ok(Value::Object(m))
}

fn table(db: &Connection, dir: &Path, name: &str, sql: &str, files: &mut Map<String, Value>) -> Result<()> {
    let path = dir.join(format!("{name}.jsonl.zst"));
    let mut out = Out::new(&path)?;
    let mut st = db.prepare(sql)?;
    let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
    let mut q = st.query([])?;
    while let Some(r) = q.next()? {
        out.line(&row_json(r, &names)?)?;
    }
    let mut meta = out.finish()?;
    meta["sha256"] = json!(crate::archive::hash_file_raw(&path)?);
    eprintln!("export {name}: {}", meta["rows"]);
    files.insert(format!("{name}.jsonl.zst"), meta);
    Ok(())
}

/// The nearest ancestor of the store that holds `.git` (the repository root).
pub(crate) fn repo_root(store: &Path) -> Option<std::path::PathBuf> {
    let mut d = fs::canonicalize(store).ok()?;
    loop {
        if d.join(".git").exists() {
            return Some(d);
        }
        if !d.pop() {
            return None;
        }
    }
}

/// SQL condition on `source.root` (aliased `col`) that holds only for roots under
/// `<repo_root>/datasets/`: an anchored prefix of the canonical repository path, never
/// a substring test, so a private root that merely contains `/datasets/` is refused.
fn published_root_sql(store: &Path, col: &str) -> Result<String> {
    let root = repo_root(store).context("no repository root (.git) above the store")?;
    let prefix = format!("{}/datasets/", root.display()).replace('\'', "''");
    Ok(format!("substr({col}, 1, {}) = '{prefix}'", prefix.chars().count()))
}

pub fn run(db: &Connection, store: &Path, out_dir: &Path, subset: &str) -> Result<Value> {
    ensure!(!out_dir.exists(), "{} exists", out_dir.display());
    let published = if subset == "redistributable" { published_root_sql(store, "root")? } else { String::new() };
    let published_s = published.replace("substr(root,", "substr(s.root,");
    // The published set goes through the per-source index (record.source_id).
    let redistributable = format!("SELECT DISTINCT text_sha256 FROM record INDEXED BY record_source
             WHERE source_id IN (SELECT source_id FROM source WHERE {published}) AND text_sha256 IS NOT NULL");
    let selection = match subset {
        "all" => "SELECT text_sha256 FROM text",
        "redistributable" => redistributable.as_str(),
        "public-candidate-v1" => bail!("subset public-candidate-v1 needs --rules (see public_candidate.rs)"),
        x => bail!("unknown subset {x} (all | redistributable | public-candidate-v1)"),
    };
    fs::create_dir_all(out_dir)?;
    let tmp = out_dir.join(".texts");
    db.execute_batch(&format!(
        "DROP TABLE IF EXISTS temp.export_texts; CREATE TEMP TABLE export_texts (text_sha256 TEXT PRIMARY KEY) WITHOUT ROWID;
         INSERT OR IGNORE INTO export_texts {selection};"))?;
    let mut files = Map::new();
    // Texts with exact bytes.
    {
        let path = out_dir.join("texts.jsonl.zst");
        let mut out = Out::new(&path)?;
        let mut cache = BlobCache::new(&store.join("packs"));
        let tok: Option<String> = db.query_row("SELECT value FROM meta WHERE key = 'tokenizer_sha256'", [], |r| r.get(0)).ok();
        let mut st = db.prepare(
            "SELECT t.text_sha256, t.n_bytes, t.n_chars, t.n_words, t.n_lines, t.language, t.language_score, t.first_source,
                    (SELECT k.n_tokens FROM text_tokens k WHERE k.text_sha256 = t.text_sha256 AND k.tokenizer_sha256 = ?1),
                    s.canonical_split, s.evaluation_only, s.group_evaluation_only
             FROM text t JOIN export_texts e ON e.text_sha256 = t.text_sha256
             LEFT JOIN text_split s ON s.text_sha256 = t.text_sha256 ORDER BY t.text_sha256")?;
        let mut q = st.query([tok.clone().unwrap_or_default()])?;
        while let Some(r) = q.next()? {
            let h: String = r.get(0)?;
            let mut b = [0u8; 32];
            hex::decode_to_slice(&h, &mut b)?;
            let text = cache.text(db, &b)?;
            out.line(&json!({"text_sha256": h, "text": text, "n_bytes": r.get::<_, i64>(1)?, "n_chars": r.get::<_, i64>(2)?,
                "n_words": r.get::<_, i64>(3)?, "n_lines": r.get::<_, i64>(4)?, "language": r.get::<_, String>(5)?,
                "language_score": r.get::<_, f64>(6)?, "first_source": r.get::<_, String>(7)?, "n_tokens": r.get::<_, Option<i64>>(8)?,
                "canonical_split": r.get::<_, Option<String>>(9)?, "evaluation_only": r.get::<_, Option<i64>>(10)?.map(|x| x == 1),
                "group_evaluation_only": r.get::<_, Option<i64>>(11)?.map(|x| x == 1)}))?;
        }
        let mut meta = out.finish()?;
        meta["sha256"] = json!(crate::archive::hash_file_raw(&path)?);
        meta["tokenizer_sha256"] = json!(tok);
        eprintln!("export texts: {}", meta["rows"]);
        files.insert("texts.jsonl.zst".into(), meta);
    }
    let _ = fs::remove_dir_all(&tmp);
    let rec_cols = "record_uid, file_sha256, line_no, role, line_sha256, source_id, adapter, record_id, text_sha256, dataset, collection,
        domain, genre, source_work, family, source_group, writer_key, original_doc_id, doc_date, composition_date, licence, rights_basis,
        cohort_id, origin, share, ai_byte_spans, generator_label, generator_key, operation, recipe_sha256, parent_record_id,
        parent_text_sha256, admission, split_hint, extra_json";
    if subset == "all" {
        table(db, out_dir, "records", &format!("SELECT {rec_cols} FROM record ORDER BY record_uid"), &mut files)?;
        table(db, out_dir, "events", "SELECT * FROM event ORDER BY event_uid", &mut files)?;
        table(db, out_dir, "observations", "SELECT * FROM observation ORDER BY obs_uid", &mut files)?;
        table(db, out_dir, "views", "SELECT * FROM view ORDER BY view_id", &mut files)?;
        table(db, out_dir, "view_members", "SELECT * FROM view_member ORDER BY view_id, ordinal", &mut files)?;
        table(db, out_dir, "split_members", "SELECT * FROM split_member ORDER BY experiment, record_id, view_id", &mut files)?;
        table(db, out_dir, "exposure", "SELECT * FROM exposure ORDER BY exposure_key", &mut files)?;
        table(db, out_dir, "sources", "SELECT * FROM source ORDER BY priority, source_id", &mut files)?;
        table(db, out_dir, "source_inputs", "SELECT * FROM source_input ORDER BY source_id, rel_path", &mut files)?;
        table(db, out_dir, "files", "SELECT * FROM file ORDER BY file_sha256", &mut files)?;
        table(db, out_dir, "file_paths", "SELECT * FROM file_path ORDER BY path, file_sha256", &mut files)?;
        table(db, out_dir, "problems", "SELECT * FROM problem ORDER BY problem_uid", &mut files)?;
        table(db, out_dir, "text_split_roles", "SELECT * FROM text_split_role ORDER BY text_sha256, role, basis", &mut files)?;
    } else {
        table(db, out_dir, "records", &format!(
            "SELECT {rec_cols} FROM record INDEXED BY record_source WHERE source_id IN (SELECT source_id FROM source WHERE {published}) ORDER BY record_uid"), &mut files)?;
        // Paths relative to the repository root, so no host path is published.
        let root = repo_root(store).map(|p| format!("{}/", p.display())).unwrap_or_default().replace('\'', "''");
        table(db, out_dir, "views", &format!(
            "SELECT v.view_id, v.kind, CASE WHEN '{root}' != '' AND substr(v.path, 1, length('{root}')) = '{root}'
                    THEN substr(v.path, length('{root}') + 1) ELSE v.path END AS path,
                    v.file_sha256, v.n_rows, v.expected_sha256, v.expected_basis, v.source_id
             FROM view v JOIN source s ON s.source_id = v.source_id WHERE {published_s} ORDER BY view_id"), &mut files)?;
        table(db, out_dir, "view_members", &format!("SELECT m.* FROM view_member m JOIN view v ON v.view_id = m.view_id JOIN source s ON s.source_id = v.source_id
            WHERE {published_s} ORDER BY m.view_id, m.ordinal"), &mut files)?;
        table(db, out_dir, "split_members", &format!("SELECT m.* FROM split_member m JOIN view v ON v.view_id = m.view_id JOIN source s ON s.source_id = v.source_id
            WHERE {published_s} ORDER BY m.experiment, m.record_id, m.view_id"), &mut files)?;
    }
    let manifest = json!({
        "schema": EXPORT_SCHEMA, "store_schema": crate::db::STORE_SCHEMA, "subset": subset,
        "subset_rule": if subset == "all" { "every table and text of the private store" } else {
            "texts already published in datasets/ snapshots, with their published records, views (paths relative to the repository root) and split membership; no observations or events" },
        "created_utc": crate::runner::now_utc(), "files": files,
        "texts_note": "texts.jsonl.zst rows carry the exact text and its SHA-256 (of the UTF-8 bytes)",
        "private": subset == "all",
    });
    fs::write(out_dir.join("MANIFEST.json"), serde_json::to_vec_pretty(&manifest)?)?;
    Ok(manifest)
}
