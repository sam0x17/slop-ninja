//! Synthetic-only per-operation trace of a full-A export; no operator changes.
//! PARENT EXPORT CASE_ID NEW_OUTPUT [--f64]  trace one bound synthetic case
//!                                         (FP32 V6 graph, or the FP64 profile)
//! compare REFERENCE ACTUAL NEW_REPORT    per-operation errors against a reference
//!                                         trace (for example the FP64 Python trace)
#[path = "../encoder.rs"]
#[allow(dead_code)]
mod encoder;
#[allow(dead_code)]
#[path = "../full_encoder.rs"]
mod full_encoder;
#[allow(dead_code)]
#[path = "../last_block_overlay.rs"]
mod last_block_overlay;
#[allow(dead_code)]
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
fn max_abs(a: &Value, b: &Value) -> Result<f64> {
    let (a, b) = (
        a.as_array().context("array")?,
        b.as_array().context("array")?,
    );
    ensure!(a.len() == b.len() && !a.is_empty(), "vector length differs");
    a.iter().zip(b).try_fold(0f64, |m, (x, y)| {
        Ok(m.max((x.as_f64().context("number")? - y.as_f64().context("number")?).abs()))
    })
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 4 || (a.len() == 5 && a[4] == "--f64" && a[0] != "compare"),
        "PARENT EXPORT CASE_ID NEW_OUTPUT [--f64] (or compare REFERENCE ACTUAL NEW_REPORT)"
    );
    let profile = if a.len() == 5 {
        full_encoder::Profile::F64
    } else {
        full_encoder::Profile::F32
    };
    if a[0] == "compare" {
        let (r, x) = (read(Path::new(&a[1]))?, read(Path::new(&a[2]))?);
        ensure!(r["case"] == x["case"] && r["ids"] == x["ids"], "different cases");
        let mut report = trace::compare(&r, &x)?;
        report["h_max_absolute_error"] = json!(max_abs(&r["h_raw"], &x["h_raw"])?);
        report["token_states_max_absolute_error"] =
            json!(max_abs(&r["token_states"], &x["token_states"])?);
        report["reference"] = json!({"device":r["device"],"dtype":r["dtype"]});
        report["actual"] = json!({"device":x["device"],"dtype":x["dtype"]});
        return put(Path::new(&a[3]), &report);
    }
    let (parent, export, case) = (Path::new(&a[0]), Path::new(&a[1]), &a[2]);
    ensure!(
        ["synthetic-short", "synthetic-unicode"].contains(&case.as_str()),
        "short synthetic cases only"
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
        m["schema"] == "slop_ninja_full_a_export_v1" && m["complete"] == true,
        "bound full-A export required"
    );
    for key in ["encoder", "reference_requests"] {
        let file = m[key]["file"].as_str().context("member")?;
        ensure!(
            m[key]["sha256"] == encoder::sha(&export.join(file))?,
            "export member changed: {key}"
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
    let weights = export.join("encoder.safetensors");
    full_encoder::validate_roster(&parent.join("classifier/model.safetensors"), &weights)?;
    let model = full_encoder::FullEncoder::load(parent, &weights, profile)?;
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
        baseline.h == actual.h && baseline.tokens == actual.tokens,
        "trace changed computation"
    );
    put(
        Path::new(&a[3]),
        &json!({"schema":"slop_ninja_full_a_trace_v1","case":case,"synthetic_only":true,"ids":ids,
        "device":"cpu","dtype":profile.name(),"matmul_backend":if cfg!(feature="mkl"){"mkl"}else{"candle_default"},
        "export_manifest_sha256":encoder::sha(&export.join("manifest.json"))?,
        "trace_preserves_outputs_exactly":true,"h_raw":actual.h,"token_states":actual.tokens,
        "tensors":trace::finish()}),
    )
}
