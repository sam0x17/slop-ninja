//! The `quant-v1` golden report: fixed seeds and subjects through every
//! primitive of [`crate::quant`], and the q-only `reward-v2` calculation on
//! synthetic results. It is compared byte for byte against
//! `fixtures/golden-quant-v1.txt` on native targets and inside a
//! `wasm32-unknown-unknown` build.

use std::fmt::Write as _;

use crate::quant::{
    self, Budget, SeMeasure, Stream, Subject, Tau, bern_exp, dlap, ladder, uniform,
};
use crate::reward_v1::{QEntrant, QInput, q_outcome};
use crate::{Digest, QuantParams, Result, RewardV1Params, SCORE_SCALE};

/// The report's fixed seed.
pub const SEED: [u8; 32] = [0x2a; 32];

/// The report's fixed cohort binding (a manifest digest).
pub const COHORT: quant::CohortBinding = quant::CohortBinding(Digest([0x3c; 32]));

/// A fixed entrant subject, by content digest.
pub fn entrant(tag: u8) -> Subject {
    Subject::Entrant(Digest([tag; 32]))
}

/// The testnet plan's quantization block.
pub const TESTNET_QUANT: QuantParams = QuantParams {
    eps_total_milli: 2000,
    identify_bits: 8,
    budget_factor: 10,
    min_success_max_failed: 2,
};

/// Utilities of a synthetic 1,000-document batch scoring about 0.847.
pub fn synthetic_utilities() -> impl Iterator<Item = u64> {
    (0..1000u64).map(|t| match t % 20 {
        0..=2 => 0,
        3 => 400_000_000_000_000_000,
        4 => 940_000_000_000_000_000,
        _ => 1_000_000_000_000_000_000 - (t % 7) * 10_000_000_000_000_000,
    })
}

struct Lines(String);

impl Lines {
    fn line(&mut self, key: &str, value: impl core::fmt::Display) {
        let _ = writeln!(self.0, "{key}={value}");
    }
}

fn words(stream: &mut Stream, count: usize) -> String {
    (0..count)
        .map(|_| format!("{:032x}", stream.word()))
        .collect::<Vec<_>>()
        .join(",")
}

/// A q-only outcome as one line per entrant.
fn outcome_lines(r: &mut Lines, name: &str, input: &QInput<'_>) -> Result<()> {
    let o = q_outcome(input)?;
    r.line(
        &format!("reward_v2.{name}.cutoff"),
        format_args!(
            "{:?} {} band_width={}",
            o.mode,
            o.cutoff.map_or("none".into(), |c| c.to_string()),
            o.band_width.map_or("none".into(), |c| c.to_string())
        ),
    );
    for c in &o.clusters {
        r.line(
            &format!("reward_v2.{name}.cluster.{}", c.head),
            format_args!(
                "members={:?} band={} kappa={} mu={}..={} credit={}",
                c.members,
                c.band,
                c.band_rank,
                c.positions.0,
                c.positions.1,
                c.credit
            ),
        );
    }
    r.line(&format!("reward_v2.{name}.units"), format_args!("{:?}", o.units));
    Ok(())
}

