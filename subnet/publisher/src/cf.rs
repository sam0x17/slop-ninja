//! Workers entry points (wasm32 only): the one-minute cron, the R2 store
//! adapter, the status reads through service bindings, the alert sinks (an
//! https URL or the private alerts relay's service binding, with its
//! dead-man heartbeat), and the `PublisherState` Durable Object that holds
//! the alert state and the run lease. There is no fetch route: the public
//! host serves the bucket's objects directly.

use std::time::Duration;

use futures_util::StreamExt;
use futures_util::future::{Either, select};
use serde::{Deserialize, Serialize};
use worker::{
    Bucket, ByteStream, Data, Date, Delay, DurableObject, Env, Headers, HttpMetadata, Method,
    Request, RequestInit, Response, ResponseBody, Result, ScheduleContext, ScheduledEvent, State,
    console_error, console_log, durable_object, event,
};

use crate::alerts::{self, Posted, PublisherObs};
use crate::arena::{self, Budget};
use crate::config::{self, Config, Sink};
use crate::durable::{Begin, Begun, Durable, Lease, Save};
use crate::layout::{self, Cache};
use crate::publish::{Buckets, Keepalive, Report, Run};
use crate::status::{self, Observed, SnapshotInputs};
use crate::store::{ObjectMeta, PublicStore, Put, Store, StoreError};
use slop_ninja_alerts_wire as wire;
use slop_ninja_protocol::Digest;

const RECEIPTS: &str = "RECEIPTS_TESTNET";
/// The private admission bucket (the miners' direct uploads), read for
/// publication at scoring start.
const ADMISSION: &str = "ADMISSION_TESTNET";
const PUBLIC: &str = "PUBLIC_TESTNET";
/// The dashboard bucket (`dashboard/v1` only), read by the private dashboard
/// feed Worker. Optional: without the binding no dashboard is written.
const DASHBOARD: &str = "DASHBOARD_TESTNET";
const COORDINATOR_SERVICE: &str = "COORDINATOR_TESTNET";
const EVAL_SERVICE: &str = "EVAL_WORKER_TESTNET";
const STATE: &str = "PUBLISHER_STATE_TESTNET";
/// The private alerts relay Worker (decision D18), reached only through this
/// service binding.
const RELAY: &str = "ALERTS_RELAY_TESTNET";
/// The internal signer Worker's copy route (`ARTIFACT_COPY = "server"`),
/// reached only through this service binding with `COPY_INTERFACE_TOKEN`.
const SIGNER: &str = "R2_SIGNER_TESTNET";
/// The eval Worker's publication report route, on the internal status host.
const PUBLICATION_URL: &str = "https://status.internal/eval/admission/published";
const SIGNER_COPY_URL: &str = "https://r2-signer.internal/internal/r2-signer/v1/copy";
const SIGNER_TOKEN_HEADER: &str = "x-sn-interface-token";
const COPY_MEDIA_TYPE: &str = "application/vnd.slop-ninja.r2-copy.v1";

/// Each status read is answered within this or skipped for the run (the
/// coordinator's own liveness probe uses the same order of bound). The wire
/// crate's value, which the alerts relay reads with too.
const STATUS_TIMEOUT_MS: u64 = wire::STATUS_TIMEOUT_MS;
/// Alert and heartbeat requests (the relay answers within half of it).
const OUTBOUND_TIMEOUT_MS: u64 = wire::OUTBOUND_TIMEOUT_MS;
/// Two cron periods. A run renews the lease before every step (one cohort
/// verification, one artifact copy), so a run killed mid-step blocks the
/// next runs for two periods at most; a blocked run still refreshes the
/// snapshot. The wire crate's value: the relay's dead-man bound derives from
/// it.
const LEASE_MS: u64 = wire::PUBLISHER_LEASE_MS;

fn now_ms() -> u64 {
    Date::now().as_millis()
}

fn var_or_secret(env: &Env, name: &str) -> Option<String> {
    env.var(name)
        .ok()
        .map(|v| v.to_string())
        .or_else(|| env.secret(name).ok().map(|s| s.to_string()))
}

async fn within<T>(ms: u64, fut: impl std::future::Future<Output = T>) -> Option<T> {
    let fut = Box::pin(fut);
    let timeout = Box::pin(Delay::from(Duration::from_millis(ms)));
    match select(fut, timeout).await {
        Either::Left((value, _)) => Some(value),
        Either::Right(_) => None,
    }
}

// ------------------------------------------------------------------ R2

struct R2(Bucket);

fn meta(o: &worker::Object) -> ObjectMeta {
    ObjectMeta {
        size: o.size(),
        sha256: o
            .checksum()
            .sha256
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .map(Digest),
        etag: Some(o.etag()),
    }
}

fn http(put: &Put<'_>) -> HttpMetadata {
    HttpMetadata {
        content_type: Some(put.content_type.to_owned()),
        content_language: None,
        content_disposition: None,
        content_encoding: None,
        cache_control: Some(put.cache.header()),
        cache_expiry: None,
    }
}

fn copy_error(e: worker::Error) -> StoreError {
    let text = e.to_string().to_ascii_lowercase();
    if text.contains("checksum")
        || text.contains("digest")
        || text.contains("sha-256")
        || text.contains("sha256")
    {
        StoreError::Integrity
    } else {
        StoreError::Unavailable("r2 copy")
    }
}

