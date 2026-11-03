//! Named coordinates in the original unweighted family geometry. Selection
//! never changes denominators or numerical scales. Unselected coordinates stay
//! represented by the residual of each complete family distance.
use anyhow::{Result, ensure};
use grammar_core::space::{Kind, Space};
use grammar_eval::{Profile, Values};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "slopninja-selected-coordinate-catalog-v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Coordinate {
    pub family_index: usize,
    pub family: String,
    pub feature: String,
    pub kind: Kind,
    pub scale: f64,
    /// Count of ALL numerical axes in the original family, not selected axes.
    pub original_family_axis_count: usize,
    pub train_author_df: usize,
    pub train_post_df: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SupportDecision {
    pub family: String,
    pub feature: String,
    pub train_author_df: usize,
    pub train_post_df: usize,
    pub training_minimum: f64,
    pub training_maximum: f64,
    pub decision: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub schema: String,
    pub source_space_id: String,
    pub feature_schema: String,
    pub parser_identity: String,
    pub families: Vec<String>,
    pub coordinates: Vec<Coordinate>,
    pub training_authors: Vec<String>,
    pub training_posts: usize,
    pub minimum_author_df: usize,
    pub minimum_post_df: usize,
    pub maximum_coordinates: usize,
    pub support_by_family: BTreeMap<String, usize>,
    pub categorical_family_caps: BTreeMap<String, usize>,
    pub support: Vec<SupportDecision>,
    pub selection: String,
}

pub fn select_catalog(space: &Space, training: &[(&str, &Profile)]) -> Result<Catalog> {
    let minimum_author_df = 10;
    let minimum_post_df = 30;
    space.validate()?;
    ensure!(!training.is_empty(), "training support required");
    let families: Vec<_> = space.family_schemas.keys().cloned().collect();
    let family_indices: BTreeMap<_, _> = families
        .iter()
        .enumerate()
        .map(|(i, f)| (f.clone(), i))
        .collect();
    let mut support = BTreeMap::<(String, String), (BTreeSet<String>, usize, f64, f64)>::new();
    let mut authors = BTreeSet::new();
    for (author, profile) in training {
        authors.insert((*author).to_owned());
        ensure!(
            profile.keys().eq(space.family_schemas.keys()),
            "training family catalog mismatch"
        );
        for (family, values) in *profile {
            for (feature, value) in values {
                ensure!(value.is_finite(), "nonfinite training coordinate");
                if *value != 0.0 {
                    let item = support
                        .entry((family.clone(), feature.clone()))
                        .or_insert_with(|| (BTreeSet::new(), 0, *value, *value));
                    item.0.insert((*author).to_owned());
                    item.1 += 1;
                    item.2 = item.2.min(*value);
                    item.3 = item.3.max(*value);
                }
            }
        }
    }
    let mut family_axes = BTreeMap::<String, usize>::new();
    for axis in &space.axes {
        *family_axes.entry(axis.family.clone()).or_default() += 1;
    }
    let mut required = Vec::new();
    let mut candidates = BTreeMap::<String, Vec<Coordinate>>::new();
    let mut support_by_family = BTreeMap::<String, usize>::new();
    let mut decisions = BTreeMap::new();
    let categorical_family_caps: BTreeMap<_, _> = space
        .family_schemas
        .iter()
        .filter(|(_, s)| s.kind == Kind::Distribution)
        .map(|(family, _)| {
            (
                family.clone(),
                if matches!(family.as_str(), "word" | "word_bigram") {
                    512
                } else {
                    96
                },
            )
        })
        .collect();
    for axis in &space.axes {
        let counts = support.get(&(axis.family.clone(), axis.feature.clone()));
        let coordinate = Coordinate {
            family_index: family_indices[&axis.family],
            family: axis.family.clone(),
            feature: axis.feature.clone(),
            kind: axis.kind.clone(),
            scale: axis.scale,
            original_family_axis_count: family_axes[&axis.family],
            train_author_df: counts.map_or(0, |row| row.0.len()),
            train_post_df: counts.map_or(0, |row| row.1),
        };
        let mut minimum = counts.map_or(0.0, |row| row.2);
        let mut maximum = counts.map_or(0.0, |row| row.3);
        if coordinate.train_post_df < training.len() {
            minimum = minimum.min(0.0);
            maximum = maximum.max(0.0);
        }
        let decision = if axis.kind != Kind::Distribution {
            if minimum != maximum {
                required.push(coordinate.clone());
                "selected"
            } else {
                "numerical_constant"
            }
        } else if coordinate.train_author_df < minimum_author_df {
            "below_author_support"
        } else if coordinate.train_post_df < minimum_post_df {
            "below_post_support"
        } else {
            *support_by_family.entry(axis.family.clone()).or_default() += 1;
            candidates
                .entry(axis.family.clone())
                .or_default()
                .push(coordinate.clone());
            "family_cap"
        };
        decisions.insert(
            (axis.family.clone(), axis.feature.clone()),
            SupportDecision {
                family: axis.family.clone(),
                feature: axis.feature.clone(),
                train_author_df: coordinate.train_author_df,
                train_post_df: coordinate.train_post_df,
                training_minimum: minimum,
                training_maximum: maximum,
                decision: decision.into(),
            },
        );
    }
    let maximum_coordinates = categorical_family_caps.values().sum::<usize>() + required.len();
    for (family, mut candidates) in candidates {
        candidates.sort_by(|a, b| {
            b.train_author_df
                .cmp(&a.train_author_df)
                .then_with(|| b.train_post_df.cmp(&a.train_post_df))
                .then_with(|| a.feature.cmp(&b.feature))
        });
        candidates.truncate(categorical_family_caps[&family]);
        for coordinate in &candidates {
            decisions
                .get_mut(&(family.clone(), coordinate.feature.clone()))
                .unwrap()
                .decision = "selected".into();
        }
        required.extend(candidates);
    }
    required.sort_by(|a, b| (&a.family, &a.feature).cmp(&(&b.family, &b.feature)));
    Ok(Catalog {schema:SCHEMA.into(),source_space_id:space.id.clone(),feature_schema:space.feature_schema.clone(),
        parser_identity:space.parser_identity.clone(),families,coordinates:required,training_authors:authors.into_iter().collect(),
        training_posts:training.len(),minimum_author_df,minimum_post_df,maximum_coordinates,support_by_family,
        categorical_family_caps,support:decisions.into_values().collect(),
        selection:"Training-only nonzero support >=10 authors and >=30 posts for categorical coordinates; rank separately within family by author DF descending, post DF descending, feature key ascending; cap word/word_bigram at512 each and other categorical families at96 each. Numerical coordinates require varying training values. Final coordinate order family/name ascending.".into()})
}

