//! The account Worker on Cloudflare (wasm32 only): its hand-written
//! exports on the bounded arena, the `QuotaLimiter` class, and the
//! [`Host`] over its bindings.
//!
//! Every export takes a glue ticket (allocation-free), sets the arena clock,
//! then opens the invocation's scope with its route's footprint
//! (`memory.rs`), chosen in JS from the path before any Rust allocation. A
//! request that cannot be admitted answers a JS-only 503.
//!
//! Bindings: D1 `ACCOUNT_STORE` (one `first-primary` session per request,
//! so revocations take effect at once), the `QUOTA_LIMITER` namespace, the
//! `SIGNIN_EMAIL` mail binding, the `DETECT_JOBS` queue producer and the
//! `ACCOUNT_METRICS` dataset. A dev build also reads `AUTH_STUB` (provider
//! calls under `STUB_ORIGIN`, and mail capture). No service binding exists
//! otherwise: this Worker calls no other Worker.

use std::rc::Rc;

use js_sys::{Array, Reflect, Uint8Array};
use slop_ninja_arena::{ArenaAlloc, AsyncRoute, glue};
use slop_ninja_auth_core::mail::{Mailer, Message, OtherCode, SendFailure};
use slop_ninja_auth_core::policy::{BucketState, TokenBucket};
use slop_ninja_detect_page::ae::Point;
use slop_ninja_detect_page::queue::QueueMessage;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use worker::worker_sys::{self, web_sys};
use worker::{Env, Request, Response, ResponseBody, State};

use crate::cf_io::{self, Settled, now_ms};
use crate::config::{Config, ConfigError};
use crate::host::{
    DbError, Host, NetError, OutReply, OutRequest, QueueUnavailable, QuotaTake, QuotaUnavailable, Row, Stmt, Val,
    classify,
};
use crate::http::{Req, Resp};
use crate::memory;
use crate::quota::{self, QuotaAnswer, QuotaCall};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

/// A D1 call's budget, ms.
const DB_MS: u64 = 10_000;
/// A quota object's call budget, ms.
const QUOTA_MS: u64 = 5_000;

