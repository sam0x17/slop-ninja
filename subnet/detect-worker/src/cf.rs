//! The detect Worker on Cloudflare (wasm32 only): its `fetch`, `queue` and
//! `scheduled` exports.
//!
//! Routes (reached only through the public forwarders' service bindings):
//! - `POST /api/detect/v1/warm`: content-free. Answered from this isolate's
//!   awake hint while it holds; otherwise, at most once per Cloudflare
//!   location per rate-limit period (a constant-keyed rate-limit binding),
//!   forwarded to the gate, which starts the one instance only if none is
//!   awake. Every other warm is answered "starting" with no forward.
//! - `POST /api/detect/v1/score`: `{"text"}`. In order: the forwarded
//!   request's admission (`routes::forwarded`: the `X-SN-*` allow-list, no
//!   principal, the CSRF check); the content type; the switches (the vars
//!   AND the store's rows, one D1 read; the vars decide when it fails); a
//!   declared length within the body bound and a full footprint that fits the
//!   isolate's scan memory (413 unread above either); the intake reservation
//!   (503 unread when busy); the body, read within the body wait (408 past
//!   it); the strict envelope; rule `x_post_v1` (a lone link to a post on X
//!   scores nothing and takes no ticket); the rest of the reservation (503
//!   when busy); `canon_v1` (`canon.rs`: the seal binds the canonical text,
//!   and the passages are mapped back to the string the page sent); the free
//!   cut (at most the first 1,000 words, never inside a sentence, within the
//!   scorer's tokens, counted exactly: `cut.rs`); the scored part's length
//!   floor and English screen. Only the scored part is
//!   forwarded. Refusals answer one JSON object with their code's status. An
//!   accepted text gets an NDJSON stream: queue positions, "starting" on a
//!   cold instance, then the result with its seal, or a refusal line. Each
//!   line is written within one hold, or the reader is taken as gone. The
//!   gate sees a ticket, a class and a client key, never the text, which
//!   stays in this invocation; the release that ends a scan carries its
//!   content-free count.
//! - `POST /api/detect/v1/feedback`: a receipt and a button. A receipt of
//!   the wrong shape is refused here; any other goes to the `DetectFeedback`
//!   object (never the gate), which accepts it once, and is counted as a
//!   content-free data point.
//! - The store's routes (`cf_routes`): `POST /api/detect/v1/share`
//!   (Get a link, with `X-SN-Principal` from the site host when saved),
//!   `/manage` (the secret host), `/rerun` (a secret page's re-run: the same
//!   ticket class, cut and lane as a paste, counted like one) and `/state`;
//!   and the binding-only `/internal/v1/*` routes (page, preview, card,
//!   states, owner, examples, example, sitemap shards, removals), refused
//!   when they carry `X-SN-Route` or `X-SN-Host`, with `X-SN-Principal` on
//!   the owner route only.
//! - anything else: 404 (405 for another method on a route).
//!
//! The `queue` export consumes `detect-jobs` (one message per invocation),
//! and `scheduled` runs the store's hourly work.
//!
//! Every answer carries `Cache-Control: no-store` and `nosniff`. No answer
//! echoes input, and nothing here writes a console line.

use crate::answer::{self, Outcome, PackageLine};
use crate::canon;
use crate::cf_io::{self, BodyError, Settled, now_ms};
use crate::counters::{self, Count};
use crate::cut::{self, Cut, NoCut};
use crate::envelope;
use crate::policy::{self, Band, LengthBin};
use crate::routes::forwarded::{self, Admission, Arrival};
use crate::routes::{self, NoRoom, ScanMemory};
use crate::seal::{self, ResultPayload};
use crate::switches::{self, Switches};
use crate::waits::Waits;
use crate::{bound, english, silence_panics, xpost};
use futures_util::future::{Either, select};
use serde::{Deserialize, Serialize};
use slop_ninja_arena::{ArenaAlloc, AsyncRoute, glue};
use slop_ninja_auth_core::csrf::{BodyType, content_type_ok};
use slop_ninja_detect_page::{ApiRefusal, Channel, CutKind, Origins, RefusalCode, RouteId, TicketClass, ae};
use std::cell::Cell;
use std::future::Future;
use std::time::Duration;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::wasm_bindgen;
use worker::worker_sys::{self, web_sys};
use worker::{Delay, Env, Method, Request, Response};

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
/// The detect store (D1): the switches rows, and the gate's hourly counts.
pub(crate) const STORE: &str = "DETECT_STORE";
/// The Worker secret the seal keys derive from.
const STORE_SECRET: &str = "DETECT_STORE_SECRET";

