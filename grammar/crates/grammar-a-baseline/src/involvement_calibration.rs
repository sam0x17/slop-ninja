//! Pre-declared CAL-only involvement calibration for A.
//!
//! Input is A's uncalibrated involvement logit g = log(p_model+p_mixed) -
//! log(p_human); the target is binary any-AI involvement from documented
//! origin. Weights are family/origin balanced, as for the Pangram comparator.
//!
//! Policy v2 (current): primary `isotonic_bounded_v2`, the `isotonic_v1` fit
//! (exact score-tie aggregation, weighted pool-adjacent-violators, linear
//! interpolation between adjacent knots, constant outside CAL support) with
//! every knot level clamped to [epsilon, 1-epsilon], epsilon =
//! 1/(2*(min(CAL human rows, CAL positive rows)+1)). The map therefore never
//! returns exactly 0 or 1, including outside CAL support. It is selected unless
//! a recorded mechanical failure occurs (declared support minimums not met, or
//! a single PAV level); the temperature map `involvement_temperature_v1`,
//! sigmoid(g/T), T from exp(0.1k), k=-30..30, nearest 1 on an exact tie, is the
//! fallback. CAL scores are never compared to choose the map; source-family
//! cross-fitted results are reported per held-out fold.
//!
//! Policy v3 (protocol v1.2, `docs/a-competitive-protocol.md` Calibration):
//! one scalar origin temperature T fitted on CAL by balanced three-class NLL,
//! then `isotonic_v1_clipped`: the `isotonic_v1` fit on sigmoid(g_T), the
//! temperature-scaled involvement, with every level and output clamped to
//! [1/(2 n_h), 1 - 1/(2 n_+)] (CAL human and positive documents). The
//! model-only/mixed ratio is the temperature-scaled origin head's. The
//! temperature-only map `origin_temperature_v1` replaces the isotonic step
//! only on a recorded mechanical failure (non-finite value, empty class, fit
//! error); CAL scores never choose the map.
//!
//! Policy v1 (legacy, frozen evidence only): unbounded `isotonic_v1`, chosen
//! when support minimums hold and its cross-fitted Brier is no worse than the
//! temperature map's. Nothing here reads Test.
use crate::shared::sigmoid;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// One CAL query: source family, documented origin (0 human-only, 1 model-only,
/// 2 mixed), and the uncalibrated involvement logit.
#[derive(Clone, Debug)]
pub struct Observation {
    pub family: String,
    pub origin: usize,
    pub logit: f64,
}

fn target(o: &Observation) -> f64 {
    f64::from(o.origin != 0)
}

/// 1/(families * observed origin strata in family * rows in the family/origin cell).
pub fn balanced_weights(rows: &[Observation]) -> Result<Vec<f64>> {
    ensure!(!rows.is_empty(), "no Calibration observations");
    let mut cells = BTreeMap::<(&str, usize), usize>::new();
    for r in rows {
        ensure!(
            !r.family.is_empty() && r.origin < 3 && r.logit.is_finite(),
            "invalid Calibration observation"
        );
        *cells.entry((&r.family, r.origin)).or_default() += 1;
    }
    let mut strata = BTreeMap::<&str, usize>::new();
    for (family, _) in cells.keys() {
        *strata.entry(family).or_default() += 1;
    }
    Ok(rows
        .iter()
        .map(|r| {
            1. / (strata.len() * strata[r.family.as_str()] * cells[&(r.family.as_str(), r.origin)])
                as f64
        })
        .collect())
}

/// Weighted Brier of predictions against binary involvement, normalized by total weight.
pub fn brier(rows: &[Observation], weights: &[f64], predict: impl Fn(f64) -> f64) -> f64 {
    let total = weights.iter().sum::<f64>();
    rows.iter()
        .zip(weights)
        .map(|(r, w)| w * (predict(r.logit) - target(r)).powi(2))
        .sum::<f64>()
        / total
}

