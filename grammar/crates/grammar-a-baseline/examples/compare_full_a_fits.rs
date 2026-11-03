//! Determinism check between two full-A fits of the same arm, seed and bundle.
//! FIT_A FIT_B NEW_REPORT.json
//!
//! Requires identical fit bindings (bundle, protocol, code, seed, device). It
//! then compares the selected checkpoint, exported weights, every history row
//! (excluding wall time) and every exported prediction. `bitwise_identical` is
//! true only when all retained artifacts hash identically; otherwise the
//! largest numeric prediction difference is reported. Exit status 1 when the
//! fits are not bitwise identical (the report is retained).
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn sha(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(p)?)))
}

/// Largest absolute difference between numeric leaves; Err on any structural or
/// non-numeric difference.
fn numeric_diff(a: &Value, b: &Value, path: &str) -> Result<f64> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            Ok((x.as_f64().context("number")? - y.as_f64().context("number")?).abs())
        }
        (Value::Array(x), Value::Array(y)) => {
            ensure!(x.len() == y.len(), "array length differs at {path}");
            x.iter()
                .zip(y)
                .enumerate()
                .try_fold(0f64, |m, (i, (p, q))| {
                    Ok(m.max(numeric_diff(p, q, &format!("{path}[{i}]"))?))
                })
        }
        (Value::Object(x), Value::Object(y)) => {
            ensure!(
                x.keys().eq(y.keys()),
                "object keys differ at {path}"
            );
            x.iter().try_fold(0f64, |m, (k, p)| {
                Ok(m.max(numeric_diff(p, &y[k], &format!("{path}.{k}"))?))
            })
        }
        _ => {
            ensure!(a == b, "non-numeric difference at {path}");
            Ok(0.)
        }
    }
}

fn predictions(p: &Path) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for line in fs::read_to_string(p)?.lines() {
        let v: Value = serde_json::from_str(line)?;
        let id = v["id"].as_str().context("id")?.to_owned();
        ensure!(out.insert(id, v).is_none(), "duplicate prediction id");
    }
    Ok(out)
}

fn main() -> Result<()> {
    let a = std::env::args_os().skip(1).collect::<Vec<_>>();
    ensure!(a.len() == 3, "FIT_A FIT_B NEW_REPORT.json");
    let (x, y, out) = (Path::new(&a[0]), Path::new(&a[1]), Path::new(&a[2]));
    ensure!(!out.exists(), "new report required");
    let (fx, fy) = (read(&x.join("fit.json"))?, read(&y.join("fit.json"))?);
    ensure!(
        fx["schema"] == "slop_ninja_full_a_fit_v2"
            && fy["schema"] == "slop_ninja_full_a_fit_v2"
            && fx["complete"] == true
            && fy["complete"] == true
            && fx["binding"] == fy["binding"],
        "fits must be complete with identical bindings"
    );
    let mut files = BTreeMap::new();
    let mut names = vec![
        "selected.pt".to_owned(),
        "encoder.safetensors".to_owned(),
        "heads.safetensors".to_owned(),
        "dev_iter-predictions.jsonl".to_owned(),
        "dev_iter-raw.jsonl".to_owned(),
        "initial-gate.json".to_owned(),
        "cohort-weights.json".to_owned(),
    ];
    for f in [&fx, &fy] {
        if let Some(split) = f["report"]["split"].as_str() {
            for kind in ["predictions", "raw"] {
                let name = format!("report-{split}-{kind}.jsonl");
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    let mut identical = true;
    for name in &names {
        let (p, q) = (x.join(name), y.join(name));
        let (hp, hq) = (sha(&p)?, sha(&q)?);
        identical &= hp == hq;
        files.insert(name.clone(), json!({"a":hp,"b":hq,"equal":hp == hq}));
    }
    let (hx, hy) = (read(&x.join("history.json"))?, read(&y.join("history.json"))?);
    let strip = |h: &Value| -> Result<Vec<Value>> {
        h.as_array()
            .context("history")?
            .iter()
            .map(|r| {
                let mut r = r.clone();
                r.as_object_mut()
                    .context("history row")?
                    .remove("elapsed_seconds_this_invocation");
                Ok(r)
            })
            .collect()
    };
    let history_equal = strip(&hx)? == strip(&hy)?;
    identical &= history_equal;
    let mut prediction_diffs = BTreeMap::new();
    for name in names.iter().filter(|n| n.ends_with("predictions.jsonl") || n.ends_with("raw.jsonl")) {
        let (p, q) = (predictions(&x.join(name))?, predictions(&y.join(name))?);
        ensure!(p.keys().eq(q.keys()), "prediction ids differ in {name}");
        let mut worst = 0f64;
        for (id, v) in &p {
            worst = worst.max(numeric_diff(v, &q[id], id)?);
        }
        prediction_diffs.insert(name.clone(), json!({"rows":p.len(),"max_abs_numeric_difference":worst}));
    }
    let same_selection = fx["selected_evaluation"] == fy["selected_evaluation"]
        && fx["selection_split_involvement_brier"] == fy["selection_split_involvement_brier"];
    identical &= same_selection;
    let report = json!({"schema":"slop_ninja_full_a_determinism_v2","complete":true,
        "bitwise_identical":identical,"binding":fx["binding"],"files":files,
        "history_equal_excluding_wall_time":history_equal,"same_selection":same_selection,
        "selected_evaluation":[fx["selected_evaluation"],fy["selected_evaluation"]],
        "selection_split_involvement_brier":[fx["selection_split_involvement_brier"],
                                             fy["selection_split_involvement_brier"]],
        "predictions":prediction_diffs,
        "implementation_sha256":hex::encode(Sha256::digest(include_bytes!("compare_full_a_fits.rs")))});
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(out)?;
    serde_json::to_writer_pretty(&mut f, &report)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    println!("{}", json!({"bitwise_identical":identical,"predictions":report["predictions"]}));
    ensure!(identical, "fits differ; report retained");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numeric_diff_reports_numbers_and_refuses_structure_changes() {
        let a = json!({"id":"x","p":[0.25,1.0],"q":{"r":2}});
        let b = json!({"id":"x","p":[0.5,1.0],"q":{"r":2}});
        assert_eq!(numeric_diff(&a, &b, "").unwrap(), 0.25);
        assert!(numeric_diff(&a, &json!({"id":"y","p":[0.25,1.0],"q":{"r":2}}), "").is_err());
        assert!(numeric_diff(&a, &json!({"id":"x","p":[0.25],"q":{"r":2}}), "").is_err());
        assert!(numeric_diff(&json!(null), &json!(0), "").is_err());
    }
}
