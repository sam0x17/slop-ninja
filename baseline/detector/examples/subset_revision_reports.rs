//! Derive the frozen Pangram cohort from completed Test exports, without inference.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{self, OriginRecord, Split, sha256},
    evaluation_view::{self, Binding, EvaluationView, Family, Selection},
    metrics::{self, OperatingPoint, ProbabilityRow},
    model::CLASS_NAMES,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

const RECORDS_SHA: &str = "1b06538d11fc770fbf6c68b5b9cdb95f12463ebbc6f37e7d518ad8aa08859475";
const MEMBERSHIP_SHA: &str = "808b495cde2d4a6c41b2ef0f6c353eb19d3fe8d0b710b641bfceb3412872f8f1";
const COMPARE_SHA: &str = "8aff9df00e6e05d4fb7039473373b6fdb35f52cceabbfdbfcee1b91a71391660";

#[derive(Parser)]
struct Args {
    #[arg(long)]
    run_root: PathBuf,
    #[arg(long)]
    preparation_dir: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
}

#[derive(Default)]
struct Inputs(BTreeMap<PathBuf, String>);
impl Inputs {
    fn bytes(&mut self, path: &Path) -> Result<Vec<u8>> {
        let bytes = fs::read(path).with_context(|| format!("Read {}", path.display()))?;
        let hash = sha256(&bytes);
        if let Some(previous) = self.0.insert(path.to_owned(), hash.clone()) {
            ensure!(previous == hash, "Input changed: {}", path.display());
        }
        Ok(bytes)
    }
    fn json(&mut self, path: &Path) -> Result<Value> {
        Ok(serde_json::from_slice(&self.bytes(path)?)?)
    }
    fn check(&mut self, path: &Path, hash: &str) -> Result<()> {
        ensure!(
            sha256(self.bytes(path)?) == hash,
            "Input binding differs: {}",
            path.display()
        );
        Ok(())
    }
    fn finish(&self) -> Result<()> {
        for (path, hash) in &self.0 {
            ensure!(
                sha256(fs::read(path)?) == *hash,
                "Input changed: {}",
                path.display()
            );
        }
        Ok(())
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("Missing {key}"))
}
fn save(path: &Path, value: &Value) -> Result<String> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let mut file = fs::File::create_new(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(sha256(bytes))
}
fn references(families: &[Family]) -> Result<BTreeMap<&str, (&str, usize)>> {
    let mut result = BTreeMap::new();
    for f in families {
        for (label, r) in [&f.human, &f.model, &f.mixed].into_iter().enumerate() {
            ensure!(
                result
                    .insert(r.id.as_str(), (f.source_group.as_str(), label))
                    .is_none(),
                "Duplicate view ID"
            );
        }
    }
    Ok(result)
}
fn select_rows(
    rows: &[ProbabilityRow],
    full: &[Family],
    selected: &[Family],
) -> Result<Vec<ProbabilityRow>> {
    metrics::summarize(rows)?;
    let expected = references(full)?;
    let wanted = references(selected)?;
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for row in rows {
        ensure!(seen.insert(row.id.as_str()), "Duplicate prediction ID");
        let &(group, label) = expected
            .get(row.id.as_str())
            .context("Unexpected prediction ID")?;
        ensure!(
            group == row.group && label == row.label,
            "Prediction family or origin differs"
        );
        if let Some(&(wanted_group, wanted_label)) = wanted.get(row.id.as_str()) {
            ensure!(
                wanted_group == group && wanted_label == label,
                "Subset reference differs"
            );
            result.push(row.clone());
        }
    }
    ensure!(
        seen.len() == expected.len() && result.len() == wanted.len(),
        "Incomplete full or subset prediction coverage"
    );
    result.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(result)
}

