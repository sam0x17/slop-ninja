//! Synthetic failure economics; authorized hypothesis, not an adopted cash contract.
use serde_json::{Value, json};

const POT: i64 = 100;
const COMPENSATION: i64 = 25;
const COST_BOUND: i64 = 200;
const COSTS: [i64; 4] = [0, 25, 100, COST_BOUND];
const SPONSOR: u8 = 1;
const A: u8 = 2;
const B: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    A,
    B,
    Both,
    QualityVoid,
}

impl Fault {
    fn actors(self) -> u8 {
        match self {
            Self::A => A,
            Self::B => B,
            Self::Both => A | B,
            Self::None | Self::QualityVoid => 0,
        }
    }
}

#[derive(Debug)]
struct Settlement {
    awards: [i64; 2],
    sponsor_refund: i64,
    principal_burn: i64,
    bond_burn: [i64; 2],
}

// Compensation is a frozen flat award for independently certified valid partner
// work, not reimbursement of unverifiable claimed compute. One defaulting endpoint
// earns nothing. A quality/platform void without an attributable fault pays neither.
fn settle(fault: Fault, refundable: bool, bond: i64, partner_certified: bool) -> Settlement {
    let awards = match fault {
        Fault::None => [40, 60],
        Fault::A if partner_certified => [0, COMPENSATION],
        Fault::B if partner_certified => [COMPENSATION, 0],
        _ => [0, 0],
    };
    let residue = POT - awards.iter().sum::<i64>();
    let sponsor_refund = if refundable { residue } else { 0 };
    Settlement {
        awards,
        sponsor_refund,
        principal_burn: residue - sponsor_refund,
        bond_burn: [
            if fault.actors() & A != 0 { bond } else { 0 },
            if fault.actors() & B != 0 { bond } else { 0 },
        ],
    }
}

fn utility(owner: u8, costs: [i64; 2], fault: Fault, result: &Settlement) -> i64 {
    let mut value = if owner & SPONSOR != 0 {
        result.sponsor_refund - POT
    } else {
        0
    };
    for (i, role) in [A, B].into_iter().enumerate() {
        if owner & role != 0 {
            let incurred_cost = if fault.actors() & role != 0 {
                0
            } else {
                costs[i]
            };
            value += result.awards[i] - result.bond_burn[i] - incurred_cost;
        }
    }
    value
}

fn comparisons() -> Vec<Value> {
    let mut results = Vec::new();
    for refundable in [true, false] {
        for bond in [0, COST_BOUND] {
            for owner in 0..8_u8 {
                let mut evaluated = 0;
                let mut profitable = 0;
                let mut maximum: Option<i64> = None;
                let mut witness = Value::Null;
                for ca in COSTS {
                    for cb in COSTS {
                        let costs = [ca, cb];
                        let success = settle(Fault::None, refundable, bond, true);
                        let baseline = utility(owner, costs, Fault::None, &success);
                        for fault in [Fault::A, Fault::B, Fault::Both] {
                            // An ownership coalition must control every deliberate defaulter.
                            if owner & fault.actors() != fault.actors() {
                                continue;
                            }
                            let failed = settle(fault, refundable, bond, true);
                            let gain = utility(owner, costs, fault, &failed) - baseline;
                            evaluated += 1;
                            profitable += usize::from(gain > 0);
                            if maximum.is_none_or(|previous| gain > previous) {
                                maximum = Some(gain);
                                witness = json!({"cost_a": ca, "cost_b": cb,
                                    "default": format!("{fault:?}"), "gain": gain,
                                    "awards": failed.awards, "sponsor_refund": failed.sponsor_refund,
                                    "principal_burn": failed.principal_burn,
                                    "bond_burn": failed.bond_burn});
                            }
                        }
                    }
                }
                results.push(
                    json!({"refundable_failure": refundable, "bond_per_endpoint": bond,
                    "owner_mask": owner, "controlled_strategy_cases": evaluated,
                    "profitable_cases": profitable, "maximum_gain": maximum, "witness": witness}),
                );
            }
        }
    }
    results
}

