//! The store's routes on Cloudflare (wasm32 only): the forwarded `share`,
//! `manage`, `rerun` and `state` routes, the binding-only `/internal/v1/*`
//! routes, the `detect-jobs` consumer and the hourly cron. Every flow is a
//! pure function of `crate::db::Io` (tested natively on the store's own
//! migration); this module reads the request, admits it, builds the
//! invocation's store (`cf_store`), keys and serving package, and answers.
//!
//! - **Admission.** A forwarded route goes through the free check's
//!   admission (`routes::forwarded`: the `X-SN-*` allow-list, the CSRF
//!   check, `X-SN-Principal` only where the route table takes one, so only
//!   on `share` from the site host). An internal route is refused when it
//!   carries `X-SN-Route` or `X-SN-Host`, and takes `X-SN-Principal` on the
//!   owner route only, where it is required.
//! - **Memory.** A link or a preview reserves its footprint from the
//!   isolate's scan memory by its declared length before reading a byte
//!   (`routes::text_footprint`); a document read reserves by the object's
//!   size (`cf_store`); a re-run also reserves its scan, as a paste does.
//! - **Re-run.** A secret page's re-run scores its stored text as a paste
//!   is scored: `canon_v1`, the free cut, a gate ticket of class
//!   interactive keyed by the address prefix, the lane, the answer's
//!   allow-list, and the pot's count (`free_text` when forwarded from the
//!   site host). It writes no progress lines: the route answers once.
//! - Nothing here writes a console line or echoes input; every answer is
//!   `no-store`.

use serde::Serialize;
use serde::de::DeserializeOwned;
use slop_ninja_auth_core::csrf::{BodyType, content_type_ok};
use slop_ninja_detect_page::api::{ManageRequest, RerunRequest, ShareRequest, StateRequest};
use slop_ninja_detect_page::internal::{
    CardRequest, ExampleRequest, OwnerRequest, PageRequest, PreviewRequest, StatesRequest,
};
use slop_ninja_detect_page::queue::QueueMessage;
use slop_ninja_detect_page::sn::InternalRoute;
use slop_ninja_detect_page::{Channel, Origins, RefusalCode, RouteId, ae};
use wasm_bindgen::{JsCast, JsValue};
use worker::worker_sys::{self, web_sys};
use worker::{Env, Request, Response};

use crate::cf::{
    Quiet, Reserved, Scan, admit, admit_internal, body_code, header, json_raw, origins, package, point, prepare,
    read_switches, refuse, run, store_secret, submitted, var, waits,
};
use crate::cf_io::{self, now_ms};
use crate::cf_store::CfStore;
use crate::doc::{Serving, StoredResult};
use crate::keys::{PEPPER_VAR_PREFIX, Peppers, STORE_SECRET_VAR, StoreKeys};
use crate::manage::OwnerAnswer;
use crate::routes::forwarded::Admission;
use crate::routes::{self, NoRoom};
use crate::share::{Keys, Sealed};
use crate::{answer, canon, cron, handoff, lookup, manage, queue, reads, rerun, seal, share, switches};

// ------------------------------------------------------------- the context

/// The store's keys: `DETECT_STORE_SECRET` and the slug peppers
/// (`DETECT_SLUG_PEPPER_V1..`), each 64 hex digits, trimmed.
struct Secrets {
    store: StoreKeys,
    peppers: Peppers,
}

impl Secrets {
    fn of(env: &Env) -> Option<Secrets> {
        let secret =
            |name: &str| env.secret(name).ok().map(|s| s.to_string().trim().to_owned()).filter(|s| !s.is_empty());
        let store = StoreKeys::from_hex(&secret(STORE_SECRET_VAR)?)?;
        let peppers = Peppers::from_vars(|v| secret(&format!("{PEPPER_VAR_PREFIX}{v}"))).ok()?;
        Some(Secrets { store, peppers })
    }

    fn keys(&self) -> Keys<'_> {
        Keys { store: &self.store, peppers: &self.peppers }
    }
}

