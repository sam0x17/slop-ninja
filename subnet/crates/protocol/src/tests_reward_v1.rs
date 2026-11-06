//! `reward-v1` tests: the whitepaper 0.56 worked examples, recomputed
//! exactly, and the edge cases of the calculation.

use crate::bigint::{BigUint, Fraction};
use crate::fixtures_reward_v1::{
    self as fx, CASES, FIXTURE_QUANT, Spec, bp, chain, computed, computed_snapshot, example,
    finalize_fixture, finney, hotkey, result_of, sigma_permille,
};
use crate::reward_v1::{self, Measurements, Threshold};
use crate::*;

const GOLDEN: &str = include_str!("../fixtures/golden-reward-v1.txt");

#[test]
fn golden_reward_v1_report_is_unchanged() -> Result<()> {
    let report = fx::golden_report()?;
    if report != GOLDEN {
        for (i, (a, b)) in report.lines().zip(GOLDEN.lines()).enumerate() {
            assert_eq!(a, b, "golden line {}", i + 1);
        }
        assert_eq!(
            report.lines().count(),
            GOLDEN.lines().count(),
            "golden length"
        );
    }
    assert_eq!(report, GOLDEN);
    Ok(())
}

/// `round(f * 1000)`: a share in tenths of a percent, rounded half up.
fn tenths_of_percent(f: &Fraction) -> u128 {
    let num = f.num.mul_u64(2000).add(&f.den);
    let den = f.den.mul_u64(2);
    num.div_rem(&den)
        .expect("nonzero")
        .0
        .to_u128()
        .expect("small")
}

fn units(p: &ScorePackageV2) -> Vec<(u8, u16)> {
    p.entrants
        .iter()
        .filter(|e| e.units > 0)
        .map(|e| (e.identity.hotkey.0[0], e.units))
        .collect()
}

fn credits(p: &ScorePackageV2) -> Result<Vec<u128>> {
    Ok(reward_v1::evaluate(p, &fx::example_index(p))?
        .outcome
        .clusters
        .iter()
        .map(|c| tenths_of_percent(&c.credit))
        .collect())
}

fn s(tag: u8, score: u64) -> Spec {
    Spec::new(tag, bp(score))
}

const REFERENCE: u64 = 9100;

#[test]
fn bigint_matches_u128_arithmetic() {
    // xorshift64*, deterministic.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    for _ in 0..2000 {
        let (a, b) = (next() >> (next() % 64), next() >> (next() % 64));
        let (x, y) = (BigUint::from_u128(a.into()), BigUint::from_u128(b.into()));
        let (a, b) = (u128::from(a), u128::from(b));
        assert_eq!(x.mul(&y).to_u128(), Some(a * b));
        assert_eq!(x.add(&y).to_u128(), Some(a + b));
        assert_eq!(x.cmp(&y), a.cmp(&b));
        assert_eq!(
            x.checked_sub(&y).map(|d| d.to_u128()),
            a.checked_sub(b).map(Some)
        );
        let m = next();
        assert_eq!(x.mul_u64(m).to_u128(), Some(a * u128::from(m)));
        if b != 0 {
            let (q, r) = x.mul(&x).div_rem(&y).expect("nonzero");
            let (aa, bb) = (BigUint::from_u128(a * a), b);
            assert_eq!(q.mul(&y).add(&r), aa);
            assert!(r.to_u128().expect("small") < bb);
        }
        let d = (next() as u32).max(1);
        let (q, r) = x.div_rem_u32(d);
        assert_eq!(
            (q.to_u128(), r),
            (Some(a / u128::from(d)), (a % u128::from(d)) as u32)
        );
    }
    assert_eq!(
        BigUint::pow_u64(2, 200).to_string(),
        "1606938044258990275541962092341162602522202993782792835301376"
    );
    assert_eq!(
        BigUint::pow_u64(17, 40).to_string(),
        "16517976926780506002833800829531584028976727363201"
    );
    assert_eq!(
        BigUint::pow_u64(20, 30)
            .gcd(&BigUint::pow_u64(17, 30).mul_u64(4))
            .to_u128(),
        Some(4)
    );
}

// ---------------------------------------------------------------------------
// Whitepaper 0.56 worked examples
// ---------------------------------------------------------------------------

#[test]
fn margin_is_two_sigma_with_a_one_percent_floor() {
    // "At 1,000 cases delta is about 0.08; at 4,000 it is about 0.04."
    assert_eq!(
        RewardV1Params::new(sigma_permille(40), FIXTURE_QUANT).margin(),
        80_000_000_000_000_000
    );
    assert_eq!(
        RewardV1Params::new(sigma_permille(20), FIXTURE_QUANT).margin(),
        40_000_000_000_000_000
    );
    // The SE at 2,000 cases is 2.8% of headroom.
    assert_eq!(
        RewardV1Params::new(sigma_permille(28), FIXTURE_QUANT).margin(),
        56_000_000_000_000_000
    );
    // delta = max(0.01, 2 sigma): the floor binds below sigma = 0.005.
    assert_eq!(
        RewardV1Params::new(sigma_permille(3), FIXTURE_QUANT).margin(),
        MARGIN_FLOOR as u128
    );
}

#[test]
fn band_width_translates_sn78_units() -> Result<()> {
    // w / headroom in parts per million, on the exact width the copy gate's
    // score clause keeps.
    let headroom_ppm = |width: u128, reference: Score| {
        width / u128::from(SCORE_SCALE - reference.get()) / 1_000_000_000_000
    };
    let p40 = RewardV1Params::new(sigma_permille(40), FIXTURE_QUANT);
    let p20 = RewardV1Params::new(sigma_permille(20), FIXTURE_QUANT);
    // "8% of headroom at 1,000 cases ... 4% at 4,000."
    assert_eq!(headroom_ppm(reward_v1::band_width(&p40, bp(9000)), bp(9000)), 80_000);
    assert_eq!(headroom_ppm(reward_v1::band_width(&p20, bp(9000)), bp(9000)), 40_000);
    // "(0.0041 on the sealed Test's mix)": headroom B0 = 0.0518.
    let sealed = reward_v1::band_width(&p40, bp(9482));
    assert_eq!(sealed / 1_000_000_000_000_000_000_000_000_000_000, 4144);
    // "A literal 2.5-point band would span about 48% of the reference's
    // headroom": 0.025 / 0.0518 = 48.3%.
    assert_eq!(250_000 / 518, 482);
    // On the grid: W_q = w max(1, ceil(W_raw / w)). With w = 0.03 the
    // sealed mix's 0.0041 rounds up to one cell; 0.008 of headroom 0.1 at
    // w = 0.001 is 8 cells.
    let w = 30_000_000_000_000_000;
    assert_eq!(reward_v1::grid_band_width(&p40, bp(9482), w)?, Some(w));
    assert_eq!(
        reward_v1::grid_band_width(&p40, bp(9000), 1_000_000_000_000_000)?,
        Some(8_000_000_000_000_000)
    );
    assert_eq!(reward_v1::grid_band_width(&p40, Score::ONE, w)?, None);
    Ok(())
}

#[test]
fn geometric_credits_match_the_paper() -> Result<()> {
    // "a lone cluster receives the whole pool"
    let lone = example(bp(REFERENCE), None, &[s(0x21, 9600)])?;
    assert_eq!(units(&lone), vec![(0x21, 65535)]);
    // "two clusters receive 54.1% and 45.9% whether they share a band or not"
    let apart = example(bp(REFERENCE), None, &[s(0x21, 9600), s(0x22, 9400)])?;
    let together = example(bp(REFERENCE), None, &[s(0x21, 9600), s(0x22, 9590)])?;
    let ev_apart = reward_v1::evaluate(&apart, &fx::example_index(&apart))?;
    let ev_together = reward_v1::evaluate(&together, &fx::example_index(&together))?;
    assert_eq!(ev_apart.outcome.bands.len(), 2);
    assert_eq!(ev_together.outcome.bands.len(), 1);
    assert_eq!(credits(&apart)?, vec![541, 459]);
    assert_eq!(credits(&together)?, vec![541, 459]);
    // 65535 * 20/37 = 35424.32, 65535 * 17/37 = 30110.68: the larger remainder
    // takes the leftover unit.
    assert_eq!(units(&apart), vec![(0x21, 35424), (0x22, 30111)]);
    assert_eq!(units(&together), units(&apart));
    // "two clusters in the top band and one below ... 29.2%, 24.8%, and 45.9%"
    let crowded = example(
        bp(REFERENCE),
        None,
        &[s(0x21, 9600), s(0x22, 9590), s(0x23, 9400)],
    )?;
    assert_eq!(credits(&crowded)?, vec![292, 248, 459]);
    // "If an honest cluster heads a band that also holds one copy cluster, they
    // split the band's share 54.1% and 45.9%; five more perturbed copies cut
    // the honest cluster to 22.1%."
    let seven: Vec<Spec> = (0..7).map(|i| s(0x21 + i, 9600 - u64::from(i))).collect();
    let crowded = example(bp(REFERENCE), None, &seven)?;
    let ev = reward_v1::evaluate(&crowded, &fx::example_index(&crowded))?;
    assert_eq!(ev.outcome.bands.len(), 1);
    assert_eq!(ev.outcome.clusters.len(), 7);
    assert_eq!(credits(&crowded)?[0], 221);
    Ok(())
}

#[test]
fn credits_sum_to_exactly_one() -> Result<()> {
    for layout in [
        vec![1],
        vec![2],
        vec![1, 1],
        vec![2, 1],
        vec![7],
        vec![3, 1, 4, 1, 5],
        vec![1; 40],
    ] {
        let credits = reward_v1::band_credits(&layout, DECAY_BPS)?;
        let mut total = Fraction::new(BigUint::zero(), BigUint::one()).unwrap();
        for c in credits.iter().flatten() {
            total = total.add(c);
        }
        assert_eq!(
            total,
            Fraction::new(BigUint::one(), BigUint::one()).unwrap(),
            "{layout:?}"
        );
    }
    Ok(())
}

#[test]
fn fallback_cutoff_matches_the_paper_example() -> Result<()> {
    // "if R_k-1 scores 0.9400 on the batch and the top entrant 0.9500, the
    // cutoff is 0.9415."
    let t = Threshold::fallback(bp(9400), bp(9500), FALLBACK_BPS)?;
    assert_eq!(t.display(), "941500000000000000");
    assert!(t.admits(bp(9415)));
    assert!(!t.admits(Score::new(bp(9415).get() - 1)?));
    let specs = [
        s(0x31, 9500),
        Spec::published_copy(0x32, 0xe1),
        s(0x33, 9450),
        s(0x34, 9420),
        s(0x35, 9410),
        s(0x36, 9300),
    ];
    let p = example(bp(9480), Some(bp(9400)), &specs)?;
    assert_eq!(p.mode, CutoffMode::Fallback);
    assert_eq!(p.promoted, None);
    let paid: Vec<u8> = units(&p).iter().map(|(t, _)| *t).collect();
    // The exact copy of R_k fails the public-copy gate unexecuted; 0.9410 is
    // below the cutoff; 0.9300 is below R_k-1.
    assert_eq!(p.entrants[1].public_copy(), Some(PublicCopy::Content));
    assert_eq!(p.entrants[1].score, None);
    assert_eq!(paid, vec![0x31, 0x33, 0x34]);
    // The reference does not advance in a fallback cohort.
    let refs = fx::references(1);
    assert_eq!(
        reward_v1::next_references(&refs, &p, &fx::example_index(&p))?,
        refs
    );
    Ok(())
}

#[test]
fn an_entrant_short_of_the_margin_earns_only_in_a_fallback_cohort() -> Result<()> {
    // R_k = 0.91 and sigma = 0.04, so T = 0.91 + 0.08 * 0.09 = 0.9172.
    let short = s(0x41, 9159);
    let fallback = example(bp(9100), Some(bp(8775)), std::slice::from_ref(&short))?;
    assert_eq!(fallback.mode, CutoffMode::Fallback);
    assert_eq!(units(&fallback), vec![(0x41, 65535)]);
    let margin = example(bp(9100), Some(bp(8775)), &[short, s(0x42, 9600)])?;
    assert_eq!(margin.mode, CutoffMode::Margin);
    assert_eq!(units(&margin), vec![(0x42, 65535)]);
    assert_eq!(margin.promoted, Some(hotkey(0x42)));
    // Eligibility reads q against T_q = w ceil(T / w); promotion also needs
    // the exact score at the exact margin: exactly at T promotes whenever the
    // cohort is a margin cohort, one unit below never does.
    let t = Threshold::margin(bp(9100), RewardV1Params::new(sigma_permille(40), FIXTURE_QUANT).margin())?;
    assert_eq!(t.display(), "917200000000000000");
    let at = example(bp(9100), None, &[s(0x43, 9172)])?;
    // T_q is computed from q_Rk, not the exact reference score.
    let t_q = Threshold::margin(at.quant.reference_q, RewardV1Params::new(sigma_permille(40), FIXTURE_QUANT).margin())?
        .on_grid(at.quant.width)?;
    assert_eq!(at.quant.cutoff.map(|c| u128::from(c.get())), (at.mode == CutoffMode::Margin).then_some(t_q));
    if at.mode == CutoffMode::Margin {
        assert_eq!(at.promoted, Some(hotkey(0x43)));
    }
    let below = example(
        bp(9100),
        None,
        &[Spec::new(0x44, Score::new(bp(9172).get() - 1)?)],
    )?;
    let below_t_q = Threshold::margin(below.quant.reference_q, RewardV1Params::new(sigma_permille(40), FIXTURE_QUANT).margin())?
        .on_grid(below.quant.width)?;
    let passes_q = u128::from(below.entrants[0].q.unwrap().get()) >= below_t_q;
    assert_eq!(below.mode == CutoffMode::Margin, passes_q);
    assert_eq!(below.promoted, None);
    assert!(below.promotion_audits.is_empty());
    Ok(())
}

#[test]
fn heads_just_below_one_another_are_all_paid() -> Result<()> {
    // reward-v1 dropped a perturbed copy within epsilon = w / 250 of a
    // retained head; reward-v2 has no drop: distinct q values differ by at
    // least w, and equal ones share credit.
    let below = |tag, by: u64| Spec::new(tag, Score::new(bp(9600).get() - by).unwrap());
    let p = example(
        bp(REFERENCE),
        None,
        &[s(0x51, 9600), below(0x52, 10_000_000_000_000)],
    )?;
    let paid: Vec<u8> = units(&p).iter().map(|(t, _)| *t).collect();
    assert_eq!(paid, vec![0x51, 0x52]);
    let second = below(0x53, 20_000_000_000_000);
    let third = below(0x54, 40_000_000_000_000);
    let p = example(bp(REFERENCE), None, &[s(0x51, 9600), second, third])?;
    let paid: Vec<u8> = units(&p).iter().map(|(t, _)| *t).collect();
    assert_eq!(paid, vec![0x51, 0x53, 0x54]);
    Ok(())
}

// ---------------------------------------------------------------------------
// The computed cohort
// ---------------------------------------------------------------------------

#[test]
fn computed_cohort_clusters_and_pays_as_designed() -> Result<()> {
    let c = computed()?;
    let by_name = |name: &str| {
        let i = c.models.iter().position(|m| m.name == name).expect("model");
        &c.package.entrants[i]
    };
    assert_eq!(c.package.reference.score, bp(9100));
    assert_eq!(c.package.predecessor.as_ref().unwrap().score, bp(8775));
    assert_eq!(by_name("alpha").score, Some(bp(9600)));
    assert_eq!(by_name("beta").score, Some(bp(9350)));
    assert_eq!(by_name("gamma").score, by_name("beta").score);
    assert_eq!(by_name("theta").score, Some(bp(8640)));
    assert_eq!(c.package.mode, CutoffMode::Margin);
    assert_eq!(c.package.promoted, Some(hotkey(0x11)));
    // Identical content (alpha_copy) and RMS 0.005 (alpha_close) join alpha.
    for name in ["alpha", "alpha_copy", "alpha_close"] {
        assert_eq!(by_name(name).cluster_head, Some(hotkey(0x11)), "{name}");
        assert_eq!(by_name(name).units, 8492, "{name}");
    }
    for name in ["beta", "beta_copy_1", "beta_copy_2"] {
        assert_eq!(by_name(name).cluster_head, Some(hotkey(0x12)), "{name}");
        assert_eq!(by_name(name).units, 3316, "{name}");
    }
    assert_eq!(by_name("iota").units, 21654);
    // gamma ties beta's exact score at RMS 0.032: its own cluster, paid
    // beside beta's (no near-equal drop); its draw ranks it below beta, and
    // the two clusters split the band position beta held alone before.
    assert_eq!(by_name("gamma").cluster_head, Some(hotkey(0x15)));
    assert_eq!(by_name("gamma").units, 8457);
    assert_eq!(3 * 3316 + 8457, 3 * 6135);
    for name in ["delta", "zeta", "eta", "theta"] {
        assert_eq!(by_name(name).cluster_head, None, "{name}");
        assert_eq!(by_name(name).units, 0, "{name}");
    }
    let snapshot = computed_snapshot(&c.specs);
    let row = reward_v1::derive_row(&c.target, Some(&result_of(&c.package)?), &snapshot)?;
    // Owner at UID 2 receives iota's (deregistered) and beta_copy_2's (new
    // generation) units; alpha is paid at its moved UID 0x21.
    assert_eq!(
        fx::row_entries(&row),
        "2:24970,18:3316,21:8457,22:8492,23:3316,27:8492,33:8492"
    );
    c.closure.check(&c.manifest, &c.package, Some(&c.target))?;
    for (i, spec) in c.specs.iter().enumerate() {
        let opening = c.closure.opening(&c.package, i)?;
        opening
            .check_chain_payload(&fx::opening(&chain(), c.manifest.cohort, spec).chain_payload())?;
    }
    Ok(())
}

