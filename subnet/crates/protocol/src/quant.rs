//! `quant-v1`: score quantization for the public record of a cohort.
//!
//! The coordinator computes exact scores `S` privately. Everything published
//! about a score is its quantized value
//!
//! ```text
//! q = w * floor((S + eta + o) / w), clamped to [0, w * floor(1 / w)]
//! ```
//!
//! The upper end is the largest grid value at most 1 ([`quantize`]).
//!
//! - `w` is the grid width: `max(2 SE, w_floor)` rounded up to one significant
//!   decimal digit ([`ladder`]), where `SE` is the sampling standard error of
//!   `R_k`'s score on the cohort batch ([`SeMeasure`]) and
//!   `w_floor = ceil(tau)` is the noise scale;
//! - `eta` is exact discrete-Laplace noise of scale `tau = Delta / eps_v`,
//!   `Delta = ceil(10^18 / n)`, `eps_v = eps_total / m`, drawn with the exact
//!   sampler of Canonne, Kamath and Steinke (2020) ([`dlap`]);
//! - `o` is a secret uniform offset in `[0, w)` ([`uniform`]);
//! - both come from a stream keyed by the private cohort seed, the cohort's
//!   manifest digest and the subject's inference content ([`Subject`],
//!   [`Stream`]), so the coordinator can reproduce every draw and nobody else
//!   can, no draw depends on any other subject or on processing order, copies
//!   of one model submitted under several hotkeys share one draw (and so one
//!   `q`), and no two cohorts share a draw even under a reused label or seed.
//!
//! The published bit budget ([`Budget`]) bounds the deterministic hidden-data
//! values a cohort releases besides the noised scores.
//!
//! Everything here is integer arithmetic on fixed-size values: no floating
//! point and no heap, so native, `wasm32` and every replay agree exactly.

use crate::{Digest, Error, Result, SCORE_SCALE, domain_hash};

/// The quantization rule's identifier, as published.
pub const QUANT_RULE: &str = "quant-v1";
/// Offset key domain.
pub const OFFSET_DOMAIN: &str = "slop_ninja.quant/offset/v2";
/// Noise key domain.
pub const NOISE_DOMAIN: &str = "slop_ninja.quant/noise/v2";
/// Stream block domain.
pub const STREAM_DOMAIN: &str = "slop_ninja.quant/stream/v1";

const SCALE: u128 = SCORE_SCALE as u128;
/// Decimal digits of [`SCORE_SCALE`].
const SCALE_DIGITS: u32 = 18;
/// Values on the one-digit ladder over `[10^-18, 1]`: nine digits in each of
/// the 18 decades below 1, and 1 itself.
pub const LADDER_VALUES: u128 = 9 * SCALE_DIGITS as u128 + 1;

// ---------------------------------------------------------------------------
// 256-bit integers for the SE sums.

/// An unsigned 256-bit integer, `hi * 2^128 + lo`, with no heap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct U256 {
    pub hi: u128,
    pub lo: u128,
}

impl U256 {
    pub const ZERO: Self = Self { hi: 0, lo: 0 };

    pub const fn from_u128(v: u128) -> Self {
        Self { hi: 0, lo: v }
    }

    /// `a * b`, exact.
    pub fn mul_u128(a: u128, b: u128) -> Self {
        let mask = u128::from(u64::MAX);
        let (a1, a0) = (a >> 64, a & mask);
        let (b1, b0) = (b >> 64, b & mask);
        let p00 = a0 * b0;
        let p01 = a0 * b1;
        let p10 = a1 * b0;
        let p11 = a1 * b1;
        let mid = (p00 >> 64) + (p01 & mask) + (p10 & mask);
        let lo = (p00 & mask) | (mid << 64);
        let hi = p11 + (p01 >> 64) + (p10 >> 64) + (mid >> 64);
        Self { hi, lo }
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        let (lo, carry) = self.lo.overflowing_add(other.lo);
        let hi = self.hi.checked_add(other.hi)?.checked_add(u128::from(carry))?;
        Some(Self { hi, lo })
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        let (lo, borrow) = self.lo.overflowing_sub(other.lo);
        let hi = self.hi.checked_sub(other.hi)?.checked_sub(u128::from(borrow))?;
        Some(Self { hi, lo })
    }

