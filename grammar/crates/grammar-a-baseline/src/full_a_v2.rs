//! Text-free readers for the shared-contract full-A artifacts (v2): bundle
//! split rows, completed fits and raw prediction runs, and the CAL-side
//! records built on them (sealed selection, calibration).
//!
//! Rows are deserialized into the metadata fields below only; the exact text
//! (`text`, or the file behind `text_ref`) is never read here.
use crate::mining::{bundle_digests, canonical_split, sha256_file};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;

pub const FIT_SCHEMA: &str = "slop_ninja_full_a_fit_v2";
pub const RAW_SCHEMA: &str = "slop_ninja_full_a_raw_predictions_v2";
pub const SELECTION_SCHEMA: &str = "slop_ninja_full_a_selection_v2";
pub const CALIBRATION_SCHEMA: &str = "slop_ninja_full_a_calibration_v2";
pub const CAL_SPLIT: &str = "cal";
pub const LOGIT_SEMANTICS: &str = "log(p_model+p_mixed)-log(p_human) of the final uncalibrated origin";

/// Metadata of one bundle row (no text).
#[derive(Clone, Debug, Deserialize)]
pub struct RowFacts {
    pub id: String,
    pub split: String,
    pub text_sha256: String,
    pub origin: String,
    pub generator_label: Option<String>,
    pub source_group: String,
}

impl RowFacts {
    /// 0 human-only, 1 model-only, 2 mixed.
    pub fn origin_index(&self) -> Result<usize> {
        Ok(match self.origin.as_str() {
            "human_only" => 0,
            "model_only" => 1,
            "mixed" => 2,
            other => anyhow::bail!("row {}: origin {other:?}", self.id),
        })
    }
}

/// Rows of one split of a bundle, in file order; refuses Test rows anywhere.
pub fn split_rows(bundle: &Path, split: &str) -> Result<Vec<RowFacts>> {
    let wanted = canonical_split(split)?;
    ensure!(wanted != "test", "Test rows are never read here");
    let reader = BufReader::new(File::open(bundle.join("rows.jsonl"))?);
    let mut out = Vec::new();
    let mut ids = HashSet::new();
    for (n, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let r: RowFacts = serde_json::from_str(&line).with_context(|| format!("rows.jsonl line {}", n + 1))?;
        let split = canonical_split(&r.split)?;
        ensure!(split != "test", "a training bundle must not contain Test rows");
        if split == wanted {
            ensure!(ids.insert(r.id.clone()), "duplicate {wanted} id {}", r.id);
            r.origin_index()?;
            out.push(r);
        }
    }
    ensure!(!out.is_empty(), "no {wanted} rows in the bundle");
    Ok(out)
}

fn read(p: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(p).with_context(|| format!("read {}", p.display()))?)
        .with_context(|| format!("parse {}", p.display()))
}

/// A completed v2 fit on this bundle and config.
pub struct Fit {
    pub report: Value,
    pub fit_sha256: String,
}

pub fn completed_fit(fit: &Path, bundle: &Path, config: &Path) -> Result<Fit> {
    let report = read(&fit.join("fit.json"))?;
    let b = &report["binding"];
    ensure!(report["schema"] == FIT_SCHEMA && report["complete"] == true && report["final_test_opened"] == false &&
            report["cohort_weight_log"]["complete"] == true,
            "{} is not a complete v2 fit", fit.display());
    ensure!(b["bundle_sha256"] == bundle_digests(bundle)? &&
            b["contract_sha256"].as_str() == Some(sha256_file(&config.join("contract.json"))?.as_str()),
            "fit {} is bound to another bundle or contract", fit.display());
    Ok(Fit { fit_sha256: sha256_file(&fit.join("fit.json"))?, report })
}

/// A raw prediction run (`export_full_a.py raw`) of `part`, bound to the fit.
pub struct RawRun {
    pub manifest: Value,
    pub manifest_sha256: String,
    pub rows: BTreeMap<String, Value>,
}

