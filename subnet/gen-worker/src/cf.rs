//! The generation Worker on Cloudflare (wasm32 only).
//!
//! The one export, `fetch`, takes a glue ticket, sets the arena clock and
//! runs its handler in its own bounded scope with the route's control
//! footprint ([`crate::routes`]). Its request class is read in JS, so a
//! request that is not the interface's (the internal host, the exact path
//! and method, the marker header) costs a static 404 and nothing else.
//!
//! - `generate`: the call message is read within
//!   [`iface::CALL_MESSAGE_BYTES`] (a larger declared length is refused
//!   before it is read) and admitted as one block; its parse is admitted
//!   before it runs; a request outside the interface's bounds is refused;
//!   the vendor's key is read from its Secrets Store binding for this call
//!   only (zeroed on drop); [`drive::drive`] admits the call's heap (reader,
//!   reply, a Flex retry's body) before anything is sent and runs the call
//!   over `fetch`, never following a redirect.
//! - a request on the internal host whose marker names another interface
//!   version: 409 `interface_version`, before its body is read.
//! - `health`: whether each key resolves to a non-empty value.
//!
//! The key reaches JS once, as the value of the vendor's key header. It is
//! never logged, never part of an error, never in a reply.

use crate::cf_io::{self, Settled};
use crate::drive::{self, Body, Fault, Head, Transport};
use crate::routes;
use js_sys::{Array, Reflect, Uint8Array};
use slop_ninja_arena::{ArenaAlloc, AsyncRoute, glue};
use slop_ninja_gen_core::iface;
use slop_ninja_gen_core::outcome::GenCall;
use slop_ninja_gen_core::plan::Health;
use slop_ninja_gen_core::vendor::Vendor;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};
use worker::worker_sys::{self, web_sys};
use worker::{Env, Request};
use zeroize::Zeroizing;

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

/// The Secrets Store bindings of the two keys (`secrets_store_secrets`).
const OPENAI_KEY: &str = "OPENAI_API_KEY";
const ANTHROPIC_KEY: &str = "ANTHROPIC_API_KEY";

const NOT_FOUND: &str = r#"{"error":"not_found"}"#;
const UNAVAILABLE: &str = r#"{"error":"unavailable"}"#;
const BAD_REQUEST: &str = r#"{"error":"bad_request"}"#;
const TOO_LARGE: &str = r#"{"error":"too_large"}"#;
const INTERFACE_VERSION: &str = r#"{"error":"interface_version"}"#;

#[wasm_bindgen(inline_js = "
export function gen_route_class(req, version) {
  let u;
  try { u = new URL(req.url); } catch (e) { return 3; }
  const marker = req.headers.get('x-slop-ninja-gen');
  if (u.host !== 'gen.internal' || marker === null) return 3;
  if (marker !== version) return 4;
  if (u.pathname === '/internal/gen/v1/generate' && req.method === 'POST') return 1;
  if (u.pathname === '/internal/gen/v1/health' && req.method === 'GET') return 2;
  return 3;
}
export function gen_post(url, names, values, body, ms) {
  let init;
  try {
    const h = new Headers();
    for (let i = 0; i < names.length; i++) h.set(names[i], values[i]);
    // A redirect is never followed: it would carry the key header.
    init = { method: 'POST', headers: h, body, redirect: 'manual' };
  } catch (e) {
    return Promise.resolve({ fault: 'transport' });
  }
  try { init.signal = AbortSignal.timeout(Math.max(1, Math.floor(ms))); } catch (e) {}
  return fetch(url, init).then((r) => {
    let rate = false;
    for (const k of r.headers.keys()) {
      if (k.startsWith('x-ratelimit-') || k.startsWith('anthropic-ratelimit-')) { rate = true; break; }
    }
    return { status: r.status, retry_after: r.headers.get('retry-after'), rate, reader: r.body ? r.body.getReader() : null };
  }, (e) => ({ fault: (e && e.name === 'TimeoutError') ? 'timeout' : 'transport' }));
}
export function gen_read(reader) {
  return reader.read();
}
export function gen_cancel(reader) {
  try { reader.cancel().catch(() => {}); } catch (e) {}
}
")]
extern "C" {
    /// 1: generate, 2: health, 4: the interface's host with another marker
    /// version, 3: anything else (read in JS, no Rust allocation).
    fn gen_route_class(req: &web_sys::Request, version: &str) -> u32;
    /// `POST url` with the header pairs and the body, under an
    /// `AbortSignal.timeout` (which also ends the body's reads): resolves to
    /// `{status, retry_after, rate, reader}` or `{fault}`; never rejects or
    /// throws (a header value the platform refuses is a transport fault, its
    /// error text dropped). Redirects are not followed (`redirect:
    /// 'manual'`): a 3xx is returned as itself and classified an outage.
    fn gen_post(url: &str, names: &Array, values: &Array, body: &str, ms: f64) -> js_sys::Promise;
    fn gen_read(reader: &JsValue) -> js_sys::Promise;
    fn gen_cancel(reader: &JsValue);
}

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