#[wasm_bindgen(inline_js = "
export function account_route_class(req) {
  let u;
  try { u = new URL(req.url); } catch (e) { return 0; }
  const p = u.pathname;
  if (p === '/internal/v1/view' || p === '/account/export' || p === '/api/account/v1/op') return 2;
  if (p.startsWith('/internal/v1/') || p.startsWith('/api/account/v1/') || p.startsWith('/auth/') || p.startsWith('/account/')) return 1;
  return 0;
}
export function account_random(n) { const b = new Uint8Array(n); crypto.getRandomValues(b); return b; }
export function account_d1(env) {
  const db = env.ACCOUNT_STORE;
  if (!db) return null;
  return typeof db.withSession === 'function' ? db.withSession('first-primary') : db;
}
function account_norm(v) {
  if (v === null || v === undefined) return null;
  if (typeof v === 'number' || typeof v === 'string') return v;
  if (typeof v === 'bigint') return Number(v);
  if (v instanceof ArrayBuffer) return new Uint8Array(v);
  if (ArrayBuffer.isView(v)) return new Uint8Array(v.buffer, v.byteOffset, v.byteLength);
  if (Array.isArray(v)) return Uint8Array.from(v);
  return null;
}
function account_rows(r) {
  const out = [];
  for (const row of (r && r.results) || []) {
    const cols = Object.keys(row);
    out.push([cols, cols.map((c) => account_norm(row[c]))]);
  }
  return out;
}
function account_bind(args) { return args.map((a) => (a instanceof Uint8Array ? a.slice().buffer : a)); }
function account_err(e) {
  const c = e && e.cause && e.cause.message;
  return { err: String(c || (e && e.message) || e) };
}
export async function account_d1_all(s, sql, args) {
  try { return { rows: account_rows(await s.prepare(sql).bind(...account_bind(args)).all()) }; }
  catch (e) { return account_err(e); }
}
export async function account_d1_batch(s, stmts) {
  try {
    const rs = await s.batch(stmts.map((x) => s.prepare(x[0]).bind(...account_bind(x[1]))));
    return { sets: rs.map((r) => account_rows(r)) };
  } catch (e) { return account_err(e); }
}
export function account_out_fetch(url, method, flat, body, ms, env, stub) {
  const h = new Headers();
  for (let i = 0; i + 1 < flat.length; i += 2) h.append(flat[i], flat[i + 1]);
  const init = { method, headers: h, redirect: 'manual', signal: AbortSignal.timeout(ms) };
  if (body !== null && body !== undefined) init.body = body;
  let p;
  try { p = stub && env.AUTH_STUB ? env.AUTH_STUB.fetch(url, init) : fetch(url, init); }
  catch (e) { return Promise.resolve({ err: 'failed' }); }
  return p.then((r) => r, (e) => ({ err: e && (e.name === 'TimeoutError' || e.name === 'AbortError') ? 'timeout' : 'failed' }));
}
export function account_ae(env, name, blobs) {
  const ds = env.ACCOUNT_METRICS;
  if (!ds) return;
  try { ds.writeDataPoint({ indexes: [name], blobs: [name, ...blobs] }); } catch (e) {}
}
export function account_cron(e) { try { return String((e && e.cron) || ''); } catch (x) { return ''; } }
")]
extern "C" {
    /// 2 a list route, 1 another account route, else 0 (read in JS, no
    /// Rust allocation).
    fn account_route_class(req: &web_sys::Request) -> u32;
    fn account_random(n: u32) -> Uint8Array;
    fn account_d1(env: &Env) -> JsValue;
    fn account_d1_all(s: &JsValue, sql: &str, args: &Array) -> js_sys::Promise;
    fn account_d1_batch(s: &JsValue, stmts: &Array) -> js_sys::Promise;
    fn account_out_fetch(
        url: &str,
        method: &str,
        flat: &Array,
        body: &JsValue,
        ms: f64,
        env: &Env,
        stub: bool,
    ) -> js_sys::Promise;
    fn account_ae(env: &Env, name: &str, blobs: &Array);
    fn account_cron(e: &JsValue) -> String;
}

/// A bodiless answer built in JS (no Rust allocation).
fn status_only(status: u16) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    if let Ok(h) = web_sys::Headers::new() {
        let _ = h.set("Cache-Control", "private, no-store");
        init.set_headers(&h);
    }
    web_sys::Response::new_with_opt_str_and_init(None, &init).unwrap_or_else(|_| web_sys::Response::error())
}

fn resolve(r: web_sys::Response) -> js_sys::Promise {
    let v: JsValue = r.into();
    js_sys::Promise::resolve(&v)
}

fn to_web(r: Resp) -> web_sys::Response {
    let Ok(out) = Response::from_body(ResponseBody::Body(r.body)) else { return status_only(500) };
    let out = out.with_status(r.status);
    let h = out.headers();
    for (k, v) in &r.headers {
        let ok = if k == "set-cookie" { h.append(k, v) } else { h.set(k, v) };
        if ok.is_err() {
            return status_only(500);
        }
    }
    worker::IntoResponse::into_raw(out).unwrap_or_else(|_| status_only(500))
}

fn config(env: &Env) -> Result<Config, ConfigError> {
    Config::from_vars(|k| env.var(k).ok().map(|v| v.to_string()).or_else(|| env.secret(k).ok().map(|s| s.to_string())))
}

/// The runtime behind one invocation.
struct CfHost {
    env: Env,
    db: JsValue,
    stub_origin: Option<String>,
}

