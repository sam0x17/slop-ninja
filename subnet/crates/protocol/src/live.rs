//! `live-v1`: segment readings while a cohort is scored.
//!
//! Right after the seal, the cohort's batch (never a probe or audit
//! document) is split into `L` disjoint segments by the private task order
//! ([`partition`]). When a model has a final outcome for every batch task of
//! segments `1..=s`, `s < L`, it may be released one noisy reading of its
//! mean utility on segment `s`, once:
//!
//! ```text
//! S^s = floor(sum_{t in segment s} u(t) / N_s)
//! v^s = w * clamp(floor((S^s + eta + o) / w), 0, floor(1 / w))
//! ```
//!
//! - `eta` is exact discrete-Laplace noise of scale
//!   `tau = ceil(10^18 / floor(n / L)) (X + 1) / eps_live` ([`tau_live`]):
//!   one reading per checkpoint for each of the `X` executed entrants and
//!   `R_k`, so each document costs `eps_live` per cohort whatever `L` is (the
//!   segments are disjoint and the order is a secret random permutation);
//! - `o` is a secret uniform offset in `[0, w)`;
//! - `w = ladder(ceil(tau))` ([`width_live`]) reads only public values
//!   (`n`, `L`, `X`, `eps_live`), so the grid is no hidden-data bit;
//! - both draws come from streams keyed by the private cohort seed, the
//!   cohort's manifest digest, the subject's inference content, the
//!   checkpoint and a revision ([`live_key`]), under domains that no
//!   `quant-v1` draw uses, so a reading never shares noise with a released
//!   score.
//!
//! Every reading counts in the cohort's bit budget at full face value
//! ([`live_bits`]), and `L` is the largest value up to the plan's maximum for
//! which the cohort's complete counted total ([`crate::quant::cohort_bits`],
//! every rule's terms) plus the readings still fits ([`derive_segments`]).
//! A larger roster gets fewer readings, and possibly none.
//!
//! Readings never enter a cutoff, band, credit, gate, cluster, promotion or
//! row. The last segment is never released alone.
//!
//! Integer arithmetic only, checked, so native and `wasm32` agree exactly.

use crate::quant::{self, CohortBinding, CountedTerms, Draw, Stream, Subject, Tau};
use crate::records_v2::QuantParams;
use crate::{Digest, Error, Result, SCORE_SCALE, domain_hash};

/// The rule's identifier, as published.
pub const LIVE_RULE: &str = "live-v1";
/// Offset key domain.
pub const OFFSET_DOMAIN: &str = "slop_ninja.quant/live-offset/v1";
/// Noise key domain.
pub const NOISE_DOMAIN: &str = "slop_ninja.quant/live-noise/v1";
/// The fewest segments that give a reading (`L - 1` readings).
pub const MIN_SEGMENTS: u8 = 2;
/// The dashboard contract's roster row bound (`roster::ROW_BYTES`): a
/// published roster row, `live` object included, never exceeds it.
pub const ROSTER_ROW_BYTES: usize = 1024;
/// The longest roster row without its `live` object (the dashboard
/// contract's rows stay below it).
pub const ROSTER_ROW_BASE_BYTES: usize = 600;
/// The symbol a withheld entrant's checkpoint shows.
pub const WITHHELD: &str = "withheld";
/// The symbol a stopped reference checkpoint shows.
pub const NONE: &str = "none";

const SCALE: u128 = SCORE_SCALE as u128;

/// A plan's `live` block: the rule's versioned parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveParams {
    /// `eps_live`, in thousandths, per cohort.
    pub eps_live_milli: u32,
    /// The most segments: `0` means no readings.
    pub segments_max: u8,
}

impl LiveParams {
    /// Whether the plan asks for readings at all.
    pub fn readings(&self) -> bool {
        self.segments_max >= MIN_SEGMENTS
    }

