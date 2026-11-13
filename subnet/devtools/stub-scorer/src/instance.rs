//! One stub scorer instance: the real scorer's single-use load protocol
//! (`inference/a-container-serve`, `a_scorer.rs`) with no model behind it.
//!
//! - `GET /healthz`: the slot (`empty`, `receiving`, `loading`, `ready`,
//!   `failed`) with its binding, `single_use: true` and a `build` block
//!   (stub build digests, one session and two threads, the specs a real
//!   scorer admits loads by, its limits and `score_fields`).
//! - `POST /v2/prepare?package=&cohort=&spec=`: the files manifest v2, whose
//!   SHA-256 must be the package digest; answers the missing chunks.
//! - `PUT /v2/files/<index>/<offset>`: one chunk of exactly its length
//!   (bytes are counted, never kept).
//! - `POST /v2/load?...`: starts the load once every chunk arrived; the slot
//!   reads `ready` after the `load_ms` switch (default at once), or `failed`
//!   with the `load_error` switch.
//! - `POST /v1/score`: the request as the real scorer reads it (unknown
//!   fields refused), answered per document by [`crate::result`].
//!
//! Control routes (dev only): `GET /__stub/state`, `POST /__stub/switch`
//! (a JSON object merged into [`Switches`]), `POST /__stub/destroy` (the
//! slot back to `empty`, as a destroyed container) and `POST /__stub/reset`
//! (the slot and every switch).

use crate::markers::{self, ReplyFault};
use crate::result::{self, Ask, sha};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use slop_ninja_artifact_check::files_manifest::{FilesManifest, MAX_MANIFEST_BYTES};
use slop_ninja_artifact_check::spec::{PART_BYTES, Version};
use slop_ninja_dev_http::{Call, Reply};

/// The real scorer's bounds (`limits.rs`).
pub const MAX_SCORE_BODY: usize = 8 << 20;
pub const MAX_DOCS: usize = 128;

/// What an instance is bound to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub package: String,
    pub cohort: Option<String>,
    /// The spec id the load admits by.
    pub spec: String,
}

impl Binding {
    fn serves(&self, package: &str, cohort: Option<&str>) -> bool {
        self.package == package && self.cohort.as_deref() == cohort
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Slot {
    Empty,
    Receiving { bound: Binding, files: Vec<u64>, written: Vec<(usize, u64)>, since_ms: u64 },
    Loading { bound: Binding, since_ms: u64, ready_at_ms: u64 },
    Ready { bound: Binding, load_ms: u64 },
    Failed { bound: Binding, error: String },
}

impl Slot {
    fn bound(&self) -> Option<&Binding> {
        match self {
            Slot::Empty => None,
            Slot::Receiving { bound, .. } | Slot::Loading { bound, .. } | Slot::Ready { bound, .. } | Slot::Failed { bound, .. } => {
                Some(bound)
            }
        }
    }
}

/// Instance behaviour the dev session can switch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Switches {
    /// How long a load takes.
    pub load_ms: u64,
    /// A load ends `failed` with this error (a refusal reads `refused
    /// <code>: <detail>`).
    pub load_error: Option<String>,
    /// `/healthz` answers this status with no body (a hung or starting
    /// instance: 503).
    pub healthz_status: Option<u16>,
    /// The build identity `/healthz` reports.
    pub binary_sha256: String,
    pub ort_sha256: String,
    pub sessions: u64,
    pub threads: u64,
    /// Whether the loaded package has a token head (spans emitted).
    pub token_head: bool,
    /// Score any package and cohort without a load (direct suites).
    pub bind_any: bool,
}

impl Default for Switches {
    fn default() -> Self {
        Switches {
            load_ms: 0,
            load_error: None,
            healthz_status: None,
            binary_sha256: sha(b"slop_ninja_dev_stub_scorer/binary"),
            ort_sha256: sha(b"slop_ninja_dev_stub_scorer/ort"),
            sessions: 1,
            threads: 2,
            token_head: true,
            bind_any: false,
        }
    }
}

/// The specs a real scorer admits loads by (spec v2 to v4 and any later
/// spec bounding the JSON role files): `(id, sha256)`.
pub fn specs() -> Vec<(&'static str, String)> {
    Version::ALL
        .into_iter()
        .filter(|v| v.max_json_bytes().is_some() && v.max_json_value_bytes().is_some())
        .map(|v| (v.id(), v.spec_sha256()))
        .collect()
}

/// The spec a load names none of.
pub const UNNAMED_LOAD_SPEC: Version = Version::V2;

/// The instance and its switches; serialized whole between requests.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    pub slot: Slot,
    pub switches: Switches,
    pub started_ms: u64,
    pub scored: u64,
}

