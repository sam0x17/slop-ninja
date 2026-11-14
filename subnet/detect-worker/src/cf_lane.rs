//! `DetectLane` (wasm32 only): one scorer Container instance and its load.
//!
//! The instance runs the pinned `a-scorer` image (the build identity it
//! reports must equal `SCORER_BINARY_SHA256` and `SCORER_ORT_SHA256`) with
//! Internet access off, one session and two threads, and is loaded once
//! with the reference package from this product's own bucket: the files
//! manifest v2 at `PACKAGE_MANIFEST_KEY`, whose SHA-256 must be
//! `PACKAGE_SHA256` (the scorer's load digest), by the artifact spec
//! `PACKAGE_SPEC_SHA256`. Every file must name the bucket label
//! `PACKAGE_BUCKET_LABEL` (the copy keeps the staged keys and the label,
//! so the manifest bytes and the load digest are the staged package's) and
//! a pinned entity tag; each chunk is a ranged read pinned to that tag and
//! piped to the instance as a stream, so no byte of the package enters this
//! isolate and nothing is hashed.
//!
//! Internal routes, called only by this Worker and the gate:
//! - `/warm`: starts the instance if none runs (the load follows in the
//!   alarm) and answers at once.
//! - `/score?left_ms=N`: one admitted request body, streamed to the
//!   instance's `/v1/score`; the reply returns to the Worker, which applies
//!   the allow-list. A second request while one runs gets 409: the lane holds
//!   no queue of bodies. Answers: 200 the scorer's reply; 502 the instance is
//!   up and ready and its scorer refused this request; 503 the instance could
//!   not be made ready, or failed and was destroyed.
//!
//! **Every wait is bounded** (`waits::Waits`). A score call works within the
//! time the Worker sends, never more than the lane's own bound: a wait for
//! a load running in the alarm, the start (`SCORER_START_WAIT_MS`), the load
//! (`LOAD_WAIT_MS`) and the score call with its reply (`SCORE_WAIT_MS`) are
//! each cut at their own wait and all at that deadline. Every instance call,
//! health checks included, is raced against its stage's end, so a hung
//! instance can hold the lane only until then. A score call that outlasts
//! its bound destroys the instance; one that fails while the instance no
//! longer reports the package ready destroys it too. A start or load that
//! fails destroys the instance; new starts then wait `SCORER_START_WAIT_MS`
//! doubled per consecutive failure, never longer than the idle timeout. Nothing
//! needs an operator to recover, and nothing here logs.
//!
//! **The dev build** (cargo feature `dev`, local dev sessions only): the
//! instance is the stub scorer Worker (`subnet/devtools/stub-scorer`), bound
//! as `STUB_SCORER` in `[env.dev]` only, in place of the Container's port.
//! It is always running; a destroy empties its slot, as a destroyed
//! Container comes back empty, and the next call loads the dev package from
//! the local package bucket through the same protocol. Release builds have
//! no such binding.

use crate::cf::{ms_var, waits};
use crate::cf_io::{self, now_ms};
use crate::waits::Waits;
use crate::{routes, silence_panics};
use futures_util::future::{Either, select};
use std::future::Future;
use js_sys::{Function, Reflect};
use serde_json::Value;
use slop_ninja_artifact_check::files_manifest::{FilesManifest, MAX_MANIFEST_BYTES};
use slop_ninja_scorer_client::{self as sc, Entry, Ours, Prep, Slot};
use std::cell::Cell;
use std::time::Duration;
use wasm_bindgen::{JsCast, JsValue};
use worker::{
    ContainerStartupOptions, Delay, DurableObject, Env, Fetcher, Headers, Method, Request, RequestInit, Response, ResponseBody, Result, State,
    durable_object,
};

const PORT: u16 = 8080;
/// The package bucket.
const PACKAGES: &str = "DETECT_PACKAGES_TESTNET";
/// One session fits standard-3's memory under the scorer's structural bound;
/// two do not. Two threads use the instance's two vCPUs.
const SESSIONS: &str = "1";
const THREADS: &str = "2";
/// One chunk: the scorer's largest transfer (the spec's part size).
const CHUNK_BYTES: u64 = slop_ninja_artifact_check::spec::PART_BYTES;
/// How often a waiting start or load asks the instance again.
const POLL_MS: u64 = 250;
/// The dev build's instance (`[env.dev]` only): the stub scorer Worker.
#[cfg(feature = "dev")]
const STUB_SCORER: &str = "STUB_SCORER";

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