#[wasm_bindgen(inline_js = "
export function detect_route(req) {
  let u;
  try { u = new URL(req.url); } catch (e) { return 0; }
  if (u.pathname.startsWith('/internal/')) return 5;
  if (u.search !== '') return 0;
  const post = req.method === 'POST';
  if (u.pathname === '/api/detect/v1/score') return post ? 1 : 9;
  if (u.pathname === '/api/detect/v1/warm') return post ? 2 : 9;
  if (u.pathname === '/api/detect/v1/feedback') return post ? 3 : 9;
  if (u.pathname === '/api/detect/v1/share') return post ? 4 : 9;
  if (u.pathname === '/api/detect/v1/manage') return post ? 6 : 9;
  if (u.pathname === '/api/detect/v1/rerun') return post ? 7 : 9;
  if (u.pathname === '/api/detect/v1/state') return post ? 8 : 9;
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
export function writer_errored(writer) {
  return writer.desiredSize === null;
}
export function random_bytes(n) {
  const b = new Uint8Array(n);
  crypto.getRandomValues(b);
  return b;
}
")]
extern "C" {
    /// 1 score, 2 warm, 3 feedback, 4 share, 5 an internal path, 6 manage,
    /// 7 rerun, 8 state, 9 another method on a route, else 0 (read in JS,
    /// no Rust allocation).
    fn detect_route(req: &web_sys::Request) -> u32;
    /// `[readable, writer]` of a fresh TransformStream.
    fn ndjson_pair() -> js_sys::Array;
    /// `[promise, resolve]`.
    fn deferred() -> js_sys::Array;
    fn write_chunk(writer: &JsValue, bytes: &js_sys::Uint8Array) -> js_sys::Promise;
    fn close_writer(writer: &JsValue) -> js_sys::Promise;
    /// The stream errored: its reader went away.
    fn writer_errored(writer: &JsValue) -> bool;
    /// `n` bytes from the platform's CSPRNG.
    pub(crate) fn random_bytes(n: u32) -> js_sys::Uint8Array;
}

/// A bodiless answer built in JS (no Rust allocation).
pub(crate) fn status_only(status: u16) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    if let Ok(h) = web_sys::Headers::new() {
        let _ = h.set("Cache-Control", "no-store");
        let _ = h.set("X-Content-Type-Options", "nosniff");
        init.set_headers(&h);
    }
    web_sys::Response::new_with_opt_str_and_init(None, &init).unwrap_or_else(|_| web_sys::Response::error())
}

