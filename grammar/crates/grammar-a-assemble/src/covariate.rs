//! Train covariate balance `a_covariate_balance_v1` (protocol A2 §6.3, S0-14)
//! and the Kish effective sample size under the trainer's weighting (§6.3,
//! V24), as amendment 5 of protocol A2 reads them.
//!
//! **Raking.** A weighting step after step 3 of `domain_origin_share_v1`
//! (family balance and cohort factors inside each cell) and before the hosted
//! label step, which then runs as in R0. Within each domain × origin cell of
//! AI-involved rows (mixed, model-only), the positive row weights are raked so
//! that the cell's weighted distribution over word-count bins matches the
//! domain's humans. One margin, so raking is one scaling per bin; the cell's
//! mass is unchanged.
//!
//! - **Word counts** are whitespace words (`grammar_a_pools::text::words`, the
//!   length window's count).
//! - **The target** is the domain's humans with positive weight, weighted by
//!   their step-3 weights. A zero-weight human (ELLIPSE) counts for nothing.
//! - **Bins** are equal-mass quantiles of those human word counts. For a
//!   nominal count `b`, cut `k` (k = 1..b−1) is the smallest human word count
//!   whose cumulative human mass reaches k/b of the total (a mass within the
//!   relative `TIE_TOLERANCE` of it reaches it, so rounding cannot move a cut
//!   at an exact tie). Equal cuts are merged and a cut at the largest human
//!   word count is dropped, so every bin holds human mass and the realised
//!   bin count can be below `b`. Bin 1 is `(−∞, c_1]`, bin k is
//!   `(c_{k−1}, c_k]` and the last bin is open above: a positive outside the
//!   human range goes to the edge bin.
//! - **Factors.** Bin k's raking factor is (human share of bin k) / (cell
//!   share of bin k). A bin without positive mass has no finite factor.
//! - **Bin choice** (amendment 5 item 5.2). A partition qualifies when every
//!   bin has a finite factor no greater than R0's row cap `R0_ROW_CAP` (5×,
//!   with the trainer's own 1e-12 tie tolerance). The candidates are the
//!   partitions of every nominal b from 1 to the domain's search bound
//!   B = ⌈T / m_min⌉ + 1 (T: the human mass, m_min: the smallest mass at one
//!   distinct word count). From B on, every b gives the finest partition
//!   (each distinct count its own bin), so the candidates are every partition
//!   the construction makes; B comes from the data, not from a tuned cap. A
//!   cell takes the qualifying candidate with the most realised bins and,
//!   among those, the smallest nominal b. b = 1 always qualifies (factor
//!   exactly 1). The realised bin count, the nominal b and the cuts per cell
//!   are S0-14, written to `VALUES-R2.json` before the bundle's first fit.
//! - **Check.** After raking, every cell's bin shares must equal the human
//!   targets within `RAKE_TOLERANCE` (1e-12, D3), or the build fails. The
//!   label step that follows moves weight between hosted and other rows of a
//!   cell, so the final bin shares are reported next to the raked ones.
//! - **The trainer's caps apply afterwards** (§6.3). They bound a row at 5×
//!   its origin class's mean and at 5× the mean of all Train rows, not at 5×
//!   its weight before raking, so a factor within 5× does not keep a raked
//!   weight under them (R0's own Train weights exceed the global cap before
//!   any raking). The report counts, per raked cell and overall, the rows
//!   and the mass above both caps on the final assembler weights, and gives
//!   each raked cell's bin shares under the trainer's weighting next to its
//!   targets (amendment 5 item 5.2). These are reported, not gated.
//! - Cells of a domain without positive-weight humans are not raked and are
//!   listed.
//!
//! **Kish ESS** (amendment 5 item 5.1) = (Σf)² / Σf² of the trainer recipe's
//! Train weights f: `full_a_bundle.train_weights` under R0's
//! `train_weighting` (`assembler_row_weight_v1`, `max_row_weight_ratio` 5,
//! `max_row_weight_global_ratio` 5, `cohort_positive_mass_caps`
//! {`raid-v1-`: 0.2}) over the Train rows with positive assembler weight. That
//! is the weighting R0's 32,826.79 (V24's 32,827) was measured under.
//! `trainer_weighting` is a port of that function, Python's float `sum`
//! included. Every round-2 build (any option on) refuses a Train bundle whose
//! ESS, rounded to the nearest integer (halves up), is below 32,827: V24
//! states R0's value at that precision, so R0's own Train passes.
use crate::{ORIGINS, Row};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const COVARIATE_RULE: &str = "a_covariate_balance_v1";
/// R0's row-weight cap (recipe `max_row_weight_ratio` 5.0; protocol A2 §11.10, inherited).
pub const R0_ROW_CAP: f64 = 5.0;
/// R0's global row-weight cap (recipe `max_row_weight_global_ratio` 5.0, inherited).
pub const R0_GLOBAL_CAP: f64 = 5.0;
/// R0's inherited RAID cap (recipe `cohort_positive_mass_caps`; protocol A2 §6.4, §11.10).
pub const R0_RAID_PREFIX: &str = "raid-v1-";
pub const R0_RAID_CAP: f64 = 0.2;
/// Raked bin shares must reproduce the human targets to this (protocol A2 D3).
pub const RAKE_TOLERANCE: f64 = 1e-12;
/// Relative tolerance of the two ties: a cumulative human mass within it of
/// k/b of the total reaches the quantile, and a factor within it of the cap
/// is at the cap (the trainer's `cap_weights` truncates only above
/// cap × (1 + 1e-12)). Float rounding of equal weights cannot move a cut.
pub const TIE_TOLERANCE: f64 = 1e-12;
/// V24's floor: R0's measured Train Kish ESS rounded to the integer.
pub const R0_KISH_ESS_FLOOR: f64 = 32_827.0;
/// R0's measured Train Kish ESS under the trainer's weighting (R0's train-mass record, weighting F).
pub const R0_KISH_ESS_MEASURED: f64 = 32_826.791_314_181_46;
pub const ESS_RULE: &str = "kish_ess_trainer_weighting_v1";
/// The trainer's `full_a_bundle.train_weights` tolerances (ported as they are).
const CAP_TIE: f64 = 1e-12;
const COHORT_CAP_TIE: f64 = 1e-9;
const COHORT_CAP_ROUNDS: usize = 1000;
/// The trainer's origin order (`full_a` origin ints: human, model-only, mixed).
const TRAINER_ORIGIN_ORDER: [usize; 3] = [0, 2, 1];

