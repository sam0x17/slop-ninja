//! Materialize manually declared paragraph boundaries for source review.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};
use unslop::{html_document::Limits, html_passage};

fn digest(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Parser)]
struct Args {
    #[arg(long)]
    document_run: PathBuf,
    #[arg(long)]
    boundaries: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a fresh output directory");
    let source_report = fs::read(args.document_run.join("report.json"))?;
    ensure!(
        digest(&source_report)
            == "f1067f94436860803ea01562f8d0de00ddc8ace42c4dc019e6dcde5f742750a1",
        "Changed source view run"
    );
    let source: Value = serde_json::from_slice(&source_report)?;
    ensure!(
        digest(include_bytes!("../src/html_document.rs")) == source["view_source_sha256"]
            && digest(include_bytes!("../Cargo.lock")) == source["cargo_lock_sha256"],
        "Source parser changed"
    );
    let boundary_bytes = fs::read(&args.boundaries)?;
    let boundaries: Value = serde_json::from_slice(&boundary_bytes)?;
    ensure!(
        boundaries["schema"] == "slop_ninja_global_voices_passage_boundaries_v1"
            && boundaries["source_run_sha256"] == digest(&source_report),
        "Wrong boundary source view"
    );
    let selected = boundaries["selected"]
        .as_array()
        .context("Missing boundaries")?;
    let works = source["works"].as_array().context("Missing source works")?;
    ensure!(
        selected.len() == 8
            && selected
                .iter()
                .map(|r| &r["id"])
                .eq(works.iter().map(|r| &r["id"])),
        "Keep the complete ordered eight-work packet"
    );
    fs::create_dir(&args.out)?;
    let mut summaries = Vec::new();
    for (selection, work) in selected.iter().zip(works) {
        let id = work["id"].as_str().context("Missing source ID")?;
        let raw = fs::read(
            args.document_run
                .join(work["artifact"].as_str().context("Missing artifact path")?),
        )?;
        ensure!(
            digest(&raw) == work["artifact_sha256"],
            "Changed document artifact"
        );
        let artifact: Value = serde_json::from_slice(&raw)?;
        let html = artifact["document"]["source_html"]
            .as_str()
            .context("Missing source HTML")?;
        ensure!(digest(html) == work["html_sha256"], "Changed source HTML");
        let first = usize::try_from(
            selection["first_paragraph"]
                .as_u64()
                .context("Missing first boundary")?,
        )?;
        let last = usize::try_from(
            selection["last_paragraph"]
                .as_u64()
                .context("Missing last boundary")?,
        )?;
        let result = html_passage::paragraph_run(html, Limits::default(), first, last);
        let (candidate, counts) = match result {
            Ok(p) => {
                for part in &p.paragraphs {
                    ensure!(
                        part.text
                            == artifact["legacy_extraction"]["paragraphs"][part.ordinal]["text"],
                        "Paragraph text differs from reviewed source crosswalk"
                    );
                }
                let counts = json!({"status":"materialized_unreviewed","paragraphs":p.paragraphs.len(),"words_unicode_whitespace":p.text.split_whitespace().count(),"text_sha256":p.text_sha256,"start_node":p.start_node,"end_node_exclusive":p.end_node_exclusive});
                (
                    json!({"selection":selection,"source_artifact_sha256":work["artifact_sha256"],"passage":p,"training_admitted":false,"origin_assigned":false}),
                    counts,
                )
            }
            Err(e) => (
                json!({"selection":selection,"source_artifact_sha256":work["artifact_sha256"],"error":format!("{e:#}"),"training_admitted":false,"origin_assigned":false}),
                json!({"status":"requires_structural_review","error":format!("{e:#}")}),
            ),
        };
        let bytes = serde_json::to_vec_pretty(&candidate)?;
        let file = format!("{}.json", id.replace(':', "-"));
        fs::write(args.out.join(&file), &bytes)?;
        summaries.push(json!({"id":id,"selection":selection,"source_artifact_sha256":work["artifact_sha256"],"source_html_sha256":work["html_sha256"],"candidate_artifact":file,"candidate_artifact_sha256":digest(&bytes),"result":counts}));
    }
    let report = json!({"schema":"slop_ninja_global_voices_passage_candidates_v1","at":chrono::Utc::now().to_rfc3339(),
        "source_run_sha256":digest(source_report),"boundaries_sha256":digest(boundary_bytes),
        "passage_source_sha256":digest(include_bytes!("../src/html_passage.rs")),"runner_source_sha256":digest(include_bytes!("global_voices_passages.rs")),
        "view_source_sha256":source["view_source_sha256"],"lib_source_sha256":digest(include_bytes!("../src/lib.rs")),"cargo_lock_sha256":source["cargo_lock_sha256"],
        "protocol_sha256":digest(include_bytes!("../baseline/detector/GLOBAL_VOICES_PASSAGES_V1.md")),"binary_sha256":digest(fs::read(std::env::current_exe()?)?),
        "works":summaries,"manual_review_complete":false,"training_admitted":false,"origin_assigned":false,"model_calls":0,"pangram_calls":0});
    fs::write(
        args.out.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
