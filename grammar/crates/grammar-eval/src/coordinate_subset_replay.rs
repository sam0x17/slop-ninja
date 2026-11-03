//! Replay compact coordinate metrics against maximal, hash-bound exports.
//! The frozen inference implementation supplies all scoring mathematics.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

#[path = "coordinate_inference.rs"]
#[allow(dead_code)] // Keep the frozen profile-scoring API unchanged in this replay binary.
pub mod coordinate_inference;
use coordinate_inference::{Artifact, Coordinate, Metric};

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

fn coordinate(axis: &Value) -> Result<Coordinate> {
    Ok(Coordinate {
        family_index: axis["family_index"]
            .as_u64()
            .context("family index")?
            .try_into()?,
        family: field(axis, "family")?.into(),
        feature: field(axis, "feature")?.into(),
        kind: serde_json::from_value(axis["kind"].clone())?,
        scale: axis["scale"].as_f64().context("coordinate scale")?,
        original_family_axis_count: axis["original_family_axis_count"]
            .as_u64()
            .context("family axis count")?
            .try_into()?,
        log_multiplier: 0.0,
        multiplier: 1.0,
    })
}

/// The complete catalog assigns multiplier one outside the compact model.
fn expanded_metric(metric: &Metric, catalog: &Value, indices: &[usize]) -> Result<Metric> {
    let mut artifact = metric.artifact().clone();
    artifact.coordinates = catalog["coordinates"]
        .as_array()
        .context("catalog coordinates")?
        .iter()
        .map(coordinate)
        .collect::<Result<_>>()?;
    for (column, selected) in indices.iter().zip(&metric.artifact().coordinates) {
        artifact.coordinates[*column] = selected.clone();
    }
    Metric::new(artifact)
}

fn gather(values: &[f64], width: usize, indices: &[usize]) -> Vec<f64> {
    values
        .chunks_exact(width)
        .flat_map(|row| indices.iter().map(|i| row[*i]))
        .collect()
}

fn target_row<'a>(
    authors: &'a [f64],
    held: Option<&'a [f64]>,
    query: usize,
    author: usize,
    own: usize,
    width: usize,
) -> &'a [f64] {
    if author == own
        && let Some(held) = held
    {
        &held[query * width..(query + 1) * width]
    } else {
        &authors[author * width..(author + 1) * width]
    }
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

