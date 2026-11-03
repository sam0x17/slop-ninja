//! Training-only sparse coordinates over the versioned grammar-core features.

use anyhow::{Result, bail, ensure};
use grammar_core::features::{Family, Features};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeatureRow {
    pub id: String,
    pub group: String,
    pub label: usize,
    pub features: Features,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeatureMode {
    Word,
    Grammar,
    #[default]
    Combined,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeatureConfig {
    pub mode: FeatureMode,
    pub max_coordinates: usize,
    /// Independent word/bigram budget in combined mode. The remaining total
    /// budget belongs to grammar; unused capacity is not transferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_coordinate_cap: Option<usize>,
    pub min_document_frequency: usize,
    pub scale_floor: f64,
}

impl Default for FeatureConfig {
    fn default() -> Self {
        Self {
            mode: FeatureMode::Combined,
            max_coordinates: 8192,
            word_coordinate_cap: None,
            min_document_frequency: 2,
            scale_floor: 0.01,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoordinateKey {
    Available { family: String },
    Value { family: String, name: String },
}

impl CoordinateKey {
    pub fn is_word(&self) -> bool {
        let (Self::Available { family } | Self::Value { family, .. }) = self;
        matches!(family.as_str(), "word" | "word_bigram")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Coordinate {
    pub key: CoordinateKey,
    pub training_document_frequency: usize,
    /// Root mean square on training rows, with the declared lower bound.
    /// There is no centering: unobserved coordinates remain sparse zeros.
    pub scale: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeatureSpace {
    pub schema: String,
    pub feature_schema: String,
    pub parser_identity: String,
    pub config: FeatureConfig,
    pub training_rows: usize,
    pub coordinates: Vec<Coordinate>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SparseFeatures {
    pub values: Vec<(usize, f64)>,
    /// Nonzero coordinates absent from the training vocabulary, including
    /// coordinates pruned by frequency or the size cap. They are ignored.
    pub unknown_coordinates: usize,
    pub unavailable_families: Vec<String>,
}

impl FeatureSpace {
    pub fn fit(rows: &[FeatureRow], config: &FeatureConfig) -> Result<Self> {
        ensure!(!rows.is_empty(), "cannot fit an empty training set");
        ensure!(
            config.max_coordinates > 0,
            "coordinate cap must be positive"
        );
        ensure!(
            config.min_document_frequency > 0,
            "document frequency must be positive"
        );
        ensure!(
            config.scale_floor.is_finite() && config.scale_floor > 0.0,
            "invalid scale floor"
        );
        if let Some(cap) = config.word_coordinate_cap {
            ensure!(
                config.mode == FeatureMode::Combined && cap > 0 && cap < config.max_coordinates,
                "independent word budget requires combined mode and positive word/grammar capacity"
            );
        }
        let first = &rows[0].features;
        let mut statistics = BTreeMap::<CoordinateKey, (usize, f64)>::new();
        for row in rows {
            ensure!(
                row.features.schema == first.schema,
                "feature schema mismatch for {}",
                row.id
            );
            ensure!(
                row.features.parser_identity == first.parser_identity,
                "parser mismatch for {}",
                row.id
            );
            for (key, value) in raw_coordinates(&row.features, config.mode)?.0 {
                if value != 0.0 {
                    let entry = statistics.entry(key).or_default();
                    entry.0 += 1;
                    entry.1 += value * value;
                }
            }
        }
        let mut entries: Vec<_> = statistics
            .into_iter()
            .filter(|(key, (count, _))| {
                matches!(key, CoordinateKey::Available { .. })
                    || *count >= config.min_document_frequency
            })
            .collect();
        entries.sort_by(|(ka, (na, _)), (kb, (nb, _))| {
            let availability = |key: &CoordinateKey| matches!(key, CoordinateKey::Available { .. });
            availability(kb)
                .cmp(&availability(ka))
                .then(nb.cmp(na))
                .then(ka.cmp(kb))
        });
        let availability_count = entries
            .iter()
            .filter(|(key, _)| matches!(key, CoordinateKey::Available { .. }))
            .count();
        ensure!(
            config.max_coordinates >= availability_count,
            "coordinate cap excludes family availability indicators"
        );
        if let Some(cap) = config.word_coordinate_cap {
            let (mut word, mut grammar): (Vec<_>, Vec<_>) =
                entries.into_iter().partition(|(key, _)| key.is_word());
            for (budget, values) in [(cap, &word), (config.max_coordinates - cap, &grammar)] {
                ensure!(
                    budget
                        >= values
                            .iter()
                            .filter(|(key, _)| matches!(key, CoordinateKey::Available { .. }))
                            .count(),
                    "independent coordinate budget excludes availability indicators"
                );
            }
            word.truncate(cap);
            grammar.truncate(config.max_coordinates - cap);
            word.extend(grammar);
            entries = word;
        } else {
            entries.truncate(config.max_coordinates);
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        ensure!(
            !entries.is_empty(),
            "no observed features after vocabulary filtering"
        );
        let coordinates = entries
            .into_iter()
            .map(|(key, (count, squares))| Coordinate {
                key,
                training_document_frequency: count,
                scale: (squares / rows.len() as f64).sqrt().max(config.scale_floor),
            })
            .collect();
        Ok(Self {
            schema: "slop_ninja_detector_feature_space_v1".into(),
            feature_schema: first.schema.clone(),
            parser_identity: first.parser_identity.clone(),
            config: config.clone(),
            training_rows: rows.len(),
            coordinates,
        })
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == "slop_ninja_detector_feature_space_v1",
            "unsupported feature-space schema"
        );
        ensure!(
            self.training_rows > 0 && !self.coordinates.is_empty(),
            "empty feature space"
        );
        ensure!(
            self.coordinates.len() <= self.config.max_coordinates,
            "coordinate count exceeds budget"
        );
        if let Some(cap) = self.config.word_coordinate_cap {
            ensure!(
                self.config.mode == FeatureMode::Combined
                    && cap > 0
                    && cap < self.config.max_coordinates,
                "invalid independent word budget"
            );
            let words = self.coordinates.iter().filter(|c| c.key.is_word()).count();
            ensure!(
                words <= cap && self.coordinates.len() - words <= self.config.max_coordinates - cap,
                "coordinate count exceeds an independent budget"
            );
        }
        let mut seen = BTreeSet::new();
        for coordinate in &self.coordinates {
            ensure!(
                coordinate.scale.is_finite() && coordinate.scale > 0.0,
                "invalid coordinate scale"
            );
            ensure!(
                coordinate.training_document_frequency <= self.training_rows,
                "invalid document frequency"
            );
            ensure!(seen.insert(&coordinate.key), "duplicate feature coordinate");
        }
        ensure!(
            self.coordinates
                .windows(2)
                .all(|pair| pair[0].key < pair[1].key),
            "feature coordinates are not in canonical order"
        );
        Ok(())
    }

    pub fn transform(&self, features: &Features) -> Result<SparseFeatures> {
        ensure!(
            features.schema == self.feature_schema,
            "inference feature schema differs from training"
        );
        ensure!(
            features.parser_identity == self.parser_identity,
            "inference parser differs from training"
        );
        let (raw, unavailable_families) = raw_coordinates(features, self.config.mode)?;
        let mut values = Vec::new();
        let mut unknown_coordinates = 0;
        for (key, value) in raw {
            if value == 0.0 {
                continue;
            }
            if let Ok(i) = self
                .coordinates
                .binary_search_by(|coordinate| coordinate.key.cmp(&key))
            {
                let scaled = value / self.coordinates[i].scale;
                ensure!(scaled.is_finite(), "non-finite transformed coordinate");
                values.push((i, scaled));
            } else {
                unknown_coordinates += 1;
            }
        }
        values.sort_by_key(|entry| entry.0);
        Ok(SparseFeatures {
            values,
            unknown_coordinates,
            unavailable_families,
        })
    }
}

type RawCoordinates = (BTreeMap<CoordinateKey, f64>, Vec<String>);

fn raw_coordinates(features: &Features, mode: FeatureMode) -> Result<RawCoordinates> {
    let mut values = BTreeMap::new();
    let mut unavailable = Vec::new();
    for (family_name, family) in &features.families {
        let lexical = matches!(family_name.as_str(), "word" | "word_bigram");
        if (mode == FeatureMode::Word && !lexical) || (mode == FeatureMode::Grammar && lexical) {
            continue;
        }
        let opportunities = match family {
            Family::Distribution { opportunities, .. }
            | Family::Rates { opportunities, .. }
            | Family::Metrics { opportunities, .. } => *opportunities,
        };
        if opportunities == 0 {
            match family {
                Family::Distribution { counts, .. } | Family::Rates { counts, .. } => {
                    ensure!(
                        counts.values().all(|count| *count == 0),
                        "nonzero counts without opportunities in {family_name}"
                    );
                }
                Family::Metrics { values, .. } => {
                    ensure!(
                        values.values().all(|value| value.is_finite()),
                        "non-finite unavailable metrics in {family_name}"
                    );
                }
            }
            unavailable.push(family_name.clone());
            continue;
        }
        values.insert(
            CoordinateKey::Available {
                family: family_name.clone(),
            },
            1.0,
        );
        match family {
            Family::Distribution { counts, .. } | Family::Rates { counts, .. } => {
                if matches!(family, Family::Distribution { .. }) {
                    let sum = counts
                        .values()
                        .try_fold(0_u64, |sum, count| sum.checked_add(*count));
                    ensure!(
                        sum == Some(opportunities),
                        "distribution counts do not sum to opportunities in {family_name}"
                    );
                }
                for (name, count) in counts {
                    let rate = *count as f64 / opportunities as f64;
                    let value = if matches!(family, Family::Distribution { .. }) {
                        rate.sqrt()
                    } else {
                        rate.ln_1p()
                    };
                    values.insert(
                        CoordinateKey::Value {
                            family: family_name.clone(),
                            name: name.clone(),
                        },
                        value,
                    );
                }
            }
            Family::Metrics {
                values: metrics, ..
            } => {
                for (name, &value) in metrics {
                    if !value.is_finite() {
                        bail!("non-finite feature {family_name}/{name}");
                    }
                    values.insert(
                        CoordinateKey::Value {
                            family: family_name.clone(),
                            name: name.clone(),
                        },
                        value,
                    );
                }
            }
        }
    }
    Ok((values, unavailable))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn row(id: &str, label: usize, word: &str) -> FeatureRow {
        FeatureRow {
            id: id.into(),
            group: id.into(),
            label,
            features: Features {
                schema: "synthetic-feature-v1".into(),
                parser_identity: "fixture-v1".into(),
                source_sha256: id.into(),
                families: BTreeMap::from([(
                    "word".into(),
                    Family::Distribution {
                        counts: BTreeMap::from([(word.into(), 4)]),
                        opportunities: 4,
                    },
                )]),
            },
        }
    }

    #[test]
    fn held_out_words_cannot_expand_or_rescale_training_vocabulary() {
        let train = vec![row("train", 0, "known")];
        let space = FeatureSpace::fit(
            &train,
            &FeatureConfig {
                min_document_frequency: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let before = serde_json::to_string(&space).unwrap();
        let result = space
            .transform(&row("held", 1, "unknown").features)
            .unwrap();
        assert_eq!(result.unknown_coordinates, 1);
        assert_eq!(result.values.len(), 1); // Availability is still observed.
        assert_eq!(serde_json::to_string(&space).unwrap(), before);
        let mut unavailable = train[0].features.clone();
        unavailable.families.insert(
            "word".into(),
            Family::Distribution {
                counts: BTreeMap::new(),
                opportunities: 0,
            },
        );
        let result = space.transform(&unavailable).unwrap();
        assert!(result.values.is_empty());
        assert_eq!(result.unavailable_families, ["word"]);
    }

    #[test]
    fn independent_budgets_preserve_the_entire_word_control_space() {
        let mut rows: Vec<_> = (0..6)
            .map(|i| row(&format!("r{i}"), i % 3, ["alpha", "beta", "gamma"][i % 3]))
            .collect();
        for row in &mut rows {
            row.features.families.insert(
                "syntax_pos".into(),
                Family::Distribution {
                    counts: (0..9).map(|i| (format!("pos{i}"), 1)).collect(),
                    opportunities: 9,
                },
            );
        }
        let word = FeatureSpace::fit(
            &rows,
            &FeatureConfig {
                mode: FeatureMode::Word,
                max_coordinates: 3,
                ..Default::default()
            },
        )
        .unwrap();
        let grammar = FeatureSpace::fit(
            &rows,
            &FeatureConfig {
                mode: FeatureMode::Grammar,
                max_coordinates: 5,
                ..Default::default()
            },
        )
        .unwrap();
        let config = FeatureConfig {
            max_coordinates: 8,
            word_coordinate_cap: Some(3),
            ..Default::default()
        };
        let combined = FeatureSpace::fit(&rows, &config).unwrap();
        combined.validate().unwrap();
        for (is_word, reference) in [(true, &word), (false, &grammar)] {
            let observed: Vec<_> = combined
                .coordinates
                .iter()
                .filter(|c| c.key.is_word() == is_word)
                .collect();
            assert_eq!(
                serde_json::to_value(observed).unwrap(),
                serde_json::to_value(&reference.coordinates).unwrap()
            );
        }
        let legacy = FeatureSpace::fit(
            &rows,
            &FeatureConfig {
                max_coordinates: 8,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            legacy
                .coordinates
                .iter()
                .filter(|c| c.key.is_word())
                .count(),
            1
        );
        for cap in [0, 8, 9] {
            assert!(
                FeatureSpace::fit(
                    &rows,
                    &FeatureConfig {
                        word_coordinate_cap: Some(cap),
                        ..config.clone()
                    }
                )
                .is_err()
            );
        }
        assert!(
            FeatureSpace::fit(
                &rows,
                &FeatureConfig {
                    mode: FeatureMode::Word,
                    ..config
                }
            )
            .is_err()
        );
        assert_eq!(
            serde_json::to_string(&FeatureConfig::default()).unwrap(),
            r#"{"mode":"combined","max_coordinates":8192,"min_document_frequency":2,"scale_floor":0.01}"#
        );
    }
}
