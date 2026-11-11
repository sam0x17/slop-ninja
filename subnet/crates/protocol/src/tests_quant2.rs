//! `quant-v2`: golden vectors, the discrete Gaussian sampler against its
//! exact law, the privacy accounting, the scale, the information bound and
//! the Bell table, the plan-time worst case, the rule-tagged encodings and
//! their fail-closed decoding by older readers, and a `quant-v2` cohort
//! through finalization and its public result.

use crate::bigint::BigUint;
use crate::codec::{Canonical, Decoder, Encoder};
use crate::fixtures_quant2::{self as fq2, MAINNET_QUANT2, TEMPLATE_TERMS, TESTNET_QUANT2, key};
use crate::fixtures_reward_v1::{self as fx, Spec, bp, chain, sigma_permille};
use crate::quant::{self, CountedTerms, Stream, U256};
use crate::quant2::{self, BELL_BITS, Budget2, MAX_SIGMA_PPB, Scale, UNIT, gaussian, log2_upper_q20};
use crate::reward_v1::{self, NoiseScale, PreviewInputs, PreviewValue};
use crate::*;

const GOLDEN: &str = include_str!("../fixtures/golden-quant-v2.txt");

#[test]
fn golden_quant_v2_report_is_unchanged() -> Result<()> {
    let report = fq2::golden_report()?;
    for (i, (a, b)) in report.lines().zip(GOLDEN.lines()).enumerate() {
        assert_eq!(a, b, "golden line {}", i + 1);
    }
    assert_eq!(report, GOLDEN);
    Ok(())
}

/// Prints the golden report, to regenerate `fixtures/golden-quant-v2.txt`
/// after a deliberate change: `cargo test -p slop-ninja-protocol --lib
/// print_golden_quant_v2_report -- --ignored --nocapture`.
#[test]
#[ignore]
fn print_golden_quant_v2_report() -> Result<()> {
    print!("{}", fq2::golden_report()?);
    Ok(())
}

/// The exact discrete Gaussian pmf over `[-range, range]`.
fn pmf(s: u64, range: i64) -> Vec<(i64, f64)> {
    let s2 = (s as f64) * (s as f64);
    let raw: Vec<(i64, f64)> = (-range..=range).map(|y| (y, (-(y as f64).powi(2) / (2.0 * s2)).exp())).collect();
    let z: f64 = raw.iter().map(|(_, p)| p).sum();
    raw.into_iter().map(|(y, p)| (y, p / z)).collect()
}

/// The chi-square upper point at probability 0.001 for `df` degrees of
/// freedom (Wilson-Hilferty).
fn chi2_crit(df: f64) -> f64 {
    let z = 3.090_232;
    df * (1.0 - 2.0 / (9.0 * df) + z * (2.0 / (9.0 * df)).sqrt()).powi(3)
}