/// Kish effective sample size of the positive weights in `w`, and their count.
pub fn kish_ess(w: impl IntoIterator<Item = f64>) -> (usize, f64) {
    let (mut n, mut s, mut s2) = (0usize, 0.0f64, 0.0f64);
    for x in w {
        if x > 0.0 {
            n += 1;
            s += x;
            s2 += x * x;
        }
    }
    (n, if s2 > 0.0 { s * s / s2 } else { 0.0 })
}

/// Python's built-in `sum` of floats (CPython 3.12 and later: Neumaier
/// compensated summation; an empty sum is 0), so the port below adds as the
/// trainer does.
pub fn py_sum(it: impl IntoIterator<Item = f64>) -> f64 {
    let mut it = it.into_iter();
    let Some(first) = it.next() else { return 0.0 };
    let mut s = 0.0 + first;
    let mut c = 0.0f64;
    for x in it {
        let t = s + x;
        if s.abs() >= x.abs() {
            c += (s - t) + x;
        } else {
            c += (x - t) + s;
        }
        s = t;
    }
    if c != 0.0 && c.is_finite() {
        s += c;
    }
    s
}

/// `full_a_bundle.cap_weights`: no weight above `ratio` × the mean; the excess
/// goes to the uncapped weights in proportion (water-filling).
fn cap_weights(weights: &[f64], ratio: f64) -> Vec<f64> {
    let n = weights.len();
    let total = py_sum(weights.iter().copied());
    if n == 0 || total <= 0.0 {
        return weights.to_vec();
    }
    let cap = ratio * total / n as f64;
    let mut out = weights.to_vec();
    let mut capped = vec![false; n];
    loop {
        let free: Vec<usize> = (0..n).filter(|&i| !capped[i]).collect();
        let fixed = py_sum((0..n).filter(|&i| capped[i]).map(|_| cap));
        let rest = py_sum(free.iter().map(|&i| weights[i]));
        if free.is_empty() || rest <= 0.0 {
            break;
        }
        let f = (total - fixed) / rest;
        let over: Vec<usize> = free.iter().copied().filter(|&i| weights[i] * f > cap * (1.0 + CAP_TIE)).collect();
        for &i in &free {
            out[i] = weights[i] * f;
        }
        for &i in &over {
            out[i] = cap;
        }
        if over.is_empty() {
            break;
        }
        for &i in &over {
            capped[i] = true;
        }
    }
    out
}

