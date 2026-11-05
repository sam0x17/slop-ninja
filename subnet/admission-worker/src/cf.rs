//! The upload Worker on Cloudflare (wasm32 only).
//!
//! Routes:
//! - `/admission/v2/...` (public, the custom domain): the admission
//!   contract's routes. Each request runs the front sequence
//!   ([`crate::gate::decide`]) before anything else happens: route,
//!   signature, access mode, the header-only checks, registration on the
//!   subnet (the isolate's roster at the finalized head, read once a block),
//!   then its body within its bound (at most one `SubmissionUpload` record).
//!   Only then is one bounded message forwarded to the eval Worker's
//!   internal interface through the service binding, and its reply relayed
//!   byte for byte. No route takes an upload byte: every artifact object
//!   goes straight to R2 under a presigned URL the eval Worker obtains from
//!   the signer. The unsigned cohort read is served from this Worker's
//!   one-block edge copy of the cohort snapshot, refreshed on a miss
//!   through the eval Worker's snapshot operation
//!   (`internal::op::SNAPSHOT`), which never reaches the cohort object.
//! - `POST /internal/upload/v2/header` (the eval Worker's service binding
//!   only: the internal host and the interface token): decodes and checks
//!   one uploaded header for the cohort authority
//!   ([`crate::front::header_check`]).
//! - anything else: 404.
//!
//! This Worker has no R2 binding and no key: its one secret is the
//! interface token, and its one binding the eval Worker's service.
//!
//! Memory: the one export is hand-written (as the coordinator's, design
//! §2.5): it takes a glue ticket, sets the arena clock, and runs its handler
//! in one bounded scope with the route's declared footprint
//! ([`crate::routes`]); bodies are read with the bounded readers of
//! `cf_io` (shared with the coordinator), which admit each body's exact
//! block before it is copied in.
//!
//! Nothing here logs request bodies, headers or secrets.

use crate::cf_io::{self, BodyError};
use crate::front::{self, InterfaceFailure};
use crate::gate::{self, Access, BodyFailure, Decision, Headers};
use crate::registry::{self, Head, Layout, LoadState, ReadError, cache, load_state, roster};
use crate::routes;
use slop_ninja_admission_wire::internal::{self as iface, Forward};
use slop_ninja_admission_wire::{ErrorCode, header};
use slop_ninja_arena::{ArenaAlloc, AsyncRoute, Refusal, glue};
use slop_ninja_protocol::{AccountId, ChainBinding, Digest, TESTNET_GENESIS};
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::wasm_bindgen;
use worker::worker_sys::{self, web_sys};
use worker::{Env, Method, Request, Response, Result};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

/// The eval Worker's service binding (its internal admission interface).
const EVAL_SERVICE: &str = "EVAL_ADMISSION_TESTNET";
/// The interface token shared with the eval Worker (a Worker secret).
const INTERFACE_TOKEN: &str = "ADMISSION_INTERFACE_TOKEN";
/// The cohort snapshot's edge cache lifetime: one block
/// ([`front::BLOCK_TIME_SECS`]). The eval Worker rewrites the snapshot at
/// every heartbeat, and every URL variant shares one key
/// ([`front::cohort_cache_key`]), so the contract's one unsigned read
/// reaches the eval Worker's snapshot operation about once a block per edge
/// location (more often only when concurrent misses in one location race
/// the first `put`: the reads are not coalesced), and never the cohort
/// object.
const COHORT_CACHE_SECS: u64 = front::BLOCK_TIME_SECS;
const USER_AGENT: &str = concat!("slop-ninja-admission-testnet/", env!("CARGO_PKG_VERSION"));

#[wasm_bindgen(inline_js = "
export function upload_route_class(req) {
  let u;
  try { u = new URL(req.url); } catch (e) { return 3; }
  const p = u.pathname;
  if (p.startsWith('/admission/v2/')) return 1;
  if (p === '/internal/upload/v2/header' && u.host === 'admission-upload.internal' && req.method === 'POST') return 2;
  return 3;
}
let roster_waiters = null;
export function roster_wait() {
  if (!roster_waiters) {
    let wake;
    const p = new Promise((res) => { wake = res; });
    roster_waiters = { p, wake };
  }
  return roster_waiters.p;
}
export function roster_wake() {
  const w = roster_waiters;
  roster_waiters = null;
  if (w) w.wake();
}
")]
extern "C" {
    /// The route class of a request, read in JS (no Rust allocation). Only
    /// the eval Worker's header request (the internal host, `POST`) gets
    /// the header footprint; any other request to that path is a 404.
    fn upload_route_class(req: &web_sys::Request) -> u32;
    /// A promise that settles when the roster load in flight ends.
    fn roster_wait() -> js_sys::Promise;
    /// Settles every [`roster_wait`] promise.
    fn roster_wake();
}

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string())
}

