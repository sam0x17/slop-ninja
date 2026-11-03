//! Stage a pinned standard dataset for source review, without training admission.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::PathBuf,
    time::Duration,
};

const REVISION: &str = "4d0ff18143b5a7e1b1e79beb540c04549d1e59d3";

#[derive(Parser)]
struct Args {
    #[arg(long)]
    output_dir: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.output_dir.exists(), "Use a new staging directory");
    fs::create_dir_all(&args.output_dir)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(45))
        .user_agent("SlopNinja-corpus-research/0.1")
        .build()?;
    let mut captures = Vec::new();
    let mut row_count = 0;
    let mut human_answers = 0;
    let mut model_answers = 0;
    let mut answer_words = 0;
    let mut short_answers = 0;
    let mut fields = BTreeSet::new();
    let mut nonempty_sources = BTreeSet::new();
    for filename in ["wiki_csai.jsonl", "README.md"] {
        let url = format!(
            "https://huggingface.co/datasets/Hello-SimpleAI/HC3/resolve/{REVISION}/{filename}"
        );
        let response = client.get(&url).send()?.error_for_status()?;
        let resolved_url = response.url().as_str().to_owned();
        let mut bytes = Vec::new();
        response.take(16_000_001).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 16_000_000, "Capture exceeds review limit");
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(args.output_dir.join(filename))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        captures.push(json!({"file":filename,"url":url,"resolved_url":resolved_url,"sha256":sha256(&bytes),"bytes":bytes.len()}));
        if filename.ends_with("jsonl") {
            for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
                let row: Value = serde_json::from_slice(line)?;
                fields.extend(
                    row.as_object()
                        .context("Expected HC3 object")?
                        .keys()
                        .cloned(),
                );
                ensure!(row["question"].is_string(), "Expected HC3 question");
                for (key, count) in [
                    ("human_answers", &mut human_answers),
                    ("chatgpt_answers", &mut model_answers),
                ] {
                    let answers = row[key].as_array().context("Expected HC3 answers")?;
                    *count += answers.len();
                    for answer in answers {
                        let words = answer
                            .as_str()
                            .context("Expected text answer")?
                            .split_whitespace()
                            .count();
                        answer_words += words;
                        short_answers += usize::from(words < 50);
                    }
                }
                if let Some(source) = row["source"].as_str().filter(|s| !s.is_empty()) {
                    nonempty_sources.insert(source.to_owned());
                }
                row_count += 1;
            }
        }
    }
    ensure!(row_count > 0, "Empty HC3 subset");
    let report = json!({
        "schema":"slop_ninja_standard_corpus_staging_v1","dataset":"Hello-SimpleAI/HC3",
        "revision":REVISION,"subset":"wiki_csai","captured_at":chrono::Utc::now().to_rfc3339(),
        "captures":captures,"question_rows":row_count,"human_answers":human_answers,
        "chatgpt_answers":model_answers,"answer_words":answer_words,"answers_below_50_words":short_answers,
        "upstream_fields":fields,"source_field_values":nonempty_sources,
        "training_admission":"pending source joins and CC-BY-SA attribution/distribution policy",
        "origin_evidence":"upstream labels retained; not independently verified production histories",
        "pangram_requests":0
    });
    let bytes = serde_json::to_vec_pretty(&report)?;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(args.output_dir.join("staging.json"))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    println!("{}", String::from_utf8(bytes)?);
    Ok(())
}
