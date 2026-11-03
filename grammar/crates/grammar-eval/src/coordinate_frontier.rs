//! Named coordinates in the original unweighted family geometry. Selection
//! never changes denominators or numerical scales. Unselected coordinates stay
//! represented by the residual of each complete family distance.
use anyhow::{Result, ensure};
use grammar_core::space::{Kind, Space};
use grammar_eval::{Profile, Values};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[allow(dead_code)]
#[path = "coordinates.rs"]
mod frozen;

pub const SCHEMA: &str = "slopninja-coordinate-frontier-catalog-v1";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Coordinate {
    pub support_rank: Option<usize>,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SupportDecision {
    pub support_rank: Option<usize>,
    pub family: String,
    pub feature: String,
    pub train_author_df: usize,
    pub train_post_df: usize,
    pub training_minimum: f64,
    pub training_maximum: f64,
    pub decision: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub subsets: BTreeMap<String, Subset>,
    pub original_catalog_sha256: String,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Subset {
    pub block: String,
    pub cap_multiplier: Option<f64>,
    pub family_caps: BTreeMap<String, usize>,
    pub max_column_indices: Vec<usize>,
    pub coordinate_count: usize,
    pub canonical_name: String,
    /// SHA256 of the compact JSON integer array, without a newline.
    pub indices_sha256: String,
}

pub fn select_catalog(
    space: &Space,
    training: &[(&str, &Profile)],
    original_bytes: &[u8],
) -> Result<Catalog> {
    let original: frozen::Catalog = serde_json::from_slice(original_bytes)?;
    original.validate(space)?;
    let fitted = frozen::select_catalog(space, training)?;
    ensure!(
        serde_json::to_value(&fitted)? == serde_json::to_value(&original)?,
        "original training catalog support or geometry changed"
    );
    assemble(
        space,
        fitted.training_authors,
        fitted.training_posts,
        fitted.support,
        hex::encode(Sha256::digest(original_bytes)),
    )
}

fn assemble(
    space: &Space,
    training_authors: Vec<String>,
    training_posts: usize,
    source: Vec<frozen::SupportDecision>,
    original_catalog_sha256: String,
) -> Result<Catalog> {
    let families: Vec<_> = space.family_schemas.keys().cloned().collect();
    ensure!(
        !training_authors.is_empty() && training_posts >= training_authors.len(),
        "invalid training counts"
    );
    ensure!(
        training_authors.windows(2).all(|pair| pair[0] < pair[1]),
        "authors duplicated or unsorted"
    );
    let family_indices: BTreeMap<_, _> = families
        .iter()
        .enumerate()
        .map(|(i, f)| (f.clone(), i))
        .collect();
    let mut family_axes = BTreeMap::<String, usize>::new();
    for axis in &space.axes {
        *family_axes.entry(axis.family.clone()).or_default() += 1;
    }
    let source: BTreeMap<_, _> = source
        .into_iter()
        .map(|s| ((s.family.clone(), s.feature.clone()), s))
        .collect();
    ensure!(
        source.len() == space.axes.len(),
        "support must cover every original axis exactly once"
    );
    let mut candidates = BTreeMap::<String, Vec<Coordinate>>::new();
    let mut coordinates = Vec::new();
    let mut support = BTreeMap::new();
    for axis in &space.axes {
        let row = source
            .get(&(axis.family.clone(), axis.feature.clone()))
            .ok_or_else(|| anyhow::anyhow!("axis absent from support"))?;
        ensure!(
            row.train_author_df <= training_authors.len()
                && row.train_post_df <= training_posts
                && row.train_author_df <= row.train_post_df
                && row.training_minimum.is_finite()
                && row.training_maximum.is_finite()
                && row.training_minimum <= row.training_maximum,
            "invalid training support statistics"
        );
        let coord = Coordinate {
            family_index: family_indices[&axis.family],
            family: axis.family.clone(),
            feature: axis.feature.clone(),
            kind: axis.kind.clone(),
            scale: axis.scale,
            original_family_axis_count: family_axes[&axis.family],
            train_author_df: row.train_author_df,
            train_post_df: row.train_post_df,
            support_rank: None,
        };
        let decision = if axis.kind != Kind::Distribution {
            if row.training_minimum != row.training_maximum {
                coordinates.push(coord);
                "selected_numeric"
            } else {
                "numerical_constant"
            }
        } else if row.train_author_df < 10 {
            "below_author_support"
        } else if row.train_post_df < 30 {
            "below_post_support"
        } else {
            candidates
                .entry(axis.family.clone())
                .or_default()
                .push(coord);
            "selected"
        };
        support.insert(
            (axis.family.clone(), axis.feature.clone()),
            SupportDecision {
                family: axis.family.clone(),
                feature: axis.feature.clone(),
                train_author_df: row.train_author_df,
                train_post_df: row.train_post_df,
                training_minimum: row.training_minimum,
                training_maximum: row.training_maximum,
                support_rank: None,
                decision: decision.into(),
            },
        );
    }
    let mut support_by_family = BTreeMap::new();
    for (family, mut rows) in candidates {
        rows.sort_by(|a, b| {
            b.train_author_df
                .cmp(&a.train_author_df)
                .then_with(|| b.train_post_df.cmp(&a.train_post_df))
                .then_with(|| a.feature.cmp(&b.feature))
        });
        support_by_family.insert(family.clone(), rows.len());
        for (i, mut coord) in rows.into_iter().enumerate() {
            coord.support_rank = Some(i + 1);
            support
                .get_mut(&(family.clone(), coord.feature.clone()))
                .unwrap()
                .support_rank = Some(i + 1);
            coordinates.push(coord);
        }
    }
    coordinates.sort_by(|a, b| (&a.family, &a.feature).cmp(&(&b.family, &b.feature)));
    let subsets = subsets(&coordinates)?;
    Ok(Catalog { schema: SCHEMA.into(), source_space_id: space.id.clone(), feature_schema: space.feature_schema.clone(),
        parser_identity: space.parser_identity.clone(), families, maximum_coordinates: coordinates.len(), coordinates,
        training_authors, training_posts, minimum_author_df: 10, minimum_post_df: 30,
        categorical_family_caps: support_by_family.clone(), support_by_family, support: support.into_values().collect(), subsets,
        original_catalog_sha256,
        selection: "Fit support only from the exact original training posts. Select categorical axes with nonzero support in >=10 authors and >=30 posts; rank within family by author DF descending, post DF descending, feature ascending. Select original numerical axes only when training values vary. Export all supported coordinates once, ordered family/feature. Subsets use per-family rank prefixes without renormalization; word/word_bigram caps are 512 times multiplier, other categorical caps 96 times multiplier. Original numerical scales and full-family axis counts remain fixed; constant numerical axes remain in the complete-family residual.".into() })
}

fn subsets(coordinates: &[Coordinate]) -> Result<BTreeMap<String, Subset>> {
    let mut result = BTreeMap::new();
    for block in ["all", "word", "grammar"] {
        let mut canonical = BTreeMap::<Vec<usize>, String>::new();
        for (suffix, multiplier) in [
            ("m0p5", Some(0.5)),
            ("m1", Some(1.0)),
            ("m2", Some(2.0)),
            ("m4", Some(4.0)),
            ("m8", Some(8.0)),
            ("supported", None),
        ] {
            let name = format!("{block}_{suffix}");
            let mut indices = Vec::new();
            let mut family_caps = BTreeMap::<String, usize>::new();
            for (i, coord) in coordinates.iter().enumerate() {
                let word = matches!(coord.family.as_str(), "word" | "word_bigram");
                if (block == "word" && !word) || (block == "grammar" && word) {
                    continue;
                }
                let cap = multiplier.map(|m| ((if word { 512 } else { 96 }) as f64 * m) as usize);
                if coord.kind == Kind::Distribution {
                    if let Some(cap) = cap {
                        family_caps.insert(coord.family.clone(), cap);
                    } else {
                        *family_caps.entry(coord.family.clone()).or_default() += 1;
                    }
                    if cap.is_some_and(|cap| coord.support_rank.unwrap() > cap) {
                        continue;
                    }
                }
                indices.push(i);
            }
            let canonical_name = canonical
                .entry(indices.clone())
                .or_insert_with(|| name.clone())
                .clone();
            result.insert(
                name,
                Subset {
                    block: block.into(),
                    cap_multiplier: multiplier,
                    family_caps,
                    coordinate_count: indices.len(),
                    indices_sha256: hex::encode(Sha256::digest(serde_json::to_vec(&indices)?)),
                    max_column_indices: indices,
                    canonical_name,
                },
            );
        }
    }
    Ok(result)
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
        let reconstructed = assemble(
            space,
            self.training_authors.clone(),
            self.training_posts,
            self.support
                .iter()
                .map(|row| frozen::SupportDecision {
                    family: row.family.clone(),
                    feature: row.feature.clone(),
                    train_author_df: row.train_author_df,
                    train_post_df: row.train_post_df,
                    training_minimum: row.training_minimum,
                    training_maximum: row.training_maximum,
                    decision: String::new(),
                })
                .collect(),
            self.original_catalog_sha256.clone(),
        )?;
        ensure!(
            *self == reconstructed,
            "frontier support/rank/subset metadata mismatch"
        );
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
    fn fixture() -> (Space, Vec<(String, Profile)>) {
        let mut references = Vec::new();
        let mut training = Vec::new();
        for author in 0..11 {
            for post in 0..4 {
                let mut words: BTreeMap<String, u64> =
                    (0..514).map(|i| (format!("word-{i:04}"), 1)).collect();
                if author == 0 && post == 0 {
                    words.remove("word-0000");
                }
                if author == 0 {
                    words.remove("word-0002");
                }
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

    fn catalog(space: &Space, training: &[(String, Profile)]) -> Catalog {
        let rows: Vec<_> = training.iter().map(|(a, p)| (a.as_str(), p)).collect();
        let bytes = serde_json::to_vec(&frozen::select_catalog(space, &rows).unwrap()).unwrap();
        select_catalog(space, &rows, &bytes).unwrap()
    }
    #[test]
    fn support_rank_nested_prefixes_and_constants_preserve_geometry() {
        let (space, training) = fixture();
        let catalog = catalog(&space, &training);
        catalog.validate(&space).unwrap();
        assert_eq!(catalog.coordinates.len(), 514 + 98 + 1);
        assert_eq!(catalog.subsets["all_m0p5"].coordinate_count, 256 + 48 + 1);
        assert_eq!(catalog.subsets["all_m1"].coordinate_count, 512 + 96 + 1);
        assert_eq!(catalog.subsets["word_supported"].canonical_name, "word_m2");
        for names in [
            ["all_m0p5", "all_m1"],
            ["all_m1", "all_m2"],
            ["all_m2", "all_m4"],
            ["all_m4", "all_m8"],
            ["all_m8", "all_supported"],
        ] {
            assert!(
                catalog.subsets[names[0]]
                    .max_column_indices
                    .iter()
                    .all(|i| catalog.subsets[names[1]]
                        .max_column_indices
                        .binary_search(i)
                        .is_ok())
            );
        }
        assert!(
            catalog
                .coordinates
                .iter()
                .filter(|c| c.kind != Kind::Distribution)
                .all(|c| c.original_family_axis_count == 2 && c.support_rank.is_none())
        );
        let query = catalog.transform(&training[0].1).unwrap();
        let target = catalog.transform(&training[1].1).unwrap();
        let full = family_distances(
            &DistanceGeometry::new(&space),
            &training[0].1,
            &training[1].1,
        )
        .unwrap();
        catalog.residual(&query, &target, &full).unwrap();
        assert!(catalog.coordinates.iter().all(|c| c.feature != "constant"));
        let mut fresh = training[0].1.clone();
        fresh
            .get_mut("syntax")
            .unwrap()
            .insert("constant".into(), 3.0);
        let fresh_full =
            family_distances(&DistanceGeometry::new(&space), &fresh, &training[0].1).unwrap();
        let residual = catalog
            .residual(&catalog.transform(&fresh).unwrap(), &query, &fresh_full)
            .unwrap();
        assert!(residual[catalog.families.iter().position(|f| f == "syntax").unwrap()] > 0.0);
        let unsupported = catalog
            .support
            .iter()
            .find(|s| s.feature == "too-few-authors")
            .unwrap();
        assert_eq!(unsupported.decision, "below_author_support");
        assert_eq!(unsupported.support_rank, None);
    }
    #[test]
    fn rank_uses_support_before_lexical_order_and_validates_tampering() {
        let (space, training) = fixture();
        let mut catalog = catalog(&space, &training);
        let row = catalog
            .coordinates
            .iter()
            .find(|c| c.feature == "word-0000")
            .unwrap();
        assert_eq!(row.support_rank, Some(513));
        assert_eq!(
            catalog
                .coordinates
                .iter()
                .find(|c| c.feature == "word-0001")
                .unwrap()
                .support_rank,
            Some(1)
        );
        assert_eq!(
            catalog
                .coordinates
                .iter()
                .find(|c| c.feature == "word-0002")
                .unwrap()
                .support_rank,
            Some(514)
        );
        catalog
            .subsets
            .get_mut("all_m1")
            .unwrap()
            .max_column_indices
            .reverse();
        assert!(catalog.validate(&space).is_err());
        let mut catalog = super::tests::catalog(&space, &training);
        catalog.coordinates[0].support_rank = Some(987);
        assert!(catalog.validate(&space).is_err());
    }
    #[test]
    fn changed_original_support_is_rejected() {
        let (space, training) = fixture();
        let rows: Vec<_> = training.iter().map(|(a, p)| (a.as_str(), p)).collect();
        let mut old = frozen::select_catalog(&space, &rows).unwrap();
        old.support[0].train_post_df += 1;
        assert!(select_catalog(&space, &rows, &serde_json::to_vec(&old).unwrap()).is_err());
    }
}
