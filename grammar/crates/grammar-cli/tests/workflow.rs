use grammar_core::{
    features::{Family, Features},
    space::Reference,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path, process::Command};

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_unslop-grammar"))
        .args(args)
        .output()
        .unwrap()
}

fn features(text: &str) -> Features {
    let mut counts = BTreeMap::new();
    for word in text.split_whitespace() {
        *counts.entry(word.into()).or_insert(0) += 1;
    }
    Features {
        schema: "fixture-v1".into(),
        parser_identity: "fixture-parser".into(),
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

#[test]
fn persisted_fit_projection_rejects_unknown_axes_and_preserves_outputs() {
    let tmp = tempfile::tempdir().unwrap();
    let refs = tmp.path().join("refs.json");
    let fitted = tmp.path().join("space.json");
    let feature_path = tmp.path().join("features.json");
    let projected = tmp.path().join("point.json");
    let references = [
        Reference {
            source_group: "a".into(),
            features: features("red red"),
        },
        Reference {
            source_group: "b".into(),
            features: features("red blue"),
        },
    ];
    fs::write(&refs, serde_json::to_vec(&references).unwrap()).unwrap();
    let fit = cli(&[
        "fit",
        refs.to_str().unwrap(),
        "--out",
        fitted.to_str().unwrap(),
    ]);
    assert!(
        fit.status.success(),
        "{}",
        String::from_utf8_lossy(&fit.stderr)
    );
    let original_space = fs::read(&fitted).unwrap();
    assert!(
        !cli(&[
            "fit",
            refs.to_str().unwrap(),
            "--out",
            fitted.to_str().unwrap()
        ])
        .status
        .success()
    );
    assert_eq!(fs::read(&fitted).unwrap(), original_space);
    fs::write(
        &feature_path,
        serde_json::to_vec(&features("blue")).unwrap(),
    )
    .unwrap();
    let project = cli(&[
        "project",
        feature_path.to_str().unwrap(),
        "--space",
        fitted.to_str().unwrap(),
        "--out",
        projected.to_str().unwrap(),
    ]);
    assert!(
        project.status.success(),
        "{}",
        String::from_utf8_lossy(&project.stderr)
    );
    let point: Value = serde_json::from_slice(&fs::read(projected).unwrap()).unwrap();
    assert!(point["distance"]["euclidean"].as_f64().unwrap() > 0.0);
    fs::write(
        &feature_path,
        serde_json::to_vec(&features("green")).unwrap(),
    )
    .unwrap();
    let failure = cli(&[
        "project",
        feature_path.to_str().unwrap(),
        "--space",
        fitted.to_str().unwrap(),
        "--out",
        tmp.path().join("unknown.json").to_str().unwrap(),
    ]);
    assert!(!failure.status.success());
    assert!(String::from_utf8_lossy(&failure.stderr).contains("unseen coordinate"));
}

#[test]
fn installed_parser_materializes_and_measures_neighborhood() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !base.join("../.venv/bin/python").exists() {
        eprintln!("Skipping local parser integration: install requirements-grammar.lock");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("run");
    let result = cli(&[
        "neighborhood",
        base.join("examples/source.txt").to_str().unwrap(),
        "--references",
        base.join("examples/references.jsonl").to_str().unwrap(),
        "--max-candidates",
        "8",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value =
        serde_json::from_slice(&fs::read(out.join("report.json")).unwrap()).unwrap();
    let source = fs::read_to_string(base.join("examples/source.txt")).unwrap();
    let source_hash = hex::encode(Sha256::digest(source.as_bytes()));
    assert_eq!(report["source_point"]["source_sha256"], source_hash);
    assert!(report["axes"].as_u64().unwrap() > 50);
    assert!(report["recommendation"].is_null());
    assert_eq!(report["llm_calls"], json!(0));
    assert_eq!(report["detector_calls"], json!(0));
    let candidates = report["candidates"].as_array().unwrap();
    assert!(
        candidates.len() >= 2,
        "expected at least contraction and passive relative candidates"
    );
    for candidate in candidates {
        let derivation: grammar_core::edits::Candidate =
            serde_json::from_value(candidate["candidate"].clone()).unwrap();
        assert_eq!(derivation.source_sha256, source_hash);
        assert_eq!(
            grammar_core::edits::apply(&source, &derivation.edits).unwrap(),
            derivation.text
        );
        assert_eq!(
            hex::encode(Sha256::digest(derivation.text.as_bytes())),
            derivation.candidate_sha256
        );
        assert_eq!(
            candidate["point"]["source_sha256"],
            derivation.candidate_sha256
        );
        assert_eq!(
            derivation.catalog_version,
            grammar_core::rules::CATALOG_VERSION
        );
        assert_eq!(
            candidate["candidate"]["semantic_equivalence_certified"],
            json!(false)
        );
        assert_eq!(candidate["point"]["space_id"], report["space_id"]);
        assert!(!candidate["movements"].as_array().unwrap().is_empty());
    }
    let audit = report["references"]["samples"].as_array().unwrap();
    assert_eq!(
        audit
            .iter()
            .filter(|row| row["used_for_fit"] == true)
            .count(),
        2
    );
    assert!(
        audit
            .iter()
            .any(|row| row["split"] == "dev" && row["used_for_fit"] == false)
    );
    let space: Value = serde_json::from_slice(&fs::read(out.join("space.json")).unwrap()).unwrap();
    let fitted_hashes: std::collections::BTreeSet<_> = space["references"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["source_sha256"].as_str().unwrap())
        .collect();
    for row in audit {
        assert_eq!(
            fitted_hashes.contains(row["source_sha256"].as_str().unwrap()),
            row["used_for_fit"] == true
        );
    }
    assert!(
        !out.join("reference-002.features.json").exists(),
        "held-out fixture was parsed into reference features"
    );
}

fn events(out: &Path) -> Vec<Value> {
    fs::read_to_string(out.join("events.jsonl"))
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn initial_parser_failure_persists_run_error_without_success_artifacts() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("failed-run");
    let result = cli(&[
        "--python",
        "/grammar-workflow-missing-python",
        "neighborhood",
        base.join("examples/source.txt").to_str().unwrap(),
        "--references",
        base.join("examples/references.jsonl").to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    let records = events(&out);
    assert!(
        records
            .iter()
            .any(|row| row["stage"] == "run" && row["status"] == "started")
    );
    let failure = records
        .iter()
        .find(|row| row["stage"] == "run" && row["status"] == "failed")
        .expect("missing persisted run failure");
    assert!(
        failure["detail"]["error"]
            .as_str()
            .unwrap()
            .contains("Cannot start syntax interpreter")
    );
    assert!(!out.join("source.syntax.json").exists());
    assert!(!out.join("report.json").exists());
    assert!(!records.iter().any(|row| row["status"] == "complete"));
}

#[test]
fn late_invalid_weight_retains_reference_audit_and_generated_candidates() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !base.join("../.venv/bin/python").exists() {
        eprintln!("Skipping local parser integration: install requirements-grammar.lock");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("failed-fit");
    let config = tmp.path().join("invalid-weight.json");
    fs::write(
        &config,
        serde_json::to_vec(&json!({"family_weights":{"word":-1.0}})).unwrap(),
    )
    .unwrap();
    let result = cli(&[
        "neighborhood",
        base.join("examples/source.txt").to_str().unwrap(),
        "--references",
        base.join("examples/references.jsonl").to_str().unwrap(),
        "--max-candidates",
        "1",
        "--config",
        config.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("invalid family weight"));
    let audit: Value =
        serde_json::from_slice(&fs::read(out.join("references.audit.json")).unwrap()).unwrap();
    assert_eq!(
        audit["samples"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["used_for_fit"] == true)
            .count(),
        2
    );
    assert!(out.join("source.syntax.json").exists());
    assert!(out.join("source.features.json").exists());
    assert!(out.join("reference-000.features.json").exists());
    assert!(out.join("reference-001.features.json").exists());
    assert!(!out.join("reference-002.features.json").exists());
    let generated: Vec<grammar_core::edits::Candidate> =
        serde_json::from_slice(&fs::read(out.join("generated.json")).unwrap()).unwrap();
    assert_eq!(generated.len(), 1);
    let features: Features =
        serde_json::from_slice(&fs::read(out.join("candidate-000.features.json")).unwrap())
            .unwrap();
    assert_eq!(features.source_sha256, generated[0].candidate_sha256);
    let records = events(&out);
    let failure = records
        .iter()
        .find(|row| row["stage"] == "run" && row["status"] == "failed")
        .expect("late fit error was not persisted");
    assert!(
        failure["detail"]["error"]
            .as_str()
            .unwrap()
            .contains("invalid family weight")
    );
    assert!(
        !records
            .iter()
            .any(|row| row["stage"] == "run" && row["status"] == "complete")
    );
    assert!(!out.join("space.json").exists());
    assert!(!out.join("report.json").exists());
}
