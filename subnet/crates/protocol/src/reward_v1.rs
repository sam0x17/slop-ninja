//! `reward-v1`: the frozen band mechanism of whitepaper 0.56, Section 7.
//!
//! It ports the band mechanism of Bittensor subnet 78 (Umi) with the paper's
//! unit translation. Scores are fixed-point utilities scaled by
//! [`SCORE_SCALE`]; every comparison, band and credit is exact integer or
//! rational arithmetic, so every target derives byte-identical rows.
//!
//! 1. **Scores.** `u = 1 - (a - y)^2` per document; a failure scores 0 and keeps
//!    its weight. The batch is self-weighting, so `S = floor(sum u / n)`. A
//!    reference with any failure holds the package ([`reference_score`]).
//! 2. **Gates.** An entrant is gated when no enforced gate failed and it was
//!    scored. Gate failures earn nothing. The public-copy gate
//!    ([`check_public`]) fails an entrant whose inference content equals that
//!    of any published artifact (`public.content`, found at admission, never
//!    executed), and every scored entrant, gate-failed ones included, that is
//!    a near copy ([`near`]) of `R_k` or `R_k-1` on the batch or of a
//!    fingerprinted published artifact on the private probe
//!    (`public.near.current`, `.predecessor`, `.probe`, in that precedence).
//!    The probe comparison always includes the fingerprints of `R_k` and
//!    `R_k-1`, whose statistics every scored entrant publishes, so a copy of
//!    a reference must escape both the batch and the probe. Every scored
//!    entrant also runs on the probe, and its probe predictions become its
//!    fingerprint when its cohort closes ([`PublishedIndex::after_closure`]);
//!    entrants with identical content record the earliest-committed one's.
//!    Model `x` is a near copy of model `y` when their monotone-aligned
//!    distance is at most `d = 0.025` ([`aligned_rss`], [`NearStats::within`])
//!    and `x` does not score more than `w / 2` above `y` ([`outscores`]).
//! 3. **Rank order.** Score descending, then finalized commitment block,
//!    extrinsic index and hotkey bytes ascending. It breaks every tie below.
//! 4. **Cutoff.** `delta = max(0.01, 2 sigma)`. If a gated entrant has
//!    `q = (S - S_Rk) / (1 - S_Rk) >= delta`, the cutoff is
//!    `T = S_Rk + delta (1 - S_Rk)` ([`CutoffMode::Margin`]). The first
//!    retained cluster head in rank order that passes the strict tier of the
//!    pure-human false-positive gate and is not blocked by two lookup-canary
//!    flags becomes `R_k+1` ([`PromotionAudit`]); when none passes nobody is
//!    promoted and credits are unchanged. Otherwise, when `R_k-1` exists and a gated
//!    entrant scores strictly above `S_Rk-1`, the cutoff is
//!    `C_N = S_Rk-1 + 0.15 (max_N - S_Rk-1)` ([`CutoffMode::Fallback`]) and the
//!    reference stays. Otherwise, or when `S_Rk = 1`, the cohort produces no
//!    target ([`CutoffMode::NoTarget`]).
//! 5. **Clusters.** Walking gated entrants at or above the cutoff in rank
//!    order, the first unassigned entrant heads a cluster with every later
//!    unassigned entrant of identical inference content or within the
//!    near-copy distance of the head on the batch; no chaining. In rank order
//!    a member never outscores its head, so the distance alone decides.
//!    Assignments and the statistics behind each distance join are coordinator
//!    measurements published in the package; validators check the structure,
//!    the identical-content rule and each published statistic against `d`
//!    ([`cluster_heads`]).
//! 6. **Near-equal drop.** Walking heads in rank order, a cluster whose score is
//!    within `epsilon = w / 250` of the last retained head's score is dropped.
//! 7. **Bands.** `w = 2 sigma (1 - S_Rk)`; band `b = min(floor(S / w),
//!    ceil(1 / w) - 1)` on the absolute score grid. Occupied bands rank
//!    `kappa = 0..B` from the top, clusters `mu = 0..n_kappa` in rank order.
//! 8. **Credit.** `c = r^kappa / sum_{j<B} r^j * r^mu / sum_{j<n_kappa} r^j`,
//!    `r = 0.85`; each of a cluster's `m` members receives `c / m`. The
//!    package publishes every retained cluster's exact `c`.
//! 9. **Row.** 65,535 units by largest remainder over members, ties in rank
//!    order. At each tempo a member's units go to the current UID of its
//!    hotkey and registration generation, or else to the UID that the owner
//!    hotkey of the validator's finalized snapshot holds, which burns them.
//!    The launch burn target sends every unit there. The row records that
//!    owner hotkey, so a rotation moves the burn destination at the next
//!    tempo without a new target. When units must burn and the finalized
//!    owner hotkey holds no UID, that row is deferred ([`Error::Deferred`]).

use std::collections::BTreeMap;

use crate::bigint::{BigUint, Fraction};
use crate::records_v2::{
    CalculationTargetV2, ClusterCredit, CutoffMode, DistanceMonitor, EntrantScore, Gate, NearStats,
    ProbeMatch, PromotionAudit, PublicCheck, PublicCopy, PublishedIndex, RegistrationSnapshotV2,
    RewardV1Params, ScorePackageV2, TargetSource, WeightRowV2,
};
use crate::reward::brier_utility;
use crate::{
    AccountId, Digest, Error, Probability, Record, ReferenceModel, Result, RowEntry, SCORE_SCALE,
    Score,
};

const SCALE: u128 = SCORE_SCALE as u128;
const BPS: u128 = 10_000;

/// One resolved document: the final involvement prediction (`None` for an
/// attributable failure) and the private label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Graded {
    pub involvement: Option<Probability>,
    pub label: bool,
}

/// `S = floor(sum u / n)` over every assigned document, failures at zero.
pub fn document_score(items: &[Graded]) -> Result<Score> {
    if items.is_empty() {
        return Err(Error::Invalid("empty batch"));
    }
    let mut sum = 0u128;
    for item in items {
        let u = item.involvement.map_or(0, |a| brier_utility(a, item.label));
        sum = sum
            .checked_add(u128::from(u))
            .ok_or(Error::Overflow("score sum"))?;
    }
    Score::new((sum / items.len() as u128) as u64)
}

/// A reference model's score. Any missing prediction holds the package for a
/// signed correction rather than scoring against a damaged baseline.
pub fn reference_score(items: &[Graded]) -> Result<Score> {
    if items.iter().any(|i| i.involvement.is_none()) {
        return Err(Error::Invalid("reference failure holds the package"));
    }
    document_score(items)
}

