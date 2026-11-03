//! Deterministic synthetic fixtures and the golden report.
//!
//! Every value here is synthetic. The signing seeds are public test vectors,
//! never real keys, and the per-receipt salts are derived only so the report
//! is reproducible; production salts are private random values. The report is
//! compared byte for byte against `fixtures/golden-v1.txt` on native targets
//! and inside a `wasm32-unknown-unknown` build.

use std::fmt::Write as _;

use crate::hash::hex;
use crate::reward::{self, ScoredTask};
use crate::{
    AccountId, ArtifactRecord, Assignment, Attempt, AttemptReceipt, AttemptResult,
    CalculationTarget, ChainBinding, CohortManifest, Digest, Error, MinerScore, NeuronIdentity,
    Outcome, Probability, ReceiptIndex, ReceiptIndexEntry, Record, RegistrationSnapshot, Result,
    RewardRule, Score, ScorePackage, Signed, SigningKey, TESTNET_GENESIS, TrustedKeys,
    WeightOperation, WeightRow, admission_commitment, domain_hash, sign, signing_payload,
    weight_operation_id, weight_tempo_operation_id,
};

/// Public test seeds for the three application key roles.
pub const PACKAGE_SEED: [u8; 32] = [0x51; 32];
pub const RECEIPT_SEED: [u8; 32] = [0x52; 32];
pub const AUTHORIZATION_SEED: [u8; 32] = [0x53; 32];

pub fn package_key() -> SigningKey {
    SigningKey::from_bytes(&PACKAGE_SEED)
}
pub fn receipt_key() -> SigningKey {
    SigningKey::from_bytes(&RECEIPT_SEED)
}
pub fn authorization_key() -> SigningKey {
    SigningKey::from_bytes(&AUTHORIZATION_SEED)
}
pub fn trusted_keys() -> TrustedKeys {
    TrustedKeys::new(
        package_key().verifying_key().to_bytes(),
        receipt_key().verifying_key().to_bytes(),
        authorization_key().verifying_key().to_bytes(),
    )
    .expect("distinct fixture keys")
}

/// The testnet genesis, which `testnet-reward-v0` requires, with a placeholder netuid.
pub fn chain() -> ChainBinding {
    ChainBinding {
        genesis: TESTNET_GENESIS,
        netuid: 4242,
        mechanism: 0,
    }
}

/// A synthetic non-testnet chain, on which `testnet-reward-v0` must fail closed.
pub fn other_chain() -> ChainBinding {
    ChainBinding {
        genesis: Digest([0x2f; 32]),
        ..chain()
    }
}

/// A score package whose `scores` prefix claims [`crate::MAX_SEQUENCE_LEN`]
/// items, followed by `padding` zero bytes instead of the items.
pub fn sequence_claim(package: &ScorePackage, padding: usize) -> Vec<u8> {
    use crate::Canonical as _;
    let mut e = crate::Encoder::new();
    e.str(ScorePackage::KIND);
    e.u16(ScorePackage::VERSION);
    package.chain.encode_to(&mut e);
    package.cohort.encode_to(&mut e);
    package.cohort_label.encode_to(&mut e);
    package.cohort_manifest.encode_to(&mut e);
    package.revision.encode_to(&mut e);
    package.reward_rule.encode_to(&mut e);
    package.baseline.encode_to(&mut e);
    package.allow_list.encode_to(&mut e);
    package.burn_identity.encode_to(&mut e);
    e.u32(crate::MAX_SEQUENCE_LEN);
    e.raw(&vec![0; padding]);
    e.into_bytes()
}

/// Encodes `record` without validation, to build inputs a decoder must refuse.
pub fn unvalidated_bytes<R: Record>(record: &R) -> Vec<u8> {
    let mut e = crate::Encoder::new();
    e.str(R::KIND);
    e.u16(R::VERSION);
    record.encode_to(&mut e);
    e.into_bytes()
}

fn identity(uid: u16, hotkey: u8, registration_block: u64) -> NeuronIdentity {
    NeuronIdentity {
        uid,
        hotkey: AccountId([hotkey; 32]),
        registration_block,
    }
}