impl Store for R2 {
    async fn head(&self, key: &str) -> std::result::Result<Option<ObjectMeta>, StoreError> {
        self.0
            .head(key)
            .await
            .map(|o| o.as_ref().map(meta))
            .map_err(|_| StoreError::Unavailable("r2 head"))
    }

    async fn get(&self, key: &str, bound: u64) -> std::result::Result<Option<Vec<u8>>, StoreError> {
        // The size is known before the body is read: refuse first.
        let Some(object) = self
            .0
            .get(key)
            .execute()
            .await
            .map_err(|_| StoreError::Unavailable("r2 get"))?
        else {
            return Ok(None);
        };
        let size = object.size();
        if size > bound {
            return Err(StoreError::TooLarge { size, bound });
        }
        let body = object.body().ok_or(StoreError::Unavailable("r2 body"))?;
        let bytes = body
            .bytes()
            .await
            .map_err(|_| StoreError::Unavailable("r2 read"))?;
        if bytes.len() as u64 != size {
            return Err(StoreError::Short);
        }
        Ok(Some(bytes))
    }

    async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        put: &Put<'_>,
    ) -> std::result::Result<(), StoreError> {
        let mut builder = self.0.put(key, bytes).http_metadata(http(put));
        if let Some(d) = put.sha256 {
            builder = builder.sha256(d.0.to_vec());
        }
        builder.execute().await.map(|_| ()).map_err(copy_error)
    }

    async fn copy_from(
        &self,
        src: &Self,
        src_key: &str,
        dst_key: &str,
        len: u64,
        put: &Put<'_>,
    ) -> std::result::Result<bool, StoreError> {
        let Some(object) = src
            .0
            .get(src_key)
            .execute()
            .await
            .map_err(|_| StoreError::Unavailable("r2 get"))?
        else {
            return Ok(false);
        };
        if object.size() != len {
            return Err(StoreError::Integrity);
        }
        let body = object.body().ok_or(StoreError::Unavailable("r2 body"))?;
        // The body streams from bucket to bucket inside the runtime; it never
        // enters Wasm memory. R2 checks the received bytes against the digest.
        let ResponseBody::Stream(stream) = body
            .response_body()
            .map_err(|_| StoreError::Unavailable("r2 body"))?
        else {
            return Err(StoreError::Unavailable("r2 body"));
        };
        let mut builder = self
            .0
            .put(dst_key, Data::ReadableStream(stream))
            .http_metadata(http(put));
        if let Some(d) = put.sha256 {
            builder = builder.sha256(d.0.to_vec());
        }
        builder.execute().await.map(|_| true).map_err(copy_error)
    }

    async fn list_dirs(&self, prefix: &str) -> std::result::Result<Vec<String>, StoreError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut list = self.0.list().prefix(prefix).delimiter("/");
            if let Some(c) = &cursor {
                list = list.cursor(c.clone());
            }
            let page = list
                .execute()
                .await
                .map_err(|_| StoreError::Unavailable("r2 list"))?;
            out.extend(page.delimited_prefixes());
            match (page.truncated(), page.cursor()) {
                (true, Some(c)) => cursor = Some(c),
                _ => break,
            }
        }
        Ok(out)
    }

    async fn list_keys(
        &self,
        prefix: &str,
        limit: usize,
    ) -> std::result::Result<Vec<String>, StoreError> {
        let limit = u32::try_from(limit).map_err(|_| StoreError::Unavailable("r2 list limit"))?;
        let page = self
            .0
            .list()
            .prefix(prefix)
            .limit(limit)
            .execute()
            .await
            .map_err(|_| StoreError::Unavailable("r2 list"))?;
        Ok(page.objects().iter().map(|o| o.key()).collect())
    }

    async fn delete(&self, key: &str) -> std::result::Result<(), StoreError> {
        self.0
            .delete(key)
            .await
            .map_err(|_| StoreError::Unavailable("r2 delete"))
    }
}

// ---------------------------------------------------- publication report

/// The eval Worker's publication report route (`POST
/// /eval/admission/published` on the internal status host, with the
/// publisher's own eval status token): one small JSON body per cohort and
/// run, bounded by the outbound timeout. Only a 200 is taken: the eval
/// Worker answers 404 when the authority the label names holds no such
/// cohort.
struct EvalPublication {
    fetcher: worker::Fetcher,
    token: String,
}

impl crate::artifacts::PublicationSink for EvalPublication {
    fn report<'a>(
        &'a self,
        cohort: u64,
        label: &'a str,
        entries: &'a [crate::artifacts::ReportEntry],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<(), StoreError>> + 'a>,
    > {
        Box::pin(async move {
            let body = serde_json::to_vec(
                &serde_json::json!({"cohort": cohort, "label": label, "entrants": entries}),
            )
                .map_err(|_| StoreError::Unavailable("publication report"))?;
            let headers = Headers::new();
            headers
                .set("authorization", &format!("Bearer {}", self.token))
                .map_err(|_| StoreError::Unavailable("publication report"))?;
            headers
                .set("content-type", "application/json")
                .map_err(|_| StoreError::Unavailable("publication report"))?;
            let mut init = RequestInit::new();
            let array = js_sys::Uint8Array::from(body.as_slice());
            init.with_method(Method::Post)
                .with_headers(headers)
                .with_body(Some(array.into()));
            let ok = within(STATUS_TIMEOUT_MS, async {
                let request = Request::new_with_init(PUBLICATION_URL, &init).ok()?;
                let mut response = self.fetcher.fetch_request(request).await.ok()?;
                let ok = response.status_code() == 200;
                let _ = response.bytes().await;
                Some(ok)
            })
            .await
            .flatten()
            .unwrap_or(false);
            if ok { Ok(()) } else { Err(StoreError::Unavailable("publication report")) }
        })
    }
}

