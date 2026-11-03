//! Hash-bound input loading copied from the frozen coordinate subset replay.
//! Changes are confined to this new module; only existing TEST exports are accepted.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
#[path = "coordinate_inference.rs"]
#[allow(dead_code)]
pub mod coordinate_inference;
pub use coordinate_inference::{Artifact, Metric};
pub const TOLERANCE: f64 = 1e-10;
pub fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing {key}"))
}

fn load_array(root: &Path, binding: &Value, shape: &[usize]) -> Result<Vec<f64>> {
    let relative = Path::new(field(binding, "path")?);
    ensure!(!relative.is_absolute(), "absolute array path");
    let path = root.join(relative).canonicalize()?;
    ensure!(path.starts_with(root), "array path escapes export");
    ensure!(
        binding["dtype"] == "float64-le" && binding["shape"] == json!(shape),
        "array dtype/shape differs"
    );
    let count = shape
        .iter()
        .try_fold(1usize, |n, d| n.checked_mul(*d).context("shape overflow"))?;
    let bytes = fs::read(path)?;
    ensure!(
        bytes.len() == count.checked_mul(8).context("byte count overflow")?
            && binding["bytes"].as_u64() == Some(bytes.len() as u64)
            && binding["sha256"] == hash(&bytes),
        "array hash/size differs"
    );
    let values: Vec<_> = bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| f64::from_le_bytes(*b))
        .collect();
    ensure!(values.iter().all(|v| v.is_finite()), "nonfinite array");
    Ok(values)
}

/// Bind the named subset to both the immutable maximal catalog and compact model.
fn subset(
    metric: &Metric,
    manifest: &Value,
    bytes: &[u8],
    name: &str,
) -> Result<(Value, Vec<usize>)> {
    ensure!(
        name == "all_m2",
        "this replay supports the frozen all_m2 subset"
    );
    let artifact = metric.artifact();
    let digest = hash(bytes);
    ensure!(
        manifest["schema"] == "slopninja-coordinate-frontier-export-v1",
        "unsupported export schema"
    );
    ensure!(
        manifest["catalog_sha256"] == digest
            && artifact.provenance_sha256.get("catalog") == Some(&digest),
        "model/catalog binding differs"
    );
    let catalog: Value = serde_json::from_slice(bytes)?;
    ensure!(
        catalog["schema"] == "slopninja-coordinate-frontier-catalog-v1",
        "unsupported catalog schema"
    );
    for (key, expected) in [
        ("source_space_id", &artifact.source_space_id),
        ("feature_schema", &artifact.feature_schema),
        ("parser_identity", &artifact.parser_identity),
    ] {
        ensure!(
            manifest[key] == *expected && catalog[key] == *expected,
            "export/catalog/model identity differs: {key}"
        );
    }
    let families: Vec<_> = artifact.families.iter().map(|f| f.name.as_str()).collect();
    ensure!(
        manifest["families"] == json!(families) && catalog["families"] == json!(families),
        "family order differs"
    );
    ensure!(
        manifest["input_bindings"]
            .as_object()
            .context("input bindings")?
            .values()
            .any(|v| v.as_str() == artifact.provenance_sha256.get("space").map(String::as_str)),
        "export lacks original-space binding"
    );
    let selected = &catalog["subsets"][name];
    ensure!(
        selected["block"] == "all"
            && selected["canonical_name"] == name
            && selected["cap_multiplier"] == 2.0,
        "named subset policy differs"
    );
    let indices: Vec<usize> = serde_json::from_value(selected["max_column_indices"].clone())?;
    let index_hash = hash(&serde_json::to_vec(&indices)?);
    ensure!(
        !indices.is_empty()
            && indices.windows(2).all(|p| p[0] < p[1])
            && selected["indices_sha256"] == index_hash
            && artifact.provenance_sha256.get("fixed_subset_indices") == Some(&index_hash),
        "subset indices/order/binding differs"
    );
    let coordinates = catalog["coordinates"]
        .as_array()
        .context("catalog coordinates")?;
    ensure!(
        manifest["coordinate_count"].as_u64() == Some(coordinates.len() as u64)
            && selected["coordinate_count"].as_u64() == Some(indices.len() as u64)
            && indices.len() == artifact.coordinates.len(),
        "coordinate catalog/subset size differs"
    );
    let mut expected_indices = Vec::new();
    for (i, axis) in coordinates.iter().enumerate() {
        let family = field(axis, "family")?;
        let selected_axis = if axis["kind"] == "distribution" {
            let cap = if ["word", "word_bigram"].contains(&family) {
                1024
            } else {
                192
            };
            ensure!(
                selected["family_caps"][family].as_u64() == Some(cap),
                "subset family cap differs"
            );
            let rank = axis["support_rank"]
                .as_u64()
                .context("categorical support rank")?;
            ensure!(rank > 0, "invalid support rank");
            rank <= cap
        } else {
            true
        };
        if selected_axis {
            expected_indices.push(i);
        }
    }
    ensure!(
        indices == expected_indices,
        "subset differs from frozen support ranks"
    );
    for (column, portable) in indices.iter().zip(&artifact.coordinates) {
        let old = coordinates
            .get(*column)
            .context("subset column out of bounds")?;
        let portable = serde_json::to_value(portable)?;
        for key in [
            "family_index",
            "family",
            "feature",
            "kind",
            "scale",
            "original_family_axis_count",
        ] {
            ensure!(
                old[key] == portable[key],
                "coordinate geometry differs: {key}"
            );
        }
    }
    // Additional fixed-panel fields are required together when a role is declared.
    if manifest.get("role").is_some() {
        let role = if manifest["split"] == "train" {
            "training_panel"
        } else {
            "test_gallery"
        };
        ensure!(
            manifest["role"] == role
                && manifest["training_catalog_policy"] == "apply_frozen_catalog_without_fitting"
                && manifest["catalog_fit_performed"] == false
                && manifest["fixed_subset_name"] == name
                && manifest["fixed_subset"] == *selected,
            "fixed-panel catalog policy differs"
        );
    }
    Ok((catalog, indices))
}
fn gather(values: &[f64], width: usize, indices: &[usize]) -> Vec<f64> {
    values
        .chunks_exact(width)
        .flat_map(|row| indices.iter().map(|i| row[*i]))
        .collect()
}
fn ordering(scores: &[f64]) -> (Vec<usize>, Vec<bool>) {
    let mut order: Vec<_> = (0..scores.len()).collect();
    order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]).then_with(|| a.cmp(b)));
    let ties = order
        .windows(2)
        .map(|pair| scores[pair[0]] == scores[pair[1]])
        .collect();
    (order, ties)
}

