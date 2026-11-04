//! The upload Worker on Cloudflare (wasm32 only).
//!
//! Routes:
//! - `/admission/v1/...` (public, the custom domain): the admission
//!   contract's routes. Each request runs the front sequence
//!   ([`crate::gate::decide`]) before anything else happens: route,
//!   signature, access mode, the header-only checks, registration on the
//!   subnet (the isolate's roster at the finalized head, read once a block),
//!   then its body within its bound. Only then is one bounded message
//!   forwarded to the eval Worker's internal interface through the service
//!   binding. A part's body streams from the request through a
//!   `FixedLengthStream` of the part's exact length to R2's `uploadPart` in
//!   the upload bucket and never enters Rust. The unsigned cohort read is
//!   served from the cohort snapshot the eval Worker writes to the upload
//!   bucket (and its one-block edge copy); it never reaches the eval Worker.
//! - `POST /internal/upload/v1/frame` (the eval Worker's service binding
//!   only: the internal host and the interface token): frames one uploaded
//!   object for the cohort authority's framing step.
//! - anything else: 404.
//!
//! Memory: the one export is hand-written (as the coordinator's, design
//! §2.5): it takes a glue ticket, sets the arena clock, and runs its handler
//! in its own bounded scope with the route's declared footprint
//! ([`crate::routes`]); bodies are read with the bounded readers of
//! `cf_io` (shared with the coordinator). A part runs in three scopes in
//! turn: the front sequence and `part-begin` (the public footprint), the
//! stream itself (the small stream footprint, for as long as the miner
//! sends), then `part-record` (the public footprint again). Only JS handles
//! cross from one scope to the next.
//!
//! Nothing here logs request bodies, headers or secrets.

use crate::cf_io::{self, BodyError};
use crate::front::{self, InterfaceFailure};
use crate::gate::{self, Access, BodyFailure, Decision, Headers};
use crate::registry::{self, Head, Layout, LoadState, ReadError, cache, load_state, roster};
use crate::routes;
use js_sys::{Function, JsString, Reflect, Uint8Array};
use slop_ninja_admission_wire::internal::{self as iface, CohortSnapshot, Forward, PartBegin, Reply};
use slop_ninja_admission_wire::{ErrorCode, header};
use slop_ninja_arena::{ArenaAlloc, AsyncRoute, Refusal, glue};
use slop_ninja_artifact_check::admission::{FrameOutcome, Framer, SpecBounds};
use slop_ninja_artifact_check::spec::Version;
use slop_ninja_protocol::{AccountId, ChainBinding, Digest, TESTNET_GENESIS};
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};
use worker::worker_sys::{self, web_sys};
use worker::{Env, Method, Request, Response, Result};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

/// The upload bucket's binding (uploaded objects only).
const ADMISSION_BUCKET: &str = "ADMISSION_TESTNET";
/// The eval Worker's service binding (its internal admission interface).
const EVAL_SERVICE: &str = "EVAL_ADMISSION_TESTNET";
/// The interface token shared with the eval Worker (a Worker secret).
const INTERFACE_TOKEN: &str = "ADMISSION_INTERFACE_TOKEN";
/// R2's documented part maximum (the coordinator's `plan::R2_MAX_PARTS`):
/// the most part records an interface reply lists.
const R2_MAX_PARTS: u32 = 10_000;
/// The fixed buffer framing streams an object through.
const FRAME_CHUNK_BYTES: usize = 1 << 20;
/// The cohort snapshot's edge cache lifetime: one block
/// ([`front::BLOCK_TIME_SECS`]). The eval Worker rewrites the snapshot at
/// every heartbeat, and every URL variant shares one key
/// ([`front::cohort_cache_key`]), so the contract's one unsigned read
/// reaches the upload bucket about once a block per edge location (more
/// often only when concurrent misses in one location race the first
/// `put`: the reads are not coalesced), and never the eval Worker.
const COHORT_CACHE_SECS: u64 = front::BLOCK_TIME_SECS;
const USER_AGENT: &str = concat!("slop-ninja-admission-testnet/", env!("CARGO_PKG_VERSION"));