// ------------------------------------------------------------ the export

/// A static contract body built in JS (no Rust allocation), `no-store`,
/// with `Retry-After` when given, and the header failure header when
/// given.
fn js_json(body: &str, status: u16, retry_after: Option<&str>, header_failure: Option<&str>) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    if let Ok(h) = web_sys::Headers::new() {
        let _ = h.set("Cache-Control", "no-store");
        let _ = h.set("Content-Type", "application/json");
        if let Some(s) = retry_after {
            let _ = h.set("Retry-After", s);
        }
        if let Some(f) = header_failure {
            let _ = h.set(iface::HEADER_FAILURE_HEADER, f);
        }
        init.set_headers(&h);
    }
    match web_sys::Response::new_with_opt_str_and_init(Some(body), &init) {
        Ok(r) => r,
        Err(_) => web_sys::Response::error(),
    }
}

/// The contract's retryable `unavailable` answer (static).
fn unavailable() -> web_sys::Response {
    js_json(front::UNAVAILABLE_BODY, 503, Some(front::UNAVAILABLE_RETRY_SECS), None)
}

/// An arena refusal (capacity, retry, a poisoned isolate, ...): the
/// contract's retryable `unavailable`, so a client waits and sends the same
/// request again (a poisoned isolate is recycled by the platform; the next
/// request may reach another). On the header route the answer carries the
/// platform failure header, so the eval Worker retries it uncounted.
fn refused(_r: Refusal, class: u32) -> web_sys::Response {
    if class == 2 {
        js_json(front::UNAVAILABLE_BODY, 503, None, Some(front::HEADER_FAILURE_PLATFORM))
    } else {
        unavailable()
    }
}

fn to_js(result: Result<Response>) -> web_sys::Response {
    match result {
        Ok(res) => match worker::IntoResponse::into_raw(res) {
            Ok(raw) => raw,
            Err(_) => unavailable(),
        },
        Err(_) => unavailable(),
    }
}

/// The Worker fetch export: a ticket (else a JS-only 503), the arena clock,
/// then the handler in its own scope.
#[wasm_bindgen]
pub fn fetch(req: web_sys::Request, env: Env, _ctx: worker_sys::Context) -> js_sys::Promise {
    let Ok(ticket) = glue::Ticket::admit() else {
        let v: JsValue = unavailable().into();
        return js_sys::Promise::resolve(&v);
    };
    let _ = slop_ninja_arena::tick(now_ms());
    let class = upload_route_class(&req);
    let (route, control): (AsyncRoute, usize) = match class {
        1 => (routes::R_PUBLIC, routes::PUBLIC_CONTROL_BYTES),
        2 => (routes::R_HEADER, routes::HEADER_CONTROL_BYTES),
        _ => (routes::R_OTHER, routes::OTHER_CONTROL_BYTES),
    };
    wasm_bindgen_futures::future_to_promise(async move {
        let _ticket = ticket;
        let res = match slop_ninja_arena::enter(route, control, move |_scope| async move {
            let req = Request::from(req);
            to_js(match class {
                1 => public(req, env).await,
                2 => header_route(req, env).await,
                _ => not_found(),
            })
        }) {
            Ok(e) => e.await.0,
            Err(r) => refused(r, class),
        };
        Ok(res.into())
    })
}

// --------------------------------------------------------------- replies

fn not_found() -> Result<Response> {
    let mut r = Response::ok(front::NOT_FOUND_BODY)?.with_status(404);
    let h = r.headers_mut();
    h.set("Cache-Control", "no-store")?;
    h.set("Content-Type", "application/json")?;
    Ok(r)
}