/// `full_a_bundle.apply_global_cap`: no row above `ratio` × the mean of all
/// rows with mass, each class of `groups` water-filled on its own.
fn apply_global_cap(out: &[f64], groups: &[Vec<usize>], ratio: f64) -> Result<Vec<f64>> {
    let n = out.iter().filter(|w| **w > 0.0).count();
    let total = py_sum(out.iter().copied());
    ensure!(n > 0 && total > 0.0, "no Train weight");
    let cap = ratio * total / n as f64;
    let mut out = out.to_vec();
    for idx in groups {
        let live: Vec<usize> = idx.iter().copied().filter(|&i| out[i] > 0.0).collect();
        let mass = py_sum(live.iter().map(|&i| out[i]));
        if live.is_empty() || mass <= 0.0 {
            continue;
        }
        let rel = cap / (mass / live.len() as f64);
        ensure!(rel >= 1.0, "the global row cap {ratio} is below a class mean ({:.3} x global)", 1.0 / rel * ratio);
        let w: Vec<f64> = live.iter().map(|&i| out[i]).collect();
        for (&i, x) in live.iter().zip(cap_weights(&w, rel)) {
            out[i] = x;
        }
    }
    Ok(out)
}

/// The trainer recipe's Train weights of one bundle: `full_a_bundle.train_weights`
/// under R0's `train_weighting`, with the per-row statistics of the trainer's
/// mass report.
#[derive(Clone, Debug)]
pub struct TrainerWeights {
    /// Per input row: its weight (summing to 1 over rows with positive input weight), 0 elsewhere.
    pub w: Vec<f64>,
    pub rows: usize,
    pub ess: f64,
    pub ess_uncapped: f64,
    pub max_over_class_mean: f64,
    pub max_over_global_mean: f64,
    pub raid_share_of_ai_involved: f64,
}

/// R0's `train_weighting` (the trainer recipe this port follows).
pub fn trainer_weighting_json() -> Value {
    json!({"scheme": "assembler_row_weight_v1", "positive_mass": null, "max_row_weight_ratio": R0_ROW_CAP,
        "max_row_weight_global_ratio": R0_GLOBAL_CAP, "cohort_positive_mass_caps": {R0_RAID_PREFIX: R0_RAID_CAP}})
}

