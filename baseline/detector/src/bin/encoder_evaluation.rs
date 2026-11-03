//! Frozen CPU encoder evaluation with separately bound calibration thresholds.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{Evidence, Origin, OriginRecord, Split, sha256},
    evaluation_view::{self, Binding, Family},
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

#[derive(Clone, Copy, ValueEnum)]
enum Partition {
    Development,
    Test,
}

#[derive(Subcommand)]
enum Task {
    /// Freeze decision thresholds from the artifact's original Calibration shard.
    FreezeThresholds {
        #[arg(long)]
        calibration_jsonl: PathBuf,
        /// Frozen Calibration observation view required by view-trained artifacts.
        #[arg(long)]
        calibration_view: Option<PathBuf>,
        /// Reuse an archived calibrated export instead of recomputing its scores.
        #[arg(long, requires = "expected_predictions_sha256")]
        calibration_predictions: Option<PathBuf>,
        #[arg(long, requires = "calibration_predictions")]
        expected_predictions_sha256: Option<String>,
    },
    /// Evaluate a partition using an existing, artifact-bound threshold file.
    Evaluate {
        #[arg(long)]
        records: PathBuf,
        #[arg(long)]
        thresholds: PathBuf,
        #[arg(long, value_enum)]
        split: Partition,
        #[arg(long)]
        open_final_test: bool,
        /// Frozen trio selection over the complete validated ancestry archive.
        #[arg(long)]
        evaluation_view: Option<PathBuf>,
        /// Diagnose FPR on every human root, regardless of model-generation completion.
        #[arg(long, conflicts_with = "evaluation_view")]
        human_roots_only: bool,
    },
}

#[derive(Parser)]
struct Args {
    #[arg(long)]
    artifact: PathBuf,
    #[arg(long)]
    python: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
    #[command(subcommand)]
    task: Task,
}

