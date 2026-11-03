//! Deterministic synthetic `reward-v1` fixtures and their golden report.
//!
//! Every value is synthetic: hotkeys are repeated bytes, nonces are derived
//! from public tags, and the signing seeds are the public test vectors of
//! [`crate::fixtures`]. The report is compared byte for byte against
//! `fixtures/golden-reward-v1.txt` on native targets and inside a
//! `wasm32-unknown-unknown` build. Scores are written in basis points of
//! utility (`bp(9500)` is 0.95) and kept exact.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::fixtures::{authorization_key, package_key, receipt_key, trusted_keys};
use crate::hash::hex;
use crate::reward_v1::{self, ArchiveProbe, Evaluation, Graded, Measurements};
use crate::submission::{
    CheckedClosure, CohortClosure, FinalizedCommitment, PublishedOpening, SubmissionOpening,
    SubmissionUpload,
};
use crate::{
    AccountId, ActiveTarget, ArtifactReader, BatchDesign, CalculationTargetV2, CanaryAudit,
    ChainBinding, CheckedAuthorization, CheckedRenewal, CohortManifestV2, CohortSchedule,
    Component, ComponentRole, CutoffMode, Digest, DistanceMonitor, EntrantFingerprint,
    EntrantScore, Error, FINNEY_GENESIS, FileRole, Fingerprint, Gate, GateFailure,
    InferenceContent, LaunchAuthorization, NEAR_COPY_PPB, NearStats, NeuronIdentity, Nonce, Origin,
    OriginMix, Probability, ProbeBinding, ProbeMatch, PromotionAudit, PublicCopy,
    PublishedContents, PublishedIndex, Record, ReferenceModel, ReferenceScore, ReferenceSet,
    RegistrationSnapshotV2, Result, RewardIntentAuthorization, RewardIntentRenewal, RewardRule,
    RewardV1Params, RosterEntry, SCORE_SCALE, Score, ScorePackageV2, Signed, Stratum,
    TESTNET_GENESIS, VerifiedArtifact, WeightRowV2, domain_hash, object_sha256, sign,
    signing_payload,
};

/// The testnet genesis with a placeholder netuid.
pub fn chain() -> ChainBinding {
    ChainBinding {
        genesis: TESTNET_GENESIS,
        netuid: 4242,
        mechanism: 0,
    }
}

/// Finney, on which `reward-v1` is permitted and `testnet-reward-v0` is not.
pub fn finney() -> ChainBinding {
    ChainBinding {
        genesis: FINNEY_GENESIS,
        ..chain()
    }
}

pub fn owner_hotkey() -> AccountId {
    AccountId([0x0f; 32])
}

pub fn hotkey(tag: u8) -> AccountId {
    AccountId([tag; 32])
}

/// Score from basis points of utility.
pub fn bp(value: u64) -> Score {
    Score::new(value * (SCORE_SCALE / 10_000)).expect("fixture score")
}

/// `sigma` from tenths of a percent of headroom: `sigma_permille(40)` is 0.04.
pub fn sigma_permille(value: u64) -> u64 {
    value * (SCORE_SCALE / 1000)
}

/// The launch schedule: `K = 20`, admission at least 1, transition at least
/// 2, lease `2K`.
pub fn schedule() -> CohortSchedule {
    CohortSchedule {
        tempos: 20,
        admission_min: 1,
        transition_min: 2,
        lease_tempos: 40,
    }
}

/// The mix of the synthetic batches: 40/40/20. The mix is a manifest
/// parameter; the launch mix is [`crate::LAUNCH_ORIGIN_MIX`].
pub const FIXTURE_MIX: OriginMix = OriginMix {
    human_only: 40,
    mixed: 40,
    model_only: 20,
};

/// A two-domain batch with the fixture mix.
pub fn batch(cases: u32) -> BatchDesign {
    let cell = |domain: &str, origin, share: u32| Stratum {
        domain: domain.into(),
        origin,
        cases: cases * share / 200,
    };
    BatchDesign {
        cases,
        source_groups: (cases * 2 / 5).max(1),
        mix: FIXTURE_MIX,
        strata: vec![
            cell("blogs", Origin::HumanOnly, 40),
            cell("blogs", Origin::Mixed, 40),
            cell("blogs", Origin::ModelOnly, 20),
            cell("essays", Origin::HumanOnly, 40),
            cell("essays", Origin::Mixed, 40),
            cell("essays", Origin::ModelOnly, 20),
        ],
        batch: Digest([0x22; 32]),
    }
}

/// The frozen output contract of every synthetic artifact.
pub const CONTRACT: &str = "slop_ninja.a.document-v2";

/// The artifact object of content family `content` as published by
/// `publisher`: a synthetic graph and tokenizer, which are its inference
/// content, and owner metadata, which is not. Equal content from different
/// publishers gives distinct objects with identical inference content.
pub fn artifact_object(content: u8, publisher: u8) -> Vec<u8> {
    let graph = format!("synthetic onnx graph {content:02x}").into_bytes();
    let owner = format!("{{\"publisher\":\"{publisher:02x}\"}}").into_bytes();
    crate::artifact_object(
        CONTRACT,
        &[
            ("model.onnx", FileRole::Graph, &graph),
            (
                "tokenizer.json",
                FileRole::Tokenizer,
                b"synthetic tokenizer",
            ),
            ("owner.json", FileRole::OwnerMetadata, &owner),
        ],
    )
    .expect("fixture artifact")
}

/// The verified artifact object of content family `content` by `publisher`.
pub fn verified(content: u8, publisher: u8) -> VerifiedArtifact {
    VerifiedArtifact::from_bytes(&artifact_object(content, publisher)).expect("fixture artifact")
}

/// Inference content of a synthetic model family, derived from its object.
pub fn inference_content(tag: u8) -> InferenceContent {
    verified(tag, tag)
        .header
        .content()
        .expect("fixture content")
}

/// Canonical inference-content digest of a synthetic model family.
pub fn content(tag: u8) -> Digest {
    verified(tag, tag).content
}

/// A reference model; its hosted object is published by itself.
pub fn reference_model(tag: u8) -> ReferenceModel {
    let artifact = verified(tag, tag);
    ReferenceModel {
        content: artifact.content,
        artifact: artifact.sha256,
    }
}

/// `R_k` with content tag `0xe0 + k`; `R_k-1` when `k > 0`.
pub fn references(index: u64) -> ReferenceSet {
    ReferenceSet {
        index,
        current: reference_model(0xe0 + index as u8),
        predecessor: (index > 0).then(|| reference_model(0xe0 + index as u8 - 1)),
    }
}

/// One synthetic entrant. Its hotkey and UID are `tag`; its registration
/// block is `1000 + tag`. Entrants with equal `content` have identical
/// inference content; with equal `family`, near-duplicate hidden predictions.
#[derive(Clone, Debug)]
pub struct Spec {
    pub tag: u8,
    pub score: Option<Score>,
    pub content: u8,
    pub family: u8,
    pub gate: Option<GateFailure>,
    pub failed: u32,
}

impl Spec {
    pub fn new(tag: u8, score: Score) -> Self {
        Self {
            tag,
            score: Some(score),
            content: tag,
            family: tag,
            gate: None,
            failed: 0,
        }
    }
    pub fn copy_of(mut self, original: u8) -> Self {
        self.content = original;
        self.family = original;
        self
    }
    /// An exact copy of the published artifact of content family `original`:
    /// the coordinator finds it at admission and never executes it.
    pub fn published_copy(tag: u8, original: u8) -> Self {
        Self {
            tag,
            score: None,
            content: original,
            family: original,
            gate: None,
            failed: 0,
        }
    }
    pub fn near(mut self, original: u8) -> Self {
        self.family = original;
        self
    }
    pub fn failing(mut self, gate: Gate, reason: &str) -> Self {
        self.gate = Some(GateFailure {
            gate,
            reason: reason.into(),
        });
        if gate == Gate::Qualification {
            self.score = None;
        }
        self
    }
    pub fn identity(&self) -> NeuronIdentity {
        NeuronIdentity {
            uid: u16::from(self.tag),
            hotkey: hotkey(self.tag),
            registration_block: 1000 + u64::from(self.tag),
        }
    }
}

pub fn nonce(tag: u8) -> Nonce {
    Nonce(domain_hash("slop_ninja.fixture-nonce", &[tag]).0)
}

pub fn opening(chain: &ChainBinding, cohort: u64, spec: &Spec) -> SubmissionOpening {
    SubmissionOpening {
        chain: chain.clone(),
        cohort,
        hotkey: hotkey(spec.tag),
        registration_block: spec.identity().registration_block,
        content: content(spec.content),
        nonce: nonce(spec.tag),
    }
}

/// The artifact object a spec uploads. Copies upload distinct objects with
/// identical inference content.
pub fn artifact_bytes(spec: &Spec) -> Vec<u8> {
    artifact_object(spec.content, spec.tag)
}

/// Finalized block of the `index`th roster entry's commitment, after every
/// fixture registration (blocks `1000 + tag`).
pub fn commitment_block(index: usize) -> u64 {
    2000 + index as u64
}

/// `spec`'s finalized on-chain commitment of `payload`: made by its own
/// registration at `block` with extrinsic index `tag`.
pub fn finalized_commitment<'a>(
    chain: &'a ChainBinding,
    spec: &Spec,
    block: u64,
    payload: &'a [u8],
) -> FinalizedCommitment<'a> {
    FinalizedCommitment {
        chain,
        committer: spec.identity(),
        block,
        extrinsic_index: u32::from(spec.tag),
        payload,
    }
}

/// The roster entry admission returns for `spec`'s upload, committed by its
/// own registration at `block` with extrinsic index `tag`.
pub fn admitted(chain: &ChainBinding, cohort: u64, spec: &Spec, block: u64) -> RosterEntry {
    let opening = opening(chain, cohort, spec);
    let bytes = artifact_bytes(spec);
    let artifact = VerifiedArtifact::from_bytes(&bytes).expect("fixture artifact");
    let payload = opening.chain_payload();
    let commitment = finalized_commitment(chain, spec, block, &payload);
    SubmissionUpload::new(opening, &bytes)
        .admit(cohort, &commitment, &artifact)
        .expect("fixture admission")
}

/// The reward-intent authorization a package carries for `target`, issued at
/// `issued_epoch` with the manifest's lease, unsigned.
pub fn authorization(
    manifest: &CohortManifestV2,
    package: &ScorePackageV2,
    target: &CalculationTargetV2,
    checkpoint_generation: u64,
    issued_epoch: u64,
) -> Result<RewardIntentAuthorization> {
    Ok(RewardIntentAuthorization {
        chain: manifest.chain.clone(),
        cohort: manifest.cohort,
        cohort_manifest: manifest.digest()?,
        score_package: package.digest()?,
        target: target.digest()?,
        checkpoint_generation,
        issued_epoch,
        expires_epoch: issued_epoch + u64::from(manifest.schedule.lease_tempos),
    })
}

/// A record signed under the fixture authorization key.
pub fn signed<R: Record>(record: R) -> Result<Signed<R>> {
    sign(record, &authorization_key())
}

/// The fingerprint probe version every fixture manifest uses.
pub const PROBE_VERSION: u32 = 1;
/// Probe size of the spec-based examples.
pub const EXAMPLE_PROBE_SIZE: u32 = 1024;

pub fn probe(size: u32) -> ProbeBinding {
    ProbeBinding {
        version: PROBE_VERSION,
        size,
        digest: Digest([0x35; 32]),
    }
}

/// Synthetic digest of a model family's private probe predictions.
pub fn fingerprint_digest(tag: u8) -> Digest {
    domain_hash("slop_ninja.fixture-fingerprint", &[tag])
}

/// The published index of the spec-based examples: `R_k`, `R_k-1` and every
/// earlier reference `R_j` (content family `0xe0 + j`), each fingerprinted on
/// the probe.
pub fn default_index(references: &ReferenceSet) -> PublishedIndex {
    let earlier = (0..references.index.saturating_sub(1)).map(|j| content(0xe0 + j as u8));
    let contents: Vec<Digest> = earlier
        .chain([references.current.content])
        .chain(references.predecessor.map(|p| p.content))
        .collect();
    PublishedIndex::new(
        contents.clone(),
        contents.into_iter().map(|content| Fingerprint {
            content,
            version: PROBE_VERSION,
            fingerprint: domain_hash("slop_ninja.fixture-fingerprint", &content.0),
        }),
    )
    .expect("fixture index")
}

/// A reference `R_0` whose content no synthetic entrant family shares, for
/// rosters that use every one-byte family.
pub fn independent_reference() -> ReferenceModel {
    let object = crate::artifact_object(
        CONTRACT,
        &[
            ("model.onnx", FileRole::Graph, b"synthetic reference graph"),
            (
                "tokenizer.json",
                FileRole::Tokenizer,
                b"synthetic tokenizer",
            ),
        ],
    )
    .expect("fixture artifact");
    let artifact = VerifiedArtifact::from_bytes(&object).expect("fixture artifact");
    ReferenceModel {
        content: artifact.content,
        artifact: artifact.sha256,
    }
}

/// The published index of a spec-based package, from its references.
pub fn example_index(package: &ScorePackageV2) -> PublishedIndex {
    default_index(&ReferenceSet {
        index: package.reference_index,
        current: package.reference.model,
        predecessor: package.predecessor.as_ref().map(|p| p.model),
    })
}

