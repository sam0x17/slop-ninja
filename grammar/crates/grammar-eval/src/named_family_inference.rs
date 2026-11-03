//! Portable positive family calibration. Distances are supplied in a bound,
//! versioned geometry; this module never fits scales, profiles or parameters.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SCHEMA: &str = "slopninja-named-family-metric-v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub schema: String,
    pub families: Vec<String>,
    pub source_space_id: String,
    pub reference_space_sha256: String,
    pub feature_schema: String,
    pub parser_identity: String,
    pub family_weights: Vec<f64>,
    pub knots: Vec<f64>,
    pub slopes: Vec<f64>,
    pub temperature: f64,
    pub provenance_sha256: BTreeMap<String, String>,
}

pub fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}

pub struct Metric {
    artifact: Artifact,
    left_edges: Vec<f64>,
    areas: Vec<f64>,
}

impl Metric {
    pub fn new(artifact: Artifact) -> Result<Self> {
        ensure!(
            artifact.schema == SCHEMA,
            "unsupported named-family metric schema"
        );
        ensure!(
            valid_hash(&artifact.source_space_id)
                && valid_hash(&artifact.reference_space_sha256)
                && !artifact.feature_schema.is_empty()
                && !artifact.parser_identity.is_empty(),
            "invalid model identity"
        );
        ensure!(
            !artifact.families.is_empty()
                && artifact.families.iter().all(|name| !name.is_empty())
                && artifact.families.windows(2).all(|p| p[0] < p[1]),
            "families must be sorted, unique and nonempty"
        );
        ensure!(
            artifact.family_weights.len() == artifact.families.len()
                && artifact
                    .family_weights
                    .iter()
                    .all(|x| x.is_finite() && *x > 0.)
                && (artifact.family_weights.iter().sum::<f64>() - 1.).abs() <= 1e-12,
            "invalid normalized positive family weights"
        );
        ensure!(
            !artifact.knots.is_empty()
                && artifact.knots.iter().all(|x| x.is_finite() && *x > 0.)
                && artifact.knots.windows(2).all(|p| p[0] < p[1]),
            "invalid shared spline knots"
        );
        ensure!(
            artifact.slopes.len() == artifact.knots.len() + 1
                && artifact.slopes[0] == 1.
                && artifact.slopes.iter().all(|x| x.is_finite() && *x > 0.),
            "invalid positive shared spline slopes"
        );
        ensure!(
            artifact.temperature.is_finite() && artifact.temperature > 0.,
            "invalid positive temperature"
        );
        ensure!(
            [
                "selection",
                "protocol",
                "calibration",
                "implementation",
                "checkpoint"
            ]
            .iter()
            .all(|key| artifact.provenance_sha256.contains_key(*key))
                && artifact
                    .provenance_sha256
                    .values()
                    .all(|value| valid_hash(value)),
            "missing or invalid model provenance"
        );
        let mut left_edges = vec![0.];
        let mut areas = vec![0.];
        for (i, knot) in artifact.knots.iter().enumerate() {
            let area = areas[i] + artifact.slopes[i] * (knot - left_edges[i]);
            ensure!(area.is_finite(), "spline integral overflow");
            areas.push(area);
            left_edges.push(*knot);
        }
        Ok(Self {
            artifact,
            left_edges,
            areas,
        })
    }
    pub fn artifact(&self) -> &Artifact {
        &self.artifact
    }

