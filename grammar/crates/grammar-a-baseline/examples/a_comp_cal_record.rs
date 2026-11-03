//! Per-arm, per-seed CAL calibration record for the Tier C seal.
//!
//! a_comp_cal_record EVAL_BUNDLE CALIBRATION.json CALIBRATED.jsonl RAW_CAL_RUN EVALUATION.json SYSTEM SEED OUT.json
//!
//! EVAL_BUNDLE is the CAL evaluation bundle (rows with `population` and
//! `origin`), CALIBRATION.json the map written by `calibrate_full_a` or
//! `calibrate_fixed_a`, CALIBRATED.jsonl its calibrated CAL predictions,
//! RAW_CAL_RUN the raw run it was fitted on, EVALUATION.json the CAL run of
//! `a_comp_evaluate` (operating points read the emitted involvement).
//!
//! The 1% and 0.1% CAL operating points are recomputed with the protocol rule
//! (`comp_eval::operating_threshold`) over the primary CAL documents twice:
//! from `1 - p_human` of the calibrated rows (the evaluator's reading) and from
//! the involvement the map emits for each raw row (`Map::apply_origin`, the
//! value the runtime returns as `attribution.ai.involvement_probability`). Both
//! must equal the evaluator's threshold bit for bit; the record stores the
//! thresholds and named-profile thresholds as f64 bits. CAL values are
//! in-sample fitted diagnostics: they are never compared between arms.
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::comp_eval::{ALPHAS, operating_threshold};
use grammar_a_baseline::involvement_calibration::Map;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}
fn jsonl(p: &Path) -> Result<Vec<Value>> {
    BufReader::new(fs::File::open(p).with_context(|| p.display().to_string())?)
        .lines()
        .filter(|l| l.as_ref().map_or(true, |l| !l.trim().is_empty()))
        .map(|l| Ok(serde_json::from_str(&l?)?))
        .collect()
}
fn bits(t: f64) -> Value {
    json!({"value": if t.is_finite() { json!(t) } else { json!("inf") }, "bits": format!("0x{:016x}", t.to_bits())})
}

