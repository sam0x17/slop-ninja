//! The `quant-v2` golden report: fixed keys through every primitive of
//! [`crate::quant2`] (the Bernoulli and discrete Gaussian samplers, the
//! scale, the grid, the information bound and the Bell table), the bit
//! budgets of the testnet and mainnet templates, quantization through the
//! draws, the q-only band outcomes on `quant-v2` grids, and a full synthetic
//! `quant-v2` result. It is compared byte for byte against
//! `fixtures/golden-quant-v2.txt` on native targets and inside a
//! `wasm32-unknown-unknown` build.

use std::fmt::Write as _;

use crate::fixtures_reward_v1::{self as fx, Spec, bp, chain, sigma_permille};
use crate::quant::{self, CountedTerms, Stream, Subject, U256, dlap};
use crate::quant2::{
    self, BELL_BITS, Budget2, Scale, UNIT, bern_exp_rational, gaussian, log2_upper_q20, noisy_bits,
    sigma_floor, sigma_target,
};
use crate::reward_v1::{Placement, QEntrant, QInput, q_outcome};
/// `F` of both templates.
const TEMPLATE_F: u16 = 10;

use crate::{
    Digest, QuantBlock, QuantParamsV2, QuantizedResult, Record, Result, RewardRule, RewardV1Params, SCORE_SCALE,
    domain_hash,
};

/// The report's fixed seed.
pub const SEED: [u8; 32] = [0x2b; 32];

/// The report's fixed cohort binding (a manifest digest).
pub const COHORT: quant::CohortBinding = quant::CohortBinding(Digest([0x3d; 32]));

/// The testnet template's `quant-v2` block: `t_H` 0.07, `rho_max` 0.5,
/// `h` 8, `F` 10, and `H` 0.153 (the released `q` of `R_0` near 0.847).
pub const TESTNET_QUANT2: QuantParamsV2 = QuantParamsV2 {
    noise_target_h_milli: 70,
    rho_max_milli: 500,
    headroom_pub_ppb: 153_000_000,
    identify_bits: 8,
    budget_factor: TEMPLATE_F,
    min_success_max_failed: 2,
};

/// The mainnet template's `quant-v2` block at `n` 4,000: `H` 0.062.
pub const MAINNET_QUANT2: QuantParamsV2 = QuantParamsV2 {
    noise_target_h_milli: 70,
    rho_max_milli: 500,
    headroom_pub_ppb: 62_000_000,
    identify_bits: 8,
    budget_factor: TEMPLATE_F,
    min_success_max_failed: 8,
};

/// The span-gate and gate-feedback terms as the templates count them: two
/// bits per executed entrant and two per cohort.
pub const TEMPLATE_TERMS: CountedTerms = CountedTerms { per_executed: 2, per_cohort: 2 };

struct Lines(String);

impl Lines {
    fn line(&mut self, key: &str, value: impl core::fmt::Display) {
        let _ = writeln!(self.0, "{key}={value}");
    }
}

/// A fixed stream key for a report entry.
pub fn key(name: &str) -> Digest {
    domain_hash("slop_ninja.protocol.golden/quant-v2/key", name.as_bytes())
}

fn scale_line(s: &Scale) -> String {
    format!(
        "target={} floor={} sigma={} rho_micro={} width={}",
        s.target, s.floor, s.sigma, s.rho_micro, s.width
    )
}

fn outcome_lines(r: &mut Lines, name: &str, input: &QInput<'_>) -> Result<()> {
    let o = q_outcome(input)?;
    r.line(
        &format!("{name}.cutoff"),
        format_args!(
            "{:?} {} band_width={}",
            o.mode,
            o.cutoff.map_or("none".into(), |c| c.to_string()),
            o.band_width.map_or("none".into(), |c| c.to_string())
        ),
    );
    for c in &o.clusters {
        r.line(
            &format!("{name}.cluster.{}", c.head),
            format_args!(
                "members={:?} band={} kappa={} mu={}..={} credit={}",
                c.members, c.band, c.band_rank, c.positions.0, c.positions.1, c.credit
            ),
        );
    }
    r.line(&format!("{name}.units"), format_args!("{:?}", o.units));
    Ok(())
}

/// A synthetic finalized `quant-v2` cohort under `reward-v3` at 1,000 cases
/// with the testnet block: four distinct entrants and one near copy of the
/// first, admitted after it.
pub fn synthetic_v2_cohort() -> Result<(crate::ScorePackageV2, QuantizedResult)> {
    let specs = vec![
        Spec::new(0x61, bp(9500)),
        Spec::new(0x62, bp(9300)),
        Spec::new(0x63, bp(9200)),
        Spec::new(0x64, bp(9000)),
        Spec::new(0x65, bp(9600)).copy_of(0x61),
    ];
    let mut m = fx::manifest(chain(), 9, 1000, sigma_permille(40), 1, &specs);
    m.reward_rule = RewardRule::RewardV3;
    m.params = RewardV1Params::new(m.params.sigma, TESTNET_QUANT2);
    let index = fx::default_index(&m.references);
    let mut p = fx::package(&m, bp(8470), Some(bp(8400)), &specs)?;
    let mut measure = fx::spec_measure(&specs, 1000, |h, o| specs[h].family == specs[o].family);
    fx::finalize_fixture(&mut p, &index, &mut measure)?;
    let result = fx::result_of(&p)?;
    Ok((p, result))
}

