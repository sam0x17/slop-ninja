//! Rust CPU inference for the separately exported final-block A derivative.
//! PARENT_ENCODER EXPORT_DIR REQUESTS.jsonl NEW_OUT [--uncalibrated-diagnostics]
#[path = "../encoder.rs"]
mod encoder;
#[path = "../last_block_overlay.rs"]
mod last_block_overlay;
#[allow(dead_code)]
#[path = "../modernbert.rs"]
mod modernbert;
#[path = "../reference_layer_norm.rs"]
mod reference_layer_norm;
#[allow(dead_code)]
#[path = "../trace.rs"]
mod trace;
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::{shared::SharedInput, warm_shared::WarmSharedModel};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};
use tokenizers::Tokenizer;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
    text: String,
}
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn bounded(p: &Path) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    fs::File::open(p)?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut b)?;
    ensure!(b.len() <= 64 * 1024 * 1024, "input exceeds64MiB");
    Ok(b)
}
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&bounded(p)?)?)
}
fn put(p: &Path, v: &Value) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(p)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn member(root: &Path, m: &Value, key: &str) -> Result<PathBuf> {
    let name = m[key]["file"].as_str().context("export member file")?;
    ensure!(
        !name.is_empty()
            && Path::new(name)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
        "unsafe export member"
    );
    let path = root.join(name).canonicalize()?;
    ensure!(path.starts_with(root), "member escaped export root");
    ensure!(
        encoder::sha(&path)? == m[key]["sha256"],
        "export member changed: {key}"
    );
    Ok(path)
}
fn guard(m: &Value, c: &Value, w: &Value, contract_hash: &str, diagnostic: bool) -> Result<()> {
    ensure!(
        m["schema"] == "slop_ninja_last_block_a_export_v1" && m["complete"] == true,
        "incomplete derivative export"
    );
    ensure!(
        m["parent_artifact_id"] == encoder::ID
            && m["parent_manifest_sha256"] == encoder::MANIFEST
            && c["parent_artifact_id"] == encoder::ID,
        "unqualified parent encoder"
    );
    ensure!(
        m["layer"] == 21
            && m["tensor_names"]
                == json!(
                    last_block_overlay::SHAPES
                        .iter()
                        .map(|(n, _)| *n)
                        .collect::<Vec<_>>()
                ),
        "overlay tensor scope changed"
    );
    ensure!(
        matches!(
            m["condition"].as_str(),
            Some("frozen_last_block" | "adapt_last_block")
        ),
        "unknown condition"
    );
    ensure!(
        m["calibrated"].is_boolean() && (diagnostic || m["calibrated"] == true),
        "uncalibrated export needs explicit diagnostic mode"
    );
    ensure!(
        c["schema"] == "slop_ninja_last_block_a_contract_v1"
            && c["input_mode"] == "text"
            && c["raw_dimension"] == 768
            && c["zero_cfg_coordinates"] == 1820
            && c["maximum_tokens_including_specials"] == 2048
            && c["token_truncation"] == false,
        "input meaning differs"
    );
    ensure!(
        w["schema"] == "slop_ninja_v6_residual_a_weights_v1"
            && w["input_mode"] == "text"
            && w["control"] == "joint"
            && w["contract_sha256"] == contract_hash
            && w["base_temperature"] == encoder::TEMP,
        "joint heads/contract differ"
    );
    for key in ["origin", "share", "mixed_fractional_share", "human", "ai"] {
        ensure!(
            diagnostic || w["supervision"][key].as_u64().is_some_and(|n| n > 0),
            "actual A requires trained {key} supervision"
        );
    }
    ensure!(
        w["architecture"]["dimension"] == 2588
            && w["architecture"]["raw_text_dimension"] == 768
            && w["architecture"]["embedding"] == 768,
        "head geometry"
    );
    let n = &c["embedding_normalization"];
    ensure!(
        n["fit"] == "train_query_only_equal_source_family_weight"
            && n["dimension"] == 768
            && n["development_used_for_fit"] == false
            && n["calibration_used_for_fit"] == false
            && n["reference_used_for_fit"] == false
            && n["final_test_opened"] == false,
        "normalization scope"
    );
    for (ck, wk) in [("mean", "text_mean"), ("scale", "text_scale")] {
        let cv = n[ck].as_array().context("normalization")?;
        let wv = w[wk].as_array().context("weight normalization")?;
        ensure!(
            cv.len() == 768 && wv.len() == 768,
            "normalization dimension"
        );
        for (a, b) in cv.iter().zip(wv) {
            let x = a.as_f64().context("number")?;
            let y = b.as_f64().context("number")?;
            ensure!(
                x.is_finite() && y.is_finite() && (x as f32) as f64 == y,
                "normalization buffer differs"
            );
        }
    }
    for role in ["human", "ai"] {
        ensure!(
            w["profiles"][role] == c[format!("{role}_profiles")],
            "profile roster differs"
        );
    }
    Ok(())
}
fn guard_calibration(m: &Value, w: &Value, cal: &Value, contract_hash: &str) -> Result<()> {
    ensure!(
        cal["schema"] == "slop_ninja_last_block_calibration_v1"
            && cal["complete"] == true
            && cal["fit_split"] == "calibration"
            && cal["final_test_opened"] == false
            && cal["origin_calibrated"] == true
            && cal["condition"] == m["condition"]
            && cal["seed"] == m["seed"]
            && cal["selected_checkpoint_sha256"] == m["selected_checkpoint_sha256"]
            && cal["contract_sha256"] == contract_hash
            && cal["temperature"] == w["temperature"],
        "calibration/checkpoint/origin temperature mismatch"
    );
    for role in ["human", "ai"] {
        ensure!(
            cal["roles"][role]["qualified"] == true
                && cal["roles"][role]["threshold"] == w["thresholds"][role],
            "unqualified or changed role threshold"
        );
    }
    Ok(())
}
fn run(parent: &Path, export: &Path, input: &Path, out: &Path, diagnostic: bool) -> Result<()> {
    ensure!(!out.exists(), "new output required");
    ensure!(diagnostic || !cfg!(feature = "mkl"), "unqualified MKL profile requires explicit diagnostics");
    let parent = parent.canonicalize()?;
    let export = export.canonicalize()?;
    ensure!(
        !out.parent()
            .context("output parent")?
            .canonicalize()?
            .starts_with(&parent),
        "output inside parent artifact"
    );
    for name in [
        "RAYON_NUM_THREADS",
        "CANDLE_NUM_THREADS",
        "OMP_NUM_THREADS",
        "MKL_NUM_THREADS",
    ] {
        ensure!(std::env::var(name).as_deref() == Ok("1"), "set {name}=1");
    }
    if cfg!(feature = "mkl") {
        ensure!(std::env::var("MKL_CBWR").as_deref() == Ok("COMPATIBLE"),
            "the MKL numeric profile requires MKL_CBWR=COMPATIBLE");
    }
    let started = Instant::now();
    let manifest = read(&export.join("manifest.json"))?;
    let cp = member(&export, &manifest, "contract")?;
    let wp = member(&export, &manifest, "weights")?;
    let lp = member(&export, &manifest, "last_layer")?;
    let cb = bounded(&cp)?;
    let wb = bounded(&wp)?;
    let c: Value = serde_json::from_slice(&cb)?;
    let w: Value = serde_json::from_slice(&wb)?;
    let contract_hash = sha(&cb);
    guard(&manifest, &c, &w, &contract_hash, diagnostic)?;
    if manifest["calibrated"] == true {
        let calibration = member(&export, &manifest, "calibration")?;
        guard_calibration(&manifest, &w, &read(&calibration)?, &contract_hash)?;
    }
    let verify_start = Instant::now();
    encoder::verify(&parent)?;
    let verification_seconds = verify_start.elapsed().as_secs_f64();
    let load_start = Instant::now();
    let encoder = encoder::Encoder::load_with_overlay(&parent, Some(&lp))?;
    let encoder_load_seconds = load_start.elapsed().as_secs_f64();
    let heads = WarmSharedModel::from_json(&wb)?;
    let mut tokenizer = Tokenizer::from_file(parent.join("classifier/tokenizer.json"))
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tokenizer
        .with_truncation(None)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tokenizer.with_padding(None);
    let rb = bounded(input)?;
    let mut seen = BTreeSet::new();
    let mut requests = Vec::new();
    for line in std::str::from_utf8(&rb)?.lines() {
        let r: Request = serde_json::from_str(line)?;
        ensure!(
            !r.id.is_empty() && seen.insert(r.id.clone()),
            "empty/duplicate id"
        );
        requests.push(r);
    }
    ensure!(!requests.is_empty(), "empty requests");
    fs::create_dir(out)?;
    let binding = json!({"export_manifest_sha256":encoder::sha(&export.join("manifest.json"))?,"parent_artifact_id":encoder::ID,"parent_manifest_sha256":encoder::MANIFEST,"last_layer_sha256":manifest["last_layer"]["sha256"],"weights_sha256":sha(&wb),"contract_sha256":contract_hash,"requests_sha256":sha(&rb),"implementation_sha256":sha(include_bytes!("last_block.rs")),"encoder_wrapper_sha256":sha(include_bytes!("../encoder.rs")),"overlay_loader_sha256":sha(include_bytes!("../last_block_overlay.rs")),"graph_sha256":sha(include_bytes!("../modernbert.rs")),"layernorm_sha256":sha(include_bytes!("../reference_layer_norm.rs")),"shared_heads_runtime_sha256":sha(include_bytes!("../../../../grammar/crates/grammar-a-baseline/src/warm_shared.rs")),"shared_math_runtime_sha256":sha(include_bytes!("../../../../grammar/crates/grammar-a-baseline/src/shared.rs")),"cargo_lock_sha256":sha(include_bytes!("../../Cargo.lock"))});
    put(&out.join("started.json"), &binding)?;
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(out.join("predictions.jsonl"))?;
    let mut supported = 0;
    let mut encoder_seconds = 0.;
    let mut heads_seconds = 0.;
    let mut token_seconds = 0.;
    for r in &requests {
        let begin = Instant::now();
        let (ids, status) = encoder::token_ids(&tokenizer, &r.text)?;
        let token_s = begin.elapsed().as_secs_f64();
        token_seconds += token_s;
        let mut result = json!({"id":r.id,"text_sha256":sha(r.text.as_bytes()),"status":if status=="ok"{"ok"}else{"unsupported_input"},"reason":if status=="ok"{Value::Null}else{json!(status)},"token_count":ids.len(),"token_ids_sha256_le_u32":sha(&ids.iter().flat_map(|v|v.to_le_bytes()).collect::<Vec<_>>()),"prediction":null,"calibrated":manifest["calibrated"],"truncation":false});
        if status == "ok" {
            let begin = Instant::now();
            let e = encoder.forward(&ids)?;
            let encoder_s = begin.elapsed().as_secs_f64();
            encoder_seconds += encoder_s;
            let mut x = e.h.iter().map(|&v| v as f64).collect::<Vec<_>>();
            x.resize(2588, 0.);
            let numeric = SharedInput {
                contract_sha256: contract_hash.clone(),
                x,
            };
            let begin = Instant::now();
            let prediction = heads.predict(&numeric)?;
            let head_s = begin.elapsed().as_secs_f64();
            heads_seconds += head_s;
            result["prediction"] = serde_json::to_value(prediction)?;
            result["timing_seconds"] =
                json!({"tokenization":token_s,"encoder":encoder_s,"shared_A_heads":head_s});
            if diagnostic {
                result["diagnostics"] = json!({"h_raw_f32":e.h,"z_shared_f64":heads.encode(&numeric)?,"token_ids":ids,"all_heads_share_one_z":true});
            }
            supported += 1;
        }
        serde_json::to_writer(&mut f, &result)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    encoder::verify(&parent)?;
    for key in ["contract", "weights", "last_layer"] {
        member(&export, &manifest, key)?;
    }
    if manifest["calibrated"] == true {
        member(&export, &manifest, "calibration")?;
    }
    ensure!(
        encoder::sha(input)? == sha(&rb)
            && encoder::sha(&export.join("manifest.json"))? == binding["export_manifest_sha256"],
        "input changed during inference"
    );
    let report = json!({"schema":"slop_ninja_last_block_rust_run_v1","complete":true,"bindings":binding,"requested":requests.len(),"supported":supported,"unsupported":requests.len()-supported,"predictions_sha256":encoder::sha(&out.join("predictions.jsonl"))?,"deployment_qualified":false,"calibrated":manifest["calibrated"],"timing_seconds":{"parent_verification":verification_seconds,"encoder_construction":encoder_load_seconds,"tokenization_total":token_seconds,"encoder_forward_total":encoder_seconds,"shared_A_heads_total":heads_seconds,"whole_run":started.elapsed().as_secs_f64()},"size_bytes":{"parent_safetensors":fs::metadata(parent.join("classifier/model.safetensors"))?.len(),"last_layer":fs::metadata(&lp)?.len(),"heads_json":wb.len(),"loaded_head_numeric_f64":heads.numeric_weight_bytes(),"binary":fs::metadata(std::env::current_exe()?)?.len()},"runtime":"Rust Candle FP32 immutable V6 with exact layer21 overlay and WarmSharedModel f64 shared heads"});
    let mut report = report;
    report["numeric_profile"] = json!(if cfg!(feature = "mkl") {
        "linux_mkl_aten_2_9_1_default_reductions_v1"
    } else {
        "candle_default_qualified_parent_v1"
    });
    put(&out.join("receipt.json"), &report)?;
    println!(
        "{}",
        json!({"complete":true,"supported":supported,"unsupported":requests.len()-supported})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 4 || (a.len() == 5 && a[4] == "--uncalibrated-diagnostics"),
        "PARENT_ENCODER EXPORT_DIR REQUESTS NEW_OUT [--uncalibrated-diagnostics]"
    );
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
        a.len() == 5,
    )
}

