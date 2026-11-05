//! The plan field `CohortPlan.generation` (`slop_ninja_generation_plan_v2`)
//! and its checks.
//!
//! Absent, a cohort runs today's v1 generation unchanged. Present, its shape
//! is checked when the plan is parsed ([`GenerationPlan::validate`]); the
//! checks that need the bound objects and the generation Worker run at
//! admission ([`admit`]): the manifest must be `measured`, every pool domain
//! must have a register, both keys must resolve, the plan's generators must
//! be the manifest's Workers AI ids, the frontier lease must fit the
//! measured latencies and the alarm wall, and the cohort's generation must
//! fit its generating window by the measured projection ([`project`]),
//! with every frontier item fallen back to Workers AI too. Nothing here
//! reads a runtime version.

use crate::manifest::{Manifest, ObjectRef, Refused, Status};
use crate::vendor::{Lane, Vendor};
use crate::voice::VoiceSplit;
use crate::{GENERATION_PLAN_SCHEMA, ROTATION_VERSION};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontierMode {
    /// Every frontier call is synchronous (OpenAI on Flex).
    Sync,
    /// Anthropic items first as a Message Batch, falling back to sync at a
    /// derived deadline. Not run by this build.
    BatchThenSync,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrontierLanes {
    /// The frontier claim lease; the call timeout is the lease less the
    /// lanes' commit margin.
    pub lease_ms: u64,
    /// Frontier calls in flight per lane (default: the plan's generation
    /// lanes' value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inflight_per_lane: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationPlan {
    pub schema: String,
    pub manifest: ObjectRef,
    pub rotation: String,
    pub frontier_mode: FrontierMode,
    pub frontier_lanes: FrontierLanes,
    pub voice_split: String,
}

/// The lanes' fixed bounds the plan is checked against (the coordinator's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneBounds {
    /// The longest a Durable Object alarm may run (`ALARM_WALL_MS`).
    pub alarm_wall_ms: u64,
    /// One lease step of the lanes (the margin a lease leaves the wall).
    pub lease_step_ms: u64,
    /// What a call's timeout leaves of its lease for the commit.
    pub commit_margin_ms: u64,
    /// The per-invocation connection limit (calls in flight per lane).
    pub max_inflight: u32,
}

impl LaneBounds {
    /// The longest call timeout any frontier lease can give: the lease
    /// ceiling (the alarm wall less one lease step) less the commit margin.
    /// A cell measured slower can never be used.
    pub const fn call_timeout_ceiling_ms(&self) -> u64 {
        self.alarm_wall_ms.saturating_sub(self.lease_step_ms).saturating_sub(self.commit_margin_ms)
    }
}

impl GenerationPlan {
    /// The plan's voice split (validated).
    pub fn split(&self) -> Option<VoiceSplit> {
        VoiceSplit::parse(&self.voice_split)
    }

    pub fn validate(&self, b: &LaneBounds) -> Result<(), Refused> {
        let need = |c: bool, w: &'static str| if c { Ok(()) } else { Err(Refused(w)) };
        need(self.schema == GENERATION_PLAN_SCHEMA, "generation.schema")?;
        self.manifest.validate("generation.manifest")?;
        need(self.rotation == ROTATION_VERSION, "generation.rotation (generation_rotation_v1)")?;
        need(VoiceSplit::parse(&self.voice_split).is_some(), "generation.voice_split (profile_none_half_v1 or profile_none_category_v1)")?;
        need(self.frontier_mode == FrontierMode::Sync, "generation.frontier_mode (this build runs sync only)")?;
        let l = &self.frontier_lanes;
        need(
            l.lease_ms > b.commit_margin_ms && l.lease_ms.saturating_add(b.lease_step_ms) <= b.alarm_wall_ms,
            "generation.frontier_lanes.lease_ms (within the alarm wall less one lease step)",
        )?;
        need(l.inflight_per_lane.is_none_or(|n| (1..=b.max_inflight).contains(&n)), "generation.frontier_lanes.inflight_per_lane")?;
        Ok(())
    }
}

