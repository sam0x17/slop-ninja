//! The detect Worker on Cloudflare (wasm32 only): its `fetch` export.
//!
//! Routes (reached only through the website Worker's service binding):
//! - `POST /api/detect/v1/warm`: content-free. Answered from this isolate's
//!   awake hint while it holds; otherwise, at most once per Cloudflare
//!   location per rate-limit period (a constant-keyed rate-limit binding),
//!   forwarded to the gate, which starts the one instance only if none is
//!   awake. Every other warm is answered "starting" with no forward.
//! - `POST /api/detect/v1/score`: `{"text","turnstile"}`. In order: content
//!   type; the tool switch; a declared length within the body bound and a
//!   full footprint that fits the isolate's scan memory (413 unread above
//!   either); the intake reservation (503 unread when busy); the body, read
//!   within Turnstile's token validity (408 past it); the strict envelope;
//!   Turnstile siteverify (single use, hostname and action checked); the rest
//!   of the reservation (503 when busy); the exact token count; the length
//!   floor; the English screen. Refusals answer one JSON object. An accepted
//!   text gets an NDJSON stream: queue positions, "starting" on a cold
//!   instance, then the result or an error. Each line is written within one
//!   hold, or the reader is taken as gone. The gate sees a ticket and a
//!   client key, never the text, which stays in this invocation.
//! - `POST /api/detect/v1/feedback`: a receipt and a button. A receipt of
//!   the wrong shape is refused here; any other goes to the `DetectFeedback`
//!   object (never the gate), which accepts it once, and is counted as a
//!   content-free data point.
//! - anything else: 404 (405 for another method on a route).
//!
//! Every answer carries `Cache-Control: no-store`. No answer echoes input,
//! and nothing here writes a console line.

use crate::answer::{self, Outcome, PackageLine};
use crate::cf_io::{self, Settled, now_ms};
use crate::envelope;
use crate::policy::{self, Band, LengthBin};
use crate::routes::{self, ScanMemory, NoRoom};
use crate::waits::Waits;
use crate::{bound, english, silence_panics};
use serde::{Deserialize, Serialize};
use slop_ninja_arena::{ArenaAlloc, AsyncRoute, glue};
use std::cell::Cell;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::wasm_bindgen;
use worker::worker_sys::{self, web_sys};
use worker::{Env, Method, Request, Response};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

thread_local! {
    static SCAN_MEMORY: ScanMemory = const { ScanMemory::new() };
    /// The earliest moment the instance could sleep, as the gate last said.
    static AWAKE_HINT: Cell<u64> = const { Cell::new(0) };
}

/// The gate object (one).
pub(crate) const GATE: &str = "DETECT_GATE";
/// The feedback object (one).
const FEEDBACK: &str = "DETECT_FEEDBACK";
/// The lane objects (one per Container instance; one at launch).
pub(crate) const LANE: &str = "DETECT_LANE";
/// The warm forward limiter (rate-limit binding, limit 1 per its shortest period).
const WARM_LIMIT: &str = "DETECT_WARM_LIMIT";
/// Content-free data points (optional Analytics Engine binding).
pub(crate) const METRICS: &str = "DETECT_METRICS";
/// The Turnstile secret (a Worker secret).
const TURNSTILE_SECRET: &str = "TURNSTILE_SECRET";
const SITEVERIFY: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";

#[wasm_bindgen(inline_js = "
export function detect_route(req) {
  let u;
  try { u = new URL(req.url); } catch (e) { return 0; }
  if (u.search !== '') return 0;
  const post = req.method === 'POST';
  if (u.pathname === '/api/detect/v1/score') return post ? 1 : 9;
  if (u.pathname === '/api/detect/v1/warm') return post ? 2 : 9;
  if (u.pathname === '/api/detect/v1/feedback') return post ? 3 : 9;
  return 0;
}
export function ndjson_pair() {
  const ts = new TransformStream();
  return [ts.readable, ts.writable.getWriter()];
}
export function deferred() {
  let resolve;
  const p = new Promise((r) => { resolve = r; });
  return [p, resolve];
}
export function write_chunk(writer, bytes) {
  return writer.write(bytes).then(() => true, () => false);
}
export function close_writer(writer) {
  return writer.close().then(() => true, () => false);
}
export function random_bytes(n) {
  const b = new Uint8Array(n);
  crypto.getRandomValues(b);
  return b;
}
")]
extern "C" {
    /// 1 score, 2 warm, 3 feedback, 9 another method on a route, else 0
    /// (read in JS, no Rust allocation).
    fn detect_route(req: &web_sys::Request) -> u32;
    /// `[readable, writer]` of a fresh TransformStream.
    fn ndjson_pair() -> js_sys::Array;
    /// `[promise, resolve]`.
    fn deferred() -> js_sys::Array;
    fn write_chunk(writer: &JsValue, bytes: &js_sys::Uint8Array) -> js_sys::Promise;
    fn close_writer(writer: &JsValue) -> js_sys::Promise;
    /// `n` bytes from the platform's CSPRNG.
    pub(crate) fn random_bytes(n: u32) -> js_sys::Uint8Array;
}

