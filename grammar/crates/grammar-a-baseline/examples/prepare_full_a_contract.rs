//! Write the full-A v2 contract (`slop_ninja_full_a_contract_v2`) for a
//! shared-contract trainer bundle.
//!
//! prepare_full_a_contract --bundle BUNDLE --normalization FILE.json
//!     --parent-artifact-id HEX [--roster ROSTER.json] --out NEW_CONTRACT.json
//!
//! The contract binds the bundle's MANIFEST.json digest, an empty human
//! roster, and the AI roster: the sorted generator labels of Train rows with a
//! positive cohort weight (`full_a_bundle.ai_roster`). With `--roster`, the AI
//! roster is instead the fixed, sorted list of names in ROSTER.json (a JSON
//! array), for example the roster an allow-listed graph class fixes; every
//! positive-weight Train label must be in it, and names without Train rows
//! stay in it. The trainer accepts such a contract only under a protocol
//! `ai_roster` of rule `fixed_v1` naming the same list's digest. The embedding
//! normalization is a Train-only fit of the frozen V6 h; FILE is either that
//! object or a contract holding it, and its digest is recorded. No text is
//! read beyond the row metadata.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

pub const CONTRACT_SCHEMA: &str = "slop_ninja_full_a_contract_v2";
const NORMALIZATION_FIT: &str = "train_query_only_equal_source_family_weight";

#[derive(Parser)]
struct Args {
    #[arg(long)]
    bundle: PathBuf,
    #[arg(long)]
    normalization: PathBuf,
    #[arg(long)]
    parent_artifact_id: String,
    /// A fixed AI roster (JSON array of sorted, unique names).
    #[arg(long)]
    roster: Option<PathBuf>,
    #[arg(long)]
    out: PathBuf,
}

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn canonical_split(s: &str) -> String {
    match s {
        "development_iter" | "dev-iter" => "dev_iter".into(),
        "development_select" | "dev-select" => "dev_select".into(),
        "calibration" => "cal".into(),
        "train-mining" => "train_mining".into(),
        s => s.into(),
    }
}

/// Train generator labels with a positive cohort weight (the AI roster).
fn roster(rows: impl BufRead) -> Result<Vec<String>> {
    let mut labels = BTreeSet::new();
    for (n, line) in rows.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).with_context(|| format!("rows.jsonl line {}", n + 1))?;
        let split = canonical_split(v["split"].as_str().context("split")?);
        ensure!(split != "test", "a training bundle must not contain Test rows");
        if split != "train" {
            continue;
        }
        let w = v["cohort_weight"].as_f64().context("cohort_weight")?;
        if let Some(l) = v["generator_label"].as_str() {
            ensure!(!l.is_empty() && !l.starts_with("other_"), "generator label {l:?}");
            if w > 0.0 {
                labels.insert(l.to_string());
            }
        }
    }
    Ok(labels.into_iter().collect())
}

/// The fixed roster of `--roster`: sorted, unique, non-empty names, none
/// `other_*`, that contain every Train label.
fn fixed_roster(v: &Value, train: &[String]) -> Result<Vec<String>> {
    let names = v.as_array().context("the roster file must hold a JSON array")?.iter()
        .map(|x| Ok(x.as_str().context("roster names are strings")?.to_owned()))
        .collect::<Result<Vec<String>>>()?;
    ensure!(names.windows(2).all(|w| w[0] < w[1]), "roster names must be sorted and unique");
    ensure!(names.iter().all(|x| !x.is_empty() && !x.starts_with("other_")), "roster name");
    let missing: Vec<&String> = train.iter().filter(|l| !names.contains(l)).collect();
    ensure!(missing.is_empty(), "Train generator labels outside the fixed roster: {missing:?}");
    Ok(names)
}

fn normalization(v: &Value) -> Result<Value> {
    let n = if v["embedding_normalization"].is_object() { &v["embedding_normalization"] } else { v };
    ensure!(
        n["fit"] == NORMALIZATION_FIT
            && n["dimension"] == 768
            && n["calibration_used_for_fit"] == false
            && n["development_used_for_fit"] == false
            && n["reference_used_for_fit"] == false
            && n["final_test_opened"] == false,
        "normalization must be a Train-only fit of the 768-dimensional V6 h"
    );
    for k in ["mean", "scale"] {
        let a = n[k].as_array().context("normalization vector")?;
        ensure!(
            a.len() == 768 && a.iter().all(|x| x.as_f64().is_some_and(f64::is_finite)),
            "normalization {k}"
        );
    }
    ensure!(n["scale"].as_array().unwrap().iter().all(|x| x.as_f64().unwrap() > 0.0), "normalization scale");
    Ok(n.clone())
}