#[test]
fn the_sampler_follows_the_discrete_gaussian() -> Result<()> {
    for s in [1u64, 2, 3, 5, 10, 30] {
        let draws = 100_000usize;
        let mut stream = Stream::new(key(&format!("chi2.{s}")));
        let range = 12 * s as i64;
        let mut counts = std::collections::BTreeMap::<i64, u64>::new();
        let (mut sum, mut sum2) = (0f64, 0f64);
        for _ in 0..draws {
            let y = gaussian(&mut stream, s)?;
            let y = i64::try_from(y).expect("small draw");
            assert!(y.abs() <= range, "draw {y} beyond 12 sigma at s={s}");
            *counts.entry(y).or_default() += 1;
            sum += y as f64;
            sum2 += (y as f64).powi(2);
        }
        // Bins with an expected count of at least 5; the tails merged.
        let law = pmf(s, range);
        let (mut stat, mut bins) = (0f64, 0usize);
        let (mut tail_e, mut tail_o) = (0f64, 0f64);
        for (y, p) in &law {
            let e = p * draws as f64;
            let o = counts.get(y).copied().unwrap_or(0) as f64;
            if e >= 5.0 {
                stat += (o - e).powi(2) / e;
                bins += 1;
            } else {
                tail_e += e;
                tail_o += o;
            }
        }
        if tail_e > 0.0 {
            stat += (tail_o - tail_e).powi(2) / tail_e;
            bins += 1;
        }
        let df = (bins - 1) as f64;
        assert!(stat < chi2_crit(df), "s={s}: chi2 {stat:.1} on {df} df");
        // Mean near 0, variance below s^2 (CKS20) within sampling error.
        let mean = sum / draws as f64;
        let var = sum2 / draws as f64 - mean * mean;
        let s2 = (s * s) as f64;
        let exact_var: f64 = law.iter().map(|(y, p)| (*y as f64).powi(2) * p).sum();
        // Below s^2 (CKS20); for s >= 2 the gap is below f64 resolution.
        assert!(exact_var <= s2 * (1.0 + 1e-12), "s={s}: exact variance {exact_var} above s^2");
        assert!(mean.abs() < 5.0 * (s2 / draws as f64).sqrt(), "s={s}: mean {mean}");
        assert!((var - exact_var).abs() < 6.0 * exact_var * (2.0 / draws as f64).sqrt() + 1e-9, "s={s}: var {var} vs {exact_var}");
        // Symmetry: y and -y within sampling error.
        for y in 1..=(2 * s as i64) {
            let (a, b) = (counts.get(&y).copied().unwrap_or(0) as f64, counts.get(&-y).copied().unwrap_or(0) as f64);
            assert!((a - b).abs() < 6.0 * (a + b).sqrt() + 6.0, "s={s}: y={y} {a} vs {b}");
        }
    }
    Ok(())
}

#[test]
fn draws_and_words_are_deterministic() -> Result<()> {
    for s in [1u64, 7, 4_015_595, MAX_SIGMA_PPB] {
        let run = || -> Result<(Vec<i128>, u64)> {
            let mut st = Stream::new(key(&format!("det.{s}")));
            let v = (0..50).map(|_| gaussian(&mut st, s)).collect::<Result<Vec<_>>>()?;
            Ok((v, st.consumed()))
        };
        assert_eq!(run()?, run()?);
    }
    // Out of the sampler's domain.
    let mut st = Stream::new(key("domain"));
    assert_eq!(gaussian(&mut st, 0).unwrap_err(), Error::OutOfRange("gaussian scale"));
    assert_eq!(gaussian(&mut st, MAX_SIGMA_PPB + 1).unwrap_err(), Error::OutOfRange("gaussian scale"));
    Ok(())
}

#[test]
fn bern_exp_rational_matches_its_probability() -> Result<()> {
    for (num, den) in [(0u128, 1u128), (1, 3), (1, 1), (3, 2), (7, 3), (5, 1)] {
        let trials = 40_000u32;
        let mut st = Stream::new(key(&format!("bern.{num}.{den}")));
        let mut hits = 0u32;
        for _ in 0..trials {
            hits += u32::from(quant2::bern_exp_rational(&mut st, U256::from_u128(num), den)?);
        }
        let p = (-(num as f64) / den as f64).exp();
        let observed = f64::from(hits) / f64::from(trials);
        let se = (p * (1.0 - p) / f64::from(trials)).sqrt().max(1e-6);
        assert!((observed - p).abs() < 5.0 * se + 1e-4, "{num}/{den}: {observed} vs {p}");
    }
    assert_eq!(
        quant2::bern_exp_rational(&mut Stream::new(key("bern.zero")), U256::from_u128(1), 0).unwrap_err(),
        Error::Invalid("bernoulli parameter")
    );
    Ok(())
}