/// `RSS(x | y)`: the residual sum of squares of `x` after the best
/// non-decreasing function of `y`, in parts per billion squared.
///
/// Documents are grouped by equal `y` and taken in ascending `y`; each group
/// enters as `(S, c)`, the sum of its `x` and its count. Pool adjacent
/// violators with a stack: while the two top blocks have `S1 c2 > S2 c1`, merge
/// them. The comparison is strict, so blocks of equal mean are not merged and
/// the floors below are deterministic. Then
/// `RSS = sum x^2 - sum_blocks floor(S^2 / c)`, which is zero for any
/// non-decreasing map of `y`. Every intermediate fits in `u128`.
pub fn aligned_rss(x: &[Probability], y: &[Probability]) -> Result<u128> {
    if x.len() != y.len() {
        return Err(Error::Mismatch("prediction vectors"));
    }
    let mut order: Vec<usize> = (0..x.len()).collect();
    // Groups merge equal keys, so the order within a group is irrelevant.
    order.sort_unstable_by_key(|&i| y[i]);
    let mut blocks: Vec<(u128, u128)> = Vec::new();
    let mut squares = 0u128;
    let mut k = 0;
    while k < order.len() {
        let level = y[order[k]];
        let (mut sum, mut count) = (0u128, 0u128);
        while k < order.len() && y[order[k]] == level {
            let v = u128::from(x[order[k]].ppb());
            sum += v;
            squares = squares
                .checked_add(v * v)
                .ok_or(Error::Overflow("aligned squares"))?;
            count += 1;
            k += 1;
        }
        blocks.push((sum, count));
        while let [.., (s1, c1), (s2, c2)] = blocks[..] {
            let lower = s1
                .checked_mul(c2)
                .ok_or(Error::Overflow("aligned pooling"))?;
            let upper = s2
                .checked_mul(c1)
                .ok_or(Error::Overflow("aligned pooling"))?;
            if lower <= upper {
                break;
            }
            blocks.pop();
            *blocks.last_mut().expect("two blocks") = (s1 + s2, c1 + c2);
        }
    }
    let mut fitted = 0u128;
    for (sum, count) in blocks {
        let square = sum.checked_mul(sum).ok_or(Error::Overflow("aligned fit"))?;
        fitted += square / count;
    }
    squares
        .checked_sub(fitted)
        .ok_or(Error::Invalid("aligned residual"))
}

/// Near-copy statistics of `x` against `y` over the documents on which both
/// returned a prediction: a document where either failed is left out, so
/// failing on purpose cannot move two models apart.
pub fn near_stats(x: &[Option<Probability>], y: &[Option<Probability>]) -> Result<NearStats> {
    if x.len() != y.len() {
        return Err(Error::Mismatch("prediction vectors"));
    }
    let (xs, ys): (Vec<Probability>, Vec<Probability>) =
        x.iter().zip(y).filter_map(|(a, b)| a.zip(*b)).unzip();
    Ok(NearStats {
        common: u32::try_from(xs.len()).map_err(|_| Error::Overflow("common documents"))?,
        rss_fwd: aligned_rss(&xs, &ys)?,
        rss_rev: aligned_rss(&ys, &xs)?,
    })
}

/// `w * SCORE_SCALE^2 = band_sigmas * sigma * (SCORE_SCALE - S_Rk)`; zero when
/// `S_Rk = 1`.
pub fn band_width(params: &RewardV1Params, reference: Score) -> u128 {
    let headroom = SCALE - u128::from(reference.get());
    u128::from(params.band_sigmas) * u128::from(params.sigma) * headroom
}

/// The score clause's exception: `S_x - S_y > w / improvement_divisor`,
/// exactly, with `width = w * SCORE_SCALE^2` ([`band_width`]). While
/// `2 sigma >= 0.01`, `delta (1 - S_Rk) = w`, so this is half the promotion
/// margin; below that `delta` is the 0.01 floor and `w / 2` is less than half
/// the margin.
pub fn outscores(x: Score, y: Score, params: &RewardV1Params, width: u128) -> Result<bool> {
    if x <= y {
        return Ok(false);
    }
    let lead = u128::from(x.get() - y.get())
        .checked_mul(SCALE)
        .and_then(|g| g.checked_mul(u128::from(params.improvement_divisor)))
        .ok_or(Error::Overflow("score clause"))?;
    Ok(lead > width)
}

/// The near-copy relation: `x` (scoring `x_score`) is a near copy of `y`
/// (scoring `y_score`) when their aligned distance is at most `d` and `x`
/// does not score more than `w / 2` above `y`.
pub fn near(
    stats: &NearStats,
    x_score: Score,
    y_score: Score,
    params: &RewardV1Params,
    width: u128,
) -> Result<bool> {
    Ok(stats.within(params.near_copy_ppb)? && !outscores(x_score, y_score, params, width)?)
}

/// The near-copy relation for one probe comparison, with both probe scores.
fn near_on_probe(found: &ProbeMatch, params: &RewardV1Params, width: u128) -> Result<bool> {
    near(
        &found.stats,
        found.entrant_probe_score,
        found.public_probe_score,
        params,
        width,
    )
}

/// Whether probe comparison `a` is a better match than `b`: strictly closer,
/// or equally close with smaller content digest bytes and then version.
fn better_match(a: &ProbeMatch, b: &ProbeMatch) -> bool {
    a.stats.closer_than(&b.stats)
        || (!b.stats.closer_than(&a.stats) && (a.content, a.version) < (b.content, b.version))
}

/// The batch part of the public-copy outcome: a near copy of `R_k`, else of
/// `R_k-1`.
fn batch_copy(
    package: &ScorePackageV2,
    score: Score,
    public: &PublicCheck,
    width: u128,
) -> Result<Option<PublicCopy>> {
    let params = &package.params;
    if near(
        &public.current,
        score,
        package.reference.score,
        params,
        width,
    )? {
        return Ok(Some(PublicCopy::Current));
    }
    if let (Some(stats), Some(reference)) = (&public.predecessor, &package.predecessor)
        && near(stats, score, reference.score, params, width)?
    {
        return Ok(Some(PublicCopy::Predecessor));
    }
    Ok(None)
}

/// The public-copy outcome that an entrant's published statistics imply: a
/// near copy of `R_k`, else of `R_k-1`, on the batch; else a probe match.
/// The probe comparison includes both references' fingerprints, whose
/// published comparisons bound it: when any of them is a near copy the
/// entrant must carry a probe match, at least as close as each of them, and
/// the match must itself satisfy the relation.
fn implied_public_copy(
    package: &ScorePackageV2,
    score: Score,
    public: &PublicCheck,
    width: u128,
) -> Result<Option<PublicCopy>> {
    if let Some(copy) = batch_copy(package, score, public, width)? {
        return Ok(Some(copy));
    }
    let params = &package.params;
    let mut near_references = Vec::new();
    for found in &public.reference_probes {
        if near_on_probe(found, params, width)? {
            near_references.push(found);
        }
    }
    match &public.probe_match {
        None if near_references.is_empty() => Ok(None),
        None => Err(Error::Mismatch("reference probe match omitted")),
        Some(found) => {
            if !near_on_probe(found, params, width)? {
                return Err(Error::Mismatch("probe match not a near copy"));
            }
            if near_references.iter().any(|r| better_match(r, found)) {
                return Err(Error::Mismatch("probe match not the closest"));
            }
            Ok(Some(PublicCopy::Probe))
        }
    }
}