#[wasm_bindgen(inline_js = "
export function upload_route_class(req) {
  let u;
  try { u = new URL(req.url); } catch (e) { return 3; }
  const p = u.pathname;
  if (p.startsWith('/admission/v1/')) return 1;
  if (p === '/internal/upload/v1/frame' && u.host === 'admission-upload.internal' && req.method === 'POST') return 2;
  return 3;
}
export function upload_get(bucket, key) {
  return bucket.get(key);
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
export function upload_part_stream(env, binding, key, uploadId, n, body, len) {
  let seen = 0, ended = false, over = false;
  const count = new TransformStream({
    transform(chunk, c) {
      seen += chunk.byteLength;
      if (seen > len) { over = true; c.error(new Error('length')); return; }
      c.enqueue(chunk);
    },
    flush() { ended = true; },
  });
  const fixed = new FixedLengthStream(len);
  body.pipeThrough(count).pipeTo(fixed.writable).catch(() => {});
  let upload;
  try { upload = env[binding].resumeMultipartUpload(key, uploadId); }
  catch (e) { return Promise.resolve({ failed: 'store' }); }
  return upload.uploadPart(n, fixed.readable).then(
    (p) => ({ etag: p.etag }),
    () => ({ failed: (over || (ended && seen !== len)) ? 'length' : 'store' }));
}
")]
extern "C" {
    /// The route class of a request, read in JS (no Rust allocation). Only
    /// the eval Worker's framing request (the internal host, `POST`) gets
    /// the framing footprint; any other request to that path is a 404.
    fn upload_route_class(req: &web_sys::Request) -> u32;
    fn upload_get(bucket: &JsValue, key: &str) -> js_sys::Promise;
    /// A promise that settles when the roster load in flight ends.
    fn roster_wait() -> js_sys::Promise;
    /// Settles every [`roster_wait`] promise.
    fn roster_wake();
    /// Streams a part's body through a count and a `FixedLengthStream` of
    /// exactly `len` bytes to R2's `uploadPart`: `{etag}`, or `{failed:
    /// "length"}` when the body was not `len` bytes, `{failed: "store"}`
    /// when R2 (or the stream) failed otherwise.
    fn upload_part_stream(env: &JsValue, binding: &str, key: &JsString, upload_id: &JsString, n: u16, body: &JsValue, len: f64) -> js_sys::Promise;
}

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string())
}

// ------------------------------------------------------------ the export