/// `D_alpha(P || Q)` of the discrete Gaussian against its shift by `delta`,
/// from the exact law in log space (both share one normalizer).
fn renyi(s: u64, delta: i64, order: f64) -> f64 {
    let s2 = (s as f64) * (s as f64);
    let range = 60 * s as i64 + delta;
    let log_p = |y: i64| -(y as f64).powi(2) / (2.0 * s2);
    let log_z = (-range..=range).map(|y| log_p(y).exp()).sum::<f64>().ln();
    let terms: Vec<f64> = (-range..=range)
        .map(|y| order * (log_p(y) - log_z) + (1.0 - order) * (log_p(y - delta) - log_z))
        .collect();
    let max = terms.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let sum: f64 = terms.iter().map(|t| (t - max).exp()).sum();
    (max + sum.ln()) / (order - 1.0)
}

#[test]
fn the_release_is_rho_zcdp() -> Result<()> {
    // Renyi divergence of N_Z(0, s^2) against a shift by Delta stays below
    // alpha Delta^2 / (2 s^2) (CKS20), the zCDP bound of one value.
    for (s, delta) in [(1u64, 1i64), (3, 1), (3, 2), (5, 5), (8, 3)] {
        for order in [1.5f64, 2.0, 4.0, 10.0] {
            let d = renyi(s, delta, order);
            let bound = order * (delta * delta) as f64 / (2.0 * (s * s) as f64);
            assert!(d <= bound + 1e-9, "s={s} delta={delta} order={order}: {d} > {bound}");
        }
    }
    // The scale meets the privacy limit exactly and publishes rho rounded up.
    for (n, m, rho) in [(1000u32, 42u32, 500u32), (4000, 258, 500), (8000, 258, 500), (1000, 10, 250)] {
        let params = QuantParamsV2 { headroom_pub_ppb: 0, rho_max_milli: rho, ..TESTNET_QUANT2 };
        let s = Scale::derive(n, m, &params)?;
        let delta = u128::from(quant2::sensitivity_ppb(n)?);
        let cost = u128::from(m) * delta * delta;
        let s2 = u128::from(s.sigma).pow(2);
        assert!(1000 * cost <= 2 * s2 * u128::from(rho));
        // One step below the floor breaks the limit.
        let below = u128::from(s.floor - 1).pow(2);
        assert!(1000 * cost > 2 * below * u128::from(rho));
        assert_eq!(u128::from(s.rho_micro), (1_000_000 * cost).div_ceil(2 * s2));
        assert!(s.rho_micro <= u64::from(rho) * 1000);
    }
    Ok(())
}

#[test]
fn the_scale_matches_the_model_rows() -> Result<()> {
    // (X, n, H) -> (sigma_T, sigma_rho, w in thousandths).
    for (x, n, h, target, floor, w_milli) in [
        (256u16, 4000u32, 62_000_000u32, 4_000_000u64, 4_015_595u64, 5u64),
        (40, 4000, 62_000_000, 4_000_000, 1_620_186, 4),
        (40, 1000, 153_000_000, 9_901_385, 6_480_741, 10),
        (8, 1000, 153_000_000, 9_901_385, 3_162_278, 10),
        (256, 8000, 62_000_000, 4_000_000, 2_007_798, 4),
    ] {
        let params = QuantParamsV2 { headroom_pub_ppb: h, ..MAINNET_QUANT2 };
        let s = Scale::derive(n, quant::charged(x, true), &params)?;
        assert_eq!((s.target, s.floor), (target, floor), "({x}, {n})");
        assert_eq!(s.sigma, target.max(floor));
        assert_eq!(s.width, w_milli * SCORE_SCALE / 1000, "({x}, {n})");
        // Total release SD within the target unless the floor binds.
        let total = ((s.sigma as f64).powi(2) + (s.width as f64 / UNIT as f64).powi(2) / 6.0).sqrt();
        let goal = f64::from(params.noise_target_h_milli) * f64::from(h) / 1000.0;
        assert!(s.floor > s.target || total <= goal, "({x}, {n}): {total} > {goal}");
    }
    Ok(())
}