    /// `self * m`, checked.
    pub fn checked_mul_u128(self, m: u128) -> Option<Self> {
        let low = Self::mul_u128(self.lo, m);
        let high = Self::mul_u128(self.hi, m);
        if high.hi != 0 {
            return None;
        }
        low.checked_add(Self { hi: high.lo, lo: 0 })
    }

    /// `ceil(self / d)` when it fits in `u128`, for `d > 0`.
    pub fn div_ceil_u128(self, d: u128) -> Option<u128> {
        if d == 0 {
            return None;
        }
        // Restoring long division, one bit at a time; the remainder stays
        // below `d`, so `rem << 1 | bit` fits in 129 bits, held as a flag.
        let mut quotient = Self::ZERO;
        let mut rem: u128 = 0;
        for i in (0..256).rev() {
            let bit = if i >= 128 {
                (self.hi >> (i - 128)) & 1
            } else {
                (self.lo >> i) & 1
            };
            let carry = rem >> 127;
            rem = (rem << 1) | bit;
            let set = carry == 1 || rem >= d;
            if set {
                rem = rem.wrapping_sub(d);
                if i >= 128 {
                    quotient.hi |= 1 << (i - 128);
                } else {
                    quotient.lo |= 1 << i;
                }
            }
        }
        if quotient.hi != 0 {
            return None;
        }
        if rem == 0 {
            Some(quotient.lo)
        } else {
            quotient.lo.checked_add(1)
        }
    }
}

impl crate::codec::Canonical for U256 {
    fn encode_to(&self, e: &mut crate::codec::Encoder) {
        e.u128(self.hi);
        e.u128(self.lo);
    }
    fn decode_from(d: &mut crate::codec::Decoder<'_>) -> Result<Self> {
        Ok(Self {
            hi: d.u128()?,
            lo: d.u128()?,
        })
    }
}

// ---------------------------------------------------------------------------
// SE, the noise scale and the grid width.

/// The sampling standard error of `R_k`'s batch score, in score units:
/// `S1 = sum u`, `S2 = sum u^2`, `V = ceil((n S2 - S1^2) / (n^2 (n - 1)))`,
/// `SE = ceil_isqrt(V)`. Every rounding goes up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeMeasure {
    pub n: u32,
    pub s1: u128,
    pub s2: U256,
    pub se: u64,
}

impl SeMeasure {
    /// Measures the per-document utilities `u_t <= 10^18` of one model, in
    /// task order. Refuses fewer than two documents.
    pub fn measure(utilities: impl IntoIterator<Item = u64>) -> Result<Self> {
        let mut n: u32 = 0;
        let mut s1: u128 = 0;
        let mut s2 = U256::ZERO;
        for u in utilities {
            if u128::from(u) > SCALE {
                return Err(Error::OutOfRange("utility"));
            }
            n = n.checked_add(1).ok_or(Error::Overflow("se count"))?;
            s1 = s1
                .checked_add(u128::from(u))
                .ok_or(Error::Overflow("se sum"))?;
            s2 = s2
                .checked_add(U256::mul_u128(u128::from(u), u128::from(u)))
                .ok_or(Error::Overflow("se squares"))?;
        }
        Self::from_sums(n, s1, s2)
    }

    /// Recomputes `SE` from recorded sums.
    pub fn from_sums(n: u32, s1: u128, s2: U256) -> Result<Self> {
        if n < 2 {
            return Err(Error::Invalid("se needs two documents"));
        }
        let n128 = u128::from(n);
        let spread = s2
            .checked_mul_u128(n128)
            .and_then(|a| a.checked_sub(U256::mul_u128(s1, s1)))
            .ok_or(Error::Invalid("se sums"))?;
        let den = n128 * n128 * (n128 - 1);
        let v = spread.div_ceil_u128(den).ok_or(Error::Overflow("se variance"))?;
        let se = u64::try_from(ceil_isqrt(v)).map_err(|_| Error::Overflow("se"))?;
        if u128::from(se) > SCALE {
            return Err(Error::OutOfRange("se"));
        }
        Ok(Self { n, s1, s2, se })
    }
}

