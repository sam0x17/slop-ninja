//! Pre-registered Pangram subsample design for the competitive A protocol,
//! amendment 1 (protocol v1.1). Numeric only: strata, allocation, selection
//! keys, Horvitz-Thompson group values, the paired source-group bootstrap,
//! Clopper-Pearson flag limits and the planning power calculation.
//!
//! Nothing here reads, parses or generates prose.

use anyhow::{Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Domain string of the subsample selection key.
pub const SUBSAMPLE_KEY_DOMAIN: &str = "slopninja-a-comp-pangram-subsample-v1";
/// Pre-registered subsample seed (amendment 1).
pub const SUBSAMPLE_SEED: u64 = 20261027;
/// Minimum documents per non-empty stratum (or the whole stratum if smaller).
pub const STRATUM_FLOOR: usize = 3;
/// Realised-share bins for mixed documents, as in the protocol's share report.
pub const SHARE_BINS: [&str; 7] = [
    "(0,10)", "[10,25)", "[25,50)", "[50,75)", "[75,90)", "[90,100)", "100",
];
/// Two-sided 95% normal quantile and the 80% / 90% power quantiles.
pub const Z_975: f64 = 1.959_963_984_540_054;
pub const Z_80: f64 = 0.841_621_233_572_914_3;
pub const Z_90: f64 = 1.281_551_565_544_600_5;

/// Index into [`SHARE_BINS`] for the realised share of a mixed document,
/// given as a fraction in (0, 1].
pub fn share_bin(share: f64) -> Result<usize> {
    ensure!(
        share.is_finite() && share > 0.0 && share <= 1.0,
        "mixed share {share} outside (0, 1]"
    );
    Ok(if share < 0.10 {
        0
    } else if share < 0.25 {
        1
    } else if share < 0.50 {
        2
    } else if share < 0.75 {
        3
    } else if share < 0.90 {
        4
    } else if share < 1.0 {
        5
    } else {
        6
    })
}

/// Origin cell of a primary document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cell {
    HumanOnly,
    ModelOnly,
    Mixed,
}

