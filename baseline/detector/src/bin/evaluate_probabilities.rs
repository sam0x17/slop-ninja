//! Compare exported encoder probabilities with the same Rust metrics used by
//! the word/grammar controls. Labels and groups come only from admitted records.

use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use slop_ninja_detector::{
    dataset::{self, Evidence, OriginRecord, Split, sha256},
    metrics::{self, EvaluationReport, OperatingPoint, ProbabilityRow},
    model::CLASS_NAMES,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(about = "Evaluate frozen detector probability exports using admitted corpus labels")]
struct Args {
    #[arg(long)]
    records: PathBuf,
    #[arg(long)]
    calibration_predictions: PathBuf,
    #[arg(long)]
    test_predictions: PathBuf,
    #[arg(long)]
    output: PathBuf,
    /// Confirm model selection and temperature calibration are already frozen.
    #[arg(long)]
    open_final_test: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ExportedPrediction {
    id: String,
    text_sha256: String,
    artifact_id: String,
    classes: [String; 3],
    probabilities: [f64; 3],
    status: String,
}

#[derive(Clone)]
struct ExpectedRecord {
    id: String,
    text_sha256: String,
    group: String,
    label: usize,
    collection: String,
    evidence: String,
}

struct JoinedPredictions {
    artifact_id: String,
    rows: Vec<ProbabilityRow>,
}

#[derive(Serialize)]
struct ImportedEvaluation {
    schema: &'static str,
    artifact_id: String,
    final_test_opened: bool,
    records_sha256: String,
    calibration_predictions_sha256: String,
    test_predictions_sha256: String,
    training_class_counts: [usize; 3],
    training_prior: [f64; 3],
    calibration_diagnostics: EvaluationReport,
    report: EvaluationReport,
    collection_slices: BTreeMap<String, EvaluationReport>,
    evidence_slices: BTreeMap<String, EvaluationReport>,
    notes: Vec<String>,
}

fn read_jsonl<T: DeserializeOwned>(path: &Path) -> Result<(Vec<T>, String)> {
    // Hash the exact bytes parsed, rather than reopening a possibly changed file.
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut records = Vec::new();
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        records.push(
            serde_json::from_slice(line)
                .with_context(|| format!("{} line {}", path.display(), index + 1))?,
        );
    }
    ensure!(
        !records.is_empty(),
        "{} contains no records",
        path.display()
    );
    Ok((records, sha256(bytes)))
}

fn expected_records(records: &[OriginRecord], split: Split) -> Result<Vec<ExpectedRecord>> {
    records
        .iter()
        .filter(|record| record.split == Some(split))
        .map(|record| {
            let evidence = serde_json::to_value(record.evidence)?;
            Ok(ExpectedRecord {
                id: record.id.clone(),
                text_sha256: record.text_sha256.clone(),
                group: record.source_group.clone(),
                label: record.origin.index(),
                collection: record.source.collection.clone(),
                evidence: evidence
                    .as_str()
                    .context("origin evidence did not serialize as a name")?
                    .into(),
            })
        })
        .collect()
}

fn join_predictions(
    expected: &[ExpectedRecord],
    exported: &[ExportedPrediction],
) -> Result<JoinedPredictions> {
    ensure!(
        !expected.is_empty(),
        "the requested corpus partition is empty"
    );
    ensure!(
        expected.len() == exported.len(),
        "prediction coverage mismatch: expected {} rows, received {}",
        expected.len(),
        exported.len()
    );
    let artifact_id = &exported[0].artifact_id;
    ensure!(
        artifact_id.len() == 64
            && artifact_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "artifact_id must be a lowercase SHA256 identity"
    );
    let mut by_id = BTreeMap::new();
    for row in exported {
        ensure!(
            row.status == "ok",
            "{} has an unsupported or failed prediction status",
            row.id
        );
        ensure!(
            &row.artifact_id == artifact_id,
            "probability file contains multiple artifact identities"
        );
        ensure!(
            row.classes == CLASS_NAMES.map(str::to_owned),
            "{} has an incorrect class order",
            row.id
        );
        ensure!(
            by_id.insert(&row.id, row).is_none(),
            "duplicate prediction ID: {}",
            row.id
        );
    }
    let mut expected_ids = BTreeSet::new();
    let mut rows = Vec::with_capacity(expected.len());
    for expected in expected {
        ensure!(
            expected_ids.insert(&expected.id),
            "duplicate expected corpus ID: {}",
            expected.id
        );
        let prediction = by_id
            .get(&expected.id)
            .with_context(|| format!("missing prediction for corpus record {}", expected.id))?;
        ensure!(
            prediction.text_sha256 == expected.text_sha256,
            "{}: prediction text hash differs from the admitted source",
            expected.id
        );
        rows.push(ProbabilityRow {
            id: expected.id.clone(),
            group: expected.group.clone(),
            label: expected.label,
            probabilities: prediction.probabilities,
        });
    }
    // Reuse the exact finite unit-simplex and row-identity checks used by the
    // native controls. Neither invalid scores nor missing rows are dropped.
    metrics::summarize(&rows)?;
    Ok(JoinedPredictions {
        artifact_id: artifact_id.clone(),
        rows,
    })
}

