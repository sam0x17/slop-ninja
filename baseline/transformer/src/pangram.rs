//! Offline evidence joins. Assistance is flagged content; windows are diagnostics.
//! Reports list salted provider bulk-id digests; the raw ids stay in the private
//! collections and the sidecar map (see `provider_ids`).
use super::{hash, json_lines, menu, provider_ids::ProviderIds, save, text};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::PathBuf};

fn probability(value: &Value, key: &str) -> Result<f64> {
    let p = value[key].as_f64().context(format!("Missing {key}"))?;
    ensure!(p.is_finite() && (0.0..=1.0).contains(&p), "Invalid {key}");
    Ok(p)
}

/// One paid request: the digested bulk id and the internal item id. A raw bulk
/// id and its digest are the same request.
fn request_key(row: &Value, ids: &ProviderIds) -> Result<(String, String)> {
    Ok((
        ids.shareable(text(row, "bulk_id")?),
        text(&row["observation"], "id")?.to_owned(),
    ))
}

fn observation(row: &Value, source: &str, ids: &ProviderIds) -> Result<Value> {
    ensure!(
        row["schema"] == "slop_ninja_b_pangram_annotation_v1"
            && row["model_selector"] == "pangram-4"
            && row["submitted_text_sha256"] == hash(source)
            && row["member"]["text_sha256"] == row["submitted_text_sha256"],
        "Wrong annotation type or input binding"
    );
    let item = &row["observation"];
    let result = &item["result"];
    ensure!(
        item["error"].is_null()
            && result["stage"] == "STAGE_SUCCESS"
            && item["id"] == row["member"]["request_id"],
        "Failed or mismatched external observation"
    );
    let returned = text(result, "text")?;
    ensure!(
        row["returned_text_sha256"] == hash(returned),
        "Bad echo hash"
    );
    let echo_ok = match text(row, "echo_match")? {
        "exact" => source == returned,
        "whitespace_only" => source.split_whitespace().eq(returned.split_whitespace()),
        _ => false,
    };
    ensure!(echo_ok, "External result changed source content");
    let ai = probability(result, "fraction_ai")?;
    let assisted = probability(result, "fraction_ai_assisted")?;
    let human = probability(result, "fraction_human")?;
    ensure!(
        (ai + assisted + human - 1.0).abs() < 1e-5,
        "Invalid class sum"
    );
    // Use the complement to avoid float32 sums just above one. Keep raw fractions.
    let flagged = 1.0 - human;
    let raw_windows = result["windows"].as_array().context("No windows")?;
    ensure!(!raw_windows.is_empty(), "Empty successful window set");
    let mut windows = vec![];
    let mut weighted_sum = 0.0;
    let mut words = 0_u64;
    for window in raw_windows {
        let score = probability(window, "ai_assistance_score")?;
        let count = window["word_count"]
            .as_u64()
            .context("Invalid window word count")?;
        ensure!(count > 0, "Empty window");
        words = words.checked_add(count).context("Window count overflow")?;
        weighted_sum += score * count as f64;
        let mut saved = window.as_object().context("Invalid window")?.clone();
        // Raw text stays in the bound collection, not the shareable aggregate.
        saved.remove("text");
        windows.push(Value::Object(saved));
    }
    Ok(json!({
        "text_sha256":row["submitted_text_sha256"],
        "returned_text_sha256":row["returned_text_sha256"],
        "bulk_id":row["bulk_id"].as_str().map(|bulk| ids.shareable(bulk)),"request_id":item["id"],
        "collected_at":row["collected_at"],"echo_match":row["echo_match"],
        "model_selector":row["model_selector"],"returned_version":result["version"],
        "fraction_ai":ai,"fraction_ai_assisted":assisted,"fraction_human":human,
        "flagged_fraction":flagged,"windows":windows,
        "word_weighted_window_score":weighted_sum / words as f64
    }))
}