/// Port of the trainer's Train weighting over `rows` = (origin index in
/// `ORIGINS`, cohort id, assembler weight), in bundle row order. Rows with
/// weight 0 are dropped as the trainer drops them.
pub fn trainer_weighting(rows: &[(usize, &str, f64)]) -> Result<TrainerWeights> {
    let live: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].2 > 0.0).collect();
    for &(o, _, w) in rows {
        ensure!(o < ORIGINS.len() && w.is_finite() && w >= 0.0, "Train weights must be finite and >= 0");
    }
    let n = live.len();
    let cls: Vec<usize> = live.iter().map(|&i| rows[i].0).collect();
    let cohort: Vec<&str> = live.iter().map(|&i| rows[i].1).collect();
    let raw: Vec<f64> = live.iter().map(|&i| rows[i].2).collect();
    let total = py_sum(raw.iter().copied());
    ensure!(total > 0.0, "no Train weight");
    let mut out: Vec<f64> = raw.iter().map(|w| w / total).collect();
    let groups: Vec<Vec<usize>> = TRAINER_ORIGIN_ORDER.iter().map(|&o| (0..n).filter(|&i| cls[i] == o).collect::<Vec<_>>())
        .filter(|g| !g.is_empty()).collect();
    for idx in &groups {
        let w: Vec<f64> = idx.iter().map(|&i| out[i]).collect();
        for (&i, x) in idx.iter().zip(cap_weights(&w, R0_ROW_CAP)) {
            out[i] = x;
        }
    }
    let out = apply_global_cap(&out, &groups, R0_GLOBAL_CAP)?;
    // apply_cohort_caps with the one cap {raid-v1-: 0.2}.
    let pos: Vec<usize> = (0..n).filter(|&i| cls[i] != 0 && out[i] > 0.0).collect();
    let mut w = out;
    let mut converged = false;
    for _ in 0..COHORT_CAP_ROUNDS {
        let sel: Vec<usize> = pos.iter().copied().filter(|&i| cohort[i].starts_with(R0_RAID_PREFIX)).collect();
        let total = py_sum(pos.iter().map(|&i| w[i]));
        let share = if total > 0.0 { py_sum(sel.iter().map(|&i| w[i])) / total } else { 0.0 };
        let over = share > R0_RAID_CAP * (1.0 + COHORT_CAP_TIE);
        if !over {
            converged = true;
            break;
        }
        let rest: Vec<usize> = pos.iter().copied().filter(|&i| !cohort[i].starts_with(R0_RAID_PREFIX)).collect();
        ensure!(!rest.is_empty() && share < 1.0, "cohort prefix {R0_RAID_PREFIX} holds every AI-involved row");
        for &i in &sel {
            w[i] *= R0_RAID_CAP / share;
        }
        for &i in &rest {
            w[i] *= (1.0 - R0_RAID_CAP) / (1.0 - share);
        }
        for idx in &groups {
            let v: Vec<f64> = idx.iter().map(|&i| w[i]).collect();
            for (&i, x) in idx.iter().zip(cap_weights(&v, R0_ROW_CAP)) {
                w[i] = x;
            }
        }
        w = apply_global_cap(&w, &groups, R0_GLOBAL_CAP)?;
    }
    if !converged {
        bail!("cohort_positive_mass_caps and the row caps did not converge in {COHORT_CAP_ROUNDS} rounds");
    }
    // The trainer's mass report: weights normalised to sum 1, ESS = 1 / Σ w².
    let tot = py_sum(w.iter().copied());
    let w: Vec<f64> = w.iter().map(|x| x / tot).collect();
    let ess = 1.0 / py_sum(w.iter().map(|x| x * x));
    let mut cls_mean = [0.0f64; 3];
    for (o, m) in cls_mean.iter_mut().enumerate() {
        let k = cls.iter().filter(|&&c| c == o).count();
        *m = py_sum((0..n).filter(|&i| cls[i] == o).map(|i| w[i])) / k.max(1) as f64;
    }
    let max_over_class_mean = (0..n).map(|i| w[i] / cls_mean[cls[i]]).fold(f64::NEG_INFINITY, f64::max);
    let max_over_global_mean = w.iter().copied().fold(f64::NEG_INFINITY, f64::max) * n as f64;
    let ai = py_sum(pos.iter().map(|&i| w[i]));
    let raid = py_sum(pos.iter().filter(|&&i| cohort[i].starts_with(R0_RAID_PREFIX)).map(|&i| w[i]));
    let mut full = vec![0.0f64; rows.len()];
    for (k, &i) in live.iter().enumerate() {
        full[i] = w[k];
    }
    // The mass report's `assembler_uncapped` weighting: the normalised weights, renormalised.
    let u: Vec<f64> = raw.iter().map(|x| x / total).collect();
    let ut = py_sum(u.iter().copied());
    let uncapped = 1.0 / py_sum(u.iter().map(|x| (x / ut) * (x / ut)));
    Ok(TrainerWeights { w: full, rows: n, ess, ess_uncapped: uncapped, max_over_class_mean, max_over_global_mean,
        raid_share_of_ai_involved: if ai > 0.0 { raid / ai } else { 0.0 } })
}

/// The ESS refusal floor in force (tests may lower it for small synthetic fixtures).
pub fn ess_floor() -> f64 {
    #[cfg(test)]
    if let Some(f) = test_floor::get() {
        return f;
    }
    R0_KISH_ESS_FLOOR
}

/// V24 as amendment 5 reads it: the ESS rounded to the nearest integer (halves up) is at least the floor.
pub fn passes_floor(ess: f64, floor: f64) -> bool {
    ess.round() >= floor
}

/// The ESS report of a Train split under the trainer's weighting (no refusal).
pub fn ess_value(t: &TrainerWeights) -> Value {
    let floor = ess_floor();
    json!({"rule": ESS_RULE,
        "definition": "Kish ESS (sum f)^2 / sum f^2 of the trainer recipe's Train weights f (full_a_bundle.train_weights under train_weighting) over Train rows with positive assembler weight; compared after rounding to the nearest integer (protocol A2 V24, amendment 5 item 5.1)",
        "train_weighting": trainer_weighting_json(), "rows_positive_weight": t.rows, "ess": t.ess,
        "ess_rounded": t.ess.round(), "floor": floor, "r0_measured": R0_KISH_ESS_MEASURED,
        "passes_floor": passes_floor(t.ess, floor), "ess_assembler_weights_uncapped": t.ess_uncapped,
        "max_over_class_mean": t.max_over_class_mean, "max_over_global_mean": t.max_over_global_mean,
        "raid_share_of_ai_involved_mass": t.raid_share_of_ai_involved})
}

/// The ESS report of a round-2 Train bundle; a bundle below the floor is refused.
pub fn ess_check(t: &TrainerWeights) -> Result<Value> {
    let v = ess_value(t);
    ensure!(v["passes_floor"] == true,
        "Train Kish ESS {:.3} (rounded {}) over {} positive-weight rows, under the trainer's weighting, is below R0's {} (protocol A2 V24, amendment 5 item 5.1): the bundle is refused and fixed by amendment",
        t.ess, t.ess.round(), t.rows, v["floor"]);
    Ok(v)
}

