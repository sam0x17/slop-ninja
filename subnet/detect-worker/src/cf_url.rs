//! The URL pipeline's Cloudflare glue (wasm32): the route `POST
//! /api/detect/v1/url`, the fetch Worker over its service binding, the
//! `DetectHost` objects, the scan of a URL text (gate class `url`, counted as
//! `free_url`) and the IndexNow ping.
//!
//! The route answers a refusal before any slug as a plain API refusal with
//! its status. Once the check has a page it answers an NDJSON stream: one
//! `{"state":"pending","slug"}` line while this invocation (or another one
//! that holds the claim) checks it, then `{"state":"done","slug"}` or a
//! refusal line. A page already made answers `done` at once.

use serde::Serialize;
use serde::de::DeserializeOwned;
use slop_ninja_auth_core::csrf::{BodyType, content_type_ok};
use slop_ninja_detect_fetch_wire::{
    FETCH_ORIGIN, FETCH_PATH, FetchAnswer, FetchRequest, X_CHECK_K, X_CHECK_PATH, X_POST_PATH, XCheckAnswer,
    XCheckRequest, XPostAnswer, XPostRequest,
};
use slop_ninja_detect_page::api::{UrlLine, UrlRequest};
use slop_ninja_detect_page::codes::Reputation;
use slop_ninja_detect_page::refusal::{ApiRefusal, RefusalCode};
use slop_ninja_detect_page::{Channel, RouteId, TicketClass};
use slop_ninja_detect_url::Policy;
use worker::{Env, Method, Request};

use crate::cf::{
    Answer, Client, Lines, Quiet, Reserved, Scan, object_call, package, prepare, read_switches, refusal, run, var,
    waits,
};
use crate::cf_host::SlotOut;
use crate::cf_io::{self, now_ms};
use crate::cf_routes::{ctx, forwarded, json_body, no_room, refused};
use crate::db::IoError;
use crate::doc::Serving;
use crate::net::{FetchApi, HostApi, Pinger, Screen, Slot};
use crate::url_flow::{self, Admitted, Scored, UrlCtx, UrlScorer};
use crate::{answer, handoff, routes, seal, switches};

/// The fetch Worker's binding.
pub(crate) const FETCH: &str = "DETECT_FETCH";
/// The `DetectHost` namespace.
pub(crate) const HOST: &str = "DETECT_HOST";
/// The fetch wait when `FETCH_WAIT_MS` is unset.
pub(crate) const FETCH_WAIT_DEFAULT_MS: u64 = 10_000;
/// A `/x/post` answer's bound: the post's text (X's 280 characters, at most
/// 4 bytes each) and its JSON.
const X_POST_ANSWER_BYTES: usize = 16 * 1024;

fn fetch_ms(env: &Env) -> u64 {
    crate::cf::ms_var(env, "FETCH_WAIT_MS").filter(|v| *v > 0).unwrap_or(FETCH_WAIT_DEFAULT_MS)
}

/// The fetch Worker.
pub(crate) struct CfFetch<'a> {
    pub(crate) env: &'a Env,
    pub(crate) call_ms: u64,
}

impl CfFetch<'_> {
    async fn call<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        bound: usize,
        wait: u64,
    ) -> Result<T, IoError> {
        let fetcher = self.env.service(FETCH).map_err(|_| IoError)?;
        let text = serde_json::to_string(body).map_err(|_| IoError)?;
        let mut req = cf_io::internal_request(&format!("{FETCH_ORIGIN}{path}"), Method::Post, Some(&text), Some(wait))
            .map_err(|_| IoError)?;
        let _ = req.headers_mut().map(|h| h.set("Content-Type", "application/json"));
        let mut res = fetcher.fetch_request(req).await.map_err(|_| IoError)?;
        if res.status_code() != 200 {
            cf_io::discard(&mut res);
            return Err(IoError);
        }
        cf_io::response_json(&mut res, bound).await.map_err(|_| IoError)
    }
}

impl FetchApi for CfFetch<'_> {
    async fn fetch(&self, req: &FetchRequest) -> Result<FetchAnswer, IoError> {
        // The answer carries the extracted text: bounded as a pasted text is.
        let bound = usize::try_from(crate::bound::RAW_BODY_BYTES).unwrap_or(usize::MAX);
        self.call(FETCH_PATH, req, bound, req.limits.wait_ms.saturating_add(self.call_ms)).await
    }

    async fn x_post(&self, req: &XPostRequest) -> Result<XPostAnswer, IoError> {
        self.call(X_POST_PATH, req, X_POST_ANSWER_BYTES, req.deadline_ms.saturating_add(self.call_ms)).await
    }

    async fn x_check(&self, req: &XCheckRequest) -> Result<XCheckAnswer, IoError> {
        // One round of controls, then a round per k ids, each at most the
        // deadline; the answer is a few bytes per id.
        let rounds = 1 + req.ids.len().div_ceil(X_CHECK_K) as u64;
        let wait = rounds.saturating_mul(req.deadline_ms).saturating_add(self.call_ms);
        self.call(X_CHECK_PATH, req, 1024 + 64 * req.ids.len(), wait).await
    }
}

