//! Bound, read-only original14 scoring for synthetic edit diagnostics.
//! New diagnostic families never enter a frozen model's input profile.

use anyhow::{Context, Result, ensure};
use grammar_core::{
    features::{DefaultExtractor, FEATURE_VERSION, Family, FeatureExtractor, Features},
    space::{Kind, Space},
    syntax::Document,
};
use grammar_eval::{Profile, document_values, grouped_profile, validate_date};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[path = "coordinate_inference.rs"]
mod inference;
use inference::{Artifact, Metric};

pub struct Scoring {
    parser_identity: String,
    models: Vec<(u64, Metric)>,
    targets: Vec<(String, Profile)>,
    audit: Value,
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("Missing string {key}"))
}

fn read_bound(path: &Path, expected: &str) -> Result<Vec<u8>> {
    ensure!(
        expected.len() == 64
            && expected
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "Malformed expected SHA256 for {}",
        path.display()
    );
    let bytes = fs::read(path).with_context(|| format!("Read {}", path.display()))?;
    ensure!(
        hash(&bytes) == expected,
        "Source SHA256 differs: {}",
        path.display()
    );
    Ok(bytes)
}

fn bound_json(path: &Path, expected: &str) -> Result<Value> {
    Ok(serde_json::from_slice(&read_bound(path, expected)?)?)
}

fn unavailable(features: &Features) -> Vec<String> {
    features
        .families
        .iter()
        .filter_map(|(name, family)| {
            let opportunities = match family {
                Family::Distribution { opportunities, .. }
                | Family::Rates { opportunities, .. }
                | Family::Metrics { opportunities, .. } => *opportunities,
            };
            (opportunities == 0).then(|| name.clone())
        })
        .collect()
}

fn validate_model(
    artifact: &Artifact,
    space: &Space,
    settings: &Value,
    parser: &str,
) -> Result<()> {
    ensure!(
        artifact.source_space_id == space.id
            && artifact.source_space_id == settings["source_space_id"]
            && artifact.feature_schema == FEATURE_VERSION
            && artifact.feature_schema == settings["base_feature_schema"]
            && artifact.parser_identity == parser
            && artifact.parser_identity == space.parser_identity,
        "Model source space, feature schema, or parser identity differs"
    );
    ensure!(
        artifact.provenance_sha256.get("space").map(String::as_str)
            == settings["reference_space"]["sha256"].as_str(),
        "Model original geometry binding differs"
    );
    ensure!(
        artifact
            .families
            .iter()
            .map(|family| &family.name)
            .eq(space.family_schemas.keys())
            && artifact.families.len() == 14,
        "Model does not contain exactly the original fourteen families"
    );
    for family in &artifact.families {
        let axes = space
            .axes
            .iter()
            .filter(|axis| axis.family == family.name)
            .collect::<Vec<_>>();
        ensure!(
            family.kind == space.family_schemas[&family.name].kind
                && family.original_axis_count == axes.len(),
            "Model family geometry differs: {}",
            family.name
        );
        if family.kind != Kind::Distribution {
            ensure!(
                family.numerical_axes.len() == axes.len()
                    && family
                        .numerical_axes
                        .iter()
                        .zip(&axes)
                        .all(|(a, b)| a.feature == b.feature && a.scale == b.scale),
                "Model full numeric axes/scales differ: {}",
                family.name
            );
        }
    }
    let axes = space
        .axes
        .iter()
        .map(|axis| ((axis.family.as_str(), axis.feature.as_str()), axis))
        .collect::<BTreeMap<_, _>>();
    for coordinate in &artifact.coordinates {
        let axis = axes
            .get(&(coordinate.family.as_str(), coordinate.feature.as_str()))
            .context("Learned coordinate absent from original geometry")?;
        ensure!(
            coordinate.kind == axis.kind && coordinate.scale == axis.scale,
            "Learned coordinate kind/scale differs from original geometry"
        );
    }
    Ok(())
}

