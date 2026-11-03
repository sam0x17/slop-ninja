//! Source-family-weighted evaluation. Inputs are predictions and frozen labels only.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn rows(p: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(p)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn weighted(values: &[(String, f64)]) -> Option<f64> {
    let mut groups = BTreeMap::<&str, Vec<f64>>::new();
    for (g, v) in values {
        groups.entry(g).or_default().push(*v);
    }
    if groups.is_empty() {
        None
    } else {
        Some(
            groups
                .values()
                .map(|v| v.iter().sum::<f64>() / v.len() as f64)
                .sum::<f64>()
                / groups.len() as f64,
        )
    }
}
fn bootstrap(values: &[(String, f64)]) -> Value {
    let mut groups = BTreeMap::<&str, Vec<f64>>::new();
    for (g, v) in values {
        groups.entry(g).or_default().push(*v);
    }
    let means = groups
        .values()
        .map(|v| v.iter().sum::<f64>() / v.len() as f64)
        .collect::<Vec<_>>();
    if means.len() < 2 {
        return Value::Null;
    }
    let mut seed = 20261026u64;
    let mut samples = Vec::new();
    for _ in 0..1000 {
        let mut total = 0.;
        for _ in 0..means.len() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            total += means[seed as usize % means.len()];
        }
        samples.push(total / means.len() as f64);
    }
    samples.sort_by(f64::total_cmp);
    json!({"lower":samples[24],"upper":samples[974],"method":"1000_percentile_source_family_bootstraps_seed20261026","families":means.len(),"scope":"sampling_uncertainty_conditional_on_grammar_exposure_not_clean_holdout"})
}
fn metrics(
    data: &[Value],
    pred: &BTreeMap<String, Value>,
    prior: Option<[f64; 3]>,
) -> Result<Value> {
    let mut brier = Vec::new();
    let mut three = Vec::new();
    let mut correct = Vec::new();
    let mut fp = Vec::new();
    let mut share_abs = Vec::new();
    let mut share_sq = Vec::new();
    let mut share_bias = Vec::new();
    let mut bins = vec![Vec::new(); 10];
    let mut slices = BTreeMap::<String, Vec<(String, f64)>>::new();
    let mut counts = BTreeMap::<String, usize>::new();
    for row in data {
        let id = row["id"].as_str().context("id")?;
        let group = row["source_group"].as_str().context("family")?.to_owned();
        let target = row["targets"]["origin"].as_u64().context("origin")? as usize;
        let prediction = pred.get(id).context("missing prediction")?;
        let p = if let Some(p) = prior {
            p
        } else {
            ["human_only", "model_only", "mixed"].map(|k| prediction["origin"][k].as_f64().unwrap())
        };
        ensure!(
            p.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
                && (p.iter().sum::<f64>() - 1.).abs() < 1e-6,
            "probabilities"
        );
        let score = p[1] + p[2];
        let label = if target == 0 { 0. } else { 1. };
        let error = (score - label).powi(2);
        brier.push((group.clone(), error));
        bins[((score * 10.).floor() as usize).min(9)].push((group.clone(), score, label));
        three.push((
            group.clone(),
            (0..3)
                .map(|i| (p[i] - if i == target { 1. } else { 0. }).powi(2))
                .sum(),
        ));
        let winner = (0..3)
            .max_by(|a, b| p[*a].total_cmp(&p[*b]).then_with(|| b.cmp(a)))
            .unwrap();
        correct.push((group.clone(), if winner == target { 1. } else { 0. }));
        if target == 0 {
            fp.push((group.clone(), if score >= 0.5 { 1. } else { 0. }));
        }
        if let Some(truth) = row["targets"]["share"].as_f64() {
            let estimate = if prior.is_some() {
                p[1] + p[2] * 0.5
            } else {
                prediction["ai_share_percent"]
                    .as_f64()
                    .context("share prediction")?
                    / 100.
            };
            ensure!(
                estimate >= p[1] - 1e-6 && estimate <= score + 1e-6,
                "incoherent share"
            );
            let e = estimate - truth;
            share_abs.push((group.clone(), e.abs() * 100.));
            share_sq.push((group.clone(), e * e * 10000.));
            share_bias.push((group.clone(), e * 100.));
            let key = if truth == 0. {
                "0"
            } else if truth == 1. {
                "100"
            } else if truth <= 0.25 {
                "0-25"
            } else if truth <= 0.5 {
                "25-50"
            } else if truth <= 0.75 {
                "50-75"
            } else {
                "75-100"
            };
            slices
                .entry(key.into())
                .or_default()
                .push((group.clone(), e.abs() * 100.));
        }
        *counts
            .entry(["human_only", "model_only", "mixed"][target].into())
            .or_default() += 1;
    }
    let mut ece = 0.;
    let mut calibration = Vec::new();
    let mut group_counts = BTreeMap::<String, usize>::new();
    for (g, _) in &brier {
        *group_counts.entry(g.clone()).or_default() += 1;
    }
    for (i, bin) in bins.iter().enumerate() {
        let mut mass = 0.;
        let mut predicted = 0.;
        let mut observed = 0.;
        for (g, p, y) in bin {
            let w = 1. / group_counts[g] as f64 / group_counts.len() as f64;
            mass += w;
            predicted += w * p;
            observed += w * y;
        }
        if mass > 0. {
            ece += (predicted - observed).abs();
            calibration.push(json!({"bin":i,"rows":bin.len(),"weight":mass,"mean_score":predicted/mass,"observed_ai":observed/mass}));
        }
    }
    Ok(
        json!({"documents":data.len(),"source_families":group_counts.len(),"origin_counts":counts,"involvement_brier":weighted(&brier),"involvement_brier_95ci":bootstrap(&brier),"origin_brier_sum3":weighted(&three),"origin_accuracy":weighted(&correct),"human_fpr_at_0_5":weighted(&fp),"human_documents":fp.len(),"ece10":ece,"calibration_bins":calibration,"share":{"labelled_documents":share_abs.len(),"mae_percentage_points":weighted(&share_abs),"mse_percentage_points_squared":weighted(&share_sq),"bias_percentage_points":weighted(&share_bias),"by_realized_share":slices.iter().map(|(k,v)|(k,json!({"documents":v.len(),"mae_percentage_points":weighted(v)}))).collect::<BTreeMap<_,_>>()},"attribution":{"human":"unavailable_no_supervision_or_independent_gallery","ai":"unavailable_no_multi_generator_supervision"},"false_positive_guarantee":false}),
    )
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 4,
        "evaluate_shared DATA FIT SPLIT NEW_REPORT.json"
    );
    ensure!(
        matches!(a[2].as_str(), "development" | "calibration" | "test"),
        "evaluation split"
    );
    let data = Path::new(&a[0]);
    let fit = Path::new(&a[1]);
    let eval = rows(&data.join(format!("{}.jsonl", a[2])))?;
    let predictions = read(&fit.join(format!("{}-predictions.json", a[2])))?;
    let pred = predictions
        .as_array()
        .context("predictions")?
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_owned(),
                r["prediction"].clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    ensure!(pred.len() == eval.len(), "prediction roster differs");
    let train = rows(&data.join("train.jsonl"))?;
    let mut weights = BTreeMap::<String, usize>::new();
    for r in &train {
        *weights
            .entry(r["source_group"].as_str().unwrap().into())
            .or_default() += 1;
    }
    let mut prior = [0.; 3];
    for r in &train {
        prior[r["targets"]["origin"].as_u64().unwrap() as usize] +=
            1. / weights[r["source_group"].as_str().unwrap()] as f64 / weights.len() as f64;
    }
    let result = json!({"schema":"slop_ninja_shared_a_development_report_v1","split":a[2],"clean_holdout":false,"scope":"grammar_exposed_single_generator_partial_supervision","model":metrics(&eval,&pred,None)?,"train_family_prior":prior,"prior_baseline":metrics(&eval,&pred,Some(prior))?,"fit":read(&fit.join("fit.json"))?,"population":read(&data.join("manifest.json"))?,"test_used_for_selection":false});
    ensure!(!Path::new(&a[3]).exists(), "output exists");
    fs::write(&a[3], serde_json::to_vec_pretty(&result)?)?;
    println!(
        "{}",
        serde_json::to_string(
            &json!({"report":a[3],"model_brier":result["model"]["involvement_brier"],"prior_brier":result["prior_baseline"]["involvement_brier"]})
        )?
    );
    Ok(())
}
