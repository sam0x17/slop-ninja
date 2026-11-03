//! Scorer outputs to protocol outcomes, and per-artifact score components.
//!
//! The reward quantity under `testnet-reward-v0` is the integer source-balanced
//! confidence Brier utility `S` from the protocol crate, with every task its own
//! group, so `S` is the plain mean over the self-weighting 40/40/20 batch. The
//! components add content-free diagnostics computed in f64: Brier by kind, share
//! error, attribution label rates, the paired difference against R_k with a
//! standard error clustered by source group, and pairwise prediction RMS for the
//! near-duplicate rule. None of them carries a document, label or prediction.

use serde::Serialize;
use serde_json::Value;
use slop_ninja_protocol::reward::{ScoredTask, aggregate_score};
use slop_ninja_protocol::{Outcome, Probability, Score};
use std::collections::BTreeMap;

/// Near-duplicate threshold on RMS involvement difference (cohort sizing,
/// verifier correction 4: independent seed retrains differ at about 0.076).
pub const NEAR_DUPLICATE_RMS: f64 = 0.01;

/// Parses one scorer result into an outcome plus the attribution labels.
/// `expected_sha` is the document's text digest; a mismatch is an
/// infrastructure error, never a miner failure.
pub fn outcome(result: &Value, expected_sha: &str) -> (Outcome, Option<String>, Option<String>, Option<u32>) {
    if result["text_sha256"].as_str() != Some(expected_sha) {
        return (Outcome::InfrastructureError, None, None, None);
    }
    let tokens = result["token_count"].as_u64().map(|t| t as u32);
    match result["status"].as_str() {
        Some("ok") => {}
        Some("unsupported_input") => return (Outcome::ParseFailure, None, None, tokens),
        Some("error") => return (Outcome::ModelError, None, None, tokens),
        _ => return (Outcome::InfrastructureError, None, None, tokens),
    }
    let p = &result["prediction"];
    let human = p["attribution"]["human"]["profile"].as_str().map(str::to_owned);
    let ai = p["attribution"]["ai"]["profile"].as_str().map(str::to_owned);
    let prob = |v: &Value| v.as_f64().and_then(|x| Probability::from_f64(x / 100.0).ok());
    match (prob(&p["ai_confidence_percent"]), prob(&p["ai_share_percent"])) {
        (Some(involvement), Some(share)) if share <= involvement => {
            (Outcome::Prediction { involvement, share }, human, ai, tokens)
        }
        _ => (Outcome::InvalidOutput, human, ai, tokens),
    }
}

/// One task's private label and its final resolved prediction for one artifact.
#[derive(Clone, Debug)]
pub struct TaskResult {
    pub kind: String,
    pub source_group: String,
    pub involved: bool,
    pub share_ppb: u32,
    /// Final non-infrastructure outcome: `Some((involvement, share))` for a
    /// prediction, `None` for an attributable failure.
    pub prediction: Option<(u32, u32)>,
    pub human_profile: Option<String>,
    pub ai_profile: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Components {
    pub assigned: u32,
    pub succeeded: u32,
    pub failed: u32,
    /// `testnet-reward-v0` utility S (10^18 scale), as a decimal string.
    pub utility_v0: String,
    pub brier: f64,
    pub brier_by_kind: BTreeMap<String, f64>,
    pub count_by_kind: BTreeMap<String, u32>,
    pub share_mae: f64,
    pub share_mae_mixed: f64,
    pub human_only_other_human_rate: f64,
    pub ai_profile_counts: BTreeMap<String, u32>,
    pub false_positive_rate_at_half: f64,
    pub true_positive_rate_at_half: f64,
}

fn p(ppb: u32) -> f64 {
    f64::from(ppb) / 1e9
}

pub fn brier(task: &TaskResult) -> f64 {
    match task.prediction {
        // An attributable failure scores the worst Brier (utility 0).
        None => 1.0,
        Some((inv, _)) => {
            let y = if task.involved { 1.0 } else { 0.0 };
            (p(inv) - y).powi(2)
        }
    }
}

pub fn utility(tasks: &[TaskResult]) -> Result<Score, slop_ninja_protocol::Error> {
    let scored: Vec<ScoredTask> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| ScoredTask {
            group: i as u32,
            involvement: t.prediction.map(|(inv, _)| Probability::new(inv).expect("stored ppb")),
            label: t.involved,
        })
        .collect();
    aggregate_score(&scored, tasks.len() as u32)
}

