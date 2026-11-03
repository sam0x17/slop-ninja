//! Recomputes the statistical and cadence figures that whitepaper 0.56
//! (Sections 7 and 7b) states alongside `reward-v1`. These are checks of the
//! paper's arithmetic, not protocol code, so floating point is allowed here;
//! the protocol's own calculation stays exact.

use slop_ninja_protocol::{
    CohortSchedule, DECAY_BPS, FALLBACK_BPS, IMPROVEMENT_DIVISOR, LAUNCH_RENEW_BY_TEMPOS,
    LAUNCH_ORIGIN_MIX, MARGIN_FLOOR, MARGIN_SIGMAS, NEAR_COPY_PPB, NEAR_EQUAL_DIVISOR, SCORE_SCALE,
};

fn phi(z: f64) -> f64 {
    (-z * z / 2.0).exp() / (2.0 * std::f64::consts::PI).sqrt()
}

/// Standard normal CDF by Simpson integration of the density from -12.
fn cdf(z: f64) -> f64 {
    let (a, n) = (-12.0, 20_000);
    let h = (z - a) / n as f64;
    let mut sum = phi(a) + phi(z);
    for i in 1..n {
        let x = a + i as f64 * h;
        sum += if i % 2 == 1 { 4.0 } else { 2.0 } * phi(x);
    }
    sum * h / 3.0
}

#[test]
fn promotion_rates_for_ten_entrants_equal_to_the_reference() {
    // Each entrant's paired difference D_i = X_i - X_R has SE sqrt(2) s, and
    // promotion needs D_i >= 2 SE, i.e. X_i >= X_R + 2 sqrt(2) s. The shared
    // X_R correlates the ten differences (rho = 0.5).
    let threshold = 2.0 * 2f64.sqrt();
    let (a, b, n) = (-10.0, 10.0, 4000);
    let h = (b - a) / n as f64;
    let mut none = 0.0;
    for i in 0..=n {
        let x = a + i as f64 * h;
        let w = if i == 0 || i == n {
            1.0
        } else if i % 2 == 1 {
            4.0
        } else {
            2.0
        };
        none += w * phi(x) * cdf(x + threshold).powi(10);
    }
    none *= h / 3.0;
    // "promote none about 87% of the time", "promotes one of them about 13%".
    assert_eq!((none * 100.0).round(), 87.0);
    assert_eq!(((1.0 - none) * 100.0).round(), 13.0);
    // "independent differences would give about 21%".
    let independent = 1.0 - cdf(2.0).powi(10);
    assert_eq!((independent * 100.0).round(), 21.0);
}

#[test]
fn standard_errors_and_the_noise_floor_sample_size() {
    // SE of a paired difference: 4.0% of headroom at 1,000 cases.
    let se_1k = 0.040;
    let per_document = se_1k * 1000f64.sqrt();
    assert_eq!(((per_document / 2000f64.sqrt()) * 1000.0).round(), 28.0); // 2.8% at 2,000
    assert_eq!(((per_document / 4000f64.sqrt()) * 1000.0).round(), 20.0); // 2.0% at 4,000
    // 1% of headroom, two-sided 5% test at 80% power: about 125,000 documents.
    let (z_alpha, z_beta) = (1.959_964, 0.841_621);
    let n = ((z_alpha + z_beta) * per_document / 0.01).powi(2);
    assert!((120_000.0..130_000.0).contains(&n), "{n}");
    assert_eq!((n / 1000.0).round(), 126.0); // 125,582: "about 125,000".
    // delta = max(0.01, 2 sigma): about 0.08 at 1,000 and 0.04 at 4,000.
    let delta = |se: f64| (2.0 * se).max(MARGIN_FLOOR as f64 / SCORE_SCALE as f64);
    assert!((delta(0.040) - 0.08).abs() < 1e-12);
    assert!((delta(0.020) - 0.04).abs() < 1e-12);
    assert_eq!(MARGIN_SIGMAS, 2);
}

