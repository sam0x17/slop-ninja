//! Synthetic-only layer trace of the exported derivative. No operator changes.
#[path = "../encoder.rs"]
mod encoder;
#[path = "../last_block_overlay.rs"]
mod last_block_overlay;
#[path = "../modernbert.rs"]
mod modernbert;
#[path = "../reference_layer_norm.rs"]
mod reference_layer_norm;
#[path = "../trace.rs"]
mod trace;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, io::Write, path::Path};
use tokenizers::Tokenizer;
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn put(path: &Path, value: &Value) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    serde_json::to_writer(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 4,
        "PARENT EXPORT CASE_ID NEW_OUTPUT (or compare REF ACTUAL NEW_REPORT)"
    );
    if a[0] == "compare" {
        return put(
            Path::new(&a[3]),
            &trace::compare(&read(Path::new(&a[1]))?, &read(Path::new(&a[2]))?)?,
        );
    }
    let parent = Path::new(&a[0]);
    let export = Path::new(&a[1]);
    let case = &a[2];
    ensure!(
        ["synthetic-short", "synthetic-unicode", "synthetic-long"].contains(&case.as_str()),
        "synthetic cases only"
    );
    ensure!(!Path::new(&a[3]).exists(), "new output required");
    for key in [
        "RAYON_NUM_THREADS",
        "CANDLE_NUM_THREADS",
        "OMP_NUM_THREADS",
        "MKL_NUM_THREADS",
    ] {
        ensure!(std::env::var(key).as_deref() == Ok("1"), "set {key}=1");
    }
    let m = read(&export.join("manifest.json"))?;
    ensure!(
        m["schema"] == "slop_ninja_last_block_a_export_v1" && m["complete"] == true,
        "bound export required"
    );
    for (key, file) in [
        ("last_layer", "last-layer.safetensors"),
        ("reference_requests", "reference-requests.jsonl"),
    ] {
        ensure!(
            m[key]["file"] == file && m[key]["sha256"] == encoder::sha(&export.join(file))?,
            "export member changed"
        );
    }
    let requests = fs::read_to_string(export.join("reference-requests.jsonl"))?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let matched = requests
        .iter()
        .filter(|r| r["id"] == *case)
        .collect::<Vec<_>>();
    ensure!(matched.len() == 1, "missing/duplicate case");
    let text = matched[0]["text"].as_str().context("synthetic text")?;
    encoder::verify(parent)?;
    let model =
        encoder::Encoder::load_with_overlay(parent, Some(&export.join("last-layer.safetensors")))?;
    let mut tokenizer = Tokenizer::from_file(parent.join("classifier/tokenizer.json"))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    tokenizer
        .with_truncation(None)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    tokenizer.with_padding(None);
    let (ids, status) = encoder::token_ids(&tokenizer, text)?;
    ensure!(status == "ok", "unsupported synthetic reference");
    let baseline = model.forward(&ids)?;
    trace::begin();
    let actual = model.forward(&ids)?;
    ensure!(
        baseline.h == actual.h && baseline.logits == actual.logits,
        "trace changed computation"
    );
    put(
        Path::new(&a[3]),
        &json!({"schema":"slop_ninja_last_block_trace_v1","case":case,"synthetic_only":true,
        "ids":ids,"export_manifest_sha256":encoder::sha(&export.join("manifest.json"))?,
        "trace_preserves_outputs_exactly":true,"h_raw_f32":actual.h,"logits":actual.logits,"tensors":trace::finish()}),
    )
}
