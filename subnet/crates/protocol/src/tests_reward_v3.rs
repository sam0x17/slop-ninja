//! `reward-v3`: the hand-worked examples, the walk in roster order, the
//! equality with `reward-v2` on rosters without near copies, promotion of
//! retained heads only, the rule's records, the restart record that names a
//! fresh record's rules, and every rejection of a malformed assignment.

use crate::fixtures_reward_v1::{self as fx, Spec, bp, chain, hotkey, sigma_permille};
use crate::fixtures_reward_v3::{self as f3, Entry, HUNDREDTH, example_outcome, gated, groups};
use crate::reward_v1::{self, Placement, QEntrant, QInput, q_outcome};
use crate::*;

const GOLDEN: &str = include_str!("../fixtures/golden-reward-v3.txt");

#[test]
fn golden_reward_v3_report_is_unchanged() -> Result<()> {
    let report = f3::golden_report()?;
    for (i, (a, b)) in report.lines().zip(GOLDEN.lines()).enumerate() {
        assert_eq!(a, b, "golden line {}", i + 1);
    }
    assert_eq!(report, GOLDEN);
    Ok(())
}

/// Prints the golden report, to regenerate `fixtures/golden-reward-v3.txt`
/// after a deliberate change: `cargo test -p slop-ninja-protocol --lib
/// print_golden_reward_v3_report -- --ignored --nocapture`.
#[test]
#[ignore]
fn print_golden_reward_v3_report() -> Result<()> {
    print!("{}", f3::golden_report()?);
    Ok(())
}

#[test]
fn late_lucky_copies_are_placed_by_the_first_copy() -> Result<()> {
    let e1 = [gated(90), gated(92), gated(95), gated(93), gated(91)];
    let near: &[&[usize]] = &[&[0, 2, 3]];
    let v2 = example_outcome(&e1, 90, None, groups(near), Placement::QRankHead)?;
    assert_eq!(v2.units, vec![0, 21654, 12738, 12737, 18406]);
    let v3 = example_outcome(&e1, 90, None, groups(near), Placement::RosterFirst)?;
    assert_eq!(v3.mode, CutoffMode::Margin);
    assert_eq!(v3.units, vec![0, 35424, 0, 0, 30111]);
    // With the first copy at 0.93, the party's cluster is retained there.
    let e1b = [gated(93), gated(92), gated(95), gated(93), gated(91)];
    let v3 = example_outcome(&e1b, 90, None, groups(near), Placement::RosterFirst)?;
    let party: u32 = [0, 2, 3].iter().map(|&i| u32::from(v3.units[i])).sum();
    assert!(party > 0 && v3.clusters[0].head == 0 && v3.clusters[0].members == vec![0, 2, 3]);
    Ok(())
}

#[test]
fn a_near_copy_cannot_flip_the_mode() -> Result<()> {
    let e2 = [gated(90), gated(92), gated(89)];
    let near: &[&[usize]] = &[&[0, 1]];
    let v2 = example_outcome(&e2, 90, Some(88), groups(near), Placement::QRankHead)?;
    assert_eq!((v2.mode, v2.units.clone()), (CutoffMode::Margin, vec![0, 65535, 0]));
    let v3 = example_outcome(&e2, 90, Some(88), groups(near), Placement::RosterFirst)?;
    assert_eq!(v3.mode, CutoffMode::Fallback);
    assert_eq!(v3.cutoff, Some(89 * HUNDREDTH));
    assert_eq!(v3.units, vec![17712, 17712, 30111]);
    // Without a predecessor: reward-v2 still margin, reward-v3 no target.
    let v3 = example_outcome(&e2, 90, None, groups(near), Placement::RosterFirst)?;
    assert_eq!(v3.mode, CutoffMode::NoTarget);
    assert_eq!(example_outcome(&e2, 90, None, groups(near), Placement::QRankHead)?.mode, CutoffMode::Margin);
    Ok(())
}

