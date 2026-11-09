//! Fan-out lane objects (wasm32 only).
//!
//! - `AScorer`: a scoring lane, `a-scorer/testnet/{scope}/lane-{k:04}` (the
//!   instance, plus `/authority/<name>` for a named main authority). It
//!   owns one `a-scorer-testnet` Container instance. Its alarm runs the lane
//!   loop: report at entry, start the container with Internet off (a running
//!   one bound to another artifact or cohort is destroyed first) and load
//!   the artifact (its files manifest v2, then each file in 64 MiB chunks
//!   read from R2 pinned to the recorded entity tag, up to 3 in flight,
//!   reporting every 20 s, holding no claim, hashing nothing), set the
//!   container's inactivity timeout, then claim, score, store
//!   the private results object and the receipt object, and commit, until its
//!   job has no claimable chunk or its 600 s step budget is spent (it then
//!   re-arms at once). A newer kick supersedes the loop without stopping the
//!   container; done and paused stop it. The 120 s idle alarm stays a backstop.
//! - `EvalLane`: generation, summary and bundle lanes, and operator probes.
//! - `GenLimiter`: the deployment's Workers AI token buckets (memory only).
//! - `SpendLedger`: the deployment's monthly spend ledger (SQLite).
//! - The Queue consumer: each content-free kick reaches its lane's `/kick`.
//!
//! Scoring lanes run each claimed chunk through `lane_logic::run_chunk` over
//! [`WorkerIo`], the same code the native simulation runs: hand-backs before
//! dispatch, reconciliation, resumable crash isolation and artifact
//! attribution are decided there. The lane's limits (its step budget, the
//! single-document limit, the artifact size bound and load allowance, the
//! start-failure streak) come from the plan through its assignment.
//!
//! JavaScript interop beyond the typed `worker` 0.8.7 API: `Container.start`
//! with options and `Container.setInactivityTimeout` (neither typed there) are
//! called through `Reflect` on the raw container object, as `transactionSync`
//! is in `cf.rs`. No JavaScript source is shipped.

use super::eval_do::{
    self as ev, EVAL_BUCKET, GEN_LIMITER, RECEIPTS_BUCKET, cohort_call, get_bytes, internal_request, put, sha_hex,
};
use super::{error, json, migrate_storage, now_ms, transaction};
use crate::eval::engine::{self, CallTiming};
use crate::eval::lane_logic::{self, ChunkCtx, ChunkEnd, CohortError, Fault, LaneIo, RestartFailure, ScoreFailure};
use crate::eval::lanes::{
    ArtifactRef, Assignment, BundleReport, ClaimReply, GenClaimReply, GenClassEnd, GenLaneEnd, LaneKick, LaneRef, LaneReport, Meter,
    SummaryReport,
};
use crate::eval::limiter::{Feedback, Grants, Limiter, LimiterConfig};
use crate::eval::scorer_client::{self as sc, Entry, Ours, Prep, Slot};
use slop_ninja_artifact_check::files_manifest::FilesManifest;
use futures_util::future::{Either, join_all, select};
use js_sys::{Function, Object, Reflect};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json as j};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::time::Duration;
use wasm_bindgen::{JsCast, JsValue};
use worker::{
    Delay, DurableObject, Env, Fetcher, Headers, Method, Request, RequestInit, Response, ResponseBody, Result, State,
    durable_object,
};

const PORT: u16 = 8080;
/// Lanes report at least this often during a cold path or a wait.
const REPORT_EVERY_MS: u64 = 20_000;
/// A scorer container with no request for this long is stopped (backstop).
const IDLE_MS: i64 = 120_000;
const STOP_RETRY_MS: i64 = 30_000;
const STOP_RETRY_MAX_MS: i64 = 300_000;
/// Chunks streamed into a container at once (and the operator's probe
/// objects).
const CHUNKS_IN_FLIGHT: usize = 3;
/// One chunk, the largest transfer: the scorer's `MAX_RANGE_BYTES` (the
/// artifact spec's scorer mapping part size, 64 MiB), so no single
/// transfer carries a whole graph.
const CHUNK_BYTES: u64 = slop_ninja_artifact_check::spec::PART_BYTES;
/// The operator's scorer probe (`/run`), which has no plan: the load wait that
/// versions up to `da569b03` compiled in.
const PROBE_LOAD_WAIT_MS: u64 = 180_000;

fn container_call(raw: &JsValue, name: &str, arg: &JsValue) -> std::result::Result<JsValue, &'static str> {
    let f: Function = Reflect::get(raw, &JsValue::from_str(name))
        .ok()
        .and_then(|f| f.dyn_into().ok())
        .ok_or("container method")?;
    f.call1(raw, arg).map_err(|_| "container call")
}

async fn port_fetch(port: &Fetcher, method: Method, path: &str, body: Option<JsValue>, len: Option<u64>) -> Result<Response> {
    let mut init = RequestInit::new();
    init.with_method(method);
    let headers = Headers::new();
    headers.set("content-type", "application/json")?;
    if let Some(n) = len {
        headers.set("content-length", &n.to_string())?;
    }
    init.with_headers(headers);
    if let Some(b) = body {
        init.with_body(Some(b));
    }
    port.fetch(format!("http://container{path}"), Some(init)).await
}

async fn healthz(port: &Fetcher) -> Option<Value> {
    let mut r = port_fetch(port, Method::Get, "/healthz", None, None).await.ok()?;
    if r.status_code() != 200 {
        return None;
    }
    r.json().await.ok()
}

fn cgroup_cpu(v: &Value) -> u64 {
    v["cgroup"].as_u64().or(v["process"].as_u64()).unwrap_or(0)
}

async fn get_kv<T: serde::de::DeserializeOwned>(st: &worker::Storage, key: &str) -> Option<T> {
    st.get::<T>(key).await.ok().flatten()
}

/// Stores a kick if its `(seq, gen)` is newer than the stored assignment and
/// arms the alarm. Returns whether it was accepted.
async fn accept_kick(state: &State, k: &LaneKick) -> Result<bool> {
    let st = state.storage();
    let stored: Option<LaneKick> = get_kv(&st, "assign").await;
    let newer = stored.as_ref().is_none_or(|s| (k.seq, k.generation) > (s.seq, s.generation));
    if !newer {
        return Ok(false);
    }
    if stored.as_ref().is_none_or(|s| s.seq != k.seq) {
        // A new cohort: this lane's metering starts again.
        for key in ["m_active", "m_cpu", "m_do"] {
            st.put(key, 0i64).await?;
        }
        st.put("faults_used", Vec::<String>::new()).await?;
        st.put("start_streak", 0i64).await?;
    }
    st.put("assign", k).await?;
    st.put("active", true).await?;
    let due = st.get_alarm().await.ok().flatten();
    if due.is_none_or(|d| d > now_ms() as i64) {
        st.set_alarm(0i64).await?;
    }
    Ok(true)
}

enum LaneEnd {
    /// Re-arm after this many ms.
    Rearm(i64),
    /// The lane's work for this assignment is over.
    Finished,
    /// A newer kick replaced the assignment; its alarm continues.
    Superseded,
    /// Load-mode fault: the alarm handler throws (the platform retries it).
    Throw,
}

async fn superseded(state: &State, k: &LaneKick) -> bool {
    let stored: Option<LaneKick> = get_kv(&state.storage(), "assign").await;
    stored.is_none_or(|s| (s.seq, s.generation) != (k.seq, k.generation))
}

fn lane_ref(k: &LaneKick) -> LaneRef {
    LaneRef { seq: k.seq, kind: k.kind.clone(), lane: k.lane, generation: k.generation }
}

// ------------------------------------------------------------------- AScorer

/// A scoring lane: one on-demand Container instance.
#[durable_object(alarm)]
pub struct AScorer {
    state: State,
    env: Env,
    /// Whether the instance last prepared reports single use, so requests
    /// name the cohort (`scorer_client::sent`).
    single_use: Cell<bool>,
    /// Whether the instance last prepared takes `"sentences": false`
    /// (`/healthz` `build.score_fields`), so span requests carry spans only.
    spans_only: Cell<bool>,
}

/// The span facts of the artifact the lane's instance loaded (its `/healthz`
/// load facts), kept in the lane object's storage by package.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredFacts {
    package: String,
    facts: crate::eval::span_gate::SpanFacts,
}

#[derive(Deserialize)]
struct RunDoc {
    id: String,
    text: String,
}

#[derive(Deserialize)]
struct RunRequest {
    package_sha256: String,
    manifest_key: String,
    docs: Vec<RunDoc>,
    /// The digest of the artifact spec the load names (`spec=`), as a cohort
    /// naming that spec loads; absent: the scorer's unnamed-load spec.
    #[serde(default)]
    spec_sha256: Option<String>,
    /// Ask the scorer for each document's sentences and spans (a package
    /// with a token head); the probe's two documents bound the reply.
    #[serde(default)]
    spans: bool,
}

/// An admission run (`POST /admit`): the cohort's label, spec digest and
/// lane limits, on a freshly started instance.
#[derive(Deserialize)]
struct AdmitRequest {
    package_sha256: String,
    manifest_key: String,
    label: String,
    spec_sha256: String,
    limits: crate::eval::admission::AdmitLimits,
    docs: Vec<RunDoc>,
    /// Spec v5 cohorts: the control document is scored with spans.
    #[serde(default)]
    spans: bool,
}


#[derive(Clone, Debug, Default)]
struct Cold {
    host: Option<String>,
    /// The scorer's reported limits (`/healthz` `build.limits`), for the probe.
    limits: Value,
    started: bool,
    /// The artifact was streamed and loaded this time.
    loaded: bool,
    start_ms: u64,
    load_ms: u64,
    cpu_ms: u64,
}

