//! Generator labels and ancestry, kept separate from detector annotations.
use crate::dataset::{OriginRecord, sha256};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Call after validating the complete corpus, including source-group ancestry.
pub fn describe(record: &OriginRecord, index: &BTreeMap<&str, &OriginRecord>) -> Result<Value> {
    let mut current = record;
    let mut ancestors = Vec::new();
    let mut steps = Vec::new();
    loop {
        ensure!(ancestors.len() <= index.len(), "Cyclic generator ancestry");
        ancestors.push(current.id.clone());
        if let Some(g) = &current.generation {
            // Prompt/style is deliberately absent from this grouping key.
            let label = sha256(serde_json::to_vec(&(
                "slop-ninja-recorded-generator-identity-v1",
                &g.model_id,
                &g.model_revision,
                &g.response_model,
            ))?);
            steps.push(json!({
                "record_id":current.id,"parent_id":current.parent_id,"operation":g.operation,
                "generator_label":label,"requested_model":g.model_id,
                "reported_model":g.response_model,"declared_revision":g.model_revision,
                "quantization":g.quantization,"runtime":g.runtime,"created_at":g.created_at,
                "temperature":g.temperature,"seed":g.seed,"max_tokens":g.max_tokens,
                "finish_reason":g.finish_reason,"prompt_sha256":sha256(&g.prompt),
                "prompt_profile":g.prompt_profile,"request_sha256":g.request_sha256,
                "response_sha256":g.response_sha256,"license":g.model_license,
                "license_sha256":g.model_license_sha256
            }));
        }
        match current.parent_id.as_deref() {
            Some(parent) => current = index.get(parent).context("Missing attribution ancestor")?,
            None => break,
        }
    }
    ancestors.reverse();
    steps.reverse();
    let identities: BTreeSet<_> = steps
        .iter()
        .map(|s| s["generator_label"].clone().as_str().unwrap().to_owned())
        .collect();
    Ok(json!({
        "schema":"slop_ninja_generator_attribution_v1","origin":record.origin,
        "source_group":record.source_group,"source_root_id":current.id,
        "ancestry_record_ids":ancestors,
        "original_draft_generator":steps.iter().find(|s| s["operation"] == "draft"),
        "final_model_stage":steps.last(),"distinct_generator_labels":identities,
        "steps":steps,
        "identity_policy":"Labels group recorded requested model, declared revision and reported model. Runtime and quantization remain separate covariates. Aliases do not prove immutable hosted weights. Prompted personas are not new generator identities."
    }))
}
