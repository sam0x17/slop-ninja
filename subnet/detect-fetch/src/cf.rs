//! The fetch Worker on Cloudflare (wasm32 only): its `fetch` export and the
//! outbound network.
//!
//! Routes (reached only through the detect Worker's service binding):
//! - `POST /fetch` [`FetchRequest`] → [`slop_ninja_detect_fetch_wire::FetchAnswer`];
//! - `POST /robots` [`RobotsRequest`] → [`slop_ninja_detect_fetch_wire::RobotsAnswer`];
//! - a request carrying any `x-sn-` header (only the public forwarders set
//!   them, and they never reach this Worker): 400; another method on a
//!   route: 405; anything else: 404.
//!
//! Configuration (vars and one secret, no binding): `DEPLOY_ENV`
//! (`production` or `staging`), `SITE_ORIGIN` and `SECRET_ORIGIN` (our own
//! hosts, and the signature agent's origin), `URL_FIXTURE_PREFIX` (read only
//! when `DEPLOY_ENV` is `staging`), and the secret `WEB_BOT_AUTH_KEY`, without
//! which a release build answers 503 and fetches nothing. Dev builds also
//! read `DEV_FIXTURE_HOST` and the `FIXTURE_SITE` service binding.
//!
//! Every answer carries `Cache-Control: no-store`; nothing writes a console
//! line.

use crate::cf_io::{self, Settled, now_ms};
use crate::fetch::{self, Ctx, MAX_WAIT_MS};
use crate::net::{Head, Headers, MAX_HEADER_BYTES, MAX_HEADERS, Net, NetError};
use crate::routes::{self, FetchMemory};
use crate::sign::Signer;
use crate::{ENV_STAGING, bound, silence_panics};
use js_sys::Uint8Array;
use serde::Serialize;
use slop_ninja_arena::{ArenaAlloc, AsyncRoute, PinKind, PoisonCause, Scope, glue, model};
use slop_ninja_detect_fetch_wire::{FetchRequest, REQUEST_BYTES, RobotsRequest, SLOT_BYTES};
use slop_ninja_detect_url::{Checked, FixturePrefix, Policy, check};
use std::cell::{Cell, RefCell};
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::wasm_bindgen;
use worker::worker_sys::{self, web_sys};
use worker::{Env, Request, Response};
use zeroize::Zeroizing;

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

thread_local! {
    static MEMORY: FetchMemory = const { FetchMemory::new() };
    /// The extraction stack's statics home (opened once, never closed).
    static HOME: RefCell<Option<Scope>> = const { RefCell::new(None) };
    static HOME_TRIED: Cell<bool> = const { Cell::new(false) };
}

/// The signing key's secret.
const WEB_BOT_AUTH_KEY: &str = "WEB_BOT_AUTH_KEY";

#[wasm_bindgen(inline_js = "
export function fetch_route(req) {
  let u;
  try { u = new URL(req.url); } catch (e) { return 0; }
  for (const k of req.headers.keys()) { if (k.startsWith('x-sn-')) return 8; }
  if (u.search !== '') return 0;
  const post = req.method === 'POST';
  if (u.pathname === '/fetch') return post ? 1 : 9;
  if (u.pathname === '/robots') return post ? 2 : 9;
  return 0;
}
export function out_fetch(url, flat, ms, binding) {
  const h = new Headers();
  for (let i = 0; i + 1 < flat.length; i += 2) h.append(flat[i], flat[i + 1]);
  const init = { method: 'GET', headers: h, redirect: 'manual', signal: AbortSignal.timeout(ms) };
  try {
    return binding ? binding.fetch(url, init) : globalThis.fetch(url, init);
  } catch (e) {
    return Promise.reject(e);
  }
}
export function head_of(res) {
  const out = [res.status];
  for (const [k, v] of res.headers) { out.push(k, v); }
  return out;
}
export function body_reader(res) { return res.body ? res.body.getReader() : null; }
export function reader_read(r) { return r.read(); }
export function reader_cancel(r) { try { r.cancel().catch(() => {}); } catch (e) {} }
export function step_done(s) { return !s || s.done === true; }
export function step_value(s) { return s.value instanceof Uint8Array ? s.value : new Uint8Array(0); }
export function is_timeout(e) { return !!e && (e.name === 'TimeoutError' || e.name === 'AbortError'); }
export function random_fill(n) { const b = new Uint8Array(n); crypto.getRandomValues(b); return b; }
")]
extern "C" {
    /// 1 fetch, 2 robots, 8 an `x-sn-` header, 9 another method on a route,
    /// else 0 (read in JS, no Rust allocation).
    fn fetch_route(req: &web_sys::Request) -> u32;
    fn out_fetch(url: &str, flat: &js_sys::Array, ms: f64, binding: &JsValue) -> js_sys::Promise;
    fn head_of(res: &JsValue) -> js_sys::Array;
    fn body_reader(res: &JsValue) -> JsValue;
    fn reader_read(r: &JsValue) -> js_sys::Promise;
    fn reader_cancel(r: &JsValue);
    fn step_done(s: &JsValue) -> bool;
    fn step_value(s: &JsValue) -> Uint8Array;
    fn is_timeout(e: &JsValue) -> bool;
    fn random_fill(n: u32) -> Uint8Array;
}