impl Catalog {
    pub fn validate(&self, space: &Space) -> Result<()> {
        ensure!(
            self.schema == SCHEMA
                && self.source_space_id == space.id
                && self.feature_schema == space.feature_schema
                && self.parser_identity == space.parser_identity,
            "coordinate catalog/source space identity mismatch"
        );
        ensure!(
            self.families == space.family_schemas.keys().cloned().collect::<Vec<_>>(),
            "family ordering mismatch"
        );
        ensure!(
            self.coordinates.len() <= self.maximum_coordinates,
            "coordinate budget exceeded"
        );
        let mut previous = None;
        let mut family_counts = BTreeMap::<String, usize>::new();
        let axes: BTreeMap<_, _> = space
            .axes
            .iter()
            .map(|axis| {
                *family_counts.entry(axis.family.clone()).or_default() += 1;
                ((axis.family.clone(), axis.feature.clone()), axis)
            })
            .collect();
        for coord in &self.coordinates {
            let key = (coord.family.clone(), coord.feature.clone());
            ensure!(
                previous.as_ref().is_none_or(|p| p < &key),
                "coordinates duplicated or unsorted"
            );
            let axis = axes
                .get(&key)
                .ok_or_else(|| anyhow::anyhow!("unknown catalog coordinate"))?;
            ensure!(
                coord.family_index < self.families.len()
                    && self.families[coord.family_index] == coord.family
                    && coord.kind == axis.kind
                    && coord.scale == axis.scale
                    && coord.original_family_axis_count == family_counts[&coord.family],
                "coordinate geometry mismatch"
            );
            previous = Some(key);
        }
        Ok(())
    }

