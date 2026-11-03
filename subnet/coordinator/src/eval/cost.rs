//! Lane planning and the scoring cost model (pure).
//!
//! A scoring job is one scored artifact. Each job gets
//! `W = max(1, min(clamp(ceil(n / lane_docs), 1, C), floor(cap / J)))` lanes,
//! where `C` is its chunk count and `cap` the smallest of the plan's lane cap,
//! the deployment's `MAX_SCORE_LANES` and the lanes the scoring budget affords.
//! Lane `i` of a job prefers chunks `[floor(i C / W), floor((i + 1) C / W))` and
//! steals any other unclaimed chunk of the same job. When `J > cap`, every job
//! gets one lane and live lanes are held at `cap` (waves).
//!
//! Costs are in nano-dollars; budgets in micro-dollars. Every rate comes from
//! the plan's [`CostModel`], never from a constant here.

use crate::eval::plan::CostModel;

/// One planned scoring lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneSpec {
    pub job: u32,
    /// Index within the cohort: the lane's Durable Object name suffix.
    pub lane: u32,
    /// Index within its job.
    pub index: u32,
    pub chunk_lo: u32,
    pub chunk_hi: u32,
}

pub fn chunks_for(n: u32, chunk_docs: u32) -> u32 {
    n.div_ceil(chunk_docs.max(1))
}

/// Lanes per job, before the cap: `clamp(ceil(n / lane_docs), 1, C)`.
pub fn wanted_lanes(n: u32, chunk_docs: u32, lane_docs: u32) -> u32 {
    let c = chunks_for(n, chunk_docs).max(1);
    n.div_ceil(lane_docs.max(1)).clamp(1, c)
}

/// The affinity range of lane `i` of `w` over `c` chunks.
pub fn lane_range(i: u32, w: u32, c: u32) -> (u32, u32) {
    let w = u64::from(w.max(1));
    let (i, c) = (u64::from(i), u64::from(c));
    ((i * c / w) as u32, ((i + 1) * c / w) as u32)
}

/// Plans every scoring lane of a cohort of `jobs` jobs at `n` documents.
pub fn plan_score_lanes(jobs: u32, n: u32, chunk_docs: u32, lane_docs: u32, cap: u32) -> Vec<LaneSpec> {
    let c = chunks_for(n, chunk_docs);
    let w0 = wanted_lanes(n, chunk_docs, lane_docs);
    let w = w0.min(cap.max(1) / jobs.max(1)).max(1);
    let mut out = Vec::with_capacity((jobs * w) as usize);
    for job in 0..jobs {
        for i in 0..w {
            let (lo, hi) = lane_range(i, w, c);
            out.push(LaneSpec { job, lane: job * w + i, index: i, chunk_lo: lo, chunk_hi: hi });
        }
    }
    out
}

/// Upper bound on a cohort's scoring cost before anything is measured:
/// `J * n * doc_ub + L * cold_ub` (nano-dollars).
pub fn scoring_upper_bound_nanousd(jobs: u32, n: u32, lanes: u32, c: &CostModel) -> u128 {
    u128::from(jobs) * u128::from(n) * u128::from(c.doc_ub_nanousd) + u128::from(lanes) * u128::from(c.cold_ub_nanousd)
}

/// How many lanes the budget affords once every document's bound is paid.
pub fn budget_lanes(jobs: u32, n: u32, budget_microusd: u64, c: &CostModel) -> u32 {
    let docs = u128::from(jobs) * u128::from(n) * u128::from(c.doc_ub_nanousd);
    let budget = u128::from(budget_microusd) * 1000;
    (budget.saturating_sub(docs) / u128::from(c.cold_ub_nanousd.max(1))).min(u128::from(u32::MAX)) as u32
}

/// Measured cost of a lane from its metering: provisioned instance time, the
/// container's CPU, and the lane Durable Object's active time.
pub fn lane_cost_nanousd(active_ms: u64, cpu_ms: u64, do_ms: u64, c: &CostModel) -> u128 {
    (u128::from(active_ms) * u128::from(c.instance_nanousd_per_s)
        + u128::from(cpu_ms) * u128::from(c.vcpu_nanousd_per_s)
        + u128::from(do_ms) * u128::from(c.lane_do_nanousd_per_s))
        / 1000
}

