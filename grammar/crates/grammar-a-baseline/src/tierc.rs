//! Tier C and Tier I gates of the competitive A protocol v1.2 (`tier_c`,
//! `tier_i`, `pangram_subsample`, `failures_and_missing`, `gated_slices`).
//!
//! Numeric only: bundle metadata ([`crate::comp_eval::Row`]), A predictions
//! ([`crate::comp_eval::Obs`]), CAL-declared thresholds and the CAL-mapped
//! Pangram involvement of the Test subsample. The estimator primitives are the
//! protocol-pinned `pangram_subsample` functions (`balanced_weights`,
//! `ht_group_values`, `bootstrap_interval`, `max_flags`,
//! `clopper_pearson_upper`, `neyman_allocation`, `select`).
//!
//! - **Test draw** ([`draw_test`]): strata domain x origin cell x realised-share
//!   bin over the primary Test documents (populations `primary` and
//!   `frontier`), `W_h` the stratum share of the full primary Test balanced
//!   mass, Neyman allocation with the fixed planning SDs, floor 3, selection
//!   by `selection_key(20261027, "test", id)`.
//! - **Condition 1** ([`noninferiority`]): `v_g = G * sum_{selected d in g}
//!   w_d * Delta_d / pi_h` over all Test groups, `Delta_d = (A_d - y)^2 -
//!   (P_d - y)^2`, A's loss averaged over the seeds for the recipe; a missing
//!   Pangram observation scores `P_d = y`; bound `draws[974] <= 0.010`.
//! - **Condition 2** ([`fpr_gate`]): per seed, flags at the CAL 1% point over
//!   every primary Test human (an execution failure is flagged) against
//!   `max_flags(n, 0.95, 0.025)`.
//! - **Condition 3** ([`slice_gaps`]): the HT gap point estimate of every
//!   gated slice that meets its realised minimum is at most 0.030.
//! - **Condition 4**: no execution failure on a primary Test document.
//! - **Tier I** ([`tier_i`]): the full-Test paired balanced Brier interval
//!   against frozen stage-3 A lies entirely below zero, and human FPR at 0.5
//!   is no higher.
use crate::comp_eval::{GATED_DOMAINS, Obs, Row, SLICE_MIN_FRONTIER_POSITIVES, SLICE_MIN_GROUPS, SLICE_MIN_HUMANS};
use crate::pangram_subsample::{
    self as ps, EvalDoc, STRATUM_FLOOR, SUBSAMPLE_SEED, Stratum, bootstrap_interval,
    clopper_pearson_upper, max_flags, neyman_allocation, select,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const REPORT_SCHEMA: &str = "slop_ninja_a_comp_tierc_report_v1";
pub const N_MIN: usize = 1900;
pub const DELTA: f64 = 0.010;
pub const SLICE_GAP_MAX: f64 = 0.030;
pub const FPR_BOUND: f64 = 0.025;
pub const FPR_LEVEL: f64 = 0.95;
pub const FPR_CUTOFF_TIER_I: f64 = 0.5;

/// Fixed planning SDs of the Test Neyman allocation
/// (`pangram_subsample.allocation.test.planning_sd`), by cell key.
pub const PLANNING_SD: [(&str, f64); 9] = [
    ("human_only", 0.1799081733989233),
    ("mixed:(0,10)", 0.3993781012413245),
    ("mixed:100", 0.06885409166883091),
    ("mixed:[10,25)", 0.32869858006495956),
    ("mixed:[25,50)", 0.15628964917188795),
    ("mixed:[50,75)", 0.07356750060392525),
    ("mixed:[75,90)", 0.07348586856106437),
    ("mixed:[90,100)", 0.07348586856106437),
    ("model_only", 0.05),
];

pub fn planning_sd(cell_key: &str) -> Option<f64> {
    PLANNING_SD.iter().find(|(k, _)| *k == cell_key).map(|x| x.1)
}

/// Check the fixed planning SDs against a protocol JSON
/// (`pangram_subsample.allocation.test.planning_sd`), exactly.
pub fn check_planning_sd(protocol: &Value) -> Result<()> {
    let m = protocol
        .pointer("/pangram_subsample/allocation/test/planning_sd")
        .and_then(Value::as_object)
        .context("protocol planning_sd")?;
    ensure!(m.len() == PLANNING_SD.len(), "planning SD key count differs");
    for (k, v) in PLANNING_SD {
        let p = m.get(k).and_then(Value::as_f64).with_context(|| format!("planning SD {k}"))?;
        ensure!(p.to_bits() == v.to_bits(), "planning SD {k}: protocol {p} != built-in {v}");
    }
    Ok(())
}

/// Cell key of a stratum key `domain|cell`.
pub fn cell_key(stratum: &str) -> Result<&str> {
    stratum.split_once('|').map(|x| x.1).context("stratum key without |")
}

// ---------------------------------------------------------------- Test draw

/// The realised Test draw.
pub struct Draw {
    pub strata: Vec<Stratum>,
    pub allocation: BTreeMap<String, usize>,
    pub selected: BTreeSet<String>,
    /// Full primary Test balanced weights, aligned to the input rows.
    pub weights: Vec<f64>,
    pub n: usize,
}

/// Indices of the primary documents of a split (populations `primary` and
/// `frontier`).
pub fn primary_indices(rows: &[Row]) -> Vec<usize> {
    (0..rows.len()).filter(|&i| rows[i].primary).collect()
}

fn eval_docs(rows: &[Row], idx: &[usize]) -> Result<Vec<EvalDoc>> {
    idx.iter()
        .map(|&i| {
            Ok(EvalDoc {
                group: rows[i].group.clone(),
                cell: rows[i].cell,
                stratum: rows[i].stratum()?,
                value: 0.0,
            })
        })
        .collect()
}

/// Draw the Test subsample from the primary rows `rows` (all must be primary
/// Test documents). `n_test` is the figure bound in the seal; the size is
/// `min(n_test, population)`, and `n_test < n_min` refuses the draw.
pub fn draw_test(rows: &[Row], n_test: usize) -> Result<Draw> {
    ensure!(!rows.is_empty(), "no primary Test documents");
    for r in rows {
        ensure!(r.split == "test" && r.primary, "{}: not a primary Test document", r.id);
    }
    ensure!(
        n_test >= N_MIN,
        "n_test {n_test} below n_min {N_MIN}: no Pangram request, Tier C not evaluated"
    );
    let idx: Vec<usize> = (0..rows.len()).collect();
    let docs = eval_docs(rows, &idx)?;
    let weights = ps::balanced_weights(&docs);
    let mut by: BTreeMap<String, (usize, f64)> = BTreeMap::new();
    for (d, w) in docs.iter().zip(&weights) {
        let e = by.entry(d.stratum.clone()).or_default();
        e.0 += 1;
        e.1 += w;
    }
    let strata: Vec<Stratum> = by
        .iter()
        .map(|(k, (n, w))| {
            let sd = planning_sd(cell_key(k)?).with_context(|| format!("no planning SD for {k}"))?;
            Ok(Stratum { key: k.clone(), population: *n, weight: *w, sd })
        })
        .collect::<Result<_>>()?;
    let n = n_test.min(rows.len());
    let alloc = neyman_allocation(&strata, n, STRATUM_FLOOR)?;
    let allocation: BTreeMap<String, usize> =
        strata.iter().zip(&alloc).map(|(s, &k)| (s.key.clone(), k)).collect();
    let pairs: Vec<(String, String)> =
        rows.iter().zip(&docs).map(|(r, d)| (r.id.clone(), d.stratum.clone())).collect();
    let selected = select(&pairs, &allocation, SUBSAMPLE_SEED, "test")?;
    ensure!(selected.len() == n, "selected {} != {n}", selected.len());
    Ok(Draw { strata, allocation, selected, weights, n })
}

// ---------------------------------------------------------------- scoring

/// One document's A output for the gates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocScore {
    /// Involvement `min(1, p_model_only + p_mixed)` (Brier); `1 - y` on failure.
    pub inv: f64,
    /// Operating-point value `1 - p_human` (the emitted calibrated involvement).
    pub flag: f64,
    pub failed: bool,
}