impl DurableObject for AScorer {
    fn new(state: State, env: Env) -> Self {
        Self { state, env, single_use: Cell::new(false), spans_only: Cell::new(false) }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        match (req.method(), req.path().as_str()) {
            (Method::Post, "/kick") => {
                let Ok(k) = req.json::<LaneKick>().await else {
                    return error("bad_internal_request", 400);
                };
                json(&j!({"accepted": accept_kick(&self.state, &k).await?}))
            }
            (Method::Post, "/stop") => {
                let st = self.state.storage();
                st.put("active", false).await?;
                let stopped = self.stop().await;
                if !stopped {
                    st.set_alarm(STOP_RETRY_MS).await?;
                }
                json(&j!({"stopped": stopped}))
            }
            (Method::Post, "/run") => {
                // The operator's scorer probe (never a cohort lane).
                let Ok(run) = req.json::<RunRequest>().await else {
                    return error("bad_internal_request", 400);
                };
                let art = ArtifactRef { id: String::new(), package_sha256: run.package_sha256, manifest_key: run.manifest_key };
                let t0 = now_ms();
                let lim = PrepLimits { inactivity_ms: 60_000, load_wait_ms: PROBE_LOAD_WAIT_MS, artifact_max_bytes: u64::MAX, load_fail: false };
                let spec = run.spec_sha256.as_deref();
                let mut prep = self.prepare(None, &art, lim, None, spec).await;
                if let Err(Prep::Infra(sc::REUSED | sc::INSTANCE_BOUND)) = prep {
                    // The instance still held another artifact and is destroyed now.
                    prep = self.prepare(None, &art, lim, None, spec).await;
                }
                let cold = match prep {
                    Ok(c) => c,
                    Err(e) => return error(e.code(), 502),
                };
                let _ = self.state.storage().set_alarm(IDLE_MS).await;
                let docs: Vec<(u32, String, String)> =
                    run.docs.iter().enumerate().map(|(n, d)| (d.id.parse().unwrap_or(n as u32), d.text.clone(), sha_hex(d.text.as_bytes()))).collect();
                match self.score_docs(&art, &docs, 120_000, None, spec, run.spans).await {
                    Ok((results, t)) => json(&j!({"results": results, "timing": t, "cold": {"started": cold.started,
                        "start_ms": cold.start_ms, "load_ms": cold.load_ms, "cpu_ms": cold.cpu_ms, "limits": cold.limits},
                        "total_ms": now_ms() - t0})),
                    Err(e) => json(&j!({"error": format!("{e:?}")})),
                }
            }
            (Method::Post, "/admit") => {
                // An admission run (never a cohort lane): the verdict comes
                // from a freshly started instance, under the cohort's label,
                // spec and limits; the reply says whether a failure was
                // definitive (`admission::classify_admit`).
                let Ok(run) = req.json::<AdmitRequest>().await else {
                    return error("bad_internal_request", 400);
                };
                if self.state.container().is_some_and(|c| c.running()) {
                    self.destroy().await;
                }
                // The cohort's validated lane limits are the only source of
                // these numbers: a zero is a malformed request, refused as
                // the platform's failure before anything loads.
                let l = run.limits;
                if !l.complete() {
                    return json(&j!({"outcome": "platform", "code": "bad_internal_request"}));
                }
                let art = ArtifactRef { id: String::new(), package_sha256: run.package_sha256, manifest_key: run.manifest_key };
                let lim = PrepLimits {
                    inactivity_ms: l.inactivity_ms,
                    load_wait_ms: l.load_wait_ms,
                    artifact_max_bytes: l.artifact_max_bytes,
                    load_fail: false,
                };
                let label = run.label.as_str();
                let cold = match self.prepare(None, &art, lim, Some(label), Some(run.spec_sha256.as_str())).await {
                    Ok(c) => c,
                    Err(Prep::Artifact(code, definitive)) => {
                        return json(&j!({"outcome": "artifact", "code": code, "definitive": definitive, "fresh": true}));
                    }
                    Err(Prep::Infra(code)) => return json(&j!({"outcome": "platform", "code": code})),
                };
                let _ = self.state.storage().set_alarm(IDLE_MS).await;
                let docs: Vec<(u32, String, String)> =
                    run.docs.iter().enumerate().map(|(n, d)| (d.id.parse().unwrap_or(n as u32), d.text.clone(), sha_hex(d.text.as_bytes()))).collect();
                match self.score_docs(&art, &docs, l.doc_timeout_ms, Some(label), Some(run.spec_sha256.as_str()), run.spans).await {
                    Ok((results, _)) => json(&j!({"outcome": "loaded", "results": results, "fresh": cold.started, "spans": run.spans})),
                    Err(e) => json(&j!({"outcome": "loaded", "error": format!("{e:?}"), "fresh": cold.started})),
                }
            }
            (Method::Get, "/state") => {
                let st = self.state.storage();
                let assign: Option<LaneKick> = get_kv(&st, "assign").await;
                let running = self.state.container().map(|c| c.running());
                let meter = self.meter().await;
                let alarm = st.get_alarm().await.ok().flatten();
                json(&j!({"running": running, "assign": assign, "active": get_kv::<bool>(&st, "active").await,
                    "starts": get_kv::<i64>(&st, "starts").await.unwrap_or(0), "meter": meter,
                    "stop_failures": get_kv::<i64>(&st, "stop_failures").await.unwrap_or(0),
                    "alarm_in_ms": alarm.map(|a| a - now_ms() as i64)}))
            }
            _ => error("not_found", 404),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        let st = self.state.storage();
        let t0 = now_ms();
        let assign: Option<LaneKick> = get_kv(&st, "assign").await;
        let active = get_kv::<bool>(&st, "active").await.unwrap_or(false);
        if let (Some(k), true) = (assign, active) {
            let end = self.lane_loop(&k, t0).await;
            let m_do: i64 = get_kv(&st, "m_do").await.unwrap_or(0);
            let _ = st.put("m_do", m_do + (now_ms() - t0) as i64).await;
            match end {
                LaneEnd::Rearm(ms) => st.set_alarm(ms.max(0)).await?,
                LaneEnd::Finished => {
                    if !superseded(&self.state, &k).await {
                        st.put("active", false).await?;
                    }
                    if self.state.container().is_some_and(|c| c.running()) {
                        st.set_alarm(IDLE_MS).await?;
                    }
                }
                LaneEnd::Superseded => {}
                LaneEnd::Throw => return Err(worker::Error::RustError("lane fault injection".into())),
            }
            return Response::ok("lane");
        }
        self.idle_alarm().await
    }
}

/// What a prepare takes from the plan (through the lane's assignment).
#[derive(Clone, Copy, Debug)]
struct PrepLimits {
    inactivity_ms: u64,
    /// The stream allowance and the model-load allowance, each.
    load_wait_ms: u64,
    artifact_max_bytes: u64,
    /// Load-mode fault: report the load as failed.
    load_fail: bool,
}

impl PrepLimits {
    fn of(a: &Assignment) -> Self {
        Self {
            inactivity_ms: a.inactivity_ms,
            load_wait_ms: if a.load_wait_ms > 0 { a.load_wait_ms } else { PROBE_LOAD_WAIT_MS },
            artifact_max_bytes: if a.artifact_max_bytes > 0 { a.artifact_max_bytes } else { u64::MAX },
            load_fail: a.faults.load_fail,
        }
    }
}

fn start_wait_ms(env: &Env) -> u64 {
    env.var("SCORER_START_WAIT_MS").ok().and_then(|v| v.to_string().trim().parse::<u64>().ok()).unwrap_or(180_000)
}

/// A scoring lane's world for `lane_logic::run_chunk`: the eval and receipts
/// buckets, the cohort object, this lane's container and its invocation clock.
struct WorkerIo<'a> {
    lane: &'a AScorer,
    inst: &'a LaneKick,
    r: &'a LaneRef,
    k: &'a LaneKick,
    a: &'a Assignment,
    art: &'a ArtifactRef,
    /// When this alarm invocation started.
    t0: u64,
}

impl LaneIo for WorkerIo<'_> {
    async fn eval_get(&mut self, key: &str) -> std::result::Result<Option<Vec<u8>>, ()> {
        get_bytes(&self.lane.env, EVAL_BUCKET, key).await.map_err(|_| ())
    }

    async fn eval_put(&mut self, key: &str, body: Vec<u8>) -> std::result::Result<(), ()> {
        put(&self.lane.env, EVAL_BUCKET, key, body).await.map_err(|_| ())
    }

    async fn rcpt_get(&mut self, key: &str) -> std::result::Result<Option<Vec<u8>>, ()> {
        get_bytes(&self.lane.env, RECEIPTS_BUCKET, key).await.map_err(|_| ())
    }

    async fn rcpt_put(&mut self, key: &str, body: Vec<u8>) -> std::result::Result<(), ()> {
        put(&self.lane.env, RECEIPTS_BUCKET, key, body).await.map_err(|_| ())
    }

    async fn cohort(&mut self, path: &str, body: Value) -> std::result::Result<Value, CohortError> {
        cohort_call::<Value>(&self.lane.env, self.inst, path, &body)
            .await
            .map_err(|e| if e == "refused by the cohort" { CohortError::Refused } else { CohortError::Unreachable })
    }

    async fn score(&mut self, docs: &[(u32, String, String)], timeout_ms: u64) -> std::result::Result<(Vec<Value>, CallTiming), ScoreFailure> {
        let f = &self.a.faults;
        let has = |t: Option<u32>| t.is_some_and(|t| docs.iter().any(|d| d.0 == t));
        // Load-mode faults: the container dies with the request in flight.
        let killed = f.crash_all
            || has(f.crash_task)
            || docs.iter().any(|d| f.crash_tasks.contains(&d.0))
            || (docs.len() == 1 && has(f.flaky_task) && self.lane.fault_once("flaky").await)
            || (docs.len() > 1 && f.kill && self.lane.fault_once("kill").await);
        if killed {
            self.lane.destroy().await;
            return Err(ScoreFailure::Died);
        }
        self.lane
            .score_docs(self.art, docs, timeout_ms, Some(self.a.label.as_str()), self.a.spec_sha256.as_deref(), self.a.spans)
            .await
    }

    async fn restart(&mut self) -> std::result::Result<(), RestartFailure> {
        self.lane.destroy().await;
        let cohort = Some(self.a.label.as_str());
        self.lane
            .prepare(Some((self.inst, self.r)), self.art, PrepLimits::of(self.a), cohort, self.a.spec_sha256.as_deref())
            .await
            .map(|_| ())
            .map_err(Prep::restart)
    }

    fn restart_bound_ms(&self) -> u64 {
        start_wait_ms(&self.lane.env) + 2 * PrepLimits::of(self.a).load_wait_ms
    }

    async fn may_run(&mut self, ms: u64) -> bool {
        now_ms().saturating_sub(self.t0) + ms <= self.a.step_budget_ms && !superseded(&self.lane.state, self.k).await
    }

    fn now_ms(&self) -> u64 {
        now_ms()
    }

    async fn sleep(&mut self, ms: u64) {
        Delay::from(Duration::from_millis(ms)).await;
    }

    async fn fault(&mut self, f: Fault) -> bool {
        match f {
            Fault::ThrowAfterClaim => self.a.faults.throw_after_claim && self.lane.fault_once("throw").await,
            Fault::DieAfterPut => self.a.faults.crash_after_put && self.lane.fault_once("put").await,
            Fault::DieBeforePut => false,
        }
    }

    async fn meter(&mut self) -> Meter {
        self.lane.meter().await
    }
}

impl AScorer {
    /// Cumulative metering of the current assignment.
    async fn meter(&self) -> Meter {
        let st = self.state.storage();
        let started: i64 = get_kv(&st, "started_at_ms").await.unwrap_or(0);
        let open = if started > 0 { (now_ms() as i64 - started).max(0) } else { 0 };
        Meter {
            active_ms: (get_kv::<i64>(&st, "m_active").await.unwrap_or(0) + open) as u64,
            cpu_ms: (get_kv::<i64>(&st, "m_cpu").await.unwrap_or(0) + get_kv::<i64>(&st, "s_cpu").await.unwrap_or(0)) as u64,
            do_ms: get_kv::<i64>(&st, "m_do").await.unwrap_or(0) as u64,
        }
    }

