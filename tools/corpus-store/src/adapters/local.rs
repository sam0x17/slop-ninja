//! Local-model inference: llama-server request journals (positives and style
//! invention/review/characterisation), plan request lists, and finalized
//! positive record sets.

use super::util::{f, get, s, strip_long};
use super::Adapter;
use crate::archive::Line;
use crate::runner::Input;
use crate::sink::{At, Event, Record, Sink};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashMap;

/// Journals with paired intent/response lines (`local_journal`). Roles:
/// `journal` (intent and response lines), `plan_requests` (planned requests
/// with their exact prompt), `server` (model identity receipts, archived),
/// anything else archived only.
pub struct Journal {
    family: String,
    pending: HashMap<String, (At, Value)>,
    servers: Vec<Value>,
    /// plan item id -> (kind, parent id, parent text digest, cohort, source group, domain)
    items: HashMap<String, [Option<String>; 6]>,
}

impl Journal {
    pub fn new(p: &Value) -> Result<Journal> {
        Ok(Journal {
            family: p.get("family").and_then(|x| x.as_str()).unwrap_or("local").into(),
            pending: HashMap::new(),
            servers: Vec::new(),
            items: HashMap::new(),
        })
    }
}

fn kind_base(k: &str) -> Option<(&'static str, String)> {
    for (suffix, which) in [("intent", "intent"), ("response", "response")] {
        if k == suffix {
            return Some((which, "generate".into()));
        }
        if let Some(t) = k.strip_suffix(&format!("_{suffix}")) {
            return Some((which, t.to_string()));
        }
    }
    None
}

fn call_id(v: &Value) -> Option<String> {
    s(v, &["request_id", "call_id"])
}

/// Exact prompt of a llama-server request body.
fn prompt_of(body: &Value) -> Option<String> {
    if let Some(p) = body.get("prompt").and_then(|x| x.as_str()) {
        return Some(p.to_string());
    }
    let msgs = body.get("messages")?.as_array()?;
    if msgs.len() == 1 {
        return msgs[0].get("content").and_then(|x| x.as_str()).map(String::from);
    }
    Some(serde_json::to_string(msgs).ok()?)
}

fn response_text(raw: &str) -> Option<String> {
    let v: Value = serde_json::from_str(raw).ok()?;
    if let Some(c) = v.get("choices").and_then(|c| c.get(0)) {
        return c.get("message").and_then(|m| m.get("content")).and_then(|x| x.as_str()).map(String::from)
            .or_else(|| c.get("text").and_then(|x| x.as_str()).map(String::from));
    }
    v.get("content").and_then(|x| x.as_str()).map(String::from)
}

