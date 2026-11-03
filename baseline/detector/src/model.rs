//! Deterministic sparse linear softmax control, with separate calibration.

use crate::features::{FeatureConfig, FeatureRow, FeatureSpace, SparseFeatures};
use crate::metrics::{
    MetricSummary, OperatingPoint, ProbabilityRow, select_operating_points, summarize,
};
use anyhow::{Context, Result, ensure};
use grammar_core::features::Features;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const CLASS_NAMES: [&str; 3] = ["human_only", "model_only", "mixed"];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrainConfig {
    pub features: FeatureConfig,
    pub epochs: usize,
    pub learning_rate: f64,
    pub l2: f64,
    pub patience: usize,
}

impl Default for TrainConfig {
    fn default() -> Self {
        Self {
            features: FeatureConfig::default(),
            epochs: 300,
            learning_rate: 0.03,
            l2: 0.01,
            patience: 25,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetectorContent {
    pub schema: String,
    pub classes: [String; 3],
    pub config: TrainConfig,
    pub feature_space: FeatureSpace,
    pub weights: [Vec<f64>; 3],
    pub bias: [f64; 3],
    pub temperature: f64,
    pub operating_points: Vec<OperatingPoint>,
    pub training_prior: [f64; 3],
    pub training_partition_sha256: String,
    pub development_partition_sha256: String,
    pub calibration_partition_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DetectorArtifact {
    pub content_sha256: String,
    pub content: DetectorContent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prediction {
    pub artifact_sha256: String,
    pub classes: [String; 3],
    pub probabilities: [f64; 3],
    /// Document production-history event, never a fraction of AI-written words.
    pub model_or_assistance_probability: f64,
    pub unknown_coordinates: usize,
    pub unavailable_families: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrainingReport {
    pub artifact_sha256: String,
    pub best_epoch: usize,
    pub epochs_run: usize,
    pub training_rows: usize,
    pub development_rows: usize,
    pub calibration_rows: usize,
    pub training_groups: usize,
    pub development_groups: usize,
    pub calibration_groups: usize,
    pub dimensions: usize,
    pub temperature: f64,
    pub development_uncalibrated: MetricSummary,
    pub calibration_uncalibrated: MetricSummary,
    pub calibration_fitted: MetricSummary,
    #[serde(default)]
    pub epoch_history: Vec<EpochObservation>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EpochObservation {
    pub epoch: usize,
    pub training_log_loss: f64,
    pub development_log_loss: f64,
    pub development_accuracy: f64,
    pub development_recall: [Option<f64>; 3],
}

impl DetectorArtifact {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.content_sha256 == content_hash(&self.content)?,
            "detector artifact content hash mismatch"
        );
        let content = &self.content;
        ensure!(
            content.schema == "slop_ninja_linear_origin_detector_v1",
            "unsupported detector artifact schema"
        );
        ensure!(
            content.classes == CLASS_NAMES.map(str::to_owned),
            "unsupported class ordering"
        );
        content.feature_space.validate()?;
        for class in &content.weights {
            ensure!(
                class.len() == content.feature_space.coordinates.len(),
                "invalid coefficient dimensions"
            );
            ensure!(
                class.iter().all(|x| x.is_finite()),
                "non-finite detector coefficient"
            );
        }
        ensure!(
            content.bias.iter().all(|x| x.is_finite()),
            "non-finite detector bias"
        );
        ensure!(
            content.temperature.is_finite() && content.temperature > 0.0,
            "invalid calibration temperature"
        );
        ensure!(
            content
                .training_prior
                .iter()
                .all(|p| p.is_finite() && *p > 0.0),
            "invalid training prior"
        );
        ensure!(
            (content.training_prior.iter().sum::<f64>() - 1.0).abs() < 1e-10,
            "training prior does not sum to one"
        );
        for point in &content.operating_points {
            point.validate()?;
        }
        Ok(())
    }

    /// After loading, validate once; batch callers can then use this checked
    /// content directly without repeatedly serializing the entire model hash.
    pub fn predict_validated(&self, features: &Features) -> Result<Prediction> {
        let row = self.content.feature_space.transform(features)?;
        let probabilities = probabilities(
            &self.content.weights,
            &self.content.bias,
            &row,
            self.content.temperature,
        )?;
        Ok(Prediction {
            artifact_sha256: self.content_sha256.clone(),
            classes: self.content.classes.clone(),
            probabilities,
            model_or_assistance_probability: probabilities[1] + probabilities[2],
            unknown_coordinates: row.unknown_coordinates,
            unavailable_families: row.unavailable_families,
        })
    }
}

pub fn predict(artifact: &DetectorArtifact, features: &Features) -> Result<Prediction> {
    artifact.validate()?;
    artifact.predict_validated(features)
}

pub fn train(
    train: &[FeatureRow],
    development: &[FeatureRow],
    calibration: &[FeatureRow],
    config: &TrainConfig,
) -> Result<(DetectorArtifact, TrainingReport)> {
    ensure!(
        config.epochs > 0 && config.patience > 0,
        "epochs and patience must be positive"
    );
    ensure!(
        config.learning_rate.is_finite() && config.learning_rate > 0.0,
        "invalid learning rate"
    );
    ensure!(
        config.l2.is_finite() && config.l2 >= 0.0,
        "invalid L2 penalty"
    );
    check_partitions(train, development, calibration)?;
    let space = FeatureSpace::fit(train, &config.features)?;
    let transform = |rows: &[FeatureRow]| -> Result<Vec<SparseFeatures>> {
        rows.iter()
            .map(|row| {
                space
                    .transform(&row.features)
                    .with_context(|| format!("transforming {}", row.id))
            })
            .collect()
    };
    let training_x = transform(train)?;
    let development_x = transform(development)?;
    let calibration_x = transform(calibration)?;
    let dimensions = space.coordinates.len();
    let mut prior = [0.0; 3];
    for row in train {
        prior[row.label] += 1.0 / train.len() as f64;
    }
    let mut weights = std::array::from_fn(|_| vec![0.0; dimensions]);
    let mut bias = prior.map(f64::ln);
    let mut first = std::array::from_fn::<_, 3, _>(|_| vec![0.0; dimensions + 1]);
    let mut second = first.clone();
    let mut best_weights = weights.clone();
    let mut best_bias = bias;
    let mut best_loss = log_loss(&weights, &bias, &development_x, development, 1.0)?;
    let mut best_epoch = 0;
    let mut epochs_run = 0;
    let observation =
        |epoch, weights: &[Vec<f64>; 3], bias: &[f64; 3], loss| -> Result<EpochObservation> {
            let dev = summarize(&probability_rows(
                development,
                &development_x,
                weights,
                bias,
                1.0,
            )?)?;
            Ok(EpochObservation {
                epoch,
                training_log_loss: log_loss(weights, bias, &training_x, train, 1.0)?,
                development_log_loss: loss,
                development_accuracy: dev.accuracy,
                development_recall: dev.classes.map(|class| class.recall),
            })
        };
    let mut epoch_history = vec![observation(0, &weights, &bias, best_loss)?];
    for epoch in 1..=config.epochs {
        let mut gradient = std::array::from_fn::<_, 3, _>(|_| vec![0.0; dimensions + 1]);
        for (row, x) in train.iter().zip(&training_x) {
            let p = probabilities(&weights, &bias, x, 1.0)?;
            for class in 0..3 {
                let residual = (p[class] - f64::from(row.label == class)) / train.len() as f64;
                gradient[class][dimensions] += residual;
                for &(index, value) in &x.values {
                    gradient[class][index] += residual * value;
                }
            }
        }
        let correction1 = 1.0 - 0.9_f64.powf(epoch as f64);
        let correction2 = 1.0 - 0.999_f64.powf(epoch as f64);
        for class in 0..3 {
            for index in 0..=dimensions {
                let coefficient = if index == dimensions {
                    &mut bias[class]
                } else {
                    &mut weights[class][index]
                };
                let derivative = gradient[class][index]
                    + if index < dimensions {
                        config.l2 * *coefficient
                    } else {
                        0.0
                    };
                first[class][index] = 0.9 * first[class][index] + 0.1 * derivative;
                second[class][index] =
                    0.999 * second[class][index] + 0.001 * derivative * derivative;
                *coefficient -= config.learning_rate * (first[class][index] / correction1)
                    / ((second[class][index] / correction2).sqrt() + 1e-8);
            }
        }
        let loss = log_loss(&weights, &bias, &development_x, development, 1.0)?;
        ensure!(loss.is_finite(), "training diverged at epoch {epoch}");
        epoch_history.push(observation(epoch, &weights, &bias, loss)?);
        epochs_run = epoch;
        if loss < best_loss - 1e-9 {
            best_loss = loss;
            best_epoch = epoch;
            best_weights.clone_from(&weights);
            best_bias = bias;
        }
        if epoch - best_epoch >= config.patience {
            break;
        }
    }
    let temperature = fit_temperature(&best_weights, &best_bias, &calibration_x, calibration)?;
    let development_predictions =
        probability_rows(development, &development_x, &best_weights, &best_bias, 1.0)?;
    let calibration_uncalibrated =
        probability_rows(calibration, &calibration_x, &best_weights, &best_bias, 1.0)?;
    let calibration_fitted = probability_rows(
        calibration,
        &calibration_x,
        &best_weights,
        &best_bias,
        temperature,
    )?;
    let content = DetectorContent {
        schema: "slop_ninja_linear_origin_detector_v1".into(),
        classes: CLASS_NAMES.map(str::to_owned),
        config: config.clone(),
        feature_space: space,
        weights: best_weights,
        bias: best_bias,
        temperature,
        operating_points: select_operating_points(&calibration_fitted, &[0.01, 0.05])?,
        training_prior: prior,
        training_partition_sha256: partition_hash(train)?,
        development_partition_sha256: partition_hash(development)?,
        calibration_partition_sha256: partition_hash(calibration)?,
    };
    let artifact = DetectorArtifact {
        content_sha256: content_hash(&content)?,
        content,
    };
    artifact.validate()?;
    let groups = |rows: &[FeatureRow]| {
        rows.iter()
            .map(|row| &row.group)
            .collect::<BTreeSet<_>>()
            .len()
    };
    let report = TrainingReport {
        artifact_sha256: artifact.content_sha256.clone(), best_epoch, epochs_run,
        training_rows: train.len(), development_rows: development.len(), calibration_rows: calibration.len(),
        training_groups: groups(train), development_groups: groups(development), calibration_groups: groups(calibration), dimensions, temperature,
        development_uncalibrated: summarize(&development_predictions)?,
        calibration_uncalibrated: summarize(&calibration_uncalibrated)?, calibration_fitted: summarize(&calibration_fitted)?,
        epoch_history,
        notes: vec![
            "Linear three-class experimental control; reference status does not establish Pangram parity.".into(),
            "Rows have equal training weight. Source-group counts and group-resampled uncertainty are reported separately.".into(),
            "Vocabulary and RMS scales use training only; early stopping uses development only; temperature and operating points use calibration only.".into(),
            "Calibration-set scores are fitted diagnostics, not an independent performance estimate. Final testing is a separate command.".into(),
        ],
    };
    Ok((artifact, report))
}

fn check_partitions(
    train: &[FeatureRow],
    development: &[FeatureRow],
    calibration: &[FeatureRow],
) -> Result<()> {
    let mut global_groups = BTreeSet::new();
    let mut global_ids = BTreeSet::new();
    let mut global_texts = BTreeSet::new();
    for (name, rows) in [
        ("training", train),
        ("development", development),
        ("calibration", calibration),
    ] {
        ensure!(!rows.is_empty(), "empty {name} partition");
        let mut classes = [0_usize; 3];
        let mut groups = BTreeSet::new();
        let mut texts = BTreeSet::new();
        for row in rows {
            ensure!(row.label < 3, "unknown class in {}", row.id);
            ensure!(
                !row.id.is_empty()
                    && !row.group.is_empty()
                    && !row.features.source_sha256.is_empty(),
                "missing row identity, source group or text hash"
            );
            ensure!(
                global_ids.insert(&row.id),
                "duplicate example ID: {}",
                row.id
            );
            ensure!(
                !global_groups.contains(&row.group),
                "source-group leakage into {name}: {}",
                row.group
            );
            ensure!(
                !global_texts.contains(&row.features.source_sha256),
                "exact-text leakage into {name}: {}",
                row.id
            );
            classes[row.label] += 1;
            groups.insert(&row.group);
            texts.insert(&row.features.source_sha256);
        }
        ensure!(
            classes.iter().all(|count| *count > 0),
            "{name} must contain all three documented origin classes; missing mixed workflows cannot be fabricated"
        );
        global_groups.extend(groups);
        global_texts.extend(texts);
    }
    Ok(())
}

fn content_hash(content: &DetectorContent) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(content)?)))
}

fn partition_hash(rows: &[FeatureRow]) -> Result<String> {
    let mut identities: Vec<_> = rows
        .iter()
        .map(|row| (&row.id, &row.group, row.label, &row.features.source_sha256))
        .collect();
    identities.sort();
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(
        &identities,
    )?)))
}

