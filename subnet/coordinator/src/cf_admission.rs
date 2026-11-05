//! Workers side of coordinator admission v2 (wasm32 only): the eval Worker's
//! internal admission interface (reachable only through the upload Worker's
//! service binding), the `EvalCohort` object's admission, template and
//! commitment-reader routes, its admission steps (the canary, the close
//! sweep, manifest writes and audits, verdict runs, the alarm sweep, deletes,
//! the reconciliation, the calendar), and the admission events Queue's
//! consumer.
//!
//! The eval Worker serves no public admission route: every `/admission/`
//! request to it answers 404. The unsigned cohort read never reaches it: at
//! every heartbeat the cohort object writes the cohort response to the
//! eval bucket (`internal::COHORT_SNAPSHOT_KEY`; no presigning credential
//! reaches it), which the upload Worker serves. The public `/admission/v2/` routes are the key-free upload
//! Worker's (`subnet/admission-worker`): it checks every untrusted request
//! (signature, body digest, registration, the record's size) and forwards
//! only checked, bounded messages here, to `/internal/admission/v2/<op>`
//! with the shared interface token. This Worker forwards them to the cohort
//! object, which authenticates against the cursor and decides in a
//! transaction, then does the operation's I/O and records its result:
//!
//! - **No upload byte reaches any of our Workers.** Miners PUT every object
//!   straight to R2 under presigned URLs; the internal signer Worker
//!   (`R2_SIGNER_TESTNET`, its token `SIGNER_INTERFACE_TOKEN`) mints them
//!   for this object only, one request per `urls` call.
//! - **Nothing hashes a model byte.** Objects are recorded from R2's
//!   `head()`: size, stored SHA-256, entity tag and attributes. The header
//!   bytes (at most 1 MiB) are piped from an entity-tag-pinned R2 read
//!   straight into the upload Worker's header route as a JS stream, so they
//!   never enter this isolate's memory; its bounded outcome comes back and
//!   the content digest is recomputed from the returned table.
//! - The R2 object-create events (a wakeup only) arrive on the admission
//!   events Queue; the consumer asks the cohort object to `observe` the key,
//!   which heads it itself.

use super::eval_do::{self, EVAL_BUCKET, EvalCohort, cohort_fetch, lane_stub};
use super::{TxError, error, json, now_ms, transaction};
use crate::eval::admission::{
    self, AdmissionEnv, Applied, CanaryFailure, Head, HeaderJob, HeaderSeen, LaneOutcome, Minted, Observed,
    Probe, Recorder, Reply, Settled, Step, Stored,
};
use crate::eval::{calendar, commitments, engine};
use crate::sql::Sql;
use js_sys::{Reflect, Uint8Array};
use serde::Deserialize;
use serde_json::{Value, json as j};
use sha2::{Digest as _, Sha256};
use slop_ninja_admission_wire::internal::{self as iface, Forward};
use slop_ninja_protocol::ChainBinding;
use slop_ninja_r2_presign::{layout, policy, wire as mint};
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};
use worker::{Env, Headers, Method, Request, RequestInit, Response, Result};

/// The admission bucket's binding: uploaded objects (private) and the
/// cohort snapshot.
pub const ADMISSION_BUCKET: &str = "ADMISSION_TESTNET";
/// The upload Worker's service binding (its internal header route).
pub const UPLOAD_SERVICE: &str = "ADMISSION_UPLOAD_TESTNET";
/// The interface token the two admission Workers share.
pub const INTERFACE_TOKEN: &str = "ADMISSION_INTERFACE_TOKEN";

/// The bound of a cohort object's admission reply the Worker forwards: the
/// contract's response limit (a session with one URL per object and its
/// object list), and an error body beside a `header_missing` session.
const REPLY_BYTES: usize = 2 * slop_ninja_admission_wire::RESPONSE_BASE_BYTES;
/// A commitment-reader round's request body: `commitments::plan` puts at
/// most one large read in a round, so its replies are one large reply and
/// the small ones beside it, which the reader's share bounds in number
/// (each small reply within the RPC client's default response bound).
pub const READER_BODY_BYTES: usize = 2 * crate::rpc::MAX_LARGE_RESPONSE_BYTES;
/// R2 heads one step keeps in flight: Workers' six simultaneous outgoing
/// connections per invocation.
const HEADS_IN_FLIGHT: usize = 6;
/// The longest chain of one session operation's steps: a header settle,
/// the files' mint (or a heads batch and its answer).
const SESSION_STEPS: usize = 4;

#[wasm_bindgen(inline_js = "
export function adm_fixed(body, len) {
  const f = new FixedLengthStream(len);
  body.pipeTo(f.writable).catch(() => {});
  return f.readable;
}
export function adm_head(bucket, key) {
  return bucket.head(key).then((o) => {
    if (o === null) return null;
    let sha = null;
    const c = o.checksums;
    if (c && c.sha256) { const b = new Uint8Array(c.sha256); if (b.length === 32) sha = b; }
    const h = o.httpMetadata || {};
    const http = Object.keys(h).some((k) => h[k] !== undefined && h[k] !== null);
    const cm = o.customMetadata || {};
    return {
      size: o.size, etag: o.etag, sha256: sha, ssec: !!o.ssecKeyMd5,
      storageClass: (o.storageClass === undefined || o.storageClass === null) ? null : String(o.storageClass),
      http: http, custom: Object.keys(cm).length > 0,
    };
  });
}
export function adm_get_pinned(bucket, key, etag) {
  return bucket.get(key, { onlyIf: { etagMatches: etag } });
}
export function adm_random32() {
  const b = new Uint8Array(32);
  crypto.getRandomValues(b);
  return b;
}
")]
extern "C" {
    fn adm_fixed(body: &JsValue, len: f64) -> JsValue;
    fn adm_head(bucket: &JsValue, key: &str) -> js_sys::Promise;
    fn adm_get_pinned(bucket: &JsValue, key: &str, etag: &str) -> js_sys::Promise;
    fn adm_random32() -> Uint8Array;
}

fn chain(env: &Env) -> Option<ChainBinding> {
    eval_do::eval_config(env, engine::INSTANCE_MAIN).ok().map(|c| c.chain)
}

fn secret(env: &Env, name: &str) -> Option<zeroize::Zeroizing<String>> {
    env.secret(name).ok().map(|s| zeroize::Zeroizing::new(s.to_string()))
}

/// Whether a request is the upload Worker's: addressed to the internal
/// origin (no public route of this Worker carries that host) and carrying
/// the shared interface token.
fn from_upload_worker(req: &Request, env: &Env) -> bool {
    let host_ok = req.url().ok().and_then(|u| u.host_str().map(str::to_owned)).as_deref() == Some(iface::EVAL_INTERFACE_HOST);
    if !host_ok {
        return false;
    }
    let token = secret(env, INTERFACE_TOKEN);
    let presented = req.headers().get(iface::TOKEN_HEADER).ok().flatten();
    iface::token_matches(token.as_deref().map(String::as_str), presented.as_deref())
}

/// The interface request body bound: a forwarded request's fields and at
/// most a session-creation body as hex (the shared
/// `slop_ninja_admission_wire::internal::FORWARD_BYTES`, the bound the
/// upload Worker sends within).
pub fn interface_body_bytes() -> usize {
    iface::FORWARD_BYTES
}