fn encode_args(args: &[Val]) -> Array {
    args.iter()
        .map(|v| match v {
            Val::Null => JsValue::NULL,
            Val::Int(i) => JsValue::from_f64(*i as f64),
            Val::Text(t) => JsValue::from_str(t),
            Val::Blob(b) => Uint8Array::from(b.as_slice()).into(),
        })
        .collect()
}

fn decode_val(v: &JsValue) -> Val {
    if v.is_null() || v.is_undefined() {
        Val::Null
    } else if let Some(f) = v.as_f64() {
        Val::Int(f as i64)
    } else if let Some(s) = v.as_string() {
        Val::Text(s)
    } else if let Some(u) = v.dyn_ref::<Uint8Array>() {
        Val::Blob(u.to_vec())
    } else {
        Val::Null
    }
}

fn decode_rows(set: &JsValue) -> Vec<Row> {
    let Some(arr) = set.dyn_ref::<Array>() else { return Vec::new() };
    arr.iter()
        .filter_map(|row| {
            let pair = row.dyn_into::<Array>().ok()?;
            let cols = pair.get(0).dyn_into::<Array>().ok()?;
            let vals = pair.get(1).dyn_into::<Array>().ok()?;
            Some(Row {
                cols: cols
                    .iter()
                    .zip(vals.iter())
                    .map(|(c, v)| (c.as_string().unwrap_or_default(), decode_val(&v)))
                    .collect(),
            })
        })
        .collect()
}

fn get(v: &JsValue, k: &str) -> JsValue {
    Reflect::get(v, &JsValue::from_str(k)).unwrap_or(JsValue::UNDEFINED)
}

async fn settle_db(p: js_sys::Promise) -> Result<JsValue, DbError> {
    match cf_io::settle(p, Some(now_ms() + DB_MS)).await {
        Settled::Value(v) => {
            let err = get(&v, "err");
            match err.as_string() {
                Some(m) => Err(classify(&m)),
                None => Ok(v),
            }
        }
        Settled::Error(_) | Settled::Timeout => Err(DbError::Unavailable),
    }
}

impl CfHost {
    async fn quota_call(&self, name: &str, call: QuotaCall) -> Result<QuotaAnswer, QuotaUnavailable> {
        let ns = self.env.durable_object("QUOTA_LIMITER").map_err(|_| QuotaUnavailable)?;
        let stub = ns.get_by_name(name).map_err(|_| QuotaUnavailable)?;
        let body = serde_json::to_string(&call).map_err(|_| QuotaUnavailable)?;
        let req = cf_io::internal_request("https://quota-limiter/", worker::Method::Post, Some(&body), Some(QUOTA_MS))
            .map_err(|_| QuotaUnavailable)?;
        let mut resp = stub.fetch_with_request(req).await.map_err(|_| QuotaUnavailable)?;
        if resp.status_code() != 200 {
            cf_io::discard(&mut resp);
            return Err(QuotaUnavailable);
        }
        let bytes = cf_io::response_bytes(&mut resp, 1024).await.map_err(|_| QuotaUnavailable)?;
        serde_json::from_slice(&bytes).map_err(|_| QuotaUnavailable)
    }
}

impl Host for CfHost {
    async fn all(&self, stmt: Stmt) -> Result<Vec<Row>, DbError> {
        if self.db.is_null() {
            return Err(DbError::Unavailable);
        }
        let v = settle_db(account_d1_all(&self.db, &stmt.sql, &encode_args(&stmt.args))).await?;
        Ok(decode_rows(&get(&v, "rows")))
    }

    async fn batch(&self, stmts: Vec<Stmt>) -> Result<Vec<Vec<Row>>, DbError> {
        if self.db.is_null() {
            return Err(DbError::Unavailable);
        }
        let list: Array = stmts
            .iter()
            .map(|s| -> JsValue { Array::of2(&JsValue::from_str(&s.sql), &encode_args(&s.args)).into() })
            .collect();
        let v = settle_db(account_d1_batch(&self.db, &list)).await?;
        let sets = get(&v, "sets");
        let Some(sets) = sets.dyn_ref::<Array>() else { return Err(DbError::Unavailable) };
        Ok(sets.iter().map(|s| decode_rows(&s)).collect())
    }