    /// Closes the metered session of a container that is no longer running.
    async fn close_session(&self) {
        let st = self.state.storage();
        let started: i64 = get_kv(&st, "started_at_ms").await.unwrap_or(0);
        if started > 0 {
            let ms = (now_ms() as i64 - started).max(0);
            for (k, add) in [("m_active", ms), ("active_ms", ms)] {
                let v: i64 = get_kv(&st, k).await.unwrap_or(0);
                let _ = st.put(k, v + add).await;
            }
            let s_cpu: i64 = get_kv(&st, "s_cpu").await.unwrap_or(0);
            let m_cpu: i64 = get_kv(&st, "m_cpu").await.unwrap_or(0);
            let _ = st.put("m_cpu", m_cpu + s_cpu).await;
            let _ = st.put("s_cpu", 0i64).await;
            let _ = st.put("started_at_ms", 0i64).await;
        }
    }

    async fn report(
        &self,
        k: &LaneKick,
        r: &LaneRef,
        state: &str,
        cold: Option<Cold>,
        code: Option<&str>,
    ) -> std::result::Result<Assignment, &'static str> {
        let m = self.meter().await;
        let rep = LaneReport {
            state: state.into(),
            cold_ms: cold.as_ref().map(|c| c.start_ms + c.load_ms),
            cold_cpu_ms: cold.as_ref().map(|c| c.cpu_ms),
            host: cold.as_ref().and_then(|c| c.host.clone()),
            started: cold.as_ref().is_some_and(|c| c.started),
            active_ms: m.active_ms,
            cpu_ms: m.cpu_ms,
            do_ms: m.do_ms,
            container_up: self.state.container().is_some_and(|c| c.running()),
            code: code.map(str::to_owned),
        };
        cohort_call(&self.env, k, "/lane/report", &j!({"lane": r, "report": rep})).await
    }

    async fn set_inactivity(&self, ms: u64) -> bool {
        let Some(c) = self.state.container() else { return false };
        match container_call(c.as_ref(), "setInactivityTimeout", &JsValue::from_f64(ms as f64)) {
            Ok(v) => match v.dyn_into::<js_sys::Promise>() {
                Ok(p) => wasm_bindgen_futures::JsFuture::from(p).await.is_ok(),
                Err(_) => true,
            },
            Err(_) => false,
        }
    }

    /// Starts the container if needed (Internet off) and loads the artifact for
    /// `cohort` (`None` for the operator probe) by the artifact spec `spec`
    /// names (a digest; `None`: the scorer's unnamed-load spec). A running
    /// instance bound to another artifact, cohort or spec, or whose load of
    /// this one failed, is destroyed first and a fresh one started, counting
    /// nothing; one that refused this artifact definitively returns the
    /// refusal. A scorer that does not list `spec` fails the prepare as the
    /// platform's (`scorer_client::SPEC_NOT_SERVED`). `ctx` (the lane's kick,
    /// lane) receives a `starting` report at least every 20 s. A
    /// non-definitive artifact failure counts only on a freshly started
    /// instance: on a reused one the instance is destroyed and the failure is
    /// the platform's, so the next try starts fresh.
    async fn prepare(
        &self,
        ctx: Option<(&LaneKick, &LaneRef)>,
        art: &ArtifactRef,
        lim: PrepLimits,
        cohort: Option<&str>,
        spec: Option<&str>,
    ) -> std::result::Result<Cold, Prep> {
        let st = self.state.storage();
        let c = self.state.container().ok_or(Prep::Infra("no container binding"))?;
        let t0 = now_ms();
        let mut last = t0;
        let mut cold = Cold::default();
        if c.running() {
            let h = match c.get_tcp_port(PORT) {
                Ok(port) => healthz(&port).await,
                Err(_) => None,
            };
            match sc::entry(h.as_ref(), &art.package_sha256, cohort, spec) {
                Entry::Keep => {}
                Entry::Fresh => self.destroy().await,
                Entry::Refused(code) => {
                    self.destroy().await;
                    return Err(Prep::Artifact(code, true));
                }
            }
        }
        if !c.running() {
            self.close_session().await;
            let opts = Object::new();
            let _ = Reflect::set(&opts, &"enableInternet".into(), &JsValue::FALSE);
            container_call(c.as_ref(), "start", &opts).map_err(Prep::Infra)?;
            cold.started = true;
            let starts: i64 = get_kv(&st, "starts").await.unwrap_or(0);
            let _ = st.put("starts", starts + 1).await;
            let _ = st.put("started_at_ms", now_ms() as i64).await;
            let _ = st.put("s_cpu", 0i64).await;
        }
        let _ = st.put("last_use_ms", now_ms() as i64).await;
        let _ = self.set_inactivity(lim.inactivity_ms).await;
        let port = c.get_tcp_port(PORT).map_err(|_| Prep::Infra("container port"))?;
        let beat = |last: &mut u64| {
            let due = now_ms().saturating_sub(*last) >= REPORT_EVERY_MS;
            if due {
                *last = now_ms();
            }
            due
        };
        // The server listens before it loads anything; bounded wait for it to
        // answer. A burst of starts can take over a minute to come up (measured
        // 2026-10-31), so the bound is a deployment parameter.
        let wait_ms = start_wait_ms(&self.env);
        let mut up = None;
        for _ in 0..wait_ms.div_ceil(250) {
            if let Some(h) = healthz(&port).await {
                up = Some(h);
                break;
            }
            if beat(&mut last) && let Some((i, r)) = ctx {
                let _ = self.report(i, r, "starting", None, None).await;
            }
            Delay::from(Duration::from_millis(250)).await;
        }
        let Some(h) = up else {
            let _ = c.destroy(Some("no answer")).await;
            self.close_session().await;
            return Err(Prep::Infra("container did not answer"));
        };
        cold.start_ms = now_ms() - t0;
        cold.limits = h["build"]["limits"].clone();
        self.single_use.set(sc::names_cohort(&h));
        self.spans_only.set(sc::takes_sentences_field(&h));
        let cohort = sc::sent(&h, cohort);
        // The spec digest is sent only when the scorer lists it, so the spec
        // sent is the cohort's whenever the prepare goes on.
        let spec = match sc::spec_sent(&h, spec) {
            Ok(s) => s,
            Err(e) => {
                let _ = c.destroy(Some("artifact spec")).await;
                self.close_session().await;
                return Err(e);
            }
        };
        // Build identity (S-1): the binary and runtime library the pinned digests name.
        for (field, var) in [("binary_sha256", "SCORER_BINARY_SHA256"), ("ort_sha256", "SCORER_ORT_SHA256")] {
            if let Ok(want) = self.env.var(var).map(|v| v.to_string())
                && !want.is_empty()
                && h["build"][field].as_str() != Some(want.as_str())
            {
                let _ = c.destroy(Some("build identity")).await;
                self.close_session().await;
                return Err(Prep::Infra("scorer build identity differs from the pinned digests"));
            }
        }
        let t1 = now_ms();
        if sc::slot(&h, &art.package_sha256, cohort, spec) != Slot::Ours(Ours::Ready) {
            if let Err(e) = self.load(&port, art, cohort, spec, ctx, &mut last, lim).await {
                self.destroy().await;
                return Err(match e {
                    Prep::Artifact(_, false) if !cold.started => Prep::Infra(sc::REUSED),
                    e => e,
                });
            }
            cold.loaded = true;
        }
        cold.load_ms = now_ms() - t1;
        if cold.started || cold.loaded {
            // The host's CPU model: outputs can differ in their last bits across CPUs.
            if let Ok(mut r) = port_fetch(&port, Method::Get, "/info", None, None).await
                && let Ok(v) = r.json::<Value>().await
            {
                cold.host = v["cpu_model"].as_str().map(str::to_owned);
            }
        }
        if let Some(h) = healthz(&port).await {
            cold.cpu_ms = cgroup_cpu(&h["cpu_ms"]);
            let s: i64 = get_kv(&st, "s_cpu").await.unwrap_or(0);
            let _ = st.put("s_cpu", s.max(cold.cpu_ms as i64)).await;
            // The loaded artifact's span facts (token head, decoder), for a
            // span cohort's format check and structural conditions.
            if h["package"].as_str() == Some(art.package_sha256.as_str()) && h["state"] == "ready" {
                let facts = StoredFacts { package: art.package_sha256.clone(), facts: crate::eval::span_gate::SpanFacts::of_healthz(&h) };
                let _ = st.put("span_facts", facts).await;
            }
        }
        let _ = st.put("last_use_ms", now_ms() as i64).await;
        Ok(cold)
    }

    /// Loads the artifact for `cohort` (the cohort sent, if any) by the spec
    /// `spec` names (the digest sent, if any) and waits for it to be ready:
    /// the files manifest v2 (read from the eval bucket, its bytes checked to
    /// be the package, a small record) is posted to the instance, every
    /// chunk it lists as missing is read from its bucket by a ranged read
    /// pinned to the entity tag admission recorded and piped as a JS stream
    /// (no byte enters this isolate, nothing is hashed) to its offset, up to
    /// 3 at a time, then the load is asked for. The transfer and the load
    /// each get the plan's load allowance; only the manifest (the artifact's)
    /// and the load's outcome can be the artifact's failure, and an admission
    /// refusal is definitive unless it can be a host fault
    /// (`scorer_client::load_failed`). An object absent or changed since
    /// admission recorded it is the platform's here
    /// (`scorer_client::OBJECT_CODES`): admission audits it.
    #[allow(clippy::too_many_arguments)]
    async fn load(
        &self,
        port: &Fetcher,
        art: &ArtifactRef,
        cohort: Option<&str>,
        spec: Option<&str>,
        ctx: Option<(&LaneKick, &LaneRef)>,
        last: &mut u64,
        lim: PrepLimits,
    ) -> std::result::Result<u64, Prep> {
        let manifest = match get_bytes(&self.env, EVAL_BUCKET, &art.manifest_key).await {
            Err(_) => return Err(Prep::Infra("artifact manifest read")),
            Ok(None) => return Err(Prep::Artifact("artifact manifest missing", false)),
            Ok(Some(m)) => m,
        };
        if sha_hex(&manifest) != art.package_sha256 {
            return Err(Prep::Artifact("artifact manifest digest", true));
        }
        let files: FilesManifest =
            serde_json::from_slice(&manifest).map_err(|_| Prep::Artifact("artifact manifest json", true))?;
        let bytes = files.files.iter().try_fold(0u64, |a, f| a.checked_add(f.bytes));
        if bytes.is_none_or(|b| b > lim.artifact_max_bytes) {
            return Err(Prep::Artifact("artifact over the declared size bound", true));
        }
        let running = || self.state.container().is_some_and(|c| c.running());
        let mut r = match port_fetch(
            port,
            Method::Post,
            &sc::prepare_path(&art.package_sha256, cohort, spec),
            Some(js_sys::Uint8Array::from(manifest.as_slice()).into()),
            Some(manifest.len() as u64),
        )
        .await
        {
            Ok(r) => r,
            Err(_) if !running() => return Err(Prep::Artifact("container died during the load", false)),
            Err(_) => return Err(Prep::Infra("prepare call")),
        };
        let status = r.status_code();
        let body = r.json::<Value>().await.unwrap_or(Value::Null);
        let missing = sc::prepare_reply(status, &body)?;
        let stream_until = now_ms() + lim.load_wait_ms;
        let mut uploaded = 0;
        for group in missing.chunks(CHUNKS_IN_FLIGHT) {
            let chunks = join_all(group.iter().map(|&(index, offset)| {
                let files = &files;
                async move {
                    let f = files.files.get(index).ok_or(Prep::Infra("chunk index"))?;
                    let len = f.bytes.checked_sub(offset).filter(|l| *l > 0).ok_or(Prep::Infra("chunk offset"))?.min(CHUNK_BYTES);
                    let binding = match f.bucket.as_str() {
                        "admission" => super::admission_do::ADMISSION_BUCKET,
                        "eval" => EVAL_BUCKET,
                        _ => return Err(Prep::Infra("chunk bucket")),
                    };
                    let bucket = self.env.bucket(binding).map_err(|_| Prep::Infra("chunk bucket"))?;
                    let mut get = bucket.get(f.key.clone()).range(worker::Range::OffsetWithLength { offset, length: len });
                    if let Some(etag) = &f.etag {
                        get = get.only_if(worker::Conditional {
                            etag_matches: Some(etag.clone()),
                            etag_does_not_match: None,
                            uploaded_before: None,
                            uploaded_after: None,
                        });
                    }
                    let obj = match get.execute().await {
                        Err(_) => return Err(Prep::Infra("chunk read")),
                        Ok(None) => return Err(Prep::Infra(sc::OBJECT_MISSING)),
                        Ok(Some(o)) => o,
                    };
                    // No body: the pinned read refused (the entity tag changed).
                    let Some(body) = obj.body() else {
                        return Err(Prep::Infra(sc::OBJECT_CHANGED));
                    };
                    let stream = match body.response_body().map_err(|_| Prep::Infra("chunk body"))? {
                        ResponseBody::Stream(s) => s,
                        _ => return Err(Prep::Infra("chunk stream")),
                    };
                    let r = port_fetch(port, Method::Put, &sc::chunk_path(index, offset), Some(stream.into()), Some(len))
                        .await
                        .map_err(|_| Prep::Infra("chunk upload"))?;
                    if !matches!(r.status_code(), 200 | 201) {
                        return Err(sc::chunk_refused(r.status_code()));
                    }
                    Ok(len)
                }
            }));
            let left = stream_until.saturating_sub(now_ms());
            let r = match select(Box::pin(chunks), Box::pin(Delay::from(Duration::from_millis(left)))).await {
                Either::Left((r, _)) => r,
                Either::Right(_) => return Err(Prep::Infra("artifact stream timeout")),
            };
            for x in r {
                uploaded += x?;
            }
            if now_ms().saturating_sub(*last) >= REPORT_EVERY_MS {
                *last = now_ms();
                if let Some((i, rr)) = ctx {
                    let _ = self.report(i, rr, "starting", None, None).await;
                }
            }
        }
        if lim.load_fail {
            // Load-mode fault: the artifact fails to load.
            return Err(Prep::Artifact("artifact load failed (load fault)", false));
        }
        let mut r = match port_fetch(port, Method::Post, &sc::load_path(&art.package_sha256, cohort, spec), None, None).await {
            Ok(r) => r,
            Err(_) if !running() => return Err(Prep::Artifact("container died during the load", false)),
            Err(_) => return Err(Prep::Infra("load call")),
        };
        let status = r.status_code();
        if !matches!(status, 200 | 202) {
            let body = r.json::<Value>().await.unwrap_or(Value::Null);
            return Err(sc::load_reply(status, &body).unwrap_or(Prep::Infra("load refused")));
        }
        let load_until = now_ms() + lim.load_wait_ms;
        while now_ms() < load_until {
            match healthz(port).await {
                Some(h) => match sc::slot(&h, &art.package_sha256, cohort, spec) {
                    Slot::Ours(Ours::Ready) => return Ok(uploaded),
                    Slot::Ours(Ours::Failed) => return Err(sc::load_failed(h["error"].as_str().unwrap_or(""))),
                    _ if h["state"] == "failed" => return Err(Prep::Artifact("artifact load failed", false)),
                    _ => {}
                },
                None if !running() => return Err(Prep::Artifact("container died during the load", false)),
                None => {}
            }
            if now_ms().saturating_sub(*last) >= REPORT_EVERY_MS {
                *last = now_ms();
                if let Some((i, rr)) = ctx {
                    let _ = self.report(i, rr, "starting", None, None).await;
                }
            }
            Delay::from(Duration::from_millis(500)).await;
        }
        Err(Prep::Artifact("artifact load outlasted its allowance", false))
    }

    /// After a dispatched request failed: `Failed` if the container is still up
    /// and ready with this artifact, cohort and spec, else it is destroyed and
    /// the request `Died`.
    async fn after_failure(&self, art: &ArtifactRef, cohort: Option<&str>, spec: Option<&str>) -> ScoreFailure {
        if let Some(c) = self.state.container()
            && c.running()
            && let Ok(port) = c.get_tcp_port(PORT)
            && let Some(h) = healthz(&port).await
            && sc::slot(&h, &art.package_sha256, cohort, spec) == Slot::Ours(Ours::Ready)
        {
            return ScoreFailure::Failed;
        }
        self.destroy().await;
        ScoreFailure::Died
    }

    /// One scorer request, bounded by `timeout_ms`. Only evidence of container
    /// death (it stopped or stopped answering, or the request outlasted its
    /// bound and was destroyed) is `Died`; a failure with the container up and
    /// ready is `Failed`, and a refusal before inference `NotDispatched`. The
    /// request names `cohort` when the instance is single use.
    async fn score_docs(
        &self,
        art: &ArtifactRef,
        docs: &[(u32, String, String)],
        timeout_ms: u64,
        cohort: Option<&str>,
        spec: Option<&str>,
        spans: bool,
    ) -> std::result::Result<(Vec<Value>, CallTiming), ScoreFailure> {
        let c = self.state.container().ok_or(ScoreFailure::NotDispatched)?;
        if !c.running() {
            return Err(ScoreFailure::NotDispatched);
        }
        let port = c.get_tcp_port(PORT).map_err(|_| ScoreFailure::NotDispatched)?;
        let body = sc::score_body_with(&art.package_sha256, cohort.filter(|_| self.single_use.get()), docs, spans, spans && self.spans_only.get());
        let st = self.state.storage();
        let _ = st.put("last_use_ms", now_ms() as i64).await;
        let t0 = now_ms();
        let call = port_fetch(&port, Method::Post, "/v1/score", Some(JsValue::from_str(&body)), None);
        // From here the documents may reach the model.
        let mut r = match select(Box::pin(call), Box::pin(Delay::from(Duration::from_millis(timeout_ms.max(1_000))))).await {
            Either::Left((Ok(r), _)) => r,
            Either::Left((Err(_), _)) => return Err(self.after_failure(art, cohort, spec).await),
            Either::Right(_) => {
                // Outlasted its bound: a hung instance is destroyed.
                self.destroy().await;
                return Err(ScoreFailure::Died);
            }
        };
        match r.status_code() {
            200 => {}
            // The scorer refused the batch before inference (not ready, or busy).
            409 | 503 => return Err(ScoreFailure::NotDispatched),
            // The scorer answered and rejected the request itself.
            400 | 404 | 413 | 422 => return Err(ScoreFailure::Failed),
            _ => return Err(self.after_failure(art, cohort, spec).await),
        }
        let v: Value = r.json().await.map_err(|_| ScoreFailure::Failed)?;
        let results = v["results"].as_array().cloned().ok_or(ScoreFailure::Failed)?;
        let after = cgroup_cpu(&v["cpu_ms_after"]);
        let s: i64 = get_kv(&st, "s_cpu").await.unwrap_or(0);
        let _ = st.put("s_cpu", s.max(after as i64)).await;
        let _ = st.put("last_use_ms", now_ms() as i64).await;
        Ok((
            results,
            CallTiming {
                call_ms: now_ms() - t0,
                server_ms: v["timing_ms"]["total"].as_f64().unwrap_or(0.0) as u64,
                load_ms: 0,
                cpu_ms: after.saturating_sub(cgroup_cpu(&v["cpu_ms_before"])),
            },
        ))
    }

    /// Destroys the container (a crash, a timeout, or isolation's fresh start)
    /// and waits briefly until it reads as stopped.
    async fn destroy(&self) {
        if let Some(c) = self.state.container() {
            if c.running() {
                let _ = c.destroy(None).await;
            }
            for _ in 0..40 {
                if !c.running() {
                    break;
                }
                Delay::from(Duration::from_millis(250)).await;
            }
        }
        self.close_session().await;
    }

    /// Stops the container; returns whether it is stopped. A refused or
    /// ineffective stop is re-checked by the idle alarm with backoff.
    async fn stop(&self) -> bool {
        let st = self.state.storage();
        if let Some(c) = self.state.container()
            && c.running()
        {
            if c.destroy(None).await.is_err() {
                let n: i64 = get_kv(&st, "stop_failures").await.unwrap_or(0);
                let _ = st.put("stop_failures", n + 1).await;
                return false;
            }
            if c.running() {
                if get_kv::<i64>(&st, "stop_requested_ms").await.unwrap_or(0) == 0 {
                    let _ = st.put("stop_requested_ms", now_ms() as i64).await;
                }
                return false;
            }
        }
        let _ = st.put("stop_requested_ms", 0i64).await;
        self.close_session().await;
        true
    }

    /// The backstop alarm: stop a container idle for `IDLE_MS`, re-checking a
    /// stop that has not taken effect, with backoff.
    async fn idle_alarm(&self) -> Result<Response> {
        let st = self.state.storage();
        let now = now_ms() as i64;
        let last: i64 = get_kv(&st, "last_use_ms").await.unwrap_or(0);
        let requested: i64 = get_kv(&st, "stop_requested_ms").await.unwrap_or(0);
        let running = self.state.container().is_some_and(|c| c.running());
        if !running {
            let _ = st.put("stop_requested_ms", 0i64).await;
            let _ = st.put("stop_retries", 0i64).await;
            self.close_session().await;
            return Response::ok("stopped");
        }
        if requested > 0 && now - requested >= STOP_RETRY_MS {
            let n: i64 = get_kv(&st, "stop_failures").await.unwrap_or(0);
            let _ = st.put("stop_failures", n + 1).await;
        }
        if now - last >= IDLE_MS || requested > 0 {
            if self.stop().await {
                let _ = st.put("stop_retries", 0i64).await;
            } else {
                let n: i64 = get_kv(&st, "stop_retries").await.unwrap_or(0) + 1;
                let _ = st.put("stop_retries", n).await;
                st.set_alarm((STOP_RETRY_MS << (n - 1).clamp(0, 4)).min(STOP_RETRY_MAX_MS)).await?;
            }
        } else {
            st.set_alarm(IDLE_MS - (now - last)).await?;
        }
        Response::ok("idle check")
    }

    async fn fault_once(&self, name: &str) -> bool {
        let st = self.state.storage();
        let mut used: Vec<String> = get_kv(&st, "faults_used").await.unwrap_or_default();
        if used.iter().any(|u| u == name) {
            return false;
        }
        used.push(name.into());
        let _ = st.put("faults_used", used).await;
        true
    }

    async fn lane_loop(&self, k: &LaneKick, t0: u64) -> LaneEnd {
        let st = self.state.storage();
        let r = lane_ref(k);
        // Every call of this lane goes back to the authority that kicked it.
        let inst = k;
        let mut a = match self.report(inst, &r, "starting", None, None).await {
            Ok(a) => a,
            Err(_) => return LaneEnd::Rearm(5_000),
        };
        match a.status.as_str() {
            "fenced" => return LaneEnd::Superseded,
            "done" | "paused" => {
                self.stop().await;
                let _ = self.report(inst, &r, &a.status, None, None).await;
                return LaneEnd::Finished;
            }
            _ => {}
        }
        let Some(art) = a.artifact.clone() else { return LaneEnd::Finished };
        let Some(seed) = ev::private_seed(&self.env) else { return LaneEnd::Rearm(30_000) };
        let seed = engine::cohort_seed(&seed, &a.label);
        let prefix = engine::receipt_prefix(&a.label);
        let mut cache = lane_logic::ShardCache::new();
        let mut loaded_at = None;
        let mut last_report = now_ms();
        let prestart = a.status == "wait";
        loop {
            if now_ms().saturating_sub(t0) > a.step_budget_ms {
                return LaneEnd::Rearm(0);
            }
            if superseded(&self.state, k).await {
                return LaneEnd::Superseded;
            }
            // The cold path holds no claim. A job whose artifact failed needs no
            // container: its unresolved documents are model errors.
            let loaded = self.state.container().is_some_and(|c| c.running()) && loaded_at.is_some();
            if a.artifact_failed {
                if self.state.container().is_some_and(|c| c.running()) {
                    self.stop().await;
                }
            } else if !loaded {
                // A lane held for its entrant's objects (below) starts no
                // scorer Container again until the cohort answers that the
                // audit or the miner's repair released it (review round 2:
                // no cold start per admission tick while the miner has not
                // repaired).
                if get_kv::<bool>(&st, "object_held").await.unwrap_or(false) {
                    match cohort_call::<bool>(&self.env, inst, "/lane/object-held", &j!({"lane": r, "job": a.job})).await {
                        Ok(true) => return LaneEnd::Rearm(crate::eval::admission::TICK_MS as i64),
                        Ok(false) => {
                            let _ = st.delete("object_held").await;
                        }
                        Err(_) => return LaneEnd::Rearm(5_000),
                    }
                }
                match self.prepare(Some((inst, &r)), &art, PrepLimits::of(&a), Some(a.label.as_str()), a.spec_sha256.as_deref()).await {
                    Ok(cold) => {
                        let _ = st.put("start_streak", 0i64).await;
                        loaded_at = Some(now_ms());
                        last_report = now_ms();
                        let state = if prestart { "waiting" } else { "running" };
                        // Only a real cold path enters the distribution, not a warm re-entry.
                        let cold = (cold.started || cold.loaded).then_some(cold);
                        if let Ok(x) = self.report(inst, &r, state, cold, None).await
                            && x.status == "fenced"
                        {
                            return LaneEnd::Superseded;
                        }
                    }
                    Err(Prep::Artifact(code, definitive)) => {
                        // The artifact's failure on a fresh instance: counted against it,
                        // not as a scorer failure (a reference's counts as one).
                        let body = j!({"lane": r, "job": a.job, "code": code, "definitive": definitive});
                        if let Ok(true) = cohort_call::<bool>(&self.env, inst, "/lane/artifact-failure", &body).await {
                            a.artifact_failed = true;
                            continue;
                        }
                        // Not (yet) attributed: retry on a fresh instance with the same
                        // backoff and streak as a start failure (a reference's failures
                        // before scoring starts are counted nowhere else).
                        let streak: i64 = get_kv(&st, "start_streak").await.unwrap_or(0) + 1;
                        let _ = st.put("start_streak", streak).await;
                        if streak >= i64::from(a.failures_before_pause.max(1)) {
                            self.stop().await;
                            let _ = self.report(inst, &r, "failed", None, Some(code)).await;
                            return LaneEnd::Finished;
                        }
                        return LaneEnd::Rearm((5_000i64 << streak.min(4)) + (js_sys::Math::random() * 5_000.0) as i64);
                    }
                    Err(Prep::Infra(code)) if sc::OBJECT_CODES.contains(&code) => {
                        // An admitted entrant's object absent or changed since
                        // admission recorded it: the coordinator's fault. Its
                        // session is audited and listed for repair; the lane
                        // holds, uncounted, and asks again each admission tick
                        // (without a Container) until the repair clears it. Once
                        // an audit found every object intact the cohort answers
                        // "not held": the read path failed, counted below.
                        let body = j!({"lane": r, "job": a.job});
                        if let Ok(true) = cohort_call::<bool>(&self.env, inst, "/lane/object-missing", &body).await {
                            let _ = st.put("object_held", true).await;
                            self.stop().await;
                            let _ = self.report(inst, &r, "waiting", None, Some(code)).await;
                            return LaneEnd::Rearm(crate::eval::admission::TICK_MS as i64);
                        }
                        let streak: i64 = get_kv(&st, "start_streak").await.unwrap_or(0) + 1;
                        let _ = st.put("start_streak", streak).await;
                        let _ = self.report(inst, &r, "start_failed", None, Some(code)).await;
                        if streak >= i64::from(a.failures_before_pause.max(1)) {
                            self.stop().await;
                            let _ = self.report(inst, &r, "failed", None, Some(code)).await;
                            return LaneEnd::Finished;
                        }
                        let backoff = (5_000i64 << streak.min(4)) + (js_sys::Math::random() * 5_000.0) as i64;
                        return LaneEnd::Rearm(backoff);
                    }
                    Err(Prep::Infra(code)) => {
                        let streak: i64 = get_kv(&st, "start_streak").await.unwrap_or(0) + 1;
                        let _ = st.put("start_streak", streak).await;
                        let _ = self.report(inst, &r, "start_failed", None, Some(code)).await;
                        if streak >= i64::from(a.failures_before_pause.max(1)) {
                            self.stop().await;
                            let _ = self.report(inst, &r, "failed", None, Some(code)).await;
                            return LaneEnd::Finished;
                        }
                        // Capacity or a transient failure: back off with jitter, holding no claim.
                        let backoff = (5_000i64 << streak.min(4)) + (js_sys::Math::random() * 5_000.0) as i64;
                        return LaneEnd::Rearm(backoff);
                    }
                }
            }
            let reply: ClaimReply = match cohort_call(&self.env, inst, "/lane/claim", &j!({"lane": r, "name": k.name})).await {
                Ok(v) => v,
                Err(_) => return LaneEnd::Rearm(5_000),
            };
            match reply {
                ClaimReply::Fenced => return LaneEnd::Superseded,
                ClaimReply::Paused | ClaimReply::Done => {
                    let state = if reply == ClaimReply::Paused { "paused" } else { "done" };
                    self.stop().await;
                    let _ = self.report(inst, &r, state, None, None).await;
                    return LaneEnd::Finished;
                }
                ClaimReply::Wait { ms } => {
                    if prestart && loaded_at.is_some_and(|t| now_ms().saturating_sub(t) > a.prestart_wait_ms) {
                        // Generation stalled past the pre-start bound: nothing waits warm.
                        self.stop().await;
                        let _ = self.report(inst, &r, "yield", None, None).await;
                        return LaneEnd::Finished;
                    }
                    if now_ms().saturating_sub(last_report) >= REPORT_EVERY_MS {
                        last_report = now_ms();
                        let _ = self.report(inst, &r, "waiting", None, None).await;
                    }
                    Delay::from(Duration::from_millis(ms.clamp(200, 5_000))).await;
                }
                ClaimReply::Chunk(c) => {
                    if a.manifest.is_none() || (c.model_error && !a.artifact_failed) {
                        // Pre-started before the seal: the assignment read then had no
                        // manifest digest, which the receipts are bound to. Read it again
                        // while the claim is held (2026-10-31: every pre-started lane
                        // otherwise lost its first claim to the lease). Likewise when the
                        // job's artifact failed since the assignment was read.
                        match self.report(inst, &r, "running", None, None).await {
                            Ok(x) if x.status == "fenced" => return LaneEnd::Superseded,
                            Ok(x) => a = x,
                            Err(_) => {}
                        }
                    }
                    let mut io = WorkerIo { lane: self, inst, r: &r, k, a: &a, art: &art, t0 };
                    let facts = if a.spans {
                        get_kv::<StoredFacts>(&st, "span_facts").await.filter(|f| f.package == art.package_sha256).map(|f| f.facts)
                    } else {
                        None
                    };
                    let cx = ChunkCtx { r: &r, name: &k.name, a: &a, art: &art, seed: &seed, prefix: &prefix, span_facts: facts.as_ref() };
                    match lane_logic::run_chunk(&mut io, &cx, &c, &mut cache).await {
                        ChunkEnd::Next => {}
                        ChunkEnd::Rearm(ms) => return LaneEnd::Rearm(ms as i64),
                        ChunkEnd::Died => return LaneEnd::Finished,
                        ChunkEnd::Throw => return LaneEnd::Throw,
                    }
                    last_report = now_ms();
                }
            }
        }
    }
}

