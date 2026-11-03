//! Qualified V6 wrapper, exposing the unchanged classifier-input h for A.
use crate::{modernbert, reference_layer_norm, trace};
use anyhow::{Context, Result, ensure};
use candle::{D, DType, Device, Tensor};
use candle_nn::{Module, VarBuilder};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};
use tokenizers::Tokenizer;
pub const ID: &str = "506b1c66a6a2ffc4eb4e50b9b057657ce0e5b6f04cff8caf3df496c956dc0d5f";
pub const MANIFEST: &str = "b68b6ba35892d5fd0cafcac49c4cde5d23c0f9a16aa8448e54f4b1684be3f70b";
pub const TEMP: f64 = 1.8776105792643432;

fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
pub fn sha(p: &Path) -> Result<String> {
    let mut f = fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut b = [0; 1024 * 1024];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(hex::encode(h.finalize()))
}
pub fn verify(root: &Path) -> Result<Value> {
    ensure!(
        sha(&root.join("manifest.json"))? == MANIFEST,
        "immutable qualified manifest changed"
    );
    let manifest = read(&root.join("manifest.json"))?;
    ensure!(
        manifest["artifact_id"] == ID && manifest["max_tokens"] == 2048,
        "artifact contract differs"
    );
    for (name, expected) in manifest["files"].as_object().context("artifact files")? {
        let p = root.join(name).canonicalize()?;
        ensure!(
            p.starts_with(root) && !Path::new(name).is_absolute(),
            "unsafe artifact path"
        );
        ensure!(
            sha(&p)? == expected.as_str().context("file digest")?,
            "immutable member changed: {name}"
        );
    }
    ensure!(
        read(&root.join("calibration.json"))?["temperature"] == TEMP,
        "base temperature changed"
    );
    Ok(manifest)
}

// Python str.strip also treats these four control separators as whitespace.
fn empty(text: &str) -> bool {
    text.chars()
        .all(|c| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}
pub fn token_ids(tokenizer: &Tokenizer, text: &str) -> Result<(Vec<u32>, &'static str)> {
    if empty(text) {
        return Ok((vec![], "empty_text"));
    }
    let encoded = tokenizer
        .encode(text, true)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let ids = encoded.get_ids().to_vec();
    let status = if ids.len() > 2048 {
        "unsupported_length"
    } else {
        "ok"
    };
    Ok((ids, status))
}

pub struct Encoder {
    model: modernbert::ModernBert,
    dense: candle_nn::Linear,
    norm: reference_layer_norm::LayerNorm,
    classifier: candle_nn::Linear,
}
impl Encoder {
    pub fn load(root: &Path) -> Result<Self> {
        Self::load_with_overlay(root, None)
    }
    // Caller must verify the immutable parent and bind the optional overlay's
    // exact bytes to its model export receipt before invoking this constructor.
    pub fn load_with_overlay(root: &Path, overlay: Option<&Path>) -> Result<Self> {
        let mut config = read(&root.join("classifier/config.json"))?;
        for field in ["attention_bias", "classifier_bias", "mlp_bias", "norm_bias"] {
            ensure!(
                config[field] == false,
                "unsupported bias configuration: {field}"
            );
        }
        for field in [
            "attention_dropout",
            "classifier_dropout",
            "embedding_dropout",
            "mlp_dropout",
        ] {
            ensure!(
                config[field] == 0.0,
                "unsupported dropout configuration: {field}"
            );
        }
        ensure!(
            config["hidden_activation"] == "gelu"
                && config["classifier_activation"] == "gelu"
                && config["classifier_pooling"] == "mean"
                && config["norm_eps"] == config["layer_norm_eps"],
            "unsupported head/norm operation"
        );
        // The upstream Rust config types label2id values as strings. Its backbone
        // does not use either map; discard classifier metadata in this local copy.
        for name in ["label2id", "id2label", "classifier_pooling"] {
            config.as_object_mut().unwrap().remove(name);
        }
        let config: modernbert::Config = serde_json::from_value(config)?;
        ensure!(
            config.hidden_size == 768
                && config.num_hidden_layers == 22
                && config.num_attention_heads == 12,
            "unexpected architecture"
        );
        // Read-only mappings, only after every immutable artifact member verifies.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[root.join("classifier/model.safetensors")],
                DType::F32,
                &Device::Cpu,
            )?
        };
        let vb = if let Some(path) = overlay {
            crate::last_block_overlay::builder(vb, path)?
        } else {
            vb
        };
        Ok(Self {
            model: modernbert::ModernBert::load(vb.clone(), &config)?,
            dense: candle_nn::linear_no_bias(768, 768, vb.pp("head.dense"))?,
            norm: reference_layer_norm::layer_norm_no_bias(
                768,
                config.layer_norm_eps,
                vb.pp("head.norm"),
            )?,
            classifier: candle_nn::linear(768, 3, vb.pp("classifier"))?,
        })
    }
    pub fn forward(&self, ids: &[u32]) -> Result<Encoding> {
        ensure!(
            !ids.is_empty() && ids.len() <= 2048,
            "unsupported input cannot reach encoder"
        );
        let input = Tensor::new(ids, &Device::Cpu)?.unsqueeze(0)?;
        let mask = Tensor::ones((1, ids.len()), DType::U32, &Device::Cpu)?;
        let hidden = self.model.forward(&input, &mask)?;
        let pooled = hidden
            .sum(1)?
            .broadcast_div(&mask.sum_keepdim(1)?.to_dtype(DType::F32)?)?;
        let dense = self.dense.forward(&pooled)?;
        trace::record("head.dense", &dense)?;
        let activation = dense.gelu_erf()?;
        trace::record("head.act", &activation)?;
        let h = self.norm.forward(&activation)?;
        trace::record("head.norm", &h)?;
        let logits = self.classifier.forward(&h)?;
        trace::record("classifier", &logits)?;
        let logits = logits.squeeze(0)?.to_vec1::<f32>()?;
        // Match the frozen runtime: FP32 logits, then float64 temperature/softmax.
        let p = candle_nn::ops::softmax(
            &(Tensor::new(logits.as_slice(), &Device::Cpu)?.to_dtype(DType::F64)? / TEMP)?,
            D::Minus1,
        )?
        .to_vec1::<f64>()?;
        ensure!(p.iter().all(|v| v.is_finite()), "nonfinite prediction");
        let h = h.squeeze(0)?.to_vec1::<f32>()?;
        ensure!(
            h.len() == 768 && h.iter().all(|v| v.is_finite()),
            "invalid classifier-input h"
        );
        Ok(Encoding {
            h,
            logits,
            probabilities: p,
        })
    }
}

pub struct Encoding {
    pub h: Vec<f32>,
    pub logits: Vec<f32>,
    pub probabilities: Vec<f64>,
}