/// `/internal/admission/v2/<op>` (the upload Worker's service binding only):
/// the forwarded message goes to the cohort object's `/admission/<op>`, and
/// its JSON answer (a `Reply`) comes back as is. Anything not addressed and
/// authenticated as the upload Worker's answers 404, as does every request
/// when the eval role is off.
pub async fn interface_route(mut req: Request, env: Env) -> Result<Response> {
    if !eval_do::enabled(&env) || !from_upload_worker(&req, &env) || req.method() != Method::Post {
        return error("not_found", 404);
    }
    let path = req.path();
    if path.strip_prefix(iface::EVAL_PATH_PREFIX) == Some(iface::op::SNAPSHOT) {
        return snapshot_read(&env).await;
    }
    let Some(op) = path.strip_prefix(iface::EVAL_PATH_PREFIX).filter(|op| iface::op::ALL.contains(op)) else {
        return error("not_found", 404);
    };
    let op = op.to_owned();
    // Re-encoded from its parse, so the object receives exactly the
    // interface's shape.
    let f: Forward = match super::cf_io::request_json(&mut req, interface_body_bytes()).await {
        Ok(f) => f,
        Err(e) => return error(e.code(), e.status()),
    };
    let body = serde_json::to_string(&f).map_err(|_| worker::Error::RustError("forward".into()))?;
    let mut r = cohort_fetch(&env, engine::INSTANCE_MAIN, Method::Post, &format!("/admission/{op}"), Some(body)).await?;
    relay(&mut r).await
}

/// The interface operation [`iface::op::SNAPSHOT`], which refreshes the
/// upload Worker's edge copy of the cohort read: the cohort snapshot's exact
/// bytes (at most `internal::cohort_snapshot_limit`), or 503 while it is not
/// written. It is answered from the snapshot object and never reaches the
/// cohort object, so no unsigned request reaches the cohort authority.
async fn snapshot_read(env: &Env) -> Result<Response> {
    let Ok(b) = env.bucket(EVAL_BUCKET) else {
        return error("not_configured", 503);
    };
    match super::cf_io::r2_bytes(&b, iface::COHORT_SNAPSHOT_KEY, iface::cohort_snapshot_limit(), None).await {
        Ok(Some(bytes)) => {
            let mut resp = Response::from_bytes(bytes)?;
            resp.headers_mut().set("Content-Type", "application/json")?;
            resp.headers_mut().set("Cache-Control", "no-store")?;
            Ok(resp)
        }
        Ok(None) => error("snapshot_unwritten", 503),
        Err(_) => error("snapshot_unavailable", 503),
    }
}

/// A cohort object's answer, relayed within the reply bound.
async fn relay(r: &mut Response) -> Result<Response> {
    if r.status_code() != 200 {
        super::cf_io::discard(r);
        return error("cohort_unavailable", 503);
    }
    match super::cf_io::response_json::<Value>(r, REPLY_BYTES).await {
        Ok(v) => {
            let mut resp = Response::from_json(&v)?;
            resp.headers_mut().set("Cache-Control", "no-store")?;
            Ok(resp)
        }
        Err(_) => error("cohort_reply", 502),
    }
}

// ------------------------------------------------------------------ R2 I/O

fn bucket(env: &Env) -> std::result::Result<worker::Bucket, &'static str> {
    env.bucket(ADMISSION_BUCKET).map_err(|_| "admission bucket")
}

fn field(v: &JsValue, k: &str) -> JsValue {
    Reflect::get(v, &JsValue::from_str(k)).unwrap_or(JsValue::NULL)
}

/// R2's `head()` of `key`: its stored size, SHA-256, entity tag and whether
/// its attributes are the allowed ones (`admission::attrs_clean`).
async fn head(b: &worker::Bucket, key: &str) -> Head {
    let v = match super::cf_io::settle(adm_head(b.as_ref(), key), None).await {
        super::cf_io::Settled::Value(v) => v,
        _ => return Head::Error,
    };
    if v.is_null() || v.is_undefined() {
        return Head::Absent;
    }
    let (Some(size), Some(etag)) = (field(&v, "size").as_f64(), field(&v, "etag").as_string()) else {
        return Head::Error;
    };
    let sha256 = field(&v, "sha256").dyn_into::<Uint8Array>().ok().and_then(|a| {
        let mut out = [0u8; 32];
        (a.length() == 32).then(|| {
            a.copy_to(&mut out);
            out
        })
    });
    let class = field(&v, "storageClass").as_string();
    let clean = admission::attrs_clean(
        field(&v, "ssec").as_bool().unwrap_or(true),
        class.as_deref(),
        field(&v, "custom").as_bool().unwrap_or(true),
        field(&v, "http").as_bool().unwrap_or(true),
    );
    Head::Present(Stored { size: size as u64, sha256, etag, clean })
}

/// Heads every probe, at most [`HEADS_IN_FLIGHT`] at once.
async fn heads(env: &Env, probes: Vec<Probe>) -> Vec<(Probe, Head)> {
    let Ok(b) = bucket(env) else {
        return probes.into_iter().map(|p| (p, Head::Error)).collect();
    };
    let mut out = Vec::with_capacity(probes.len());
    let mut it = probes.into_iter().peekable();
    while it.peek().is_some() {
        let group: Vec<Probe> = it.by_ref().take(HEADS_IN_FLIGHT).collect();
        let got = futures_util::future::join_all(group.iter().map(|p| head(&b, &p.key))).await;
        out.extend(group.into_iter().zip(got));
    }
    out
}

/// Makes a conditional delete ([`admission::Delete`]): only while a fresh
/// head still shows its entity tag and the object is still not a clean copy
/// of the committed digest, so a correct object PUT since (the same bytes
/// have the same tag) survives. A delete that failed is queued; the queue
/// itself never deletes a key a live session holds
/// ([`admission::queued_delete_allowed`]). One delete per key is in flight
/// at a time ([`admission::delete_begin`]): a path that finds another
/// deleting the key leaves it to that one.
async fn delete_if(env: &Env, storage: &worker::Storage, d: &admission::Delete) {
    let Ok(b) = bucket(env) else { return };
    let (k, now) = (d.key.clone(), now_ms());
    if !matches!(transaction(storage, move |sql| admission::delete_begin(sql, &k, now)), Ok(true)) {
        return;
    }
    if d.allows(&head(&b, &d.key).await) && b.delete(d.key.clone()).await.is_err() {
        let (k, now) = (d.key.clone(), now_ms());
        let _ = transaction(storage, move |sql| admission::object_delete_failed(sql, &k, now));
    }
    let k = d.key.clone();
    let _ = transaction(storage, move |sql| admission::delete_end(sql, &k));
}