// ----------------------------------------------------------- server copy

/// The signer's copy route: one fixed-size request, one presigned
/// `CopyObject` back, which is sent to R2 with no body and exactly the
/// signed headers. The bytes are copied inside R2; none passes through this
/// Worker. The signer request is bounded by the outbound timeout; the copy
/// by the wait derived from the object's length (`copy_wait`), since R2
/// answers only once the copy is done. At the wait the copy request is
/// aborted, which closes its connection, and R2 abandons a copy whose caller
/// disconnected (probe G7-x X11).
struct SignerCopy {
    fetcher: worker::Fetcher,
    token: String,
    /// The admission bucket's name (`ADMISSION_BUCKET_NAME`, the signer's
    /// value): `x-amz-copy-source` names it.
    source_bucket: String,
}

impl crate::artifacts::ServerCopy for SignerCopy {
    fn copy<'a>(
        &'a self,
        spec: slop_ninja_r2_presign::copy::CopySpec,
        wait_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<(), StoreError>> + 'a>,
    > {
        Box::pin(async move {
            use slop_ninja_r2_presign::{copy, wire};
            let req = wire::encode_copy_request(&spec, slop_ninja_r2_presign::policy::SKEW_SECS as u32);
            let headers = Headers::new();
            headers
                .set(SIGNER_TOKEN_HEADER, &self.token)
                .map_err(|_| StoreError::Unavailable("signer headers"))?;
            headers
                .set("content-type", COPY_MEDIA_TYPE)
                .map_err(|_| StoreError::Unavailable("signer headers"))?;
            let mut init = RequestInit::new();
            let body = js_sys::Uint8Array::from(req.as_slice());
            init.with_method(Method::Post)
                .with_headers(headers)
                .with_body(Some(body.into()));
            let reply = within(OUTBOUND_TIMEOUT_MS, async {
                let request = Request::new_with_init(SIGNER_COPY_URL, &init).ok()?;
                let mut response = self.fetcher.fetch_request(request).await.ok()?;
                if response.status_code() != 200 {
                    return None;
                }
                let bytes = response.bytes().await.ok()?;
                (bytes.len() <= wire::COPY_REPLY_MAX).then_some(bytes)
            })
            .await
            .flatten()
            .ok_or(StoreError::Unavailable("signer copy"))?;
            let r = wire::decode_copy_reply(&reply).ok_or(StoreError::Unavailable("signer reply"))?;
            if r.status != wire::Status::Ok {
                return Err(StoreError::Unavailable("signer refused the copy"));
            }
            // The headers are the signed values, derived here from the same
            // spec; the signer's URL carries only the signature.
            let put_headers = Headers::new();
            let (src, ifm) = (spec.copy_source(&self.source_bucket), spec.if_match());
            for (name, value) in copy::copy_headers(src.as_str(), ifm.as_str()) {
                put_headers
                    .set(name, value)
                    .map_err(|_| StoreError::Unavailable("copy headers"))?;
            }
            let mut put = RequestInit::new();
            put.with_method(Method::Put).with_headers(put_headers);
            // The platform's own timeout (`AbortSignal.timeout`) bounds the
            // copy: at the wait the fetch is aborted and its connection
            // closed, so R2 abandons the copy (probe G7-x X11) and no pending
            // future is dropped (docs/workers-arena-design.md section 2.6).
            let signal = worker::AbortSignal::from(
                worker::worker_sys::web_sys::AbortSignal::timeout_with_u32(
                    u32::try_from(wait_ms).unwrap_or(u32::MAX),
                ),
            );
            let request = Request::new_with_init(r.url, &put)
                .map_err(|_| StoreError::Unavailable("copy request"))?;
            let sent = worker::Fetch::Request(request).send_with_signal(&signal).await;
            // A copy sent but not confirmed: the caller reads the
            // destination first, then copies by stream
            // (`artifacts::copy_object`).
            crate::artifacts::copy_answer(sent.ok().map(|r| r.status_code()), signal.aborted())
        })
    }
}

// --------------------------------------------------------------- status

/// Reads a response body within the status bound the budget leaves
/// ([`arena::status_body_bound`]), reserving it while held, and parses it
/// into a typed view. A larger body is refused while it streams in, or from
/// its declared length before any byte is read.
async fn bounded_parse<T>(
    mut response: Response,
    budget: &Budget,
    parse: impl Fn(&[u8]) -> Option<T>,
) -> Option<T> {
    let declared = declared_length(response.headers());
    let stream = response.stream().ok()?;
    bounded_read(declared, stream, budget, |b| parse(b).ok_or(()))
        .await
        .ok()
}

fn declared_length(headers: &Headers) -> Option<u64> {
    headers
        .get("content-length")
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
}