/// A bodiless or small JSON answer built in JS (no Rust allocation).
fn status_only(status: u16) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    if let Ok(h) = web_sys::Headers::new() {
        let _ = h.set("Cache-Control", "no-store");
        if status == 405 {
            let _ = h.set("Allow", "POST");
        }
        init.set_headers(&h);
    }
    web_sys::Response::new_with_opt_str_and_init(None, &init).unwrap_or_else(|_| web_sys::Response::error())
}

fn json_raw(status: u16, body: &[u8]) -> web_sys::Response {
    let res = Response::from_bytes(body.to_vec()).map(|r| r.with_status(status)).and_then(|mut r| {
        let h = r.headers_mut();
        h.set("Content-Type", "application/json; charset=utf-8")?;
        h.set("Cache-Control", "no-store")?;
        h.set("X-Content-Type-Options", "nosniff")?;
        Ok(r)
    });
    match res {
        Ok(r) => match worker::IntoResponse::into_raw(r) {
            Ok(raw) => raw,
            Err(_) => status_only(500),
        },
        Err(_) => status_only(500),
    }
}

/// A fixed error code (never input).
#[derive(Serialize)]
struct Fail {
    state: &'static str,
    code: &'static str,
}

fn fail(status: u16, code: &'static str) -> web_sys::Response {
    let body = serde_json::to_vec(&Fail { state: "error", code }).unwrap_or_default();
    json_raw(status, &body)
}

/// A refusal of the text itself (a normal outcome, 200).
#[derive(Serialize)]
struct Refused {
    state: &'static str,
    code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    words: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens: Option<u64>,
}

fn refused(code: &'static str, words: Option<usize>, tokens: Option<u64>) -> web_sys::Response {
    let body = serde_json::to_vec(&Refused { state: "refused", code, words, tokens }).unwrap_or_default();
    json_raw(200, &body)
}

#[wasm_bindgen]
pub fn fetch(req: web_sys::Request, env: Env, _ctx: worker_sys::Context) -> js_sys::Promise {
    silence_panics();
    let Ok(ticket) = glue::Ticket::admit() else {
        let v: JsValue = status_only(503).into();
        return js_sys::Promise::resolve(&v);
    };
    let _ = slop_ninja_arena::tick(now_ms());
    let class = detect_route(&req);
    let (route, control): (AsyncRoute, usize) = match class {
        1 => (routes::R_SCORE, routes::SCORE_CONTROL_BYTES),
        2 => (routes::R_WARM, routes::WARM_CONTROL_BYTES),
        3 => (routes::R_FEEDBACK, routes::FEEDBACK_CONTROL_BYTES),
        _ => (routes::R_OTHER, routes::OTHER_CONTROL_BYTES),
    };
    // The score route resolves its answer early (a stream it keeps writing),
    // so its promise is a deferred one the handler resolves.
    let pair = deferred();
    let (outer, resolve) = (pair.get(0), pair.get(1));
    let outer: js_sys::Promise = outer.into();
    let resolve: js_sys::Function = resolve.into();
    wasm_bindgen_futures::spawn_local(async move {
        let _ticket = ticket;
        let done = resolve.clone();
        let r = slop_ninja_arena::enter(route, control, move |_scope| async move {
            let req = Request::from(req);
            let answer = Answer { resolve };
            match class {
                1 => score(req, env, &answer).await,
                2 => answer.send(warm(&req, &env).await),
                3 => answer.send(feedback(req, &env).await),
                9 => answer.send(status_only(405)),
                _ => answer.send(status_only(404)),
            }
        });
        match r {
            Ok(e) => {
                let _ = e.await;
            }
            Err(_) => {
                let _ = done.call1(&JsValue::NULL, &status_only(503).into());
            }
        }
    });
    outer
}

