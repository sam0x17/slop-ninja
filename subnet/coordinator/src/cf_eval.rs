//! Workers side of hidden evaluation (wasm32 only): the `EvalCohort` authority,
//! its lane routes and sweep, and the heartbeat kick. The lane objects
//! themselves (`AScorer`, `EvalLane`, `GenLimiter`, `SpendLedger`) are in
//! `cf_lanes.rs`.
//!
//! `EvalCohort` is the cohort authority, one instance per subnet (`main`) plus
//! the separate load-rehearsal instance (`load`). A kick (from every webhook
//! delivery and every watchdog tick) records the RPC-confirmed block and, from
//! ticks, the subnet epoch index, advances boundary phases, and arms an alarm
//! when work is runnable. The alarm runs the root steps (draw, seal, package,
//! ledger settlement) and the lane sweep: due lanes get `gen + 1` in a
//! transaction, and only then are their content-free kicks sent through the
//! Queue (directly, at most 6 at a time, if `sendBatch` throws). It re-arms
//! about every 10 s while heartbeats are fresh, so coordinator silence pauses
//! progress and the next heartbeat resumes the same cohort.
//!
//! Lanes reach this object only through Durable Object stubs; each lane route
//! is one transaction over the engine. Nothing here logs. Texts move only
//! between the eval bucket, Workers AI, Durable Object storage, lane memory and
//! containers; receipts and the package go to the receipts bucket and carry no
//! text.

use super::{TxError, error, json, migrate_storage, now_ms, transaction};
use crate::eval::engine::{self, EvalConfig, GenItem, GenOutcome, HEARTBEAT_FRESH_MS, Kick, Work};
use crate::eval::lanes::{self, BundleReport, LaneKick, LaneRef, LaneReport, SummaryReport};
use crate::eval::package;
use crate::eval::plan::{Ceilings, CohortPlan, LoadSource, parse_hex32};
use crate::eval::pool::{self, LedgerEntry, PoolManifest, ShardRow};
use crate::eval::textgen::{self, Reject};
use crate::eval::v3;
use crate::sql::{EVAL_COHORT_SCHEMA, Sql as _};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json as j};
use sha2::{Digest as _, Sha256};
use slop_ninja_protocol::{ChainBinding, Record, SigningKey, TESTNET_GENESIS};
use std::collections::BTreeMap;
use std::time::Duration;
use wasm_bindgen::JsValue;
use slop_ninja_arena::{Refusal, RefusalKind, Scope, ScopeRef};
use worker::{Delay, DurableObject, Env, Method, Request, RequestInit, Response, Result, State};

pub const A_SCORER: &str = "A_SCORER";
pub const EVAL_LANE: &str = "EVAL_LANE";
pub const GEN_LIMITER: &str = "GEN_LIMITER";
pub const SPEND_LEDGER: &str = "SPEND_LEDGER";
pub const LANE_QUEUE: &str = "EVAL_LANES";
pub const EVAL_BUCKET: &str = "EVAL_TESTNET";
pub const RECEIPTS_BUCKET: &str = "RECEIPTS_TESTNET";
pub const INTERNAL: &str = "https://eval.internal";
/// The header naming the cohort instance a request is for (`main` or `load`).
pub const INSTANCE_HEADER: &str = "x-eval-instance";
/// The header naming the main authority a request is for (`config::Authority`;
/// `main`, the original, when absent). Every cohort object records the one
/// its first request named and refuses any other.
pub const AUTHORITY_HEADER: &str = "x-eval-authority";
/// Marks a read-only audit request (`/eval/authority/<name>/status`): an
/// object that stores no identity answers 404 and writes nothing.
pub const AUDIT_HEADER: &str = "x-eval-audit";
/// Wall-clock budget of one cohort alarm step.
const STEP_BUDGET_MS: u64 = 60_000;
/// The sweep cadence while lanes live.
const SWEEP_MS: i64 = 10_000;
/// Messages per `sendBatch` (the Queues limit).
const KICK_BATCH: usize = 100;
/// Connections a fallback may hold open at once (the per-invocation limit).
/// R2 operations a step keeps in flight (`arena_routes::STEP_READS_IN_FLIGHT`,
/// which the package footprint counts).
pub(super) const PARALLEL: usize = crate::arena_routes::STEP_READS_IN_FLIGHT;

pub fn sha_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn secret32(env: &Env, name: &str) -> Option<[u8; 32]> {
    let v = env.secret(name).ok()?.to_string();
    parse_hex32(v.trim())
}

fn var_u64(env: &Env, name: &str) -> Option<u64> {
    env.var(name).ok().and_then(|v| v.to_string().trim().parse().ok())
}

/// The eval role is opt-in per deployment (`EVAL_ROLE = "eval"`).
pub fn enabled(env: &Env) -> bool {
    super::role(env) == crate::config::Role::Eval
}

pub fn private_seed(env: &Env) -> Option<[u8; 32]> {
    secret32(env, "EVAL_PRIVATE_SEED")
}

/// The main instance's authority the deployment names (`EVAL_MAIN_AUTHORITY`,
/// `EVAL_MAIN_FIRST_COHORT` and `EVAL_MAIN_PREVIOUS`, tracked in
/// `wrangler.toml`; `config::Authority::parse_with`).
pub fn main_authority(env: &Env) -> std::result::Result<crate::config::Authority, &'static str> {
    let v = |name: &str| env.var(name).ok().map(|v| v.to_string());
    crate::config::Authority::parse_with(
        v("EVAL_MAIN_AUTHORITY").as_deref(),
        v("EVAL_MAIN_FIRST_COHORT").as_deref(),
        v("EVAL_MAIN_PREVIOUS").as_deref(),
    )
    .map_err(|_| "EVAL_MAIN_AUTHORITY")
}

/// An instance's configuration, with the authority the deployment names for
/// the main instance (the load instance is always the original authority's).
/// A cohort object replaces the authority with its own ([`EvalCohort`]).
pub fn eval_config(env: &Env, instance: &str) -> std::result::Result<EvalConfig, &'static str> {
    let authority =
        if instance == engine::INSTANCE_LOAD { crate::config::Authority::default() } else { main_authority(env)? };
    eval_config_with(env, instance, authority)
}

/// A cohort object's configuration under its own authority `own` (recorded
/// from its first request) and the successor it recorded, if any
/// (`config::own_authority`): the deployment's when it names `own` and no
/// successor superseded it, else `own` retired, which keeps its lane names,
/// probe keys and kicks for the work it still holds and admits no plan.
pub fn own_config(
    env: &Env,
    instance: &str,
    own: &str,
    superseded_by: Option<&str>,
) -> std::result::Result<EvalConfig, &'static str> {
    if instance == engine::INSTANCE_LOAD {
        return eval_config(env, instance);
    }
    let deployed = main_authority(env).ok();
    eval_config_with(env, instance, crate::config::own_authority(deployed.as_ref(), own, superseded_by))
}

fn eval_config_with(
    env: &Env,
    instance: &str,
    authority: crate::config::Authority,
) -> std::result::Result<EvalConfig, &'static str> {
    let netuid = var_u64(env, "SUBNET_NETUID").and_then(|v| u16::try_from(v).ok()).ok_or("netuid")?;
    let load = instance == engine::INSTANCE_LOAD;
    let cap = var_u64(env, if load { "LOAD_ROSTER_CAP" } else { "ROSTER_CAP" }).map(|v| v as usize);
    Ok(EvalConfig {
        chain: ChainBinding { genesis: TESTNET_GENESIS, netuid, mechanism: 0 },
        private_seed: private_seed(env).ok_or("EVAL_PRIVATE_SEED")?,
        instance: instance.into(),
        max_score_lanes: var_u64(env, "MAX_SCORE_LANES").unwrap_or(0) as u32,
        ceilings: Ceilings { roster_cap: cap, accept_reward_v0: false },
        exposed_pools: env
            .var("EXPOSED_POOLS")
            .map(|v| v.to_string().split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned).collect())
            .unwrap_or_default(),
        scorer_binary: env.var("SCORER_BINARY_SHA256").map(|v| v.to_string()).unwrap_or_default(),
        authority,
    })
}

/// The package and receipt signing keys of the main instance. Each must derive
/// the public trust root configured beside it (`PACKAGE_PUBLIC_KEY`,
/// `RECEIPT_PUBLIC_KEY`: the T12 roots validators are configured with), so
/// nothing is signed under a key no configured validator trusts.
pub fn keys(env: &Env) -> std::result::Result<(SigningKey, SigningKey), &'static str> {
    let p = secret32(env, "EVAL_PACKAGE_SIGNING_KEY").ok_or("EVAL_PACKAGE_SIGNING_KEY")?;
    let r = secret32(env, "EVAL_RECEIPT_SIGNING_KEY").ok_or("EVAL_RECEIPT_SIGNING_KEY")?;
    if p == r {
        return Err("package and receipt keys must differ");
    }
    let (p, r) = (SigningKey::from_bytes(&p), SigningKey::from_bytes(&r));
    let root = |name: &str| env.var(name).ok().and_then(|v| parse_hex32(v.to_string().trim()));
    if root("PACKAGE_PUBLIC_KEY") != Some(p.verifying_key().to_bytes()) {
        return Err("package signing key is not the configured trust root");
    }
    if root("RECEIPT_PUBLIC_KEY") != Some(r.verifying_key().to_bytes()) {
        return Err("receipt signing key is not the configured trust root");
    }
    Ok((p, r))
}

/// The load instance's own keys, derived from the private seed: load packages
/// verify only under these, never under a root any validator trusts.
pub fn load_keys(env: &Env) -> std::result::Result<(SigningKey, SigningKey), &'static str> {
    let seed = private_seed(env).ok_or("EVAL_PRIVATE_SEED")?;
    let k = |role: &str| SigningKey::from_bytes(&slop_ninja_protocol::domain_hash(&format!("slop_ninja.eval/load-signing/v1/{role}"), &seed).0);
    Ok((k("package"), k("receipt")))
}

fn instance_keys(env: &Env, instance: &str) -> std::result::Result<(SigningKey, SigningKey), &'static str> {
    if instance == engine::INSTANCE_LOAD { load_keys(env) } else { keys(env) }
}

pub async fn put(env: &Env, bucket: &str, key: &str, bytes: Vec<u8>) -> Result<()> {
    env.bucket(bucket)?.put(key, bytes).execute().await?;
    Ok(())
}

/// An object of at most `bound` bytes (its kind's bound), read through the
/// object body's stream (JS holds one chunk at a time) into one exact block
/// of the current scope admitted before the read (`cf_io::r2_bytes`).
pub async fn get_bounded(env: &Env, bucket: &str, key: &str, bound: usize) -> std::result::Result<Option<Vec<u8>>, BodyError> {
    let b = env.bucket(bucket).map_err(|_| BodyError::Transport)?;
    super::cf_io::r2_bytes(&b, key, bound, None).await
}

/// An object read by the fan-out lane objects (`cf_lanes.rs`): streamed like
/// every R2 read, bounded by the largest route the memory model holds (the
/// lane objects run unscoped until they are wrapped; their objects' kinds
/// keep their own checks). An arena refusal or a bound is an error the
/// caller's existing retry path handles.
pub async fn get_bytes(env: &Env, bucket: &str, key: &str) -> Result<Option<Vec<u8>>> {
    get_bounded(env, bucket, key, slop_ninja_arena::model::LARGEST_ROUTE_BYTES)
        .await
        .map_err(|e| worker::Error::RustError(e.code().into()))
}

pub use crate::arena_routes::{STEP_OBJECT_BYTES, summary_bytes};

/// A lane object's request to its cohort object: at most one chunk's
/// outcomes, one generation batch's texts (`inflight_per_lane` of at most
/// six) or one page of a job's predictions, read whole: bounded like any
/// body a route holds whole (`STEP_OBJECT_BYTES`).
const LANE_BODY_BYTES: usize = STEP_OBJECT_BYTES;

/// An operator or internal body to the cohort object: forwarded by the
/// Worker's `/eval/` routes, which already bound it by the plan body bound,
/// or built by the Worker (a kick).
const OPERATOR_BODY_BYTES: usize = crate::arena_routes::MAX_PLAN_BODY_BYTES;
use super::cf_io::BodyError;

/// `dir/rest`, or `rest` for a pool manifest at the bucket root.
fn under(dir: &str, rest: &str) -> String {
    if dir.is_empty() { rest.to_owned() } else { format!("{dir}/{rest}") }
}

fn ledger_key(dir: &str, label: &str, authority: &str) -> String {
    under(dir, &format!("used/{label}.{authority}.json"))
}

/// Every entry of the durable used ledger beside the pool. Any list, read or
/// parse failure refuses the draw. Each entry is read into one block that
/// the next read re-uses (the arena re-keys a returned block run), so the
/// draw's pages do not grow with the ledger's history.
async fn read_ledger(c: &EvalCohort, seq: i64, dir: &str, pool_id: &str) -> std::result::Result<Vec<LedgerEntry>, &'static str> {
    let env = &c.env;
    let bucket = env.bucket(EVAL_BUCKET).map_err(|_| "eval bucket")?;
    let prefix = under(dir, "used/");
    let mut entries = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut list = bucket.list().prefix(prefix.clone());
        if let Some(c) = &cursor {
            list = list.cursor(c.clone());
        }
        let listed = list.execute().await.map_err(|_| "used ledger list")?;
        for o in listed.objects() {
            let bytes = c.read(seq, EVAL_BUCKET, &o.key(), STEP_OBJECT_BYTES, "used ledger read").await?.ok_or("used ledger missing")?;
            entries.push(LedgerEntry::parse(&bytes, pool_id).map_err(|e| e.0)?);
        }
        if !listed.truncated() {
            break;
        }
        cursor = Some(listed.cursor().ok_or("used ledger cursor")?);
    }
    Ok(entries)
}

pub fn internal_request(method: Method, path: &str, body: Option<String>) -> Result<Request> {
    let mut init = RequestInit::new();
    init.with_method(method);
    if let Some(body) = body {
        init.with_body(Some(JsValue::from_str(&body)));
    }
    Request::new_with_init(&format!("{INTERNAL}{path}"), &init)
}

fn chain_of(env: &Env) -> Option<crate::config::ChainConfig> {
    let genesis = env.var("CHAIN_GENESIS").ok()?.to_string();
    let netuid = env.var("SUBNET_NETUID").ok().map(|v| v.to_string());
    crate::config::chain_config(&genesis, netuid.as_deref()).ok()
}

/// The authority an instance's requests go to: the one the deployment names
/// for the main instance, the original for the load instance. `None` when the
/// authority vars are malformed, so a misconfigured deployment reaches none.
fn configured_authority(env: &Env, instance: &str) -> Option<String> {
    if instance == engine::INSTANCE_LOAD {
        Some(crate::config::ORIGINAL_AUTHORITY.into())
    } else {
        main_authority(env).ok().map(|a| a.name().to_owned())
    }
}

/// The cohort object's Durable Object name for an instance under `authority`:
/// the load instance's `<base>/load`, a main authority's
/// `config::authority_cohort_name`.
fn object_name(env: &Env, instance: &str, authority: &str) -> Option<String> {
    let chain = chain_of(env)?;
    if instance == engine::INSTANCE_LOAD {
        Some(format!("{}/load", crate::config::eval_cohort_name(&chain)?))
    } else {
        crate::config::authority_cohort_name(&chain, authority)
    }
}

/// The cohort authority's Durable Object name for an instance, under the
/// authority the deployment names.
pub fn cohort_name(env: &Env, instance: &str) -> Option<String> {
    object_name(env, instance, &configured_authority(env, instance)?)
}

/// A request to an instance's cohort authority.
pub async fn cohort_fetch(env: &Env, instance: &str, method: Method, path: &str, body: Option<String>) -> Result<Response> {
    cohort_fetch_within(env, instance, method, path, body, None).await
}

/// [`cohort_fetch`] with an `AbortSignal.timeout(timeout_ms)` the platform
/// enforces on the call and its reply (`cf_io::internal_request`).
pub async fn cohort_fetch_within(env: &Env, instance: &str, method: Method, path: &str, body: Option<String>, timeout_ms: Option<u64>) -> Result<Response> {
    let authority =
        configured_authority(env, instance).ok_or_else(|| worker::Error::RustError("not configured".into()))?;
    authority_fetch(env, instance, &authority, method, path, body, timeout_ms).await
}

/// A request to main authority `authority` (or the load instance), whatever
/// the deployment names: lanes call back the authority that kicked them, a
/// successor asks a predecessor to record it, and the operator reaches an
/// earlier authority's faults.
pub async fn authority_fetch(
    env: &Env,
    instance: &str,
    authority: &str,
    method: Method,
    path: &str,
    body: Option<String>,
    timeout_ms: Option<u64>,
) -> Result<Response> {
    authority_fetch_as(env, instance, authority, method, path, body, timeout_ms, false).await
}