/// Why a body was not read: each reason is short and holds none of the
/// body's content, so it can be logged.
#[derive(Debug)]
enum BodyRefused<E> {
    /// The declared length exceeds the status bound.
    Declared(u64),
    /// The budget could not hold the body's reservation.
    Budget(arena::Refused),
    Stream,
    /// The stream ran past this many bytes (the declared length, or the
    /// status bound).
    Longer(u64),
    Parse(E),
}

impl<E: std::fmt::Display> std::fmt::Display for BodyRefused<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Declared(n) => write!(f, "declared length {n} exceeds the bound"),
            Self::Budget(r) => write!(
                f,
                "reservation of {} refused ({} of {} held)",
                r.want, r.held, r.limit
            ),
            Self::Stream => f.write_str("body stream failed"),
            Self::Longer(n) => write!(f, "body longer than {n} bytes"),
            Self::Parse(e) => write!(f, "parse: {e}"),
        }
    }
}

/// Reads a body by its plan ([`arena::body_plan`]), reserving it while
/// held, and parses it.
async fn bounded_read<T, E>(
    declared: Option<u64>,
    mut stream: ByteStream,
    budget: &Budget,
    parse: impl Fn(&[u8]) -> std::result::Result<T, E>,
) -> std::result::Result<T, BodyRefused<E>> {
    let plan = arena::body_plan(declared, budget.remaining())
        .ok_or(BodyRefused::Declared(declared.unwrap_or(0)))?;
    let _hold = budget.reserve(plan.reserve).map_err(BodyRefused::Budget)?;
    let mut out = Vec::with_capacity(plan.capacity as usize);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| BodyRefused::Stream)?;
        if (out.len() + chunk.len()) as u64 > plan.limit {
            return Err(BodyRefused::Longer(plan.limit));
        }
        out.extend_from_slice(&chunk);
    }
    parse(&out).map_err(BodyRefused::Parse)
}

/// A state call's body, read like a status body within the state object's
/// one budget (its fetches interleave at await points, so bodies read at
/// once share it) and parsed with `serde_json`, the encoding the run wrote
/// it in ([`crate::durable::parse_body`]).
async fn state_body<T: for<'d> Deserialize<'d>>(
    req: &mut Request,
    budget: &Budget,
) -> std::result::Result<T, BodyRefused<serde_json::Error>> {
    let declared = declared_length(req.headers());
    let stream = req.stream().map_err(|_| BodyRefused::Stream)?;
    bounded_read(declared, stream, budget, crate::durable::parse_body).await
}

async fn status_read<T>(
    env: &Env,
    service: &str,
    path: &str,
    token: &str,
    budget: &Budget,
    parse: impl Fn(&[u8]) -> Option<T>,
) -> Option<T> {
    let fetcher = env.service(service).ok()?;
    let headers = Headers::new();
    headers
        .set("authorization", &format!("Bearer {token}"))
        .ok()?;
    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let url = format!("https://status.internal{path}");
    within(STATUS_TIMEOUT_MS, async {
        let request = Request::new_with_init(&url, &init).ok()?;
        let response = fetcher.fetch_request(request).await.ok()?;
        if response.status_code() != 200 {
            return None;
        }
        bounded_parse(response, budget, &parse).await
    })
    .await
    .flatten()
}

/// Each Worker checks its own status token, so the coordinator's and the
/// eval Worker's are separate secrets. A missing token leaves that Worker's
/// reads unanswered (the status-unreachable alert), never borrowed from the
/// other.
async fn observe(env: &Env, budget: &Budget) -> Observed {
    let w = status::parse_worker_bytes;
    let mut observed = Observed::default();
    if let Some(token) = var_or_secret(env, "COORDINATOR_STATUS_TOKEN") {
        observed.coordinator =
            status_read(env, COORDINATOR_SERVICE, "/status", &token, budget, w).await;
    }
    if let Some(token) = var_or_secret(env, "EVAL_STATUS_TOKEN") {
        observed.eval_worker = status_read(env, EVAL_SERVICE, "/status", &token, budget, w).await;
        observed.eval = status_read(
            env,
            EVAL_SERVICE,
            "/eval/status",
            &token,
            budget,
            status::parse_eval_bytes,
        )
        .await;
    }
    observed
}

async fn post(url: &str, body: Option<Vec<u8>>) -> Posted {
    let headers = Headers::new();
    let mut init = RequestInit::new();
    match body {
        Some(b) => {
            let _ = headers.set("content-type", "application/json");
            let array = js_sys::Uint8Array::from(b.as_slice());
            init.with_method(Method::Post).with_body(Some(array.into()));
        }
        None => {
            init.with_method(Method::Get);
        }
    }
    init.with_headers(headers);
    let answered = within(OUTBOUND_TIMEOUT_MS, async {
        let request = Request::new_with_init(url, &init).ok()?;
        let response = worker::Fetch::Request(request).send().await.ok()?;
        Some(response.status_code())
    })
    .await
    .flatten();
    posted(answered)
}

fn posted(status: Option<u16>) -> Posted {
    match status {
        Some(200..=299) => Posted::Accepted,
        Some(_) => Posted::Refused,
        None => Posted::NoAnswer,
    }
}

