//! Fully fine-tuned V6-architecture encoder for full A (E1/E2).
//!
//! The immutable parent supplies the verified config and tokenizer. The export
//! supplies every encoder tensor under the parent's exact roster (names, shapes,
//! FP32), and the original V6 classifier tensors must be byte-identical: they
//! are never used for A outputs. Returns the classifier input h and the
//! final-norm token states that feed the span operator. Same graph as V6.
//!
//! Span tower (export v4, protocol spans-S2 arm S2c): `tower.safetensors`
//! holds copies of the top encoder layers and of the final norm, with the
//! declared roster (every tensor FP32 with its source tensor's shape). The
//! encoder runs unchanged; the tower runs from the hidden state entering its
//! first layer with the encoder's masks and positions, and its normed states
//! are returned beside the encoder's (they feed only the token head).
//!
//! Numeric profiles: `F64` (qualified for full A) loads the FP32 weights
//! exactly into FP64 and runs every operation in FP64 except the rotary
//! tables, which are HF's float32 angle and cos/sin tables widened exactly
//! (the trained function); `F32` is the V6 FP32 graph, kept for diagnostics. Fine-tuned E2 weights amplify FP32 rounding
//! (massive residual activations), so FP32 CPU replay of a CUDA reference can
//! exceed the 1e-5 output gate without any operator difference.
use crate::{modernbert, reference_layer_norm, trace};
use anyhow::{Context, Result, bail, ensure};
use candle::{DType, Device, Tensor, safetensors::MmapedSafetensors};
use candle_nn::{Module, VarBuilder};
use grammar_a_baseline::full_a_tower::TowerSpec;
use serde_json::Value;
use std::{fs, path::Path, time::Instant};

pub const CLS: u32 = 50281;
pub const SEP: u32 = 50282;

/// Exact roster equality with the parent, finite FP32 values and an unchanged classifier.
pub fn validate_roster(parent: &Path, export: &Path) -> Result<usize> {
    let p = unsafe { MmapedSafetensors::new(parent)? };
    let e = unsafe { MmapedSafetensors::new(export)? };
    let mut pn = p.tensors().into_iter().map(|(n, _)| n).collect::<Vec<_>>();
    let mut en = e.tensors().into_iter().map(|(n, _)| n).collect::<Vec<_>>();
    pn.sort();
    en.sort();
    ensure!(
        pn == en,
        "full encoder tensor roster differs from the parent"
    );
    for name in &pn {
        let a = p.load(name, &Device::Cpu)?;
        let b = e.load(name, &Device::Cpu)?;
        ensure!(
            b.dtype() == DType::F32 && a.dims() == b.dims(),
            "full encoder tensor shape/dtype changed: {name}"
        );
        let values = b.flatten_all()?.to_vec1::<f32>()?;
        ensure!(
            values.iter().all(|v| v.is_finite()),
            "nonfinite tensor {name}"
        );
        if name.starts_with("classifier.") {
            ensure!(
                a.flatten_all()?.to_vec1::<f32>()? == values,
                "the V6 classifier must remain unchanged"
            );
        }
    }
    Ok(pn.len())
}

/// The span tower file's roster: exactly the declared tensors, each finite FP32 with the
/// shape of the parent tensor it copies. Returns the tensor count.
pub fn validate_tower_roster(parent: &Path, tower: &Path, spec: &TowerSpec) -> Result<usize> {
    let p = unsafe { MmapedSafetensors::new(parent)? };
    let t = unsafe { MmapedSafetensors::new(tower)? };
    let mut names = t.tensors().into_iter().map(|(n, _)| n).collect::<Vec<_>>();
    names.sort();
    ensure!(
        names == spec.tensor_names(),
        "span tower tensor roster differs from its declaration"
    );
    for name in &names {
        let a = p.load(&spec.source_tensor(name)?, &Device::Cpu)?;
        let b = t.load(name, &Device::Cpu)?;
        ensure!(
            b.dtype() == DType::F32 && a.dims() == b.dims(),
            "span tower tensor shape/dtype differs from its source: {name}"
        );
        ensure!(
            b.flatten_all()?.to_vec1::<f32>()?.iter().all(|v| v.is_finite()),
            "nonfinite tensor {name}"
        );
    }
    Ok(names.len())
}