#[test]
fn every_v2_record_round_trips_canonically() -> Result<()> {
    fn round_trip<R: Record + PartialEq + core::fmt::Debug>(record: &R) -> Result<()> {
        let bytes = record.to_bytes()?;
        let decoded = R::from_bytes(&bytes)?;
        assert_eq!(&decoded, record);
        assert_eq!(decoded.to_bytes()?, bytes);
        Ok(())
    }
    let c = computed()?;
    round_trip(&c.manifest)?;
    round_trip(&c.package)?;
    round_trip(&c.target)?;
    round_trip(&CalculationTargetV2::launch_burn(chain()))?;
    round_trip(&c.closure)?;
    let snapshot = computed_snapshot(&c.specs);
    round_trip(&snapshot)?;
    round_trip(&reward_v1::derive_row(
        &c.target,
        Some(&result_of(&c.package)?),
        &snapshot,
    )?)?;
    let opening = fx::opening(&chain(), 3, &c.specs[0]);
    let upload = submission::SubmissionUpload::new(opening.clone(), b"header")?;
    assert_eq!(upload.to_bytes()?.len(), submission::SubmissionUpload::ENCODED_LEN);
    assert!(submission::SubmissionUpload::new(opening.clone(), b"").is_err());
    let too_long = vec![0u8; crate::MAX_ARTIFACT_HEADER_LEN as usize + 1];
    assert!(submission::SubmissionUpload::new(opening, &too_long).is_err());
    round_trip(&upload)?;
    round_trip(&c.authorization.record)?;
    round_trip(&c.renewal.record)?;
    round_trip(&LaunchAuthorization::new(&chain(), 1, 20)?)?;
    round_trip(&c.index.contents)?;
    round_trip(&c.index.fingerprints)?;
    round_trip(&fx::verified(0x11, 0x11).header)?;
    // A version-5 manifest is not a version-1 manifest.
    assert_eq!(
        CohortManifest::from_bytes(&c.manifest.to_bytes()?).unwrap_err(),
        Error::WrongVersion {
            kind: "cohort-manifest",
            version: 5
        }
    );
    round_trip(&result_of(&c.package)?)?;
    let audit = fx::promotion_audit(hotkey(0x31), true, 1);
    let mut e = Encoder::new();
    audit.encode_to(&mut e);
    let bytes = e.into_bytes();
    let mut d = Decoder::new(&bytes);
    assert_eq!(PromotionAudit::decode_from(&mut d)?, audit);
    d.finish()?;
    Ok(())
}