/// The smallest `r` with `r^2 >= v`.
pub fn ceil_isqrt(v: u128) -> u128 {
    let r = v.isqrt();
    if r * r < v { r + 1 } else { r }
}

/// `Delta = ceil(10^18 / n)`: the largest change one replaced document can
/// make to `S = floor(sum u / n)`.
pub fn sensitivity(n: u32) -> Result<u128> {
    if n == 0 {
        return Err(Error::Invalid("empty batch"));
    }
    Ok(SCALE.div_ceil(u128::from(n)))
}

/// The noise scale `tau = A / B` in score units, with `A = Delta m 1000` and
/// `B = eps_total_milli`, so `tau = Delta / eps_v` with `eps_v = eps_total / m`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tau {
    pub a: u128,
    pub b: u128,
}

impl Tau {
    pub fn new(n: u32, m: u32, eps_total_milli: u32) -> Result<Self> {
        if m == 0 || eps_total_milli == 0 {
            return Err(Error::Invalid("noise scale"));
        }
        let a = sensitivity(n)?
            .checked_mul(u128::from(m))
            .and_then(|v| v.checked_mul(1000))
            .ok_or(Error::Overflow("noise scale"))?;
        Ok(Self {
            a,
            b: u128::from(eps_total_milli),
        })
    }

    /// `w_floor = ceil(tau)`: the grid is never finer than the noise.
    pub fn floor_width(&self) -> u128 {
        self.a.div_ceil(self.b)
    }
}

/// `w_raw` rounded up to one significant decimal digit and clamped to `1`:
/// with `p` the largest power of ten at most `w_raw`, `w = p ceil(w_raw / p)`.
/// It only widens, by less than a factor of two, and every multiple of `w`
/// prints exactly with [`decimals`] digits.
pub fn ladder(w_raw: u128) -> Result<u64> {
    if w_raw == 0 {
        return Err(Error::Invalid("grid width"));
    }
    if w_raw >= SCALE {
        return Ok(SCORE_SCALE);
    }
    let mut p: u128 = 1;
    while p * 10 <= w_raw {
        p *= 10;
    }
    let w = (p * w_raw.div_ceil(p)).min(SCALE);
    Ok(w as u64)
}

/// The grid width `w = ladder(max(2 SE, w_floor))`.
pub fn width(se: u64, tau: &Tau) -> Result<u64> {
    ladder((2 * u128::from(se)).max(tau.floor_width()))
}

/// Whether `w` is on the one-digit ladder: `digit * 10^j` with digit `1..=9`
/// and `w <= 1`.
pub fn on_ladder(w: u64) -> bool {
    if w == 0 || w > SCORE_SCALE {
        return false;
    }
    let mut v = w;
    while v.is_multiple_of(10) {
        v /= 10;
    }
    v <= 9
}

/// `K = floor(10^18 / w) + 1`, the number of possible `q` values.
pub fn grid_values(w: u64) -> Result<u128> {
    if w == 0 {
        return Err(Error::Invalid("grid width"));
    }
    Ok(SCALE / u128::from(w) + 1)
}

/// `d` with `w = digit * 10^-d`: the decimals every multiple of `w` needs.
pub fn decimals(w: u64) -> Result<u32> {
    if !on_ladder(w) {
        return Err(Error::Invalid("grid width"));
    }
    let mut zeros = 0;
    let mut v = w;
    while v.is_multiple_of(10) {
        v /= 10;
        zeros += 1;
    }
    Ok(SCALE_DIGITS - zeros)
}

/// Longest grid decimal: `"1."` and 18 digits.
pub const MAX_DECIMAL_LEN: usize = 2 + SCALE_DIGITS as usize;