fn derive_report(
    full: &Value,
    rows: &[ProbabilityRow],
    binding: &Binding,
    points: &[OperatingPoint],
    source_hash: &str,
) -> Result<Value> {
    let artifact = field(full, "artifact_id")?;
    let prior = serde_json::from_value(full["effective_training_class_prior"].clone())?;
    let report = metrics::evaluate_probability_rows(artifact, rows, prior, points)?;
    let margins = full["near_cutoffs"]
        .as_array()
        .context("Missing cutoff diagnostics")?;
    ensure!(
        margins.len() == points.len(),
        "Cutoff diagnostic count differs"
    );
    let mut near = Vec::new();
    for (point, diagnostic) in points.iter().zip(margins) {
        let margin = diagnostic["diagnostic_margin"]
            .as_f64()
            .context("Missing cutoff margin")?;
        ensure!(
            margin.is_finite() && margin > 0.0 && diagnostic["threshold"] == point.threshold,
            "Cutoff diagnostic differs"
        );
        let selected: Vec<_> = rows
            .iter()
            .filter(|r| (r.probabilities[1] + r.probabilities[2] - point.threshold).abs() <= margin)
            .collect();
        near.push(json!({"target_human_false_positive_rate":point.target_human_false_positive_rate,"threshold":point.threshold,"diagnostic_margin":margin,"rows":selected.len(),"human_rows":selected.iter().filter(|r|r.label==0).count(),"ids":selected.iter().map(|r|&r.id).collect::<Vec<_>>()}));
    }
    Ok(json!({
        "schema":"slop_ninja_frozen_encoder_evaluation_v1","artifact_id":artifact,
        "records_sha256":binding.records_sha256,"split":"test","final_test_opened":true,
        "thresholds_sha256":full["thresholds_sha256"],"effective_training_class_prior":prior,
        "evaluation_view":binding,"report":report,"human_model":metrics::summarize_human_model(rows)?,
        "near_cutoffs":near,
        "derivation":{"mode":"frozen_prediction_subset_v1","source_report_sha256":source_hash,"source_evaluation_view":full["evaluation_view"],"additional_inference":false,"thresholds_refitted":false},
        "notes":["All metrics and denominators were recomputed on the declared subset using unchanged probabilities and frozen thresholds. The original complete reports remain separate.","Shared roots and edits across views are repeated exports of the same texts. Different operating thresholds do not establish matched population FPR."]
    }))
}