/// A bodiless answer built in JS (no Rust allocation).
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

fn json<T: Serialize>(value: &T) -> web_sys::Response {
    let Ok(body) = serde_json::to_vec(value) else { return status_only(500) };
    let res = Response::from_bytes(body).and_then(|mut r| {
        let h = r.headers_mut();
        h.set("Content-Type", "application/json; charset=utf-8")?;
        h.set("Cache-Control", "no-store")?;
        h.set("X-Content-Type-Options", "nosniff")?;
        Ok(r)
    });
    match res {
        Ok(r) => worker::IntoResponse::into_raw(r).unwrap_or_else(|_| status_only(500)),
        Err(_) => status_only(500),
    }
}

/// Recycles the instance at an idle entry when the pool is poisoned or the
/// linear memory grew past half its ceiling (a large page's extraction),
/// so the JS heap gets its room back (risk F14). The trap fails this one
/// call; the detect Worker retries it once, on a fresh instance.
fn maybe_recycle() {
    let st = slop_ninja_arena::stats();
    let home = HOME.with(|h| u32::from(h.borrow().is_some()));
    if st.open_scopes > home {
        return;
    }
    let grown = st.memory_bytes > model::LINEAR_MEMORY_BYTES / 2;
    if grown || !st.healthy {
        let pages = u32::try_from(st.memory_bytes / slop_ninja_arena::PAGE).unwrap_or(u32::MAX);
        slop_ninja_arena::recycle(PoisonCause { route: routes::id::FETCH, kind: PinKind::Refusal, pages });
    }
}

/// Opens the statics home once and initializes the extraction stack's lazy
/// statics inside it, so no request scope ever holds them.
fn warm_statics() {
    if HOME_TRIED.with(Cell::get) {
        return;
    }
    HOME_TRIED.with(|t| t.set(true));
    let Ok(s) = Scope::open(routes::R_STATICS, routes::STATICS_HOME_BYTES) else { return };
    s.set_permanent();
    s.enter(crate::extract::warm_up);
    HOME.with(|h| *h.borrow_mut() = Some(s));
}

#[wasm_bindgen]
pub fn fetch(req: web_sys::Request, env: Env, _ctx: worker_sys::Context) -> js_sys::Promise {
    silence_panics();
    maybe_recycle();
    let Ok(ticket) = glue::Ticket::admit() else {
        let v: JsValue = status_only(503).into();
        return js_sys::Promise::resolve(&v);
    };
    let _ = slop_ninja_arena::tick(now_ms());
    let class = fetch_route(&req);
    if class == 1 {
        warm_statics();
    }
    let (route, control): (AsyncRoute, usize) = match class {
        1 => (routes::R_FETCH, routes::FETCH_CONTROL_BYTES),
        2 => (routes::R_ROBOTS, routes::ROBOTS_CONTROL_BYTES),
        _ => (routes::R_OTHER, routes::OTHER_CONTROL_BYTES),
    };
    wasm_bindgen_futures::future_to_promise(async move {
        let _ticket = ticket;
        let res = match slop_ninja_arena::enter(route, control, move |_scope| async move {
            match class {
                1 => handle_fetch(Request::from(req), env).await,
                2 => handle_robots(Request::from(req), env).await,
                8 => status_only(400),
                9 => status_only(405),
                _ => status_only(404),
            }
        }) {
            Ok(e) => e.await.0,
            Err(_) => status_only(503),
        };
        Ok(res.into())
    })
}

/// A reservation of fetch memory, given back when the request ends.
struct Held(usize);