fn slices(
    artifact_id: &str,
    expected: &[ExpectedRecord],
    predictions: &[ProbabilityRow],
    prior: [f64; 3],
    points: &[OperatingPoint],
    key: impl Fn(&ExpectedRecord) -> &str,
) -> Result<BTreeMap<String, EvaluationReport>> {
    let mut groups: BTreeMap<String, Vec<ProbabilityRow>> = BTreeMap::new();
    for (record, prediction) in expected.iter().zip(predictions) {
        groups
            .entry(key(record).into())
            .or_default()
            .push(prediction.clone());
    }
    groups
        .into_iter()
        .map(|(name, rows)| {
            Ok((
                name,
                metrics::evaluate_probability_rows(artifact_id, &rows, prior, points)?,
            ))
        })
        .collect()
}

fn evaluate(args: &Args) -> Result<ImportedEvaluation> {
    ensure!(
        args.open_final_test,
        "final test remains sealed; pass --open-final-test only after model selection and calibration are frozen"
    );
    ensure!(!args.output.exists(), "evaluation output already exists");
    let (records, records_sha256): (Vec<OriginRecord>, _) = read_jsonl(&args.records)?;
    dataset::validate_records(&records)?;
    ensure!(
        records.iter().all(|record| record.split.is_some()),
        "all source-family splits must be frozen before evaluation"
    );
    ensure!(
        records
            .iter()
            .all(|record| record.evidence != Evidence::SyntheticFixture),
        "synthetic protocol fixtures cannot support normal detector evaluation"
    );
    let mut training_class_counts = [0_usize; 3];
    for record in records
        .iter()
        .filter(|record| record.split == Some(Split::Train))
    {
        training_class_counts[record.origin.index()] += 1;
    }
    ensure!(
        training_class_counts.iter().all(|count| *count > 0),
        "training records must contain all three admitted origin classes to derive the fixed prior control"
    );
    let training_rows = training_class_counts.iter().sum::<usize>();
    let training_prior = training_class_counts.map(|count| count as f64 / training_rows as f64);

    let calibration_expected = expected_records(&records, Split::Calibration)?;
    let (calibration_exports, calibration_predictions_sha256) =
        read_jsonl(&args.calibration_predictions)?;
    let calibration = join_predictions(&calibration_expected, &calibration_exports)?;
    let points = metrics::select_operating_points(&calibration.rows, &[0.01, 0.05])?;
    let calibration_diagnostics = metrics::evaluate_probability_rows(
        &calibration.artifact_id,
        &calibration.rows,
        training_prior,
        &points,
    )?;

    // Threshold selection is complete before reading any test probability.
    let test_expected = expected_records(&records, Split::Test)?;
    let (test_exports, test_predictions_sha256) = read_jsonl(&args.test_predictions)?;
    let test = join_predictions(&test_expected, &test_exports)?;
    ensure!(
        calibration.artifact_id == test.artifact_id,
        "calibration and test probabilities reference different artifacts"
    );
    let report =
        metrics::evaluate_probability_rows(&test.artifact_id, &test.rows, training_prior, &points)?;
    let collection_slices = slices(
        &test.artifact_id,
        &test_expected,
        &test.rows,
        training_prior,
        &points,
        |row| &row.collection,
    )?;
    let evidence_slices = slices(
        &test.artifact_id,
        &test_expected,
        &test.rows,
        training_prior,
        &points,
        |row| &row.evidence,
    )?;
    Ok(ImportedEvaluation {
        schema: "slop_ninja_imported_probability_evaluation_v1", artifact_id: test.artifact_id, final_test_opened: true,
        records_sha256, calibration_predictions_sha256, test_predictions_sha256, training_class_counts, training_prior,
        calibration_diagnostics, report, collection_slices, evidence_slices,
        notes: vec![
            "Model weights and temperature are not fitted or changed here. The verified artifact runner must export already calibrated probabilities.".into(),
            "Operating points use only calibration human rows. Calibration diagnostics are fitted-set observations, not independent performance estimates.".into(),
            "All controls and collection/evidence slices reuse the same training prior and global calibration thresholds; test scores never choose a threshold.".into(),
            "Labels, source groups and slice metadata come exclusively from the admitted corpus, with exact prediction-ID and UTF-8 text-hash coverage checks.".into(),
            "This stage checks score records and computes metrics; an artifact ID on a prediction is not independent proof of model execution.".into(),
        ],
    })
}