/// A JSON answer from its exact bytes, `no-store`.
fn json_bytes(status: u16, body: Vec<u8>, retry_after: Option<u64>) -> Result<Response> {
    let mut resp = Response::from_bytes(body)?.with_status(status);
    let h = resp.headers_mut();
    h.set("Cache-Control", "no-store")?;
    h.set("Content-Type", "application/json")?;
    if let Some(s) = retry_after {
        h.set("Retry-After", &s.to_string())?;
    }
    Ok(resp)
}

fn refusal(r: gate::Refusal) -> Result<Response> {
    let reply = r.reply();
    let body = serde_json::to_vec(&reply.body).map_err(|_| worker::Error::RustError("refusal".into()))?;
    json_bytes(reply.status, body, reply.retry_after)
}

const EVAL_UNAVAILABLE: gate::Refusal =
    gate::Refusal::retry(ErrorCode::Unavailable, "the cohort authority is unavailable", 2);
const EVAL_REFUSED: gate::Refusal =
    gate::Refusal::new(ErrorCode::BadRequest, "the request could not be forwarded as sent");
const COHORT_UNAVAILABLE: gate::Refusal =
    gate::Refusal::retry(ErrorCode::Unavailable, "the cohort snapshot is not available yet", 12);

/// The client's answer when the interface call failed.
fn interface_refusal(f: InterfaceFailure) -> Result<Response> {
    refusal(match f {
        InterfaceFailure::Refused => EVAL_REFUSED,
        InterfaceFailure::Unavailable => EVAL_UNAVAILABLE,
    })
}

// ---------------------------------------------------------------- config

/// The chain the requests must be signed for: the tracked genesis (testnet
/// only, as the coordinator's guard) and netuid.
fn chain_binding(env: &Env) -> Option<ChainBinding> {
    let g = var(env, "CHAIN_GENESIS")?;
    let mut genesis = [0u8; 32];
    hex::decode_to_slice(g.trim().trim_start_matches("0x"), &mut genesis).ok()?;
    if Digest(genesis) != TESTNET_GENESIS || var(env, "NETWORK").as_deref().map(str::trim) != Some("testnet") {
        return None;
    }
    let netuid = var(env, "SUBNET_NETUID")?.trim().parse().ok()?;
    // Mechanism 0, as the eval Worker binds its cohorts (`cf_eval`).
    Some(ChainBinding {
        genesis: TESTNET_GENESIS,
        netuid,
        mechanism: 0,
    })
}

fn access(env: &Env) -> Access {
    Access::parse(var(env, "ADMISSION_ROUTES").as_deref(), var(env, "ADMISSION_HOTKEYS").as_deref())
}

fn token(env: &Env) -> Option<zeroize::Zeroizing<String>> {
    env.secret(INTERFACE_TOKEN).ok().map(|s| zeroize::Zeroizing::new(s.to_string()))
}

// ---------------------------------------------------------- public routes

fn headers_of(req: &Request) -> Headers {
    let h = req.headers();
    let get = |n: &str| h.get(n).ok().flatten();
    Headers {
        hotkey: get(header::HOTKEY),
        valid_until: get(header::VALID_UNTIL),
        content_sha256: get(header::CONTENT_SHA256),
        signature: get(header::SIGNATURE),
        commitment_block: get(header::COMMITMENT_BLOCK),
    }
}

/// The front sequence's I/O on this Worker: the roster lookup, the cohort
/// snapshot's repair list (the edge copy the cohort read serves), and the
/// bounded body read.
struct WorkerIo<'a> {
    req: &'a mut Request,
    env: &'a Env,
    rpc_url: Option<String>,
    netuid: u16,
}

impl gate::Io for WorkerIo<'_> {
    async fn registered(&mut self, who: &AccountId) -> std::result::Result<bool, ()> {
        let Some(url) = self.rpc_url.as_deref() else { return Err(()) };
        registered(url, self.netuid, who).await.map_err(|_| ())
    }

    async fn repair_listed(&mut self, who: &AccountId) -> std::result::Result<bool, ()> {
        let url = self.req.url().map_err(|_| ())?;
        let Some(key) = front::cohort_cache_key(url.as_str()) else { return Err(()) };
        snapshot_with(&key, self.env, |raw| front::repair_listed(raw, who)).await.ok_or(())
    }

    async fn read_body(&mut self, bound: usize) -> std::result::Result<Vec<u8>, BodyFailure> {
        cf_io::request_bytes(self.req, bound).await.map_err(|e| match e {
            BodyError::TooLarge | BodyError::Refused(slop_ninja_arena::RefusalKind::Invalid) => BodyFailure::TooLarge,
            BodyError::Refused(_) | BodyError::Transport | BodyError::Timeout => BodyFailure::Unavailable,
        })
    }
}