#[cfg(test)]
mod calibration_tests {
    use super::*;
    #[test]
    fn exported_thresholds_and_temperature_require_matching_calibration() {
        let m = json!({"condition":"adapt_last_block","seed":17,"selected_checkpoint_sha256":"checkpoint"});
        let w = json!({"temperature":1.2,"thresholds":{"human":0.7,"ai":0.8}});
        let cal = json!({"schema":"slop_ninja_last_block_calibration_v1","complete":true,
            "fit_split":"calibration","final_test_opened":false,"origin_calibrated":true,
            "condition":"adapt_last_block","seed":17,"selected_checkpoint_sha256":"checkpoint",
            "contract_sha256":"contract","temperature":1.2,
            "roles":{"human":{"qualified":true,"threshold":0.7},"ai":{"qualified":true,"threshold":0.8}}});
        guard_calibration(&m, &w, &cal, "contract").unwrap();
        let mut wrong = cal.clone();
        wrong["temperature"] = json!(1.);
        assert!(guard_calibration(&m, &w, &wrong, "contract").is_err());
        let mut wrong = cal.clone();
        wrong["roles"]["ai"]["threshold"] = json!(0.7);
        assert!(guard_calibration(&m, &w, &wrong, "contract").is_err());
        let mut wrong = cal;
        wrong["selected_checkpoint_sha256"] = json!("different");
        assert!(guard_calibration(&m, &w, &wrong, "contract").is_err());
    }
}
