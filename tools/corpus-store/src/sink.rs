//! Writers for the structured tables. Every insert is idempotent: natural keys
//! are unique and repeated ingestion of the same input is a no-op.

use crate::packs::Packs;
use crate::skeleton::sha256;
use crate::textstats;
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::Value;
use std::collections::HashSet;

pub struct Sink<'a> {
    pub db: &'a Connection,
    pub packs: &'a mut Packs,
    pub source_id: String,
    pub adapter: String,
    /// Texts known to have a `text` row (cache for this run).
    known_texts: HashSet<[u8; 32]>,
    pub n_records: u64,
    pub n_events: u64,
    pub n_observations: u64,
    pub n_new_texts: u64,
    pub n_problems: u64,
    tx_open: bool,
    tx_started: std::time::Instant,
    tx_units: u64,
}

/// Position of the line that produced a row.
#[derive(Clone, Default)]
pub struct At {
    pub file_sha256: String,
    pub line_no: i64,
    pub line_sha256: String,
    pub path: String,
}

#[derive(Default, Clone)]
pub struct Record {
    pub role: String,
    pub record_id: Option<String>,
    pub text_sha256: Option<String>,
    pub dataset: Option<String>,
    pub collection: Option<String>,
    pub domain: Option<String>,
    pub genre: Option<String>,
    pub source_work: Option<String>,
    pub family: Option<String>,
    pub source_group: Option<String>,
    pub writer_key: Option<String>,
    pub original_doc_id: Option<String>,
    pub doc_date: Option<String>,
    pub composition_date: Option<String>,
    pub licence: Option<String>,
    pub rights_basis: Option<String>,
    pub cohort_id: Option<String>,
    pub origin: Option<String>,
    pub share: Option<f64>,
    pub ai_byte_spans: Option<String>,
    pub generator_label: Option<String>,
    pub generator_key: Option<String>,
    pub operation: Option<String>,
    pub recipe_sha256: Option<String>,
    pub parent_record_id: Option<String>,
    pub parent_text_sha256: Option<String>,
    pub admission: Option<String>,
    pub split_hint: Option<String>,
    pub extra: Option<Value>,
}

#[derive(Default, Clone)]
pub struct Event {
    pub event_key: String,
    pub kind: String,
    pub generator_label: Option<String>,
    pub generator_key: Option<String>,
    pub requested_model: Option<String>,
    pub reported_model: Option<String>,
    pub operation: Option<String>,
    pub status: String,
    pub prompt_sha256: Option<String>,
    pub params: Option<Value>,
    pub receipt_sha256: Option<String>,
    pub response_sha256: Option<String>,
    pub request_id: Option<String>,
    pub attempt: Option<i64>,
    pub started_utc: Option<String>,
    pub finished_utc: Option<String>,
    pub input_text_sha256: Option<String>,
    pub output_text_sha256: Option<String>,
    pub split_hint: Option<String>,
    pub extra: Option<Value>,
}

#[derive(Default, Clone)]
pub struct Observation {
    pub obs_key: String,
    pub detector: String,
    pub detector_version: Option<String>,
    pub text_sha256: Option<String>,
    pub item_id: Option<String>,
    pub job_ref: Option<String>,
    pub observed_utc: Option<String>,
    pub fraction_ai: Option<f64>,
    pub fraction_ai_assisted: Option<f64>,
    pub fraction_human: Option<f64>,
    pub score: Option<f64>,
    pub prediction: Option<Value>,
    pub cost_units: Option<f64>,
    pub status: String,
    pub context: Option<String>,
}

fn js(v: &Option<Value>) -> Option<String> {
    v.as_ref().map(|v| v.to_string())
}

