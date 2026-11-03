//! Append a versioned raw family while retaining every original distance bit.
//! Inputs contain metadata and cached features; corpus text is never opened.
use anyhow::{Context, Result, ensure};
use grammar_core::features::{Family, Features};
use grammar_core::space::{Axis, FamilySchema, Kind};
use grammar_eval::validate_date;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path},
};

#[path = "clause_composition.rs"]
#[allow(dead_code)]
mod clause_composition;
pub const SCHEMA: &str = "slopninja-named-family-tensor-v1";
const OLD_FAMILIES: [&str; 14] = [
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
type Probabilities = BTreeMap<String, f64>;

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("missing string {key}"))
}
fn valid_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())
}
fn read_bound(path: &Path, digest: &str) -> Result<Vec<u8>> {
    ensure!(valid_hash(digest), "malformed expected SHA256");
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    ensure!(hash(&bytes) == digest, "SHA256 differs: {}", path.display());
    Ok(bytes)
}
fn json_bound(path: &Path, digest: &str) -> Result<Value> {
    Ok(serde_json::from_slice(&read_bound(path, digest)?)?)
}
fn write_fresh(path: &Path, bytes: &[u8]) -> Result<String> {
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?
        .write_all(bytes)?;
    Ok(hash(bytes))
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<String> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_fresh(path, &bytes)
}
fn local_path(binding: &Value) -> Result<&Path> {
    let path = Path::new(string(binding, "path")?);
    ensure!(
        !path.as_os_str().is_empty()
            && path.components().all(|c| matches!(c, Component::Normal(_))),
        "array path must remain inside export"
    );
    Ok(path)
}
fn load_array(root: &Path, binding: &Value, shape: &[usize]) -> Result<Vec<u8>> {
    ensure!(
        binding["dtype"] == "float64-le" && binding["shape"] == json!(shape),
        "array dtype/shape differs"
    );
    let count = shape
        .iter()
        .try_fold(1usize, |n, d| n.checked_mul(*d).context("shape overflow"))?;
    let path = root.join(local_path(binding)?).canonicalize()?;
    ensure!(
        path.starts_with(root.canonicalize()?),
        "array symlink escapes export"
    );
    let bytes = read_bound(&path, string(binding, "sha256")?)?;
    ensure!(
        bytes.len() == count.checked_mul(8).context("array size overflow")?
            && binding["bytes"].as_u64() == Some(bytes.len() as u64),
        "array size differs"
    );
    for word in bytes.as_chunks::<8>().0 {
        let x = f64::from_le_bytes(*word);
        ensure!(x.is_finite() && x >= 0., "nonfinite or negative distance");
    }
    Ok(bytes)
}
fn array_binding(path: &str, bytes: &[u8], shape: &[usize]) -> Value {
    json!({"path":path,"dtype":"float64-le","shape":shape,"bytes":bytes.len(),"sha256":hash(bytes)})
}

/// A hash-bound prepared file may have a historical absolute prefix. Match its
/// filename and bytes, never reopen the stale path or trust an unbound file.
fn prepared_file(
    prepared: &Path,
    name: &str,
    manifest: &Value,
    audit: &Value,
) -> Result<(Value, String)> {
    let bytes = fs::read(prepared.join(name))?;
    let digest = hash(&bytes);
    for bindings in [&manifest["input_bindings"], &audit["input_bindings"]] {
        ensure!(
            bindings
                .as_object()
                .context("input bindings missing")?
                .iter()
                .any(
                    |(p, h)| Path::new(p).file_name().and_then(|n| n.to_str()) == Some(name)
                        && h.as_str() == Some(&digest)
                ),
            "prepared {name} is not bound to source export"
        );
    }
    Ok((serde_json::from_slice(&bytes)?, digest))
}

fn unique_file_binding(manifest: &Value, name: &str) -> Result<String> {
    let values: BTreeSet<_> = manifest["input_bindings"]
        .as_object()
        .context("input bindings missing")?
        .iter()
        .filter(|(p, _)| Path::new(p).file_name().and_then(|n| n.to_str()) == Some(name))
        .map(|(_, h)| h.as_str().context("binding must be a SHA256 string"))
        .collect::<Result<_>>()?;
    ensure!(values.len() == 1, "missing or ambiguous {name} binding");
    let value = values.first().unwrap();
    ensure!(valid_hash(value), "malformed {name} binding");
    Ok(value.to_string())
}

/// Read only the fixed numerical axes. The large, extended categorical basis
/// is hash-bound but need not be materialized to verify the numerical geometry.
#[derive(Debug, Deserialize)]
struct SpaceIdentity {
    id: String,
    geometry: String,
    feature_schema: String,
    parser_identity: String,
    family_schemas: BTreeMap<String, FamilySchema>,
    #[serde(deserialize_with = "numerical_axes")]
    axes: BTreeMap<(String, String), Axis>,
}

