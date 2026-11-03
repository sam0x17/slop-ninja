//! Apply one fit's frozen CAL calibration to its raw predictions of a scored
//! split (Test after the seal; CAL only as the mechanical identity check).
//!
//! ```text
//! a_comp_test_calibrate --calibration CALIBRATION.json --record RECORD.json \
//!   --raw RAW_DIR --rows SCORING_ROWS_DIR --share-mode head|token_mean \
//!   --roster ROSTER.json --out PREDICTIONS.jsonl
//! ```
//!
//! - CALIBRATION.json is the fit's CAL calibration (`calibrate_full_a` or
//!   `calibrate_fixed_a`), RECORD.json its Tier C CAL record, which must name
//!   that calibration by sha256 and carry the CAL operating points.
//! - RAW_DIR is a raw run (`raw.jsonl`, `manifest.json`, optional
//!   `failures.jsonl`) over the rows of SCORING_ROWS_DIR/rows.jsonl: every row
//!   appears once, in row order, either as a raw row with its exact
//!   `text_sha256` or as an execution failure.
//! - Each raw row becomes the calibrated shared-contract prediction of
//!   `calibrate_full_a` (`Map::apply_origin` on the origin logits, the arm's
//!   share rule clamped to [p_model_only, involvement], softmax attribution
//!   over the roster): the same projection, so a CAL run reproduces the fit's
//!   `cal-calibrated.jsonl` byte for byte.
//! - An execution failure (input over 2,048 encoder tokens, non-finite output,
//!   crash) is written `{"id", "status": "failed", "failure"}`: the evaluator
//!   keeps it in every denominator as a failure.
//!
//! Reads no text field and calls no provider.
use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use grammar_a_baseline::involvement_calibration::Map;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(about = "Apply a fit's frozen CAL calibration to a raw run (no network)")]
struct Args {
    #[arg(long)]
    calibration: PathBuf,
    #[arg(long)]
    record: PathBuf,
    #[arg(long)]
    raw: PathBuf,
    #[arg(long)]
    rows: PathBuf,
    #[arg(long)]
    share_mode: String,
    /// JSON with `ai_profiles` (the fit's contract or the comparator record).
    #[arg(long)]
    roster: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

fn read(p: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(p).with_context(|| p.display().to_string())?).with_context(|| p.display().to_string())
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p).with_context(|| p.display().to_string())?)))
}
fn lines(p: &Path) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    if !p.exists() {
        return Ok(out);
    }
    for (i, l) in BufReader::new(fs::File::open(p)?).lines().enumerate() {
        let l = l?;
        if !l.trim().is_empty() {
            out.push(serde_json::from_str(&l).with_context(|| format!("{} line {}", p.display(), i + 1))?);
        }
    }
    Ok(out)
}

