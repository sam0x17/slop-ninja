#[path = "../stability.rs"]
mod stability;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    about = "Measure training-only grammar concordance with content-overlap matched controls"
)]
struct Args {
    /// Original author export, whose manifests are bound to the prior evaluation.
    export_dir: PathBuf,
    /// Prior evaluation containing the frozen Space, train-only IDF, and audit.
    #[arg(long)]
    evaluation: PathBuf,
    /// Existing annotation and feature cache; no parser or model is invoked.
    #[arg(long)]
    cache: PathBuf,
    /// Fresh output directory in ignored local data.
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    stability::run(&args.export_dir, &args.evaluation, &args.cache, &args.out)
}
