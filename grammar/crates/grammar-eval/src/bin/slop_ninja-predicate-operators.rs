use anyhow::{Context, Result, ensure};
use clap::Parser;
use grammar_core::{features::FeatureExtractor, syntax::Document};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::PathBuf};

#[path = "../lexical_context.rs"]
mod lexical_context;
#[path = "../predicate_operators.rs"]
mod predicate_operators;

#[derive(Parser)]
#[command(
    about = "Extract a versioned local predicate-operator family from a hash-bound syntax Document"
)]
struct Args {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    expected_sha256: String,
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let bytes = fs::read(&args.input).with_context(|| format!("Read {}", args.input.display()))?;
    ensure!(
        hex::encode(Sha256::digest(&bytes)) == args.expected_sha256,
        "Input annotation SHA256 differs"
    );
    let doc: Document = serde_json::from_slice(&bytes)?;
    let features = predicate_operators::PredicateOperatorExtractor.extract(&doc)?;
    let observations = predicate_operators::document_observations(&doc)?;
    let result = json!({
        "schema": "slopninja-predicate-operator-extraction-v1",
        "annotation_sha256": args.expected_sha256,
        "features": features,
        "source_aligned_observations": observations,
        "parser_calls": 0,
        "training_calls": 0
    });
    let mut output = serde_json::to_vec_pretty(&result)?;
    output.push(b'\n');
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args.out)?
        .write_all(&output)?;
    Ok(())
}