#[test]
fn sigma_target_is_monotone_and_never_refuses() -> Result<()> {
    assert_eq!(quant2::sigma_target(70, 0)?, 0);
    let mut last = 0;
    for h in (0..=200u32).map(|k| k * 1_000_000) {
        let s = quant2::sigma_target(70, h)?;
        assert!(s >= last && s <= u64::from(h));
        last = s;
        // The defining inequality holds at s and fails at s + 1.
        let lhs = |s: u64| {
            let w = if s == 0 { 0 } else { u128::from(quant::ladder(u128::from(s) * u128::from(UNIT)).unwrap() / UNIT) };
            6_000_000 * u128::from(s).pow(2) + 1_000_000 * w * w
        };
        let bound = 6 * (70u128 * u128::from(h)).pow(2);
        assert!(lhs(s) <= bound);
        if s < u64::from(h) {
            assert!(lhs(s + 1) > bound, "h={h}");
        }
    }
    Ok(())
}

#[test]
fn log2_upper_q20_bounds_log2_tightly() -> Result<()> {
    let mut checked = 0;
    for den in [1u64, 3, 1_000_003, 16_125_000_000_000, 98_000_000_000_000, (1 << 40) + 7] {
        for ratio_num in [1u64, 2, 3, 5, 17, 1000, 123_457] {
            let num = den.saturating_mul(ratio_num) + ratio_num % 3;
            if num < den || num >= 1 << 62 {
                continue;
            }
            let got = log2_upper_q20(num, den)? as f64 / f64::from(1u32 << 20);
            let exact = (num as f64 / den as f64).log2();
            assert!(got >= exact - 1e-12, "{num}/{den}: {got} < {exact}");
            assert!(got - exact <= 2f64.powi(-19) + 1e-12, "{num}/{den}: {got} - {exact}");
            checked += 1;
        }
    }
    assert!(checked > 30);
    assert_eq!(log2_upper_q20(1, 1)?, 1);
    assert_eq!(log2_upper_q20(2, 1)?, (1 << 20) + 1);
    assert!(log2_upper_q20(1, 2).is_err() && log2_upper_q20(1 << 62, 1).is_err() && log2_upper_q20(1, 0).is_err());
    Ok(())
}

#[test]
fn bell_bits_match_the_bell_triangle() {
    let mut row = vec![BigUint::one()];
    let mut bells = vec![BigUint::one()];
    for _ in 1..=usize::from(MAX_ROSTER) {
        let mut next = vec![row.last().expect("row").clone()];
        for v in &row {
            let sum = next.last().expect("next").add(v);
            next.push(sum);
        }
        row = next;
        bells.push(row[0].clone());
    }
    for (x, bell) in bells.iter().enumerate() {
        // ceil(log2 N) = bit length of N - 1.
        let bits = bell.checked_sub(&BigUint::one()).expect("bell >= 1").bits();
        assert_eq!(u64::from(BELL_BITS[x]), bits, "Bell({x})");
    }
}

#[test]
fn the_worst_case_holds_at_every_seal() -> Result<()> {
    for (n, limit, params) in [(1000u32, 40u16, TESTNET_QUANT2), (4000, 256, MAINNET_QUANT2)] {
        let (worst, ok) = quant2::worst_case_terms(n, limit, &params, &TEMPLATE_TERMS)?;
        assert!(ok);
        // Attained at the roster limit with a predecessor.
        let floor = Scale::floor_only(n, quant::charged(limit, true), &params)?;
        assert_eq!(worst.total, quant2::cohort_bits(limit, true, n, &floor, &TEMPLATE_TERMS)?.total);
        // Any signed H, roster size and reference shape stays within it.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..200 {
            let h = (next() % 1_000_000_001) as u32;
            let x = (next() % (u64::from(limit) + 1)) as u16;
            let k = next() % 2 == 0;
            let sealed = QuantParamsV2 { headroom_pub_ppb: h, ..params };
            let scale = Scale::derive(n, quant::charged(x, k), &sealed)?;
            let bits = quant2::cohort_bits(x, k, n, &scale, &TEMPLATE_TERMS)?;
            assert!(bits.total <= worst.total);
            assert!(bits.within(n, params.identify_bits, params.budget_factor));
        }
    }
    // The model totals: testnet 384 of 800 at worst, mainnet 2,828 of 3,200.
    assert_eq!(quant2::worst_case_terms(1000, 40, &TESTNET_QUANT2, &TEMPLATE_TERMS)?.0.total, 384);
    assert_eq!(quant2::worst_case_terms(4000, 256, &MAINNET_QUANT2, &TEMPLATE_TERMS)?.0.total, 2828);
    // A plan whose floor exceeds one score unit is refused.
    assert_eq!(
        quant2::worst_case_terms(2, 256, &TESTNET_QUANT2, &CountedTerms::NONE).unwrap_err(),
        Error::OutOfRange("noise scale above one score unit")
    );
    Ok(())
}

