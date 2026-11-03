//! Exploratory discrimination/calibration diagnostics from saved public scores.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::sha256,
    metrics::{ProbabilityRow, summarize_human_model},
};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const ROLES: [&str; 3] = ["v6", "control", "consistency"];
#[derive(Parser)]
struct Args {
    #[arg(long)]
    plan: PathBuf,
    #[arg(long)]
    reference_artifact: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}
fn read(p: &Path, bindings: &mut BTreeMap<PathBuf, String>) -> Result<Value> {
    let bytes = fs::read(p)?;
    let hash = sha256(&bytes);
    if let Some(old) = bindings.insert(p.to_owned(), hash.clone()) {
        ensure!(old == hash, "File changed during analysis");
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn temperature_one(q: [f64; 3], temperature: f64) -> Result<[f64; 3]> {
    ensure!(
        temperature.is_finite()
            && temperature > 0.0
            && q.iter().all(|p| p.is_finite() && *p > 0.0 && *p <= 1.0)
            && (q.iter().sum::<f64>() - 1.0).abs() < 1e-6,
        "Cannot reconstruct logits from invalid/zero probabilities"
    );
    let logits = q.map(|p| temperature * p.ln());
    let maximum = logits.into_iter().fold(f64::NEG_INFINITY, f64::max);
    let exp = logits.map(|p| (p - maximum).exp());
    let denominator = exp.iter().sum::<f64>();
    Ok(exp.map(|p| p / denominator))
}
fn ai(q: [f64; 3]) -> f64 {
    (q[1] + q[2]).clamp(0.0, 1.0)
}
fn win(model: f64, human: f64) -> f64 {
    if model > human {
        1.0
    } else if model == human {
        0.5
    } else {
        0.0
    }
}
fn auc(pairs: &[(f64, f64)]) -> f64 {
    pairs
        .iter()
        .map(|(_, model)| {
            pairs
                .iter()
                .map(|(human, _)| win(*model, *human))
                .sum::<f64>()
        })
        .sum::<f64>()
        / (pairs.len() * pairs.len()) as f64
}
fn pairs(rows: &[ProbabilityRow]) -> Result<Vec<(f64, f64)>> {
    let mut groups = BTreeMap::<&str, [Option<f64>; 2]>::new();
    for r in rows.iter().filter(|r| r.label < 2) {
        ensure!(
            groups.entry(&r.group).or_default()[r.label]
                .replace(ai(r.probabilities))
                .is_none(),
            "Expected one row per human/model origin and source family"
        );
    }
    if groups.values().all(|v| v[1].is_none()) {
        return Ok(Vec::new());
    }
    groups
        .values()
        .map(|v| {
            Ok((
                v[0].context("Missing human counterpart")?,
                v[1].context("Missing model counterpart")?,
            ))
        })
        .collect()
}
fn measures(rows: &[ProbabilityRow]) -> Result<Value> {
    let binary = summarize_human_model(rows)?.context("Missing human/model observations")?;
    let paired = pairs(rows)?;
    let ranking = if paired.is_empty() {
        Value::Null
    } else {
        json!({"auroc":auc(&paired),"paired_source_groups":paired.len(),
            "within_family_win_rate":paired.iter().map(|(h,m)|win(*m,*h)).sum::<f64>()/paired.len() as f64,
            "model_below_own_human":paired.iter().filter(|(h,m)|m<h).count(),
            "model_tied_with_own_human":paired.iter().filter(|(h,m)|m==h).count(),
            "mean_model_minus_own_human_pp":100.0*paired.iter().map(|(h,m)|m-h).sum::<f64>()/paired.len() as f64})
    };
    Ok(json!({"binary":binary,"ranking":ranking}))
}
fn bootstrap(all: &[Vec<(f64, f64)>]) -> Result<Value> {
    let n = all[0].len();
    ensure!(
        all.len() == 3 && all.iter().all(|a| a.len() == n),
        "Unmatched bootstrap groups"
    );
    if n == 0 {
        return Ok(Value::Null);
    }
    let point: Vec<_> = all.iter().map(|r| auc(r)).collect();
    let mut samples: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::new());
    let mut rng = 0x243f6a8885a308d3_u64;
    let comparisons = [(0, 1), (0, 2), (1, 2)];
    for _ in 0..512 {
        let indices: Vec<_> = (0..n)
            .map(|_| {
                rng = rng.wrapping_add(0x9e3779b97f4a7c15);
                let mut z = rng;
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
                z ^= z >> 31;
                (z % n as u64) as usize
            })
            .collect();
        let scores: Vec<_> = all
            .iter()
            .map(|r| auc(&indices.iter().map(|&i| r[i]).collect::<Vec<_>>()))
            .collect();
        for (i, (before, after)) in comparisons.into_iter().enumerate() {
            samples[i].push(scores[after] - scores[before]);
        }
    }
    let result: Vec<_> = comparisons.into_iter().enumerate().map(|(i,(before,after))| {
        samples[i].sort_by(f64::total_cmp);
        let interval = if (samples[i][499]-samples[i][12]).abs()<1e-12 {None} else {Some([samples[i][12],samples[i][499]])};
        json!({"baseline":ROLES[before],"candidate":ROLES[after],"candidate_minus_baseline":point[after]-point[before],
            "paired_source_group_percentile_95":interval,"uncertainty_status":if interval.is_some(){"paired_source_group_percentile"}else{"degenerate_observed_outcomes"}})
    }).collect();
    Ok(json!({"source_groups":n,"replicates":512,"seed":"243f6a8885a308d3","differences":result}))
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new diagnostic result file");
    let mut bindings = BTreeMap::new();
    let plan = read(&args.plan, &mut bindings)?;
    ensure!(
        plan["schema"] == "slop_ninja_origin_comparison_v1",
        "Unexpected comparison plan"
    );
    let root = PathBuf::from(text(&plan, "output")?);
    let done = read(&root.join("completion.json"), &mut bindings)?;
    ensure!(
        done["public_diagnostics_complete"] == true && done["error"].is_null(),
        "Public results must be complete"
    );
    let mut artifacts = BTreeMap::new();
    for role in ROLES {
        let dir = if role == "v6" {
            args.reference_artifact.clone()
        } else {
            PathBuf::from(text(&plan, "fits")?).join(role)
        };
        let manifest = read(&dir.join("manifest.json"), &mut bindings)?;
        let cp = dir.join("calibration.json");
        let calibration = read(&cp, &mut bindings)?;
        ensure!(
            manifest["files"]["calibration.json"] == bindings[&cp],
            "Calibration file differs from artifact"
        );
        let temp = calibration["temperature"]
            .as_f64()
            .context("Missing temperature")?;
        ensure!(
            temp.is_finite() && temp > 0.0,
            "Invalid original temperature"
        );
        artifacts.insert(role, (text(&manifest, "artifact_id")?.to_string(), temp));
    }
    let mut cases = Vec::new();
    let mut humans = Vec::new();
    let mut original_points = BTreeMap::new();
    for case in plan["cases"].as_array().context("Missing cases")? {
        let name = text(case, "name")?;
        let mut key_reference = None;
        let mut views: [Vec<Vec<ProbabilityRow>>; 2] = std::array::from_fn(|_| Vec::new());
        let mut outputs = Vec::new();
        for role in ROLES {
            let p = if role == "v6" {
                PathBuf::from(text(case, "baseline")?)
            } else {
                root.join(role).join(name).join("report.json")
            };
            let r = read(&p, &mut bindings)?;
            ensure!(
                r["schema"] == "slop_ninja_frozen_encoder_evaluation_v1"
                    && r["split"] == "test"
                    && r["final_test_opened"] == true
                    && r["artifact_id"] == artifacts[role].0,
                "Wrong report identity or partition"
            );
            let mut rows: Vec<ProbabilityRow> =
                serde_json::from_value(r["report"]["predictions"].clone())?;
            rows.sort_by(|a, b| a.id.cmp(&b.id));
            let keys: Vec<_> = rows.iter().map(|r| (&r.id, &r.group, r.label)).collect();
            let signature = json!({"rows":keys,"records":r["records_sha256"],"view":r["evaluation_view"],"policy":r["observation_policy"]});
            ensure!(
                key_reference.as_ref().is_none_or(|v| v == &signature),
                "Report rows/source views differ across models"
            );
            key_reference = Some(signature);
            let points = json!(
                r["report"]["operating_points"]
                    .as_array()
                    .context("Missing original operating points")?
                    .iter()
                    .map(|p| &p["calibration"])
                    .collect::<Vec<_>>()
            );
            if let Some(old) = original_points.insert(role, points.clone()) {
                ensure!(old == points, "Thresholds vary across cases");
            }
            let reconstructed = rows
                .iter()
                .map(|r| {
                    Ok(ProbabilityRow {
                        probabilities: temperature_one(r.probabilities, artifacts[role].1)?,
                        ..r.clone()
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let calibrated = measures(&rows)?;
            ensure!(
                (calibrated["binary"]["log_loss"].as_f64().unwrap()
                    - r["human_model"]["log_loss"]
                        .as_f64()
                        .context("Missing prior binary loss")?)
                .abs()
                    < 1e-12,
                "Recomputed deployed loss differs"
            );
            outputs.push(json!({"role":role,"artifact_id":artifacts[role].0,"temperature":artifacts[role].1,"calibrated":calibrated,"temperature_one":measures(&reconstructed)?}));
            views[0].push(rows);
            views[1].push(reconstructed);
        }
        cases.push(json!({"case":name,"models":outputs,
            "calibrated_auroc_differences":bootstrap(&views[0].iter().map(|r|pairs(r)).collect::<Result<Vec<_>>>()?)?,
            "temperature_one_auroc_differences":bootstrap(&views[1].iter().map(|r|pairs(r)).collect::<Result<Vec<_>>>()?)?}));
        if name == "all-humans" {
            let source = PathBuf::from(text(case, "records")?);
            let bytes = fs::read(&source)?;
            ensure!(
                key_reference.as_ref().context("Missing report identity")?["records"]
                    == sha256(&bytes),
                "Human source corpus differs from scored records"
            );
            bindings.insert(source, sha256(&bytes));
            let mut records = BTreeMap::new();
            for line in std::str::from_utf8(&bytes)?.lines() {
                let r: Value = serde_json::from_str(line)?;
                ensure!(
                    r["origin"] == "human_only"
                        && r["split"] == "test"
                        && r["text_sha256"] == sha256(text(&r, "text")?),
                    "Invalid human source record"
                );
                ensure!(
                    records.insert(text(&r, "id")?.to_string(), r).is_none(),
                    "Duplicate human source"
                );
            }
            ensure!(
                records.len() == views[0][0].len(),
                "Human source coverage differs"
            );
            for (i, first) in views[0][0].iter().enumerate() {
                let source = records.get(&first.id).context("Human text not found")?;
                ensure!(
                    first.label == 0 && source["source_group"] == first.group,
                    "Human source identity differs"
                );
                let mut scores = BTreeMap::new();
                for (j, role) in ROLES.into_iter().enumerate() {
                    let cal = &views[0][j][i];
                    let raw = &views[1][j][i];
                    let flags: Vec<_> = original_points[role]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|t| {
                            Ok(ai(cal.probabilities)
                                > t["threshold"]
                                    .as_f64()
                                    .context("Missing frozen threshold")?)
                        })
                        .collect::<Result<_>>()?;
                    scores.insert(role,json!({"calibrated":cal.probabilities,"temperature_one":raw.probabilities,"flags":flags,
                        "calibrated_ai":ai(cal.probabilities),"temperature_one_ai":ai(raw.probabilities),
                        "human_log_loss":-cal.probabilities[0].max(1e-15).ln(),"temperature_one_human_log_loss":-raw.probabilities[0].max(1e-15).ln()}));
                }
                let delta = scores["consistency"]["human_log_loss"].as_f64().unwrap()
                    - scores["v6"]["human_log_loss"].as_f64().unwrap();
                humans.push(json!({"id":first.id,"source_group":first.group,"collection":source["source"]["collection"],
                    "source_url":source["source"]["url"],"evidence":source["evidence"],"text_sha256":source["text_sha256"],
                    "whitespace_words":text(source,"text")?.split_whitespace().count(),"scores":scores,"consistency_minus_v6_human_log_loss":delta}));
            }
        }
    }
    ensure!(
        cases.len() == 7 && humans.len() == 117,
        "Unexpected diagnostic cohort size"
    );
    humans.sort_by(|a, b| {
        b["consistency_minus_v6_human_log_loss"]
            .as_f64()
            .unwrap()
            .total_cmp(&a["consistency_minus_v6_human_log_loss"].as_f64().unwrap())
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    let mut collections = BTreeMap::<String, Vec<&Value>>::new();
    for row in &humans {
        collections
            .entry(text(row, "collection")?.to_string())
            .or_default()
            .push(row);
    }
    let collection_results:Vec<_>=collections.iter().map(|(name,rows)| {
        let mut scores=BTreeMap::new();
        for role in ROLES {
            let mean=|key:&str|rows.iter().map(|r|r["scores"][role][key].as_f64().unwrap()).sum::<f64>()/rows.len() as f64;
            scores.insert(role,json!({"human_log_loss":mean("human_log_loss"),"temperature_one_human_log_loss":mean("temperature_one_human_log_loss"),
                "false_positives":(0..2).map(|i|rows.iter().filter(|r|r["scores"][role]["flags"][i]==true).count()).collect::<Vec<_>>() }));
        }
        json!({"collection":name,"source_groups":rows.len(),"scores":scores})
    }).collect();
    let delta = |r: &Value| r["consistency_minus_v6_human_log_loss"].as_f64().unwrap();
    let positive: f64 = humans.iter().map(delta).filter(|d| *d > 0.0).sum();
    let top5: f64 = humans.iter().take(5).map(delta).sum();
    let net: f64 = humans.iter().map(delta).sum();
    for (p, h) in &bindings {
        ensure!(sha256(fs::read(p)?) == *h, "Bound input changed");
    }
    let output = json!({"schema":"slop_ninja_origin_error_anatomy_v1","exploratory_posthoc":true,
        "cases":cases,"human_collections":collection_results,"human_error_rows":humans,"original_operating_points":original_points,
        "human_loss_concentration":{"net_total_increase":net,"positive_increases_total":positive,"five_largest_increases_total":top5,
            "five_largest_share_of_positive_increases":if positive>0.0 {Some(top5/positive)}else{None}},
        "bindings":bindings,"new_model_calls":0,"provider_calls":0,"reference_promoted":false,"temperature_or_threshold_fitted":false,
        "limits":"Known Test diagnostics, not fresh confirmation. Family-bootstrap intervals assume independent source families; repeated stages reuse families. Temperature-one values reconstruct relative logits from saved positive probabilities and do not change artifacts or operating points. Three-class temperature scaling can alter collapsed binary rankings. Human labels retain historical-proxy evidence limits. Error inspection does not admit these sources to training."});
    let mut f = fs::File::create_new(&args.out)?;
    serde_json::to_writer_pretty(&mut f, &output)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inverse_temperature_recovers_relative_logits_without_inventing_zero_values() -> Result<()> {
        let q = [0.2, 0.3, 0.5];
        let result = temperature_one(q, 2.0)?;
        for i in 0..3 {
            assert!((result[i] - q[i] * q[i] / 0.38).abs() < 1e-12);
        }
        assert!(temperature_one([0.0, 0.4, 0.6], 2.0).is_err());
        assert!(temperature_one(q, 0.0).is_err());
        assert_eq!(win(0.4, 0.4), 0.5);
        Ok(())
    }
    #[test]
    fn pooled_ranking_and_family_separation_are_different_diagnostics() -> Result<()> {
        let p = vec![(0.8, 0.9), (0.1, 0.2)];
        assert_eq!(auc(&p), 0.75);
        assert!(p.iter().all(|(h, m)| m > h));
        assert_eq!(auc(&[(0.5, 0.5), (0.5, 0.5)]), 0.5);
        let r = bootstrap(&[p.clone(), p.clone(), p])?;
        assert!(
            r["differences"]
                .as_array()
                .unwrap()
                .iter()
                .all(|d| d["candidate_minus_baseline"] == 0.0
                    && d["paired_source_group_percentile_95"].is_null())
        );
        Ok(())
    }
}
