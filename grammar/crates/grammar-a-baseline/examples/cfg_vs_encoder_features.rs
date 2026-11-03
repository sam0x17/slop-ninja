//! Feature extraction for the frozen `cfg-vs-encoder-20261031` writer-profile
//! protocol. Inputs and outputs contain or derive from exact private text: keep
//! every path under ignored `data/`. No task metric, profile or score is
//! computed here; the separate evaluator fits and scores the frozen arms.
//!
//! Subcommands:
//!   select FROZEN.json REPO NEW_OUT_DIR
//!       Apply the frozen author-role, document-rank, cap and T3 rules; write the
//!       selection (IDs and metadata only) and chunked {id, text} inputs.
//!   cfg-index EXPORT_DIR NEW_INDEX.jsonl
//!       Index one finished `cfg_document_export` chunk: per-document window
//!       spans, tiers and token counts, verified against the row digests, so the
//!       window cache can then be compressed.
//!   cfg-restricted FROZEN.json SELECTION_DIR EXPORTS_DIR NEW_OUT_DIR
//!       Coverage-restricted texts once every chunk holding a T1/T2 document
//!       is exported (byte-identical to the cfg-collect restricted output).
//!   cfg-collect FROZEN.json SELECTION_DIR EXPORTS_DIR NEW_OUT_DIR
//!       Merge verified chunk receipts into sparse per-document CFG count rows,
//!       a sorted key dictionary, coverage counts and the coverage-restricted
//!       texts (complete windows only) for the T1/T2 population.
//!   stylometric FROZEN.json INPUTS.jsonl NEW_OUT_DIR
//!       Function-word and character 3-gram counts (raw counts; the vocabulary
//!       and transform are fitted later on training authors).
//!   encoder-collect FROZEN.json CHUNKS_DIR RUNS_DIR NEW_OUT_DIR
//!       Merge verified R0 runtime chunk outputs into h_raw / z_shared rows.
#![recursion_limit = "256"]

#[path = "cfg_vs_encoder_features/cfg.rs"]
mod cfg;
#[path = "cfg_vs_encoder_features/common.rs"]
mod common;
#[path = "cfg_vs_encoder_features/encoder.rs"]
mod encoder;
#[path = "cfg_vs_encoder_features/select.rs"]
mod select;
#[path = "cfg_vs_encoder_features/stylo.rs"]
mod stylo;
#[cfg(test)]
#[path = "cfg_vs_encoder_features/tests.rs"]
mod tests;

use anyhow::{Result, bail, ensure};
use std::path::Path;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(!args.is_empty(), "usage: see module documentation");
    let p = |i: usize| -> Result<&Path> {
        Ok(Path::new(
            args.get(i)
                .ok_or_else(|| anyhow::anyhow!("missing argument {i}"))?,
        ))
    };
    match args[0].as_str() {
        "select" => {
            ensure!(args.len() == 4, "select FROZEN.json REPO NEW_OUT_DIR");
            select::run(p(1)?, p(2)?, p(3)?)
        }
        "cfg-index" => {
            ensure!(args.len() == 3, "cfg-index EXPORT_DIR NEW_INDEX.jsonl");
            cfg::index_cli(p(1)?, p(2)?)
        }
        "cfg-restricted" => {
            ensure!(
                args.len() == 5,
                "cfg-restricted FROZEN.json SELECTION_DIR EXPORTS_DIR NEW_OUT_DIR"
            );
            cfg::restricted_cli(p(1)?, p(2)?, p(3)?, p(4)?)
        }
        "cfg-collect" => {
            ensure!(
                args.len() == 5,
                "cfg-collect FROZEN.json SELECTION_DIR EXPORTS_DIR NEW_OUT_DIR"
            );
            cfg::collect_cli(p(1)?, p(2)?, p(3)?, p(4)?)
        }
        "stylometric" => {
            ensure!(
                args.len() == 4,
                "stylometric FROZEN.json INPUTS.jsonl NEW_OUT_DIR"
            );
            stylo::run(p(1)?, p(2)?, p(3)?)
        }
        "encoder-collect" => {
            ensure!(
                args.len() == 5,
                "encoder-collect FROZEN.json CHUNKS_DIR RUNS_DIR NEW_OUT_DIR"
            );
            encoder::collect_cli(p(1)?, p(2)?, p(3)?, p(4)?)
        }
        other => bail!("unknown subcommand {other}"),
    }
}
