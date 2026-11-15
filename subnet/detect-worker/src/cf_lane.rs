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
//! **The start options** ([`crate::lane`]) are a plain object with own
//! `enableInternet` and `env` properties, each set through `Reflect`, and
//! the Container's `start` is called through `Reflect` with it, as the
//! coordinator's evaluation lanes call it. `worker`'s typed startup options
//! are not used: their fields are accessors on a class prototype, with `env`
//! a `Map` that has no own keys, so the instance would not receive its
//! sessions and threads, would run the image's defaults, and the lane would
//! refuse it.
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
//! fails destroys the instance. A start of a fresh instance that fails is
//! tried once more in the same request, within its deadline, unless a new
//! instance would fail the same way ([`lane::again_after`]); after that, new
//! starts wait `SCORER_START_WAIT_MS` doubled per consecutive failure, never
//! longer than the idle timeout. Nothing needs an operator to recover, and
//! nothing here logs.
//!
//! **The instance is kept for `IDLE_MS`** after the last scan, or after its
//! start when no scan has used it yet ([`lane::keep_until`]). While it is
//! kept, the alarm runs at least every [`lane::KEEPALIVE_MS`]
//! ([`lane::keep`]): each run sets the Container's inactivity timeout to the
//! time left and asks the instance's health, so neither the object nor the
//! instance is idle meanwhile, and the run after the time is up destroys
//! the instance. A score or a load in flight holds the instance itself; the
//! alarm then only comes back. Warms never extend the time; only scans do.
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
use crate::lane::{self, ENV_FIELD, INTERNET_FIELD, Failed, Keep, StartOptions};
use crate::waits::Waits;
use crate::{routes, silence_panics};
use futures_util::future::{Either, select};
use std::future::Future;
use js_sys::{Function, Object, Reflect};
use serde_json::Value;
use slop_ninja_artifact_check::files_manifest::{FilesManifest, MAX_MANIFEST_BYTES};
use slop_ninja_scorer_client::{self as sc, Entry, Ours, Prep, Slot};
use std::cell::Cell;
use std::time::Duration;
use wasm_bindgen::{JsCast, JsValue};
use worker::{
    Delay, DurableObject, Env, Fetcher, Headers, Method, Request, RequestInit, Response, ResponseBody, Result, State,
    durable_object,
};

const PORT: u16 = 8080;
/// The package bucket.
const PACKAGES: &str = "DETECT_PACKAGES_TESTNET";
/// One chunk: the scorer's largest transfer (the spec's part size).
const CHUNK_BYTES: u64 = slop_ninja_artifact_check::spec::PART_BYTES;
/// How often a waiting start or load asks the instance again.
const POLL_MS: u64 = 250;
/// The pause before a fresh start is tried again, and the bound on the
/// keepalive's health ask.
const SETTLE_MS: u64 = POLL_MS * 4;
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

/// Sets one own property of a plain object.
fn put(o: &Object, key: &str, value: &JsValue) -> std::result::Result<(), &'static str> {
    match Reflect::set(o, &JsValue::from_str(key), value) {
        Ok(true) => Ok(()),
        _ => Err("start options"),
    }
}

/// The start options as the plain object the runtime reads
/// ([`StartOptions::to_json`]): own `enableInternet` and `env` properties,
/// `env` a plain object of strings, each set through `Reflect`.
fn start_object(o: &StartOptions) -> std::result::Result<Object, &'static str> {
    let env = Object::new();
    for (key, value) in &o.env {
        put(&env, key, &JsValue::from_str(value))?;
    }
    let opts = Object::new();
    put(&opts, INTERNET_FIELD, &JsValue::from_bool(o.enable_internet))?;
    put(&opts, ENV_FIELD, &env)?;
    Ok(opts)
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
    /// The alarm is destroying an instance whose idle time is up.
    stopping: Cell<bool>,
    /// A warm started the instance: the alarm loads it.
    warm_load: Cell<bool>,
    failures: Cell<u32>,
    retry_at: Cell<u64>,
    /// The instance is kept until this time (0: none is kept).
    until: Cell<u64>,
}

/// How a start ended.
enum Start {
    /// An instance already runs.
    Running,
    /// This call started a fresh instance.
    Started,
    /// No start is allowed yet (the backoff after failures), or there is no
    /// binding: nothing was started.
    NotYet,
    /// The start call failed.
    Failed,
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
        self.until.set(0);
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
    /// threads: [`lane::start_options`]). A fresh instance is kept for the
    /// idle time from now. `again`: the one retry of a fresh start that
    /// just failed, which the backoff does not hold back.
    fn start(&self, plan: &Plan, again: bool) -> Start {
        if self.running() {
            return Start::Running;
        }
        let Some(c) = self.state.container() else { return Start::NotYet };
        if !again && !self.may_start() {
            return Start::NotYet;
        }
        let started = start_object(&lane::start_options()).and_then(|o| container_call(c.as_ref(), "start", &o));
        if started.is_err() {
            self.failed(plan);
            return Start::Failed;
        }
        self.until.set(lane::keep_until(now_ms(), plan.idle_ms, plan.waits.hold_ms));
        Start::Started
    }

