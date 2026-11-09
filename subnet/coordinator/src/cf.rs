//! Cloudflare Workers entry points and Durable Objects (wasm32 only).
//!
//! Routes:
//! - `POST /hook`: rtao-rpc finalized-block webhook, the primary heartbeat.
//!   Authenticates, commits the delivery and its per-block step in the
//!   ChainJournal, acknowledges with 204, then runs any bounded read-only RPC
//!   round in `waitUntil`.
//! - `GET /status`: content-free JSON, served only to the operator token
//!   from any host and to the publisher's and the alerts relay's own tokens
//!   through their service bindings (`access`).
//! - `POST /fault/clear`: operator action; requires `COORDINATOR_OPERATOR_TOKEN`.
//! - `POST /signing/target`, `POST /signing/control`, `POST /signing/kick`,
//!   `POST /signing/restart?from_epoch=F`, `POST /signing/revoke`: operator
//!   actions for the signing journal (install the T12 target,
//!   resume/pause/clear/arm a testnet drill, run one signing round now,
//!   restart the held target with the restarted chain record's launch
//!   bundle, journal a signed revocation: the body is the signed record's
//!   bytes).
//! - cron (`* * * * *`): the watchdog, the only timer. On the coordinator it
//!   also runs the automatic target handoff ([`auto_drive`]: each closed
//!   cohort's target installed and its authorization signed by the
//!   ChainJournal, `signing::auto`), then keeps the private authorization
//!   pointer (`authorizations/current.json` in the receipts bucket) equal to
//!   what the signing journal holds ([`pointer_drive`]). Nothing expires:
//!   neither the pointer nor the authorization it names has a validity
//!   window.
//!
//! Memory: every export is hand-written in `cf_arena.rs` and runs its handler
//! inside its own bounded arena scope (`arena_routes`); bodies are read with
//! the bounded readers of `cf_io.rs`.
//!
//! Every delivery and every tick also runs one bounded signing round
//! (`drive_sign`) after its acknowledgment. The ChainJournal plans and
//! journals each step and signs inside its own transaction; the Worker only
//! makes the step's single RPC call. The hotkey secret is read only by the
//! ChainJournal, only while signing, and never leaves it except as a
//! signature inside the extrinsic.
//!
//! Nothing here logs request bodies, headers or secrets.

use crate::access;
use crate::chain::{ChainError, Hotkey};
use crate::config::{self, ChainConfig, DEFAULT_KEY_ID};
use crate::heartbeat::{self, DeliveryReport, RpcPlan, RpcResult, Settings, Step};
use crate::journal::IngestError;
use crate::liveness;
use crate::rpc::{self, RpcError};
use crate::signing::{
    self, Ctx, KeySource, RuntimeCache, SignConfig, SignOutcome, SignPlan, SignStep, SubmitOutcome,
};
use crate::sql::{
    self, CHAIN_JOURNAL_SCHEMA, Row, Schema, Sql, SqlValue, StoreError,
};
use crate::webhook::{self, BlockNotification, ConfigError, Delivery, ReceiverConfig};
use crate::wire::{BlockRef, FinalizedHeader};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use slop_ninja_arena::ScopeRef;
use worker::{
    Context, Date, Delay, DurableObject, Env, Fetch, Headers, Method, Request,
    RequestInit, Response, Result, SqlStorage, SqlStorageValue, State, Storage,
};
use zeroize::Zeroizing;

const CHAIN_JOURNAL: &str = "CHAIN_JOURNAL";
const INTERNAL_ORIGIN: &str = "https://coordinator.internal";
const MAX_OPERATOR_BODY: usize = 1024;

/// Bounds of the internal Durable Object bodies, read with `cf_io` (one
/// admitted block, refused past the bound). Each body is built by this
/// crate's Worker side from a value bounded where it entered: it re-encodes
/// a subset of that value's hex strings and integers (no string needs a JSON
/// escape), so it is never longer than its source, plus the small fields
/// beside it (a holder or claim token, at most an operator body).
/// `/ingest`: a notification parsed from an authenticated webhook body.
const INGEST_BODY_BYTES: usize = webhook::MAX_BODY_BYTES;
/// `/rpc-result`: one RPC round's parsed result (a reply of at most
/// `rpc::MAX_RESPONSE_BYTES`) and its holder.
const RPC_RESULT_BODY_BYTES: usize = rpc::MAX_RESPONSE_BYTES + MAX_OPERATOR_BODY;
/// `/sign-result`: one signing step's outcome (a reply of at most
/// `rpc::MAX_LARGE_RESPONSE_BYTES`: the metadata hex or a storage read; a
/// block body is reduced to its `rpc::BlockMatch` in the Worker) and its
/// claim.
const SIGN_RESULT_BODY_BYTES: usize = rpc::MAX_LARGE_RESPONSE_BYTES + MAX_OPERATOR_BODY;
/// `/sign-admin`: an operator body carrying at most a target bundle as one
/// JSON string; escaping its quotes and newlines at most doubles it.
const SIGN_ADMIN_BODY_BYTES: usize =
    2 * (if crate::target::MAX_BUNDLE_BYTES > crate::target::MAX_BUNDLE_V2_BYTES { crate::target::MAX_BUNDLE_BYTES } else { crate::target::MAX_BUNDLE_V2_BYTES })
        + MAX_OPERATOR_BODY;
/// The small fixed-shape bodies (`/clear-fault`, `/sign-step`, `/sign-mark`,
/// `/sign-stall`): claim tokens, holders and counters, at most an operator
/// body.
const SMALL_BODY_BYTES: usize = MAX_OPERATOR_BODY;
/// Signing rounds per invocation: a delivery's `waitUntil` allows 30 s.
const DELIVERY_SIGN_ROUNDS: u64 = 3;
const TICK_SIGN_ROUNDS: u64 = 8;
/// The stale-invocation drill: the stalled holder's shortened lease, and how
/// long it sleeps before trying to mark.
const DRILL_STALL_LEASE_MS: u64 = 2_000;
const DRILL_STALL_SLEEP_MS: u64 = signing::DRILL_STALL_SLEEP_MS;

fn now_ms() -> u64 {
    Date::now().as_millis()
}

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string())
}

fn settings(env: &Env) -> std::result::Result<Settings, ConfigError> {
    config::settings(|name| var(env, name))
}

/// This Worker's deployment role, from `EVAL_ROLE`.
fn role(env: &Env) -> config::Role {
    config::role(var(env, "EVAL_ROLE").as_deref())
}

// ---------------------------------------------------------------- storage

struct DoSql(SqlStorage);

impl Sql for DoSql {
    fn exec(&self, query: &str, params: &[SqlValue]) -> std::result::Result<Vec<Row>, StoreError> {
        let bindings: Vec<SqlStorageValue> = params
            .iter()
            .map(|value| match value {
                SqlValue::Null => SqlStorageValue::Null,
                SqlValue::Integer(v) => SqlStorageValue::Integer(*v),
                SqlValue::Text(v) => SqlStorageValue::String(v.clone()),
            })
            .collect();
        let cursor = self
            .0
            .exec(query, bindings)
            .map_err(|_| StoreError("sql exec"))?;
        cursor
            .raw()
            .map(|row| {
                row.map_err(|_| StoreError("sql row"))?
                    .into_iter()
                    .map(from_storage)
                    .collect()
            })
            .collect()
    }
}

fn from_storage(value: SqlStorageValue) -> std::result::Result<SqlValue, StoreError> {
    match value {
        SqlStorageValue::Null => Ok(SqlValue::Null),
        SqlStorageValue::Integer(v) => Ok(SqlValue::Integer(v)),
        SqlStorageValue::Boolean(v) => Ok(SqlValue::Integer(v.into())),
        SqlStorageValue::String(v) => Ok(SqlValue::Text(v)),
        SqlStorageValue::Float(_) | SqlStorageValue::Blob(_) => {
            Err(StoreError("unsupported sql column type"))
        }
    }
}

enum TxError<E> {
    /// The operation returned an error; its writes were rolled back.
    Operation(E),
    /// The runtime refused or failed the transaction.
    Runtime,
}

/// Runs `operation` inside `ctx.storage.transactionSync`, so its SQL writes
/// commit together or not at all. workers-rs 0.8.7 does not wrap
/// `transactionSync`, so it is called through the JS object.
fn transaction<T: 'static, E: 'static>(
    storage: &Storage,
    operation: impl FnOnce(&DoSql) -> std::result::Result<T, E> + 'static,
) -> std::result::Result<T, TxError<E>> {
    let sql = DoSql(storage.sql());
    let outcome: Rc<RefCell<Option<std::result::Result<T, E>>>> = Rc::new(RefCell::new(None));
    let slot = outcome.clone();
    let callback = Closure::once(move || -> std::result::Result<JsValue, JsValue> {
        let result = operation(&sql);
        let failed = result.is_err();
        *slot.borrow_mut() = Some(result);
        if failed {
            // Throwing makes transactionSync roll back.
            Err(JsValue::from_str("rollback"))
        } else {
            Ok(JsValue::UNDEFINED)
        }
    });
    let raw: &JsValue = storage.as_raw();
    let function = js_sys::Reflect::get(raw, &JsValue::from_str("transactionSync"))
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .ok_or(TxError::Runtime)?;
    let thrown = function
        .call1(raw, callback.as_ref().unchecked_ref())
        .is_err();
    let result = outcome.borrow_mut().take();
    match result {
        Some(Ok(value)) if !thrown => Ok(value),
        Some(Err(error)) => Err(TxError::Operation(error)),
        _ => Err(TxError::Runtime),
    }
}

/// Migration entry point for a Durable Object class. Runs once per instance
/// start, before any request is served; a failure refuses all requests.
fn migrate_storage(
    storage: &Storage,
    schema: &'static Schema,
) -> std::result::Result<u32, &'static str> {
    match transaction(storage, move |sql| sql::migrate(sql, schema)) {
        Ok(report) => Ok(report.to),
        Err(TxError::Operation(sql::MigrationError::NewerStoredVersion { .. })) => {
            Err("stored_schema_newer")
        }
        Err(TxError::Operation(sql::MigrationError::Family)) => Err("schema_family"),
        Err(_) => Err("migration_failed"),
    }
}

fn error(code: &str, status: u16) -> Result<Response> {
    let mut response = Response::error(code, status)?;
    response.headers_mut().set("Cache-Control", "no-store")?;
    Ok(response)
}

fn json<T: Serialize>(value: &T) -> Result<Response> {
    let mut response = Response::from_json(value)?;
    response.headers_mut().set("Cache-Control", "no-store")?;
    Ok(response)
}

// ---------------------------------------------------------- ChainJournal

/// The Worker-to-journal message for one finished RPC round.
#[derive(Serialize, Deserialize)]
struct RpcRound {
    result: RpcResult,
    more: bool,
    holder: String,
}

#[derive(Serialize, Deserialize)]
struct ClearRequest {
    fault_id: u64,
    #[serde(default)]
    reanchor: bool,
}

/// The hotkey secret, read from the Worker secret only while signing.
#[derive(Clone)]
struct EnvKeys(Env);

impl KeySource for EnvKeys {
    fn hotkey(&self) -> std::result::Result<Hotkey, ChainError> {
        let secret = self
            .0
            .secret("COORDINATOR_HOTKEY_SEED")
            .map_err(|_| ChainError::new("hotkey_secret_missing", ""))?;
        let seed = Zeroizing::new(secret.to_string());
        Hotkey::from_seed_hex(&seed)
    }
}

/// The authorization signing key, from the Worker secret
/// `AUTHORIZATION_SIGNING_KEY`, read only by the ChainJournal while it signs
/// a closed cohort's authorization (`signing::auto`). It must derive the
/// trusted authorization root and differ from the hotkey's seed; it never
/// leaves the journal except as the authorization's signature.
impl signing::auto::AuthorizationKeySource for EnvKeys {
    fn authorization_key(
        &self,
        keys: &slop_ninja_protocol::TrustedKeys,
    ) -> std::result::Result<slop_ninja_protocol::SigningKey, &'static str> {
        let seed = self.0.secret("AUTHORIZATION_SIGNING_KEY").map_err(|_| "authorization_key_missing")?;
        let seed = Zeroizing::new(seed.to_string());
        let hotkey = self.0.secret("COORDINATOR_HOTKEY_SEED").ok().map(|s| Zeroizing::new(s.to_string()));
        signing::auto::authorization_key_from(&seed, hotkey.as_ref().map(|h| h.as_str()), keys)
    }

    /// The per-pass check reads the authorization secret alone: the hotkey's
    /// seed is compared only while an authorization is signed.
    fn check_key(&self, keys: &slop_ninja_protocol::TrustedKeys) -> std::result::Result<(), &'static str> {
        let seed = self.0.secret("AUTHORIZATION_SIGNING_KEY").map_err(|_| "authorization_key_missing")?;
        let seed = Zeroizing::new(seed.to_string());
        signing::auto::authorization_key_from(&seed, None, keys).map(|_| ())
    }
}

