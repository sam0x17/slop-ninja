//! R0 encoder arm (B): merge verified runtime chunk outputs into compact
//! per-document `h_raw` and `z_shared` rows. Calibrated predictions and
//! detector outputs in the runtime files are ignored.
use crate::common::*;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::Path,
};

pub const DIMENSION: usize = 768;

#[derive(Deserialize)]
pub struct Prediction {
    pub id: String,
    pub text_sha256: String,
    pub status: String,
    #[serde(default)]
    pub reason: Option<Value>,
    pub token_count: u64,
    #[serde(default)]
    pub truncation: Option<bool>,
    #[serde(default)]
    pub diagnostics: Option<Diagnostics>,
}

#[derive(Deserialize)]
pub struct Diagnostics {
    #[serde(default)]
    pub h_raw: Option<Vec<f64>>,
    #[serde(default)]
    pub z_shared_f64: Option<Vec<f64>>,
}

pub fn vectors(p: &Prediction) -> Result<Option<(&[f64], &[f64])>> {
    if p.status != "ok" {
        return Ok(None);
    }
    let d = p.diagnostics.as_ref().context("diagnostics")?;
    let h = d.h_raw.as_deref().context("h_raw")?;
    let z = d.z_shared_f64.as_deref().context("z_shared_f64")?;
    ensure!(h.len() == DIMENSION && z.len() == DIMENSION, "embedding dimension");
    ensure!(h.iter().chain(z).all(|v| v.is_finite()), "nonfinite embedding");
    Ok(Some((h, z)))
}

pub fn collect_cli(frozen_path: &Path, chunks: &Path, runs: &Path, out: &Path) -> Result<()> {
    let frozen = read_frozen(frozen_path)?;
    let r0 = &frozen["pipelines"]["r0"];
    let want = |k: &str| r0[k].as_str().with_context(|| format!("r0 {k}"));
    let mut names: Vec<String> = fs::read_dir(chunks)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".jsonl"))
        .collect();
    names.sort();
    ensure!(!names.is_empty(), "no chunks");
    create_new_dir(out)?;
    let mut writer = JsonlWriter::create(&out.join("encoder-features.jsonl"))?;
    let mut chunk_records = Vec::new();
    let mut timing: BTreeMap<String, f64> = BTreeMap::new();
    let (mut requested, mut supported, mut tokens, mut truncated) = (0u64, 0u64, 0u64, 0u64);
    let mut reasons: BTreeMap<String, u64> = BTreeMap::new();
    let mut binding_sets = std::collections::BTreeSet::new();
    let mut max_tokens = 0u64;
    for name in &names {
        let chunk_path = chunks.join(name);
        let stem = name.trim_end_matches(".jsonl");
        let dir = runs.join(stem);
        let chunk_sha = hash_file(&chunk_path)?;
        let requests = read_requests(&chunk_path)?;
        let texts: HashMap<&str, String> = requests
            .iter()
            .map(|r| (r.id.as_str(), sha_hex(r.text.as_bytes())))
            .collect();
        let receipt: Value = serde_json::from_slice(&fs::read(dir.join("receipt.json"))?)?;
        ensure!(receipt["complete"] == true, "{stem}: incomplete run");
        let b = &receipt["bindings"];
        ensure!(b["export_manifest_sha256"] == want("export_manifest_sha256")?, "{stem}: export");
        ensure!(b["encoder_sha256"] == want("encoder_safetensors_sha256")?, "{stem}: encoder");
        ensure!(b["heads_sha256"] == want("heads_sha256")?, "{stem}: heads");
        ensure!(b["contract_sha256"] == want("contract_sha256")?, "{stem}: contract");
        ensure!(b["parent_manifest_sha256"] == want("parent_manifest_sha256")?, "{stem}: parent");
        ensure!(b["requests_sha256"] == chunk_sha.as_str(), "{stem}: request binding");
        ensure!(receipt["numeric_profile"] == want("numeric_profile")?, "{stem}: numeric profile");
        let mut identity = b.clone();
        identity.as_object_mut().unwrap().remove("requests_sha256");
        binding_sets.insert(serde_json::to_string(&identity)?);
        let pred_bytes = fs::read(dir.join("predictions.jsonl"))?;
        ensure!(sha_hex(&pred_bytes) == receipt["predictions_sha256"], "{stem}: predictions digest");
        let mut n = 0u64;
        for line in pred_bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            let p: Prediction = serde_json::from_slice(line)?;
            ensure!(texts.get(p.id.as_str()) == Some(&p.text_sha256), "{stem}: text binding");
            requested += 1;
            n += 1;
            tokens += p.token_count;
            max_tokens = max_tokens.max(p.token_count);
            truncated += p.truncation.unwrap_or(false) as u64;
            match vectors(&p)? {
                Some((h, z)) => {
                    supported += 1;
                    writer.write(&json!({"id":p.id,"status":"ok","token_count":p.token_count,
                        "text_sha256":p.text_sha256,"h_raw":h,"z_shared":z}))?;
                }
                None => {
                    let r = p.reason.as_ref().and_then(Value::as_str).unwrap_or("unknown").to_string();
                    *reasons.entry(r.clone()).or_default() += 1;
                    writer.write(&json!({"id":p.id,"status":p.status,"reason":r,"token_count":p.token_count,
                        "text_sha256":p.text_sha256}))?;
                }
            }
        }
        ensure!(n as usize == requests.len(), "{stem}: prediction count");
        if let Some(t) = receipt["timing_seconds"].as_object() {
            for (k, v) in t {
                *timing.entry(k.clone()).or_default() += v.as_f64().unwrap_or(0.0);
            }
        }
        chunk_records.push(json!({"chunk":stem,"requests_sha256":chunk_sha,"documents":n,
            "receipt_sha256":hash_file(&dir.join("receipt.json"))?}));
    }
    ensure!(binding_sets.len() == 1, "mixed runtime identities across chunks");
    let (rows, sha) = writer.finish()?;
    let manifest = json!({
        "schema":"slop_ninja_cfg_vs_encoder_feature_manifest_v1","arm":"B","protocol_id":PROTOCOL_ID,
        "frozen_sha256":FROZEN_SHA256,"created":now_utc(),
        "runtime_identity":{"binary_sha256":want("runtime_binary_sha256")?,"runtime_commit":want("runtime_commit")?,
            "numeric_profile":want("numeric_profile")?,"flags":r0["flags"],"device":"cpu, one thread per shard",
            "bindings":serde_json::from_str::<Value>(binding_sets.iter().next().unwrap())?},
        "inputs":{"chunks":chunk_records},
        "outputs":{"features":{"file":"encoder-features.jsonl","rows":rows,"sha256":sha,
            "layout":"per document: h_raw (768, diagnostics.h_raw) and z_shared (768, diagnostics.z_shared_f64)"}},
        "coverage":{"documents":requested,"supported":supported,"unsupported":requested - supported,
            "unsupported_reasons":reasons,"tokens":tokens,"max_tokens":max_tokens,"truncated":truncated},
        "runtime":{"timing_seconds_total":timing},
        "metrics_computed":false
    });
    write_new_json(&out.join("manifest.json"), &manifest)?;
    println!("{}", serde_json::to_string(&json!({"coverage":manifest["coverage"],"runtime":manifest["runtime"]}))?);
    Ok(())
}