#[test]
fn the_fallback_maximum_comes_from_heads() -> Result<()> {
    let e3 = [gated(82), gated(90), gated(85)];
    let near: &[&[usize]] = &[&[0, 1]];
    let v2 = example_outcome(&e3, 90, Some(80), groups(near), Placement::QRankHead)?;
    assert_eq!((v2.cutoff, v2.units.clone()), (Some(82 * HUNDREDTH), vec![17712, 17712, 30111]));
    let v3 = example_outcome(&e3, 90, Some(80), groups(near), Placement::RosterFirst)?;
    assert_eq!(v3.cutoff, Some(81 * HUNDREDTH));
    assert_eq!(v3.units, vec![15056, 15055, 35424]);
    Ok(())
}

#[test]
fn clusters_do_not_chain_in_roster_order() -> Result<()> {
    let chain = |a: usize, b: usize| a.abs_diff(b) == 1;
    let abc = example_outcome(&[gated(95), gated(94), gated(93)], 90, None, chain, Placement::RosterFirst)?;
    let members: Vec<Vec<usize>> = abc.clusters.iter().map(|c| c.members.clone()).collect();
    assert_eq!(members, vec![vec![0, 1], vec![2]]);
    // Roster [C, B, A]: {C, B} and {A}, listed in rank order.
    let cba = example_outcome(&[gated(93), gated(94), gated(95)], 90, None, chain, Placement::RosterFirst)?;
    let mut members: Vec<Vec<usize>> = cba.clusters.iter().map(|c| c.members.clone()).collect();
    members.sort();
    assert_eq!(members, vec![vec![0, 1], vec![2]]);
    Ok(())
}

#[test]
fn best_of_n_never_moves_the_placement() -> Result<()> {
    // N near copies after the first: whatever their draws, the cluster sits
    // at the first copy's q.
    for draws in [[91u64, 92, 93], [99, 99, 99], [85, 86, 87], [95, 80, 99]] {
        let entries = [gated(92), gated(draws[0]), gated(draws[1]), gated(draws[2]), gated(91)];
        let o = example_outcome(&entries, 90, None, groups(&[&[0, 1, 2, 3]]), Placement::RosterFirst)?;
        let party = o.clusters.iter().find(|c| c.head == 0).expect("party cluster");
        assert_eq!(party.members, vec![0, 1, 2, 3]);
        assert_eq!(entries[party.head].q, Some(92 * HUNDREDTH));
        let total: u32 = [0usize, 1, 2, 3].iter().map(|&i| u32::from(o.units[i])).sum();
        assert_eq!(total, 65535 - u32::from(o.units[4]));
        assert_eq!(o.units[4], 30111);
    }
    Ok(())
}

/// A deterministic generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn without_near_copies_reward_v3_equals_reward_v2() -> Result<()> {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let params = f3::example_params();
    for case in 0..400 {
        let n = 1 + (rng.next() % 24) as usize;
        let entries: Vec<Entry> = (0..n)
            .map(|_| {
                let q = 80 + rng.next() % 21;
                let gated = !rng.next().is_multiple_of(5);
                Entry { q: if rng.next().is_multiple_of(9) { None } else { Some(q * HUNDREDTH) }, gated: gated && q > 0 }
            })
            .map(|e| Entry { gated: e.gated && e.q.is_some(), ..e })
            .collect();
        let reference = 85 + rng.next() % 10;
        let predecessor = if rng.next().is_multiple_of(2) { Some(80 + rng.next() % 8) } else { None };
        let none = |_: usize, _: usize| false;
        let w = HUNDREDTH;
        let (r, p) = (reference * w, predecessor.map(|p| p * w));
        let mut outcomes = Vec::new();
        for placement in [Placement::QRankHead, Placement::RosterFirst] {
            let entrants = f3::walk_labels(&params, w, r, p, &entries, none, placement)?;
            outcomes.push(q_outcome(&QInput { params: &params, width: w, reference_q: r, predecessor_q: p, entrants: &entrants, placement })?);
        }
        let (a, b) = (&outcomes[0], &outcomes[1]);
        assert_eq!((a.mode, a.cutoff, &a.bands, &a.units, &a.band_rank), (b.mode, b.cutoff, &b.bands, &b.units, &b.band_rank), "case {case}");
    }
    Ok(())
}

