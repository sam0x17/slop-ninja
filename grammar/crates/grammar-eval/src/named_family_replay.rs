//! Bound tensor loading and complete ordering/tie replay for named family metrics.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path},
};
#[path = "named_family_inference.rs"]
pub mod inference;
use inference::{Artifact, Metric, valid_hash};

const TOLERANCE: f64 = 1e-10;
const BASELINE: [&str; 14] = [
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
const NEW_FAMILY: &str = "clause_child_backoff_v1";
const NEW_SCHEMA: &str = "grammar-features-v2;nfc-lowercase-letter-apostrophe-v1;alphabetic-parse-token-v1;syntax-families-v1;clause-child-backoff-v1";

pub struct Request<'a> {
    pub model: &'a Path,
    pub model_sha256: &'a str,
    pub export: &'a Path,
    pub manifest_sha256: &'a str,
    pub reference_space: &'a Path,
    pub expected_scores: Option<(&'a Path, &'a str)>,
    pub out: &'a Path,
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing string {key}"))
}
fn read_bound(path: &Path, digest: &str) -> Result<Vec<u8>> {
    ensure!(valid_hash(digest), "malformed expected digest");
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    ensure!(hash(&bytes) == digest, "hash differs: {}", path.display());
    Ok(bytes)
}
fn write_fresh(path: &Path, bytes: &[u8]) -> Result<String> {
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?
        .write_all(bytes)?;
    Ok(hash(bytes))
}
fn write_json(path: &Path, value: &Value) -> Result<String> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_fresh(path, &bytes)
}
fn array(root: &Path, binding: &Value, shape: &[usize]) -> Result<Vec<u8>> {
    ensure!(
        binding["shape"] == json!(shape) && binding["dtype"] == "float64-le",
        "array shape/dtype differs"
    );
    let relative = Path::new(field(binding, "path")?);
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "array path escapes export"
    );
    let path = root.join(relative).canonicalize()?;
    ensure!(
        path.starts_with(root.canonicalize()?),
        "array symlink escapes export"
    );
    let bytes = read_bound(&path, field(binding, "sha256")?)?;
    let count = shape.iter().try_fold(1usize, |n, d| {
        n.checked_mul(*d).context("array shape overflow")
    })?;
    ensure!(
        bytes.len() == count.checked_mul(8).context("array byte count overflow")?
            && binding["bytes"].as_u64() == Some(bytes.len() as u64),
        "array byte count differs"
    );
    ensure!(
        bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|b| f64::from_le_bytes(*b))
            .all(|v| v.is_finite() && v >= 0.),
        "invalid family distance"
    );
    Ok(bytes)
}

fn family_indices(metric: &Metric, manifest: &Value) -> Result<Vec<usize>> {
    let model = metric.artifact();
    ensure!(
        manifest["schema"] == "slopninja-named-family-tensor-v1",
        "unsupported tensor schema"
    );
    let baseline: Vec<String> = serde_json::from_value(manifest["baseline_families"].clone())?;
    let families: Vec<String> = serde_json::from_value(manifest["families"].clone())?;
    let mut expected: Vec<_> = BASELINE.iter().map(|name| name.to_string()).collect();
    expected.push(NEW_FAMILY.into());
    expected.sort();
    ensure!(
        baseline == BASELINE && families == expected && manifest["new_family"] == NEW_FAMILY,
        "named tensor family contract differs"
    );
    let indices: Vec<_> = baseline
        .iter()
        .map(|name| families.iter().position(|x| x == name).unwrap())
        .collect();
    ensure!(
        manifest["baseline_family_indices"] == json!(indices),
        "baseline named mapping differs"
    );
    ensure!(
        manifest["legacy_feature_schema"] == grammar_core::features::FEATURE_VERSION
            && manifest["feature_schema"] == NEW_SCHEMA,
        "tensor feature schema differs"
    );
    ensure!(
        manifest["source_space_id"] == model.source_space_id
            && manifest["reference_space_sha256"] == model.reference_space_sha256
            && manifest["parser_identity"] == model.parser_identity
            && manifest["protocol_sha256"] == model.provenance_sha256["protocol"],
        "tensor/model provenance differs"
    );
    if model.families == baseline {
        ensure!(
            model.feature_schema == field(manifest, "legacy_feature_schema")?,
            "baseline model feature schema differs"
        );
        Ok(indices)
    } else {
        ensure!(
            model.families == families
                && model.feature_schema == field(manifest, "feature_schema")?,
            "active model families/schema differ"
        );
        Ok((0..families.len()).collect())
    }
}

