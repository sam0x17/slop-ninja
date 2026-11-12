//! Seed-ensemble means of full-A outputs (protocol doc-r2; `grammar_a_baseline::seed_mean`).
//!
//! distill-targets --raw RAW_DIR [--raw RAW_DIR ...] --out TARGETS.jsonl
//!     Teacher-mean distillation targets from `raw_full_a.py train` outputs of
//!     distinct fits on the same Train block (`train_full_a.py --distill-targets`).
//! mining-mean --scores SCORES_DIR [--scores SCORES_DIR ...] --out MEAN_DIR
//!     Mean P(any AI) of `mine_full_a.py` score directories of distinct fits on
//!     the same pool and bundle, in the same v2 score schema (`mine_full_a select`).
//!
//! Outputs are new paths, written once. Text-free.
use anyhow::Result;
use clap::{Parser, Subcommand};
use grammar_a_baseline::seed_mean;
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    DistillTargets {
        #[arg(long = "raw", required = true)]
        raw: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    MiningMean {
        #[arg(long = "scores", required = true)]
        scores: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::DistillTargets { raw, out } => {
            let dirs: Vec<&Path> = raw.iter().map(PathBuf::as_path).collect();
            println!("{}", seed_mean::write_distill_targets(&dirs, &out)?);
        }
        Command::MiningMean { scores, out } => {
            let dirs: Vec<&Path> = scores.iter().map(PathBuf::as_path).collect();
            let m = seed_mean::write_mining_mean(&dirs, &out)?;
            println!("{}", json!({"rows": m["rows"], "mean_of": m["mean_of"], "scores_sha256": m["scores_sha256"]}));
        }
    }
    Ok(())
}
