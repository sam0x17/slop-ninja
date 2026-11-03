//! Pangram submissions and summaries of repeated, exact-input observations.
//!
//! Submission and polling are separate so callers can save a task ID before
//! waiting. A failed request never triggers another potentially billable POST.

use anyhow::{Result, anyhow, bail, ensure};
use chrono::Utc;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, Instant};

use crate::{provider_ids::ProviderIds, util::digest};

const API_BASE: &str = "https://text.external-api.pangram.com";
const QUALITY_DIMENSIONS: [&str; 4] = ["readability", "argumentation", "detail", "tone"];

/// Inspect with `anyhow::Error::downcast_ref` and save `metadata()` alongside
/// the submission record. Display excludes response bodies and credentials.
#[derive(Debug)]
pub struct PangramFailure {
    pub kind: String,
    pub task_id: Option<String>,
    pub response: Option<Value>,
    message: String,
}

impl PangramFailure {
    fn new(
        kind: &str,
        message: impl Into<String>,
        task_id: Option<&str>,
        response: Option<Value>,
    ) -> Self {
        Self {
            kind: kind.into(),
            task_id: task_id.map(str::to_owned),
            response,
            message: message.into(),
        }
    }

    pub fn metadata(&self) -> Value {
        json!({
            "kind": self.kind,
            "task_id": self.task_id,
            "response": self.response,
            "message": self.message,
        })
    }
}

impl fmt::Display for PangramFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PangramFailure {}

pub struct PangramClient {
    client: Client,
    key: String,
    base: String,
    request_timeout: Duration,
    poll_interval: Duration,
}

impl PangramClient {
    pub fn new() -> Result<Self> {
        let key = std::env::var("PANGRAM_API_KEY")
            .map_err(|_| anyhow!("Set PANGRAM_API_KEY in the environment"))?;
        Self::configured(key, API_BASE.into(), Duration::from_secs(2))
    }

    /// Explicit secret injection for research runners; the key is never logged.
    pub fn with_api_key(key: String) -> Result<Self> {
        Self::configured(key, API_BASE.into(), Duration::from_secs(2))
    }

    fn configured(key: String, base: String, poll_interval: Duration) -> Result<Self> {
        ensure!(!key.is_empty(), "Set PANGRAM_API_KEY in the environment");
        let request_timeout = Duration::from_secs(60);
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(request_timeout)
            .build()?;
        Ok(Self {
            client,
            key,
            base,
            request_timeout,
            poll_interval,
        })
    }