fn q_entrant(q: u64, gated: bool, cluster: Option<u16>) -> QEntrant {
    QEntrant { q: Some(q * HUNDREDTH), gated, cluster }
}

fn v3_input(entrants: &[QEntrant]) -> Result<reward_v1::QOutcome> {
    let params = f3::example_params();
    q_outcome(&QInput {
        params: &params,
        width: HUNDREDTH,
        reference_q: 90 * HUNDREDTH,
        predecessor_q: Some(88 * HUNDREDTH),
        entrants,
        placement: Placement::RosterFirst,
    })
}

#[test]
fn malformed_reward_v3_membership_is_refused() -> Result<()> {
    let ok = [q_entrant(95, true, Some(0)), q_entrant(80, true, Some(1))];
    v3_input(&ok)?;
    // A gated entrant below the cutoff without a label.
    let missing = [q_entrant(95, true, Some(0)), q_entrant(80, true, None)];
    assert_eq!(v3_input(&missing).unwrap_err(), Error::Mismatch("cluster membership"));
    // A label after the member.
    let late = [q_entrant(95, true, Some(1)), q_entrant(80, true, Some(1))];
    assert_eq!(v3_input(&late).unwrap_err(), Error::Mismatch("cluster first member"));
    // A label on an entrant that is not gated.
    let ungated = [q_entrant(95, true, Some(0)), q_entrant(80, false, Some(0))];
    assert_eq!(v3_input(&ungated).unwrap_err(), Error::Mismatch("cluster membership"));
    // A label naming a member that is not its own head.
    let not_head = [q_entrant(95, true, Some(0)), q_entrant(94, true, Some(0)), q_entrant(93, true, Some(1))];
    assert_eq!(v3_input(&not_head).unwrap_err(), Error::Mismatch("cluster first member"));
    Ok(())
}

fn v3_example(specs: &[Spec]) -> Result<ScorePackageV2> {
    fx::example_under(RewardRule::RewardV3, bp(9100), Some(bp(9000)), specs)
}

fn units(p: &ScorePackageV2) -> Vec<(u8, u16)> {
    p.entrants.iter().filter(|e| e.units > 0).map(|e| (e.identity.hotkey.0[0], e.units)).collect()
}

#[test]
fn packages_place_clusters_by_their_roster_first_member() -> Result<()> {
    // A member above its head's band and one below the cutoff share the
    // head's credit.
    let specs = [
        Spec::new(0x4a, bp(9400)),
        Spec::new(0x4b, bp(9800)).near(0x4a),
        Spec::new(0x4c, bp(9000)).near(0x4a),
        Spec::new(0x4d, bp(9600)),
    ];
    let p = v3_example(&specs)?;
    let index = fx::default_index(&fx::references(1));
    reward_v1::check_outcome(&p, &index)?;
    for e in &p.entrants[1..3] {
        assert_eq!(e.cluster_head, Some(hotkey(0x4a)));
    }
    let u = units(&p);
    assert!(u.iter().any(|&(h, _)| h == 0x4c), "below-cutoff member paid: {u:?}");
    let result = fx::result_of(&p)?;
    result.validate()?;
    // Every gated entrant carries its label in the result.
    assert!(result.entrants.iter().all(|e| matches!(e.outcome, EntrantOutcome::Scored { cluster_first_member: Some(_), .. })));
    // The same package under reward-v2 places the cluster at its best draw.
    let v2 = fx::example_under(RewardRule::RewardV2, bp(9100), Some(bp(9000)), &specs)?;
    assert_ne!(units(&v2), u);
    Ok(())
}

#[test]
fn a_gated_out_or_withheld_first_copy_leaves_the_next_copy_heading() -> Result<()> {
    let gated_out = [
        Spec::new(0x44, bp(9500)).failing(Gate::Share, "share.mae_above_threshold"),
        Spec::new(0x45, bp(9400)).near(0x44),
        Spec::new(0x46, bp(9300)),
    ];
    let p = v3_example(&gated_out)?;
    assert_eq!(p.entrants[0].cluster_head, None);
    assert_eq!(p.entrants[1].cluster_head, Some(hotkey(0x45)));
    let mut withheld = Spec::new(0x47, bp(9500));
    withheld.failed = fx::CASES;
    let p = v3_example(&[withheld, Spec::new(0x48, bp(9400)).near(0x47), Spec::new(0x49, bp(9300))])?;
    assert!(p.entrants[0].withheld() && p.entrants[0].cluster_head.is_none());
    assert_eq!(p.entrants[1].cluster_head, Some(hotkey(0x48)));
    Ok(())
}