/// The invocation's answer: resolving it more than once has no effect.
struct Answer {
    resolve: js_sys::Function,
}

impl Answer {
    fn send(&self, r: web_sys::Response) {
        let _ = self.resolve.call1(&JsValue::NULL, &r.into());
    }
}

/// The isolate's scan reservation, given back when the request ends.
struct Reserved(usize);

impl Reserved {
    fn take(bytes: usize) -> std::result::Result<Reserved, NoRoom> {
        SCAN_MEMORY.with(|b| b.take(bytes)).map(|()| Reserved(bytes))
    }

    /// Reserves `more` on top of what this already holds.
    fn grow(&mut self, more: usize) -> std::result::Result<(), NoRoom> {
        SCAN_MEMORY.with(|b| b.take(more))?;
        self.0 = self.0.saturating_add(more);
        Ok(())
    }
}

impl Drop for Reserved {
    fn drop(&mut self) {
        SCAN_MEMORY.with(|b| b.give_back(self.0));
    }
}

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string()).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// A switch is off only when its var says exactly `off`.
pub(crate) fn switch_on(env: &Env, name: &str) -> bool {
    var(env, name).as_deref() != Some("off")
}

/// The deployment's package line and load identity.
pub(crate) struct Package {
    /// The files manifest v2's SHA-256: the scorer's load digest.
    pub load: String,
    /// The published package digest shown on the page.
    pub published: String,
    pub pending: bool,
}

pub(crate) fn package(env: &Env) -> Option<Package> {
    Some(Package {
        load: var(env, "PACKAGE_SHA256")?,
        published: var(env, "PUBLISHED_PACKAGE_DIGEST")?,
        pending: var(env, "PUBLICATION").as_deref() != Some("published"),
    })
}

pub(crate) fn ms_var(env: &Env, name: &str) -> Option<u64> {
    var(env, name).and_then(|v| v.parse::<u64>().ok()).filter(|v| *v > 0)
}

/// The deployment's liveness waits (the scan lease derives from them).
pub(crate) fn waits(env: &Env) -> Waits {
    let d = Waits::DEFAULT;
    Waits {
        start_ms: ms_var(env, "SCORER_START_WAIT_MS").unwrap_or(d.start_ms),
        load_ms: ms_var(env, "LOAD_WAIT_MS").unwrap_or(d.load_ms),
        score_ms: ms_var(env, "SCORE_WAIT_MS").unwrap_or(d.score_ms),
        hold_ms: ms_var(env, "QUEUE_HOLD_MS").unwrap_or(d.hold_ms),
    }
}

// ------------------------------------------------------------------- warm

#[derive(Serialize)]
struct WarmAnswer {
    state: &'static str,
    tool: bool,
    spans: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    site_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<PackageLine>,
}

#[derive(Deserialize)]
pub(crate) struct GateWarm {
    pub start: bool,
    pub awake_until: u64,
}

async fn warm(req: &Request, env: &Env) -> web_sys::Response {
    let _ = req;
    let tool = switch_on(env, "DETECT_TOOL");
    let now = now_ms();
    let state = if !tool {
        "off"
    } else if AWAKE_HINT.with(Cell::get) > now {
        "awake"
    } else {
        let allowed = match env.rate_limiter(WARM_LIMIT) {
            Ok(l) => l.limit("warm".into()).await.map(|o| o.success).unwrap_or(false),
            Err(_) => false,
        };
        if allowed {
            match gate_call::<GateWarm, ()>(env, "/warm", None, 10_000).await {
                Some(w) => {
                    AWAKE_HINT.with(|h| h.set(w.awake_until));
                    if w.start { "starting" } else { "awake" }
                }
                None => "starting",
            }
        } else {
            "starting"
        }
    };
    let body = WarmAnswer {
        state,
        tool,
        spans: switch_on(env, "DETECT_SPANS"),
        site_key: var(env, "TURNSTILE_SITE_KEY"),
        package: package(env).map(|p| PackageLine { digest: p.published, publication_pending: p.pending }),
    };
    json_raw(200, &serde_json::to_vec(&body).unwrap_or_default())
}