// ------------------------------------------------------------------- EvalLane

/// Generation, summary and bundle lanes, and operator probes. No container.
#[durable_object(alarm)]
pub struct EvalLane {
    state: State,
    env: Env,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct RateProbe {
    model: String,
    rpm: f64,
    until_ms: u64,
    max_tokens: u32,
    started_ms: u64,
    sent: u64,
    ok: u64,
    throttled: u64,
    unavailable: u64,
    errors: u64,
    ms_total: u64,
    ms_max: u64,
    error_texts: Vec<String>,
    per_minute: BTreeMap<u64, (u64, u64)>,
}

fn limiter_stub(env: &Env) -> Option<worker::Stub> {
    let binding = crate::config::binding(GEN_LIMITER, crate::config::NETWORK);
    env.durable_object(&binding).ok()?.get_by_name(&format!("gen-limiter/{}", crate::config::NETWORK)).ok()
}

async fn limiter_post<T: serde::de::DeserializeOwned>(env: &Env, path: &str, body: &Value) -> Option<T> {
    let stub = limiter_stub(env)?;
    let mut r = stub.fetch_with_request(internal_request(Method::Post, path, Some(body.to_string())).ok()?).await.ok()?;
    r.json::<T>().await.ok()
}

impl DurableObject for EvalLane {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let path = req.path();
        match (req.method(), path.as_str()) {
            (Method::Post, "/kick") => {
                let Ok(k) = req.json::<LaneKick>().await else {
                    return error("bad_internal_request", 400);
                };
                json(&j!({"accepted": accept_kick(&self.state, &k).await?}))
            }
            (Method::Post, "/stop") => {
                self.state.storage().put("active", false).await?;
                json(&j!({"stopped": true}))
            }
            (Method::Post, "/sleep") => {
                // M1: a stub call that stays open for `ms`.
                let ms: u64 = req.url()?.query_pairs().find(|(k, _)| k == "ms").and_then(|(_, v)| v.parse().ok()).unwrap_or(2_000);
                Delay::from(Duration::from_millis(ms.min(30_000))).await;
                json(&j!({"slept_ms": ms}))
            }
            (Method::Post, "/probe/start") => {
                let v: Value = req.json().await?;
                let p = RateProbe {
                    model: v["model"].as_str().unwrap_or("").into(),
                    rpm: v["rpm"].as_f64().unwrap_or(60.0).clamp(1.0, 600.0),
                    until_ms: now_ms() + v["seconds"].as_u64().unwrap_or(60).min(900) * 1000,
                    max_tokens: v["max_tokens"].as_u64().unwrap_or(8).clamp(1, 64) as u32,
                    started_ms: now_ms(),
                    ..Default::default()
                };
                if crate::eval::textgen::price(&p.model).is_none() {
                    return error("unpriced_model", 400);
                }
                let st = self.state.storage();
                st.put("probe", &p).await?;
                st.set_alarm(0i64).await?;
                json(&j!({"started": true}))
            }
            (Method::Get, "/probe/state") => json(&get_kv::<RateProbe>(&self.state.storage(), "probe").await),
            (Method::Get, "/state") => {
                let st = self.state.storage();
                json(&j!({"assign": get_kv::<LaneKick>(&st, "assign").await, "active": get_kv::<bool>(&st, "active").await,
                    "do_ms": get_kv::<i64>(&st, "m_do").await}))
            }
            _ => error("not_found", 404),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        let st = self.state.storage();
        let t0 = now_ms();
        let assign: Option<LaneKick> = get_kv(&st, "assign").await;
        let active = get_kv::<bool>(&st, "active").await.unwrap_or(false);
        if let (Some(k), true) = (assign, active) {
            let end = match k.kind.as_str() {
                "gen" => self.gen_loop(&k, t0).await,
                "summary" => self.summary_lane(&k).await,
                "bundle" => self.bundle_lane(&k).await,
                _ => LaneEnd::Finished,
            };
            let m_do: i64 = get_kv(&st, "m_do").await.unwrap_or(0);
            let _ = st.put("m_do", m_do + (now_ms() - t0) as i64).await;
            match end {
                LaneEnd::Rearm(ms) => st.set_alarm(ms.max(0)).await?,
                LaneEnd::Finished => {
                    if !superseded(&self.state, &k).await {
                        st.put("active", false).await?;
                    }
                }
                LaneEnd::Superseded | LaneEnd::Throw => {}
            }
            return Response::ok("lane");
        }
        if let Some(p) = get_kv::<RateProbe>(&st, "probe").await
            && now_ms() < p.until_ms
        {
            self.rate_probe(p).await;
        }
        Response::ok("idle")
    }
}

impl EvalLane {
    async fn report(&self, k: &LaneKick, state: &str, code: Option<&str>) -> std::result::Result<Assignment, &'static str> {
        let do_ms = get_kv::<i64>(&self.state.storage(), "m_do").await.unwrap_or(0) as u64;
        let rep = LaneReport { state: state.into(), do_ms, code: code.map(str::to_owned), ..Default::default() };
        cohort_call(&self.env, k, "/lane/report", &j!({"lane": lane_ref(k), "report": rep})).await
    }

