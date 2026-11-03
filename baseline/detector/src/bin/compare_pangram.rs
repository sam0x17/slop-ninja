//! Frozen development comparison; provider fractions remain separate annotations.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::features::Features;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{self, Split},
    features::FeatureRow,
    metrics::{self, OperatingPoint, ProbabilityRow},
    model::DetectorArtifact,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    records: PathBuf,
    #[arg(long)]
    annotations: PathBuf,
    #[arg(long)]
    features: PathBuf,
    #[arg(long)]
    linear_artifact: PathBuf,
    #[arg(long)]
    encoder_artifact: PathBuf,
    #[arg(long)]
    encoder_operating_points: PathBuf,
    #[arg(long)]
    python: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
}
fn json_file(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn jsonl(path: &Path) -> Result<Vec<Value>> {
    fs::read(path)?
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| Ok(serde_json::from_slice(l)?))
        .collect()
}
fn save(path: &Path, value: &Value) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    Ok(())
}
fn strip_predictions(mut report: Value) -> Value {
    report.as_object_mut().unwrap().remove("predictions");
    report
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new comparison directory");
    let source_hash = dataset::sha256(fs::read(&args.records)?);
    ensure!(
        source_hash == "a5967f8f564d0f25fabb82ba3f9c90777ee094c781bfb9a32f712766b2e94bbf",
        "Expected frozen v2 Development records"
    );
    let records = dataset::read_records(&args.records)?;
    ensure!(
        records.len() == 57 && records.iter().all(|r| r.split == Some(Split::Development)),
        "Development cohort mismatch"
    );
    let by_id: BTreeMap<_, _> = records.iter().map(|r| (r.id.as_str(), r)).collect();
    let annotations = jsonl(&args.annotations)?;
    let mut observed = BTreeMap::new();
    let mut tasks = BTreeSet::new();
    let mut versions = BTreeSet::new();
    for annotation in &annotations {
        ensure!(
            annotation["schema"] == "slop_ninja_pangram_corpus_annotation_v1"
                && annotation["model_selector"] == "pangram-4",
            "Wrong annotation schema/provider"
        );
        let observation = &annotation["observation"];
        ensure!(
            observation["stage"] == "STAGE_SUCCESS",
            "Failed observations must be reported before comparison"
        );
        ensure!(
            tasks.insert(observation["task_id"].as_str().context("Missing task ID")?),
            "Duplicate task"
        );
        versions.insert(
            observation["result"]["version"]
                .as_str()
                .context("Missing version")?,
        );
        for member in annotation["member"]["records"]
            .as_array()
            .context("Missing record members")?
        {
            let id = member["record_id"].as_str().context("Missing record ID")?;
            let record = by_id.get(id).context("Unrequested annotation")?;
            ensure!(
                annotation["member"]["repeat_index"] == 0
                    && annotation["submitted_text_sha256"] == record.text_sha256
                    && member["origin"] == serde_json::to_value(record.origin)?
                    && member["source_group"] == record.source_group,
                "Annotation provenance mismatch"
            );
            let returned = observation["result"]["text"]
                .as_str()
                .context("Missing echoed text")?;
            ensure!(
                annotation["returned_text_sha256"] == dataset::sha256(returned)
                    && returned
                        .split_whitespace()
                        .eq(record.text.split_whitespace()),
                "Echo mismatch"
            );
            ensure!(
                observed.insert(id, annotation).is_none(),
                "Duplicate observation"
            );
        }
    }
    ensure!(observed.len() == records.len(), "Missing observations");
    let linear: DetectorArtifact = serde_json::from_slice(&fs::read(&args.linear_artifact)?)?;
    linear.validate()?;
    ensure!(
        linear.content_sha256 == "2dcc41316a8233095b2e2fb31c4265f6be9d706b862c1c6e7fae93600a5eedcc",
        "Wrong selected linear model"
    );
    ensure!(
        dataset::sha256(fs::read(&args.features)?)
            == "e2ff10f9c53a81333e6227aafdda3a93b962ebb33a313e469827e91ed01aa33c",
        "Wrong feature cache"
    );
    let mut feature_rows = Vec::new();
    let mut feature_ids = BTreeSet::new();
    for row in jsonl(&args.features)? {
        let id = row["id"].as_str().context("Missing feature ID")?;
        if let Some(record) = by_id.get(id) {
            ensure!(
                row["split"] == "development"
                    && row["text_sha256"] == record.text_sha256
                    && row["group"] == record.source_group,
                "Feature join mismatch"
            );
            let features: Features = serde_json::from_value(row["features"].clone())?;
            ensure!(
                row["features_sha256"] == dataset::sha256(serde_json::to_vec(&features)?),
                "Feature hash mismatch"
            );
            ensure!(feature_ids.insert(id.to_owned()), "Duplicate features");
            feature_rows.push(FeatureRow {
                id: id.into(),
                group: record.source_group.clone(),
                label: record.origin.index(),
                features,
            });
        }
    }
    ensure!(feature_rows.len() == records.len(), "Missing features");
    let linear_report = metrics::evaluate(&linear, &feature_rows)?;
    let encoder = json_file(&args.encoder_artifact.join("manifest.json"))?;
    let encoder_id = encoder["artifact_id"]
        .as_str()
        .context("Missing encoder ID")?;
    ensure!(
        encoder_id == "52f0054fc5b3c3f22d04a34fb1dcd9855e7296a0ff3ad943fc270741ccc3224e",
        "Wrong selected encoder"
    );
    let points = json_file(&args.encoder_operating_points)?;
    ensure!(
        points["artifact_id"] == encoder_id,
        "Operating-point artifact mismatch"
    );
    let operating_points: Vec<OperatingPoint> = serde_json::from_value(points["points"].clone())?;
    ensure!(
        operating_points.len() == 2
            && operating_points
                .iter()
                .all(|p| p.threshold == 0.8935751794569573
                    && p.comparison == "strictly_greater_than"
                    && p.calibration_human_rows == 14),
        "Frozen encoder operating points changed"
    );
    fs::create_dir_all(&args.output_dir)?;
    save(
        &args.output_dir.join("inputs.json"),
        &json!({"records_sha256":source_hash,"annotations_sha256":dataset::sha256(fs::read(&args.annotations)?),
        "features_sha256":dataset::sha256(fs::read(&args.features)?),"linear_id":linear.content_sha256,"encoder_id":encoder_id,
        "operating_points_sha256":dataset::sha256(fs::read(&args.encoder_operating_points)?),"pangram_flag_rule":"fraction_ai + fraction_ai_assisted >= 0.10"}),
    )?;
    let input_path = args.output_dir.join("inference-requests.jsonl");
    let mut input = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&input_path)?;
    for record in &records {
        serde_json::to_writer(&mut input, &json!({"id":record.id,"text":record.text}))?;
        input.write_all(b"\n")?;
    }
    input.sync_all()?;
    drop(input);
    let output_path = args.output_dir.join("encoder-predictions.jsonl");
    let output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&output_path)?;
    let log = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(args.output_dir.join("encoder.log"))?;
    eprintln!("Running frozen CPU encoder on 57 development texts");
    let status = Command::new(&args.python)
        .arg(args.encoder_artifact.join("runner/infer.py"))
        .arg("--artifact")
        .arg(&args.encoder_artifact)
        .stdin(Stdio::from(fs::File::open(input_path)?))
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(log))
        .status()?;
    ensure!(
        status.success(),
        "Encoder inference failed; retained log and partial output"
    );
    let mut encoder_predictions = Vec::new();
    let mut seen = BTreeSet::new();
    for prediction in jsonl(&output_path)? {
        let id = prediction["id"].as_str().context("Missing prediction ID")?;
        let record = by_id.get(id).context("Unknown prediction")?;
        ensure!(
            prediction["artifact_id"] == encoder_id
                && prediction["status"] == "ok"
                && prediction["labels"] == json!(["human_only", "model_only", "mixed"]),
            "Prediction identity/status mismatch"
        );
        ensure!(seen.insert(id.to_owned()), "Duplicate prediction");
        encoder_predictions.push(ProbabilityRow {
            id: id.into(),
            group: record.source_group.clone(),
            label: record.origin.index(),
            probabilities: serde_json::from_value(prediction["probabilities"].clone())?,
        });
    }
    ensure!(seen.len() == records.len(), "Missing encoder output");
    let encoder_report = metrics::evaluate_probability_rows(
        encoder_id,
        &encoder_predictions,
        [1.0 / 3.0; 3],
        &operating_points,
    )?;
    let mut slices = BTreeMap::<String, Value>::new();
    let mut native = BTreeMap::<String, usize>::new();
    let mut rows = Vec::new();
    for record in &records {
        let annotation = observed[record.id.as_str()];
        let result = &annotation["observation"]["result"];
        let fraction = result["fraction_ai"]
            .as_f64()
            .context("Missing AI fraction")?
            + result["fraction_ai_assisted"]
                .as_f64()
                .context("Missing assisted fraction")?;
        ensure!(
            fraction.is_finite() && (0.0..=1.0 + 1e-5).contains(&fraction),
            "Invalid flagged fraction"
        );
        let flagged = fraction >= 0.10;
        let origin = serde_json::to_value(record.origin)?
            .as_str()
            .unwrap()
            .to_owned();
        let echo = annotation["echo_match"]
            .as_str()
            .context("Missing echo match")?;
        *native
            .entry(
                result["prediction_short"]
                    .as_str()
                    .unwrap_or("missing")
                    .into(),
            )
            .or_default() += 1;
        let mut keys = vec![
            format!("origin/{origin}"),
            format!("collection/{}/{origin}", record.source.collection),
            format!("echo/{echo}/{origin}"),
        ];
        if let Some(generation) = &record.generation {
            keys.push(format!("generator/{}/{origin}", generation.model_revision));
            if let Some(profile) = &generation.prompt_profile {
                keys.push(format!("profile/{}/{origin}", profile.profile.id));
            }
        }
        for key in keys {
            let entry = slices
                .entry(key)
                .or_insert(json!({"rows":0,"flagged":0,"flagged_fraction_sum":0.0}));
            entry["rows"] = json!(entry["rows"].as_u64().unwrap() + 1);
            entry["flagged"] = json!(entry["flagged"].as_u64().unwrap() + u64::from(flagged));
            entry["flagged_fraction_sum"] =
                json!(entry["flagged_fraction_sum"].as_f64().unwrap() + fraction);
        }
        rows.push(
            json!({"id":record.id,"source_group":record.source_group,"origin":record.origin,
            "pangram_fraction":fraction,"flagged":flagged,"echo_match":echo}),
        );
    }
    for slice in slices.values_mut() {
        slice["mean_flagged_fraction"] = json!(
            slice["flagged_fraction_sum"].as_f64().unwrap() / slice["rows"].as_f64().unwrap()
        );
        slice
            .as_object_mut()
            .unwrap()
            .remove("flagged_fraction_sum");
    }
    save(&args.output_dir.join("pangram-rows.json"), &json!(rows))?;
    save(
        &args.output_dir.join("linear-full.json"),
        &serde_json::to_value(&linear_report)?,
    )?;
    save(
        &args.output_dir.join("encoder-full.json"),
        &serde_json::to_value(&encoder_report)?,
    )?;
    let summary = json!({"schema":"slop_ninja_pangram_development_comparison_v1","rows":records.len(),"source_groups":19,
        "provider_versions":versions,"pangram_threshold":0.10,"pangram_native_labels":native,"pangram_slices":slices,
        "linear":strip_predictions(serde_json::to_value(linear_report)?),"encoder":strip_predictions(serde_json::to_value(encoder_report)?),
        "limitations":["Previously used Development cohort, not fresh confirmation","Human and mixed labels are historical proxies","Pangram and local models use different frozen operating points; not a matched-FPR comparison","Pangram fractions are not origin probabilities"],
        "inputs_sha256":dataset::sha256(fs::read(args.output_dir.join("inputs.json"))?)});
    save(&args.output_dir.join("summary.json"), &summary)?;
    println!(
        "Comparison complete: {}",
        args.output_dir.join("summary.json").display()
    );
    Ok(())
}
