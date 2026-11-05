//! Derives the minimum-success gate's `k_n` (`reward_v1.quant.min_success_max_failed`)
//! from a measured honest failure count, as `quant-v1` specifies:
//!
//! 1. the honest per-document failure rate's one-sided upper confidence
//!    bound `p_ub` (Clopper-Pearson, at `--confidence`, default 0.999) from
//!    `--failures` attributable failures in `--documents` honest documents;
//! 2. the target rate `alpha = 1 / (365 * MAX_ROSTER)`: at most one honest
//!    entrant gated a year at the protocol's full roster, one cohort a day;
//! 3. for each batch size `n` (`--n`, repeatable; default 1000, 2000 and
//!    4000), the smallest `k` with `P[Bin(n, p_ub) > k] <= alpha`.
//!
//! ```text
//! min_success_k --failures 1 --documents 254250 [--confidence 0.999] [--n 1000 ...]
//! ```
//!
//! It prints one JSON object: the inputs, `p_ub`, `alpha`, each `k_n` with its
//! tail probability, and a content-free `min_success_basis` line for the
//! plan. This is an offline planning tool: its floating-point arithmetic
//! never enters a consensus path, and the plan records only the integer
//! `k_n` and the basis text. Re-measure, and re-derive, when the honest
//! population changes (outside entrants arrive); a new `k_n` is a versioned
//! plan change.

use serde_json::json;

fn fail(msg: &str) -> ! {
    eprintln!("min_success_k: {msg}");
    std::process::exit(2)
}

/// `ln C(n, k)`.
fn ln_choose(n: u64, k: u64) -> f64 {
    ln_gamma(n as f64 + 1.0) - ln_gamma(k as f64 + 1.0) - ln_gamma((n - k) as f64 + 1.0)
}

/// Lanczos approximation of `ln Gamma(x)`, `x > 0`.
fn ln_gamma(x: f64) -> f64 {
    const G: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        let pi = std::f64::consts::PI;
        return (pi / (pi * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = G[0];
    let t = x + 7.5;
    for (i, g) in G.iter().enumerate().skip(1) {
        a += g / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

/// `P[Bin(n, p) = k]`.
fn pmf(n: u64, k: u64, p: f64) -> f64 {
    if p <= 0.0 {
        return if k == 0 { 1.0 } else { 0.0 };
    }
    (ln_choose(n, k) + k as f64 * p.ln() + (n - k) as f64 * (-p).ln_1p()).exp()
}

/// `P[Bin(n, p) <= k]`.
fn cdf(n: u64, k: u64, p: f64) -> f64 {
    (0..=k.min(n)).map(|j| pmf(n, j, p)).sum::<f64>().min(1.0)
}

/// The one-sided Clopper-Pearson upper bound: the `p` with
/// `P[Bin(n, p) <= x] = 1 - confidence`, by bisection.
fn upper_bound(x: u64, n: u64, confidence: f64) -> f64 {
    let target = 1.0 - confidence;
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if cdf(n, x, mid) > target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi
}

/// `P[Bin(n, p) > k]`, summed over the upper tail directly so it stays
/// accurate when it is tiny.
fn tail(n: u64, k: u64, p: f64) -> f64 {
    let mut sum = 0.0;
    for j in k + 1..=n {
        let term = pmf(n, j, p);
        sum += term;
        if term < sum * 1e-18 {
            break;
        }
    }
    sum
}

fn main() {
    let mut failures: Option<u64> = None;
    let mut documents: Option<u64> = None;
    let mut confidence = 0.999f64;
    let mut sizes: Vec<u64> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| fail(&format!("{a} needs a value")));
        match a.as_str() {
            "--failures" => failures = Some(value().parse().unwrap_or_else(|_| fail("--failures"))),
            "--documents" => documents = Some(value().parse().unwrap_or_else(|_| fail("--documents"))),
            "--confidence" => confidence = value().parse().unwrap_or_else(|_| fail("--confidence")),
            "--n" => sizes.push(value().parse().unwrap_or_else(|_| fail("--n"))),
            other => fail(&format!("unknown argument {other}")),
        }
    }
    let failures = failures.unwrap_or_else(|| fail("--failures is required"));
    let documents = documents.unwrap_or_else(|| fail("--documents is required"));
    if documents == 0 || failures >= documents || !(0.5..1.0).contains(&confidence) {
        fail("need 0 <= failures < documents and 0.5 <= confidence < 1");
    }
    if sizes.is_empty() {
        sizes = vec![1000, 2000, 4000];
    }
    let p_ub = upper_bound(failures, documents, confidence);
    let roster = u64::from(slop_ninja_protocol::MAX_ROSTER);
    let alpha = 1.0 / (365.0 * roster as f64);
    let ks: Vec<_> = sizes
        .iter()
        .map(|&n| {
            let k = (0..n).find(|&k| tail(n, k, p_ub) <= alpha).unwrap_or(n - 1);
            json!({"n": n, "k_n": k, "fires_above": k, "tail": tail(n, k, p_ub)})
        })
        .collect();
    let basis = format!(
        "{failures} attributable failures in {documents} honest documents; one-sided {}% Clopper-Pearson p_ub {p_ub:.3e}; \
         alpha 1/(365*{roster})",
        confidence * 100.0
    );
    let out = json!({
        "rule": "quant-v1 minimum-success gate",
        "failures": failures, "documents": documents, "confidence": confidence,
        "p_ub": p_ub, "alpha": alpha, "max_roster": roster,
        "k": ks,
        "min_success_basis": basis,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule's numbers: 1 failure in 254,250 documents gives
    /// `p_ub = 3.63e-5`, and `k_n` is 2, 3 and 4 at n = 1000, 2000 and 4000.
    #[test]
    fn reproduces_the_rule_values() {
        let p = upper_bound(1, 254_250, 0.999);
        assert!((p - 3.63e-5).abs() < 0.01e-5, "{p}");
        let alpha = 1.0 / (365.0 * f64::from(slop_ninja_protocol::MAX_ROSTER));
        let k = |n| (0..n).find(|&k| tail(n, k, p) <= alpha).unwrap();
        assert_eq!([k(1000), k(2000), k(4000)], [2, 3, 4]);
        // The bound grows with the failures and shrinks with the documents.
        assert!(upper_bound(2, 254_250, 0.999) > p && upper_bound(1, 500_000, 0.999) < p);
    }
}