fn comparison(
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

pub fn replay(model: &Path, export: &Path, name: &str, expected: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "use a fresh output directory");
    let model_bytes = fs::read(model)?;
    let metric = Metric::new(serde_json::from_slice::<Artifact>(&model_bytes)?)?;
    let root = export.canonicalize()?;
    let manifest_bytes = fs::read(root.join("manifest.json"))?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    let catalog_bytes = fs::read(root.join("catalog.json"))?;
    let (catalog, indices) = subset(&metric, &manifest, &catalog_bytes, name)?;
    let expanded = expanded_metric(&metric, &catalog, &indices)?;
    let query_bytes = fs::read(root.join("queries.json"))?;
    ensure!(
        manifest["queries_sha256"] == hash(&query_bytes),
        "query metadata changed"
    );
    let audit_bytes = fs::read(root.join("audit.json"))?;
    ensure!(
        manifest["audit_sha256"] == hash(&audit_bytes),
        "export audit changed"
    );
    let queries: Vec<Value> = serde_json::from_slice(&query_bytes)?;
    let authors = manifest["authors"].as_array().context("gallery authors")?;
    ensure!(
        !authors.is_empty()
            && authors.iter().all(Value::is_string)
            && authors.windows(2).all(|p| p[0].as_str() < p[1].as_str()),
        "authors duplicated or unsorted"
    );
    let (q, a, k, max_k, f) = (
        queries.len(),
        authors.len(),
        indices.len(),
        expanded.artifact().coordinates.len(),
        metric.artifact().families.len(),
    );
    ensure!(
        q > 0 && manifest["query_count"].as_u64() == Some(q as u64),
        "query count differs"
    );
    let split = field(&manifest, "split")?;
    ensure!(
        ["train", "dev", "test"].contains(&split),
        "unsupported query split"
    );
    let mut own = Vec::new();
    for query in &queries {
        let index: usize = query["own"]
            .as_u64()
            .context("true author index")?
            .try_into()?;
        ensure!(
            index < a && query["author"] == authors[index] && query["split"] == split,
            "query/gallery metadata differs"
        );
        own.push(index);
    }
    let arrays = &manifest["arrays"];
    let query_max = load_array(&root, &arrays["query_coordinates"], &[q, max_k])?;
    let author_max = load_array(&root, &arrays["author_coordinates"], &[a, max_k])?;
    let raw = load_array(&root, &arrays["family_distances"], &[q, a, f])?;
    let held_max = if split == "train" {
        Some(load_array(
            &root,
            &arrays["own_heldout_coordinates"],
            &[q, max_k],
        )?)
    } else {
        None
    };
    let query = gather(&query_max, max_k, &indices);
    let author = gather(&author_max, max_k, &indices);
    let held = held_max.as_ref().map(|v| gather(v, max_k, &indices));
    let mut logits = Vec::with_capacity(q * a);
    let mut minimum_tail = f64::INFINITY;
    let mut compact_full_error = 0.0_f64;
    for i in 0..q {
        for j in 0..a {
            let target = target_row(&author, held.as_deref(), i, j, own[i], k);
            let target_max = target_row(&author_max, held_max.as_deref(), i, j, own[i], max_k);
            let full = &raw[(i * a + j) * f..(i * a + j + 1) * f];
            let score = metric.score_pair(full, &query[i * k..(i + 1) * k], target)?;
            let maximum =
                expanded.score_pair(full, &query_max[i * max_k..(i + 1) * max_k], target_max)?;
            compact_full_error = compact_full_error.max((score.logit - maximum.logit).abs());
            for (small, large) in score
                .adjusted_family_distances
                .iter()
                .zip(&maximum.adjusted_family_distances)
            {
                compact_full_error = compact_full_error.max((small - large).abs());
            }
            minimum_tail = minimum_tail.min(score.minimum_unselected_tail);
            logits.push(score.logit);
        }
    }
    ensure!(
        compact_full_error <= TOLERANCE,
        "compact/maximal-with-unit-tail score differs: {compact_full_error}"
    );
    let expected_bytes = fs::read(expected)?;
    let error = comparison(
        &logits,
        &queries,
        authors,
        &serde_json::from_slice(&expected_bytes)?,
    )?;
    let logit_bytes: Vec<_> = logits.iter().flat_map(|v| v.to_le_bytes()).collect();
    let receipt = json!({"schema":"slopninja-subset-coordinate-replay-v1", "model_sha256":hash(&model_bytes),
        "export_manifest_sha256":hash(&manifest_bytes),"catalog_sha256":hash(&catalog_bytes),
        "subset_name":name,"subset_indices_sha256":hash(&serde_json::to_vec(&indices)?),
        "authors":authors,"query_count":q,"coordinate_count":k,"maximal_coordinate_count":max_k,
        "queries_sha256":hash(&query_bytes),"minimum_unselected_tail":minimum_tail,
        "logits":{"path":"logits.f64","shape":[q,a],"dtype":"float64-le","bytes":logit_bytes.len(),"sha256":hash(&logit_bytes)},
        "comparison":{"reference_scores_sha256":hash(&expected_bytes),"maximum_absolute_logit_error":error,"absolute_tolerance":TOLERANCE,
            "all_candidate_rankings_and_ties_identical":true,"compared_queries":q,"compared_candidate_scores":q*a,
            "compact_maximal_unit_tail_maximum_absolute_error":compact_full_error},
        "source_sha256":{"coordinate_inference.rs":hash(include_bytes!("coordinate_inference.rs")),"coordinate_subset_replay.rs":hash(include_bytes!("coordinate_subset_replay.rs")),
            "scorer.rs":hash(include_bytes!("bin/slop_ninja-score-subset.rs")),"grammar_eval_lib.rs":hash(include_bytes!("lib.rs"))},
        "executed_binary_sha256":hash(&fs::read(std::env::current_exe()?)?),"training_calls":0,"external_model_calls":0});
    fs::create_dir(out)?;
    fs::write(out.join("logits.f64"), logit_bytes)?;
    fs::write(out.join("queries.json"), query_bytes)?;
    fs::write(
        out.join("manifest.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    println!(
        "Replayed {q} queries against {a} authors, {k}/{max_k} coordinates; all rankings and ties match, maximum logit error {error:e}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use coordinate_inference::{FamilyGeometry, NumericAxis};
    use grammar_core::space::Kind;

    fn fixture() -> (Metric, Value, Vec<u8>) {
        let family = FamilyGeometry {
            name: "word".into(),
            kind: Kind::Distribution,
            original_axis_count: 3,
            numerical_axes: vec![],
        };
        let axes = vec![
            json!({"family_index":0,"family":"word","feature":"a","kind":"distribution","scale":1.0,"original_family_axis_count":3,"support_rank":1}),
            json!({"family_index":0,"family":"word","feature":"b","kind":"distribution","scale":1.0,"original_family_axis_count":3,"support_rank":1025}),
        ];
        let indices = vec![0usize];
        let catalog = json!({"schema":"slopninja-coordinate-frontier-catalog-v1","source_space_id":"fixture","feature_schema":"fixture","parser_identity":"fixture","families":["word"],"coordinates":axes,
            "subsets":{"all_m2":{"block":"all","canonical_name":"all_m2","cap_multiplier":2.0,"family_caps":{"word":1024},"max_column_indices":indices,"coordinate_count":1,"indices_sha256":hash(&serde_json::to_vec(&indices).unwrap())}}});
        let bytes = serde_json::to_vec(&catalog).unwrap();
        let mut provenance: std::collections::BTreeMap<String, String> = [
            "space",
            "selection",
            "protocol",
            "coordinate_checkpoint",
            "baseline_checkpoint",
        ]
        .into_iter()
        .map(|k| (k.into(), "0".repeat(64)))
        .collect();
        provenance.insert("catalog".into(), hash(&bytes));
        provenance.insert(
            "fixed_subset_indices".into(),
            hash(&serde_json::to_vec(&indices).unwrap()),
        );
        let mut axis = coordinate(&axes[0]).unwrap();
        axis.multiplier = 0.5;
        axis.log_multiplier = 0.5_f64.ln();
        let artifact = Artifact {
            schema: coordinate_inference::SCHEMA.into(),
            source_space_id: "fixture".into(),
            feature_schema: "fixture".into(),
            parser_identity: "fixture".into(),
            families: vec![family],
            coordinates: vec![axis],
            family_weights: vec![1.0],
            knots: vec![0.5],
            slopes: vec![1.0, 2.0],
            temperature: 0.5,
            provenance_sha256: provenance,
        };
        let manifest = json!({"schema":"slopninja-coordinate-frontier-export-v1","catalog_sha256":hash(&bytes),"source_space_id":"fixture","feature_schema":"fixture","parser_identity":"fixture","families":["word"],"coordinate_count":2,"input_bindings":{"space":"0".repeat(64)}});
        (Metric::new(artifact).unwrap(), manifest, bytes)
    }

    #[test]
    fn rejects_subset_catalog_policy_and_identity_changes() {
        let (metric, manifest, bytes) = fixture();
        assert_eq!(
            subset(&metric, &manifest, &bytes, "all_m2").unwrap().1,
            vec![0]
        );
        assert!(subset(&metric, &manifest, &bytes, "word_m2").is_err());
        let mut changed = manifest.clone();
        changed["parser_identity"] = json!("changed");
        assert!(subset(&metric, &changed, &bytes, "all_m2").is_err());
        let mut changed = manifest.clone();
        changed["catalog_sha256"] = json!("0".repeat(64));
        assert!(subset(&metric, &changed, &bytes, "all_m2").is_err());
        let mut changed: Value = serde_json::from_slice(&bytes).unwrap();
        changed["subsets"]["all_m2"]["max_column_indices"] = json!([1]);
        assert!(
            subset(
                &metric,
                &manifest,
                &serde_json::to_vec(&changed).unwrap(),
                "all_m2"
            )
            .is_err()
        );
        let mut artifact = metric.artifact().clone();
        artifact
            .provenance_sha256
            .insert("fixed_subset_indices".into(), "0".repeat(64));
        assert!(subset(&Metric::new(artifact).unwrap(), &manifest, &bytes, "all_m2").is_err());
        let mut fixed = manifest.clone();
        fixed["role"] = json!("test_gallery");
        assert!(subset(&metric, &fixed, &bytes, "all_m2").is_err());
        let mut artifact = metric.artifact().clone();
        artifact.coordinates[0].feature = "b".into();
        assert!(subset(&Metric::new(artifact).unwrap(), &manifest, &bytes, "all_m2").is_err());
    }

    #[test]
    fn compact_scores_keep_unselected_and_unseen_distribution_mass() {
        let (metric, manifest, bytes) = fixture();
        let (catalog, indices) = subset(&metric, &manifest, &bytes, "all_m2").unwrap();
        let expanded = expanded_metric(&metric, &catalog, &indices).unwrap();
        let q = [0.5_f64.sqrt(), 0.0];
        let p = [0.0, 0.25_f64.sqrt()];
        // The target's other half is an unseen category, absent even from the maximal catalog.
        let compact = metric
            .score_pair(&[1.0], &gather(&q, 2, &indices), &gather(&p, 2, &indices))
            .unwrap();
        let full = expanded.score_pair(&[1.0], &q, &p).unwrap();
        assert_eq!(compact.logit, full.logit);
        assert_eq!(compact.adjusted_family_distances, vec![0.75]);
        assert!((compact.minimum_unselected_tail - 0.5).abs() < 1e-12);
        assert!((full.minimum_unselected_tail - 0.25).abs() < 1e-12);
        assert!(metric.score_pair(&[0.1], &[q[0]], &[p[0]]).is_err());
    }

    #[test]
    fn numeric_full_denominator_remains_original_when_coordinates_are_compact() {
        let (base, _, _) = fixture();
        let mut a = base.artifact().clone();
        a.families = vec![FamilyGeometry {
            name: "numeric".into(),
            kind: Kind::Metric,
            original_axis_count: 2,
            numerical_axes: vec![
                NumericAxis {
                    feature: "a".into(),
                    scale: 2.0,
                },
                NumericAxis {
                    feature: "b".into(),
                    scale: 4.0,
                },
            ],
        }];
        a.coordinates = vec![Coordinate {
            family_index: 0,
            family: "numeric".into(),
            feature: "a".into(),
            kind: Kind::Metric,
            scale: 2.0,
            original_family_axis_count: 2,
            log_multiplier: 2.0_f64.ln(),
            multiplier: 2.0,
        }];
        let metric = Metric::new(a).unwrap();
        let profile = |a, b| {
            [(
                "numeric".into(),
                [("a".into(), a), ("b".into(), b)].into_iter().collect(),
            )]
            .into_iter()
            .collect()
        };
        let score = metric
            .score_profiles(&profile(2.0, 4.0), &profile(0.0, 0.0))
            .unwrap();
        assert!((score.adjusted_family_distances[0] - 1.5).abs() < 1e-12);
        assert!((score.minimum_unselected_tail - 0.5).abs() < 1e-12);
    }

    #[test]
    fn whole_date_target_only_replaces_the_true_training_author() {
        let authors = [1.0, 2.0, 3.0, 4.0];
        let held = [5.0, 6.0, 7.0, 8.0];
        assert_eq!(target_row(&authors, Some(&held), 1, 0, 0, 2), &[7.0, 8.0]);
        assert_eq!(target_row(&authors, Some(&held), 1, 1, 0, 2), &[3.0, 4.0]);
        assert_eq!(target_row(&authors, None, 1, 0, 0, 2), &[1.0, 2.0]);
        let columns = [1];
        assert_eq!(gather(&authors, 2, &columns), vec![2.0, 4.0]);
        assert_eq!(gather(&held, 2, &columns), vec![6.0, 8.0]);
    }

    #[test]
    fn rehashed_catalog_cannot_silently_change_the_named_support_rank_policy() {
        let (metric, mut manifest, bytes) = fixture();
        let mut catalog: Value = serde_json::from_slice(&bytes).unwrap();
        catalog["coordinates"][1]["support_rank"] = json!(1024);
        let bytes = serde_json::to_vec(&catalog).unwrap();
        let mut artifact = metric.artifact().clone();
        artifact
            .provenance_sha256
            .insert("catalog".into(), hash(&bytes));
        manifest["catalog_sha256"] = json!(hash(&bytes));
        assert!(
            subset(&Metric::new(artifact).unwrap(), &manifest, &bytes, "all_m2")
                .unwrap_err()
                .to_string()
                .contains("support ranks")
        );
    }

    #[test]
    fn reference_comparison_rejects_tiny_tie_and_order_changes() {
        let q = vec![json!({"id":"q"})];
        let a = vec![json!("a"), json!("b")];
        let expected = json!({"query_ids":["q"],"candidate_authors":a,"logits":[[1.0,1.0]]});
        assert_eq!(comparison(&[1.0, 1.0], &q, &a, &expected).unwrap(), 0.0);
        assert!(comparison(&[1.0, 1.0 - 1e-12], &q, &a, &expected).is_err());
        let expected = json!({"query_ids":["other"],"candidate_authors":a,"logits":[[1.0,1.0]]});
        assert!(comparison(&[1.0, 1.0], &q, &a, &expected).is_err());
    }

    #[test]
    fn array_reader_rejects_shape_hash_nonfinite_and_escape() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let bytes = 1.0_f64.to_le_bytes();
        fs::write(root.join("a.f64"), bytes).unwrap();
        let binding = json!({"path":"a.f64","shape":[1],"dtype":"float64-le","bytes":8,"sha256":hash(&bytes)});
        assert_eq!(load_array(&root, &binding, &[1]).unwrap(), vec![1.0]);
        assert!(load_array(&root, &binding, &[1, 1]).is_err());
        let mut bad = binding.clone();
        bad["sha256"] = json!("0".repeat(64));
        assert!(load_array(&root, &bad, &[1]).is_err());
        let mut bad = binding.clone();
        bad["path"] = json!("../");
        assert!(load_array(&root, &bad, &[1]).is_err());
        let bytes = f64::NAN.to_le_bytes();
        fs::write(root.join("a.f64"), bytes).unwrap();
        let mut bad = binding;
        bad["sha256"] = json!(hash(&bytes));
        assert!(load_array(&root, &bad, &[1]).is_err());
    }
}