pub fn run() -> Value {
    let both_costs = [200, 25];
    let success = settle(Fault::None, false, 0, true);
    let failed = settle(Fault::A, false, 0, true);
    let saving = utility(A | B, both_costs, Fault::A, &failed)
        - utility(A | B, both_costs, Fault::None, &success);
    let sponsor_attack = |refundable| {
        utility(
            SPONSOR | A,
            [0, 25],
            Fault::A,
            &settle(Fault::A, refundable, 0, true),
        ) - utility(
            SPONSOR | A,
            [0, 25],
            Fault::None,
            &settle(Fault::None, refundable, 0, true),
        )
    };
    let fake_saving = utility(B, [0, 0], Fault::None, &success)
        - utility(B, [0, COST_BOUND], Fault::None, &success);
    let voided = settle(Fault::QualityVoid, false, COST_BOUND, true);
    json!({"status": "authorized hypothesis comparison; not adopted settlement or native-cash simulation",
        "assumptions": {"pot": POT, "success_awards_a_b": [40,60],
            "cost_grid_each_role": COSTS, "declared_cost_bound_each_role": COST_BOUND,
            "partner_compensation": COMPENSATION,
            "compensation_condition": "One attributable defaulter; the other endpoint has independently certified valid work. Fixed flat cap, no claimed-cost reimbursement.",
            "default_funding": "All unearned principal burns; no same-epoch redistribution or selected rollover.",
            "bond_condition": "Only independently certified attributable default burns the responsible endpoint's irreversibly reserved bond outside this modeled coalition.",
            "ownership_bits": {"sponsor": SPONSOR, "a": A, "b": B},
            "excluded": "Gas, liquidity/time value, native emissions, external transfers, registration costs, future eligibility changes and arbitrary third-party ownership are not modeled."},
        "ownership_and_cost_comparisons": comparisons(),
        "bad_b_quality_or_unattributed_void": {"awards": voided.awards,
            "principal_burn": voided.principal_burn, "bond_burn": voided.bond_burn},
        "same_owner_cost_saving": {"owner_mask": A|B, "costs_a_b": both_costs,
            "default": "A", "refundable": false, "bond": 0, "gain": saving},
        "sponsor_a_refund_attack": {"owner_mask": SPONSOR|A, "costs_a_b": [0,25],
            "refundable_gain": sponsor_attack(true), "irreversible_funding_gain": sponsor_attack(false)},
        "collateral": {"concurrent_tickets": 3, "roles_reserved_each_ticket": 2,
            "bond_each_role_ticket": COST_BOUND, "required_reserved_total": 3*2*COST_BOUND,
            "reusing_one_two_role_reservation": 2*COST_BOUND,
            "reuse_shortfall": 2*2*COST_BOUND},
        "unproven_fake_execution": {"payout_verdict_unchanged": true,
            "certified_attributable_fault": false, "b_cost_before": COST_BOUND,
            "b_cost_after": 0, "b_gain": fake_saving, "bond_burn": 0,
            "limit": "An unproven on-time execution claim cannot trigger the modeled default bond; this assumes the claimed shortcut leaves all certified evidence unchanged."},
        "limits": "A finite bounded-cost fixture can show deterrence of its attributable defaults. It does not prove truthful private execution, sound certificates, Sybil resistance, all-strategy profit resistance or actual native payout conservation."})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_admission_and_void_never_award_a_full_pot() {
        for fault in [Fault::A, Fault::B, Fault::Both, Fault::QualityVoid] {
            for certified in [false, true] {
                let result = settle(fault, false, COST_BOUND, certified);
                assert!(result.awards[0] <= COMPENSATION);
                assert!(result.awards[1] <= COMPENSATION);
                assert_eq!(
                    result.awards.iter().sum::<i64>() + result.principal_burn,
                    POT
                );
                assert_eq!(result.sponsor_refund, 0);
                if fault == Fault::QualityVoid {
                    assert_eq!(result.awards, [0, 0]);
                }
            }
        }
    }

    #[test]
    fn irreversible_bonds_cover_this_bounded_controlled_default_grid() {
        let rows = comparisons();
        for row in rows
            .iter()
            .filter(|r| r["refundable_failure"] == false && r["bond_per_endpoint"] == COST_BOUND)
        {
            assert_eq!(row["profitable_cases"], 0);
        }
        assert!(rows.iter().any(|r| r["refundable_failure"] == false
            && r["bond_per_endpoint"] == 0
            && r["profitable_cases"].as_u64().unwrap() > 0));
    }

    #[test]
    fn refund_removal_does_not_prove_private_execution() {
        let results = run();
        assert_eq!(results["sponsor_a_refund_attack"]["refundable_gain"], 35);
        assert_eq!(
            results["sponsor_a_refund_attack"]["irreversible_funding_gain"],
            -40
        );
        assert_eq!(results["same_owner_cost_saving"]["gain"], 125);
        assert_eq!(results["unproven_fake_execution"]["b_gain"], COST_BOUND);
        assert_eq!(results["collateral"]["reuse_shortfall"], 800);
    }
}