fn encoded<T: Canonical>(v: &T) -> Vec<u8> {
    let mut e = Encoder::new();
    v.encode_to(&mut e);
    e.into_bytes()
}

fn decoded<T: Canonical>(bytes: &[u8]) -> Result<T> {
    let mut d = Decoder::new(bytes);
    let v = T::decode_from(&mut d)?;
    d.finish()?;
    Ok(v)
}

#[test]
fn quant_blocks_round_trip_and_old_readers_fail_closed() -> Result<()> {
    // V1 encodes exactly as the bare parameters.
    let v1 = QuantBlock::V1(fx::FIXTURE_QUANT);
    assert_eq!(encoded(&v1), encoded(&fx::FIXTURE_QUANT));
    assert_eq!(decoded::<QuantBlock>(&encoded(&v1))?, v1);
    let v2 = QuantBlock::V2(TESTNET_QUANT2);
    let bytes = encoded(&v2);
    assert_eq!(&bytes[..5], &[0, 0, 0, 0, 2]);
    assert_eq!(decoded::<QuantBlock>(&bytes)?, v2);
    // A reader from before quant-v2 decodes eps 0, which it refuses.
    let mut d = Decoder::new(&bytes);
    let old = QuantParams::decode_from(&mut d)?;
    assert_eq!(old.eps_total_milli, 0);
    assert_eq!(old.validate().unwrap_err(), Error::OutOfRange("quant.eps_total_milli"));
    // An unknown rule tag after the marker is refused.
    let mut bad = bytes.clone();
    bad[4] = 3;
    assert_eq!(decoded::<QuantBlock>(&bad).unwrap_err(), Error::InvalidTag { field: "quant_block", tag: 3 });
    // Validation of the v2 block.
    for broken in [
        QuantParamsV2 { noise_target_h_milli: 0, ..TESTNET_QUANT2 },
        QuantParamsV2 { noise_target_h_milli: 1001, ..TESTNET_QUANT2 },
        QuantParamsV2 { rho_max_milli: 0, ..TESTNET_QUANT2 },
        QuantParamsV2 { headroom_pub_ppb: 1_000_000_001, ..TESTNET_QUANT2 },
        QuantParamsV2 { identify_bits: 0, ..TESTNET_QUANT2 },
        QuantParamsV2 { budget_factor: u16::MIN, ..TESTNET_QUANT2 },
    ] {
        assert!(broken.validate().is_err(), "{broken:?}");
    }
    // The rule tag of a result.
    assert_eq!(QuantRule::from_tag(2), Some(QuantRule::QuantV2));
    assert_eq!(QuantRule::QuantV2.name(), "quant-v2");
    Ok(())
}