// --------------------------------------------------------------- gate calls

/// One call to a singleton object (`binding`, named `name`), bounded by
/// `timeout_ms` (the platform enforces it on the call and its reply).
/// `None` on any failure; `Some(None)` for a bodiless 2xx.
async fn object_call<T: serde::de::DeserializeOwned, B: Serialize>(
    env: &Env,
    binding: &str,
    name: &str,
    path: &str,
    body: Option<&B>,
    timeout_ms: u64,
) -> Option<Option<T>> {
    let ns = env.durable_object(binding).ok()?;
    let id = ns.id_from_name(name).ok()?;
    let stub = id.get_stub().ok()?;
    let text = match body {
        Some(b) => Some(serde_json::to_string(b).ok()?),
        None => None,
    };
    let req = cf_io::internal_request(&format!("https://{name}{path}"), Method::Post, text.as_deref(), Some(timeout_ms)).ok()?;
    let mut res = stub.fetch_with_request(req).await.ok()?;
    match res.status_code() {
        200 => cf_io::response_json::<T>(&mut res, 64 * 1024).await.ok().map(Some),
        204 => Some(None),
        _ => {
            cf_io::discard(&mut res);
            None
        }
    }
}

/// One call to the gate object. `None` on any failure: the callers answer
/// with a fixed code.
pub(crate) async fn gate_call<T: serde::de::DeserializeOwned, B: Serialize>(
    env: &Env,
    path: &str,
    body: Option<&B>,
    timeout_ms: u64,
) -> Option<T> {
    object_call::<T, B>(env, GATE, "gate", path, body, timeout_ms).await.flatten()
}

/// A gate call whose answer is only its success.
async fn gate_tell<B: Serialize>(env: &Env, path: &str, body: &B, timeout_ms: u64) -> bool {
    object_call::<serde::de::IgnoredAny, B>(env, GATE, "gate", path, Some(body), timeout_ms).await.is_some()
}

/// One call to the feedback object (never the gate).
async fn feedback_call<T: serde::de::DeserializeOwned, B: Serialize>(env: &Env, path: &str, body: &B, timeout_ms: u64) -> Option<T> {
    object_call::<T, B>(env, FEEDBACK, "feedback", path, Some(body), timeout_ms).await.flatten()
}

#[derive(Serialize)]
struct EnterReq<'a> {
    prefix: &'a str,
    hold_ms: u64,
}

#[derive(Serialize)]
struct AskReq<'a> {
    ticket: &'a str,
    hold_ms: u64,
}

#[derive(Serialize)]
struct DoneReq<'a> {
    ticket: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    served_ms: Option<u64>,
}

#[derive(Serialize)]
struct IssueReq {
    band: &'static str,
    length: &'static str,
}

#[derive(Deserialize)]
struct GateStatus {
    #[serde(default)]
    ticket: String,
    status: String,
    #[serde(default)]
    position: Option<usize>,
    #[serde(default)]
    estimate_ms: Option<u64>,
    #[serde(default)]
    awake: bool,
    #[serde(default)]
    tool: bool,
}

#[derive(Deserialize)]
struct IssueReply {
    #[serde(default)]
    receipt: Option<String>,
}

// ------------------------------------------------------------------ score

/// The NDJSON stream of an accepted text. Each write must complete within
/// `bound_ms` (one hold): a reader that takes longer to accept a line is
/// taken as gone, so no invocation waits on a reader indefinitely.
struct Lines {
    writer: JsValue,
    open: bool,
    bound_ms: u64,
}

impl Lines {
    async fn line<T: Serialize>(&mut self, v: &T) -> bool {
        if !self.open {
            return false;
        }
        let Ok(mut bytes) = serde_json::to_vec(v) else { return false };
        bytes.push(b'\n');
        let chunk = js_sys::Uint8Array::from(bytes.as_slice());
        let until = now_ms().saturating_add(self.bound_ms);
        self.open = matches!(cf_io::settle(write_chunk(&self.writer, &chunk), Some(until)).await, Settled::Value(v) if v.as_bool() == Some(true));
        self.open
    }

