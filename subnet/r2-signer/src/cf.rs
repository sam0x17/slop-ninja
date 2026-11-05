//! The signer Worker on Cloudflare (wasm32 only).
//!
//! Routes:
//! - `POST /internal/r2-signer/v1/mint` on the internal host (the eval
//!   Worker's service binding only), with the interface token: one mint
//!   request in, one reply out ([`crate::mint`]).
//! - `POST /internal/r2-signer/v1/copy` on the internal host (the
//!   publisher's service binding only), with the copy token: one copy
//!   request in, one presigned `CopyObject` out ([`crate::mint::copy`]),
//!   signed with the copy credential; 503 while that credential is not
//!   bound.
//! - anything else: 404, with no body.
//!
//! The credential is read from Secrets Store on each request (workers-rs has
//! no typed Secrets Store API, so a small JS shim calls the binding's
//! `get()`), copied into the request's arena scope, used for the HMACs and
//! zeroed when the scope ends. Nothing here logs a request, a header, a URL
//! or a secret.

use crate::cf_io::{self, Settled};
use crate::mint::{self, COPY_REPLY_BYTES, COPY_REQUEST_BYTES, Config, CopyConfig, REPLY_BYTES, REQUEST_BYTES};
use crate::{COPY_MEDIA_TYPE, MEDIA_TYPE, TOKEN_HEADER, routes, token_matches};
use slop_ninja_arena::{ArenaAlloc, AsyncRoute, glue};
use slop_ninja_r2_presign::presign::Credentials;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::wasm_bindgen;
use worker::worker_sys::{self, web_sys};
use worker::{Env, Request, Response, Result};
use zeroize::Zeroizing;

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

/// Secrets Store binding of the access key id.
const KEY_ID: &str = "R2_PRESIGN_KEY_ID";
/// Secrets Store binding of the secret access key.
const SECRET: &str = "R2_PRESIGN_SECRET";
/// The account's S3 host (a Worker secret: it carries the account id).
const HOST: &str = "R2_S3_HOST";
/// The admission bucket's name (a var).
const BUCKET: &str = "ADMISSION_BUCKET_NAME";
/// The interface token shared with the eval Worker (a Worker secret).
const TOKEN: &str = "SIGNER_INTERFACE_TOKEN";
/// Secrets Store binding of the copy credential's access key id.
const COPY_KEY_ID: &str = "R2_COPY_KEY_ID";
/// Secrets Store binding of the copy credential's secret access key.
const COPY_SECRET: &str = "R2_COPY_SECRET";
/// The public bucket's name (a var).
const PUBLIC_BUCKET: &str = "PUBLIC_BUCKET_NAME";
/// The copy token shared with the publisher (a Worker secret), distinct
/// from the eval Worker's, so neither caller reaches the other's route.
const COPY_TOKEN: &str = "COPY_INTERFACE_TOKEN";
/// The header naming a platform-side failure (the eval side treats it as
/// `signer_unavailable`, never as a refusal of the miner).
const FAILURE_HEADER: &str = "x-sn-signer-failure";
/// How long one Secrets Store read may take before the request answers
/// 503 (the eval side retries; the cohort's close waits on a later success).
const SECRET_READ_MS: u64 = 10_000;

#[wasm_bindgen(inline_js = "
export function signer_route_class(req) {
  let u;
  try { u = new URL(req.url); } catch (e) { return 2; }
  if (u.host === 'r2-signer.internal' && u.pathname === '/internal/r2-signer/v1/mint' && req.method === 'POST') return 1;
  if (u.host === 'r2-signer.internal' && u.pathname === '/internal/r2-signer/v1/copy' && req.method === 'POST') return 3;
  return 2;
}
export function secret_store_get(env, name) {
  const b = env[name];
  if (!b || typeof b.get !== 'function') return Promise.resolve(null);
  return Promise.resolve(b.get()).then((v) => (typeof v === 'string' ? v : null), () => null);
}
")]
extern "C" {
    /// 1 for the mint route on the internal host, 3 for the copy route,
    /// else 2 (read in JS, no Rust allocation).
    fn signer_route_class(req: &web_sys::Request) -> u32;
    /// The Secrets Store binding `name`'s value, or `null`.
    fn secret_store_get(env: &JsValue, name: &str) -> js_sys::Promise;
}

/// A bodiless answer built in JS (no Rust allocation).
fn status_only(status: u16, failure: Option<&str>) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    if let Ok(h) = web_sys::Headers::new() {
        let _ = h.set("Cache-Control", "no-store");
        if let Some(f) = failure {
            let _ = h.set(FAILURE_HEADER, f);
        }
        init.set_headers(&h);
    }
    web_sys::Response::new_with_opt_str_and_init(None, &init).unwrap_or_else(|_| web_sys::Response::error())
}

fn to_js(result: Result<Response>) -> web_sys::Response {
    match result {
        Ok(res) => match worker::IntoResponse::into_raw(res) {
            Ok(raw) => raw,
            Err(_) => status_only(503, Some("internal")),
        },
        Err(_) => status_only(503, Some("internal")),
    }
}