impl Held {
    fn take(bytes: usize) -> Option<Held> {
        MEMORY.with(|m| m.take(bytes)).ok().map(|()| Held(bytes))
    }

    /// Gives back all but `keep` bytes.
    fn shrink_to(&mut self, keep: usize) {
        if keep < self.0 {
            MEMORY.with(|m| m.give_back(self.0 - keep));
            self.0 = keep;
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        MEMORY.with(|m| m.give_back(self.0));
    }
}

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string()).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// The deployment's URL policy and signer; `None` when a release build has
/// no usable signing key (it then fetches nothing).
fn config(env: &Env) -> Option<(Policy, Option<Signer>)> {
    let site = var(env, "SITE_ORIGIN");
    let mut policy = Policy::from_origins(site.as_deref(), var(env, "SECRET_ORIGIN").as_deref());
    if var(env, "DEPLOY_ENV").as_deref() == Some(ENV_STAGING)
        && let Some(p) = var(env, "URL_FIXTURE_PREFIX").and_then(|v| FixturePrefix::parse(&v))
    {
        policy = policy.with_fixture(p);
    }
    #[cfg(feature = "dev")]
    if let Some(h) = var(env, "DEV_FIXTURE_HOST") {
        policy = policy.with_dev_host(&h);
    }
    let key = env.secret(WEB_BOT_AUTH_KEY).ok().map(|s| Zeroizing::new(s.to_string()));
    let signer = match key {
        Some(k) => Some(Signer::from_jwk(&k, site.as_deref().unwrap_or("https://slop.ninja")).ok()?),
        None => None,
    };
    #[cfg(not(feature = "dev"))]
    signer.as_ref()?;
    Some((policy, signer))
}

async fn handle_fetch(mut req: Request, env: Env) -> web_sys::Response {
    let body = match cf_io::request_bytes(&mut req, REQUEST_BYTES).await {
        Ok(b) => b,
        Err(e) => return status_only(e.status()),
    };
    let Ok(fr) = serde_json::from_slice::<FetchRequest>(&body) else { return status_only(400) };
    drop(body);
    if fr.slot.len() > SLOT_BYTES || fr.limits.wait_ms == 0 {
        return status_only(400);
    }
    let Some((policy, signer)) = config(&env) else { return status_only(503) };
    let html_bound = usize::try_from(fr.limits.html_bound).unwrap_or(usize::MAX);
    let Some(mut held) = Held::take(routes::fetch_work(html_bound)) else { return status_only(503) };
    let mut net = CfNet::new(&env);
    let ctx = Ctx { policy: &policy, signer: signer.as_ref() };
    let answer = fetch::fetch(&mut net, &ctx, &fr, &mut |bytes, elements| held.shrink_to(bound::page_work(bytes, elements))).await;
    json(&answer)
}

async fn handle_robots(mut req: Request, env: Env) -> web_sys::Response {
    let body = match cf_io::request_bytes(&mut req, REQUEST_BYTES).await {
        Ok(b) => b,
        Err(e) => return status_only(e.status()),
    };
    let Ok(rr) = serde_json::from_slice::<RobotsRequest>(&body) else { return status_only(400) };
    drop(body);
    let Some((policy, signer)) = config(&env) else { return status_only(503) };
    // The origin passes the input checks and names no path.
    let Ok(Checked::Url(c)) = check(&rr.origin, &policy) else { return status_only(400) };
    if c.url.path() != "/" || c.url.query().is_some() || c.url.fragment().is_some() || rr.wait_ms == 0 {
        return status_only(400);
    }
    let Some(_held) = Held::take(routes::ROBOTS_WORK_BYTES) else { return status_only(503) };
    let mut net = CfNet::new(&env);
    let ctx = Ctx { policy: &policy, signer: signer.as_ref() };
    let deadline = now_ms().saturating_add(rr.wait_ms.min(MAX_WAIT_MS));
    let mut screened = vec![c.host];
    let answer = crate::robots::read(&mut net, &ctx, &c.url, &mut screened, deadline).await;
    json(&answer)
}

/// The platform's Fetch, one response at a time.
struct CfNet {
    reader: Option<JsValue>,
    #[cfg(feature = "dev")]
    dev: Option<(String, JsValue)>,
}

impl CfNet {
    fn new(env: &Env) -> CfNet {
        let _ = env;
        CfNet {
            reader: None,
            #[cfg(feature = "dev")]
            dev: var(env, "DEV_FIXTURE_HOST").map(|h| {
                let b = js_sys::Reflect::get(env, &JsValue::from_str("FIXTURE_SITE")).unwrap_or(JsValue::NULL);
                (h.to_ascii_lowercase(), b)
            }),
        }
    }

