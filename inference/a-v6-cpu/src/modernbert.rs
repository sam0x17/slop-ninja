//! ModernBERT
//!
//! ModernBERT is a modernized bidirectional encoder-only Transformer model.
//! - [Arxiv](https://arxiv.org/abs/2412.13663) "Smarter, Better, Faster, Longer: A Modern Bidirectional Encoder for Fast, Memory Efficient, and Long Context Finetuning and Inference"
//! - Upstream [GitHub repo](https://github.com/AnswerDotAI/ModernBERT).
//! - See modernbert in [candle-examples](https://github.com/huggingface/candle/tree/main/candle-examples/) for runnable code
//!

use candle::{DType, Device, IndexOp, Result, Tensor, D};
use crate::reference_layer_norm::{layer_norm_no_bias, LayerNorm};
use candle_nn::{
    embedding, linear, linear_no_bias, ops::softmax, Embedding,
    Linear, Module, VarBuilder,
};
use serde::Deserialize;

use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Config {
    pub vocab_size: usize,
    pub hidden_size: usize,
    pub num_hidden_layers: usize,
    pub num_attention_heads: usize,
    pub intermediate_size: usize,
    pub max_position_embeddings: usize,
    pub layer_norm_eps: f64,
    pub pad_token_id: u32,
    pub global_attn_every_n_layers: usize,
    pub global_rope_theta: f64,
    pub local_attention: usize,
    pub local_rope_theta: f64,
    #[serde(default)]
    #[serde(flatten)]
    pub classifier_config: Option<ClassifierConfig>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Copy, Default)]
#[serde(rename_all = "lowercase")]
pub enum ClassifierPooling {
    #[default]
    CLS,
    MEAN,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ClassifierConfig {
    pub id2label: HashMap<String, String>,
    pub label2id: HashMap<String, String>,
    pub classifier_pooling: ClassifierPooling,
}

#[derive(Debug, Clone)]
struct RotaryEmbedding {
    sin: Tensor,
    cos: Tensor,
}

#[cfg(feature = "mkl")]
fn training_cpu_softmax(x: &Tensor) -> Result<Tensor> {
    // ATen DEFAULT _vec_softmax_lastdim / reduce_all at PyTorch 5811a8d7...
    // Eight FP32 lanes, tail merged into the first lanes, sequential horizontal
    // reduction, then reciprocal multiplication. No fitted correction.
    let width = *x.dims().last().ok_or_else(|| candle::Error::Msg("softmax scalar".into()))?;
    if width == 0 || x.dtype() != DType::F32 || !x.device().is_cpu() {
        candle::bail!("training CPU softmax requires nonempty CPU FP32 rows");
    }
    let mut values = x.flatten_all()?.to_vec1::<f32>()?;
    for row in values.chunks_exact_mut(width) {
        let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        for v in row.iter_mut() { *v = (*v - max).exp(); }
        let sum = if width < 8 {
            row[1..].iter().fold(row[0], |a,b| a+b)
        } else {
            let mut lanes = [0f32;8];
            lanes.copy_from_slice(&row[..8]);
            for chunk in row[8..].chunks(8) {
                for (i,v) in chunk.iter().enumerate() { lanes[i] += v; }
            }
            lanes[1..].iter().fold(lanes[0], |a,b| a+b)
        };
        let inverse = 1f32 / sum;
        for v in row { *v *= inverse; }
    }
    Tensor::from_vec(values,x.shape(),x.device())
}

impl RotaryEmbedding {
    /// HF `ModernBertRotaryEmbedding` (default rope, the function A was trained
    /// and evaluated with) forms every angle as one float32 product of the
    /// float32 position and inverse frequency, takes cos/sin in float32 with
    /// autocast disabled, and only then casts to the model dtype. The tables
    /// are built the same way here for every dtype: the FP64 profile widens the
    /// float32 tables exactly instead of recomputing the angles in float64
    /// (which would be a different function: about 1e-4 rad apart at position
    /// 2,047). The FP32 graph is unchanged by this.
    fn new(dtype: DType, config: &Config, rope_theta: f64, dev: &Device) -> Result<Self> {
        let dim = config.hidden_size / config.num_attention_heads;
        let inv_freq: Vec<_> = (0..dim)
            .step_by(2)
            .map(|i| 1f32 / rope_theta.powf(i as f64 / dim as f64) as f32)
            .collect();
        let inv_freq_len = inv_freq.len();
        let inv_freq = Tensor::from_vec(inv_freq, (1, inv_freq_len), dev)?;
        let max_seq_len = config.max_position_embeddings;
        let t = Tensor::arange(0u32, max_seq_len as u32, dev)?
            .to_dtype(DType::F32)?
            .reshape((max_seq_len, 1))?;
        // One rounding per angle, as HF's k = 1 float32 matmul.
        let freqs = t.broadcast_mul(&inv_freq)?;
        Ok(Self {
            sin: freqs.sin()?.to_dtype(dtype)?,
            cos: freqs.cos()?.to_dtype(dtype)?,
        })
    }

