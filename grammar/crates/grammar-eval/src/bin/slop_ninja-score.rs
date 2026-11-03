use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use grammar_eval::Profile;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[path = "../coordinate_inference.rs"]
mod coordinate_inference;
use coordinate_inference::{Artifact, Metric};

#[derive(Parser)]
#[command(about = "Score writing profiles with a portable positive coordinate metric")]
struct Args {
    #[arg(long)]
    model: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Score an existing hash-bound coordinate export, optionally checking saved logits.
    Export {
        #[arg(long)]
        export: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        expected_scores: Option<PathBuf>,
    },
    /// Score two identity-bound JSON profile envelopes containing raw probabilities/measurements.
    Profiles {
        #[arg(long)]
        query: PathBuf,
        #[arg(long)]
        target: PathBuf,
    },
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(
        &fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )?)
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing {key}"))
}
fn file(root: &Path, relative: &str) -> Result<PathBuf> {
    ensure!(!Path::new(relative).is_absolute(), "absolute array path");
    let path = root.join(relative).canonicalize()?;
    ensure!(path.starts_with(root), "array path escapes export");
    Ok(path)
}
fn load_array(root: &Path, binding: &Value, shape: &[usize]) -> Result<Vec<f64>> {
    ensure!(
        binding["dtype"] == "float64-le" && binding["shape"] == json!(shape),
        "array dtype/shape differs"
    );
    let bytes = fs::read(file(root, string(binding, "path")?)?)?;
    let count = shape.iter().try_fold(1usize, |total, n| {
        total.checked_mul(*n).context("shape overflow")
    })?;
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
fn ordering(scores: &[f64]) -> (Vec<usize>, Vec<bool>) {
    let mut order: Vec<_> = (0..scores.len()).collect();
    order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]).then_with(|| a.cmp(b)));
    let ties = order
        .windows(2)
        .map(|pair| scores[pair[0]] == scores[pair[1]])
        .collect();
    (order, ties)
}

fn bound_profile(path: &Path, metric: &Metric) -> Result<Profile> {
    let value = read(path)?;
    ensure!(
        value["schema"] == "slopninja-coordinate-profile-v1",
        "unsupported profile envelope"
    );
    let artifact = metric.artifact();
    for (key, expected) in [
        ("source_space_id", &artifact.source_space_id),
        ("feature_schema", &artifact.feature_schema),
        ("parser_identity", &artifact.parser_identity),
    ] {
        ensure!(value[key] == *expected, "profile identity differs: {key}");
    }
    Ok(serde_json::from_value(value["profile"].clone())?)
}

