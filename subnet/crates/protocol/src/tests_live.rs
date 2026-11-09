//! `live-v1`: the partition, the privacy accounting (noise scale, grid, bits
//! and the derived segment count), the draws and the display values.

use crate::live::{self, LiveParams};
use crate::quant::{self, CohortBinding, CountedTerms, Subject};
use crate::{Digest, Error, QuantParams, SCORE_SCALE};

const N: u32 = 1000;
/// The bound factor `F` of the r1 template's `quant-v1` block.
const F: u16 = 10;
/// The r1 template's `quant-v1` block: eps 2, h 8 and `F`, so n h / F is
/// an 800-bit bound.
const Q: QuantParams = QuantParams {
    eps_total_milli: 2000,
    identify_bits: 8,
    min_success_max_failed: 2,
    // `F` above.
    budget_factor: F,
};
const LIVE: LiveParams = LiveParams { eps_live_milli: 1000, segments_max: 4 };
/// The span gate's counted bits: one per executed entrant and one per cohort.
const SPAN: CountedTerms = CountedTerms { per_executed: 1, per_cohort: 1 };
const HUNDREDTH: u64 = SCORE_SCALE / 100;

#[test]
fn segments_are_contiguous_smaller_first() {
    assert_eq!((0..4).map(|s| live::segment_bounds(1000, 4, s).unwrap()).collect::<Vec<_>>(), [(0, 250), (250, 500), (500, 750), (750, 1000)]);
    // 10 tasks in 4 segments: 2, 2, 3, 3.
    assert_eq!((0..4).map(|s| live::segment_bounds(10, 4, s).unwrap()).collect::<Vec<_>>(), [(0, 2), (2, 4), (4, 7), (7, 10)]);
    assert!(live::segment_bounds(3, 4, 0).is_err());
    assert!(live::segment_bounds(10, 4, 4).is_err());
}

/// The r1 layout: 2,224 sealed tasks in chunks of 32, 1,000 of them batch
/// tasks spread evenly over them by the private order.
fn r1_positions() -> Vec<u32> {
    (0..1000u32).map(|i| i * 2224 / 1000).collect()
}

#[test]
fn one_lane_segments_are_task_order_runs() {
    let pos = r1_positions();
    let seg = live::partition(&pos, 32, 1, 70, 4).unwrap();
    for (i, s) in seg.iter().enumerate() {
        assert_eq!(*s as usize, i / 250 + 1, "position {i}");
    }
}

#[test]
fn three_lanes_put_every_lane_in_every_segment() {
    let pos = r1_positions();
    let (lanes, chunks) = (3u32, 70u32);
    let seg = live::partition(&pos, 32, lanes, chunks, 4).unwrap();
    let mut counts = [[0u32; 4]; 3];
    for (p, s) in pos.iter().zip(&seg) {
        let c = p / 32;
        let lane = (0..lanes).find(|k| (k * chunks / lanes..(k + 1) * chunks / lanes).contains(&c)).unwrap();
        counts[lane as usize][usize::from(*s) - 1] += 1;
    }
    for (lane, row) in counts.iter().enumerate() {
        for (s, n) in row.iter().enumerate() {
            assert!(*n > 40, "lane {lane} segment {} holds {n}", s + 1);
        }
    }
    assert_eq!(seg.iter().filter(|s| **s == 1).count(), 250);
}

/// Commit order, steals, retries and timing never enter the partition: it is
/// a function of the positions alone, whatever order they are listed in.
#[test]
fn the_partition_reads_positions_only() {
    let pos = r1_positions();
    let base = live::partition(&pos, 32, 3, 70, 4).unwrap();
    let mut rev = pos.clone();
    rev.reverse();
    let other = live::partition(&rev, 32, 3, 70, 4).unwrap();
    for (i, p) in rev.iter().enumerate() {
        let j = pos.binary_search(p).unwrap();
        assert_eq!(other[i], base[j]);
    }
    // A position past the job's chunks or a repeated one is refused.
    assert!(live::partition(&[0, 2240], 32, 1, 70, 2).is_err());
    assert!(live::partition(&[5, 5, 7, 9], 32, 1, 70, 2).is_err());
}