impl Scoring {
    /// Protocol paths and their hashes must be frozen by the caller before this
    /// function runs. Only selected original TRAIN author feature caches are read.
    pub fn load(protocol: &Value) -> Result<Self> {
        ensure!(
            protocol["schema"] == "slopninja-edit-utility-protocol-v1",
            "Unsupported edit protocol"
        );
        let settings = &protocol["scoring"];
        let parser_identity = text(protocol, "parser_identity")?.to_owned();
        ensure!(
            settings["base_feature_schema"] == FEATURE_VERSION
                && settings["target_author_count"] == 10
                && settings["target_author_policy"]
                    == "first lexicographically sorted original TRAIN author IDs"
                && settings["target_profile_policy"]
                    == "raw original14 post probabilities/measurements averaged equally within dates then equally across dates; no fitting or coordinate selection",
            "Frozen scoring profile policy differs"
        );
        let space_binding = &settings["reference_space"];
        let space_bytes = read_bound(
            Path::new(text(space_binding, "path")?),
            text(space_binding, "sha256")?,
        )?;
        let space: Space = serde_json::from_slice(&space_bytes)?;
        space.validate()?;
        ensure!(
            space.id == settings["source_space_id"]
                && space.feature_schema == FEATURE_VERSION
                && space.parser_identity == parser_identity,
            "Original space identity differs"
        );
        let definitions = settings["models"]
            .as_array()
            .context("Missing frozen model definitions")?;
        ensure!(
            definitions.len() == 3
                && definitions.iter().map(|row| row["seed"].as_u64()).eq([
                    Some(0),
                    Some(17),
                    Some(29)
                ]),
            "Expected exactly the three inherited seeds in fixed order"
        );
        let mut models = Vec::new();
        let mut model_bindings = Vec::new();
        for definition in definitions {
            let seed = definition["seed"]
                .as_u64()
                .context("Missing inherited seed")?;
            let digest = text(definition, "sha256")?;
            let artifact: Artifact =
                serde_json::from_slice(&read_bound(Path::new(text(definition, "path")?), digest)?)?;
            validate_model(&artifact, &space, settings, &parser_identity)?;
            model_bindings.push(
                json!({"seed":seed,"sha256":digest,"provenance_sha256":artifact.provenance_sha256}),
            );
            models.push((seed, Metric::new(artifact)?));
        }
        let target_binding = &settings["target_export"];
        let directory = PathBuf::from(text(target_binding, "path")?);
        let manifest_hash = text(target_binding, "manifest_sha256")?;
        let manifest = bound_json(&directory.join("manifest.json"), manifest_hash)?;
        ensure!(
            manifest["schema"] == "slopninja-named-family-tensor-v1"
                && manifest["split"] == "train"
                && manifest["source_space_id"] == space.id
                && manifest["reference_space_sha256"] == space_binding["sha256"]
                && manifest["legacy_feature_schema"] == FEATURE_VERSION
                && manifest["parser_identity"] == parser_identity,
            "Target export is not the bound original TRAIN geometry"
        );
        let family_names = space.family_schemas.keys().cloned().collect::<Vec<_>>();
        ensure!(
            manifest["baseline_families"] == json!(family_names),
            "Target baseline family catalog differs"
        );
        for (_, model) in &models {
            ensure!(
                model
                    .artifact()
                    .provenance_sha256
                    .get("catalog")
                    .map(String::as_str)
                    == manifest["source_catalog_sha256"].as_str(),
                "Target/model coordinate catalog binding differs"
            );
        }
        let authors = manifest["authors"]
            .as_array()
            .context("Missing original author catalog")?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .context("Invalid author ID")
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            authors.len() == 100 && authors.windows(2).all(|pair| pair[0] < pair[1]),
            "Original TRAIN author catalog differs"
        );
        let selected = authors.iter().take(10).cloned().collect::<BTreeSet<_>>();
        let queries = bound_json(
            &directory.join("queries.json"),
            text(&manifest, "queries_sha256")?,
        )?;
        let rows = queries.as_array().context("Missing TRAIN query array")?;
        ensure!(
            rows.len() == 1224 && manifest["query_count"] == rows.len(),
            "Original TRAIN query count differs"
        );
        let audit = bound_json(
            &directory.join("audit.json"),
            text(&manifest, "audit_sha256")?,
        )?;
        ensure!(
            audit["schema"] == "slopninja-named-family-tensor-audit-v1"
                && audit["loaded_splits"] == json!(["train"]),
            "Target audit scope differs"
        );
        let mut bindings = BTreeMap::new();
        for row in audit["posts"]
            .as_array()
            .context("Missing audited TRAIN posts")?
        {
            ensure!(
                bindings
                    .insert(text(&row["post"], "id")?.to_owned(), row)
                    .is_none(),
                "Repeated audit post ID"
            );
        }
        ensure!(
            bindings.len() == rows.len(),
            "TRAIN audit/query count differs"
        );
        let cache = PathBuf::from(text(settings, "feature_cache")?);
        let mut loaded = BTreeMap::<String, Vec<(String, Features)>>::new();
        let mut sources = Vec::new();
        let mut seen_ids = BTreeSet::new();
        let mut seen_authors = BTreeSet::new();
        for row in rows {
            let id = text(row, "id")?;
            let author = text(row, "author")?;
            let date = text(row, "date")?;
            let group = text(row, "source_group")?;
            let source_hash = text(row, "text_sha256")?;
            ensure!(
                source_hash.len() == 64
                    && source_hash
                        .bytes()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "Malformed TRAIN source hash"
            );
            ensure!(
                seen_ids.insert(id)
                    && row["split"] == "train"
                    && authors.binary_search(&author.to_owned()).is_ok(),
                "TRAIN query is duplicated or outside author/split scope"
            );
            seen_authors.insert(author);
            validate_date(date)?;
            ensure!(
                group == format!("blog:{author}:date:{date}"),
                "TRAIN source group/date binding differs"
            );
            let binding = bindings.get(id).context("Query absent from target audit")?;
            for key in [
                "id",
                "author",
                "date",
                "source_group",
                "split",
                "text_sha256",
            ] {
                ensure!(
                    binding["post"][key] == row[key],
                    "TRAIN query/audit metadata differs: {key}"
                );
            }
            if !selected.contains(author) {
                continue;
            }
            let feature_digest = text(binding, "feature_cache_sha256")?;
            let wrapper = bound_json(
                &cache.join(format!("{source_hash}.features.json")),
                feature_digest,
            )?;
            let features: Features = serde_json::from_value(wrapper["features"].clone())
                .context("Missing cached Features")?;
            ensure!(
                features.source_sha256 == source_hash
                    && features.schema == FEATURE_VERSION
                    && features.parser_identity == parser_identity
                    && features.families.keys().eq(space.family_schemas.keys()),
                "Target feature source/parser/schema differs"
            );
            ensure!(
                unavailable(&features).is_empty(),
                "Target post lacks required original feature opportunities"
            );
            loaded
                .entry(author.into())
                .or_default()
                .push((group.into(), features));
            sources.push(json!({"id":id,"author":author,"date":date,"source_group":group,"text_sha256":source_hash,
                "feature_cache_sha256":feature_digest,"annotation_sha256":binding["annotation_sha256"]}));
        }
        ensure!(
            seen_authors == authors.iter().map(String::as_str).collect()
                && loaded.keys().eq(selected.iter()),
            "Original or selected author coverage differs"
        );
        let mut targets = Vec::new();
        let mut target_bindings = Vec::new();
        for (author, posts) in &loaded {
            let dates = posts
                .iter()
                .map(|(group, _)| group)
                .collect::<BTreeSet<_>>();
            ensure!(
                dates.len() >= 2,
                "Target author has fewer than two eligible TRAIN dates"
            );
            let profile = grouped_profile(
                &posts
                    .iter()
                    .map(|(group, features)| (group.as_str(), features))
                    .collect::<Vec<_>>(),
            )?;
            for (_, model) in &models {
                model.transform_profile(&profile)?;
            }
            target_bindings.push(
                json!({"author":author,"posts":posts.len(),"dates":dates.len(),
                "profile_sha256":hash(&serde_json::to_vec(&profile)?)}),
            );
            targets.push((author.clone(), profile));
        }
        let receipt = json!({"schema":"slopninja-edit-utility-target-audit-v1","selected_author_ids":selected,
            "target_author_policy":settings["target_author_policy"],"target_profile_policy":settings["target_profile_policy"],
            "target_manifest_sha256":manifest_hash,"target_queries_sha256":manifest["queries_sha256"],
            "target_audit_sha256":manifest["audit_sha256"],"reference_space_sha256":space_binding["sha256"],
            "source_space_id":space.id,"feature_schema":FEATURE_VERSION,"parser_identity":parser_identity,
            "models":model_bindings,"targets":target_bindings,"selected_post_bindings":sources,
            "other_author_feature_caches_read":0,"annotation_or_text_files_read":0,"validation_or_test_files_read":0,
            "parameter_updates":0});
        Ok(Self {
            parser_identity,
            models,
            targets,
            audit: receipt,
        })
    }

    pub fn target_audit(&self) -> Value {
        self.audit.clone()
    }

    pub fn score(&self, doc: &Document) -> Result<Value> {
        ensure!(
            doc.parser_identity == self.parser_identity,
            "Query parser identity differs from frozen models"
        );
        let features = DefaultExtractor.extract(doc)?;
        let unavailable_families = unavailable(&features);
        if !unavailable_families.is_empty() {
            return Ok(
                json!({"status":"unscorable","unavailable_families":unavailable_families,
                "reason":"Zero observed opportunities in required original14 families; no zero filling or family removal"}),
            );
        }
        let query = document_values(&features)?;
        let mut scores = Vec::new();
        let mut sum = 0.0;
        for (seed, model) in &self.models {
            for (author, target) in &self.targets {
                let value = model.score_profiles(&query, target)?;
                sum += value.logit;
                scores.push(json!({"seed":seed,"author":author,"logit":value.logit,
                    "adjusted_family_distances":value.adjusted_family_distances,
                    "minimum_unselected_tail":value.minimum_unselected_tail}));
            }
        }
        ensure!(
            !scores.is_empty() && sum.is_finite(),
            "Empty or nonfinite score collection"
        );
        Ok(json!({"status":"scored","mean_logit":sum/scores.len() as f64,"scores":scores}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_reads_reject_mutation_and_malformed_hash() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.json");
        fs::write(&path, b"{}\n").unwrap();
        let expected = hash(b"{}\n");
        assert!(bound_json(&path, &expected).is_ok());
        assert!(bound_json(&path, "not-a-sha").is_err());
        fs::write(&path, b"{\"changed\":true}\n").unwrap();
        assert!(bound_json(&path, &expected).is_err());
    }

    #[test]
    fn no_opportunities_are_unscorable_and_parser_mismatch_is_an_error() {
        let scorer = Scoring {
            parser_identity: "synthetic-parser-v1".into(),
            models: vec![],
            targets: vec![],
            audit: json!({}),
        };
        let mut doc = Document {
            text: String::new(),
            parser_identity: "synthetic-parser-v1".into(),
            tokens: vec![],
            sentences: vec![],
        };
        let result = scorer.score(&doc).unwrap();
        assert_eq!(result["status"], "unscorable");
        assert_eq!(result["unavailable_families"].as_array().unwrap().len(), 14);
        doc.parser_identity = "different-parser".into();
        assert!(scorer.score(&doc).is_err());
    }

    #[test]
    fn target_profile_balances_dates_before_posts() {
        let features = |value: &str| Features {
            schema: FEATURE_VERSION.into(),
            parser_identity: "synthetic".into(),
            source_sha256: "a".repeat(64),
            families: BTreeMap::from([(
                "word".into(),
                Family::Distribution {
                    counts: BTreeMap::from([(value.into(), 1)]),
                    opportunities: 1,
                },
            )]),
        };
        let a = features("alpha");
        let b = features("beta");
        let profile =
            grouped_profile(&[("date1", &a), ("date1", &a), ("date1", &a), ("date2", &b)]).unwrap();
        assert_eq!(profile["word"]["alpha"], 0.5);
        assert_eq!(profile["word"]["beta"], 0.5);
    }
}
