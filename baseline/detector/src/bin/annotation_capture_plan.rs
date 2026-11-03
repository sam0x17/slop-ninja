//! Prepare a complete audited hosted-model cohort for research-only annotation.
#[path = "support/annotation_captures.rs"]
mod annotation_captures;
use anyhow::{Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    capture_review: PathBuf,
    #[arg(long)]
    expected_audit_sha256: String,
    #[arg(long)]
    output_dir: PathBuf,
}
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = fs::File::create_new(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new output directory");
    let captures = annotation_captures::load(&args.capture_review, &args.expected_audit_sha256)?;
    let mut unique = BTreeMap::<String, (String, Vec<Value>)>::new();
    for capture in &captures {
        let entry = unique
            .entry(sha256(&capture.text))
            .or_insert_with(|| (capture.text.clone(), Vec::new()));
        ensure!(entry.0 == capture.text, "Text hash collision");
        entry.1.push(capture.reference.clone());
    }
    let mut batches = Vec::new();
    let mut items = Vec::new();
    let mut members = Vec::new();
    let mut units = 0;
    let mut total_units = 0;
    let mut total_words = 0;
    for (hash, (text, provenance)) in &unique {
        let words = text.split_whitespace().count();
        let billable = words.div_ceil(100);
        ensure!(
            (1..=1000).contains(&billable),
            "Input exceeds the provider batch limit"
        );
        if units + billable > 1000 {
            batches.push(json!({"model":"pangram-4","items":items}));
            items = Vec::new();
            units = 0;
        }
        let id = format!("{hash}:0");
        members.push(json!({"request_id":id,"text_sha256":hash,"repeat_index":0,
            "batch_index":batches.len(),"item_index":items.len(),"words":words,
            "estimated_billable_units":billable,"records":provenance}));
        items.push(json!({"id":id,"text":text}));
        units += billable;
        total_units += billable;
        total_words += words;
    }
    if !items.is_empty() {
        batches.push(json!({"model":"pangram-4","items":items}));
    }
    fs::create_dir_all(args.output_dir.join("requests"))?;
    fs::create_dir(args.output_dir.join("capture-review"))?;
    for name in ["reviews.jsonl", "summary.json", "source-records.jsonl"] {
        write(
            &args.output_dir.join("capture-review").join(name),
            &fs::read(args.capture_review.join(name))?,
        )?;
    }
    // Revalidate the copied archive so a changing source cannot alter a prepared job.
    let copied = annotation_captures::load(
        &args.output_dir.join("capture-review"),
        &args.expected_audit_sha256,
    )?;
    ensure!(copied.len() == captures.len(), "Copy differs");
    let mut request_files = Vec::new();
    for (i, batch) in batches.iter().enumerate() {
        let bytes = serde_json::to_vec(batch)?;
        let path = format!("requests/batch-{i:04}.json");
        write(&args.output_dir.join(&path), &bytes)?;
        request_files.push(json!({"path":path,"sha256":sha256(&bytes)}));
    }
    let manifest = json!({"schema":annotation_captures::PLAN_SCHEMA,"model_selector":"pangram-4",
        "status":"prepared_not_submitted","source_kind":"hosted_cli_capture","commercial_training_admitted":false,
        "capture_audit_sha256":args.expected_audit_sha256,"record_count":captures.len(),"unique_texts":unique.len(),
        "repeats":1,"repeat_start":0,"request_items":members.len(),"request_batches":batches.len(),
        "estimated_submitted_words":total_words,"estimated_billable_units":total_units,
        "estimated_bulk_usd_cents":total_units*4,"estimated_realtime_usd_cents":total_units*5,
        "pricing_checked":"2026-09-12","pricing_url":"https://www.pangram.com/pricing",
        "api_url":"https://docs.pangram.com/api-reference/bulk-api",
        "word_count_policy":"Rust split_whitespace; provider counting can differ; estimate only",
        "label_policy":"Captured model responses retain model-only origin independently of provider fractions. The plan grants no training admission; each capture keeps its own recorded admission state.",
        "annotation_publication":"retain in ignored storage until a release includes them","request_files":request_files,"members":members,
        "planner_executable_sha256":sha256(fs::read(std::env::current_exe()?)?),
        "planner_source_sha256":sha256(include_bytes!("annotation_capture_plan.rs")),
        "capture_adapter_source_sha256":sha256(include_bytes!("support/annotation_captures.rs"))});
    write(
        &args.output_dir.join("plan.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"records":captures.len(),"unique_texts":unique.len(),
        "billable_units":total_units,"estimated_bulk_usd_cents":total_units*4,"reserve_at_least_usd_cents":total_units*5,
        "plan_sha256":sha256(fs::read(args.output_dir.join("plan.json"))?),"submitted":false})
        )?
    );
    Ok(())
}