pub fn report(
    plan_path: PathBuf,
    scores_path: PathBuf,
    paths: Vec<PathBuf>,
    out: PathBuf,
) -> Result<()> {
    ensure!(
        !out.exists() && !paths.is_empty(),
        "Use a new output and collected annotations"
    );
    let ids = ProviderIds::load()?;
    let plan = menu::plan_at(&plan_path)?;
    let states = plan["states"].as_array().unwrap();
    let expected = states
        .iter()
        .map(|s| text(s, "id").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    let scores = menu::scores_by_id(
        &scores_path,
        text(&plan["input"], "detector_artifact_id")?,
        &expected,
    )?;
    let root = text(&plan["walk_binding"]["root_source"], "text")?;
    let root_id = hash(root);
    let mut files = vec![];
    let mut seen_files = BTreeSet::new();
    let mut seen_requests = BTreeSet::new();
    let mut observations = vec![];
    for path in paths {
        let digest = hash(fs::read(&path)?);
        ensure!(
            seen_files.insert(digest.clone()),
            "Duplicate collection file"
        );
        files.push(json!({"sha256":digest}));
        for row in json_lines(&path)? {
            // A second GET of one job is not another paid inference observation.
            let key = request_key(&row, &ids)?;
            ensure!(
                seen_requests.insert(key),
                "Repeated collection of the same request"
            );
            let id = text(&row, "submitted_text_sha256")?;
            let state = states.iter().find(|s| s["id"] == id);
            let source = if let Some(state) = state {
                text(state, "text")?
            } else {
                ensure!(
                    id == root_id,
                    "Observation is outside this menu and its root"
                );
                root
            };
            let mut measured = observation(&row, source, &ids)?;
            measured["choices"] = state.map_or(Value::Null, |s| s["choices"].clone());
            measured["root_control"] = json!(id == root_id);
            measured["a_probability"] = json!(scores.get(id));
            measured["function_grammar_distance"] =
                state.map_or(Value::Null, |s| s["function_grammar_distance"].clone());
            measured["full_word_grammar_distance"] =
                state.map_or(Value::Null, |s| s["full_word_grammar_distance"].clone());
            measured["observed_joint_detector_target"] =
                json!(scores.get(id).is_some_and(
                    |a| *a < 0.1 && measured["flagged_fraction"].as_f64().unwrap() < 0.1
                ));
            observations.push(measured);
        }
    }
    ensure!(!observations.is_empty(), "No observations");
    let measured_ids: BTreeSet<_> = observations
        .iter()
        .filter_map(|v| v["text_sha256"].as_str())
        .collect();
    let unmeasured: Vec<_> = expected
        .iter()
        .filter(|id| !measured_ids.contains(id.as_str()))
        .collect();
    save(
        &out,
        &json!({
            "schema":"slop_ninja_b_external_report_v1",
            "plan_sha256":hash(fs::read(plan_path)?),"scores_sha256":hash(fs::read(scores_path)?),
            "annotation_files":files,"observations":observations,"unmeasured_menu_ids":unmeasured,
            "scope":"Adaptive measurements, not independent validation. AI and AI-assisted both count as flagged. Each window score and its word-weighted mean are auxiliary diagnostics, not calibrated content percentages. A score and Pangram fractions measure different quantities. Missing external observations are not successes. Reviews remain separate.",
            "semantic_preservation_certified":false,"research_only":true
        }),
    )?;
    ids.record(&out.display().to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> ProviderIds {
        ProviderIds::new(
            b"synthetic-provider-id-salt-v1".to_vec(),
            "unused-sidecar".into(),
        )
        .unwrap()
    }

    #[test]
    fn bulk_ids_are_digested_and_both_forms_name_one_request() {
        let mut row = fixture();
        row["bulk_id"] = json!("synthetic-bulk-0001");
        let result = observation(&row, "First. Second.", &ids()).unwrap();
        assert_eq!(result["bulk_id"], "redacted-d3ebf7237c923a1b");
        assert_eq!(result["request_id"], "x:0");
        let raw = request_key(&row, &ids()).unwrap();
        row["bulk_id"] = json!("redacted-d3ebf7237c923a1b");
        assert_eq!(request_key(&row, &ids()).unwrap(), raw);
        let again = observation(&row, "First. Second.", &ids()).unwrap();
        assert_eq!(again["bulk_id"], result["bulk_id"]);
    }

    fn fixture() -> Value {
        json!({"schema":"slop_ninja_b_pangram_annotation_v1","model_selector":"pangram-4",
            "submitted_text_sha256":hash("First. Second."),"returned_text_sha256":hash("First. Second."),
            "member":{"text_sha256":hash("First. Second."),"request_id":"x:0"},"echo_match":"exact",
            "observation":{"id":"x:0","error":null,"result":{"stage":"STAGE_SUCCESS","text":"First. Second.",
                "fraction_ai":0.0,"fraction_ai_assisted":1.0,"fraction_human":0.0,
                "windows":[{"text":"First.","word_count":1,"ai_assistance_score":0.2},{"text":"Second.","word_count":1,"ai_assistance_score":0.8}]}}})
    }

    #[test]
    fn assistance_remains_flagged_and_all_windows_contribute() {
        let result = observation(&fixture(), "First. Second.", &ids()).unwrap();
        assert_eq!(result["fraction_ai"], 0.0);
        assert_eq!(result["flagged_fraction"], 1.0);
        assert_eq!(result["windows"].as_array().unwrap().len(), 2);
        assert_eq!(result["word_weighted_window_score"], 0.5);
        assert!(result["windows"][0].get("text").is_none());
    }

    #[test]
    fn whitespace_permission_does_not_accept_changed_meaning() {
        let mut row = fixture();
        row["echo_match"] = json!("whitespace_only");
        row["observation"]["result"]["text"] = json!("First.\nSecond.");
        row["returned_text_sha256"] = json!(hash("First.\nSecond."));
        assert!(observation(&row, "First. Second.", &ids()).is_ok());
        row["observation"]["result"]["text"] = json!("First. Not second.");
        row["returned_text_sha256"] = json!(hash("First. Not second."));
        assert!(observation(&row, "First. Second.", &ids()).is_err());
    }
}
