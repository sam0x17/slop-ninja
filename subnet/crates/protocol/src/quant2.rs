//! `quant-v2`: score quantization with discrete Gaussian release noise sized
//! to a headroom target under a per-cohort zCDP cap.
//!
//! As under `quant-v1` ([`crate::quant`]), everything published about an
//! exact score `S` is its quantized value
//!
//! ```text
//! q = w * floor((S + eta + o) / w), clamped to [0, w * floor(1 / w)]
//! ```
//!
//! with these differences:
//!
//! - **Lattice.** Noise, offsets and scales live on the lattice of one ppb of
//!   a score ([`UNIT`] = 10^9 score units). Because `w`, `o` and `eta` are
//!   multiples of [`UNIT`], `q` depends on `S` only through
//!   `S_ppb = floor(S / UNIT)`, an integer query whose sensitivity to one
//!   replaced batch document is `Delta_ppb = ceil(ceil(10^18 / n) / UNIT)`
//!   ([`sensitivity_ppb`]).
//! - **Noise.** `eta = UNIT * Y` with `Y` drawn exactly from the discrete
//!   Gaussian `N_Z(0, sigma_v^2)` by the sampler of Canonne, Kamath and
//!   Steinke (2020) ([`gaussian`]): `P[Y = y]` is proportional to
//!   `exp(-y^2 / (2 sigma_v^2))`, and its variance is below `sigma_v^2`.
//! - **Scale.** `sigma_v = max(sigma_T, sigma_rho, 1)` ([`Scale::derive`]):
//!   `sigma_T` is the largest scale whose total release error
//!   `sigma^2 + w^2 / 6` stays within `t_H` of the signed reference headroom
//!   `H` ([`sigma_target`]); `sigma_rho` is the privacy floor, the smallest
//!   scale at which the `m` released values spend at most `rho_max`
//!   ([`sigma_floor`]). The spent `rho = m Delta_ppb^2 / (2 sigma_v^2)` is
//!   published (in millionths, rounded up).
//! - **Grid.** `w = ladder(sigma_v * UNIT)`: public, from the plan's values,
//!   `n`, `m` and the signed `H`, with no measured SE.
//! - **Bits.** The counted bits are information-based ([`Budget2`]): 3 per
//!   executed entrant (withheld, or scored with the gate and copy-gate bits),
//!   `ceil(log2 Bell(X))` for the cluster partition, `ceil(log2(X + 1))` for
//!   the promotion, and an upper bound on the information each noisy value
//!   carries about the batch ([`noisy_bits`]). The grid width is public and
//!   adds no term.
//!
//! Keys use new domains (`.../offset/v3`, `.../noise/v3`), so no quant-v2
//! draw equals a quant-v1 draw even under a reused manifest digest; the
//! stream blocks are unchanged. Subjects stay keyed by inference content, so
//! identical copies share one draw.
//!
//! Everything here is integer arithmetic on fixed-size values: no floating
//! point and no heap, so native, `wasm32` and every replay agree exactly.

use crate::quant::{
    self, CohortBinding, CountedTerms, Draw, Stream, Subject, U256, Words, bern_exp, ceil_isqrt,
    ceil_log2, dlap, ladder, uniform,
};
use crate::records_v2::{MAX_ROSTER, QuantParamsV2};
use crate::{Digest, Error, Result, SCORE_SCALE, domain_hash};

/// The quantization rule's identifier, as published.
pub const QUANT_RULE: &str = "quant-v2";
/// Offset key domain.
pub const OFFSET_DOMAIN: &str = "slop_ninja.quant/offset/v3";
/// Noise key domain.
pub const NOISE_DOMAIN: &str = "slop_ninja.quant/noise/v3";
/// The noise lattice: one ppb of a score, in score units.
pub const UNIT: u64 = 1_000_000_000;
/// The largest noise scale, in ppb: one score unit. It is the sampler's
/// domain, where every intermediate fits `u128`, not a tuned cap.
pub const MAX_SIGMA_PPB: u64 = 1_000_000_000;
/// The largest signed headroom, in ppb.
pub const MAX_HEADROOM_PPB: u32 = 1_000_000_000;
/// Bits of one executed entrant's deterministic outcome: `ceil(log2 5)`
/// (withheld, or scored with the gate bit and the copy-gate bit).
pub const ENTRANT_BITS: u32 = 3;
/// Fractional bits of the information counts ([`log2_upper_q20`]).
pub const Q20: u32 = 20;