#[allow(clippy::too_many_arguments)]
async fn authority_fetch_as(
    env: &Env,
    instance: &str,
    authority: &str,
    method: Method,
    path: &str,
    body: Option<String>,
    timeout_ms: Option<u64>,
    audit: bool,
) -> Result<Response> {
    let name =
        object_name(env, instance, authority).ok_or_else(|| worker::Error::RustError("not configured".into()))?;
    let binding = crate::config::binding("EVAL_COHORT", crate::config::NETWORK);
    let stub = env.durable_object(&binding)?.get_by_name_with_location_hint(&name, "enam")?;
    let req = super::cf_io::internal_request(&format!("{INTERNAL}{path}"), method, body.as_deref(), timeout_ms)?;
    req.headers().set(INSTANCE_HEADER, instance)?;
    req.headers().set(AUTHORITY_HEADER, authority)?;
    if audit {
        req.headers().set(AUDIT_HEADER, "1")?;
    }
    stub.fetch_with_request(req).await
}

/// `GET /eval/authority/<name>/status`: main authority `name`'s status, read
/// only. An object that stores no identity answers 404 and writes nothing.
pub async fn audit_status(env: &Env, authority: &str) -> Result<Response> {
    authority_fetch_as(env, engine::INSTANCE_MAIN, authority, Method::Get, "/status", None, None, true).await
}

/// A tick's kick to main authority `authority` by name (an earlier one that
/// may still hold work; `config::kicked_authorities`).
pub async fn kick_authority(env: &Env, authority: &str, kick: Kick) -> Option<Value> {
    let body = serde_json::to_string(&kick).ok()?;
    let mut r = authority_fetch(
        env,
        engine::INSTANCE_MAIN,
        authority,
        Method::Post,
        "/kick",
        Some(body),
        Some(KICK_TIMEOUT_MS),
    )
    .await
    .ok()?;
    if r.status_code() != 200 {
        super::cf_io::discard(&mut r);
        return None;
    }
    super::cf_io::response_json::<Value>(&mut r, DO_REPLY_BYTES).await.ok()
}

/// A heartbeat's kick to a cohort object (one transaction), bounded like a
/// journal call (`liveness::TIMEOUT_MS`): the delivery continuation and the
/// cron start one only when this allowance still fits their budget.
pub const KICK_TIMEOUT_MS: u64 = crate::liveness::TIMEOUT_MS;

/// The bound of a reply from a cohort, ledger or limiter object that a
/// route holds whole: the smallest control footprint of the routes that
/// read them (the alarm's and the Worker routes', 1 MiB); a larger reply is
/// refused (`body_too_large`) instead of overdrawing the scope.
const DO_REPLY_BYTES: usize = crate::arena_routes::ALARM_CONTROL_BYTES;

/// A named authority's record of its predecessors' successions: `(name,
/// first cohort checked)` pairs (`config::succession_recorded`), one per
/// predecessor, so it is bounded by `EVAL_MAIN_PREVIOUS`.
const SUCCEEDED_KEY: &str = "succession_recorded";

/// The successions this object recorded (none when unreadable: then every
/// predecessor is asked again, which changes nothing it recorded).
async fn succeeded(storage: &worker::Storage) -> Vec<(String, u64)> {
    storage.get::<Vec<(String, u64)>>(SUCCEEDED_KEY).await.ok().flatten().unwrap_or_default()
}

/// A lane's JSON call to the cohort authority that kicked it (`k.authority`,
/// the original when absent), retried with backoff on overload or transport
/// errors (holding no claim while it waits). An earlier authority's lane
/// never reports to a later one.
pub async fn cohort_call<T: serde::de::DeserializeOwned>(
    env: &Env,
    k: &LaneKick,
    path: &str,
    body: &impl Serialize,
) -> std::result::Result<T, &'static str> {
    let body = serde_json::to_string(body).map_err(|_| "request json")?;
    let authority = k.authority.as_deref().unwrap_or(crate::config::ORIGINAL_AUTHORITY);
    let mut delay = 250u64;
    for attempt in 0..6 {
        match authority_fetch(env, &k.instance, authority, Method::Post, path, Some(body.clone()), None).await {
            Ok(mut r) if r.status_code() == 200 => return super::cf_io::response_json::<T>(&mut r, LANE_BODY_BYTES).await.map_err(|_| "reply json"),
            Ok(mut r) => {
                // An unread body keeps the invocation open (`cf_io::discard`).
                super::cf_io::discard(&mut r);
                if r.status_code() == 422 {
                    return Err("refused by the cohort");
                }
                if attempt == 5 {
                    break;
                }
            }
            _ if attempt == 5 => break,
            _ => {}
        }
        let jitter = (js_sys::Math::random() * delay as f64) as u64;
        Delay::from(Duration::from_millis(delay + jitter)).await;
        delay = (delay * 2).min(10_000);
    }
    Err("cohort unreachable")
}

/// A lane's Durable Object stub: scoring lanes are container-owning `AScorer`
/// objects, every other kind an `EvalLane`.
pub fn lane_stub(env: &Env, kind: &str, name: &str) -> Result<worker::Stub> {
    let base = if kind == "score" { A_SCORER } else { EVAL_LANE };
    let binding = crate::config::binding(base, crate::config::NETWORK);
    env.durable_object(&binding)?.get_by_name_with_location_hint(name, "enam")
}

/// Delivers one kick straight to its lane (the queue consumer, and the
/// fallback when `sendBatch` throws).
pub async fn kick_lane(env: &Env, k: &LaneKick) -> bool {
    let Ok(stub) = lane_stub(env, &k.kind, &k.name) else { return false };
    let Ok(body) = serde_json::to_string(k) else { return false };
    let Ok(req) = internal_request(Method::Post, "/kick", Some(body)) else { return false };
    match stub.fetch_with_request(req).await {
        Ok(mut r) => {
            // Only the status is needed; the body is cancelled, not left
            // unread (`cf_io::discard`).
            super::cf_io::discard(&mut r);
            r.status_code() == 200
        }
        Err(_) => false,
    }
}

async fn stop_lanes(env: &Env, names: &[String]) {
    for group in names.chunks(PARALLEL) {
        join_all(group.iter().map(|name| async move {
            let kind = if name.starts_with("a-scorer/") { "score" } else { "gen" };
            if let (Ok(stub), Ok(req)) = (lane_stub(env, kind, name), internal_request(Method::Post, "/stop", None)) {
                let _ = stub.fetch_with_request(req).await;
            }
        }))
        .await;
    }
}

/// Sends kicks through the Queue, in batches of 100; if `sendBatch` throws,
/// kicks directly, at most 6 in flight. Load-mode fault hooks may drop some
/// kicks (the sweep re-sends them) or send each twice.
async fn send_kicks(env: &Env, kicks: Vec<LaneKick>, drop_permille: u16, duplicate: bool) -> Value {
    let mut msgs = Vec::new();
    let mut dropped = 0;
    for k in kicks {
        let h = Sha256::digest(format!("{}:{}", k.name, k.generation).as_bytes());
        if u16::from_le_bytes([h[0], h[1]]) % 1000 < drop_permille {
            dropped += 1;
            continue;
        }
        if duplicate {
            msgs.push(k.clone());
        }
        msgs.push(k);
    }
    let total = msgs.len();
    let mut queued = 0;
    let mut direct = 0;
    let mut rest: Vec<LaneKick> = Vec::new();
    match env.queue(&crate::config::binding(LANE_QUEUE, crate::config::NETWORK)) {
        Ok(q) => {
            for (n, batch) in msgs.chunks(KICK_BATCH).enumerate() {
                if q.send_batch(batch.to_vec()).await.is_ok() {
                    queued += batch.len();
                } else {
                    rest = msgs[n * KICK_BATCH..].to_vec();
                    break;
                }
            }
        }
        Err(_) => rest = msgs,
    }
    for group in rest.chunks(PARALLEL) {
        direct += join_all(group.iter().map(|k| kick_lane(env, k))).await.into_iter().filter(|ok| *ok).count();
    }
    j!({"kicks": total, "queued": queued, "direct": direct, "dropped": dropped})
}

/// The cohort authority's ledger account in the deployment's `SpendLedger`.
/// Records that a bookkeeping step of `kind` failed (`code`), so it waits
/// out its retry time (`engine::bookkeeping_failed`). Best effort: a record
/// that cannot be written leaves the step due, as before.
fn bookkeeping_failed(storage: &worker::Storage, kind: &'static str, code: &'static str) {
    let now = now_ms();
    let _ = transaction(storage, move |sql| engine::bookkeeping_failed(sql, kind, code, now));
}

pub async fn ledger_call(env: &Env, path: &str, body: &Value) -> std::result::Result<Value, &'static str> {
    let binding = crate::config::binding(SPEND_LEDGER, crate::config::NETWORK);
    let stub = env
        .durable_object(&binding)
        .and_then(|ns| ns.get_by_name(&format!("spend-ledger/{}", crate::config::NETWORK)))
        .map_err(|_| "ledger binding")?;
    let req = internal_request(Method::Post, path, Some(body.to_string())).map_err(|_| "ledger request")?;
    let mut r = stub.fetch_with_request(req).await.map_err(|_| "ledger unreachable")?;
    if r.status_code() != 200 {
        super::cf_io::discard(&mut r);
        return Err("ledger refused");
    }
    super::cf_io::response_json::<Value>(&mut r, DO_REPLY_BYTES).await.map_err(|_| "ledger json")
}

pub async fn limiter_status(env: &Env) -> Option<Value> {
    let binding = crate::config::binding(GEN_LIMITER, crate::config::NETWORK);
    let stub = env.durable_object(&binding).ok()?.get_by_name(&format!("gen-limiter/{}", crate::config::NETWORK)).ok()?;
    let mut r = stub.fetch_with_request(internal_request(Method::Get, "/status", None).ok()?).await.ok()?;
    super::cf_io::response_json::<Value>(&mut r, DO_REPLY_BYTES).await.ok()
}

// ------------------------------------------------------------------ EvalCohort

/// Per-subnet cohort authority (testnet): phases, the never-reuse pool set, the
/// batch, scoring jobs, the lane table, receipts and the package inputs. Its
/// wasm-bindgen class is hand-written (`cf_arena::EvalCohortExport`).
pub struct EvalCohort {
    pub(super) state: State,
    pub(super) env: Env,
    schema: std::result::Result<u32, &'static str>,
}

fn tx_reply<T: Serialize>(r: std::result::Result<T, TxError<engine::EvalError>>) -> Result<Response> {
    match r {
        Ok(v) => json(&v),
        Err(TxError::Operation(e)) => {
            let mut resp = Response::from_json(&j!({"error": e.code()}))?.with_status(422);
            resp.headers_mut().set("Cache-Control", "no-store")?;
            Ok(resp)
        }
        Err(TxError::Runtime) => error("store_unavailable", 503),
    }
}

impl worker::has_durable_object_attribute for EvalCohort {}

