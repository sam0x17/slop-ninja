//! Content-free attempt receipts, stored in chunks.
//!
//! Every inference attempt gets a v1 `AttemptReceipt` (IDs, outcome class and a
//! salted result commitment; never text, labels or predictions). Receipts are
//! written as chunk objects, one per scorer request, instead of one object per
//! document: a chunk is the concatenation of `u32_le(len) || receipt bytes`, and
//! the v1 receipt index still binds each receipt's own SHA-256 and length, so
//! `ReceiptIndex::verify_object` checks every receipt cut from a chunk exactly as
//! it would a separate object. A chunk map (key, SHA-256, length, count) lets a
//! verifier fetch and check every chunk object.
//!
//! Fan-out: a scoring lane builds its chunk's receipt object with
//! [`build_chunk`], a pure function of the claim, the outcomes and the cohort
//! seed, and stores it before committing; the cohort object rebuilds the same
//! bytes from the committed outcomes and refuses a commit whose object digest or
//! length differs, so it stays the authority over receipts. A bundle lane
//! concatenates one job's referenced chunk objects in key order into one
//! object, which is itself a valid chunk object ([`verify_bundle`]).

use slop_ninja_protocol::{
    Assignment, Attempt, AttemptReceipt, AttemptResult, Digest, Outcome, OutcomeClass, ReceiptIndex,
    ReceiptIndexEntry, Record, domain_hash,
};

pub const CHUNK_FORMAT: &str = "slop_ninja_receipt_chunk_v1";
const MAX_RECEIPT_LEN: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiptError(pub &'static str);

/// The private per-attempt salt: derived from the cohort's private seed.
pub fn salt(cohort_seed: &[u8; 32], attempt: &Digest) -> [u8; 32] {
    let mut payload = cohort_seed.to_vec();
    payload.extend_from_slice(&attempt.0);
    domain_hash("slop_ninja.eval/result-salt/v1", &payload).0
}

/// One attempt's protocol records.
#[derive(Clone, Debug)]
pub struct AttemptRecords {
    pub assignment: Assignment,
    pub attempt: Attempt,
    pub result: AttemptResult,
    pub receipt: AttemptReceipt,
    pub bytes: Vec<u8>,
}

/// Builds the records of attempt `number` of task `task_index` (its own group) for
/// `artifact` under `manifest`.
pub fn attempt_records(
    manifest: &Digest,
    artifact: &Digest,
    task_index: u32,
    number: u8,
    outcome: Outcome,
    cohort_seed: &[u8; 32],
) -> Result<AttemptRecords, ReceiptError> {
    let assignment = Assignment {
        cohort_manifest: *manifest,
        artifact: *artifact,
        task_index,
        group: task_index,
    };
    let attempt = Attempt {
        assignment: assignment.digest().map_err(|_| ReceiptError("assignment"))?,
        number,
    };
    let attempt_id = attempt.digest().map_err(|_| ReceiptError("attempt"))?;
    let result = AttemptResult {
        attempt: attempt_id,
        outcome,
    };
    let receipt = AttemptReceipt::new(&assignment, &attempt, &result, &salt(cohort_seed, &attempt_id))
        .map_err(|_| ReceiptError("receipt"))?;
    let bytes = receipt.to_bytes().map_err(|_| ReceiptError("receipt bytes"))?;
    Ok(AttemptRecords {
        assignment,
        attempt,
        result,
        receipt,
        bytes,
    })
}

pub fn encode_chunk(receipts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for r in receipts {
        out.extend_from_slice(&(r.len() as u32).to_le_bytes());
        out.extend_from_slice(r);
    }
    out
}

pub fn decode_chunk(bytes: &[u8]) -> Result<Vec<&[u8]>, ReceiptError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let len = bytes
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
            .ok_or(ReceiptError("chunk length prefix"))?;
        if len == 0 || len > MAX_RECEIPT_LEN {
            return Err(ReceiptError("chunk receipt length"));
        }
        let r = bytes.get(i + 4..i + 4 + len).ok_or(ReceiptError("chunk truncated"))?;
        out.push(r);
        i += 4 + len;
    }
    Ok(out)
}

