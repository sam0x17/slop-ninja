//! Heads v3 (`slop_ninja_full_a_heads_v3`, protocol spans-S2 arm S2c) and its
//! span tower. Heads v3 has the heads v2 document heads, token head and span
//! emission spec, plus a `span_tower` declaration ([`TowerSpec`]). Its token
//! head reads the span tower's normed token states
//! (`span_tower_final_norm_token_states`) instead of the encoder's final-norm
//! states. The span tower is a set of copies of the encoder's top layers and
//! of its final norm, with their own weights. It runs from the hidden state
//! entering the first copied layer, with the encoder's attention masks and
//! positions. Document outputs never read the tower.
//!
//! This file is separate from the heads runtime that the served profile and
//! the artifact-check spec pin by digest (`full_a.rs`, `full_a_spans.rs`,
//! `involvement_calibration.rs` and `shared.rs`), and it leaves those files
//! unchanged. The Container, `a_scorer` and artifact-check compile the four
//! pinned files through `#[path]` and never compile this one. Every crate that
//! links `grammar-a-baseline` (`a-v6-cpu`, `grammar-a-positives`) compiles it.
//! [`SpanTowerHeads::from_json`] dispatches on the declared heads schema only,
//! with no runtime-version check:
//! - Heads v1 and v2 bytes go to [`FullAHeads::from_json`] unchanged. Neither
//!   schema may carry a `span_tower` key, not even as null.
//! - For heads v3 it checks what v3 adds: the schema, the token head's input
//!   name and the tower declaration. It then passes the pinned parser the same
//!   object read as heads v2: the schema and the input name changed, the
//!   declaration removed, and every other member's exact bytes. The pinned
//!   parser checks everything else.
//!
//! The token head function is the same for every schema; only the token
//! states it reads differ.
use crate::full_a::{self, FullAHeads};
use crate::full_a_spans::TokenHead;
use crate::shared::Tensor;
use anyhow::{Context, Result, bail, ensure};
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;

/// Heads v2 plus the spans-S2 span tower declaration (export v4).
pub const SCHEMA_V3: &str = "slop_ninja_full_a_heads_v3";
/// Token-head input of heads v1/v2: the encoder's final-norm token states.
pub const FINAL_NORM_INPUT: &str = "final_norm_token_states";
/// Token-head input of heads v3: the span tower's normed token states.
pub const TOWER_INPUT: &str = "span_tower_final_norm_token_states";
pub const TOWER_RULE: &str =
    "slop_ninja_s2_span_tower_v1: token_head(final_norm_copy(layers_copy[L-K..L-1](h_in(L-K))))";
pub const TOWER_CAPTURE: &str = "hidden_state_entering_source_layer";
pub const TOWER_WEIGHTS: &str = "tower.safetensors";
pub const TOWER_PREFIX: &str = "span_tower.";
/// Tensors of one copied layer, under `span_tower.layers.{j}.` (j = 0 for the first copy).
pub const TOWER_LAYER_TENSORS: [&str; 6] = [
    "attn_norm.weight",
    "attn.Wqkv.weight",
    "attn.Wo.weight",
    "mlp_norm.weight",
    "mlp.Wi.weight",
    "mlp.Wo.weight",
];
/// `full_a.rs`'s research size limit, applied before the heads object is read.
const SIZE_LIMIT: usize = 64 * 1024 * 1024;

/// Which token states feed the token head (declared by the heads schema, never chosen at run time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenInput {
    /// Heads v1/v2: the encoder's final-norm token states.
    FinalNorm,
    /// Heads v3: the span tower's normed token states.
    SpanTower,
}

/// Row-major token states of one request (count x width): the encoder's
/// final-norm states and, when the export carries a span tower, the tower's.
#[derive(Clone, Copy, Debug)]
pub struct TokenStates<'a> {
    pub final_norm: &'a [f64],
    pub span_tower: Option<&'a [f64]>,
}