fn main() -> Result<()> {
    let args = Args::parse();
    let report = evaluate(&args)?;
    let parent = args
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut output = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut output, &report)?;
    output.write_all(b"\n")?;
    output.as_file().sync_all()?;
    output.persist_noclobber(&args.output).with_context(|| {
        format!(
            "cannot create {} without overwriting",
            args.output.display()
        )
    })?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"output":args.output,"artifact_id":report.artifact_id,"test_metrics":report.report.model})
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(prefix: &str) -> (Vec<ExpectedRecord>, Vec<ExportedPrediction>) {
        // Arbitrary protocol labels over invented identifiers; this fixture is
        // exercised below corpus admission and carries no authorship evidence.
        let expected: Vec<_> = (0..3)
            .map(|label| ExpectedRecord {
                id: format!("synthetic-{prefix}-{label}"),
                text_sha256: sha256(format!("synthetic payload {prefix} {label}")),
                group: format!("synthetic-{prefix}-family"),
                label,
                collection: "synthetic-protocol-fixture".into(),
                evidence: "synthetic_fixture".into(),
            })
            .collect();
        let predictions = expected
            .iter()
            .map(|row| ExportedPrediction {
                id: row.id.clone(),
                text_sha256: row.text_sha256.clone(),
                artifact_id: sha256("synthetic classifier identity"),
                classes: CLASS_NAMES.map(str::to_owned),
                probabilities: std::array::from_fn(
                    |class| if class == row.label { 0.8 } else { 0.1 },
                ),
                status: "ok".into(),
            })
            .collect();
        (expected, predictions)
    }

    #[test]
    fn exported_scores_require_exact_coverage_identity_hash_classes_and_probabilities() {
        let (expected, good) = fixture("test");
        assert!(join_predictions(&expected, &good).is_ok());
        assert!(join_predictions(&expected, &good[..2]).is_err());
        for mutation in 0..8 {
            let mut changed = good.clone();
            match mutation {
                0 => changed[0].id = changed[1].id.clone(),
                1 => changed[0].text_sha256 = sha256("other text"),
                2 => changed[0].classes.swap(0, 1),
                3 => changed[0].artifact_id = sha256("another artifact"),
                4 => changed[0].status = "unsupported_input".into(),
                5 => changed[0].probabilities = [0.8, 0.2, 0.2],
                6 => changed[0].probabilities = [f64::NAN, 0.1, 0.1],
                _ => changed[0].id = "unexpected-record".into(),
            }
            assert!(
                join_predictions(&expected, &changed).is_err(),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn explicit_test_gate_and_calibration_thresholds_survive_changed_test_scores() {
        let missing = PathBuf::from("/synthetic-missing-evaluation-input");
        let error = evaluate(&Args {
            records: missing.clone(),
            calibration_predictions: missing.clone(),
            test_predictions: missing.clone(),
            output: missing,
            open_final_test: false,
        })
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("final test remains sealed"));
        let (calibration_expected, calibration_exports) = fixture("calibration");
        let calibration = join_predictions(&calibration_expected, &calibration_exports).unwrap();
        let points = metrics::select_operating_points(&calibration.rows, &[0.01, 0.05]).unwrap();
        let frozen = serde_json::to_value(&points).unwrap();
        let (test_expected, mut test_exports) = fixture("test");
        let first = join_predictions(&test_expected, &test_exports).unwrap();
        let report = metrics::evaluate_probability_rows(
            &first.artifact_id,
            &first.rows,
            [1.0 / 3.0; 3],
            &points,
        )
        .unwrap();
        test_exports[0].probabilities = [0.1, 0.8, 0.1];
        let changed = join_predictions(&test_expected, &test_exports).unwrap();
        let changed_report = metrics::evaluate_probability_rows(
            &changed.artifact_id,
            &changed.rows,
            [1.0 / 3.0; 3],
            &points,
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&points).unwrap(), frozen);
        assert_eq!(
            report.operating_points[0].calibration.threshold,
            changed_report.operating_points[0].calibration.threshold
        );
        assert_eq!(
            report.operating_points[0].human_false_positive_rate.events,
            0
        );
        assert_eq!(
            changed_report.operating_points[0]
                .human_false_positive_rate
                .events,
            1
        );
        assert!(report.unknown_coordinates.is_none());
    }
}