/// The validators' check of the public-copy gate against the published
/// index they rebuilt from retained packages:
///
/// 1. the index is canonical, matches the package's (and so the manifest's)
///    binding, and holds both references, each fingerprinted on every probe
///    version in use, so neither escapes the probe once it is no longer a
///    reference;
/// 2. an entrant fails `public.content` exactly when its content is in the
///    published set, and such an entrant was never executed; this rule is
///    fully verified;
/// 3. every scored entrant carries statistics, whatever its score (checked
///    with the package's structure);
/// 4. each recorded near-copy failure follows from the published statistics
///    and scores with the frozen `d` and `w / 2` clause, in the precedence
///    `R_k`, `R_k-1`, probe, and a probe match names a fingerprint the index
///    holds on a probe version in use;
/// 5. the probe comparison includes both references: every scored entrant
///    publishes its comparison with each reference fingerprint on each probe
///    version in use (checked with the package's structure), and an entrant
///    near neither reference on the batch fails `public.near.probe` whenever
///    one of those comparisons is a near copy, with a match no farther than
///    it;
/// 6. an entrant with statistics and no public-copy failure is near neither
///    reference on the batch or the probe and has no probe match.
///
/// The statistics themselves, and the absence of a match among the other
/// archived fingerprints, are coordinator measurements on hidden
/// predictions, trusted like the scores.
pub fn check_public(package: &ScorePackageV2, index: &PublishedIndex) -> Result<()> {
    index.validate()?;
    if index.binding()? != package.published {
        return Err(Error::Mismatch("published index"));
    }
    for reference in core::iter::once(&package.reference).chain(&package.predecessor) {
        let content = &reference.model.content;
        if !index.contains(content) {
            return Err(Error::Mismatch("reference not published"));
        }
        if package
            .probes
            .iter()
            .any(|p| !index.fingerprinted(content, p.version))
        {
            return Err(Error::Mismatch("reference not fingerprinted"));
        }
    }
    let width = band_width(&package.params, package.reference.score);
    for entrant in &package.entrants {
        let recorded = entrant.public_copy();
        if index.contains(&entrant.content) != (recorded == Some(PublicCopy::Content)) {
            return Err(Error::Mismatch("public content gate"));
        }
        let implied = match (&entrant.public, entrant.score) {
            (Some(public), Some(score)) => {
                if let Some(found) = &public.probe_match
                    && !index.fingerprinted(&found.content, found.version)
                {
                    return Err(Error::Mismatch("probe match not fingerprinted"));
                }
                implied_public_copy(package, score, public, width)?
            }
            _ => None,
        };
        if recorded != Some(PublicCopy::Content) && recorded != implied {
            return Err(Error::Mismatch("public near-copy gate"));
        }
    }
    Ok(())
}

/// Of an entrant's probe comparisons, the near copy with the smallest aligned
/// distance, ties broken by content digest bytes and then version; `None` when
/// none is near.
pub fn closest_probe_match(
    candidates: impl IntoIterator<Item = ProbeMatch>,
    params: &RewardV1Params,
    width: u128,
) -> Result<Option<ProbeMatch>> {
    let mut best: Option<ProbeMatch> = None;
    for candidate in candidates {
        if !near_on_probe(&candidate, params, width)? {
            continue;
        }
        if best.as_ref().is_none_or(|b| better_match(&candidate, b)) {
            best = Some(candidate);
        }
    }
    Ok(best)
}

/// An exact score threshold `num / den`, in units of `1 / SCORE_SCALE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Threshold {
    pub num: u128,
    pub den: u128,
}

impl Threshold {
    /// `T = S_Rk + delta (1 - S_Rk)`.
    pub fn margin(reference: Score, delta: u128) -> Result<Self> {
        let headroom = SCALE - u128::from(reference.get());
        let num = u128::from(reference.get())
            .checked_mul(SCALE)
            .and_then(|a| delta.checked_mul(headroom).and_then(|b| a.checked_add(b)))
            .ok_or(Error::Overflow("margin cutoff"))?;
        Ok(Self { num, den: SCALE })
    }

    /// `C_N = S_Rk-1 + f (max_N - S_Rk-1)` with `f` in basis points.
    pub fn fallback(predecessor: Score, max: Score, fraction_bps: u16) -> Result<Self> {
        let range = u128::from(max.get())
            .checked_sub(u128::from(predecessor.get()))
            .ok_or(Error::Invalid("fallback range"))?;
        let num = u128::from(predecessor.get()) * BPS + u128::from(fraction_bps) * range;
        Ok(Self { num, den: BPS })
    }

    /// `score >= num / den`.
    pub fn admits(&self, score: Score) -> bool {
        u128::from(score.get()) * self.den >= self.num
    }

    /// The threshold in `1 / SCORE_SCALE` units, as `whole` or `whole+rem/den`.
    pub fn display(&self) -> String {
        let (whole, rem) = (self.num / self.den, self.num % self.den);
        if rem == 0 {
            whole.to_string()
        } else {
            let g = gcd_u128(rem, self.den);
            format!("{whole}+{}/{}", rem / g, self.den / g)
        }
    }
}

fn gcd_u128(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The band width `w = 2 sigma (1 - S_Rk)` as `num / SCORE_SCALE` in units of
/// `1 / SCORE_SCALE`, with the band grid and near-equal tolerance it fixes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bands {
    /// `w * SCORE_SCALE^2`: `band_sigmas * sigma * (SCORE_SCALE - S_Rk)`.
    pub width: u128,
    pub near_equal_divisor: u16,
}

impl Bands {
    /// `None` when `S_Rk = 1`, which leaves no width.
    pub fn new(params: &RewardV1Params, reference: Score) -> Option<Self> {
        let width = band_width(params, reference);
        (width > 0).then_some(Self {
            width,
            near_equal_divisor: params.near_equal_divisor,
        })
    }

    /// `min(floor(S / w), ceil(1 / w) - 1)`.
    pub fn band(&self, score: Score) -> u128 {
        let index = u128::from(score.get()) * SCALE / self.width;
        let count = (SCALE * SCALE).div_ceil(self.width);
        index.min(count - 1)
    }

