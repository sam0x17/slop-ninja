use serde_json::{Value, json};
use std::{collections::BTreeMap, env, fs};

const ROW: u64 = 60_000;
const BASE: u64 = 48_000;
const RESERVE: u64 = 12_000;
const SCALE: u64 = 1_000;

#[derive(Clone, Copy, Debug)]
struct Admission {
    preservation: bool,
    independent_positive_quality: bool,
    eligible_a: bool,
    eligible_b: bool,
    complete_evidence: bool,
}

impl Admission {
    fn from_mask(mask: u8) -> Self {
        Self {
            preservation: mask & 1 != 0,
            independent_positive_quality: mask & 2 != 0,
            eligible_a: mask & 4 != 0,
            eligible_b: mask & 8 != 0,
            complete_evidence: mask & 16 != 0,
        }
    }

    fn passes(self) -> bool {
        self.preservation
            && self.independent_positive_quality
            && self.eligible_a
            && self.eligible_b
            && self.complete_evidence
    }
}

// Slots are in their already frozen canonical order. Answers never enter this rule.
fn allocate(budget: u64, slots: usize) -> Vec<u64> {
    if slots == 0 {
        return Vec::new();
    }
    let slots_u64 = slots as u64;
    (0..slots_u64)
        .map(|i| budget / slots_u64 + u64::from(i < budget % slots_u64))
        .collect()
}

// Multiclass Brier loss is sum((p-y)^2)/(2*scale^2).
fn brier_numerator(probabilities: &[u64], label: usize, scale: u64) -> u64 {
    assert!(label < probabilities.len());
    assert_eq!(probabilities.iter().sum::<u64>(), scale);
    probabilities
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            let target = if i == label { scale } else { 0 };
            p.abs_diff(target).pow(2)
        })
        .sum()
}

fn pair_credit(
    c: u64,
    loss_numerator: u64,
    loss_denominator: u64,
    w: u64,
    m: Admission,
) -> [u64; 2] {
    assert!(w <= SCALE && loss_numerator <= loss_denominator);
    assert!(loss_denominator > 0);
    if !m.passes() {
        return [0, 0];
    }
    let b = c * w * loss_numerator / (SCALE * loss_denominator);
    [c - b, b]
}

// The third coordinate is the proposed native owner-associated burn destination.
fn target_row(base_earned: u64, paired_earned: u64) -> [u64; 3] {
    assert!(base_earned <= BASE && paired_earned <= RESERVE);
    let row = [
        base_earned,
        paired_earned,
        ROW - base_earned - paired_earned,
    ];
    assert_eq!(row.iter().sum::<u64>(), ROW);
    assert!(row.iter().all(|&x| u16::try_from(x).is_ok()));
    row
}

fn invariant_grid() -> Value {
    let mut checked = 0_u64;
    let mut admitted = 0_u64;
    let mut rounding_cases = 0_u64;
    for c in [0, 1, 2, 3, 7, RESERVE] {
        for w in [0, 1, 500, 999, SCALE] {
            for p in 0..=SCALE {
                for label in 0..2 {
                    let loss = brier_numerator(&[p, SCALE - p], label, SCALE);
                    for mask in 0..32 {
                        let m = Admission::from_mask(mask);
                        let [a, b] = pair_credit(c, loss, 2 * SCALE.pow(2), w, m);
                        assert_eq!(a + b, if m.passes() { c } else { 0 });
                        if m.passes() {
                            admitted += 1;
                            if !(c * w * loss).is_multiple_of(2 * SCALE.pow(3)) {
                                rounding_cases += 1;
                            }
                        }
                        checked += 1;
                    }
                }
            }
        }
    }
    let mut multiclass_checked = 0_u64;
    for p0 in 0..=20 {
        for p1 in 0..=20 - p0 {
            let p = [p0, p1, 20 - p0 - p1];
            for label in 0..3 {
                let loss = brier_numerator(&p, label, 20);
                let [a, b] = pair_credit(7, loss, 800, 999, Admission::from_mask(31));
                assert_eq!(a + b, 7);
                multiclass_checked += 1;
            }
        }
    }
    json!({"binary_cases": checked, "admitted_binary_cases": admitted,
        "admitted_nonintegral_rounding_cases": rounding_cases,
        "three_class_cases": multiclass_checked, "violations": 0,
        "probability_scale": SCALE, "weights": [0,1,500,999,1000],
        "pots": [0,1,2,3,7,12000], "admission_masks": 32})
}