    async fn close(&mut self) {
        if self.open {
            let until = now_ms().saturating_add(self.bound_ms);
            let _ = cf_io::settle(close_writer(&self.writer), Some(until)).await;
            self.open = false;
        }
    }
}

#[derive(Serialize)]
struct Queued {
    state: &'static str,
    position: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimate_ms: Option<u64>,
}

#[derive(Serialize)]
struct Phase {
    state: &'static str,
}

enum Verify {
    Passed,
    Refused,
    Unavailable,
}

#[derive(Serialize)]
struct SiteverifyReq<'a> {
    secret: &'a str,
    response: &'a str,
}

#[derive(Deserialize)]
struct SiteverifyReply {
    success: bool,
    #[serde(default)]
    hostname: Option<String>,
    #[serde(default)]
    action: Option<String>,
}

/// Server-side Turnstile check: success, the expected hostname and action.
/// Cloudflare accepts each token once and for 300 s.
async fn turnstile(env: &Env, token: &str) -> Verify {
    let (Some(secret), Some(host)) = (env.secret(TURNSTILE_SECRET).ok().map(|s| s.to_string()), var(env, "TURNSTILE_HOSTNAME")) else {
        return Verify::Unavailable;
    };
    let Ok(body) = serde_json::to_string(&SiteverifyReq { secret: secret.trim(), response: token }) else {
        return Verify::Unavailable;
    };
    let mut init = worker::RequestInit::new();
    init.with_method(Method::Post);
    let headers = worker::Headers::new();
    if headers.set("content-type", "application/json").is_err() {
        return Verify::Unavailable;
    }
    init.with_headers(headers);
    init.with_body(Some(JsValue::from_str(&body)));
    let Ok(req) = Request::new_with_init(SITEVERIFY, &init) else { return Verify::Unavailable };
    let Ok(mut res) = worker::Fetch::Request(req).send().await else { return Verify::Unavailable };
    if res.status_code() != 200 {
        cf_io::discard(&mut res);
        return Verify::Unavailable;
    }
    match cf_io::response_json::<SiteverifyReply>(&mut res, 16 * 1024).await {
        Ok(r) if r.success && r.hostname.as_deref() == Some(host.as_str()) && r.action.as_deref() == Some(crate::TURNSTILE_ACTION) => {
            Verify::Passed
        }
        Ok(_) => Verify::Refused,
        Err(_) => Verify::Unavailable,
    }
}

#[derive(Serialize)]
struct Forward<'a> {
    package: &'a str,
    spans: bool,
    docs: [ForwardDoc<'a>; 1],
}

#[derive(Serialize)]
struct ForwardDoc<'a> {
    id: &'static str,
    text: &'a str,
}

/// Why the lane did not score.
pub(crate) enum LaneFail {
    Busy,
    Restarting,
    Unavailable,
    /// The instance is up and ready, and its scorer refused this request.
    Refused,
}

/// The lane's score call. The lane works within `left_ms` (sent with the
/// call); the call itself is allowed that time plus one hold, so the lane's
/// own answer, including a timeout's, arrives before this gives up on it.
async fn lane_score(env: &Env, body: &str, left_ms: u64, hold_ms: u64) -> std::result::Result<Vec<u8>, LaneFail> {
    let ns = env.durable_object(LANE).map_err(|_| LaneFail::Unavailable)?;
    let id = ns.id_from_name("lane-0").map_err(|_| LaneFail::Unavailable)?;
    let stub = id.get_stub().map_err(|_| LaneFail::Unavailable)?;
    let url = format!("https://lane/score?left_ms={left_ms}");
    let req = cf_io::internal_request(&url, Method::Post, Some(body), Some(left_ms.saturating_add(hold_ms)))
        .map_err(|_| LaneFail::Unavailable)?;
    let mut res = stub.fetch_with_request(req).await.map_err(|_| LaneFail::Restarting)?;
    match res.status_code() {
        200 => cf_io::response_bytes(&mut res, routes::REPLY_BYTES).await.map_err(|_| LaneFail::Restarting),
        409 => {
            cf_io::discard(&mut res);
            Err(LaneFail::Busy)
        }
        502 => {
            cf_io::discard(&mut res);
            Err(LaneFail::Refused)
        }
        _ => {
            cf_io::discard(&mut res);
            Err(LaneFail::Restarting)
        }
    }
}

