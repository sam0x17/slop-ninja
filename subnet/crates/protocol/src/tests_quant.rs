//! `quant-v1` and the q-only `reward-v2` rules: golden vectors, sampler
//! correctness, determinism, the privacy of exact values, the bit budget,
//! ties, the grid cutoff, promotion and the minimum-success gate, each with
//! its negative control.

use crate::bigint::Fraction;
use crate::fixtures_quant::{self as fq, COHORT, SEED, TESTNET_QUANT, entrant};
use crate::fixtures_reward_v1::{
    self as fx, FIXTURE_QUANT, Spec, bp, chain, finalize_fixture, result_of, sigma_permille,
};
use crate::quant::{self, Budget, SeMeasure, Stream, Subject, Tau, U256, Words};
use crate::reward_v1::{self, QEntrant, QInput, q_outcome, q_rank};
use crate::*;

const GOLDEN: &str = include_str!("../fixtures/golden-quant-v1.txt");

#[test]
fn golden_quant_v1_report_is_unchanged() -> Result<()> {
    let report = fq::golden_report()?;
    for (i, (a, b)) in report.lines().zip(GOLDEN.lines()).enumerate() {
        assert_eq!(a, b, "golden line {}", i + 1);
    }
    assert_eq!(report, GOLDEN);
    Ok(())
}

/// Prints the golden report, to regenerate `fixtures/golden-quant-v1.txt`
/// after a deliberate change: `cargo test -p slop-ninja-protocol --lib
/// print_golden_quant_v1_report -- --ignored --nocapture`.
#[test]
#[ignore]
fn print_golden_quant_v1_report() -> Result<()> {
    print!("{}", fq::golden_report()?);
    Ok(())
}

#[test]
fn a_changed_seed_byte_changes_every_draw() -> Result<()> {
    let mut other = SEED;
    other[31] ^= 1;
    let tau = Tau::new(1000, 42, 2000)?;
    let w = 30_000_000_000_000_000;
    let a = quant::draw(&SEED, &COHORT, &entrant(0x11), w, &tau)?;
    let b = quant::draw(&other, &COHORT, &entrant(0x11), w, &tau)?;
    assert_ne!(a, b);
    assert_ne!(
        quant::noise_key(&SEED, &COHORT, &entrant(0x11)),
        quant::noise_key(&other, &COHORT, &entrant(0x11))
    );
    // Subjects are domain separated: the same 32 bytes as an entrant and as
    // a reference give different keys.
    assert_ne!(
        quant::offset_key(&SEED, &COHORT, &Subject::Entrant(Digest([0x21; 32]))),
        quant::offset_key(&SEED, &COHORT, &Subject::Current(Digest([0x21; 32])))
    );
    Ok(())
}

