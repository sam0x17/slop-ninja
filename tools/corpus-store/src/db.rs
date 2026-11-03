//! SQLite schema of the consolidated corpus.

use anyhow::{Result, ensure};
use rusqlite::Connection;
use std::path::Path;

pub const STORE_SCHEMA: &str = "slop_ninja_corpus_store_v1";

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

-- Content-addressed bytes (documents, prompts, receipts, skeleton chunks, whole files).
CREATE TABLE IF NOT EXISTS pack (pack_id INTEGER PRIMARY KEY, committed_len INTEGER NOT NULL, sealed INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS blob (
  sha256 BLOB PRIMARY KEY, pack_id INTEGER NOT NULL, offset INTEGER NOT NULL,
  zlen INTEGER NOT NULL, len INTEGER NOT NULL) WITHOUT ROWID;

-- One row per unique corpus text (exact UTF-8 bytes).
CREATE TABLE IF NOT EXISTS text (
  text_sha256 TEXT PRIMARY KEY,
  n_bytes INTEGER NOT NULL, n_chars INTEGER NOT NULL, n_words INTEGER NOT NULL,
  n_lines INTEGER NOT NULL,
  language TEXT NOT NULL, language_score REAL NOT NULL,
  first_source TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS text_tokens (
  text_sha256 TEXT NOT NULL, tokenizer_sha256 TEXT NOT NULL, n_tokens INTEGER NOT NULL,
  PRIMARY KEY (text_sha256, tokenizer_sha256)) WITHOUT ROWID;

-- Ingestion sources, each with an input manifest (paths, sizes, SHA-256).
CREATE TABLE IF NOT EXISTS source (
  source_id TEXT PRIMARY KEY, adapter TEXT NOT NULL, adapter_version INTEGER NOT NULL,
  priority INTEGER NOT NULL, root TEXT NOT NULL, spec_json TEXT NOT NULL,
  input_manifest_sha256 TEXT, status TEXT NOT NULL, started_utc TEXT, finished_utc TEXT,
  counts_json TEXT);
CREATE TABLE IF NOT EXISTS source_input (
  source_id TEXT NOT NULL, rel_path TEXT NOT NULL, file_sha256 TEXT NOT NULL,
  n_bytes INTEGER NOT NULL, role TEXT NOT NULL, done INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (source_id, rel_path)) WITHOUT ROWID;

-- Exact files: line count and the skeleton chunks that rebuild them byte for byte.
CREATE TABLE IF NOT EXISTS file (
  file_sha256 TEXT PRIMARY KEY, n_bytes INTEGER NOT NULL, n_lines INTEGER,
  format TEXT NOT NULL, whole_blob INTEGER NOT NULL, complete INTEGER NOT NULL,
  final_newline INTEGER NOT NULL DEFAULT 1) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS file_chunk (
  file_sha256 TEXT NOT NULL, chunk_no INTEGER NOT NULL, first_line INTEGER NOT NULL,
  n_lines INTEGER NOT NULL, blob_sha256 BLOB NOT NULL,
  PRIMARY KEY (file_sha256, chunk_no)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS file_path (
  path TEXT NOT NULL, file_sha256 TEXT NOT NULL, source_id TEXT NOT NULL,
  PRIMARY KEY (path, file_sha256)) WITHOUT ROWID;

-- A record that carries (or names) a corpus text: provenance, rights and labels.
-- Keyed by the exact line it came from, so a file shared by several sources or
-- snapshot versions yields one set of records.
CREATE TABLE IF NOT EXISTS record (
  record_uid INTEGER PRIMARY KEY,
  file_sha256 TEXT NOT NULL, line_no INTEGER NOT NULL, role TEXT NOT NULL,
  line_sha256 TEXT NOT NULL, source_id TEXT NOT NULL, adapter TEXT NOT NULL,
  record_id TEXT, text_sha256 TEXT,
  dataset TEXT, collection TEXT, domain TEXT, genre TEXT,
  source_work TEXT, family TEXT, source_group TEXT, writer_key TEXT, original_doc_id TEXT,
  doc_date TEXT, composition_date TEXT, licence TEXT, rights_basis TEXT, cohort_id TEXT,
  origin TEXT, share REAL, ai_byte_spans TEXT, generator_label TEXT, generator_key TEXT,
  operation TEXT, recipe_sha256 TEXT, parent_record_id TEXT, parent_text_sha256 TEXT,
  admission TEXT, split_hint TEXT, extra_json TEXT,
  UNIQUE (file_sha256, line_no, role));
CREATE INDEX IF NOT EXISTS record_text ON record(text_sha256);
CREATE INDEX IF NOT EXISTS record_id ON record(record_id);
CREATE INDEX IF NOT EXISTS record_group ON record(source_group);

-- Every production event (generation request/attempt, human acquisition, transformation).
CREATE TABLE IF NOT EXISTS event (
  event_uid INTEGER PRIMARY KEY,
  event_key TEXT NOT NULL UNIQUE, kind TEXT NOT NULL, source_id TEXT NOT NULL,
  file_sha256 TEXT, line_no INTEGER, line_sha256 TEXT,
  generator_label TEXT, generator_key TEXT, requested_model TEXT, reported_model TEXT,
  operation TEXT, status TEXT NOT NULL, prompt_sha256 TEXT, params_json TEXT,
  receipt_sha256 TEXT, response_sha256 TEXT, request_id TEXT, attempt INTEGER,
  started_utc TEXT, finished_utc TEXT,
  input_text_sha256 TEXT, output_text_sha256 TEXT, split_hint TEXT, extra_json TEXT);
CREATE INDEX IF NOT EXISTS event_output ON event(output_text_sha256);
CREATE INDEX IF NOT EXISTS event_kind ON event(kind, status);
CREATE INDEX IF NOT EXISTS event_request ON event(request_id);

-- Every detector observation; evidence only, never a label.
CREATE TABLE IF NOT EXISTS observation (
  obs_uid INTEGER PRIMARY KEY,
  obs_key TEXT NOT NULL UNIQUE, detector TEXT NOT NULL, detector_version TEXT,
  text_sha256 TEXT, item_id TEXT, job_ref TEXT, observed_utc TEXT,
  fraction_ai REAL, fraction_ai_assisted REAL, fraction_human REAL, score REAL,
  prediction_json TEXT, cost_units REAL, status TEXT NOT NULL, context TEXT,
  source_id TEXT NOT NULL, file_sha256 TEXT, line_no INTEGER, line_sha256 TEXT);
CREATE INDEX IF NOT EXISTS observation_text ON observation(text_sha256);

-- Frozen experiment views and their exact membership.
CREATE TABLE IF NOT EXISTS view (
  view_id TEXT PRIMARY KEY, kind TEXT NOT NULL, path TEXT NOT NULL, file_sha256 TEXT NOT NULL,
  n_rows INTEGER NOT NULL, expected_sha256 TEXT, expected_basis TEXT, source_id TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS view_member (
  view_id TEXT NOT NULL, ordinal INTEGER NOT NULL, record_id TEXT, text_sha256 TEXT,
  line_sha256 TEXT NOT NULL, split TEXT, source_group TEXT,
  PRIMARY KEY (view_id, ordinal)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS view_member_text ON view_member(text_sha256);

-- Split membership per experiment view (role: train, train_mining, dev_iter,
-- dev_select, cal, test_sealed, test_consumed, held, reserved, diagnostic, ...).
CREATE TABLE IF NOT EXISTS split_member (
  experiment TEXT NOT NULL, record_id TEXT NOT NULL, text_sha256 TEXT, source_group TEXT,
  role TEXT NOT NULL, view_id TEXT NOT NULL,
  PRIMARY KEY (experiment, record_id, view_id)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS split_member_text ON split_member(text_sha256);
CREATE INDEX IF NOT EXISTS split_member_group ON split_member(experiment, source_group);

-- Exposure records: a text or group that was opened, scored, published or consumed.
CREATE TABLE IF NOT EXISTS exposure (
  exposure_key TEXT PRIMARY KEY, text_sha256 TEXT, source_group TEXT, kind TEXT NOT NULL,
  basis TEXT NOT NULL, date TEXT, source_id TEXT NOT NULL) WITHOUT ROWID;

-- Per-cohort rights: licence, basis and commercial-use status.
CREATE TABLE IF NOT EXISTS rights (
  cohort_id TEXT PRIMARY KEY, licence TEXT, rights_basis TEXT, commercial_use TEXT NOT NULL,
  uses TEXT, date_rule TEXT, source_id TEXT NOT NULL) WITHOUT ROWID;

-- Per-source lookups (status counts, re-ingestion checks).
CREATE INDEX IF NOT EXISTS record_source ON record(source_id);
CREATE INDEX IF NOT EXISTS event_source ON event(source_id);
CREATE INDEX IF NOT EXISTS observation_source ON observation(source_id);
CREATE INDEX IF NOT EXISTS text_first_source ON text(first_source);

-- Problems found while ingesting (never silently dropped).
CREATE TABLE IF NOT EXISTS problem (
  problem_uid INTEGER PRIMARY KEY, source_id TEXT NOT NULL, kind TEXT NOT NULL,
  path TEXT, line_no INTEGER, detail TEXT NOT NULL,
  UNIQUE (source_id, kind, path, line_no, detail));
"#;

/// Open an existing store read-only (`immutable=1`: no writer may be live).
/// Temporary tables and sorts go to `SQLITE_TMPDIR` when set.
pub fn open_ro(path: &Path) -> Result<Connection> {
    ensure!(path.exists(), "{} does not exist", path.display());
    let uri = format!("file:{}?immutable=1", path.display());
    let db = Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let cache_mb: i64 = std::env::var("CORPUS_SQLITE_CACHE_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(256);
    db.pragma_update(None, "cache_size", -(cache_mb * 1024))?;
    db.pragma_update(None, "temp_store", "FILE")?;
    let got: String = db.query_row("SELECT value FROM meta WHERE key = 'schema'", [], |r| r.get(0))?;
    ensure!(got == STORE_SCHEMA, "store schema {got}, expected {STORE_SCHEMA}");
    Ok(db)
}

pub fn open(path: &Path) -> Result<Connection> {
    let db = Connection::open(path)?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "synchronous", "NORMAL")?;
    db.pragma_update(None, "foreign_keys", "OFF")?;
    // Page cache in MiB (default 256); large ingests benefit from more.
    let cache_mb: i64 = std::env::var("CORPUS_SQLITE_CACHE_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(256);
    db.pragma_update(None, "cache_size", -(cache_mb * 1024))?;
    db.pragma_update(None, "temp_store", "FILE")?;
    db.busy_timeout(std::time::Duration::from_secs(60))?;
    db.execute_batch(DDL)?;
    let got: Option<String> = db
        .query_row("SELECT value FROM meta WHERE key = 'schema'", [], |r| r.get(0))
        .ok();
    match got {
        None => {
            db.execute("INSERT INTO meta(key, value) VALUES ('schema', ?1)", [STORE_SCHEMA])?;
        }
        Some(s) => ensure!(s == STORE_SCHEMA, "store schema {s}, expected {STORE_SCHEMA}"),
    }
    Ok(db)
}