#[test]
fn perturbed_copy_costs_against_the_near_copy_distance() {
    // The figures of the copy-policy wording proposed for 07-rewards.tex
    // (reward-v1 SPEC-GAPS, sections E to G), measured on the consumed Test
    // (copy-stats `price.rs`, 128 noise realizations, 200 batches per
    // realization and size, a fixed 1,024-document probe per realization
    // disjoint from its batches).
    let d = f64::from(NEAR_COPY_PPB) / 1e9;
    // "Variation of that size that carries no label information costs about
    // d^2 = 6e-4 of expected score, less any gain also available by
    // recalibrating the reference, about 4e-4 for R_0 on the launch mix."
    assert_eq!((d * d * 1e4).round(), 6.0);
    let recalibration_gain: f64 = 3.66e-4;
    assert_eq!((recalibration_gain * 1e4_f64).round(), 4.0);
    // The floor d^2 - g, about 2.6e-4.
    assert_eq!(((d * d - recalibration_gain) * 1e5).round(), 26.0);
    // R_0's launch-mix headroom is 1 - 0.93845; w = 2 sigma (1 - S_Rk).
    let headroom: f64 = 1.0 - 0.93845;
    let w_1k = 2.0 * 0.040 * headroom;
    let w_4k = 2.0 * 0.020 * headroom;
    let band_1k = |c: f64| (c / w_1k * 100.0).round();
    let band_4k = |c: f64| (c / w_4k * 100.0).round();
    let permille = |c: f64| (c / headroom * 1000.0).round();
    // "The cheapest perturbed copy of R_0 found ... cleared the gate in more
    // than 90% of 1,000-document batches for about 4e-4 to 5.5e-4 of
    // expected score, 0.6% to 0.9% of the reference's headroom: 0.08 to 0.11
    // of a band at 1,000 cases." Rank-space noise after the best
    // recalibration: s = 0.011 cost 3.70e-4 and cleared 94.9% of batches;
    // s = 0.012 cost 4.59e-4 and cleared 99.4%. The verifier's
    // 32-realization replication found 4.1e-4 (94.4%) and 5.5e-4 (99.5%).
    let (cost_s11, cost_s12) = (3.70e-4_f64, 4.59e-4_f64);
    let (verifier_s11, verifier_s12) = (4.1e-4_f64, 5.5e-4_f64);
    assert_eq!((cost_s11 * 1e4).round(), 4.0);
    assert_eq!((verifier_s11 * 1e4).round(), 4.0);
    assert_eq!((cost_s12 * 1e4).round(), 5.0);
    assert_eq!((verifier_s12 * 2e4).round() / 2.0, 5.5);
    assert_eq!(permille(cost_s11), 6.0);
    assert_eq!(permille(verifier_s12), 9.0);
    assert_eq!(band_1k(cost_s11), 8.0);
    assert_eq!(band_1k(verifier_s11), 8.0);
    assert_eq!(band_1k(verifier_s12), 11.0);
    // "At 4,000 cases the batch alone is cleared in more than 99% of batches
    // for about 3e-4 to 4e-4, 0.11 to 0.15 of a band." s = 0.010 cost
    // 2.67e-4 (99.6%) and s = 0.011 cost 3.70e-4 (100%); the verifier found
    // 2.85e-4 (99.4%) at s = 0.010.
    let (cost_s10, verifier_s10) = (2.67e-4_f64, 2.85e-4_f64);
    assert_eq!((cost_s10 * 1e4).round(), 3.0);
    assert_eq!((verifier_s10 * 1e4).round(), 3.0);
    assert_eq!(band_4k(cost_s10), 11.0);
    assert_eq!(band_4k(cost_s11), 15.0);
    // "The probe comparison with R_k's fingerprint then binds: clearing both
    // in more than 99% of cohorts costs about 4.5e-4 to 5.5e-4, about a
    // fifth of a band at 4,000 cases." With the probe, s = 0.010 cleared
    // 74.8% and s = 0.011 91.4%; s = 0.012 (4.59e-4) cleared 100%, and the
    // verifier's s = 0.012 (5.5e-4) 99.7%.
    assert_eq!((cost_s12 * 2e4).round() / 2.0, 4.5);
    assert_eq!(band_4k(cost_s12), 19.0);
    assert_eq!(band_4k(verifier_s12), 22.0);
    assert_eq!(
        ((cost_s12 / w_4k + verifier_s12 / w_4k) / 2.0 * 10.0).round(),
        2.0
    );
    // "A 50/50 blend of two published seeds of the launch recipe scored about
    // 0.2 to 0.3 of a band above R_0 at 1,000 cases and cleared the gate
    // against both in every batch." Faux-17 with Faux-29 gained 9.38e-4
    // (1.5% of headroom), with Faux-41 1.41e-3 (2.3%).
    let (blend_29, blend_41) = (9.38e-4_f64, 1.41e-3_f64);
    assert_eq!(band_1k(blend_29), 19.0);
    assert_eq!(band_1k(blend_41), 29.0);
    assert_eq!((blend_29 / headroom * 1000.0).round() / 10.0, 1.5);
    assert_eq!((blend_41 / headroom * 1000.0).round() / 10.0, 2.3);
    // "A tolerance large enough to price an escape at a full band would be
    // about 0.07."
    assert_eq!(((w_1k + recalibration_gain).sqrt() * 100.0).round(), 7.0);
    // The closest independent pair (0.0336 at the 1,024-document probe size)
    // leaves a 1.34x margin over d.
    assert_eq!((0.0336 / d * 100.0).round(), 134.0);
    // "With r = 0.85, an honest cluster that shares a band with one such copy
    // receives 54.1% of the band's share."
    let r = f64::from(DECAY_BPS) / 1e4;
    assert_eq!((1.0 / (1.0 + r) * 1000.0).round(), 541.0);
    // The score clause's w / 2 is 125 near-equal tolerances.
    assert_eq!(
        f64::from(NEAR_EQUAL_DIVISOR) / f64::from(IMPROVEMENT_DIVISOR),
        125.0
    );
}

