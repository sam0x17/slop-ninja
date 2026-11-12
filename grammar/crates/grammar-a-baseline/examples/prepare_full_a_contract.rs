//! Write the full-A v2 contract (`slop_ninja_full_a_contract_v2`) for a
//! shared-contract trainer bundle.
//!
//! prepare_full_a_contract --bundle BUNDLE --normalization FILE.json
//!     --parent-artifact-id HEX [--roster ROSTER.json] [--teacher-class NAME]
//!     --out NEW_CONTRACT.json
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
//! object or a contract holding it, and its digest is recorded. The contract
//! is published with the artifact, so any absolute local path in the
//! normalization (an older fit's run receipts) is recorded as
//! `grammar_a_baseline::provenance` makes it portable at the run receipts'
//! pointers: repository-relative, or only its final component under `data/` or
//! outside the repository. The recorded digest stays
//! that of FILE as read. No text is read beyond the row metadata.
//!
//! With `--teacher-class NAME`, the contract is a teacher class's (the trainer's
//! `teacher_a.py`): it records `teacher_class`, its `raw_dimension` is the
//! class's encoder width, and the normalization must be the same Train-only
//! rule fitted on that class's pretrained h at that width. A teacher fit is
//! never exported, so such a contract is never published. Without it the
//! contract is the V6-width one, unchanged.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_a_baseline::provenance::portable_at;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

pub const CONTRACT_SCHEMA: &str = "slop_ninja_full_a_contract_v2";
const NORMALIZATION_FIT: &str = "train_query_only_equal_source_family_weight";
/// The V6 h width (every contract without a teacher class).
const V6_WIDTH: usize = 768;
/// Teacher classes and their encoder widths (`teacher_a.CLASSES` in grammar-eval/python).
const TEACHER_CLASSES: &[(&str, usize)] = &[("modernbert_large_v1", 1024)];

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
    /// A teacher class (never exported): its width replaces the V6 width.
    #[arg(long)]
    teacher_class: Option<String>,
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

/// The encoder width of a teacher class, or the V6 width without one.
fn width(teacher: Option<&str>) -> Result<usize> {
    match teacher {
        None => Ok(V6_WIDTH),
        Some(name) => TEACHER_CLASSES.iter().find(|(n, _)| *n == name).map(|(_, w)| *w)
            .with_context(|| format!("unknown teacher class {name:?}")),
    }
}

fn normalization(v: &Value, width: usize) -> Result<Value> {
    let n = if v["embedding_normalization"].is_object() { &v["embedding_normalization"] } else { v };
    ensure!(
        n["fit"] == NORMALIZATION_FIT
            && n["dimension"] == width
            && n["calibration_used_for_fit"] == false
            && n["development_used_for_fit"] == false
            && n["reference_used_for_fit"] == false
            && n["final_test_opened"] == false,
        "normalization must be a Train-only fit of the {width}-dimensional h"
    );
    for k in ["mean", "scale"] {
        let a = n[k].as_array().context("normalization vector")?;
        ensure!(
            a.len() == width && a.iter().all(|x| x.as_f64().is_some_and(f64::is_finite)),
            "normalization {k}"
        );
    }
    ensure!(n["scale"].as_array().unwrap().iter().all(|x| x.as_f64().unwrap() > 0.0), "normalization scale");
    let mut n = n.clone();
    // Only the run receipts record locations (`fit_full_a_normalization`).
    for pointer in portable_at(&mut n, &["/runs/*/run"]) {
        let recorded = n.pointer(&pointer).unwrap_or(&Value::Null);
        eprintln!("provenance: embedding_normalization{pointer} recorded as {recorded}");
    }
    Ok(n)
}