#[cfg(test)]
pub mod test_floor {
    use std::cell::Cell;
    thread_local! {
        static FLOOR: Cell<Option<f64>> = const { Cell::new(None) };
    }
    pub fn get() -> Option<f64> {
        FLOOR.with(Cell::get)
    }
    /// Lower the floor on this thread until the guard drops (synthetic fixtures are small).
    pub struct Guard(Option<f64>);
    pub fn set(f: f64) -> Guard {
        Guard(FLOOR.with(|c| c.replace(Some(f))))
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            FLOOR.with(|c| c.set(self.0));
        }
    }
}

/// Equal-mass quantile bins of a domain's human word counts.
#[derive(Clone, Debug, PartialEq)]
pub struct Bins {
    /// Nominal bin count `b` the cuts were made for.
    pub nominal: usize,
    /// Indices into the distinct human word counts of every cut, ascending.
    pub cut_index: Vec<usize>,
    /// The cuts as word counts: the inclusive upper bound of every bin but the last.
    pub cuts: Vec<usize>,
}

impl Bins {
    /// Bin index of a word count (positives outside the human range go to an edge bin).
    pub fn of(&self, words: usize) -> usize {
        self.cuts.partition_point(|&c| c < words)
    }
    /// The realised bin count.
    pub fn len(&self) -> usize {
        self.cuts.len() + 1
    }
    pub fn is_empty(&self) -> bool {
        false
    }
    /// Bin masses from masses per index (`per[j]`): bin k covers indices
    /// cut_{k-1}+1 ..= cut_k, the last bin every index after the last cut.
    /// Index j of a human distribution is its j-th distinct word count; slot j
    /// of a cell holds the positives whose word count x has exactly j distinct
    /// human word counts below it (j = D: above the human range).
    pub fn masses(&self, per: &[f64]) -> Vec<f64> {
        let mut out = Vec::with_capacity(self.len());
        let mut lo = 0usize;
        for &c in &self.cut_index {
            out.push(per[lo..=c].iter().sum());
            lo = c + 1;
        }
        out.push(per[lo..].iter().sum());
        out
    }
}

/// Distinct human word counts (ascending) with their summed masses.
pub fn human_distribution(humans: impl IntoIterator<Item = (usize, f64)>) -> Vec<(usize, f64)> {
    let mut m: BTreeMap<usize, f64> = BTreeMap::new();
    for (x, w) in humans {
        if w > 0.0 {
            *m.entry(x).or_default() += w;
        }
    }
    m.into_iter().collect()
}

/// Mass of a cell per slot of `dist` (len D + 1; see `Bins::masses`).
pub fn slot_masses(dist: &[(usize, f64)], cell: impl IntoIterator<Item = (usize, f64)>) -> Vec<f64> {
    let mut m = vec![0.0f64; dist.len() + 1];
    for (x, w) in cell {
        m[dist.partition_point(|v| v.0 < x)] += w;
    }
    m
}

/// The equal-mass quantile bins for nominal count `b` over `dist` (non-empty).
///
/// Cut k is at the smallest index j with P_j·b ≥ k·T·(1 − tol) (P_j the
/// running human mass through j), or at the last index (dropped). The cuts
/// are found per index in O(D): index j < D − 1 is a cut iff the smallest k
/// with k·T·(1 − tol) > P_{j−1}·b is at most b − 1 and reaches P_j·b.
pub fn quantile_bins(dist: &[(usize, f64)], b: usize) -> Bins {
    assert!(!dist.is_empty() && b >= 1);
    let total: f64 = dist.iter().map(|x| x.1).sum();
    let last = dist.len() - 1;
    let bf = b as f64;
    let g = |k: usize| (k as f64) * total * (1.0 - TIE_TOLERANCE);
    let step = total * (1.0 - TIE_TOLERANCE);
    let mut cut_index: Vec<usize> = Vec::new();
    let mut prev = f64::NEG_INFINITY;
    let mut cum = 0.0f64;
    let mut k = 1usize;
    for (j, x) in dist.iter().enumerate().take(last) {
        cum += x.1;
        let hi = cum * bf;
        if k < b && g(k) <= prev {
            // Jump near the smallest k with g(k) > prev, then step to it exactly
            // (g is non-decreasing in k, and this k never decreases with j).
            let est = (prev / step).floor();
            if est.is_finite() && est >= 1.0 {
                k = k.max((est as usize).min(b));
            }
            while k > 1 && g(k - 1) > prev {
                k -= 1;
            }
            while k < b && g(k) <= prev {
                k += 1;
            }
        }
        if k < b && g(k) <= hi {
            cut_index.push(j);
        }
        prev = hi;
    }
    let cuts = cut_index.iter().map(|&j| dist[j].0).collect();
    Bins { nominal: b, cut_index, cuts }
}

