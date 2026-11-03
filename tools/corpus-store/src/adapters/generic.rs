//! Field-mapped JSONL documents: splits, pools, evaluation humans, bundles,
//! published snapshots. One record per line; optional view registration.
//!
//! Parameters (all optional except `text` or `text_sha256`):
//! - `text`: paths of the document text; `text_sha256`: paths of a declared
//!   digest (checked against the text, or used alone for metadata-only rows);
//! - `id`: paths of the record id;
//! - `fields`: record column -> paths; `consts`: record column -> value
//!   (applied when the row has no value);
//! - `role`: record role (default `document`);
//! - `view`: `{ "experiment": E, "split": S | "split_paths": [..], "kind": K,
//!   "expected": "sha256sums" | "manifest" | null }` registers every input
//!   file as a frozen view with ordered membership;
//! - `extra`: keep the remaining fields (long strings replaced by digests);
//! - `exposure`: `{kind, basis, date}` recorded per text.

use super::util::{get, s, strip_long, without};
use super::Adapter;
use crate::archive::Line;
use crate::runner::Input;
use crate::sink::{Record, Sink};
use anyhow::{Result, ensure};
use rusqlite::params;
use serde_json::{Map, Value};

pub struct Generic {
    p: Value,
    text: Vec<String>,
    text_sha: Vec<String>,
    id: Vec<String>,
    fields: Vec<(String, Vec<String>)>,
    consts: Map<String, Value>,
    role: String,
    extra: bool,
    view: Option<Value>,
    cur_view: Option<(String, i64)>,
}

fn paths(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    }
}

pub const COLUMNS: [&str; 27] = [
    "dataset", "collection", "domain", "genre", "source_work", "family", "source_group", "writer_key", "original_doc_id",
    "doc_date", "composition_date", "licence", "rights_basis", "cohort_id", "origin", "share", "ai_byte_spans",
    "generator_label", "generator_key", "operation", "recipe_sha256", "parent_record_id", "parent_text_sha256",
    "admission", "split_hint", "record_id_alt", "text_kind",
];

impl Generic {
    pub fn new(p: &Value) -> Result<Generic> {
        let mut fields = Vec::new();
        if let Some(Value::Object(m)) = p.get("fields") {
            for (k, v) in m {
                ensure!(COLUMNS.contains(&k.as_str()), "unknown record column {k}");
                fields.push((k.clone(), paths(Some(v))));
            }
        }
        let consts = match p.get("consts") {
            Some(Value::Object(m)) => {
                for k in m.keys() {
                    ensure!(COLUMNS.contains(&k.as_str()), "unknown record column {k}");
                }
                m.clone()
            }
            _ => Map::new(),
        };
        let g = Generic {
            p: p.clone(),
            text: paths(p.get("text")),
            text_sha: paths(p.get("text_sha256")),
            id: paths(p.get("id")),
            fields,
            consts,
            role: p.get("role").and_then(|x| x.as_str()).unwrap_or("document").into(),
            extra: p.get("extra").and_then(|x| x.as_bool()).unwrap_or(false),
            view: p.get("view").cloned(),
            cur_view: None,
        };
        ensure!(!g.text.is_empty() || !g.text_sha.is_empty(), "generic adapter needs text or text_sha256 paths");
        Ok(g)
    }
}

fn set(r: &mut Record, col: &str, v: &Value) {
    let st = match v {
        Value::String(t) => t.clone(),
        x => x.to_string(),
    };
    match col {
        "dataset" => r.dataset = Some(st),
        "collection" => r.collection = Some(st),
        "domain" => r.domain = Some(st),
        "genre" => r.genre = Some(st),
        "source_work" => r.source_work = Some(st),
        "family" => r.family = Some(st),
        "source_group" => r.source_group = Some(st),
        "writer_key" => r.writer_key = Some(st),
        "original_doc_id" => r.original_doc_id = Some(st),
        "doc_date" => r.doc_date = Some(st),
        "composition_date" => r.composition_date = Some(st),
        "licence" => r.licence = Some(st),
        "rights_basis" => r.rights_basis = Some(st),
        "cohort_id" => r.cohort_id = Some(st),
        "origin" => r.origin = Some(st),
        "share" => r.share = v.as_f64().or_else(|| st.parse().ok()),
        "ai_byte_spans" => r.ai_byte_spans = Some(v.to_string()),
        "generator_label" => r.generator_label = Some(st),
        "generator_key" => r.generator_key = Some(st),
        "operation" => r.operation = Some(st),
        "recipe_sha256" => r.recipe_sha256 = Some(st),
        "parent_record_id" => r.parent_record_id = Some(st),
        "parent_text_sha256" => r.parent_text_sha256 = Some(st),
        "admission" => r.admission = Some(st),
        "split_hint" => r.split_hint = Some(st),
        _ => {}
    }
}