// ---------------------------------------------------------------------------
// Sensitivity and the sampler.

/// `Delta_ppb = ceil(Delta / UNIT)` with `Delta = ceil(10^18 / n)`: two
/// scores within `Delta` have `S_ppb` within `Delta_ppb`.
pub fn sensitivity_ppb(n: u32) -> Result<u64> {
    let delta = quant::sensitivity(n)?;
    Ok(delta.div_ceil(u128::from(UNIT)) as u64)
}

/// A Bernoulli draw with success probability `exp(-num / den)` for any
/// `num >= 0`, `den >= 1`: CKS Algorithm 1 for `gamma > 1`. With
/// `(k, r) = divmod(num, den)` it needs `k` successes of
/// `Bernoulli(exp(-1))` and then one of `Bernoulli(exp(-r / den))`.
pub fn bern_exp_rational(words: &mut impl Words, num: U256, den: u128) -> Result<bool> {
    let (mut k, r) = num.div_rem_u128(den).ok_or(Error::Invalid("bernoulli parameter"))?;
    let one = U256::from_u128(1);
    while !k.is_zero() {
        if !bern_exp(words, 1, 1)? {
            return Ok(false);
        }
        k = k.checked_sub(one).ok_or(Error::Overflow("bernoulli"))?;
    }
    bern_exp(words, r, den)
}

/// An exact draw from the discrete Gaussian `N_Z(0, s^2)` for an integer
/// scale `1 <= s <= MAX_SIGMA_PPB`: CKS Algorithm 3. With `t = s + 1`, it
/// proposes `Y` from the discrete Laplace of scale `t` ([`dlap`]) and accepts
/// it with probability `exp(-(|Y| - s^2 / t)^2 / (2 s^2))`
/// ([`bern_exp_rational`]). Overflow of `|Y| t` is reported, never clamped;
/// its probability is below `exp(-2^128 / t^2)`, about `exp(-3e20)` at the
/// largest scale.
pub fn gaussian(words: &mut impl Words, s: u64) -> Result<i128> {
    if s == 0 || s > MAX_SIGMA_PPB {
        return Err(Error::OutOfRange("gaussian scale"));
    }
    let s = u128::from(s);
    let t = s + 1;
    let s2 = s * s;
    let den = 2 * s2 * t * t;
    loop {
        let y = dlap(words, t, 1)?;
        let d = y
            .unsigned_abs()
            .checked_mul(t)
            .ok_or(Error::Overflow("gaussian"))?
            .abs_diff(s2);
        if bern_exp_rational(words, U256::mul_u128(d, d), den)? {
            return Ok(y);
        }
    }
}

// ---------------------------------------------------------------------------
// The scale.

/// `w_ppb(s) = ladder(s UNIT) / UNIT`, and 0 at `s = 0`.
fn width_ppb(s: u64) -> Result<u128> {
    if s == 0 {
        return Ok(0);
    }
    let w = ladder(u128::from(s) * u128::from(UNIT))?;
    Ok(u128::from(w / UNIT))
}