/// Today's roster (X = 8, no predecessor): grid 0.04, 135 live bits on the
/// final's 148, 283 of 800.
#[test]
fn the_accounting_at_eight_entrants() {
    let tau = live::tau_live(N, 4, 8, 1000).unwrap();
    assert_eq!(tau.floor_width(), 36 * SCORE_SCALE as u128 / 1000);
    let w = live::width_live(N, 4, 8, 1000).unwrap();
    assert_eq!(w, 4 * HUNDREDTH);
    assert_eq!(live::live_bits(8, 4, w).unwrap(), 135);
    let d = live::derive_segments(N, 8, false, &Q, &LIVE, &CountedTerms::NONE).unwrap();
    assert_eq!(d, live::Derivation { segments: 4, counted: 148, live: 135, bound: 800 });
    let d = live::derive_segments(N, 8, false, &Q, &LIVE, &SPAN).unwrap();
    assert_eq!((d.segments, d.counted, d.live), (4, 157, 135));
    // One reading per subject per checkpoint: X + 1 subjects, R_k-1 has none.
    assert_eq!(live::live_bits(8, 2, w).unwrap(), 45);
    assert_eq!(live::live_bits(8, 0, w).unwrap(), 0);
}

/// The L(X) table at epsilon_live 1, n 1000, pinned for X = 0..=40 with and
/// without the span gate's X + 1 term, under both reference shapes.
#[test]
fn the_segment_count_table() {
    let table = |pred: bool, terms: &CountedTerms| -> Vec<u8> {
        (0..=40).map(|x| live::derive_segments(N, x, pred, &Q, &LIVE, terms).unwrap().segments).collect()
    };
    let run = |fours: usize, rest: &[u8]| -> Vec<u8> {
        let mut v = vec![4u8; fours];
        v.extend_from_slice(rest);
        v.resize(41, 0);
        v
    };
    assert_eq!(table(false, &CountedTerms::NONE), run(31, &[3, 2, 2, 2, 2]));
    assert_eq!(table(true, &CountedTerms::NONE), run(30, &[3, 3, 2, 2, 2, 2]));
    assert_eq!(table(false, &SPAN), run(30, &[3, 2, 2, 2, 2]));
    assert_eq!(table(true, &SPAN), run(29, &[3, 2, 2, 2, 2]));
    // Every case keeps the complete total within the bound.
    for pred in [false, true] {
        for terms in [CountedTerms::NONE, SPAN] {
            for x in 0..=40 {
                let d = live::derive_segments(N, x, pred, &Q, &LIVE, &terms).unwrap();
                assert!(u64::from(d.counted + d.live) <= d.bound, "X {x}");
            }
        }
    }
    assert_eq!(
        live::segments_table(N, 2, &Q, &LIVE, &CountedTerms::NONE).unwrap(),
        [(4, 4), (4, 4), (4, 4)]
    );
}

/// Negative control: a derivation that counts `Budget::count` alone picks a
/// larger L than the complete total allows once another rule adds terms,
/// and so breaks the bound.
#[test]
fn counting_the_quant_budget_alone_breaks_the_bound() {
    let mut broke = 0;
    for x in 0..=40u16 {
        for pred in [false, true] {
            let alone = live::derive_segments(N, x, pred, &Q, &LIVE, &CountedTerms::NONE).unwrap();
            let complete = live::derive_segments(N, x, pred, &Q, &LIVE, &SPAN).unwrap();
            let span_bits = SPAN.bits(x).unwrap();
            if alone.segments > complete.segments {
                assert!(u64::from(alone.counted + span_bits + alone.live) > alone.bound, "X {x}");
                broke += 1;
            }
        }
    }
    assert!(broke >= 2, "the extra term must lower L somewhere");
}