/// The `DetectHost` objects.
pub(crate) struct CfHost<'a> {
    pub(crate) env: &'a Env,
    pub(crate) wait_ms: u64,
}

#[derive(Serialize)]
struct ScreenReq<'a> {
    host: &'a str,
}

#[derive(serde::Deserialize)]
struct ScreenAns {
    risk: bool,
}

#[derive(Serialize)]
struct SlotReq<'a> {
    url: &'a str,
    lease_ms: u64,
}

#[derive(Serialize)]
struct ReleaseReq<'a> {
    slot: &'a str,
    retry_after_ms: Option<u64>,
}

#[derive(Serialize)]
struct RepReq<'a> {
    domain: &'a str,
}

#[derive(serde::Deserialize)]
struct RepAns {
    reputation: Reputation,
    list_v: String,
}

impl HostApi for CfHost<'_> {
    async fn screen(&self, registrable: &str, host: &str) -> Result<Screen, IoError> {
        let a = object_call::<ScreenAns, _>(
            self.env,
            HOST,
            registrable,
            "/screen",
            Some(&ScreenReq { host }),
            self.wait_ms,
        )
        .await
        .flatten()
        .ok_or(IoError)?;
        Ok(if a.risk { Screen::SecurityRisk } else { Screen::Clear })
    }

    async fn slot(&self, registrable: &str, url: &str, lease_ms: u64) -> Result<Slot, IoError> {
        let a = object_call::<SlotOut, _>(
            self.env,
            HOST,
            registrable,
            "/slot",
            Some(&SlotReq { url, lease_ms }),
            self.wait_ms,
        )
        .await
        .flatten()
        .ok_or(IoError)?;
        a.slot().ok_or(IoError)
    }

    async fn release(&self, registrable: &str, slot: &str, retry_after_ms: Option<u64>) {
        let _ = object_call::<serde::de::IgnoredAny, _>(
            self.env,
            HOST,
            registrable,
            "/release",
            Some(&ReleaseReq { slot, retry_after_ms }),
            self.wait_ms,
        )
        .await;
    }

    async fn reputation(&self, registrable: &str) -> Result<(Reputation, String), IoError> {
        let a = object_call::<RepAns, _>(
            self.env,
            HOST,
            registrable,
            "/reputation",
            Some(&RepReq { domain: registrable }),
            self.wait_ms,
        )
        .await
        .flatten()
        .ok_or(IoError)?;
        Ok((a.reputation, a.list_v))
    }

    async fn wait_until(&self, t: u64) {
        let ms = t.saturating_sub(now_ms());
        if ms > 0 {
            worker::Delay::from(std::time::Duration::from_millis(ms)).await;
        }
    }
}

/// The scan of a URL text: the free check's own path, under ticket class
/// `url`, counted on the `free_url` channel.
pub(crate) struct CfUrlScorer<'a> {
    env: &'a Env,
    ip: Option<&'a str>,
    serving: &'a Serving,
}

impl UrlScorer for CfUrlScorer<'_> {
    async fn score(&self, canonical: &str) -> Result<Scored, ApiRefusal> {
        let (env, started) = (self.env, now_ms());
        let w = waits(env);
        let sw = read_switches(env, switches::SCORE, w.hold_ms).await;
        if !sw.on(switches::TOOL) {
            return Err(ApiRefusal::new(RefusalCode::ToolOff));
        }
        let pkg = package(env).ok_or_else(|| ApiRefusal::new(RefusalCode::NotConfigured))?;
        let reservation = routes::scan_footprint(Some(canonical.len() as u64));
        let _held = Reserved::take(reservation).map_err(|e| ApiRefusal::new(no_room(e)))?;
        let a = prepare(
            env,
            RouteId::Url,
            canonical,
            sw.on(switches::SPANS),
            pkg,
            Some(Channel::FreeUrl),
            reservation,
            started,
        )?;
        let client = Client { class: TicketClass::Url, ..Client::prefix(self.ip) };
        match run(env, &client, &mut Quiet, &w, &a).await {
            Scan::Result(mut line) => {
                // The text is canonical already: the offsets map to itself.
                answer::to_original(&mut line, canonical, canonical);
                let result =
                    handoff::stored_result(&line.result_full(), now_ms(), a.load(), seal::POLICY_V, self.serving)
                        .map_err(|_| ApiRefusal::new(RefusalCode::Internal))?;
                Ok(Scored { result, scored: a.scored().to_owned() })
            }
            Scan::Refused(r) => Err(r),
            Scan::Gone => Err(ApiRefusal::new(RefusalCode::Unavailable)),
        }
    }
}