impl<'a> Sink<'a> {
    pub fn new(db: &'a Connection, packs: &'a mut Packs, source_id: &str, adapter: &str) -> Sink<'a> {
        Sink {
            db,
            packs,
            source_id: source_id.into(),
            adapter: adapter.into(),
            known_texts: HashSet::new(),
            n_records: 0,
            n_events: 0,
            n_observations: 0,
            n_new_texts: 0,
            n_problems: 0,
            tx_open: false,
            tx_started: std::time::Instant::now(),
            tx_units: 0,
        }
    }

    /// Open the batch transaction if none is open.
    pub fn begin(&mut self) -> Result<()> {
        if !self.tx_open {
            self.db.execute_batch("BEGIN")?;
            self.tx_open = true;
            self.tx_started = std::time::Instant::now();
            self.tx_units = 0;
        }
        Ok(())
    }

    /// Count work done in the open transaction (files, or batches of lines).
    pub fn work(&mut self, units: u64) {
        self.tx_units += units;
    }

    /// Commit (packs synced first) when forced or when enough work or time has
    /// accumulated, then reopen a transaction unless `close`.
    pub fn checkpoint(&mut self, force: bool, close: bool) -> Result<()> {
        if !self.tx_open {
            if !close {
                self.begin()?;
            }
            return Ok(());
        }
        if force || close || self.tx_units >= 2000 || self.tx_started.elapsed().as_secs() >= 30 {
            self.packs.sync(self.db)?;
            self.db.execute_batch("COMMIT")?;
            self.tx_open = false;
            if !close {
                self.begin()?;
            }
        }
        Ok(())
    }

    pub fn rollback(&mut self) {
        if self.tx_open {
            let _ = self.db.execute_batch("ROLLBACK");
            self.tx_open = false;
        }
    }

    /// Store bytes in the blob store; returns the hex SHA-256.
    pub fn blob(&mut self, bytes: &[u8]) -> Result<String> {
        let sha = sha256(bytes);
        self.packs.put(self.db, &sha, bytes, None)?;
        Ok(hex::encode(sha))
    }

    /// Register a corpus text (stored once); returns its hex SHA-256.
    pub fn text(&mut self, text: &str) -> Result<String> {
        let sha = sha256(text.as_bytes());
        let hex = hex::encode(sha);
        if self.known_texts.contains(&sha) {
            return Ok(hex);
        }
        self.packs.put(self.db, &sha, text.as_bytes(), None)?;
        let s = textstats::stats(text);
        let n = self
            .db
            .prepare_cached(
                "INSERT OR IGNORE INTO text(text_sha256, n_bytes, n_chars, n_words, n_lines, language, language_score, first_source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?
            .execute(params![hex, s.n_bytes as i64, s.n_chars as i64, s.n_words as i64, s.n_lines as i64, s.language, s.language_score, self.source_id])?;
        self.n_new_texts += n as u64;
        if self.known_texts.len() > 2_000_000 {
            self.known_texts.clear();
        }
        self.known_texts.insert(sha);
        Ok(hex)
    }

    pub fn record(&mut self, at: &At, r: &Record) -> Result<()> {
        let n = self.db.prepare_cached(
            "INSERT OR IGNORE INTO record(file_sha256, line_no, role, line_sha256, source_id, adapter, record_id, text_sha256,
               dataset, collection, domain, genre, source_work, family, source_group, writer_key, original_doc_id,
               doc_date, composition_date, licence, rights_basis, cohort_id, origin, share, ai_byte_spans,
               generator_label, generator_key, operation, recipe_sha256, parent_record_id, parent_text_sha256,
               admission, split_hint, extra_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34)")?
            .execute(params![
                at.file_sha256, at.line_no, r.role, at.line_sha256, self.source_id, self.adapter, r.record_id, r.text_sha256,
                r.dataset, r.collection, r.domain, r.genre, r.source_work, r.family, r.source_group, r.writer_key, r.original_doc_id,
                r.doc_date, r.composition_date, r.licence, r.rights_basis, r.cohort_id, r.origin, r.share, r.ai_byte_spans,
                r.generator_label, r.generator_key, r.operation, r.recipe_sha256, r.parent_record_id, r.parent_text_sha256,
                r.admission, r.split_hint, js(&r.extra)])?;
        self.n_records += n as u64;
        Ok(())
    }

    pub fn event(&mut self, at: Option<&At>, e: &Event) -> Result<()> {
        let (f, l, ls) = match at {
            Some(a) => (Some(a.file_sha256.clone()), Some(a.line_no), Some(a.line_sha256.clone())),
            None => (None, None, None),
        };
        let n = self.db.prepare_cached(
            "INSERT OR IGNORE INTO event(event_key, kind, source_id, file_sha256, line_no, line_sha256, generator_label, generator_key,
               requested_model, reported_model, operation, status, prompt_sha256, params_json, receipt_sha256, request_id, attempt,
               started_utc, finished_utc, input_text_sha256, output_text_sha256, split_hint, extra_json, response_sha256)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)")?
            .execute(params![
                e.event_key, e.kind, self.source_id, f, l, ls, e.generator_label, e.generator_key, e.requested_model,
                e.reported_model, e.operation, e.status, e.prompt_sha256, js(&e.params), e.receipt_sha256, e.request_id,
                e.attempt, e.started_utc, e.finished_utc, e.input_text_sha256, e.output_text_sha256, e.split_hint, js(&e.extra),
                e.response_sha256])?;
        self.n_events += n as u64;
        Ok(())
    }

    pub fn observation(&mut self, at: Option<&At>, o: &Observation) -> Result<()> {
        let (f, l, ls) = match at {
            Some(a) => (Some(a.file_sha256.clone()), Some(a.line_no), Some(a.line_sha256.clone())),
            None => (None, None, None),
        };
        let n = self.db.prepare_cached(
            "INSERT OR IGNORE INTO observation(obs_key, detector, detector_version, text_sha256, item_id, job_ref, observed_utc,
               fraction_ai, fraction_ai_assisted, fraction_human, score, prediction_json, cost_units, status, context,
               source_id, file_sha256, line_no, line_sha256)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)")?
            .execute(params![
                o.obs_key, o.detector, o.detector_version, o.text_sha256, o.item_id, o.job_ref, o.observed_utc,
                o.fraction_ai, o.fraction_ai_assisted, o.fraction_human, o.score, js(&o.prediction), o.cost_units,
                o.status, o.context, self.source_id, f, l, ls])?;
        self.n_observations += n as u64;
        Ok(())
    }

    pub fn split_member(&mut self, experiment: &str, record_id: &str, text_sha256: Option<&str>, group: Option<&str>, role: &str, view_id: &str) -> Result<()> {
        self.db
            .prepare_cached("INSERT OR IGNORE INTO split_member(experiment, record_id, text_sha256, source_group, role, view_id) VALUES (?1,?2,?3,?4,?5,?6)")?
            .execute(params![experiment, record_id, text_sha256, group, role, view_id])?;
        Ok(())
    }

    pub fn exposure(&mut self, key: &str, text_sha256: Option<&str>, group: Option<&str>, kind: &str, basis: &str, date: Option<&str>) -> Result<()> {
        self.db
            .prepare_cached("INSERT OR IGNORE INTO exposure(exposure_key, text_sha256, source_group, kind, basis, date, source_id) VALUES (?1,?2,?3,?4,?5,?6,?7)")?
            .execute(params![key, text_sha256, group, kind, basis, date, self.source_id])?;
        Ok(())
    }

    pub fn rights(&mut self, cohort: &str, licence: Option<&str>, basis: Option<&str>, commercial: &str, uses: Option<&str>, date_rule: Option<&str>) -> Result<()> {
        self.db
            .prepare_cached("INSERT OR IGNORE INTO rights(cohort_id, licence, rights_basis, commercial_use, uses, date_rule, source_id) VALUES (?1,?2,?3,?4,?5,?6,?7)")?
            .execute(params![cohort, licence, basis, commercial, uses, date_rule, self.source_id])?;
        Ok(())
    }

    pub fn problem(&mut self, kind: &str, path: Option<&str>, line_no: Option<i64>, detail: &str) -> Result<()> {
        self.db
            .prepare_cached("INSERT OR IGNORE INTO problem(source_id, kind, path, line_no, detail) VALUES (?1,?2,?3,?4,?5)")?
            .execute(params![self.source_id, kind, path, line_no.unwrap_or(-1), detail])?;
        self.n_problems += 1;
        Ok(())
    }
}