    fn request(&self, route: &str, payload: Option<Value>, timeout: Duration) -> Result<Value> {
        let url = format!("{}{route}", self.base);
        let request = match payload {
            Some(payload) => self.client.post(url).json(&payload),
            None => self.client.get(url),
        }
        .header("x-api-key", &self.key)
        .header("Content-Type", "application/json")
        .timeout(timeout);
        let response = request.send().map_err(|_| PangramFailure::new(
            "transport", "Pangram transport error; request was not retried. A submitted task might still have been charged.",
            None, None,
        ))?;
        let status = response.status();
        if !status.is_success() {
            return Err(PangramFailure::new(
                "http",
                format!("Pangram HTTP {}; request was not retried", status.as_u16()),
                None,
                None,
            )
            .into());
        }
        let bytes = response.bytes().map_err(|_| PangramFailure::new(
            "transport", "Pangram response could not be read; request was not retried. A submitted task might still have been charged.",
            None, None,
        ))?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            PangramFailure::new(
                "invalid_json",
                "Pangram returned invalid JSON; request was not retried",
                None,
                None,
            )
        })?;
        if !value.is_object() {
            return Err(PangramFailure::new(
                "invalid_response",
                "Pangram returned a non-object JSON response",
                None,
                Some(value),
            )
            .into());
        }
        Ok(value)
    }

    pub fn models(&self) -> Result<Value> {
        self.request("/models", None, self.request_timeout)
    }

    /// Save the returned record before polling. Text and its hash are exact.
    pub fn submit(&self, text: &str, model: &str) -> Result<Value> {
        ensure!(
            text.split_whitespace().count() >= 50,
            "Pangram requires natural-language prose of at least 50 words"
        );
        ensure!(
            !model.trim().is_empty(),
            "An explicit model selector is required"
        );
        let submitted_at = Utc::now().to_rfc3339();
        let response = self.request(
            "/task",
            Some(json!({
                "text": text, "model": model, "public_dashboard_link": false,
            })),
            self.request_timeout,
        )?;
        let task_id = match response
            .get("task_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            Some(task_id) => task_id.to_owned(),
            None => {
                return Err(PangramFailure::new(
                    "invalid_submission",
                    "Submission response omitted task_id; do not automatically resubmit",
                    None,
                    Some(response),
                )
                .into());
            }
        };
        Ok(json!({
            "detector": "pangram", "model": model, "task_id": task_id,
            "submitted_text": text, "sha256": digest(text),
            "submitted_at": submitted_at, "submission_response": response,
        }))
    }

    /// Resume an existing task. Even a hung HTTP request is bounded by the
    /// remaining polling budget; failures retain the last parsed response.
    pub fn poll(&self, task_id: &str, timeout_secs: f64) -> Result<Value> {
        ensure!(!task_id.is_empty(), "task_id must be a nonempty string");
        ensure!(
            timeout_secs.is_finite() && timeout_secs > 0.0,
            "timeout must be a finite positive number"
        );
        let timeout = Duration::try_from_secs_f64(timeout_secs)
            .map_err(|_| anyhow!("timeout is outside the supported duration range"))?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| anyhow!("timeout is outside the supported duration range"))?;
        // Encode a task ID as one path segment, never as a query or fragment.
        let escaped: String = task_id
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'~') {
                    (byte as char).to_string()
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect();
        let route = format!("/task/{escaped}");
        let mut last_response = None;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(PangramFailure::new(
                    "timeout",
                    format!("Polling timed out for {task_id}; resume this task ID."),
                    Some(task_id),
                    last_response,
                )
                .into());
            }
            let response = match self.request(&route, None, remaining.min(self.request_timeout)) {
                Ok(response) => response,
                Err(error) => {
                    if let Some(failure) = error.downcast_ref::<PangramFailure>() {
                        return Err(PangramFailure::new(
                            &failure.kind,
                            failure.message.clone(),
                            Some(task_id),
                            failure.response.clone().or(last_response),
                        )
                        .into());
                    }
                    return Err(error);
                }
            };
            if response
                .get("task_id")
                .filter(|returned| !returned.is_null())
                .is_some_and(|returned| returned != task_id)
            {
                return Err(PangramFailure::new(
                    "invalid_task",
                    "Pangram returned a different task ID",
                    Some(task_id),
                    Some(response),
                )
                .into());
            }
            match response.get("stage").and_then(Value::as_str) {
                Some("STAGE_SUCCESS") => return Ok(response),
                Some("STAGE_FAILED") => {
                    return Err(PangramFailure::new(
                        "task_failed",
                        format!("Pangram task {task_id} failed; preserve its response."),
                        Some(task_id),
                        Some(response),
                    )
                    .into());
                }
                Some(stage) if stage.starts_with("STAGE_") => {}
                _ => {
                    return Err(PangramFailure::new(
                        "invalid_stage",
                        "Pangram returned an invalid task stage",
                        Some(task_id),
                        Some(response),
                    )
                    .into());
                }
            }
            last_response = Some(response);
            let remaining = deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(self.poll_interval.min(remaining));
        }
    }
}

type GroupKey = (String, String, String, String);

#[derive(Debug, Clone, PartialEq)]
struct Observation {
    task_id: String,
    ai: f64,
    assisted: f64,
    combined: f64,
    human: f64,
    full_document: bool,
    returned_text_differs: bool,
}

fn fraction(result: &Value, key: &str) -> Result<f64> {
    let value = result
        .get(key)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && (0.0..=1.0).contains(n))
        .ok_or_else(|| anyhow!("{key} must be a finite number in [0, 1]"))?;
    Ok(value)
}