/// The search bound B = ⌈T / m_min⌉ + 1 of a domain's human distribution:
/// from B on every nominal b gives the finest partition.
pub fn search_bound(dist: &[(usize, f64)]) -> usize {
    let total: f64 = dist.iter().map(|x| x.1).sum();
    let m = dist.iter().map(|x| x.1).fold(f64::INFINITY, f64::min);
    if dist.len() <= 1 || m.is_nan() || m <= 0.0 {
        return 1;
    }
    (total / m).ceil() as usize + 1
}

/// Raking factors (human share / cell share per bin), or None if a bin has no positive mass.
pub fn factors(human: &[f64], cell: &[f64]) -> Option<Vec<f64>> {
    let h: f64 = human.iter().sum();
    let p: f64 = cell.iter().sum();
    if h <= 0.0 || p <= 0.0 {
        return None;
    }
    human.iter().zip(cell).map(|(&hk, &pk)| (pk > 0.0).then(|| (hk * p) / (h * pk))).collect()
}

/// True when every bin has a finite factor no greater than R0's row cap (within the tie tolerance).
pub fn within_cap(f: &Option<Vec<f64>>) -> bool {
    f.as_ref().is_some_and(|f| f.iter().all(|&x| x <= R0_ROW_CAP * (1.0 + TIE_TOLERANCE)))
}

/// The chosen bins of every cell of one domain (`hum`: human mass per index
/// of `dist`; `cells`: each cell's slot masses): over every nominal b in
/// 1..=`search_bound(dist)`, the qualifying partition with the most realised
/// bins, the smallest nominal b among those.
pub fn choose_bins(dist: &[(usize, f64)], hum: &[f64], cells: &[Vec<f64>]) -> Vec<Bins> {
    let one = quantile_bins(dist, 1);
    let mut best: Vec<Bins> = vec![one.clone(); cells.len()];
    let finest = dist.len();
    let bound = search_bound(dist);
    let mut prev = one.cut_index;
    for b in 2..=bound {
        if best.iter().all(|x| x.len() == finest) {
            break;
        }
        let bins = quantile_bins(dist, b);
        if bins.cut_index == prev {
            continue;
        }
        prev = bins.cut_index.clone();
        let hm = bins.masses(hum);
        for (k, slots) in cells.iter().enumerate() {
            if bins.len() > best[k].len() && within_cap(&factors(&hm, &bins.masses(slots))) {
                best[k] = bins.clone();
            }
        }
    }
    best
}

/// `choose_bins` for one cell.
pub fn best_bins(dist: &[(usize, f64)], hum: &[f64], slots: &[f64]) -> Bins {
    choose_bins(dist, hum, &[slots.to_vec()]).pop().expect("one cell")
}

fn bin_masses(bins: &Bins, items: impl IntoIterator<Item = (usize, f64)>) -> Vec<f64> {
    let mut m = vec![0.0f64; bins.len()];
    for (x, w) in items {
        m[bins.of(x)] += w;
    }
    m
}

fn shares_and_deviation(m: &[f64], target: &[f64]) -> (Vec<f64>, f64) {
    let s: f64 = m.iter().sum();
    let shares: Vec<f64> = m.iter().map(|x| if s > 0.0 { x / s } else { 0.0 }).collect();
    let dev = shares.iter().zip(target).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
    (shares, dev)
}

/// One raked cell: its bins, the human target shares and its row indices.
struct RakedCell {
    bins: Bins,
    target: Vec<f64>,
    rows: Vec<usize>,
}

/// Bin assignments kept for the final report after the label step.
pub struct Raked {
    /// (domain, origin index) -> the raked cell.
    cells: BTreeMap<(String, usize), RakedCell>,
    words: Vec<usize>,
    report: Value,
}

