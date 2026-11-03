use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
#[path = "../family_ablation.rs"]
mod family_ablation;
#[derive(Parser)]
#[command(
    about = "Measure removal of complete lexical or grammatical family contributions from a frozen model"
)]
struct Args {
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    out: PathBuf,
}
fn main() -> Result<()> {
    let args = Args::parse();
    family_ablation::run(&args.protocol, &args.out)
}
