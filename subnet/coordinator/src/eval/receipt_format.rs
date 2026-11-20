//! Explicit receipt-format binding in the signed manifest's limits digest.
//! A cohort selects this format explicitly; absent fields retain v1.

use super::engine::{EvalError, R};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use slop_ninja_protocol::receipt_chunks::{CHUNK_BYTES, CHUNK_ENTRIES, COMMITMENT_BYTES};
use slop_ninja_protocol::{Digest, domain_hash};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReceiptIndexFormat {
    #[serde(rename = "chunked-v2")]
    ChunkedV2,
}

pub const LIMITS_SCHEMA: &str = "slop_ninja_eval_limits_v3";
/// A bounded, content-free copy of the limits record. Its digest is already
/// authenticated by the signed manifest; the object is not a new authority.
pub const OBJECT: &str = "receipt-format.json";
/// The Worker SQL bridge accepts only integers exactly representable through
/// JavaScript (`sql::bind_u64`), tighter than SQLite's signed 64-bit range.
/// This storage bound is versioned with the format declaration.
pub const MAX_RECEIPTS: u64 = crate::wire::MAX_SAFE_INTEGER;

/// Probe builds have no cohort manifest. Their signed commitment instead binds
/// the exact frozen plan bytes. Authenticate that binding before accepting the
/// format declaration; a reserialized or otherwise changed plan is refused.
pub fn verify_probe(plan_bytes: &[u8], authenticated_plan: Digest, record: &Value) -> R<ReceiptIndexFormat> {
    if slop_ninja_protocol::object_sha256(plan_bytes) != authenticated_plan {
        return Err(EvalError::State("receipt probe plan binding"));
    }
    let plan: super::plan::CohortPlan = serde_json::from_slice(plan_bytes)
        .map_err(|_| EvalError::State("receipt probe plan"))?;
    if !plan.is_probe_build() || plan.receipt_index_format != Some(ReceiptIndexFormat::ChunkedV2) {
        return Err(EvalError::State("receipt probe format"));
    }
    let (expected, domain) = super::v3::limits_record(&plan)?;
    if &expected != record {
        return Err(EvalError::State("receipt probe limits"));
    }
    verify(record, domain_hash(domain, expected.to_string().as_bytes()))
}

pub fn declaration(format: ReceiptIndexFormat) -> Value {
    match format {
        ReceiptIndexFormat::ChunkedV2 => json!({
            "format": "chunked-v2", "commitment_version": 2, "chunk_version": 1,
            "commitment_record_bytes": COMMITMENT_BYTES,
            "chunk_record_bytes": CHUNK_BYTES, "chunk_entries": CHUNK_ENTRIES,
            "receipt_count_max": MAX_RECEIPTS,
        }),
    }
}

