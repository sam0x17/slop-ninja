//! Fixed, post-filter fixtures using pinned production Subtensor math functions.
//! This module does not execute a runtime, read chain state, or select defaults.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use substrate_fixed::types::{I32F32, I64F64, I96F32};

// Only these division methods from the pinned Apache-2.0 safe-math source are
// needed by math.rs. Method bodies are unchanged; unrelated SDK methods are not
// compiled. CheckedAdd in the runtime is the num-traits trait used here.
#[allow(dead_code)]
mod compatibility {
    pub trait SafeDiv {
        fn safe_div_or(self, rhs: Self, def: Self) -> Self;
        fn safe_div(self, rhs: Self) -> Self;
    }
    macro_rules! impl_safe_div_for_primitive {
        ($($t:ty),*) => {$(
            impl SafeDiv for $t {
                fn safe_div_or(self, rhs: Self, def: Self) -> Self {
                    self.checked_div(rhs).unwrap_or(def)
                }
                fn safe_div(self, rhs: Self) -> Self {
                    self.checked_div(rhs).unwrap_or_default()
                }
            }
        )*};
    }
    impl_safe_div_for_primitive!(u8, u16, u32, u64, u128, i8, i16, i32, i64, usize);
    pub trait FixedExt: substrate_fixed::traits::Fixed {
        fn safe_div_or(&self, rhs: Self, def: Self) -> Self {
            self.checked_div(rhs).unwrap_or(def)
        }
        fn safe_div(&self, rhs: Self) -> Self {
            self.checked_div(rhs).unwrap_or_default()
        }
    }
    impl<T: substrate_fixed::traits::Fixed> FixedExt for T {}
}
use compatibility::FixedExt;

#[rustfmt::skip]
#[allow(dead_code, unused_imports, clippy::all)]
#[path = "../upstream/math_standalone.rs"]
mod math;

type Sparse = Vec<Vec<(u16, I32F32)>>;
const N: u16 = 5;
const EMISSION: u64 = 1_000_000_000;
const COMMIT: &str = "67dcf7f791dc495064c293f080a0702cb433e51e";

fn source_checks() -> Value {
    let original = include_str!("../upstream/source/pallets/subtensor/src/epoch/math.rs");
    let adapted = include_str!("../upstream/math_standalone.rs");
    let replacements = [
        (
            "use crate::alloc::borrow::ToOwned;",
            "use std::borrow::ToOwned;",
        ),
        ("use safe_math::*;", "use super::compatibility::*;"),
        (
            "use sp_runtime::traits::CheckedAdd;",
            "use num_traits::CheckedAdd;",
        ),
        ("use sp_std::vec;", "use std::vec;"),
        ("use sp_std::vec::Vec;", "use std::vec::Vec;"),
    ];
    let mut transformed = original.to_owned();
    for (before, after) in replacements {
        assert_eq!(original.matches(before).count(), 1);
        transformed = transformed.replace(before, after);
    }
    assert_eq!(
        transformed, adapted,
        "only the five import replacements are allowed"
    );
    let hashes = [
        (
            "math.rs",
            original,
            "a445d0da2e66654cefb16965d3b0cef712f6414c8b8c594a658fd4c17eb698c2",
        ),
        (
            "math_standalone.rs",
            adapted,
            "860ed4bdef9df29f2f7e0d761ba24645cb367284ec366dd652c2bc727d186639",
        ),
        (
            "run_epoch.rs",
            include_str!("../upstream/source/pallets/subtensor/src/epoch/run_epoch.rs"),
            "222c57244ad24ebaffe79430c4e6b1897d87d2710f591bff3f2c284bd4a2f0a3",
        ),
    ];
    let mut checks = Vec::new();
    for (path, text, expected) in hashes {
        let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
        assert_eq!(hash, expected, "source digest: {path}");
        checks.push(json!({"path":path,"sha256":hash}));
    }
    json!({"passed":true,"import_replacements":5,"sources":checks})
}