/// Heads v3 `span_tower`: copies of the encoder layers `source_layer..encoder_layers`
/// and of its final norm (own weights in `tower.safetensors`, tensor names under
/// `span_tower.`), each keeping its layer's attention kind (global when
/// `layer % global_attn_every_n_layers == 0`, else local with the encoder's
/// window) and rotary table, run on the hidden state entering `source_layer`
/// with the encoder's masks and positions.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TowerSpec {
    pub rule: String,
    pub encoder_layers: usize,
    pub source_layer: usize,
    pub layers: Vec<usize>,
    pub attention: Vec<String>,
    pub global_attn_every_n_layers: usize,
    pub local_attention: usize,
    pub capture: String,
    pub masks_and_positions: String,
    pub final_norm: String,
    pub weights: String,
    pub tensor_prefix: String,
}

impl TowerSpec {
    pub fn from_json(v: &Value) -> Result<Self> {
        serde_json::from_value::<Self>(v.clone())
            .context("span tower declaration")?
            .checked()
    }

    fn checked(self) -> Result<Self> {
        let s = self;
        ensure!(
            s.rule == TOWER_RULE
                && s.capture == TOWER_CAPTURE
                && s.masks_and_positions == "encoder"
                && s.final_norm == "copy"
                && s.weights == TOWER_WEIGHTS
                && s.tensor_prefix == TOWER_PREFIX,
            "span tower rule identifiers"
        );
        let n = s.encoder_layers;
        ensure!(
            s.source_layer >= 1
                && s.source_layer < n
                && s.layers == (s.source_layer..n).collect::<Vec<_>>(),
            "span tower copies the encoder's top layers"
        );
        ensure!(
            s.global_attn_every_n_layers >= 1 && s.local_attention >= 2,
            "span tower attention configuration"
        );
        ensure!(
            s.attention.len() == s.layers.len()
                && s.layers.iter().zip(&s.attention).all(|(i, a)| {
                    a == if i % s.global_attn_every_n_layers == 0 {
                        "global"
                    } else {
                        "local"
                    }
                }),
            "span tower attention kinds differ from the encoder's layers"
        );
        Ok(s)
    }

    pub fn depth(&self) -> usize {
        self.layers.len()
    }

    /// Per copied layer: true for local (sliding-window) attention.
    pub fn local_attention_layers(&self) -> Vec<bool> {
        self.attention.iter().map(|a| a == "local").collect()
    }

    /// The exact tensor roster of `tower.safetensors`, sorted.
    pub fn tensor_names(&self) -> Vec<String> {
        let mut out = (0..self.depth())
            .flat_map(|j| {
                TOWER_LAYER_TENSORS
                    .iter()
                    .map(move |t| format!("{TOWER_PREFIX}layers.{j}.{t}"))
            })
            .chain([format!("{TOWER_PREFIX}norm.weight")])
            .collect::<Vec<_>>();
        out.sort();
        out
    }

    /// The encoder tensor each tower tensor copies (same shape), by name.
    pub fn source_tensor(&self, name: &str) -> Result<String> {
        let rest = name
            .strip_prefix(TOWER_PREFIX)
            .context("span tower tensor prefix")?;
        if rest == "norm.weight" {
            return Ok("model.final_norm.weight".into());
        }
        let rest = rest
            .strip_prefix("layers.")
            .context("span tower tensor name")?;
        let (j, t) = rest.split_once('.').context("span tower tensor name")?;
        let j: usize = j.parse().context("span tower layer index")?;
        ensure!(
            j < self.depth() && TOWER_LAYER_TENSORS.contains(&t),
            "unknown span tower tensor {name}"
        );
        Ok(format!("model.layers.{}.{t}", self.layers[j]))
    }
}

/// A token head from heads-format JSON tensors named `token_head.{first,second}.{weight,bias}`
/// (golden vectors and tests); nothing else may be present.
pub fn token_head_from_json(weights: &Value, hidden: usize, input: usize) -> Result<TokenHead> {
    let mut w: BTreeMap<String, Tensor> =
        serde_json::from_value(weights.clone()).context("token head tensors")?;
    let head = TokenHead::take(&mut w, hidden, input)?;
    ensure!(w.is_empty(), "unrecognized token head tensors");
    Ok(head)
}

