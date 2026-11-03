//! Prepare supervised Train/Development/Calibration numeric CFG inputs.
//! See prepare_supervised_forest/pipeline.rs for the manifest and feature contract.
#![recursion_limit = "256"]

#[allow(dead_code, unused_imports)]
#[path = "prepare_supervised_forest/pipeline.rs"]
mod pipeline;

fn main() -> anyhow::Result<()> {
    pipeline::prepare_cli()
}