fn numerical_axes<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<(String, String), Axis>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = BTreeMap<(String, String), Axis>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an array of named space axes")
        }
        fn visit_seq<S: serde::de::SeqAccess<'de>>(
            self,
            mut seq: S,
        ) -> std::result::Result<Self::Value, S::Error> {
            let mut axes = BTreeMap::new();
            while let Some(axis) = seq.next_element::<Axis>()? {
                if axis.kind != Kind::Distribution {
                    let key = (axis.family.clone(), axis.feature.clone());
                    if axes.insert(key, axis).is_some() {
                        return Err(serde::de::Error::custom("duplicate numerical axis"));
                    }
                }
            }
            Ok(axes)
        }
    }
    deserializer.deserialize_seq(Visitor)
}

fn validate_space_pair(
    reference: &SpaceIdentity,
    prepared: &SpaceIdentity,
    target_id: &str,
    manifest: &Value,
    catalog: &Value,
) -> Result<()> {
    ensure!(
        valid_hash(&reference.id)
            && valid_hash(&prepared.id)
            && reference.id == string(manifest, "source_space_id")?
            && prepared.id == target_id,
        "reference or prepared target space identity differs"
    );
    ensure!(
        reference.geometry == "unslop-hellinger-euclidean-v2"
            && prepared.geometry == reference.geometry
            && reference.feature_schema == string(manifest, "feature_schema")?
            && reference.parser_identity == string(manifest, "parser_identity")?
            && reference.feature_schema == prepared.feature_schema
            && reference.parser_identity == prepared.parser_identity
            && reference.family_schemas == prepared.family_schemas
            && reference
                .family_schemas
                .keys()
                .map(String::as_str)
                .eq(OLD_FAMILIES),
        "prepared/reference feature contract differs"
    );
    ensure!(
        !reference.axes.is_empty() && reference.axes.keys().eq(prepared.axes.keys()),
        "numerical axis membership differs"
    );
    let mut denominators = BTreeMap::<&str, usize>::new();
    for ((family, feature), axis) in &reference.axes {
        let actual = &prepared.axes[&(family.clone(), feature.clone())];
        ensure!(
            axis.kind == actual.kind
                && axis.kind == reference.family_schemas[family].kind
                && axis.center.is_finite()
                && axis.scale.is_finite()
                && axis.scale > 0.0
                && axis.multiplier.is_finite()
                && axis.center.to_bits() == actual.center.to_bits()
                && axis.scale.to_bits() == actual.scale.to_bits()
                && axis.multiplier.to_bits() == actual.multiplier.to_bits(),
            "prepared numerical geometry changed: {family}/{feature}"
        );
        *denominators.entry(family).or_default() += 1;
    }
    for (family, schema) in &reference.family_schemas {
        if schema.kind != Kind::Distribution {
            let names: BTreeSet<_> = reference
                .axes
                .keys()
                .filter(|(f, _)| f == family)
                .map(|(_, k)| k.clone())
                .collect();
            ensure!(
                schema.fixed_features.as_ref() == Some(&names),
                "incomplete original numerical family"
            );
        }
    }
    for axis in catalog["coordinates"]
        .as_array()
        .context("catalog coordinates missing")?
    {
        if axis["kind"] != "distribution" {
            let family = string(axis, "family")?;
            let feature = string(axis, "feature")?;
            let original = reference
                .axes
                .get(&(family.into(), feature.into()))
                .context("catalog has an unknown numerical axis")?;
            ensure!(
                axis["scale"].as_f64().map(f64::to_bits) == Some(original.scale.to_bits())
                    && axis["original_family_axis_count"].as_u64()
                        == Some(denominators[family] as u64),
                "catalog numerical scale or full denominator differs"
            );
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Query {
    id: String,
    author: String,
    date: String,
    source_group: String,
    split: String,
    own: usize,
    loss_weight: f64,
    text_sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Post {
    id: String,
    author: String,
    date: String,
    source_group: String,
    split: String,
    text_sha256: String,
}
impl Post {
    fn from_prepared(v: &Value) -> Result<Self> {
        let p = Self {
            id: string(v, "id")?.into(),
            author: string(v, "author_id")?.into(),
            date: string(&v["provenance"], "supplied_composition_date")?.into(),
            source_group: string(v, "source_group")?.into(),
            split: string(v, "split")?.into(),
            text_sha256: string(v, "text_sha256")?.into(),
        };
        validate_date(&p.date)?;
        ensure!(
            !p.id.is_empty()
                && !p.author.is_empty()
                && !p.source_group.is_empty()
                && valid_hash(&p.text_sha256),
            "invalid post metadata"
        );
        Ok(p)
    }
    fn agrees(&self, q: &Query) -> bool {
        self.id == q.id
            && self.author == q.author
            && self.date == q.date
            && self.source_group == q.source_group
            && self.split == q.split
            && self.text_sha256 == q.text_sha256
    }
}

fn validate_probabilities(p: &Probabilities) -> Result<()> {
    ensure!(
        !p.is_empty()
            && p.iter()
                .all(|(k, v)| !k.is_empty() && v.is_finite() && *v > 0.0)
            && (p.values().sum::<f64>() - 1.0).abs() <= 1e-10,
        "missing or malformed probability distribution"
    );
    Ok(())
}
fn probabilities(family: &Family) -> Result<Probabilities> {
    let Family::Distribution {
        counts,
        opportunities,
    } = family
    else {
        anyhow::bail!("new family is not categorical")
    };
    ensure!(
        *opportunities > 0,
        "new family unavailable; abort without dropping any row"
    );
    ensure!(
        counts
            .values()
            .try_fold(0u64, |n, v| n.checked_add(*v))
            .context("count overflow")?
            == *opportunities,
        "family denominator differs"
    );
    let p = counts
        .iter()
        .map(|(k, n)| (k.clone(), *n as f64 / *opportunities as f64))
        .collect();
    validate_probabilities(&p)?;
    Ok(p)
}

/// Average probabilities within dates, then average dates. Exclusion precedes
/// both operations; there is no square-root averaging or pooled event count.
fn date_profile(rows: &[(&str, &Probabilities)], excluded: Option<&str>) -> Result<Probabilities> {
    let mut dates: BTreeMap<&str, Vec<&Probabilities>> = BTreeMap::new();
    for (date, p) in rows {
        validate_probabilities(p)?;
        if Some(*date) != excluded {
            dates.entry(date).or_default().push(p);
        }
    }
    ensure!(!dates.is_empty(), "no independent training date remains");
    let mut out = Probabilities::new();
    for posts in dates.values() {
        let w = 1.0 / dates.len() as f64 / posts.len() as f64;
        for p in posts {
            for (key, value) in *p {
                *out.entry(key.clone()).or_default() += w * value;
            }
        }
    }
    validate_probabilities(&out)?;
    Ok(out)
}
fn hellinger(a: &Probabilities, b: &Probabilities) -> Result<f64> {
    validate_probabilities(a)?;
    validate_probabilities(b)?;
    let overlap: f64 = a
        .iter()
        .map(|(key, p)| (p * b.get(key).copied().unwrap_or(0.)).sqrt())
        .sum();
    ensure!(
        overlap.is_finite() && overlap <= 1.0 + 1e-10,
        "invalid Hellinger overlap"
    );
    Ok((1.0 - overlap).max(0.0))
}
fn balanced_weights(queries: &[Query]) -> Result<Vec<f64>> {
    let mut counts = BTreeMap::<&str, BTreeMap<&str, usize>>::new();
    for q in queries {
        *counts
            .entry(&q.author)
            .or_default()
            .entry(&q.date)
            .or_default() += 1;
    }
    ensure!(!counts.is_empty(), "empty queries");
    Ok(queries
        .iter()
        .map(|q| {
            1.0 / counts.len() as f64
                / counts[q.author.as_str()].len() as f64
                / counts[q.author.as_str()][q.date.as_str()] as f64
        })
        .collect())
}
fn validate_queries(
    queries: &[Query],
    posts: &BTreeMap<String, Post>,
    authors: &[String],
    split: &str,
) -> Result<()> {
    ensure!(
        !queries.is_empty() && !authors.is_empty() && authors.windows(2).all(|p| p[0] < p[1]),
        "empty/unsorted author or query catalog"
    );
    let mut seen = BTreeSet::new();
    for (q, w) in queries.iter().zip(balanced_weights(queries)?) {
        ensure!(seen.insert(&q.id), "duplicate query ID");
        ensure!(
            q.split == split && authors.get(q.own) == Some(&q.author),
            "query split/own-author alignment differs"
        );
        ensure!(
            posts
                .get(&q.id)
                .context("query lacks eligible prepared post")?
                .agrees(q),
            "query metadata differs from prepared post"
        );
        ensure!(
            q.loss_weight.is_finite() && q.loss_weight > 0.0 && (q.loss_weight - w).abs() <= 1e-14,
            "query loss weight is not author/date/post balanced"
        );
    }
    let expected: BTreeSet<_> = posts
        .values()
        .filter(|p| p.split == split)
        .map(|p| &p.id)
        .collect();
    ensure!(
        seen == expected,
        "query support differs from source eligible split"
    );
    ensure!(
        queries.iter().map(|q| &q.author).collect::<BTreeSet<_>>() == authors.iter().collect(),
        "query author support differs"
    );
    Ok(())
}
fn validate_groups(posts: &BTreeMap<String, Post>) -> Result<()> {
    let mut groups = BTreeMap::new();
    let mut dates = BTreeMap::new();
    let mut texts = BTreeSet::new();
    for p in posts.values() {
        let group_identity = (&p.author, &p.date, &p.split);
        if let Some(old) = groups.insert(&p.source_group, group_identity) {
            ensure!(
                old == group_identity,
                "source group crosses author/date/split"
            );
        }
        if let Some(old) = dates.insert((&p.author, &p.date), &p.split) {
            ensure!(old == &p.split, "date crosses split");
        }
        ensure!(
            texts.insert(&p.text_sha256),
            "duplicate source text in loaded posts"
        );
    }
    Ok(())
}

/// Insert the new column by name and preserve source f64 bytes, including -0.
fn augment(baseline: &[u8], extra: &[f64], old: &[String], all: &[String]) -> Result<Vec<u8>> {
    ensure!(
        old == OLD_FAMILIES
            && all.windows(2).all(|p| p[0] < p[1])
            && all.len() == old.len() + 1
            && all
                .iter()
                .filter(|f| !old.contains(f))
                .map(String::as_str)
                .eq([clause_composition::FAMILY]),
        "family names/order differ"
    );
    ensure!(
        baseline.len()
            == extra
                .len()
                .checked_mul(old.len())
                .and_then(|n| n.checked_mul(8))
                .context("shape overflow")?,
        "baseline/new row count differs"
    );
    let mut out = Vec::with_capacity(extra.len() * all.len() * 8);
    for (row, value) in baseline.chunks_exact(old.len() * 8).zip(extra) {
        ensure!(
            value.is_finite() && (0.0..=1.0).contains(value),
            "invalid new distance"
        );
        for name in all {
            if let Some(i) = old.iter().position(|x| x == name) {
                out.extend_from_slice(&row[i * 8..(i + 1) * 8]);
            } else {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    Ok(out)
}

pub fn export(
    coordinate_export: &Path,
    expected: &str,
    prepared: &Path,
    reference_space: &Path,
    cache: &Path,
    protocol: &Path,
    out: &Path,
) -> Result<Value> {
    ensure!(!out.exists(), "output must be a fresh directory");
    let manifest = json_bound(&coordinate_export.join("manifest.json"), expected)?;
    ensure!(
        manifest["schema"] == "slopninja-coordinate-frontier-export-v1",
        "unsupported coordinate export schema"
    );
    let split = string(&manifest, "split")?;
    ensure!(
        ["train", "dev", "test"].contains(&split),
        "unsupported split"
    );
    let old: Vec<String> = serde_json::from_value(manifest["families"].clone())?;
    ensure!(old == OLD_FAMILIES, "legacy family names/order differ");
    let catalog_bytes = read_bound(
        &coordinate_export.join("catalog.json"),
        string(&manifest, "catalog_sha256")?,
    )?;
    let catalog: Value = serde_json::from_slice(&catalog_bytes)?;
    ensure!(
        catalog["schema"] == "slopninja-coordinate-frontier-catalog-v1",
        "catalog schema differs"
    );
    for key in [
        "source_space_id",
        "feature_schema",
        "parser_identity",
        "families",
    ] {
        ensure!(
            manifest[key] == catalog[key],
            "catalog/export identity differs: {key}"
        );
    }
    ensure!(
        valid_hash(string(&manifest, "source_space_id")?)
            && !string(&manifest, "parser_identity")?.is_empty(),
        "invalid source identity"
    );
    let query_bytes = read_bound(
        &coordinate_export.join("queries.json"),
        string(&manifest, "queries_sha256")?,
    )?;
    let queries: Vec<Query> = serde_json::from_slice(&query_bytes)?;
    let source_audit = json_bound(
        &coordinate_export.join("audit.json"),
        string(&manifest, "audit_sha256")?,
    )?;
    let reference_space_sha256 = unique_file_binding(&manifest, "space.json")?;
    let expected_splits = if split == "train" {
        json!(["train"])
    } else {
        json!(["train", split])
    };
    ensure!(
        source_audit["loaded_splits"] == expected_splits,
        "source audit split scope differs"
    );
    let authors: Vec<String> = serde_json::from_value(manifest["authors"].clone())?;
    ensure!(
        manifest["query_count"].as_u64() == Some(queries.len() as u64),
        "query count differs"
    );
    let (annotations, annotation_digest) =
        prepared_file(prepared, "annotation-audit.json", &manifest, &source_audit)?;
    let (old_targets, target_digest) =
        prepared_file(prepared, "author-targets.json", &manifest, &source_audit)?;
    let (implementation, implementation_digest) =
        prepared_file(prepared, "implementation.json", &manifest, &source_audit)?;
    ensure!(
        implementation["schema"] == "unslop-author-evaluation-implementation-v1",
        "prepared implementation schema differs"
    );
    for (name, digest) in [
        ("annotation-audit.json", &annotation_digest),
        ("author-targets.json", &target_digest),
    ] {
        ensure!(
            implementation["artifacts_sha256"][name] == *digest,
            "prepared implementation artifact differs: {name}"
        );
    }
    let prepared_space_sha256 = string(&implementation["artifacts_sha256"], "space.json")?;
    let reference_identity: SpaceIdentity =
        serde_json::from_slice(&read_bound(reference_space, &reference_space_sha256)?)?;
    let prepared_identity: SpaceIdentity = serde_json::from_slice(&read_bound(
        &prepared.join("space.json"),
        prepared_space_sha256,
    )?)?;
    validate_space_pair(
        &reference_identity,
        &prepared_identity,
        string(&old_targets, "space_id")?,
        &manifest,
        &catalog,
    )?;
    let target_authors: Vec<_> = old_targets["profiles"]
        .as_object()
        .context("prepared profiles")?
        .keys()
        .cloned()
        .collect();
    ensure!(target_authors == authors, "prepared target authors differ");
    let mut posts = BTreeMap::new();
    for row in annotations
        .as_array()
        .context("prepared annotation audit")?
    {
        let p = &row["post"];
        if row["status"] != "eligible"
            || !authors.iter().any(|a| p["author_id"] == *a)
            || !(p["split"] == "train" || p["split"] == split)
        {
            continue;
        }
        let p = Post::from_prepared(p)?;
        ensure!(
            posts.insert(p.id.clone(), p).is_none(),
            "duplicate prepared post ID"
        );
    }
    validate_groups(&posts)?;
    validate_queries(&queries, &posts, &authors, split)?;
    let mut bound_training = BTreeMap::new();
    for v in old_targets["training_bindings"]
        .as_array()
        .context("training bindings")?
    {
        let p = Post::from_prepared(v)?;
        ensure!(
            p.split == "train" && authors.contains(&p.author),
            "unexpected training target member"
        );
        ensure!(
            bound_training.insert(p.id.clone(), p).is_none(),
            "duplicate training binding"
        );
    }
    let training: BTreeMap<_, _> = posts
        .iter()
        .filter(|(_, p)| p.split == "train")
        .map(|(id, p)| (id.clone(), p.clone()))
        .collect();
    ensure!(
        training == bound_training,
        "eligible training members differ from frozen targets"
    );
    ensure!(
        training
            .values()
            .map(|p| &p.author)
            .collect::<BTreeSet<_>>()
            == authors.iter().collect(),
        "training author support differs"
    );
    let mut cache_bindings = BTreeMap::new();
    for binding in source_audit["cache_bindings"]
        .as_array()
        .context("cache bindings")?
    {
        ensure!(
            cache_bindings
                .insert(string(binding, "id")?.to_owned(), binding)
                .is_none(),
            "duplicate cached post ID"
        );
    }
    ensure!(
        cache_bindings.keys().eq(posts.keys()),
        "cache/prepared post support differs"
    );
    let mut profiles = BTreeMap::new();
    let mut loaded = Vec::new();
    for (id, post) in &posts {
        let binding = cache_bindings[id];
        ensure!(
            binding["text_sha256"] == post.text_sha256
                && valid_hash(string(binding, "annotation_sha256")?),
            "cache source binding differs"
        );
        let feature_bytes = read_bound(
            &cache.join(format!("{}.features.json", post.text_sha256)),
            string(binding, "feature_cache_sha256")?,
        )?;
        let wrapper: Value = serde_json::from_slice(&feature_bytes)?;
        let features: Features = serde_json::from_value(
            wrapper
                .get("features")
                .context("missing feature wrapper")?
                .clone(),
        )?;
        ensure!(
            features.schema == manifest["feature_schema"]
                && features.parser_identity == manifest["parser_identity"]
                && features.source_sha256 == post.text_sha256,
            "cached feature identity differs"
        );
        let projected = clause_composition::project_legacy(&features)
            .with_context(|| format!("project {id}"))?;
        let family = &projected.families[clause_composition::FAMILY];
        let p = probabilities(family)
            .with_context(|| format!("new family unavailable for required post {id}"))?;
        loaded.push(json!({"post":post,"feature_cache_sha256":binding["feature_cache_sha256"],"annotation_sha256":binding["annotation_sha256"],"new_family_sha256":hash(&serde_json::to_vec(family)?),"probabilities_sha256":hash(&serde_json::to_vec(&p)?)}));
        profiles.insert(id.clone(), p);
    }
    let mut by_author = BTreeMap::<&str, Vec<(&str, &Probabilities)>>::new();
    for p in training.values() {
        by_author
            .entry(&p.author)
            .or_default()
            .push((&p.date, &profiles[&p.id]));
    }
    let mut author_profiles = BTreeMap::new();
    for author in &authors {
        author_profiles.insert(
            author.clone(),
            date_profile(&by_author[author.as_str()], None)?,
        );
    }
    let mut held = BTreeMap::new();
    if split == "train" {
        for q in &queries {
            let key = (q.author.clone(), q.date.clone());
            if let std::collections::btree_map::Entry::Vacant(entry) = held.entry(key) {
                entry.insert(date_profile(&by_author[q.author.as_str()], Some(&q.date))?);
            }
        }
    }
    let mut extra = Vec::with_capacity(queries.len() * authors.len());
    for q in &queries {
        for (i, author) in authors.iter().enumerate() {
            let target = if split == "train" && i == q.own {
                &held[&(q.author.clone(), q.date.clone())]
            } else {
                &author_profiles[author]
            };
            extra.push(hellinger(&profiles[&q.id], target)?);
        }
    }
    let baseline = load_array(
        coordinate_export,
        &manifest["arrays"]["family_distances"],
        &[queries.len(), authors.len(), old.len()],
    )?;
    let mut families = old.clone();
    families.push(clause_composition::FAMILY.into());
    families.sort();
    let mapping: Vec<_> = old
        .iter()
        .map(|name| families.iter().position(|x| x == name).unwrap())
        .collect();
    let extended = augment(&baseline, &extra, &old, &families)?;
    let protocol_bytes = fs::read(protocol)?;
    ensure!(
        serde_json::from_slice::<Value>(&protocol_bytes)?.is_object(),
        "protocol must be a JSON object"
    );
    let own_targets: Vec<_> = queries.iter().filter(|_| split == "train").map(|q| json!({"id":q.id,"author":q.author,"excluded_date":q.date,"probabilities":held[&(q.author.clone(),q.date.clone())]})).collect();
    let targets = json!({"schema":"slopninja-named-family-targets-v1","family":clause_composition::FAMILY,"authors":authors,"author_profiles":author_profiles,"training_positive_profiles":own_targets,"training_bindings":training.values().collect::<Vec<_>>(),"profile_rule":"mean raw probabilities within dates, then mean dates; TRAIN own target excludes the whole query date"});
    let audit = json!({"schema":"slopninja-named-family-tensor-audit-v1","loaded_splits":expected_splits,"posts":loaded,"loaded_post_count":posts.len(),"training_post_count":training.len(),"training_author_count":authors.len(),"unavailable_required_posts":0,"source_audit_sha256":manifest["audit_sha256"],"source_annotation_audit_sha256":annotation_digest,"source_author_targets_sha256":target_digest,"baseline_bytes_exact":true,"baseline_mapped_columns_exact":true,"cache_annotation_policy":"annotation hashes inherited from the bound coordinate audit; only feature-cache bytes are opened and projected","prepared_space_id":prepared_identity.id,"prepared_space_sha256":prepared_space_sha256,"source_implementation_sha256":implementation_digest,"original_numerical_geometry_verified":true,"original_numerical_axis_count":reference_identity.axes.len()});
    fs::create_dir(out)?;
    let targets_sha = write_json(&out.join("targets.json"), &targets)?;
    let audit_sha = write_json(&out.join("audit.json"), &audit)?;
    let queries_sha = write_fresh(&out.join("queries.json"), &query_bytes)?;
    write_fresh(&out.join("baseline-family-distances.f64"), &baseline)?;
    write_fresh(&out.join("family-distances.f64"), &extended)?;
    let source_sha = json!({"named_family_tensor.rs":hash(include_bytes!("named_family_tensor.rs")),"bin/unslop-export-named-families.rs":hash(include_bytes!("bin/unslop-export-named-families.rs")),"clause_composition.rs":hash(include_bytes!("clause_composition.rs")),"grammar-eval/lib.rs":hash(include_bytes!("lib.rs"))});
    let result = json!({"schema":SCHEMA,"families":families,"baseline_families":old,"baseline_family_indices":mapping,"new_family":clause_composition::FAMILY,
        "authors":authors,"split":split,"query_count":queries.len(),"training_post_count":training.len(),"source_space_id":manifest["source_space_id"],"parser_identity":manifest["parser_identity"],"legacy_feature_schema":manifest["feature_schema"],"feature_schema":clause_composition::FEATURE_VERSION,
        "arrays":{"family_distances":array_binding("family-distances.f64",&extended,&[queries.len(),authors.len(),families.len()]),"baseline_family_distances":array_binding("baseline-family-distances.f64",&baseline,&[queries.len(),authors.len(),old.len()])},
        "queries_sha256":queries_sha,"targets_sha256":targets_sha,"audit_sha256":audit_sha,"coordinate_export_manifest_sha256":expected,"source_catalog_sha256":manifest["catalog_sha256"],"baseline_source_array_sha256":manifest["arrays"]["family_distances"]["sha256"],
        "protocol_sha256":hash(&protocol_bytes),"reference_space_sha256":reference_space_sha256,"prepared_space_id":prepared_identity.id,"prepared_space_sha256":prepared_space_sha256,"input_bindings":{"coordinate_export":coordinate_export,"prepared":prepared,"reference_space":reference_space,"cache":cache,"protocol":protocol,"original_export_input_bindings":manifest["input_bindings"],"source_annotation_audit_sha256":annotation_digest,"source_author_targets_sha256":target_digest,"source_implementation_sha256":implementation_digest},
        "source_sha256":source_sha,"executed_binary_sha256":hash(&fs::read(std::env::current_exe()?)?),"geometry":"Original14 f64 bytes and scales unchanged; new full squared Hellinger distance 1-sum(sqrt(p*q)), no vocabulary truncation; means of raw probabilities balance dates and posts",
        "training_positive":"TRAIN own target excludes the entire query date; DEV/TEST use all eligible training dates","missing_family_policy":"abort if any required query or training reference lacks new-family opportunities; no support attrition","external_model_calls":0,"detector_calls":0});
    write_json(&out.join("manifest.json"), &result)?;
    Ok(
        json!({"schema":SCHEMA,"split":split,"authors":authors.len(),"queries":queries.len(),"training_posts":training.len(),"families":families.len(),"manifest_sha256":hash(&fs::read(out.join("manifest.json"))?)}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(pairs: &[(&str, f64)]) -> Probabilities {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }
    #[test]
    fn probabilities_are_averaged_before_square_roots_and_dates_are_equal() {
        let a = p(&[("a", 1.)]);
        let b = p(&[("b", 1.)]);
        let rows = [("d1", &a), ("d1", &a), ("d2", &b)];
        let target = date_profile(&rows, None).unwrap();
        assert_eq!(target, p(&[("a", 0.5), ("b", 0.5)]));
        assert!((hellinger(&a, &target).unwrap() - (1. - 0.5f64.sqrt())).abs() < 1e-15);
    }
    #[test]
    fn entire_date_is_excluded_before_averaging() {
        let a = p(&[("a", 1.)]);
        let b = p(&[("b", 1.)]);
        assert_eq!(
            date_profile(&[("d1", &a), ("d1", &a), ("d2", &b)], Some("d1")).unwrap(),
            b
        );
        assert!(date_profile(&[("d1", &a), ("d1", &a)], Some("d1")).is_err());
    }
    #[test]
    fn unseen_tails_remain_in_full_distribution() {
        let a = p(&[("shared", 0.25), ("unseen-a", 0.75)]);
        let b = p(&[("shared", 0.25), ("unseen-b", 0.75)]);
        assert_eq!(hellinger(&a, &b).unwrap(), 0.75);
        assert_eq!(hellinger(&p(&[("a", 1.)]), &p(&[("b", 1.)])).unwrap(), 1.);
    }
    #[test]
    fn malformed_and_unavailable_distributions_fail_closed() {
        for x in [
            p(&[]),
            p(&[("a", 0.)]),
            p(&[("a", f64::NAN)]),
            p(&[("a", 0.9)]),
        ] {
            assert!(validate_probabilities(&x).is_err());
        }
        assert!(
            probabilities(&Family::Distribution {
                counts: BTreeMap::new(),
                opportunities: 0
            })
            .is_err()
        );
        assert!(
            probabilities(&Family::Distribution {
                counts: BTreeMap::from([("a".into(), 2)]),
                opportunities: 3
            })
            .is_err()
        );
    }
    fn query(id: &str, author: &str, date: &str, own: usize, weight: f64) -> Query {
        Query {
            id: id.into(),
            author: author.into(),
            date: date.into(),
            source_group: format!("{author}:{date}"),
            split: "train".into(),
            own,
            loss_weight: weight,
            text_sha256: hash(id.as_bytes()),
        }
    }
    fn post(q: &Query) -> Post {
        Post {
            id: q.id.clone(),
            author: q.author.clone(),
            date: q.date.clone(),
            source_group: q.source_group.clone(),
            split: q.split.clone(),
            text_sha256: q.text_sha256.clone(),
        }
    }
    #[test]
    fn query_alignment_and_date_balanced_loss_are_required() {
        let mut q = vec![
            query("1", "a", "d1", 0, 0.125),
            query("2", "a", "d1", 0, 0.125),
            query("3", "a", "d2", 0, 0.25),
            query("4", "b", "d1", 1, 0.5),
        ];
        let posts = q.iter().map(|q| (q.id.clone(), post(q))).collect();
        let authors = vec!["a".into(), "b".into()];
        validate_queries(&q, &posts, &authors, "train").unwrap();
        q[0].own = 1;
        assert!(validate_queries(&q, &posts, &authors, "train").is_err());
        q[0].own = 0;
        q[0].loss_weight = 0.25;
        assert!(validate_queries(&q, &posts, &authors, "train").is_err());
    }
    #[test]
    fn date_and_source_group_split_leakage_is_rejected() {
        let a = post(&query("1", "a", "d1", 0, 0.5));
        let mut b = post(&query("2", "a", "d1", 0, 0.5));
        b.split = "test".into();
        assert!(validate_groups(&BTreeMap::from([(a.id.clone(), a), (b.id.clone(), b)])).is_err());
    }
    #[test]
    fn original_columns_are_preserved_as_bytes_in_sorted_fifteen() {
        let old: Vec<_> = OLD_FAMILIES.iter().map(|s| s.to_string()).collect();
        let mut all = old.clone();
        all.push(clause_composition::FAMILY.into());
        all.sort();
        let values: Vec<_> = (0..28)
            .map(|i| if i == 0 { -0.0f64 } else { i as f64 / 7. })
            .collect();
        let bytes: Vec<_> = values.iter().flat_map(|x| x.to_le_bytes()).collect();
        let out = augment(&bytes, &[0.25, 0.75], &old, &all).unwrap();
        for r in 0..2 {
            for (i, name) in old.iter().enumerate() {
                let j = all.iter().position(|n| n == name).unwrap();
                assert_eq!(
                    &bytes[(r * 14 + i) * 8..(r * 14 + i + 1) * 8],
                    &out[(r * 15 + j) * 8..(r * 15 + j + 1) * 8]
                );
            }
        }
        all.swap(0, 1);
        assert!(augment(&bytes, &[0.25, 0.75], &old, &all).is_err());
        assert!(augment(&bytes, &[f64::NAN], &old, &old).is_err());
    }
    #[test]
    fn array_paths_are_strings_and_cannot_escape() {
        let binding = array_binding("tensor.f64", &0.0f64.to_le_bytes(), &[1]);
        assert!(binding["path"].is_string());
        assert!(local_path(&binding).is_ok());
        for path in ["../tensor.f64", "/tmp/tensor.f64", ""] {
            assert!(local_path(&json!({"path":path})).is_err());
        }
    }

    #[test]
    fn ambiguous_or_missing_space_bindings_fail() {
        let a = hash(b"a");
        let b = hash(b"b");
        assert_eq!(
            unique_file_binding(
                &json!({"input_bindings":{"old/space.json":a}}),
                "space.json"
            )
            .unwrap(),
            a
        );
        assert!(
            unique_file_binding(
                &json!({"input_bindings":{"old/space.json":a,"other/space.json":b}}),
                "space.json"
            )
            .is_err()
        );
        assert!(unique_file_binding(&json!({"input_bindings":{}}), "space.json").is_err());
    }

    #[test]
    fn genuinely_extended_space_id_is_allowed_but_numerical_changes_fail() {
        use grammar_core::space::{Config, Reference, fit};
        let features = Features {
            schema: "synthetic-test-features-v1".into(),
            parser_identity: "synthetic-parser-v1".into(),
            source_sha256: hash(b"reference"),
            families: OLD_FAMILIES
                .iter()
                .map(|name| {
                    let family = if name.starts_with("syntax_") {
                        Family::Metrics {
                            values: BTreeMap::from([("value".into(), 2.)]),
                            scale_floors: BTreeMap::from([("value".into(), 1.)]),
                            opportunities: 1,
                        }
                    } else {
                        Family::Distribution {
                            counts: BTreeMap::from([("seen".into(), 1)]),
                            opportunities: 1,
                        }
                    };
                    (name.to_string(), family)
                })
                .collect(),
        };
        let mut second = features.clone();
        second.source_sha256 = hash(b"second-reference");
        let reference = fit(
            &[
                Reference {
                    source_group: "reference-date".into(),
                    features: features.clone(),
                },
                Reference {
                    source_group: "second-reference-date".into(),
                    features: second,
                },
            ],
            &[],
            &Config::default(),
        )
        .unwrap();
        let mut fresh = features;
        fresh.source_sha256 = hash(b"fresh");
        fresh.families.insert(
            "word".into(),
            Family::Distribution {
                counts: BTreeMap::from([("new-word".into(), 1)]),
                opportunities: 1,
            },
        );
        let prepared = reference.extend_basis(&[fresh]).unwrap();
        assert_ne!(reference.id, prepared.id);
        let original: SpaceIdentity =
            serde_json::from_value(serde_json::to_value(&reference).unwrap()).unwrap();
        let mut expanded: SpaceIdentity =
            serde_json::from_value(serde_json::to_value(&prepared).unwrap()).unwrap();
        let manifest = json!({"source_space_id":reference.id,"feature_schema":reference.feature_schema,"parser_identity":reference.parser_identity});
        let mut catalog = json!({"coordinates":original.axes.values().map(|axis| json!({"family":axis.family,"feature":axis.feature,"kind":axis.kind,"scale":axis.scale,"original_family_axis_count":1})).collect::<Vec<_>>()});
        validate_space_pair(&original, &expanded, &prepared.id, &manifest, &catalog).unwrap();
        assert!(
            validate_space_pair(&original, &expanded, &reference.id, &manifest, &catalog).is_err()
        );
        let key = expanded.axes.first_key_value().unwrap().0.clone();
        expanded.axes.get_mut(&key).unwrap().scale *= 2.;
        assert!(
            validate_space_pair(&original, &expanded, &prepared.id, &manifest, &catalog).is_err()
        );
        expanded.axes.get_mut(&key).unwrap().scale /= 2.;
        catalog["coordinates"][0]["original_family_axis_count"] = json!(2);
        assert!(
            validate_space_pair(&original, &expanded, &prepared.id, &manifest, &catalog).is_err()
        );
    }
}
