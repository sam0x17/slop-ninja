//! Diagnostic E0/E1/E2 metrics on the exposed stage-3 Development and CAL.
//! Usage: evaluate_full_a_existing BUNDLE POLICY.json RUNS.json NEW_OUTPUT_DIR
//!
//! For every run, the involvement temperature and isotonic maps are both fitted
//! on CAL only (`involvement_calibration::select`), then applied to
//! Development and CAL. CAL numbers under a CAL-fitted map are in-sample; the
//! cross-fitted CAL Brier from `select` is reported beside them. No fitting of
//! model weights, no detector input, no Test. Metric definitions come from the
//! frozen stage-3 evaluator (`evaluate_supervised.rs`) plus explicit
//! per-stratum and count summaries.
#[allow(dead_code, unused_imports)]
#[path = "evaluate_supervised.rs"]
pub(crate) mod evaluation;
use anyhow::{Context, Result, bail, ensure};
use grammar_a_baseline::involvement_calibration::{Map, Observation, Policy, select};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

type Pred = BTreeMap<String, Value>;
const PARTS: [&str; 2] = ["development", "calibration"];
const VARIANTS: [&str; 3] = ["uncalibrated", "temperature", "isotonic"];
const STRATA: [&str; 5] = ["human", "mixed_(0,25]", "mixed_(25,50]", "mixed_(50,100]", "model_only"];

fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(
        fs::read(p).with_context(|| format!("read {}", p.display()))?,
    )))
}
fn read(p: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(p).with_context(|| format!("read {}", p.display()))?)
        .with_context(|| format!("parse {}", p.display()))
}
fn jsonl(p: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(p)
        .with_context(|| format!("read {}", p.display()))?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing string {k}"))
}
fn finite(v: &Value) -> Result<f64> {
    v.as_f64().filter(|x| x.is_finite()).context("finite number")
}
#[cfg(test)]
fn sigmoid(x: f64) -> f64 {
    if x >= 0. {
        1. / (1. + (-x).exp())
    } else {
        let e = x.exp();
        e / (1. + e)
    }
}
fn logits3(v: &Value) -> Result<[f64; 3]> {
    let a = v.as_array().context("origin logits")?;
    ensure!(a.len() == 3, "origin logit dimension");
    Ok([finite(&a[0])?, finite(&a[1])?, finite(&a[2])?])
}
fn logsumexp(a: f64, b: f64) -> f64 {
    let m = a.max(b);
    m + ((a - m).exp() + (b - m).exp()).ln()
}

/// One document's uncalibrated raw outputs in the common form.
#[derive(Clone, Debug)]
pub(crate) struct Raw {
    pub g: f64,
    pub r: f64,
    pub q: f64,
    pub token_mean: Option<f64>,
}

/// g = log(p_model + p_mixed) - log(p_human); r = p_model / (p_model + p_mixed).
pub(crate) fn raw_from(p: &Value, format: &str) -> Result<Raw> {
    let l = logits3(&p["origin_logits"])?;
    let from_logits = logsumexp(l[1], l[2]) - l[0];
    let g = match format {
        "last_block" => from_logits,
        "full_a" => finite(&p["involvement_logit"])?,
        _ => bail!("unknown raw format"),
    };
    let m = l[1].max(l[2]);
    let r = (l[1] - m).exp() / ((l[1] - m).exp() + (l[2] - m).exp());
    let q = finite(&p["conditional_mixed_share"])?;
    ensure!((0.0..=1.).contains(&q), "conditional share bound");
    let token_mean = match &p["token_mean"] {
        Value::Null => None,
        v => Some(finite(v)?),
    };
    Ok(Raw { g, r, q, token_mean })
}

/// Document outputs under a frozen involvement map (same rule as full_a.py
/// `calibrated_outputs`): origin = [1-p, p r, p (1-r)], share projected into
/// [P(model_only), P(involvement)].
pub(crate) fn outputs(raw: &Raw, map: &Map, share_mode: &str) -> Result<Value> {
    let p = map.apply(raw.g)?;
    let o = [1. - p, p * raw.r, p * (1. - raw.r)];
    let s = match share_mode {
        "head" => o[1] + o[2] * raw.q,
        "token_mean_projected" => raw.token_mean.context("token mean required")?,
        _ => bail!("unknown share mode"),
    };
    let share = s.max(o[1]).min(p);
    Ok(json!({"origin":{"human_only":o[0],"model_only":o[1],"mixed":o[2]},
        "ai_confidence_percent":100.*p,"ai_share_percent":100.*share}))
}

