//! Frozen CFG feature projection for disjoint additional Train queries.
//! This writes a fragment, not an implicitly enlarged fitting population.
#![recursion_limit = "256"]
#[allow(dead_code, unused_imports)]
#[path = "prepare_supervised_forest/pipeline.rs"]
mod pipeline;
use anyhow::{Result, ensure};
use std::path::Path;
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 4,
        "transform_train_extension FROZEN_NUMERIC TRAIN_EXPORT TRAIN_MANIFEST NEW_OUT"
    );
    pipeline::transform_train_extension(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
    )
}
