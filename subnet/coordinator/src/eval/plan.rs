//! The operator's frozen cohort plan (testnet).
//!
//! A plan fixes everything the cohort engine needs before it opens: size and mix,
//! timing in tempos (K, admission and transition minimums), the human pool, the
//! generator set and budget, the roster (reference R_k, optional predecessor
//! R_{k-1}, entrants), the allow-list, the burn identity and the reward rule. Its
//! digest is the SHA-256 of the exact submitted bytes and is bound into the
//! cohort's evaluation-set digest, which the signed manifest carries.
//!
//! Rosters: a v3 plan with an [`AdmissionPlan`] block is admitted by the
//! coordinator itself (`crate::eval::admission`): miners commit on chain,
//! upload privately, and the roster freezes at the cutoff from finalized
//! commitments, so the stored plan gains its entrants only then. A plan
//! without the block keeps the testnet operator-roster shortcut
//! (`testnet_operator_roster`): the operator declares entrants from artifacts
//! already in the eval bucket, for rehearsals.
//!
//! Schema v2 (`slop_ninja_eval_cohort_plan_v2`) adds the fan-out parameters
//! ([`Fanout`]): lane widths, leases, lane caps, the per-cohort scoring budget,
//! the just-in-time pre-start lead and the cost model the budget is checked
//! with. They are plan parameters, recorded with the plan's digest, not
//! compiled-in constants; the deployment's own ceilings (`MAX_SCORE_LANES`,
//! which equals the Container `max_instances`, and the roster cap) apply on top.
//! Stored v1 plans (closed cohorts) still deserialize, with no fan-out block.
//!
//! The load-rehearsal plan (`slop_ninja_eval_load_plan_v1`, [`LoadPlan`]) is
//! accepted only by the separate load instance: it re-scores an existing sealed
//! batch against synthetic identities, never writes a calculation target, the
//! pool's used set or anything validators read, and may enable fault hooks.

use serde::{Deserialize, Serialize};
use slop_ninja_protocol::{AccountId, ArtifactRecord, Digest, NeuronIdentity, Record};

pub const PLAN_SCHEMA_V1: &str = "slop_ninja_eval_cohort_plan_v1";
/// `testnet-reward-v0` plans: stored cohorts still parse; new ones are refused.
pub const PLAN_SCHEMA: &str = "slop_ninja_eval_cohort_plan_v2";
/// `reward-v1` cohort plans ([`RewardV1Plan`]).
pub const PLAN_SCHEMA_V3: &str = "slop_ninja_eval_cohort_plan_v3";
/// A fingerprint probe build: draws and generates a probe version like a
/// batch, scores the references on it and records their fingerprints.
pub const PROBE_PLAN_SCHEMA: &str = "slop_ninja_eval_probe_plan_v1";
pub const LOAD_PLAN_SCHEMA: &str = "slop_ninja_eval_load_plan_v1";
/// The smallest batch a plan may name. The largest is not a constant: every
/// receipt a cohort can record (each scored document of each job, at each
/// attempt) must fit one signed receipt index ([`receipt_capacity`]).
pub const MIN_CASES: u32 = 10;

/// The most receipts one cohort may record: the entries one signed receipt
/// index holds (`target_bundle::receipt_index_capacity`, the tighter of the
/// codec's sequence bound and the signed-record limit). A native validator's
/// fetch session holds two indexes of that size, a target's cohort's and its
/// carrier's (`target_bundle::validator_receipt_index_bytes`), so this is
/// also what every validator can fetch.
pub fn receipt_index_bound() -> u64 {
    slop_ninja_protocol::target_bundle::receipt_index_capacity().unwrap_or(0)
}

/// Whether `jobs_docs` scored documents at up to `max_attempts` attempts fit
/// one signed receipt index ([`receipt_index_bound`]).
pub fn receipt_capacity(jobs_docs: u64, max_attempts: u8) -> bool {
    jobs_docs.saturating_mul(u64::from(max_attempts)) <= receipt_index_bound()
}
/// Ceiling on the declared per-kind shortfall tolerance (5 %).
pub const MAX_KIND_SHORTFALL_PERMILLE: u16 = 50;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityJson {
    pub uid: u16,
    /// `0x` + 64 lowercase hex: the 32-byte hotkey account.
    pub hotkey: String,
    pub registration_block: u64,
}