pub(crate) fn json_raw(status: u16, body: &[u8], allow: bool) -> web_sys::Response {
    let res = Response::from_bytes(body.to_vec()).map(|r| r.with_status(status)).and_then(|mut r| {
        let h = r.headers_mut();
        h.set("Content-Type", "application/json; charset=utf-8")?;
        h.set("Cache-Control", "no-store")?;
        h.set("X-Content-Type-Options", "nosniff")?;
        if allow {
            h.set("Allow", "POST")?;
        }
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

/// A refusal: one JSON object with its code's status.
pub(crate) fn refusal(r: &ApiRefusal) -> web_sys::Response {
    json_raw(r.code.status(), &serde_json::to_vec(r).unwrap_or_default(), r.code == RefusalCode::Method)
}

pub(crate) fn refuse(code: RefusalCode) -> web_sys::Response {
    refusal(&ApiRefusal::new(code))
}

fn u32_of(n: impl TryInto<u32>) -> u32 {
    n.try_into().unwrap_or(u32::MAX)
}

/// A refusal of the free cut's scored part, with its counts.
fn of_cut(code: RefusalCode, c: &Cut<'_>) -> ApiRefusal {
    ApiRefusal {
        words: Some(u32_of(c.scored_words)),
        tokens: Some(u32_of(c.tokens)),
        total_words: Some(u32_of(c.total_words)),
        cut: CutKind::parse(c.kind.code()),
        ..ApiRefusal::new(code)
    }
}

/// The `too_short` refusal, naming the minimum that applies.
fn too_short(c: &Cut<'_>, min_words: usize) -> ApiRefusal {
    ApiRefusal { min_words: Some(u32_of(min_words)), ..of_cut(RefusalCode::TooShort, c) }
}

/// A body read's failure as a refusal.
pub(crate) fn body_code(e: BodyError) -> RefusalCode {
    match e {
        BodyError::TooLarge | BodyError::Refused(slop_ninja_arena::RefusalKind::Invalid) => RefusalCode::TooLarge,
        BodyError::Timeout | BodyError::Transport => RefusalCode::Body,
        BodyError::Refused(_) => RefusalCode::Busy,
    }
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
        4 => (routes::R_SHARE, routes::STORE_CONTROL_BYTES),
        5 => (routes::R_INTERNAL, routes::STORE_CONTROL_BYTES),
        6 => (routes::R_MANAGE, routes::STORE_CONTROL_BYTES),
        7 => (routes::R_RERUN, routes::RERUN_CONTROL_BYTES),
        8 => (routes::R_STATE, routes::STORE_CONTROL_BYTES),
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
                4 => answer.send(crate::cf_routes::share(req, &env).await),
                5 => answer.send(crate::cf_routes::internal(req, &env).await),
                6 => answer.send(crate::cf_routes::manage(req, &env).await),
                7 => answer.send(crate::cf_routes::rerun(req, &env).await),
                8 => answer.send(crate::cf_routes::state(req, &env).await),
                9 => answer.send(refuse(RefusalCode::Method)),
                _ => answer.send(refuse(RefusalCode::NotFound)),
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

/// The `detect-jobs` consumer (one message per invocation): refused (no
/// ticket, or no scope), the batch is retried and the export resolves; it
/// never rejects.
#[wasm_bindgen]
pub fn queue(batch: worker_sys::MessageBatch, env: Env, _ctx: worker_sys::Context) -> js_sys::Promise {
    silence_panics();
    let Ok(ticket) = glue::Ticket::admit() else {
        let _ = batch.retry_all(JsValue::UNDEFINED);
        return js_sys::Promise::resolve(&JsValue::UNDEFINED);
    };
    let _ = slop_ninja_arena::tick(now_ms());
    let retry = batch.clone();
    wasm_bindgen_futures::future_to_promise(async move {
        let _ticket = ticket;
        let r = slop_ninja_arena::enter(routes::R_QUEUE, routes::QUEUE_CONTROL_BYTES, move |_scope| async move {
            crate::cf_routes::consume(batch, env).await;
        });
        match r {
            Ok(e) => {
                let _ = e.await;
            }
            Err(_) => {
                let _ = retry.retry_all(JsValue::UNDEFINED);
            }
        }
        Ok(JsValue::UNDEFINED)
    })
}

/// The hourly cron: refused (no ticket, or no scope), the tick resolves and
/// the next hour's does the work.
#[wasm_bindgen]
pub fn scheduled(_event: worker_sys::ScheduledEvent, env: Env, _ctx: worker_sys::ScheduleContext) -> js_sys::Promise {
    silence_panics();
    let Ok(ticket) = glue::Ticket::admit() else { return js_sys::Promise::resolve(&JsValue::UNDEFINED) };
    let _ = slop_ninja_arena::tick(now_ms());
    wasm_bindgen_futures::future_to_promise(async move {
        let _ticket = ticket;
        if let Ok(e) = slop_ninja_arena::enter(routes::R_CRON, routes::CRON_CONTROL_BYTES, move |_scope| async move {
            crate::cf_routes::hourly(env).await;
        }) {
            let _ = e.await;
        }
        Ok(JsValue::UNDEFINED)
    })
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
pub(crate) struct Reserved(usize);

impl Reserved {
    pub(crate) fn take(bytes: usize) -> std::result::Result<Reserved, NoRoom> {
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

pub(crate) fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string()).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// A switch's state from its var alone (the hard off): for code that reads
/// no row.
pub(crate) fn var_switch_on(env: &Env, name: &str) -> bool {
    slop_ninja_detect_page::switches::switch(name)
        .is_some_and(|sw| switches::var_state(sw, var(env, name).as_deref()) == "on")
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
        body_ms: ms_var(env, "BODY_WAIT_MS").unwrap_or(d.body_ms),
    }
}

/// `fut`, or `None` once `ms` have passed.
pub(crate) async fn within<T>(ms: u64, fut: impl Future<Output = T>) -> Option<T> {
    match select(Box::pin(fut), Box::pin(Delay::from(Duration::from_millis(ms)))).await {
        Either::Left((v, _)) => Some(v),
        Either::Right(_) => None,
    }
}

// --------------------------------------------------------------- metrics

/// One content-free data point of the `detect` dataset: the point's name as
/// its index and first blob, then its fields in the schema's order.
pub(crate) fn point(env: &Env, p: ae::Point, fields: &[&str], double: f64) {
    debug_assert_eq!(fields.len(), p.blobs.len());
    let Ok(ds) = env.analytics_engine(METRICS) else { return };
    let blobs = std::iter::once(p.name).chain(fields.iter().copied());
    let _ =
        worker::AnalyticsEngineDataPointBuilder::new().indexes([p.name]).blobs(blobs).add_double(double).write_to(&ds);
}

/// A refusal before any ticket: an AE `submit` point only.
pub(crate) fn submitted(env: &Env, route: RouteId, outcome: &str) {
    point(env, ae::detect::SUBMIT, &[route.as_str(), outcome], 1.0);
}

/// A power-of-two bucket label: `le1`, `le2`, `le4`, ...
fn pow2_bucket(v: u64) -> String {
    format!("le{}", v.max(1).next_power_of_two())
}

/// A service time's bucket, in seconds by powers of two.
fn ms_bucket(ms: u64) -> String {
    format!("{}s", pow2_bucket(ms.div_ceil(1000)))
}

// ------------------------------------------------------------- admission

/// The request headers the admission reads (`routes::forwarded::READ_HEADERS`).
struct Heads {
    method: String,
    route: Option<String>,
    host: Option<String>,
    principal: Option<String>,
    origin: Option<String>,
    fetch_site: Option<String>,
}

pub(crate) fn header(req: &Request, name: &str) -> Option<String> {
    req.headers().get(name).ok().flatten()
}

impl Heads {
    fn of(req: &Request) -> Heads {
        Heads {
            method: req.inner().method(),
            route: header(req, "x-sn-route"),
            host: header(req, "x-sn-host"),
            principal: header(req, "x-sn-principal"),
            origin: header(req, "origin"),
            fetch_site: header(req, "sec-fetch-site"),
        }
    }

    fn arrival(&self) -> Arrival<'_> {
        Arrival {
            method: &self.method,
            sn_route: self.route.as_deref(),
            sn_host: self.host.as_deref(),
            sn_principal: self.principal.as_deref(),
            origin: self.origin.as_deref(),
            sec_fetch_site: self.fetch_site.as_deref(),
        }
    }
}

/// The two public origins (`SITE_ORIGIN`, `SECRET_ORIGIN`).
pub(crate) fn origins(env: &Env) -> Option<Origins> {
    Origins::from_vars(&var(env, "SITE_ORIGIN")?, &var(env, "SECRET_ORIGIN")?).ok()
}

/// A forwarded route's admission, before any body is read.
pub(crate) fn admit(req: &Request, env: &Env, route: RouteId) -> std::result::Result<Admission, RefusalCode> {
    let Some(o) = origins(env) else { return Err(RefusalCode::NotConfigured) };
    forwarded::admit(route, &Heads::of(req).arrival(), &o)
}

/// An internal route's admission and its principal (the owner route's
/// only), before any body is read.
pub(crate) fn admit_internal(
    req: &Request,
    route: slop_ninja_detect_page::sn::InternalRoute,
) -> std::result::Result<Option<slop_ninja_detect_page::Principal>, RefusalCode> {
    forwarded::internal_principal(route, &Heads::of(req).arrival())
}

/// The switches `names`, read from the vars and the store's rows in one
/// statement within `wait_ms`. The vars decide when the rows cannot be read,
/// which writes an AE `error {d1}` point.
pub(crate) async fn read_switches(env: &Env, names: &[&str], wait_ms: u64) -> Switches {
    let rows = async {
        let db = env.d1(STORE).ok()?;
        let binds: Vec<JsValue> = names.iter().map(|n| JsValue::from_str(n)).collect();
        let st = db.prepare(switches::select_sql(names.len())).bind(&binds).ok()?;
        let res = within(wait_ms, st.all()).await?.ok()?;
        res.results::<switches::Row>().ok()
    }
    .await;
    let s = Switches::read(names, |n| var(env, n), rows.as_deref());
    if s.d1_failed() {
        point(env, ae::detect::ERROR, &["d1"], 1.0);
    }
    s
}

// ------------------------------------------------------------------- warm

#[derive(Serialize)]
struct WarmAnswer {
    state: &'static str,
    tool: bool,
    spans: bool,
    /// The fewest words scored: 24, or 150 with spans off.
    min_words: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<PackageLine>,
}

#[derive(Deserialize)]
pub(crate) struct GateWarm {
    pub start: bool,
    pub awake_until: u64,
}

async fn warm(req: &Request, env: &Env) -> web_sys::Response {
    if let Err(code) = admit(req, env, RouteId::Warm) {
        submitted(env, RouteId::Warm, code.as_str());
        return refuse(code);
    }
    let sw = read_switches(env, switches::WARM, waits(env).hold_ms).await;
    let (tool, spans) = (sw.on(switches::TOOL), sw.on(switches::SPANS));
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
        spans,
        min_words: policy::min_words(spans),
        package: package(env).map(|p| PackageLine { digest: p.published, publication_pending: p.pending }),
    };
    json_raw(200, &serde_json::to_vec(&body).unwrap_or_default(), false)
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
    let req =
        cf_io::internal_request(&format!("https://{name}{path}"), Method::Post, text.as_deref(), Some(timeout_ms))
            .ok()?;
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
async fn feedback_call<T: serde::de::DeserializeOwned, B: Serialize>(
    env: &Env,
    path: &str,
    body: &B,
    timeout_ms: u64,
) -> Option<T> {
    object_call::<T, B>(env, FEEDBACK, "feedback", path, Some(body), timeout_ms).await.flatten()
}

/// The gate's client key input: its kind and value. The gate hashes it
/// under its daily key; neither part is kept.
#[derive(Serialize)]
pub(crate) struct ClientIn<'a> {
    /// `prefix` (the address prefix of an anonymous request), `owner` (a
    /// paid account's owner hash) or `api` (an API key's hash).
    pub kind: &'static str,
    pub value: &'a str,
}

#[derive(Serialize)]
struct EnterReq<'a> {
    client: ClientIn<'a>,
    class: &'static str,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    count: Option<Count>,
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
    /// The hold the next ask carries (the position-scaled hold).
    #[serde(default)]
    hold_ms: Option<u64>,
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

    /// The reader is still there (no write needed to find out).
    fn alive(&mut self) -> bool {
        if self.open && writer_errored(&self.writer) {
            self.open = false;
        }
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

/// What the free check knows before its stream starts.
pub(crate) struct Accepted<'a> {
    pkg: Package,
    fwd: String,
    cut: Cut<'a>,
    length: LengthBin,
    spans_on: bool,
    /// `SHA-256(canon_v1(text))` (hex): the free cut runs on the canonical
    /// text, and the seal binds it.
    text_sha256: String,
    /// `free_text` when the pot may count the scan; `None` on an uncovered
    /// path (recorded as `uncovered_path`).
    channel: Option<Channel>,
    started: u64,
}

impl Accepted<'_> {
    /// The files manifest digest the scan runs under.
    pub(crate) fn load(&self) -> &str {
        &self.pkg.load
    }
}

async fn score(mut req: Request, env: Env, answer: &Answer) {
    let started = now_ms();
    let route = RouteId::Score;
    let refused = |code: RefusalCode| {
        submitted(&env, route, code.as_str());
        refuse(code)
    };
    let admission = match admit(&req, &env, route) {
        Ok(a) => a,
        Err(code) => return answer.send(refused(code)),
    };
    if !content_type_ok(header(&req, "content-type").as_deref(), BodyType::Json) {
        return answer.send(refused(RefusalCode::ContentType));
    }
    let w = waits(&env);
    let sw = read_switches(&env, switches::SCORE, w.hold_ms).await;
    if !sw.on(switches::TOOL) {
        return answer.send(refused(RefusalCode::ToolOff));
    }
    let Some(pkg) = package(&env) else { return answer.send(refused(RefusalCode::NotConfigured)) };
    let declared = header(&req, "content-length").and_then(|v| v.trim().parse::<u64>().ok());
    let Some(declared) = declared else { return answer.send(refused(RefusalCode::LengthRequired)) };
    if declared > bound::RAW_BODY_BYTES {
        return answer.send(refused(RefusalCode::TooLarge));
    }
    // The full footprint must fit alone; until the envelope parses, only the
    // intake (the body and the unescaped text) is reserved.
    let reservation = routes::scan_footprint(Some(declared));
    if reservation > routes::SCAN_MEMORY_BYTES {
        return answer.send(refused(RefusalCode::TooLarge));
    }
    let intake = routes::intake_footprint(declared);
    let mut held = match Reserved::take(intake) {
        Ok(h) => h,
        Err(NoRoom::Busy) => return answer.send(refused(RefusalCode::Busy)),
        Err(NoRoom::TooLarge) => return answer.send(refused(RefusalCode::TooLarge)),
    };
    let Ok(limit) = usize::try_from(declared) else { return answer.send(refused(RefusalCode::TooLarge)) };
    let until = started.saturating_add(w.body_ms);
    let body = match cf_io::request_bytes_until(&mut req, limit, Some(until)).await {
        Ok(b) => b,
        Err(e) => return answer.send(refused(body_code(e))),
    };
    let request = match envelope::parse(&body) {
        Ok(e) => e,
        Err(m) => return answer.send(refused(m.code())),
    };
    let text: &str = &request.text;
    // A lone link to a post on X: nothing is scored and no ticket is taken.
    if let Some(id) = xpost::x_post_v1(text) {
        let r = if sw.on(switches::URLS) && sw.on(switches::X) { ApiRefusal::x_post_link(id) } else { None };
        let r = r.unwrap_or_else(|| ApiRefusal::new(RefusalCode::XOff));
        submitted(&env, route, r.code.as_str());
        return answer.send(refusal(&r));
    }
    if held.grow(reservation.saturating_sub(intake)).is_err() {
        return answer.send(refused(RefusalCode::Busy));
    }
    if text.trim().is_empty() {
        return answer.send(refused(RefusalCode::Empty));
    }
    // canon_v1 before the cut: the seal binds the canonical text, and the
    // answer's offsets are mapped back to the string the page sent.
    let canonical = canon::canon_v1(text);
    let spans_on = sw.on(switches::SPANS);
    let accepted = match prepare(&env, route, &canonical, spans_on, pkg, admission.channel(route), reservation, started)
    {
        Ok(a) => a,
        Err(r) => return answer.send(refusal(&r)),
    };
    // Accepted: from here the answer is a stream.
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
        Err(_) => return answer.send(refuse(RefusalCode::Internal)),
    }
    submitted(&env, route, "accepted");
    let ip = header(&req, "cf-connecting-ip");
    match run(&env, ip.as_deref(), &mut lines, &w, &accepted).await {
        Scan::Result(mut line) => {
            // The seal keeps the canonical text's offsets (what a link stores
            // and a preview renders); the line gets the sent string's.
            let full = answer::sealed_result(&mut line, text, &canonical);
            line.seal = result_seal(&env, &accepted, full);
            let _ = lines.line(&line).await;
        }
        Scan::Refused(r) => {
            let _ = lines.line(&r).await;
        }
        Scan::Gone => {}
    }
    lines.close().await;
    drop(held);
}

/// The free check's part before any ticket, on `canonical` (`canon_v1` of
/// the text), within `reservation`: the free cut (at most the first 1,000
/// words, never inside a sentence, within the scorer's tokens, counted
/// exactly), the floor (the span-free minimum with spans off: a short
/// text's rule reads passages) and the English screen, then the scorer's
/// body. A refusal is counted as a `submit` point of `route`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare<'a>(
    env: &Env,
    route: RouteId,
    canonical: &'a str,
    spans_on: bool,
    pkg: Package,
    channel: Option<Channel>,
    reservation: usize,
    started: u64,
) -> std::result::Result<Accepted<'a>, ApiRefusal> {
    let refused = |r: ApiRefusal| {
        submitted(env, route, r.code.as_str());
        r
    };
    // The cut's counts allocate within the reservation.
    let mut fits = |bytes: usize, _align: usize| bytes <= reservation;
    let cut = match cut::free_cut(canonical, &mut fits) {
        Ok(c) => c,
        Err(NoCut::NoSentenceEnd) => {
            return Err(refused(ApiRefusal {
                total_words: Some(u32_of(policy::words(canonical))),
                ..ApiRefusal::new(RefusalCode::NoSentenceEnd)
            }));
        }
        Err(NoCut::Count(_)) => return Err(ApiRefusal::new(RefusalCode::Busy)),
    };
    let length = match policy::length_bin(cut.scored_words) {
        Ok(l) if spans_on || !l.is_short() => l,
        _ => return Err(refused(too_short(&cut, policy::min_words(spans_on)))),
    };
    if english::english_screen(cut.scored).is_some() {
        return Err(refused(of_cut(RefusalCode::NotEnglish, &cut)));
    }
    let fwd = Forward { package: &pkg.load, spans: true, docs: [ForwardDoc { id: "d", text: cut.scored }] };
    let Ok(fwd) = serde_json::to_string(&fwd) else { return Err(ApiRefusal::new(RefusalCode::Internal)) };
    if fwd.len() as u64 > bound::SCORER_MAX_BODY {
        return Err(refused(ApiRefusal { tokens: Some(u32_of(cut.tokens)), ..ApiRefusal::new(RefusalCode::TooLong) }));
    }
    Ok(Accepted {
        pkg,
        fwd,
        text_sha256: hex::encode(canon::canonical_sha256(canonical)),
        cut,
        length,
        spans_on,
        channel,
        started,
    })
}