    async fn quota_take(&self, name: &str, size: TokenBucket) -> Result<QuotaTake, QuotaUnavailable> {
        let call = QuotaCall::Take { capacity: size.capacity, refill_every_s: size.refill_every_s };
        self.quota_call(name, call).await.map(Into::into)
    }

    async fn quota_reset(&self, name: &str) -> Result<(), QuotaUnavailable> {
        self.quota_call(name, QuotaCall::Reset).await.map(|_| ())
    }

    async fn send_mail(&self, m: &Message) -> Result<(), SendFailure> {
        #[cfg(feature = "dev")]
        if let Ok(stub) = self.env.service("AUTH_STUB") {
            return slop_ninja_auth_core::mail::capture::ServiceCapture::new(stub).send(m).await;
        }
        let binding = self.env.send_email("SIGNIN_EMAIL").map_err(|_| SendFailure::Other(OtherCode::Unknown))?;
        slop_ninja_auth_core::mail::cloudflare::CloudflareMailer::new(binding).send(m).await
    }

    async fn http(&self, req: &OutRequest) -> Result<OutReply, NetError> {
        let flat: Array = req.headers.iter().flat_map(|(k, v)| [JsValue::from_str(k), JsValue::from_str(v)]).collect();
        let body = if req.body.is_empty() { JsValue::NULL } else { JsValue::from_str(&req.body) };
        let stub = self.stub_origin.as_deref().is_some_and(|o| req.url.starts_with(o));
        let p = account_out_fetch(&req.url, req.method, &flat, &body, req.timeout_ms as f64, &self.env, stub);
        let v = JsFuture::from(p).await.map_err(|_| NetError::Failed)?;
        match get(&v, "err").as_string().as_deref() {
            Some("timeout") => return Err(NetError::Timeout),
            Some(_) => return Err(NetError::Failed),
            None => {}
        }
        let raw = v.dyn_into::<web_sys::Response>().map_err(|_| NetError::Failed)?;
        let mut resp = Response::from(raw);
        let status = resp.status_code();
        match cf_io::response_bytes(&mut resp, req.max_body).await {
            Ok(b) => Ok(OutReply { status, body: Some(b) }),
            Err(cf_io::BodyError::TooLarge) => {
                cf_io::discard(&mut resp);
                Ok(OutReply { status, body: None })
            }
            Err(cf_io::BodyError::Timeout) => Err(NetError::Timeout),
            Err(_) => Err(NetError::Failed),
        }
    }

    async fn enqueue(&self, msg: &QueueMessage) -> Result<(), QueueUnavailable> {
        let q = self.env.queue("DETECT_JOBS").map_err(|_| QueueUnavailable)?;
        let text = serde_json::to_string(msg).map_err(|_| QueueUnavailable)?;
        let value = js_sys::JSON::parse(&text).map_err(|_| QueueUnavailable)?;
        let send = worker::RawMessageBuilder::new(value).build_with_content_type(worker::QueueContentType::Json);
        q.send_raw(send).await.map_err(|_| QueueUnavailable)
    }

    fn point(&self, point: &Point, blobs: &[&str]) {
        let arr: Array = blobs.iter().map(|b| JsValue::from_str(b)).collect();
        account_ae(&self.env, point.name, &arr);
    }

    fn fill(&self, buf: &mut [u8]) {
        let n = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        account_random(n).copy_to(buf);
    }

    fn now_ms(&self) -> u64 {
        now_ms()
    }
}

fn host_of(env: Env, cfg: &Result<Config, ConfigError>) -> CfHost {
    let db = account_d1(&env);
    let stub_origin = cfg.as_ref().ok().and_then(|c| c.stub_origin.as_ref()).map(|o| o.as_str().to_owned());
    CfHost { env, db, stub_origin }
}

