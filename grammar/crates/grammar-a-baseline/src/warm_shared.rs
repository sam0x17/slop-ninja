//! Portable V6 residual adapter and shared A heads. The upstream text encoder
//! and native CFG extraction are separate, versioned dependencies.
use crate::shared::{
    Linear, RolePrediction, SharedInput, SharedModel, SharedPrediction, Tensor, sigmoid, softmax,
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    schema: String,
    architecture: Architecture,
    operations: Vec<String>,
    input_mode: String,
    contract_sha256: String,
    initialization_sha256: String,
    base_temperature: f64,
    temperature: f64,
    text_mean: Vec<f64>,
    text_scale: Vec<f64>,
    profiles: BTreeMap<String, Vec<String>>,
    thresholds: BTreeMap<String, f64>,
    supervision: BTreeMap<String, usize>,
    origin_order: Vec<String>,
    control: String,
    #[serde(default)]
    control_protocol_sha256: Option<String>,
    weights: BTreeMap<String, Tensor>,
}

pub struct WarmSharedModel {
    pub architecture: Architecture,
    pub contract_sha256: String,
    pub initialization_sha256: String,
    pub supervision: BTreeMap<String, usize>,
    pub control: String,
    pub control_protocol_sha256: Option<String>,
    input_mode: String,
    base_temperature: f64,
    temperature: f64,
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
}

impl WarmSharedModel {
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= 64 * 1024 * 1024,
            "artifact exceeds research size limit"
        );
        let mut a: Artifact = serde_json::from_slice(bytes)?;
        ensure!(
            a.schema == "slop_ninja_v6_residual_a_weights_v1",
            "unsupported artifact"
        );
        ensure!(
            a.operations
                == [
                    "linear", "tanh", "add", "subtract", "divide", "softmax", "sigmoid"
                ]
                && a.origin_order == ["human_only", "model_only", "mixed"],
            "unsupported operator/order contract"
        );
        ensure!(
            matches!(a.input_mode.as_str(), "text" | "combined")
                && matches!(
                    a.control.as_str(),
                    "joint" | "origin_only" | "attribution_only"
                ),
            "unsupported input mode/control"
        );
        for hash in [&a.contract_sha256, &a.initialization_sha256] {
            ensure!(
                hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid digest"
            );
        }
        ensure!(
            a.base_temperature.is_finite()
                && a.base_temperature > 0.
                && a.temperature.is_finite()
                && a.temperature > 0.,
            "invalid temperature"
        );
        let d = &a.architecture;
        ensure!(
            (1..=32768).contains(&d.dimension)
                && (1..=512).contains(&d.hidden)
                && (1..=4096).contains(&d.raw_text_dimension)
                && d.embedding == d.raw_text_dimension
                && d.raw_text_dimension <= d.dimension
                && d.human_profiles <= 4096
                && d.ai_profiles <= 4096,
            "architecture bounds"
        );
        ensure!(
            a.text_mean.len() == d.raw_text_dimension
                && a.text_scale.len() == d.raw_text_dimension
                && a.text_mean.iter().all(|v| v.is_finite())
                && a.text_scale.iter().all(|v| v.is_finite() && *v > 0.),
            "invalid frozen normalization"
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
        ensure!(
            a.profiles.len() == 2 && a.thresholds.len() == 2,
            "unexpected role"
        );
        if a.control != "joint" {
            let pin = a
                .control_protocol_sha256
                .as_deref()
                .context("adaptation protocol absent")?;
            ensure!(
                pin.len() == 64 && pin.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid adaptation protocol digest"
            );
            let count = |key: &str| {
                a.supervision
                    .get(key)
                    .copied()
                    .context("adaptation supervision absent")
            };
            ensure!(
                count("share")? == 0 && count("mixed_fractional_share")? == 0,
                "adaptation controls cannot train share"
            );
            if a.control == "origin_only" {
                ensure!(
                    count("origin")? > 0 && count("human")? == 0 && count("ai")? == 0,
                    "origin-only supervision differs"
                );
            } else {
                ensure!(
                    count("origin")? == 0
                        && (count("human")? > 0 || count("ai")? > 0)
                        && a.temperature == 1.,
                    "attribution-only cannot adapt/calibrate origin"
                );
            }
            for role in ["human", "ai"] {
                if count(role)? == 0 {
                    ensure!(
                        a.thresholds[role] == 1. && a.profiles[role].len() != 1,
                        "untrained adaptation attribution must use fallback threshold"
                    );
                }
            }
        } else {
            ensure!(
                a.control_protocol_sha256.is_none(),
                "joint does not use an adaptation protocol"
            );
        }
        let first = Linear::take(&mut a.weights, "residual.0", d.hidden, d.dimension)?;
        let residual = Linear::take(&mut a.weights, "residual.2", d.embedding, d.hidden)?;
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
            initialization_sha256: a.initialization_sha256,
            supervision: a.supervision,
            control: a.control,
            control_protocol_sha256: a.control_protocol_sha256,
            input_mode: a.input_mode,
            base_temperature: a.base_temperature,
            temperature: a.temperature,
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
        })
    }

    pub fn encode(&self, input: &SharedInput) -> Result<Vec<f64>> {
        ensure!(
            input.contract_sha256 == self.contract_sha256,
            "different feature contract"
        );
        ensure!(
            input.x.len() == self.architecture.dimension && input.x.iter().all(|v| v.is_finite()),
            "invalid numeric input"
        );
        let n = self.architecture.raw_text_dimension;
        let mut u = input.x.clone();
        for ((v, mean), scale) in u[..n].iter_mut().zip(&self.text_mean).zip(&self.text_scale) {
            *v = (*v - mean) / scale;
        }
        if self.input_mode == "text" {
            u[n..].fill(0.);
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
            .zip(&input.x[..n])
            .map(|(r, h)| r + h)
            .collect::<Vec<_>>();
        ensure!(z.iter().all(|v| v.is_finite()), "invalid shared encoding");
        Ok(z)
    }

    pub fn predict(&self, input: &SharedInput) -> Result<SharedPrediction> {
        let z = self.encode(input)?;
        let logits = self
            .origin
            .apply(&z)
            .into_iter()
            .map(|v| v / self.base_temperature / self.temperature)
            .collect::<Vec<_>>();
        let p = softmax(&logits)?;
        let ai_probability = (p[1] + p[2]).min(1.);
        let share_logit = self.share.apply(&z)[0];
        ensure!(share_logit.is_finite(), "invalid share logit");
        let share = (p[1] + p[2] * sigmoid(share_logit)).clamp(p[1], ai_probability);
        let mut attribution = BTreeMap::new();
        for (role, involvement, head) in [
            ("human", (p[0] + p[2]).min(1.), &self.human),
            ("ai", ai_probability, &self.ai),
        ] {
            let names = &self.profiles[role];
            let scores = softmax(&head.apply(&z))?;
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
            &self.residual,
            &self.origin,
            &self.share,
            &self.human,
            &self.ai,
        ]
        .iter()
        .map(|l| l.numeric_bytes())
        .sum::<usize>()
            + (self.text_mean.len() + self.text_scale.len()) * std::mem::size_of::<f64>()
    }
}