/// The header settle's I/O (design A1-A2): the head, then the header bytes
/// read pinned to the entity tag just seen and piped as a JS stream into
/// the upload Worker's header route (no byte enters this isolate), whose
/// bounded outcome is parsed.
async fn settle_header(env: &Env, job: &HeaderJob) -> HeaderSeen {
    let Ok(b) = bucket(env) else { return HeaderSeen::Platform("admission bucket") };
    let etag = match admission::header_head(job, &head(&b, &job.key).await) {
        Ok(etag) => etag,
        Err(seen) => return seen,
    };
    let obj = match super::cf_io::settle(adm_get_pinned(b.as_ref(), &job.key, &etag), None).await {
        super::cf_io::Settled::Value(v) => v,
        _ => return HeaderSeen::Platform("header read"),
    };
    if obj.is_null() || obj.is_undefined() {
        return HeaderSeen::Platform("header gone since its head");
    }
    let body = field(&obj, "body");
    if body.is_null() || body.is_undefined() {
        // The pinned read refused: the object changed since its head.
        return HeaderSeen::Platform("header changed since its head");
    }
    let stream = adm_fixed(&body, f64::from(job.len));
    let Some(token) = secret(env, INTERFACE_TOKEN) else { return HeaderSeen::Platform("interface token") };
    let headers = Headers::new();
    if headers.set(iface::TOKEN_HEADER, token.as_str()).is_err()
        || headers.set(iface::SPEC_HEADER, job.spec_id).is_err()
        || headers.set("content-type", "application/octet-stream").is_err()
    {
        return HeaderSeen::Platform("header request");
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(stream));
    let url = format!("{}{}", iface::UPLOAD_INTERFACE_ORIGIN, iface::UPLOAD_HEADER_PATH);
    let Ok(req) = Request::new_with_init(&url, &init) else { return HeaderSeen::Platform("header request") };
    let Ok(service) = env.service(UPLOAD_SERVICE) else { return HeaderSeen::Platform("upload service binding") };
    let Ok(mut r) = service.fetch_request(req).await else { return HeaderSeen::Platform("header call") };
    if r.status_code() != 200 {
        let marked = r.headers().get(iface::HEADER_FAILURE_HEADER).ok().flatten().is_some();
        let status = r.status_code();
        super::cf_io::discard(&mut r);
        return HeaderSeen::Platform(match (marked, status) {
            (true, _) => "upload Worker platform failure",
            (false, 400 | 404) => "header route link",
            _ => "header route unavailable",
        });
    }
    let bound = slop_ninja_artifact_check::admission::header_outcome_bound();
    match super::cf_io::response_json::<slop_ninja_artifact_check::admission::HeaderOutcome>(&mut r, bound).await {
        Ok(outcome) => HeaderSeen::Checked { etag, outcome },
        Err(_) => HeaderSeen::Platform("header outcome"),
    }
}

/// One signer request (`slop_ninja_r2_presign::wire`) through the service
/// binding with the shared token: every entry minted, or the platform's
/// failure (`signer_unavailable`).
async fn mint_urls(env: &Env, entries: &[mint::Entry]) -> std::result::Result<Vec<Minted>, &'static str> {
    let token = secret(env, admission::signer::TOKEN_SECRET).ok_or("signer token")?;
    let mut buf = vec![0u8; mint::request_len(entries.len())];
    let n = mint::encode_request(entries, &mut buf).ok_or("signer request")?;
    let headers = Headers::new();
    headers.set(admission::signer::TOKEN_HEADER, token.as_str()).map_err(|_| "signer request")?;
    headers.set("content-type", admission::signer::MEDIA_TYPE).map_err(|_| "signer request")?;
    let body = Uint8Array::from(buf.get(..n).ok_or("signer request")?);
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(body.into()));
    let url = format!("{}{}", admission::signer::ORIGIN, admission::signer::MINT_PATH);
    let req = Request::new_with_init(&url, &init).map_err(|_| "signer request")?;
    let service = env.service(admission::signer::BINDING).map_err(|_| "signer binding")?;
    let mut r = service.fetch_request(req).await.map_err(|_| "signer unreachable")?;
    if r.status_code() != 200 {
        super::cf_io::discard(&mut r);
        return Err("signer status");
    }
    let bytes = super::cf_io::response_bytes(&mut r, mint::reply_max(entries.len())).await.map_err(|_| "signer reply")?;
    let reply = mint::decode_reply(&bytes, entries.len()).ok_or("signer reply")?;
    let mut out = Vec::with_capacity(entries.len());
    for (e, got) in entries.iter().zip(reply) {
        let got = got.ok_or("signer reply")?;
        if got.status != mint::Status::Ok || got.content_length != e.len || got.valid_secs == 0 {
            return Err("signer refused an entry");
        }
        out.push(Minted { url: got.url.to_owned(), checksum: got.checksum.to_owned(), signed_at: got.signed_at, valid_secs: got.valid_secs });
    }
    Ok(out)
}

/// The upload path's end-to-end canary (skeptic K4): the signer mints a
/// presigned PUT for a `health/` key (outside the events prefix) with the
/// fixed 64-byte canary body, this Worker sends it to the S3 host (our own
/// bytes, never a miner's), confirms it with `head()` (size and stored
/// checksum), then deletes it. It does not exercise miners' Internet path.
/// `Ok(Some(key))` when the canary passed but its delete must be retried.
async fn canary(env: &Env, cohort: u64) -> std::result::Result<Option<String>, CanaryFailure> {
    let mut nonce = [0u8; 32];
    adm_random32().copy_to(&mut nonce);
    let body = layout::canary_body(&nonce);
    // The canary body is our own 64 bytes, a small record.
    let sha: [u8; 32] = Sha256::digest(body).into();
    let valid = policy::valid_secs(body.len() as u64).map_err(|_| CanaryFailure::Signer("canary validity"))?;
    let entry = mint::Entry { op: mint::Op::Canary, cohort, commitment: nonce, sha256: sha, len: body.len() as u64, valid_secs: valid };
    let minted = mint_urls(env, &[entry]).await.map_err(CanaryFailure::Signer)?;
    let url = minted.first().ok_or(CanaryFailure::Signer("signer reply"))?;
    let headers = Headers::new();
    headers.set("if-none-match", slop_ninja_r2_presign::presign::IF_NONE_MATCH).map_err(|_| CanaryFailure::Path("canary request"))?;
    headers.set("x-amz-checksum-sha256", &url.checksum).map_err(|_| CanaryFailure::Path("canary request"))?;
    let mut init = RequestInit::new();
    init.with_method(Method::Put).with_headers(headers).with_body(Some(Uint8Array::from(&body[..]).into()));
    let req = Request::new_with_init(&url.url, &init).map_err(|_| CanaryFailure::Path("canary request"))?;
    let mut r = worker::Fetch::Request(req).send().await.map_err(|_| CanaryFailure::Path("canary put unreachable"))?;
    let status = r.status_code();
    super::cf_io::discard(&mut r);
    if status != 200 {
        return Err(CanaryFailure::Path("canary put refused"));
    }
    let b = bucket(env).map_err(CanaryFailure::Path)?;
    let key = layout::health_key(cohort, &nonce);
    match head(&b, key.as_str()).await {
        Head::Present(st) if st.size == body.len() as u64 && st.sha256 == Some(sha) => {}
        Head::Present(_) => return Err(CanaryFailure::Path("canary stored otherwise")),
        Head::Absent => return Err(CanaryFailure::Path("canary absent after its put")),
        Head::Error => return Err(CanaryFailure::Path("canary head")),
    }
    Ok(b.delete(key.as_str().to_owned()).await.err().map(|_| key.as_str().to_owned()))
}

// ------------------------------------------------------- cohort object side

fn tx_or_unavailable<T>(r: std::result::Result<T, TxError<engine::EvalError>>) -> std::result::Result<T, Reply> {
    r.map_err(|_| unavailable())
}