    /// `upper - lower <= epsilon = w / divisor`, for `upper >= lower`.
    pub fn near_equal(&self, upper: Score, lower: Score) -> Result<bool> {
        let gap = u128::from(upper.get() - lower.get());
        let scaled = gap
            .checked_mul(u128::from(self.near_equal_divisor))
            .and_then(|g| g.checked_mul(SCALE))
            .ok_or(Error::Overflow("near-equal"))?;
        Ok(scaled <= self.width)
    }

    /// `w` as an exact fraction of score.
    pub fn width_fraction(&self) -> Fraction {
        Fraction::new(
            BigUint::from_u128(self.width),
            BigUint::from_u128(SCALE * SCALE),
        )
        .expect("nonzero")
    }
}

/// Rank order key: score descending, then commitment block, extrinsic index
/// and hotkey ascending.
fn rank_key(e: &EntrantScore) -> (core::cmp::Reverse<Score>, u64, u32, AccountId) {
    (
        core::cmp::Reverse(e.score.unwrap_or(Score::ZERO)),
        e.commitment_block,
        e.extrinsic_index,
        e.identity.hotkey,
    )
}

/// Gated entrants, as package indices in rank order.
pub fn rank_order(package: &ScorePackageV2) -> Vec<usize> {
    let mut ranked: Vec<usize> = (0..package.entrants.len())
        .filter(|&i| package.entrants[i].gated())
        .collect();
    ranked.sort_by_key(|&i| rank_key(&package.entrants[i]));
    ranked
}

fn score_of(package: &ScorePackageV2, index: usize) -> Score {
    package.entrants[index]
        .score
        .expect("gated entrants are scored")
}

/// The cutoff and the gated entrants at or above it, in rank order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cutoff {
    pub mode: CutoffMode,
    pub threshold: Option<Threshold>,
    pub eligible: Vec<usize>,
}

/// Step 1 of the paper's calculation.
pub fn cutoff(package: &ScorePackageV2) -> Result<Cutoff> {
    let none = Cutoff {
        mode: CutoffMode::NoTarget,
        threshold: None,
        eligible: Vec::new(),
    };
    let reference = package.reference.score;
    if reference == Score::ONE {
        return Ok(none);
    }
    let ranked = rank_order(package);
    let margin = Threshold::margin(reference, package.params.margin())?;
    let passing: Vec<usize> = ranked
        .iter()
        .copied()
        .filter(|&i| margin.admits(score_of(package, i)))
        .collect();
    if !passing.is_empty() {
        return Ok(Cutoff {
            mode: CutoffMode::Margin,
            threshold: Some(margin),
            eligible: passing,
        });
    }
    let (Some(predecessor), Some(&top)) = (&package.predecessor, ranked.first()) else {
        return Ok(none);
    };
    let max = score_of(package, top);
    if max <= predecessor.score {
        return Ok(none);
    }
    let fallback = Threshold::fallback(predecessor.score, max, package.params.fallback_bps)?;
    Ok(Cutoff {
        mode: CutoffMode::Fallback,
        threshold: Some(fallback),
        eligible: ranked
            .into_iter()
            .filter(|&i| fallback.admits(score_of(package, i)))
            .collect(),
    })
}

/// The coordinator's cluster assignment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterAssignment {
    /// Each entrant's head hotkey, `None` outside the walk.
    pub heads: Vec<Option<AccountId>>,
    /// Each member's statistics against its head, for members joined by
    /// distance rather than identical content.
    pub stats: Vec<Option<NearStats>>,
    /// The smallest aligned distance between two heads, in parts per billion.
    pub min_head_pair_ppb: Option<u32>,
}

/// The coordinator's cluster walk over the eligible entrants in rank order.
/// `pair(head, other)` measures two package indices on the hidden batch
/// predictions ([`near_stats`] of `other` against `head`); identical inference
/// content joins without it, and `other` joins when the aligned distance is
/// at most `d`. Every later head was unassigned during each earlier head's
/// walk, so every pair of heads is measured once.
pub fn assign_clusters(
    package: &ScorePackageV2,
    eligible: &[usize],
    mut pair: impl FnMut(usize, usize) -> Result<NearStats>,
) -> Result<ClusterAssignment> {
    let d = package.params.near_copy_ppb;
    let mut heads = vec![None; package.entrants.len()];
    let mut stats = vec![None; package.entrants.len()];
    let mut apart: Vec<(usize, Option<u32>)> = Vec::new();
    for (pos, &head) in eligible.iter().enumerate() {
        if heads[head].is_some() {
            continue;
        }
        let head_entrant = &package.entrants[head];
        heads[head] = Some(head_entrant.identity.hotkey);
        for &other in &eligible[pos + 1..] {
            if heads[other].is_some() {
                continue;
            }
            if package.entrants[other].content == head_entrant.content {
                heads[other] = Some(head_entrant.identity.hotkey);
                continue;
            }
            let measured = pair(head, other)?;
            if measured.within(d)? {
                heads[other] = Some(head_entrant.identity.hotkey);
                stats[other] = Some(measured);
            } else {
                apart.push((other, measured.distance_ppb()));
            }
        }
    }
    let min_head_pair_ppb = apart
        .into_iter()
        .filter(|&(other, _)| heads[other] == Some(package.entrants[other].identity.hotkey))
        .filter_map(|(_, distance)| distance)
        .min();
    Ok(ClusterAssignment {
        heads,
        stats,
        min_head_pair_ppb,
    })
}

/// One near-copy cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cluster {
    /// Package index of the head, whose score and position the cluster takes.
    pub head: usize,
    /// Package indices in rank order, head first.
    pub members: Vec<usize>,
}