/// The generation Worker's `/health` answer: whether each key resolves to a
/// non-empty value (nothing else).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Health {
    /// The interface version the generation Worker speaks ([`crate::iface::MARKER_VALUE`]).
    pub interface: String,
    pub openai: bool,
    pub anthropic: bool,
}

/// The cohort's generating phase as admission sees it: the coordinator's
/// calendar window and the plan's document count and lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Phase {
    /// Generated documents (mixed and model-only).
    pub documents: u64,
    /// The generating window of the calendar (milliseconds).
    pub generating_window_ms: u64,
    /// Generation lanes of the plan.
    pub lanes: u32,
    /// Workers AI calls in flight per lane (the plan's generation lanes').
    pub workers_ai_inflight_per_lane: u32,
}

/// The projection of one lane's share (`phase_fit_v1`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Projection {
    /// Total call time over the lane's concurrency.
    pub work_ms: u64,
    /// The slowest model's rate limit (`rate_limits.rpm`), where declared.
    pub rate_ms: u64,
}

impl Projection {
    pub fn ms(&self) -> u64 {
        self.work_ms.max(self.rate_ms)
    }
}

/// The projected time of `documents` on `lane` at `concurrency` calls in
/// flight (`phase_fit_v1`): the documents spread equally over the lane's
/// models and each model's usable levels (the rotation's counts, rounded
/// up), each call taking its cell's measured slowest latency times its
/// measured attempts per accepted answer (`calls / accepted`, rounded up);
/// and, per model with a declared `rpm`, its calls at that rate. An upper
/// bound, not a schedule: run-time slowness still only moves advancement to
/// a later boundary.
pub fn project(m: &Manifest, lane: Lane, documents: u64, concurrency: u64) -> Projection {
    let models = m.lane_models(lane);
    let per_model = documents.div_ceil(models.len().max(1) as u64);
    let mut work: u64 = 0;
    let mut rate: u64 = 0;
    for x in &models {
        let levels = x.usable_efforts();
        let per_cell = per_model.div_ceil(levels.len().max(1) as u64);
        let mut model_calls: u64 = 0;
        for e in &levels {
            let (lat, attempts) = x.measured(*e).map_or((0, 1), |s| (s.latency_ms_max, u64::from(s.calls).div_ceil(u64::from(s.accepted.max(1))).max(1)));
            let calls = per_cell.saturating_mul(attempts);
            model_calls = model_calls.saturating_add(calls);
            work = work.saturating_add(calls.saturating_mul(lat));
        }
        if let Some(rpm) = x.rate_limits.as_ref().and_then(|r| r.rpm).filter(|r| *r > 0) {
            rate = rate.max(model_calls.saturating_mul(60_000).div_ceil(rpm));
        }
    }
    Projection { work_ms: work.div_ceil(concurrency.max(1)), rate_ms: rate }
}

/// The phase projections a plan is admitted on: the normal split (both
/// lanes in parallel) and every frontier item fallen back to Workers AI (a
/// paused vendor moves its share there).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhaseFit {
    pub frontier: Projection,
    pub workers_ai: Projection,
    pub workers_ai_all: Projection,
}

pub fn phase_fit(p: &GenerationPlan, m: &Manifest, phase: &Phase) -> PhaseFit {
    let frontier_docs = phase.documents.div_ceil(2);
    let lanes = u64::from(phase.lanes.max(1));
    let wai_conc = lanes.saturating_mul(u64::from(phase.workers_ai_inflight_per_lane.max(1)));
    let frontier_conc = lanes.saturating_mul(u64::from(p.frontier_lanes.inflight_per_lane.unwrap_or(phase.workers_ai_inflight_per_lane).max(1)));
    PhaseFit {
        frontier: project(m, Lane::Frontier, frontier_docs, frontier_conc),
        workers_ai: project(m, Lane::WorkersAi, phase.documents - frontier_docs, wai_conc),
        workers_ai_all: project(m, Lane::WorkersAi, phase.documents, wai_conc),
    }
}