pub(crate) fn stratum(row: &Value) -> Result<&'static str> {
    let t = &row["targets"];
    Ok(match t["origin"].as_u64().context("origin")? {
        0 => "human",
        1 => "model_only",
        2 => {
            let s = finite(&t["share"])?;
            if s <= 0.25 {
                "mixed_(0,25]"
            } else if s <= 0.5 {
                "mixed_(25,50]"
            } else {
                "mixed_(50,100]"
            }
        }
        _ => bail!("origin"),
    })
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// Explicit counts and unweighted per-stratum summaries beside the frozen metrics.
fn extra(rows: &[Value], pred: &Pred) -> Result<Value> {
    let mut by = BTreeMap::<&str, (Vec<f64>, BTreeMap<String, Vec<f64>>, Vec<f64>)>::new();
    let (mut flags, mut humans, mut human_p) = (0usize, 0usize, Vec::new());
    let mut mixed_abs = Vec::new();
    for r in rows {
        let p = &pred[string(r, "id")?];
        let score = finite(&p["ai_confidence_percent"])? / 100.;
        let y = f64::from(r["targets"]["origin"].as_u64() != Some(0));
        let s = stratum(r)?;
        let e = by.entry(s).or_default();
        e.0.push((score - y).powi(2));
        e.1.entry(string(r, "source_group")?.into()).or_default().push((score - y).powi(2));
        e.2.push(score);
        if s == "human" {
            humans += 1;
            flags += usize::from(score >= 0.5);
            human_p.push(score);
        }
        if r["targets"]["origin"] == 2 {
            let share = finite(&p["ai_share_percent"])?;
            mixed_abs.push((share - 100. * finite(&r["targets"]["share"])?).abs());
        }
    }
    let strata = STRATA
        .iter()
        .map(|s| {
            let (b, g, p) = by.get(s).cloned().unwrap_or_default();
            let fam = g.values().map(|v| v.iter().sum::<f64>() / v.len() as f64).collect::<Vec<_>>();
            (*s, json!({"documents":b.len(),"families":g.len(),"brier_unweighted":mean(&b),
                "brier_family_weighted":mean(&fam),"mean_involvement":mean(&p)}))
        })
        .collect::<BTreeMap<_, _>>();
    let mut sorted = human_p.clone();
    sorted.sort_by(f64::total_cmp);
    Ok(json!({"strata":strata,"human_documents":humans,"human_flags_at_0_5":flags,
        "human_fpr_at_0_5_unweighted":(humans>0).then(|| flags as f64 / humans as f64),
        "human_mean_involvement_unweighted":mean(&human_p),
        "human_max_involvement":sorted.last(),
        "mixed_share_mae_points_unweighted":mean(&mixed_abs),"mixed_documents":mixed_abs.len()}))
}

/// Per family/origin cell mean involvement squared error (balanced metric cells).
fn cells(rows: &[Value], pred: &Pred) -> Result<BTreeMap<(String, u64), f64>> {
    let mut c = BTreeMap::<(String, u64), Vec<f64>>::new();
    for r in rows {
        let y = r["targets"]["origin"].as_u64().context("origin")?;
        let p = finite(&pred[string(r, "id")?]["ai_confidence_percent"])? / 100.;
        c.entry((string(r, "source_group")?.into(), y))
            .or_default()
            .push((p - f64::from(y != 0)).powi(2));
    }
    Ok(c.into_iter().map(|(k, v)| (k, v.iter().sum::<f64>() / v.len() as f64)).collect())
}

