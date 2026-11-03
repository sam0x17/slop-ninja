//! A training admission for hosted/frontier model outputs.
//! build REVIEWS.jsonl HOSTED_STYLE_ROOT OUT.json
//! check ADMISSION.json RECORDS.jsonl
//!
//! `build` binds the retained hosted captures by exact record hash without
//! rewriting their receipts. `check` reports, for each hosted record, whether
//! it is admitted by that manifest or by its own collection-time basis. A
//! record whose text does not match its digest is refused.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::Path,
};

pub const SCHEMA: &str = "slop_ninja_a_hosted_output_admission_v1";
pub const BASIS_SCHEMA: &str = "slop_ninja_a_hosted_training_admission_v1";

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn lines(bytes: &[u8]) -> Result<Vec<Value>> {
    bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| Ok(serde_json::from_slice(l)?))
        .collect()
}
fn logical(p: &Path) -> String {
    let s = p.to_string_lossy();
    ["datasets/", "data/"]
        .iter()
        .find_map(|m| s.find(m))
        .map(|i| s[i..].to_owned())
        .unwrap_or_else(|| s.into_owned())
}
fn str_of<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}

/// Requested-versus-reported identity: a reported name is the resolved label;
/// a requested selector alone forms its own, separately reported label basis.
fn identity(requested: &str, reported: Option<&str>) -> Value {
    match reported {
        Some(r) => json!({"model_requested":requested,"model_reported":r,
            "attribution_label":r,"identity_basis":"reported"}),
        None => json!({"model_requested":requested,"model_reported":null,
            "attribution_label":format!("requested:{requested}"),"identity_basis":"requested_selector"}),
    }
}

fn frontier(reviews: &Path) -> Result<Value> {
    let bytes = fs::read(reviews)?;
    let mut records = Vec::new();
    let mut models = BTreeMap::<String, usize>::new();
    for r in lines(&bytes)? {
        ensure!(
            r["schema"] == "slop_ninja_cli_capture_review_v1"
                && r["capture_origin"] == "model_response"
                && r["text_sha256"] == sha(str_of(&r, "text")?.as_bytes()),
            "unexpected or altered capture review"
        );
        let reported = r["generator_metadata"]["reported_models"]
            .as_array()
            .context("reported models")?;
        ensure!(reported.len() <= 1, "ambiguous reported prose model");
        let id = identity(
            str_of(&r, "requested_model")?,
            reported.first().and_then(Value::as_str),
        );
        *models
            .entry(str_of(&id, "attribution_label")?.to_owned())
            .or_default() += 1;
        records.push(
            json!({"record_id":format!("cli-capture:{}",str_of(&r,"request_sha256")?),
            "text_sha256":r["text_sha256"],"capture_sha256":r["capture_sha256"],
            "request_sha256":r["request_sha256"],"source_group":r["source_group"],
            "source_record_id":r["source_record_id"],"backend":r["backend"],
            "operation":r["operation"],"origin":"model_only","identity":id,
            "receipt_commercial_training_admitted":r["commercial_training_admitted"]}),
        );
    }
    Ok(
        json!({"records_path":logical(reviews),"reviews_sha256":sha(&bytes),
        "operation":"source_conditioned_draft","origin":"model_only","attribution_labels":models,
        "records_count":records.len(),"records":records}),
    )
}

fn style(root: &Path) -> Result<Value> {
    let canonical = read(&root.join("CANONICAL_EXPORTS.json"))?;
    ensure!(
        canonical["schema"] == "slop_ninja_hosted_canonical_exports_v1",
        "canonical export index required"
    );
    let mut exports = Vec::new();
    let mut records = Vec::new();
    let mut models = BTreeMap::<String, usize>::new();
    for role in ["astra", "claude"] {
        let dir = root.join(str_of(&canonical[role], "path")?);
        let manifest_bytes = fs::read(dir.join("manifest.json"))?;
        ensure!(
            canonical[role]["manifest_sha256"] == sha(&manifest_bytes),
            "canonical export manifest changed"
        );
        let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
        let bytes = fs::read(dir.join("records.jsonl"))?;
        ensure!(
            manifest["records_sha256"] == sha(&bytes),
            "export records changed"
        );
        exports.push(json!({"role":role,"path":logical(&dir),"manifest_sha256":sha(&manifest_bytes),
            "records_sha256":manifest["records_sha256"],"acquisition_manifest_sha256":manifest["acquisition_manifest_sha256"]}));
        for r in lines(&bytes)? {
            ensure!(
                r["text_sha256"] == sha(str_of(&r, "text")?.as_bytes()),
                "record text changed"
            );
            let g = &r["generation"];
            let id = identity(str_of(g, "model_requested")?, g["model_reported"].as_str());
            *models
                .entry(str_of(&id, "attribution_label")?.to_owned())
                .or_default() += 1;
            records.push(json!({"record_id":r["id"],"text_sha256":r["text_sha256"],
                "response_document_sha256":g["response_document_sha256"],"parent_id":r["parent_id"],
                "source_group":r["source_group"],"provider":g["provider"],"origin":r["origin"],
                "share_target":r["share_target"],"identity":id,
                "receipt_commercial_training_admitted":g["commercial_training_admitted"]}));
        }
    }
    Ok(
        json!({"canonical_index_sha256":sha(&fs::read(root.join("CANONICAL_EXPORTS.json"))?),
        "canonical_exports":exports,"operation":"source_conditioned_style_rewrite","origin":"mixed",
        "superseded_exports":"astra-export-v1 and claude-export-v1 hold the same texts and responses with the earlier identity assembly; train from the canonical exports",
        "attribution_labels":models,"records_count":records.len(),"records":records}),
    )
}