fn contract(manifest: &[u8], roster: Vec<String>, norm: Value, norm_sha: &str, parent: &str,
            teacher: Option<&str>) -> Result<Value> {
    ensure!(roster.len() >= 2, "the AI roster needs at least two Train generator labels");
    ensure!(parent.len() == 64 && parent.bytes().all(|b| b.is_ascii_hexdigit()), "parent artifact id");
    let w = width(teacher)?;
    ensure!(norm["dimension"] == w, "the normalization's dimension is not the encoder width {w}");
    let mut c = json!({"schema": CONTRACT_SCHEMA, "parent_artifact_id": parent, "bundle_manifest_sha256": sha(manifest),
        "input_mode": "text", "raw_dimension": w, "zero_cfg_coordinates": 1820,
        "maximum_tokens_including_specials": 2048, "token_truncation": false,
        "origin_order": ["human_only", "model_only", "mixed"], "span_label_policy": "slop_ninja_token_span_label_v1",
        "span_coordinates": "utf8_byte_offsets_in_exact_final_text", "human_profiles": [], "ai_profiles": roster,
        "ai_roster_rule": "sorted generator labels of Train rows with a positive cohort weight",
        "embedding_normalization": norm, "embedding_normalization_source_sha256": norm_sha,
        "final_test_opened": false});
    if let Some(name) = teacher {
        c["teacher_class"] = json!(name);
    }
    Ok(c)
}

fn main() -> Result<()> {
    let a = Args::parse();
    ensure!(!a.out.exists(), "{} exists", a.out.display());
    let manifest = fs::read(a.bundle.join("MANIFEST.json"))?;
    let rows = BufReader::new(fs::File::open(a.bundle.join("rows.jsonl"))?);
    let nb = fs::read(&a.normalization)?;
    let teacher = a.teacher_class.as_deref();
    let norm = normalization(&serde_json::from_slice(&nb)?, width(teacher)?)?;
    let train = roster(rows)?;
    let ai = match &a.roster {
        Some(p) => fixed_roster(&serde_json::from_slice(&fs::read(p)?)?, &train)?,
        None => train,
    };
    let c = contract(&manifest, ai, norm, &sha(&nb), &a.parent_artifact_id, teacher)?;
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
        assert!(normalization(&json!({"embedding_normalization": n.clone()}), V6_WIDTH).is_ok());
        // Run receipts with absolute local paths keep no local directory.
        let mut runs = n.clone();
        runs["runs"] = json!([{"run": "/opt/elsewhere/norm/run-000", "shard": 0}, {"run": "norm/run-001", "shard": 1}]);
        let kept = normalization(&runs, V6_WIDTH).unwrap();
        assert_eq!(
            (kept["runs"][0]["run"].as_str(), kept["runs"][1]["run"].as_str()),
            (Some("run-000"), Some("norm/run-001"))
        );
        let mut bad = n.clone();
        bad["development_used_for_fit"] = json!(true);
        assert!(normalization(&bad, V6_WIDTH).is_err());
        let c = contract(b"{}", vec!["a".into(), "b".into()], n.clone(), "x", &"0".repeat(64), None).unwrap();
        assert_eq!(c["bundle_manifest_sha256"], sha(b"{}"));
        assert_eq!(c["human_profiles"], json!([]));
        assert_eq!(c["raw_dimension"], 768);
        assert!(c.get("teacher_class").is_none());
        assert!(contract(b"{}", vec!["a".into()], n, "x", &"0".repeat(64), None).is_err());
    }

    #[test]
    fn a_teacher_contract_takes_the_class_width_and_names_the_class() {
        let at = |w: usize| json!({"fit": NORMALIZATION_FIT, "dimension": w, "mean": vec![0.0; w],
            "scale": vec![1.0; w], "calibration_used_for_fit": false, "development_used_for_fit": false,
            "reference_used_for_fit": false, "final_test_opened": false});
        let teacher = Some("modernbert_large_v1");
        let w = width(teacher).unwrap();
        assert_eq!(w, 1024);
        let n = normalization(&at(w), w).unwrap();
        let c = contract(b"{}", vec!["a".into(), "b".into()], n.clone(), "x", &"0".repeat(64), teacher).unwrap();
        assert_eq!((c["raw_dimension"].as_u64(), c["teacher_class"].as_str()), (Some(1024), teacher));
        assert_eq!(c["embedding_normalization"]["dimension"], 1024);
        // A V6-width normalization never makes a teacher contract, nor a teacher-width one a V6 contract.
        assert!(normalization(&at(V6_WIDTH), w).is_err());
        assert!(normalization(&at(w), V6_WIDTH).is_err());
        assert!(contract(b"{}", vec!["a".into(), "b".into()], n, "x", &"0".repeat(64), None).is_err());
        let v6 = normalization(&at(V6_WIDTH), V6_WIDTH).unwrap();
        assert!(contract(b"{}", vec!["a".into(), "b".into()], v6, "x", &"0".repeat(64), teacher).is_err());
        assert!(width(Some("unknown_v1")).is_err());
    }
}