#[derive(Serialize, Deserialize)]
struct StepRequest {
    holder: String,
    /// The worst-case time the step may take (`signing::step_worst_ms`);
    /// absent from an older Worker: no bound.
    #[serde(default)]
    budget_ms: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct MarkRequest {
    claim: crate::claims::ClaimToken,
    op_id: String,
    attempt: u32,
    #[serde(default)]
    stall: bool,
}

#[derive(Serialize, Deserialize)]
struct ResultRequest {
    claim: crate::claims::ClaimToken,
    outcome: SignOutcome,
    /// Whether the round may run another step after this one.
    #[serde(default = "more_default")]
    more: bool,
    /// The worst-case time the next step may take; absent: no bound.
    #[serde(default)]
    budget_ms: Option<u64>,
}

fn more_default() -> bool {
    true
}

/// `/sign-pointer-object`: the SHA-256 of an object the pointer names.
#[derive(Deserialize)]
struct PointerObjectRequest {
    sha256: String,
}

/// `/sign-auto`: one bounded pass of automatic target handoff; every bucket
/// operation of the pass shares a deadline `budget_ms` from its start (at
/// most [`AUTO_PASS_MS`]).
#[derive(Serialize, Deserialize)]
struct AutoRequest {
    budget_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct AdminRequest {
    action: String,
    #[serde(default)]
    bundle: Option<String>,
    #[serde(default)]
    drill: Option<String>,
    #[serde(default)]
    count: Option<u64>,
    /// `restart_target`: the restarted record's first epoch.
    #[serde(default)]
    from_epoch: Option<u64>,
    /// `revoke`: the Worker's RPC reading of the subnet epoch index at the
    /// finalized cursor, when it has one (`signing::install_revocation`).
    #[serde(default)]
    observed_epoch: Option<u64>,
    /// `auto_abandoned` / `auto_unabandoned`: the cohort label the operator
    /// records as abandoned, or takes that record back for.
    #[serde(default)]
    label: Option<String>,
    /// `decide`: the operator decision's request body
    /// (`signing::decide::decide`).
    #[serde(default)]
    operator: Option<serde_json::Value>,
    /// `auto_gap`: the cohort number the operator records as having no
    /// main-record label.
    #[serde(default)]
    cohort: Option<u64>,
}

/// Authoritative chain observation state for one chain: delivery dedup, the
/// per-block path, the finalized cursor, faults, the watchdog mode, claims,
/// the RPC budget and the coordinator's own signing journal (signer
/// ownership, nonce allocation, attempts and reconciliation). It sets no
/// alarms: deliveries and the cron watchdog are its only wakeups. Its
/// wasm-bindgen class is hand-written (`cf_arena::ChainJournalExport`); the
/// object itself lives in the isolate's home scope.
pub struct ChainJournal {
    state: State,
    env: Env,
    schema: std::result::Result<u32, &'static str>,
    settings: std::result::Result<Settings, ConfigError>,
    sign: std::result::Result<(SignConfig, slop_ninja_protocol::TrustedKeys), ConfigError>,
    runtime: Rc<RuntimeCache>,
    /// This instance's construction time and requests served, and the Wasm
    /// memory it holds, reported on `/status` to diagnose restarts.
    started_ms: u64,
    requests: std::cell::Cell<u64>,
}

fn sign_setup(
    env: &Env,
    settings: &std::result::Result<Settings, ConfigError>,
) -> std::result::Result<(SignConfig, slop_ninja_protocol::TrustedKeys), ConfigError> {
    let s = settings.as_ref().map_err(|e| *e)?;
    let genesis = var(env, "CHAIN_GENESIS").ok_or(ConfigError::Chain)?;
    let chain = config::chain_config(&genesis, var(env, "SUBNET_NETUID").as_deref())?;
    config::sign_config(|name| var(env, name), &chain, s)
}

impl worker::has_durable_object_attribute for ChainJournal {}

/// Runs a synchronous step (a constructor's storage migration) in its own
/// scope, so its transient allocations never reach the scope it is called
/// from; when that scope cannot be admitted it runs where it is (the State
/// home, counted there as an overdraft). The fallback is deliberate: a
/// constructor runs once per instance, so a refusal surfaced as a schema
/// error would answer 503 for the instance's whole life. `R` holds no heap.
fn sync_scoped<R>(route: slop_ninja_arena::Route, control: usize, f: impl FnOnce() -> R) -> R {
    match slop_ninja_arena::Scope::open(route, control) {
        Ok(scope) => {
            let out = scope.enter(f);
            let _ = scope.close();
            out
        }
        Err(_) => f(),
    }
}

impl DurableObject for ChainJournal {
    fn new(state: State, env: Env) -> Self {
        let schema = sync_scoped(crate::arena_routes::R_JOURNAL_NEW, crate::arena_routes::CONSTRUCTOR_CONTROL_BYTES, || {
            let schema = migrate_storage(&state.storage(), &CHAIN_JOURNAL_SCHEMA);
            if schema.is_ok() {
                // Every construction (deploy, eviction, or the Wasm rebuild
                // after a trap) is counted, so restarts are visible on
                // `/status`.
                let _ = transaction(&state.storage(), |sql| {
                    crate::journal::bump(sql, "chain_journal_starts")
                });
            }
            schema
        });
        let settings = settings(&env);
        let sign = sign_setup(&env, &settings);
        Self {
            state,
            env,
            schema,
            settings,
            sign,
            runtime: Rc::new(RuntimeCache::default()),
            started_ms: now_ms(),
            requests: std::cell::Cell::new(0),
        }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        self.requests.set(self.requests.get().saturating_add(1));
        let version = match self.schema {
            Ok(version) => version,
            Err(code) => return error(code, 503),
        };
        let Ok(s) = self.settings else {
            return error("bad_settings", 503);
        };
        let storage = self.state.storage();
        let now = now_ms();
        match (req.method(), req.path().as_str()) {
            (Method::Post, "/ingest") => {
                let n = match cf_io::request_json::<BlockNotification>(&mut req, INGEST_BODY_BYTES).await {
                    Ok(v) => v,
                    Err(e) => return error(e.code(), e.status()),
                };
                match transaction(&storage, move |sql| {
                    let report = heartbeat::on_delivery(sql, &s, &n, now)?;
                    // Testnet lost-acknowledgement drill: a new delivery is
                    // committed but answered 503, so the provider retries it.
                    let drill = report.outcome == crate::journal::DeliveryOutcome::Accepted
                        && signing::take_drill(sql, &["ack_failure"])
                            .map_err(IngestError::Store)?
                            .is_some();
                    if drill {
                        signing::note(sql, now, Some(n.header.number), "drill_ack_failure", None)
                            .map_err(IngestError::Store)?;
                    }
                    Ok::<_, IngestError>((report, drill))
                }) {
                    Ok((report, drill)) => {
                        let mut value = serde_json::to_value(&report)?;
                        if let Some(map) = value.as_object_mut() {
                            map.insert("drill_ack_failure".into(), serde_json::Value::Bool(drill));
                        }
                        json(&value)
                    }
                    Err(TxError::Operation(IngestError::DeliveryConflict)) => {
                        error("delivery_conflict", 409)
                    }
                    Err(TxError::Operation(IngestError::EventConflict)) => {
                        error("event_conflict", 409)
                    }
                    Err(_) => error("store_unavailable", 503),
                }
            }
            (Method::Post, "/tick") => {
                match transaction(&storage, move |sql| heartbeat::on_tick(sql, &s, now)) {
                    Ok(report) => json(&report),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            (Method::Post, "/rpc-result") => {
                let round = match cf_io::request_json::<RpcRound>(&mut req, RPC_RESULT_BODY_BYTES).await {
                    Ok(v) => v,
                    Err(e) => return error(e.code(), e.status()),
                };
                match transaction(&storage, move |sql| {
                    heartbeat::on_rpc_result(sql, &s, &round.result, round.more, &round.holder, now)
                }) {
                    Ok(step) => json(&step),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            (Method::Post, "/clear-fault") => {
                let clear = match cf_io::request_json::<ClearRequest>(&mut req, SMALL_BODY_BYTES).await {
                    Ok(v) => v,
                    Err(e) => return error(e.code(), e.status()),
                };
                match transaction(&storage, move |sql| {
                    heartbeat::clear_fault(sql, &s, clear.fault_id, clear.reanchor, now)
                }) {
                    Ok((outcome, _)) => json(&outcome),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            (Method::Get, "/confirmed") => {
                match transaction(&storage, crate::journal::confirmed) {
                    Ok(c) => json(&c),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            // The confirmed watermark with the RPC finalized head it trails:
            // a tick reads the epoch at both, so the cohort authority knows
            // whether its epoch observation is the head's (a live epoch) or a
            // catch-up replay (`eval::engine::ACTIVE_TEMPO_RULE`).
            (Method::Get, "/confirmed-head") => {
                match transaction(&storage, |sql| {
                    Ok::<_, StoreError>(ConfirmedHead {
                        confirmed: crate::journal::confirmed(sql)?,
                        head: crate::journal::rpc_finalized_head(sql)?,
                    })
                }) {
                    Ok(c) => json(&c),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            (Method::Post, "/rpc-reserve") => {
                let per_minute = s.rpc_calls_per_minute;
                match transaction(&storage, move |sql| {
                    crate::claims::reserve_rpc(sql, 1, per_minute, now)
                }) {
                    Ok(granted) => json(&granted),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            (Method::Get, "/status") => {
                let cfg = self.sign.as_ref().ok().map(|(c, _)| c.clone());
                match transaction(&storage, move |sql| {
                    let status = heartbeat::status(sql, &s, version, now)?;
                    let signing = match &cfg {
                        Some(c) => Some(signing::status(sql, c, 6, now)?),
                        None => None,
                    };
                    // The latest registration read (public chain facts:
                    // the publisher's `registry.json`).
                    let registry = signing::registry::view(sql)?;
                    Ok::<_, StoreError>((status, signing, registry))
                }) {
                    Ok((status, signing, registry)) => {
                        let mut value = serde_json::to_value(&status)?;
                        let signing = match signing {
                            Some(sg) => serde_json::to_value(&sg)?,
                            None => serde_json::json!({"configured": false}),
                        };
                        let instance = serde_json::json!({
                            "age_ms": now.saturating_sub(self.started_ms),
                            "requests": self.requests.get(),
                            "wasm_memory_bytes": core::arch::wasm32::memory_size(0) as u64 * 65_536,
                            "arena": arena_exports::arena_status(),
                        });
                        if let Some(map) = value.as_object_mut() {
                            map.insert("signing".into(), signing);
                            map.insert("registry".into(), registry);
                            map.insert("instance".into(), instance);
                        }
                        json(&value)
                    }
                    Err(_) => error("store_unavailable", 503),
                }
            }
            (Method::Post, path) if path.starts_with("/sign-") => {
                self.signing_route(req, path.to_owned(), now).await
            }
            _ => error("not_found", 404),
        }
    }
}

impl ChainJournal {
    async fn signing_route(&self, mut req: Request, path: String, now: u64) -> Result<Response> {
        let Ok((cfg, trusted)) = self.sign.clone() else {
            return error("signing_not_configured", 503);
        };
        let storage = self.state.storage();
        let runtime = self.runtime.clone();
        let env = self.env.clone();
        match path.as_str() {
            "/sign-step" => {
                let r = match cf_io::request_json::<StepRequest>(&mut req, SMALL_BODY_BYTES).await {
                    Ok(v) => v,
                    Err(e) => return error(e.code(), e.status()),
                };
                // Testnet trap drill: the drill is taken (and logged) in its
                // own committed transaction, then this instance panics outside
                // any transaction. workers-rs marks the Wasm instance broken
                // and rebuilds it on the next request; the drill measures that.
                let trap = transaction(&storage, move |sql| {
                    let taken = signing::take_drill(sql, &["trap"])?.is_some();
                    if taken {
                        signing::note(sql, now, None, "drill_trap", None)?;
                    }
                    Ok::<_, StoreError>(taken)
                });
                if matches!(trap, Ok(true)) {
                    drill_trap();
                }
                match transaction(&storage, move |sql| {
                    let keys = EnvKeys(env);
                    let ctx = Ctx {
                        cfg: &cfg,
                        trusted: &trusted,
                        runtime: &runtime,
                        keys: &keys,
                    };
                    signing::plan_within(sql, &ctx, &r.holder, r.budget_ms, now)
                }) {
                    Ok(plan) => json(&plan),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            "/sign-mark" => {
                let r = match cf_io::request_json::<MarkRequest>(&mut req, SMALL_BODY_BYTES).await {
                    Ok(v) => v,
                    Err(e) => return error(e.code(), e.status()),
                };
                match transaction(&storage, move |sql| {
                    signing::mark(sql, &r.claim, &r.op_id, r.attempt, now)
                }) {
                    Ok(marked) => json(&marked),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            "/sign-result" => {
                let r = match cf_io::request_json::<ResultRequest>(&mut req, SIGN_RESULT_BODY_BYTES).await {
                    Ok(v) => v,
                    Err(e) => return error(e.code(), e.status()),
                };
                match transaction(&storage, move |sql| {
                    let keys = EnvKeys(env);
                    let ctx = Ctx {
                        cfg: &cfg,
                        trusted: &trusted,
                        runtime: &runtime,
                        keys: &keys,
                    };
                    signing::on_result_within(sql, &ctx, &r.claim, &r.outcome, r.more, r.budget_ms, now)
                }) {
                    Ok(step) => json(&step),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            "/sign-stall" => {
                // Stale-invocation drill: shorten the holder's lease.
                let claim = match cf_io::request_json::<crate::claims::ClaimToken>(&mut req, SMALL_BODY_BYTES).await {
                    Ok(v) => v,
                    Err(e) => return error(e.code(), e.status()),
                };
                match transaction(&storage, move |sql| {
                    signing::shorten_claim(sql, &claim, DRILL_STALL_LEASE_MS, now)
                }) {
                    Ok(ok) => json(&ok),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            "/sign-admin" => {
                let r = match cf_io::request_json::<AdminRequest>(&mut req, SIGN_ADMIN_BODY_BYTES).await {
                    Ok(v) => v,
                    Err(e) => return error(e.code(), e.status()),
                };
                let op_key = crate::operator::configured_key(env.var("OPERATOR_DECISION_KEY").ok().map(|v| v.to_string()).as_deref());
                match transaction(&storage, move |sql| admin(sql, &cfg, &trusted, &r, op_key, now)) {
                    Ok(v) => json(&v),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            // The coordinator's latest derived row is signed as the row other
            // validators copy (`signing::rows`) before the plan names it. The
            // refresh runs in its own transaction: whatever it fails with is
            // recorded (`row_record_store`) and the pointer, with its
            // revocation set, is planned regardless.
            "/sign-pointer" => {
                let (rcfg, rtrusted) = (cfg.clone(), trusted.clone());
                if transaction(&storage, move |sql| signing::rows::refresh(sql, &rcfg, &rtrusted, &EnvKeys(env), now)).is_err() {
                    let _ = transaction(&storage, move |sql| signing::rows::note_error(sql, "row_record_store", now));
                }
                match transaction(&storage, move |sql| signing::pointer_plan(sql, &cfg, &trusted)) {
                    Ok(plan) => json(&plan),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            "/sign-pointer-object" => {
                let r = match cf_io::request_json::<PointerObjectRequest>(&mut req, SMALL_BODY_BYTES).await {
                    Ok(v) => v,
                    Err(e) => return error(e.code(), e.status()),
                };
                match transaction(&storage, move |sql| signing::pointer_object(sql, &cfg, &trusted, &r.sha256)) {
                    Ok(Some(bytes)) => {
                        let mut response = Response::from_bytes(bytes)?;
                        response.headers_mut().set("content-type", "application/octet-stream")?;
                        Ok(response)
                    }
                    Ok(None) => error("not_found", 404),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            "/sign-auto" => {
                let r = match cf_io::request_json::<AutoRequest>(&mut req, SMALL_BODY_BYTES).await {
                    Ok(v) => v,
                    Err(e) => return error(e.code(), e.status()),
                };
                let deadline = now.saturating_add(r.budget_ms.min(AUTO_PASS_MS));
                json(&self.auto_pass(cfg, trusted, deadline).await)
            }
            "/sign-pointer-outcome" => {
                let r = match cf_io::request_json::<signing::PointerOutcome>(&mut req, SMALL_BODY_BYTES).await {
                    Ok(v) => v,
                    Err(e) => return error(e.code(), e.status()),
                };
                match transaction(&storage, move |sql| signing::pointer_outcome(sql, &r, now)) {
                    Ok(changed) => json(&changed),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            _ => error("not_found", 404),
        }
    }
}

/// The ChainJournal's side of an automatic-handoff pass
/// (`signing::auto::run_pass`): its storage's transactions, the receipts
/// bucket with every operation raced against the pass's deadline, and the
/// clock.
struct AutoIo {
    storage: Storage,
    bucket: worker::Bucket,
    deadline: u64,
}

impl signing::auto::PassIo for AutoIo {
    type Sql = DoSql;

    fn tx<T: 'static>(
        &self,
        f: impl FnOnce(&DoSql) -> std::result::Result<T, StoreError> + 'static,
    ) -> std::result::Result<T, StoreError> {
        transaction(&self.storage, f).map_err(|e| match e {
            TxError::Operation(e) => e,
            TxError::Runtime => StoreError("transaction"),
        })
    }

    async fn list(&mut self, prefix: &str, cursor: Option<&str>) -> std::result::Result<(Vec<String>, Option<String>), ()> {
        cf_io::r2_list_prefixes(&self.bucket, prefix, "/", cursor, Some(self.deadline)).await.map_err(|_| ())
    }

    async fn read(&mut self, key: &str, bound: usize) -> std::result::Result<Option<Vec<u8>>, signing::auto::ReadError> {
        match cf_io::r2_bytes(&self.bucket, key, bound, Some(self.deadline)).await {
            Ok(b) => Ok(b),
            Err(cf_io::BodyError::TooLarge) => Err(signing::auto::ReadError::TooLarge),
            Err(_) => Err(signing::auto::ReadError::Unavailable),
        }
    }

    async fn exists(&mut self, key: &str) -> std::result::Result<bool, ()> {
        cf_io::r2_size(&self.bucket, key, Some(self.deadline)).await.map(|s| s.is_some()).map_err(|_| ())
    }

    async fn fold(&mut self, key: &str, f: &mut dyn FnMut(&[u8])) -> std::result::Result<bool, ()> {
        cf_io::r2_fold(&self.bucket, key, Some(self.deadline), f).await.map_err(|_| ())
    }

    fn now_ms(&self) -> u64 {
        now_ms()
    }

    fn past_deadline(&self) -> bool {
        now_ms() >= self.deadline
    }
}

impl ChainJournal {
    /// One bounded pass of automatic target handoff (`signing::auto`).
    async fn auto_pass(
        &self,
        cfg: SignConfig,
        trusted: slop_ninja_protocol::TrustedKeys,
        deadline: u64,
    ) -> signing::auto::PassReport {
        let storage = self.state.storage();
        let Ok(bucket) = self.env.bucket(eval_do::RECEIPTS_BUCKET) else {
            let _ = transaction(&storage, move |sql| signing::auto::note_failure(sql, "", "bucket_binding", now_ms()));
            return signing::auto::PassReport { error: Some("bucket_binding".into()), ..Default::default() };
        };
        let mut io = AutoIo { storage, bucket, deadline };
        // The main chain record's eval authorities (`AUTO_TARGET_AUTHORITIES`,
        // tracked in wrangler.toml beside the eval Worker's own).
        let authorities = self.env.var("AUTO_TARGET_AUTHORITIES").ok().map(|v| v.to_string());
        let record = signing::auto::Record::parse(authorities.as_deref());
        signing::auto::run_pass(&mut io, &cfg, &trusted, record.as_ref().map_err(|e| *e), &EnvKeys(self.env.clone())).await
    }
}

fn admin(
    sql: &DoSql,
    cfg: &SignConfig,
    trusted: &slop_ninja_protocol::TrustedKeys,
    r: &AdminRequest,
    op_key: std::result::Result<Option<slop_ninja_protocol::operator::OperatorKey>, &'static str>,
    now: u64,
) -> std::result::Result<serde_json::Value, StoreError> {
    use serde_json::json;
    Ok(match r.action.as_str() {
        // Operator decisions through the shared request mechanism.
        "decide" => match op_key {
            Err(code) => json!({"refused": code}),
            Ok(key) => match signing::decide::decide(
                sql,
                cfg,
                trusted,
                r.operator.clone().unwrap_or_default(),
                key.as_ref(),
                r.observed_epoch,
                now,
            )? {
                Ok(v) => v,
                Err(code) => json!({"refused": code}),
            },
        },
        "install_target" => match signing::install_target(
            sql,
            cfg,
            trusted,
            r.bundle.as_deref().unwrap_or_default(),
            now,
        )? {
            Ok(digest) => json!({"installed": digest}),
            Err(refusal) => json!({"refused": refusal}),
        },
        "resume" => json!({"resumed": signing::resume(sql, now)?}),
        "pause" => {
            signing::pause(sql, "operator_hold", now)?;
            json!({"paused": true})
        }
        "clear_fault" => json!({"cleared": signing::clear_fault(sql, now)?}),
        // The operator's hold on automatic target handoff; a resume
        // continues from the same cohort (`signing::auto`).
        "auto_pause" => json!({"auto_paused": signing::auto::set_paused(sql, true, now)?}),
        "auto_resume" => json!({"auto_resumed": signing::auto::set_paused(sql, false, now)?}),
        // The operator's positive evidence that an in-record cohort was
        // abandoned and will never close: the cohorts after it may then be
        // handed in (`signing::auto::mark_abandoned`).
        "auto_abandoned" => match signing::auto::mark_abandoned(sql, r.label.as_deref().unwrap_or_default(), now)? {
            Ok(cohort) => json!({"abandoned": cohort}),
            Err(code) => json!({"refused": code}),
        },
        // Takes back an `auto_abandoned` record while nothing after it was
        // handled (`signing::auto::unmark_abandoned`).
        "auto_unabandoned" => match signing::auto::unmark_abandoned(sql, r.label.as_deref().unwrap_or_default(), now)? {
            Ok(cohort) => json!({"unabandoned": cohort}),
            Err(code) => json!({"refused": code}),
        },
        // A cohort number with no main-record label: the numbering
        // continues past it (`signing::auto::mark_gap`).
        "auto_gap" => match r.cohort {
            Some(c) => match signing::auto::mark_gap(sql, c, now)? {
                Ok(cohort) => json!({"gap": cohort}),
                Err(code) => json!({"refused": code}),
            },
            None => json!({"refused": "cohort_missing"}),
        },
        // A chain record restarted under the same trust roots: the
        // restarted record's launch bundle replaces the held target in one
        // transaction (`signing::restart_target`); a refusal changes nothing.
        "restart_target" => match signing::restart_target(
            sql,
            cfg,
            trusted,
            r.bundle.as_deref().unwrap_or_default(),
            r.from_epoch.unwrap_or(u64::MAX),
            now,
        )? {
            Ok(done) => json!({"restarted": done}),
            Err(refusal) => json!({"refused": refusal}),
        },
        // A signed revocation (hex): journaled and applied at once, refused
        // with nothing changed (`signing::install_revocation`).
        "revoke" => match signing::install_revocation(
            sql,
            cfg,
            trusted,
            r.bundle.as_deref().unwrap_or_default(),
            r.observed_epoch,
            now,
        )? {
            Ok(done) => json!({"revoked": done}),
            Err(refusal) => json!({"refused": refusal}),
        },
        "arm_drill" => json!({
            "armed": signing::arm_drill(sql, r.drill.as_deref().unwrap_or_default(), r.count.unwrap_or(1).min(3), now)?
        }),
        _ => json!({"error": "unknown_action"}),
    })
}

/// The trap drill's deliberate panic (never reached unless armed).
#[allow(clippy::panic)]
fn drill_trap() -> ! {
    panic!("testnet drill: trap in the ChainJournal");
}

// ------------------------------------------------------------ EvalCohort

// Hidden evaluation: the EvalCohort authority (M4) and the fan-out lane
// objects (AScorer, EvalLane, GenLimiter, SpendLedger). Exempt from the
// crate's panic lints until converted (see the crate documentation).
#[cfg_attr(
    not(test),
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
#[path = "cf_eval.rs"]
mod eval_do;
#[cfg_attr(
    not(test),
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
#[path = "cf_lanes.rs"]
mod lanes_do;

#[path = "cf_admission.rs"]
mod admission_do;
#[path = "cf_gen.rs"]
mod gen_do;
#[path = "cf_reference.rs"]
mod reference_do;
#[path = "cf_arena.rs"]
mod arena_exports;
#[path = "cf_io.rs"]
mod cf_io;

// ------------------------------------------------------------ Worker side

/// Everything a Worker invocation needs, checked once per invocation.
#[derive(Clone)]
struct Deployment {
    network: &'static str,
    /// Which deployment this is; it selects the liveness record.
    role: config::Role,
    chain: ChainConfig,
    settings: Settings,
    rpc_url: String,
}

fn deployment(env: &Env) -> std::result::Result<Deployment, ConfigError> {
    let network = config::network(var(env, "NETWORK").as_deref())?;
    let genesis = var(env, "CHAIN_GENESIS").ok_or(ConfigError::Chain)?;
    let chain = config::chain_config(&genesis, var(env, "SUBNET_NETUID").as_deref())?;
    Ok(Deployment {
        network,
        role: role(env),
        chain,
        settings: settings(env)?,
        rpc_url: config::rpc_url(var(env, "RPC_URL").as_deref())?,
    })
}

fn receiver_config(
    env: &Env,
    chain: &ChainConfig,
) -> std::result::Result<ReceiverConfig, ConfigError> {
    let key_id = var(env, "RTAO_WEBHOOK_KEY_ID").unwrap_or_else(|| DEFAULT_KEY_ID.to_owned());
    let secret = Zeroizing::new(
        env.secret("RTAO_WEBHOOK_SIGNING_SECRET")
            .map_err(|_| ConfigError::SigningSecret)?
            .to_string(),
    );
    let subscription = env
        .secret("RTAO_WEBHOOK_SUBSCRIPTION_ID")
        .map_err(|_| ConfigError::Subscription)?
        .to_string();
    config::receiver_config(chain, &key_id, &secret, &subscription)
}

/// A Durable Object call. `timeout_ms` is an `AbortSignal.timeout` the
/// platform enforces on the call and its reply body (`cf_io::internal_request`),
/// so a slow object never holds the caller past it and no pending future is
/// dropped; `None` only for operator routes whose work has no bound yet.
#[allow(clippy::too_many_arguments)]
async fn call_do(
    env: &Env,
    d: &Deployment,
    base: &str,
    name: &str,
    method: Method,
    path: &str,
    body: Option<String>,
    timeout_ms: Option<u64>,
) -> Result<Response> {
    let binding = config::binding(base, d.network);
    let stub = env.durable_object(&binding)?.get_by_name(name)?;
    let request = cf_io::internal_request(&format!("{INTERNAL_ORIGIN}{path}"), method, body.as_deref(), timeout_ms)?;
    stub.fetch_with_request(request).await
}

/// A ChainJournal call on the heartbeat, cron and signing paths, bounded by
/// `JOURNAL_CALL_MS`, the allowance every budget charges for it.
async fn journal_call(
    env: &Env,
    d: &Deployment,
    method: Method,
    path: &str,
    body: Option<String>,
) -> Result<Response> {
    journal_call_within(env, d, method, path, body, Some(JOURNAL_CALL_MS)).await
}

async fn journal_call_within(
    env: &Env,
    d: &Deployment,
    method: Method,
    path: &str,
    body: Option<String>,
    timeout_ms: Option<u64>,
) -> Result<Response> {
    let name = config::chain_journal_name(&d.chain);
    call_do(env, d, CHAIN_JOURNAL, &name, method, path, body, timeout_ms).await
}

/// The bound of a Durable Object reply a route holds whole: its control
/// footprint (a reply the route's admitted heap could not hold is refused
/// as `body_too_large` instead of overdrawing the scope). The heartbeat
/// paths hold a delivery's (`HOOK_CONTROL_BYTES`) or a continuation's and a
/// tick's (`CONTINUATION_CONTROL_BYTES`); the status and operator routes the
/// Worker routes' (`WORKER_CONTROL_BYTES`).
const HOOK_REPLY_BYTES: usize = crate::arena_routes::HOOK_CONTROL_BYTES;
const ROUND_REPLY_BYTES: usize = crate::arena_routes::CONTINUATION_CONTROL_BYTES;
const ROUTE_REPLY_BYTES: usize = crate::arena_routes::WORKER_CONTROL_BYTES;

fn eval_bucket(env: &Env, d: &Deployment) -> Option<worker::Bucket> {
    env.bucket(&config::binding("EVAL", d.network)).ok()
}

async fn read_liveness(env: &Env, d: &Deployment) -> Option<liveness::Liveness> {
    let bucket = eval_bucket(env, d)?;
    // A streamed, bounded read of the small record (`cf_io::r2_bytes`),
    // raced against its timeout in JS.
    let deadline = now_ms().saturating_add(liveness::TIMEOUT_MS);
    let bytes = cf_io::r2_bytes(&bucket, liveness::key(d.role), liveness::MAX_RECORD_BYTES, Some(deadline))
        .await
        .ok()??;
    serde_json::from_slice(&bytes).ok()
}

/// Records one timed journal call in the external liveness object.
async fn write_liveness(env: &Env, d: &Deployment, ok: bool, latency_ms: u64, status: Option<u16>) {
    let prev = read_liveness(env, d).await;
    let record = liveness::next(prev.as_ref(), now_ms(), ok, latency_ms, status);
    let (Some(bucket), Ok(body)) = (eval_bucket(env, d), serde_json::to_vec(&record)) else {
        return;
    };
    let deadline = now_ms().saturating_add(liveness::TIMEOUT_MS);
    let _ = cf_io::r2_put(&bucket, liveness::key(d.role), &body, Some(deadline)).await;
}

// ------------------------------------------------------- RPC transport

/// One JSON-RPC call over `fetch`, bounded in time and size.
async fn rpc_post(url: &str, body: String) -> std::result::Result<Vec<u8>, RpcError> {
    rpc_post_capped(url, body, rpc::MAX_RESPONSE_BYTES, rpc::CALL_TIMEOUT_MS).await
}

async fn rpc_post_capped(
    url: &str,
    body: String,
    cap: usize,
    timeout_ms: u64,
) -> std::result::Result<Vec<u8>, RpcError> {
    let mut response = rpc_send(url, body, timeout_ms).await?;
    // A declared exact length above the cap is refused before reading; the
    // reply is read into one admitted block, and a stream past the cap is
    // refused (`cf_io`).
    match cf_io::response_bytes(&mut response, cap).await {
        Ok(body) => Ok(body),
        Err(cf_io::BodyError::TooLarge) => Err(RpcError::TooLarge),
        Err(_) => Err(RpcError::Transport),
    }
}

/// `chain_getBlock` at `at`, matched against `extrinsic` while its reply
/// streams in (`rpc::BlockScan`): the body is never held, so a block of any
/// size the chain allows (an upgrade block carries the runtime code) is
/// read in one scratch block, and only the match crosses to the journal.
async fn block_scan(
    url: &str,
    at: &BlockRef,
    extrinsic: &str,
) -> std::result::Result<rpc::BlockMatch, RpcError> {
    let mut response = rpc_send(url, rpc::block_request(14, &at.hash), rpc::BLOCK_TIMEOUT_MS).await?;
    let mut scan = rpc::BlockScan::new(14, at.number, extrinsic);
    match cf_io::response_scan(&mut response, &mut |part| scan.feed(part)).await {
        Ok(()) => scan.finish(),
        Err(_) => Err(RpcError::Transport),
    }
}

/// Sends one JSON-RPC POST; a 200 answer's body is left to the caller.
async fn rpc_send(url: &str, body: String, timeout_ms: u64) -> std::result::Result<Response, RpcError> {
    let headers = Headers::new();
    for (k, v) in rpc::REQUEST_HEADERS {
        headers.set(k, v).map_err(|_| RpcError::Transport)?;
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(JsValue::from_str(&body)));
    let request = Request::new_with_init(url, &init).map_err(|_| RpcError::Transport)?;
    // The platform's own timeout (`AbortSignal.timeout`) bounds the whole
    // call, the reply body included: the fetch or the body read rejects at
    // the deadline, so no pending future is ever dropped (design section
    // 2.6; `select` with `Delay` is not used).
    let signal = worker::AbortSignal::from(worker::worker_sys::web_sys::AbortSignal::timeout_with_u32(
        u32::try_from(timeout_ms).unwrap_or(u32::MAX),
    ));
    let mut response = Fetch::Request(request)
        .send_with_signal(&signal)
        .await
        .map_err(|_| RpcError::Transport)?;
    match response.status_code() {
        200 => Ok(response),
        429 => {
            cf_io::discard(&mut response);
            Err(RpcError::RateLimited)
        }
        _ => {
            cf_io::discard(&mut response);
            Err(RpcError::Transport)
        }
    }
}

async fn finalized_head(url: &str) -> std::result::Result<FinalizedHeader, RpcError> {
    let body = rpc_post(url, rpc::finalized_head_request(1)).await?;
    let hash = rpc::parse_finalized_head(1, &body)?;
    let body = rpc_post(url, rpc::header_request(2, &hash)).await?;
    rpc::parse_header(2, &body, hash)
}

async fn execute(url: &str, plan: RpcPlan) -> RpcResult {
    match plan {
        RpcPlan::FinalizedHead {
            purpose,
            origin,
            claim,
        } => RpcResult::FinalizedHead {
            purpose,
            origin,
            claim,
            head: finalized_head(url).await,
        },
        RpcPlan::BlockHashes {
            from,
            to,
            origin,
            claim,
        } => {
            let hashes = match rpc::block_hashes_request(3, from, to) {
                Ok(body) => match rpc_post(url, body).await {
                    Ok(reply) => rpc::parse_block_hashes(3, &reply, to - from + 1),
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            };
            RpcResult::BlockHashes {
                from,
                to,
                origin,
                claim,
                hashes,
            }
        }
    }
}

/// The wall-clock budget of one run of rounds (a delivery's `waitUntil`
/// continuation, a cron tick, an operator kick). A round starts only when
/// its worst case, its RPC calls at their own timeouts (which the platform
/// enforces, `AbortSignal.timeout`) and its journal report, ends inside the
/// budget, so a continuation never outlives its 30 s `waitUntil` window
/// and leaves no scope abandoned; a round it does not start is carried over
/// by the next delivery or tick, as after any failure.
#[derive(Clone, Copy)]
struct Budget {
    until_ms: u64,
}

impl Budget {
    fn new(limit_ms: u64) -> Self {
        Budget { until_ms: now_ms().saturating_add(limit_ms) }
    }

    /// Whether work of at most `worst_ms` still ends inside the budget.
    fn allows(&self, worst_ms: u64) -> bool {
        now_ms().saturating_add(worst_ms) <= self.until_ms
    }

    /// The time left in the budget.
    fn left_ms(&self) -> u64 {
        self.until_ms.saturating_sub(now_ms())
    }

    /// What a step planned after one more journal call may still take: the
    /// time left, less that call's allowance. The journal plans only a step
    /// whose worst case fits it (`signing::step_worst_ms`), so a round never
    /// claims the signer for a step it cannot finish.
    fn after_call_ms(&self) -> u64 {
        self.until_ms.saturating_sub(now_ms()).saturating_sub(JOURNAL_CALL_MS)
    }
}

/// A journal call's allowance in a round's worst case: the bound past which
/// the watchdog already counts a journal call as failed
/// (`liveness::TIMEOUT_MS`).
const JOURNAL_CALL_MS: u64 = liveness::TIMEOUT_MS;

/// The worst case of one heartbeat RPC round and its report.
fn rpc_round_ms(plan: &RpcPlan) -> u64 {
    let calls = match plan {
        RpcPlan::FinalizedHead { .. } => 2,
        RpcPlan::BlockHashes { .. } => 1,
    };
    calls * rpc::CALL_TIMEOUT_MS + JOURNAL_CALL_MS
}

/// The worst case of one signing step and its report
/// (`signing::step_worst_ms`, the same function the journal plans with).
fn sign_round_ms(plan: &SignPlan) -> u64 {
    signing::step_worst_ms(plan, JOURNAL_CALL_MS)
}

/// Runs `plan` and its follow-up rounds, at most `rounds` in all and only
/// while each fits `budget`, handing each result back to the journal. Every
/// failure ends the run; the claim then expires or is released, and a later
/// block or tick carries the work over.
async fn drive(env: &Env, d: &Deployment, plan: Option<RpcPlan>, rounds: u64, holder: String, budget: Budget) {
    let mut plan = plan;
    let mut round = 0;
    while let Some(p) = plan.take() {
        if !budget.allows(rpc_round_ms(&p)) {
            return;
        }
        round += 1;
        let result = execute(&d.rpc_url, p).await;
        let Ok(body) = serde_json::to_string(&RpcRound {
            result,
            more: round < rounds,
            holder: holder.clone(),
        }) else {
            return;
        };
        let Ok(mut response) = journal_call(env, d, Method::Post, "/rpc-result", Some(body)).await
        else {
            return;
        };
        if response.status_code() != 200 {
            cf_io::discard(&mut response);
            return;
        }
        let Ok(step) = cf_io::response_json::<Step>(&mut response, ROUND_REPLY_BYTES).await else {
            return;
        };
        plan = step.plan;
    }
}

// --------------------------------------------------------- signing driver

const LARGE: usize = rpc::MAX_LARGE_RESPONSE_BYTES;

async fn read_at(
    url: &str,
    at: &BlockRef,
    keys: &[String],
) -> std::result::Result<Vec<(String, Option<String>)>, RpcError> {
    let body = rpc_post_capped(
        url,
        rpc::query_storage_request(11, keys, &at.hash),
        LARGE,
        rpc::CALL_TIMEOUT_MS,
    )
    .await?;
    rpc::parse_query_storage(11, &body, keys, &at.hash)
}

/// The single submission. Only an HTTP 200 reply is parsed; every transport
/// failure, timeout or other status is uncertain.
async fn submit(url: &str, extrinsic: &str) -> SubmitOutcome {
    match rpc_post_capped(
        url,
        rpc::submit_request(15, extrinsic),
        rpc::MAX_RESPONSE_BYTES,
        rpc::CALL_TIMEOUT_MS,
    )
    .await
    {
        Ok(body) => rpc::parse_submit(15, &body),
        Err(RpcError::RateLimited) => SubmitOutcome::Uncertain {
            reason: "http_429".into(),
        },
        Err(e) => SubmitOutcome::Uncertain {
            reason: format!("transport_{e:?}").to_ascii_lowercase(),
        },
    }
}

async fn journal_json<T: serde::de::DeserializeOwned>(
    env: &Env,
    d: &Deployment,
    path: &str,
    body: String,
) -> Option<T> {
    let mut response = journal_call(env, d, Method::Post, path, Some(body))
        .await
        .ok()?;
    if response.status_code() != 200 {
        cf_io::discard(&mut response);
        return None;
    }
    cf_io::response_json::<T>(&mut response, ROUND_REPLY_BYTES).await.ok()
}

/// Performs one step's call. `None` means the invocation stops here without
/// reporting (a refused mark, or the eviction drill).
async fn execute_sign(env: &Env, d: &Deployment, plan: &SignPlan) -> Option<SignOutcome> {
    let url = d.rpc_url.as_str();
    Some(match plan {
        SignPlan::RuntimeVersion { at, .. } => {
            let reply = rpc_post(url, rpc::runtime_version_request(12, &at.hash)).await;
            let result = reply.and_then(|body| rpc::parse_runtime_version(12, &body));
            SignOutcome::RuntimeVersion { result }
        }
        SignPlan::Metadata { at, .. } => {
            let result = match rpc_post_capped(
                url,
                rpc::metadata_request(13, &at.hash),
                LARGE,
                rpc::METADATA_TIMEOUT_MS,
            )
            .await
            {
                Ok(body) => rpc::parse_large_hex(13, &body),
                Err(e) => Err(e),
            };
            SignOutcome::Metadata { result }
        }
        SignPlan::Read { at, keys, .. } => SignOutcome::Read {
            result: read_at(url, at, keys).await,
        },
        SignPlan::Block { at, extrinsic, .. } => SignOutcome::Block {
            result: block_scan(url, at, extrinsic).await,
        },
        SignPlan::Submit {
            claim,
            op_id,
            attempt,
            extrinsic,
            drill,
        } => {
            let stall = drill.as_deref() == Some("stall_before_mark");
            if stall {
                // Stale-invocation drill: this holder's lease is cut short and
                // it stalls past it before marking. Its mark must be refused.
                let _ =
                    journal_json::<bool>(env, d, "/sign-stall", serde_json::to_string(claim).ok()?)
                        .await;
                Delay::from(Duration::from_millis(DRILL_STALL_SLEEP_MS)).await;
            }
            let mark = MarkRequest {
                claim: claim.clone(),
                op_id: op_id.clone(),
                attempt: *attempt,
                stall,
            };
            let marked =
                journal_json::<bool>(env, d, "/sign-mark", serde_json::to_string(&mark).ok()?)
                    .await;
            if marked != Some(true) {
                if stall {
                    // Also show that its late outcome is fenced.
                    let late = ResultRequest {
                        claim: claim.clone(),
                        outcome: SignOutcome::Submit {
                            result: SubmitOutcome::Uncertain {
                                reason: "drill_stale_holder".into(),
                            },
                        },
                        more: false,
                        budget_ms: None,
                    };
                    let _ = journal_json::<SignStep>(
                        env,
                        d,
                        "/sign-result",
                        serde_json::to_string(&late).ok()?,
                    )
                    .await;
                }
                return None;
            }
            if drill.as_deref() == Some("never_send") {
                // Lost-request drill: marked but never sent, and reported as
                // uncertain. Only the era's expiry with the nonce unconsumed
                // may lead to a second attempt with the same nonce.
                return Some(SignOutcome::Submit {
                    result: SubmitOutcome::Uncertain {
                        reason: "drill_never_sent".into(),
                    },
                });
            }
            let result = submit(url, extrinsic).await;
            match drill.as_deref() {
                Some("uncertain_after_send") => {
                    let seen = match &result {
                        SubmitOutcome::Accepted { .. } => "accepted",
                        SubmitOutcome::Rejected { .. } => "rejected",
                        SubmitOutcome::Uncertain { .. } => "uncertain",
                    };
                    SignOutcome::Submit {
                        result: SubmitOutcome::Uncertain {
                            reason: format!("drill_response_discarded:{seen}"),
                        },
                    }
                }
                // Eviction drill: the invocation stops without reporting; the
                // operator then redeploys, which restarts the Durable Object.
                // `state.abort()` is not used: on 2026-10-30 it left the
                // instance unresponsive until a second redeploy.
                Some("evict_after_send") => return None,
                _ => SignOutcome::Submit { result },
            }
        }
    })
}

/// One bounded signing round: ask the journal for a step, make its call,
/// report, and continue with the step the journal returns, while each step
/// fits `budget`.
async fn drive_sign(env: &Env, d: &Deployment, holder: String, rounds: u64, budget: Budget) -> u64 {
    if rounds == 0 || !budget.allows(JOURNAL_CALL_MS) {
        return 0;
    }
    // The journal claims the signer only for a step whose worst case fits
    // what is left after this call (`budget_ms`); a step that does not fit
    // stays for a wakeup with more time (the cron's 15 minutes), so a short
    // delivery continuation never takes and abandons the claim.
    let request = serde_json::to_string(&StepRequest { holder, budget_ms: Some(budget.after_call_ms()) }).unwrap_or_default();
    let mut plan = journal_json::<Option<SignPlan>>(env, d, "/sign-step", request)
        .await
        .flatten();
    let mut done = 0;
    while let Some(p) = plan.take() {
        if done >= rounds || !budget.allows(sign_round_ms(&p)) {
            // Neither happens with a journal that honours `more` and
            // `budget_ms` (it plans nothing past them); the lease then
            // hands the claim back.
            break;
        }
        done += 1;
        let Some(outcome) = execute_sign(env, d, &p).await else {
            break;
        };
        let more = done < rounds && budget.allows(JOURNAL_CALL_MS);
        let body = ResultRequest {
            claim: p.claim().clone(),
            outcome,
            more,
            budget_ms: Some(budget.after_call_ms()),
        };
        let Ok(body) = serde_json::to_string(&body) else {
            break;
        };
        plan = journal_json::<SignStep>(env, d, "/sign-result", body)
            .await
            .and_then(|s| s.plan);
    }
    done
}

// --------------------------------------------------------------- routes

async fn hook(mut req: Request, env: Env, ctx: Context, scope: &ScopeRef) -> Result<Response> {
    let Ok(d) = deployment(&env) else {
        return error("not_configured", 503);
    };
    let Ok(receiver) = receiver_config(&env, &d.chain) else {
        return error("not_configured", 503);
    };
    // The signature header's fixed fields (key, subscription, attempt,
    // timestamp window) are checked before any body byte is admitted: a
    // request that can never authenticate is refused unread.
    let header = req.headers().get(webhook::SIGNATURE_HEADER)?;
    if let Err(e) = webhook::precheck(&receiver, header.as_deref(), now_ms() / 1000) {
        return error(e.code(), e.status());
    }
    // A declared length above the bound is refused unread; the body is
    // admitted as its bytes arrive (`cf_io`), and must arrive inside the
    // provider's acknowledgement window (`webhook::DELIVERY_ACK_MS`): the
    // provider sends it whole at once, so a body still incomplete then is
    // cancelled (408) instead of holding the invocation.
    let deadline = now_ms().saturating_add(webhook::DELIVERY_ACK_MS);
    let raw = match cf_io::request_bytes_until(&mut req, webhook::MAX_BODY_BYTES, Some(deadline)).await {
        Ok(raw) => raw,
        Err(e) => return error(e.code(), e.status()),
    };
    let signed = match webhook::authenticate(&receiver, header.as_deref(), &raw, now_ms() / 1000) {
        Ok(signed) => signed,
        Err(e) => return error(e.code(), e.status()),
    };
    // Contracts 1 and 2 share this wire format; `/2` is only sooner (it can
    // arrive before the RPC endpoint serves the block, which nothing on the
    // delivery path reads: `webhook` module docs).
    let contract = match webhook::contract(req.headers().get(webhook::CONTRACT_HEADER)?.as_deref()) {
        Ok(contract) => contract,
        Err(e) => return error(e.code(), e.status()),
    };
    match webhook::parse_authenticated(&receiver, &signed, &raw) {
        Err(e) => error(e.code(), e.status()),
        Ok(Delivery::Challenge { challenge }) => {
            let mut response = Response::empty()?.with_status(204);
            response
                .headers_mut()
                .set(webhook::CHALLENGE_RESPONSE_HEADER, &challenge)?;
            Ok(response)
        }
        Ok(Delivery::Finalized(mut notification)) => {
            notification.contract = contract;
            let holder = format!("delivery:{}", notification.delivery_id.to_hex());
            let body = serde_json::to_string(&notification)?;
            let Ok(mut response) =
                journal_call(&env, &d, Method::Post, "/ingest", Some(body)).await
            else {
                return error("store_unavailable", 503);
            };
            match response.status_code() {
                200 => {}
                409 => return error("conflict", 409),
                _ => return error("store_unavailable", 503),
            }
            // The delivery and its step are committed: acknowledge now, inside
            // the provider's 5 s window, and continue any RPC round, the
            // cohort authority's heartbeat and one signing round after the
            // response (waitUntil allows 30 s; the claim lease is 25 s).
            let Ok(value) = cf_io::response_json::<serde_json::Value>(&mut response, HOOK_REPLY_BYTES).await else {
                return Ok(Response::empty()?.with_status(204));
            };
            let drill_ack_failure =
                value.get("drill_ack_failure").and_then(|v| v.as_bool()) == Some(true);
            let report = serde_json::from_value::<DeliveryReport>(value).ok();
            let new_block = report
                .as_ref()
                .is_some_and(|r| r.outcome != crate::journal::DeliveryOutcome::DuplicateDelivery);
            let eval = eval_do::enabled(&env) && config::eval_cohort_name(&d.chain).is_some();
            let confirmed = report.as_ref().map(|r| r.progress.confirmed);
            let plan = report.and_then(|r| r.plan);
            let rounds = d.settings.delivery_rounds;
            // The continuation is admitted before it is spawned: its own glue
            // ticket and a child scope with the 30 s `waitUntil` deadline.
            // It outlives this invocation, so what it needs is copied into
            // its scope while it is built (values cross scopes only by copy).
            // Refused, nothing is spawned: the next delivery drives the same
            // work (the cron watchdog drives the signing and backfill work
            // too, but the commitment reader only in fallback).
            let continuation = slop_ninja_arena::wait_until(
                scope,
                crate::arena_routes::R_HOOK_CONTINUATION,
                crate::arena_routes::CONTINUATION_CONTROL_BYTES,
                now_ms(),
                |_child| {
                    let (env, d, plan, holder) = (env.clone(), d.clone(), plan.clone(), holder.clone());
                    async move {
                        // Every await below is a call the platform bounds
                        // (an `AbortSignal` timeout) and starts only when
                        // its allowance still fits the 30 s window, so the
                        // continuation ends inside it and its scope and
                        // ticket are never abandoned.
                        let budget = Budget::new(slop_ninja_arena::limits::WAIT_UNTIL_MS);
                        if eval && let Some(confirmed) = confirmed {
                            // Each delivery is a heartbeat for both cohort
                            // instances: the load instance's claims check its
                            // own heartbeat.
                            for instance in ["main", "load"] {
                                if !budget.allows(eval_do::KICK_TIMEOUT_MS) {
                                    break;
                                }
                                let kick = crate::eval::engine::Kick { confirmed, ..Default::default() };
                                let _ = eval_do::kick(&env, instance, kick).await;
                            }
                        }
                        if plan.is_some() {
                            drive(&env, &d, plan, rounds, holder.clone(), budget).await;
                        }
                        if new_block {
                            drive_sign(&env, &d, holder, DELIVERY_SIGN_ROUNDS, budget).await;
                            // The finalized commitment reader runs on the
                            // per-block path: each new block's delivery (the
                            // cron drives it only in fallback, below).
                            if eval {
                                commitments_drive(&env, &d, budget).await;
                            }
                        }
                    }
                },
            );
            if let Ok(continuation) = continuation {
                continuation.spawn(|c| {
                    ctx.wait_until(async move {
                        let _ = c.await;
                    })
                });
            }
            if drill_ack_failure {
                // Committed, but answered as a failure: the provider retries
                // the same delivery, which must be deduplicated.
                return error("drill_ack_failure", 503);
            }
            Ok(Response::empty()?.with_status(204))
        }
    }
}

/// One secret, when installed (the token checks drop short ones).
fn token_secret(env: &Env, name: &str) -> Option<Zeroizing<String>> {
    env.secret(name).ok().map(|s| Zeroizing::new(s.to_string()))
}

/// The installed status and operator tokens.
struct StatusSecrets {
    operator: Option<Zeroizing<String>>,
    monitor: Option<Zeroizing<String>>,
    publisher: Option<Zeroizing<String>>,
    alerts: Option<Zeroizing<String>>,
    shared: Option<Zeroizing<String>>,
}

impl StatusSecrets {
    fn read(env: &Env) -> Self {
        Self {
            operator: token_secret(env, access::OPERATOR_TOKEN),
            monitor: token_secret(env, access::MONITOR_TOKEN),
            publisher: token_secret(env, access::PUBLISHER_TOKEN),
            alerts: token_secret(env, access::ALERTS_TOKEN),
            shared: token_secret(env, access::SHARED_TOKEN),
        }
    }

    fn tokens(&self) -> access::Tokens<'_> {
        fn s(t: &Option<Zeroizing<String>>) -> Option<&str> {
            t.as_ref().map(|t| t.as_str())
        }
        access::Tokens::new(s(&self.operator), s(&self.monitor), s(&self.publisher), s(&self.alerts), s(&self.shared))
    }
}

/// The status gate (`access::decide`): the caller's report, or the refusal to send.
fn status_gate(req: &Request, env: &Env, route: access::StatusRoute) -> std::result::Result<access::Report, Result<Response>> {
    let secrets = StatusSecrets::read(env);
    let tokens = secrets.tokens();
    let internal = access::is_internal_host(req.url().ok().as_ref().and_then(|u| u.host_str()));
    let header = req.headers().get("authorization").ok().flatten();
    match access::decide(&tokens, internal, header.as_deref(), route) {
        access::Access::Granted(caller) => Ok(tokens.report(caller)),
        access::Access::Unauthorized => Err(unauthorized()),
        access::Access::NotConfigured => Err(error("status_token_not_configured", 503)),
    }
}

fn bearer_ok(req: &Request, env: &Env, secret: &str) -> std::result::Result<bool, ()> {
    let Ok(token) = env.secret(secret).map(|s| Zeroizing::new(s.to_string())) else {
        return Err(());
    };
    if config::token(&token).is_err() {
        return Err(());
    }
    let header = req.headers().get("authorization").ok().flatten();
    Ok(config::bearer_matches(header.as_deref(), &token))
}

fn unauthorized() -> Result<Response> {
    let mut response = error("unauthorized", 401)?;
    response
        .headers_mut()
        .set("WWW-Authenticate", "Bearer realm=\"coordinator\"")?;
    Ok(response)
}

#[derive(Serialize)]
struct ConfigStatus {
    network: Option<&'static str>,
    chain_genesis: Option<String>,
    netuid: Option<u16>,
    webhook_receiver_configured: bool,
    testnet_only_genesis: bool,
    rpc_host: Option<String>,
}

#[derive(Serialize)]
struct ServiceStatus {
    service: &'static str,
    version: &'static str,
    deployment: &'static str,
    config: ConfigStatus,
    chain_journal: Option<serde_json::Value>,
    /// The cron watchdog's last timed call to the ChainJournal, kept outside
    /// it, with a verdict (`ok`, `slow`, `unresponsive`, `stale`, `unknown`).
    chain_journal_liveness: serde_json::Value,
    eval_cohort: Option<serde_json::Value>,
    /// This Worker instance's arena counters (content-free).
    arena: serde_json::Value,
    /// Who read this status and which status tokens are installed.
    access: access::Report,
}

async fn do_json(env: &Env, d: &Deployment, base: &str, name: &str) -> Option<serde_json::Value> {
    let mut response = call_do(env, d, base, name, Method::Get, "/status", None, Some(liveness::TIMEOUT_MS))
        .await
        .ok()?;
    if response.status_code() != 200 {
        cf_io::discard(&mut response);
        return None;
    }
    cf_io::response_json(&mut response, ROUTE_REPLY_BYTES).await.ok()
}

async fn status(req: Request, env: Env) -> Result<Response> {
    let access_report = match status_gate(&req, &env, access::StatusRoute::Worker) {
        Ok(report) => report,
        Err(refusal) => return refusal,
    };
    // The alerts relay's `/status` is its liveness subset only: no cohort
    // status (not even read), liveness record or arena counters.
    let detail = access_report.caller.reads_detail();
    let d = deployment(&env).ok();
    let receiver_ok = d
        .as_ref()
        .is_some_and(|d| receiver_config(&env, &d.chain).is_ok());
    let (chain_journal, eval_cohort, live) = match &d {
        Some(d) => {
            let journal = do_json(
                &env,
                d,
                CHAIN_JOURNAL,
                &config::chain_journal_name(&d.chain),
            )
            .await;
            // The main authority the deployment names (`EVAL_MAIN_AUTHORITY`).
            let cohort = match config::eval_cohort_name(&d.chain) {
                Some(_) if detail => eval_do::main_status(&env).await,
                _ => None,
            };
            let journal = match journal {
                Some(j) if !detail => Some(access::alerts_journal(&j)),
                j => j,
            };
            (journal, cohort, read_liveness(&env, d).await)
        }
        None => (None, None, None),
    };
    let now = now_ms();
    let chain_journal_liveness = serde_json::json!({
        "verdict": liveness::verdict(live.as_ref(), now),
        "status_answered": chain_journal.is_some(),
        "age_ms": live.as_ref().map(|l| now.saturating_sub(l.checked_at_ms)),
        "record": live.as_ref().filter(|_| detail),
    });
    json(&ServiceStatus {
        service: "slop-ninja-coordinator",
        version: env!("CARGO_PKG_VERSION"),
        deployment: "testnet",
        config: ConfigStatus {
            network: d.as_ref().map(|d| d.network),
            chain_genesis: d.as_ref().map(|d| d.chain.genesis.to_hex()),
            netuid: d.as_ref().and_then(|d| d.chain.netuid),
            webhook_receiver_configured: receiver_ok,
            testnet_only_genesis: config::TESTNET_ONLY_GENESIS,
            rpc_host: d.as_ref().and_then(|d| {
                d.rpc_url
                    .strip_prefix("https://")
                    .and_then(|r| r.split('/').next())
                    .map(str::to_owned)
            }),
        },
        chain_journal,
        chain_journal_liveness,
        eval_cohort,
        arena: if detail { arena_exports::arena_status() } else { serde_json::Value::Null },
        access: access_report,
    })
}

async fn clear_fault(mut req: Request, env: Env) -> Result<Response> {
    match bearer_ok(&req, &env, access::OPERATOR_TOKEN) {
        Err(()) => return error("operator_token_not_configured", 503),
        Ok(false) => return unauthorized(),
        Ok(true) => {}
    }
    let Ok(d) = deployment(&env) else {
        return error("not_configured", 503);
    };
    let raw = match cf_io::request_bytes(&mut req, MAX_OPERATOR_BODY).await {
        Ok(raw) => raw,
        Err(e) => return error(e.code(), e.status()),
    };
    let Ok(clear) = serde_json::from_slice::<ClearRequest>(&raw) else {
        return error("bad_request", 400);
    };
    let body = serde_json::to_string(&clear)?;
    let Ok(mut response) = journal_call_within(&env, &d, Method::Post, "/clear-fault", Some(body), None).await
    else {
        return error("store_unavailable", 503);
    };
    if response.status_code() != 200 {
        cf_io::discard(&mut response);
        return error("store_unavailable", 503);
    }
    match cf_io::response_json::<serde_json::Value>(&mut response, ROUTE_REPLY_BYTES).await {
        Ok(outcome) => json(&outcome),
        Err(e) => error(e.code(), 503),
    }
}

/// Operator signing actions. The body is the T12 bundle for `target` and a
/// small JSON control object otherwise.
async fn signing_admin(mut req: Request, env: Env, which: &str) -> Result<Response> {
    match bearer_ok(&req, &env, access::OPERATOR_TOKEN) {
        Err(()) => return error("operator_token_not_configured", 503),
        Ok(false) => return unauthorized(),
        Ok(true) => {}
    }
    let Ok(d) = deployment(&env) else {
        return error("not_configured", 503);
    };
    // A target body is either bundle form; each parser applies its own bound
    // (a reward-v1 bundle: one stored Durable Object value).
    let limit = if which == "target" || which == "restart" || which == "decide" {
        crate::target::MAX_BUNDLE_BYTES.max(crate::target::MAX_BUNDLE_V2_BYTES)
    } else {
        MAX_OPERATOR_BODY
    };
    let raw = match cf_io::request_bytes(&mut req, limit).await {
        Ok(raw) => raw,
        Err(e) => return error(e.code(), e.status()),
    };
    let body = match which {
        "target" => {
            let Ok(bundle) = String::from_utf8(raw) else {
                return error("bad_request", 400);
            };
            serde_json::to_string(&AdminRequest {
                action: "install_target".into(),
                bundle: Some(bundle),
                drill: None,
                count: None,
                from_epoch: None,
                observed_epoch: None,
                label: None,
                cohort: None,
                operator: None,
            })?
        }
        "restart" => {
            // The body is the launch bundle; the restarted record's first
            // epoch is the `from_epoch` query parameter.
            let Ok(bundle) = String::from_utf8(raw) else {
                return error("bad_request", 400);
            };
            let from_epoch =
                req.url()?.query_pairs().find(|(k, _)| k == "from_epoch").and_then(|(_, v)| v.parse::<u64>().ok());
            let Some(from_epoch) = from_epoch else {
                return error("from_epoch", 400);
            };
            serde_json::to_string(&AdminRequest {
                action: "restart_target".into(),
                bundle: Some(bundle),
                drill: None,
                count: None,
                from_epoch: Some(from_epoch),
                observed_epoch: None,
                label: None,
                cohort: None,
                operator: None,
            })?
        }
        "revoke" => serde_json::to_string(&AdminRequest {
            action: "revoke".into(),
            bundle: Some(hex::encode(&raw)),
            drill: None,
            count: None,
            from_epoch: None,
            // The epoch bound's second source: a journal that never ran a
            // signing preflight has no schedule of its own.
            observed_epoch: epoch_read(&env, &d).await.map(|e| e.index),
            label: None,
            cohort: None,
            operator: None,
        })?,
        // An operator decision (`signing::decide`): the body is its request.
        "decide" => {
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else {
                return error("bad_request", 400);
            };
            serde_json::to_string(&AdminRequest {
                action: "decide".into(),
                bundle: None,
                drill: None,
                count: None,
                from_epoch: None,
                observed_epoch: epoch_read(&env, &d).await.map(|e| e.index),
                label: None,
                cohort: None,
                operator: Some(v),
            })?
        }
        "kick" => {
            let budget = Budget::new(crate::arena_routes::UNBOUNDED_FETCH_MS);
            let rounds = drive_sign(&env, &d, format!("kick:{}", now_ms()), TICK_SIGN_ROUNDS, budget).await;
            return json(&serde_json::json!({"rounds": rounds}));
        }
        _ => {
            let Ok(r) = serde_json::from_slice::<AdminRequest>(&raw) else {
                return error("bad_request", 400);
            };
            if r.action == "install_target" || r.action == "restart_target" || r.action == "revoke" {
                return error("bad_request", 400);
            }
            // An abandonment record is refused for a cohort whose closure is
            // in the receipts bucket: it closed, so it is handed in instead.
            if r.action == "auto_abandoned" {
                let prefix = format!("{}{}/", signing::auto::COHORT_PREFIX, r.label.as_deref().unwrap_or_default());
                let Some(label) = signing::auto::label_of(&prefix) else {
                    return json(&serde_json::json!({"refused": "label_malformed"}));
                };
                let Ok(bucket) = env.bucket(eval_do::RECEIPTS_BUCKET) else {
                    return error("bucket_binding", 503);
                };
                let key = signing::auto::key(label, signing::auto::object::CLOSURE);
                let deadline = now_ms().saturating_add(AUTO_PASS_MS);
                match cf_io::r2_size(&bucket, &key, Some(deadline)).await {
                    Ok(None) => {}
                    Ok(Some(_)) => return json(&serde_json::json!({"refused": "cohort_closed"})),
                    Err(_) => return error("object_unavailable", 503),
                }
            }
            serde_json::to_string(&r)?
        }
    };
    let Ok(mut response) = journal_call_within(&env, &d, Method::Post, "/sign-admin", Some(body), None).await
    else {
        return error("store_unavailable", 503);
    };
    if response.status_code() != 200 {
        cf_io::discard(&mut response);
        return error("signing_unavailable", 503);
    }
    match cf_io::response_json::<serde_json::Value>(&mut response, ROUTE_REPLY_BYTES).await {
        Ok(mut outcome) => {
            // A journaled revocation reaches validators only through the
            // pointer: write it now rather than at the next tick, and say
            // how that went (the tick retries a failure either way).
            if which == "revoke" && outcome.get("revoked").is_some() {
                let written = pointer_drive(&env, &d).await;
                if let Some(o) = outcome.as_object_mut() {
                    o.insert("pointer".into(), serde_json::json!(written.err().unwrap_or("confirmed")));
                }
            }
            json(&outcome)
        }
        Err(e) => error(e.code(), 503),
    }
}

/// Operator and status routes of the cohort authority (`EVAL_ROLE = "eval"` only).
/// `GET /eval/status` passes the status gate as the publisher's read; `GET
/// /eval/load/status`, `GET /eval/admission/link` and `GET
/// /eval/authority/<name>/status` as operator reads (`access`); everything
/// else needs the operator token. `/eval/load/...` reaches the separate load-rehearsal
/// instance. `/eval/authority/<name>/...` reaches main authority `<name>`
/// whatever the deployment names, only when the deployment knows it (the
/// original, the named one and each `EVAL_MAIN_PREVIOUS` entry,
/// `config::Authority::known`): `GET .../status` for audit (it never creates
/// an object), and `POST .../clear` and `.../abandon` for a fault in work an
/// earlier authority still holds.
#[cfg_attr(
    not(test),
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
async fn eval_route(mut req: Request, env: Env) -> Result<Response> {
    if !eval_do::enabled(&env) {
        return error("not_found", 404);
    }
    let path = req.path();
    let audit = config::authority_route(&path);
    let get = req.method() == Method::Get;
    match access::status_route(get, &path) {
        Some(route) => {
            if let Err(refusal) = status_gate(&req, &env, route) {
                return refusal;
            }
        }
        None => match bearer_ok(&req, &env, access::OPERATOR_TOKEN) {
            Err(()) => return error("token_not_configured", 503),
            Ok(false) => return unauthorized(),
            Ok(true) => {}
        },
    }
    // The chain, the netuid and the main authority vars must all name an object.
    if eval_do::cohort_name(&env, "main").is_none() {
        return error("not_configured", 503);
    }
    let raw = if req.method() == Method::Post {
        let raw = match cf_io::request_bytes(&mut req, crate::arena_routes::MAX_PLAN_BODY_BYTES).await {
            Ok(raw) => raw,
            Err(e) => return error(e.code(), e.status()),
        };
        Some(String::from_utf8(raw).map_err(|_| worker::Error::RustError("utf-8".into()))?)
    } else {
        None
    };
    let (instance, rest) = match path.strip_prefix("/eval/load/") {
        Some(rest) => ("load", format!("/{rest}")),
        None => ("main", path.trim_start_matches("/eval").to_owned()),
    };
    let body: serde_json::Value = serde_json::from_str(raw.as_deref().unwrap_or("{}")).unwrap_or_default();
    match (req.method(), rest.as_str()) {
        (Method::Get, "/status") => {
            let mut r = eval_do::forward(&env, instance, Method::Get, "/status", None).await?;
            if r.status_code() != 200 {
                return Ok(r);
            }
            let mut v: serde_json::Value = match cf_io::response_json(&mut r, ROUTE_REPLY_BYTES).await {
                Ok(v) => v,
                Err(e) => return error(e.code(), 503),
            };
            v["limiter"] = eval_do::limiter_status(&env).await.unwrap_or_default();
            v["ledger"] = eval_do::ledger_call(&env, "/status", &serde_json::json!({})).await.unwrap_or_default();
            json(&v)
        }
        // The eval-to-upload link probe: the framing service binding, the
        // internal host and the shared interface token (the deploy probe).
        (Method::Get, "/admission/link") if instance == "main" => json(&admission_do::link_probe(&env).await),
        // The publisher's report of a cohort's publication at scoring start
        // (`access::StatusRoute::Publication`): recorded by the object of
        // the known authority whose label the cohort carries
        // (`config::Authority::label_authority`), a predecessor too after a
        // succession, by `admission::publication_report`; 404 when that
        // authority holds no such cohort.
        (Method::Post, "/admission/published") if instance == "main" => {
            let Ok(known) = eval_do::main_authority(&env) else {
                return error("not_configured", 503);
            };
            match body["label"].as_str().and_then(|l| known.label_authority(l)) {
                Some(authority) => {
                    eval_do::authority_fetch(&env, "main", authority, Method::Post, "/admission/published", raw, None)
                        .await
                }
                None => error("cohort_unknown", 404),
            }
        }
        // Main authority `<name>`, only when the deployment knows it, so no
        // request creates an object for any other name. A named authority's
        // own succession check runs in its object (`EvalCohort::succeed`), so
        // the operator's plans and the calendar's are covered alike.
        (_, r) if r.starts_with("/authority/") => {
            let known = eval_do::main_authority(&env).ok();
            match audit {
                Some((name, action)) if known.as_ref().is_some_and(|a| a.known(name)) => match (req.method(), action) {
                    (Method::Get, "status") => eval_do::audit_status(&env, name).await,
                    (Method::Post, "clear" | "abandon") => {
                        eval_do::authority_fetch(&env, "main", name, Method::Post, &format!("/{action}"), raw, None)
                            .await
                    }
                    _ => error("not_found", 404),
                },
                _ => error("not_found", 404),
            }
        }
        (Method::Post, "/plan" | "/clear" | "/abandon" | "/hold") => eval_do::forward(&env, instance, Method::Post, &rest, raw).await,
        // Live values (`eval::live`): the report on the status route
        // `/eval/live` (status token, `access::status_route`), and the
        // operator's stop (operator token alone), on the main instance.
        (Method::Get, "/live") if instance == "main" => eval_do::forward(&env, instance, Method::Get, &rest, None).await,
        (Method::Post, "/live/stop") if instance == "main" => eval_do::forward(&env, instance, Method::Post, &rest, raw).await,
        // The span gate: the operator's void of one cohort's measured
        // conditions (`span_gate::void`), and the span-equivalent reference
        // decision, which refuses every request until the offline
        // operator-decision key exists.
        (Method::Post, "/span-gate/void" | "/references/decision") if instance == "main" => {
            eval_do::forward(&env, instance, Method::Post, &rest, raw).await
        }
        // Operator decisions (`eval::decide`): a declined faulted closure and
        // the operator's withheld release of a cohort's objects.
        (Method::Post, "/closure/decline" | "/release-objects" | "/cohort/start") if instance == "main" => {
            eval_do::forward(&env, instance, Method::Post, &rest, raw).await
        }
        // The operator action log (`crate::operator`), paged by sequence.
        (Method::Get, "/operator/actions") => {
            let from = req.url().ok().and_then(|u| u.query_pairs().find(|(k, _)| k == "from").and_then(|(_, v)| v.parse::<u64>().ok()));
            let p = format!("/operator/actions?from={}", from.unwrap_or(0));
            eval_do::forward(&env, instance, Method::Get, &p, None).await
        }
        // Generation v2: the operator's clear of a vendor pause (a spend cap
        // or a key failure, `gen_v2::resume_vendor`).
        (Method::Post, crate::eval::lanes::GEN_VENDOR_CLEAR_PATH) if instance == "main" => eval_do::forward(&env, instance, Method::Post, &rest, raw).await,
        // The standing template and the calendar (main instance).
        (Method::Post, "/template" | "/template/pause" | "/session/refuse") if instance == "main" => eval_do::forward(&env, instance, Method::Post, &rest, raw).await,
        (Method::Get, "/calendar") if instance == "main" => eval_do::forward(&env, instance, Method::Get, &rest, None).await,
        // Generation v2: the generation Worker's key health, and one call of
        // the compiled synthetic measurement set (a cell, a call index and an
        // attempt index, never text), charged to the ledger per call.
        (Method::Get, "/gen/health") if instance == "main" => json(&match gen_do::health(&env).await {
            Some(h) => serde_json::to_value(h).unwrap_or_default(),
            None => serde_json::json!({"error": "gen_unavailable"}),
        }),
        (Method::Post, "/gen/measure") if instance == "main" => {
            let (mut v, micro, key) = gen_do::measure(&env, &body).await;
            // One ledger key per vendor call (vendor, cell, call, attempt):
            // a repeated request settles once, two calls never share a key,
            // and a ledger failure is reported to the operator, not dropped.
            if let Some(key) = key.filter(|_| micro > 0) {
                let settled = eval_do::ledger_call(&env, "/settle", &serde_json::json!({"key": key, "microusd": micro, "note": "operator gen-measure call"})).await;
                v["ledger"] = serde_json::json!(if settled.is_ok() { "settled" } else { "ledger_error" });
            }
            json(&v)
        }
        (Method::Post, "/probe") => {
            let Some(model) = body["model"].as_str().filter(|m| crate::eval::textgen::price(m).is_some()) else {
                return error("unpriced_model", 400);
            };
            let v = eval_do::probe(&env, model).await;
            // Operator probes are charged to the tracking ledger like cohorts.
            let micro: u64 = v["probes"].as_array().map_or(0, |a| a.iter().filter_map(|p| p["cost_microusd"].as_u64()).sum());
            charge_probe(&env, "ai", micro).await;
            json(&v)
        }
        (Method::Post, "/probe/connections") => {
            let kind = body["kind"].as_str().unwrap_or("ai");
            let model = body["model"].as_str().unwrap_or("@cf/meta/llama-3.2-1b-instruct");
            if crate::eval::textgen::price(model).is_none() {
                return error("unpriced_model", 400);
            }
            let n = body["n"].as_u64().unwrap_or(12) as usize;
            let max_tokens = body["max_tokens"].as_u64().unwrap_or(8).clamp(1, 512) as u32;
            let v = eval_do::probe_connections(&env, kind, n, model, body["ms"].as_u64().unwrap_or(2_000), max_tokens).await;
            if kind == "ai" {
                // Each call's tokens at the model's price (prompt tokens bounded
                // by the synthetic prompt; completions as reported).
                let completion: u64 = v["ops"].as_array().map_or(0, |a| a.iter().filter_map(|o| o["result"]["completion_tokens"].as_u64()).sum());
                let calls = v["ops"].as_array().map_or(0, Vec::len) as u64;
                charge_probe(&env, "connections", crate::eval::textgen::cost_microusd(model, calls * 64, completion)).await;
            }
            json(&v)
        }
        (Method::Post, "/probe/rate" | "/probe/rate-state") => {
            let models: Vec<String> = serde_json::from_value(body["models"].clone()).unwrap_or_default();
            if models.is_empty() || models.len() > 4 || models.iter().any(|m| crate::eval::textgen::price(m).is_none()) {
                return error("models", 400);
            }
            let lanes = body["lanes_per_model"].as_u64().unwrap_or(4).clamp(1, 16) as usize;
            if rest == "/probe/rate" {
                let rpm = body["rpm_per_model"].as_f64().unwrap_or(250.0).clamp(1.0, 2_000.0);
                let seconds = body["seconds"].as_u64().unwrap_or(180).min(900);
                let max_tokens = body["max_tokens"].as_u64().unwrap_or(8).clamp(1, 64) as u32;
                json(&lanes_do::rate_probe_start(&env, &models, rpm, seconds, lanes, max_tokens).await)
            } else {
                json(&lanes_do::rate_probe_state(&env, &models, lanes).await)
            }
        }
        (Method::Post, "/lanes/state") => {
            let names: Vec<String> = serde_json::from_value(body["names"].clone()).unwrap_or_default();
            json(&lanes_do::lanes_state(&env, &names[..names.len().min(64)]).await)
        }
        (Method::Post, "/scorer-probe") => {
            let (Some(p), Some(k)) = (body["package_sha256"].as_str(), body["manifest_key"].as_str()) else {
                return error("bad_request", 400);
            };
            // An optional artifact spec (id) the probe's load names by digest,
            // as a cohort naming it loads.
            let spec = match body.get("artifact_spec") {
                None | Some(serde_json::Value::Null) => None,
                Some(id) => match id.as_str().and_then(crate::eval::plan::load_spec_sha256_of) {
                    Some(sha) => Some(sha),
                    None => return error("artifact_spec", 400),
                },
            };
            // Optional `"spans": true`: each prediction carries its sentences
            // and spans (a package with a token head, such as S2c's).
            let spans = match body.get("spans") {
                None | Some(serde_json::Value::Null) => false,
                Some(serde_json::Value::Bool(b)) => *b,
                Some(_) => return error("spans", 400),
            };
            let v = eval_do::scorer_probe(&env, p, k, spec.as_deref(), spans).await;
            // The probe instance's wall time at the latest plan's Container
            // rates (the cost model plans are checked with).
            let wall_ms = v["total_ms"].as_u64().unwrap_or(0) + v["cold"]["start_ms"].as_u64().unwrap_or(0);
            if let Some(model) = probe_cost_model(&env).await {
                charge_probe(&env, "scorer", crate::eval::cost::probe_charge_microusd(&model, wall_ms)).await;
            }
            json(&v)
        }
        _ => error("not_found", 404),
    }
}

/// Settles an operator probe's measured cost in the tracking ledger under
/// its own key (never refused; a ledger outage leaves it unrecorded).
async fn charge_probe(env: &Env, kind: &str, microusd: u64) {
    if microusd == 0 {
        return;
    }
    let key = format!("probe:{kind}:{}", now_ms());
    let _ = eval_do::ledger_call(env, "/settle", &serde_json::json!({"key": key, "microusd": microusd, "note": format!("operator {kind} probe")})).await;
}

/// The latest plan's (or the template's) Container cost model.
async fn probe_cost_model(env: &Env) -> Option<crate::eval::plan::CostModel> {
    let mut r = eval_do::forward(env, "main", Method::Get, "/cost-model", None).await.ok()?;
    if r.status_code() != 200 {
        cf_io::discard(&mut r);
        return None;
    }
    cf_io::response_json(&mut r, ROUTE_REPLY_BYTES).await.ok()
}

/// The Worker's routes, run inside the invocation's scope by the
/// hand-written `fetch` export (`cf_arena`).
async fn fetch(req: Request, env: Env, ctx: Context, scope: &ScopeRef) -> Result<Response> {
    let path = req.path();
    match (req.method(), path.as_str()) {
        (Method::Post, "/hook") => hook(req, env, ctx, scope).await,
        (Method::Get, "/status") => status(req, env).await,
        (Method::Post, "/fault/clear") => clear_fault(req, env).await,
        (Method::Post, "/signing/target") => signing_admin(req, env, "target").await,
        (Method::Post, "/signing/control") => signing_admin(req, env, "control").await,
        (Method::Post, "/signing/kick") => signing_admin(req, env, "kick").await,
        (Method::Post, "/signing/restart") => signing_admin(req, env, "restart").await,
        (Method::Post, "/signing/revoke") => signing_admin(req, env, "revoke").await,
        (Method::Post, "/signing/decide") => signing_admin(req, env, "decide").await,
        // Reference staging (operator token; its own arena route).
        (_, p) if p.starts_with(crate::eval::reference::PREFIX) => reference_do::route(req, env).await,
        (_, p) if p.starts_with("/eval/") => eval_route(req, env).await,
        // The public admission routes are the upload Worker's; this Worker
        // serves only the internal interface its service binding reaches.
        (_, p) if p.starts_with("/internal/admission/") => admission_do::interface_route(req, env).await,
        (
            _,
            "/hook" | "/status" | "/fault/clear" | "/signing/target" | "/signing/control"
            | "/signing/kick" | "/signing/restart" | "/signing/revoke" | "/signing/decide",
        ) => error("method_not_allowed", 405),
        _ => error("not_found", 404),
    }
}

/// The one-minute watchdog: a cheap read of the last delivery time and cursor,
/// at most one RPC head read, and in fallback the bounded per-tick RPC rounds.
/// Duplicate or missed runs change only when work happens, never which work.
/// Run inside its own scope by the hand-written `scheduled` export.
async fn scheduled(env: Env) {
    let Ok(d) = deployment(&env) else {
        return;
    };
    // The tick is also the external liveness probe of the ChainJournal: it
    // is bounded in time, and its outcome is recorded outside the journal.
    // The call carries the platform's own timeout (`liveness::TIMEOUT_MS`,
    // the journal-call allowance), so a wedged journal fails the call at the
    // deadline and nothing is left pending in the tick's scope.
    let started = now_ms();
    let answer = match journal_call(&env, &d, Method::Post, "/tick", None).await {
        Ok(mut response) => {
            let status = response.status_code();
            let report = if status == 200 {
                cf_io::response_json::<heartbeat::TickReport>(&mut response, ROUND_REPLY_BYTES).await.ok()
            } else {
                cf_io::discard(&mut response);
                None
            };
            Some((status, report))
        }
        Err(_) => None,
    };
    let latency = now_ms().saturating_sub(started);
    let status = answer.as_ref().map(|(s, _)| *s);
    let report = answer.and_then(|(_, r)| r);
    write_liveness(&env, &d, report.is_some(), latency, status).await;
    let Some(report) = report else {
        return;
    };
    let holder = format!("tick:{}", now_ms());
    // The cron handler's own wall limit, from the tick's start.
    let budget = Budget { until_ms: started.saturating_add(slop_ninja_arena::limits::CRON_MS) };
    // The automatic-handoff and pointer passes' worst cases are reserved
    // first, so catch-up or fallback rounds that use the rest of the tick
    // never starve them. The handoff runs before the pointer pass, so a
    // target installed by it reaches the pointer in the same tick.
    let handoff = signs_here(&env);
    let reserved = POINTER_PASS_MS + if handoff { AUTO_CALL_MS } else { 0 };
    let rounds = Budget { until_ms: budget.until_ms.saturating_sub(reserved) };
    drive(&env, &d, report.plan, d.settings.tick_rounds, holder.clone(), rounds).await;
    drive_sign(&env, &d, holder, TICK_SIGN_ROUNDS, rounds).await;
    if handoff && budget.allows(AUTO_CALL_MS + POINTER_PASS_MS) {
        auto_drive(&env, &d).await;
    }
    if budget.allows(POINTER_PASS_MS) {
        let _ = pointer_drive(&env, &d).await;
    }
    if eval_do::enabled(&env) && config::eval_cohort_name(&d.chain).is_some() {
        let kick = tick_kick(&env, &d).await;
        let mut main_reply = None;
        for instance in ["main", "load"] {
            if budget.allows(eval_do::KICK_TIMEOUT_MS) {
                let reply = eval_do::kick(&env, instance, kick).await;
                if instance == "main" {
                    main_reply = reply;
                }
            }
        }
        // Earlier main authorities on ticks only, until the named authority
        // reports its succession complete (a reply without the field, or no
        // reply, kicks them): before it one may still hold work (a switch
        // while it ran) and advances its boundary phases until its cohorts
        // close; after it every one is superseded and idle for good
        // (`config::kicked_authorities`).
        if let Ok(a) = eval_do::main_authority(&env) {
            let complete = main_reply.as_ref().is_some_and(|v| v["succession_complete"] == true);
            for name in config::kicked_authorities(&a, true, complete).iter().skip(1) {
                if budget.allows(eval_do::KICK_TIMEOUT_MS) {
                    let _ = eval_do::kick_authority(&env, name, kick).await;
                }
            }
        }
        // The commitment reader is per-block work: deliveries drive it, and
        // the cron only while the watchdog's fallback carries the per-block
        // path through RPC (no delivery then), never as its own schedule.
        if report.fallback {
            commitments_drive(&env, &d, budget).await;
        }
    }
}

/// Whether this deployment signs (the coordinator, not the eval Worker): only
/// its ChainJournal holds a hotkey and the authorization key.
fn signs_here(env: &Env) -> bool {
    !eval_do::enabled(env)
}

/// One automatic-handoff pass (`/sign-auto`): its bucket operations share one
/// deadline of the platform allowance a journal call has, as the pointer
/// pass's do.
const AUTO_PASS_MS: u64 = liveness::TIMEOUT_MS;
/// The pass's worst case seen from the tick: the pass and the journal call
/// that carries it.
const AUTO_CALL_MS: u64 = AUTO_PASS_MS + JOURNAL_CALL_MS;

/// Runs one automatic target handoff pass in the ChainJournal
/// (`signing::auto`): it follows the eval Worker's cohort records in the
/// receipts bucket, and installs and authorizes each closed cohort's target
/// once, in cohort order. Its outcome is on `/status`
/// (`signing.auto_target`); every failure is retried at the next tick, and
/// nothing here expires or reads another validator.
async fn auto_drive(env: &Env, d: &Deployment) {
    let Ok(body) = serde_json::to_string(&AutoRequest { budget_ms: AUTO_PASS_MS }) else {
        return;
    };
    if let Ok(mut r) = journal_call_within(env, d, Method::Post, "/sign-auto", Some(body), Some(AUTO_CALL_MS)).await {
        cf_io::discard(&mut r);
    }
}

/// The bucket operations of one pointer pass share one deadline of the
/// platform allowance a journal call has (`liveness::TIMEOUT_MS`): a stalled
/// bucket fails the pass at that deadline instead of holding the tick.
const POINTER_BUCKET_MS: u64 = liveness::TIMEOUT_MS;
/// A pointer pass's worst case: the plan call, at most one object call, the
/// bucket operations and the outcome call.
const POINTER_PASS_MS: u64 = 3 * JOURNAL_CALL_MS + POINTER_BUCKET_MS;

/// Keeps `authorizations/current.json` in the receipts bucket equal to the
/// signing journal's pointer ([`signing::pointer_plan`]): checks every signed
/// object the pointer names (content-addressed) and stores one that is
/// missing or of another length, fetching its bytes from the journal
/// ([`signing::pointer_object`]); reads the stored pointer back and, when its
/// bytes differ or it is missing, writes it after the objects and reads it
/// back. Every pass reports its outcome to the journal
/// ([`signing::pointer_outcome`]): the pointer confirmed stored, or the
/// failure code returned here, so `/status` shows a writer that stopped
/// (the journal cannot be told when the journal call itself fails; the
/// confirmation's age shows that too). A deployment without signing, or
/// with no reward-v1 bundle installed, writes nothing. Every failure is
/// retried at the next tick; nothing here expires.
async fn pointer_drive(env: &Env, d: &Deployment) -> std::result::Result<(), &'static str> {
    let outcome = pointer_pass(env, d).await;
    let report = match &outcome {
        Ok(None) => return Ok(()),
        Ok(Some(sha)) => signing::PointerOutcome { sha256: Some(sha.clone()), error: None },
        Err(code) => signing::PointerOutcome { sha256: None, error: Some((*code).into()) },
    };
    if let Ok(body) = serde_json::to_string(&report)
        && let Ok(mut r) = journal_call(env, d, Method::Post, "/sign-pointer-outcome", Some(body)).await
    {
        cf_io::discard(&mut r);
    }
    outcome.map(|_| ())
}

/// One pass of [`pointer_drive`]: `Ok(Some(sha))` when the stored pointer
/// with body SHA-256 `sha` was confirmed, `Ok(None)` when there is nothing
/// to write, or the failure's code.
async fn pointer_pass(env: &Env, d: &Deployment) -> std::result::Result<Option<String>, &'static str> {
    let mut r = journal_call(env, d, Method::Post, "/sign-pointer", None).await.map_err(|_| "plan_call")?;
    if r.status_code() != 200 {
        cf_io::discard(&mut r);
        return Err("plan_status");
    }
    let plan = cf_io::response_json::<Option<signing::PointerPlan>>(&mut r, ROUTE_REPLY_BYTES)
        .await
        .map_err(|_| "plan_unreadable")?;
    let Some(plan) = plan else {
        return Ok(None);
    };
    let bucket = env.bucket(eval_do::RECEIPTS_BUCKET).map_err(|_| "bucket_binding")?;
    let deadline = Some(now_ms().saturating_add(POINTER_BUCKET_MS));
    // The stored pointer, bounded by the exact length it should have: a
    // longer object differs.
    let stored = cf_io::r2_bytes(&bucket, signing::authority::POINTER_KEY, plan.body.len(), deadline).await;
    let current = matches!(&stored, Ok(Some(bytes)) if bytes.as_slice() == plan.body.as_bytes());
    drop(stored);
    // Every object the pointer names is checked on every pass, whether the
    // pointer changed or not: a content-addressed object of the right length
    // is the one named, and a missing or different one is stored again, so
    // a deleted object is restored without waiting for the pointer to change.
    for o in &plan.objects {
        let size = cf_io::r2_size(&bucket, &o.path, deadline).await.map_err(|_| "object_head")?;
        if size == Some(o.bytes) {
            continue;
        }
        let bytes = pointer_object_bytes(env, d, o).await?;
        if !cf_io::r2_put(&bucket, &o.path, &bytes, deadline).await {
            return Err("object_put");
        }
    }
    if !current {
        if !cf_io::r2_put(&bucket, signing::authority::POINTER_KEY, plan.body.as_bytes(), deadline).await {
            return Err("pointer_put");
        }
        let back = cf_io::r2_bytes(&bucket, signing::authority::POINTER_KEY, plan.body.len(), deadline).await;
        if !matches!(&back, Ok(Some(bytes)) if bytes.as_slice() == plan.body.as_bytes()) {
            return Err("pointer_read_back");
        }
    }
    Ok(Some(plan.sha256))
}

/// The bytes of one object the plan names, from the journal, checked
/// against the plan's length and digest. Read as one admitted block of its
/// declared length, within the revocation set's bound
/// ([`signing::authority::REVOCATION_SET_BYTES`], which also bounds the
/// smaller authorization and renewal objects).
async fn pointer_object_bytes(
    env: &Env,
    d: &Deployment,
    o: &slop_ninja_admission_wire::json::ObjectRef,
) -> std::result::Result<Vec<u8>, &'static str> {
    let bound = signing::authority::REVOCATION_SET_BYTES.max(signing::rows::record_bound());
    if o.bytes > bound as u64 {
        return Err("object_too_large");
    }
    let body = serde_json::json!({"sha256": o.sha256}).to_string();
    let mut r = journal_call(env, d, Method::Post, "/sign-pointer-object", Some(body)).await.map_err(|_| "object_call")?;
    if r.status_code() != 200 {
        cf_io::discard(&mut r);
        return Err("object_status");
    }
    let bytes = cf_io::response_bytes(&mut r, o.bytes as usize).await.map_err(|_| "object_unreadable")?;
    if bytes.len() as u64 != o.bytes || crate::chain::sha256_hex(&bytes) != o.sha256 {
        return Err("object_mismatch");
    }
    Ok(bytes)
}

/// The commitment reader's rounds for this block (a new block's delivery, or
/// a fallback tick): the cohort object plans
/// each round's reads within the reader's own share of the RPC budget
/// ([`crate::eval::commitments::reader_share`]), every read is also reserved
/// in the journal's shared budget, and the replies go back to the object.
/// Stops when a round needs nothing, a reservation is refused or the
/// invocation's budget ends.
async fn commitments_drive(env: &Env, d: &Deployment, budget: Budget) {
    use crate::eval::commitments::{READER_CALL_MS, READER_OBJECT_CALL_MS, ROUND_START_MS, fitting, read_ms, read_step_ms};
    let configured = env.var("READER_RPC_CALLS_PER_MINUTE").ok().and_then(|v| v.to_string().trim().parse::<u64>().ok());
    let share = crate::eval::commitments::reader_share(d.settings.rpc_calls_per_minute, configured);
    // Past-block state a pruning endpoint no longer keeps, or a block its
    // answering node has not imported yet, is read again from the archive
    // endpoint, when one is configured, before the reader acts on the
    // refusal (a coordinator outage longer than the node's state window must
    // not cost the cohort).
    let archive = var(env, "ARCHIVE_RPC_URL").and_then(|v| config::rpc_url(Some(&v)).ok()).filter(|u| *u != d.rpc_url);
    // Every call below carries the reader's own allowance
    // (`commitments::READER_OBJECT_CALL_MS`, `READER_CALL_MS`, `read_ms`),
    // sized so a round fits the delivery continuation's 30 s window after
    // the heartbeat and signing. The plan and ingest round trips, and a
    // failure of either, go to the object with the next plan (`Prior`).
    for n in 0..d.settings.tick_rounds {
        if !budget.allows(ROUND_START_MS) {
            // No room for even the first round (signing ran first in the
            // same budget): counted, so per-block starvation shows in the
            // reader's status (`rounds_starved`).
            if n == 0
                && budget.allows(READER_CALL_MS)
                && let Ok(mut r) =
                    eval_do::cohort_fetch_within(env, "main", Method::Post, "/commitments/starved", None, Some(READER_CALL_MS)).await
            {
                cf_io::discard(&mut r);
            }
            return;
        }
        // The round's claim lasts the rest of this invocation unless the
        // ingest below ends it, so an overlapping invocation (the next
        // block's delivery) plans nothing while these reads are in flight.
        // `fit_ms`: what is left once the plan returns within its
        // allowance, so the object plans and charges only reads that fit.
        let left = budget.left_ms();
        let plan_body = serde_json::json!({
            "per_minute": share, "lease_ms": left, "fit_ms": left.saturating_sub(READER_OBJECT_CALL_MS),
            "prior": reader_calls::take(),
        })
        .to_string();
        let sent = now_ms();
        let planned =
            eval_do::cohort_fetch_within(env, "main", Method::Post, "/commitments/plan", Some(plan_body), Some(READER_OBJECT_CALL_MS)).await;
        let Ok(mut r) = planned else {
            reader_calls::plan(now_ms().saturating_sub(sent), true);
            return;
        };
        if r.status_code() != 200 {
            cf_io::discard(&mut r);
            reader_calls::plan(now_ms().saturating_sub(sent), true);
            return;
        }
        let round = cf_io::response_json::<crate::eval::commitments::Round>(&mut r, ROUTE_REPLY_BYTES).await;
        reader_calls::plan(now_ms().saturating_sub(sent), round.is_err());
        let Ok(mut round) = round else { return };
        if round.reads.is_empty() {
            return;
        }
        // Only the leading reads whose steps and the ingest still fit go
        // out (the runtime round whole or not at all); the rest stay due.
        let fit = fitting(&round, budget.left_ms());
        let runtime = round.reads.iter().any(|r| r.tag.starts_with("version@"));
        let mut refused = fit < round.reads.len();
        round.reads.truncate(fit);
        let mut results = Vec::new();
        for read in round.reads {
            let call_ms = read_ms(read.large);
            if !budget.allows(read_step_ms(read.large) + READER_OBJECT_CALL_MS) {
                refused = true;
                break;
            }
            let reserved = match journal_call_within(env, d, Method::Post, "/rpc-reserve", None, Some(READER_CALL_MS)).await {
                Ok(mut r) => cf_io::response_json::<bool>(&mut r, ROUND_REPLY_BYTES).await.unwrap_or(false),
                Err(_) => false,
            };
            if !reserved {
                refused = true;
                break;
            }
            let post = |url: &str| {
                let (url, body) = (url.to_owned(), read.body.clone());
                async move {
                    let cap = if read.large { rpc::MAX_LARGE_RESPONSE_BYTES } else { rpc::MAX_RESPONSE_BYTES };
                    let reply = rpc_post_capped(&url, body, cap, call_ms).await;
                    reply.ok().and_then(|b| String::from_utf8(b).ok())
                }
            };
            // A resolution or snapshot read whose earlier answer served no
            // state goes to the archive endpoint first when one is configured.
            let first = match &archive {
                Some(url) if read.archive => url.as_str(),
                _ => d.rpc_url.as_str(),
            };
            let mut result = crate::eval::commitments::ReadResult { tag: read.tag.clone(), id: read.id, reply: post(first).await };
            if let Some(url) = &archive
                && first != url.as_str()
                && crate::eval::commitments::refusal(&result).is_some()
                && budget.allows(read_step_ms(read.large) + READER_OBJECT_CALL_MS)
                && match journal_call_within(env, d, Method::Post, "/rpc-reserve", None, Some(READER_CALL_MS)).await {
                    Ok(mut r) => cf_io::response_json::<bool>(&mut r, ROUND_REPLY_BYTES).await.unwrap_or(false),
                    Err(_) => false,
                }
            {
                // Only the archive's answer is ingested; its own refusal
                // then decides (discarded: gone; not served: read again).
                result.reply = post(url).await;
            }
            results.push(result);
        }
        if refused && runtime {
            // The runtime round is ingested only whole: a part is no answer.
            results.clear();
        }
        {
            // Sent even when no read went out (a refused reservation), so the
            // round's claim ends now. Serialized straight from the replies (no
            // intermediate JSON value), which are dropped before the call: one
            // copy in flight.
            #[derive(serde::Serialize)]
            struct Body<'a> {
                results: &'a [crate::eval::commitments::ReadResult],
            }
            let Ok(body) = serde_json::to_string(&Body { results: &results }) else { return };
            drop(results);
            let sent = now_ms();
            let ingested =
                eval_do::cohort_fetch_within(env, "main", Method::Post, "/commitments/ingest", Some(body), Some(READER_OBJECT_CALL_MS))
                    .await;
            let failed = match ingested {
                Ok(mut r) => {
                    let failed = r.status_code() != 200;
                    cf_io::discard(&mut r);
                    failed
                }
                Err(_) => true,
            };
            reader_calls::ingest(now_ms().saturating_sub(sent), failed);
        }
        if refused {
            return;
        }
    }
}

/// The commitment reader's last plan and ingest observations in this
/// isolate (round trip in ms, and the call that failed), sent with the next
/// plan ([`crate::eval::commitments::Prior`]): a plan that times out still
/// reaches the cohort object, so the plans after it count it. Fixed-size
/// atomics, no allocation between requests.
mod reader_calls {
    use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

    const NONE: u64 = u64::MAX;
    static PLAN_MS: AtomicU64 = AtomicU64::new(NONE);
    static INGEST_MS: AtomicU64 = AtomicU64::new(NONE);
    /// 0 none, 1 plan, 2 ingest.
    static FAILED: AtomicU8 = AtomicU8::new(0);

    pub fn plan(ms: u64, failed: bool) {
        PLAN_MS.store(ms.min(NONE - 1), Ordering::Relaxed);
        if failed {
            FAILED.store(1, Ordering::Relaxed);
        }
    }

    pub fn ingest(ms: u64, failed: bool) {
        INGEST_MS.store(ms.min(NONE - 1), Ordering::Relaxed);
        if failed {
            FAILED.store(2, Ordering::Relaxed);
        }
    }

    pub fn take() -> crate::eval::commitments::Prior {
        let ms = |a: &AtomicU64| Some(a.swap(NONE, Ordering::Relaxed)).filter(|v| *v != NONE);
        crate::eval::commitments::Prior {
            plan_ms: ms(&PLAN_MS),
            ingest_ms: ms(&INGEST_MS),
            failed: match FAILED.swap(0, Ordering::Relaxed) {
                1 => Some("plan".into()),
                2 => Some("ingest".into()),
                _ => None,
            },
        }
    }
}

/// The eval deployment's Queue consumers, run inside their own scope by the
/// hand-written `queue` export: the admission events Queue (R2
/// object-create notifications, `admission_do::events`), named by the
/// `ADMISSION_EVENTS_QUEUE` var, and the lane-wakeup Queue (lane kicks).
/// Without that var no batch is routed: the handler fails, the export
/// retries the whole batch (the consumer's `max_retries` applies), and an
/// event never reaches the lane consumer by mistake.
async fn queue(batch: worker::worker_sys::MessageBatch, env: Env, scope: slop_ninja_arena::ScopeRef) -> Result<()> {
    let Some(events) = var(&env, crate::eval::admission::EVENTS_QUEUE_VAR).filter(|v| !v.is_empty()) else {
        return Err(worker::Error::RustError(format!("{} unset: queue batch not routed", crate::eval::admission::EVENTS_QUEUE_VAR)));
    };
    let name = batch.queue().ok().and_then(|q| q.as_string());
    if name.as_deref() == Some(events.as_str()) {
        return admission_do::events(batch, env, scope).await;
    }
    lanes_do::consume(batch, env, scope).await
}

/// Reads the subnet epoch index at the RPC-confirmed watermark: one
/// `state_queryStorageAt` call, charged to the journal's shared RPC budget.
async fn epoch_read(env: &Env, d: &Deployment) -> Option<crate::eval::chain::EpochObservation> {
    let mut r = journal_call(env, d, Method::Get, "/confirmed", None).await.ok()?;
    let confirmed: Option<crate::wire::BlockRef> = cf_io::response_json(&mut r, ROUND_REPLY_BYTES).await.ok()?;
    epoch_at(env, d, confirmed?).await
}

/// The epoch facts at `at`: one `state_queryStorageAt` call, charged to the
/// journal's shared RPC budget.
async fn epoch_at(env: &Env, d: &Deployment, at: crate::wire::BlockRef) -> Option<crate::eval::chain::EpochObservation> {
    let netuid = d.chain.netuid?;
    let mut r = journal_call(env, d, Method::Post, "/rpc-reserve", None).await.ok()?;
    if !cf_io::response_json::<bool>(&mut r, ROUND_REPLY_BYTES).await.ok()? {
        return None;
    }
    let body = rpc_post(&d.rpc_url, crate::eval::chain::epoch_request(4, netuid, &at.hash))
        .await
        .ok()?;
    crate::eval::chain::parse_epoch(4, &body, netuid, at)
}

/// The confirmed watermark and the RPC finalized head (`/confirmed-head`).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
struct ConfirmedHead {
    confirmed: Option<crate::wire::BlockRef>,
    head: Option<crate::wire::BlockRef>,
}

/// A tick's kick for the cohort authorities: the epoch at the RPC-confirmed
/// watermark, and the epoch index at the RPC finalized head, which decides
/// whether that observation is live (`eval::engine::ACTIVE_TEMPO_RULE`).
/// While the watermark is the head this is the same reading; while it lags
/// (a catch-up, and the usual tick right after a boundary) one more call,
/// charged to the same budget. A head read that fails or is refused leaves
/// the head epoch unknown: a new epoch is then decided by the next tick
/// with a head reading, never settled as uncredited.
async fn tick_kick(env: &Env, d: &Deployment) -> crate::eval::engine::Kick {
    let mut kick = crate::eval::engine::Kick::default();
    let Ok(mut r) = journal_call(env, d, Method::Get, "/confirmed-head", None).await else {
        return kick;
    };
    let Ok(heads) = cf_io::response_json::<ConfirmedHead>(&mut r, ROUND_REPLY_BYTES).await else {
        return kick;
    };
    let Some(confirmed) = heads.confirmed else {
        return kick;
    };
    let Some(epoch) = epoch_at(env, d, confirmed).await else {
        return kick;
    };
    kick.confirmed = Some(epoch.block);
    kick.epoch = Some(epoch);
    kick.head_epoch = match crate::eval::engine::head_epoch_source(confirmed, epoch.index, heads.head) {
        crate::eval::engine::HeadEpoch::Known(index) => Some(index),
        crate::eval::engine::HeadEpoch::ReadAt(h) => epoch_at(env, d, h).await.map(|e| e.index),
        crate::eval::engine::HeadEpoch::Unknown => None,
    };
    kick
}