fn valid_cohort(c: &str) -> bool {
    !c.is_empty() && c.len() <= 96 && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// A load query: `package`, optional `cohort` and `spec` (a digest), each once.
fn load_query(call: &Call) -> Result<Binding, &'static str> {
    let (mut package, mut cohort, mut spec) = (None, None, None);
    for (k, v) in &call.query {
        match k.as_str() {
            "package" if package.is_none() => package = Some(v.clone()),
            "cohort" if cohort.is_none() && valid_cohort(v) => cohort = Some(v.clone()),
            "spec" if spec.is_none() => {
                spec = Some(specs().into_iter().find(|(_, d)| d == v).map(|(id, _)| id.to_owned()).ok_or("spec")?)
            }
            _ => return Err("query"),
        }
    }
    Ok(Binding { package: package.ok_or("query")?, cohort, spec: spec.unwrap_or_else(|| UNNAMED_LOAD_SPEC.id().to_owned()) })
}

fn chunk_offsets(bytes: u64) -> impl Iterator<Item = u64> {
    (0..bytes.div_ceil(PART_BYTES)).map(|i| i * PART_BYTES)
}

/// The score request, as the real scorer reads it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScoreRequest {
    package: String,
    #[serde(default)]
    cohort: Option<String>,
    #[serde(default)]
    spans: bool,
    #[serde(default = "yes")]
    sentences: bool,
    docs: Vec<Doc>,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Doc {
    id: String,
    text: String,
}

impl Instance {
    pub fn new(now_ms: u64) -> Instance {
        Instance { slot: Slot::Empty, switches: Switches::default(), started_ms: now_ms, scored: 0 }
    }

    /// Moves a finished load to `ready` or `failed`.
    fn settle(&mut self, now_ms: u64) {
        if let Slot::Loading { bound, since_ms, ready_at_ms } = &self.slot
            && now_ms >= *ready_at_ms
        {
            self.slot = match &self.switches.load_error {
                Some(e) => Slot::Failed { bound: bound.clone(), error: e.clone() },
                None => Slot::Ready { bound: bound.clone(), load_ms: ready_at_ms.saturating_sub(*since_ms) },
            };
        }
    }