pub fn manifest(
    chain: ChainBinding,
    cohort: u64,
    cases: u32,
    sigma: u64,
    reference_index: u64,
    specs: &[Spec],
) -> CohortManifestV2 {
    let references = references(reference_index);
    let index = default_index(&references);
    manifest_with(
        chain,
        cohort,
        cases,
        sigma,
        references,
        &index,
        probe(EXAMPLE_PROBE_SIZE),
        specs,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn manifest_with(
    chain: ChainBinding,
    cohort: u64,
    cases: u32,
    sigma: u64,
    references: ReferenceSet,
    index: &PublishedIndex,
    probe: ProbeBinding,
    specs: &[Spec],
) -> CohortManifestV2 {
    CohortManifestV2 {
        cohort,
        cohort_label: format!("synthetic-reward-v1-{cohort}"),
        opened_epoch: 20 * cohort,
        reward_rule: RewardRule::RewardV1,
        schedule: schedule(),
        batch: batch(cases),
        params: RewardV1Params::new(sigma),
        references,
        published: index.binding().expect("fixture index"),
        probes: vec![probe],
        input_profile: Digest([0x31; 32]),
        output_contract: "slop_ninja.a.document-v2".into(),
        reference_gallery: Digest([0x32; 32]),
        gate_policy: Digest([0x33; 32]),
        span_gate: false,
        limits: Digest([0x34; 32]),
        max_attempts: 2,
        roster_cap: 32,
        roster: specs
            .iter()
            .enumerate()
            .map(|(i, s)| admitted(&chain, cohort, s, commitment_block(i)))
            .collect(),
        chain,
    }
}

fn reference_score(model: ReferenceModel, score: Score, cases: u32) -> ReferenceScore {
    ReferenceScore {
        model,
        score,
        assigned: cases,
        succeeded: cases,
        failed: 0,
    }
}

/// An unfinalized package for `manifest` with the given reference scores.
pub fn package(
    manifest: &CohortManifestV2,
    reference: Score,
    predecessor: Option<Score>,
    specs: &[Spec],
) -> Result<ScorePackageV2> {
    let cases = manifest.batch.cases;
    let entrants = specs
        .iter()
        .zip(&manifest.roster)
        .map(|(s, r)| {
            let scored = s.score.is_some();
            EntrantScore {
                identity: r.identity,
                commitment: r.commitment,
                commitment_block: r.commitment_block,
                extrinsic_index: r.extrinsic_index,
                content: content(s.content),
                artifact: object_sha256(&artifact_bytes(s)),
                gate_failures: s.gate.clone().into_iter().collect(),
                score: s.score,
                assigned: if scored { cases } else { 0 },
                succeeded: if scored { cases - s.failed } else { 0 },
                failed: if scored { s.failed } else { 0 },
                public: None,
                cluster_head: None,
                cluster_stats: None,
                units: 0,
            }
        })
        .collect::<Vec<_>>();
    let assigned: u64 = entrants.iter().map(|e| u64::from(e.assigned)).sum();
    Ok(ScorePackageV2 {
        chain: manifest.chain.clone(),
        cohort: manifest.cohort,
        cohort_label: manifest.cohort_label.clone(),
        cohort_manifest: manifest.digest()?,
        revision: 1,
        reward_rule: RewardRule::RewardV1,
        params: manifest.params,
        span_gate: manifest.span_gate,
        cases,
        reference_index: manifest.references.index,
        reference: reference_score(manifest.references.current, reference, cases),
        predecessor: manifest
            .references
            .predecessor
            .zip(predecessor)
            .map(|(m, s)| reference_score(m, s, cases)),
        published: manifest.published,
        probes: manifest.probes.clone(),
        entrants,
        mode: CutoffMode::NoTarget,
        promoted: None,
        promotion_audits: Vec::new(),
        credits: Vec::new(),
        distance_monitor: DistanceMonitor::default(),
        receipt_root: Digest([0x44; 32]),
        receipt_count: assigned + u64::from(cases) * 2,
    })
}

/// Statistics of two models that differ by twice the near-copy distance.
pub fn far(common: u32) -> NearStats {
    let d = 2 * u128::from(NEAR_COPY_PPB);
    NearStats {
        common,
        rss_fwd: u128::from(common) * d * d,
        rss_rev: u128::from(common) * d * d,
    }
}

/// Statistics of two models related by a rank-preserving map.
pub fn aligned(common: u32) -> NearStats {
    NearStats {
        common,
        rss_fwd: 0,
        rss_rev: 0,
    }
}

/// The private digest of synthetic canary draw `n`.
pub fn canary_draw(n: u8) -> Digest {
    domain_hash("slop_ninja.fixtures/canary-draw/v1", &[n])
}

/// A promotion audit of `head`: the strict tier's result and `flags` canary
/// flags (0, 1 or 2). A first flag is followed by a re-audit on a fresh draw,
/// which flags again when `flags` is 2.
pub fn promotion_audit(head: AccountId, strict_fpr_passed: bool, flags: u8) -> PromotionAudit {
    let flag = |on: bool| on.then(|| "canary.statistic".to_owned());
    let mut canary = vec![CanaryAudit {
        draw: canary_draw(1),
        flag: flag(flags >= 1),
    }];
    if flags >= 1 {
        canary.push(CanaryAudit {
            draw: canary_draw(2),
            flag: flag(flags >= 2),
        });
    }
    PromotionAudit {
        head,
        strict_fpr_passed,
        canary,
    }
}

/// Measurements of the spec-based examples: every entrant is twice `d` from
/// both references on the batch and on the probe, where probe scores are not
/// modelled (zero), and has no other archived fingerprint to match;
/// `near(head, other)` decides whether a pair is aligned or twice `d` apart.
pub struct SpecMeasure<F> {
    succeeded: Vec<u32>,
    near: F,
    /// Promotion-audit results by package index: (strict tier passed,
    /// canary flags). Unlisted candidates pass cleanly.
    pub audits: BTreeMap<usize, (bool, u8)>,
    hotkeys: Vec<AccountId>,
}

/// Spec-based measurements for a batch of `cases` documents.
pub fn spec_measure<F: FnMut(usize, usize) -> bool>(
    specs: &[Spec],
    cases: u32,
    near: F,
) -> SpecMeasure<F> {
    SpecMeasure {
        succeeded: specs.iter().map(|s| cases - s.failed).collect(),
        near,
        audits: BTreeMap::new(),
        hotkeys: specs.iter().map(|s| s.identity().hotkey).collect(),
    }
}

impl<F: FnMut(usize, usize) -> bool> Measurements for SpecMeasure<F> {
    fn current(&mut self, entrant: usize) -> Result<NearStats> {
        Ok(far(self.succeeded[entrant]))
    }
    fn predecessor(&mut self, entrant: usize) -> Result<NearStats> {
        Ok(far(self.succeeded[entrant]))
    }
    fn reference_probe(
        &mut self,
        _entrant: usize,
        content: &Digest,
        version: u32,
    ) -> Result<ProbeMatch> {
        Ok(ProbeMatch {
            content: *content,
            version,
            stats: far(EXAMPLE_PROBE_SIZE),
            entrant_probe_score: Score::ZERO,
            public_probe_score: Score::ZERO,
        })
    }
    fn archive_probe(&mut self, _entrant: usize) -> Result<ArchiveProbe> {
        Ok(ArchiveProbe::default())
    }
    fn pair(&mut self, head: usize, other: usize) -> Result<NearStats> {
        let common = self.succeeded[head].min(self.succeeded[other]);
        Ok(if (self.near)(head, other) {
            aligned(common)
        } else {
            far(common)
        })
    }
    fn promotion_audit(&mut self, head: usize) -> Result<PromotionAudit> {
        let (strict, flags) = self.audits.get(&head).copied().unwrap_or((true, 0));
        Ok(promotion_audit(self.hotkeys[head], strict, flags))
    }
}

/// A finalized package whose near-copy test compares spec families.
pub fn finalized(
    manifest: &CohortManifestV2,
    reference: Score,
    predecessor: Option<Score>,
    specs: &[Spec],
) -> Result<(ScorePackageV2, Evaluation)> {
    let mut p = package(manifest, reference, predecessor, specs)?;
    let index = default_index(&manifest.references);
    let mut measure = spec_measure(specs, manifest.batch.cases, |h, o| {
        specs[h].family == specs[o].family
    });
    let evaluation = reward_v1::finalize(&mut p, &index, &mut measure)?;
    Ok((p, evaluation))
}

/// A testnet example at 1,000 cases with `sigma = 0.04`, `R_k` at index 1.
pub fn example(
    reference: Score,
    predecessor: Option<Score>,
    specs: &[Spec],
) -> Result<ScorePackageV2> {
    let index = u64::from(predecessor.is_some());
    let m = manifest(chain(), 7, 1000, sigma_permille(40), index, specs);
    Ok(finalized(&m, reference, predecessor, specs)?.0)
}

/// Every spec registered at its own UID, with the owner at `owner_uid`.
pub fn snapshot(chain: ChainBinding, owner_uid: u16, specs: &[Spec]) -> RegistrationSnapshotV2 {
    let mut neurons: Vec<_> = specs.iter().map(Spec::identity).collect();
    neurons.push(NeuronIdentity {
        uid: owner_uid,
        hotkey: owner_hotkey(),
        registration_block: 10,
    });
    neurons.sort();
    RegistrationSnapshotV2 {
        chain,
        block: 9000,
        block_hash: Digest([0x55; 32]),
        owner_hotkey: owner_hotkey(),
        neurons,
    }
}

// ---------------------------------------------------------------------------
// The computed cohort: hidden predictions to scores, clusters and a row.
// ---------------------------------------------------------------------------

/// Documents in the computed cohort: 8 human-only, 8 mixed, 4 model-only.
/// The private probe follows the same design and layout.
pub const CASES: u32 = 20;

pub fn label(doc: usize) -> bool {
    doc >= 8
}

/// Per-document predictions from per-document errors in parts per billion.
pub fn predictions(errors: &[u32]) -> Predictions {
    errors
        .iter()
        .enumerate()
        .map(|(doc, &e)| {
            let a = if label(doc) { 1_000_000_000 - e } else { e };
            Some(Probability::new(a).expect("fixture probability"))
        })
        .collect()
}

fn uniform(e: u32) -> Vec<u32> {
    vec![e; CASES as usize]
}

/// Errors of `base` everywhere except a texture pair at each of `pairs`:
/// documents `p` and `p + 1` err by `base / 5` and `7 base / 5`, which keeps
/// the sum of squared errors, and so the score, while giving the model its own
/// ranking of documents within a class. Models with textures at different
/// positions are not rank-preserving maps of each other.
fn textured(base: u32, pairs: &[usize]) -> Vec<u32> {
    let mut errors = uniform(base);
    for &p in pairs {
        errors[p] = base / 5;
        errors[p + 1] = base / 5 * 7;
    }
    errors
}

/// One model's per-document predictions; `None` marks an attributable failure.
pub type Predictions = Vec<Option<Probability>>;

/// Fingerprinted artifacts: each one's content and probe predictions.
pub type Fingerprints = Vec<(Digest, Predictions)>;

/// A named computed-cohort entrant.
pub struct Model {
    pub name: &'static str,
    pub spec: Spec,
    /// `None` for an entrant that was never executed.
    pub predictions: Option<Predictions>,
    /// Its predictions on the private probe when they differ from its batch
    /// predictions; otherwise it predicts on the probe as on the batch.
    pub probe: Option<Predictions>,
}

impl Model {
    pub fn probe_predictions(&self) -> Option<&Predictions> {
        self.probe.as_ref().or(self.predictions.as_ref())
    }
}

/// `R_k` (`R_1`) errs by 0.30 and `R_k-1` (`R_0`) by 0.35, each with its own
/// texture.
pub fn reference_predictions() -> (Predictions, Predictions) {
    (
        predictions(&textured(300_000_000, &[6, 8])),
        predictions(&textured(350_000_000, &[2, 10])),
    )
}

/// An older published entrant (`omega`, content family `0xd0`), fingerprinted
/// on the probe when its cohort closed.
pub const ARCHIVED: u8 = 0xd0;

pub fn archived_predictions() -> Predictions {
    predictions(&textured(330_000_000, &[2, 12]))
}

pub fn models() -> Vec<Model> {
    let alpha = textured(200_000_000, &[12]);
    let mut close = alpha.clone();
    for (doc, e) in close.iter_mut().enumerate().take(8) {
        *e = if doc < 4 { 205_000_000 } else { 195_000_000 };
    }
    // beta errs by 0.2 on the first ten documents and 0.3 on the rest; gamma
    // permutes the same errors, so it ties beta's score with its own ranking.
    let mut beta = uniform(300_000_000);
    for e in beta.iter_mut().take(10) {
        *e = 200_000_000;
    }
    beta[2] = 40_000_000;
    beta[3] = 280_000_000;
    beta[16] = 60_000_000;
    beta[17] = 420_000_000;
    let mut gamma = vec![200_000_000; CASES as usize];
    for doc in [0, 1, 12, 13, 14, 15, 16, 17] {
        gamma[doc] = 300_000_000;
    }
    gamma[4] = 40_000_000;
    gamma[5] = 280_000_000;
    gamma[18] = 60_000_000;
    gamma[19] = 420_000_000;
    // theta errs like alpha with its own texture, and fails on two
    // documents: a failure scores zero and leaves the near-copy statistics.
    let mut failing = predictions(&textured(200_000_000, &[4]));
    failing[12] = None;
    failing[13] = None;
    // R_k flattened by 0.015 toward 0.5, a rank-preserving recalibration, with
    // one document pushed past R_k's next level: raw RMS 0.035 from R_k.
    let mut recalibrated: Vec<u32> = textured(300_000_000, &[6, 8])
        .iter()
        .map(|e| e + 15_000_000)
        .collect();
    recalibrated[0] = 440_000_000;
    // R_k-1 sharpened by 0.001 toward the labels: rank-preserving.
    let sharpened = textured(350_000_000, &[2, 10])
        .iter()
        .map(|e| e - 1_000_000)
        .collect::<Vec<_>>();
    let mut kappa = uniform(320_000_000);
    kappa[3] = 620_000_000;
    kappa[15] = 550_000_000;
    let spec = |tag| Spec::new(tag, Score::ZERO);
    let entries: Vec<(&'static str, Spec, Option<Predictions>, Option<Predictions>)> = vec![
        ("alpha", spec(0x11), Some(predictions(&alpha)), None),
        ("beta", spec(0x12), Some(predictions(&beta)), None),
        (
            "iota",
            spec(0x13),
            Some(predictions(&textured(220_000_000, &[0, 14]))),
            None,
        ),
        (
            "delta",
            spec(0x14),
            Some(predictions(&uniform(290_000_000))),
            None,
        ),
        ("gamma", spec(0x15), Some(predictions(&gamma)), None),
        (
            "alpha_copy",
            spec(0x16).copy_of(0x11),
            Some(predictions(&alpha)),
            None,
        ),
        (
            "beta_copy_1",
            spec(0x17).copy_of(0x12),
            Some(predictions(&beta)),
            None,
        ),
        (
            "zeta",
            spec(0x18).failing(Gate::Share, "share.mae_above_threshold"),
            Some(predictions(&uniform(100_000_000))),
            None,
        ),
        (
            "eta",
            spec(0x19).failing(Gate::Qualification, "qualification.embedded_text_index"),
            None,
            None,
        ),
        ("theta", spec(0x1a), Some(failing), None),
        ("alpha_close", spec(0x1b), Some(predictions(&close)), None),
        (
            "beta_copy_2",
            spec(0x1c).copy_of(0x12),
            Some(predictions(&beta)),
            None,
        ),
        // Copy policy B + D.
        (
            "rk_exact_copy",
            Spec::published_copy(0x1d, 0xe1),
            None,
            None,
        ),
        (
            "rk_recalibrated",
            spec(0x1e),
            Some(predictions(&recalibrated)),
            None,
        ),
        (
            "rk_retrain",
            spec(0x1f),
            Some(predictions(&textured(300_000_000, &[0, 16]))),
            None,
        ),
        (
            "rk1_sharpened",
            spec(0x20),
            Some(predictions(&sharpened)),
            None,
        ),
        (
            "omega_probe_copy",
            spec(0x22),
            Some(predictions(&textured(310_000_000, &[4, 18]))),
            Some(archived_predictions()),
        ),
        // An independent model below S_Rk-1, which misranks one document of
        // each class: executed, so tested and fingerprinted like every other
        // scored entrant.
        ("kappa", spec(0x23), Some(predictions(&kappa)), None),
    ];
    entries
        .into_iter()
        .map(|(name, mut spec, predictions, probe)| {
            if let Some(p) = &predictions {
                spec.score = Some(reward_v1::document_score(&graded(p)).expect("scored"));
                spec.failed = p.iter().filter(|x| x.is_none()).count() as u32;
            }
            Model {
                name,
                spec,
                predictions,
                probe,
            }
        })
        .collect()
}

fn graded(p: &[Option<Probability>]) -> Vec<Graded> {
    p.iter()
        .enumerate()
        .map(|(doc, &involvement)| Graded {
            involvement,
            label: label(doc),
        })
        .collect()
}

/// The computed cohort's published index: `R_0`, `R_1` and the archived
/// `omega`, each fingerprinted on the probe.
pub fn computed_index() -> Result<PublishedIndex> {
    let (current, previous) = reference_predictions();
    let fingerprinted = [
        (0xe0, previous),
        (0xe1, current),
        (ARCHIVED, archived_predictions()),
    ];
    PublishedIndex::new(
        fingerprinted.iter().map(|(tag, _)| content(*tag)),
        fingerprinted.iter().map(|(tag, p)| Fingerprint {
            content: content(*tag),
            version: PROBE_VERSION,
            fingerprint: prediction_digest(p),
        }),
    )
}

/// Synthetic fingerprint digest of exact probe predictions.
pub fn prediction_digest(p: &[Option<Probability>]) -> Digest {
    let bytes: Vec<u8> = p
        .iter()
        .flat_map(|x| x.map_or(u32::MAX, |x| x.ppb()).to_le_bytes())
        .collect();
    domain_hash("slop_ninja.fixture-fingerprint", &bytes)
}

/// The coordinator's measurements of the computed cohort on its hidden batch
/// and private probe predictions.
pub struct ComputedMeasure<'a> {
    pub models: &'a [Model],
    pub current: Predictions,
    pub previous: Predictions,
    /// Each fingerprinted artifact's content and probe predictions, the
    /// references' included.
    pub archive: Fingerprints,
    /// Reference fingerprints left out of `archive`, which `reference_probe`
    /// still finds: a coordinator whose archive comparison skips the
    /// references is still made to compare them.
    pub references: Fingerprints,
}

impl<'a> ComputedMeasure<'a> {
    pub fn new(models: &'a [Model]) -> Self {
        let (current, previous) = reference_predictions();
        Self {
            models,
            archive: vec![
                (content(0xe0), previous.clone()),
                (content(0xe1), current.clone()),
                (content(ARCHIVED), archived_predictions()),
            ],
            references: Vec::new(),
            current,
            previous,
        }
    }

    fn batch(&self, entrant: usize) -> Result<&Predictions> {
        self.models[entrant]
            .predictions
            .as_ref()
            .ok_or(Error::Invalid("unexecuted entrant measured"))
    }

    /// The entrant against one archived fingerprint on the probe.
    fn compare(
        &self,
        entrant: usize,
        content: &Digest,
        fingerprint: &Predictions,
    ) -> Result<ProbeMatch> {
        let own = self.models[entrant]
            .probe_predictions()
            .ok_or(Error::Invalid("unexecuted entrant measured"))?;
        Ok(ProbeMatch {
            content: *content,
            version: PROBE_VERSION,
            stats: reward_v1::near_stats(own, fingerprint)?,
            entrant_probe_score: reward_v1::document_score(&graded(own))?,
            public_probe_score: reward_v1::document_score(&graded(fingerprint))?,
        })
    }
}

impl Measurements for ComputedMeasure<'_> {
    fn current(&mut self, entrant: usize) -> Result<NearStats> {
        reward_v1::near_stats(self.batch(entrant)?, &self.current)
    }
    fn predecessor(&mut self, entrant: usize) -> Result<NearStats> {
        reward_v1::near_stats(self.batch(entrant)?, &self.previous)
    }
    fn reference_probe(
        &mut self,
        entrant: usize,
        content: &Digest,
        version: u32,
    ) -> Result<ProbeMatch> {
        if version != PROBE_VERSION {
            return Err(Error::Mismatch("fixture probe version"));
        }
        let (_, fingerprint) = self
            .references
            .iter()
            .chain(&self.archive)
            .find(|(c, _)| c == content)
            .ok_or(Error::Invalid("reference not fingerprinted"))?;
        self.compare(entrant, content, fingerprint)
    }
    fn archive_probe(&mut self, entrant: usize) -> Result<ArchiveProbe> {
        let candidates = self
            .archive
            .iter()
            .map(|(content, fingerprint)| self.compare(entrant, content, fingerprint))
            .collect::<Result<Vec<_>>>()?;
        Ok(ArchiveProbe {
            min_distance_ppb: candidates
                .iter()
                .filter_map(|m| m.stats.distance_ppb())
                .min(),
            candidates,
        })
    }
    fn pair(&mut self, head: usize, other: usize) -> Result<NearStats> {
        reward_v1::near_stats(self.batch(other)?, self.batch(head)?)
    }
    fn promotion_audit(&mut self, head: usize) -> Result<PromotionAudit> {
        Ok(promotion_audit(
            self.models[head].spec.identity().hotkey,
            true,
            0,
        ))
    }
}