fn allocation_results() -> Value {
    let allocations: Vec<Value> = [0, 1, 7, 11, 127]
        .into_iter()
        .map(|slots| {
            let pots = allocate(RESERVE, slots);
            let allocated = pots.iter().sum::<u64>();
            assert_eq!(allocated, if slots == 0 { 0 } else { RESERVE });
            let mut paid = [0_u64; 2];
            for (i, c) in pots.iter().enumerate() {
                let p = (i as u64 * 137) % (SCALE + 1);
                let loss = brier_numerator(&[p, SCALE - p], 0, SCALE);
                let credit =
                    pair_credit(*c, loss, 2 * SCALE.pow(2), SCALE, Admission::from_mask(31));
                paid[0] += credit[0];
                paid[1] += credit[1];
            }
            let rows = [target_row(BASE, paid[0]), target_row(BASE, paid[1])];
            assert_eq!(paid.iter().sum::<u64>(), allocated);
            json!({"slots": slots, "pot_min": pots.iter().min(), "pot_max": pots.iter().max(),
                "first_pots": pots.iter().take(12).collect::<Vec<_>>(),
                "paired_paid": paid, "rows_base_pair_burn": rows,
                "total_burn": rows[0][2] + rows[1][2]})
        })
        .collect();
    let rejected: Vec<Value> = [
        ("unchanged_no_positive_improvement", 29),
        ("preservation_failed", 30),
        ("a_ineligible", 27),
        ("b_ineligible", 23),
        ("evidence_missing", 15),
    ]
    .into_iter()
    .map(|(name, mask)| {
        let credit = pair_credit(RESERVE, 2, 2, SCALE, Admission::from_mask(mask));
        assert_eq!(credit, [0, 0]);
        json!({"case": name, "paired_credit": credit, "paired_burn": 2 * RESERVE})
    })
    .collect();
    let partial_base_rows = [target_row(BASE / 2, 0), target_row(0, 0)];
    json!({"mechanism_budget": ROW, "base_budget": BASE, "paired_budget": RESERVE,
        "allocations": allocations, "rejected_pairs": rejected,
        "partial_base_rows_base_pair_burn": partial_base_rows})
}

fn old_attack_results() -> Value {
    let normalized = |a: f64, b: f64| 0.5 * (a / (BASE as f64 + a) + b / (BASE as f64 + b));
    let before = normalized(12_000.0, 0.0);
    let after = normalized(9_000.0, 3_000.0);
    assert!(after > before);
    let epsilon = 0.01_f64;
    let a_loss = epsilon.powi(2);
    let b_gain = epsilon / 2.0;
    assert!(b_gain > a_loss);
    let candidate_loss = 0.25_f64;
    let delta_before = (candidate_loss - 0.0).max(0.0);
    let delta_after = (candidate_loss - 1.0).max(0.0);
    assert_eq!(delta_after, 0.0);
    json!({"normalized_separate_pools": {"coalition_share_before": before,
        "coalition_share_after": after, "raw_pair_sum_both": 12000},
        "brier_penalty_plus_linear_b_bonus": {"epsilon": epsilon,
        "a_brier_loss": a_loss, "b_linear_gain": b_gain, "coalition_gain": b_gain-a_loss},
        "source_baseline_poisoning": {"candidate_loss_fixed": candidate_loss,
        "source_loss_before": 0, "source_loss_after": 1,
        "delta_before": delta_before, "delta_after": delta_after,
        "a_credit_before": 9000, "a_credit_after": 12000,
        "candidate_only_a_credit_both": 9000}})
}

fn support(weights: &[u64], mask: usize) -> u64 {
    weights
        .iter()
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .map(|(_, w)| w)
        .sum()
}

fn quorum(signed: u64, total: u64) -> bool {
    assert!(signed <= total);
    total > 0 && u128::from(signed) * 3 > u128::from(total) * 2
}

fn quorum_results() -> Value {
    let mut admissible_vote_pairs = 0;
    let mut conflicts_below_or_at_third = 0;
    for weights in [
        &[1, 1, 1][..],
        &[1, 2, 3][..],
        &[0, 1, 2, 3][..],
        &[2, 3, 4, 5][..],
    ] {
        let total = weights.iter().sum::<u64>();
        let limit = 1_usize << weights.len();
        for bad in 0..limit {
            if support(weights, bad) * 3 > total {
                continue;
            }
            for votes_a in 0..limit {
                for votes_b in 0..limit {
                    // Only Byzantine signers may appear on both decisions.
                    if votes_a & votes_b & !bad != 0 {
                        continue;
                    }
                    admissible_vote_pairs += 1;
                    if quorum(support(weights, votes_a), total)
                        && quorum(support(weights, votes_b), total)
                    {
                        conflicts_below_or_at_third += 1;
                    }
                }
            }
        }
    }
    assert_eq!(conflicts_below_or_at_third, 0);
    assert!(!quorum(2, 3)); // Exactly one-third withholding blocks progress.
    assert!(quorum(3, 4)); // Two bad weight plus one distinct honest signer per verdict.
    assert!(!quorum(2, 4) && quorum(2, 2));
    assert!(!quorum(0, 0));
    json!({"admissible_vote_pairs": admissible_vote_pairs,
        "conflicting_certificates_with_bad_weight_at_most_one_third": conflicts_below_or_at_third,
        "exact_one_third": {"total": 3, "bad_withholding": 1, "honest_support": 2,
            "certificate": false, "failure": "liveness; safety still holds"},
        "above_one_third": {"weights": [2,1,1], "bad_signs_both": [0],
            "first_signers": [0,1], "second_signers": [0,2], "support_each": 3,
            "both_certify": true},
        "unsafe_responder_denominator": {"frozen_total": 4, "bad_weight": 1,
            "support_each": 2, "frozen_certifies": false, "responding_total": 2,
            "shrunk_certifies": true, "honest_double_signers": 0},
        "one_honest_verdict": "Below one-third, bad-only support cannot certify its contrary verdict.",
        "zero_weight_certificate": false})
}

