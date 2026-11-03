//! Statistics: the protocol ChaCha20 bootstrap stream, weighted ROC metrics,
//! percentile intervals and the class-balanced L2 logistic fusion.
use anyhow::{Result, ensure};

/// ChaCha20 (20 rounds, original layout: 64-bit block counter in words 12-13,
/// 64-bit stream id 0 in words 14-15), keyed by a 32-byte seed. Output words
/// are consumed in keystream order, as `rand_chacha::ChaCha20Rng` does.
pub struct ChaCha20 {
    key: [u32; 8],
    counter: u64,
    buf: [u32; 16],
    pos: usize,
}

fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

pub fn chacha_block(key: &[u32; 8], w12: u32, w13: u32, w14: u32, w15: u32) -> [u32; 16] {
    let mut init = [0u32; 16];
    init[0] = 0x6170_7865;
    init[1] = 0x3320_646e;
    init[2] = 0x7962_2d32;
    init[3] = 0x6b20_6574;
    init[4..12].copy_from_slice(key);
    init[12] = w12;
    init[13] = w13;
    init[14] = w14;
    init[15] = w15;
    let mut s = init;
    for _ in 0..10 {
        quarter(&mut s, 0, 4, 8, 12);
        quarter(&mut s, 1, 5, 9, 13);
        quarter(&mut s, 2, 6, 10, 14);
        quarter(&mut s, 3, 7, 11, 15);
        quarter(&mut s, 0, 5, 10, 15);
        quarter(&mut s, 1, 6, 11, 12);
        quarter(&mut s, 2, 7, 8, 13);
        quarter(&mut s, 3, 4, 9, 14);
    }
    for i in 0..16 {
        s[i] = s[i].wrapping_add(init[i]);
    }
    s
}

impl ChaCha20 {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let mut key = [0u32; 8];
        for (i, k) in key.iter_mut().enumerate() {
            *k = u32::from_le_bytes(seed[4 * i..4 * i + 4].try_into().unwrap());
        }
        ChaCha20 {
            key,
            counter: 0,
            buf: [0; 16],
            pos: 16,
        }
    }

    pub fn next_u32(&mut self) -> u32 {
        if self.pos == 16 {
            self.buf = chacha_block(
                &self.key,
                self.counter as u32,
                (self.counter >> 32) as u32,
                0,
                0,
            );
            self.counter += 1;
            self.pos = 0;
        }
        let x = self.buf[self.pos];
        self.pos += 1;
        x
    }

    /// Uniform integer in [0, n): Lemire's multiply-shift with rejection.
    pub fn below(&mut self, n: u32) -> u32 {
        assert!(n > 0);
        let threshold = n.wrapping_neg() % n;
        loop {
            let m = self.next_u32() as u64 * n as u64;
            if (m as u32) >= threshold {
                return (m >> 32) as u32;
            }
        }
    }
}

/// One scored trial: `pos` is genuine (T1) or the positive class; `unit` is
/// the bootstrap unit (author) whose multiplicity scales the base weight `w`.
#[derive(Clone, Copy, Debug)]
pub struct Item {
    pub s: f64,
    pub pos: bool,
    pub unit: u32,
    pub w: f64,
}

/// Trials sorted by score with tie groups, evaluated under unit multiplicities.
pub struct Roc {
    items: Vec<Item>,
    groups: Vec<(u32, u32)>,
}

#[derive(Clone, Copy, Debug)]
pub struct RocOut {
    pub auc: f64,
    pub eer: f64,
    pub fpr90: f64,
    pub fpr95: f64,
}

impl Roc {
    pub fn new(mut items: Vec<Item>) -> Self {
        items.sort_by(|a, b| a.s.total_cmp(&b.s));
        let mut groups = Vec::new();
        let mut i = 0;
        while i < items.len() {
            let mut j = i + 1;
            while j < items.len() && items[j].s == items[i].s {
                j += 1;
            }
            groups.push((i as u32, j as u32));
            i = j;
        }
        Roc { items, groups }
    }

    fn group_weights(&self, g: (u32, u32), mult: Option<&[f64]>) -> (f64, f64) {
        let (mut p, mut n) = (0.0, 0.0);
        for it in &self.items[g.0 as usize..g.1 as usize] {
            let w = it.w * mult.map_or(1.0, |m| m[it.unit as usize]);
            if it.pos {
                p += w;
            } else {
                n += w;
            }
        }
        (p, n)
    }

    /// AUC (ties half), EER and FPR at TPR 0.90 / 0.95 by linear interpolation
    /// of the weighted ROC. NaN when either class has zero weight.
    pub fn eval(&self, mult: Option<&[f64]>) -> RocOut {
        let (mut tp_total, mut tn_total) = (0.0, 0.0);
        let mut auc_num = 0.0;
        let mut neg_below = 0.0;
        for &g in &self.groups {
            let (p, n) = self.group_weights(g, mult);
            auc_num += p * (neg_below + 0.5 * n);
            neg_below += n;
            tp_total += p;
            tn_total += n;
        }
        let nan = RocOut {
            auc: f64::NAN,
            eer: f64::NAN,
            fpr90: f64::NAN,
            fpr95: f64::NAN,
        };
        if tp_total <= 0.0 || tn_total <= 0.0 {
            return nan;
        }
        let auc = auc_num / (tp_total * tn_total);
        // Descending threshold sweep: ROC points (fpr, tpr) from (0, 0).
        let (mut tp, mut fp) = (0.0, 0.0);
        let (mut prev_fpr, mut prev_tpr) = (0.0f64, 0.0f64);
        let mut eer = f64::NAN;
        let mut fpr90 = f64::NAN;
        let mut fpr95 = f64::NAN;
        for &g in self.groups.iter().rev() {
            let (p, n) = self.group_weights(g, mult);
            tp += p;
            fp += n;
            let tpr = (tp / tp_total).min(1.0);
            let fpr = (fp / tn_total).min(1.0);
            if eer.is_nan() {
                let g0 = prev_fpr + prev_tpr - 1.0;
                let g1 = fpr + tpr - 1.0;
                if g1 >= 0.0 {
                    let lam = if g1 > g0 { -g0 / (g1 - g0) } else { 0.0 };
                    eer = prev_fpr + lam * (fpr - prev_fpr);
                }
            }
            for (target, slot) in [(0.90, &mut fpr90), (0.95, &mut fpr95)] {
                if slot.is_nan() && tpr >= target {
                    let lam = if tpr > prev_tpr {
                        (target - prev_tpr) / (tpr - prev_tpr)
                    } else {
                        0.0
                    };
                    *slot = prev_fpr + lam * (fpr - prev_fpr);
                }
            }
            prev_fpr = fpr;
            prev_tpr = tpr;
        }
        RocOut {
            auc,
            eer,
            fpr90,
            fpr95,
        }
    }
}