fn unavailable() -> Reply {
    let mut e = admission::ApiError::new(slop_ninja_admission_wire::ErrorCode::Unavailable, "store");
    e.retry_after = Some(2);
    admission::reply_error(&e)
}

impl EvalCohort {
    /// The cohort object's `/admission/*` routes (from the Worker only): the
    /// forwarded session operations, the unsigned cohort read, and
    /// `observe` (the admission events Queue's consumer).
    pub(super) async fn admission_route(&self, req: &mut Request, path: &str) -> Result<Response> {
        let Some(chain) = chain(&self.env) else {
            return error("not_configured", 503);
        };
        let storage = self.state.storage();
        if path == "/admission/cohort" {
            let paused = !self.fresh();
            return match transaction(&storage, move |sql| admission::cohort_info(sql, &chain, paused)) {
                Ok(v) => json(&v),
                Err(_) => error("store_unavailable", 503),
            };
        }
        if path == "/admission/published" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct B {
                cohort: u64,
                label: String,
                entrants: Vec<admission::PublicationEntry>,
            }
            let b: B = match super::cf_io::request_json(req, admission::PUBLICATION_REPORT_BYTES).await {
                Ok(b) => b,
                Err(e) => return error(e.code(), e.status()),
            };
            let now = now_ms();
            return match transaction(&storage, move |sql| {
                admission::publication_report(sql, b.cohort, &b.label, &b.entrants, now)
            }) {
                // Not this authority's cohort: never a 200, so the publisher
                // keeps the report pending (its status shows it) instead of
                // taking it as recorded.
                Ok(v) if v["known"] == false => error("cohort_unknown", 404),
                Ok(v) => {
                    let _ = storage.set_alarm(500i64).await;
                    json(&v)
                }
                Err(_) => error("store_unavailable", 503),
            };
        }
        if path == "/admission/observe" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct B {
                key: String,
            }
            let b: B = match super::cf_io::request_json(req, admission::EVENT_MESSAGE_BYTES).await {
                Ok(b) => b,
                Err(e) => return error(e.code(), e.status()),
            };
            return match self.observe(&chain, &b.key).await {
                Ok(()) => json(&j!({"ok": true})),
                Err(code) => error(code, 503),
            };
        }
        let b: Forward = match super::cf_io::request_json(req, interface_body_bytes()).await {
            Ok(b) => b,
            Err(e) => return error(e.code(), e.status()),
        };
        // The route fields must be the signed request's own (the key-holding
        // side re-derives what the upload Worker parsed).
        let op = path.strip_prefix("/admission/").unwrap_or_default().to_owned();
        if !admission::forward_matches(&op, &b) {
            return json(&admission::forward_mismatch());
        }
        let reply = self.session_op(&op, &chain, b).await;
        json(&reply)
    }

    /// One session operation: its first transaction decides, then each
    /// step's I/O runs and its result is recorded, at most
    /// [`SESSION_STEPS`] steps.
    async fn session_op(&self, op: &str, chain: &ChainBinding, b: Forward) -> Reply {
        let storage = self.state.storage();
        let now = now_ms();
        let (c2, b2, op2) = (chain.clone(), b.clone(), op.to_owned());
        let first = transaction(&storage, move |sql| {
            Ok::<_, engine::EvalError>(match op2.as_str() {
                iface::op::CREATE => admission::create(sql, &c2, &b2.signed, now),
                iface::op::READ => Step::Reply(admission::read(sql, &c2, &b2.signed, &b2.session)),
                iface::op::URLS => admission::urls(sql, &c2, &b2.signed, &b2.session),
                iface::op::COMPLETE => admission::complete(sql, &c2, &b2.signed, &b2.session),
                iface::op::REPAIR => admission::repair(sql, &c2, &b2.signed, &b2.session),
                iface::op::HOTKEY => Step::Reply(admission::hotkey_view(sql, &c2, &b2.signed, b2.cohort, &b2.hotkey)),
                _ => Step::Reply(admission::forward_mismatch()),
            })
        });
        let mut step = match tx_or_unavailable(first) {
            Ok(s) => s,
            Err(reply) => return reply,
        };
        for _ in 0..SESSION_STEPS {
            step = match step {
                Step::Reply(r) => return r,
                Step::Mint(job) => {
                    let minted = mint_urls(&self.env, &job.entries).await;
                    let now = now_ms();
                    return transaction(&storage, move |sql| admission::finish_mint(sql, &job, minted, now))
                        .unwrap_or_else(|_| unavailable());
                }
                Step::Header(job) => {
                    let settled = self.settle(chain, &job).await;
                    let (c2, b2, op2) = (chain.clone(), b.clone(), op.to_owned());
                    let next = transaction(&storage, move |sql| {
                        Ok::<_, engine::EvalError>(match op2.as_str() {
                            iface::op::URLS => admission::urls_after(sql, &c2, &b2.signed, &b2.session, &settled),
                            iface::op::COMPLETE => admission::complete_after(sql, &c2, &b2.signed, &b2.session, &settled),
                            _ => Step::Reply(admission::read(sql, &c2, &b2.signed, &b2.session)),
                        })
                    });
                    match tx_or_unavailable(next) {
                        Ok(s) => s,
                        Err(reply) => return reply,
                    }
                }
                Step::Heads(probes) => {
                    let who = if op == iface::op::REPAIR { Recorder::Recorded } else { Recorder::Upload };
                    self.record_heads(probes, who).await;
                    let b2 = b.clone();
                    let op2 = op.to_owned();
                    let next = transaction(&storage, move |sql| {
                        Ok::<_, engine::EvalError>(match op2.as_str() {
                            iface::op::REPAIR => admission::repair_after(sql, &b2.session),
                            _ => admission::complete_reply(sql, &b2.session)
                                .map_or_else(|e| Step::Reply(admission::reply_error(&e)), Step::Reply),
                        })
                    });
                    match tx_or_unavailable(next) {
                        Ok(s) => s,
                        Err(reply) => return reply,
                    }
                }
            };
        }
        unavailable()
    }

    /// Settles one header (its I/O, then [`admission::apply_header`]); a
    /// header that refused its session is deleted while it keeps the tag
    /// seen. Arms the alarm when the session may have moved on.
    async fn settle(&self, chain: &ChainBinding, job: &HeaderJob) -> Settled {
        let seen = settle_header(&self.env, job).await;
        let storage = self.state.storage();
        let (c2, j2, now) = (chain.clone(), job.clone(), now_ms());
        let settled = match transaction(&storage, move |sql| admission::apply_header(sql, &c2, &j2, seen, now)) {
            Ok(s) => s,
            Err(_) => Settled::Platform("store"),
        };
        if let Settled::Refused(Some(d)) | Settled::Unclean(d) = &settled {
            delete_if(&self.env, &storage, d).await;
        }
        settled
    }

    /// Heads probes and records them ([`admission::apply_heads`]); deletes
    /// the mismatched objects it found. Arms the alarm when a session may
    /// have completed (its manifest step is next).
    async fn record_heads(&self, probes: Vec<Probe>, who: Recorder) -> Applied {
        if probes.is_empty() {
            return Applied::default();
        }
        let seen = heads(&self.env, probes).await;
        let storage = self.state.storage();
        let now = now_ms();
        let applied = transaction(&storage, move |sql| admission::apply_heads(sql, &seen, who, now))
            .unwrap_or(Applied { deletes: Vec::new(), platform: true });
        for d in &applied.deletes {
            delete_if(&self.env, &storage, d).await;
        }
        let _ = storage.set_alarm(500i64).await;
        applied
    }

    /// An R2 object-create event for `key` (a wakeup only): settles that
    /// object as a client request would, or deletes an object no live
    /// session holds.
    async fn observe(&self, chain: &ChainBinding, key: &str) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let k = key.to_owned();
        let o = transaction(&storage, move |sql| admission::observe(sql, &k)).map_err(|_| "store_unavailable")?;
        match o {
            Observed::Nothing => {}
            Observed::Delete(k) => {
                let b = bucket(&self.env)?;
                if b.delete(k.clone()).await.is_err() {
                    let now = now_ms();
                    transaction(&storage, move |sql| admission::object_delete_failed(sql, &k, now)).map_err(|_| "store_unavailable")?;
                }
            }
            Observed::Header(job) => {
                self.settle(chain, &job).await;
            }
            Observed::Heads(probes) => {
                let who = if probes.iter().any(|p| p.etag.is_some()) { Recorder::Recorded } else { Recorder::Upload };
                self.record_heads(probes, who).await;
            }
        }
        Ok(())
    }

    /// Whether the cohort object's heartbeat is fresh (the coordinator is
    /// observing); admission answers `paused` in the cohort response when not.
    fn fresh(&self) -> bool {
        let storage = self.state.storage();
        transaction(&storage, engine::observed)
            .map(|o| now_ms().saturating_sub(o.kick_ms) < engine::HEARTBEAT_FRESH_MS)
            .unwrap_or(false)
    }

    /// Writes the cohort snapshot (`internal::CohortSnapshot`) to the
    /// eval bucket (review round 2: not the admission bucket, which the
    /// presigning credential can write): the cohort response as this object
    /// answers it now, and when, with the hotkeys waiting for a repair (the
    /// gate's exemption from registration, `admission::repair_list`).
    /// Called at every heartbeat (`/kick`) of the configured main authority,
    /// so the snapshot follows the cursor about once a block. A failed write
    /// is left to the next heartbeat; a snapshot over the bound the upload
    /// Worker reads within is not written.
    pub(super) async fn write_cohort_snapshot(&self) {
        let Some(chain) = chain(&self.env) else { return };
        let Ok(bucket) = self.env.bucket(EVAL_BUCKET) else { return };
        let paused = !self.fresh();
        let storage = self.state.storage();
        let Ok((cohort, (repair, repair_all))) = transaction(&storage, move |sql| {
            Ok::<_, engine::EvalError>((admission::cohort_info(sql, &chain, paused)?, admission::repair_list(sql)?))
        }) else {
            return;
        };
        let snap = iface::CohortSnapshot {
            schema: iface::COHORT_SNAPSHOT_SCHEMA.into(),
            written_ms: now_ms(),
            cohort,
            repair,
            repair_all,
        };
        let Ok(bytes) = serde_json::to_vec(&snap) else { return };
        if bytes.len() > iface::cohort_snapshot_limit() {
            return;
        }
        let _ = bucket.put(iface::COHORT_SNAPSHOT_KEY, bytes).execute().await;
    }

    /// The cohort object's `/template`, `/template/pause` and `/calendar`
    /// operator routes.
    pub(super) async fn template_route(
        &self,
        req: &mut Request,
        path: &str,
        cfg: engine::EvalConfig,
    ) -> Result<Response> {
        let storage = self.state.storage();
        let now = now_ms();
        match (req.method(), path) {
            (Method::Post, "/template") => {
                let bytes = match super::cf_io::request_bytes(req, crate::arena_routes::MAX_PLAN_BODY_BYTES).await {
                    Ok(b) => b,
                    Err(e) => return error(e.code(), e.status()),
                };
                // A named main authority installs a template only once every
                // earlier authority recorded it as its successor; a template
                // it would refuse supersedes nothing.
                if let Err(e) = calendar::check(&cfg, &bytes) {
                    let mut resp = Response::from_json(&j!({"error": e.code()}))?.with_status(422);
                    resp.headers_mut().set("Cache-Control", "no-store")?;
                    return Ok(resp);
                }
                if let Err(code) = self.succeed(&cfg).await {
                    return error(code, 409);
                }
                match transaction(&storage, move |sql| calendar::install(sql, &cfg, &bytes, now)) {
                    Ok(v) => {
                        storage.set_alarm(500i64).await?;
                        json(&v)
                    }
                    Err(TxError::Operation(e)) => {
                        let mut resp = Response::from_json(&j!({"error": e.code()}))?.with_status(422);
                        resp.headers_mut().set("Cache-Control", "no-store")?;
                        Ok(resp)
                    }
                    Err(TxError::Runtime) => error("store_unavailable", 503),
                }
            }
            (Method::Post, "/template/pause") => {
                let v: Value = super::cf_io::request_json(req, 1024).await.unwrap_or(Value::Null);
                let paused = v.get("paused") != Some(&Value::Bool(false));
                match transaction(&storage, move |sql| calendar::set_paused(sql, paused)) {
                    Ok(found) => json(&j!({"template": found, "paused": paused})),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            (Method::Post, "/session/refuse") => {
                // The operator's recorded way out of a stalled session.
                #[derive(Deserialize)]
                struct B {
                    session: String,
                    reason: String,
                }
                let b: B = match super::cf_io::request_json(req, 1024).await {
                    Ok(b) => b,
                    Err(e) => return error(e.code(), e.status()),
                };
                match transaction(&storage, move |sql| {
                    admission::operator_refuse(sql, &b.session, &b.reason, now)
                }) {
                    Ok(refused) => {
                        storage.set_alarm(500i64).await?;
                        json(&j!({"refused": refused}))
                    }
                    Err(TxError::Operation(e)) => error(e.code(), 400),
                    Err(TxError::Runtime) => error("store_unavailable", 503),
                }
            }
            (Method::Get, "/calendar") => match transaction(&storage, calendar::view) {
                Ok(v) => json(&v),
                Err(_) => error("store_unavailable", 503),
            },
            (Method::Get, "/cost-model") => {
                // The latest cohort's cost model, else the template's.
                let r = transaction(&storage, |sql| {
                    // One row: the newest cohort whose plan has a fanout
                    // block, never every cohort's plan.
                    let rows = sql.exec(
                        "SELECT seq FROM cohorts WHERE json_extract(plan, '$.fanout') IS NOT NULL ORDER BY seq DESC LIMIT 1",
                        &[],
                    )?;
                    let last = match rows.first() {
                        Some(r) => engine::cohort(sql, crate::sql::int(r, 0)?)?.plan.fanout.map(|f| f.cost),
                        None => None,
                    };
                    Ok::<_, engine::EvalError>(match last {
                        Some(m) => Some(m),
                        None => calendar::template(sql)?.and_then(|p| p.fanout.map(|f| f.cost)),
                    })
                });
                match r {
                    Ok(Some(m)) => json(&m),
                    Ok(None) => error("no_cost_model", 404),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            _ => error("not_found", 404),
        }
    }

    /// The cohort object's commitment-reader routes (from the Worker's cron).
    pub(super) async fn commitments_route(&self, req: &mut Request, path: &str) -> Result<Response> {
        let Some(chain) = chain(&self.env) else {
            return error("not_configured", 503);
        };
        let storage = self.state.storage();
        let netuid = chain.netuid;
        let now = now_ms();
        match path {
            "/commitments/plan" => {
                #[derive(Deserialize)]
                struct B {
                    /// The reader's share of the RPC budget, calls a minute.
                    per_minute: u64,
                    /// The planning invocation's remaining time: how long a
                    /// planned round's claim lasts unless its ingest ends it.
                    lease_ms: u64,
                    /// Its time left once this call returns (at the call's
                    /// allowance): the round keeps the reads that fit.
                    #[serde(default)]
                    fit_ms: Option<u64>,
                    /// The Worker's observations of its previous calls.
                    #[serde(default)]
                    prior: commitments::Prior,
                }
                let b: B = match super::cf_io::request_json(req, 1024).await {
                    Ok(b) => b,
                    Err(e) => return error(e.code(), e.status()),
                };
                let r = transaction(&storage, move |sql| {
                    commitments::note_prior(sql, &b.prior)?;
                    let o = engine::observed(sql)?;
                    let needs = commitments::Needs {
                        poll: admission::window(sql)?.is_some_and(|w| w.admitting()),
                        snapshot: admission::snapshot_needs(sql)?,
                    };
                    // The stored metadata is parsed only when a round could
                    // be planned (nothing is due between windows).
                    let rt = if commitments::wants_round(sql, &needs, now)? { runtime(sql)? } else { None };
                    commitments::plan_claimed(sql, rt.as_ref(), netuid, o.confirmed, &needs, b.per_minute, now, b.lease_ms, b.fit_ms)
                });
                match r {
                    Ok(round) => json(&round),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            "/commitments/starved" => match transaction(&storage, commitments::note_starved) {
                Ok(()) => json(&serde_json::json!({"ok": true})),
                Err(_) => error("store_unavailable", 503),
            },
            "/commitments/ingest" => {
                #[derive(Deserialize)]
                struct B {
                    results: Vec<commitments::ReadResult>,
                }
                let b: B = match super::cf_io::request_json(req, READER_BODY_BYTES).await {
                    Ok(b) => b,
                    Err(e) => return error(e.code(), e.status()),
                };
                let r = transaction(&storage, move |sql| {
                    // Not parsed for a claim release or the runtime round.
                    let rt = if commitments::ingest_needs_runtime(&b.results) { runtime(sql)? } else { None };
                    let out = commitments::ingest(sql, rt.as_ref(), netuid, &b.results, now)?;
                    if let Some((spec, tx, meta, at, marker)) = &out.runtime {
                        // Stored only when it parses as V15 metadata.
                        if crate::chain::ChainRuntime::parse(meta, *spec, *tx).is_ok() {
                            commitments::store_runtime(sql, *spec, *tx, meta, *at, marker)?;
                        }
                    }
                    Ok::<_, engine::EvalError>(out)
                });
                match r {
                    Ok(out) => json(&out),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            _ => error("not_found", 404),
        }
    }

    // ------------------------------------------------------------- steps

    /// One admission step of the alarm.
    pub(super) async fn admission_step(
        &self,
        work: engine::Work,
        cfg: &engine::EvalConfig,
    ) -> std::result::Result<(), &'static str> {
        match work {
            engine::Work::AdmissionVerify { session, stage } if stage == "manifest" => self.manifest_step(&session).await,
            engine::Work::AdmissionVerify { session, .. } => self.audit_step(&session).await,
            engine::Work::AdmissionCanary => self.canary_step().await,
            engine::Work::AdmissionClose => self.close_step().await,
            engine::Work::AdmissionSweep => self.sweep_step(&cfg.chain).await,
            engine::Work::AdmissionLanes => self.lanes().await,
            engine::Work::AdmissionCleanup => self.cleanup().await,
            engine::Work::AdmissionReconcile => self.reconcile_step().await,
            engine::Work::OpenNext { label, plan } => self.open_next(cfg, &label, plan).await,
            _ => Ok(()),
        }
    }

    /// Writes a verifying session's files manifest v2 to the eval bucket
    /// (a small record the eval Worker builds; no upload byte), then its
    /// admission run is next.
    async fn manifest_step(&self, session: &str) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let id = session.to_owned();
        let due = transaction(&storage, move |sql| admission::manifest_due(sql, &id));
        let now = now_ms();
        let Ok(Some((key, bytes))) = due else {
            let id = session.to_owned();
            if due.is_err() {
                transaction(&storage, move |sql| admission::step_failed(sql, &id, now)).map_err(|_| "manifest commit")?;
            }
            return Ok(());
        };
        let bucket = self.env.bucket(EVAL_BUCKET).map_err(|_| "eval bucket")?;
        let ok = super::cf_io::r2_put(&bucket, &key, &bytes, None).await;
        let (id, env, now) = (session.to_owned(), admission_env(&self.env), now_ms());
        transaction(&storage, move |sql| {
            if ok { admission::manifest_written(sql, &env, &id, now) } else { admission::step_failed(sql, &id, now) }
        })
        .map_err(|_| "manifest commit")
    }

    /// Audits a session whose admission run met a missing or changed object.
    async fn audit_step(&self, session: &str) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let id = session.to_owned();
        let probes = transaction(&storage, move |sql| admission::audit_probes(sql, &id)).map_err(|_| "audit")?;
        let applied = self.record_heads(probes, Recorder::Recorded).await;
        let (id, now) = (session.to_owned(), now_ms());
        transaction(&storage, move |sql| admission::audit_done(sql, &id, &applied, now)).map_err(|_| "audit commit")
    }

    /// The upload path's canary (the close's first step, and the retry
    /// while a fault is outstanding).
    async fn canary_step(&self) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let now = now_ms();
        let Some(cohort) = transaction(&storage, move |sql| admission::canary_begin(sql, now)).map_err(|_| "canary")? else {
            return Ok(());
        };
        let outcome = canary(&self.env, cohort).await;
        // The canary also covers the front every session passes before any
        // file URL exists: the upload Worker's header route and its
        // registration read (review finding: their outage must defer the
        // close too).
        let outcome = match outcome {
            Ok(k) => match header_probe(&self.env).await {
                Ok(()) => Ok(k),
                Err(code) => match k {
                    Some(undeleted) => {
                        let (u, now) = (undeleted, now_ms());
                        let _ = transaction(&storage, move |sql| admission::object_delete_failed(sql, &u, now));
                        Err(CanaryFailure::Front(code))
                    }
                    None => Err(CanaryFailure::Front(code)),
                },
            },
            Err(f) => Err(f),
        };
        let now = now_ms();
        let (result, undeleted) = match outcome {
            Ok(k) => (Ok(()), k),
            Err(f) => (Err(f), None),
        };
        transaction(&storage, move |sql| {
            admission::canary_done(sql, result, now)?;
            if let Some(k) = &undeleted {
                admission::object_delete_failed(sql, k, now)?;
            }
            Ok::<_, engine::EvalError>(())
        })
        .map_err(|_| "canary commit")
    }

    /// One batch of the close sweep.
    async fn close_step(&self) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let now = now_ms();
        let Some((seq, probes)) = transaction(&storage, move |sql| admission::close_begin(sql, now)).map_err(|_| "close")? else {
            return Ok(());
        };
        let applied = self.record_heads(probes, Recorder::Close).await;
        let now = now_ms();
        transaction(&storage, move |sql| admission::close_step_done(sql, seq, &applied, now)).map_err(|_| "close commit")
    }

    /// The alarm sweep: headers settled, unrecorded files and objects
    /// awaiting repair headed.
    async fn sweep_step(&self, chain: &ChainBinding) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let now = now_ms();
        let Some(sweep) = transaction(&storage, move |sql| admission::sweep_begin(sql, now)).map_err(|_| "sweep")? else {
            return Ok(());
        };
        for job in &sweep.headers {
            self.settle(chain, job).await;
        }
        let (recorded, upload): (Vec<Probe>, Vec<Probe>) = sweep.heads.into_iter().partition(|p| p.etag.is_some());
        self.record_heads(upload, Recorder::Upload).await;
        self.record_heads(recorded, Recorder::Recorded).await;
        Ok(())
    }

    /// One page of the standing reconciliation of `admission/v2/`: every key
    /// no live session holds is deleted (a stale URL recreated it).
    async fn reconcile_step(&self) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let now = now_ms();
        let Some(cursor) = transaction(&storage, move |sql| admission::reconcile_begin(sql, now)).map_err(|_| "reconcile")? else {
            return Ok(());
        };
        let b = bucket(&self.env)?;
        let mut list = b.list().prefix(layout::ADMISSION_PREFIX.to_owned());
        if let Some(c) = cursor {
            list = list.cursor(c);
        }
        let page = list.execute().await.map_err(|_| "reconcile list")?;
        let keys: Vec<String> = page.objects().iter().map(worker::Object::key).collect();
        let next = if page.truncated() { page.cursor() } else { None };
        let now = now_ms();
        let gone = transaction(&storage, move |sql| admission::reconcile_page(sql, &keys, next.as_deref(), now))
            .map_err(|_| "reconcile commit")?;
        for k in gone {
            if b.delete(k.clone()).await.is_err() {
                let now = now_ms();
                let _ = transaction(&storage, move |sql| admission::object_delete_failed(sql, &k, now));
            }
        }
        Ok(())
    }

    /// Runs the claimable verdicts on fresh scorer instances, at most
    /// [`eval_do::PARALLEL`] at once and never past the deployment's ceiling.
    async fn lanes(&self) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let env = admission_env(&self.env);
        let e2 = env.clone();
        let now = now_ms();
        let runs = transaction(&storage, move |sql| {
            admission::claim_runs(sql, &e2, eval_do::PARALLEL, now)
        })
        .map_err(|_| "claim runs")?;
        // Each run is recorded as soon as it finishes, with its instance time
        // at the cohort's Container rates (one charge key per hotkey,
        // commitment and try), so a later run cut off by the platform never
        // drops an earlier run's outcome or charge. The settle in the tracking
        // ledger (never refused) is best effort here: a charge it did not
        // confirm stays recorded and the step settles it later
        // (`Work::AdmissionSettle`).
        let storage = &storage;
        let results = futures_util::future::join_all(runs.into_iter().map(|run| {
            let env = env.clone();
            async move {
                let name = run.lane_name(crate::config::NETWORK);
                // The lane's admission route: a fresh instance, the cohort's
                // label, spec digest and lane limits, and the definitive flag.
                let body = admission::admit_body(&run);
                let t0 = now_ms();
                let outcome = async {
                    let Ok(stub) = lane_stub(&self.env, "score", &name) else {
                        return LaneOutcome::Platform("lane_binding".into());
                    };
                    let Ok(req) = eval_do::internal_request(Method::Post, "/admit", Some(body.to_string())) else {
                        return LaneOutcome::Platform("lane_request".into());
                    };
                    match stub.fetch_with_request(req).await {
                        Ok(mut r) => {
                            let status = r.status_code();
                            let v: Value = super::cf_io::response_json(&mut r, 64 * 1024)
                                .await
                                .unwrap_or(Value::Null);
                            admission::classify_admit(Some(status), &v)
                        }
                        Err(_) => admission::classify_admit(None, &Value::Null),
                    }
                }
                .await;
                // The instance served one artifact: stop it now.
                if let (Ok(stub), Ok(req)) = (
                    lane_stub(&self.env, "score", &name),
                    eval_do::internal_request(Method::Post, "/stop", None),
                ) {
                    let _ = stub.fetch_with_request(req).await;
                }
                let wall_ms = now_ms().saturating_sub(t0);
                let now = now_ms();
                let charge = transaction(storage, move |sql| {
                    let bound = admission::failures_bound(sql, &run.label)?;
                    admission::record_run(sql, &env, &run, &outcome, bound, now)?;
                    admission::record_charge(sql, &run, wall_ms, now)
                })
                .map_err(|_| "record run")?;
                if let Some((key, micro)) = charge
                    && eval_do::ledger_call(
                        &self.env,
                        "/settle",
                        &j!({"key": key, "microusd": micro, "note": "admission run"}),
                    )
                    .await
                    .is_ok()
                {
                    let _ = transaction(storage, move |sql| admission::charge_settled(sql, &key));
                }
                Ok::<(), &'static str>(())
            }
        }))
        .await;
        results.into_iter().collect()
    }

    /// Deletes the queued objects; a delete the bucket does not confirm
    /// backs its row off and the others still run.
    async fn cleanup(&self) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let now = now_ms();
        let pending = transaction(&storage, move |sql| {
            admission::object_deletes_due(sql, admission::R2_CALLS_PER_STEP as i64, now)
        })
        .map_err(|_| "object deletes due")?;
        let uploads = bucket(&self.env)?;
        for (key, upload_id) in pending {
            // A key a live session holds (it was recorded, or re-PUT, since
            // the delete was queued) is never deleted from the queue: that
            // session's own heads delete an unclean copy conditionally.
            let k = key.clone();
            let allowed = transaction(&storage, move |sql| admission::queued_delete_allowed(sql, &k))
                .map_err(|_| "object delete check")?;
            if !allowed {
                let k = key.clone();
                transaction(&storage, move |sql| admission::object_deleted(sql, &k)).map_err(|_| "object delete commit")?;
                continue;
            }
            if let Some(u) = upload_id
                && let Ok(m) = uploads.resume_multipart_upload(key.clone(), u)
            {
                // A v1 multipart upload left by the store migration.
                let _ = m.abort().await;
            }
            let ok = uploads.delete(key.clone()).await.is_ok();
            let now = now_ms();
            transaction(&storage, move |sql| {
                if ok { admission::object_deleted(sql, &key) } else { admission::object_delete_failed(sql, &key, now) }
            })
            .map_err(|_| "object delete commit")?;
        }
        Ok(())
    }

    /// Queues the next cohort from the standing template. Its budgets are
    /// recorded in the tracking ledger (never refused); a plan the engine
    /// refuses pauses the template with the reason, for the operator.
    async fn open_next(
        &self,
        cfg: &engine::EvalConfig,
        label: &str,
        plan: Vec<u8>,
    ) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let adm = match engine::admit(cfg, &plan) {
            Ok((_, adm)) => adm,
            Err(e) => {
                let code = e.code();
                let _ = transaction(&storage, move |sql| {
                    calendar::note_error(sql, code)?;
                    calendar::set_paused(sql, true).map(|_| ())
                });
                return Err("template plan refused");
            }
        };
        // The calendar of a named authority opens nothing until every
        // earlier authority recorded it as its successor (`succeed`, done
        // once at the template's install or the first plan).
        if let Err(code) = self.succeed(cfg).await {
            let _ = transaction(&storage, move |sql| {
                calendar::note_error(sql, code)?;
                calendar::set_paused(sql, true).map(|_| ())
            });
            return Err("template predecessors");
        }
        let key = format!("{}:{label}", engine::INSTANCE_MAIN);
        // `created` is false when the key was already held (a retried open):
        // then a refused queue leaves it alone, as the operator `/plan` does.
        let created = match eval_do::ledger_call(
            &self.env,
            "/reserve",
            &j!({"key": key, "microusd": adm.reserve_microusd}),
        )
        .await
        {
            Ok(v) => v.get("created").and_then(Value::as_bool) == Some(true),
            Err(_) => false,
        };
        let c2 = cfg.clone();
        let now = now_ms();
        match transaction(&storage, move |sql| engine::queue(sql, &c2, &plan, now)) {
            Ok(_) => Ok(()),
            Err(TxError::Operation(e)) => {
                // A refused queue never leaves an orphan reservation that
                // every later cycle's total would count.
                if created {
                    let _ = eval_do::ledger_call(&self.env, "/release", &j!({"key": key})).await;
                }
                let code = e.code();
                let _ = transaction(&storage, move |sql| {
                    calendar::note_error(sql, code)?;
                    calendar::set_paused(sql, true).map(|_| ())
                });
                Err("template queue refused")
            }
            Err(TxError::Runtime) => Err("template queue"),
        }
    }
}