/// Paired family bootstrap of balanced Brier differences: cell differences
/// averaged over seeds, family mean of cells, 1000 xorshift64 draws seed 20261026.
fn paired(diffs: &BTreeMap<(String, u64), Vec<f64>>) -> Value {
    let mut fam = BTreeMap::<&str, Vec<f64>>::new();
    for ((g, _), v) in diffs {
        fam.entry(g).or_default().push(v.iter().sum::<f64>() / v.len() as f64);
    }
    let means = fam.values().map(|v| v.iter().sum::<f64>() / v.len() as f64).collect::<Vec<_>>();
    let estimate = mean(&means);
    if means.len() < 2 {
        return json!({"estimate":estimate,"ci95":null});
    }
    let mut seed = 20261026u64;
    let mut draws = Vec::with_capacity(1000);
    for _ in 0..1000 {
        let mut total = 0.;
        for _ in &means {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            total += means[seed as usize % means.len()];
        }
        draws.push(total / means.len() as f64);
    }
    draws.sort_by(f64::total_cmp);
    json!({"estimate":estimate,"ci95":[draws[24],draws[974]],"families":means.len(),
        "method":"1000_paired_source_family_bootstraps_seed20261026; cell differences averaged over seeds first"})
}

fn time_file(p: &Path) -> Result<Value> {
    let s = fs::read_to_string(p)?;
    let mut wall = None;
    let mut rss = None;
    for l in s.lines() {
        let l = l.trim();
        if let Some(v) = l.strip_prefix("Elapsed (wall clock) time (h:mm:ss or m:ss): ") {
            let parts = v.split(':').map(|x| x.parse::<f64>()).collect::<Result<Vec<_>, _>>()?;
            wall = Some(parts.iter().fold(0., |a, x| a * 60. + x));
        }
        if let Some(v) = l.strip_prefix("Maximum resident set size (kbytes): ") {
            rss = Some(v.parse::<u64>()?);
        }
    }
    Ok(json!({"wall_seconds":wall,"max_rss_kib":rss}))
}

