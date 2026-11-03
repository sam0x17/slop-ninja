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
    Bucket, Data, Date, Delay, DurableObject, Env, Headers, HttpMetadata, Method, Request,
    RequestInit, Response, ResponseBody, Result, ScheduleContext, ScheduledEvent, State,
    console_error, console_log, durable_object, event,
};

use crate::alerts::{self, AlertState, Posted, PublisherObs};
use crate::arena::{self, Budget};
use crate::config::{self, Config, Sink};
use crate::layout::{self, Cache};
use crate::publish::{Buckets, Keepalive, Memo, Report, Run};
use crate::status::{self, Observed, SnapshotInputs};
use crate::store::{ObjectMeta, Put, Store, StoreError};
use slop_ninja_alerts_wire as wire;
use slop_ninja_protocol::Digest;

const RECEIPTS: &str = "RECEIPTS_TESTNET";
const EVAL: &str = "EVAL_TESTNET";
const PUBLIC: &str = "PUBLIC_TESTNET";
const COORDINATOR_SERVICE: &str = "COORDINATOR_TESTNET";
const EVAL_SERVICE: &str = "EVAL_WORKER_TESTNET";
const STATE: &str = "PUBLISHER_STATE_TESTNET";
/// The private alerts relay Worker (decision D18), reached only through this
/// service binding.
const RELAY: &str = "ALERTS_RELAY_TESTNET";

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

    async fn delete(&self, key: &str) -> std::result::Result<(), StoreError> {
        self.0
            .delete(key)
            .await
            .map_err(|_| StoreError::Unavailable("r2 delete"))
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
    let bound = arena::status_body_bound(budget.remaining());
    // The body, and a doubling buffer's old copy while it grows.
    let _hold = budget.reserve(bound.saturating_mul(3)).ok()?;
    let declared = response
        .headers()
        .get("content-length")
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|n| n > bound) {
        return None;
    }
    let mut out = Vec::with_capacity(declared.unwrap_or(0) as usize);
    let mut stream = response.stream().ok()?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.ok()?;
        if (out.len() + chunk.len()) as u64 > bound {
            return None;
        }
        out.extend_from_slice(&chunk);
    }
    parse(&out)
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Durable {
    alerts: AlertState,
    memo: Memo,
    /// The last publication pass's own conditions, alerted by the next
    /// run's first phase.
    publisher: PublisherObs,
    last_published: Option<u64>,
    /// Due alert events the last alert phase left undelivered, and the start
    /// of the first run of the current series that left some (sent in the
    /// relay heartbeat).
    #[serde(default)]
    undelivered: u64,
    #[serde(default)]
    undelivered_since_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Lease {
    holder: String,
    until_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct Begin {
    holder: String,
    now_ms: u64,
}

/// `/begin`'s answer: the state, and whether the previous holder left its
/// lease behind (it stopped before `/end`).
#[derive(Serialize, Deserialize)]
struct Begun {
    state: Durable,
    interrupted: bool,
}

#[derive(Serialize, Deserialize)]
struct Save {
    holder: String,
    now_ms: u64,
    state: Durable,
}

/// Holds the alert state and the publisher's memo, and serializes runs: a
/// run works only while it holds the lease, which it renews between steps,
/// so overlapping cron invocations never publish or post twice, and a run
/// killed mid-step blocks the next ones for one lease at most.
#[durable_object(fetch)]
pub struct PublisherState {
    state: State,
}

impl DurableObject for PublisherState {
    fn new(state: State, _env: Env) -> Self {
        Self { state }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let storage = self.state.storage();
        let lease: Option<Lease> = storage.get("lease").await?;
        match (req.method(), req.path().as_str()) {
            (Method::Post, "/begin") => {
                let b: Begin = req.json().await?;
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
                let b: Begin = req.json().await?;
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
                let e: Save = req.json().await?;
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

async fn put_status(public: &R2, snapshot: &status::PublicStatus) -> bool {
    match serde_json::to_vec_pretty(snapshot) {
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
        put_status(&R2(public), &snapshot).await;
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
    let (Ok(receipts), Ok(eval), Ok(public)) =
        (env.bucket(RECEIPTS), env.bucket(EVAL), env.bucket(PUBLIC))
    else {
        console_error!("publisher: a bucket binding is missing");
        return;
    };
    let (receipts, eval, public) = (R2(receipts), R2(eval), R2(public));
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
        put_status(&public, &snapshot).await;
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
    let status_written = put_status(&public, &snapshot).await;
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
            eval: &eval,
            public: &public,
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
        config: None,
        interrupted: false,
        warnings: Vec::new(),
    };
    if let Some(i) = &index {
        durable.last_published = i.last_cohort();
    }
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
    console_log!(
        "publisher: published {:?}, {} artifacts copied, {} refused, {} pending, {} unverifiable, {} ms",
        report.published,
        report.artifacts_copied,
        report.refused.len(),
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
