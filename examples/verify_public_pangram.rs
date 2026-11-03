//! Fetch an existing public report; this example never submits an inference.
use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::Parser;
use std::{fs, path::PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    report_id: String,
    #[arg(long)]
    text: PathBuf,
    #[arg(long)]
    version: String,
    #[arg(long)]
    not_before: DateTime<Utc>,
    #[arg(long)]
    before: DateTime<Utc>,
    /// New directory for raw response and checked observation.
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let text = fs::read_to_string(args.text)?;
    fs::create_dir(&args.out)?;
    let bytes = unslop::public_result::fetch(&args.report_id)?;
    fs::write(args.out.join("provider-response.json"), &bytes)?;
    let observation = unslop::public_result::verify(
        &bytes,
        &args.report_id,
        &text,
        &args.version,
        args.not_before,
        args.before,
    )?;
    fs::write(
        args.out.join("observation.json"),
        serde_json::to_vec_pretty(&observation)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&observation)?);
    Ok(())
}
