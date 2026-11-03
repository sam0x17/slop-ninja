//! Single-document CPU V6 -> raw h -> text-only residual shared A inference.
mod encoder;
mod last_block_overlay;
#[allow(dead_code)]
mod modernbert;
mod reference_layer_norm;
#[allow(dead_code)]
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
    path::Path,
    time::Instant,
};
use tokenizers::Tokenizer;

const TEXT: usize = 768;
const CFG: usize = 1820;
const MAX_FILE: u64 = 64 * 1024 * 1024;

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
    fs::File::open(p)?.take(MAX_FILE + 1).read_to_end(&mut b)?;
    ensure!(b.len() as u64 <= MAX_FILE, "input exceeds 64 MiB boundary");
    Ok(b)
}
fn write_new(path: &Path, value: &Value) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn contract_guard(c: &Value, w: &Value, compact_hash: &str, manifest: &Value) -> Result<()> {
    ensure!(
        c["schema"] == "slop_ninja_shared_a_numeric_contract_v1"
            && c["warm_start_schema"] == "slop_ninja_raw_v6_cfg_residual_input_v1"
            && c["ablation"] == "text"
            && w["input_mode"] == "text",
        "TEXT only; combined is unsupported"
    );
    ensure!(
        w["schema"] == "slop_ninja_v6_residual_a_weights_v1"
            && w["control"] == "joint"
            && w.get("control_protocol_sha256").is_none_or(Value::is_null),
        "actual A requires joint weights, not diagnostic controls"
    );
    for k in ["origin", "share", "mixed_fractional_share", "human", "ai"] {
        ensure!(
            w["supervision"][k].as_u64().is_some_and(|n| n > 0),
            "joint A requires positive {k} supervision"
        );
    }
    ensure!(
        w["contract_sha256"] == compact_hash,
        "weights/contract hash mismatch"
    );
    ensure!(
        c["input_dimension"] == TEXT + CFG
            && w["architecture"]["dimension"] == TEXT + CFG
            && w["architecture"]["raw_text_dimension"] == TEXT
            && w["architecture"]["embedding"] == TEXT,
        "unsupported raw768 + zero1820 input layout"
    );
    let blocks = &c["coordinate_blocks"];
    ensure!(
        blocks.as_array().is_some_and(|b| b.len() == 2)
            && blocks[0]["start_inclusive"] == 0
            && blocks[0]["end_exclusive"] == TEXT
            && blocks[1]["start_inclusive"] == TEXT
            && blocks[1]["end_exclusive"] == TEXT + CFG,
        "coordinate blocks differ"
    );
    let r = &c["representation_contract"];
    ensure!(
        r["schema"] == "slop_ninja_v6_penultimate_contract_v1"
            && r["artifact_id"] == encoder::ID
            && r["artifact_files_sha256"] == manifest["files"]
            && r["dimension"] == TEXT
            && r["embedding_dtype"] == "float32"
            && r["hook"]["module"] == "model.classifier"
            && r["hook"]["argument"] == 0
            && r["hook"]["shape"] == json!([1, TEXT])
            && r["hook"]["normalization"] == "none"
            && r["hook"]["projection"] == "none"
            && r["input"]["truncation"] == false
            && r["input"]["max_tokens_including_special_tokens"] == 2048
            && r["model"]["temperature"] == encoder::TEMP
            && w["base_temperature"] == encoder::TEMP,
        "unqualified classifier-input representation"
    );
    let n = &c["embedding_normalization"];
    ensure!(
        n["dimension"] == TEXT
            && n["fit"] == "train_query_only_equal_source_family_weight"
            && n["development_used_for_fit"] == false
            && n["calibration_used_for_fit"] == false
            && n["reference_used_for_fit"] == false
            && n["labels_used_for_fit"] == false
            && n["final_test_opened"] == false
            && c["final_test_opened"] == false,
        "unqualified frozen Train normalization"
    );
    for (ck, wk) in [("mean", "text_mean"), ("scale", "text_scale")] {
        let cv = n[ck].as_array().context("normalization array")?;
        let wv = w[wk].as_array().context("weight normalization array")?;
        ensure!(
            cv.len() == TEXT && wv.len() == TEXT,
            "normalization dimension"
        );
        for (a, b) in cv.iter().zip(wv) {
            let a = a.as_f64().context("normalization scalar")?;
            let b = b.as_f64().context("normalization scalar")?;
            ensure!(
                a.is_finite() && b.is_finite() && (a as f32) as f64 == b,
                "weight normalization differs from frozen FP32 buffer"
            );
        }
    }
    for role in ["human", "ai"] {
        ensure!(
            w["profiles"][role] == c[format!("{role}_profiles")],
            "profile roster changed"
        );
    }
    Ok(())
}
fn numeric(h: &[f32], hash: &str) -> Result<SharedInput> {
    ensure!(
        h.len() == TEXT && h.iter().all(|v| v.is_finite()),
        "invalid raw classifier-input h"
    );
    let mut x = h.iter().map(|&v| v as f64).collect::<Vec<_>>();
    x.resize(TEXT + CFG, 0.0);
    Ok(SharedInput {
        contract_sha256: hash.into(),
        x,
    })
}
fn requests(b: &[u8]) -> Result<Vec<Request>> {
    let mut ids = BTreeSet::new();
    let mut out = Vec::new();
    for line in std::str::from_utf8(b)?.lines() {
        let r: Request =
            serde_json::from_str(line).context("expected exact id/text JSONL request")?;
        ensure!(
            !r.id.is_empty() && ids.insert(r.id.clone()),
            "empty or duplicate request ID"
        );
        out.push(r);
    }
    ensure!(!out.is_empty(), "empty request file");
    Ok(out)
}
fn source_binding() -> Result<Value> {
    for (bytes, pin) in [
        (
            include_bytes!("modernbert.rs").as_slice(),
            "f6abca9ee20f845fa38e10963318dddeedb966b6c517263ced2b67c772e30a5f",
        ),
        (
            include_bytes!("reference_layer_norm.rs").as_slice(),
            "20aac3ea2e5a5ab071581fbb650b119ff89dcb8335079f8b6ccca21d7933501f",
        ),
        (
            include_bytes!("trace.rs").as_slice(),
            "d581e9f3bc2c88858fee1569b6a6afea4bf6644902c2e666c28a8da48172ffea",
        ),
    ] {
        ensure!(sha(bytes) == pin, "qualified graph/operator source changed");
    }
    Ok(
        json!({"main_rs":sha(include_bytes!("main.rs")),"encoder_rs":sha(include_bytes!("encoder.rs")),
        "modernbert_rs":sha(include_bytes!("modernbert.rs")),"reference_layer_norm_rs":sha(include_bytes!("reference_layer_norm.rs")),
        "trace_rs":sha(include_bytes!("trace.rs")),"cargo_lock":sha(include_bytes!("../Cargo.lock")),
        "warm_shared_rs":sha(include_bytes!("../../../grammar/crates/grammar-a-baseline/src/warm_shared.rs")),
        "shared_rs":sha(include_bytes!("../../../grammar/crates/grammar-a-baseline/src/shared.rs"))}),
    )
}
fn run(
    artifact: &Path,
    weights: &Path,
    contract: &Path,
    input: &Path,
    out: &Path,
    diagnostics: bool,
) -> Result<()> {
    ensure!(
        !out.exists() && !out.is_symlink(),
        "new output directory required"
    );
    let artifact = artifact.canonicalize()?;
    ensure!(
        !out.parent()
            .context("output parent")?
            .canonicalize()?
            .starts_with(&artifact),
        "output inside immutable encoder"
    );
    for name in [
        "RAYON_NUM_THREADS",
        "CANDLE_NUM_THREADS",
        "OMP_NUM_THREADS",
        "MKL_NUM_THREADS",
    ] {
        ensure!(
            std::env::var(name).as_deref() == Ok("1"),
            "set {name}=1 for the qualified CPU profile"
        );
    }
    let sources = source_binding()?;
    let started = Instant::now();
    let cb = bounded(contract)?;
    let wb = bounded(weights)?;
    let rb = bounded(input)?;
    let c: Value = serde_json::from_slice(&cb)?;
    let w: Value = serde_json::from_slice(&wb)?;
    let contract_hash = sha(&serde_json::to_vec(&c)?);
    let rows = requests(&rb)?;
    // Reject controls/combined before mapping weights or running any encoder.
    let declared: Value = serde_json::from_slice(&bounded(&artifact.join("manifest.json"))?)?;
    contract_guard(&c, &w, &contract_hash, &declared)?;
    let hash_start = Instant::now();
    let manifest = encoder::verify(&artifact)?;
    let verify_seconds = hash_start.elapsed().as_secs_f64();
    let start = Instant::now();
    let model = WarmSharedModel::from_json(&wb)?;
    let head_load_seconds = start.elapsed().as_secs_f64();
    let start = Instant::now();
    let encoder = encoder::Encoder::load(&artifact)?;
    let encoder_load_seconds = start.elapsed().as_secs_f64();
    let mut tokenizer = Tokenizer::from_file(artifact.join("classifier/tokenizer.json"))
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tokenizer
        .with_truncation(None)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tokenizer.with_padding(None);
    let binding = json!({"encoder_artifact_id":encoder::ID,"encoder_manifest_sha256":encoder::MANIFEST,
        "encoder_weight_sha256":manifest["files"]["classifier/model.safetensors"],
        "weights_file_sha256":sha(&wb),"contract_file_sha256":sha(&cb),"contract_compact_sha256":contract_hash,
        "requests_file_sha256":sha(&rb),"sources":sources});
    fs::create_dir(out)?;
    write_new(
        &out.join("started.json"),
        &json!({"schema":"slop_ninja_a_v6_cpu_started_v1","bindings":binding,
        "requested":rows.len(),"diagnostics":diagnostics,"completed":false}),
    )?;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out.join("predictions.jsonl"))?;
    let mut supported = 0;
    let mut encoder_seconds = 0.;
    let mut head_seconds = 0.;
    let mut token_seconds = 0.;
    for request in &rows {
        let row_start = Instant::now();
        let token_start = Instant::now();
        let (ids, status) = encoder::token_ids(&tokenizer, &request.text)?;
        let token_s = token_start.elapsed().as_secs_f64();
        token_seconds += token_s;
        let token_bytes = ids.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>();
        let mut result = json!({"schema":"slop_ninja_a_v6_cpu_result_v1","id":request.id,"text_sha256":sha(request.text.as_bytes()),
            "input_utf8_bytes":request.text.len(),"token_count":ids.len(),"token_ids_sha256_le_u32":sha(&token_bytes),
            "status":"unsupported_input","reason":status,"prediction":null,"bindings":binding,
            "input_mode":"text","raw_text_dimension":TEXT,"zero_cfg_coordinates":CFG,"truncation":false,
            "timing_seconds":{"tokenization":token_s,"encoder":0.,"shared_A_heads":0.}});
        if status == "ok" {
            let start = Instant::now();
            let e = encoder.forward(&ids)?;
            let encode_s = start.elapsed().as_secs_f64();
            encoder_seconds += encode_s;
            let raw = numeric(&e.h, &contract_hash)?;
            let start = Instant::now();
            let prediction = model.predict(&raw)?;
            let heads_s = start.elapsed().as_secs_f64();
            head_seconds += heads_s;
            result["status"] = "ok".into();
            result["reason"] = Value::Null;
            result["prediction"] = serde_json::to_value(prediction)?;
            result["classifier_input_h_sha256_le_f32"] =
                sha(&e.h.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>()).into();
            result["timing_seconds"]["encoder"] = encode_s.into();
            result["timing_seconds"]["shared_A_heads"] = heads_s.into();
            if diagnostics {
                let diag_start = Instant::now();
                let z = model.encode(&raw)?;
                result["diagnostics"] = json!({"h_raw_f32":e.h,"z_shared_f64":z,"v6_logits_f32":e.logits,"v6_probabilities_f64":e.probabilities,
                    "token_ids":ids,"h_normalized_before_adapter":false,"all_heads_share_one_z":true});
                result["timing_seconds"]["diagnostic_reencode"] =
                    diag_start.elapsed().as_secs_f64().into();
            }
            supported += 1;
        } else if diagnostics {
            result["diagnostics"] = json!({"token_ids":ids});
        }
        result["timing_seconds"]["total_before_receipt_write"] =
            row_start.elapsed().as_secs_f64().into();
        serde_json::to_writer(&mut f, &result)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    // Reverify immutable files after forwarding; a partial run never gets a complete receipt.
    encoder::verify(&artifact)?;
    ensure!(
        encoder::sha(weights)? == sha(&wb)
            && encoder::sha(contract)? == sha(&cb)
            && encoder::sha(input)? == sha(&rb),
        "input file changed during inference"
    );
    let report = json!({"schema":"slop_ninja_a_v6_cpu_run_v1","complete":true,"bindings":binding,
        "requested":rows.len(),"supported":supported,"unsupported":rows.len()-supported,
        "predictions_sha256":encoder::sha(&out.join("predictions.jsonl"))?,"inputs_reverified_unchanged":true,
        "runtime":"Rust Candle FP32 frozen V6 plus existing WarmSharedModel f64 CPU; one unpadded document",
        "scope":{"mode":"text","control":"joint","zero_cfg_coordinates":CFG,"truncation":false,"deployment_qualified":false},
        "timing_seconds":{"initial_encoder_artifact_verification":verify_seconds,"encoder_construction":encoder_load_seconds,
            "head_construction":head_load_seconds,"tokenization_total":token_seconds,"encoder_forward_total":encoder_seconds,
            "shared_A_heads_total":head_seconds,"whole_run_including_hashes_and_receipts":started.elapsed().as_secs_f64()},
        "size_bytes":{"encoder_safetensors":fs::metadata(artifact.join("classifier/model.safetensors"))?.len(),
            "heads_json":wb.len(),"loaded_head_numeric_f64":model.numeric_weight_bytes(),"binary":fs::metadata(std::env::current_exe()?)?.len()}});
    write_new(&out.join("receipt.json"), &report)?;
    println!(
        "{}",
        json!({"complete":true,"supported":supported,"unsupported":rows.len()-supported})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        (a.len() == 5 || a.len() == 6 && a[5] == "--diagnostics"),
        "slop_ninja_a_v6_cpu ENCODER WEIGHTS.json CONTRACT.json REQUESTS.jsonl NEW_OUT [--diagnostics]"
    );
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
        Path::new(&a[4]),
        a.len() == 6,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_requests_reject_labels_and_duplicate_ids_without_trimming_text() {
        let r = requests(b"{\"id\":\"a\",\"text\":\"  exact\\n\"}\n").unwrap();
        assert_eq!(r[0].text, "  exact\n");
        assert!(requests(b"{\"id\":\"a\",\"text\":\"x\",\"label\":0}\n").is_err());
        assert!(
            requests(b"{\"id\":\"a\",\"text\":\"x\"}\n{\"id\":\"a\",\"text\":\"y\"}\n").is_err()
        );
    }
    #[test]
    fn raw_h_is_not_normalized_and_cfg_is_explicit_zero() {
        let h = (0..TEXT).map(|i| i as f32 / 17.).collect::<Vec<_>>();
        let x = numeric(&h, "hash").unwrap();
        assert_eq!(x.x.len(), 2588);
        assert!(x.x[TEXT..].iter().all(|v| *v == 0.));
        assert!(x.x[..TEXT].iter().zip(h).all(|(a, b)| *a == b as f64));
        assert!(numeric(&vec![f32::NAN; TEXT], "hash").is_err());
        assert!(numeric(&[0.; 4], "hash").is_err());
    }
    fn fixture() -> (Value, Value, Value) {
        let manifest = json!({"files":{"classifier/model.safetensors":"synthetic"}});
        let c = json!({"schema":"slop_ninja_shared_a_numeric_contract_v1","warm_start_schema":"slop_ninja_raw_v6_cfg_residual_input_v1",
            "ablation":"text","input_dimension":2588,"coordinate_blocks":[{"start_inclusive":0,"end_exclusive":768},{"start_inclusive":768,"end_exclusive":2588}],
            "final_test_opened":false,"human_profiles":["synthetic-human"],"ai_profiles":["synthetic-ai"],
            "representation_contract":{"schema":"slop_ninja_v6_penultimate_contract_v1","artifact_id":encoder::ID,"artifact_files_sha256":manifest["files"],
                "dimension":768,"embedding_dtype":"float32","hook":{"module":"model.classifier","argument":0,"shape":[1,768],"normalization":"none","projection":"none"},
                "input":{"truncation":false,"max_tokens_including_special_tokens":2048},"model":{"temperature":encoder::TEMP}},
            "embedding_normalization":{"dimension":768,"fit":"train_query_only_equal_source_family_weight","development_used_for_fit":false,
                "calibration_used_for_fit":false,"reference_used_for_fit":false,"labels_used_for_fit":false,"final_test_opened":false,
                "mean":vec![0.;768],"scale":vec![1.;768]}});
        let w = json!({"schema":"slop_ninja_v6_residual_a_weights_v1","input_mode":"text","control":"joint","contract_sha256":"bound",
            "architecture":{"dimension":2588,"raw_text_dimension":768,"embedding":768},"base_temperature":encoder::TEMP,
            "supervision":{"origin":1,"share":1,"mixed_fractional_share":1,"human":1,"ai":1},"text_mean":vec![0.;768],"text_scale":vec![1.;768],
            "profiles":{"human":["synthetic-human"],"ai":["synthetic-ai"]}});
        (c, w, manifest)
    }
    #[test]
    fn actual_a_rejects_combined_controls_missing_supervision_and_changed_contract() {
        let (c, w, m) = fixture();
        contract_guard(&c, &w, "bound", &m).unwrap();
        for mode in ["origin_only", "attribution_only"] {
            let mut bad = w.clone();
            bad["control"] = mode.into();
            assert!(contract_guard(&c, &bad, "bound", &m).is_err());
        }
        for k in ["origin", "share", "mixed_fractional_share", "human", "ai"] {
            let mut bad = w.clone();
            bad["supervision"][k] = 0.into();
            assert!(contract_guard(&c, &bad, "bound", &m).is_err());
        }
        let mut bad = c.clone();
        bad["ablation"] = "combined".into();
        assert!(contract_guard(&bad, &w, "bound", &m).is_err());
        let mut bad = w.clone();
        bad["input_mode"] = "combined".into();
        assert!(contract_guard(&c, &bad, "bound", &m).is_err());
        assert!(contract_guard(&c, &w, "other", &m).is_err());
    }
    #[test]
    fn representation_and_frozen_normalization_are_bound() {
        let (c, w, m) = fixture();
        let mut bad = c.clone();
        bad["representation_contract"]["hook"]["module"] = "model.pooler".into();
        assert!(contract_guard(&bad, &w, "bound", &m).is_err());
        let mut bad = c.clone();
        bad["embedding_normalization"]["development_used_for_fit"] = true.into();
        assert!(contract_guard(&bad, &w, "bound", &m).is_err());
        let mut bad = w.clone();
        bad["text_mean"][0] = 1.into();
        assert!(contract_guard(&c, &bad, "bound", &m).is_err());
        let mut bad = m;
        bad["files"]["classifier/model.safetensors"] = "changed".into();
        assert!(contract_guard(&c, &w, "bound", &bad).is_err());
    }
    #[test]
    fn qualified_sources_are_unchanged() {
        source_binding().unwrap();
    }
}
