use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{error::Error, fs};

const UIDS: usize = 100;
const SLOTS: usize = 10_000;
const PEERS: usize = 3;
const BATCH: usize = 10;
const SEEDS: [u64; 3] = [0, 17, 29];
const CARTELS: [usize; 5] = [0, 10, 20, 30, 50];

fn digest(domain: &str, seed: u64, requester: usize, index: usize, peer: usize) -> [u8; 32] {
    let mut hash = Sha256::new();
    // Domain, synthetic chain ID, netuid, requester, lifetime index, peer.
    // Each field is prefixed by its u32 big-endian byte length. Roster absent.
    for field in [
        domain.as_bytes().to_vec(),
        seed.to_be_bytes().to_vec(),
        1u64.to_be_bytes().to_vec(),
        (requester as u64).to_be_bytes().to_vec(),
        (index as u64).to_be_bytes().to_vec(),
        (peer as u64).to_be_bytes().to_vec(),
    ] {
        hash.update((field.len() as u32).to_be_bytes());
        hash.update(field);
    }
    hash.finalize().into()
}

fn rank(domain: &str, seed: u64, requester: usize, index: usize, excluded: &[usize]) -> Vec<usize> {
    let mut rows = (0..UIDS)
        .filter(|u| !excluded.contains(u))
        .map(|u| (digest(domain, seed, requester, index, u), u))
        .collect::<Vec<_>>();
    rows.sort_unstable();
    rows.into_iter().map(|(_, u)| u).collect()
}

fn gate(n: usize, failures: usize) -> bool {
    n > 0 && failures <= n && 10 * failures <= n
}
fn recovery(previous: usize, n: usize, failures: usize) -> usize {
    assert!(failures <= n);
    (previous + 10 * failures).saturating_sub(n)
}
fn fraction(a: usize, b: usize) -> Option<f64> {
    (b > 0).then(|| a as f64 / b as f64)
}
fn choose(n: usize, k: usize) -> f64 {
    if k > n {
        return 0.0;
    }
    (0..k.min(n - k)).fold(1.0, |v, i| v * (n - i) as f64 / (i + 1) as f64)
}
fn any_cartel(n: usize, cartel: usize, draws: usize) -> f64 {
    1.0 - choose(n - cartel, draws) / choose(n, draws)
}

#[derive(Clone)]
struct Draw {
    requester: usize,
    interface: usize,
    peers: Vec<usize>,
    panel: Vec<usize>,
    batch: Vec<usize>,
    reviewers: Vec<usize>,
}
fn draws(seed: u64) -> Vec<Draw> {
    (0..SLOTS)
        .map(|slot| {
            let requester = slot % UIDS;
            let index = slot / UIDS;
            let peers = rank(
                "slopninja-assignment-gameability/peers-v1",
                seed,
                requester,
                index,
                &[requester],
            )
            .into_iter()
            .take(PEERS)
            .collect();
            let mut panel = vec![0];
            panel.extend(
                rank(
                    "slopninja-assignment-gameability/panel-v1",
                    seed,
                    0,
                    slot,
                    &[0],
                )
                .into_iter()
                .take(2),
            );
            let batch: Vec<usize> = rank(
                "slopninja-assignment-gameability/batch-v1",
                seed,
                0,
                slot,
                &panel,
            )
            .into_iter()
            .take(BATCH)
            .collect();
            // Already-filtered validator roster, disjoint from all A/B UIDs.
            // Model one scored submitter (the first B entry) per comparison.
            let mut reviewer_ranks = (1000..1000 + UIDS)
                .map(|u| {
                    (
                        digest(
                            "slopninja-assignment-gameability/reviewers-v1",
                            seed,
                            batch[0],
                            slot,
                            u,
                        ),
                        u,
                    )
                })
                .collect::<Vec<_>>();
            reviewer_ranks.sort_unstable();
            let reviewers = reviewer_ranks.into_iter().take(3).map(|(_, u)| u).collect();
            Draw {
                requester,
                interface: index % 2,
                peers,
                panel,
                batch,
                reviewers,
            }
        })
        .collect()
}