/// `sigma_T`: the largest integer `s` in `[0, H]` with
/// `6e6 s^2 + 1e6 w_ppb(s)^2 <= 6 (t_H H)^2`, which is
/// `sigma^2 + w^2 / 6 <= (t_H H / 1000)^2`: the noise variance plus the
/// dithered floor's error averaged over the cell. The left side is monotone
/// in `s`, so a binary search finds it. `H = 0` gives 0, never a refusal.
pub fn sigma_target(noise_target_h_milli: u16, headroom_ppb: u32) -> Result<u64> {
    let bound = 6 * (u128::from(noise_target_h_milli) * u128::from(headroom_ppb)).pow(2);
    let fits = |s: u64| -> Result<bool> {
        let w = width_ppb(s)?;
        Ok(6_000_000 * u128::from(s).pow(2) + 1_000_000 * w * w <= bound)
    };
    let (mut lo, mut hi) = (0u64, u64::from(headroom_ppb));
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(mid)? {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Ok(lo)
}

/// `sigma_rho`: the smallest `s >= 1` with
/// `2 s^2 rho_max_milli >= 1000 m Delta_ppb^2`, so that `m` values with
/// sensitivity `Delta_ppb` each spend at most `rho_max`. It may exceed
/// [`MAX_SIGMA_PPB`]; [`Scale`] refuses that.
pub fn sigma_floor(n: u32, m: u32, rho_max_milli: u32) -> Result<u64> {
    if m == 0 || rho_max_milli == 0 {
        return Err(Error::Invalid("privacy floor"));
    }
    let delta = u128::from(sensitivity_ppb(n)?);
    let num = 1000u128
        .checked_mul(u128::from(m))
        .and_then(|v| v.checked_mul(delta * delta))
        .ok_or(Error::Overflow("privacy floor"))?;
    let s2 = num.div_ceil(2 * u128::from(rho_max_milli));
    u64::try_from(ceil_isqrt(s2).max(1)).map_err(|_| Error::Overflow("privacy floor"))
}

/// One cohort's noise scale and grid ([`Scale::derive`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scale {
    /// `sigma_T`, in ppb.
    pub target: u64,
    /// `sigma_rho`, in ppb.
    pub floor: u64,
    /// `sigma_v = max(sigma_T, sigma_rho, 1)`, in ppb.
    pub sigma: u64,
    /// The `rho` the release costs, `m Delta_ppb^2 / (2 sigma_v^2)`, in
    /// millionths, rounded up.
    pub rho_micro: u64,
    /// `w = ladder(sigma_v UNIT)`, in score units.
    pub width: u64,
}

impl Scale {
    /// The scale of a cohort with batch size `n` and `m` charged values under
    /// `params` (its signed `H`).
    pub fn derive(n: u32, m: u32, params: &QuantParamsV2) -> Result<Self> {
        let target = sigma_target(params.noise_target_h_milli, params.headroom_pub_ppb)?;
        Self::with_target(n, m, params.rho_max_milli, target)
    }

    /// The scale as if `sigma_T` were 0: the plan-time worst case, whose
    /// bits no signed `H` can exceed.
    pub fn floor_only(n: u32, m: u32, params: &QuantParamsV2) -> Result<Self> {
        Self::with_target(n, m, params.rho_max_milli, 0)
    }

    fn with_target(n: u32, m: u32, rho_max_milli: u32, target: u64) -> Result<Self> {
        let floor = sigma_floor(n, m, rho_max_milli)?;
        let sigma = target.max(floor).max(1);
        if sigma > MAX_SIGMA_PPB {
            return Err(Error::OutOfRange("noise scale above one score unit"));
        }
        let delta = u128::from(sensitivity_ppb(n)?);
        let cost = u128::from(m) * delta * delta;
        let s2 = u128::from(sigma) * u128::from(sigma);
        if 1000 * cost > 2 * s2 * u128::from(rho_max_milli) {
            return Err(Error::Invalid("privacy cap"));
        }
        let rho_micro = u64::try_from((1_000_000 * cost).div_ceil(2 * s2))
            .map_err(|_| Error::Overflow("rho"))?;
        let width = ladder(u128::from(sigma) * u128::from(UNIT))?;
        Ok(Self {
            target,
            floor,
            sigma,
            rho_micro,
            width,
        })
    }

    /// Checks a recorded scale against the one `params` give.
    pub fn check(&self, n: u32, m: u32, params: &QuantParamsV2) -> Result<()> {
        if *self != Self::derive(n, m, params)? {
            return Err(Error::Mismatch("noise scale"));
        }
        Ok(())
    }
}

/// The narrowest grid a cohort of `executed` entrants can have under
/// `params`: the floor's ladder, as if `sigma_T` were 0.
pub fn narrowest_width(n: u32, executed: u16, predecessor: bool, params: &QuantParamsV2) -> Result<u64> {
    Ok(Scale::floor_only(n, quant::charged(executed, predecessor), params)?.width)
}

// ---------------------------------------------------------------------------
// Keys and the draw.