pub struct FullEncoder {
    model: modernbert::ModernBert,
    dense: candle_nn::Linear,
    norm: reference_layer_norm::LayerNorm,
    dtype: DType,
    tower: Option<modernbert::SpanTower>,
}

/// h and token states, widened exactly to f64 under the F32 profile; with a
/// span tower also its normed token states and the seconds the tower took.
pub struct FullEncoding {
    pub h: Vec<f64>,
    pub tokens: Vec<f64>,
    pub tower_tokens: Option<Vec<f64>>,
    pub tower_seconds: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Profile {
    F64,
    F32,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Profile::F64 => "candle_f64_full_a_v2",
            Profile::F32 => "candle_f32_v6_graph_diagnostic",
        }
    }
}

impl FullEncoder {
    /// Caller verifies the parent and binds `weights` to the export receipt first.
    pub fn load(root: &Path, weights: &Path, profile: Profile) -> Result<Self> {
        Self::load_with_tower(root, weights, None, profile)
    }

    /// As `load`, plus the declared span tower (caller binds the tower file and validates its roster).
    pub fn load_with_tower(
        root: &Path,
        weights: &Path,
        tower: Option<(&Path, &TowerSpec)>,
        profile: Profile,
    ) -> Result<Self> {
        let dtype = match profile {
            Profile::F64 => DType::F64,
            Profile::F32 => DType::F32,
        };
        let mut config: Value =
            serde_json::from_slice(&fs::read(root.join("classifier/config.json"))?)?;
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
                && config["norm_eps"] == config["layer_norm_eps"]
                && config["cls_token_id"] == CLS
                && config["sep_token_id"] == SEP,
            "unsupported head/norm operation"
        );
        for name in ["label2id", "id2label", "classifier_pooling"] {
            config
                .as_object_mut()
                .context("config object")?
                .remove(name);
        }
        let config: modernbert::Config = serde_json::from_value(config)?;
        ensure!(
            config.hidden_size == 768
                && config.num_hidden_layers == 22
                && config.num_attention_heads == 12,
            "unexpected architecture"
        );
        let vb =
            unsafe { VarBuilder::from_mmaped_safetensors(&[weights], dtype, &Device::Cpu)? };
        let tower = match tower {
            None => None,
            Some((path, spec)) => {
                ensure!(
                    spec.encoder_layers == config.num_hidden_layers
                        && spec.global_attn_every_n_layers == config.global_attn_every_n_layers
                        && spec.local_attention == config.local_attention,
                    "span tower declaration differs from the parent encoder"
                );
                let tvb =
                    unsafe { VarBuilder::from_mmaped_safetensors(&[path], dtype, &Device::Cpu)? };
                let t = modernbert::SpanTower::load(tvb.pp("span_tower"), &config, spec.source_layer)?;
                ensure!(
                    t.local_attention_layers() == spec.local_attention_layers(),
                    "span tower attention kinds differ from the declaration"
                );
                Some(t)
            }
        };
        Ok(Self {
            model: modernbert::ModernBert::load(vb.clone(), &config)?,
            dense: candle_nn::linear_no_bias(768, 768, vb.pp("head.dense"))?,
            norm: reference_layer_norm::layer_norm_no_bias(
                768,
                config.layer_norm_eps,
                vb.pp("head.norm"),
            )?,
            dtype,
            tower,
        })
    }

    pub fn has_tower(&self) -> bool {
        self.tower.is_some()
    }

    pub fn forward(&self, ids: &[u32]) -> Result<FullEncoding> {
        ensure!(
            ids.len() >= 3 && ids.len() <= 2048 && ids[0] == CLS && ids[ids.len() - 1] == SEP,
            "unsupported input cannot reach encoder"
        );
        let input = Tensor::new(ids, &Device::Cpu)?.unsqueeze(0)?;
        let mask = Tensor::ones((1, ids.len()), DType::U32, &Device::Cpu)?;
        let (hidden, branch) = self.model.forward_with_branch(
            &input,
            &mask,
            self.tower.as_ref().map(|t| t.first_layer()),
        )?;
        let pooled = hidden
            .sum(1)?
            .broadcast_div(&mask.sum_keepdim(1)?.to_dtype(self.dtype)?)?;
        let dense = self.dense.forward(&pooled)?;
        trace::record("head.dense", &dense)?;
        let activation = dense.gelu_erf()?;
        trace::record("head.act", &activation)?;
        let h = self.norm.forward(&activation)?;
        trace::record("head.norm", &h)?;
        let h = h.squeeze(0)?.to_dtype(DType::F64)?.to_vec1::<f64>()?;
        let tokens = hidden
            .squeeze(0)?
            .flatten_all()?
            .to_dtype(DType::F64)?
            .to_vec1::<f64>()?;
        ensure!(
            h.len() == 768
                && tokens.len() == ids.len() * 768
                && h.iter().chain(&tokens).all(|v| v.is_finite()),
            "invalid encoder outputs"
        );
        let (tower_tokens, tower_seconds) = match (&self.tower, branch) {
            (None, None) => (None, 0.),
            (Some(t), Some(b)) => {
                let begin = Instant::now();
                let states = t
                    .forward(&b)?
                    .squeeze(0)?
                    .flatten_all()?
                    .to_dtype(DType::F64)?
                    .to_vec1::<f64>()?;
                let seconds = begin.elapsed().as_secs_f64();
                ensure!(
                    states.len() == ids.len() * 768 && states.iter().all(|v| v.is_finite()),
                    "invalid span tower outputs"
                );
                (Some(states), seconds)
            }
            _ => bail!("span tower branch input missing"),
        };
        Ok(FullEncoding {
            h,
            tokens,
            tower_tokens,
            tower_seconds,
        })
    }
}

