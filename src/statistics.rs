//! Descriptive feature contrasts. These scores are not detector probabilities.

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

fn count(value: &Value, name: &str) -> Result<u64> {
    value[name]
        .as_u64()
        .with_context(|| format!("Invalid nonnegative count: {name}"))
}

fn feature_counts<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>> {
    value[name]
        .as_object()
        .with_context(|| format!("Missing feature counts: {name}"))
}

fn observed(counts: &Map<String, Value>, feature: &str) -> Result<u64> {
    match counts.get(feature) {
        None => Ok(0),
        Some(value) => value
            .as_u64()
            .with_context(|| format!("Invalid count for {feature}")),
    }
}

/// Half-count smoothed binary log odds; the variance ignores document dependence.
pub fn contrast(left: &Value, right: &Value, min_count: u64, min_documents: u64) -> Result<Value> {
    if left["extractor"].as_str().is_none()
        || left["family"].as_str().is_none()
        || left["extractor"] != right["extractor"]
        || left["family"] != right["family"]
    {
        bail!("Contrasts require identical extractor and feature family");
    }
    let (n, m) = (count(left, "total")?, count(right, "total")?);
    if n == 0 || m == 0 {
        bail!("Contrasts require nonempty opportunity counts on both sides");
    }
    if min_count == 0 || min_documents == 0 {
        bail!("Minimum evidence counts must be positive");
    }
    let (lc, rc) = (
        feature_counts(left, "counts")?,
        feature_counts(right, "counts")?,
    );
    let (ldc, rdc) = (
        feature_counts(left, "document_counts")?,
        feature_counts(right, "document_counts")?,
    );
    let (lgc, rgc) = (
        feature_counts(left, "group_counts")?,
        feature_counts(right, "group_counts")?,
    );
    let keys: BTreeSet<_> = lc.keys().chain(rc.keys()).collect();
    let mut rows = Vec::new();
    for feature in keys {
        let (a, b) = (observed(lc, feature)?, observed(rc, feature)?);
        if a > n || b > m {
            bail!("Feature count exceeds opportunity count");
        }
        let cells = [
            a as f64 + 0.5,
            (n - a) as f64 + 0.5,
            b as f64 + 0.5,
            (m - b) as f64 + 0.5,
        ];
        let log_odds = (cells[0] / cells[1]).ln() - (cells[2] / cells[3]).ln();
        let z = log_odds / cells.iter().map(|cell| 1.0 / cell).sum::<f64>().sqrt();
        let (lp, rp) = (
            (a as f64 + 0.5) / (n as f64 + 1.0),
            (b as f64 + 0.5) / (m as f64 + 1.0),
        );
        let (ld, rd) = (observed(ldc, feature)?, observed(rdc, feature)?);
        let (lg, rg) = (observed(lgc, feature)?, observed(rgc, feature)?);
        rows.push(json!({
            "feature": feature, "left_count": a, "right_count": b,
            "left_per_1000": 1000.0 * a as f64 / n as f64,
            "right_per_1000": 1000.0 * b as f64 / m as f64,
            "smoothed_lift": lp / rp, "log2_odds": log_odds / 2_f64.ln(),
            "z_heuristic": z, "left_documents": ld, "right_documents": rd,
            "left_source_groups": lg, "right_source_groups": rg,
            "enough_evidence": a.saturating_add(b) >= min_count
                && ld.max(rd) >= min_documents && lg.max(rg) >= min_documents,
        }));
    }
    rows.sort_by(|a, b| {
        b["z_heuristic"]
            .as_f64()
            .unwrap()
            .abs()
            .total_cmp(&a["z_heuristic"].as_f64().unwrap().abs())
            .then_with(|| a["feature"].as_str().cmp(&b["feature"].as_str()))
    });
    Ok(Value::Array(rows))
}

/// Sparse rates normalized by each feature family's opportunity denominator.
pub fn vector(extracted: &Value, family: &str) -> Result<Value> {
    let counts = extracted["families"][family]
        .as_object()
        .with_context(|| format!("Unknown feature family: {family}"))?;
    let total = extracted["totals"][family]
        .as_u64()
        .context("Invalid feature denominator")?;
    let mut result = Map::new();
    for (feature, value) in counts {
        let observed = value.as_u64().context("Invalid feature count")?;
        if observed > total {
            bail!("Feature count exceeds opportunity count");
        }
        if total > 0 {
            result.insert(feature.clone(), json!(observed as f64 / total as f64));
        }
    }
    Ok(Value::Object(result))
}