/// Rake the AI-involved rows of every domain × origin cell in place (`w`
/// holds the step-3 weights of `rows`; rows with weight 0 are left alone).
pub fn rake(rows: &[Row], w: &mut [f64]) -> Result<Raked> {
    let words: Vec<usize> = rows.iter().map(|r| grammar_a_pools::text::words(&r.text)).collect();
    let mut humans: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    let mut cells: BTreeMap<&str, BTreeMap<usize, Vec<usize>>> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() {
        if w[i] <= 0.0 {
            continue;
        }
        match ORIGINS.iter().position(|o| *o == r.origin) {
            Some(0) => humans.entry(&r.domain).or_default().push(i),
            Some(o) => cells.entry(&r.domain).or_default().entry(o).or_default().push(i),
            None => unreachable!("origin checked on read"),
        }
    }
    let mut out = BTreeMap::new();
    let mut by_domain: BTreeMap<String, Value> = BTreeMap::new();
    let mut bin_counts: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    let mut unraked = Vec::new();
    let mut worst = 0.0f64;
    for (d, dcells) in &cells {
        let Some(h) = humans.get(d) else {
            for (o, idx) in dcells {
                unraked.push(json!({"domain": d, "origin": ORIGINS[*o], "rows": idx.len(), "reason": "no_positive_weight_humans_in_domain"}));
            }
            continue;
        };
        let dist = human_distribution(h.iter().map(|&i| (words[i], w[i])));
        let hum: Vec<f64> = dist.iter().map(|x| x.1).collect();
        let order: Vec<(usize, &Vec<usize>)> = dcells.iter().map(|(o, idx)| (*o, idx)).collect();
        let slots: Vec<Vec<f64>> = order.iter().map(|(_, idx)| slot_masses(&dist, idx.iter().map(|&i| (words[i], w[i])))).collect();
        let chosen = choose_bins(&dist, &hum, &slots);
        let bound = search_bound(&dist);
        let (lo, hi) = (dist[0].0, dist[dist.len() - 1].0);
        let dom = by_domain.entry(d.to_string()).or_insert_with(|| json!({
            "human_rows": h.len(), "distinct_human_word_counts": dist.len(),
            "human_word_count_range": [lo, hi], "search_bound_b": bound, "cells": {}}));
        for ((o, idx), bins) in order.into_iter().zip(chosen) {
            let cell: Vec<(usize, f64)> = idx.iter().map(|&i| (words[i], w[i])).collect();
            let hm = bins.masses(&hum);
            let pm = bin_masses(&bins, cell.iter().copied());
            let f = factors(&hm, &pm).expect("b = 1 always has factor 1");
            let mass: f64 = pm.iter().sum();
            let hsum: f64 = hm.iter().sum();
            let target: Vec<f64> = hm.iter().map(|x| x / hsum).collect();
            for &i in idx {
                w[i] *= f[bins.of(words[i])];
            }
            // Renormalise the cell to its exact mass.
            let s: f64 = idx.iter().map(|&i| w[i]).sum();
            for &i in idx {
                w[i] *= mass / s;
            }
            let (shares, dev) = shares_and_deviation(&bin_masses(&bins, idx.iter().map(|&i| (words[i], w[i]))), &target);
            ensure!(dev <= RAKE_TOLERANCE, "{COVARIATE_RULE}: cell {d} x {} reproduces its target bins only to {dev:e}", ORIGINS[o]);
            worst = worst.max(dev);
            bin_counts.entry(d.to_string()).or_default().insert(ORIGINS[o].into(),
                json!({"bins": bins.len(), "nominal_b": bins.nominal, "cuts_words": bins.cuts}));
            dom["cells"][ORIGINS[o]] = json!({
                "rows": idx.len(), "bins": bins.len(), "nominal_b": bins.nominal, "cuts_words": bins.cuts,
                "below_human_range": cell.iter().filter(|x| x.0 < lo).count(),
                "above_human_range": cell.iter().filter(|x| x.0 > hi).count(),
                "human_target_shares": target, "before_shares": pm.iter().map(|x| x / mass).collect::<Vec<_>>(),
                "factors": f, "max_factor": f.iter().copied().fold(0.0, f64::max), "raked_shares": shares,
                "raked_max_abs_deviation": dev});
            out.insert((d.to_string(), o), RakedCell { bins, target, rows: idx.clone() });
        }
    }
    let report = json!({"rule": COVARIATE_RULE, "row_cap": R0_ROW_CAP, "tolerance": RAKE_TOLERANCE,
        "word_count": "whitespace words (grammar_a_pools::text::words)",
        "human_target": "positive-weight humans of the domain, step-3 weights",
        "step": "after step 3 (family balance, cohort factors), before the hosted label step",
        "bin_choice": "the qualifying quantile partition (every bin a positive, no factor above 5x) with the most realised bins over nominal b = 1..search_bound_b, the smallest nominal b among those (protocol A2 amendment 5 item 5.2)",
        "bin_counts_s0_14": bin_counts, "raked_max_abs_deviation": worst, "unraked_cells": unraked,
        "domains": by_domain});
    Ok(Raked { cells: out, words, report })
}