#[test]
fn signed_v2_package_rejects_any_single_bit_flip() -> Result<()> {
    let c = computed()?;
    let bytes = sign(c.package.clone(), &crate::fixtures::package_key())?.to_bytes()?;
    let keys = crate::fixtures::trusted_keys();
    assert_eq!(Signed::<ScorePackageV2>::open(&bytes, &keys)?, c.package);
    for i in (0..bytes.len()).step_by(7) {
        let mut tampered = bytes.clone();
        tampered[i] ^= 0x01;
        assert!(
            Signed::<ScorePackageV2>::open(&tampered, &keys).is_err(),
            "byte {i}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn reward_v1_is_permitted_on_finney_and_v0_is_not() -> Result<()> {
    assert_eq!(
        FINNEY_GENESIS.hex(),
        "2f0555cc76fc2840a25a6ea3b9637146806f1f44b090c175ffde2a7e5ab36c03"
    );
    RewardRule::RewardV2.check_chain(&finney())?;
    assert_eq!(
        RewardRule::TestnetRewardV0
            .check_chain(&finney())
            .unwrap_err(),
        Error::Mismatch("testnet_reward_v0 genesis")
    );
    let specs = [s(0xb1, 9600)];
    let m = fx::manifest(finney(), 1, 2000, sigma_permille(28), 0, &specs);
    let (p, _) = fx::finalized(&m, bp(REFERENCE), None, &specs)?;
    let t = CalculationTargetV2::for_package(&p, None)?;
    let row = reward_v1::derive_row(
        &t,
        Some(&result_of(&p)?),
        &fx::snapshot(finney(), 0, &specs),
    )?;
    assert_eq!(fx::row_entries(&row), "177:65535");
    let mut small = m.clone();
    small.batch = fx::batch(1000);
    assert_eq!(
        small.to_bytes().unwrap_err(),
        Error::Invalid("batch below mainnet floor")
    );
    // A v0 rule cannot enter a v2 record, nor reward-v1 a v1 record.
    let mut v0 = m.clone();
    v0.reward_rule = RewardRule::TestnetRewardV0;
    assert_eq!(
        v0.to_bytes().unwrap_err(),
        Error::Mismatch("reward rule record version")
    );
    let mut v1 = crate::fixtures::manifest();
    v1.reward_rule = RewardRule::RewardV2;
    assert_eq!(
        v1.to_bytes().unwrap_err(),
        Error::Mismatch("reward rule record version")
    );
    Ok(())
}

#[test]
fn single_entrant_all_in_one_band_and_ties() -> Result<()> {
    let one_band = example(
        bp(REFERENCE),
        None,
        &[s(0x61, 9600), s(0x62, 9590), s(0x63, 9580)],
    )?;
    // 1 / (1 + r + r^2), r / ..., r^2 / ... of 65535.
    assert_eq!(
        units(&one_band),
        vec![(0x61, 25475), (0x62, 21654), (0x63, 18406)]
    );
    // Equal exact scores without near-duplicate predictions: no near-equal
    // drop, so both clusters are paid; their independent draws order them.
    let tie = example(bp(REFERENCE), None, &[s(0x81, 9600), s(0x82, 9600)])?;
    assert_eq!(units(&tie).len(), 2);
    // Largest-remainder ties go in roster order, whatever the rank order.
    let shares = vec![Fraction::new(BigUint::one(), BigUint::from_u128(4)).unwrap(); 4];
    assert_eq!(
        reward_v1::largest_remainder(65535, &shares, &[0, 1, 2, 3])?,
        vec![16384, 16384, 16384, 16383]
    );
    assert_eq!(
        reward_v1::largest_remainder(65535, &shares, &[3, 0, 2, 1])?,
        vec![16383, 16384, 16384, 16384]
    );
    Ok(())
}

#[test]
fn sybil_clusters_bound_the_cluster_not_the_party() -> Result<()> {
    // One party holding 3 of a cluster's 4 members receives 3/4 of its credit.
    let four = example(
        bp(REFERENCE),
        None,
        &[
            s(0x71, 9600),
            s(0x72, 9600).copy_of(0x71),
            s(0x73, 9600).copy_of(0x71),
            s(0x74, 9600).copy_of(0x71),
        ],
    )?;
    assert_eq!(
        units(&four),
        vec![(0x71, 16384), (0x72, 16384), (0x73, 16384), (0x74, 16383)]
    );
    // Adding hotkeys never enlarges the cluster's total beside an honest entrant.
    let alone = example(bp(REFERENCE), None, &[s(0x71, 9600), s(0x75, 9400)])?;
    let crowded = example(
        bp(REFERENCE),
        None,
        &[
            s(0x71, 9600),
            s(0x72, 9600).copy_of(0x71),
            s(0x73, 9600).copy_of(0x71),
            s(0x75, 9400),
        ],
    )?;
    // The copies' draws decide which of the three heads the cluster.
    let total = |p: &ScorePackageV2| {
        p.entrants
            .iter()
            .filter(|e| e.identity.hotkey != hotkey(0x75))
            .map(|e| u32::from(e.units))
            .sum::<u32>()
    };
    assert_eq!(total(&alone), total(&crowded));
    assert_eq!(units(&crowded).last(), units(&alone).last());
    Ok(())
}

#[test]
fn no_target_cases_keep_the_previous_target_or_burn() -> Result<()> {
    let no = |p: &ScorePackageV2| {
        assert_eq!(p.mode, CutoffMode::NoTarget);
        assert!(units(p).is_empty());
        assert_eq!(
            CalculationTargetV2::for_package(p, None).unwrap_err(),
            Error::Invalid("cohort produced no target")
        );
    };
    // R_0 in force (no predecessor) and nobody reaches the margin.
    no(&example(bp(REFERENCE), None, &[s(0x91, 9150)])?);
    // A predecessor exists but nobody scores above it. (An exact tie with
    // the predecessor is not a tie on q: the two values draw independent
    // offsets, so either can land a grid cell higher; the fixture stays a
    // basis point below.)
    no(&example(
        bp(REFERENCE),
        Some(bp(9165)),
        &[s(0x92, 9150), s(0x93, 9164)],
    )?);
    // An empty roster.
    no(&example(bp(REFERENCE), Some(bp(9000)), &[])?);
    // A perfect reference leaves no headroom and no band width. The rule
    // reads q: an exact S_Rk = 1 can quantize a cell below 1 (negative
    // noise), so the case is the q-level one.
    let params = RewardV1Params::new(sigma_permille(40), FIXTURE_QUANT);
    let w = 10_000_000_000_000_000;
    let top = [reward_v1::QEntrant { q: Some(SCORE_SCALE), gated: true, cluster: None }];
    let perfect = reward_v1::q_outcome(&reward_v1::QInput {
        params: &params,
        width: w,
        reference_q: SCORE_SCALE,
        predecessor_q: Some(900_000_000_000_000_000),
        entrants: &top,
    })?;
    assert_eq!((perfect.mode, perfect.band_width), (CutoffMode::NoTarget, None));
    assert!(perfect.units.iter().all(|&u| u == 0));
    // Gate failures earn nothing.
    no(&example(
        bp(REFERENCE),
        None,
        &[s(0x95, 9900).failing(Gate::Attribution, "attribution.open_set_rejection")],
    )?);
    // With no target ever produced, the active target is the launch burn row.
    let specs = [s(0x96, 9000)];
    let launch = CalculationTargetV2::launch_burn(chain());
    let row = reward_v1::derive_row(&launch, None, &fx::snapshot(chain(), 5, &specs))?;
    assert_eq!(fx::row_entries(&row), "5:65535");
    Ok(())
}

#[test]
fn units_of_unregistered_generations_burn_through_the_owner_uid() -> Result<()> {
    let specs = [s(0x21, 9600), s(0x22, 9400)];
    let p = example(bp(REFERENCE), None, &specs)?;
    let t = CalculationTargetV2::for_package(&p, None)?;
    let result = result_of(&p)?;
    let scored = Some(&result);
    let mut snap = fx::snapshot(chain(), 9, &specs);
    assert_eq!(
        fx::row_entries(&reward_v1::derive_row(&t, scored, &snap)?),
        "33:35424,34:30111"
    );
    // Re-registration is a new generation: its units burn.
    snap.neurons
        .iter_mut()
        .find(|n| n.uid == 0x22)
        .unwrap()
        .registration_block += 1;
    assert_eq!(
        fx::row_entries(&reward_v1::derive_row(&t, scored, &snap)?),
        "9:30111,33:35424"
    );
    // Deregistration: everything burns; the row is never empty.
    snap.neurons.retain(|n| n.hotkey == fx::owner_hotkey());
    assert_eq!(
        fx::row_entries(&reward_v1::derive_row(&t, scored, &snap)?),
        "9:65535"
    );
    Ok(())
}

#[test]
fn clusters_do_not_chain_through_members() -> Result<()> {
    // B is within tolerance of head A, C within tolerance of B only.
    let specs = [s(0x21, 9600), s(0x22, 9500), s(0x23, 9400)];
    let m = fx::manifest(chain(), 7, 1000, sigma_permille(40), 0, &specs);
    let mut p = fx::package(&m, bp(REFERENCE), None, &specs)?;
    let near =
        |a: usize, b: usize| (a.min(b), a.max(b)) == (0, 1) || (a.min(b), a.max(b)) == (1, 2);
    finalize_fixture(
        &mut p,
        &fx::default_index(&m.references),
        &mut fx::spec_measure(&specs, 1000, near),
    )?;
    let heads: Vec<_> = p
        .entrants
        .iter()
        .map(|e| e.cluster_head.unwrap().0[0])
        .collect();
    assert_eq!(heads, vec![0x21, 0x21, 0x23]);
    Ok(())
}

fn ppb(values: &[u32]) -> Vec<Probability> {
    values
        .iter()
        .map(|&v| Probability::new(v).unwrap())
        .collect()
}

#[test]
fn aligned_distance_ignores_rank_preserving_maps() -> Result<()> {
    let y = ppb(&[
        50_000_000,
        200_000_000,
        200_000_000,
        450_000_000,
        700_000_000,
        990_000_000,
    ]);
    // Any non-decreasing map is at distance zero, however far it moves the
    // probabilities: a temperature-like squaring, a bias with clamping, a
    // step function.
    let maps: [fn(u32) -> u32; 3] = [
        |v| (u64::from(v) * u64::from(v) / 1_000_000_000) as u32,
        |v| (v + 300_000_000).min(1_000_000_000),
        |v| if v < 300_000_000 { 0 } else { 1_000_000_000 },
    ];
    for map in maps {
        let x: Vec<Probability> = y
            .iter()
            .map(|p| Probability::new(map(p.ppb())).unwrap())
            .collect();
        assert_eq!(reward_v1::aligned_rss(&x, &y)?, 0);
    }
    // Ties in y form one group fitted by its mean; the order within a group
    // is irrelevant.
    let tied = ppb(&[100_000_000, 100_000_000, 100_000_000, 100_000_000]);
    let x = ppb(&[100_000_000, 300_000_000, 200_000_000, 400_000_000]);
    let mut permuted = x.clone();
    permuted.reverse();
    assert_eq!(reward_v1::aligned_rss(&x, &tied)?, 5 * 10u128.pow(16));
    assert_eq!(
        reward_v1::aligned_rss(&permuted, &tied)?,
        reward_v1::aligned_rss(&x, &tied)?
    );
    // A reversed order pools into one block: the residual is the variance.
    let up = ppb(&[100_000_000, 200_000_000, 300_000_000]);
    let down = ppb(&[300_000_000, 200_000_000, 100_000_000]);
    assert_eq!(reward_v1::aligned_rss(&down, &up)?, 2 * 10u128.pow(16));
    // Each block contributes floor(S^2 / c): x = (1, 2) ppb over a tie
    // leaves 1^2 + 2^2 - floor(9 / 2) = 1, never a negative residual.
    let tie2 = ppb(&[7, 7]);
    assert_eq!(reward_v1::aligned_rss(&ppb(&[1, 2]), &tie2)?, 1);
    assert_eq!(reward_v1::aligned_rss(&[], &[])?, 0);
    // The relation takes the smaller direction and is symmetric.
    let a = fx::predictions(&[200_000_000, 300_000_000, 100_000_000, 250_000_000]);
    let b = fx::predictions(&[150_000_000, 100_000_000, 350_000_000, 200_000_000]);
    let ab = reward_v1::near_stats(&a, &b)?;
    let ba = reward_v1::near_stats(&b, &a)?;
    assert_eq!((ab.rss_fwd, ab.rss_rev), (ba.rss_rev, ba.rss_fwd));
    assert_eq!(ab.min_rss(), ba.min_rss());
    assert_eq!(ab.distance_ppb(), ba.distance_ppb());
    // The threshold is exact: a uniform shift is a rank-preserving map, so it
    // is at distance zero, while noise of RMS exactly d is within d and one
    // ppb^2 more is not.
    let d = u128::from(NEAR_COPY_PPB);
    let at = NearStats {
        common: 4,
        rss_fwd: 4 * d * d,
        rss_rev: 4 * d * d + 7,
    };
    assert!(at.within(NEAR_COPY_PPB)?);
    assert_eq!(at.distance_ppb(), Some(NEAR_COPY_PPB));
    let past = NearStats {
        rss_fwd: 4 * d * d + 1,
        rss_rev: 4 * d * d + 1,
        ..at
    };
    assert!(!past.within(NEAR_COPY_PPB)?);
    assert_eq!(past.distance_ppb(), Some(NEAR_COPY_PPB));
    // Documents on which either model failed are left out; with no common
    // document the pair is not near.
    let mut failed = a.clone();
    failed[0] = None;
    let stats = reward_v1::near_stats(&failed, &a)?;
    assert_eq!(stats.common, 3);
    assert_eq!(stats.min_rss(), 0);
    let none = reward_v1::near_stats(&[None, Some(a[1].unwrap())], &[Some(a[0].unwrap()), None])?;
    assert_eq!((none.common, none.distance_ppb()), (0, None));
    assert!(!none.within(NEAR_COPY_PPB)?);
    assert_eq!(
        reward_v1::near_stats(&fx::predictions(&[1]), &fx::predictions(&[1, 2])).unwrap_err(),
        Error::Mismatch("prediction vectors")
    );
    Ok(())
}

#[test]
fn near_copies_must_beat_the_other_by_half_a_band() -> Result<()> {
    // sigma = 0.04 and S_y = 0.91: w = 0.0072, so w / 2 = 0.0036.
    let params = RewardV1Params::new(sigma_permille(40), FIXTURE_QUANT);
    let width = reward_v1::band_width(&params, bp(9100));
    let zero = fx::aligned(1000);
    for (x, near) in [
        (bp(8000), true),
        (bp(9100), true),
        (bp(9136), true),
        (Score::new(bp(9136).get() + 1)?, false),
    ] {
        assert_eq!(reward_v1::near(&zero, x, bp(9100), &params, width)?, near);
    }
    // While 2 sigma >= 0.01, w / 2 is half the promotion margin:
    // delta (1 - S_Rk) = w.
    let margin = Threshold::margin(bp(9100), params.margin())?;
    assert_eq!(
        margin.num - u128::from(bp(9100).get()) * u128::from(SCORE_SCALE),
        width
    );
    // Below that, delta is the 0.01 floor and w / 2 is less than half the
    // margin: at sigma = 0.004, w = 0.008 (1 - S_Rk) and the margin is 0.01
    // (1 - S_Rk).
    let tight = RewardV1Params::new(sigma_permille(4), FIXTURE_QUANT);
    let tight_width = reward_v1::band_width(&tight, bp(9100));
    let tight_margin = Threshold::margin(bp(9100), tight.margin())?;
    let margin_width = tight_margin.num - u128::from(bp(9100).get()) * u128::from(SCORE_SCALE);
    assert_eq!(tight_width * 10, margin_width * 8);
    // Beyond d, no score makes a near copy.
    assert!(!reward_v1::near(
        &fx::far(1000),
        bp(9100),
        bp(9100),
        &params,
        width
    )?);
    // A perfect reference leaves no band width: only a strictly higher score
    // escapes.
    let flat = reward_v1::band_width(&params, Score::ONE);
    assert_eq!(flat, 0);
    assert!(reward_v1::near(
        &zero,
        Score::ONE,
        Score::ONE,
        &params,
        flat
    )?);
    Ok(())
}

#[test]
fn maximum_roster_stays_exact_and_fast() -> Result<()> {
    // 256 clusters in 256 bands: the largest exact credits. sigma = 10^-6 and
    // R_k = 0.1 give w = 1.8e-6; scores 10^-5 apart land in distinct bands.
    // Every one-byte family enters, so R_0 is a reference no entrant copies.
    let spread: Vec<Spec> = (0..MAX_ROSTER)
        .map(|i| {
            Spec::new(
                i as u8,
                Score::new(SCORE_SCALE - u64::from(i) * 10_000_000_000_000).unwrap(),
            )
        })
        .collect();
    // epsilon = w / 250 allows at most 250 retained clusters per band: 200
    // clusters 0.00029 apart in one band (w = 0.072), plus 56 copies of the head.
    let packed: Vec<Spec> = (0..MAX_ROSTER)
        .map(|i| {
            if i < 200 {
                Spec::new(
                    i as u8,
                    Score::new(999_900_000_000_000_000 - u64::from(i) * 290_000_000_000_000)
                        .unwrap(),
                )
            } else {
                Spec::new(i as u8, Score::new(999_900_000_000_000_000).unwrap()).copy_of(0)
            }
        })
        .collect();
    for (specs, sigma, clusters, bands) in [
        (spread, 1_000_000_000_000, 256, 256),
        (packed, sigma_permille(40), 200, 1),
    ] {
        let references = ReferenceSet {
            index: 0,
            current: fx::independent_reference(),
            predecessor: None,
        };
        let mut m = fx::manifest_with(
            chain(),
            7,
            1000,
            sigma,
            references,
            &fx::default_index(&references),
            fx::probe(fx::EXAMPLE_PROBE_SIZE),
            &specs,
        );
        m.roster_cap = MAX_ROSTER;
        let (p, ev) = fx::finalized(&m, bp(1000), None, &specs)?;
        let total: u32 = p.entrants.iter().map(|e| u32::from(e.units)).sum();
        assert_eq!(total, 65535);
        // The exact credits of the largest roster are recordable.
        assert_eq!(p.credits.len(), clusters);
        assert!(p.credits.iter().all(|c| c.den.bits() < 1120));
        assert_eq!(ScorePackageV2::from_bytes(&p.to_bytes()?)?, p);
        let placed = &ev.outcome.clusters;
        assert_eq!(placed.len(), clusters);
        assert_eq!(ev.outcome.bands.len(), bands);
        // Credits never increase down the ranking.
        assert!(placed.windows(2).all(|w| w[0].credit >= w[1].credit));
    }
    Ok(())
}

#[test]
fn extreme_parameters_do_not_overflow() -> Result<()> {
    // sigma = 1: delta = 2 can never be met; the fallback still bands with w = 2.
    let specs = [Spec::new(0xa1, Score::ONE)];
    let m = fx::manifest(chain(), 8, 1000, SCORE_SCALE, 1, &specs);
    let mut p = fx::package(&m, Score::ZERO, Some(Score::ZERO), &specs)?;
    let ev = finalize_fixture(
        &mut p,
        &fx::default_index(&m.references),
        &mut fx::spec_measure(&specs, 1000, |_, _| false),
    )?;
    assert_eq!(ev.mode(), CutoffMode::Fallback);
    assert_eq!(ev.outcome.clusters[0].band, 0);
    // sigma = 10^-18: W_q is one grid cell, and a perfect score clamps to
    // the top band.
    let specs = [Spec::new(0xa2, Score::ONE), s(0xa3, 9999)];
    let m = fx::manifest(chain(), 9, 1000, 1, 0, &specs);
    let mut p = fx::package(&m, bp(9999), None, &specs)?;
    let ev = finalize_fixture(
        &mut p,
        &fx::default_index(&m.references),
        &mut fx::spec_measure(&specs, 1000, |_, _| false),
    )?;
    let width = p.quant.band_width.expect("band width");
    assert_eq!(width, p.quant.width);
    if p.entrants[0].q == Some(Score::ONE) {
        assert_eq!(ev.outcome.clusters[0].band, SCORE_SCALE.div_ceil(width) - 1);
    }
    // The band of the largest score under the widest band stays in range.
    assert_eq!(reward_v1::grid_band(SCORE_SCALE, 2 * SCORE_SCALE), 0);
    Ok(())
}

#[test]
fn per_tempo_operation_ids_bind_epoch_validator_and_target() -> Result<()> {
    let c = computed()?;
    let v = AccountId([0x66; 32]);
    let e = c.closure.closed_epoch;
    let id = |t: &CalculationTargetV2, v, e| WeightOperationV2::for_target(t, v, e).map(|o| o.id());
    let base = id(&c.target, v, e)?;
    assert!(base.starts_with(WEIGHT_OPERATION_V2_PREFIX));
    assert_eq!(base, id(&c.target, v, e)?);
    assert_ne!(base, id(&c.target, v, e + 1)?);
    assert_ne!(base, id(&c.target, AccountId([0x67; 32]), e)?);
    let launch = CalculationTargetV2::launch_burn(chain());
    assert_ne!(base, id(&launch, v, e)?);
    let authorized = c.checked_authorization()?;
    let active = c.active_after()?;
    let op = |e| authorized.operation(None, &[], &active, v, e);
    assert_eq!(
        op(e - 1).unwrap_err(),
        Error::Invalid("epoch before target activation")
    );
    assert_eq!(op(e)?.id(), base);
    Ok(())
}

#[test]
fn reward_intent_authorization_never_expires() -> Result<()> {
    let c = computed()?;
    let keys = crate::fixtures::trusted_keys();
    let v = AccountId([0x66; 32]);
    let a = &c.authorization.record;
    let authorized = c.checked_authorization()?;
    assert_eq!(authorized.cohort_authorization(), Some(a));
    assert_eq!(authorized.launch_authorization(), None);
    assert_eq!(authorized.manifest(), Some(&c.manifest));
    assert_eq!(authorized.package(), Some(&c.package));
    assert_eq!(authorized.target(), &c.target);
    assert_eq!(authorized.digest(), a.digest()?);
    // The renew-by hint is the manifest's 2K tempos after publication at
    // tempo 16; it is only a hint.
    assert_eq!(a.renew_by_tempos(), 40);
    assert_eq!(a.issued_epoch, c.manifest.opened_epoch + 16);
    assert_eq!(authorized.renew_by_epoch(), a.renew_by_epoch);
    let active = c.active_after()?;
    let op = |renewal: Option<&CheckedRenewal<'_>>, e| authorized.operation(renewal, &[], &active, v, e);
    // Every epoch from activation on, through and long past the hint, with
    // no renewal: a coordinator silent for any length of time stops nothing.
    for e in (c.closure.closed_epoch..=a.renew_by_epoch + 3).chain([a.renew_by_epoch + 100, 1_000_000, u64::MAX]) {
        assert_eq!(
            op(None, e)?,
            WeightOperationV2::for_target(&c.target, v, e)?
        );
    }
    // A hint refresh names the authorization and changes nothing else.
    let renewal = c.checked_renewal(&authorized)?;
    let r = &c.renewal.record;
    assert_eq!(renewal.renewal(), r);
    assert_eq!(r.renew_by_epoch, c.manifest.opened_epoch + 30 + 40);
    assert_eq!(op(Some(&renewal), r.renew_by_epoch + 1)?, op(None, r.renew_by_epoch + 1)?);
    // Before its own issue an authorization yields nothing.
    let late = fx::signed(fx::authorization(&c.manifest, &c.package, &c.target, 2, a.renew_by_epoch + 20)?)?;
    let late = RewardIntentAuthorization::check(&late, &keys, &c.manifest, &c.package, &c.target, &active)?;
    assert_eq!(
        late.operation(None, &[], &active, v, a.renew_by_epoch + 19).unwrap_err(),
        Error::Invalid("epoch before authorization")
    );
    late.operation(None, &[], &active, v, a.renew_by_epoch + 20)?;
    // Neither the authorization nor a renewal can retarget a commitment: the
    // authorization is checked against the one target it names, and a renewal
    // extends only the checked authorization it names.
    let launch = CalculationTargetV2::launch_burn(chain());
    assert_eq!(
        RewardIntentAuthorization::check(
            &c.authorization,
            &keys,
            &c.manifest,
            &c.package,
            &launch,
            &c.active_after()?
        )
        .unwrap_err(),
        Error::Mismatch("authorized target")
    );
    let first = fx::signed(LaunchAuthorization::new(&chain(), 1, a.issued_epoch)?)?;
    let first = LaunchAuthorization::check(&first, &keys, &chain(), &c.active_before()?)?;
    assert_eq!(
        RewardIntentRenewal::check(&c.renewal, &keys, &first).unwrap_err(),
        Error::Mismatch("renewed authorization")
    );
    assert_eq!(
        first
            .operation(Some(&renewal), &[], &c.active_before()?, v, r.issued_epoch)
            .unwrap_err(),
        Error::Mismatch("renewed authorization")
    );
    // Both records are verified under the authorization role only, inside the
    // checked API.
    assert_eq!(
        Signed::<RewardIntentAuthorization>::open(&c.authorization.to_bytes()?, &keys)?,
        *a
    );
    let by_package = sign(r.clone(), &crate::fixtures::package_key())?;
    assert_eq!(
        RewardIntentRenewal::check(&by_package, &keys, &authorized).unwrap_err(),
        Error::UntrustedKey
    );
    let mut forged = c.renewal.clone();
    forged.signature[5] ^= 0x10;
    assert_eq!(
        RewardIntentRenewal::check(&forged, &keys, &authorized).unwrap_err(),
        Error::BadSignature
    );
    Ok(())
}

#[test]
fn a_no_target_package_authorizes_the_target_it_leaves_live() -> Result<()> {
    let c = computed()?;
    let keys = crate::fixtures::trusted_keys();
    let v = AccountId([0x66; 32]);
    let specs = [s(0x91, 9050)];
    let m4 = fx::manifest(chain(), 4, 1000, sigma_permille(40), 2, &specs);
    let (p4, _) = fx::finalized(&m4, bp(9600), Some(bp(9100)), &specs)?;
    assert_eq!(p4.mode, CutoffMode::NoTarget);
    let carried = fx::signed(fx::authorization(
        &m4,
        &p4,
        &c.target,
        3,
        m4.opened_epoch + 16,
    )?)?;
    let active = c.active_after()?;
    let authorized =
        RewardIntentAuthorization::check(&carried, &keys, &m4, &p4, &c.target, &active)?;
    // Cohort 3's closure still activates its target; cohort 4's package
    // authorizes it again, with no end either.
    let end = carried.record.renew_by_epoch;
    assert!(end > c.authorization.record.renew_by_epoch);
    authorized.operation(None, &[], &active, v, end)?;
    authorized.operation(None, &[], &active, v, end + 1)?;
    // Cohort 4's own closure names no target, so it activates nothing and
    // cohort 3's target stays the active one.
    let closure4 = fx::closure(&m4, &p4, None, &specs)?;
    let checked4 = closure4.check(&m4, &p4, None)?;
    assert_eq!(
        ActiveTarget::activated(&checked4).unwrap_err(),
        Error::Invalid("closure activated no target")
    );
    // A validator with a gap in its history, still on the launch burn row,
    // commits nothing under the checked authorization.
    assert_eq!(
        authorized
            .operation(None, &[], &c.active_before()?, v, end)
            .unwrap_err(),
        Error::Mismatch("authorized target not active")
    );
    // The carried target must be the one the validator holds active: a
    // validator still on the launch burn row (a gap in its history) refuses
    // it until it has checked cohort 3's closure.
    assert_eq!(
        RewardIntentAuthorization::check(&carried, &keys, &m4, &p4, &c.target, &c.active_before()?)
            .unwrap_err(),
        Error::Mismatch("carried target not active")
    );
    // The launch burn row is carried while no cohort target has ever been
    // active, and never after.
    let launch = CalculationTargetV2::launch_burn(chain());
    let m0 = fx::manifest(chain(), 0, 1000, sigma_permille(40), 0, &specs);
    let (p0, _) = fx::finalized(&m0, bp(9100), None, &specs)?;
    let burn = fx::signed(fx::authorization(&m0, &p0, &launch, 1, 16)?)?;
    let unactivated = ActiveTarget::launch(&chain())?;
    let first = RewardIntentAuthorization::check(&burn, &keys, &m0, &p0, &launch, &unactivated)?;
    first.operation(None, &[], &unactivated, v, 16)?;
    // Once cohort 3's target is active, the carried burn row commits nothing.
    assert_eq!(
        first.operation(None, &[], &active, v, 16).unwrap_err(),
        Error::Mismatch("authorized target not active")
    );
    assert_eq!(
        RewardIntentAuthorization::check(&burn, &keys, &m0, &p0, &launch, &active).unwrap_err(),
        Error::Mismatch("carried target not active")
    );
    Ok(())
}

#[test]
fn launch_authorization_serves_until_the_first_target_activates() -> Result<()> {
    let keys = crate::fixtures::trusted_keys();
    let v = AccountId([0x66; 32]);
    let launch = CalculationTargetV2::launch_burn(chain());
    let unactivated = ActiveTarget::launch(&chain())?;
    assert!(unactivated.is_launch_burn());
    assert_eq!(unactivated.digest(), launch.digest()?);
    // Issued at the first tempo of cohort 1 (epoch 20), before any manifest:
    // it names only the chain and the burn row, with the fixed hint distance.
    let first = LaunchAuthorization::new(&chain(), 1, 20)?;
    assert_eq!(
        first.renew_by_epoch - first.issued_epoch,
        LAUNCH_RENEW_BY_TEMPOS
    );
    assert_eq!(first.target, launch.digest()?);
    let signed = fx::signed(first.clone())?;
    let checked = LaunchAuthorization::check(&signed, &keys, &chain(), &unactivated)?;
    assert_eq!(checked.launch_authorization(), Some(&first));
    assert_eq!((checked.manifest(), checked.package()), (None, None));
    assert_eq!(checked.target(), &launch);
    assert_eq!(
        checked.operation(None, &[], &unactivated, v, 20)?,
        WeightOperationV2::for_target(&launch, v, 20)?
    );
    assert_eq!(
        checked.operation(None, &[], &unactivated, v, 19).unwrap_err(),
        Error::Invalid("epoch before authorization")
    );
    // No end: the hint (epoch 60) passes and the burn row keeps committing.
    for e in [60, 61, 117, 118, 10_000] {
        checked.operation(None, &[], &unactivated, v, e)?;
    }
    // A hint refresh keeps the same fixed distance and is never needed.
    let renewal = fx::signed(RewardIntentRenewal::new(&checked, 1, 30)?)?;
    assert_eq!(renewal.record.renew_by_epoch, 30 + LAUNCH_RENEW_BY_TEMPOS);
    let renewal = RewardIntentRenewal::check(&renewal, &keys, &checked)?;
    checked.operation(Some(&renewal), &[], &unactivated, v, 70)?;
    // Cohort 1 publishes the first package with a new target. Its target is
    // pending until the closing boundary, so the burn row stays active and
    // the launch authorization keeps serving.
    let specs = [s(0x23, 9600)];
    let m1 = fx::manifest(chain(), 1, 1000, sigma_permille(40), 0, &specs);
    let (p1, _) = fx::finalized(&m1, bp(REFERENCE), None, &specs)?;
    assert_eq!(p1.mode, CutoffMode::Margin);
    let t1 = CalculationTargetV2::for_package(&p1, Some(launch.digest()?))?;
    let a1 = fx::signed(fx::authorization(&m1, &p1, &t1, 2, 36)?)?;
    RewardIntentAuthorization::check(&a1, &keys, &m1, &p1, &t1, &unactivated)?;
    LaunchAuthorization::check(&signed, &keys, &chain(), &unactivated)?.operation(
        None,
        &[],
        &unactivated,
        v,
        37,
    )?;
    // From the closing boundary every validator refuses it, and commits the
    // first cohort target under its package's authorization.
    let closure1 = fx::closure(&m1, &p1, Some(&t1), &specs)?;
    let checked1 = closure1.check(&m1, &p1, Some(&t1))?;
    let activated = ActiveTarget::activated(&checked1)?;
    assert!(!activated.is_launch_burn());
    assert_eq!(activated.digest(), t1.digest()?);
    assert_eq!(
        LaunchAuthorization::check(&signed, &keys, &chain(), &activated).unwrap_err(),
        Error::Invalid("launch authorization after activation")
    );
    let a1 = RewardIntentAuthorization::check(&a1, &keys, &m1, &p1, &t1, &activated)?;
    a1.operation(None, &[], &activated, v, closure1.closed_epoch)?;
    // Hint distance, chain, target and signer are fixed.
    let mut long = first.clone();
    long.renew_by_epoch += 1;
    assert_eq!(
        LaunchAuthorization::check(&fx::signed(long)?, &keys, &chain(), &unactivated).unwrap_err(),
        Error::Mismatch("authorization renew-by hint")
    );
    assert_eq!(
        LaunchAuthorization::check(&signed, &keys, &finney(), &ActiveTarget::launch(&finney())?)
            .unwrap_err(),
        Error::Mismatch("authorization chain")
    );
    let mut named = first.clone();
    named.target = t1.digest()?;
    assert_eq!(
        LaunchAuthorization::check(&fx::signed(named)?, &keys, &chain(), &unactivated).unwrap_err(),
        Error::Mismatch("authorized target")
    );
    assert_eq!(
        LaunchAuthorization::check(
            &sign(first, &crate::fixtures::package_key())?,
            &keys,
            &chain(),
            &unactivated
        )
        .unwrap_err(),
        Error::UntrustedKey
    );
    Ok(())
}

#[test]
fn published_artifacts_open_their_on_chain_commitments() -> Result<()> {
    let c = computed()?;
    let checked = c.closure.check(&c.manifest, &c.package, Some(&c.target))?;
    for (i, spec) in c.specs.iter().enumerate() {
        let payload = fx::opening(&chain(), c.manifest.cohort, spec).chain_payload();
        let artifact = VerifiedArtifact::from_bytes(&fx::artifact_bytes(spec))?.verified_header();
        assert_eq!(artifact.content, c.package.entrants[i].content);
        assert_eq!(artifact.sha256, c.package.entrants[i].artifact);
        let testnet = chain();
        let finalized = fx::finalized_commitment(&testnet, spec, fx::commitment_block(i), &payload);
        checked.check_entrant(i, &finalized, &artifact)?;
        // Admission returned exactly the manifest's roster entry.
        let entry = submission::SubmissionUpload::new(
            fx::opening(&chain(), 3, spec),
            &fx::header_bytes(&fx::artifact_bytes(spec)),
        )?
        .admit_header(
            3,
            &submission::FinalizedCommitment {
                chain: &chain(),
                committer: spec.identity(),
                block: fx::commitment_block(i),
                extrinsic_index: u32::from(spec.tag),
                payload: &payload,
            },
            &artifact,
        )?;
        assert_eq!(entry, c.manifest.roster[i]);
    }
    // Identical content: a copy publishes a distinct object with the same
    // derived content, so the cluster rule rests on checked content.
    let alpha = VerifiedArtifact::from_bytes(&fx::artifact_bytes(&c.specs[0]))?.verified_header();
    let copy = VerifiedArtifact::from_bytes(&fx::artifact_bytes(&c.specs[5]))?.verified_header();
    assert_ne!(alpha.sha256, copy.sha256);
    assert_eq!(alpha.content, copy.content);
    // A package asserting other content for the published object fails.
    let mut asserted = c.package.clone();
    asserted.entrants[0].content = fx::content(0x12);
    assert_eq!(
        submission::check_published_artifact(&asserted, &hotkey(0x11), &alpha).unwrap_err(),
        Error::Mismatch("published artifact content")
    );
    Ok(())
}

#[test]
fn artifact_reader_accepts_any_chunking() -> Result<()> {
    let object = fx::artifact_object(0x11, 0x11);
    let whole = VerifiedArtifact::from_bytes(&object)?;
    assert_eq!(whole.sha256, object_sha256(&object));
    assert_eq!(whole.len, object.len() as u64);
    for size in 1..=object.len() {
        let mut reader = ArtifactReader::new();
        for chunk in object.chunks(size) {
            reader.update(chunk)?;
        }
        assert_eq!(reader.finish()?, whole, "chunk size {size}");
    }
    // An error poisons the reader.
    let mut reader = ArtifactReader::new();
    let mut bad = object.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert_eq!(
        reader.update(&bad).unwrap_err(),
        Error::Mismatch("artifact file")
    );
    assert!(reader.update(&[]).is_err());
    assert!(reader.finish().is_err());
    Ok(())
}

#[test]
fn artifact_reader_refuses_an_empty_header_length_and_reports_its_header() -> Result<()> {
    // A zero header length is refused once the prefix is complete, in any
    // chunking (an earlier reader looped without consuming input).
    for split in 0..=4 {
        let object = [0u8, 0, 0, 0, 1, 2, 3];
        let mut reader = ArtifactReader::new();
        let (a, b) = object.split_at(split);
        let r = reader.update(a).and_then(|()| reader.update(b));
        assert_eq!(
            r.unwrap_err(),
            Error::Invalid("artifact header length"),
            "split {split}"
        );
    }
    // The header and the offset of its first file, once read.
    let object = fx::artifact_object(0x11, 0x11);
    let whole = VerifiedArtifact::from_bytes(&object)?;
    let len = u32::from_le_bytes(object[..4].try_into().unwrap()) as u64;
    let mut reader = ArtifactReader::new();
    reader.update(&object[..4 + len as usize - 1])?;
    assert!(reader.header().is_none());
    reader.update(&object[4 + len as usize - 1..4 + len as usize])?;
    let (h, at) = reader.header().unwrap();
    assert_eq!((h, at), (&whole.header, 4 + len));
    Ok(())
}

#[test]
fn reference_failure_holds_the_package() {
    let (current, _) = fx::reference_predictions();
    let mut graded: Vec<_> = current
        .iter()
        .enumerate()
        .map(|(doc, &involvement)| reward_v1::Graded {
            involvement,
            label: fx::label(doc),
        })
        .collect();
    assert_eq!(reward_v1::reference_score(&graded).unwrap(), bp(9100));
    graded[0].involvement = None;
    assert_eq!(
        reward_v1::reference_score(&graded).unwrap_err(),
        Error::Invalid("reference failure holds the package")
    );
    // An entrant's failure scores zero utility and keeps its weight.
    assert_eq!(
        reward_v1::document_score(&graded).unwrap(),
        Score::new(bp(9100).get() * 19 / 20).unwrap()
    );
}

#[test]
fn operator_may_replace_only_r0() -> Result<()> {
    let candidate = fx::reference_model(0xee);
    let replaced = fx::references(0).replace_r0(candidate)?;
    assert_eq!((replaced.index, replaced.predecessor), (0, None));
    assert_eq!(
        fx::references(1).replace_r0(candidate).unwrap_err(),
        Error::Invalid("only R_0 can be replaced")
    );
    let c = computed()?;
    let next = reward_v1::next_references(&c.manifest.references, &c.package, &c.index)?;
    assert_eq!(next.index, 2);
    assert_eq!(next.current.content, fx::content(0x11));
    assert_eq!(next.predecessor, Some(c.manifest.references.current));
    Ok(())
}

#[test]
fn package_binds_its_full_receipt_index() -> Result<()> {
    let c = computed()?;
    let mut package = c.package.clone();
    let entries: Vec<ReceiptIndexEntry> = (0..package.receipt_count)
        .map(|i| ReceiptIndexEntry {
            attempt: domain_hash("slop_ninja.fixture-attempt", &i.to_le_bytes()),
            outcome: OutcomeClass::Prediction,
            object_sha256: Digest([7; 32]),
            object_len: 100,
        })
        .collect();
    let mut index = ReceiptIndex {
        cohort_manifest: c.manifest.digest()?,
        entries,
    };
    index.entries.sort_by_key(|e| e.attempt);
    package.receipt_root = index.digest()?;
    package.check_receipts(&index)?;
    index.entries.pop();
    assert_eq!(
        package.check_receipts(&index).unwrap_err(),
        Error::Mismatch("receipt root or count")
    );
    Ok(())
}

#[test]
fn burned_units_follow_the_finalized_owner_hotkey() -> Result<()> {
    let c = computed()?;
    let result = result_of(&c.package)?;
    let scored = Some(&result);
    let before = reward_v1::derive_row(&c.target, scored, &computed_snapshot(&c.specs))?;
    let rotated = fx::rotated_owner_snapshot(&c.specs);
    let after = reward_v1::derive_row(&c.target, scored, &rotated)?;
    assert_eq!(
        fx::row_entries(&before),
        "2:24970,18:3316,21:8457,22:8492,23:3316,27:8492,33:8492"
    );
    // Same target and package: only the burned units moved, to the UID of
    // the rotated owner hotkey, and the row records that hotkey.
    assert_eq!(
        fx::row_entries(&after),
        "18:3316,21:8457,22:8492,23:3316,27:8492,33:8492,48:24970"
    );
    assert_eq!(before.owner_hotkey, fx::owner_hotkey());
    assert_eq!(after.owner_hotkey, hotkey(0x77));
    assert_eq!(before.calculation_target, after.calculation_target);
    let launch = CalculationTargetV2::launch_burn(chain());
    assert_eq!(
        fx::row_entries(&reward_v1::derive_row(&launch, None, &rotated)?),
        "48:65535"
    );
    // A target carried over a rotation stays authorized: no target, package
    // or manifest names the owner hotkey.
    let specs = [s(0x91, 9050)];
    let m4 = fx::manifest(chain(), 4, 1000, sigma_permille(40), 2, &specs);
    let (p4, _) = fx::finalized(&m4, bp(9600), Some(bp(9100)), &specs)?;
    let carried = fx::signed(fx::authorization(
        &m4,
        &p4,
        &c.target,
        3,
        m4.opened_epoch + 16,
    )?)?;
    RewardIntentAuthorization::check(
        &carried,
        &crate::fixtures::trusted_keys(),
        &m4,
        &p4,
        &c.target,
        &c.active_after()?,
    )?;
    // Rotated to a hotkey with no UID: a row that must burn is deferred,
    // never sent elsewhere.
    let mut unregistered = computed_snapshot(&c.specs);
    unregistered.owner_hotkey = hotkey(0x77);
    let deferred = Error::Deferred("owner hotkey not registered");
    assert_eq!(
        reward_v1::derive_row(&c.target, scored, &unregistered).unwrap_err(),
        deferred
    );
    assert_eq!(
        reward_v1::derive_row(&launch, None, &unregistered).unwrap_err(),
        deferred
    );
    assert_eq!(deferred.code(), "deferred");
    // A row with nothing to burn needs no owner UID.
    let specs = [s(0x21, 9600), s(0x22, 9400)];
    let p = example(bp(REFERENCE), None, &specs)?;
    let t = CalculationTargetV2::for_package(&p, None)?;
    let mut snap = fx::snapshot(chain(), 9, &specs);
    snap.owner_hotkey = hotkey(0x77);
    let row = reward_v1::derive_row(&t, Some(&result_of(&p)?), &snap)?;
    assert_eq!(fx::row_entries(&row), "33:35424,34:30111");
    assert_eq!(row.owner_hotkey, hotkey(0x77));
    Ok(())
}

#[test]
fn admission_binds_the_committing_account_cohort_and_chain() -> Result<()> {
    let c = computed()?;
    let spec = &c.specs[0];
    let testnet = chain();
    let opening = fx::opening(&testnet, 3, spec);
    let bytes = fx::artifact_bytes(spec);
    let artifact = VerifiedArtifact::from_bytes(&bytes)?.verified_header();
    let upload = submission::SubmissionUpload::new(opening.clone(), &fx::header_bytes(&bytes))?;
    let payload = opening.chain_payload();
    let finalized = submission::FinalizedCommitment {
        chain: &testnet,
        committer: spec.identity(),
        block: fx::commitment_block(0),
        extrinsic_index: u32::from(spec.tag),
        payload: &payload,
    };
    assert_eq!(
        upload.admit_header(3, &finalized, &artifact)?,
        c.manifest.roster[0]
    );
    let admit = |f: &submission::FinalizedCommitment<'_>, cohort| {
        upload.admit_header(cohort, f, &artifact).unwrap_err()
    };
    let mut other = spec.identity();
    other.hotkey = hotkey(0x12);
    assert_eq!(
        admit(
            &submission::FinalizedCommitment {
                committer: other,
                ..finalized
            },
            3
        ),
        Error::Mismatch("committing account")
    );
    let mut regenerated = spec.identity();
    regenerated.registration_block = 7777;
    assert_eq!(
        admit(
            &submission::FinalizedCommitment {
                committer: regenerated,
                ..finalized
            },
            3
        ),
        Error::Mismatch("committing account")
    );
    assert_eq!(
        admit(
            &submission::FinalizedCommitment {
                block: spec.identity().registration_block - 1,
                ..finalized
            },
            3
        ),
        Error::Invalid("registration after commitment")
    );
    // A commitment queued for a later cohort is not admitted into this one.
    assert_eq!(admit(&finalized, 4), Error::Mismatch("commitment cohort"));
    // An opening naming another chain would never reopen under the package's
    // chain at closure, so admission refuses it.
    let mainnet = finney();
    assert_eq!(
        admit(
            &submission::FinalizedCommitment {
                chain: &mainnet,
                ..finalized
            },
            3
        ),
        Error::Mismatch("commitment chain")
    );
    // The UID and finalized position come from the chain read, not the
    // opening: a moved UID is admitted at its UID at the commitment block.
    let mut moved = spec.identity();
    moved.uid = 0x21;
    let entry = upload.admit_header(
        3,
        &submission::FinalizedCommitment {
            committer: moved,
            block: 5000,
            extrinsic_index: 9,
            ..finalized
        },
        &artifact,
    )?;
    assert_eq!((entry.identity, entry.position()), (moved, (5000, 9)));
    Ok(())
}

#[test]
fn closure_enforces_the_phase_minimums() -> Result<()> {
    let c = computed()?;
    let opened = c.manifest.opened_epoch;
    let check = |admission, published, closed| {
        let mut closure = c.closure.clone();
        closure.admission_closed_epoch = admission;
        closure.published_epoch = published;
        closure.closed_epoch = closed;
        closure
            .check(&c.manifest, &c.package, Some(&c.target))
            .map(|_| ())
    };
    // The nominal schedule: admission ends at tempo 1, publication at 16,
    // the close at K = 20.
    assert_eq!(
        (
            c.closure.admission_closed_epoch,
            c.closure.published_epoch,
            c.closure.closed_epoch
        ),
        (opened + 1, opened + 16, opened + 20)
    );
    check(opened + 1, opened + 16, opened + 20)?;
    assert_eq!(
        check(opened, opened + 16, opened + 20).unwrap_err(),
        Error::Invalid("admission closed before its minimum")
    );
    assert_eq!(
        check(opened + 5, opened + 5, opened + 20).unwrap_err(),
        Error::Invalid("published before admission closed")
    );
    // The transition lasts at least two tempos.
    check(opened + 1, opened + 18, opened + 20)?;
    assert_eq!(
        check(opened + 1, opened + 19, opened + 20).unwrap_err(),
        Error::Invalid("closed before the transition minimum")
    );
    // A late publication moves the close back by whole tempos.
    check(opened + 1, opened + 19, opened + 21)?;
    check(opened + 3, opened + 30, opened + 32)?;
    // The cohort still cannot close before K.
    assert_eq!(
        check(opened + 1, opened + 16, opened + 19).unwrap_err(),
        Error::Invalid("closed before the cohort minimum")
    );
    Ok(())
}

#[test]
fn only_a_checked_closure_activates_its_target() -> Result<()> {
    let c = computed()?;
    let v = AccountId([0x66; 32]);
    let opened = c.manifest.opened_epoch;
    // A closure that closed two tempos after opening fails the frozen
    // minimums, so no checked closure exists to activate its target at the
    // authorization's issue epoch, where an unchecked one would have.
    let mut early = c.closure.clone();
    early.closed_epoch = opened + 2;
    let authorized = c.checked_authorization()?;
    let activation = |closure: &submission::CohortClosure| {
        closure
            .check(&c.manifest, &c.package, Some(&c.target))
            .and_then(|k| ActiveTarget::activated(&k))
            .and_then(|a| authorized.operation(None, &[], &a, v, c.authorization.record.issued_epoch))
    };
    assert_eq!(
        activation(&early).unwrap_err(),
        Error::Invalid("closed before the transition minimum")
    );
    let mut short = c.closure.clone();
    short.published_epoch = opened + 2;
    short.closed_epoch = opened + 4;
    assert_eq!(
        activation(&short).unwrap_err(),
        Error::Invalid("closed before the cohort minimum")
    );
    // The checked closure activates at its closing epoch, never before.
    let checked = c.closure.check(&c.manifest, &c.package, Some(&c.target))?;
    assert_eq!(checked.closure(), &c.closure);
    assert_eq!(checked.package(), &c.package);
    let active = ActiveTarget::activated(&checked)?;
    assert_eq!(active.activated_epoch(), Some(c.closure.closed_epoch));
    assert_eq!(
        authorized
            .operation(None, &[], &active, v, c.closure.closed_epoch)?
            .id(),
        WeightOperationV2::for_target(&c.target, v, c.closure.closed_epoch)?.id()
    );
    assert_eq!(
        authorized
            .operation(None, &[], &active, v, c.closure.closed_epoch - 1)
            .unwrap_err(),
        Error::Invalid("epoch before target activation")
    );
    Ok(())
}

#[test]
fn only_a_checked_authorization_yields_an_operation() -> Result<()> {
    let c = computed()?;
    let keys = crate::fixtures::trusted_keys();
    let v = AccountId([0x66; 32]);
    let launch = CalculationTargetV2::launch_burn(chain());
    let epoch = c.closure.closed_epoch;
    let lease = u64::from(c.manifest.schedule.lease_tempos);
    let active = c.active_after()?;
    let unactivated = c.active_before()?;
    // An unchecked authorization has no `operation`, and only `check` builds
    // a `CheckedAuthorization`; the doctests on `CheckedAuthorization` show
    // that neither compiles otherwise. Each authorization below meets every
    // condition the operation itself applies at its epoch (the target and its
    // closure and the issue epoch), but fails its check, so it yields no
    // operation.
    let base = c.authorization.record.clone();
    // A renew-by hint one tempo past the frozen `lease_tempos` distance.
    let mut long = base.clone();
    long.renew_by_epoch += 1;
    // Issued before the cohort opened.
    let mut early = base.clone();
    early.issued_epoch = c.manifest.opened_epoch - 1;
    early.renew_by_epoch = early.issued_epoch + lease;
    // The launch burn row under a package that produced a target.
    let burn = fx::authorization(&c.manifest, &c.package, &launch, 1, epoch - 4)?;
    // The package's own target, but signed under the package key.
    let cases = [
        (
            fx::signed(long.clone())?,
            &c.target,
            long.renew_by_epoch,
            &active,
            Error::Mismatch("authorization renew-by hint"),
        ),
        (
            fx::signed(early.clone())?,
            &c.target,
            epoch,
            &active,
            Error::Invalid("authorization before its cohort"),
        ),
        (
            fx::signed(burn.clone())?,
            &launch,
            epoch,
            &unactivated,
            Error::Mismatch("calculation target package"),
        ),
        (
            sign(base.clone(), &crate::fixtures::package_key())?,
            &c.target,
            epoch,
            &active,
            Error::UntrustedKey,
        ),
    ];
    for (authorization, target, at, active, refusal) in cases {
        let record = &authorization.record;
        assert_eq!(record.target, target.digest()?);
        assert!(record.issued_epoch <= at);
        assert!(active.activated_epoch().is_none_or(|a| at >= a));
        assert_eq!(
            RewardIntentAuthorization::check(
                &authorization,
                &keys,
                &c.manifest,
                &c.package,
                target,
                active
            )
            .and_then(|a| a.operation(None, &[], active, v, at))
            .unwrap_err(),
            refusal
        );
    }
    // A new target whose predecessor is not the active target is refused.
    let orphan = CalculationTargetV2::for_package(&c.package, Some(Digest([9; 32])))?;
    let named = fx::signed(fx::authorization(
        &c.manifest,
        &c.package,
        &orphan,
        1,
        epoch - 4,
    )?)?;
    assert_eq!(
        RewardIntentAuthorization::check(
            &named,
            &keys,
            &c.manifest,
            &c.package,
            &orphan,
            &unactivated
        )
        .unwrap_err(),
        Error::Mismatch("target predecessor not active")
    );
    // The package's own target is accepted while its predecessor is active,
    // as well as once it is itself active (`checked_authorization`).
    RewardIntentAuthorization::check(
        &c.authorization,
        &keys,
        &c.manifest,
        &c.package,
        &c.target,
        &unactivated,
    )?;
    // A renewal cannot carry the lease over to another target: the
    // authorization naming the launch burn row is refused at its check, so no
    // renewal is checked against it and the closure never activates it.
    let burn = fx::signed(burn)?;
    assert_eq!(
        RewardIntentAuthorization::check(
            &burn,
            &keys,
            &c.manifest,
            &c.package,
            &launch,
            &unactivated
        )
        .unwrap_err(),
        Error::Mismatch("calculation target package")
    );
    // The checked authorization yields exactly the per-tempo identity.
    let authorized = c.checked_authorization()?;
    let renewal = c.checked_renewal(&authorized)?;
    assert_eq!(
        authorized.operation(Some(&renewal), &[], &active, v, epoch)?,
        WeightOperationV2::for_target(&c.target, v, epoch)?
    );
    Ok(())
}

#[test]
fn validators_check_each_entrant_against_its_finalized_commitment() -> Result<()> {
    let c = computed()?;
    let checked = c.closure.check(&c.manifest, &c.package, Some(&c.target))?;
    let spec = &c.specs[0];
    let testnet = chain();
    let payload = fx::opening(&testnet, 3, spec).chain_payload();
    let artifact = VerifiedArtifact::from_bytes(&fx::artifact_bytes(spec))?.verified_header();
    let finalized = fx::finalized_commitment(&testnet, spec, fx::commitment_block(0), &payload);
    checked.check_entrant(0, &finalized, &artifact)?;
    let check = |f: submission::FinalizedCommitment<'_>| {
        checked.check_entrant(0, &f, &artifact).unwrap_err()
    };
    // The position breaks rank-order ties, so a misstated block or
    // extrinsic index is caught.
    assert_eq!(
        check(submission::FinalizedCommitment {
            block: finalized.block + 1,
            ..finalized
        }),
        Error::Mismatch("commitment position")
    );
    assert_eq!(
        check(submission::FinalizedCommitment {
            extrinsic_index: finalized.extrinsic_index - 1,
            ..finalized
        }),
        Error::Mismatch("commitment position")
    );
    // The committer is the entrant's registration, UID included.
    let mut other = spec.identity();
    other.hotkey = hotkey(0x12);
    assert_eq!(
        check(submission::FinalizedCommitment {
            committer: other,
            ..finalized
        }),
        Error::Mismatch("committing account")
    );
    let mut regenerated = spec.identity();
    regenerated.registration_block += 1;
    assert_eq!(
        check(submission::FinalizedCommitment {
            committer: regenerated,
            ..finalized
        }),
        Error::Mismatch("committing account")
    );
    let mut moved = spec.identity();
    moved.uid = 0x21;
    assert_eq!(
        check(submission::FinalizedCommitment {
            committer: moved,
            ..finalized
        }),
        Error::Mismatch("committing uid")
    );
    let mainnet = finney();
    assert_eq!(
        check(submission::FinalizedCommitment {
            chain: &mainnet,
            ..finalized
        }),
        Error::Mismatch("commitment chain")
    );
    let other_payload = fx::opening(&testnet, 3, &c.specs[1]).chain_payload();
    assert_eq!(
        check(submission::FinalizedCommitment {
            payload: &other_payload,
            ..finalized
        }),
        Error::Mismatch("submission commitment")
    );
    // Another entrant's commitment is not this entrant's.
    let second = fx::finalized_commitment(
        &testnet,
        &c.specs[1],
        fx::commitment_block(1),
        &other_payload,
    );
    assert_eq!(
        checked.check_entrant(0, &second, &artifact).unwrap_err(),
        Error::Mismatch("committing account")
    );
    Ok(())
}