/// The tempo of the cohort at which the computed cohort's publication
/// completes and its transition begins, as in the paper's phase table.
pub const PUBLISHED_TEMPO: u64 = 16;

/// Everything the coordinator produces for the computed cohort.
pub struct Computed {
    pub models: Vec<Model>,
    pub specs: Vec<Spec>,
    /// The published index as of the cohort's seal.
    pub index: PublishedIndex,
    pub manifest: CohortManifestV2,
    pub package: ScorePackageV2,
    pub evaluation: Evaluation,
    pub target: CalculationTargetV2,
    /// Carried with the package, issued at publication (tempo 16).
    pub authorization: Signed<RewardIntentAuthorization>,
    /// The coordinator's renewal at tempo 30 of the cohort.
    pub renewal: Signed<RewardIntentRenewal>,
    pub closure: CohortClosure,
}

impl Computed {
    /// The closure checked against the cohort's manifest, package and target.
    pub fn checked_closure(&self) -> Result<CheckedClosure<'_>> {
        self.closure
            .check(&self.manifest, &self.package, Some(&self.target))
    }

    /// The validator's active target before the closure: the launch burn row,
    /// the computed target's predecessor.
    pub fn active_before(&self) -> Result<ActiveTarget> {
        ActiveTarget::launch(&chain())
    }

    /// The active target from the closing boundary on: the computed target.
    pub fn active_after(&self) -> Result<ActiveTarget> {
        ActiveTarget::activated(&self.checked_closure()?)
    }

    /// The authorization checked against the cohort's manifest, package and
    /// target, with the computed target active.
    pub fn checked_authorization(&self) -> Result<CheckedAuthorization<'_>> {
        RewardIntentAuthorization::check(
            &self.authorization,
            &trusted_keys(),
            &self.manifest,
            &self.package,
            &self.target,
            &self.active_after()?,
        )
    }

    /// The tempo-30 renewal checked against the checked authorization.
    pub fn checked_renewal<'a>(
        &'a self,
        authorization: &CheckedAuthorization<'_>,
    ) -> Result<CheckedRenewal<'a>> {
        RewardIntentRenewal::check(&self.renewal, &trusted_keys(), authorization)
    }
}

/// A cohort after the computed one whose entrant beats its reference by the
/// margin, so that its target, the computed target's successor, activates at
/// its closing boundary (epoch 100), inside the computed authorization's
/// lease.
pub struct Successor {
    pub manifest: CohortManifestV2,
    pub package: ScorePackageV2,
    pub target: CalculationTargetV2,
    pub closure: CohortClosure,
}

pub fn successor_of(c: &Computed) -> Result<Successor> {
    let specs = [Spec::new(0x92, bp(9900))];
    let manifest = self::manifest(chain(), 4, 1000, sigma_permille(40), 2, &specs);
    let (package, _) = finalized(&manifest, bp(9600), Some(bp(9100)), &specs)?;
    if package.mode != CutoffMode::Margin {
        return Err(Error::Invalid("fixture successor"));
    }
    let target = CalculationTargetV2::for_package(&package, Some(c.target.digest()?))?;
    let closure = closure(&manifest, &package, Some(&target), &specs)?;
    Ok(Successor {
        manifest,
        package,
        target,
        closure,
    })
}

/// The signed closure of `package` at the nominal phases: admission ends at
/// tempo 1 and publication at tempo 16, then the cohort closes at its minimum
/// `K = 20`.
pub fn closure(
    manifest: &CohortManifestV2,
    package: &ScorePackageV2,
    target: Option<&CalculationTargetV2>,
    specs: &[Spec],
) -> Result<CohortClosure> {
    Ok(CohortClosure {
        chain: manifest.chain.clone(),
        cohort: manifest.cohort,
        cohort_label: manifest.cohort_label.clone(),
        cohort_manifest: manifest.digest()?,
        score_package: package.digest()?,
        target: target.map(Record::digest).transpose()?,
        admission_closed_epoch: manifest.earliest_admission_close_epoch()?,
        published_epoch: manifest.opened_epoch + PUBLISHED_TEMPO,
        closed_epoch: manifest.earliest_close_epoch()?,
        openings: specs
            .iter()
            .map(|s| PublishedOpening {
                hotkey: hotkey(s.tag),
                registration_block: s.identity().registration_block,
                nonce: nonce(s.tag),
            })
            .collect(),
    })
}

pub fn computed() -> Result<Computed> {
    let models = models();
    let specs: Vec<Spec> = models.iter().map(|m| m.spec.clone()).collect();
    let index = computed_index()?;
    let mut manifest = manifest_with(
        chain(),
        3,
        CASES,
        sigma_permille(40),
        references(1),
        &index,
        probe(CASES),
        &specs,
    );
    manifest.batch.source_groups = 8;
    let (current, previous) = reference_predictions();
    let reference = reward_v1::reference_score(&graded(&current))?;
    let predecessor = reward_v1::reference_score(&graded(&previous))?;
    let mut package = package(&manifest, reference, Some(predecessor), &specs)?;
    let evaluation = reward_v1::finalize(&mut package, &index, &mut ComputedMeasure::new(&models))?;
    let launch = CalculationTargetV2::launch_burn(chain());
    let target = CalculationTargetV2::for_package(&package, Some(launch.digest()?))?;
    let closure = closure(&manifest, &package, Some(&target), &specs)?;
    let authorization = signed(authorization(
        &manifest,
        &package,
        &target,
        1,
        manifest.opened_epoch + PUBLISHED_TEMPO,
    )?)?;
    let renewal = {
        let checked = closure.check(&manifest, &package, Some(&target))?;
        let checked = RewardIntentAuthorization::check(
            &authorization,
            &trusted_keys(),
            &manifest,
            &package,
            &target,
            &ActiveTarget::activated(&checked)?,
        )?;
        signed(RewardIntentRenewal::new(
            &checked,
            2,
            manifest.opened_epoch + 30,
        )?)?
    };
    Ok(Computed {
        models,
        specs,
        index,
        manifest,
        package,
        evaluation,
        target,
        authorization,
        renewal,
        closure,
    })
}

/// The fingerprints a closure of the computed cohort records: every scored
/// entrant's probe predictions on the probe version in use, gate-failed
/// entrants included.
pub fn closure_fingerprints(c: &Computed) -> Vec<EntrantFingerprint> {
    c.models
        .iter()
        .zip(&c.package.entrants)
        .filter(|(_, e)| e.score.is_some())
        .map(|(m, e)| EntrantFingerprint {
            hotkey: e.identity.hotkey,
            version: PROBE_VERSION,
            fingerprint: prediction_digest(m.probe_predictions().expect("scored")),
        })
        .collect()
}

/// A cohort with its index, manifest, finalized package and outcome.
pub struct Scenario {
    pub models: Vec<Model>,
    pub index: PublishedIndex,
    pub manifest: CohortManifestV2,
    pub package: ScorePackageV2,
    pub evaluation: Evaluation,
}

fn scored(name: &'static str, tag: u8, batch: Predictions, probe: Option<Predictions>) -> Model {
    let mut spec = Spec::new(tag, Score::ZERO);
    spec.score = Some(reward_v1::document_score(&graded(&batch)).expect("scored"));
    spec.failed = batch.iter().filter(|x| x.is_none()).count() as u32;
    Model {
        name,
        spec,
        predictions: Some(batch),
        probe,
    }
}

/// Seals, scores and finalizes a computed cohort against `index`, with
/// `current` and `previous` the hidden batch predictions of `R_k` and `R_k-1`,
/// `archive` the fingerprinted artifacts' probe predictions that the archive
/// comparison covers, and `skipped` reference fingerprints that it leaves out.
fn scenario(
    cohort: u64,
    references: ReferenceSet,
    index: PublishedIndex,
    models: Vec<Model>,
    (current, previous): (Predictions, Predictions),
    (archive, skipped): (Fingerprints, Fingerprints),
) -> Result<Scenario> {
    let specs: Vec<Spec> = models.iter().map(|m| m.spec.clone()).collect();
    let mut manifest = manifest_with(
        chain(),
        cohort,
        CASES,
        sigma_permille(40),
        references,
        &index,
        probe(CASES),
        &specs,
    );
    manifest.batch.source_groups = 8;
    let reference = reward_v1::reference_score(&graded(&current))?;
    let predecessor = reward_v1::reference_score(&graded(&previous))?;
    let mut package = package(&manifest, reference, Some(predecessor), &specs)?;
    let mut measure = ComputedMeasure {
        models: &models,
        current,
        previous,
        archive,
        references: skipped,
    };
    let evaluation = reward_v1::finalize(&mut package, &index, &mut measure)?;
    Ok(Scenario {
        models,
        index,
        manifest,
        package,
        evaluation,
    })
}

/// `R_k-1` outscores `R_k` by more than the promotion margin on this batch:
/// `R_1` errs by 0.35 and `R_0` by 0.30, so `S_R0 = 0.91` clears
/// `T = 0.8873`. A repackaged `R_0` (new content, identical predictions)
/// scores exactly `S_Rk-1`; it fails `public.near.predecessor` and is neither
/// promoted nor paid. With `independent`, an unrelated model at 0.9159 is
/// promoted instead; without it, nobody is gated and the cohort produces no
/// target.
pub fn predecessor_above_margin(independent: bool) -> Result<Scenario> {
    let (better, worse) = reference_predictions();
    let mut models = vec![scored("r0_repackaged", 0x41, better.clone(), None)];
    if independent {
        models.push(scored(
            "independent",
            0x42,
            predictions(&textured(290_000_000, &[4, 12])),
            None,
        ));
    }
    let fingerprinted = [(0xe0, better.clone()), (0xe1, worse.clone())];
    let index = PublishedIndex::new(
        fingerprinted.iter().map(|(tag, _)| content(*tag)),
        fingerprinted.iter().map(|(tag, p)| Fingerprint {
            content: content(*tag),
            version: PROBE_VERSION,
            fingerprint: prediction_digest(p),
        }),
    )?;
    let archive = fingerprinted
        .iter()
        .map(|(tag, p)| (content(*tag), p.clone()))
        .collect();
    scenario(
        5,
        references(1),
        index,
        models,
        (worse, better),
        (archive, Vec::new()),
    )
}

/// A perturbed copy of `R_k` that escapes the batch test, `0.084` from `R_k`
/// there at `R_k`'s own score, and predicts on the probe exactly as `R_k`'s
/// fingerprint. The coordinator's archive comparison here leaves both
/// references out; `finalize` compares them anyway, and the copy fails
/// `public.near.probe` against `R_k` itself.
pub fn current_copy_on_probe() -> Result<Scenario> {
    let (current, previous) = reference_predictions();
    let models = vec![scored(
        "rk_probe_copy",
        0x43,
        predictions(&textured(300_000_000, &[0, 16])),
        Some(current.clone()),
    )];
    let fingerprinted = [(0xe0, previous.clone()), (0xe1, current.clone())];
    let index = PublishedIndex::new(
        fingerprinted.iter().map(|(tag, _)| content(*tag)),
        fingerprinted.iter().map(|(tag, p)| Fingerprint {
            content: content(*tag),
            version: PROBE_VERSION,
            fingerprint: prediction_digest(p),
        }),
    )?;
    let skipped = fingerprinted
        .iter()
        .map(|(tag, p)| (content(*tag), p.clone()))
        .collect();
    scenario(
        6,
        references(1),
        index,
        models,
        (current, previous),
        (Vec::new(), skipped),
    )
}

/// The cohort after the computed one, `R_2 = alpha` with `R_1` as its
/// predecessor. `kappa` scored below `S_Rk-1` in the computed cohort; its
/// repackaged copy meets new documents here and scores 0.9559, above `S_R1`
/// and short of `alpha`'s margin, while it predicts on the probe exactly as
/// `kappa` did. With `kappa`'s fingerprint, which the closure records because
/// `kappa` was executed, the copy fails `public.near.probe`. Without it
/// (`fingerprint_kappa = false`, an index no closure produces), the copy would
/// be gated and take the whole fallback row.
pub fn later_copy_of_a_low_scorer(fingerprint_kappa: bool) -> Result<Scenario> {
    let c = computed()?;
    let kappa = c
        .models
        .iter()
        .position(|m| m.name == "kappa")
        .ok_or(Error::Invalid("fixture model"))?;
    let kappa_probe = c.models[kappa]
        .probe_predictions()
        .ok_or(Error::Invalid("fixture model"))?
        .clone();
    let kappa_content = c.package.entrants[kappa].content;
    let fingerprints = closure_fingerprints(&c);
    let index = if fingerprint_kappa {
        c.index.after_closure(&c.package, fingerprints)?
    } else {
        let mut fingerprints = crate::canonical_fingerprints(&c.package, &fingerprints)?;
        fingerprints.retain(|f| f.content != kappa_content);
        PublishedIndex::new(
            c.index
                .contents
                .contents
                .iter()
                .copied()
                .chain(c.package.entrants.iter().map(|e| e.content)),
            c.index
                .fingerprints
                .entries
                .iter()
                .copied()
                .chain(fingerprints),
        )?
    };
    let references = reward_v1::next_references(&c.manifest.references, &c.package, &c.index)?;
    let (current, _) = reference_predictions();
    let alpha = c.models[0]
        .predictions
        .clone()
        .ok_or(Error::Invalid("fixture model"))?;
    let mut archive = ComputedMeasure::new(&c.models).archive;
    for (m, e) in c.models.iter().zip(&c.package.entrants) {
        if e.score.is_some() && (fingerprint_kappa || e.content != kappa_content) {
            archive.push((e.content, m.probe_predictions().expect("scored").clone()));
        }
    }
    let models = vec![scored(
        "kappa_repackaged",
        0x51,
        predictions(&textured(210_000_000, &[0, 16])),
        Some(kappa_probe),
    )];
    scenario(
        4,
        references,
        index,
        models,
        (alpha, current),
        (archive, Vec::new()),
    )
}

/// The validator's view at payout: the owner at UID 2, `alpha` moved to UID
/// 0x21 under the same registration, `iota` deregistered, and
/// `beta_copy_2`'s hotkey re-registered as a new generation.
pub fn computed_snapshot(specs: &[Spec]) -> RegistrationSnapshotV2 {
    let mut s = snapshot(chain(), 2, specs);
    s.neurons.retain(|n| n.hotkey != hotkey(0x13));
    for n in &mut s.neurons {
        if n.hotkey == hotkey(0x11) {
            n.uid = 0x21;
        }
        if n.hotkey == hotkey(0x1c) {
            n.registration_block = 7777;
        }
    }
    s.neurons.sort_by_key(|n| n.uid);
    s
}

/// The computed cohort's payout view after the subnet owner hotkey rotated to
/// `0x77`, registered at UID `0x30`. The former owner hotkey stays registered
/// at UID 2 as an ordinary neuron.
pub fn rotated_owner_snapshot(specs: &[Spec]) -> RegistrationSnapshotV2 {
    let mut s = computed_snapshot(specs);
    s.owner_hotkey = hotkey(0x77);
    s.neurons.push(NeuronIdentity {
        uid: 0x30,
        hotkey: hotkey(0x77),
        registration_block: 8000,
    });
    s.neurons.sort_by_key(|n| n.uid);
    s
}