/// The package serving now: the package vars, `PACKAGE_LABEL`, the scorer
/// build (the first 8 hex digits of `SCORER_BINARY_SHA256`) and
/// `EVIDENCE_ID`.
fn serving(env: &Env) -> Option<Serving> {
    let p = package(env)?;
    Some(Serving {
        load: p.load,
        label: var(env, "PACKAGE_LABEL")?,
        published: p.published,
        pending: p.pending,
        scorer_build: var(env, "SCORER_BINARY_SHA256")?.chars().take(8).collect(),
        evidence: var(env, "EVIDENCE_ID")?,
    })
}

/// The invocation's store, keys, origins and serving package; `None` when
/// any is missing (`not_configured`).
struct Ctx<'e> {
    io: CfStore<'e>,
    secrets: Secrets,
    origins: Origins,
    serving: Serving,
}

fn ctx(env: &Env) -> Result<Ctx<'_>, RefusalCode> {
    let missing = RefusalCode::NotConfigured;
    Ok(Ctx {
        io: CfStore::new(env, waits(env).hold_ms).ok_or(missing)?,
        secrets: Secrets::of(env).ok_or(missing)?,
        origins: origins(env).ok_or(missing)?,
        serving: serving(env).ok_or(missing)?,
    })
}

// ----------------------------------------------------------------- answers

fn ok<T: Serialize>(v: &T) -> web_sys::Response {
    match serde_json::to_vec(v) {
        Ok(b) => json_raw(200, &b, false),
        Err(_) => refuse(RefusalCode::Internal),
    }
}

fn answer_of<T: Serialize>(r: Result<T, RefusalCode>) -> web_sys::Response {
    match r {
        Ok(v) => ok(&v),
        Err(c) => refuse(c),
    }
}

/// A sitemap shard's XML.
fn xml(body: Vec<u8>) -> web_sys::Response {
    let res = Response::from_bytes(body).and_then(|mut r| {
        let h = r.headers_mut();
        h.set("Content-Type", "application/xml; charset=utf-8")?;
        h.set("Cache-Control", "no-store")?;
        h.set("X-Content-Type-Options", "nosniff")?;
        Ok(r)
    });
    match res {
        Ok(r) => match worker::IntoResponse::into_raw(r) {
            Ok(raw) => raw,
            Err(_) => crate::cf::status_only(500),
        },
        Err(_) => crate::cf::status_only(500),
    }
}

// ------------------------------------------------------------------ bodies

fn declared(req: &Request) -> Option<u64> {
    header(req, "content-length").and_then(|v| v.trim().parse::<u64>().ok())
}

/// A JSON body of at most `bound` bytes, read within the body wait.
async fn json_body<T: DeserializeOwned>(req: &mut Request, env: &Env, bound: usize) -> Result<T, RefusalCode> {
    if declared(req).is_some_and(|d| d > bound as u64) {
        return Err(RefusalCode::TooLarge);
    }
    let until = now_ms().saturating_add(waits(env).body_ms);
    let body = cf_io::request_bytes_until(req, bound, Some(until)).await.map_err(body_code)?;
    serde_json::from_slice(&body).map_err(|_| RefusalCode::MalformedRequest)
}

/// A text body (a link's or a preview's): its declared length within
/// `routes::TEXT_BODY_BYTES`, its footprint reserved from the scan memory
/// before a byte is read.
async fn text_body<T: DeserializeOwned>(req: &mut Request, env: &Env) -> Result<(T, Reserved), RefusalCode> {
    let n = declared(req).ok_or(RefusalCode::LengthRequired)?;
    if n > routes::TEXT_BODY_BYTES {
        return Err(RefusalCode::TooLarge);
    }
    let held = Reserved::take(routes::text_footprint(n)).map_err(no_room)?;
    let bound = usize::try_from(n).map_err(|_| RefusalCode::TooLarge)?;
    Ok((json_body(req, env, bound).await?, held))
}

fn no_room(e: NoRoom) -> RefusalCode {
    match e {
        NoRoom::Busy => RefusalCode::Busy,
        NoRoom::TooLarge => RefusalCode::TooLarge,
    }
}

/// A forwarded route's admission and JSON content type.
fn forwarded(req: &Request, env: &Env, route: RouteId) -> Result<Admission, RefusalCode> {
    let a = admit(req, env, route)?;
    if !content_type_ok(header(req, "content-type").as_deref(), BodyType::Json) {
        return Err(RefusalCode::ContentType);
    }
    Ok(a)
}