/// The seal of a result: `None` when the store secret is not configured or
/// the seal fails (the result is shown, but cannot be shared or quoted).
/// `full`'s offsets are the canonical text's (`answer::sealed_result`).
fn result_seal(env: &Env, a: &Accepted<'_>, full: answer::ResultFull) -> Option<String> {
    let Some(secret) = store_secret(env) else {
        point(env, ae::detect::ERROR, &["seal_not_configured"], 1.0);
        return None;
    };
    let now = now_ms();
    let payload = ResultPayload {
        day: seal::day_of(now),
        text_sha256: a.text_sha256.clone(),
        scored_end: full.scored_end,
        cut: a.cut.kind.code().to_owned(),
        package_load: a.pkg.load.clone(),
        policy_v: seal::POLICY_V.to_owned(),
        result_full: full,
        issued_ms: now,
    };
    let mut nonce = [0u8; seal::NONCE_BYTES];
    random_bytes(seal::NONCE_BYTES as u32).copy_to(&mut nonce);
    let sealed = seal::seal(&secret, seal::Purpose::Result, payload.day, nonce, &payload).ok();
    if sealed.is_none() {
        point(env, ae::detect::ERROR, &["seal"], 1.0);
    }
    sealed
}

/// The store secret's 32 bytes (`DETECT_STORE_SECRET`, 64 hex digits,
/// trimmed); `None` when unset or of another form.
pub(crate) fn store_secret(env: &Env) -> Option<[u8; seal::SECRET_BYTES]> {
    env.secret(STORE_SECRET).ok().and_then(|s| seal::parse_secret(s.to_string().trim()))
}