#[test]
fn only_retained_heads_are_promotion_candidates() -> Result<()> {
    // The later member has the higher exact score; the head is promoted.
    let specs = [Spec::new(0x51, bp(9700)), Spec::new(0x52, bp(9900)).near(0x51)];
    let p = v3_example(&specs)?;
    assert_eq!(p.promoted, Some(hotkey(0x51)));
    assert!(p.promotion_audits.iter().all(|a| a.head == hotkey(0x51)));
    // A strict-tier failure passes promotion to the next retained head.
    let specs = [Spec::new(0x71, bp(9700)), Spec::new(0x72, bp(9650)), Spec::new(0x73, bp(9600))];
    let m = {
        let mut m = fx::manifest(chain(), 7, 1000, sigma_permille(40), 1, &specs);
        m.reward_rule = RewardRule::RewardV3;
        m
    };
    let index = fx::default_index(&m.references);
    let mut p = fx::package(&m, bp(9100), Some(bp(9000)), &specs)?;
    let mut measure = fx::spec_measure(&specs, 1000, |_, _| false);
    measure.audits.insert(0, (false, 0));
    fx::finalize_fixture(&mut p, &index, &mut measure)?;
    assert_eq!(p.promoted, Some(hotkey(0x72)));
    reward_v1::check_outcome(&p, &index)?;
    let refs = fx::references(1);
    let next = reward_v1::next_references(&refs, &p, &index)?;
    assert_eq!(next.index, refs.index + 1);
    Ok(())
}

#[test]
fn tampered_reward_v3_packages_are_refused() -> Result<()> {
    let specs = [
        Spec::new(0x61, bp(9500)),
        Spec::new(0x62, bp(9300)),
        Spec::new(0x63, bp(9600)).copy_of(0x61),
        Spec::new(0x64, bp(8800)),
    ];
    let p = v3_example(&specs)?;
    let index = fx::default_index(&fx::references(1));
    reward_v1::check_outcome(&p, &index)?;
    // A label removed from a gated entrant below the cutoff.
    let mut t = p.clone();
    t.entrants[3].cluster_head = None;
    assert!(reward_v1::evaluate(&t, &index).is_err());
    // A twin heading its own cluster beside its content's first head.
    let mut t = p.clone();
    t.entrants[2].cluster_head = Some(t.entrants[2].identity.hotkey);
    assert!(reward_v1::evaluate(&t, &index).is_err());
    // A member joined by distance without its statistics.
    let near = [Spec::new(0x65, bp(9500)), Spec::new(0x66, bp(9400)).near(0x65)];
    let p2 = v3_example(&near)?;
    let mut t = p2.clone();
    t.entrants[1].cluster_stats = None;
    assert!(reward_v1::evaluate(&t, &index).is_err());
    // A distance monitor below d.
    let mut t = p.clone();
    t.distance_monitor.min_entrant_pair_ppb = Some(1);
    assert!(reward_v1::evaluate(&t, &index).is_err());
    // A reward-v3 result read as reward-v2.
    let result = fx::result_of(&p)?;
    let mut as_v2 = result.clone();
    as_v2.reward_rule = RewardRule::RewardV2;
    assert!(as_v2.validate().is_err());
    // A twin labelled after its content's first head in the public result.
    let mut twin = result.clone();
    if let EntrantOutcome::Scored { cluster_first_member, .. } = &mut twin.entrants[2].outcome {
        *cluster_first_member = Some(2);
    }
    assert!(twin.validate().is_err());
    Ok(())
}