/// A refusal of a forwarded route, counted as its `submit` point.
fn refused(env: &Env, route: RouteId, code: RefusalCode) -> web_sys::Response {
    submitted(env, route, code.as_str());
    refuse(code)
}

/// An opened result seal as the store takes it; `None` for a forged,
/// expired or mismatched seal, or one of another package.
fn open_seal(env: &Env, serving: &Serving, text: &str, sealed: &str) -> Option<Sealed> {
    let secret = store_secret(env)?;
    let o = seal::open_result(&secret, now_ms(), sealed, text).ok()?;
    handoff::sealed(o, serving).ok()
}

// ---------------------------------------------------------- forwarded routes

/// `POST /api/detect/v1/share`: Get a link.
pub(crate) async fn share(mut req: Request, env: &Env) -> web_sys::Response {
    let route = RouteId::Share;
    let principal = match forwarded(&req, env, route) {
        Ok(Admission::Forwarded { principal, .. }) => principal,
        Ok(Admission::Direct) => None,
        Err(c) => return refused(env, route, c),
    };
    let (body, _held) = match text_body::<ShareRequest>(&mut req, env).await {
        Ok(b) => b,
        Err(c) => return refused(env, route, c),
    };
    let c = match ctx(env) {
        Ok(c) => c,
        Err(code) => return refused(env, route, code),
    };
    let sealed = open_seal(env, &c.serving, &body.text, &body.seal);
    match share::share(&c.io, c.secrets.keys(), &body, sealed, principal).await {
        Ok(a) => ok(&a),
        Err(code) => refused(env, route, code),
    }
}

/// `POST /api/detect/v1/manage` (the secret host).
pub(crate) async fn manage(mut req: Request, env: &Env) -> web_sys::Response {
    let route = RouteId::Manage;
    if let Err(c) = forwarded(&req, env, route) {
        return refused(env, route, c);
    }
    let body = match json_body::<ManageRequest>(&mut req, env, routes::SMALL_BODY_BYTES).await {
        Ok(b) => b,
        Err(c) => return refused(env, route, c),
    };
    let c = match ctx(env) {
        Ok(c) => c,
        Err(code) => return refused(env, route, code),
    };
    answer_of(manage::manage(&c.io, c.secrets.keys(), &c.origins, &body).await)
}

/// `POST /api/detect/v1/rerun`: a secret page's re-run.
pub(crate) async fn rerun(mut req: Request, env: &Env) -> web_sys::Response {
    let route = RouteId::Rerun;
    let admission = match forwarded(&req, env, route) {
        Ok(a) => a,
        Err(c) => return refused(env, route, c),
    };
    let body = match json_body::<RerunRequest>(&mut req, env, routes::SMALL_BODY_BYTES).await {
        Ok(b) => b,
        Err(c) => return refused(env, route, c),
    };
    let c = match ctx(env) {
        Ok(c) => c,
        Err(code) => return refused(env, route, code),
    };
    let ip = header(&req, "cf-connecting-ip");
    let scorer = CfScorer { env, ip: ip.as_deref(), channel: admission.channel(route), serving: &c.serving };
    match rerun::rerun_secret(&c.io, c.secrets.keys(), &body.slug, &c.serving, &scorer).await {
        Ok(a) => ok(&a),
        Err(code) => refused(env, route, code),
    }
}

/// `POST /api/detect/v1/state`: a pending public page's poll.
pub(crate) async fn state(mut req: Request, env: &Env) -> web_sys::Response {
    let route = RouteId::State;
    if let Err(c) = forwarded(&req, env, route) {
        return refused(env, route, c);
    }
    let body = match json_body::<StateRequest>(&mut req, env, routes::SMALL_BODY_BYTES).await {
        Ok(b) => b,
        Err(c) => return refused(env, route, c),
    };
    let Some(io) = CfStore::new(env, waits(env).hold_ms) else {
        return refused(env, route, RefusalCode::NotConfigured);
    };
    answer_of(reads::state(&io, &body.slug).await)
}

/// The secret re-run's scan: the free check's own path, with no progress
/// lines.
struct CfScorer<'a> {
    env: &'a Env,
    ip: Option<&'a str>,
    channel: Option<Channel>,
    serving: &'a Serving,
}