/// Index entry for stored receipt bytes.
pub fn index_entry(bytes: &[u8]) -> Result<ReceiptIndexEntry, ReceiptError> {
    let receipt = AttemptReceipt::from_bytes(bytes).map_err(|_| ReceiptError("receipt decode"))?;
    ReceiptIndexEntry::for_receipt(&receipt).map_err(|_| ReceiptError("index entry"))
}

/// The sorted receipt index over every stored receipt of a cohort.
pub fn build_index(manifest: Digest, mut entries: Vec<ReceiptIndexEntry>) -> Result<ReceiptIndex, ReceiptError> {
    entries.sort_by_key(|a| a.attempt);
    let index = ReceiptIndex {
        cohort_manifest: manifest,
        entries,
    };
    index.validate().map_err(|_| ReceiptError("receipt index"))?;
    Ok(index)
}

/// Verifies every receipt in the given chunks against `index`: each must match its
/// entry by attempt ID, digest and length, and every entry must be covered once.
pub fn verify_chunks(index: &ReceiptIndex, chunks: &[Vec<u8>]) -> Result<Vec<AttemptReceipt>, ReceiptError> {
    let mut seen = vec![false; index.entries.len()];
    let mut out = Vec::new();
    for chunk in chunks {
        for bytes in decode_chunk(chunk)? {
            let r = AttemptReceipt::from_bytes(bytes).map_err(|_| ReceiptError("receipt decode"))?;
            let pos = index
                .entries
                .binary_search_by(|e| e.attempt.cmp(&r.attempt))
                .map_err(|_| ReceiptError("receipt not in index"))?;
            if std::mem::replace(&mut seen[pos], true) {
                return Err(ReceiptError("receipt stored twice"));
            }
            out.push(index.verify_object(pos, bytes).map_err(|_| ReceiptError("receipt object mismatch"))?);
        }
    }
    if seen.iter().any(|s| !s) {
        return Err(ReceiptError("index entry without a stored receipt"));
    }
    Ok(out)
}

/// One task's outcome as a lane commits it (and as the private results object
/// stores it). Content-free apart from the prediction itself.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskOutcome {
    pub task: u32,
    /// [`outcome_tag`] of the class.
    pub class: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inv_ppb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_ppb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u32>,
    /// A model error decided by crash isolation: the document crashed its
    /// container alone, on an instance that then scored a control document.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub isolated_crash: bool,
    /// A model error because the job's artifact failed (it could not be loaded,
    /// or could not score a control document, on fresh instances).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub artifact_failed: bool,
    /// A reference job's document the scorer left without an answer (its
    /// error for the document, or a crash reproduced on a proven instance):
    /// the receipt records the model error the scorer gave, but a reference
    /// is never attributed, so the commit keeps no outcome row for it and
    /// the document stays unresolved (`lane_logic::unanswered`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unattributed: bool,
    /// The span gate's statistics of a resolved batch document, for a cohort
    /// that requests spans (`span_gate::SpanCell`). Never part of a receipt:
    /// receipts are a function of the document outcome alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<crate::eval::span_gate::SpanCell>,
}

impl TaskOutcome {
    pub fn new(task: u32, o: &Outcome, human: Option<String>, ai: Option<String>, tokens: Option<u32>) -> Self {
        let (inv, share) = match o {
            Outcome::Prediction { involvement, share } => (Some(involvement.ppb()), Some(share.ppb())),
            _ => (None, None),
        };
        Self {
            task,
            class: outcome_tag(o.class()) as u8,
            inv_ppb: inv,
            share_ppb: share,
            human,
            ai,
            tokens,
            isolated_crash: false,
            artifact_failed: false,
            unattributed: false,
            span: None,
        }
    }