/// Span-operator content positions: every position except the CLS/SEP frame.
pub fn content_mask(ids: &[u32]) -> Vec<bool> {
    (0..ids.len())
        .map(|i| i != 0 && i + 1 != ids.len())
        .collect()
}

/// Python `str.isspace`: Unicode White_Space plus U+001C..U+001F.
pub fn is_space(c: char) -> bool {
    grammar_a_baseline::full_a_spans::is_space(c)
}

/// `slop_ninja_word_token_mask_v1`: content positions whose character offsets
/// (code points of the exact text) cover at least one non-whitespace character.
pub fn word_mask(text: &str, ids: &[u32], offsets: &[(usize, usize)]) -> Result<Vec<bool>> {
    ensure!(offsets.len() == ids.len(), "token/offset length mismatch");
    let chars = text.chars().collect::<Vec<_>>();
    let content = content_mask(ids);
    let mut out = Vec::with_capacity(ids.len());
    for (keep, &(a, b)) in content.iter().zip(offsets) {
        ensure!(a <= b && b <= chars.len(), "token offset out of range");
        out.push(*keep && chars[a..b].iter().any(|c| !is_space(*c)));
    }
    ensure!(out.iter().any(|v| *v), "no word-bearing content tokens");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    fn write(path: &Path, tensors: &[(&str, Tensor)]) {
        let map = tensors
            .iter()
            .map(|(n, t)| (n.to_string(), t.clone()))
            .collect::<HashMap<_, _>>();
        candle::safetensors::save(&map, path).unwrap();
    }
    #[test]
    fn roster_must_match_and_classifier_must_be_unchanged() {
        let dir = std::env::temp_dir().join(format!("full-encoder-roster-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let z = |s: &[usize]| Tensor::zeros(s, DType::F32, &Device::Cpu).unwrap();
        let o = |s: &[usize]| Tensor::ones(s, DType::F32, &Device::Cpu).unwrap();
        let parent = dir.join("parent.safetensors");
        write(
            &parent,
            &[("model.x", z(&[2, 2])), ("classifier.weight", o(&[3, 2]))],
        );
        let good = dir.join("good.safetensors");
        write(
            &good,
            &[("model.x", o(&[2, 2])), ("classifier.weight", o(&[3, 2]))],
        );
        assert_eq!(validate_roster(&parent, &good).unwrap(), 2);
        let changed = dir.join("changed.safetensors");
        write(
            &changed,
            &[("model.x", o(&[2, 2])), ("classifier.weight", z(&[3, 2]))],
        );
        assert!(validate_roster(&parent, &changed).is_err());
        let extra = dir.join("extra.safetensors");
        write(
            &extra,
            &[
                ("model.x", o(&[2, 2])),
                ("classifier.weight", o(&[3, 2])),
                ("model.y", o(&[1])),
            ],
        );
        assert!(validate_roster(&parent, &extra).is_err());
        let shape = dir.join("shape.safetensors");
        write(
            &shape,
            &[("model.x", o(&[4])), ("classifier.weight", o(&[3, 2]))],
        );
        assert!(validate_roster(&parent, &shape).is_err());
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            content_mask(&[CLS, 7, 8, SEP]),
            vec![false, true, true, false]
        );
    }
    #[test]
    fn whitespace_matches_python_isspace_and_word_mask_skips_space_tokens() {
        // Every code point for which CPython 3.12 str.isspace() is true.
        let python = [
            0x9, 0xa, 0xb, 0xc, 0xd, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x85, 0xa0, 0x1680, 0x2000,
            0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a,
            0x2028, 0x2029, 0x202f, 0x205f, 0x3000,
        ];
        let rust = (0..=0x10ffffu32)
            .filter_map(char::from_u32)
            .filter(|c| is_space(*c))
            .map(|c| c as u32)
            .collect::<Vec<_>>();
        assert_eq!(rust, python);
        let text = "Hi\n\n é\u{3000}x";
        // CLS, "Hi", "\n\n", " é", "\u{3000}", "x", SEP (code-point offsets).
        let ids = [CLS, 1, 2, 3, 4, 5, SEP];
        let offsets = [(0, 0), (0, 2), (2, 4), (4, 6), (6, 7), (7, 8), (0, 0)];
        assert_eq!(
            word_mask(text, &ids, &offsets).unwrap(),
            vec![false, true, false, true, false, true, false]
        );
        assert!(word_mask("\n", &[CLS, 1, SEP], &[(0, 0), (0, 1), (0, 0)]).is_err());
        assert!(word_mask(text, &ids, &offsets[..3]).is_err());
    }
}

#[cfg(test)]
mod tower_tests {
    //! The span tower branch against the Python golden vectors (synthetic weights and ids only): the trainer's
    //! TowerFullA on a tiny 22-layer ModernBERT, evaluated by the export's FP64 reference function.
    use super::*;
    use crate::modernbert::{Config, ModernBert, SpanTower};
    use grammar_a_baseline::full_a_spans::TokenHead;
    use grammar_a_baseline::full_a_tower::{TOWER_LAYER_TENSORS, token_head_from_json};
    use serde_json::json;
    use std::collections::HashMap;

    const GOLDEN: &str =
        include_str!("../../../grammar/crates/grammar-a-baseline/tests/golden/span-tower-golden-v1.json");

    /// `slop_ninja_tower_golden_weights_v1`, as span_tower_golden.py (exact in float32).
    fn golden_values(name: &str, count: usize) -> Vec<f64> {
        let mut state = name
            .bytes()
            .fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
        (0..count)
            .map(|_| {
                state = state.wrapping_add(0x9e3779b97f4a7c15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
                z ^= z >> 31;
                let u = ((z >> 48) as f64 - 32768.) / 32768.;
                if name.ends_with("norm.weight") {
                    1. + u / 16.
                } else if name.ends_with("tok_embeddings.weight") {
                    u
                } else {
                    u / 8.
                }
            })
            .collect()
    }

    fn golden() -> Value {
        serde_json::from_str(GOLDEN).unwrap()
    }

    fn config(g: &Value) -> Config {
        let c = &g["config"];
        let u = |k: &str| c[k].as_u64().unwrap() as usize;
        let f = |k: &str| c[k].as_f64().unwrap();
        Config {
            vocab_size: u("vocab_size"),
            hidden_size: u("hidden_size"),
            num_hidden_layers: u("num_hidden_layers"),
            num_attention_heads: u("num_attention_heads"),
            intermediate_size: u("intermediate_size"),
            max_position_embeddings: u("max_position_embeddings"),
            layer_norm_eps: f("layer_norm_eps"),
            pad_token_id: u("pad_token_id") as u32,
            global_attn_every_n_layers: u("global_attn_every_n_layers"),
            global_rope_theta: f("global_rope_theta"),
            local_attention: u("local_attention"),
            local_rope_theta: f("local_rope_theta"),
            classifier_config: None,
        }
    }

    fn layer_shapes(c: &Config) -> Vec<(&'static str, Vec<usize>)> {
        let (h, i) = (c.hidden_size, c.intermediate_size);
        vec![
            ("attn_norm.weight", vec![h]),
            ("attn.Wqkv.weight", vec![3 * h, h]),
            ("attn.Wo.weight", vec![h, h]),
            ("mlp_norm.weight", vec![h]),
            ("mlp.Wi.weight", vec![2 * i, h]),
            ("mlp.Wo.weight", vec![h, i]),
        ]
    }

    /// Every encoder and tower tensor, FP32 as in the export files. With `copies`, the tower tensors are the
    /// encoder's own top layers and final norm (the tower's initial state).
    fn tensors(c: &Config, first: usize, copies: bool) -> HashMap<String, Tensor> {
        let mut names = vec![
            ("model.embeddings.tok_embeddings.weight".to_string(), vec![c.vocab_size, c.hidden_size]),
            ("model.embeddings.norm.weight".to_string(), vec![c.hidden_size]),
            ("model.final_norm.weight".to_string(), vec![c.hidden_size]),
        ];
        for l in 0..c.num_hidden_layers {
            for (t, shape) in layer_shapes(c) {
                if l > 0 || t != "attn_norm.weight" {
                    names.push((format!("model.layers.{l}.{t}"), shape));
                }
            }
        }
        let tower_names = (first..c.num_hidden_layers)
            .flat_map(|l| layer_shapes(c).into_iter().map(move |(t, s)| (l, t, s)))
            .map(|(l, t, s)| (format!("span_tower.layers.{}.{t}", l - first), format!("model.layers.{l}.{t}"), s))
            .chain([("span_tower.norm.weight".to_string(), "model.final_norm.weight".to_string(), vec![c.hidden_size])]);
        let mut out = HashMap::new();
        for (name, shape) in &names {
            out.insert(name.clone(), tensor(name, shape));
        }
        for (name, source, shape) in tower_names {
            let from = if copies { source } else { name.clone() };
            out.insert(name, tensor(&from, &shape));
        }
        out
    }

    fn tensor(name: &str, shape: &[usize]) -> Tensor {
        let v = golden_values(name, shape.iter().product())
            .into_iter()
            .map(|x| {
                assert_eq!((x as f32) as f64, x, "golden weights are exact in float32");
                x as f32
            })
            .collect::<Vec<_>>();
        Tensor::from_vec(v, shape, &Device::Cpu).unwrap()
    }

    fn token_head(g: &Value) -> TokenHead {
        let (h, w) = (
            g["config"]["hidden_size"].as_u64().unwrap() as usize,
            g["token_head_width"].as_u64().unwrap() as usize,
        );
        let mut weights = serde_json::Map::new();
        for (k, shape) in [
            ("first.weight", vec![w, h]),
            ("first.bias", vec![w]),
            ("second.weight", vec![1, w]),
            ("second.bias", vec![1]),
        ] {
            let name = format!("token_head.{k}");
            let values = golden_values(&name, shape.iter().product());
            weights.insert(name, json!({"shape":shape,"values":values}));
        }
        token_head_from_json(&Value::Object(weights), w, h).unwrap()
    }

    fn flat(t: &Tensor) -> Vec<f64> {
        t.squeeze(0).unwrap().flatten_all().unwrap().to_dtype(DType::F64).unwrap().to_vec1().unwrap()
    }

    fn floats(v: &Value) -> Vec<f64> {
        v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect()
    }

    fn max_err(a: &[f64], b: &[f64]) -> f64 {
        assert_eq!(a.len(), b.len());
        a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0., f64::max)
    }

    fn models(g: &Value, dtype: DType, copies: bool) -> (Config, ModernBert, SpanTower) {
        let c = config(g);
        let first = g["tower"]["source_layer"].as_u64().unwrap() as usize;
        let vb = VarBuilder::from_tensors(tensors(&c, first, copies), dtype, &Device::Cpu);
        let encoder = ModernBert::load(vb.clone(), &c).unwrap();
        let tower = SpanTower::load(vb.pp("span_tower"), &c, first).unwrap();
        (c, encoder, tower)
    }

    fn ids(case: &Value) -> (Tensor, Tensor) {
        let ids = case["input_ids"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u32).collect::<Vec<_>>();
        let n = ids.len();
        (
            Tensor::new(ids.as_slice(), &Device::Cpu).unwrap().unsqueeze(0).unwrap(),
            Tensor::ones((1, n), DType::U32, &Device::Cpu).unwrap(),
        )
    }

    /// FP64 profile: final-norm states, tower states and token logits equal the Python FP64 reference.
    /// The one-token case uses position 0 only, where both runtimes' rotary tables are exact (cos 1, sin
    /// 0): it agrees to 1e-12. Longer cases also carry each runtime's float32 cos/sin rounding (torch's
    /// vectorized float32 cos/sin and glibc's cosf/sinf, which Rust calls, differ by one ulp in some table
    /// entries; measured 2026-11-01), so they are held to 1e-6. A wrong layer, mask, position, rotary base or
    /// weight mapping moves these values by 1e-3 or more.
    #[test]
    fn tower_branch_matches_the_python_golden_vectors() {
        let g = golden();
        assert_eq!(g["schema"], "slop_ninja_span_tower_golden_v1");
        let spec = TowerSpec::from_json(&g["tower"]).unwrap();
        let (c, encoder, tower) = models(&g, DType::F64, false);
        assert_eq!(tower.first_layer(), spec.source_layer);
        assert_eq!(tower.local_attention_layers(), spec.local_attention_layers());
        let head = token_head(&g);
        let (mut fin, mut tow, mut logit) = (0f64, 0f64, 0f64);
        let mut exact = None;
        for case in g["cases"].as_array().unwrap() {
            let (ids, mask) = ids(case);
            let (states, branch) = encoder.forward_with_branch(&ids, &mask, Some(spec.source_layer)).unwrap();
            let t = flat(&tower.forward(&branch.unwrap()).unwrap());
            let words = vec![true; t.len() / c.hidden_size];
            let got = head.logits(&t, c.hidden_size, &words).unwrap().into_iter().map(Option::unwrap).collect::<Vec<_>>();
            let errors = (
                max_err(&flat(&states), &floats(&case["final_norm_states"])),
                max_err(&t, &floats(&case["span_tower_states"])),
                max_err(&got, &floats(&case["token_logits"])),
            );
            if case["id"] == "one-token" {
                exact = Some(errors);
                continue;
            }
            fin = fin.max(errors.0);
            tow = tow.max(errors.1);
            logit = logit.max(errors.2);
        }
        let exact = exact.expect("one-token case");
        println!("golden max abs errors, one token: {exact:?}; longer cases: final_norm {fin:e} span_tower {tow:e} token_logit {logit:e}");
        assert!(exact.0 <= 1e-12 && exact.1 <= 1e-12 && exact.2 <= 1e-12, "{exact:?}");
        assert!(fin <= 1e-6 && tow <= 1e-6 && logit <= 1e-6, "{fin} {tow} {logit}");
    }

    /// FP32 diagnostic graph: same branch, FP32 rounding only.
    #[test]
    fn tower_branch_f32_graph_stays_near_the_golden_vectors() {
        let g = golden();
        let (_, encoder, tower) = models(&g, DType::F32, false);
        let mut worst = 0f64;
        for case in g["cases"].as_array().unwrap() {
            let (ids, mask) = ids(case);
            let (_, branch) = encoder.forward_with_branch(&ids, &mask, Some(18)).unwrap();
            let t = flat(&tower.forward(&branch.unwrap()).unwrap());
            worst = worst.max(max_err(&t, &floats(&case["span_tower_states"])));
        }
        assert!(worst > 0. && worst < 1e-3, "{worst}");
    }

    /// With the encoder's own top layers and final norm as tower weights (the tower's initial state), the
    /// tower states equal the final-norm states bit for bit; the encoder path is the same with or without
    /// the branch.
    #[test]
    fn tower_of_copies_reproduces_the_encoder_and_the_branch_leaves_it_unchanged() {
        let g = golden();
        for dtype in [DType::F64, DType::F32] {
            let (_, encoder, tower) = models(&g, dtype, true);
            for case in g["cases"].as_array().unwrap() {
                let (ids, mask) = ids(case);
                let plain = flat(&encoder.forward(&ids, &mask).unwrap());
                let (states, branch) = encoder.forward_with_branch(&ids, &mask, Some(18)).unwrap();
                let (none, nothing) = encoder.forward_with_branch(&ids, &mask, None).unwrap();
                assert!(nothing.is_none());
                assert_eq!(plain, flat(&states));
                assert_eq!(plain, flat(&none));
                assert_eq!(plain, flat(&tower.forward(&branch.unwrap()).unwrap()));
            }
        }
    }

    #[test]
    fn tower_refuses_other_capture_points_and_missing_tensors() {
        let g = golden();
        let (c, encoder, tower) = models(&g, DType::F64, false);
        let (ids, mask) = ids(&g["cases"][0]);
        for layer in [0, 22, 30] {
            assert!(encoder.forward_with_branch(&ids, &mask, Some(layer)).is_err(), "{layer}");
        }
        let (_, other) = encoder.forward_with_branch(&ids, &mask, Some(17)).unwrap();
        assert!(tower.forward(&other.unwrap()).is_err());
        for first in [0, 22] {
            let vb = VarBuilder::from_tensors(tensors(&c, 18, false), DType::F64, &Device::Cpu);
            assert!(SpanTower::load(vb.pp("span_tower"), &c, first).is_err());
        }
        // A missing attention norm is refused (ModernBertLayer::load treats it as optional).
        let mut t = tensors(&c, 18, false);
        t.remove("span_tower.layers.0.attn_norm.weight");
        let vb = VarBuilder::from_tensors(t, DType::F64, &Device::Cpu);
        assert!(SpanTower::load(vb.pp("span_tower"), &c, 18).is_err());
    }

    fn write(path: &Path, tensors: &[(String, Tensor)]) {
        let map = tensors.iter().cloned().collect::<HashMap<_, _>>();
        candle::safetensors::save(&map, path).unwrap();
    }

    #[test]
    fn tower_roster_is_exact_fp32_with_source_shapes() {
        let g = golden();
        let spec = TowerSpec::from_json(&g["tower"]).unwrap();
        let dir = std::env::temp_dir().join(format!("full-encoder-tower-roster-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let shape = |name: &str| if name.ends_with("norm.weight") { vec![3] } else { vec![2, 3] };
        let z = |s: &[usize], d: DType| Tensor::zeros(s, d, &Device::Cpu).unwrap();
        let names = spec.tensor_names();
        assert_eq!(names.len(), 4 * TOWER_LAYER_TENSORS.len() + 1);
        let parent = dir.join("parent.safetensors");
        write(
            &parent,
            &names
                .iter()
                .map(|n| { let s = spec.source_tensor(n).unwrap(); (s.clone(), z(&shape(&s), DType::F32)) })
                .collect::<Vec<_>>(),
        );
        let good = names.iter().map(|n| (n.clone(), z(&shape(n), DType::F32))).collect::<Vec<_>>();
        let path = dir.join("tower.safetensors");
        write(&path, &good);
        assert_eq!(validate_tower_roster(&parent, &path, &spec).unwrap(), names.len());
        let bad: [Box<dyn Fn(&mut Vec<(String, Tensor)>)>; 4] = [
            Box::new(|v| { v.pop(); }),
            Box::new(|v| v.push(("span_tower.extra.weight".into(), z(&[3], DType::F32)))),
            Box::new(|v| v[0].1 = z(&[3, 3], DType::F32)),
            Box::new(|v| v[0].1 = z(&[2, 3], DType::F64)),
        ];
        for (i, f) in bad.iter().enumerate() {
            let mut v = good.clone();
            f(&mut v);
            let p = dir.join(format!("bad-{i}.safetensors"));
            write(&p, &v);
            assert!(validate_tower_roster(&parent, &p, &spec).is_err(), "{i}");
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}