/// The admission checks of a v2 plan against its bound manifest (already
/// parsed with [`Manifest::parse_bound`]), the pool's domains, the plan's
/// Workers AI generators, the generation Worker's health and the cohort's
/// generating phase.
#[allow(clippy::too_many_arguments)]
pub fn admit(p: &GenerationPlan, m: &Manifest, pool_domains: &[String], generators: &[String], health: Option<Health>, b: &LaneBounds, phase: &Phase) -> Result<PhaseFit, Refused> {
    let need = |c: bool, w: &'static str| if c { Ok(()) } else { Err(Refused(w)) };
    p.validate(b)?;
    need(m.status == Status::Measured, "generation.manifest (not measured: run the measurement first)")?;
    need(pool_domains.iter().all(|d| m.register(d).is_some()), "generation.manifest (a pool domain without a register)")?;
    let wai: BTreeSet<&str> = m.lane_models(Lane::WorkersAi).iter().map(|x| x.id.as_str()).collect();
    let gens: BTreeSet<&str> = generators.iter().map(String::as_str).collect();
    need(wai == gens && gens.len() == generators.len(), "generators (the manifest's Workers AI ids)")?;
    let h = health.ok_or(Refused("generation.keys (the generation Worker did not answer)"))?;
    need(h.interface == crate::iface::MARKER_VALUE, "generation.interface (the generation Worker speaks another interface version)")?;
    for v in [Vendor::Openai, Vendor::Anthropic] {
        let uses = m.models.iter().any(|x| x.vendor == v);
        let ok = match v {
            Vendor::Openai => h.openai,
            _ => h.anthropic,
        };
        need(!uses || ok, "generation.keys (a vendor key is missing)")?;
    }
    // Every usable frontier cell's measured latency fits the call timeout.
    let timeout = p.frontier_lanes.lease_ms - b.commit_margin_ms;
    let slowest = m
        .lane_models(Lane::Frontier)
        .iter()
        .flat_map(|x| x.usable_efforts().into_iter().filter_map(|e| x.measured(e).map(|s| s.latency_ms_max)))
        .max()
        .unwrap_or(0);
    need(slowest <= timeout, "generation.frontier_lanes.lease_ms (below the slowest measured cell: mark it unusable or raise the lease)")?;
    let fit = phase_fit(p, m, phase);
    let window = phase.generating_window_ms;
    need(fit.frontier.ms() <= window && fit.workers_ai.ms() <= window, "generation (the projected generation does not fit the generating window)")?;
    need(fit.workers_ai_all.ms() <= window, "generation (with every frontier item fallen back to Workers AI, the projection does not fit the generating window)")?;
    Ok(fit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::fixture::{lane_ids, measured, planned};

    const B: LaneBounds = LaneBounds { alarm_wall_ms: 900_000, lease_step_ms: 60_000, commit_margin_ms: 15_000, max_inflight: 6 };

    fn plan() -> GenerationPlan {
        GenerationPlan {
            schema: GENERATION_PLAN_SCHEMA.into(),
            manifest: ObjectRef { key: "generation/manifests/generator-manifest-v1.json".into(), sha256: "cd".repeat(32), bytes: 9000 },
            rotation: ROTATION_VERSION.into(),
            frontier_mode: FrontierMode::Sync,
            frontier_lanes: FrontierLanes { lease_ms: 300_000, inflight_per_lane: None },
            voice_split: VoiceSplit::NoneHalf.code().into(),
        }
    }

    #[test]
    fn the_shape_is_checked() {
        plan().validate(&B).unwrap();
        let mut p = plan();
        p.frontier_mode = FrontierMode::BatchThenSync;
        assert!(p.validate(&B).is_err());
        let mut p = plan();
        p.frontier_lanes.lease_ms = 850_000;
        assert!(p.validate(&B).is_err());
        let mut p = plan();
        p.frontier_lanes.inflight_per_lane = Some(7);
        assert!(p.validate(&B).is_err());
        let mut v = serde_json::to_value(plan()).unwrap();
        v["budget_cap"] = 1.into();
        assert!(serde_json::from_value::<GenerationPlan>(v).is_err(), "unknown fields are refused");
    }

    const PHASE: Phase = Phase { documents: 510, generating_window_ms: 3_600_000, lanes: 4, workers_ai_inflight_per_lane: 6 };

    fn keys() -> Option<Health> {
        Some(Health { interface: crate::iface::MARKER_VALUE.into(), openai: true, anthropic: true })
    }

    #[test]
    fn admission_needs_a_measured_manifest_registers_keys_and_a_fitting_lease() {
        let m = measured(planned());
        let gens = lane_ids(&m, Lane::WorkersAi);
        let doms = vec!["wikipedia".to_owned(), "fineweb".to_owned()];
        admit(&plan(), &m, &doms, &gens, keys(), &B, &PHASE).unwrap();
        assert_eq!(admit(&plan(), &planned(), &doms, &gens, keys(), &B, &PHASE).unwrap_err().0, "generation.manifest (not measured: run the measurement first)");
        let more = vec!["wikipedia".to_owned(), "plos".to_owned()];
        assert_eq!(admit(&plan(), &m, &more, &gens, keys(), &B, &PHASE).unwrap_err().0, "generation.manifest (a pool domain without a register)");
        assert!(admit(&plan(), &m, &doms, &gens[1..], keys(), &B, &PHASE).is_err());
        let half = Some(Health { anthropic: false, ..keys().unwrap() });
        assert_eq!(admit(&plan(), &m, &doms, &gens, half, &B, &PHASE).unwrap_err().0, "generation.keys (a vendor key is missing)");
        let other = Some(Health { interface: "v0".into(), ..keys().unwrap() });
        assert!(admit(&plan(), &m, &doms, &gens, other, &B, &PHASE).unwrap_err().0.starts_with("generation.interface"));
        assert!(admit(&plan(), &m, &doms, &gens, None, &B, &PHASE).is_err());
        let mut p = plan();
        p.frontier_lanes.lease_ms = 70_000;
        assert!(admit(&p, &m, &doms, &gens, keys(), &B, &PHASE).is_err(), "60 s measured maximum does not fit 55 s");
        assert_eq!(B.call_timeout_ceiling_ms(), 825_000);
    }

    #[test]
    fn the_phase_projection_bounds_the_generating_window() {
        let m = measured(planned());
        let gens = lane_ids(&m, Lane::WorkersAi);
        let doms = vec!["wikipedia".to_owned()];
        // 255 frontier documents over 8 models (32 each) and their levels:
        // every cell 60 s, 20 calls for 19 accepted (2 calls per answer).
        let f = project(&m, Lane::Frontier, 255, 24);
        let cells: u64 = m.lane_models(Lane::Frontier).iter().map(|x| 32u64.div_ceil(x.usable_efforts().len() as u64) * x.usable_efforts().len() as u64).sum();
        assert_eq!(f.work_ms, (cells * 2 * 60_000).div_ceil(24));
        assert_eq!(f.rate_ms, 0, "no declared rate limits");
        let fit = admit(&plan(), &m, &doms, &gens, keys(), &B, &PHASE).unwrap();
        assert!(fit.workers_ai_all.ms() >= fit.workers_ai.ms());
        // A window shorter than the projection is refused.
        let short = Phase { generating_window_ms: fit.frontier.ms().max(fit.workers_ai_all.ms()) - 1, ..PHASE };
        assert!(admit(&plan(), &m, &doms, &gens, keys(), &B, &short).unwrap_err().0.starts_with("generation ("));
        // A declared 20-rpm Workers AI model bounds the all-fallback case by its rate.
        let mut slow = m.clone();
        let x = slow.models.iter_mut().find(|x| x.id == "@cf/meta/llama-3.2-3b-instruct").unwrap();
        x.rate_limits = Some(crate::manifest::RateLimits { rpm: Some(20), tpm: None, itpm: None, otpm: None, source: "test".into() });
        let all = project(&slow, Lane::WorkersAi, 510, 24);
        assert_eq!(all.rate_ms, (510u64.div_ceil(14) * 2 * 60_000).div_ceil(20));
        let tight = Phase { generating_window_ms: all.rate_ms - 1, documents: 510, ..PHASE };
        let fit = phase_fit(&plan(), &slow, &tight);
        assert!(fit.workers_ai_all.ms() > tight.generating_window_ms);
    }
}