/// The stored reader runtime, parsed (`None` until one is stored).
fn runtime(sql: &impl Sql) -> std::result::Result<Option<crate::chain::ChainRuntime>, engine::EvalError> {
    Ok(commitments::load_runtime(sql)?
        .and_then(|(spec, tx, meta)| crate::chain::ChainRuntime::parse(&meta, spec, tx).ok()))
}

/// The deployment facts verdicts bind to.
pub fn admission_env(env: &Env) -> AdmissionEnv {
    AdmissionEnv {
        scorer_binary: env
            .var("SCORER_BINARY_SHA256")
            .map(|v| v.to_string())
            .unwrap_or_default(),
        max_instances: env
            .var("MAX_SCORE_LANES")
            .ok()
            .and_then(|v| v.to_string().trim().parse().ok())
            .unwrap_or(0),
    }
}

// ------------------------------------------------- the events consumer

/// The admission events Queue's consumer (eval deployment only): each R2
/// object-create notification, refused past its bound before it is decoded
/// (`admission::EVENT_MESSAGE_BYTES`), names a key the cohort object then
/// observes (it heads the object itself: the payload carries no checksum,
/// and a forged or repeated message costs one head). A message whose key is
/// not an admission object's is acknowledged and dropped; one the cohort
/// object could not take is retried.
pub async fn events(batch: worker::worker_sys::MessageBatch, env: Env, scope: slop_ninja_arena::ScopeRef) -> Result<()> {
    let Ok(all) = batch.messages() else {
        let _ = batch.retry_all(JsValue::UNDEFINED);
        return Ok(());
    };
    for v in all.iter() {
        let m: worker::worker_sys::Message = v.unchecked_into();
        // The message's size is checked on the JS string before any copy
        // enters this isolate's memory: its UTF-8 form is at least as long
        // as its UTF-16 length, so a longer string is refused uncopied, and
        // a copy is at most three bytes per unit (`event_key` then checks
        // the exact bound).
        let key = m
            .body()
            .ok()
            .and_then(|b| js_sys::JSON::stringify(&b).ok())
            .filter(|s| s.length() as usize <= admission::EVENT_MESSAGE_BYTES)
            .and_then(|s| s.as_string())
            .and_then(|s| admission::event_key(&s));
        let Some(key) = key else {
            let _ = m.ack();
            continue;
        };
        let delivered = match scope.child_physical(crate::arena_routes::R_QUEUE_ADMISSION, crate::arena_routes::ADMISSION_EVENT_BYTES) {
            Ok(child) => {
                let body = j!({"key": key}).to_string();
                let r = match child.scoped(cohort_fetch(&env, engine::INSTANCE_MAIN, Method::Post, "/admission/observe", Some(body))) {
                    Ok(f) => match f.await {
                        Ok(mut r) => {
                            let ok = r.status_code() == 200;
                            super::cf_io::discard(&mut r);
                            ok
                        }
                        Err(_) => false,
                    },
                    Err(_) => false,
                };
                let _ = child.close();
                r
            }
            Err(_) => false,
        };
        let _ = if delivered { m.ack() } else { m.retry(JsValue::UNDEFINED) };
    }
    Ok(())
}