/// IndexNow over the platform's fetch.
pub(crate) struct CfPinger {
    pub(crate) wait_ms: u64,
}

impl Pinger for CfPinger {
    async fn indexnow(&self, body: Vec<u8>) -> Result<(), IoError> {
        let text = String::from_utf8(body).map_err(|_| IoError)?;
        let mut req = cf_io::internal_request(crate::indexnow::ENDPOINT, Method::Post, Some(&text), Some(self.wait_ms))
            .map_err(|_| IoError)?;
        let _ = req.headers_mut().map(|h| h.set("Content-Type", "application/json; charset=utf-8"));
        let mut res = worker::Fetch::Request(req).send().await.map_err(|_| IoError)?;
        let ok = (200..300).contains(&res.status_code());
        cf_io::discard(&mut res);
        if ok { Ok(()) } else { Err(IoError) }
    }
}

/// The URL rules' policy: the configured origins, and the staging fixture
/// allowance when `URL_FIXTURE_PREFIX` holds one (staging only).
fn policy(env: &Env) -> Policy {
    let p = Policy::from_origins(var(env, "SITE_ORIGIN").as_deref(), var(env, "SECRET_ORIGIN").as_deref());
    match (var(env, "DEPLOY_ENV").as_deref(), var(env, "URL_FIXTURE_PREFIX")) {
        (Some("staging"), Some(f)) => match slop_ninja_detect_url::FixturePrefix::parse(&f) {
            Some(fx) => p.with_fixture(fx),
            None => p,
        },
        _ => p,
    }
}

/// `POST /api/detect/v1/url {url}`.
pub(crate) async fn url(mut req: Request, env: Env, answer: &Answer) {
    let route = RouteId::Url;
    if let Err(c) = forwarded(&req, &env, route) {
        return answer.send(refused(&env, route, c));
    }
    if !content_type_ok(crate::cf::header(&req, "content-type").as_deref(), BodyType::Json) {
        return answer.send(refused(&env, route, RefusalCode::ContentType));
    }
    let body = match json_body::<UrlRequest>(&mut req, &env, routes::SMALL_BODY_BYTES).await {
        Ok(b) => b,
        Err(c) => return answer.send(refused(&env, route, c)),
    };
    let c = match ctx(&env) {
        Ok(c) => c,
        Err(code) => return answer.send(refused(&env, route, code)),
    };
    let w = waits(&env);
    let fetch_wait = fetch_ms(&env);
    let policy = policy(&env);
    let uctx = UrlCtx {
        serving: &c.serving,
        origins: &c.origins,
        keys: &c.secrets.store,
        policy: &policy,
        fetch_ms: fetch_wait,
        lease_ms: fetch_wait.saturating_add(w.scan_lease_ms()).saturating_add(w.hold_ms),
        html_bound: 4 * 1024 * 1024,
    };
    let host = CfHost { env: &env, wait_ms: w.hold_ms.max(fetch_wait) };
    let admitted = match url_flow::admit(&c.io, &host, &uctx, &body.url).await {
        Ok(a) => a,
        Err(r) => {
            crate::cf::submitted(&env, route, r.code.as_str());
            return answer.send(refusal(&r));
        }
    };
    let Some(mut lines) = Lines::open(answer, w.hold_ms) else {
        return;
    };
    crate::cf::submitted(&env, route, "accepted");
    match admitted {
        Admitted::Done(slug) => {
            let _ = lines.line(&UrlLine::Done { slug }).await;
        }
        Admitted::Joined(slug) => {
            let _ = lines.line(&UrlLine::Pending { slug }).await;
        }
        Admitted::Drive(job) => {
            let _ = lines.line(&UrlLine::Pending { slug: job.held.slug.clone() }).await;
            let ip = crate::cf::header(&req, "cf-connecting-ip");
            let scorer = CfUrlScorer { env: &env, ip: ip.as_deref(), serving: &c.serving };
            let fetch = CfFetch { env: &env, call_ms: w.hold_ms };
            // The check runs to its end even when the reader leaves: the
            // pending page polls its state.
            let ended = url_flow::drive(&c.io, &host, &fetch, &scorer, &uctx, &job).await;
            match ended.failed {
                None => {
                    let _ = lines.line(&UrlLine::Done { slug: ended.slug }).await;
                }
                Some(code) => {
                    let _ = lines.line(&ApiRefusal::new(code)).await;
                }
            }
        }
    }
    lines.close().await;
}