#[derive(Clone, Copy, Debug, Default)]
struct Service {
    success: u64,
    failure: u64,
    unresolved: u64,
    platform_void: u64,
}

impl Service {
    fn settled(self) -> u64 {
        self.success + self.failure
    }
    fn activated(self) -> u64 {
        self.settled() + self.unresolved + self.platform_void
    }
    fn passes(self) -> bool {
        self.unresolved == 0 && self.settled() > 0 && 10 * self.failure <= self.settled()
    }
    fn deficit(self, previous: u64) -> u64 {
        (previous + 10 * self.failure).saturating_sub(self.settled())
    }
    fn record(self, previous: u64) -> Value {
        json!({"S": self.success, "F": self.failure, "U": self.unresolved,
            "V": self.platform_void, "N": self.settled(), "activated": self.activated(),
            "availability": self.passes(), "previous_deficit": previous,
            "new_deficit": self.deficit(previous)})
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputVerdict {
    Accepted,
    Rejected,
    Unresolved,
}

fn input_accounting(events: &[(u64, InputVerdict)]) -> (Service, u64) {
    let mut ledger = BTreeMap::new();
    for &(ticket, verdict) in events {
        if let Some(previous) = ledger.insert(ticket, verdict) {
            assert_eq!(
                previous, verdict,
                "conflicting final verdicts stop accounting"
            );
        }
    }
    let mut requester = Service::default();
    let mut activated_provider_obligations = 0;
    for verdict in ledger.values() {
        match verdict {
            InputVerdict::Accepted => {
                requester.success += 1;
                activated_provider_obligations += 1;
            }
            InputVerdict::Rejected => requester.failure += 1,
            InputVerdict::Unresolved => requester.unresolved += 1,
        }
    }
    (requester, activated_provider_obligations)
}

fn service_results() -> Value {
    let exact = Service {
        success: 9,
        failure: 1,
        ..Service::default()
    };
    let above = Service {
        success: 8,
        failure: 1,
        ..Service::default()
    };
    let unknown = Service {
        success: 9,
        unresolved: 1,
        ..Service::default()
    };
    let empty = Service::default();
    let finished = Service {
        success: 89,
        failure: 11,
        ..Service::default()
    };
    let abandoned = Service {
        failure: 100,
        ..Service::default()
    };
    let recovery = Service {
        success: 10,
        ..Service::default()
    };
    let (request_invalid, invalid_provider_count) = input_accounting(&[
        (1, InputVerdict::Rejected),
        (1, InputVerdict::Rejected),
        (1, InputVerdict::Rejected),
    ]);
    let (retried, accepted_provider_count) = input_accounting(&[
        (2, InputVerdict::Accepted),
        (2, InputVerdict::Accepted),
        (2, InputVerdict::Accepted),
    ]);
    let (unresolved_request, unresolved_provider_count) =
        input_accounting(&[(3, InputVerdict::Unresolved)]);
    let provider_not_activated = Service::default();
    assert!(exact.passes() && !above.passes() && !unknown.passes() && !empty.passes());
    assert_eq!(finished.deficit(0), 10);
    assert_eq!(abandoned.deficit(0), 900);
    assert_eq!(recovery.deficit(10), 0);
    assert_eq!(empty.deficit(10), 10);
    assert_eq!(provider_not_activated.activated(), 0);
    assert_eq!(invalid_provider_count, 0);
    assert_eq!(request_invalid.failure, 1);
    assert_eq!(retried.success, 1);
    assert_eq!(accepted_provider_count, 1);
    assert_eq!(unresolved_provider_count, 0);
    json!({"exact_ten_percent": exact.record(0), "above_ten_percent": above.record(0),
        "unresolved_blocks": unknown.record(0), "no_work": empty.record(10),
        "finish_failed_epoch": finished.record(0), "abandon_failed_epoch": abandoned.record(0),
        "recovery": recovery.record(10), "invalid_input_requester": request_invalid.record(0),
        "invalid_input_provider": provider_not_activated.record(0),
        "unresolved_input_requester": unresolved_request.record(0),
        "unresolved_input_provider_activations": unresolved_provider_count,
        "retry": {"transmissions": 3, "requester": retried.record(0),
            "provider_activations": accepted_provider_count}})
}

// Deliberately simplified: two stake groups, no self/registration filters, permits,
// bonds, fixed-point runtime arithmetic, activity changes, or pruning.
fn simplified_yuma(target: [u64; 3], malicious: [u64; 3]) -> [f64; 3] {
    let h_total = target.iter().sum::<u64>() as f64;
    let m_total = malicious.iter().sum::<u64>() as f64;
    let mut ranks = [0.0; 3];
    for i in 0..3 {
        let honest = target[i] as f64 / h_total;
        let bad = malicious[i] as f64 / m_total;
        // Seventy percent identical honest rows fix the weighted median.
        ranks[i] = 0.7 * honest + 0.3 * bad.min(honest);
    }
    let total: f64 = ranks.iter().sum();
    ranks.map(|rank| rank / total)
}

fn native_counterexample() -> Value {
    let cases: Vec<Value> = [1000, 500]
        .into_iter()
        .map(|p| {
            let loss = brier_numerator(&[p, SCALE - p], 0, SCALE);
            let [a, b] = pair_credit(
                RESERVE,
                loss,
                2 * SCALE.pow(2),
                SCALE,
                Admission::from_mask(31),
            );
            let targets = [[a, BASE, RESERVE - a], [b, BASE, RESERVE - b]];
            let incentives = targets.map(|target| simplified_yuma(target, [ROW, 0, 0]));
            let coalition = (incentives[0][0] + incentives[1][0]) / 2.0;
            json!({"candidate_true_label_probability": p as f64/SCALE as f64,
            "pair_credits": [a,b], "combined_pair_credits": a+b,
            "targets_coalition_other_burn": targets,
            "normalized_incentives": incentives, "coalition_total_miner_share": coalition})
        })
        .collect();
    let before = cases[0]["coalition_total_miner_share"].as_f64().unwrap();
    let after = cases[1]["coalition_total_miner_share"].as_f64().unwrap();
    assert!(after > before);
    json!({"model": "simplified normalized rows, stake median clipping, normalized ranks; NOT native execution",
        "honest_stake": 0.7, "malicious_stake": 0.3, "mechanism_shares": [0.5,0.5],
        "malicious_row_both": [60000,0,0], "cases": cases,
        "coalition_share_gain": after-before,
        "official_algorithm": "https://www.bittensor.com/docs/concepts/emissions"})
}

fn sybil_results() -> Value {
    let pots = allocate(RESERVE, 8);
    let cases: Vec<Value> = [1, 2, 4]
        .into_iter()
        .map(|owned_pairs| {
            let coalition = pots.iter().take(owned_pairs).sum::<u64>();
            assert_eq!(pots.iter().sum::<u64>(), RESERVE);
            json!({"owned_endpoint_pairs": owned_pairs, "global_pairs": 8,
            "coalition_paired_credits": coalition, "global_paired_credits": RESERVE,
            "allocated_each_mechanism": ROW})
        })
        .collect();
    json!({"fixed_roster_identity_capture": cases,
        "limit": "Fixed budgets prevent extra total credit; acquiring more scheduled identities can increase an owner's share."})
}

fn main() {
    let output = env::args()
        .nth(1)
        .expect("usage: settlement-gameability OUTPUT.json");
    let results = json!({"schema": "slopninja/settlement-gameability-v1",
        "status": "hypothesis comparison; settlement proposal not adopted",
        "scope": "Synthetic policy credits, ideal certificate votes, and a simplified native-transform counterexample; no models, chain or detector calls.",
        "pair_invariant": invariant_grid(), "allocation": allocation_results(),
        "old_attacks": old_attack_results(), "quorums": quorum_results(),
        "service": service_results(), "native_counterexample": native_counterexample(),
        "sybils": sybil_results()});
    fs::write(
        output,
        format!("{}\n", serde_json::to_string_pretty(&results).unwrap()),
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complementary_grid_and_rounding() {
        invariant_grid();
    }
    #[test]
    fn fixed_budgets_and_joint_admission() {
        allocation_results();
    }
    #[test]
    fn normalization_and_source_poisoning_counterexamples() {
        old_attack_results();
    }
    #[test]
    fn weighted_quorum_safety_and_liveness_boundaries() {
        quorum_results();
    }
    #[test]
    fn unresolved_accounting_and_recovery() {
        service_results();
    }
    #[test]
    fn clipping_does_not_preserve_coalition_cash() {
        native_counterexample();
    }
    #[test]
    fn fixed_budget_does_not_prove_sybil_resistance() {
        sybil_results();
    }
}