/// Score a document; a missing prediction is an execution failure.
pub fn a_score(r: &Row, o: Option<&Obs>) -> DocScore {
    match o {
        Some(Obs::Ok(p)) => DocScore {
            inv: (p.p_model_only + p.p_mixed).min(1.0),
            flag: (1.0 - p.p_human).clamp(0.0, 1.0),
            failed: false,
        },
        _ => DocScore { inv: 1.0 - r.y, flag: 1.0 - r.y, failed: true },
    }
}

/// Flag rule: calibrated value `>= t` in f64; a failed human is flagged and a
/// failed positive missed, whatever `t` is.
pub fn flagged(r: &Row, s: &DocScore, t: f64) -> bool {
    if s.failed { r.is_human() } else { s.flag >= t }
}

pub fn loss(r: &Row, s: &DocScore) -> f64 {
    (s.inv - r.y).powi(2)
}

/// One fit of an A system on Test: per-row scores and its CAL thresholds.
pub struct Fit {
    pub seed: u64,
    pub scores: Vec<DocScore>,
    pub t_01: f64,
    pub t_001: f64,
}

impl Fit {
    pub fn new(rows: &[Row], seed: u64, preds: &BTreeMap<String, Obs>, t_01: f64, t_001: f64) -> Result<Self> {
        let ids: BTreeSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        for id in preds.keys() {
            ensure!(ids.contains(id.as_str()), "seed {seed}: prediction {id} is not a Test document");
        }
        ensure!(!t_01.is_nan() && !t_001.is_nan(), "thresholds");
        Ok(Fit { seed, scores: rows.iter().map(|r| a_score(r, preds.get(&r.id))).collect(), t_01, t_001 })
    }
}

/// CAL-mapped Pangram on the Test subsample, aligned to rows: `pi[i]` is the
/// inclusion probability of a selected document, `inv[i]` its calibrated
/// involvement (None: missing, scored at the label).
pub struct PangramTest {
    pub pi: Vec<Option<f64>>,
    pub inv: Vec<Option<f64>>,
    pub t_01: f64,
    pub t_001: f64,
}

impl PangramTest {
    pub fn selected(&self) -> usize {
        self.pi.iter().filter(|p| p.is_some()).count()
    }
    pub fn missing(&self) -> usize {
        self.pi.iter().zip(&self.inv).filter(|(p, v)| p.is_some() && v.is_none()).count()
    }
    fn loss(&self, rows: &[Row], i: usize) -> f64 {
        match self.inv[i] {
            Some(p) => (p - rows[i].y).powi(2),
            None => 0.0,
        }
    }
}

// ---------------------------------------------------------------- estimators

/// HT group values over the documents `set`: balanced weights over `set`
/// (`pangram_subsample::balanced_weights`), and `v_g = G * sum_{selected d in
/// g} w_d * delta_d / pi_d` in group-key order (0 for a group without a
/// selected document). With `pi = 1` for every document this is the plain
/// balanced group value.
pub fn ht_values(rows: &[Row], set: &[usize], pi: &dyn Fn(usize) -> Option<f64>, delta: &dyn Fn(usize) -> f64) -> Result<Vec<(String, f64)>> {
    let docs = eval_docs(rows, set)?;
    let w = ps::balanced_weights(&docs);
    let mut m: BTreeMap<&str, f64> = BTreeMap::new();
    for &i in set {
        m.entry(rows[i].group.as_str()).or_default();
    }
    let g = m.len() as f64;
    for (k, &i) in set.iter().enumerate() {
        if let Some(p) = pi(i) {
            ensure!(p > 0.0 && p <= 1.0, "bad inclusion probability {p}");
            *m.get_mut(rows[i].group.as_str()).unwrap() += g * (w[k] / p) * delta(i);
        }
    }
    Ok(m.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn interval_json(values: &[(String, f64)]) -> Value {
    let v: Vec<f64> = values.iter().map(|x| x.1).collect();
    let est = v.iter().sum::<f64>() / v.len() as f64;
    let ci = bootstrap_interval(&v);
    json!({"estimate": est, "lower_draws_24": ci.map(|c| c.0), "upper_draws_974": ci.map(|c| c.1),
           "source_groups": v.len(),
           "bootstrap": "pangram_subsample::bootstrap_interval over all group values: 1000 draws, xorshift64 seed 20261026, draws[24], draws[974]"})
}

/// Recipe or released variant: which fits of the A system enter `Delta_d`.
#[derive(Clone, Copy, Debug)]
pub enum Variant {
    Recipe,
    Released(u64),
}

impl Variant {
    pub fn name(&self) -> &'static str {
        match self {
            Variant::Recipe => "recipe",
            Variant::Released(_) => "released_artifact",
        }
    }
    pub fn fits<'a>(&self, fits: &'a [Fit]) -> Result<Vec<&'a Fit>> {
        Ok(match self {
            Variant::Recipe => fits.iter().collect(),
            Variant::Released(s) => {
                let f: Vec<&Fit> = fits.iter().filter(|f| f.seed == *s).collect();
                match f.len() {
                    1 => f,
                    0 if fits.len() == 1 => vec![&fits[0]],
                    _ => bail!("released seed {s} not uniquely available"),
                }
            }
        })
    }
}