fn probabilities(
    weights: &[Vec<f64>; 3],
    bias: &[f64; 3],
    x: &SparseFeatures,
    temperature: f64,
) -> Result<[f64; 3]> {
    let logits = std::array::from_fn(|class| {
        (bias[class]
            + x.values
                .iter()
                .map(|&(index, value)| weights[class][index] * value)
                .sum::<f64>())
            / temperature
    });
    softmax(logits)
}

fn softmax(logits: [f64; 3]) -> Result<[f64; 3]> {
    ensure!(
        logits.iter().all(|x| x.is_finite()),
        "non-finite inference logits"
    );
    let maximum = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exponentials = logits.map(|x| (x - maximum).exp());
    let total: f64 = exponentials.iter().sum();
    Ok(exponentials.map(|x| x / total))
}

fn log_loss(
    weights: &[Vec<f64>; 3],
    bias: &[f64; 3],
    x: &[SparseFeatures],
    rows: &[FeatureRow],
    temperature: f64,
) -> Result<f64> {
    let mut loss = 0.0;
    for (x, row) in x.iter().zip(rows) {
        loss -= probabilities(weights, bias, x, temperature)?[row.label]
            .max(1e-15)
            .ln();
    }
    Ok(loss / rows.len() as f64)
}

fn fit_temperature(
    weights: &[Vec<f64>; 3],
    bias: &[f64; 3],
    x: &[SparseFeatures],
    rows: &[FeatureRow],
) -> Result<f64> {
    // Deterministic bounded scalar search; the unscaled checkpoint remains a
    // candidate, so calibration cannot worsen its own fitted objective.
    let mut best_log = 0.0_f64;
    let mut best_loss = log_loss(weights, bias, x, rows, 1.0)?;
    for step in -60..=60 {
        let log = f64::from(step) * 0.05;
        let loss = log_loss(weights, bias, x, rows, log.exp())?;
        if loss < best_loss {
            best_loss = loss;
            best_log = log;
        }
    }
    for scale in [0.005, 0.0005, 0.00005] {
        let center = best_log;
        for step in -10..=10 {
            let log = (center + f64::from(step) * scale).clamp(-3.0, 3.0);
            let loss = log_loss(weights, bias, x, rows, log.exp())?;
            if loss < best_loss {
                best_loss = loss;
                best_log = log;
            }
        }
    }
    Ok(best_log.exp())
}

