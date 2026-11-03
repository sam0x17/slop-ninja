//! spans-v1.1 `+` trainer bundle (PROTOCOL-v1.1 Sections 3.2, 3.9 and 5.1):
//! the release trainer bundle plus the frozen `spliced_rewrite_v1` snapshot.
//!
//! Document loss. Within every domain that holds spliced documents, the
//! spliced rows take 25% of that domain's existing AI-involved assembler
//! mass and the domain's existing AI-involved rows keep 75% (equal weight per
//! spliced document within the domain). Human rows and domains without
//! spliced documents are unchanged, so the human / AI-involved mass split of
//! the release bundle is kept and the spliced cohort holds at most 25% of the
//! AI-involved mass (the trainer's `cohort_positive_mass_caps` entry bounds it
//! again after its row caps).
//!
//! Token loss (`token_weight`). The spliced cohort holds 60% of the
//! token-supervised mass in proportion to its row weights; every other Train
//! row keeps its release assembler row-weight proportion (the unscaled
//! `cohort_weight`) inside the remaining 40%. Zero-weight rows stay at 0.
//!
//! Paired originals (Section 3.2): every spliced record's `pair_id` is an
//! existing release Train human of the same split and source group, so no
//! parent row is added (AMENDMENT-D1).
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const RULE: &str = "slop_ninja_spans_plus_weights_v1";
pub const SPLICED_COHORT: &str = "spliced_rewrite_v1";
pub const SPLICED_DOMAIN_SHARE: f64 = 0.25;
pub const SPLICED_TOKEN_SHARE: f64 = 0.6;

/// One release Train row as the weighting sees it.
#[derive(Clone, Debug)]
pub struct Existing {
    pub domain: String,
    pub ai_involved: bool,
    pub weight: f64,
}

/// Weights for the `+` bundle.
#[derive(Clone, Debug)]
pub struct PlusWeights {
    /// New document-loss weight of every existing row (same order).
    pub existing: Vec<f64>,
    /// Document-loss weight of every spliced row (same order).
    pub spliced: Vec<f64>,
    pub existing_token: Vec<f64>,
    pub spliced_token: Vec<f64>,
    pub report: Value,
}

pub fn plus_weights(existing: &[Existing], spliced_domains: &[String]) -> Result<PlusWeights> {
    ensure!(existing.iter().all(|r| r.weight.is_finite() && r.weight >= 0.), "existing weights");
    let mut ai_mass: BTreeMap<&str, f64> = BTreeMap::new();
    for r in existing.iter().filter(|r| r.ai_involved) {
        *ai_mass.entry(&r.domain).or_default() += r.weight;
    }
    let mut n_spliced: BTreeMap<&str, usize> = BTreeMap::new();
    for d in spliced_domains {
        *n_spliced.entry(d).or_default() += 1;
    }
    for (d, n) in &n_spliced {
        let a = ai_mass.get(d).copied().unwrap_or(0.);
        ensure!(a > 0., "domain {d} has {n} spliced documents but no AI-involved release Train mass");
    }
    let existing_new: Vec<f64> = existing
        .iter()
        .map(|r| {
            if r.ai_involved && n_spliced.contains_key(r.domain.as_str()) {
                r.weight * (1. - SPLICED_DOMAIN_SHARE)
            } else {
                r.weight
            }
        })
        .collect();
    let spliced: Vec<f64> = spliced_domains
        .iter()
        .map(|d| SPLICED_DOMAIN_SHARE * ai_mass[d.as_str()] / n_spliced[d.as_str()] as f64)
        .collect();
    let total0: f64 = existing.iter().map(|r| r.weight).sum();
    let s_total: f64 = spliced.iter().sum();
    ensure!(total0 > 0. && s_total > 0., "empty weighting");
    let existing_token: Vec<f64> =
        existing.iter().map(|r| (1. - SPLICED_TOKEN_SHARE) * total0 * r.weight / total0).collect();
    let spliced_token: Vec<f64> = spliced.iter().map(|w| SPLICED_TOKEN_SHARE * total0 * w / s_total).collect();
    let ai_before: f64 = ai_mass.values().sum();
    let ai_after: f64 = existing
        .iter()
        .zip(&existing_new)
        .filter(|(r, _)| r.ai_involved)
        .map(|(_, w)| *w)
        .sum::<f64>()
        + s_total;
    let human: f64 = existing.iter().filter(|r| !r.ai_involved).map(|r| r.weight).sum();
    let report = json!({
        "rule": RULE,
        "spliced_domain_share_of_ai_involved_mass": SPLICED_DOMAIN_SHARE,
        "spliced_token_share": SPLICED_TOKEN_SHARE,
        "domains_with_spliced_rows": n_spliced.len(),
        "existing_train_weight_sum": total0,
        "human_weight_sum": human,
        "ai_involved_weight_sum_release": ai_before,
        "ai_involved_weight_sum_plus": ai_after,
        "spliced_weight_sum": s_total,
        "spliced_share_of_ai_involved_mass": s_total / ai_after,
        "human_share_release": human / (human + ai_before),
        "human_share_plus": human / (human + ai_after),
        "token_weight_sum": existing_token.iter().sum::<f64>() + spliced_token.iter().sum::<f64>(),
        "spliced_token_weight_sum": spliced_token.iter().sum::<f64>(),
        "per_domain": n_spliced.iter().map(|(d, n)| (d.to_string(), json!({
            "spliced_rows": n, "ai_involved_release_mass": ai_mass[d],
            "spliced_row_weight": SPLICED_DOMAIN_SHARE * ai_mass[d] / *n as f64}))).collect::<BTreeMap<_, _>>(),
    });
    Ok(PlusWeights { existing: existing_new, spliced, existing_token, spliced_token, report })
}