impl DurableObject for EvalCohort {
    fn new(state: State, env: Env) -> Self {
        let schema = super::sync_scoped(crate::arena_routes::R_COHORT_NEW, crate::arena_routes::CONSTRUCTOR_CONTROL_BYTES, || {
            migrate_storage(&state.storage(), &EVAL_COHORT_SCHEMA)
        });
        Self { state, env, schema }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let version = match self.schema {
            Ok(v) => v,
            Err(code) => return error(code, 503),
        };
        let storage = self.state.storage();
        let path = req.path();
        // A read-only request (the audit route, a successor's supersede call)
        // never creates an object's identity (`config::object_identity`).
        let may_create = !(path.starts_with("/succession/") || req.headers().get(AUDIT_HEADER)?.is_some());
        let stored_instance = storage.get::<String>("instance").await.ok().flatten();
        // An object created before authorities were named is the original's.
        let stored_authority = storage
            .get::<String>("authority")
            .await
            .ok()
            .flatten()
            .or_else(|| stored_instance.as_ref().map(|_| crate::config::ORIGINAL_AUTHORITY.to_owned()));
        // This object's instance and authority, fixed by its first request: a
        // main authority's object is only ever addressed under its own name
        // (`authority_fetch`).
        let asked = req.headers().get(INSTANCE_HEADER)?.unwrap_or_else(|| engine::INSTANCE_MAIN.into());
        let (instance, write_instance) =
            match crate::config::object_identity(stored_instance.as_deref(), &asked, may_create) {
                Ok(v) => v,
                Err("unknown_authority") => return error("unknown_authority", 404),
                Err(_) => return error("instance_mismatch", 409),
            };
        let asked = req.headers().get(AUTHORITY_HEADER)?.unwrap_or_else(|| crate::config::ORIGINAL_AUTHORITY.into());
        let (authority, write_authority) =
            match crate::config::object_identity(stored_authority.as_deref(), &asked, may_create) {
                Ok(v) => v,
                Err("unknown_authority") => return error("unknown_authority", 404),
                Err(code) => return error(code, 409),
            };
        if write_instance {
            storage.put("instance", &instance).await?;
        }
        if write_authority {
            storage.put("authority", &authority).await?;
        }
        // The successor it recorded, if any (`eval::succession`); read in a
        // transaction, and an unreadable store refuses rather than admits.
        let superseded = if instance == engine::INSTANCE_MAIN {
            match transaction(&storage, crate::eval::succession::superseded_by) {
                Ok(v) => v,
                Err(_) => return error("store_unavailable", 503),
            }
        } else {
            None
        };
        let Ok(cfg) = own_config(&self.env, &instance, &authority, superseded.as_deref()) else {
            return error("not_configured", 503);
        };
        let now = now_ms();
        // The lanes' routes; the operator's vendor clear, under `/gen/`
        // too, is answered below (`lanes::lane_route_path`).
        if lanes::lane_route_path(&path) {
            return self.lane_route(&mut req, &path, cfg, now).await;
        }
        // Admission, the commitment reader and the calendar belong to the
        // authoritative instance only.
        let main = instance == engine::INSTANCE_MAIN;
        if path.starts_with("/admission/") {
            return if main { self.admission_route(&mut req, &path).await } else { error("main_instance_only", 404) };
        }
        if path.starts_with("/commitments/") {
            return if main { self.commitments_route(&mut req, &path).await } else { error("main_instance_only", 404) };
        }
        if matches!(
            path.as_str(),
            "/template" | "/template/pause" | "/calendar" | "/cost-model" | "/session/refuse"
        ) {
            return if main { self.template_route(&mut req, &path, cfg).await } else { error("main_instance_only", 404) };
        }
        match (req.method(), path.as_str()) {
            (Method::Post, "/succession/supersede") if instance == engine::INSTANCE_MAIN => {
                let r =
                    match super::cf_io::request_json::<crate::config::Supersede>(&mut req, OPERATOR_BODY_BYTES).await {
                        Ok(r) => r,
                        Err(e) => return error(e.code(), e.status()),
                    };
                // Under this authority's own configuration (retired: the
                // deployment names its successor), so its calendar counts no work.
                let c2 = cfg.clone();
                match transaction(&storage, move |sql| crate::eval::succession::supersede(sql, &c2, &r, now)) {
                    Ok(Ok(v)) => json(&v),
                    Ok(Err(code)) => {
                        let mut resp = Response::from_json(&j!({"refused": code}))?.with_status(409);
                        resp.headers_mut().set("Cache-Control", "no-store")?;
                        Ok(resp)
                    }
                    Err(_) => error("store_unavailable", 503),
                }
            }
            (Method::Post, "/succession/retention") if instance == engine::INSTANCE_MAIN => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Notice {
                    after_cohort: u64,
                }
                let n = match super::cf_io::request_json::<Notice>(&mut req, OPERATOR_BODY_BYTES).await {
                    Ok(n) => n,
                    Err(e) => return error(e.code(), e.status()),
                };
                let c2 = cfg.clone();
                let r = transaction(&storage, move |sql| {
                    let r = crate::eval::retention::accept_notice(sql, n.after_cohort)?;
                    Ok::<_, engine::EvalError>((r, engine::has_work(sql, &c2)?))
                });
                match r {
                    Ok((Ok(v), work)) => {
                        // A retired authority is no longer kicked: the notice
                        // arms the step that deletes what became due.
                        if work && storage.get_alarm().await?.is_none() {
                            storage.set_alarm(500i64).await?;
                        }
                        json(&v)
                    }
                    Ok((Err(code), _)) => {
                        let mut resp = Response::from_json(&j!({"refused": code}))?.with_status(409);
                        resp.headers_mut().set("Cache-Control", "no-store")?;
                        Ok(resp)
                    }
                    Err(_) => error("store_unavailable", 503),
                }
            }
            (Method::Post, "/kick") => {
                let kick = match super::cf_io::request_json::<Kick>(&mut req, OPERATOR_BODY_BYTES).await {
                    Ok(k) => k,
                    Err(e) => return error(e.code(), e.status()),
                };
                let c2 = cfg.clone();
                let moved = match transaction(&storage, move |sql| {
                    let moved = engine::observe(sql, &kick, now)?;
                    let scoring = engine::active(sql)?.is_some_and(|c| c.phase == "scoring");
                    Ok::<_, engine::EvalError>((moved, engine::kick_due(sql, &c2, &kick, now)?, scoring))
                }) {
                    Ok(v) => v,
                    Err(_) => return error("store_unavailable", 503),
                };
                // The configured main authority writes the cohort snapshot
                // the upload Worker serves the unsigned cohort read from
                // (a predecessor's kick never does).
                if instance == engine::INSTANCE_MAIN
                    && configured_authority(&self.env, &instance).as_deref() == Some(cfg.authority.name())
                {
                    self.write_cohort_snapshot().await;
                }
                // While a step runs its alarm is consumed (get_alarm is empty); the
                // step re-arms itself, so a kick never schedules an overlapping step.
                let running: i64 = storage.get::<i64>("step_until_ms").await.ok().flatten().unwrap_or(0);
                if moved.1 && running < now as i64 && storage.get_alarm().await?.is_none() {
                    storage.set_alarm(500i64).await?;
                }
                // Once every predecessor recorded this authority as successor,
                // none holds or takes work again: the Worker stops kicking them
                // (`config::kicked_authorities`).
                let complete = crate::config::succession_complete(&cfg.authority, &succeeded(&storage).await);
                json(&j!({"moved": moved.0, "work": moved.1, "scoring": moved.2, "succession_complete": complete}))
            }
            (Method::Get, "/status") => {
                let alarm = storage.get_alarm().await.ok().flatten();
                let chain = Some(cfg.chain.clone());
                let c2 = cfg.clone();
                match transaction(&storage, move |sql| {
                    let mut v = engine::status(sql, now)?;
                    if let Some(chain) = &chain {
                        v["reward"] = v3::regime(sql, chain)?;
                    }
                    // Whether it still has work (the deploy check's switch
                    // waits for none: `config::deploy_check`); a due receipt
                    // deletion is reported apart and never holds a switch.
                    v["work"] = j!(engine::blocking_work(sql, &c2)?);
                    v["retention_work"] = j!(crate::eval::retention::has_work(sql)?);
                    // Structural stalls (completions, never a clock): the
                    // publisher's `retention_stalled` reads this list.
                    v["receipt_retention"]["stalled"] = j!(crate::eval::retention::stalled(sql, &c2)?);
                    Ok::<_, engine::EvalError>(v)
                }) {
                    Ok(mut v) => {
                        v["schema_version"] = j!(version);
                        v["instance"] = j!(instance);
                        v["authority"] = j!(self.state.id().to_string());
                        v["authority_name"] = j!(cfg.authority.name());
                        v["authority_retired"] = j!(cfg.authority.is_retired());
                        v["first_cohort"] = j!(cfg.authority.first_cohort());
                        v["label_prefix"] = j!(cfg.authority.label_prefix());
                        v["superseded_by"] = j!(cfg.authority.superseded_by());
                        v["predecessors"] = j!(cfg.authority.predecessors());
                        // The predecessors' successions this store recorded
                        // (each of them is superseded), for the deploy check;
                        // null when unreadable, which that check refuses.
                        match storage.get::<Vec<(String, u64)>>(SUCCEEDED_KEY).await {
                            Ok(done) => {
                                let done = done.unwrap_or_default();
                                v["succession_recorded"] = j!(done
                                    .iter()
                                    .map(|(p, f)| j!({"predecessor": p, "first_cohort": f}))
                                    .collect::<Vec<_>>());
                                v["succession_complete"] =
                                    j!(crate::config::succession_complete(&cfg.authority, &done));
                            }
                            Err(_) => {
                                v["succession_recorded"] = Value::Null;
                                v["succession_complete"] = Value::Null;
                            }
                        }
                        v["last_step_error"] = storage.get::<Value>("last_step_error").await.ok().flatten().unwrap_or(Value::Null);
                        v["step_errors"] = j!(storage.get::<i64>("step_errors").await.ok().flatten().unwrap_or(0));
                        v["last_dispatch"] = storage.get::<Value>("last_dispatch").await.ok().flatten().unwrap_or(Value::Null);
                        v["alarm_in_ms"] = j!(alarm.map(|a| a - now as i64));
                        v["max_score_lanes"] = j!(cfg.max_score_lanes);
                        v["arena"] = super::arena_exports::arena_status();
                        v["roster_cap"] = j!(cfg.ceilings.roster_cap);
                        if instance == engine::INSTANCE_MAIN {
                            v["admission"] = transaction(&storage, crate::eval::admission::status).unwrap_or(Value::Null);
                            v["calendar"] = transaction(&storage, crate::eval::calendar::view).unwrap_or(Value::Null);
                        }
                        match instance_keys(&self.env, &instance) {
                            Ok((p, r)) => {
                                v["package_public_key"] = j!(hex::encode(p.verifying_key().to_bytes()));
                                v["receipt_public_key"] = j!(hex::encode(r.verifying_key().to_bytes()));
                            }
                            Err(code) => v["signing_keys_error"] = j!(code),
                        }
                        json(&v)
                    }
                    Err(_) => error("store_unavailable", 503),
                }
            }
            (Method::Post, "/plan") => {
                let bytes = match super::cf_io::request_bytes(&mut req, crate::arena_routes::MAX_PLAN_BODY_BYTES).await {
                    Ok(b) => b,
                    Err(super::cf_io::BodyError::TooLarge) => return error("plan_too_large", 413),
                    Err(e) => return error(e.code(), e.status()),
                };
                let adm = match engine::admit(&cfg, &bytes) {
                    Ok((plan, adm)) => {
                        // A plan whose package step the memory model cannot
                        // hold is refused now, never admitted to pause at
                        // packaging (`arena_routes::package_fits`).
                        if !crate::eval::calendar::fits(&plan) {
                            return tx_reply::<()>(Err(TxError::Operation(engine::EvalError::Plan("arena_capacity: the package step exceeds the memory model"))));
                        }
                        adm
                    }
                    Err(e) => return tx_reply::<()>(Err(TxError::Operation(e))),
                };
                // A named authority takes a plan only once every earlier
                // authority recorded it as its successor (`succeed`).
                if let Err(code) = self.succeed(&cfg).await {
                    return error(code, 409);
                }
                // A label already queued never touches the ledger again: the same bytes
                // (a retry after a lost reply) answer with the queued cohort, others
                // are refused, and the live cohort's reservation stays as it is.
                let label = adm.label.clone();
                let plan_sha = sha_hex(&bytes);
                let prior = transaction(&storage, move |sql| {
                    Ok::<_, engine::EvalError>(engine::cohorts(sql)?.into_iter().find(|c| c.label == label).map(|c| (c.seq, c.plan_sha256)))
                });
                match prior {
                    Ok(Some((seq, sha))) if sha == plan_sha => return json(&j!({"queued": seq, "already": true, "admission": adm})),
                    Ok(Some(_)) => return tx_reply::<()>(Err(TxError::Operation(engine::EvalError::Plan("label already queued")))),
                    Ok(None) => {}
                    Err(_) => return error("store_unavailable", 503),
                }
                // The plan's budgets are recorded in the tracking ledger before
                // it queues; spend never refuses a plan, and a ledger outage only
                // leaves this reservation unrecorded (the cohort's settle still
                // charges what it spent).
                let key = format!("{instance}:{}", adm.label);
                // `created` is false when the key was already held (a retried POST,
                // or the label is queued): then a refused queue must leave it alone.
                let created = match ledger_call(&self.env, "/reserve", &j!({"key": key, "microusd": adm.reserve_microusd})).await {
                    Ok(v) => v["created"] == true,
                    Err(_) => false,
                };
                let c2 = cfg.clone();
                match transaction(&storage, move |sql| engine::queue(sql, &c2, &bytes, now)) {
                    Ok(seq) => {
                        storage.set_alarm(500i64).await?;
                        json(&j!({"queued": seq, "admission": adm}))
                    }
                    Err(e) => {
                        if created {
                            let _ = ledger_call(&self.env, "/release", &j!({"key": key})).await;
                        }
                        tx_reply::<()>(Err(e))
                    }
                }
            }
            (Method::Post, "/clear") => {
                #[derive(Deserialize)]
                struct Clear {
                    label: String,
                }
                let c = match super::cf_io::request_json::<Clear>(&mut req, OPERATOR_BODY_BYTES).await {
                    Ok(c) => c,
                    Err(super::cf_io::JsonError::Parse) => return error("bad_request", 400),
                    Err(e) => return error(e.code(), e.status()),
                };
                match transaction(&storage, move |sql| engine::clear_fault(sql, &c.label, now)) {
                    Ok(cleared) => {
                        if cleared.cleared {
                            storage.set_alarm(500i64).await?;
                        }
                        json(&cleared)
                    }
                    Err(_) => error("store_unavailable", 503),
                }
            }
            (Method::Post, "/abandon") => {
                #[derive(Deserialize)]
                struct Abandon {
                    label: String,
                    reason: String,
                }
                let a = match super::cf_io::request_json::<Abandon>(&mut req, OPERATOR_BODY_BYTES).await {
                    Ok(a) => a,
                    Err(super::cf_io::JsonError::Parse) => return error("bad_request", 400),
                    Err(e) => return error(e.code(), e.status()),
                };
                let label = a.label.clone();
                match transaction(&storage, move |sql| {
                    let seq = engine::cohorts(sql)?.into_iter().find(|c| c.label == label).map(|c| c.seq);
                    let ok = engine::abandon(sql, &a.label, &a.reason, now)?;
                    let stop = match (ok, seq) {
                        (true, Some(seq)) => lanes::live_lane_names(sql, seq, &lanes::KINDS)?,
                        _ => Vec::new(),
                    };
                    Ok::<_, engine::EvalError>((ok, stop))
                }) {
                    Ok((abandoned, stop)) => {
                        stop_lanes(&self.env, &stop).await;
                        if abandoned {
                            storage.set_alarm(500i64).await?;
                        }
                        json(&j!({"abandoned": abandoned, "lanes_stopped": stop.len()}))
                    }
                    Err(TxError::Operation(e)) => error(e.code(), 422),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            (Method::Post, lanes::GEN_VENDOR_CLEAR_PATH) => {
                // An operator clear of a generation v2 vendor pause: the
                // vendor is active again from the next draw (and for the
                // retries and fallbacks of running cohorts). Idempotent.
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct VendorClear {
                    vendor: String,
                }
                let c = match super::cf_io::request_json::<VendorClear>(&mut req, OPERATOR_BODY_BYTES).await {
                    Ok(c) => c,
                    Err(super::cf_io::JsonError::Parse) => return error("bad_request", 400),
                    Err(e) => return error(e.code(), e.status()),
                };
                let Some(vendor) = crate::eval::gen_v2::pausable_vendor(&c.vendor) else {
                    return error("unknown_vendor", 400);
                };
                tx_reply(transaction(&storage, move |sql| {
                    let cleared = crate::eval::gen_v2::resume_vendor(sql, vendor)?;
                    Ok::<_, engine::EvalError>(j!({"vendor": vendor.code(), "cleared": cleared}))
                }))
            }
            (Method::Post, "/hold") => {
                if instance != engine::INSTANCE_LOAD {
                    return error("load_instance_only", 404);
                }
                let v: Value = match super::cf_io::request_json(&mut req, OPERATOR_BODY_BYTES).await {
                    Ok(v) => v,
                    Err(super::cf_io::JsonError::Parse) => Value::Null,
                    Err(e) => return error(e.code(), e.status()),
                };
                let hold = v["hold"] == true;
                tx_reply(transaction(&storage, move |sql| lanes::set_hold(sql, hold).map(|_| j!({"hold": hold}))))
            }
            _ => error("not_found", 404),
        }
    }

}

use crate::eval::calendar::{max_job_tasks, package_shape};

/// The documents the seal step writes for `plan`: the batch, and under
/// reward-v1 the probe and audit sets beside it (`v3::seal`).
fn seal_docs(plan: &CohortPlan) -> u64 {
    if plan.is_v3() { max_job_tasks(plan) } else { u64::from(plan.n) }
}

impl EvalCohort {
    /// One object read by a cohort step, at most `bound` bytes. A failure
    /// no wait changes (the object is over its kind's bound, or the arena
    /// has no capacity for it, or a defect) faults cohort `seq` durably, so
    /// it pauses with a code instead of retrying forever; a transport error,
    /// a `Retry` refusal or a poisoned pool is the step error `what` (the
    /// alarm's 30 s re-arm).
    async fn read(&self, seq: i64, bucket: &str, key: &str, bound: usize, what: &'static str) -> std::result::Result<Option<Vec<u8>>, &'static str> {
        match get_bounded(&self.env, bucket, key, bound).await {
            Ok(v) => Ok(v),
            Err(BodyError::TooLarge) => Err(self.fault_now(seq, "object_over_bound")),
            Err(BodyError::Refused(kind)) => match kind {
                RefusalKind::Retry | RefusalKind::Poisoned => Err(what),
                RefusalKind::Capacity | RefusalKind::Invalid => Err(self.fault_now(seq, "arena_capacity")),
                RefusalKind::Defect => Err(self.fault_now(seq, "arena_defect")),
            },
            Err(BodyError::Transport | BodyError::Timeout) => Err(what),
        }
    }

    fn fault_now(&self, seq: i64, code: &'static str) -> &'static str {
        let now = now_ms();
        let _ = transaction(&self.state.storage(), move |sql| engine::fault(sql, seq, code, now));
        code
    }

    /// A root step of cohort `seq` failed with `code`: recorded against the
    /// cohort (`engine::note_step_error`), which pauses it when no wait
    /// changes the failure. Returns the code for the step's own error.
    fn step_failed(&self, seq: i64, code: &'static str) -> &'static str {
        self.note_step_failed(seq, code);
        code
    }

    /// Records a root step's failure (`engine::note_step_error`); whether
    /// the record was written (its retry time and any fault with it).
    fn note_step_failed(&self, seq: i64, code: &'static str) -> bool {
        let now = now_ms();
        transaction(&self.state.storage(), move |sql| engine::note_step_error(sql, seq, code, now)).is_ok()
    }

    /// The engine refused a root step of cohort `seq` under its stored
    /// inputs (a plan or state refusal): the same inputs refuse it again, so
    /// the cohort pauses (`engine::refuse_step`, resumable) instead of
    /// retrying every 30 s. A storage error stays a transient step error.
    fn step_refused_now(&self, seq: i64, e: engine::EvalError) -> &'static str {
        let code = e.code();
        if matches!(e, engine::EvalError::Store(_)) {
            return code;
        }
        let now = now_ms();
        let _ = transaction(&self.state.storage(), move |sql| engine::refuse_step(sql, seq, code, now));
        code
    }

    /// A child scope of the alarm for one step of cohort `seq`, holding
    /// `bytes` of heap, admitted before the step allocates. A refusal is
    /// classed by cause: `Capacity` (no wait can hold it) and `Defect` fault
    /// the cohort durably (`arena_capacity`, `arena_defect`), so it pauses and
    /// is never skipped until the operator clears it; `Retry` and `Poisoned`
    /// are step errors (the existing 30 s re-arm).
    fn step_scope(&self, parent: &ScopeRef, route: slop_ninja_arena::Route, seq: i64, bytes: usize) -> std::result::Result<Scope, &'static str> {
        parent.child(route, bytes).map_err(|r| self.step_refused(seq, r))
    }

    /// [`step_scope`] for a shape-derived step (seal, package), admitted
    /// physically: refused (`Retry`, the 30 s re-arm) in every mode when the
    /// pool cannot serve its footprint even scattered, so a second large step
    /// of a co-resident object waits instead of exhausting linear memory
    /// and trapping the isolate mid-step.
    fn step_scope_physical(&self, parent: &ScopeRef, route: slop_ninja_arena::Route, seq: i64, bytes: usize) -> std::result::Result<Scope, &'static str> {
        parent.child_physical(route, bytes).map_err(|r| self.step_refused(seq, r))
    }

    fn step_refused(&self, seq: i64, r: Refusal) -> &'static str {
        match r.kind {
            RefusalKind::Retry => "arena_retry",
            RefusalKind::Poisoned => "arena_poisoned",
            RefusalKind::Capacity | RefusalKind::Invalid => self.fault_now(seq, "arena_capacity"),
            RefusalKind::Defect => self.fault_now(seq, "arena_defect"),
        }
    }

