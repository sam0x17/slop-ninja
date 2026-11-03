//! Frozen final-Test query projections for cfg_all, cfg_leaf, ordered_cfg and
//! global_cfg_control. No fitting, parsing, models or provider calls.
//! transform_final_cfg SELECTION CANDIDATE RECORDS MANIFEST CFG_EXPORT NEW_OUTPUT
//! The six-argument route validates v3 selection before opening evaluation data.
//! References have a separate scope and are rejected here.
#![recursion_limit = "256"]

#[allow(dead_code, unused_imports)]
#[path = "prepare_supervised_forest/pipeline.rs"]
mod pipeline;

fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::args_os().len() == 7,
        "transform_final_cfg SELECTION CANDIDATE RECORDS MANIFEST CFG_EXPORT NEW_OUTPUT"
    );
    pipeline::transform_cli()
}
