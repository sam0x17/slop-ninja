use serde_json::{Value, json};
use std::{env, fs};

mod failure;
mod native;

const EPS: f64 = 1e-10;

#[derive(Clone, Copy, Debug)]
struct Outcome {
    miner: f64,
    validator: f64,
    combined: f64,
}

// Ideal real-arithmetic classic branch, AFTER filtering and with an honest
// stake majority fixing each clipping ceiling. Native source math is separate.
fn classic(
    h: [f64; 3],
    malicious: [f64; 3],
    honest_stake: f64,
    new_bond_fraction: f64,
    old_bad_bonds: [f64; 3],
) -> Outcome {
    let bad_stake = 1.0 - honest_stake;
    let mut ranks = [0.0; 3];
    let mut bad_bonds = [0.0; 3];
    for j in 0..3 {
        let bad_contribution = bad_stake * malicious[j].min(h[j]);
        ranks[j] = honest_stake * h[j] + bad_contribution;
        let delta = if ranks[j] > 0.0 {
            bad_contribution / ranks[j]
        } else {
            0.0
        };
        bad_bonds[j] = (1.0 - new_bond_fraction) * old_bad_bonds[j] + new_bond_fraction * delta;
    }
    let total: f64 = ranks.iter().sum();
    assert!(total > 0.0);
    let incentives = ranks.map(|v| v / total);
    let miner = incentives[0] + incentives[1];
    let validator = (0..3).map(|j| incentives[j] * bad_bonds[j]).sum::<f64>();
    Outcome {
        miner,
        validator,
        combined: (miner + validator) / 2.0,
    }
}

fn rows(scale: u32) -> Vec<[f64; 3]> {
    let mut values = Vec::new();
    for a in 0..=scale {
        for b in 0..=scale - a {
            values.push([
                f64::from(a) / f64::from(scale),
                f64::from(b) / f64::from(scale),
                f64::from(scale - a - b) / f64::from(scale),
            ]);
        }
    }
    values
}

fn best(
    h: [f64; 3],
    s: f64,
    lambda: f64,
    old: [f64; 3],
    candidates: &[[f64; 3]],
) -> (Outcome, [f64; 3]) {
    let mut result = (classic(h, candidates[0], s, lambda, old), candidates[0]);
    for &m in candidates.iter().skip(1) {
        let next = classic(h, m, s, lambda, old);
        if next.combined > result.0.combined {
            result = (next, m);
        }
    }
    result
}

fn miner_envelope() -> Value {
    let candidates = rows(100);
    let mut checked = 0_u64;
    let mut max_excess = 0.0_f64;
    let mut witness_error = 0.0_f64;
    let mut examples = Vec::new();
    for s in [0.51, 0.6, 0.7, 0.8, 0.9, 1.0] {
        for c in [0.05, 0.2, 0.4, 0.7, 0.9] {
            let bound = c / (s + (1.0 - s) * c);
            for split in 0..=20 {
                let a = c * f64::from(split) / 20.0;
                let h = [a, c - a, 1.0 - c];
                for &m in &candidates {
                    let o = classic(h, m, s, 1.0, [0.0; 3]);
                    max_excess = max_excess.max(o.miner - bound);
                    assert!(o.miner <= bound + EPS);
                    checked += 1;
                }
                let optimal_m = [h[0] / c, h[1] / c, 0.0];
                let attained = classic(h, optimal_m, s, 1.0, [0.0; 3]).miner;
                witness_error = witness_error.max((attained - bound).abs());
                assert!((attained - bound).abs() < EPS);
                if s == 0.8 && c == 0.2 && [0, 5, 15, 20].contains(&split) {
                    examples.push(json!({"honest_row": h, "optimal_bad_row": optimal_m,
                        "maximum_coalition_miner_share": attained}));
                }
            }
        }
    }
    // Invariant OPTIMUM does not imply every fixed arbitrary row is invariant.
    let arbitrary_before = classic([0.0, 0.2, 0.8], [1.0, 0.0, 0.0], 0.7, 1.0, [0.0; 3]);
    let arbitrary_after = classic([0.2, 0.0, 0.8], [1.0, 0.0, 0.0], 0.7, 1.0, [0.0; 3]);
    assert!(arbitrary_after.miner > arbitrary_before.miner);
    json!({"cases": checked, "bad_weight_grid_step": 0.01,
        "honest_stakes": [0.51,0.6,0.7,0.8,0.9,1.0], "coalition_credit_fractions": [0.05,0.2,0.4,0.7,0.9],
        "internal_splits_per_fraction": 21, "bound": "c / (s + (1-s)*c)",
        "maximum_floating_excess_over_bound": max_excess,
        "maximum_error_at_explicit_optimal_witness": witness_error,
        "examples": examples,
        "fixed_arbitrary_bad_row_counterexample": {"bad_row": [1,0,0],
            "share_before": arbitrary_before.miner, "share_after": arbitrary_after.miner},
        "scope": "Ideal post-filter miner incentives only; fixed membership, stake, complete matching honest rows, fixed outside coordinates and clipping ceiling; no dividends or net cost claim."})
}

