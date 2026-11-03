//! Evaluation on retained source groups; no provider calls or threshold fitting
//! are performed by the final evaluation path.

use crate::features::FeatureRow;
use crate::model::DetectorArtifact;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProbabilityRow {
    pub id: String,
    pub group: String,
    pub label: usize,
    pub probabilities: [f64; 3],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClassMetric {
    pub support: usize,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CalibrationBin {
    pub lower_inclusive: f64,
    pub upper_inclusive_last_bin: f64,
    pub rows: usize,
    pub mean_confidence: Option<f64>,
    pub accuracy: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetricSummary {
    pub rows: usize,
    pub source_groups: usize,
    pub log_loss: f64,
    /// Sum of squared class errors per row, with range [0, 2].
    pub multiclass_brier: f64,
    pub accuracy: f64,
    pub mean_group_accuracy: f64,
    /// Rows are true labels, columns are predicted labels, in artifact order.
    pub confusion: [[usize; 3]; 3],
    pub classes: [ClassMetric; 3],
    pub top_label_ece_10_bins: f64,
    pub calibration_bins: Vec<CalibrationBin>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HumanModelSummary {
    pub rows: usize,
    pub source_groups: usize,
    pub human_rows: usize,
    pub model_rows: usize,
    pub excluded_mixed_rows: usize,
    pub log_loss: f64,
    /// Mean squared error of P(model_only) + P(mixed), with range [0, 1].
    pub binary_brier: f64,
    pub accuracy_at_half: f64,
}

/// Human versus fully model-written examples, using the existing AI score.
/// Mixed-origin labels remain in the separate three-class report.
pub fn summarize_human_model(rows: &[ProbabilityRow]) -> Result<Option<HumanModelSummary>> {
    validate_rows(rows)?;
    let selected: Vec<_> = rows.iter().filter(|r| r.label != 2).collect();
    if selected.is_empty() {
        return Ok(None);
    }
    let mut loss = 0.0;
    let mut brier = 0.0;
    let mut correct = 0;
    for row in &selected {
        let ai = (row.probabilities[1] + row.probabilities[2]).clamp(0.0, 1.0);
        let truth = row.label == 1;
        loss -= if truth { ai } else { row.probabilities[0] }
            .max(1e-15)
            .ln();
        brier += (ai - f64::from(truth)).powi(2);
        correct += usize::from((ai > 0.5) == truth);
    }
    let n = selected.len();
    Ok(Some(HumanModelSummary {
        rows: n,
        source_groups: selected
            .iter()
            .map(|r| &r.group)
            .collect::<BTreeSet<_>>()
            .len(),
        human_rows: selected.iter().filter(|r| r.label == 0).count(),
        model_rows: selected.iter().filter(|r| r.label == 1).count(),
        excluded_mixed_rows: rows.len() - n,
        log_loss: loss / n as f64,
        binary_brier: brier / n as f64,
        accuracy_at_half: correct as f64 / n as f64,
    }))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperatingPoint {
    pub target_human_false_positive_rate: f64,
    pub threshold: f64,
    pub comparison: String,
    pub calibration_human_rows: usize,
    pub calibration_human_groups: usize,
    pub calibration_false_positives: usize,
}

impl OperatingPoint {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.target_human_false_positive_rate.is_finite()
                && (0.0..1.0).contains(&self.target_human_false_positive_rate),
            "invalid target FPR"
        );
        ensure!(
            self.threshold.is_finite() && (0.0..=1.0).contains(&self.threshold),
            "invalid decision threshold"
        );
        ensure!(
            self.comparison == "strictly_greater_than",
            "unsupported threshold boundary rule"
        );
        ensure!(
            self.calibration_human_rows > 0
                && self.calibration_human_groups > 0
                && self.calibration_human_groups <= self.calibration_human_rows
                && self.calibration_false_positives <= self.calibration_human_rows,
            "invalid calibration counts"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RateEstimate {
    pub events: usize,
    pub rows: usize,
    pub source_groups: usize,
    pub rate: Option<f64>,
    pub group_bootstrap_percentile_95: Option<[f64; 2]>,
    pub uncertainty_status: String,
    pub bootstrap_replicates: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperatingPointEvaluation {
    pub calibration: OperatingPoint,
    pub human_false_positive_rate: RateEstimate,
    pub model_or_mixed_sensitivity: RateEstimate,
    pub model_only_sensitivity: RateEstimate,
    pub mixed_sensitivity: RateEstimate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvaluationReport {
    pub artifact_sha256: String,
    pub model: MetricSummary,
    pub uniform_control: MetricSummary,
    pub training_prior_control: MetricSummary,
    pub operating_points: Vec<OperatingPointEvaluation>,
    pub predictions: Vec<ProbabilityRow>,
    /// These diagnostics apply only to the explicit word/grammar representation.
    /// Imported encoder probabilities have no observations for either field.
    pub unknown_coordinates: Option<usize>,
    pub rows_with_unavailable_families: Option<usize>,
    pub notes: Vec<String>,
}

pub fn evaluate(artifact: &DetectorArtifact, rows: &[FeatureRow]) -> Result<EvaluationReport> {
    artifact.validate()?;
    ensure!(!rows.is_empty(), "cannot evaluate an empty partition");
    let mut predictions = Vec::with_capacity(rows.len());
    let mut unknown_coordinates = 0;
    let mut rows_with_unavailable_families = 0;
    for row in rows {
        let prediction = artifact.predict_validated(&row.features)?;
        unknown_coordinates += prediction.unknown_coordinates;
        rows_with_unavailable_families += usize::from(!prediction.unavailable_families.is_empty());
        predictions.push(ProbabilityRow {
            id: row.id.clone(),
            group: row.group.clone(),
            label: row.label,
            probabilities: prediction.probabilities,
        });
    }
    let mut report = evaluate_probability_rows(
        &artifact.content_sha256,
        &predictions,
        artifact.content.training_prior,
        &artifact.content.operating_points,
    )?;
    report.unknown_coordinates = Some(unknown_coordinates);
    report.rows_with_unavailable_families = Some(rows_with_unavailable_families);
    Ok(report)
}

/// Apply the same metrics, controls and grouped uncertainty to any frozen
/// detector's probability rows. This function never fits model parameters,
/// temperatures or thresholds; callers supply already frozen operating points.
pub fn evaluate_probability_rows(
    artifact_id: &str,
    predictions: &[ProbabilityRow],
    training_prior: [f64; 3],
    operating_points: &[OperatingPoint],
) -> Result<EvaluationReport> {
    ensure!(
        !artifact_id.is_empty(),
        "missing detector artifact identity"
    );
    let model_summary = summarize(predictions)?;
    ensure!(
        training_prior
            .iter()
            .all(|p| p.is_finite() && (0.0..=1.0).contains(p))
            && (training_prior.iter().sum::<f64>() - 1.0).abs() < 1e-10,
        "invalid training-prior probabilities"
    );
    for point in operating_points {
        point.validate()?;
    }
    let control = |probabilities: [f64; 3]| -> Result<MetricSummary> {
        summarize(
            &predictions
                .iter()
                .map(|row| ProbabilityRow {
                    probabilities,
                    ..row.clone()
                })
                .collect::<Vec<_>>(),
        )
    };
    let operating_points = operating_points
        .iter()
        .map(|point| OperatingPointEvaluation {
            calibration: point.clone(),
            human_false_positive_rate: rate_estimate(predictions, point.threshold, |label| {
                label == 0
            }),
            model_or_mixed_sensitivity: rate_estimate(predictions, point.threshold, |label| {
                label != 0
            }),
            model_only_sensitivity: rate_estimate(predictions, point.threshold, |label| label == 1),
            mixed_sensitivity: rate_estimate(predictions, point.threshold, |label| label == 2),
        })
        .collect();
    Ok(EvaluationReport {
        artifact_sha256: artifact_id.into(), model: model_summary,
        uniform_control: control([1.0 / 3.0; 3])?, training_prior_control: control(training_prior)?,
        operating_points, predictions: predictions.to_vec(), unknown_coordinates: None, rows_with_unavailable_families: None,
        notes: vec![
            "Probabilities estimate document production-history classes under the dataset's evidence policy; they do not establish provenance or estimate the fraction of AI-written words.".into(),
            "Thresholds and temperature are frozen from calibration. A 1% or 5% target is not a certified population false-positive bound.".into(),
            "Uncertainty resamples complete source groups 512 times with a fixed seed. Intervals require at least two groups and assume groups are independent; related writers/topics may require broader grouping.".into(),
            "Collapsed percentile bootstrap intervals are omitted and marked degenerate_observed_outcomes. Zero observed errors does not establish a zero population error rate.".into(),
            "Log loss clips only evaluated class probabilities below 1e-15. Brier score sums all three squared class errors; calibration bins use the top predicted class.".into(),
        ],
    })
}

pub fn summarize(rows: &[ProbabilityRow]) -> Result<MetricSummary> {
    validate_rows(rows)?;
    let mut confusion = [[0_usize; 3]; 3];
    let mut brier = 0.0;
    let mut log_loss = 0.0;
    let mut bins = [(0_usize, 0_usize, 0.0_f64); 10];
    let mut groups = BTreeMap::<&str, (usize, usize)>::new();
    for row in rows {
        // Lowest class index wins exact ties; the rule is fixed for controls.
        let mut predicted = 0;
        for class in 1..3 {
            if row.probabilities[class] > row.probabilities[predicted] {
                predicted = class;
            }
        }
        confusion[row.label][predicted] += 1;
        log_loss -= row.probabilities[row.label].max(1e-15).ln();
        for class in 0..3 {
            brier += (row.probabilities[class] - f64::from(row.label == class)).powi(2);
        }
        let confidence = row.probabilities[predicted];
        let bin = ((confidence * 10.0) as usize).min(9);
        bins[bin].0 += 1;
        bins[bin].1 += usize::from(predicted == row.label);
        bins[bin].2 += confidence;
        let group = groups.entry(&row.group).or_default();
        group.0 += usize::from(predicted == row.label);
        group.1 += 1;
    }
    let classes = std::array::from_fn(|class| {
        let support = confusion[class].iter().sum();
        let predicted: usize = confusion.iter().map(|row| row[class]).sum();
        ClassMetric {
            support,
            precision: divide(confusion[class][class], predicted),
            recall: divide(confusion[class][class], support),
        }
    });
    let calibration_bins = bins
        .iter()
        .enumerate()
        .map(|(i, &(count, correct, confidence))| CalibrationBin {
            lower_inclusive: i as f64 / 10.0,
            upper_inclusive_last_bin: (i + 1) as f64 / 10.0,
            rows: count,
            mean_confidence: if count == 0 {
                None
            } else {
                Some(confidence / count as f64)
            },
            accuracy: divide(correct, count),
        })
        .collect();
    Ok(MetricSummary {
        rows: rows.len(),
        source_groups: groups.len(),
        log_loss: log_loss / rows.len() as f64,
        multiclass_brier: brier / rows.len() as f64,
        accuracy: (0..3).map(|class| confusion[class][class]).sum::<usize>() as f64
            / rows.len() as f64,
        mean_group_accuracy: groups
            .values()
            .map(|(correct, count)| *correct as f64 / *count as f64)
            .sum::<f64>()
            / groups.len() as f64,
        confusion,
        classes,
        top_label_ece_10_bins: bins
            .iter()
            .map(|&(_, correct, confidence)| (correct as f64 - confidence).abs())
            .sum::<f64>()
            / rows.len() as f64,
        calibration_bins,
    })
}

pub fn select_operating_points(
    rows: &[ProbabilityRow],
    targets: &[f64],
) -> Result<Vec<OperatingPoint>> {
    validate_rows(rows)?;
    let human: Vec<_> = rows.iter().filter(|row| row.label == 0).collect();
    ensure!(
        !human.is_empty(),
        "human calibration rows required to select FPR thresholds"
    );
    let mut scores: Vec<f64> = human
        .iter()
        .map(|row| row.probabilities[1] + row.probabilities[2])
        .collect();
    scores.sort_by(f64::total_cmp);
    targets
        .iter()
        .map(|&target| {
            ensure!(
                target.is_finite() && (0.0..1.0).contains(&target),
                "invalid target FPR"
            );
            let allowed = (target * scores.len() as f64).floor() as usize;
            let threshold = scores[scores.len() - allowed - 1];
            Ok(OperatingPoint {
                target_human_false_positive_rate: target,
                threshold,
                comparison: "strictly_greater_than".into(),
                calibration_human_rows: human.len(),
                calibration_human_groups: human
                    .iter()
                    .map(|row| &row.group)
                    .collect::<BTreeSet<_>>()
                    .len(),
                calibration_false_positives: scores
                    .iter()
                    .filter(|&&score| score > threshold)
                    .count(),
            })
        })
        .collect()
}

fn validate_rows(rows: &[ProbabilityRow]) -> Result<()> {
    ensure!(!rows.is_empty(), "no probability rows");
    let mut ids = BTreeSet::new();
    for row in rows {
        ensure!(
            row.label < 3 && !row.group.is_empty() && !row.id.is_empty(),
            "invalid labeled probability row"
        );
        ensure!(ids.insert(&row.id), "duplicate prediction ID: {}", row.id);
        ensure!(
            row.probabilities
                .iter()
                .all(|p| p.is_finite() && (0.0..=1.0).contains(p)),
            "invalid probability in {}",
            row.id
        );
        ensure!(
            (row.probabilities.iter().sum::<f64>() - 1.0).abs() < 1e-10,
            "probabilities do not sum to one in {}",
            row.id
        );
    }
    Ok(())
}

fn divide(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}

fn rate_estimate(
    rows: &[ProbabilityRow],
    threshold: f64,
    include: impl Fn(usize) -> bool,
) -> RateEstimate {
    let mut groups = BTreeMap::<&str, (usize, usize)>::new();
    for row in rows.iter().filter(|row| include(row.label)) {
        let counts = groups.entry(&row.group).or_default();
        counts.0 += usize::from(row.probabilities[1] + row.probabilities[2] > threshold);
        counts.1 += 1;
    }
    let values: Vec<_> = groups.into_values().collect();
    let events = values.iter().map(|value| value.0).sum();
    let count = values.iter().map(|value| value.1).sum();
    let mut bootstrap = Vec::new();
    if values.len() >= 2 {
        let mut rng = 0x9e3779b97f4a7c15_u64;
        for _ in 0..512 {
            let mut sampled_events = 0;
            let mut sampled_rows = 0;
            for _ in &values {
                rng = rng.wrapping_add(0x9e3779b97f4a7c15);
                let mut bits = rng;
                bits = (bits ^ (bits >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                bits = (bits ^ (bits >> 27)).wrapping_mul(0x94d049bb133111eb);
                bits ^= bits >> 31;
                let pair = values[(bits % values.len() as u64) as usize];
                sampled_events += pair.0;
                sampled_rows += pair.1;
            }
            bootstrap.push(sampled_events as f64 / sampled_rows as f64);
        }
        bootstrap.sort_by(f64::total_cmp);
    }
    let (interval, uncertainty_status) = if bootstrap.is_empty() {
        (None, "insufficient_source_groups")
    } else if bootstrap[12] == bootstrap[499] {
        (None, "degenerate_observed_outcomes")
    } else {
        (
            Some([bootstrap[12], bootstrap[499]]),
            "group_bootstrap_percentile",
        )
    };
    RateEstimate {
        events,
        rows: count,
        source_groups: values.len(),
        rate: divide(events, count),
        group_bootstrap_percentile_95: interval,
        uncertainty_status: uncertainty_status.into(),
        bootstrap_replicates: bootstrap.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_model_objective_ignores_mixed_rows_and_combines_ai_probabilities() {
        let rows: Vec<_> = [[0.8, 0.1, 0.1], [0.1, 0.2, 0.7], [0.999, 0.0005, 0.0005]]
            .into_iter()
            .enumerate()
            .map(|(label, probabilities)| ProbabilityRow {
                id: format!("synthetic-{label}"),
                group: "synthetic-family".into(),
                label,
                probabilities,
            })
            .collect();
        let metric = summarize_human_model(&rows).unwrap().unwrap();
        assert_eq!(
            (
                metric.rows,
                metric.human_rows,
                metric.model_rows,
                metric.excluded_mixed_rows
            ),
            (2, 1, 1, 1)
        );
        assert!((metric.log_loss + (0.8_f64.ln() + 0.9_f64.ln()) / 2.0).abs() < 1e-12);
        assert!((metric.binary_brier - 0.025).abs() < 1e-12);
        assert_eq!(metric.accuracy_at_half, 1.0);
        let mut changed = rows.clone();
        changed[1].probabilities = [0.1, 0.8, 0.1];
        changed[2].probabilities = [0.1, 0.1, 0.8];
        assert!(
            (summarize_human_model(&changed).unwrap().unwrap().log_loss - metric.log_loss).abs()
                < 1e-12
        );
        assert!(summarize_human_model(&rows[2..]).unwrap().is_none());
    }

    #[test]
    fn threshold_ties_and_small_calibration_sets_do_not_invent_fpr_guarantees() {
        let rows: Vec<_> = (0..10)
            .map(|i| ProbabilityRow {
                id: format!("r{i}"),
                group: "one-source".into(),
                label: 0,
                probabilities: [0.8, 0.1, 0.1],
            })
            .collect();
        let point = select_operating_points(&rows, &[0.01]).unwrap().remove(0);
        assert_eq!(point.threshold, 0.2);
        assert_eq!(point.calibration_false_positives, 0);
        let observed = rate_estimate(&rows, point.threshold, |label| label == 0);
        assert_eq!(observed.rate, Some(0.0));
        assert_eq!(observed.source_groups, 1);
        assert_eq!(observed.group_bootstrap_percentile_95, None);
        let independent_groups: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| ProbabilityRow {
                group: format!("group-{i}"),
                ..row.clone()
            })
            .collect();
        let degenerate = rate_estimate(&independent_groups, point.threshold, |label| label == 0);
        assert_eq!(degenerate.group_bootstrap_percentile_95, None);
        assert_eq!(
            degenerate.uncertainty_status,
            "degenerate_observed_outcomes"
        );
        assert!(
            summarize(&[ProbabilityRow {
                probabilities: [0.7, 0.4, 0.0],
                ..rows[0].clone()
            }])
            .is_err()
        );
    }
}
