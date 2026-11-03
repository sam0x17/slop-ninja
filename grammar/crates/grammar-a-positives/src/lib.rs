//! Slop Ninja A competitive-baseline positive generation.
#![recursion_limit = "512"]
//!
//! Plans are mechanical (protocol-v1 `positive_plan`); generation is
//! resumable against one local llama-server at a time; assembly keeps exact
//! replacement spans and realised final-word shares, and retains failures.
//! Outputs contain prose and belong in ignored private storage.
pub mod breadth;
pub mod briefs;
pub mod diverse;
pub mod enron_half;
pub mod finalize;
pub mod form_post;
pub mod generation;
pub mod human_derived;
pub mod library;
pub mod mirror_regen;
pub mod order;
pub mod overlap;
pub mod pilot;
pub mod plan;
pub mod renorm;
pub mod span_ops;
pub mod splice;
pub mod splice_amend;
pub mod splice_bundle;
pub mod split_copy;
pub mod stratify;
pub mod text;
pub mod topup;
pub mod train_mirror;
pub mod values;

/// Version tag bound into every plan and record.
pub const CODE_ID: &str = "grammar-a-positives-v1";

/// Atomic JSONL write (tmp + fsync + rename); returns the SHA-256 of the bytes.
pub fn write_jsonl<T: serde::Serialize>(path: &std::path::Path, rows: &[T]) -> anyhow::Result<String> {
    use std::io::Write;
    let mut b = Vec::new();
    for r in rows {
        serde_json::to_writer(&mut b, r)?;
        b.push(b'\n');
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(&b)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(text::sha_hex(&b))
}

/// Atomic pretty JSON write; returns the SHA-256 of the bytes.
pub fn write_json(path: &std::path::Path, v: &serde_json::Value) -> anyhow::Result<String> {
    use std::io::Write;
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(&b)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(text::sha_hex(&b))
}