    /// The alarm, run inside the alarm's scope by the hand-written export
    /// (`cf_arena`): each seal and package step runs in a child scope that
    /// admits its shape-derived footprint first.
    pub(super) async fn alarm_in(&self, scope: &ScopeRef) -> Result<Response> {
        if self.schema.is_err() {
            return Response::ok("schema");
        }
        let storage = self.state.storage();
        let started = now_ms();
        let _ = storage.put("step_until_ms", (started + STEP_BUDGET_MS + 60_000) as i64).await;
        let outcome = self.step(started, scope).await;
        let _ = storage.delete("step_until_ms").await;
        match &outcome {
            Err(code) => {
                let errors: i64 = storage.get::<i64>("step_errors").await.ok().flatten().unwrap_or(0);
                let _ = storage.put("step_errors", errors + 1).await;
                let _ = storage.put("last_step_error", j!({"code": code, "at_ms": now_ms()})).await;
            }
            Ok(_) => {
                let _ = storage.delete("last_step_error").await;
            }
        }
        let now = now_ms();
        // The calendar's work counts only under this object's own
        // configuration: a retired authority opens nothing, so its step finds
        // nothing to do and never re-arms for it. Without a configuration the
        // next kick re-arms instead.
        let Ok(cfg) = self.config(&self.instance().await).await else {
            return Response::ok("step");
        };
        let again = transaction(&storage, move |sql| {
            let o = engine::observed(sql)?;
            Ok::<_, engine::EvalError>((engine::has_due_work(sql, &cfg, now)?, o.kick_ms))
        });
        if let Ok((work, kick_ms)) = again
            && work
            && now.saturating_sub(kick_ms) < HEARTBEAT_FRESH_MS
        {
            let delay = match outcome {
                Err(_) => 30_000i64,
                Ok(true) => 1_000,
                Ok(false) => SWEEP_MS,
            };
            storage.set_alarm(delay).await?;
        }
        Response::ok("step")
    }
}

impl EvalCohort {
    async fn instance(&self) -> String {
        self.state.storage().get::<String>("instance").await.ok().flatten().unwrap_or_else(|| engine::INSTANCE_MAIN.into())
    }