/// SHA-256 of the manifest bytes that records collected before 2026-10-31 cite.
const PREDECESSOR_SHA256: &str = "e402401ac102e681e80bf77f491356f18f9c60c64f7dade778dd30d8f35618ee";

fn policy() -> Value {
    json!({
        "decision":{"by":"user","date":"2026-10-27",
            "statement":"Hosted and frontier model outputs are admitted for commercial A training and evaluation.",
            "goal":"Per-frontier-model attribution that holds across voices, alongside AI-involvement confidence and AI share."},
        "uses":["a_train","a_train_mining","a_dev_iter","a_dev_select","a_cal","a_evaluation"],
        "provider_terms":{
            "note":"Each output remains subject to its provider's terms. This manifest records the 2026-10-27 admission decision for the cohorts, records and uses listed here."},
        "predecessor":{"sha256":PREDECESSOR_SHA256,
            "note":"Earlier bytes of this manifest, dated 2026-10-27. The cohorts, records, conditions and identity policy are unchanged; only provider_terms and this entry differ. Records collected before 2026-10-31 cite this digest, and the earlier bytes are retained privately."},
        "conditions":[
            "Hosted records form their own cohort with explicit, recorded training weights so their influence can be measured and the cohort removed.",
            "Only public passages from admitted pools are sent to hosted models: Train, Train-mining, Dev-iter, Dev-select and CAL passages; never Test passages before the seal, private correspondence, the private campaign pool or B text.",
            "Each record keeps the exact source, prompt, voice or brief, provider, requested selector, reported identity when exposed, CLI version, argv, reasoning effort and the full event stream.",
            "New hosted generation uses medium reasoning effort, recorded per record.",
            "A record is used only in the split its source group holds in the current split assignment; a source group outside that split is excluded with a receipt.",
            "Rate limits and account terms are respected; collections stop cleanly at limits and resume from their durable receipts."],
        "identity_policy":{
            "reported":"When the provider reports the prose model, that name is the attribution label.",
            "requested_selector":"When only the requested selector is known, the label is requested:<selector>; it is reported as its own basis and never presented as a resolved model snapshot.",
            "helper_models":"Models present only in usage accounting are metadata, not the prose author."},
        "historical_receipts":"Captured receipts keep commercial_training_admitted:false as their capture-time state. This manifest admits those records by exact hash without rewriting them.",
        "new_collections":{"basis_field":"training_admission","basis_schema":BASIS_SCHEMA,
            "collectors":["examples/a_hosted_style_rewrites.rs","baseline/detector/src/bin/collect_cli_samples.rs"],
            "reasoning_effort":"medium"}
    })
}

pub fn build(reviews: &Path, style_root: &Path) -> Result<Value> {
    let mut m = json!({"schema":SCHEMA,"date":"2026-10-27","status":"admitted_for_a_training_and_evaluation"});
    for (k, v) in policy().as_object().unwrap() {
        m[k] = v.clone();
    }
    let f = frontier(reviews)?;
    let s = style(style_root)?;
    m["cohorts"] = json!({"frontier-captures-v1":f,"hosted-style-v1":s});
    m["admitted_records"] =
        json!(f["records_count"].as_u64().unwrap() + s["records_count"].as_u64().unwrap());
    m["limitations"] = json!([
        "Mechanical checks only: no record has a meaning-preservation, readability or style-compliance review.",
        "The frontier captures are drafts from 24 v5 Train source families; the style rewrites cover five public-domain works. Both are small and source-familiar.",
        "Codex streams expose no resolved model snapshot; GPT-6 Astra and GPT-5.6 Sol labels are requested selectors.",
        "Hosted aliases do not identify immutable weights; unexposed sampling settings remain unknown.",
        "Hosted captures not bound here by record hash are not admitted by this manifest."
    ]);
    Ok(m)
}

fn is_basis(basis: &Value) -> bool {
    basis["schema"] == BASIS_SCHEMA && basis["manifest_schema"] == SCHEMA
}

/// Admission of one record, refusing a text that does not match its digest.
pub fn admitted_checked(manifest: &Value, record: &Value) -> Option<String> {
    if let (Some(t), Some(d)) = (record["text"].as_str(), record["text_sha256"].as_str()) {
        if sha(t.as_bytes()) != d {
            return Some("text_digest_mismatch".into());
        }
    }
    admitted(manifest, record)
}