    /// Whether the commit keeps an outcome row for this document (it is
    /// resolved): every outcome except an infrastructure error and a
    /// reference's unattributed one ([`TaskOutcome::unattributed`]).
    pub fn resolves(&self) -> bool {
        self.class != 5 && !self.unattributed
    }

    pub fn infrastructure(task: u32) -> Self {
        Self::new(task, &Outcome::InfrastructureError, None, None, None)
    }

    /// A model error decided by crash isolation.
    pub fn isolated_crash(task: u32) -> Self {
        Self { isolated_crash: true, ..Self::new(task, &Outcome::ModelError, None, None, None) }
    }

    /// A model error because the job's artifact failed.
    pub fn artifact_failed(task: u32) -> Self {
        Self { artifact_failed: true, ..Self::new(task, &Outcome::ModelError, None, None, None) }
    }

    pub fn outcome(&self) -> Result<Outcome, ReceiptError> {
        use slop_ninja_protocol::Probability;
        Ok(match self.class {
            1 => {
                let p = |v: Option<u32>| v.and_then(|x| Probability::new(x).ok()).ok_or(ReceiptError("prediction"));
                let (involvement, share) = (p(self.inv_ppb)?, p(self.share_ppb)?);
                if share > involvement {
                    return Err(ReceiptError("share above involvement"));
                }
                Outcome::Prediction { involvement, share }
            }
            2 => Outcome::ModelError,
            3 => Outcome::InvalidOutput,
            4 => Outcome::ParseFailure,
            5 => Outcome::InfrastructureError,
            _ => return Err(ReceiptError("outcome class")),
        })
    }
}

/// Everything a chunk's receipts depend on: the claim and the cohort.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkFacts<'a> {
    pub manifest: Digest,
    pub artifact: Digest,
    pub seed: &'a [u8; 32],
    pub prefix: &'a str,
    pub job: i64,
    pub chunk: i64,
    /// The attempt run (record-only: the highest lost attempt).
    pub attempt: u8,
    pub lost: &'a [u8],
    pub record_only: bool,
    /// The unresolved tasks the claim covered, ascending.
    pub tasks: &'a [u32],
}

/// A chunk's receipt object and its receipts, built identically by the lane
/// and, from the committed outcomes, by the cohort object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkReceipts {
    pub object_key: String,
    pub object: Vec<u8>,
    /// (attempt id, task, attempt number, class, receipt bytes).
    pub receipts: Vec<(Digest, u32, u8, OutcomeClass, Vec<u8>)>,
    /// The highest attempt this object records.
    pub recorded: u8,
}

pub fn chunk_object_key(prefix: &str, job: i64, chunk: i64, attempt: u8, record_only: bool) -> String {
    format!("{prefix}/receipts/job-{job:03}/chunk-{chunk:04}-attempt-{attempt}{}.bin", if record_only { "-lost" } else { "" })
}