    fn missing(&self) -> Vec<(usize, u64)> {
        match &self.slot {
            Slot::Receiving { files, written, .. } => files
                .iter()
                .enumerate()
                .flat_map(|(i, b)| chunk_offsets(*b).map(move |o| (i, o)))
                .filter(|c| !written.contains(c))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn build(&self) -> Value {
        let s = &self.switches;
        let specs: Vec<Value> = specs().into_iter().map(|(id, d)| json!({"id": id, "sha256": d})).collect();
        json!({
            "binary_sha256": s.binary_sha256, "ort_sha256": s.ort_sha256,
            "sessions": s.sessions, "threads": s.threads,
            "limits": {"specs": specs, "unnamed_load_spec": UNNAMED_LOAD_SPEC.id(), "tokens_max": result::MAX_TOKENS,
                "score_body_bytes_max": MAX_SCORE_BODY, "docs_max": MAX_DOCS, "manifest_bytes_max": MAX_MANIFEST_BYTES,
                "range_bytes_max": PART_BYTES},
            "score_fields": ["spans", "sentences"],
            "stub": "slop_ninja_dev_stub_scorer",
        })
    }

    /// The `/healthz` body.
    pub fn slot_json(&self, now_ms: u64) -> Value {
        let mut v = match &self.slot {
            Slot::Empty => json!({"state": "empty"}),
            Slot::Receiving { bound, since_ms, .. } => json!({"state": "receiving", "package": bound.package,
                "cohort": bound.cohort, "spec": bound.spec, "receiving_ms": now_ms.saturating_sub(*since_ms)}),
            Slot::Loading { bound, since_ms, .. } => json!({"state": "loading", "package": bound.package,
                "cohort": bound.cohort, "spec": bound.spec, "loading_ms": now_ms.saturating_sub(*since_ms)}),
            Slot::Ready { bound, load_ms } => json!({"state": "ready", "package": bound.package,
                "cohort": bound.cohort, "spec": bound.spec, "load_ms": load_ms, "load": {"stub": true},
                "spans": self.switches.token_head}),
            Slot::Failed { bound, error } => json!({"state": "failed", "package": bound.package,
                "cohort": bound.cohort, "spec": bound.spec, "error": error}),
        };
        v["single_use"] = json!(true);
        v["uptime_ms"] = json!(now_ms.saturating_sub(self.started_ms));
        v["scored"] = json!(self.scored);
        v["build"] = self.build();
        v
    }

    /// Every route but the chunk upload; `oversize`: the body passed the
    /// adapter's bound and was dropped.
    pub fn handle(&mut self, call: &Call, now_ms: u64, oversize: bool) -> Reply {
        self.settle(now_ms);
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/healthz") => match self.switches.healthz_status {
                Some(s) => Reply::new(s),
                None => Reply::json(200, &self.slot_json(now_ms)),
            },
            ("POST", "/v2/prepare") => self.prepare(call, now_ms),
            ("POST", "/v2/load") => self.load(call, now_ms),
            ("POST", "/v1/score") if oversize || call.body.len() > MAX_SCORE_BODY => Reply::json(413, &json!({"error": "body"})),
            ("POST", "/v1/score") => self.score(&call.body),
            ("GET", "/__stub/state") => Reply::json(200, &json!({"slot": self.slot_json(now_ms), "switches": self.switches})),
            ("POST", "/__stub/switch") => self.switch(call),
            ("POST", "/__stub/destroy") => {
                *self = Instance { slot: Slot::Empty, switches: self.switches.clone(), started_ms: now_ms, scored: 0 };
                Reply::json(200, &self.slot_json(now_ms))
            }
            ("POST", "/__stub/reset") => {
                *self = Instance::new(now_ms);
                Reply::json(200, &self.slot_json(now_ms))
            }
            _ => Reply::json(404, &json!({"error": "not found"})),
        }
    }

    fn switch(&mut self, call: &Call) -> Reply {
        let Some(Value::Object(patch)) = call.json_body() else { return Reply::json(400, &json!({"error": "a JSON object"})) };
        let mut cur = serde_json::to_value(&self.switches).unwrap_or(Value::Null);
        for (k, v) in patch {
            cur[k] = v;
        }
        match serde_json::from_value::<Switches>(cur) {
            Ok(s) => {
                self.switches = s;
                Reply::json(200, &self.switches)
            }
            Err(e) => Reply::json(400, &json!({"error": e.to_string()})),
        }
    }

    fn prepare(&mut self, call: &Call, now_ms: u64) -> Reply {
        let want = match load_query(call) {
            Ok(w) => w,
            Err(code) => return Reply::json(400, &json!({"error": code})),
        };
        if self.slot.bound().is_some_and(|b| *b != want) {
            return Reply::json(409, &json!({"error": "instance_bound"}));
        }
        if call.body.len() > MAX_MANIFEST_BYTES || sha(&call.body) != want.package {
            return Reply::json(422, &json!({"error": "refused object: the files manifest's digest is not the package digest"}));
        }
        let m: FilesManifest = match serde_json::from_slice(&call.body) {
            Ok(m) => m,
            Err(e) => return Reply::json(422, &json!({"error": format!("refused schema: files manifest: {e}")})),
        };
        if m.files.is_empty() || m.files.iter().any(|f| f.bytes == 0) {
            return Reply::json(422, &json!({"error": "refused file_size: an empty file"}));
        }
        if self.slot.bound().is_some() {
            let mut v = self.slot_json(now_ms);
            v["missing"] = json!(self.missing());
            return Reply::json(200, &v);
        }
        self.slot = Slot::Receiving { bound: want, files: m.files.iter().map(|f| f.bytes).collect(), written: Vec::new(), since_ms: now_ms };
        let mut v = self.slot_json(now_ms);
        v["missing"] = json!(self.missing());
        Reply::json(201, &v)
    }