#[test]
fn package_publishes_each_retained_clusters_exact_credit() -> Result<()> {
    let c = computed()?;
    let listed: Vec<(u8, String)> = c
        .package
        .credits
        .iter()
        .map(|k| (k.head.0[0], k.fraction().to_string()))
        .collect();
    // gamma's cluster (0x15) shares beta's band: the band's 289/1029 splits
    // 1 : 0.85 between them, so the top bands' credits are unchanged.
    assert_eq!(
        listed,
        vec![
            (0x11, "400/1029".to_string()),
            (0x13, "340/1029".to_string()),
            (0x12, "5780/38073".to_string()),
            (0x15, "4913/38073".to_string()),
        ]
    );
    let sum = c
        .package
        .credits
        .iter()
        .map(ClusterCredit::fraction)
        .reduce(|a, b| a.add(&b))
        .unwrap();
    assert_eq!(sum, Fraction::new(BigUint::one(), BigUint::one()).unwrap());
    // Credits come out of band_credits in lowest terms.
    for sizes in [
        vec![1],
        vec![2],
        vec![3, 1],
        vec![1, 4, 2],
        vec![7],
        vec![5; 6],
    ] {
        for row in reward_v1::band_credits(&sizes, DECAY_BPS)? {
            for credit in row {
                assert_eq!(credit.num.gcd(&credit.den), BigUint::one(), "{sizes:?}");
            }
        }
    }
    // The coordinator's package check compares each credit exactly,
    // including its representation, before it derives the row.
    let derive = |p: &ScorePackageV2| {
        reward_v1::check_outcome(p, &c.index)?;
        let t = CalculationTargetV2::for_package(p, c.target.predecessor)?;
        reward_v1::derive_row(&t, Some(&result_of(p)?), &computed_snapshot(&c.specs))
    };
    let mut tampered = c.package.clone();
    tampered.credits[2].num = tampered.credits[2].num.add(&BigUint::one());
    assert_eq!(
        derive(&tampered).unwrap_err(),
        Error::Mismatch("cluster credits")
    );
    let mut scaled = c.package.clone();
    scaled.credits[0].num = scaled.credits[0].num.mul_u64(3);
    scaled.credits[0].den = scaled.credits[0].den.mul_u64(3);
    assert_eq!(
        scaled.credits[0].fraction(),
        c.package.credits[0].fraction()
    );
    assert_eq!(
        derive(&scaled).unwrap_err(),
        Error::Mismatch("cluster credits")
    );
    // Big integers have one encoding and a bounded size.
    let mut e = Encoder::new();
    vec![5u32, 0].encode_to(&mut e);
    assert_eq!(
        BigUint::decode_from(&mut Decoder::new(&e.into_bytes())).unwrap_err(),
        Error::NonCanonical
    );
    let mut e = Encoder::new();
    vec![1u32; crate::bigint::MAX_LIMBS + 1].encode_to(&mut e);
    assert!(matches!(
        BigUint::decode_from(&mut Decoder::new(&e.into_bytes())).unwrap_err(),
        Error::LengthLimit { .. }
    ));
    let mut huge = c.package.clone();
    huge.credits[0].den = BigUint::pow_u64(2, 32 * crate::bigint::MAX_LIMBS as u32);
    assert!(matches!(
        huge.to_bytes().unwrap_err(),
        Error::LengthLimit { .. }
    ));
    Ok(())
}