    fn apply_rotary_emb_qkv(&self, q: &Tensor, k: &Tensor) -> Result<(Tensor, Tensor)> {
        let q_embed = candle_nn::rotary_emb::rope(&q.contiguous()?, &self.cos, &self.sin)?;
        let k_embed = candle_nn::rotary_emb::rope(&k.contiguous()?, &self.cos, &self.sin)?;
        Ok((q_embed, k_embed))
    }
}

#[derive(Clone)]
struct ModernBertAttention {
    qkv: Linear,
    proj: Linear,
    num_attention_heads: usize,
    attention_head_size: usize,
    rotary_emb: Arc<RotaryEmbedding>,
}

impl ModernBertAttention {
    fn load(vb: VarBuilder, config: &Config, rotary_emb: Arc<RotaryEmbedding>) -> Result<Self> {
        let num_attention_heads = config.num_attention_heads;
        let attention_head_size = config.hidden_size / config.num_attention_heads;

        let qkv = linear_no_bias(config.hidden_size, config.hidden_size * 3, vb.pp("Wqkv"))?;
        let proj = linear_no_bias(config.hidden_size, config.hidden_size, vb.pp("Wo"))?;

        Ok(Self {
            qkv,
            proj,
            num_attention_heads,
            attention_head_size,
            rotary_emb,
        })
    }