/// Validates the package's published cluster assignment against the cutoff
/// and returns the clusters in head rank order. Each member joined by
/// distance carries its statistics against the head, which must be within
/// `d`; a member with identical content carries none. The statistics are
/// hidden measurements trusted like the scores, and so is the absence of a
/// join; the structure, the identical-content rule and each published
/// statistic are checked here.
pub fn cluster_heads(package: &ScorePackageV2, eligible: &[usize]) -> Result<Vec<Cluster>> {
    let mut position = vec![None; package.entrants.len()];
    for (pos, &i) in eligible.iter().enumerate() {
        position[i] = Some(pos);
    }
    let index_of: BTreeMap<AccountId, usize> = package
        .entrants
        .iter()
        .enumerate()
        .map(|(i, e)| (e.identity.hotkey, i))
        .collect();
    for (i, e) in package.entrants.iter().enumerate() {
        if position[i].is_none() && e.cluster_head.is_some() {
            return Err(Error::Mismatch("cluster outside the cutoff"));
        }
    }
    let mut clusters: Vec<Cluster> = Vec::new();
    let mut cluster_of_head = BTreeMap::<usize, usize>::new();
    let mut first_head_by_content = BTreeMap::<Digest, usize>::new();
    for (pos, &i) in eligible.iter().enumerate() {
        let entrant = &package.entrants[i];
        let head_key = entrant
            .cluster_head
            .ok_or(Error::Mismatch("eligible entrant without cluster"))?;
        let head = *index_of
            .get(&head_key)
            .ok_or(Error::Mismatch("cluster head not an entrant"))?;
        let head_pos = position[head].ok_or(Error::Mismatch("cluster head outside the cutoff"))?;
        if head_pos > pos {
            return Err(Error::Mismatch("cluster head ranks below a member"));
        }
        if package.entrants[head].cluster_head != Some(head_key) {
            return Err(Error::Mismatch("cluster head not a head"));
        }
        let same_content = first_head_by_content.get(&entrant.content).copied();
        if head == i {
            if same_content.is_some() {
                return Err(Error::Mismatch("identical content heads two clusters"));
            }
            first_head_by_content.insert(entrant.content, pos);
            cluster_of_head.insert(i, clusters.len());
            clusters.push(Cluster {
                head: i,
                members: vec![i],
            });
        } else {
            if same_content.is_some_and(|c| head_pos > c) {
                return Err(Error::Mismatch(
                    "identical content outside its first cluster",
                ));
            }
            match &entrant.cluster_stats {
                None if entrant.content == package.entrants[head].content => {}
                Some(stats)
                    if entrant.content != package.entrants[head].content
                        && stats.within(package.params.near_copy_ppb)? => {}
                _ => return Err(Error::Mismatch("cluster member not near its head")),
            }
            clusters[cluster_of_head[&head]].members.push(i);
        }
    }
    Ok(clusters)
}

/// `A_n = sum_{j<n} p^j q^(n-1-j)`, so that `sum_{j<n} r^j = A_n / q^(n-1)`.
fn geometric_numerators(p: u64, q: u64, max: usize) -> Vec<BigUint> {
    // a[n] = A_n; A_1 = 1, A_{n+1} = q A_n + p^n.
    let mut out = vec![BigUint::zero(), BigUint::one()];
    let mut p_pow = BigUint::one();
    for _ in 1..max {
        p_pow = p_pow.mul_u64(p);
        let next = out.last().expect("nonempty").mul_u64(q).add(&p_pow);
        out.push(next);
    }
    out
}

fn gcd_u64(a: u64, b: u64) -> u64 {
    gcd_u128(u128::from(a), u128::from(b)) as u64
}

/// Exact credits `c(kappa, mu)` for occupied bands of the given sizes, with
/// decay `r = decay_bps / 10000`. They sum to exactly one.
pub fn band_credits(band_sizes: &[usize], decay_bps: u16) -> Result<Vec<Vec<Fraction>>> {
    if decay_bps == 0 || u128::from(decay_bps) > BPS {
        return Err(Error::OutOfRange("decay"));
    }
    if band_sizes.contains(&0) {
        return Err(Error::Invalid("empty band"));
    }
    let g = gcd_u64(u64::from(decay_bps), BPS as u64);
    let (p, q) = (u64::from(decay_bps) / g, BPS as u64 / g);
    let bands = band_sizes.len();
    let widest = band_sizes.iter().copied().max().unwrap_or(0).max(bands);
    let a = geometric_numerators(p, q, widest);
    let mut p_pow = vec![BigUint::one()];
    let mut q_pow = vec![BigUint::one()];
    for i in 1..widest {
        p_pow.push(p_pow[i - 1].mul_u64(p));
        q_pow.push(q_pow[i - 1].mul_u64(q));
    }
    let mut out = Vec::with_capacity(bands);
    for (kappa, &size) in band_sizes.iter().enumerate() {
        // r^kappa / sum_{j<B} r^j = p^kappa q^(B-1-kappa) / A_B.
        let band_num = p_pow[kappa].mul(&q_pow[bands - 1 - kappa]);
        let mut row = Vec::with_capacity(size);
        for mu in 0..size {
            let num = band_num.mul(&p_pow[mu]).mul(&q_pow[size - 1 - mu]);
            let den = a[bands].mul(&a[size]);
            row.push(Fraction::new(num, den).ok_or(Error::Invalid("credit"))?);
        }
        out.push(row);
    }
    Ok(out)
}

/// Largest-remainder apportionment of `total` units over exact shares that
/// sum to one, listed in rank order: each takes the integer part of its share,
/// and leftover units go to the largest fractional remainders, ties in rank order.
pub fn largest_remainder(total: u16, shares: &[Fraction]) -> Result<Vec<u16>> {
    let mut floors = Vec::with_capacity(shares.len());
    let mut remainders = Vec::with_capacity(shares.len());
    for share in shares {
        let scaled = share.num.mul_u64(u64::from(total));
        // Largest u in 0..=total with u * den <= total * num.
        let (mut lo, mut hi) = (0u32, u32::from(total));
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if share.den.mul_u64(u64::from(mid)) <= scaled {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let used = share.den.mul_u64(u64::from(lo));
        remainders.push(
            scaled
                .checked_sub(&used)
                .ok_or(Error::Invalid("apportionment"))?,
        );
        floors.push(lo as u16);
    }
    let assigned: u32 = floors.iter().map(|&u| u32::from(u)).sum();
    let leftover = u32::from(total)
        .checked_sub(assigned)
        .ok_or(Error::Invalid("apportionment"))? as usize;
    if leftover > shares.len() || (leftover > 0 && leftover == shares.len()) {
        return Err(Error::Invalid("apportionment"));
    }
    let mut order: Vec<usize> = (0..shares.len()).collect();
    // Remainder fractions rem_i / den_i, descending; the stable sort keeps
    // rank order among exact ties.
    order.sort_by(|&a, &b| {
        remainders[b]
            .mul(&shares[a].den)
            .cmp(&remainders[a].mul(&shares[b].den))
    });
    for &i in order.iter().take(leftover) {
        floors[i] += 1;
    }
    Ok(floors)
}

/// Where a retained cluster landed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// Absolute band index `b`.
    pub band: u128,
    /// `kappa`, from the top occupied band.
    pub band_rank: usize,
    /// `mu`, within the band.
    pub rank_in_band: usize,
    /// `B`.
    pub bands: usize,
    /// `n_kappa`.
    pub band_size: usize,
    /// `c`, the cluster's whole credit.
    pub credit: Fraction,
}

/// A cluster and its placement; `None` when the near-equal drop removed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClusterOutcome {
    pub cluster: Cluster,
    pub placement: Option<Placement>,
}

/// The full `reward-v1` outcome of a package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evaluation {
    pub mode: CutoffMode,
    pub threshold: Option<Threshold>,
    pub bands: Option<Bands>,
    /// Package indices at or above the cutoff, in rank order.
    pub eligible: Vec<usize>,
    pub clusters: Vec<ClusterOutcome>,
    /// Package index of `R_k+1` in a margin cohort.
    pub promoted: Option<usize>,
    /// Units per entrant, in package order.
    pub units: Vec<u16>,
}

