//! Positive diagonal metric learning with analytic derivatives and full-batch Adam.
//! This one-layer network learns family weights, not individual word embeddings.
use anyhow::{Result, ensure};
use grammar_eval::{Profile, Values};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const EPOCHS: usize = 200;
pub const LEARNING_RATE: f64 = 0.03;
pub const BETA1: f64 = 0.9;
pub const BETA2: f64 = 0.999;
pub const EPSILON: f64 = 1e-8;
pub const LOG_TAU_BOUNDS: [f64; 2] = [-6.0, 2.0];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Example {
    pub id: String,
    pub author: String,
    pub source_group: String,
    pub own: usize,
    pub loss_weight: f64,
    /// Candidate-major, then family-major unweighted squared distances.
    pub distances: Vec<Vec<f64>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub theta: Vec<f64>,
    pub log_tau: f64,
}

impl Model {
    pub fn initial(weights: &[f64]) -> Result<Self> {
        ensure!(!weights.is_empty(), "empty initial weights");
        ensure!(
            weights.iter().all(|w| w.is_finite() && *w > 0.0),
            "invalid initial weight"
        );
        ensure!(
            (weights.iter().sum::<f64>() - 1.0).abs() < 1e-12,
            "initial weight mass"
        );
        Ok(Self {
            theta: weights.iter().map(|w| w.ln()).collect(),
            log_tau: 0.1_f64.ln(),
        })
    }

    pub fn weights(&self) -> Vec<f64> {
        let max = self.theta.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let mut weights: Vec<_> = self.theta.iter().map(|theta| (theta - max).exp()).collect();
        let sum: f64 = weights.iter().sum();
        weights.iter_mut().for_each(|w| *w /= sum);
        weights
    }

    pub fn named_weights(&self, families: &[String]) -> Values {
        families.iter().cloned().zip(self.weights()).collect()
    }

    /// Loss sums the supplied query weights. The temperature derivative is with
    /// respect to log(tau), and theta differentiates through family softmax.
    pub fn loss_gradient(&self, rows: &[Example]) -> Result<(f64, Vec<f64>)> {
        let weights = self.weights();
        let tau = self.log_tau.exp();
        ensure!(tau.is_finite() && tau > 0.0, "invalid temperature");
        let mut loss = 0.0;
        let mut gradient = vec![0.0; weights.len() + 1];
        for row in rows {
            ensure!(
                row.own < row.distances.len()
                    && row.loss_weight.is_finite()
                    && row.loss_weight > 0.0,
                "invalid training row"
            );
            let mut logits = Vec::with_capacity(row.distances.len());
            for candidate in &row.distances {
                ensure!(
                    candidate.len() == weights.len()
                        && candidate.iter().all(|d| d.is_finite() && *d >= 0.0),
                    "invalid distance row"
                );
                logits.push(
                    -candidate
                        .iter()
                        .zip(&weights)
                        .map(|(d, w)| d * w)
                        .sum::<f64>()
                        / tau,
                );
            }
            let max = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let normalizer = logits.iter().map(|logit| (logit - max).exp()).sum::<f64>();
            loss += row.loss_weight * ((max - logits[row.own]) + normalizer.ln());
            let mut weight_gradient = vec![0.0; weights.len()];
            for (index, (logit, distances)) in logits.iter().zip(&row.distances).enumerate() {
                let residual = (logit - max).exp() / normalizer - f64::from(index == row.own);
                for (g, distance) in weight_gradient.iter_mut().zip(distances) {
                    *g -= residual * distance / tau;
                }
                gradient[weights.len()] -= row.loss_weight * residual * logit;
            }
            let average = weight_gradient
                .iter()
                .zip(&weights)
                .map(|(g, w)| g * w)
                .sum::<f64>();
            for (index, (g, w)) in weight_gradient.iter().zip(&weights).enumerate() {
                gradient[index] += row.loss_weight * w * (g - average);
            }
        }
        ensure!(
            loss.is_finite() && gradient.iter().all(|g| g.is_finite()),
            "nonfinite network result"
        );
        Ok((loss, gradient))
    }
}

#[derive(Debug, Serialize)]
pub struct Adam {
    pub step: usize,
    pub first: Vec<f64>,
    pub second: Vec<f64>,
}

/// Development selection has no effect on optimization. Ties preserve the
/// earliest checkpoint, and the saved model is a copy of that epoch's state.
#[derive(Clone, Debug)]
pub struct BestCheckpoint {
    pub epoch: usize,
    pub model: Model,
    pub score: (f64, f64),
}