/// A static JSON response built in JS.
fn js_json(body: &str, status: u16) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    if let Ok(h) = web_sys::Headers::new() {
        let _ = h.set("Cache-Control", "no-store");
        let _ = h.set("Content-Type", "application/json");
        init.set_headers(&h);
    }
    match web_sys::Response::new_with_opt_str_and_init(Some(body), &init) {
        Ok(r) => r,
        Err(_) => web_sys::Response::error(),
    }
}

/// The Worker fetch export.
#[wasm_bindgen]
pub fn fetch(req: web_sys::Request, env: Env, _ctx: worker_sys::Context) -> js_sys::Promise {
    let Ok(ticket) = glue::Ticket::admit() else {
        let v: JsValue = js_json(UNAVAILABLE, 503).into();
        return js_sys::Promise::resolve(&v);
    };
    let _ = slop_ninja_arena::tick(now_ms());
    let class = gen_route_class(&req, iface::MARKER_VALUE);
    let route: AsyncRoute = match class {
        1 => routes::R_GENERATE,
        2 => routes::R_HEALTH,
        _ => routes::R_OTHER,
    };
    wasm_bindgen_futures::future_to_promise(async move {
        let _ticket = ticket;
        let res = match slop_ninja_arena::enter(route, routes::CONTROL_BYTES, move |_scope| async move {
            match class {
                1 => generate(Request::from(req), env).await,
                2 => health(&env).await,
                4 => js_json(INTERFACE_VERSION, iface::VERSION_MISMATCH_STATUS),
                _ => js_json(NOT_FOUND, 404),
            }
        }) {
            Ok(e) => e.await.0,
            Err(_) => js_json(UNAVAILABLE, 503),
        };
        Ok(res.into())
    })
}

/// A key from its Secrets Store binding; `None` when the binding, the
/// secret or its value is missing or empty.
async fn key(env: &Env, binding: &str) -> Option<Zeroizing<String>> {
    let store = env.secret_store(binding).ok()?;
    let v = Zeroizing::new(store.get().await.ok()??);
    (!v.is_empty()).then_some(v)
}

async fn health(env: &Env) -> web_sys::Response {
    let ok = |k: Option<Zeroizing<String>>| k.is_some_and(|k| drive::key_ok(&k));
    let h = Health { interface: iface::MARKER_VALUE.to_owned(), openai: ok(key(env, OPENAI_KEY).await), anthropic: ok(key(env, ANTHROPIC_KEY).await) };
    match serde_json::to_string(&h) {
        Ok(s) => js_json(&s, 200),
        Err(_) => js_json(UNAVAILABLE, 503),
    }
}

async fn generate(mut req: Request, env: Env) -> web_sys::Response {
    let raw = match cf_io::request_bytes(&mut req, iface::CALL_MESSAGE_BYTES).await {
        Ok(r) => r,
        Err(cf_io::BodyError::TooLarge) => return js_json(TOO_LARGE, 413),
        Err(_) => return js_json(UNAVAILABLE, 503),
    };
    if slop_ninja_arena::admit_current_heap(routes::parse_bytes(raw.len())).is_err() {
        return js_json(UNAVAILABLE, 503);
    }
    let Ok(call) = serde_json::from_slice::<GenCall>(&raw) else {
        return js_json(BAD_REQUEST, 400);
    };
    drop(raw);
    if !iface::within(&call.request) {
        return js_json(BAD_REQUEST, 400);
    }
    let binding = match call.request.vendor {
        Vendor::Openai => OPENAI_KEY,
        Vendor::Anthropic => ANTHROPIC_KEY,
        Vendor::WorkersAi => return js_json(BAD_REQUEST, 400),
    };
    // The call's heap (reader, read buffer, reply, a Flex retry's body) is
    // admitted inside drive() before the POST; a refusal there answers a
    // throttle without contacting the vendor.
    let reply = {
        let k = key(&env, binding).await;
        let mut t = Fetch;
        drive::drive(&mut t, &call, k.as_deref().map_or("", String::as_str)).await
    };
    match serde_json::to_string(&reply) {
        Ok(s) => js_json(&s, 200),
        Err(_) => js_json(UNAVAILABLE, 503),
    }
}