#[derive(Deserialize)]
struct ReferenceIdentity {
    id: String,
    geometry: String,
    feature_schema: String,
    parser_identity: String,
    family_schemas: BTreeMap<String, Value>,
}
fn validate_reference(metric: &Metric, bytes: &[u8]) -> Result<()> {
    let reference: ReferenceIdentity = serde_json::from_slice(bytes)?;
    let model = metric.artifact();
    ensure!(
        hash(bytes) == model.reference_space_sha256
            && reference.id == model.source_space_id
            && reference.geometry == "unslop-hellinger-euclidean-v2"
            && reference.feature_schema == grammar_core::features::FEATURE_VERSION
            && reference.parser_identity == model.parser_identity
            && reference
                .family_schemas
                .keys()
                .map(String::as_str)
                .eq(BASELINE),
        "original reference geometry/identity differs"
    );
    Ok(())
}

fn validate_queries(rows: &[Value], authors: &[String], manifest: &Value) -> Result<()> {
    ensure!(
        !rows.is_empty()
            && !authors.is_empty()
            && authors.windows(2).all(|p| p[0] < p[1])
            && manifest["query_count"].as_u64() == Some(rows.len() as u64),
        "empty/unsorted tensor support"
    );
    let split = field(manifest, "split")?;
    ensure!(
        ["train", "dev", "test"].contains(&split),
        "unknown query split"
    );
    let mut ids = BTreeSet::new();
    let mut coverage = BTreeSet::new();
    for row in rows {
        let id = field(row, "id")?;
        let author = field(row, "author")?;
        let date = field(row, "date")?;
        let own = row["own"]
            .as_u64()
            .context("own index must be a nonnegative integer")? as usize;
        ensure!(
            !id.is_empty()
                && ids.insert(id)
                && authors.get(own).map(String::as_str) == Some(author)
                && row["split"] == split,
            "query label/order/split differs"
        );
        grammar_eval::validate_date(date)?;
        ensure!(
            !field(row, "source_group")?.is_empty()
                && valid_hash(field(row, "text_sha256")?)
                && row["loss_weight"]
                    .as_f64()
                    .is_some_and(|w| w.is_finite() && w > 0.),
            "query source/weight metadata invalid"
        );
        coverage.insert(author);
    }
    ensure!(
        coverage == authors.iter().map(String::as_str).collect(),
        "query author coverage differs"
    );
    Ok(())
}

fn baseline_exact(full: &[u8], baseline: &[u8], families: usize, indices: &[usize]) -> Result<()> {
    ensure!(
        families > 0
            && !indices.is_empty()
            && indices.windows(2).all(|p| p[0] < p[1])
            && indices.iter().all(|i| *i < families)
            && full.len().is_multiple_of(families * 8)
            && baseline.len().is_multiple_of(indices.len() * 8)
            && baseline.len() / 8 == full.len() / 8 / families * indices.len(),
        "baseline tensor shapes/mapping differ"
    );
    for (row, old) in full
        .chunks_exact(families * 8)
        .zip(baseline.chunks_exact(indices.len() * 8))
    {
        for (i, column) in indices.iter().enumerate() {
            ensure!(
                row[column * 8..(column + 1) * 8] == old[i * 8..(i + 1) * 8],
                "original14 mapped distance bytes differ"
            );
        }
    }
    Ok(())
}
fn ordering(scores: &[f64]) -> Vec<usize> {
    let mut indices: Vec<_> = (0..scores.len()).collect();
    indices.sort_by(|a, b| {
        scores[*b]
            .partial_cmp(&scores[*a])
            .unwrap()
            .then_with(|| a.cmp(b))
    });
    indices
}