    /// Python torch.bucketize(right=false) chooses the interval ending at an
    /// exact knot. Positive slopes also continue beyond the final knot.
    pub fn calibrate(&self, distance: f64) -> Result<f64> {
        ensure!(
            distance.is_finite() && distance >= 0.,
            "distance must be finite and nonnegative"
        );
        let i = self.artifact.knots.partition_point(|knot| *knot < distance);
        let value = self.areas[i] + self.artifact.slopes[i] * (distance - self.left_edges[i]);
        ensure!(
            value.is_finite() && value >= 0.,
            "calibrated distance overflow"
        );
        Ok(value)
    }
    /// Larger logits rank closer candidates first. A logit is not a probability.
    pub fn score_distances(&self, distances: &[f64]) -> Result<f64> {
        ensure!(
            distances.len() == self.artifact.families.len(),
            "family distance count differs"
        );
        let mut total = 0.;
        for (distance, weight) in distances.iter().zip(&self.artifact.family_weights) {
            total += self.calibrate(*distance)? * weight;
        }
        let score = -total / self.artifact.temperature;
        ensure!(score.is_finite(), "nonfinite logit");
        Ok(score)
    }
    /// Require exactly the model's named families; callers bind parser/schema
    /// and original reference geometry before passing bare distance values.
    #[allow(dead_code)]
    pub fn score_named(&self, distances: &BTreeMap<String, f64>) -> Result<f64> {
        ensure!(
            distances.keys().eq(self.artifact.families.iter()),
            "named distance family set differs"
        );
        self.score_distances(&distances.values().copied().collect::<Vec<_>>())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub fn artifact() -> Artifact {
        Artifact {
            schema: SCHEMA.into(),
            families: vec!["grammar".into(), "word".into()],
            source_space_id: "a".repeat(64),
            reference_space_sha256: "b".repeat(64),
            feature_schema: "synthetic-features-v1".into(),
            parser_identity: "synthetic-parser-v1".into(),
            family_weights: vec![0.25, 0.75],
            knots: vec![1., 2.],
            slopes: vec![1., 2., 0.5],
            temperature: 0.5,
            provenance_sha256: [
                "selection",
                "protocol",
                "calibration",
                "implementation",
                "checkpoint",
            ]
            .into_iter()
            .map(|k| (k.into(), "c".repeat(64)))
            .collect(),
        }
    }
    #[test]
    fn spline_boundaries_and_tail_are_continuous_and_increasing() {
        let metric = Metric::new(artifact()).unwrap();
        assert_eq!(metric.calibrate(0.).unwrap(), 0.);
        assert_eq!(metric.calibrate(1.).unwrap(), 1.);
        assert_eq!(metric.calibrate(2.).unwrap(), 3.);
        assert_eq!(metric.calibrate(4.).unwrap(), 4.);
        let mut previous = -1.;
        for value in [
            0.,
            0.5,
            1. - 1e-12,
            1.,
            1. + 1e-12,
            1.5,
            2. - 1e-12,
            2.,
            2. + 1e-12,
            3.,
            1e6,
        ] {
            let current = metric.calibrate(value).unwrap();
            assert!(current > previous);
            previous = current;
        }
        assert_eq!(metric.score_distances(&[1., 2.]).unwrap(), -5.);
    }
    #[test]
    fn weights_and_temperature_preserve_coordinatewise_order() {
        let metric = Metric::new(artifact()).unwrap();
        let base = metric.score_distances(&[0.5, 0.5]).unwrap();
        assert!(metric.score_distances(&[0.6, 0.5]).unwrap() < base);
        assert!(metric.score_distances(&[0.5, 0.6]).unwrap() < base);
        let named = BTreeMap::from([("grammar".into(), 1.), ("word".into(), 2.)]);
        assert_eq!(metric.score_named(&named).unwrap(), -5.);
        assert!(
            metric
                .score_named(&BTreeMap::from([("word".into(), 1.)]))
                .is_err()
        );
    }
    #[test]
    fn malformed_artifacts_and_nonfinite_inputs_are_rejected() {
        let mut variants = vec![];
        let mut bad = artifact();
        bad.schema = "unknown".into();
        variants.push(bad);
        let mut bad = artifact();
        bad.families.reverse();
        variants.push(bad);
        let mut bad = artifact();
        bad.families[1] = "grammar".into();
        variants.push(bad);
        let mut bad = artifact();
        bad.family_weights[0] = 0.;
        variants.push(bad);
        let mut bad = artifact();
        bad.family_weights[0] = 0.5;
        variants.push(bad);
        let mut bad = artifact();
        bad.temperature = f64::INFINITY;
        variants.push(bad);
        let mut bad = artifact();
        bad.temperature = 0.;
        variants.push(bad);
        let mut bad = artifact();
        bad.slopes[0] = 2.;
        variants.push(bad);
        let mut bad = artifact();
        bad.slopes[1] = -1.;
        variants.push(bad);
        let mut bad = artifact();
        bad.knots[1] = 1.;
        variants.push(bad);
        let mut bad = artifact();
        bad.provenance_sha256.remove("checkpoint");
        variants.push(bad);
        let mut bad = artifact();
        bad.reference_space_sha256 = "wrong".into();
        variants.push(bad);
        for bad in variants {
            assert!(Metric::new(bad).is_err());
        }
        let metric = Metric::new(artifact()).unwrap();
        for values in [
            vec![1.],
            vec![f64::NAN, 0.],
            vec![-1., 0.],
            vec![f64::INFINITY, 0.],
        ] {
            assert!(metric.score_distances(&values).is_err());
        }
        let mut value = serde_json::to_value(artifact()).unwrap();
        value["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Artifact>(value).is_err());
    }
}