pub fn golden_report() -> Result<String> {
    let mut r = Lines(String::new());
    r.line("schema", "slop_ninja.protocol.golden/quant-v1");
    // Stream blocks and words.
    let key = quant::noise_key(&SEED, &COHORT, &entrant(0x11));
    r.line("stream.key", key);
    for j in 0..3 {
        r.line(&format!("stream.block.{j}"), Stream::block(&key, j));
    }
    r.line("stream.words", words(&mut Stream::new(key), 6));
    // Uniform draws at the edge moduli.
    for (name, m) in [
        ("1", 1u128),
        ("2", 2),
        ("2^127+1", (1u128 << 127) + 1),
        ("u128_max", u128::MAX),
        ("10^18", 1_000_000_000_000_000_000),
    ] {
        let mut s = Stream::new(quant::offset_key(&SEED, &COHORT, &entrant(0x12)));
        let draws: Vec<String> = (0..4)
            .map(|_| uniform(&mut s, m).map(|v| v.to_string()))
            .collect::<Result<_>>()?;
        r.line(
            &format!("uniform.{name}"),
            format_args!("{} words={}", draws.join(","), s.consumed()),
        );
    }
    // Bernoulli(exp(-num/den)) sequences.
    for (num, den) in [(0u128, 1u128), (1, 1), (1, 2), (3, 7), (1000, 1001)] {
        let mut s = Stream::new(quant::noise_key(&SEED, &COHORT, &entrant(0x13)));
        let bits: String = (0..32)
            .map(|_| bern_exp(&mut s, num, den).map(|b| if b { '1' } else { '0' }))
            .collect::<Result<_>>()?;
        r.line(&format!("bern_exp.{num}/{den}"), format_args!("{bits} words={}", s.consumed()));
    }
    // Discrete-Laplace draws: the testnet scale (n = 1000, 40 entrants) and
    // a small one.
    let testnet = Tau::new(1000, quant::charged(40, true), TESTNET_QUANT.eps_total_milli)?;
    r.line("tau.testnet", format_args!("{}/{} floor={}", testnet.a, testnet.b, testnet.floor_width()));
    for (name, tau) in [("testnet", testnet), ("3/2", Tau { a: 3, b: 2 })] {
        let mut s = Stream::new(quant::noise_key(&SEED, &COHORT, &entrant(0x14)));
        let draws: Vec<String> = (0..12)
            .map(|_| dlap(&mut s, tau.a, tau.b).map(|v| v.to_string()))
            .collect::<Result<_>>()?;
        r.line(&format!("dlap.{name}"), format_args!("{} words={}", draws.join(","), s.consumed()));
    }
    // SE of a synthetic batch, the grid and one subject's q.
    let se = SeMeasure::measure(synthetic_utilities())?;
    r.line(
        "se.synthetic",
        format_args!("n={} s1={} s2={:032x}{:032x} se={}", se.n, se.s1, se.s2.hi, se.s2.lo, se.se),
    );
    let mean = se.s1 / u128::from(se.n);
    for executed in [5u16, 40] {
        let tau = Tau::new(1000, quant::charged(executed, true), TESTNET_QUANT.eps_total_milli)?;
        let w = quant::width(se.se, &tau)?;
        let mut buf = [0u8; quant::MAX_DECIMAL_LEN];
        r.line(
            &format!("grid.x{executed}"),
            format_args!(
                "floor={} w={} ({}) K={}",
                tau.floor_width(),
                w,
                quant::format_grid(w, w, &mut buf)?,
                quant::grid_values(w)?
            ),
        );
        for (name, subject) in [
            ("entrant_11", entrant(0x11)),
            ("current", Subject::Current(Digest([0x21; 32]))),
            ("predecessor", Subject::Predecessor(Digest([0x22; 32]))),
        ] {
            let d = quant::draw(&SEED, &COHORT, &subject, w, &tau)?;
            let q = quant::quantize(mean as u64, &d, w)?;
            let mut buf = [0u8; quant::MAX_DECIMAL_LEN];
            r.line(
                &format!("q.x{executed}.{name}"),
                format_args!(
                    "offset={} noise={} q={} ({})",
                    d.offset,
                    d.noise,
                    q,
                    quant::format_grid(q, w, &mut buf)?
                ),
            );
        }
    }
    // The ladder.
    for (name, raw) in [
        ("0.021", 21_000_000_000_000_000u128),
        ("0.01", 10_000_000_000_000_000),
        ("0.0100..01", 10_000_000_000_000_001),
        ("1e-18", 1),
        ("1", SCORE_SCALE as u128),
        ("2", 2 * SCORE_SCALE as u128),
    ] {
        r.line(&format!("ladder.{name}"), ladder(raw)?);
    }
    // The bit budget.
    for (n, cap) in [(1000u32, 40u16), (2000, 40), (4000, 256)] {
        let (worst, ok) = quant::worst_case(n, cap, 2000, 8, 10)?;
        r.line(
            &format!("budget.n{n}.cap{cap}"),
            format_args!(
                "worst={} x={} K={} per_entrant={} references={} clusters={} promotion={} width={} bound={} ok={ok}",
                worst.total,
                worst.executed,
                worst.grid_values,
                worst.per_entrant,
                worst.references,
                worst.clusters,
                worst.promotion,
                worst.width,
                n * 8 / 10
            ),
        );
    }
    let b = Budget::count(40, 30_000_000_000_000_000, true)?;
    r.line("budget.x40.w0.03", format_args!("{b:?}"));
    // The q-only reward-v2 calculation: a tie group, two heads one grid cell
    // apart, a withheld and a gate-failed entrant, and roster-order
    // leftovers.
    let params = RewardV1Params::new(40_000_000_000_000_000, TESTNET_QUANT);
    let w = 30_000_000_000_000_000u64;
    let g = |q: u64, cluster: Option<u16>| QEntrant {
        q: Some(q * w),
        gated: true,
        cluster,
    };
    let entrants = [
        g(31, Some(0)),
        g(31, Some(1)),
        QEntrant { q: None, gated: false, cluster: None },
        g(30, Some(3)),
        QEntrant { q: Some(33 * w), gated: false, cluster: None },
        g(31, Some(0)),
        g(29, Some(6)),
    ];
    outcome_lines(
        &mut r,
        "ties",
        &QInput {
            params: &params,
            width: w,
            reference_q: 28 * w,
            predecessor_q: Some(27 * w),
            entrants: &entrants,
        },
    )?;
    let three = [g(31, Some(0)), g(31, Some(1)), g(31, Some(2))];
    outcome_lines(
        &mut r,
        "three_way_tie",
        &QInput {
            params: &params,
            width: w,
            reference_q: 28 * w,
            predecessor_q: None,
            entrants: &three,
        },
    )?;
    let fallback = [g(28, Some(0)), g(27, Some(1)), g(25, None)];
    outcome_lines(
        &mut r,
        "fallback",
        &QInput {
            params: &params,
            width: w,
            reference_q: 28 * w,
            predecessor_q: Some(26 * w),
            entrants: &fallback,
        },
    )?;
    Ok(r.0)
}