/// One content-free data point: route outcome, band, length and token bins,
/// cold or warm, service time. Never text, never a hash, never an address.
pub(crate) fn metric(env: &Env, outcome: &str, band: &str, length: &str, tokens: Option<u64>, cold: bool, ms: u64) {
    let Ok(ds) = env.analytics_engine(METRICS) else { return };
    let token_bin = match tokens {
        None => "none",
        Some(t) if t <= 300 => "le300",
        Some(t) if t <= 512 => "le512",
        Some(t) if t <= 1024 => "le1024",
        Some(_) => "le2048",
    };
    let _ = worker::AnalyticsEngineDataPointBuilder::new()
        .indexes([outcome])
        .blobs([outcome, band, length, token_bin, if cold { "cold" } else { "warm" }])
        .add_double(ms as f64)
        .write_to(&ds);
}

async fn score(mut req: Request, env: Env, answer: &Answer) {
    let started = now_ms();
    let json_type = req.headers().get("content-type").ok().flatten().is_some_and(|c| {
        c.split(';').next().is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
    });
    if !json_type {
        return answer.send(fail(415, "content_type"));
    }
    if !switch_on(&env, "DETECT_TOOL") {
        return answer.send(fail(503, "tool_off"));
    }
    let Some(pkg) = package(&env) else { return answer.send(fail(503, "not_configured")) };
    let declared = req.headers().get("content-length").ok().flatten().and_then(|v| v.trim().parse::<u64>().ok());
    let Some(declared) = declared else { return answer.send(fail(411, "length_required")) };
    if declared > bound::RAW_BODY_BYTES {
        return answer.send(fail(413, "too_large"));
    }
    // The full footprint must fit alone; until Turnstile passes, only the
    // intake (the body and the unescaped text) is reserved.
    let reservation = routes::scan_footprint(Some(declared));
    if reservation > routes::SCAN_MEMORY_BYTES {
        return answer.send(fail(413, "too_large"));
    }
    let intake = routes::intake_footprint(declared);
    let mut held = match Reserved::take(intake) {
        Ok(h) => h,
        Err(NoRoom::Busy) => return answer.send(fail(503, "busy")),
        Err(NoRoom::TooLarge) => return answer.send(fail(413, "too_large")),
    };
    let Ok(limit) = usize::try_from(declared) else { return answer.send(fail(413, "too_large")) };
    let until = started.saturating_add(crate::TURNSTILE_TOKEN_VALIDITY_MS);
    let body = match cf_io::request_bytes_until(&mut req, limit, Some(until)).await {
        Ok(b) => b,
        Err(e) => return answer.send(fail(e.status(), "body")),
    };
    let request = match envelope::parse(&body) {
        Ok(e) => e,
        Err(m) => return answer.send(fail(400, m.code())),
    };
    match turnstile(&env, request.turnstile).await {
        Verify::Passed => {}
        Verify::Refused => return answer.send(fail(403, "turnstile")),
        Verify::Unavailable => return answer.send(fail(503, "turnstile_unavailable")),
    }
    if held.grow(reservation.saturating_sub(intake)).is_err() {
        return answer.send(fail(503, "busy"));
    }
    let text: &str = &request.text;
    if text.trim().is_empty() {
        return answer.send(refused("empty", None, None));
    }
    let tokens = if text.len() as u64 > bound::MAX_TEXT_BYTES {
        None
    } else {
        let mut admit = |bytes: usize, _align: usize| bytes <= reservation;
        match slop_ninja_scorer_tokens::count(text, &mut admit) {
            Ok(n) => Some(n),
            Err(_) => return answer.send(fail(503, "busy")),
        }
    };
    let Some(tokens) = tokens.filter(|t| *t <= bound::MAX_TOKENS) else {
        metric(&env, "too_long", "", "", tokens, false, 0);
        return answer.send(refused("too_long", None, tokens));
    };
    let words = policy::words(text);
    let length = match policy::length_bin(words) {
        Ok(l) => l,
        Err(w) => {
            metric(&env, "too_short", "", "", Some(tokens), false, 0);
            return answer.send(refused("too_short", Some(w), Some(tokens)));
        }
    };
    if english::english_screen(text).is_some() {
        metric(&env, "not_english", "", length.code(), Some(tokens), false, 0);
        return answer.send(refused("not_english", Some(words), Some(tokens)));
    }
    let fwd = Forward { package: &pkg.load, spans: true, docs: [ForwardDoc { id: "d", text }] };
    let Ok(fwd) = serde_json::to_string(&fwd) else { return answer.send(fail(500, "internal")) };
    if fwd.len() as u64 > bound::SCORER_MAX_BODY {
        return answer.send(refused("too_long", None, Some(tokens)));
    }
    // Accepted: from here the answer is a stream.
    let w = waits(&env);
    let pair = ndjson_pair();
    let (readable, writer) = (pair.get(0), pair.get(1));
    let mut lines = Lines { writer, open: true, bound_ms: w.hold_ms };
    let init = web_sys::ResponseInit::new();
    init.set_status(200);
    if let Ok(h) = web_sys::Headers::new() {
        let _ = h.set("Content-Type", "application/x-ndjson; charset=utf-8");
        let _ = h.set("Cache-Control", "no-store");
        let _ = h.set("X-Content-Type-Options", "nosniff");
        init.set_headers(&h);
    }
    let stream: Option<web_sys::ReadableStream> = wasm_bindgen::JsCast::dyn_into(readable).ok();
    match web_sys::Response::new_with_opt_readable_stream_and_init(stream.as_ref(), &init) {
        Ok(r) => answer.send(r),
        Err(_) => return answer.send(fail(500, "internal")),
    }
    run(&env, &req, &mut lines, &w, &pkg, fwd, text, tokens, words, length, started).await;
    lines.close().await;
    drop(held);
}

