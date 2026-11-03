//! Held-out behaviour of the CAL-fitted involvement maps for full-A runs
//! (shared-contract v2 artifacts).
//! BUNDLE POLICY.json RUNS.json NEW_REPORT.json
//!
//! RUNS.json: {"runs": [{"format": "full_a", "arm", "seed", "dev_iter": RAW_DIR, "cal": RAW_DIR}]},
//! each RAW_DIR an `export_full_a.py raw` run on BUNDLE. For each run, fits
//! `involvement_calibration::select` on the raw CAL predictions only, then
//! applies the bounded isotonic map, the legacy unbounded isotonic map and the
//! temperature map to the held-out Dev-iter predictions and to the CAL
//! source-family cross-fit folds. Reports balanced Brier, exact 0/1 outputs,
//! values at the declared bound and human flags at 0.5. Diagnostic only: no
//! map is chosen by these numbers. Only ids, digests, source groups and
//! origins of the bundle rows are read, never text.
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::full_a_v2::{CAL_SPLIT, joined, raw_run, split_rows};
use grammar_a_baseline::involvement_calibration::{
    Map, Observation, Policy, balanced_weights, brier, select,
};
use grammar_a_baseline::mining::bundle_digests;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

const HELD_OUT: &str = "dev_iter";

fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}

/// Observations for one partition, bound to the bundle rows and the raw run.
fn observations(bundle: &Path, raw: &Path, part: &str, arm: &Value, seed: &Value) -> Result<Vec<Observation>> {
    let run = raw_run(raw, part, bundle, None)?;
    ensure!(&run.manifest["arm"] == arm && &run.manifest["seed"] == seed, "raw {part} run is for another fit");
    let rows = split_rows(bundle, part)?;
    let out = joined(&rows, &run)?
        .into_iter()
        .map(|(r, p)| Ok(Observation { family: r.source_group.clone(), origin: r.origin_index()?,
                                       logit: p["involvement_logit"].as_f64().context("logit")? }))
        .collect::<Result<Vec<_>>>()?;
    ensure!(!out.is_empty(), "no origin-labelled rows");
    Ok(out)
}

fn held_out(rows: &[Observation], map: &Map, epsilon: Option<f64>) -> Result<Value> {
    let w = balanced_weights(rows)?;
    let p = rows
        .iter()
        .map(|r| map.apply(r.logit))
        .collect::<Result<Vec<_>>>()?;
    let count = |f: &dyn Fn(&Observation, f64) -> bool| {
        rows.iter().zip(&p).filter(|(r, p)| f(r, **p)).count()
    };
    let humans = rows.iter().filter(|r| r.origin == 0).count();
    Ok(json!({"method":map.method(),
        "balanced_brier":brier(rows, &w, |g| map.apply(g).unwrap()),
        "rows":rows.len(),"human_rows":humans,
        "exact_zero":count(&|_, p| p == 0.),"exact_one":count(&|_, p| p == 1.),
        "humans_mapped_to_exactly_one":count(&|r, p| r.origin == 0 && p == 1.),
        "positives_mapped_to_exactly_zero":count(&|r, p| r.origin != 0 && p == 0.),
        "at_lower_bound":epsilon.map(|e| count(&|_, p| p <= e)),
        "at_upper_bound":epsilon.map(|e| count(&|_, p| p >= 1. - e)),
        "human_flags_at_0_5":count(&|r, p| r.origin == 0 && p >= 0.5),
        "max_human_prediction":rows.iter().zip(&p).filter(|(r, _)| r.origin == 0).map(|(_, p)| *p).fold(f64::NEG_INFINITY, f64::max),
        "min_positive_prediction":rows.iter().zip(&p).filter(|(r, _)| r.origin != 0).map(|(_, p)| *p).fold(f64::INFINITY, f64::min)}))
}