    fn forward(&self, hidden_states: &Tensor, attention_mask: &Tensor) -> Result<Tensor> {
        let xs = hidden_states.clone();
        let (b, seq_len, d) = xs.dims3()?;
        let projected = xs.apply(&self.qkv)?;
        crate::trace::local("attn.Wqkv", &projected)?;
        let qkv = projected.reshape((
                b,
                seq_len,
                3,
                self.num_attention_heads,
                self.attention_head_size,
            ))?
            .permute((2, 0, 3, 1, 4))?;

        let q = qkv.get(0)?;
        let k = qkv.get(1)?;
        let v = qkv.get(2)?;

        let (q, k) = self.rotary_emb.apply_rotary_emb_qkv(&q, &k)?;
        crate::trace::local("attn.rotated_q", &q)?;
        crate::trace::local("attn.rotated_k", &k)?;

        let scale = (self.attention_head_size as f64).powf(-0.5);
        let q = (q * scale)?;

        let att = q.matmul(&k.transpose(D::Minus2, D::Minus1)?)?;
        crate::trace::local("attn.scores", &att)?;

        let att = att.broadcast_add(attention_mask)?;
        #[cfg(not(feature = "mkl"))]
        let att = softmax(&att, D::Minus1)?;
        // The ported training softmax is FP32-only; the FP64 profile uses Candle's.
        #[cfg(feature = "mkl")]
        let att = if att.dtype() == DType::F32 {
            training_cpu_softmax(&att)?
        } else {
            softmax(&att, D::Minus1)?
        };
        crate::trace::local("attn.probabilities", &att)?;

        let xs = att.matmul(&v)?;
        crate::trace::local("attn.context", &xs)?;

        let xs = xs.transpose(1, 2)?.reshape((b, seq_len, d))?;
        let xs = xs.apply(&self.proj)?;
        crate::trace::local("attn.Wo", &xs)?;
        let xs = xs.reshape((b, seq_len, d))?;

        Ok(xs)
    }
}

#[derive(Clone)]
pub struct ModernBertMLP {
    wi: Linear,
    wo: Linear,
}

impl ModernBertMLP {
    fn load(vb: VarBuilder, config: &Config) -> Result<Self> {
        let wi = linear_no_bias(
            config.hidden_size,
            config.intermediate_size * 2,
            vb.pp("Wi"),
        )?;
        let wo = linear_no_bias(config.intermediate_size, config.hidden_size, vb.pp("Wo"))?;
        Ok(Self { wi, wo })
    }
}

impl Module for ModernBertMLP {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        let xs = xs.apply(&self.wi)?;
        crate::trace::local("mlp.Wi", &xs)?;
        let xs = xs.chunk(2, D::Minus1)?;
        let activation = xs[0].gelu_erf()?;
        crate::trace::local("mlp.act", &activation)?;
        let xs = (&activation * &xs[1])?.apply(&self.wo)?; // GeGLU
        crate::trace::local("mlp.Wo", &xs)?;
        Ok(xs)
    }
}

#[derive(Clone)]
pub struct ModernBertLayer {
    attn: ModernBertAttention,
    mlp: ModernBertMLP,
    attn_norm: Option<LayerNorm>,
    mlp_norm: LayerNorm,
    uses_local_attention: bool,
}

impl ModernBertLayer {
    fn load(
        vb: VarBuilder,
        config: &Config,
        rotary_emb: Arc<RotaryEmbedding>,
        uses_local_attention: bool,
    ) -> Result<Self> {
        let attn = ModernBertAttention::load(vb.pp("attn"), config, rotary_emb)?;
        let mlp = ModernBertMLP::load(vb.pp("mlp"), config)?;
        let attn_norm = layer_norm_no_bias(
            config.hidden_size,
            config.layer_norm_eps,
            vb.pp("attn_norm"),
        )
        .ok();
        let mlp_norm =
            layer_norm_no_bias(config.hidden_size, config.layer_norm_eps, vb.pp("mlp_norm"))?;
        Ok(Self {
            attn,
            mlp,
            attn_norm,
            mlp_norm,
            uses_local_attention,
        })
    }

    fn forward(
        &self,
        xs: &Tensor,
        global_attention_mask: &Tensor,
        local_attention_mask: &Tensor,
    ) -> Result<Tensor> {
        let residual = xs.clone();
        let mut xs = xs.clone();
        if let Some(norm) = &self.attn_norm {
            xs = xs.apply(norm)?;
        }
        crate::trace::local("attn_norm", &xs)?;

        let attention_mask = if self.uses_local_attention {
            &global_attention_mask.broadcast_add(local_attention_mask)?
        } else {
            global_attention_mask
        };
        let xs = self.attn.forward(&xs, attention_mask)?;
        let xs = (xs + residual)?;
        let normed = xs.apply(&self.mlp_norm)?;
        crate::trace::local("mlp_norm", &normed)?;
        let mlp_out = normed.apply(&self.mlp)?;
        let xs = (xs + mlp_out)?;
        Ok(xs)
    }
}

#[derive(Clone)]
pub struct ModernBertHead {
    dense: Linear,
    norm: LayerNorm,
}

impl ModernBertHead {
    fn load(vb: VarBuilder, config: &Config) -> Result<Self> {
        let dense = linear_no_bias(config.hidden_size, config.hidden_size, vb.pp("dense"))?;
        let norm = layer_norm_no_bias(config.hidden_size, config.layer_norm_eps, vb.pp("norm"))?;
        Ok(Self { dense, norm })
    }
}