#[allow(clippy::too_many_arguments)]
async fn run(
    env: &Env,
    req: &Request,
    lines: &mut Lines,
    w: &Waits,
    pkg: &Package,
    fwd: String,
    text: &str,
    tokens: u64,
    words: usize,
    length: LengthBin,
    started: u64,
) {
    let hold = w.hold_ms;
    let ip = req.headers().get("cf-connecting-ip").ok().flatten();
    let prefix = ip.as_deref().and_then(crate::address_prefix).unwrap_or_else(|| "none".into());
    let Some(mut st) = gate_call::<GateStatus, _>(env, "/enter", Some(&EnterReq { prefix: &prefix, hold_ms: hold }), hold).await else {
        let _ = lines.line(&Fail { state: "error", code: "unavailable" }).await;
        return;
    };
    let ticket_id = std::mem::take(&mut st.ticket);
    let ticket = ticket_id.as_str();
    let done = move |served_ms: Option<u64>| DoneReq { ticket, served_ms };
    loop {
        match st.status.as_str() {
            "admitted" => break,
            "queued" => {
                let q = Queued { state: "queued", position: st.position.unwrap_or(1), estimate_ms: st.estimate_ms };
                if !lines.line(&q).await {
                    let _ = gate_tell(env, "/leave", &done(None), hold).await;
                    return;
                }
                let ask = AskReq { ticket, hold_ms: hold };
                // The gate holds the ask until the ticket is admitted or the
                // hold ends.
                match gate_call::<GateStatus, _>(env, "/ask", Some(&ask), hold.saturating_mul(2)).await {
                    Some(next) => st = next,
                    None => {
                        let _ = lines.line(&Fail { state: "error", code: "unavailable" }).await;
                        return;
                    }
                }
            }
            "busy" => {
                metric(env, "busy", "", length.code(), Some(tokens), false, 0);
                let _ = lines.line(&Fail { state: "error", code: "busy" }).await;
                return;
            }
            _ => {
                let _ = lines.line(&Fail { state: "error", code: "expired" }).await;
                return;
            }
        }
    }
    // The admission's clock: the lane gets what is left of its bound.
    let admitted_at = now_ms();
    if !st.tool {
        let _ = gate_tell(env, "/done", &done(None), hold).await;
        let _ = lines.line(&Fail { state: "error", code: "tool_off" }).await;
        return;
    }
    let cold = !st.awake;
    if cold && !lines.line(&Phase { state: "starting" }).await {
        let _ = gate_tell(env, "/done", &done(None), hold).await;
        return;
    }
    let t0 = now_ms();
    let scored = match w.lane_left_ms(t0.saturating_sub(admitted_at)) {
        Some(left) => lane_score(env, &fwd, left, hold).await,
        None => Err(LaneFail::Restarting),
    };
    drop(fwd);
    let served = now_ms().saturating_sub(t0);
    let reply = match scored {
        Ok(r) => r,
        Err(f) => {
            let _ = gate_tell(env, "/done", &done(None), hold).await;
            let code = match f {
                LaneFail::Busy => "busy",
                LaneFail::Restarting => "restarting",
                LaneFail::Unavailable => "unavailable",
                LaneFail::Refused => "scorer",
            };
            metric(env, code, "", length.code(), Some(tokens), cold, served);
            let _ = lines.line(&Fail { state: "error", code }).await;
            return;
        }
    };
    let outcome = answer::read(&reply, &pkg.load, text, switch_on(env, "DETECT_SPANS"));
    drop(reply);
    match outcome {
        Outcome::Scored { band, confidence, share, tokens: scorer_tokens, spans } => {
            // The slot is freed and the receipt issued at once: the receipt
            // comes from the feedback object, which answers within a hold or
            // the result carries no receipt.
            let issue = IssueReq { band: band.code(), length: length.code() };
            let (_, receipt) = futures_util::future::join(
                gate_tell(env, "/done", &done(Some(served)), hold),
                feedback_call::<IssueReply, _>(env, "/issue", &issue, hold),
            )
            .await;
            let receipt = receipt.and_then(|r| r.receipt).unwrap_or_default();
            let line = answer::answer(
                band,
                confidence,
                share,
                words,
                scorer_tokens,
                length,
                spans,
                text,
                PackageLine { digest: pkg.published.clone(), publication_pending: pkg.pending },
                receipt,
            );
            metric(env, "result", band.code(), length.code(), Some(tokens), cold, now_ms().saturating_sub(started));
            let _ = lines.line(&line).await;
        }
        Outcome::Declined(code) => {
            let _ = gate_tell(env, "/done", &done(Some(served)), hold).await;
            let _ = lines.line(&Refused { state: "refused", code, words: Some(words), tokens: Some(tokens) }).await;
        }
        Outcome::Fault => {
            let _ = gate_tell(env, "/done", &done(None), hold).await;
            metric(env, "fault", "", length.code(), Some(tokens), cold, served);
            let _ = lines.line(&Fail { state: "error", code: "scorer" }).await;
        }
    }
}