pub fn golden_report() -> Result<String> {
    let mut r = Lines(String::new());
    r.line("schema", "slop_ninja.protocol.golden/quant-v2");
    r.line("unit", UNIT);
    // Bernoulli(exp(-num/den)) for any gamma: below 1, at 1, above 1, a
    // large integer part and a zero remainder.
    for (name, num, den) in [
        ("1/3", 1u128, 3u128),
        ("5/5", 5, 5),
        ("7/2", 7, 2),
        ("40000001/1000000", 40_000_001, 1_000_000),
        ("6/3", 6, 3),
    ] {
        let mut s = Stream::new(key(&format!("bern_exp_rational.{name}")));
        let bits: String = (0..32)
            .map(|_| bern_exp_rational(&mut s, U256::from_u128(num), den).map(|b| if b { '1' } else { '0' }))
            .collect::<Result<_>>()?;
        r.line(&format!("bern_exp_rational.{name}"), format_args!("{bits} words={}", s.consumed()));
    }
    // Proposals of the Gaussian sampler at s = 3 (t = 4), step by step.
    let mut s = Stream::new(key("dlap.4"));
    let proposals: Vec<String> = (0..12)
        .map(|_| dlap(&mut s, 4, 1).map(|v| v.to_string()))
        .collect::<Result<_>>()?;
    r.line("dlap.4", format_args!("{} words={}", proposals.join(","), s.consumed()));
    // Discrete Gaussian draws.
    for sigma in [1u64, 2, 3, 10, 1_000, 4_015_595, 9_901_385, 1_000_000_000] {
        let mut s = Stream::new(key(&format!("gaussian.{sigma}")));
        let draws: Vec<String> = (0..6)
            .map(|_| gaussian(&mut s, sigma).map(|v| v.to_string()))
            .collect::<Result<_>>()?;
        r.line(&format!("gaussian.{sigma}"), format_args!("{} words={}", draws.join(","), s.consumed()));
    }
    // The target scale and the privacy floor.
    for t_h in [35u16, 70] {
        for h in [0u32, 1, 62_000_000, 153_000_000, 1_000_000_000] {
            r.line(&format!("sigma_target.{t_h}.{h}"), sigma_target(t_h, h)?);
        }
    }
    for (n, m, rho) in [(1000u32, 42u32, 500u32), (4000, 258, 500), (8000, 258, 500)] {
        r.line(
            &format!("sigma_floor.{n}.{m}.{rho}"),
            format_args!("{} delta_ppb={}", sigma_floor(n, m, rho)?, quant2::sensitivity_ppb(n)?),
        );
    }
    // The scale of the five model rows: (X, n, H).
    for (x, n, h) in [
        (256u16, 4000u32, 62_000_000u32),
        (40, 4000, 62_000_000),
        (40, 1000, 153_000_000),
        (8, 1000, 153_000_000),
        (256, 8000, 62_000_000),
    ] {
        let params = QuantParamsV2 { headroom_pub_ppb: h, ..MAINNET_QUANT2 };
        let m = quant::charged(x, true);
        let scale = Scale::derive(n, m, &params)?;
        let (q20, noisy) = noisy_bits(m, scale.sigma, n)?;
        r.line(
            &format!("scale.{x}.{n}.{h}"),
            format_args!("{} noisy_q20={q20} noisy={noisy}", scale_line(&scale)),
        );
    }
    // The ladder jumps where the floor binds at (256, 4,000).
    for sigma in [4_000_000u64, 4_000_001, 4_015_595, 9_999_999, 10_000_000, 10_000_001] {
        r.line(&format!("ladder.{sigma}"), quant::ladder(u128::from(sigma) * u128::from(UNIT))?);
    }
    // The information bound's logarithm at its edges.
    for (num, den) in [
        (1u64, 1u64),
        (2, 1),
        (3, 1),
        ((1 << 62) - 1, 1),
        (16_125_000_000_000 + 62_500_000_000_000, 16_125_000_000_000),
        (98_000_000_000_000 + 250_000_000_000_000, 98_000_000_000_000),
    ] {
        r.line(&format!("log2_upper_q20.{num}/{den}"), log2_upper_q20(num, den)?);
    }
    for n in [1000u32, 4000] {
        r.line(&format!("value_variance_bound.{n}"), quant2::value_variance_bound(n)?);
    }
    // The Bell table.
    let bell: Vec<String> = (0..=10)
        .chain([40, 128, 255, 256])
        .map(|x| format!("{x}:{}", BELL_BITS[x]))
        .collect();
    r.line("bell_bits", bell.join(","));
    // Budgets: at the seal and the plan-time worst case of each template.
    for (name, n, cap, params) in [
        ("testnet", 1000u32, 40u16, TESTNET_QUANT2),
        ("mainnet", 4000, 256, MAINNET_QUANT2),
    ] {
        let m = quant::charged(cap, true);
        let scale = Scale::derive(n, m, &params)?;
        let b = Budget2::count(cap, true, n, &scale)?;
        let bits = quant2::cohort_bits(cap, true, n, &scale, &TEMPLATE_TERMS)?;
        r.line(&format!("budget.{name}.seal"), format_args!("{b:?} terms={} total={}", bits.terms, bits.total));
        let (worst, ok) = quant2::worst_case_terms(n, cap, &params, &TEMPLATE_TERMS)?;
        r.line(
            &format!("budget.{name}.worst"),
            format_args!("{:?} terms={} total={} within={ok} bound={}", worst.quant, worst.terms, worst.total, n * 8 / 10),
        );
    }
    r.line(
        "budget.infeasible",
        format_args!("{:?}", quant2::worst_case_terms(2, 256, &TESTNET_QUANT2, &CountedTerms::NONE).map(|_| ())),
    );
    // Quantization through the draws at the testnet scale, with clamps.
    let (n, roster) = (1000, 40);
    let scale = Scale::derive(n, quant::charged(roster, true), &TESTNET_QUANT2)?;
    for (name, subject, score) in [
        ("entrant", Subject::Entrant(Digest([0x71; 32])), 900_000_000_000_000_000u64),
        ("current", Subject::Current(Digest([0x72; 32])), 847_000_000_000_000_000),
        ("predecessor", Subject::Predecessor(Digest([0x73; 32])), 840_000_000_000_000_000),
        ("clamp_low", Subject::Entrant(Digest([0x74; 32])), 0),
        ("clamp_high", Subject::Entrant(Digest([0x75; 32])), SCORE_SCALE),
    ] {
        let draw = quant2::draw(&SEED, &COHORT, &subject, &scale)?;
        let q = quant::quantize(score, &draw, scale.width)?;
        r.line(
            &format!("quantize.{name}"),
            format_args!("offset={} noise={} q={q}", draw.offset, draw.noise),
        );
    }
    // The q-only band outcomes on quant-v2 grids (w 0.005 and 0.01) under
    // both placements, with `q_Rk` near 0.845: entrant 2 is a later near
    // copy of entrant 0 with a better draw, and entrant 3 is gated below
    // the cutoff.
    for w in [5_000_000_000_000_000u64, 10_000_000_000_000_000] {
        let params = RewardV1Params::new(sigma_permille(40), TESTNET_QUANT2);
        let at = |cells: u64| cells * w;
        let ref_q = SCORE_SCALE / 1000 * 845 / w * w;
        let g = |q: u64, cluster: Option<u16>| QEntrant { q: Some(q), gated: true, cluster };
        let above = [g(ref_q + at(4), Some(0)), g(ref_q + at(6), Some(1)), g(ref_q + at(9), Some(0))];
        for (placement, tag, below) in [
            (Placement::QRankHead, "reward_v2", None),
            (Placement::RosterFirst, "reward_v3", Some(3)),
        ] {
            let mut entrants = above.to_vec();
            entrants.push(g(ref_q - at(1), below));
            outcome_lines(
                &mut r,
                &format!("{tag}.w{w}"),
                &QInput {
                    params: &params,
                    width: w,
                    reference_q: ref_q,
                    predecessor_q: Some(ref_q - at(3)),
                    entrants: &entrants,
                    placement,
                },
            )?;
        }
    }
    // A full synthetic quant-v2 / reward-v3 result.
    let (p, result) = synthetic_v2_cohort()?;
    let q = p.quant.v2()?;
    r.line(
        "synthetic.record",
        format_args!(
            "sigma={} target={} floor={} rho_micro={} m={} width={} band_width={:?} cutoff={:?} reference_q={} predecessor_q={:?}",
            q.sigma_ppb,
            q.sigma_target_ppb,
            q.sigma_floor_ppb,
            q.rho_micro,
            q.charged,
            q.width,
            q.band_width,
            q.cutoff.map(|c| c.get()),
            q.reference_q.get(),
            q.predecessor_q.map(|c| c.get())
        ),
    );
    for (i, e) in result.entrants.iter().enumerate() {
        r.line(
            &format!("synthetic.entrant.{i}"),
            format_args!("{:?} band_rank={:?} units={}", e.outcome, e.band_rank, e.units),
        );
    }
    r.line("synthetic.budget", format_args!("{:?}", result.budget));
    let bytes = result.to_bytes()?;
    r.line("synthetic.result.len", bytes.len());
    r.line("synthetic.result.digest", result.digest()?);
    r.line("synthetic.package.digest", p.digest()?);
    r.line("max_signed_len", QuantizedResult::max_signed_len()?);
    r.line(
        "manifest_quant_block",
        format_args!("{:?}", QuantBlock::V2(TESTNET_QUANT2).rule()),
    );
    Ok(r.0)
}