fn mean_loss(rows: &[Row], fits: &[&Fit], i: usize) -> f64 {
    fits.iter().map(|f| loss(&rows[i], &f.scores[i])).sum::<f64>() / fits.len() as f64
}

/// Condition 1: HT estimate and protocol interval of the full primary Test
/// balanced Brier difference A minus Pangram; cross-checked against
/// `pangram_subsample::ht_group_values` (which recomputes `pi_h` from the
/// selection flags).
pub fn noninferiority(rows: &[Row], fits: &[&Fit], p: &PangramTest) -> Result<Value> {
    let pop = primary_indices(rows);
    let delta = |i: usize| mean_loss(rows, fits, i) - p.loss(rows, i);
    let v = ht_values(rows, &pop, &|i| p.pi[i], &delta)?;
    // Cross-check with the pinned module's own HT group values.
    let docs: Vec<EvalDoc> = pop
        .iter()
        .map(|&i| {
            Ok(EvalDoc {
                group: rows[i].group.clone(),
                cell: rows[i].cell,
                stratum: rows[i].stratum()?,
                value: if p.pi[i].is_some() { delta(i) } else { 0.0 },
            })
        })
        .collect::<Result<_>>()?;
    let sampled: Vec<bool> = pop.iter().map(|&i| p.pi[i].is_some()).collect();
    let pinned = ps::ht_group_values(&docs, &sampled)?;
    ensure!(pinned.len() == v.len(), "HT cross-check group count");
    let mut max_diff: f64 = 0.0;
    for (a, b) in v.iter().zip(&pinned) {
        ensure!(a.0 == b.0, "HT cross-check group order");
        max_diff = max_diff.max((a.1 - b.1).abs());
    }
    ensure!(max_diff <= 1e-9, "HT cross-check differs by {max_diff}");
    let mut out = interval_json(&v);
    let upper = out["upper_draws_974"].as_f64();
    out["pass"] = json!(upper.is_some_and(|u| u <= DELTA));
    out["bound"] = json!({"delta": DELTA, "rule": "draws[974] <= delta"});
    out["pinned_module_cross_check_max_abs_group_difference"] = json!(max_diff);
    out["documents"] = json!(pop.len());
    out["selected_documents"] = json!(p.selected());
    out["pangram_missing_scored_at_label"] = json!(p.missing());
    Ok(out)
}

/// Condition 2 for one fit: flags at the CAL 1% point over every primary
/// Test human; also the reported 0.1% point and TPR.
pub fn fpr_gate(rows: &[Row], f: &Fit) -> Value {
    let pop = primary_indices(rows);
    let humans: Vec<usize> = pop.iter().copied().filter(|&i| rows[i].is_human()).collect();
    let positives: Vec<usize> = pop.iter().copied().filter(|&i| !rows[i].is_human()).collect();
    let n = humans.len();
    let at = |t: f64| {
        let k = humans.iter().filter(|&&i| flagged(&rows[i], &f.scores[i], t)).count();
        let failed = humans.iter().filter(|&&i| f.scores[i].failed).count();
        let tp = positives.iter().filter(|&&i| flagged(&rows[i], &f.scores[i], t)).count();
        let limit = (n > 0).then(|| max_flags(n as u64, FPR_LEVEL, FPR_BOUND)).flatten();
        let cp = (n > 0).then(|| clopper_pearson_upper(k as u64, n as u64, FPR_LEVEL));
        json!({"threshold": {"value": if t.is_finite() { json!(t) } else { json!("inf") }, "bits": format!("0x{:016x}", t.to_bits())},
               "human_flags": k, "humans": n, "failed_humans_counted_flagged": failed,
               "fpr": (n > 0).then(|| k as f64 / n as f64), "clopper_pearson_upper_one_sided_95": cp,
               "max_flags": limit, "pass": limit.is_some_and(|l| k as u64 <= l) && cp.is_some_and(|c| c <= FPR_BOUND),
               "tpr": (!positives.is_empty()).then(|| tp as f64 / positives.len() as f64),
               "positives_flagged": tp, "positives": positives.len()})
    };
    let gated = at(f.t_01);
    let reported = at(f.t_001);
    json!({"seed": f.seed, "alpha_0.01": gated, "alpha_0.001_reported": reported,
           "human_fpr_at_0_5": at(FPR_CUTOFF_TIER_I)})
}

/// Membership of the protocol's gated slices over the primary rows.
pub fn gated_slice_members(rows: &[Row], held_out: &BTreeSet<String>) -> Vec<(String, Vec<usize>)> {
    let pop = primary_indices(rows);
    let humans: Vec<usize> = pop.iter().copied().filter(|&i| rows[i].is_human()).collect();
    let mut out: Vec<(String, Vec<usize>)> = GATED_DOMAINS
        .iter()
        .map(|d| (format!("domain:{d}"), pop.iter().copied().filter(|&i| rows[i].domain == *d).collect()))
        .collect();
    let with_humans = |f: &dyn Fn(&Row) -> bool| {
        let mut v: Vec<usize> = pop.iter().copied().filter(|&i| !rows[i].is_human() && f(&rows[i])).collect();
        v.extend(&humans);
        v.sort();
        v
    };
    out.push((
        "held_out_generator:all".into(),
        with_humans(&|r| r.label.as_ref().is_some_and(|l| held_out.contains(l))),
    ));
    out.push(("frontier:all".into(), with_humans(&|r| r.is_frontier())));
    out
}

/// Realised minimum from pipeline counts: 15 groups and 150 humans; the
/// frontier slice also 150 frontier positives.
pub fn realised_status(rows: &[Row], name: &str, idx: &[usize]) -> Value {
    let groups: BTreeSet<&str> = idx.iter().map(|&i| rows[i].group.as_str()).collect();
    let humans = idx.iter().filter(|&&i| rows[i].is_human()).count();
    let frontier = idx.iter().filter(|&&i| rows[i].is_frontier()).count();
    let mut shortfall = vec![];
    if idx.is_empty() {
        shortfall.push("slice has no documents".to_string());
    }
    if groups.len() < SLICE_MIN_GROUPS {
        shortfall.push(format!("source_groups {} < {SLICE_MIN_GROUPS}", groups.len()));
    }
    if humans < SLICE_MIN_HUMANS {
        shortfall.push(format!("human_documents {humans} < {SLICE_MIN_HUMANS}"));
    }
    if name == "frontier:all" && frontier < SLICE_MIN_FRONTIER_POSITIVES {
        shortfall.push(format!("frontier_positives {frontier} < {SLICE_MIN_FRONTIER_POSITIVES}"));
    }
    json!({"documents": idx.len(), "source_groups": groups.len(), "human_documents": humans,
           "frontier_positives": frontier, "status": if shortfall.is_empty() { "gated" } else { "reported_ungated" },
           "shortfall": shortfall})
}

