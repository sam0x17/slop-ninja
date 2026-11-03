//! Portable numeric inference for the shared A research artifact.
//! No filesystem, Python, CUDA, network, raw prose, or platform-specific operator.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Architecture {
    pub dimension: usize,
    pub human_profiles: usize,
    pub ai_profiles: usize,
    pub hidden: usize,
    pub embedding: usize,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Tensor {
    shape: Vec<usize>,
    values: Vec<f64>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    schema: String,
    architecture: Architecture,
    operations: Vec<String>,
    contract_sha256: String,
    temperature: f64,
    profiles: BTreeMap<String, Vec<String>>,
    thresholds: BTreeMap<String, f64>,
    supervision: BTreeMap<String, usize>,
    origin_order: Vec<String>,
    control: String,
    weights: BTreeMap<String, Tensor>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SharedInput {
    pub contract_sha256: String,
    pub x: Vec<f64>,
}
#[derive(Debug, Serialize)]
pub struct RolePrediction {
    pub profile: String,
    pub involvement_probability: f64,
    pub conditional_named_scores: BTreeMap<String, f64>,
    pub fallback_asserts_presence: bool,
}
#[derive(Debug, Serialize)]
pub struct SharedPrediction {
    pub origin: BTreeMap<String, f64>,
    pub ai_confidence_percent: f64,
    pub ai_share_percent: f64,
    pub attribution: BTreeMap<String, RolePrediction>,
}
pub(crate) struct Linear {
    width: usize,
    weights: Vec<f64>,
    bias: Vec<f64>,
}
impl Linear {
    pub(crate) fn take(
        weights: &mut BTreeMap<String, Tensor>,
        prefix: &str,
        rows: usize,
        cols: usize,
    ) -> Result<Self> {
        let w = weights
            .remove(&format!("{prefix}.weight"))
            .context("missing weight matrix")?;
        let b = weights
            .remove(&format!("{prefix}.bias"))
            .context("missing bias")?;
        ensure!(
            w.shape == [rows, cols]
                && w.values.len() == rows * cols
                && b.shape == [rows]
                && b.values.len() == rows,
            "weight shape mismatch for {prefix}"
        );
        ensure!(
            w.values.iter().chain(&b.values).all(|v| v.is_finite()),
            "nonfinite weights"
        );
        Ok(Self {
            width: cols,
            weights: w.values,
            bias: b.values,
        })
    }
    pub(crate) fn apply(&self, x: &[f64]) -> Vec<f64> {
        self.weights
            .chunks_exact(self.width)
            .zip(&self.bias)
            .map(|(row, b)| row.iter().zip(x).map(|(w, v)| w * v).sum::<f64>() + b)
            .collect()
    }
    pub(crate) fn numeric_bytes(&self) -> usize {
        (self.weights.len() + self.bias.len()) * std::mem::size_of::<f64>()
    }
}
pub(crate) fn softmax(x: &[f64]) -> Result<Vec<f64>> {
    ensure!(
        !x.is_empty() && x.iter().all(|v| v.is_finite()),
        "invalid logits"
    );
    let max = x.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut p = x.iter().map(|v| (v - max).exp()).collect::<Vec<_>>();
    let sum = p.iter().sum::<f64>();
    for v in &mut p {
        *v /= sum;
    }
    Ok(p)
}
pub(crate) fn sigmoid(x: f64) -> f64 {
    if x >= 0. {
        1. / (1. + (-x).exp())
    } else {
        let e = x.exp();
        e / (1. + e)
    }
}

pub struct SharedModel {
    pub architecture: Architecture,
    pub contract_sha256: String,
    pub supervision: BTreeMap<String, usize>,
    pub control: String,
    temperature: f64,
    profiles: BTreeMap<String, Vec<String>>,
    thresholds: BTreeMap<String, f64>,
    first: Linear,
    second: Linear,
    origin: Linear,
    share: Linear,
    human: Linear,
    ai: Linear,
}
impl SharedModel {
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= 64 * 1024 * 1024,
            "artifact exceeds research size limit"
        );
        let mut a: Artifact = serde_json::from_slice(bytes)?;
        ensure!(
            a.schema == "slop_ninja_shared_a_weights_v1",
            "unsupported artifact"
        );
        ensure!(
            a.operations == ["linear", "tanh", "softmax", "sigmoid"]
                && a.origin_order == ["human_only", "model_only", "mixed"],
            "unsupported operator/order contract"
        );
        ensure!(
            a.contract_sha256.len() == 64
                && a.contract_sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "contract digest"
        );
        ensure!(
            a.temperature.is_finite() && a.temperature > 0.,
            "temperature"
        );
        let d = &a.architecture;
        ensure!(
            (1..=32768).contains(&d.dimension)
                && (1..=512).contains(&d.hidden)
                && (1..=512).contains(&d.embedding)
                && d.human_profiles <= 4096
                && d.ai_profiles <= 4096,
            "architecture bounds"
        );
        for (role, count) in [("human", d.human_profiles), ("ai", d.ai_profiles)] {
            let names = a.profiles.get(role).context("role profiles absent")?;
            ensure!(
                names.len() == count
                    && names
                        .iter()
                        .all(|s| !s.is_empty() && !s.starts_with("other_"))
                    && names
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == names.len(),
                "invalid profile roster"
            );
            let threshold = a.thresholds.get(role).context("role threshold absent")?;
            ensure!((0.0..=1.0).contains(threshold), "invalid role threshold");
        }
        ensure!(
            a.profiles.len() == 2 && a.thresholds.len() == 2,
            "unexpected role"
        );
        let first = Linear::take(&mut a.weights, "encoder.0", d.hidden, d.dimension)?;
        let second = Linear::take(&mut a.weights, "encoder.2", d.embedding, d.hidden)?;
        let origin = Linear::take(&mut a.weights, "origin", 3, d.embedding)?;
        let share = Linear::take(&mut a.weights, "mixed_share", 1, d.embedding)?;
        let human = Linear::take(
            &mut a.weights,
            "human",
            d.human_profiles.max(1),
            d.embedding,
        )?;
        let ai = Linear::take(&mut a.weights, "ai", d.ai_profiles.max(1), d.embedding)?;
        ensure!(a.weights.is_empty(), "unrecognized weights");
        Ok(Self {
            architecture: a.architecture,
            contract_sha256: a.contract_sha256,
            supervision: a.supervision,
            control: a.control,
            temperature: a.temperature,
            profiles: a.profiles,
            thresholds: a.thresholds,
            first,
            second,
            origin,
            share,
            human,
            ai,
        })
    }
    /// Frozen shared encoder only; validates the same contract and dimensions as inference.
    pub fn encode(&self, input: &SharedInput) -> Result<Vec<f64>> {
        ensure!(
            input.contract_sha256 == self.contract_sha256,
            "different feature contract"
        );
        ensure!(
            input.x.len() == self.architecture.dimension && input.x.iter().all(|v| v.is_finite()),
            "invalid numeric input"
        );
        let hidden = self
            .first
            .apply(&input.x)
            .into_iter()
            .map(f64::tanh)
            .collect::<Vec<_>>();
        Ok(self
            .second
            .apply(&hidden)
            .into_iter()
            .map(f64::tanh)
            .collect::<Vec<_>>())
    }
    pub fn predict(&self, input: &SharedInput) -> Result<SharedPrediction> {
        let encoding = self.encode(input)?;
        let logits = self
            .origin
            .apply(&encoding)
            .into_iter()
            .map(|v| v / self.temperature)
            .collect::<Vec<_>>();
        let p = softmax(&logits)?;
        let ai_probability = (p[1] + p[2]).min(1.);
        let share =
            (p[1] + p[2] * sigmoid(self.share.apply(&encoding)[0])).clamp(p[1], ai_probability);
        let mut attribution = BTreeMap::new();
        for (role, involvement, head) in [
            ("human", (p[0] + p[2]).min(1.), &self.human),
            ("ai", ai_probability, &self.ai),
        ] {
            let names = &self.profiles[role];
            let scores = softmax(&head.apply(&encoding))?;
            let best = (0..scores.len())
                .max_by(|a, b| scores[*a].total_cmp(&scores[*b]).then_with(|| b.cmp(a)))
                .context("empty head")?;
            let profile = if !names.is_empty() && scores[best] >= self.thresholds[role] {
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
        Ok(SharedPrediction {
            origin: ["human_only", "model_only", "mixed"]
                .into_iter()
                .map(str::to_owned)
                .zip(p)
                .collect(),
            ai_confidence_percent: ai_probability * 100.,
            ai_share_percent: share * 100.,
            attribution,
        })
    }
    pub fn numeric_weight_bytes(&self) -> usize {
        [
            &self.first,
            &self.second,
            &self.origin,
            &self.share,
            &self.human,
            &self.ai,
        ]
        .iter()
        .map(|l| (l.weights.len() + l.bias.len()) * std::mem::size_of::<f64>())
        .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frozen_encoder_matches_known_arithmetic_and_prediction() {
        let model = SharedModel {
            architecture: Architecture {
                dimension: 2,
                hidden: 2,
                embedding: 2,
                human_profiles: 0,
                ai_profiles: 0,
            },
            contract_sha256: "a".repeat(64),
            supervision: BTreeMap::new(),
            control: "joint".into(),
            temperature: 1.,
            profiles: BTreeMap::from([("human".into(), vec![]), ("ai".into(), vec![])]),
            thresholds: BTreeMap::from([("human".into(), 1.), ("ai".into(), 1.)]),
            first: Linear {
                width: 2,
                weights: vec![1., 0., 0., 2.],
                bias: vec![0., 0.],
            },
            second: Linear {
                width: 2,
                weights: vec![2., 0., 0., 1.],
                bias: vec![0., 0.1],
            },
            origin: Linear {
                width: 2,
                weights: vec![1., 0., 0., 1., 0., 0.],
                bias: vec![0.; 3],
            },
            share: Linear {
                width: 2,
                weights: vec![0.; 2],
                bias: vec![0.],
            },
            human: Linear {
                width: 2,
                weights: vec![0.; 2],
                bias: vec![0.],
            },
            ai: Linear {
                width: 2,
                weights: vec![0.; 2],
                bias: vec![0.],
            },
        };
        let input = SharedInput {
            contract_sha256: "a".repeat(64),
            x: vec![0.25, -0.5],
        };
        let expected = vec![(2. * 0.25f64.tanh()).tanh(), ((-1f64).tanh() + 0.1).tanh()];
        assert_eq!(model.encode(&input).unwrap(), expected);
        let prediction = model.predict(&input).unwrap();
        let denominator = expected[0].exp() + expected[1].exp() + 1.;
        assert!((prediction.origin["human_only"] - expected[0].exp() / denominator).abs() < 1e-15);
        let mut bad = input.clone();
        bad.contract_sha256 = "b".repeat(64);
        assert!(model.encode(&bad).is_err());
        bad = input;
        bad.x.pop();
        assert!(model.encode(&bad).is_err());
    }
    #[test]
    fn stable_nonlinear_endpoints() {
        assert_eq!(sigmoid(1000.), 1.);
        assert_eq!(sigmoid(-1000.), 0.);
        let p = softmax(&[10000., 10000., -10000.]).unwrap();
        assert_eq!(p, vec![0.5, 0.5, 0.]);
    }
    #[test]
    fn rejects_prose_numeric_request() {
        assert!(
            serde_json::from_str::<SharedInput>(
                r#"{"contract_sha256":"a","x":[1.0],"text":"prose"}"#
            )
            .is_err()
        );
    }
    #[test]
    fn rejects_shape_mismatch() {
        let mut w = BTreeMap::from([
            (
                "layer.weight".into(),
                Tensor {
                    shape: vec![2, 2],
                    values: vec![0.; 3],
                },
            ),
            (
                "layer.bias".into(),
                Tensor {
                    shape: vec![2],
                    values: vec![0.; 2],
                },
            ),
        ]);
        assert!(Linear::take(&mut w, "layer", 2, 2).is_err());
    }
}