#[test]
fn reward_v2_semantics_are_unchanged() -> Result<()> {
    // A reward-v2 result carrying a label below its cutoff is refused.
    let specs = [Spec::new(0x61, bp(9500)), Spec::new(0x64, bp(8800))];
    let p = fx::example_under(RewardRule::RewardV2, bp(9100), Some(bp(9000)), &specs)?;
    let mut result = fx::result_of(&p)?;
    result.validate()?;
    if let EntrantOutcome::Scored { cluster_first_member, .. } = &mut result.entrants[1].outcome {
        *cluster_first_member = Some(1);
    }
    assert!(result.validate().is_err());
    Ok(())
}

#[test]
fn the_rule_tag_round_trips_and_unknown_tags_are_refused() -> Result<()> {
    assert_eq!(RewardRule::from_name("reward-v3"), Some(RewardRule::RewardV3));
    let mut e = crate::codec::Encoder::new();
    crate::codec::Canonical::encode_to(&RewardRule::RewardV3, &mut e);
    let bytes = e.into_bytes();
    assert_eq!(bytes, vec![4]);
    let mut d = crate::codec::Decoder::new(&bytes);
    assert_eq!(<RewardRule as crate::codec::Canonical>::decode_from(&mut d)?, RewardRule::RewardV3);
    let mut d = crate::codec::Decoder::new(&[5]);
    assert_eq!(
        <RewardRule as crate::codec::Canonical>::decode_from(&mut d).unwrap_err(),
        Error::InvalidTag { field: "reward_rule", tag: 5 }
    );
    // Permitted on every chain; the launch burn row keeps reward-v2.
    let finney = ChainBinding { genesis: FINNEY_GENESIS, netuid: 1, mechanism: 0 };
    RewardRule::RewardV3.check_chain(&finney)?;
    assert_eq!(CalculationTargetV2::launch_burn(chain()).reward_rule, RewardRule::RewardV2);
    // A reward-v3 manifest, package, result and target are accepted.
    let p = v3_example(&[Spec::new(0x61, bp(9500))])?;
    p.to_bytes()?;
    fx::result_of(&p)?.to_bytes()?;
    let target = CalculationTargetV2::for_package(&p, None)?;
    assert_eq!(target.reward_rule, RewardRule::RewardV3);
    target.to_bytes()?;
    Ok(())
}

#[test]
fn restarts_name_the_fresh_records_rules() -> Result<()> {
    let keys = crate::fixtures::trusted_keys();
    let base = crate::decisions::ChainRestartV2 {
        chain: chain(),
        floor: 400,
        launch_authorization: Digest([0x11; 32]),
        previous_target: Some(Digest([0x12; 32])),
        previous_activated_epoch: Some(300),
        last: Some(31),
        past: Vec::new(),
        unlisted: vec![30, 31],
        decision: Some(Digest([0x13; 32])),
        statement: Some(Digest([0x14; 32])),
        reward_rule: RewardRule::RewardV3,
        quant_rule: QuantRule::QuantV2,
    };
    let bytes = crate::sign(base.clone(), &crate::fixtures::authorization_key())?.to_bytes()?;
    let view = crate::decisions::open_restart(&bytes, &keys)?;
    assert_eq!((view.version, view.reward_rule, view.quant_rule, view.floor), (2, RewardRule::RewardV3, QuantRule::QuantV2, 400));
    // A version-1 restart opens as the rules then in force.
    let v1 = crate::decisions::ChainRestart {
        chain: chain(),
        floor: 400,
        launch_authorization: Digest([0x11; 32]),
        previous_target: None,
        previous_activated_epoch: None,
        last: None,
        past: Vec::new(),
        unlisted: Vec::new(),
        decision: None,
        statement: None,
    };
    let bytes = crate::sign(v1, &crate::fixtures::authorization_key())?.to_bytes()?;
    let view = crate::decisions::open_restart(&bytes, &keys)?;
    assert_eq!((view.version, view.reward_rule, view.quant_rule), (1, RewardRule::RewardV2, QuantRule::QuantV1));
    // A restart may not name the testnet shortcut.
    let bad = crate::decisions::ChainRestartV2 { reward_rule: RewardRule::TestnetRewardV0, ..base };
    assert!(bad.to_bytes().is_err());
    Ok(())
}