fn service(seed: u64, cartel: usize, draws: &[Draw]) -> Value {
    let mut initial = vec![vec![0usize; UIDS]; 2];
    let mut issued = vec![0usize; UIDS];
    let mut issued_by_interface = vec![vec![0usize; UIDS]; 2];
    for d in draws {
        initial[d.interface][d.peers[0]] += 1;
        issued[d.requester] += 1;
        issued_by_interface[d.interface][d.requester] += 1;
    }
    let budget = initial
        .iter()
        .map(|x| x.iter().map(|n| n / 10).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let mut attempts = vec![vec![0usize; UIDS]; 2];
    let mut failures = attempts.clone();
    let mut total_completed = 0;
    let mut fallback_successes = 0;
    let mut dishonest_successes = 0;
    let mut erased_failures = vec![vec![0usize; UIDS]; 2];
    let mut cartel_slots = 0;
    let mut mandatory_favorable = 0;
    let mut menu_favorable = 0;
    let mut capped_skips = vec![vec![0usize; UIDS]; 2];
    let mut aggregate_only_skips = vec![vec![0usize; UIDS]; 2];
    let mut all_skips = vec![0usize; UIDS];
    let mut capped_delivered = 0;
    let mut capped_favorable = 0;
    for d in draws {
        if d.requester < cartel {
            cartel_slots += 1;
            let favorable = d.peers[0] < cartel;
            mandatory_favorable += usize::from(favorable);
            menu_favorable += usize::from(d.peers.iter().any(|p| *p < cartel));
            if !favorable {
                all_skips[d.requester] += 1;
            }
            if !favorable
                && aggregate_only_skips
                    .iter()
                    .map(|row| row[d.requester])
                    .sum::<usize>()
                    < issued[d.requester] / 10
            {
                aggregate_only_skips[d.interface][d.requester] += 1;
            }
            if !favorable
                && capped_skips[d.interface][d.requester]
                    < issued_by_interface[d.interface][d.requester] / 10
            {
                capped_skips[d.interface][d.requester] += 1;
            } else {
                capped_delivered += 1;
                capped_favorable += usize::from(favorable);
            }
        }
        let mut failed = Vec::new();
        let mut served = false;
        for (position, &provider) in d.peers.iter().enumerate() {
            attempts[d.interface][provider] += 1;
            if provider < cartel
                && d.requester >= cartel
                && failures[d.interface][provider] < budget[d.interface][provider]
            {
                failures[d.interface][provider] += 1;
                failed.push(provider);
            } else {
                served = true;
                total_completed += 1;
                fallback_successes += usize::from(position > 0);
                dishonest_successes += usize::from(provider < cartel);
                break;
            }
        }
        if !served {
            for p in failed {
                erased_failures[d.interface][p] += 1;
            }
        }
    }
    let total_failures: usize = failures.iter().flatten().sum();
    assert!(attempts.iter().enumerate().all(|(i, row)| {
        row.iter()
            .enumerate()
            .all(|(u, n)| gate(*n, failures[i][u]))
    }));
    assert!((0..cartel).all(
        |u| gate(issued[u], capped_skips.iter().map(|row| row[u]).sum())
            && (0..2).all(|i| gate(issued_by_interface[i][u], capped_skips[i][u]))
    ));
    json!({"seed":seed,"cartel_uids":cartel,"cartel_requester_slots":cartel_slots,
        "mandatory_first":{"favorable":mandatory_favorable,"fraction":fraction(mandatory_favorable,cartel_slots),"theory":(cartel>0).then(||(cartel-1) as f64/(UIDS-1) as f64)},
        "choose_any_three":{"favorable":menu_favorable,"fraction":fraction(menu_favorable,cartel_slots),"theory":(cartel>0).then(||any_cartel(UIDS-1,cartel-1,PEERS))},
        "provider_selective_failures":{"strategy":"Cartel providers decline noncartel requesters in canonical slot order, with budget floor(initial assignments/10) separately per interface. Extra fallback attempts increase the denominator but not the attack budget.",
            "count":total_failures,"attempts":attempts.iter().flatten().sum::<usize>(),"completed_tickets":total_completed,"exhausted_tickets":draws.len()-total_completed,
            "tickets_recovered_by_fallback":fallback_successes,"failures_if_incorrectly_erased_after_success":erased_failures.iter().flatten().sum::<usize>(),
            "max_observed_provider_interface_failure_rate":attempts.iter().enumerate().flat_map(|(i,row)|row.iter().enumerate().map(move |(u,n)|(i,u,*n))).map(|(i,u,n)|failures[i][u] as f64/n as f64).fold(0.0,f64::max),
            "all_providers_pass_per_interface_and_aggregate":true,"cartel_successful_answers_that_may_still_lie":dishonest_successes},
        "requester_abandonment_capped":{"skipped_issued_slots":capped_skips.iter().flatten().sum::<usize>(),"delivered":capped_delivered,"favorable":capped_favorable,"favorable_fraction_of_delivered":fraction(capped_favorable,capped_delivered),"all_cartel_requesters_pass":true,"all_interfaces_and_aggregate_pass":true,"provider_failures_charged_for_undelivered_payload":0},
        "requester_abandonment_aggregate_only":{"skipped_issued_slots":aggregate_only_skips.iter().flatten().sum::<usize>(),
            "uids_passing_aggregate":(0..cartel).filter(|u|gate(issued[*u],aggregate_only_skips.iter().map(|row|row[*u]).sum())).count(),
            "uids_failing_at_least_one_interface":(0..cartel).filter(|u|(0..2).any(|i|!gate(issued_by_interface[i][*u],aggregate_only_skips[i][*u]))).count()},
        "requester_abandonment_all_unfavorable":{"skipped_issued_slots":all_skips.iter().sum::<usize>(),"delivered":mandatory_favorable,"favorable_fraction_of_delivered":fraction(mandatory_favorable,mandatory_favorable),
            "cartel_requesters_passing_correct_gate":(0..cartel).filter(|u|gate(issued[*u],all_skips[*u])).count(),
            "incorrect_delivered_only_gate_would_pass":(0..cartel).filter(|u|issued[*u]>all_skips[*u]).count(),
            "new_free_draws":0,"provider_failures_charged_for_undelivered_payload":0}})
}

fn panels(seed: u64, cartel: usize, strongest_cartel: bool, draws: &[Draw]) -> Value {
    let is_cartel = |u: usize| {
        if strongest_cartel {
            u < cartel
        } else {
            u > 0 && u <= cartel
        }
    };
    let other_cartel = cartel - usize::from(strongest_cartel);
    let mut majority = 0;
    let mut joint = 0;
    let mut reviewers = 0;
    let mut cartel_batch_entries = 0;
    let mut cartel_submissions = 0;
    let mut targeted_bad_submissions = 0;
    for d in draws {
        assert!(d.batch.iter().all(|u| !d.panel.contains(u)));
        let captured = d.panel.iter().filter(|u| is_cartel(**u)).count() >= 2;
        majority += usize::from(captured);
        let b = d.batch.iter().filter(|u| is_cartel(**u)).count();
        joint += usize::from(captured && b > 0);
        cartel_batch_entries += b;
        assert!(
            d.reviewers
                .iter()
                .all(|u| *u >= 1000 && !d.panel.contains(u) && !d.batch.contains(u))
        );
        let reviewer_capture = d.reviewers.iter().filter(|u| **u - 1000 < cartel).count() >= 2;
        reviewers += usize::from(reviewer_capture);
        cartel_submissions += usize::from(is_cartel(d.batch[0]));
        targeted_bad_submissions += usize::from(is_cartel(d.batch[0]) && reviewer_capture);
    }
    let mut theory = 0.0;
    let mut joint_theory = 0.0;
    for additional in 0..=2 {
        let count = additional + usize::from(strongest_cartel);
        let p = choose(other_cartel, additional) * choose(UIDS - 1 - other_cartel, 2 - additional)
            / choose(UIDS - 1, 2);
        if count >= 2 && count <= cartel {
            theory += p;
            joint_theory += p * any_cartel(UIDS - 3, cartel - count, BATCH);
        }
    }
    let reviewer_theory =
        (choose(cartel, 2) * choose(UIDS - cartel, 1) + choose(cartel, 3)) / choose(UIDS, 3);
    json!({"seed":seed,"cartel_uids":cartel,"strongest":"UID 0 fixed, chosen before any draw","strongest_cartel":strongest_cartel,
        "trials":draws.len(),"median_panel_cartel_majority":majority,"median_panel_majority_fraction":fraction(majority,draws.len()),"median_panel_majority_theory":theory,
        "majority_with_other_cartel_uid_in_b_batch":joint,"joint_fraction":fraction(joint,draws.len()),"joint_theory":joint_theory,
        "cartel_b_batch_entries":cartel_batch_entries,"exact_panel_uid_exclusion_always_holds":true,
        "reviewer_cartel_majority":reviewers,"reviewer_majority_fraction":fraction(reviewers,draws.len()),"reviewer_majority_theory":reviewer_theory,
        "modeled_cartel_submitters":cartel_submissions,"targeted_bad_submissions_on_known_captured_reviewers":targeted_bad_submissions,
        "all_modeled_submissions_receive_three_reviews":true,"reviewer_uids_disjoint_from_all_involved_a_b_uids":true,
        "cartel_fulfillment_fraction":1.0,"availability_gate_detects_fabricated_quality":false,
        "attack":"Every assigned service answers on time. Dishonest panel/reviewer answers only on favorable known majority assignments can control a median/majority while passing availability. Honest votes=0 and cartel votes=1 in this stylized capture indicator."})
}

fn accounting() -> Value {
    let low=(0..=20).map(|n|json!({"requests":n,"maximum_failures_passing":(n>0).then_some(n/10),"zero_failures_pass":gate(n,0),"one_failure_pass":gate(n,1),"two_failures_pass":gate(n,2)})).collect::<Vec<_>>();
    let dilution=[(10,2,100),(10,10,0),(100,20,1000)].into_iter().map(|(mandatory,fails,other_interface_successes)| {
        let paid_needed=10*fails-mandatory;
        json!({"mandatory_interface_a_requests":mandatory,"mandatory_interface_a_failures":fails,"mandatory_interface_b_successes":other_interface_successes,
            "minimum_paid_successes_to_pass_blended_a":paid_needed,"blended_a_gate":gate(mandatory+paid_needed,fails),"separate_mandatory_a_gate":gate(mandatory,fails),
            "aggregate_mandatory_gate":gate(mandatory+other_interface_successes,fails),
            "all_committed_interfaces_and_aggregate_gate":gate(mandatory,fails)&&gate(other_interface_successes,0)&&gate(mandatory+other_interface_successes,fails)})
    }).collect::<Vec<_>>();
    let cliff=[0usize,88,89].into_iter().map(|completed|json!({"fixed_total_obligations":100,"already_failed":11,"remaining_obligations":89,
        "remaining_completed":completed,"final_failures":100-completed,"gate":gate(100,100-completed),
        "proposed_recovery_deficit":recovery(0,100,100-completed),
        "future_same_class_successes_needed_without_new_failures":recovery(0,100,100-completed),
        "current_epoch_emission_credit":0,"unit_service_cost":1,"payoff_from_current_emissions_minus_remaining_cost":-(completed as i64)})).collect::<Vec<_>>();
    json!({"low_task_counts":low,"dilution_examples":dilution,"irrecoverable_cliff":cliff,
        "recovery_proposal":{"recurrence":"D_e=max(0,D_previous+10F_e-N_e), separately per mandatory interface/class, initial zero; current availability and qualification gates remain separate.",
            "complete_remaining_89_deficit":recovery(0,100,11),"abandon_remaining_89_deficit":recovery(0,100,100),
            "after_ten_subsequent_same_class_successes":recovery(10,10,0),"after_nine_successes":recovery(10,9,0),
            "no_assigned_work_preserves_deficit":recovery(10,0,0),"healthy_epoch_does_not_bank_credit":recovery(0,100,0),
            "after_healthy_epoch_then_11_of_100_failures":recovery(recovery(0,100,0),100,11),
            "paid_or_other_class_successes_reduce_deficit":false,"current_failed_epoch_credit_restored":false,
            "limit":"Continuing now reduces future recovery work if future eligibility has value and probation capacity is available. This is not an equilibrium guarantee; fresh identity/slot acquisition remains outside the model."},
        "cliff_scope":"The final mandatory denominator is already fixed at 100, and 11 failures are irreversible. No future eligibility, probation/recovery benefit, customer fees, or penalties enter this toy payoff. Current emissions alone then favor abandoning remaining costly work.",
        "exact_threshold":"N>0 and 10F<=N. Exactly 10% failure passes; N=0 supplies no availability evidence.",
        "paid_successes_count_in_separate_mandatory_gate":false,
        "minimum_paid_successes_formula":"max(0,10*mandatory_failures-mandatory_requests)",
        "unavailable_committed_interface_policy":"No automatic pass from aggregate service on another interface."})
}

fn size_sensitivity() -> Vec<Value> {
    let mut rows = Vec::new();
    for cartel in CARTELS {
        for size in [3usize, 5, 7, 9] {
            let threshold = size / 2 + 1;
            let reviewer = (threshold..=size)
                .map(|c| choose(cartel, c) * choose(UIDS - cartel, size - c) / choose(UIDS, size))
                .sum::<f64>();
            for strongest_cartel in [false, true] {
                if strongest_cartel && cartel == 0 {
                    continue;
                }
                let extra = cartel - usize::from(strongest_cartel);
                let panel = (0..size)
                    .filter(|c| *c + usize::from(strongest_cartel) >= threshold)
                    .map(|c| {
                        choose(extra, c) * choose(UIDS - 1 - extra, size - 1 - c)
                            / choose(UIDS - 1, size - 1)
                    })
                    .sum::<f64>();
                rows.push(json!({"cartel_uids":cartel,"size":size,"strongest_cartel":strongest_cartel,
                    "panel_majority_probability":panel,"reviewer_majority_probability":reviewer,
                    "calls_relative_to_three":size as f64/3.0,"source":"Exact without-replacement calculation; sizes above three not hash-simulated."}));
            }
        }
    }
    rows
}

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("Usage: assignment-gameability OUT.json")?;
    let mut service_rows = Vec::new();
    let mut panel_rows = Vec::new();
    for seed in SEEDS {
        let generated = draws(seed);
        for cartel in CARTELS {
            service_rows.push(service(seed, cartel, &generated));
            panel_rows.push(panels(seed, cartel, false, &generated));
            if cartel > 0 {
                panel_rows.push(panels(seed, cartel, true, &generated));
            }
        }
    }
    let result = json!({"schema":"slopninja-assignment-gameability-v1","config":{"uids":UIDS,"slots_per_seed":SLOTS,"seeds":SEEDS,"cartel_uid_counts":CARTELS,"total_permitted_peers":PEERS,"ordered_fallbacks":PEERS-1,"b_batch_size":BATCH,
        "synthetic_roster":"100 UIDs qualified for both logical interfaces; capacity reserved for every possible stage, with no time/cost model. This does not validate deployed joint mechanism/UID capacity.",
        "hash":"Real SHA-256 over canonical u32-length-prefixed fields; seed is a synthetic chain ID, netuid=1. Domain separation for peers/panels/batches/reviewers; hash ties broken by UID. Roster membership is excluded from each hash.",
        "membership":"Fixed before all draws. One coordinated coalition may own several UIDs. Strongest-UID control is an exogenous scenario, not a qualification/identity proof.",
        "ordering":"Requester=slot modulo 100; lifetime index=floor(slot/100); requested interface=index modulo 2. All issued slots consumed even when abandoned. Same seed draws reused across cartel shares/strategies for paired comparison.",
        "reviewers":"Already-filtered separate validator roster with UIDs 1000..1099, disjoint from every A/B UID. Same conditional cartel share; its first M identities collude. Hash binds the actual first B submitter UID. Model one scored submission per comparison, each receiving all three reviews; this does not simulate every entry of the whole B batch or real-world eligibility filtering.",
        "comparison_reserves":"Only initial common detector panels and B batches are modeled. No shared-matrix fallback or reserve simulation. Actual reserve order must exclude the initial panel and every frozen B batch UID before issuance.",
        "theory":"Uniform independent digest ranks imply sampling without replacement. Combinatorial formulas validate empirical draws under that model; they are not protocol security proofs."},
        "service":service_rows,"panels":panel_rows,"size_sensitivity":size_sensitivity(),"accounting":accounting()});
    fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&result)?),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    #[test]
    fn sha256_and_ranking_are_real_stable_and_exclude_only_named_uids() {
        let encoded = Sha256::digest(b"abc")
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect::<String>();
        assert_eq!(
            encoded,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let a = rank("test", 17, 9, 4, &[9]);
        let b = rank("test", 17, 9, 4, &[9, 7]);
        assert_eq!(a.iter().copied().filter(|u| *u != 7).collect::<Vec<_>>(), b);
        assert_eq!(a.iter().copied().collect::<BTreeSet<_>>().len(), UIDS - 1);
        assert_ne!(digest("test", 17, 9, 4, 1), digest("other", 17, 9, 4, 1));
    }
    #[test]
    fn exact_gate_and_dilution_boundaries() {
        assert!(!gate(0, 0));
        assert!(!gate(9, 1));
        assert!(gate(10, 1));
        assert!(!gate(19, 2));
        assert!(gate(20, 2));
        assert!(!gate(10, 2));
        assert!(gate(10 + 10, 2));
        assert!(gate(10 + 100, 2));
        assert_eq!(recovery(0, 100, 11), 10);
        assert_eq!(recovery(0, 100, 100), 900);
        assert_eq!(recovery(10, 10, 0), 0);
        assert_eq!(recovery(10, 0, 0), 10);
        assert_eq!(recovery(recovery(0, 100, 0), 100, 11), 10);
    }
    #[test]
    fn combinatorics_and_small_service_are_consistent() {
        assert_eq!(choose(5, 3), 10.0);
        assert_eq!(choose(2, 3), 0.0);
        assert!((any_cartel(5, 2, 3) - 0.9).abs() < 1e-12);
        let generated = (0..2000)
            .map(|slot| {
                let requester = slot % UIDS;
                Draw {
                    requester,
                    interface: (slot / UIDS) % 2,
                    peers: (1..=PEERS)
                        .map(|offset| (requester + offset) % UIDS)
                        .collect(),
                    panel: vec![],
                    batch: vec![],
                    reviewers: vec![],
                }
            })
            .collect::<Vec<_>>();
        let a = service(91, 20, &generated);
        let b = service(91, 20, &generated);
        assert_eq!(a, b);
        assert_eq!(
            a["provider_selective_failures"]["all_providers_pass_per_interface_and_aggregate"],
            true
        );
    }
}