async fn handle_fetch(mut req: Request, env: Env) -> web_sys::Response {
    let Ok(url) = req.url() else { return status_only(400) };
    let method = req.method().to_string();
    let headers: Vec<(String, String)> = req.headers().entries().map(|(k, v)| (k.to_ascii_lowercase(), v)).collect();
    let bound = if url.path() == "/api/account/v1/op" { crate::ops::OP_BODY_MAX } else { memory::BODY_BYTES };
    let body = if method == "GET" || method == "HEAD" {
        Vec::new()
    } else {
        match cf_io::request_bytes(&mut req, bound).await {
            Ok(b) => b,
            Err(cf_io::BodyError::TooLarge) => return status_only(413),
            Err(_) => return status_only(400),
        }
    };
    let r = Req { method, path: url.path().to_owned(), query: url.query().unwrap_or("").to_owned(), headers, body };
    let cfg = config(&env);
    let host = host_of(env, &cfg);
    to_web(crate::routes::handle(&cfg, &host, &r).await)
}

/// The Worker's fetch export.
#[wasm_bindgen]
pub fn fetch(req: web_sys::Request, env: Env, _ctx: worker_sys::Context) -> js_sys::Promise {
    crate::silence_panics();
    let Ok(ticket) = glue::Ticket::admit() else { return resolve(status_only(503)) };
    let _ = slop_ninja_arena::tick(now_ms());
    let (route, control): (AsyncRoute, usize) = match account_route_class(&req) {
        2 => (memory::R_LIST, memory::LIST_CONTROL_BYTES),
        1 => (memory::R_API, memory::API_CONTROL_BYTES),
        _ => (memory::R_OTHER, memory::SMALL_CONTROL_BYTES),
    };
    wasm_bindgen_futures::future_to_promise(async move {
        let _ticket = ticket;
        let res = match slop_ninja_arena::enter(route, control, move |_scope| async move {
            handle_fetch(Request::from(req), env).await
        }) {
            Ok(e) => e.await.0,
            Err(_) => status_only(503),
        };
        Ok(res.into())
    })
}

/// The cron export: the 5-minute flow sweep and the hourly session sweep.
#[wasm_bindgen]
pub fn scheduled(event: JsValue, env: Env, _ctx: JsValue) -> js_sys::Promise {
    crate::silence_panics();
    let Ok(ticket) = glue::Ticket::admit() else { return js_sys::Promise::resolve(&JsValue::UNDEFINED) };
    let _ = slop_ninja_arena::tick(now_ms());
    let cron = account_cron(&event);
    wasm_bindgen_futures::future_to_promise(async move {
        let _ticket = ticket;
        if let Ok(e) =
            slop_ninja_arena::enter(memory::R_SCHEDULED, memory::SCHEDULED_CONTROL_BYTES, move |_s| async move {
                let cfg = config(&env);
                let host = host_of(env, &cfg);
                crate::sweep::run(&host, &cron).await;
            })
        {
            let _ = e.await;
        }
        Ok(JsValue::UNDEFINED)
    })
}

// ------------------------------------------------------- QuotaLimiter

/// One bucket's stored state, with the size it was taken under (so the
/// alarm can tell whether the bucket is full again).
#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    state: BucketState,
    capacity: u32,
    refill_every_s: u64,
}

const KEY: &str = "b";

async fn load(state: &State) -> Option<Stored> {
    let text: String = state.storage().get(KEY).await.ok().flatten()?;
    serde_json::from_str(&text).ok()
}

