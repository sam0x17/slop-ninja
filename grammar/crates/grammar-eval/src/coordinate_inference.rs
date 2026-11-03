//! Portable inference for positive coordinate weights over complete family distances.
//! No fitting, parser, tensor runtime, or vocabulary truncation is involved.
use anyhow::{Result, ensure};
use grammar_core::space::Kind;
use grammar_eval::{DistanceGeometry, Profile, family_distances};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SCHEMA: &str = "slopninja-positive-coordinate-metric-v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NumericAxis {
    pub feature: String,
    pub scale: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyGeometry {
    pub name: String,
    pub kind: Kind,
    pub original_axis_count: usize,
    /// Every original numerical axis, including axes without learned multipliers.
    pub numerical_axes: Vec<NumericAxis>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coordinate {
    pub family_index: usize,
    pub family: String,
    pub feature: String,
    pub kind: Kind,
    pub scale: f64,
    pub original_family_axis_count: usize,
    pub log_multiplier: f64,
    pub multiplier: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub schema: String,
    pub source_space_id: String,
    pub feature_schema: String,
    pub parser_identity: String,
    pub families: Vec<FamilyGeometry>,
    pub coordinates: Vec<Coordinate>,
    pub family_weights: Vec<f64>,
    pub knots: Vec<f64>,
    pub slopes: Vec<f64>,
    pub temperature: f64,
    pub provenance_sha256: BTreeMap<String, String>,
}

/// Validate once, then reuse for many query/candidate pairs.
pub struct Metric {
    artifact: Artifact,
    geometry: DistanceGeometry,
    corrections: Vec<f64>,
    left_edges: Vec<f64>,
    areas: Vec<f64>,
}

#[derive(Debug, Serialize)]
pub struct PairScore {
    /// Larger logits rank closer candidates first. This is not a probability.
    pub logit: f64,
    pub adjusted_family_distances: Vec<f64>,
    pub minimum_unselected_tail: f64,
}

impl Metric {
    pub fn new(artifact: Artifact) -> Result<Self> {
        ensure!(
            artifact.schema == SCHEMA,
            "unsupported coordinate metric schema"
        );
        ensure!(
            !artifact.source_space_id.is_empty()
                && !artifact.feature_schema.is_empty()
                && !artifact.parser_identity.is_empty(),
            "missing model identity"
        );
        ensure!(
            !artifact.families.is_empty() && !artifact.coordinates.is_empty(),
            "empty metric"
        );
        ensure!(
            artifact
                .families
                .windows(2)
                .all(|pair| pair[0].name < pair[1].name),
            "families duplicated or unsorted"
        );
        ensure!(
            artifact.family_weights.len() == artifact.families.len()
                && artifact
                    .family_weights
                    .iter()
                    .all(|w| w.is_finite() && *w > 0.0)
                && (artifact.family_weights.iter().sum::<f64>() - 1.0).abs() <= 1e-12,
            "invalid normalized positive family weights"
        );
        ensure!(
            artifact.temperature.is_finite() && artifact.temperature > 0.0,
            "invalid temperature"
        );
        ensure!(
            artifact.knots.iter().all(|x| x.is_finite() && *x > 0.0)
                && artifact.knots.windows(2).all(|pair| pair[0] < pair[1]),
            "invalid spline knots"
        );
        ensure!(
            artifact.slopes.len() == artifact.knots.len() + 1
                && artifact.slopes.iter().all(|s| s.is_finite() && *s > 0.0)
                && artifact.slopes[0] == 1.0,
            "invalid monotone spline slopes"
        );
        ensure!(
            !artifact.provenance_sha256.is_empty()
                && artifact.provenance_sha256.values().all(|h| h.len() == 64
                    && h.bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())),
            "invalid provenance digest"
        );
        ensure!(
            [
                "catalog",
                "space",
                "selection",
                "protocol",
                "coordinate_checkpoint",
                "baseline_checkpoint"
            ]
            .iter()
            .all(|key| artifact.provenance_sha256.contains_key(*key)),
            "missing model source binding"
        );
        let mut geometry = DistanceGeometry {
            families: BTreeMap::new(),
            numerical_scales: BTreeMap::new(),
        };
        for family in &artifact.families {
            ensure!(
                !family.name.is_empty() && family.original_axis_count > 0,
                "invalid family geometry"
            );
            geometry
                .families
                .insert(family.name.clone(), family.kind.clone());
            if family.kind == Kind::Distribution {
                ensure!(
                    family.numerical_axes.is_empty(),
                    "categorical family contains numeric transforms"
                );
            } else {
                ensure!(
                    family.numerical_axes.len() == family.original_axis_count
                        && family
                            .numerical_axes
                            .windows(2)
                            .all(|pair| pair[0].feature < pair[1].feature)
                        && family
                            .numerical_axes
                            .iter()
                            .all(|a| !a.feature.is_empty() && a.scale.is_finite() && a.scale > 0.0),
                    "incomplete or invalid original numeric geometry"
                );
                geometry.numerical_scales.insert(
                    family.name.clone(),
                    family
                        .numerical_axes
                        .iter()
                        .map(|a| (a.feature.clone(), a.scale))
                        .collect(),
                );
            }
        }
        ensure!(
            artifact
                .coordinates
                .windows(2)
                .all(|pair| (&pair[0].family, &pair[0].feature)
                    < (&pair[1].family, &pair[1].feature)),
            "coordinates duplicated or unsorted"
        );
        let mut corrections = Vec::new();
        for coordinate in &artifact.coordinates {
            let family = artifact
                .families
                .get(coordinate.family_index)
                .ok_or_else(|| anyhow::anyhow!("coordinate family index out of bounds"))?;
            ensure!(
                coordinate.family == family.name
                    && coordinate.kind == family.kind
                    && !coordinate.feature.is_empty()
                    && coordinate.scale.is_finite()
                    && coordinate.scale > 0.0
                    && coordinate.original_family_axis_count == family.original_axis_count,
                "coordinate transform differs from family geometry"
            );
            if coordinate.kind == Kind::Distribution {
                ensure!(
                    coordinate.scale == 1.0,
                    "categorical transform scale must be one"
                );
            } else {
                ensure!(
                    family
                        .numerical_axes
                        .iter()
                        .any(|axis| axis.feature == coordinate.feature
                            && axis.scale == coordinate.scale),
                    "selected numeric transform absent from original geometry"
                );
            }
            ensure!(
                coordinate.log_multiplier.is_finite()
                    && coordinate.multiplier.is_finite()
                    && coordinate.multiplier > 0.0
                    && (coordinate.log_multiplier.exp() - coordinate.multiplier).abs()
                        <= 1e-14 * coordinate.multiplier,
                "invalid positive coordinate multiplier"
            );
            let correction = coordinate.log_multiplier.exp_m1();
            ensure!(correction.is_finite(), "nonfinite coordinate correction");
            corrections.push(correction);
        }
        let mut left_edges = vec![0.0];
        left_edges.extend_from_slice(&artifact.knots);
        let mut areas = vec![0.0];
        for (i, edges) in left_edges.windows(2).enumerate() {
            let area = areas[i] + artifact.slopes[i] * (edges[1] - edges[0]);
            ensure!(area.is_finite(), "spline integral overflow");
            areas.push(area);
        }
        Ok(Self {
            artifact,
            geometry,
            corrections,
            left_edges,
            areas,
        })
    }

    pub fn artifact(&self) -> &Artifact {
        &self.artifact
    }

    fn validate_profile(&self, profile: &Profile) -> Result<()> {
        ensure!(
            profile.keys().eq(self.geometry.families.keys()),
            "profile family schema mismatch"
        );
        for family in &self.artifact.families {
            let values = &profile[&family.name];
            ensure!(
                values.values().all(|v| v.is_finite()),
                "nonfinite profile value"
            );
            if family.kind == Kind::Distribution {
                ensure!(
                    values.values().all(|v| *v >= 0.0)
                        && (values.values().sum::<f64>() - 1.0).abs() < 1e-9,
                    "invalid probability distribution"
                );
            } else {
                ensure!(
                    values
                        .keys()
                        .eq(family.numerical_axes.iter().map(|axis| &axis.feature)),
                    "missing or unknown numeric profile axis"
                );
            }
        }
        Ok(())
    }

    /// Profiles contain raw probabilities/measurements, already averaged if appropriate.
    pub fn transform_profile(&self, profile: &Profile) -> Result<Vec<f64>> {
        self.validate_profile(profile)?;
        self.artifact
            .coordinates
            .iter()
            .map(|coordinate| {
                let value = profile[&coordinate.family]
                    .get(&coordinate.feature)
                    .copied()
                    .unwrap_or(0.0);
                let transformed = if coordinate.kind == Kind::Distribution {
                    (value / 2.0).sqrt()
                } else {
                    value
                        / (coordinate.scale * (coordinate.original_family_axis_count as f64).sqrt())
                };
                ensure!(transformed.is_finite(), "profile transform overflow");
                Ok(transformed)
            })
            .collect()
    }

    /// Preserve every unselected/unseen contribution through the complete family distances.
    pub fn score_pair(&self, raw: &[f64], query: &[f64], target: &[f64]) -> Result<PairScore> {
        ensure!(
            raw.len() == self.artifact.families.len()
                && query.len() == self.corrections.len()
                && target.len() == query.len(),
            "score input dimensions differ"
        );
        ensure!(
            raw.iter().all(|d| d.is_finite() && *d >= 0.0)
                && query.iter().chain(target).all(|v| v.is_finite()),
            "nonfinite or negative score input"
        );
        let mut selected = vec![0.0; raw.len()];
        let mut delta = vec![0.0; raw.len()];
        for (((coordinate, correction), left), right) in self
            .artifact
            .coordinates
            .iter()
            .zip(&self.corrections)
            .zip(query)
            .zip(target)
        {
            if coordinate.kind == Kind::Distribution {
                ensure!(
                    *left >= 0.0 && *right >= 0.0,
                    "negative square-root probability coordinate"
                );
            }
            let distance = (left - right).powi(2);
            selected[coordinate.family_index] += distance;
            delta[coordinate.family_index] += correction * distance;
        }
        let mut adjusted = Vec::new();
        let mut minimum_tail = f64::INFINITY;
        let mut total = 0.0;
        for (i, full) in raw.iter().enumerate() {
            let tail = full - selected[i];
            ensure!(
                tail.is_finite() && tail >= -1e-9 * (1.0 + full.abs()),
                "selected contribution exceeds complete family distance"
            );
            minimum_tail = minimum_tail.min(tail);
            let distance = full + delta[i];
            ensure!(
                distance.is_finite() && distance >= -1e-9,
                "invalid adjusted family distance"
            );
            let distance = distance.max(0.0);
            let interval = self.artifact.knots.partition_point(|knot| *knot < distance);
            let calibrated = self.areas[interval]
                + self.artifact.slopes[interval] * (distance - self.left_edges[interval]);
            total += calibrated * self.artifact.family_weights[i];
            adjusted.push(distance);
        }
        let logit = -total / self.artifact.temperature;
        ensure!(logit.is_finite(), "nonfinite score");
        Ok(PairScore {
            logit,
            adjusted_family_distances: adjusted,
            minimum_unselected_tail: minimum_tail,
        })
    }

    /// Bare Profiles carry no parser/schema identity. Callers must bind their provenance
    /// to this artifact before calling; the CLI checks an explicit identity envelope.
    pub fn score_profiles(&self, query: &Profile, target: &Profile) -> Result<PairScore> {
        let q = self.transform_profile(query)?;
        let p = self.transform_profile(target)?;
        let full = family_distances(&self.geometry, query, target)?;
        self.score_pair(
            &self
                .artifact
                .families
                .iter()
                .map(|f| full[&f.name])
                .collect::<Vec<_>>(),
            &q,
            &p,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn artifact() -> Artifact {
        Artifact {
            schema: SCHEMA.into(),
            source_space_id: "synthetic-space".into(),
            feature_schema: "synthetic-features".into(),
            parser_identity: "synthetic-parser".into(),
            families: vec![
                FamilyGeometry {
                    name: "numeric".into(),
                    kind: Kind::Metric,
                    original_axis_count: 2,
                    numerical_axes: vec![
                        NumericAxis {
                            feature: "selected".into(),
                            scale: 2.0,
                        },
                        NumericAxis {
                            feature: "tail".into(),
                            scale: 4.0,
                        },
                    ],
                },
                FamilyGeometry {
                    name: "word".into(),
                    kind: Kind::Distribution,
                    original_axis_count: 3,
                    numerical_axes: vec![],
                },
            ],
            coordinates: vec![
                Coordinate {
                    family_index: 0,
                    family: "numeric".into(),
                    feature: "selected".into(),
                    kind: Kind::Metric,
                    scale: 2.0,
                    original_family_axis_count: 2,
                    log_multiplier: 2.0_f64.ln(),
                    multiplier: 2.0,
                },
                Coordinate {
                    family_index: 1,
                    family: "word".into(),
                    feature: "selected".into(),
                    kind: Kind::Distribution,
                    scale: 1.0,
                    original_family_axis_count: 3,
                    log_multiplier: 0.5_f64.ln(),
                    multiplier: 0.5,
                },
            ],
            family_weights: vec![0.4, 0.6],
            knots: vec![0.5, 1.0],
            slopes: vec![1.0, 2.0, 0.25],
            temperature: 0.5,
            provenance_sha256: [
                "catalog",
                "space",
                "selection",
                "protocol",
                "coordinate_checkpoint",
                "baseline_checkpoint",
            ]
            .into_iter()
            .map(|name| (name.into(), "0".repeat(64)))
            .collect(),
        }
    }
    #[test]
    fn profile_scoring_preserves_unseen_words_and_unselected_numeric_mass() {
        let metric = Metric::new(artifact()).unwrap();
        let query = BTreeMap::from([
            (
                "numeric".into(),
                BTreeMap::from([("selected".into(), 2.0), ("tail".into(), 4.0)]),
            ),
            (
                "word".into(),
                BTreeMap::from([("selected".into(), 0.25), ("unseen-query".into(), 0.75)]),
            ),
        ]);
        let target = BTreeMap::from([
            (
                "numeric".into(),
                BTreeMap::from([("selected".into(), 0.0), ("tail".into(), 0.0)]),
            ),
            ("word".into(), BTreeMap::from([("selected".into(), 1.0)])),
        ]);
        let score = metric.score_profiles(&query, &target).unwrap();
        assert!((score.adjusted_family_distances[0] - 1.5).abs() < 1e-14);
        assert!((score.adjusted_family_distances[1] - 0.4375).abs() < 1e-14);
        assert!((score.logit + 1.825).abs() < 1e-14);
        let q = metric.transform_profile(&query).unwrap();
        assert!((q[0] - 1.0 / 2.0_f64.sqrt()).abs() < 1e-14);
        assert_eq!(metric.score_profiles(&query, &query).unwrap().logit, -0.0);
        let mut incomplete = query.clone();
        incomplete.get_mut("numeric").unwrap().remove("tail");
        assert!(metric.score_profiles(&incomplete, &target).is_err());
    }
    #[test]
    fn invalid_schema_weights_transforms_and_tails_fail() {
        let mut unknown = serde_json::to_value(artifact()).unwrap();
        unknown["unknown_field"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Artifact>(unknown).is_err());
        let mut a = artifact();
        a.schema = "unknown".into();
        assert!(Metric::new(a).is_err());
        let mut a = artifact();
        a.coordinates[0].multiplier = 0.0;
        assert!(Metric::new(a).is_err());
        let mut a = artifact();
        a.coordinates[0].original_family_axis_count = 1;
        assert!(Metric::new(a).is_err());
        let mut a = artifact();
        a.family_weights[0] = -0.4;
        assert!(Metric::new(a).is_err());
        let mut a = artifact();
        a.slopes[1] = -1.0;
        assert!(Metric::new(a).is_err());
        let mut a = artifact();
        a.knots.reverse();
        assert!(Metric::new(a).is_err());
        let mut a = artifact();
        a.coordinates[0].log_multiplier = 0.0;
        assert!(Metric::new(a).is_err());
        let metric = Metric::new(artifact()).unwrap();
        assert!(
            metric
                .score_pair(&[0.0, 0.0], &[1.0, 0.0], &[0.0, 0.0])
                .is_err()
        );
        assert!(
            metric
                .score_pair(&[1.0, 1.0], &[f64::NAN, 0.0], &[0.0, 0.0])
                .is_err()
        );
    }
    #[test]
    fn identity_multipliers_and_spline_knots_are_continuous_and_monotone() {
        let mut a = artifact();
        for c in &mut a.coordinates {
            c.log_multiplier = 0.0;
            c.multiplier = 1.0;
        }
        let metric = Metric::new(a).unwrap();
        let mut previous = 0.0;
        for distance in [
            0.0,
            0.5 - 1e-10,
            0.5,
            0.5 + 1e-10,
            1.0 - 1e-10,
            1.0,
            1.0 + 1e-10,
            8.0,
        ] {
            let score = metric
                .score_pair(&[distance, distance], &[0.0, 0.0], &[0.0, 0.0])
                .unwrap();
            assert_eq!(score.adjusted_family_distances, vec![distance, distance]);
            assert!(score.logit <= previous);
            previous = score.logit;
        }
        assert!(
            (metric
                .score_pair(&[0.5, 0.5], &[0.0, 0.0], &[0.0, 0.0])
                .unwrap()
                .logit
                + 1.0)
                .abs()
                < 1e-14
        );
        assert!(
            (metric
                .score_pair(&[1.0, 1.0], &[0.0, 0.0], &[0.0, 0.0])
                .unwrap()
                .logit
                + 3.0)
                .abs()
                < 1e-14
        );
    }
}