/// One post to the relay through its service binding: accepted on a 2xx (the
/// relay sent the email, or had already sent every event in the post).
async fn relay_post(env: &Env, path: &str, body: Vec<u8>) -> Posted {
    let Ok(fetcher) = env.service(RELAY) else {
        return Posted::Refused;
    };
    let headers = Headers::new();
    let _ = headers.set("content-type", "application/json");
    let mut init = RequestInit::new();
    let array = js_sys::Uint8Array::from(body.as_slice());
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(array.into()));
    let url = format!("https://alerts-relay.internal{path}");
    let answered = within(OUTBOUND_TIMEOUT_MS, async {
        let request = Request::new_with_init(&url, &init).ok()?;
        let response = fetcher.fetch_request(request).await.ok()?;
        Some(response.status_code())
    })
    .await
    .flatten();
    posted(answered)
}

/// An https sink.
struct UrlPoster<'a>(&'a str);

impl alerts::Poster for UrlPoster<'_> {
    async fn post(&self, body: Vec<u8>) -> Posted {
        post(self.0, Some(body)).await
    }
}

/// The alerts relay's `/alerts` route.
struct RelayPoster<'a>(&'a Env);

impl alerts::Poster for RelayPoster<'_> {
    async fn post(&self, body: Vec<u8>) -> Posted {
        relay_post(self.0, wire::ALERTS_PATH, body).await
    }
    fn ledger(&self) -> bool {
        true
    }
}

/// The relay's dead-man heartbeat for a run whose alert phase finished,
/// with how its posts fared.
fn heartbeat(
    run_started_ms: u64,
    rules: &alerts::Rules,
    config_refused: bool,
    durable: &Durable,
) -> wire::Heartbeat {
    wire::Heartbeat {
        schema: wire::HEARTBEAT_SCHEMA.into(),
        network: "testnet".into(),
        rules: alerts::RULES_ID.into(),
        post_schema: wire::POST_SCHEMA.into(),
        run_started_ms,
        renotify_ms: rules.renotify_ms,
        watermark_stall_ms: rules.watermark_stall_ms,
        config_refused,
        undelivered: durable.undelivered,
        undelivered_since_ms: durable.undelivered_since_ms,
    }
}

async fn relay_heartbeat(env: Env, beat: wire::Heartbeat) {
    if relay_post(&env, wire::HEARTBEAT_PATH, beat.body()).await != Posted::Accepted {
        console_error!("publisher: relay heartbeat not accepted");
    }
}

/// The run that posts alerts: its lease holder, for the save before posting.
struct AlertRun<'a> {
    holder: &'a str,
    sink: Option<&'a Sink>,
}

/// Evaluates the rules and posts what is due, one post per batch within the
/// sink's limit; each delivered batch is acknowledged on its own. Records in
/// `durable` how many due events stayed undelivered, and since which run.
async fn alert(
    env: &Env,
    durable: &mut Durable,
    observed: &Observed,
    publisher: &PublisherObs,
    rules: &alerts::Rules,
    run: AlertRun<'_>,
    run_started_ms: u64,
) {
    let left = deliver_due(env, durable, observed, publisher, rules, run).await;
    durable.undelivered = left as u64;
    durable.undelivered_since_ms = if left > 0 {
        Some(durable.undelivered_since_ms.unwrap_or(run_started_ms))
    } else {
        None
    };
}

/// Saves the observed state, with the marks of the posts about to go out,
/// before any of them is sent: the event ids derive from it (each entry's
/// first-seen time), so a run that dies after posting, or whose save after
/// posting fails, leaves the next run the same ids, and the relay emails
/// them once. Nothing is posted when this save fails.
async fn save_then_deliver(
    env: &Env,
    durable: &mut Durable,
    events: &[alerts::Event],
    format: config::SinkFormat,
    holder: &str,
    now: u64,
    poster: &impl alerts::Poster,
) -> usize {
    alerts::mark(&mut durable.alerts, events, now, poster.ledger());
    let saved: Option<()> = state_call(
        env,
        "/checkpoint",
        &Save {
            holder: holder.to_owned(),
            now_ms: now_ms(),
            state: durable.clone(),
        },
    )
    .await;
    if saved.is_none() {
        console_error!("publisher: alert state not saved; nothing posted this run");
        return events.len();
    }
    alerts::deliver(&mut durable.alerts, events, format, "testnet", now, poster).await
}

async fn deliver_due(
    env: &Env,
    durable: &mut Durable,
    observed: &Observed,
    publisher: &PublisherObs,
    rules: &alerts::Rules,
    run: AlertRun<'_>,
) -> usize {
    let now = now_ms();
    // An unanswered status read carries the open entries it would decide:
    // unknown is never a resolution.
    let conditions = alerts::conditions(observed, publisher, rules, &durable.alerts, now);
    alerts::observe(&mut durable.alerts, &conditions, now);
    let events = alerts::due(&durable.alerts, now, rules.renotify_ms);
    if events.is_empty() {
        return 0;
    }
    let holder = run.holder;
    let left = match run.sink {
        Some(Sink::Url { url, format }) => {
            let p = UrlPoster(url);
            save_then_deliver(env, durable, &events, *format, holder, now, &p).await
        }
        Some(Sink::Relay) => {
            let format = config::SinkFormat::Json;
            let p = RelayPoster(env);
            save_then_deliver(env, durable, &events, format, holder, now, &p).await
        }
        None => {
            let state = &mut durable.alerts;
            for e in &events {
                console_log!(
                    "publisher alert {:?} {} {}: {}",
                    e.state,
                    e.rule,
                    e.subject,
                    e.detail
                );
            }
            alerts::acknowledge(state, &events, now);
            0
        }
    };
    if left > 0 {
        console_error!("publisher: {left} alert events not delivered; retried next run");
    }
    left
}