/// Each retained cluster's head and exact credit, as the package lists them.
fn credit_list(package: &ScorePackageV2) -> String {
    package
        .credits
        .iter()
        .map(|c| format!("{:02x}:{}", c.head.0[0], c.fraction()))
        .collect::<Vec<_>>()
        .join(",")
}

// ---------------------------------------------------------------------------
// Golden report
// ---------------------------------------------------------------------------

struct Report(String);

impl Report {
    fn line(&mut self, key: &str, value: impl std::fmt::Display) {
        writeln!(self.0, "{key}={value}").expect("string write");
    }
    fn result<T: std::fmt::Display>(&mut self, key: &str, value: Result<T>) {
        match value {
            Ok(v) => self.line(key, v),
            Err(e) => self.line(key, format_args!("error:{}", e.code())),
        }
    }
    fn code<T>(&mut self, key: &str, value: Result<T>) {
        let code = match value {
            Ok(_) => "ok",
            Err(e) => e.code(),
        };
        self.line(key, code);
    }
    fn record<R: Record>(&mut self, key: &str, record: &R) -> Result<()> {
        self.line(
            &format!("{key}.bytes_sha256"),
            object_sha256(&record.to_bytes()?),
        );
        self.line(&format!("{key}.digest"), record.digest()?);
        Ok(())
    }
    fn signed<R: Record>(&mut self, key: &str, signed: &Signed<R>) -> Result<()> {
        self.line(
            &format!("{key}.signing_payload"),
            hex(&signing_payload(&signed.record)?),
        );
        self.line(&format!("{key}.signature"), hex(&signed.signature));
        Ok(())
    }
    fn row(&mut self, key: &str, row: &WeightRowV2) -> Result<()> {
        self.line(&format!("{key}.entries"), row_entries(row));
        self.line(
            &format!("{key}.owner_hotkey"),
            format_args!("{:02x}", row.owner_hotkey.0[0]),
        );
        self.line(&format!("{key}.digest"), row.digest()?);
        Ok(())
    }
    fn outcome(&mut self, key: &str, package: &ScorePackageV2) {
        let units: Vec<_> = package
            .entrants
            .iter()
            .filter(|e| e.units > 0)
            .map(|e| format!("{:02x}:{}", e.identity.hotkey.0[0], e.units))
            .collect();
        self.line(&format!("{key}.mode"), format_args!("{:?}", package.mode));
        self.line(&format!("{key}.units"), units.join(","));
    }
}

pub fn row_entries(row: &WeightRowV2) -> String {
    row.entries
        .iter()
        .map(|e| format!("{}:{}", e.uid, e.weight))
        .collect::<Vec<_>>()
        .join(",")
}

/// A share as an exact reduced fraction and in parts per million (floored).
fn share(f: &crate::bigint::Fraction) -> String {
    format!("{} ppm={}", f.reduced(), f.floor_scaled(1_000_000))
}

/// Builds the `reward-v1` golden report.
pub fn golden_report() -> Result<String> {
    let mut r = Report(String::new());
    r.line("schema", "slop_ninja.protocol.golden/reward-v1");
    parameters(&mut r)?;
    near_copies(&mut r)?;
    computed_cohort(&mut r)?;
    copy_scenarios(&mut r)?;
    reward_intent(&mut r)?;
    submission(&mut r)?;
    artifacts(&mut r)?;
    worked_examples(&mut r)?;
    edge_cases(&mut r)?;
    rejections(&mut r)?;
    promotion(&mut r)?;
    Ok(r.0)
}