    async fn entry(&self, k: &LaneKick) -> std::result::Result<Assignment, LaneEnd> {
        let a = self.report(k, "starting", None).await.map_err(|_| LaneEnd::Rearm(5_000))?;
        match a.status.as_str() {
            "go" => Ok(a),
            "fenced" => Err(LaneEnd::Superseded),
            s => {
                let _ = self.report(k, if s == "paused" { "paused" } else { "done" }, None).await;
                Err(LaneEnd::Finished)
            }
        }
    }

    async fn gen_loop(&self, k: &LaneKick, t0: u64) -> LaneEnd {
        let a = match self.entry(k).await {
            Ok(a) => a,
            Err(e) => return e,
        };
        let sh = GenShared {
            wants: RefCell::new(None),
            last_report: Cell::new(now_ms()),
            stop: Cell::new(false),
            active: [Cell::new(true), Cell::new(false)],
            events: Cell::new(0),
            wakers: [RefCell::new(None), RefCell::new(None)],
        };
        let end = if a.gen_frontier_lease_ms == 0 {
            // A v1 cohort: Workers AI only, as before.
            self.gen_class(k, t0, &a, &sh, false).await
        } else {
            // Generation v2: the Workers AI and frontier classes run as two
            // loops in this invocation, each claiming, calling and
            // committing only its own items (within the split connection
            // limit, `Assignment::inflight` + `gen_frontier_inflight`), so a
            // Workers AI result never waits uncommitted past its short lease
            // behind a frontier call.
            sh.active[1].set(true);
            let (w, f) = futures_util::future::join(self.gen_class(k, t0, &a, &sh, false), self.gen_class(k, t0, &a, &sh, true)).await;
            GenClassEnd::combine(w, f)
        };
        match end.lane() {
            GenLaneEnd::Rearm(ms) => LaneEnd::Rearm(ms),
            GenLaneEnd::Superseded => LaneEnd::Superseded,
            GenLaneEnd::Terminal(state) => {
                let _ = self.report(k, state, None).await;
                LaneEnd::Finished
            }
        }
    }