impl Evaluation {
    /// Each retained cluster's exact credit in head rank order, as the
    /// package publishes it.
    pub fn credits(&self, package: &ScorePackageV2) -> Vec<ClusterCredit> {
        self.clusters
            .iter()
            .filter_map(|o| {
                o.placement.as_ref().map(|p| ClusterCredit {
                    head: package.entrants[o.cluster.head].identity.hotkey,
                    num: p.credit.num.clone(),
                    den: p.credit.den.clone(),
                })
            })
            .collect()
    }
}

/// The distance monitor's checks:
///
/// - the smallest distance to `R_k` on the batch among gated entrants is
///   recomputed from their public-copy statistics;
/// - the smallest distance between heads, a coordinator measurement, is
///   absent with fewer than two clusters and never below `d`, since a head
///   within `d` of an earlier head would have joined it;
/// - the smallest probe distance between a gated entrant and any
///   fingerprinted artifact, a coordinator measurement, is absent without a
///   gated entrant, present whenever a gated entrant's published reference
///   comparison has a common document, and never above the smallest of those
///   comparisons.
fn check_monitor(package: &ScorePackageV2, clusters: &[Cluster]) -> Result<()> {
    let monitor = &package.distance_monitor;
    let mismatch = Err(Error::Mismatch("distance monitor"));
    if monitor.min_gated_vs_current_ppb != min_gated_vs_current(package) {
        return mismatch;
    }
    match monitor.min_entrant_pair_ppb {
        Some(_) if clusters.len() < 2 => return mismatch,
        Some(d) if d < package.params.near_copy_ppb => return mismatch,
        _ => {}
    }
    let gated = package.entrants.iter().any(EntrantScore::gated);
    let bound = package
        .entrants
        .iter()
        .filter(|e| e.gated())
        .filter_map(|e| e.public.as_ref())
        .flat_map(|p| &p.reference_probes)
        .filter_map(|m| m.stats.distance_ppb())
        .min();
    match (monitor.min_gated_vs_archive_ppb, bound) {
        (Some(_), _) if !gated => mismatch,
        (None, Some(_)) => mismatch,
        (Some(m), Some(b)) if m > b => mismatch,
        _ => Ok(()),
    }
}

fn min_gated_vs_current(package: &ScorePackageV2) -> Option<u32> {
    package
        .entrants
        .iter()
        .filter(|e| e.gated())
        .filter_map(|e| e.public.as_ref()?.current.distance_ppb())
        .min()
}

/// The near-equal drop against the last retained head only, before banding:
/// the indices into `clusters` of the retained clusters, in head rank order.
fn retained_clusters(
    package: &ScorePackageV2,
    clusters: &[Cluster],
    bands: &Bands,
) -> Result<Vec<usize>> {
    let mut retained = Vec::new();
    let mut last: Option<Score> = None;
    for (c, cluster) in clusters.iter().enumerate() {
        let score = score_of(package, cluster.head);
        if let Some(prev) = last
            && bands.near_equal(prev, score)?
        {
            continue;
        }
        last = Some(score);
        retained.push(c);
    }
    Ok(retained)
}

/// The promotion walk over a margin cohort's `candidates`, the retained
/// cluster heads in rank order: the package's audits must name exactly the
/// candidates walked, in order, up to and including the first that passes
/// every promotion condition, or all of them when none passes. Returns the
/// package index of the promoted candidate.
fn check_promotion(package: &ScorePackageV2, candidates: &[usize]) -> Result<Option<usize>> {
    let audits = &package.promotion_audits;
    let mut promoted = None;
    let mut walked = 0;
    for &candidate in candidates {
        let audit = audits
            .get(walked)
            .ok_or(Error::Mismatch("promotion audits"))?;
        audit.validate()?;
        if audit.head != package.entrants[candidate].identity.hotkey {
            return Err(Error::Mismatch("promotion audit order"));
        }
        walked += 1;
        if audit.passes() {
            promoted = Some(candidate);
            break;
        }
    }
    if walked != audits.len() {
        return Err(Error::Mismatch("promotion audits"));
    }
    Ok(promoted)
}

/// Derives the outcome from the package's score components, public-copy
/// records and published cluster assignment, checking the public-copy gate
/// against the published `index` first ([`check_public`]), so copies cannot
/// set `max_N`, trigger the fallback or be promoted. The package's own outcome
/// fields are ignored here; [`check_outcome`] compares them.
pub fn evaluate(package: &ScorePackageV2, index: &PublishedIndex) -> Result<Evaluation> {
    package.validate_inputs()?;
    check_public(package, index)?;
    let cut = cutoff(package)?;
    let mut evaluation = Evaluation {
        mode: cut.mode,
        threshold: cut.threshold,
        bands: None,
        eligible: cut.eligible.clone(),
        clusters: Vec::new(),
        promoted: None,
        units: vec![0; package.entrants.len()],
    };
    let clusters = cluster_heads(package, &cut.eligible)?;
    check_monitor(package, &clusters)?;
    if cut.mode == CutoffMode::NoTarget {
        return Ok(evaluation);
    }
    let bands =
        Bands::new(&package.params, package.reference.score).ok_or(Error::Invalid("band width"))?;
    evaluation.bands = Some(bands);

    let retained = retained_clusters(package, &clusters, &bands)?;

    // Occupied bands from the top. Rank order is score order, so each band is
    // a contiguous run of retained clusters.
    let mut band_runs: Vec<(u128, Vec<usize>)> = Vec::new();
    for &c in &retained {
        let band = bands.band(score_of(package, clusters[c].head));
        match band_runs.last_mut() {
            Some((b, run)) if *b == band => run.push(c),
            Some((b, _)) if *b < band => return Err(Error::Unsorted("bands")),
            _ => band_runs.push((band, vec![c])),
        }
    }
    let sizes: Vec<usize> = band_runs.iter().map(|(_, run)| run.len()).collect();
    let credits = band_credits(&sizes, package.params.decay_bps)?;
    let mut placements: BTreeMap<usize, Placement> = BTreeMap::new();
    for (kappa, (band, run)) in band_runs.iter().enumerate() {
        for (mu, &c) in run.iter().enumerate() {
            placements.insert(
                c,
                Placement {
                    band: *band,
                    band_rank: kappa,
                    rank_in_band: mu,
                    bands: band_runs.len(),
                    band_size: run.len(),
                    credit: credits[kappa][mu].clone(),
                },
            );
        }
    }

    // Equal split, then one largest-remainder pass over members in rank order.
    let mut position = vec![usize::MAX; package.entrants.len()];
    for (pos, &i) in cut.eligible.iter().enumerate() {
        position[i] = pos;
    }
    let mut members: Vec<(usize, Fraction)> = Vec::new();
    for (c, placement) in &placements {
        let cluster = &clusters[*c];
        let m = cluster.members.len() as u64;
        for &i in &cluster.members {
            let share = Fraction::new(
                placement.credit.num.clone(),
                placement.credit.den.mul_u64(m),
            )
            .ok_or(Error::Invalid("share"))?;
            members.push((i, share));
        }
    }
    members.sort_by_key(|(i, _)| position[*i]);
    let shares: Vec<Fraction> = members.iter().map(|(_, s)| s.clone()).collect();
    let units = largest_remainder(package.params.row_total, &shares)?;
    for ((i, _), u) in members.iter().zip(units) {
        evaluation.units[*i] = u;
    }
    if cut.mode == CutoffMode::Margin {
        let candidates: Vec<usize> = retained.iter().map(|&c| clusters[c].head).collect();
        evaluation.promoted = check_promotion(package, &candidates)?;
    } else if !package.promotion_audits.is_empty() {
        return Err(Error::Invalid("promotion audits outside a margin cohort"));
    }
    evaluation.clusters = clusters
        .into_iter()
        .enumerate()
        .map(|(c, cluster)| ClusterOutcome {
            cluster,
            placement: placements.remove(&c),
        })
        .collect();
    Ok(evaluation)
}