/// Condition 3: per gated slice, the HT gap point estimate (slice's own
/// documents, groups and balanced weights, each document's full-Test `pi_h`)
/// and its interval (reported, not gated).
pub fn slice_gaps(rows: &[Row], fits: &[&Fit], p: &PangramTest, held_out: &BTreeSet<String>) -> Result<Value> {
    let mut list = vec![];
    let mut pass = true;
    for (name, idx) in gated_slice_members(rows, held_out) {
        let st = realised_status(rows, &name, &idx);
        let gated = st["status"] == "gated";
        let mut v = json!({"slice": name, "realised": st});
        if !idx.is_empty() && idx.iter().any(|&i| p.pi[i].is_some()) {
            let delta = |i: usize| mean_loss(rows, fits, i) - p.loss(rows, i);
            let vals = ht_values(rows, &idx, &|i| p.pi[i], &delta)?;
            let iv = interval_json(&vals);
            let est = iv["estimate"].as_f64().unwrap();
            let ok = est <= SLICE_GAP_MAX;
            v["gap_point_estimate"] = json!(est);
            v["interval_reported_not_gated"] = json!([iv["lower_draws_24"], iv["upper_draws_974"]]);
            v["selected_documents"] = json!(idx.iter().filter(|&&i| p.pi[i].is_some()).count());
            v["within_0_030"] = json!(ok);
            if gated && !ok {
                pass = false;
            }
        } else if gated {
            // A gated slice without any selected document cannot be estimated.
            v["gap_point_estimate"] = Value::Null;
            pass = false;
        }
        v["gated"] = json!(gated);
        list.push(v);
    }
    Ok(json!({"max_gap": SLICE_GAP_MAX, "pass": pass, "slices": list}))
}

/// Tier I for one variant: paired full-Test balanced Brier interval of A
/// minus the frozen stage-3 A entirely below zero, and human FPR at 0.5 no
/// higher (recipe: flag counts summed over the seeds, compared as rates).
pub fn tier_i(rows: &[Row], a: &[&Fit], b: &[&Fit]) -> Result<Value> {
    let pop = primary_indices(rows);
    let delta = |i: usize| mean_loss(rows, a, i) - mean_loss(rows, b, i);
    let v = ht_values(rows, &pop, &|_| Some(1.0), &delta)?;
    let mut brier = interval_json(&v);
    let upper = brier["upper_draws_974"].as_f64();
    brier["pass"] = json!(upper.is_some_and(|u| u < 0.0));
    brier["rule"] = json!("draws[974] < 0 (interval entirely below zero)");
    let humans: Vec<usize> = pop.iter().copied().filter(|&i| rows[i].is_human()).collect();
    let flags = |fits: &[&Fit]| -> usize {
        fits.iter()
            .map(|f| humans.iter().filter(|&&i| flagged(&rows[i], &f.scores[i], FPR_CUTOFF_TIER_I)).count())
            .sum()
    };
    let (fa, fb) = (flags(a), flags(b));
    let n = humans.len();
    let fpr_ok = fa * b.len() <= fb * a.len();
    Ok(json!({
        "brier_difference_a_minus_frozen": brier,
        "human_fpr_at_0_5": {"a_flags_summed_over_fits": fa, "a_fits": a.len(), "frozen_flags_summed_over_fits": fb,
            "frozen_fits": b.len(), "humans": n,
            "a_fpr_mean": (n > 0).then(|| fa as f64 / (n * a.len()) as f64),
            "frozen_fpr_mean": (n > 0).then(|| fb as f64 / (n * b.len()) as f64),
            "pass": fpr_ok, "rule": "A's mean human FPR at involvement >= 0.5 over its fits <= the frozen model's"},
        "pass": brier["pass"] == true && fpr_ok,
    }))
}

/// Condition 4: execution failures on primary Test documents.
pub fn execution(rows: &[Row], fits: &[&Fit]) -> Value {
    let pop = primary_indices(rows);
    let per: Vec<Value> = fits
        .iter()
        .map(|f| json!({"seed": f.seed, "failures": pop.iter().filter(|&&i| f.scores[i].failed).count()}))
        .collect();
    let total: usize = per.iter().map(|x| x["failures"].as_u64().unwrap() as usize).sum();
    json!({"primary_documents": pop.len(), "per_seed": per, "pass": total == 0})
}

/// Pangram FPR and TPR at its own CAL-derived points on the Test subsample
/// documents with valid observations (reported, not gated).
pub fn pangram_rates(rows: &[Row], p: &PangramTest) -> Value {
    let at = |t: f64| {
        let (mut h, mut hf, mut q, mut qf) = (0usize, 0usize, 0usize, 0usize);
        for (i, r) in rows.iter().enumerate() {
            let (Some(_), Some(v)) = (p.pi[i], p.inv[i]) else { continue };
            if r.is_human() {
                h += 1;
                hf += (v >= t) as usize;
            } else {
                q += 1;
                qf += (v >= t) as usize;
            }
        }
        json!({"threshold_bits": format!("0x{:016x}", t.to_bits()),
               "human_flags": hf, "humans_observed": h, "fpr": (h > 0).then(|| hf as f64 / h as f64),
               "clopper_pearson_upper_one_sided_95": (h > 0).then(|| clopper_pearson_upper(hf as u64, h as u64, FPR_LEVEL)),
               "positives_flagged": qf, "positives_observed": q, "tpr": (q > 0).then(|| qf as f64 / q as f64)})
    };
    json!({"population": "Test subsample documents with valid pinned-version observations (unweighted counts)",
           "alpha_0.01": at(p.t_01), "alpha_0.001": at(p.t_001)})
}

