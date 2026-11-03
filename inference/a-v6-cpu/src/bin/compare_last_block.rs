//! Compare the changed encoder's full Rust inference with its synthetic oracle.
//! EXPORT_DIR RUST_REFERENCE_RUN NEW_REPORT.json; no model/provider calls.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

fn sha(path: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(path)?)))
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn number(value: &Value) -> Result<f64> {
    let x = value.as_f64().context("numeric coordinate")?;
    ensure!(x.is_finite(), "nonfinite coordinate");
    Ok(x)
}
fn vector(a: &Value, b: &Value) -> Result<f64> {
    let a = a.as_array().context("array")?;
    let b = b.as_array().context("array")?;
    ensure!(a.len() == 768 && b.len() == 768, "encoding dimension");
    a.iter().zip(b).try_fold(0.0_f64, |m, (a, b)| {
        Ok(m.max((number(a)? - number(b)?).abs()))
    })
}
fn prediction(a: &Value, b: &Value) -> Result<f64> {
    let mut error = 0.0_f64;
    for key in ["ai_confidence_percent", "ai_share_percent"] {
        error = error.max((number(&a[key])? - number(&b[key])?).abs() / 100.);
    }
    let mut total = 0.;
    for key in ["human_only", "model_only", "mixed"] {
        let p = number(&a["origin"][key])?;
        ensure!((0.0..=1.).contains(&p), "probability bounds");
        total += p;
        error = error.max((p - number(&b["origin"][key])?).abs());
    }
    ensure!((total - 1.).abs() < 1e-12, "origin sum");
    let low = number(&a["origin"]["model_only"])?;
    let high = low + number(&a["origin"]["mixed"])?;
    let share = number(&a["ai_share_percent"])? / 100.;
    ensure!(
        low <= share + 1e-12 && share <= high + 1e-12,
        "share constraints"
    );
    ensure!(
        (number(&a["ai_confidence_percent"])? / 100. - high).abs() < 1e-12,
        "confidence meaning"
    );
    for role in ["human", "ai"] {
        let x = &a["attribution"][role];
        let y = &b["attribution"][role];
        ensure!(
            x["profile"] == y["profile"]
                && x["fallback_asserts_presence"] == false
                && y["fallback_asserts_presence"] == false,
            "role/fallback mismatch"
        );
        error = error.max(
            (number(&x["involvement_probability"])? - number(&y["involvement_probability"])?).abs(),
        );
        let x = x["conditional_named_scores"]
            .as_object()
            .context("named scores")?;
        let y = y["conditional_named_scores"]
            .as_object()
            .context("named scores")?;
        ensure!(x.keys().eq(y.keys()), "named roster differs");
        for (key, value) in x {
            error = error.max((number(value)? - number(&y[key])?).abs());
        }
    }
    Ok(error)
}
fn run(export: &Path, run: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new report required");
    let m = read(&export.join("manifest.json"))?;
    let receipt = read(&run.join("receipt.json"))?;
    ensure!(
        m["schema"] == "slop_ninja_last_block_a_export_v1"
            && m["complete"] == true
            && receipt["schema"] == "slop_ninja_last_block_rust_run_v1"
            && receipt["complete"] == true,
        "completed export and inference required"
    );
    ensure!(
        receipt["bindings"]["export_manifest_sha256"] == sha(&export.join("manifest.json"))?
            && receipt["bindings"]["requests_sha256"] == m["reference_requests"]["sha256"]
            && receipt["calibrated"] == m["calibrated"],
        "runtime/export lineage mismatch"
    );
    let refs = export.join("reference-predictions.json");
    ensure!(
        m["reference_predictions"]["file"] == "reference-predictions.json"
            && m["reference_predictions"]["sha256"] == sha(&refs)?,
        "oracle changed"
    );
    let mut expected = BTreeMap::new();
    let reference = read(&refs)?;
    for r in reference.as_array().context("oracle array")? {
        let id = r["id"].as_str().context("reference id")?;
        ensure!(
            expected.insert(id.to_owned(), r).is_none(),
            "duplicate reference"
        );
    }
    let actual = run.join("predictions.jsonl");
    ensure!(
        receipt["predictions_sha256"] == sha(&actual)?,
        "runtime predictions changed"
    );
    let mut h_error = 0.0_f64;
    let mut z_error = 0.0_f64;
    let mut output_error = 0.0_f64;
    let count = expected.len();
    let expected_supported = expected.values().filter(|e| e.get("status").is_none_or(|s| s == "ok")).count();
    let mut per_case = Vec::new();
    for line in fs::read_to_string(&actual)?.lines() {
        let r: Value = serde_json::from_str(line)?;
        let e = expected
            .remove(r["id"].as_str().context("runtime id")?)
            .context("unexpected/duplicate runtime id")?;
        ensure!(
            r["text_sha256"] == e["text_sha256"]
                && r["truncation"] == false,
            "text/token/status mismatch"
        );
        if e["status"] == "unsupported_input" {
            let ids = e["token_ids"].as_array().context("unsupported token IDs")?.iter()
                .map(|v| u32::try_from(v.as_u64().context("token id")?).map_err(anyhow::Error::from))
                .collect::<Result<Vec<_>>>()?;
            let bytes=ids.iter().flat_map(|v|v.to_le_bytes()).collect::<Vec<_>>();
            ensure!(r["status"] == "unsupported_input" && r["reason"] == e["reason"]
                && r["prediction"].is_null() && r["token_count"] == ids.len()
                && r["token_ids_sha256_le_u32"] == hex::encode(Sha256::digest(bytes)),
                "unsupported boundary must retain exact status and token count");
            per_case.push(json!({"id":r["id"],"unsupported_status_matches":true}));
            continue;
        }
        ensure!(r["status"] == "ok" && r["diagnostics"]["token_ids"] == e["token_ids"],"supported token/status mismatch");
        per_case.push(json!({"id":r["id"],"tokens":r["token_count"],
            "raw_h":vector(&r["diagnostics"]["h_raw_f32"], &e["h_raw_f32"])?,
            "shared_z":vector(&r["diagnostics"]["z_shared_f64"], &e["prediction"]["encoding"])?,
            "outputs_on_0_1_scale":prediction(&r["prediction"], &e["prediction"])?}));
        h_error = h_error.max(vector(&r["diagnostics"]["h_raw_f32"], &e["h_raw_f32"])?);
        z_error = z_error.max(vector(
            &r["diagnostics"]["z_shared_f64"],
            &e["prediction"]["encoding"],
        )?);
        output_error = output_error.max(prediction(&r["prediction"], &e["prediction"])?);
    }
    ensure!(
        count > 0
            && expected.is_empty()
            && receipt["requested"] == count
            && receipt["supported"] == expected_supported
            && receipt["unsupported"] == count - expected_supported,
        "reference population incomplete"
    );
    let passed = h_error <= 1e-4 && z_error <= 1e-3 && output_error <= 1e-5;
    let report = json!({"schema":"slop_ninja_last_block_rust_comparison_v1","complete":true,"passed":passed,
        "synthetic_cases":count,"supported_cases":expected_supported,"per_case":per_case,
        "condition":m["condition"],"seed":m["seed"],"selected_epoch":m["selected_epoch"],
        "maximum_absolute_error":{"raw_h":h_error,"shared_z":z_error,"outputs_on_0_1_scale":output_error},
        "fixed_gates":{"raw_h":1e-4,"shared_z":1e-3,"outputs_on_0_1_scale":1e-5},
        "bindings":{"export":sha(&export.join("manifest.json"))?,"runtime":sha(&run.join("receipt.json"))?,
            "oracle":sha(&refs)?,"implementation":hex::encode(Sha256::digest(include_bytes!("compare_last_block.rs")))},
        "limits":["Small synthetic numerical replay; not detection quality or a broad runtime qualification.",
            "Workers/Wasm and Cloudflare Containers remain unqualified."]});
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)?;
    serde_json::to_writer_pretty(&mut f, &report)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!(
        "{}",
        json!({"passed":passed,"cases":count,"errors":report["maximum_absolute_error"]})
    );
    ensure!(passed, "changed-weight Rust numeric qualification failed");
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 3,
        "EXPORT_DIR RUST_REFERENCE_RUN NEW_REPORT.json"
    );
    run(Path::new(&a[0]), Path::new(&a[1]), Path::new(&a[2]))
}