struct ExportContext<'a> {
    view: &'a EvaluationView,
    view_hash: &'a str,
    archive: &'a [OriginRecord],
    artifact: &'a str,
    threshold_hash: &'a str,
    points: &'a [OperatingPoint],
}
fn verify_export(
    inputs: &mut Inputs,
    directory: &Path,
    full: &Value,
    context: &ExportContext<'_>,
) -> Result<Vec<ProbabilityRow>> {
    let ExportContext {
        view,
        view_hash,
        archive,
        artifact,
        threshold_hash,
        points,
    } = *context;
    ensure!(
        full["schema"] == "slop_ninja_frozen_encoder_evaluation_v1"
            && full["split"] == "test"
            && full["final_test_opened"] == true,
        "Expected completed Test report"
    );
    ensure!(
        full["artifact_id"] == artifact
            && full["report"]["artifact_sha256"] == artifact
            && full["thresholds_sha256"] == threshold_hash
            && full["records_sha256"] == view.records_sha256,
        "Report artifact, thresholds or archive differs"
    );
    let binding = Binding {
        sha256: view_hash.into(),
        records_sha256: view.records_sha256.clone(),
        split: Split::Test,
        selected_rows: view.families.len() * 3,
        source_groups: view.families.len(),
    };
    ensure!(
        full["evaluation_view"] == serde_json::to_value(binding)?,
        "Full view binding differs"
    );
    let declared_points: Vec<OperatingPoint> = full["report"]["operating_points"]
        .as_array()
        .context("Missing operating points")?
        .iter()
        .map(|p| serde_json::from_value(p["calibration"].clone()))
        .collect::<serde_json::Result<_>>()?;
    ensure!(
        serde_json::to_value(declared_points)? == serde_json::to_value(points)?,
        "Report changed its frozen operating points"
    );
    let records: BTreeMap<_, _> = archive.iter().map(|r| (r.id.as_str(), r)).collect();
    let expected = references(&view.families)?;
    let inference = inputs.json(&directory.join("inference-bindings.json"))?;
    ensure!(
        inference["artifact_id"] == artifact,
        "Inference artifact differs"
    );
    let request_bytes = inputs.bytes(&directory.join("requests.jsonl"))?;
    let response_bytes = inputs.bytes(&directory.join("responses.jsonl"))?;
    ensure!(
        inference["requests_sha256"] == sha256(&request_bytes)
            && inference["responses_sha256"] == sha256(&response_bytes),
        "Saved inference file differs"
    );
    let mut requests = BTreeSet::new();
    for line in request_bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        let r: Value = serde_json::from_slice(line)?;
        let id = field(&r, "id")?;
        ensure!(
            expected.contains_key(id) && requests.insert(id.to_owned()),
            "Unexpected or duplicate inference request"
        );
        let record = records.get(id).context("Request ID absent from archive")?;
        ensure!(
            r["text"] == record.text && sha256(field(&r, "text")?) == record.text_sha256,
            "Request text differs from the frozen archive"
        );
    }
    let mut response_rows = BTreeMap::new();
    for line in response_bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        let r: Value = serde_json::from_slice(line)?;
        let id = field(&r, "id")?;
        ensure!(
            expected.contains_key(id)
                && r["artifact_id"] == artifact
                && r["status"] == "ok"
                && r["labels"] == json!(CLASS_NAMES),
            "Response identity, status or labels differ"
        );
        let probabilities: [f64; 3] = serde_json::from_value(r["probabilities"].clone())?;
        ensure!(
            response_rows.insert(id.to_owned(), probabilities).is_none(),
            "Duplicate inference response"
        );
    }
    let bound: Vec<evaluation_view::RecordRef> =
        serde_json::from_value(inference["records"].clone())?;
    let mut bound_ids = BTreeSet::new();
    for reference in bound {
        ensure!(
            expected.contains_key(reference.id.as_str())
                && bound_ids.insert(reference.id.clone())
                && reference.text_sha256 == records[reference.id.as_str()].text_sha256,
            "Inference record hash differs"
        );
    }
    ensure!(
        requests.len() == expected.len()
            && response_rows.len() == expected.len()
            && bound_ids.len() == expected.len(),
        "Incomplete inference archive"
    );
    let rows: Vec<ProbabilityRow> = serde_json::from_value(full["report"]["predictions"].clone())?;
    let rows = select_rows(&rows, &view.families, &view.families)?;
    for row in &rows {
        ensure!(
            row.probabilities == response_rows[&row.id],
            "Report probabilities differ from saved inference"
        );
    }
    Ok(rows)
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new output directory");
    let mut inputs = Inputs::default();
    let control = args
        .run_root
        .join("postfit-evaluation-v1/guarded-handoff-v2");
    let completion = inputs.json(&control.join("completion.json"))?;
    let status = inputs.json(&control.join("status.json"))?;
    ensure!(
        completion["success"] == true
            && completion["check_only"] == false
            && status["stage"] == "evaluation_complete_results_require_review"
            && status["test_predictions_opened"] == true,
        "The original evaluation must finish successfully first"
    );
    let evaluation = args.run_root.join("final-evaluation-v1");
    let freeze_path = evaluation.join("pre-test-bindings.json");
    let freeze = inputs.json(&freeze_path)?;
    ensure!(
        freeze["schema"] == "slop_ninja_v6_final_test_bindings_v1"
            && freeze["plan_sha256"]
                == "c6b5b1d2db43b74aafe175c6f509e02992d0a5e2caeac2b553380b35b802d424",
        "Unexpected final evaluation freeze"
    );
    let opening = inputs.json(&evaluation.join("opening-receipt.json"))?;
    ensure!(
        opening["pre_test_bindings_sha256"] == inputs.0[&freeze_path]
            && opening["checksum_file_sha256"]
                == sha256(inputs.bytes(&evaluation.join("pre-test-bindings.sha256"))?),
        "Opening receipt differs"
    );
    let frozen = freeze["bindings"]
        .as_object()
        .context("Missing pre-Test bindings")?;
    for (path, hash) in frozen {
        inputs.check(
            Path::new(path),
            hash.as_str().context("Invalid frozen hash")?,
        )?;
    }
    let corpus_path = args.preparation_dir.join("selection-v1/records.jsonl");
    inputs.check(&corpus_path, RECORDS_SHA)?;
    inputs.check(
        &args.preparation_dir.join("selection-v1/membership.json"),
        MEMBERSHIP_SHA,
    )?;
    let records = dataset::read_records(&corpus_path)?;
    let groups: BTreeSet<_> = records.iter().map(|r| r.source_group.as_str()).collect();
    ensure!(
        records.len() == 162
            && groups.len() == 18
            && records.iter().all(|r| r.split == Some(Split::Test)),
        "Frozen cohort coverage differs"
    );
    let compare = args.run_root.join("repo/data/baseline-detector/revision-expansion-v6/fitting-tools-v1/bin/compare_encoder_reports");
    inputs.check(&compare, COMPARE_SHA)?;
    let v5_thresholds = args.run_root.join("repo/data/baseline-detector/narrative-expansion-v5/recovered-first-thresholds-v1/thresholds.json");
    let mut detectors = BTreeMap::new();
    for (name, artifact_key, thresholds_path) in [
        ("v5", "v5_artifact", v5_thresholds),
        (
            "v6",
            "candidate_artifact",
            evaluation.join("v6-thresholds/thresholds.json"),
        ),
    ] {
        ensure!(
            frozen.contains_key(thresholds_path.to_str().context("Non-UTF-8 path")?),
            "Thresholds were not frozen before Test"
        );
        let artifact_path = Path::new(field(&freeze, artifact_key)?);
        let manifest = inputs.json(&artifact_path.join("manifest.json"))?;
        ensure!(
            frozen.contains_key(
                artifact_path
                    .join("manifest.json")
                    .to_str()
                    .context("Non-UTF-8 artifact")?
            ),
            "Artifact was not frozen"
        );
        let thresholds = inputs.json(&thresholds_path)?;
        let content = &thresholds["content"];
        ensure!(
            thresholds["content_sha256"] == sha256(serde_json::to_vec(content)?)
                && content["artifact_id"] == manifest["artifact_id"],
            "Threshold content or artifact differs"
        );
        let points: Vec<OperatingPoint> = serde_json::from_value(content["points"].clone())?;
        ensure!(points.len() == 2, "Expected two operating points");
        for (point, rate) in points.iter().zip([0.01, 0.05]) {
            point.validate()?;
            ensure!(
                point.target_human_false_positive_rate == rate,
                "Unexpected target rate"
            );
        }
        let training = inputs.json(&artifact_path.join("training.json"))?;
        let prior = if training["sample_weight_policy"] == "source_origin"
            || training["class_weight_policy"] == "inverse_frequency"
        {
            [1.0 / 3.0; 3]
        } else {
            let counts: [usize; 3] =
                serde_json::from_value(training["class_counts"]["train"].clone())?;
            let n: usize = counts.iter().sum();
            ensure!(n > 0, "Empty training prior");
            counts.map(|n_class| n_class as f64 / n as f64)
        };
        detectors.insert(
            name,
            (
                field(&manifest, "artifact_id")?.to_owned(),
                inputs.0[&thresholds_path].clone(),
                points,
                prior,
            ),
        );
    }
    ensure!(
        detectors["v5"].0 == "da3a604d7bd1708f5ba1c4a4a4be946d77670492b702e41e267fb86237dfb8c9"
            && detectors["v5"].0 != detectors["v6"].0,
        "Unexpected detector pair"
    );
    fs::create_dir(&args.output_dir)?;
    fs::create_dir(args.output_dir.join("views"))?;
    fs::create_dir(args.output_dir.join("reports"))?;
    fs::File::create_new(args.output_dir.join("records.jsonl"))?
        .write_all(&inputs.bytes(&corpus_path)?)?;
    let mut summaries = BTreeMap::new();
    for route in ["primary", "phi4"] {
        let archive_path = args
            .run_root
            .join(format!("{route}-coverage-v1/full/records.jsonl"));
        let (archive, archive_hash) = evaluation_view::read_archive(&archive_path)?;
        let archive_key = archive_path.to_str().context("Non-UTF-8 archive")?;
        inputs.check(
            &archive_path,
            frozen
                .get(archive_key)
                .and_then(Value::as_str)
                .context("Archive not frozen")?,
        )?;
        for stage in ["R2", "R0", "R1"] {
            let key = format!("{route}-{stage}");
            let view_path = args
                .run_root
                .join(format!("{route}-coverage-v1/full/{stage}.json"));
            let value = inputs.json(&view_path)?;
            ensure!(
                frozen.get(view_path.to_str().context("Non-UTF-8 view")?)
                    == Some(&json!(inputs.0[&view_path])),
                "View was not frozen before Test"
            );
            let view: EvaluationView = serde_json::from_value(value)?;
            ensure!(
                view.records_sha256 == archive_hash && view.split == Split::Test,
                "Original view archive differs"
            );
            let selected_families: Vec<_> = view
                .families
                .iter()
                .filter(|f| groups.contains(f.source_group.as_str()))
                .cloned()
                .collect();
            ensure!(
                selected_families.len() == 18,
                "Missing selected family in a full view"
            );
            let selections: Vec<_> = selected_families
                .iter()
                .map(|f| Selection {
                    source_group: f.source_group.clone(),
                    human_id: f.human.id.clone(),
                    model_id: f.model.id.clone(),
                    mixed_id: f.mixed.id.clone(),
                })
                .collect();
            let subset_view = evaluation_view::build_view(
                &records,
                RECORDS_SHA.into(),
                Split::Test,
                &selections,
            )?;
            ensure!(
                subset_view.families == selected_families,
                "Subset ancestry differs from the original view"
            );
            let view_hash = save(
                &args.output_dir.join(format!("views/{key}.json")),
                &serde_json::to_value(&subset_view)?,
            )?;
            let binding = Binding {
                sha256: view_hash,
                records_sha256: RECORDS_SHA.into(),
                split: Split::Test,
                selected_rows: 54,
                source_groups: 18,
            };
            for detector in ["v5", "v6"] {
                let directory = evaluation.join(format!("{key}-{detector}"));
                let path = directory.join("report.json");
                let full = inputs.json(&path)?;
                let (artifact, threshold_hash, points, prior) = &detectors[detector];
                ensure!(
                    full["effective_training_class_prior"] == json!(prior),
                    "Training prior differs from the artifact"
                );
                let view_hash = sha256(fs::read(&view_path)?);
                let context = ExportContext {
                    view: &view,
                    view_hash: &view_hash,
                    archive: &archive,
                    artifact,
                    threshold_hash,
                    points,
                };
                let full_rows = verify_export(&mut inputs, &directory, &full, &context)?;
                let rows = select_rows(&full_rows, &view.families, &subset_view.families)?;
                let subset = derive_report(&full, &rows, &binding, points, &inputs.0[&path])?;
                save(
                    &args
                        .output_dir
                        .join(format!("reports/{key}-{detector}.json")),
                    &subset,
                )?;
            }
            let comparison_path = args.output_dir.join(format!("{key}-comparison.json"));
            let exit = Command::new(&compare)
                .arg("--baseline")
                .arg(args.output_dir.join(format!("reports/{key}-v5.json")))
                .arg("--candidate")
                .arg(args.output_dir.join(format!("reports/{key}-v6.json")))
                .arg("--output")
                .arg(&comparison_path)
                .stdout(fs::File::create_new(
                    args.output_dir.join(format!("{key}-comparison.stdout")),
                )?)
                .stderr(fs::File::create_new(
                    args.output_dir.join(format!("{key}-comparison.stderr")),
                )?)
                .status()?;
            ensure!(
                exit.success(),
                "Frozen paired comparison failed; partial files retained"
            );
            let mut paired: Value = serde_json::from_slice(&fs::read(&comparison_path)?)?;
            for field in ["baseline_near_cutoffs", "candidate_near_cutoffs"] {
                for point in paired[field]
                    .as_array_mut()
                    .context("Missing paired cutoff diagnostics")?
                {
                    point
                        .as_object_mut()
                        .context("Invalid cutoff object")?
                        .remove("ids");
                }
            }
            summaries.insert(key, paired);
        }
    }
    inputs.finish()?;
    save(
        &args.output_dir.join("input-bindings.json"),
        &json!({"inputs":inputs.0,"model_calls":0,"pangram_calls":0,"membership_sha256":MEMBERSHIP_SHA,"source_sha256":sha256(include_bytes!("subset_revision_reports.rs")),"executable_sha256":sha256(fs::read(std::env::current_exe()?)?)}),
    )?;
    save(
        &args.output_dir.join("summary.json"),
        &json!({"schema":"slop_ninja_revision_subset_comparisons_v1","selected_families":18,"unique_texts":162,"comparisons":summaries,"model_calls":0,"pangram_calls":0,"source_prediction_exports_preserved":true,"limits":"Six stage views share human and edited texts. These are not independent sample expansions. The 18 complete paired families cannot establish rare population false-positive rates or Pangram parity."}),
    )?;
    println!("Derived twelve subset reports and six paired comparisons without inference.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use evaluation_view::{Chain, Composer, RecordRef, RevisionMode};
    fn family(group: &str) -> Family {
        let r = |label| RecordRef {
            id: format!("{group}-{label}"),
            text_sha256: sha256(format!("synthetic-{group}-{label}")),
        };
        Family {
            source_group: group.into(),
            human: r(0),
            model: r(1),
            mixed: r(2),
            chain: Chain {
                composer: Composer {
                    id: format!("{group}-1"),
                    model_id: "synthetic".into(),
                    model_revision: "synthetic-v1".into(),
                },
                initial_profile_id: None,
                revisions: vec![],
                revision_depth: 0,
                mode: RevisionMode::Unrevised,
            },
        }
    }
    fn rows() -> Vec<ProbabilityRow> {
        ["a", "b"]
            .into_iter()
            .flat_map(|g| {
                (0..3).map(move |label| ProbabilityRow {
                    id: format!("{g}-{label}"),
                    group: g.into(),
                    label,
                    probabilities: [0.2, 0.5, 0.3],
                })
            })
            .collect()
    }
    #[test]
    fn subset_rejects_missing_duplicate_and_mislabelled_full_predictions() {
        let families = [family("a"), family("b")];
        let r = rows();
        assert_eq!(select_rows(&r, &families, &families[1..]).unwrap().len(), 3);
        assert!(select_rows(&r[..5], &families, &families[1..]).is_err());
        for (field, value) in [("id", "a-0"), ("group", "wrong")] {
            let mut bad = r.clone();
            if field == "id" {
                bad[1].id = value.into();
            } else {
                bad[1].group = value.into();
            }
            assert!(select_rows(&bad, &families, &families[1..]).is_err());
        }
        let mut bad = r;
        bad[1].label = 0;
        assert!(select_rows(&bad, &families, &families[1..]).is_err());
    }
    #[test]
    fn derived_metrics_recompute_denominators_and_keep_strict_thresholds() {
        let point = OperatingPoint {
            target_human_false_positive_rate: 0.01,
            threshold: 0.8,
            comparison: "strictly_greater_than".into(),
            calibration_human_rows: 100,
            calibration_human_groups: 100,
            calibration_false_positives: 1,
        };
        let mut r = rows()[3..].to_vec();
        r[0].probabilities = [0.1, 0.6, 0.3];
        let prior = [1.0 / 3.0; 3];
        let full = json!({"artifact_id":sha256("synthetic"),"thresholds_sha256":sha256("frozen thresholds"),"effective_training_class_prior":prior,"report":{"model":{"rows":999}},"evaluation_view":{"source_groups":2},"near_cutoffs":[{"diagnostic_margin":0.002,"threshold":0.8}]});
        let binding = Binding {
            sha256: sha256("subset view"),
            records_sha256: sha256("subset archive"),
            split: Split::Test,
            selected_rows: 3,
            source_groups: 1,
        };
        let derived =
            derive_report(&full, &r, &binding, &[point], &sha256("source report")).unwrap();
        assert_eq!(derived["report"]["model"]["rows"], 3);
        assert_eq!(derived["human_model"]["rows"], 2);
        assert_eq!(
            derived["report"]["operating_points"][0]["human_false_positive_rate"]["events"],
            1
        );
        assert_eq!(
            derived["report"]["operating_points"][0]["model_only_sensitivity"]["events"],
            0
        );
        assert_eq!(derived["evaluation_view"]["selected_rows"], 3);
        assert_eq!(derived["derivation"]["additional_inference"], false);
        assert_eq!(
            derived["report"]["operating_points"][0]["calibration"]["threshold"],
            0.8
        );
    }
}