/// Allow-listed and currently registered.
pub fn miner_a() -> NeuronIdentity {
    identity(3, 0xa1, 1000)
}
/// Allow-listed and registered; one parse failure and one retried attempt.
pub fn miner_b() -> NeuronIdentity {
    identity(5, 0xb2, 1100)
}
/// Allow-listed, but UID 7 has since been reused by another registration.
pub fn miner_c() -> NeuronIdentity {
    identity(7, 0xc3, 1200)
}
/// Registered with a perfect score but not allow-listed.
pub fn stranger() -> NeuronIdentity {
    identity(9, 0xd4, 1300)
}
/// Owner registration at UID 0: the burn identity.
pub fn owner() -> NeuronIdentity {
    identity(0, 0x0f, 10)
}
/// An unrelated registration holding UID 0.
pub fn foreign_uid0() -> NeuronIdentity {
    identity(0, 0xee, 99)
}

const LABELS: [bool; 3] = [true, false, true];
const GROUPS: [u32; 3] = [0, 0, 1];

#[derive(Clone)]
enum Planned {
    Predict(f64),
    Fail(Outcome),
    RetryThenPredict(f64),
}

pub struct Entrant {
    pub owner: NeuronIdentity,
    pub artifact: ArtifactRecord,
    plan: [Planned; 3],
}

fn artifact_for(owner: NeuronIdentity, tag: u8) -> ArtifactRecord {
    ArtifactRecord {
        owner,
        contract: "slop_ninja.a.document-v1".into(),
        package_sha256: Digest([tag; 32]),
        model_sha256: Digest([tag ^ 0x80; 32]),
        image_digest: Digest([tag ^ 0x40; 32]),
        commitment_block: 2000 + u64::from(tag),
    }
}

pub fn baseline() -> Entrant {
    Entrant {
        owner: owner(),
        artifact: artifact_for(owner(), 0x01),
        plan: [
            Planned::Predict(0.6),
            Planned::Predict(0.4),
            Planned::Predict(0.5),
        ],
    }
}

pub fn entrants() -> Vec<Entrant> {
    vec![
        Entrant {
            owner: miner_a(),
            artifact: artifact_for(miner_a(), 0x0a),
            plan: [
                Planned::Predict(0.9),
                Planned::Predict(0.1),
                Planned::Predict(0.8),
            ],
        },
        Entrant {
            owner: miner_b(),
            artifact: artifact_for(miner_b(), 0x0b),
            plan: [
                Planned::RetryThenPredict(0.7),
                Planned::Predict(0.3),
                Planned::Fail(Outcome::ParseFailure),
            ],
        },
        Entrant {
            owner: miner_c(),
            artifact: artifact_for(miner_c(), 0x0c),
            plan: [
                Planned::Predict(0.95),
                Planned::Predict(0.05),
                Planned::Predict(0.9),
            ],
        },
        Entrant {
            owner: stranger(),
            artifact: artifact_for(stranger(), 0x0d),
            plan: [
                Planned::Predict(1.0),
                Planned::Predict(0.0),
                Planned::Predict(1.0),
            ],
        },
    ]
}

fn prediction(p: f64) -> Outcome {
    let involvement = Probability::from_f64(p).expect("fixture probability");
    let share = Probability::new(involvement.ppb() / 2).expect("half is in range");
    Outcome::Prediction { involvement, share }
}

pub fn manifest() -> CohortManifest {
    let mut artifacts: Vec<_> = entrants()
        .iter()
        .map(|e| e.artifact.digest().expect("valid artifact"))
        .collect();
    artifacts.sort();
    CohortManifest {
        chain: chain(),
        cohort: 1,
        cohort_label: "synthetic-cohort-1".into(),
        opened_block: 3600,
        evaluation_set: Digest([0x22; 32]),
        task_count: 3,
        group_count: 2,
        baseline_artifact: baseline().artifact.digest().expect("valid artifact"),
        reward_rule: RewardRule::TestnetRewardV0,
        max_attempts: 2,
        allow_list: vec![miner_a(), miner_b(), miner_c()],
        burn_identity: owner(),
        artifacts,
    }
}

