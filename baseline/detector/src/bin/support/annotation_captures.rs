//! Pangram evaluation adapter for complete, hash-bound hosted capture reviews.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{self, Evidence, Origin, Split, sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

pub const PLAN_SCHEMA: &str = "slop_ninja_pangram_cli_annotation_plan_v1";

pub struct Capture {
    pub text: String,
    pub reference: Value,
}

fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("Missing capture {key}"))
}

/// Captures from runs before the hosted admission are unadmitted; later runs carry
/// their collection-time A training basis. Either may be annotated for evaluation.
fn admission_consistent(r: &Value) -> bool {
    match &r["commercial_training_admitted"] {
        Value::Bool(false) => r.get("training_admission").is_none(),
        Value::Bool(true) => {
            r["training_admission"]["schema"] == "slop_ninja_a_hosted_training_admission_v1"
        }
        _ => false,
    }
}

pub fn load(directory: &Path, expected_audit_sha256: &str) -> Result<Vec<Capture>> {
    let audit_bytes = fs::read(directory.join("summary.json"))?;
    ensure!(
        sha256(&audit_bytes) == expected_audit_sha256,
        "Capture audit hash differs"
    );
    let audit: Value = serde_json::from_slice(&audit_bytes)?;
    ensure!(
        audit["schema"] == "slop_ninja_cli_capture_audit_v1"
            && audit["matrix_complete"] == true
            && audit["allow_partial"] == false
            && audit["missing"] == json!([])
            && audit["unfinished_attempts"] == json!([]),
        "Require a complete capture audit"
    );
    let source_bytes = fs::read(directory.join("source-records.jsonl"))?;
    let review_bytes = fs::read(directory.join("reviews.jsonl"))?;
    ensure!(
        audit["sources_sha256"] == sha256(&source_bytes)
            && audit["reviews_sha256"] == sha256(&review_bytes),
        "Capture input binding differs"
    );
    let sources = dataset::read_records(&directory.join("source-records.jsonl"))?;
    ensure!(
        sources.iter().all(|r| r.split == Some(Split::Train)),
        "Captures must remain Train"
    );
    let source_index: BTreeMap<_, _> = sources.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut drafts = BTreeMap::new();
    for r in &sources {
        if r.generation
            .as_ref()
            .is_some_and(|g| g.operation == "draft")
        {
            ensure!(
                drafts.insert(r.source_group.as_str(), r).is_none(),
                "Duplicate source draft"
            );
        }
    }
    let expected_models = audit["by_model"]
        .as_object()
        .context("Missing audit model counts")?;
    ensure!(
        !drafts.is_empty() && !expected_models.is_empty(),
        "Empty capture matrix"
    );
    let mut seen = BTreeSet::new();
    let mut pairs = BTreeSet::new();
    let mut captures = Vec::new();
    for line in review_bytes
        .split(|b| *b == b'\n')
        .filter(|b| !b.is_empty())
    {
        let r: Value = serde_json::from_slice(line)?;
        ensure!(
            r["schema"] == "slop_ninja_cli_capture_review_v1"
                && r["split"] == "train"
                && r["capture_origin"] == "model_response"
                && r["operation"] == "source_conditioned_draft"
                && admission_consistent(&r)
                && r["provider_output_rights_reviewed"] == false,
            "Unsupported capture admission state"
        );
        let group = field(&r, "source_group")?;
        let model = field(&r, "requested_model")?;
        let backend = field(&r, "backend")?;
        ensure!(
            matches!(backend, "codex" | "claude") && expected_models.contains_key(model),
            "Unknown capture generator"
        );
        ensure!(
            pairs.insert((group.to_owned(), model.to_owned())),
            "Duplicate source/model capture"
        );
        let source = source_index
            .get(field(&r, "source_record_id")?)
            .context("Missing source root")?;
        let draft = drafts.get(group).context("Missing source draft profile")?;
        ensure!(
            source.origin == Origin::HumanOnly
                && source.parent_id.is_none()
                && source.evidence != Evidence::SyntheticFixture
                && source.rights.external_evaluation
                && source.source_group == group
                && draft.parent_id.as_deref() == Some(source.id.as_str())
                && r["source_text_sha256"] == source.text_sha256
                && r["source"] == serde_json::to_value(&source.source)?
                && r["source_rights"] == serde_json::to_value(&source.rights)?
                && r["prompt_profile"]
                    == serde_json::to_value(&draft.generation.as_ref().unwrap().prompt_profile)?,
            "Source provenance, external evaluation rights or profile differs"
        );
        let text = field(&r, "text")?;
        ensure!(
            r["text_sha256"] == sha256(text) && text.split_whitespace().count() >= 50,
            "Changed or too-short capture text"
        );
        let reported = r["generator_metadata"]["reported_models"]
            .as_array()
            .context("Missing reported model evidence")?;
        ensure!(
            reported
                .iter()
                .all(|m| m.as_str().is_some_and(|s| !s.is_empty())),
            "Invalid reported model"
        );
        let identity = sha256(serde_json::to_vec(&(
            "slop-ninja-cli-generator-identity-v1",
            backend,
            model,
            reported,
        ))?);
        ensure!(
            r["generator_label"] == identity,
            "Generator identity differs"
        );
        for key in [
            "request_sha256",
            "stdout_sha256",
            "stderr_sha256",
            "capture_sha256",
            "run_manifest_sha256",
            "invocation_sha256",
        ] {
            let hash = field(&r, key)?;
            ensure!(
                hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "Invalid capture binding"
            );
        }
        let id = format!("cli-capture:{}", field(&r, "request_sha256")?);
        ensure!(seen.insert(id.clone()), "Duplicate capture request");
        let mut reference = json!({
        "record_id":id,"source_group":group,"split":"train","origin":"model_only",
        "evidence":"recorded_model_generation","parent_id":source.id,
        "source_kind":"hosted_cli_capture","commercial_training_admitted":r["commercial_training_admitted"],
        "provider_output_rights_reviewed":false,"capture_review_sha256":sha256(line),
        "content_checks_passed":r["content_checks_passed"],"content_issues":r["content_issues"],
        "fidelity_reviewed":r["fidelity_reviewed"],"source_rights":source.rights,
        "generator_attribution":{
            "schema":"slop_ninja_hosted_generator_attribution_v1","generator_label":identity,
            "backend":backend,"requested_model":model,"metadata":r["generator_metadata"],
            "cli_version":r["cli_version"],"operation":r["operation"],"prompt_profile":r["prompt_profile"],
            "started_at":r["started_at"],"closed_at":r["closed_at"],
            "request_sha256":r["request_sha256"],"stdout_sha256":r["stdout_sha256"],"stderr_sha256":r["stderr_sha256"],
            "capture_sha256":r["capture_sha256"],"run_manifest_sha256":r["run_manifest_sha256"],
            "invocation_sha256":r["invocation_sha256"],"temperature":null,"seed":null,"max_output_tokens":null,
            "model_license":null,"model_revision":null,
            "settings_policy":"Unexposed CLI sampling settings and immutable hosted weights remain unknown. Source rights are separate from provider-output reuse rights."
        }});
        if let Some(basis) = r.get("training_admission") {
            reference["training_admission"] = basis.clone();
        }
        captures.push(Capture {
            text: text.to_owned(),
            reference,
        });
    }
    ensure!(
        captures.len() == drafts.len() * expected_models.len()
            && audit["captured"] == captures.len()
            && audit["expected_records"] == captures.len()
            && audit["source_families"] == drafts.len(),
        "Incomplete capture matrix"
    );
    for (model, counts) in expected_models {
        ensure!(
            counts["captured"] == drafts.len(),
            "Audit model count differs"
        );
        for group in drafts.keys() {
            ensure!(
                pairs.contains(&(group.to_string(), model.clone())),
                "Missing source/model pair"
            );
        }
    }
    ensure!(
        audit_bytes == fs::read(directory.join("summary.json"))?
            && source_bytes == fs::read(directory.join("source-records.jsonl"))?
            && review_bytes == fs::read(directory.join("reviews.jsonl"))?,
        "Capture inputs changed during validation"
    );
    Ok(captures)
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn capture_admission_state_must_match_its_basis() {
        assert!(admission_consistent(
            &json!({"commercial_training_admitted":false})
        ));
        let basis = json!({"schema":"slop_ninja_a_hosted_training_admission_v1"});
        assert!(admission_consistent(
            &json!({"commercial_training_admitted":true,"training_admission":basis})
        ));
        assert!(!admission_consistent(
            &json!({"commercial_training_admitted":true})
        ));
        assert!(!admission_consistent(
            &json!({"commercial_training_admitted":false,"training_admission":basis})
        ));
        assert!(!admission_consistent(&json!({})));
    }
}