/// Admission for one hosted record: by this manifest's record hashes, or by a
/// collection-time basis that names this manifest's schema.
pub fn admitted(manifest: &Value, record: &Value) -> Option<String> {
    let text = record["text_sha256"].as_str()?;
    for (name, cohort) in manifest["cohorts"].as_object()? {
        if cohort["records"]
            .as_array()?
            .iter()
            .any(|r| r["text_sha256"] == text)
        {
            return Some(format!("manifest:{name}"));
        }
    }
    let basis = record
        .get("training_admission")
        .or_else(|| record["generation"].get("training_admission"))?;
    is_basis(basis).then(|| "collection_basis".to_owned())
}

fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    match a.first().map(String::as_str) {
        Some("build") if a.len() == 4 => {
            let out = Path::new(&a[3]);
            ensure!(!out.exists(), "new output required");
            let m = build(Path::new(&a[1]), Path::new(&a[2]))?;
            let mut f = fs::File::create_new(out)?;
            serde_json::to_writer_pretty(&mut f, &m)?;
            f.write_all(b"\n")?;
            f.sync_all()?;
            Ok(())
        }
        Some("check") if a.len() == 3 => {
            let manifest_bytes = fs::read(&a[1])?;
            let m: Value = serde_json::from_slice(&manifest_bytes)?;
            ensure!(m["schema"] == SCHEMA, "admission manifest required");
            let records = lines(&fs::read(&a[2])?)?;
            let mut counts = BTreeMap::<String, usize>::new();
            // Basis digests recorded at collection time against this manifest's bytes.
            let mut digests = BTreeMap::<String, usize>::new();
            let current = sha(&manifest_bytes);
            let predecessor = m["predecessor"]["sha256"].as_str();
            for r in &records {
                let how = admitted_checked(&m, r).unwrap_or_else(|| "not_admitted".into());
                let basis = r
                    .get("training_admission")
                    .or_else(|| r["generation"].get("training_admission"));
                if let Some(d) = basis.and_then(|b| b["manifest_sha256"].as_str()) {
                    *digests
                        .entry(if d == current {
                            "current".into()
                        } else if Some(d) == predecessor {
                            "predecessor".into()
                        } else {
                            d.to_owned()
                        })
                        .or_default() += 1;
                }
                *counts.entry(how).or_default() += 1;
            }
            println!(
                "{}",
                json!({"records":records.len(),"admission":counts,"basis_manifest_sha256":digests})
            );
            Ok(())
        }
        _ => anyhow::bail!(
            "build REVIEWS.jsonl HOSTED_STYLE_ROOT OUT.json | check ADMISSION.json RECORDS.jsonl"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requested_selector_is_its_own_identity_basis() {
        let r = identity("gpt-6-astra", None);
        assert_eq!(r["attribution_label"], "requested:gpt-6-astra");
        assert_eq!(r["identity_basis"], "requested_selector");
        assert!(r["model_reported"].is_null());
        let c = identity("sonnet", Some("claude-sonnet-5-5"));
        assert_eq!(c["attribution_label"], "claude-sonnet-5-5");
        assert_eq!(c["model_requested"], "sonnet");
    }
    #[test]
    fn admission_is_by_exact_hash_or_collection_basis() {
        let m = json!({"schema":SCHEMA,"cohorts":{"c":{"records":[{"text_sha256":"aa"}]}}});
        assert_eq!(
            admitted(&m, &json!({"text_sha256":"aa"})).unwrap(),
            "manifest:c"
        );
        assert!(admitted(&m, &json!({"text_sha256":"bb"})).is_none());
        let basis = json!({"schema":BASIS_SCHEMA,"manifest_schema":SCHEMA});
        let new = json!({"text_sha256":"bb","generation":{"training_admission":basis}});
        assert_eq!(admitted(&m, &new).unwrap(), "collection_basis");
        let wrong = json!({"text_sha256":"bb","training_admission":{"schema":"other"}});
        assert!(admitted(&m, &wrong).is_none());
    }
    #[test]
    fn a_text_that_differs_from_its_digest_is_refused() {
        let m = json!({"schema":SCHEMA,"cohorts":{}});
        let basis = json!({"schema":BASIS_SCHEMA,"manifest_schema":SCHEMA});
        let text = "A frontier record.";
        let ok = json!({"text":text,"text_sha256":sha(text.as_bytes()),"training_admission":basis});
        assert_eq!(admitted_checked(&m, &ok).unwrap(), "collection_basis");
        let forged = json!({"text":"Edited.","text_sha256":sha(text.as_bytes()),"training_admission":basis});
        assert_eq!(admitted_checked(&m, &forged).unwrap(), "text_digest_mismatch");
    }
    #[test]
    fn the_tracked_manifest_carries_the_current_policy() {
        let doc: Value =
            serde_json::from_str(include_str!("../docs/a-hosted-output-admission.json")).unwrap();
        assert_eq!(doc["schema"], SCHEMA);
        for (k, v) in policy().as_object().unwrap() {
            assert_eq!(&doc[k], v, "{k}");
        }
        assert_eq!(doc["predecessor"]["sha256"], PREDECESSOR_SHA256);
    }
}