/// CAL-declared operating points `(t_0.01, t_0.001)` of one fit, from either
/// a thresholds file (`comp_eval::parse_operating_points`) or a CAL
/// `evaluation.json` of the competitive evaluator (split `cal`, the in-split
/// CAL rule, value and bits agreeing). `system` and `seed` select the fit of
/// an evaluation record; a record with one system accepts any name.
pub fn load_operating_points(v: &Value, system: &str, seed: u64) -> Result<(f64, f64)> {
    use crate::comp_eval::{EVAL_SCHEMA, parse_operating_points, parse_threshold};
    if v["schema"] == EVAL_SCHEMA {
        ensure!(v["split"] == "cal", "operating points must come from a CAL evaluation");
        let systems = v["systems"].as_object().context("systems")?;
        let sys = match systems.get(system) {
            Some(s) => s,
            None if systems.len() == 1 => systems.values().next().unwrap(),
            None => bail!("CAL evaluation has no system {system}"),
        };
        let ops = sys
            .pointer(&format!("/seeds/{seed}/operating_points"))
            .with_context(|| format!("CAL evaluation has no seed {seed}"))?;
        let get = |a: &str| -> Result<f64> {
            let th = &ops[a]["threshold"];
            ensure!(th["source"] == "in_split_cal_rule", "threshold {a} is not the in-split CAL rule");
            ensure!(th["bits"].is_string(), "threshold {a} without bits");
            parse_threshold(th)
        };
        return Ok((get("0.01")?, get("0.001")?));
    }
    let m = parse_operating_points(v)?;
    Ok((
        *m.get("0.01").context("operating point 0.01")?,
        *m.get("0.001").context("operating point 0.001")?,
    ))
}

/// Why Tier C is not evaluated, if it is not.
#[derive(Clone, Debug, Default)]
pub struct NotEvaluated(pub Vec<String>);