fn split_pool_reproduction() -> Value {
    // Old two-mechanism example: coalition occupies first coordinate only.
    let single = |h: [f64; 3]| {
        let rank = [h[0], 0.7 * h[1], 0.7 * h[2]];
        rank[0] / rank.iter().sum::<f64>()
    };
    let before = (single([0.2, 0.8, 0.0]) + single([0.0, 0.8, 0.2])) / 2.0;
    let after = (single([0.15, 0.8, 0.05]) + single([0.05, 0.8, 0.15])) / 2.0;
    assert!(after > before);
    json!({"coalition_combined_credit_fraction": 0.2,
        "equal_mechanism_shares": true, "before": before, "after": after,
        "increase_percentage_points": (after-before)*100.0})
}

fn dividend_search() -> Value {
    let candidates = rows(200);
    let mut results = Vec::new();
    for lambda in [0.0, 0.1, 0.5, 1.0] {
        let mut cases = Vec::new();
        for a in [0.05, 0.15] {
            let h = [a, 0.2 - a, 0.8];
            let (optimal, m) = best(h, 0.8, lambda, [0.2, 0.0, 0.0], &candidates);
            let fixed = classic(h, [0.5, 0.5, 0.0], 0.8, lambda, [0.2, 0.0, 0.0]);
            cases.push(json!({"honest_row": h,
                "grid_optimal_bad_row": m,
                "grid_maximum_coalition_miner_share": optimal.miner,
                "grid_maximum_coalition_validator_share": optimal.validator,
                "grid_maximum_combined_share": optimal.combined,
                "same_fixed_bad_row": [0.5,0.5,0],
                "fixed_row_miner_share": fixed.miner,
                "fixed_row_validator_share": fixed.validator,
                "fixed_row_combined_share": fixed.combined}));
        }
        let before = cases[0]["grid_maximum_combined_share"].as_f64().unwrap();
        let after = cases[1]["grid_maximum_combined_share"].as_f64().unwrap();
        if lambda == 0.1 {
            assert!((before - 0.126_785_714_285_714_28).abs() < EPS);
            assert!((after - 0.1375).abs() < EPS);
        }
        results.push(json!({"new_bond_fraction": lambda,
            "old_bad_bond_shares": [0.2,0,0], "cases": cases,
            "optimized_combined_share_gain": after-before}));
    }
    json!({"model": "Ideal classic Yuma with fully clipped bond deltas and normalized historical columns; equal miner/dividend buckets, owner cut omitted.",
        "bad_stake": 0.2, "bad_weight_grid_step": 0.005,
        "rows_searched_per_case": candidates.len(),
        "cases": results,
        "history": "One earlier A-only support epoch from empty classic bonds produces the old 0.2/0/0 normalized shares in exact arithmetic. Native fixtures quantize a supplied historical state; they do not execute that earlier epoch.",
        "limitation": "This search is a finite grid. The companion economic audit supplies an analytic optimum for the 0.1 fixture; production arithmetic is checked separately."})
}