impl Module for ModernBertHead {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        let xs = xs.apply(&self.dense)?.gelu_erf()?.apply(&self.norm)?;
        Ok(xs)
    }
}

#[derive(Clone)]
pub struct ModernBertDecoder {
    decoder: Linear,
}

impl ModernBertDecoder {
    fn load(vb: VarBuilder, config: &Config) -> Result<Self> {
        // The decoder weights are tied with the embeddings layer weights
        let decoder_weights = vb.get(
            (config.vocab_size, config.hidden_size),
            "model.embeddings.tok_embeddings.weight",
        )?;
        let decoder_bias = vb.get(config.vocab_size, "decoder.bias")?;
        let decoder = Linear::new(decoder_weights, Some(decoder_bias));
        Ok(Self { decoder })
    }
}

impl Module for ModernBertDecoder {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        let xs = xs.apply(&self.decoder)?;
        Ok(xs)
    }
}

// Global attention mask calculated from padded token inputs
fn prepare_4d_attention_mask(
    mask: &Tensor,
    dtype: DType,
    tgt_len: Option<usize>,
) -> Result<Tensor> {
    let bsz = mask.dim(0)?;
    let src_len = mask.dim(1)?;
    let tgt_len = tgt_len.unwrap_or(src_len);

    let expanded_mask = mask
        .unsqueeze(1)?
        .unsqueeze(2)?
        .expand((bsz, 1, tgt_len, src_len))?
        .to_dtype(dtype)?;

    let inverted_mask = (1.0 - expanded_mask)?;

    (inverted_mask * f32::MIN as f64)?.to_dtype(dtype)
}

// Attention mask caused by the sliding window
fn get_local_attention_mask(
    seq_len: usize,
    max_distance: usize,
    device: &Device,
) -> Result<Tensor> {
    let mask: Vec<_> = (0..seq_len)
        .flat_map(|i| {
            (0..seq_len).map(move |j| {
                if (j as i32 - i as i32).abs() > max_distance as i32 {
                    f32::NEG_INFINITY
                } else {
                    0.
                }
            })
        })
        .collect();
    Tensor::from_slice(&mask, (seq_len, seq_len), device)
}

// ModernBERT backbone
#[derive(Clone)]
pub struct ModernBert {
    word_embeddings: Embedding,
    norm: LayerNorm,
    layers: Vec<ModernBertLayer>,
    final_norm: LayerNorm,
    local_attention_size: usize,
    dtype: DType,
}

impl ModernBert {
    pub fn load(vb: VarBuilder, config: &Config) -> Result<Self> {
        let word_embeddings = embedding(
            config.vocab_size,
            config.hidden_size,
            vb.pp("model.embeddings.tok_embeddings"),
        )?;
        let norm = layer_norm_no_bias(
            config.hidden_size,
            config.layer_norm_eps,
            vb.pp("model.embeddings.norm"),
        )?;
        let global_rotary_emb = Arc::new(RotaryEmbedding::new(
            vb.dtype(),
            config,
            config.global_rope_theta,
            vb.device(),
        )?);
        let local_rotary_emb = Arc::new(RotaryEmbedding::new(
            vb.dtype(),
            config,
            config.local_rope_theta,
            vb.device(),
        )?);

        let mut layers = Vec::with_capacity(config.num_hidden_layers);
        for layer_id in 0..config.num_hidden_layers {
            let layer_uses_local_attention = layer_id % config.global_attn_every_n_layers != 0;
            layers.push(ModernBertLayer::load(
                vb.pp(format!("model.layers.{layer_id}")),
                config,
                if layer_uses_local_attention {
                    local_rotary_emb.clone()
                } else {
                    global_rotary_emb.clone()
                },
                layer_uses_local_attention,
            )?);
        }

        let final_norm = layer_norm_no_bias(
            config.hidden_size,
            config.layer_norm_eps,
            vb.pp("model.final_norm"),
        )?;

        Ok(Self {
            word_embeddings,
            norm,
            layers,
            final_norm,
            local_attention_size: config.local_attention,
            dtype: vb.dtype(),
        })
    }