/// The grid and L read only public values: two cohorts that differ in their
/// seed, labels or predictions get the same grid and the same L.
#[test]
fn the_grid_reads_public_values_only() {
    let w = live::width_live(N, 4, 8, 1000).unwrap();
    let tau = live::tau_live(N, 4, 8, 1000).unwrap();
    for seed in [[0u8; 32], [7u8; 32]] {
        let cohort = CohortBinding(Digest(seed));
        let s = Subject::Entrant(Digest([3; 32]));
        let d = live::draw_live(&seed, &cohort, &s, 1, 0, w, &tau).unwrap();
        assert!(d.offset < w);
    }
    assert_eq!(live::width_live(N, 4, 8, 1000).unwrap(), w);
}

#[test]
fn the_noise_sd_display() {
    let sd = |x: u16| live::format_milli(live::noise_sd_milli(N, 4, x, 1000).unwrap());
    assert_eq!(sd(1), "0.012");
    assert_eq!(sd(4), "0.030");
    assert_eq!(sd(8), "0.054");
    assert_eq!(sd(13), "0.083");
    assert_eq!(sd(20), "0.125");
    assert_eq!(live::format_milli(1234), "1.234");
}

/// Live draws never share a stream with `quant-v1`: a reading minus the
/// released score cannot cancel the noise. A revision draws fresh noise.
#[test]
fn live_draws_have_their_own_domains() {
    let seed = [9u8; 32];
    let cohort = CohortBinding(Digest([1; 32]));
    let s = Subject::Entrant(Digest([2; 32]));
    let mut keys = vec![quant::offset_key(&seed, &cohort, &s), quant::noise_key(&seed, &cohort, &s)];
    for cp in 1..=3u8 {
        for rev in 0..=1u8 {
            keys.push(live::live_key(live::OFFSET_DOMAIN, &seed, &cohort, &s, cp, rev));
            keys.push(live::live_key(live::NOISE_DOMAIN, &seed, &cohort, &s, cp, rev));
        }
    }
    let mut sorted = keys.clone();
    sorted.sort_by_key(|d| d.0);
    sorted.dedup();
    assert_eq!(sorted.len(), keys.len(), "every key distinct");
    // Mutation: a live key under the quant domain would be a quant draw's
    // relative, never equal to the live key.
    assert_ne!(
        live::live_key(quant::OFFSET_DOMAIN, &seed, &cohort, &s, 1, 0),
        live::live_key(live::OFFSET_DOMAIN, &seed, &cohort, &s, 1, 0)
    );
}

/// Golden readings: identical on every target (integer arithmetic only).
#[test]
fn golden_readings() {
    let seed = [0x5au8; 32];
    let cohort = CohortBinding(Digest([0x11; 32]));
    let s = Subject::Entrant(Digest([0x22; 32]));
    let r = Subject::Current(Digest([0x33; 32]));
    let mut out = Vec::new();
    for (subject, score) in [(s, 84 * HUNDREDTH), (r, 86 * HUNDREDTH), (s, 0), (r, SCORE_SCALE)] {
        for cp in 1..=3u8 {
            out.push(live::release(&seed, &cohort, &subject, cp, 0, N, 4, 8, 1000, score).unwrap());
        }
    }
    let w = 4 * HUNDREDTH;
    assert!(out.iter().all(|v| v % w == 0 && *v <= SCORE_SCALE));
    assert_eq!(out, GOLDEN_READINGS);
    // The last segment is never released alone; checkpoint 0 does not exist.
    assert!(live::release(&seed, &cohort, &s, 4, 0, N, 4, 8, 1000, 0).is_err());
    assert!(live::release(&seed, &cohort, &s, 0, 0, N, 4, 8, 1000, 0).is_err());
}

const GOLDEN_READINGS: [u64; 12] = [
    96 * HUNDREDTH, 80 * HUNDREDTH, 80 * HUNDREDTH, // an entrant at 0.84
    88 * HUNDREDTH, 88 * HUNDREDTH, 84 * HUNDREDTH, // R_k at 0.86
    12 * HUNDREDTH, 0, 0, // an entrant at 0, clamped below
    SCORE_SCALE, SCORE_SCALE, SCORE_SCALE, // R_k at 1, clamped above
];

