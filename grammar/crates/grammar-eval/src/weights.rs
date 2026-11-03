//! Development-only weight selection and paired author uncertainty.
use crate::{Metrics, Values};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SELECTION_SCHEMA: &str = "unslop-frozen-author-weights-v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrozenSelection {
    pub schema: String,
    pub selected_name: String,
    pub weights: Values,
    pub selection_author_ids: BTreeSet<String>,
    pub source_space_id: String,
    pub feature_schema: String,
    pub parser_identity: String,
}

impl FrozenSelection {
    pub fn validate_reference_authors(&self, reference: &grammar_core::space::Space) -> Result<()> {
        let mut authors = BTreeSet::new();
        for binding in &reference.references {
            let parts: Vec<_> = binding.source_group.split(':').collect();
            ensure!(
                parts.len() == 4
                    && parts[0] == "blog"
                    && !parts[1].is_empty()
                    && parts[2] == "date",
                "frozen reference lacks Blog author/date provenance"
            );
            authors.insert(parts[1].to_owned());
        }
        ensure!(
            authors == self.selection_author_ids,
            "selection author IDs differ from fitted reference authors"
        );
        Ok(())
    }

    pub fn validate(&self, families: &BTreeSet<String>, authors: &BTreeSet<String>) -> Result<()> {
        ensure!(
            self.schema == SELECTION_SCHEMA,
            "unsupported weight selection schema"
        );
        ensure!(
            !self.selected_name.is_empty(),
            "missing selected candidate name"
        );
        validate_weights(&self.weights, families)?;
        ensure!(
            !self.selection_author_ids.is_empty(),
            "missing selection author IDs"
        );
        ensure!(
            self.selection_author_ids.is_disjoint(authors),
            "evaluation authors overlap weight-selection authors"
        );
        Ok(())
    }
}