/// Where a scan's progress goes: the free check's NDJSON stream, or nowhere
/// (a re-run answers once, at the end).
#[allow(async_fn_in_trait)]
pub(crate) trait Progress {
    /// Writes one line; `false` when the reader is gone.
    async fn line<T: Serialize>(&mut self, v: &T) -> bool;
    /// The reader is still there.
    fn alive(&mut self) -> bool;
}

impl Progress for Lines {
    async fn line<T: Serialize>(&mut self, v: &T) -> bool {
        Lines::line(self, v).await
    }

    fn alive(&mut self) -> bool {
        Lines::alive(self)
    }
}

/// No progress lines.
pub(crate) struct Quiet;

impl Progress for Quiet {
    async fn line<T: Serialize>(&mut self, _v: &T) -> bool {
        true
    }

    fn alive(&mut self) -> bool {
        true
    }
}

/// What a scan came to.
pub(crate) enum Scan {
    /// The result line, offsets into the canonical text, without its seal.
    Result(answer::Answer),
    /// A refusal for the reader.
    Refused(ApiRefusal),
    /// The reader left: nothing to answer.
    Gone,
}

/// One scan of an accepted text: a gate ticket (class interactive, keyed by
/// the address prefix), the lane, the answer's allow-list. The release that
/// ends it carries its content-free count.
pub(crate) async fn run(env: &Env, ip: Option<&str>, lines: &mut impl Progress, w: &Waits, a: &Accepted<'_>) -> Scan {
    let (cut, length) = (&a.cut, a.length);
    let hold = w.hold_ms;
    let prefix = ip.and_then(crate::address_prefix).unwrap_or_else(|| "none".into());
    let class = TicketClass::Interactive;
    let enter = EnterReq { client: ClientIn { kind: "prefix", value: &prefix }, class: class.as_str(), hold_ms: hold };
    let Some(mut st) = gate_call::<GateStatus, _>(env, "/enter", Some(&enter), hold).await else {
        point(env, ae::detect::ERROR, &[RefusalCode::Unavailable.as_str()], 1.0);
        return Scan::Refused(ApiRefusal::new(RefusalCode::Unavailable));
    };
    let depth = st.position.unwrap_or(0);
    point(env, ae::detect::QUEUE, &[pow2_bucket(depth as u64).as_str()], depth as f64);
    let ticket_id = std::mem::take(&mut st.ticket);
    let ticket = ticket_id.as_str();
    // What a release counts: the outcome, on the channel the pot reads, or
    // under `uncovered_path` when the scan did not arrive through it.
    let count = |outcome: &str| {
        let outcome = match (a.channel, outcome) {
            (None, counters::SCORED) => counters::UNCOVERED_PATH,
            (_, o) => o,
        };
        Some(Count {
            channel: a.channel.unwrap_or(Channel::FreeText).as_str().to_owned(),
            package_load: a.pkg.load.clone(),
            outcome: outcome.to_owned(),
        })
    };
    let done = |served_ms: Option<u64>, count: Option<Count>| DoneReq { ticket, served_ms, count };
    let refused = |code: RefusalCode| Scan::Refused(ApiRefusal::new(code));
    loop {
        match st.status.as_str() {
            "admitted" => break,
            "queued" => {
                let q = Queued { state: "queued", position: st.position.unwrap_or(1), estimate_ms: st.estimate_ms };
                if !lines.line(&q).await {
                    let _ = gate_tell(env, "/leave", &done(None, None), hold).await;
                    return Scan::Gone;
                }
                // The gate holds the ask until the ticket is admitted or the
                // (position-scaled) hold ends.
                let wait = st.hold_ms.unwrap_or(hold).max(hold);
                let ask = AskReq { ticket, hold_ms: wait };
                match gate_call::<GateStatus, _>(env, "/ask", Some(&ask), wait.saturating_mul(2)).await {
                    Some(next) => st = next,
                    None => {
                        point(env, ae::detect::ERROR, &[RefusalCode::Unavailable.as_str()], 1.0);
                        return refused(RefusalCode::Unavailable);
                    }
                }
            }
            "busy" => {
                point(env, ae::detect::ERROR, &[RefusalCode::Busy.as_str()], 1.0);
                return refused(RefusalCode::Busy);
            }
            _ => {
                point(env, ae::detect::ERROR, &[RefusalCode::ExpiredPlace.as_str()], 1.0);
                return refused(RefusalCode::ExpiredPlace);
            }
        }
    }
    // The admission's clock: the lane gets what is left of its bound.
    let admitted_at = now_ms();
    if !st.tool {
        let _ = gate_tell(env, "/done", &done(None, count(RefusalCode::ToolOff.as_str())), hold).await;
        return refused(RefusalCode::ToolOff);
    }
    // A reader that left while its ticket waited costs no scan.
    if !lines.alive() {
        let _ = gate_tell(env, "/done", &done(None, None), hold).await;
        return Scan::Gone;
    }
    let cold = !st.awake;
    if cold && !lines.line(&Phase { state: "starting" }).await {
        let _ = gate_tell(env, "/done", &done(None, None), hold).await;
        return Scan::Gone;
    }
    let t0 = now_ms();
    let scored = match w.lane_left_ms(t0.saturating_sub(admitted_at)) {
        Some(left) => lane_score(env, &a.fwd, left, hold).await,
        None => Err(LaneFail::Restarting),
    };
    let served = now_ms().saturating_sub(t0);
    let reply = match scored {
        Ok(r) => r,
        Err(f) => {
            let code = match f {
                LaneFail::Busy => RefusalCode::Busy,
                LaneFail::Restarting => RefusalCode::Restarting,
                LaneFail::Unavailable => RefusalCode::Unavailable,
                LaneFail::Refused => RefusalCode::Scorer,
            };
            let _ = gate_tell(env, "/done", &done(None, count(code.as_str())), hold).await;
            point(env, ae::detect::ERROR, &[code.as_str()], served as f64);
            return refused(code);
        }
    };
    let outcome = answer::read(&reply, &a.pkg.load, cut.scored, a.spans_on, length);
    drop(reply);
    match outcome {
        Outcome::Scored(scored) => {
            let band = scored.band;
            // The slot is freed and the receipt issued at once: the receipt
            // comes from the feedback object, which answers within a hold or
            // the result carries no receipt.
            let issue = IssueReq { band: band.code(), length: length.code() };
            let (_, receipt) = futures_util::future::join(
                gate_tell(env, "/done", &done(Some(served), count(counters::SCORED)), hold),
                feedback_call::<IssueReply, _>(env, "/issue", &issue, hold),
            )
            .await;
            let receipt = receipt.and_then(|r| r.receipt).unwrap_or_default();
            let line = answer::answer(
                scored,
                length,
                cut,
                PackageLine { digest: a.pkg.published.clone(), publication_pending: a.pkg.pending },
                receipt,
            );
            let ms = now_ms().saturating_sub(a.started);
            point(
                env,
                ae::detect::SCAN,
                &[class.as_str(), length.code(), band.code(), ms_bucket(served).as_str()],
                ms as f64,
            );
            Scan::Result(line)
        }
        Outcome::ShortWithoutSpans => {
            let _ = gate_tell(env, "/done", &done(Some(served), count(RefusalCode::TooShort.as_str())), hold).await;
            point(env, ae::detect::ERROR, &["short_without_spans"], served as f64);
            Scan::Refused(too_short(cut, bound::MEASURED_MIN_WORDS))
        }
        Outcome::Declined(code) => {
            let code = RefusalCode::parse(code).unwrap_or(RefusalCode::Scorer);
            let _ = gate_tell(env, "/done", &done(Some(served), count(code.as_str())), hold).await;
            Scan::Refused(of_cut(code, cut))
        }
        Outcome::Fault => {
            let _ = gate_tell(env, "/done", &done(None, count(RefusalCode::Scorer.as_str())), hold).await;
            point(env, ae::detect::ERROR, &[RefusalCode::Scorer.as_str()], served as f64);
            refused(RefusalCode::Scorer)
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
    if let Err(code) = admit(&req, env, RouteId::Feedback) {
        submitted(env, RouteId::Feedback, code.as_str());
        return refuse(code);
    }
    let body = match cf_io::request_bytes(&mut req, routes::FEEDBACK_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => return refuse(body_code(e)),
    };
    let Ok(f) = serde_json::from_slice::<FeedbackReq<'_>>(&body) else { return refuse(RefusalCode::MalformedRequest) };
    let Some(claim) = crate::receipt::Claim::from_code(f.claim) else { return refuse(RefusalCode::MalformedRequest) };
    // A receipt of the wrong shape never leaves this invocation; any other
    // goes to the feedback object, never to the admission gate.
    if crate::receipt::parse(f.receipt).is_none() {
        return refuse(RefusalCode::NotFound);
    }
    let fwd = FeedbackFwd { receipt: f.receipt, claim: claim.code() };
    match feedback_call::<FeedbackReply, _>(env, "/vote", &fwd, waits(env).hold_ms).await {
        Some(r) => {
            // Content-free: the band, length bin, package and button.
            if let (Some(b), Some(l), Ok(ds)) =
                (Band::from_code(&r.band), LengthBin::from_code(&r.length), env.analytics_engine(METRICS))
            {
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
        None => refuse(RefusalCode::NotFound),
    }
}
