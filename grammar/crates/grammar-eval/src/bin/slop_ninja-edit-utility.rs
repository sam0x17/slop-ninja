use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[path = "../edit_utility.rs"]
mod edit_utility;
#[path = "../edit_utility_scoring.rs"]
mod edit_utility_scoring;
#[allow(dead_code)]
#[path = "../function_word_roles.rs"]
mod function_word_roles;
#[allow(dead_code)]
#[path = "../lexical_context.rs"]
mod lexical_context;
#[allow(dead_code)]
#[path = "../predicate_operators.rs"]
mod predicate_operators;

#[derive(Parser)]
#[command(
    about = "Measure synthetic licensed edits and counterexamples under frozen grammar and author metrics"
)]
struct Args {
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    expected_protocol_sha256: String,
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    edit_utility::run(&args.protocol, &args.expected_protocol_sha256, &args.out)
}