/// Review 2026-11-05 (leak 2): the same seed and subject under another
/// cohort (a reused label on a fresh chain record, a reset ledger) draws
/// independently, for entrants and for the references alike.
#[test]
fn a_changed_cohort_changes_every_draw() -> Result<()> {
    let tau = Tau::new(1000, 42, 2000)?;
    let w = 30_000_000_000_000_000;
    let mut other = COHORT;
    other.0.0[0] ^= 1;
    for subject in [
        entrant(0x11),
        Subject::Current(Digest([0x21; 32])),
        Subject::Predecessor(Digest([0x22; 32])),
    ] {
        assert_ne!(
            quant::draw(&SEED, &COHORT, &subject, w, &tau)?,
            quant::draw(&SEED, &other, &subject, w, &tau)?
        );
        assert_ne!(
            quant::noise_key(&SEED, &COHORT, &subject),
            quant::noise_key(&SEED, &other, &subject)
        );
        assert_ne!(
            quant::offset_key(&SEED, &COHORT, &subject),
            quant::offset_key(&SEED, &other, &subject)
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Integers, SE and the grid
// ---------------------------------------------------------------------------

#[test]
fn u256_arithmetic_is_exact() {
    let a = U256::mul_u128(u128::MAX, u128::MAX);
    assert_eq!(a, U256 { hi: u128::MAX - 1, lo: 1 });
    assert_eq!(a.div_ceil_u128(u128::MAX), Some(u128::MAX));
    assert_eq!(U256::from_u128(10).div_ceil_u128(3), Some(4));
    assert_eq!(U256::from_u128(9).div_ceil_u128(3), Some(3));
    assert_eq!(U256 { hi: 1, lo: 0 }.div_ceil_u128(1), None);
    assert_eq!(U256::ZERO.checked_sub(U256::from_u128(1)), None);
    assert_eq!(
        U256::from_u128(u128::MAX).checked_add(U256::from_u128(1)),
        Some(U256 { hi: 1, lo: 0 })
    );
}

#[test]
fn se_is_the_rounded_up_sampling_error_of_the_mean() -> Result<()> {
    // Constant utilities: no spread.
    let flat = SeMeasure::measure([5u64; 10])?;
    assert_eq!(flat.se, 0);
    // Two documents at 0 and 1: V = (2 * 1 - 1) / (4 * 1) = 0.25 in score^2,
    // so SE = 0.5.
    let pair = SeMeasure::measure([0, SCORE_SCALE])?;
    assert_eq!(pair.se, SCORE_SCALE / 2);
    // Matches the floating sample SE of the mean to within rounding up.
    let se = SeMeasure::measure(fq::synthetic_utilities())?;
    let u: Vec<f64> = fq::synthetic_utilities().map(|u| u as f64 / 1e18).collect();
    let n = u.len() as f64;
    let mean = u.iter().sum::<f64>() / n;
    let var = u.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0) / n;
    let float = var.sqrt() * 1e18;
    assert!((se.se as f64) >= float - 1e3 && (se.se as f64) <= float * (1.0 + 1e-9) + 1e3);
    assert_eq!(SeMeasure::from_sums(se.n, se.s1, se.s2)?, se);
    assert_eq!(SeMeasure::measure([1u64]).unwrap_err(), Error::Invalid("se needs two documents"));
    assert!(SeMeasure::measure([SCORE_SCALE + 1, 0]).is_err());
    Ok(())
}

#[test]
fn the_ladder_widens_to_one_significant_digit() -> Result<()> {
    let w = |x: u128| quant::ladder(x).unwrap();
    assert_eq!(w(21_000_000_000_000_000), 30_000_000_000_000_000);
    assert_eq!(w(10_000_000_000_000_000), 10_000_000_000_000_000);
    assert_eq!(w(10_000_000_000_000_001), 20_000_000_000_000_000);
    assert_eq!(w(1), 1);
    assert_eq!(w(SCORE_SCALE as u128), SCORE_SCALE);
    assert_eq!(w(5 * SCORE_SCALE as u128), SCORE_SCALE);
    assert!(quant::ladder(0).is_err());
    // Every ladder value is on the ladder, and the ladder has 163 values.
    let mut count = 0u32;
    let mut p = 1u64;
    loop {
        for d in 1..=9u64 {
            let v = d * p;
            if v > SCORE_SCALE {
                break;
            }
            assert!(quant::on_ladder(v));
            assert_eq!(quant::ladder(u128::from(v))?, v);
            count += 1;
        }
        if p == SCORE_SCALE {
            break;
        }
        p *= 10;
    }
    assert_eq!(u128::from(count), quant::LADDER_VALUES);
    assert!(!quant::on_ladder(11));
    // Testnet at n = 1000: b = 0.021 with 40 entrants gives w = 0.03.
    let tau = Tau::new(1000, quant::charged(40, true), 2000)?;
    assert_eq!(tau.floor_width(), 21_000_000_000_000_000);
    assert_eq!(quant::width(0, &tau)?, 30_000_000_000_000_000);
    assert_eq!(quant::grid_values(30_000_000_000_000_000)?, 34);
    // 2 SE above the floor sets the grid.
    assert_eq!(quant::width(16_000_000_000_000_000, &tau)?, 40_000_000_000_000_000);
    Ok(())
}

#[test]
fn grid_decimals_print_and_parse_exactly() -> Result<()> {
    let mut buf = [0u8; quant::MAX_DECIMAL_LEN];
    let w = 30_000_000_000_000_000;
    assert_eq!(quant::format_grid(840_000_000_000_000_000, w, &mut buf)?, "0.84");
    assert_eq!(quant::format_grid(0, w, &mut buf)?, "0.00");
    assert_eq!(quant::parse_grid("0.84", w)?, 840_000_000_000_000_000);
    assert!(quant::parse_grid("0.85", w).is_err());
    assert!(quant::parse_grid("0.840", w).is_err());
    assert!(quant::format_grid(850_000_000_000_000_000, w, &mut buf).is_err());
    assert_eq!(quant::format_grid(SCORE_SCALE, SCORE_SCALE, &mut buf)?, "1");
    assert_eq!(quant::format_grid(SCORE_SCALE, 1, &mut buf)?, "1.000000000000000000");
    assert_eq!(quant::format_grid(3, 1, &mut buf)?, "0.000000000000000003");
    Ok(())
}

// ---------------------------------------------------------------------------
// The sampler
// ---------------------------------------------------------------------------

/// Words from a fixed list, for the sampler's control flow.
struct Fixed(Vec<u128>, usize);

impl Words for Fixed {
    fn word(&mut self) -> u128 {
        let w = self.0[self.1];
        self.1 += 1;
        w
    }
}

#[test]
fn uniform_rejects_exactly_the_biased_tail() -> Result<()> {
    // M = 2^127 + 1: r = 2^128 mod M = 2^127 - 1, so x >= 2^128 - r is
    // rejected.
    let m = (1u128 << 127) + 1;
    let r = (u128::MAX % m + 1) % m;
    assert_eq!(r, (1u128 << 127) - 1);
    let cutoff = u128::MAX - r;
    let mut f = Fixed(vec![cutoff + 1, u128::MAX, cutoff], 0);
    assert_eq!(quant::uniform(&mut f, m)?, cutoff % m);
    assert_eq!(f.1, 3);
    // A power of two never rejects.
    let mut f = Fixed(vec![u128::MAX], 0);
    assert_eq!(quant::uniform(&mut f, 2)?, 1);
    // M = u128::MAX: 2^128 mod M = 1, so the word u128::MAX is rejected.
    let mut f = Fixed(vec![u128::MAX, 5], 0);
    assert_eq!(quant::uniform(&mut f, u128::MAX)?, 5);
    assert!(quant::uniform(&mut f, 0).is_err());
    Ok(())
}

#[test]
fn bern_exp_follows_the_series_exactly() -> Result<()> {
    // With den = 2 and num = 1, each inner test needs x < 1 (x = 0) and
    // y == 0. Words 0 (x = 0), 0 (y = 0 mod 1), then 1 (x = 1) stops at
    // k = 2: even, so false.
    let mut f = Fixed(vec![0, 0, 1, 0], 0);
    assert!(!quant::bern_exp(&mut f, 1, 2)?);
    // Stopping at once (x = 1): k = 1, true.
    let mut f = Fixed(vec![1, 0], 0);
    assert!(quant::bern_exp(&mut f, 1, 2)?);
    // num = 0 always succeeds; num > den is refused.
    let mut s = Stream::new(Digest([1; 32]));
    assert!((0..64).all(|_| quant::bern_exp(&mut s, 0, 5).unwrap()));
    assert!(quant::bern_exp(&mut s, 6, 5).is_err());
    // Exact: P(true) = sum_k odd P(K = k) with P(K = k) = g^(k-1)/(k-1)! - g^k/k!,
    // the series of exp(-g); compared with the empirical rate.
    for (num, den) in [(1u128, 4u128), (1, 2), (3, 4), (1, 1)] {
        let g = num as f64 / den as f64;
        let mut series = 0.0;
        let mut term = 1.0;
        for k in 1..40 {
            let next = term * g / k as f64;
            if k % 2 == 1 {
                series += term - next;
            }
            term = next;
        }
        assert!((series - (-g).exp()).abs() < 1e-12);
        let mut s = Stream::new(quant::noise_key(&SEED, &COHORT, &entrant(num as u8 + den as u8)));
        let draws = 100_000;
        let hits = (0..draws).filter(|_| quant::bern_exp(&mut s, num, den).unwrap()).count();
        let p = (-g).exp();
        let sd = (p * (1.0 - p) / draws as f64).sqrt();
        assert!((hits as f64 / draws as f64 - p).abs() < 5.0 * sd, "{num}/{den}");
    }
    Ok(())
}

/// `P[X = x]` of the discrete Laplace of scale `a / b`.
fn dlap_pmf(x: i64, a: f64, b: f64) -> f64 {
    let r = (-b / a).exp();
    (1.0 - r) / (1.0 + r) * r.powi(x.unsigned_abs() as i32)
}

fn chi_square(a: u128, b: u128, draws: usize, seed_tag: u8, assumed: (f64, f64)) -> f64 {
    let mut s = Stream::new(quant::noise_key(&SEED, &COHORT, &entrant(seed_tag)));
    let range = 12i64;
    let mut counts = vec![0u64; (2 * range + 1) as usize + 1];
    for _ in 0..draws {
        let x = quant::dlap(&mut s, a, b).unwrap() as i64;
        let i = if x.abs() > range { counts.len() - 1 } else { (x + range) as usize };
        counts[i] += 1;
    }
    let mut stat = 0.0;
    let mut tail = 1.0;
    for x in -range..=range {
        let p = dlap_pmf(x, assumed.0, assumed.1);
        tail -= p;
        let e = p * draws as f64;
        let o = counts[(x + range) as usize] as f64;
        stat += (o - e).powi(2) / e;
    }
    let e = tail * draws as f64;
    stat + (counts[counts.len() - 1] as f64 - e).powi(2) / e
}

#[test]
fn dlap_matches_the_exact_pmf() {
    // 10^6 seeded draws at scale 3/2 over 26 cells (25 degrees of freedom):
    // the 99.99% point of chi-square(25) is about 59.0.
    let stat = chi_square(3, 2, 1_000_000, 0x31, (3.0, 2.0));
    assert!(stat < 59.0, "chi-square {stat}");
    // Negative control: a sampler run at the wrong scale fails the test.
    let wrong = chi_square(5, 2, 1_000_000, 0x31, (3.0, 2.0));
    assert!(wrong > 59.0, "wrong-scale chi-square {wrong}");
}

/// `P[q = j w | S]` exactly, for the offset uniform on `[0, w)` and noise
/// discrete Laplace of scale `a / b`, in score units.
fn q_pmf(score: i64, j: i64, w: i64, a: f64, b: f64) -> f64 {
    // q = j w when j w <= S + eta + o < (j + 1) w.
    let mut p = 0.0;
    for o in 0..w {
        let lo = j * w - score - o;
        for eta in lo..lo + w {
            p += dlap_pmf(eta, a, b);
        }
    }
    p / w as f64
}

#[test]
fn neighbouring_scores_change_each_q_by_at_most_e_to_the_eps() {
    // A toy scale in score units: Delta = 1, eps_v = b / a.
    let (a, b) = (4.0, 2.0);
    let eps = b / a;
    let w = 3;
    let worst = |a_used: f64| {
        let mut worst: f64 = 0.0;
        for score in 0..6 {
            for j in -10..12 {
                let p = q_pmf(score, j, w, a_used, b);
                let q = q_pmf(score + 1, j, w, a_used, b);
                worst = worst.max((p / q).ln().abs());
            }
        }
        worst
    };
    assert!(worst(a) <= eps + 1e-9, "{}", worst(a));
    // Negative control: noise at half the scale exceeds the bound.
    assert!(worst(a / 2.0) > eps);
}

// ---------------------------------------------------------------------------
// Determinism and replay
// ---------------------------------------------------------------------------

fn specs() -> Vec<Spec> {
    vec![
        Spec::new(0x21, bp(9600)),
        Spec::new(0x22, bp(9500)),
        Spec::new(0x23, bp(9400)),
        Spec::new(0x24, bp(9300)),
    ]
}

fn finalized_with(specs: &[Spec], quant_params: QuantParams) -> Result<(ScorePackageV2, PublishedIndex)> {
    let mut m = fx::manifest(chain(), 7, 1000, sigma_permille(40), 0, specs);
    m.params.quant = quant_params;
    let index = fx::default_index(&m.references);
    let mut p = fx::package(&m, bp(9100), None, specs)?;
    finalize_fixture(&mut p, &index, &mut fx::spec_measure(specs, 1000, |_, _| false))?;
    Ok((p, index))
}

#[test]
fn identical_inputs_give_identical_records_in_any_order() -> Result<()> {
    let (a, _) = finalized_with(&specs(), FIXTURE_QUANT)?;
    let (b, _) = finalized_with(&specs(), FIXTURE_QUANT)?;
    assert_eq!(a.to_bytes()?, b.to_bytes()?);
    assert_eq!(result_of(&a)?.to_bytes()?, result_of(&b)?.to_bytes()?);
    // Replay from durable state: the recorded draws are the seed's.
    reward_v1::check_draws(&a, &fx::FIXTURE_SEED)?;
    let mut other = fx::FIXTURE_SEED;
    other[0] ^= 1;
    assert_eq!(reward_v1::check_draws(&a, &other).unwrap_err(), Error::Mismatch("quant draw"));
    // Each recorded draw is a function of the seed, the cohort and the
    // subject's content alone: no other entrant and no processing order
    // enters it.
    let tau = quant::Tau::new(a.cases, a.quant.charged, a.params.quant.eps_total_milli)?;
    let cohort = quant::CohortBinding(a.cohort_manifest);
    for e in &a.entrants {
        let d = quant::draw(&fx::FIXTURE_SEED, &cohort, &Subject::Entrant(e.content), a.quant.width, &tau)?;
        assert_eq!(e.draw, Some(QuantDraw::of(&d)));
    }
    // Another roster order is another cohort (another manifest digest), so
    // its draws are independent: no two cohorts share noise.
    let mut permuted = specs();
    permuted.swap(1, 3);
    let (c, _) = finalized_with(&permuted, FIXTURE_QUANT)?;
    assert_ne!(c.cohort_manifest, a.cohort_manifest);
    for e in &a.entrants {
        let f = c.entrants.iter().find(|x| x.content == e.content).expect("same content");
        assert_ne!(f.draw, e.draw);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// No exact value leaves the coordinator
// ---------------------------------------------------------------------------

#[test]
fn the_result_is_unchanged_by_exact_values_with_the_same_q() -> Result<()> {
    let (p, _) = finalized_with(&specs(), FIXTURE_QUANT)?;
    let mut changed = p.clone();
    // Exact scores move, statistics move, SE sums and draws are replaced:
    // the q values and bits stay.
    for e in &mut changed.entrants {
        e.score = e.score.map(|s| Score::new(s.get() - 7).unwrap());
        e.draw = None;
        e.public = None;
        e.cluster_stats = None;
    }
    changed.reference.score = Score::new(p.reference.score.get() + 3)?;
    changed.quant.s1 += 11;
    changed.quant.se += 5;
    changed.quant.reference_draw.offset ^= 1;
    changed.distance_monitor = DistanceMonitor::default();
    assert_eq!(result_of(&changed)?.to_bytes()?, result_of(&p)?.to_bytes()?);
    // Negative control: a quantizer without noise and offset (q = S) would
    // publish the exact scores, and the result would change with them.
    let mut exact = p.clone();
    exact.quant.width = 1;
    exact.quant.reference_q = exact.reference.score;
    for e in &mut exact.entrants {
        e.q = e.score;
    }
    let mut exact_moved = exact.clone();
    exact_moved.entrants[0].q = exact_moved.entrants[0].score.map(|s| Score::new(s.get() - 7).unwrap());
    assert_ne!(
        QuantizedResult::of_package(&exact)?.entrants,
        QuantizedResult::of_package(&exact_moved)?.entrants
    );
    Ok(())
}

fn encodings(value: u128) -> Vec<Vec<u8>> {
    let mut out = vec![value.to_string().into_bytes(), format!("{value:x}").into_bytes()];
    if let Ok(v) = u64::try_from(value) {
        out.push(v.to_le_bytes().to_vec());
        out.push(v.to_be_bytes().to_vec());
    }
    out.push(value.to_le_bytes().to_vec());
    out.push(value.to_be_bytes().to_vec());
    out
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn planted_exact_values_appear_in_no_public_byte() -> Result<()> {
    // Distinctive exact scores and their private quantization values.
    let specs = vec![
        Spec::new(0x21, Score::new(912_345_678_901_234_567)?),
        Spec::new(0x22, Score::new(934_567_890_123_456_789)?),
    ];
    let (p, _) = finalized_with(&specs, FIXTURE_QUANT)?;
    let result = result_of(&p)?;
    let signed = fx::signed(result.clone())?.to_bytes()?;
    let target = CalculationTargetV2::for_package(&p, None)?;
    let row = reward_v1::derive_row(&target, Some(&result), &fx::snapshot(chain(), 9, &specs))?;
    let public = [signed, row.to_bytes()?, target.to_bytes()?];
    let mut planted: Vec<u128> = vec![p.quant.s1, u128::from(p.quant.se)];
    for e in &p.entrants {
        planted.push(u128::from(e.score.unwrap().get()));
        let d = e.draw.unwrap();
        planted.push(u128::from(d.offset));
        planted.push(d.noise_magnitude);
    }
    for value in planted.into_iter().filter(|v| *v > 1 << 20) {
        for needle in encodings(value) {
            for bytes in &public {
                assert!(!contains(bytes, &needle), "planted {value} found");
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The bit budget
// ---------------------------------------------------------------------------

#[test]
fn the_budget_reproduces_the_rule_table() -> Result<()> {
    let (w, ok) = quant::worst_case(1000, 40, 2000, 8, 10)?;
    assert_eq!((w.total, w.executed, w.grid_values, ok), (746, 40, 34, true));
    assert_eq!(
        (w.per_entrant, w.references, w.clusters, w.promotion, w.width),
        (14, 12, 160, 6, 8)
    );
    // Without a predecessor the 2,000-document grid drops a ladder step:
    // that case (760) is the worst, not the one with a predecessor (747).
    let (w, ok) = quant::worst_case(2000, 40, 2000, 8, 10)?;
    assert_eq!((w.total, ok), (760, true));
    let (w, ok) = quant::worst_case(4000, 256, 2000, 8, 10)?;
    assert_eq!((w.total, ok), (4917, false));
    assert_eq!(quant::ceil_log2_factorial(0)?, 0);
    assert_eq!(quant::ceil_log2_factorial(1)?, 0);
    assert_eq!(quant::ceil_log2_factorial(3)?, 3);
    assert_eq!(quant::ceil_log2_factorial(4)?, 5);
    assert!(quant::ceil_log2_factorial(MAX_ROSTER + 1).is_err());
    // Negative control: an inflated alphabet (one more q value) costs bits.
    let honest = Budget::count(40, 30_000_000_000_000_000, true)?;
    let inflated = Budget::count(40, 29_000_000_000_000_000, true)?;
    assert!(inflated.total > honest.total || inflated.grid_values > honest.grid_values);
    Ok(())
}

/// Review 2026-11-05 (leak 1): the plan-time worst case covers cohorts with
/// no predecessor (every `R_0` cohort), whose smaller `m` can drop the grid
/// one ladder step and so cost more bits than the reference term saves. A
/// plan that passes can then never be refused at seal: every executed count
/// and both reference shapes, at any `SE`, fit under the worst case.
#[test]
fn the_worst_case_covers_every_roster_size_and_both_reference_shapes() -> Result<()> {
    for (n, cap) in [(1000u32, 13u16), (1000, 19), (1000, 40), (1000, 60), (2000, 40), (4000, 80)] {
        let (worst, _) = quant::worst_case(n, cap, 2000, 8, 10)?;
        for x in 0..=cap {
            for predecessor in [false, true] {
                let tau = Tau::new(n, quant::charged(x, predecessor), 2000)?;
                for se in [0u64, 5_000_000_000_000_000, 11_000_000_000_000_000, 40_000_000_000_000_000] {
                    let w = quant::width(se, &tau)?;
                    let seal = Budget::count(x, w, predecessor)?;
                    assert!(
                        seal.total <= worst.total,
                        "n={n} cap={cap} x={x} k>0={predecessor} se={se}: {} > {}",
                        seal.total,
                        worst.total
                    );
                }
            }
        }
    }
    // The reviewer's failing plan: n = 1000, cap 60, h = 11, F = 10. At
    // X = 59 with no predecessor the grid is 0.03, not 0.04, and the seal
    // would exceed the bound; the worst case now refuses the plan.
    let (_, ok) = quant::worst_case(1000, 60, 2000, 11, 10)?;
    let tau = Tau::new(1000, quant::charged(59, false), 2000)?;
    let seal = Budget::count(59, quant::width(11_000_000_000_000_000, &tau)?, false)?;
    assert!(!seal.within(1000, 11, 10), "the k = 0 seal is over the bound");
    assert!(!ok, "so the plan is refused up front");
    Ok(())
}

#[test]
fn manifests_and_seals_refuse_an_over_budget_cohort() -> Result<()> {
    // The manifest check is the plan-time worst case.
    let mut m = fx::manifest(chain(), 7, 1000, sigma_permille(40), 0, &specs());
    m.params.quant = TESTNET_QUANT;
    m.roster_cap = 42;
    m.to_bytes()?;
    m.roster_cap = 43;
    assert_eq!(m.to_bytes().unwrap_err(), Error::Invalid("bit budget"));
    // At seal the actual roster is rechecked: a budget that the worst case
    // never allowed refuses the package.
    let tight = QuantParams { identify_bits: 1, ..TESTNET_QUANT };
    assert_eq!(finalized_with(&specs(), tight).unwrap_err(), Error::Invalid("bit budget"));
    Ok(())
}

#[test]
fn the_result_recounts_its_budget() -> Result<()> {
    let (p, _) = finalized_with(&specs(), TESTNET_QUANT)?;
    let result = result_of(&p)?;
    result.validate()?;
    assert!(result.budget.within);
    let mut tampered = result.clone();
    tampered.budget.per_entrant -= 1;
    assert_eq!(tampered.validate().unwrap_err(), Error::Mismatch("bit budget"));
    let mut tampered = result.clone();
    tampered.budget.within = false;
    assert_eq!(tampered.validate().unwrap_err(), Error::Mismatch("bit budget"));
    Ok(())
}

// ---------------------------------------------------------------------------
// Ties, cutoff and bands from q alone
// ---------------------------------------------------------------------------

const W: u64 = 30_000_000_000_000_000;

fn params() -> RewardV1Params {
    RewardV1Params::new(sigma_permille(40), TESTNET_QUANT)
}

fn gated(cells: u64, cluster: u16) -> QEntrant {
    QEntrant { q: Some(cells * W), gated: true, cluster: Some(cluster) }
}

fn input<'a>(p: &'a RewardV1Params, entrants: &'a [QEntrant], predecessor: Option<u64>) -> QInput<'a> {
    QInput { params: p, width: W, reference_q: 28 * W, predecessor_q: predecessor.map(|c| c * W), entrants }
}

fn sum(credits: &[Fraction]) -> Fraction {
    let mut total = credits[0].clone();
    for c in &credits[1..] {
        total = total.add(c);
    }
    total.reduced()
}

#[test]
fn tied_clusters_share_their_summed_credit_equally() -> Result<()> {
    let p = params();
    let entrants = [gated(31, 0), gated(31, 1), gated(30, 2)];
    let o = q_outcome(&input(&p, &entrants, None))?;
    assert_eq!(o.mode, CutoffMode::Margin);
    // W_q is one cell here: the tied pair holds the top band's positions 0
    // and 1 and shares (c(0, 0) + c(0, 1)) / 2; the third is the next band.
    assert_eq!(o.band_width, Some(W));
    assert_eq!(o.bands.len(), 2);
    let credits: Vec<Fraction> = o.clusters.iter().map(|c| c.credit.clone()).collect();
    assert_eq!(credits[0], credits[1]);
    assert_eq!(o.clusters[0].positions, (0, 1));
    let base = reward_v1::band_credits(&[2, 1], DECAY_BPS)?;
    assert_eq!(credits[0], Fraction::new(base[0][0].add(&base[0][1]).num, base[0][0].add(&base[0][1]).den.mul_u64(2)).unwrap().reduced());
    assert_eq!(sum(&credits), Fraction::new(crate::bigint::BigUint::one(), crate::bigint::BigUint::one()).unwrap());
    // Equal shares give equal units; here the single leftover unit has the
    // third entrant's larger remainder.
    assert_eq!(o.units[0], o.units[1]);
    assert_eq!(o.units.iter().map(|&u| u32::from(u)).sum::<u32>(), 65535);
    // A lone tied pair splits 65535 into two equal halves: the leftover unit
    // of the equal remainders goes to the earlier roster member.
    let pair = [gated(31, 0), gated(31, 1)];
    let o = q_outcome(&input(&p, &pair, None))?;
    assert_eq!(o.units, vec![32768, 32767]);
    Ok(())
}

#[test]
fn two_heads_one_cell_apart_are_both_paid() -> Result<()> {
    let p = params();
    let entrants = [gated(31, 0), gated(30, 1)];
    let o = q_outcome(&input(&p, &entrants, None))?;
    assert!(o.units.iter().all(|&u| u > 0));
    assert!(o.clusters[0].credit > o.clusters[1].credit);
    Ok(())
}

#[test]
fn rank_reads_q_and_roster_order_only() {
    // Equal q: roster order. Higher q first.
    let entrants = [gated(30, 0), gated(31, 1), gated(30, 2)];
    assert_eq!(q_rank(&entrants), vec![1, 0, 2]);
    // Negative control: ranking by an exact score behind the q values
    // (here, larger for roster index 2) breaks the tie the other way and
    // fails the tie rule.
    let exact = [0.900_001_f64, 0.931, 0.900_002];
    let mut by_exact: Vec<usize> = (0..3).collect();
    by_exact.sort_by(|&a, &b| exact[b].partial_cmp(&exact[a]).unwrap());
    assert_ne!(by_exact, q_rank(&entrants));
}

#[test]
fn cutoffs_lie_on_the_grid_and_bound_every_eligible_q() -> Result<()> {
    let p = params();
    // Margin: T = 0.84 + 0.08 * 0.16 = 0.8528, so T_q = 0.87.
    let entrants = [gated(29, 0), gated(28, 1), gated(31, 2)];
    let o = q_outcome(&input(&p, &[entrants[0], QEntrant { cluster: None, ..entrants[1] }, entrants[2]], None))?;
    assert_eq!(o.cutoff, Some(29 * W));
    assert!(o.eligible.iter().all(|&i| entrants[i].q.unwrap() >= o.cutoff.unwrap()));
    // Fallback: C = 0.78 + 0.15 (0.84 - 0.78) = 0.789, so C_q = 0.81 <= max.
    let entrants = [gated(28, 0), gated(27, 1), QEntrant { q: Some(25 * W), gated: true, cluster: None }];
    let o = q_outcome(&input(&p, &entrants, Some(26)))?;
    assert_eq!((o.mode, o.cutoff), (CutoffMode::Fallback, Some(27 * W)));
    assert!(o.cutoff.unwrap() <= 28 * W);
    // Band edges are multiples of W_q, itself a whole number of cells.
    let bw = o.band_width.unwrap();
    assert_eq!(bw % W, 0);
    // q_Rk = 1: no target.
    let top = [gated(33, 0)];
    let o = q_outcome(&QInput { reference_q: SCORE_SCALE - SCORE_SCALE % W, ..input(&p, &top, None) });
    assert!(o.is_err() || o.unwrap().mode != CutoffMode::Fallback);
    let one = QInput { params: &p, width: SCORE_SCALE, reference_q: SCORE_SCALE, predecessor_q: None, entrants: &[] };
    assert_eq!(q_outcome(&one)?.mode, CutoffMode::NoTarget);
    // An off-grid q is refused.
    let off = [QEntrant { q: Some(29 * W + 1), gated: true, cluster: Some(0) }];
    assert_eq!(q_outcome(&input(&p, &off, None)).unwrap_err(), Error::Invalid("q off the grid"));
    // A published membership that names a later roster member is refused.
    let bad = [gated(31, 1), gated(31, 1)];
    assert_eq!(q_outcome(&input(&p, &bad, None)).unwrap_err(), Error::Mismatch("cluster first member"));
    Ok(())
}

// ---------------------------------------------------------------------------
// Results, promotion and the minimum-success gate
// ---------------------------------------------------------------------------

#[test]
fn a_result_with_other_units_or_off_grid_q_is_refused() -> Result<()> {
    let (p, _) = finalized_with(&specs(), TESTNET_QUANT)?;
    let result = result_of(&p)?;
    assert_eq!(result.digest()?, p.result);
    assert_eq!(QuantizedResult::from_bytes(&result.to_bytes()?)?, result);
    let paid = result.entrants.iter().position(|e| e.units > 1).unwrap();
    let other = result.entrants.iter().position(|e| e.units == 0).unwrap_or((paid + 1) % result.entrants.len());
    let mut tampered = result.clone();
    tampered.entrants[paid].units -= 1;
    tampered.entrants[other].units += 1;
    assert!(tampered.validate().is_err());
    let mut off = result.clone();
    if let EntrantOutcome::Scored { q, .. } = &mut off.entrants[0].outcome {
        *q = Score::new(q.get() + 1)?;
    }
    assert_eq!(off.validate().unwrap_err(), Error::Invalid("q off the grid"));
    // The signed length bound covers the largest roster, and a reader
    // refuses anything longer before allocating.
    assert!(QuantizedResult::max_signed_len()? <= sign::MAX_SIGNED_RECORD_LEN as u64);
    assert!(result.to_bytes()?.len() as u64 <= QuantizedResult::max_signed_len()?);
    Ok(())
}

#[test]
fn promotion_needs_the_exact_margin_and_a_margin_cohort() -> Result<()> {
    // R_k = 0.91, sigma = 0.04: T = 0.9172. A head just below T on exact
    // scores is never audited or promoted, whatever its q.
    let specs = [Spec::new(0x41, Score::new(bp(9172).get() - 1)?)];
    let m = fx::manifest(chain(), 7, 1000, sigma_permille(40), 0, &specs);
    let index = fx::default_index(&m.references);
    let mut p = fx::package(&m, bp(9100), None, &specs)?;
    finalize_fixture(&mut p, &index, &mut fx::spec_measure(&specs, 1000, |_, _| false))?;
    assert!(p.promotion_audits.is_empty());
    assert_eq!(p.promoted, None);
    // In a fallback cohort nobody is audited, even above the exact margin of
    // the reference that stays.
    let specs = [Spec::new(0x42, bp(9159))];
    let m = fx::manifest(chain(), 7, 1000, sigma_permille(40), 1, &specs);
    let index = fx::default_index(&m.references);
    let mut p = fx::package(&m, bp(9100), Some(bp(8775)), &specs)?;
    finalize_fixture(&mut p, &index, &mut fx::spec_measure(&specs, 1000, |_, _| false))?;
    assert_eq!(p.mode, CutoffMode::Fallback);
    assert!(p.promotion_audits.is_empty());
    Ok(())
}

#[test]
fn the_minimum_success_gate_withholds_above_k_n() -> Result<()> {
    let mut specs = specs();
    let k_n = 2;
    let gate = QuantParams { min_success_max_failed: k_n, ..FIXTURE_QUANT };
    specs[1].failed = k_n;
    specs[2].failed = k_n + 1;
    let (p, _) = finalized_with(&specs, gate)?;
    assert!(!p.entrants[1].withheld());
    assert!(p.entrants[2].withheld());
    assert_eq!(p.entrants[2].units, 0);
    let result = result_of(&p)?;
    assert!(matches!(result.entrants[1].outcome, EntrantOutcome::Scored { .. }));
    assert_eq!(result.entrants[2].outcome, EntrantOutcome::Withheld);
    assert_eq!((result.entrants[2].band_rank, result.entrants[2].units), (None, 0));
    // The withheld entrant still consumed its draw: every other subject's q
    // is the one it gets without the failures.
    specs[2].failed = 0;
    let (clean, _) = finalized_with(&specs, gate)?;
    for i in [0, 1, 3] {
        assert_eq!(clean.entrants[i].q, p.entrants[i].q);
    }
    assert_eq!(clean.entrants[2].draw, p.entrants[2].draw);
    // A package that hides the gate is refused.
    let mut hidden = p.clone();
    hidden.entrants[2].gate_failures.clear();
    assert_eq!(hidden.validate_inputs().unwrap_err(), Error::Mismatch("minimum-success gate"));
    Ok(())
}

#[test]
fn a_tampered_q_or_draw_is_refused_by_the_package() -> Result<()> {
    let (p, _) = finalized_with(&specs(), FIXTURE_QUANT)?;
    let mut q = p.clone();
    q.entrants[0].q = Some(Score::new(q.entrants[0].q.unwrap().get() + q.quant.width)?);
    assert_eq!(q.validate_inputs().unwrap_err(), Error::Mismatch("entrant q"));
    let mut w = p.clone();
    w.quant.width *= 2;
    assert!(w.validate_inputs().is_err());
    let mut se = p.clone();
    se.quant.se += 1;
    assert_eq!(se.validate_inputs().unwrap_err(), Error::Mismatch("quant se"));
    Ok(())
}

// ---------------------------------------------------------------------------
// The closure checked against the public result alone
// ---------------------------------------------------------------------------

#[test]
fn a_closure_checks_against_its_result_without_the_package() -> Result<()> {
    let specs = specs();
    let mut m = fx::manifest(chain(), 7, 1000, sigma_permille(40), 0, &specs);
    m.params.quant = FIXTURE_QUANT;
    let index = fx::default_index(&m.references);
    let mut p = fx::package(&m, bp(9100), None, &specs)?;
    finalize_fixture(&mut p, &index, &mut fx::spec_measure(&specs, 1000, |_, _| false))?;
    let target = (p.mode != CutoffMode::NoTarget)
        .then(|| CalculationTargetV2::for_package(&p, None))
        .transpose()?;
    let closure = fx::closure(&m, &p, target.as_ref(), &specs)?;
    let result = result_of(&p)?;
    assert_eq!(result.opened_epoch, m.opened_epoch);
    closure.check(&m, &p, target.as_ref())?;
    closure.check_result(&result)?;
    closure.check_schedule(&m)?;
    result.check_manifest(&m)?;
    let mut moved = result.clone();
    moved.entrants[0].commitment = Digest([3; 32]);
    assert_eq!(moved.check_manifest(&m).unwrap_err(), Error::Mismatch("result roster"));
    let mut reopened_at = result.clone();
    reopened_at.opened_epoch += 1;
    assert_eq!(reopened_at.check_manifest(&m).unwrap_err(), Error::Mismatch("result manifest"));
    // Another result, a dropped target or a changed opening is refused.
    let mut other = result.clone();
    other.revision += 1;
    assert_eq!(closure.check_result(&other).unwrap_err(), Error::Mismatch("closure result"));
    let mut untargeted = closure.clone();
    untargeted.target = if untargeted.target.is_some() { None } else { Some(Digest([1; 32])) };
    assert_eq!(untargeted.check_result(&result).unwrap_err(), Error::Mismatch("closure target"));
    let mut reopened = closure.clone();
    reopened.openings[0].nonce.0[0] ^= 1;
    assert_eq!(reopened.check_result(&result).unwrap_err(), Error::Mismatch("opening commitment"));
    // The coordinator's own check binds the same digest.
    let mut rebound = closure.clone();
    rebound.result = Digest([2; 32]);
    assert_eq!(rebound.check(&m, &p, target.as_ref()).unwrap_err(), Error::Mismatch("closure package"));
    Ok(())
}

/// Review 2026-11-05 (correctness 1): copies of one model under several
/// hotkeys share one draw, so they get one `q` and the best of `N` noisy
/// draws never places their cluster. The cluster's band, the cutoff, the
/// mode and the other entrant's units are those of the single submission,
/// and the cluster's units only split among its members.
#[test]
fn copies_under_several_hotkeys_share_one_q_and_gain_nothing() -> Result<()> {
    let honest = Spec::new(0x40, bp(9400));
    let original = Spec::new(0x31, bp(9600));
    let single = [original.clone(), honest.clone()];
    let mut copied = vec![original.clone(), honest.clone()];
    for tag in 0x32..0x3a {
        copied.push(Spec::new(tag, bp(9600)).copy_of(0x31));
    }
    let m1 = fx::manifest(chain(), 7, 1000, sigma_permille(40), 0, &single);
    let m2 = fx::manifest(chain(), 7, 1000, sigma_permille(40), 0, &copied);
    let (a, _) = fx::finalized(&m1, bp(9100), None, &single)?;
    let (b, _) = fx::finalized(&m2, bp(9100), None, &copied)?;
    // Every copy has the original's q in its own cohort.
    let q0 = b.entrants[0].q;
    assert!(q0.is_some());
    for e in &b.entrants[2..] {
        assert_eq!(e.q, q0, "a copy draws the original's q");
        assert_eq!(e.draw, b.entrants[0].draw);
    }
    // The cohorts differ (different manifests), so compare what places the
    // cluster: the same cutoff mode, and in each cohort the cluster sits in
    // the band of the original's q, not of a better copy.
    let (oa, ob) = (
        reward_v1::derive_units(&result_of(&a)?)?,
        reward_v1::derive_units(&result_of(&b)?)?,
    );
    assert_eq!(oa.mode, ob.mode);
    let head = ob.clusters.iter().find(|c| c.members.contains(&0)).expect("cluster");
    assert_eq!(head.members.len(), copied.len() - 1, "all copies in one cluster");
    for &i in &head.members {
        assert_eq!(b.entrants[i].q, q0);
    }
    // Within one cohort, the copies change nothing but the split: removing
    // them from the public result (same grid, same references) leaves the
    // cutoff, the mode, the cluster's band and the honest entrant's units,
    // and the cluster's total is what the original alone would get.
    let result = result_of(&b)?;
    let mut entrants = result.q_entrants();
    let mut alone = entrants.clone();
    for e in &mut alone[2..] {
        *e = QEntrant { q: None, gated: false, cluster: None };
    }
    let input = |entrants: &[QEntrant]| -> Result<reward_v1::QOutcome> {
        q_outcome(&QInput {
            params: &result.params,
            width: result.width,
            reference_q: result.current.q.get(),
            predecessor_q: result.predecessor.as_ref().map(|p| p.q.get()),
            entrants,
        })
    };
    let with = input(&entrants)?;
    let without = input(&alone)?;
    assert_eq!((with.mode, with.cutoff), (without.mode, without.cutoff));
    assert_eq!(with.band_rank[0], without.band_rank[0]);
    assert_eq!(with.units[1], without.units[1]);
    let cluster_units: u32 = head.members.iter().map(|&i| u32::from(with.units[i])).sum();
    assert_eq!(cluster_units, u32::from(without.units[0]));
    // Negative control: one copy a grid cell higher (what an independent
    // draw could give) would lift the cluster.
    entrants[2].q = entrants[2].q.map(|q| q + result.width);
    let lifted = input(&entrants);
    assert!(
        lifted.is_err() || lifted.as_ref().is_ok_and(|o| o.clusters != with.clusters),
        "a higher copy changes the placement"
    );
    // Negative control: under hotkey-keyed draws the copies would differ.
    let tau = Tau::new(1000, b.quant.charged, b.params.quant.eps_total_milli)?;
    let cohort = quant::CohortBinding(b.cohort_manifest);
    let d1 = quant::draw(&fx::FIXTURE_SEED, &cohort, &Subject::Entrant(Digest([0x31; 32])), b.quant.width, &tau)?;
    let d2 = quant::draw(&fx::FIXTURE_SEED, &cohort, &Subject::Entrant(Digest([0x32; 32])), b.quant.width, &tau)?;
    assert_ne!(d1, d2, "distinct subjects draw independently");
    Ok(())
}