// ---------------------------------------------------------------------------
// Copy policy B + D
// ---------------------------------------------------------------------------

#[test]
fn computed_cohort_applies_the_public_copy_gate() -> Result<()> {
    let c = computed()?;
    let by_name = |name: &str| {
        let i = c.models.iter().position(|m| m.name == name).expect("model");
        &c.package.entrants[i]
    };
    // An exact copy of R_k (renamed files, another publisher) is found by its
    // content digest at admission and never executed.
    let exact = by_name("rk_exact_copy");
    assert_eq!(exact.public_copy(), Some(PublicCopy::Content));
    assert_eq!((exact.score, exact.public.is_some()), (None, false));
    assert!(c.index.contains(&exact.content));
    // A recalibrated copy of R_k, 0.035 from it in raw RMS, is within d after
    // the best rank-preserving fit and does not beat R_k.
    let recalibrated = by_name("rk_recalibrated");
    let public = recalibrated.public.as_ref().unwrap();
    assert_eq!(recalibrated.public_copy(), Some(PublicCopy::Current));
    assert!(public.current.within(NEAR_COPY_PPB)?);
    assert!(public.current.distance_ppb().unwrap() > 0);
    assert!(recalibrated.score.unwrap() < c.package.reference.score);
    // An independent retrain of R_k's recipe at R_k's own score stays apart
    // and passes the gate.
    let retrain = by_name("rk_retrain");
    assert_eq!(retrain.score, Some(c.package.reference.score));
    assert!(retrain.gated());
    assert!(
        retrain
            .public
            .as_ref()
            .unwrap()
            .current
            .distance_ppb()
            .unwrap()
            > 2 * NEAR_COPY_PPB
    );
    // A sharpened copy of R_k-1 is caught by R_k-1, not R_k.
    let sharpened = by_name("rk1_sharpened");
    let public = sharpened.public.as_ref().unwrap();
    assert_eq!(sharpened.public_copy(), Some(PublicCopy::Predecessor));
    assert!(!public.current.within(NEAR_COPY_PPB)?);
    assert_eq!(public.predecessor.unwrap().min_rss(), 0);
    // A copy of an older published artifact is caught on the probe only.
    let probe_copy = by_name("omega_probe_copy");
    let public = probe_copy.public.as_ref().unwrap();
    assert_eq!(probe_copy.public_copy(), Some(PublicCopy::Probe));
    let found = public.probe_match.as_ref().unwrap();
    assert_eq!(
        (found.content, found.version),
        (fx::content(fx::ARCHIVED), fx::PROBE_VERSION)
    );
    assert_eq!(found.stats.min_rss(), 0);
    assert_eq!(found.entrant_probe_score, found.public_probe_score);
    assert!(!public.current.within(NEAR_COPY_PPB)?);
    // Entrants above R_k by more than w / 2 pass whatever their distance.
    assert!(
        by_name("delta")
            .public
            .as_ref()
            .unwrap()
            .current
            .within(NEAR_COPY_PPB)?
    );
    assert!(by_name("delta").public_copy().is_none());
    // Every scored entrant is tested, whatever its score: theta and kappa
    // score below S_Rk-1 and carry statistics; only the unexecuted carry none.
    let predecessor = c.package.predecessor.as_ref().unwrap().score;
    for name in ["theta", "kappa"] {
        let e = by_name(name);
        assert!(e.score.unwrap() < predecessor, "{name}");
        assert!(e.public.is_some() && e.gated(), "{name}");
    }
    for (e, m) in c.package.entrants.iter().zip(&c.models) {
        assert_eq!(e.public.is_some(), e.score.is_some(), "{}", m.name);
    }
    // Copies cannot set max_N, trigger the fallback or be promoted: the
    // cohort's outcome is that of the honest entrants alone.
    for name in [
        "rk_exact_copy",
        "rk_recalibrated",
        "rk1_sharpened",
        "omega_probe_copy",
    ] {
        let e = by_name(name);
        assert!(!e.gated(), "{name}");
        assert_eq!((e.cluster_head, e.units), (None, 0), "{name}");
    }
    assert_eq!(c.package.promoted, Some(hotkey(0x11)));
    // The near-duplicate alpha_close joins alpha by distance and carries its
    // statistics; identical content joins without any.
    let close = by_name("alpha_close");
    assert_eq!(close.cluster_head, Some(hotkey(0x11)));
    assert!(close.cluster_stats.unwrap().within(NEAR_COPY_PPB)?);
    assert_eq!(by_name("alpha_copy").cluster_stats, None);
    // The monitor holds the smallest distances, at least d between heads.
    let monitor = c.package.distance_monitor;
    assert!(monitor.min_entrant_pair_ppb.unwrap() >= NEAR_COPY_PPB);
    assert!(monitor.min_gated_vs_current_ppb.is_some());
    reward_v1::check_outcome(&c.package, &c.index)?;
    Ok(())
}

#[test]
fn independent_heads_stay_apart_and_ties_keep_their_clusters() -> Result<()> {
    let c = computed()?;
    let find = |n: &str| {
        c.models
            .iter()
            .find(|m| m.name == n)
            .and_then(|m| m.predictions.clone())
            .unwrap()
    };
    // Independent textures keep every pair of heads more than 1.5 d apart;
    // gamma ties beta's score with its own ranking.
    for (a, b) in [
        ("alpha", "iota"),
        ("alpha", "beta"),
        ("alpha", "gamma"),
        ("iota", "beta"),
        ("iota", "gamma"),
        ("beta", "gamma"),
    ] {
        let d = reward_v1::near_stats(&find(b), &find(a))?
            .distance_ppb()
            .unwrap();
        assert!(d > NEAR_COPY_PPB / 2 * 3, "{a} {b} {d}");
    }
    let (current, previous) = fx::reference_predictions();
    let refs = reward_v1::near_stats(&previous, &current)?;
    assert!(refs.distance_ppb().unwrap() > 2 * NEAR_COPY_PPB);
    Ok(())
}

#[test]
fn exact_copies_of_published_artifacts_are_refused_both_ways() -> Result<()> {
    let c = computed()?;
    let i = c
        .models
        .iter()
        .position(|m| m.name == "rk_exact_copy")
        .unwrap();
    // An executed public copy is not a valid record.
    let mut executed = c.package.clone();
    executed.entrants[i].score = Some(bp(9100));
    (
        executed.entrants[i].assigned,
        executed.entrants[i].succeeded,
    ) = (CASES, CASES);
    assert_eq!(
        executed.to_bytes().unwrap_err(),
        Error::Invalid("published content executed")
    );
    // The coordinator's finalization refuses to have executed it.
    let mut p = c.package.clone();
    p.entrants[i].score = Some(bp(9100));
    assert_eq!(
        finalize_fixture(&mut p, &c.index, &mut fx::ComputedMeasure::new(&c.models))
            .unwrap_err(),
        Error::Invalid("published content executed")
    );
    // Published content without the failure, and the failure without
    // published content, are both refused.
    let alpha = fx::content(0x11);
    let grown = PublishedIndex::new(
        c.index.contents.contents.iter().copied().chain([alpha]),
        c.index.fingerprints.entries.iter().copied(),
    )?;
    let mut p = c.package.clone();
    p.published = grown.binding()?;
    assert_eq!(
        reward_v1::check_outcome(&p, &grown).unwrap_err(),
        Error::Mismatch("public content gate")
    );
    let mut p = c.package.clone();
    p.entrants[i].content = fx::content(0x99);
    assert_eq!(
        reward_v1::check_outcome(&p, &c.index).unwrap_err(),
        Error::Mismatch("public content gate")
    );
    // The index is bound to the package and holds both references.
    assert_eq!(
        reward_v1::check_outcome(&c.package, &PublishedIndex::default()).unwrap_err(),
        Error::Mismatch("published index")
    );
    Ok(())
}

#[test]
fn package_checks_refuse_a_misapplied_distance_clause_or_coverage() -> Result<()> {
    let c = computed()?;
    let at = |name: &str| c.models.iter().position(|m| m.name == name).unwrap();
    let outcome = |p: &ScorePackageV2| reward_v1::check_outcome(p, &c.index).unwrap_err();
    let set = |p: &mut ScorePackageV2, i: usize, copy: Option<PublicCopy>| {
        p.entrants[i]
            .gate_failures
            .retain(|g| g.gate != Gate::PublicCopy);
        p.entrants[i]
            .gate_failures
            .extend(copy.map(PublicCopy::failure));
    };
    let gate = Error::Mismatch("public near-copy gate");
    // d: a copy within d recorded as gated, an independent model beyond d
    // recorded as a copy.
    let mut p = c.package.clone();
    set(&mut p, at("rk_recalibrated"), None);
    assert_eq!(outcome(&p), gate);
    let mut p = c.package.clone();
    set(&mut p, at("rk_retrain"), Some(PublicCopy::Current));
    assert_eq!(outcome(&p), gate);
    // w / 2: delta is a rank-preserving map of R_k but beats it by more than
    // half a band, so it is not a copy.
    let mut p = c.package.clone();
    set(&mut p, at("delta"), Some(PublicCopy::Current));
    assert_eq!(outcome(&p), gate);
    // Precedence: R_k, then R_k-1, then the probe.
    let mut p = c.package.clone();
    set(&mut p, at("rk_recalibrated"), Some(PublicCopy::Predecessor));
    assert_eq!(outcome(&p), gate);
    // Coverage: statistics for every scored entrant, whatever its score.
    for name in ["rk_retrain", "theta", "kappa"] {
        let mut p = c.package.clone();
        p.entrants[at(name)].public = None;
        assert_eq!(
            outcome(&p),
            Error::Mismatch("public check coverage"),
            "{name}"
        );
    }
    let mut p = c.package.clone();
    p.entrants[at("eta")].public = c.package.entrants[at("alpha")].public.clone();
    assert_eq!(
        outcome(&p),
        Error::Invalid("public check of an unscored entrant")
    );
    // Probe matches must be near, fingerprinted and on a probe in use.
    let probe = at("omega_probe_copy");
    let tamper = |f: &dyn Fn(&mut ProbeMatch)| {
        let mut p = c.package.clone();
        f(p.entrants[probe]
            .public
            .as_mut()
            .unwrap()
            .probe_match
            .as_mut()
            .unwrap());
        outcome(&p)
    };
    assert_eq!(
        tamper(&|m| m.stats = fx::far(CASES)),
        Error::Mismatch("probe match not a near copy")
    );
    assert_eq!(
        tamper(&|m| m.content = fx::content(0x11)),
        Error::Mismatch("probe match not fingerprinted")
    );
    assert_eq!(
        tamper(&|m| m.version = 2),
        Error::Mismatch("probe version not in use")
    );
    // An entrant's own probe score is the same in all its comparisons on a
    // version; raised in all of them, it beats the archived artifact by more
    // than w / 2 and the match is no near copy.
    assert_eq!(
        tamper(&|m| m.entrant_probe_score = Score::ONE),
        Error::Mismatch("entrant probe score")
    );
    let mut p = c.package.clone();
    let public = p.entrants[probe].public.as_mut().unwrap();
    for m in public
        .reference_probes
        .iter_mut()
        .chain(public.probe_match.as_mut())
    {
        m.entrant_probe_score = Score::ONE;
    }
    assert_eq!(outcome(&p), Error::Mismatch("probe match not a near copy"));
    // Cluster statistics and the monitor.
    let mut p = c.package.clone();
    p.entrants[at("alpha_close")].cluster_stats = Some(fx::far(CASES));
    assert_eq!(
        outcome(&p),
        Error::Mismatch("cluster member not near its head")
    );
    let mut p = c.package.clone();
    p.distance_monitor.min_entrant_pair_ppb = Some(NEAR_COPY_PPB - 1);
    assert_eq!(outcome(&p), Error::Mismatch("distance monitor"));
    let mut p = c.package.clone();
    p.distance_monitor.min_gated_vs_current_ppb = None;
    assert_eq!(outcome(&p), Error::Mismatch("distance monitor"));
    let mut p = c.package.clone();
    p.entrants[at("alpha_close")].cluster_stats = None;
    assert_eq!(
        outcome(&p),
        Error::Mismatch("cluster member not near its head")
    );
    let mut p = c.package.clone();
    p.entrants[at("alpha_copy")].cluster_stats = Some(fx::aligned(CASES));
    assert_eq!(
        outcome(&p),
        Error::Mismatch("cluster member not near its head")
    );
    // Statistic sanity: batch statistics cover exactly the entrant's
    // successful documents, and no residual exceeds its bound.
    let mut p = c.package.clone();
    p.entrants[at("alpha")]
        .public
        .as_mut()
        .unwrap()
        .current
        .common -= 1;
    assert_eq!(outcome(&p), Error::Mismatch("near-copy common documents"));
    let mut p = c.package.clone();
    p.entrants[at("alpha")]
        .public
        .as_mut()
        .unwrap()
        .current
        .rss_rev = u128::from(CASES) * 10u128.pow(18) + 1;
    assert_eq!(outcome(&p), Error::OutOfRange("near-copy residual"));
    // A probe match goes with a probe failure, and both references are in
    // the index.
    let mut p = c.package.clone();
    set(&mut p, probe, None);
    assert_eq!(
        p.to_bytes().unwrap_err(),
        Error::Invalid("probe match without a probe failure")
    );
    let without_r0 = PublishedIndex::new(
        [fx::content(0xe1), fx::content(fx::ARCHIVED)],
        c.index
            .fingerprints
            .entries
            .iter()
            .copied()
            .filter(|f| f.content != fx::content(0xe0)),
    )?;
    let mut p = c.package.clone();
    p.published = without_r0.binding()?;
    assert_eq!(
        reward_v1::check_outcome(&p, &without_r0).unwrap_err(),
        Error::Mismatch("reference not published")
    );
    // Both references are fingerprinted on every probe version in use.
    let unfingerprinted = PublishedIndex::new(
        c.index.contents.contents.iter().copied(),
        c.index
            .fingerprints
            .entries
            .iter()
            .copied()
            .filter(|f| f.content != fx::content(0xe0)),
    )?;
    let mut p = c.package.clone();
    p.published = unfingerprinted.binding()?;
    assert_eq!(
        reward_v1::check_outcome(&p, &unfingerprinted).unwrap_err(),
        Error::Mismatch("reference not fingerprinted")
    );
    Ok(())
}

