//! Package inputs and the pluggable reward rule.
//!
//! The cohort engine produces [`PackageInputs`]: the frozen manifest, each scored
//! job's integer utility and outcome counts, and the receipt root and count. A
//! [`RewardPolicy`] turns them into the v1 score package. Only
//! `testnet-reward-v0` exists (the mapping policy for reward-v1 is under review);
//! a later rule implements the same trait and reads the same inputs plus the
//! content-free components (paired SE, clusters) recorded beside the package.

use slop_ninja_protocol::{
    CalculationTarget, ChainBinding, CohortManifest, Digest, MinerScore, NeuronIdentity, Record,
    RewardRule, Score, ScorePackage, Signed, SigningKey, sign,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackageError(pub &'static str);

/// One scored job's reward-relevant numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobScore {
    pub artifact: Digest,
    pub score: Score,
    pub assigned: u32,
    pub succeeded: u32,
    pub failed: u32,
}

#[derive(Clone, Debug)]
pub struct PackageInputs {
    pub manifest: CohortManifest,
    pub manifest_digest: Digest,
    pub revision: u64,
    pub reference: JobScore,
    pub previous_reference: Option<JobScore>,
    pub entrants: Vec<(NeuronIdentity, JobScore)>,
    pub receipt_root: Digest,
    pub receipt_count: u64,
}

pub trait RewardPolicy {
    fn rule(&self) -> RewardRule;
    fn package(&self, inputs: &PackageInputs) -> Result<ScorePackage, PackageError>;
}

/// `testnet-reward-v0` (shortcut T8): the baseline is R_k's utility on the same
/// batch, and every entrant's utility, assignment and outcome counts are
/// published; the row is derived by `slop_ninja_protocol::reward::derive_row`.
pub struct RewardV0;

impl RewardPolicy for RewardV0 {
    fn rule(&self) -> RewardRule {
        RewardRule::TestnetRewardV0
    }

    fn package(&self, i: &PackageInputs) -> Result<ScorePackage, PackageError> {
        let mut scores: Vec<MinerScore> = i
            .entrants
            .iter()
            .map(|(identity, j)| MinerScore {
                identity: *identity,
                artifact: j.artifact,
                score: j.score,
                assigned: j.assigned,
                succeeded: j.succeeded,
                failed: j.failed,
            })
            .collect();
        scores.sort_by_key(|a| a.identity);
        let package = ScorePackage {
            chain: i.manifest.chain.clone(),
            cohort: i.manifest.cohort,
            cohort_label: i.manifest.cohort_label.clone(),
            cohort_manifest: i.manifest_digest,
            revision: i.revision,
            reward_rule: self.rule(),
            baseline: i.reference.score,
            allow_list: i.manifest.allow_list.clone(),
            burn_identity: i.manifest.burn_identity,
            scores,
            receipt_root: i.receipt_root,
            receipt_count: i.receipt_count,
        };
        package.validate().map_err(|_| PackageError("score package"))?;
        package.check_manifest(&i.manifest).map_err(|_| PackageError("package manifest"))?;
        Ok(package)
    }
}

pub fn policy(rule: &str) -> Option<Box<dyn RewardPolicy>> {
    match rule {
        "testnet-reward-v0" => Some(Box::new(RewardV0)),
        _ => None,
    }
}

/// The evaluation-set digest: the plan bytes' SHA-256 and the private batch index's
/// SHA-256, under one domain.
pub fn evaluation_set(plan_sha256: &[u8; 32], batch_index_sha256: &[u8; 32]) -> Digest {
    let mut payload = plan_sha256.to_vec();
    payload.extend_from_slice(batch_index_sha256);
    slop_ninja_protocol::domain_hash("slop_ninja.eval/evaluation-set/v1", &payload)
}

#[allow(clippy::too_many_arguments)]
pub fn manifest(
    chain: ChainBinding,
    cohort: u64,
    label: &str,
    opened_block: u64,
    evaluation_set: Digest,
    task_count: u32,
    reference: Digest,
    max_attempts: u8,
    allow_list: Vec<NeuronIdentity>,
    burn_identity: NeuronIdentity,
    mut artifacts: Vec<Digest>,
) -> Result<CohortManifest, PackageError> {
    artifacts.sort();
    let m = CohortManifest {
        chain,
        cohort,
        cohort_label: label.into(),
        opened_block,
        evaluation_set,
        task_count,
        // Every task is its own group: the self-weighting batch's plain mean.
        group_count: task_count,
        baseline_artifact: reference,
        reward_rule: RewardRule::TestnetRewardV0,
        max_attempts,
        allow_list,
        burn_identity,
        artifacts,
    };
    m.validate().map_err(|_| PackageError("cohort manifest"))?;
    Ok(m)
}

pub fn target(package: &ScorePackage, predecessor: Option<Digest>) -> Result<CalculationTarget, PackageError> {
    CalculationTarget::for_package(package, predecessor).map_err(|_| PackageError("calculation target"))
}

pub fn signed_bytes<R: Record>(record: R, key: &SigningKey) -> Result<Vec<u8>, PackageError> {
    sign(record, key)
        .and_then(|s: Signed<R>| s.to_bytes())
        .map_err(|_| PackageError("sign"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_protocol::{AccountId, RegistrationSnapshot, TESTNET_GENESIS, TrustedKeys, reward};

    fn id(uid: u16, b: u8) -> NeuronIdentity {
        NeuronIdentity {
            uid,
            hotkey: AccountId([b; 32]),
            registration_block: 7,
        }
    }

    #[test]
    fn a_copy_of_the_reference_earns_nothing_and_the_row_burns() {
        let chain = ChainBinding {
            genesis: TESTNET_GENESIS,
            netuid: 601,
            mechanism: 0,
        };
        let reference = Digest([5; 32]);
        let copy = Digest([6; 32]);
        let m = manifest(chain.clone(), 1, "c1", 100, Digest([9; 32]), 10, reference, 3,
            vec![id(3, 3)], id(0, 1), vec![copy]).unwrap();
        let md = m.digest().unwrap();
        let s = Score::new(900_000_000_000_000_000).unwrap();
        let job = |a| JobScore { artifact: a, score: s, assigned: 10, succeeded: 10, failed: 0 };
        let inputs = PackageInputs {
            manifest: m.clone(),
            manifest_digest: md,
            revision: 0,
            reference: job(reference),
            previous_reference: None,
            entrants: vec![(id(3, 3), job(copy))],
            receipt_root: Digest([7; 32]),
            receipt_count: 20,
        };
        let p = policy("testnet-reward-v0").unwrap().package(&inputs).unwrap();
        let t = target(&p, None).unwrap();
        let snap = RegistrationSnapshot {
            chain,
            block: 200,
            block_hash: Digest([8; 32]),
            neurons: vec![id(0, 1), id(3, 3)],
        };
        let row = reward::derive_row(&t, &p, &snap).unwrap();
        assert_eq!(row.entries.len(), 1);
        assert_eq!((row.entries[0].uid, row.entries[0].weight), (0, u16::MAX));
        // A better entrant earns the row.
        let mut better = inputs.clone();
        better.entrants[0].1.score = Score::new(950_000_000_000_000_000).unwrap();
        let p2 = RewardV0.package(&better).unwrap();
        let row2 = reward::derive_row(&target(&p2, Some(t.digest().unwrap())).unwrap(), &p2, &snap).unwrap();
        assert_eq!((row2.entries[0].uid, row2.entries[0].weight), (3, u16::MAX));
        // Signed records open under the trusted keys.
        let pk = SigningKey::from_bytes(&[11; 32]);
        let rk = SigningKey::from_bytes(&[12; 32]);
        let keys = TrustedKeys::new(pk.verifying_key().to_bytes(), rk.verifying_key().to_bytes(), [13; 32]).unwrap();
        let bytes = signed_bytes(p.clone(), &pk).unwrap();
        assert_eq!(Signed::<ScorePackage>::open(&bytes, &keys).unwrap(), p);
        assert!(policy("reward-v1").is_none());
    }
}