/// Recomputes the outcome against the published `index` the validator
/// rebuilt, and requires the package's published mode, promotion, exact
/// cluster credits and units to equal it.
pub fn check_outcome(package: &ScorePackageV2, index: &PublishedIndex) -> Result<Evaluation> {
    package.validate()?;
    let evaluation = evaluate(package, index)?;
    let promoted = evaluation
        .promoted
        .map(|i| package.entrants[i].identity.hotkey);
    if package.mode != evaluation.mode
        || package.promoted != promoted
        || package
            .entrants
            .iter()
            .zip(&evaluation.units)
            .any(|(e, &u)| e.units != u)
    {
        return Err(Error::Mismatch("reward outcome"));
    }
    if package.credits != evaluation.credits(package) {
        return Err(Error::Mismatch("cluster credits"));
    }
    Ok(evaluation)
}

/// The coordinator's measurements on hidden predictions, for [`finalize`].
/// Package indices name entrants; `x` is always the entrant.
pub trait Measurements {
    /// The entrant against `R_k` on the batch ([`near_stats`]).
    fn current(&mut self, entrant: usize) -> Result<NearStats>;
    /// The entrant against `R_k-1` on the batch.
    fn predecessor(&mut self, entrant: usize) -> Result<NearStats>;
    /// The entrant against the recorded fingerprint of the published artifact
    /// `content` on probe `version`: [`near_stats`] of the entrant's probe
    /// predictions against the fingerprint, with both models' probe scores
    /// ([`document_score`], failures at zero). [`finalize`] asks for `R_k` and
    /// `R_k-1` on every probe version in use, for every scored entrant, and
    /// publishes the results.
    fn reference_probe(
        &mut self,
        entrant: usize,
        content: &Digest,
        version: u32,
    ) -> Result<ProbeMatch>;
    /// The entrant's comparisons on every probe version in use with the
    /// recorded fingerprint of every other fingerprinted published artifact.
    /// Asked only when neither reference is a near copy on the batch.
    fn archive_probe(&mut self, entrant: usize) -> Result<ArchiveProbe>;
    /// `other` against cluster head `head` on the batch.
    fn pair(&mut self, head: usize, other: usize) -> Result<NearStats>;
    /// The promotion audit of candidate `head`, a retained cluster head of a
    /// margin cohort: the strict false-positive tier on the first canary
    /// draw, and the canary, re-audited on a fresh draw after a first flag.
    /// [`finalize`] asks for the candidates in rank order until one passes.
    fn promotion_audit(&mut self, head: usize) -> Result<PromotionAudit>;
}

/// An entrant's probe comparisons with the archived fingerprints other than
/// `R_k` and `R_k-1`, for [`Measurements::archive_probe`]. Every scored
/// entrant runs on the probe for its own fingerprint whether or not this is
/// asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArchiveProbe {
    /// The comparisons that may be near copies. Only near ones can match, so
    /// others may be left out; comparisons naming `R_k` or `R_k-1` are
    /// ignored, since [`Measurements::reference_probe`] supplies those.
    pub candidates: Vec<ProbeMatch>,
    /// The smallest `floor(A_sym)` in parts per billion over every
    /// comparison, near or not; `None` when no comparison has a common
    /// document.
    pub min_distance_ppb: Option<u32>,
}