fn subject_key(domain: &str, seed: &[u8; 32], cohort: &CohortBinding, subject: &Subject) -> Digest {
    let mut payload = [0u8; 64 + quant::SUBJECT_LEN];
    payload[..32].copy_from_slice(seed);
    payload[32..64].copy_from_slice(&cohort.0.0);
    payload[64..].copy_from_slice(&subject.encode());
    domain_hash(domain, &payload)
}

/// `K_off = H("slop_ninja.quant/offset/v3", seed || manifest || subj)`.
pub fn offset_key(seed: &[u8; 32], cohort: &CohortBinding, subject: &Subject) -> Digest {
    subject_key(OFFSET_DOMAIN, seed, cohort, subject)
}

/// `K_noise = H("slop_ninja.quant/noise/v3", seed || manifest || subj)`.
pub fn noise_key(seed: &[u8; 32], cohort: &CohortBinding, subject: &Subject) -> Digest {
    subject_key(NOISE_DOMAIN, seed, cohort, subject)
}

/// A subject's draw: the offset `UNIT * uniform(w / UNIT)` and the noise
/// `UNIT * gaussian(sigma_v)`, each from its own stream. Both are multiples
/// of [`UNIT`], so [`quant::quantize`] reads `S` only through `S_ppb`.
pub fn draw(seed: &[u8; 32], cohort: &CohortBinding, subject: &Subject, scale: &Scale) -> Result<Draw> {
    let cells = scale.width / UNIT;
    if cells == 0 || !scale.width.is_multiple_of(UNIT) {
        return Err(Error::Invalid("grid width"));
    }
    let offset = uniform(&mut Stream::new(offset_key(seed, cohort, subject)), u128::from(cells))? as u64 * UNIT;
    let y = gaussian(&mut Stream::new(noise_key(seed, cohort, subject)), scale.sigma)?;
    let noise = y.checked_mul(i128::from(UNIT)).ok_or(Error::Overflow("noise"))?;
    Ok(Draw { offset, noise })
}

/// Whether a recorded draw lies on the lattice of `scale`: the offset below
/// `w`, and offset and noise multiples of [`UNIT`].
pub fn draw_on_lattice(draw: &Draw, scale: &Scale) -> bool {
    draw.offset < scale.width
        && draw.offset.is_multiple_of(UNIT)
        && draw.noise.rem_euclid(i128::from(UNIT)) == 0
}

// ---------------------------------------------------------------------------
// The information-based bit count.

/// `ceil(log2 Bell(X))` for `X = 0..=MAX_ROSTER`: the bits of any partition
/// of `X` labelled entrants. A test rebuilds the table from the Bell triangle.
pub const BELL_BITS: [u16; MAX_ROSTER as usize + 1] = [
    0, 0, 1, 3, 4, 6, 8, 10, 13, 15, 17, 20, 23, 25, 28, 31,
    34, 37, 40, 43, 46, 49, 53, 56, 59, 63, 66, 69, 73, 76, 80, 84,
    87, 91, 95, 98, 102, 106, 110, 114, 117, 121, 125, 129, 133, 137, 141, 145,
    149, 153, 158, 162, 166, 170, 174, 178, 183, 187, 191, 195, 200, 204, 208, 213,
    217, 222, 226, 230, 235, 239, 244, 248, 253, 257, 262, 266, 271, 276, 280, 285,
    289, 294, 299, 303, 308, 313, 318, 322, 327, 332, 337, 341, 346, 351, 356, 360,
    365, 370, 375, 380, 385, 390, 395, 399, 404, 409, 414, 419, 424, 429, 434, 439,
    444, 449, 454, 459, 464, 469, 474, 479, 485, 490, 495, 500, 505, 510, 515, 520,
    526, 531, 536, 541, 546, 551, 557, 562, 567, 572, 578, 583, 588, 593, 599, 604,
    609, 614, 620, 625, 630, 636, 641, 646, 652, 657, 662, 668, 673, 679, 684, 689,
    695, 700, 706, 711, 717, 722, 727, 733, 738, 744, 749, 755, 760, 766, 771, 777,
    782, 788, 793, 799, 804, 810, 816, 821, 827, 832, 838, 843, 849, 855, 860, 866,
    872, 877, 883, 888, 894, 900, 905, 911, 917, 922, 928, 934, 939, 945, 951, 956,
    962, 968, 974, 979, 985, 991, 997, 1002, 1008, 1014, 1020, 1025, 1031, 1037, 1043, 1049,
    1054, 1060, 1066, 1072, 1078, 1083, 1089, 1095, 1101, 1107, 1113, 1118, 1124, 1130, 1136, 1142,
    1148, 1154, 1160, 1165, 1171, 1177, 1183, 1189, 1195, 1201, 1207, 1213, 1219, 1225, 1231, 1236,
    1242,
];