    pub fn transform(&self, profile: &Profile) -> Result<Vec<f64>> {
        self.coordinates
            .iter()
            .map(|coordinate| {
                let family = profile
                    .get(&coordinate.family)
                    .ok_or_else(|| anyhow::anyhow!("missing coordinate family"))?;
                let value = family.get(&coordinate.feature).copied().unwrap_or(0.0);
                ensure!(value.is_finite(), "nonfinite profile coordinate");
                let transformed = if coordinate.kind == Kind::Distribution {
                    ensure!(value >= 0.0, "negative categorical probability");
                    (value / 2.0).sqrt()
                } else {
                    value
                        / (coordinate.scale * (coordinate.original_family_axis_count as f64).sqrt())
                };
                ensure!(transformed.is_finite(), "nonfinite transformed coordinate");
                Ok(transformed)
            })
            .collect()
    }

    pub fn residual(&self, query: &[f64], target: &[f64], full: &Values) -> Result<Vec<f64>> {
        ensure!(
            query.len() == self.coordinates.len() && target.len() == query.len(),
            "coordinate shape mismatch"
        );
        let mut selected = vec![0.0; self.families.len()];
        for ((left, right), coordinate) in query.iter().zip(target).zip(&self.coordinates) {
            selected[coordinate.family_index] += (left - right).powi(2);
        }
        self.families
            .iter()
            .zip(selected)
            .map(|(family, selected)| {
                let total = *full
                    .get(family)
                    .ok_or_else(|| anyhow::anyhow!("missing full family distance"))?;
                let residual = total - selected;
                ensure!(
                    residual >= -1e-9 * (1.0 + total.abs()),
                    "selected contribution exceeds complete family distance: {family}"
                );
                Ok(residual.max(0.0))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::{
        features::{Family, Features},
        space::{self, Config, Reference},
    };
    use grammar_eval::{DistanceGeometry, document_values, family_distances};
    use sha2::{Digest, Sha256};

    fn fixture() -> (Space, Vec<(String, Profile)>) {
        let mut references = Vec::new();
        let mut training = Vec::new();
        for author in 0..10 {
            for post in 0..3 {
                let mut words: BTreeMap<String, u64> =
                    (0..514).map(|i| (format!("word-{i:04}"), 1)).collect();
                if author < 9 {
                    words.insert("too-few-authors".into(), 1);
                }
                if post == 0 {
                    words.insert("too-few-posts".into(), 1);
                }
                let grammar: BTreeMap<String, u64> =
                    (0..98).map(|i| (format!("frame-{i:04}"), 1)).collect();
                let features = Features {
                    schema: "synthetic-coordinates-v1".into(),
                    parser_identity: "synthetic-parser-v1".into(),
                    source_sha256: hex::encode(Sha256::digest(format!("{author}-{post}"))),
                    families: BTreeMap::from([
                        (
                            "word".into(),
                            Family::Distribution {
                                opportunities: words.values().sum(),
                                counts: words,
                            },
                        ),
                        (
                            "dependency".into(),
                            Family::Distribution {
                                opportunities: grammar.values().sum(),
                                counts: grammar,
                            },
                        ),
                        (
                            "syntax".into(),
                            Family::Metrics {
                                values: BTreeMap::from([
                                    ("constant".into(), 2.0),
                                    ("varying".into(), post as f64),
                                ]),
                                scale_floors: BTreeMap::from([
                                    ("constant".into(), 1.0),
                                    ("varying".into(), 1.0),
                                ]),
                                opportunities: 1,
                            },
                        ),
                    ]),
                };
                training.push((author.to_string(), document_values(&features).unwrap()));
                references.push(Reference {
                    source_group: format!("{author}-{post}"),
                    features,
                });
            }
        }
        (
            space::fit(&references, &[], &Config::default()).unwrap(),
            training,
        )
    }

    #[test]
    fn catalog_uses_only_training_support_per_family_caps_and_numeric_variation() {
        let (space, training) = fixture();
        let rows: Vec<_> = training.iter().map(|(a, p)| (a.as_str(), p)).collect();
        let catalog = select_catalog(&space, &rows).unwrap();
        catalog.validate(&space).unwrap();
        assert_eq!(
            catalog
                .coordinates
                .iter()
                .filter(|c| c.family == "word")
                .count(),
            512
        );
        assert_eq!(
            catalog
                .coordinates
                .iter()
                .filter(|c| c.family == "dependency")
                .count(),
            96
        );
        assert_eq!(
            catalog
                .coordinates
                .iter()
                .filter(|c| c.family == "syntax")
                .count(),
            1
        );
        let reason = |name: &str| {
            catalog
                .support
                .iter()
                .find(|r| r.feature == name)
                .unwrap()
                .decision
                .as_str()
        };
        assert_eq!(reason("too-few-authors"), "below_author_support");
        assert_eq!(reason("too-few-posts"), "below_post_support");
        assert_eq!(reason("word-0512"), "family_cap");
        assert_eq!(reason("constant"), "numerical_constant");
        assert_eq!(
            catalog
                .coordinates
                .iter()
                .find(|c| c.family == "syntax")
                .unwrap()
                .original_family_axis_count,
            2
        );
        assert_eq!(
            serde_json::to_value(&catalog).unwrap(),
            serde_json::to_value(select_catalog(&space, &rows).unwrap()).unwrap()
        );
    }

    #[test]
    fn selected_differences_and_residual_preserve_unseen_and_constant_axis_mass() {
        let (space, training) = fixture();
        let rows: Vec<_> = training.iter().map(|(a, p)| (a.as_str(), p)).collect();
        let catalog = select_catalog(&space, &rows).unwrap();
        let target = training[0].1.clone();
        let mut query = training[1].1.clone();
        for value in query.get_mut("word").unwrap().values_mut() {
            *value *= 0.7;
        }
        query
            .get_mut("word")
            .unwrap()
            .insert("unseen-query-term".into(), 0.3);
        query
            .get_mut("syntax")
            .unwrap()
            .insert("constant".into(), 5.0);
        let q = catalog.transform(&query).unwrap();
        let p = catalog.transform(&target).unwrap();
        let full = family_distances(&DistanceGeometry::new(&space), &query, &target).unwrap();
        let residual = catalog.residual(&q, &p, &full).unwrap();
        let mut selected = vec![0.0; catalog.families.len()];
        for ((left, right), coordinate) in q.iter().zip(&p).zip(&catalog.coordinates) {
            selected[coordinate.family_index] += (left - right).powi(2);
            if coordinate.family == "syntax" {
                assert!(((left - right).powi(2) - 0.5).abs() < 1e-12);
            } else {
                let expected = (query[&coordinate.family]
                    .get(&coordinate.feature)
                    .copied()
                    .unwrap_or(0.0)
                    .sqrt()
                    - target[&coordinate.family]
                        .get(&coordinate.feature)
                        .copied()
                        .unwrap_or(0.0)
                        .sqrt())
                .powi(2)
                    / 2.0;
                assert!(((left - right).powi(2) - expected).abs() < 1e-12);
            }
        }
        for (i, family) in catalog.families.iter().enumerate() {
            assert!((full[family] - selected[i] - residual[i]).abs() < 1e-12);
        }
        assert!(residual[catalog.families.iter().position(|f| f == "word").unwrap()] > 0.1);
        assert!(
            (residual[catalog.families.iter().position(|f| f == "syntax").unwrap()] - 4.5).abs()
                < 1e-12
        );
    }

    #[test]
    fn changed_axis_scales_and_negative_residuals_fail() {
        let (space, training) = fixture();
        let rows: Vec<_> = training.iter().map(|(a, p)| (a.as_str(), p)).collect();
        let mut catalog = select_catalog(&space, &rows).unwrap();
        catalog.coordinates[0].scale *= 2.0;
        assert!(catalog.validate(&space).is_err());
        catalog.coordinates[0].scale /= 2.0;
        let q = vec![1.0; catalog.coordinates.len()];
        let p = vec![0.0; q.len()];
        let totals = catalog
            .families
            .iter()
            .map(|family| (family.clone(), 0.0))
            .collect();
        assert!(catalog.residual(&q, &p, &totals).is_err());
    }
}