/// Races `fut` against `until` (absolute, `now_ms` clock). `None`: the
/// deadline came first (the instance call is abandoned).
async fn within<T>(until: u64, fut: impl Future<Output = T>) -> Option<T> {
    let left = until.saturating_sub(now_ms());
    if left == 0 {
        return None;
    }
    match select(Box::pin(fut), Box::pin(Delay::from(Duration::from_millis(left)))).await {
        Either::Left((v, _)) => Some(v),
        Either::Right(_) => None,
    }
}

/// The instance's health report, if it answers before `until`.
async fn healthz(port: &Fetcher, until: u64) -> Option<Value> {
    within(until, async {
        let mut r = port_fetch(port, Method::Get, "/healthz", None, None).await.ok()?;
        if r.status_code() != 200 {
            cf_io::discard(&mut r);
            return None;
        }
        cf_io::response_json::<Value>(&mut r, 64 * 1024).await.ok()
    })
    .await
    .flatten()
}

/// How a score call ended without a reply.
enum CallFail {
    /// The call or its reply body failed in transport.
    Transport,
    /// The scorer answered with this status.
    Status(u16),
}

async fn sleep(ms: u64) {
    Delay::from(Duration::from_millis(ms)).await;
}

/// The lane's deployment: what it loads and how long it waits.
struct Plan {
    package: String,
    manifest_key: String,
    spec: String,
    label: String,
    binary: Option<String>,
    ort: Option<String>,
    waits: Waits,
    idle_ms: u64,
}

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string().trim().to_owned()).filter(|v| !v.is_empty())
}

impl Plan {
    fn of(env: &Env) -> Option<Plan> {
        Some(Plan {
            package: var(env, "PACKAGE_SHA256")?,
            manifest_key: var(env, "PACKAGE_MANIFEST_KEY")?,
            spec: var(env, "PACKAGE_SPEC_SHA256")?,
            label: var(env, "PACKAGE_BUCKET_LABEL")?,
            binary: var(env, "SCORER_BINARY_SHA256"),
            ort: var(env, "SCORER_ORT_SHA256"),
            waits: waits(env),
            idle_ms: ms_var(env, "IDLE_MS").unwrap_or(600_000),
        })
    }
}

#[durable_object(alarm)]
pub struct DetectLane {
    state: State,
    env: Env,
    busy: Cell<bool>,
    loading: Cell<bool>,
    failures: Cell<u32>,
    retry_at: Cell<u64>,
}

/// Clears a flag when the holder ends, however it ends.
struct Flag<'a>(&'a Cell<bool>);

impl Drop for Flag<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl DetectLane {
    /// The dev build's stub scorer, when the dev session binds it.
    #[cfg(feature = "dev")]
    fn stub(&self) -> Option<Fetcher> {
        self.env.service(STUB_SCORER).ok()
    }

    /// Whether an instance runs (the dev stub always does).
    fn running(&self) -> bool {
        #[cfg(feature = "dev")]
        if self.stub().is_some() {
            return true;
        }
        self.state.container().is_some_and(|c| c.running())
    }

    /// The instance's port: the Container's, or the dev build's stub.
    fn port(&self) -> Option<Fetcher> {
        #[cfg(feature = "dev")]
        if let Some(stub) = self.stub() {
            return Some(stub);
        }
        self.state.container()?.get_tcp_port(PORT).ok()
    }

    /// Whether this deployment has an instance binding at all.
    fn bound(&self) -> bool {
        #[cfg(feature = "dev")]
        if self.stub().is_some() {
            return true;
        }
        self.state.container().is_some()
    }

    fn may_start(&self) -> bool {
        now_ms() >= self.retry_at.get()
    }

    fn failed(&self, plan: &Plan) {
        let n = self.failures.get().saturating_add(1);
        self.failures.set(n);
        let wait = plan.waits.start_ms.saturating_mul(1u64 << n.saturating_sub(1).min(16)).min(plan.idle_ms);
        self.retry_at.set(now_ms().saturating_add(wait));
    }

    async fn destroy(&self) {
        #[cfg(feature = "dev")]
        if let Some(stub) = self.stub() {
            let until = now_ms().saturating_add(POLL_MS * 4);
            let call = port_fetch(&stub, Method::Post, "/__stub/destroy", None, None);
            if let Some(Ok(mut r)) = within(until, call).await {
                cf_io::discard(&mut r);
            }
            return;
        }
        if let Some(c) = self.state.container()
            && c.running()
        {
            let _ = c.destroy(Some("detect lane")).await;
        }
    }