#[test]
fn published_index_grows_by_closed_cohorts() -> Result<()> {
    let c = computed()?;
    let next = c
        .index
        .after_closure(&c.package, fx::closure_fingerprints(&c))?;
    // Every entrant's content is published, the exact copy's once, and every
    // executed entrant is fingerprinted, the low-scoring theta and kappa and
    // the gate-failed zeta included; the unexecuted eta is not.
    for e in &c.package.entrants {
        assert!(next.contains(&e.content));
        assert_eq!(
            next.fingerprinted(&e.content, fx::PROBE_VERSION),
            e.score.is_some() || c.index.contains(&e.content)
        );
    }
    for tag in [0x12, 0x18, 0x1a, 0x23] {
        assert!(next.fingerprinted(&fx::content(tag), fx::PROBE_VERSION));
    }
    assert!(!next.fingerprinted(&fx::content(0x19), fx::PROBE_VERSION));
    // A closure that leaves an executed entrant out, fingerprints one that
    // was never executed, or names a probe version not in use is refused.
    let mut missing = fx::closure_fingerprints(&c);
    missing.retain(|f| f.hotkey != hotkey(0x23));
    let mut unexecuted = fx::closure_fingerprints(&c);
    unexecuted.push(EntrantFingerprint {
        hotkey: hotkey(0x19),
        version: fx::PROBE_VERSION,
        fingerprint: Digest([0x19; 32]),
    });
    let mut other_version = fx::closure_fingerprints(&c);
    for f in &mut other_version {
        f.version = 2;
    }
    let mut twice = fx::closure_fingerprints(&c);
    twice.push(EntrantFingerprint {
        fingerprint: Digest([0x77; 32]),
        ..twice[0]
    });
    for fingerprints in [missing, unexecuted, other_version, twice] {
        assert_eq!(
            c.index.after_closure(&c.package, fingerprints).unwrap_err(),
            Error::Mismatch("closure fingerprints")
        );
    }
    let binding = next.binding()?;
    assert_eq!(binding.content_count, next.contents.contents.len() as u64);
    assert!(binding.content_count > c.index.contents.contents.len() as u64);
    // Two different fingerprints of one content on one version are refused.
    let twice = PublishedIndex::new(
        [fx::content(0xe0)],
        [1u8, 2].map(|t| Fingerprint {
            content: fx::content(0xe0),
            version: fx::PROBE_VERSION,
            fingerprint: Digest([t; 32]),
        }),
    );
    assert_eq!(
        twice.unwrap_err(),
        Error::Duplicate("fingerprint content and version")
    );
    Ok(())
}

/// 2026-11-05, testnet cohort 19: a probe cut between a cohort's seal and its
/// close fingerprinted the current reference on a new probe version, so the
/// eval Worker's index after the closure (its global index) held one entry
/// more than the rebuild from the cohort's own at-seal index and closure.
/// That entry is admitted; any other difference is refused, naming the
/// record that differs.
#[test]
fn an_index_after_the_closure_admits_only_a_probe_cut_of_the_references() -> Result<()> {
    let c = computed()?;
    let at_seal = &c.index;
    let rebuilt = at_seal.after_closure(&c.package, fx::closure_fingerprints(&c))?;
    let refs = &c.manifest.references;
    let probes = &c.manifest.probes;
    let bound = probes.iter().map(|p| p.version).max().unwrap();
    let current = refs.current.content;
    assert!(at_seal.contains(&current));
    let with = |extra: Vec<Fingerprint>| -> Result<PublishedIndex> {
        PublishedIndex::new(
            rebuilt.contents.contents.iter().copied(),
            rebuilt.fingerprints.entries.iter().copied().chain(extra),
        )
    };
    let fp = |content: Digest, version: u32, tag: u8| Fingerprint {
        content,
        version,
        fingerprint: Digest([tag; 32]),
    };
    // The rebuild itself, and the rebuild plus the cut's reference entries
    // (the predecessor's too, when the cohort names one), on one or two
    // later versions.
    rebuilt
        .admits_after_closure(at_seal, refs, probes, &rebuilt)
        .unwrap();
    let mut cut = vec![fp(current, bound + 1, 0x91)];
    if let Some(p) = &refs.predecessor {
        cut.push(fp(p.content, bound + 1, 0x92));
    }
    cut.push(fp(current, bound + 7, 0x93));
    let stored = with(cut)?;
    assert!(stored != rebuilt);
    rebuilt
        .admits_after_closure(at_seal, refs, probes, &stored)
        .unwrap();
    let refused = |stored: &PublishedIndex| {
        rebuilt
            .admits_after_closure(at_seal, refs, probes, stored)
            .unwrap_err()
    };
    // An extra fingerprint of a published content that is not a reference.
    let other = c
        .package
        .entrants
        .iter()
        .map(|e| e.content)
        .find(|d| *d != current && refs.predecessor.as_ref().is_none_or(|p| p.content != *d))
        .unwrap();
    assert_eq!(
        refused(&with(vec![fp(other, bound + 1, 0x94)])?).0,
        AfterIndexPart::Fingerprints
    );
    // A reference fingerprint on a version below the cohort's newest bound
    // one (the rebuild already holds the reference on each bound version:
    // another there is a duplicate, refused as a changed entry below).
    assert!(at_seal.fingerprinted(&current, bound));
    assert!(with(vec![fp(current, bound, 0x95)]).is_err());
    assert_eq!(
        refused(&with(vec![fp(current, bound - 1, 0x96)])?).1,
        "a fingerprint the rebuild lacks is on a probe version the cohort binds or below"
    );
    // A rebuilt fingerprint changed, or missing.
    let mut changed = rebuilt.clone();
    changed.fingerprints.entries[0].fingerprint = Digest([0x97; 32]);
    changed.fingerprints.entries.sort_unstable();
    assert_eq!(
        refused(&changed).1,
        "a rebuilt fingerprint is missing or changed in the index after the closure"
    );
    let mut missing = rebuilt.clone();
    missing.fingerprints.entries.pop();
    assert_eq!(refused(&missing).0, AfterIndexPart::Fingerprints);
    // A reference the cohort names that was not published at its seal (an
    // entrant's content only the closure publishes).
    let new = c
        .package
        .entrants
        .iter()
        .map(|e| e.content)
        .find(|d| !at_seal.contains(d))
        .unwrap();
    let mut late_refs = *refs;
    late_refs.current.content = new;
    assert_eq!(
        rebuilt
            .admits_after_closure(at_seal, &late_refs, probes, &with(vec![fp(new, bound + 1, 0x98)])?)
            .unwrap_err()
            .1,
        "a fingerprint the rebuild lacks is of a reference not published at the seal"
    );
    // Contents that differ, even by a reference already published.
    let mut fewer = stored.clone();
    fewer.contents.contents.retain(|d| *d != other);
    fewer.fingerprints.entries.retain(|f| f.content != other);
    assert_eq!(refused(&fewer).0, AfterIndexPart::Contents);
    let more = PublishedIndex::new(
        rebuilt.contents.contents.iter().copied().chain([Digest([0x99; 32])]),
        rebuilt.fingerprints.entries.iter().copied(),
    )?;
    assert_eq!(refused(&more).0, AfterIndexPart::Contents);
    // A stored index whose fingerprints name an unpublished content.
    let mut dangling = rebuilt.clone();
    dangling.fingerprints.entries.push(fp(Digest([0xff; 32]), bound + 1, 0x9a));
    assert_eq!(refused(&dangling).0, AfterIndexPart::Fingerprints);
    Ok(())
}

#[test]
fn a_repackaged_predecessor_above_the_margin_earns_nothing() -> Result<()> {
    // R_k-1 outscores R_k by more than the promotion margin on this batch, so
    // a repackaged R_k-1 clears the margin cutoff at exactly S_Rk-1. It is
    // tested like every scored entrant and fails public.near.predecessor.
    for independent in [true, false] {
        let h = fx::predecessor_above_margin(independent)?;
        let p = &h.package;
        let predecessor = p.predecessor.as_ref().unwrap().score;
        let margin = Threshold::margin(p.reference.score, p.params.margin())?;
        let copy = &p.entrants[0];
        assert_eq!(copy.score, Some(predecessor));
        assert!(margin.admits(predecessor));
        assert_eq!(copy.public_copy(), Some(PublicCopy::Predecessor));
        let public = copy.public.as_ref().unwrap();
        assert!(!public.current.within(NEAR_COPY_PPB)?);
        assert_eq!(public.predecessor.unwrap().min_rss(), 0);
        assert_eq!((copy.cluster_head, copy.units), (None, 0));
        if independent {
            // The unrelated model between the cutoff and the copy is promoted.
            let other = &p.entrants[1];
            assert!(margin.admits(other.score.unwrap()));
            assert!(other.gated());
            let distance = other.public.as_ref().unwrap();
            assert!(!distance.current.within(NEAR_COPY_PPB)?);
            assert!(!distance.predecessor.unwrap().within(NEAR_COPY_PPB)?);
            assert_eq!(p.mode, CutoffMode::Margin);
            assert_eq!(p.promoted, Some(hotkey(0x42)));
            assert_eq!(other.units, u16::MAX);
        } else {
            assert_eq!((p.mode, p.promoted), (CutoffMode::NoTarget, None));
        }
        reward_v1::check_outcome(p, &h.index)?;
        // The package cannot omit the copy's statistics, and the recorded
        // gate cannot be dropped.
        let mut q = p.clone();
        q.entrants[0].public = None;
        assert!(reward_v1::check_outcome(&q, &h.index).is_err());
        let mut q = p.clone();
        q.entrants[0].gate_failures.clear();
        assert_eq!(
            reward_v1::check_outcome(&q, &h.index).unwrap_err(),
            Error::Mismatch("public near-copy gate")
        );
    }
    Ok(())
}

#[test]
fn a_later_copy_of_a_low_scorer_is_caught_on_the_probe() -> Result<()> {
    // kappa scored below S_Rk-1 in the computed cohort and was still
    // fingerprinted. Its repackaged copy meets new documents in the next
    // cohort, scores above S_Rk-1 and short of the margin, and matches kappa's
    // fingerprint on the probe.
    let later = fx::later_copy_of_a_low_scorer(true)?;
    let p = &later.package;
    let copy = &p.entrants[0];
    let predecessor = p.predecessor.as_ref().unwrap().score;
    assert!(copy.score.unwrap() > predecessor);
    assert_eq!(copy.public_copy(), Some(PublicCopy::Probe));
    let public = copy.public.as_ref().unwrap();
    assert!(!public.current.within(NEAR_COPY_PPB)?);
    assert!(!public.predecessor.unwrap().within(NEAR_COPY_PPB)?);
    let found = public.probe_match.as_ref().unwrap();
    assert_eq!(found.content, fx::content(0x23));
    assert_eq!(found.stats.min_rss(), 0);
    assert_eq!((p.mode, copy.units), (CutoffMode::NoTarget, 0));
    reward_v1::check_outcome(p, &later.index)?;
    // Without kappa's fingerprint, an index no closure produces, the same
    // copy would be gated and take the whole fallback row.
    let unprotected = fx::later_copy_of_a_low_scorer(false)?;
    let q = &unprotected.package;
    assert!(q.entrants[0].gated());
    assert_eq!(
        (q.mode, q.entrants[0].units),
        (CutoffMode::Fallback, u16::MAX)
    );
    Ok(())
}

#[test]
fn the_probe_comparison_includes_both_references() -> Result<()> {
    // A perturbed copy of R_k escapes the batch test by chance and predicts on
    // the probe as R_k's fingerprint does. The coordinator's archive
    // comparison leaves both references out; finalize compares them anyway,
    // and the copy fails public.near.probe against R_k itself.
    let h = fx::current_copy_on_probe()?;
    let p = &h.package;
    let copy = &p.entrants[0];
    let public = copy.public.as_ref().unwrap();
    assert!(!public.current.within(NEAR_COPY_PPB)?);
    assert!(!public.predecessor.unwrap().within(NEAR_COPY_PPB)?);
    assert_eq!(copy.score, Some(p.reference.score));
    assert_eq!(copy.public_copy(), Some(PublicCopy::Probe));
    let keys = p.reference_probe_keys();
    assert_eq!(
        keys,
        vec![
            (fx::content(0xe1), fx::PROBE_VERSION),
            (fx::content(0xe0), fx::PROBE_VERSION)
        ]
    );
    assert!(
        public
            .reference_probes
            .iter()
            .map(|m| (m.content, m.version))
            .eq(keys.iter().copied())
    );
    let found = public.probe_match.as_ref().unwrap();
    assert_eq!(found, &public.reference_probes[0]);
    assert_eq!(found.stats.min_rss(), 0);
    assert_eq!((p.mode, copy.units), (CutoffMode::NoTarget, 0));
    reward_v1::check_outcome(p, &h.index)?;
    // Validators check the comparison set: a package that leaves a reference
    // out, reorders them, or passes an entrant whose reference comparison is
    // a near copy is refused.
    let outcome = |q: &ScorePackageV2| reward_v1::check_outcome(q, &h.index).unwrap_err();
    let mut q = p.clone();
    q.entrants[0]
        .public
        .as_mut()
        .unwrap()
        .reference_probes
        .remove(0);
    assert_eq!(outcome(&q), Error::Mismatch("reference probe comparisons"));
    let mut q = p.clone();
    q.entrants[0]
        .public
        .as_mut()
        .unwrap()
        .reference_probes
        .swap(0, 1);
    assert_eq!(outcome(&q), Error::Mismatch("reference probe comparisons"));
    let mut q = p.clone();
    q.entrants[0].gate_failures.clear();
    q.entrants[0].public.as_mut().unwrap().probe_match = None;
    assert_eq!(
        outcome(&q),
        Error::Mismatch("reference probe match omitted")
    );
    // A match that names R_k repeats R_k's published comparison exactly.
    let mut q = p.clone();
    let m = q.entrants[0].public.as_mut().unwrap();
    m.probe_match.as_mut().unwrap().stats.rss_fwd = 1;
    assert_eq!(
        outcome(&q),
        Error::Mismatch("probe match differs from its reference")
    );
    // In the computed cohort, a probe match farther than a near reference
    // comparison is refused.
    let c = computed()?;
    let i = c
        .models
        .iter()
        .position(|m| m.name == "omega_probe_copy")
        .unwrap();
    let mut q = c.package.clone();
    let public = q.entrants[i].public.as_mut().unwrap();
    public.reference_probes[0].stats = fx::aligned(CASES);
    let half = u128::from(NEAR_COPY_PPB / 2);
    let found = public.probe_match.as_mut().unwrap();
    found.stats.rss_fwd = u128::from(CASES) * half * half;
    found.stats.rss_rev = found.stats.rss_fwd;
    assert_eq!(
        reward_v1::check_outcome(&q, &c.index).unwrap_err(),
        Error::Mismatch("probe match not the closest")
    );
    // A coordinator measurement that answers for another fingerprint than
    // the one asked is refused at finalization.
    struct Misnamed<'a>(fx::ComputedMeasure<'a>);
    impl Measurements for Misnamed<'_> {
        fn current(&mut self, e: usize) -> Result<NearStats> {
            self.0.current(e)
        }
        fn predecessor(&mut self, e: usize) -> Result<NearStats> {
            self.0.predecessor(e)
        }
        fn reference_probe(&mut self, e: usize, _: &Digest, v: u32) -> Result<ProbeMatch> {
            self.0.reference_probe(e, &fx::content(fx::ARCHIVED), v)
        }
        fn archive_probe(&mut self, e: usize) -> Result<reward_v1::ArchiveProbe> {
            self.0.archive_probe(e)
        }
        fn pair(&mut self, h: usize, o: usize) -> Result<NearStats> {
            self.0.pair(h, o)
        }
        fn promotion_audit(&mut self, h: usize) -> Result<crate::PromotionAudit> {
            self.0.promotion_audit(h)
        }
    }
    let mut q = c.package.clone();
    assert_eq!(
        finalize_fixture(
            &mut q,
            &c.index,
            &mut Misnamed(fx::ComputedMeasure::new(&c.models))
        )
        .unwrap_err(),
        Error::Mismatch("reference probe comparison")
    );
    Ok(())
}