async fn public(mut req: Request, env: Env) -> Result<Response> {
    let access = access(&env);
    if access == Access::Off {
        return not_found();
    }
    let Some(chain) = chain_binding(&env) else {
        return not_found();
    };
    let path = req.path();
    let method = req.method().to_string();
    let header = |n: &str| req.headers().get(n).ok().flatten();
    let (cl, ce, ct) = (header("content-length"), header("content-encoding"), header("content-type"));
    let r = gate::Request {
        method: &method,
        path: &path,
        headers: headers_of(&req),
        content_length: cl.as_deref(),
        content_encoding: ce.as_deref(),
        content_type: ct.as_deref(),
    };
    let decision = {
        let mut io = WorkerIo { req: &mut req, env: &env, rpc_url: rpc_url(&env).ok(), netuid: chain.netuid };
        gate::decide(&mut io, &access, &chain, &r).await
    };
    match decision {
        Decision::Refuse(e) => refusal(e),
        Decision::Cohort => cohort_read(&req, &env).await,
        Decision::Forward { op, forward } => forward_op(&env, op, &forward).await,
    }
}

/// The contract's one unsigned read, served from this Worker's edge copy of
/// the cohort snapshot (one block, one key for every URL variant: no
/// query), refreshed on a miss through the eval Worker's snapshot
/// operation. The client's response is `no-store` (the contract), and
/// `paused` once the snapshot is older than the coordinator's heartbeat
/// freshness ([`front::serve_snapshot`]). A snapshot that does not serve is
/// never cached.
async fn cohort_read(req: &Request, env: &Env) -> Result<Response> {
    let Some(key) = front::cohort_cache_key(req.url()?.as_str()) else {
        return refusal(gate::Refusal::new(ErrorCode::BadRequest, "no such route or method"));
    };
    match snapshot_with(&key, env, |raw| front::serve_snapshot(raw, now_ms())).await {
        Some(body) => json_bytes(200, body, None),
        None => refusal(COHORT_UNAVAILABLE),
    }
}

/// `read` of the cohort snapshot's exact bytes (at most
/// `internal::cohort_snapshot_limit`): this edge location's copy under
/// `key` (one block), or on a miss the eval Worker's snapshot operation,
/// whose bytes are then cached (moved, never copied) only when `read`
/// answers. `None` when neither gives a snapshot `read` answers.
async fn snapshot_with<T>(key: &str, env: &Env, read: impl FnOnce(&[u8]) -> Option<T>) -> Option<T> {
    let bound = iface::cohort_snapshot_limit();
    let cache = worker::Cache::default();
    if let Ok(Some(mut hit)) = cache.get(key, false).await {
        return read(&cf_io::response_bytes(&mut hit, bound).await.ok()?);
    }
    let raw = interface(env, iface::op::SNAPSHOT, "{}", bound).await.ok()?;
    let out = read(&raw)?;
    if let Ok(mut copy) = Response::from_bytes(raw) {
        let h = copy.headers_mut();
        if h.set("Content-Type", "application/json").is_ok()
            && h.set("Cache-Control", &format!("public, max-age={COHORT_CACHE_SECS}")).is_ok()
        {
            let _ = cache.put(key, copy).await;
        }
    }
    Some(out)
}

/// One forwarded request: the message to the interface operation `op`, its
/// reply relayed to the client byte for byte ([`front::relay`]).
async fn forward_op(env: &Env, op: &str, f: &Forward) -> Result<Response> {
    let Ok(body) = serde_json::to_string(f) else {
        return refusal(EVAL_REFUSED);
    };
    if body.len() > iface::FORWARD_BYTES {
        return refusal(EVAL_REFUSED);
    }
    match interface(env, op, &body, front::reply_limit()).await {
        Ok(raw) => match front::relay(&raw) {
            Some(r) => json_bytes(r.status, r.body, r.retry_after),
            None => refusal(EVAL_UNAVAILABLE),
        },
        Err(e) => interface_refusal(e),
    }
}