pub fn select_checkpoint(
    best: &mut Option<BestCheckpoint>,
    epoch: usize,
    model: &Model,
    score: (f64, f64),
) -> Result<bool> {
    ensure!(
        score.0.is_finite() && score.1.is_finite(),
        "nonfinite selection score"
    );
    let replace = best
        .as_ref()
        .is_none_or(|old| score > old.score || (score == old.score && epoch < old.epoch));
    if replace {
        *best = Some(BestCheckpoint {
            epoch,
            model: model.clone(),
            score,
        });
    }
    Ok(replace)
}

impl Adam {
    pub fn new(parameters: usize) -> Self {
        Self {
            step: 0,
            first: vec![0.0; parameters],
            second: vec![0.0; parameters],
        }
    }

    pub fn update(&mut self, model: &mut Model, gradient: &[f64]) -> Result<()> {
        ensure!(
            gradient.len() == self.first.len() && gradient.len() == model.theta.len() + 1,
            "gradient shape"
        );
        self.step += 1;
        let b1 = 1.0 - BETA1.powf(self.step as f64);
        let b2 = 1.0 - BETA2.powf(self.step as f64);
        for (index, g) in gradient.iter().enumerate() {
            self.first[index] = BETA1 * self.first[index] + (1.0 - BETA1) * g;
            self.second[index] = BETA2 * self.second[index] + (1.0 - BETA2) * g * g;
            let delta = LEARNING_RATE * (self.first[index] / b1)
                / ((self.second[index] / b2).sqrt() + EPSILON);
            if index < model.theta.len() {
                model.theta[index] -= delta;
            } else {
                model.log_tau = (model.log_tau - delta).clamp(LOG_TAU_BOUNDS[0], LOG_TAU_BOUNDS[1]);
            }
        }
        ensure!(
            model.theta.iter().all(|x| x.is_finite()) && model.log_tau.is_finite(),
            "nonfinite update"
        );
        Ok(())
    }
}

/// Average probabilities/metrics within a date and then across dates. Exclusion
/// applies to every post on the query's date, before computing either average.
pub fn target_profile(rows: &[(&str, &Profile)], excluded_date: Option<&str>) -> Result<Profile> {
    let mut dates: BTreeMap<&str, Vec<&Profile>> = BTreeMap::new();
    for (date, profile) in rows {
        if Some(*date) != excluded_date {
            dates.entry(date).or_default().push(profile);
        }
    }
    ensure!(!dates.is_empty(), "no independent training date remains");
    let mut target = Profile::new();
    for profiles in dates.values() {
        let divisor = dates.len() as f64 * profiles.len() as f64;
        for profile in profiles {
            for (family, values) in *profile {
                for (feature, value) in values {
                    *target
                        .entry(family.clone())
                        .or_default()
                        .entry(feature.clone())
                        .or_default() += value / divisor;
                }
            }
        }
    }
    Ok(target)
}