impl Journal {
    fn emit(&mut self, sink: &mut Sink, id: &str, task: &str, intent: Option<&(At, Value)>, resp: Option<(&At, &Value)>) -> Result<()> {
        let iv = intent.map(|x| &x.1);
        let body = iv.and_then(|v| v.get("body"));
        let prompt_sha = match body.and_then(prompt_of) {
            Some(p) => Some(sink.blob(p.as_bytes())?),
            None => None,
        };
        let params = body.map(|b| {
            let mut m = b.clone();
            if let Value::Object(o) = &mut m {
                o.remove("messages");
                o.remove("prompt");
            }
            m
        });
        let generator = iv.and_then(|v| s(v, &["generator", "inventor", "reviewer", "describer", "family"]))
            .or_else(|| resp.and_then(|(_, v)| s(v, &["generator"])));
        let attempt = iv.and_then(|v| v.get("attempt")).or_else(|| resp.and_then(|(_, v)| v.get("attempt"))).and_then(|x| x.as_i64());
        let (status, receipt, response_sha, extra_resp) = match resp {
            None => ("no_response".to_string(), None, None, Value::Null),
            Some((_, rv)) => {
                let raw = rv.get("raw_body").and_then(|x| x.as_str());
                let receipt = match raw {
                    Some(r) => Some(sink.blob(r.as_bytes())?),
                    None => None,
                };
                let rt = raw.and_then(response_text);
                let rsha = match &rt {
                    Some(t) => Some(sink.blob(t.as_bytes())?),
                    None => None,
                };
                let http = rv.get("status").and_then(|x| x.as_i64());
                let st = if let Some(vd) = rv.get("verdict") {
                    match vd.get("accepted").and_then(|x| x.as_bool()) {
                        Some(true) => "accepted".to_string(),
                        Some(false) if http == Some(0) => "transport_error".to_string(),
                        Some(false) => "rejected".to_string(),
                        None => "unknown".to_string(),
                    }
                } else {
                    match http {
                        Some(200) => "ok".to_string(),
                        Some(0) => "transport_error".to_string(),
                        Some(c) => format!("http_{c}"),
                        None => "unknown".to_string(),
                    }
                };
                let vtext = get(rv, "verdict.text").and_then(|x| x.as_str());
                let vsha = match vtext {
                    Some(t) => Some(sink.blob(t.as_bytes())?),
                    None => None,
                };
                let ex = json!({"http_status": http, "verdict_reasons": get(rv, "verdict.reasons"), "verdict_diag": get(rv, "verdict.diag"),
                    "verdict_text_sha256": vsha, "timings": get(rv, "timings"), "usage": get(rv, "usage"), "elapsed_ms": get(rv, "elapsed_ms"),
                    "error": get(rv, "error")});
                (st, receipt, rsha, ex)
            }
        };
        let at = resp.map(|x| x.0).or(intent.map(|x| &x.0));
        let t0 = iv.and_then(|v| s(v, &["t"])).unwrap_or_default();
        // An accepted whole-document output (mirror or full rewrite) is itself a corpus text.
        let item_id = iv.and_then(|v| s(v, &["item_id"])).or_else(|| resp.and_then(|(_, v)| s(v, &["item_id"])));
        let item = item_id.as_ref().and_then(|i| self.items.get(i)).cloned();
        let mut output_text = None;
        if status == "accepted" {
            if let (Some((rat, rv)), Some(it)) = (resp, &item) {
                if matches!(it[0].as_deref(), Some("mirror") | Some("full")) {
                    if let Some(t) = get(rv, "verdict.text").and_then(|x| x.as_str()) {
                        let h = sink.text(t)?;
                        sink.record(rat, &Record {
                            role: "local_output".into(),
                            record_id: item_id.clone(),
                            text_sha256: Some(h.clone()),
                            dataset: Some(self.family.clone()),
                            domain: it[5].clone(),
                            source_group: it[4].clone(),
                            cohort_id: it[3].clone(),
                            origin: Some(if it[0].as_deref() == Some("mirror") { "model_only".into() } else { "mixed".into() }),
                            share: Some(1.0),
                            generator_label: generator.clone(),
                            generator_key: generator.clone(),
                            operation: it[0].clone(),
                            recipe_sha256: prompt_sha.clone(),
                            parent_record_id: it[1].clone(),
                            parent_text_sha256: it[2].clone(),
                            admission: Some("journal_accepted_verdict; finalized record set decides admission".into()),
                            split_hint: iv.and_then(|v| s(v, &["split"])),
                            extra: Some(json!({"request_id": id, "attempt": attempt})),
                            ..Default::default()
                        })?;
                        output_text = Some(h);
                    }
                }
            }
        }
        sink.event(at, &Event {
            event_key: format!("local:{}:{}:{}:{}:{}", self.family, task, id, attempt.unwrap_or(-1), t0),
            kind: format!("local_{}", task),
            generator_label: generator.clone(),
            generator_key: generator,
            requested_model: body.and_then(|b| s(b, &["model"])),
            status,
            prompt_sha256: prompt_sha,
            params,
            receipt_sha256: receipt,
            response_sha256: response_sha,
            output_text_sha256: output_text,
            operation: item.as_ref().and_then(|it| it[0].clone()),
            request_id: Some(id.to_string()),
            attempt,
            started_utc: resp.and_then(|(_, v)| s(v, &["t_start"])).or(Some(t0)),
            finished_utc: resp.and_then(|(_, v)| s(v, &["t_end", "t"])),
            split_hint: iv.and_then(|v| s(v, &["split"])),
            extra: Some(json!({"family": self.family, "item_id": iv.and_then(|v| v.get("item_id")), "seed": iv.and_then(|v| v.get("seed")),
                "round": iv.and_then(|v| v.get("round")), "task": iv.and_then(|v| v.get("task")), "response": extra_resp,
                "intent_line": intent.map(|x| json!([x.0.path, x.0.line_no]))})),
            ..Default::default()
        })
    }
}