fn save(path: &Path, value: &Value) -> Result<()> {
    let mut file = fs::File::create_new(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn object(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn bind_predictions(
    records: &[OriginRecord],
    outputs: &[Value],
    artifact_id: &str,
) -> Result<Vec<ProbabilityRow>> {
    let by_id: BTreeMap<_, _> = records.iter().map(|r| (r.id.as_str(), r)).collect();
    ensure!(by_id.len() == records.len(), "Duplicate input ID");
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for output in outputs {
        let id = output["id"].as_str().context("Missing prediction ID")?;
        let record = by_id.get(id).context("Unexpected prediction ID")?;
        ensure!(
            seen.insert(id)
                && output["artifact_id"] == artifact_id
                && output["status"] == "ok"
                && output["labels"] == json!(CLASS_NAMES),
            "Prediction coverage, artifact, status or class order differs"
        );
        rows.push(ProbabilityRow {
            id: record.id.clone(),
            group: record.source_group.clone(),
            label: record.origin.index(),
            probabilities: serde_json::from_value(output["probabilities"].clone())?,
        });
    }
    ensure!(rows.len() == records.len(), "Missing predictions");
    metrics::summarize(&rows)?;
    Ok(rows)
}

fn infer(args: &Args, records: &[OriginRecord], artifact_id: &str) -> Result<Vec<ProbabilityRow>> {
    let input = args.output_dir.join("requests.jsonl");
    let mut file = fs::File::create_new(&input)?;
    for record in records {
        // No labels, grouping or source metadata are sent to the detector.
        serde_json::to_writer(&mut file, &json!({"id":record.id,"text":record.text}))?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    drop(file);
    let output = args.output_dir.join("responses.jsonl");
    let status = Command::new(&args.python)
        .arg(args.artifact.join("runner/infer.py"))
        .arg("--artifact")
        .arg(&args.artifact)
        .env("HF_HUB_OFFLINE", "1")
        .env("TRANSFORMERS_OFFLINE", "1")
        .stdin(fs::File::open(&input)?)
        .stdout(fs::File::create_new(&output)?)
        .stderr(fs::File::create_new(args.output_dir.join("inference.log"))?)
        .status()?;
    ensure!(
        status.success(),
        "Bundled CPU inference failed; partial outputs retained"
    );
    let values = fs::read_to_string(&output)?
        .lines()
        .map(serde_json::from_str)
        .collect::<serde_json::Result<Vec<Value>>>()?;
    let rows = bind_predictions(records, &values, artifact_id)?;
    save(
        &args.output_dir.join("inference-bindings.json"),
        &json!({
            "artifact_id":artifact_id,"requests_sha256":sha256(fs::read(input)?),
            "responses_sha256":sha256(fs::read(output)?),
            "records":records.iter().map(|r| json!({"id":r.id,"text_sha256":r.text_sha256})).collect::<Vec<_>>()
        }),
    )?;
    Ok(rows)
}

fn import_calibration(
    args: &Args,
    records: &[OriginRecord],
    artifact_id: &str,
    path: &Path,
    expected_hash: &str,
) -> Result<Vec<ProbabilityRow>> {
    let bytes = fs::read(path)?;
    ensure!(
        sha256(&bytes) == expected_hash,
        "Archived prediction hash differs"
    );
    let expected: BTreeMap<_, _> = records
        .iter()
        .map(|r| (r.id.as_str(), r.text_sha256.as_str()))
        .collect();
    let mut values = Vec::new();
    for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let mut value: Value = serde_json::from_slice(line)?;
        let id = value["id"]
            .as_str()
            .context("Missing archived prediction ID")?;
        ensure!(
            value["text_sha256"]
                == *expected
                    .get(id)
                    .context("Unexpected archived prediction ID")?,
            "Archived prediction text differs from the original Calibration shard"
        );
        value["labels"] = value["classes"].clone();
        values.push(value);
    }
    let rows = bind_predictions(records, &values, artifact_id)?;
    fs::File::create_new(args.output_dir.join("archived-predictions.jsonl"))?.write_all(&bytes)?;
    save(
        &args.output_dir.join("inference-bindings.json"),
        &json!({
            "mode":"archived_calibrated_cpu_export","artifact_id":artifact_id,
            "predictions_sha256":expected_hash,"recomputed":false,
            "records":records.iter().map(|r| json!({"id":r.id,"text_sha256":r.text_sha256})).collect::<Vec<_>>()
        }),
    )?;
    Ok(rows)
}

fn training_prior(training: &Value) -> Result<[f64; 3]> {
    if training["sample_weight_policy"] == "source_origin"
        || training["class_weight_policy"] == "inverse_frequency"
    {
        return Ok([1.0 / 3.0; 3]);
    }
    let counts: [usize; 3] = serde_json::from_value(training["class_counts"]["train"].clone())?;
    let total = counts.iter().sum::<usize>();
    ensure!(total > 0, "Empty training prior");
    Ok(counts.map(|n| n as f64 / total as f64))
}

fn validate_calibration_view(training: &Value, actual: Option<&Value>) -> Result<()> {
    let expected = training
        .get("evaluation_views")
        .map(|views| &views["calibration"]);
    match (expected, actual) {
        (None, None) => Ok(()),
        (Some(expected), Some(actual)) => {
            let binding: Binding = serde_json::from_value(expected.clone())?;
            ensure!(
                binding.split == Split::Calibration
                    && binding.source_groups > 0
                    && binding.source_groups.checked_mul(3) == Some(binding.selected_rows)
                    && binding.records_sha256 == training["partition_sha256"]["calibration"],
                "Invalid training Calibration view binding"
            );
            ensure!(
                [&binding.sha256, &binding.records_sha256]
                    .iter()
                    .all(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())),
                "Invalid training Calibration view SHA256"
            );
            ensure!(
                expected == actual,
                "Calibration view differs from the artifact's frozen training view"
            );
            Ok(())
        }
        _ => anyhow::bail!(
            "Calibration view must be supplied exactly when bound by the artifact's training metadata"
        ),
    }
}

fn check_calibration_overlap(archive: &[OriginRecord], content: &Value) -> Result<()> {
    let cal_ids: BTreeSet<String> = serde_json::from_value(content["calibration_ids"].clone())?;
    let cal_groups: BTreeSet<String> =
        serde_json::from_value(content["calibration_groups"].clone())?;
    let cal_hashes: BTreeSet<String> =
        serde_json::from_value(content["calibration_text_hashes"].clone())?;
    ensure!(
        archive.iter().all(|r| !cal_ids.contains(&r.id)
            && !cal_groups.contains(&r.source_group)
            && !cal_hashes.contains(&r.text_sha256)),
        "Evaluation archive overlaps the model's calibration IDs, families or exact texts"
    );
    Ok(())
}

fn select_human_roots(archive: &[OriginRecord]) -> Result<Vec<OriginRecord>> {
    let groups: BTreeSet<_> = archive
        .iter()
        .map(|record| record.source_group.as_str())
        .collect();
    let mut roots = BTreeMap::new();
    for record in archive
        .iter()
        .filter(|record| record.origin == Origin::HumanOnly)
    {
        ensure!(
            record.parent_id.is_none()
                && record.generation.is_none()
                && matches!(
                    record.evidence,
                    Evidence::HistoricalProxy | Evidence::DocumentedHuman
                ),
            "Human-root diagnostics require historical or documented human roots without generation metadata"
        );
        ensure!(
            roots
                .insert(record.source_group.as_str(), record.clone())
                .is_none(),
            "Human-root diagnostics require exactly one human root per source family"
        );
    }
    ensure!(
        !groups.is_empty() && roots.keys().copied().collect::<BTreeSet<_>>() == groups,
        "Human-root diagnostics require one human root for every family in the archive partition"
    );
    Ok(roots.into_values().collect())
}

fn view_slice_keys(family: &Family, collection: &str) -> Result<Vec<String>> {
    let chain = &family.chain;
    let composer = &chain.composer.model_revision;
    let selected_model = chain
        .revisions
        .last()
        .map(|r| r.model_revision.as_str())
        .unwrap_or(composer);
    let profile = chain
        .initial_profile_id
        .as_deref()
        .unwrap_or("legacy-unprofiled");
    let path: Vec<_> = std::iter::once(composer.as_str())
        .chain(chain.revisions.iter().map(|r| r.model_revision.as_str()))
        .collect();
    Ok(vec![
        format!("collection:{collection}"),
        format!("composer:{composer}"),
        format!("selected-model:{selected_model}"),
        format!("initial-profile:{profile}"),
        format!("composer-profile:{composer}|{profile}"),
        format!("revision-depth:{}", chain.revision_depth),
        format!("revision-mode:{}", chain.mode.as_str()),
        format!("revision-path:{}", serde_json::to_string(&path)?),
    ])
}

fn main() -> Result<()> {
    let args = Args::parse();
    if matches!(
        args.task,
        Task::Evaluate {
            split: Partition::Test,
            open_final_test: false,
            ..
        }
    ) {
        anyhow::bail!("Final Test remains sealed; explicitly freeze selection before opening it");
    }
    ensure!(!args.output_dir.exists(), "Use a new evaluation directory");
    let output_parent = args
        .output_dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure!(
        !output_parent
            .canonicalize()
            .context("Evaluation parent directory must already exist")?
            .starts_with(args.artifact.canonicalize()?),
        "Evaluation outputs must stay outside the immutable artifact"
    );
    let manifest = object(&args.artifact.join("manifest.json"))?;
    let artifact_id = manifest["artifact_id"]
        .as_str()
        .context("Missing artifact ID")?;
    let training_path = args.artifact.join("training.json");
    let training_hash = sha256(fs::read(&training_path)?);
    ensure!(
        manifest["files"]["training.json"] == training_hash,
        "Training manifest differs"
    );
    let training = object(&training_path)?;
    let prior = training_prior(&training)?;
    match &args.task {
        Task::FreezeThresholds {
            calibration_jsonl,
            calibration_view,
            calibration_predictions,
            expected_predictions_sha256,
        } => {
            let (records, hash) = evaluation_view::read_archive(calibration_jsonl)?;
            ensure!(
                training["partition_sha256"]["calibration"] == hash,
                "Calibration input differs from the frozen artifact's original shard"
            );
            ensure!(
                records.iter().all(|r| r.split == Some(Split::Calibration)),
                "Unexpected Calibration split"
            );
            let view = calibration_view
                .as_ref()
                .map(|path| evaluation_view::load_view(path, calibration_jsonl, Split::Calibration))
                .transpose()?;
            let view_binding = view
                .as_ref()
                .map(|v| serde_json::to_value(v.binding()))
                .transpose()?;
            validate_calibration_view(&training, view_binding.as_ref())?;
            let observations = view
                .as_ref()
                .map_or(records.as_slice(), |v| v.selected.as_slice());
            fs::create_dir_all(&args.output_dir)?;
            let rows = match calibration_predictions {
                Some(path) => import_calibration(
                    &args,
                    observations,
                    artifact_id,
                    path,
                    expected_predictions_sha256
                        .as_deref()
                        .context("Missing archived prediction hash")?,
                )?,
                None => infer(&args, observations, artifact_id)?,
            };
            let points = metrics::select_operating_points(&rows, &[0.01, 0.05])?;
            let mut content = json!({"schema":"slop_ninja_encoder_thresholds_v1",
                "artifact_id":artifact_id,"training_sha256":training_hash,"calibration_shard_sha256":hash,
                "inference_bindings_sha256":sha256(fs::read(args.output_dir.join("inference-bindings.json"))?),
                "calibration_ids":records.iter().map(|r| &r.id).collect::<Vec<_>>(),
                "calibration_groups":records.iter().map(|r| &r.source_group).collect::<BTreeSet<_>>(),
                "calibration_text_hashes":records.iter().map(|r| &r.text_sha256).collect::<BTreeSet<_>>(),
                "effective_training_class_prior":prior,"points":points,
                "temperature_refitted":false,"test_predictions_opened":false});
            if let Some(binding) = view_binding {
                content["calibration_view"] = binding;
            }
            save(
                &args.output_dir.join("thresholds.json"),
                &json!({
                "content_sha256":sha256(serde_json::to_vec(&content)?),"content":content}),
            )?;
            println!("{}", serde_json::to_string_pretty(&points)?);
        }
        Task::Evaluate {
            records,
            thresholds,
            split,
            evaluation_view: view_path,
            human_roots_only,
            ..
        } => {
            ensure!(
                training.get("evaluation_views").is_none()
                    || view_path.is_some()
                    || *human_roots_only,
                "View-trained artifacts require --evaluation-view or the explicit --human-roots-only diagnostic"
            );
            let frozen = object(thresholds)?;
            let content = &frozen["content"];
            ensure!(
                frozen["content_sha256"] == sha256(serde_json::to_vec(content)?)
                    && content["schema"] == "slop_ninja_encoder_thresholds_v1"
                    && content["artifact_id"] == artifact_id
                    && content["training_sha256"] == training_hash
                    && content["calibration_shard_sha256"]
                        == training["partition_sha256"]["calibration"],
                "Threshold file is not bound to this artifact and its original calibration"
            );
            validate_calibration_view(&training, content.get("calibration_view"))?;
            let points: Vec<OperatingPoint> = serde_json::from_value(content["points"].clone())?;
            ensure!(
                points.len() == 2,
                "Expected the two declared operating points"
            );
            for (point, rate) in points.iter().zip([0.01, 0.05]) {
                point.validate()?;
                ensure!(
                    point.target_human_false_positive_rate == rate,
                    "Unexpected target FPR"
                );
            }
            let selected_split = match split {
                Partition::Development => Split::Development,
                Partition::Test => Split::Test,
            };
            let view = view_path
                .as_ref()
                .map(|path| evaluation_view::load_view(path, records, selected_split))
                .transpose()?;
            let (archive, records_hash) = if let Some(view) = &view {
                (view.archive.clone(), view.view.records_sha256.clone())
            } else {
                let (all_records, hash) = evaluation_view::read_archive(records)?;
                (
                    all_records
                        .into_iter()
                        .filter(|r| r.split == Some(selected_split))
                        .collect(),
                    hash,
                )
            };
            ensure!(!archive.is_empty(), "Empty evaluation partition");
            check_calibration_overlap(&archive, content)?;
            let view_binding = view
                .as_ref()
                .map(|v| serde_json::to_value(v.binding()))
                .transpose()?;
            let human_roots = human_roots_only
                .then(|| select_human_roots(&archive))
                .transpose()?;
            let selected = human_roots.as_deref().unwrap_or_else(|| {
                view.as_ref()
                    .map_or(archive.as_slice(), |v| v.selected.as_slice())
            });
            let mut providers = BTreeMap::<String, BTreeSet<String>>::new();
            let mut profiles = BTreeMap::<String, BTreeSet<String>>::new();
            for record in selected {
                if let Some(generation) = &record.generation {
                    providers
                        .entry(record.source_group.clone())
                        .or_default()
                        .insert(generation.model_revision.clone());
                    profiles
                        .entry(record.source_group.clone())
                        .or_default()
                        .insert(
                            generation
                                .prompt_profile
                                .as_ref()
                                .map(|p| p.profile.id.clone())
                                .unwrap_or_else(|| "legacy-unprofiled".into()),
                        );
                }
            }
            ensure!(
                *human_roots_only
                    || view.is_some()
                    || selected.iter().all(|r| providers
                        .get(&r.source_group)
                        .is_some_and(|p| p.len() == 1)
                        && profiles.get(&r.source_group).is_some_and(|p| p.len() == 1)),
                "Evaluation requires one declared generator/profile pair per source family"
            );
            fs::create_dir_all(&args.output_dir)?;
            let mut inputs = json!({
                "records_sha256":records_hash,"thresholds_sha256":sha256(fs::read(thresholds)?),
                "artifact_id":artifact_id,"split":selected_split,
                "final_test_opened":matches!(split, Partition::Test)});
            if let Some(binding) = &view_binding {
                inputs["evaluation_view"] = binding.clone();
            }
            if *human_roots_only {
                inputs["observation_policy"] = json!("all_human_roots_v1");
            }
            save(&args.output_dir.join("inputs.json"), &inputs)?;
            let rows = infer(&args, selected, artifact_id)?;
            let by_id: BTreeMap<_, _> = selected.iter().map(|r| (r.id.as_str(), r)).collect();
            let view_families: BTreeMap<_, _> = view
                .iter()
                .flat_map(|v| &v.view.families)
                .map(|f| (f.source_group.as_str(), f))
                .collect();
            let mut slices = BTreeMap::<String, Vec<ProbabilityRow>>::new();
            for row in &rows {
                let record = by_id[row.id.as_str()];
                let keys = if *human_roots_only {
                    vec![format!("collection:{}", record.source.collection)]
                } else if let Some(family) = view_families.get(record.source_group.as_str()) {
                    view_slice_keys(family, &record.source.collection)?
                } else {
                    let provider = providers[&record.source_group].iter().next().unwrap();
                    let profile = profiles[&record.source_group].iter().next().unwrap();
                    vec![
                        format!("provider:{provider}"),
                        format!("collection:{}", record.source.collection),
                        format!("profile:{profile}"),
                        format!("provider-profile:{provider}|{profile}"),
                    ]
                };
                for key in keys {
                    slices.entry(key).or_default().push(row.clone());
                }
            }
            let mut slice_reports = BTreeMap::new();
            let mut binary_slice_reports = BTreeMap::new();
            for (key, slice_rows) in slices {
                binary_slice_reports
                    .insert(key.clone(), metrics::summarize_human_model(&slice_rows)?);
                slice_reports.insert(
                    key,
                    metrics::evaluate_probability_rows(artifact_id, &slice_rows, prior, &points)?,
                );
            }
            let report = metrics::evaluate_probability_rows(artifact_id, &rows, prior, &points)?;
            let tolerance = manifest["reference_runtime"]["probability_absolute_tolerance"]
                .as_f64()
                .context("Missing reference probability tolerance")?;
            ensure!(
                tolerance.is_finite() && tolerance > 0.0,
                "Invalid reference tolerance"
            );
            let near_cutoffs: Vec<_> = points.iter().map(|point| {
                let near: Vec<_> = rows.iter().filter(|r|
                    (r.probabilities[1] + r.probabilities[2] - point.threshold).abs() <= 2.0 * tolerance).collect();
                json!({"target_human_false_positive_rate":point.target_human_false_positive_rate,
                    "threshold":point.threshold,"diagnostic_margin":2.0*tolerance,"rows":near.len(),
                    "human_rows":near.iter().filter(|r| r.label == 0).count(),
                    "ids":near.iter().map(|r| &r.id).collect::<Vec<_>>()})
            }).collect();
            let mut output_report = json!({
                "schema":"slop_ninja_frozen_encoder_evaluation_v1","artifact_id":artifact_id,
                "records_sha256":records_hash,"split":selected_split,
                "thresholds_sha256":sha256(fs::read(thresholds)?),"effective_training_class_prior":prior,
                "final_test_opened":matches!(split, Partition::Test),"report":report,"slices":slice_reports,
                "human_model":metrics::summarize_human_model(&rows)?,"human_model_slices":binary_slice_reports,
                "near_cutoffs":near_cutoffs,
                "notes":["Each detector retains its own original Calibration shard and frozen temperature; threshold files must exist before evaluation.",
                    "Provider slices include the corresponding human roots selected with that provider's draft/edit pair.",
                    "Human/model metrics exclude mixed-origin rows and use P(model_only)+P(mixed); accuracy_at_half is diagnostic, separate from frozen operating thresholds. Profile slices include the corresponding human roots; legacy rows retain an explicit unprofiled category.",
                    "Effective training-prior control accounts for source-origin or inverse-frequency loss weights.",
                    "Different frozen operating points do not prove superiority at matched population FPR.",
                    "Cutoff proximity uses twice the per-class reference-vector tolerance as a diagnostic margin, not a proven bound on cross-platform drift for every text."]});
            if let Some(binding) = view_binding {
                output_report["evaluation_view"] = binding;
                output_report["notes"][1] = json!(
                    "View slices pair each selected human root and direct human edit with the explicitly selected model stage. All ancestry records are validated and retained in overlap guards."
                );
                output_report["notes"][2] = json!(
                    "Human/model metrics exclude mixed-origin rows and use P(model_only)+P(mixed); accuracy_at_half is diagnostic. Initial-profile slices use only the original composer's recorded profile, with an explicit unprofiled category; no revision style is inferred. Revision mode compares exact pinned model revisions along the ordered ancestry path."
                );
            }
            if *human_roots_only {
                output_report["observation_policy"] = json!("all_human_roots_v1");
                output_report["notes"][1] = json!(
                    "Human-only diagnostic: every historical or documented human root in the declared archive partition is included, independent of model-generation completion. Full archive ancestry and Calibration overlap checks precede selection; slices are by source collection only."
                );
                output_report["notes"][2] = json!(
                    "No model or mixed-origin observations are scored in this diagnostic. Sensitivity has a zero denominator and no estimate; human false-positive rates use the existing frozen thresholds. Human/model log loss and Brier score describe only human examples here."
                );
            }
            save(&args.output_dir.join("report.json"), &output_report)?;
            println!("{}", serde_json::to_string_pretty(&report.model)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_detector::dataset;

    fn binding() -> Value {
        json!({"sha256":sha256("view"),"records_sha256":sha256("archive"),"split":"calibration","selected_rows":6,"source_groups":2})
    }

    #[test]
    fn calibration_views_are_required_and_exactly_bound_with_legacy_default() {
        let legacy = json!({"partition_sha256":{"calibration":sha256("archive")}});
        assert!(validate_calibration_view(&legacy, None).is_ok());
        assert!(validate_calibration_view(&legacy, Some(&binding())).is_err());
        let mut training = legacy;
        training["evaluation_views"] = json!({"calibration":binding()});
        assert!(validate_calibration_view(&training, Some(&binding())).is_ok());
        assert!(validate_calibration_view(&training, None).is_err());
        let mut changed = binding();
        changed["sha256"] = json!(sha256("another view"));
        assert!(validate_calibration_view(&training, Some(&changed)).is_err());
        for (key, value) in [
            ("split", json!("development")),
            ("source_groups", json!(0)),
            ("selected_rows", json!(5)),
            ("records_sha256", json!(sha256("another archive"))),
            ("sha256", json!("bad")),
        ] {
            let mut invalid = binding();
            invalid[key] = value;
            training["evaluation_views"]["calibration"] = invalid.clone();
            assert!(validate_calibration_view(&training, Some(&invalid)).is_err());
        }
        training["evaluation_views"] = Value::Null;
        assert!(validate_calibration_view(&training, None).is_err());
    }

    #[test]
    fn overlap_guard_includes_unselected_ancestors() {
        // Only identity fields enter this guard; the archive reader validates the records first.
        let record: OriginRecord = serde_json::from_value(json!({
            "schema":dataset::RECORD_SCHEMA,"id":"unselected-composer","source_group":"new-family",
            "split":"test","origin":"model_only","evidence":"synthetic_fixture","evidence_notes":"Invented guard fixture",
            "text":"Invented ancestor text.","text_sha256":sha256("Invented ancestor text."),
            "source":{"collection":"fixture","url":"fixture://root","version":"v1","published_at":null,"author_ids":[],"raw_path":"","raw_sha256":sha256("raw"),"extraction":"fixture"},
            "rights":{"license":"Synthetic-Original","evidence_url":"fixture://rights","evidence_sha256":sha256("rights"),"attribution":"Synthetic fixture","commercial_training":true,"model_release":true,"external_evaluation":false,"redistribute_text":true},
            "parent_id":null,"generation":null
        })).unwrap();
        let empty =
            json!({"calibration_ids":[],"calibration_groups":[],"calibration_text_hashes":[]});
        assert!(check_calibration_overlap(std::slice::from_ref(&record), &empty).is_ok());
        for (field, value) in [
            ("calibration_ids", record.id.clone()),
            ("calibration_groups", record.source_group.clone()),
            ("calibration_text_hashes", record.text_sha256.clone()),
        ] {
            let mut exclusions = empty.clone();
            exclusions[field] = json!([value]);
            assert!(check_calibration_overlap(std::slice::from_ref(&record), &exclusions).is_err());
        }
    }

    #[test]
    fn chain_slices_use_recorded_initial_profile_and_explicit_unrevised_mode() {
        let family: Family = serde_json::from_value(json!({
            "source_group":"fixture-family","human":{"id":"human","text_sha256":sha256("human")},
            "model":{"id":"draft","text_sha256":sha256("draft")},"mixed":{"id":"edit","text_sha256":sha256("edit")},
            "chain":{"composer":{"id":"draft","model_id":"composer","model_revision":"composer@pinned"},
                "initial_profile_id":null,"revisions":[],"revision_depth":0,"mode":"unrevised"}
        })).unwrap();
        let keys = view_slice_keys(&family, "fixture").unwrap();
        assert!(keys.contains(&"revision-mode:unrevised".into()));
        assert!(keys.contains(&"revision-depth:0".into()));
        assert!(keys.contains(&"initial-profile:legacy-unprofiled".into()));
        assert!(keys.contains(&"revision-path:[\"composer@pinned\"]".into()));
        assert!(!keys.iter().any(|key| key.starts_with("revision-style:")));
    }

    fn human_fixture(id: &str) -> OriginRecord {
        serde_json::from_value(json!({
            "schema":dataset::RECORD_SCHEMA,"id":id,"source_group":id,"split":"development",
            "origin":"human_only","evidence":"historical_proxy","evidence_notes":"Invented diagnostic fixture; not authorship evidence.",
            "text":format!("A paper moon marks fixture {id}."),"text_sha256":sha256(format!("A paper moon marks fixture {id}.")),
            "source":{"collection":"fixture","url":"fixture://root","version":"v1","published_at":null,"author_ids":[],"raw_path":"","raw_sha256":sha256("raw"),"extraction":"fixture"},
            "rights":{"license":"Synthetic-Original","evidence_url":"fixture://rights","evidence_sha256":sha256("rights"),"attribution":"Synthetic fixture","commercial_training":true,"model_release":true,"external_evaluation":false,"redistribute_text":true},
            "parent_id":null,"generation":null
        })).unwrap()
    }

    #[test]
    fn human_root_diagnostic_keeps_families_without_completed_model_branches() {
        let successful = human_fixture("successful-generation");
        let failed = human_fixture("failed-generation");
        let mut model = successful.clone();
        model.id = "synthetic-model-child".into();
        model.parent_id = Some(successful.id.clone());
        model.origin = Origin::ModelOnly;
        model.evidence = Evidence::SyntheticFixture;
        let archive = vec![successful, model, failed];
        dataset::validate_records(&archive).unwrap();
        let selected = select_human_roots(&archive).unwrap();
        assert_eq!(
            selected.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["failed-generation", "successful-generation"]
        );
        assert!(selected.iter().all(|r| r.origin == Origin::HumanOnly));
        let mut duplicate = archive.clone();
        let mut second_root = duplicate[0].clone();
        second_root.id = "second-root-same-family".into();
        duplicate.push(second_root);
        assert!(select_human_roots(&duplicate).is_err());
        assert!(select_human_roots(&archive[1..]).is_err());
        let mut synthetic = archive;
        synthetic[0].evidence = Evidence::SyntheticFixture;
        assert!(select_human_roots(&synthetic).is_err());
    }

    #[test]
    fn human_only_operating_points_have_no_sensitivity_estimate() {
        let rows = vec![
            ProbabilityRow {
                id: "one".into(),
                group: "one".into(),
                label: 0,
                probabilities: [0.8, 0.1, 0.1],
            },
            ProbabilityRow {
                id: "two".into(),
                group: "two".into(),
                label: 0,
                probabilities: [0.3, 0.4, 0.3],
            },
        ];
        let point = OperatingPoint {
            target_human_false_positive_rate: 0.01,
            threshold: 0.5,
            comparison: "strictly_greater_than".into(),
            calibration_human_rows: 100,
            calibration_human_groups: 100,
            calibration_false_positives: 1,
        };
        let report =
            metrics::evaluate_probability_rows("fixture", &rows, [1.0 / 3.0; 3], &[point]).unwrap();
        let result = &report.operating_points[0];
        assert_eq!(result.human_false_positive_rate.rate, Some(0.5));
        for rate in [
            &result.model_only_sensitivity,
            &result.mixed_sensitivity,
            &result.model_or_mixed_sensitivity,
        ] {
            assert_eq!(rate.rows, 0);
            assert!(rate.rate.is_none());
            assert!(rate.group_bootstrap_percentile_95.is_none());
        }
    }

    #[test]
    fn human_root_flag_is_explicit_and_conflicts_with_trio_views() {
        let base = [
            "encoder_evaluation",
            "--artifact",
            "fixture-artifact",
            "--python",
            "fixture-python",
            "--output-dir",
            "fixture-output",
            "evaluate",
            "--records",
            "fixture-records",
            "--thresholds",
            "fixture-thresholds",
            "--split",
            "development",
        ];
        let parse =
            |extra: &[&str]| Args::try_parse_from(base.into_iter().chain(extra.iter().copied()));
        assert!(matches!(
            parse(&[]).unwrap().task,
            Task::Evaluate {
                human_roots_only: false,
                ..
            }
        ));
        assert!(matches!(
            parse(&["--human-roots-only"]).unwrap().task,
            Task::Evaluate {
                human_roots_only: true,
                ..
            }
        ));
        assert!(parse(&["--human-roots-only", "--evaluation-view", "fixture-view"]).is_err());
    }
}