/// The header route's link probe (marked by `internal::HEADER_PROBE_HEADER`,
/// no body): the service binding, the internal host, the shared token, and
/// the upload Worker's own registration read. The close's canary step runs
/// it; its failure is the `upload_front_unavailable` fault.
pub async fn header_probe(env: &Env) -> std::result::Result<(), &'static str> {
    let token = secret(env, INTERFACE_TOKEN).ok_or("interface token not configured")?;
    let headers = Headers::new();
    headers.set(iface::TOKEN_HEADER, token.as_str()).map_err(|_| "header request")?;
    headers.set(iface::HEADER_PROBE_HEADER, "1").map_err(|_| "header request")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers);
    let url = format!("{}{}", iface::UPLOAD_INTERFACE_ORIGIN, iface::UPLOAD_HEADER_PATH);
    let req = Request::new_with_init(&url, &init).map_err(|_| "header request")?;
    let service = env.service(UPLOAD_SERVICE).map_err(|_| "upload service binding missing")?;
    let mut r = service.fetch_request(req).await.map_err(|_| "header call failed")?;
    if r.status_code() != 200 {
        let status = r.status_code();
        super::cf_io::discard(&mut r);
        return Err(match status {
            404 => "upload Worker refused the host or token (404)",
            503 => "upload Worker's registration read is unavailable",
            _ => "upload Worker answered an error",
        });
    }
    let v: Value = super::cf_io::response_json(&mut r, 1024).await.map_err(|_| "header reply")?;
    if v.get("probe").and_then(Value::as_str) == Some("ok") { Ok(()) } else { Err("header reply") }
}

/// The eval Worker's links (`GET /eval/admission/link`, status token): the
/// link probe above, and whether the signer's binding and token are
/// configured (a mint is a canary, run by the cohort object).
pub async fn link_probe(env: &Env) -> Value {
    let header = match header_probe(env).await {
        Ok(()) => j!("ok"),
        Err(e) => j!(e),
    };
    let signer = j!({
        "binding": env.service(admission::signer::BINDING).is_ok(),
        "token": secret(env, admission::signer::TOKEN_SECRET).is_some_and(|t| t.len() >= iface::MIN_TOKEN_BYTES),
    });
    j!({"header": header, "signer": signer})
}