/// Checks the exact supported declaration against an already authenticated
/// manifest's limits digest. No legacy fallback occurs on any failure. A
/// caller handling a legacy cohort keeps its existing v1 path separately.
pub fn verify(record: &Value, authenticated_limits: Digest) -> R<ReceiptIndexFormat> {
    let object = record
        .as_object()
        .ok_or(EvalError::State("receipt format record"))?;
    let fields = [
        "schema",
        "max_attempts",
        "chunk_docs",
        "step_wall_ms",
        "doc_timeout_ms",
        "artifact_max_bytes",
        "load_wait_ms",
        "artifact_failures_before_model_error",
        "crash_rule",
        "artifact_rule",
        "artifact_spec",
        "receipt_index",
    ];
    if object.len() != fields.len()
        || fields.iter().any(|key| !object.contains_key(*key))
        || record["schema"] != LIMITS_SCHEMA
        || record["receipt_index"] != declaration(ReceiptIndexFormat::ChunkedV2)
    {
        return Err(EvalError::State("receipt format declaration"));
    }
    for name in [
        "max_attempts",
        "chunk_docs",
        "step_wall_ms",
        "doc_timeout_ms",
        "artifact_max_bytes",
        "load_wait_ms",
        "artifact_failures_before_model_error",
    ] {
        if !record[name].is_u64() {
            return Err(EvalError::State("receipt format limits"));
        }
    }
    let spec = record["artifact_spec"]
        .as_object()
        .ok_or(EvalError::State("receipt format artifact spec"))?;
    if spec.len() != 2
        || !spec.get("id").is_some_and(Value::is_string)
        || !spec.get("sha256").and_then(Value::as_str).is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        || !record["crash_rule"].is_string()
        || !record["artifact_rule"].is_string()
    {
        return Err(EvalError::State("receipt format artifact spec"));
    }
    if domain_hash(super::v3::LIMITS_DOMAIN_V3, record.to_string().as_bytes())
        != authenticated_limits
    {
        return Err(EvalError::State("receipt format manifest binding"));
    }
    Ok(ReceiptIndexFormat::ChunkedV2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{plan, v3};

    fn candidate() -> plan::CohortPlan {
        let mut p = crate::eval::tests::plan_v3(
            1,
            "testnet-receipt-format",
            20,
            plan::tests::artifact(plan::tests::identity(0, 0xb0), 0x22),
        );
        let score = &mut p.fanout.as_mut().unwrap().score;
        score.artifact_max_mib = None;
        score.artifact_spec = Some("slop_ninja_a_artifact_spec_v3".into());
        score.artifact_spec_sha256 =
            Some(slop_ninja_artifact_check::spec::Version::V3.spec_sha256());
        p
    }

    #[test]
    fn receipt_count_limit_matches_the_worker_storage_bridge() {
        assert!(crate::sql::bind_u64(MAX_RECEIPTS).is_ok());
        assert!(crate::sql::bind_u64(MAX_RECEIPTS + 1).is_err());
    }

    #[test]
    fn explicit_format_changes_limits_binding_but_absence_keeps_legacy_bytes() {
        let mut p = candidate();
        let old_plan = serde_json::to_vec(&p).unwrap();
        assert!(
            serde_json::to_value(&p)
                .unwrap()
                .get("receipt_index_format")
                .is_none()
        );
        let legacy = v3::limits_record(&p).unwrap();
        p.receipt_index_format = Some(ReceiptIndexFormat::ChunkedV2);
        let (record, domain) = v3::limits_record(&p).unwrap();
        assert_eq!(domain, v3::LIMITS_DOMAIN_V3);
        assert_eq!(v3::published_limits(&p).unwrap(), Some(record.clone()));
        let digest = domain_hash(domain, record.to_string().as_bytes());
        assert_eq!(
            verify(&record, digest).unwrap(),
            ReceiptIndexFormat::ChunkedV2
        );
        assert!(
            verify(
                &record,
                domain_hash(legacy.1, legacy.0.to_string().as_bytes())
            )
            .is_err()
        );
        assert_eq!(p.receipt_limit(), MAX_RECEIPTS);
        // Capacity selection does not waive the frozen artifact contract.
        let mut missing_spec = p.clone();
        missing_spec.fanout.as_mut().unwrap().score.artifact_spec_sha256 = None;
        assert_eq!(
            missing_spec.validate(plan::Ceilings {
                roster_cap: None,
                accept_reward_v0: true
            })
            .unwrap_err()
            .0,
            "receipt_index_format (frozen artifact spec)"
        );
        p.receipt_index_format = None;
        assert_eq!(p.receipt_limit(), plan::receipt_index_bound());
        assert_eq!(serde_json::to_vec(&p).unwrap(), old_plan);
        assert_eq!(v3::limits_record(&p).unwrap(), legacy);
        let restored: plan::CohortPlan = serde_json::from_slice(&old_plan).unwrap();
        assert_eq!(restored.receipt_index_format, None);
        assert_eq!(v3::limits_record(&restored).unwrap(), legacy);
        let mut unknown = serde_json::to_value(&p).unwrap();
        unknown["receipt_index_format"] = "chunked-v9".into();
        assert!(serde_json::from_value::<plan::CohortPlan>(unknown).is_err());
    }

    #[test]
    fn probe_format_binds_exact_frozen_plan_and_never_accepts_normal_manifest_input() {
        let mut p = candidate();
        p.schema = plan::PROBE_PLAN_SCHEMA.into();
        p.receipt_index_format = Some(ReceiptIndexFormat::ChunkedV2);
        let bytes = serde_json::to_vec_pretty(&p).unwrap();
        let digest = slop_ninja_protocol::object_sha256(&bytes);
        let (record, _) = v3::limits_record(&p).unwrap();
        assert_eq!(verify_probe(&bytes, digest, &record).unwrap(), ReceiptIndexFormat::ChunkedV2);
        assert!(verify_probe(&serde_json::to_vec(&p).unwrap(), digest, &record).is_err());
        let mut bad = record.clone();
        bad["receipt_index"]["receipt_count_max"] = json!(1);
        assert!(verify_probe(&bytes, digest, &bad).is_err());
        p.schema = plan::PLAN_SCHEMA_V3.into();
        let normal = serde_json::to_vec(&p).unwrap();
        assert!(verify_probe(&normal, slop_ninja_protocol::object_sha256(&normal), &record).is_err());
        p.schema = plan::PROBE_PLAN_SCHEMA.into();
        p.receipt_index_format = None;
        let old = serde_json::to_vec(&p).unwrap();
        assert!(verify_probe(&old, slop_ninja_protocol::object_sha256(&old), &record).is_err());
    }

    #[test]
    fn unsupported_or_tampered_declarations_never_fall_back() {
        let mut p = candidate();
        p.receipt_index_format = Some(ReceiptIndexFormat::ChunkedV2);
        let (valid, domain) = v3::limits_record(&p).unwrap();
        let digest = domain_hash(domain, valid.to_string().as_bytes());
        for key in [
            "format",
            "commitment_version",
            "chunk_version",
            "commitment_record_bytes",
            "chunk_record_bytes",
            "chunk_entries",
            "receipt_count_max",
        ] {
            let mut bad = valid.clone();
            bad["receipt_index"][key] = Value::Null;
            // Even a matching signed digest cannot authorize another format.
            assert!(verify(&bad, domain_hash(domain, bad.to_string().as_bytes())).is_err());
        }
        for change in [json!(null), json!({}), json!("wrong")] {
            let mut bad = valid.clone();
            bad["artifact_spec"] = change;
            assert!(verify(&bad, domain_hash(domain, bad.to_string().as_bytes())).is_err());
        }
        let mut bad = valid.clone();
        bad["max_attempts"] = json!(9);
        assert!(verify(&bad, digest).is_err());
        bad["schema"] = "slop_ninja_eval_limits_v2".into();
        assert!(verify(&bad, domain_hash(domain, bad.to_string().as_bytes())).is_err());
        p.fanout.as_mut().unwrap().score.artifact_spec_sha256 = None;
        assert!(v3::limits_record(&p).is_err());
        p.schema = plan::PLAN_SCHEMA.into();
        assert!(v3::limits_record(&p).is_err());
    }
}