impl rerun::Scorer for CfScorer<'_> {
    async fn score(&self, text: &str) -> Result<StoredResult, RefusalCode> {
        let (env, started) = (self.env, now_ms());
        let w = waits(env);
        let sw = read_switches(env, switches::SCORE, w.hold_ms).await;
        if !sw.on(switches::TOOL) {
            return Err(RefusalCode::ToolOff);
        }
        let pkg = package(env).ok_or(RefusalCode::NotConfigured)?;
        let reservation = routes::scan_footprint(Some(text.len() as u64));
        let _held = Reserved::take(reservation).map_err(no_room)?;
        if text.trim().is_empty() {
            return Err(RefusalCode::Empty);
        }
        let canonical = canon::canon_v1(text);
        let spans_on = sw.on(switches::SPANS);
        let a = prepare(env, RouteId::Rerun, &canonical, spans_on, pkg, self.channel, reservation, started)
            .map_err(|r| r.code)?;
        match run(env, self.ip, &mut Quiet, &w, &a).await {
            Scan::Result(mut line) => {
                answer::to_original(&mut line, text, &canonical);
                handoff::stored_result(&line.result_full(), now_ms(), a.load(), seal::POLICY_V, self.serving)
                    .map_err(|_| RefusalCode::Internal)
            }
            Scan::Refused(r) => Err(r.code),
            Scan::Gone => Err(RefusalCode::Unavailable),
        }
    }
}

// ----------------------------------------------------------- internal routes

/// `/internal/v1/*` (binding-only).
pub(crate) async fn internal(mut req: Request, env: &Env) -> web_sys::Response {
    let method = req.inner().method();
    let url = req.url().ok();
    if url.as_ref().is_none_or(|u| u.query().is_some()) {
        return refuse(RefusalCode::NotFound);
    }
    let path = url.as_ref().map(|u| u.path().to_owned()).unwrap_or_default();
    let (route, shard) = match routes::internal_route(&method, &path) {
        Ok(r) => r,
        Err(c) => return refuse(c),
    };
    let principal = match admit_internal(&req, route) {
        Ok(p) => p,
        Err(c) => return refuse(c),
    };
    let small = routes::SMALL_BODY_BYTES;
    match route {
        InternalRoute::Preview => {
            let (body, _held) = match text_body::<PreviewRequest>(&mut req, env).await {
                Ok(b) => b,
                Err(c) => return refuse(c),
            };
            let (Some(io), Some(serving)) = (CfStore::new(env, waits(env).hold_ms), serving(env)) else {
                return refuse(RefusalCode::NotConfigured);
            };
            let sealed = open_seal(env, &serving, &body.text, &body.seal);
            answer_of(reads::preview(&io, &serving, &body.text, sealed.as_ref()).await)
        }
        InternalRoute::States => {
            let body = match json_body::<StatesRequest>(&mut req, env, routes::STATES_BODY_BYTES).await {
                Ok(b) => b,
                Err(c) => return refuse(c),
            };
            let Some(io) = CfStore::new(env, waits(env).hold_ms) else { return refuse(RefusalCode::NotConfigured) };
            answer_of(manage::states(&io, &body).await)
        }
        InternalRoute::Removals => {
            let Some(io) = CfStore::new(env, waits(env).hold_ms) else { return refuse(RefusalCode::NotConfigured) };
            answer_of(reads::removals(&io).await)
        }
        InternalRoute::Sitemap => {
            let Some(io) = CfStore::new(env, waits(env).hold_ms) else { return refuse(RefusalCode::NotConfigured) };
            match reads::sitemap_shard(&io, shard.unwrap_or_default()).await {
                Ok(b) => xml(b),
                Err(c) => refuse(c),
            }
        }
        InternalRoute::Page => match json_body::<PageRequest>(&mut req, env, small).await {
            Ok(b) => {
                with_ctx(env, |c| async move { reads::page(&c.io, c.secrets.keys(), &c.serving, &b.slug).await }).await
            }
            Err(c) => refuse(c),
        },
        InternalRoute::Card => match json_body::<CardRequest>(&mut req, env, small).await {
            Ok(b) => {
                with_ctx(env, |c| async move { reads::card(&c.io, c.secrets.keys(), &c.serving, &b.slug).await }).await
            }
            Err(c) => refuse(c),
        },
        InternalRoute::Examples => {
            with_ctx(env, |c| async move { reads::examples(&c.io, c.secrets.keys(), &c.serving).await }).await
        }
        InternalRoute::Example => match json_body::<ExampleRequest>(&mut req, env, small).await {
            Ok(b) => {
                with_ctx(env, |c| async move { reads::example(&c.io, c.secrets.keys(), &c.serving, b.slot).await })
                    .await
            }
            Err(c) => refuse(c),
        },
        InternalRoute::Owner => {
            let Some(p) = principal else { return refuse(RefusalCode::SignedOut) };
            let body = match json_body::<OwnerRequest>(&mut req, env, small).await {
                Ok(b) => b,
                Err(c) => return refuse(c),
            };
            let c = match ctx(env) {
                Ok(c) => c,
                Err(code) => return refuse(code),
            };
            match manage::owner(&c.io, c.secrets.keys(), &c.origins, &p, &body).await {
                Ok(OwnerAnswer::Peek(a)) => ok(&a),
                Ok(OwnerAnswer::Claim(a)) => ok(&a),
                Ok(OwnerAnswer::Done(a)) => ok(&a),
                Err(code) => refuse(code),
            }
        }
        // The account Worker's internal routes never parse as this Worker's.
        _ => refuse(RefusalCode::NotFound),
    }
}