// Same as calibrate_full_a / calibrate_fixed_a.
fn softmax(logits: &Value, n: usize) -> Result<Vec<f64>> {
    let v = logits
        .as_array()
        .context("logits")?
        .iter()
        .map(|x| x.as_f64().filter(|x| x.is_finite()).context("finite logit"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(v.len() == n && n > 1, "logit dimension");
    let m = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let e = v.iter().map(|x| (x - m).exp()).collect::<Vec<_>>();
    let s = e.iter().sum::<f64>();
    Ok(e.into_iter().map(|x| x / s).collect())
}

// Same projection as calibrate_full_a / calibrate_fixed_a.
fn calibrated_row(id: &str, p: &Value, map: &Map, share_mode: &str, ai: &[String]) -> Result<Value> {
    let logits = p["origin_logits"].as_array().context("origin logits")?.iter()
        .map(|v| v.as_f64().context("origin logit")).collect::<Result<Vec<_>>>()?;
    let g = p["involvement_logit"].as_f64().context("logit")?;
    ensure!(logits.len() == 3, "three origin logits");
    let (inv, r) = map.apply_origin(&logits, g)?;
    let origin = [1. - inv, inv * r, inv * (1. - r)];
    let share = match share_mode {
        "head" => origin[1] + origin[2] * p["conditional_mixed_share"].as_f64().context("mixed share")?,
        _ => p["token_mean"].as_f64().context("token mean")?,
    }
    .clamp(origin[1], inv);
    let scores = softmax(&p["ai_logits"], ai.len())?;
    let attribution: serde_json::Map<String, Value> = ai.iter().cloned().zip(scores.into_iter().map(Value::from)).collect();
    Ok(json!({"id":id,"p_human":origin[0],"p_model_only":origin[1],"p_mixed":origin[2],"share":share,
        "attribution":attribution,"attribution_basis":"conditional_on_ai_involvement","calibrated":true}))
}

fn finite(v: &Value) -> bool {
    match v {
        Value::Number(n) => n.as_f64().is_some_and(f64::is_finite),
        Value::Array(a) => a.iter().all(finite),
        Value::Object(m) => m.values().all(finite),
        _ => true,
    }
}

fn run(a: &Args) -> Result<Value> {
    ensure!(!a.out.exists(), "{} exists: predictions are written once", a.out.display());
    ensure!(matches!(a.share_mode.as_str(), "head" | "token_mean"), "share mode head or token_mean");
    let cal = read(&a.calibration)?;
    ensure!(cal["complete"] == true && cal["fit_split"] == "cal" && cal["final_test_opened"] == false,
        "a complete CAL calibration is required");
    let rec = read(&a.record)?;
    ensure!(rec["schema"].as_str().is_some_and(|s| s.starts_with("slop_ninja_a_comp_")) && rec["fit_split"] == "cal",
        "Tier C CAL record required");
    let cal_sha = sha(&a.calibration)?;
    ensure!(rec["calibration"]["sha256"] == cal_sha.as_str(), "the record names another calibration");
    let map = Map::from_description(&cal["involvement"]["selected"])?;
    let ai: Vec<String> = read(&a.roster)?["ai_profiles"].as_array().context("ai_profiles")?.iter()
        .map(|v| Ok(v.as_str().context("profile")?.to_owned())).collect::<Result<_>>()?;
    let rows_p = a.rows.join("rows.jsonl");
    let rows = lines(&rows_p)?;
    ensure!(!rows.is_empty(), "no scoring rows");
    let split = rows[0]["split"].as_str().context("split")?.to_string();
    ensure!(matches!(split.as_str(), "test" | "cal"), "only Test (or CAL for the identity check) is calibrated here");
    ensure!(rows.iter().all(|r| r["split"] == split.as_str()), "scoring rows of one split required");
    let manifest = read(&a.raw.join("manifest.json"))?;
    ensure!(manifest["complete"] == true && manifest["partition"] == split.as_str() && manifest["calibrated"] == false,
        "{} is not a complete uncalibrated {split} raw run", a.raw.display());
    let raw_p = a.raw.join("raw.jsonl");
    ensure!(manifest["raw_sha256"].as_str() == Some(sha(&raw_p)?.as_str()), "raw.jsonl differs from its manifest");
    if let Some(s) = manifest["scoring_rows_sha256"].as_str() {
        ensure!(s == sha(&rows_p)?, "the raw run scored other rows");
    }
    if let Some(m) = manifest["share_mode"].as_str() {
        ensure!(m == a.share_mode, "share mode differs from the raw run's");
    }
    let fail_p = a.raw.join("failures.jsonl");
    if fail_p.exists() {
        ensure!(manifest["failures_sha256"].as_str() == Some(sha(&fail_p)?.as_str()), "failures.jsonl differs from its manifest");
    } else {
        ensure!(manifest.get("failures_sha256").is_none_or(Value::is_null), "failures.jsonl missing");
    }
    let raw = lines(&raw_p)?;
    let failures: BTreeMap<String, String> = lines(&fail_p)?
        .into_iter()
        .map(|f| Ok((f["id"].as_str().context("failure id")?.to_owned(), f["failure"].as_str().context("failure reason")?.to_owned())))
        .collect::<Result<_>>()?;
    let mut ri = raw.iter().peekable();
    let mut w = std::io::BufWriter::new(fs::OpenOptions::new().create_new(true).write(true).open(&a.out)?);
    let (mut ok, mut failed) = (0usize, BTreeMap::<String, usize>::new());
    let mut seen = BTreeSet::new();
    for r in &rows {
        let id = r["id"].as_str().context("row id")?;
        ensure!(seen.insert(id.to_owned()), "duplicate row {id}");
        let line = match (failures.get(id), ri.peek()) {
            (Some(reason), next) => {
                ensure!(next.is_none_or(|p| p["id"] != id), "{id} is both a failure and a raw row");
                *failed.entry(reason.clone()).or_default() += 1;
                json!({"id": id, "status": "failed", "failure": reason})
            }
            (None, Some(p)) if p["id"] == id => {
                let p = ri.next().unwrap();
                ensure!(p["text_sha256"] == r["text_sha256"], "{id}: raw text digest differs from the row");
                let c = calibrated_row(id, p, &map, &a.share_mode, &ai)?;
                if finite(&c) {
                    ok += 1;
                    c
                } else {
                    *failed.entry("non_finite_calibrated_output".into()).or_default() += 1;
                    json!({"id": id, "status": "failed", "failure": "non_finite_calibrated_output"})
                }
            }
            _ => bail!("{id}: neither a raw row in row order nor a recorded failure"),
        };
        serde_json::to_writer(&mut w, &line)?;
        w.write_all(b"\n")?;
    }
    ensure!(ri.next().is_none(), "raw rows outside the scoring rows");
    ensure!(failures.keys().all(|k| seen.contains(k)), "failures outside the scoring rows");
    w.into_inner()?.sync_all()?;
    Ok(json!({"split": split, "rows": rows.len(), "ok": ok, "failed": failed,
        "calibration_sha256": cal_sha, "record_sha256": sha(&a.record)?, "raw_manifest_sha256": sha(&a.raw.join("manifest.json"))?,
        "scoring_rows_sha256": sha(&rows_p)?, "predictions_sha256": sha(&a.out)?, "method": map.method(),
        "implementation_sha256": hex::encode(Sha256::digest(include_bytes!("a_comp_test_calibrate.rs")))}))
}

fn main() -> Result<()> {
    let a = Args::parse();
    println!("{}", serde_json::to_string(&run(&a)?)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_a_baseline::involvement_calibration::{Map, OriginObservation, Policy, PolicyVersion, select_protocol};

    fn w(p: &Path, v: &Value) {
        fs::write(p, serde_json::to_vec(v).unwrap()).unwrap();
    }
    fn wl(p: &Path, v: &[Value]) {
        let mut s = String::new();
        for x in v {
            s.push_str(&x.to_string());
            s.push('\n');
        }
        fs::write(p, s).unwrap();
    }

    #[test]
    fn applies_the_frozen_map_in_row_order_and_records_failures() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        // A fitted map from synthetic CAL observations.
        let obs: Vec<OriginObservation> = (0..40).map(|i| {
            let g = if i < 20 { -2.0 + 0.05 * i as f64 } else { 1.0 + 0.05 * i as f64 };
            OriginObservation { family: format!("g{}", i / 2), origin: usize::from(i >= 20), logits: [0.0, g, g - 30.0], logit: g }
        }).collect();
        let policy = Policy { version: PolicyVersion::V3, min_human_rows: 0, min_positive_rows: 0, min_human_families: 0, min_positive_families: 0, folds: 2 };
        let (map, _) = select_protocol(&obs, &policy).unwrap();
        let cal = json!({"complete": true, "fit_split": "cal", "final_test_opened": false, "involvement": {"selected": map.description()}});
        w(&d.join("cal.json"), &cal);
        let cal_sha = sha(&d.join("cal.json")).unwrap();
        w(&d.join("record.json"), &json!({"schema": "slop_ninja_a_comp_tierc_cal_record_v1", "fit_split": "cal", "calibration": {"sha256": cal_sha}}));
        w(&d.join("roster.json"), &json!({"ai_profiles": ["a", "b"]}));
        fs::create_dir_all(d.join("rows")).unwrap();
        fs::create_dir_all(d.join("raw")).unwrap();
        let rows: Vec<Value> = (0..4).map(|i| json!({"id": format!("t{i}"), "split": "test", "text_sha256": format!("s{i}")})).collect();
        wl(&d.join("rows/rows.jsonl"), &rows);
        let raw: Vec<Value> = [0, 2, 3].iter().map(|i| json!({"id": format!("t{i}"), "text_sha256": format!("s{i}"),
            "origin_logits": [0.0, 1.0, -29.0], "involvement_logit": 1.0, "conditional_mixed_share": 0.5, "ai_logits": [1.0, 0.0]})).collect();
        wl(&d.join("raw/raw.jsonl"), &raw);
        wl(&d.join("raw/failures.jsonl"), &[json!({"id": "t1", "failure": "input_over_2048_encoder_tokens"})]);
        let man = json!({"complete": true, "partition": "test", "calibrated": false, "share_mode": "head",
            "raw_sha256": sha(&d.join("raw/raw.jsonl")).unwrap(), "failures_sha256": sha(&d.join("raw/failures.jsonl")).unwrap(),
            "scoring_rows_sha256": sha(&d.join("rows/rows.jsonl")).unwrap()});
        w(&d.join("raw/manifest.json"), &man);
        let args = |out: &str| Args { calibration: d.join("cal.json"), record: d.join("record.json"), raw: d.join("raw"),
            rows: d.join("rows"), share_mode: "head".into(), roster: d.join("roster.json"), out: d.join(out) };
        let r = run(&args("p.jsonl")).unwrap();
        assert_eq!((r["ok"].as_u64(), r["failed"]["input_over_2048_encoder_tokens"].as_u64()), (Some(3), Some(1)));
        let out = lines(&d.join("p.jsonl")).unwrap();
        assert_eq!(out.iter().map(|x| x["id"].as_str().unwrap()).collect::<Vec<_>>(), ["t0", "t1", "t2", "t3"]);
        assert_eq!((out[1]["status"].as_str(), out[1]["failure"].as_str()), (Some("failed"), Some("input_over_2048_encoder_tokens")));
        let (inv, _) = Map::from_description(&map.description()).unwrap().apply_origin(&[0.0, 1.0, -29.0], 1.0).unwrap();
        assert!((1.0 - out[0]["p_human"].as_f64().unwrap() - inv).abs() < 1e-15);
        // The evaluator reads the failure line as an execution failure.
        let parsed = grammar_a_baseline::comp_eval::parse_predictions(&fs::read_to_string(d.join("p.jsonl")).unwrap()).unwrap();
        assert!(matches!(parsed["t1"], grammar_a_baseline::comp_eval::Obs::Failed(_)));
        assert!(matches!(parsed["t0"], grammar_a_baseline::comp_eval::Obs::Ok(_)));
        assert!(run(&args("p.jsonl")).is_err(), "written once");
        // A row neither scored nor failed, a changed record or another split is refused.
        wl(&d.join("raw/failures.jsonl"), &[]);
        let mut m2 = man.clone();
        m2["failures_sha256"] = json!(sha(&d.join("raw/failures.jsonl")).unwrap());
        w(&d.join("raw/manifest.json"), &m2);
        assert!(run(&args("q.jsonl")).is_err());
        w(&d.join("raw/manifest.json"), &man);
        wl(&d.join("raw/failures.jsonl"), &[json!({"id": "t1", "failure": "input_over_2048_encoder_tokens"})]);
        w(&d.join("record.json"), &json!({"schema": "slop_ninja_a_comp_tierc_cal_record_v1", "fit_split": "cal", "calibration": {"sha256": "x"}}));
        assert!(run(&args("r.jsonl")).is_err());
    }
}
