//! External B checks have a separate schema and never enter detector training.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub const SCHEMA: &str = "slop_ninja_b_pangram_plan_v1";

fn build(selections: &[(String, Value)]) -> Result<(Value, Vec<u8>, u64)> {
    ensure!(
        !selections.is_empty() && selections.len() <= 8,
        "Use 1..8 reviewed selections"
    );
    let mut texts: BTreeMap<String, (String, Vec<Value>)> = BTreeMap::new();
    let mut files = vec![];
    for (index, (digest, selection)) in selections.iter().enumerate() {
        ensure!(
            selection["schema"] == "slop_ninja_b_edit_menu_selection_v1"
                && selection["research_only"] == true,
            "Expected a recorded B research selection"
        );
        let review = &selection["review"];
        for field in [
            "meaning",
            "target_tone",
            "readability",
            "cleanup",
            "author_fit",
        ] {
            ensure!(
                review[field] == true,
                "B editorial review must pass: {field}"
            );
        }
        ensure!(
            review["plan_sha256"] == selection["plan_sha256"]
                && review["state_id"] == selection["state"]["id"],
            "Review is for another selection"
        );
        for field in ["reviewer", "method", "notes"] {
            ensure!(
                review[field].as_str().is_some_and(|s| !s.trim().is_empty()),
                "Missing review attribution"
            );
        }
        let source = &selection["walk_binding"]["root_source"];
        ensure!(
            source["provenance"]["authorship"] == "synthetic_fixture"
                && source["provenance"]["license"] == "MIT",
            "This research path accepts only MIT synthetic fixtures, not private customer writing"
        );
        let output = selection["state"]["text"]
            .as_str()
            .context("No selected text")?;
        ensure!(
            selection["state"]["id"] == sha256(output),
            "Selected text hash mismatch"
        );
        for (role, text) in [
            (
                "synthetic_source",
                source["text"].as_str().context("No source text")?,
            ),
            ("b_revision", output),
        ] {
            ensure!(
                (50..=1000).contains(&text.split_whitespace().count()),
                "Research text outside 50..1000 words"
            );
            let hash = sha256(text);
            let entry = texts
                .entry(hash.clone())
                .or_insert_with(|| (text.to_owned(), vec![]));
            ensure!(entry.0 == text, "Text hash collision");
            entry.1.push(json!({"record_id":hash,"role":role,"selection_sha256":digest,"detector_training_admitted":false,"commercial_training_admitted":false}));
        }
        files.push(json!({"path":format!("selections/{index:04}.json"),"sha256":digest}));
    }
    let mut items = vec![];
    let mut members = vec![];
    let mut units = 0;
    for (hash, (text, records)) in texts {
        let id = format!("{hash}:0");
        units += text.split_whitespace().count().div_ceil(100) as u64;
        members.push(json!({"request_id":id,"text_sha256":hash,"repeat_index":0,"batch_index":0,"item_index":items.len(),"records":records}));
        items.push(json!({"id":id,"text":text}));
    }
    let payload = serde_json::to_vec_pretty(&json!({"model":"pangram-4","items":items}))?;
    Ok((
        json!({"schema":SCHEMA,"model_selector":"pangram-4","source_kind":"reviewed_b_synthetic_selection","detector_training_admitted":false,"commercial_training_admitted":false,"scope":"Adaptive B research only. Synthetic source and B descendants remain excluded from A training. No reference-author text is submitted.","selection_files":files,"members":members,"request_files":[{"path":"requests/batch-0000.json","sha256":sha256(&payload)}]}),
        payload,
        units,
    ))
}

pub fn prepare(paths: &[PathBuf], out: &Path) -> Result<Value> {
    ensure!(!out.exists(), "Refusing to overwrite a B evaluation plan");
    let mut bytes = vec![];
    let mut selections = vec![];
    for path in paths {
        let raw = fs::read(path)?;
        selections.push((sha256(&raw), serde_json::from_slice(&raw)?));
        bytes.push(raw);
    }
    let (plan, payload, units) = build(&selections)?;
    fs::create_dir_all(out.join("selections"))?;
    fs::create_dir(out.join("requests"))?;
    for (index, raw) in bytes.iter().enumerate() {
        super::write_new(&out.join(format!("selections/{index:04}.json")), raw)?;
    }
    super::write_new(&out.join("requests/batch-0000.json"), &payload)?;
    super::save(&out.join("plan.json"), &plan)?;
    Ok(
        json!({"status":"prepared","plan_dir":out,"unique_texts":plan["members"].as_array().unwrap().len(),"units":units,"minimum_reserve_cents":units*5,"network_calls":0,"detector_training_admitted":false}),
    )
}

pub fn validate(dir: &Path, batch: usize) -> Result<(Value, Vec<u8>, u64)> {
    ensure!(batch == 0, "B research plans contain one bounded batch");
    let plan = super::read(&dir.join("plan.json"))?;
    let files = plan["selection_files"]
        .as_array()
        .context("Missing frozen selections")?;
    ensure!(
        !files.is_empty() && files.len() <= 8,
        "Invalid selection count"
    );
    let mut selections = vec![];
    for (index, file) in files.iter().enumerate() {
        let path = format!("selections/{index:04}.json");
        ensure!(file["path"] == path, "Unexpected selection path");
        let bytes = fs::read(dir.join(path))?;
        let digest = sha256(&bytes);
        ensure!(file["sha256"] == digest, "Frozen selection changed");
        selections.push((digest, serde_json::from_slice(&bytes)?));
    }
    let (expected, payload, units) = build(&selections)?;
    ensure!(
        plan == expected && fs::read(dir.join("requests/batch-0000.json"))? == payload,
        "B plan or payload differs from reviewed texts"
    );
    Ok((plan, payload, units))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_low_detector_score_cannot_admit_an_unreviewed_or_changed_b_text() {
        let text = "word ".repeat(60);
        let review = json!({"meaning":true,"target_tone":true,"readability":true,"cleanup":true,"author_fit":true,"reviewer":"fixture","method":"fixture","notes":"fixture","plan_sha256":"plan","state_id":sha256(&text)});
        let mut selection = json!({"schema":"slop_ninja_b_edit_menu_selection_v1","research_only":true,"review":review,"plan_sha256":"plan","state":{"id":sha256(&text),"text":text},"walk_binding":{"root_source":{"text":text,"provenance":{"authorship":"synthetic_fixture","license":"MIT"}}},"a_score":0.0});
        let build_one = |s: &Value| build(&[("fixture".into(), s.clone())]);
        let (plan, _, _) = build_one(&selection).unwrap();
        assert_eq!(plan["detector_training_admitted"], false);
        selection["review"]["meaning"] = Value::Null;
        assert!(build_one(&selection).is_err());
        selection["review"]["meaning"] = json!(true);
        selection["state"]["text"] = json!("different");
        assert!(build_one(&selection).is_err());
    }
}