/// Prints a multiple of `w` with exactly [`decimals`]`(w)` decimals into
/// `buf`, for example `0.84` under `w = 0.03`. Refuses a value off the grid.
pub fn format_grid(value: u64, w: u64, buf: &mut [u8; MAX_DECIMAL_LEN]) -> Result<&str> {
    let d = decimals(w)?;
    if value > SCORE_SCALE || !value.is_multiple_of(w) {
        return Err(Error::Invalid("value off the grid"));
    }
    let whole = value / SCORE_SCALE;
    let mut frac = (value % SCORE_SCALE) / 10u64.pow(SCALE_DIGITS - d);
    buf[0] = b'0' + whole as u8;
    if d == 0 {
        return core::str::from_utf8(&buf[..1]).map_err(|_| Error::Invalid("grid decimal"));
    }
    buf[1] = b'.';
    for i in (0..d as usize).rev() {
        buf[2 + i] = b'0' + (frac % 10) as u8;
        frac /= 10;
    }
    core::str::from_utf8(&buf[..2 + d as usize]).map_err(|_| Error::Invalid("grid decimal"))
}

/// Parses a decimal printed by [`format_grid`] under `w`: exactly
/// [`decimals`]`(w)` decimals and a multiple of `w`.
pub fn parse_grid(text: &str, w: u64) -> Result<u64> {
    let d = decimals(w)? as usize;
    let bytes = text.as_bytes();
    let bad = Error::Invalid("grid decimal");
    let (whole, frac) = match bytes {
        [w0] if d == 0 => (*w0, &[][..]),
        [w0, b'.', rest @ ..] if d > 0 && rest.len() == d => (*w0, rest),
        _ => return Err(bad),
    };
    if !whole.is_ascii_digit() || !frac.iter().all(u8::is_ascii_digit) {
        return Err(bad);
    }
    let mut value = u64::from(whole - b'0') * SCORE_SCALE;
    let mut f: u64 = 0;
    for c in frac {
        f = f * 10 + u64::from(c - b'0');
    }
    value += f * 10u64.pow(SCALE_DIGITS - d as u32);
    if value > SCORE_SCALE || !value.is_multiple_of(w) {
        return Err(bad);
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// Subjects, keys and the stream.

/// Encoded length of a [`Subject`].
pub const SUBJECT_LEN: usize = 33;

/// Whose `q` a draw belongs to. Every subject is keyed by inference content,
/// never by hotkey: entrants submitting identical content get one draw, so
/// submitting copies under several hotkeys draws no extra chances (an
/// entrant's identical copy of `R_k` or `R_k-1` is published content and is
/// never executed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    /// A roster entrant, by inference-content digest.
    Entrant(Digest),
    /// `R_k`, by inference-content digest.
    Current(Digest),
    /// `R_k-1`, by inference-content digest.
    Predecessor(Digest),
}

impl Subject {
    /// `tag || content(32)`.
    pub fn encode(&self) -> [u8; SUBJECT_LEN] {
        let mut out = [0u8; SUBJECT_LEN];
        let (tag, id) = match self {
            Self::Entrant(d) => (0u8, &d.0),
            Self::Current(d) => (1, &d.0),
            Self::Predecessor(d) => (2, &d.0),
        };
        out[0] = tag;
        out[1..].copy_from_slice(id);
        out
    }
}

/// The cohort a draw belongs to: its signed manifest's digest. Binding every
/// key to it means a label, seed or subject reused on another chain record,
/// after a testnet reset or under a re-created ledger never reproduces a
/// draw, so two releases of one model never share noise that differencing
/// could cancel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CohortBinding(pub Digest);

fn subject_key(domain: &str, seed: &[u8; 32], cohort: &CohortBinding, subject: &Subject) -> Digest {
    let mut payload = [0u8; 64 + SUBJECT_LEN];
    payload[..32].copy_from_slice(seed);
    payload[32..64].copy_from_slice(&cohort.0.0);
    payload[64..].copy_from_slice(&subject.encode());
    domain_hash(domain, &payload)
}

/// `K_off = H("slop_ninja.quant/offset/v2", seed || manifest || subj)`.
pub fn offset_key(seed: &[u8; 32], cohort: &CohortBinding, subject: &Subject) -> Digest {
    subject_key(OFFSET_DOMAIN, seed, cohort, subject)
}

/// `K_noise = H("slop_ninja.quant/noise/v2", seed || manifest || subj)`.
pub fn noise_key(seed: &[u8; 32], cohort: &CohortBinding, subject: &Subject) -> Digest {
    subject_key(NOISE_DOMAIN, seed, cohort, subject)
}

/// The word stream of key `K`: block `j` is
/// `H("slop_ninja.quant/stream/v1", K || j as u64 BE)`, read as two
/// big-endian `u128` words in order.
#[derive(Clone, Debug)]
pub struct Stream {
    key: [u8; 32],
    block: u64,
    words: [u128; 2],
    next: usize,
}

impl Stream {
    pub fn new(key: Digest) -> Self {
        Self {
            key: key.0,
            block: 0,
            words: [0; 2],
            next: 2,
        }
    }

    /// Stream block `j`.
    pub fn block(key: &Digest, j: u64) -> Digest {
        let mut payload = [0u8; 40];
        payload[..32].copy_from_slice(&key.0);
        payload[32..].copy_from_slice(&j.to_be_bytes());
        domain_hash(STREAM_DOMAIN, &payload)
    }

    /// The next word.
    pub fn word(&mut self) -> u128 {
        if self.next == 2 {
            let block = Self::block(&Digest(self.key), self.block).0;
            self.block += 1;
            let mut hi = [0u8; 16];
            let mut lo = [0u8; 16];
            hi.copy_from_slice(&block[..16]);
            lo.copy_from_slice(&block[16..]);
            self.words = [u128::from_be_bytes(hi), u128::from_be_bytes(lo)];
            self.next = 0;
        }
        let w = self.words[self.next];
        self.next += 1;
        w
    }

    /// Words consumed so far.
    pub fn consumed(&self) -> u64 {
        self.block * 2 - (2 - self.next as u64)
    }
}

/// A source of uniform words: the keyed [`Stream`], or a test source.
pub trait Words {
    fn word(&mut self) -> u128;
}

impl Words for Stream {
    fn word(&mut self) -> u128 {
        Stream::word(self)
    }
}

/// A uniform draw from `[0, M)`, `M >= 1`, by rejection: accept a word
/// `x < 2^128 - (2^128 mod M)` and return `x mod M`.
pub fn uniform(words: &mut impl Words, m: u128) -> Result<u128> {
    if m == 0 {
        return Err(Error::Invalid("uniform modulus"));
    }
    let r = (u128::MAX % m + 1) % m;
    loop {
        let x = words.word();
        if x <= u128::MAX - r {
            return Ok(x % m);
        }
    }
}

/// A Bernoulli draw with success probability `exp(-num / den)`, for
/// `0 <= num <= den`, `den >= 1`: CKS Algorithm 1, where each inner test is a
/// Bernoulli draw with probability `(num / den) (1 / k)`.
pub fn bern_exp(words: &mut impl Words, num: u128, den: u128) -> Result<bool> {
    if den == 0 || num > den {
        return Err(Error::Invalid("bernoulli parameter"));
    }
    let mut k: u128 = 1;
    loop {
        let x = uniform(words, den)?;
        let y = uniform(words, k)?;
        if !(x < num && y == 0) {
            break;
        }
        k = k.checked_add(1).ok_or(Error::Overflow("bernoulli"))?;
    }
    Ok(k % 2 == 1)
}

/// An exact discrete-Laplace draw of scale `a / b` over the integers
/// (`P[x] proportional to exp(-|x| b / a)`), CKS Algorithm 2. Overflow of
/// `U + A V` is reported, never clamped; its probability is about
/// `exp(-2^128 / A)`.
pub fn dlap(words: &mut impl Words, a: u128, b: u128) -> Result<i128> {
    if a == 0 || b == 0 {
        return Err(Error::Invalid("laplace scale"));
    }
    loop {
        let u = uniform(words, a)?;
        if !bern_exp(words, u, a)? {
            continue;
        }
        let mut v: u128 = 0;
        while bern_exp(words, 1, 1)? {
            v = v.checked_add(1).ok_or(Error::Overflow("laplace"))?;
        }
        let x = a
            .checked_mul(v)
            .and_then(|av| av.checked_add(u))
            .ok_or(Error::Overflow("laplace"))?;
        let y = x / b;
        let s = uniform(words, 2)?;
        if s == 1 && y == 0 {
            continue;
        }
        let y = i128::try_from(y).map_err(|_| Error::Overflow("laplace"))?;
        return Ok(if s == 1 { -y } else { y });
    }
}

// ---------------------------------------------------------------------------
// The draw and the floor.

/// One subject's private draw: the offset in `[0, w)` and the noise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Draw {
    pub offset: u64,
    pub noise: i128,
}