/// Percentile (type 7, linear interpolation) of sorted finite values.
pub fn quantile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let h = (sorted.len() - 1) as f64 * p;
    let lo = h.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    sorted[lo] + (h - lo as f64) * (sorted[hi] - sorted[lo])
}

pub struct Summary {
    pub n: usize,
    pub se: f64,
    pub ci95: (f64, f64),
    pub ci975: (f64, f64),
}

/// Bootstrap summary over replicates (non-finite replicates are dropped and
/// counted); SE is the replicate SD with n - 1.
pub fn summarize(reps: &[f64]) -> Summary {
    let mut v: Vec<f64> = reps.iter().copied().filter(|x| x.is_finite()).collect();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let mean = v.iter().sum::<f64>() / n.max(1) as f64;
    let se = if n > 1 {
        (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt()
    } else {
        f64::NAN
    };
    Summary {
        n,
        se,
        ci95: (quantile(&v, 0.025), quantile(&v, 0.975)),
        ci975: (quantile(&v, 0.0125), quantile(&v, 0.9875)),
    }
}

/// Class-balanced logistic regression with an intercept and an L2 penalty on
/// the two slopes: minimize sum_i w'_i logloss_i + lambda (b1^2 + b2^2), where
/// each class's weights are rescaled to total 0.5. Newton iterations.
pub fn logistic_fit(x: &[[f64; 2]], y: &[bool], w: &[f64], lambda: f64) -> Result<[f64; 3]> {
    ensure!(x.len() == y.len() && y.len() == w.len() && !x.is_empty(), "fusion inputs");
    let pos: f64 = y.iter().zip(w).filter(|(p, _)| **p).map(|(_, w)| w).sum();
    let neg: f64 = y.iter().zip(w).filter(|(p, _)| !**p).map(|(_, w)| w).sum();
    ensure!(pos > 0.0 && neg > 0.0, "fusion needs both classes");
    let wb: Vec<f64> = y
        .iter()
        .zip(w)
        .map(|(p, w)| if *p { 0.5 * w / pos } else { 0.5 * w / neg })
        .collect();
    let mut b = [0.0f64; 3];
    for _ in 0..200 {
        let mut g = [0.0f64; 3];
        let mut h = [[0.0f64; 3]; 3];
        for i in 0..x.len() {
            let f = [1.0, x[i][0], x[i][1]];
            let eta = b[0] + b[1] * f[1] + b[2] * f[2];
            let p = 1.0 / (1.0 + (-eta).exp());
            let r = p - if y[i] { 1.0 } else { 0.0 };
            let c = p * (1.0 - p);
            for a in 0..3 {
                g[a] += wb[i] * r * f[a];
                for bb in 0..3 {
                    h[a][bb] += wb[i] * c * f[a] * f[bb];
                }
            }
        }
        for a in 1..3 {
            g[a] += 2.0 * lambda * b[a];
            h[a][a] += 2.0 * lambda;
        }
        let step = solve3(h, g)?;
        let mut delta = 0.0f64;
        for a in 0..3 {
            b[a] -= step[a];
            delta = delta.max(step[a].abs());
        }
        if delta < 1e-13 {
            return Ok(b);
        }
    }
    ensure!(b.iter().all(|v| v.is_finite()), "fusion did not converge");
    Ok(b)
}

fn solve3(mut a: [[f64; 3]; 3], mut b: [f64; 3]) -> Result<[f64; 3]> {
    for c in 0..3 {
        let piv = (c..3)
            .max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))
            .unwrap();
        ensure!(a[piv][c].abs() > 1e-300, "singular fusion Hessian");
        a.swap(c, piv);
        b.swap(c, piv);
        for r in 0..3 {
            if r != c {
                let f = a[r][c] / a[c][c];
                for k in 0..3 {
                    a[r][k] -= f * a[c][k];
                }
                b[r] -= f * b[c];
            }
        }
    }
    Ok([b[0] / a[0][0], b[1] / a[1][1], b[2] / a[2][2]])
}

/// Weighted mean and population SD.
pub fn weighted_mean_sd(xs: &[(f64, f64)]) -> (f64, f64) {
    let w: f64 = xs.iter().map(|x| x.1).sum();
    let m = xs.iter().map(|x| x.0 * x.1).sum::<f64>() / w;
    let v = xs.iter().map(|x| x.1 * (x.0 - m).powi(2)).sum::<f64>() / w;
    (m, v.sqrt())
}