// ------------------------------------------------------- state and lease

/// Holds the alert state and the publisher's memo, and serializes runs: a
/// run works only while it holds the lease, which it renews between steps,
/// so overlapping cron invocations never publish or post twice, and a run
/// killed mid-step blocks the next ones for one lease at most.
#[durable_object(fetch)]
pub struct PublisherState {
    state: State,
    /// Accounts every state call body held at once ([`state_body`]).
    budget: Budget,
}

impl DurableObject for PublisherState {
    fn new(state: State, _env: Env) -> Self {
        Self {
            state,
            budget: Budget::worker(),
        }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let storage = self.state.storage();
        let lease: Option<Lease> = storage.get("lease").await?;
        match (req.method(), req.path().as_str()) {
            (Method::Post, "/begin") => {
                let b: Begin = match state_body(&mut req, &self.budget).await {
                    Ok(b) => b,
                    Err(why) => return refuse_body(&req.path(), &why),
                };
                if lease
                    .as_ref()
                    .is_some_and(|l| l.holder != b.holder && l.until_ms > b.now_ms)
                {
                    return Response::error("lease_held", 409);
                }
                let interrupted = lease.is_some();
                storage
                    .put(
                        "lease",
                        Lease {
                            holder: b.holder,
                            until_ms: b.now_ms + LEASE_MS,
                        },
                    )
                    .await?;
                let state: Durable = storage.get("state").await?.unwrap_or_default();
                Response::from_json(&Begun { state, interrupted })
            }
            (Method::Post, "/renew") => {
                let b: Begin = match state_body(&mut req, &self.budget).await {
                    Ok(b) => b,
                    Err(why) => return refuse_body(&req.path(), &why),
                };
                if lease.as_ref().is_none_or(|l| l.holder != b.holder) {
                    return Response::error("lease_lost", 409);
                }
                storage
                    .put(
                        "lease",
                        Lease {
                            holder: b.holder,
                            until_ms: b.now_ms + LEASE_MS,
                        },
                    )
                    .await?;
                Response::ok("")
            }
            (Method::Post, path @ ("/checkpoint" | "/end")) => {
                let e: Save = match state_body(&mut req, &self.budget).await {
                    Ok(e) => e,
                    Err(why) => return refuse_body(path, &why),
                };
                if lease.as_ref().is_none_or(|l| l.holder != e.holder) {
                    return Response::error("lease_lost", 409);
                }
                storage.put("state", e.state).await?;
                if path == "/end" {
                    storage.delete("lease").await?;
                } else {
                    storage
                        .put(
                            "lease",
                            Lease {
                                holder: e.holder,
                                until_ms: e.now_ms + LEASE_MS,
                            },
                        )
                        .await?;
                }
                Response::ok("")
            }
            _ => Response::error("not_found", 404),
        }
    }
}

/// One call to the state object: `Some` with the parsed answer on 200
/// (`/begin`), `Some(())`-like `null` for the others.
/// Logs why a state call's body was refused and answers 400.
fn refuse_body(path: &str, why: &BodyRefused<serde_json::Error>) -> Result<Response> {
    console_error!("publisher state: {path} body refused: {why}");
    Response::error("bad_body", 400)
}

async fn state_call<T: Serialize, R: for<'d> Deserialize<'d>>(
    env: &Env,
    path: &str,
    body: &T,
) -> Option<R> {
    let ns = env.durable_object(STATE).ok()?;
    let stub = ns.id_from_name("publisher/testnet").ok()?.get_stub().ok()?;
    let mut init = RequestInit::new();
    let text = serde_json::to_string(body).ok()?;
    init.with_method(Method::Post).with_body(Some(text.into()));
    let request =
        Request::new_with_init(&format!("https://publisher.internal{path}"), &init).ok()?;
    let mut response = stub.fetch_with_request(request).await.ok()?;
    if response.status_code() != 200 {
        return None;
    }
    if path != "/begin" {
        return serde_json::from_str("null").ok();
    }
    response.json().await.ok()
}

struct DoLease<'a> {
    env: &'a Env,
    holder: String,
}

impl Keepalive for DoLease<'_> {
    async fn renew(&self) -> bool {
        let r: Option<()> = state_call(
            self.env,
            "/renew",
            &Begin {
                holder: self.holder.clone(),
                now_ms: now_ms(),
            },
        )
        .await;
        r.is_some()
    }
}

async fn put_status(public: PublicStore<'_, R2>, snapshot: &status::PublicStatus) -> bool {
    match serde_json::to_vec_pretty(snapshot) {
        // The public store checks it against the allow-list like every
        // public write.
        Ok(bytes) => public
            .put(
                layout::STATUS,
                bytes,
                &Put {
                    content_type: "application/json",
                    cache: Cache::Snapshot,
                    sha256: None,
                },
            )
            .await
            .is_ok(),
        Err(_) => false,
    }
}

// ------------------------------------------------------------------ cron