/// An upper bound on `log2(num / den) 2^20`, for `den <= num < 2^62`,
/// `den > 0`: with `2^k den <= num < 2^(k+1) den`, the mantissa
/// `x = num / (den 2^k)` in `[1, 2)` is kept with 62 fractional bits rounded
/// up, squared 20 times (rounding up, halving and recording 1 whenever it
/// reaches 2), and the result is `k 2^20 + bits + 1`. Every rounding goes
/// up, so it is an upper bound, within `2^-19` of the true value.
pub fn log2_upper_q20(num: u64, den: u64) -> Result<u64> {
    const ONE: u128 = 1 << 62;
    const TWO: u128 = 1 << 63;
    if den == 0 || num < den || num >= 1 << 62 {
        return Err(Error::OutOfRange("log2 argument"));
    }
    let (num, den) = (u128::from(num), u128::from(den));
    let mut k = 0u32;
    while den << (k + 1) <= num {
        k += 1;
    }
    let mut y = (num << 62).div_ceil(den << k);
    let mut bits = 0u64;
    for _ in 0..Q20 {
        y = (y * y).div_ceil(ONE);
        bits <<= 1;
        if y >= TWO {
            bits |= 1;
            y = y.div_ceil(2);
        }
    }
    Ok((u64::from(k) << Q20) + bits + 1)
}

/// `V`, in ppb^2: the batch-mean variance bound `10^18 / (4 n)` (a mean of
/// `n` utilities in `[0, 1]`), its square root widened by one lattice step
/// for the floor to `S_ppb`, squared, plus one for the integer-entropy term.
pub fn value_variance_bound(n: u32) -> Result<u64> {
    if n == 0 {
        return Err(Error::Invalid("empty batch"));
    }
    let sd = ceil_isqrt((SCORE_SCALE as u128).div_ceil(4 * u128::from(n))) + 1;
    u64::try_from(sd * sd + 1).map_err(|_| Error::Overflow("value variance"))
}

/// The information one noisy value carries about the batch, in units of
/// `2^-20` bits, rounded up: `ceil(L / 2) + 1` with
/// `L = log2_upper_q20(sigma^2 + V, sigma^2)`. It bounds
/// `I(batch; q) <= H(Y) - H(Z) <= 1/2 log2(1 + (V + 1/12) / sigma^2) + theta`
/// (Cover and Thomas, ch. 8, for integer `Y`; `theta < 1e-15` for
/// `sigma >= 1`, covered by the trailing one).
pub fn value_bits_q20(sigma: u64, n: u32) -> Result<u64> {
    if sigma == 0 || sigma > MAX_SIGMA_PPB {
        return Err(Error::OutOfRange("noise scale"));
    }
    let s2 = sigma * sigma;
    let v = value_variance_bound(n)?;
    let l = log2_upper_q20(s2.checked_add(v).ok_or(Error::Overflow("value bits"))?, s2)?;
    Ok(l.div_ceil(2) + 1)
}

/// The `m` noisy values' information: `(m u, ceil(m u / 2^20))`, the count in
/// `2^-20` bits and in whole bits. The noise is independent per value, so
/// the bounds add; identical copies share a draw and only overcount.
pub fn noisy_bits(m: u32, sigma: u64, n: u32) -> Result<(u64, u32)> {
    let q20 = u64::from(m)
        .checked_mul(value_bits_q20(sigma, n)?)
        .ok_or(Error::Overflow("noisy bits"))?;
    let bits = u32::try_from(q20.div_ceil(1 << Q20)).map_err(|_| Error::Overflow("noisy bits"))?;
    Ok((q20, bits))
}

