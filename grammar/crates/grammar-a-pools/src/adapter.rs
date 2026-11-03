//! Shared bookkeeping for one adapter run: candidates, a text-free
//! rejection ledger and a report binding inputs, parameters and outputs.
use crate::{
    io::{self, JsonlWriter},
    record,
    text,
};
use anyhow::Result;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::{Path, PathBuf}};

pub const REPORT_SCHEMA: &str = "slop_ninja_a_pool_adapter_report_v1";

pub struct Run {
    out: PathBuf,
    adapter: &'static str,
    collection: String,
    candidates: JsonlWriter,
    rejections: JsonlWriter,
    pub units_seen: u64,
    reasons: BTreeMap<String, u64>,
    inputs: Vec<Value>,
    params: Value,
    words: Vec<usize>,
    max_candidates: Option<u64>,
    buckets: Option<(u32, u32)>,
    counted: BTreeMap<String, u64>,
    metadata_only: Option<JsonlWriter>,
    selection: Option<std::collections::HashSet<String>>,
}

/// Schema of a metadata-only reservation row: identifiers, bibliographic
/// fields and hashes of source units, never prose.
pub const RESERVED_METADATA_SCHEMA: &str = "slop_ninja_a_pool_reserved_unit_metadata_v1";

pub const SAMPLE_DOMAIN: &str = "slop_ninja_a_pool_unit_sample_v1\0";

/// Bucket 0..1000 of a source family key. Split reservations and sampling
/// share this function, so a run restricted to Train buckets never renders
/// a reserved family's prose.
pub fn bucket(family_key: &str) -> u32 {
    (text::h64(format!("{SAMPLE_DOMAIN}{family_key}").as_bytes()) % 1000) as u32
}

/// `LO..HI` with 0 <= LO < HI <= 1000.
pub fn parse_buckets(s: &str) -> Result<(u32, u32)> {
    let (a, b) = s.split_once("..").ok_or_else(|| anyhow::anyhow!("buckets must look like LO..HI"))?;
    let (a, b): (u32, u32) = (a.trim().parse()?, b.trim().parse()?);
    anyhow::ensure!(a < b && b <= 1000, "buckets must satisfy 0 <= LO < HI <= 1000");
    Ok((a, b))
}