/// One `POST` to the eval Worker's interface operation `op` with the JSON
/// `body`, its answer's bytes read within `bound`. A non-200 answer maps by
/// its status ([`InterfaceFailure`]); everything else that fails is
/// `Unavailable`.
async fn interface(env: &Env, op: &str, body: &str, bound: usize) -> std::result::Result<Vec<u8>, InterfaceFailure> {
    let un = |_| InterfaceFailure::Unavailable;
    let token = token(env).ok_or(InterfaceFailure::Unavailable)?;
    let url = format!("{}{}{op}", iface::EVAL_INTERFACE_ORIGIN, iface::EVAL_PATH_PREFIX);
    let req = cf_io::internal_request(&url, Method::Post, Some(body), None).map_err(un)?;
    req.headers().set(iface::TOKEN_HEADER, token.as_str()).map_err(un)?;
    req.headers().set("Content-Type", "application/json").map_err(un)?;
    let service = env.service(EVAL_SERVICE).map_err(un)?;
    let mut r = service.fetch_request(req).await.map_err(un)?;
    if r.status_code() != 200 {
        cf_io::discard(&mut r);
        return Err(InterfaceFailure::of(r.status_code()));
    }
    cf_io::response_bytes(&mut r, bound).await.map_err(|_| InterfaceFailure::Unavailable)
}

// ------------------------------------------------------------- header route