#[test]
fn decay_credits_in_percent() {
    let r = f64::from(DECAY_BPS) / 1e4;
    let pct = |x: f64| (x * 1000.0).round() / 10.0;
    assert_eq!(pct(1.0 / (1.0 + r)), 54.1);
    assert_eq!(pct(r / (1.0 + r)), 45.9);
    assert_eq!(pct(1.0 / (1.0 + r) / (1.0 + r)), 29.2);
    assert_eq!(pct(r / (1.0 + r) / (1.0 + r)), 24.8);
    assert_eq!(pct((1.0 - r) / (1.0 - r.powi(7))), 22.1);
    // The fallback example: 0.9400 + 0.15 (0.9500 - 0.9400) = 0.9415.
    let f = f64::from(FALLBACK_BPS) / 1e4;
    assert!((0.94 + f * (0.95 - 0.94) - 0.9415).abs() < 1e-12);
}

#[test]
fn daily_cadence_lease_and_payout_delay() {
    let schedule = CohortSchedule {
        tempos: 20,
        admission_min: 1,
        transition_min: 2,
        lease_tempos: 40,
    };
    schedule.validate().expect("launch schedule");
    let tempo_minutes = 360.0 * 12.0 / 60.0;
    assert_eq!(tempo_minutes, 72.0);
    let cohort_hours = f64::from(schedule.tempos) * tempo_minutes / 60.0;
    assert_eq!(cohort_hours, 24.0); // one cohort per day, 20 commitments per day
    assert_eq!(
        f64::from(schedule.lease_tempos) * tempo_minutes / 60.0,
        48.0
    );
    // The launch authorization's "40 tempos", two launch cohorts: now only
    // the distance of its non-binding renew-by hint (no authorization
    // expires; the whitepaper's lease wording awaits its no-expiry update).
    assert_eq!(LAUNCH_RENEW_BY_TEMPOS, u64::from(schedule.lease_tempos));
    assert_eq!(LAUNCH_RENEW_BY_TEMPOS as f64 * tempo_minutes / 60.0, 48.0);
    // "A package finished by tempo 18": block 6,480 of the cohort.
    assert_eq!(18 * 360, 6480);
    // First payout: a commitment at the admission cutoff waits until the
    // closing boundary (K - 1 tempos), one reveal tempo and one epoch; one
    // made just after the cutoff waits a further cohort.
    let early = f64::from(schedule.tempos - schedule.admission_min + 2) * tempo_minutes / 60.0;
    let late = early + cohort_hours;
    assert_eq!((early.round(), late.round()), (25.0, 49.0)); // "about 25-50 hours"
    // Immunity must exceed the payout delay; on testnet subnet 601 it is
    // 21,600 blocks, 72 hours.
    let immunity_hours = 21_600.0 * 12.0 / 3600.0;
    assert_eq!(immunity_hours, 72.0);
    assert!(immunity_hours > late);
    // Rehearsal manifests may use K = 4.
    CohortSchedule {
        tempos: 4,
        lease_tempos: 8,
        ..schedule
    }
    .validate()
    .expect("rehearsal schedule");
}

#[test]
fn batch_mix_and_human_stock() {
    assert_eq!(
        (
            LAUNCH_ORIGIN_MIX.human_only,
            LAUNCH_ORIGIN_MIX.mixed,
            LAUNCH_ORIGIN_MIX.model_only
        ),
        (20, 55, 25)
    );
    LAUNCH_ORIGIN_MIX.validate().expect("launch mix");
    // In steady state only pure-human negatives consume stock: 0.2 n per
    // daily cohort, 73,000 documents a year at 1,000 cases and 292,000 at 4,000.
    let per_year = |n: u64| u64::from(LAUNCH_ORIGIN_MIX.human_only) * n / 100 * 365;
    assert_eq!(per_year(1000), 73_000);
    assert_eq!(per_year(4000), 292_000);
}