/// Everything the coordinator produces for the synthetic cohort.
pub struct Cohort {
    pub manifest: CohortManifest,
    pub assignments: Vec<Assignment>,
    pub attempts: Vec<Attempt>,
    pub results: Vec<AttemptResult>,
    pub receipts: Vec<AttemptReceipt>,
    pub index: ReceiptIndex,
    pub baseline: Score,
    pub scores: Vec<MinerScore>,
}

pub fn cohort() -> Result<Cohort> {
    let manifest = manifest();
    let manifest_id = manifest.digest()?;
    let mut out = Cohort {
        manifest,
        assignments: Vec::new(),
        attempts: Vec::new(),
        results: Vec::new(),
        receipts: Vec::new(),
        index: ReceiptIndex {
            cohort_manifest: manifest_id,
            entries: Vec::new(),
        },
        baseline: Score::ZERO,
        scores: Vec::new(),
    };
    let mut all = vec![baseline()];
    all.extend(entrants());
    for (n, entrant) in all.iter().enumerate() {
        let artifact = entrant.artifact.digest()?;
        let mut tasks = Vec::new();
        let (mut succeeded, mut failed) = (0, 0);
        for (task, planned) in entrant.plan.iter().enumerate() {
            let assignment = Assignment {
                cohort_manifest: manifest_id,
                artifact,
                task_index: task as u32,
                group: GROUPS[task],
            };
            assignment.check_manifest(&out.manifest)?;
            let outcomes = match planned {
                Planned::Predict(p) => vec![prediction(*p)],
                Planned::Fail(o) => vec![o.clone()],
                Planned::RetryThenPredict(p) => vec![Outcome::InfrastructureError, prediction(*p)],
            };
            let mut numbered = Vec::new();
            for (i, outcome) in outcomes.iter().enumerate() {
                let attempt = Attempt {
                    assignment: assignment.digest()?,
                    number: i as u8 + 1,
                };
                let result = AttemptResult {
                    attempt: attempt.digest()?,
                    outcome: outcome.clone(),
                };
                let salt = domain_hash("slop_ninja.fixture-salt", &result.attempt.0).0;
                out.receipts
                    .push(AttemptReceipt::new(&assignment, &attempt, &result, &salt)?);
                out.attempts.push(attempt);
                out.results.push(result);
                numbered.push((i as u8 + 1, outcome));
            }
            let involvement = reward::final_involvement(&numbered)?;
            if involvement.is_some() {
                succeeded += 1;
            } else {
                failed += 1;
            }
            tasks.push(ScoredTask {
                group: GROUPS[task],
                involvement,
                label: LABELS[task],
            });
            out.assignments.push(assignment);
        }
        let score = reward::aggregate_score(&tasks, out.manifest.group_count)?;
        if n == 0 {
            out.baseline = score;
        } else {
            out.scores.push(MinerScore {
                identity: entrant.owner,
                artifact,
                score,
                assigned: 3,
                succeeded,
                failed,
            });
        }
    }
    out.scores.sort_by_key(|s| s.identity);
    out.index.entries = out
        .receipts
        .iter()
        .map(ReceiptIndexEntry::for_receipt)
        .collect::<Result<_>>()?;
    out.index.entries.sort_by_key(|e| e.attempt);
    Ok(out)
}

pub fn package(cohort: &Cohort) -> Result<ScorePackage> {
    Ok(ScorePackage {
        chain: chain(),
        cohort: cohort.manifest.cohort,
        cohort_label: cohort.manifest.cohort_label.clone(),
        cohort_manifest: cohort.manifest.digest()?,
        revision: 1,
        reward_rule: RewardRule::TestnetRewardV0,
        baseline: cohort.baseline,
        allow_list: cohort.manifest.allow_list.clone(),
        burn_identity: cohort.manifest.burn_identity,
        scores: cohort.scores.clone(),
        receipt_root: cohort.index.digest()?,
        receipt_count: cohort.index.count(),
    })
}