impl Adapter for Generic {
    fn line(&mut self, sink: &mut Sink, input: &Input, l: &Line) -> Result<()> {
        if input.role == "archive" {
            return Ok(());
        }
        // A view per input file; `view.roles` limits which input roles are views.
        if let Some(view) = &self.view {
            let roles = paths(view.get("roles"));
            if roles.is_empty() || roles.contains(&input.role) {
                let vid = format!("{}:{}", sink.source_id, input.rel);
                if self.cur_view.as_ref().map(|x| &x.0) != Some(&vid) {
                    sink.db.execute(
                        "INSERT OR IGNORE INTO view(view_id, kind, path, file_sha256, n_rows, expected_sha256, expected_basis, source_id)
                         VALUES (?1, ?2, ?3, ?4, 0, NULL, NULL, ?5)",
                        params![vid, view.get("kind").and_then(|x| x.as_str()).unwrap_or("view"), l.at.path, input.sha, sink.source_id],
                    )?;
                    self.cur_view = Some((vid.clone(), 0));
                }
            } else {
                self.cur_view = None;
            }
        }
        let Some(v) = l.value else {
            if l.bytes.is_empty() {
                return Ok(());
            }
            return Ok(());
        };
        let text = self.text.iter().find_map(|p| get(v, p).and_then(|x| x.as_str()));
        let declared = s(v, &self.text_sha.iter().map(String::as_str).collect::<Vec<_>>());
        let text_sha = match text {
            Some(t) => {
                let h = sink.text(t)?;
                if let Some(d) = &declared {
                    if d != &h {
                        sink.problem("text_sha_mismatch", Some(&l.at.path), Some(l.at.line_no), &format!("declared {d} computed {h}"))?;
                    }
                }
                Some(h)
            }
            None => declared.clone(),
        };
        let mut r = Record { role: self.role.clone(), ..Default::default() };
        r.record_id = s(v, &self.id.iter().map(String::as_str).collect::<Vec<_>>());
        r.text_sha256 = text_sha.clone();
        for (col, ps) in &self.fields {
            if let Some(x) = ps.iter().find_map(|p| get(v, p)) {
                set(&mut r, col, x);
            }
        }
        for (col, x) in &self.consts {
            let empty = match col.as_str() {
                "dataset" => r.dataset.is_none(),
                "collection" => r.collection.is_none(),
                "domain" => r.domain.is_none(),
                "licence" => r.licence.is_none(),
                "rights_basis" => r.rights_basis.is_none(),
                "cohort_id" => r.cohort_id.is_none(),
                "origin" => r.origin.is_none(),
                "operation" => r.operation.is_none(),
                "admission" => r.admission.is_none(),
                "split_hint" => r.split_hint.is_none(),
                _ => true,
            };
            if empty {
                set(&mut r, col, x);
            }
        }
        if self.extra {
            let mut skip: Vec<&str> = self.text.iter().map(String::as_str).collect();
            skip.extend(self.id.iter().map(String::as_str));
            r.extra = Some(strip_long(&without(v, &skip), 160));
        }
        sink.record(&l.at, &r)?;
        // Optional production event per row (generation records that carry their request).
        if let Some(ev) = self.p.get("event").cloned() {
            let ps = |k: &str| paths(ev.get(k));
            let gs = |k: &str| s(v, &ps(k).iter().map(String::as_str).collect::<Vec<_>>());
            let prompt = ps("prompt").iter().find_map(|p| get(v, p)).cloned();
            let prompt_sha = match prompt {
                Some(Value::String(t)) => Some(sink.blob(t.as_bytes())?),
                Some(x) => Some(sink.blob(serde_json::to_string(&x)?.as_bytes())?),
                None => None,
            };
            let mut params = serde_json::Map::new();
            for p in ps("params") {
                if let Some(x) = get(v, &p) {
                    params.insert(p.clone(), strip_long(x, 160));
                }
            }
            let id = gs("request_id").or(r.record_id.clone()).unwrap_or_else(|| l.at.line_sha256.clone());
            let response_sha = match ps("response_text").iter().find_map(|p| get(v, p)).and_then(|x| x.as_str()) {
                Some(t) => Some(sink.blob(t.as_bytes())?),
                None => gs("response"),
            };
            sink.event(Some(&l.at), &crate::sink::Event {
                event_key: if ev.get("key_with_line").and_then(|x| x.as_bool()) == Some(true) {
                    format!("{}:{}:{}:{}", ev.get("kind").and_then(|x| x.as_str()).unwrap_or("generation"), sink.source_id, id, l.at.line_sha256)
                } else {
                    format!("{}:{}:{}", ev.get("kind").and_then(|x| x.as_str()).unwrap_or("generation"), sink.source_id, id)
                },
                kind: ev.get("kind").and_then(|x| x.as_str()).unwrap_or("generation").into(),
                generator_label: gs("generator").or(r.generator_label.clone()),
                generator_key: gs("generator").or(r.generator_key.clone()),
                operation: gs("operation").or(r.operation.clone()),
                status: gs("status").or(ev.get("status_const").and_then(|x| x.as_str()).map(String::from)).unwrap_or_else(|| "recorded".into()),
                prompt_sha256: prompt_sha,
                params: Some(Value::Object(params)),
                receipt_sha256: gs("receipt"),
                response_sha256: response_sha,
                request_id: Some(id),
                started_utc: gs("started"),
                output_text_sha256: text_sha.clone(),
                input_text_sha256: r.parent_text_sha256.clone(),
                split_hint: r.split_hint.clone(),
                ..Default::default()
            })?;
        }
        if let Some(e) = self.p.get("exposure") {
            if let Some(h) = &text_sha {
                let kind = e.get("kind").and_then(|x| x.as_str()).unwrap_or("exposed");
                let basis = e.get("basis").and_then(|x| x.as_str()).unwrap_or("");
                sink.exposure(&format!("{}:{}:{}", sink.source_id, kind, h), Some(h), r.source_group.as_deref(), kind, basis, e.get("date").and_then(|x| x.as_str()))?;
            }
        }
        if let (Some(view), Some((vid, ord))) = (&self.view, self.cur_view.as_mut()) {
            let vid = vid.clone();
            let split = match view.get("split").and_then(|x| x.as_str()) {
                Some(sp) => Some(sp.to_string()),
                None => s(v, &paths(view.get("split_paths")).iter().map(String::as_str).collect::<Vec<_>>()),
            };
            let split = split.map(|x| super::util::split_role(&x)).map(|x| {
                view.get("role_map").and_then(|m| m.get(&x)).and_then(|y| y.as_str()).map(String::from).unwrap_or(x)
            });
            sink.db
                .prepare_cached("INSERT OR IGNORE INTO view_member(view_id, ordinal, record_id, text_sha256, line_sha256, split, source_group) VALUES (?1,?2,?3,?4,?5,?6,?7)")?
                .execute(params![vid, *ord, r.record_id, text_sha, l.at.line_sha256, split, r.source_group])?;
            if let (Some(exp), Some(sp), Some(id)) = (view.get("experiment").and_then(|x| x.as_str()), split.as_deref(), r.record_id.as_deref()) {
                sink.split_member(exp, id, text_sha.as_deref(), r.source_group.as_deref(), sp, &vid)?;
            }
            *ord += 1;
            sink.db.prepare_cached("UPDATE view SET n_rows = ?2 WHERE view_id = ?1")?.execute(params![vid, *ord])?;
        }
        Ok(())
    }
}