    async fn set_inactivity(&self, ms: u64) {
        #[cfg(feature = "dev")]
        if self.stub().is_some() {
            return;
        }
        if let Some(c) = self.state.container()
            && let Ok(v) = container_call(c.as_ref(), "setInactivityTimeout", &JsValue::from_f64(ms as f64))
            && let Ok(p) = v.dyn_into::<js_sys::Promise>()
        {
            let _ = wasm_bindgen_futures::JsFuture::from(p).await;
        }
    }

    /// Starts the instance if none runs (Internet off, one session, two
    /// threads). `false`: no binding, or a start is not allowed yet.
    async fn start(&self, plan: &Plan) -> bool {
        if self.running() {
            return true;
        }
        let Some(c) = self.state.container() else { return false };
        if !self.may_start() {
            return false;
        }
        let mut opts = ContainerStartupOptions::new();
        opts.enable_internet(false);
        opts.add_env("A_SESSIONS", SESSIONS);
        opts.add_env("A_THREADS", THREADS);
        if c.start(Some(opts)).is_err() {
            self.failed(plan);
            return false;
        }
        self.set_inactivity(plan.idle_ms).await;
        true
    }

    /// Destroys the instance and counts a failed start or load.
    async fn give_up(&self, plan: &Plan) {
        self.destroy().await;
        self.failed(plan);
    }