pub fn snapshot() -> RegistrationSnapshot {
    RegistrationSnapshot {
        chain: chain(),
        block: 3960,
        block_hash: Digest([0x33; 32]),
        neurons: vec![
            owner(),
            miner_a(),
            miner_b(),
            identity(7, 0xc9, 1500),
            stranger(),
        ],
    }
}

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
    fn record<R: Record>(&mut self, key: &str, record: &R) -> Result<()> {
        self.line(&format!("{key}.bytes"), hex(&record.to_bytes()?));
        self.line(&format!("{key}.digest"), record.digest()?);
        Ok(())
    }
    fn signed<R: Record>(&mut self, key: &str, signed: &Signed<R>) -> Result<()> {
        self.line(
            &format!("{key}.signing_payload"),
            hex(&signing_payload(&signed.record)?),
        );
        self.line(&format!("{key}.signature"), hex(&signed.signature));
        self.line(
            &format!("{key}.envelope_sha256"),
            crate::object_sha256(&signed.to_bytes()?),
        );
        Ok(())
    }
    fn row(&mut self, key: &str, row: &WeightRow) -> Result<()> {
        let entries: Vec<_> = row
            .entries
            .iter()
            .map(|e| format!("{}:{}", e.uid, e.weight))
            .collect();
        self.line(&format!("{key}.entries"), entries.join(","));
        self.line(&format!("{key}.digest"), row.digest()?);
        Ok(())
    }
}

fn code<T>(r: Result<T>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(e) => e.code(),
    }
}

fn flip(bytes: &[u8], at: usize) -> Vec<u8> {
    let mut out = bytes.to_vec();
    out[at] ^= 1;
    out
}

/// Builds the golden report.
pub fn golden_report() -> Result<String> {
    let mut r = Report(String::new());
    r.line("schema", "slop_ninja.protocol.golden/v1");

    for p in [0.0, 1.0, 0.5, 0.1, 1e-10, 5e-10, 1.5e-9, 1.0 / 3.0] {
        r.result(
            &format!("probability.{p:e}"),
            Probability::from_f64(p).map(Probability::ppb),
        );
    }
    for p in [f64::NAN, f64::INFINITY, -1e-12, 1.000_000_1] {
        r.result(
            &format!("probability.{p:e}"),
            Probability::from_f64(p).map(Probability::ppb),
        );
    }
    for (ppb, label) in [
        (0, false),
        (0, true),
        (500_000_000, true),
        (123_456_789, false),
    ] {
        r.line(
            &format!("brier.{ppb}.{label}"),
            reward::brier_utility(Probability::new(ppb)?, label),
        );
    }
    let infra = Outcome::InfrastructureError;
    let parse = Outcome::ParseFailure;
    let good = prediction(0.25);
    r.result(
        "attempts.infra_then_prediction",
        reward::final_involvement(&[(1, &infra), (2, &good)])
            .map(|v| format!("{:?}", v.map(Probability::ppb))),
    );
    r.result(
        "attempts.parse_then_prediction",
        reward::final_involvement(&[(1, &parse), (2, &good)])
            .map(|v| format!("{:?}", v.map(Probability::ppb))),
    );
    r.result(
        "attempts.all_infra",
        reward::final_involvement(&[(1, &infra), (2, &infra)]).map(|_| "resolved"),
    );
    r.result(
        "attempts.misnumbered",
        reward::final_involvement(&[(2, &good)]).map(|_| "resolved"),
    );

    let cohort = cohort()?;
    r.record("manifest", &cohort.manifest)?;
    r.record("artifact.a", &entrants()[0].artifact)?;
    let nonce = [0x44; 32];
    r.line(
        "artifact.a.admission_commitment",
        admission_commitment(&entrants()[0].artifact.digest()?, &nonce),
    );
    r.record("assignment.0", &cohort.assignments[0])?;
    r.record("attempt.0", &cohort.attempts[0])?;
    r.record("result.0", &cohort.results[0])?;
    r.record("receipt.0", &cohort.receipts[0])?;
    r.line("receipts.count", cohort.receipts.len());
    r.record("receipt_index", &cohort.index)?;
    r.line("score.baseline", cohort.baseline.get());
    for s in &cohort.scores {
        r.line(
            &format!("score.uid{}", s.identity.uid),
            format_args!(
                "{} q={} succeeded={} failed={}",
                s.score.get(),
                reward::improvement(s.score, cohort.baseline),
                s.succeeded,
                s.failed
            ),
        );
    }
    let package = package(&cohort)?;
    package.check_receipts(&cohort.index)?;
    r.record("package", &package)?;
    let target = CalculationTarget::for_package(&package, None)?;
    r.record("target", &target)?;
    let snapshot = snapshot();
    r.record("snapshot", &snapshot)?;
    let row = reward::derive_row(&target, &package, &snapshot)?;
    r.row("row", &row)?;
    r.line("row.bytes", hex(&row.to_bytes()?));

    let signed_manifest = sign(cohort.manifest.clone(), &package_key())?;
    r.signed("signed.manifest", &signed_manifest)?;
    let signed_package = sign(package.clone(), &package_key())?;
    r.signed("signed.package", &signed_package)?;
    let signed_target = sign(target.clone(), &package_key())?;
    r.signed("signed.target", &signed_target)?;
    let signed_index = sign(cohort.index.clone(), &receipt_key())?;
    r.signed("signed.receipt_index", &signed_index)?;

    r.result(
        "operation.adapter_compat",
        weight_operation_id(&[1; 32], 7, &[2; 32], &[3; 32], "synthetic-cohort", 1, 0),
    );
    r.result(
        "operation.target",
        WeightOperation::for_target(&target, AccountId([0x66; 32])).id(),
    );
    r.result(
        "operation.tempo",
        WeightOperation::for_target(&target, AccountId([0x66; 32]))
            .id()
            .and_then(|base| weight_tempo_operation_id(&base, 23_083)),
    );

    reward_cases(&mut r, &cohort, &package, &snapshot)?;
    rejection_cases(&mut r, &cohort, &package, &target, &signed_package)?;
    Ok(r.0)
}

