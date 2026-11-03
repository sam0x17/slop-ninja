//! Portable full-A heads (`slop_ninja_full_a_heads_v2`): the stage-3 shared
//! residual heads on the V6 classifier input h, an optional token-span
//! involvement head over the final-norm token states (E2), an optional
//! spans-v1 token head with its span emission spec (`full_a_spans`), and a
//! frozen CAL map. No filesystem, tokenizer, encoder or platform-specific
//! operator; f64 math. The spans-v1 token head never feeds the document heads.
//! Heads v1 (`slop_ninja_full_a_heads_v1`, the R0 release package) is read
//! unchanged: it has no `token_head` or `span_emission` key, and its document
//! heads are the same operators.
//!
//! Span operator (per document, content = non-special token positions):
//!   t_i = w_t . x_i + b_t                  token involvement logit
//!   attention: a_i = w_a . x_i + b_a, pooled = sum_i softmax_content(a)_i t_i
//!   max:       pooled = max_content t_i
//!   mean       = (1/|S|) sum_S sigmoid(t_i), S = content for share mode
//!                `token_mean_projected` (v1, legacy) and S = word tokens
//!                (content positions covering a non-whitespace character,
//!                `slop_ninja_word_token_mask_v1`) for `word_token_mean_projected` (v2)
//! Origin: l = origin(z)/base_temperature, lp = log_softmax(l),
//!   g = logsumexp(lp1, lp2) - lp0 (+ gate * pooled with a span head),
//!   p = CAL(g), r = softmax(l1, l2)[0], origin = [1-p, p r, p (1-r)].
//! Share: head mode clamp(o1 + o2 sigmoid(s), o1, p); token modes clamp(mean, o1, p).
use crate::full_a_spans::{self, Emission, Spec, TokenHead};
use crate::involvement_calibration::Map;
use crate::shared::{Linear, RolePrediction, SharedPrediction, Tensor, sigmoid, softmax};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "slop_ninja_full_a_heads_v2";
/// The pre-spans heads schema, read for replay of v2 exports (R0).
pub const SCHEMA_V1: &str = "slop_ninja_full_a_heads_v1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Architecture {
    pub dimension: usize,
    pub raw_text_dimension: usize,
    pub hidden: usize,
    pub embedding: usize,
    pub human_profiles: usize,
    pub ai_profiles: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpanSpec {
    pooling: String,
    gate: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenHeadSpec {
    hidden: usize,
    activation: String,
    rule: String,
    input: String,
    label_policy: String,
    feeds_document_heads: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    schema: String,
    architecture: Architecture,
    span_head: Option<SpanSpec>,
    token_head: Option<TokenHeadSpec>,
    span_emission: Option<Value>,
    share_mode: String,
    contract_sha256: String,
    initialization_sha256: String,
    base_temperature: f64,
    text_mean: Vec<f64>,
    text_scale: Vec<f64>,
    profiles: BTreeMap<String, Vec<String>>,
    thresholds: BTreeMap<String, f64>,
    involvement_calibration: Value,
    supervision: BTreeMap<String, usize>,
    origin_order: Vec<String>,
    weights: BTreeMap<String, Tensor>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pooling {
    Attention,
    Max,
}

struct Span {
    pooling: Pooling,
    gate: f64,
    token: Linear,
    attention: Option<Linear>,
}

/// Raw, uncalibrated quantities retained for replay comparison.
#[derive(Debug, Serialize)]
pub struct RawOutputs {
    pub origin_logits: Vec<f64>,
    pub involvement_logit: f64,
    pub conditional_mixed_share: f64,
    pub token_mean: Option<f64>,
    pub pooled_token_logit: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct FullPrediction {
    pub prediction: SharedPrediction,
    pub raw: RawOutputs,
}

pub struct FullAHeads {
    pub architecture: Architecture,
    pub contract_sha256: String,
    pub initialization_sha256: String,
    pub supervision: BTreeMap<String, usize>,
    pub calibration: Map,
    pub token_share: bool,
    /// Share mean over word tokens only (v2); otherwise all content tokens.
    pub word_share: bool,
    base_temperature: f64,
    text_mean: Vec<f64>,
    text_scale: Vec<f64>,
    profiles: BTreeMap<String, Vec<String>>,
    thresholds: BTreeMap<String, f64>,
    first: Linear,
    residual: Linear,
    origin: Linear,
    share: Linear,
    human: Linear,
    ai: Linear,
    span: Option<Span>,
    token_head: Option<TokenHead>,
    span_spec: Option<Spec>,
}

fn log_softmax(x: &[f64]) -> Vec<f64> {
    let m = x.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let s = x.iter().map(|v| (v - m).exp()).sum::<f64>().ln() + m;
    x.iter().map(|v| v - s).collect()
}

fn logsumexp2(a: f64, b: f64) -> f64 {
    let m = a.max(b);
    m + ((a - m).exp() + (b - m).exp()).ln()
}

impl FullAHeads {
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= 64 * 1024 * 1024,
            "artifact exceeds research size limit"
        );
        let mut a: Artifact = serde_json::from_slice(bytes)?;
        match a.schema.as_str() {
            SCHEMA => {}
            SCHEMA_V1 => {
                // v1 predates the spans-v1 token head: both keys are absent, not null.
                let keys: BTreeMap<String, serde::de::IgnoredAny> = serde_json::from_slice(bytes)?;
                ensure!(
                    !keys.contains_key("token_head") && !keys.contains_key("span_emission"),
                    "full-A heads v1 has no token head or span emission spec"
                );
            }
            _ => anyhow::bail!("unsupported full-A heads artifact"),
        }
        ensure!(
            a.origin_order == ["human_only", "model_only", "mixed"],
            "origin order"
        );
        for hash in [&a.contract_sha256, &a.initialization_sha256] {
            ensure!(
                hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid digest"
            );
        }
        ensure!(
            a.base_temperature.is_finite() && a.base_temperature > 0.,
            "invalid base temperature"
        );
        let d = &a.architecture;
        ensure!(
            (1..=32768).contains(&d.dimension)
                && (1..=512).contains(&d.hidden)
                && (1..=4096).contains(&d.raw_text_dimension)
                && d.embedding == d.raw_text_dimension
                && d.raw_text_dimension <= d.dimension
                && (d.human_profiles == 0 || (2..=4096).contains(&d.human_profiles))
                && (2..=4096).contains(&d.ai_profiles),
            "architecture bounds"
        );
        ensure!(
            a.text_mean.len() == d.raw_text_dimension
                && a.text_scale.len() == d.raw_text_dimension
                && a.text_mean.iter().all(|v| v.is_finite())
                && a.text_scale.iter().all(|v| v.is_finite() && *v > 0.),
            "invalid frozen normalization"
        );
        ensure!(
            a.profiles.len() == 2 && a.thresholds.len() == 2,
            "unexpected role"
        );
        for (role, count) in [("human", d.human_profiles), ("ai", d.ai_profiles)] {
            let names = a.profiles.get(role).context("role profiles absent")?;
            ensure!(
                names.len() == count
                    && names
                        .iter()
                        .all(|s| !s.is_empty() && !s.starts_with("other_"))
                    && names.iter().collect::<BTreeSet<_>>().len() == names.len(),
                "invalid profile roster"
            );
            ensure!(
                (0.0..=1.0).contains(a.thresholds.get(role).context("role threshold absent")?),
                "invalid threshold"
            );
        }
        let (token_share, word_share) = match a.share_mode.as_str() {
            "head" => (false, false),
            "token_mean_projected" => (true, false),
            "word_token_mean_projected" => (true, true),
            _ => anyhow::bail!("unsupported share mode"),
        };
        ensure!(
            !token_share || a.span_head.is_some(),
            "token share needs a span head"
        );
        let calibration = Map::from_description(&a.involvement_calibration)?;
        let n = d.raw_text_dimension;
        let first = Linear::take(&mut a.weights, "residual.0", d.hidden, d.dimension)?;
        let residual = Linear::take(&mut a.weights, "residual.2", d.embedding, d.hidden)?;
        let origin = Linear::take(&mut a.weights, "origin", 3, d.embedding)?;
        let share = Linear::take(&mut a.weights, "mixed_share", 1, d.embedding)?;
        let human = Linear::take(&mut a.weights, "human", d.human_profiles, d.embedding)?;
        let ai = Linear::take(&mut a.weights, "ai", d.ai_profiles, d.embedding)?;
        let span = match a.span_head {
            None => None,
            Some(s) => {
                ensure!(s.gate.is_finite(), "nonfinite gate");
                let pooling = match s.pooling.as_str() {
                    "attention" => Pooling::Attention,
                    "max" => Pooling::Max,
                    _ => anyhow::bail!("unsupported pooling"),
                };
                let token = Linear::take(&mut a.weights, "span.token", 1, n)?;
                let attention = if pooling == Pooling::Attention {
                    Some(Linear::take(&mut a.weights, "span.attention", 1, n)?)
                } else {
                    None
                };
                Some(Span {
                    pooling,
                    gate: s.gate,
                    token,
                    attention,
                })
            }
        };
        ensure!(
            a.token_head.is_some() == a.span_emission.is_some(),
            "span emission spec iff token head"
        );
        let token_head = match &a.token_head {
            None => None,
            Some(t) => {
                ensure!(
                    t.activation == "gelu_tanh"
                        && t.rule == full_a_spans::TOKEN_HEAD_RULE
                        && t.input == "final_norm_token_states"
                        && t.label_policy == full_a_spans::LABEL_POLICY
                        && !t.feeds_document_heads,
                    "token head meaning differs"
                );
                Some(TokenHead::take(&mut a.weights, t.hidden, n)?)
            }
        };
        let span_spec = a.span_emission.as_ref().map(Spec::from_json).transpose()?;
        ensure!(a.weights.is_empty(), "unrecognized weights");
        Ok(Self {
            architecture: a.architecture,
            contract_sha256: a.contract_sha256,
            initialization_sha256: a.initialization_sha256,
            supervision: a.supervision,
            calibration,
            token_share,
            word_share,
            base_temperature: a.base_temperature,
            text_mean: a.text_mean,
            text_scale: a.text_scale,
            profiles: a.profiles,
            thresholds: a.thresholds,
            first,
            residual,
            origin,
            share,
            human,
            ai,
            span,
            token_head,
            span_spec,
        })
    }

    pub fn has_span_head(&self) -> bool {
        self.span.is_some()
    }

    pub fn has_token_head(&self) -> bool {
        self.token_head.is_some()
    }

    pub fn span_spec(&self) -> Option<&Spec> {
        self.span_spec.as_ref()
    }

    /// spans-v1 token logits at word-bearing positions (None elsewhere, and
    /// None overall without a token head).
    pub fn token_logits(&self, tokens: &[f64], words: &[bool]) -> Result<Option<Vec<Option<f64>>>> {
        let Some(t) = &self.token_head else {
            return Ok(None);
        };
        Ok(Some(t.logits(tokens, self.architecture.raw_text_dimension, words)?))
    }

    /// Sentences and spans for one request from its token logits and the
    /// emitted document outputs; `offsets` are the tokenizer's char offsets.
    pub fn emit_spans(
        &self,
        text: &str,
        offsets: &[(usize, usize)],
        words: &[bool],
        token_logits: &[Option<f64>],
        prediction: &SharedPrediction,
    ) -> Result<Emission> {
        let spec = self.span_spec.as_ref().context("no span emission spec")?;
        full_a_spans::emit(
            text,
            offsets,
            words,
            token_logits,
            prediction.ai_share_percent / 100.,
            prediction.ai_confidence_percent / 100.,
            spec,
        )
    }

    /// Shared encoding z = h + residual(normalized h, zero CFG); text mode only.
    pub fn encode(&self, h: &[f64]) -> Result<Vec<f64>> {
        let n = self.architecture.raw_text_dimension;
        ensure!(h.len() == n && h.iter().all(|v| v.is_finite()), "invalid h");
        let mut u = vec![0.; self.architecture.dimension];
        for (i, v) in h.iter().enumerate() {
            u[i] = (v - self.text_mean[i]) / self.text_scale[i];
        }
        ensure!(u.iter().all(|v| v.is_finite()), "normalization overflow");
        let hidden = self
            .first
            .apply(&u)
            .into_iter()
            .map(f64::tanh)
            .collect::<Vec<_>>();
        let z = self
            .residual
            .apply(&hidden)
            .iter()
            .zip(h)
            .map(|(r, h)| r + h)
            .collect::<Vec<_>>();
        ensure!(z.iter().all(|v| v.is_finite()), "invalid shared encoding");
        Ok(z)
    }

    /// Token-span operator on row-major `tokens` (count x raw_text_dimension).
    /// `words` marks word-bearing content tokens; it must be a nonempty subset
    /// of `content` and is used only by the v2 word-token share mean.
    pub fn span(
        &self,
        tokens: &[f64],
        content: &[bool],
        words: &[bool],
    ) -> Result<Option<(f64, f64)>> {
        let Some(s) = &self.span else {
            return Ok(None);
        };
        let n = self.architecture.raw_text_dimension;
        ensure!(
            !content.is_empty()
                && tokens.len() == content.len() * n
                && words.len() == content.len()
                && words.iter().zip(content).all(|(w, c)| !w || *c)
                && words.iter().any(|w| *w),
            "token state shape or word mask"
        );
        let mut logits = Vec::new();
        let mut scores = Vec::new();
        let mut share = Vec::new();
        for ((x, keep), word) in tokens.chunks_exact(n).zip(content).zip(words) {
            if !keep {
                continue;
            }
            ensure!(x.iter().all(|v| v.is_finite()), "nonfinite token state");
            let t = s.token.apply(x)[0];
            logits.push(t);
            if *word || !self.word_share {
                share.push(sigmoid(t));
            }
            if let Some(a) = &s.attention {
                scores.push(a.apply(x)[0]);
            }
        }
        ensure!(!logits.is_empty() && !share.is_empty(), "no content tokens");
        let pooled = match s.pooling {
            Pooling::Attention => softmax(&scores)?
                .iter()
                .zip(&logits)
                .map(|(w, t)| w * t)
                .sum::<f64>(),
            Pooling::Max => logits.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        };
        let mean = share.iter().sum::<f64>() / share.len() as f64;
        ensure!(
            pooled.is_finite() && mean.is_finite(),
            "invalid span outputs"
        );
        Ok(Some((pooled, mean)))
    }

    pub fn predict(
        &self,
        h: &[f64],
        tokens: &[f64],
        content: &[bool],
        words: &[bool],
    ) -> Result<FullPrediction> {
        let z = self.encode(h)?;
        let logits = self
            .origin
            .apply(&z)
            .into_iter()
            .map(|v| v / self.base_temperature)
            .collect::<Vec<_>>();
        ensure!(
            logits.iter().all(|v| v.is_finite()),
            "invalid origin logits"
        );
        let lp = log_softmax(&logits);
        let mut g = logsumexp2(lp[1], lp[2]) - lp[0];
        let span = self.span(tokens, content, words)?;
        if let (Some((pooled, _)), Some(s)) = (span, &self.span) {
            g += s.gate * pooled;
        }
        ensure!(g.is_finite(), "invalid involvement logit");
        // Protocol v1.2 maps scale the origin logits first; the others read g only.
        let (p, r) = if self.calibration.needs_origin() {
            self.calibration.apply_origin(&logits, g)?
        } else {
            (self.calibration.apply(g)?, softmax(&logits[1..])?[0])
        };
        let origin = [1. - p, p * r, p * (1. - r)];
        let share_logit = self.share.apply(&z)[0];
        ensure!(share_logit.is_finite(), "invalid share logit");
        let q = sigmoid(share_logit);
        let share = match (self.token_share, span) {
            (true, Some((_, mean))) => mean,
            _ => origin[1] + origin[2] * q,
        }
        .clamp(origin[1], p);
        let mut attribution = BTreeMap::new();
        for (role, involvement, head) in [
            ("human", (origin[0] + origin[2]).min(1.), &self.human),
            ("ai", p, &self.ai),
        ] {
            let names = &self.profiles[role];
            // An empty roster (the shared-contract human role) has no named
            // score and always returns the fallback.
            if names.is_empty() {
                attribution.insert(
                    role.into(),
                    RolePrediction {
                        profile: format!("other_{role}"),
                        involvement_probability: involvement,
                        conditional_named_scores: BTreeMap::new(),
                        fallback_asserts_presence: false,
                    },
                );
                continue;
            }
            let scores = softmax(&head.apply(&z))?;
            let best = (0..scores.len())
                .max_by(|a, b| scores[*a].total_cmp(&scores[*b]).then_with(|| b.cmp(a)))
                .context("empty head")?;
            let profile = if scores[best] >= self.thresholds[role] {
                names[best].clone()
            } else {
                format!("other_{role}")
            };
            attribution.insert(
                role.into(),
                RolePrediction {
                    profile,
                    involvement_probability: involvement,
                    conditional_named_scores: names.iter().cloned().zip(scores).collect(),
                    fallback_asserts_presence: false,
                },
            );
        }
        Ok(FullPrediction {
            prediction: SharedPrediction {
                origin: ["human_only", "model_only", "mixed"]
                    .into_iter()
                    .map(str::to_owned)
                    .zip(origin)
                    .collect(),
                ai_confidence_percent: p * 100.,
                ai_share_percent: share * 100.,
                attribution,
            },
            raw: RawOutputs {
                origin_logits: logits,
                involvement_logit: g,
                conditional_mixed_share: q,
                token_mean: span.map(|s| s.1),
                pooled_token_logit: span.map(|s| s.0),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    fn artifact(span: Option<(&str, f64)>, share_mode: &str, calibration: Value) -> Vec<u8> {
        let (n, dim, hidden) = (4, 6, 3);
        let mut weights = serde_json::Map::new();
        for (name, rows, cols, seed) in [
            ("residual.0", hidden, dim, 1),
            ("residual.2", n, hidden, 2),
            ("origin", 3, n, 3),
            ("mixed_share", 1, n, 4),
            ("human", 2, n, 5),
            ("ai", 2, n, 6),
            ("span.token", 1, n, 7),
            ("span.attention", 1, n, 8),
        ] {
            if name.starts_with("span.")
                && (span.is_none() || (name == "span.attention" && span.unwrap().0 != "attention"))
            {
                continue;
            }
            let (w, b) = linear(rows, cols, 2., seed);
            weights.insert(format!("{name}.weight"), w);
            weights.insert(format!("{name}.bias"), b);
        }
        serde_json::to_vec(&json!({"schema":SCHEMA,
            "architecture":{"dimension":dim,"raw_text_dimension":n,"hidden":hidden,"embedding":n,
                "human_profiles":2,"ai_profiles":2},
            "span_head":span.map(|(p, g)| json!({"pooling":p,"gate":g})),"token_head":null,"span_emission":null,
            "share_mode":share_mode,"contract_sha256":"a".repeat(64),"initialization_sha256":"b".repeat(64),
            "base_temperature":1.5,"text_mean":[0.1,0.,-0.1,0.2],"text_scale":[1.,2.,0.5,1.],
            "profiles":{"human":["h1","h2"],"ai":["a1","a2"]},"thresholds":{"human":0.6,"ai":1.0},
            "involvement_calibration":calibration,"supervision":{"origin":1},
            "origin_order":["human_only","model_only","mixed"],"weights":weights}))
        .unwrap()
    }

    const H: [f64; 4] = [0.3, -0.2, 0.5, 0.1];
    const TOKENS: [f64; 16] = [
        9., 9., 9., 9., 0.1, 0.2, -0.3, 0.4, -0.5, 0.6, 0.7, -0.8, 9., 9., 9., 9.,
    ];
    const CONTENT: [bool; 4] = [false, true, true, false];
    const WORDS: [bool; 4] = [false, true, false, false];

    #[test]
    fn empty_human_roster_returns_other_human_and_keeps_ai_scores() {
        let mut v: Value =
            serde_json::from_slice(&artifact(None, "head", json!({"method":"identity_v1"}))).unwrap();
        v["architecture"]["human_profiles"] = json!(0);
        v["profiles"]["human"] = json!([]);
        v["thresholds"]["human"] = json!(1.0);
        v["weights"]["human.weight"] = json!({"shape":[0, 4],"values":[]});
        v["weights"]["human.bias"] = json!({"shape":[0],"values":[]});
        let heads = FullAHeads::from_json(&serde_json::to_vec(&v).unwrap()).unwrap();
        let out = heads.predict(&H, &TOKENS, &CONTENT, &CONTENT).unwrap();
        let human = &out.prediction.attribution["human"];
        assert_eq!(human.profile, "other_human");
        assert!(human.conditional_named_scores.is_empty() && !human.fallback_asserts_presence);
        let o = &out.prediction.origin;
        assert!((human.involvement_probability - (o["human_only"] + o["mixed"])).abs() < 1e-15);
        let ai = &out.prediction.attribution["ai"];
        assert_eq!(ai.conditional_named_scores.len(), 2);
        assert!((ai.conditional_named_scores.values().sum::<f64>() - 1.).abs() < 1e-12);
        // One named human profile is still refused.
        v["architecture"]["human_profiles"] = json!(1);
        v["profiles"]["human"] = json!(["h1"]);
        assert!(FullAHeads::from_json(&serde_json::to_vec(&v).unwrap()).is_err());
    }

    #[test]
    fn zero_gate_and_identity_reproduce_plain_origin_softmax() {
        for span in [None, Some(("attention", 0.)), Some(("max", 0.))] {
            let heads =
                FullAHeads::from_json(&artifact(span, "head", json!({"method":"identity_v1"})))
                    .unwrap();
            let out = heads.predict(&H, &TOKENS, &CONTENT, &CONTENT).unwrap();
            let p = softmax(&out.raw.origin_logits).unwrap();
            for (k, v) in ["human_only", "model_only", "mixed"].iter().zip(&p) {
                assert!((out.prediction.origin[*k] - v).abs() < 1e-14);
            }
            assert!((sigmoid(out.raw.involvement_logit) - (p[1] + p[2])).abs() < 1e-14);
            assert_eq!(out.raw.token_mean.is_some(), span.is_some());
        }
    }

    #[test]
    fn span_pooling_ignores_special_positions_and_moves_involvement() {
        let a = FullAHeads::from_json(&artifact(
            Some(("attention", 0.7)),
            "token_mean_projected",
            json!({"method":"identity_v1"}),
        ))
        .unwrap();
        let out = a.predict(&H, &TOKENS, &CONTENT, &CONTENT).unwrap();
        // Changing masked (special) token states cannot change any output.
        let mut other = TOKENS;
        other[0] = -40.;
        other[15] = 17.;
        let same = a.predict(&H, &other, &CONTENT, &CONTENT).unwrap();
        assert_eq!(out.raw.involvement_logit, same.raw.involvement_logit);
        assert_eq!(out.raw.token_mean, same.raw.token_mean);
        let zero = FullAHeads::from_json(&artifact(
            Some(("attention", 0.)),
            "token_mean_projected",
            json!({"method":"identity_v1"}),
        ))
        .unwrap()
        .predict(&H, &TOKENS, &CONTENT, &CONTENT)
        .unwrap();
        let pooled = out.raw.pooled_token_logit.unwrap();
        assert!(
            (out.raw.involvement_logit - zero.raw.involvement_logit - 0.7 * pooled).abs() < 1e-12
        );
        let o = &out.prediction.origin;
        let p = out.prediction.ai_confidence_percent / 100.;
        let s = out.prediction.ai_share_percent / 100.;
        assert!(o["model_only"] <= s + 1e-15 && s <= p + 1e-15);
        assert!((o["model_only"] + o["mixed"] - p).abs() < 1e-15);
        let max = FullAHeads::from_json(&artifact(
            Some(("max", 1.)),
            "head",
            json!({"method":"identity_v1"}),
        ))
        .unwrap()
        .predict(&H, &TOKENS, &CONTENT, &CONTENT)
        .unwrap();
        // Same token head (seed): a max is never below an attention-weighted mean.
        assert!(max.raw.pooled_token_logit.unwrap() >= pooled - 1e-12);
    }

    #[test]
    fn calibration_changes_involvement_but_keeps_the_ratio_and_share_bounds() {
        let plain = FullAHeads::from_json(&artifact(None, "head", json!({"method":"identity_v1"})))
            .unwrap()
            .predict(&H, &TOKENS, &CONTENT, &CONTENT)
            .unwrap();
        let hot = FullAHeads::from_json(&artifact(
            None,
            "head",
            json!({"method":"involvement_temperature_v1","temperature":(-2.0f64).exp()}),
        ))
        .unwrap()
        .predict(&H, &TOKENS, &CONTENT, &CONTENT)
        .unwrap();
        let ratio = |o: &BTreeMap<String, f64>| o["model_only"] / o["mixed"];
        assert!((ratio(&plain.prediction.origin) - ratio(&hot.prediction.origin)).abs() < 1e-12);
        assert_eq!(plain.raw.involvement_logit, hot.raw.involvement_logit);
        let p = hot.prediction.ai_confidence_percent;
        assert!(hot.prediction.ai_share_percent <= p + 1e-12);
        assert!(
            hot.prediction.origin["model_only"] * 100. <= hot.prediction.ai_share_percent + 1e-12
        );
    }

    #[test]
    fn rejects_inconsistent_artifacts() {
        let ok = artifact(Some(("max", 0.)), "head", json!({"method":"identity_v1"}));
        assert!(FullAHeads::from_json(&ok).is_ok());
        assert!(
            FullAHeads::from_json(&artifact(
                None,
                "token_mean_projected",
                json!({"method":"identity_v1"})
            ))
            .is_err()
        );
        assert!(FullAHeads::from_json(&artifact(None, "head", json!({"method":"platt"}))).is_err());
        let mut v: Value = serde_json::from_slice(&ok).unwrap();
        v["weights"]["span.attention.weight"] = json!({"shape":[1,4],"values":[0.,0.,0.,0.]});
        v["weights"]["span.attention.bias"] = json!({"shape":[1],"values":[0.]});
        assert!(FullAHeads::from_json(&serde_json::to_vec(&v).unwrap()).is_err());
        let heads = FullAHeads::from_json(&ok).unwrap();
        assert!(heads.predict(&H, &TOKENS, &[false; 4], &[false; 4]).is_err());
        assert!(heads.predict(&H, &TOKENS[..12], &CONTENT, &CONTENT).is_err());
        // Word tokens must be a nonempty subset of content.
        assert!(heads.predict(&H, &TOKENS, &CONTENT, &[true, false, false, false]).is_err());
        assert!(heads.predict(&H, &TOKENS, &CONTENT, &[false; 4]).is_err());
    }

    fn with_token_head(bytes: &[u8], hidden: usize) -> Vec<u8> {
        let mut v: Value = serde_json::from_slice(bytes).unwrap();
        let (w1, b1) = linear(hidden, 4, 1.5, 21);
        let (w2, b2) = linear(1, hidden, 1.5, 22);
        for (k, t) in [("first.weight", w1), ("first.bias", b1), ("second.weight", w2), ("second.bias", b2)] {
            v["weights"][format!("token_head.{k}")] = t;
        }
        v["token_head"] = json!({"hidden":hidden,"activation":"gelu_tanh","rule":full_a_spans::TOKEN_HEAD_RULE,
            "input":"final_norm_token_states","label_policy":full_a_spans::LABEL_POLICY,"feeds_document_heads":false});
        v["span_emission"] = json!({"rule":full_a_spans::EMISSION_RULE,"word_rule":full_a_spans::WORD_RULE,
            "word_probability_rule":full_a_spans::WORD_PROBABILITY_RULE,"tilt_rule":full_a_spans::TILT_RULE,
            "splitter":full_a_spans::SPLITTER_ID,
            "decoder":{"rule":full_a_spans::DECODER_RULE,"tau_hi":0.5,"delta":0.0,"min_words":1,"gap":0,"snap":null},
            "sentence_confidence":{"method":"identity_v1"},"span_confidence":{"method":"identity_v1"}});
        serde_json::to_vec(&v).unwrap()
    }

    #[test]
    fn token_head_leaves_document_outputs_and_emits_consistent_spans() {
        let plain = artifact(None, "head", json!({"method":"identity_v1"}));
        let a = FullAHeads::from_json(&plain).unwrap();
        let b = FullAHeads::from_json(&with_token_head(&plain, 5)).unwrap();
        assert!(!a.has_token_head() && b.has_token_head());
        let pa = a.predict(&H, &TOKENS, &CONTENT, &CONTENT).unwrap();
        let pb = b.predict(&H, &TOKENS, &CONTENT, &CONTENT).unwrap();
        assert_eq!(serde_json::to_value(&pa.prediction).unwrap(), serde_json::to_value(&pb.prediction).unwrap());
        assert!(a.token_logits(&TOKENS, &CONTENT).unwrap().is_none());
        let t = b.token_logits(&TOKENS, &CONTENT).unwrap().unwrap();
        assert!(t[0].is_none() && t[1].is_some() && t[2].is_some() && t[3].is_none());
        // Two words, one word-bearing token each (synthetic char offsets).
        let text = "Ab cd.";
        let e = b
            .emit_spans(text, &[(0, 0), (0, 2), (2, 6), (0, 0)], &CONTENT, &t, &pb.prediction)
            .unwrap();
        assert_eq!(e.words, 2);
        assert_eq!(e.sentences.len(), 1);
        let s = pb.prediction.ai_share_percent;
        assert!((e.sentences[0].ai_share_percent - s.clamp(1e-4, 100. - 1e-4)).abs() < 1e-9);
        assert!(e.sentences[0].ai_confidence_percent <= pb.prediction.ai_confidence_percent + 1e-12);
        // A token head without its emission spec (or the reverse) is refused.
        let mut v: Value = serde_json::from_slice(&with_token_head(&plain, 5)).unwrap();
        v["span_emission"] = Value::Null;
        assert!(FullAHeads::from_json(&serde_json::to_vec(&v).unwrap()).is_err());
        let mut v: Value = serde_json::from_slice(&with_token_head(&plain, 5)).unwrap();
        v["token_head"]["feeds_document_heads"] = json!(true);
        assert!(FullAHeads::from_json(&serde_json::to_vec(&v).unwrap()).is_err());
    }

    #[test]
    fn heads_v1_is_read_with_the_same_document_outputs_and_no_token_head() {
        for span in [None, Some(("attention", 0.4)), Some(("max", -0.2))] {
            let v2 = artifact(span, "head", json!({"method":"identity_v1"}));
            let mut v1: Value = serde_json::from_slice(&v2).unwrap();
            v1["schema"] = json!(SCHEMA_V1);
            let map = v1.as_object_mut().unwrap();
            map.remove("token_head");
            map.remove("span_emission");
            let v1_bytes = serde_json::to_vec(&v1).unwrap();
            let (a, b) = (FullAHeads::from_json(&v1_bytes).unwrap(), FullAHeads::from_json(&v2).unwrap());
            assert!(!a.has_token_head() && a.span_spec().is_none());
            let (pa, pb) = (
                a.predict(&H, &TOKENS, &CONTENT, &WORDS).unwrap(),
                b.predict(&H, &TOKENS, &CONTENT, &WORDS).unwrap(),
            );
            assert_eq!(serde_json::to_value(&pa.prediction).unwrap(), serde_json::to_value(&pb.prediction).unwrap());
            assert_eq!(serde_json::to_value(&pa.raw).unwrap(), serde_json::to_value(&pb.raw).unwrap());
            // v1 never carries the spans-v1 keys, not even as null.
            for key in ["token_head", "span_emission"] {
                let mut w = v1.clone();
                w[key] = Value::Null;
                assert!(FullAHeads::from_json(&serde_json::to_vec(&w).unwrap()).is_err());
            }
            let with_head: Value = serde_json::from_slice(&with_token_head(&v2, 5)).unwrap();
            let mut w = with_head;
            w["schema"] = json!(SCHEMA_V1);
            assert!(FullAHeads::from_json(&serde_json::to_vec(&w).unwrap()).is_err());
            let mut w = v1.clone();
            w["schema"] = json!("slop_ninja_full_a_heads_v0");
            assert!(FullAHeads::from_json(&serde_json::to_vec(&w).unwrap()).is_err());
        }
    }

    #[test]
    fn word_token_share_averages_only_word_tokens() {
        let v1 = FullAHeads::from_json(&artifact(
            Some(("attention", 0.3)),
            "token_mean_projected",
            json!({"method":"identity_v1"}),
        ))
        .unwrap();
        let v2 = FullAHeads::from_json(&artifact(
            Some(("attention", 0.3)),
            "word_token_mean_projected",
            json!({"method":"identity_v1"}),
        ))
        .unwrap();
        assert!(v2.word_share && !v1.word_share);
        let a = v1.predict(&H, &TOKENS, &CONTENT, &WORDS).unwrap();
        let b = v2.predict(&H, &TOKENS, &CONTENT, &WORDS).unwrap();
        // Pooling (hence involvement) ignores the word mask; only the share mean changes.
        assert_eq!(a.raw.involvement_logit, b.raw.involvement_logit);
        let s = v2.span.as_ref().unwrap();
        let t1 = s.token.apply(&TOKENS[4..8])[0];
        let t2 = s.token.apply(&TOKENS[8..12])[0];
        assert!((a.raw.token_mean.unwrap() - (sigmoid(t1) + sigmoid(t2)) / 2.).abs() < 1e-15);
        assert!((b.raw.token_mean.unwrap() - sigmoid(t1)).abs() < 1e-15);
        // v1 ignores the word mask entirely.
        let c = v1.predict(&H, &TOKENS, &CONTENT, &CONTENT).unwrap();
        assert_eq!(a.raw.token_mean, c.raw.token_mean);
    }
}