// ------------------------------------------------------------ transport

struct Fetch;

struct FetchBody {
    reader: Option<JsValue>,
    chunk: Option<Uint8Array>,
    off: u32,
    deadline_ms: u64,
}

impl Drop for FetchBody {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn get(v: &JsValue, k: &str) -> JsValue {
    Reflect::get(v, &JsValue::from_str(k)).unwrap_or(JsValue::UNDEFINED)
}

impl Body for FetchBody {
    async fn read(&mut self, out: &mut [u8]) -> Result<usize, Fault> {
        loop {
            if let Some(c) = &self.chunk {
                let len = c.length();
                if self.off < len {
                    let n = (len - self.off).min(u32::try_from(out.len()).unwrap_or(u32::MAX));
                    let dst = out.get_mut(..n as usize).ok_or(Fault::Transport)?;
                    c.subarray(self.off, self.off + n).copy_to(dst);
                    self.off += n;
                    return Ok(n as usize);
                }
                self.chunk = None;
            }
            let Some(r) = &self.reader else { return Ok(0) };
            match cf_io::settle(gen_read(r), Some(self.deadline_ms)).await {
                Settled::Value(v) => {
                    if get(&v, "done").as_bool() == Some(true) {
                        self.reader = None;
                        return Ok(0);
                    }
                    let Ok(c) = get(&v, "value").dyn_into::<Uint8Array>() else {
                        self.cancel();
                        return Err(Fault::Transport);
                    };
                    self.chunk = Some(c);
                    self.off = 0;
                }
                Settled::Error(_) => {
                    self.cancel();
                    return Err(Fault::Transport);
                }
                Settled::Timeout => {
                    self.cancel();
                    return Err(Fault::Timeout);
                }
            }
        }
    }

    fn cancel(&mut self) {
        if let Some(r) = self.reader.take() {
            gen_cancel(&r);
        }
        self.chunk = None;
    }
}

impl Transport for Fetch {
    type Body = FetchBody;

    async fn post(&mut self, url: &str, headers: &[(&str, &str)], body: &str, deadline_ms: u64) -> Result<(Head, FetchBody), Fault> {
        let left = deadline_ms.saturating_sub(now_ms());
        if left == 0 {
            return Err(Fault::Timeout);
        }
        let names = Array::new();
        let values = Array::new();
        for (n, v) in headers {
            names.push(&JsValue::from_str(n));
            values.push(&JsValue::from_str(v));
        }
        let answer = match cf_io::settle(gen_post(url, &names, &values, body, left as f64), Some(deadline_ms)).await {
            Settled::Value(v) => v,
            Settled::Error(_) => return Err(Fault::Transport),
            Settled::Timeout => return Err(Fault::Timeout),
        };
        // The header values (the key among them) leave JS's arrays now.
        values.set_length(0);
        match get(&answer, "fault").as_string().as_deref() {
            Some("timeout") => return Err(Fault::Timeout),
            Some(_) => return Err(Fault::Transport),
            None => {}
        }
        let status = get(&answer, "status").as_f64().map_or(0, |s| s as u16);
        let reader = Some(get(&answer, "reader")).filter(|r| !r.is_null() && !r.is_undefined());
        let head = Head { status, retry_after: get(&answer, "retry_after").as_string(), rate_headers: get(&answer, "rate").as_bool() == Some(true) };
        Ok((head, FetchBody { reader, chunk: None, off: 0, deadline_ms }))
    }

    fn now_ms(&self) -> u64 {
        now_ms()
    }

    fn admit(&mut self, bytes: usize) -> bool {
        slop_ninja_arena::admit_current_heap(bytes).is_ok()
    }
}