fn contract(manifest: &[u8], roster: Vec<String>, norm: Value, norm_sha: &str, parent: &str) -> Result<Value> {
    ensure!(roster.len() >= 2, "the AI roster needs at least two Train generator labels");
    ensure!(parent.len() == 64 && parent.bytes().all(|b| b.is_ascii_hexdigit()), "parent artifact id");
    Ok(json!({"schema": CONTRACT_SCHEMA, "parent_artifact_id": parent, "bundle_manifest_sha256": sha(manifest),
        "input_mode": "text", "raw_dimension": 768, "zero_cfg_coordinates": 1820,
        "maximum_tokens_including_specials": 2048, "token_truncation": false,
        "origin_order": ["human_only", "model_only", "mixed"], "span_label_policy": "slop_ninja_token_span_label_v1",
        "span_coordinates": "utf8_byte_offsets_in_exact_final_text", "human_profiles": [], "ai_profiles": roster,
        "ai_roster_rule": "sorted generator labels of Train rows with a positive cohort weight",
        "embedding_normalization": norm, "embedding_normalization_source_sha256": norm_sha,
        "final_test_opened": false}))
}

fn main() -> Result<()> {
    let a = Args::parse();
    ensure!(!a.out.exists(), "{} exists", a.out.display());
    let manifest = fs::read(a.bundle.join("MANIFEST.json"))?;
    let rows = BufReader::new(fs::File::open(a.bundle.join("rows.jsonl"))?);
    let nb = fs::read(&a.normalization)?;
    let norm = normalization(&serde_json::from_slice(&nb)?)?;
    let train = roster(rows)?;
    let ai = match &a.roster {
        Some(p) => fixed_roster(&serde_json::from_slice(&fs::read(p)?)?, &train)?,
        None => train,
    };
    let c = contract(&manifest, ai, norm, &sha(&nb), &a.parent_artifact_id)?;
    let mut b = serde_json::to_vec_pretty(&c)?;
    b.push(b'\n');
    fs::write(&a.out, &b)?;
    println!("{}", json!({"out": a.out, "contract_sha256": sha(&b), "ai_profiles": c["ai_profiles"],
        "bundle_manifest_sha256": c["bundle_manifest_sha256"]}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_is_sorted_positive_weight_train_labels_and_refuses_test() {
        let rows = [
            json!({"split": "train", "generator_label": "qwen3.6", "cohort_weight": 1.0}),
            json!({"split": "train", "generator_label": "claude-opus-5-5", "cohort_weight": 0.7}),
            json!({"split": "train", "generator_label": "removed", "cohort_weight": 0.0}),
            json!({"split": "train", "generator_label": null, "cohort_weight": 1.0}),
            json!({"split": "dev-iter", "generator_label": "dev-only", "cohort_weight": 1.0}),
        ];
        let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
        assert_eq!(roster(text.as_bytes()).unwrap(), ["claude-opus-5-5", "qwen3.6"]);
        let bad = format!("{text}{}\n", json!({"split": "test", "generator_label": null, "cohort_weight": 1.0}));
        assert!(roster(bad.as_bytes()).is_err());
    }

    #[test]
    fn fixed_roster_keeps_unseen_names_and_needs_every_train_label() {
        let train = vec!["gen-a".to_string(), "gen-b".to_string()];
        let ok = fixed_roster(&json!(["gen-a", "gen-b", "gen-z"]), &train).unwrap();
        assert_eq!(ok, ["gen-a", "gen-b", "gen-z"]);
        assert!(fixed_roster(&json!(["gen-a", "gen-z"]), &train).is_err());
        assert!(fixed_roster(&json!(["gen-b", "gen-a"]), &train).is_err());
        assert!(fixed_roster(&json!(["gen-a", "gen-a", "gen-b"]), &train).is_err());
        assert!(fixed_roster(&json!(["gen-a", "gen-b", "other_ai"]), &train).is_err());
        assert!(fixed_roster(&json!({"names": []}), &train).is_err());
    }

    #[test]
    fn normalization_must_be_train_only_and_contract_binds_the_manifest() {
        let n = json!({"fit": NORMALIZATION_FIT, "dimension": 768, "mean": vec![0.0; 768], "scale": vec![1.0; 768],
            "calibration_used_for_fit": false, "development_used_for_fit": false, "reference_used_for_fit": false,
            "final_test_opened": false});
        assert!(normalization(&json!({"embedding_normalization": n.clone()})).is_ok());
        let mut bad = n.clone();
        bad["development_used_for_fit"] = json!(true);
        assert!(normalization(&bad).is_err());
        let c = contract(b"{}", vec!["a".into(), "b".into()], n.clone(), "x", &"0".repeat(64)).unwrap();
        assert_eq!(c["bundle_manifest_sha256"], sha(b"{}"));
        assert_eq!(c["human_profiles"], json!([]));
        assert!(contract(b"{}", vec!["a".into()], n, "x", &"0".repeat(64)).is_err());
    }
}