impl IdentityJson {
    pub fn identity(&self) -> Result<NeuronIdentity, PlanError> {
        Ok(NeuronIdentity {
            uid: self.uid,
            hotkey: AccountId(parse32(&self.hotkey).ok_or(PlanError("identity hotkey"))?),
            registration_block: self.registration_block,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactJson {
    pub owner: IdentityJson,
    pub contract: String,
    /// SHA-256 of the artifact files manifest bytes (lowercase hex, no prefix).
    pub package_sha256: String,
    pub model_sha256: String,
    /// OCI digest of the scorer image (lowercase hex, no prefix).
    pub image_digest: String,
    pub commitment_block: u64,
    /// Eval-bucket key of the artifact files manifest.
    pub manifest_key: String,
    /// Admitted entrants only (set at the roster freeze): the opening's secret
    /// nonce (hex), published in the closure. Operator-roster entrants derive
    /// a private nonce instead (`v3::opening_nonce`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening_nonce: Option<String>,
    /// Admitted entrants only: the index of the extrinsic that emitted the
    /// commitment in `commitment_block`. Operator-roster entrants use their
    /// roster position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extrinsic_index: Option<u32>,
    /// Admitted entrants only: the key of the uploaded artifact object in
    /// the **upload bucket** (`admission-bucket-testnet`, where the
    /// upload Worker stores parts since the two-Worker split; the eval
    /// bucket holds only its content-addressed blobs), published after the
    /// cohort closes. A publisher copy of the object reads it from the
    /// upload bucket (`TESTNET.md`, upload Worker, open items).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_key: Option<String>,
}

impl ArtifactJson {
    pub fn record(&self) -> Result<ArtifactRecord, PlanError> {
        let d = |s: &str, what: &'static str| parse_hex32(s).map(Digest).ok_or(PlanError(what));
        let record = ArtifactRecord {
            owner: self.owner.identity()?,
            contract: self.contract.clone(),
            package_sha256: d(&self.package_sha256, "package_sha256")?,
            model_sha256: d(&self.model_sha256, "model_sha256")?,
            image_digest: d(&self.image_digest, "image_digest")?,
            commitment_block: self.commitment_block,
        };
        record.validate().map_err(|_| PlanError("artifact record"))?;
        Ok(record)
    }

    pub fn id(&self) -> Result<Digest, PlanError> {
        self.record()?.digest().map_err(|_| PlanError("artifact digest"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolRef {
    pub pool_id: String,
    pub manifest_key: String,
    pub manifest_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mix {
    pub human_only: u32,
    pub mixed: u32,
    pub model_only: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scorer {
    /// Documents per scorer request (one attempt per document per request).
    pub chunk_docs: u32,
    /// v1 only (the two-slot engine); absent in v2, where lanes replace slots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_jobs: Option<u32>,
}

/// The fan-out parameters of a v2 plan. Every bulk step runs as lanes: one
/// Durable Object invocation per lane, scoring lanes each driving their own
/// Container instance, all claiming through the cohort's leases.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fanout {
    pub score: ScoreLanes,
    pub generation: GenLanes,
    pub package: PackageLanes,
    pub cost: CostModel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoreLanes {
    /// Documents per lane of one job: W = ceil(n / lane_docs) lanes per job, at
    /// least one, at most the job's chunk count. `lane_docs = chunk_docs` is the
    /// wide width (lanes = miners x chunks).
    pub lane_docs: u32,
    /// Plan cap on live scoring lanes (the deployment's `MAX_SCORE_LANES`, equal
    /// to the Container `max_instances`, and the budget also bound it).
    pub max_lanes: u32,
    /// Chunk claim lease. The cold path holds no claim, so it covers one call.
    pub lease_ms: u64,
    /// Lease of a crash-isolation claim (single-document requests on a fresh
    /// instance, with restarts after each crash).
    pub isolation_lease_ms: u64,
    /// Per-cohort ceiling on scoring cost (Containers and lane Durable Objects).
    pub scoring_budget_microusd: u64,
    /// Just-in-time pre-start: kick scoring lanes when the generation tail is
    /// projected to end within this many ms (0: off). Nothing runs at rest.
    pub prestart_lead_ms: u64,
    /// A pre-started lane stops its instance if scoring has not begun this long
    /// after its artifact loaded.
    pub prestart_wait_ms: u64,
    /// `setInactivityTimeout` on each lane's container: how long it survives
    /// between two alarm invocations of its lane.
    pub inactivity_ms: u64,
    /// Consecutive lane start/load failures (no commit in between) that pause
    /// the cohort (`scorer_unavailable`).
    pub failures_before_pause: u32,
    /// The rest are required in new plans ([`ScoreLanes::limits`]) and absent
    /// only from plans stored before 2026-10-31.
    ///
    /// A lane invocation's work budget: a lane starts no step (a chunk, a
    /// restart, a single-document request) that could end past it, then re-arms.
    /// With one lease it fits the platform's 15-minute alarm wall.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_budget_ms: Option<u64>,
    /// The declared per-document time limit of crash isolation: a document
    /// scored alone that outlasts it has its container destroyed, which counts
    /// as a crash (a limit violation) and is attributed only when reproduced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_timeout_ms: Option<u64>,
    /// The declared bound on an artifact's total file size, in MiB (plans
    /// written before `artifact_spec`). A larger artifact manifest is an
    /// artifact failure that needs no instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_max_mib: Option<u32>,
    /// The artifact spec (id, as `slop_ninja_artifact_check spec` prints it)
    /// of the cohort. Its bounds size the artifact: the largest file total
    /// any artifact it admits can have (`admission::SpecBounds::files_bytes`).
    /// Replaces `artifact_max_mib`; a plan names exactly one of the two. Every
    /// scorer load names it by digest (`spec=<digest>` on `/v1/load`,
    /// [`CohortPlan::load_spec_sha256`]), so the scorer admits each artifact
    /// by it; spec v3 admits the span tower class. Absent: loads name no spec
    /// and are admitted by the scorer's unnamed-load spec (spec v2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_spec: Option<String>,
    /// The digest of `artifact_spec`, frozen with the plan: the digest every
    /// scoring load names. A plan that names `artifact_spec` names its digest
    /// here or in its admission block (`admission.artifact_spec_sha256`);
    /// each one named must be this build's digest of that spec, so a cohort
    /// never loads by a spec record other than the one it was queued with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_spec_sha256: Option<String>,
    /// The load allowance per MiB of that bound: the artifact stream and the
    /// model load each get `bound in MiB (rounded up) x load_ms_per_mib`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_ms_per_mib: Option<u32>,
    /// Attributable artifact failures on fresh instances (a load that fails or
    /// outlasts its allowance, or a control document that kills a freshly
    /// started instance), with no prediction of that job committed in between,
    /// after which an entrant's unresolved documents are model errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_failures_before_model_error: Option<u32>,
}

/// The platform's wall-time limit of one Durable Object alarm invocation
/// (LIMITS.md §3: "15 minutes"); a lane's step budget plus one lease fits it.
pub const ALARM_WALL_MS: u64 = 900_000;

/// A scoring lane's resolved limits (see the [`ScoreLanes`] fields).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneLimits {
    pub step_budget_ms: u64,
    pub doc_timeout_ms: u64,
    pub artifact_max_bytes: u64,
    /// The stream allowance and the model-load allowance, each.
    pub load_wait_ms: u64,
    pub artifact_failures_before_model_error: u32,
}

impl ScoreLanes {
    /// The lane limits. Plans stored before these fields existed get the
    /// values that versions up to `da569b03` compiled in (600 s step, a
    /// single-document bound of the lease less 15 s, a 180 s load wait), with
    /// no size bound and no artifact attribution; new plans must declare them.
    pub fn limits(&self) -> LaneLimits {
        let max_bytes = self.artifact_max_bytes();
        let load_wait_ms = match (max_bytes, self.load_ms_per_mib) {
            (Some(b), Some(r)) => b.div_ceil(1 << 20) * u64::from(r),
            _ => 180_000,
        };
        LaneLimits {
            step_budget_ms: self.step_budget_ms.unwrap_or(600_000),
            doc_timeout_ms: self.doc_timeout_ms.unwrap_or(self.isolation_lease_ms.saturating_sub(15_000).max(5_000)),
            artifact_max_bytes: max_bytes.unwrap_or(u64::MAX),
            load_wait_ms,
            artifact_failures_before_model_error: self.artifact_failures_before_model_error.unwrap_or(u32::MAX),
        }
    }

    /// The bound on an artifact's total file size: derived from the named
    /// artifact spec, or the declared MiB of plans written before it. The
    /// bound comes from the spec's class and JSON bounds alone, so a lane
    /// report never rebuilds the spec record (`admission::files_bytes`).
    pub fn artifact_max_bytes(&self) -> Option<u64> {
        match (&self.artifact_spec, self.artifact_max_mib) {
            (Some(id), _) => {
                slop_ninja_artifact_check::spec::Version::from_id(id).and_then(crate::eval::admission::files_bytes)
            }
            (None, Some(m)) => Some(u64::from(m) << 20),
            (None, None) => None,
        }
    }
}

/// This build's digest of artifact spec `id`, when the scorer admits loads
/// by it (`admission::SpecBounds`: spec v2 and v3, the scorer's
/// `engine::admits_by`). It rebuilds the spec record, so it runs at plan
/// validation and for the operator probe only, never per lane report.
pub fn load_spec_sha256_of(id: &str) -> Option<String> {
    crate::eval::admission::SpecBounds::of_id(id).map(|b| b.spec_sha256)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenLanes {
    /// Plan cap on generation lanes; G = min(max_lanes, ceil(pending / items_per_lane)).
    pub max_lanes: u32,
    pub items_per_lane: u32,
    /// Workers AI calls in flight per lane (at most 6: the per-invocation
    /// connection limit, should AI calls count toward it).
    pub inflight_per_lane: u32,
    /// Generation claim lease; a lane's call timeout is the lease less 15 s.
    pub lease_ms: u64,
    /// Accumulated throttling per item that consumes no attempt; past it a
    /// throttle counts as an outage.
    pub max_throttle_ms: u64,
    /// Consecutive outages of one item that pause the cohort
    /// (`generation_unavailable`, cleared by the first outage probe the
    /// provider answers). Required in a newly queued cohort plan
    /// (`engine::admit`); a cohort the calendar opens from a template without
    /// it carries the compiled bound written into its plan. Only plans stored
    /// before it was required lack it, and keep the bound those builds
    /// compiled in ([`GenLanes::failures_before_pause`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failures_before_pause: Option<u32>,
}

/// The generation pause bound of plans that do not declare one: what builds
/// before 2026-11-04 compiled in (with the backoff, about 10 minutes).
pub const GEN_FAILURES_BEFORE_PAUSE_COMPILED: u32 = 10;

impl GenLanes {
    /// The plan's generation pause bound (`failures_before_pause`).
    pub fn failures_before_pause(&self) -> u32 {
        self.failures_before_pause.unwrap_or(GEN_FAILURES_BEFORE_PAUSE_COMPILED)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageLanes {
    /// Live summary and bundle lanes at once.
    pub max_lanes: u32,
}

/// Rates (nano-dollars) the scoring budget is checked against: the Container
/// instance type's provisioned and per-vCPU rates, the lane Durable Object's
/// duration rate, and the per-document and per-cold-path upper bounds used
/// before anything is measured.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostModel {
    pub instance_nanousd_per_s: u64,
    pub vcpu_nanousd_per_s: u64,
    pub vcpus: u32,
    pub lane_do_nanousd_per_s: u64,
    pub doc_ub_nanousd: u64,
    pub cold_ub_nanousd: u64,
}

/// A load plan's source batch: an existing sealed batch, re-scored through its
/// text-free index; lanes read the texts from its private shards.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadSource {
    pub label: String,
    pub index_key: String,
    pub index_sha256: String,
    /// The source batch's task count (checked against its index at seal).
    pub tasks: u32,
}

/// Load-mode fault hooks (testnet load instance only).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadFaults {
    /// Lanes (by index) that destroy their own container mid-request once.
    #[serde(default)]
    pub kill_lanes: Vec<u32>,
    /// Lanes that stop after storing their first chunk's objects, before the
    /// commit, once (the next claim reconciles with no compute).
    #[serde(default)]
    pub crash_after_put: Vec<u32>,
    /// Lanes whose alarm throws once right after a claim.
    #[serde(default)]
    pub throw_after_claim: Vec<u32>,
    /// Kicks the cohort drops (per mille) instead of sending; the sweep re-sends.
    #[serde(default)]
    pub drop_kick_permille: u16,
    /// Every kick is sent twice.
    #[serde(default)]
    pub duplicate_kicks: bool,
    /// A document that crashes its container whenever (job, task) is in a
    /// request, alone or not: a reproducible crash, attributed to that job only.
    #[serde(default)]
    pub crash_task: Option<(u32, u32)>,
    /// More such documents, (job, task) each (several in one chunk make its
    /// isolation outlast one lane step).
    #[serde(default)]
    pub crash_tasks: Vec<(u32, u32)>,
    /// Jobs whose every request, the control document included, kills the
    /// container (an artifact that crashes on any input).
    #[serde(default)]
    pub crash_jobs: Vec<u32>,
    /// (job, task) whose first single-document request on each lane kills the
    /// container once, then scores normally: a one-off failure in isolation.
    #[serde(default)]
    pub flaky_task: Option<(u32, u32)>,
    /// Jobs whose artifact load reports failure on every fresh instance.
    #[serde(default)]
    pub load_fail_jobs: Vec<u32>,
    /// Every batch-shard read fails this long after scoring starts (an R2
    /// read outage before any document is dispatched).
    #[serde(default)]
    pub r2_outage_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadSpec {
    pub source: LoadSource,
    #[serde(default)]
    pub faults: LoadFaults,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortPlan {
    pub schema: String,
    pub cohort: u64,
    pub label: String,
    pub mechanism: u8,
    /// Cases per model.
    pub n: u32,
    pub mix: Mix,
    /// Minimum cohort length in tempos (epoch-index increments).
    pub k_tempos: u64,
    pub admission_tempos: u64,
    pub transition_tempos: u64,
    pub max_attempts: u8,
    pub pool: PoolRef,
    /// Workers AI text-generation model ids.
    pub generators: Vec<String>,
    pub generation_budget_microusd: u64,
    pub reward_rule: String,
    pub burn_identity: IdentityJson,
    pub allow_list: Vec<IdentityJson>,
    pub reference: ArtifactJson,
    pub previous_reference: Option<ArtifactJson>,
    pub entrants: Vec<ArtifactJson>,
    pub scorer: Scorer,
    /// Declared tolerance, per kind, for cases the draw or generation could not
    /// supply (`kind_shortfall_permille_v1`): kind k may seal with at most
    /// floor(count_k * permille / 1000) fewer cases than the plan's count. Absent
    /// means 0: the batch must match the plan exactly. A larger shortfall pauses
    /// the cohort before anything is exposed or sealed.
    #[serde(default)]
    pub max_kind_shortfall_permille: u16,
    /// v2: the fan-out parameters (absent only in stored v1 plans).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fanout: Option<Fanout>,
    /// Load instance only: the re-scored source batch and fault hooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<LoadSpec>,
    /// v3 (`reward-v1`) cohorts and probe builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward_v1: Option<RewardV1Plan>,
    /// v3 cohorts admitted by the coordinator (absent: the operator roster).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission: Option<AdmissionPlan>,
}

/// The `reward-v1` block of a v3 plan. Every value is a plan parameter,
/// recorded with the plan's digest and bound into the signed manifest (the
/// gate policy and limits by digest); none is compiled in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewardV1Plan {
    /// Pooled paired standard error of `q` at this batch size, in headroom
    /// units scaled by 10^18, measured before the cohort.
    pub sigma: u64,
    /// Where `sigma` was measured (a short content-free description).
    pub sigma_basis: String,
    /// The reward-intent lease, at least two cohort lengths.
    pub lease_tempos: u32,
    /// Rewrite-source reuse: a pure-human passage may parent at most
    /// `max_uses` mixed children, uses at least `min_gap` cohorts apart.
    pub reuse: Reuse,
    /// Fingerprint probe versions in use, in version order (empty in a probe build).
    #[serde(default)]
    pub probes: Vec<ProbeRef>,
    pub gates: GatePolicy,
    pub canary: CanaryPolicy,
    /// Extra mixed and model-only items generated (per mille of each kind's
    /// count) so that rejected generations do not short the exact mix; the
    /// seal keeps exactly the planned count of each kind by private rank.
    pub spares_permille: u16,
    /// The frozen output contract identifier the manifest names.
    pub output_contract: String,
    /// Probe build only: the probe version this plan creates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_version: Option<u32>,
    /// The incentive rule the cohort runs under and whether its gate
    /// thresholds are provisional; bound into the signed manifest with the
    /// gate policy (`v3::gate_policy_json`). Absent in plans written before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incentive: Option<Incentive>,
}

/// The incentive rule of a cohort (user decision D11 (a), testnet).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Incentive {
    /// [`INCENTIVE_REFERENCE_MARGIN`]: the paper rule. Every unit burns to the
    /// owner hotkey's UID until an entrant beats `R_k` by delta and its
    /// cohort's target activates; then the target's clusters are paid.
    pub rule: String,
    /// The gate thresholds are provisional testnet values, re-fixed from
    /// development evidence before live rewards.
    pub provisional_gates: bool,
}

/// The reward-v1 paper rule: burn until an entrant beats `R_k` by delta.
pub const INCENTIVE_REFERENCE_MARGIN: &str = "reward_v1_reference_margin/v1";

/// Coordinator admission of a v3 cohort's roster (`crate::eval::admission`,
/// contract `slop_ninja.admission/v1`). Every value is a versioned plan
/// parameter frozen with the plan; the object bound is derived from the
/// named artifact spec, never declared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionPlan {
    /// The artifact spec admission and the scorer's admission load run, by id
    /// and by the digest `slop_ninja_artifact_check spec` prints.
    pub artifact_spec: String,
    pub artifact_spec_sha256: String,
    /// Entrants admitted at the freeze (`ROSTER_CAP`); the deployment's cap
    /// and the package step's memory model bound it.
    pub roster_cap: u16,
    /// The selection rule (`earliest_commitment/v1`).
    pub selection: String,
    /// Upload part size in bytes: at least R2's multipart minimum and at most
    /// a Worker request body.
    pub part_size: u64,
    /// The most parts one object may take (R2 allows 10,000).
    pub max_parts: u32,
    /// How far past the finalized cursor a request's `valid_until` may reach.
    pub request_validity_blocks: u64,
    /// The operator's own hotkeys (`0x` + 64 hex). Their admitted entrants are
    /// never outside entrants, so they never unlock fresh stock.
    #[serde(default)]
    pub operator_hotkeys: Vec<String>,
    /// The operator's own coldkeys (`0x` + 64 hex). An admitted entrant
    /// whose hotkey a listed coldkey owned at the cutoff (`Owner`, read in
    /// the cutoff snapshot) is never an outside entrant, whatever hotkey it
    /// registered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operator_coldkeys: Vec<String>,
    /// Fresh testnet human stock, drawn instead of `pool` only when the frozen
    /// roster holds an admitted outside entrant (its artifact passed the
    /// scorer's admission load). Absent: the cohort always draws `pool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_pool: Option<PoolRef>,
}

/// R2's multipart part minimum (every part but the last).
pub const R2_MIN_PART_BYTES: u64 = 5 << 20;
/// R2's multipart part-count maximum.
pub const R2_MAX_PARTS: u32 = 10_000;
/// The Workers request-body limit of the Free and Paid plans ("100 MB"): one
/// part is one Worker request.
pub const WORKER_REQUEST_BODY_BYTES: u64 = 100_000_000;

impl AdmissionPlan {
    pub fn validate(&self) -> Result<(), PlanError> {
        let e = |c: bool, what: &'static str| if c { Ok(()) } else { Err(PlanError(what)) };
        let bounds = crate::eval::admission::SpecBounds::of_id(&self.artifact_spec)
            .ok_or(PlanError("admission.artifact_spec (a spec the scorer admits by)"))?;
        e(bounds.spec_sha256 == self.artifact_spec_sha256, "admission.artifact_spec_sha256 (the spec's digest)")?;
        // Every admission run names the cohort's spec digest on its load
        // (the scorer lane's admission route): a spec outside the list
        // could never reach a verdict, so it is refused here.
        e(
            crate::eval::admission::ADMISSION_SPECS.contains(&self.artifact_spec.as_str()),
            "admission_spec_unsupported: admission runs load by the specs in admission::ADMISSION_SPECS",
        )?;
        e(self.roster_cap >= 1 && usize::from(self.roster_cap) <= usize::from(slop_ninja_protocol::MAX_ROSTER), "admission.roster_cap")?;
        e(self.selection == slop_ninja_admission_wire::SELECTION_EARLIEST_COMMITMENT_V1, "admission.selection")?;
        e(
            (R2_MIN_PART_BYTES..=WORKER_REQUEST_BODY_BYTES).contains(&self.part_size),
            "admission.part_size (R2's part minimum to a Worker request body)",
        )?;
        e((1..=R2_MAX_PARTS).contains(&self.max_parts), "admission.max_parts (1-10,000)")?;
        e(
            bounds.object_bytes.div_ceil(self.part_size) <= u64::from(self.max_parts),
            "admission.max_parts (the spec's largest object fits)",
        )?;
        e(self.request_validity_blocks >= 1, "admission.request_validity_blocks")?;
        let mut hk = Vec::new();
        for h in &self.operator_hotkeys {
            hk.push(parse32(h).ok_or(PlanError("admission.operator_hotkeys"))?);
        }
        hk.sort();
        e(hk.windows(2).all(|w| w[0] != w[1]), "admission.operator_hotkeys duplicate")?;
        let mut ck = Vec::new();
        for c in &self.operator_coldkeys {
            ck.push(parse32(c).ok_or(PlanError("admission.operator_coldkeys"))?);
        }
        ck.sort();
        e(ck.windows(2).all(|w| w[0] != w[1]), "admission.operator_coldkeys duplicate")?;
        if let Some(p) = &self.fresh_pool {
            e(key_ok(&p.manifest_key) && parse_hex32(&p.manifest_sha256).is_some() && !p.pool_id.is_empty(), "admission.fresh_pool")?;
            // The fresh-stock gate fails closed: a plan that can unlock fresh
            // stock names the operator's own hotkeys and coldkeys, which
            // never unlock it.
            e(!self.operator_hotkeys.is_empty(), "admission.fresh_pool needs admission.operator_hotkeys")?;
            e(!self.operator_coldkeys.is_empty(), "admission.fresh_pool needs admission.operator_coldkeys")?;
        }
        Ok(())
    }

    /// Whether `hotkey` is one of the operator's own.
    pub fn is_operator(&self, hotkey: &[u8; 32]) -> bool {
        self.operator_hotkeys.iter().any(|h| parse32(h).as_ref() == Some(hotkey))
    }

    /// Whether `coldkey` is one of the operator's own.
    pub fn is_operator_coldkey(&self, coldkey: &[u8; 32]) -> bool {
        self.operator_coldkeys.iter().any(|c| parse32(c).as_ref() == Some(coldkey))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reuse {
    pub max_uses: u32,
    pub min_gap: u64,
}

/// A probe version in use: its private record in the eval bucket.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeRef {
    pub version: u32,
    /// Documents in the version (bounds the scoring budget before any read).
    pub size: u32,
    pub key: String,
    pub sha256: String,
}

/// Gate thresholds (testnet provisional values are fixed in each plan).
/// Probabilities are in parts per billion; rates in per mille.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatePolicy {
    /// A human-only document is a false positive when its involvement
    /// probability is at or above this operating point, or it failed.
    pub operating_ppb: u32,
    /// Loose per-entrant tier, on the entrant's own cohort humans.
    pub loose_fpr_max_permille: u32,
    /// Strict promotion tier, on the first canary audit draw.
    pub strict_fpr_max_permille: u32,
    /// Share gate: mean absolute share error on mixed documents.
    pub share_mae_max_ppb: u32,
    /// Attribution gate: minimum share of predictions carrying both role results.
    pub attribution_min_permille: u32,
}

/// Lookup canary policy. Testnet has no anchors, so no canary statistic is
/// computed; flags come only from the drill list (`canary.drill`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanaryPolicy {
    /// Fresh human documents in each audit draw (A, and B for a re-audit).
    pub draw_humans: u32,
    #[serde(default)]
    pub drill: Vec<CanaryDrill>,
}

/// A testnet drill: the candidate owned by `hotkey` is flagged on its first
/// audit (`flags` 1), or on its re-audit as well (`flags` 2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanaryDrill {
    pub hotkey: String,
    pub flags: u8,
}

impl RewardV1Plan {
    fn validate(&self, k_tempos: u64, probe_build: bool) -> Result<(), PlanError> {
        let e = |c: bool, what: &'static str| if c { Ok(()) } else { Err(PlanError(what)) };
        e(self.sigma > 0 && self.sigma <= slop_ninja_protocol::SCORE_SCALE, "reward_v1.sigma")?;
        e(!self.sigma_basis.is_empty() && self.sigma_basis.len() <= 200, "reward_v1.sigma_basis")?;
        e(u64::from(self.lease_tempos) >= 2 * k_tempos, "reward_v1.lease_tempos (at least 2K)")?;
        e(self.reuse.max_uses >= 1 && self.reuse.min_gap >= 1, "reward_v1.reuse")?;
        let g = &self.gates;
        e(
            g.operating_ppb > 0
                && g.operating_ppb <= 1_000_000_000
                && g.loose_fpr_max_permille <= 1000
                && g.strict_fpr_max_permille <= 1000
                && g.share_mae_max_ppb <= 1_000_000_000
                && g.attribution_min_permille <= 1000,
            "reward_v1.gates",
        )?;
        e(self.spares_permille <= 1000, "reward_v1.spares_permille")?;
        e(!self.output_contract.is_empty() && self.output_contract.len() <= 128, "reward_v1.output_contract")?;
        if let Some(i) = &self.incentive {
            e(i.rule == INCENTIVE_REFERENCE_MARGIN, "reward_v1.incentive.rule")?;
        }
        for d in &self.canary.drill {
            e(parse32(&d.hotkey).is_some() && (1..=2).contains(&d.flags), "reward_v1.canary.drill")?;
        }
        if probe_build {
            e(self.probe_version.is_some_and(|v| v >= 1), "reward_v1.probe_version")?;
            e(self.probes.is_empty(), "reward_v1.probes (none in a probe build)")?;
        } else {
            e(self.probe_version.is_none(), "reward_v1.probe_version (probe builds only)")?;
            e(!self.probes.is_empty(), "reward_v1.probes (at least one version in use)")?;
            let mut last = 0u32;
            for p in &self.probes {
                e(
                    p.version > last
                        && (1..=slop_ninja_protocol::MAX_PROBE_SIZE).contains(&p.size)
                        && key_ok(&p.key)
                        && parse_hex32(&p.sha256).is_some(),
                    "reward_v1.probes",
                )?;
                last = p.version;
            }
        }
        Ok(())
    }

    /// The drill flags for an owner hotkey, if any.
    pub fn drill_flags(&self, hotkey: &[u8; 32]) -> u8 {
        self.canary
            .drill
            .iter()
            .find(|d| parse32(&d.hotkey).as_ref() == Some(hotkey))
            .map_or(0, |d| d.flags)
    }
}

/// The load-rehearsal plan as submitted to the load instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadPlan {
    pub schema: String,
    pub cohort: u64,
    /// Starts with `load-`, so its objects sit under `cohorts/load-.../`.
    pub label: String,
    pub source: LoadSource,
    pub max_attempts: u8,
    pub burn_identity: IdentityJson,
    pub allow_list: Vec<IdentityJson>,
    pub reference: ArtifactJson,
    pub entrants: Vec<ArtifactJson>,
    pub scorer: Scorer,
    pub fanout: Fanout,
    #[serde(default)]
    pub faults: LoadFaults,
}

/// Deployment ceilings applied to a plan on top of its own parameters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ceilings {
    /// Roster cap (entrants); `None` means none (mainnet).
    pub roster_cap: Option<usize>,
    /// Whether new `testnet-reward-v0` (v2) cohort plans are accepted. The
    /// deployed main instance refuses them (`reward-v1` only); the engine's
    /// plumbing tests still drive v2 plans.
    pub accept_reward_v0: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanError(pub &'static str);

pub fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    hex::decode(s).ok()?.try_into().ok()
}

pub fn parse32(s: &str) -> Option<[u8; 32]> {
    parse_hex32(s.strip_prefix("0x")?)
}

fn key_ok(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 256
        && !k.starts_with('/')
        && !k.split('/').any(|c| c.is_empty() || c == "." || c == "..")
        && k.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))
}

/// Counts per kind for `n` cases under `mix` (largest remainder, ties by kind order).
pub fn kind_counts(n: u32, mix: &Mix) -> [u32; 3] {
    let w = [mix.human_only, mix.mixed, mix.model_only];
    let total: u32 = w.iter().sum();
    let mut out = [0u32; 3];
    let mut rem = [(0u64, 0usize); 3];
    for i in 0..3 {
        let x = u64::from(n) * u64::from(w[i]);
        out[i] = (x / u64::from(total)) as u32;
        rem[i] = (x % u64::from(total), i);
    }
    let mut left = n - out.iter().sum::<u32>();
    rem.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (_, i) in rem {
        if left == 0 {
            break;
        }
        out[i] += 1;
        left -= 1;
    }
    out
}

impl Fanout {
    pub fn validate(&self, chunk_docs: u32, load: bool) -> Result<(), PlanError> {
        let e = |c: bool, what: &'static str| if c { Ok(()) } else { Err(PlanError(what)) };
        let s = &self.score;
        e(s.lane_docs >= chunk_docs, "fanout.score.lane_docs (at least chunk_docs)")?;
        e(s.max_lanes >= 1, "fanout.score.max_lanes")?;
        // Load plans may shorten the lease below a chunk call (the short-lease drill).
        let lease_min = if load { 1_000 } else { 30_000 };
        e((lease_min..=600_000).contains(&s.lease_ms), "fanout.score.lease_ms")?;
        e(s.isolation_lease_ms >= s.lease_ms && s.isolation_lease_ms <= 900_000, "fanout.score.isolation_lease_ms")?;
        e(s.scoring_budget_microusd > 0, "fanout.score.scoring_budget_microusd")?;
        e(s.prestart_lead_ms <= 600_000 && s.prestart_wait_ms <= 600_000, "fanout.score.prestart")?;
        e((10_000..=600_000).contains(&s.inactivity_ms), "fanout.score.inactivity_ms")?;
        e(s.failures_before_pause >= 1, "fanout.score.failures_before_pause")?;
        e(self.generation.failures_before_pause != Some(0), "fanout.generation.failures_before_pause")?;
        let (Some(step), Some(doc), Some(per_mib), Some(attrib)) =
            (s.step_budget_ms, s.doc_timeout_ms, s.load_ms_per_mib, s.artifact_failures_before_model_error)
        else {
            return Err(PlanError(
                "fanout.score needs step_budget_ms, doc_timeout_ms, artifact_spec (or artifact_max_mib), \
                 load_ms_per_mib and artifact_failures_before_model_error",
            ));
        };
        e(
            s.artifact_spec.is_some() != s.artifact_max_mib.is_some(),
            "fanout.score names exactly one of artifact_spec and artifact_max_mib",
        )?;
        let max_bytes = s.artifact_max_bytes().ok_or(PlanError("fanout.score.artifact_spec (a spec the scorer admits by)"))?;
        let mib = max_bytes.div_ceil(1 << 20);
        // Lanes start no step past the budget, and a step is at most one lease
        // (a chunk, a generation call or an isolation request).
        let longest = s.lease_ms.max(self.generation.lease_ms).max(doc);
        e(step >= 60_000 && step + longest <= ALARM_WALL_MS, "fanout.score.step_budget_ms (plus one lease within the alarm wall)")?;
        e(doc >= 1_000 && doc + 15_000 <= s.isolation_lease_ms, "fanout.score.doc_timeout_ms (within the isolation lease less 15 s)")?;
        e(mib >= 1 && per_mib >= 1, "fanout.score.artifact bound, load_ms_per_mib")?;
        // One document's proof and reproduction (two restarts, each a stream and
        // a load, and three single-document requests) fits one step, apart from
        // the deployment's start wait; otherwise isolation could never progress.
        let load_wait = mib * u64::from(per_mib);
        e(4 * load_wait + 3 * doc <= step, "fanout.score.step_budget_ms (one isolation proof per step)")?;
        e(attrib >= 1, "fanout.score.artifact_failures_before_model_error")?;
        let g = &self.generation;
        e(g.max_lanes >= 1 && g.items_per_lane >= 1, "fanout.generation lanes")?;
        e((1..=6).contains(&g.inflight_per_lane), "fanout.generation.inflight_per_lane (1-6)")?;
        e((30_000..=600_000).contains(&g.lease_ms), "fanout.generation.lease_ms")?;
        e(g.max_throttle_ms <= 3_600_000, "fanout.generation.max_throttle_ms")?;
        e(self.package.max_lanes >= 1, "fanout.package.max_lanes")?;
        let c = &self.cost;
        e(
            c.instance_nanousd_per_s > 0 && c.vcpu_nanousd_per_s > 0 && c.vcpus >= 1 && c.lane_do_nanousd_per_s > 0
                && c.doc_ub_nanousd > 0 && c.cold_ub_nanousd > 0,
            "fanout.cost",
        )?;
        Ok(())
    }
}

impl LoadPlan {
    pub fn parse(bytes: &[u8], ceilings: Ceilings) -> Result<Self, PlanError> {
        let plan: Self = serde_json::from_slice(bytes).map_err(|_| PlanError("load plan json"))?;
        let e = |c: bool, what: &'static str| if c { Ok(()) } else { Err(PlanError(what)) };
        e(plan.schema == LOAD_PLAN_SCHEMA, "schema")?;
        e(plan.label.starts_with("load-"), "load label (load- prefix)")?;
        e(
            key_ok(&plan.source.index_key)
                && parse_hex32(&plan.source.index_sha256).is_some()
                && plan.source.tasks >= MIN_CASES
                && receipt_capacity(
                    u64::from(plan.source.tasks) * (1 + plan.entrants.len() as u64),
                    plan.max_attempts,
                ),
            "load source",
        )?;
        let c = plan.cohort_plan();
        c.validate_common(ceilings, true)?;
        Ok(plan)
    }

    /// The stored form: a cohort plan with the load block, no pool, no
    /// generators and no tempo minimums (load cohorts never wait on boundaries).
    pub fn cohort_plan(&self) -> CohortPlan {
        CohortPlan {
            schema: LOAD_PLAN_SCHEMA.into(),
            cohort: self.cohort,
            label: self.label.clone(),
            mechanism: 0,
            n: self.source.tasks,
            mix: Mix { human_only: 40, mixed: 40, model_only: 20 },
            k_tempos: 0,
            admission_tempos: 0,
            transition_tempos: 0,
            max_attempts: self.max_attempts,
            pool: PoolRef { pool_id: String::new(), manifest_key: String::new(), manifest_sha256: String::new() },
            generators: Vec::new(),
            generation_budget_microusd: 0,
            reward_rule: "testnet-reward-v0".into(),
            burn_identity: self.burn_identity.clone(),
            allow_list: self.allow_list.clone(),
            reference: self.reference.clone(),
            previous_reference: None,
            entrants: self.entrants.clone(),
            scorer: self.scorer.clone(),
            max_kind_shortfall_permille: 0,
            fanout: Some(self.fanout.clone()),
            load: Some(LoadSpec { source: self.source.clone(), faults: self.faults.clone() }),
            reward_v1: None,
            admission: None,
        }
    }
}

impl CohortPlan {
    pub fn parse(bytes: &[u8], ceilings: Ceilings) -> Result<Self, PlanError> {
        let plan: Self = serde_json::from_slice(bytes).map_err(|_| PlanError("plan json"))?;
        plan.validate(ceilings)?;
        Ok(plan)
    }

    /// The artifact spec digest every scoring load names, as the plan froze
    /// it (`fanout.score.artifact_spec_sha256`, else the admission block's);
    /// `None` when the plan names no spec (loads admitted by spec v2).
    pub fn load_spec_sha256(&self) -> Option<&str> {
        let s = &self.fanout.as_ref()?.score;
        s.artifact_spec.as_ref()?;
        s.artifact_spec_sha256.as_deref().or_else(|| self.admission.as_ref().map(|a| a.artifact_spec_sha256.as_str()))
    }

    /// The fan-out block (present in every v2 and load plan).
    pub fn fanout(&self) -> Result<&Fanout, PlanError> {
        self.fanout.as_ref().ok_or(PlanError("plan has no fan-out block (v1)"))
    }

    pub fn is_load(&self) -> bool {
        self.load.is_some()
    }

    /// A `reward-v1` cohort or probe build (v3 records).
    pub fn is_v3(&self) -> bool {
        self.reward_v1.is_some()
    }

    /// A probe build: no admission wait, no package or target.
    pub fn is_probe_build(&self) -> bool {
        self.schema == PROBE_PLAN_SCHEMA
    }

    /// Items to create per kind, spares included for the generated kinds.
    pub fn generated_counts(&self) -> [u32; 3] {
        let [h, m, s] = self.counts();
        let p = self.reward_v1.as_ref().map_or(0, |v| u64::from(v.spares_permille));
        let spare = |c: u32| c + (u64::from(c) * p).div_ceil(1000) as u32;
        [h, spare(m), spare(s)]
    }

    /// Canary audit humans in draw A (zero outside reward-v1 cohorts).
    pub fn audit_humans(&self) -> u32 {
        match &self.reward_v1 {
            Some(v) if !self.is_probe_build() => v.canary.draw_humans,
            _ => 0,
        }
    }

    /// Canary audit humans in draw B, the re-audit after a first flag. The
    /// testnet canary has no anchors (`testnet_drill_only_v1`): only a drill
    /// entry can flag, so draw B is taken only when the plan lists one.
    pub fn audit_b_humans(&self) -> u32 {
        match &self.reward_v1 {
            Some(v) if !v.canary.drill.is_empty() => self.audit_humans(),
            _ => 0,
        }
    }

    /// Scored artifacts: R_k, R_{k-1} when present, then the entrants.
    pub fn jobs(&self) -> usize {
        1 + usize::from(self.previous_reference.is_some()) + self.entrants.len()
    }

    /// Documents the cohort's jobs score, one attempt each: under reward-v1
    /// the references score the batch and every entrant also scores every
    /// probe version in use and both canary audit draws; otherwise every
    /// job scores the batch.
    pub fn scored_docs(&self) -> u64 {
        match &self.reward_v1 {
            Some(_) if !self.is_probe_build() => {
                let refs = 1 + u64::from(self.previous_reference.is_some());
                refs * u64::from(self.n) + self.entrants.len() as u64 * self.entrant_docs()
            }
            _ => self.jobs() as u64 * u64::from(self.n),
        }
    }

    /// Documents one entrant's job scores, one attempt: under reward-v1 the
    /// batch, every probe version in use and both canary audit draws;
    /// otherwise the batch.
    pub fn entrant_docs(&self) -> u64 {
        match &self.reward_v1 {
            Some(v) if !self.is_probe_build() => {
                let probe: u64 = v.probes.iter().map(|p| u64::from(p.size)).sum();
                u64::from(self.n) + probe + u64::from(self.audit_humans()) + u64::from(self.audit_b_humans())
            }
            _ => u64::from(self.n),
        }
    }

    /// Receipts the cohort can record: every scored document at every
    /// attempt (`max_attempts`).
    pub fn receipt_bound(&self) -> u64 {
        self.scored_docs().saturating_mul(u64::from(self.max_attempts))
    }

    pub fn validate(&self, ceilings: Ceilings) -> Result<(), PlanError> {
        let e = |c: bool, what: &'static str| if c { Ok(()) } else { Err(PlanError(what)) };
        // New cohorts are reward-v1 (v3) or probe builds; stored v2 plans still
        // deserialize through serde without this check.
        let v0 = self.schema == PLAN_SCHEMA;
        e(
            self.schema == PLAN_SCHEMA_V3 || self.schema == PROBE_PLAN_SCHEMA || (v0 && ceilings.accept_reward_v0),
            "schema (v3 or probe build)",
        )?;
        e(self.load.is_none(), "load block (load instance only)")?;
        e(self.admission.is_none() || self.schema == PLAN_SCHEMA_V3, "admission (v3 cohort plans only)")?;
        e(
            !self.label.is_empty()
                && self.label.len() <= 96
                && self
                    .label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "label",
        )?;
        e(self.n >= MIN_CASES, "n")?;
        // Every document each job scores, at every attempt, fits one receipt
        // index: references score the batch, entrants the batch, every probe
        // version in use and both canary audit draws.
        e(receipt_capacity(self.scored_docs(), self.max_attempts), "n (receipts beyond one signed receipt index)")?;
        // The mix is a plan parameter: whole percentages summing to 100, every
        // origin present, and for a cohort realized exactly at this n (the
        // manifest's batch design checks the same). A probe build has no
        // manifest batch: its P documents follow the mix by largest remainder
        // (1,024 at 20/55/25 is 205/563/256).
        let parts = [self.mix.human_only, self.mix.mixed, self.mix.model_only];
        let exact = self.schema == PROBE_PLAN_SCHEMA
            || parts.iter().all(|p| u64::from(self.n) * u64::from(*p) % 100 == 0);
        e(
            !parts.contains(&0) && parts.iter().sum::<u32>() == 100 && exact,
            "mix (whole percentages summing to 100, exact at n)",
        )?;
        e(self.admission_tempos >= 1, "admission_tempos")?;
        if v0 {
            e(self.reward_v1.is_none(), "reward_v1 block (v3 plans only)")?;
            e(self.transition_tempos >= 1, "transition_tempos")?;
            e(self.k_tempos > self.admission_tempos + self.transition_tempos, "k_tempos")?;
        } else {
            e(self.transition_tempos >= 2, "transition_tempos (at least 2)")?;
            e(
                self.k_tempos >= self.admission_tempos + self.transition_tempos,
                "k_tempos (at least the admission and transition minimums)",
            )?;
            let probe_build = self.schema == PROBE_PLAN_SCHEMA;
            let v1 = self.reward_v1.as_ref().ok_or(PlanError("reward_v1 block"))?;
            v1.validate(self.k_tempos, probe_build)?;
            // The strict false-positive tier is measured on audit draw A; it
            // must rest on at least as many humans as the loose tier (the
            // cohort's own), so its interval is no wider.
            e(
                probe_build || self.audit_humans() >= self.counts()[0],
                "reward_v1.canary.draw_humans (at least the cohort's humans)",
            )?;
            e(
                self.max_kind_shortfall_permille == 0,
                "max_kind_shortfall_permille (reward-v1 batches are exact; use spares)",
            )?;
            if probe_build {
                e(self.entrants.is_empty(), "entrants (none in a probe build)")?;
                e(self.admission.is_none(), "admission (none in a probe build)")?;
            }
            if let Some(a) = &self.admission {
                a.validate()?;
                // The scorer admits and sizes every load by the cohort's spec.
                let f = self.fanout()?;
                e(
                    f.score.artifact_spec.as_deref() == Some(a.artifact_spec.as_str()),
                    "fanout.score.artifact_spec (the admission spec)",
                )?;
                e(
                    ceilings.roster_cap.is_none_or(|cap| usize::from(a.roster_cap) <= cap),
                    "admission.roster_cap (beyond the deployment's roster cap)",
                )?;
                // Entrants come only from admission, in position order; each
                // carries its opening nonce, extrinsic index and object.
                e(
                    self.entrants.iter().all(|x| {
                        x.opening_nonce.as_deref().and_then(parse_hex32).is_some()
                            && x.extrinsic_index.is_some()
                            && x.object_key.as_deref().is_some_and(key_ok)
                    }),
                    "entrants (an admitted roster names each opening nonce, extrinsic and object)",
                )?;
                e(self.entrants.len() <= usize::from(a.roster_cap), "entrants (beyond admission.roster_cap)")?;
                e(
                    self.entrants.windows(2).all(|w| {
                        (w[0].commitment_block, w[0].extrinsic_index) < (w[1].commitment_block, w[1].extrinsic_index)
                    }),
                    "entrants (in commitment position order)",
                )?;
            } else {
                e(
                    self.entrants.iter().all(|x| x.opening_nonce.is_none() && x.extrinsic_index.is_none() && x.object_key.is_none()),
                    "entrants (admission fields need an admission block)",
                )?;
            }
        }
        // At least one attempt; the receipt's attempt number (u8) bounds it.
        e(self.max_attempts >= 1, "max_attempts")?;
        e(
            key_ok(&self.pool.manifest_key)
                && parse_hex32(&self.pool.manifest_sha256).is_some()
                && !self.pool.pool_id.is_empty(),
            "pool",
        )?;
        e(
            !self.generators.is_empty()
                && self.generators.iter().collect::<std::collections::BTreeSet<_>>().len() == self.generators.len()
                && self
                    .generators
                    .iter()
                    .all(|g| g.starts_with("@cf/") && g.len() <= 96 && crate::eval::textgen::price(g).is_some()),
            "generators (distinct priced @cf models only)",
        )?;
        // The deployment's monthly spend ledger bounds every budget; the plan
        // only has to declare one.
        e(self.generation_budget_microusd > 0, "generation_budget_microusd")?;
        e(
            self.max_kind_shortfall_permille <= MAX_KIND_SHORTFALL_PERMILLE,
            "max_kind_shortfall_permille",
        )?;
        self.validate_common(ceilings, false)
    }

    /// Checks shared by cohort and load plans: scorer, fan-out, identities and roster.
    fn validate_common(&self, ceilings: Ceilings, load: bool) -> Result<(), PlanError> {
        let e = |c: bool, what: &'static str| if c { Ok(()) } else { Err(PlanError(what)) };
        let rule = if load || self.schema == PLAN_SCHEMA { "testnet-reward-v0" } else { "reward-v1" };
        e(self.reward_rule == rule, "reward_rule")?;
        // At least one attempt; the receipt's attempt number (u8) bounds it.
        e(self.max_attempts >= 1, "max_attempts")?;
        e(
            ceilings.roster_cap.is_none_or(|cap| self.entrants.len() <= cap),
            "entrants (beyond the deployment's roster cap)",
        )?;
        // 128 is the scorer's per-request document limit (a-scorer MAX_DOCS).
        e((1..=128).contains(&self.scorer.chunk_docs) && self.scorer.parallel_jobs.is_none(), "scorer")?;
        self.fanout()?.validate(self.scorer.chunk_docs, load)?;
        // A named artifact spec is frozen with its digest (`load_spec_sha256`):
        // lanes read it from the plan, and it is this build's digest of the spec.
        if let Some(id) = &self.fanout()?.score.artifact_spec {
            let have = load_spec_sha256_of(id)
                .ok_or(PlanError("fanout.score.artifact_spec (a spec the scorer admits loads by)"))?;
            let named = [
                self.fanout()?.score.artifact_spec_sha256.as_deref(),
                self.admission.as_ref().map(|a| a.artifact_spec_sha256.as_str()),
            ];
            e(
                named.iter().any(Option::is_some),
                "fanout.score.artifact_spec_sha256 (the named spec's digest, or the admission block's)",
            )?;
            e(
                named.iter().flatten().all(|d| *d == have),
                "fanout.score.artifact_spec_sha256 (this build's digest of the named spec)",
            )?;
        } else {
            e(
                self.fanout()?.score.artifact_spec_sha256.is_none(),
                "fanout.score.artifact_spec_sha256 (needs artifact_spec)",
            )?;
        }
        e(
            !self.label.is_empty()
                && self.label.len() <= 96
                && self.label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "label",
        )?;
        let burn = self.burn_identity.identity()?;
        e(burn.uid == 0, "burn identity uid")?;
        let mut allow = Vec::new();
        for a in &self.allow_list {
            let id = a.identity()?;
            e(id.uid != 0, "allow_list uid 0")?;
            allow.push(id);
        }
        allow.sort();
        e(allow.windows(2).all(|w| w[0] != w[1]), "allow_list duplicate")?;
        let mut ids = vec![self.reference.id()?];
        if let Some(p) = &self.previous_reference {
            ids.push(p.id()?);
        }
        for a in std::iter::once(&self.reference)
            .chain(self.previous_reference.iter())
            .chain(self.entrants.iter())
        {
            e(key_ok(&a.manifest_key), "artifact manifest_key")?;
        }
        let mut owners = Vec::new();
        for a in &self.entrants {
            ids.push(a.id()?);
            owners.push(a.owner.identity()?);
        }
        let mut sorted = ids.clone();
        sorted.sort();
        e(sorted.windows(2).all(|w| w[0] != w[1]), "duplicate artifact id")?;
        owners.sort();
        e(owners.windows(2).all(|w| w[0] != w[1]), "one entrant per identity")?;
        Ok(())
    }

    pub fn allow_list(&self) -> Result<Vec<NeuronIdentity>, PlanError> {
        let mut v = self
            .allow_list
            .iter()
            .map(IdentityJson::identity)
            .collect::<Result<Vec<_>, _>>()?;
        v.sort();
        Ok(v)
    }

    pub fn counts(&self) -> [u32; 3] {
        kind_counts(self.n, &self.mix)
    }

    /// The most cases each kind may lack and still seal.
    pub fn shortfall_tolerance(&self) -> [u32; 3] {
        self.counts()
            .map(|c| (u64::from(c) * u64::from(self.max_kind_shortfall_permille) / 1000) as u32)
    }

    /// Per-kind shortfall of `have` against the plan, if any kind exceeds its tolerance.
    pub fn excess_shortfall(&self, have: [u32; 3]) -> Option<[u32; 3]> {
        let want = self.counts();
        let tol = self.shortfall_tolerance();
        let short = [0, 1, 2].map(|k| want[k].saturating_sub(have[k]));
        (0..3).any(|k| short[k] > tol[k]).then_some(short)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn identity(uid: u16, byte: u8) -> IdentityJson {
        IdentityJson {
            uid,
            hotkey: format!("0x{}", hex::encode([byte; 32])),
            registration_block: 1000 + u64::from(uid),
        }
    }

    pub fn artifact(owner: IdentityJson, package: u8) -> ArtifactJson {
        ArtifactJson {
            owner,
            contract: "slop_ninja_full_a_contract_v2".into(),
            package_sha256: hex::encode([package; 32]),
            model_sha256: hex::encode([0x33; 32]),
            image_digest: hex::encode([0x44; 32]),
            commitment_block: 5,
            manifest_key: format!("artifacts/packages/{}.json", hex::encode([package; 32])),
            opening_nonce: None,
            extrinsic_index: None,
            object_key: None,
        }
    }

    pub fn plan(n: u32) -> CohortPlan {
        CohortPlan {
            schema: PLAN_SCHEMA.into(),
            cohort: 1,
            label: "testnet-rehearsal-1".into(),
            mechanism: 0,
            n,
            mix: Mix {
                human_only: 40,
                mixed: 40,
                model_only: 20,
            },
            k_tempos: 4,
            admission_tempos: 1,
            transition_tempos: 1,
            max_attempts: 3,
            pool: PoolRef {
                pool_id: "p".into(),
                manifest_key: "pools/p/manifest.json".into(),
                manifest_sha256: hex::encode([1; 32]),
            },
            generators: vec!["@cf/meta/llama-3.2-3b-instruct".into()],
            generation_budget_microusd: 600_000,
            reward_rule: "testnet-reward-v0".into(),
            burn_identity: identity(0, 0xb0),
            allow_list: vec![identity(3, 0xa3)],
            reference: artifact(identity(0, 0xb0), 0x22),
            previous_reference: None,
            entrants: vec![artifact(identity(3, 0xa3), 0x22)],
            scorer: Scorer {
                chunk_docs: 8,
                parallel_jobs: None,
            },
            max_kind_shortfall_permille: 0,
            fanout: Some(fanout(8)),
            load: None,
            reward_v1: None,
            admission: None,
        }
    }

    /// The fan-out block the tests use: two documents' worth of chunks per lane.
    pub fn fanout(lane_docs: u32) -> Fanout {
        Fanout {
            score: ScoreLanes {
                lane_docs,
                max_lanes: 64,
                lease_ms: 60_000,
                isolation_lease_ms: 240_000,
                scoring_budget_microusd: 5_000_000,
                prestart_lead_ms: 0,
                prestart_wait_ms: 120_000,
                inactivity_ms: 60_000,
                failures_before_pause: 6,
                step_budget_ms: Some(600_000),
                doc_timeout_ms: Some(60_000),
                artifact_max_mib: Some(640),
                artifact_spec: None,
                artifact_spec_sha256: None,
                load_ms_per_mib: Some(150),
                artifact_failures_before_model_error: Some(3),
            },
            generation: GenLanes {
                max_lanes: 4,
                items_per_lane: 4,
                inflight_per_lane: 4,
                lease_ms: 120_000,
                max_throttle_ms: 900_000,
                failures_before_pause: Some(GEN_FAILURES_BEFORE_PAUSE_COMPILED),
            },
            package: PackageLanes { max_lanes: 16 },
            cost: CostModel {
                instance_nanousd_per_s: 31_400,
                vcpu_nanousd_per_s: 20_000,
                vcpus: 4,
                lane_do_nanousd_per_s: 1_563,
                doc_ub_nanousd: 40_000,
                cold_ub_nanousd: 2_500_000,
            },
        }
    }

    const NONE: Ceilings = Ceilings { roster_cap: None, accept_reward_v0: true };

    /// The specs a plan may name for its loads are the specs the scorer
    /// admits loads by (its `engine::admits_by`), with the digests the
    /// deployed scorer lists.
    #[test]
    fn a_plan_names_only_a_spec_the_scorer_admits_loads_by() {
        use slop_ninja_artifact_check::spec::Version;
        let scorer = include_str!("../../../../inference/a-container-serve/src/engine.rs");
        let rule = scorer
            .split_once("pub fn admits_by(v: Version) -> bool {")
            .and_then(|(_, rest)| rest.split_once('}'))
            .expect("the scorer's admits_by")
            .0
            .trim();
        assert_eq!(rule, "v.max_json_bytes().is_some() && v.max_json_value_bytes().is_some()");
        assert!(load_spec_sha256_of(Version::V1.id()).is_none());
        assert_eq!(load_spec_sha256_of(Version::V2.id()), Some(Version::V2.spec_sha256()));
        assert_eq!(load_spec_sha256_of(Version::V3.id()), Some(Version::V3.spec_sha256()));
        assert!(load_spec_sha256_of("slop_ninja_a_artifact_spec_v4").is_none() && load_spec_sha256_of("v3").is_none());
        // The digests the live scorer lists in /healthz (scorer go-live, 2026-11-02).
        assert_eq!(Version::V2.spec_sha256(), "0f25e636ba52b45269c0a4d0520c2f01550b51325bb6399d4ddae418c8384b50");
        assert_eq!(Version::V3.spec_sha256(), "0bb326a93b1f4669018dadac3186f08bcc0ac527983eed0c75ce1e889e970d25");
        // The size bound comes from the class and JSON bounds without the
        // spec record, and equals the admission bounds'.
        for v in [Version::V2, Version::V3] {
            assert_eq!(
                crate::eval::admission::files_bytes(v),
                crate::eval::admission::SpecBounds::of(v).map(|b| b.files_bytes)
            );
        }
        let mut p = plan(10);
        assert_eq!(p.load_spec_sha256(), None);
        let lanes = &mut p.fanout.as_mut().unwrap().score;
        lanes.artifact_max_mib = None;
        lanes.artifact_spec = Some(Version::V3.id().into());
        // Spec v3's bound (the tower class) needs a faster load rate to fit
        // one isolation proof in the plan's step budget.
        lanes.load_ms_per_mib = Some(50);
        // The digest is frozen with the plan: naming the spec without it is refused.
        assert!(p.validate(NONE).unwrap_err().0.contains("artifact_spec_sha256"));
        p.fanout.as_mut().unwrap().score.artifact_spec_sha256 = Some(Version::V3.spec_sha256());
        p.validate(NONE).unwrap();
        assert_eq!(p.load_spec_sha256(), Some(Version::V3.spec_sha256().as_str()));
        // Another spec's digest, or a digest without a spec, is refused.
        p.fanout.as_mut().unwrap().score.artifact_spec_sha256 = Some(Version::V2.spec_sha256());
        assert!(p.validate(NONE).unwrap_err().0.contains("this build's digest"));
        let mut q = plan(10);
        q.fanout.as_mut().unwrap().score.artifact_spec_sha256 = Some(Version::V2.spec_sha256());
        assert!(q.validate(NONE).unwrap_err().0.contains("needs artifact_spec"));
        p.fanout.as_mut().unwrap().score.artifact_spec = Some(Version::V1.id().into());
        p.fanout.as_mut().unwrap().score.artifact_spec_sha256 = Some(Version::V1.spec_sha256());
        assert!(p.validate(NONE).unwrap_err().0.contains("artifact_spec"));
    }

    #[test]
    fn plan_validation_and_counts() {
        let p = plan(200);
        p.validate(NONE).unwrap();
        assert_eq!(p.counts(), [80, 80, 40]);
        assert_eq!(kind_counts(1000, &p.mix), [400, 400, 200]);
        assert_eq!(kind_counts(11, &p.mix).iter().sum::<u32>(), 11);
        // A duplicate of the reference by another owner is a distinct artifact.
        assert_ne!(p.reference.id().unwrap(), p.entrants[0].id().unwrap());
        let mut bad = plan(200);
        bad.k_tempos = 2;
        assert!(bad.validate(NONE).is_err());
        let mut bad = plan(200);
        bad.mix.model_only = 30;
        assert!(bad.validate(NONE).is_err());
        let mut bad = plan(200);
        bad.generators = vec!["@cf/unpriced/model".into()];
        assert!(bad.validate(NONE).is_err());
        let mut bad = plan(200);
        bad.entrants.push(bad.entrants[0].clone());
        assert!(bad.validate(NONE).is_err());
        let mut bad = plan(200);
        bad.reward_rule = "reward-v1".into();
        assert!(bad.validate(NONE).is_err());
        let mut bad = plan(200);
        bad.pool.manifest_key = "../x".into();
        assert!(bad.validate(NONE).is_err());
        let mut bad = plan(200);
        bad.max_kind_shortfall_permille = 51;
        assert!(bad.validate(NONE).is_err());
        // The roster cap is the deployment's, not the plan's.
        let mut wide = plan(200);
        wide.entrants = (1..=41u16).map(|u| artifact(identity(u, u as u8), 0x40 + u as u8)).collect();
        wide.allow_list = (1..=41u16).map(|u| identity(u, u as u8)).collect();
        wide.validate(NONE).unwrap();
        assert!(wide.validate(Ceilings { roster_cap: Some(40), accept_reward_v0: true }).is_err());
        wide.entrants.pop();
        wide.validate(Ceilings { roster_cap: Some(40), accept_reward_v0: true }).unwrap();
        // v2 needs the fan-out block, and v1's slot count is gone.
        let mut bad = plan(200);
        bad.fanout = None;
        assert!(bad.validate(NONE).is_err());
        let mut bad = plan(200);
        bad.scorer.parallel_jobs = Some(2);
        assert!(bad.validate(NONE).is_err());
        let mut bad = plan(200);
        bad.fanout.as_mut().unwrap().score.lane_docs = 4;
        assert!(bad.validate(NONE).is_err(), "a lane covers at least one chunk");
        let mut bad = plan(200);
        bad.fanout.as_mut().unwrap().generation.inflight_per_lane = 7;
        assert!(bad.validate(NONE).is_err());
        // The lane limits are plan parameters: required, and consistent with
        // the alarm wall, the isolation lease and one isolation proof per step.
        let mut bad = plan(200);
        bad.fanout.as_mut().unwrap().score.doc_timeout_ms = None;
        assert!(bad.validate(NONE).is_err());
        let mut bad = plan(200);
        bad.fanout.as_mut().unwrap().score.step_budget_ms = Some(ALARM_WALL_MS);
        assert!(bad.validate(NONE).is_err(), "a step plus a lease must fit the alarm wall");
        let mut bad = plan(200);
        bad.fanout.as_mut().unwrap().score.doc_timeout_ms = Some(230_000);
        assert!(bad.validate(NONE).is_err(), "a single-document request fits the isolation lease");
        let mut bad = plan(200);
        bad.fanout.as_mut().unwrap().score.load_ms_per_mib = Some(400);
        assert!(bad.validate(NONE).is_err(), "one isolation proof fits a step");
        let l = plan(200).fanout.unwrap().score.limits();
        assert_eq!((l.load_wait_ms, l.artifact_max_bytes, l.doc_timeout_ms), (96_000, 640 << 20, 60_000));
        // A plan stored before the fields existed reads with the earlier compiled values.
        let mut v = serde_json::to_value(plan(200)).unwrap();
        for k in ["step_budget_ms", "doc_timeout_ms", "artifact_max_mib", "load_ms_per_mib", "artifact_failures_before_model_error"] {
            v["fanout"]["score"].as_object_mut().unwrap().remove(k);
        }
        let stored: CohortPlan = serde_json::from_value(v).unwrap();
        let l = stored.fanout.unwrap().score.limits();
        assert_eq!((l.step_budget_ms, l.load_wait_ms, l.artifact_failures_before_model_error), (600_000, 180_000, u32::MAX));
    }

    #[test]
    fn stored_v1_plans_still_read_and_load_plans_are_their_own_schema() {
        let mut v = serde_json::to_value(plan(200)).unwrap();
        v["schema"] = PLAN_SCHEMA_V1.into();
        v["scorer"] = serde_json::json!({"chunk_docs": 32, "parallel_jobs": 2});
        v.as_object_mut().unwrap().remove("fanout");
        let stored: CohortPlan = serde_json::from_value(v.clone()).unwrap();
        assert_eq!((stored.scorer.parallel_jobs, stored.fanout.is_none()), (Some(2), true));
        assert!(CohortPlan::parse(&serde_json::to_vec(&v).unwrap(), NONE).is_err(), "v1 is no longer accepted");
        let p = plan(200);
        let load = LoadPlan {
            schema: LOAD_PLAN_SCHEMA.into(),
            cohort: 1001,
            label: "load-s2a".into(),
            source: LoadSource { label: "r2".into(), index_key: "cohorts/r2/batch/index.json".into(), index_sha256: hex::encode([7; 32]), tasks: 20 },
            max_attempts: 3,
            burn_identity: p.burn_identity.clone(),
            allow_list: p.allow_list.clone(),
            reference: p.reference.clone(),
            entrants: p.entrants.clone(),
            scorer: p.scorer.clone(),
            fanout: fanout(8),
            faults: LoadFaults::default(),
        };
        let bytes = serde_json::to_vec(&load).unwrap();
        let parsed = LoadPlan::parse(&bytes, NONE).unwrap();
        assert!(parsed.cohort_plan().is_load());
        assert!(CohortPlan::parse(&bytes, NONE).is_err(), "a load plan is never a cohort plan");
        let mut bad = load.clone();
        bad.label = "s2a".into();
        assert!(LoadPlan::parse(&serde_json::to_vec(&bad).unwrap(), NONE).is_err());
        let mut short = load;
        short.fanout.score.lease_ms = 5_000;
        LoadPlan::parse(&serde_json::to_vec(&short).unwrap(), NONE).unwrap();
    }

    #[test]
    fn shortfall_tolerance_is_per_kind_and_absent_means_exact() {
        let mut p = plan(1000);
        // A plan without the field parses with tolerance 0.
        let mut v = serde_json::to_value(&p).unwrap();
        v.as_object_mut().unwrap().remove("max_kind_shortfall_permille");
        let parsed = CohortPlan::parse(&serde_json::to_vec(&v).unwrap(), NONE).unwrap();
        assert_eq!(parsed.max_kind_shortfall_permille, 0);
        assert_eq!(parsed.excess_shortfall([400, 399, 200]), Some([0, 1, 0]));
        assert_eq!(parsed.excess_shortfall([400, 400, 200]), None);
        p.max_kind_shortfall_permille = 10;
        assert_eq!(p.shortfall_tolerance(), [4, 4, 2]);
        assert_eq!(p.excess_shortfall([396, 400, 198]), None);
        assert_eq!(p.excess_shortfall([400, 400, 197]), Some([0, 0, 3]));
    }
}