#[test]
fn segment_scores_floor_the_mean() {
    assert_eq!(live::segment_score([1, 2]).unwrap(), 1);
    assert_eq!(live::segment_score([SCORE_SCALE, 0, 0]).unwrap(), SCORE_SCALE / 3);
    assert!(live::segment_score([]).is_err());
    assert!(live::segment_score([SCORE_SCALE + 1]).is_err());
}

#[test]
fn the_plan_checks() {
    let r1 = LiveParams { eps_live_milli: 1000, segments_max: 4 };
    r1.validate(N, 32, 40, &Q, &CountedTerms::NONE).unwrap();
    // The r1 plan's live row adds well under 100 bytes to a roster row.
    let bytes = live::row_bytes_max(N, 40, 2000, &r1).unwrap();
    assert!(bytes <= live::ROSTER_ROW_BYTES && bytes - live::ROSTER_ROW_BASE_BYTES < 100, "{bytes}");
    // No readings: always valid, nothing derived.
    LiveParams { eps_live_milli: 0, segments_max: 0 }.validate(N, 32, 40, &Q, &CountedTerms::NONE).unwrap();
    assert_eq!(live::derive_segments(N, 8, false, &Q, &LiveParams { eps_live_milli: 0, segments_max: 0 }, &CountedTerms::NONE).unwrap().segments, 0);
    for bad in [
        LiveParams { eps_live_milli: 1000, segments_max: 1 },
        LiveParams { eps_live_milli: 0, segments_max: 4 },
        // A segment shorter than a chunk.
        LiveParams { eps_live_milli: 1000, segments_max: 32 },
    ] {
        assert!(bad.validate(N, 32, 40, &Q, &CountedTerms::NONE).is_err(), "{bad:?}");
    }
}

/// The row bound comes from the schema: the largest `segments_max` whose
/// longest row fits is accepted and one more checkpoint is refused.
#[test]
fn the_row_bound_refuses_one_checkpoint_more() {
    let n = 1_000_000;
    let fits = |m: u8| live::row_bytes_max(n, 40, 2000, &LiveParams { eps_live_milli: 1000, segments_max: m }).unwrap() <= live::ROSTER_ROW_BYTES;
    let largest = (2..=u8::MAX).take_while(|m| fits(*m)).last().unwrap();
    assert!(largest >= 4);
    LiveParams { eps_live_milli: 1000, segments_max: largest }.validate(n, 1, 40, &Q, &CountedTerms::NONE).unwrap();
    assert_eq!(
        LiveParams { eps_live_milli: 1000, segments_max: largest + 1 }.validate(n, 1, 40, &Q, &CountedTerms::NONE),
        Err(Error::OutOfRange("live row bytes"))
    );
}

/// The complete counted total is `Budget::count` plus the terms, and the
/// worst case over the plan agrees with `quant::worst_case` without terms.
#[test]
fn cohort_bits_adds_every_rule_term() {
    let w = quant::narrowest_width(N, 8, false, 2000).unwrap();
    let b = quant::cohort_bits(8, w, false, &CountedTerms::NONE).unwrap();
    assert_eq!((b.quant.total, b.terms, b.total), (148, 0, 148));
    let b = quant::cohort_bits(8, w, false, &SPAN).unwrap();
    assert_eq!((b.terms, b.total), (9, 157));
    let (alone, ok) = quant::worst_case(N, 40, 2000, 8, 10).unwrap();
    let (complete, ok2) = quant::worst_case_terms(N, 40, 2000, 8, 10, &CountedTerms::NONE).unwrap();
    assert_eq!((alone, ok), (complete.quant, ok2));
    let (with_span, _) = quant::worst_case_terms(N, 40, 2000, 8, 10, &SPAN).unwrap();
    // The worst case is the full roster, where the span term adds X + 1.
    assert_eq!((with_span.total, with_span.terms), (complete.total + 41, 41));
}
