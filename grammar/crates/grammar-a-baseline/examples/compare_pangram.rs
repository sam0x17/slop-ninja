//! Pangram fractions are not involvement probabilities. Fit a labelled calibration map separately.
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
fn mean(v: &[(String, f64)]) -> f64 {
    let mut g = BTreeMap::<&str, Vec<f64>>::new();
    for (k, x) in v {
        g.entry(k).or_default().push(*x);
    }
    g.values()
        .map(|v| v.iter().sum::<f64>() / v.len() as f64)
        .sum::<f64>()
        / g.len() as f64
}
fn map(score: f64, t: f64) -> f64 {
    let p = score.clamp(0.001, 0.999);
    1. / (1. + (-((p / (1. - p)).ln() / t)).exp())
}
fn pairs(rows: &[Value], root: &Path) -> Result<Vec<(String, f64, f64, Value)>> {
    rows.iter().map(|r|{
 let id=r["id"].as_str().context("id")?;let value=read(&root.join(id).join("result.json"))?;let response=&value["response"];
 ensure!(response["stage"]=="STAGE_SUCCESS","Pangram failure retained separately");
 let score=response["fraction_ai"].as_f64().context("ai fraction")?+response["fraction_ai_assisted"].as_f64().context("assisted fraction")?;
 ensure!((0.0..=1.000001).contains(&score),"fraction range");let y=if r["targets"]["ai_involvement"]==true{1.}else{0.};
 Ok((r["source_group"].as_str().context("family")?.into(),score.min(1.),y,json!({"id":id,"version":response["version"],"selector":value["model"],"raw_prediction_short":response["prediction_short"],"nonhuman_fraction":score,"origin":r["targets"]["origin"]})))
 }).collect()
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 5,
        "compare_pangram DATA DEV_RESULTS CAL_RESULTS MODEL_DEV_REPORT NEW_REPORT"
    );
    let data = Path::new(&a[0]);
    let calibration = pairs(&rows(&data.join("calibration.jsonl"))?, Path::new(&a[2]))?;
    let development = pairs(&rows(&data.join("development.jsonl"))?, Path::new(&a[1]))?;
    let objective = |t: f64| {
        mean(
            &calibration
                .iter()
                .map(|(g, s, y, _)| (g.clone(), (map(*s, t) - y).powi(2)))
                .collect::<Vec<_>>(),
        )
    };
    let temperature = std::iter::once(1.)
        .chain((-30..=30).map(|k| (k as f64 * 0.1).exp()))
        .min_by(|a, b| {
            objective(*a)
                .total_cmp(&objective(*b))
                .then_with(|| (a - 1.).abs().total_cmp(&(b - 1.).abs()))
        })
        .unwrap();
    let brier = mean(
        &development
            .iter()
            .map(|(g, s, y, _)| (g.clone(), (map(*s, temperature) - y).powi(2)))
            .collect::<Vec<_>>(),
    );
    let human = development
        .iter()
        .filter(|(_, _, y, _)| *y == 0.)
        .collect::<Vec<_>>();
    let fpr = mean(
        &human
            .iter()
            .map(|(g, s, _, _)| (g.clone(), if map(*s, temperature) >= 0.5 { 1. } else { 0. }))
            .collect::<Vec<_>>(),
    );
    let involvement_accuracy = mean(
        &development
            .iter()
            .map(|(g, s, y, _)| {
                (
                    g.clone(),
                    if (map(*s, temperature) >= 0.5) == (*y == 1.) {
                        1.
                    } else {
                        0.
                    },
                )
            })
            .collect::<Vec<_>>(),
    );
    let model = read(Path::new(&a[3]))?;
    let paired=development.iter().map(|(_,s,y,r)|json!({"id":r["id"],"label_ai":*y==1.,"response_version":r["version"],"selector":r["selector"],"raw_prediction_short":r["raw_prediction_short"],"raw_nonhuman_fraction":s,"calibrated_involvement_probability":map(*s,temperature)})).collect::<Vec<_>>();
    let result = json!({"schema":"slop_ninja_a_pangram_comparison_v1","scope":"same_9_grammar_exposed_development_documents_4_families;not_clean_holdout","calibration":{"method":"nonhuman_fraction_clipped_0.001_0.999_logodds_temperature;equal_source_family_brier","temperature":temperature,"calibration_rows":calibration.len(),"grid":"exp(k*0.1),k=-30..30,includes1","meaning":"our_Calibration_fitted_probability_mapping_not_Pangram_native_confidence"},"pangram":{"involvement_brier":brier,"human_fpr_at_0_5":fpr,"human_documents":human.len(),"involvement_accuracy":involvement_accuracy,"share_qualification":"fraction_predictions_are_not_known_replacement_span_share_ground_truth","records":paired},"slop_ninja":model["model"],"brier_gap_slop_ninja_minus_pangram":model["model"]["involvement_brier"].as_f64().unwrap()-brier,"native_pangram_probabilities_available":false,"parity_established":false,"test_opened":false});
    ensure!(!Path::new(&a[4]).exists(), "new output required");
    fs::write(&a[4], serde_json::to_vec_pretty(&result)?)?;
    println!(
        "{}",
        serde_json::to_string(
            &json!({"pangram_brier":brier,"slop_ninja_brier":model["model"]["involvement_brier"],"pangram_human_fpr":fpr,"report":a[4]})
        )?
    );
    Ok(())
}