/// A static contract body built in JS (no Rust allocation), `no-store`,
/// with `Retry-After` when given, and the framing failure header when
/// given.
fn js_json(body: &str, status: u16, retry_after: Option<&str>, frame_failure: Option<&str>) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    if let Ok(h) = web_sys::Headers::new() {
        let _ = h.set("Cache-Control", "no-store");
        let _ = h.set("Content-Type", "application/json");
        if let Some(s) = retry_after {
            let _ = h.set("Retry-After", s);
        }
        if let Some(f) = frame_failure {
            let _ = h.set(iface::FRAME_FAILURE_HEADER, f);
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
/// request may reach another). On the framing route the answer carries the
/// platform failure header, so the eval Worker retries it uncounted.
fn refused(_r: Refusal, class: u32) -> web_sys::Response {
    if class == 2 {
        js_json(front::UNAVAILABLE_BODY, 503, None, Some(iface::FRAME_PLATFORM))
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

/// What the first scope of a public request leaves: its answer, or a part
/// whose body is to stream (JS handles only, so nothing of the first
/// scope's arena outlives it).
enum Step {
    Done(web_sys::Response),
    Stream(PartStream),
}

/// A part ready to stream: everything the stream and `part-record` scopes
/// need, as JS values.
struct PartStream {
    env: Env,
    body: JsValue,
    key: JsString,
    upload_id: JsString,
    n: u16,
    len: f64,
    /// The `part-record` message without its tag (JSON).
    record: JsString,
}

/// The Worker fetch export: a ticket (else a JS-only 503), the arena clock,
/// then the handler in its own scope; a part continues in the stream and
/// record scopes.
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
        2 => (routes::R_FRAME, routes::FRAME_CONTROL_BYTES),
        _ => (routes::R_OTHER, routes::OTHER_CONTROL_BYTES),
    };
    wasm_bindgen_futures::future_to_promise(async move {
        let _ticket = ticket;
        let step = match slop_ninja_arena::enter(route, control, move |_scope| async move {
            let req = Request::from(req);
            match class {
                1 => public(req, env).await,
                2 => Step::Done(to_js(frame(req, env).await)),
                _ => Step::Done(to_js(not_found())),
            }
        }) {
            Ok(e) => e.await.0,
            Err(r) => Step::Done(refused(r, class)),
        };
        let res = match step {
            Step::Done(r) => r,
            Step::Stream(p) => stream_and_record(p).await,
        };
        Ok(res.into())
    })
}

/// The stream scope (the small stream footprint, for as long as the body
/// streams), then the record scope (the public footprint).
async fn stream_and_record(p: PartStream) -> web_sys::Response {
    let PartStream { env, body, key, upload_id, n, len, record } = p;
    let env2 = env.clone();
    let streamed = match slop_ninja_arena::enter(routes::R_STREAM, routes::STREAM_CONTROL_BYTES, move |_scope| async move {
        let promise = upload_part_stream(&env2, ADMISSION_BUCKET, &key, &upload_id, n, &body, len);
        wasm_bindgen_futures::JsFuture::from(promise).await.ok()
    }) {
        Ok(e) => e.await.0,
        Err(_) => None,
    };
    match slop_ninja_arena::enter(routes::R_PUBLIC, routes::PUBLIC_CONTROL_BYTES, move |_scope| async move {
        to_js(part_record(&env, streamed, &record).await)
    }) {
        Ok(e) => e.await.0,
        Err(_) => unavailable(),
    }
}

// --------------------------------------------------------------- replies

fn not_found() -> Result<Response> {
    let mut r = Response::ok(front::NOT_FOUND_BODY)?.with_status(404);
    let h = r.headers_mut();
    h.set("Cache-Control", "no-store")?;
    h.set("Content-Type", "application/json")?;
    Ok(r)
}

fn reply_response(r: &Reply) -> Result<Response> {
    let mut resp = Response::from_json(&r.body)?.with_status(r.status);
    let h = resp.headers_mut();
    h.set("Cache-Control", "no-store")?;
    if let Some(s) = r.retry_after {
        h.set("Retry-After", &s.to_string())?;
    }
    Ok(resp)
}

fn refusal(r: gate::Refusal) -> Result<Response> {
    reply_response(&r.reply())
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
const PART_UNSTORED: gate::Refusal =
    gate::Refusal::retry(ErrorCode::Unavailable, "the part could not be stored; send it again", 2);
const PART_SHORT: gate::Refusal =
    gate::Refusal::new(ErrorCode::PartLengthMismatch, "the part's body is not its declared length");

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

/// The front sequence's I/O on this Worker: the roster lookup and the
/// bounded body read.
struct WorkerIo<'a> {
    req: &'a mut Request,
    rpc_url: Option<String>,
    netuid: u16,
}

impl gate::Io for WorkerIo<'_> {
    async fn registered(&mut self, who: &AccountId) -> std::result::Result<bool, ()> {
        let Some(url) = self.rpc_url.as_deref() else { return Err(()) };
        registered(url, self.netuid, who).await.map_err(|_| ())
    }

    async fn read_body(&mut self, bound: usize) -> std::result::Result<Vec<u8>, BodyFailure> {
        cf_io::request_bytes(self.req, bound).await.map_err(|e| match e {
            BodyError::TooLarge | BodyError::Refused(slop_ninja_arena::RefusalKind::Invalid) => BodyFailure::TooLarge,
            BodyError::Refused(_) | BodyError::Transport | BodyError::Timeout => BodyFailure::Unavailable,
        })
    }
}

async fn public(mut req: Request, env: Env) -> Step {
    let done = |r: Result<Response>| Step::Done(to_js(r));
    let access = access(&env);
    if access == Access::Off {
        return done(not_found());
    }
    let Some(chain) = chain_binding(&env) else {
        return done(not_found());
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
        let mut io = WorkerIo { req: &mut req, rpc_url: rpc_url(&env).ok(), netuid: chain.netuid };
        gate::decide(&mut io, &access, &chain, &r).await
    };
    match decision {
        Decision::Refuse(e) => done(refusal(e)),
        Decision::Cohort => done(cohort_read(&req, &env).await),
        Decision::Forward { op, forward } => done(match interface::<Reply>(&env, op, Some(&forward)).await {
            Ok(r) => reply_response(&r),
            Err(e) => interface_refusal(e),
        }),
        Decision::Part { forward, len } => part_begin(&req, env, forward, len).await,
    }
}

/// The contract's one unsigned read, served from the cohort snapshot the
/// eval Worker writes to the upload bucket ([`iface::COHORT_SNAPSHOT_KEY`]).
/// The client's response is `no-store` (the contract); this Worker keeps
/// the snapshot in the edge cache for one block under one key for every URL
/// variant (no query), and marks it `paused` once it is older than the
/// coordinator's heartbeat freshness ([`CohortSnapshot::serve`]).
async fn cohort_read(req: &Request, env: &Env) -> Result<Response> {
    let Some(key) = front::cohort_cache_key(req.url()?.as_str()) else {
        return refusal(gate::Refusal::new(ErrorCode::BadRequest, "no such route or method"));
    };
    let bound = iface::cohort_snapshot_limit();
    let cache = worker::Cache::default();
    let snapshot: CohortSnapshot = match cache.get(key.as_str(), false).await {
        Ok(Some(mut hit)) => match cf_io::response_json::<CohortSnapshot>(&mut hit, bound).await {
            Ok(v) => v,
            Err(_) => return refusal(COHORT_UNAVAILABLE),
        },
        _ => {
            let Some(raw) = snapshot_bytes(env, bound).await else {
                return refusal(COHORT_UNAVAILABLE);
            };
            let Ok(snap) = serde_json::from_slice::<CohortSnapshot>(&raw) else {
                return refusal(COHORT_UNAVAILABLE);
            };
            if snap.schema != iface::COHORT_SNAPSHOT_SCHEMA {
                return refusal(COHORT_UNAVAILABLE);
            }
            let mut copy = Response::from_bytes(raw)?;
            copy.headers_mut().set("Content-Type", "application/json")?;
            copy.headers_mut()
                .set("Cache-Control", &format!("public, max-age={COHORT_CACHE_SECS}"))?;
            let _ = cache.put(key.as_str(), copy).await;
            snap
        }
    };
    let mut resp = Response::from_json(&snapshot.serve(now_ms()))?;
    resp.headers_mut().set("Cache-Control", "no-store")?;
    Ok(resp)
}

/// The snapshot object's bytes, refused before reading when R2 reports it
/// larger than `bound`.
async fn snapshot_bytes(env: &Env, bound: usize) -> Option<Vec<u8>> {
    let bucket = env.bucket(ADMISSION_BUCKET).ok()?;
    let obj = bucket.get(iface::COHORT_SNAPSHOT_KEY).execute().await.ok()??;
    if obj.size() > bound as u64 {
        return None;
    }
    obj.body()?.bytes().await.ok()
}

/// One message to the eval Worker's interface, its answer read within the
/// contract's response limit at R2's part maximum. A non-200 answer maps by
/// its status ([`InterfaceFailure`]); everything else that fails is
/// `Unavailable`.
async fn interface<T: serde::de::DeserializeOwned>(env: &Env, op: &str, body: Option<&Forward>) -> std::result::Result<T, InterfaceFailure> {
    let un = |_| InterfaceFailure::Unavailable;
    let token = token(env).ok_or(InterfaceFailure::Unavailable)?;
    let body = match body {
        Some(f) => serde_json::to_string(f).map_err(|_| InterfaceFailure::Refused)?,
        None => "{}".to_owned(),
    };
    if body.len() > iface::FORWARD_BYTES {
        return Err(InterfaceFailure::Refused);
    }
    let url = format!("{}{}{op}", iface::EVAL_INTERFACE_ORIGIN, iface::EVAL_PATH_PREFIX);
    let req = cf_io::internal_request(&url, Method::Post, Some(&body), None).map_err(un)?;
    req.headers().set(iface::TOKEN_HEADER, token.as_str()).map_err(un)?;
    req.headers().set("Content-Type", "application/json").map_err(un)?;
    let service = env.service(EVAL_SERVICE).map_err(un)?;
    let mut r = service.fetch_request(req).await.map_err(un)?;
    if r.status_code() != 200 {
        cf_io::discard(&mut r);
        return Err(InterfaceFailure::of(r.status_code()));
    }
    cf_io::response_json::<T>(&mut r, slop_ninja_admission_wire::response_limit(Some(R2_MAX_PARTS)))
        .await
        .map_err(|_| InterfaceFailure::Unavailable)
}

/// `PUT .../parts/{n}`, first scope: the cohort authority checks the part
/// (`part-begin`); the target and the record message leave as JS values
/// for the stream scope.
async fn part_begin(req: &Request, env: Env, f: Forward, declared: u64) -> Step {
    let done = |r: Result<Response>| Step::Done(to_js(r));
    let begin: PartBegin = match interface(&env, iface::op::PART_BEGIN, Some(&f)).await {
        Ok(b) => b,
        Err(e) => return done(interface_refusal(e)),
    };
    let Some(t) = begin.target else {
        return done(match begin.reply {
            Some(reply) => reply_response(&reply),
            None => refusal(EVAL_UNAVAILABLE),
        });
    };
    // The authority checked the declared length against the part; the
    // target must say the same and name an uploaded object.
    if !front::target_matches(&t, f.part, declared) {
        return done(refusal(gate::Refusal::new(ErrorCode::PartLengthMismatch, "the part does not match its session")));
    }
    let Ok(n) = u16::try_from(t.part) else {
        return done(refusal(gate::Refusal::new(ErrorCode::PartOutOfRange, "no such part")));
    };
    let Some(body) = req.inner().body() else {
        return done(refusal(gate::Refusal::new(ErrorCode::PartLengthMismatch, "the part has no body")));
    };
    // The record carries the part's signed request again, so the cohort
    // authority checks the target against the signer's session and part.
    let (key, upload_id, len) = (JsString::from(t.key.as_str()), JsString::from(t.upload_id.as_str()), t.len as f64);
    let record = Forward {
        signed: f.signed,
        session: f.session,
        part: f.part,
        content_length: f.content_length,
        target: Some(t),
        ..Forward::default()
    };
    let Ok(record) = serde_json::to_string(&record) else {
        return done(refusal(EVAL_REFUSED));
    };
    Step::Stream(PartStream {
        env,
        body: body.into(),
        key,
        upload_id,
        n,
        len,
        record: JsString::from(record.as_str()),
    })
}

/// `PUT .../parts/{n}`, record scope: R2's answer to the stream, then
/// `part-record` with the part's tag.
async fn part_record(env: &Env, streamed: Option<JsValue>, record: &JsString) -> Result<Response> {
    let Some(v) = streamed else {
        return refusal(PART_UNSTORED);
    };
    let field = |name: &str| Reflect::get(&v, &JsValue::from_str(name)).ok().and_then(|x| x.as_string());
    let etag = match (field("etag"), field("failed").as_deref()) {
        (Some(etag), _) => etag,
        (None, Some("length")) => return refusal(PART_SHORT),
        _ => return refusal(PART_UNSTORED),
    };
    let text = String::from(record);
    let Ok(mut record) = serde_json::from_str::<Forward>(&text) else {
        return refusal(EVAL_REFUSED);
    };
    record.etag = etag;
    match interface::<Reply>(env, iface::op::PART_RECORD, Some(&record)).await {
        Ok(r) => reply_response(&r),
        Err(e) => interface_refusal(e),
    }
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

// --------------------------------------------------------------- framing

/// `POST /internal/upload/v1/frame`: the eval Worker's framing step only
/// (its service binding addresses the internal host and carries the
/// interface token; every other request answers 404). A failure that is
/// this Worker's platform's (the request body unread, the object read)
/// carries the framing failure header, so the eval Worker retries it
/// without counting it; a request that does not decode is 400.
async fn frame(mut req: Request, env: Env) -> Result<Response> {
    let host = req.url().ok().and_then(|u| u.host_str().map(str::to_owned));
    let tok = token(&env);
    let presented = req.headers().get(iface::TOKEN_HEADER).ok().flatten();
    let method = req.method().to_string();
    if !front::frame_admitted(host.as_deref(), &method, tok.as_deref().map(String::as_str), presented.as_deref()) {
        return not_found();
    }
    let platform = |why: &str, value: &str| -> Result<Response> {
        let mut resp = Response::error(why, 503)?;
        resp.headers_mut().set(iface::FRAME_FAILURE_HEADER, value)?;
        Ok(resp)
    };
    let fr: iface::FrameRequest = match cf_io::request_json(&mut req, iface::FRAME_REQUEST_BYTES).await {
        Ok(f) => f,
        Err(cf_io::JsonError::Body(BodyError::Refused(k))) if k != slop_ninja_arena::RefusalKind::Invalid => {
            return platform("frame request unread", iface::FRAME_PLATFORM);
        }
        Err(cf_io::JsonError::Body(BodyError::Transport | BodyError::Timeout)) => {
            return platform("frame request unread", iface::FRAME_PLATFORM);
        }
        Err(_) => return Response::error("bad frame request", 400),
    };
    // The eval Worker's link probe: the binding and the token work.
    if fr.key == iface::FRAME_PROBE_KEY {
        let mut resp = Response::from_json(&serde_json::json!({"probe": "ok"}))?;
        resp.headers_mut().set("Cache-Control", "no-store")?;
        return Ok(resp);
    }
    let Some(version) = Version::from_id(&fr.spec).filter(|v| SpecBounds::of(*v).is_some()) else {
        return Response::error("unknown spec", 400);
    };
    if !iface::is_object_key(&fr.key) {
        return Response::error("not an uploaded object", 400);
    }
    let bucket = env.bucket(ADMISSION_BUCKET)?;
    // Read at most one byte past the envelope's length: a longer object is
    // already a mismatch, and its framing never streams more.
    let cap = fr.len.saturating_add(1);
    let mut framer = Framer::new();
    match stream_object(&bucket, &fr.key, cap, |chunk| framer.update(chunk)).await {
        Ok(_) => {}
        // A read that failed is the platform's: the eval Worker retries it
        // without counting it.
        Err(_) => return platform("object read", iface::FRAME_OBJECT_READ),
    }
    let outcome: FrameOutcome = framer.finish(version).into();
    let mut resp = Response::from_json(&outcome)?;
    resp.headers_mut().set("Cache-Control", "no-store")?;
    Ok(resp)
}

fn call0(target: &JsValue, name: &str) -> std::result::Result<JsValue, &'static str> {
    let f = Reflect::get(target, &JsValue::from_str(name)).map_err(|_| "js")?;
    let f: Function = f.dyn_into().map_err(|_| "js")?;
    f.call0(target).map_err(|_| "js")
}

/// Streams an R2 object through `each`, one fixed buffer at a time, so no
/// read holds more than [`FRAME_CHUNK_BYTES`], and stops once `cap` bytes
/// have been fed. Returns the bytes read, or `None` when the object is
/// missing (framing then sees no bytes).
async fn stream_object(
    bucket: &worker::Bucket,
    key: &str,
    cap: u64,
    mut each: impl FnMut(&[u8]),
) -> std::result::Result<Option<u64>, &'static str> {
    let b: &JsValue = bucket.as_ref();
    let obj = match cf_io::settle(upload_get(b, key), None).await {
        cf_io::Settled::Value(v) => v,
        _ => return Err("object read"),
    };
    if obj.is_null() || obj.is_undefined() {
        return Ok(None);
    }
    let body = Reflect::get(&obj, &JsValue::from_str("body")).map_err(|_| "object body")?;
    let reader = call0(&body, "getReader")?;
    let mut buf = vec![0u8; FRAME_CHUNK_BYTES];
    let mut total = 0u64;
    while total < cap {
        let promise: js_sys::Promise = call0(&reader, "read")?.dyn_into().map_err(|_| "read")?;
        let step = match cf_io::settle(promise, None).await {
            cf_io::Settled::Value(v) => v,
            _ => return Err("object stream"),
        };
        if Reflect::get(&step, &JsValue::from_str("done")).ok().and_then(|d| d.as_bool()).unwrap_or(true) {
            break;
        }
        let chunk: Uint8Array =
            Reflect::get(&step, &JsValue::from_str("value")).ok().and_then(|v| v.dyn_into().ok()).ok_or("chunk")?;
        let n = u64::from(chunk.length()).min(cap - total) as u32;
        let mut at = 0u32;
        while at < n {
            let take = (n - at).min(FRAME_CHUNK_BYTES as u32);
            let dst = buf.get_mut(..take as usize).ok_or("buffer")?;
            chunk.subarray(at, at + take).copy_to(dst);
            each(dst);
            at += take;
        }
        total += u64::from(n);
    }
    if total >= cap {
        let _ = call0(&reader, "cancel");
    }
    Ok(Some(total))
}