    /// This object's configuration under its own recorded authority and
    /// successor ([`own_config`]).
    async fn config(&self, instance: &str) -> std::result::Result<EvalConfig, &'static str> {
        let storage = self.state.storage();
        let own = storage
            .get::<String>("authority")
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| crate::config::ORIGINAL_AUTHORITY.into());
        let superseded = if instance == engine::INSTANCE_MAIN {
            transaction(&storage, crate::eval::succession::superseded_by).map_err(|_| "succession read")?
        } else {
            None
        };
        own_config(&self.env, instance, &own, superseded.as_deref())
    }

    /// Before a named authority's plan or template: every predecessor
    /// (`config::Authority::predecessors`) records this authority as its
    /// successor (`eval::succession::supersede`), each only while it holds no
    /// cohort that is not closed or abandoned, no label or number within
    /// this authority's rule, and no work left (`predecessor_work`: the kicks
    /// that continue until succession completes finish it, and the plan or
    /// template is retried). A predecessor that a successor already
    /// superseded answers with that one, which must be this authority or one
    /// it lists after that predecessor (`config::succession_reply`), so a
    /// deployment that leaves an earlier authority out of
    /// `EVAL_MAIN_PREVIOUS` is refused. Each predecessor's record is kept in
    /// this store with the first cohort it was checked for
    /// (`config::succession_recorded`): a superseded authority's cohorts
    /// never change again, but a lower first cohort asks again. The original
    /// authority, and a retired one, need nothing.
    pub(super) async fn succeed(&self, cfg: &EvalConfig) -> std::result::Result<(), &'static str> {
        let Some(req) = crate::config::Supersede::of(&cfg.authority) else { return Ok(()) };
        if cfg.authority.is_retired() {
            return Ok(());
        }
        let storage = self.state.storage();
        let mut done = succeeded(&storage).await;
        let first = cfg.authority.first_cohort();
        let body = serde_json::to_string(&req).map_err(|_| "succession json")?;
        for p in cfg.authority.predecessors() {
            if crate::config::succession_recorded(&done, p, first) {
                continue;
            }
            let mut r = authority_fetch(
                &self.env,
                engine::INSTANCE_MAIN,
                p,
                Method::Post,
                "/succession/supersede",
                Some(body.clone()),
                Some(KICK_TIMEOUT_MS),
            )
            .await
            .map_err(|_| "predecessor_unreachable")?;
            match r.status_code() {
                200 => {
                    let v = super::cf_io::response_json::<Value>(&mut r, DO_REPLY_BYTES).await.unwrap_or_default();
                    crate::config::succession_reply(&cfg.authority, p, v["superseded_by"].as_str())?;
                }
                404 => {
                    super::cf_io::discard(&mut r);
                    return Err("predecessor_unknown");
                }
                409 => {
                    let v = super::cf_io::response_json::<Value>(&mut r, DO_REPLY_BYTES).await.unwrap_or_default();
                    return Err(match v["refused"].as_str() {
                        Some("predecessor_busy") => "predecessor_busy",
                        Some("predecessor_numbers") => "predecessor_numbers",
                        Some("predecessor_work") => "predecessor_work",
                        _ => "predecessor_refused",
                    });
                }
                _ => {
                    super::cf_io::discard(&mut r);
                    return Err("predecessor_unreadable");
                }
            }
            done.retain(|(d, _)| d != p);
            done.push((p.to_owned(), first));
            storage.put(SUCCEEDED_KEY, &done).await.map_err(|_| "succession store")?;
        }
        Ok(())
    }

    /// Lane routes: one transaction each, through stubs only.
    async fn lane_route(&self, req: &mut Request, path: &str, cfg: EvalConfig, now: u64) -> Result<Response> {
        if req.method() != Method::Post {
            return error("method_not_allowed", 405);
        }
        // One admitted block of the body's exact length, refused past the
        // lane body bound (`cf_io`).
        let body = match super::cf_io::request_bytes(req, LANE_BODY_BYTES).await {
            Ok(b) => b,
            Err(e) => return error(e.code(), e.status()),
        };
        let storage = self.state.storage();
        let bad = || error("bad_internal_request", 400);
        macro_rules! parse {
            ($t:ty) => {
                match serde_json::from_slice::<$t>(&body) {
                    Ok(v) => v,
                    Err(_) => return bad(),
                }
            };
        }
        match path {
            "/lane/report" => {
                #[derive(Deserialize)]
                struct B {
                    lane: LaneRef,
                    report: LaneReport,
                }
                let b = parse!(B);
                tx_reply(transaction(&storage, move |sql| lanes::lane_report(sql, &b.lane, &b.report, now)))
            }
            "/lane/claim" | "/lane/commit" | "/lane/lost" | "/lane/release" | "/lane/renew" | "/lane/not-dispatched"
            | "/lane/give-back" | "/lane/iso-progress" | "/lane/iso-yield" | "/lane/artifact-failure"
            | "/lane/object-missing" | "/lane/object-held" => {
                let p = path.to_owned();
                let r = transaction(&storage, move |sql| lanes::scoring_route(sql, &cfg, &p, &body, now));
                // The commit that finishes scoring arms the cohort alarm at once.
                if path == "/lane/commit"
                    && let Ok(Some(v)) = &r
                    && v["Committed"]["scoring_done"] == true
                {
                    storage.set_alarm(500i64).await?;
                }
                match r {
                    Ok(Some(v)) => json(&v),
                    Ok(None) => error("not_found", 404),
                    Err(e) => tx_reply::<()>(Err(e)),
                }
            }
            "/gen/claim" => {
                #[derive(Deserialize)]
                struct B {
                    lane: LaneRef,
                    name: String,
                    grants: BTreeMap<String, u32>,
                }
                let b = parse!(B);
                tx_reply(transaction(&storage, move |sql| lanes::gen_claim(sql, &cfg, &b.lane, &b.name, &b.grants, now)))
            }
            "/gen/commit" => {
                #[derive(Deserialize)]
                struct B {
                    lane: LaneRef,
                    results: Vec<GenOutcome>,
                }
                let b = parse!(B);
                let r = transaction(&storage, move |sql| {
                    let v = lanes::gen_commit(sql, &cfg, &b.lane, &b.results, now)?;
                    let left = sql.exec("SELECT COUNT(*) FROM docs WHERE seq = ? AND status = 'pending'", &[crate::sql::SqlValue::Integer(b.lane.seq)])?;
                    Ok::<_, engine::EvalError>((v, crate::sql::int(&left[0], 0)? == 0))
                });
                if matches!(r, Ok((_, true))) {
                    storage.set_alarm(500i64).await?;
                }
                tx_reply(r.map(|x| x.0))
            }
            "/job/preds" => {
                #[derive(Deserialize)]
                struct B {
                    lane: LaneRef,
                    job: i64,
                    after: i64,
                    limit: i64,
                }
                let b = parse!(B);
                tx_reply(transaction(&storage, move |sql| engine::job_preds(sql, b.lane.seq, b.job, b.after, b.limit)))
            }
            "/job/objects" => {
                #[derive(Deserialize)]
                struct B {
                    lane: LaneRef,
                    job: i64,
                }
                let b = parse!(B);
                tx_reply(transaction(&storage, move |sql| lanes::job_objects(sql, b.lane.seq, b.job)))
            }
            "/job/receipts" => {
                #[derive(Deserialize)]
                struct B {
                    lane: LaneRef,
                    job: i64,
                    after: i64,
                    limit: i64,
                }
                let b = parse!(B);
                tx_reply(transaction(&storage, move |sql| lanes::job_receipts(sql, b.lane.seq, b.job, b.after, b.limit)))
            }
            "/lane/summary" | "/lane/bundle" => {
                #[derive(Deserialize)]
                struct S {
                    lane: LaneRef,
                    report: SummaryReport,
                }
                #[derive(Deserialize)]
                struct B {
                    lane: LaneRef,
                    report: BundleReport,
                }
                let r = if path == "/lane/summary" {
                    let b = parse!(S);
                    transaction(&storage, move |sql| lanes::summary_done(sql, &b.lane, &b.report, now))
                } else {
                    let b = parse!(B);
                    transaction(&storage, move |sql| lanes::bundle_done(sql, &b.lane, &b.report, now))
                };
                if matches!(r, Ok(true)) {
                    storage.set_alarm(1_000i64).await?;
                }
                tx_reply(r)
            }
            _ => error("not_found", 404),
        }
    }

    /// One bounded step: root steps, then one sweep. `Ok(true)` when a root step ran.
    async fn step(&self, started: u64, scope: &ScopeRef) -> std::result::Result<bool, &'static str> {
        let instance = self.instance().await;
        let cfg = self.config(&instance).await?;
        let storage = self.state.storage();
        let mut progress = false;
        while now_ms().saturating_sub(started) < STEP_BUDGET_MS {
            let now = now_ms();
            let c2 = cfg.clone();
            let work = transaction(&storage, move |sql| engine::next_work(sql, &c2, now)).map_err(|_| "next_work")?;
            let Some(work) = work else { return Ok(progress) };
            let t0 = now_ms();
            if matches!(
                work,
                Work::AdmissionVerify { .. }
                    | Work::AdmissionLanes
                    | Work::AdmissionCleanup
                    | Work::AdmissionCanary
                    | Work::AdmissionClose
                    | Work::AdmissionSweep
                    | Work::AdmissionReconcile
                    | Work::OpenNext { .. }
            ) {
                // An arena refusal faults the admitting or closing cohort the
                // step serves (none: nothing to fault).
                let adm_seq = transaction(&storage, crate::eval::admission::current_seq).unwrap_or(0);
                let child = self.step_scope(scope, crate::arena_routes::R_COHORT_ADMISSION, adm_seq, crate::arena_routes::ADMISSION_STEP_BYTES)?;
                child.scoped(self.admission_step(work, &cfg)).map_err(|_| "arena_defect")?.await?;
                let _ = child.close();
                progress = true;
                continue;
            }
            let seq = match &work {
                Work::Draw { seq, .. }
                | Work::Seal { seq }
                | Work::LoadSeal { seq, .. }
                | Work::Package { seq }
                | Work::Close { seq } => Some(*seq),
                _ => None,
            };
            let plan_of = |seq: i64| {
                transaction(&storage, move |sql| Ok::<_, engine::EvalError>(engine::cohort(sql, seq)?.plan)).map_err(|_| "cohort plan")
            };
            use crate::arena_routes::{R_COHORT_CLOSE, R_COHORT_DRAW, R_COHORT_LOAD_SEAL, R_COHORT_PACKAGE, R_COHORT_SEAL, STEP_BASE_BYTES};
            match work {
                // A cohort's root step: a failure is recorded against its
                // cohort (`engine::note_step_error`): a transient one waits
                // its retry time (a closure meanwhile yields to every other
                // step), a deterministic one pauses that cohort, resumably.
                Work::Draw { seq, plan } => {
                    let r = async {
                        // A v2 plan's draw also holds the generation objects (`gen_v2::GEN_DRAW_BYTES`).
                        let bytes = STEP_BASE_BYTES + if plan.generation.is_some() { crate::eval::gen_v2::GEN_DRAW_BYTES } else { 0 };
                        let child = self.step_scope(scope, R_COHORT_DRAW, seq, bytes)?;
                        let fut = async { if plan.is_v3() { self.draw_v3(&cfg, seq, &plan).await } else { self.draw(&cfg, seq, &plan).await } };
                        child.scoped(fut).map_err(|_| "arena_defect")?.await?;
                        let _ = child.close();
                        Ok::<(), &'static str>(())
                    }
                    .await;
                    r.map_err(|code| self.step_failed(seq, code))?;
                }
                Work::Seal { seq } => {
                    let r = async {
                        let plan = plan_of(seq)?;
                        let child = self.step_scope_physical(scope, R_COHORT_SEAL, seq, crate::arena_routes::seal_bytes(seal_docs(&plan)))?;
                        let fut = async { if plan.is_v3() { self.seal_v3(&cfg, seq, &plan).await } else { self.seal(&cfg, seq).await } };
                        child.scoped(fut).map_err(|_| "arena_defect")?.await?;
                        let _ = child.close();
                        Ok::<(), &'static str>(())
                    }
                    .await;
                    r.map_err(|code| self.step_failed(seq, code))?;
                }
                Work::LoadSeal { seq, source } => {
                    let r = async {
                        let child = self.step_scope(scope, R_COHORT_LOAD_SEAL, seq, STEP_BASE_BYTES)?;
                        child.scoped(self.load_seal(&cfg, seq, &source)).map_err(|_| "arena_defect")?.await?;
                        let _ = child.close();
                        Ok::<(), &'static str>(())
                    }
                    .await;
                    r.map_err(|code| self.step_failed(seq, code))?;
                }
                Work::Package { seq } => {
                    let r = async {
                        let plan = plan_of(seq)?;
                        let (jobs, docs) = package_shape(&plan);
                        let child = self.step_scope_physical(scope, R_COHORT_PACKAGE, seq, crate::arena_routes::package_step_bytes(jobs, docs))?;
                        let fut = async { if plan.is_v3() { self.package_v3(&cfg, seq, &plan).await } else { self.package(&cfg, seq, &instance, &plan).await } };
                        child.scoped(fut).map_err(|_| "arena_defect")?.await?;
                        let _ = child.close();
                        Ok::<(), &'static str>(())
                    }
                    .await;
                    r.map_err(|code| self.step_failed(seq, code))?;
                }
                Work::Close { seq } => {
                    let r = async {
                        let child = self.step_scope(scope, R_COHORT_CLOSE, seq, STEP_BASE_BYTES)?;
                        child.scoped(self.close_v3(&cfg, seq)).map_err(|_| "arena_defect")?.await?;
                        let _ = child.close();
                        Ok::<(), &'static str>(())
                    }
                    .await;
                    if let Err(code) = r {
                        // The closure waits out its retry time (or its fault)
                        // and this step goes on with the rest of the work:
                        // a closure that cannot be written holds nothing up.
                        // A failure that could not be recorded has no retry
                        // time, so the same closure would be due again at
                        // once: the step ends and the alarm's re-arm waits.
                        if !self.note_step_failed(seq, code) {
                            return Err(code);
                        }
                        continue;
                    }
                }
                // A failed bookkeeping step waits out its retry time
                // (`engine::bookkeeping_failed`), so it never holds other
                // work, the outage probe included.
                Work::Settle { seq, key, microusd, vendors } => {
                    // A ledger outage only delays the charge; evaluation never waits on it.
                    // Each frontier vendor's generation spend first, under its own key
                    // (every settle is idempotent per key), then the cohort's own.
                    let mut failed = false;
                    for (vkey, vmicro) in &vendors {
                        if ledger_call(&self.env, "/settle", &j!({"key": vkey, "microusd": vmicro, "note": "vendor generation spend"})).await.is_err() {
                            failed = true;
                            break;
                        }
                    }
                    if failed || ledger_call(&self.env, "/settle", &j!({"key": key, "microusd": microusd, "note": "cohort spend"})).await.is_err() {
                        bookkeeping_failed(&storage, engine::BOOKKEEPING_SETTLE, "ledger_settle");
                        return Ok(progress);
                    }
                    transaction(&storage, move |sql| {
                        lanes::mark_settled(sql, seq, microusd)?;
                        engine::bookkeeping_done(sql, engine::BOOKKEEPING_SETTLE)
                    })
                    .map_err(|_| "settle commit")?;
                }
                Work::AdmissionSettle { key, microusd } => {
                    // As for a cohort's spend: a ledger outage only delays it.
                    if ledger_call(&self.env, "/settle", &j!({"key": key, "microusd": microusd, "note": "admission run"})).await.is_err() {
                        bookkeeping_failed(&storage, engine::BOOKKEEPING_CHARGE, "ledger_settle");
                        return Ok(progress);
                    }
                    transaction(&storage, move |sql| {
                        crate::eval::admission::charge_settled(sql, &key)?;
                        engine::bookkeeping_done(sql, engine::BOOKKEEPING_CHARGE)
                    })
                    .map_err(|_| "charge commit")?;
                }
                Work::Purge { seq, prefixes } => {
                    // Private per-document predictions leave the eval bucket at close.
                    if self.purge(&prefixes).await.is_err() {
                        bookkeeping_failed(&storage, engine::BOOKKEEPING_PURGE, "purge_delete");
                        return Ok(progress);
                    }
                    let now = now_ms();
                    transaction(&storage, move |sql| {
                        lanes::mark_purged(sql, seq, now)?;
                        engine::bookkeeping_done(sql, engine::BOOKKEEPING_PURGE)
                    })
                    .map_err(|_| "purge commit")?;
                }
                Work::RetireReceipts { seq, prefix, keys, marker } => {
                    // Detailed receipts leave the receipts bucket under the
                    // N+2 rule (`eval::retention`). The current authorization
                    // is read first: a cohort it names goes back to waiting.
                    // A failure is recorded and retried at a later step;
                    // nothing else waits on it.
                    let now = now_ms();
                    let note = |code: &'static str| {
                        let _ = transaction(&storage, move |sql| {
                            crate::eval::retention::note_error(sql, code, now)?;
                            engine::bookkeeping_failed(sql, engine::BOOKKEEPING_RETENTION, code, now)
                        });
                    };
                    let hold = match self.authorization_hold(&cfg.chain).await {
                        Ok(h) => h,
                        Err(code) => {
                            note(code);
                            return Ok(progress);
                        }
                    };
                    let due = transaction(&storage, move |sql| crate::eval::retention::apply_hold(sql, &hold, seq, now))
                        .map_err(|_| "retention commit")?;
                    if !due {
                        progress = true;
                        continue;
                    }
                    if let Some((key, body)) = marker {
                        if put(&self.env, RECEIPTS_BUCKET, &key, body.into_bytes()).await.is_err() {
                            note("retention_marker");
                            return Ok(progress);
                        }
                        transaction(&storage, move |sql| crate::eval::retention::note_retiring(sql, seq, now))
                            .map_err(|_| "retention commit")?;
                    }
                    let deleted = match self.retire_page(&prefix, &keys).await {
                        Ok(d) => d,
                        Err(code) => {
                            note(code);
                            return Ok(progress);
                        }
                    };
                    let r = match deleted {
                        Some(n) => transaction(&storage, move |sql| {
                            crate::eval::retention::note_page(sql, seq, n)?;
                            crate::eval::retention::clear_error(sql)?;
                            engine::bookkeeping_done(sql, engine::BOOKKEEPING_RETENTION)
                        }),
                        None => transaction(&storage, move |sql| {
                            crate::eval::retention::finish(sql, seq, crate::eval::retention::PAGE, now)?;
                            crate::eval::retention::clear_error(sql)?;
                            engine::bookkeeping_done(sql, engine::BOOKKEEPING_RETENTION)
                        }),
                    };
                    r.map_err(|_| "retention commit")?;
                }
                Work::RetentionNotice { predecessor, after_cohort } => {
                    // Retried after its retry time until the predecessor
                    // reports nothing left (its alarm re-arms only while kicked).
                    if !self.retention_notice(&predecessor, after_cohort).await {
                        bookkeeping_failed(&storage, engine::BOOKKEEPING_RETENTION, "retention_notice");
                        return Ok(progress);
                    }
                    let now = now_ms();
                    transaction(&storage, move |sql| crate::eval::retention::record_notice(sql, &predecessor, after_cohort, now))
                        .map_err(|_| "notice commit")?;
                }
                Work::AdmissionVerify { .. }
                | Work::AdmissionLanes
                | Work::AdmissionCleanup
                | Work::AdmissionCanary
                | Work::AdmissionClose
                | Work::AdmissionSweep
                | Work::AdmissionReconcile
                | Work::OpenNext { .. } => {}
                Work::Probe { seq, code, probe } => {
                    // One bounded probe of a paused cohort's outage. Its start
                    // (the next probe's time, the bound charged if no outcome
                    // is recorded) is durable before the call; its outcome and
                    // spend are recorded either way, and a success resumes
                    // the same cohort (`engine::probe_done`).
                    // The call is bounded by the ticket's deadline, and the
                    // step's lease (`step_until_ms`, which a kick reads before
                    // it arms an alarm) is raised to cover it, so no kick
                    // schedules a step that overlaps this one.
                    let now = now_ms();
                    let p2 = probe.clone();
                    let ticket = transaction(&storage, move |sql| engine::probe_begin(sql, seq, &p2, now)).map_err(|_| "probe begin")?;
                    let lease = ticket.until_ms.saturating_add(60_000) as i64;
                    let held: i64 = storage.get::<i64>("step_until_ms").await.ok().flatten().unwrap_or(0);
                    if lease > held && storage.put("step_until_ms", lease).await.is_err() {
                        return Ok(progress);
                    }
                    let outcome = self.run_probe(seq, &probe, &ticket).await;
                    let now = now_ms();
                    let resumed = transaction(&storage, move |sql| engine::probe_done(sql, seq, &code, &ticket, &outcome, now))
                        .map_err(|_| "probe commit")?;
                    if !resumed {
                        return Ok(progress);
                    }
                }
                Work::AbandonMarker { seq } => {
                    // The signed, content-free abandonment record. A failure
                    // waits out its retry time and this step goes on with the
                    // rest of the work: the record holds nothing up.
                    let r = async {
                        let c2 = cfg.clone();
                        let (key, record) = transaction(&storage, move |sql| engine::abandonment_record(sql, &c2, seq))
                            .map_err(|_| "abandonment record")?;
                        let (package_key, _) = keys(&self.env).map_err(|_| "package key")?;
                        let bytes = package::signed_bytes(record, &package_key).map_err(|_| "abandonment record signing")?;
                        put(&self.env, RECEIPTS_BUCKET, &key, bytes).await.map_err(|_| "abandonment record put")?;
                        Ok::<(), &'static str>(())
                    }
                    .await;
                    let now = now_ms();
                    let recorded = match r {
                        Ok(()) => transaction(&storage, move |sql| engine::abandon_marker_written(sql, seq, now)),
                        Err(code) => transaction(&storage, move |sql| engine::abandon_marker_failed(sql, seq, code, now)),
                    };
                    // A result that could not be recorded would make the same
                    // record due again at once: the step ends and the alarm's
                    // re-arm waits.
                    if recorded.is_err() {
                        return Ok(progress);
                    }
                }
                Work::Dispatch { seq, kicks, stop } => {
                    let faults = transaction(&storage, move |sql| {
                        Ok::<_, engine::EvalError>(engine::cohort(sql, seq)?.plan.load.map(|l| l.faults))
                    })
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                    let n = kicks.len();
                    let sent = send_kicks(&self.env, kicks, faults.drop_kick_permille, faults.duplicate_kicks).await;
                    stop_lanes(&self.env, &stop).await;
                    if n > 0 || !stop.is_empty() {
                        let _ = storage.put("last_dispatch", j!({"at_ms": now_ms(), "sent": sent, "stopped": stop.len()})).await;
                    }
                    return Ok(progress);
                }
            }
            progress = true;
            if let Some(seq) = seq {
                let ms = now_ms() - t0;
                let _ = transaction(&storage, move |sql| engine::note_step(sql, seq, ms));
            }
        }
        Ok(progress)
    }

    /// Runs one outage probe ([`engine::Probe`]) within its ticket's
    /// deadline ([`engine::ProbeTicket`]): whether it succeeded,
    /// whether the scorer refused the artifact definitively, its wall time,
    /// its measured spend (nano-dollars) and a short content-free code for a
    /// failure. Never reads cohort text and never touches a job.
    async fn run_probe(&self, seq: i64, probe: &engine::Probe, ticket: &engine::ProbeTicket) -> engine::ProbeOutcome {
        let t0 = now_ms();
        match probe {
            engine::Probe::Generation { item, timeout_ms } => {
                let (g, _) = ai_generate(&self.env, item, *timeout_ms).await;
                let micro = textgen::cost_microusd(&item.generator, g.prompt_tokens, g.completion_tokens);
                let ok = engine::probe_answered(&g);
                let note = match &g.output {
                    _ if ok => "ok",
                    Err(e) => e.code(),
                    Ok(_) => "empty",
                };
                engine::ProbeOutcome { ok, definitive: false, ms: now_ms() - t0, nanousd: micro.saturating_mul(1_000), note: note.into() }
            }
            engine::Probe::Scorer { lane, body, admit } => {
                let path = if *admit { "/admit" } else { "/run" };
                // The platform aborts the call and its reply at the ticket's
                // deadline (`AbortSignal.timeout`, `cf_io::internal_request`):
                // a probe that runs that long is a failed probe.
                let left = ticket.until_ms.saturating_sub(now_ms()).max(1);
                let body = body.to_string();
                let r = async {
                    let stub = lane_stub(&self.env, "score", lane).ok()?;
                    let req = super::cf_io::internal_request(&format!("{INTERNAL}{path}"), Method::Post, Some(&body), Some(left)).ok()?;
                    let mut resp = stub.fetch_with_request(req).await.ok()?;
                    let status = resp.status_code();
                    let v: Value = super::cf_io::response_json(&mut resp, 64 * 1024).await.unwrap_or(Value::Null);
                    Some((status, v))
                }
                .await;
                // The probe's instance served one control document: stop it now.
                if let (Ok(stub), Ok(req)) = (
                    lane_stub(&self.env, "score", lane),
                    super::cf_io::internal_request(&format!("{INTERNAL}/stop"), Method::Post, None, Some(KICK_TIMEOUT_MS)),
                ) {
                    let _ = stub.fetch_with_request(req).await;
                }
                let ms = now_ms() - t0;
                // The scorer instance's and the lane object's time, at the
                // cohort's own rates.
                let rates = transaction(&self.state.storage(), move |sql| {
                    Ok::<_, engine::EvalError>(engine::cohort(sql, seq)?.plan.fanout.map(|f| f.cost))
                })
                .ok()
                .flatten();
                let nanousd = rates.map_or(0, |c| u64::try_from(crate::eval::cost::lane_cost_nanousd(ms, 0, ms, &c)).unwrap_or(u64::MAX));
                let (status, v) = match r {
                    Some((st, v)) => (Some(st), v),
                    None => (None, Value::Null),
                };
                use crate::eval::admission::{LaneOutcome, classify_admit};
                let (ok, definitive, note) = if *admit {
                    match classify_admit(status, &v) {
                        LaneOutcome::Admitted => (true, false, "ok".to_owned()),
                        // A deterministic refusal that every fresh instance repeats.
                        LaneOutcome::Refused(code) => (false, true, format!("definitive_refusal:{}", crate::eval::lanes::textgen_safe(&code))),
                        LaneOutcome::ArtifactFailure(code) | LaneOutcome::Platform(code) => (false, false, crate::eval::lanes::textgen_safe(&code)),
                    }
                } else {
                    let ok = status == Some(200)
                        && v["results"].as_array().is_some_and(|rs| rs.iter().any(|x| x["id"] == "0" && x["status"] == "ok"));
                    (ok, false, if ok { "ok" } else if status.is_none() { "lane_unreachable" } else { "control_not_scored" }.to_owned())
                };
                let note = if !ok && status.is_none() && now_ms() >= ticket.until_ms { "probe_timeout".to_owned() } else { note };
                engine::ProbeOutcome { ok, definitive, ms, nanousd, note }
            }
        }
    }

    /// Deletes one listing page of detailed receipt objects under `prefix` in
    /// the receipts bucket: `Some(n)` keys deleted, or `None` once the listing
    /// is empty and the single `keys` are deleted too (a missing key is a
    /// successful delete).
    async fn retire_page(&self, prefix: &str, keys: &[String]) -> std::result::Result<Option<u64>, &'static str> {
        let bucket = self.env.bucket(RECEIPTS_BUCKET).map_err(|_| "receipts_bucket")?;
        let listed = bucket
            .list()
            .prefix(prefix.to_owned())
            .limit(crate::eval::retention::PAGE as u32)
            .execute()
            .await
            .map_err(|_| "retention_list")?;
        let page: Vec<String> = listed.objects().iter().map(|o| o.key()).collect();
        if !page.is_empty() {
            let n = page.len() as u64;
            bucket.delete_multiple(page).await.map_err(|_| "retention_delete")?;
            return Ok(Some(n));
        }
        for k in keys {
            bucket.delete(k).await.map_err(|_| "retention_delete")?;
        }
        Ok(None)
    }

    /// What the current authorization names (`eval::retention::Hold`), from
    /// the private pointer in the receipts bucket and the object it names,
    /// each read within its bound (together at most 5 KiB, inside the
    /// alarm's base footprint, `arena_routes::ALARM_CONTROL_BYTES`). No
    /// pointer holds nothing; one that cannot be read or parsed is an error,
    /// so nothing is deleted until it can.
    async fn authorization_hold(
        &self,
        chain: &slop_ninja_protocol::ChainBinding,
    ) -> std::result::Result<crate::eval::retention::Hold, &'static str> {
        use crate::eval::retention::contract::{AUTHORIZATION_OBJECT_BOUND, AUTHORIZATION_POINTER_BOUND, AUTHORIZATION_POINTER_KEY};
        use crate::eval::retention::{Hold, hold_of, pointer_ref};
        let Some(bytes) = get_bounded(&self.env, RECEIPTS_BUCKET, AUTHORIZATION_POINTER_KEY, AUTHORIZATION_POINTER_BOUND as usize)
            .await
            .map_err(|_| "authorization_pointer")?
        else {
            return Ok(Hold::None);
        };
        let (kind, r) = pointer_ref(&bytes, chain)?;
        let object = get_bounded(&self.env, RECEIPTS_BUCKET, &r.path, AUTHORIZATION_OBJECT_BOUND as usize)
            .await
            .map_err(|_| "authorization_object")?
            .ok_or("authorization_object")?;
        hold_of(&kind, &r, &object)
    }

    /// Tells predecessor `predecessor` that `after_cohort` is this authority's
    /// second completed cohort (`/succession/retention`). `true` once it
    /// answers with nothing left to delete; any other answer is retried.
    async fn retention_notice(&self, predecessor: &str, after_cohort: u64) -> bool {
        let body = j!({"after_cohort": after_cohort}).to_string();
        let Ok(mut r) = authority_fetch(
            &self.env,
            engine::INSTANCE_MAIN,
            predecessor,
            Method::Post,
            "/succession/retention",
            Some(body),
            Some(KICK_TIMEOUT_MS),
        )
        .await
        else {
            return false;
        };
        if r.status_code() != 200 {
            super::cf_io::discard(&mut r);
            return false;
        }
        let v = super::cf_io::response_json::<Value>(&mut r, DO_REPLY_BYTES).await.unwrap_or_default();
        v["remaining"].as_u64() == Some(0)
    }

    /// Deletes every object under `prefixes` in the eval bucket (deletes are free).
    async fn purge(&self, prefixes: &[String]) -> std::result::Result<u64, &'static str> {
        let bucket = self.env.bucket(EVAL_BUCKET).map_err(|_| "eval bucket")?;
        let mut deleted = 0;
        for prefix in prefixes {
            loop {
                let listed = bucket.list().prefix(prefix.clone()).limit(1000).execute().await.map_err(|_| "purge list")?;
                let keys: Vec<String> = listed.objects().iter().map(|o| o.key()).collect();
                if keys.is_empty() {
                    break;
                }
                deleted += keys.len() as u64;
                bucket.delete_multiple(keys).await.map_err(|_| "purge delete")?;
                if !listed.truncated() {
                    break;
                }
            }
        }
        Ok(deleted)
    }

    async fn draw(&self, cfg: &EvalConfig, seq: i64, plan: &CohortPlan) -> std::result::Result<(), &'static str> {
        let env = &self.env;
        let mbytes = self.read(seq, EVAL_BUCKET, &plan.pool.manifest_key, STEP_OBJECT_BYTES, "pool manifest read").await?.ok_or("pool manifest missing")?;
        if sha_hex(&mbytes) != plan.pool.manifest_sha256 {
            return Err("pool manifest digest");
        }
        let manifest = PoolManifest::parse(&mbytes, &plan.pool.pool_id).map_err(|e| e.0)?;
        let storage = self.state.storage();
        let dir = plan.pool.manifest_key.rsplit_once('/').map_or("", |x| x.0).to_owned();
        let authority = self.state.id().to_string();
        let entries = read_ledger(self, seq, &dir, &plan.pool.pool_id).await?;
        let mut ledger = pool::Used::default();
        ledger.extend_from_ledger(&entries, &authority, &plan.label);
        let (mut used_rows, mut used_groups) = transaction(&storage, engine::used_set).map_err(|_| "used set")?;
        used_rows.extend(ledger.row_ids(&manifest));
        used_groups.extend(ledger.groups.iter().cloned());
        let seed = engine::cohort_seed(&cfg.private_seed, &plan.label);
        let d = pool::draw(&manifest, &used_rows, &used_groups, u64::from(plan.counts()[0]), &seed);
        let shards: std::collections::BTreeSet<u32> = d.rows.iter().map(|r| r.shard).collect();
        let mut texts = BTreeMap::new();
        for k in shards {
            let info = manifest.shards.iter().find(|s| s.index == k).ok_or("shard info")?;
            let body = self.read(seq, EVAL_BUCKET, &format!("{dir}/{}", info.file), STEP_OBJECT_BYTES, "pool shard read").await?.ok_or("pool shard missing")?;
            if sha_hex(&body) != info.sha256 {
                return Err("pool shard digest");
            }
            for line in std::str::from_utf8(&body).map_err(|_| "shard utf-8")?.lines() {
                let r: ShardRow = serde_json::from_str(line).map_err(|_| "shard row")?;
                if r.schema == pool::SHARD_SCHEMA && sha_hex(r.text.as_bytes()) == r.text_sha256 {
                    texts.insert(r.row_id, r.text);
                }
            }
        }
        let wanted: BTreeMap<String, String> =
            d.rows.iter().filter_map(|r| texts.remove(&r.row_id).map(|t| (r.row_id.clone(), t))).collect();
        // Write-ahead: the ledger entry is durable before the draw commits.
        let plan_sha = transaction(&storage, move |sql| engine::plan_sha256(sql, seq)).map_err(|_| "plan digest")?;
        let entry = pool::ledger_entry(&plan.pool.pool_id, plan.cohort, &plan.label, &authority, &plan_sha, &d);
        let entry_key = ledger_key(&dir, &plan.label, &authority);
        put(env, EVAL_BUCKET, &entry_key, serde_json::to_vec_pretty(&entry).map_err(|_| "ledger entry")?)
            .await
            .map_err(|_| "used ledger write")?;
        let c2 = cfg.clone();
        let now = now_ms();
        let drawn = transaction(&storage, move |sql| engine::apply_draw(sql, &c2, seq, &d, &wanted, &ledger, now))
            .map_err(|e| match e {
                TxError::Operation(e) => self.step_refused_now(seq, e),
                TxError::Runtime => "draw commit",
            })?;
        if !drawn && let Ok(b) = env.bucket(EVAL_BUCKET) {
            let _ = b.delete(entry_key).await;
        }
        Ok(())
    }

    async fn seal(&self, cfg: &EvalConfig, seq: i64) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let c2 = cfg.clone();
        let b = transaction(&storage, move |sql| engine::prepare_seal(sql, &c2, seq)).map_err(|_| "prepare seal")?;
        let (package_key, _) = keys(&self.env)?;
        let label = b.manifest.cohort_label.clone();
        // The shards go up to 6 at a time.
        for group in b.shards.chunks(PARALLEL) {
            let r = join_all(group.iter().map(|(key, body)| put(&self.env, EVAL_BUCKET, key, body.clone()))).await;
            if r.iter().any(|x| x.is_err()) {
                return Err("batch shard put");
            }
        }
        put(&self.env, EVAL_BUCKET, &b.index_key, b.index.clone()).await.map_err(|_| "batch index put")?;
        let signed = package::signed_bytes(b.manifest.clone(), &package_key).map_err(|e| e.0)?;
        put(&self.env, RECEIPTS_BUCKET, &format!("{}/manifest.signed", engine::receipt_prefix(&label)), signed)
            .await
            .map_err(|_| "manifest put")?;
        let puts = b.shards.len() as u64 + 2;
        let now = now_ms();
        transaction(&storage, move |sql| engine::apply_seal(sql, seq, &b, puts, now)).map_err(|_| "seal commit")?;
        Ok(())
    }

    async fn load_seal(&self, cfg: &EvalConfig, seq: i64, source: &LoadSource) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let index = self.read(seq, EVAL_BUCKET, &source.index_key, STEP_OBJECT_BYTES, "source index read").await?.ok_or("source index missing")?;
        let c2 = cfg.clone();
        let b = transaction(&storage, move |sql| engine::prepare_load_seal(sql, &c2, seq, &index)).map_err(|e| match e {
            TxError::Operation(e) => self.step_refused_now(seq, e),
            TxError::Runtime => "prepare load seal",
        })?;
        let (package_key, _) = load_keys(&self.env)?;
        let signed = package::signed_bytes(b.manifest.clone(), &package_key).map_err(|e| e.0)?;
        put(&self.env, RECEIPTS_BUCKET, &format!("{}/manifest.signed", engine::receipt_prefix(&b.manifest.cohort_label)), signed)
            .await
            .map_err(|_| "manifest put")?;
        let c3 = cfg.clone();
        let now = now_ms();
        transaction(&storage, move |sql| engine::apply_load_seal(sql, &c3, seq, &b, now)).map_err(|_| "load seal commit")?;
        Ok(())
    }

    async fn package(&self, cfg: &EvalConfig, seq: i64, instance: &str, plan: &CohortPlan) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let bound = summary_bytes(max_job_tasks(plan));
        // Every summary object, checked against the digest its lane reported.
        let refs: Vec<(String, String)> = transaction(&storage, move |sql| {
            Ok::<_, engine::EvalError>(
                sql.exec("SELECT summary_key, summary_sha256 FROM jobs WHERE seq = ? ORDER BY job", &[crate::sql::SqlValue::Integer(seq)])?
                    .iter()
                    .map(|r| (crate::sql::text(r, 0).unwrap_or("").to_owned(), crate::sql::text(r, 1).unwrap_or("").to_owned()))
                    .collect::<Vec<_>>(),
            )
        })
        .map_err(|_| "summary refs")?;
        let mut summaries = Vec::new();
        for group in refs.chunks(PARALLEL) {
            for r in join_all(group.iter().map(|(k, sha)| async move {
                let b = self.read(seq, EVAL_BUCKET, k, bound, "summary read").await?.ok_or("summary missing")?;
                if &sha_hex(&b) != sha {
                    return Err("summary digest");
                }
                serde_json::from_slice::<engine::JobSummary>(&b).map_err(|_| "summary json")
            }))
            .await
            {
                summaries.push(r?);
            }
        }
        let c2 = cfg.clone();
        let b = transaction(&storage, move |sql| engine::prepare_package(sql, &c2, seq, &summaries)).map_err(|e| match e {
            TxError::Operation(e) => self.step_refused_now(seq, e),
            TxError::Runtime => "prepare package",
        })?;
        let (package_key, receipt_key) = instance_keys(&self.env, instance)?;
        let prefix = engine::receipt_prefix(&b.manifest.cohort_label);
        let objects = [
            ("receipt-index.signed", package::signed_bytes(b.index.clone(), &receipt_key).map_err(|e| e.0)?),
            ("package.signed", package::signed_bytes(b.package.clone(), &package_key).map_err(|e| e.0)?),
            ("target.signed", package::signed_bytes(b.target.clone(), &package_key).map_err(|e| e.0)?),
            ("chunk-map.json", serde_json::to_vec_pretty(&b.chunk_map).unwrap_or_default()),
            ("components.json", serde_json::to_vec_pretty(&b.components).unwrap_or_default()),
            (
                "keys.json",
                serde_json::to_vec_pretty(&j!({
                    "instance": instance,
                    "package_public_key": hex::encode(package_key.verifying_key().to_bytes()),
                    "receipt_public_key": hex::encode(receipt_key.verifying_key().to_bytes()),
                    "package_digest": b.package.digest().map(|d| d.to_string()).unwrap_or_default(),
                    "target_digest": b.target.digest().map(|d| d.to_string()).unwrap_or_default(),
                }))
                .unwrap_or_default(),
            ),
        ];
        let puts = objects.len() as u64;
        for (name, bytes) in objects {
            put(&self.env, RECEIPTS_BUCKET, &format!("{prefix}/{name}"), bytes).await.map_err(|_| "package put")?;
        }
        let now = now_ms();
        transaction(&storage, move |sql| engine::commit_package(sql, seq, &b, puts, now)).map_err(|_| "package commit")?;
        Ok(())
    }
}

