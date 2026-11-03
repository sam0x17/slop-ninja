//! Compare complete Pangram observations without averaging away failures or drift.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::PathBuf,
};

#[derive(Parser)]
struct Args {
    #[arg(long, required = true)]
    annotations: Vec<PathBuf>,
    #[arg(long)]
    output: PathBuf,
}
fn changed_paths(a: &Value, b: &Value, path: &str, paths: &mut BTreeSet<String>) {
    if a == b {
        return;
    }
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeSet<_> = a.keys().chain(b.keys()).collect();
            for key in keys {
                changed_paths(
                    a.get(key).unwrap_or(&Value::Null),
                    b.get(key).unwrap_or(&Value::Null),
                    &format!("{path}/{key}"),
                    paths,
                );
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (a, b) in a.iter().zip(b) {
                changed_paths(a, b, &format!("{path}/*"), paths);
            }
        }
        _ => {
            paths.insert(path.into());
        }
    }
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output.exists(), "Use a new output file");
    let mut groups = BTreeMap::<String, BTreeMap<u64, Value>>::new();
    let mut tasks = BTreeSet::new();
    let mut versions = BTreeSet::new();
    let mut files = Vec::new();
    for path in &args.annotations {
        let bytes = fs::read(path)?;
        files.push(json!({"sha256":sha256(&bytes)}));
        for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            let row: Value = serde_json::from_slice(line)?;
            ensure!(
                row["schema"] == "slop_ninja_pangram_corpus_annotation_v1"
                    && row["model_selector"] == "pangram-4",
                "Invalid annotation"
            );
            ensure!(
                row["observation"]["stage"] == "STAGE_SUCCESS",
                "Failed observation must remain visible; complete-repeat summary unavailable"
            );
            let hash = row["submitted_text_sha256"]
                .as_str()
                .context("Missing input hash")?
                .to_owned();
            ensure!(row["member"]["text_sha256"] == hash, "Member hash mismatch");
            let repeat = row["member"]["repeat_index"]
                .as_u64()
                .context("Missing repeat index")?;
            let task = row["observation"]["task_id"]
                .as_str()
                .context("Missing task")?
                .to_owned();
            ensure!(
                tasks.insert(task),
                "A provider task is reused across repeats"
            );
            versions.insert(
                row["observation"]["result"]["version"]
                    .as_str()
                    .context("Missing version")?
                    .to_owned(),
            );
            ensure!(
                groups
                    .entry(hash)
                    .or_default()
                    .insert(repeat, row)
                    .is_none(),
                "Duplicate text/repeat observation"
            );
        }
    }
    ensure!(groups.len() == 57, "Expected the complete 57-text cohort");
    ensure!(
        versions.len() == 1,
        "Provider versions differ; report drift separately"
    );
    let mut ranges = BTreeMap::<String, f64>::new();
    let mut full_identical = 0;
    let mut label_changes = 0;
    let mut threshold_crossings = 0;
    let mut paths = BTreeSet::new();
    let mut differing_docs = Vec::new();
    let mut echo_counts = BTreeMap::<String, usize>::new();
    for (hash, repeats) in &groups {
        ensure!(
            repeats.keys().copied().collect::<Vec<_>>() == [0, 1, 2],
            "Missing planned repeat"
        );
        let observations: Vec<_> = repeats.values().collect();
        for row in &observations {
            ensure!(
                row["member"]["records"] == observations[0]["member"]["records"],
                "Source membership changed"
            );
            let echo = row["echo_match"].as_str().context("Missing echo binding")?;
            ensure!(
                ["exact", "whitespace_only"].contains(&echo),
                "Unsupported text binding"
            );
            *echo_counts.entry(echo.into()).or_default() += 1;
        }
        let results: Vec<_> = observations
            .iter()
            .map(|r| &r["observation"]["result"])
            .collect();
        let identical = results.iter().all(|r| *r == results[0]);
        full_identical += usize::from(identical);
        if !identical {
            differing_docs.push(hash);
        }
        label_changes += usize::from(
            results
                .iter()
                .any(|r| r["prediction_short"] != results[0]["prediction_short"]),
        );
        for result in &results[1..] {
            changed_paths(results[0], result, "", &mut paths);
        }
        let mut flagged = Vec::new();
        for key in [
            "fraction_human",
            "fraction_ai",
            "fraction_ai_assisted",
            "flagged_content",
        ] {
            let values: Vec<f64> = results
                .iter()
                .map(|r| -> Result<f64> {
                    let value = if key == "flagged_content" {
                        r["fraction_ai"].as_f64().context("Missing AI fraction")?
                            + r["fraction_ai_assisted"]
                                .as_f64()
                                .context("Missing assisted fraction")?
                    } else {
                        r[key].as_f64().context("Missing fraction")?
                    };
                    ensure!(
                        value.is_finite() && (0.0..=1.0 + 1e-5).contains(&value),
                        "Invalid fraction"
                    );
                    Ok(value)
                })
                .collect::<Result<_>>()?;
            let range = values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                - values.iter().copied().fold(f64::INFINITY, f64::min);
            let entry = ranges.entry(key.into()).or_default();
            *entry = entry.max(range);
            if key == "flagged_content" {
                flagged = values;
            }
        }
        threshold_crossings +=
            usize::from(flagged.iter().any(|f| (*f >= 0.10) != (flagged[0] >= 0.10)));
    }
    let report = json!({"schema":"slop_ninja_pangram_bulk_repeatability_v1","texts":groups.len(),"observations":tasks.len(),
        "repeats_per_text":3,"provider_versions":versions,"full_results_identical_texts":full_identical,
        "full_result_differences_texts":differing_docs.len(),"native_label_changes":label_changes,
        "threshold_crossings_at_0_10":threshold_crossings,"maximum_document_fraction_ranges":ranges,
        "changed_result_paths":paths,"echo_counts":echo_counts,"annotation_files":files,
        "scope":"Same account and same-day requests; no cross-account or future-version determinism claim"});
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&args.output)?;
    file.write_all(&serde_json::to_vec_pretty(&report)?)?;
    file.sync_all()?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