/// Finalizes the synthetic quant-v2 cohort and returns it with its index.
fn v2_cohort(specs: &[Spec], reward_rule: RewardRule) -> Result<(ScorePackageV2, PublishedIndex)> {
    let mut m = fx::manifest(chain(), 9, 1000, sigma_permille(40), 1, specs);
    m.reward_rule = reward_rule;
    m.params = RewardV1Params::new(m.params.sigma, TESTNET_QUANT2);
    m.to_bytes()?;
    let index = fx::default_index(&m.references);
    let mut p = fx::package(&m, bp(8470), Some(bp(8400)), specs)?;
    let mut measure = fx::spec_measure(specs, 1000, |h, o| specs[h].family == specs[o].family);
    fx::finalize_fixture(&mut p, &index, &mut measure)?;
    Ok((p, index))
}

fn specs() -> Vec<Spec> {
    vec![
        Spec::new(0x61, bp(9500)),
        Spec::new(0x62, bp(9300)),
        Spec::new(0x63, bp(9200)),
        Spec::new(0x64, bp(9000)),
        Spec::new(0x65, bp(9600)).copy_of(0x61),
    ]
}

#[test]
fn a_quant_v2_cohort_finalizes_and_its_result_recomputes() -> Result<()> {
    for rule in [RewardRule::RewardV2, RewardRule::RewardV3] {
        let (p, index) = v2_cohort(&specs(), rule)?;
        p.to_bytes()?;
        reward_v1::check_outcome(&p, &index)?;
        reward_v1::check_draws(&p, &fx::FIXTURE_SEED)?;
        let q = p.quant.v2()?;
        let scale = q.scale();
        scale.check(p.cases, q.charged, &TESTNET_QUANT2)?;
        // Every draw is on the lattice and q reads S only through S_ppb.
        for e in p.entrants.iter().filter(|e| e.score.is_some()) {
            let draw = e.draw.expect("drawn").draw()?;
            assert!(quant2::draw_on_lattice(&draw, &scale));
            let s = e.score.expect("scored").get();
            let floor_ppb = s - s % UNIT;
            assert_eq!(quant::quantize(s, &draw, scale.width)?, quant::quantize(floor_ppb, &draw, scale.width)?);
        }
        let result = fx::result_of(&p)?;
        result.validate()?;
        assert_eq!(result.quant_rule, QuantRule::QuantV2);
        let BudgetBlock::V2(b) = result.budget else { panic!("v2 budget") };
        assert!(b.within && b.sigma_ppb == q.sigma_ppb && b.rho_micro == q.rho_micro);
        let bytes = result.to_bytes()?;
        assert!(bytes.len() as u64 <= QuantizedResult::max_signed_len()?);
        assert_eq!(QuantizedResult::from_bytes(&bytes)?, result);
        // Pre-finalized values equal the package's, with no reference
        // measurement: the sealed scale alone fixes the grid.
        let noise = NoiseScale::Gaussian(scale);
        let cohort = quant::CohortBinding(p.cohort_manifest);
        for e in p.entrants.iter().filter(|e| e.score.is_some()) {
            let preview = reward_v1::preview_q(&PreviewInputs {
                seed: &fx::FIXTURE_SEED,
                cohort,
                subject: quant::Subject::Entrant(e.content),
                score: e.score.expect("scored"),
                failed: Some((e.failed, TESTNET_QUANT2.min_success_max_failed)),
                noise,
            })?;
            let expected = if e.withheld() { PreviewValue::Withheld } else { PreviewValue::Q(e.q.expect("q").get()) };
            assert_eq!(preview.value(), expected);
        }
    }
    Ok(())
}

