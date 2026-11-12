//! Seed-ensemble means of full-A outputs (protocol doc-r2).
//!
//! `distill_targets` averages the FP32 raw heads that `raw_full_a.py train`
//! writes for several fits (the teachers) on the same Train block, in
//! probability space: the origin distribution, the document share, the
//! conditional mixed share q and the conditional AI attribution distribution
//! (the softmax of each teacher's AI logits). It refuses inputs whose row sets,
//! text digests, bundle, contract or AI roster disagree, a teacher given twice
//! and any value outside its range, and writes one targets file (a header line,
//! then one row per Train row) that names every input digest. The trainer
//! (`full_a_doc.py`) binds that file by its digest.
//!
//! `mining_mean` averages `mine_full_a.py` score files of several fits on the
//! same pool and bundle into one file in the same v2 schema, so the mining
//! selection reads the mean P(any AI) as it reads one fit's scores.
//!
//! Means are sums in input order divided by the input count. Text-free: only
//! ids, digests and numbers are read or written.
use crate::mining::{self, SCORE_SCHEMA};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

pub const RAW_HEADS_SCHEMA: &str = "slop_ninja_full_a_raw_heads_v1";
pub const TARGETS_SCHEMA: &str = "slop_ninja_full_a_distill_targets_v1";
pub const MEAN_BINDING_SCHEMA: &str = "slop_ninja_full_a_mining_mean_binding_v1";
pub const TARGETS_RULE: &str = "slop_ninja_seed_mean_targets_v1: per Train row, the mean over the teachers (sum in \
                                input order / count) of the FP32 raw uncalibrated origin distribution, document \
                                share, conditional mixed share and softmax(AI logits) in f64";
pub const MINING_MEAN_RULE: &str = "slop_ninja_seed_mean_mining_v1: per pool row, the mean over the fits (sum in \
                                    input order / count) of the uncalibrated P(model_only)+P(mixed)";
/// Probability vectors read from FP32 heads sum to 1 within this tolerance.
pub const PROBABILITY_SUM_TOLERANCE: f64 = 1e-6;
const ORIGINS: [&str; 3] = ["human_only", "model_only", "mixed"];

fn str_of<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key).and_then(Value::as_str).with_context(|| format!("{key} missing"))
}

fn probability(v: &Value, what: &str) -> Result<f64> {
    let x = v.as_f64().with_context(|| format!("{what}: number required"))?;
    ensure!(x.is_finite() && (0.0..=1.0).contains(&x), "{what}: outside [0, 1]");
    Ok(x)
}

fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("read {}", path.display()))?)
        .with_context(|| format!("parse {}", path.display()))
}