impl Adapter for Journal {
    fn needs_all_files(&self) -> bool {
        true
    }

    fn line(&mut self, sink: &mut Sink, input: &Input, l: &Line) -> Result<()> {
        let Some(v) = l.value else {
            if input.role == "journal" && !l.bytes.is_empty() {
                sink.problem("journal_truncated_line", Some(&l.at.path), Some(l.at.line_no), "unparseable journal line (kept in the archive)")?;
            }
            return Ok(());
        };
        match input.role.as_str() {
            "journal" => {
                let Some(k) = v.get("kind").and_then(|x| x.as_str()) else { return Ok(()) };
                let Some((which, task)) = kind_base(k) else { return Ok(()) };
                let Some(id) = call_id(v) else { return Ok(()) };
                let att = v.get("attempt").and_then(|x| x.as_i64()).unwrap_or(-1);
                let key = format!("{task}:{id}:{att}");
                if which == "intent" {
                    if let Some(prev) = self.pending.remove(&key) {
                        self.emit(sink, &id, &task, Some(&prev), None)?;
                    }
                    self.pending.insert(key, (l.at.clone(), v.clone()));
                } else {
                    let intent = self.pending.remove(&key);
                    self.emit(sink, &id, &task, intent.as_ref(), Some((&l.at, v)))?;
                }
            }
            "plan_requests" => {
                let Some(id) = s(v, &["request_id"]) else { return Ok(()) };
                let prompt_sha = match get(v, "prompt").and_then(|x| x.as_str()) {
                    Some(p) => Some(sink.blob(p.as_bytes())?),
                    None => None,
                };
                sink.event(Some(&l.at), &Event {
                    event_key: format!("local_plan:{}:{}:{}", self.family, input.rel, id),
                    kind: "local_planned_request".into(),
                    generator_label: s(v, &["generator"]),
                    generator_key: s(v, &["generator"]),
                    operation: s(v, &["op", "template_id"]),
                    status: "planned".into(),
                    prompt_sha256: prompt_sha,
                    params: Some(json!({"decoding": get(v, "decoding"), "seed": get(v, "seed"), "endpoint": get(v, "endpoint"),
                        "template_id": get(v, "template_id"), "target_words": get(v, "target_words"), "source_span": get(v, "source_span"),
                        "span_index": get(v, "span_index"), "cut_budget": get(v, "cut_budget")})),
                    request_id: Some(id),
                    split_hint: s(v, &["split"]),
                    extra: Some(json!({"item_id": get(v, "item_id"), "parent_id": get(v, "parent_id"), "cohort": get(v, "cohort"),
                        "request_sha256": get(v, "request_sha256")})),
                    ..Default::default()
                })?;
            }
            "server" => self.servers.push(strip_long(v, 160)),
            "plan_items" => {
                if let Some(id) = s(v, &["item_id"]) {
                    self.items.insert(id, [s(v, &["kind"]), s(v, &["parent_id"]), s(v, &["parent_text_sha256"]),
                        s(v, &["cohort"]), s(v, &["source_group"]), s(v, &["domain"])]);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn end_file(&mut self, sink: &mut Sink, _input: &Input) -> Result<()> {
        let pending: Vec<(String, (At, Value))> = self.pending.drain().collect();
        for (key, it) in pending {
            let mut p = key.splitn(3, ':');
            let task = p.next().unwrap_or("generate").to_string();
            let id = p.next().unwrap_or("").to_string();
            self.emit(sink, &id, &task, Some(&it), None)?;
        }
        Ok(())
    }
}

/// Finalized local positive record sets (`slop_ninja_a_comp_positive_v1`) and
/// their failure lists (`local_records`). Roles: `records`, `failures`.
pub struct Records {
    set: String,
}

impl Records {
    pub fn new(p: &Value) -> Result<Records> {
        Ok(Records { set: p.get("set").and_then(|x| x.as_str()).unwrap_or("local").into() })
    }
}

impl Adapter for Records {
    fn line(&mut self, sink: &mut Sink, input: &Input, l: &Line) -> Result<()> {
        let Some(v) = l.value else { return Ok(()) };
        match input.role.as_str() {
            "records" => {
                let Some(t) = get(v, "text").and_then(|x| x.as_str()) else { return Ok(()) };
                let h = sink.text(t)?;
                let reqs: Vec<Value> = get(v, "generation.requests")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().map(|r| json!({"request_id": r.get("request_id"), "request_sha256": r.get("request_sha256")})).collect())
                    .unwrap_or_default();
                sink.record(&l.at, &Record {
                    role: "local_positive".into(),
                    record_id: s(v, &["id"]),
                    text_sha256: Some(h),
                    dataset: Some(self.set.clone()),
                    collection: s(v, &["collection"]),
                    domain: s(v, &["domain"]),
                    source_group: s(v, &["source_group"]),
                    writer_key: s(v, &["writer_key"]),
                    licence: s(v, &["rights.license", "rights.source.license"]),
                    cohort_id: s(v, &["cohort"]),
                    origin: s(v, &["origin"]),
                    share: f(v, &["share_target"]),
                    ai_byte_spans: get(v, "ai_byte_spans").map(|x| x.to_string()),
                    generator_label: s(v, &["generation.generator.family", "generation.generator_key"]),
                    generator_key: s(v, &["generation.generator_key"]),
                    operation: s(v, &["kind"]),
                    recipe_sha256: s(v, &["generation.prompt_sha256.0"]),
                    parent_record_id: s(v, &["parent_id"]),
                    parent_text_sha256: s(v, &["parent_text_sha256"]),
                    split_hint: s(v, &["split", "frozen_split"]),
                    extra: Some(json!({"population": get(v, "population"), "share_bin": get(v, "share_bin"),
                        "operation": get(v, "generation.operation"), "template": get(v, "generation.prompt_template_id"),
                        "prompt_sha256": get(v, "generation.prompt_sha256"), "requests": reqs, "plan_sha256": get(v, "generation.plan_sha256"),
                        "decoding": get(v, "generation.decoding"), "style_ids": get(v, "generation.style").map(|x| strip_long(x, 160))})),
                    ..Default::default()
                })?;
            }
            "failures" => {
                let id = s(v, &["item_id"]).unwrap_or_default();
                sink.event(Some(&l.at), &Event {
                    event_key: format!("local_records:{}:{}:{}", self.set, input.rel, id),
                    kind: "local_item_outcome".into(),
                    generator_key: s(v, &["generator"]),
                    operation: s(v, &["kind", "operation"]),
                    status: s(v, &["status", "reason"]).unwrap_or_else(|| "failed".into()),
                    request_id: Some(id),
                    split_hint: s(v, &["split"]),
                    extra: Some(strip_long(v, 160)),
                    ..Default::default()
                })?;
            }
            _ => {}
        }
        Ok(())
    }
}
