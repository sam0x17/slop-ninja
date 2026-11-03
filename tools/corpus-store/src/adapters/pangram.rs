//! Pangram observations (comparator evidence only; never labels).
//!
//! One observation per paid result, keyed like the Pangram gold inventory:
//! `bulk:<bulk id>:<item index>` for bulk jobs (result pages, annotation
//! lines and per-item result files of one job collapse to one row) and
//! `task:<task id>` for realtime calls. Identical texts scored by different
//! calls stay separate rows.
//!
//! Roles:
//! - `bulk_request` (`{model, items:[{id,text}]}`) and `submission`
//!   (`submitted_text`): register the exact submitted texts;
//! - `observations`: any JSON or JSONL file; bulk result pages
//!   (`{bulk_id, items:[{id,index,result,...}]}`), annotation lines
//!   (`{bulk_id, observation:{index,result,...}, submitted_text_sha256}`) and
//!   per-item result files (`result.json` with `task_id` and/or
//!   `bulk_id`+`bulk_index`, fractions inline or under `response`) are
//!   recognised; anything else is archived only.

use super::util::{f, get, s, without};
use super::Adapter;
use crate::archive::Line;
use crate::runner::Input;
use crate::sink::{Observation, Sink};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashMap;

pub struct Pangram {
    store: String,
    /// Split context for observations whose files do not name one.
    split_default: Option<String>,
    /// submitted item id -> text digest
    item_text: HashMap<String, String>,
    /// realtime entry directory -> (task id, submitted text digest)
    entry: HashMap<String, (Option<String>, String)>,
    model: Option<String>,
}

fn dir_of(path: &str) -> String {
    path.rsplit_once('/').map(|x| x.0.to_string()).unwrap_or_default()
}

impl Pangram {
    pub fn new(p: &Value) -> Result<Pangram> {
        Ok(Pangram {
            store: p.get("store").and_then(|x| x.as_str()).unwrap_or("").into(),
            split_default: p.get("split_default").and_then(|x| x.as_str()).map(String::from),
            item_text: HashMap::new(),
            entry: HashMap::new(),
            model: None,
        })
    }
}

fn pred(r: &Value) -> Value {
    let mut p = without(r, &["text", "windows", "fraction_ai", "fraction_ai_assisted", "fraction_human"]);
    if let Some(Value::Array(w)) = r.get("windows") {
        p["windows"] = Value::Array(w.iter().map(|x| without(x, &["text"])).collect());
    }
    p
}

impl Pangram {
    #[allow(clippy::too_many_arguments)]
    fn emit(&mut self, sink: &mut Sink, l: &Line, key: String, r: &Value, text: Option<String>, item: Option<String>, job: Option<String>,
            date: Option<String>, ctx: Value, failed: bool) -> Result<()> {
        if text.is_none() {
            sink.problem("pangram_observation_without_text", Some(&l.at.path), Some(l.at.line_no), &key)?;
        }
        let mut p = pred(r);
        let mut ctx = ctx;
        if ctx.get("split").is_none_or(|x| x.is_null()) {
            if let Some(d) = &self.split_default {
                ctx["split"] = json!(d);
            }
        }
        p["context"] = ctx;
        p["role"] = json!("comparator_evidence_only");
        sink.observation(Some(&l.at), &Observation {
            obs_key: key,
            detector: "pangram".into(),
            detector_version: s(r, &["version"]).or(self.model.clone()),
            text_sha256: text,
            item_id: item,
            job_ref: job,
            observed_utc: date,
            fraction_ai: f(r, &["fraction_ai"]),
            fraction_ai_assisted: f(r, &["fraction_ai_assisted"]),
            fraction_human: f(r, &["fraction_human"]),
            score: f(r, &["fraction_ai"]),
            prediction: Some(p),
            cost_units: None,
            status: if failed { "failed".into() } else { "success".into() },
            context: Some(self.store.clone()),
        })
    }
}

impl Adapter for Pangram {
    fn needs_all_files(&self) -> bool {
        true
    }