    pub fn forward(&self, xs: &Tensor, mask: &Tensor) -> Result<Tensor> {
        Ok(self.forward_with_branch(xs, mask, None)?.0)
    }

    /// The final-norm states and, with `branch = Some(layer)`, the hidden state
    /// entering that layer together with the attention masks every layer used
    /// (the input of a [`SpanTower`]). The encoder computation is unchanged.
    pub fn forward_with_branch(
        &self,
        xs: &Tensor,
        mask: &Tensor,
        branch: Option<usize>,
    ) -> Result<(Tensor, Option<BranchInput>)> {
        if let Some(b) = branch {
            if b == 0 || b >= self.layers.len() {
                candle::bail!("branch layer {b} outside 1..{}", self.layers.len());
            }
        }
        let seq_len = xs.shape().dims()[1];
        // F32 is the qualified V6 graph; F64 runs the same graph at double precision.
        let global_attention_mask =
            prepare_4d_attention_mask(mask, self.dtype, None)?.to_device(xs.device())?;
        let local_attention_mask =
            get_local_attention_mask(seq_len, self.local_attention_size / 2, xs.device())?
                .to_dtype(self.dtype)?;
        let mut xs = xs.apply(&self.word_embeddings)?;
        crate::trace::record("model.embeddings.tok_embeddings", &xs)?;
        xs = xs.apply(&self.norm)?;
        crate::trace::record("model.embeddings.norm", &xs)?;
        let mut captured = None;
        for (index, layer) in self.layers.iter().enumerate() {
            if branch == Some(index) {
                captured = Some(xs.clone());
            }
            crate::trace::layer(index);
            xs = layer.forward(&xs, &global_attention_mask, &local_attention_mask)?;
            crate::trace::record(&format!("model.layers.{index}"), &xs)?;
        }
        let xs = xs.apply(&self.final_norm)?;
        crate::trace::record("model.final_norm", &xs)?;
        let branch = match (branch, captured) {
            (None, _) => None,
            (Some(layer), Some(hidden)) => Some(BranchInput {
                layer,
                hidden,
                global_attention_mask,
                local_attention_mask,
            }),
            (Some(layer), None) => candle::bail!("branch layer {layer} not reached"),
        };
        Ok((xs, branch))
    }
}

/// The hidden state entering encoder layer `layer`, with the encoder's masks.
pub struct BranchInput {
    layer: usize,
    hidden: Tensor,
    global_attention_mask: Tensor,
    local_attention_mask: Tensor,
}

impl BranchInput {
    pub fn layer(&self) -> usize {
        self.layer
    }
}

/// Slop Ninja addition (protocol spans-S2, arm S2c): a span tower of copies of
/// the encoder layers `first..num_hidden_layers` and of the final norm, with
/// their own weights under `layers.{j}` and `norm`. Copy j keeps its source
/// layer's attention kind (local when `(first + j) % global_attn_every_n_layers
/// != 0`) and rotary table, and runs the encoder's layer code on the hidden
/// state entering layer `first` with the encoder's masks and positions; the
/// copied final norm follows. The encoder itself is not touched.
#[derive(Clone)]
pub struct SpanTower {
    first: usize,
    layers: Vec<ModernBertLayer>,
    norm: LayerNorm,
}

