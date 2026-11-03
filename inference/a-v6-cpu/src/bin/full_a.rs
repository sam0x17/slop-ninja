//! Rust CPU inference for an exported full-A (E1/E2, spans-v1) derivative.
//! PARENT_ENCODER EXPORT_DIR REQUESTS.jsonl NEW_OUT [--uncalibrated-diagnostics] [--f32-diagnostic]
//!
//! Reads export v3 (heads v2, calibration v3), export v4 (heads v3,
//! calibration v3, `tower.safetensors`: the spans-S2 span tower) and, for
//! replay of the R0 release package, export v2 (heads v1, calibration v2, no
//! token head). The pairing, and whether the span tower branch runs, are keyed
//! off the export's declared schema only. With export v4 the token head reads
//! the tower's normed states (the tower runs from the hidden state entering its
//! first copied layer, with the encoder's masks and positions); the document
//! outputs read the encoder alone, as for v3. Heads of every schema are read by
//! `grammar_a_baseline::full_a_tower` (`full_a_tower.rs`), which passes heads
//! v1 and v2 unchanged to the pinned heads runtime (`full_a.rs`,
//! `full_a_spans.rs`) and reads the heads v3 tower declaration itself. With a spans-v1
//! token head the prediction adds `sentences` and `spans` (exact char and
//! UTF-8 byte offsets, AI share and confidence percents); `span_status`
//! records why span emission was unsupported for a request while its document
//! outputs stand.
//!
//! The qualified numeric profile is FP64 (`candle_f64_full_a_v2`): FP32 weights
//! widened exactly, every encoder operation in FP64 except the rotary tables,
//! which are HF's float32 tables widened exactly. `--f32-diagnostic` runs the
//! V6 FP32 graph instead and requires `--uncalibrated-diagnostics`.
#[path = "../encoder.rs"]
#[allow(dead_code)]
mod encoder;
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
#[allow(dead_code)]
#[path = "../trace.rs"]
mod trace;
use anyhow::{Context, Result, ensure};
use grammar_a_baseline::full_a as heads_schema;
use grammar_a_baseline::full_a_tower::{self as tower_schema, SpanTowerHeads, TokenStates};
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
fn bounded(p: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    fs::File::open(p)?.take(limit + 1).read_to_end(&mut b)?;
    ensure!(b.len() as u64 <= limit, "input exceeds size limit");
    Ok(b)
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
/// The span tower file of a v4 export, bound to the manifest's digest like every other member; no other
/// export schema reads a tower file.
fn tower_file(root: &Path, m: &Value, export: Export) -> Result<Option<PathBuf>> {
    Ok(if export.span_tower() {
        Some(member(root, m, "span_tower")?)
    } else {
        None
    })
}
/// The digests of the sources this binary was built from, bound into every run.
fn source_bindings() -> Value {
    json!({"implementation_sha256":sha(include_bytes!("full_a.rs")),
        "full_encoder_sha256":sha(include_bytes!("../full_encoder.rs")),
        "graph_sha256":sha(include_bytes!("../modernbert.rs")),
        "layernorm_sha256":sha(include_bytes!("../reference_layer_norm.rs")),
        "heads_runtime_sha256":sha(include_bytes!("../../../../grammar/crates/grammar-a-baseline/src/full_a.rs")),
        "spans_runtime_sha256":sha(include_bytes!("../../../../grammar/crates/grammar-a-baseline/src/full_a_spans.rs")),
        "calibration_runtime_sha256":sha(include_bytes!("../../../../grammar/crates/grammar-a-baseline/src/involvement_calibration.rs")),
        "tower_runtime_sha256":sha(include_bytes!("../../../../grammar/crates/grammar-a-baseline/src/full_a_tower.rs")),
        "cargo_lock_sha256":sha(include_bytes!("../../Cargo.lock"))})
}
/// The export schemas this runtime reads, from the manifest's declared schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Export {
    /// `slop_ninja_full_a_export_v2` (R0): heads v1, calibration v2, no token head.
    V2,
    /// `slop_ninja_full_a_export_v3`: heads v2, calibration v3, optional token head.
    V3,
    /// `slop_ninja_full_a_export_v4`: heads v3, calibration v3, token head on the span tower.
    V4,
}
impl Export {
    fn of(m: &Value) -> Result<Self> {
        Ok(match m["schema"].as_str() {
            Some("slop_ninja_full_a_export_v2") => Export::V2,
            Some("slop_ninja_full_a_export_v3") => Export::V3,
            Some("slop_ninja_full_a_export_v4") => Export::V4,
            _ => anyhow::bail!("incomplete full-A export"),
        })
    }
    fn heads_schema(self) -> &'static str {
        match self {
            Export::V2 => heads_schema::SCHEMA_V1,
            Export::V3 => heads_schema::SCHEMA,
            Export::V4 => tower_schema::SCHEMA_V3,
        }
    }
    fn calibration_schema(self) -> &'static str {
        match self {
            Export::V2 => "slop_ninja_full_a_calibration_v2",
            Export::V3 | Export::V4 => "slop_ninja_full_a_calibration_v3",
        }
    }
    fn span_tower(self) -> bool {
        self == Export::V4
    }
}
fn guard(m: &Value, c: &Value, w: &Value, contract_hash: &str, diagnostic: bool) -> Result<Export> {
    let export = Export::of(m)?;
    ensure!(
        m["complete"] == true && m["final_test_opened"] == false,
        "incomplete full-A export"
    );
    ensure!(
        w["schema"] == export.heads_schema(),
        "heads schema does not match the export schema"
    );
    // A v2 export predates the spans-v1 token head and its emission spec.
    ensure!(
        export != Export::V2 || (m.get("token_head").is_none() && m.get("span_emission").is_none()),
        "a v2 export has no token head"
    );
    // Only v4 declares the span tower: a tower file whose declaration is the heads' own, and the file the
    // manifest binds is the weights file that declaration names.
    if export.span_tower() {
        ensure!(
            m["span_tower"]["file"].is_string()
                && m["span_tower"]["sha256"].is_string()
                && w.get("span_tower").is_some_and(|t| !t.is_null())
                && m["span_tower"]["declaration"] == w["span_tower"]
                && m["span_tower"]["file"] == w["span_tower"]["weights"]
                && !m["token_head"].is_null(),
            "a v4 export declares its span tower and token head"
        );
    } else {
        ensure!(
            m.get("span_tower").is_none() && w.get("span_tower").is_none(),
            "only a v4 export has a span tower"
        );
    }
    ensure!(
        m["parent_artifact_id"] == encoder::ID
            && m["parent_manifest_sha256"] == encoder::MANIFEST
            && c["parent_artifact_id"] == encoder::ID,
        "unqualified parent encoder"
    );
    ensure!(
        m["calibrated"].is_boolean() && (diagnostic || m["calibrated"] == true),
        "uncalibrated export needs explicit diagnostic mode"
    );
    ensure!(
        c["schema"] == "slop_ninja_full_a_contract_v2"
            && c["input_mode"] == "text"
            && c["raw_dimension"] == 768
            && c["zero_cfg_coordinates"] == 1820
            && c["maximum_tokens_including_specials"] == 2048
            && c["token_truncation"] == false,
        "input meaning differs"
    );
    ensure!(
        w["contract_sha256"] == contract_hash && w["base_temperature"] == encoder::TEMP,
        "heads/contract differ"
    );
    if !diagnostic {
        for key in ["origin", "share", "human", "ai"] {
            // An empty roster (the shared-contract human role) has no head to train.
            if (key == "human" || key == "ai")
                && w["profiles"][key].as_array().is_some_and(|a| a.is_empty())
            {
                continue;
            }
            ensure!(
                w["supervision"][key].as_u64().is_some_and(|n| n > 0),
                "A requires trained {key} supervision"
            );
        }
    }
    let n = &c["embedding_normalization"];
    for (ck, wk) in [("mean", "text_mean"), ("scale", "text_scale")] {
        let cv = n[ck].as_array().context("normalization")?;
        let wv = w[wk].as_array().context("weight normalization")?;
        ensure!(
            cv.len() == 768 && wv.len() == 768,
            "normalization dimension"
        );
        for (a, b) in cv.iter().zip(wv) {
            let (x, y) = (a.as_f64().context("number")?, b.as_f64().context("number")?);
            ensure!((x as f32) as f64 == y, "normalization buffer differs");
        }
    }
    for role in ["human", "ai"] {
        ensure!(
            w["profiles"][role] == c[format!("{role}_profiles")],
            "profile roster differs"
        );
    }
    Ok(export)
}
fn guard_calibration(export: Export, m: &Value, w: &Value, cal: &Value, contract_hash: &str) -> Result<()> {
    // v2 records carry no span emission spec; v3 records carry the exported one.
    let spans = match export {
        Export::V2 => cal.get("spans").is_none(),
        Export::V3 | Export::V4 => cal["spans"] == w["span_emission"],
    };
    ensure!(
        cal["schema"] == export.calibration_schema()
            && cal["complete"] == true
            && cal["fit_split"] == "cal"
            && cal["final_test_opened"] == false
            && cal["arm"] == m["arm"]
            && cal["seed"] == m["seed"]
            && cal["selected_checkpoint_sha256"] == m["selected_checkpoint_sha256"]
            && cal["contract_sha256"] == contract_hash
            && cal["involvement"]["selected"] == w["involvement_calibration"]
            && spans,
        "calibration/checkpoint/involvement map mismatch"
    );
    for role in ["human", "ai"] {
        let r = &cal["roles"][role];
        // An empty roster is marked roster_empty and exported with threshold 1.
        let empty = r["roster_empty"] == true
            && w["profiles"][role].as_array().is_some_and(|a| a.is_empty())
            && w["thresholds"][role] == 1.0;
        ensure!(
            empty || (r["qualified"] == true && r["threshold"] == w["thresholds"][role]),
            "unqualified or changed role threshold"
        );
    }
    Ok(())
}
fn run(
    parent: &Path,
    export: &Path,
    input: &Path,
    out: &Path,
    diagnostic: bool,
    profile: full_encoder::Profile,
) -> Result<()> {
    ensure!(
        profile == full_encoder::Profile::F64 || diagnostic,
        "the FP32 profile is diagnostic only"
    );
    ensure!(!out.exists(), "new output required");
    ensure!(
        diagnostic || !cfg!(feature = "mkl"),
        "unqualified MKL profile requires explicit diagnostics"
    );
    let parent = parent.canonicalize()?;
    let export = export.canonicalize()?;
    for name in [
        "RAYON_NUM_THREADS",
        "CANDLE_NUM_THREADS",
        "OMP_NUM_THREADS",
        "MKL_NUM_THREADS",
    ] {
        ensure!(std::env::var(name).as_deref() == Ok("1"), "set {name}=1");
    }
    if cfg!(feature = "mkl") {
        ensure!(
            std::env::var("MKL_CBWR").as_deref() == Ok("COMPATIBLE"),
            "MKL profile requires MKL_CBWR=COMPATIBLE"
        );
    }
    let started = Instant::now();
    let manifest: Value =
        serde_json::from_slice(&bounded(&export.join("manifest.json"), 1 << 24)?)?;
    let cp = member(&export, &manifest, "contract")?;
    let hp = member(&export, &manifest, "heads")?;
    let ep = member(&export, &manifest, "encoder")?;
    let cb = bounded(&cp, 64 << 20)?;
    let hb = bounded(&hp, 64 << 20)?;
    let c: Value = serde_json::from_slice(&cb)?;
    let w: Value = serde_json::from_slice(&hb)?;
    let contract_hash = sha(&cb);
    let schema = guard(&manifest, &c, &w, &contract_hash, diagnostic)?;
    if manifest["calibrated"] == true {
        let calibration = member(&export, &manifest, "calibration")?;
        guard_calibration(
            schema,
            &manifest,
            &w,
            &serde_json::from_slice(&fs::read(calibration)?)?,
            &contract_hash,
        )?;
    }
    let heads = SpanTowerHeads::from_json(&hb)?;
    ensure!(
        heads.has_span_head() == !w["span_head"].is_null()
            && heads.has_token_head() == !w["token_head"].is_null()
            && manifest["token_head"] == w["token_head"]
            && manifest["span_emission"] == w["span_emission"]
            && (schema != Export::V2 || !heads.has_token_head())
            && heads.span_tower().is_some() == schema.span_tower(),
        "span head declaration"
    );
    let tp = tower_file(&export, &manifest, schema)?;
    let verify_start = Instant::now();
    encoder::verify(&parent)?;
    let parent_tensors = parent.join("classifier/model.safetensors");
    let roster = full_encoder::validate_roster(&parent_tensors, &ep)?;
    let tower_roster = match (&tp, heads.span_tower()) {
        (Some(p), Some(spec)) => Some(full_encoder::validate_tower_roster(&parent_tensors, p, spec)?),
        _ => None,
    };
    let verification_seconds = verify_start.elapsed().as_secs_f64();
    let load_start = Instant::now();
    let model = match tp.as_deref().zip(heads.span_tower()) {
        None => full_encoder::FullEncoder::load(&parent, &ep, profile)?,
        tower => full_encoder::FullEncoder::load_with_tower(&parent, &ep, tower, profile)?,
    };
    ensure!(model.has_tower() == schema.span_tower(), "span tower branch");
    let encoder_load_seconds = load_start.elapsed().as_secs_f64();
    let mut tokenizer = Tokenizer::from_file(parent.join("classifier/tokenizer.json"))
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tokenizer
        .with_truncation(None)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tokenizer.with_padding(None);
    let rb = bounded(input, 64 << 20)?;
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
    let mut binding = json!({"export_manifest_sha256":encoder::sha(&export.join("manifest.json"))?,
        "parent_artifact_id":encoder::ID,"parent_manifest_sha256":encoder::MANIFEST,
        "encoder_sha256":manifest["encoder"]["sha256"],"heads_sha256":sha(&hb),"contract_sha256":contract_hash,
        "requests_sha256":sha(&rb),"encoder_tensors":roster});
    let Value::Object(sources) = source_bindings() else {
        unreachable!("source bindings are an object")
    };
    binding.as_object_mut().context("run binding")?.extend(sources);
    if schema.span_tower() {
        binding["tower_sha256"] = manifest["span_tower"]["sha256"].clone();
        binding["tower_tensors"] = json!(tower_roster);
        binding["span_tower_declaration"] = w["span_tower"].clone();
    }
    put(&out.join("started.json"), &binding)?;
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(out.join("predictions.jsonl"))?;
    let (mut supported, mut encoder_seconds, mut heads_seconds, mut token_seconds) =
        (0, 0., 0., 0.);
    let (mut token_head_seconds, mut decoder_seconds, mut span_unsupported) = (0., 0., 0);
    let mut tower_seconds = 0.;
    for r in &requests {
        let begin = Instant::now();
        let (ids, status) = encoder::token_ids(&tokenizer, &r.text)?;
        let (words, offsets) = if status == "ok" {
            let encoded = tokenizer
                .encode_char_offsets(r.text.as_str(), true)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            ensure!(encoded.get_ids() == ids.as_slice(), "offset encoding differs");
            (
                full_encoder::word_mask(&r.text, &ids, encoded.get_offsets())?,
                encoded.get_offsets().to_vec(),
            )
        } else {
            (Vec::new(), Vec::new())
        };
        let token_s = begin.elapsed().as_secs_f64();
        token_seconds += token_s;
        let mut result = json!({"id":r.id,"text_sha256":sha(r.text.as_bytes()),
            "status":if status=="ok"{"ok"}else{"unsupported_input"},"reason":if status=="ok"{Value::Null}else{json!(status)},
            "token_count":ids.len(),"token_ids_sha256_le_u32":sha(&ids.iter().flat_map(|v|v.to_le_bytes()).collect::<Vec<_>>()),
            "prediction":null,"calibrated":manifest["calibrated"],"truncation":false});
        if status == "ok" {
            let begin = Instant::now();
            let e = model.forward(&ids)?;
            // The encoder's own time; the tower's (v4 only) is reported beside it.
            let tower_s = e.tower_seconds;
            let encoder_s = begin.elapsed().as_secs_f64() - tower_s;
            encoder_seconds += encoder_s;
            tower_seconds += tower_s;
            let begin = Instant::now();
            let h = e.h;
            let out = heads.predict(&h, &e.tokens, &full_encoder::content_mask(&ids), &words)?;
            let head_s = begin.elapsed().as_secs_f64();
            heads_seconds += head_s;
            result["prediction"] = serde_json::to_value(&out.prediction)?;
            result["timing_seconds"] =
                json!({"tokenization":token_s,"encoder":encoder_s,"full_A_heads":head_s});
            if schema.span_tower() {
                result["timing_seconds"]["span_tower"] = json!(tower_s);
            }
            let mut emission = Value::Null;
            if heads.has_token_head() {
                let begin = Instant::now();
                let states = TokenStates {
                    final_norm: &e.tokens,
                    span_tower: e.tower_tokens.as_deref(),
                };
                let logits = heads.token_logits(&states, &words)?.context("token head")?;
                let token_s = begin.elapsed().as_secs_f64();
                let begin = Instant::now();
                match heads.emit_spans(&r.text, &offsets, &words, &logits, &out.prediction) {
                    Ok(x) => {
                        result["prediction"]["sentences"] = serde_json::to_value(&x.sentences)?;
                        result["prediction"]["spans"] = serde_json::to_value(&x.spans)?;
                        result["span_status"] = json!("ok");
                        emission = json!({"status":"ok","rule":x.rule,"words":x.words,"tilt_bias":x.tilt_bias,
                            "share_target":x.share_target,"near_threshold":x.near_threshold,
                            "diagnostics":x.diagnostics});
                    }
                    Err(error) => {
                        result["prediction"]["sentences"] = Value::Null;
                        result["prediction"]["spans"] = Value::Null;
                        result["span_status"] = json!({"unsupported":error.to_string()});
                        emission = json!({"status":"unsupported","reason":error.to_string()});
                        span_unsupported += 1;
                    }
                }
                let decode_s = begin.elapsed().as_secs_f64();
                token_head_seconds += token_s;
                decoder_seconds += decode_s;
                result["timing_seconds"]["token_head"] = json!(token_s);
                result["timing_seconds"]["span_decoder"] = json!(decode_s);
            }
            if diagnostic {
                result["diagnostics"] = json!({"h_raw":h,"z_shared_f64":heads.encode(&h)?,"token_ids":ids,
                    "word_mask":words,"token_offsets":offsets,"raw":out.raw,"span_emission":emission});
            }
            supported += 1;
        }
        serde_json::to_writer(&mut f, &result)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    encoder::verify(&parent)?;
    for key in ["contract", "heads", "encoder"] {
        member(&export, &manifest, key)?;
    }
    if schema.span_tower() {
        member(&export, &manifest, "span_tower")?;
    }
    ensure!(
        encoder::sha(input)? == sha(&rb)
            && encoder::sha(&export.join("manifest.json"))? == binding["export_manifest_sha256"],
        "input changed during inference"
    );
    let mut report = json!({"schema":"slop_ninja_full_a_rust_run_v1","complete":true,"bindings":binding,
        "export_schema":manifest["schema"],"heads_schema":w["schema"],"requested":requests.len(),"supported":supported,"unsupported":requests.len()-supported,
        "token_head":heads.has_token_head(),"span_emission_unsupported":span_unsupported,
        "predictions_sha256":encoder::sha(&out.join("predictions.jsonl"))?,"deployment_qualified":false,
        "calibrated":manifest["calibrated"],"numeric_profile":profile.name(),
        "matmul_backend":if cfg!(feature="mkl"){"mkl"}else{"candle_default"},
        "timing_seconds":{"parent_and_roster_verification":verification_seconds,"encoder_construction":encoder_load_seconds,
            "tokenization_total":token_seconds,"encoder_forward_total":encoder_seconds,"full_A_heads_total":heads_seconds,
            "token_head_total":token_head_seconds,"span_decoder_total":decoder_seconds,
            "whole_run":started.elapsed().as_secs_f64()},
        "size_bytes":{"encoder_safetensors":fs::metadata(&ep)?.len(),"heads_json":hb.len(),
            "binary":fs::metadata(std::env::current_exe()?)?.len()},
        "runtime":"Rust Candle full fine-tuned V6-architecture encoder (numeric_profile), full-A f64 heads, span operator, spans-v1 token head and span emission"});
    if schema.span_tower() {
        report["span_tower"] = json!(true);
        report["timing_seconds"]["span_tower_total"] = json!(tower_seconds);
        report["size_bytes"]["tower_safetensors"] = json!(fs::metadata(tp.as_ref().context("tower file")?)?.len());
        report["runtime"] = json!("Rust Candle full fine-tuned V6-architecture encoder (numeric_profile), spans-S2 span tower (copied top layers and final norm) for the token head, full-A f64 heads, span operator, spans-v1 token head and span emission");
    }
    put(&out.join("receipt.json"), &report)?;
    println!(
        "{}",
        json!({"complete":true,"supported":supported,"unsupported":requests.len()-supported})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    let flags = a.get(4..).unwrap_or_default();
    ensure!(
        a.len() >= 4
            && flags.iter().collect::<BTreeSet<_>>().len() == flags.len()
            && flags
                .iter()
                .all(|f| f == "--uncalibrated-diagnostics" || f == "--f32-diagnostic"),
        "PARENT_ENCODER EXPORT_DIR REQUESTS NEW_OUT [--uncalibrated-diagnostics] [--f32-diagnostic]"
    );
    let has = |f: &str| flags.iter().any(|x| x == f);
    run(
        Path::new(&a[0]),
        Path::new(&a[1]),
        Path::new(&a[2]),
        Path::new(&a[3]),
        has("--uncalibrated-diagnostics"),
        if has("--f32-diagnostic") {
            full_encoder::Profile::F32
        } else {
            full_encoder::Profile::F64
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn calibration(schema: &str, map: &Value) -> Value {
        json!({"schema":schema,"complete":true,"fit_split":"cal",
            "final_test_opened":false,"arm":"E2","seed":17,"selected_checkpoint_sha256":"checkpoint",
            "contract_sha256":"contract","involvement":{"selected":map},
            "roles":{"human":{"qualified":true,"threshold":0.7},"ai":{"qualified":true,"threshold":0.8}}})
    }
    #[test]
    fn exported_map_and_thresholds_require_matching_calibration() {
        let m = json!({"arm":"E2","seed":17,"selected_checkpoint_sha256":"checkpoint"});
        let map = json!({"method":"involvement_temperature_v1","temperature":1.2});
        let w = json!({"involvement_calibration":map,"thresholds":{"human":0.7,"ai":0.8},"span_emission":null});
        let mut cal = calibration("slop_ninja_full_a_calibration_v3", &map);
        cal["spans"] = Value::Null;
        guard_calibration(Export::V3, &m, &w, &cal, "contract").unwrap();
        let mut wrong = cal.clone();
        wrong["involvement"]["selected"]["temperature"] = json!(1.);
        assert!(guard_calibration(Export::V3, &m, &w, &wrong, "contract").is_err());
        let mut wrong = cal.clone();
        wrong["roles"]["ai"]["threshold"] = json!(0.7);
        assert!(guard_calibration(Export::V3, &m, &w, &wrong, "contract").is_err());
        let mut wrong = cal.clone();
        wrong["arm"] = json!("E1");
        assert!(guard_calibration(Export::V3, &m, &w, &wrong, "contract").is_err());
        // The CAL span emission spec must be the exported one.
        let mut wrong = cal.clone();
        wrong["spans"] = json!({"rule":"slop_ninja_span_emission_v1"});
        assert!(guard_calibration(Export::V3, &m, &w, &wrong, "contract").is_err());
        // A v3 export never takes a v2 record.
        assert!(guard_calibration(Export::V3, &m, &w, &calibration("slop_ninja_full_a_calibration_v2", &map), "contract").is_err());
        // A v4 export takes the same v3 record and checks.
        guard_calibration(Export::V4, &m, &w, &cal, "contract").unwrap();
        assert!(guard_calibration(Export::V4, &m, &w, &calibration("slop_ninja_full_a_calibration_v2", &map), "contract").is_err());
    }
    #[test]
    fn v2_exports_take_v2_calibration_with_the_same_checks() {
        // R0's heads v1 has no span_emission key.
        let m = json!({"arm":"E1aux","seed":17,"selected_checkpoint_sha256":"checkpoint"});
        let map = json!({"method":"isotonic_bounded_v2","knots":[]});
        let w = json!({"involvement_calibration":map,"thresholds":{"human":1.0,"ai":0.8},
            "profiles":{"human":[],"ai":["a1","a2"]}});
        let mut cal = calibration("slop_ninja_full_a_calibration_v2", &map);
        cal["arm"] = json!("E1aux");
        cal["roles"]["human"] = json!({"roster_empty":true,"qualified":false});
        guard_calibration(Export::V2, &m, &w, &cal, "contract").unwrap();
        // A v3 record (or any span emission spec) is refused for a v2 export.
        let mut wrong = cal.clone();
        wrong["schema"] = json!("slop_ninja_full_a_calibration_v3");
        assert!(guard_calibration(Export::V2, &m, &w, &wrong, "contract").is_err());
        let mut wrong = cal.clone();
        wrong["spans"] = Value::Null;
        assert!(guard_calibration(Export::V2, &m, &w, &wrong, "contract").is_err());
        let mut wrong = cal.clone();
        wrong["selected_checkpoint_sha256"] = json!("other");
        assert!(guard_calibration(Export::V2, &m, &w, &wrong, "contract").is_err());
        let mut wrong = cal.clone();
        wrong["roles"]["ai"]["threshold"] = json!(0.9);
        assert!(guard_calibration(Export::V2, &m, &w, &wrong, "contract").is_err());
        let mut wrong = cal;
        wrong["contract_sha256"] = json!("other");
        assert!(guard_calibration(Export::V2, &m, &w, &wrong, "contract").is_err());
    }
    #[test]
    fn guard_pairs_each_export_schema_with_its_heads_schema() {
        let mean: Vec<f64> = (0..768).map(|i| i as f64 / 1024.).collect();
        let scale = vec![0.5f64; 768];
        let c = json!({"schema":"slop_ninja_full_a_contract_v2","input_mode":"text","raw_dimension":768,
            "zero_cfg_coordinates":1820,"maximum_tokens_including_specials":2048,"token_truncation":false,
            "parent_artifact_id":encoder::ID,"embedding_normalization":{"mean":mean,"scale":scale},
            "human_profiles":[],"ai_profiles":["a1","a2"]});
        let heads = |schema: &str| {
            json!({"schema":schema,"contract_sha256":"contract","base_temperature":encoder::TEMP,
                "supervision":{"origin":1,"share":1,"human":0,"ai":1},"text_mean":mean,"text_scale":scale,
                "profiles":{"human":[],"ai":["a1","a2"]}})
        };
        let manifest = |schema: &str| {
            json!({"schema":schema,"complete":true,"final_test_opened":false,"calibrated":true,
                "parent_artifact_id":encoder::ID,"parent_manifest_sha256":encoder::MANIFEST})
        };
        let (m2, m3) = (manifest("slop_ninja_full_a_export_v2"), manifest("slop_ninja_full_a_export_v3"));
        let (w1, w2) = (heads("slop_ninja_full_a_heads_v1"), heads("slop_ninja_full_a_heads_v2"));
        assert_eq!(guard(&m2, &c, &w1, "contract", false).unwrap(), Export::V2);
        assert_eq!(guard(&m3, &c, &w2, "contract", false).unwrap(), Export::V3);
        assert!(guard(&m2, &c, &w2, "contract", false).is_err());
        assert!(guard(&m3, &c, &w1, "contract", false).is_err());
        // A v2 manifest declares no token head or emission spec, not even null.
        for key in ["token_head", "span_emission"] {
            let mut m = m2.clone();
            m[key] = Value::Null;
            assert!(guard(&m, &c, &w1, "contract", false).is_err());
        }
        let mut m = m2.clone();
        m["complete"] = json!(false);
        assert!(guard(&m, &c, &w1, "contract", false).is_err());
        // v4 pairs with heads v3 and declares the heads' own span tower; only v4 carries the key.
        let tower = json!({"rule":"slop_ninja_s2_span_tower_v1","source_layer":18,"weights":"tower.safetensors"});
        let mut w3 = heads("slop_ninja_full_a_heads_v3");
        w3["span_tower"] = tower.clone();
        let mut m4 = manifest("slop_ninja_full_a_export_v4");
        m4["span_tower"] = json!({"file":"tower.safetensors","sha256":"t","declaration":tower});
        m4["token_head"] = json!({"input":"span_tower_final_norm_token_states"});
        assert_eq!(guard(&m4, &c, &w3, "contract", false).unwrap(), Export::V4);
        assert!(guard(&m4, &c, &w2, "contract", false).is_err());
        assert!(guard(&m3, &c, &w3, "contract", false).is_err());
        let mut m = m4.clone();
        m["span_tower"]["declaration"]["source_layer"] = json!(17);
        assert!(guard(&m, &c, &w3, "contract", false).is_err());
        // The manifest's tower file must be the declaration's weights file, even when hash-bound.
        let mut m = m4.clone();
        m["span_tower"]["file"] = json!("other.safetensors");
        assert!(guard(&m, &c, &w3, "contract", false).is_err());
        let mut m = m4.clone();
        m.as_object_mut().unwrap().remove("span_tower");
        assert!(guard(&m, &c, &w3, "contract", false).is_err());
        let mut m = m4;
        m["token_head"] = Value::Null;
        assert!(guard(&m, &c, &w3, "contract", false).is_err());
        let mut m = m3.clone();
        m["span_tower"] = Value::Null;
        assert!(guard(&m, &c, &w2, "contract", false).is_err());
        let mut w = w2.clone();
        w["span_tower"] = Value::Null;
        w["schema"] = json!("slop_ninja_full_a_heads_v2");
        assert!(guard(&m3, &c, &w, "contract", false).is_err());
    }
    #[test]
    fn export_schema_selects_the_heads_and_calibration_schemas() {
        let v2 = json!({"schema":"slop_ninja_full_a_export_v2"});
        let v3 = json!({"schema":"slop_ninja_full_a_export_v3"});
        let v4 = json!({"schema":"slop_ninja_full_a_export_v4"});
        assert_eq!(Export::of(&v2).unwrap(), Export::V2);
        assert_eq!(Export::of(&v3).unwrap(), Export::V3);
        assert_eq!(Export::of(&v4).unwrap(), Export::V4);
        assert!(Export::V4.span_tower() && !Export::V3.span_tower() && !Export::V2.span_tower());
        assert_eq!(Export::V4.heads_schema(), "slop_ninja_full_a_heads_v3");
        assert_eq!(Export::V4.calibration_schema(), "slop_ninja_full_a_calibration_v3");
        assert!(Export::of(&json!({"schema":"slop_ninja_full_a_export_v1"})).is_err());
        assert!(Export::of(&json!({})).is_err());
        assert_eq!(Export::V2.heads_schema(), "slop_ninja_full_a_heads_v1");
        assert_eq!(Export::V3.heads_schema(), "slop_ninja_full_a_heads_v2");
        assert_eq!(Export::V2.calibration_schema(), "slop_ninja_full_a_calibration_v2");
        assert_eq!(Export::V3.calibration_schema(), "slop_ninja_full_a_calibration_v3");
    }
    /// A scratch directory, canonical like the export root that `run` resolves, removed on drop.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let p = std::env::temp_dir().join(format!("full-a-{tag}-{}-{nanos}", std::process::id()));
            fs::create_dir(&p).unwrap();
            Scratch(p.canonicalize().unwrap())
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn members_must_match_their_manifest_digest_inside_the_export() {
        let (root, outside) = (Scratch::new("member"), Scratch::new("outside"));
        fs::write(root.0.join("heads.json"), b"{}").unwrap();
        fs::write(outside.0.join("heads.json"), b"{}").unwrap();
        let digest = sha(b"{}");
        let m = |file: &str, digest: &str| json!({"heads":{"file":file,"sha256":digest}});
        assert_eq!(member(&root.0, &m("heads.json", digest.as_str()), "heads").unwrap(), root.0.join("heads.json"));
        let err = member(&root.0, &m("heads.json", sha(b"[]").as_str()), "heads").unwrap_err();
        assert!(err.to_string().contains("export member changed: heads"), "{err}");
        // A changed file is refused against the unchanged manifest.
        fs::write(root.0.join("heads.json"), b"[]").unwrap();
        assert!(member(&root.0, &m("heads.json", digest.as_str()), "heads").is_err());
        // Only plain relative names resolve: no empty, current, parent or absolute path.
        let escape = format!("../{}/heads.json", outside.0.file_name().unwrap().to_str().unwrap());
        let absolute = outside.0.join("heads.json");
        for name in ["", ".", "./heads.json", escape.as_str(), absolute.to_str().unwrap()] {
            let err = member(&root.0, &m(name, digest.as_str()), "heads").unwrap_err();
            assert!(err.to_string().contains("unsafe export member"), "{name}: {err}");
        }
        assert!(member(&root.0, &json!({"heads":{"sha256":digest}}), "heads").is_err());
        assert!(member(&root.0, &m("missing.json", digest.as_str()), "heads").is_err());
        // A link inside the export that resolves outside it is refused, though its target has the digest.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.0.join("heads.json"), root.0.join("link.json")).unwrap();
            let err = member(&root.0, &m("link.json", digest.as_str()), "heads").unwrap_err();
            assert!(err.to_string().contains("member escaped export root"), "{err}");
        }
    }
    #[test]
    fn a_v4_run_reads_only_a_tower_file_that_matches_the_manifest() {
        let root = Scratch::new("tower");
        fs::write(root.0.join("tower.safetensors"), b"tower").unwrap();
        let m = json!({"span_tower":{"file":"tower.safetensors","sha256":sha(b"tower")}});
        assert_eq!(
            tower_file(&root.0, &m, Export::V4).unwrap(),
            Some(root.0.join("tower.safetensors"))
        );
        let mut changed = m.clone();
        changed["span_tower"]["sha256"] = json!(sha(b"other"));
        let err = tower_file(&root.0, &changed, Export::V4).unwrap_err();
        assert!(err.to_string().contains("export member changed: span_tower"), "{err}");
        assert!(tower_file(&root.0, &json!({}), Export::V4).is_err());
        // v2 and v3 exports never read a tower file (the guard refuses a manifest that declares one).
        for export in [Export::V2, Export::V3] {
            assert_eq!(tower_file(&root.0, &m, export).unwrap(), None);
            assert_eq!(tower_file(&root.0, &changed, export).unwrap(), None);
        }
    }
    #[test]
    fn every_run_binds_each_runtime_source() {
        let b = source_bindings();
        let expected = [
            ("calibration_runtime_sha256", sha(include_bytes!("../../../../grammar/crates/grammar-a-baseline/src/involvement_calibration.rs"))),
            ("cargo_lock_sha256", sha(include_bytes!("../../Cargo.lock"))),
            ("full_encoder_sha256", sha(include_bytes!("../full_encoder.rs"))),
            ("graph_sha256", sha(include_bytes!("../modernbert.rs"))),
            ("heads_runtime_sha256", sha(include_bytes!("../../../../grammar/crates/grammar-a-baseline/src/full_a.rs"))),
            ("implementation_sha256", sha(include_bytes!("full_a.rs"))),
            ("layernorm_sha256", sha(include_bytes!("../reference_layer_norm.rs"))),
            ("spans_runtime_sha256", sha(include_bytes!("../../../../grammar/crates/grammar-a-baseline/src/full_a_spans.rs"))),
            ("tower_runtime_sha256", sha(include_bytes!("../../../../grammar/crates/grammar-a-baseline/src/full_a_tower.rs"))),
        ];
        assert_eq!(b.as_object().unwrap().len(), expected.len());
        for (key, digest) in expected {
            assert_eq!(b[key], digest, "{key}");
        }
    }
}