/// Draws a subject's offset (after `w` is known) and noise from its own
/// streams.
pub fn draw(
    seed: &[u8; 32],
    cohort: &CohortBinding,
    subject: &Subject,
    w: u64,
    tau: &Tau,
) -> Result<Draw> {
    if w == 0 {
        return Err(Error::Invalid("grid width"));
    }
    let offset = uniform(&mut Stream::new(offset_key(seed, cohort, subject)), u128::from(w))? as u64;
    let noise = dlap(&mut Stream::new(noise_key(seed, cohort, subject)), tau.a, tau.b)?;
    Ok(Draw { offset, noise })
}

/// `q = w * clamp(floor((S + eta + o) / w), 0, floor(10^18 / w))`.
pub fn quantize(score: u64, draw: &Draw, w: u64) -> Result<u64> {
    if w == 0 || w > SCORE_SCALE {
        return Err(Error::Invalid("grid width"));
    }
    if score > SCORE_SCALE || draw.offset >= w {
        return Err(Error::OutOfRange("quantize input"));
    }
    let x = i128::from(score)
        .checked_add(draw.noise)
        .and_then(|v| v.checked_add(i128::from(draw.offset)))
        .ok_or(Error::Overflow("quantize"))?;
    let top = i128::from(SCORE_SCALE / w);
    let j = x.div_euclid(i128::from(w)).clamp(0, top);
    Ok((j as u64) * w)
}

