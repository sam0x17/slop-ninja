//! `slop_ninja_cc_human_measure`: contamination and era measurements on the
//! Common Crawl human-slice pilot pool. Steps:
//! `sample` (stratified draw and request shards), `report` (flag rates from
//! the released A runtime and the Pangram bulk store) and `era-probe`
//! (hashed n-gram logistic regression). No network or model calls.
use anyhow::Result;
use clap::{Parser, Subcommand};
use grammar_cc_human::{era_probe, measure};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "slop_ninja_cc_human_measure")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Stratified keyed draw by crawl year x domain, with a nested Pangram subsample.
    Sample {
        #[arg(long)]
        pool: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "cc-human-contamination-20261030")]
        seed: String,
        #[arg(long, default_value_t = 50)]
        per_cell: usize,
        #[arg(long, default_value_t = 20)]
        pangram_per_cell: usize,
        #[arg(long, default_value_t = 8)]
        shards: usize,
    },
    /// Flag-rate tables, trends, drift and the Tier C human comparison.
    Report {
        #[arg(long)]
        sample: PathBuf,
        /// SAMPLE.json (pool cell sizes for pool-weighted margins).
        #[arg(long)]
        pool_cells: PathBuf,
        #[arg(long, num_args = 1..)]
        a_pred: Vec<PathBuf>,
        /// The CAL 1% operating point, as f64 bits.
        #[arg(long, default_value = "0x3fed1745d1745d17")]
        threshold_bits: String,
        #[arg(long)]
        pangram_store: Option<PathBuf>,
        #[arg(long)]
        pangram_map: PathBuf,
        #[arg(long)]
        tierc_rows: Option<PathBuf>,
        #[arg(long)]
        tierc_pred: Option<PathBuf>,
        #[arg(long)]
        tierc_pangram: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 1000)]
        draws: usize,
    },
    /// Early (2013-2016) vs late (2020-2022) probe on human pilot text.
    EraProbe {
        #[arg(long)]
        pool: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "cc-human-era-probe-20261030")]
        seed: String,
        #[arg(long, default_value_t = 3000)]
        per_domain_class: usize,
        #[arg(long, default_value_t = 20)]
        dims_log2: u32,
        #[arg(long, default_value_t = 5)]
        epochs: usize,
        #[arg(long, default_value_t = 0.2)]
        lr: f64,
        #[arg(long, default_value_t = 1e-6)]
        l2: f64,
        #[arg(long, default_value_t = 500)]
        draws: usize,
        #[arg(long)]
        sample: Option<PathBuf>,
        #[arg(long)]
        a_scores: Option<PathBuf>,
        #[arg(long)]
        tierc_rows: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let out = match Cli::parse().cmd {
        Cmd::Sample { pool, out, seed, per_cell, pangram_per_cell, shards } => measure::sample(&measure::SampleArgs {
            pool: &pool,
            out: &out,
            seed: &seed,
            per_cell,
            pangram_per_cell,
            shards,
        })?,
        Cmd::Report { sample, pool_cells, a_pred, threshold_bits, pangram_store, pangram_map, tierc_rows, tierc_pred, tierc_pangram, out, draws } => {
            let bits = u64::from_str_radix(threshold_bits.trim_start_matches("0x"), 16)?;
            measure::report(&measure::ReportArgs {
                sample,
                a_pred,
                threshold: f64::from_bits(bits),
                pangram_store,
                pangram_map,
                tierc_rows,
                tierc_pred,
                tierc_pangram,
                pool_cells,
                out,
                draws,
            })?
        }
        Cmd::EraProbe { pool, out, seed, per_domain_class, dims_log2, epochs, lr, l2, draws, sample, a_scores, tierc_rows } => {
            era_probe::run(&era_probe::ProbeArgs { pool, out, seed, per_domain_class, dims_log2, epochs, lr, l2, draws, sample, a_scores, tierc_rows })?
        }
    };
    println!("{out}");
    Ok(())
}