    /// One lane class's loop (see [`Self::gen_loop`]); marks the class
    /// inactive when it ends.
    async fn gen_class(&self, k: &LaneKick, t0: u64, a: &Assignment, sh: &GenShared, frontier: bool) -> GenClassEnd {
        let end = self.gen_class_inner(k, t0, a, sh, frontier).await;
        sh.active[usize::from(frontier)].set(false);
        if end.stops_lane() {
            sh.stop.set(true);
        }
        // The other class may be waiting on this one.
        sh.signal();
        end
    }

    async fn gen_class_inner(&self, k: &LaneKick, t0: u64, a: &Assignment, sh: &GenShared, frontier: bool) -> GenClassEnd {
        use crate::eval::lanes::{GenClassWait, frontier_grants, gen_class_models, gen_class_wait};
        let r = lane_ref(k);
        let env = &self.env;
        let other_active = || sh.active[usize::from(!frontier)].get();
        let refund = |grants: BTreeMap<String, u32>| async move {
            for (m, n) in grants.into_iter().filter(|(_, n)| *n > 0) {
                let _ = limiter_post::<Value>(env, "/refund", &j!({"model": m, "n": n})).await;
            }
        };
        let waiting = || async {
            if now_ms().saturating_sub(sh.last_report.get()) >= REPORT_EVERY_MS {
                sh.last_report.set(now_ms());
                let _ = self.report(k, "waiting", None).await;
            }
        };
        if frontier && a.gen_frontier_inflight == 0 {
            // No frontier share of the connection limit.
            return GenClassEnd::Idle;
        }
        let mut round: usize = usize::try_from(k.lane).unwrap_or(0);
        loop {
            // The other class ended the lane (done, paused or superseded).
            if sh.stop.get() {
                return GenClassEnd::Idle;
            }
            // The plan's lane step budget (with one generation lease it fits the alarm wall).
            if now_ms().saturating_sub(t0) > a.step_budget_ms {
                return GenClassEnd::Rearm(0);
            }
            if superseded(&self.state, k).await {
                return GenClassEnd::Superseded;
            }
            let models: Vec<String> = match &*sh.wants.borrow() {
                Some(w) if !w.is_empty() => gen_class_models(w, frontier),
                _ if frontier => Vec::new(),
                _ => a.generators.clone(),
            };
            // This class's grants: the limiter's for Workers AI; for the
            // frontier, its share of the connection limit, and only while a
            // whole frontier lease still fits this invocation's alarm wall.
            // No model of this class with work: an empty claim refreshes
            // the wants.
            let mut wai_grants: BTreeMap<String, u32> = BTreeMap::new();
            let grants: BTreeMap<String, u32> = if models.is_empty() {
                BTreeMap::new()
            } else if frontier {
                let fits = now_ms().saturating_sub(t0).saturating_add(a.gen_frontier_lease_ms) <= crate::eval::plan::ALARM_WALL_MS;
                if !fits {
                    // A fresh invocation has the whole wall for the lease.
                    return GenClassEnd::Rearm(0);
                }
                round = round.wrapping_add(1);
                frontier_grants(sh.wants.borrow().as_ref().unwrap_or(&BTreeMap::new()), a.gen_frontier_inflight, round)
            } else {
                let Some(g) = limiter_post::<Grants>(env, "/acquire", &j!({"models": models, "slots": a.inflight})).await else {
                    return GenClassEnd::Rearm(2_000);
                };
                if g.grants.is_empty() {
                    waiting().await;
                    let wait = g.retry_at_ms.map_or(1_000, |t| t.saturating_sub(now_ms())).clamp(50, 60_000);
                    if wait <= 5_000 {
                        // A short wait for a token holds the invocation (cheaper than an alarm round trip).
                        Delay::from(Duration::from_millis(wait)).await;
                        continue;
                    }
                    // A long wait holds no invocation.
                    return GenClassEnd::Rearm(wait as i64);
                }
                wai_grants = g.grants.clone();
                g.grants
            };
            // The other class's events up to this claim (one during the
            // claim may already have moved an item this reply missed).
            let seen = sh.events.get();
            let reply: GenClaimReply = match cohort_call(env, k, "/gen/claim", &j!({"lane": r, "name": k.name, "grants": grants})).await {
                Ok(v) => v,
                Err(_) => {
                    refund(wai_grants).await;
                    return GenClassEnd::Rearm(5_000);
                }
            };
            match reply {
                GenClaimReply::Items { items, wants: w } => {
                    let mut unused = wai_grants.clone();
                    for it in &items {
                        if let Some(n) = unused.get_mut(&it.generator) {
                            *n = n.saturating_sub(1);
                        }
                    }
                    refund(unused).await;
                    *sh.wants.borrow_mut() = Some(w);
                    let timeout = crate::eval::lanes::gen_call_timeout_ms(a.gen_lease_ms);
                    let done = join_all(items.iter().map(|it| gen_call(env, it, timeout))).await;
                    let feedback: Vec<Feedback> = done
                        .iter()
                        .filter(|(o, _)| !crate::eval::lanes::gen_frontier_model(&o.generator))
                        .map(|(o, e)| Feedback {
                            model: o.generator.clone(),
                            result: match &o.output {
                                Ok(_) => "ok",
                                Err(crate::eval::textgen::Reject::Throttled) => "throttled",
                                Err(crate::eval::textgen::Reject::ModelUnavailable) => "unavailable",
                                Err(crate::eval::textgen::Reject::Transport) => "error",
                                Err(_) => "ok",
                            }
                            .into(),
                            error_text: e.clone(),
                        })
                        .collect();
                    if !feedback.is_empty() {
                        let _ = limiter_post::<Value>(env, "/feedback", &j!(feedback)).await;
                    }
                    let results: Vec<_> = done.into_iter().map(|(o, _)| o).collect();
                    let _ = cohort_call::<Value>(env, k, "/gen/commit", &j!({"lane": r, "results": results})).await;
                    sh.last_report.set(now_ms());
                    // A commit can move items to the other class.
                    sh.signal();
                }
                GenClaimReply::Wait { ms, wants: w } => {
                    refund(wai_grants).await;
                    let step = gen_class_wait(ms, &w, frontier, other_active(), REPORT_EVERY_MS);
                    *sh.wants.borrow_mut() = Some(w);
                    match step {
                        GenClassWait::Claim => continue,
                        GenClassWait::AwaitOther { ms } => {
                            // Nothing for this class while the other class
                            // works: no claim until the other class commits
                            // or ends (items moved to this class, a fallback
                            // or a retry's next model, are claimed then),
                            // or until `ms`, within the step budget.
                            waiting().await;
                            let left = a.step_budget_ms.saturating_sub(now_ms().saturating_sub(t0));
                            let _ = select(Box::pin(sh.changed(frontier, seen)), Box::pin(Delay::from(Duration::from_millis(ms.min(left))))).await;
                            continue;
                        }
                        GenClassWait::Hold { ms } => {
                            waiting().await;
                            Delay::from(Duration::from_millis(ms)).await;
                            continue;
                        }
                        GenClassWait::Rearm { ms } => {
                            waiting().await;
                            return GenClassEnd::Rearm(ms as i64);
                        }
                        GenClassWait::Idle => return GenClassEnd::Idle,
                    }
                }
                GenClaimReply::Done => {
                    refund(wai_grants).await;
                    return GenClassEnd::Terminal("done");
                }
                GenClaimReply::Paused => {
                    refund(wai_grants).await;
                    return GenClassEnd::Terminal("paused");
                }
                GenClaimReply::Fenced => {
                    refund(wai_grants).await;
                    return GenClassEnd::Superseded;
                }
            }
        }
    }