impl SpanTower {
    pub fn load(vb: VarBuilder, config: &Config, first: usize) -> Result<Self> {
        let n = config.num_hidden_layers;
        if first == 0 || first >= n {
            candle::bail!("span tower must copy the top layers: first {first} of {n}");
        }
        let global_rotary_emb = Arc::new(RotaryEmbedding::new(
            vb.dtype(),
            config,
            config.global_rope_theta,
            vb.device(),
        )?);
        let local_rotary_emb = Arc::new(RotaryEmbedding::new(
            vb.dtype(),
            config,
            config.local_rope_theta,
            vb.device(),
        )?);
        let mut layers = Vec::with_capacity(n - first);
        for layer_id in first..n {
            let local = layer_id % config.global_attn_every_n_layers != 0;
            let layer = ModernBertLayer::load(
                vb.pp(format!("layers.{}", layer_id - first)),
                config,
                if local {
                    local_rotary_emb.clone()
                } else {
                    global_rotary_emb.clone()
                },
                local,
            )?;
            // Copies of layers above 0 carry their attention norm (ModernBertLayer::load treats it as optional).
            if layer.attn_norm.is_none() {
                candle::bail!("span tower layer {layer_id} lacks its attention norm");
            }
            layers.push(layer);
        }
        let norm = layer_norm_no_bias(config.hidden_size, config.layer_norm_eps, vb.pp("norm"))?;
        Ok(Self {
            first,
            layers,
            norm,
        })
    }

    pub fn first_layer(&self) -> usize {
        self.first
    }

    /// Per copied layer: true for local (sliding-window) attention.
    pub fn local_attention_layers(&self) -> Vec<bool> {
        self.layers.iter().map(|l| l.uses_local_attention).collect()
    }

    /// The tower's normed states from the hidden state entering its first layer.
    pub fn forward(&self, input: &BranchInput) -> Result<Tensor> {
        if input.layer != self.first {
            candle::bail!("span tower reads layer {} input, got layer {}", self.first, input.layer);
        }
        let mut xs = input.hidden.clone();
        for layer in &self.layers {
            xs = layer.forward(&xs, &input.global_attention_mask, &input.local_attention_mask)?;
        }
        let xs = xs.apply(&self.norm)?;
        crate::trace::record("span_tower.norm", &xs)?;
        Ok(xs)
    }
}

// ModernBERT for the fill-mask task
#[derive(Clone)]
pub struct ModernBertForMaskedLM {
    model: ModernBert,
    decoder: ModernBertDecoder,
    head: ModernBertHead,
}

impl ModernBertForMaskedLM {
    pub fn load(vb: VarBuilder, config: &Config) -> Result<Self> {
        let model = ModernBert::load(vb.clone(), config)?;
        let decoder = ModernBertDecoder::load(vb.clone(), config)?;
        let head = ModernBertHead::load(vb.pp("head"), config)?;
        Ok(Self {
            model,
            decoder,
            head,
        })
    }

    pub fn forward(&self, xs: &Tensor, mask: &Tensor) -> Result<Tensor> {
        let xs = self
            .model
            .forward(xs, mask)?
            .apply(&self.head)?
            .apply(&self.decoder)?;
        Ok(xs)
    }
}

#[derive(Clone)]
pub struct ModernBertClassifier {
    classifier: Linear,
}

impl ModernBertClassifier {
    fn load(vb: VarBuilder, config: &Config) -> Result<Self> {
        // The decoder weights are tied with the embeddings layer weights
        let classifier = linear(
            config.hidden_size,
            config
                .classifier_config
                .as_ref()
                .map(|cc| cc.id2label.len())
                .unwrap_or_default(),
            vb.pp("classifier"),
        )?;
        Ok(Self { classifier })
    }
}

impl Module for ModernBertClassifier {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        let xs = xs.apply(&self.classifier)?;
        softmax(&xs, D::Minus1)
    }
}

#[derive(Clone)]
pub struct ModernBertForSequenceClassification {
    model: ModernBert,
    head: ModernBertHead,
    classifier: ModernBertClassifier,
    classifier_pooling: ClassifierPooling,
}

