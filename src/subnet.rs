//! Portable, offline reference contract for a prose-revision subnet.
//!
//! These pure functions check supplied evidence. They do not authenticate a
//! miner, a detector response, or a human reviewer, and keep no replay ledger.
//! A network adapter must provide those checks before using a reference reward.

use crate::{evaluation::summarize_runs, provider_ids::ProviderIds, util::digest};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const PROTOCOL: &str = "unslop-v1";
const QUALITY_FIELDS: [&str; 7] = [
    "reviewed_by_human",
    "readability",
    "argumentation",
    "detail",
    "tone",
    "protected_invariants_preserved",
    "full_document",
];

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("{field} must be a nonempty string"))
}

fn integer(value: &Value, field: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("{field} must be a nonnegative integer"))
}

struct Contract<'a> {
    challenge_id: &'a str,
    nonce: &'a str,
    source_hash: &'a str,
    inventory_hash: String,
    detector: &'a str,
    model: &'a str,
    version: &'a str,
    threshold: f64,
    min_repeats: usize,
    max_candidates: u64,
    max_detector_queries: u64,
}

fn contract(challenge: &Value) -> Result<Contract<'_>> {
    ensure!(challenge.is_object(), "Challenge must be an object");
    ensure!(
        text(challenge, "protocol_version")? == PROTOCOL,
        "Unsupported challenge protocol_version"
    );
    let challenge_id = text(challenge, "challenge_id")?;
    let nonce = text(challenge, "nonce")?;
    let source = text(challenge, "source_text")?;
    let source_hash = text(challenge, "source_sha256")?;
    ensure!(
        source_hash == digest(source),
        "Challenge source_sha256 does not match exact source_text"
    );
    for field in ["domain", "register", "tone", "group_id"] {
        text(challenge, field)?;
    }
    ensure!(
        ["train", "dev", "test", "exploratory"].contains(&text(challenge, "split")?),
        "Invalid source-group split"
    );
    ensure!(
        challenge["full_document"] == true,
        "Reference rewards require full_document=true"
    );
    let budgets = &challenge["budgets"];
    let max_candidates = integer(budgets, "max_candidates")?;
    let max_detector_queries = integer(budgets, "max_detector_queries")?;
    ensure!(
        max_candidates > 0 && max_detector_queries > 0,
        "Challenge budgets must be positive"
    );
    let config = &challenge["detector"];
    let detector = text(config, "name")?;
    let model = text(config, "model")?;
    let version = text(config, "version")?;
    let threshold = config["threshold"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0 && *v <= 0.1)
        .ok_or_else(|| anyhow!("Detector threshold must be finite and in (0, 0.10]"))?;
    let repeats = integer(config, "min_repeats")?;
    ensure!(
        repeats >= 3 && repeats <= max_detector_queries,
        "min_repeats must be at least 3 and fit the detector query budget"
    );
    let min_repeats =
        usize::try_from(repeats).map_err(|_| anyhow!("min_repeats exceeds platform capacity"))?;
    let inventory = challenge["protected_invariants"]
        .as_array()
        .ok_or_else(|| anyhow!("protected_invariants must be an explicit array, possibly empty"))?;
    let mut ids = BTreeSet::new();
    for invariant in inventory {
        ensure!(
            invariant.is_object(),
            "Each protected invariant must be an object"
        );
        ensure!(
            ids.insert(text(invariant, "id")?),
            "Protected invariant IDs must be unique"
        );
        text(invariant, "requirement")?;
    }
    // serde_json's default map representation serializes object keys in order;
    // array order and the exact strings within the opaque inventory are retained.
    let inventory_hash = digest(&serde_json::to_string(&challenge["protected_invariants"])?);
    Ok(Contract {
        challenge_id,
        nonce,
        source_hash,
        inventory_hash,
        detector,
        model,
        version,
        threshold,
        min_repeats,
        max_candidates,
        max_detector_queries,
    })
}

