use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
#[path = "../named_family_replay.rs"]
mod named_family_replay;

#[derive(Parser)]
#[command(about = "Score a frozen named-family tensor with a portable positive spline metric")]
struct Args {
    #[arg(long)]
    model: PathBuf,
    #[arg(long)]
    expected_model_sha256: String,
    #[arg(long)]
    export: PathBuf,
    #[arg(long)]
    expected_manifest_sha256: String,
    #[arg(long)]
    reference_space: PathBuf,
    #[arg(long, requires = "expected_scores_sha256")]
    expected_scores: Option<PathBuf>,
    #[arg(long, requires = "expected_scores")]
    expected_scores_sha256: Option<String>,
    #[arg(long)]
    out: PathBuf,
}
fn main() -> Result<()> {
    let args = Args::parse();
    named_family_replay::replay(named_family_replay::Request {
        model: &args.model,
        model_sha256: &args.expected_model_sha256,
        export: &args.export,
        manifest_sha256: &args.expected_manifest_sha256,
        reference_space: &args.reference_space,
        expected_scores: args
            .expected_scores
            .as_deref()
            .zip(args.expected_scores_sha256.as_deref()),
        out: &args.out,
    })
}