/// The heads `token_head` declaration, field for field as the pinned parser reads it.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TokenHeadDeclaration {
    hidden: usize,
    activation: String,
    rule: String,
    input: String,
    label_policy: String,
    feeds_document_heads: bool,
}

/// A JSON object's members in document order, each value as its exact bytes.
/// A repeated key is refused, as the pinned parser refuses a repeated field.
struct Members<'a>(Vec<(String, &'a RawValue)>);

impl<'a> Members<'a> {
    fn get(&self, key: &str) -> Option<&'a RawValue> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
    }
}

impl<'de> Deserialize<'de> for Members<'de> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct Object;
        impl<'de> Visitor<'de> for Object {
            type Value = Members<'de>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut out: Vec<(String, &'de RawValue)> = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, &'de RawValue>()? {
                    if out.iter().any(|(seen, _)| *seen == k) {
                        return Err(de::Error::custom(format!("duplicate field `{k}`")));
                    }
                    out.push((k, v));
                }
                Ok(Members(out))
            }
        }
        d.deserialize_map(Object)
    }
}

/// Full-A heads of any supported schema, with the heads v3 span tower
/// declaration when present. Dereferences to the pinned [`FullAHeads`] for the
/// document heads and span emission; [`SpanTowerHeads::token_logits`] takes
/// the token states the schema declares.
pub struct SpanTowerHeads {
    heads: FullAHeads,
    span_tower: Option<TowerSpec>,
}

impl Deref for SpanTowerHeads {
    type Target = FullAHeads;
    fn deref(&self) -> &FullAHeads {
        &self.heads
    }
}

