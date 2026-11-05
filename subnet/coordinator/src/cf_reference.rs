//! Reference staging (`slop_ninja_reference_staging_v1`) on Workers
//! (wasm32 only): the eval Worker's two
//! operator routes for the operator's reference packages
//! ([`crate::eval::reference`] holds the decisions, natively tested).
//!
//! - `POST /eval/reference/v2/urls`: the internal signer mints one
//!   presigned `PUT` per reference file (entry kind `Reference`), through
//!   the same service binding and token as admission's mints. The operator's
//!   client PUTs each file straight to R2; no file byte reaches this Worker.
//! - `POST /eval/reference/v2/commit?spec=<spec id>`: heads every file in
//!   the admission bucket, pins its entity tag and writes the pinned files
//!   manifest to the eval bucket with R2 checking its SHA-256.
//!
//! Both need the operator token (`COORDINATOR_OPERATOR_TOKEN`) and the eval
//! role, and run in their own arena route (`arena_routes::R_REFERENCE`).
//! Nothing here logs a URL, a body or a token.

use super::admission_do;
use super::eval_do::{self, EVAL_BUCKET};
use super::{bearer_ok, cf_io, error, unauthorized};
use crate::access;
use crate::eval::admission::Head;
use crate::eval::reference::{self, RefError};
use js_sys::Uint8Array;
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use worker::{Env, Method, Request, Response, Result};

#[wasm_bindgen(inline_js = "
export function ref_put_verified(bucket, key, bytes, sha256hex) {
  return bucket.put(key, bytes, { sha256: sha256hex }).then((o) => (o ? o.etag : null));
}
")]
extern "C" {
    /// A binding `put` R2 verifies against `sha256hex` (refused on a
    /// mismatch); resolves to the stored entity tag.
    fn ref_put_verified(bucket: &JsValue, key: &str, bytes: &Uint8Array, sha256hex: &str) -> js_sys::Promise;
}

fn answer(e: &RefError) -> Result<Response> {
    let mut r = Response::from_json(&e.body())?.with_status(e.status());
    r.headers_mut().set("Cache-Control", "no-store")?;
    Ok(r)
}

fn ok<T: serde::Serialize>(v: &T) -> Result<Response> {
    let mut r = Response::from_json(v)?;
    r.headers_mut().set("Cache-Control", "no-store")?;
    Ok(r)
}

/// The routes under `/eval/reference/`.
pub async fn route(mut req: Request, env: Env) -> Result<Response> {
    if !eval_do::enabled(&env) {
        return error("not_found", 404);
    }
    match bearer_ok(&req, &env, access::OPERATOR_TOKEN) {
        Err(()) => return error("token_not_configured", 503),
        Ok(false) => return unauthorized(),
        Ok(true) => {}
    }
    let path = req.path();
    match (req.method(), path.as_str()) {
        (Method::Post, reference::URLS_PATH) => urls(&mut req, &env).await,
        (Method::Post, reference::COMMIT_PATH) => commit(&mut req, &env).await,
        (_, reference::URLS_PATH | reference::COMMIT_PATH) => error("method_not_allowed", 405),
        _ => error("not_found", 404),
    }
}

async fn urls(req: &mut Request, env: &Env) -> Result<Response> {
    let body = match cf_io::request_bytes(req, reference::URLS_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => return error(e.code(), e.status()),
    };
    let entries = match reference::mint_entries(&body) {
        Ok(e) => e,
        Err(e) => return answer(&e),
    };
    match admission_do::mint_urls(env, &entries).await {
        Ok(minted) => match reference::urls_reply(&entries, &minted) {
            Ok(v) => ok(&v),
            Err(e) => answer(&e),
        },
        Err(_) => answer(&RefError::Platform("signer_unavailable")),
    }
}

async fn commit(req: &mut Request, env: &Env) -> Result<Response> {
    let spec = req.url()?.query_pairs().find(|(k, _)| k == "spec").map(|(_, v)| v.into_owned());
    let Some(spec) = spec else {
        return answer(&RefError::Request("spec"));
    };
    let body = match cf_io::request_bytes(req, reference::COMMIT_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => return error(e.code(), e.status()),
    };
    let checked = match reference::commit_request(&body, &spec) {
        Ok(c) => c,
        Err(e) => return answer(&e),
    };
    drop(body);
    let Ok(b) = admission_do::bucket(env) else {
        return answer(&RefError::Platform("admission bucket"));
    };
    let mut heads: Vec<Head> = Vec::with_capacity(checked.keys.len());
    for group in checked.keys.chunks(admission_do::HEADS_IN_FLIGHT) {
        heads.extend(futures_util::future::join_all(group.iter().map(|k| admission_do::head(&b, k))).await);
    }
    let pinned = match reference::pin(checked, &heads) {
        Ok(p) => p,
        Err(e) => return answer(&e),
    };
    let Ok(eval) = env.bucket(EVAL_BUCKET) else {
        return answer(&RefError::Platform("eval bucket"));
    };
    let key = crate::eval::admission::manifest_key(&pinned.package);
    let put = ref_put_verified(eval.as_ref(), &key, &Uint8Array::from(pinned.bytes.as_slice()), &pinned.package);
    match cf_io::settle(put, None).await {
        cf_io::Settled::Value(v) if v.as_string().is_some() => ok(&reference::commit_reply(&pinned)),
        _ => answer(&RefError::Platform("manifest put")),
    }
}