impl ModernBertForSequenceClassification {
    pub fn load(vb: VarBuilder, config: &Config) -> Result<Self> {
        let model = ModernBert::load(vb.clone(), config)?;
        let classifier = ModernBertClassifier::load(vb.clone(), config)?;
        let head = ModernBertHead::load(vb.pp("head"), config)?;
        Ok(Self {
            model,
            head,
            classifier,
            classifier_pooling: config
                .classifier_config
                .as_ref()
                .map(|cc| cc.classifier_pooling)
                .unwrap_or_default(),
        })
    }

    pub fn forward(&self, xs: &Tensor, mask: &Tensor) -> Result<Tensor> {
        let output = self.model.forward(xs, mask)?;
        let last_hidden_state = match self.classifier_pooling {
            ClassifierPooling::CLS => output.i((.., 0, ..))?.contiguous()?,
            ClassifierPooling::MEAN => {
                let unsqueezed_mask = &mask.unsqueeze(D::Minus1)?.to_dtype(DType::F32)?;
                let sum_output = output.broadcast_mul(unsqueezed_mask)?.sum(1)?;
                sum_output.broadcast_div(&mask.sum_keepdim(1)?.to_dtype(DType::F32)?)?
            }
        };
        let xs = self
            .head
            .forward(&last_hidden_state)?
            .apply(&self.classifier)?;
        Ok(xs)
    }
}

#[cfg(test)]
mod rotary_tests {
    use super::*;

    fn config() -> Config {
        Config {
            vocab_size: 8,
            hidden_size: 768,
            num_hidden_layers: 1,
            num_attention_heads: 12,
            intermediate_size: 8,
            max_position_embeddings: 2048,
            layer_norm_eps: 1e-5,
            pad_token_id: 0,
            global_attn_every_n_layers: 3,
            global_rope_theta: 160_000.0,
            local_attention: 128,
            local_rope_theta: 10_000.0,
            classifier_config: None,
        }
    }

    /// The FP64 tables are the FP32 (HF) tables widened exactly; each angle is
    /// one float32 product, and float64 angles would be a different function.
    #[test]
    fn rotary_tables_are_hf_float32_tables_widened() {
        let (c, dev) = (config(), Device::Cpu);
        for theta in [c.global_rope_theta, c.local_rope_theta] {
            let r32 = RotaryEmbedding::new(DType::F32, &c, theta, &dev).unwrap();
            let r64 = RotaryEmbedding::new(DType::F64, &c, theta, &dev).unwrap();
            let mut f64_angle_gap = 0f64;
            let tables: [(&Tensor, &Tensor, fn(f32) -> f32, fn(f64) -> f64); 2] =
                [(&r32.cos, &r64.cos, f32::cos, f64::cos), (&r32.sin, &r64.sin, f32::sin, f64::sin)];
            for (t32, t64, f, f_wide) in tables {
                assert_eq!(t64.dtype(), DType::F64);
                let (a, b) = (t32.to_vec2::<f32>().unwrap(), t64.to_vec2::<f64>().unwrap());
                assert_eq!((a.len(), a[0].len()), (2048, 32));
                for (ra, rb) in a.iter().zip(&b) {
                    for (x, y) in ra.iter().zip(rb) {
                        assert_eq!(*x as f64, *y);
                    }
                }
                for p in [1usize, 511, 2047] {
                    for i in 0..32 {
                        let inv = 1f32 / theta.powf((2 * i) as f64 / 64.0) as f32;
                        let want = f((p as f32) * inv);
                        assert!((a[p][i] - want).abs() <= 2.0 * f32::EPSILON, "{p} {i}");
                        let wide_angle = f_wide((p as f64) * inv as f64);
                        f64_angle_gap = f64_angle_gap.max((b[p][i] - wide_angle).abs());
                    }
                }
            }
            // float64 angles move the tables by far more than float32 cos/sin rounding
            assert!(f64_angle_gap > 1e-6, "{f64_angle_gap}");
        }
    }
}