impl Cell {
    pub fn parse(origin: &str) -> Result<Self> {
        Ok(match origin {
            "human_only" => Cell::HumanOnly,
            "model_only" => Cell::ModelOnly,
            "mixed" => Cell::Mixed,
            other => bail!("unknown origin {other}"),
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Cell::HumanOnly => "human_only",
            Cell::ModelOnly => "model_only",
            Cell::Mixed => "mixed",
        }
    }
}

/// Stratum key `domain|human_only`, `domain|model_only` or `domain|mixed:<bin>`.
pub fn stratum_key(domain: &str, cell: Cell, share: Option<f64>) -> Result<String> {
    ensure!(
        !domain.is_empty() && !domain.contains('|'),
        "bad domain {domain:?}"
    );
    Ok(match cell {
        Cell::Mixed => {
            let s = share.ok_or_else(|| anyhow::anyhow!("mixed document without share"))?;
            format!("{domain}|mixed:{}", SHARE_BINS[share_bin(s)?])
        }
        c => format!("{domain}|{}", c.name()),
    })
}

/// One stratum for allocation: population `N_h`, balanced mass `W_h` and a
/// planning standard deviation `S_h` (use 1 for proportional allocation).
#[derive(Clone, Debug)]
pub struct Stratum {
    pub key: String,
    pub population: usize,
    pub weight: f64,
    pub sd: f64,
}

/// Exact allocation rule: Neyman allocation `n_h ∝ W_h S_h`, with each
/// stratum held to at least `min(N_h, floor)` and at most `N_h`.
///
/// Real-valued allocation is iterated over the free strata: strata below
/// their floor are held at the floor; otherwise strata above their population
/// are fixed at `N_h` and every floor hold is released; the remainder is
/// reallocated over the free strata until nothing moves.
/// Free strata are then rounded down and the deficit is given one unit at a
/// time by largest fractional part, ties by ascending key, never above `N_h`.
/// Strata with zero score (`W_h S_h = 0`) receive only their floor.
pub fn neyman_allocation(strata: &[Stratum], total: usize, floor: usize) -> Result<Vec<usize>> {
    let mut keys = BTreeSet::new();
    for s in strata {
        ensure!(keys.insert(&s.key), "duplicate stratum {}", s.key);
        ensure!(
            s.weight.is_finite() && s.weight >= 0.0 && s.sd.is_finite() && s.sd >= 0.0,
            "bad weight or sd in {}",
            s.key
        );
    }
    let pop: usize = strata.iter().map(|s| s.population).sum();
    let floors: Vec<usize> = strata.iter().map(|s| s.population.min(floor)).collect();
    let need: usize = floors.iter().sum();
    ensure!(total <= pop, "total {total} exceeds population {pop}");
    ensure!(
        total >= need,
        "total {total} below the stratum floors {need}"
    );
    let score: Vec<f64> = strata.iter().map(|s| s.weight * s.sd).collect();
    // Fixed strata: zero-score strata at their floor (permanent), strata at
    // their population (permanent) and strata held at their floor (released
    // whenever a new stratum reaches its population).
    let permanent: Vec<bool> = strata
        .iter()
        .enumerate()
        .map(|(i, s)| s.population == 0 || score[i] == 0.0)
        .collect();
    let mut fixed: Vec<Option<usize>> = (0..strata.len())
        .map(|i| permanent[i].then_some(floors[i]))
        .collect();
    let mut capped = vec![false; strata.len()];
    let mut real = vec![0.0f64; strata.len()];
    loop {
        let used: usize = fixed.iter().flatten().sum();
        let free: Vec<usize> = (0..strata.len()).filter(|&i| fixed[i].is_none()).collect();
        if free.is_empty() {
            break;
        }
        ensure!(used <= total, "allocation infeasible");
        let remaining = (total - used) as f64;
        let sum: f64 = free.iter().map(|&i| score[i]).sum();
        for &i in &free {
            real[i] = remaining * score[i] / sum;
        }
        let under: Vec<usize> = free
            .iter()
            .copied()
            .filter(|&i| real[i] < floors[i] as f64)
            .collect();
        if !under.is_empty() {
            for i in under {
                fixed[i] = Some(floors[i]);
            }
            continue;
        }
        let over: Vec<usize> = free
            .iter()
            .copied()
            .filter(|&i| real[i] > strata[i].population as f64)
            .collect();
        if !over.is_empty() {
            for i in over {
                fixed[i] = Some(strata[i].population);
                capped[i] = true;
            }
            for i in 0..strata.len() {
                if !permanent[i] && !capped[i] {
                    fixed[i] = None;
                }
            }
            continue;
        }
        break;
    }
    let mut out: Vec<usize> = (0..strata.len())
        .map(|i| fixed[i].unwrap_or_else(|| (real[i] * (1.0 - 1e-12)).floor() as usize))
        .collect();
    let mut deficit = total as i64 - out.iter().sum::<usize>() as i64;
    ensure!(deficit >= 0, "allocation overshoot");
    let mut order: Vec<usize> = (0..strata.len()).filter(|&i| fixed[i].is_none()).collect();
    order.sort_by(|&a, &b| {
        let fa = real[a] - real[a].floor();
        let fb = real[b] - real[b].floor();
        fb.total_cmp(&fa)
            .then_with(|| strata[a].key.cmp(&strata[b].key))
    });
    // Fixed strata that sit at their floor may also absorb units if the free
    // strata are exhausted (all at population).
    let mut fallback: Vec<usize> = (0..strata.len()).filter(|&i| fixed[i].is_some()).collect();
    fallback.sort_by(|&a, &b| strata[a].key.cmp(&strata[b].key));
    for pass in [&order, &fallback] {
        while deficit > 0 {
            let mut moved = false;
            for &i in pass.iter() {
                if deficit == 0 {
                    break;
                }
                if out[i] < strata[i].population {
                    out[i] += 1;
                    deficit -= 1;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
    }
    ensure!(deficit == 0, "could not place {deficit} documents");
    Ok(out)
}

/// Selection key `SHA-256(domain + ":" + seed + ":" + split + ":" + id)`.
pub fn selection_key(seed: u64, split: &str, doc_id: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(format!("{SUBSAMPLE_KEY_DOMAIN}:{seed}:{split}:{doc_id}").as_bytes());
    h.finalize().into()
}

/// Select, within each stratum, the `n_h` documents with the smallest
/// selection keys. `docs` is `(doc_id, stratum_key)`.
pub fn select(
    docs: &[(String, String)],
    allocation: &BTreeMap<String, usize>,
    seed: u64,
    split: &str,
) -> Result<BTreeSet<String>> {
    let mut by: BTreeMap<&str, Vec<([u8; 32], &str)>> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for (id, k) in docs {
        ensure!(ids.insert(id.as_str()), "duplicate document {id}");
        by.entry(k.as_str())
            .or_default()
            .push((selection_key(seed, split, id), id.as_str()));
    }
    for k in allocation.keys() {
        ensure!(
            by.contains_key(k.as_str()),
            "allocation names empty stratum {k}"
        );
    }
    let mut out = BTreeSet::new();
    for (k, mut v) in by {
        let n = *allocation
            .get(k)
            .ok_or_else(|| anyhow::anyhow!("no allocation for stratum {k}"))?;
        ensure!(
            n <= v.len(),
            "stratum {k} allocation {n} above population {}",
            v.len()
        );
        v.sort();
        out.extend(v.into_iter().take(n).map(|(_, id)| id.to_string()));
    }
    Ok(out)
}

/// One primary evaluation document for the paired comparison.
#[derive(Clone, Debug)]
pub struct EvalDoc {
    pub group: String,
    pub cell: Cell,
    pub stratum: String,
    /// Per-document value, for example the paired Brier difference.
    pub value: f64,
}

/// Balanced weights `1 / (G * C_g * n_gc)` over the full population; they sum to 1.
pub fn balanced_weights(docs: &[EvalDoc]) -> Vec<f64> {
    let mut cells: BTreeMap<(&str, Cell), usize> = BTreeMap::new();
    for d in docs {
        *cells.entry((d.group.as_str(), d.cell)).or_default() += 1;
    }
    let mut per_group: BTreeMap<&str, usize> = BTreeMap::new();
    for (g, _) in cells.keys() {
        *per_group.entry(g).or_default() += 1;
    }
    let groups = per_group.len() as f64;
    docs.iter()
        .map(|d| {
            1.0 / (groups
                * per_group[d.group.as_str()] as f64
                * cells[&(d.group.as_str(), d.cell)] as f64)
        })
        .collect()
}

/// Inclusion probability `n_h / N_h` of each stratum given the sampled flags.
pub fn inclusion_probabilities(
    docs: &[EvalDoc],
    sampled: &[bool],
) -> Result<BTreeMap<String, f64>> {
    ensure!(docs.len() == sampled.len(), "sampled flags length mismatch");
    let mut counts: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (d, &s) in docs.iter().zip(sampled) {
        let e = counts.entry(d.stratum.as_str()).or_default();
        e.0 += 1;
        e.1 += s as usize;
    }
    let mut out = BTreeMap::new();
    for (k, (n_pop, n_s)) in counts {
        ensure!(n_s > 0, "stratum {k} has no sampled document");
        out.insert(k.to_string(), n_s as f64 / n_pop as f64);
    }
    Ok(out)
}

/// Horvitz-Thompson weight `w_d / pi_h` for each sampled document (0 when
/// not sampled). Summed over the sample it estimates the full-population sum.
pub fn ht_weights(docs: &[EvalDoc], sampled: &[bool]) -> Result<Vec<f64>> {
    let w = balanced_weights(docs);
    let pi = inclusion_probabilities(docs, sampled)?;
    Ok(docs
        .iter()
        .zip(sampled)
        .zip(w)
        .map(|((d, &s), w)| if s { w / pi[&d.stratum] } else { 0.0 })
        .collect())
}

/// Per-group HT values `v_g = G * sum_{sampled d in g} w_d value_d / pi_h`,
/// one per population group (0 for a group with no sampled document), in
/// group-key order. Their mean is the HT estimate of the full-population
/// balanced mean, and they are the resampling units of [`bootstrap_interval`].
pub fn ht_group_values(docs: &[EvalDoc], sampled: &[bool]) -> Result<Vec<(String, f64)>> {
    let ht = ht_weights(docs, sampled)?;
    let mut groups: BTreeMap<&str, f64> = BTreeMap::new();
    for d in docs {
        groups.entry(d.group.as_str()).or_default();
    }
    let g = groups.len() as f64;
    for (d, w) in docs.iter().zip(ht) {
        *groups.get_mut(d.group.as_str()).unwrap() += g * w * d.value;
    }
    Ok(groups
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect())
}

/// The protocol bootstrap: 1,000 draws of the mean of resampled group values
/// with xorshift64 (<<13, >>7, <<17) seeded 20261026; returns
/// `(draws[24], draws[974])`, as `compare_supervised_pangram.rs ci()`.
pub fn bootstrap_interval(values: &[f64]) -> Option<(f64, f64)> {
    if values.len() < 2 {
        return None;
    }
    let mut seed = 20261026u64;
    let mut draws = Vec::with_capacity(1000);
    for _ in 0..1000 {
        let mut total = 0.0;
        for _ in values {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            total += values[seed as usize % values.len()];
        }
        draws.push(total / values.len() as f64);
    }
    draws.sort_by(f64::total_cmp);
    Some((draws[24], draws[974]))
}

fn binomial_cdf(k: u64, n: u64, p: f64) -> f64 {
    if p <= 0.0 {
        return 1.0;
    }
    if p >= 1.0 {
        return if k >= n { 1.0 } else { 0.0 };
    }
    let (lp, lq) = (p.ln(), (1.0 - p).ln());
    let mut ln_c = 0.0;
    let mut sum = 0.0;
    for i in 0..=k.min(n) {
        if i > 0 {
            ln_c += ((n - i + 1) as f64).ln() - (i as f64).ln();
        }
        sum += (ln_c + i as f64 * lp + (n - i) as f64 * lq).exp();
    }
    sum.min(1.0)
}

/// One-sided Clopper-Pearson upper bound at `level` (for example 0.95) for
/// `k` events in `n` trials: the `p` with `P(X <= k | n, p) = 1 - level`.
pub fn clopper_pearson_upper(k: u64, n: u64, level: f64) -> f64 {
    assert!(n > 0 && k <= n && level > 0.0 && level < 1.0);
    if k == n {
        return 1.0;
    }
    let alpha = 1.0 - level;
    let (mut lo, mut hi) = (k as f64 / n as f64, 1.0);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if binomial_cdf(k, n, mid) > alpha {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi
}

/// Largest flag count whose one-sided Clopper-Pearson bound is at most `bound`.
pub fn max_flags(n: u64, level: f64, bound: f64) -> Option<u64> {
    (0..=n)
        .take_while(|&k| clopper_pearson_upper(k, n, level) <= bound)
        .last()
}

/// Planning standard error `sqrt(deff * sum_h W_h^2 S_h^2 / n_h)` of a
/// stratified estimate of a balanced mean (independent documents, no finite
/// population correction, times a clustering design effect).
pub fn stratified_se(strata: &[Stratum], allocation: &[usize], deff: f64) -> Result<f64> {
    ensure!(
        strata.len() == allocation.len(),
        "allocation length mismatch"
    );
    let mut v = 0.0;
    for (s, &n) in strata.iter().zip(allocation) {
        if s.weight * s.sd > 0.0 {
            ensure!(n > 0, "stratum {} has mass but no allocation", s.key);
            v += s.weight * s.weight * s.sd * s.sd / n as f64;
        }
    }
    Ok((deff * v).sqrt())
}

/// Clustering design effect of the balanced (equal group mass) estimate,
/// `1 + rho * f * sum_g (1 - 1/M_g) / sum_g (1/M_g)`, for sampling fraction
/// `f` and group sizes `M_g` (documents per group). Each group has equal mass
/// and its documents share it, so a document weighs `1 / (G * M_g)`; the
/// within-group pair term is `sum_g M_g (M_g - 1) w_g^2` against the
/// independent term `sum_g M_g w_g^2`. With equal group sizes this equals the
/// document-weighted form `1 + rho * f * (M - 1)`.
pub fn cluster_deff(rho: f64, fraction: f64, group_sizes: &[(usize, usize)]) -> f64 {
    // group_sizes: (documents per group, number of such groups)
    let (mut num, mut den) = (0.0, 0.0);
    for &(m, count) in group_sizes {
        if m == 0 {
            continue;
        }
        let inv = 1.0 / m as f64;
        num += count as f64 * (1.0 - inv);
        den += count as f64 * inv;
    }
    if den == 0.0 {
        1.0
    } else {
        1.0 + rho.max(0.0) * fraction * num / den
    }
}

/// Smallest total `n` (searched upward from the floors) whose Neyman
/// allocation has planning SE at most `target_se`; `deff` takes the sampling
/// fraction `n / N`.
pub fn required_total(
    strata: &[Stratum],
    floor: usize,
    target_se: f64,
    deff: impl Fn(f64) -> f64,
) -> Result<Option<usize>> {
    let pop: usize = strata.iter().map(|s| s.population).sum();
    let start: usize = strata
        .iter()
        .map(|s| s.population.min(floor))
        .sum::<usize>()
        .max(1);
    for n in start..=pop {
        let a = neyman_allocation(strata, n, floor)?;
        if stratified_se(strata, &a, deff(n as f64 / pop as f64))? <= target_se {
            return Ok(Some(n));
        }
    }
    Ok(None)
}

/// Standard normal CDF (Abramowitz-Stegun 7.1.26 erf; absolute error < 1.5e-7).
pub fn normal_cdf(x: f64) -> f64 {
    let z = x / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * z.abs());
    let y = 1.0
        - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t
            + 0.254_829_592)
            * t
            * (-z * z).exp();
    let erf = if z >= 0.0 { y } else { -y };
    0.5 * (1.0 + erf)
}

/// Power of the noninferiority gate (upper 97.5% bound at most `delta`) when
/// the true difference is `true_diff` and the estimate has standard error `se`.
pub fn noninferiority_power(se: f64, delta: f64, true_diff: f64) -> f64 {
    normal_cdf((delta - true_diff) / se - Z_975)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(key: &str, n: usize, w: f64, sd: f64) -> Stratum {
        Stratum {
            key: key.into(),
            population: n,
            weight: w,
            sd,
        }
    }

    #[test]
    fn share_bins_match_protocol_edges() {
        let cases = [
            (0.05, 0),
            (0.10, 1),
            (0.2499, 1),
            (0.25, 2),
            (0.5, 3),
            (0.75, 4),
            (0.9, 5),
            (0.9999, 5),
            (1.0, 6),
        ];
        for (s, b) in cases {
            assert_eq!(share_bin(s).unwrap(), b, "share {s}");
        }
        assert!(share_bin(0.0).is_err());
        assert!(share_bin(1.01).is_err());
        assert!(share_bin(f64::NAN).is_err());
        assert_eq!(
            stratum_key("wikipedia", Cell::Mixed, Some(0.3)).unwrap(),
            "wikipedia|mixed:[25,50)"
        );
        assert_eq!(
            stratum_key("plos", Cell::HumanOnly, None).unwrap(),
            "plos|human_only"
        );
        assert!(stratum_key("plos", Cell::Mixed, None).is_err());
    }

    #[test]
    fn neyman_allocation_is_proportional_and_exact() {
        let s = [
            st("a", 1000, 0.5, 1.0),
            st("b", 1000, 0.25, 2.0),
            st("c", 1000, 0.25, 0.0),
        ];
        let a = neyman_allocation(&s, 103, 3).unwrap();
        // c has zero score and gets its floor; a and b split 100 equally.
        assert_eq!(a, vec![50, 50, 3]);
        assert_eq!(a.iter().sum::<usize>(), 103);
    }

    #[test]
    fn neyman_allocation_respects_caps_floors_and_ties() {
        // b would get 90 of 100 but has only 20 documents; a takes the rest.
        let s = [
            st("a", 500, 0.1, 1.0),
            st("b", 20, 0.9, 1.0),
            st("c", 2, 1e-9, 1.0),
        ];
        let a = neyman_allocation(&s, 100, 3).unwrap();
        assert_eq!(a, vec![78, 20, 2]);
        // Ties in fractional parts go to the smaller key.
        let s = [
            st("x", 10, 1.0, 1.0),
            st("y", 10, 1.0, 1.0),
            st("z", 10, 1.0, 1.0),
        ];
        assert_eq!(neyman_allocation(&s, 10, 0).unwrap(), vec![4, 3, 3]);
        assert!(neyman_allocation(&s, 31, 0).is_err());
        assert!(neyman_allocation(&s, 8, 3).is_err());
        assert_eq!(neyman_allocation(&s, 30, 3).unwrap(), vec![10, 10, 10]);
        // A capped stratum must not push the floors past the total.
        let s = [
            st("a", 5, 0.98, 1.0),
            st("b", 100, 0.01, 1.0),
            st("c", 100, 0.01, 1.0),
        ];
        assert_eq!(neyman_allocation(&s, 10, 3).unwrap(), vec![4, 3, 3]);
        // Releasing floors after a cap lets them take their Neyman share.
        let s = [
            st("a", 5, 0.6, 1.0),
            st("b", 100, 0.2, 1.0),
            st("c", 100, 0.2, 1.0),
        ];
        assert_eq!(neyman_allocation(&s, 45, 3).unwrap(), vec![5, 20, 20]);
    }

    #[test]
    fn selection_is_seeded_and_stratified() {
        let docs: Vec<(String, String)> = (0..40)
            .map(|i| {
                (
                    format!("d{i}"),
                    if i < 30 { "s1".into() } else { "s2".into() },
                )
            })
            .collect();
        let alloc = BTreeMap::from([("s1".to_string(), 5), ("s2".to_string(), 2)]);
        let a = select(&docs, &alloc, SUBSAMPLE_SEED, "test").unwrap();
        let b = select(&docs, &alloc, SUBSAMPLE_SEED, "test").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 7);
        assert_eq!(
            a.iter()
                .filter(|id| id[1..].parse::<usize>().unwrap() >= 30)
                .count(),
            2
        );
        let c = select(&docs, &alloc, SUBSAMPLE_SEED + 1, "test").unwrap();
        assert_ne!(a, c);
        let d = select(&docs, &alloc, SUBSAMPLE_SEED, "cal").unwrap();
        assert_ne!(a, d);
        // Key definition is fixed.
        let mut h = Sha256::new();
        h.update(b"slopninja-a-comp-pangram-subsample-v1:20261027:test:d0");
        let want: [u8; 32] = h.finalize().into();
        assert_eq!(selection_key(SUBSAMPLE_SEED, "test", "d0"), want);
        let bad = BTreeMap::from([("s1".to_string(), 31), ("s2".to_string(), 2)]);
        assert!(select(&docs, &bad, SUBSAMPLE_SEED, "test").is_err());
        let missing = BTreeMap::from([("s1".to_string(), 1)]);
        assert!(select(&docs, &missing, SUBSAMPLE_SEED, "test").is_err());
    }

    fn doc(g: &str, cell: Cell, stratum: &str, value: f64) -> EvalDoc {
        EvalDoc {
            group: g.into(),
            cell,
            stratum: stratum.into(),
            value,
        }
    }

    #[test]
    fn balanced_weights_follow_group_cell_rule() {
        let docs = [
            doc("g1", Cell::HumanOnly, "h", 1.0),
            doc("g1", Cell::HumanOnly, "h", 1.0),
            doc("g1", Cell::Mixed, "m", 1.0),
            doc("g2", Cell::ModelOnly, "o", 1.0),
        ];
        let w = balanced_weights(&docs);
        assert_eq!(w, vec![0.125, 0.125, 0.25, 0.5]);
        assert!((w.iter().sum::<f64>() - 1.0).abs() < 1e-15);
    }

    #[test]
    fn full_sample_ht_equals_balanced_mean_and_partial_is_unbiased() {
        let docs = [
            doc("g1", Cell::HumanOnly, "h", 0.2),
            doc("g1", Cell::ModelOnly, "o", 0.0),
            doc("g1", Cell::Mixed, "m", 0.6),
            doc("g2", Cell::HumanOnly, "h", 0.1),
            doc("g2", Cell::ModelOnly, "o", 0.3),
            doc("g2", Cell::Mixed, "m", 0.3),
        ];
        let full = balanced_weights(&docs)
            .iter()
            .zip(&docs)
            .map(|(w, d)| w * d.value)
            .sum::<f64>();
        let all = ht_group_values(&docs, &[true; 6]).unwrap();
        let mean = all.iter().map(|x| x.1).sum::<f64>() / all.len() as f64;
        assert!((mean - full).abs() < 1e-15);
        // Enumerate every one-per-stratum sample: the HT mean averages to the truth.
        let mut avg = 0.0;
        let mut count = 0.0;
        for h in 0..2 {
            for o in 0..2 {
                for m in 0..2 {
                    let mut s = [false; 6];
                    s[h * 3] = true;
                    s[o * 3 + 1] = true;
                    s[m * 3 + 2] = true;
                    let v = ht_group_values(&docs, &s).unwrap();
                    avg += v.iter().map(|x| x.1).sum::<f64>() / v.len() as f64;
                    count += 1.0;
                }
            }
        }
        assert!((avg / count - full).abs() < 1e-12);
        assert!(ht_group_values(&docs, &[true, true, false, true, true, false]).is_err());
    }

    #[test]
    fn bootstrap_matches_reference_generator() {
        let v: Vec<f64> = (0..20).map(|i| i as f64).collect();
        let (lo, hi) = bootstrap_interval(&v).unwrap();
        assert!(lo < 9.5 && hi > 9.5 && lo > 5.0 && hi < 14.0);
        assert_eq!(bootstrap_interval(&v), bootstrap_interval(&v));
        assert!(bootstrap_interval(&[1.0]).is_none());
    }

    #[test]
    fn clopper_pearson_reproduces_protocol_flag_limits() {
        assert_eq!(max_flags(1200, 0.95, 0.025), Some(20));
        assert_eq!(max_flags(1280, 0.95, 0.025), Some(22));
        let u = clopper_pearson_upper(23, 1280, 0.95);
        assert!((u - 0.02536).abs() < 5e-5, "{u}");
        assert!((clopper_pearson_upper(0, 100, 0.95) - (1.0 - 0.05f64.powf(0.01))).abs() < 1e-9);
    }

    #[test]
    fn power_helpers_are_consistent() {
        assert!((normal_cdf(Z_975) - 0.975).abs() < 1e-6);
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-9);
        let se = 0.010 / (Z_975 + Z_90);
        assert!((noninferiority_power(se, 0.010, 0.0) - 0.90).abs() < 1e-5);
        let s = [st("a", 10_000, 0.5, 0.2), st("b", 10_000, 0.5, 0.1)];
        let n = required_total(&s, 3, 0.010_000_1, |_| 1.0)
            .unwrap()
            .unwrap();
        let a = neyman_allocation(&s, n, 3).unwrap();
        assert!(stratified_se(&s, &a, 1.0).unwrap() <= 0.010_000_1);
        let a1 = neyman_allocation(&s, n - 1, 3).unwrap();
        assert!(stratified_se(&s, &a1, 1.0).unwrap() > 0.010_000_1);
        // (sum W S)^2 / n = 0.15^2 / n <= 1e-4  =>  n = 225.
        assert_eq!(n, 225);
        assert!((cluster_deff(0.1, 0.5, &[(3, 10)]) - 1.1).abs() < 1e-12);
        assert_eq!(cluster_deff(-0.2, 0.5, &[(3, 10)]), 1.0);
        // One very large group carries only its own group mass.
        let mixed = cluster_deff(0.1, 0.5, &[(3, 10), (600, 1)]);
        let expect = 1.0 + 0.05 * (10.0 * (2.0 / 3.0) + 599.0 / 600.0) / (10.0 / 3.0 + 1.0 / 600.0);
        assert!((mixed - expect).abs() < 1e-12, "{mixed}");
        assert!(mixed < 1.2);
    }
}
