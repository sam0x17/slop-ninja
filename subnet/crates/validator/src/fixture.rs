//! Testnet shortcut T12: build a minimal coordinator-signed fixture package.
//!
//! The cohort is synthetic plumbing: four tasks in two groups, a baseline that
//! predicts 0.5 everywhere, and each allow-listed miner predicting closer to
//! the labels than the baseline, so every listed miner earns positive credit.
//! Nothing here is evaluation evidence. Receipt salts are derived from the
//! attempt ID so the bundle is reproducible; production salts are private
//! random values. Removed by M6, when the coordinator publishes real packages.
//!
//! Signing keys are supplied by the caller. Tests use public deterministic dev
//! seeds; an operator building a live testnet fixture uses seed files they
//! generated locally (`slop_ninja_fixture_package keygen`).

use slop_ninja_protocol::{
    ArtifactRecord, Assignment, Attempt, AttemptReceipt, AttemptResult, CalculationTarget,
    ChainBinding, CohortManifest, Digest, MinerScore, NeuronIdentity, Outcome, Probability,
    ReceiptIndex, ReceiptIndexEntry, Record, RewardRule, ScorePackage, SigningKey, TESTNET_GENESIS,
    domain_hash, reward, sign,
};

use crate::package::{BUNDLE_SCHEMA, Bundle};

/// Labels and source groups of the four synthetic tasks.
const LABELS: [bool; 4] = [true, false, true, false];
const GROUPS: [u32; 4] = [0, 0, 1, 1];

/// Inputs of one fixture cohort. The chain is always the testnet genesis,
/// since `testnet-reward-v0` (T8) is valid nowhere else.
#[derive(Clone, Debug)]
pub struct FixtureParams {
    pub netuid: u16,
    pub mechanism: u8,
    pub cohort: u64,
    pub cohort_label: String,
    pub revision: u64,
    /// Finalized tempo-boundary block recorded as the cohort's opening.
    pub opened_block: u64,
    /// Owner registration at UID 0.
    pub burn_identity: NeuronIdentity,
    /// Allow-listed miners, each with its exact registration.
    pub miners: Vec<NeuronIdentity>,
}

impl FixtureParams {
    pub fn chain(&self) -> ChainBinding {
        ChainBinding {
            genesis: TESTNET_GENESIS,
            netuid: self.netuid,
            mechanism: self.mechanism,
        }
    }
}

fn artifact(owner: NeuronIdentity) -> ArtifactRecord {
    let mut seed = owner.uid.to_le_bytes().to_vec();
    seed.extend_from_slice(&owner.hotkey.0);
    seed.extend_from_slice(&owner.registration_block.to_le_bytes());
    ArtifactRecord {
        owner,
        contract: "slop_ninja.fixture.t12.v1".into(),
        package_sha256: domain_hash("slop_ninja.fixture-t12/package", &seed),
        model_sha256: domain_hash("slop_ninja.fixture-t12/model", &seed),
        image_digest: domain_hash("slop_ninja.fixture-t12/image", &seed),
        commitment_block: owner.registration_block,
    }
}

/// Predicted involvement for task `task` by entrant `rank` (0 is the baseline).
fn prediction(rank: usize, task: usize) -> slop_ninja_protocol::Result<Outcome> {
    let p = if rank == 0 {
        0.5
    } else {
        // Ranks 1, 2, ... predict 0.95, 0.90, ..., never worse than 0.55.
        let confidence = (0.95 - 0.05 * (rank as f64 - 1.0)).max(0.55);
        if LABELS[task] {
            confidence
        } else {
            1.0 - confidence
        }
    };
    let involvement = Probability::from_f64(p)?;
    let share = Probability::new(involvement.ppb() / 2)?;
    Ok(Outcome::Prediction { involvement, share })
}