    /// The service binding a URL goes through (dev builds: the fixture site).
    fn binding(&self, url: &str) -> JsValue {
        let _ = url;
        #[cfg(feature = "dev")]
        if let Some((host, b)) = &self.dev
            && url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned)).as_deref() == Some(host.as_str())
        {
            return b.clone();
        }
        JsValue::NULL
    }
}

impl Net for CfNet {
    async fn get(&mut self, url: &str, headers: &Headers, deadline_ms: u64) -> Result<Head, NetError> {
        self.discard();
        let left = deadline_ms.saturating_sub(now_ms());
        if left == 0 {
            return Err(NetError::Timeout);
        }
        let flat = js_sys::Array::new();
        for (k, v) in headers {
            flat.push(&JsValue::from_str(k));
            flat.push(&JsValue::from_str(v));
        }
        let res = match cf_io::settle(out_fetch(url, &flat, left as f64, &self.binding(url)), Some(deadline_ms)).await {
            Settled::Value(r) => r,
            Settled::Error(e) if is_timeout(&e) => return Err(NetError::Timeout),
            Settled::Error(_) => return Err(NetError::Transport),
            Settled::Timeout => return Err(NetError::Timeout),
        };
        let arr = head_of(&res);
        let status = arr.get(0).as_f64().map_or(0, |s| s as u16);
        let n = arr.length().saturating_sub(1) / 2;
        if n as usize > MAX_HEADERS {
            self.reader = Some(body_reader(&res));
            self.discard();
            return Err(NetError::Transport);
        }
        let mut headers = Vec::with_capacity(n as usize);
        let mut bytes = 0usize;
        for i in 0..n {
            let k = arr.get(1 + 2 * i).as_string().unwrap_or_default().to_ascii_lowercase();
            let v = arr.get(2 + 2 * i).as_string().unwrap_or_default();
            bytes += k.len() + v.len();
            if bytes > MAX_HEADER_BYTES {
                self.reader = Some(body_reader(&res));
                self.discard();
                return Err(NetError::Transport);
            }
            headers.push((k, v));
        }
        let r = body_reader(&res);
        self.reader = (!r.is_null()).then_some(r);
        Ok(Head { status, headers })
    }

    async fn read_body(&mut self, deadline_ms: u64, sink: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), NetError> {
        let Some(reader) = self.reader.take() else { return Ok(()) };
        let window = slop_ninja_arena::PAGE.saturating_sub(slop_ninja_arena::BLOCK_OVERHEAD).max(1);
        let mut scratch: Vec<u8> = Vec::new();
        loop {
            let step = match cf_io::settle(reader_read(&reader), Some(deadline_ms)).await {
                Settled::Value(v) => v,
                Settled::Error(_) => {
                    reader_cancel(&reader);
                    return Err(NetError::Transport);
                }
                Settled::Timeout => {
                    reader_cancel(&reader);
                    return Err(NetError::Timeout);
                }
            };
            if step_done(&step) {
                return Ok(());
            }
            let chunk = step_value(&step);
            let n = chunk.length() as usize;
            if n > 0 && scratch.is_empty() {
                scratch.resize(window, 0);
            }
            let mut from = 0usize;
            while from < n {
                let to = n.min(from + window);
                let Some(dst) = scratch.get_mut(..to - from) else {
                    reader_cancel(&reader);
                    return Err(NetError::Transport);
                };
                chunk.subarray(from as u32, to as u32).copy_to(dst);
                if !sink(dst) {
                    reader_cancel(&reader);
                    return Ok(());
                }
                from = to;
            }
        }
    }

    fn discard(&mut self) {
        if let Some(r) = self.reader.take() {
            reader_cancel(&r);
        }
    }

    fn now_ms(&self) -> u64 {
        now_ms()
    }

    fn random(&mut self, out: &mut [u8]) {
        let b = random_fill(u32::try_from(out.len()).unwrap_or(0));
        if b.length() as usize == out.len() {
            b.copy_to(out);
        }
    }

    fn screen_exempt(&self, host: &str) -> bool {
        #[cfg(feature = "dev")]
        {
            self.dev.as_ref().is_some_and(|(h, _)| h == host)
        }
        #[cfg(not(feature = "dev"))]
        {
            let _ = host;
            false
        }
    }
}