pub fn components(tasks: &[TaskResult]) -> Result<Components, slop_ninja_protocol::Error> {
    let s = utility(tasks)?;
    let n = tasks.len().max(1) as f64;
    let mut by_kind: BTreeMap<String, (f64, u32)> = BTreeMap::new();
    let (mut share_err, mut mixed_err, mut mixed_n) = (0.0, 0.0, 0u32);
    let (mut hum, mut hum_other, mut fp, mut tp, mut pos) = (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut ai_profiles: BTreeMap<String, u32> = BTreeMap::new();
    let mut succeeded = 0;
    for t in tasks {
        let e = by_kind.entry(t.kind.clone()).or_default();
        e.0 += brier(t);
        e.1 += 1;
        let (inv, share) = t.prediction.map_or((None, None), |(i, s)| (Some(p(i)), Some(p(s))));
        if t.prediction.is_some() {
            succeeded += 1;
        }
        let err = share.map_or(1.0, |s| (s - p(t.share_ppb)).abs());
        share_err += err;
        if t.kind == "mixed" {
            mixed_err += err;
            mixed_n += 1;
        }
        if t.involved {
            pos += 1;
            if inv.is_some_and(|x| x >= 0.5) {
                tp += 1;
            }
            if let Some(a) = &t.ai_profile {
                *ai_profiles.entry(a.clone()).or_default() += 1;
            }
        } else {
            hum += 1;
            if inv.is_none_or(|x| x >= 0.5) {
                fp += 1;
            }
            if t.human_profile.as_deref() == Some("other_human") {
                hum_other += 1;
            }
        }
    }
    let rate = |a: u32, b: u32| if b == 0 { 0.0 } else { f64::from(a) / f64::from(b) };
    Ok(Components {
        assigned: tasks.len() as u32,
        succeeded,
        failed: tasks.len() as u32 - succeeded,
        utility_v0: s.get().to_string(),
        brier: by_kind.values().map(|v| v.0).sum::<f64>() / n,
        brier_by_kind: by_kind.iter().map(|(k, v)| (k.clone(), v.0 / f64::from(v.1))).collect(),
        count_by_kind: by_kind.iter().map(|(k, v)| (k.clone(), v.1)).collect(),
        share_mae: share_err / n,
        share_mae_mixed: if mixed_n == 0 { 0.0 } else { mixed_err / f64::from(mixed_n) },
        human_only_other_human_rate: rate(hum_other, hum),
        ai_profile_counts: ai_profiles,
        false_positive_rate_at_half: rate(fp, hum),
        true_positive_rate_at_half: rate(tp, pos),
    })
}

/// Paired Brier difference `mean(brier_ref - brier_x)` (positive: x is better) with
/// a standard error clustered by source group, as a fraction of the reference's
/// headroom when that is positive.
#[derive(Clone, Debug, Serialize)]
pub struct Paired {
    pub mean_brier_gain: f64,
    pub se: f64,
    pub clusters: usize,
    pub gain_over_headroom: Option<f64>,
    pub se_over_headroom: Option<f64>,
}

pub fn paired(reference: &[TaskResult], x: &[TaskResult]) -> Paired {
    let n = reference.len().min(x.len());
    let mut clusters: BTreeMap<&str, (f64, usize)> = BTreeMap::new();
    let mut total = 0.0;
    let mut ref_brier = 0.0;
    for i in 0..n {
        let d = brier(&reference[i]) - brier(&x[i]);
        total += d;
        ref_brier += brier(&reference[i]);
        let c = clusters.entry(reference[i].source_group.as_str()).or_default();
        c.0 += d;
        c.1 += 1;
    }
    let nf = n.max(1) as f64;
    let mean = total / nf;
    let g = clusters.len() as f64;
    // CR1 sandwich variance of the mean with cluster totals.
    let var = if g > 1.0 {
        let s: f64 = clusters.values().map(|(sum, k)| (sum - *k as f64 * mean).powi(2)).sum();
        g / (g - 1.0) * s / (nf * nf)
    } else {
        0.0
    };
    let headroom = ref_brier / nf;
    let se = var.sqrt();
    Paired {
        mean_brier_gain: mean,
        se,
        clusters: clusters.len(),
        gain_over_headroom: (headroom > 0.0).then(|| mean / headroom),
        se_over_headroom: (headroom > 0.0).then(|| se / headroom),
    }
}

/// RMS difference of involvement predictions over tasks both resolved.
pub fn rms(a: &[TaskResult], b: &[TaskResult]) -> f64 {
    let mut s = 0.0;
    let mut n = 0usize;
    for (x, y) in a.iter().zip(b) {
        match (x.prediction, y.prediction) {
            (Some((i, _)), Some((j, _))) => s += (p(i) - p(j)).powi(2),
            (None, None) => {}
            _ => s += 1.0,
        }
        n += 1;
    }
    if n == 0 { 0.0 } else { (s / n as f64).sqrt() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(kind: &str, group: &str, involved: bool, inv: Option<u32>) -> TaskResult {
        TaskResult {
            kind: kind.into(),
            source_group: group.into(),
            involved,
            share_ppb: if involved { 500_000_000 } else { 0 },
            prediction: inv.map(|i| (i, i / 2)),
            human_profile: Some("other_human".into()),
            ai_profile: Some("other_ai".into()),
        }
    }

    #[test]
    fn scorer_results_map_to_outcomes() {
        let ok = json!({"text_sha256": "aa", "status": "ok", "token_count": 300, "prediction": {
            "ai_confidence_percent": 90.0, "ai_share_percent": 40.0,
            "attribution": {"human": {"profile": "other_human"}, "ai": {"profile": "gemma3"}}}});
        let (o, h, a, t) = outcome(&ok, "aa");
        assert!(matches!(o, Outcome::Prediction { involvement, share } if involvement.ppb() == 900_000_000 && share.ppb() == 400_000_000));
        assert_eq!((h.as_deref(), a.as_deref(), t), (Some("other_human"), Some("gemma3"), Some(300)));
        assert_eq!(outcome(&ok, "bb").0, Outcome::InfrastructureError);
        let mut bad = ok.clone();
        bad["prediction"]["ai_share_percent"] = json!(95.0);
        assert_eq!(outcome(&bad, "aa").0, Outcome::InvalidOutput);
        bad["prediction"]["ai_share_percent"] = json!(f64::NAN);
        assert_eq!(outcome(&bad, "aa").0, Outcome::InvalidOutput);
        let long = json!({"text_sha256": "aa", "status": "unsupported_input"});
        assert_eq!(outcome(&long, "aa").0, Outcome::ParseFailure);
        assert_eq!(outcome(&json!({"text_sha256": "aa", "status": "error"}), "aa").0, Outcome::ModelError);
    }

    #[test]
    fn components_and_pairing() {
        let r = vec![
            task("human_only", "g1", false, Some(200_000_000)),
            task("mixed", "g1", true, Some(700_000_000)),
            task("model_only", "g2", true, Some(950_000_000)),
            task("human_only", "g3", false, None),
        ];
        let c = components(&r).unwrap();
        assert_eq!((c.assigned, c.succeeded, c.failed), (4, 3, 1));
        let expect = (0.04 + 0.09 + 0.0025 + 1.0) / 4.0;
        assert!((c.brier - expect).abs() < 1e-12);
        let s: u64 = c.utility_v0.parse().unwrap();
        assert!(((1.0 - s as f64 / 1e18) - expect).abs() < 1e-9, "utility is one minus mean Brier");
        assert_eq!(c.false_positive_rate_at_half, 0.5, "a failed human doc counts as a false positive");
        let same = paired(&r, &r);
        assert_eq!((same.mean_brier_gain, same.se), (0.0, 0.0));
        assert_eq!(rms(&r, &r), 0.0);
        let mut better = r.clone();
        better[3].prediction = Some((100_000_000, 0));
        let pr = paired(&r, &better);
        assert!(pr.mean_brier_gain > 0.0 && pr.clusters == 3);
        assert!(rms(&r, &better) > NEAR_DUPLICATE_RMS);
    }
}