pub fn document_profile(extracted: &Value, family: &str) -> Result<Value> {
    // Validate denominator/count types before offering the single-document profile.
    vector(extracted, family)?;
    let counts = &extracted["families"][family];
    let documents: Map<String, Value> = counts
        .as_object()
        .unwrap()
        .keys()
        .map(|key| (key.clone(), json!(1)))
        .collect();
    Ok(json!({
        "extractor": extracted["extractor"], "family": family,
        "counts": counts, "total": extracted["totals"][family],
        "documents": 1, "groups": 1, "document_counts": documents,
        "group_counts": documents,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(counts: Value, total: u64, docs: u64, groups: u64) -> Value {
        let document_counts: Map<String, Value> = counts
            .as_object()
            .unwrap()
            .keys()
            .map(|k| (k.clone(), json!(docs)))
            .collect();
        let group_counts: Map<String, Value> = counts
            .as_object()
            .unwrap()
            .keys()
            .map(|k| (k.clone(), json!(groups)))
            .collect();
        json!({"extractor":"test", "family":"word", "counts":counts, "total":total,
            "document_counts":document_counts, "group_counts":group_counts})
    }

    #[test]
    fn swapping_corpora_reverses_scores_and_lift() {
        let a = sample(json!({"rare":1, "common":99}), 100, 5, 5);
        let b = sample(json!({"rare":10, "common":90}), 100, 5, 5);
        let left = contrast(&a, &b, 5, 3).unwrap();
        let right = contrast(&b, &a, 5, 3).unwrap();
        for row in left.as_array().unwrap() {
            let inverse = right
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["feature"] == row["feature"])
                .unwrap();
            assert!(
                (row["z_heuristic"].as_f64().unwrap() + inverse["z_heuristic"].as_f64().unwrap())
                    .abs()
                    < 1e-12
            );
            assert!(
                (row["smoothed_lift"].as_f64().unwrap()
                    * inverse["smoothed_lift"].as_f64().unwrap()
                    - 1.0)
                    .abs()
                    < 1e-12
            );
        }
    }

    #[test]
    fn singleton_and_repeated_source_do_not_satisfy_evidence_floor() {
        let a = sample(json!({"singleton":1,"other":100}), 101, 100, 1);
        let b = sample(json!({"other":100}), 100, 100, 1);
        for row in contrast(&a, &b, 5, 3).unwrap().as_array().unwrap() {
            assert!(row["z_heuristic"].as_f64().unwrap().is_finite());
            assert_eq!(row["enough_evidence"], false);
        }
    }

    #[test]
    fn construction_counts_use_sentence_denominator() {
        let mut a = sample(json!({"passive":3,"relative":4}), 5, 5, 5);
        let mut b = sample(json!({"passive":1,"relative":4}), 5, 5, 5);
        a["family"] = json!("construction");
        b["family"] = json!("construction");
        let rows = contrast(&a, &b, 5, 3).unwrap();
        let passive = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["feature"] == "passive")
            .unwrap();
        assert_eq!(passive["left_per_1000"], 600.0);
    }

    #[test]
    fn reject_invalid_counts_and_version_mismatch() {
        let a = sample(json!({"x":3}), 2, 5, 5);
        let mut b = sample(json!({"x":1}), 1, 5, 5);
        assert!(contrast(&a, &b, 5, 3).is_err());
        b["extractor"] = json!("other");
        assert!(contrast(&a, &b, 5, 3).is_err());
        assert!(contrast(&b, &b, 0, 3).is_err());
    }

    #[test]
    fn equal_counts_are_zero_and_vectors_respect_denominators() {
        let a = sample(json!({"x":10,"y":20}), 30, 5, 5);
        assert!(
            contrast(&a, &a, 5, 3)
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["z_heuristic"] == 0.0)
        );
        let extraction = json!({"extractor":"test", "families":{"construction":{"passive":3,"relative":4}}, "totals":{"construction":5}});
        assert_eq!(vector(&extraction, "construction").unwrap()["passive"], 0.6);
        assert_eq!(
            document_profile(&extraction, "construction").unwrap()["total"],
            5
        );
        assert_eq!(
            vector(&json!({"families":{"word":{}},"totals":{"word":0}}), "word").unwrap(),
            json!({})
        );
    }
}