    async fn summary_lane(&self, k: &LaneKick) -> LaneEnd {
        let a = match self.entry(k).await {
            Ok(a) => a,
            Err(e) => return e,
        };
        let (Some(job), Some(art)) = (a.job, a.artifact.as_ref()) else { return LaneEnd::Finished };
        let r = lane_ref(k);
        let mut rows = Vec::new();
        let mut after = -1i64;
        loop {
            let page: Vec<engine::PredRow> =
                match cohort_call(&self.env, k, "/job/preds", &j!({"lane": r, "job": job, "after": after, "limit": 1000})).await {
                    Ok(p) => p,
                    Err(_) => return LaneEnd::Rearm(10_000),
                };
            let n = page.len();
            let last = page.last().map(|p| i64::from(p.task));
            rows.extend(page);
            match last {
                Some(t) if n == 1000 => after = t,
                _ => break,
            }
        }
        let Ok(sm) = engine::summarize_job(job, &art.id, &rows) else { return LaneEnd::Rearm(30_000) };
        let bytes = serde_json::to_vec(&sm).unwrap_or_default();
        let key = lane_logic::summary_key(&a.label, job);
        if put(&self.env, EVAL_BUCKET, &key, bytes.clone()).await.is_err() {
            return LaneEnd::Rearm(10_000);
        }
        let rep = SummaryReport { job, key, sha256: sha_hex(&bytes), count: sm.count, rows_sha256: sm.rows_sha256 };
        match cohort_call::<bool>(&self.env, k, "/lane/summary", &j!({"lane": r, "report": rep})).await {
            Ok(_) => LaneEnd::Finished,
            Err(_) => LaneEnd::Rearm(10_000),
        }
    }

    async fn bundle_lane(&self, k: &LaneKick) -> LaneEnd {
        let a = match self.entry(k).await {
            Ok(a) => a,
            Err(e) => return e,
        };
        let Some(job) = a.job else { return LaneEnd::Finished };
        let r = lane_ref(k);
        let env = &self.env;
        let objects: Vec<(String, String, i64, i64)> = match cohort_call(env, k, "/job/objects", &j!({"lane": r, "job": job})).await {
            Ok(o) => o,
            Err(_) => return LaneEnd::Rearm(10_000),
        };
        let mut bundle = Vec::new();
        for group in objects.chunks(CHUNKS_IN_FLIGHT) {
            let got = join_all(group.iter().map(|(key, sha, _, _)| async move {
                let b = get_bytes(env, RECEIPTS_BUCKET, key).await.ok().flatten()?;
                (sha_hex(&b) == *sha).then_some(b)
            }))
            .await;
            for b in got {
                let Some(b) = b else { return LaneEnd::Rearm(10_000) };
                bundle.extend_from_slice(&b);
            }
        }
        let mut expected = BTreeMap::new();
        let mut after = 0i64;
        loop {
            let page: Vec<(i64, String, String, i64)> =
                match cohort_call(env, k, "/job/receipts", &j!({"lane": r, "job": job, "after": after, "limit": 5000})).await {
                    Ok(p) => p,
                    Err(_) => return LaneEnd::Rearm(10_000),
                };
            for (_, id, sha, len) in &page {
                let (Some(d), Some(s)) = (engine::parse_digest(id), crate::eval::plan::parse_hex32(sha)) else {
                    return LaneEnd::Rearm(30_000);
                };
                expected.insert(d, (s, *len as u32));
            }
            match page.last() {
                Some(p) if page.len() == 5000 => after = p.0,
                _ => break,
            }
        }
        let Ok(n) = crate::eval::receipts::verify_bundle(&bundle, &expected) else { return LaneEnd::Rearm(30_000) };
        let key = lane_logic::bundle_key(&a.label, job);
        if put(env, RECEIPTS_BUCKET, &key, bundle.clone()).await.is_err() {
            return LaneEnd::Rearm(10_000);
        }
        let rep = BundleReport { job, key, sha256: sha_hex(&bundle), bytes: bundle.len() as u64, receipts: n };
        match cohort_call::<bool>(env, k, "/lane/bundle", &j!({"lane": r, "report": rep})).await {
            Ok(_) => LaneEnd::Finished,
            Err(_) => LaneEnd::Rearm(10_000),
        }
    }

    /// M2 and M10: calls one model at a fixed rate until the probe's end,
    /// counting results per minute and keeping the error texts. Calls overlap
    /// (up to 6 in flight) so the rate holds at the model's latency.
    async fn rate_probe(&self, mut p: RateProbe) {
        use futures_util::stream::{FuturesUnordered, StreamExt};
        let env = &self.env;
        let st = self.state.storage();
        let t0 = now_ms();
        let interval = (60_000.0 / p.rpm) as u64;
        let mut next = now_ms();
        let mut inflight = FuturesUnordered::new();
        let make = |k: u64, model: String, max_tokens: u32| async move {
            let it = engine::GenItem {
                idx: k as i64,
                kind: "probe".into(),
                generator: model,
                prompt: crate::eval::textgen::Prompt {
                    system: "Reply with one word.".into(),
                    user: format!("Rate probe {k}: reply with the word ready."),
                    max_tokens,
                },
                attempt: 0,
                claim: 0,
                call: None,
            };
            ev::ai_generate(env, &it, 60_000).await
        };
        while now_ms() < p.until_ms && now_ms().saturating_sub(t0) < 600_000 {
            while next <= now_ms() && inflight.len() < 6 {
                p.sent += 1;
                inflight.push(make(p.sent, p.model.clone(), p.max_tokens));
                next += interval;
            }
            let wait = next.saturating_sub(now_ms()).max(1);
            let done = if inflight.is_empty() {
                Delay::from(Duration::from_millis(wait)).await;
                None
            } else {
                match select(inflight.next(), Box::pin(Delay::from(Duration::from_millis(wait)))).await {
                    Either::Left((v, _)) => v,
                    Either::Right(_) => None,
                }
            };
            if let Some((o, err)) = done {
                let minute = (now_ms() - p.started_ms) / 60_000;
                let slot = p.per_minute.entry(minute).or_default();
                match &o.output {
                    Ok(_) | Err(crate::eval::textgen::Reject::Empty) => {
                        p.ok += 1;
                        slot.0 += 1;
                    }
                    Err(crate::eval::textgen::Reject::Throttled) => {
                        p.throttled += 1;
                        slot.1 += 1;
                    }
                    Err(crate::eval::textgen::Reject::ModelUnavailable) => p.unavailable += 1,
                    Err(_) => p.errors += 1,
                }
                p.ms_total += o.ms;
                p.ms_max = p.ms_max.max(o.ms);
                if let Some(e) = err
                    && p.error_texts.len() < 8
                    && !p.error_texts.contains(&e)
                {
                    p.error_texts.push(e);
                }
                if p.sent.is_multiple_of(20) {
                    let _ = st.put("probe", &p).await;
                }
            }
        }
        while let Some((o, _)) = inflight.next().await {
            match &o.output {
                Err(crate::eval::textgen::Reject::Throttled) => p.throttled += 1,
                Ok(_) => p.ok += 1,
                _ => p.errors += 1,
            }
        }
        let _ = st.put("probe", &p).await;
        if now_ms() < p.until_ms {
            let _ = st.set_alarm(0i64).await;
        }
    }
}

/// The state a v2 generation lane's two class loops share in one
/// invocation (index 0: Workers AI, 1: frontier).
struct GenShared {
    /// The latest claim reply's models with claimable work.
    wants: RefCell<Option<BTreeMap<String, u32>>>,
    last_report: Cell<u64>,
    /// A class ended the lane (done, paused or superseded): the other stops.
    stop: Cell<bool>,
    /// Whether each class's loop is still running.
    active: [Cell<bool>; 2],
    /// Counts the events a waiting class wakes on (a commit, a class's end).
    events: Cell<u64>,
    /// Each class's waker while it waits on the other ([`GenShared::changed`]).
    wakers: [RefCell<Option<std::task::Waker>>; 2],
}

impl GenShared {
    /// Records an event and wakes a waiting class.
    fn signal(&self) {
        self.events.set(self.events.get().wrapping_add(1));
        for w in &self.wakers {
            if let Some(w) = w.borrow_mut().take() {
                w.wake();
            }
        }
    }