/// Whether `q` is a grid value under `w`.
pub fn on_grid(q: u64, w: u64) -> bool {
    w > 0 && q <= SCORE_SCALE && q.is_multiple_of(w)
}

/// `w * ceil(t / w)` for an exact threshold `t = num / den` in score units.
/// It may exceed `10^18`, when no grid value reaches `t`.
pub fn ceil_to_grid(num: u128, den: u128, w: u64) -> Result<u128> {
    if den == 0 || w == 0 {
        return Err(Error::Invalid("grid threshold"));
    }
    let w = u128::from(w);
    let cells = num.div_ceil(den.checked_mul(w).ok_or(Error::Overflow("grid threshold"))?);
    cells.checked_mul(w).ok_or(Error::Overflow("grid threshold"))
}

// ---------------------------------------------------------------------------
// The published bit budget.

/// `ceil(log2(n))` for `n >= 1`.
pub fn ceil_log2(n: u128) -> u32 {
    if n <= 1 { 0 } else { 128 - (n - 1).leading_zeros() }
}

/// Limbs for `x!` with `x <= MAX_ROSTER`: each factor adds at most
/// `ceil(log2(MAX_ROSTER)) + 1` bits.
const FACTORIAL_LIMBS: usize =
    (crate::records_v2::MAX_ROSTER as usize * (u16::BITS as usize + 1 - (crate::records_v2::MAX_ROSTER - 1).leading_zeros() as usize)) / 64 + 1;