impl SpanTowerHeads {
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= SIZE_LIMIT,
            "artifact exceeds research size limit"
        );
        let members: Members = serde_json::from_slice(bytes).context("full-A heads object")?;
        let schema = members
            .get("schema")
            .and_then(|v| serde_json::from_str::<String>(v.get()).ok());
        if schema.as_deref() != Some(SCHEMA_V3) {
            // Only heads v3 declares a span tower; v1 and v2 never carry the key, not even as null.
            ensure!(
                members.get("span_tower").is_none(),
                "only full-A heads v3 declares a span tower"
            );
            return Ok(Self {
                heads: FullAHeads::from_json(bytes)?,
                span_tower: None,
            });
        }
        let tower = members
            .get("span_tower")
            .context("full-A heads v3 declares a span tower")?;
        let tower = serde_json::from_str::<TowerSpec>(tower.get())
            .context("span tower declaration")?
            .checked()?;
        let mut head: TokenHeadDeclaration = members
            .get("token_head")
            .map(|v| serde_json::from_str(v.get()))
            .transpose()
            .context("token head declaration")?
            .context("the span tower feeds the token head")?;
        ensure!(head.input == TOWER_INPUT, "token head meaning differs");
        head.input = FINAL_NORM_INPUT.into();
        // Heads v2 bytes: every member but these three is copied exactly.
        let mut v2 = Vec::with_capacity(bytes.len());
        v2.push(b'{');
        for (k, v) in &members.0 {
            let value = match k.as_str() {
                "span_tower" => continue,
                "schema" => serde_json::to_string(full_a::SCHEMA)?,
                "token_head" => serde_json::to_string(&head)?,
                _ => v.get().to_owned(),
            };
            if v2.len() > 1 {
                v2.push(b',');
            }
            v2.extend(serde_json::to_vec(k)?);
            v2.push(b':');
            v2.extend(value.as_bytes());
        }
        v2.push(b'}');
        let heads = FullAHeads::from_json(&v2)?;
        ensure!(
            heads.has_token_head(),
            "the span tower feeds the token head"
        );
        Ok(Self {
            heads,
            span_tower: Some(tower),
        })
    }

    /// The pinned heads runtime's view (document heads, token head and emission spec).
    pub fn heads(&self) -> &FullAHeads {
        &self.heads
    }

    /// The token states the token head reads, as the heads schema declares (None without a token head).
    pub fn token_input(&self) -> Option<TokenInput> {
        self.heads
            .has_token_head()
            .then_some(match self.span_tower {
                Some(_) => TokenInput::SpanTower,
                None => TokenInput::FinalNorm,
            })
    }

    /// The heads v3 span tower declaration (None for heads v1 and v2).
    pub fn span_tower(&self) -> Option<&TowerSpec> {
        self.span_tower.as_ref()
    }

    /// spans-v1 token logits at word-bearing positions (None elsewhere, and
    /// None overall without a token head), from the declared token states:
    /// the encoder's final-norm states for heads v2, the span tower's for v3.
    /// Tower states are required exactly when the heads declare a tower.
    pub fn token_logits(
        &self,
        states: &TokenStates<'_>,
        words: &[bool],
    ) -> Result<Option<Vec<Option<f64>>>> {
        ensure!(
            states.span_tower.is_some() == self.span_tower.is_some(),
            "span tower token states must be given exactly when the heads declare a tower"
        );
        let x = match (self.token_input(), states.span_tower) {
            (None, _) => return Ok(None),
            (Some(TokenInput::FinalNorm), None) => states.final_norm,
            (Some(TokenInput::SpanTower), Some(tower)) => tower,
            _ => bail!("token states differ from the declared token-head input"),
        };
        self.heads.token_logits(x, words)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::full_a_spans;
    use serde_json::json;

    const TOWER_GOLDEN: &str = include_str!("../tests/golden/span-tower-golden-v1.json");

    fn linear(rows: usize, cols: usize, scale: f64, seed: u64) -> (Value, Value) {
        let mut state = seed;
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * scale
        };
        let w = (0..rows * cols).map(|_| next()).collect::<Vec<_>>();
        let b = (0..rows).map(|_| next()).collect::<Vec<_>>();
        (
            json!({"shape":[rows, cols],"values":w}),
            json!({"shape":[rows],"values":b}),
        )
    }

    /// The golden weight generator (`slop_ninja_tower_golden_weights_v1`), as `span_tower_golden.py`.
    fn golden_values(name: &str, count: usize) -> Vec<f64> {
        let mut state = name.bytes().fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ b as u64).wrapping_mul(0x100000001b3)
        });
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

    /// Heads v3 on width-`n` token states: tiny document heads, the golden token head and the golden tower.
    fn tower_artifact(golden: &Value) -> Value {
        let n = golden["config"]["hidden_size"].as_u64().unwrap() as usize;
        let width = golden["token_head_width"].as_u64().unwrap() as usize;
        let (dim, hidden) = (n + 2, 3);
        let mut weights = serde_json::Map::new();
        for (name, rows, cols, seed) in [
            ("residual.0", hidden, dim, 31),
            ("residual.2", n, hidden, 32),
            ("origin", 3, n, 33),
            ("mixed_share", 1, n, 34),
            ("human", 2, n, 35),
            ("ai", 2, n, 36),
        ] {
            let (w, b) = linear(rows, cols, 0.2, seed);
            weights.insert(format!("{name}.weight"), w);
            weights.insert(format!("{name}.bias"), b);
        }
        for (k, shape) in [
            ("first.weight", vec![width, n]),
            ("first.bias", vec![width]),
            ("second.weight", vec![1, width]),
            ("second.bias", vec![1]),
        ] {
            let name = format!("token_head.{k}");
            let values = golden_values(&name, shape.iter().product());
            weights.insert(name, json!({"shape":shape,"values":values}));
        }
        json!({"schema":SCHEMA_V3,
            "architecture":{"dimension":dim,"raw_text_dimension":n,"hidden":hidden,"embedding":n,
                "human_profiles":2,"ai_profiles":2},
            "span_head":null,
            "token_head":{"hidden":width,"activation":"gelu_tanh","rule":full_a_spans::TOKEN_HEAD_RULE,
                "input":TOWER_INPUT,"label_policy":full_a_spans::LABEL_POLICY,"feeds_document_heads":false},
            "span_emission":{"rule":full_a_spans::EMISSION_RULE,"word_rule":full_a_spans::WORD_RULE,
                "word_probability_rule":full_a_spans::WORD_PROBABILITY_RULE,"tilt_rule":full_a_spans::TILT_RULE,
                "splitter":full_a_spans::SPLITTER_ID,
                "decoder":{"rule":full_a_spans::DECODER_RULE,"tau_hi":0.5,"delta":0.0,"min_words":1,"gap":0,"snap":null},
                "sentence_confidence":{"method":"identity_v1"},"span_confidence":{"method":"identity_v1"}},
            "span_tower":golden["tower"],
            "share_mode":"head","contract_sha256":"a".repeat(64),"initialization_sha256":"b".repeat(64),
            "base_temperature":1.5,"text_mean":vec![0.01; n],"text_scale":vec![1.5; n],
            "profiles":{"human":["h1","h2"],"ai":["a1","a2"]},"thresholds":{"human":0.6,"ai":1.0},
            "involvement_calibration":{"method":"identity_v1"},"supervision":{"origin":1},
            "origin_order":["human_only","model_only","mixed"],"weights":weights})
    }

    /// The same heads as heads v2: final-norm input and no tower key.
    fn as_v2(v3: &Value) -> Value {
        let mut v2 = v3.clone();
        v2["schema"] = json!(full_a::SCHEMA);
        v2["token_head"]["input"] = json!(FINAL_NORM_INPUT);
        v2.as_object_mut().unwrap().remove("span_tower");
        v2
    }

    fn floats(v: &Value) -> Vec<f64> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap())
            .collect()
    }

    fn parse(v: &Value) -> Result<SpanTowerHeads> {
        SpanTowerHeads::from_json(&serde_json::to_vec(v).unwrap())
    }

    /// Heads v3: the token head reads the span tower's states (golden vectors from the Python FP64
    /// reference of the trainer's TowerFullA); the document outputs are those of the same heads as v2.
    #[test]
    fn heads_v3_token_head_reads_the_span_tower_and_matches_the_golden_logits() {
        let golden: Value = serde_json::from_str(TOWER_GOLDEN).unwrap();
        assert_eq!(golden["schema"], "slop_ninja_span_tower_golden_v1");
        assert_eq!(golden["rules"]["token_input"], TOWER_INPUT);
        assert_eq!(golden["rules"]["tower"], TOWER_RULE);
        assert_eq!(golden["rules"]["token_head"], full_a_spans::TOKEN_HEAD_RULE);
        let v3 = tower_artifact(&golden);
        let heads = parse(&v3).unwrap();
        assert_eq!(heads.token_input(), Some(TokenInput::SpanTower));
        assert_eq!(heads.span_tower().unwrap().layers, [18, 19, 20, 21]);
        let plain = parse(&as_v2(&v3)).unwrap();
        assert_eq!(plain.token_input(), Some(TokenInput::FinalNorm));
        assert!(plain.span_tower().is_none());
        let n = heads.architecture.raw_text_dimension;
        let mut worst = 0f64;
        for c in golden["cases"].as_array().unwrap() {
            let (fin, tower, want) = (
                floats(&c["final_norm_states"]),
                floats(&c["span_tower_states"]),
                floats(&c["token_logits"]),
            );
            let words = vec![true; want.len()];
            assert_eq!(tower.len(), want.len() * n);
            let got = heads
                .token_logits(
                    &TokenStates {
                        final_norm: &fin,
                        span_tower: Some(&tower),
                    },
                    &words,
                )
                .unwrap()
                .unwrap();
            for (g, w) in got.iter().zip(&want) {
                worst = worst.max((g.unwrap() - w).abs());
            }
            // The pinned token head on the tower states is the same function, bit for bit.
            assert_eq!(
                heads.heads().token_logits(&tower, &words).unwrap().unwrap(),
                got
            );
            // Without the tower states (or with the encoder's in their place) the v3 head refuses or differs.
            assert!(
                heads
                    .token_logits(
                        &TokenStates {
                            final_norm: &fin,
                            span_tower: None
                        },
                        &words
                    )
                    .is_err()
            );
            let wrong = heads
                .token_logits(
                    &TokenStates {
                        final_norm: &tower,
                        span_tower: Some(&fin),
                    },
                    &words,
                )
                .unwrap()
                .unwrap();
            assert!(
                wrong
                    .iter()
                    .zip(&want)
                    .any(|(g, w)| (g.unwrap() - w).abs() > 1e-6)
            );
            // Heads v2 read the final-norm states only: tower states are refused.
            assert!(
                plain
                    .token_logits(
                        &TokenStates {
                            final_norm: &fin,
                            span_tower: Some(&tower)
                        },
                        &words
                    )
                    .is_err()
            );
            let v2_logits = plain
                .token_logits(
                    &TokenStates {
                        final_norm: &fin,
                        span_tower: None,
                    },
                    &words,
                )
                .unwrap()
                .unwrap();
            assert_eq!(
                v2_logits,
                plain.heads().token_logits(&fin, &words).unwrap().unwrap()
            );
            // Document outputs are the v2 heads' outputs: the tower never feeds them.
            let h = &fin[..n];
            let content = (0..want.len())
                .map(|i| i > 0 && i + 1 < want.len())
                .collect::<Vec<_>>();
            let (a, b) = (
                heads.predict(h, &fin, &content, &content).unwrap(),
                plain.predict(h, &fin, &content, &content).unwrap(),
            );
            assert_eq!(
                serde_json::to_value(&a.prediction).unwrap(),
                serde_json::to_value(&b.prediction).unwrap()
            );
            assert_eq!(
                serde_json::to_value(&a.raw).unwrap(),
                serde_json::to_value(&b.raw).unwrap()
            );
        }
        assert!(worst <= 1e-12, "golden token logits: {worst}");
    }

    #[test]
    fn heads_v1_and_v2_bytes_reach_the_pinned_parser_unchanged() {
        let golden: Value = serde_json::from_str(TOWER_GOLDEN).unwrap();
        let v2 = as_v2(&tower_artifact(&golden));
        let bytes = serde_json::to_vec_pretty(&v2).unwrap();
        let (ours, pinned) = (
            SpanTowerHeads::from_json(&bytes).unwrap(),
            FullAHeads::from_json(&bytes).unwrap(),
        );
        assert!(ours.has_token_head() && ours.span_spec().is_some() && ours.span_tower().is_none());
        let c = &golden["cases"][1];
        let fin = floats(&c["final_norm_states"]);
        let words = vec![true; fin.len() / ours.architecture.raw_text_dimension];
        assert_eq!(
            ours.token_logits(
                &TokenStates {
                    final_norm: &fin,
                    span_tower: None
                },
                &words
            )
            .unwrap(),
            pinned.token_logits(&fin, &words).unwrap()
        );
        // Heads v1: no token head, no emission spec, no tower.
        let mut v1 = v2.clone();
        v1["schema"] = json!(full_a::SCHEMA_V1);
        for k in ["token_head", "span_emission"] {
            v1.as_object_mut().unwrap().remove(k);
        }
        for k in ["first.weight", "first.bias", "second.weight", "second.bias"] {
            v1["weights"]
                .as_object_mut()
                .unwrap()
                .remove(&format!("token_head.{k}"));
        }
        let h1 = parse(&v1).unwrap();
        assert!(!h1.has_token_head() && h1.token_input().is_none() && h1.span_tower().is_none());
        let states = TokenStates {
            final_norm: &fin,
            span_tower: None,
        };
        assert!(h1.token_logits(&states, &words).unwrap().is_none());
        // A refusal of the pinned parser stays a refusal.
        let mut bad = v2.clone();
        bad["origin_order"] = json!(["model_only", "human_only", "mixed"]);
        assert!(
            parse(&bad).is_err()
                && FullAHeads::from_json(&serde_json::to_vec(&bad).unwrap()).is_err()
        );
    }

    #[test]
    fn only_heads_v3_declares_a_span_tower() {
        let golden: Value = serde_json::from_str(TOWER_GOLDEN).unwrap();
        let v3 = tower_artifact(&golden);
        assert!(parse(&v3).is_ok());
        let mut v = v3.clone();
        v.as_object_mut().unwrap().remove("span_tower");
        assert!(parse(&v).is_err());
        let mut v = v3.clone();
        v["span_tower"] = Value::Null;
        assert!(parse(&v).is_err());
        // v3 needs the tower input; v2 the final-norm input and no tower key, not even null.
        let mut v = v3.clone();
        v["token_head"]["input"] = json!(FINAL_NORM_INPUT);
        assert!(parse(&v).is_err());
        let mut v = v3.clone();
        v["schema"] = json!(full_a::SCHEMA);
        assert!(parse(&v).is_err());
        v["token_head"]["input"] = json!(FINAL_NORM_INPUT);
        assert!(parse(&v).is_err());
        v["span_tower"] = Value::Null;
        assert!(parse(&v).is_err());
        v.as_object_mut().unwrap().remove("span_tower");
        assert!(parse(&v).is_ok());
        let mut v = v3.clone();
        v["schema"] = json!(full_a::SCHEMA);
        v["token_head"]["input"] = json!(TOWER_INPUT);
        v.as_object_mut().unwrap().remove("span_tower");
        assert!(parse(&v).is_err());
        // A tower without a token head, or a declaration that does not copy the top layers, is refused.
        let mut v = v3.clone();
        v["token_head"] = Value::Null;
        v["span_emission"] = Value::Null;
        for k in ["first.weight", "first.bias", "second.weight", "second.bias"] {
            v["weights"]
                .as_object_mut()
                .unwrap()
                .remove(&format!("token_head.{k}"));
        }
        assert!(parse(&v).is_err());
        v.as_object_mut().unwrap().remove("token_head");
        assert!(parse(&v).is_err());
        let mut v = v3.clone();
        v["span_tower"]["layers"] = json!([17, 18, 19, 20]);
        assert!(parse(&v).is_err());
        let mut v = v3.clone();
        v["span_tower"]["attention"] = json!(["local", "local", "local", "global"]);
        assert!(parse(&v).is_err());
        // Unknown keys in the token head or the declaration, and the pinned parser's own checks, still refuse.
        let mut v = v3.clone();
        v["token_head"]["extra"] = json!(1);
        assert!(parse(&v).is_err());
        let mut v = v3.clone();
        v["span_tower"]["extra"] = json!(1);
        assert!(parse(&v).is_err());
        let mut v = v3.clone();
        v["extra"] = json!(1);
        assert!(parse(&v).is_err());
        let mut v = v3.clone();
        v["token_head"]["rule"] = json!("slop_ninja_token_head_v0");
        assert!(parse(&v).is_err());
        let mut v = v3.clone();
        v["weights"]["token_head.extra.weight"] = json!({"shape":[1],"values":[0.]});
        assert!(parse(&v).is_err());
        let mut v = v3.clone();
        v["span_emission"] = Value::Null;
        assert!(parse(&v).is_err());
        // The golden generator matches the Python generator on its exact float32 grid.
        let w = golden_values("span_tower.norm.weight", 4);
        assert!(
            w.iter()
                .all(|x| (*x as f32) as f64 == *x && (0.9375..1.0625).contains(x))
        );
    }

    #[test]
    fn heads_v3_members_keep_their_exact_bytes_and_repeated_keys_are_refused() {
        let golden: Value = serde_json::from_str(TOWER_GOLDEN).unwrap();
        let v3 = tower_artifact(&golden);
        let text = String::from_utf8(serde_json::to_vec(&v3).unwrap()).unwrap();
        // The exact-bytes view: the v3 heads and the same heads written as v2 give identical outputs.
        let (a, b) = (parse(&v3).unwrap(), parse(&as_v2(&v3)).unwrap());
        let c = &golden["cases"][2];
        let fin = floats(&c["final_norm_states"]);
        let n = a.architecture.raw_text_dimension;
        let words = vec![true; fin.len() / n];
        assert_eq!(
            a.heads().token_logits(&fin, &words).unwrap(),
            b.heads().token_logits(&fin, &words).unwrap()
        );
        // A repeated top-level key is refused, as the pinned parser refuses a repeated field.
        // (rfind: the top-level keys are written sorted, so the last match is the top-level member).
        for key in ["\"schema\":", "\"span_tower\":", "\"weights\":"] {
            let at = text.rfind(key).unwrap();
            let end = at + key.len();
            let value_end = {
                let mut d = serde_json::Deserializer::from_str(&text[end..]).into_iter::<Value>();
                d.next().unwrap().unwrap();
                end + d.byte_offset()
            };
            let doubled = format!(
                "{},{}{}",
                &text[..value_end],
                &text[at..value_end],
                &text[value_end..]
            );
            assert!(serde_json::from_str::<Value>(&doubled).is_ok(), "{key}");
            assert!(
                SpanTowerHeads::from_json(doubled.as_bytes()).is_err(),
                "{key}"
            );
        }
        // Non-object input and an oversized input are refused before any member is read.
        assert!(SpanTowerHeads::from_json(b"[1]").is_err());
        assert!(SpanTowerHeads::from_json(&vec![b' '; SIZE_LIMIT + 1]).is_err());
    }

    #[test]
    fn tower_declaration_copies_the_top_layers_with_their_attention() {
        let g: Value = serde_json::from_str(TOWER_GOLDEN).unwrap();
        let t = TowerSpec::from_json(&g["tower"]).unwrap();
        assert_eq!((t.encoder_layers, t.source_layer, t.depth()), (22, 18, 4));
        assert_eq!(t.local_attention_layers(), [false, true, true, false]);
        let names = t.tensor_names();
        assert_eq!(serde_json::to_value(&names).unwrap(), g["tower_tensors"]);
        assert_eq!(names.len(), 4 * TOWER_LAYER_TENSORS.len() + 1);
        assert_eq!(
            t.source_tensor("span_tower.norm.weight").unwrap(),
            "model.final_norm.weight"
        );
        assert_eq!(
            t.source_tensor("span_tower.layers.0.attn.Wqkv.weight")
                .unwrap(),
            "model.layers.18.attn.Wqkv.weight"
        );
        assert_eq!(
            t.source_tensor("span_tower.layers.3.mlp_norm.weight")
                .unwrap(),
            "model.layers.21.mlp_norm.weight"
        );
        for bad in [
            "span_tower.layers.4.attn.Wo.weight",
            "span_tower.layers.0.attn.Wq.weight",
            "model.final_norm.weight",
        ] {
            assert!(t.source_tensor(bad).is_err(), "{bad}");
        }
        let bad = |f: &dyn Fn(&mut Value)| {
            let mut v = g["tower"].clone();
            f(&mut v);
            TowerSpec::from_json(&v).is_err()
        };
        assert!(bad(&|v| v["rule"] = json!("slop_ninja_s2_span_tower_v0")));
        assert!(bad(&|v| v["layers"] = json!([18, 19, 20])));
        assert!(bad(&|v| v["source_layer"] = json!(17)));
        assert!(bad(&|v| {
            v["source_layer"] = json!(0);
            v["layers"] = json!((0..22).collect::<Vec<_>>());
        }));
        assert!(bad(
            &|v| v["attention"] = json!(["global", "global", "local", "global"])
        ));
        assert!(bad(&|v| v["global_attn_every_n_layers"] = json!(2)));
        assert!(bad(&|v| v["capture"] = json!("final_norm_token_states")));
        assert!(bad(&|v| v["weights"] = json!("encoder.safetensors")));
        assert!(bad(&|v| v["extra"] = json!(1)));
        // Another depth is the same rule: a two-layer tower copies layers 20 and 21.
        let mut v = g["tower"].clone();
        v["source_layer"] = json!(20);
        v["layers"] = json!([20, 21]);
        v["attention"] = json!(["local", "global"]);
        assert_eq!(TowerSpec::from_json(&v).unwrap().tensor_names().len(), 13);
    }

    #[test]
    fn token_head_from_json_takes_exactly_the_token_head_tensors() {
        let (w1, b1) = linear(3, 4, 1., 41);
        let (w2, b2) = linear(1, 3, 1., 42);
        let mut weights = json!({"token_head.first.weight":w1,"token_head.first.bias":b1,
            "token_head.second.weight":w2,"token_head.second.bias":b2});
        let head = token_head_from_json(&weights, 3, 4).unwrap();
        assert_eq!(head.hidden, 3);
        assert!(token_head_from_json(&weights, 3, 5).is_err());
        weights["token_head.extra.weight"] = json!({"shape":[1],"values":[0.]});
        assert!(token_head_from_json(&weights, 3, 4).is_err());
    }
}