pub fn compare_reference(
    logits: &[f64],
    queries: &[Value],
    authors: &[Value],
    expected: &Value,
) -> Result<f64> {
    let ids: Vec<_> = queries.iter().map(|q| q["id"].clone()).collect();
    ensure!(
        expected["query_ids"] == json!(ids) && expected["candidate_authors"] == json!(authors),
        "reference query/candidate order changed"
    );
    let rows: Vec<Vec<f64>> = serde_json::from_value(expected["logits"].clone())?;
    ensure!(
        rows.len() == queries.len()
            && rows
                .iter()
                .all(|r| r.len() == authors.len() && r.iter().all(|v| v.is_finite())),
        "reference score shape/values differ"
    );
    let mut error = 0.0_f64;
    for (i, (actual, reference)) in logits.chunks_exact(authors.len()).zip(&rows).enumerate() {
        ensure!(
            ordering(actual) == ordering(reference),
            "candidate ordering or ties changed for query {i}"
        );
        for (a, b) in actual.iter().zip(reference) {
            error = error.max((a - b).abs());
        }
    }
    ensure!(
        error <= TOLERANCE,
        "Python/Rust logit error exceeds tolerance: {error}"
    );
    Ok(error)
}

pub struct Loaded {
    pub manifest: Value,
    pub queries: Vec<Value>,
    pub authors: Vec<Value>,
    pub own: Vec<usize>,
    pub query: Vec<f64>,
    pub target: Vec<f64>,
    pub raw: Vec<f64>,
}

pub fn load(metric: &Metric, export: &Path, expected_manifest_sha256: &str) -> Result<Loaded> {
    let root = export.canonicalize()?;
    let manifest_bytes = fs::read(root.join("manifest.json"))?;
    ensure!(
        hash(&manifest_bytes) == expected_manifest_sha256,
        "export manifest changed"
    );
    let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    let catalog_bytes = fs::read(root.join("catalog.json"))?;
    let (catalog, indices) = subset(metric, &manifest, &catalog_bytes, "all_m2")?;
    ensure!(
        manifest["split"] == "test",
        "full-family ablation requires test exports"
    );
    ensure!(
        indices.len() == 3557
            && catalog["coordinates"]
                .as_array()
                .context("coordinates")?
                .len()
                == 7353
            && metric.artifact().families.len() == 14,
        "frozen dimensions differ"
    );
    let query_bytes = fs::read(root.join("queries.json"))?;
    ensure!(
        manifest["queries_sha256"] == hash(&query_bytes),
        "query metadata changed"
    );
    ensure!(
        manifest["audit_sha256"] == hash(&fs::read(root.join("audit.json"))?),
        "export audit changed"
    );
    let queries: Vec<Value> = serde_json::from_slice(&query_bytes)?;
    let authors = manifest["authors"].as_array().context("authors")?.clone();
    ensure!(
        authors.len() == 100
            && authors.iter().all(Value::is_string)
            && authors.windows(2).all(|p| p[0].as_str() < p[1].as_str()),
        "author gallery differs"
    );
    let q = queries.len();
    ensure!(
        q > 0 && manifest["query_count"].as_u64() == Some(q as u64),
        "query count differs"
    );
    let mut own = Vec::new();
    for query in &queries {
        let index: usize = query["own"].as_u64().context("own index")?.try_into()?;
        ensure!(
            index < authors.len() && query["author"] == authors[index] && query["split"] == "test",
            "query own/split differs"
        );
        own.push(index);
    }
    let arrays = &manifest["arrays"];
    let query_max = load_array(&root, &arrays["query_coordinates"], &[q, 7353])?;
    let author_max = load_array(&root, &arrays["author_coordinates"], &[authors.len(), 7353])?;
    let raw = load_array(&root, &arrays["family_distances"], &[q, authors.len(), 14])?;
    ensure!(raw.iter().all(|d| *d >= 0.0), "negative raw distance");
    Ok(Loaded {
        manifest,
        queries,
        authors,
        own,
        query: gather(&query_max, 7353, &indices),
        target: gather(&author_max, 7353, &indices),
        raw,
    })
}