fn reward_cases(
    r: &mut Report,
    cohort: &Cohort,
    package: &ScorePackage,
    snapshot: &RegistrationSnapshot,
) -> Result<()> {
    let derive = |p: &ScorePackage, s: &RegistrationSnapshot| {
        let t = CalculationTarget::for_package(p, None)?;
        reward::derive_row(&t, p, s)
    };

    // Nothing allow-listed: the full row goes to UID 0.
    let mut empty = package.clone();
    empty.allow_list.clear();
    r.row("reward.empty_pool", &derive(&empty, snapshot)?)?;

    // A perfect baseline leaves no headroom to reward.
    let mut perfect = package.clone();
    perfect.baseline = Score::ONE;
    r.row("reward.perfect_baseline", &derive(&perfect, snapshot)?)?;

    // Every eligible score at or below the baseline.
    let mut below = package.clone();
    below.baseline = Score::new(
        cohort
            .scores
            .iter()
            .map(|s| s.score.get())
            .max()
            .unwrap_or(0),
    )?;
    r.row(
        "reward.all_at_or_below_baseline",
        &derive(&below, snapshot)?,
    )?;

    // Miner C still registered: its own UID is credited instead of UID 0.
    let mut registered = snapshot.clone();
    registered.neurons[3] = miner_c();
    r.row("reward.uid_7_registered", &derive(package, &registered)?)?;

    // Miner A deregistered and UID 3 reused with a later registration block.
    let mut reused = snapshot.clone();
    reused.neurons[1].registration_block = 5000;
    r.row("reward.uid_3_reused", &derive(package, &reused)?)?;

    // Rounding: 65535 / 2 = 32767.5 rounds half up; 65535 / 3 = 21845 exactly.
    r.result(
        "quantize.half_up",
        reward::quantize(&[(1, 1), (2, 2)]).map(|row| format!("{row:?}")),
    );
    r.result(
        "quantize.equal_credits",
        reward::quantize(&[(1, 5), (2, 5), (4, 15)]).map(|row| format!("{row:?}")),
    );
    r.result(
        "quantize.tiny_dropped",
        reward::quantize(&[(1, 1), (2, 1_000_000)]).map(|row| format!("{row:?}")),
    );
    r.result(
        "quantize.overflow",
        reward::quantize(&[(1, u128::MAX)]).map(|row| format!("{row:?}")),
    );
    r.result(
        "quantize.zero",
        reward::quantize(&[]).map(|row| format!("{row:?}")),
    );
    r.result(
        "aggregate.empty_group",
        reward::aggregate_score(
            &[ScoredTask {
                group: 0,
                involvement: None,
                label: true,
            }],
            2,
        )
        .map(Score::get),
    );
    r.result(
        "aggregate.group_out_of_range",
        reward::aggregate_score(
            &[ScoredTask {
                group: 3,
                involvement: None,
                label: true,
            }],
            2,
        )
        .map(Score::get),
    );
    Ok(())
}

