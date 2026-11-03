//! User-authorized private document diagnostics, never corpus/training records.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

pub const SCHEMA: &str = "slop_ninja_b_private_context_pangram_plan_v1";

fn build(raw: &[u8], authorization: &Value) -> Result<(Value, Vec<u8>, u64)> {
    let document: Value = serde_json::from_slice(raw)?;
    ensure!(
        document["schema"] == "slop_ninja_b_document_plan_v1",
        "Expected a recorded document plan"
    );
    let source = super::field(&document, "source")?;
    ensure!(
        document["source_sha256"] == sha256(source),
        "Document source hash mismatch"
    );
    ensure!(
        authorization["schema"] == "slop_ninja_b_private_context_authorization_v1"
            && authorization["document_plan_sha256"] == sha256(raw)
            && authorization["external_provider"] == "pangram"
            && authorization["note"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()),
        "Missing/badly bound authorization record"
    );
    let views = authorization["views"]
        .as_array()
        .context("Missing authorized views")?;
    ensure!(
        !views.is_empty() && views.len() <= 8,
        "Use 1..8 exact document views"
    );
    let windows = document["windows"]
        .as_array()
        .context("Missing document windows")?;
    let mut seen = BTreeSet::new();
    let mut texts: BTreeMap<String, (String, Vec<Value>)> = BTreeMap::new();
    for view in views {
        let label = view.as_str().context("Invalid view label")?;
        ensure!(
            (label == "whole" || label.starts_with("probe:")) && seen.insert(label),
            "Use unique whole/probe views"
        );
        let matching: Vec<_> = windows.iter().filter(|w| w["view"] == label).collect();
        ensure!(
            matching.len() == 1,
            "Authorized view is missing or ambiguous"
        );
        let w = matching[0];
        let start = usize::try_from(w["start"].as_u64().context("Invalid start")?)?;
        let end = usize::try_from(w["end"].as_u64().context("Invalid end")?)?;
        ensure!(
            start < end && (label != "whole" || (start == 0 && end == source.len())),
            "Invalid document range"
        );
        let text = source
            .get(start..end)
            .context("Invalid UTF-8 source range")?;
        let h = sha256(text);
        ensure!(w["text_sha256"] == h, "Document view hash mismatch");
        ensure!(
            text.split_whitespace().count() >= 50,
            "Context diagnostic requires at least 50 words"
        );
        let entry = texts
            .entry(h.clone())
            .or_insert_with(|| (text.to_owned(), vec![]));
        ensure!(entry.0 == text, "Text hash collision");
        entry.1.push(json!({"record_id":h,"role":"private_document_context_diagnostic","view":label,"source_utf8_range":[start,end],"document_plan_sha256":sha256(raw),"detector_training_admitted":false,"commercial_training_admitted":false}));
    }
    let mut items = vec![];
    let mut members = vec![];
    let mut units = 0;
    for (h, (text, records)) in texts {
        let id = format!("{h}:0");
        units += text.split_whitespace().count().div_ceil(100) as u64;
        members.push(json!({"request_id":id,"text_sha256":h,"repeat_index":0,"batch_index":0,"item_index":items.len(),"records":records}));
        items.push(json!({"id":id,"text":text}));
    }
    ensure!(units <= 1000, "Exceeds documented Pangram bulk unit limit");
    let payload = serde_json::to_vec_pretty(&json!({"model":"pangram-4","items":items}))?;
    Ok((
        json!({"schema":SCHEMA,"model_selector":"pangram-4","source_kind":"user_authorized_private_document_context","document_plan_sha256":sha256(raw),"authorization":authorization,"detector_training_admitted":false,"commercial_training_admitted":false,"public_dashboard_requested":false,"scope":"Private context diagnostics only. Not a transformation selection or evidence of fidelity. No reference-author text is submitted. Keep source, outputs and identifying manifests out of Git.","members":members,"request_files":[{"path":"requests/batch-0000.json","sha256":sha256(&payload)}]}),
        payload,
        units,
    ))
}

pub fn prepare(document_plan: &Path, views: &[String], note: &str, out: &Path) -> Result<Value> {
    ensure!(
        !out.exists(),
        "Refusing to overwrite a private context plan"
    );
    let raw = fs::read(document_plan)?;
    let authorization = json!({"schema":"slop_ninja_b_private_context_authorization_v1","document_plan_sha256":sha256(&raw),"views":views,"external_provider":"pangram","note":note});
    let (plan, payload, units) = build(&raw, &authorization)?;
    fs::create_dir_all(out.join("requests"))?;
    super::write_new(&out.join("document-plan.json"), &raw)?;
    super::save(&out.join("authorization.json"), &authorization)?;
    super::save(&out.join("plan.json"), &plan)?;
    super::write_new(&out.join("requests/batch-0000.json"), &payload)?;
    Ok(
        json!({"status":"prepared","schema":SCHEMA,"unique_texts":plan["members"].as_array().unwrap().len(),"units":units,"minimum_reserve_cents":units*5,"network_calls":0,"detector_training_admitted":false}),
    )
}

pub fn validate(dir: &Path, batch: usize) -> Result<(Value, Vec<u8>, u64)> {
    ensure!(
        batch == 0,
        "Private context plans contain one bounded batch"
    );
    let raw = fs::read(dir.join("document-plan.json"))?;
    let authorization = super::read(&dir.join("authorization.json"))?;
    let (expected, payload, units) = build(&raw, &authorization)?;
    ensure!(
        super::read(&dir.join("plan.json"))? == expected
            && fs::read(dir.join("requests/batch-0000.json"))? == payload,
        "Private context plan/payload differs from authorized views"
    );
    Ok((expected, payload, units))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_context_cannot_change_ranges_or_imply_training_admission() {
        let source = "An invented sentence for this private-route test. ".repeat(30);
        let mut document = json!({"schema":"slop_ninja_b_document_plan_v1","source":source,"source_sha256":sha256(&source),"windows":[{"view":"whole","start":0,"end":source.len(),"text_sha256":sha256(&source)}]});
        let raw = serde_json::to_vec(&document).unwrap();
        let mut auth = json!({"schema":"slop_ninja_b_private_context_authorization_v1","document_plan_sha256":sha256(&raw),"views":["whole"],"external_provider":"pangram","note":"User authorized this fixture"});
        let (plan, _, _) = build(&raw, &auth).unwrap();
        assert_eq!(plan["commercial_training_admitted"], false);
        assert_eq!(plan["detector_training_admitted"], false);
        document["windows"][0]["end"] = json!(source.len() - 1);
        let changed = serde_json::to_vec(&document).unwrap();
        assert!(build(&changed, &auth).is_err());
        auth["document_plan_sha256"] = json!(sha256(&changed));
        assert!(build(&changed, &auth).is_err());
        auth["document_plan_sha256"] = json!(sha256(&raw));
        auth["note"] = json!("");
        assert!(build(&raw, &auth).is_err());
    }
}