    /// Makes the instance ready with the package, starting and loading as
    /// needed: the start within `SCORER_START_WAIT_MS`, the load within
    /// `LOAD_WAIT_MS`, both cut at `deadline`.
    async fn prepare(&self, plan: &Plan, deadline: u64) -> std::result::Result<Fetcher, &'static str> {
        if !self.bound() {
            return Err("no container binding");
        }
        let w = plan.waits;
        if self.running() {
            let h = match self.port() {
                Some(port) => healthz(&port, Waits::stage_until(now_ms(), w.start_ms, deadline)).await,
                None => None,
            };
            if !matches!(sc::entry(h.as_ref(), &plan.package, None, Some(&plan.spec)), Entry::Keep) && h.is_some() {
                self.destroy().await;
            }
        }
        if !self.start(plan).await {
            return Err("start");
        }
        let port = self.port().ok_or("container port")?;
        let until = Waits::stage_until(now_ms(), w.start_ms, deadline);
        let h = loop {
            if let Some(h) = healthz(&port, until).await {
                break h;
            }
            if now_ms() >= until {
                self.give_up(plan).await;
                return Err("container did not answer");
            }
            sleep(POLL_MS).await;
        };
        // Build identity: the binary and runtime library the pinned digests name.
        for (field, want) in [("/build/binary_sha256", &plan.binary), ("/build/ort_sha256", &plan.ort)] {
            if let Some(w) = want
                && h.pointer(field).and_then(Value::as_str) != Some(w.as_str())
            {
                self.give_up(plan).await;
                return Err("scorer build identity");
            }
        }
        let n = |p: &str| h.pointer(p).and_then(Value::as_u64);
        if n("/build/sessions") != Some(1) || n("/build/threads") != Some(2) {
            self.give_up(plan).await;
            return Err("scorer sessions or threads");
        }
        let spec = match sc::spec_sent(&h, Some(&plan.spec)) {
            Ok(s) => s,
            Err(_) => {
                self.give_up(plan).await;
                return Err(sc::SPEC_NOT_SERVED);
            }
        };
        let until = Waits::stage_until(now_ms(), w.load_ms, deadline);
        let loaded = match sc::slot(&h, &plan.package, None, spec) {
            Slot::Ours(Ours::Ready) => Ok(()),
            Slot::Ours(Ours::Receiving | Ours::Loading) => self.wait_ready(&port, plan, spec, until).await,
            _ => self.load(&port, plan, spec, until).await.map_err(|e| e.code()),
        };
        if let Err(e) = loaded {
            self.give_up(plan).await;
            return Err(e);
        }
        self.failures.set(0);
        self.retry_at.set(0);
        Ok(port)
    }

    async fn wait_ready(&self, port: &Fetcher, plan: &Plan, spec: Option<&str>, until: u64) -> std::result::Result<(), &'static str> {
        while now_ms() < until {
            if let Some(h) = healthz(port, until).await {
                match sc::slot(&h, &plan.package, None, spec) {
                    Slot::Ours(Ours::Ready) => return Ok(()),
                    Slot::Ours(Ours::Failed) => return Err("load failed"),
                    _ => {}
                }
            }
            sleep(POLL_MS * 2).await;
        }
        Err("load outlasted its wait")
    }

    /// The load: manifest, missing chunks as pinned ranged reads streamed
    /// to their offsets, then the load and a bounded wait for ready.
    async fn load(&self, port: &Fetcher, plan: &Plan, spec: Option<&str>, deadline: u64) -> std::result::Result<(), Prep> {
        let bucket = self.env.bucket(PACKAGES).map_err(|_| Prep::Infra("package bucket"))?;
        let manifest = match cf_io::r2_bytes(&bucket, &plan.manifest_key, MAX_MANIFEST_BYTES, Some(deadline)).await {
            Ok(Some(m)) => m,
            Ok(None) => return Err(Prep::Infra("package manifest missing")),
            Err(_) => return Err(Prep::Infra("package manifest read")),
        };
        if hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&manifest)) != plan.package {
            return Err(Prep::Infra("package manifest digest"));
        }
        let files: FilesManifest = serde_json::from_slice(&manifest).map_err(|_| Prep::Infra("package manifest json"))?;
        if files.files.iter().any(|f| f.bucket != plan.label || f.etag.is_none()) {
            return Err(Prep::Infra("package file outside the bound bucket or unpinned"));
        }
        let prepared = within(deadline, async {
            let mut r = port_fetch(
                port,
                Method::Post,
                &sc::prepare_path(&plan.package, None, spec),
                Some(js_sys::Uint8Array::from(manifest.as_slice()).into()),
                Some(manifest.len() as u64),
            )
            .await
            .map_err(|_| Prep::Infra("prepare call"))?;
            let status = r.status_code();
            let body = cf_io::response_json::<Value>(&mut r, 1 << 20).await.unwrap_or(Value::Null);
            Ok::<_, Prep>((status, body))
        })
        .await
        .ok_or(Prep::Infra("prepare call timeout"))?;
        let (status, body) = prepared?;
        let missing = sc::prepare_reply(status, &body)?;
        drop(body);
        for (index, offset) in missing {
            let f = files.files.get(index).ok_or(Prep::Infra("chunk index"))?;
            let len = f.bytes.checked_sub(offset).filter(|l| *l > 0).ok_or(Prep::Infra("chunk offset"))?.min(CHUNK_BYTES);
            let etag = f.etag.clone().ok_or(Prep::Infra("chunk etag"))?;
            let get = bucket.get(f.key.clone()).range(worker::Range::OffsetWithLength { offset, length: len }).only_if(
                worker::Conditional { etag_matches: Some(etag), etag_does_not_match: None, uploaded_before: None, uploaded_after: None },
            );
            let obj = match within(deadline, get.execute()).await {
                None => return Err(Prep::Infra("package stream timeout")),
                Some(Err(_)) => return Err(Prep::Infra("chunk read")),
                Some(Ok(None)) => return Err(Prep::Infra(sc::OBJECT_MISSING)),
                Some(Ok(Some(o))) => o,
            };
            let Some(body) = obj.body() else { return Err(Prep::Infra(sc::OBJECT_CHANGED)) };
            let stream = match body.response_body().map_err(|_| Prep::Infra("chunk body"))? {
                ResponseBody::Stream(s) => s,
                _ => return Err(Prep::Infra("chunk stream")),
            };
            let path = sc::chunk_path(index, offset);
            let put = port_fetch(port, Method::Put, &path, Some(stream.into()), Some(len));
            let mut r = match within(deadline, put).await {
                Some(r) => r.map_err(|_| Prep::Infra("chunk upload"))?,
                None => return Err(Prep::Infra("package stream timeout")),
            };
            let code = r.status_code();
            cf_io::discard(&mut r);
            if !matches!(code, 200 | 201) {
                return Err(sc::chunk_refused(code));
            }
        }
        let started = within(deadline, async {
            let mut r = port_fetch(port, Method::Post, &sc::load_path(&plan.package, None, spec), None, None)
                .await
                .map_err(|_| Prep::Infra("load call"))?;
            let status = r.status_code();
            if !matches!(status, 200 | 202) {
                let body = cf_io::response_json::<Value>(&mut r, 1 << 20).await.unwrap_or(Value::Null);
                return Err(sc::load_reply(status, &body).unwrap_or(Prep::Infra("load refused")));
            }
            cf_io::discard(&mut r);
            Ok(())
        })
        .await
        .ok_or(Prep::Infra("load call timeout"))?;
        started?;
        self.wait_ready(port, plan, spec, deadline).await.map_err(Prep::Infra)
    }

    /// After a failed score call: keeps an instance that still reports the
    /// package ready (the request failed, not the instance) and destroys any
    /// other. `true`: kept.
    async fn after_failure(&self, port: &Fetcher, plan: &Plan, until: u64) -> bool {
        if let Some(h) = healthz(port, until).await
            && sc::slot(&h, &plan.package, None, Some(&plan.spec)) == Slot::Ours(Ours::Ready)
        {
            return true;
        }
        self.destroy().await;
        false
    }

    async fn score(&self, req: Request) -> Result<Response> {
        let Some(plan) = Plan::of(&self.env) else { return Ok(Response::empty()?.with_status(503)) };
        if self.busy.get() {
            return Ok(Response::empty()?.with_status(409));
        }
        self.busy.set(true);
        let _busy = Flag(&self.busy);
        let w = plan.waits;
        let left = req.url().ok().and_then(|u| u.query_pairs().find(|(k, _)| k == "left_ms").and_then(|(_, v)| v.parse::<u64>().ok()));
        let deadline = w.lane_deadline(now_ms(), left);
        // A warm start may be loading in the alarm: wait for it rather
        // than load twice, within the same deadline.
        while self.loading.get() && now_ms() < deadline {
            sleep(POLL_MS * 2).await;
        }
        if self.loading.get() {
            return Ok(Response::empty()?.with_status(503));
        }
        let port = match self.prepare(&plan, deadline).await {
            Ok(p) => p,
            Err(_) => return Ok(Response::empty()?.with_status(503)),
        };
        let until = Waits::stage_until(now_ms(), w.score_ms, deadline);
        let len = req.headers().get("content-length").ok().flatten().and_then(|v| v.trim().parse::<u64>().ok());
        let body = req.inner().body().map(JsValue::from);
        let call = async {
            let mut r = port_fetch(&port, Method::Post, "/v1/score", body, len).await.map_err(|_| CallFail::Transport)?;
            let code = r.status_code();
            if code != 200 {
                cf_io::discard(&mut r);
                return Err(CallFail::Status(code));
            }
            cf_io::response_bytes(&mut r, routes::REPLY_BYTES).await.map_err(|_| CallFail::Transport)
        };
        match within(until, call).await {
            Some(Ok(bytes)) => {
                let mut out = Response::from_bytes(bytes)?;
                out.headers_mut().set("Content-Type", "application/json")?;
                Ok(out)
            }
            // The scorer refused this request itself: the instance stays.
            Some(Err(CallFail::Status(400 | 404 | 413 | 422))) => Ok(Response::empty()?.with_status(502)),
            Some(Err(_)) => {
                let kept = self.after_failure(&port, &plan, deadline).await;
                Ok(Response::empty()?.with_status(if kept { 502 } else { 503 }))
            }
            None => {
                // Outlasted its bound: a hung instance is destroyed.
                self.destroy().await;
                Ok(Response::empty()?.with_status(503))
            }
        }
    }
}