/// Builds a chunk's receipts: one infrastructure-error receipt per lost attempt
/// per task, then (unless record-only) the run attempt's outcome per task.
/// `outcomes` must name exactly `facts.tasks` (record-only: none).
pub fn build_chunk(facts: &ChunkFacts<'_>, outcomes: &[TaskOutcome]) -> Result<ChunkReceipts, ReceiptError> {
    if facts.record_only {
        if !outcomes.is_empty() {
            return Err(ReceiptError("a record-only claim has no outcomes"));
        }
    } else {
        let named: Vec<u32> = outcomes.iter().map(|o| o.task).collect();
        if named != facts.tasks {
            return Err(ReceiptError("outcomes do not name the claimed tasks"));
        }
    }
    let mut receipts = Vec::new();
    let mut objects = Vec::new();
    for (n, task) in facts.tasks.iter().enumerate() {
        for &lost in facts.lost {
            let r = attempt_records(&facts.manifest, &facts.artifact, *task, lost, Outcome::InfrastructureError, facts.seed)?;
            receipts.push((r.receipt.attempt, *task, lost, OutcomeClass::InfrastructureError, r.bytes.clone()));
            objects.push(r.bytes);
        }
        if facts.record_only {
            continue;
        }
        let o = outcomes[n].outcome()?;
        let r = attempt_records(&facts.manifest, &facts.artifact, *task, facts.attempt, o.clone(), facts.seed)?;
        receipts.push((r.receipt.attempt, *task, facts.attempt, o.class(), r.bytes.clone()));
        objects.push(r.bytes);
    }
    let recorded = facts.lost.iter().copied().chain((!facts.record_only).then_some(facts.attempt)).max().unwrap_or(0);
    Ok(ChunkReceipts {
        object_key: chunk_object_key(facts.prefix, facts.job, facts.chunk, facts.attempt, facts.record_only),
        object: encode_chunk(&objects),
        receipts,
        recorded,
    })
}

/// Checks a bundle against its job's receipt rows: every receipt it holds is
/// one of `expected` (attempt id -> (SHA-256, length)), at most once, and every
/// expected receipt is present. Returns the receipt count.
pub fn verify_bundle(
    bundle: &[u8],
    expected: &std::collections::BTreeMap<Digest, ([u8; 32], u32)>,
) -> Result<u64, ReceiptError> {
    use sha2::{Digest as _, Sha256};
    let mut seen = std::collections::BTreeSet::new();
    for bytes in decode_chunk(bundle)? {
        let r = AttemptReceipt::from_bytes(bytes).map_err(|_| ReceiptError("receipt decode"))?;
        let (sha, len) = expected.get(&r.attempt).ok_or(ReceiptError("bundle receipt not in the job's rows"))?;
        let got: [u8; 32] = Sha256::digest(bytes).into();
        if &got != sha || bytes.len() as u32 != *len {
            return Err(ReceiptError("bundle receipt differs from its row"));
        }
        if !seen.insert(r.attempt) {
            return Err(ReceiptError("bundle receipt stored twice"));
        }
    }
    if seen.len() != expected.len() {
        return Err(ReceiptError("bundle misses a receipt of its job"));
    }
    Ok(seen.len() as u64)
}

