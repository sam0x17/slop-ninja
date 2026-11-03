//! Pangram comparator map of the competitive A protocol v1.2
//! (`calibration.pangram_map`, `comparator`, `operating_point.pangram`).
//!
//! `isotonic_v1_clipped` on Pangram's clamped fraction
//! `min(1, fraction_ai + fraction_ai_assisted)` over the CAL subsample, with
//! fit weight = full-CAL balanced weight / stratum inclusion probability, valid
//! pinned-version observations only, and clip bounds `[1/(2 n_h),
//! 1 - 1/(2 n_+)]` counted over the subsample documents with valid
//! observations. The fit and prediction are A's calibrator code path
//! (`involvement_calibration::fit_isotonic_scores` and
//! `isotonic_predict_score`). Operating points use A's rule
//! (`comp_eval::operating_threshold`) on the calibrated values of the CAL
//! subsample documents with valid observations, humans counted once each.
use crate::comp_eval::{ALPHA_01PCT, ALPHA_1PCT, operating_threshold};
use crate::involvement_calibration::{Knot, fit_isotonic_scores, isotonic_predict_score};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub const MAP_SCHEMA: &str = "slop_ninja_a_comp_pangram_cal_map_v1";
pub const METHOD: &str = "isotonic_v1_clipped";
pub const INPUT: &str = "min(1, fraction_ai + fraction_ai_assisted) of the pinned Pangram version";
pub const WEIGHT_RULE: &str =
    "full-CAL balanced weight (equal source-group mass, equal observed origin-cell mass within a group, equal document mass within a cell, over all primary CAL documents) / stratum inclusion probability pi_h = n_h / N_h";
pub const CLIP_RULE: &str =
    "lo = 1/(2 n_h), hi = 1 - 1/(2 n_pos), n_h and n_pos the CAL subsample human and positive documents with valid observations; every PAV level and every output clamped to [lo, hi]";
/// More than this fraction of selected documents missing: Tier C not evaluated.
pub const MISSING_FRACTION_MAX: f64 = 0.02;

/// `missing / selected > 2%`, in exact integer arithmetic.
pub fn exceeds_missing_limit(missing: usize, selected: usize) -> bool {
    missing * 50 > selected
}

/// One CAL subsample document with a valid observation.
#[derive(Clone, Debug)]
pub struct FitDoc {
    pub id: String,
    pub human: bool,
    pub balanced_weight: f64,
    pub pi_h: f64,
    pub fraction: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PangramMap {
    pub version: String,
    pub knots: Vec<Knot>,
    pub lo: f64,
    pub hi: f64,
}

pub fn bits(t: f64) -> String {
    format!("0x{:016x}", t.to_bits())
}

fn threshold_json(t: f64) -> Value {
    json!({"value": if t.is_finite() { json!(t) } else { json!("inf") }, "bits": bits(t)})
}

impl PangramMap {
    /// Calibrated involvement of a clamped fraction in [0, 1].
    pub fn apply(&self, fraction: f64) -> Result<f64> {
        ensure!(
            fraction.is_finite() && (0.0..=1.0).contains(&fraction),
            "Pangram fraction {fraction} outside [0, 1]"
        );
        Ok(isotonic_predict_score(&self.knots, fraction).clamp(self.lo, self.hi))
    }

    pub fn description(&self) -> Value {
        json!({"method": METHOD, "version": self.version, "input": INPUT,
            "target": "binary_any_AI_involvement_from_documented_origin",
            "algorithm": "exact_score_tie_aggregation_then_weighted_pool_adjacent_violators (involvement_calibration::fit_isotonic_scores)",
            "interpolation": "linear_between_adjacent_unique_CAL_fraction_knots; constant_at_each_end_outside_CAL_support",
            "weighting": WEIGHT_RULE, "bound": CLIP_RULE, "lo": self.lo, "hi": self.hi,
            "lo_bits": bits(self.lo), "hi_bits": bits(self.hi),
            "knots": self.knots.iter().map(|k| json!({"fraction": k.score, "involvement": k.involvement,
                "weight": k.weight, "weighted_positives": k.weighted_positives, "rows": k.rows,
                "fraction_bits": bits(k.score), "involvement_bits": bits(k.involvement)})).collect::<Vec<_>>()})
    }