/// Runs `f` with the invocation's context and answers its result.
async fn with_ctx<'e, T, F, Fut>(env: &'e Env, f: F) -> web_sys::Response
where
    T: Serialize,
    F: FnOnce(Ctx<'e>) -> Fut,
    Fut: std::future::Future<Output = Result<T, RefusalCode>>,
{
    match ctx(env) {
        Ok(c) => answer_of(f(c).await),
        Err(code) => refuse(code),
    }
}

// ------------------------------------------------------- queue and cron

/// One `detect-jobs` batch (one message, `max_batch_size = 1`): each
/// message is acknowledged once its flow is done, or retried after the
/// flow's delay. A message that does not parse, or that arrives while the
/// store is not configured, is retried, so it reaches the dead-letter queue
/// (an alert) rather than being dropped.
pub(crate) async fn consume(batch: worker_sys::MessageBatch, env: Env) {
    let Ok(messages) = batch.messages() else {
        let _ = batch.retry_all(JsValue::UNDEFINED);
        return;
    };
    let secrets = Secrets::of(&env);
    let io = CfStore::new(&env, waits(&env).hold_ms);
    for m in messages.iter() {
        let m: worker_sys::Message = m.unchecked_into();
        let msg = m
            .body()
            .ok()
            .and_then(|b| js_sys::JSON::stringify(&b).ok())
            .and_then(|s| s.as_string())
            .and_then(|s| serde_json::from_str::<QueueMessage>(&s).ok());
        let ack = match (msg, &secrets, &io) {
            (Some(msg), Some(k), Some(io)) => queue::consume(io, &k.store, &msg).await,
            (None, ..) => {
                point(&env, ae::detect::ERROR, &["queue_message"], 1.0);
                queue::Ack::Retry(queue::RETRY_S)
            }
            _ => {
                point(&env, ae::detect::ERROR, &[RefusalCode::NotConfigured.as_str()], 1.0);
                queue::Ack::Retry(queue::RETRY_S)
            }
        };
        match ack {
            queue::Ack::Done => {
                let _ = m.ack();
            }
            queue::Ack::Retry(s) => {
                let o = js_sys::Object::new();
                let _ = js_sys::Reflect::set(&o, &JsValue::from_str("delaySeconds"), &JsValue::from(s));
                let _ = m.retry(o.into());
            }
        }
    }
}

/// The hourly store work (`cron::hourly`): a failure writes an AE
/// `error {cron}` point, and the next hour runs again.
pub(crate) async fn hourly(env: Env) {
    let (Some(io), Some(o)) = (CfStore::new(&env, waits(&env).hold_ms), origins(&env)) else {
        point(&env, ae::detect::ERROR, &["cron"], 1.0);
        return;
    };
    let index_on = lookup::read_switches(&io).await.on(&io, "DETECT_INDEX");
    if cron::hourly(&io, &o, index_on).await.is_err() {
        point(&env, ae::detect::ERROR, &["cron"], 1.0);
    }
}