pub fn raw_run(raw: &Path, part: &str, bundle: &Path, fit: Option<&Fit>) -> Result<RawRun> {
    let manifest = read(&raw.join("manifest.json"))?;
    ensure!(manifest["schema"] == RAW_SCHEMA && manifest["complete"] == true && manifest["partition"] == part &&
            manifest["calibrated"] == false && manifest["final_test_opened"] == false &&
            manifest["involvement_logit_semantics"] == LOGIT_SEMANTICS,
            "{} is not a complete uncalibrated raw {part} run", raw.display());
    ensure!(manifest["bundle_sha256"] == bundle_digests(bundle)? &&
            manifest["raw_sha256"].as_str() == Some(sha256_file(&raw.join("raw.jsonl"))?.as_str()),
            "raw {part} run is not bound to this bundle or changed");
    if let Some(f) = fit {
        ensure!(manifest["fit_sha256"].as_str() == Some(f.fit_sha256.as_str()) &&
                manifest["selected_checkpoint_sha256"] == f.report["selected_checkpoint_sha256"] &&
                manifest["contract_sha256"] == f.report["binding"]["contract_sha256"] &&
                manifest["arm"] == f.report["binding"]["arm"] && manifest["seed"] == f.report["binding"]["seed"],
                "raw {part} run is not from this fit's selected checkpoint");
    }
    let mut rows = BTreeMap::new();
    let reader = BufReader::new(File::open(raw.join("raw.jsonl"))?);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line)?;
        let id = v["id"].as_str().context("raw id")?.to_owned();
        ensure!(v["involvement_logit"].as_f64().is_some_and(f64::is_finite), "raw {id}: involvement logit");
        ensure!(rows.insert(id.clone(), v).is_none(), "duplicate raw row {id}");
    }
    ensure!(manifest["rows"].as_u64() == Some(rows.len() as u64), "raw row count differs from its manifest");
    Ok(RawRun { manifest_sha256: sha256_file(&raw.join("manifest.json"))?, manifest, rows })
}

/// Every split row has exactly one raw row with the same text digest.
pub fn joined<'a>(rows: &'a [RowFacts], raw: &'a RawRun) -> Result<Vec<(&'a RowFacts, &'a Value)>> {
    ensure!(rows.len() == raw.rows.len(), "raw coverage: {} rows, {} predictions", rows.len(), raw.rows.len());
    rows.iter()
        .map(|r| {
            let p = raw.rows.get(&r.id).with_context(|| format!("missing raw prediction {}", r.id))?;
            ensure!(p["text_sha256"].as_str() == Some(r.text_sha256.as_str()), "raw {} text binding differs", r.id);
            Ok((r, p))
        })
        .collect()
}

/// A sealed selection record (`slop_ninja_full_a_selection_v2`) that froze
/// this fit's checkpoint without reading CAL.
pub fn sealed_selection(path: &Path, bundle: &Path, fit: &Fit) -> Result<Value> {
    let s = read(path)?;
    ensure!(s["schema"] == SELECTION_SCHEMA && s["complete"] == true && s["final_test_opened"] == false &&
            s["calibration_read_for_selection"] == false &&
            ["dev_iter", "dev_select"].iter().any(|x| s["selection_split"] == *x) &&
            s["bundle_sha256"] == bundle_digests(bundle)?,
            "sealed Dev-iter/Dev-select selection on this bundle required before CAL");
    let b = &fit.report["binding"];
    let runs: Vec<&Value> = s["runs"].as_array().context("selection runs")?.iter()
        .filter(|r| r["arm"] == b["arm"] && r["seed"] == b["seed"]).collect();
    ensure!(runs.len() == 1 && runs[0]["fit_sha256"].as_str() == Some(fit.fit_sha256.as_str()) &&
            runs[0]["selected_checkpoint_sha256"] == fit.report["selected_checkpoint_sha256"],
            "checkpoint not frozen by the sealed selection");
    Ok(s)
}