// --------------------------------------------------------------- feedback

#[derive(Deserialize)]
struct FeedbackReq<'a> {
    receipt: &'a str,
    claim: &'a str,
}

#[derive(Serialize)]
struct FeedbackFwd<'a> {
    receipt: &'a str,
    claim: &'static str,
}

#[derive(Deserialize)]
struct FeedbackReply {
    band: String,
    length: String,
}

async fn feedback(mut req: Request, env: &Env) -> web_sys::Response {
    let body = match cf_io::request_bytes(&mut req, routes::FEEDBACK_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => return fail(e.status(), "body"),
    };
    let Ok(f) = serde_json::from_slice::<FeedbackReq<'_>>(&body) else { return fail(400, "malformed_request") };
    let Some(claim) = crate::receipt::Claim::from_code(f.claim) else { return fail(400, "malformed_request") };
    // A receipt of the wrong shape never leaves this invocation; any other
    // goes to the feedback object, never to the admission gate.
    if crate::receipt::parse(f.receipt).is_none() {
        return fail(409, "receipt");
    }
    let fwd = FeedbackFwd { receipt: f.receipt, claim: claim.code() };
    match feedback_call::<FeedbackReply, _>(env, "/vote", &fwd, waits(env).hold_ms).await {
        Some(r) => {
            // Content-free: the band, length bin, package and button.
            if let (Some(b), Some(l), Ok(ds)) = (Band::from_code(&r.band), LengthBin::from_code(&r.length), env.analytics_engine(METRICS)) {
                let digest = package(env).map(|p| p.published).unwrap_or_default();
                let prefix: String = digest.chars().take(16).collect();
                let _ = worker::AnalyticsEngineDataPointBuilder::new()
                    .indexes(["feedback"])
                    .blobs(["feedback", b.code(), l.code(), claim.code(), prefix.as_str()])
                    .add_double(1.0)
                    .write_to(&ds);
            }
            status_only(204)
        }
        None => fail(409, "receipt"),
    }
}