async fn quota_fetch(state: Rc<State>, mut req: Request) -> web_sys::Response {
    let Ok(raw) = cf_io::request_bytes(&mut req, 512).await else { return status_only(400) };
    let Ok(call) = serde_json::from_slice::<QuotaCall>(&raw) else { return status_only(400) };
    let stored = load(&state).await;
    let now = now_ms();
    let next = quota::apply(stored.as_ref().map(|s| s.state), call, now);
    let storage = state.storage();
    let ok = match (next.state, call) {
        (Some(s), QuotaCall::Take { capacity, refill_every_s }) => {
            let text = serde_json::to_string(&Stored { state: s, capacity, refill_every_s }).unwrap_or_default();
            let put = storage.put(KEY, text).await.is_ok();
            let alarm = match next.full_at_ms {
                Some(at) => storage.set_alarm(i64::try_from(at).unwrap_or(i64::MAX)).await.is_ok(),
                None => true,
            };
            put && alarm
        }
        _ => {
            let _ = storage.delete_alarm().await;
            storage.delete_all().await.is_ok()
        }
    };
    if !ok {
        return status_only(503);
    }
    let body = serde_json::to_vec(&next.answer).unwrap_or_default();
    to_web(Resp { status: 200, headers: vec![("content-type".into(), "application/json".into())], body })
}

async fn quota_alarm(state: Rc<State>) {
    let now = now_ms();
    let storage = state.storage();
    match load(&state).await {
        Some(s) => {
            let size = TokenBucket { capacity: s.capacity, refill_every_s: s.refill_every_s };
            match quota::full_at(&size, &s.state) {
                Some(at) if at > now => {
                    let _ = storage.set_alarm(i64::try_from(at).unwrap_or(i64::MAX)).await;
                }
                _ => {
                    let _ = storage.delete_all().await;
                }
            }
        }
        None => {
            let _ = storage.delete_all().await;
        }
    }
}

/// The `QuotaLimiter` Durable Object class (one bucket per object).
#[wasm_bindgen(js_name = QuotaLimiter)]
pub struct QuotaLimiterExport {
    state: Rc<State>,
    _resident: glue::Resident,
}

#[wasm_bindgen(js_class = QuotaLimiter)]
impl QuotaLimiterExport {
    /// Past the resident count the constructor throws and the caller
    /// retries.
    #[wasm_bindgen(constructor)]
    pub fn new(state: worker_sys::DurableObjectState, _env: Env) -> Result<QuotaLimiterExport, JsValue> {
        crate::silence_panics();
        let resident = glue::Resident::admit().map_err(|_| JsValue::from_str("arena: resident limit"))?;
        Ok(QuotaLimiterExport { state: Rc::new(State::from(state)), _resident: resident })
    }

    #[wasm_bindgen(js_name = fetch)]
    pub fn fetch(&self, req: web_sys::Request) -> js_sys::Promise {
        let Ok(ticket) = glue::Ticket::admit() else { return resolve(status_only(503)) };
        let _ = slop_ninja_arena::tick(now_ms());
        let state = self.state.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let _ticket = ticket;
            let res =
                match slop_ninja_arena::enter(memory::R_QUOTA, memory::SMALL_CONTROL_BYTES, move |_s| async move {
                    quota_fetch(state, Request::from(req)).await
                }) {
                    Ok(e) => e.await.0,
                    Err(_) => status_only(503),
                };
            Ok(res.into())
        })
    }

    #[wasm_bindgen(js_name = alarm)]
    pub fn alarm(&self) -> js_sys::Promise {
        let Ok(ticket) = glue::Ticket::admit() else {
            return js_sys::Promise::reject(&JsValue::from_str("arena_busy"));
        };
        let _ = slop_ninja_arena::tick(now_ms());
        let state = self.state.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let _ticket = ticket;
            match slop_ninja_arena::enter(memory::R_QUOTA_ALARM, memory::SMALL_CONTROL_BYTES, move |_s| async move {
                quota_alarm(state).await;
            }) {
                Ok(e) => {
                    let _ = e.await;
                    Ok(JsValue::UNDEFINED)
                }
                Err(_) => Err(JsValue::from_str("arena_refused")),
            }
        })
    }
}