#[wasm_bindgen]
pub fn fetch(req: web_sys::Request, env: Env, _ctx: worker_sys::Context) -> js_sys::Promise {
    let Ok(ticket) = glue::Ticket::admit() else {
        let v: JsValue = status_only(503, Some("capacity")).into();
        return js_sys::Promise::resolve(&v);
    };
    let _ = slop_ninja_arena::tick(cf_io::now_ms());
    let class = signer_route_class(&req);
    let (route, control): (AsyncRoute, usize) = match class {
        1 => (routes::R_MINT, routes::MINT_CONTROL_BYTES),
        3 => (routes::R_COPY, routes::COPY_CONTROL_BYTES),
        _ => (routes::R_OTHER, routes::OTHER_CONTROL_BYTES),
    };
    wasm_bindgen_futures::future_to_promise(async move {
        let _ticket = ticket;
        let res = match slop_ninja_arena::enter(route, control, move |_scope| async move {
            match class {
                1 => to_js(handle(Request::from(req), env).await),
                3 => to_js(handle_copy(Request::from(req), env).await),
                _ => status_only(404, None),
            }
        }) {
            Ok(e) => e.await.0,
            Err(_) => status_only(503, Some("capacity")),
        };
        Ok(res.into())
    })
}

async fn store_secret(env: &Env, name: &str) -> Option<Zeroizing<String>> {
    let deadline = cf_io::now_ms() + SECRET_READ_MS;
    match cf_io::settle(secret_store_get(env, name), Some(deadline)).await {
        Settled::Value(v) => v.as_string().map(Zeroizing::new),
        Settled::Error(_) | Settled::Timeout => None,
    }
}

fn plain(env: &Env, name: &str, secret: bool) -> Option<Zeroizing<String>> {
    let v = if secret { env.secret(name).ok()?.to_string() } else { env.var(name).ok()?.to_string() };
    Some(Zeroizing::new(v.trim().to_owned()))
}

fn answer(status: u16, failure: Option<&str>) -> Result<Response> {
    let mut r = Response::empty()?.with_status(status);
    r.headers_mut().set("Cache-Control", "no-store")?;
    if let Some(f) = failure {
        r.headers_mut().set(FAILURE_HEADER, f)?;
    }
    Ok(r)
}

async fn handle(mut req: Request, env: Env) -> Result<Response> {
    let configured = plain(&env, TOKEN, true);
    let presented = req.headers().get(TOKEN_HEADER)?.map(Zeroizing::new);
    if !token_matches(configured.as_deref().map(String::as_str), presented.as_deref().map(String::as_str)) {
        return answer(404, None);
    }
    let body = match cf_io::request_bytes(&mut req, REQUEST_BYTES).await {
        Ok(b) => b,
        Err(e) => return answer(e.status(), (e.status() >= 500).then_some("body")),
    };
    let (Some(key_id), Some(secret), Some(host), Some(bucket)) = (
        store_secret(&env, KEY_ID).await,
        store_secret(&env, SECRET).await,
        plain(&env, HOST, true),
        plain(&env, BUCKET, false),
    ) else {
        return answer(503, Some("config"));
    };
    let cfg = Config {
        host: host.as_str(),
        bucket: bucket.as_str(),
        cred: Credentials { access_key_id: key_id.trim(), secret_access_key: secret.trim().as_bytes() },
    };
    let mut out = Zeroizing::new(vec![0u8; REPLY_BYTES]);
    let now = cf_io::now_ms() / 1000;
    match mint::mint(&body, &cfg, now, out.as_mut_slice()) {
        Ok(n) => {
            let bytes = out.get(..n).map(<[u8]>::to_vec).unwrap_or_default();
            let mut r = Response::from_bytes(bytes)?;
            r.headers_mut().set("Content-Type", MEDIA_TYPE)?;
            r.headers_mut().set("Cache-Control", "no-store")?;
            Ok(r)
        }
        Err(mint::Malformed) => answer(400, None),
    }
}

async fn handle_copy(mut req: Request, env: Env) -> Result<Response> {
    let configured = plain(&env, COPY_TOKEN, true);
    let presented = req.headers().get(TOKEN_HEADER)?.map(Zeroizing::new);
    if !token_matches(configured.as_deref().map(String::as_str), presented.as_deref().map(String::as_str)) {
        return answer(404, None);
    }
    let body = match cf_io::request_bytes(&mut req, COPY_REQUEST_BYTES).await {
        Ok(b) => b,
        Err(e) => return answer(e.status(), (e.status() >= 500).then_some("body")),
    };
    let (Some(key_id), Some(secret), Some(host), Some(source), Some(destination)) = (
        store_secret(&env, COPY_KEY_ID).await,
        store_secret(&env, COPY_SECRET).await,
        plain(&env, HOST, true),
        plain(&env, BUCKET, false),
        plain(&env, PUBLIC_BUCKET, false),
    ) else {
        return answer(503, Some("copy_config"));
    };
    let cfg = CopyConfig {
        host: host.as_str(),
        source_bucket: source.as_str(),
        destination_bucket: destination.as_str(),
        cred: Credentials { access_key_id: key_id.trim(), secret_access_key: secret.trim().as_bytes() },
    };
    let mut out = Zeroizing::new(vec![0u8; COPY_REPLY_BYTES]);
    let now = cf_io::now_ms() / 1000;
    match mint::copy(&body, &cfg, now, out.as_mut_slice()) {
        Ok(n) => {
            let bytes = out.get(..n).map(<[u8]>::to_vec).unwrap_or_default();
            let mut r = Response::from_bytes(bytes)?;
            r.headers_mut().set("Content-Type", COPY_MEDIA_TYPE)?;
            r.headers_mut().set("Cache-Control", "no-store")?;
            Ok(r)
        }
        Err(mint::Malformed) => answer(400, None),
    }
}