fn score_export(
    metric: &Metric,
    model_path: &Path,
    export: &Path,
    out: &Path,
    expected: Option<&Path>,
) -> Result<()> {
    ensure!(!out.exists(), "use a fresh output directory");
    let root = export.canonicalize()?;
    let manifest_path = root.join("manifest.json");
    let manifest = read(&manifest_path)?;
    ensure!(
        manifest["schema"] == "slopninja-coordinate-export-v1",
        "unsupported export schema"
    );
    let artifact = metric.artifact();
    ensure!(
        manifest["input_bindings"]
            .as_object()
            .context("export input bindings")?
            .values()
            .any(|value| value.as_str()
                == artifact.provenance_sha256.get("space").map(String::as_str)),
        "export lacks the model's original-space binding"
    );
    for (key, value) in [
        ("source_space_id", &artifact.source_space_id),
        ("feature_schema", &artifact.feature_schema),
        ("parser_identity", &artifact.parser_identity),
    ] {
        ensure!(
            manifest[key] == *value,
            "export/model identity differs: {key}"
        );
    }
    let names: Vec<_> = artifact.families.iter().map(|f| f.name.as_str()).collect();
    ensure!(manifest["families"] == json!(names), "family order differs");
    let catalog_bytes = fs::read(root.join("catalog.json"))?;
    let catalog_hash = hash(&catalog_bytes);
    ensure!(
        manifest["catalog_sha256"] == catalog_hash
            && artifact.provenance_sha256.get("catalog") == Some(&catalog_hash),
        "model/catalog binding differs"
    );
    let catalog: Value = serde_json::from_slice(&catalog_bytes)?;
    let coordinates = catalog["coordinates"]
        .as_array()
        .context("catalog coordinates")?;
    ensure!(
        coordinates.len() == artifact.coordinates.len(),
        "coordinate catalog size differs"
    );
    for (old, coordinate) in coordinates.iter().zip(&artifact.coordinates) {
        let portable = serde_json::to_value(coordinate)?;
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
                "coordinate catalog geometry differs: {key}"
            );
        }
    }
    let query_bytes = fs::read(root.join("queries.json"))?;
    ensure!(
        manifest["queries_sha256"] == hash(&query_bytes),
        "query metadata changed"
    );
    let queries: Vec<Value> = serde_json::from_slice(&query_bytes)?;
    let authors = manifest["authors"].as_array().context("gallery authors")?;
    ensure!(
        !authors.is_empty()
            && authors.iter().all(Value::is_string)
            && authors
                .windows(2)
                .all(|pair| pair[0].as_str() < pair[1].as_str()),
        "authors duplicated or unsorted"
    );
    let q = queries.len();
    let a = authors.len();
    let k = coordinates.len();
    let f = artifact.families.len();
    ensure!(
        q > 0
            && manifest["query_count"].as_u64() == Some(q as u64)
            && manifest["coordinate_count"].as_u64() == Some(k as u64),
        "query/coordinate count mismatch"
    );
    let split = string(&manifest, "split")?;
    ensure!(
        ["train", "dev", "test"].contains(&split),
        "unsupported query split"
    );
    let mut own = Vec::new();
    for query in &queries {
        let index = query["own"].as_u64().context("true author index")? as usize;
        ensure!(
            index < a && query["author"] == authors[index] && query["split"] == split,
            "query/gallery metadata differs"
        );
        own.push(index);
    }
    let arrays = &manifest["arrays"];
    let query = load_array(&root, &arrays["query_coordinates"], &[q, k])?;
    let author = load_array(&root, &arrays["author_coordinates"], &[a, k])?;
    let raw = load_array(&root, &arrays["family_distances"], &[q, a, f])?;
    let held = if split == "train" {
        Some(load_array(
            &root,
            &arrays["own_heldout_coordinates"],
            &[q, k],
        )?)
    } else {
        None
    };
    let mut logits = Vec::with_capacity(q * a);
    let mut minimum_tail = f64::INFINITY;
    for i in 0..q {
        for j in 0..a {
            let target = if j == own[i] {
                held.as_ref()
                    .map(|values| &values[i * k..(i + 1) * k])
                    .unwrap_or(&author[j * k..(j + 1) * k])
            } else {
                &author[j * k..(j + 1) * k]
            };
            let score = metric.score_pair(
                &raw[(i * a + j) * f..(i * a + j + 1) * f],
                &query[i * k..(i + 1) * k],
                target,
            )?;
            minimum_tail = minimum_tail.min(score.minimum_unselected_tail);
            logits.push(score.logit);
        }
    }
    let query_ids: Vec<_> = queries.iter().map(|row| row["id"].clone()).collect();
    let comparison = if let Some(path) = expected {
        let previous = read(path)?;
        ensure!(
            previous["query_ids"] == json!(query_ids)
                && previous["candidate_authors"] == json!(authors),
            "reference query/candidate order changed"
        );
        let previous: Vec<Vec<f64>> = serde_json::from_value(previous["logits"].clone())?;
        ensure!(
            previous.len() == q
                && previous
                    .iter()
                    .all(|row| row.len() == a && row.iter().all(|v| v.is_finite())),
            "reference logit shape/values differ"
        );
        let mut error = 0.0_f64;
        for (i, row) in previous.iter().enumerate() {
            let actual = &logits[i * a..(i + 1) * a];
            for (actual, expected) in actual.iter().zip(row) {
                error = error.max((actual - expected).abs());
            }
            ensure!(
                ordering(actual) == ordering(row),
                "candidate ordering or ties changed for query {i}"
            );
        }
        ensure!(
            error <= 1e-10,
            "Python/Rust logit error exceeds tolerance: {error}"
        );
        Some(
            json!({"reference_scores_sha256":hash(&fs::read(path)?),"maximum_absolute_logit_error":error,
            "absolute_tolerance":1e-10,"all_candidate_rankings_and_ties_identical":true,"compared_queries":q,
            "compared_candidate_scores":q*a}),
        )
    } else {
        None
    };
    fs::create_dir_all(out)?;
    let bytes: Vec<_> = logits
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    fs::write(out.join("logits.f64"), &bytes)?;
    fs::write(out.join("queries.json"), &query_bytes)?;
    let receipt = json!({"schema":"slopninja-coordinate-inference-scores-v1","model_sha256":hash(&fs::read(model_path)?),
        "export_manifest_sha256":hash(&fs::read(manifest_path)?),"authors":authors,"query_count":q,"coordinate_count":k,
        "logits":{"path":"logits.f64","shape":[q,a],"dtype":"float64-le","bytes":bytes.len(),"sha256":hash(&bytes)},
        "queries_sha256":hash(&query_bytes),"minimum_unselected_tail":minimum_tail,"comparison":comparison,
        "executed_binary_sha256":hash(&fs::read(std::env::current_exe()?)?),
        "source_sha256":{"coordinate_inference.rs":hash(include_bytes!("../coordinate_inference.rs")),
            "scorer.rs":hash(include_bytes!("slop_ninja-score.rs"))},"training_calls":0,"external_model_calls":0});
    fs::write(
        out.join("manifest.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    println!(
        "Scored {q} queries against {a} authors; comparison {}",
        if expected.is_some() {
            "passed"
        } else {
            "not requested"
        }
    );
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    let artifact: Artifact = serde_json::from_slice(&fs::read(&args.model)?)?;
    let metric = Metric::new(artifact)?;
    match args.command {
        Command::Export {
            export,
            out,
            expected_scores,
        } => score_export(
            &metric,
            &args.model,
            &export,
            &out,
            expected_scores.as_deref(),
        ),
        Command::Profiles { query, target } => {
            let query = bound_profile(&query, &metric)?;
            let target = bound_profile(&target, &metric)?;
            println!(
                "{}",
                serde_json::to_string(&metric.score_profiles(&query, &target)?)?
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_reader_rejects_digest_shape_nonfinite_values_and_path_escape() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let bytes: Vec<_> = [1.0_f64, 2.0]
            .into_iter()
            .flat_map(f64::to_le_bytes)
            .collect();
        fs::write(root.join("a.f64"), &bytes).unwrap();
        let binding = json!({"path":"a.f64","shape":[1,2],"dtype":"float64-le","bytes":bytes.len(),"sha256":hash(&bytes)});
        assert_eq!(
            load_array(&root, &binding, &[1, 2]).unwrap(),
            vec![1.0, 2.0]
        );
        assert!(load_array(&root, &binding, &[2, 1]).is_err());
        fs::write(root.join("a.f64"), f64::NAN.to_le_bytes()).unwrap();
        assert!(load_array(&root, &binding, &[1, 2]).is_err());
        let nan = json!({"path":"a.f64","shape":[1],"dtype":"float64-le","bytes":8,"sha256":hash(&f64::NAN.to_le_bytes())});
        assert!(load_array(&root, &nan, &[1]).is_err());
        assert!(file(&root, "/tmp").is_err());
        assert!(file(&root, "..").is_err());
    }
    #[test]
    fn ranking_comparison_preserves_tie_groups() {
        assert_eq!(
            ordering(&[3.0, 3.0, 1.0]),
            (vec![0, 1, 2], vec![true, false])
        );
        assert_ne!(ordering(&[3.0, 3.0, 1.0]), ordering(&[3.0, 2.0, 1.0]));
    }
}