/// Rows and mass above the trainer's two row caps on the final assembler
/// weights `w` (class: 5× the mean of the row's origin over positive rows;
/// global: 5× the mean of all positive rows). Mass is a share of the Train total.
struct CapExcess {
    class_cap: [f64; 3],
    global_cap: f64,
    total: f64,
}

impl CapExcess {
    fn new(rows: &[Row], w: &[f64]) -> Self {
        let mut s = [0.0f64; 3];
        let mut n = [0usize; 3];
        for (r, &x) in rows.iter().zip(w) {
            if x > 0.0 {
                let o = ORIGINS.iter().position(|o| *o == r.origin).expect("origin checked on read");
                s[o] += x;
                n[o] += 1;
            }
        }
        let total: f64 = s.iter().sum();
        let all: usize = n.iter().sum();
        let class_cap = [0, 1, 2].map(|o| if n[o] > 0 { R0_ROW_CAP * s[o] / n[o] as f64 } else { 0.0 });
        CapExcess { class_cap, global_cap: if all > 0 { R0_GLOBAL_CAP * total / all as f64 } else { 0.0 }, total }
    }
    fn summary(&self, items: impl IntoIterator<Item = (usize, f64)>) -> Value {
        let (mut rc, mut mc, mut rg, mut mg) = (0usize, 0.0f64, 0usize, 0.0f64);
        for (o, x) in items {
            if x > self.class_cap[o] * (1.0 + CAP_TIE) {
                rc += 1;
                mc += x - self.class_cap[o];
            }
            if x > self.global_cap * (1.0 + CAP_TIE) {
                rg += 1;
                mg += x - self.global_cap;
            }
        }
        let share = |m: f64| if self.total > 0.0 { m / self.total } else { 0.0 };
        json!({"rows_above_class_cap": rc, "mass_above_class_cap": share(mc),
            "rows_above_global_cap": rg, "mass_above_global_cap": share(mg)})
    }
}

impl Raked {
    /// The raking report with the final bin shares (after the label step and
    /// normalisation, `w`), the rows and mass above the trainer's caps, and
    /// the bin shares under the trainer's weighting `trainer` (amendment 5 item 5.2).
    pub fn finish(mut self, rows: &[Row], w: &[f64], trainer: &[f64]) -> Value {
        let caps = CapExcess::new(rows, w);
        let mut worst = 0.0f64;
        let mut worst_trainer = 0.0f64;
        for ((d, o), RakedCell { bins, target, rows: idx }) in &self.cells {
            let (shares, dev) = shares_and_deviation(&bin_masses(bins, idx.iter().map(|&i| (self.words[i], w[i]))), target);
            let (tshares, tdev) = shares_and_deviation(&bin_masses(bins, idx.iter().map(|&i| (self.words[i], trainer[i]))), target);
            worst = worst.max(dev);
            worst_trainer = worst_trainer.max(tdev);
            let c = &mut self.report["domains"][d]["cells"][ORIGINS[*o]];
            c["final_shares"] = json!(shares);
            c["final_max_abs_deviation"] = json!(dev);
            c["trainer_weighting_shares"] = json!(tshares);
            c["trainer_weighting_max_abs_deviation"] = json!(tdev);
            c["above_trainer_caps"] = caps.summary(idx.iter().map(|&i| (*o, w[i])));
        }
        let pos = |o: usize| rows.iter().zip(w).filter(move |(r, x)| **x > 0.0 && r.origin == ORIGINS[o]).map(move |(_, x)| (o, *x));
        self.report["final_max_abs_deviation"] = json!(worst);
        self.report["trainer_weighting_max_abs_deviation"] = json!(worst_trainer);
        self.report["above_trainer_caps"] = json!({
            "rule": "final assembler weights above 5x their origin class's mean (class cap) and above 5x the mean of all positive-weight Train rows (global cap); mass as a share of the Train total; the trainer truncates them (reported, not gated)",
            "class_cap": {ORIGINS[0]: caps.class_cap[0], ORIGINS[1]: caps.class_cap[1], ORIGINS[2]: caps.class_cap[2]},
            "global_cap": caps.global_cap,
            "all": caps.summary((0..3).flat_map(pos)),
            "by_origin": {ORIGINS[0]: caps.summary(pos(0)), ORIGINS[1]: caps.summary(pos(1)), ORIGINS[2]: caps.summary(pos(2))},
            "raked_cells": caps.summary(self.cells.iter().flat_map(|((_, o), c)| c.rows.iter().map(move |&i| (*o, w[i])))),
        });
        self.report
    }
}
