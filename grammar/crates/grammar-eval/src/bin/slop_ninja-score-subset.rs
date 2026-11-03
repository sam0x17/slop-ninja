use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[path = "../coordinate_subset_replay.rs"]
mod coordinate_subset_replay;

#[derive(Parser)]
#[command(about = "Replay a compact frozen coordinate metric against maximal catalog exports")]
struct Args {
    #[arg(long)]
    model: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Export {
        #[arg(long)]
        export: PathBuf,
        #[arg(long, default_value = "all_m2")]
        subset: String,
        #[arg(long)]
        expected_scores: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    let Command::Export {
        export,
        subset,
        expected_scores,
        out,
    } = args.command;
    coordinate_subset_replay::replay(&args.model, &export, &subset, &expected_scores, &out)
}