impl Run {
    pub fn start(out: &Path, adapter: &'static str, collection: &str, params: Value, max_candidates: Option<u64>) -> Result<Self> {
        Self::start_sampled(out, adapter, collection, params, max_candidates, None)
    }
    /// `buckets`: keep a source family iff `bucket(family key)` lies in
    /// `[LO, HI)`. Selection by identifier only, never by text, model or
    /// detector score.
    pub fn start_sampled(out: &Path, adapter: &'static str, collection: &str, params: Value, max_candidates: Option<u64>, buckets: Option<(u32, u32)>) -> Result<Self> {
        anyhow::ensure!(buckets.is_none_or(|(a, b)| a < b && b <= 1000), "buckets must satisfy 0 <= LO < HI <= 1000");
        io::fresh_dir(out)?;
        Ok(Run {
            out: out.to_owned(),
            adapter,
            collection: collection.to_owned(),
            candidates: JsonlWriter::create(&out.join("candidates.jsonl"))?,
            rejections: JsonlWriter::create(&out.join("rejections.jsonl"))?,
            units_seen: 0,
            reasons: BTreeMap::new(),
            inputs: Vec::new(),
            params,
            words: Vec::new(),
            max_candidates,
            buckets,
            counted: BTreeMap::new(),
            metadata_only: None,
            selection: None,
        })
    }
    /// Metadata-only mode: a family inside the bucket range writes one
    /// text-free row to `reserved-metadata.jsonl` and is never rendered or
    /// screened; no candidate is ever written. Split reservation uses it to
    /// list reserved families before any of their prose is opened.
    pub fn enable_metadata_only(&mut self) -> Result<()> {
        self.metadata_only = Some(JsonlWriter::create(&self.out.join("reserved-metadata.jsonl"))?);
        Ok(())
    }
    /// Keep only these source families (for acquiring one reserved split);
    /// any other family is counted and never rendered.
    pub fn select_families(&mut self, keys: std::collections::HashSet<String>) {
        self.selection = Some(keys);
    }
    pub fn is_metadata_only(&self) -> bool {
        self.metadata_only.is_some()
    }
    pub fn input(&mut self, v: Value) {
        self.inputs.push(v);
    }
    /// True once the optional smoke-run cap is reached.
    pub fn full(&self) -> bool {
        self.max_candidates.is_some_and(|m| self.candidates.rows >= m)
    }
    /// Bucket gate on a source family key; families outside the range are
    /// counted only and their prose is never rendered.
    pub fn sampled(&mut self, family_key: &str) -> bool {
        match self.buckets {
            None => self.selected(family_key),
            Some((a, b)) => {
                let k = bucket(family_key);
                let keep = a <= k && k < b;
                if !keep {
                    self.count("family_bucket_outside_run_range");
                    return false;
                }
                self.selected(family_key)
            }
        }
    }
    fn selected(&mut self, family_key: &str) -> bool {
        let keep = self.selection.as_ref().is_none_or(|s| s.contains(family_key));
        if !keep {
            self.count("family_not_in_selection");
        }
        keep
    }
    /// Bucket gate with a metadata hook. Outside the range: counted, false.
    /// Inside the range in metadata-only mode: `meta()` (which must hold
    /// identifiers and metadata only, never prose) is written with the family
    /// key and bucket, and the family is not rendered (false). Otherwise true.
    pub fn sampled_meta(&mut self, family_key: &str, meta: impl FnOnce() -> Value) -> Result<bool> {
        if !self.sampled(family_key) {
            return Ok(false);
        }
        let Some(w) = self.metadata_only.as_mut() else {
            return Ok(true);
        };
        let mut v = meta();
        anyhow::ensure!(v.get("text").is_none(), "metadata-only rows never carry text");
        v["schema"] = json!(RESERVED_METADATA_SCHEMA);
        v["family_key"] = json!(family_key);
        v["bucket"] = json!(bucket(family_key));
        v["prose_opened"] = json!(false);
        w.write(&v)?;
        *self.counted.entry("metadata_only_rows".to_owned()).or_default() += 1;
        Ok(false)
    }
    /// Bucket gate on a contributor key (`bucket_key`: a site or a writer)
    /// with family selection on the unit's own family key. Used where the
    /// split group is the contributor while the source family (and the
    /// conflict unit at dedup) is one page or article, so one overlapping
    /// page never removes a whole site or writer. Metadata rows carry both
    /// keys and the contributor's bucket.
    pub fn sampled_meta_grouped(&mut self, bucket_key: &str, family_key: &str, meta: impl FnOnce() -> Value) -> Result<bool> {
        if let Some((a, b)) = self.buckets {
            let k = bucket(bucket_key);
            if !(a <= k && k < b) {
                self.count("contributor_bucket_outside_run_range");
                return Ok(false);
            }
        }
        if !self.selected(family_key) {
            return Ok(false);
        }
        let Some(w) = self.metadata_only.as_mut() else {
            return Ok(true);
        };
        let mut v = meta();
        anyhow::ensure!(v.get("text").is_none(), "metadata-only rows never carry text");
        v["schema"] = json!(RESERVED_METADATA_SCHEMA);
        v["family_key"] = json!(family_key);
        v["group_key"] = json!(bucket_key);
        v["bucket"] = json!(bucket(bucket_key));
        v["bucket_basis"] = json!("group_key");
        v["prose_opened"] = json!(false);
        w.write(&v)?;
        *self.counted.entry("metadata_only_rows".to_owned()).or_default() += 1;
        Ok(false)
    }
    /// True if `key` hashes inside this run's bucket range (always true
    /// without a range). Lets an adapter skip a whole case or meeting when
    /// any of its contributors lies outside the rendered range.
    pub fn in_range(&self, key: &str) -> bool {
        self.buckets.is_none_or(|(a, b)| {
            let k = bucket(key);
            a <= k && k < b
        })
    }
    /// Count a high-volume trivial outcome without a ledger row.
    pub fn count(&mut self, reason: &str) {
        *self.counted.entry(reason.to_owned()).or_default() += 1;
    }
    /// Add `n` to a counted outcome (for outcomes tallied by worker threads).
    pub fn count_n(&mut self, reason: &str, n: u64) {
        if n > 0 {
            *self.counted.entry(reason.to_owned()).or_default() += n;
        }
    }
    /// Record a rejected source unit by id and reason only (no text).
    pub fn reject(&mut self, unit_id: &str, reason: &str) -> Result<()> {
        *self.reasons.entry(reason.to_owned()).or_default() += 1;
        self.rejections.write(&json!({"unit_id": unit_id, "reason": reason}))
    }
    /// Final whole-document checks shared by every adapter, then write.
    pub fn accept(&mut self, unit_id: &str, doc: &record::Doc) -> Result<bool> {
        anyhow::ensure!(self.metadata_only.is_none(), "metadata-only runs never accept candidates");
        let t = &doc.excerpt.text;
        let n = text::words(t);
        let reason = if !(text::MIN_WORDS..=text::MAX_WORDS).contains(&n) {
            Some("word_count_outside_150_450")
        } else if let Some(r) = text::damaged(t) {
            Some(r)
        } else if !record::admitted_rights(&doc.rights) {
            Some("license_not_admitted")
        } else if doc.cutoff_policy == "dated_before_cutoff"
            && !doc.date.as_deref().is_some_and(record::before_cutoff)
        {
            Some("not_dated_before_2022_11_30")
        } else {
            None
        };
        if let Some(r) = reason {
            self.reject(unit_id, r)?;
            return Ok(false);
        }
        self.words.push(n);
        self.candidates.write(&doc.to_value())?;
        Ok(true)
    }
    pub fn finish(self, extra: Value) -> Result<Value> {
        let (cand_sha, cand_rows) = self.candidates.finish()?;
        let (rej_sha, rej_rows) = self.rejections.finish()?;
        let metadata = match self.metadata_only {
            Some(w) => {
                let (sha, rows) = w.finish()?;
                json!({"file": "reserved-metadata.jsonl", "rows": rows, "sha256": sha,
                       "rule": "families inside the bucket range listed by identifier and metadata only; prose never rendered or screened"})
            }
            None => json!(null),
        };
        let mut w = self.words;
        w.sort_unstable();
        let stats = if w.is_empty() {
            json!(null)
        } else {
            json!({"min": w[0], "median": w[w.len() / 2], "max": w[w.len() - 1],
                   "mean": w.iter().sum::<usize>() as f64 / w.len() as f64})
        };
        let report = json!({
            "schema": REPORT_SCHEMA,
            "adapter": self.adapter,
            "collection": self.collection,
            "finished_utc": io::now_utc(),
            "complete": true,
            "parameters": self.params,
            "inputs": self.inputs,
            "units_seen": self.units_seen,
            "candidates": cand_rows,
            "candidates_sha256": cand_sha,
            "rejections": rej_rows,
            "rejections_sha256": rej_sha,
            "rejection_reasons": self.reasons,
            "counted_without_ledger_rows": self.counted,
            "family_buckets": self.buckets.map(|(a, b)| json!([a, b])),
            "metadata_only": metadata,
            "family_selection_size": self.selection.as_ref().map(|s| s.len()),
            "family_bucket_rule": self.buckets.map(|_| format!("keep iff (first 8 bytes of SHA-256(\"{}\\0\" + source family key) as big-endian u64) mod 1000 lies in [LO, HI)", SAMPLE_DOMAIN.trim_end_matches('\0'))),
            "candidate_word_counts": stats,
            "word_bound": [text::MIN_WORDS, text::MAX_WORDS],
            "cutoff": record::CUTOFF,
            "detector_calls": 0,
            "selection_by_model_or_detector_score": false,
            "split_assigned": false,
            "extra": extra,
            "limits": [
                "Candidates are not admitted until deduplicated against every registered prior snapshot and the rest of the pool.",
                "Historical or pre-cutoff human text is a proxy, not an authenticated unaided production history.",
                "Pretraining exposure for the encoder and comparator is unknown.",
                "Word bounds do not establish tokenizer fit."
            ],
        });
        io::write_json(&self.out.join("report.json"), &report)?;
        Ok(report)
    }
}