/// Check exact challenge bindings and locally declared candidate limits.
/// Nonce uniqueness and cumulative budgets require an external ledger.
pub fn validate_submission(challenge: &Value, submission: &Value) -> Result<Value> {
    let c = contract(challenge)?;
    ensure!(submission.is_object(), "Submission must be an object");
    ensure!(
        text(submission, "protocol_version")? == PROTOCOL,
        "Unsupported submission protocol_version"
    );
    ensure!(
        text(submission, "challenge_id")? == c.challenge_id,
        "Submission challenge_id mismatch"
    );
    ensure!(
        text(submission, "nonce")? == c.nonce,
        "Submission nonce mismatch"
    );
    ensure!(
        text(submission, "source_sha256")? == c.source_hash,
        "Submission source_sha256 mismatch"
    );
    let candidate_id = text(submission, "candidate_id")?;
    let candidate_index = integer(submission, "candidate_index")?;
    ensure!(
        (1..=c.max_candidates).contains(&candidate_index),
        "candidate_index exceeds the candidate budget"
    );
    let revision = text(submission, "revised_text")?;
    let revision_hash = text(submission, "revision_sha256")?;
    ensure!(
        revision_hash == digest(revision),
        "revision_sha256 does not match exact revised_text"
    );
    text(submission, "process_version")?;
    let declared_queries = integer(submission, "declared_detector_queries")?;
    ensure!(
        declared_queries <= c.max_detector_queries,
        "Declared detector queries exceed challenge budget"
    );
    Ok(json!({
        "protocol_version": PROTOCOL, "valid_submission": true,
        "challenge_id": c.challenge_id, "nonce": c.nonce, "candidate_id": candidate_id,
        "candidate_index": candidate_index, "source_sha256": c.source_hash,
        "revision_sha256": revision_hash, "protected_invariants_sha256": c.inventory_hash,
        "unchanged_revision": revision_hash == c.source_hash,
        "declared_detector_queries": declared_queries,
        "declarations_authenticated": false, "replay_checked": false,
        "global_budget_verified": false, "network_ready": false,
        "external_checks_required": ["authenticated miner and challenge assignment", "nonce and candidate replay ledger",
            "cumulative candidate and query accounting", "source-group split isolation", "deadline and receipt time"],
    }))
}