/// Every counted term of a cohort's published bits under `quant-v2`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Budget2 {
    /// `X`, executed entrants.
    pub executed: u16,
    /// `sigma_v`, in ppb.
    pub sigma_ppb: u64,
    /// The spent `rho`, in millionths.
    pub rho_micro: u64,
    /// `3 X` ([`ENTRANT_BITS`] per executed entrant).
    pub deterministic: u32,
    /// `ceil(log2 Bell(X))`.
    pub clusters: u32,
    /// `ceil(log2(X + 1))`.
    pub promotion: u32,
    /// The noisy values' information in `2^-20` bits ([`noisy_bits`]).
    pub noisy_q20: u64,
    /// `ceil(noisy_q20 / 2^20)`.
    pub noisy: u32,
    pub total: u32,
}

impl Budget2 {
    pub fn count(executed: u16, predecessor: bool, n: u32, scale: &Scale) -> Result<Self> {
        if executed > MAX_ROSTER {
            return Err(Error::OutOfRange("roster size"));
        }
        let m = quant::charged(executed, predecessor);
        let deterministic = ENTRANT_BITS * u32::from(executed);
        let clusters = u32::from(BELL_BITS[usize::from(executed)]);
        let promotion = ceil_log2(u128::from(executed) + 1);
        let (noisy_q20, noisy) = noisy_bits(m, scale.sigma, n)?;
        let total = deterministic
            .checked_add(clusters)
            .and_then(|t| t.checked_add(promotion))
            .and_then(|t| t.checked_add(noisy))
            .ok_or(Error::Overflow("bit budget"))?;
        Ok(Self {
            executed,
            sigma_ppb: scale.sigma,
            rho_micro: scale.rho_micro,
            deterministic,
            clusters,
            promotion,
            noisy_q20,
            noisy,
            total,
        })
    }

    /// `total * F <= n * h`.
    pub fn within(&self, n: u32, identify_bits: u16, budget_factor: u16) -> bool {
        quant::within_bound(self.total, n, identify_bits, budget_factor)
    }
}

/// A cohort's complete counted bits under `quant-v2`: [`Budget2::count`] plus
/// every other rule's [`CountedTerms`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CohortBits2 {
    pub quant: Budget2,
    pub terms: u32,
    pub total: u32,
}

impl CohortBits2 {
    /// `total * F <= n * h`.
    pub fn within(&self, n: u32, identify_bits: u16, budget_factor: u16) -> bool {
        quant::within_bound(self.total, n, identify_bits, budget_factor)
    }
}

/// The cohort's complete counted bits at `scale`.
pub fn cohort_bits(
    executed: u16,
    predecessor: bool,
    n: u32,
    scale: &Scale,
    terms: &CountedTerms,
) -> Result<CohortBits2> {
    let quant = Budget2::count(executed, predecessor, n, scale)?;
    let extra = terms.bits(executed)?;
    let total = quant.total.checked_add(extra).ok_or(Error::Overflow("bit budget"))?;
    Ok(CohortBits2 { quant, terms: extra, total })
}

/// The plan-time worst case over every `X` up to `roster_limit` and both
/// reference shapes, each at the floor-only scale ([`Scale::floor_only`]).
/// Bits only fall as `sigma` rises, so a plan that passes is never refused at
/// the seal, whatever `H` turns out to be. A plan whose floor at the cap
/// exceeds [`MAX_SIGMA_PPB`] (an `n` too small for its cap at that
/// `rho_max`) is refused with the scale's error. Returns the largest total
/// and whether every case fits.
pub fn worst_case_terms(
    n: u32,
    roster_limit: u16,
    params: &QuantParamsV2,
    terms: &CountedTerms,
) -> Result<(CohortBits2, bool)> {
    let mut worst = CohortBits2::default();
    let mut ok = true;
    for x in 0..=roster_limit {
        for predecessor in [false, true] {
            let scale = Scale::floor_only(n, quant::charged(x, predecessor), params)?;
            let bits = cohort_bits(x, predecessor, n, &scale, terms)?;
            ok &= bits.within(n, params.identify_bits, params.budget_factor);
            if bits.total > worst.total {
                worst = bits;
            }
        }
    }
    Ok((worst, ok))
}