/// The launch mix, the promotion audits of a margin cohort with three heads
/// above the margin, and a canary clearance.
fn promotion(r: &mut Report) -> Result<()> {
    r.line(
        "mix.launch",
        format_args!(
            "{}/{}/{} valid={}",
            crate::LAUNCH_ORIGIN_MIX.human_only,
            crate::LAUNCH_ORIGIN_MIX.mixed,
            crate::LAUNCH_ORIGIN_MIX.model_only,
            crate::LAUNCH_ORIGIN_MIX.validate().is_ok()
        ),
    );
    let specs = [
        Spec::new(0x71, bp(9600)),
        Spec::new(0x72, bp(9500)),
        Spec::new(0x73, bp(9400)),
    ];
    type Audits<'a> = &'a [(usize, bool, u8)];
    let cases: [(&str, Audits<'_>); 5] = [
        ("clean", &[]),
        ("one_flag", &[(0, true, 1)]),
        ("two_flags", &[(0, true, 2)]),
        ("strict_failure", &[(0, false, 0), (1, true, 2)]),
        (
            "none_qualifies",
            &[(0, true, 2), (1, false, 0), (2, false, 1)],
        ),
    ];
    for (name, audits) in cases {
        let m = manifest(chain(), 7, 1000, sigma_permille(40), 0, &specs);
        let index = default_index(&m.references);
        let mut p = package(&m, bp(9100), None, &specs)?;
        let mut measure = spec_measure(&specs, 1000, |_, _| false);
        for &(i, strict, flags) in audits {
            measure.audits.insert(i, (strict, flags));
        }
        reward_v1::finalize(&mut p, &index, &mut measure)?;
        let walked: Vec<String> = p
            .promotion_audits
            .iter()
            .map(|a| {
                format!(
                    "{:02x}:strict={}:flags={}:blocked={}",
                    a.head.0[0],
                    a.strict_fpr_passed,
                    a.canary.iter().filter(|c| c.flag.is_some()).count(),
                    a.blocked()
                )
            })
            .collect();
        r.line(&format!("promotion.{name}.audits"), walked.join(","));
        r.line(
            &format!("promotion.{name}.promoted"),
            p.promoted
                .map_or("none".into(), |h| format!("{:02x}", h.0[0])),
        );
        r.outcome(&format!("promotion.{name}"), &p);
        r.record(&format!("promotion.{name}.package"), &p)?;
        if name == "two_flags" {
            let clearance = crate::CanaryClearance {
                chain: p.chain.clone(),
                cohort: p.cohort,
                cohort_manifest: p.cohort_manifest,
                score_package: p.digest()?,
                head: hotkey(0x71),
                review: Digest([0x5a; 32]),
                cleared_epoch: 900,
            };
            clearance.check(&p)?;
            r.record("promotion.clearance", &clearance)?;
        }
    }
    Ok(())
}

fn parameters(r: &mut Report) -> Result<()> {
    for permille in [3u64, 20, 28, 40] {
        let p = RewardV1Params::new(sigma_permille(permille));
        r.line(&format!("params.sigma_{permille}e-3.delta"), p.margin());
    }
    for (name, reference) in [("0.9", bp(9000)), ("0.9482", bp(9482)), ("1", Score::ONE)] {
        let p = RewardV1Params::new(sigma_permille(40));
        match reward_v1::Bands::new(&p, reference) {
            Some(b) => r.line(
                &format!("params.w.sigma_40e-3.reference_{name}"),
                format_args!("{} epsilon_x250={}", b.width_fraction().reduced(), b.width),
            ),
            None => r.line(&format!("params.w.sigma_40e-3.reference_{name}"), "none"),
        }
    }
    let p = RewardV1Params::new(sigma_permille(40));
    r.line(
        "params.near_copy",
        format_args!(
            "d_ppb={} improvement_divisor={}",
            p.near_copy_ppb, p.improvement_divisor
        ),
    );
    r.record("content.alpha", &inference_content(0x11))?;
    Ok(())
}

/// `x` from parts per billion; `0` marks a failed document.
fn ppb_vector(values: &[u32]) -> Predictions {
    values
        .iter()
        .map(|&v| (v > 0).then(|| Probability::new(v).expect("fixture probability")))
        .collect()
}

fn stats_line(stats: &NearStats, d_ppb: u32) -> Result<String> {
    Ok(format!(
        "common={} fwd={} rev={} distance_ppb={} within_d={}",
        stats.common,
        stats.rss_fwd,
        stats.rss_rev,
        stats
            .distance_ppb()
            .map_or("none".into(), |d| d.to_string()),
        stats.within(d_ppb)?
    ))
}

/// The monotone-aligned distance on fixed synthetic vectors.
fn near_copies(r: &mut Report) -> Result<()> {
    let d = NEAR_COPY_PPB;
    let y = [
        100_000_000,
        200_000_000,
        300_000_000,
        400_000_000,
        600_000_000,
        900_000_000,
    ];
    // A convex, strictly increasing remap of y: a recalibration.
    let remapped: Vec<u32> = y
        .iter()
        .map(|&v: &u32| (u64::from(v) * u64::from(v) / 1_000_000_000) as u32 + 50_000_000)
        .collect();
    let reversed: Vec<u32> = y.iter().rev().copied().collect();
    let cases: [(&str, Vec<u32>, Vec<u32>); 6] = [
        ("identical", y.to_vec(), y.to_vec()),
        ("recalibrated", remapped, y.to_vec()),
        (
            "ties",
            vec![
                150_000_000,
                250_000_000,
                200_000_000,
                600_000_000,
                640_000_000,
                620_000_000,
            ],
            vec![
                200_000_000,
                200_000_000,
                200_000_000,
                600_000_000,
                600_000_000,
                600_000_000,
            ],
        ),
        ("reversed", reversed, y.to_vec()),
        (
            "failures_left_out",
            vec![100_000_000, 0, 300_000_000, 400_000_000, 0, 900_000_000],
            vec![
                100_000_000,
                800_000_000,
                300_000_000,
                0,
                600_000_000,
                900_000_000,
            ],
        ),
        (
            "no_common_document",
            vec![100_000_000, 0, 0],
            vec![0, 200_000_000, 300_000_000],
        ),
    ];
    for (name, x, y) in cases {
        let stats = reward_v1::near_stats(&ppb_vector(&x), &ppb_vector(&y))?;
        r.line(&format!("near.{name}"), stats_line(&stats, d)?);
    }
    // The score clause: within d, x is a near copy of y unless it scores more
    // than w / 2 above it. sigma = 0.04 and S_y = 0.91: w = 0.0072.
    let params = RewardV1Params::new(sigma_permille(40));
    let width = reward_v1::band_width(&params, bp(9100));
    for (name, x) in [
        ("below", bp(9000)),
        ("equal", bp(9100)),
        ("half_band_above", bp(9136)),
        ("past_half_band", Score::new(bp(9136).get() + 1)?),
    ] {
        r.line(
            &format!("near.score_clause.{name}"),
            format_args!(
                "outscores={} near={}",
                reward_v1::outscores(x, bp(9100), &params, width)?,
                reward_v1::near(&aligned(1000), x, bp(9100), &params, width)?
            ),
        );
    }
    Ok(())
}

fn computed_cohort(r: &mut Report) -> Result<()> {
    let c = computed()?;
    let binding = c.index.binding()?;
    r.line(
        "cohort.index",
        format_args!(
            "contents={} content_root={} fingerprints={} fingerprint_root={}",
            binding.content_count,
            binding.content_root,
            binding.fingerprint_count,
            binding.fingerprint_root
        ),
    );
    r.record("cohort.manifest", &c.manifest)?;
    r.line("cohort.reference", c.package.reference.score.get());
    r.line(
        "cohort.predecessor",
        c.package.predecessor.as_ref().map_or(0, |p| p.score.get()),
    );
    for (m, e) in c.models.iter().zip(&c.package.entrants) {
        r.line(
            &format!("cohort.entrant.{}", m.name),
            format_args!(
                "score={} gated={} head={} units={}",
                e.score.map_or("none".into(), |s| s.get().to_string()),
                e.gated(),
                e.cluster_head
                    .map_or("none".into(), |h| format!("{:02x}", h.0[0])),
                e.units
            ),
        );
    }
    // The public-copy gate: every entrant with a recorded public-copy result.
    let distance = |s: &NearStats| s.distance_ppb().map_or("none".into(), |d| d.to_string());
    for (m, e) in c.models.iter().zip(&c.package.entrants) {
        let gate = e.public_copy().map_or("pass", PublicCopy::reason);
        let line = match &e.public {
            None if e.public_copy().is_none() => continue,
            None => format!("gate={gate}"),
            Some(p) => format!(
                "gate={gate} current={} predecessor={} probe_references={} probe={}",
                distance(&p.current),
                p.predecessor.as_ref().map_or("none".into(), distance),
                p.reference_probes
                    .iter()
                    .map(|m| distance(&m.stats))
                    .collect::<Vec<_>>()
                    .join(","),
                p.probe_match.as_ref().map_or("none".into(), |found| {
                    let tag = [0xe0, 0xe1, ARCHIVED]
                        .into_iter()
                        .find(|&t| content(t) == found.content)
                        .map_or("?".into(), |t| format!("{t:02x}"));
                    format!(
                        "{tag}@v{} distance={} scores={}/{}",
                        found.version,
                        distance(&found.stats),
                        found.entrant_probe_score.get(),
                        found.public_probe_score.get()
                    )
                })
            ),
        };
        r.line(&format!("cohort.public.{}", m.name), line);
    }
    // The recalibrated copy's raw RMS from R_k: a raw-RMS tolerance tight
    // enough to keep independent models apart would not catch it.
    let (current, _) = reference_predictions();
    let recalibrated = c
        .models
        .iter()
        .find(|m| m.name == "rk_recalibrated")
        .and_then(|m| m.predictions.as_ref())
        .ok_or(Error::Invalid("fixture model"))?;
    let squares: u128 = recalibrated
        .iter()
        .zip(&current)
        .map(|(a, b)| u128::from(a.unwrap().ppb().abs_diff(b.unwrap().ppb())).pow(2))
        .sum();
    r.line(
        "cohort.rk_recalibrated.raw_rms_ppb",
        (squares / u128::from(CASES)).isqrt(),
    );
    // The clustering-critical pairs: independent heads stay apart.
    let measure_models = &c.models;
    let pair = |a: &str, b: &str| -> Result<String> {
        let find = |n: &str| {
            measure_models
                .iter()
                .find(|m| m.name == n)
                .and_then(|m| m.predictions.as_ref())
                .ok_or(Error::Invalid("fixture model"))
        };
        let stats = reward_v1::near_stats(find(b)?, find(a)?)?;
        Ok(distance(&stats))
    };
    for (a, b) in [
        ("alpha", "alpha_close"),
        ("alpha", "iota"),
        ("alpha", "beta"),
        ("alpha", "gamma"),
        ("iota", "beta"),
        ("iota", "gamma"),
        ("beta", "gamma"),
    ] {
        r.line(&format!("cohort.pair.{a}.{b}"), pair(a, b)?);
    }
    for (m, e) in c.models.iter().zip(&c.package.entrants) {
        if let Some(stats) = &e.cluster_stats {
            r.line(&format!("cohort.cluster_stats.{}", m.name), distance(stats));
        }
    }
    let monitor = &c.package.distance_monitor;
    let show = |v: Option<u32>| v.map_or("none".into(), |v| v.to_string());
    r.line(
        "cohort.distance_monitor",
        format_args!(
            "min_entrant_pair_ppb={} min_gated_vs_current_ppb={} min_gated_vs_archive_ppb={}",
            show(monitor.min_entrant_pair_ppb),
            show(monitor.min_gated_vs_current_ppb),
            show(monitor.min_gated_vs_archive_ppb)
        ),
    );
    let ev = &c.evaluation;
    r.line("cohort.mode", format_args!("{:?}", ev.mode));
    r.line(
        "cohort.cutoff",
        ev.threshold.map_or("none".into(), |t| t.display()),
    );
    for o in &ev.clusters {
        let members: Vec<_> = o
            .cluster
            .members
            .iter()
            .map(|&i| format!("{:02x}", c.specs[i].tag))
            .collect();
        match &o.placement {
            Some(p) => r.line(
                &format!("cohort.cluster.{:02x}", c.specs[o.cluster.head].tag),
                format_args!(
                    "members={} band={} kappa={}/{} mu={}/{} credit={}",
                    members.join("+"),
                    p.band,
                    p.band_rank,
                    p.bands,
                    p.rank_in_band,
                    p.band_size,
                    share(&p.credit)
                ),
            ),
            None => r.line(
                &format!("cohort.cluster.{:02x}", c.specs[o.cluster.head].tag),
                format_args!("members={} dropped=near_equal", members.join("+")),
            ),
        }
    }
    r.line(
        "cohort.promoted",
        c.package
            .promoted
            .map_or("none".into(), |h| format!("{:02x}", h.0[0])),
    );
    r.line("cohort.package.credits", credit_list(&c.package));
    r.record("cohort.package", &c.package)?;
    r.record("cohort.target", &c.target)?;
    let snapshot = computed_snapshot(&c.specs);
    r.record("cohort.snapshot", &snapshot)?;
    let row = reward_v1::derive_row(&c.target, Some((&c.package, &c.index)), &snapshot)?;
    r.row("cohort.row", &row)?;
    let launch = CalculationTargetV2::launch_burn(chain());
    r.record("cohort.launch_burn_target", &launch)?;
    r.row(
        "cohort.launch_burn_row",
        &reward_v1::derive_row(&launch, None, &snapshot)?,
    )?;
    // An owner-hotkey rotation moves the burn destination at the next tempo
    // under the same targets.
    let rotated = rotated_owner_snapshot(&c.specs);
    r.record("cohort.owner_rotated.snapshot", &rotated)?;
    r.row(
        "cohort.owner_rotated.row",
        &reward_v1::derive_row(&c.target, Some((&c.package, &c.index)), &rotated)?,
    )?;
    r.row(
        "cohort.owner_rotated.launch_burn_row",
        &reward_v1::derive_row(&launch, None, &rotated)?,
    )?;
    let next = reward_v1::next_references(&c.manifest.references, &c.package, &c.index)?;
    r.line(
        "cohort.next_references",
        format_args!(
            "index={} current_is_alpha={} predecessor_is_r1={}",
            next.index,
            next.current.content == content(0x11),
            next.predecessor == Some(c.manifest.references.current)
        ),
    );
    // The next cohort's index: every entrant's content joins the published
    // set, and every executed entrant's probe predictions join the
    // fingerprint index. Identical contents with identical probe predictions
    // record one fingerprint.
    let next_index = c
        .index
        .after_closure(&c.package, closure_fingerprints(&c))?;
    let next_binding = next_index.binding()?;
    r.line(
        "cohort.next_index",
        format_args!(
            "contents={} content_root={} fingerprints={} fingerprint_root={}",
            next_binding.content_count,
            next_binding.content_root,
            next_binding.fingerprint_count,
            next_binding.fingerprint_root
        ),
    );
    // Identical content with divergent probe predictions (alpha, committed
    // first, and alpha_copy): the index records the earliest-committed
    // entrant's fingerprint, whichever of the two diverges.
    for (name, diverging) in [("later_copy", 0x16), ("earliest", 0x11)] {
        let mut fingerprints = closure_fingerprints(&c);
        for f in &mut fingerprints {
            if f.hotkey == hotkey(diverging) {
                f.fingerprint = fingerprint_digest(diverging);
            }
        }
        let recorded = c
            .index
            .after_closure(&c.package, fingerprints.iter().copied())?
            .fingerprints
            .entries
            .iter()
            .find(|f| f.content == content(0x11))
            .map(|f| f.fingerprint);
        let alpha = fingerprints
            .iter()
            .find(|f| f.hotkey == hotkey(0x11))
            .map(|f| f.fingerprint);
        r.line(
            &format!("cohort.next_index.divergent_{name}"),
            format_args!(
                "recorded_is_alpha={} same_index={}",
                recorded == alpha,
                c.index.after_closure(&c.package, fingerprints)? == next_index
            ),
        );
    }
    r.signed(
        "cohort.signed.manifest",
        &sign(c.manifest.clone(), &package_key())?,
    )?;
    r.signed(
        "cohort.signed.package",
        &sign(c.package.clone(), &package_key())?,
    )?;
    r.signed(
        "cohort.signed.target",
        &sign(c.target.clone(), &package_key())?,
    )?;
    r.signed(
        "cohort.signed.closure",
        &sign(c.closure.clone(), &package_key())?,
    )?;
    r.code(
        "cohort.closure.check",
        c.closure.check(&c.manifest, &c.package, Some(&c.target)),
    );
    let checked = c.checked_closure()?;
    for (i, spec) in c.specs.iter().enumerate() {
        let payload = opening(&chain(), c.manifest.cohort, spec).chain_payload();
        let artifact = VerifiedArtifact::from_bytes(&artifact_bytes(spec))?;
        let testnet = chain();
        let commitment = finalized_commitment(&testnet, spec, commitment_block(i), &payload);
        r.code(
            &format!("cohort.closure.entrant.{:02x}", spec.tag),
            checked.check_entrant(i, &commitment, &artifact),
        );
    }
    r.code(
        "cohort.reference.artifact",
        c.manifest
            .references
            .current
            .check_artifact(&verified(0xe1, 0xe1)),
    );
    r.record("cohort.authorization", &c.authorization.record)?;
    r.signed("cohort.signed.authorization", &c.authorization)?;
    // Checked while the launch burn row is still active (the new target's
    // predecessor) and from the closing boundary on (the new target itself).
    let keys = trusted_keys();
    r.code(
        "cohort.authorization.check.before_activation",
        RewardIntentAuthorization::check(
            &c.authorization,
            &keys,
            &c.manifest,
            &c.package,
            &c.target,
            &c.active_before()?,
        ),
    );
    r.code("cohort.authorization.check", c.checked_authorization());
    let authorized = c.checked_authorization()?;
    r.record("cohort.renewal", &c.renewal.record)?;
    r.signed("cohort.signed.renewal", &c.renewal)?;
    r.code("cohort.renewal.check", c.checked_renewal(&authorized));
    let renewal = c.checked_renewal(&authorized)?;
    let validator = AccountId([0x66; 32]);
    let epoch = c.closure.closed_epoch;
    let lease_end = c.authorization.record.expires_epoch;
    let renewed_end = c.renewal.record.expires_epoch;
    let active = c.active_after()?;
    for (name, renewal, e) in [
        ("activation", None, epoch),
        ("next_tempo", None, epoch + 1),
        ("lease_end", None, lease_end),
        ("past_lease", None, lease_end + 1),
        ("renewed", Some(&renewal), lease_end + 1),
        ("renewed_end", Some(&renewal), renewed_end),
        ("past_renewal", Some(&renewal), renewed_end + 1),
        ("before_activation", None, epoch - 1),
    ] {
        r.result(
            &format!("cohort.operation.{name}.epoch_{e}"),
            authorized
                .operation(renewal, &active, validator, e)
                .map(|o| o.id()),
        );
    }
    // The operation re-checks the validator's active target at every tempo:
    // a validator still on the launch burn row (a gap in its history) commits
    // nothing, and once a successor target activates, cohort 3's
    // authorization yields no further commitment inside its lease.
    r.code(
        &format!("cohort.operation.stale_active.epoch_{epoch}"),
        authorized.operation(None, &c.active_before()?, validator, epoch),
    );
    let successor = successor_of(&c)?;
    let next_closure = &successor.closure;
    let checked5 = next_closure.check(
        &successor.manifest,
        &successor.package,
        Some(&successor.target),
    )?;
    let after_successor = ActiveTarget::activated(&checked5)?;
    let e5 = next_closure.closed_epoch;
    r.code(
        &format!("cohort.operation.after_successor_activation.epoch_{e5}"),
        authorized.operation(Some(&renewal), &after_successor, validator, e5),
    );
    // A renewal issued after the lease lapsed takes effect only from its own
    // issue epoch.
    let late = signed(RewardIntentRenewal::new(&authorized, 3, lease_end + 10)?)?;
    let late = RewardIntentRenewal::check(&late, &keys, &authorized)?;
    for (name, e) in [("gap", lease_end + 5), ("resumed", lease_end + 10)] {
        r.result(
            &format!("cohort.operation.late_renewal.{name}.epoch_{e}"),
            authorized
                .operation(Some(&late), &active, validator, e)
                .map(|o| o.id()),
        );
    }
    Ok(())
}

/// The reward-intent lease across a cohort that produces no target, the
/// launch burn row carried by a no-target package, and the manifest-free
/// launch authorization before and after the first package.
/// The public-copy gate needs no earning floor: a repackaged `R_k-1` that
/// clears the margin cutoff, and a later copy of an executed entrant that
/// scored below `S_Rk-1` in its own cohort, both earn nothing.
fn copy_scenarios(r: &mut Report) -> Result<()> {
    let gate = |p: &ScorePackageV2, i: usize| {
        p.entrants[i]
            .public_copy()
            .map_or("pass", PublicCopy::reason)
    };
    let h1 = predecessor_above_margin(true)?;
    let p = &h1.package;
    let threshold = reward_v1::Threshold::margin(p.reference.score, p.params.margin())?;
    r.line(
        "scenario.predecessor_above_margin.scores",
        format_args!(
            "reference={} predecessor={} margin_cutoff={} copy={} independent={}",
            p.reference.score.get(),
            p.predecessor.as_ref().map_or(0, |x| x.score.get()),
            threshold.display(),
            p.entrants[0].score.map_or(0, Score::get),
            p.entrants[1].score.map_or(0, Score::get),
        ),
    );
    r.line("scenario.predecessor_above_margin.copy_gate", gate(p, 0));
    r.outcome("scenario.predecessor_above_margin", p);
    r.outcome(
        "scenario.predecessor_above_margin.copy_alone",
        &predecessor_above_margin(false)?.package,
    );
    let h2 = later_copy_of_a_low_scorer(true)?;
    let p = &h2.package;
    let found = p.entrants[0]
        .public
        .as_ref()
        .and_then(|x| x.probe_match.as_ref())
        .map_or("none".into(), |m| {
            let name = if m.content == content(0x23) {
                "kappa"
            } else {
                "?"
            };
            format!("{name}@v{}", m.version)
        });
    r.line(
        "scenario.later_copy.copy",
        format_args!(
            "score={} gate={} probe_match={found}",
            p.entrants[0].score.map_or(0, Score::get),
            gate(p, 0),
        ),
    );
    r.outcome("scenario.later_copy", p);
    r.outcome(
        "scenario.later_copy.without_fingerprint",
        &later_copy_of_a_low_scorer(false)?.package,
    );
    // The probe comparison includes R_k's own fingerprint: a copy of R_k that
    // escapes the batch test is caught on the probe, although the
    // coordinator's archive comparison left the references out.
    let h3 = current_copy_on_probe()?;
    let p = &h3.package;
    let copy = &p.entrants[0];
    let public = copy
        .public
        .as_ref()
        .ok_or(Error::Invalid("fixture public"))?;
    let found = public.probe_match.as_ref().map_or("none".into(), |m| {
        let name = if m.content == p.reference.model.content {
            "r_k"
        } else {
            "?"
        };
        format!("{name}@v{}", m.version)
    });
    r.line(
        "scenario.current_on_probe.copy",
        format_args!(
            "score={} batch_current={} gate={} probe_match={found}",
            copy.score.map_or(0, Score::get),
            public
                .current
                .distance_ppb()
                .map_or("none".into(), |d| d.to_string()),
            gate(p, 0),
        ),
    );
    r.outcome("scenario.current_on_probe", p);
    Ok(())
}

fn reward_intent(r: &mut Report) -> Result<()> {
    let c = computed()?;
    let keys = trusted_keys();
    let validator = AccountId([0x66; 32]);
    // Cohort 4 produces no target, so its package authorizes cohort 3's
    // target, which stays active under the fresh lease.
    let specs = [Spec::new(0x91, bp(9050))];
    let m4 = manifest(chain(), 4, 1000, sigma_permille(40), 2, &specs);
    let (p4, _) = finalized(&m4, bp(9600), Some(bp(9100)), &specs)?;
    let carried = signed(authorization(&m4, &p4, &c.target, 3, m4.opened_epoch + 16)?)?;
    let active = c.active_after()?;
    r.line("intent.no_target.mode", format_args!("{:?}", p4.mode));
    r.record("intent.no_target.authorization", &carried.record)?;
    r.code(
        "intent.no_target.check",
        RewardIntentAuthorization::check(&carried, &keys, &m4, &p4, &c.target, &active),
    );
    let authorized =
        RewardIntentAuthorization::check(&carried, &keys, &m4, &p4, &c.target, &active)?;
    let e = carried.record.expires_epoch;
    r.result(
        &format!("intent.no_target.operation.epoch_{e}"),
        authorized
            .operation(None, &active, validator, e)
            .map(|o| o.id()),
    );
    r.code(
        "intent.no_target.past_lease",
        authorized.operation(None, &active, validator, e + 1),
    );
    // A validator still on the launch burn row (it has not checked cohort 3's
    // closure) commits nothing under it.
    r.code(
        "intent.no_target.stale_active",
        authorized.operation(None, &ActiveTarget::launch(&chain())?, validator, e),
    );
    // Cohort 4's own checked closure names no target, so it activates
    // nothing; cohort 3's target stays active.
    let closure4 = closure(&m4, &p4, None, &specs)?;
    let checked4 = closure4.check(&m4, &p4, None)?;
    r.code(
        "intent.no_target.own_closure_activates_nothing",
        ActiveTarget::activated(&checked4),
    );
    // The launch burn row carried by a no-target package while no cohort
    // target has ever activated.
    let launch = CalculationTargetV2::launch_burn(chain());
    let unactivated = ActiveTarget::launch(&chain())?;
    let m0 = manifest(chain(), 0, 1000, sigma_permille(40), 0, &specs);
    let (p0, _) = finalized(&m0, bp(9100), None, &specs)?;
    let burn = signed(authorization(&m0, &p0, &launch, 1, m0.opened_epoch + 16)?)?;
    r.code(
        "intent.launch_burn.check",
        RewardIntentAuthorization::check(&burn, &keys, &m0, &p0, &launch, &unactivated),
    );
    r.result(
        "intent.launch_burn.operation.epoch_16",
        RewardIntentAuthorization::check(&burn, &keys, &m0, &p0, &launch, &unactivated)?
            .operation(None, &unactivated, validator, 16)
            .map(|o| o.id()),
    );
    // The manifest-free launch authorization, from the first tempo of cohort
    // 1 (epoch 20), before any manifest is sealed.
    let first = signed(LaunchAuthorization::new(&chain(), 1, 20)?)?;
    r.record("intent.launch.authorization", &first.record)?;
    r.signed("intent.launch.signed.authorization", &first)?;
    r.code(
        "intent.launch.check.before_first_package",
        LaunchAuthorization::check(&first, &keys, &chain(), &unactivated),
    );
    let launch_authorized = LaunchAuthorization::check(&first, &keys, &chain(), &unactivated)?;
    for (name, e) in [
        ("first_tempo", 20),
        ("lease_end", 60),
        ("past_lease", 61),
        ("before_issue", 19),
    ] {
        r.result(
            &format!("intent.launch.operation.{name}.epoch_{e}"),
            launch_authorized
                .operation(None, &unactivated, validator, e)
                .map(|o| o.id()),
        );
    }
    let renewal = signed(RewardIntentRenewal::new(&launch_authorized, 1, 21)?)?;
    r.record("intent.launch.renewal", &renewal.record)?;
    let renewal = RewardIntentRenewal::check(&renewal, &keys, &launch_authorized)?;
    r.result(
        "intent.launch.operation.renewed.epoch_61",
        launch_authorized
            .operation(Some(&renewal), &unactivated, validator, 61)
            .map(|o| o.id()),
    );
    // Cohort 1 publishes the first package at tempo 16 (epoch 36) with a new
    // target. Until that target activates at the closing boundary (epoch 40)
    // the burn row stays active and the launch authorization keeps serving.
    let first_specs = [Spec::new(0x23, bp(9600))];
    let m1 = manifest(chain(), 1, 1000, sigma_permille(40), 0, &first_specs);
    let (p1, _) = finalized(&m1, bp(9100), None, &first_specs)?;
    let t1 = CalculationTargetV2::for_package(&p1, Some(launch.digest()?))?;
    let closure1 = closure(&m1, &p1, Some(&t1), &first_specs)?;
    let checked1 = closure1.check(&m1, &p1, Some(&t1))?;
    r.line(
        "intent.launch.first_package.mode",
        format_args!("{:?}", p1.mode),
    );
    r.code(
        "intent.launch.check.after_first_package",
        LaunchAuthorization::check(&first, &keys, &chain(), &unactivated),
    );
    r.result(
        "intent.launch.operation.after_first_package.epoch_37",
        launch_authorized
            .operation(Some(&renewal), &unactivated, validator, 37)
            .map(|o| o.id()),
    );
    // From the closing boundary on, every validator refuses the launch
    // authorization and commits the first cohort target under its package's
    // authorization.
    let activated = ActiveTarget::activated(&checked1)?;
    r.code(
        "intent.launch.check.after_activation",
        LaunchAuthorization::check(&first, &keys, &chain(), &activated),
    );
    // A validator that kept the launch authorization checked before the
    // closing boundary commits the burn row no longer: the operation re-checks
    // the active target, inside the renewed lease.
    r.code(
        "intent.launch.operation.after_activation.epoch_40",
        launch_authorized.operation(Some(&renewal), &activated, validator, 40),
    );
    let a1 = signed(authorization(&m1, &p1, &t1, 2, m1.opened_epoch + 16)?)?;
    r.code(
        "intent.first_target.check.pending",
        RewardIntentAuthorization::check(&a1, &keys, &m1, &p1, &t1, &unactivated),
    );
    let first_target = RewardIntentAuthorization::check(&a1, &keys, &m1, &p1, &t1, &activated)?;
    r.result(
        "intent.first_target.operation.epoch_40",
        first_target
            .operation(None, &activated, validator, 40)
            .map(|o| o.id()),
    );
    Ok(())
}

/// Artifact objects: content identity is derived from the bytes.
fn artifacts(r: &mut Report) -> Result<()> {
    let object = artifact_object(0x11, 0x11);
    let artifact = VerifiedArtifact::from_bytes(&object)?;
    r.line("artifact.alpha.sha256", artifact.sha256);
    r.line("artifact.alpha.len", artifact.len);
    r.line("artifact.alpha.content", artifact.content);
    r.record("artifact.alpha.header", &artifact.header)?;
    let copy = verified(0x11, 0x16);
    r.line(
        "artifact.copy",
        format_args!(
            "same_object={} same_content={}",
            copy.sha256 == artifact.sha256,
            copy.content == artifact.content
        ),
    );
    // Streaming in one-byte chunks gives the same result.
    let mut reader = ArtifactReader::new();
    for byte in &object {
        reader.update(std::slice::from_ref(byte))?;
    }
    r.line("artifact.streamed_matches", reader.finish()? == artifact);
    let mut flipped = object.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    r.code(
        "artifact.reject.file_bytes",
        VerifiedArtifact::from_bytes(&flipped),
    );
    r.code(
        "artifact.reject.truncated",
        VerifiedArtifact::from_bytes(&object[..object.len() - 1]),
    );
    let mut trailing = object.clone();
    trailing.push(0);
    r.code(
        "artifact.reject.trailing",
        VerifiedArtifact::from_bytes(&trailing),
    );
    let mut huge = object.clone();
    huge[..4].copy_from_slice(&(crate::MAX_ARTIFACT_HEADER_LEN + 1).to_le_bytes());
    r.code(
        "artifact.reject.header_length",
        VerifiedArtifact::from_bytes(&huge),
    );
    r.code(
        "artifact.reject.no_graph",
        crate::artifact_object(CONTRACT, &[("tokenizer.json", FileRole::Tokenizer, b"t")]),
    );
    r.code(
        "artifact.reject.parent_segment",
        crate::artifact_object(CONTRACT, &[("../model.onnx", FileRole::Graph, b"g")]),
    );
    r.code(
        "artifact.reject.empty_file",
        crate::artifact_object(CONTRACT, &[("model.onnx", FileRole::Graph, b"")]),
    );
    r.code(
        "artifact.reject.duplicate_component",
        crate::artifact_object(
            CONTRACT,
            &[
                ("a.onnx", FileRole::Graph, b"g"),
                ("b.onnx", FileRole::Graph, b"g"),
            ],
        ),
    );
    Ok(())
}

fn opening_for(c: &Computed, index: usize) -> SubmissionOpening {
    opening(&chain(), c.manifest.cohort, &c.specs[index])
}

fn submission(r: &mut Report) -> Result<()> {
    let c = computed()?;
    let spec = &c.specs[0];
    let cohort = c.manifest.cohort;
    let chain = chain();
    let opening = opening(&chain, cohort, spec);
    r.line("submission.commitment", opening.commitment());
    r.line("submission.chain_payload", hex(&opening.chain_payload()));
    let bytes = artifact_bytes(spec);
    let artifact = VerifiedArtifact::from_bytes(&bytes)?;
    let upload = SubmissionUpload::new(opening.clone(), &bytes);
    r.record("submission.upload", &upload)?;
    let payload = opening.chain_payload();
    let finalized = finalized_commitment(&chain, spec, commitment_block(0), &payload);
    r.result(
        "submission.admit",
        upload.admit(cohort, &finalized, &artifact).map(|entry| {
            format!(
                "uid={} block={} extrinsic={} roster_entry={}",
                entry.identity.uid,
                entry.commitment_block,
                entry.extrinsic_index,
                entry == c.manifest.roster[0]
            )
        }),
    );
    let mut tampered = payload;
    tampered[20] ^= 1;
    r.code(
        "submission.admit.tampered_payload",
        upload.admit(
            cohort,
            &FinalizedCommitment {
                payload: &tampered,
                ..finalized
            },
            &artifact,
        ),
    );
    r.code(
        "submission.admit.other_artifact",
        upload.admit(cohort, &finalized, &verified(0x11, 0x16)),
    );
    // An envelope for a different object whose content is not the committed one.
    let other = artifact_object(0x12, spec.tag);
    let mismatched = SubmissionUpload::new(opening.clone(), &other);
    r.code(
        "submission.admit.uncommitted_content",
        mismatched.admit(cohort, &finalized, &VerifiedArtifact::from_bytes(&other)?),
    );
    let mut other_cohort = opening.clone();
    other_cohort.cohort += 1;
    let other_payload = other_cohort.chain_payload();
    r.code(
        "submission.admit.other_cohort_payload",
        upload.admit(
            cohort,
            &FinalizedCommitment {
                payload: &other_payload,
                ..finalized
            },
            &artifact,
        ),
    );
    // Admission binds the admitting cohort, the chain read and the committing
    // account to the opening.
    r.code(
        "submission.admit.into_other_cohort",
        upload.admit(cohort + 1, &finalized, &artifact),
    );
    let finney = finney();
    r.code(
        "submission.admit.other_chain",
        upload.admit(
            cohort,
            &FinalizedCommitment {
                chain: &finney,
                ..finalized
            },
            &artifact,
        ),
    );
    let mut stranger = spec.identity();
    stranger.hotkey = hotkey(0x12);
    r.code(
        "submission.admit.other_hotkey",
        upload.admit(
            cohort,
            &FinalizedCommitment {
                committer: stranger,
                ..finalized
            },
            &artifact,
        ),
    );
    let mut regenerated = spec.identity();
    regenerated.registration_block += 1;
    r.code(
        "submission.admit.other_generation",
        upload.admit(
            cohort,
            &FinalizedCommitment {
                committer: regenerated,
                ..finalized
            },
            &artifact,
        ),
    );
    r.code(
        "submission.admit.registered_after_commitment",
        upload.admit(
            cohort,
            &FinalizedCommitment {
                block: spec.identity().registration_block - 1,
                ..finalized
            },
            &artifact,
        ),
    );
    // A registration in the commitment's own block is admitted.
    r.code(
        "submission.admit.registered_in_commitment_block",
        upload.admit(
            cohort,
            &FinalizedCommitment {
                block: spec.identity().registration_block,
                ..finalized
            },
            &artifact,
        ),
    );
    let mut forged = upload.clone();
    forged.opening.nonce = Nonce([0; 32]);
    r.code("submission.upload.unbound_nonce", forged.to_bytes());
    r.code(
        "submission.payload.short",
        crate::submission::parse_chain_payload(&tampered[..40]),
    );
    r.code(
        "submission.published_artifact",
        crate::submission::check_published_artifact(&c.package, &hotkey(spec.tag), &artifact),
    );
    r.code(
        "submission.published_artifact.other_object",
        crate::submission::check_published_artifact(
            &c.package,
            &hotkey(spec.tag),
            &verified(0x11, 0x16),
        ),
    );
    // A package that names the object but asserts other content.
    let mut asserted = c.package.clone();
    asserted.entrants[0].content = content(0x12);
    r.code(
        "submission.published_artifact.asserted_content",
        crate::submission::check_published_artifact(&asserted, &hotkey(spec.tag), &artifact),
    );
    // Each validator checks every entrant against the finalized commitment
    // it reads itself: the payload, the committing registration and the
    // position that breaks rank-order ties.
    let checked = c.closure.check(&c.manifest, &c.package, Some(&c.target))?;
    r.code(
        "submission.entrant",
        checked.check_entrant(0, &finalized, &artifact),
    );
    let other_payload = opening_for(&c, 1).chain_payload();
    let mut moved = spec.identity();
    moved.uid += 1;
    for (name, commitment) in [
        (
            "other_payload",
            FinalizedCommitment {
                payload: &other_payload,
                ..finalized
            },
        ),
        (
            "other_chain",
            FinalizedCommitment {
                chain: &finney,
                ..finalized
            },
        ),
        (
            "other_hotkey",
            FinalizedCommitment {
                committer: stranger,
                ..finalized
            },
        ),
        (
            "other_generation",
            FinalizedCommitment {
                committer: regenerated,
                ..finalized
            },
        ),
        (
            "other_uid",
            FinalizedCommitment {
                committer: moved,
                ..finalized
            },
        ),
        (
            "other_block",
            FinalizedCommitment {
                block: finalized.block + 1,
                ..finalized
            },
        ),
        (
            "other_extrinsic",
            FinalizedCommitment {
                extrinsic_index: finalized.extrinsic_index + 1,
                ..finalized
            },
        ),
    ] {
        r.code(
            &format!("submission.entrant.{name}"),
            checked.check_entrant(0, &commitment, &artifact),
        );
    }
    r.code(
        "submission.entrant.other_artifact",
        checked.check_entrant(0, &finalized, &verified(0x11, 0x16)),
    );
    Ok(())
}

fn worked_examples(r: &mut Report) -> Result<()> {
    let s = |tag, score| Spec::new(tag, bp(score));
    let reference = bp(9100);
    let cases: Vec<(&str, Vec<Spec>)> = vec![
        ("lone_cluster", vec![s(0x21, 9600)]),
        ("two_clusters_two_bands", vec![s(0x21, 9600), s(0x22, 9400)]),
        ("two_clusters_one_band", vec![s(0x21, 9600), s(0x22, 9590)]),
        (
            "two_in_top_band_one_below",
            vec![s(0x21, 9600), s(0x22, 9590), s(0x23, 9400)],
        ),
        (
            "honest_and_six_perturbed_copies",
            (0..7).map(|i| s(0x21 + i, 9600 - u64::from(i))).collect(),
        ),
    ];
    for (name, specs) in cases {
        let p = example(reference, None, &specs)?;
        r.outcome(&format!("example.{name}"), &p);
        let ev = reward_v1::evaluate(&p, &example_index(&p))?;
        for o in ev.clusters.iter().filter(|o| o.placement.is_some()) {
            let placement = o.placement.as_ref().expect("placed");
            r.line(
                &format!("example.{name}.credit.{:02x}", specs[o.cluster.head].tag),
                share(&placement.credit),
            );
        }
    }
    // Fallback: R_k-1 scores 0.9400 and the top entrant 0.9500; cutoff 0.9415.
    // The exact copy of R_k fails the public-copy gate and earns nothing.
    let specs = vec![
        s(0x31, 9500),
        Spec::published_copy(0x32, 0xe1),
        s(0x33, 9450),
        s(0x34, 9420),
        s(0x35, 9410),
        s(0x36, 9300),
    ];
    let p = example(bp(9480), Some(bp(9400)), &specs)?;
    r.outcome("example.fallback", &p);
    r.line(
        "example.fallback.cutoff",
        reward_v1::cutoff(&p)?
            .threshold
            .map_or("none".into(), |t| t.display()),
    );
    // Just short of the margin: earns in a fallback cohort, not beside a passer.
    let short = s(0x41, 9159);
    let p = example(bp(9100), Some(bp(8775)), std::slice::from_ref(&short))?;
    r.outcome("example.short_of_margin.fallback", &p);
    let p = example(bp(9100), Some(bp(8775)), &[short, s(0x42, 9600)])?;
    r.outcome("example.short_of_margin.beside_passer", &p);
    // A perturbed copy just below its original, within epsilon, is dropped.
    let p = example(
        bp(9100),
        None,
        &[
            s(0x51, 9600),
            Spec::new(0x52, Score::new(bp(9600).get() - 10_000_000_000_000)?),
        ],
    )?;
    r.outcome("example.near_equal_drop", &p);
    Ok(())
}

fn edge_cases(r: &mut Report) -> Result<()> {
    let s = |tag, score| Spec::new(tag, bp(score));
    let reference = bp(9100);
    let three_in_one_band = [s(0x61, 9600), s(0x62, 9590), s(0x63, 9580)];
    r.outcome(
        "edge.all_in_one_band",
        &example(reference, None, &three_in_one_band)?,
    );
    let sybil = [
        s(0x71, 9600),
        s(0x72, 9600).copy_of(0x71),
        s(0x73, 9600).copy_of(0x71),
        s(0x74, 9600).copy_of(0x71),
    ];
    r.outcome(
        "edge.sybil_cluster_of_four",
        &example(reference, None, &sybil)?,
    );
    let sybil_and_honest = [
        s(0x71, 9600),
        s(0x72, 9600).copy_of(0x71),
        s(0x73, 9600).copy_of(0x71),
        s(0x75, 9400),
    ];
    r.outcome(
        "edge.sybil_cluster_beside_honest",
        &example(reference, None, &sybil_and_honest)?,
    );
    let tie = [s(0x81, 9600), s(0x82, 9600)];
    r.outcome(
        "edge.score_tie_independent",
        &example(reference, None, &tie)?,
    );
    let tie_rank = [s(0x83, 9600), s(0x82, 9600)];
    r.outcome(
        "edge.score_tie_commitment_order",
        &example(reference, None, &tie_rank)?,
    );
    let thirds = [
        s(0x84, 9600),
        s(0x85, 9600).copy_of(0x84),
        s(0x86, 9600).copy_of(0x84),
    ];
    r.outcome(
        "edge.equal_split_of_three",
        &example(reference, None, &thirds)?,
    );
    // No target: R_0 in force and nobody passes the margin.
    r.outcome(
        "edge.no_target.r0_no_margin",
        &example(reference, None, &[s(0x91, 9150)])?,
    );
    r.outcome(
        "edge.no_target.nobody_above_predecessor",
        &example(reference, Some(bp(9165)), &[s(0x92, 9150), s(0x93, 9165)])?,
    );
    r.outcome(
        "edge.no_target.empty_roster",
        &example(reference, Some(bp(9000)), &[])?,
    );
    r.outcome(
        "edge.no_target.perfect_reference",
        &example(Score::ONE, Some(bp(9000)), &[Spec::new(0x94, Score::ONE)])?,
    );
    r.outcome(
        "edge.no_target.all_gated_out",
        &example(
            reference,
            None,
            &[s(0x95, 9900).failing(Gate::Attribution, "attribution.open_set_rejection")],
        )?,
    );
    // Extreme parameters stay within checked u128 and exact credits.
    let big = manifest(chain(), 8, 1000, SCORE_SCALE, 1, &[s(0xa1, 10_000)]);
    let mut p = package(&big, Score::ZERO, Some(Score::ZERO), &[s(0xa1, 10_000)])?;
    let ev = reward_v1::finalize(
        &mut p,
        &default_index(&big.references),
        &mut spec_measure(&[s(0xa1, 10_000)], 1000, |_, _| false),
    )?;
    r.line(
        "edge.overflow.sigma_one",
        format_args!("{:?} {:?}", ev.mode, ev.bands),
    );
    let tiny_specs = [s(0xa2, 10_000), s(0xa3, 9999)];
    let tiny = manifest(chain(), 9, 1000, 1, 0, &tiny_specs);
    let mut p = package(&tiny, bp(9999), None, &tiny_specs)?;
    let ev = reward_v1::finalize(
        &mut p,
        &default_index(&tiny.references),
        &mut spec_measure(&tiny_specs, 1000, |_, _| false),
    )?;
    r.line(
        "edge.overflow.sigma_minimum",
        format_args!(
            "{:?} bands={}",
            ev.mode,
            ev.clusters
                .iter()
                .filter_map(|o| o.placement.as_ref().map(|p| p.band.to_string()))
                .collect::<Vec<_>>()
                .join(",")
        ),
    );
    r.outcome("edge.overflow.sigma_minimum", &p);
    // An unregistered owner hotkey defers only a row that must burn.
    let specs = [s(0xc1, 9600), s(0xc2, 9400)];
    let p = example(reference, None, &specs)?;
    let t = CalculationTargetV2::for_package(&p, None)?;
    let idx = example_index(&p);
    let mut no_owner = snapshot(chain(), 9, &specs);
    no_owner.neurons.retain(|n| n.hotkey != owner_hotkey());
    r.row(
        "edge.owner_unregistered.nothing_burns",
        &reward_v1::derive_row(&t, Some((&p, &idx)), &no_owner)?,
    )?;
    no_owner.neurons.retain(|n| n.hotkey != hotkey(0xc2));
    r.code(
        "edge.owner_unregistered.must_burn",
        reward_v1::derive_row(&t, Some((&p, &idx)), &no_owner),
    );
    // Finney: reward-v1 is permitted; the mainnet floor applies.
    let specs = [s(0xb1, 9600)];
    let m = manifest(finney(), 1, 2000, sigma_permille(28), 0, &specs);
    let (p, _) = finalized(&m, reference, None, &specs)?;
    let t = CalculationTargetV2::for_package(&p, None)?;
    let idx = default_index(&m.references);
    let row = reward_v1::derive_row(&t, Some((&p, &idx)), &snapshot(finney(), 0, &specs))?;
    r.row("edge.finney.row", &row)?;
    r.code(
        "edge.finney.below_floor",
        manifest(finney(), 1, 1000, sigma_permille(40), 0, &specs).to_bytes(),
    );
    Ok(())
}

fn rejections(r: &mut Report) -> Result<()> {
    let c = computed()?;
    let snapshot = computed_snapshot(&c.specs);
    let derive = |p: &ScorePackageV2| -> Result<WeightRowV2> {
        let t = CalculationTargetV2::for_package(p, c.target.predecessor)?;
        reward_v1::derive_row(&t, Some((p, &c.index)), &snapshot)
    };
    // Rule and record version are bound together.
    let mut v0 = c.package.clone();
    v0.reward_rule = RewardRule::TestnetRewardV0;
    r.code("reject.v0_rule_in_v2_package", v0.to_bytes());
    let mut v1_manifest = crate::fixtures::manifest();
    v1_manifest.reward_rule = RewardRule::RewardV1;
    r.code("reject.v1_rule_in_v1_manifest", v1_manifest.to_bytes());
    let mut v1_package = crate::fixtures::package(&crate::fixtures::cohort()?)?;
    v1_package.reward_rule = RewardRule::RewardV1;
    r.code("reject.v1_rule_in_v1_package", v1_package.to_bytes());
    r.code(
        "reject.v2_package_as_v1",
        crate::ScorePackage::from_bytes(&c.package.to_bytes()?),
    );
    // Parameters are frozen.
    let mut params = c.manifest.clone();
    params.params.decay_bps = 7500;
    r.code("reject.params_decay", params.to_bytes());
    params.params = RewardV1Params::new(0);
    r.code("reject.params_sigma_zero", params.to_bytes());
    params.params = RewardV1Params::new(SCORE_SCALE + 1);
    r.code("reject.params_sigma_above_one", params.to_bytes());
    let mut mix = c.manifest.clone();
    mix.batch.mix.mixed = 30;
    mix.batch.mix.model_only = 30;
    r.code("reject.origin_mix", mix.to_bytes());
    let mut strata = c.manifest.clone();
    strata.batch.strata[0].cases += 1;
    r.code("reject.strata_cases", strata.to_bytes());
    let mut sched = c.manifest.clone();
    sched.schedule.lease_tempos = 39;
    r.code("reject.lease_below_two_cohorts", sched.to_bytes());
    sched.schedule = schedule();
    sched.schedule.transition_min = 1;
    r.code("reject.transition_minimum", sched.to_bytes());
    sched.schedule = CohortSchedule {
        tempos: 2,
        ..schedule()
    };
    r.code("reject.phase_minimums_exceed_k", sched.to_bytes());
    let mut refs = c.manifest.clone();
    refs.references.predecessor = None;
    r.code("reject.reference_without_predecessor", refs.to_bytes());
    let mut roster = c.manifest.clone();
    roster.roster.swap(0, 1);
    r.code("reject.roster_order", roster.to_bytes());
    let mut dup = c.manifest.clone();
    dup.roster[1].identity.hotkey = dup.roster[0].identity.hotkey;
    r.code("reject.roster_duplicate_hotkey", dup.to_bytes());
    let mut cap = c.manifest.clone();
    cap.roster_cap = 4;
    r.code("reject.roster_cap", cap.to_bytes());
    cap.roster_cap = crate::MAX_ROSTER + 1;
    r.code("reject.roster_cap_above_max", cap.to_bytes());
    // References: a failed prediction holds the package.
    let mut held = c.package.clone();
    held.reference.failed = 1;
    held.reference.succeeded -= 1;
    r.code("reject.reference_failure", held.to_bytes());
    let (current, _) = reference_predictions();
    let mut damaged = graded(&current);
    damaged[5].involvement = None;
    r.code(
        "reject.reference_failure_score",
        reward_v1::reference_score(&damaged),
    );
    // Package structure and gates.
    let mut span = c.package.clone();
    span.entrants[0].gate_failures = vec![GateFailure {
        gate: Gate::Span,
        reason: "span.iou_below_threshold".into(),
    }];
    r.code("reject.span_gate_not_enforced", span.to_bytes());
    let mut reason = c.package.clone();
    reason.entrants[7].gate_failures[0].reason = "Not A Code".into();
    r.code("reject.gate_reason_code", reason.to_bytes());
    let mut unscored = c.package.clone();
    unscored.entrants[8].gate_failures[0].gate = Gate::Share;
    r.code(
        "reject.unscored_without_qualification_failure",
        unscored.to_bytes(),
    );
    let mut counts = c.package.clone();
    counts.entrants[0].failed += 1;
    r.code("reject.outcome_count", counts.to_bytes());
    let mut manifest_bind = c.package.clone();
    manifest_bind.entrants[0].commitment = Digest([1; 32]);
    r.code(
        "reject.package_roster",
        manifest_bind.check_manifest(&c.manifest),
    );
    r.code(
        "reject.package_manifest_ok",
        c.package.check_manifest(&c.manifest),
    );
    // Outcome: validators recompute every unit.
    let mut units = c.package.clone();
    units.entrants[0].units += 1;
    units.entrants[1].units -= 1;
    r.code("reject.tampered_units", derive(&units));
    let mut mode = c.package.clone();
    mode.mode = CutoffMode::Fallback;
    mode.promoted = None;
    r.code("reject.tampered_mode", derive(&mode));
    let mut promoted = c.package.clone();
    promoted.promoted = Some(hotkey(0x12));
    r.code("reject.tampered_promotion", derive(&promoted));
    // Exact credits: validators recompute each retained cluster's fraction.
    let mut credit = c.package.clone();
    credit.credits[0].num = credit.credits[0].num.add(&crate::bigint::BigUint::one());
    r.code("reject.tampered_credit", derive(&credit));
    let mut credit = c.package.clone();
    credit.credits[0].num = credit.credits[0].num.mul_u64(2);
    credit.credits[0].den = credit.credits[0].den.mul_u64(2);
    r.code("reject.credit_not_in_lowest_terms", derive(&credit));
    let mut credit = c.package.clone();
    credit.credits.swap(0, 1);
    r.code("reject.credit_order", derive(&credit));
    let mut credit = c.package.clone();
    credit.credits.pop();
    r.code("reject.credit_missing", derive(&credit));
    let mut credit = c.package.clone();
    credit.credits[1].head = hotkey(0x16);
    r.code("reject.credit_of_member", credit.to_bytes());
    let mut credit = c.package.clone();
    credit.credits.push(credit.credits[0].clone());
    r.code("reject.credit_duplicate", credit.to_bytes());
    let mut credit = c.package.clone();
    credit.credits[0].num = crate::bigint::BigUint::zero();
    r.code("reject.credit_zero", credit.to_bytes());
    let none = example(bp(9100), None, &[Spec::new(0x91, bp(9150))])?;
    let mut credit = none.clone();
    credit.credits = c.package.credits[..1].to_vec();
    r.code("reject.no_target_with_credit", credit.to_bytes());
    // Clusters: structure and the identical content rule.
    let mut split = c.package.clone();
    split.entrants[5].cluster_head = Some(hotkey(0x16));
    r.code("reject.identical_content_split", derive(&split));
    let mut late_head = c.package.clone();
    for e in &mut late_head.entrants {
        if e.cluster_head == Some(hotkey(0x11)) {
            e.cluster_head = Some(hotkey(0x16));
        }
    }
    r.code("reject.head_ranked_below_member", derive(&late_head));
    let mut outside = c.package.clone();
    outside.entrants[3].cluster_head = Some(hotkey(0x14));
    r.code("reject.cluster_outside_cutoff", derive(&outside));
    let mut missing = c.package.clone();
    missing.entrants[10].cluster_head = None;
    missing.entrants[10].cluster_stats = None;
    missing.entrants[10].units = 0;
    missing.entrants[0].units += c.package.entrants[10].units;
    r.code("reject.eligible_without_cluster", derive(&missing));
    // Snapshots and burn routing: units that must burn while the finalized
    // owner hotkey holds no UID defer the row.
    let scored = Some((&c.package, &c.index));
    let mut owner = snapshot.clone();
    owner.owner_hotkey = hotkey(0x77);
    r.code(
        "reject.rotated_owner_unregistered",
        reward_v1::derive_row(&c.target, scored, &owner),
    );
    let mut absent = snapshot.clone();
    absent.neurons.retain(|n| n.hotkey != owner_hotkey());
    r.code(
        "reject.owner_unregistered",
        reward_v1::derive_row(&c.target, scored, &absent),
    );
    r.code(
        "reject.launch_burn_owner_unregistered",
        reward_v1::derive_row(&CalculationTargetV2::launch_burn(chain()), None, &absent),
    );
    let mut twice = snapshot.clone();
    twice.neurons.push(NeuronIdentity {
        uid: 200,
        hotkey: owner_hotkey(),
        registration_block: 1,
    });
    r.code("reject.snapshot_duplicate_hotkey", twice.to_bytes());
    let launch = CalculationTargetV2::launch_burn(chain());
    r.code(
        "reject.burn_target_with_package",
        reward_v1::derive_row(&launch, scored, &snapshot),
    );
    r.code(
        "reject.cohort_target_without_package",
        reward_v1::derive_row(&c.target, None, &snapshot),
    );
    r.code(
        "reject.target_for_no_target_package",
        CalculationTargetV2::for_package(&none, None),
    );
    // Closure and openings.
    let mut wrong_nonce = c.closure.clone();
    wrong_nonce.openings[2].nonce = Nonce([9; 32]);
    r.code(
        "reject.closure_wrong_nonce",
        wrong_nonce.check(&c.manifest, &c.package, Some(&c.target)),
    );
    let mut early = c.closure.clone();
    early.closed_epoch -= 1;
    r.code(
        "reject.closure_before_minimum",
        early.check(&c.manifest, &c.package, Some(&c.target)),
    );
    // Phase minimums: admission at least one tempo, publication after it,
    // and a transition of at least two tempos before the close.
    let mut admission = c.closure.clone();
    admission.admission_closed_epoch = c.manifest.opened_epoch;
    r.code(
        "reject.closure_admission_before_minimum",
        admission.check(&c.manifest, &c.package, Some(&c.target)),
    );
    let mut unordered = c.closure.clone();
    unordered.published_epoch = unordered.admission_closed_epoch;
    r.code(
        "reject.closure_published_before_admission_closed",
        unordered.check(&c.manifest, &c.package, Some(&c.target)),
    );
    let mut short = c.closure.clone();
    short.published_epoch = short.closed_epoch - 1;
    r.code(
        "reject.closure_before_transition_minimum",
        short.check(&c.manifest, &c.package, Some(&c.target)),
    );
    // A late publication closes later than K, after its full transition.
    let mut late = short.clone();
    late.closed_epoch += 1;
    r.code(
        "reject.closure_late_publication_ok",
        late.check(&c.manifest, &c.package, Some(&c.target)),
    );
    r.code(
        "reject.closure_target_missing",
        c.closure.check(&c.manifest, &c.package, None),
    );
    authorization_rejections(r, &c)?;
    // Sizes.
    let mut long = c.package.clone();
    long.entrants = vec![c.package.entrants[0].clone(); usize::from(crate::MAX_ROSTER) + 1];
    r.code("reject.roster_above_max", long.to_bytes());
    let receipt = sign(c.package.clone(), &receipt_key())?;
    r.code(
        "reject.receipt_key_signs_package",
        receipt.verify(&crate::fixtures::trusted_keys()),
    );
    let envelope = sign(c.package.clone(), &package_key())?.to_bytes()?;
    r.code(
        "reject.package_envelope_as_target",
        Signed::<CalculationTargetV2>::from_bytes(&envelope),
    );
    r.code(
        "reject.signed_ok",
        Signed::<ScorePackageV2>::open(&envelope, &crate::fixtures::trusted_keys()),
    );
    r.code(
        "reject.content_without_graph",
        InferenceContent {
            contract: CONTRACT.into(),
            components: vec![Component {
                role: ComponentRole::Tokenizer,
                sha256: Digest([1; 32]),
                len: 1,
            }],
        }
        .to_bytes(),
    );
    let mut unsorted = inference_content(0x11);
    unsorted.components.reverse();
    r.code("reject.content_unsorted", unsorted.to_bytes());
    copy_rejections(r, &c)?;
    Ok(())
}

/// Reward-intent, launch and renewal refusals.
fn authorization_rejections(r: &mut Report, c: &Computed) -> Result<()> {
    let keys = trusted_keys();
    let validator = AccountId([0x66; 32]);
    let epoch = c.closure.closed_epoch;
    let active = c.active_after()?;
    let unactivated = c.active_before()?;
    let check = |a: RewardIntentAuthorization,
                 p: &ScorePackageV2,
                 t: &CalculationTargetV2,
                 active: &ActiveTarget|
     -> Result<()> {
        let a = signed(a)?;
        RewardIntentAuthorization::check(&a, &keys, &c.manifest, p, t, active).map(|_| ())
    };
    let base = c.authorization.record.clone();
    let mut lease = base.clone();
    lease.expires_epoch += 1;
    r.code(
        "reject.authorization_lease",
        check(lease, &c.package, &c.target, &active),
    );
    let mut early = base.clone();
    early.issued_epoch = c.manifest.opened_epoch - 1;
    early.expires_epoch = early.issued_epoch + u64::from(c.manifest.schedule.lease_tempos);
    r.code(
        "reject.authorization_before_cohort",
        check(early, &c.package, &c.target, &active),
    );
    let launch = CalculationTargetV2::launch_burn(chain());
    let burn = authorization(&c.manifest, &c.package, &launch, 1, epoch - 4)?;
    r.code(
        "reject.authorization_not_the_new_target",
        check(burn.clone(), &c.package, &launch, &unactivated),
    );
    r.code(
        "reject.authorization_other_target",
        check(base.clone(), &c.package, &launch, &active),
    );
    let later = example(bp(9100), Some(bp(9000)), &[Spec::new(0x91, bp(8950))])?;
    let m7 = manifest(
        chain(),
        7,
        1000,
        sigma_permille(40),
        1,
        &[Spec::new(0x91, bp(8950))],
    );
    let mut stale = c.target.clone();
    if let crate::TargetSource::Cohort { cohort, .. } = &mut stale.source {
        *cohort = 9;
    }
    let carried = signed(authorization(&m7, &later, &stale, 1, m7.opened_epoch + 16)?)?;
    r.code(
        "reject.carried_target_from_later_cohort",
        RewardIntentAuthorization::check(&carried, &keys, &m7, &later, &stale, &active),
    );
    // A no-target package carries only the target the validator holds active.
    let specs4 = [Spec::new(0x91, bp(9050))];
    let m4 = manifest(chain(), 4, 1000, sigma_permille(40), 2, &specs4);
    let (p4, _) = finalized(&m4, bp(9600), Some(bp(9100)), &specs4)?;
    let carried = signed(authorization(&m4, &p4, &c.target, 3, m4.opened_epoch + 16)?)?;
    r.code(
        "reject.carried_target_not_active",
        RewardIntentAuthorization::check(&carried, &keys, &m4, &p4, &c.target, &unactivated),
    );
    // A new target's predecessor is the active target.
    let orphan = CalculationTargetV2::for_package(&c.package, Some(Digest([9; 32])))?;
    let named = authorization(&c.manifest, &c.package, &orphan, 1, epoch - 4)?;
    r.code(
        "reject.target_predecessor_not_active",
        check(named, &c.package, &orphan, &unactivated),
    );
    let by_package_key = sign(base.clone(), &package_key())?;
    r.code(
        "reject.authorization_signed_by_package_key",
        RewardIntentAuthorization::check(
            &by_package_key,
            &keys,
            &c.manifest,
            &c.package,
            &c.target,
            &active,
        ),
    );
    let mut forged = c.authorization.clone();
    forged.signature[0] ^= 1;
    r.code(
        "reject.authorization_bad_signature",
        RewardIntentAuthorization::check(
            &forged,
            &keys,
            &c.manifest,
            &c.package,
            &c.target,
            &active,
        ),
    );
    r.code(
        "reject.authorization_signed_ok",
        check(base.clone(), &c.package, &c.target, &active),
    );
    // The launch authorization.
    let first = LaunchAuthorization::new(&chain(), 1, 20)?;
    let launch_check = |a: LaunchAuthorization, chain: &ChainBinding, active: &ActiveTarget| {
        let a = signed(a)?;
        LaunchAuthorization::check(&a, &keys, chain, active).map(|_| ())
    };
    let mut long = first.clone();
    long.expires_epoch += 1;
    r.code(
        "reject.launch_lease",
        launch_check(long, &chain(), &unactivated),
    );
    r.code(
        "reject.launch_after_activation",
        launch_check(first.clone(), &chain(), &active),
    );
    let mut named = first.clone();
    named.target = c.target.digest()?;
    r.code(
        "reject.launch_target_not_burn",
        launch_check(named, &chain(), &unactivated),
    );
    r.code(
        "reject.launch_other_chain",
        launch_check(
            LaunchAuthorization::new(&finney(), 1, 20)?,
            &chain(),
            &unactivated,
        ),
    );
    r.code(
        "reject.launch_signed_by_package_key",
        LaunchAuthorization::check(
            &sign(first.clone(), &package_key())?,
            &keys,
            &chain(),
            &unactivated,
        ),
    );
    // Renewals.
    let authorized = c.checked_authorization()?;
    let renew = |renewal: RewardIntentRenewal, a: &CheckedAuthorization<'_>| -> Result<()> {
        let renewal = signed(renewal)?;
        RewardIntentRenewal::check(&renewal, &keys, a).map(|_| ())
    };
    let mut renewal = c.renewal.record.clone();
    renewal.expires_epoch += 1;
    r.code("reject.renewal_lease", renew(renewal, &authorized));
    let mut renewal = c.renewal.record.clone();
    renewal.checkpoint_generation = 0;
    r.code("reject.renewal_generation", renew(renewal, &authorized));
    let mut renewal = c.renewal.record.clone();
    renewal.issued_epoch = base.issued_epoch - 1;
    renewal.expires_epoch = base.expires_epoch - 1;
    r.code(
        "reject.renewal_before_authorization",
        renew(renewal, &authorized),
    );
    // A reissued authorization for the same target, at a later checkpoint
    // generation, passes its check but is not the one the renewal renews.
    let reissued = signed(authorization(
        &c.manifest,
        &c.package,
        &c.target,
        2,
        base.issued_epoch + 1,
    )?)?;
    r.code(
        "reject.renewal_of_other_authorization",
        RewardIntentAuthorization::check(
            &reissued,
            &keys,
            &c.manifest,
            &c.package,
            &c.target,
            &active,
        )
        .and_then(|a| RewardIntentRenewal::check(&c.renewal, &keys, &a).map(|_| ())),
    );
    let mut forged = c.renewal.clone();
    forged.signature[0] ^= 1;
    r.code(
        "reject.renewal_bad_signature",
        RewardIntentRenewal::check(&forged, &keys, &authorized),
    );
    r.code(
        "reject.renewal_signed_by_package_key",
        RewardIntentRenewal::check(
            &sign(c.renewal.record.clone(), &package_key())?,
            &keys,
            &authorized,
        ),
    );
    // A renewal cannot carry the lease over to another target: an
    // authorization naming the launch burn row under a package that produced
    // a target is refused at its check, so no renewal is ever checked against
    // it and it yields no operation.
    let active = c.active_after()?;
    let burn = signed(burn)?;
    r.code(
        "reject.renewal_retargets",
        RewardIntentAuthorization::check(
            &burn,
            &keys,
            &c.manifest,
            &c.package,
            &launch,
            &unactivated,
        )
        .and_then(|a| {
            let renewal = RewardIntentRenewal::check(&c.renewal, &keys, &a)?;
            a.operation(Some(&renewal), &active, validator, epoch)
        }),
    );
    // A renewal takes effect only from its issue epoch.
    let late = signed(RewardIntentRenewal::new(
        &authorized,
        3,
        base.expires_epoch + 10,
    )?)?;
    let late = RewardIntentRenewal::check(&late, &keys, &authorized)?;
    r.code(
        "reject.epoch_before_renewal_issue",
        authorized.operation(Some(&late), &active, validator, base.expires_epoch + 5),
    );
    // Only a checked closure activates a target, so a closure naming another
    // target, or one that closed before the frozen minimums, never does.
    let mut other_closure = c.closure.clone();
    other_closure.target = Some(launch.digest()?);
    r.code(
        "reject.operation_closure_of_other_target",
        other_closure
            .check(&c.manifest, &c.package, Some(&c.target))
            .and_then(|k| ActiveTarget::activated(&k))
            .and_then(|a| authorized.operation(None, &a, validator, epoch)),
    );
    let mut early = c.closure.clone();
    early.published_epoch = c.manifest.opened_epoch + 2;
    early.closed_epoch = c.manifest.opened_epoch + 4;
    r.code(
        "reject.operation_closed_before_minimum",
        early
            .check(&c.manifest, &c.package, Some(&c.target))
            .and_then(|k| ActiveTarget::activated(&k))
            .and_then(|a| authorized.operation(None, &a, validator, base.issued_epoch)),
    );
    Ok(())
}