pub fn outcome_tag(class: OutcomeClass) -> i64 {
    match class {
        OutcomeClass::Prediction => 1,
        OutcomeClass::ModelError => 2,
        OutcomeClass::InvalidOutput => 3,
        OutcomeClass::ParseFailure => 4,
        OutcomeClass::InfrastructureError => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_protocol::Probability;

    #[test]
    fn chunked_receipts_verify_against_the_index() {
        let manifest = Digest([1; 32]);
        let artifact = Digest([2; 32]);
        let seed = [3; 32];
        let mut chunks = Vec::new();
        let mut entries = Vec::new();
        for c in 0..3u32 {
            let mut objs = Vec::new();
            for t in 0..5u32 {
                let outcome = if t == 4 {
                    Outcome::InfrastructureError
                } else {
                    Outcome::Prediction {
                        involvement: Probability::new(900_000_000).unwrap(),
                        share: Probability::new(400_000_000).unwrap(),
                    }
                };
                let r = attempt_records(&manifest, &artifact, c * 5 + t, 1, outcome, &seed).unwrap();
                entries.push(index_entry(&r.bytes).unwrap());
                objs.push(r.bytes);
            }
            chunks.push(encode_chunk(&objs));
        }
        let index = build_index(manifest, entries).unwrap();
        assert_eq!(index.count(), 15);
        assert_eq!(verify_chunks(&index, &chunks).unwrap().len(), 15);
        // A missing chunk, a duplicated chunk and a flipped byte are refused.
        assert!(verify_chunks(&index, &chunks[..2]).is_err());
        let mut dup = chunks.clone();
        dup.push(chunks[0].clone());
        assert!(verify_chunks(&index, &dup).is_err());
        let mut bad = chunks.clone();
        let last = bad[1].len() - 1;
        bad[1][last] ^= 1;
        assert!(verify_chunks(&index, &bad).is_err());
        // The salt is private and per attempt: the same outcome commits differently.
        let a = attempt_records(&manifest, &artifact, 0, 1, Outcome::ModelError, &seed).unwrap();
        let b = attempt_records(&manifest, &artifact, 0, 1, Outcome::ModelError, &[4; 32]).unwrap();
        assert_ne!(a.receipt.result_commitment, b.receipt.result_commitment);
        assert_eq!(a.receipt.attempt, b.receipt.attempt);
    }

    #[test]
    fn chunk_builds_are_deterministic_and_bundles_verify() {
        use sha2::{Digest as _, Sha256};
        let seed = [3; 32];
        let tasks = [4u32, 5, 6];
        let facts = ChunkFacts {
            manifest: Digest([1; 32]),
            artifact: Digest([2; 32]),
            seed: &seed,
            prefix: "cohorts/x",
            job: 1,
            chunk: 2,
            attempt: 2,
            lost: &[1],
            record_only: false,
            tasks: &tasks,
        };
        let p = Outcome::Prediction {
            involvement: Probability::new(800_000_000).unwrap(),
            share: Probability::new(100_000_000).unwrap(),
        };
        let outs = vec![
            TaskOutcome::new(4, &p, Some("other_human".into()), None, Some(10)),
            TaskOutcome::new(5, &Outcome::ModelError, None, None, None),
            TaskOutcome::infrastructure(6),
        ];
        let a = build_chunk(&facts, &outs).unwrap();
        let b = build_chunk(&facts, &outs).unwrap();
        assert_eq!(a, b, "the lane and the cohort build identical bytes");
        assert_eq!(a.receipts.len(), 6, "a lost receipt and an outcome per task");
        assert_eq!(a.recorded, 2);
        assert_eq!(a.object_key, "cohorts/x/receipts/job-001/chunk-0002-attempt-2.bin");
        // A changed outcome changes the bytes; a wrong task list is refused.
        let mut other = outs.clone();
        other[1] = TaskOutcome::infrastructure(5);
        assert_ne!(build_chunk(&facts, &other).unwrap().object, a.object);
        assert!(build_chunk(&facts, &outs[..2]).is_err());
        let other_tasks = [7u32, 8, 9];
        let rec = ChunkFacts { record_only: true, attempt: 3, lost: &[1, 2, 3], chunk: 3, tasks: &other_tasks, ..facts.clone() };
        let r = build_chunk(&rec, &[]).unwrap();
        assert!(r.object_key.ends_with("attempt-3-lost.bin"));
        assert_eq!(r.receipts.len(), 9);
        // A bundle of both objects verifies against their rows, and only them.
        let mut expected = std::collections::BTreeMap::new();
        for (id, _, _, _, bytes) in a.receipts.iter().chain(r.receipts.iter()) {
            expected.insert(*id, (Sha256::digest(bytes).into(), bytes.len() as u32));
        }
        let mut bundle = a.object.clone();
        bundle.extend_from_slice(&r.object);
        assert_eq!(verify_bundle(&bundle, &expected).unwrap(), 15);
        assert!(verify_bundle(&a.object, &expected).is_err(), "a missing object");
        let mut dup = bundle.clone();
        dup.extend_from_slice(&a.object);
        assert!(verify_bundle(&dup, &expected).is_err());
        let mut flipped = bundle.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 1;
        assert!(verify_bundle(&flipped, &expected).is_err());
        // A wire outcome with share above involvement is refused.
        let mut bad = outs[0].clone();
        bad.share_ppb = Some(900_000_000);
        assert!(bad.outcome().is_err());
    }
}
