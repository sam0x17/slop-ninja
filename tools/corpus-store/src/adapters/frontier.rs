//! Hosted frontier generation: collector plan directories (every request and
//! attempt, with raw receipts) and the published per-model exports.

use super::util::{f, get, i, s, strip_long};
use super::Adapter;
use crate::archive::Line;
use crate::runner::Input;
use crate::sink::{Event, Record, Sink};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashMap;

/// One collector plan directory (`frontier_wave`). Input roles, in order:
/// `state`, `withdrawn`, `refusal`, `stdout`, `receipt`, `requests`,
/// `passages`, and any other role (archived only).
pub struct Wave {
    wave: String,
    state: HashMap<String, Value>,
    withdrawn: HashMap<String, Value>,
    /// attempt dir (relative) -> (response text, events digest)
    responses: HashMap<String, String>,
    /// SHA-256 of the earlier receipts' call-log key name.
    earlier_log_key_sha256: String,
}

impl Wave {
    pub fn new(p: &Value) -> Result<Wave> {
        Ok(Wave {
            wave: p.get("wave").and_then(|x| x.as_str()).unwrap_or("unknown").into(),
            state: HashMap::new(),
            withdrawn: HashMap::new(),
            responses: HashMap::new(),
            earlier_log_key_sha256: p
                .get("earlier_call_log_key_sha256")
                .and_then(|x| x.as_str())
                .unwrap_or(EARLIER_CALL_LOG_KEY_SHA256)
                .into(),
        })
    }
}

/// Attempt receipts with the attempt-receipt schema keep the optional
/// call-log rows under `request_log`. Earlier receipts have no schema field and
/// keep them under an older key, identified by the SHA-256 of its name (a plan
/// can override it with the source parameter `earlier_call_log_key_sha256`).
pub const EARLIER_CALL_LOG_KEY_SHA256: &str = "e8b5bace5b489c2b77657596263924455f5bbe281045a8ede929c9a9257d1154";

/// The call-log rows of an earlier receipt: the value of its key whose name
/// has the SHA-256 `key_sha256`.
pub fn earlier_call_log<'a>(v: &'a Value, key_sha256: &str) -> Option<&'a Value> {
    v.as_object()?
        .iter()
        .find(|(k, _)| hex::encode(crate::skeleton::sha256(k.as_bytes())) == key_sha256)
        .map(|(_, x)| x)
}

fn parent(rel: &str) -> &str {
    rel.rsplit_once('/').map(|x| x.0).unwrap_or("")
}

/// `runs/<label>/<request>/attempt-N/<file>` -> (label, request, attempt dir, N).
fn attempt_parts(rel: &str) -> Option<(String, String, String, i64)> {
    let parts: Vec<&str> = rel.split('/').collect();
    let k = parts.iter().position(|p| *p == "runs")?;
    let label = parts.get(k + 1)?;
    let req = parts.get(k + 2)?;
    let att = parts.get(k + 3)?;
    let n = att.strip_prefix("attempt-")?.parse().ok()?;
    Some((label.to_string(), req.to_string(), parts[..k + 4].join("/"), n))
}

/// The final response text in a CLI event stream line, if this line carries it.
pub fn response_of(v: &Value) -> Option<String> {
    match v.get("type").and_then(|x| x.as_str()) {
        Some("result") => v.get("result").and_then(|x| x.as_str()).map(String::from),
        Some("item.completed") => {
            let it = v.get("item")?;
            if it.get("type").and_then(|x| x.as_str()) == Some("agent_message") {
                it.get("text").and_then(|x| x.as_str()).map(String::from)
            } else {
                None
            }
        }
        _ => None,
    }
}

impl Adapter for Wave {
    fn needs_all_files(&self) -> bool {
        true
    }

