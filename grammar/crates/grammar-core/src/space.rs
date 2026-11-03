//! Frozen named coordinates. Only reference documents fit the target and scales.
use crate::features::{Family, Features};
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const GEOMETRY: &str = "unslop-hellinger-euclidean-v2";
/// Rates are unrestricted occurrences per opportunity. This physical floor
/// prevents division by a nearly zero reference-group standard deviation.
pub const RATE_SCALE_FLOOR: f64 = 0.01;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reference {
    pub source_group: String,
    pub features: Features,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Overrides for family weights; zero explicitly disables a family.
    #[serde(default)]
    pub family_weights: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Distribution,
    Rate,
    Metric,
}

/// Preserve the feature contract separately from fitted statistical scales.
/// Distribution vocabularies are sparse; rate and metric catalogs are fixed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FamilySchema {
    pub kind: Kind,
    pub fixed_features: Option<BTreeSet<String>>,
    pub scale_floors: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Axis {
    pub family: String,
    pub feature: String,
    pub kind: Kind,
    pub center: f64,
    pub scale: f64,
    pub multiplier: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Binding {
    pub source_group: String,
    pub source_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Space {
    pub id: String,
    pub geometry: String,
    pub feature_schema: String,
    pub parser_identity: String,
    pub family_weights: BTreeMap<String, f64>,
    pub family_schemas: BTreeMap<String, FamilySchema>,
    pub references: Vec<Binding>,
    pub basis_sources: Vec<String>,
    pub axes: Vec<Axis>,
    pub target: Vec<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Point {
    pub space_id: String,
    pub source_sha256: String,
    pub coordinates: Vec<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Contribution {
    pub family: String,
    pub squared_distance: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Distance {
    pub euclidean: f64,
    pub families: Vec<Contribution>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Movement {
    pub family: String,
    pub feature: String,
    pub before: f64,
    pub after: f64,
    pub target: f64,
    /// Positive means this coordinate moved closer to the target.
    pub squared_distance_improvement: f64,
}

fn opportunities(family: &Family) -> u64 {
    match family {
        Family::Distribution { opportunities, .. }
        | Family::Rates { opportunities, .. }
        | Family::Metrics { opportunities, .. } => *opportunities,
    }
}

fn values(family: &Family) -> BTreeMap<String, f64> {
    match family {
        Family::Distribution {
            counts,
            opportunities,
        }
        | Family::Rates {
            counts,
            opportunities,
        } => counts
            .iter()
            .map(|(k, n)| (k.clone(), *n as f64 / *opportunities as f64))
            .collect(),
        Family::Metrics { values, .. } => values.clone(),
    }
}

fn kind(family: &Family) -> Kind {
    match family {
        Family::Distribution { .. } => Kind::Distribution,
        Family::Rates { .. } => Kind::Rate,
        Family::Metrics { .. } => Kind::Metric,
    }
}

fn family_schema(family: &Family) -> FamilySchema {
    match family {
        Family::Distribution { .. } => FamilySchema {
            kind: Kind::Distribution,
            fixed_features: None,
            scale_floors: BTreeMap::new(),
        },
        Family::Rates { counts, .. } => FamilySchema {
            kind: Kind::Rate,
            fixed_features: Some(counts.keys().cloned().collect()),
            scale_floors: counts
                .keys()
                .map(|key| (key.clone(), RATE_SCALE_FLOOR))
                .collect(),
        },
        Family::Metrics {
            values,
            scale_floors,
            ..
        } => FamilySchema {
            kind: Kind::Metric,
            fixed_features: Some(values.keys().cloned().collect()),
            scale_floors: scale_floors.clone(),
        },
    }
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch))
}

fn approximately_equal(left: f64, right: f64) -> bool {
    (left - right).abs() <= 1e-12 * left.abs().max(right.abs()).max(f64::MIN_POSITIVE)
}

fn validate_features(features: &Features) -> Result<()> {
    ensure!(
        !features.schema.trim().is_empty() && !features.parser_identity.trim().is_empty(),
        "missing feature identity"
    );
    ensure!(valid_hash(&features.source_sha256), "invalid source hash");
    for (name, family) in &features.families {
        ensure!(!name.is_empty(), "empty family name");
        match family {
            Family::Distribution {
                counts,
                opportunities,
            } => {
                ensure!(
                    counts.keys().all(|key| !key.is_empty()),
                    "{name}: empty distribution coordinate"
                );
                let total = counts.values().try_fold(0_u64, |a, b| a.checked_add(*b));
                ensure!(
                    total == Some(*opportunities),
                    "{name}: distribution counts must sum to opportunities"
                );
            }
            Family::Rates {
                counts,
                opportunities,
            } => {
                ensure!(
                    counts.keys().all(|key| !key.is_empty()),
                    "{name}: empty rate coordinate"
                );
                ensure!(
                    *opportunities > 0 || counts.values().all(|n| *n == 0),
                    "{name}: counts without opportunities"
                );
            }
            Family::Metrics {
                values,
                scale_floors,
                opportunities,
            } => {
                ensure!(
                    values.keys().all(|key| !key.is_empty()),
                    "{name}: empty metric coordinate"
                );
                ensure!(
                    values.keys().eq(scale_floors.keys()),
                    "{name}: metrics need explicit matching floors"
                );
                ensure!(
                    values.values().all(|n| n.is_finite()),
                    "{name}: nonfinite metric"
                );
                ensure!(
                    scale_floors.values().all(|n| n.is_finite() && *n > 0.0),
                    "{name}: invalid scale floor"
                );
                ensure!(
                    *opportunities > 0 || values.values().all(|n| *n == 0.0),
                    "{name}: metric values without opportunities"
                );
            }
        }
    }
    Ok(())
}

fn check_compatible(first: &Features, next: &Features) -> Result<()> {
    validate_features(next)?;
    ensure!(first.schema == next.schema, "feature schema mismatch");
    ensure!(
        first.parser_identity == next.parser_identity,
        "parser identity mismatch"
    );
    ensure!(
        first.families.keys().eq(next.families.keys()),
        "family catalog mismatch"
    );
    for (name, family) in &first.families {
        ensure!(
            kind(family) == kind(&next.families[name]),
            "{name}: family kind mismatch"
        );
        if let Family::Rates { counts, .. } = family {
            let Family::Rates { counts: other, .. } = &next.families[name] else {
                unreachable!()
            };
            ensure!(
                counts.keys().eq(other.keys()),
                "{name}: rate catalog mismatch; fixed rates must include zero coordinates"
            );
        }
        if let Family::Metrics { scale_floors, .. } = family {
            let Family::Metrics {
                scale_floors: other,
                ..
            } = &next.families[name]
            else {
                unreachable!()
            };
            ensure!(
                scale_floors == other,
                "{name}: metric catalog or physical scale mismatch"
            );
        }
    }
    Ok(())
}

fn defaults(features: &Features) -> BTreeMap<String, f64> {
    let lexical = features
        .families
        .keys()
        .filter(|k| matches!(k.as_str(), "word" | "word_bigram"))
        .count();
    let grammar = features.families.len() - lexical;
    features
        .families
        .keys()
        .map(|name| {
            let is_word = matches!(name.as_str(), "word" | "word_bigram");
            let n = if is_word { lexical } else { grammar };
            let mass = if lexical > 0 && grammar > 0 { 0.5 } else { 1.0 };
            (name.clone(), mass / n as f64)
        })
        .collect()
}

/// Fit a target from independent reference groups. Additional basis documents
/// expose possible coordinates; they never fit target probabilities or scales.
/// Changing the basis creates a new identity, even if the old axes are unchanged.
pub fn fit(references: &[Reference], basis: &[Features], config: &Config) -> Result<Space> {
    ensure!(!references.is_empty(), "no reference documents");
    let first = &references[0].features;
    validate_features(first)?;
    let mut seen = BTreeSet::new();
    let mut grouped: BTreeMap<&str, Vec<&Features>> = BTreeMap::new();
    for reference in references {
        ensure!(
            !reference.source_group.trim().is_empty(),
            "empty source group"
        );
        ensure!(
            seen.insert(&reference.features.source_sha256),
            "duplicate reference text"
        );
        check_compatible(first, &reference.features)?;
        grouped
            .entry(&reference.source_group)
            .or_default()
            .push(&reference.features);
    }
    ensure!(
        grouped.len() >= 2,
        "need at least two independent reference groups"
    );
    for docs in grouped.values_mut() {
        docs.sort_by_key(|doc| &doc.source_sha256);
    }
    for doc in basis {
        check_compatible(first, doc)?;
    }
    let all: Vec<&Features> = references
        .iter()
        .map(|r| &r.features)
        .chain(basis)
        .collect();
    let mut weights = defaults(first);
    for (name, weight) in &config.family_weights {
        ensure!(weights.contains_key(name), "unknown family weight: {name}");
        ensure!(
            weight.is_finite() && *weight >= 0.0,
            "invalid family weight: {name}"
        );
        weights.insert(name.clone(), *weight);
    }
    let weight_sum: f64 = weights.values().sum();
    ensure!(
        weight_sum.is_finite() && weight_sum > 0.0,
        "empty or invalid geometry weights"
    );
    for weight in weights.values_mut() {
        *weight /= weight_sum;
    }
    let mut axes = Vec::new();
    let mut target = Vec::new();
    for (name, weight) in &weights {
        if *weight == 0.0 {
            continue;
        }
        for doc in &all {
            ensure!(
                opportunities(&doc.families[name]) > 0,
                "{name}: unavailable on {}; explicitly disable this family to fit a reduced space",
                doc.source_sha256
            );
        }
        let family_kind = kind(&first.families[name]);
        let keys: BTreeSet<String> = all
            .iter()
            .flat_map(|doc| values(&doc.families[name]).into_keys())
            .collect();
        ensure!(!keys.is_empty(), "{name}: active family has no coordinates");
        // Each source group contributes equally, then each document within it.
        let group_values: Vec<BTreeMap<String, f64>> = grouped
            .values()
            .map(|docs| {
                let mut means = BTreeMap::new();
                for doc in docs {
                    for (key, value) in values(&doc.families[name]) {
                        *means.entry(key).or_insert(0.0) += value / docs.len() as f64;
                    }
                }
                means
            })
            .collect();
        for key in &keys {
            let samples: Vec<f64> = group_values
                .iter()
                .map(|v| v.get(key).copied().unwrap_or(0.0))
                .collect();
            let mean: f64 = samples.iter().sum::<f64>() / samples.len() as f64;
            let sd = (samples.iter().map(|n| (n - mean).powi(2)).sum::<f64>()
                / samples.len() as f64)
                .sqrt();
            let (center, scale, multiplier, target_value) = match family_kind {
                Kind::Distribution => {
                    let multiplier = (weight / 2.0).sqrt();
                    (0.0, 1.0, multiplier, mean.sqrt() * multiplier)
                }
                Kind::Rate => (
                    mean,
                    sd.max(RATE_SCALE_FLOOR),
                    (weight / keys.len() as f64).sqrt(),
                    0.0,
                ),
                Kind::Metric => {
                    let Family::Metrics { scale_floors, .. } = &first.families[name] else {
                        unreachable!()
                    };
                    (
                        mean,
                        sd.max(scale_floors[key]),
                        (weight / keys.len() as f64).sqrt(),
                        0.0,
                    )
                }
            };
            axes.push(Axis {
                family: name.clone(),
                feature: key.clone(),
                kind: family_kind.clone(),
                center,
                scale,
                multiplier,
            });
            target.push(target_value);
        }
    }
    let mut bindings: Vec<_> = references
        .iter()
        .map(|r| Binding {
            source_group: r.source_group.clone(),
            source_sha256: r.features.source_sha256.clone(),
        })
        .collect();
    bindings.sort_by(|a, b| {
        (&a.source_group, &a.source_sha256).cmp(&(&b.source_group, &b.source_sha256))
    });
    let mut space = Space {
        id: String::new(),
        geometry: GEOMETRY.into(),
        feature_schema: first.schema.clone(),
        parser_identity: first.parser_identity.clone(),
        family_weights: weights,
        family_schemas: first
            .families
            .iter()
            .map(|(name, family)| (name.clone(), family_schema(family)))
            .collect(),
        references: bindings,
        basis_sources: all
            .iter()
            .map(|f| f.source_sha256.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        axes,
        target,
    };
    space.id = space.computed_id()?;
    space.validate()?;
    Ok(space)
}

impl Space {
    fn computed_id(&self) -> Result<String> {
        let mut payload = self.clone();
        payload.id.clear();
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(&payload)?)))
    }

    fn check_features(&self, features: &Features) -> Result<()> {
        validate_features(features)?;
        ensure!(
            features.schema == self.feature_schema
                && features.parser_identity == self.parser_identity,
            "incompatible features or parser"
        );
        ensure!(
            features.families.keys().eq(self.family_weights.keys()),
            "family catalog mismatch"
        );
        for (name, weight) in &self.family_weights {
            let family = &features.families[name];
            ensure!(
                family_schema(family) == self.family_schemas[name],
                "{name}: fixed feature catalog, kind, or physical scale mismatch"
            );
            if *weight > 0.0 {
                ensure!(opportunities(family) > 0, "{name}: unavailable family");
            }
        }
        Ok(())
    }

    /// Extend the categorical vocabulary without refitting the geometry.
    /// Existing axis transforms, target values and reference bindings stay
    /// unchanged. New categorical coordinates have zero target mass. Basis
    /// sources and axes are canonicalized, and the returned identity binds the
    /// expanded basis; reproject old points before comparing them in this space.
    pub fn extend_basis(&self, basis: &[Features]) -> Result<Self> {
        self.validate()?;
        let mut axes: BTreeMap<_, _> = self
            .axes
            .iter()
            .zip(&self.target)
            .map(|(axis, target)| {
                (
                    (axis.family.clone(), axis.feature.clone()),
                    (axis.clone(), *target),
                )
            })
            .collect();
        let mut sources: BTreeSet<_> = self.basis_sources.iter().cloned().collect();
        for features in basis {
            self.check_features(features)?;
            sources.insert(features.source_sha256.clone());
            for (name, family) in &features.families {
                let weight = self.family_weights[name];
                if weight == 0.0 {
                    continue;
                }
                if let Family::Distribution { counts, .. } = family {
                    for feature in counts.keys() {
                        axes.entry((name.clone(), feature.clone()))
                            .or_insert_with(|| {
                                (
                                    Axis {
                                        family: name.clone(),
                                        feature: feature.clone(),
                                        kind: Kind::Distribution,
                                        center: 0.0,
                                        scale: 1.0,
                                        multiplier: (weight / 2.0).sqrt(),
                                    },
                                    0.0,
                                )
                            });
                    }
                }
            }
        }
        let mut expanded = self.clone();
        (expanded.axes, expanded.target) = axes.into_values().unzip();
        expanded.basis_sources = sources.into_iter().collect();
        expanded.id = expanded.computed_id()?;
        expanded.validate()?;
        Ok(expanded)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(self.geometry == GEOMETRY, "unsupported geometry version");
        ensure!(self.id == self.computed_id()?, "space identity mismatch");
        ensure!(
            !self.feature_schema.trim().is_empty() && !self.parser_identity.trim().is_empty(),
            "missing space feature identity"
        );
        ensure!(
            !self.axes.is_empty() && self.axes.len() == self.target.len(),
            "invalid basis dimensions"
        );
        ensure!(
            self.target.iter().all(|v| v.is_finite()),
            "nonfinite target"
        );
        ensure!(
            !self.family_weights.is_empty()
                && self.family_weights.keys().eq(self.family_schemas.keys()),
            "family schema/weight catalog mismatch"
        );
        ensure!(
            self.family_weights
                .iter()
                .all(|(name, weight)| !name.trim().is_empty()
                    && weight.is_finite()
                    && *weight >= 0.0),
            "invalid family weight"
        );
        let total_weight: f64 = self.family_weights.values().sum();
        ensure!(
            total_weight.is_finite() && approximately_equal(total_weight, 1.0),
            "family weights are not normalized"
        );
        for (name, schema) in &self.family_schemas {
            match schema.kind {
                Kind::Distribution => ensure!(
                    schema.fixed_features.is_none() && schema.scale_floors.is_empty(),
                    "{name}: invalid sparse distribution schema"
                ),
                Kind::Rate | Kind::Metric => {
                    let fixed = schema
                        .fixed_features
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("{name}: missing fixed feature catalog"))?;
                    ensure!(
                        fixed.iter().all(|key| !key.is_empty())
                            && fixed.iter().eq(schema.scale_floors.keys()),
                        "{name}: invalid fixed feature catalog or scale floors"
                    );
                    ensure!(
                        schema
                            .scale_floors
                            .values()
                            .all(|floor| floor.is_finite() && *floor > 0.0),
                        "{name}: invalid physical scale floor"
                    );
                    if schema.kind == Kind::Rate {
                        ensure!(
                            schema
                                .scale_floors
                                .values()
                                .all(|floor| *floor == RATE_SCALE_FLOOR),
                            "{name}: incompatible rate scale floor"
                        );
                    }
                }
            }
        }
        let mut known_axes = BTreeSet::new();
        let mut by_family: BTreeMap<&str, Vec<(usize, &Axis)>> = BTreeMap::new();
        for (index, axis) in self.axes.iter().enumerate() {
            let schema = self
                .family_schemas
                .get(&axis.family)
                .ok_or_else(|| anyhow::anyhow!("axis refers to unknown family {}", axis.family))?;
            ensure!(
                self.family_weights[&axis.family] > 0.0,
                "disabled family has active axes: {}",
                axis.family
            );
            ensure!(
                axis.kind == schema.kind,
                "axis kind differs from family schema: {}",
                axis.family
            );
            ensure!(
                !axis.feature.is_empty() && known_axes.insert((&axis.family, &axis.feature)),
                "empty or duplicate basis coordinate"
            );
            ensure!(
                axis.center.is_finite()
                    && axis.scale.is_finite()
                    && axis.scale > 0.0
                    && axis.multiplier.is_finite()
                    && axis.multiplier > 0.0,
                "invalid axis transform"
            );
            by_family
                .entry(&axis.family)
                .or_default()
                .push((index, axis));
        }
        for (name, weight) in &self.family_weights {
            if *weight == 0.0 {
                continue;
            }
            let axes = by_family
                .get(name.as_str())
                .ok_or_else(|| anyhow::anyhow!("active family has no axes: {name}"))?;
            let schema = &self.family_schemas[name];
            if let Some(fixed) = &schema.fixed_features {
                let actual: BTreeSet<&String> =
                    axes.iter().map(|(_, axis)| &axis.feature).collect();
                ensure!(
                    actual.iter().copied().eq(fixed.iter()),
                    "{name}: basis omits or adds fixed coordinates"
                );
            }
            match schema.kind {
                Kind::Distribution => {
                    let multiplier = (weight / 2.0).sqrt();
                    let mut target_mass = 0.0;
                    for (index, axis) in axes {
                        ensure!(
                            axis.center == 0.0
                                && axis.scale == 1.0
                                && approximately_equal(axis.multiplier, multiplier),
                            "{name}: invalid Hellinger axis transform"
                        );
                        ensure!(
                            self.target[*index] >= 0.0,
                            "{name}: negative distribution target"
                        );
                        target_mass += self.target[*index].powi(2);
                    }
                    ensure!(
                        target_mass.is_finite() && approximately_equal(target_mass, weight / 2.0),
                        "{name}: distribution target mass differs from family weight"
                    );
                }
                Kind::Rate | Kind::Metric => {
                    let multiplier = (weight / axes.len() as f64).sqrt();
                    for (index, axis) in axes {
                        ensure!(
                            approximately_equal(axis.multiplier, multiplier)
                                && self.target[*index] == 0.0,
                            "{name}: invalid standardized axis transform"
                        );
                        ensure!(
                            axis.scale >= schema.scale_floors[&axis.feature],
                            "{name}: axis scale is below its physical floor"
                        );
                        if schema.kind == Kind::Rate {
                            ensure!(
                                axis.center >= 0.0,
                                "{name}: negative occurrence-rate center"
                            );
                        }
                    }
                }
            }
        }
        let mut reference_hashes = BTreeSet::new();
        let mut groups = BTreeSet::new();
        for reference in &self.references {
            ensure!(
                !reference.source_group.trim().is_empty() && valid_hash(&reference.source_sha256),
                "invalid reference binding"
            );
            ensure!(
                reference_hashes.insert(&reference.source_sha256),
                "duplicate reference binding"
            );
            groups.insert(&reference.source_group);
        }
        ensure!(
            groups.len() >= 2,
            "need at least two independent reference groups"
        );
        ensure!(
            self.basis_sources.iter().all(|hash| valid_hash(hash))
                && self.basis_sources.windows(2).all(|pair| pair[0] < pair[1]),
            "invalid or noncanonical basis source hashes"
        );
        let basis: BTreeSet<&String> = self.basis_sources.iter().collect();
        ensure!(
            reference_hashes.is_subset(&basis),
            "basis provenance omits reference documents"
        );
        Ok(())
    }

    pub fn project(&self, features: &Features) -> Result<Point> {
        self.validate()?;
        self.check_features(features)?;
        let mut available: BTreeMap<&str, BTreeMap<String, f64>> = BTreeMap::new();
        for (name, weight) in &self.family_weights {
            let family = &features.families[name];
            if *weight == 0.0 {
                continue;
            }
            let observed = values(family);
            let known: BTreeSet<&str> = self
                .axes
                .iter()
                .filter(|a| a.family == *name)
                .map(|a| a.feature.as_str())
                .collect();
            if let Some(unknown) = observed.keys().find(|key| !known.contains(key.as_str())) {
                bail!(
                    "{name}/{unknown}: unseen coordinate; extend the basis and reproject all points"
                );
            }
            available.insert(name, observed);
        }
        let mut coordinates = Vec::with_capacity(self.axes.len());
        for axis in &self.axes {
            ensure!(
                kind(&features.families[&axis.family]) == axis.kind,
                "family kind mismatch"
            );
            let value = available[axis.family.as_str()]
                .get(&axis.feature)
                .copied()
                .unwrap_or(0.0);
            let coordinate = match axis.kind {
                Kind::Distribution => value.sqrt() * axis.multiplier,
                _ => (value - axis.center) / axis.scale * axis.multiplier,
            };
            ensure!(coordinate.is_finite(), "nonfinite projected coordinate");
            coordinates.push(coordinate);
        }
        Ok(Point {
            space_id: self.id.clone(),
            source_sha256: features.source_sha256.clone(),
            coordinates,
        })
    }

    fn check_point(&self, point: &Point) -> Result<()> {
        ensure!(
            point.space_id == self.id,
            "points from different spaces are incomparable"
        );
        ensure!(
            valid_hash(&point.source_sha256),
            "invalid point source hash"
        );
        ensure!(
            point.coordinates.len() == self.axes.len(),
            "coordinate length mismatch"
        );
        ensure!(
            point.coordinates.iter().all(|n| n.is_finite()),
            "nonfinite point"
        );
        Ok(())
    }

    pub fn distance(&self, point: &Point) -> Result<Distance> {
        self.validate()?;
        self.check_point(point)?;
        let mut families = BTreeMap::new();
        for ((axis, value), target) in self.axes.iter().zip(&point.coordinates).zip(&self.target) {
            *families.entry(axis.family.clone()).or_insert(0.0) += (value - target).powi(2);
        }
        let squared: f64 = families.values().sum();
        ensure!(squared.is_finite(), "distance overflow");
        Ok(Distance {
            euclidean: squared.sqrt(),
            families: families
                .into_iter()
                .map(|(family, squared_distance)| Contribution {
                    family,
                    squared_distance,
                })
                .collect(),
        })
    }

    /// All changed coordinates, sorted by absolute contribution to improvement.
    pub fn movements(&self, before: &Point, after: &Point) -> Result<Vec<Movement>> {
        self.validate()?;
        self.check_point(before)?;
        self.check_point(after)?;
        let mut movements = Vec::new();
        let mut before_squared = 0.0;
        let mut after_squared = 0.0;
        for (((axis, before), after), target) in self
            .axes
            .iter()
            .zip(&before.coordinates)
            .zip(&after.coordinates)
            .zip(&self.target)
        {
            let before_error = (before - target).powi(2);
            let after_error = (after - target).powi(2);
            before_squared += before_error;
            after_squared += after_error;
            if before != after {
                movements.push(Movement {
                    family: axis.family.clone(),
                    feature: axis.feature.clone(),
                    before: *before,
                    after: *after,
                    target: *target,
                    squared_distance_improvement: before_error - after_error,
                });
            }
        }
        ensure!(
            before_squared.is_finite() && after_squared.is_finite(),
            "movement distance overflow"
        );
        movements.sort_by(|a, b| {
            b.squared_distance_improvement
                .abs()
                .total_cmp(&a.squared_distance_improvement.abs())
                .then_with(|| (&a.family, &a.feature).cmp(&(&b.family, &b.feature)))
        });
        Ok(movements)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Features {
        let mut counts = BTreeMap::new();
        for word in text.split_whitespace() {
            *counts.entry(word.into()).or_insert(0) += 1;
        }
        Features {
            schema: "test-v1".into(),
            parser_identity: "fixture-v1".into(),
            source_sha256: hex::encode(Sha256::digest(text)),
            families: BTreeMap::from([(
                "word".into(),
                Family::Distribution {
                    counts,
                    opportunities: text.split_whitespace().count() as u64,
                },
            )]),
        }
    }
    fn reference(group: &str, text: &str) -> Reference {
        Reference {
            source_group: group.into(),
            features: words(text),
        }
    }

    #[test]
    fn group_balance_and_exact_hellinger_geometry() -> Result<()> {
        let refs = [
            reference("a", "red"),
            reference("a", "red red"),
            reference("b", "blue"),
        ];
        let space = fit(&refs, &[], &Config::default())?;
        let point = space.project(&words("red"))?;
        let expected = (1.0 - 0.5_f64.sqrt()).sqrt();
        assert!((space.distance(&point)?.euclidean - expected).abs() < 1e-12);
        Ok(())
    }

    #[test]
    fn expanded_basis_keeps_oov_mass_and_does_not_fit_target() -> Result<()> {
        let refs = [reference("a", "red"), reference("b", "red red")];
        let old = fit(&refs, &[], &Config::default())?;
        assert!(old.project(&words("green")).is_err());
        let expanded = fit(&refs, &[words("green")], &Config::default())?;
        let further = fit(&refs, &[words("green blue yellow")], &Config::default())?;
        assert_ne!(old.id, expanded.id);
        assert!(
            (expanded
                .distance(&expanded.project(&words("green"))?)?
                .euclidean
                - 1.0)
                .abs()
                < 1e-12
        );
        assert!(
            (further
                .distance(&further.project(&words("green"))?)?
                .euclidean
                - 1.0)
                .abs()
                < 1e-12
        );
        assert!(expanded.distance(&old.project(&words("red"))?).is_err());
        Ok(())
    }

    #[test]
    fn duplicates_incompatible_and_missing_opportunities_rejected() -> Result<()> {
        assert!(
            fit(
                &[reference("a", "red"), reference("b", "red")],
                &[],
                &Config::default()
            )
            .is_err()
        );
        let refs = [reference("a", "red"), reference("b", "blue")];
        let mut changed = words("green");
        changed.schema = "changed".into();
        assert!(fit(&refs, &[changed], &Config::default()).is_err());
        assert!(fit(&refs, &[words("")], &Config::default()).is_err());
        Ok(())
    }

    #[test]
    fn disk_roundtrip_identity_and_complete_movement_accounting() -> Result<()> {
        let refs = [
            reference("a", "red red blue"),
            reference("b", "red blue blue blue"),
        ];
        let space = fit(&refs, &[], &Config::default())?;
        let decoded: Space = serde_json::from_slice(&serde_json::to_vec(&space)?)?;
        decoded.validate()?;
        let before = decoded.project(&words("blue"))?;
        let after = decoded.project(&words("red blue"))?;
        let changes = decoded.movements(&before, &after)?;
        let total: f64 = changes.iter().map(|m| m.squared_distance_improvement).sum();
        let expected = decoded.distance(&before)?.euclidean.powi(2)
            - decoded.distance(&after)?.euclidean.powi(2);
        assert!((total - expected).abs() < 1e-12);
        let mut tampered = decoded;
        tampered.target[0] += 0.1;
        assert!(tampered.validate().is_err());
        Ok(())
    }

    fn measured(text: &str, load: f64, count: u64) -> Features {
        let mut features = words(text);
        features.schema = "measured-fixture-v1".into();
        features.families = BTreeMap::from([
            (
                "events".into(),
                Family::Rates {
                    counts: BTreeMap::from([("observed".into(), count), ("absent".into(), 0)]),
                    opportunities: 2,
                },
            ),
            (
                "load".into(),
                Family::Metrics {
                    values: BTreeMap::from([("mean".into(), load), ("spread".into(), 0.0)]),
                    scale_floors: BTreeMap::from([("mean".into(), 0.5), ("spread".into(), 0.25)]),
                    opportunities: 2,
                },
            ),
        ]);
        features
    }

    fn measured_references() -> [Reference; 2] {
        [
            Reference {
                source_group: "a".into(),
                features: measured("first", 2.0, 4),
            },
            Reference {
                source_group: "b".into(),
                features: measured("second", 4.0, 6),
            },
        ]
    }

    fn measured_words(text: &str, load: f64, count: u64) -> Features {
        let mut features = measured(text, load, count);
        features.families.extend(words(text).families);
        features
    }

    #[test]
    fn basis_extension_preserves_fitted_transforms_targets_and_old_distances() -> Result<()> {
        let references = [
            Reference {
                source_group: "a".into(),
                features: measured_words("red red blue", 2.0, 4),
            },
            Reference {
                source_group: "b".into(),
                features: measured_words("red blue blue", 6.0, 12),
            },
        ];
        let original = fit(
            &references,
            &[],
            &Config {
                family_weights: BTreeMap::from([
                    ("word".into(), 0.2),
                    ("load".into(), 0.7),
                    ("events".into(), 0.1),
                ]),
            },
        )?;
        let new = measured_words("green yellow", 1_000_000.0, 1_000_000);
        assert!(original.project(&new).is_err());
        let expanded = original.extend_basis(std::slice::from_ref(&new))?;
        expanded.validate()?;
        expanded.project(&new)?;
        assert_ne!(expanded.id, original.id);
        assert_eq!(expanded.family_weights, original.family_weights);
        assert_eq!(expanded.family_schemas, original.family_schemas);
        assert_eq!(
            serde_json::to_value(&expanded.references)?,
            serde_json::to_value(&original.references)?
        );
        assert!(expanded.axes.windows(2).all(|pair| {
            (&pair[0].family, &pair[0].feature) < (&pair[1].family, &pair[1].feature)
        }));
        for (axis, target) in original.axes.iter().zip(&original.target) {
            let index = expanded
                .axes
                .iter()
                .position(|candidate| {
                    candidate.family == axis.family && candidate.feature == axis.feature
                })
                .unwrap();
            assert_eq!(
                serde_json::to_value(axis)?,
                serde_json::to_value(&expanded.axes[index])?
            );
            assert_eq!(*target, expanded.target[index]);
        }
        for reference in &references {
            let before = original.project(&reference.features)?;
            let after = expanded.project(&reference.features)?;
            for (index, axis) in expanded.axes.iter().enumerate() {
                let old = original.axes.iter().position(|candidate| {
                    candidate.family == axis.family && candidate.feature == axis.feature
                });
                if let Some(old) = old {
                    assert_eq!(after.coordinates[index], before.coordinates[old]);
                } else {
                    assert_eq!(after.coordinates[index], 0.0);
                    assert_eq!(expanded.target[index], 0.0);
                }
            }
            assert_eq!(
                original.distance(&before)?.euclidean,
                expanded.distance(&after)?.euclidean
            );
            assert!(expanded.distance(&before).is_err());
        }
        let pair_distance = |space: &Space| -> Result<f64> {
            let a = space.project(&references[0].features)?;
            let b = space.project(&references[1].features)?;
            Ok(a.coordinates
                .iter()
                .zip(b.coordinates)
                .map(|(a, b)| (a - b).powi(2))
                .sum::<f64>())
        };
        assert_eq!(pair_distance(&original)?, pair_distance(&expanded)?);
        Ok(())
    }

    #[test]
    fn basis_extension_is_canonical_and_binds_even_existing_vocabulary() -> Result<()> {
        let references = [reference("a", "red"), reference("b", "blue")];
        let original = fit(&references, &[], &Config::default())?;
        assert_eq!(original.extend_basis(&[])?.id, original.id);
        let green = words("green");
        let yellow = words("yellow");
        let expanded = original.extend_basis(&[green.clone(), yellow.clone()])?;
        assert_eq!(
            expanded.id,
            original
                .extend_basis(&[yellow.clone(), green.clone(), yellow])?
                .id
        );
        assert_eq!(expanded.extend_basis(&[green])?.id, expanded.id);
        let known = words("red blue");
        let rebound = original.extend_basis(std::slice::from_ref(&known))?;
        assert_ne!(rebound.id, original.id);
        assert_eq!(
            serde_json::to_value(&rebound.axes)?,
            serde_json::to_value(&original.axes)?
        );
        assert_eq!(rebound.target, original.target);
        let expected: BTreeSet<_> = original
            .basis_sources
            .iter()
            .cloned()
            .chain([known.source_sha256])
            .collect();
        assert_eq!(
            rebound.basis_sources,
            expected.into_iter().collect::<Vec<_>>()
        );
        let decoded: Space = serde_json::from_slice(&serde_json::to_vec(&expanded)?)?;
        decoded.validate()?;
        decoded.project(&words("green yellow"))?;
        Ok(())
    }

    #[test]
    fn basis_extension_rejects_incompatible_or_invalid_features_and_spaces() -> Result<()> {
        let references = measured_references();
        let original = fit(&references, &[], &Config::default())?;
        let features = measured("candidate", 3.0, 5);
        let edits: [fn(&mut Features); 7] = [
            |features| features.schema = "incompatible".into(),
            |features| features.parser_identity = "incompatible".into(),
            |features| features.source_sha256 = "invalid".into(),
            |features| {
                features.families.remove("events");
            },
            |features| {
                features.families.insert(
                    "events".into(),
                    Family::Distribution {
                        counts: BTreeMap::from([("observed".into(), 2)]),
                        opportunities: 2,
                    },
                );
            },
            |features| {
                let Family::Rates { counts, .. } = features.families.get_mut("events").unwrap()
                else {
                    unreachable!()
                };
                counts.remove("absent");
            },
            |features| {
                let Family::Metrics { scale_floors, .. } =
                    features.families.get_mut("load").unwrap()
                else {
                    unreachable!()
                };
                scale_floors.insert("mean".into(), 5.0);
            },
        ];
        for edit in edits {
            let mut incompatible = features.clone();
            edit(&mut incompatible);
            assert!(original.extend_basis(&[incompatible]).is_err());
        }
        let mut invalid_space = original.clone();
        invalid_space.target.clear();
        assert!(invalid_space.extend_basis(&[]).is_err());
        let mut unavailable = measured("unavailable", 0.0, 4);
        let Family::Metrics { opportunities, .. } = unavailable.families.get_mut("load").unwrap()
        else {
            unreachable!()
        };
        *opportunities = 0;
        assert!(original.extend_basis(&[unavailable.clone()]).is_err());
        let reduced = fit(
            &references,
            &[],
            &Config {
                family_weights: BTreeMap::from([("load".into(), 0.0)]),
            },
        )?;
        let expanded = reduced.extend_basis(&[unavailable.clone()])?;
        expanded.project(&unavailable)?;
        assert!(expanded.axes.iter().all(|axis| axis.family != "load"));
        Ok(())
    }

    #[test]
    fn projection_requires_exact_fixed_catalogs_and_physical_floors() -> Result<()> {
        let references = measured_references();
        let space = fit(&references, &[], &Config::default())?;
        let original = measured("candidate", 3.0, 5);
        space.project(&original)?;
        let mut missing_metrics = original.clone();
        let Family::Metrics {
            values,
            scale_floors,
            ..
        } = missing_metrics.families.get_mut("load").unwrap()
        else {
            unreachable!()
        };
        values.clear();
        scale_floors.clear();
        assert!(
            space
                .project(&missing_metrics)
                .unwrap_err()
                .to_string()
                .contains("fixed feature catalog")
        );
        let mut missing_rate = original.clone();
        let Family::Rates { counts, .. } = missing_rate.families.get_mut("events").unwrap() else {
            unreachable!()
        };
        counts.remove("absent");
        assert!(space.project(&missing_rate).is_err());
        let mut changed_floor = original.clone();
        let Family::Metrics { scale_floors, .. } = changed_floor.families.get_mut("load").unwrap()
        else {
            unreachable!()
        };
        scale_floors.insert("mean".into(), 2.0);
        assert!(space.project(&changed_floor).is_err());
        let reduced = fit(
            &references,
            &[],
            &Config {
                family_weights: BTreeMap::from([("load".into(), 0.0)]),
            },
        )?;
        reduced.project(&original)?;
        // Disabling a family does not change the extractor's feature contract.
        assert!(reduced.project(&missing_metrics).is_err());
        assert!(reduced.project(&changed_floor).is_err());
        let decoded: Space = serde_json::from_slice(&serde_json::to_vec(&space)?)?;
        assert!(decoded.project(&changed_floor).is_err());
        Ok(())
    }

    #[test]
    fn unavailable_active_families_require_an_explicit_reduced_space() -> Result<()> {
        let references = measured_references();
        let mut candidate = measured("unavailable", 0.0, 4);
        let Family::Metrics { opportunities, .. } = candidate.families.get_mut("load").unwrap()
        else {
            unreachable!()
        };
        *opportunities = 0;
        let space = fit(&references, &[], &Config::default())?;
        assert!(
            space
                .project(&candidate)
                .unwrap_err()
                .to_string()
                .contains("unavailable family")
        );
        assert!(fit(&references, &[candidate.clone()], &Config::default()).is_err());
        let reduced = fit(
            &references,
            &[candidate.clone()],
            &Config {
                family_weights: BTreeMap::from([("load".into(), 0.0)]),
            },
        )?;
        reduced.project(&candidate)?;
        assert_ne!(space.id, reduced.id);
        Ok(())
    }

    #[test]
    fn public_measurements_validate_space_before_zipping_dimensions() -> Result<()> {
        let references = [reference("a", "red"), reference("b", "blue")];
        let space = fit(&references, &[], &Config::default())?;
        let point = space.project(&words("red"))?;
        let mut truncated = space.clone();
        truncated.target.clear();
        assert!(truncated.distance(&point).is_err());
        assert!(truncated.movements(&point, &point).is_err());
        // Even a matching recomputed content ID cannot legitimize bad dimensions.
        truncated.id = truncated.computed_id()?;
        let mut rebound = point.clone();
        rebound.space_id = truncated.id.clone();
        assert!(truncated.distance(&rebound).is_err());
        assert!(truncated.movements(&rebound, &rebound).is_err());
        Ok(())
    }

    #[test]
    fn structural_validation_rejects_resigned_bad_axes_weights_and_target_mass() -> Result<()> {
        let references = [reference("a", "red"), reference("b", "blue")];
        let space = fit(&references, &[], &Config::default())?;
        let edits: [fn(&mut Space); 8] = [
            |space| space.axes[0].family = "missing-family".into(),
            |space| space.axes[1].feature = space.axes[0].feature.clone(),
            |space| {
                space.family_weights.insert("word".into(), 2.0);
            },
            |space| {
                space.family_weights.insert("word".into(), f64::NAN);
            },
            |space| space.axes[0].kind = Kind::Metric,
            |space| space.axes[0].multiplier *= 2.0,
            |space| space.target[0] *= 0.5,
            |space| space.family_schemas.clear(),
        ];
        for edit in edits {
            let mut malformed = space.clone();
            edit(&mut malformed);
            malformed.id = malformed.computed_id()?;
            assert!(malformed.validate().is_err());
            // This used to index a missing family and could panic.
            assert!(malformed.project(&words("red")).is_err());
        }
        Ok(())
    }

    #[test]
    fn occurrence_rates_can_exceed_one_and_retain_their_physical_floor() -> Result<()> {
        let mut first = measured("first-rate", 2.0, 8);
        let mut second = measured("second-rate", 4.0, 8);
        first.families.remove("load");
        second.families.remove("load");
        let references = [
            Reference {
                source_group: "a".into(),
                features: first.clone(),
            },
            Reference {
                source_group: "b".into(),
                features: second,
            },
        ];
        let space = fit(&references, &[], &Config::default())?;
        let observed = space
            .axes
            .iter()
            .find(|axis| axis.feature == "observed")
            .unwrap();
        assert_eq!(observed.center, 4.0);
        assert_eq!(observed.scale, RATE_SCALE_FLOOR);
        assert_eq!(
            space.family_schemas["events"].scale_floors["observed"],
            RATE_SCALE_FLOOR
        );
        assert_eq!(space.distance(&space.project(&first)?)?.euclidean, 0.0);
        Ok(())
    }

    #[test]
    fn movement_overflow_fails_while_a_real_matching_distribution_has_zero_distance() -> Result<()>
    {
        let references = [reference("a", "red"), reference("b", "red red")];
        let space = fit(&references, &[], &Config::default())?;
        let point = space.project(&words("red red red"))?;
        assert_eq!(space.distance(&point)?.euclidean, 0.0);
        let mut huge = point.clone();
        huge.coordinates[0] = f64::MAX;
        assert!(space.distance(&huge).is_err());
        assert!(space.movements(&huge, &point).is_err());
        assert!(space.movements(&huge, &huge).is_err());
        Ok(())
    }
}