/// `ceil(log2(x!))`, which bounds the Bell number of any partition of `x`.
pub fn ceil_log2_factorial(x: u16) -> Result<u32> {
    if x > crate::records_v2::MAX_ROSTER {
        return Err(Error::OutOfRange("roster size"));
    }
    let mut limbs = [0u64; FACTORIAL_LIMBS];
    limbs[0] = 1;
    for f in 2..=u64::from(x) {
        let mut carry: u128 = 0;
        for limb in limbs.iter_mut() {
            let v = u128::from(*limb) * u128::from(f) + carry;
            *limb = v as u64;
            carry = v >> 64;
        }
        if carry != 0 {
            return Err(Error::Overflow("factorial"));
        }
    }
    // ceil(log2(N)) = bit length of N - 1, for N >= 1.
    let mut i = 0;
    loop {
        let (v, borrow) = limbs[i].overflowing_sub(1);
        limbs[i] = v;
        if !borrow {
            break;
        }
        i += 1;
    }
    let top = limbs.iter().rposition(|&l| l != 0);
    Ok(top.map_or(0, |t| t as u32 * 64 + (64 - limbs[t].leading_zeros())))
}

/// Every counted term of a cohort's published bits, each
/// `ceil(log2(alphabet))`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Budget {
    /// `X`, executed entrants.
    pub executed: u16,
    /// `K`.
    pub grid_values: u128,
    /// Per executed entrant: `ceil(log2(K + 1))` (`q` or withheld)
    /// + `ceil(log2(min(X, K) + 1))` (band) + 1 (gate) + 1 (copy gate).
    pub per_entrant: u32,
    /// `(1 + [k > 0]) ceil(log2 K)`.
    pub references: u32,
    /// `ceil(log2(X!))`.
    pub clusters: u32,
    /// `ceil(log2(X + 1))`.
    pub promotion: u32,
    /// `ceil(log2(LADDER_VALUES))`.
    pub width: u32,
    pub total: u32,
}

impl Budget {
    pub fn count(executed: u16, w: u64, predecessor: bool) -> Result<Self> {
        let k = grid_values(w)?;
        let x = u128::from(executed);
        let per_entrant = ceil_log2(k + 1) + ceil_log2(x.min(k) + 1) + 1 + 1;
        let references = (1 + u32::from(predecessor)) * ceil_log2(k);
        let clusters = ceil_log2_factorial(executed)?;
        let promotion = ceil_log2(x + 1);
        let width = ceil_log2(LADDER_VALUES);
        let total = u32::from(executed)
            .checked_mul(per_entrant)
            .and_then(|t| t.checked_add(references))
            .and_then(|t| t.checked_add(clusters))
            .and_then(|t| t.checked_add(promotion))
            .and_then(|t| t.checked_add(width))
            .ok_or(Error::Overflow("bit budget"))?;
        Ok(Self {
            executed,
            grid_values: k,
            per_entrant,
            references,
            clusters,
            promotion,
            width,
            total,
        })
    }

    /// `total * F <= n * h`.
    pub fn within(&self, n: u32, identify_bits: u16, budget_factor: u16) -> bool {
        u64::from(self.total) * u64::from(budget_factor)
            <= u64::from(n) * u64::from(identify_bits)
    }
}

/// `m = X + 1 + [k > 0]`, the charged values.
pub fn charged(executed: u16, predecessor: bool) -> u32 {
    u32::from(executed) + 1 + u32::from(predecessor)
}

/// The counted hidden-data bits other rules add to a cohort's published
/// values, beside [`Budget::count`]'s terms: each rule that publishes a
/// deterministic value from hidden data adds its alphabet's bits here, so
/// every caller of [`cohort_bits`] (plan validation's worst case, the
/// `live-v1` derivation of the segment count and the private recount at
/// packaging) counts one complete total. No rule adds a term today.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CountedTerms {
    /// Bits per executed entrant.
    pub per_executed: u32,
    /// Bits per cohort.
    pub per_cohort: u32,
}