/// Bound on what a lane may have spent beyond its last report: provisioned for
/// `ms` with every vCPU busy and its Durable Object active.
pub fn unreported_bound_nanousd(ms: u64, c: &CostModel) -> u128 {
    let per_s = u128::from(c.instance_nanousd_per_s)
        + u128::from(c.vcpus) * u128::from(c.vcpu_nanousd_per_s)
        + u128::from(c.lane_do_nanousd_per_s);
    u128::from(ms) * per_s / 1000
}

pub fn nano_to_micro_ceil(n: u128) -> u64 {
    n.div_ceil(1000).min(u128::from(u64::MAX)) as u64
}

/// An operator scorer probe's charge: its instance's wall time at the
/// cost model's provisioned and per-vCPU rates (rounded up), plus the lane
/// object's duration.
pub fn probe_charge_microusd(model: &CostModel, wall_ms: u64) -> u64 {
    let per_s = model
        .instance_nanousd_per_s
        .saturating_add(model.vcpu_nanousd_per_s.saturating_mul(u64::from(model.vcpus)))
        .saturating_add(model.lane_do_nanousd_per_s);
    nano_to_micro_ceil(u128::from(per_s).saturating_mul(u128::from(wall_ms)).div_ceil(1000))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::plan::tests::fanout;

    #[test]
    fn lane_planning_at_the_edges() {
        // The design's table: n = 1k, chunks of 32, economical width 200, cap 375.
        assert_eq!(wanted_lanes(1000, 32, 200), 5);
        assert_eq!(plan_score_lanes(3, 1000, 32, 200, 375).len(), 15);
        assert_eq!(plan_score_lanes(65, 1000, 32, 200, 375).len(), 325);
        assert_eq!(plan_score_lanes(129, 1000, 32, 200, 375).len(), 258, "the cap sets W = 2");
        assert_eq!(plan_score_lanes(41, 1000, 32, 200, 375).len(), 205);
        // Wide width: lanes = jobs x chunks while it fits.
        assert_eq!(plan_score_lanes(11, 1000, 32, 32, 375).len(), 352);
        // More jobs than the cap: one lane each (waves at dispatch).
        let waves = plan_score_lanes(400, 1000, 32, 200, 375);
        assert_eq!(waves.len(), 400);
        assert!(waves.iter().all(|l| (l.chunk_lo, l.chunk_hi) == (0, 32)));
        // C not divisible by W: ranges tile [0, C) exactly, without overlap.
        for (c_docs, w_docs, n) in [(32, 200, 1000), (7, 30, 100), (5, 5, 23), (128, 128, 10)] {
            let lanes = plan_score_lanes(2, n, c_docs, w_docs, 1000);
            let c = chunks_for(n, c_docs);
            for job in 0..2 {
                let mut next = 0;
                for l in lanes.iter().filter(|l| l.job == job) {
                    assert_eq!(l.chunk_lo, next);
                    assert!(l.chunk_hi > l.chunk_lo, "no empty lane while W <= C");
                    next = l.chunk_hi;
                }
                assert_eq!(next, c);
            }
            let names: std::collections::BTreeSet<u32> = lanes.iter().map(|l| l.lane).collect();
            assert_eq!(names.len(), lanes.len(), "lane indices are unique");
        }
        assert_eq!(lane_range(4, 5, 32), (25, 32));
    }

    #[test]
    fn budget_bounds_and_lane_costs() {
        let c = fanout(200).cost;
        // 65 artifacts x 1k at $0.00004 a document plus 325 cold paths at $0.0025.
        let ub = scoring_upper_bound_nanousd(65, 1000, 325, &c);
        assert_eq!(ub, 65 * 1000 * 40_000 + 325 * 2_500_000);
        assert_eq!(nano_to_micro_ceil(ub), 3_412_500);
        assert_eq!(budget_lanes(65, 1000, 3_412_500, &c), 325);
        assert_eq!(budget_lanes(65, 1000, 2_000_000, &c), 0, "the documents alone exceed it");
        // 30 s provisioned, 60 vCPU-s, 40 s of lane DO time.
        assert_eq!(lane_cost_nanousd(30_000, 60_000, 40_000, &c), 942_000 + 1_200_000 + 62_520);
        assert_eq!(unreported_bound_nanousd(1000, &c), 31_400 + 80_000 + 1_563);
    }
}
