//! Fixed original-space distances for materialized synthetic alternatives.
//! These distances are diagnostics, not estimates of author preference or meaning.
use anyhow::{Context, Result, ensure};
use grammar_core::{
    features::{DefaultExtractor, FEATURE_VERSION, Family, FeatureExtractor, Features},
    space::{Kind, Space},
    syntax::Document,
};
use grammar_eval::{
    DistanceGeometry, Profile, Values, ablations, document_values, family_distances, rank,
    weighted_distance,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const SPACE_SHA256: &str = "cf2e0fd83fc6711b0cf39856e0d5ddf85324b6975781d8490bb6f47cbc9b20c6";
const TARGETS_SHA256: &str = "6abae438e69240e8ca29e96ac08cffcb562691ddcf74c0385b55d5023398242d";
const SPACE_ID: &str = "e8a2ebbb468c97d33c81133a9cc7d41fbb5d93bd6f09a9fbff8ba277e3703002";
const PARSER: &str =
    "grammar-spacy-annotation-v1;spacy=3.8.16;model=en_core_web_sm@3.8.0;raw-text-v1";
const REPRESENTATION: &str = "sparse raw per-date-group mean probabilities and metrics; distributions transform sqrt(p)*axis.multiplier and numerical means transform (mean-axis.center)/axis.scale*axis.multiplier";
const FAMILIES: [&str; 14] = [
    "clause_head_frame",
    "dependency",
    "dependency_path_2",
    "dependency_path_3",
    "morphology_bundle",
    "ordered_head_frame",
    "pos",
    "pos_bigram",
    "pos_tag",
    "pos_trigram",
    "syntax_load",
    "syntax_sentence_load",
    "word",
    "word_bigram",
];

pub struct Scoring {
    space: Space,
    geometry: DistanceGeometry,
    targets: BTreeMap<String, Profile>,
    target_support: BTreeMap<String, BTreeSet<String>>,
    weights: BTreeMap<String, Values>,
    audit: Value,
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn load_bound(path: &Path, expected: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("Read {}", path.display()))?;
    ensure!(
        hash(&bytes) == expected,
        "Frozen input hash differs: {}",
        path.display()
    );
    Ok(bytes)
}
fn opportunities(f: &Family) -> u64 {
    match f {
        Family::Distribution { opportunities, .. }
        | Family::Rates { opportunities, .. }
        | Family::Metrics { opportunities, .. } => *opportunities,
    }
}
fn kind(f: &Family) -> Kind {
    match f {
        Family::Distribution { .. } => Kind::Distribution,
        Family::Rates { .. } => Kind::Rate,
        Family::Metrics { .. } => Kind::Metric,
    }
}
fn validate_values(kind: &Kind, values: &Values, fixed: Option<&BTreeSet<String>>) -> Result<()> {
    ensure!(
        values
            .iter()
            .all(|(key, value)| !key.is_empty() && value.is_finite()),
        "Invalid feature value"
    );
    if *kind != Kind::Metric {
        ensure!(
            values.values().all(|v| *v >= 0.0),
            "Negative count/rate probability"
        );
    }
    if *kind == Kind::Distribution {
        ensure!(
            (values.values().sum::<f64>() - 1.0).abs() < 1e-9,
            "Distribution mass differs"
        );
    } else {
        ensure!(
            fixed.is_some_and(|keys| keys.iter().eq(values.keys())),
            "Numerical feature catalog differs; missing values cannot be zero-filled"
        );
    }
    Ok(())
}
fn validate_profile(space: &Space, profile: &Profile) -> Result<()> {
    ensure!(
        profile.keys().eq(space.family_schemas.keys()),
        "Target family catalog differs"
    );
    for (name, values) in profile {
        let schema = &space.family_schemas[name];
        validate_values(&schema.kind, values, schema.fixed_features.as_ref())?;
    }
    Ok(())
}
fn available_profile(space: &Space, features: &Features) -> Result<(Profile, Vec<String>, Value)> {
    ensure!(
        features.schema == space.feature_schema
            && features.parser_identity == space.parser_identity,
        "Feature identity differs"
    );
    ensure!(
        features.families.keys().eq(space.family_schemas.keys()),
        "Extracted family catalog differs"
    );
    let mut available = features.clone();
    let mut missing = Vec::new();
    let mut summaries = BTreeMap::new();
    for (name, family) in &features.families {
        let observed = opportunities(family) > 0;
        ensure!(
            kind(family) == space.family_schemas[name].kind,
            "Feature family kind differs"
        );
        let coordinates = match family {
            Family::Distribution { counts, .. } | Family::Rates { counts, .. } => counts.len(),
            Family::Metrics { values, .. } => values.len(),
        };
        summaries.insert(name,json!({"kind":kind(family),"opportunities":opportunities(family),"available":observed,"coordinate_count":coordinates,"unavailable_reason":if observed {None}else{Some("zero opportunities")}}));
        if !observed {
            missing.push(name.clone());
            available.families.remove(name);
        }
    }
    let profile = document_values(&available)?;
    for (name, values) in &profile {
        let schema = &space.family_schemas[name];
        validate_values(&schema.kind, values, schema.fixed_features.as_ref())?;
    }
    Ok((profile, missing, json!(summaries)))
}
fn partial_distances(
    geometry: &DistanceGeometry,
    profile: &Profile,
    target: &Profile,
) -> Result<Values> {
    let available = DistanceGeometry {
        families: geometry
            .families
            .iter()
            .filter(|(name, _)| profile.contains_key(*name))
            .map(|(n, k)| (n.clone(), k.clone()))
            .collect(),
        numerical_scales: geometry.numerical_scales.clone(),
    };
    family_distances(&available, profile, target)
}
fn coordinates(space: &Space, name: &str, values: &Values) -> Result<Values> {
    let schema = &space.family_schemas[name];
    if schema.kind == Kind::Distribution {
        Ok(values
            .iter()
            .map(|(key, p)| (key.clone(), (p / 2.0).sqrt()))
            .collect())
    } else {
        let axes = space
            .axes
            .iter()
            .filter(|a| a.family == name)
            .collect::<Vec<_>>();
        ensure!(!axes.is_empty(), "Missing frozen numerical axes");
        axes.iter()
            .map(|axis| {
                Ok((
                    axis.feature.clone(),
                    (values
                        .get(&axis.feature)
                        .context("Missing numerical value")?
                        - axis.center)
                        / axis.scale
                        / (axes.len() as f64).sqrt(),
                ))
            })
            .collect()
    }
}
fn counts(family: &Family) -> Option<&BTreeMap<String, u64>> {
    match family {
        Family::Distribution { counts, .. } | Family::Rates { counts, .. } => Some(counts),
        Family::Metrics { .. } => None,
    }
}
fn delta(before: Option<f64>, after: Option<f64>) -> Option<f64> {
    before.zip(after).map(|(a, b)| b - a)
}

fn mode_result(
    name: &str,
    weights: &Values,
    before: &BTreeMap<String, Values>,
    after: &BTreeMap<String, Values>,
    missing_before: &[String],
    missing_after: &[String],
) -> Result<Value> {
    let missing_before = missing_before
        .iter()
        .filter(|name| weights.contains_key(*name))
        .collect::<Vec<_>>();
    let missing_after = missing_after
        .iter()
        .filter(|name| weights.contains_key(*name))
        .collect::<Vec<_>>();
    if !missing_before.is_empty() || !missing_after.is_empty() {
        return Ok(
            json!({"mode":name,"status":"unavailable","source_missing_families":missing_before,"candidate_missing_families":missing_after,"reason":"Every weighted family must have observed opportunities on both sides; weights are not renormalized around missing families.","targets":null,"source_ranking":null,"candidate_ranking":null}),
        );
    }
    let source = before
        .iter()
        .map(|(id, d)| (id.clone(), weighted_distance(d, weights)))
        .collect::<Vec<_>>();
    let candidate = after
        .iter()
        .map(|(id, d)| (id.clone(), weighted_distance(d, weights)))
        .collect::<Vec<_>>();
    let source_ranking = rank(source)?;
    let candidate_ranking = rank(candidate)?;
    let targets=before.keys().map(|id| {
        let a=source_ranking.iter().find(|r|&r.author_id==id).unwrap();
        let b=candidate_ranking.iter().find(|r|&r.author_id==id).unwrap();
        json!({"author_id":id,"before":a.distance,"after":b.distance,"delta":b.distance-a.distance,"source_rank_min":a.rank_min,"source_rank_max":a.rank_max,"candidate_rank_min":b.rank_min,"candidate_rank_max":b.rank_max})
    }).collect::<Vec<_>>();
    Ok(
        json!({"mode":name,"status":"scored","weights":weights,"targets":targets,"source_ranking":source_ranking,"candidate_ranking":candidate_ranking,"rank_scope":"Only the ten preselected targets; exact ties have rank intervals and lexical ID display order."}),
    )
}

impl Scoring {
    /// Read the two exact frozen aggregate artifacts only. Never open paths in
    /// training bindings, corpus text, feature caches, or annotations.
    pub fn load(space_path: &Path, author_targets_path: &Path) -> Result<Self> {
        let space: Space = serde_json::from_slice(&load_bound(space_path, SPACE_SHA256)?)?;
        space.validate()?;
        ensure!(
            space.id == SPACE_ID
                && space.feature_schema == FEATURE_VERSION
                && space.parser_identity == PARSER,
            "Original space identity differs"
        );
        ensure!(
            space.family_schemas.keys().map(String::as_str).eq(FAMILIES),
            "Expected exact original fourteen families"
        );
        let targets: Value =
            serde_json::from_slice(&load_bound(author_targets_path, TARGETS_SHA256)?)?;
        ensure!(
            targets["space_id"] == space.id && targets["representation"] == REPRESENTATION,
            "Target space/representation differs"
        );
        let profiles: BTreeMap<String, Profile> =
            serde_json::from_value(targets["profiles"].clone())?;
        ensure!(
            profiles.len() == 100,
            "Original target author catalog differs"
        );
        for p in profiles.values() {
            validate_profile(&space, p)?;
        }
        let bindings = targets["training_bindings"]
            .as_array()
            .context("Missing training bindings")?;
        let mut ids = BTreeSet::new();
        let mut authors = BTreeSet::new();
        let mut references = Vec::new();
        let mut group_authors = BTreeMap::new();
        for binding in bindings {
            ensure!(binding["split"] == "train", "Non-TRAIN target input");
            let id = binding["id"].as_str().context("Missing post ID")?;
            let author = binding["author_id"].as_str().context("Missing author ID")?;
            let group = binding["source_group"]
                .as_str()
                .context("Missing date group")?;
            let sha = binding["text_sha256"]
                .as_str()
                .context("Missing source hash")?;
            ensure!(
                !id.is_empty() && !author.is_empty() && !group.is_empty() && ids.insert(id),
                "Invalid or duplicate training identity"
            );
            if let Some(previous) = group_authors.insert(group, author) {
                ensure!(previous == author, "Date group mixes authors");
            }
            authors.insert(author);
            references.push((group, sha));
        }
        ensure!(
            authors
                .iter()
                .copied()
                .eq(profiles.keys().map(String::as_str)),
            "Training/profile author catalog differs"
        );
        references.sort_unstable();
        let mut original = space
            .references
            .iter()
            .map(|r| (r.source_group.as_str(), r.source_sha256.as_str()))
            .collect::<Vec<_>>();
        original.sort_unstable();
        ensure!(
            references == original,
            "Target training bindings differ from frozen space references"
        );
        let selected = profiles.into_iter().take(10).collect::<BTreeMap<_, _>>();
        let selected_bindings = bindings
            .iter()
            .filter(|b| {
                b["author_id"]
                    .as_str()
                    .is_some_and(|id| selected.contains_key(id))
            })
            .collect::<Vec<_>>();
        let mut target_support = BTreeMap::<String, BTreeSet<String>>::new();
        for profile in selected.values() {
            for (family, values) in profile {
                target_support
                    .entry(family.clone())
                    .or_default()
                    .extend(values.keys().cloned());
            }
        }
        let weights = ablations(&space.family_weights)
            .into_iter()
            .map(|(name, w)| (if name == "word" { "words".into() } else { name }, w))
            .collect::<BTreeMap<_, _>>();
        let audit = json!({"schema":"slopninja-dative-vector-audit-v1","space":{"path":space_path,"sha256":SPACE_SHA256,"id":space.id},
            "author_targets":{"path":author_targets_path,"sha256":TARGETS_SHA256},"feature_schema":FEATURE_VERSION,"parser_identity":PARSER,
            "families":FAMILIES,"family_weights":weights,"target_author_count":10,"selected_author_ids":selected.keys().collect::<Vec<_>>(),
            "target_selection":"First ten lexicographically sorted original TRAIN author IDs, selected before any diagnostic scores.",
            "profile_rule":REPRESENTATION,"training_bindings_count":bindings.len(),"training_bindings_sha256":hash(&serde_json::to_vec(bindings)?),"selected_training_bindings":selected_bindings,
            "target_reference_multiset_matches_space":true,"corpus_text_reads":0,"feature_cache_reads":0,"fitting_calls":0,
            "coordinate_definition":"Categorical sqrt(p/2), including unseen names with zero target mass; numerical (value-frozen_center)/frozen_scale/sqrt(original_family_axis_count). Mode weights apply after per-family squared distance.",
            "numerical_axes":space.axes.iter().filter(|a|a.kind!=Kind::Distribution).collect::<Vec<_>>()});
        let geometry = DistanceGeometry::new(&space);
        Ok(Self {
            space,
            geometry,
            targets: selected,
            target_support,
            weights,
            audit,
        })
    }

    pub fn audit(&self) -> Value {
        self.audit.clone()
    }

    pub fn score_pair(&self, source: &Document, candidate: &Document) -> Result<Value> {
        ensure!(
            source.parser_identity == self.space.parser_identity
                && candidate.parser_identity == self.space.parser_identity,
            "Scoring parser differs from frozen space"
        );
        let before_features = DefaultExtractor.extract(source)?;
        let after_features = DefaultExtractor.extract(candidate)?;
        self.score_features(before_features, after_features)
    }

    fn score_features(&self, before_features: Features, after_features: Features) -> Result<Value> {
        let (before, missing_before, before_summary) =
            available_profile(&self.space, &before_features)?;
        let (after, missing_after, after_summary) =
            available_profile(&self.space, &after_features)?;
        let mut vectors = BTreeMap::new();
        for name in self.space.family_schemas.keys() {
            let left = before.get(name);
            let right = after.get(name);
            let a = left
                .map(|v| coordinates(&self.space, name, v))
                .transpose()?;
            let b = right
                .map(|v| coordinates(&self.space, name, v))
                .transpose()?;
            let mut support = self.target_support[name].clone();
            let observed = left
                .into_iter()
                .chain(right)
                .flat_map(|v| v.keys().cloned())
                .collect::<BTreeSet<_>>();
            support.extend(observed.iter().cloned());
            let count_a = counts(&before_features.families[name]);
            let count_b = counts(&after_features.families[name]);
            let changes=observed.iter().map(|key| {
                let pa=left.map(|v|v.get(key).copied().unwrap_or(0.0));
                let pb=right.map(|v|v.get(key).copied().unwrap_or(0.0));
                let xa=a.as_ref().map(|v|v.get(key).copied().unwrap_or(0.0));
                let xb=b.as_ref().map(|v|v.get(key).copied().unwrap_or(0.0));
                let ca=count_a.map(|v|v.get(key).copied().unwrap_or(0));
                let cb=count_b.map(|v|v.get(key).copied().unwrap_or(0));
                (key.clone(),json!({"count_before":ca,"count_after":cb,"count_delta":ca.zip(cb).map(|(a,b)|i128::from(b)-i128::from(a)),"value_before":pa,"value_after":pb,"value_delta":delta(pa,pb),"coordinate_before":xa,"coordinate_after":xb,"coordinate_delta":delta(xa,xb)}))
            }).collect::<BTreeMap<_,_>>();
            vectors.insert(name,json!({"kind":self.geometry.families[name],"source_values":left,"candidate_values":right,"source_coordinates":a,"candidate_coordinates":b,
                "coordinate_deltas_on_source_or_candidate_support":changes,"support":{"definition":"Sorted union of source, candidate, and all ten selected target feature names; both points use the same union.","count":support.len(),"sha256":hash(&serde_json::to_vec(&support)?),"target_only_coordinates_delta_if_both_available":if left.is_some()&&right.is_some(){Some(0.0)}else{None}},
                "sparse_categorical_omission":"Absent categorical coordinates have zero mass when the family is available. An unavailable family is null, never a zero vector."}));
        }
        let before_distances = self
            .targets
            .iter()
            .map(|(id, p)| Ok((id.clone(), partial_distances(&self.geometry, &before, p)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let after_distances = self
            .targets
            .iter()
            .map(|(id, p)| Ok((id.clone(), partial_distances(&self.geometry, &after, p)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let per_family = self
            .targets
            .keys()
            .map(|id| {
                let values = self
                    .geometry
                    .families
                    .keys()
                    .map(|name| {
                        let a = before_distances[id].get(name).copied();
                        let b = after_distances[id].get(name).copied();
                        (name, json!({"before":a,"after":b,"delta":delta(a,b)}))
                    })
                    .collect::<BTreeMap<_, _>>();
                json!({"author_id":id,"squared_family_distances":values})
            })
            .collect::<Vec<_>>();
        let mut modes = BTreeMap::new();
        for (name, weights) in &self.weights {
            modes.insert(
                name,
                mode_result(
                    name,
                    weights,
                    &before_distances,
                    &after_distances,
                    &missing_before,
                    &missing_after,
                )?,
            );
        }
        let scored = modes.values().filter(|v| v["status"] == "scored").count();
        let words_a =
            counts(&before_features.families["word"]).context("Word distribution missing")?;
        let words_b =
            counts(&after_features.families["word"]).context("Word distribution missing")?;
        let word_keys = words_a
            .keys()
            .chain(words_b.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let word_changes=word_keys.iter().filter_map(|word| {
            let a=words_a.get(word).copied().unwrap_or(0);let b=words_b.get(word).copied().unwrap_or(0);
            (a!=b).then(||json!({"word":word,"before":a,"after":b,"delta":i128::from(b)-i128::from(a)}))
        }).collect::<Vec<_>>();
        let to_a = words_a.get("to").copied().unwrap_or(0);
        let to_b = words_b.get("to").copied().unwrap_or(0);
        Ok(
            json!({"schema":"slopninja-dative-vector-pair-v1","status":if scored==3{"scored"}else if scored==0{"unscorable"}else{"partially_scorable"},"space_id":self.space.id,"space_sha256":SPACE_SHA256,"author_targets_sha256":TARGETS_SHA256,"feature_schema":self.space.feature_schema,"parser_identity":self.space.parser_identity,
            "source_features":before_features,"candidate_features":after_features,"source_feature_summary":before_summary,"candidate_feature_summary":after_summary,"source_unavailable_families":missing_before,"candidate_unavailable_families":missing_after,
            "sparse_vectors":vectors,"per_target_family_distances":per_family,"modes":modes,"word_bag_delta":{"changes":word_changes,"to":{"before":to_a,"after":to_b,"delta":i128::from(to_b)-i128::from(to_a)}},
            "delta_orientation":"Candidate minus source; negative distance delta means closer to that fixed target only.","target_count":self.targets.len(),"author_preference_estimated":false,"semantic_equivalence_certified":false,"detector_success_assessed":false,"automatic_edit_license":false}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::space::{Axis, FamilySchema};

    fn fixture() -> (Scoring, Features, Features) {
        let mut families = BTreeMap::new();
        let mut schemas = BTreeMap::new();
        let mut axes = Vec::new();
        for name in FAMILIES {
            let metric = matches!(name, "syntax_load" | "syntax_sentence_load");
            if metric {
                families.insert(
                    name.into(),
                    Family::Metrics {
                        values: BTreeMap::from([("m".into(), 2.0)]),
                        scale_floors: BTreeMap::from([("m".into(), 1.0)]),
                        opportunities: 1,
                    },
                );
                schemas.insert(
                    name.into(),
                    FamilySchema {
                        kind: Kind::Metric,
                        fixed_features: Some(BTreeSet::from(["m".into()])),
                        scale_floors: BTreeMap::from([("m".into(), 1.0)]),
                    },
                );
                axes.push(Axis {
                    family: name.into(),
                    feature: "m".into(),
                    kind: Kind::Metric,
                    center: 1.0,
                    scale: 2.0,
                    multiplier: 1.0,
                });
            } else {
                families.insert(
                    name.into(),
                    Family::Distribution {
                        counts: BTreeMap::from([("common".into(), 1)]),
                        opportunities: 1,
                    },
                );
                schemas.insert(
                    name.into(),
                    FamilySchema {
                        kind: Kind::Distribution,
                        fixed_features: None,
                        scale_floors: BTreeMap::new(),
                    },
                );
            }
        }
        let space = Space {
            id: SPACE_ID.into(),
            geometry: grammar_core::space::GEOMETRY.into(),
            feature_schema: FEATURE_VERSION.into(),
            parser_identity: PARSER.into(),
            family_weights: FAMILIES
                .into_iter()
                .map(|name| {
                    (
                        name.into(),
                        if name == "word" || name == "word_bigram" {
                            0.25
                        } else {
                            0.5 / 12.0
                        },
                    )
                })
                .collect(),
            family_schemas: schemas,
            references: Vec::new(),
            basis_sources: Vec::new(),
            axes,
            target: Vec::new(),
        };
        let mut before = Features {
            schema: FEATURE_VERSION.into(),
            parser_identity: PARSER.into(),
            source_sha256: hash(b"synthetic source"),
            families,
        };
        before.families.insert(
            "word".into(),
            Family::Distribution {
                counts: BTreeMap::from([("seen".into(), 3), ("to".into(), 1)]),
                opportunities: 4,
            },
        );
        let mut after = before.clone();
        after.source_sha256 = hash(b"synthetic candidate");
        after.families.insert(
            "word".into(),
            Family::Distribution {
                counts: BTreeMap::from([("seen".into(), 3)]),
                opportunities: 3,
            },
        );
        let mut target = document_values(&before).unwrap();
        target.insert(
            "word".into(),
            BTreeMap::from([("seen".into(), 0.5), ("target-only".into(), 0.5)]),
        );
        let targets = (0..10)
            .map(|i| (format!("synthetic-{i:02}"), target.clone()))
            .collect::<BTreeMap<_, _>>();
        let target_support = target
            .iter()
            .map(|(name, v)| (name.clone(), v.keys().cloned().collect()))
            .collect();
        let geometry = DistanceGeometry::new(&space);
        let weights = ablations(&space.family_weights)
            .into_iter()
            .map(|(name, w)| (if name == "word" { "words".into() } else { name }, w))
            .collect();
        (
            Scoring {
                space,
                geometry,
                targets,
                target_support,
                weights,
                audit: Value::Null,
            },
            before,
            after,
        )
    }

    #[test]
    fn sparse_coordinates_retain_disjoint_mass_and_frozen_numerical_geometry() {
        let (scoring, _, _) = fixture();
        let geometry = DistanceGeometry {
            families: BTreeMap::from([
                ("word".into(), Kind::Distribution),
                ("syntax_load".into(), Kind::Metric),
            ]),
            numerical_scales: BTreeMap::from([("syntax_load".into(), vec![("m".into(), 2.0)])]),
        };
        let left = BTreeMap::from([
            ("word".into(), BTreeMap::from([("unseen".into(), 1.0)])),
            ("syntax_load".into(), BTreeMap::from([("m".into(), 2.0)])),
        ]);
        let right = BTreeMap::from([
            ("word".into(), BTreeMap::from([("old".into(), 1.0)])),
            ("syntax_load".into(), BTreeMap::from([("m".into(), 4.0)])),
        ]);
        let distances = family_distances(&geometry, &left, &right).unwrap();
        assert_eq!(distances["word"], 1.0);
        assert_eq!(distances["syntax_load"], 1.0);
        for name in ["word", "syntax_load"] {
            let a = coordinates(&scoring.space, name, &left[name]).unwrap();
            let b = coordinates(&scoring.space, name, &right[name]).unwrap();
            let union = a.keys().chain(b.keys()).collect::<BTreeSet<_>>();
            let squared = union
                .iter()
                .map(|key| {
                    (a.get(*key).copied().unwrap_or(0.0) - b.get(*key).copied().unwrap_or(0.0))
                        .powi(2)
                })
                .sum::<f64>();
            assert!((squared - distances[name]).abs() < 1e-12);
        }
    }

    #[test]
    fn materialized_vectors_counts_deltas_and_tie_ranks_are_reconstructible() {
        let (scoring, before, after) = fixture();
        let result = scoring.score_features(before, after).unwrap();
        assert_eq!(result["status"], "scored");
        let word = &result["sparse_vectors"]["word"];
        assert_eq!(word["support"]["count"], 3);
        assert_eq!(
            word["support"]["sha256"],
            hash(&serde_json::to_vec(&BTreeSet::from(["seen", "target-only", "to"])).unwrap())
        );
        assert_eq!(word["source_values"]["seen"], 0.75);
        assert_eq!(word["candidate_values"]["seen"], 1.0);
        assert_eq!(
            word["coordinate_deltas_on_source_or_candidate_support"]["seen"]["value_delta"],
            0.25
        );
        assert_eq!(
            word["coordinate_deltas_on_source_or_candidate_support"]["to"]["count_delta"],
            -1
        );
        assert_eq!(result["word_bag_delta"]["to"]["delta"], -1);
        let mode = &result["modes"]["words"];
        assert_eq!(mode["source_ranking"].as_array().unwrap().len(), 10);
        for target in mode["targets"].as_array().unwrap() {
            assert_eq!(target["source_rank_min"], 1);
            assert_eq!(target["source_rank_max"], 10);
            assert_eq!(target["candidate_rank_min"], 1);
            assert_eq!(target["candidate_rank_max"], 10);
            let a = target["before"].as_f64().unwrap();
            let b = target["after"].as_f64().unwrap();
            assert!(b < a);
            assert_eq!(target["delta"].as_f64().unwrap(), b - a);
        }
        assert_eq!(result["author_preference_estimated"], false);
    }

    #[test]
    fn unavailable_grammar_never_becomes_a_zero_vector_or_renormalized_score() {
        let (scoring, mut before, after) = fixture();
        before.families.insert(
            "dependency_path_3".into(),
            Family::Distribution {
                counts: BTreeMap::new(),
                opportunities: 0,
            },
        );
        let result = scoring.score_features(before, after).unwrap();
        assert_eq!(result["status"], "partially_scorable");
        assert_eq!(result["modes"]["words"]["status"], "scored");
        for name in ["grammar", "combined"] {
            assert_eq!(result["modes"][name]["status"], "unavailable");
            assert!(result["modes"][name]["targets"].is_null());
        }
        assert!(result["sparse_vectors"]["dependency_path_3"]["source_coordinates"].is_null());
        let distance = &result["per_target_family_distances"][0]["squared_family_distances"]["dependency_path_3"];
        assert!(distance["before"].is_null());
        assert!(distance["delta"].is_null());
        assert_eq!(distance["after"], 0.0);
    }

    #[test]
    fn invalid_profiles_and_missing_numerical_values_fail_instead_of_zero_filling() {
        let fixed = BTreeSet::from(["m".into()]);
        assert!(validate_values(&Kind::Metric, &Values::new(), Some(&fixed)).is_err());
        for values in [
            BTreeMap::from([("x".into(), -1.0), ("y".into(), 2.0)]),
            BTreeMap::from([("x".into(), 0.5)]),
            BTreeMap::from([("x".into(), f64::NAN)]),
        ] {
            assert!(validate_values(&Kind::Distribution, &values, None).is_err());
        }
        let (scoring, mut before, after) = fixture();
        before.parser_identity = "different parser".into();
        assert!(scoring.score_features(before, after).is_err());
    }

    #[test]
    fn empty_document_is_unscorable_and_parser_mismatch_is_an_error() {
        let (scoring, _, _) = fixture();
        let empty = Document {
            text: String::new(),
            parser_identity: PARSER.into(),
            tokens: Vec::new(),
            sentences: Vec::new(),
        };
        let result = scoring.score_pair(&empty, &empty).unwrap();
        assert_eq!(result["status"], "unscorable");
        assert_eq!(
            result["source_unavailable_families"]
                .as_array()
                .unwrap()
                .len(),
            14
        );
        assert_eq!(result["word_bag_delta"]["to"]["delta"], 0);
        let mut wrong = empty.clone();
        wrong.parser_identity = "different parser".into();
        assert!(scoring.score_pair(&empty, &wrong).is_err());
    }
}