    fn line(&mut self, sink: &mut Sink, input: &Input, l: &Line) -> Result<()> {
        if input.role == "candidate_text" {
            // A raw text file that may be the exact bytes of a submission.
            if let Ok(t) = std::str::from_utf8(l.bytes) {
                let h = sink.text(t)?;
                sink.record(&l.at, &crate::sink::Record {
                    role: "pangram_submission_file".into(),
                    record_id: Some(input.rel.clone()),
                    text_sha256: Some(h),
                    dataset: Some(self.store.clone()),
                    ..Default::default()
                })?;
            }
            return Ok(());
        }
        let Some(v) = l.value else { return Ok(()) };
        match input.role.as_str() {
            "bulk_request" => {
                if self.model.is_none() {
                    self.model = s(v, &["model"]);
                }
                for it in v.get("items").and_then(|x| x.as_array()).into_iter().flatten() {
                    if let (Some(id), Some(t)) = (s(it, &["id"]), it.get("text").and_then(|x| x.as_str())) {
                        let h = sink.text(t)?;
                        self.item_text.insert(id, h);
                    }
                }
            }
            "submission" => {
                if let Some(t) = v.get("submitted_text").and_then(|x| x.as_str()) {
                    let h = sink.text(t)?;
                    self.entry.insert(dir_of(&l.at.path), (s(v, &["task_id"]), h.clone()));
                    if let Some(d) = s(v, &["sha256"]) {
                        if d != h {
                            sink.problem("pangram_submission_digest", Some(&l.at.path), None, &format!("declared {d} computed {h}"))?;
                        }
                    }
                }
            }
            "observations" => {
                // Bulk result page.
                if let (Some(bulk), Some(items)) = (s(v, &["bulk_id"]), v.get("items").and_then(|x| x.as_array())) {
                    for it in items {
                        let Some(r) = it.get("result").filter(|r| !r.is_null()) else {
                            continue;
                        };
                        let idx = s(it, &["index"]).unwrap_or_default();
                        let id = s(it, &["id"]);
                        let text = id.as_ref().and_then(|i| self.item_text.get(i).cloned());
                        let ctx = json!({"stage": get(it, "stage"), "task_id": get(it, "task_id"), "error": get(it, "error")});
                        self.emit(sink, l, format!("bulk:{bulk}:{idx}"), r, text, id, Some(bulk.clone()), None, ctx, false)?;
                    }
                    return Ok(());
                }
                // Annotation line.
                if let (Some(bulk), Some(o)) = (s(v, &["bulk_id"]), v.get("observation")) {
                    let Some(r) = o.get("result").filter(|r| !r.is_null()) else { return Ok(()) };
                    let idx = s(o, &["index"]).unwrap_or_default();
                    let text = s(v, &["submitted_text_sha256", "member.text_sha256"]);
                    let ctx = json!({"member": get(v, "member"), "echo_match": get(v, "echo_match"),
                        "returned_text_sha256": get(v, "returned_text_sha256"), "model_selector": get(v, "model_selector"),
                        "task_id": get(o, "task_id"), "stage": get(o, "stage")});
                    self.emit(sink, l, format!("bulk:{bulk}:{idx}"), r, text, s(o, &["id"]), Some(bulk.clone()), s(v, &["collected_at"]), ctx, false)?;
                    return Ok(());
                }
                // Per-item result file (`response` or `result` object, or inline fractions).
                let r = match v.get("response").filter(|r| r.is_object())
                    .or_else(|| v.get("result").filter(|r| r.get("fraction_ai").is_some())) {
                    Some(r) => r.clone(),
                    None if v.get("fraction_ai").is_some() => v.clone(),
                    None => return Ok(()),
                };
                // A realtime record that carries its own submitted bytes.
                let inline_text = match v.get("submitted_text").and_then(|x| x.as_str()) {
                    Some(t) => Some(sink.text(t)?),
                    None => None,
                };
                let ent = self.entry.get(&dir_of(&l.at.path)).cloned();
                let task = s(v, &["task_id"]).or_else(|| ent.as_ref().and_then(|e| e.0.clone()));
                let key = match (s(v, &["bulk_id"]), s(v, &["bulk_index"]), task.clone()) {
                    (Some(b), Some(i), _) => format!("bulk:{b}:{i}"),
                    (_, _, Some(t)) => format!("task:{t}"),
                    _ => {
                        sink.problem("pangram_result_without_key", Some(&l.at.path), None, "no task id or bulk index")?;
                        return Ok(());
                    }
                };
                if let (Some(d), Some(e)) = (s(v, &["text_sha256"]), ent.as_ref()) {
                    if d != e.1 {
                        sink.problem("pangram_result_text_differs_from_submission", Some(&l.at.path), None, &format!("declared {d} submitted {}", e.1))?;
                    }
                }
                let text = inline_text.clone()
                    .or_else(|| ent.as_ref().map(|e| e.1.clone()))
                    .or_else(|| s(v, &["text_sha256", "sha256"]))
                    .or_else(|| s(v, &["id"]).and_then(|i| self.item_text.get(&i).cloned()));
                let ctx = json!({"split": get(v, "split"), "source_group": get(v, "source_group"), "task_id": task,
                    "submitted_text_sha256": ent.as_ref().map(|e| e.1.clone()),
                    "input_sha256": get(v, "input_sha256"), "returned_text_sha256": get(v, "returned_text_sha256"),
                    "echo_match": get(v, "echo_match"), "is_training_label": get(v, "is_training_label"), "schema": get(v, "schema"),
                    "elapsed_ms": get(v, "elapsed_ms"), "model": get(v, "model")});
                let job = s(v, &["bulk_id"]).or(task);
                self.emit(sink, l, key, &r, text, s(v, &["id", "name"]), job, s(v, &["collected_at", "collected_utc", "submitted_at"]), ctx, false)?;
            }
            _ => {}
        }
        Ok(())
    }
}
