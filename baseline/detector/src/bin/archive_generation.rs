//! Retain every accepted branch and failed attempt from closed generation cohorts.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::{
    dataset::{self, OriginRecord, sha256},
    evaluation_view,
};
use std::{collections::BTreeMap, fs, path::PathBuf};

#[derive(Parser)]
struct Args {
    /// Every cohort's exact input, including roots from failed families.
    #[arg(long, required = true)]
    input: Vec<PathBuf>,
    #[arg(long, required = true)]
    generation_dir: Vec<PathBuf>,
    #[arg(long)]
    output_dir: PathBuf,
}

fn insert(records: &mut BTreeMap<String, OriginRecord>, record: OriginRecord) -> Result<()> {
    if let Some(previous) = records.get(&record.id) {
        ensure!(
            serde_json::to_value(previous)? == serde_json::to_value(&record)?,
            "Conflicting record identity: {}",
            record.id
        );
    } else {
        records.insert(record.id.clone(), record);
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new archive directory");
    let mut records = BTreeMap::new();
    let mut inputs = BTreeMap::new();
    for path in &args.input {
        let (rows, hash) = evaluation_view::read_archive(path)?;
        inputs.insert(
            hash,
            json!({"path":path.canonicalize()?,"records":rows.len()}),
        );
        for row in rows {
            insert(&mut records, row)?;
        }
    }
    let mut cohorts = Vec::new();
    for directory in &args.generation_dir {
        ensure!(
            !directory.join("run.lock").try_exists()?,
            "Generation cohort has run.lock; wait for generation/export to close"
        );
        let run_bytes = fs::read(directory.join("run.json"))?;
        let run: Value = serde_json::from_slice(&run_bytes)?;
        ensure!(
            inputs.contains_key(
                run["input_sha256"]
                    .as_str()
                    .context("Missing input binding")?
            ),
            "Every cohort input must be supplied unchanged"
        );
        let summary_bytes = fs::read(directory.join("summary.json"))?;
        let summary: Value = serde_json::from_slice(&summary_bytes)?;
        ensure!(
            summary["status"] == "complete"
                && summary["complete_export_ready"] == true
                && summary["unattempted"]
                    .as_array()
                    .is_some_and(|a| a.is_empty()),
            "Archive only closed cohorts; preserve an active cache in place"
        );
        let mut calls = BTreeMap::new();
        let mut accepted = 0u64;
        let mut failed = 0u64;
        for entry in fs::read_dir(directory.join("calls"))? {
            let path = entry?.path();
            ensure!(path.is_dir(), "Unexpected entry in calls directory");
            let key = path
                .file_name()
                .context("Missing task key")?
                .to_str()
                .context("Non-UTF8 key")?;
            ensure!(
                key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()),
                "Invalid task key"
            );
            let record_path = path.join("record.json");
            let error_path = path.join("error.txt");
            ensure!(
                record_path.exists() != error_path.exists(),
                "Call must have exactly one terminal status"
            );
            let mut files = BTreeMap::new();
            for file in fs::read_dir(&path)? {
                let file = file?.path();
                ensure!(file.is_file(), "Unexpected nested call entry");
                files.insert(
                    file.file_name().unwrap().to_string_lossy().into_owned(),
                    sha256(fs::read(file)?),
                );
            }
            let observation = if record_path.exists() {
                let row: OriginRecord = serde_json::from_slice(&fs::read(&record_path)?)?;
                row.validate()?;
                ensure!(
                    row.id == format!("generation:{key}"),
                    "Record does not bind its task directory"
                );
                let generation = row
                    .generation
                    .as_ref()
                    .context("Accepted call has no generation provenance")?;
                ensure!(
                    files.get("request.json") == Some(&generation.request_sha256)
                        && files.get("response.json") == Some(&generation.response_sha256),
                    "Cached request/response hash differs from record"
                );
                let value = json!({"status":"accepted","record_id":row.id,"source_group":row.source_group,
                    "parent_id":row.parent_id,"operation":generation.operation,"files":files});
                insert(&mut records, row)?;
                accepted += 1;
                value
            } else {
                failed += 1;
                json!({"status":"failed","reason":fs::read_to_string(error_path)?,"files":files})
            };
            ensure!(
                calls.insert(key.to_owned(), observation).is_none(),
                "Duplicate task key"
            );
        }
        ensure!(
            summary["requested"] == calls.len() && summary["completed"] == accepted,
            "Cache counts differ from closed summary"
        );
        let (complete, export_hash) =
            evaluation_view::read_archive(&directory.join("records.jsonl"))?;
        ensure!(
            summary["summary"]["records"] == complete.len(),
            "Complete export count differs from closed summary"
        );
        for row in &complete {
            ensure!(records.get(&row.id).is_some_and(|r| serde_json::to_value(r).ok() == serde_json::to_value(row).ok()),
                "Complete export contains an unbound record");
        }
        cohorts.push(json!({"directory":directory.canonicalize()?,"run_sha256":sha256(run_bytes),
            "input_sha256":run["input_sha256"],"summary_sha256":sha256(summary_bytes),"summary":summary,
            "complete_export_sha256":export_hash,"complete_export_records":complete.len(),
            "accepted_requests":accepted,"failed_requests":failed,"calls":calls}));
    }
    let records: Vec<_> = records.into_values().collect();
    dataset::validate_records(&records)?;
    let summary = dataset::summarize(&records);
    fs::create_dir_all(&args.output_dir)?;
    let output = args.output_dir.join("records.jsonl");
    dataset::write_records(&output, &records)?;
    fs::write(
        args.output_dir.join("archive.json"),
        serde_json::to_vec_pretty(&json!({
            "schema":"slop_ninja_generation_master_archive_v1",
            "usage":"Attempt and lineage archive; use a separately admitted complete-family assembly for fitting and paired evaluation",
            "inputs":inputs,"cohorts":cohorts,"records_sha256":sha256(fs::read(output)?),"summary":summary,
        "source_sha256":sha256(include_bytes!("archive_generation.rs")),
        "executable_sha256":sha256(fs::read(std::env::current_exe()?)?),
            "model_calls":0,"detector_calls":0,"pangram_calls":0
        }))?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}