fn ratio(n: u64, d: u64) -> I32F32 {
    I32F32::from_num(n).safe_div(I32F32::from_num(d))
}
fn vector(values: &[I32F32]) -> Value {
    json!({"values":values.iter().map(|v| v.to_num::<f64>()).collect::<Vec<_>>(),
        "i32f32_bits":values.iter().map(|v| v.to_bits()).collect::<Vec<_>>()})
}
fn matrix(values: &Sparse) -> Value {
    Value::Array(
        values
            .iter()
            .map(|row| {
                Value::Array(row.iter().map(|(uid,v)|
        json!({"uid":uid,"value":v.to_num::<f64>(),"i32f32_bits":v.to_bits()})
    ).collect())
            })
            .collect(),
    )
}
fn encoded_row(raw: [u16; 3]) -> (Vec<u16>, Vec<(u16, I32F32)>) {
    let encoded = math::vec_u16_max_upscale_to_u16(&raw);
    let row = encoded
        .iter()
        .enumerate()
        .filter(|(_, w)| **w > 0)
        .map(|(j, w)| ((j + 2) as u16, I32F32::from_num(*w)))
        .collect();
    (encoded, row)
}
fn stored_bonds(yuma3: bool) -> (Vec<Vec<(u16, u16)>>, Sparse) {
    let mut old = if yuma3 {
        vec![
            vec![(2, ratio(1, 10)), (3, ratio(1, 10)), (4, ratio(8, 10))],
            vec![(2, ratio(1, 10))],
            vec![],
            vec![],
            vec![],
        ]
    } else {
        vec![
            vec![
                (2, ratio(8, 10)),
                (3, I32F32::from_num(1)),
                (4, I32F32::from_num(1)),
            ],
            vec![(2, ratio(2, 10))],
            vec![],
            vec![],
            vec![],
        ]
    };
    if !yuma3 {
        math::inplace_col_max_upscale_sparse(&mut old, N);
    }
    let stored: Vec<Vec<(u16, u16)>> = old
        .iter()
        .map(|r| {
            r.iter()
                .map(|(j, v)| (*j, math::fixed_proportion_to_u16(*v)))
                .collect()
        })
        .collect();
    let loaded = stored
        .iter()
        .map(|r| {
            r.iter()
                .map(|(j, v)| {
                    (
                        *j,
                        if yuma3 {
                            math::u16_proportion_to_fixed(*v)
                        } else {
                            I32F32::from_num(*v)
                        },
                    )
                })
                .collect()
        })
        .collect();
    (stored, loaded)
}

fn fixture(a: u16, yuma3: bool, beta: u64, malicious_matches_honest: bool) -> Value {
    let honest_raw = [a, 20 - a, 80];
    let malicious_raw = if malicious_matches_honest {
        honest_raw
    } else {
        [1, 1, 0]
    };
    let (honest_u16, honest_row) = encoded_row(honest_raw);
    let (malicious_u16, malicious_row) = encoded_row(malicious_raw);
    let mut weights = vec![honest_row, malicious_row, vec![], vec![], vec![]];
    math::inplace_row_normalize_sparse(&mut weights);
    // Runtime first normalizes raw integer stake in I64F64, then converts it
    // to I32F32 and normalizes the active/permit-filtered vector again.
    let mut stake64 = vec![
        I64F64::from_num(8),
        I64F64::from_num(2),
        I64F64::from_num(0),
        I64F64::from_num(0),
        I64F64::from_num(0),
    ];
    math::inplace_normalize_64(&mut stake64);
    let mut stake = math::vec_fixed64_to_fixed32(stake64);
    math::inplace_normalize(&mut stake);
    let kappa = math::u16_proportion_to_fixed(32767);
    let consensus = math::weighted_median_col_sparse(&stake, &weights, N, kappa);
    let clipped = math::col_clip_sparse(&weights, &consensus);
    let mut incentive = math::matmul_sparse(&clipped, &stake, N);
    math::inplace_normalize(&mut incentive);
    let for_bonds = math::interpolate_sparse(&weights, &clipped, N, I32F32::from_num(1));
    let (old_stored, mut old) = stored_bonds(yuma3);
    let beta_fixed = I64F64::from_num(beta).safe_div(I64F64::from_num(1_000_000));
    let alpha = I32F32::from_num(1).saturating_sub(I32F32::from_num(beta_fixed));
    let mut bonds;
    let mut dividends;
    if yuma3 {
        bonds = math::mat_ema_sparse(&for_bonds, &old, alpha);
        let mut normalized_bonds = bonds.clone();
        math::inplace_col_normalize_sparse(&mut normalized_bonds, N);
        let total = math::row_sum_sparse(&math::mat_vec_mul_sparse(&normalized_bonds, &incentive));
        dividends = math::vec_mul(&total, &stake);
        math::inplace_normalize(&mut dividends);
    } else {
        math::inplace_col_normalize_sparse(&mut old, N);
        let mut delta = math::row_hadamard_sparse(&for_bonds, &stake);
        math::inplace_col_normalize_sparse(&mut delta, N);
        bonds = math::mat_ema_sparse(&delta, &old, alpha);
        math::inplace_col_normalize_sparse(&mut bonds, N);
        dividends = math::matmul_transpose_sparse(&bonds, &incentive);
        math::inplace_normalize(&mut dividends);
    }
    let bonds_for_dividends = bonds.clone();
    if !yuma3 {
        math::inplace_col_max_upscale_sparse(&mut bonds, N);
    }
    let new_stored: Vec<Vec<(u16, u16)>> = bonds
        .iter()
        .map(|r| {
            r.iter()
                .map(|(j, v)| (*j, math::fixed_proportion_to_u16(*v)))
                .collect()
        })
        .collect();
    let combined: Vec<I32F32> = incentive
        .iter()
        .zip(&dividends)
        .map(|(i, d)| i.saturating_add(*d))
        .collect();
    let sum: I32F32 = combined.iter().sum();
    let mut miner = incentive.clone();
    let mut validator = dividends.clone();
    math::inplace_normalize_using_sum(&mut miner, sum);
    math::inplace_normalize_using_sum(&mut validator, sum);
    let emit = |v: &I32F32| {
        I96F32::from_num(*v)
            .saturating_mul(I96F32::from_num(EMISSION))
            .saturating_to_num::<u64>()
    };
    let miner_units: Vec<u64> = miner.iter().map(emit).collect();
    let validator_units: Vec<u64> = validator.iter().map(emit).collect();
    let coalition_miner = incentive[2].saturating_add(incentive[3]);
    let coalition_total = miner[2]
        .saturating_add(miner[3])
        .saturating_add(validator[1]);
    json!({
        "branch":if yuma3 {"yuma3"} else {"classic"},"liquid_alpha_on":false,
        "bonds_moving_average":beta,"bonds_penalty":65535,"kappa":32767,
        "honest_input_ratio":honest_raw,"malicious_input_ratio":malicious_raw,
        "encoded_weight_rows_u16":[honest_u16,malicious_u16],
        "normalized_weights":matrix(&weights),"active_stake":vector(&stake),
        "alpha_new":vector(&[alpha]),"consensus":vector(&consensus),"clipped_weights":matrix(&clipped),
        "old_stored_bonds_u16":old_stored,"old_loaded_bonds":matrix(&old),
        "bonds_for_dividends":matrix(&bonds_for_dividends),"new_stored_bonds_u16":new_stored,
        "incentives":vector(&incentive),"dividends":vector(&dividends),
        "miner_emission_units":miner_units,"validator_emission_units":validator_units,
        "coalition_miner_incentive":coalition_miner.to_num::<f64>(),
        "coalition_miner_incentive_bits":coalition_miner.to_bits(),
        "coalition_validator_dividend":dividends[1].to_num::<f64>(),
        "coalition_gross_epoch_share":coalition_total.to_num::<f64>(),
        "coalition_gross_epoch_share_bits":coalition_total.to_bits(),
        "coalition_gross_emission_units":miner_units[2]+miner_units[3]+validator_units[1]
    })
}