impl CountedTerms {
    /// No other rule's terms.
    pub const NONE: Self = Self { per_executed: 0, per_cohort: 0 };

    /// The terms' bits at `executed` entrants.
    pub fn bits(&self, executed: u16) -> Result<u32> {
        u32::from(executed)
            .checked_mul(self.per_executed)
            .and_then(|t| t.checked_add(self.per_cohort))
            .ok_or(Error::Overflow("counted terms"))
    }
}

/// A cohort's complete counted bit total: [`Budget::count`] plus every other
/// rule's [`CountedTerms`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CohortBits {
    pub quant: Budget,
    pub terms: u32,
    pub total: u32,
}

impl CohortBits {
    /// `total * F <= n * h`.
    pub fn within(&self, n: u32, identify_bits: u16, budget_factor: u16) -> bool {
        within_bound(self.total, n, identify_bits, budget_factor)
    }
}

/// `bits * F <= n * h`.
pub fn within_bound(bits: u32, n: u32, identify_bits: u16, budget_factor: u16) -> bool {
    u64::from(bits) * u64::from(budget_factor) <= u64::from(n) * u64::from(identify_bits)
}

/// The cohort's complete counted bits at grid width `w`: the quantization
/// budget and every other rule's terms. The signed [`crate::BudgetRecord`]
/// keeps counting [`Budget::count`] alone.
pub fn cohort_bits(executed: u16, w: u64, predecessor: bool, terms: &CountedTerms) -> Result<CohortBits> {
    let quant = Budget::count(executed, w, predecessor)?;
    let extra = terms.bits(executed)?;
    let total = quant.total.checked_add(extra).ok_or(Error::Overflow("bit budget"))?;
    Ok(CohortBits { quant, terms: extra, total })
}

/// The narrowest grid a cohort of `executed` entrants can have: `SE = 0`, so
/// `w = ladder(w_floor(X, k))`, which maximizes `K` and so the counted bits.
pub fn narrowest_width(n: u32, executed: u16, predecessor: bool, eps_total_milli: u32) -> Result<u64> {
    let m = charged(executed, predecessor);
    let tau = Tau::new(n, m, eps_total_milli)?;
    width(0, &tau)
}

/// The plan-time worst case over every `X` in `0..=roster_cap` and both
/// reference shapes (with and without a predecessor), each at the narrowest
/// possible grid (`SE = 0`, so `w = ladder(w_floor(X, k))`, which maximizes
/// `K`). Without a predecessor `m = X + 1` is smaller, so `w_floor` and `w`
/// can drop a ladder step and `K` roughly doubles: that case can cost more
/// than the reference term it saves, so both are counted. Returns the
/// largest budget and whether every case fits; a plan that passes can never
/// be refused at seal, whatever `SE` and `k` turn out to be, because `K`
/// only shrinks as `w` grows.
pub fn worst_case(
    n: u32,
    roster_cap: u16,
    eps_total_milli: u32,
    identify_bits: u16,
    budget_factor: u16,
) -> Result<(Budget, bool)> {
    let (worst, ok) = worst_case_terms(n, roster_cap, eps_total_milli, identify_bits, budget_factor, &CountedTerms::NONE)?;
    Ok((worst.quant, ok))
}

/// [`worst_case`] over the complete counted total ([`cohort_bits`]) with
/// `terms`: the largest total and whether every case fits.
pub fn worst_case_terms(
    n: u32,
    roster_cap: u16,
    eps_total_milli: u32,
    identify_bits: u16,
    budget_factor: u16,
    terms: &CountedTerms,
) -> Result<(CohortBits, bool)> {
    let mut worst = CohortBits::default();
    let mut ok = true;
    for x in 0..=roster_cap {
        for predecessor in [false, true] {
            let w = narrowest_width(n, x, predecessor, eps_total_milli)?;
            let bits = cohort_bits(x, w, predecessor, terms)?;
            ok &= bits.within(n, identify_bits, budget_factor);
            if bits.total > worst.total {
                worst = bits;
            }
        }
    }
    Ok((worst, ok))
}