// --------------------------------------------------------------- Workers AI

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct AiInput<'a> {
    messages: Vec<Message<'a>>,
    max_tokens: u32,
    temperature: f64,
    top_p: f64,
}

fn ai_text(v: &Value) -> Option<String> {
    if let Some(s) = v["response"].as_str() {
        return Some(s.to_owned());
    }
    v["choices"][0]["message"]["content"].as_str().map(str::to_owned)
}

/// One generation call, bounded by `timeout_ms`. Returns the outcome and, for a
/// failure, its (content-free) error text for the limiter's records.
pub async fn ai_generate(env: &Env, it: &GenItem, timeout_ms: u64) -> (GenOutcome, Option<String>) {
    let t0 = now_ms();
    let input = AiInput {
        messages: vec![
            Message { role: "system", content: &it.prompt.system },
            Message { role: "user", content: &it.prompt.user },
        ],
        max_tokens: it.prompt.max_tokens,
        temperature: textgen::TEMPERATURE,
        top_p: textgen::TOP_P,
    };
    // The run is raced against `timeout_ms` in JS (`cf_io::settle`): the
    // awaited promise always settles, so no pending future is dropped
    // (design section 2.6); a late reply is discarded by JS.
    let deadline = now_ms().saturating_add(timeout_ms);
    let reply: std::result::Result<Value, String> = async {
        use wasm_bindgen::JsCast;
        let ai = js_sys::Reflect::get(env.as_ref(), &JsValue::from_str("AI")).ok().filter(|v| !v.is_undefined()).ok_or_else(|| "Env does not contain binding `AI`".to_owned())?;
        let run: js_sys::Function = js_sys::Reflect::get(&ai, &JsValue::from_str("run")).ok().and_then(|f| f.dyn_into().ok()).ok_or_else(|| "ai binding".to_owned())?;
        let input = serde_json::to_string(&input).map_err(|e| e.to_string())?;
        let input = js_sys::JSON::parse(&input).map_err(|_| "ai input".to_owned())?;
        let promise: js_sys::Promise = match run.call2(&ai, &JsValue::from_str(&it.generator), &input) {
            Ok(p) => p.dyn_into().map_err(|_| "ai run".to_owned())?,
            Err(e) => return Err(worker::Error::from(e).to_string()),
        };
        match super::cf_io::settle(promise, Some(deadline)).await {
            super::cf_io::Settled::Value(v) => {
                let text: String = js_sys::JSON::stringify(&v).map(String::from).map_err(|_| "ai reply".to_owned())?;
                serde_json::from_str::<Value>(&text).map_err(|e| e.to_string())
            }
            super::cf_io::Settled::Error(e) => Err(worker::Error::from(e).to_string()),
            super::cf_io::Settled::Timeout => Err("timeout".to_owned()),
        }
    }
    .await;
    let (output, pt, ct, err) = match reply {
        Ok(v) => {
            let usage = &v["usage"];
            let pt = usage["prompt_tokens"].as_u64().unwrap_or(0);
            let ct = usage["completion_tokens"].as_u64().unwrap_or(0);
            (ai_text(&v).ok_or(Reject::Empty), pt, ct, None)
        }
        Err(e) => (Err(textgen::classify_ai_error(&e)), 0, 0, Some(crate::eval::limiter::sanitize(&e))),
    };
    (
        GenOutcome { idx: it.idx, generator: it.generator.clone(), output, prompt_tokens: pt, completion_tokens: ct, ms: now_ms() - t0, claim: it.claim, reply: None, effort: None },
        err,
    )
}

/// Operator pre-flight: one generation on a fixed synthetic passage. The output is
/// synthetic and returned to the operator only.
pub async fn probe(env: &Env, model: &str) -> Value {
    let passage = "The committee met on Tuesday to review the budget for the new library wing. \
Members asked about heating costs and the timeline for the roof repairs.";
    let mut out = Vec::new();
    for (kind, prompt) in [
        ("mixed", textgen::rewrite_prompt("wikinews", passage)),
        ("model_only", textgen::scratch_prompt("fineweb", "restoring an old bicycle", 150)),
    ] {
        let it = GenItem { idx: 0, kind: kind.into(), generator: model.into(), prompt, attempt: 0, claim: 0, call: None };
        let (g, err) = ai_generate(env, &it, 120_000).await;
        let verdict = match (&g.output, kind) {
            (Ok(t), "mixed") => textgen::validate_rewrite(passage, t).map(|_| "accepted"),
            (Ok(t), _) => textgen::validate_scratch(t, 150).map(|_| "accepted"),
            (Err(e), _) => Err(*e),
        };
        out.push(j!({"kind": kind, "verdict": verdict.map_err(|e| e.code()), "ms": g.ms,
            "prompt_tokens": g.prompt_tokens, "completion_tokens": g.completion_tokens, "error": err,
            "cost_microusd": textgen::cost_microusd(model, g.prompt_tokens, g.completion_tokens),
            "synthetic_output": g.output.ok()}));
    }
    j!({"model": model, "probes": out})
}

/// M1: whether `n` concurrent operations of `kind` from one invocation wait on
/// the 6-connection limit. `ai`: tiny synthetic generations; `do`: `n`
/// distinct Durable Objects that each sleep `ms`; `r2`: concurrent object
/// reads. Each operation's start and end offsets are returned.
pub async fn probe_connections(env: &Env, kind: &str, n: usize, model: &str, ms: u64, max_tokens: u32) -> Value {
    let t0 = now_ms();
    let n = n.clamp(1, 24);
    let ops = (0..n).map(|k| async move {
        let s = now_ms() - t0;
        let ok = match kind {
            "ai" => {
                let it = GenItem {
                    idx: k as i64,
                    kind: "probe".into(),
                    generator: model.into(),
                    prompt: textgen::Prompt {
                        system: "You are a writer. Reply with plain prose only.".into(),
                        user: format!("Probe {k}: write a few sentences about a quiet harbour town at dawn."),
                        max_tokens,
                    },
                    attempt: 0,
                    claim: 0,
                    call: None,
                };
                let (g, err) = ai_generate(env, &it, 60_000).await;
                j!({"ok": g.output.is_ok(), "error": err, "completion_tokens": g.completion_tokens})
            }
            "do" => {
                let name = format!("eval-lane/{}/probe/sleep-{k:02}", crate::config::NETWORK);
                let r = async {
                    let stub = lane_stub(env, "gen", &name).ok()?;
                    let resp = stub.fetch_with_request(internal_request(Method::Post, &format!("/sleep?ms={ms}"), None).ok()?).await.ok()?;
                    Some(resp.status_code())
                }
                .await;
                j!({"ok": r == Some(200)})
            }
            _ => {
                let r = get_bytes(env, EVAL_BUCKET, "artifacts/packages/probe-absent").await;
                j!({"ok": r.is_ok()})
            }
        };
        j!({"k": k, "start_ms": s, "end_ms": now_ms() - t0, "result": ok})
    });
    let results = join_all(ops).await;
    j!({"kind": kind, "n": n, "model": model, "total_ms": now_ms() - t0, "ops": results})
}