/// `POST /internal/upload/v2/header`: the eval Worker's header check only
/// (its service binding addresses the internal host and carries the
/// interface token; every other request answers 404). The body is the
/// header bytes, read within [`iface::HEADER_REQUEST_BYTES`] (a declared
/// length above it is refused before the body is touched, 413); the spec id
/// is [`iface::SPEC_HEADER`]. A body this Worker's platform could not read
/// answers 503 with the header failure header, so the eval Worker retries
/// it without counting it.
async fn header_route(mut req: Request, env: Env) -> Result<Response> {
    let host = req.url().ok().and_then(|u| u.host_str().map(str::to_owned));
    let tok = token(&env);
    let presented = req.headers().get(iface::TOKEN_HEADER).ok().flatten();
    let method = req.method().to_string();
    if !front::header_admitted(host.as_deref(), &method, tok.as_deref().map(String::as_str), presented.as_deref()) {
        return not_found();
    }
    drop((host, tok, presented, method));
    // The eval Worker's link probe (the close's canary step): answered
    // before any body is read, once this Worker's registration read works
    // (one roster load a block, shared with the signed routes), so an
    // outage of the front's chain read defers the close like the header
    // route's own.
    if req.headers().get(iface::HEADER_PROBE_HEADER).ok().flatten().is_some() {
        let chain = chain_binding(&env);
        let read = match (rpc_url(&env).ok(), chain) {
            (Some(url), Some(c)) => registered(&url, c.netuid, &AccountId([0; 32])).await.is_ok(),
            _ => false,
        };
        return if read {
            json_bytes(200, front::PROBE_OK.to_vec(), None)
        } else {
            let mut resp = json_bytes(503, br#"{"probe":"chain_unavailable"}"#.to_vec(), None)?;
            resp.headers_mut().set(iface::HEADER_FAILURE_HEADER, front::HEADER_FAILURE_PLATFORM)?;
            Ok(resp)
        };
    }
    let spec = req.headers().get(iface::SPEC_HEADER).ok().flatten();
    let body = match cf_io::request_bytes(&mut req, iface::HEADER_REQUEST_BYTES).await {
        Ok(b) => b,
        Err(BodyError::TooLarge | BodyError::Refused(slop_ninja_arena::RefusalKind::Invalid)) => {
            return json_bytes(413, br#"{"error":"header_too_large"}"#.to_vec(), None);
        }
        Err(BodyError::Refused(_) | BodyError::Transport | BodyError::Timeout) => {
            let mut resp = json_bytes(503, br#"{"error":"header_unread"}"#.to_vec(), None)?;
            resp.headers_mut().set(iface::HEADER_FAILURE_HEADER, front::HEADER_FAILURE_PLATFORM)?;
            return Ok(resp);
        }
    };
    let answer = front::header_check(spec.as_deref(), &body);
    drop(body);
    json_bytes(answer.status, answer.body, None)
}

// -------------------------------------------------------- registration

fn rpc_url(env: &Env) -> std::result::Result<String, ReadError> {
    let u = var(env, "RPC_URL").ok_or(ReadError("rpc_url"))?;
    let u = u.trim();
    if !u.starts_with("https://") {
        return Err(ReadError("rpc_url"));
    }
    Ok(u.to_owned())
}

/// One JSON-RPC call: its reply read within `bound`, under `timeout_ms`.
async fn rpc(url: &str, body: &str, bound: usize, timeout_ms: u64) -> std::result::Result<Vec<u8>, ReadError> {
    let req = cf_io::internal_request(url, Method::Post, Some(body), Some(timeout_ms)).map_err(|_| ReadError("rpc_request"))?;
    let h = req.headers();
    h.set("Content-Type", "application/json").map_err(|_| ReadError("rpc_request"))?;
    h.set("Accept-Encoding", "identity").map_err(|_| ReadError("rpc_request"))?;
    h.set("User-Agent", USER_AGENT).map_err(|_| ReadError("rpc_request"))?;
    let mut r = worker::Fetch::Request(req).send().await.map_err(|_| ReadError("rpc_transport"))?;
    if r.status_code() != 200 {
        cf_io::discard(&mut r);
        return Err(ReadError("rpc_status"));
    }
    cf_io::response_bytes(&mut r, bound).await.map_err(|_| ReadError("rpc_body"))
}

/// Derives the layout at `head` under `marker` (the upgrade marker's digest
/// the read saw there): the runtime version (an observation: the parse
/// takes it) and the V15 metadata at that block.
async fn build_layout(url: &str, head: &str, netuid: u16, marker: [u8; 32]) -> std::result::Result<Layout, ReadError> {
    let v = rpc(url, &registry::runtime_version_request(2, head), registry::MAX_RESPONSE_BYTES, registry::CALL_TIMEOUT_MS).await?;
    let (spec, tx) = registry::parse_runtime_version(2, &v)?;
    let m = rpc(url, &registry::metadata_request(3, head), registry::MAX_LARGE_RESPONSE_BYTES, registry::METADATA_TIMEOUT_MS).await?;
    let prefixed = registry::parse_metadata(3, &m)?;
    drop(m);
    // The parse's heap, from the decoded length, before the parse allocates.
    slop_ninja_arena::admit_current_heap(crate::routes::metadata_parse_bytes(prefixed.len()))
        .map_err(|_| ReadError("metadata_parse_refused"))?;
    let rt = registry::parse_runtime(&prefixed, spec, tx)?;
    Layout::derive(&rt, netuid, marker)
}

/// One read at `head`: the upgrade marker's digest, and `SubnetworkN` under
/// `layout` (`None` without a layout: only the marker is read).
async fn marker_and_n(url: &str, layout: Option<&Layout>, head: &str) -> std::result::Result<([u8; 32], Option<u16>), ReadError> {
    let marker = registry::marker_key();
    let n = layout.map(Layout::n_key_hex).transpose()?;
    let mut keys = vec![marker.as_str()];
    if let Some(k) = &n {
        keys.push(k.as_str());
    }
    let b = rpc(url, &registry::query_storage_request(4, &keys, head), registry::MAX_RESPONSE_BYTES, registry::CALL_TIMEOUT_MS).await?;
    let values = registry::parse_query_storage(4, &b, &keys, head)?;
    let digest = registry::marker_digest(values.first().and_then(Option::as_deref));
    let n = match n {
        Some(_) => Some(registry::parse_n(values.get(1).and_then(Option::as_deref))?),
        None => None,
    };
    Ok((digest, n))
}

/// The finalized head now: `chain_getFinalizedHead`, then that block's
/// header for its number.
async fn read_head(url: &str, now: u64) -> std::result::Result<Head, ReadError> {
    let b = rpc(url, &registry::finalized_head_request(1), registry::MAX_RESPONSE_BYTES, registry::CALL_TIMEOUT_MS).await?;
    let hash = registry::parse_finalized_head(1, &b)?;
    let b = rpc(url, &registry::header_request(5, &hash), registry::MAX_RESPONSE_BYTES, registry::CALL_TIMEOUT_MS).await?;
    let number = registry::parse_header_number(5, &b)?;
    Ok(Head { hash: registry::hash_bytes(&hash)?, number, read_ms: now })
}

/// Loads the roster at the finalized head (module docs of
/// [`crate::registry`]): the head; when it is the roster's head already,
/// that roster serves another block; else the marker and `SubnetworkN` (a
/// marker other than the cached layout's derives the layout again at that
/// head and reads once more), then every uid's hotkey in calls of
/// [`registry::keys_per_call`] keys. No version is compared.
async fn load_roster(url: &str, netuid: u16) -> std::result::Result<(), ReadError> {
    let head = read_head(url, now_ms()).await?;
    if roster::head().is_some_and(|h| h.hash == head.hash) {
        roster::renew(head.read_ms);
        return Ok(());
    }
    roster::begin();
    let at = head.hash_hex();
    let cached = cache::get();
    let (marker, n) = marker_and_n(url, cached.as_ref(), &at).await?;
    let (layout, n) = match (cached, n) {
        (Some(l), Some(n)) if l.marker == marker => (l, n),
        _ => {
            let fresh = build_layout(url, &at, netuid, marker).await?;
            cache::set(Some(fresh));
            let (again, n) = marker_and_n(url, Some(&fresh), &at).await?;
            // The same block, so the same marker; a reply that disagrees is
            // not trusted either way.
            if again != marker {
                cache::set(None);
                return Err(ReadError("rpc_inconsistent"));
            }
            (fresh, n.ok_or(ReadError("n_unread"))?)
        }
    };
    let per = registry::keys_per_call(layout.key_len()?).max(1);
    let n = usize::from(n);
    let mut uid = 0usize;
    while uid < n {
        let end = (uid + per).min(n);
        let hexes = (uid..end).map(|u| layout.key_hex(u as u16)).collect::<std::result::Result<Vec<_>, _>>()?;
        let keys: Vec<&str> = hexes.iter().map(String::as_str).collect();
        let b = rpc(url, &registry::query_storage_request(6, &keys, &at), registry::MAX_RESPONSE_BYTES, registry::CALL_TIMEOUT_MS).await?;
        let values = registry::parse_query_storage(6, &b, &keys, &at)?;
        for (i, v) in values.iter().enumerate() {
            if !roster::put(uid + i, registry::parse_account(v.as_deref())?) {
                return Err(ReadError("roster_capacity"));
            }
        }
        uid = end;
    }
    if !roster::finish(head, n) {
        return Err(ReadError("roster_capacity"));
    }
    Ok(())
}

/// Clears the load flag and wakes the waiting requests however the load
/// ends (its future dropped included).
struct LoadGuard;

impl LoadGuard {
    fn begin() -> Self {
        let st = load_state::get().unwrap_or_default();
        load_state::set(Some(LoadState { loading: true, ..st }));
        LoadGuard
    }
}

impl Drop for LoadGuard {
    fn drop(&mut self) {
        let st = load_state::get().unwrap_or_default();
        load_state::set(Some(LoadState { loading: false, ..st }));
        roster_wake();
    }
}

/// Whether `who` holds a UID on the subnet at the finalized head: a lookup
/// in the isolate's roster, loaded once a block by one request at a time.
/// A request that finds a load in flight waits for it; after a failed load
/// the isolate answers `unavailable` for one block without reading.
async fn registered(url: &str, netuid: u16, who: &AccountId) -> std::result::Result<bool, ReadError> {
    if let Some(present) = roster::lookup(now_ms(), &who.0) {
        return Ok(present);
    }
    let st = load_state::get().unwrap_or_default();
    if st.backing_off(now_ms()) {
        return Err(ReadError("roster_backoff"));
    }
    if st.loading {
        let _ = wasm_bindgen_futures::JsFuture::from(roster_wait()).await;
        return roster::lookup(now_ms(), &who.0).ok_or(ReadError("roster_unavailable"));
    }
    let outcome = {
        let _guard = LoadGuard::begin();
        load_roster(url, netuid).await
    };
    let st = load_state::get().unwrap_or_default();
    load_state::set(Some(LoadState { failed_ms: outcome.as_ref().err().map(|_| now_ms()), ..st }));
    outcome?;
    roster::lookup(now_ms(), &who.0).ok_or(ReadError("roster_unavailable"))
}