#[test]
fn operations_recheck_the_active_target_every_tempo() -> Result<()> {
    let c = computed()?;
    let keys = crate::fixtures::trusted_keys();
    let v = AccountId([0x66; 32]);
    // A launch authorization checked before cohort 3's closing boundary, with
    // a hint refresh, keeps the burn row committed while cohort 3's target is
    // pending.
    let unactivated = c.active_before()?;
    let issued = c.manifest.opened_epoch;
    let launch = fx::signed(LaunchAuthorization::new(&chain(), 1, issued)?)?;
    let kept = LaunchAuthorization::check(&launch, &keys, &chain(), &unactivated)?;
    let renewal = fx::signed(RewardIntentRenewal::new(
        &kept,
        1,
        c.closure.published_epoch,
    )?)?;
    let renewal = RewardIntentRenewal::check(&renewal, &keys, &kept)?;
    let boundary = c.closure.closed_epoch;
    assert_eq!(
        kept.operation(Some(&renewal), &[], &unactivated, v, boundary - 1)?,
        WeightOperationV2::for_target(&CalculationTargetV2::launch_burn(chain()), v, boundary - 1)?
    );
    // Kept across the boundary, it commits the burn row no longer: it is
    // superseded by cohort 3's activation, with no end epoch involved.
    let active = c.active_after()?;
    for e in [boundary, boundary + 1, renewal.renewal().renew_by_epoch] {
        assert_eq!(
            kept.operation(Some(&renewal), &[], &active, v, e).unwrap_err(),
            Error::Invalid("launch authorization after activation")
        );
    }
    // Cohort 3's own authorization takes over at the boundary, and never
    // before its target is active.
    let authorized = c.checked_authorization()?;
    assert_eq!(
        authorized
            .operation(None, &[], &unactivated, v, boundary)
            .unwrap_err(),
        Error::Mismatch("authorized target not active")
    );
    authorized.operation(None, &[], &active, v, boundary)?;
    // Once a successor target activates, cohort 3's authorization stops too.
    let successor = fx::successor_of(&c)?;
    let checked = successor.closure.check(
        &successor.manifest,
        &successor.package,
        Some(&successor.target),
    )?;
    let next = ActiveTarget::activated(&checked)?;
    let e = successor.closure.closed_epoch;
    let renewal3 = c.checked_renewal(&authorized)?;
    authorized.operation(Some(&renewal3), &[], &active, v, e)?;
    assert_eq!(
        authorized
            .operation(Some(&renewal3), &[], &next, v, e)
            .unwrap_err(),
        Error::Mismatch("authorized target not active")
    );
    // An active target on another chain is never the authorized one.
    assert_eq!(
        kept.operation(None, &[], &ActiveTarget::launch(&finney())?, v, issued)
            .unwrap_err(),
        Error::Mismatch("authorized target not active")
    );
    Ok(())
}

#[test]
fn identical_content_records_the_earliest_committed_fingerprint() -> Result<()> {
    let c = computed()?;
    let at = |name: &str| c.models.iter().position(|m| m.name == name).unwrap();
    let (alpha, copy) = (at("alpha"), at("alpha_copy"));
    let (a, b) = (&c.package.entrants[alpha], &c.package.entrants[copy]);
    assert_eq!(a.content, b.content);
    assert!(a.position() < b.position());
    let base = fx::closure_fingerprints(&c);
    let digest_of = |fs: &[EntrantFingerprint], hotkey: AccountId| {
        fs.iter().find(|f| f.hotkey == hotkey).unwrap().fingerprint
    };
    let recorded = |package: &ScorePackageV2, fs: &[EntrantFingerprint]| -> Result<Digest> {
        Ok(canonical_fingerprints(package, fs)?
            .into_iter()
            .find(|f| f.content == a.content)
            .unwrap()
            .fingerprint)
    };
    // Whichever of the two returns divergent probe predictions (for example a
    // failure on one probe document), and whatever the digest bytes, alpha's
    // fingerprint is recorded: it committed first.
    for diverging in [b.identity.hotkey, a.identity.hotkey] {
        for bytes in [0x00, 0xff] {
            let mut fs = base.clone();
            for f in &mut fs {
                if f.hotkey == diverging {
                    f.fingerprint = Digest([bytes; 32]);
                }
            }
            assert_eq!(
                recorded(&c.package, &fs)?,
                digest_of(&fs, a.identity.hotkey)
            );
            let next = c.index.after_closure(&c.package, fs.iter().copied())?;
            assert!(next.fingerprinted(&a.content, fx::PROBE_VERSION));
            assert_eq!(
                next.fingerprints
                    .entries
                    .iter()
                    .filter(|f| f.content == a.content)
                    .count(),
                1
            );
            // The order in which the closure lists them plays no part.
            let mut reversed = fs.clone();
            reversed.reverse();
            assert_eq!(
                canonical_fingerprints(&c.package, &reversed)?,
                canonical_fingerprints(&c.package, &fs)?
            );
        }
    }
    // Earliest by commitment block, then extrinsic index, then hotkey bytes.
    let mut fs = base.clone();
    for f in &mut fs {
        if f.hotkey == b.identity.hotkey {
            f.fingerprint = Digest([0x42; 32]);
        }
    }
    let mut p = c.package.clone();
    p.entrants[alpha].commitment_block = b.commitment_block + 1;
    assert_eq!(recorded(&p, &fs)?, Digest([0x42; 32]));
    let mut p = c.package.clone();
    p.entrants[alpha].commitment_block = b.commitment_block;
    p.entrants[alpha].extrinsic_index = b.extrinsic_index + 1;
    assert_eq!(recorded(&p, &fs)?, Digest([0x42; 32]));
    p.entrants[alpha].extrinsic_index = b.extrinsic_index;
    assert!(a.identity.hotkey < b.identity.hotkey);
    assert_eq!(recorded(&p, &fs)?, digest_of(&fs, a.identity.hotkey));
    Ok(())
}

