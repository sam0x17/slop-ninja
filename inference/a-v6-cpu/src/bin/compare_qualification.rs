//! Offline comparison only. Reads synthetic numerical receipts, never corpus files.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Write, path::Path};
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn scalar(v: &Value) -> Result<f64> {
    let x = v.as_f64().context("number")?;
    ensure!(x.is_finite(), "nonfinite result");
    Ok(x)
}
fn delta(a: &Value, b: &Value) -> Result<f64> {
    Ok((scalar(a)? - scalar(b)?).abs())
}
fn tensor(a: &Value, b: &Value, n: usize) -> Result<f64> {
    let a = a.as_array().context("tensor")?;
    let b = b.as_array().context("tensor")?;
    ensure!(a.len() == n && b.len() == n, "tensor dimension");
    a.iter()
        .zip(b)
        .try_fold(0.0f64, |m, (a, b)| Ok(m.max(delta(a, b)?)))
}
fn prediction(a: &Value, b: &Value) -> Result<f64> {
    ensure!(
        a.as_object()
            .context("prediction")?
            .keys()
            .eq(b.as_object().context("reference prediction")?.keys()),
        "prediction fields"
    );
    let mut max = 0.0f64;
    for k in ["ai_confidence_percent", "ai_share_percent"] {
        max = max.max(delta(&a[k], &b[k])? / 100.);
    }
    for k in ["human_only", "model_only", "mixed"] {
        max = max.max(delta(&a["origin"][k], &b["origin"][k])?);
    }
    let p = &a["origin"];
    let share = scalar(&a["ai_share_percent"])? / 100.;
    ensure!(
        scalar(&p["model_only"])? <= share + 1e-12
            && share <= scalar(&p["model_only"])? + scalar(&p["mixed"])? + 1e-12,
        "share bounds"
    );
    for role in ["human", "ai"] {
        let (a, b) = (&a["attribution"][role], &b["attribution"][role]);
        ensure!(
            a["profile"] == b["profile"]
                && a["fallback_asserts_presence"] == false
                && b["fallback_asserts_presence"] == false,
            "profile/fallback differs"
        );
        max = max.max(delta(
            &a["involvement_probability"],
            &b["involvement_probability"],
        )?);
        let (a, b) = (
            a["conditional_named_scores"]
                .as_object()
                .context("scores")?,
            b["conditional_named_scores"]
                .as_object()
                .context("scores")?,
        );
        ensure!(a.keys().eq(b.keys()), "profile roster differs");
        for (key, value) in a {
            max = max.max(delta(value, &b[key])?);
        }
    }
    Ok(max)
}
fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        args.len() == 3,
        "compare_qualification ORACLE.json RUST_OUTPUT NEW_REPORT.json"
    );
    let oracle = read(Path::new(&args[0]))?;
    let dir = Path::new(&args[1]);
    let receipt = read(&dir.join("receipt.json"))?;
    ensure!(
        oracle["schema"] == "slop_ninja_a_v6_cpu_synthetic_oracle_v1"
            && oracle["synthetic_only"] == true
            && receipt["complete"] == true,
        "unqualified receipt"
    );
    for (ok, rk) in [
        ("requests_sha256", "requests_file_sha256"),
        ("weights_sha256", "weights_file_sha256"),
        ("contract_file_sha256", "contract_file_sha256"),
        ("encoder_manifest_sha256", "encoder_manifest_sha256"),
    ] {
        ensure!(
            oracle[ok] == receipt["bindings"][rk],
            "input binding differs"
        );
    }
    let rb = fs::read(dir.join("predictions.jsonl"))?;
    ensure!(
        sha(&rb) == receipt["predictions_sha256"].as_str().context("hash")?,
        "predictions checksum"
    );
    let mut rows = BTreeMap::new();
    for line in std::str::from_utf8(&rb)?.lines() {
        let r: Value = serde_json::from_str(line)?;
        ensure!(
            rows.insert(r["id"].as_str().context("id")?.to_owned(), r)
                .is_none(),
            "duplicate ID"
        );
    }
    let refs = oracle["rows"].as_array().context("oracle rows")?;
    ensure!(refs.len() == rows.len(), "roster length");
    let mut all = Vec::new();
    let mut failed = Vec::new();
    let mut max_a = 0.0f64;
    let mut max_v6 = 0.0f64;
    let mut max_h = 0.0f64;
    let mut max_z = 0.0f64;
    let mut max_logits = 0.0f64;
    let mut lengths = BTreeMap::<usize, Vec<Value>>::new();
    let mut unsupported = 0;
    for reference in refs {
        let id = reference["id"].as_str().context("reference id")?;
        let actual = rows.remove(id).context("missing reference ID")?;
        let result = (|| -> Result<Value> {
            for key in ["text_sha256", "status", "reason", "token_count"] {
                ensure!(
                    actual[key] == reference[key],
                    "request/status mismatch {key}"
                );
            }
            ensure!(
                actual["bindings"] == receipt["bindings"]
                    && actual["truncation"] == false
                    && actual["zero_cfg_coordinates"] == 1820,
                "row binding"
            );
            ensure!(
                actual["diagnostics"]["token_ids"] == reference["token_ids"],
                "token IDs differ"
            );
            if reference["status"] != "ok" {
                unsupported += 1;
                ensure!(
                    actual["prediction"].is_null(),
                    "unsupported prediction emitted"
                );
                return Ok(json!({"id":id,"passed":true,"unsupported":true}));
            }
            let a = prediction(&actual["prediction"], &reference["prediction"])?;
            let h = tensor(
                &actual["diagnostics"]["h_raw_f32"],
                &reference["h_raw_f32"],
                768,
            )?;
            let z = tensor(
                &actual["diagnostics"]["z_shared_f64"],
                &reference["z_shared_f32"],
                768,
            )?;
            let v6 = tensor(
                &actual["diagnostics"]["v6_probabilities_f64"],
                &reference["v6_probabilities_f64"],
                3,
            )?;
            let logits = tensor(
                &actual["diagnostics"]["v6_logits_f32"],
                &reference["v6_logits_f32"],
                3,
            )?;
            max_a = max_a.max(a);
            max_h = max_h.max(h);
            max_z = max_z.max(z);
            max_v6 = max_v6.max(v6);
            max_logits = max_logits.max(logits);
            lengths
                .entry(actual["token_count"].as_u64().context("length")? as usize)
                .or_default()
                .push(actual["timing_seconds"].clone());
            Ok(
                json!({"id":id,"passed":a<=1e-5&&h<=1e-4&&z<=1e-3&&v6<=1e-6,"A_max_probability_error":a,
                "h_max_coordinate_error":h,"z_max_coordinate_error":z,"V6_max_probability_error":v6,"V6_max_logit_error":logits}),
            )
        })();
        let row = match result {
            Ok(v) => v,
            Err(e) => json!({"id":id,"passed":false,"error":e.to_string()}),
        };
        if row["passed"] != true {
            failed.push(id.to_owned());
        }
        all.push(row);
    }
    let report = json!({"schema":"slop_ninja_a_v6_cpu_qualification_v1","synthetic_only":true,"passed":failed.is_empty(),"cases":refs.len(),
        "supported":refs.len()-unsupported,"unsupported":unsupported,"failed_ids":failed,
        "maxima":{"A_probability":max_a,"A_percentage_points":max_a*100.,"raw_h":max_h,"shared_z":max_z,"V6_probability":max_v6,"V6_logits":max_logits},
        "tolerances":{"A_probability":1e-5,"raw_h":1e-4,"shared_z":1e-3,"V6_probability":1e-6},
        "all_outputs_and_1536_coordinates_checked":true,"timing_by_token_length":lengths,"cases_detail":all,
        "oracle_sha256":sha(&fs::read(&args[0])?),"runtime_receipt_sha256":sha(&fs::read(dir.join("receipt.json"))?),"runtime_receipt":receipt});
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&args[2])?;
    serde_json::to_writer_pretty(&mut f, &report)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!(
        "{}",
        json!({"passed":failed.is_empty(),"cases":refs.len(),"maxima":report["maxima"]})
    );
    ensure!(
        failed.is_empty(),
        "qualification failed; complete diagnostics retained"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tensors_reject_dimension_and_compare_every_coordinate() {
        assert_eq!(tensor(&json!([0., 2.]), &json!([0., 3.]), 2).unwrap(), 1.);
        assert!(tensor(&json!([0.]), &json!([0., 1.]), 2).is_err());
    }
}