fn probability_rows(
    rows: &[FeatureRow],
    x: &[SparseFeatures],
    weights: &[Vec<f64>; 3],
    bias: &[f64; 3],
    temperature: f64,
) -> Result<Vec<ProbabilityRow>> {
    rows.iter()
        .zip(x)
        .map(|(row, x)| {
            Ok(ProbabilityRow {
                id: row.id.clone(),
                group: row.group.clone(),
                label: row.label,
                probabilities: probabilities(weights, bias, x, temperature)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::tests::row;

    fn partition(prefix: &str) -> Vec<FeatureRow> {
        ["alpha", "beta", "gamma"]
            .iter()
            .enumerate()
            .flat_map(|(label, word)| {
                (0..4).map(move |i| row(&format!("{prefix}-{label}-{i}"), label, word))
            })
            .collect()
    }

    #[test]
    fn train_calibrate_roundtrip_and_detect_tampering() {
        let (artifact, report) = train(
            &partition("train"),
            &partition("dev"),
            &partition("cal"),
            &TrainConfig {
                epochs: 80,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(report.development_uncalibrated.log_loss < 0.2);
        assert_eq!(report.epoch_history.len(), report.epochs_run + 1);
        assert_eq!(
            report.epoch_history[report.best_epoch].development_log_loss,
            report.development_uncalibrated.log_loss
        );
        assert!(
            report
                .epoch_history
                .iter()
                .all(|e| e.training_log_loss.is_finite()
                    && e.development_log_loss.is_finite()
                    && e.development_recall.iter().all(Option::is_some))
        );
        assert!(
            report.calibration_fitted.log_loss <= report.calibration_uncalibrated.log_loss + 1e-12
        );
        let bytes = serde_json::to_vec(&artifact).unwrap();
        let mut restored: DetectorArtifact = serde_json::from_slice(&bytes).unwrap();
        let p = predict(&restored, &row("new", 1, "beta").features).unwrap();
        assert!(p.probabilities[1] > 0.9);
        assert!((p.probabilities.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        restored.content.bias[0] += 0.01;
        assert!(predict(&restored, &row("new", 1, "beta").features).is_err());
    }

    #[test]
    fn reject_partition_leakage_and_missing_mixed() {
        let train_rows = partition("train");
        let mut dev = partition("dev");
        dev[0].group = train_rows[0].group.clone();
        assert!(
            train(
                &train_rows,
                &dev,
                &partition("cal"),
                &TrainConfig::default()
            )
            .is_err()
        );
        dev = partition("dev")
            .into_iter()
            .filter(|row| row.label != 2)
            .collect();
        assert!(
            train(
                &train_rows,
                &dev,
                &partition("cal"),
                &TrainConfig::default()
            )
            .is_err()
        );
    }
}