/// Record every changed candidate position, exact pairwise tie and score above
/// tolerance. All rows are visited even if an earlier comparison fails.
fn compare(scores: &[f64], rows: &[Value], authors: &[String], expected: &Value) -> Result<Value> {
    ensure!(
        expected["query_ids"] == json!(rows.iter().map(|q| &q["id"]).collect::<Vec<_>>())
            && expected["candidate_authors"] == json!(authors),
        "reference query/candidate ordering differs"
    );
    let source: Vec<Vec<f64>> = serde_json::from_value(expected["logits"].clone())?;
    ensure!(
        source.len() == rows.len()
            && source
                .iter()
                .all(|r| r.len() == authors.len() && r.iter().all(|v| v.is_finite()))
            && scores.len() == rows.len() * authors.len()
            && scores.iter().all(|v| v.is_finite()),
        "reference/actual score shape or finiteness differs"
    );
    let mut orders = vec![];
    let mut ties = vec![];
    let mut discrepancies = vec![];
    let mut max_absolute = 0.0f64;
    let mut max_scaled = 0.0f64;
    for (q, (actual, expected)) in scores.chunks_exact(authors.len()).zip(&source).enumerate() {
        let actual_order = ordering(actual);
        let expected_order = ordering(expected);
        for (rank, (a, b)) in actual_order.iter().zip(&expected_order).enumerate() {
            if a != b {
                orders.push(json!({"query_id":rows[q]["id"],"rank":rank,"actual_author":authors[*a],"expected_author":authors[*b]}));
            }
        }
        for i in 0..authors.len() {
            let error = (actual[i] - expected[i]).abs();
            let scaled = error / (1. + expected[i].abs());
            max_absolute = max_absolute.max(error);
            max_scaled = max_scaled.max(scaled);
            if scaled > TOLERANCE {
                discrepancies.push(json!({"query_id":rows[q]["id"],"author":authors[i],"actual":actual[i],"expected":expected[i],"absolute_error":error,"scaled_error":scaled}));
            }
            for j in i + 1..authors.len() {
                if (actual[i] == actual[j]) != (expected[i] == expected[j]) {
                    ties.push(json!({"query_id":rows[q]["id"],"left_author":authors[i],"right_author":authors[j],"actual_tie":actual[i]==actual[j],"expected_tie":expected[i]==expected[j]}));
                }
            }
        }
    }
    Ok(
        json!({"passed":orders.is_empty()&&ties.is_empty()&&discrepancies.is_empty(),"scores":scores.len(),"rankings":rows.len(),"pairwise_tie_comparisons":rows.len()*authors.len()*(authors.len()-1)/2,"max_absolute_error":max_absolute,"max_scaled_error":max_scaled,"scaled_tolerance":TOLERANCE,"tolerance_rule":"absolute_error <= tolerance * (1 + abs(expected)); ordering and equality ties checked exactly","ordering_discrepancies":orders,"tie_discrepancies":ties,"score_discrepancies":discrepancies}),
    )
}

