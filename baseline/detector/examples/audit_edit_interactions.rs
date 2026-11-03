//! Separate probability-scale effects from class-logit interactions in a saved edit cube.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde::Deserialize;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};

const ARTIFACT: &str = "4e061df4789e546dfdd58c8005860a98fb8f200a4c7642b7491461f44f16f138";
const SCALE_NAMES: [&str; 5] = [
    "probability",
    "calibrated_union_margin",
    "raw_union_margin",
    "model_minus_human_logit",
    "mixed_minus_human_logit",
];
type Scales = [f64; 5];

#[derive(Parser)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    path: PathBuf,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    plan: Binding,
    scores: Binding,
    ranking: Binding,
    artifact_manifest: Binding,
    calibration: Binding,
    protocol: Binding,
    choices: Vec<String>,
}
fn load(binding: &Binding) -> Result<Vec<u8>> {
    let bytes = fs::read(&binding.path)?;
    ensure!(sha256(&bytes) == binding.sha256, "Changed bound input");
    Ok(bytes)
}
fn log_add(a: f64, b: f64) -> f64 {
    let maximum = a.max(b);
    maximum + ((a - maximum).exp() + (b - maximum).exp()).ln()
}
fn sigmoid(x: f64) -> f64 {
    if x >= 0.0 {
        1.0 / (1.0 + (-x).exp())
    } else {
        let e = x.exp();
        e / (1.0 + e)
    }
}
fn scales(q: [f64; 3], temperature: f64) -> Result<Scales> {
    ensure!(
        temperature.is_finite() && temperature > 0.0,
        "Invalid temperature"
    );
    ensure!(
        q.iter().all(|p| p.is_finite() && *p > 0.0 && *p < 1.0)
            && (q.iter().sum::<f64>() - 1.0).abs() <= 1e-6,
        "Cannot invert missing, zero, saturated or invalid probabilities; no clipping"
    );
    let (model, mixed) = (
        temperature * (q[1].ln() - q[0].ln()),
        temperature * (q[2].ln() - q[0].ln()),
    );
    let p = q[1] + q[2];
    let result = [p, p.ln() - q[0].ln(), log_add(model, mixed), model, mixed];
    ensure!(result.iter().all(|x| x.is_finite()), "Nonfinite inverse");
    Ok(result)
}
fn additive(values: &[Scales], mask: usize) -> Scales {
    let mut prediction = values[0];
    for bit in 0..values.len().ilog2() {
        let singleton = 1usize << bit;
        if mask & singleton != 0 {
            for (i, value) in prediction.iter_mut().enumerate() {
                *value += values[singleton][i] - values[0][i];
            }
        }
    }
    prediction
}
fn interactions(values: &[Scales]) -> Vec<Scales> {
    let mut coefficients = values.to_vec();
    let mut bit = 1;
    while bit < values.len() {
        for mask in 0..values.len() {
            if mask & bit != 0 {
                let lower = coefficients[mask ^ bit];
                for (value, lower) in coefficients[mask].iter_mut().zip(lower) {
                    *value -= lower;
                }
            }
        }
        bit <<= 1;
    }
    coefficients
}
fn named(values: Scales) -> Value {
    json!(
        SCALE_NAMES
            .into_iter()
            .zip(values)
            .collect::<BTreeMap<_, _>>()
    )
}
fn strings(value: &Value) -> Result<Vec<String>> {
    value
        .as_array()
        .context("Expected array")?
        .iter()
        .map(|v| v.as_str().map(str::to_owned).context("Expected string"))
        .collect()
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a new output path");
    let config_bytes = fs::read(&args.config)?;
    let config: Config = serde_json::from_slice(&config_bytes)?;
    let n = config.choices.len();
    ensure!((1..=8).contains(&n), "Expected 1-8 edit dimensions");
    let choice_bits: BTreeMap<_, _> = config
        .choices
        .iter()
        .enumerate()
        .map(|(i, choice)| (choice.as_str(), 1usize << i))
        .collect();
    ensure!(
        choice_bits.len() == n && config.choices.iter().all(|s| !s.is_empty()),
        "Invalid choice IDs"
    );
    let plan: Value = serde_json::from_slice(&load(&config.plan)?)?;
    let score_bytes = load(&config.scores)?;
    let ranking: Value = serde_json::from_slice(&load(&config.ranking)?)?;
    let manifest: Value = serde_json::from_slice(&load(&config.artifact_manifest)?)?;
    let calibration: Value = serde_json::from_slice(&load(&config.calibration)?)?;
    load(&config.protocol)?;
    ensure!(
        manifest["artifact_id"] == ARTIFACT
            && manifest["files"]["calibration.json"] == config.calibration.sha256
            && plan["input"]["detector_artifact_id"] == ARTIFACT,
        "Wrong artifact or calibration"
    );
    ensure!(
        plan["schema"] == "slop_ninja_b_edit_menu_plan_v1"
            && ranking["plan_sha256"] == config.plan.sha256
            && ranking["scores_sha256"] == config.scores.sha256,
        "Wrong plan/ranking ancestry"
    );
    let temperature = calibration["temperature"]
        .as_f64()
        .context("Missing temperature")?;
    let mut scores = BTreeMap::new();
    for line in std::str::from_utf8(&score_bytes)?.lines() {
        let row: Value = serde_json::from_str(line)?;
        let id = row["id"]
            .as_str()
            .context("Missing response ID")?
            .to_owned();
        ensure!(scores.insert(id, row).is_none(), "Duplicate score");
    }
    let states = plan["states"].as_array().context("Missing states")?;
    let ranked = ranking["ranked_states"]
        .as_array()
        .context("Missing ranked states")?;
    let count = 1usize << n;
    ensure!(
        states.len() == count && scores.len() == count && ranked.len() == count,
        "Incomplete edit cube"
    );
    let mut values = vec![[0.0; 5]; count];
    let mut state_ids = vec![None; count];
    for state in states {
        let text = state["text"].as_str().context("Missing text")?;
        let id = state["id"].as_str().context("Missing state ID")?;
        ensure!(sha256(text) == id, "Changed text");
        let mut mask = 0;
        let choices = strings(&state["choices"])?;
        for choice in &choices {
            let &bit = choice_bits.get(choice.as_str()).context("Unknown edit")?;
            ensure!(mask & bit == 0, "Repeated edit");
            mask |= bit;
        }
        ensure!(
            state_ids[mask].replace(id.to_owned()).is_none(),
            "Repeated subset"
        );
        let response = scores.remove(id).context("Missing state score")?;
        ensure!(
            response["artifact_id"] == ARTIFACT
                && response["runtime"] == "cpu_float32_eager"
                && response["status"] == "ok"
                && response["labels"] == json!(["human_only", "model_only", "mixed"]),
            "Unsupported score or runtime"
        );
        let probabilities: [f64; 3] = serde_json::from_value(response["probabilities"].clone())?;
        let observed = scales(probabilities, temperature)?;
        ensure!(
            (response["model_or_assistance_probability"]
                .as_f64()
                .context("Missing probability")?
                - observed[0])
                .abs()
                < 1e-12,
            "Union probability mismatch"
        );
        let prior: Vec<_> = ranked.iter().filter(|r| r["id"] == id).collect();
        ensure!(
            prior.len() == 1
                && strings(&prior[0]["choices"])? == choices
                && (prior[0]["a_probability"]
                    .as_f64()
                    .context("Missing ranked probability")?
                    - observed[0])
                    .abs()
                    < 1e-12,
            "Changed prior ranking"
        );
        values[mask] = observed;
    }
    ensure!(
        scores.is_empty() && state_ids.iter().all(Option::is_some),
        "Incomplete unique coverage"
    );
    let coefficients = interactions(&values);
    let rows: Vec<_> = values.iter().enumerate().map(|(mask, observed)| {
        let predicted = if mask == 0 { values[0] } else { additive(&values, mask) };
        let residual = std::array::from_fn(|i| observed[i] - predicted[i]);
        json!({"mask":mask,"text_sha256":state_ids[mask],
            "choices":config.choices.iter().enumerate().filter(|(i,_)|mask&(1usize<<*i)!=0).map(|(_,c)|c).collect::<Vec<_>>(),
            "observed":named(*observed),"single_edit_prediction":named(predicted),
            "residual":named(residual),"interaction_coefficient":named(coefficients[mask]),
            "probability_from_additive_class_logits":sigmoid(log_add(predicted[3]/temperature,predicted[4]/temperature))})
    }).collect();
    let mut effects = Vec::new();
    for (i, choice) in config.choices.iter().enumerate() {
        let bit = 1usize << i;
        let differences: Vec<Scales> = (0..count)
            .filter(|mask| mask & bit == 0)
            .map(|mask| std::array::from_fn(|j| values[mask | bit][j] - values[mask][j]))
            .collect();
        effects.push(json!({"choice":choice,"contexts":differences.len(),
            "minimum_delta":named(std::array::from_fn(|j|differences.iter().map(|v|v[j]).fold(f64::INFINITY,f64::min))),
            "maximum_delta":named(std::array::from_fn(|j|differences.iter().map(|v|v[j]).fold(f64::NEG_INFINITY,f64::max)))}));
    }
    for binding in [
        &config.plan,
        &config.scores,
        &config.ranking,
        &config.artifact_manifest,
        &config.calibration,
        &config.protocol,
    ] {
        load(binding)?;
    }
    ensure!(
        fs::read(&args.config)? == config_bytes,
        "Config changed during audit"
    );
    let result = json!({"schema":"slop_ninja_edit_cube_interaction_audit_v1","artifact_id":ARTIFACT,
        "temperature":temperature,"dimensions":n,"states":count,"rows":rows,"contextual_effect_ranges":effects,
        "bindings":{"config":sha256(config_bytes),"plan":config.plan.sha256,"scores":config.scores.sha256,
            "ranking":config.ranking.sha256,"artifact_manifest":config.artifact_manifest.sha256,
            "calibration":config.calibration.sha256,"protocol":config.protocol.sha256},
        "model_calls":0,"provider_calls":0,"training_admission":false,
        "limitations":"Retrospective, adaptively selected B development menu. Logit differences are recovered from recorded softmax probabilities and temperature up to floating-point error. Coefficients describe this finite cube, not a population or causal linguistic attribution. No new source, editorial approval, detector training or Pangram observation."});
    let mut file = fs::File::create_new(&args.out)?;
    serde_json::to_writer_pretty(&mut file, &result)?;
    file.write_all(b"\n")?;
    println!(
        "{}",
        json!({"status":"audited","states":count,"model_calls":0})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn from_logits(model: f64, mixed: f64, temperature: f64) -> Scales {
        let scores = [0.0, model / temperature, mixed / temperature];
        let denominator = scores.iter().map(|x| x.exp()).sum::<f64>();
        scales(scores.map(|x| x.exp() / denominator), temperature).unwrap()
    }
    #[test]
    fn nonlinear_probability_can_coexist_with_additive_class_logits() {
        let values: Vec<_> = (0..4)
            .map(|mask| {
                let a = if mask & 1 != 0 { 1.0 } else { 0.0 };
                let b = if mask & 2 != 0 { 1.0 } else { 0.0 };
                from_logits(2.0 - 2.0 * a - b, 0.5 + 0.7 * a - 3.0 * b, 1.8)
            })
            .collect();
        let beta = interactions(&values);
        assert!(beta[3][0].abs() > 0.001);
        assert!(beta[3][3].abs() < 1e-12 && beta[3][4].abs() < 1e-12);
        let predicted = additive(&values, 3);
        assert!(
            (sigmoid(log_add(predicted[3] / 1.8, predicted[4] / 1.8)) - values[3][0]).abs() < 1e-12
        );
    }
    #[test]
    fn recovers_injected_third_order_effect_without_clipping() {
        let values: Vec<_> = (0..8usize)
            .map(|mask| {
                let shift = mask.count_ones() as f64 * 0.2;
                let effect = if mask == 7 { -4.0 } else { 0.0 };
                from_logits(1.0 + shift + effect, 0.5 - shift + effect / 2.0, 1.5)
            })
            .collect();
        let beta = interactions(&values);
        assert!((beta[7][3] + 4.0).abs() < 1e-12 && (beta[7][4] + 2.0).abs() < 1e-12);
        assert!(scales([0.0, 0.6, 0.4], 1.0).is_err());
        assert!(scales([0.2, 0.3, 0.5], f64::NAN).is_err());
    }
}