/// Version-dispatched numeric heads. This does not qualify full V6 serving.
pub enum NumericSharedModel {
    Mlp(SharedModel),
    V6Residual(WarmSharedModel),
}
impl NumericSharedModel {
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= 64 * 1024 * 1024,
            "artifact exceeds research size limit"
        );
        #[derive(Deserialize)]
        struct Header<'a> {
            schema: &'a str,
        }
        let h: Header<'_> = serde_json::from_slice(bytes)?;
        match h.schema {
            "slop_ninja_shared_a_weights_v1" => Ok(Self::Mlp(SharedModel::from_json(bytes)?)),
            "slop_ninja_v6_residual_a_weights_v1" => {
                Ok(Self::V6Residual(WarmSharedModel::from_json(bytes)?))
            }
            _ => anyhow::bail!("unsupported shared artifact"),
        }
    }
    pub fn predict(&self, x: &SharedInput) -> Result<SharedPrediction> {
        match self {
            Self::Mlp(m) => m.predict(x),
            Self::V6Residual(m) => m.predict(x),
        }
    }
    pub fn encode(&self, x: &SharedInput) -> Result<Vec<f64>> {
        match self {
            Self::Mlp(m) => m.encode(x),
            Self::V6Residual(m) => m.encode(x),
        }
    }
    pub fn numeric_weight_bytes(&self) -> usize {
        match self {
            Self::Mlp(m) => m.numeric_weight_bytes(),
            Self::V6Residual(m) => m.numeric_weight_bytes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn fixture() -> Value {
        let mut weights = serde_json::Map::new();
        for (name, rows, cols, values) in [
            ("residual.0", 1, 3, vec![1., 0., 2.]),
            ("residual.2", 2, 1, vec![0., 0.]),
            ("origin", 3, 2, vec![1., 0., 0., 1., 0., 0.]),
            ("mixed_share", 1, 2, vec![1., 1.]),
            ("human", 2, 2, vec![1., 0., 0., 1.]),
            ("ai", 1, 2, vec![1., 1.]),
        ] {
            weights.insert(
                format!("{name}.weight"),
                json!({"shape":[rows,cols],"values":values}),
            );
            weights.insert(
                format!("{name}.bias"),
                json!({"shape":[rows],"values":vec![0.;rows]}),
            );
        }
        json!({"schema":"slop_ninja_v6_residual_a_weights_v1",
            "architecture":{"dimension":3,"raw_text_dimension":2,"hidden":1,"embedding":2,"human_profiles":2,"ai_profiles":1},
            "operations":["linear","tanh","add","subtract","divide","softmax","sigmoid"],
            "input_mode":"combined","contract_sha256":"a".repeat(64),"initialization_sha256":"b".repeat(64),
            "base_temperature":2.,"temperature":1.,"text_mean":[2.,1.],"text_scale":[2.,4.],
            "profiles":{"human":["synthetic_a","synthetic_b"],"ai":["synthetic_generator"]},
            "thresholds":{"human":0.9,"ai":0.9},"supervision":{"origin":4},
            "origin_order":["human_only","model_only","mixed"],"control":"joint","weights":weights})
    }
    fn model(v: &Value) -> WarmSharedModel {
        WarmSharedModel::from_json(&serde_json::to_vec(v).unwrap()).unwrap()
    }
    fn input() -> SharedInput {
        SharedInput {
            contract_sha256: "a".repeat(64),
            x: vec![4., -3., 0.5],
        }
    }
    #[test]
    fn zero_residual_preserves_original_logits_and_temperature() {
        let m = model(&fixture());
        assert_eq!(m.encode(&input()).unwrap(), vec![4., -3.]);
        let expected = softmax(&[2., -1.5, 0.]).unwrap();
        let p = m.predict(&input()).unwrap();
        assert_eq!(p.origin["human_only"], expected[0]);
        assert_eq!(p.origin["model_only"], expected[1]);
        assert_eq!(p.origin["mixed"], expected[2]);
        assert!(
            100. * expected[1] <= p.ai_share_percent
                && p.ai_share_percent <= p.ai_confidence_percent
                && p.ai_confidence_percent <= 100.
        );
        assert_eq!(p.attribution.len(), 2);
        assert!(!p.attribution["ai"].fallback_asserts_presence);
    }
    #[test]
    fn residual_normalizes_only_its_input_and_all_heads_consume_adjusted_encoding() {
        let mut v = fixture();
        v["weights"]["residual.2.weight"]["values"] = json!([2., -1.]);
        let m = model(&v);
        let t = 2f64.tanh(); // normalized h[0]=1, CFG=0.5, first=[1,0,2].
        let z = vec![4. + 2. * t, -3. - t];
        assert_eq!(m.encode(&input()).unwrap(), z);
        let p = m.predict(&input()).unwrap();
        let origin = softmax(&[z[0] / 2., z[1] / 2., 0.]).unwrap();
        assert_eq!(p.origin["mixed"], origin[2]);
        assert!(
            (p.ai_share_percent / 100. - (origin[1] + origin[2] * sigmoid(z[0] + z[1]))).abs()
                < 1e-15
        );
        assert_eq!(
            p.attribution["human"].conditional_named_scores["synthetic_a"],
            softmax(&z).unwrap()[0]
        );
        v["input_mode"] = json!("text");
        assert_eq!(
            model(&v).encode(&input()).unwrap(),
            vec![4. + 2. * 1f64.tanh(), -3. - 1f64.tanh()]
        );
    }
    #[test]
    fn invalid_contracts_and_artifacts_fail_closed() {
        let v = fixture();
        for bad in [
            {
                let mut b = v.clone();
                b["text_scale"][0] = json!(0.);
                b
            },
            {
                let mut b = v.clone();
                b["architecture"]["embedding"] = json!(3);
                b
            },
            {
                let mut b = v.clone();
                b["weights"]["origin.weight"]["shape"] = json!([2, 3]);
                b
            },
            {
                let mut b = v.clone();
                b["operations"] = json!(["unqualified"]);
                b
            },
            {
                let mut b = v.clone();
                b["profiles"]["human"] = json!(["duplicate", "duplicate"]);
                b
            },
        ] {
            assert!(WarmSharedModel::from_json(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        let m = model(&v);
        let mut bad = input();
        bad.contract_sha256 = "c".repeat(64);
        assert!(m.predict(&bad).is_err());
        let mut bad = input();
        bad.x[0] = f64::NAN;
        assert!(m.predict(&bad).is_err());
        let mut bad = input();
        bad.x.pop();
        assert!(m.predict(&bad).is_err());
    }
    #[test]
    fn low_role_involvement_never_suppresses_conditional_attribution() {
        let mut v = fixture();
        v["weights"]["origin.bias"]["values"] = json!([0., 100., 0.]);
        v["thresholds"]["human"] = json!(1.);
        let p = model(&v).predict(&input()).unwrap();
        assert!(p.attribution["human"].involvement_probability < 1e-10);
        assert_eq!(p.attribution["human"].profile, "other_human");
        assert_eq!(p.attribution["human"].conditional_named_scores.len(), 2);
        assert!(!p.attribution["human"].fallback_asserts_presence);
    }

    fn control_fixture(control: &str) -> Value {
        let mut v = fixture();
        v["architecture"]["ai_profiles"] = json!(2);
        v["profiles"]["ai"] = json!(["synthetic_x", "synthetic_y"]);
        for (head, rows) in [("human", 2), ("ai", 2), ("mixed_share", 1)] {
            v["weights"][format!("{head}.weight")] =
                json!({"shape":[rows,2],"values":vec![0.; rows*2]});
            v["weights"][format!("{head}.bias")] = json!({"shape":[rows],"values":vec![0.; rows]});
        }
        v["thresholds"] = json!({"human":1.,"ai":1.});
        v["control"] = json!(control);
        if control != "joint" {
            v["control_protocol_sha256"] = json!("c".repeat(64));
        }
        v["supervision"] = if control == "attribution_only" {
            json!({"origin":0,"share":0,"mixed_fractional_share":0,"human":2,"ai":2})
        } else {
            json!({"origin":4,"share":0,"mixed_fractional_share":0,"human":0,"ai":0})
        };
        v
    }

    #[test]
    fn adaptation_controls_preserve_prediction_math_and_neutral_fallback() {
        let baseline = model(&control_fixture("joint"));
        let expected = serde_json::to_value(baseline.predict(&input()).unwrap()).unwrap();
        for control in ["origin_only", "attribution_only"] {
            let m = model(&control_fixture(control));
            assert_eq!(m.control, control);
            assert_eq!(m.control_protocol_sha256, Some("c".repeat(64)));
            assert_eq!(
                m.encode(&input()).unwrap(),
                baseline.encode(&input()).unwrap()
            );
            let p = m.predict(&input()).unwrap();
            assert_eq!(serde_json::to_value(&p).unwrap(), expected);
            for role in ["human", "ai"] {
                assert_eq!(p.attribution[role].profile, format!("other_{role}"));
                assert_eq!(p.attribution[role].conditional_named_scores.len(), 2);
                assert!(!p.attribution[role].fallback_asserts_presence);
            }
            assert!(
                (p.ai_share_percent / 100. - (p.origin["model_only"] + p.origin["mixed"] * 0.5))
                    .abs()
                    < 1e-15
            );
            assert!(
                NumericSharedModel::from_json(
                    &serde_json::to_vec(&control_fixture(control)).unwrap()
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn adaptation_controls_reject_inconsistent_supervision_or_calibration() {
        for (control, field, value) in [
            ("origin_only", "control", json!("unknown")),
            ("origin_only", "control_protocol_sha256", Value::Null),
            ("attribution_only", "control_protocol_sha256", json!("bad")),
            ("attribution_only", "temperature", json!(1.1)),
            ("origin_only", "thresholds", json!({"human":0.5,"ai":1.})),
            (
                "origin_only",
                "supervision",
                json!({"origin":4,"share":1,"mixed_fractional_share":0,"human":0,"ai":0}),
            ),
            (
                "origin_only",
                "supervision",
                json!({"origin":4,"share":0,"mixed_fractional_share":0,"human":1,"ai":0}),
            ),
            (
                "attribution_only",
                "supervision",
                json!({"origin":1,"share":0,"mixed_fractional_share":0,"human":2,"ai":2}),
            ),
        ] {
            let mut v = control_fixture(control);
            v[field] = value;
            assert!(
                WarmSharedModel::from_json(&serde_json::to_vec(&v).unwrap()).is_err(),
                "{control}/{field}"
            );
        }
    }
}
