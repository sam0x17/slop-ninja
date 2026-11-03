//! Paired prediction sensitivity for the frozen whitespace-only diagnostic.
//! Reports descriptive document-weighted changes, not a new selection metric.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

fn read(path: &str) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn digest(path: &str) -> Result<String> {
    Ok(hex::encode(Sha256::digest(fs::read(path)?)))
}
fn predictions(value: &Value) -> Result<BTreeMap<String, &Value>> {
    let mut result = BTreeMap::new();
    for row in value.as_array().context("prediction array")? {
        let id = row["id"].as_str().context("prediction id")?;
        ensure!(
            result.insert(id.to_owned(), &row["prediction"]).is_none(),
            "duplicate id"
        );
    }
    Ok(result)
}
fn percentage(prediction: &Value, field: &str) -> Result<f64> {
    let x = prediction[field]
        .as_f64()
        .context("prediction percentage")?;
    ensure!(
        x.is_finite() && (0.0..=100.0).contains(&x),
        "percentage range"
    );
    Ok(x)
}
fn summary(mut deltas: Vec<f64>) -> Value {
    deltas.sort_by(f64::total_cmp);
    json!({"mean": deltas.iter().sum::<f64>() / deltas.len() as f64,
        "p95": deltas[(deltas.len()-1)*95/100], "max": deltas.last(),
        "above_one_percentage_point": deltas.iter().filter(|x| **x > 1.0).count()})
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 4,
        "compare_layout_predictions ORIGINAL_PREDICTIONS LAYOUT_PREDICTIONS LAYOUT_MANIFEST NEW_REPORT"
    );
    ensure!(!Path::new(&a[3]).exists(), "new output required");
    let original_value = read(&a[0])?;
    let layout_value = read(&a[1])?;
    let original = predictions(&original_value)?;
    let layout = predictions(&layout_value)?;
    let manifest = read(&a[2])?;
    let control = &manifest["layout_control"];
    ensure!(
        control["normalization_policy"] == "rust_unicode_split_whitespace_join_ascii_space_v1"
            && control["fitting_performed"] == false
            && control["selection_changed"] == false,
        "unsupported diagnostic"
    );
    let ids = control["parent_test_ids"]
        .as_array()
        .context("parent ids")?;
    ensure!(
        !ids.is_empty() && ids.len() == original.len() && ids.len() == layout.len(),
        "population mismatch"
    );
    let mut confidence = Vec::new();
    let mut share = Vec::new();
    let mut role_changes = BTreeMap::from([("human", 0usize), ("ai", 0)]);
    let mut threshold_changes = 0;
    let mut records = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for id in ids {
        let id = id.as_str().context("parent id")?;
        ensure!(seen.insert(id), "duplicate parent id");
        let before = original.get(id).context("original prediction missing")?;
        let after = layout
            .get(&format!("{id}-layout"))
            .context("layout prediction missing")?;
        let c0 = percentage(before, "ai_confidence_percent")?;
        let c1 = percentage(after, "ai_confidence_percent")?;
        let dc = (c1 - c0).abs();
        let ds = (percentage(after, "ai_share_percent")? - percentage(before, "ai_share_percent")?)
            .abs();
        confidence.push(dc);
        share.push(ds);
        if (c0 >= 50.0) != (c1 >= 50.0) {
            threshold_changes += 1;
        }
        for role in ["human", "ai"] {
            let p0 = before["attribution"][role]["profile"]
                .as_str()
                .context("original profile")?;
            let p1 = after["attribution"][role]["profile"]
                .as_str()
                .context("layout profile")?;
            if p0 != p1 {
                *role_changes.get_mut(role).unwrap() += 1;
            }
        }
        records
            .push(json!({"id":id,"absolute_confidence_delta_pp":dc,"absolute_share_delta_pp":ds}));
    }
    let report = json!({"schema":"slop_ninja_a_layout_prediction_sensitivity_v1",
        "documents":ids.len(), "absolute_confidence_delta_pp":summary(confidence),
        "absolute_share_delta_pp":summary(share), "confidence_threshold_50_crossings":threshold_changes,
        "conditional_profile_changes":role_changes,
        "weighting":"descriptive equal-document changes;not source-family performance estimates",
        "limits":["conditional profile changes include absent roles and are not false-attribution counts",
            "whitespace collapse is one perturbation, not general formatting robustness", "no model selection or refitting"],
        "bindings":{"original_predictions_sha256":digest(&a[0])?,"layout_predictions_sha256":digest(&a[1])?,
            "layout_manifest_sha256":digest(&a[2])?}, "records":records});
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&a[3])?;
    file.write_all(&serde_json::to_vec_pretty(&report)?)?;
    file.sync_all()?;
    println!(
        "{}",
        json!({"documents":report["documents"],"confidence":report["absolute_confidence_delta_pp"],
        "share":report["absolute_share_delta_pp"],"threshold_crossings":threshold_changes,"profile_changes":role_changes})
    );
    Ok(())
}