    /// `PUT /v2/files/<index>/<offset>` with a body of `len` bytes.
    pub fn chunk(&mut self, path: &str, len: u64, now_ms: u64) -> Reply {
        self.settle(now_ms);
        let target = path.strip_prefix("/v2/files/").and_then(|p| p.split_once('/')).and_then(|(i, o)| Some((i.parse::<usize>().ok()?, o.parse::<u64>().ok()?)));
        let Some((index, offset)) = target else { return Reply::json(400, &json!({"error": "chunk path"})) };
        let Slot::Receiving { files, written, .. } = &mut self.slot else {
            let bound = self.slot.bound().is_some();
            let error = if bound { Value::Null } else { json!("not_prepared") };
            return Reply::json(if bound { 200 } else { 409 }, &json!({"written": 0, "bound": bound, "error": error}));
        };
        let Some(bytes) = files.get(index).copied() else { return Reply::json(422, &json!({"error": "no such chunk"})) };
        if offset % PART_BYTES != 0 || offset >= bytes {
            return Reply::json(422, &json!({"error": "no such chunk"}));
        }
        let want = (bytes - offset).min(PART_BYTES);
        if len != want {
            return Reply::json(422, &json!({"error": format!("chunk length {len}, expected {want}")}));
        }
        if !written.contains(&(index, offset)) {
            written.push((index, offset));
        }
        Reply::json(201, &json!({"written": len, "ms": 0.0}))
    }

    fn load(&mut self, call: &Call, now_ms: u64) -> Reply {
        let want = match load_query(call) {
            Ok(w) => w,
            Err(code) => return Reply::json(400, &json!({"error": code})),
        };
        match self.slot.bound() {
            None => return Reply::json(409, &json!({"error": "not_prepared"})),
            Some(b) if *b != want => return Reply::json(409, &json!({"error": "instance_bound"})),
            _ => {}
        }
        if !matches!(self.slot, Slot::Receiving { .. }) {
            return Reply::json(200, &self.slot_json(now_ms));
        }
        let missing = self.missing();
        if !missing.is_empty() {
            return Reply::json(409, &json!({"error": "missing_chunks", "missing": missing}));
        }
        self.slot = Slot::Loading { bound: want, since_ms: now_ms, ready_at_ms: now_ms.saturating_add(self.switches.load_ms) };
        let reply = Reply::json(202, &self.slot_json(now_ms));
        self.settle(now_ms);
        reply
    }