    /// The time left of the instance's keep, at least 1 ms (the runtime
    /// refuses a timeout of 0).
    fn left(&self) -> u64 {
        self.until.get().saturating_sub(now_ms()).max(1)
    }

    /// Schedules the alarm `ms` from now (replacing any scheduled one).
    async fn arm(&self, ms: u64) {
        let _ = self.state.storage().set_alarm(i64::try_from(ms).unwrap_or(i64::MAX)).await;
    }

    /// A scan used the instance: it is kept for the idle time from now, and
    /// the keepalive runs.
    async fn kept(&self, plan: &Plan) {
        self.until.set(lane::keep_until(now_ms(), plan.idle_ms, plan.waits.hold_ms));
        self.set_inactivity(self.left()).await;
        self.arm(lane::KEEPALIVE_MS).await;
    }

    /// One keepalive run of the alarm ([`lane::keep`]).
    async fn tick(&self, plan: &Plan) {
        // A scan or a load in flight holds the instance and sets its time
        // when it ends; the alarm only comes back.
        if self.busy.get() || self.loading.get() {
            self.arm(lane::KEEPALIVE_MS).await;
            return;
        }
        if !self.running() {
            self.until.set(0);
            return;
        }
        if self.until.get() == 0 {
            // A running instance this object keeps no time for (the object
            // was evicted and came back while it ran): kept as if just used.
            self.until.set(lane::keep_until(now_ms(), plan.idle_ms, plan.waits.hold_ms));
        }
        match lane::keep(now_ms(), self.until.get()) {
            Keep::Stop => {
                self.stopping.set(true);
                let _stopping = Flag(&self.stopping);
                self.destroy().await;
            }
            Keep::Tick { sleep_ms, next_ms } => {
                self.set_inactivity(sleep_ms).await;
                if let Some(port) = self.port() {
                    let _ = healthz(&port, now_ms().saturating_add(SETTLE_MS)).await;
                }
                self.arm(next_ms).await;
            }
        }
    }

    /// Waits, until `deadline`, for the alarm's load or stop to end.
    /// `false`: one is still running.
    async fn settled(&self, deadline: u64) -> bool {
        while (self.loading.get() || self.stopping.get()) && now_ms() < deadline {
            sleep(POLL_MS * 2).await;
        }
        !(self.loading.get() || self.stopping.get())
    }

    /// Destroys the instance and counts a failed start or load.
    async fn give_up(&self, plan: &Plan) {
        self.destroy().await;
        self.failed(plan);
    }