/// Equal author mass, equal dates within each author, equal posts within a date.
pub fn balanced_weights(rows: &[(&str, &str)]) -> Result<Vec<f64>> {
    let mut counts = BTreeMap::<&str, BTreeMap<&str, usize>>::new();
    for (author, date) in rows {
        *counts.entry(author).or_default().entry(date).or_default() += 1;
    }
    ensure!(!counts.is_empty(), "empty training set");
    Ok(rows
        .iter()
        .map(|(author, date)| {
            1.0 / counts.len() as f64 / counts[author].len() as f64 / counts[author][date] as f64
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, own: usize, distances: Vec<Vec<f64>>, loss_weight: f64) -> Example {
        Example {
            id: id.into(),
            author: own.to_string(),
            source_group: id.into(),
            own,
            loss_weight,
            distances,
        }
    }

    #[test]
    fn analytical_gradient_matches_finite_differences() {
        let mut model = Model::initial(&[0.2, 0.3, 0.5]).unwrap();
        let rows = vec![
            row("a", 0, vec![vec![0.1, 0.8, 0.2], vec![0.7, 0.2, 0.5]], 0.7),
            row("b", 1, vec![vec![0.3, 0.2, 0.6], vec![0.6, 0.1, 0.5]], 0.3),
        ];
        let (_, analytic) = model.loss_gradient(&rows).unwrap();
        for (index, expected) in analytic.iter().enumerate() {
            let original = if index < 3 {
                model.theta[index]
            } else {
                model.log_tau
            };
            let epsilon = 1e-6;
            if index < 3 {
                model.theta[index] = original + epsilon;
            } else {
                model.log_tau = original + epsilon;
            }
            let plus = model.loss_gradient(&rows).unwrap().0;
            if index < 3 {
                model.theta[index] = original - epsilon;
            } else {
                model.log_tau = original - epsilon;
            }
            let minus = model.loss_gradient(&rows).unwrap().0;
            if index < 3 {
                model.theta[index] = original;
            } else {
                model.log_tau = original;
            }
            assert!(((plus - minus) / (2.0 * epsilon) - expected).abs() < 1e-8);
        }
    }

    #[test]
    fn adam_learns_the_discriminative_family() {
        let rows = vec![
            row("a", 0, vec![vec![0.0, 0.8], vec![1.0, 0.0]], 0.5),
            row("b", 1, vec![vec![1.0, 0.0], vec![0.0, 0.8]], 0.5),
        ];
        let mut model = Model::initial(&[0.5, 0.5]).unwrap();
        let original = model.loss_gradient(&rows).unwrap().0;
        let mut adam = Adam::new(3);
        for _ in 0..EPOCHS {
            let gradient = model.loss_gradient(&rows).unwrap().1;
            adam.update(&mut model, &gradient).unwrap();
        }
        let weights = model.weights();
        assert!(model.loss_gradient(&rows).unwrap().0 < original / 100.0);
        assert!(weights[0] > 0.7);
        assert!(weights.iter().all(|w| *w > 0.0));
        assert!((weights.iter().sum::<f64>() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn whole_query_date_is_excluded_before_profile_averaging() {
        let profile = |value| Profile::from([("f".into(), Values::from([("x".into(), value)]))]);
        let a = profile(100.0);
        let b = profile(200.0);
        let c = profile(2.0);
        let d = profile(4.0);
        let rows = vec![("one", &a), ("one", &b), ("two", &c), ("three", &d)];
        assert_eq!(target_profile(&rows, Some("one")).unwrap()["f"]["x"], 3.0);
        assert_eq!(target_profile(&rows, None).unwrap()["f"]["x"], 52.0);
        assert!(target_profile(&rows[..2], Some("one")).is_err());
    }

    #[test]
    fn training_loss_balances_authors_dates_and_posts() {
        let weights =
            balanced_weights(&[("a", "one"), ("a", "one"), ("a", "two"), ("b", "one")]).unwrap();
        assert_eq!(weights, vec![0.125, 0.125, 0.25, 0.5]);
    }

    #[test]
    fn softmax_and_temperature_remain_bounded() {
        let mut model = Model {
            theta: vec![1000.0, 999.0],
            log_tau: 2.0,
        };
        let weights = model.weights();
        assert!(weights.iter().all(|w| w.is_finite() && *w > 0.0));
        assert!((weights.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        let mut adam = Adam::new(3);
        adam.update(&mut model, &[0.0, 0.0, -100.0]).unwrap();
        assert_eq!(model.log_tau, 2.0);
        model.log_tau = -6.0;
        let mut adam = Adam::new(3);
        adam.update(&mut model, &[0.0, 0.0, 100.0]).unwrap();
        assert_eq!(model.log_tau, -6.0);
    }

    #[test]
    fn checkpoint_selection_replays_selected_weights_and_keeps_earliest_tie() {
        let initial = Model::initial(&[0.5, 0.5]).unwrap();
        let mut changed = Model::initial(&[0.8, 0.2]).unwrap();
        let mut best = None;
        assert!(select_checkpoint(&mut best, 0, &initial, (0.3, 0.4)).unwrap());
        assert!(select_checkpoint(&mut best, 1, &changed, (0.3, 0.5)).unwrap());
        changed.theta = vec![0.0, 0.0];
        assert!(!select_checkpoint(&mut best, 2, &changed, (0.3, 0.5)).unwrap());
        assert!(!select_checkpoint(&mut best, 3, &changed, (0.2, 0.9)).unwrap());
        let checkpoint = best.unwrap();
        assert_eq!(checkpoint.epoch, 1);
        let replay: Model =
            serde_json::from_str(&serde_json::to_string(&checkpoint.model).unwrap()).unwrap();
        assert_eq!(replay.weights(), checkpoint.model.weights());
        assert!((replay.weights()[0] - 0.8).abs() < 1e-12);
    }
}
