//! Transform an explicitly authorized Test export with a frozen numeric contract.
//! No vocabulary, profile, normalization, model or calibration fitting occurs.
//!
//! Usage: transform_supervised_forest FROZEN_NUMERIC_DIR TEST_EXPORT_DIR
//!        TEST_DATASET_MANIFEST NEW_OUTPUT
//!
//! The Test manifest uses slop_ninja_a_supervised_dataset_v1 with records_sha256,
//! a nonempty test_ids allow-list, non_test_ids (possibly empty), sources or
//! provenance, and source-family split_assignments. Test rows must be queries.
//! Existing Train/Development/Calibration/reference files and contract are
//! copied exactly after verification; only a Test binding and audit are added.
#![recursion_limit = "256"]

#[allow(dead_code, unused_imports)]
#[path = "prepare_supervised_forest/pipeline.rs"]
mod pipeline;

fn main() -> anyhow::Result<()> {
    pipeline::transform_cli()
}