    /// Resolves at the first event after `seen` (class `frontier` waiting).
    fn changed(&self, frontier: bool, seen: u64) -> GenChanged<'_> {
        GenChanged { sh: self, me: usize::from(frontier), seen }
    }
}

/// [`GenShared::changed`].
struct GenChanged<'a> {
    sh: &'a GenShared,
    me: usize,
    seen: u64,
}

impl std::future::Future for GenChanged<'_> {
    type Output = ();
    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        if self.sh.events.get() != self.seen {
            return std::task::Poll::Ready(());
        }
        *self.sh.wakers[self.me].borrow_mut() = Some(cx.waker().clone());
        std::task::Poll::Pending
    }
}

/// One generation call of a claimed item: a v1 item through the Workers AI
/// binding as before; a generation v2 item's vendor call (built by the
/// claim) through the generation Worker (frontier) or the binding (Workers
/// AI), under the call's own timeout.
async fn gen_call(env: &Env, it: &engine::GenItem, timeout_ms: u64) -> (engine::GenOutcome, Option<String>) {
    let Some(call) = it.call.as_deref() else {
        return ev::ai_generate(env, it, timeout_ms).await;
    };
    let reply = if call.request.vendor.lane() == slop_ninja_gen_core::vendor::Lane::Frontier {
        super::gen_do::frontier(env, call).await
    } else {
        super::gen_do::workers_ai(env, &call.request, call.timeout_ms).await
    };
    let err = (reply.outcome != slop_ninja_gen_core::outcome::OutcomeCode::Answer).then(|| reply.outcome.code().to_owned());
    (engine::GenOutcome::from_reply(it, reply), err)
}

// ----------------------------------------------------------------- GenLimiter

/// The deployment's shared Workers AI token buckets, in memory; a reset
/// restarts at the configured rates.
#[durable_object(fetch)]
pub struct GenLimiter {
    env: Env,
    limiter: RefCell<Option<Limiter>>,
}

fn limiter_config(env: &Env) -> LimiterConfig {
    let v = |name: &str, d: u64| env.var(name).ok().and_then(|x| x.to_string().trim().parse().ok()).unwrap_or(d);
    LimiterConfig {
        global_rpm: v("GEN_GLOBAL_RPM", 290) as u32,
        model_rpm: v("GEN_MODEL_RPM", 290) as u32,
        burst: v("GEN_BURST", 10) as u32,
        floor_rpm: v("GEN_FLOOR_RPM", 10) as u32,
        raise_after: v("GEN_RAISE_AFTER", 50) as u32,
        penalty_ms: v("GEN_PENALTY_MS", 5_000),
    }
}

impl DurableObject for GenLimiter {
    fn new(_state: State, env: Env) -> Self {
        Self { env, limiter: RefCell::new(None) }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let now = now_ms();
        let path = req.path();
        let body: Value = if req.method() == Method::Post { req.json().await.unwrap_or(Value::Null) } else { Value::Null };
        let mut guard = self.limiter.borrow_mut();
        let l = guard.get_or_insert_with(|| Limiter::new(limiter_config(&self.env), now));
        match path.as_str() {
            "/acquire" => {
                let models: Vec<String> = serde_json::from_value(body["models"].clone()).unwrap_or_default();
                let slots = body["slots"].as_u64().unwrap_or(1).clamp(1, 6) as u32;
                json(&l.acquire(&models, slots, now))
            }
            "/refund" => {
                l.refund(body["model"].as_str().unwrap_or(""), body["n"].as_u64().unwrap_or(0) as u32, now);
                json(&j!({"ok": true}))
            }
            "/feedback" => {
                let f: Vec<Feedback> = serde_json::from_value(body).unwrap_or_default();
                for x in &f {
                    l.feedback(x, now);
                }
                json(&j!({"ok": true}))
            }
            "/status" => json(&*l),
            _ => error("not_found", 404),
        }
    }
}

// ---------------------------------------------------------------- SpendLedger

/// The deployment's spend tracking ledger, shared by both cohort instances.
#[durable_object(fetch)]
pub struct SpendLedger {
    state: State,
    env: Env,
    schema: std::result::Result<u32, &'static str>,
}

impl DurableObject for SpendLedger {
    fn new(state: State, env: Env) -> Self {
        let schema = migrate_storage(&state.storage(), &crate::eval::ledger::LEDGER_SCHEMA);
        Self { state, env, schema }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        if let Err(code) = self.schema {
            return error(code, 503);
        }
        let v = |name: &str| self.env.var(name).ok().and_then(|x| x.to_string().trim().parse::<u64>().ok());
        // The alert threshold is a deployment value (kept out of the tracked
        // config). The ledger only tracks: without a threshold there is no
        // alert, and nothing is refused or left unrecorded.
        let alert = crate::eval::ledger::alert_threshold(v).unwrap_or(u64::MAX);
        let now = now_ms();
        let cycle = crate::eval::ledger::cycle_key(now, v("BILLING_CYCLE_DAY").unwrap_or(1) as u32);
        let storage = self.state.storage();
        let body: Value = if req.method() == Method::Post { req.json().await.unwrap_or(Value::Null) } else { Value::Null };
        let key = body["key"].as_str().unwrap_or("").to_owned();
        let micro = body["microusd"].as_u64().unwrap_or(0);
        let note = body["note"].as_str().unwrap_or("").to_owned();
        let r = match req.path().as_str() {
            "/reserve" if !key.is_empty() => transaction(&storage, move |sql| {
                crate::eval::ledger::reserve(sql, &key, micro, &cycle, alert, now).map(|x| match x {
                    Ok(r) => j!({"reserved": true, "created": r.created, "standing": r.standing}),
                    Err(st) => j!({"reserved": false, "standing": st, "headroom_microusd": st.headroom()}),
                })
            }),
            "/release" if !key.is_empty() => {
                transaction(&storage, move |sql| crate::eval::ledger::release(sql, &key).map(|_| j!({"released": true})))
            }
            "/settle" if !key.is_empty() => transaction(&storage, move |sql| {
                crate::eval::ledger::settle(sql, &key, micro, &cycle, &note, now).map(|_| j!({"settled": true}))
            }),
            "/status" => transaction(&storage, move |sql| crate::eval::ledger::status(sql, &cycle, alert)),
            _ => return error("not_found", 404),
        };
        match r {
            Ok(v) => json(&v),
            Err(_) => error("store_unavailable", 503),
        }
    }
}

// --------------------------------------------------------------- queue consumer

/// The delay of a retried delivery (a kick that did not reach its lane).
const RETRY_DELAY_S: u32 = 5;

/// The lane-wakeup Queue consumer. Each content-free kick reaches its
/// lane's `/kick`, at most 6 at a time; a failed delivery is retried after
/// 5 s. A message that does not parse is acknowledged (the sweep re-sends
/// any kick that was lost). A batch whose messages cannot be read is
/// retried whole, never acknowledged.
pub async fn consume(batch: worker::worker_sys::MessageBatch, env: Env, scope: slop_ninja_arena::ScopeRef) -> Result<()> {
    let _ = scope;
    let Ok(all) = batch.messages() else {
        let _ = batch.retry_all(retry_options(RETRY_DELAY_S));
        return Ok(());
    };
    let mut kicks: Vec<(worker::worker_sys::Message, LaneKick)> = Vec::new();
    for v in all.iter() {
        let m: worker::worker_sys::Message = v.unchecked_into();
        let parsed = m
            .body()
            .ok()
            .and_then(|b| js_sys::JSON::stringify(&b).ok())
            .and_then(|s| s.as_string())
            .and_then(|s| serde_json::from_str::<LaneKick>(&s).ok());
        match parsed {
            Some(k) => kicks.push((m, k)),
            None => {
                let _ = m.ack();
            }
        }
    }
    let retry = || retry_options(RETRY_DELAY_S);
    for group in kicks.chunks(6) {
        let delivered = join_all(group.iter().map(|(_, k)| ev::kick_lane(&env, k))).await;
        for ((m, _), ok) in group.iter().zip(delivered) {
            let _ = if ok { m.ack() } else { m.retry(retry()) };
        }
    }
    Ok(())
}

/// Retry options `{delaySeconds}` as the Queues API takes them.
fn retry_options(delay_s: u32) -> JsValue {
    let o = Object::new();
    let _ = Reflect::set(&o, &JsValue::from_str("delaySeconds"), &JsValue::from(delay_s));
    o.into()
}

/// Content-free state of named lanes (operator diagnostics).
pub async fn lanes_state(env: &Env, names: &[String]) -> Value {
    let mut out = Vec::new();
    for group in names.chunks(6) {
        let got = join_all(group.iter().map(|n| async move {
            let kind = if n.starts_with("a-scorer/") { "score" } else { "gen" };
            let stub = ev::lane_stub(env, kind, n).ok()?;
            let mut r = stub.fetch_with_request(internal_request(Method::Get, "/state", None).ok()?).await.ok()?;
            r.json::<Value>().await.ok()
        }))
        .await;
        for (n, v) in group.iter().zip(got) {
            out.push(j!({"name": n, "state": v}));
        }
    }
    j!(out)
}

/// Rate-probe lanes' state for each model (M2).
pub async fn rate_probe_state(env: &Env, models: &[String], lanes: usize) -> Value {
    let mut out = serde_json::Map::new();
    for (mi, m) in models.iter().enumerate() {
        let mut per = Vec::new();
        for k in 0..lanes {
            let name = format!("eval-lane/{}/probe/rate-{mi}-{k}", crate::config::NETWORK);
            let v = async {
                let stub = ev::lane_stub(env, "gen", &name).ok()?;
                let mut r = stub.fetch_with_request(internal_request(Method::Get, "/probe/state", None).ok()?).await.ok()?;
                r.json::<Value>().await.ok()
            }
            .await;
            per.push(v.unwrap_or(Value::Null));
        }
        out.insert(m.clone(), j!(per));
    }
    Value::Object(out)
}

/// Starts rate-probe lanes: `lanes` per model, each at `rpm / lanes`.
pub async fn rate_probe_start(env: &Env, models: &[String], rpm: f64, seconds: u64, lanes: usize, max_tokens: u32) -> Value {
    let mut started = 0;
    for (mi, m) in models.iter().enumerate() {
        for k in 0..lanes {
            let name = format!("eval-lane/{}/probe/rate-{mi}-{k}", crate::config::NETWORK);
            let body = j!({"model": m, "rpm": rpm / lanes as f64, "seconds": seconds, "max_tokens": max_tokens});
            if let Ok(stub) = ev::lane_stub(env, "gen", &name)
                && let Ok(req) = internal_request(Method::Post, "/probe/start", Some(body.to_string()))
                && stub.fetch_with_request(req).await.is_ok_and(|r| r.status_code() == 200)
            {
                started += 1;
            }
        }
    }
    j!({"started": started})
}