#[test]
fn mixed_rule_records_are_refused() -> Result<()> {
    let (p, _) = v2_cohort(&specs(), RewardRule::RewardV3)?;
    // A quant-v1 record under quant-v2 parameters.
    let mut mixed = p.clone();
    mixed.quant = QuantRecordBlock::V1(QuantRecord::unset());
    assert_eq!(mixed.check_quant().unwrap_err(), Error::Mismatch("quant rule"));
    // A result whose rule tag disagrees with its parameters or budget.
    let result = fx::result_of(&p)?;
    let mut wrong = result.clone();
    wrong.quant_rule = QuantRule::QuantV1;
    assert_eq!(wrong.validate().unwrap_err(), Error::Mismatch("quant rule"));
    let mut wrong = result.clone();
    wrong.budget = BudgetBlock::V1(BudgetRecord::of(&quant::Budget::default(), true)?);
    assert_eq!(wrong.validate().unwrap_err(), Error::Mismatch("quant rule"));
    // A tampered scale, rho, width or draw.
    let mut tampered = p.clone();
    tampered.quant.v2_mut()?.sigma_ppb += 1;
    assert_eq!(tampered.check_quant().unwrap_err(), Error::Mismatch("noise scale"));
    let mut tampered = p.clone();
    tampered.quant.v2_mut()?.rho_micro += 1;
    assert_eq!(tampered.check_quant().unwrap_err(), Error::Mismatch("noise scale"));
    let mut tampered = result.clone();
    tampered.width *= 2;
    assert_eq!(tampered.validate().unwrap_err(), Error::Mismatch("grid width"));
    let mut tampered = p.clone();
    tampered.quant.v2_mut()?.reference_draw.offset += 1;
    assert_eq!(tampered.check_quant().unwrap_err(), Error::Invalid("draw off the noise lattice"));
    let mut tampered = result.clone();
    tampered.budget.v2_mut()?.noisy += 1;
    assert_eq!(tampered.validate().unwrap_err(), Error::Mismatch("bit budget"));
    // The package and result encodings of quant-v2 carry their markers, so a
    // quant-v1 reader of the package record misparses S1 as u128::MAX.
    let bytes = encoded(&p.quant);
    assert_eq!(&bytes[..17], &[[0xffu8; 16].as_slice(), &[2]].concat()[..]);
    let rbytes = encoded(&result.budget);
    assert_eq!((rbytes[0], rbytes[1], rbytes[2]), (0xff, 0xff, 2));
    Ok(())
}

#[test]
fn quant_v1_inputs_are_refused_under_quant_v2() -> Result<()> {
    let specs = specs();
    let mut m = fx::manifest(chain(), 9, 1000, sigma_permille(40), 1, &specs);
    m.params = RewardV1Params::new(m.params.sigma, TESTNET_QUANT2);
    let index = fx::default_index(&m.references);
    let mut p = fx::package(&m, bp(8470), Some(bp(8400)), &specs)?;
    let se = quant::SeMeasure::measure((0..1000).map(|_| SCORE_SCALE / 2))?;
    let mut measure = fx::spec_measure(&specs, 1000, |_, _| false);
    let inputs = reward_v1::QuantInputs::V1 { seed: fx::FIXTURE_SEED, se };
    assert_eq!(reward_v1::finalize(&mut p, &index, &mut measure, &inputs).unwrap_err(), Error::Mismatch("quant inputs"));
    Ok(())
}

#[test]
fn the_seal_counts_the_information_bound() -> Result<()> {
    let (p, _) = v2_cohort(&specs(), RewardRule::RewardV3)?;
    let q = p.quant.v2()?;
    let b = Budget2::count(p.executed(), true, p.cases, &q.scale())?;
    assert_eq!(b.deterministic, 3 * u32::from(p.executed()));
    assert_eq!(b.clusters, u32::from(BELL_BITS[usize::from(p.executed())]));
    // The float form of the bound, per value: 1/2 log2(1 + V / sigma^2).
    let v = quant2::value_variance_bound(p.cases)? as f64;
    let s2 = (q.sigma_ppb as f64).powi(2);
    let per_value = 0.5 * (1.0 + v / s2).log2();
    let float_bits = f64::from(q.charged) * per_value;
    let counted = b.noisy_q20 as f64 / f64::from(1u32 << 20);
    assert!(counted >= float_bits);
    assert!(counted <= float_bits + f64::from(q.charged) / f64::from(1u32 << 18) + 1.0);
    Ok(())
}
