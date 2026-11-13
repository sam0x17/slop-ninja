//! The card Worker on Cloudflare (wasm32 Worker builds only): its `fetch`
//! export.
//!
//! Routes (reached only through the website's and the secret-host Worker's
//! service bindings):
//! - `POST /render` [`RenderRequest`] → `200 image/png`, the card; 400 for a
//!   body that is not a render request or a card the template refuses (a
//!   failed check, a character outside the fonts), 413 for a body over
//!   [`REQUEST_BYTES`], 503 when the configuration is unusable or the arena
//!   cannot hold the render (the caller then serves the static site card);
//! - a request carrying any `x-sn-` header (only the public forwarders set
//!   them, and they never reach this Worker): 400; another method on
//!   `/render`: 405; anything else: 404.
//!
//! Configuration (vars only, no binding, no secret): `SITE_ORIGIN` and
//! `SECRET_ORIGIN` (the hosts a card's link prints) and `GAUGE_ENDS`
//! (`icons`, the default, or `letters`; this Worker ships no emoji art, so
//! `emoji` is a configuration error).
//!
//! Every answer carries `Cache-Control: no-store` (the callers cache cards
//! themselves); nothing writes a console line.

use crate::cf_io::{self, now_ms};
use crate::render::{self, RenderError};
use crate::routes;
use crate::silence_panics;
use crate::wire::{DOWNLOAD_NAME, REQUEST_BYTES, RenderRequest};
use js_sys::Uint8Array;
use slop_ninja_arena::{ArenaAlloc, AsyncRoute, PinKind, PoisonCause, ScopeRef, glue, model};
use slop_ninja_detect_page::{GaugeEnds, Origins};
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::wasm_bindgen;
use worker::worker_sys::{self, web_sys};
use worker::{Env, Request};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

#[wasm_bindgen(inline_js = "
export function og_route(req) {
  let u;
  try { u = new URL(req.url); } catch (e) { return 0; }
  for (const k of req.headers.keys()) { if (k.startsWith('x-sn-')) return 8; }
  if (u.search !== '') return 0;
  if (u.pathname === '/render') return req.method === 'POST' ? 1 : 9;
  return 0;
}
")]
extern "C" {
    /// 1 render, 8 an `x-sn-` header, 9 another method on `/render`, else 0
    /// (read in JS, no Rust allocation).
    fn og_route(req: &web_sys::Request) -> u32;
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

/// The PNG answer over a JS copy of the bytes.
fn png_response(bytes: &Uint8Array) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    init.set_status(200);
    if let Ok(h) = web_sys::Headers::new() {
        let _ = h.set("Content-Type", "image/png");
        let _ = h.set("Cache-Control", "no-store");
        let _ = h.set("X-Content-Type-Options", "nosniff");
        let _ = h.set("Content-Disposition", &format!("inline; filename=\"{DOWNLOAD_NAME}\""));
        init.set_headers(&h);
    }
    let body: &js_sys::Object = bytes.as_ref();
    web_sys::Response::new_with_opt_buffer_source_and_init(Some(body), &init)
        .unwrap_or_else(|_| web_sys::Response::error())
}

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string()).map(|v| v.trim().to_owned())
}

/// The deployment's origins and ends; `None` when unusable.
fn config(env: &Env) -> Option<(Origins, GaugeEnds)> {
    let origins = Origins::from_vars(&var(env, "SITE_ORIGIN")?, &var(env, "SECRET_ORIGIN")?).ok()?;
    let ends = GaugeEnds::parse(var(env, "GAUGE_ENDS").as_deref())?;
    (ends != GaugeEnds::Emoji).then_some((origins, ends))
}

/// Recycles the instance at an idle entry when the pool is poisoned or the
/// linear memory grew past half its ceiling, so the JS heap gets its room
/// back. The trap fails this one call; the caller serves the static card.
fn maybe_recycle() {
    let st = slop_ninja_arena::stats();
    if st.open_scopes > 0 {
        return;
    }
    let grown = st.memory_bytes > model::LINEAR_MEMORY_BYTES / 2;
    if grown || !st.healthy {
        let pages = u32::try_from(st.memory_bytes / slop_ninja_arena::PAGE).unwrap_or(u32::MAX);
        slop_ninja_arena::recycle(PoisonCause { route: routes::id::RENDER, kind: PinKind::Refusal, pages });
    }
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
    let class = og_route(&req);
    let (route, control): (AsyncRoute, usize) = match class {
        1 => (routes::R_RENDER, routes::RENDER_CONTROL_BYTES),
        _ => (routes::R_OTHER, routes::OTHER_CONTROL_BYTES),
    };
    wasm_bindgen_futures::future_to_promise(async move {
        let _ticket = ticket;
        let res = match slop_ninja_arena::enter(route, control, move |scope| async move {
            match class {
                1 => handle_render(&scope, Request::from(req), env).await,
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

async fn handle_render(scope: &ScopeRef, mut req: Request, env: Env) -> web_sys::Response {
    let body = match cf_io::request_bytes(&mut req, REQUEST_BYTES).await {
        Ok(b) => b,
        Err(e) => return status_only(e.status()),
    };
    let Ok(rr) = serde_json::from_slice::<RenderRequest>(&body) else { return status_only(400) };
    drop(body);
    let Some((origins, ends)) = config(&env) else { return status_only(503) };
    // The render runs in its own child scope, admitted whole before it
    // starts; the PNG leaves it as a JS copy.
    let Ok(work) = scope.child_physical(routes::R_RENDER_WORK, routes::RENDER_WORK_BYTES) else {
        return status_only(503);
    };
    let out = work.enter(|| render::card_png(&rr.card, &origins, ends).map(|png| Uint8Array::from(png.as_slice())));
    let _ = work.close();
    match out {
        Ok(bytes) => png_response(&bytes),
        Err(RenderError::Card(_)) => status_only(400),
        Err(_) => status_only(500),
    }
}
