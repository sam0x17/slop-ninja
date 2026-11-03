//! Frozen numeric Test inference into a new, separate receipt directory.
//! predict_final_test NUMERIC FIT SELECTION NEW_OUTPUT_DIRECTORY
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::{shared::SharedInput, warm_shared::NumericSharedModel};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, time::Instant};
#[path = "predict_final_test/binding.rs"]
mod binding;
#[allow(dead_code)]
#[path = "final_test_binding/mod.rs"]
mod final_test_binding;
#[allow(dead_code)]
#[path = "compare_supervised_pangram/test_binding.rs"]
mod test_binding;
#[cfg(test)]
#[path = "predict_final_test/tests.rs"]
mod tests;

struct Outcomes {
    rows: Vec<Value>,
    excluded: Vec<Value>,
    encoding_failures: Vec<Value>,
}
// Callback boundary keeps failure accounting testable without fitted artifacts.
fn infer(
    rows: &BTreeMap<String, Value>,
    mut predict: impl FnMut(&Value) -> Result<(Value, Option<String>)>,
) -> Outcomes {
    let (mut accepted, mut excluded, mut encoding_failures) = (Vec::new(), Vec::new(), Vec::new());
    for (id, row) in rows {
        let start = Instant::now();
        match predict(row) {
            Ok((prediction,encoding_error))=>{
                if let Some(error)=encoding_error {encoding_failures.push(json!({"id":id,"reason":"query_encoding_failed","error":error}));}
                accepted.push(json!({"id":id,"prediction":prediction,"elapsed_numeric_ns":start.elapsed().as_nanos()}));
            },
            Err(error)=>excluded.push(json!({"id":id,"reason":"numeric_inference_failed","stage":"shared_model","error":format!("{error:#}")})),
        }
    }
    Outcomes {
        rows: accepted,
        excluded,
        encoding_failures,
    }
}
fn predict_row(model: &NumericSharedModel, row: &Value) -> Result<(Value, Option<String>)> {
    let input: SharedInput =
        serde_json::from_value(row["input"].clone()).context("numeric input decoding")?;
    let mut p = serde_json::to_value(model.predict(&input)?)?;
    let encoding_error = match model.encode(&input) {
        Ok(z) if !z.is_empty() && z.iter().all(|x| x.is_finite()) => {
            p["encoding"] = json!(z);
            None
        }
        Ok(_) => Some("nonfinite/empty query encoding".into()),
        Err(e) => Some(format!("{e:#}")),
    };
    Ok((p, encoding_error))
}
fn run(a: &[String]) -> Result<()> {
    ensure!(
        a.len() == 4,
        "predict_final_test NUMERIC FIT SELECTION NEW_OUTPUT_DIRECTORY"
    );
    let (numeric, fit, selection_path, out) = (
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
    );
    let admitted = binding::admit(numeric, fit, selection_path)?;
    binding::new_output(out, &admitted.selection, numeric)?;
    let start = Instant::now();
    let model = NumericSharedModel::from_json(&admitted.weight_bytes);
    let load_us = start.elapsed().as_micros();
    let result = infer(&admitted.data.numeric, |row| match &model {
        Ok(m) => predict_row(m, row),
        Err(e) => anyhow::bail!("frozen numeric model load failed: {e:#}"),
    });
    let output = serde_json::to_vec_pretty(&result.rows)?;
    let receipt = receipt(
        &admitted,
        &result,
        &output,
        load_us,
        model
            .as_ref()
            .ok()
            .map(NumericSharedModel::numeric_weight_bytes),
    );
    binding::unchanged(&admitted, numeric, fit, selection_path)?;
    fs::create_dir(out)?;
    // Publish the complete receipt only after its prediction array is durable.
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(out.join("predictions.json"))?;
    use std::io::Write;
    f.write_all(&output)?;
    f.sync_all()?;
    test_binding::write_new(&out.join("receipt.json"), &receipt)?;
    fs::File::open(out)?.sync_all()?;
    println!(
        "{}",
        json!({"requested":admitted.data.numeric.len(),"supported":result.rows.len(),"failed":result.excluded.len(),"receipt":out.join("receipt.json")})
    );
    Ok(())
}
fn receipt(
    a: &binding::Admitted,
    r: &Outcomes,
    bytes: &[u8],
    load_us: u128,
    numeric_bytes: Option<usize>,
) -> Value {
    json!({"schema":"slop_ninja_final_test_predictions_v1","complete":true,"split":"test","fit_performed":false,
        "selection_receipt_sha256":a.data.selection_sha,"numeric_manifest_sha256":a.data.manifest_sha,"query_sha256":a.data.query_sha,
        "contract_sha256":a.data.contract_sha,"weights_sha256":a.run["weights_sha256"],"fit_sha256":a.run["fit_sha256"],
        "candidate":a.candidate["candidate"],"seed":a.run["seed"],"predictions_file":"predictions.json","predictions_sha256":test_binding::digest(bytes),
        "requested":a.data.numeric.len(),"supported":r.rows.len(),"excluded":r.excluded,"encoding_failures":r.encoding_failures,
        "all_failed":!a.data.numeric.is_empty() && r.rows.is_empty(),"upstream":a.data.upstream,
        "assembled_queries":a.data.rows.len(),"projection_excluded":a.data.projection_excluded,
        "runtime":"NumericSharedModel pure Rust CPU numeric heads","model_load_microseconds":load_us,"numeric_weight_bytes":numeric_bytes,
        "encoding":"same frozen NumericSharedModel::encode; no pooling/normalization added; diagnostic export failure does not discard successful all-head prediction",
        "no_prose_or_provider_calls":true,"frozen_parents_modified":false,
        "implementation_sha256":test_binding::digest(include_bytes!("predict_final_test.rs")),
        "binding_helper_sha256":test_binding::digest(include_bytes!("predict_final_test/binding.rs")),
        "runtime_source_sha256":{"shared":test_binding::digest(include_bytes!("../src/shared.rs")),"warm":test_binding::digest(include_bytes!("../src/warm_shared.rs"))},
        "limits":["Numeric head costs only; parsing and full V6 text encoding excluded","Execution failure is separate from attribution fallback","No Test fitting, selection, threshold adjustment or model changes"]})
}
fn main() -> Result<()> {
    run(&std::env::args().skip(1).collect::<Vec<_>>())
}