    /// Makes the instance ready with the package, starting and loading as
    /// needed: the start within `SCORER_START_WAIT_MS`, the load within
    /// `LOAD_WAIT_MS`, both cut at `deadline`. A fresh start that fails is
    /// tried once more, after a short pause, while the deadline allows
    /// ([`lane::again_after`]).
    async fn prepare(&self, plan: &Plan, deadline: u64) -> std::result::Result<Fetcher, &'static str> {
        let mut tried = false;
        loop {
            match self.prepare_once(plan, deadline, tried).await {
                Ok(port) => return Ok(port),
                Err((f, fresh)) => {
                    if !lane::again_after(f, fresh, tried, now_ms(), deadline) {
                        return Err(f.code);
                    }
                    tried = true;
                    sleep(SETTLE_MS).await;
                }
            }
        }
    }

    /// One attempt of [`Self::prepare`]. The error says why, and whether
    /// this attempt started a fresh instance. `again`: the retry, whose start
    /// the backoff does not hold back.
    async fn prepare_once(
        &self,
        plan: &Plan,
        deadline: u64,
        again: bool,
    ) -> std::result::Result<Fetcher, (Failed, bool)> {
        if !self.bound() {
            return Err((Failed::fixed("no container binding"), false));
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
        let fresh = match self.start(plan, again) {
            Start::Running => false,
            Start::Started => true,
            Start::NotYet => return Err((Failed::transient("start"), false)),
            Start::Failed => return Err((Failed::transient("start"), true)),
        };
        let Some(port) = self.port() else { return Err((Failed::transient("container port"), fresh)) };
        let until = Waits::stage_until(now_ms(), w.start_ms, deadline);
        let h = loop {
            if let Some(h) = healthz(&port, until).await {
                break h;
            }
            if now_ms() >= until {
                self.give_up(plan).await;
                return Err((Failed::transient("container did not answer"), fresh));
            }
            sleep(POLL_MS).await;
        };
        // The instance runs: the runtime's own sleep timer gets the time left.
        self.set_inactivity(self.left()).await;
        // Build identity: the binary and runtime library the pinned digests name.
        for (field, want) in [("/build/binary_sha256", &plan.binary), ("/build/ort_sha256", &plan.ort)] {
            if let Some(w) = want
                && h.pointer(field).and_then(Value::as_str) != Some(w.as_str())
            {
                self.give_up(plan).await;
                return Err((Failed::fixed("scorer build identity"), fresh));
            }
        }
        // The sessions and threads the start passed (an instance that did
        // not receive them runs the image's defaults).
        if !lane::runs_ours(&h) {
            self.give_up(plan).await;
            return Err((Failed::fixed("scorer sessions or threads"), fresh));
        }
        let spec = match sc::spec_sent(&h, Some(&plan.spec)) {
            Ok(s) => s,
            Err(_) => {
                self.give_up(plan).await;
                return Err((Failed::fixed(sc::SPEC_NOT_SERVED), fresh));
            }
        };
        let until = Waits::stage_until(now_ms(), w.load_ms, deadline);
        let loaded = match sc::slot(&h, &plan.package, None, spec) {
            Slot::Ours(Ours::Ready) => Ok(()),
            Slot::Ours(Ours::Receiving | Ours::Loading) => {
                self.wait_ready(&port, plan, spec, until).await.map_err(Failed::transient)
            }
            _ => self.load(&port, plan, spec, until).await.map_err(|e| match e {
                Prep::Infra(code) => Failed::of_load(code, false),
                Prep::Artifact(code, definitive) => Failed::of_load(code, definitive),
            }),
        };
        if let Err(e) = loaded {
            self.give_up(plan).await;
            return Err((e, fresh));
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
        // A warm start may be loading in the alarm, or the alarm may be
        // stopping an instance whose time is up: wait for it rather than
        // load twice or start beside a stopping one, within the same
        // deadline.
        if !self.settled(deadline).await {
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
                self.kept(&plan).await;
                let mut out = Response::from_bytes(bytes)?;
                out.headers_mut().set("Content-Type", "application/json")?;
                Ok(out)
            }
            // The scorer refused this request itself: the instance stays.
            Some(Err(CallFail::Status(400 | 404 | 413 | 422))) => {
                self.kept(&plan).await;
                Ok(Response::empty()?.with_status(502))
            }
            Some(Err(_)) => {
                let kept = self.after_failure(&port, &plan, deadline).await;
                if kept {
                    self.kept(&plan).await;
                }
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
        Self {
            state,
            env,
            busy: Cell::new(false),
            loading: Cell::new(false),
            stopping: Cell::new(false),
            warm_load: Cell::new(false),
            failures: Cell::new(0),
            retry_at: Cell::new(0),
            until: Cell::new(0),
        }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        silence_panics();
        if req.method() != Method::Post {
            return Ok(Response::empty()?.with_status(405));
        }
        match req.path().as_str() {
            "/warm" => {
                let Some(plan) = Plan::of(&self.env) else { return Ok(Response::empty()?.with_status(503)) };
                // An instance the alarm is stopping is not started over.
                if !self.settled(now_ms().saturating_add(plan.waits.hold_ms)).await {
                    return Ok(Response::empty()?.with_status(503));
                }
                // The load follows in the alarm (a ready instance needs none).
                // A warm never extends the time the lane keeps the instance:
                // a fresh one is kept from its start, a running one as it was.
                match self.start(&plan, false) {
                    Start::Started | Start::Running => {
                        self.warm_load.set(true);
                        self.state.storage().set_alarm(0i64).await?;
                    }
                    Start::NotYet | Start::Failed => return Ok(Response::empty()?.with_status(503)),
                }
                Ok(Response::empty()?.with_status(202))
            }
            "/score" => self.score(req).await,
            _ => Ok(Response::empty()?.with_status(404)),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        silence_panics();
        let Some(plan) = Plan::of(&self.env) else { return Response::empty() };
        // A warm's load (a scan in flight loads the instance itself, and the
        // next run finds it ready).
        if self.warm_load.get() && !self.busy.get() && !self.loading.get() {
            self.warm_load.set(false);
            if self.running() {
                self.loading.set(true);
                let _loading = Flag(&self.loading);
                let w = plan.waits;
                let deadline = now_ms().saturating_add(w.start_ms).saturating_add(w.load_ms);
                let _ = self.prepare(&plan, deadline).await;
            }
        }
        self.tick(&plan).await;
        Response::empty()
    }
}