    fn line(&mut self, sink: &mut Sink, input: &Input, l: &Line) -> Result<()> {
        let rel = input.rel.as_str();
        let Some(v) = l.value else { return Ok(()) };
        match input.role.as_str() {
            "state" => {
                if let Some((_, req)) = parent(rel).rsplit_once('/') {
                    self.state.insert(req.to_string(), v.clone());
                }
            }
            "withdrawn" => {
                if let Some(id) = s(v, &["request_id"]) {
                    self.withdrawn.insert(id.clone(), v.clone());
                    sink.event(Some(&l.at), &Event {
                        event_key: format!("frontier:{}:{}:withdrawal", self.wave, id),
                        kind: "frontier_withdrawal".into(),
                        generator_label: s(v, &["label"]),
                        status: "withdrawn".into(),
                        request_id: Some(id),
                        finished_utc: s(v, &["withdrawn_at"]),
                        extra: Some(strip_long(v, 160)),
                        ..Default::default()
                    })?;
                }
            }
            "refusal" => {
                let name = rel.rsplit('/').next().unwrap_or(rel);
                sink.event(Some(&l.at), &Event {
                    event_key: format!("frontier:{}:refusal:{}", self.wave, name),
                    kind: "frontier_passage_refusal".into(),
                    generator_label: s(v, &["label"]),
                    status: "refused".into(),
                    request_id: s(v, &["request"]),
                    finished_utc: s(v, &["at"]),
                    extra: Some(strip_long(v, 160)),
                    ..Default::default()
                })?;
            }
            "stdout" => {
                if let Some(t) = response_of(v) {
                    self.responses.insert(parent(rel).to_string(), t);
                }
            }
            "receipt" => {
                let Some((label, req, dir, n)) = attempt_parts(rel) else {
                    sink.problem("frontier_receipt_path", Some(rel), None, "not under runs/<label>/<request>/attempt-N")?;
                    return Ok(());
                };
                let resp = self.responses.get(&dir).cloned();
                let mut response_sha = None;
                if let Some(t) = &resp {
                    let h = sink.blob(t.as_bytes())?;
                    if let Some(d) = s(v, &["text_sha256"]) {
                        if d != h {
                            sink.problem("frontier_response_digest", Some(rel), None, &format!("receipt {d} events {h}"))?;
                        }
                    }
                    response_sha = Some(h);
                } else if s(v, &["status"]).as_deref() == Some("accepted") {
                    sink.problem("frontier_response_missing", Some(rel), None, "accepted receipt without a response in its event stream")?;
                }
                let logged_model = v.get("request_log").or_else(|| earlier_call_log(v, &self.earlier_log_key_sha256)).and_then(|p| p.as_array()).and_then(|a| a.iter().find_map(|x| s(x, &["model"])));
                let params = json!({
                    "effort": get(v, "effort"), "transport": get(v, "transport"), "argv": get(v, "argv"),
                    "cli_versions": get(v, "cli_versions"), "usage": get(v, "usage"), "latency_ms": get(v, "latency_ms"),
                    "exit_code": get(v, "exit_code"), "timed_out": get(v, "timed_out"),
                    "transport_config_sha256": get(v, "transport_config_sha256"), "collector_sha256": get(v, "collector_sha256"),
                    "call_log_model": logged_model, "identity_source": get(v, "identity_source"),
                });
                sink.event(Some(&l.at), &Event {
                    event_key: format!("frontier:{}:{}:attempt-{}", self.wave, req, n),
                    kind: "frontier_attempt".into(),
                    generator_label: Some(label.clone()),
                    generator_key: Some(label),
                    requested_model: s(v, &["selector_requested"]),
                    reported_model: s(v, &["model_reported"]),
                    status: s(v, &["status"]).unwrap_or_else(|| "unknown".into()),
                    prompt_sha256: s(v, &["prompt_sha256"]),
                    params: Some(params),
                    receipt_sha256: s(v, &["events_sha256"]),
                    response_sha256: response_sha,
                    request_id: Some(req),
                    attempt: Some(n),
                    started_utc: s(v, &["started_at"]),
                    finished_utc: s(v, &["finished_at"]),
                    extra: Some(json!({"error": get(v, "error"), "screen_error": get(v, "screen_error"),
                        "stderr_sha256": get(v, "stderr_sha256"), "receipt_file_sha256": l.at.file_sha256, "wave": self.wave,
                        "cost_basis": get(v, "cost_basis"), "cost_estimate": get(v, "cost_usd_estimate")})),
                    ..Default::default()
                })?;
            }
            "requests" => {
                let Some(id) = s(v, &["id"]) else { return Ok(()) };
                let st = self.state.get(&id);
                let wd = self.withdrawn.get(&id);
                let status = match st.and_then(|x| s(x, &["status"])) {
                    Some(x) => x,
                    None => "not_started".into(),
                };
                let params = json!({
                    "selector": get(v, "selector"), "effort": get(v, "effort"), "transport": get(v, "transport"),
                    "target_words": get(v, "target_words"), "template": get(v, "template"), "kind": get(v, "kind"),
                    "bin": get(v, "bin"), "cell": get(v, "cell"), "source_byte_span": get(v, "source_byte_span"),
                    "voice_id": v.get("voice").and_then(|x| x.get("id")), "voice_ladder": get(v, "voice_ladder"),
                });
                sink.event(Some(&l.at), &Event {
                    event_key: format!("frontier:{}:{}", self.wave, id),
                    kind: "frontier_request".into(),
                    generator_label: s(v, &["label"]),
                    generator_key: s(v, &["label"]),
                    requested_model: s(v, &["selector"]),
                    operation: s(v, &["kind"]),
                    status,
                    prompt_sha256: s(v, &["prompt_sha256"]),
                    params: Some(params),
                    request_id: Some(id),
                    input_text_sha256: s(v, &["source_sha256"]),
                    split_hint: s(v, &["split"]),
                    extra: Some(json!({"wave": self.wave, "passage_id": get(v, "passage_id"), "source_group": get(v, "source_group"),
                        "domain": get(v, "domain"), "state": st.map(|x| strip_long(x, 160)), "withdrawal": wd.map(|x| strip_long(x, 160))})),
                    ..Default::default()
                })?;
            }
            "passages" => {
                let Some(t) = get(v, "text").and_then(|x| x.as_str()) else { return Ok(()) };
                let h = sink.text(t)?;
                sink.record(&l.at, &Record {
                    role: "frontier_source_passage".into(),
                    record_id: s(v, &["id"]),
                    text_sha256: Some(h),
                    collection: s(v, &["collection"]),
                    domain: s(v, &["domain"]),
                    source_group: s(v, &["source_group"]),
                    writer_key: s(v, &["writer_key"]),
                    licence: s(v, &["rights.license"]),
                    cohort_id: s(v, &["rights.cohort_id"]),
                    origin: Some("human_only".into()),
                    split_hint: s(v, &["frozen_split", "split_assigned"]),
                    ..Default::default()
                })?;
            }
            "run_record" => {
                // Earlier hosted style collector: runs/<hash>/record.json = one call.
                let name = parent(rel).rsplit('/').next().unwrap_or(rel).to_string();
                let prompt_sha = match get(v, "prompt").and_then(|x| x.as_str()) {
                    Some(p) => Some(sink.blob(p.as_bytes())?),
                    None => None,
                };
                let out = match get(v, "document.text").and_then(|x| x.as_str()) {
                    Some(t) => Some(sink.text(t)?),
                    None => None,
                };
                sink.event(Some(&l.at), &Event {
                    event_key: format!("hosted:{}:{}", self.wave, name),
                    kind: "hosted_style_call".into(),
                    generator_label: s(v, &["document.model", "model_requested"]),
                    requested_model: s(v, &["model_requested"]),
                    reported_model: s(v, &["document.model"]),
                    status: s(v, &["status"]).unwrap_or_else(|| "unknown".into()),
                    prompt_sha256: prompt_sha,
                    response_sha256: out.clone(),
                    output_text_sha256: out,
                    request_id: Some(name),
                    started_utc: s(v, &["requested_at"]),
                    split_hint: s(v, &["document.split"]),
                    extra: Some(json!({"provider": get(v, "provider"), "wave": self.wave,
                        "metadata": get(v, "document.metadata").map(|x| strip_long(x, 160)),
                        "domain": get(v, "document.domain"), "group_id": get(v, "document.group_id")})),
                    ..Default::default()
                })?;
            }
            "capture_request" => {
                if let (Some(rs), Some(p)) = (s(v, &["request_sha256"]), get(v, "request.prompt").and_then(|x| x.as_str())) {
                    let ps = sink.blob(p.as_bytes())?;
                    self.state.insert(format!("capture:{rs}"), json!({"prompt_sha256": ps,
                        "params": strip_long(&super::util::without(v.get("request").unwrap_or(&Value::Null), &["prompt"]), 160)}));
                }
            }
            "capture_review" => {
                let Some(t) = get(v, "text").and_then(|x| x.as_str()) else { return Ok(()) };
                let h = sink.text(t)?;
                let rs = s(v, &["request_sha256"]).unwrap_or_default();
                let req = self.state.get(&format!("capture:{rs}")).cloned().unwrap_or(Value::Null);
                let key = s(v, &["capture_sha256"]).unwrap_or_else(|| l.at.line_sha256.clone());
                sink.event(Some(&l.at), &Event {
                    event_key: format!("hosted_capture:{key}"),
                    kind: "hosted_capture".into(),
                    generator_label: s(v, &["generator_label"]),
                    requested_model: s(v, &["requested_model"]),
                    status: if v.get("content_checks_passed").and_then(|x| x.as_bool()) == Some(false) { "checks_failed".into() } else { "captured".into() },
                    prompt_sha256: s(&req, &["prompt_sha256"]),
                    params: req.get("params").cloned(),
                    receipt_sha256: s(v, &["stdout_sha256"]),
                    response_sha256: Some(h.clone()),
                    output_text_sha256: Some(h.clone()),
                    request_id: Some(rs),
                    started_utc: s(v, &["started_at"]),
                    finished_utc: s(v, &["closed_at"]),
                    input_text_sha256: s(v, &["source_text_sha256"]),
                    split_hint: s(v, &["split"]),
                    extra: Some(json!({"backend": get(v, "backend"), "invocation_sha256": get(v, "invocation_sha256"),
                        "prompt_profile": get(v, "prompt_profile"), "source_record_id": get(v, "source_record_id")})),
                    ..Default::default()
                })?;
                sink.record(&l.at, &Record {
                    role: "hosted_capture".into(),
                    record_id: s(v, &["capture_sha256"]),
                    text_sha256: Some(h),
                    dataset: Some(self.wave.clone()),
                    cohort_id: Some("frontier-captures-v1".into()),
                    origin: Some("model_only".into()),
                    share: Some(1.0),
                    generator_label: s(v, &["generator_label"]),
                    generator_key: s(v, &["requested_model"]),
                    operation: Some("source_conditioned_draft".into()),
                    recipe_sha256: s(&req, &["prompt_sha256"]),
                    parent_record_id: s(v, &["source_record_id"]),
                    parent_text_sha256: s(v, &["source_text_sha256"]),
                    split_hint: s(v, &["split"]),
                    ..Default::default()
                })?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Published per-model export (`frontier_export`): `records` (v1 or v2 schema),
/// `failures` and `not_assembled`.
pub struct Export;

impl Export {
    pub fn new(_: &Value) -> Result<Export> {
        Ok(Export)
    }
}

impl Adapter for Export {
    fn line(&mut self, sink: &mut Sink, input: &Input, l: &Line) -> Result<()> {
        let Some(v) = l.value else { return Ok(()) };
        match input.role.as_str() {
            "records" => {
                let Some(t) = get(v, "text").and_then(|x| x.as_str()) else { return Ok(()) };
                let h = sink.text(t)?;
                let spans = match get(v, "share_evidence.final_byte_span") {
                    Some(sp) => Some(json!([sp]).to_string()),
                    None => get(v, "ai_byte_spans").map(|x| x.to_string()),
                };
                let label = s(v, &["ai_attribution_label", "ai_identity"]);
                let admission = get(v, "training_admission").map(|x| strip_long(x, 160).to_string());
                sink.record(&l.at, &Record {
                    role: "frontier_positive".into(),
                    record_id: s(v, &["id"]),
                    text_sha256: Some(h),
                    dataset: Some(input.rel.split('/').next().unwrap_or("").to_string()),
                    collection: s(v, &["collection"]),
                    domain: s(v, &["domain"]),
                    source_group: s(v, &["source_group"]),
                    licence: s(v, &["rights.source.license"]),
                    rights_basis: s(v, &["rights.source.admission_decision"]),
                    cohort_id: s(v, &["cohort_id", "cohort"]),
                    origin: s(v, &["origin"]),
                    share: f(v, &["share_evidence.realized_share", "share_target"]),
                    ai_byte_spans: spans,
                    generator_label: label,
                    generator_key: Some(input.rel.split('/').next().unwrap_or("").to_string()),
                    operation: s(v, &["generation.kind"]),
                    recipe_sha256: s(v, &["generation.prompt_sha256"]),
                    parent_record_id: s(v, &["parent_id"]),
                    parent_text_sha256: s(v, &["share_evidence.source_sha256"]),
                    admission,
                    split_hint: s(v, &["frozen_split"]),
                    extra: Some(json!({
                        "cohort_wave": get(v, "cohort_wave"), "ai_identity_basis": get(v, "ai_identity_basis"),
                        "ai_identity_detail": get(v, "ai_identity_detail"),
                        "selector_requested": get(v, "generation.selector_requested"), "model_reported": get(v, "generation.model_reported"),
                        "template": get(v, "generation.template"), "bin": get(v, "generation.bin"),
                        "voice_id": get(v, "generation.voice.id"), "voice_ladder": get(v, "generation.voice_ladder"),
                        "events_sha256": get(v, "generation.events_sha256"), "receipt_dir": get(v, "generation.receipt_dir"),
                        "replacement_sha256": get(v, "share_evidence.replacement_sha256"), "plan_dir": get(v, "plan.dir"),
                        "schema": get(v, "schema"),
                    })),
                    ..Default::default()
                })?;
            }
            "failures" | "not_assembled" => {
                let id = s(v, &["request_id", "id"]).unwrap_or_default();
                let att = s(v, &["attempt"]).unwrap_or_default();
                sink.event(Some(&l.at), &Event {
                    event_key: format!("frontier_export:{}:{}:{}:{}", input.role, input.rel, id, att),
                    kind: format!("frontier_export_{}", input.role),
                    generator_key: Some(input.rel.split('/').next().unwrap_or("").to_string()),
                    status: s(v, &["status"]).unwrap_or_else(|| "unknown".into()),
                    request_id: Some(id),
                    attempt: i(v, &["attempt"]).or_else(|| att.strip_prefix("attempt-").and_then(|x| x.parse().ok())),
                    operation: s(v, &["kind"]),
                    extra: Some(strip_long(v, 160)),
                    ..Default::default()
                })?;
            }
            _ => {}
        }
        Ok(())
    }
}