/// Package checks that refuse a misapplied public-copy gate, near-copy
/// distance, `w / 2` clause, coverage, published index or closure
/// fingerprints.
fn copy_rejections(r: &mut Report, c: &Computed) -> Result<()> {
    let outcome = |p: &ScorePackageV2| reward_v1::check_outcome(p, &c.index).map(|_| ());
    let index_of = |name: &str| {
        c.models
            .iter()
            .position(|m| m.name == name)
            .expect("fixture model")
    };
    let (exact, recalibrated, retrain, sharpened, probe_copy) = (
        index_of("rk_exact_copy"),
        index_of("rk_recalibrated"),
        index_of("rk_retrain"),
        index_of("rk1_sharpened"),
        index_of("omega_probe_copy"),
    );
    let (alpha, delta, kappa) = (index_of("alpha"), index_of("delta"), index_of("kappa"));
    let eta = index_of("eta");
    let (alpha_copy, alpha_close) = (index_of("alpha_copy"), index_of("alpha_close"));
    let fail = |p: &mut ScorePackageV2, i: usize, copy: Option<PublicCopy>| {
        let e = &mut p.entrants[i];
        e.gate_failures.retain(|g| g.gate != Gate::PublicCopy);
        e.gate_failures.extend(copy.map(PublicCopy::failure));
    };
    // public.content: never executed, and recorded exactly for published
    // content.
    let mut p = c.package.clone();
    let e = &mut p.entrants[exact];
    (e.score, e.assigned, e.succeeded) = (Some(bp(9100)), CASES, CASES);
    r.code("reject.public_content_executed", p.to_bytes());
    let with_alpha = PublishedIndex::new(
        c.index
            .contents
            .contents
            .iter()
            .copied()
            .chain([content(0x11)]),
        c.index.fingerprints.entries.iter().copied(),
    )?;
    let mut p = c.package.clone();
    p.published = with_alpha.binding()?;
    r.code(
        "reject.public_content_unrecorded",
        reward_v1::check_outcome(&p, &with_alpha).map(|_| ()),
    );
    let mut p = c.package.clone();
    p.entrants[exact].content = content(0x99);
    r.code("reject.public_content_not_published", outcome(&p));
    // Coverage: statistics for every scored entrant, whatever its score, and
    // none for an entrant that was never executed.
    let mut p = c.package.clone();
    p.entrants[retrain].public = None;
    r.code("reject.public_coverage_missing", outcome(&p));
    let mut p = c.package.clone();
    p.entrants[kappa].public = None;
    r.code("reject.public_coverage_low_score", outcome(&p));
    let mut p = c.package.clone();
    p.entrants[eta].public = c.package.entrants[alpha].public.clone();
    r.code("reject.public_check_unexecuted", outcome(&p));
    // The near-copy relation, applied exactly from published integers.
    let mut p = c.package.clone();
    fail(&mut p, recalibrated, None);
    r.code("reject.public_near_unrecorded", outcome(&p));
    let mut p = c.package.clone();
    fail(&mut p, retrain, Some(PublicCopy::Current));
    r.code("reject.public_near_not_near", outcome(&p));
    // delta is a rank-preserving map of R_k but beats it by more than w / 2.
    let mut p = c.package.clone();
    fail(&mut p, delta, Some(PublicCopy::Current));
    r.code("reject.public_improvement_clause", outcome(&p));
    let mut p = c.package.clone();
    fail(&mut p, recalibrated, Some(PublicCopy::Predecessor));
    r.code("reject.public_precedence", outcome(&p));
    let mut p = c.package.clone();
    fail(&mut p, sharpened, Some(PublicCopy::Current));
    r.code("reject.public_predecessor_copy_as_current", outcome(&p));
    // Probe matches.
    fn probe_match(p: &mut ScorePackageV2, i: usize) -> Result<&mut ProbeMatch> {
        p.entrants[i]
            .public
            .as_mut()
            .and_then(|x| x.probe_match.as_mut())
            .ok_or(Error::Invalid("fixture probe match"))
    }
    let mut p = c.package.clone();
    probe_match(&mut p, probe_copy)?.content = content(0x11);
    r.code("reject.probe_match_not_fingerprinted", outcome(&p));
    let mut p = c.package.clone();
    probe_match(&mut p, probe_copy)?.version = 2;
    r.code("reject.probe_version_not_in_use", outcome(&p));
    let mut p = c.package.clone();
    probe_match(&mut p, probe_copy)?.stats = far(CASES);
    r.code("reject.probe_match_not_near", outcome(&p));
    let mut p = c.package.clone();
    fail(&mut p, probe_copy, None);
    r.code("reject.probe_match_without_failure", p.to_bytes());
    // The probe comparison includes both references: every scored entrant
    // publishes its comparison with each reference fingerprint, and one that
    // is a near copy cannot pass the gate or lose to a farther match.
    let mut p = c.package.clone();
    if let Some(public) = p.entrants[alpha].public.as_mut() {
        public.reference_probes.pop();
    }
    r.code("reject.reference_probe_missing", outcome(&p));
    let mut p = c.package.clone();
    if let Some(public) = p.entrants[alpha].public.as_mut() {
        public.reference_probes.reverse();
    }
    r.code("reject.reference_probe_order", outcome(&p));
    let mut p = c.package.clone();
    if let Some(public) = p.entrants[retrain].public.as_mut() {
        public.reference_probes[0].stats = aligned(CASES);
    }
    r.code("reject.reference_probe_near_passed", outcome(&p));
    let mut p = c.package.clone();
    if let Some(public) = p.entrants[probe_copy].public.as_mut() {
        public.reference_probes[0].stats = aligned(CASES);
        if let Some(found) = public.probe_match.as_mut() {
            let half = u128::from(NEAR_COPY_PPB / 2);
            found.stats.rss_fwd = u128::from(CASES) * half * half;
            found.stats.rss_rev = found.stats.rss_fwd;
        }
    }
    r.code("reject.probe_match_not_closest", outcome(&p));
    let mut p = c.package.clone();
    if let Some(public) = p.entrants[alpha].public.as_mut() {
        public.reference_probes[0].public_probe_score = Score::ONE;
    }
    r.code("reject.reference_probe_score", outcome(&p));
    let mut p = c.package.clone();
    if let Some(public) = p.entrants[alpha].public.as_mut() {
        public.reference_probes[1].entrant_probe_score = Score::ONE;
    }
    r.code("reject.entrant_probe_score", outcome(&p));
    // The published index.
    let without_omega = PublishedIndex::new(
        [content(0xe0), content(0xe1)],
        c.index
            .fingerprints
            .entries
            .iter()
            .copied()
            .filter(|f| f.content != content(ARCHIVED)),
    )?;
    r.code(
        "reject.published_index_binding",
        reward_v1::check_outcome(&c.package, &without_omega).map(|_| ()),
    );
    let without_r0 = PublishedIndex::new(
        [content(0xe1), content(ARCHIVED)],
        c.index
            .fingerprints
            .entries
            .iter()
            .copied()
            .filter(|f| f.content != content(0xe0)),
    )?;
    let mut p = c.package.clone();
    p.published = without_r0.binding()?;
    r.code(
        "reject.reference_not_published",
        reward_v1::check_outcome(&p, &without_r0).map(|_| ()),
    );
    let r0_unfingerprinted = PublishedIndex::new(
        c.index.contents.contents.iter().copied(),
        c.index
            .fingerprints
            .entries
            .iter()
            .copied()
            .filter(|f| f.content != content(0xe0)),
    )?;
    let mut p = c.package.clone();
    p.published = r0_unfingerprinted.binding()?;
    r.code(
        "reject.reference_not_fingerprinted",
        reward_v1::check_outcome(&p, &r0_unfingerprinted).map(|_| ()),
    );
    // Closure fingerprints: exactly one per executed entrant and probe
    // version, so a low scorer cannot be left out.
    let mut fingerprints = closure_fingerprints(c);
    fingerprints.retain(|f| f.hotkey != c.package.entrants[kappa].identity.hotkey);
    r.code(
        "reject.closure_fingerprint_missing",
        c.index.after_closure(&c.package, fingerprints).map(|_| ()),
    );
    let mut fingerprints = closure_fingerprints(c);
    fingerprints.push(EntrantFingerprint {
        hotkey: c.package.entrants[eta].identity.hotkey,
        version: PROBE_VERSION,
        fingerprint: fingerprint_digest(0x19),
    });
    r.code(
        "reject.closure_fingerprint_unexecuted",
        c.index.after_closure(&c.package, fingerprints).map(|_| ()),
    );
    let mut fingerprints = closure_fingerprints(c);
    fingerprints[0].version = 2;
    r.code(
        "reject.closure_fingerprint_version",
        c.index.after_closure(&c.package, fingerprints).map(|_| ()),
    );
    let mut fingerprints = closure_fingerprints(c);
    fingerprints.push(EntrantFingerprint {
        fingerprint: fingerprint_digest(0x11),
        ..fingerprints[0]
    });
    r.code(
        "reject.closure_fingerprint_twice",
        c.index.after_closure(&c.package, fingerprints).map(|_| ()),
    );
    // Cluster statistics.
    let mut p = c.package.clone();
    p.entrants[alpha_close].cluster_stats = Some(far(CASES));
    r.code("reject.cluster_stats_too_far", outcome(&p));
    let mut p = c.package.clone();
    p.entrants[alpha_close].cluster_stats = None;
    r.code("reject.cluster_join_without_stats", outcome(&p));
    let mut p = c.package.clone();
    p.entrants[alpha_copy].cluster_stats = Some(aligned(CASES));
    r.code("reject.cluster_stats_on_identical_content", outcome(&p));
    // Statistic sanity and the distance monitor.
    let mut p = c.package.clone();
    if let Some(public) = p.entrants[alpha].public.as_mut() {
        public.current.common -= 1;
    }
    r.code("reject.near_stats_common", outcome(&p));
    let mut p = c.package.clone();
    if let Some(public) = p.entrants[alpha].public.as_mut() {
        public.current.rss_fwd = u128::from(CASES) * 1_000_000_000_000_000_000 + 1;
    }
    r.code("reject.near_stats_residual", outcome(&p));
    let mut p = c.package.clone();
    p.distance_monitor.min_gated_vs_current_ppb =
        p.distance_monitor.min_gated_vs_current_ppb.map(|d| d + 1);
    r.code("reject.distance_monitor_current", outcome(&p));
    let mut p = c.package.clone();
    p.distance_monitor.min_entrant_pair_ppb = Some(NEAR_COPY_PPB - 1);
    r.code("reject.distance_monitor_pair_below_d", outcome(&p));
    let mut p = c.package.clone();
    p.distance_monitor.min_gated_vs_archive_ppb = Some(NEAR_COPY_PPB * 4);
    r.code(
        "reject.distance_monitor_archive_above_reference",
        outcome(&p),
    );
    let mut p = c.package.clone();
    p.distance_monitor.min_gated_vs_archive_ppb = None;
    r.code("reject.distance_monitor_archive_missing", outcome(&p));
    // Records and inputs.
    r.code(
        "reject.near_stats_length",
        reward_v1::near_stats(&predictions(&[1]), &predictions(&[1, 2])),
    );
    let mut contents = vec![content(0xe0), content(0xe1)];
    contents.sort_unstable();
    contents.reverse();
    r.code(
        "reject.published_contents_unsorted",
        PublishedContents { contents }.to_bytes(),
    );
    r.code(
        "reject.fingerprint_of_unpublished",
        PublishedIndex::new(
            [content(0xe0)],
            [Fingerprint {
                content: content(0xe1),
                version: PROBE_VERSION,
                fingerprint: fingerprint_digest(0xe1),
            }],
        ),
    );
    r.code(
        "reject.fingerprint_twice_on_one_version",
        PublishedIndex::new(
            [content(0xe0)],
            [0xe0, 0xe1].map(|tag| Fingerprint {
                content: content(0xe0),
                version: PROBE_VERSION,
                fingerprint: fingerprint_digest(tag),
            }),
        ),
    );
    let mut m = c.manifest.clone();
    m.probes.clear();
    r.code("reject.manifest_without_probe", m.to_bytes());
    Ok(())
}
