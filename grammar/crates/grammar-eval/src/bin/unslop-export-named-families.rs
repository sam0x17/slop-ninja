use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[path = "../named_family_tensor.rs"]
mod named_family_tensor;

#[derive(Parser)]
#[command(about = "Export matched raw fourteen/fifteen-family distances from frozen caches")]
struct Args {
    #[arg(long)]
    coordinate_export: PathBuf,
    #[arg(long)]
    expected_manifest_sha256: String,
    #[arg(long)]
    prepared: PathBuf,
    #[arg(long)]
    reference_space: PathBuf,
    #[arg(long)]
    cache: PathBuf,
    #[arg(long)]
    protocol: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let report = named_family_tensor::export(
        &args.coordinate_export,
        &args.expected_manifest_sha256,
        &args.prepared,
        &args.reference_space,
        &args.cache,
        &args.protocol,
        &args.out,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
