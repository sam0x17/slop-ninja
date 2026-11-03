//! `testnet-reward-v0`: testnet shortcut T8. Finney uses `reward-v1`
//! ([`crate::reward_v1`]). Records using it are valid only on
//! [`crate::TESTNET_GENESIS`]; every other chain fails closed.
//!
//! All arithmetic is integer. The rules, in order:
//!
//! 1. **Utility.** For predicted involvement `a` (parts per 10^9) and label
//!    `y`, `u = 10^18 - (a - 10^9 y)^2`, exactly. Only confidence Brier is
//!    rewarded; share has no reward weight under v0.
//! 2. **Attempt order.** Attempts are numbered 1, 2, ... in order. The first
//!    outcome that is not an infrastructure error is final. A model error,
//!    invalid output or parse failure has zero utility and keeps its weight. A
//!    task whose attempts are all infrastructure errors is unresolved and
//!    blocks the package rather than becoming a miner failure.
//! 3. **Aggregation.** Groups have equal weight and tasks within a group have
//!    equal weight: `S = floor(sum_g floor(sum_t u / n_g) / G)`. Every declared
//!    group must contain at least one task.
//! 4. **Improvement.** `q = floor((S - S0) 10^18 / (10^18 - S0))` when
//!    `S > S0` and `S0 < 10^18`, else 0. A perfect baseline rewards nobody.
//! 5. **Eligibility.** Only score entries whose exact `(uid, hotkey,
//!    registration_block)` is on the package allow-list earn credit. Others
//!    earn nothing and their share is not redistributed.
//! 6. **UID mapping.** Credit goes to the entry's UID when the validator's
//!    finalized registration snapshot holds the same identity at that UID.
//!    Otherwise (deregistration, UID reuse, hotkey change) the credit goes to
//!    UID 0. Allow-lists never contain UID 0.
//! 7. **Empty pool.** When no credit is positive, the row is `[(0, 65535)]`.
//!    UID 0 is the owner burn path: the manifest and package bind its exact
//!    burn identity, and the row is refused unless the validator's snapshot
//!    holds that identity at UID 0.
//! 8. **Quantization.** With `m` the largest credit, each UID's weight is
//!    `floor((2 c 65535 + m) / (2 m))`: `c 65535 / m` rounded half up. The
//!    largest credit gets exactly 65535, so the chain's max-upscale leaves the
//!    row unchanged; chain normalization by the sum then gives the share
//!    `c / sum(c)` up to quantization. Entries are independent, so equal
//!    credits always get equal weights and no tie order is needed. Zero
//!    weights are omitted and entries are sorted by UID.
//! 9. **Overflow.** Inputs are range-checked; every intermediate uses checked
//!    `u128` arithmetic and fails closed with [`Error::Overflow`].

use std::collections::BTreeMap;

use crate::records::{PROBABILITY_SCALE, Record};
use crate::{
    CalculationTarget, Error, Outcome, Probability, RegistrationSnapshot, Result, RowEntry,
    SCORE_SCALE, Score, ScorePackage, WeightRow,
};

/// Exact binary Brier utility, scaled by [`SCORE_SCALE`].
pub fn brier_utility(involvement: Probability, label: bool) -> u64 {
    let target = if label { PROBABILITY_SCALE } else { 0 };
    let d = u64::from(involvement.ppb().abs_diff(target));
    SCORE_SCALE - d * d
}

/// Applies the attempt-order rule to one task's attempts, given as
/// `(attempt number, outcome)` in increasing number order from 1.
/// Returns the predicted involvement, or `None` for an attributable failure.
pub fn final_involvement(attempts: &[(u8, &Outcome)]) -> Result<Option<Probability>> {
    for (position, (number, outcome)) in attempts.iter().enumerate() {
        if usize::from(*number) != position + 1 {
            return Err(Error::Unsorted("attempt number"));
        }
        match outcome {
            Outcome::InfrastructureError => continue,
            Outcome::Prediction { involvement, .. } => return Ok(Some(*involvement)),
            Outcome::ModelError | Outcome::InvalidOutput | Outcome::ParseFailure => {
                return Ok(None);
            }
        }
    }
    Err(Error::Invalid("unresolved infrastructure failure"))
}