/// Trainer-row projection of one v1.1 spliced record (the `record_check.py`
/// contract row), with its document and token weights.
pub fn project(r: &Value, split: &str, cohort_weight: f64, token_weight: Option<f64>) -> Result<Value> {
    let s = |k: &str| -> Result<Value> {
        let v = r.get(k).with_context(|| format!("record field {k}"))?;
        ensure!(v.is_string(), "record field {k} must be a string");
        Ok(v.clone())
    };
    ensure!(r["schema"] == "slop_ninja_a_spliced_rewrite_v1" && r["cohort_id"] == SPLICED_COHORT, "spliced record");
    ensure!(r["origin"] == "mixed" && r["source_kind"] == "source_conditioned_mixed_rewrite", "spliced labels");
    ensure!(r["two_generators"] == false && r["generator_label"].is_string(), "one generator label (A2)");
    ensure!(r["split"] == split, "record split {} differs from {split}", r["split"]);
    let provenance = match r.get("parent_provenance_sha256") {
        Some(Value::String(p)) if !p.is_empty() => Value::String(p.clone()),
        _ => s("parent_text_sha256")?,
    };
    let mut out = json!({
        "id": s("id")?, "split": split, "text": s("text")?, "text_sha256": s("text_sha256")?,
        "origin": "mixed", "share": r["share"].clone(), "ai_byte_spans": r["ai_byte_spans"].clone(),
        "generator_label": s("generator_label")?, "cohort_id": SPLICED_COHORT, "cohort_weight": cohort_weight,
        "source_group": s("source_group")?, "domain": s("domain")?, "genre": r.get("genre").cloned().unwrap_or(Value::Null),
        "operation": s("operation")?, "held_out_style": false, "provenance_sha256": provenance,
        "pair_id": s("pair_id")?, "source_kind": s("source_kind")?, "edit_history_sha256": s("edit_history_sha256")?,
    });
    if let Some(t) = token_weight {
        out["token_weight"] = json!(t);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(d: &str, ai: bool, w: f64) -> Existing {
        Existing { domain: d.into(), ai_involved: ai, weight: w }
    }

    #[test]
    fn domain_shares_and_token_mass() {
        let existing = vec![row("a", false, 3.), row("a", true, 2.), row("a", true, 2.), row("b", false, 1.),
                            row("b", true, 1.), row("c", true, 5.), row("c", false, 0.)];
        let spliced = vec!["a".to_string(), "a".into(), "b".into()];
        let w = plus_weights(&existing, &spliced).unwrap();
        // Domain a: AI mass 4 -> existing 3, spliced 1 (0.5 each); b: AI mass 1 -> 0.75 / 0.25; c untouched.
        assert_eq!(w.existing, vec![3., 1.5, 1.5, 1., 0.75, 5., 0.]);
        assert_eq!(w.spliced, vec![0.5, 0.5, 0.25]);
        let human: f64 = 4.;
        let ai_after: f64 = 1.5 + 1.5 + 0.75 + 5. + 1.25;
        assert!((ai_after - 10.).abs() < 1e-12 && (human / (human + ai_after) - 4. / 14.).abs() < 1e-12);
        let tok: f64 = w.existing_token.iter().sum::<f64>() + w.spliced_token.iter().sum::<f64>();
        let st: f64 = w.spliced_token.iter().sum();
        assert!((st / tok - 0.6).abs() < 1e-12);
        // Existing rows keep their release proportions inside the 40%.
        assert!((w.existing_token[0] / w.existing_token[3] - 3.).abs() < 1e-12 && w.existing_token[6] == 0.);
        assert!((w.spliced_token[0] / w.spliced_token[2] - 2.).abs() < 1e-12);
    }

    #[test]
    fn spliced_domain_without_ai_mass_is_refused() {
        let existing = vec![row("a", false, 1.)];
        assert!(plus_weights(&existing, &["a".to_string()]).is_err());
    }
}