pub fn replay(request: Request<'_>) -> Result<()> {
    ensure!(!request.out.exists(), "output must be a fresh directory");
    let model_bytes = read_bound(request.model, request.model_sha256)?;
    let metric = Metric::new(serde_json::from_slice::<Artifact>(&model_bytes)?)?;
    let manifest: Value = serde_json::from_slice(&read_bound(
        &request.export.join("manifest.json"),
        request.manifest_sha256,
    )?)?;
    let indices = family_indices(&metric, &manifest)?;
    validate_reference(
        &metric,
        &read_bound(
            request.reference_space,
            &metric.artifact().reference_space_sha256,
        )?,
    )?;
    let query_bytes = read_bound(
        &request.export.join("queries.json"),
        field(&manifest, "queries_sha256")?,
    )?;
    let rows: Vec<Value> = serde_json::from_slice(&query_bytes)?;
    let authors: Vec<String> = serde_json::from_value(manifest["authors"].clone())?;
    validate_queries(&rows, &authors, &manifest)?;
    for (name, key) in [
        ("audit.json", "audit_sha256"),
        ("targets.json", "targets_sha256"),
    ] {
        read_bound(&request.export.join(name), field(&manifest, key)?)?;
    }
    let full = array(
        request.export,
        &manifest["arrays"]["family_distances"],
        &[rows.len(), authors.len(), 15],
    )?;
    let baseline = array(
        request.export,
        &manifest["arrays"]["baseline_family_distances"],
        &[rows.len(), authors.len(), 14],
    )?;
    ensure!(
        manifest["baseline_source_array_sha256"]
            == manifest["arrays"]["baseline_family_distances"]["sha256"],
        "baseline original byte binding differs"
    );
    let baseline_indices: Vec<usize> =
        serde_json::from_value(manifest["baseline_family_indices"].clone())?;
    baseline_exact(&full, &baseline, 15, &baseline_indices)?;
    let mut scores = Vec::with_capacity(rows.len() * authors.len());
    for row in full.as_chunks::<{ 15 * 8 }>().0 {
        let distances: Vec<_> = indices
            .iter()
            .map(|i| f64::from_le_bytes(row[i * 8..(i + 1) * 8].try_into().unwrap()))
            .collect();
        scores.push(metric.score_distances(&distances)?);
    }
    let comparison = if let Some((path, sha)) = request.expected_scores {
        let expected: Value = serde_json::from_slice(&read_bound(path, sha)?)?;
        Some(compare(&scores, &rows, &authors, &expected)?)
    } else {
        None
    };
    let bytes: Vec<_> = scores.iter().flat_map(|v| v.to_le_bytes()).collect();
    fs::create_dir(request.out)?;
    write_fresh(&request.out.join("logits.f64"), &bytes)?;
    write_fresh(&request.out.join("queries.json"), &query_bytes)?;
    let result = json!({"schema":"slopninja-named-family-replay-v1","model_sha256":request.model_sha256,"export_manifest_sha256":request.manifest_sha256,
        "source_space_id":metric.artifact().source_space_id,"reference_space_sha256":metric.artifact().reference_space_sha256,"feature_schema":metric.artifact().feature_schema,"parser_identity":metric.artifact().parser_identity,"families":metric.artifact().families,"selected_family_indices":indices,
        "split":manifest["split"],"authors":authors,"query_count":rows.len(),"queries_sha256":hash(&query_bytes),"arrays":{"logits":{"path":"logits.f64","dtype":"float64-le","shape":[rows.len(),authors.len()],"bytes":bytes.len(),"sha256":hash(&bytes)}},
        "reference_comparison":comparison,"expected_scores_sha256":request.expected_scores.map(|(_,sha)|sha),"baseline_bytes_verified":true,
        "input_bindings":{"model":request.model,"export":request.export,"reference_space":request.reference_space,"expected_scores":request.expected_scores.map(|(p,_)|p),"model_provenance_sha256":metric.artifact().provenance_sha256,"source_tensor_arrays":manifest["arrays"],"audit_sha256":manifest["audit_sha256"],"targets_sha256":manifest["targets_sha256"]},
        "source_sha256":{"named_family_inference.rs":hash(include_bytes!("named_family_inference.rs")),"named_family_replay.rs":hash(include_bytes!("named_family_replay.rs")),"bin/slop_ninja-score-families.rs":hash(include_bytes!("bin/slop_ninja-score-families.rs")),"grammar-eval/lib.rs":hash(include_bytes!("lib.rs"))},"executed_binary_sha256":hash(&fs::read(std::env::current_exe()?)?),"training_calls":0,"external_model_calls":0,"detector_calls":0});
    let digest = write_json(&request.out.join("manifest.json"), &result)?;
    println!(
        "{}",
        serde_json::to_string(
            &json!({"manifest_sha256":digest,"scores":scores.len(),"rankings":rows.len(),"reference_comparison":comparison})
        )?
    );
    ensure!(
        comparison.as_ref().is_none_or(|c| c["passed"] == true),
        "reference replay failed; all score/order/tie discrepancies are recorded in manifest.json"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_tie_and_order_discrepancy_is_reported_even_below_numeric_tolerance() {
        let rows = vec![json!({"id":"q"})];
        let authors = vec!["a".into(), "b".into(), "c".into()];
        let expected = json!({"query_ids":["q"],"candidate_authors":authors,"logits":[[1.,1.,0.]]});
        let report = compare(&[1., 1. + 1e-12, 0.], &rows, &authors, &expected).unwrap();
        assert_eq!(report["passed"], false);
        assert_eq!(report["score_discrepancies"].as_array().unwrap().len(), 0);
        assert_eq!(
            report["ordering_discrepancies"].as_array().unwrap().len(),
            2
        );
        assert_eq!(report["tie_discrepancies"].as_array().unwrap().len(), 1);
        let exact = compare(&[1., 1., 0.], &rows, &authors, &expected).unwrap();
        assert_eq!(exact["passed"], true);
        assert_eq!(ordering(&[0., -0., -1.]), vec![0, 1, 2]);
    }
    #[test]
    fn baseline_comparison_preserves_zero_bits_and_rejects_changed_columns() {
        let bytes: Vec<_> = [0.0f64, -0., 3., 4.]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        baseline_exact(&bytes, &bytes, 2, &[0, 1]).unwrap();
        let changed: Vec<_> = [0.0f64, 0., 3., 4.]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert!(baseline_exact(&bytes, &changed, 2, &[0, 1]).is_err());
        assert!(baseline_exact(&bytes, &bytes, 2, &[1, 0]).is_err());
    }
    fn manifest() -> Value {
        let mut families: Vec<_> = BASELINE.iter().map(|s| s.to_string()).collect();
        families.push(NEW_FAMILY.into());
        families.sort();
        let indices: Vec<_> = BASELINE
            .iter()
            .map(|name| families.iter().position(|x| x == name).unwrap())
            .collect();
        json!({"schema":"slopninja-named-family-tensor-v1","families":families,"baseline_families":BASELINE,"new_family":NEW_FAMILY,"baseline_family_indices":indices,"legacy_feature_schema":grammar_core::features::FEATURE_VERSION,"feature_schema":NEW_SCHEMA,"source_space_id":"a".repeat(64),"reference_space_sha256":"b".repeat(64),"parser_identity":"synthetic-parser-v1","protocol_sha256":"c".repeat(64)})
    }
    #[test]
    fn exact_named_projection_requires_model_schema_parser_and_reference_binding() {
        let mut artifact = inference::tests::artifact();
        artifact.families = BASELINE.iter().map(|x| x.to_string()).collect();
        artifact.family_weights = vec![1. / 14.; 14];
        artifact.feature_schema = grammar_core::features::FEATURE_VERSION.into();
        let metric = Metric::new(artifact.clone()).unwrap();
        let mut m = manifest();
        assert_eq!(
            family_indices(&metric, &m).unwrap(),
            (1..15).collect::<Vec<_>>()
        );
        for key in [
            "source_space_id",
            "reference_space_sha256",
            "parser_identity",
            "legacy_feature_schema",
            "protocol_sha256",
        ] {
            let mut bad = m.clone();
            bad[key] = json!("wrong");
            assert!(family_indices(&metric, &bad).is_err());
        }
        m["baseline_family_indices"][0] = json!(0);
        assert!(family_indices(&metric, &m).is_err());
        let m = manifest();
        artifact.families = serde_json::from_value(m["families"].clone()).unwrap();
        artifact.family_weights = vec![1. / 15.; 15];
        artifact.feature_schema = NEW_SCHEMA.into();
        assert_eq!(
            family_indices(&Metric::new(artifact).unwrap(), &m).unwrap(),
            (0..15).collect::<Vec<_>>()
        );
    }
    #[test]
    fn query_own_indices_and_reference_order_must_align() {
        let authors = vec!["a".into()];
        let manifest = json!({"split":"test","query_count":1});
        let mut rows = vec![
            json!({"id":"q","author":"a","date":"2004-01-02","source_group":"a:date:2004-01-02","own":0,"split":"test","text_sha256":"a".repeat(64),"loss_weight":1.}),
        ];
        validate_queries(&rows, &authors, &manifest).unwrap();
        rows[0]["own"] = json!(1);
        assert!(validate_queries(&rows, &authors, &manifest).is_err());
        let expected = json!({"query_ids":["wrong"],"candidate_authors":authors,"logits":[[0.]]});
        assert!(compare(&[0.], &rows, &authors, &expected).is_err());
    }
}