/// Tier C and Tier I verdicts for the claimant's fits against Pangram and
/// the frozen stage-3 A.
pub fn verdicts(
    rows: &[Row],
    claimant: &[Fit],
    released_seed: u64,
    frozen: &[Fit],
    pangram: Option<&PangramTest>,
    not_evaluated: &NotEvaluated,
    held_out: &BTreeSet<String>,
) -> Result<Value> {
    ensure!(rows.iter().all(|r| r.split == "test"), "Tier C runs on Test rows only");
    let mut reasons = not_evaluated.0.clone();
    if let Some(p) = pangram {
        let (m, s) = (p.missing(), p.selected());
        if crate::pangram_map::exceeds_missing_limit(m, s) {
            reasons.push(format!("Pangram missing on {m} of {s} selected primary Test documents (> 2%)"));
        }
    } else if reasons.is_empty() {
        reasons.push("no Pangram Test observations".into());
    }
    let variants = [Variant::Recipe, Variant::Released(released_seed)];
    let mut c_out = serde_json::Map::new();
    let mut i_out = serde_json::Map::new();
    let mut c_all = true;
    let mut i_all = true;
    for v in variants {
        let fits = v.fits(claimant)?;
        let base = v.fits(frozen)?;
        let fpr: Vec<Value> = fits.iter().map(|f| fpr_gate(rows, f)).collect();
        let fpr_pass = fpr.iter().all(|x| x["alpha_0.01"]["pass"] == true);
        let headline = fpr.iter().filter_map(|x| x["alpha_0.01"]["human_flags"].as_u64()).max();
        let exec = execution(rows, &fits);
        let mut c = json!({
            "condition_2_human_fpr": {"pass": fpr_pass, "headline_largest_flag_count": headline, "per_seed": fpr},
            "condition_4_execution": exec,
        });
        if reasons.is_empty() {
            let p = pangram.unwrap();
            let c1 = noninferiority(rows, &fits, p)?;
            let c3 = slice_gaps(rows, &fits, p, held_out)?;
            let pass = c1["pass"] == true && fpr_pass && c3["pass"] == true && c["condition_4_execution"]["pass"] == true;
            c["condition_1_noninferiority"] = c1;
            c["condition_3_slices"] = c3;
            c["status"] = json!(if pass { "pass" } else { "fail" });
            c_all &= pass;
        } else {
            c["status"] = json!("not_evaluated");
        }
        c["fits"] = json!(fits.iter().map(|f| f.seed).collect::<Vec<_>>());
        c_out.insert(v.name().into(), c);
        let t = tier_i(rows, &fits, &base)?;
        i_all &= t["pass"] == true;
        let mut t = t;
        t["fits"] = json!(fits.iter().map(|f| f.seed).collect::<Vec<_>>());
        t["frozen_fits"] = json!(base.iter().map(|f| f.seed).collect::<Vec<_>>());
        i_out.insert(v.name().into(), t);
    }
    let tier_c = if !reasons.is_empty() {
        json!({"status": "not_evaluated", "reasons": reasons, "variants": c_out})
    } else {
        json!({"status": if c_all { "pass" } else { "fail" }, "rule": "all four conditions for the recipe and for the released artifact", "variants": c_out})
    };
    Ok(json!({
        "tier_c": tier_c,
        "tier_i": {"status": if i_all { "pass" } else { "fail" }, "rule": "both gates for the recipe and for the released artifact", "variants": i_out},
        "pangram_rates_at_own_cal_points": pangram.map(|p| pangram_rates(rows, p)),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp_eval::{Prediction, parse_rows};

    fn row(id: &str, g: &str, domain: &str, origin: &str, share: f64, pop: &str, label: Option<&str>) -> Value {
        json!({"id": id, "split": "test", "origin": origin, "share": share, "source_group": g,
               "domain": domain, "population": pop, "generator_label": label,
               "operation": if origin == "human_only" { "human" } else if origin == "model_only" { "mirror" } else { "span" }})
    }

    fn pred(inv: f64) -> Obs {
        Obs::Ok(Prediction {
            p_human: 1.0 - inv,
            p_model_only: inv,
            p_mixed: 0.0,
            share: inv,
            attribution: BTreeMap::new(),
            roles: BTreeMap::new(),
        })
    }

    /// `groups` groups in `domains`, each with a human, a mirror and a mixed doc.
    fn rows(groups: usize, domains: &[&str]) -> Vec<Row> {
        let mut s = String::new();
        for g in 0..groups {
            let d = domains[g % domains.len()];
            s += &format!("{}\n", row(&format!("h{g}"), &format!("g{g}"), d, "human_only", 0.0, "primary", None));
            s += &format!("{}\n", row(&format!("m{g}"), &format!("g{g}"), d, "model_only", 1.0, "primary", Some("olmo2")));
            s += &format!("{}\n", row(&format!("x{g}"), &format!("g{g}"), d, "mixed", 0.05 + (g % 9) as f64 * 0.1, "primary", Some("glm4")));
        }
        parse_rows(&s).unwrap()
    }

    fn fit(rows: &[Row], seed: u64, f: impl Fn(&Row) -> f64) -> Fit {
        let p: BTreeMap<String, Obs> = rows.iter().map(|r| (r.id.clone(), pred(f(r)))).collect();
        Fit::new(rows, seed, &p, 0.9, 0.99).unwrap()
    }

    #[test]
    fn draw_uses_planning_sds_floor_and_selection_key() {
        let r = rows(60, &["wikipedia", "plos"]);
        assert!(draw_test(&r, 100).is_err(), "below n_min");
        let mut big = String::new();
        for g in 0..3000 {
            let d = ["wikipedia", "plos"][g % 2];
            big += &format!("{}\n", row(&format!("h{g}"), &format!("g{g}"), d, "human_only", 0.0, "primary", None));
            big += &format!("{}\n", row(&format!("m{g}"), &format!("g{g}"), d, "model_only", 1.0, "primary", Some("a")));
            big += &format!("{}\n", row(&format!("x{g}"), &format!("g{g}"), d, "mixed", if g % 50 == 0 { 0.05 } else { 0.3 }, "frontier", Some("b")));
        }
        let r = parse_rows(&big).unwrap();
        let d = draw_test(&r, 1900).unwrap();
        assert_eq!(d.selected.len(), 1900);
        assert_eq!(d.allocation.values().sum::<usize>(), 1900);
        // Seven strata: two domains x (human_only, model_only, mixed:[25,50)),
        // plus mixed:(0,10), which only wikipedia holds (frontier rows share
        // their domain's strata).
        assert_eq!(d.strata.len(), 7);
        let wsum: f64 = d.strata.iter().map(|s| s.weight).sum();
        assert!((wsum - 1.0).abs() < 1e-12);
        // Independent strata: N_h, W_h (group mass 1/3000, cell mass 1/3) and S_h.
        let w = 1.0 / 3000.0 / 3.0;
        let want = vec![
            Stratum { key: "plos|human_only".into(), population: 1500, weight: 1500.0 * w, sd: 0.1799081733989233 },
            Stratum { key: "plos|mixed:[25,50)".into(), population: 1500, weight: 1500.0 * w, sd: 0.15628964917188795 },
            Stratum { key: "plos|model_only".into(), population: 1500, weight: 1500.0 * w, sd: 0.05 },
            Stratum { key: "wikipedia|human_only".into(), population: 1500, weight: 1500.0 * w, sd: 0.1799081733989233 },
            Stratum { key: "wikipedia|mixed:(0,10)".into(), population: 60, weight: 60.0 * w, sd: 0.3993781012413245 },
            Stratum { key: "wikipedia|mixed:[25,50)".into(), population: 1440, weight: 1440.0 * w, sd: 0.15628964917188795 },
            Stratum { key: "wikipedia|model_only".into(), population: 1500, weight: 1500.0 * w, sd: 0.05 },
        ];
        for (a, b) in d.strata.iter().zip(&want) {
            assert_eq!((&a.key, a.population, a.sd.to_bits()), (&b.key, b.population, b.sd.to_bits()));
            assert!((a.weight - b.weight).abs() < 1e-12);
        }
        let expect = neyman_allocation(&want, 1900, 3).unwrap();
        assert_eq!(d.allocation.values().copied().collect::<Vec<_>>(), expect);
        // Uncapped Neyman: n_h / n_m close to S_h / S_m at equal W.
        let h = d.allocation["plos|human_only"] as f64;
        let m = d.allocation["plos|model_only"] as f64;
        assert!((h / m - 0.1799081733989233 / 0.05).abs() < 0.05, "{h} {m}");
        // Deterministic; n_test above the population is clamped.
        let again = draw_test(&r, 1900).unwrap();
        assert_eq!(again.selected, d.selected);
        let all = draw_test(&r, 10_000).unwrap();
        assert_eq!(all.n, r.len());
        // Selection is the smallest keys within each stratum.
        let key = |id: &str| ps::selection_key(SUBSAMPLE_SEED, "test", id);
        let mut hum: Vec<&Row> = r.iter().filter(|x| x.stratum().unwrap() == "plos|human_only").collect();
        hum.sort_by_key(|x| key(&x.id));
        let n = d.allocation["plos|human_only"];
        assert!(hum[..n].iter().all(|x| d.selected.contains(&x.id)));
        assert!(hum[n..].iter().all(|x| !d.selected.contains(&x.id)));
        assert!(planning_sd("mixed:[90,100)").is_some() && planning_sd("mixed").is_none());
    }

    #[test]
    fn flag_and_failure_rules() {
        let r = rows(2, &["wikipedia"]);
        let h = &r[0];
        let s = a_score(h, None);
        assert!(s.failed && s.inv == 1.0);
        assert!(flagged(h, &s, f64::INFINITY), "failed human flagged even at +inf");
        let m = &r[1];
        let s = a_score(m, Some(&Obs::Failed("timeout".into())));
        assert!(s.failed && s.inv == 0.0 && !flagged(m, &s, 0.0));
        assert_eq!(loss(m, &s), 1.0);
        // The flag reads 1 - p_human.
        let s = a_score(h, Some(&pred(0.9)));
        assert_eq!(s.flag, 1.0 - (1.0 - 0.9));
        assert!(flagged(h, &s, s.flag) && !flagged(h, &s, f64::from_bits(s.flag.to_bits() + 1)));
    }

    fn pangram(r: &[Row], every: usize, f: impl Fn(&Row) -> Option<f64>) -> PangramTest {
        // Select every document in strata order (pi from the selection).
        let mut by: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        let sel: Vec<bool> = (0..r.len()).map(|i| i % every == 0 || every == 1).collect();
        for (i, x) in r.iter().enumerate() {
            let e = by.entry(x.stratum().unwrap()).or_default();
            e.0 += 1;
            e.1 += sel[i] as usize;
        }
        PangramTest {
            pi: r.iter().enumerate().map(|(i, x)| sel[i].then(|| { let (n, s) = by[&x.stratum().unwrap()]; s as f64 / n as f64 })).collect(),
            inv: r.iter().enumerate().map(|(i, x)| if sel[i] { f(x) } else { None }).collect(),
            t_01: 0.9,
            t_001: 0.99,
        }
    }

    #[test]
    fn full_selection_ht_equals_full_balanced_difference_and_gates() {
        let r = rows(40, &["wikipedia", "plos"]);
        let a = fit(&r, 17, |x| if x.is_human() { 0.1 } else { 0.8 });
        let p = pangram(&r, 1, |x| Some(if x.is_human() { 0.2 } else { 0.9 }));
        let v = noninferiority(&r, &[&a], &p).unwrap();
        // A: humans 0.01, positives 0.04; Pangram 0.04 and 0.01. Balanced mass:
        // humans 1/3 and positives 2/3 per group -> 1/3*(-0.03) + 2/3*(0.03) = 0.01.
        assert!((v["estimate"].as_f64().unwrap() - 0.01).abs() < 1e-12);
        // Constant per-group value: degenerate interval at the estimate.
        assert!((v["upper_draws_974"].as_f64().unwrap() - 0.01).abs() < 1e-12);
        assert!(v["pinned_module_cross_check_max_abs_group_difference"].as_f64().unwrap() < 1e-15);
        // Missing Pangram scores at the label: the difference grows by A's loss there.
        let pm = pangram(&r, 1, |x| if x.is_human() { None } else { Some(0.9) });
        let w = noninferiority(&r, &[&a], &pm).unwrap();
        assert!((w["estimate"].as_f64().unwrap() - (1.0 / 3.0 * 0.01 + 2.0 / 3.0 * 0.03)).abs() < 1e-12);
        assert_eq!(pm.missing(), 40);
    }

    #[test]
    fn ht_is_unbiased_over_every_one_per_stratum_sample() {
        // Two domains x 3 cells, 2 documents per stratum: every one-per-stratum
        // sample (2^6) averages to the full value.
        let mut s = String::new();
        let mut k = 0;
        for d in ["wikipedia", "plos"] {
            for g in 0..2 {
                let gid = format!("{d}{g}");
                s += &format!("{}\n", row(&format!("h{k}"), &gid, d, "human_only", 0.0, "primary", None));
                s += &format!("{}\n", row(&format!("m{k}"), &gid, d, "model_only", 1.0, "primary", Some("a")));
                s += &format!("{}\n", row(&format!("x{k}"), &gid, d, "mixed", 0.3, "primary", Some("a")));
                k += 1;
            }
        }
        let r = parse_rows(&s).unwrap();
        let a = fit(&r, 17, |x| (x.id.len() as f64 * 0.07 + x.share * 0.3).min(1.0));
        let pv = |x: &Row| ((x.id.as_bytes()[1] - b'0') as f64 * 0.2 + 0.1).min(1.0);
        let full = noninferiority(&r, &[&a], &pangram(&r, 1, |x| Some(pv(x)))).unwrap()["estimate"].as_f64().unwrap();
        let strata: BTreeMap<String, Vec<usize>> = r.iter().enumerate().fold(BTreeMap::new(), |mut m, (i, x)| {
            m.entry(x.stratum().unwrap()).or_insert_with(Vec::new).push(i);
            m
        });
        let st: Vec<&Vec<usize>> = strata.values().collect();
        let (mut sum, mut n) = (0.0, 0.0);
        for mask in 0..(1u32 << st.len()) {
            let mut pi = vec![None; r.len()];
            let mut inv = vec![None; r.len()];
            for (j, members) in st.iter().enumerate() {
                let i = members[((mask >> j) & 1) as usize];
                pi[i] = Some(0.5);
                // Some selected documents are missing: scored at the label.
                inv[i] = if i % 5 == 0 { None } else { Some(pv(&r[i])) };
            }
            let p = PangramTest { pi, inv, t_01: 0.9, t_001: 0.99 };
            sum += noninferiority(&r, &[&a], &p).unwrap()["estimate"].as_f64().unwrap();
            n += 1.0;
        }
        // Expected value equals the full-sample value with the same missing rule.
        let mut pi = vec![Some(1.0); r.len()];
        let inv: Vec<Option<f64>> = (0..r.len()).map(|i| if i % 5 == 0 { None } else { Some(pv(&r[i])) }).collect();
        pi.iter_mut().for_each(|x| *x = Some(1.0));
        let truth = noninferiority(&r, &[&a], &PangramTest { pi, inv, t_01: 0.9, t_001: 0.99 }).unwrap()["estimate"].as_f64().unwrap();
        assert!((sum / n - truth).abs() < 1e-12, "{} vs {truth}", sum / n);
        assert!(full.is_finite());
    }

    #[test]
    fn fpr_gate_uses_max_flags_and_counts_failures() {
        let r = rows(1600, &["wikipedia"]);
        // 29 flagged humans pass at 1,600 humans (protocol table); 30 fail.
        let mk = |k: usize, fail: usize| {
            let mut p: BTreeMap<String, Obs> = BTreeMap::new();
            for (j, x) in r.iter().enumerate() {
                let g = j / 3;
                if x.is_human() && g < fail {
                    continue; // missing prediction = failure, counted flagged
                }
                p.insert(x.id.clone(), pred(if x.is_human() && g < k { 0.95 } else if x.is_human() { 0.1 } else { 0.97 }));
            }
            Fit::new(&r, 17, &p, 0.9, 0.99).unwrap()
        };
        let ok = fpr_gate(&r, &mk(29, 0));
        assert_eq!(ok["alpha_0.01"]["max_flags"], 29);
        assert_eq!(ok["alpha_0.01"]["pass"], true);
        assert_eq!(fpr_gate(&r, &mk(30, 0))["alpha_0.01"]["pass"], false);
        // 25 flagged by value plus 5 failures (failures are flagged): 30 fail.
        let f = fpr_gate(&r, &mk(30, 5));
        assert_eq!(f["alpha_0.01"]["human_flags"], 30);
        assert_eq!(f["alpha_0.01"]["failed_humans_counted_flagged"], 5);
        assert_eq!(f["alpha_0.01"]["pass"], false);
        assert_eq!(f["alpha_0.01"]["tpr"], 1.0);
        assert_eq!(execution(&r, &[&mk(0, 1)])["pass"], false);
        assert_eq!(execution(&r, &[&mk(0, 0)])["pass"], true);
    }

    #[test]
    fn slices_realised_minimum_and_gap_gate() {
        // wikipedia: 160 groups; plos: 10 groups (below minimum, ungated).
        let mut s = String::new();
        for g in 0..170 {
            let d = if g < 160 { "wikipedia" } else { "plos" };
            s += &format!("{}\n", row(&format!("h{g}"), &format!("g{g}"), d, "human_only", 0.0, "primary", None));
            s += &format!("{}\n", row(&format!("m{g}"), &format!("g{g}"), d, "model_only", 1.0, "primary", Some("olmo2")));
            s += &format!("{}\n", row(&format!("f{g}"), &format!("g{g}"), d, "model_only", 1.0, "frontier", Some("gpt-x")));
        }
        let r = parse_rows(&s).unwrap();
        let held: BTreeSet<String> = ["olmo2".to_string()].into();
        let members = gated_slice_members(&r, &held);
        let get = |n: &str| members.iter().find(|x| x.0 == n).unwrap().1.clone();
        assert_eq!(realised_status(&r, "domain:wikipedia", &get("domain:wikipedia"))["status"], "gated");
        assert_eq!(realised_status(&r, "domain:plos", &get("domain:plos"))["status"], "reported_ungated");
        assert_eq!(realised_status(&r, "frontier:all", &get("frontier:all"))["status"], "gated");
        assert_eq!(get("held_out_generator:all").len(), 340);
        assert_eq!(realised_status(&r, "domain:reddit", &get("domain:reddit"))["status"], "reported_ungated");
        // A is worse than Pangram by 0.08 on plos positives only (half the
        // group mass: gap 0.04): the ungated plos slice exceeds 0.030 but the
        // gate holds.
        let a = fit(&r, 17, |x| if x.domain == "plos" && !x.is_human() { 1.0 - 0.08f64.sqrt() } else if x.is_human() { 0.0 } else { 1.0 });
        let p = pangram(&r, 1, |x| Some(if x.is_human() { 0.0 } else { 1.0 }));
        let g = slice_gaps(&r, &[&a], &p, &held).unwrap();
        assert_eq!(g["pass"], true);
        let plos = g["slices"].as_array().unwrap().iter().find(|x| x["slice"] == "domain:plos").unwrap();
        assert!(plos["gap_point_estimate"].as_f64().unwrap() > 0.03 && plos["gated"] == false);
        // The same on wikipedia fails the gate.
        let a = fit(&r, 17, |x| if x.domain == "wikipedia" && !x.is_human() { 1.0 - 0.1f64.sqrt() } else if x.is_human() { 0.0 } else { 1.0 });
        assert_eq!(slice_gaps(&r, &[&a], &p, &held).unwrap()["pass"], false);
    }

    #[test]
    fn operating_points_load_from_records_with_bits() {
        let t = f64::from_bits(0x3fed1745d1745d17);
        let ev = json!({"schema": crate::comp_eval::EVAL_SCHEMA, "split": "cal", "systems": {"Faux": {"seeds": {"17": {"operating_points": {
            "0.01": {"threshold": {"bits": "0x3fed1745d1745d17", "value": t, "source": "in_split_cal_rule"}},
            "0.001": {"threshold": {"bits": "0x7ff0000000000000", "value": "inf", "source": "in_split_cal_rule"}}}}}}}});
        let (a, b) = load_operating_points(&ev, "Faux", 17).unwrap();
        assert_eq!(a.to_bits(), t.to_bits());
        assert_eq!(b, f64::INFINITY);
        assert!(load_operating_points(&ev, "Faux", 29).is_err());
        let mut dev = ev.clone();
        dev["split"] = json!("dev_iter");
        assert!(load_operating_points(&dev, "Faux", 17).is_err());
        let mut wrong = ev.clone();
        wrong["systems"]["Faux"]["seeds"]["17"]["operating_points"]["0.01"]["threshold"]["value"] = json!(0.9);
        assert!(load_operating_points(&wrong, "Faux", 17).is_err());
        let plain = json!({"0.01": {"bits": "0x3fed1745d1745d17"}, "0.001": "inf"});
        assert_eq!(load_operating_points(&plain, "x", 1).unwrap().0.to_bits(), t.to_bits());
    }

    #[test]
    fn tier_i_and_verdicts() {
        // 240 humans: zero flags have a Clopper-Pearson bound below 2.5%.
        let r = rows(240, &["wikipedia", "plos"]);
        let good: Vec<Fit> = [17, 29, 41].iter().map(|&s| fit(&r, s, |x| if x.is_human() { 0.1 } else { 0.9 })).collect();
        let frozen: Vec<Fit> = [17, 29, 41].iter().map(|&s| fit(&r, s, |x| if x.is_human() { 0.3 + (x.group.len() % 3) as f64 * 0.05 } else { 0.7 })).collect();
        let g: Vec<&Fit> = good.iter().collect();
        let f: Vec<&Fit> = frozen.iter().collect();
        let t = tier_i(&r, &g, &f).unwrap();
        assert_eq!(t["pass"], true);
        // Against itself the interval is exactly zero: not entirely below zero.
        assert_eq!(tier_i(&r, &g, &g).unwrap()["brier_difference_a_minus_frozen"]["pass"], false);
        let held = BTreeSet::new();
        let p = pangram(&r, 1, |x| Some(if x.is_human() { 0.1 } else { 0.9 }));
        let v = verdicts(&r, &good, 17, &frozen, Some(&p), &NotEvaluated::default(), &held).unwrap();
        assert_eq!(v["tier_i"]["status"], "pass");
        // Equal to Pangram: noninferior (0 <= 0.01); slices below minimum.
        assert_eq!(v["tier_c"]["variants"]["recipe"]["condition_1_noninferiority"]["pass"], true);
        assert_eq!(v["tier_c"]["status"], "pass");
        assert_eq!(v["tier_c"]["variants"]["released_artifact"]["fits"], json!([17]));
        // Too many missing Pangram observations: not evaluated, Tier I still is.
        let pm = pangram(&r, 1, |x| if ["g1", "g2", "g3", "g4", "g5"].contains(&x.group.as_str()) { None } else { Some(0.5) });
        let v = verdicts(&r, &good, 17, &frozen, Some(&pm), &NotEvaluated::default(), &held).unwrap();
        assert_eq!(v["tier_c"]["status"], "not_evaluated");
        assert_eq!(v["tier_i"]["status"], "pass");
        let v = verdicts(&r, &good, 17, &frozen, None, &NotEvaluated(vec!["CAL map refused".into()]), &held).unwrap();
        assert_eq!(v["tier_c"]["reasons"], json!(["CAL map refused"]));
        // An execution failure fails Tier C condition 4 for the recipe only
        // when it is outside seed 17.
        let mut bad: Vec<Fit> = [17, 29, 41].iter().map(|&s| fit(&r, s, |x| if x.is_human() { 0.1 } else { 0.9 })).collect();
        bad[1].scores[3] = DocScore { inv: 0.0, flag: 0.0, failed: true };
        let v = verdicts(&r, &bad, 17, &frozen, Some(&p), &NotEvaluated::default(), &held).unwrap();
        assert_eq!(v["tier_c"]["variants"]["recipe"]["condition_4_execution"]["pass"], false);
        assert_eq!(v["tier_c"]["variants"]["released_artifact"]["condition_4_execution"]["pass"], true);
        assert_eq!(v["tier_c"]["status"], "fail");
        // Pangram's own-point rates on valid observations.
        assert_eq!(v["pangram_rates_at_own_cal_points"]["alpha_0.01"]["humans_observed"], 240);
    }
}