/// Write `bytes` to a new file (refusing an existing one) through a sibling temporary file.
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    ensure!(!path.exists(), "{} exists; outputs are written once", path.display());
    let tmp = path.with_extension("tmp-seed-mean");
    {
        let mut f = BufWriter::new(File::create_new(&tmp).with_context(|| format!("create {}", tmp.display()))?);
        f.write_all(bytes)?;
        f.into_inner()?.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Softmax in f64 with the maximum subtracted.
pub fn softmax(logits: &[f64]) -> Vec<f64> {
    let m = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let e: Vec<f64> = logits.iter().map(|z| (z - m).exp()).collect();
    let s: f64 = e.iter().sum();
    e.iter().map(|x| x / s).collect()
}

fn mean(values: impl Iterator<Item = f64>, n: usize) -> f64 {
    values.fold(0.0, |a, b| a + b) / n as f64
}

// ------------------------------------------------------------------ distillation targets

#[derive(Clone, Debug, PartialEq)]
pub struct RawRow {
    pub id: String,
    pub text_sha256: String,
    pub origin: [f64; 3],
    pub share: f64,
    pub q: f64,
    pub ai: Vec<f64>,
}

#[derive(Debug)]
pub struct RawInput {
    pub manifest: Value,
    pub manifest_sha256: String,
    pub rows: Vec<RawRow>,
}

fn raw_row(v: &Value, roster: usize) -> Result<RawRow> {
    let id = str_of(v, "id")?.to_owned();
    let o = v.get("uncalibrated_origin").context("uncalibrated_origin missing")?;
    let mut origin = [0.0; 3];
    for (k, name) in ORIGINS.iter().enumerate() {
        origin[k] = probability(&o[*name], &format!("row {id}: origin {name}"))?;
    }
    ensure!((origin.iter().sum::<f64>() - 1.0).abs() <= PROBABILITY_SUM_TOLERANCE, "row {id}: origin sum");
    let logits = v["ai_logits"].as_array().with_context(|| format!("row {id}: ai_logits"))?;
    ensure!(logits.len() == roster, "row {id}: {} AI logits for a roster of {roster}", logits.len());
    let mut z = Vec::with_capacity(roster);
    for x in logits {
        let x = x.as_f64().with_context(|| format!("row {id}: AI logit"))?;
        ensure!(x.is_finite(), "row {id}: non-finite AI logit");
        z.push(x);
    }
    Ok(RawRow { text_sha256: str_of(v, "text_sha256")?.to_owned(), origin,
                share: probability(&v["uncalibrated_share"], &format!("row {id}: share"))?,
                q: probability(&v["conditional_mixed_share"], &format!("row {id}: conditional share"))?,
                ai: softmax(&z), id })
}

/// One `raw_full_a.py` output directory, checked against its manifest.
pub fn read_raw_dir(dir: &Path) -> Result<RawInput> {
    let mpath = dir.join("manifest.json");
    let manifest = read_json(&mpath)?;
    ensure!(manifest["schema"] == RAW_HEADS_SCHEMA && manifest["complete"] == true &&
            manifest["final_test_opened"] == false && manifest["calibrated"] == false &&
            manifest["precision"] == "fp32", "{}: complete FP32 raw heads required", dir.display());
    let raw = dir.join("raw.jsonl");
    ensure!(mining::sha256_file(&raw)? == str_of(&manifest, "raw_sha256")?, "{}: raw.jsonl changed", dir.display());
    let roster = manifest["ai_profiles"].as_array().context("ai_profiles")?.len();
    ensure!(roster >= 2, "AI roster");
    let mut rows = Vec::new();
    mining::for_each_jsonl(&raw, |_, v| {
        rows.push(raw_row(&v, roster)?);
        Ok(())
    })?;
    ensure!(manifest["rows"].as_u64() == Some(rows.len() as u64), "{}: row count differs", dir.display());
    Ok(RawInput { manifest_sha256: mining::sha256_file(&mpath)?, manifest, rows })
}

/// Same value of `key` in every input manifest.
fn common(manifests: &[&Value], key: &str) -> Result<Value> {
    let first = manifests[0].get(key).cloned().unwrap_or(Value::Null);
    ensure!(!first.is_null(), "{key} missing");
    for m in &manifests[1..] {
        ensure!(m.get(key) == Some(&first), "inputs differ in {key}");
    }
    Ok(first)
}

/// Distinct teachers: every input names another fit.
fn distinct_fits(manifests: &[&Value]) -> Result<()> {
    let mut seen = HashSet::new();
    for m in manifests {
        ensure!(seen.insert(str_of(m, "fit_sha256")?.to_owned()), "a fit is given twice");
    }
    Ok(())
}

/// (header, rows) of the targets file from raw heads of the same Train block.
pub fn distill_targets(inputs: &[RawInput]) -> Result<(Value, Vec<Value>)> {
    ensure!(!inputs.is_empty(), "at least one teacher");
    let manifests: Vec<&Value> = inputs.iter().map(|i| &i.manifest).collect();
    ensure!(manifests[0]["partition"] == "train", "teachers' raw heads must cover the Train block");
    let partition = common(&manifests, "partition")?;
    let bundle = common(&manifests, "bundle_sha256")?;
    let contract = common(&manifests, "contract_sha256")?;
    let profiles = common(&manifests, "ai_profiles")?;
    distinct_fits(&manifests)?;
    let n = inputs.len();
    let mut index: Vec<HashMap<&str, &RawRow>> = Vec::with_capacity(n);
    for (t, input) in inputs.iter().enumerate() {
        let mut map = HashMap::with_capacity(input.rows.len());
        for r in &input.rows {
            ensure!(map.insert(r.id.as_str(), r).is_none(), "teacher {t}: duplicate id {}", r.id);
        }
        ensure!(map.len() == inputs[0].rows.len(), "teacher {t}: row count differs from teacher 0");
        index.push(map);
    }
    let mut rows = Vec::with_capacity(inputs[0].rows.len());
    for first in &inputs[0].rows {
        let group: Vec<&RawRow> = index.iter().enumerate()
            .map(|(t, m)| m.get(first.id.as_str()).copied().with_context(|| format!("teacher {t} lacks {}", first.id)))
            .collect::<Result<_>>()?;
        for r in &group {
            ensure!(r.text_sha256 == first.text_sha256, "row {}: text digests differ between teachers", first.id);
        }
        let origin: Vec<f64> = (0..3).map(|k| mean(group.iter().map(|r| r.origin[k]), n)).collect();
        let ai: Vec<f64> = (0..first.ai.len()).map(|k| mean(group.iter().map(|r| r.ai[k]), n)).collect();
        rows.push(json!({"id": first.id, "text_sha256": first.text_sha256, "origin": origin,
                         "share": mean(group.iter().map(|r| r.share), n),
                         "conditional_mixed_share": mean(group.iter().map(|r| r.q), n), "ai": ai}));
    }
    let teachers: Vec<Value> = inputs.iter().map(|i| json!({
        "manifest_sha256": i.manifest_sha256, "raw_sha256": i.manifest["raw_sha256"],
        "fit_sha256": i.manifest["fit_sha256"], "selected_checkpoint_sha256": i.manifest["selected_checkpoint_sha256"],
        "arm": i.manifest["arm"], "seed": i.manifest["seed"], "rows": i.rows.len()})).collect();
    let header = json!({"schema": TARGETS_SCHEMA, "partition": partition, "rows": rows.len(), "ai_profiles": profiles,
                        "bundle_sha256": bundle, "contract_sha256": contract, "teachers": teachers,
                        "rule": TARGETS_RULE, "implementation": concat!("grammar-a-baseline ", env!("CARGO_PKG_VERSION"),
                                                                        " seed_mean")});
    Ok((header, rows))
}

/// Read the teachers' raw-head directories and write the targets file `out` (new). Returns the header.
pub fn write_distill_targets(dirs: &[&Path], out: &Path) -> Result<Value> {
    let inputs = dirs.iter().map(|d| read_raw_dir(d)).collect::<Result<Vec<_>>>()?;
    let (header, rows) = distill_targets(&inputs)?;
    let mut bytes = serde_json::to_vec(&header)?;
    bytes.push(b'\n');
    for r in &rows {
        serde_json::to_writer(&mut bytes, r)?;
        bytes.push(b'\n');
    }
    write_new(out, &bytes)?;
    Ok(json!({"targets_sha256": mining::sha256_hex(&bytes), "rows": rows.len(), "teachers": inputs.len()}))
}

// ------------------------------------------------------------------ mining mean

#[derive(Clone, Debug)]
pub struct ScoreRow {
    pub id: String,
    pub text_sha256: String,
    pub source_group: String,
    pub domain: Value,
    pub tokens: Value,
    pub p_any_ai: f64,
}

#[derive(Debug)]
pub struct ScoreInput {
    pub manifest: Value,
    pub binding: Value,
    pub manifest_sha256: String,
    pub rows: Vec<ScoreRow>,
}

/// One `mine_full_a.py` score directory, checked as the mining selection checks it.
pub fn read_scores_dir(dir: &Path) -> Result<ScoreInput> {
    let mpath = dir.join("manifest.json");
    let manifest = read_json(&mpath)?;
    ensure!(manifest["schema"] == SCORE_SCHEMA && manifest["complete"] == true &&
            manifest["pool_split"] == mining::POOL_SPLIT && manifest["final_test_opened"] == false,
            "{}: complete Train-mining score file required", dir.display());
    let bpath = dir.join("binding.json");
    let binding = read_json(&bpath)?;
    ensure!(str_of(&manifest, "binding_sha256")? == mining::sha256_file(&bpath)? && manifest["binding"] == binding &&
            binding["fit_sha256"] == manifest["fit_sha256"] && binding["pool_sha256"] == manifest["pool_sha256"],
            "{}: score binding differs", dir.display());
    let spath = dir.join("scores.jsonl");
    ensure!(mining::sha256_file(&spath)? == str_of(&manifest, "scores_sha256")?, "{}: scores changed", dir.display());
    let mut rows = Vec::new();
    mining::for_each_jsonl(&spath, |_, v| {
        let id = str_of(&v, "id")?.to_owned();
        rows.push(ScoreRow { text_sha256: str_of(&v, "text_sha256")?.to_owned(),
                             source_group: str_of(&v, "source_group")?.to_owned(),
                             domain: v.get("domain").cloned().unwrap_or(Value::Null),
                             tokens: v.get("tokens").cloned().unwrap_or(Value::Null),
                             p_any_ai: probability(&v["p_any_ai"], &format!("score {id}"))?, id });
        Ok(())
    })?;
    Ok(ScoreInput { manifest_sha256: mining::sha256_file(&mpath)?, manifest, binding, rows })
}

/// (binding, manifest without its file digests, rows) of the mean score file.
pub fn mining_mean(inputs: &[ScoreInput]) -> Result<(Value, Vec<Value>)> {
    ensure!(!inputs.is_empty(), "at least one score file");
    let manifests: Vec<&Value> = inputs.iter().map(|i| &i.manifest).collect();
    let bindings: Vec<&Value> = inputs.iter().map(|i| &i.binding).collect();
    let first_binding = bindings[0];
    let bundle = common(&manifests, "bundle_sha256")?;
    let pool = common(&manifests, "pool_sha256")?;
    let precision = common(&manifests, "precision")?;
    for key in ["sample", "padded_token_budget"] {
        let first = bindings[0].get(key).cloned().unwrap_or(Value::Null);
        ensure!(bindings.iter().all(|b| b.get(key).cloned().unwrap_or(Value::Null) == first), "inputs differ in {key}");
    }
    ensure!(bindings.iter().all(|b| b["bundle_sha256"] == bundle), "a score binding names another bundle");
    distinct_fits(&manifests)?;
    let n = inputs.len();
    let mut index: Vec<HashMap<&str, &ScoreRow>> = Vec::with_capacity(n);
    for (t, input) in inputs.iter().enumerate() {
        let mut map = HashMap::with_capacity(input.rows.len());
        for r in &input.rows {
            ensure!(map.insert(r.id.as_str(), r).is_none(), "score file {t}: duplicate id {}", r.id);
        }
        ensure!(map.len() == inputs[0].rows.len(), "score file {t}: row count differs from file 0");
        index.push(map);
    }
    let mut rows = Vec::with_capacity(inputs[0].rows.len());
    for first in &inputs[0].rows {
        let group: Vec<&ScoreRow> = index.iter().enumerate()
            .map(|(t, m)| m.get(first.id.as_str()).copied().with_context(|| format!("score file {t} lacks {}", first.id)))
            .collect::<Result<_>>()?;
        for r in &group {
            if r.text_sha256 != first.text_sha256 || r.source_group != first.source_group || r.domain != first.domain ||
               r.tokens != first.tokens {
                bail!("score {}: row facts differ between score files", first.id);
            }
        }
        rows.push(json!({"id": first.id, "text_sha256": first.text_sha256, "source_group": first.source_group,
                         "domain": first.domain, "tokens": first.tokens,
                         "p_any_ai": mean(group.iter().map(|r| r.p_any_ai), n)}));
    }
    let list = |key: &str| Value::Array(manifests.iter().map(|m| m[key].clone()).collect());
    let binding = json!({"schema": MEAN_BINDING_SCHEMA, "bundle_sha256": bundle, "pool_sha256": pool,
                         "fit_sha256": list("fit_sha256"), "sample": first_binding["sample"],
                         "padded_token_budget": first_binding["padded_token_budget"], "precision": precision,
                         "inputs": inputs.iter().map(|i| json!({
                             "manifest_sha256": i.manifest_sha256, "binding_sha256": i.manifest["binding_sha256"],
                             "scores_sha256": i.manifest["scores_sha256"], "fit_sha256": i.manifest["fit_sha256"],
                             "selected_checkpoint_sha256": i.manifest["selected_checkpoint_sha256"],
                             "seed": i.manifest["seed"], "rows": i.rows.len()})).collect::<Vec<_>>(),
                         "rule": MINING_MEAN_RULE,
                         "implementation": concat!("grammar-a-baseline ", env!("CARGO_PKG_VERSION"), " seed_mean")});
    Ok((binding, rows))
}

/// Read the score directories and write the mean score directory `out` (new). Returns its manifest.
pub fn write_mining_mean(dirs: &[&Path], out: &Path) -> Result<Value> {
    ensure!(!out.exists(), "{} exists; outputs are written once", out.display());
    let inputs = dirs.iter().map(|d| read_scores_dir(d)).collect::<Result<Vec<_>>>()?;
    let (binding, rows) = mining_mean(&inputs)?;
    let manifests: Vec<&Value> = inputs.iter().map(|i| &i.manifest).collect();
    let mut scores = Vec::new();
    for r in &rows {
        serde_json::to_writer(&mut scores, r)?;
        scores.push(b'\n');
    }
    let binding_bytes = serde_json::to_vec_pretty(&binding)?;
    let list = |key: &str| Value::Array(manifests.iter().map(|m| m[key].clone()).collect());
    let manifest = json!({"schema": SCORE_SCHEMA, "complete": true, "binding": binding,
                          "binding_sha256": mining::sha256_hex(&binding_bytes), "pool_split": mining::POOL_SPLIT,
                          "pool": manifests[0]["pool"], "pool_sha256": binding["pool_sha256"],
                          "fit_sha256": binding["fit_sha256"], "selected_checkpoint_sha256": list("selected_checkpoint_sha256"),
                          "arm": list("arm"), "seed": list("seed"), "bundle_sha256": binding["bundle_sha256"],
                          "scores_sha256": mining::sha256_hex(&scores), "rows": rows.len(), "mean_of": inputs.len(),
                          "precision": binding["precision"],
                          "score": "mean over the input fits of their uncalibrated P(model_only)+P(mixed)",
                          "rule": MINING_MEAN_RULE, "final_test_opened": false,
                          "calibration_or_development_scored": false});
    let tmp = out.with_extension("tmp-seed-mean");
    ensure!(!tmp.exists(), "{} exists", tmp.display());
    fs::create_dir(&tmp)?;
    write_new(&tmp.join("scores.jsonl"), &scores)?;
    write_new(&tmp.join("binding.json"), &binding_bytes)?;
    write_new(&tmp.join("manifest.json"), &serde_json::to_vec_pretty(&manifest)?)?;
    fs::rename(&tmp, out)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (id, text digest, origin, share, q, AI logits) of one raw-heads row.
    type Row = (&'static str, &'static str, [f64; 3], f64, f64, [f64; 3]);

    fn raw_dir(dir: &Path, fit: &str, rows: &[Row], partition: &str) {
        fs::create_dir_all(dir).unwrap();
        let mut body = Vec::new();
        for (id, text, o, share, q, z) in rows {
            serde_json::to_writer(&mut body, &json!({"id": id, "text_sha256": text,
                "uncalibrated_origin": {"human_only": o[0], "model_only": o[1], "mixed": o[2]},
                "uncalibrated_share": share, "conditional_mixed_share": q, "ai_logits": z, "calibrated": false}))
                .unwrap();
            body.push(b'\n');
        }
        fs::write(dir.join("raw.jsonl"), &body).unwrap();
        let m = json!({"schema": RAW_HEADS_SCHEMA, "complete": true, "partition": partition, "rows": rows.len(),
                       "ai_profiles": ["gen-a", "gen-b", "gen-c"], "bundle_sha256": {"rows.jsonl": "b"},
                       "contract_sha256": "c", "fit_sha256": fit, "selected_checkpoint_sha256": format!("s-{fit}"),
                       "raw_sha256": mining::sha256_hex(&body), "final_test_opened": false, "calibrated": false,
                       "precision": "fp32", "arm": "E1aux", "seed": 17});
        fs::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    }

    const A: [Row; 2] =
        [("r1", "t1", [0.5, 0.25, 0.25], 0.4, 0.6, [0.0, 0.0, 0.0]), ("r2", "t2", [0.1, 0.8, 0.1], 0.85, 0.5, [1.0, 2.0, 3.0])];
    const B: [Row; 2] =
        [("r2", "t2", [0.3, 0.6, 0.1], 0.65, 0.3, [3.0, 2.0, 1.0]), ("r1", "t1", [0.25, 0.5, 0.25], 0.6, 0.2, [0.0, 0.0, 0.0])];

    #[test]
    fn targets_are_probability_means_in_the_first_input_order() {
        let d = tempfile::tempdir().unwrap();
        raw_dir(&d.path().join("a"), "fa", &A, "train");
        raw_dir(&d.path().join("b"), "fb", &B, "train");
        let out = d.path().join("targets.jsonl");
        let s = write_distill_targets(&[&d.path().join("a"), &d.path().join("b")], &out).unwrap();
        assert_eq!(s["rows"], 2);
        let lines: Vec<Value> = fs::read_to_string(&out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let h = &lines[0];
        assert_eq!(h["schema"], TARGETS_SCHEMA);
        assert_eq!(h["partition"], "train");
        assert_eq!(h["rows"], 2);
        assert_eq!(h["teachers"].as_array().unwrap().len(), 2);
        assert_eq!(h["teachers"][1]["fit_sha256"], "fb");
        assert_eq!(h["teachers"][0]["manifest_sha256"],
                   mining::sha256_file(&d.path().join("a/manifest.json")).unwrap());
        assert_eq!(s["targets_sha256"], mining::sha256_file(&out).unwrap());
        let r1 = &lines[1];
        assert_eq!(r1["id"], "r1");
        assert_eq!(r1["origin"], json!([0.375, 0.375, 0.25]));
        assert_eq!(r1["share"], 0.5);
        assert_eq!(r1["conditional_mixed_share"], 0.4);
        let third = 1.0 / 3.0;
        assert_eq!(r1["ai"], json!([third, third, third]));
        let r2 = &lines[2];
        assert_eq!(r2["id"], "r2");
        let (p, q) = (softmax(&[1.0, 2.0, 3.0]), softmax(&[3.0, 2.0, 1.0]));
        let ai: Vec<f64> = r2["ai"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        for k in 0..3 {
            assert_eq!(ai[k], (p[k] + q[k]) / 2.0);
        }
        assert!((ai.iter().sum::<f64>() - 1.0).abs() < 1e-15);
        assert_eq!(r2["share"], (0.85 + 0.65) / 2.0);
        // Written once.
        assert!(write_distill_targets(&[&d.path().join("a")], &out).is_err());
    }

    #[test]
    fn targets_refusals() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        raw_dir(&p.join("a"), "fa", &A, "train");
        let refuse = |dirs: &[&Path], what: &str| {
            let e = write_distill_targets(dirs, &p.join(format!("out-{what}.jsonl"))).unwrap_err();
            assert!(format!("{e:#}").contains(what), "{what}: {e:#}");
        };
        // The same teacher twice.
        refuse(&[&p.join("a"), &p.join("a")], "given twice");
        // Row sets differ.
        raw_dir(&p.join("short"), "fs", &A[..1], "train");
        refuse(&[&p.join("a"), &p.join("short")], "row count differs");
        let other = [A[0], ("r3", "t3", [0.1, 0.8, 0.1], 0.85, 0.5, [1.0, 2.0, 3.0])];
        raw_dir(&p.join("other"), "fo", &other, "train");
        refuse(&[&p.join("a"), &p.join("other")], "lacks r2");
        // Text digests differ.
        let digest = [A[0], ("r2", "changed", [0.1, 0.8, 0.1], 0.85, 0.5, [1.0, 2.0, 3.0])];
        raw_dir(&p.join("digest"), "fd", &digest, "train");
        refuse(&[&p.join("a"), &p.join("digest")], "text digests differ");
        // Another partition, a changed raw file, an origin that does not sum to 1, a short logit row.
        raw_dir(&p.join("cal"), "fc", &A, "cal");
        refuse(&[&p.join("cal")], "Train block");
        raw_dir(&p.join("changed"), "fx", &A, "train");
        fs::write(p.join("changed/raw.jsonl"), b"{}\n").unwrap();
        refuse(&[&p.join("changed")], "raw.jsonl changed");
        let bad = [("r1", "t1", [0.5, 0.5, 0.25], 0.4, 0.6, [0.0, 0.0, 0.0])];
        raw_dir(&p.join("sum"), "fu", &bad, "train");
        refuse(&[&p.join("sum")], "origin sum");
        // Inputs naming different bundles.
        raw_dir(&p.join("bundle"), "fb2", &B, "train");
        let mpath = p.join("bundle/manifest.json");
        let mut m: Value = serde_json::from_slice(&fs::read(&mpath).unwrap()).unwrap();
        m["bundle_sha256"] = json!({"rows.jsonl": "other"});
        fs::write(&mpath, serde_json::to_vec(&m).unwrap()).unwrap();
        refuse(&[&p.join("a"), &p.join("bundle")], "bundle_sha256");
    }

    const TOKENS: u64 = 16384;

    fn score_dir(dir: &Path, fit: &str, rows: &[(&str, f64)]) {
        fs::create_dir_all(dir).unwrap();
        let mut body = Vec::new();
        for (id, p) in rows {
            serde_json::to_writer(&mut body, &json!({"id": id, "text_sha256": format!("t-{id}"),
                "source_group": format!("g-{id}"), "domain": "web", "tokens": 300, "p_any_ai": p})).unwrap();
            body.push(b'\n');
        }
        fs::write(dir.join("scores.jsonl"), &body).unwrap();
        let binding = json!({"schema": "slop_ninja_full_a_mining_binding_v1", "fit_sha256": fit,
                             "selected_checkpoint_sha256": format!("s-{fit}"), "bundle_sha256": {"rows.jsonl": "b"},
                             "pool_sha256": "pool", "sample": {"seed": "x", "keep": 1, "modulus": 2},
                             "precision": "fp32", "padded_token_budget": TOKENS, "implementation_sha256": "i"});
        let bb = serde_json::to_vec(&binding).unwrap();
        fs::write(dir.join("binding.json"), &bb).unwrap();
        let m = json!({"schema": SCORE_SCHEMA, "complete": true, "binding": binding,
                       "binding_sha256": mining::sha256_hex(&bb), "pool": "pool.jsonl", "pool_sha256": "pool",
                       "pool_split": mining::POOL_SPLIT, "fit_sha256": fit, "selected_checkpoint_sha256": format!("s-{fit}"),
                       "arm": "E1aux", "seed": 17, "bundle_sha256": {"rows.jsonl": "b"},
                       "scores_sha256": mining::sha256_hex(&body), "precision": "fp32", "final_test_opened": false});
        fs::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    }

    #[test]
    fn mining_mean_writes_a_v2_score_file_the_selection_accepts() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        score_dir(&p.join("s17"), "f17", &[("a", 0.9), ("b", 0.2)]);
        score_dir(&p.join("s29"), "f29", &[("b", 0.4), ("a", 0.5)]);
        score_dir(&p.join("s41"), "f41", &[("a", 0.7), ("b", 0.3)]);
        let out = p.join("mean");
        let m = write_mining_mean(&[&p.join("s17"), &p.join("s29"), &p.join("s41")], &out).unwrap();
        // The checks `mine_full_a select` applies to a score directory (bundle digests aside).
        let back = read_json(&out.join("manifest.json")).unwrap();
        assert_eq!(back, m);
        assert!(back["schema"] == SCORE_SCHEMA && back["complete"] == true && back["pool_split"] == mining::POOL_SPLIT &&
                back["final_test_opened"] == false);
        let binding = &back["binding"];
        assert_eq!(binding["fit_sha256"], back["fit_sha256"]);
        assert_eq!(binding["fit_sha256"], json!(["f17", "f29", "f41"]));
        assert_eq!(binding["pool_sha256"], back["pool_sha256"]);
        assert_eq!(back["binding_sha256"], mining::sha256_file(&out.join("binding.json")).unwrap());
        assert_eq!(read_json(&out.join("binding.json")).unwrap(), *binding);
        assert_eq!(back["scores_sha256"], mining::sha256_file(&out.join("scores.jsonl")).unwrap());
        let mut scores = Vec::new();
        mining::for_each_jsonl(&out.join("scores.jsonl"), |_, v| {
            scores.push(mining::Score::from_json(&v)?);
            Ok(())
        }).unwrap();
        assert_eq!(scores.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(scores[0].p_any_ai, (0.9 + 0.5 + 0.7) / 3.0);
        assert_eq!(scores[1].p_any_ai, (0.2 + 0.4 + 0.3) / 3.0);
        // The mean of the means file is itself a valid input.
        assert_eq!(read_scores_dir(&out).unwrap().rows.len(), 2);
        assert!(write_mining_mean(&[&p.join("s17")], &out).is_err());
    }

    #[test]
    fn mining_mean_refusals() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        score_dir(&p.join("s17"), "f17", &[("a", 0.9), ("b", 0.2)]);
        let refuse = |dirs: &[&Path], what: &str| {
            let e = write_mining_mean(dirs, &p.join(format!("out-{}", what.replace(' ', "-")))).unwrap_err();
            assert!(format!("{e:#}").contains(what), "{what}: {e:#}");
        };
        refuse(&[&p.join("s17"), &p.join("s17")], "given twice");
        score_dir(&p.join("s29"), "f29", &[("a", 0.9)]);
        refuse(&[&p.join("s17"), &p.join("s29")], "row count differs");
        score_dir(&p.join("s41"), "f41", &[("a", 0.9), ("c", 0.2)]);
        refuse(&[&p.join("s17"), &p.join("s41")], "lacks b");
        score_dir(&p.join("pool"), "fp", &[("a", 0.9), ("b", 0.2)]);
        let mpath = p.join("pool/manifest.json");
        let mut m: Value = serde_json::from_slice(&fs::read(&mpath).unwrap()).unwrap();
        m["pool_sha256"] = json!("other");
        fs::write(&mpath, serde_json::to_vec(&m).unwrap()).unwrap();
        refuse(&[&p.join("pool")], "score binding differs");
        score_dir(&p.join("edit"), "fe", &[("a", 0.9), ("b", 0.2)]);
        fs::write(p.join("edit/scores.jsonl"), b"{}\n").unwrap();
        refuse(&[&p.join("edit")], "scores changed");
    }
}