fn run(bundle: &Path, policy_path: &Path, runs_path: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new report required");
    let policy = Policy::from_json(&read(policy_path)?)?;
    let runs = read(runs_path)?;
    let mut reports = Vec::new();
    for r in runs["runs"].as_array().context("runs")? {
        if r["format"] != "full_a" {
            continue;
        }
        let dev = observations(bundle, Path::new(r[HELD_OUT].as_str().context("dev_iter raw")?), HELD_OUT,
                               &r["arm"], &r["seed"])?;
        let cal = observations(bundle, Path::new(r[CAL_SPLIT].as_str().context("cal raw")?), CAL_SPLIT,
                               &r["arm"], &r["seed"])?;
        // Both runs must come from the same selected checkpoint.
        let (a, b) = (read(&Path::new(r[HELD_OUT].as_str().unwrap()).join("manifest.json"))?,
                      read(&Path::new(r[CAL_SPLIT].as_str().unwrap()).join("manifest.json"))?);
        ensure!(a["selected_checkpoint_sha256"] == b["selected_checkpoint_sha256"] && a["fit_sha256"] == b["fit_sha256"],
                "Dev-iter and CAL raw runs come from different checkpoints");
        let (selected, report) = select(&cal, &policy)?;
        let bounded = Map::from_description(&report["isotonic_bounded_candidate"])?;
        let unbounded = Map::from_description(&report["isotonic_candidate"])?;
        let temperature = Map::Temperature(report["temperature"].as_f64().context("T")?);
        let epsilon = report["isotonic_bound_epsilon"].as_f64();
        let identity = Map::Identity;
        reports.push(json!({"arm":r["arm"],"seed":r["seed"],
            "selected_method":selected.method(),"selection_reason":report["selection_reason"],
            "cal_support":report["support"],"isotonic_bound_epsilon":epsilon,
            "isotonic_pav_levels":report["isotonic_pav_levels"],
            "cal_crossfit_brier":{"isotonic_bounded_v2":report["isotonic_bounded_crossfit_CAL_brier"],
                "isotonic_v1":report["isotonic_crossfit_CAL_brier"],
                "involvement_temperature_v1":report["temperature_crossfit_CAL_brier"]},
            "cal_crossfit_folds":report["crossfit_folds"],
            "dev_iter_held_out":{"identity_v1":held_out(&dev, &identity, None)?,
                "isotonic_bounded_v2":held_out(&dev, &bounded, epsilon)?,
                "isotonic_v1":held_out(&dev, &unbounded, None)?,
                "involvement_temperature_v1":held_out(&dev, &temperature, None)?}}));
    }
    ensure!(!reports.is_empty(), "no full-A runs");
    let mut means = BTreeMap::<String, BTreeMap<String, (f64, usize)>>::new();
    for r in &reports {
        let arm = r["arm"].as_str().unwrap_or("?").to_owned();
        for (method, v) in r["dev_iter_held_out"].as_object().context("held")? {
            let e = means
                .entry(arm.clone())
                .or_default()
                .entry(method.clone())
                .or_default();
            e.0 += v["balanced_brier"].as_f64().context("brier")?;
            e.1 += 1;
        }
    }
    let means = means
        .into_iter()
        .map(|(arm, m)| {
            (
                arm,
                Value::Object(
                    m.into_iter()
                        .map(|(k, (s, n))| (k, json!(s / n as f64)))
                        .collect::<serde_json::Map<_, _>>(),
                ),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let report = json!({"schema":"slop_ninja_full_a_calibration_holdout_v2","complete":true,
        "diagnostic_only":true,"final_test_opened":false,"fit_split":CAL_SPLIT,
        "held_out_split":HELD_OUT,"policy":policy.description(),
        "policy_sha256":sha(policy_path)?,"bundle_sha256":bundle_digests(bundle)?,
        "runs_sha256":sha(runs_path)?,"mean_dev_iter_balanced_brier_by_arm":means,"runs":reports,
        "implementation_sha256":hex::encode(Sha256::digest(include_bytes!("calibration_holdout_full_a.rs"))),
        "library_sha256":hex::encode(Sha256::digest(include_bytes!("../src/involvement_calibration.rs"))),
        "limits":["Maps are fitted on CAL only; these numbers never choose a map.",
            "The held-out partition is Dev-iter, which also selects checkpoints: engineering signal only.",
            "Cross-fit folds are out of sample within CAL only."]});
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(out)?;
    serde_json::to_writer_pretty(&mut f, &report)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!("{}", json!({"runs":reports.len(),"means":report["mean_dev_iter_balanced_brier_by_arm"]}));
    Ok(())
}

fn main() -> Result<()> {
    let a = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 4, "BUNDLE POLICY.json RUNS.json NEW_REPORT.json");
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
    )
}