    /// Load the map of a fitted map record (`map` object of [`MAP_SCHEMA`]).
    pub fn from_description(v: &Value) -> Result<Self> {
        ensure!(
            v["method"] == METHOD && v["input"] == INPUT && v["bound"] == CLIP_RULE && v["weighting"] == WEIGHT_RULE,
            "Pangram map meaning differs"
        );
        let f = |x: &Value, k: &str, b: &str| -> Result<f64> {
            let val = x[k].as_f64().with_context(|| format!("map {k}"))?;
            let bits = x[b].as_str().with_context(|| format!("map {b}"))?;
            let from = f64::from_bits(u64::from_str_radix(bits.trim_start_matches("0x"), 16)?);
            ensure!(from.to_bits() == val.to_bits(), "map {k} value and bits differ");
            Ok(val)
        };
        let (lo, hi) = (f(v, "lo", "lo_bits")?, f(v, "hi", "hi_bits")?);
        ensure!(lo > 0.0 && lo <= 0.5 && hi >= 0.5 && hi < 1.0, "map bounds");
        let mut knots = vec![];
        for k in v["knots"].as_array().context("knots")? {
            let knot = Knot {
                score: f(k, "fraction", "fraction_bits")?,
                involvement: f(k, "involvement", "involvement_bits")?,
                weight: k["weight"].as_f64().context("knot weight")?,
                weighted_positives: k["weighted_positives"].as_f64().context("knot positives")?,
                rows: k["rows"].as_u64().context("knot rows")? as usize,
            };
            ensure!(
                (0.0..=1.0).contains(&knot.score)
                    && knot.involvement >= lo
                    && knot.involvement <= hi
                    && knot.weight > 0.0
                    && knot.rows > 0,
                "invalid knot"
            );
            knots.push(knot);
        }
        ensure!(
            !knots.is_empty()
                && knots
                    .windows(2)
                    .all(|p| p[0].score < p[1].score && p[0].involvement <= p[1].involvement),
            "knots must be sorted and monotone"
        );
        Ok(PangramMap {
            version: v["version"].as_str().context("version")?.to_string(),
            knots,
            lo,
            hi,
        })
    }
}

/// Fit the map on the valid CAL subsample documents (sorted by id for a
/// deterministic summation order within score ties).
pub fn fit(version: &str, docs: &[FitDoc]) -> Result<PangramMap> {
    let mut d: Vec<&FitDoc> = docs.iter().collect();
    d.sort_by(|a, b| a.id.cmp(&b.id));
    for w in d.windows(2) {
        ensure!(w[0].id != w[1].id, "duplicate CAL document {}", w[0].id);
    }
    let n_h = d.iter().filter(|x| x.human).count();
    let n_pos = d.len() - n_h;
    ensure!(n_h > 0 && n_pos > 0, "mechanical failure: empty class (humans {n_h}, positives {n_pos})");
    let mut samples = Vec::with_capacity(d.len());
    for x in &d {
        ensure!(
            x.fraction.is_finite() && (0.0..=1.0).contains(&x.fraction),
            "{}: fraction outside [0, 1]",
            x.id
        );
        ensure!(
            x.balanced_weight.is_finite() && x.balanced_weight > 0.0 && x.pi_h.is_finite() && x.pi_h > 0.0 && x.pi_h <= 1.0,
            "{}: bad weight or inclusion probability",
            x.id
        );
        samples.push((x.fraction, if x.human { 0.0 } else { 1.0 }, x.balanced_weight / x.pi_h));
    }
    let (lo, hi) = (1.0 / (2.0 * n_h as f64), 1.0 - 1.0 / (2.0 * n_pos as f64));
    let mut knots = fit_isotonic_scores(samples)?;
    ensure!(
        knots.iter().all(|k| k.score.is_finite() && k.involvement.is_finite()),
        "mechanical failure: non-finite isotonic value"
    );
    for k in &mut knots {
        k.involvement = k.involvement.clamp(lo, hi);
    }
    Ok(PangramMap { version: version.to_string(), knots, lo, hi })
}

/// Pangram's CAL operating points (0.01 gated rule, 0.001 reported) on the
/// calibrated values of the valid CAL subsample documents; humans unweighted.
pub fn operating_points(map: &PangramMap, docs: &[FitDoc]) -> Result<Value> {
    let mut all = vec![];
    let mut humans = vec![];
    for d in docs {
        let p = map.apply(d.fraction)?;
        all.push(p);
        if d.human {
            humans.push(p);
        }
    }
    let mut out = serde_json::Map::new();
    for a in [ALPHA_1PCT, ALPHA_01PCT] {
        let t = operating_threshold(&all, &humans, a);
        let flagged = humans.iter().filter(|&&x| x >= t).count();
        let mut v = threshold_json(t);
        v["cal_humans"] = json!(humans.len());
        v["cal_humans_flagged"] = json!(flagged);
        v["cal_positives_flagged"] = json!(docs.iter().filter(|d| !d.human).filter(|d| map.apply(d.fraction).unwrap() >= t).count());
        out.insert(a.label.into(), v);
    }
    Ok(Value::Object(out))
}

/// In-sample HT-weighted CAL Brier of the map (diagnostic only; never used to
/// choose anything).
pub fn in_sample_brier(map: &PangramMap, docs: &[FitDoc]) -> Result<f64> {
    let (mut l, mut w) = (0.0, 0.0);
    for d in docs {
        let y = if d.human { 0.0 } else { 1.0 };
        let wt = d.balanced_weight / d.pi_h;
        l += wt * (map.apply(d.fraction)? - y).powi(2);
        w += wt;
    }
    Ok(l / w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp_eval::parse_threshold;

    fn doc(id: &str, human: bool, w: f64, pi: f64, x: f64) -> FitDoc {
        FitDoc { id: id.into(), human, balanced_weight: w, pi_h: pi, fraction: x }
    }

    #[test]
    fn fit_uses_ht_weights_ties_and_clip_bounds() {
        // Two humans and three positives; the tie at 0.4 pools a human (weight
        // 0.1/0.5 = 0.2) and a positive (0.2/1.0 = 0.2) into one knot at 0.5.
        let docs = vec![
            doc("h1", true, 0.3, 1.0, 0.0),
            doc("h2", true, 0.1, 0.5, 0.4),
            doc("p1", false, 0.2, 1.0, 0.4),
            doc("p2", false, 0.2, 1.0, 0.9),
            doc("p3", false, 0.2, 1.0, 1.0),
        ];
        let m = fit("4.1", &docs).unwrap();
        assert_eq!(m.lo, 0.25);
        assert_eq!(m.hi, 1.0 - 1.0 / 6.0);
        assert_eq!(m.knots.len(), 4);
        assert_eq!(m.knots[1].score, 0.4);
        assert!((m.knots[1].weight - 0.4).abs() < 1e-15);
        assert_eq!(m.knots[1].involvement, 0.5);
        // Levels clamped: 0 -> lo, 1 -> hi.
        assert_eq!(m.knots[0].involvement, 0.25);
        assert_eq!(m.knots[3].involvement, m.hi);
        // Interpolation and constant ends, outputs clamped.
        assert_eq!(m.apply(0.0).unwrap(), 0.25);
        assert!((m.apply(0.2).unwrap() - 0.375).abs() < 1e-15);
        assert_eq!(m.apply(1.0).unwrap(), m.hi);
        assert!(m.apply(1.5).is_err());
        // Order invariance.
        let mut rev = docs.clone();
        rev.reverse();
        assert_eq!(fit("4.1", &rev).unwrap(), m);
        // Round trip through the record with exact bits.
        let d = m.description();
        assert_eq!(PangramMap::from_description(&d).unwrap(), m);
        let mut bad = d.clone();
        bad["lo_bits"] = json!(bits(0.3));
        assert!(PangramMap::from_description(&bad).is_err());
        let mut bad = d;
        bad["method"] = json!("isotonic_v1");
        assert!(PangramMap::from_description(&bad).is_err());
        // Empty class is a mechanical failure.
        assert!(fit("4.1", &docs[2..]).is_err());
    }

    #[test]
    fn fit_matches_a_calibrator_on_the_same_samples() {
        use crate::involvement_calibration::isotonic_predict_score;
        let docs: Vec<FitDoc> = (0..40)
            .map(|i| doc(&format!("d{i:02}"), i % 3 == 0, 0.01 + i as f64 * 1e-3, 0.5, ((i * 7) % 11) as f64 / 10.0))
            .collect();
        let m = fit("v", &docs).unwrap();
        let mut d = docs.clone();
        d.sort_by(|a, b| a.id.cmp(&b.id));
        let raw = fit_isotonic_scores(d.iter().map(|x| (x.fraction, if x.human { 0.0 } else { 1.0 }, x.balanced_weight / x.pi_h)).collect()).unwrap();
        for x in [0.0, 0.05, 0.33, 0.7, 1.0] {
            assert_eq!(m.apply(x).unwrap(), isotonic_predict_score(&raw, x).clamp(m.lo, m.hi));
        }
    }

    #[test]
    fn operating_points_follow_protocol_rule_on_valid_humans() {
        // 100 humans at distinct fractions 0.00..0.99 and 10 positives at 1.0.
        let mut docs: Vec<FitDoc> = (0..100).map(|i| doc(&format!("h{i:03}"), true, 1.0, 1.0, i as f64 / 100.0)).collect();
        docs.extend((0..10).map(|i| doc(&format!("p{i}"), false, 1.0, 1.0, 1.0)));
        let m = fit("4.1", &docs).unwrap();
        let ops = operating_points(&m, &docs).unwrap();
        let t = parse_threshold(&ops["0.01"]).unwrap();
        let all: Vec<f64> = docs.iter().map(|d| m.apply(d.fraction).unwrap()).collect();
        let hum: Vec<f64> = docs.iter().filter(|d| d.human).map(|d| m.apply(d.fraction).unwrap()).collect();
        assert_eq!(t.to_bits(), operating_threshold(&all, &hum, ALPHA_1PCT).to_bits());
        assert!(ops["0.01"]["cal_humans_flagged"].as_u64().unwrap() <= 1);
        assert_eq!(ops["0.01"]["cal_humans"], 100);
        assert!(exceeds_missing_limit(13, 600) && !exceeds_missing_limit(12, 600));
        assert!(in_sample_brier(&m, &docs).unwrap() >= 0.0);
    }
}