// ------------------------------------------------------------------ scoring probe

/// Operator pre-flight of the scorer path on synthetic documents, through the
/// probe lane `a-scorer/testnet/probe` (never a cohort lane). `spec_sha256`
/// is the digest of the artifact spec the probe's load names, as a cohort
/// naming that spec loads (`None`: the scorer's unnamed-load spec, v2);
/// `spans` asks the scorer for each document's sentences and spans.
pub async fn scorer_probe(
    env: &Env,
    package_sha256: &str,
    manifest_key: &str,
    spec_sha256: Option<&str>,
    spans: bool,
) -> Value {
    let name = format!("a-scorer/{}/probe", crate::config::NETWORK);
    let mut body = j!({"package_sha256": package_sha256, "manifest_key": manifest_key, "docs": [
        {"id": "0", "text": crate::eval::lane_logic::CONTROL_TEXT},
        {"id": "1", "text": "Rainfall in the valley was lower than usual this spring, so farmers planted later and chose shorter-season varieties."},
    ]});
    if let Some(spec) = spec_sha256 {
        body["spec_sha256"] = j!(spec);
    }
    if spans {
        body["spans"] = j!(true);
    }
    let r = async {
        let stub = lane_stub(env, "score", &name).ok()?;
        let mut resp = stub.fetch_with_request(internal_request(Method::Post, "/run", Some(body.to_string())).ok()?).await.ok()?;
        let text = super::cf_io::response_bytes(&mut resp, DO_REPLY_BYTES).await.ok().and_then(|b| String::from_utf8(b).ok()).unwrap_or_default();
        Some((resp.status_code(), text))
    }
    .await;
    match r {
        Some((200, text)) => serde_json::from_str(&text).unwrap_or(j!({"error": "probe reply"})),
        // The lane's failure is a short content-free code.
        Some((status, text)) => j!({"error": "probe failed", "status": status, "code": text.chars().take(120).collect::<String>()}),
        None => j!({"error": "probe unreachable"}),
    }
}

/// The kick a heartbeat sends to an instance: the confirmed watermark and, from
/// ticks, the epoch with the head's epoch index. Returns the authority's
/// reply (`moved`, `work`, `scoring`).
pub async fn kick(env: &Env, instance: &str, kick: Kick) -> Option<Value> {
    let body = serde_json::to_string(&kick).ok()?;
    let mut r = cohort_fetch_within(env, instance, Method::Post, "/kick", Some(body), Some(KICK_TIMEOUT_MS)).await.ok()?;
    if r.status_code() != 200 {
        super::cf_io::discard(&mut r);
        return None;
    }
    super::cf_io::response_json::<Value>(&mut r, DO_REPLY_BYTES).await.ok()
}

/// Forwards an operator or status request to an instance's cohort authority.
pub async fn forward(env: &Env, instance: &str, method: Method, path: &str, body: Option<String>) -> Result<Response> {
    cohort_fetch(env, instance, method, path, body).await
}

/// The main authority's status for the Worker's `/status` (the authority the
/// deployment names), bounded like a journal call.
pub async fn main_status(env: &Env) -> Option<Value> {
    let mut r = cohort_fetch_within(env, engine::INSTANCE_MAIN, Method::Get, "/status", None, Some(KICK_TIMEOUT_MS))
        .await
        .ok()?;
    if r.status_code() != 200 {
        super::cf_io::discard(&mut r);
        return None;
    }
    super::cf_io::response_json::<Value>(&mut r, crate::arena_routes::WORKER_CONTROL_BYTES).await.ok()
}

// ------------------------------------------------------- reward-v1 (plan v3)

/// A sub-seed of the cohort seed for one draw role.
fn sub_seed(seed: &[u8; 32], role: &str) -> [u8; 32] {
    slop_ninja_protocol::domain_hash(&format!("slop_ninja.eval/draw-role/v1/{role}"), seed).0
}

fn mark_used(rows: &mut std::collections::BTreeSet<String>, groups: &mut std::collections::BTreeSet<String>, d: &pool::Draw) {
    rows.extend(d.rows.iter().map(|r| r.row_id.clone()));
    groups.extend(d.groups.iter().cloned());
}