fn nonempty(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn validate_record(record: &Value) -> Result<(GroupKey, Observation)> {
    let text = nonempty(record.get("submitted_text"))
        .ok_or_else(|| anyhow!("Each run needs nonempty submitted_text"))?;
    let hash = digest(text);
    ensure!(
        record
            .get("sha256")
            .is_none_or(|stored| stored == hash.as_str()),
        "Stored sha256 does not match submitted_text"
    );
    let result = record
        .get("result")
        .filter(|v| v.is_object() && v["stage"] == "STAGE_SUCCESS")
        .ok_or_else(|| anyhow!("Only completed successful runs can be summarized"))?;
    let detector = match record.get("detector") {
        None => Some("pangram"),
        value => nonempty(value),
    };
    let (Some(detector), Some(model), Some(version), Some(task_id)) = (
        detector,
        nonempty(record.get("model")),
        nonempty(result.get("version")),
        nonempty(record.get("task_id")),
    ) else {
        bail!("Each run needs detector, model, response version, and task_id");
    };
    ensure!(
        result
            .get("task_id")
            .is_none_or(|returned| returned == task_id),
        "Result task_id does not match submitted task_id"
    );
    let ai = fraction(result, "fraction_ai")?;
    let assisted = fraction(result, "fraction_ai_assisted")?;
    let human = fraction(result, "fraction_human")?;
    ensure!(
        (ai + assisted + human - 1.0).abs() <= 1e-6,
        "The three fractions must sum to 1 within rounding tolerance"
    );
    Ok((
        (detector.into(), model.into(), version.into(), hash),
        Observation {
            task_id: task_id.into(),
            ai,
            assisted,
            combined: (ai + assisted).min(1.0),
            human,
            full_document: record.get("full_document") == Some(&Value::Bool(true)),
            returned_text_differs: result
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|r| r != text),
        },
    ))
}

/// Check one stored run as `summarize_runs` would, without writing a summary.
pub fn validate_run(record: &Value) -> Result<()> {
    validate_record(record).map(|_| ())
}

fn settings(threshold: f64, min_repeats: usize) -> Result<()> {
    ensure!(
        threshold.is_finite() && threshold > 0.0 && threshold <= 1.0,
        "threshold must be finite and in (0, 1]"
    );
    ensure!(min_repeats > 0, "min_repeats must be a positive integer");
    Ok(())
}

fn stats(values: &[f64]) -> Value {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    let median = if n.is_multiple_of(2) {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    } else {
        sorted[n / 2]
    };
    json!({"min": sorted[0], "median": median, "mean": values.iter().sum::<f64>() / n as f64,
        "max": sorted[n - 1], "values": values})
}