/// The coordinator's finalization:
///
/// 1. an entrant whose inference content is in the published `index` fails
///    `public.content`; it must not have been executed;
/// 2. every scored entrant, gate-failed ones included, is compared with
///    `R_k` and `R_k-1` on the batch and with both references' fingerprints
///    on every probe version in use, and all of these statistics are
///    published; only when neither reference is a near copy on the batch is
///    it compared with the rest of the archive, and the closest near probe
///    comparison, the references' included, becomes its probe match. A near
///    copy fails the public-copy gate;
/// 3. the cutoff, the cluster walk with the hidden batch statistics, the
///    distance monitor and the outcome fields follow, all after the gate.
pub fn finalize(
    package: &mut ScorePackageV2,
    index: &PublishedIndex,
    measure: &mut impl Measurements,
) -> Result<Evaluation> {
    index.validate()?;
    if index.binding()? != package.published {
        return Err(Error::Mismatch("published index"));
    }
    for e in &mut package.entrants {
        e.gate_failures.retain(|g| g.gate != Gate::PublicCopy);
        e.public = None;
        e.cluster_head = None;
        e.cluster_stats = None;
        e.units = 0;
        if index.contains(&e.content) {
            if e.score.is_some() {
                return Err(Error::Invalid("published content executed"));
            }
            e.gate_failures.push(PublicCopy::Content.failure());
            e.gate_failures.sort_by_key(|g| g.gate);
        }
    }
    package.credits.clear();
    package.promotion_audits.clear();
    package.distance_monitor = DistanceMonitor::default();
    let width = band_width(&package.params, package.reference.score);
    let reference_keys = package.reference_probe_keys();
    let references: Vec<Digest> = core::iter::once(&package.reference)
        .chain(&package.predecessor)
        .map(|r| r.model.content)
        .collect();
    let mut archive_distance: Vec<Option<u32>> = vec![None; package.entrants.len()];
    for (i, closest) in archive_distance.iter_mut().enumerate() {
        let Some(score) = package.entrants[i].score else {
            continue;
        };
        let mut reference_probes = Vec::with_capacity(reference_keys.len());
        for (content, version) in &reference_keys {
            let found = measure.reference_probe(i, content, *version)?;
            if (found.content, found.version) != (*content, *version) {
                return Err(Error::Mismatch("reference probe comparison"));
            }
            reference_probes.push(found);
        }
        let mut public = PublicCheck {
            current: measure.current(i)?,
            predecessor: match package.predecessor {
                Some(_) => Some(measure.predecessor(i)?),
                None => None,
            },
            reference_probes,
            probe_match: None,
        };
        let mut copy = batch_copy(package, score, &public, width)?;
        if copy.is_none() {
            let archive = measure.archive_probe(i)?;
            *closest = public
                .reference_probes
                .iter()
                .filter_map(|m| m.stats.distance_ppb())
                .chain(archive.min_distance_ppb)
                .min();
            let candidates = public.reference_probes.iter().cloned().chain(
                archive
                    .candidates
                    .into_iter()
                    .filter(|m| !references.contains(&m.content)),
            );
            public.probe_match = closest_probe_match(candidates, &package.params, width)?;
            copy = public.probe_match.as_ref().map(|_| PublicCopy::Probe);
        }
        let entrant = &mut package.entrants[i];
        entrant.public = Some(public);
        if let Some(copy) = copy {
            // Gate failures are recorded in gate order, so the public-copy
            // failure (gate 5) goes before any later gate already recorded,
            // such as the loose false-positive tier (gate 6).
            entrant.gate_failures.push(copy.failure());
            entrant.gate_failures.sort_by_key(|g| g.gate);
        }
    }
    package.validate_inputs()?;
    let cut = cutoff(package)?;
    let assignment = assign_clusters(package, &cut.eligible, |h, o| measure.pair(h, o))?;
    for ((e, head), stats) in package
        .entrants
        .iter_mut()
        .zip(assignment.heads)
        .zip(assignment.stats)
    {
        e.cluster_head = head;
        e.cluster_stats = stats;
    }
    package.distance_monitor = DistanceMonitor {
        min_entrant_pair_ppb: assignment.min_head_pair_ppb,
        min_gated_vs_current_ppb: min_gated_vs_current(package),
        min_gated_vs_archive_ppb: package
            .entrants
            .iter()
            .zip(&archive_distance)
            .filter(|(e, _)| e.gated())
            .filter_map(|(_, d)| *d)
            .min(),
    };
    if cut.mode == CutoffMode::Margin {
        let clusters = cluster_heads(package, &cut.eligible)?;
        let bands = Bands::new(&package.params, package.reference.score)
            .ok_or(Error::Invalid("band width"))?;
        for c in retained_clusters(package, &clusters, &bands)? {
            let head = clusters[c].head;
            let audit = measure.promotion_audit(head)?;
            audit.validate()?;
            if audit.head != package.entrants[head].identity.hotkey {
                return Err(Error::Mismatch("promotion audit head"));
            }
            let passes = audit.passes();
            package.promotion_audits.push(audit);
            if passes {
                break;
            }
        }
    }
    let evaluation = evaluate(package, index)?;
    for (e, &u) in package.entrants.iter_mut().zip(&evaluation.units) {
        e.units = u;
    }
    package.mode = evaluation.mode;
    package.promoted = evaluation
        .promoted
        .map(|i| package.entrants[i].identity.hotkey);
    package.credits = evaluation.credits(package);
    package.validate()?;
    Ok(evaluation)
}

/// The references for the next cohort: the promoted entrant becomes `R_k+1`
/// with `R_k` as its predecessor; otherwise the references do not change.
pub fn next_references(
    current: &crate::ReferenceSet,
    package: &ScorePackageV2,
    index: &PublishedIndex,
) -> Result<crate::ReferenceSet> {
    let evaluation = check_outcome(package, index)?;
    let Some(i) = evaluation.promoted else {
        return Ok(*current);
    };
    let entrant = &package.entrants[i];
    Ok(crate::ReferenceSet {
        index: current
            .index
            .checked_add(1)
            .ok_or(Error::Overflow("reference index"))?,
        current: ReferenceModel {
            content: entrant.content,
            artifact: entrant.artifact,
        },
        predecessor: Some(current.current),
    })
}

/// Derives the canonical row for the active `target` against a validator's
/// finalized registration snapshot. A cohort target needs its package and the
/// published index as of that cohort's manifest, against which
/// [`check_outcome`] runs; the launch burn row needs neither.
///
/// Burned units go to the UID that the snapshot's finalized owner hotkey
/// holds at this tempo, whatever owner hotkey was in force when the target
/// was signed, and the row records that hotkey. If units must burn and the
/// owner hotkey holds no UID in the snapshot, the row fails with
/// [`Error::Deferred`]: that tempo's row is deferred rather than sent to
/// another UID, and a later snapshot is tried afresh. A row with nothing to
/// burn does not need the owner's UID.
pub fn derive_row(
    target: &CalculationTargetV2,
    package: Option<(&ScorePackageV2, &PublishedIndex)>,
    snapshot: &RegistrationSnapshotV2,
) -> Result<WeightRowV2> {
    target.validate()?;
    snapshot.validate()?;
    if snapshot.chain != target.chain {
        return Err(Error::Mismatch("registration chain"));
    }
    let owner_uid = snapshot.by_hotkey(&snapshot.owner_hotkey).map(|n| n.uid);
    let burn = || owner_uid.ok_or(Error::Deferred("owner hotkey not registered"));
    let mut units = BTreeMap::<u16, u32>::new();
    match (&target.source, package) {
        (TargetSource::LaunchBurn, None) => {
            units.insert(burn()?, u32::from(crate::records_v2::ROW_TOTAL));
        }
        (TargetSource::Cohort { .. }, Some((package, index))) => {
            target.check_package(package)?;
            check_outcome(package, index)?;
            for entrant in package.entrants.iter().filter(|e| e.units > 0) {
                let uid = match snapshot.by_hotkey(&entrant.identity.hotkey) {
                    Some(n) if n.registration_block == entrant.identity.registration_block => n.uid,
                    _ => burn()?,
                };
                *units.entry(uid).or_default() += u32::from(entrant.units);
            }
        }
        (TargetSource::LaunchBurn, Some(_)) => {
            return Err(Error::Invalid("burn target has no package"));
        }
        (TargetSource::Cohort { .. }, None) => {
            return Err(Error::Invalid("cohort target needs its package"));
        }
    }
    let entries = units
        .into_iter()
        .map(|(uid, weight)| {
            Ok(RowEntry {
                uid,
                weight: u16::try_from(weight).map_err(|_| Error::Overflow("row weight"))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let row = WeightRowV2 {
        chain: target.chain.clone(),
        calculation_target: target.digest()?,
        reward_rule: target.reward_rule,
        registration_block: snapshot.block,
        registration: snapshot.digest()?,
        owner_hotkey: snapshot.owner_hotkey,
        entries,
    };
    row.validate()?;
    Ok(row)
}