    /// The plan-time checks, before anything runs: `segments_max` is `0` or
    /// at least two, `eps_live > 0` with readings, every segment at least a
    /// chunk long, every derived value computable for every roster size up
    /// to `roster_cap`, and the longest `live` roster row within
    /// [`ROSTER_ROW_BYTES`] ([`row_bytes_max`]). Never refuses for the bit
    /// budget: `L` adapts ([`derive_segments`]).
    pub fn validate(&self, n: u32, chunk_docs: u32, roster_cap: u16, quant: &QuantParams) -> Result<()> {
        if self.segments_max == 1 {
            return Err(Error::OutOfRange("live.segments_max"));
        }
        if !self.readings() {
            return Ok(());
        }
        if self.eps_live_milli == 0 {
            return Err(Error::OutOfRange("live.eps_live_milli"));
        }
        if chunk_docs == 0 || n / u32::from(self.segments_max) < chunk_docs {
            return Err(Error::OutOfRange("live.segments_max"));
        }
        if roster_cap > crate::MAX_ROSTER {
            return Err(Error::OutOfRange("roster cap"));
        }
        let top = roster_cap;
        for x in 0..=top {
            for l in MIN_SEGMENTS..=self.segments_max {
                let w = width_live(n, l, x, self.eps_live_milli)?;
                live_bits(x, l, w)?;
                noise_sd_milli(n, l, x, self.eps_live_milli)?;
            }
            for predecessor in [false, true] {
                derive_segments(n, x, predecessor, quant, self, &CountedTerms::NONE)?;
            }
        }
        if row_bytes_max(n, roster_cap, quant.eps_total_milli, self)? > ROSTER_ROW_BYTES {
            return Err(Error::OutOfRange("live row bytes"));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The partition.

/// Segment `s`'s range `[start, end)` among the `n` batch tasks in partition
/// order, `s` in `0..l`: `l` contiguous segments of `floor(n / l)` or
/// `ceil(n / l)` tasks, the smaller ones first.
pub fn segment_bounds(n: u32, l: u8, s: u8) -> Result<(u32, u32)> {
    if l == 0 || s >= l || n < u32::from(l) {
        return Err(Error::Invalid("segment"));
    }
    let (l, s) = (u32::from(l), u32::from(s));
    let (q, r) = (n / l, n % l);
    let small = l - r;
    let start = |s: u32| s * q + s.saturating_sub(small);
    Ok((start(s), start(s + 1)))
}

/// The lane whose canonical affinity range `[floor(k C / W), floor((k + 1) C / W))`
/// holds chunk `c`, with that range.
fn lane_of(c: u32, lanes: u32, chunks: u32) -> (u64, u64) {
    let (w, cc) = (u64::from(lanes), u64::from(chunks));
    let range = |k: u64| (k * cc / w, (k + 1) * cc / w);
    // The largest k with floor(k C / W) <= c; empty ranges are skipped.
    let mut k = (u64::from(c) * w / cc).min(w - 1);
    while k + 1 < w && range(k + 1).0 <= u64::from(c) {
        k += 1;
    }
    while k > 0 && range(k).0 > u64::from(c) {
        k -= 1;
    }
    range(k)
}

/// Each batch task's segment, `1..=l`, in the order given.
///
/// `positions` are the batch tasks' positions in an entrant job's task list
/// (every sealed task in task order), distinct. A task's chunk is
/// `c = floor(pos / chunk_docs)`; with the job's planned lane count `W` and
/// chunk count `C`, its lane `k` is the one whose canonical range
/// `[lo, hi)` holds `c`, and its relative position is the exact rational
/// `r = (c - lo) / (hi - lo)`. The tasks are sorted by `(r, pos)` and cut
/// into [`segment_bounds`]. At `W = 1`, `r` increases with `pos`, so segment
/// `s` is the `s`-th run of batch tasks in task order; with more lanes every
/// lane contributes to every segment, so segment `s` completes near
/// fraction `s / L` of the job in every lane.
///
/// The partition reads only what the seal fixed (task positions, the chunk
/// size and the lane plan), never commit order, steals, retries or timing,
/// so no artifact can steer a document between segments.
pub fn partition(positions: &[u32], chunk_docs: u32, lanes: u32, chunks: u32, l: u8) -> Result<Vec<u8>> {
    if chunk_docs == 0 || lanes == 0 || chunks == 0 {
        return Err(Error::Invalid("partition shape"));
    }
    let n = u32::try_from(positions.len()).map_err(|_| Error::Overflow("partition"))?;
    segment_bounds(n, l, 0)?;
    let mut keyed = Vec::with_capacity(positions.len());
    for (at, &pos) in positions.iter().enumerate() {
        let c = pos / chunk_docs;
        if c >= chunks {
            return Err(Error::Invalid("task beyond the job's chunks"));
        }
        let (lo, hi) = lane_of(c, lanes, chunks);
        if !(lo..hi).contains(&u64::from(c)) {
            return Err(Error::Invalid("lane range"));
        }
        keyed.push((u64::from(c) - lo, hi - lo, pos, at));
    }
    // (num, den) compared exactly by cross-multiplication, then position.
    keyed.sort_by(|a, b| (u128::from(a.0) * u128::from(b.1)).cmp(&(u128::from(b.0) * u128::from(a.1))).then(a.2.cmp(&b.2)));
    if keyed.windows(2).any(|w| w[0].2 == w[1].2) {
        return Err(Error::Invalid("repeated task position"));
    }
    let mut out = vec![0u8; positions.len()];
    for s in 0..l {
        let (a, b) = segment_bounds(n, l, s)?;
        for k in &keyed[a as usize..b as usize] {
            out[k.3] = s + 1;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Noise scale, grid and bits.

/// `tau = ceil(10^18 / floor(n / L)) (X + 1) / eps_live`.
pub fn tau_live(n: u32, l: u8, executed: u16, eps_live_milli: u32) -> Result<Tau> {
    if l < MIN_SEGMENTS {
        return Err(Error::Invalid("segments"));
    }
    let seg = n / u32::from(l);
    Tau::new(seg, u32::from(executed) + 1, eps_live_milli)
}

/// `w = ladder(ceil(tau))`: public values only.
pub fn width_live(n: u32, l: u8, executed: u16, eps_live_milli: u32) -> Result<u64> {
    quant::ladder(tau_live(n, l, executed, eps_live_milli)?.floor_width())
}

/// The counted bits of every reading: `(L - 1) (X + 1) ceil(log2(K + 1))`,
/// one symbol (a grid value or withheld) per subject per checkpoint. Where
/// an entrant stopped is a function of these symbols, so it adds no term.
pub fn live_bits(executed: u16, l: u8, w: u64) -> Result<u32> {
    if l < MIN_SEGMENTS {
        return Ok(0);
    }
    let k = quant::grid_values(w)?;
    (u32::from(l) - 1)
        .checked_mul(u32::from(executed) + 1)
        .and_then(|v| v.checked_mul(quant::ceil_log2(k + 1)))
        .ok_or(Error::Overflow("live bits"))
}

/// The segment count derived right after the seal, with the counted terms it
/// rests on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Derivation {
    /// `L`: `0` (no readings) or `2..=segments_max`.
    pub segments: u8,
    /// The cohort's complete counted bits at the seal ([`quant::cohort_bits`]
    /// at the narrowest grid, `SE = 0`).
    pub counted: u32,
    /// [`live_bits`] at `L` (`0` without readings).
    pub live: u32,
    /// `floor(n h / F)`.
    pub bound: u64,
}

/// `L` = the largest `L'` in `2..=segments_max` with
/// `(counted + live_bits(L')) F <= n h`, else `0`. Reads only public values:
/// `n`, the sealed `X` and reference shape, the manifest's quantization
/// parameters, the live block and every other rule's counted terms.
pub fn derive_segments(
    n: u32,
    executed: u16,
    predecessor: bool,
    quant: &QuantParams,
    live: &LiveParams,
    terms: &CountedTerms,
) -> Result<Derivation> {
    let w = quant::narrowest_width(n, executed, predecessor, quant.eps_total_milli)?;
    let counted = quant::cohort_bits(executed, w, predecessor, terms)?.total;
    let bound = u64::from(n) * u64::from(quant.identify_bits) / u64::from(quant.budget_factor.max(1));
    let mut out = Derivation { segments: 0, counted, live: 0, bound };
    if !live.readings() || live.eps_live_milli == 0 {
        return Ok(out);
    }
    for l in (MIN_SEGMENTS..=live.segments_max).rev() {
        if n / u32::from(l) == 0 {
            continue;
        }
        let bits = live_bits(executed, l, width_live(n, l, executed, live.eps_live_milli)?)?;
        let total = counted.checked_add(bits).ok_or(Error::Overflow("live bits"))?;
        if quant::within_bound(total, n, quant.identify_bits, quant.budget_factor) {
            out.segments = l;
            out.live = bits;
            return Ok(out);
        }
    }
    Ok(out)
}

/// The L(X) table a plan echoes: `L` for every `X` from none to the roster
/// cap, as `(without a predecessor, with one)`.
pub fn segments_table(
    n: u32,
    roster_cap: u16,
    quant: &QuantParams,
    live: &LiveParams,
    terms: &CountedTerms,
) -> Result<Vec<(u8, u8)>> {
    let top = roster_cap;
    (0..=top)
        .map(|x| {
            Ok((
                derive_segments(n, x, false, quant, live, terms)?.segments,
                derive_segments(n, x, true, quant, live, terms)?.segments,
            ))
        })
        .collect()
}

/// `floor(sum u / N)` over one segment's utilities (`u <= 10^18`, a model
/// error scoring `0`).
pub fn segment_score(utilities: impl IntoIterator<Item = u64>) -> Result<u64> {
    let mut sum: u128 = 0;
    let mut count: u128 = 0;
    for u in utilities {
        if u128::from(u) > SCALE {
            return Err(Error::OutOfRange("utility"));
        }
        sum = sum.checked_add(u128::from(u)).ok_or(Error::Overflow("segment sum"))?;
        count += 1;
    }
    if count == 0 {
        return Err(Error::Invalid("empty segment"));
    }
    Ok((sum / count) as u64)
}

// ---------------------------------------------------------------------------
// Keys and draws.

/// `H(domain, seed || manifest || subject(33) || s || revision)`.
pub fn live_key(domain: &str, seed: &[u8; 32], cohort: &CohortBinding, subject: &Subject, s: u8, revision: u8) -> Digest {
    let mut payload = [0u8; 64 + quant::SUBJECT_LEN + 2];
    payload[..32].copy_from_slice(seed);
    payload[32..64].copy_from_slice(&cohort.0.0);
    payload[64..64 + quant::SUBJECT_LEN].copy_from_slice(&subject.encode());
    payload[64 + quant::SUBJECT_LEN] = s;
    payload[64 + quant::SUBJECT_LEN + 1] = revision;
    domain_hash(domain, &payload)
}

/// Checkpoint `s`'s offset in `[0, w)` and noise, from their own streams.
pub fn draw_live(
    seed: &[u8; 32],
    cohort: &CohortBinding,
    subject: &Subject,
    s: u8,
    revision: u8,
    w: u64,
    tau: &Tau,
) -> Result<Draw> {
    if w == 0 {
        return Err(Error::Invalid("grid width"));
    }
    let offset =
        quant::uniform(&mut Stream::new(live_key(OFFSET_DOMAIN, seed, cohort, subject, s, revision)), u128::from(w))? as u64;
    let noise = quant::dlap(&mut Stream::new(live_key(NOISE_DOMAIN, seed, cohort, subject, s, revision)), tau.a, tau.b)?;
    Ok(Draw { offset, noise })
}

/// The released reading: the floor after offset on the live grid.
pub fn reading(score: u64, draw: &Draw, w: u64) -> Result<u64> {
    quant::quantize(score, draw, w)
}

/// One subject's reading on checkpoint `s` at `L` segments: the whole rule.
#[allow(clippy::too_many_arguments)]
pub fn release(
    seed: &[u8; 32],
    cohort: &CohortBinding,
    subject: &Subject,
    s: u8,
    revision: u8,
    n: u32,
    l: u8,
    executed: u16,
    eps_live_milli: u32,
    score: u64,
) -> Result<u64> {
    if s == 0 || s >= l {
        return Err(Error::Invalid("checkpoint"));
    }
    let tau = tau_live(n, l, executed, eps_live_milli)?;
    let w = quant::ladder(tau.floor_width())?;
    reading(score, &draw_live(seed, cohort, subject, s, revision, w, &tau)?, w)
}

// ---------------------------------------------------------------------------
// Display values.

/// One reading's standard deviation, `sqrt(2 tau^2 + w^2 / 6)` (discrete
/// Laplace plus the floor after a uniform offset), rounded up to
/// thousandths: the smallest `d` with `(d / 1000)^2 >= var`. The eval and
/// the publisher both call this, so they print the same `noise_sd`.
pub fn noise_sd_milli(n: u32, l: u8, executed: u16, eps_live_milli: u32) -> Result<u64> {
    let tau = tau_live(n, l, executed, eps_live_milli)?;
    let w = u128::from(quant::ladder(tau.floor_width())?);
    let overflow = || Error::Overflow("noise sd");
    // var = (12 a^2 + w^2 b^2) / (6 b^2) in 1e36 units; d^2 >= var / 1e30.
    let a2 = quant::U256::mul_u128(tau.a, tau.a).checked_mul_u128(12).ok_or_else(overflow)?;
    let b2 = tau.b.checked_mul(tau.b).ok_or_else(overflow)?;
    let wb = quant::U256::mul_u128(w, w).checked_mul_u128(b2).ok_or_else(overflow)?;
    let num = a2.checked_add(wb).ok_or_else(overflow)?;
    let den = b2.checked_mul(6).ok_or_else(overflow)?;
    let q = num.div_ceil_u128(den).ok_or_else(overflow)?;
    let d = quant::ceil_isqrt(q.div_ceil(1_000_000_000_000_000_000_000_000_000_000));
    u64::try_from(d).map_err(|_| overflow())
}

/// Thousandths printed with three decimals: `54` -> `"0.054"`.
pub fn format_milli(d: u64) -> String {
    format!("{}.{:03}", d / 1000, d % 1000)
}

/// The longest grid decimal any `w` in `widths` prints.
fn longest_decimal(widths: impl IntoIterator<Item = u64>) -> Result<usize> {
    let mut longest = 0;
    for w in widths {
        let d = quant::decimals(w)? as usize;
        longest = longest.max(if d == 0 { 1 } else { 2 + d });
    }
    Ok(longest)
}

/// The longest roster row a plan can produce at the `live` level:
/// [`ROSTER_ROW_BASE_BYTES`] plus the longest `live` row object, with
/// `segments_max - 1` checkpoints at the longest decimal any live grid of the
/// plan prints (every `X` up to `roster_cap`, every `L` up to
/// `segments_max`) or `"withheld"`, whichever is longer, and the longest
/// pre-finalized value (the narrowest `quant-v1` grid, `X = 0` without a
/// predecessor, or `"withheld"`). A bound from the schema, not a tuned cap.
pub fn row_bytes_max(n: u32, roster_cap: u16, eps_total_milli: u32, live: &LiveParams) -> Result<usize> {
    let mut widths = Vec::new();
    if live.readings() {
        let top = roster_cap;
        for x in 0..=top {
            for l in MIN_SEGMENTS..=live.segments_max {
                widths.push(width_live(n, l, x, live.eps_live_milli)?);
            }
        }
    }
    let cp = longest_decimal(widths)?.max(WITHHELD.len());
    let pf = longest_decimal([quant::narrowest_width(n, 0, false, eps_total_milli)?])?.max(WITHHELD.len());
    let checkpoints = usize::from(live.segments_max.saturating_sub(1));
    Ok(ROSTER_ROW_BASE_BYTES + live_row_len(checkpoints, cp, pf))
}

/// The bytes of `,"live":{"checkpoints":[..],"stopped":false,"prefinal":".."}`
/// with `checkpoints` values of `cp` characters and a prefinal value of `pf`.
pub fn live_row_len(checkpoints: usize, cp: usize, pf: usize) -> usize {
    let values = checkpoints * (cp + 2) + checkpoints.saturating_sub(1);
    r#","live":{"checkpoints":[],"stopped":false,"prefinal":""}"#.len() + values + pf
}

