//! Frozen final-Test references: admission, projection, then per-seed encoding.
//! select SELECTION FINAL_DATASET_MANIFEST REFERENCES.jsonl NEW_OUT
//! project SELECTION CANDIDATE REF_RECORDS REF_MANIFEST REF_CFG_EXPORT REF_EMBEDDINGS_OR_DASH NEW_OUT
//! encode SELECTION CANDIDATE SEED REF_NUMERIC NEW_GALLERY.json
//! No fitting, parsing, source acquisition or upstream model/provider execution.
#![recursion_limit = "256"]
#[allow(dead_code, unused_imports)]
#[path = "prepare_supervised_forest/pipeline.rs"]
mod pipeline;

fn main() -> anyhow::Result<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        matches!(
            args.first().and_then(|v| v.to_str()),
            Some("select" | "project" | "encode")
        ),
        "prepare_final_test_references select|project|encode ..."
    );
    pipeline::transform_cli()
}