fn run(bundle: &Path, policy_path: &Path, runs_path: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output directory required");
    let manifest = read(&bundle.join("manifest.json"))?;
    ensure!(
        manifest["schema"] == "slop_ninja_full_a_dataset_v1"
            && manifest["complete"] == true
            && manifest["final_test_opened"] == false
            && manifest["contract_sha256"].as_str() == Some(sha(&bundle.join("contract.json"))?.as_str()),
        "non-Test full-A bundle required"
    );
    let policy = Policy::from_json(&read(policy_path)?)?;
    let spec = read(runs_path)?;
    let baseline = string(&spec, "baseline_arm")?.to_owned();
    let mut rows = BTreeMap::<&str, Vec<Value>>::new();
    for part in PARTS {
        let name = format!("{part}.jsonl");
        ensure!(
            manifest["files_sha256"][&name].as_str() == Some(sha(&bundle.join(&name))?.as_str()),
            "{name} changed"
        );
        let r = jsonl(&bundle.join(&name))?;
        ensure!(
            r.iter().all(|x| x["frozen_split"] == part && x["purpose"] == "query"),
            "partition routing"
        );
        rows.insert(part, r);
    }
    fs::create_dir(out)?;
    let mut per_run = Vec::new();
    // (arm, variant, part) -> seed -> cells
    let mut cell_store = BTreeMap::<(String, &str, &str), BTreeMap<i64, BTreeMap<(String, u64), f64>>>::new();
    let mut summaries = BTreeMap::<(String, &str, &str), Vec<Value>>::new();
    let mut inputs = Vec::new();
    for run in spec["runs"].as_array().context("runs")? {
        let arm = string(run, "arm")?.to_owned();
        let seed = run["seed"].as_i64().context("seed")?;
        let format = string(run, "format")?;
        let share_mode = string(run, "share_mode")?;
        let mut raw = BTreeMap::<&str, BTreeMap<String, Raw>>::new();
        for part in PARTS {
            let dir = PathBuf::from(string(run, part)?);
            let pm = read(&dir.join("manifest.json"))?;
            let predictions = dir.join("predictions.jsonl");
            ensure!(
                pm["complete"] == true
                    && pm["partition"] == part
                    && pm["final_test_opened"] == false
                    && pm["predictions_sha256"].as_str() == Some(sha(&predictions)?.as_str())
                    && pm["unsupported"] == 0,
                "raw prediction receipt"
            );
            inputs.push(json!({"arm":arm,"seed":seed,"partition":part,"manifest_sha256":sha(&dir.join("manifest.json"))?,
                "predictions_sha256":pm["predictions_sha256"],"path":dir}));
            let truth = rows[part]
                .iter()
                .map(|r| Ok((string(r, "id")?, r)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            let mut m = BTreeMap::new();
            for p in jsonl(&predictions)? {
                let id = string(&p, "id")?.to_owned();
                let t = truth.get(id.as_str()).context("prediction outside partition")?;
                if format == "full_a" {
                    ensure!(p["text_sha256"] == t["text_sha256"], "prediction bound to other text");
                }
                ensure!(m.insert(id, raw_from(&p, format)?).is_none(), "duplicate prediction");
            }
            ensure!(m.len() == truth.len(), "incomplete predictions");
            raw.insert(part, m);
        }
        let observations = rows["calibration"]
            .iter()
            .map(|r| {
                Ok(Observation {
                    family: string(r, "source_group")?.into(),
                    origin: r["targets"]["origin"].as_u64().context("origin")? as usize,
                    logit: raw["calibration"][string(r, "id")?].g,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let (_, report) = select(&observations, &policy)?;
        let maps = [
            Map::Identity,
            Map::Temperature(finite(&report["temperature"])?),
            Map::from_description(&report["isotonic_candidate"])?,
        ];
        let mut results = BTreeMap::new();
        for (variant, map) in VARIANTS.iter().zip(&maps) {
            let mut by_part = BTreeMap::new();
            for part in PARTS {
                let pred = raw[part]
                    .iter()
                    .map(|(id, r)| Ok((id.clone(), outputs(r, map, share_mode)?)))
                    .collect::<Result<Pred>>()?;
                let rows = &rows[part];
                let family = evaluation::origin_share_metrics(rows, &pred, None)?;
                let balanced = evaluation::balanced_origin_metrics(rows, &pred, None)?;
                let ex = extra(rows, &pred)?;
                let summary = json!({
                    "balanced_involvement_brier": balanced["involvement_brier"],
                    "balanced_involvement_brier_95ci": balanced["involvement_brier_95ci"],
                    "family_involvement_brier": family["involvement_brier"],
                    "human_fpr_at_0_5_family_weighted": family["human_fpr_at_0_5"],
                    "human_flags_at_0_5": ex["human_flags_at_0_5"], "human_documents": ex["human_documents"],
                    "human_mean_involvement": ex["human_mean_involvement_unweighted"],
                    "human_max_involvement": ex["human_max_involvement"],
                    "strata": ex["strata"],
                    "share_mae_points_family_weighted": family["share"]["mae_percentage_points"]["estimate"],
                    "share_bias_points_family_weighted": family["share"]["bias_percentage_points"]["estimate"],
                    "share_by_realized_share": family["share"]["by_realized_share"],
                    "mixed_share_mae_points_unweighted": ex["mixed_share_mae_points_unweighted"],
                    "three_way": {"balanced_origin_accuracy": balanced["origin_accuracy"],
                        "balanced_origin_brier_sum3": balanced["origin_brier_sum3"],
                        "family_origin_accuracy": family["origin_accuracy"]["estimate"],
                        "family_origin_brier_sum3": family["origin_brier_sum3"]["estimate"],
                        "confusion_counts_truth_by_predicted": family["confusion_counts_truth_by_predicted"],
                        "origin_order": family["origin_order"]},
                    "ece10_balanced": balanced["ece10"], "ece10_family": family["ece10"],
                });
                cell_store
                    .entry((arm.clone(), variant, part))
                    .or_default()
                    .insert(seed, cells(rows, &pred)?);
                summaries.entry((arm.clone(), variant, part)).or_default().push(summary.clone());
                by_part.insert(part, json!({"summary":summary,"family":family,"family_origin_balanced":balanced}));
            }
            results.insert(*variant, by_part);
        }
        let mut resources = json!({});
        if let Some(p) = run["fit_json"].as_str() {
            let f = read(Path::new(p))?;
            for k in ["peak_cuda_allocated_bytes", "selected_epoch", "selected_evaluation", "selected_global_step",
                      "development_involvement_brier", "evaluations", "stopped_early", "epochs"] {
                if !f[k].is_null() {
                    resources[k] = f[k].clone();
                }
            }
        }
        if let Some(p) = run["time_file"].as_str() {
            resources["fit_process"] = time_file(Path::new(p))?;
        }
        let entry = json!({"arm":arm,"seed":seed,"format":format,"share_mode":share_mode,
            "calibration_fit":report,"results":results,"resources":resources});
        fs::write(out.join(format!("{arm}-seed-{seed}.json")), serde_json::to_vec_pretty(&entry)?)?;
        per_run.push(json!({"arm":arm,"seed":seed,"resources":entry["resources"],
            "temperature":report["temperature"],"isotonic_knots":report["isotonic_candidate"]["knots"].as_array().map(Vec::len),
            "crossfit_CAL_brier":{"isotonic":report["isotonic_crossfit_CAL_brier"],"temperature":report["temperature_crossfit_CAL_brier"]},
            "in_sample_CAL_brier":{"uncalibrated":report["uncalibrated_CAL_brier"],"isotonic":report["isotonic_in_sample_CAL_brier"],
                "temperature":report["temperature_in_sample_CAL_brier"]},
            "policy_selected":report["selected_method"],"policy_reason":report["selection_reason"]}));
    }
    // Seed means and ranges of the headline numbers.
    let pick = |v: &Value, path: &[&str]| -> Option<f64> {
        let mut x = v;
        for k in path {
            x = &x[*k];
        }
        x.as_f64()
    };
    let headline: [(&str, &[&str]); 13] = [
        ("balanced_involvement_brier", &["balanced_involvement_brier"]),
        ("family_involvement_brier", &["family_involvement_brier"]),
        ("human_fpr_at_0_5_family_weighted", &["human_fpr_at_0_5_family_weighted"]),
        ("human_mean_involvement", &["human_mean_involvement"]),
        ("brier_human", &["strata", "human", "brier_unweighted"]),
        ("brier_mixed_(0,25]", &["strata", "mixed_(0,25]", "brier_unweighted"]),
        ("brier_mixed_(25,50]", &["strata", "mixed_(25,50]", "brier_unweighted"]),
        ("brier_mixed_(50,100]", &["strata", "mixed_(50,100]", "brier_unweighted"]),
        ("brier_model_only", &["strata", "model_only", "brier_unweighted"]),
        ("share_mae_points_family_weighted", &["share_mae_points_family_weighted"]),
        ("mixed_share_mae_points_unweighted", &["mixed_share_mae_points_unweighted"]),
        ("balanced_origin_accuracy", &["three_way", "balanced_origin_accuracy"]),
        ("balanced_origin_brier_sum3", &["three_way", "balanced_origin_brier_sum3"]),
    ];
    let mut aggregate = BTreeMap::<String, Value>::new();
    for ((arm, variant, part), list) in &summaries {
        let mut m = serde_json::Map::new();
        for (name, path) in &headline {
            let v = list.iter().filter_map(|s| pick(s, path)).collect::<Vec<_>>();
            let lo = v.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let (lo, hi) = if v.is_empty() { (Value::Null, Value::Null) } else { (json!(lo), json!(hi)) };
            m.insert((*name).into(), json!({"mean":mean(&v),"min":lo,"max":hi,"seeds":v.len()}));
        }
        let flags = list.iter().filter_map(|s| s["human_flags_at_0_5"].as_u64()).collect::<Vec<_>>();
        m.insert("human_flags_at_0_5_per_seed".into(), json!(flags));
        m.insert("human_documents".into(), list[0]["human_documents"].clone());
        aggregate.insert(format!("{arm}|{variant}|{part}"), Value::Object(m));
    }
    // Paired balanced differences against the baseline arm, matched seeds.
    let mut comparisons = BTreeMap::<String, Value>::new();
    let arms = summaries.keys().map(|(a, _, _)| a.clone()).collect::<BTreeSet<_>>();
    for arm in arms.iter().filter(|a| **a != baseline) {
        for variant in VARIANTS {
            for part in PARTS {
                let (Some(a), Some(b)) = (
                    cell_store.get(&(arm.clone(), variant, part)),
                    cell_store.get(&(baseline.clone(), variant, part)),
                ) else {
                    continue;
                };
                let seeds = a.keys().filter(|s| b.contains_key(s)).copied().collect::<Vec<_>>();
                ensure!(!seeds.is_empty(), "no matched seeds");
                let mut diffs = BTreeMap::<(String, u64), Vec<f64>>::new();
                for s in &seeds {
                    for (k, v) in &a[s] {
                        diffs.entry(k.clone()).or_default().push(v - b[s].get(k).context("cell")?);
                    }
                }
                let mut r = paired(&diffs);
                r["seeds"] = json!(seeds);
                comparisons.insert(format!("{arm}-minus-{baseline}|{variant}|{part}"), r);
            }
        }
    }
    let report = json!({"schema":"slop_ninja_full_a_existing_data_diagnostics_v1","complete":true,
        "final_test_opened":false,"diagnostic_only":true,
        "scope":"stage-3 Development and CAL are already exposed; engineering signal only, not selection evidence",
        "calibration":"both involvement maps fitted on CAL only per run; CAL rows under a fitted map are in-sample",
        "bundle_manifest_sha256":sha(&bundle.join("manifest.json"))?,"policy":policy.description(),
        "runs_spec_sha256":sha(runs_path)?,"inputs":inputs,"runs":per_run,"seed_aggregates":aggregate,
        "paired_balanced_brier_differences":comparisons,
        "implementation_sha256":hex::encode(Sha256::digest(include_bytes!("evaluate_full_a_existing.rs"))),
        "metrics_implementation_sha256":hex::encode(Sha256::digest(include_bytes!("evaluate_supervised.rs")))});
    let mut f = fs::OpenOptions::new().create_new(true).write(true).open(out.join("report.json"))?;
    serde_json::to_writer_pretty(&mut f, &report)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!("{}", json!({"complete":true,"runs":per_run.len()}));
    Ok(())
}

fn main() -> Result<()> {
    let a = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 4, "BUNDLE POLICY.json RUNS.json NEW_OUTPUT_DIR");
    run(Path::new(&a[0]), Path::new(&a[1]), Path::new(&a[2]), Path::new(&a[3]))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn last_block_logit_and_identity_reproduce_softmax() {
        let p = json!({"origin_logits":[0.3,-1.2,0.9],"conditional_mixed_share":0.25});
        let raw = raw_from(&p, "last_block").unwrap();
        let o = outputs(&raw, &Map::Identity, "head").unwrap();
        let e = [0.3f64.exp(), (-1.2f64).exp(), 0.9f64.exp()];
        let s = e.iter().sum::<f64>();
        for (k, v) in ["human_only", "model_only", "mixed"].iter().zip(e) {
            assert!((o["origin"][*k].as_f64().unwrap() - v / s).abs() < 1e-12);
        }
        let share = o["ai_share_percent"].as_f64().unwrap() / 100.;
        assert!((share - (e[1] + e[2] * 0.25) / s).abs() < 1e-12);
    }
    #[test]
    fn token_share_is_projected_into_bounds() {
        let p = json!({"origin_logits":[2.0,-3.0,-1.0],"involvement_logit":-1.5,"conditional_mixed_share":0.5,"token_mean":0.9});
        let raw = raw_from(&p, "full_a").unwrap();
        let o = outputs(&raw, &Map::Temperature(2.0), "token_mean_projected").unwrap();
        let pi = o["ai_confidence_percent"].as_f64().unwrap();
        assert!((pi - 100. * sigmoid(-0.75)).abs() < 1e-9);
        assert!((o["ai_share_percent"].as_f64().unwrap() - pi).abs() < 1e-12);
        assert!(raw_from(&json!({"origin_logits":[0,0],"conditional_mixed_share":0.5}), "last_block").is_err());
    }
    #[test]
    fn strata_and_paired_bootstrap() {
        let row = |o: u64, s: f64| json!({"targets":{"origin":o,"share":s}});
        assert_eq!(stratum(&row(2, 0.25)).unwrap(), "mixed_(0,25]");
        assert_eq!(stratum(&row(2, 0.26)).unwrap(), "mixed_(25,50]");
        assert_eq!(stratum(&row(2, 1.0)).unwrap(), "mixed_(50,100]");
        assert_eq!(stratum(&row(0, 0.0)).unwrap(), "human");
        let mut d = BTreeMap::new();
        d.insert(("a".to_string(), 0), vec![0.1, 0.3]);
        d.insert(("a".to_string(), 2), vec![0.0, 0.0]);
        d.insert(("b".to_string(), 0), vec![-0.2, -0.2]);
        let r = paired(&d);
        assert!((r["estimate"].as_f64().unwrap() - (0.1 - 0.2) / 2.).abs() < 1e-12);
        let ci = r["ci95"].as_array().unwrap();
        assert!(ci[0].as_f64().unwrap() <= ci[1].as_f64().unwrap());
    }
}