pub fn run() -> Value {
    let historical: Vec<Value> = [false, true]
        .into_iter()
        .flat_map(|y| {
            [5, 15]
                .into_iter()
                .map(move |a| fixture(a, y, 900_000, false))
        })
        .collect();
    let current_only: Vec<Value> = [false, true]
        .into_iter()
        .flat_map(|matches| {
            [5, 15]
                .into_iter()
                .map(move |a| fixture(a, false, 0, matches))
        })
        .collect();
    let sweep:Vec<Value> = (1..20).map(|a| {
        let adversarial = fixture(a,false,0,false);
        let matches = fixture(a,false,0,true);
        json!({"honest_A_ratio_over_100":a,"fixed_concentrated_row_gross":adversarial["coalition_gross_epoch_share"],
            "matching_row_gross":matches["coalition_gross_epoch_share"],
            "matching_row_units":matches["coalition_gross_emission_units"],
            "fixed_concentrated_row_units":adversarial["coalition_gross_emission_units"]})
    }).collect();
    json!({"schema":"slop_ninja/native-shared-pool-replay-v1","upstream_commit":COMMIT,
        "source_checks":source_checks(),"uids":["honest_validator","coalition_validator","coalition_A","coalition_B","outside_miner"],
        "emission_budget_units":EMISSION,"runtime_executed":false,
        "scope":"Production math functions with explicit post-filter inputs and branch orchestration; no live state or SDK runtime execution",
        "filters_assumed":"All validators active/permitted; no diagonal votes; no registration or commitment masks; no owner exception used; no UID churn",
        "historical_bond_fixtures":historical,"current_only_classic_fixtures":current_only,
        "current_only_classic_u16_split_sensitivity":sweep,
        "liquid_alpha_enabled_tested":false})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_only_and_native_rounding() {
        assert_eq!(source_checks()["passed"], true);
        assert_eq!(
            math::vec_u16_max_upscale_to_u16(&[1, 2]),
            vec![32768, 65535]
        );
        assert_eq!(math::fixed_proportion_to_u16(ratio(1, 2)), 32767);
        assert_eq!(
            I32F32::from_num(1).safe_div(I32F32::from_num(0)),
            I32F32::from_num(0)
        );
    }
    #[test]
    fn historical_dividends_break_fixed_combined_miner_credit() {
        for yuma3 in [false, true] {
            let low = fixture(5, yuma3, 900_000, false);
            let high = fixture(15, yuma3, 900_000, false);
            assert_eq!(
                low["coalition_miner_incentive_bits"],
                high["coalition_miner_incentive_bits"]
            );
            assert!(
                high["coalition_gross_emission_units"].as_u64().unwrap()
                    > low["coalition_gross_emission_units"].as_u64().unwrap()
            );
        }
    }
    #[test]
    fn current_only_matching_row_exceeds_concentrated_row() {
        for a in 1..20 {
            let concentrated = fixture(a, false, 0, false);
            let matching = fixture(a, false, 0, true);
            assert!(
                matching["coalition_gross_emission_units"].as_u64().unwrap()
                    > concentrated["coalition_gross_emission_units"]
                        .as_u64()
                        .unwrap()
            );
        }
    }
}