/// A refused configuration still leaves a trace: a fault snapshot on the
/// public host and a `config_refused` alert through whatever sink parses.
/// The status is still read, and the last publication pass's conditions are
/// still raised, so no other alert resolves because of the refusal.
async fn config_fault(env: &Env, error: config::ConfigError) {
    console_error!("publisher: configuration refused: {error}");
    let started = now_ms();
    let budget = Budget::worker();
    let observed = observe(env, &budget).await;
    let holder = format!("config:{started}");
    let begin = Begin {
        holder: holder.clone(),
        now_ms: started,
    };
    let begun = state_call::<_, Begun>(env, "/begin", &begin).await;
    if let (Ok(public), Ok((_, netuid))) = (env.bucket(PUBLIC), config::trust()) {
        let chain = slop_ninja_protocol::ChainBinding {
            genesis: slop_ninja_protocol::TESTNET_GENESIS,
            netuid,
            mechanism: 0,
        };
        let mut snapshot = status::snapshot(&SnapshotInputs {
            chain: &chain,
            observed: &observed,
            admission_origin: None,
            operator_notice: Some(status::CONFIG_NOTICE),
            last_published_cohort: begun.as_ref().and_then(|b| b.state.last_published),
            now_ms: started,
        });
        snapshot.fault = true;
        put_status(PublicStore::new(&R2(public)), &snapshot).await;
    }
    let Some(Begun {
        state: mut durable, ..
    }) = begun
    else {
        return;
    };
    let publisher = PublisherObs {
        config: Some(error.to_string()),
        ..durable.publisher.clone()
    };
    let sink = config::sink_only(|name| var_or_secret(env, name));
    let rules = config::default_rules();
    let run = AlertRun {
        holder: &holder,
        sink: sink.as_ref(),
    };
    alert(
        env,
        &mut durable,
        &observed,
        &publisher,
        &rules,
        run,
        started,
    )
    .await;
    let beat = heartbeat(started, &rules, true, &durable);
    let ended: Option<()> = state_call(
        env,
        "/end",
        &Save {
            holder,
            now_ms: now_ms(),
            state: durable,
        },
    )
    .await;
    // The alert phase finished (the refusal itself is alerted): heartbeat.
    if ended.is_some() && sink == Some(Sink::Relay) {
        relay_heartbeat(env.clone(), beat).await;
    }
}