#[derive(Clone, Debug, PartialEq)]
pub struct Knot {
    pub score: f64,
    pub involvement: f64,
    pub weight: f64,
    pub weighted_positives: f64,
    pub rows: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Map {
    Identity,
    Temperature(f64),
    /// Legacy unbounded isotonic map; can return exactly 0 or 1.
    Isotonic(Vec<Knot>),
    /// Isotonic levels clamped to [epsilon, 1-epsilon]; never 0 or 1.
    BoundedIsotonic { knots: Vec<Knot>, epsilon: f64 },
    /// Protocol v1.2 fallback: involvement of the temperature-scaled origin logits.
    OriginTemperature(f64),
    /// Protocol v1.2 primary: clipped isotonic map of the temperature-scaled involvement.
    ClippedIsotonic { temperature: f64, knots: Vec<Knot>, lo: f64, hi: f64 },
}

pub const CLIPPED_RULE: &str =
    "lo=1/(2*CAL human documents), hi=1-1/(2*CAL positive documents); every PAV level and every output clamped to [lo,hi]";
pub const ORIGIN_TEMPERATURE_RULE: &str =
    "one scalar T on the three origin logits; balanced-weighted three-class NLL on CAL; T=exp(0.01k), integer k=-300..300; nearest 1 on an exact tie";
const CLIPPED_INPUT: &str = "sigmoid(origin-temperature-scaled A involvement logit)";

/// The declared origin-temperature grid (exp(0) is exactly 1).
pub fn origin_temperature_grid() -> Vec<f64> {
    (-300..=300).map(|k| (f64::from(k) * 0.01).exp()).collect()
}

fn lse2(a: f64, b: f64) -> f64 {
    let m = a.max(b);
    m + ((a - m).exp() + (b - m).exp()).ln()
}

/// Temperature-scaled involvement logit g_T and the model-only share r of
/// the AI mass: g_T = lse(l1/T, l2/T) - l0/T + (g - (lse(l1, l2) - l0))/T,
/// where the last term is E2's span-gate offset (0 for E1).
pub fn origin_scaled(logits: &[f64], g: f64, t: f64) -> Result<(f64, f64)> {
    ensure!(
        logits.len() == 3 && logits.iter().all(|v| v.is_finite()) && g.is_finite(),
        "origin logits"
    );
    ensure!(t.is_finite() && t > 0., "origin temperature");
    let base = lse2(logits[1], logits[2]) - logits[0];
    let s = [logits[0] / t, logits[1] / t, logits[2] / t];
    let gt = lse2(s[1], s[2]) - s[0] + (g - base) / t;
    let m = s[1].max(s[2]);
    let (a, b) = ((s[1] - m).exp(), (s[2] - m).exp());
    let r = a / (a + b);
    ensure!(gt.is_finite() && r.is_finite(), "nonfinite scaled involvement");
    Ok((gt, r))
}

pub const BOUNDED_RULE: &str =
    "epsilon=1/(2*(min(human_rows,positive_rows)+1)) over the fitting rows; every PAV level clamped to [epsilon,1-epsilon]";

/// Declared bound for a fitting set: 1/(2*(min(humans, positives)+1)).
pub fn bound_epsilon(rows: &[Observation]) -> f64 {
    let humans = rows.iter().filter(|r| r.origin == 0).count();
    let positives = rows.len() - humans;
    1. / (2. * (humans.min(positives) as f64 + 1.))
}

fn fit_bounded(rows: &[Observation], weights: &[f64]) -> Result<(Vec<Knot>, f64)> {
    let epsilon = bound_epsilon(rows);
    let mut knots = fit_isotonic(rows, weights)?;
    for k in &mut knots {
        k.involvement = k.involvement.clamp(epsilon, 1. - epsilon);
    }
    Ok((knots, epsilon))
}

fn levels(knots: &[Knot]) -> usize {
    let mut v = knots.iter().map(|k| k.involvement).collect::<Vec<_>>();
    v.dedup();
    v.len()
}

pub fn temperature_grid() -> Vec<f64> {
    (-30..=30).map(|k| (f64::from(k) * 0.1).exp()).collect()
}

fn fit_temperature(rows: &[Observation], weights: &[f64]) -> (f64, f64) {
    let mut best: (f64, f64) = (brier(rows, weights, sigmoid), 1.);
    for t in temperature_grid() {
        let score = brier(rows, weights, |g| sigmoid(g / t));
        if score < best.0 || (score == best.0 && (t - 1.).abs() < (best.1 - 1.).abs()) {
            best = (score, t);
        }
    }
    (best.1, best.0)
}

fn fit_isotonic(rows: &[Observation], weights: &[f64]) -> Result<Vec<Knot>> {
    ensure!(
        !rows.is_empty() && rows.len() == weights.len(),
        "isotonic input"
    );
    let samples = rows
        .iter()
        .zip(weights)
        .map(|(r, w)| (sigmoid(r.logit), target(r), *w))
        .collect::<Vec<_>>();
    fit_isotonic_scores(samples)
}

/// The `isotonic_v1` fit on `(score, binary target, weight)` samples: exact
/// score-tie aggregation (stable order within a tie), then weighted
/// pool-adjacent-violators. A's calibrator applies it to sigmoid(g_T); the
/// Pangram comparator map applies it to the clamped content fraction, so both
/// use this one code path.
pub fn fit_isotonic_scores(mut samples: Vec<(f64, f64, f64)>) -> Result<Vec<Knot>> {
    ensure!(!samples.is_empty(), "isotonic input");
    // Equal scores share one target before PAV, irrespective of row order.
    samples.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut knots = Vec::<Knot>::new();
    for (x, y, w) in samples {
        match knots.last_mut() {
            Some(k) if k.score == x => {
                k.weight += w;
                k.weighted_positives += w * y;
                k.rows += 1;
            }
            _ => knots.push(Knot {
                score: x,
                involvement: 0.,
                weight: w,
                weighted_positives: w * y,
                rows: 1,
            }),
        }
    }
    // Blocks: (first, last, weight, positives).
    let mut blocks = Vec::<(usize, usize, f64, f64)>::new();
    for (i, k) in knots.iter().enumerate() {
        blocks.push((i, i, k.weight, k.weighted_positives));
        while blocks.len() >= 2 {
            let n = blocks.len();
            let (a, b) = (blocks[n - 2], blocks[n - 1]);
            if a.3 / a.2 <= b.3 / b.2 {
                break;
            }
            blocks.pop();
            let left = blocks.last_mut().unwrap();
            left.1 = b.1;
            left.2 += b.2;
            left.3 += b.3;
        }
    }
    for (first, last, w, p) in blocks {
        for k in &mut knots[first..=last] {
            k.involvement = (p / w).clamp(0., 1.);
        }
    }
    Ok(knots)
}

fn knot_json(knots: &[Knot]) -> Vec<Value> {
    knots
        .iter()
        .map(|k| {
            json!({"score":k.score,"involvement":k.involvement,"weight":k.weight,
                "weighted_positives":k.weighted_positives,"rows":k.rows})
        })
        .collect()
}

fn isotonic_predict(knots: &[Knot], g: f64) -> f64 {
    isotonic_predict_score(knots, sigmoid(g))
}

/// `isotonic_v1` prediction at score `x`: linear between adjacent unique
/// knots, constant outside the knot support (the path of [`Map`]).
pub fn isotonic_predict_score(knots: &[Knot], x: f64) -> f64 {
    let (first, last) = (&knots[0], &knots[knots.len() - 1]);
    if x <= first.score {
        return first.involvement;
    }
    if x >= last.score {
        return last.involvement;
    }
    let upper = knots.partition_point(|k| k.score <= x);
    let (a, b) = (&knots[upper - 1], &knots[upper]);
    (a.involvement + (b.involvement - a.involvement) * ((x - a.score) / (b.score - a.score)))
        .clamp(0., 1.)
}

impl Map {
    pub fn apply(&self, logit: f64) -> Result<f64> {
        ensure!(logit.is_finite(), "nonfinite involvement logit");
        Ok(match self {
            Map::Identity => sigmoid(logit),
            Map::Temperature(t) => sigmoid(logit / t),
            Map::Isotonic(knots) => isotonic_predict(knots, logit),
            Map::BoundedIsotonic { knots, epsilon } => {
                isotonic_predict(knots, logit).clamp(*epsilon, 1. - *epsilon)
            }
            Map::OriginTemperature(_) | Map::ClippedIsotonic { .. } => {
                anyhow::bail!("protocol v1.2 maps need the origin logits (apply_origin)")
            }
        })
    }
    /// True for the protocol v1.2 maps, which read the origin logits.
    pub fn needs_origin(&self) -> bool {
        matches!(self, Map::OriginTemperature(_) | Map::ClippedIsotonic { .. })
    }
    /// Calibrated involvement p and model-only share r of the AI mass from the
    /// origin logits (already divided by the base temperature) and g.
    pub fn apply_origin(&self, logits: &[f64], g: f64) -> Result<(f64, f64)> {
        match self {
            Map::OriginTemperature(t) => {
                let (gt, r) = origin_scaled(logits, g, *t)?;
                Ok((sigmoid(gt), r))
            }
            Map::ClippedIsotonic { temperature, knots, lo, hi } => {
                let (gt, r) = origin_scaled(logits, g, *temperature)?;
                Ok((isotonic_predict(knots, gt).clamp(*lo, *hi), r))
            }
            _ => Ok((self.apply(g)?, origin_scaled(logits, g, 1.)?.1)),
        }
    }
    pub fn method(&self) -> &'static str {
        match self {
            Map::Identity => "identity_v1",
            Map::Temperature(_) => "involvement_temperature_v1",
            Map::Isotonic(_) => "isotonic_v1",
            Map::BoundedIsotonic { .. } => "isotonic_bounded_v2",
            Map::OriginTemperature(_) => "origin_temperature_v1",
            Map::ClippedIsotonic { .. } => "isotonic_v1_clipped",
        }
    }
    pub fn description(&self) -> Value {
        match self {
            Map::Identity => json!({"method":"identity_v1"}),
            Map::Temperature(t) => json!({"method":"involvement_temperature_v1","temperature":t,
                "grid":"exp(k*0.1), integer k=-30..30, and 1; nearest 1 on exact tie"}),
            Map::Isotonic(knots) => json!({"method":"isotonic_v1",
                "input":"sigmoid(uncalibrated A involvement logit)",
                "target":"binary_any_AI_involvement_from_documented_origin",
                "algorithm":"exact_score_tie_aggregation_then_weighted_pool_adjacent_violators",
                "interpolation":"linear_between_adjacent_unique_CAL_score_knots; constant_at_each_end_outside_CAL_support",
                "knots":knot_json(knots)}),
            Map::BoundedIsotonic { knots, epsilon } => json!({"method":"isotonic_bounded_v2",
                "input":"sigmoid(uncalibrated A involvement logit)",
                "target":"binary_any_AI_involvement_from_documented_origin",
                "algorithm":"exact_score_tie_aggregation_then_weighted_pool_adjacent_violators",
                "interpolation":"linear_between_adjacent_unique_CAL_score_knots; constant_at_each_end_outside_CAL_support",
                "bound":BOUNDED_RULE,"epsilon":epsilon,"knots":knot_json(knots)}),
            Map::OriginTemperature(t) => json!({"method":"origin_temperature_v1","temperature":t,
                "temperature_rule":ORIGIN_TEMPERATURE_RULE,
                "output":"involvement of softmax(origin_logits/T); model-only/mixed ratio of the same softmax"}),
            Map::ClippedIsotonic { temperature, knots, lo, hi } => json!({"method":"isotonic_v1_clipped",
                "temperature":temperature,"temperature_rule":ORIGIN_TEMPERATURE_RULE,
                "input":CLIPPED_INPUT,
                "target":"binary_any_AI_involvement_from_documented_origin",
                "algorithm":"exact_score_tie_aggregation_then_weighted_pool_adjacent_violators",
                "interpolation":"linear_between_adjacent_unique_CAL_score_knots; constant_at_each_end_outside_CAL_support",
                "bound":CLIPPED_RULE,"lo":lo,"hi":hi,"knots":knot_json(knots)}),
        }
    }
    /// Load a frozen map without labels; refuses unknown or inconsistent maps.
    pub fn from_description(v: &Value) -> Result<Self> {
        match v["method"].as_str().context("calibration method")? {
            "identity_v1" => Ok(Map::Identity),
            "involvement_temperature_v1" => {
                let t = v["temperature"].as_f64().context("temperature")?;
                ensure!(
                    temperature_grid().contains(&t) || t == 1.,
                    "temperature outside the declared grid"
                );
                Ok(Map::Temperature(t))
            }
            "origin_temperature_v1" => {
                let t = v["temperature"].as_f64().context("temperature")?;
                ensure!(
                    v["temperature_rule"] == ORIGIN_TEMPERATURE_RULE && origin_temperature_grid().contains(&t),
                    "origin temperature outside the declared grid"
                );
                Ok(Map::OriginTemperature(t))
            }
            "isotonic_v1_clipped" => {
                let t = v["temperature"].as_f64().context("temperature")?;
                let (lo, hi) = (
                    v["lo"].as_f64().context("lo")?,
                    v["hi"].as_f64().context("hi")?,
                );
                ensure!(
                    v["temperature_rule"] == ORIGIN_TEMPERATURE_RULE
                        && origin_temperature_grid().contains(&t)
                        && v["input"] == CLIPPED_INPUT
                        && v["target"] == "binary_any_AI_involvement_from_documented_origin"
                        && v["bound"] == CLIPPED_RULE,
                    "clipped isotonic meaning differs"
                );
                ensure!(lo > 0. && lo <= 0.5 && hi >= 0.5 && hi < 1., "clipped isotonic bounds");
                let knots = parse_knots(v)?;
                ensure!(
                    knots.iter().all(|k| k.involvement >= lo && k.involvement <= hi),
                    "clipped isotonic levels outside [lo, hi]"
                );
                Ok(Map::ClippedIsotonic { temperature: t, knots, lo, hi })
            }
            method @ ("isotonic_v1" | "isotonic_bounded_v2") => {
                ensure!(
                    v["input"] == "sigmoid(uncalibrated A involvement logit)"
                        && v["target"] == "binary_any_AI_involvement_from_documented_origin",
                    "isotonic meaning differs"
                );
                let knots = parse_knots(v)?;
                if method == "isotonic_v1" {
                    return Ok(Map::Isotonic(knots));
                }
                let epsilon = v["epsilon"].as_f64().context("bounded epsilon")?;
                ensure!(
                    v["bound"] == BOUNDED_RULE
                        && epsilon.is_finite()
                        && epsilon > 0.
                        && epsilon < 0.5
                        && knots.iter().all(|k| {
                            k.involvement >= epsilon && k.involvement <= 1. - epsilon
                        }),
                    "bounded isotonic levels outside [epsilon, 1-epsilon]"
                );
                Ok(Map::BoundedIsotonic { knots, epsilon })
            }
            other => anyhow::bail!("unsupported calibration method {other}"),
        }
    }
}