    fn score(&mut self, body: &[u8]) -> Reply {
        let r: ScoreRequest = match serde_json::from_slice(body) {
            Ok(r) => r,
            Err(e) => return Reply::json(400, &json!({"error": format!("score request JSON: {e}")})),
        };
        if r.docs.is_empty() || r.docs.len() > MAX_DOCS {
            return Reply::json(400, &json!({"error": format!("1 to {MAX_DOCS} documents per batch")}));
        }
        if r.docs.iter().any(|d| d.id.is_empty() || d.id.len() > 128) {
            return Reply::json(400, &json!({"error": "document id length"}));
        }
        if !self.switches.bind_any {
            match &self.slot {
                Slot::Ready { bound, .. } if bound.serves(&r.package, r.cohort.as_deref()) => {}
                slot if slot.bound().is_some_and(|b| !b.serves(&r.package, r.cohort.as_deref())) => {
                    return Reply::json(409, &json!({"error": "instance_bound"}));
                }
                _ => return Reply::json(409, &json!({"error": "package_not_ready", "slot": Value::Null})),
            }
        }
        // Reply-level markers: the first document that names each.
        let all: Vec<markers::Directives> = r.docs.iter().map(|d| markers::parse(&d.text)).collect();
        let delay = all.iter().filter_map(|d| d.delay_ms).max().unwrap_or(0);
        if let Some(status) = all.iter().find_map(|d| d.http) {
            return Reply::json(status, &json!({"error": "stub"})).with_delay(delay);
        }
        let ask = Ask { spans: r.spans, sentences: r.sentences, token_head: self.switches.token_head };
        let mut results: Vec<Value> = r.docs.iter().map(|d| result::document(&d.id, &d.text, ask)).collect();
        match all.iter().find_map(|d| d.reply) {
            Some(ReplyFault::Garbage) => return Reply::text(200, "this is not JSON").with_delay(delay),
            Some(ReplyFault::Two) => results.extend(results.clone()),
            Some(ReplyFault::None) => results.clear(),
            None => {}
        }
        self.scored += r.docs.len() as u64;
        let package = if all.iter().any(|d| d.package_other) { sha(r.package.as_bytes()) } else { r.package };
        Reply::json(200, &json!({"package": package, "results": results, "timing_ms": {"total": 1.0},
            "cpu_ms_before": Value::Null, "cpu_ms_after": Value::Null}))
        .with_delay(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(sizes: &[u64]) -> Vec<u8> {
        let files: Vec<Value> = sizes
            .iter()
            .enumerate()
            .map(|(i, b)| json!({"name": format!("f{i}"), "role": "r", "bytes": b, "sha256": "00".repeat(32),
                "bucket": "admission", "key": format!("k/{i}"), "etag": "e"}))
            .collect();
        serde_json::to_vec(&json!({"schema": "s", "profile_id": "p", "contract": "c", "graph": "f0", "files": files})).unwrap()
    }

    fn post(path: &str, body: &[u8]) -> Call {
        Call::new("POST", path).body(body.to_vec())
    }

    #[test]
    fn the_load_protocol_matches_the_scorer_client() {
        use slop_ninja_artifact_check::spec::Version;
        let m = manifest(&[10, PART_BYTES + 5]);
        let pkg = sha(&m);
        let v4 = Version::V4.spec_sha256();
        let mut i = Instance::new(0);
        let h = i.handle(&Call::new("GET", "/healthz"), 1, false).body_json();
        assert_eq!(h["state"], "empty");
        assert_eq!((h["build"]["sessions"].as_u64(), h["build"]["threads"].as_u64()), (Some(1), Some(2)));
        assert!(specs().iter().any(|(id, d)| *d == v4 && *id == Version::V4.id()));
        let path = format!("/v2/prepare?package={pkg}&spec={v4}");
        let r = i.handle(&post(&path, &m), 2, false);
        assert_eq!(r.status, 201);
        assert_eq!(r.body_json()["missing"], json!([[0, 0], [1, 0], [1, PART_BYTES]]));
        // A second prepare of the same binding answers what is still missing.
        assert_eq!(i.chunk("/v2/files/1/0", PART_BYTES, 3).status, 201);
        let again = i.handle(&post(&path, &m), 4, false);
        assert_eq!((again.status, again.body_json()["missing"].clone()), (200, json!([[0, 0], [1, PART_BYTES]])));
        assert_eq!(i.chunk("/v2/files/1/1", 5, 5).status, 422, "not a chunk offset");
        assert_eq!(i.chunk("/v2/files/0/0", 9, 5).status, 422, "wrong length");
        let load = format!("/v2/load?package={pkg}&spec={v4}");
        let early = i.handle(&post(&load, b""), 6, false);
        assert_eq!((early.status, early.body_json()["error"].clone()), (409, json!("missing_chunks")));
        assert_eq!(i.chunk("/v2/files/0/0", 10, 7).status, 201);
        assert_eq!(i.chunk(&format!("/v2/files/1/{PART_BYTES}"), 5, 7).status, 201);
        assert_eq!(i.handle(&post(&load, b""), 8, false).status, 202);
        let h = i.handle(&Call::new("GET", "/healthz"), 9, false).body_json();
        assert_eq!((h["state"].as_str(), h["package"].as_str(), h["spec"].as_str()), (Some("ready"), Some(pkg.as_str()), Some(Version::V4.id())));
        // Another binding is refused; late chunks are acknowledged.
        let other = format!("/v2/prepare?package={}", "ab".repeat(32));
        assert_eq!(i.handle(&post(&other, &m), 10, false).status, 409);
        assert_eq!(i.chunk("/v2/files/0/0", 10, 10).status, 200);
        // Score: this package, then another.
        let body = json!({"package": pkg, "spans": true, "docs": [{"id": "d", "text": "sn-stub:band=very_strong ".to_owned() + &"word ".repeat(30)}]});
        let s = i.handle(&post("/v1/score", body.to_string().as_bytes()), 11, false);
        assert_eq!(s.status, 200);
        let v = s.body_json();
        assert_eq!(v["package"], pkg);
        assert_eq!(v["results"][0]["prediction"]["ai_confidence_percent"], 99.995);
        let wrong = json!({"package": "cd".repeat(32), "docs": [{"id": "d", "text": "x"}]});
        assert_eq!(i.handle(&post("/v1/score", wrong.to_string().as_bytes()), 12, false).status, 409);
        // Destroy empties the slot.
        i.handle(&post("/__stub/destroy", b""), 13, false);
        assert_eq!(i.slot, Slot::Empty);
    }

    #[test]
    fn queries_and_manifests_are_checked() {
        let m = manifest(&[3]);
        let pkg = sha(&m);
        let mut i = Instance::new(0);
        assert_eq!(i.handle(&post(&format!("/v2/prepare?package={pkg}&spec={}", "00".repeat(32)), &m), 1, false).body_json()["error"], "spec");
        assert_eq!(i.handle(&post(&format!("/v2/prepare?package={pkg}&x=1"), &m), 1, false).body_json()["error"], "query");
        assert_eq!(i.handle(&post(&format!("/v2/prepare?package={}", "ab".repeat(32)), &m), 1, false).status, 422);
        assert_eq!(i.handle(&post("/v2/load?package=aa", b""), 1, false).body_json()["error"], "not_prepared");
        assert_eq!(i.chunk("/v2/files/0/0", 3, 1).status, 409);
    }

    #[test]
    fn slow_and_failed_loads_and_switches() {
        let m = manifest(&[3]);
        let pkg = sha(&m);
        let mut i = Instance::new(0);
        assert_eq!(i.handle(&post("/__stub/switch", br#"{"load_ms": 100, "load_error": "refused graph_class: stub"}"#), 0, false).status, 200);
        assert_eq!(i.handle(&post("/__stub/switch", br#"{"nonsense": 1}"#), 0, false).status, 400);
        i.handle(&post(&format!("/v2/prepare?package={pkg}"), &m), 1, false);
        i.chunk("/v2/files/0/0", 3, 2);
        i.handle(&post(&format!("/v2/load?package={pkg}"), b""), 3, false);
        assert_eq!(i.handle(&Call::new("GET", "/healthz"), 50, false).body_json()["state"], "loading");
        let h = i.handle(&Call::new("GET", "/healthz"), 103, false).body_json();
        assert_eq!((h["state"].as_str(), h["error"].as_str()), (Some("failed"), Some("refused graph_class: stub")));
        assert_eq!(h["spec"], UNNAMED_LOAD_SPEC.id());
        i.handle(&post("/__stub/switch", br#"{"healthz_status": 503}"#), 104, false);
        assert_eq!(i.handle(&Call::new("GET", "/healthz"), 105, false).status, 503);
        i.handle(&post("/__stub/reset", b""), 106, false);
        assert_eq!(i, Instance::new(106));
    }

    #[test]
    fn reply_markers_and_request_checks() {
        let mut i = Instance::new(0);
        i.switches.bind_any = true;
        let ask = |i: &mut Instance, text: &str| {
            let body = json!({"package": "pp", "docs": [{"id": "d", "text": text}]});
            i.handle(&post("/v1/score", body.to_string().as_bytes()), 1, false)
        };
        let r = ask(&mut i, "sn-stub:http=503 sn-stub:delay_ms=1500 hello");
        assert_eq!((r.status, r.delay_ms), (503, 1500));
        assert_eq!(ask(&mut i, "sn-stub:reply=garbage x").body_text(), "this is not JSON");
        assert_eq!(ask(&mut i, "sn-stub:reply=two x").body_json()["results"].as_array().map(Vec::len), Some(2));
        assert_eq!(ask(&mut i, "sn-stub:reply=none x").body_json()["results"], json!([]));
        assert_ne!(ask(&mut i, "sn-stub:package=other x").body_json()["package"], "pp");
        let unknown = json!({"package": "pp", "cohort": null, "extra": 1, "docs": [{"id": "d", "text": "x"}]});
        assert_eq!(i.handle(&post("/v1/score", unknown.to_string().as_bytes()), 1, false).status, 400);
        assert_eq!(i.handle(&post("/v1/score", b"{}"), 1, true).status, 413);
        let none = json!({"package": "pp", "docs": []});
        assert_eq!(i.handle(&post("/v1/score", none.to_string().as_bytes()), 1, false).status, 400);
    }
}