#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, ctx: ScheduleContext) {
    let started = now_ms();
    let cfg: Config = match config::parse(|name| var_or_secret(&env, name)) {
        Ok(c) => c,
        Err(e) => return config_fault(&env, e).await,
    };
    let (Ok(receipts), Ok(admission), Ok(public)) =
        (env.bucket(RECEIPTS), env.bucket(ADMISSION), env.bucket(PUBLIC))
    else {
        console_error!("publisher: a bucket binding is missing");
        return;
    };
    let (receipts, admission, public_bucket) = (R2(receipts), R2(admission), R2(public));
    let server_copy = match (
        env.service(SIGNER),
        // A secret only, never a var: a mis-deployed var would show the
        // token in the dashboard's variables.
        env.secret("COPY_INTERFACE_TOKEN").ok().map(|s| s.to_string()),
        var_or_secret(&env, "ADMISSION_BUCKET_NAME"),
    ) {
        (Ok(fetcher), Some(token), Some(source_bucket)) => Some(SignerCopy {
            fetcher,
            token,
            source_bucket: source_bucket.trim().to_owned(),
        }),
        _ => None,
    };
    let publication = match (env.service(EVAL_SERVICE), var_or_secret(&env, "EVAL_STATUS_TOKEN")) {
        (Ok(fetcher), Some(token)) => Some(EvalPublication { fetcher, token }),
        _ => None,
    };
    // The public bucket is written only through the allow-list.
    let public = PublicStore::new(&public_bucket);
    let budget = Budget::worker();
    let holder = format!("run:{started}");
    let begin = Begin {
        holder: holder.clone(),
        now_ms: started,
    };
    let Some(Begun {
        state: mut durable,
        interrupted,
    }) = state_call::<_, Begun>(&env, "/begin", &begin).await
    else {
        // Another run holds the lease: keep the snapshot fresh, nothing else.
        let observed = observe(&env, &budget).await;
        let last = match public.get(layout::COHORT_INDEX, budget.remaining()).await {
            Ok(Some(bytes)) => serde_json::from_slice::<crate::index::CohortIndex>(&bytes)
                .ok()
                .and_then(|i| i.last_cohort()),
            _ => None,
        };
        let snapshot = status::snapshot(&SnapshotInputs {
            chain: &cfg.chain,
            observed: &observed,
            admission_origin: cfg.admission_origin.as_deref(),
            operator_notice: cfg.notice.as_deref(),
            last_published_cohort: last,
            now_ms: now_ms(),
        });
        put_status(public, &snapshot).await;
        console_log!("publisher: another run holds the lease; snapshot refreshed");
        return;
    };

    // Phase 1, before any cohort work: the snapshot and the alerts, so a run
    // that dies later still leaves both current.
    let observed = observe(&env, &budget).await;
    let snapshot = status::snapshot(&SnapshotInputs {
        chain: &cfg.chain,
        observed: &observed,
        admission_origin: cfg.admission_origin.as_deref(),
        operator_notice: cfg.notice.as_deref(),
        last_published_cohort: durable.last_published,
        now_ms: now_ms(),
    });
    let status_written = put_status(public, &snapshot).await;
    let mut publisher = durable.publisher.clone();
    publisher.interrupted = interrupted;
    publisher.warnings.clone_from(&cfg.warnings);
    if !status_written {
        publisher
            .refused
            .push(("status".into(), "status snapshot not written".into()));
    }
    let run = AlertRun {
        holder: &holder,
        sink: cfg.sink.as_ref(),
    };
    alert(
        &env,
        &mut durable,
        &observed,
        &publisher,
        &cfg.rules,
        run,
        started,
    )
    .await;
    let saved: Option<()> = state_call(
        &env,
        "/checkpoint",
        &Save {
            holder: holder.clone(),
            now_ms: now_ms(),
            state: durable.clone(),
        },
    )
    .await;
    if saved.is_none() {
        console_log!("publisher: lease lost after the snapshot");
        return;
    }
    // The relay's dead-man heartbeat: the alert phase finished and its state
    // is saved (a run blocked by a held lease, or one whose state object did
    // not answer, sends none). It runs beside phase 2 (kept alive by
    // wait_until), so a slow relay never takes phase 2's wall budget.
    if cfg.sink == Some(Sink::Relay) {
        let beat = heartbeat(started, &cfg.rules, false, &durable);
        ctx.wait_until(relay_heartbeat(env.clone(), beat));
    }

    // Phase 2: publication, step by step under the renewed lease.
    let elapsed = move || now_ms().saturating_sub(started);
    let lease = DoLease {
        env: &env,
        holder: holder.clone(),
    };
    let run = Run {
        buckets: Buckets {
            receipts: &receipts,
            admission: &admission,
            public,
            server_copy: server_copy
                .as_ref()
                .map(|c| c as &dyn crate::artifacts::ServerCopy),
            publication: publication
                .as_ref()
                .map(|p| p as &dyn crate::artifacts::PublicationSink),
        },
        cfg: &cfg,
        budget: &budget,
        elapsed_ms: &elapsed,
        keepalive: &lease,
        stopped: std::cell::Cell::new(false),
    };
    let mut report = Report::default();
    let index = match run.cohorts(&mut durable.memo, &mut report).await {
        Ok(i) => Some(i),
        Err(e) => {
            report.refused.push(("publisher".into(), e));
            None
        }
    };
    durable.publisher = PublisherObs {
        refused: report.refused.clone(),
        pending: report.pending.clone(),
        unverifiable: report.unverifiable.clone(),
        revocations_unopened: report.revocations_unopened,
        config: None,
        interrupted: false,
        warnings: Vec::new(),
        retention_mismatch: crate::alerts::retention_after_pass(
            &durable.publisher.retention_mismatch,
            report
                .retention_checked
                .then_some(report.retention_mismatch.as_slice()),
        ),
    };
    if let Some(i) = &index {
        durable.last_published = i.last_cohort();
    }
    // Phase 3: the dashboard documents, from the status read in phase 1 and
    // the index phase 2 left. A refusal is alerted like phase 2's.
    if let Ok(bucket) = env.bucket(DASHBOARD) {
        let dash = R2(bucket);
        let out = crate::dashboard::DashboardStore::new(&dash);
        let inputs = crate::dashboard::RunInputs {
            settings: crate::dashboard::Settings {
                chain: &cfg.chain,
                tempo_blocks: cfg.tempo_blocks,
                block_secs: cfg.block_secs,
                silent_ms: cfg.rules.watermark_stall_ms,
                operator_notice: cfg.notice.is_some(),
                record_prefix: cfg.record_label_prefix.as_deref(),
                publish_from_epoch: cfg.publish_from_epoch,
            },
            observed: &observed,
            index: index.as_ref(),
            now_ms: now_ms(),
            budget: &budget,
        };
        let budget_ms = cfg.run_wall_budget_ms;
        let within = move || elapsed() < budget_ms;
        // The rolled memo is saved before any document carries its
        // boundary-latched values, so a run that dies (or whose final save
        // fails) after writing never lets a later run in the same tempo read
        // them again.
        let snapshot = durable.clone();
        let save = async |memo: &crate::dashboard::Memo| -> bool {
            let mut state = snapshot;
            state.dashboard = memo.clone();
            let saved: Option<()> = state_call(
                &env,
                "/checkpoint",
                &Save {
                    holder: holder.clone(),
                    now_ms: now_ms(),
                    state,
                },
            )
            .await;
            saved.is_some()
        };
        let pass = crate::dashboard::publish(&out, &public, &inputs, &mut durable.dashboard, save, &within).await;
        durable.publisher.refused.extend(pass.refused);
    }
    // Every refusal of this run, the dashboard's included: the count the
    // final save carries to the next run's `publication_refused` alerts.
    let refused = durable.publisher.refused.len();
    let ended: Option<()> = state_call(
        &env,
        "/end",
        &Save {
            holder,
            now_ms: now_ms(),
            state: durable,
        },
    )
    .await;
    if ended.is_none() {
        console_error!(
            "publisher: final state not saved; this run's refusals are not carried to the alerts"
        );
    }
    console_log!(
        "publisher: published {:?}, retired before publication {:?}, {} artifacts copied, {} refused, {} pending, {} unverifiable, {} ms",
        report.published,
        report.retired_unpublished,
        report.artifacts_copied,
        refused,
        report.pending.len(),
        report.unverifiable.len(),
        now_ms().saturating_sub(started)
    );
    // The dead-man heartbeat: only a run that completed both phases pings.
    if let (Some(url), true, true, true) = (
        &cfg.heartbeat,
        index.is_some(),
        status_written,
        ended.is_some(),
    ) {
        let _ = post(url, None).await;
    }
}