#[test]
fn the_distance_monitor_reports_the_closest_archive_probe_distance() -> Result<()> {
    // The computed cohort: the smallest probe distance between a gated
    // entrant and any fingerprint, the references included, recomputed from
    // the hidden probe predictions.
    let c = computed()?;
    let measure = fx::ComputedMeasure::new(&c.models);
    let mut expected: Option<u32> = None;
    for (m, e) in c.models.iter().zip(&c.package.entrants) {
        if !e.gated() {
            continue;
        }
        for (_, fingerprint) in &measure.archive {
            let own = m.probe_predictions().unwrap();
            let d = reward_v1::near_stats(own, fingerprint)?.distance_ppb();
            expected = expected.into_iter().chain(d).min();
        }
    }
    assert_eq!(
        c.package.distance_monitor.min_gated_vs_archive_ppb,
        expected
    );
    // A coordinator measurement below the published reference comparisons:
    // spec entrants are 2d from both references on the probe, and the
    // archive comparison reports 1.2d for one gated entrant and 1 ppb for a
    // gate-failed one, which does not count.
    struct ArchiveAt<M> {
        inner: M,
        distances: Vec<Option<u32>>,
    }
    impl<M: Measurements> Measurements for ArchiveAt<M> {
        fn current(&mut self, e: usize) -> Result<NearStats> {
            self.inner.current(e)
        }
        fn predecessor(&mut self, e: usize) -> Result<NearStats> {
            self.inner.predecessor(e)
        }
        fn reference_probe(&mut self, e: usize, c: &Digest, v: u32) -> Result<ProbeMatch> {
            self.inner.reference_probe(e, c, v)
        }
        fn archive_probe(&mut self, e: usize) -> Result<reward_v1::ArchiveProbe> {
            Ok(reward_v1::ArchiveProbe {
                candidates: Vec::new(),
                min_distance_ppb: self.distances[e],
            })
        }
        fn pair(&mut self, h: usize, o: usize) -> Result<NearStats> {
            self.inner.pair(h, o)
        }
        fn promotion_audit(&mut self, h: usize) -> Result<crate::PromotionAudit> {
            self.inner.promotion_audit(h)
        }
    }
    let specs = [
        s(0x21, 9600),
        s(0x22, 9590),
        s(0x23, 9580).failing(Gate::Share, "share.mae_above_threshold"),
    ];
    let m = fx::manifest(chain(), 7, 1000, sigma_permille(40), 1, &specs);
    let index = fx::default_index(&m.references);
    let mut p = fx::package(&m, bp(9100), Some(bp(9000)), &specs)?;
    let d = NEAR_COPY_PPB;
    let mut measure = ArchiveAt {
        inner: fx::spec_measure(&specs, 1000, |_, _| false),
        distances: vec![Some(d / 5 * 8), Some(d / 5 * 6), Some(1)],
    };
    finalize_fixture(&mut p, &index, &mut measure)?;
    assert_eq!(p.distance_monitor.min_gated_vs_archive_ppb, Some(d / 5 * 6));
    reward_v1::check_outcome(&p, &index)?;
    // Validators bound it by the published reference comparisons (2d here).
    let outcome = |q: &ScorePackageV2| reward_v1::check_outcome(q, &index).map(|_| ());
    let mut q = p.clone();
    q.distance_monitor.min_gated_vs_archive_ppb = Some(2 * d);
    outcome(&q)?;
    q.distance_monitor.min_gated_vs_archive_ppb = Some(2 * d + 1);
    assert_eq!(
        outcome(&q).unwrap_err(),
        Error::Mismatch("distance monitor")
    );
    q.distance_monitor.min_gated_vs_archive_ppb = None;
    assert_eq!(
        outcome(&q).unwrap_err(),
        Error::Mismatch("distance monitor")
    );
    // Without a gated entrant it is absent.
    let copy = fx::current_copy_on_probe()?;
    assert!(!copy.package.entrants[0].gated());
    assert_eq!(copy.package.distance_monitor.min_gated_vs_archive_ppb, None);
    let mut q = copy.package.clone();
    q.distance_monitor.min_gated_vs_archive_ppb = Some(0);
    assert_eq!(
        reward_v1::check_outcome(&q, &copy.index).unwrap_err(),
        Error::Mismatch("distance monitor")
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// reward-v1 live: the manifest mix, the loose false-positive gate, the
// promotion audits (strict tier and lookup canary) and the clearance record.
// ---------------------------------------------------------------------------

/// A margin cohort at R_k = 0.91 (T = 0.9172) with three distinct heads above
/// the margin, finalized with the given audit results by package index.
fn audited(audits: &[(usize, bool, u8)]) -> Result<(ScorePackageV2, PublishedIndex)> {
    let specs = [s(0x71, 9600), s(0x72, 9500), s(0x73, 9400)];
    let m = fx::manifest(chain(), 7, 1000, sigma_permille(40), 0, &specs);
    let index = fx::default_index(&m.references);
    let mut p = fx::package(&m, bp(REFERENCE), None, &specs)?;
    let mut measure = fx::spec_measure(&specs, 1000, |_, _| false);
    for &(i, strict, flags) in audits {
        measure.audits.insert(i, (strict, flags));
    }
    finalize_fixture(&mut p, &index, &mut measure)?;
    Ok((p, index))
}

fn audit_heads(p: &ScorePackageV2) -> Vec<u8> {
    p.promotion_audits.iter().map(|a| a.head.0[0]).collect()
}

#[test]
fn the_origin_mix_is_a_manifest_parameter() -> Result<()> {
    LAUNCH_ORIGIN_MIX.validate()?;
    let mut b = fx::batch(1000);
    b.validate(&chain())?;
    // The launch 20/55/25 mix is accepted when the strata realize it.
    let cell = |domain: &str, origin, cases| Stratum {
        domain: String::from(domain),
        origin,
        cases,
    };
    b.mix = LAUNCH_ORIGIN_MIX;
    assert_eq!(
        b.validate(&chain()),
        Err(Error::Mismatch("origin mix cases"))
    );
    b.strata = vec![
        cell("blogs", Origin::HumanOnly, 100),
        cell("blogs", Origin::Mixed, 275),
        cell("blogs", Origin::ModelOnly, 125),
        cell("essays", Origin::HumanOnly, 100),
        cell("essays", Origin::Mixed, 275),
        cell("essays", Origin::ModelOnly, 125),
    ];
    b.validate(&chain())?;
    // Shares must be whole percentages summing to 100, every origin present.
    for bad in [
        OriginMix {
            human_only: 20,
            mixed: 55,
            model_only: 26,
        },
        OriginMix {
            human_only: 0,
            mixed: 75,
            model_only: 25,
        },
        OriginMix {
            human_only: 100,
            mixed: 0,
            model_only: 0,
        },
    ] {
        assert_eq!(bad.validate(), Err(Error::Invalid("origin mix")));
        b.mix = bad;
        assert_eq!(b.validate(&chain()), Err(Error::Invalid("origin mix")));
    }
    Ok(())
}

#[test]
fn a_loose_false_positive_failure_is_a_gate_failure() -> Result<()> {
    // The best entrant fails the loose tier on its own cohort humans: it earns
    // nothing, cannot set the cutoff and is not promoted.
    let specs = [
        s(0x81, 9700).failing(Gate::HumanFalsePositive, HUMAN_FPR_LOOSE_REASON),
        s(0x82, 9600),
    ];
    let p = example(bp(REFERENCE), None, &specs)?;
    assert_eq!(p.mode, CutoffMode::Margin);
    assert_eq!(units(&p), vec![(0x82, 65535)]);
    assert_eq!(p.promoted, Some(hotkey(0x82)));
    assert!(!p.entrants[0].gated());
    let round = ScorePackageV2::from_bytes(&p.to_bytes()?)?;
    assert_eq!(
        round.entrants[0].gate_failures[0].gate,
        Gate::HumanFalsePositive
    );
    // Alone, it leaves no gated entrant: no target.
    let alone = example(bp(REFERENCE), None, &specs[..1])?;
    assert_eq!(alone.mode, CutoffMode::NoTarget);
    Ok(())
}

#[test]
fn a_clean_audit_promotes_the_top_head_and_audits_nobody_else() -> Result<()> {
    let (p, index) = audited(&[])?;
    assert_eq!(p.mode, CutoffMode::Margin);
    assert_eq!(p.promoted, Some(hotkey(0x71)));
    assert_eq!(audit_heads(&p), vec![0x71]);
    reward_v1::check_outcome(&p, &index)?;
    Ok(())
}

#[test]
fn one_flag_triggers_a_reaudit_and_a_clean_reaudit_promotes() -> Result<()> {
    let (p, index) = audited(&[(0, true, 1)])?;
    assert_eq!(p.promoted, Some(hotkey(0x71)));
    let audit = &p.promotion_audits[0];
    assert_eq!(audit.canary.len(), 2);
    assert!(audit.canary[0].flag.is_some() && audit.canary[1].flag.is_none());
    assert_ne!(audit.canary[0].draw, audit.canary[1].draw);
    assert!(!audit.blocked() && audit.passes());
    reward_v1::check_outcome(&p, &index)?;
    Ok(())
}

#[test]
fn two_flags_block_and_the_next_head_is_promoted() -> Result<()> {
    let (p, index) = audited(&[(0, true, 2)])?;
    assert_eq!(p.mode, CutoffMode::Margin);
    assert_eq!(p.promoted, Some(hotkey(0x72)));
    assert_eq!(audit_heads(&p), vec![0x71, 0x72]);
    assert!(p.promotion_audits[0].blocked());
    // Credits are paid as usual: the blocked head keeps its top-band credit.
    let clean = audited(&[])?.0;
    assert_eq!(units(&p), units(&clean));
    reward_v1::check_outcome(&p, &index)?;
    // The next references follow the promoted head.
    let refs = fx::references(0);
    let next = reward_v1::next_references(&refs, &p, &index)?;
    assert_eq!(next.current.content, p.entrants[1].content);
    assert_eq!(next.predecessor, Some(refs.current));
    Ok(())
}

#[test]
fn a_strict_tier_failure_passes_promotion_to_the_next_head() -> Result<()> {
    let (p, index) = audited(&[(0, false, 0), (1, true, 2)])?;
    assert_eq!(p.promoted, Some(hotkey(0x73)));
    assert_eq!(audit_heads(&p), vec![0x71, 0x72, 0x73]);
    reward_v1::check_outcome(&p, &index)?;
    Ok(())
}

#[test]
fn when_no_head_qualifies_nobody_is_promoted_and_credits_stand() -> Result<()> {
    let (p, index) = audited(&[(0, true, 2), (1, false, 0), (2, false, 1)])?;
    assert_eq!((p.mode, p.promoted), (CutoffMode::Margin, None));
    assert_eq!(audit_heads(&p), vec![0x71, 0x72, 0x73]);
    assert_eq!(units(&p), units(&audited(&[])?.0));
    reward_v1::check_outcome(&p, &index)?;
    let refs = fx::references(0);
    assert_eq!(reward_v1::next_references(&refs, &p, &index)?, refs);
    // The target is still produced and pays the clusters.
    CalculationTargetV2::for_package(&p, None)?;
    Ok(())
}

#[test]
fn a_head_just_below_another_is_a_promotion_candidate() -> Result<()> {
    // reward-v1 dropped 0x92 as near-equal; under reward-v2 it is a cluster
    // head that meets the exact margin, so when 0x91 is blocked the walk
    // promotes it.
    let below = |tag, by: u64| Spec::new(tag, Score::new(bp(9600).get() - by).unwrap());
    let specs = [
        s(0x91, 9600),
        below(0x92, 10_000_000_000_000),
        s(0x93, 9400),
    ];
    let m = fx::manifest(chain(), 7, 1000, sigma_permille(40), 0, &specs);
    let index = fx::default_index(&m.references);
    let mut p = fx::package(&m, bp(REFERENCE), None, &specs)?;
    let mut measure = fx::spec_measure(&specs, 1000, |_, _| false);
    measure.audits.insert(0, (true, 2));
    finalize_fixture(&mut p, &index, &mut measure)?;
    assert_eq!(audit_heads(&p), vec![0x91, 0x92]);
    assert_eq!(p.promoted, Some(hotkey(0x92)));
    reward_v1::check_outcome(&p, &index)?;
    Ok(())
}

#[test]
fn tampered_promotion_audits_are_refused() -> Result<()> {
    let (p, index) = audited(&[(0, true, 2)])?;
    let refused = |q: &ScorePackageV2| {
        q.validate()
            .and_then(|_| reward_v1::check_outcome(q, &index).map(|_| ()))
            .unwrap_err()
    };
    // Promoting the blocked head.
    let mut q = p.clone();
    q.promoted = Some(hotkey(0x71));
    assert_eq!(refused(&q), Error::Mismatch("promotion audits"));
    // Leaving out the blocked head's audit.
    let mut q = p.clone();
    q.promotion_audits.remove(0);
    assert_eq!(refused(&q), Error::Mismatch("promotion audit order"));
    // A second flag recorded without a first.
    let mut q = p.clone();
    q.promotion_audits[0].canary[0].flag = None;
    assert_eq!(refused(&q), Error::Invalid("canary audits"));
    // A flag without its re-audit.
    let mut q = p.clone();
    q.promotion_audits[0].canary.pop();
    assert_eq!(refused(&q), Error::Invalid("canary audits"));
    // Auditing past the promoted head.
    let mut q = p.clone();
    q.promotion_audits
        .push(fx::promotion_audit(hotkey(0x73), true, 0));
    assert_eq!(refused(&q), Error::Mismatch("promotion audits"));
    // Audits outside a margin cohort.
    let mut f = example(bp(REFERENCE), Some(bp(8775)), &[s(0x41, 9159)])?;
    assert_eq!(f.mode, CutoffMode::Fallback);
    f.promotion_audits
        .push(fx::promotion_audit(hotkey(0x41), true, 0));
    assert_eq!(
        f.validate().unwrap_err(),
        Error::Invalid("promotion audits outside a margin cohort")
    );
    // A margin cohort that promotes nobody needs audits that all fail.
    let mut q = p.clone();
    q.promoted = None;
    q.promotion_audits.clear();
    assert_eq!(refused(&q), Error::Mismatch("promotion audits"));
    Ok(())
}

#[test]
fn a_canary_clearance_names_a_blocked_candidate_and_changes_nothing() -> Result<()> {
    let (p, index) = audited(&[(0, true, 2)])?;
    let clearance = CanaryClearance {
        chain: p.chain.clone(),
        cohort: p.cohort,
        cohort_manifest: p.cohort_manifest,
        score_package: p.digest()?,
        head: hotkey(0x71),
        review: Digest([0x5a; 32]),
        cleared_epoch: 900,
    };
    clearance.check(&p)?;
    let signed = sign(clearance.clone(), &crate::fixtures::package_key())?;
    let keys = crate::fixtures::trusted_keys();
    assert_eq!(signed.verify(&keys)?, &clearance);
    // The package and its outcome are unchanged; the promotion stands.
    reward_v1::check_outcome(&p, &index)?;
    assert_eq!(p.promoted, Some(hotkey(0x72)));
    // Only a blocked candidate can be cleared, and only for its package.
    let mut other = clearance.clone();
    other.head = hotkey(0x72);
    assert_eq!(
        other.check(&p).unwrap_err(),
        Error::Mismatch("clearance of a candidate not blocked")
    );
    let mut other = clearance;
    other.score_package = Digest([0; 32]);
    assert_eq!(
        other.check(&p).unwrap_err(),
        Error::Mismatch("clearance package")
    );
    Ok(())
}

/// Prints the golden report, to regenerate `fixtures/golden-reward-v1.txt`
/// after a deliberate change: `cargo test -p slop-ninja-protocol --lib
/// print_golden_reward_v1_report -- --ignored --nocapture`.
#[test]
#[ignore]
fn print_golden_reward_v1_report() -> Result<()> {
    print!("{}", fx::golden_report()?);
    Ok(())
}

#[test]
fn a_launch_bundle_commits_only_the_burn_row_with_no_end() -> Result<()> {
    let keys = crate::fixtures::trusted_keys();
    let launch = fx::signed(LaunchAuthorization::new(&chain(), 1, 100)?)?.to_bytes()?;
    let v = target_bundle::verify(&chain(), &keys, &ActiveTarget::launch(&chain())?, None, Authorizing::Launch { authorization: &launch, renewal: None })?;
    assert_eq!(v.target, CalculationTargetV2::launch_burn(chain()));
    let terms = v.checked_terms(&keys)?;
    assert_eq!((terms.issued_epoch, terms.renew_by_epoch, terms.checkpoint_generation), (100, 140, 1));
    let hk = AccountId([0x66; 32]);
    let op = v.operation(&keys, &[], hk, 120)?;
    assert_eq!(op, WeightOperationV2::for_target(&v.target, hk, 120)?);
    // Past the renew-by hint the burn row keeps committing.
    for e in [140, 141, 100_000] {
        assert_eq!(v.operation(&keys, &[], hk, e)?, WeightOperationV2::for_target(&v.target, hk, e)?);
    }
    assert_eq!(v.operation(&keys, &[], hk, 99).unwrap_err(), Error::Invalid("epoch before authorization"));
    // A signed revocation of the burn row's authorization stops it; one of
    // another target or chain does not. The holder checks the signature
    // when it journals one; a journaled one is not verified again, so a
    // rotated trust root never turns it into an error.
    let auth = Signed::<LaunchAuthorization>::from_bytes(&launch)?;
    let checked = LaunchAuthorization::check(&auth, &keys, &chain(), &ActiveTarget::launch(&chain())?)?;
    let revocation = fx::signed(RewardIntentRevocation::new(&checked, 1, 150)?)?;
    RewardIntentRevocation::check(&revocation, &keys, &chain())?;
    let journal = std::slice::from_ref(&revocation.record);
    assert_eq!(v.revoked_by(&keys, journal)?, Some(revocation.record.digest()?));
    assert_eq!(v.operation(&keys, journal, hk, 151).unwrap_err(), Error::Revoked("reward intent authorization"));
    let mut other = revocation.record.clone();
    other.target = Digest([7; 32]);
    let mut elsewhere = revocation.record.clone();
    elsewhere.chain.netuid += 1;
    let unrelated = [other, elsewhere];
    assert_eq!(v.revoked_by(&keys, &unrelated)?, None);
    v.operation(&keys, &unrelated, hk, 151)?;
    let mut forged = revocation.clone();
    forged.signature[1] ^= 4;
    assert_eq!(RewardIntentRevocation::check(&forged, &keys, &chain()).unwrap_err(), Error::BadSignature);
    // A later launch authorization re-authorizes the burn row.
    let again = fx::signed(LaunchAuthorization::new(&chain(), 2, 151)?)?.to_bytes()?;
    let again = target_bundle::verify(&chain(), &keys, &ActiveTarget::launch(&chain())?, None, Authorizing::Launch { authorization: &again, renewal: None })?;
    assert_eq!(again.revoked_by(&keys, journal)?, None);
    again.operation(&keys, journal, hk, 151)?;
    // The row burns through the owner hotkey of the holder's snapshot.
    let snapshot = RegistrationSnapshotV2 {
        chain: chain(),
        block: 9,
        block_hash: Digest([1; 32]),
        owner_hotkey: fx::owner_hotkey(),
        neurons: vec![NeuronIdentity { uid: 4, hotkey: fx::owner_hotkey(), registration_block: 3 }],
    };
    let row = v.derive_row(&snapshot)?;
    assert_eq!((row.entries.len(), row.entries[0].uid, row.entries[0].weight), (1, 4, u16::MAX));
    // An authorization under another key, or for another chain, is refused.
    let other = sign(LaunchAuthorization::new(&chain(), 1, 100)?, &crate::fixtures::package_key())?.to_bytes()?;
    assert!(target_bundle::verify(&chain(), &keys, &ActiveTarget::launch(&chain())?, None, Authorizing::Launch { authorization: &other, renewal: None }).is_err());
    assert!(target_bundle::verify(&finney(), &keys, &ActiveTarget::launch(&finney())?, None, Authorizing::Launch { authorization: &launch, renewal: None }).is_err());
    // A cohort authorization needs its target's records.
    assert_eq!(
        target_bundle::verify(&chain(), &keys, &ActiveTarget::launch(&chain())?, None, Authorizing::Cohort { authorization: &launch, renewal: None, carrier: None })
            .unwrap_err(),
        Error::Invalid("cohort authorization without its target")
    );
    Ok(())
}

#[test]
fn a_receipt_takes_one_length_in_a_validator_bundle() -> Result<()> {
    // Every receipt object and index entry has one length, whatever its
    // digests, attempt number and outcome class, so the receipts' part of a
    // bundle is exact from the receipt count.
    let classes = [
        OutcomeClass::Prediction,
        OutcomeClass::ModelError,
        OutcomeClass::InvalidOutput,
        OutcomeClass::ParseFailure,
        OutcomeClass::InfrastructureError,
    ];
    let mut lengths = std::collections::BTreeSet::new();
    for (n, outcome) in classes.into_iter().enumerate() {
        let receipt = AttemptReceipt {
            attempt: Digest([n as u8; 32]),
            assignment: Digest([0xff; 32]),
            artifact: Digest([0x11; 32]),
            number: u8::try_from(n + 1).unwrap_or(u8::MAX).max(1) * 50,
            outcome,
            result_commitment: Digest([0x12; 32]),
        };
        let entry = ReceiptIndexEntry::for_receipt(&receipt)?;
        let mut e = Encoder::new();
        entry.encode_to(&mut e);
        lengths.insert((receipt.to_bytes()?.len(), e.into_bytes().len()));
    }
    assert_eq!(lengths.len(), 1, "{lengths:?}");
    let (object, entry) = lengths.into_iter().next().unwrap();
    let per = 2 * (4 + object + entry) as u64;
    assert_eq!(target_bundle::receipt_bundle_bytes(1)?, per);
    assert_eq!(target_bundle::receipt_bundle_bytes(150_000)?, 150_000 * per);
    assert_eq!(target_bundle::receipt_bundle_bytes(0)?, 0);
    assert!(target_bundle::receipt_bundle_bytes(u64::MAX).is_err());
    Ok(())
}

/// Review (standing-r0 round 1): a signed receipt index's length is exact
/// (69 bytes an entry beside a fixed header), and the receipt bound is the
/// tighter of the codec's sequence bound and the signed-record limit: an
/// index one entry past [`target_bundle::receipt_index_capacity`] cannot be
/// signed, so the coordinator could not package such a cohort.
#[test]
fn the_receipt_index_capacity_is_what_one_signed_envelope_holds() -> Result<()> {
    let receipt = |n: u32| AttemptReceipt {
        attempt: Digest(domain_hash("test/receipt-index-capacity", &n.to_be_bytes()).0),
        assignment: Digest([0x10; 32]),
        artifact: Digest([0x11; 32]),
        number: 1,
        outcome: OutcomeClass::Prediction,
        result_commitment: Digest([0x12; 32]),
    };
    let key = crate::fixtures::receipt_key();
    for n in [0u32, 1, 3, 100] {
        let mut entries: Vec<ReceiptIndexEntry> = (0..n).map(|i| ReceiptIndexEntry::for_receipt(&receipt(i)).unwrap()).collect();
        entries.sort_by_key(|e| e.attempt);
        let signed = crate::sign(ReceiptIndex { cohort_manifest: Digest([9; 32]), entries }, &key)?.to_bytes()?;
        assert_eq!(target_bundle::receipt_index_bytes(u64::from(n))?, signed.len() as u64, "{n} entries");
    }
    let per = target_bundle::receipt_index_bytes(1)? - target_bundle::receipt_index_bytes(0)?;
    assert_eq!(per, 69);
    let capacity = target_bundle::receipt_index_capacity()?;
    assert!(capacity < u64::from(crate::MAX_SEQUENCE_LEN), "the signed-record limit is the tighter bound");
    // (64 MiB - 55 bytes of record header) / 69.
    assert_eq!(capacity, 972_591);
    // The record (the signed bytes less the envelope) fits at the capacity
    // and not one entry later.
    let envelope = target_bundle::receipt_index_bytes(0)?
        - ReceiptIndex { cohort_manifest: Digest([0; 32]), entries: Vec::new() }.to_bytes()?.len() as u64;
    let record = |n: u64| target_bundle::receipt_index_bytes(n).unwrap() - envelope;
    assert!(record(capacity) <= u64::from(crate::MAX_SIGNED_RECORD_LEN));
    assert!(record(capacity + 1) > u64::from(crate::MAX_SIGNED_RECORD_LEN));
    // And the encoder agrees: one entry past the capacity is refused.
    let entries: Vec<ReceiptIndexEntry> = (0..=capacity)
        .map(|i| ReceiptIndexEntry {
            attempt: Digest({
                let mut d = [0u8; 32];
                d[24..].copy_from_slice(&i.to_be_bytes());
                d
            }),
            outcome: OutcomeClass::Prediction,
            object_sha256: Digest([1; 32]),
            object_len: 150,
        })
        .collect();
    let over = Signed { record: ReceiptIndex { cohort_manifest: Digest([9; 32]), entries }, public_key: [0; 32], signature: [0; 64] };
    assert!(matches!(over.to_bytes(), Err(Error::LengthLimit { field: "signed record", .. })));
    assert_eq!(target_bundle::validator_receipt_index_bytes()?, 2 * target_bundle::receipt_index_bytes(capacity)?);
    Ok(())
}

#[test]
fn receipt_chunks_must_hold_every_indexed_receipt_once() -> Result<()> {
    let receipt = |n: u8| AttemptReceipt {
        attempt: Digest([n; 32]),
        assignment: Digest([0x10; 32]),
        artifact: Digest([0x11; 32]),
        number: 1,
        outcome: OutcomeClass::Prediction,
        result_commitment: Digest([0x12; 32]),
    };
    let objects: Vec<Vec<u8>> = (1..=3u8).map(|n| receipt(n).to_bytes().unwrap()).collect();
    let index = ReceiptIndex {
        cohort_manifest: Digest([9; 32]),
        entries: (1..=3u8).map(|n| ReceiptIndexEntry::for_receipt(&receipt(n)).unwrap()).collect(),
    };
    let chunk = |items: &[&Vec<u8>]| {
        let mut out = Vec::new();
        for o in items {
            out.extend_from_slice(&(o.len() as u32).to_le_bytes());
            out.extend_from_slice(o);
        }
        out
    };
    let (a, b) = (chunk(&[&objects[0], &objects[2]]), chunk(&[&objects[1]]));
    assert_eq!(target_bundle::verify_receipt_chunks(&index, &[&a, &b])?.len(), 3);
    assert_eq!(target_bundle::verify_receipt_chunks(&index, &[&a]).unwrap_err(), Error::Mismatch("index entry without a stored receipt"));
    assert_eq!(target_bundle::verify_receipt_chunks(&index, &[&a, &b, &b]).unwrap_err(), Error::Duplicate("receipt stored twice"));
    let mut cut = b.clone();
    cut.pop();
    assert_eq!(target_bundle::verify_receipt_chunks(&index, &[&a, &cut]).unwrap_err(), Error::Mismatch("receipt chunk truncated"));
    Ok(())
}

#[test]
fn an_abandonment_record_round_trips_signs_and_refuses_free_text() -> Result<()> {
    let record = submission::CohortAbandonment {
        chain: chain(),
        cohort: 19,
        cohort_label: "testnet-r0-19".into(),
        cohort_manifest: Some(Digest([7; 32])),
        fault: "unresolved_infrastructure".into(),
        reason: "operator:receipt_capacity".into(),
        abandoned_epoch: Some(120),
    };
    let bytes = record.to_bytes()?;
    assert_eq!(submission::CohortAbandonment::from_bytes(&bytes)?, record);
    // Before its seal: no manifest; no epoch observed.
    let early = submission::CohortAbandonment { cohort_manifest: None, abandoned_epoch: None, ..record.clone() };
    assert_eq!(submission::CohortAbandonment::from_bytes(&early.to_bytes()?)?, early);
    // Signed only by the package key, and opened under it.
    let keys = crate::fixtures::trusted_keys();
    let signed = sign(record.clone(), &crate::fixtures::package_key())?.to_bytes()?;
    assert_eq!(Signed::<submission::CohortAbandonment>::open(&signed, &keys)?, record);
    let by_receipt_key = sign(record.clone(), &crate::fixtures::receipt_key())?.to_bytes()?;
    assert!(Signed::<submission::CohortAbandonment>::open(&by_receipt_key, &keys).is_err());
    // Codes only: no free text, nothing empty or over the bound.
    for bad in ["Budget Spent", "", "a b", &"x".repeat(submission::ABANDONMENT_CODE_MAX + 1)] {
        let r = submission::CohortAbandonment { reason: bad.to_owned(), ..record.clone() };
        assert!(r.to_bytes().is_err(), "{bad:?}");
        let f = submission::CohortAbandonment { fault: bad.to_owned(), ..record.clone() };
        assert!(f.to_bytes().is_err(), "{bad:?}");
    }
    Ok(())
}

#[test]
fn a_verified_header_takes_the_store_digest_and_hashes_nothing() -> Result<()> {
    let object = fx::artifact_object(0x11, 0x11);
    let h = fx::header_bytes(&object);
    let full = VerifiedArtifact::from_bytes(&object)?;
    let stored = crate::VerifiedHeader::from_stored(&h, full.header_sha256)?;
    assert_eq!(stored, full.verified_header());
    assert_eq!(stored, crate::VerifiedHeader::from_bytes(&h)?);
    assert_eq!(stored.len as usize, h.len());
    // The digest is the store's: a different stored digest is carried as
    // given, so the envelope comparison is what refuses it.
    let other = crate::VerifiedHeader::from_stored(&h, Digest([9; 32]))?;
    assert_eq!(other.sha256, Digest([9; 32]));
    assert_eq!(other.content, stored.content);
    assert!(crate::VerifiedHeader::from_stored(&[], Digest([0; 32])).is_err());
    assert!(crate::VerifiedHeader::from_stored(&h[..h.len() - 1], full.header_sha256).is_err());
    let c = computed()?;
    let spec = &c.specs[0];
    let testnet = chain();
    let opening = fx::opening(&testnet, 3, spec);
    let upload = submission::SubmissionUpload::new(opening.clone(), &fx::header_bytes(&fx::artifact_bytes(spec)))?;
    let payload = opening.chain_payload();
    let finalized = fx::finalized_commitment(&testnet, spec, fx::commitment_block(0), &payload);
    let good = fx::verified_header(&fx::artifact_bytes(spec));
    assert!(upload.admit_header(3, &finalized, &good).is_ok());
    let wrong_digest = crate::VerifiedHeader { sha256: Digest([9; 32]), ..good.clone() };
    assert_eq!(upload.admit_header(3, &finalized, &wrong_digest).unwrap_err(), Error::Mismatch("upload header"));
    let wrong_len = crate::VerifiedHeader { len: good.len + 1, ..good.clone() };
    assert_eq!(upload.admit_header(3, &finalized, &wrong_len).unwrap_err(), Error::Mismatch("upload header"));
    Ok(())
}
