use anyhow::{Context, Result, ensure};
use grammar_a_baseline::shared::SharedInput;
use grammar_a_baseline::warm_shared::NumericSharedModel as SharedModel;
use serde_json::{Value, json};
use std::{fs, time::Instant};
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 4,
        "shared_inference WEIGHTS.json NUMERIC_ROWS.jsonl REFERENCE_PREDICTIONS.json NEW_OUTPUT.json"
    );
    let load = Instant::now();
    let bytes = fs::read(&a[0])?;
    let model = SharedModel::from_json(&bytes)?;
    let load_us = load.elapsed().as_micros();
    let rows = fs::read_to_string(&a[1])?
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<Vec<Value>, _>>()?;
    let refs: Vec<Value> = serde_json::from_slice(&fs::read(&a[2])?)?;
    let mut output = Vec::new();
    let mut inputs = Vec::new();
    let mut max_delta: f64 = 0.;
    for row in rows {
        let input: SharedInput = serde_json::from_value(row["input"].clone())?;
        let prediction = serde_json::to_value(model.predict(&input)?)?;
        let reference = &refs
            .iter()
            .find(|r| r["id"] == row["id"])
            .context("reference absent")?["prediction"];
        for key in ["ai_confidence_percent", "ai_share_percent"] {
            max_delta = max_delta
                .max((prediction[key].as_f64().unwrap() - reference[key].as_f64().unwrap()).abs());
        }
        for key in ["human_only", "model_only", "mixed"] {
            max_delta = max_delta.max(
                (prediction["origin"][key].as_f64().unwrap()
                    - reference["origin"][key].as_f64().unwrap())
                .abs()
                    * 100.,
            );
        }
        for role in ["human", "ai"] {
            let actual = &prediction["attribution"][role];
            let expected = &reference["attribution"][role];
            ensure!(
                actual["profile"] == expected["profile"],
                "Rust/PyTorch conditional profile differs for {role}"
            );
            ensure!(
                actual["fallback_asserts_presence"] == false
                    && expected["fallback_asserts_presence"] == false,
                "fallback presence contract"
            );
            max_delta = max_delta.max(
                (actual["involvement_probability"]
                    .as_f64()
                    .context("role involvement")?
                    - expected["involvement_probability"]
                        .as_f64()
                        .context("reference involvement")?)
                .abs()
                    * 100.,
            );
            let scores = actual["conditional_named_scores"]
                .as_object()
                .context("conditional scores")?;
            let refs = expected["conditional_named_scores"]
                .as_object()
                .context("reference conditional scores")?;
            ensure!(
                scores.keys().eq(refs.keys()),
                "named profile roster differs"
            );
            for (name, score) in scores {
                max_delta = max_delta.max(
                    (score.as_f64().context("named score")?
                        - refs[name].as_f64().context("reference named score")?)
                    .abs()
                        * 100.,
                );
            }
        }
        output.push(json!({"id":row["id"],"prediction":prediction}));
        inputs.push(input);
    }
    ensure!(
        max_delta < 0.001,
        "Rust/PyTorch disagreement {max_delta}percentage points"
    );
    let mut times = Vec::new();
    for i in 0..1000 {
        let start = Instant::now();
        std::hint::black_box(model.predict(std::hint::black_box(&inputs[i % inputs.len()]))?);
        times.push(start.elapsed().as_nanos() as u64);
    }
    times.sort_unstable();
    ensure!(!std::path::Path::new(&a[3]).exists(), "output exists");
    fs::write(
        &a[3],
        serde_json::to_vec_pretty(
            &json!({"predictions":output,"max_absolute_delta_percentage_points":max_delta,"latency":{"repetitions":1000,"p50_ns":times[499],"p95_ns":times[949],"p99_ns":times[989],"artifact_load_us":load_us,"scope":"numeric_head_only_not_parsing_or_resolution"},"artifact_json_bytes":bytes.len(),"loaded_numeric_weight_bytes":model.numeric_weight_bytes(),"runtime":"pure_Rust_f64_cpu"}),
        )?,
    )?;
    println!("{}", a[3]);
    Ok(())
}