/// Build and sign the bundle.
pub fn build(
    params: &FixtureParams,
    package_key: &SigningKey,
    receipt_key: &SigningKey,
) -> slop_ninja_protocol::Result<Bundle> {
    let chain = params.chain();
    let mut miners = params.miners.clone();
    miners.sort();
    let baseline = artifact(params.burn_identity);
    let mut artifacts: Vec<Digest> = miners
        .iter()
        .map(|m| artifact(*m).digest())
        .collect::<slop_ninja_protocol::Result<_>>()?;
    artifacts.sort();
    let manifest = CohortManifest {
        chain: chain.clone(),
        cohort: params.cohort,
        cohort_label: params.cohort_label.clone(),
        opened_block: params.opened_block,
        evaluation_set: domain_hash("slop_ninja.fixture-t12/evaluation-set", b"synthetic"),
        task_count: LABELS.len() as u32,
        group_count: 2,
        baseline_artifact: baseline.digest()?,
        reward_rule: RewardRule::TestnetRewardV0,
        max_attempts: 1,
        allow_list: miners.clone(),
        burn_identity: params.burn_identity,
        artifacts,
    };
    let manifest_id = manifest.digest()?;
    let mut receipts = Vec::new();
    let mut baseline_score = None;
    let mut scores = Vec::new();
    let entrants = std::iter::once(params.burn_identity).chain(miners.iter().copied());
    for (rank, owner) in entrants.enumerate() {
        let record = artifact(owner);
        let artifact_id = record.digest()?;
        let mut tasks = Vec::new();
        for task in 0..LABELS.len() {
            let assignment = Assignment {
                cohort_manifest: manifest_id,
                artifact: artifact_id,
                task_index: task as u32,
                group: GROUPS[task],
            };
            assignment.check_manifest(&manifest)?;
            let attempt = Attempt {
                assignment: assignment.digest()?,
                number: 1,
            };
            let outcome = prediction(rank, task)?;
            let result = AttemptResult {
                attempt: attempt.digest()?,
                outcome: outcome.clone(),
            };
            let salt = domain_hash("slop_ninja.fixture-t12/salt", &result.attempt.0).0;
            receipts.push(AttemptReceipt::new(&assignment, &attempt, &result, &salt)?);
            tasks.push(reward::ScoredTask {
                group: GROUPS[task],
                involvement: reward::final_involvement(&[(1, &outcome)])?,
                label: LABELS[task],
            });
        }
        let score = reward::aggregate_score(&tasks, manifest.group_count)?;
        if rank == 0 {
            baseline_score = Some(score);
        } else {
            scores.push(MinerScore {
                identity: owner,
                artifact: artifact_id,
                score,
                assigned: LABELS.len() as u32,
                succeeded: LABELS.len() as u32,
                failed: 0,
            });
        }
    }
    scores.sort_by_key(|s| s.identity);
    let mut entries = receipts
        .iter()
        .map(ReceiptIndexEntry::for_receipt)
        .collect::<slop_ninja_protocol::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.attempt);
    receipts.sort_by_key(|r| r.attempt);
    let index = ReceiptIndex {
        cohort_manifest: manifest_id,
        entries,
    };
    let package = ScorePackage {
        chain,
        cohort: params.cohort,
        cohort_label: params.cohort_label.clone(),
        cohort_manifest: manifest_id,
        revision: params.revision,
        reward_rule: RewardRule::TestnetRewardV0,
        baseline: baseline_score.expect("baseline scored first"),
        allow_list: miners,
        burn_identity: params.burn_identity,
        scores,
        receipt_root: index.digest()?,
        receipt_count: index.count(),
    };
    let target = CalculationTarget::for_package(&package, None)?;
    Ok(Bundle {
        schema: BUNDLE_SCHEMA.into(),
        shortcut: "T12".into(),
        manifest: hex::encode(sign(manifest, package_key)?.to_bytes()?),
        package: hex::encode(sign(package, package_key)?.to_bytes()?),
        target: hex::encode(sign(target, package_key)?.to_bytes()?),
        receipt_index: hex::encode(sign(index, receipt_key)?.to_bytes()?),
        receipts: receipts
            .iter()
            .map(|r| r.to_bytes().map(hex::encode))
            .collect::<slop_ninja_protocol::Result<_>>()?,
    })
}