impl DurableObject for DetectLane {
    fn new(state: State, env: Env) -> Self {
        silence_panics();
        Self { state, env, busy: Cell::new(false), loading: Cell::new(false), failures: Cell::new(0), retry_at: Cell::new(0) }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        silence_panics();
        if req.method() != Method::Post {
            return Ok(Response::empty()?.with_status(405));
        }
        match req.path().as_str() {
            "/warm" => {
                let Some(plan) = Plan::of(&self.env) else { return Ok(Response::empty()?.with_status(503)) };
                if !self.start(&plan).await {
                    return Ok(Response::empty()?.with_status(503));
                }
                self.state.storage().set_alarm(0i64).await?;
                Ok(Response::empty()?.with_status(202))
            }
            "/score" => self.score(req).await,
            _ => Ok(Response::empty()?.with_status(404)),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        silence_panics();
        let Some(plan) = Plan::of(&self.env) else { return Response::empty() };
        if self.busy.get() || self.loading.get() {
            return Response::empty();
        }
        if self.running() {
            self.loading.set(true);
            let _loading = Flag(&self.loading);
            let w = plan.waits;
            let deadline = now_ms().saturating_add(w.start_ms).saturating_add(w.load_ms);
            let _ = self.prepare(&plan, deadline).await;
        }
        Response::empty()
    }
}