pub fn validate_weights(weights: &Values, families: &BTreeSet<String>) -> Result<()> {
    ensure!(!weights.is_empty(), "empty weights");
    ensure!(
        weights.keys().all(|name| families.contains(name)),
        "unknown weight family"
    );
    ensure!(
        weights.values().all(|w| w.is_finite() && *w >= 0.0),
        "weights must be finite and nonnegative"
    );
    ensure!(
        (weights.values().sum::<f64>() - 1.0).abs() < 1e-12,
        "weights must sum to one"
    );
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct Candidate {
    pub name: String,
    pub grammar_mass: f64,
    pub shape: String,
    pub weights: Values,
}

/// Fixed small grid. Candidate order supplies the last deterministic tie-breaks.
pub fn candidates(reliability: &Values, families: &BTreeSet<String>) -> Result<Vec<Candidate>> {
    let grammar: BTreeSet<_> = families
        .iter()
        .filter(|name| !matches!(name.as_str(), "word" | "word_bigram"))
        .cloned()
        .collect();
    ensure!(
        families.contains("word") && families.contains("word_bigram") && !grammar.is_empty(),
        "missing lexical or grammar families"
    );
    ensure!(
        reliability.keys().cloned().collect::<BTreeSet<_>>() == grammar,
        "reliability family catalog mismatch"
    );
    validate_weights(reliability, &grammar)?;
    let mut out = Vec::<Candidate>::new();
    for (percent, mass) in [
        (0, 0.0),
        (10, 0.1),
        (25, 0.25),
        (50, 0.5),
        (75, 0.75),
        (100, 1.0),
    ] {
        for shape in ["equal", "reliability"] {
            let mut weights = Values::from([
                ("word".into(), (1.0 - mass) / 2.0),
                ("word_bigram".into(), (1.0 - mass) / 2.0),
            ]);
            for name in &grammar {
                weights.insert(
                    name.clone(),
                    mass * if shape == "equal" {
                        1.0 / grammar.len() as f64
                    } else {
                        reliability[name]
                    },
                );
            }
            validate_weights(&weights, families)?;
            if out.iter().any(|candidate| candidate.weights == weights) {
                continue;
            }
            out.push(Candidate {
                name: format!("grammar_{percent:03}_{shape}"),
                grammar_mass: mass,
                shape: shape.into(),
                weights,
            });
        }
    }
    Ok(out)
}

/// Joint resampling preserves the pairing between models for each author.
pub fn paired_author_difference(
    left: &BTreeMap<String, Metrics>,
    right: &BTreeMap<String, Metrics>,
) -> Result<serde_json::Value> {
    ensure!(
        !left.is_empty() && left.keys().eq(right.keys()),
        "paired author catalogs differ or are empty"
    );
    let rows: Vec<[f64; 3]> = left
        .iter()
        .map(|(author, a)| {
            let b = right[author];
            [a.top1 - b.top1, a.top5 - b.top5, a.mrr - b.mrr]
        })
        .collect();
    let mean: Vec<f64> = (0..3)
        .map(|k| rows.iter().map(|row| row[k]).sum::<f64>() / rows.len() as f64)
        .collect();
    let mut samples = [Vec::new(), Vec::new(), Vec::new()];
    let mut state = 0x6a09e667f3bcc909_u64;
    for _ in 0..2000 {
        let mut sum = [0.0; 3];
        for _ in 0..rows.len() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let row = rows[(state % rows.len() as u64) as usize];
            for k in 0..3 {
                sum[k] += row[k] / rows.len() as f64;
            }
        }
        for k in 0..3 {
            samples[k].push(sum[k]);
        }
    }
    let mut result = serde_json::Map::new();
    for (k, name) in ["top1", "top5", "mrr"].into_iter().enumerate() {
        samples[k].sort_by(f64::total_cmp);
        result.insert(name.into(), serde_json::json!({"mean_difference":mean[k],"percentile_95":[samples[k][49],samples[k][1949]]}));
    }
    Ok(
        serde_json::json!({"author_count":rows.len(),"replicates":2000,"seed":"0x6a09e667f3bcc909","metrics":result}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grid_includes_word_only_and_deduplicates_equal_reliability() {
        let families = BTreeSet::from(["word".into(), "word_bigram".into(), "syntax".into()]);
        let grid = candidates(&Values::from([("syntax".into(), 1.0)]), &families).unwrap();
        assert_eq!(grid.len(), 6);
        assert_eq!(grid[0].weights["word"], 0.5);
        assert_eq!(grid[5].weights["syntax"], 1.0);
    }
    #[test]
    fn selection_rejects_author_leakage_and_bad_weights() {
        let families = BTreeSet::from(["word".into()]);
        let mut selection = FrozenSelection {
            schema: SELECTION_SCHEMA.into(),
            selected_name: "fixed".into(),
            weights: Values::from([("word".into(), 1.0)]),
            selection_author_ids: BTreeSet::from(["old".into()]),
            source_space_id: "space".into(),
            feature_schema: "feature".into(),
            parser_identity: "parser".into(),
        };
        assert!(
            selection
                .validate(&families, &BTreeSet::from(["old".into()]))
                .is_err()
        );
        assert!(
            selection
                .validate(&families, &BTreeSet::from(["new".into()]))
                .is_ok()
        );
        selection.weights.insert("word".into(), -1.0);
        assert!(
            selection
                .validate(&families, &BTreeSet::from(["new".into()]))
                .is_err()
        );
    }
    #[test]
    fn paired_intervals_preserve_constant_difference() {
        let left = BTreeMap::from([
            (
                "a".into(),
                Metrics {
                    top1: 1.0,
                    ..Metrics::default()
                },
            ),
            (
                "b".into(),
                Metrics {
                    top1: 0.5,
                    ..Metrics::default()
                },
            ),
        ]);
        let right = BTreeMap::from([
            (
                "a".into(),
                Metrics {
                    top1: 0.5,
                    ..Metrics::default()
                },
            ),
            ("b".into(), Metrics::default()),
        ]);
        let result = paired_author_difference(&left, &right).unwrap();
        assert_eq!(
            result["metrics"]["top1"]["percentile_95"],
            serde_json::json!([0.5, 0.5])
        );
        assert!(paired_author_difference(&left, &BTreeMap::new()).is_err());
    }
}