/// Calculate a binary benchmark reward from validator-supplied observations and
/// a human audit. Inputs with wrong bindings are errors; incomplete evidence or
/// failed quality/score requirements receive zero. This does not set weights.
pub fn reward(
    challenge: &Value,
    submission: &Value,
    detector_records: &[Value],
    quality_audit: &Value,
    ids: &ProviderIds,
) -> Result<Value> {
    let validation = validate_submission(challenge, submission)?;
    let c = contract(challenge)?;
    let revision = text(submission, "revised_text")?;
    let revision_hash = text(submission, "revision_sha256")?;
    let candidate_id = text(submission, "candidate_id")?;
    ensure!(
        detector_records.len() as u128 <= c.max_detector_queries as u128,
        "Supplied detector records exceed the query budget"
    );
    for record in detector_records {
        ensure!(record.is_object(), "Detector record must be an object");
        ensure!(
            record["challenge_id"] == c.challenge_id
                && record["nonce"] == c.nonce
                && record["candidate_id"] == candidate_id,
            "Detector record challenge/candidate binding mismatch"
        );
        ensure!(
            record["submitted_text"] == revision && record["sha256"] == revision_hash,
            "Detector record must contain the exact revision bytes and hash"
        );
        ensure!(
            record["detector"] == c.detector
                && record["model"] == c.model
                && record["result"]["version"] == c.version,
            "Detector record selector/version mismatch"
        );
        ensure!(
            record["full_document"] == true,
            "Detector record must explicitly cover the full document"
        );
    }
    let summary = summarize_runs(detector_records, c.threshold, c.min_repeats, ids)?;
    ensure!(
        summary["duplicate_records_ignored"] == 0,
        "Duplicate detector tasks are not independent evidence"
    );
    ensure!(quality_audit.is_object(), "Quality audit must be an object");
    if !quality_audit.as_object().unwrap().is_empty() {
        ensure!(
            quality_audit["challenge_id"] == c.challenge_id && quality_audit["nonce"] == c.nonce,
            "Quality audit challenge/nonce binding mismatch"
        );
        ensure!(
            quality_audit["source_sha256"] == c.source_hash
                && quality_audit["candidate_sha256"] == revision_hash,
            "Quality audit must bind exact source and candidate hashes"
        );
        ensure!(
            quality_audit["protected_invariants_sha256"] == c.inventory_hash,
            "Quality audit protected-inventory hash mismatch"
        );
    }
    let quality_passed = QUALITY_FIELDS
        .iter()
        .all(|field| quality_audit[field] == true);
    let detector_passed = summary["all_groups_observed_success"] == true;
    let score = u8::from(quality_passed && detector_passed);
    Ok(json!({
        "protocol_version": PROTOCOL, "challenge_id": c.challenge_id,
        "candidate_id": candidate_id, "source_sha256": c.source_hash, "revision_sha256": revision_hash,
        "reward": score, "reward_scope": "offline_reference", "quality_passed": quality_passed,
        "detector_passed": detector_passed, "quality_audit": quality_audit, "detector_summary": summary,
        "submission_validation": validation, "supplied_detector_records": detector_records.len(),
        "quality_verification": "human_attestation; reviewer identity is not authenticated here",
        "detector_records_authenticated": false, "replay_checked": false,
        "global_budget_verified": false, "network_ready": false, "reliability_established": false,
        "interpretation": "This is an offline reference reward for the supplied evidence. A network validator must authenticate provenance, track replay and cumulative budgets, and enforce deadlines before issuing rewards. General fidelity and detector performance on unseen sources remain separate benchmark questions.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shadow `reward` with a synthetic salt; these tests never write a sidecar.
    fn reward(c: &Value, s: &Value, records: &[Value], audit: &Value) -> Result<Value> {
        let ids = ProviderIds::new(
            b"synthetic-provider-id-salt-v1".to_vec(),
            "unused-sidecar".into(),
        )?;
        super::reward(c, s, records, audit, &ids)
    }

    #[test]
    fn detector_summary_lists_task_digests_not_raw_ids() {
        let c = challenge();
        let s = submission(&c);
        let report = reward(&c, &s, &records(&c, &s), &audit(&c, &s)).unwrap();
        let listed = report["detector_summary"]["groups"][0]["task_ids"]
            .as_array()
            .unwrap();
        assert_eq!(listed.len(), 3);
        assert!(listed.iter().all(|id| {
            let id = id.as_str().unwrap();
            crate::provider_ids::is_digest(id) && !id.contains("task-")
        }));
    }

    fn challenge() -> Value {
        let source = "The study involved 40 readers. It may improve comprehension, but the evidence remains preliminary.\r\n";
        json!({"protocol_version": PROTOCOL, "challenge_id": "challenge-1", "nonce": "fresh-validator-nonce",
            "source_text": source, "source_sha256": digest(source), "domain": "research", "register": "scientific",
            "tone": "measured, with explicit uncertainty", "group_id": "source-family-1", "split": "test", "full_document": true,
            "budgets": {"max_candidates": 2, "max_detector_queries": 4},
            "detector": {"name": "pangram", "model": "pangram-4", "version": "4.0", "threshold": 0.1, "min_repeats": 3},
            "protected_invariants": [{"id": "n", "requirement": "Keep the 40 readers."},
                {"id": "qualification", "requirement": "Retain possibility and preliminary evidence."}]})
    }

    fn submission(c: &Value) -> Value {
        let revision = "Forty readers participated. Comprehension may improve, though the evidence is preliminary.\r\n";
        json!({"protocol_version": PROTOCOL, "challenge_id": c["challenge_id"], "nonce": c["nonce"],
            "source_sha256": c["source_sha256"], "candidate_id": "candidate-1", "candidate_index": 1,
            "revised_text": revision, "revision_sha256": digest(revision), "process_version": "baseline-v1",
            "declared_detector_queries": 0})
    }

    fn records(c: &Value, s: &Value) -> Vec<Value> {
        (0..3).map(|i| json!({"challenge_id": c["challenge_id"], "nonce": c["nonce"], "candidate_id": s["candidate_id"],
            "submitted_text": s["revised_text"], "sha256": s["revision_sha256"], "full_document": true,
            "task_id": format!("task-{i}"), "detector": "pangram", "model": "pangram-4",
            "result": {"stage": "STAGE_SUCCESS", "version": "4.0", "fraction_ai": 0.04,
                "fraction_ai_assisted": 0.01, "fraction_human": 0.95}})).collect()
    }

    fn audit(c: &Value, s: &Value) -> Value {
        let binding = validate_submission(c, s).unwrap();
        json!({"challenge_id": c["challenge_id"], "nonce": c["nonce"], "source_sha256": c["source_sha256"],
            "candidate_sha256": s["revision_sha256"], "protected_invariants_sha256": binding["protected_invariants_sha256"],
            "reviewed_by_human": true, "readability": true, "argumentation": true, "detail": true,
            "tone": true, "protected_invariants_preserved": true, "full_document": true})
    }

    #[test]
    fn exact_binding_passes_but_does_not_claim_replay_or_budget_verification() {
        let c = challenge();
        let s = submission(&c);
        let first = validate_submission(&c, &s).unwrap();
        assert_eq!(first["valid_submission"], true);
        assert_eq!(first["replay_checked"], false);
        assert_eq!(first["global_budget_verified"], false);
        assert_eq!(first["network_ready"], false);
        // Stateless validation intentionally has no memory of the first call.
        assert_eq!(validate_submission(&c, &s).unwrap(), first);
    }

    #[test]
    fn rejects_wrong_submission_bindings_and_exact_byte_hashes() {
        let c = challenge();
        for field in [
            "protocol_version",
            "challenge_id",
            "nonce",
            "source_sha256",
            "revision_sha256",
        ] {
            let mut s = submission(&c);
            s[field] = json!("wrong");
            assert!(validate_submission(&c, &s).is_err(), "{field}");
        }
        let mut s = submission(&c);
        s["revised_text"] = json!(s["revised_text"].as_str().unwrap().replace("\r\n", "\n"));
        assert!(
            validate_submission(&c, &s)
                .unwrap_err()
                .to_string()
                .contains("exact revised_text")
        );
        let mut c = c;
        c["source_text"] = json!("Changed source");
        assert!(validate_submission(&c, &submission(&c)).is_err());
    }

    #[test]
    fn candidate_and_declared_query_limits_are_validated_without_trusting_them() {
        let c = challenge();
        for index in [0, 3] {
            let mut s = submission(&c);
            s["candidate_index"] = json!(index);
            assert!(validate_submission(&c, &s).is_err());
        }
        let mut s = submission(&c);
        s["declared_detector_queries"] = json!(5);
        assert!(validate_submission(&c, &s).is_err());
        s["declared_detector_queries"] = json!(0);
        let scored = reward(&c, &s, &[], &audit(&c, &s)).unwrap();
        assert_eq!(scored["reward"], 0);
        assert_eq!(scored["detector_passed"], false);
    }

    #[test]
    fn passing_scores_and_bound_human_audit_produce_only_reference_reward() {
        let c = challenge();
        let s = submission(&c);
        let report = reward(&c, &s, &records(&c, &s), &audit(&c, &s)).unwrap();
        assert_eq!(report["reward"], 1);
        assert_eq!(report["reward_scope"], "offline_reference");
        assert_eq!(report["network_ready"], false);
        assert_eq!(report["reliability_established"], false);
        assert_eq!(report["detector_records_authenticated"], false);
    }

    #[test]
    fn missing_human_review_or_failed_fidelity_blocks_low_detector_scores() {
        let c = challenge();
        let s = submission(&c);
        let records = records(&c, &s);
        assert_eq!(reward(&c, &s, &records, &json!({})).unwrap()["reward"], 0);
        for field in QUALITY_FIELDS {
            let mut a = audit(&c, &s);
            a[field] = json!(false);
            assert_eq!(
                reward(&c, &s, &records, &a).unwrap()["reward"],
                0,
                "{field}"
            );
        }
    }

    #[test]
    fn omission_does_not_earn_reward_even_at_zero_ai() {
        let c = challenge();
        let mut s = submission(&c);
        s["revised_text"] = json!("The study improved comprehension.");
        s["revision_sha256"] = json!(digest(s["revised_text"].as_str().unwrap()));
        let mut a = audit(&c, &s);
        a["detail"] = json!(false);
        a["protected_invariants_preserved"] = json!(false);
        let mut runs = records(&c, &s);
        for r in &mut runs {
            r["result"]["fraction_ai"] = json!(0);
            r["result"]["fraction_ai_assisted"] = json!(0);
            r["result"]["fraction_human"] = json!(1);
        }
        assert_eq!(reward(&c, &s, &runs, &a).unwrap()["reward"], 0);
    }

    #[test]
    fn every_repeat_must_pass_strict_combined_threshold() {
        let c = challenge();
        let s = submission(&c);
        let a = audit(&c, &s);
        let runs = records(&c, &s);
        assert_eq!(reward(&c, &s, &runs[..2], &a).unwrap()["reward"], 0);
        for (ai, assisted) in [(0.08, 0.02), (0.0, 0.25), (0.9, 0.0)] {
            let mut bad = runs.clone();
            bad[2]["result"]["fraction_ai"] = json!(ai);
            bad[2]["result"]["fraction_ai_assisted"] = json!(assisted);
            bad[2]["result"]["fraction_human"] = json!(1.0 - ai - assisted);
            assert_eq!(reward(&c, &s, &bad, &a).unwrap()["reward"], 0);
        }
    }

    #[test]
    fn detector_evidence_must_match_full_revision_and_frozen_selector() {
        let c = challenge();
        let s = submission(&c);
        let a = audit(&c, &s);
        for field in [
            "challenge_id",
            "nonce",
            "candidate_id",
            "submitted_text",
            "sha256",
            "detector",
            "model",
            "full_document",
        ] {
            let mut runs = records(&c, &s);
            runs[0][field] = json!("wrong");
            assert!(reward(&c, &s, &runs, &a).is_err(), "{field}");
        }
        let mut runs = records(&c, &s);
        runs[0]["result"]["version"] = json!("4.1");
        assert!(reward(&c, &s, &runs, &a).is_err());
    }

    #[test]
    fn repeated_task_and_observed_query_overrun_cannot_be_hidden_by_declaration() {
        let c = challenge();
        let s = submission(&c);
        let a = audit(&c, &s);
        let mut runs = records(&c, &s);
        runs[2] = runs[1].clone();
        assert!(
            reward(&c, &s, &runs, &a)
                .unwrap_err()
                .to_string()
                .contains("Duplicate detector")
        );
        let runs = vec![records(&c, &s)[0].clone(); 5];
        assert!(
            reward(&c, &s, &runs, &a)
                .unwrap_err()
                .to_string()
                .contains("exceed the query budget")
        );
    }

    #[test]
    fn audit_is_bound_to_source_revision_challenge_and_inventory() {
        let c = challenge();
        let s = submission(&c);
        let runs = records(&c, &s);
        for field in [
            "challenge_id",
            "nonce",
            "source_sha256",
            "candidate_sha256",
            "protected_invariants_sha256",
        ] {
            let mut a = audit(&c, &s);
            a[field] = json!("wrong");
            assert!(reward(&c, &s, &runs, &a).is_err(), "{field}");
        }
        let a = audit(&c, &s);
        let mut changed = c.clone();
        changed["protected_invariants"][0]["requirement"] = json!("Changed obligation");
        assert!(
            reward(&changed, &s, &runs, &a)
                .unwrap_err()
                .to_string()
                .contains("inventory hash")
        );
    }

    #[test]
    fn challenge_requires_explicit_inventory_scope_and_strict_evidence_budget() {
        let c = challenge();
        let s = submission(&c);
        for path in [
            "domain",
            "register",
            "tone",
            "group_id",
            "split",
            "protected_invariants",
            "full_document",
        ] {
            let mut invalid = c.clone();
            invalid.as_object_mut().unwrap().remove(path);
            assert!(validate_submission(&invalid, &s).is_err(), "{path}");
        }
        for threshold in [0.0, 0.11, 1.0] {
            let mut invalid = c.clone();
            invalid["detector"]["threshold"] = json!(threshold);
            assert!(validate_submission(&invalid, &s).is_err());
        }
        for repeats in [2, 5] {
            let mut invalid = c.clone();
            invalid["detector"]["min_repeats"] = json!(repeats);
            assert!(validate_submission(&invalid, &s).is_err());
        }
    }
}
