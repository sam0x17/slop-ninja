//! Freeze an explicit trio per family without reading predictions.
use anyhow::Result;
use clap::{Parser, ValueEnum};
use slop_ninja_detector::{
    dataset::{Split, sha256},
    evaluation_view::{self, Selection},
};
use std::{fs, io::Write, path::PathBuf};

#[derive(Clone, Copy, ValueEnum)]
enum Partition {
    Development,
    Calibration,
    Test,
}

#[derive(Parser)]
struct Args {
    /// Complete ancestry archive. Every record is validated before selection.
    #[arg(long)]
    records: PathBuf,
    #[arg(long, value_enum)]
    split: Partition,
    /// JSON array of source_group, human_id, model_id and mixed_id declarations.
    #[arg(long)]
    selection: PathBuf,
    /// New immutable view file; existing files are never overwritten.
    #[arg(long)]
    output: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let (records, records_sha256) = evaluation_view::read_archive(&args.records)?;
    let selections: Vec<Selection> = serde_json::from_slice(&fs::read(args.selection)?)?;
    let split = match args.split {
        Partition::Development => Split::Development,
        Partition::Calibration => Split::Calibration,
        Partition::Test => Split::Test,
    };
    let view = evaluation_view::build_view(&records, records_sha256, split, &selections)?;
    let mut bytes = serde_json::to_vec_pretty(&view)?;
    bytes.push(b'\n');
    let mut file = fs::File::create_new(&args.output)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    println!(
        "{}",
        serde_json::to_string(&evaluation_view::Binding {
            sha256: sha256(bytes),
            records_sha256: view.records_sha256,
            split,
            selected_rows: view.families.len() * 3,
            source_groups: view.families.len(),
        })?
    );
    Ok(())
}