fn current_only_search() -> Value {
    let candidates = rows(100);
    let mut groups = Vec::new();
    let mut max_error = 0.0_f64;
    let mut max_split_spread = 0.0_f64;
    for s in [0.6, 0.7, 0.8, 0.9] {
        for c in [0.1, 0.2, 0.4, 0.7, 0.9] {
            // With current, fully clipped classic bonds, optimize total
            // coalition miner+validator receipts, not miner receipts alone.
            let expected = if c <= s {
                (c + 1.0 - s) / 2.0
            } else {
                c * (2.0 - s) / (2.0 * (s + (1.0 - s) * c))
            };
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            for split in 0..=10 {
                let a = c * f64::from(split) / 10.0;
                let h = [a, c - a, 1.0 - c];
                let (o, _) = best(h, s, 1.0, [0.2, 0.0, 0.0], &candidates);
                assert!(o.combined <= expected + EPS);
                // Explicit continuous optimum is in addition to the finite grid.
                let witness = if c <= s { h } else { [a / c, (c - a) / c, 0.0] };
                let attained = classic(h, witness, s, 1.0, [0.2, 0.0, 0.0]).combined;
                assert!((attained - expected).abs() < EPS);
                max_error = max_error.max((attained - expected).abs());
                min = min.min(attained);
                max = max.max(attained);
            }
            max_split_spread = max_split_spread.max(max - min);
            groups.push(json!({"honest_stake": s, "coalition_credit": c,
                "maximum_combined_share": expected, "split_spread": max-min}));
        }
    }
    json!({"status": "Candidate configuration, NOT a deployment recommendation",
        "new_bond_fraction": 1, "legacy_bonds_fully_clipped": true,
        "groups": groups, "splits_per_group": 11,
        "bad_rows_per_grid": candidates.len(), "maximum_witness_error": max_error,
        "maximum_split_spread": max_split_spread,
        "optimal_combined_share": "(c+1-s)/2 for c<=s; c*(2-s)/(2*(s+(1-s)*c)) for c>s",
        "limitations": "Frozen identical post-filter honest rows, fixed eligible identities and stake, current fully clipped classic bonds only; no native rounding, withholding, cost or future-round proof."})
}

fn changed_assumptions() -> Value {
    let before = classic([0.05, 0.15, 0.8], [0.25, 0.75, 0.0], 0.8, 1.0, [0.0; 3]);
    let changed_stake = classic([0.05, 0.15, 0.8], [0.25, 0.75, 0.0], 0.7, 1.0, [0.0; 3]);
    // Removing a failed outsider and renormalizing earned rows breaks the fixed
    // outside-allocation hypothesis; retaining its allocation as burn does not.
    let excluded_outsider = classic([0.125, 0.375, 0.5], [0.25, 0.75, 0.0], 0.8, 1.0, [0.0; 3]);
    assert!(changed_stake.miner > before.miner);
    assert!(excluded_outsider.miner > before.miner);
    json!({"fixed_credits_changed_stake": {"honest_stake_before":0.8,"honest_stake_after":0.7,
        "miner_share_before":before.miner,"miner_share_after":changed_stake.miner},
        "unsafe_exclusion_and_renormalization": {"credit_before":0.2,"credit_after":0.5,
            "miner_share_before":before.miner,"miner_share_after":excluded_outsider.miner},
        "scope":"Sensitivity examples, not a demonstrated way for an attacker to cause these state changes. Membership, eligibility, actual filters and native timing need separate enforcement."})
}

fn main() {
    let output = env::args()
        .nth(1)
        .expect("usage: shared-pool-gameability OUTPUT.json");
    let result = json!({"schema":"slop_ninja/shared-pool-gameability-v1",
        "status":"Synthetic experiment; proposed settlement remains under evaluation",
        "split_pool":split_pool_reproduction(), "shared_pool_miner_envelope":miner_envelope(),
        "classic_dividend_search":dividend_search(), "classic_current_only":current_only_search(),
        "changed_assumptions":changed_assumptions(), "failure_economics":failure::run(),
        "pinned_native_math":native::run()});
    fs::write(
        output,
        format!("{}\n", serde_json::to_string_pretty(&result).unwrap()),
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_pool_optimum_and_split_pool_counterexample() {
        split_pool_reproduction();
        miner_envelope();
    }

    #[test]
    fn optimize_combined_receipts_with_and_without_bond_history() {
        dividend_search();
        current_only_search();
    }

    #[test]
    fn fixed_stake_and_outside_allocations_are_necessary_assumptions() {
        changed_assumptions();
    }
}