/// Group by exact input, detector, selector, and response version. Repeated
/// records for the same task never add evidence. Observed success requires
/// every distinct run below the strict combined AI + assisted threshold.
/// `task_ids` holds salted digests; a task is identified by its digest, so a
/// raw id and its digest name the same task.
pub fn summarize_runs(
    records: &[Value],
    threshold: f64,
    min_repeats: usize,
    ids: &ProviderIds,
) -> Result<Value> {
    settings(threshold, min_repeats)?;
    let mut grouped: BTreeMap<GroupKey, Vec<Observation>> = BTreeMap::new();
    let mut seen: BTreeMap<(String, String), (GroupKey, Observation)> = BTreeMap::new();
    let mut duplicates = 0;
    for record in records {
        let (key, mut observation) = validate_record(record)?;
        observation.task_id = ids.shareable(&observation.task_id);
        let task_key = (key.0.clone(), observation.task_id.clone());
        if let Some(previous) = seen.get(&task_key) {
            ensure!(
                previous == &(key, observation),
                "The same task ID has conflicting observations"
            );
            duplicates += 1;
            continue;
        }
        seen.insert(task_key, (key.clone(), observation.clone()));
        grouped.entry(key).or_default().push(observation);
    }
    let groups: Vec<Value> = grouped
        .into_iter()
        .map(|((detector, model, version, hash), observations)| {
            let enough = observations.len() >= min_repeats;
            let ai: Vec<f64> = observations.iter().map(|r| r.ai).collect();
            let combined: Vec<f64> = observations.iter().map(|r| r.combined).collect();
            json!({
                "detector": detector, "model": model, "version": version, "sha256": hash,
                "repeat_count": observations.len(), "ai_fraction": stats(&ai),
                "ai_plus_assisted_fraction": stats(&combined), "sufficient_repeats": enough,
                "ai_only_observed_success": enough && ai.iter().all(|v| *v < threshold),
                "observed_success": enough && combined.iter().all(|v| *v < threshold),
                "full_document": observations.iter().all(|r| r.full_document),
                "normalization_observed": observations.iter().any(|r| r.returned_text_differs),
                "task_ids": observations.iter().map(|r| &r.task_id).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(json!({
        "threshold": threshold, "comparison": "strictly_less_than", "min_repeats": min_repeats,
        "record_count": records.len(), "unique_runs": seen.len(), "duplicate_records_ignored": duplicates,
        "all_groups_observed_success": !groups.is_empty() && groups.iter().all(|g| g["observed_success"] == true),
        "groups": groups, "reliability_established": false,
        "interpretation": "These are repeated observations of particular inputs. They do not establish reliability on unseen documents or determine a text's authorship.",
    }))
}

/// Lists are ordered, temporally matched baseline/candidate pairs. Human
/// quality flags are attestations; this function cannot verify the reviewer.
pub fn paired_study(
    baseline_runs: &[Value],
    candidate_runs: &[Value],
    quality_audit: &Value,
    threshold: f64,
    min_repeats: usize,
    ids: &ProviderIds,
) -> Result<Value> {
    ensure!(
        !baseline_runs.is_empty() && baseline_runs.len() == candidate_runs.len(),
        "Provide equally sized, nonempty lists of matched repeat pairs"
    );
    ensure!(
        quality_audit.is_object(),
        "quality_audit must be an object of human review attestations"
    );
    let baseline = summarize_runs(baseline_runs, threshold, min_repeats, ids)?;
    let candidate = summarize_runs(candidate_runs, threshold, min_repeats, ids)?;
    ensure!(
        baseline["duplicate_records_ignored"] == 0 && candidate["duplicate_records_ignored"] == 0,
        "Paired studies require distinct submitted task IDs in each arm"
    );
    ensure!(
        baseline_runs
            .iter()
            .chain(candidate_runs)
            .all(|r| r.get("full_document") == Some(&Value::Bool(true))),
        "Every paired run must explicitly have full_document=true"
    );
    let base_groups = baseline["groups"]
        .as_array()
        .expect("summary groups are an array");
    let edit_groups = candidate["groups"]
        .as_array()
        .expect("summary groups are an array");
    let base_hashes: BTreeSet<&str> = base_groups
        .iter()
        .map(|g| g["sha256"].as_str().unwrap())
        .collect();
    let edit_hashes: BTreeSet<&str> = edit_groups
        .iter()
        .map(|g| g["sha256"].as_str().unwrap())
        .collect();
    ensure!(
        base_hashes.len() == 1 && edit_hashes.len() == 1,
        "Each arm must contain repeated observations of one exact document"
    );
    ensure!(
        base_hashes != edit_hashes,
        "Baseline and candidate must be different documents"
    );
    let base_tasks: BTreeSet<(String, String)> = baseline_runs
        .iter()
        .map(|record| {
            validate_record(record)
                .map(|(key, observation)| (key.0, ids.shareable(&observation.task_id)))
        })
        .collect::<Result<_>>()?;
    let mut deltas: BTreeMap<(String, String, String), Vec<f64>> = BTreeMap::new();
    for (original, edited) in baseline_runs.iter().zip(candidate_runs) {
        let (base_key, base_obs) = validate_record(original)?;
        let (edit_key, edit_obs) = validate_record(edited)?;
        ensure!(
            (&base_key.0, &base_key.1, &base_key.2) == (&edit_key.0, &edit_key.1, &edit_key.2),
            "Each pair must use the same detector, model selector, and version"
        );
        ensure!(
            !base_tasks.contains(&(edit_key.0.clone(), ids.shareable(&edit_obs.task_id))),
            "Baseline and candidate cannot share a submitted task"
        );
        deltas
            .entry((base_key.0, base_key.1, base_key.2))
            .or_default()
            .push(edit_obs.combined - base_obs.combined);
    }
    let quality_passed = quality_audit["reviewed_by_human"] == true
        && QUALITY_DIMENSIONS
            .iter()
            .all(|dimension| quality_audit[dimension] == true);
    let sufficient = base_groups
        .iter()
        .chain(edit_groups)
        .all(|g| g["sufficient_repeats"] == true);
    let observed_success =
        quality_passed && sufficient && candidate["all_groups_observed_success"] == true;
    let paired_deltas: Vec<Value> = deltas
        .into_iter()
        .map(|((detector, model, version), values)| {
            json!({
                "detector": detector, "model": model, "version": version,
                "candidate_minus_baseline_ai_plus_assisted": stats(&values),
            })
        })
        .collect();
    Ok(json!({
        "baseline": baseline, "candidate": candidate, "paired_deltas": paired_deltas,
        "quality_audit": quality_audit, "quality_passed": quality_passed,
        "sufficient_repeats": sufficient, "observed_success": observed_success,
        "reliability_established": false,
        "interpretation": "Passing this document requires all quality checks and every repeated candidate score below threshold. Estimate general reliability separately on untouched source-group holdouts across domains, generators, and detectors.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn run(task: &str, ai: f64, assisted: f64, text: &str) -> Value {
        json!({"task_id": task, "model": "pangram-4", "submitted_text": text,
            "sha256": digest(text), "full_document": true,
            "result": {"stage": "STAGE_SUCCESS", "version": "4.0", "fraction_ai": ai,
                "fraction_ai_assisted": assisted, "fraction_human": 1.0 - ai - assisted}})
    }

    fn repeats(text: &str, ai: f64, assisted: f64, prefix: &str) -> Vec<Value> {
        (0..3)
            .map(|n| run(&format!("{prefix}{n}"), ai, assisted, text))
            .collect()
    }

    fn audit() -> Value {
        json!({"reviewed_by_human": true, "readability": true, "argumentation": true, "detail": true, "tone": true})
    }

    // A synthetic salt; these tests never write a sidecar map.
    fn ids() -> ProviderIds {
        ProviderIds::new(
            b"synthetic-provider-id-salt-v1".to_vec(),
            "unused-sidecar".into(),
        )
        .unwrap()
    }

    // Shadow the library functions with the synthetic salt bound in.
    fn summarize_runs(records: &[Value], threshold: f64, min_repeats: usize) -> Result<Value> {
        super::summarize_runs(records, threshold, min_repeats, &ids())
    }

    fn paired_study(
        baseline: &[Value],
        candidate: &[Value],
        audit: &Value,
        threshold: f64,
        min_repeats: usize,
    ) -> Result<Value> {
        super::paired_study(baseline, candidate, audit, threshold, min_repeats, &ids())
    }

    #[test]
    fn task_ids_are_salted_digests_and_both_forms_name_one_task() {
        let records = vec![
            run("synthetic-task-0001", 0.03, 0.01, "Original"),
            run("synthetic-task-0002", 0.02, 0.01, "Original"),
        ];
        let summary = summarize_runs(&records, 0.1, 3).unwrap();
        let listed = summary["groups"][0]["task_ids"].as_array().unwrap();
        assert_eq!(listed[0], "redacted-6730f8b88427cc9b");
        assert!(listed.iter().all(|id| {
            crate::provider_ids::is_digest(id.as_str().unwrap())
                && !id.as_str().unwrap().contains("synthetic-task")
        }));
        // A record already carrying the digest is the same task as its raw form.
        let mut redacted = records[0].clone();
        redacted["task_id"] = json!("redacted-6730f8b88427cc9b");
        let mixed = summarize_runs(&[records[0].clone(), redacted.clone()], 0.1, 3).unwrap();
        assert_eq!(mixed["unique_runs"], 1);
        assert_eq!(mixed["duplicate_records_ignored"], 1);
        assert_eq!(mixed["groups"][0]["task_ids"], json!([listed[0]]));
        validate_run(&redacted).unwrap();
        let mut failed = redacted;
        failed["result"]["stage"] = json!("STAGE_FAILED");
        assert!(validate_run(&failed).is_err());
    }

    #[test]
    fn paired_tasks_are_compared_by_digest() {
        let base = repeats("Original", 0.7, 0.01, "synthetic-b");
        let mut reused = repeats("Edited", 0.04, 0.01, "synthetic-c");
        reused[0]["task_id"] = json!(ids().shareable("synthetic-b1"));
        assert!(
            paired_study(&base, &reused, &audit(), 0.1, 3)
                .unwrap_err()
                .to_string()
                .contains("cannot share")
        );
    }

    #[test]
    fn reports_all_observations_and_worst_case() {
        let records = vec![
            run("a", 0.03, 0.01, "Original"),
            run("b", 0.02, 0.01, "Original"),
            run("c", 0.21, 0.01, "Original"),
        ];
        let summary = summarize_runs(&records, 0.1, 3).unwrap();
        let group = &summary["groups"][0];
        assert_eq!(group["repeat_count"], 3);
        assert_eq!(group["ai_fraction"]["median"], 0.03);
        assert!((group["ai_plus_assisted_fraction"]["max"].as_f64().unwrap() - 0.22).abs() < 1e-12);
        assert_eq!(group["observed_success"], false);
        assert_eq!(summary["reliability_established"], false);
    }

    #[test]
    fn requires_three_distinct_tasks_and_counts_duplicates() {
        let records = repeats("Original", 0.04, 0.01, "a");
        assert_eq!(
            summarize_runs(&records[..2], 0.1, 3).unwrap()["all_groups_observed_success"],
            false
        );
        assert_eq!(
            summarize_runs(&records, 0.1, 3).unwrap()["all_groups_observed_success"],
            true
        );
        let summary = summarize_runs(
            &[records[0].clone(), records[1].clone(), records[1].clone()],
            0.1,
            3,
        )
        .unwrap();
        assert_eq!(summary["duplicate_records_ignored"], 1);
        assert_eq!(summary["all_groups_observed_success"], false);
    }

    #[test]
    fn exact_threshold_and_assistance_do_not_pass() {
        for (ai, assisted) in [(0.08, 0.02), (0.0, 0.25)] {
            let summary = summarize_runs(&repeats("Original", ai, assisted, "a"), 0.1, 3).unwrap();
            assert_eq!(summary["groups"][0]["observed_success"], false);
            assert_eq!(summary["groups"][0]["ai_only_observed_success"], true);
        }
    }

    #[test]
    fn provenance_axes_form_distinct_groups() {
        let mut records = vec![
            run("a", 0.04, 0.01, "Original"),
            run("b", 0.04, 0.01, "Other"),
        ];
        let mut model = run("c", 0.04, 0.01, "Original");
        model["model"] = json!("default");
        records.push(model);
        let mut version = run("d", 0.04, 0.01, "Original");
        version["result"]["version"] = json!("4.1");
        records.push(version);
        let mut detector = records[0].clone();
        detector["detector"] = json!("other");
        records.push(detector);
        let summary = summarize_runs(&records, 0.1, 3).unwrap();
        assert_eq!(summary["groups"].as_array().unwrap().len(), 5);
        assert_eq!(summary["all_groups_observed_success"], false);
    }

    #[test]
    fn rejects_hash_conflicts_and_same_task_conflicts() {
        let original = run("a", 0.04, 0.01, "Original");
        let mut changed = original.clone();
        changed["submitted_text"] = json!("Original\n");
        assert!(
            summarize_runs(&[changed], 0.1, 3)
                .unwrap_err()
                .to_string()
                .contains("sha256")
        );
        assert!(
            summarize_runs(&[original, run("a", 0.9, 0.01, "Original")], 0.1, 3)
                .unwrap_err()
                .to_string()
                .contains("conflicting")
        );
    }

    #[test]
    fn reports_normalization_without_replacing_input_hash() {
        let mut record = run("a", 0.04, 0.01, "Original\r\n");
        record["result"]["text"] = json!("Original\n");
        let summary = summarize_runs(&[record.clone()], 0.1, 3).unwrap();
        assert_eq!(summary["groups"][0]["normalization_observed"], true);
        assert_eq!(summary["groups"][0]["sha256"], record["sha256"]);
    }

    #[test]
    fn rejects_invalid_fractions_missing_provenance_and_non_success() {
        for value in [
            json!(-0.1),
            json!(1.1),
            json!(true),
            json!(".1"),
            Value::Null,
        ] {
            let mut record = run("a", 0.04, 0.01, "Original");
            record["result"]["fraction_ai"] = value;
            assert!(summarize_runs(&[record], 0.1, 3).is_err());
        }
        let mut record = run("a", 0.04, 0.01, "Original");
        record["result"]["fraction_human"] = json!(0.2);
        assert!(
            summarize_runs(&[record], 0.1, 3)
                .unwrap_err()
                .to_string()
                .contains("sum to 1")
        );
        for field in ["task_id", "model", "submitted_text"] {
            let mut record = run("a", 0.04, 0.01, "Original");
            record.as_object_mut().unwrap().remove(field);
            assert!(summarize_runs(&[record], 0.1, 3).is_err());
        }
        let mut record = run("a", 0.04, 0.01, "Original");
        record["result"]["stage"] = json!("STAGE_FAILED");
        assert!(
            summarize_runs(&[record], 0.1, 3)
                .unwrap_err()
                .to_string()
                .contains("successful")
        );
    }

    #[test]
    fn validates_settings_and_empty_summaries_do_not_pass() {
        for threshold in [0.0, f64::NAN, f64::INFINITY, -0.1, 1.1] {
            assert!(summarize_runs(&[], threshold, 3).is_err());
        }
        assert!(summarize_runs(&[], 0.1, 0).is_err());
        assert_eq!(
            summarize_runs(&[], 0.1, 3).unwrap()["all_groups_observed_success"],
            false
        );
    }

    #[test]
    fn paired_gate_requires_every_quality_attestation() {
        let base = repeats("Original", 0.7, 0.01, "b");
        let candidate = repeats("Edited", 0.04, 0.01, "c");
        let study = paired_study(&base, &candidate, &audit(), 0.1, 3).unwrap();
        assert_eq!(study["observed_success"], true);
        assert_eq!(study["reliability_established"], false);
        assert!(
            (study["paired_deltas"][0]["candidate_minus_baseline_ai_plus_assisted"]["mean"]
                .as_f64()
                .unwrap()
                + 0.66)
                .abs()
                < 1e-12
        );
        for dimension in QUALITY_DIMENSIONS.into_iter().chain(["reviewed_by_human"]) {
            let mut review = audit();
            review[dimension] = json!(false);
            assert_eq!(
                paired_study(&base, &candidate, &review, 0.1, 3).unwrap()["observed_success"],
                false
            );
        }
    }

    #[test]
    fn paired_gate_requires_full_scope_and_one_exact_document() {
        let base = repeats("Original", 0.7, 0.01, "b");
        let mut candidate = repeats("Edited", 0.04, 0.01, "c");
        candidate[0]["full_document"] = json!(false);
        assert!(
            paired_study(&base, &candidate, &audit(), 0.1, 3)
                .unwrap_err()
                .to_string()
                .contains("full_document")
        );
        candidate[0] = run("c0", 0.04, 0.01, "Third text");
        assert!(
            paired_study(&base, &candidate, &audit(), 0.1, 3)
                .unwrap_err()
                .to_string()
                .contains("one exact document")
        );
        assert!(
            paired_study(
                &base,
                &repeats("Original", 0.04, 0.01, "c"),
                &audit(),
                0.1,
                3
            )
            .unwrap_err()
            .to_string()
            .contains("different documents")
        );
    }

    #[test]
    fn rejects_unmatched_pairs_reused_tasks_and_nonobject_audit() {
        let base = repeats("Original", 0.7, 0.01, "b");
        let mut candidate = repeats("Edited", 0.04, 0.01, "c");
        candidate[0]["model"] = json!("default");
        assert!(
            paired_study(&base, &candidate, &audit(), 0.1, 3)
                .unwrap_err()
                .to_string()
                .contains("same detector")
        );
        assert!(paired_study(&base, &candidate[..2], &audit(), 0.1, 3).is_err());
        assert!(
            paired_study(&vec![base[0].clone(); 3], &candidate, &audit(), 0.1, 3)
                .unwrap_err()
                .to_string()
                .contains("distinct submitted")
        );
        assert!(paired_study(&base, &candidate, &json!(true), 0.1, 3).is_err());
        let mut reused = repeats("Edited", 0.04, 0.01, "c");
        reused[0]["task_id"] = json!("b1");
        assert!(
            paired_study(&base, &reused, &audit(), 0.1, 3)
                .unwrap_err()
                .to_string()
                .contains("cannot share")
        );
    }

    // Each fixture closes the socket, making any accidental retry visible.
    fn server(
        responses: Vec<(u16, String)>,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let handle = thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut buffer = [0_u8; 4096];
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(header_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers =
                            String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .map(|n| n.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= header_end + 4 + length {
                            break;
                        }
                    }
                }
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(bytes).unwrap());
                write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\nLocation: /redirected\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        (address, requests, handle)
    }

    fn client(base: String) -> PangramClient {
        PangramClient::configured("secret-key".into(), base, Duration::from_millis(1)).unwrap()
    }

    #[test]
    fn submit_preserves_exact_text_and_disables_public_link() {
        let (base, requests, handle) = server(vec![(200, json!({"task_id": "job-1"}).to_string())]);
        let text = "Café means a place to sit and read. ".repeat(7) + "\r\n";
        let record = client(base).submit(&text, "pangram-4").unwrap();
        handle.join().unwrap();
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert!(captured[0].starts_with("POST /task "));
        let payload: Value =
            serde_json::from_str(captured[0].split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(payload["text"], text);
        assert_eq!(payload["public_dashboard_link"], false);
        assert_eq!(record["submitted_text"], text);
        assert_eq!(record["sha256"], digest(&text));
        assert!(!record.to_string().contains("secret-key"));
    }

    #[test]
    fn rejects_http_errors_and_redirects_without_retry_or_body_echo() {
        for status in [402, 503, 307] {
            let (base, requests, handle) = server(vec![(status, "secret-key private text".into())]);
            let error = client(base)
                .submit(&"word ".repeat(50), "pangram-4")
                .unwrap_err();
            handle.join().unwrap();
            assert!(error.to_string().contains(&status.to_string()));
            assert!(!error.to_string().contains("secret"));
            assert_eq!(requests.lock().unwrap().len(), 1);
        }
    }

    #[test]
    fn rejects_malformed_json_without_retry_and_preserves_invalid_submission() {
        let (base, requests, handle) = server(vec![(200, "{\"task_id\": NaN}".into())]);
        let error = client(base)
            .submit(&"word ".repeat(50), "pangram-4")
            .unwrap_err();
        handle.join().unwrap();
        assert!(error.to_string().contains("invalid JSON"));
        assert_eq!(requests.lock().unwrap().len(), 1);
        let (base, _, handle) = server(vec![(200, json!({"unexpected": "saved"}).to_string())]);
        let error = client(base)
            .submit(&"word ".repeat(50), "pangram-4")
            .unwrap_err();
        handle.join().unwrap();
        assert_eq!(
            error.downcast_ref::<PangramFailure>().unwrap().metadata()["response"]["unexpected"],
            "saved"
        );
    }

    #[test]
    fn polling_preserves_terminal_failure_and_encodes_task_segment() {
        let (base, requests, handle) = server(vec![
            (200, json!({"stage": "STAGE_PREPROCESSING"}).to_string()),
            (
                200,
                json!({"stage": "STAGE_SUCCESS", "fraction_ai": 0.1}).to_string(),
            ),
        ]);
        assert_eq!(
            client(base).poll("job/1?x=y", 5.0).unwrap()["stage"],
            "STAGE_SUCCESS"
        );
        handle.join().unwrap();
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .all(|r| r.starts_with("GET /task/job%2F1%3Fx%3Dy "))
        );
        let failure = json!({"stage": "STAGE_FAILED", "headline": "Input error"});
        let (base, _, handle) = server(vec![(200, failure.to_string())]);
        let error = client(base).poll("job-1", 5.0).unwrap_err();
        handle.join().unwrap();
        let metadata = error.downcast_ref::<PangramFailure>().unwrap().metadata();
        assert_eq!(metadata["task_id"], "job-1");
        assert_eq!(metadata["response"], failure);
    }

    #[test]
    fn timeout_retains_task_and_last_response() {
        let pending = json!({"stage": "STAGE_PREPROCESSING"});
        let (base, _, handle) = server(vec![(200, pending.to_string())]);
        let client =
            PangramClient::configured("secret-key".into(), base, Duration::from_secs(1)).unwrap();
        let error = client.poll("job-1", 0.05).unwrap_err();
        handle.join().unwrap();
        let failure = error.downcast_ref::<PangramFailure>().unwrap();
        assert_eq!(failure.kind, "timeout");
        assert_eq!(failure.task_id.as_deref(), Some("job-1"));
        assert_eq!(failure.response.as_ref(), Some(&pending));
    }

    #[test]
    fn invalid_inputs_never_reach_network() {
        let client = client("http://127.0.0.1:1".into());
        assert!(client.submit("short", "pangram-4").is_err());
        assert!(client.submit(&"word ".repeat(50), "").is_err());
        for timeout in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::MAX] {
            assert!(client.poll("job-1", timeout).is_err());
        }
        assert!(client.poll("", 1.0).is_err());
    }
}