/// Sorted, monotone, finite knots of a frozen isotonic description.
fn parse_knots(v: &Value) -> Result<Vec<Knot>> {
    let mut knots = Vec::new();
    for k in v["knots"].as_array().context("knots")? {
        let f = |key: &str| -> Result<f64> {
            let x = k[key].as_f64().context("knot field")?;
            ensure!(x.is_finite(), "nonfinite knot");
            Ok(x)
        };
        let knot = Knot {
            score: f("score")?,
            involvement: f("involvement")?,
            weight: f("weight")?,
            weighted_positives: f("weighted_positives")?,
            rows: k["rows"].as_u64().context("knot rows")? as usize,
        };
        ensure!(
            (0.0..=1.).contains(&knot.score)
                && (0.0..=1.).contains(&knot.involvement)
                && knot.weight > 0.
                && (0.0..=knot.weight * (1. + 1e-12))
                    .contains(&knot.weighted_positives)
                && knot.rows > 0,
            "invalid knot"
        );
        knots.push(knot);
    }
    ensure!(
        !knots.is_empty()
            && knots.windows(2).all(|p| {
                p[0].score < p[1].score && p[0].involvement <= p[1].involvement
            }),
        "knots must be sorted and monotone"
    );
    Ok(knots)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PolicyVersion {
    /// Legacy: unbounded isotonic_v1, chosen by cross-fitted Brier.
    V1,
    /// Shared contract: isotonic_bounded_v2 unless a recorded mechanical failure.
    V2,
    /// Protocol v1.2: origin temperature, then isotonic_v1_clipped unless a
    /// recorded mechanical failure (`select_protocol`).
    V3,
}

const V1_SELECTION: &str =
    "isotonic_iff_support_minimums_and_crossfit_brier_not_worse_than_temperature";
const V2_SELECTION: &str = "isotonic_unless_recorded_mechanical_failure";
const V3_SELECTION: &str =
    "isotonic_v1_clipped_unless_recorded_mechanical_failure_nonfinite_value_empty_class_or_fit_error";

/// Declared before CAL is opened; every field is part of the frozen protocol.
#[derive(Clone, Debug)]
pub struct Policy {
    pub version: PolicyVersion,
    pub min_human_rows: usize,
    pub min_positive_rows: usize,
    pub min_human_families: usize,
    pub min_positive_families: usize,
    pub folds: usize,
}

impl Policy {
    pub fn from_json(v: &Value) -> Result<Self> {
        let version = if v["schema"] == "slop_ninja_involvement_calibration_policy_v1"
            && v["primary"] == "isotonic_v1"
            && v["fallback"] == "involvement_temperature_v1"
            && v["selection"] == V1_SELECTION
        {
            PolicyVersion::V1
        } else if v["schema"] == "slop_ninja_involvement_calibration_policy_v2"
            && v["primary"] == "isotonic_bounded_v2"
            && v["bound"] == BOUNDED_RULE
            && v["fallback"] == "involvement_temperature_v1"
            && v["selection"] == V2_SELECTION
        {
            PolicyVersion::V2
        } else if v["schema"] == "slop_ninja_involvement_calibration_policy_v3"
            && v["primary"] == "isotonic_v1_clipped"
            && v["bound"] == CLIPPED_RULE
            && v["temperature_rule"] == ORIGIN_TEMPERATURE_RULE
            && v["fallback"] == "origin_temperature_v1"
            && v["selection"] == V3_SELECTION
        {
            // Protocol v1.2 has no support minimums or cross-fit choice.
            return Ok(Self { version: PolicyVersion::V3, min_human_rows: 0, min_positive_rows: 0,
                             min_human_families: 0, min_positive_families: 0, folds: 2 });
        } else {
            anyhow::bail!("unsupported calibration policy")
        };
        let n = |key: &str| -> Result<usize> {
            Ok(v[key].as_u64().with_context(|| format!("policy {key}"))? as usize)
        };
        let p = Self {
            version,
            min_human_rows: n("min_human_rows")?,
            min_positive_rows: n("min_positive_rows")?,
            min_human_families: n("min_human_families")?,
            min_positive_families: n("min_positive_families")?,
            folds: n("crossfit_folds")?,
        };
        ensure!((2..=20).contains(&p.folds), "cross-fit folds");
        Ok(p)
    }
    pub fn description(&self) -> Value {
        if self.version == PolicyVersion::V3 {
            return json!({"schema":"slop_ninja_involvement_calibration_policy_v3",
                "primary":"isotonic_v1_clipped","bound":CLIPPED_RULE,
                "temperature_rule":ORIGIN_TEMPERATURE_RULE,"fallback":"origin_temperature_v1",
                "selection":V3_SELECTION});
        }
        let mut v = json!({"fallback":"involvement_temperature_v1",
            "min_human_rows":self.min_human_rows,"min_positive_rows":self.min_positive_rows,
            "min_human_families":self.min_human_families,"min_positive_families":self.min_positive_families,
            "crossfit_folds":self.folds});
        match self.version {
            PolicyVersion::V1 => {
                v["schema"] = json!("slop_ninja_involvement_calibration_policy_v1");
                v["primary"] = json!("isotonic_v1");
                v["selection"] = json!(V1_SELECTION);
            }
            PolicyVersion::V3 => unreachable!(),
            PolicyVersion::V2 => {
                v["schema"] = json!("slop_ninja_involvement_calibration_policy_v2");
                v["primary"] = json!("isotonic_bounded_v2");
                v["bound"] = json!(BOUNDED_RULE);
                v["selection"] = json!(V2_SELECTION);
            }
        }
        v
    }
}

/// Deterministic family folds: sort families by SHA-256(name), deal round-robin.
pub fn family_folds(rows: &[Observation], folds: usize) -> BTreeMap<String, usize> {
    let mut families = rows
        .iter()
        .map(|r| r.family.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|f| (hex::encode(Sha256::digest(f.as_bytes())), f))
        .collect::<Vec<_>>();
    families.sort();
    families
        .into_iter()
        .enumerate()
        .map(|(i, (_, f))| (f, i % folds))
        .collect()
}

/// Out-of-sample behaviour of one map on held-out rows.
#[derive(Default)]
struct Held {
    weight: f64,
    loss: f64,
    rows: usize,
    exact_zero: usize,
    exact_one: usize,
    at_lower_bound: usize,
    at_upper_bound: usize,
    max_human: f64,
    min_positive: f64,
    human_rows: usize,
    positive_rows: usize,
}

impl Held {
    fn new() -> Self {
        Self {
            min_positive: f64::INFINITY,
            max_human: f64::NEG_INFINITY,
            ..Self::default()
        }
    }
    fn add(&mut self, r: &Observation, w: f64, p: f64, bound: Option<f64>) {
        self.weight += w;
        self.loss += w * (p - target(r)).powi(2);
        self.rows += 1;
        self.exact_zero += usize::from(p == 0.);
        self.exact_one += usize::from(p == 1.);
        if let Some(e) = bound {
            self.at_lower_bound += usize::from(p <= e);
            self.at_upper_bound += usize::from(p >= 1. - e);
        }
        if r.origin == 0 {
            self.human_rows += 1;
            self.max_human = self.max_human.max(p);
        } else {
            self.positive_rows += 1;
            self.min_positive = self.min_positive.min(p);
        }
    }
    fn json(&self) -> Value {
        let finite = |x: f64| if x.is_finite() { json!(x) } else { Value::Null };
        json!({"rows":self.rows,"human_rows":self.human_rows,"positive_rows":self.positive_rows,
            "balanced_brier_within_fold_weight":finite(self.loss / self.weight),
            "exact_zero":self.exact_zero,"exact_one":self.exact_one,
            "at_lower_bound":self.at_lower_bound,"at_upper_bound":self.at_upper_bound,
            "max_human_prediction":finite(self.max_human),"min_positive_prediction":finite(self.min_positive)})
    }
}

struct Crossfit {
    bounded: f64,
    isotonic: f64,
    temperature: f64,
    folds: Vec<Value>,
}

/// Source-family cross-fit: every map is refit (with refit weights) on the
/// training folds and applied to the held-out fold; per-fold behaviour kept.
fn crossfit(rows: &[Observation], weights: &[f64], folds: usize) -> Result<Crossfit> {
    let assignment = family_folds(rows, folds);
    let (mut bounded, mut iso, mut temp) = (
        vec![f64::NAN; rows.len()],
        vec![f64::NAN; rows.len()],
        vec![f64::NAN; rows.len()],
    );
    let mut reports = Vec::new();
    for fold in 0..folds {
        let (train, held): (Vec<usize>, Vec<usize>) =
            (0..rows.len()).partition(|&i| assignment[&rows[i].family] != fold);
        if held.is_empty() {
            continue;
        }
        ensure!(!train.is_empty(), "cross-fit fold without training rows");
        let subset = train.iter().map(|&i| rows[i].clone()).collect::<Vec<_>>();
        let w = balanced_weights(&subset)?;
        let (bounded_knots, epsilon) = fit_bounded(&subset, &w)?;
        let knots = fit_isotonic(&subset, &w)?;
        let (t, _) = fit_temperature(&subset, &w);
        let (lo, hi) = (knots[0].score, knots[knots.len() - 1].score);
        let (mut hb, mut hi_, mut ht) = (Held::new(), Held::new(), Held::new());
        let (mut below, mut above) = (0, 0);
        for &i in &held {
            let r = &rows[i];
            let x = sigmoid(r.logit);
            below += usize::from(x < lo);
            above += usize::from(x > hi);
            bounded[i] = isotonic_predict(&bounded_knots, r.logit).clamp(epsilon, 1. - epsilon);
            iso[i] = isotonic_predict(&knots, r.logit);
            temp[i] = sigmoid(r.logit / t);
            hb.add(r, weights[i], bounded[i], Some(epsilon));
            hi_.add(r, weights[i], iso[i], None);
            ht.add(r, weights[i], temp[i], None);
        }
        reports.push(json!({"fold":fold,"held_out_families":held.iter().map(|&i| rows[i].family.as_str())
                .collect::<BTreeSet<_>>().len(),
            "training_rows":train.len(),"epsilon":epsilon,"temperature":t,
            "training_score_support":[lo,hi],"held_out_scores_below_support":below,
            "held_out_scores_above_support":above,
            "isotonic_bounded_v2":hb.json(),"isotonic_v1":hi_.json(),"involvement_temperature_v1":ht.json()}));
    }
    let score = |p: &[f64]| {
        rows.iter()
            .zip(weights)
            .zip(p)
            .map(|((r, w), p)| w * (p - target(r)).powi(2))
            .sum::<f64>()
            / weights.iter().sum::<f64>()
    };
    Ok(Crossfit {
        bounded: score(&bounded),
        isotonic: score(&iso),
        temperature: score(&temp),
        folds: reports,
    })
}

/// Fit the maps on CAL, apply the declared rule, and report every number.
pub fn select(rows: &[Observation], policy: &Policy) -> Result<(Map, Value)> {
    ensure!(policy.version != PolicyVersion::V3, "policy v3 needs the origin logits (select_protocol)");
    let weights = balanced_weights(rows)?;
    let humans = rows.iter().filter(|r| r.origin == 0);
    let positives = rows.iter().filter(|r| r.origin != 0);
    let families = |it: &mut dyn Iterator<Item = &Observation>| {
        it.map(|r| r.family.as_str()).collect::<BTreeSet<_>>().len()
    };
    let support = json!({"human_rows":humans.clone().count(),"positive_rows":positives.clone().count(),
        "human_families":families(&mut humans.clone()),"positive_families":families(&mut positives.clone())});
    let supported = support["human_rows"].as_u64().unwrap() as usize >= policy.min_human_rows
        && support["positive_rows"].as_u64().unwrap() as usize >= policy.min_positive_rows
        && support["human_families"].as_u64().unwrap() as usize >= policy.min_human_families
        && support["positive_families"].as_u64().unwrap() as usize >= policy.min_positive_families;
    let knots = fit_isotonic(rows, &weights)?;
    let (bounded_knots, epsilon) = fit_bounded(rows, &weights)?;
    let (t, temperature_brier) = fit_temperature(rows, &weights);
    let iso = Map::Isotonic(knots);
    let bounded = Map::BoundedIsotonic {
        knots: bounded_knots,
        epsilon,
    };
    let iso_brier = brier(rows, &weights, |g| iso.apply(g).unwrap());
    let bounded_brier = brier(rows, &weights, |g| bounded.apply(g).unwrap());
    let fold_count = rows
        .iter()
        .map(|r| r.family.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let cv = if fold_count >= policy.folds {
        Some(crossfit(rows, &weights, policy.folds)?)
    } else {
        None
    };
    let pav_levels = match &bounded {
        Map::BoundedIsotonic { knots, .. } => levels(knots),
        _ => unreachable!(),
    };
    let (selected, reason) = match policy.version {
        PolicyVersion::V1 => {
            let cv = cv.as_ref().filter(|_| supported);
            let choose = cv.is_some_and(|c| c.isotonic <= c.temperature);
            let reason = if !supported {
                "support_minimums_not_met"
            } else if cv.is_none() {
                "fewer_families_than_crossfit_folds"
            } else if choose {
                "isotonic_crossfit_brier_not_worse"
            } else {
                "temperature_crossfit_brier_lower"
            };
            (if choose { iso.clone() } else { Map::Temperature(t) }, reason)
        }
        PolicyVersion::V3 => unreachable!(),
        PolicyVersion::V2 => {
            // Mechanical failures only; no CAL score comparison selects the map.
            if !supported {
                (Map::Temperature(t), "mechanical_failure_support_minimums_not_met")
            } else if pav_levels < 2 {
                (Map::Temperature(t), "mechanical_failure_single_pav_level")
            } else {
                (bounded.clone(), "isotonic_bounded_primary")
            }
        }
    };
    let finite = |x: Option<f64>| match x {
        Some(x) if x.is_finite() => json!(x),
        _ => Value::Null,
    };
    let report = json!({"policy":policy.description(),"support":support,
        "weighting":"family_origin_balanced","weight_formula":"1/(families * observed_origin_strata_in_family * rows_in_family_origin_cell)",
        "uncalibrated_CAL_brier":brier(rows, &weights, sigmoid),
        "isotonic_in_sample_CAL_brier":iso_brier,"isotonic_bounded_in_sample_CAL_brier":bounded_brier,
        "temperature_in_sample_CAL_brier":temperature_brier,"temperature":t,
        "isotonic_bound_epsilon":epsilon,"isotonic_pav_levels":pav_levels,
        "isotonic_crossfit_CAL_brier":finite(cv.as_ref().map(|c| c.isotonic)),
        "isotonic_bounded_crossfit_CAL_brier":finite(cv.as_ref().map(|c| c.bounded)),
        "temperature_crossfit_CAL_brier":finite(cv.as_ref().map(|c| c.temperature)),
        "crossfit_folds":cv.as_ref().map(|c| json!(c.folds)).unwrap_or(Value::Null),
        "crossfit":"source-family folds by sorted SHA-256(family), round robin; every map and its weights refit inside each training fold; Brier normalized by the full CAL weight; per-fold Brier by the held-out fold's weight",
        "selected_method":selected.method(),"selection_reason":reason,"selected":selected.description(),
        "isotonic_candidate":iso.description(),"isotonic_bounded_candidate":bounded.description(),
        "limits":["CAL scores are fitted diagnostics, never independent evaluation.",
                  "Cross-fitted folds are out of sample within CAL only; they are not Development or Test evidence.",
                  "Calibration changes involvement only; the model-only/mixed ratio stays the origin head's."]});
    Ok((selected, report))
}

/// One CAL document for the protocol v1.2 map: source family, documented
/// origin, the origin logits (divided by the base temperature) and g.
#[derive(Clone, Debug)]
pub struct OriginObservation {
    pub family: String,
    pub origin: usize,
    pub logits: [f64; 3],
    pub logit: f64,
}

fn origin_nll(rows: &[OriginObservation], weights: &[f64], t: f64) -> f64 {
    let total = weights.iter().sum::<f64>();
    rows.iter()
        .zip(weights)
        .map(|(r, w)| {
            let s = r.logits.map(|v| v / t);
            let m = s.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let z = m + s.iter().map(|v| (v - m).exp()).sum::<f64>().ln();
            w * (z - s[r.origin])
        })
        .sum::<f64>()
        / total
}

/// Protocol v1.2 CAL fit (policy v3): origin temperature by balanced
/// three-class NLL over the declared grid (nearest 1 on an exact tie), then
/// the clipped isotonic map of the scaled involvement. The temperature-only
/// map replaces the isotonic step only on a recorded mechanical failure.
pub fn select_protocol(rows: &[OriginObservation], policy: &Policy) -> Result<(Map, Value)> {
    ensure!(policy.version == PolicyVersion::V3, "select_protocol needs policy v3");
    ensure!(!rows.is_empty(), "no CAL observations");
    for r in rows {
        ensure!(r.origin < 3 && !r.family.is_empty(), "invalid CAL observation");
    }
    let plain = rows
        .iter()
        .map(|r| Observation { family: r.family.clone(), origin: r.origin, logit: r.logit })
        .collect::<Vec<_>>();
    let weights = balanced_weights(&plain)?;
    let mut best: Option<(f64, f64)> = None;
    for t in origin_temperature_grid() {
        let nll = origin_nll(rows, &weights, t);
        if !nll.is_finite() {
            continue;
        }
        let better = match best {
            None => true,
            Some((b, bt)) => nll < b || (nll == b && t.ln().abs() < bt.ln().abs()),
        };
        if better {
            best = Some((nll, t));
        }
    }
    let (nll, t) = best.context("origin temperature: no finite balanced NLL on the grid")?;
    let scaled = rows
        .iter()
        .map(|r| {
            Ok(Observation { family: r.family.clone(), origin: r.origin,
                             logit: origin_scaled(&r.logits, r.logit, t)?.0 })
        })
        .collect::<Result<Vec<_>>>()?;
    let humans = rows.iter().filter(|r| r.origin == 0).count();
    let positives = rows.len() - humans;
    let (lo, hi) = (1. / (2. * humans.max(1) as f64), 1. - 1. / (2. * positives.max(1) as f64));
    let fitted = if humans == 0 || positives == 0 {
        Err("mechanical_failure_empty_class".to_owned())
    } else {
        match fit_isotonic(&scaled, &weights) {
            Err(e) => Err(format!("mechanical_failure_fit_error: {e}")),
            Ok(mut knots) => {
                if knots.iter().all(|k| k.score.is_finite() && k.involvement.is_finite()) {
                    for k in &mut knots {
                        k.involvement = k.involvement.clamp(lo, hi);
                    }
                    Ok(knots)
                } else {
                    Err("mechanical_failure_nonfinite_value".to_owned())
                }
            }
        }
    };
    let temperature_map = Map::OriginTemperature(t);
    let (selected, reason, candidate) = match fitted {
        Ok(knots) => {
            let m = Map::ClippedIsotonic { temperature: t, knots, lo, hi };
            (m.clone(), "isotonic_v1_clipped_primary".to_owned(), Some(m))
        }
        Err(reason) => (temperature_map.clone(), reason, None),
    };
    let scaled_brier = |m: &Map| -> Result<f64> {
        let mut loss = 0.;
        for ((r, o), w) in rows.iter().zip(&plain).zip(&weights) {
            loss += w * (m.apply_origin(&r.logits, r.logit)?.0 - target(o)).powi(2);
        }
        Ok(loss / weights.iter().sum::<f64>())
    };
    let pav_levels = match &candidate {
        Some(Map::ClippedIsotonic { knots, .. }) => Some(levels(knots)),
        _ => None,
    };
    let report = json!({"policy":policy.description(),
        "support":{"human_rows":humans,"positive_rows":positives,
            "human_families":rows.iter().filter(|r| r.origin == 0).map(|r| r.family.as_str()).collect::<BTreeSet<_>>().len(),
            "positive_families":rows.iter().filter(|r| r.origin != 0).map(|r| r.family.as_str()).collect::<BTreeSet<_>>().len()},
        "weighting":"family_origin_balanced",
        "weight_formula":"1/(families * observed_origin_strata_in_family * rows_in_family_origin_cell)",
        "origin_temperature":t,"origin_temperature_balanced_nll":nll,
        "origin_temperature_one_balanced_nll":origin_nll(rows, &weights, 1.),
        "uncalibrated_CAL_brier":brier(&plain, &weights, sigmoid),
        "origin_temperature_in_sample_CAL_brier":scaled_brier(&temperature_map)?,
        "isotonic_clipped_in_sample_CAL_brier":match &candidate { Some(m) => json!(scaled_brier(m)?), None => Value::Null },
        "clip":{"lo":lo,"hi":hi,"rule":CLIPPED_RULE},"isotonic_pav_levels":pav_levels,
        "selected_method":selected.method(),"selection_reason":reason,"selected":selected.description(),
        "limits":["CAL scores are fitted diagnostics, never independent evaluation.",
                  "In-sample CAL Brier is reported, never compared to choose the map.",
                  "The model-only/mixed ratio is that of the temperature-scaled origin head."]});
    Ok((selected, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn obs(family: &str, origin: usize, logit: f64) -> Observation {
        Observation {
            family: family.into(),
            origin,
            logit,
        }
    }
    #[test]
    fn weights_balance_families_and_origin_cells() {
        let rows = [
            obs("a", 0, 0.),
            obs("a", 2, 0.),
            obs("a", 2, 0.),
            obs("b", 1, 0.),
        ];
        let w = balanced_weights(&rows).unwrap();
        assert_eq!(w, vec![0.25, 0.125, 0.125, 0.5]);
        assert!((w.iter().sum::<f64>() - 1.).abs() < 1e-15);
    }
    #[test]
    fn isotonic_is_monotone_ties_aggregate_and_round_trips() {
        let rows = [
            obs("a", 0, -2.),
            obs("b", 2, -1.),
            obs("c", 0, -1.),
            obs("d", 0, 0.5),
            obs("e", 1, 1.),
            obs("f", 2, 3.),
        ];
        let w = vec![1.; rows.len()];
        let knots = fit_isotonic(&rows, &w).unwrap();
        assert_eq!(knots.len(), 5);
        assert_eq!(knots[1].rows, 2);
        assert!(
            knots
                .windows(2)
                .all(|p| p[0].involvement <= p[1].involvement)
        );
        // -1 tie (one positive of two) pools with the following human: 1/3.
        assert!((knots[1].involvement - 1. / 3.).abs() < 1e-12);
        assert!((knots[2].involvement - 1. / 3.).abs() < 1e-12);
        let map = Map::Isotonic(knots);
        let back = Map::from_description(&map.description()).unwrap();
        assert_eq!(map, back);
        assert_eq!(map.apply(-50.).unwrap(), 0.);
        assert_eq!(map.apply(50.).unwrap(), 1.);
        let mid = map.apply(0.75).unwrap();
        assert!(mid > 1. / 3. && mid < 1.);
        let mut bad = map.description();
        bad["knots"][1]["involvement"] = json!(0.9);
        assert!(Map::from_description(&bad).is_err());
    }
    #[test]
    fn temperature_grid_tie_and_frozen_load() {
        // Symmetric separable data: sharper is better, so the largest-T-inverse wins.
        let rows = [obs("a", 0, -1.), obs("b", 1, 1.)];
        let (t, _) = fit_temperature(&rows, &[1., 1.]);
        assert!((t - (-3f64).exp()).abs() < 1e-15);
        // All-zero logits: every T ties; nearest 1 is chosen.
        let rows = [obs("a", 0, 0.), obs("b", 1, 0.)];
        assert_eq!(fit_temperature(&rows, &[1., 1.]).0, 1.);
        assert!(
            Map::from_description(
                &json!({"method":"involvement_temperature_v1","temperature":1.7})
            )
            .is_err()
        );
        assert_eq!(
            Map::from_description(&Map::Temperature((0.3f64).exp()).description()).unwrap(),
            Map::Temperature((0.3f64).exp())
        );
    }
    #[test]
    fn selection_falls_back_without_support_and_crossfits_otherwise() {
        let mut rows = Vec::new();
        for f in 0..30 {
            let fam = format!("work-{f}");
            for k in 0..4 {
                let x = f64::from(f % 7) * 0.3 - 1. + f64::from(k) * 0.05;
                rows.push(obs(&fam, 0, x - 1.5));
                rows.push(obs(&fam, 2, x + 0.5 + f64::from(k % 2)));
            }
        }
        let strict = Policy {
            version: PolicyVersion::V1,
            min_human_rows: 1000,
            min_positive_rows: 1,
            min_human_families: 1,
            min_positive_families: 1,
            folds: 5,
        };
        let (map, report) = select(&rows, &strict).unwrap();
        assert_eq!(map.method(), "involvement_temperature_v1");
        assert_eq!(report["selection_reason"], "support_minimums_not_met");
        let open = Policy {
            min_human_rows: 10,
            ..strict
        };
        let (map, report) = select(&rows, &open).unwrap();
        let (iso, temp) = (
            report["isotonic_crossfit_CAL_brier"].as_f64().unwrap(),
            report["temperature_crossfit_CAL_brier"].as_f64().unwrap(),
        );
        assert_eq!(map.method() == "isotonic_v1", iso <= temp);
        assert!(
            report["isotonic_in_sample_CAL_brier"].as_f64().unwrap()
                <= report["temperature_in_sample_CAL_brier"].as_f64().unwrap() + 1e-12
        );
        let folds = family_folds(&rows, 5);
        assert_eq!(folds.len(), 30);
        assert!((0..5).all(|k| folds.values().filter(|v| **v == k).count() == 6));
        assert!(Policy::from_json(&open.description()).is_ok());
    }

    fn separable() -> Vec<Observation> {
        // 30 families; humans strictly below positives, so unbounded PAV gives 0 and 1.
        let mut rows = Vec::new();
        for f in 0..30u32 {
            let fam = format!("work-{f}");
            for k in 0..3u32 {
                rows.push(obs(&fam, 0, -3. + f64::from(f) * 0.01 + f64::from(k) * 0.001));
                let origin = (2 - k % 2) as usize;
                rows.push(obs(&fam, origin, 1. + f64::from(f) * 0.02 + f64::from(k) * 0.002));
            }
        }
        rows
    }
    #[test]
    fn bounded_isotonic_never_returns_zero_or_one_and_round_trips() {
        let rows = separable();
        let w = balanced_weights(&rows).unwrap();
        let unbounded = Map::Isotonic(fit_isotonic(&rows, &w).unwrap());
        assert_eq!(unbounded.apply(-40.).unwrap(), 0.);
        assert_eq!(unbounded.apply(40.).unwrap(), 1.);
        let (knots, epsilon) = fit_bounded(&rows, &w).unwrap();
        // 90 humans, 90 positives.
        assert_eq!(epsilon, 1. / 182.);
        let map = Map::BoundedIsotonic { knots, epsilon };
        for g in [-1e6, -40., -3., -1., 0., 1., 2., 40., 1e6] {
            let p = map.apply(g).unwrap();
            assert!(p >= epsilon && p <= 1. - epsilon && p != 0. && p != 1.);
        }
        assert_eq!(map.apply(-40.).unwrap(), epsilon);
        assert_eq!(map.apply(40.).unwrap(), 1. - epsilon);
        let back = Map::from_description(&map.description()).unwrap();
        assert_eq!(map, back);
        let mut bad = map.description();
        bad["knots"][0]["involvement"] = json!(0.);
        assert!(Map::from_description(&bad).is_err());
        let mut bad = map.description();
        bad["epsilon"] = json!(0.);
        assert!(Map::from_description(&bad).is_err());
        let mut bad = map.description();
        bad["bound"] = json!("other");
        assert!(Map::from_description(&bad).is_err());
    }
    #[test]
    fn v2_selects_bounded_without_score_comparison_and_reports_folds() {
        let rows = separable();
        let policy = Policy {
            version: PolicyVersion::V2,
            min_human_rows: 10,
            min_positive_rows: 10,
            min_human_families: 5,
            min_positive_families: 5,
            folds: 5,
        };
        let back = Policy::from_json(&policy.description()).unwrap();
        assert_eq!(back.version, PolicyVersion::V2);
        let (map, report) = select(&rows, &policy).unwrap();
        assert_eq!(map.method(), "isotonic_bounded_v2");
        assert_eq!(report["selection_reason"], "isotonic_bounded_primary");
        let folds = report["crossfit_folds"].as_array().unwrap();
        assert_eq!(folds.len(), 5);
        for f in folds {
            assert_eq!(f["isotonic_bounded_v2"]["exact_zero"], 0);
            assert_eq!(f["isotonic_bounded_v2"]["exact_one"], 0);
            assert!(f["epsilon"].as_f64().unwrap() > 0.);
            assert_eq!(f["isotonic_bounded_v2"]["rows"], 36);
        }
        // Unbounded isotonic does hit 0/1 out of sample on this data.
        let zeros = folds
            .iter()
            .map(|f| {
                f["isotonic_v1"]["exact_zero"].as_u64().unwrap()
                    + f["isotonic_v1"]["exact_one"].as_u64().unwrap()
            })
            .sum::<u64>();
        assert!(zeros > 0);
        // Support failure is the only route to the fallback (plus a single level).
        let strict = Policy {
            min_human_rows: 1000,
            ..policy.clone()
        };
        let (map, report) = select(&rows, &strict).unwrap();
        assert_eq!(map.method(), "involvement_temperature_v1");
        assert_eq!(
            report["selection_reason"],
            "mechanical_failure_support_minimums_not_met"
        );
        let flat = (0..12)
            .map(|i| obs(&format!("f{}", i % 6), (i % 2) as usize, 0.5))
            .collect::<Vec<_>>();
        let loose = Policy {
            min_human_rows: 1,
            min_positive_rows: 1,
            min_human_families: 1,
            min_positive_families: 1,
            ..policy
        };
        let (map, report) = select(&flat, &loose).unwrap();
        assert_eq!(map.method(), "involvement_temperature_v1");
        assert_eq!(report["selection_reason"], "mechanical_failure_single_pav_level");
    }

    fn oobs(family: &str, origin: usize, logits: [f64; 3]) -> OriginObservation {
        let g = lse2(logits[1], logits[2]) - logits[0];
        OriginObservation { family: family.into(), origin, logits, logit: g }
    }

    fn v3() -> Policy {
        Policy::from_json(&json!({"schema":"slop_ninja_involvement_calibration_policy_v3",
            "primary":"isotonic_v1_clipped","bound":CLIPPED_RULE,"temperature_rule":ORIGIN_TEMPERATURE_RULE,
            "fallback":"origin_temperature_v1","selection":V3_SELECTION})).unwrap()
    }

    #[test]
    fn protocol_policy_round_trips_and_refuses_the_plain_path() {
        let p = v3();
        assert_eq!(p.version, PolicyVersion::V3);
        assert_eq!(Policy::from_json(&p.description()).unwrap().version, PolicyVersion::V3);
        assert!(select(&[obs("a", 0, 0.), obs("b", 1, 1.)], &p).is_err());
        let mut bad = p.description();
        bad["bound"] = json!("other");
        assert!(Policy::from_json(&bad).is_err());
    }

    #[test]
    fn origin_temperature_one_is_the_identity_and_scales_the_ratio() {
        let l = [0.3, 1.1, -0.4];
        let g = lse2(l[1], l[2]) - l[0];
        let (p, r) = Map::OriginTemperature(1.).apply_origin(&l, g).unwrap();
        assert!((p - sigmoid(g)).abs() < 1e-15);
        assert!((r - 1.1f64.exp() / (1.1f64.exp() + (-0.4f64).exp())).abs() < 1e-15);
        let t = 2.;
        let (p, r) = Map::OriginTemperature(t).apply_origin(&l, g).unwrap();
        let e = l.map(|v| (v / t).exp());
        assert!((p - (e[1] + e[2]) / (e[0] + e[1] + e[2])).abs() < 1e-14);
        assert!((r - e[1] / (e[1] + e[2])).abs() < 1e-14);
        // The span-gate offset is scaled by 1/T.
        let (p2, _) = Map::OriginTemperature(t).apply_origin(&l, g + 1.).unwrap();
        assert!((p2 - sigmoid((p / (1. - p)).ln() + 0.5)).abs() < 1e-12);
        assert!(Map::OriginTemperature(t).apply(g).is_err());
        // Plain maps keep their involvement and take the unscaled ratio.
        let (p, r) = Map::Temperature(2.).apply_origin(&l, g).unwrap();
        assert_eq!(p, sigmoid(g / 2.));
        assert!((r - 1.1f64.exp() / (1.1f64.exp() + (-0.4f64).exp())).abs() < 1e-15);
    }

    #[test]
    fn protocol_fit_clips_to_document_counts_and_round_trips() {
        let mut rows = Vec::new();
        for i in 0..12 {
            let x = f64::from(i) * 0.4;
            rows.push(oobs(&format!("h{i}"), 0, [x.mul_add(-1., 2.), 0., -1.]));
            rows.push(oobs(&format!("p{i}"), 1 + (i % 2) as usize, [-x, 1.5, 0.2 * x]));
        }
        rows.push(oobs("p99", 1, [3., -2., -2.]));
        let (map, report) = select_protocol(&rows, &v3()).unwrap();
        assert_eq!(report["selected_method"], "isotonic_v1_clipped");
        let Map::ClippedIsotonic { temperature, knots, lo, hi } = &map else { panic!("primary map") };
        assert_eq!((*lo, *hi), (1. / 24., 1. - 1. / 26.));
        assert!(origin_temperature_grid().contains(temperature));
        assert!(knots.iter().all(|k| k.involvement >= *lo && k.involvement <= *hi));
        for r in &rows {
            let (p, _) = map.apply_origin(&r.logits, r.logit).unwrap();
            assert!(p >= *lo && p <= *hi);
        }
        assert_eq!(map.apply_origin(&[50., -50., -50.], -100.).unwrap().0, knots[0].involvement);
        let back = Map::from_description(&map.description()).unwrap();
        assert_eq!(back, map);
        let mut bad = map.description();
        bad["lo"] = json!(0.0);
        assert!(Map::from_description(&bad).is_err());
        let mut bad = map.description();
        bad["temperature"] = json!(1.2345);
        assert!(Map::from_description(&bad).is_err());
        let t = Map::OriginTemperature(*temperature);
        assert_eq!(Map::from_description(&t.description()).unwrap(), t);
        // The temperature minimizes the balanced NLL over the grid.
        let w = balanced_weights(&rows.iter().map(|r| obs(&r.family, r.origin, r.logit)).collect::<Vec<_>>()).unwrap();
        let best = origin_nll(&rows, &w, *temperature);
        assert!(origin_temperature_grid().iter().all(|t| origin_nll(&rows, &w, *t) >= best));
    }

    #[test]
    fn protocol_fit_falls_back_only_on_an_empty_class() {
        let rows = [oobs("a", 0, [1., 0., 0.]), oobs("b", 0, [0.5, 0., 0.])];
        let (map, report) = select_protocol(&rows, &v3()).unwrap();
        assert_eq!(report["selection_reason"], "mechanical_failure_empty_class");
        assert!(matches!(map, Map::OriginTemperature(_)));
        // A single PAV level is not a mechanical failure under the protocol.
        let rows = [oobs("a", 0, [0., 1., 0.]), oobs("b", 1, [1., 0., 0.])];
        let (map, _) = select_protocol(&rows, &v3()).unwrap();
        assert_eq!(map.method(), "isotonic_v1_clipped");
    }
}