fn rejection_cases(
    r: &mut Report,
    cohort: &Cohort,
    package: &ScorePackage,
    target: &CalculationTarget,
    signed: &Signed<ScorePackage>,
) -> Result<()> {
    let keys = trusted_keys();
    let bytes = package.to_bytes()?;
    let mut trailing = bytes.clone();
    trailing.push(0);
    r.line("reject.trailing", code(ScorePackage::from_bytes(&trailing)));
    r.line(
        "reject.truncated",
        code(ScorePackage::from_bytes(&bytes[..bytes.len() - 1])),
    );
    r.line(
        "reject.wrong_kind",
        code(CalculationTarget::from_bytes(&bytes)),
    );
    // Kind is `u32 len || "score-package"`; the version follows at offset 17.
    r.line(
        "reject.wrong_version",
        code(ScorePackage::from_bytes(&flip(&bytes, 17))),
    );

    let mut unsorted = package.clone();
    unsorted.allow_list.swap(0, 1);
    r.line("reject.unsorted_allow_list", code(unsorted.to_bytes()));
    let mut uid0 = package.clone();
    uid0.allow_list.insert(0, owner());
    r.line("reject.uid0_allow_list", code(uid0.to_bytes()));
    let mut duplicate = package.clone();
    duplicate.scores.insert(1, duplicate.scores[0].clone());
    r.line("reject.duplicate_score", code(duplicate.to_bytes()));
    let mut copied = package.clone();
    copied.scores[1].artifact = copied.scores[0].artifact;
    r.line("reject.duplicate_artifact", code(copied.to_bytes()));
    let mut counts = package.clone();
    counts.scores[0].failed += 1;
    r.line("reject.outcome_count", code(counts.to_bytes()));
    let mut receipts = package.clone();
    receipts.receipt_count = 1;
    r.line("reject.receipt_count", code(receipts.to_bytes()));
    let mut unsorted_snapshot = self::snapshot();
    unsorted_snapshot.neurons.swap(0, 1);
    r.line(
        "reject.unsorted_snapshot",
        code(unsorted_snapshot.to_bytes()),
    );

    let mut score_bytes = bytes.clone();
    let score_at = find(&bytes, &package.scores[0].score.get().to_le_bytes());
    score_bytes[score_at..score_at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    r.line(
        "reject.score_out_of_range",
        code(ScorePackage::from_bytes(&score_bytes)),
    );
    let tag_record = {
        let mut a = cohort.results[0].clone();
        a.outcome = Outcome::ModelError;
        let mut b = a.to_bytes()?;
        let last = b.len() - 1;
        b[last] = 9;
        b
    };
    r.line(
        "reject.invalid_tag",
        code(AttemptResult::from_bytes(&tag_record)),
    );
    let inverted = AttemptResult {
        attempt: cohort.results[0].attempt,
        outcome: Outcome::Prediction {
            involvement: Probability::new(1)?,
            share: Probability::new(2)?,
        },
    };
    r.line(
        "reject.share_exceeds_involvement",
        code(inverted.to_bytes()),
    );

    let envelope = signed.to_bytes()?;
    r.line(
        "reject.signed_ok",
        code(Signed::<ScorePackage>::open(&envelope, &keys)),
    );
    r.line(
        "reject.signature_tampered",
        code(Signed::<ScorePackage>::open(
            &flip(&envelope, envelope.len() - 1),
            &keys,
        )),
    );
    let record_byte = envelope.len() - 96 - 9;
    r.line(
        "reject.signed_record_tampered",
        code(Signed::<ScorePackage>::open(
            &flip(&envelope, record_byte),
            &keys,
        )),
    );
    let wrong_role = sign(package.clone(), &receipt_key())?;
    r.line(
        "reject.receipt_key_signs_package",
        code(wrong_role.verify(&keys)),
    );
    r.line(
        "reject.package_envelope_as_target",
        code(Signed::<CalculationTarget>::from_bytes(&envelope)),
    );
    let index_envelope = sign(cohort.index.clone(), &receipt_key())?.to_bytes()?;
    r.line(
        "reject.index_envelope_as_package",
        code(Signed::<ScorePackage>::from_bytes(&index_envelope)),
    );
    r.line(
        "reject.duplicate_trusted_keys",
        code(TrustedKeys::new([1; 32], [1; 32], [2; 32])),
    );
    r.line(
        "reject.unsigned_kind",
        code(signing_payload(&cohort.receipts[0])),
    );

    r.line(
        "reject.package_manifest_ok",
        code(package.check_manifest(&cohort.manifest)),
    );
    let mut relisted = package.clone();
    relisted.allow_list.pop();
    r.line(
        "reject.package_manifest_mismatch",
        code(relisted.check_manifest(&cohort.manifest)),
    );
    let mut moved = package.clone();
    moved.revision += 1;
    r.line(
        "reject.target_package_mismatch",
        code(target.check_package(&moved)),
    );
    let mut short_index = cohort.index.clone();
    short_index.entries.pop();
    r.line(
        "reject.receipt_index_mismatch",
        code(package.check_receipts(&short_index)),
    );
    let mut unsorted_index = cohort.index.clone();
    unsorted_index.entries.swap(0, 1);
    r.line(
        "reject.unsorted_receipt_index",
        code(unsorted_index.to_bytes()),
    );
    let position = cohort
        .index
        .entries
        .iter()
        .position(|e| e.attempt == cohort.receipts[0].attempt)
        .ok_or(Error::Invalid("fixture receipt"))?;
    let object = cohort.receipts[0].to_bytes()?;
    r.line(
        "reject.receipt_object_ok",
        code(cohort.index.verify_object(position, &object)),
    );
    r.line(
        "reject.receipt_object_tampered",
        code(
            cohort
                .index
                .verify_object(position, &flip(&object, object.len() - 1)),
        ),
    );
    let mut other_netuid = self::snapshot();
    other_netuid.chain.netuid += 1;
    r.line(
        "reject.snapshot_chain",
        code(reward::derive_row(target, package, &other_netuid)),
    );

    // UID 0 held by anyone but the bound burn identity refuses the row.
    let mut foreign = self::snapshot();
    foreign.neurons[0] = foreign_uid0();
    r.line(
        "reject.foreign_uid0",
        code(reward::derive_row(target, package, &foreign)),
    );
    let mut no_uid0 = self::snapshot();
    no_uid0.neurons.remove(0);
    r.line(
        "reject.missing_uid0",
        code(reward::derive_row(target, package, &no_uid0)),
    );
    let mut burn_uid = package.clone();
    burn_uid.burn_identity.uid = 1;
    r.line("reject.burn_identity_uid", code(burn_uid.to_bytes()));
    let mut burn_moved = package.clone();
    burn_moved.burn_identity = foreign_uid0();
    r.line(
        "reject.package_manifest_burn",
        code(burn_moved.check_manifest(&cohort.manifest)),
    );

    // testnet-reward-v0 is refused on any chain but the testnet.
    let mut finney_manifest = cohort.manifest.clone();
    finney_manifest.chain = other_chain();
    r.line(
        "reject.rule_genesis.manifest",
        code(finney_manifest.to_bytes()),
    );
    let mut finney_package = package.clone();
    finney_package.chain = other_chain();
    r.line(
        "reject.rule_genesis.package",
        code(finney_package.to_bytes()),
    );
    r.line(
        "reject.rule_genesis.decode",
        code(ScorePackage::from_bytes(&unvalidated_bytes(
            &finney_package,
        ))),
    );
    let mut finney_snapshot = self::snapshot();
    finney_snapshot.chain = other_chain();
    r.line(
        "reject.rule_genesis.derive",
        code(reward::derive_row(
            target,
            &finney_package,
            &finney_snapshot,
        )),
    );

    r.line(
        "reject.sequence_claim",
        code(ScorePackage::from_bytes(&sequence_claim(package, 64))),
    );
    r.line(
        "reject.empty_label",
        code(weight_operation_id(
            &[0; 32], 1, &[0; 32], &[0; 32], "", 0, 0,
        )),
    );
    Ok(())
}

fn find(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("fixture field present")
}