fn run(a: &[PathBuf]) -> Result<()> {
    let [bundle, cal_path, calibrated, raw_dir, eval_path, system, seed, out] = a else {
        anyhow::bail!("EVAL_BUNDLE CALIBRATION.json CALIBRATED.jsonl RAW_CAL_RUN EVALUATION.json SYSTEM SEED OUT.json")
    };
    ensure!(!out.exists(), "new output required");
    let (system, seed) = (system.to_str().context("system")?, seed.to_str().context("seed")?);
    let cal = read(cal_path)?;
    ensure!(cal["complete"] == true && cal["fit_split"] == "cal" && cal["final_test_opened"] == false, "CAL calibration");
    let map = Map::from_description(&cal["involvement"]["selected"])?;
    let eval = read(eval_path)?;
    ensure!(eval["split"] == "cal", "CAL evaluation required");
    let s = &eval["systems"][system]["seeds"][seed];
    ensure!(s.is_object(), "evaluation has no {system}:{seed}");
    let pred_file = &eval["inputs"]["files"][format!("pred:{system}:{seed}")];
    ensure!(pred_file["sha256"].as_str() == Some(sha(calibrated)?.as_str()), "evaluation scored other predictions");

    // Primary CAL documents of the evaluation bundle.
    let mut primary = BTreeMap::<String, bool>::new();
    for r in jsonl(&bundle.join("rows.jsonl"))? {
        ensure!(r["split"] == "cal", "CAL bundle rows only");
        if r["population"] == "primary" {
            primary.insert(r["id"].as_str().context("id")?.to_owned(), r["origin"] == "human_only");
        }
    }
    let mut flag = BTreeMap::new();
    for p in jsonl(calibrated)? {
        flag.insert(p["id"].as_str().context("id")?.to_owned(), (1.0 - p["p_human"].as_f64().context("p_human")?).clamp(0.0, 1.0));
    }
    let mut emitted = BTreeMap::new();
    for r in jsonl(&raw_dir.join("raw.jsonl"))? {
        let l = r["origin_logits"].as_array().context("origin logits")?.iter()
            .map(|v| v.as_f64().context("logit")).collect::<Result<Vec<_>>>()?;
        let (inv, _) = map.apply_origin(&l, r["involvement_logit"].as_f64().context("involvement logit")?)?;
        emitted.insert(r["id"].as_str().context("id")?.to_owned(), inv);
    }
    let collect = |m: &BTreeMap<String, f64>| -> Result<(Vec<f64>, Vec<f64>)> {
        let mut all = Vec::new();
        let mut hum = Vec::new();
        for (id, human) in &primary {
            let v = *m.get(id).with_context(|| format!("no value for primary CAL document {id}"))?;
            all.push(v);
            if *human {
                hum.push(v);
            }
        }
        Ok((all, hum))
    };
    let (fa, fh) = collect(&flag)?;
    let (ea, eh) = collect(&emitted)?;
    let mut points = serde_json::Map::new();
    for alpha in ALPHAS {
        let t_flag = operating_threshold(&fa, &fh, alpha);
        let t_emit = operating_threshold(&ea, &eh, alpha);
        let ev = &s["operating_points"][alpha.label];
        let ev_bits = ev["threshold"]["bits"].as_str().context("evaluator threshold bits")?;
        let mine = format!("0x{:016x}", t_flag.to_bits());
        ensure!(t_flag.to_bits() == t_emit.to_bits(), "{}: 1-p_human threshold {t_flag} differs from emitted {t_emit}", alpha.label);
        ensure!(mine == ev_bits, "{}: threshold {mine} differs from the evaluator's {ev_bits}", alpha.label);
        let flagged = |v: &[f64]| v.iter().filter(|x| **x >= t_flag).count();
        points.insert(alpha.label.into(), json!({"threshold": bits(t_flag),
            "cal_humans_flagged": [flagged(&fh), fh.len()],
            "cal_positives_flagged": [flagged(&fa) - flagged(&fh), fa.len() - fh.len()],
            "evaluator_human_fpr": ev["human_fpr"], "evaluator_tpr": ev["tpr"],
            "checks": {"emitted_involvement_bits_equal": true, "evaluator_bits_equal": true}}));
    }
    let named = |role: &str| -> Result<Value> {
        let r = &cal["roles"][role];
        let t = r["threshold"].as_f64().context("named threshold")?;
        Ok(json!({"qualified": r["qualified"], "threshold": bits(t),
                  "roster_empty": r["roster_empty"], "rule": r.get("rule").cloned().unwrap_or(r["objective"].clone())}))
    };
    let report = &cal["involvement"]["report"];
    let record = json!({"schema": "slop_ninja_a_comp_cal_record_v1", "system": system, "seed": seed.parse::<u64>()?,
        "fit_split": "cal", "final_test_opened": false,
        "calibration": {"path": cal_path, "sha256": sha(cal_path)?, "method": map.method(),
            "origin_temperature": report["origin_temperature"],
            "origin_temperature_bits": report["origin_temperature"].as_f64().map(|t| format!("0x{:016x}", t.to_bits())),
            "clip": report["clip"]},
        "calibrated_predictions": {"path": calibrated, "sha256": sha(calibrated)?},
        "raw_cal_run": {"path": raw_dir, "manifest_sha256": sha(&raw_dir.join("manifest.json"))?,
                        "raw_sha256": sha(&raw_dir.join("raw.jsonl"))?},
        "evaluation": {"path": eval_path, "sha256": sha(eval_path)?},
        "operating_points": points,
        "operating_point_rule": "smallest distinct calibrated involvement of primary CAL documents (or +inf) with at most alpha of CAL humans >= it; flag = value >= t in f64",
        "named_profile_thresholds": {"ai": named("ai")?, "human": named("human")?},
        "cal_reliability_in_sample": {"ece10": s["ece10"], "ece10_humans": s["ece10_humans"],
            "calibration_bins": s["calibration_bins"], "balanced_involvement_brier": s["balanced_involvement_brier"],
            "human_fpr_at_0_5": s["human_fpr_at_0_5"], "documents": s["documents"],
            "isotonic_clipped_in_sample_CAL_brier": report["isotonic_clipped_in_sample_CAL_brier"],
            "isotonic_pav_levels": report["isotonic_pav_levels"]},
        "limits": ["CAL values are in-sample fitted diagnostics; never compare arms on them.",
                   "Thresholds apply to calibrated Test involvement in f64 (value >= t)."]});
    let mut f = fs::OpenOptions::new().create_new(true).write(true).open(out)?;
    serde_json::to_writer_pretty(&mut f, &record)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!("{}", json!({"system": system, "seed": seed, "points": record["operating_points"].as_object().unwrap()
        .iter().map(|(k, v)| (k.clone(), v["threshold"]["bits"].clone())).collect::<BTreeMap<_, _>>()}));
    Ok(())
}

fn main() -> Result<()> {
    run(&std::env::args_os().skip(1).map(PathBuf::from).collect::<Vec<_>>())
}