impl EvalCohort {
    /// Reads and checks one scored artifact's files manifest, for its digests.
    async fn artifact_digests(&self, a: &crate::eval::plan::ArtifactJson) -> std::result::Result<v3::ArtifactDigests, &'static str> {
        let b = get_bounded(&self.env, EVAL_BUCKET, &a.manifest_key, STEP_OBJECT_BYTES)
            .await
            .map_err(|_| "artifact manifest read")?
            .ok_or("artifact manifest missing")?;
        if sha_hex(&b) != a.package_sha256 {
            return Err("artifact manifest digest");
        }
        // An admitted entrant's header digest is the one admission recorded
        // (R2 verified it at ingest); it is never hashed again here.
        let storage = self.state.storage();
        let package = a.package_sha256.clone();
        let recorded = transaction(&storage, move |sql| crate::eval::admission::recorded_header_sha256(sql, &package))
            .map_err(|_| "admission store")?;
        v3::artifact_digests(&b, recorded)
    }

    async fn draw_v3(&self, cfg: &EvalConfig, seq: i64, plan: &CohortPlan) -> std::result::Result<(), &'static str> {
        let env = &self.env;
        let mbytes = self.read(seq, EVAL_BUCKET, &plan.pool.manifest_key, STEP_OBJECT_BYTES, "pool manifest read").await?.ok_or("pool manifest missing")?;
        if sha_hex(&mbytes) != plan.pool.manifest_sha256 {
            return Err("pool manifest digest");
        }
        let manifest = PoolManifest::parse(&mbytes, &plan.pool.pool_id).map_err(|e| e.0)?;
        let storage = self.state.storage();
        let dir = plan.pool.manifest_key.rsplit_once('/').map_or("", |x| x.0).to_owned();
        // The seeded order's input screen on pool rows (finding L9,
        // `v3::INPUT_SCREEN_RULE`), before anything is drawn or written: a
        // pool whose export records no token bound inside the scorer's limit
        // is refused (deterministic: the cohort pauses), and every row the
        // scorer would refuse as empty is held out of every role, one shard
        // at a time.
        let seeded = plan.reward_v1.as_ref().is_some_and(crate::eval::plan::RewardV1Plan::seeded_task_order);
        let mut screened = std::collections::BTreeSet::new();
        if seeded {
            if !v3::pool_token_bound_ok(&manifest) {
                return Err(v3::POOL_TOKEN_BOUND_REFUSED);
            }
            for info in &manifest.shards {
                let body = self.read(seq, EVAL_BUCKET, &format!("{dir}/{}", info.file), STEP_OBJECT_BYTES, "pool shard read").await?.ok_or("pool shard missing")?;
                if sha_hex(&body) != info.sha256 {
                    return Err("pool shard digest");
                }
                let rows = std::str::from_utf8(&body)
                    .map_err(|_| "shard utf-8")?
                    .lines()
                    .map(|line| serde_json::from_str::<ShardRow>(line).map_err(|_| "shard row"))
                    .collect::<std::result::Result<Vec<_>, &'static str>>()?;
                screened.extend(v3::screen_pool(
                    rows.iter()
                        .filter(|r| r.schema == pool::SHARD_SCHEMA && sha_hex(r.text.as_bytes()) == r.text_sha256)
                        .map(|r| (r.row_id.as_str(), r.text.as_str())),
                ));
            }
        }
        let authority = self.state.id().to_string();
        let entries = read_ledger(self, seq, &dir, &plan.pool.pool_id).await?;
        let mut ledger = pool::Used::default();
        ledger.extend_from_ledger(&entries, &authority, &plan.label);
        let (mut used_rows, mut used_groups) = transaction(&storage, engine::used_set).map_err(|_| "used set")?;
        used_rows.extend(ledger.row_ids(&manifest));
        used_groups.extend(ledger.groups.iter().cloned());
        let seed = engine::cohort_seed(&cfg.private_seed, &plan.label);
        let [n_h, n_m, _] = plan.generated_counts();
        let c_a = u64::from(plan.audit_humans());
        // Fresh stock is drawn only for a cohort whose frozen roster holds an
        // admitted outside entrant (`v3::fresh_allowed`); every other cohort
        // draws only the pools the deployment lists as exposed.
        let exposed = cfg.exposed_pools.contains(&plan.pool.pool_id);
        let fresh_ok = transaction(&storage, move |sql| {
            let c = engine::cohort(sql, seq)?;
            v3::fresh_allowed(sql, &c)
        })
        .map_err(|_| "fresh stock gate")?;
        if !exposed && !fresh_ok {
            let now = now_ms();
            let _ = transaction(&storage, move |sql| engine::fault(sql, seq, engine::FRESH_STOCK_REFUSED_CODE, now));
            return Err(engine::FRESH_STOCK_REFUSED_CODE);
        }
        // Exposed stock: rows no cohort drew come first, then any row again,
        // never twice in this cohort. Fresh stock is never drawn again.
        // `drawn` holds this cohort's picks, and from the start every row the
        // screen held out, so no role (and no draw again) takes one or its
        // source group.
        let mut drawn: (std::collections::BTreeSet<String>, std::collections::BTreeSet<String>) = (screened.clone(), Default::default());
        let draw_exposed = |want: u64, role_seed: [u8; 32], used: &(std::collections::BTreeSet<String>, std::collections::BTreeSet<String>), drawn: &mut (std::collections::BTreeSet<String>, std::collections::BTreeSet<String>)| {
            let mut avoid_rows = used.0.clone();
            avoid_rows.extend(drawn.0.iter().cloned());
            let mut avoid_groups = used.1.clone();
            avoid_groups.extend(drawn.1.iter().cloned());
            let mut d = pool::draw(&manifest, &avoid_rows, &avoid_groups, want, &role_seed);
            mark_used(&mut drawn.0, &mut drawn.1, &d);
            let got = d.rows.len() as u64;
            if got < want && exposed {
                let again = pool::draw(&manifest, &drawn.0, &drawn.1, want - got, &sub_seed(&role_seed, "again"));
                mark_used(&mut drawn.0, &mut drawn.1, &again);
                d.rows.extend(again.rows);
                d.groups.extend(again.groups);
                d.shortfall = again.shortfall;
            }
            d
        };
        let used = (used_rows, used_groups);
        let negatives = draw_exposed(u64::from(n_h), seed, &used, &mut drawn);
        let mut audits = Vec::new();
        for (role, want) in [("audit-a", c_a), ("audit-b", u64::from(plan.audit_b_humans()))] {
            if want == 0 {
                audits.push(None);
                continue;
            }
            audits.push(Some(draw_exposed(want, sub_seed(&seed, role), &used, &mut drawn)));
        }
        let p2 = plan.clone();
        let (earlier, ledger_all, ineligible) = if plan.is_probe_build() {
            (Vec::new(), Default::default(), Default::default())
        } else {
            let p3 = plan.clone();
            let earlier = transaction(&storage, move |sql| v3::reuse_candidates(sql, &p2)).map_err(|_| "reuse candidates")?;
            let (all, ineligible) = transaction(&storage, move |sql| v3::ledger_rows(sql, &p3)).map_err(|_| "reuse ledger")?;
            (earlier, all, ineligible)
        };
        // A passage drawn this cohort as a negative or an audit human never
        // also parents as an earlier passage (one child per parent per cohort).
        let earlier: Vec<v3::ReuseCandidate> = earlier.into_iter().filter(|e| !drawn.0.contains(&e.row.row_id)).collect();
        // Fresh parents never come from the ledger (eligible passages are the
        // earlier candidates), so the reuse limit and gap always hold.
        let mut parent_avoid = drawn.0.clone();
        parent_avoid.extend(ledger_all.iter().cloned());
        let fresh = pool::draw(&manifest, &parent_avoid, &drawn.1, u64::from(n_m), &sub_seed(&seed, "parents"));
        // Texts of every row the draw may use, checked by digest.
        let mut want: std::collections::BTreeSet<String> = negatives.rows.iter().map(|r| r.row_id.clone()).collect();
        for d in audits.iter().flatten() {
            want.extend(d.rows.iter().map(|r| r.row_id.clone()));
        }
        want.extend(fresh.rows.iter().map(|r| r.row_id.clone()));
        want.extend(earlier.iter().map(|e| e.row.row_id.clone()));
        let shard_of: BTreeMap<&str, u32> = manifest.rows.iter().map(|r| (r.row_id.as_str(), r.shard)).collect();
        let shards: std::collections::BTreeSet<u32> = want.iter().filter_map(|r| shard_of.get(r.as_str()).copied()).collect();
        let mut texts = BTreeMap::new();
        for k in shards {
            let info = manifest.shards.iter().find(|s| s.index == k).ok_or("shard info")?;
            let body = self.read(seq, EVAL_BUCKET, &format!("{dir}/{}", info.file), STEP_OBJECT_BYTES, "pool shard read").await?.ok_or("pool shard missing")?;
            if sha_hex(&body) != info.sha256 {
                return Err("pool shard digest");
            }
            for line in std::str::from_utf8(&body).map_err(|_| "shard utf-8")?.lines() {
                let r: ShardRow = serde_json::from_str(line).map_err(|_| "shard row")?;
                if want.contains(&r.row_id) && r.schema == pool::SHARD_SCHEMA && sha_hex(r.text.as_bytes()) == r.text_sha256 {
                    texts.insert(r.row_id, r.text);
                }
            }
        }
        let parents = v3::choose_parents(&seed, &negatives.rows, &earlier, &fresh.rows, &texts, n_m, &ineligible);
        // Every probe version in use, from its private record.
        let mut probe = Vec::new();
        for p in plan.reward_v1.as_ref().map(|v| v.probes.as_slice()).unwrap_or(&[]) {
            let rec = self.read(seq, EVAL_BUCKET, &p.key, STEP_OBJECT_BYTES, "probe record read").await?.ok_or("probe record missing")?;
            if sha_hex(&rec) != p.sha256 {
                return Err("probe record digest");
            }
            let record: v3::ProbeRecord = serde_json::from_slice(&rec).map_err(|_| "probe record json")?;
            if record.version != p.version || record.size != p.size || record.docs.len() != p.size as usize {
                return Err("probe record version or size");
            }
            let mut by_task: BTreeMap<u32, String> = BTreeMap::new();
            for (key, sha) in &record.shards {
                let body = self.read(seq, EVAL_BUCKET, key, STEP_OBJECT_BYTES, "probe shard read").await?.ok_or("probe shard missing")?;
                if &sha_hex(&body) != sha {
                    return Err("probe shard digest");
                }
                for line in std::str::from_utf8(&body).map_err(|_| "probe shard utf-8")?.lines() {
                    let v: Value = serde_json::from_str(line).map_err(|_| "probe shard row")?;
                    let task = v["task"].as_u64().and_then(|t| u32::try_from(t).ok()).ok_or("probe task")?;
                    by_task.insert(task, v["text"].as_str().ok_or("probe text")?.to_owned());
                }
            }
            let docs = record
                .docs
                .iter()
                .map(|d| Ok((d.clone(), by_task.remove(&d.task).ok_or("probe text missing")?)))
                .collect::<std::result::Result<Vec<_>, &'static str>>()?;
            probe.push((p.version, docs));
        }
        // Write-ahead: the ledger entry is durable before the draw commits.
        let plan_sha = transaction(&storage, move |sql| engine::plan_sha256(sql, seq)).map_err(|_| "plan digest")?;
        let mut all = negatives.clone();
        for d in audits.iter().flatten() {
            all.rows.extend(d.rows.iter().cloned());
            all.groups.extend(d.groups.iter().cloned());
        }
        all.rows.extend(parents.fresh_used.iter().cloned());
        all.groups.extend(parents.fresh_used.iter().map(|r| r.source_group.clone()));
        all.groups.sort();
        all.groups.dedup();
        let mut entry = pool::ledger_entry(&plan.pool.pool_id, plan.cohort, &plan.label, &authority, &plan_sha, &all);
        entry.rule = v3::REUSE_RULE.into();
        let digest_of: BTreeMap<&str, &str> = negatives
            .rows
            .iter()
            .chain(parents.fresh_used.iter())
            .chain(earlier.iter().map(|e| &e.row))
            .map(|r| (r.row_id.as_str(), r.text_sha256.as_str()))
            .collect();
        entry.sources = parents
            .children
            .iter()
            .map(|(row, origin)| pool::SourceUse {
                row_id: row.clone(),
                text_sha256: digest_of.get(row.as_str()).copied().unwrap_or_default().to_owned(),
                origin: (*origin).to_owned(),
            })
            .collect();
        entry.draw_mode = Some(v3::DRAW_EXPOSED.into());
        entry.screened = manifest
            .rows
            .iter()
            .filter(|r| screened.contains(&r.row_id))
            .map(|r| pool::LedgerRow { row_id: r.row_id.clone(), text_sha256: r.text_sha256.clone() })
            .collect();
        let entry_key = ledger_key(&dir, &plan.label, &authority);
        put(env, EVAL_BUCKET, &entry_key, serde_json::to_vec_pretty(&entry).map_err(|_| "ledger entry")?)
            .await
            .map_err(|_| "used ledger write")?;
        // Generation v2: the plan's bound manifest and voice library, refused
        // before each read when the declared sizes pass the generation
        // objects bound (`gen_v2::GEN_OBJECTS_BYTES`: the manifest's alone,
        // then both), and checked by digest again in the draw.
        let generation = match &plan.generation {
            Some(g) => {
                use crate::eval::gen_v2;
                if !gen_v2::storable(&g.manifest, None) {
                    return Err(self.step_refused_now(seq, engine::EvalError::State("generation manifest (larger than the generation objects bound)")));
                }
                let bound = usize::try_from(g.manifest.bytes).map_err(|_| "generation manifest size")?;
                let mb = self.read(seq, EVAL_BUCKET, &g.manifest.key, bound, "generation manifest read").await?.ok_or("generation manifest missing")?;
                let m = slop_ninja_gen_core::manifest::Manifest::parse_bound(&mb, &g.manifest)
                    .map_err(|e| self.step_refused_now(seq, engine::EvalError::State(e.0)))?;
                let lref = gen_v2::library_ref(&m);
                if !gen_v2::storable(&g.manifest, Some(&lref)) {
                    return Err(self.step_refused_now(seq, engine::EvalError::State("voice library (with the manifest, larger than the generation objects bound)")));
                }
                let bound = usize::try_from(lref.bytes).map_err(|_| "voice library size")?;
                let lb = self.read(seq, EVAL_BUCKET, &lref.key, bound, "voice library read").await?.ok_or("voice library missing")?;
                Some(gen_v2::DrawObjects { manifest: mb, library: lb })
            }
            None => None,
        };
        let d = v3::DrawV3 {
            negatives,
            audit_a: audits.first().cloned().flatten(),
            audit_b: audits.get(1).cloned().flatten(),
            fresh: Some(fresh),
            earlier,
            probe,
            generation,
        };
        let c2 = cfg.clone();
        let now = now_ms();
        let drawn = transaction(&storage, move |sql| v3::apply_draw(sql, &c2, seq, &d, &parents, &texts, &ledger, now))
            .map_err(|e| match e {
                TxError::Operation(e) => self.step_refused_now(seq, e),
                TxError::Runtime => "draw commit",
            })?;
        if !drawn && let Ok(b) = env.bucket(EVAL_BUCKET) {
            let _ = b.delete(entry_key).await;
        }
        Ok(())
    }

    async fn seal_v3(&self, cfg: &EvalConfig, seq: i64, plan: &CohortPlan) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let reference = self.artifact_digests(&plan.reference).await?;
        let predecessor = match &plan.previous_reference {
            Some(p) => Some(self.artifact_digests(p).await?),
            None => None,
        };
        let mut entrants = Vec::new();
        for a in &plan.entrants {
            entrants.push(self.artifact_digests(a).await?);
        }
        let inputs = v3::SealInputs { reference, predecessor, entrants };
        let c2 = cfg.clone();
        let b = transaction(&storage, move |sql| v3::prepare_seal(sql, &c2, seq, &inputs)).map_err(|e| match e {
            // A plan naming other references than the chain can never seal:
            // the cohort faults for the operator instead of retrying.
            TxError::Operation(e) if e.code() == v3::REFERENCE_CHAIN_MISMATCH => self.fault_now(seq, "reference_chain_mismatch"),
            // A token count the arena refused keeps the arena's class
            // (`v3::TokenRefusal`): a busy pool is a transient step error
            // (the seal runs again after its retry time), capacity and
            // defect pause the cohort resumably, never as `step_refused`.
            TxError::Operation(e) if e.code() == v3::TOKEN_SCRATCH_REFUSED => v3::TOKEN_SCRATCH_REFUSED,
            TxError::Operation(e) if matches!(e.code(), "arena_capacity" | "arena_defect") => self.fault_now(seq, e.code()),
            TxError::Operation(e) => self.step_refused_now(seq, e),
            TxError::Runtime => "prepare seal",
        })?;
        for group in b.shards.chunks(PARALLEL) {
            let r = join_all(group.iter().map(|(key, body)| put(&self.env, EVAL_BUCKET, key, body.clone()))).await;
            if r.iter().any(|x| x.is_err()) {
                return Err("batch shard put");
            }
        }
        // Generation v2's private records, before the index that binds them.
        for group in b.gen_shards.chunks(PARALLEL) {
            let r = join_all(group.iter().map(|(key, body)| put(&self.env, EVAL_BUCKET, key, body.clone()))).await;
            if r.iter().any(|x| x.is_err()) {
                return Err("generation record put");
            }
        }
        put(&self.env, EVAL_BUCKET, &b.index_key, b.index.clone()).await.map_err(|_| "batch index put")?;
        let mut puts = b.shards.len() as u64 + b.gen_shards.len() as u64 + 1;
        if let Some((key, bytes)) = &b.probe_record {
            put(&self.env, EVAL_BUCKET, key, bytes.clone()).await.map_err(|_| "probe record put")?;
            puts += 1;
        }
        if let Some(m) = &b.manifest {
            let (package_key, _) = keys(&self.env)?;
            let prefix = engine::receipt_prefix(&m.cohort_label);
            let signed = package::signed_bytes(m.clone(), &package_key).map_err(|e| e.0)?;
            put(&self.env, RECEIPTS_BUCKET, &format!("{prefix}/manifest.signed"), signed).await.map_err(|_| "manifest put")?;
            if let Some((contents, fps)) = &b.published {
                put(&self.env, RECEIPTS_BUCKET, &format!("{prefix}/published-contents.bin"), contents.clone())
                    .await
                    .map_err(|_| "published contents put")?;
                put(&self.env, RECEIPTS_BUCKET, &format!("{prefix}/fingerprint-index.bin"), fps.clone())
                    .await
                    .map_err(|_| "fingerprint index put")?;
            }
            puts += 3;
        }
        let now = now_ms();
        transaction(&storage, move |sql| v3::apply_seal(sql, seq, &b, puts, now)).map_err(|_| "seal commit")?;
        Ok(())
    }

    async fn summaries(&self, seq: i64, plan: &CohortPlan) -> std::result::Result<Vec<engine::JobSummary>, &'static str> {
        let storage = self.state.storage();
        let bound = summary_bytes(max_job_tasks(plan));
        let refs: Vec<(String, String)> = transaction(&storage, move |sql| {
            Ok::<_, engine::EvalError>(
                sql.exec("SELECT summary_key, summary_sha256 FROM jobs WHERE seq = ? ORDER BY job", &[crate::sql::SqlValue::Integer(seq)])?
                    .iter()
                    .map(|r| (crate::sql::text(r, 0).unwrap_or("").to_owned(), crate::sql::text(r, 1).unwrap_or("").to_owned()))
                    .collect::<Vec<_>>(),
            )
        })
        .map_err(|_| "summary refs")?;
        let mut summaries = Vec::new();
        for group in refs.chunks(PARALLEL) {
            for r in join_all(group.iter().map(|(k, sha)| async move {
                let b = self.read(seq, EVAL_BUCKET, k, bound, "summary read").await?.ok_or("summary missing")?;
                if &sha_hex(&b) != sha {
                    return Err("summary digest");
                }
                serde_json::from_slice::<engine::JobSummary>(&b).map_err(|_| "summary json")
            }))
            .await
            {
                summaries.push(r?);
            }
        }
        Ok(summaries)
    }

    async fn package_v3(&self, cfg: &EvalConfig, seq: i64, plan: &CohortPlan) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let summaries = self.summaries(seq, plan).await?;
        let (index, chunk_map) = transaction(&storage, move |sql| engine::receipt_index(sql, seq)).map_err(|e| match e {
            TxError::Operation(e) => self.step_refused_now(seq, e),
            TxError::Runtime => "receipt index",
        })?;
        let (package_key, receipt_key) = keys(&self.env)?;
        let label = plan.label.clone();
        if plan.is_probe_build() {
            let version = plan.reward_v1.as_ref().and_then(|v| v.probe_version).ok_or("probe version")?;
            let record_key = format!("{}/record.json", v3::probe_prefix(cfg, version));
            let record = self
                .read(seq, EVAL_BUCKET, &record_key, STEP_OBJECT_BYTES, "probe record read")
                .await?
                .ok_or("probe record missing")?;
            let s2 = summaries.clone();
            let c2p = cfg.clone();
            let f = match transaction(&storage, move |sql| {
                let c = engine::cohort(sql, seq)?;
                v3::prepare_probe_finish(sql, &c2p, &c, &s2, &record)
            }) {
                Ok(f) => f,
                Err(TxError::Operation(e)) if e.code() == "reference_failure" => {
                    let now = now_ms();
                    let _ = transaction(&storage, move |sql| engine::fault(sql, seq, "reference_failure", now));
                    return Err("reference_failure");
                }
                Err(_) => return Err("probe finish"),
            };
            for (key, bytes) in &f.vectors {
                put(&self.env, EVAL_BUCKET, key, bytes.clone()).await.map_err(|_| "fingerprint put")?;
            }
            let prefix = v3::probe_prefix(cfg, f.version);
            put(&self.env, RECEIPTS_BUCKET, &format!("{prefix}/receipt-index.signed"),
                package::signed_bytes(index.clone(), &receipt_key).map_err(|e| e.0)?)
                .await
                .map_err(|_| "receipt index put")?;
            put(&self.env, RECEIPTS_BUCKET, &format!("{prefix}/chunk-map.json"), serde_json::to_vec_pretty(&chunk_map).unwrap_or_default())
                .await
                .map_err(|_| "chunk map put")?;
            let now = now_ms();
            transaction(&storage, move |sql| v3::commit_probe_finish(sql, seq, &f, now)).map_err(|_| "probe finish commit")?;
            return Ok(());
        }
        // Every fingerprint the manifest's index holds on the probe versions in use.
        let keys_needed = transaction(&storage, move |sql| {
            let st = v3::state(sql, seq)?;
            let at_seal = v3::index_at_seal(&st)?;
            let (_, keys) = v3::published_index(sql)?;
            let mut out = Vec::new();
            for f in &at_seal.fingerprints.entries {
                let key = keys.get(&(f.content, f.version)).ok_or(engine::EvalError::State("fingerprint vector key"))?;
                out.push((f.content, f.version, f.fingerprint, key.clone()));
            }
            Ok::<_, engine::EvalError>(out)
        })
        .map_err(|_| "fingerprint keys")?;
        let versions: Vec<u32> = plan.reward_v1.as_ref().map(|v| v.probes.iter().map(|p| p.version).collect()).unwrap_or_default();
        let mut fps = BTreeMap::new();
        for (content, version, digest, key) in keys_needed {
            if !versions.contains(&version) {
                continue;
            }
            // A fingerprint vector is one 4-byte word per probe document.
            let size = plan.reward_v1.as_ref().and_then(|v| v.probes.iter().find(|p| p.version == version)).map_or(0, |p| p.size as usize);
            let b = self.read(seq, EVAL_BUCKET, &key, size.saturating_mul(4), "fingerprint read").await?.ok_or("fingerprint missing")?;
            let v = v3::vector_from_bytes(&b)?;
            if v3::vector_digest(&v) != digest {
                return Err("fingerprint vector digest");
            }
            fps.insert((content, version), v);
        }
        let c2 = cfg.clone();
        let root = index.digest().map_err(|_| "receipt root")?;
        let count = index.count();
        let b = match transaction(&storage, move |sql| {
            let c = engine::cohort(sql, seq)?;
            v3::prepare_package(sql, &c2, &c, &summaries, &fps, root, count)
        }) {
            Ok(b) => b,
            Err(TxError::Operation(e)) if e.code() == "reference_failure" => {
                let now = now_ms();
                let _ = transaction(&storage, move |sql| engine::fault(sql, seq, "reference_failure", now));
                return Err("reference_failure");
            }
            Err(TxError::Operation(e)) if e.code() == v3::PACKAGE_REFUSED => {
                // Deterministic: the cohort pauses for reconciliation.
                let now = now_ms();
                let _ = transaction(&storage, move |sql| engine::fault(sql, seq, v3::PACKAGE_REFUSED, now));
                return Err(v3::PACKAGE_REFUSED);
            }
            Err(TxError::Operation(e)) => return Err(self.step_refused_now(seq, e)),
            Err(TxError::Runtime) => return Err("prepare package"),
        };
        for (key, bytes) in &b.vectors {
            put(&self.env, EVAL_BUCKET, key, bytes.clone()).await.map_err(|_| "fingerprint put")?;
        }
        let prefix = engine::receipt_prefix(&label);
        let mut objects = vec![
            ("receipt-index.signed", package::signed_bytes(index.clone(), &receipt_key).map_err(|e| e.0)?),
            // Private: exact scores and hidden measurements. The publisher
            // reads only `result.signed` and the closure.
            ("package.signed", package::signed_bytes(b.package.clone(), &package_key).map_err(|e| e.0)?),
            ("result.signed", package::signed_bytes(b.result.clone(), &package_key).map_err(|e| e.0)?),
            ("chunk-map.json", serde_json::to_vec_pretty(&chunk_map).unwrap_or_default()),
            ("components.json", serde_json::to_vec_pretty(&b.components).unwrap_or_default()),
        ];
        if let Some(t) = &b.target {
            objects.push(("target.signed", package::signed_bytes(t.clone(), &package_key).map_err(|e| e.0)?));
        }
        objects.push((
            "keys.json",
            serde_json::to_vec_pretty(&j!({
                "instance": engine::INSTANCE_MAIN,
                "package_public_key": hex::encode(package_key.verifying_key().to_bytes()),
                "receipt_public_key": hex::encode(receipt_key.verifying_key().to_bytes()),
                "package_digest": b.package.digest().map(|d| d.to_string()).unwrap_or_default(),
                "result_digest": b.package.result.to_string(),
                "target_digest": b.target.as_ref().and_then(|t| t.digest().ok()).map(|d| d.to_string()),
            }))
            .unwrap_or_default(),
        ));
        let puts = objects.len() as u64 + b.vectors.len() as u64;
        for (name, bytes) in objects {
            put(&self.env, RECEIPTS_BUCKET, &format!("{prefix}/{name}"), bytes).await.map_err(|_| "package put")?;
        }
        let now = now_ms();
        transaction(&storage, move |sql| v3::commit_package(sql, seq, &b, puts, now)).map_err(|_| "package commit")?;
        Ok(())
    }

    async fn close_v3(&self, cfg: &EvalConfig, seq: i64) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let c2 = cfg.clone();
        let b = transaction(&storage, move |sql| v3::prepare_closure(sql, &c2, seq)).map_err(|e| match e {
            TxError::Operation(e) => self.step_refused_now(seq, e),
            TxError::Runtime => "prepare closure",
        })?;
        let (package_key, _) = keys(&self.env)?;
        let prefix = engine::receipt_prefix(&b.closure.cohort_label);
        let digest = b.closure.digest().map_err(|_| "closure digest")?;
        let fps: Vec<Value> = b
            .closure_fingerprints
            .iter()
            .map(|f| j!({"hotkey": hex::encode(f.hotkey.0), "version": f.version, "fingerprint": f.fingerprint.to_string()}))
            .collect();
        let plan = transaction(&storage, move |sql| Ok::<_, engine::EvalError>(engine::cohort(sql, seq)?.plan)).map_err(|_| "closure plan")?;
        let objects = [
            ("closure.signed", package::signed_bytes(b.closure.clone(), &package_key).map_err(|e| e.0)?),
            ("published-contents-after.bin", b.published_after.0.clone()),
            ("fingerprint-index-after.bin", b.published_after.1.clone()),
            ("closure-fingerprints.json", serde_json::to_vec_pretty(&j!({"fingerprints": fps})).unwrap_or_default()),
            // Admitted entrants' objects, published after the close.
            ("closure-artifacts.json", serde_json::to_vec_pretty(&crate::eval::admission::closure_artifacts(&plan)).unwrap_or_default()),
        ];
        let puts = objects.len() as u64;
        for (name, bytes) in objects {
            put(&self.env, RECEIPTS_BUCKET, &format!("{prefix}/{name}"), bytes).await.map_err(|_| "closure put")?;
        }
        transaction(&storage, move |sql| v3::commit_closure(sql, seq, &digest, puts)).map_err(|_| "closure commit")?;
        Ok(())
    }
}