/// One assigned task's resolved outcome and private label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScoredTask {
    pub group: u32,
    /// `None` is an attributable failure with zero utility.
    pub involvement: Option<Probability>,
    pub label: bool,
}

/// Source-balanced utility `S` over every assigned task.
pub fn aggregate_score(tasks: &[ScoredTask], group_count: u32) -> Result<Score> {
    if group_count == 0 {
        return Err(Error::Invalid("group_count"));
    }
    let mut groups = vec![(0u128, 0u64); group_count as usize];
    for task in tasks {
        let slot = groups
            .get_mut(task.group as usize)
            .ok_or(Error::OutOfRange("task group"))?;
        let utility = task.involvement.map_or(0, |a| brier_utility(a, task.label));
        slot.0 = slot
            .0
            .checked_add(u128::from(utility))
            .ok_or(Error::Overflow("group sum"))?;
        slot.1 += 1;
    }
    let mut total = 0u128;
    for (sum, n) in groups {
        if n == 0 {
            return Err(Error::Invalid("empty group"));
        }
        total = total
            .checked_add(sum / u128::from(n))
            .ok_or(Error::Overflow("group means"))?;
    }
    Score::new((total / u128::from(group_count)) as u64)
}

/// Baseline-relative improvement `q`, scaled by [`SCORE_SCALE`].
pub fn improvement(score: Score, baseline: Score) -> u64 {
    if baseline.get() >= SCORE_SCALE || score <= baseline {
        return 0;
    }
    let gain = u128::from(score.get() - baseline.get());
    let headroom = u128::from(SCORE_SCALE - baseline.get());
    (gain * u128::from(SCORE_SCALE) / headroom) as u64
}

/// Quantizes positive per-UID credits (sorted by UID) into native `u16` weights.
pub fn quantize(credits: &[(u16, u128)]) -> Result<Vec<RowEntry>> {
    let max = credits.iter().map(|(_, c)| *c).max().unwrap_or(0);
    if max == 0 {
        return Ok(vec![RowEntry {
            uid: 0,
            weight: u16::MAX,
        }]);
    }
    let denominator = max.checked_mul(2).ok_or(Error::Overflow("quantize"))?;
    let mut row = Vec::with_capacity(credits.len());
    for &(uid, credit) in credits {
        let numerator = credit
            .checked_mul(2 * u128::from(u16::MAX))
            .and_then(|n| n.checked_add(max))
            .ok_or(Error::Overflow("quantize"))?;
        let weight = (numerator / denominator) as u16;
        if weight > 0 {
            row.push(RowEntry { uid, weight });
        }
    }
    Ok(row)
}

/// Derives the canonical row for `target` from its package and a validator's
/// finalized registration snapshot.
pub fn derive_row(
    target: &CalculationTarget,
    package: &ScorePackage,
    snapshot: &RegistrationSnapshot,
) -> Result<WeightRow> {
    target.validate()?;
    package.validate()?;
    snapshot.validate()?;
    target.check_package(package)?;
    if snapshot.chain != package.chain {
        return Err(Error::Mismatch("registration chain"));
    }
    package.reward_rule.check_chain(&package.chain)?;
    // Snapshots are sorted by UID and the burn identity's UID is 0.
    if snapshot.neurons.first() != Some(&package.burn_identity) {
        return Err(Error::Mismatch("burn identity"));
    }
    let mut credits = BTreeMap::<u16, u128>::new();
    for entry in &package.scores {
        if package.allow_list.binary_search(&entry.identity).is_err() {
            continue;
        }
        let q = improvement(entry.score, package.baseline);
        if q == 0 {
            continue;
        }
        let registered = snapshot
            .neurons
            .binary_search_by_key(&entry.identity.uid, |n| n.uid)
            .is_ok_and(|i| snapshot.neurons[i] == entry.identity);
        let uid = if registered { entry.identity.uid } else { 0 };
        let slot = credits.entry(uid).or_default();
        *slot = slot
            .checked_add(u128::from(q))
            .ok_or(Error::Overflow("credit"))?;
    }
    let credits: Vec<_> = credits.into_iter().collect();
    let row = WeightRow {
        chain: package.chain.clone(),
        calculation_target: target.digest()?,
        reward_rule: package.reward_rule,
        registration_block: snapshot.block,
        registration: snapshot.digest()?,
        entries: quantize(&credits)?,
    };
    row.validate()?;
    Ok(row)
}
