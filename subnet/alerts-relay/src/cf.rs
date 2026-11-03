//! Workers entry points (wasm32 only).
//!
//! - `fetch`: the publisher's `POST /alerts` and `POST /heartbeat`, which
//!   reach this Worker only through the publisher's service binding (the
//!   Worker has no route and no `workers.dev` name). Each is forwarded,
//!   body streaming, to the `AlertsRelayState` Durable Object.
//! - `scheduled`: the one-minute cron, which asks the object for one tick,
//!   and pings the external dead-man URL (when enabled) only after a tick
//!   that finished with its delivery path working.
//! - `AlertsRelayState`: one SQLite-backed object holding the sent-event
//!   ledger, the alert feed's index, the relay's alert state and the
//!   publisher's last heartbeat. It reads every body within its bound
//!   ([`crate::arena`]), writes the alert feed's objects to its private
//!   bucket, sends email through the `send_email` binding while email is on
//!   and runs one tick at a time.

use std::time::Duration;

use futures_util::StreamExt;
use futures_util::future::{Either, select};
use serde::{Deserialize, Serialize};
use slop_ninja_alerts_wire as wire;
use wasm_bindgen::JsValue;
use worker::{
    Context, Date, Delay, DurableObject, EmailMessage, Env, Headers, HttpMetadata, Method, Request,
    RequestInit, Response, Result, ScheduleContext, ScheduledEvent, SqlStorage, SqlStorageValue,
    State, console_error, console_log, durable_object, event,
};

use crate::arena::{self, Budget, ReadError, Reader};
use crate::config::{self, Config};
use crate::feed::{FiringEntry, WriteNote};
use crate::ledger::{Mark, Row, StoreError, Table};
use crate::mime::Message;
use crate::relay::{self, Failure, Mailer, SendError, TickHooks, TickReads, TickState};
use crate::rules::{self, HeartbeatRecord, Intake};
use crate::{status, tracker};

const STATE: &str = "ALERTS_RELAY_STATE_TESTNET";
const BUCKET: &str = "ALERTS_TESTNET";
const EMAIL: &str = "ALERT_EMAIL_TESTNET";
const COORDINATOR_SERVICE: &str = "COORDINATOR_TESTNET";
const EVAL_SERVICE: &str = "EVAL_WORKER_TESTNET";
/// Each status read, and the tracker object's read, answers within this or
/// counts as unanswered (the publisher's own status-read bound).
const STATUS_TIMEOUT_MS: u64 = wire::STATUS_TIMEOUT_MS;
/// A tick's reads before it evaluates ([`relay::TICK_READS_MS`]).
const TICK_READS_MS: u64 = relay::TICK_READS_MS;

fn now_ms() -> u64 {
    Date::now().as_millis()
}

/// The isolate's one budget (wasm32 is single-threaded).
fn budget() -> &'static Budget {
    thread_local! {
        static BUDGET: &'static Budget = Box::leak(Box::new(Budget::new(arena::READBACK_BUDGET)));
    }
    BUDGET.with(|b| *b)
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

fn declared_length(headers: &Headers) -> Option<u64> {
    headers
        .get("content-length")
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
}

async fn read_stream(
    mut stream: worker::ByteStream,
    bound: u64,
    declared: Option<u64>,
) -> std::result::Result<(Vec<u8>, arena::Hold<'static>), ReadError> {
    let mut reader = Reader::new(budget(), bound, declared)?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ReadError::Transport)?;
        reader.push(&chunk)?;
    }
    Ok(reader.finish())
}

/// A request body within `bound`, refused from its declared length first.
async fn request_body(
    req: &mut Request,
    bound: u64,
) -> std::result::Result<(Vec<u8>, arena::Hold<'static>), ReadError> {
    let declared = declared_length(req.headers());
    if declared.is_some_and(|n| n > bound) {
        return Err(ReadError::TooLarge);
    }
    let stream = req.stream().map_err(|_| ReadError::Transport)?;
    read_stream(stream, bound, declared).await
}

fn json_response(status: u16, body: &str) -> Result<Response> {
    let headers = Headers::new();
    headers.set("content-type", "application/json")?;
    Ok(Response::ok(body)?
        .with_status(status)
        .with_headers(headers))
}

fn read_error(e: ReadError) -> Result<Response> {
    match e {
        ReadError::TooLarge => json_response(413, r#"{"error":"too_large"}"#),
        ReadError::Budget => json_response(503, r#"{"error":"budget"}"#),
        ReadError::Transport => json_response(400, r#"{"error":"body"}"#),
    }
}

// ------------------------------------------------------------- storage

fn ensure_schema(sql: &SqlStorage) -> Result<()> {
    sql.exec(
        "CREATE TABLE IF NOT EXISTS sent (id TEXT PRIMARY KEY, mark INTEGER NOT NULL, at_ms INTEGER NOT NULL)",
        None,
    )?;
    sql.exec(
        "CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL)",
        None,
    )?;
    // The alert feed: the keys written (for the retention sweep) and the
    // firing set (for latest.json).
    sql.exec(
        "CREATE TABLE IF NOT EXISTS feed_keys (k TEXT PRIMARY KEY, at_ms INTEGER NOT NULL)",
        None,
    )?;
    sql.exec(
        "CREATE TABLE IF NOT EXISTS feed_firing (k TEXT PRIMARY KEY, v TEXT NOT NULL, at_ms INTEGER NOT NULL)",
        None,
    )?;
    Ok(())
}

#[derive(Deserialize)]
struct KeyRow {
    k: String,
}

#[derive(Deserialize)]
struct CountRow {
    n: i64,
}

#[derive(Deserialize)]
struct SentRow {
    mark: i64,
    at_ms: i64,
}

#[derive(Deserialize)]
struct KvRow {
    v: String,
}

/// The relay's store: the ledger's table (one row per event id touched
/// within the retention, see [`crate::ledger`]; its sweep cutoff is a `kv`
/// row), the feed's index tables and the private bucket. A post's puts end
/// by `answer_by` (relay clock), like its sends; a tick's are bounded by
/// [`wire::RELAY_ANSWER_MS`] each.
struct SqlTable<'a> {
    sql: &'a SqlStorage,
    env: &'a Env,
    answer_by: Option<u64>,
}

impl<'a> SqlTable<'a> {
    fn new(sql: &'a SqlStorage, env: &'a Env, answer_by: Option<u64>) -> Self {
        Self {
            sql,
            env,
            answer_by,
        }
    }

    /// What is left of the answer bound, or one answer bound for a tick.
    fn bound(&self) -> u64 {
        match self.answer_by {
            Some(by) => by.saturating_sub(now_ms()),
            None => wire::RELAY_ANSWER_MS,
        }
    }
}

const LEDGER_CUTOFF: &str = "ledger_cutoff";

fn int(n: u64) -> SqlStorageValue {
    SqlStorageValue::Integer(n as i64)
}

impl Table for SqlTable<'_> {
    fn get(&self, id: &str) -> std::result::Result<Option<Row>, StoreError> {
        let rows: Vec<SentRow> = self
            .sql
            .exec("SELECT mark, at_ms FROM sent WHERE id = ?", vec![id.into()])
            .and_then(|c| c.to_array())
            .map_err(|_| StoreError("sql get"))?;
        rows.first()
            .map(|r| {
                Mark::from_code(r.mark.max(0) as u64)
                    .map(|mark| Row {
                        mark,
                        at_ms: r.at_ms.max(0) as u64,
                    })
                    .ok_or(StoreError("sql mark"))
            })
            .transpose()
    }

    fn put(&self, id: &str, row: Row) -> std::result::Result<(), StoreError> {
        self.sql
            .exec(
                "INSERT INTO sent (id, mark, at_ms) VALUES (?, ?, ?) \
                 ON CONFLICT(id) DO UPDATE SET mark = excluded.mark, at_ms = excluded.at_ms",
                vec![id.into(), int(row.mark.code()), int(row.at_ms)],
            )
            .map(|_| ())
            .map_err(|_| StoreError("sql put"))
    }

    fn remove(&self, id: &str) -> std::result::Result<(), StoreError> {
        self.sql
            .exec("DELETE FROM sent WHERE id = ?", vec![id.into()])
            .map(|_| ())
            .map_err(|_| StoreError("sql delete"))
    }

    fn sweep(&self, before_ms: u64) -> std::result::Result<(), StoreError> {
        let cutoff = self.cutoff()?.max(before_ms);
        self.sql
            .exec("DELETE FROM sent WHERE at_ms < ?", vec![int(cutoff)])
            .map_err(|_| StoreError("sql sweep"))?;
        kv_put(self.sql, LEDGER_CUTOFF, &cutoff.to_string()).map_err(|_| StoreError("sql cutoff"))
    }

    fn cutoff(&self) -> std::result::Result<u64, StoreError> {
        match kv_get(self.sql, LEDGER_CUTOFF).map_err(|_| StoreError("sql cutoff"))? {
            None => Ok(0),
            Some(v) => v.parse().map_err(|_| StoreError("sql cutoff")),
        }
    }

    fn feed_stamp(&self, now: u64) -> std::result::Result<u64, StoreError> {
        let last: u64 = match kv_get(self.sql, FEED_STAMP).map_err(|_| StoreError("sql stamp"))? {
            None => 0,
            Some(v) => v.parse().map_err(|_| StoreError("sql stamp"))?,
        };
        let at = now.max(now_ms()).max(last.saturating_add(1));
        kv_put(self.sql, FEED_STAMP, &at.to_string()).map_err(|_| StoreError("sql stamp"))?;
        Ok(at)
    }

    fn feed_record(&self, key: &str, at: u64) -> std::result::Result<(), StoreError> {
        self.sql
            .exec(
                "INSERT INTO feed_keys (k, at_ms) VALUES (?, ?) \
                 ON CONFLICT(k) DO UPDATE SET at_ms = excluded.at_ms",
                vec![key.into(), int(at)],
            )
            .map(|_| ())
            .map_err(|_| StoreError("sql feed record"))
    }

    async fn feed_put(&self, key: &str, body: Vec<u8>) -> std::result::Result<(), SendError> {
        let bound = self.bound();
        if bound == 0 {
            return Err(SendError::Refused(Failure::new("no_time", "")));
        }
        let bucket = self
            .env
            .bucket(BUCKET)
            .map_err(|_| SendError::Refused(Failure::new("bucket_binding", "")))?;
        let put = bucket
            .put(key, body)
            .http_metadata(HttpMetadata {
                content_type: Some("application/json".into()),
                ..HttpMetadata::default()
            })
            .execute();
        match within(bound, put).await {
            None => Err(SendError::Timeout),
            Some(Ok(_)) => Ok(()),
            Some(Err(e)) => Err(SendError::Refused(Failure::new("r2_put", &e.to_string()))),
        }
    }

    fn feed_firing(
        &self,
        key: &str,
        entry: Option<&FiringEntry>,
        at: u64,
    ) -> std::result::Result<(), StoreError> {
        let done = match entry {
            Some(e) => {
                let v = serde_json::to_string(e).map_err(|_| StoreError("firing encode"))?;
                self.sql.exec(
                    "INSERT INTO feed_firing (k, v, at_ms) VALUES (?, ?, ?) \
                     ON CONFLICT(k) DO UPDATE SET v = excluded.v, at_ms = excluded.at_ms",
                    vec![key.into(), v.into(), int(at)],
                )
            }
            None => self
                .sql
                .exec("DELETE FROM feed_firing WHERE k = ?", vec![key.into()]),
        };
        done.map(|_| ()).map_err(|_| StoreError("sql firing"))
    }

    fn feed_firing_sweep(&self, before: u64) -> std::result::Result<(), StoreError> {
        self.sql
            .exec("DELETE FROM feed_firing WHERE at_ms < ?", vec![int(before)])
            .map(|_| ())
            .map_err(|_| StoreError("sql firing sweep"))
    }

    fn feed_note(&self, note: &WriteNote) -> std::result::Result<(), StoreError> {
        let v = serde_json::to_string(note).map_err(|_| StoreError("note encode"))?;
        kv_put(self.sql, FEED_NOTE, &v).map_err(|_| StoreError("sql note"))
    }

    fn feed_expired(
        &self,
        before: u64,
        max: usize,
    ) -> std::result::Result<Vec<String>, StoreError> {
        let rows: Vec<KeyRow> = self
            .sql
            .exec(
                "SELECT k FROM feed_keys WHERE at_ms < ? ORDER BY at_ms LIMIT ?",
                vec![int(before), int(max as u64)],
            )
            .and_then(|c| c.to_array())
            .map_err(|_| StoreError("sql feed expired"))?;
        Ok(rows.into_iter().map(|r| r.k).collect())
    }

    async fn feed_delete(&self, keys: &[String]) -> std::result::Result<(), SendError> {
        let bucket = self
            .env
            .bucket(BUCKET)
            .map_err(|_| SendError::Refused(Failure::new("bucket_binding", "")))?;
        let names: Vec<&str> = keys.iter().map(String::as_str).collect();
        match within(self.bound(), bucket.delete_multiple(names)).await {
            None => Err(SendError::Timeout),
            Some(Ok(())) => Ok(()),
            Some(Err(e)) => Err(SendError::Refused(Failure::new(
                "r2_delete",
                &e.to_string(),
            ))),
        }
    }

    fn feed_forget(&self, keys: &[String]) -> std::result::Result<(), StoreError> {
        for k in keys {
            self.sql
                .exec("DELETE FROM feed_keys WHERE k = ?", vec![k.as_str().into()])
                .map_err(|_| StoreError("sql feed forget"))?;
        }
        Ok(())
    }

    fn feed_firing_entries(&self) -> std::result::Result<(Vec<FiringEntry>, u64), StoreError> {
        let count: Vec<CountRow> = self
            .sql
            .exec("SELECT COUNT(*) AS n FROM feed_firing", None)
            .and_then(|c| c.to_array())
            .map_err(|_| StoreError("sql firing count"))?;
        let total = count.first().map_or(0, |r| r.n.max(0) as u64);
        // Rows one at a time, oldest first, while they fit latest.json's
        // bound for the firing set (one post's events bound).
        let cursor = self
            .sql
            .exec("SELECT v FROM feed_firing ORDER BY at_ms", None)
            .map_err(|_| StoreError("sql firing list"))?;
        let mut out = Vec::new();
        let mut room = wire::POST_EVENTS_BYTES;
        for row in cursor.next::<KvRow>() {
            let row = row.map_err(|_| StoreError("sql firing row"))?;
            if row.v.len() + 1 > room {
                break;
            }
            room -= row.v.len() + 1;
            if let Ok(e) = serde_json::from_str::<FiringEntry>(&row.v) {
                out.push(e);
            }
        }
        Ok((out, total))
    }

    fn feed_last_note(&self) -> std::result::Result<Option<WriteNote>, StoreError> {
        Ok(kv_get(self.sql, FEED_NOTE)
            .map_err(|_| StoreError("sql note"))?
            .and_then(|v| serde_json::from_str(&v).ok()))
    }
}

/// The last feed stamp (keys are stamped strictly increasing) and the last
/// write's outcome.
const FEED_STAMP: &str = "feed_stamp";
const FEED_NOTE: &str = "feed_last_write";

fn kv_get(sql: &SqlStorage, k: &str) -> Result<Option<String>> {
    let rows: Vec<KvRow> = sql
        .exec("SELECT v FROM kv WHERE k = ?", vec![k.into()])?
        .to_array()?;
    Ok(rows.into_iter().next().map(|r| r.v))
}

fn kv_put(sql: &SqlStorage, k: &str, v: &str) -> Result<()> {
    sql.exec(
        "INSERT INTO kv (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        vec![k.into(), v.into()],
    )?;
    Ok(())
}

fn kv_delete(sql: &SqlStorage, k: &str) -> Result<()> {
    sql.exec("DELETE FROM kv WHERE k = ?", vec![k.into()])?;
    Ok(())
}

// --------------------------------------------------------------- email

/// The `send_email` binding (raw `EmailMessage` path), used only while email
/// is on. A post's sends end by
/// `answer_by` (relay clock), so the relay answers before the publisher stops
/// waiting; a tick's sends are bounded by [`wire::RELAY_ANSWER_MS`] each.
struct Binding<'a> {
    env: &'a Env,
    answer_by: Option<u64>,
}

/// A thrown error, content-free: its `code` (Cloudflare's, for example
/// `E_RECIPIENT_NOT_ALLOWED`; workers-rs names the recipient refusal
/// `RCPT_NOT_ALLOWED`), else its `name`, and its message without any word
/// that holds an address or a URL.
fn js_failure(v: &JsValue) -> Failure {
    let get = |k: &str| {
        js_sys::Reflect::get(v, &JsValue::from_str(k))
            .ok()
            .and_then(|c| c.as_string())
    };
    let code = get("code")
        .or_else(|| get("name"))
        .unwrap_or_else(|| "send_error".into());
    let message = get("message").or_else(|| v.as_string()).unwrap_or_default();
    Failure::new(&code, &message)
}

impl Mailer for Binding<'_> {
    async fn send(&self, m: &Message) -> std::result::Result<(), SendError> {
        let bound = match self.answer_by {
            Some(by) => by.saturating_sub(now_ms()),
            None => wire::RELAY_ANSWER_MS,
        };
        if bound == 0 {
            // The answer bound has passed: nothing is sent.
            return Err(SendError::Refused(Failure::new("no_time", "")));
        }
        let sender = self
            .env
            .send_email(EMAIL)
            .map_err(|_| SendError::Refused(Failure::new("binding", "")))?;
        let message = EmailMessage::new(&m.from, &m.to, &m.raw)
            .map_err(|e| SendError::Refused(js_failure(e.as_ref())))?;
        match within(bound, sender.send(&message)).await {
            None => Err(SendError::Timeout),
            Some(Ok(_)) => Ok(()),
            Some(Err(e)) => Err(SendError::Refused(js_failure(e.as_ref()))),
        }
    }
}

fn log_failure(context: &str, f: &Failure) {
    console_error!(
        "relay: {context} send failed: code {} message \"{}\"",
        f.code,
        f.message
    );
}

// ---------------------------------------------------------------- reads

/// The tracker's upload, read within its schema bound and the status-read
/// time bound (a bucket that does not answer in time is unavailable).
async fn read_tracker(env: &Env, now: u64) -> tracker::Read {
    let Ok(bucket) = env.bucket(BUCKET) else {
        return tracker::Read::Unreadable("bucket binding");
    };
    within(STATUS_TIMEOUT_MS, async {
        let object = match bucket.get(tracker::UPLOAD_KEY).execute().await {
            Ok(Some(o)) => o,
            Ok(None) => return tracker::Read::Missing,
            Err(_) => return tracker::Read::Unavailable,
        };
        let bound = tracker::upload_bound(now);
        let size = object.size();
        if size > bound {
            return tracker::Read::Unreadable("too large");
        }
        let uploaded_ms = object.uploaded().as_millis();
        let Ok(_hold) = budget().reserve(arena::read_footprint(bound)) else {
            return tracker::Read::Unavailable;
        };
        let Some(body) = object.body() else {
            return tracker::Read::Unavailable;
        };
        match body.bytes().await {
            Ok(bytes) if bytes.len() as u64 == size => tracker::parse(&bytes, uploaded_ms),
            Ok(_) => tracker::Read::Unreadable("short"),
            Err(_) => tracker::Read::Unavailable,
        }
    })
    .await
    .unwrap_or(tracker::Read::Unavailable)
}

/// One coordinator Worker's `/status`, read within the coordinator's route
/// bound with that Worker's status token.
async fn read_status(env: &Env, service: &str, token_name: &str) -> status::Read {
    let Some(token) = var_or_secret(env, token_name).filter(|t| !t.trim().is_empty()) else {
        return status::Read::NoToken;
    };
    let Ok(fetcher) = env.service(service) else {
        return status::Read::Unanswered;
    };
    let read = within(STATUS_TIMEOUT_MS, async {
        let headers = Headers::new();
        headers
            .set("authorization", &format!("Bearer {}", token.trim()))
            .ok()?;
        let mut init = RequestInit::new();
        init.with_method(Method::Get).with_headers(headers);
        let request = Request::new_with_init("https://status.internal/status", &init).ok()?;
        let mut response = fetcher.fetch_request(request).await.ok()?;
        if response.status_code() != 200 {
            return None;
        }
        let declared = declared_length(response.headers());
        let stream = response.stream().ok()?;
        let (body, _hold) = read_stream(stream, arena::STATUS_BODY_BOUND, declared)
            .await
            .ok()?;
        status::parse(&body)
    })
    .await
    .flatten();
    read.map_or(status::Read::Unanswered, status::Read::Ok)
}

// ------------------------------------------------------- durable object

/// The relay's state and serializer.
#[durable_object(fetch)]
pub struct AlertsRelayState {
    state: State,
    env: Env,
}

/// The tick lease: the holder's id and until when it holds.
#[derive(Serialize, Deserialize)]
struct TickLease {
    holder: String,
    until_ms: u64,
}

fn tick_lease(sql: &SqlStorage) -> Option<TickLease> {
    kv_get(sql, "tick_lease")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
}

/// A tick's lease and state row in the object's SQLite. Every write checks
/// the lease is still this tick's, so a tick that overran its lease (another
/// took it over) never overwrites the newer tick's state.
struct Hooks<'a> {
    sql: &'a SqlStorage,
    holder: String,
}

impl Hooks<'_> {
    fn held(&self) -> bool {
        tick_lease(self.sql).is_some_and(|l| l.holder == self.holder)
    }
}

impl TickHooks for Hooks<'_> {
    fn renew(&self, ms: u64) -> bool {
        if !self.held() {
            return false;
        }
        let lease = TickLease {
            holder: self.holder.clone(),
            until_ms: now_ms().saturating_add(ms),
        };
        serde_json::to_string(&lease)
            .ok()
            .is_some_and(|v| kv_put(self.sql, "tick_lease", &v).is_ok())
    }

    fn save(&self, st: &TickState) -> std::result::Result<(), StoreError> {
        if !self.held() {
            return Err(StoreError("lease lost"));
        }
        let v = serde_json::to_string(st).map_err(|_| StoreError("state encode"))?;
        kv_put(self.sql, "state", &v).map_err(|_| StoreError("state put"))
    }
}

/// A distinct holder id per tick within the object.
fn holder(now: u64) -> String {
    thread_local! {
        static N: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    let n = N.with(|c| {
        c.set(c.get().wrapping_add(1));
        c.get()
    });
    format!("tick:{now}:{n}")
}

fn intake(sql: &SqlStorage) -> Intake {
    kv_get(sql, "intake")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_intake(sql: &SqlStorage, i: &Intake) {
    if serde_json::to_string(i)
        .ok()
        .is_none_or(|v| kv_put(sql, "intake", &v).is_err())
    {
        console_error!("relay: intake record not saved");
    }
}

impl AlertsRelayState {
    async fn alerts(&self, mut req: Request, sql: &SqlStorage, cfg: &Config) -> Result<Response> {
        let started = now_ms();
        let (body, _hold) = match request_body(&mut req, wire::post_bound() as u64).await {
            Ok(b) => b,
            Err(e) => {
                if let Some(code) = refused_read(e) {
                    let mut i = intake(sql);
                    i.post(Some(code), now_ms());
                    save_intake(sql, &i);
                }
                return read_error(e);
            }
        };
        let answer_by = Some(started.saturating_add(wire::RELAY_ANSWER_MS));
        let mailer = Binding {
            env: &self.env,
            answer_by,
        };
        let table = SqlTable::new(sql, &self.env, answer_by);
        let answer = relay::handle_post(&body, cfg, &table, &mailer, budget(), now_ms()).await;
        match (answer.status, answer.refusal) {
            (200, _) => {
                let mut i = intake(sql);
                if i.posts_refused > 0 {
                    i.post(None, now_ms());
                    save_intake(sql, &i);
                }
            }
            (_, Some(code)) => {
                let mut i = intake(sql);
                i.post(Some(code), now_ms());
                save_intake(sql, &i);
                console_error!("relay: post refused: {}", answer.body);
            }
            (status, None) => console_error!("relay: post answered {status}: {}", answer.body),
        }
        json_response(answer.status, &answer.body)
    }

    async fn heartbeat(
        &self,
        mut req: Request,
        sql: &SqlStorage,
        cfg: &Config,
    ) -> Result<Response> {
        let (body, _hold) = match request_body(&mut req, wire::HEARTBEAT_BYTES as u64).await {
            Ok(b) => b,
            Err(e) => {
                if let Some(code) = refused_read(e) {
                    let mut i = intake(sql);
                    i.heartbeat(Some(code), now_ms());
                    save_intake(sql, &i);
                }
                return read_error(e);
            }
        };
        let mut i = intake(sql);
        match relay::heartbeat(&body, &cfg.network, now_ms()) {
            Ok(record) => {
                kv_put(sql, "heartbeat", &serde_json::to_string(&record)?)?;
                if i.heartbeats_refused > 0 {
                    i.heartbeat(None, now_ms());
                    save_intake(sql, &i);
                }
                json_response(200, "{}")
            }
            Err(r) => {
                i.heartbeat(Some(r.code()), now_ms());
                save_intake(sql, &i);
                console_error!("relay: heartbeat refused: {}", r.code());
                json_response(400, &format!(r#"{{"error":"{}"}}"#, r.code()))
            }
        }
    }

    async fn tick(&self, sql: &SqlStorage, cfg: &Config) -> Result<Response> {
        let now = now_ms();
        if tick_lease(sql).is_some_and(|l| l.until_ms > now) {
            // The tick under way pings when it finishes.
            return json_response(200, r#"{"tick":"busy"}"#);
        }
        let hooks = Hooks {
            sql,
            holder: holder(now),
        };
        let lease = TickLease {
            holder: hooks.holder.clone(),
            until_ms: now.saturating_add(TICK_READS_MS),
        };
        kv_put(sql, "tick_lease", &serde_json::to_string(&lease)?)?;
        let mut st: TickState = kv_get(sql, "state")?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let heartbeat: Option<HeartbeatRecord> =
            kv_get(sql, "heartbeat")?.and_then(|s| serde_json::from_str(&s).ok());
        let intake = intake(sql);
        let watch = st.watch_since_ms.unwrap_or(now);
        let workers = if rules::publisher_down(heartbeat.as_ref(), watch, now) {
            Some([
                read_status(&self.env, COORDINATOR_SERVICE, "COORDINATOR_STATUS_TOKEN").await,
                read_status(&self.env, EVAL_SERVICE, "EVAL_STATUS_TOKEN").await,
            ])
        } else {
            None
        };
        let tracker = read_tracker(&self.env, now).await;
        let mailer = Binding {
            env: &self.env,
            answer_by: None,
        };
        let reads = TickReads {
            heartbeat: heartbeat.as_ref(),
            intake: &intake,
            tracker: &tracker,
            workers: workers.as_ref(),
        };
        let table = SqlTable::new(sql, &self.env, None);
        let report =
            relay::tick(&mut st, cfg, &reads, &table, &mailer, budget(), &hooks, now).await;
        // Release the lease only while it is still this tick's.
        if hooks.held() {
            kv_delete(sql, "tick_lease")?;
        }
        match report {
            Ok(r) => {
                // The budget's peak: the drill's live reading of the
                // accounted memory (arena rule; the reserve is not measured
                // otherwise).
                console_log!(
                    "relay: tick {} due {} written {} sent {} undelivered {} stopped {} delivery_ok {} email {} swept {} latest {} budget peak {} of {}",
                    st.ticks,
                    r.due,
                    r.written,
                    r.sent,
                    r.undelivered,
                    r.stopped,
                    r.delivery_ok,
                    if cfg.email.is_some() { "on" } else { "off" },
                    r.swept,
                    r.latest.unwrap_or("skipped"),
                    budget().peak(),
                    arena::READBACK_BUDGET
                );
                if let Some(d) = r.drill_feed {
                    // The deploy-time drill (DRILL_ID) in the feed: written,
                    // timeout or refused (a refusal's code follows).
                    console_log!("relay: drill feed {d}");
                }
                if let Some(d) = r.drill {
                    // The drill's email (email on only): sent, timeout or
                    // refused.
                    console_log!("relay: drill {d}");
                }
                if let Some(f) = &r.failure {
                    log_failure("tick", f);
                }
                for w in &cfg.warnings {
                    console_error!("relay: setting ignored: {w}");
                }
                let headers = Headers::new();
                headers.set("content-type", "application/json")?;
                if r.delivery_ok {
                    headers.set(PING_HEADER, "1")?;
                }
                let body = format!(
                    r#"{{"due":{},"written":{},"sent":{},"undelivered":{},"delivery_ok":{}}}"#,
                    r.due, r.written, r.sent, r.undelivered, r.delivery_ok
                );
                Ok(Response::ok(body)?.with_headers(headers))
            }
            Err(StoreError(code)) => {
                console_error!("relay: tick store error {code}");
                json_response(503, r#"{"error":"store"}"#)
            }
        }
    }
}

/// A body refused for its content (too large, or no readable body), which
/// the intake record counts; a budget refusal is the relay's own and is not.
fn refused_read(e: ReadError) -> Option<&'static str> {
    match e {
        ReadError::TooLarge => Some("too_large"),
        ReadError::Transport => Some("body"),
        ReadError::Budget => None,
    }
}

impl DurableObject for AlertsRelayState {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let sql = self.state.storage().sql();
        ensure_schema(&sql)?;
        let cfg = match config::parse(|name| var_or_secret(&self.env, name)) {
            Ok(c) => c,
            Err(e) => {
                console_error!("relay: configuration refused: {e}");
                return json_response(503, r#"{"error":"config_refused"}"#);
            }
        };
        match (req.method(), req.path().as_str()) {
            (Method::Post, wire::ALERTS_PATH) => self.alerts(req, &sql, &cfg).await,
            (Method::Post, wire::HEARTBEAT_PATH) => self.heartbeat(req, &sql, &cfg).await,
            (Method::Post, "/tick") => self.tick(&sql, &cfg).await,
            _ => json_response(404, r#"{"error":"not_found"}"#),
        }
    }
}

fn stub(env: &Env) -> Result<worker::Stub> {
    env.durable_object(STATE)?
        .id_from_name("alerts-relay/testnet")?
        .get_stub()
}

// --------------------------------------------------------------- entries

/// Only the publisher's two routes; the request (and its body stream) goes
/// to the state object as it is.
#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = req.path();
    if req.method() != Method::Post || (path != wire::ALERTS_PATH && path != wire::HEARTBEAT_PATH) {
        return json_response(404, r#"{"error":"not_found"}"#);
    }
    stub(&env)?.fetch_with_request(req).await
}

/// Set on a tick's answer when the external dead-man may be pinged: the tick
/// finished, and its deliveries have not been failing for the undelivered
/// hold.
const PING_HEADER: &str = "x-alerts-relay-delivery-ok";

/// The external dead-man ping (a GET within the outbound bound), after a
/// tick that finished with its delivery path working.
async fn ping(url: &str) {
    let mut init = RequestInit::new();
    init.with_method(Method::Get);
    let ok = within(wire::OUTBOUND_TIMEOUT_MS, async {
        let request = Request::new_with_init(url, &init).ok()?;
        let response = worker::Fetch::Request(request).send().await.ok()?;
        Some((200..300).contains(&response.status_code()))
    })
    .await
    .flatten()
    .unwrap_or(false);
    if !ok {
        console_error!("relay: external heartbeat not accepted");
    }
}

#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let mut init = RequestInit::new();
    init.with_method(Method::Post);
    let answered = match (
        stub(&env),
        Request::new_with_init("https://alerts-relay.internal/tick", &init),
    ) {
        (Ok(s), Ok(r)) => s.fetch_with_request(r).await.map(|r| {
            let ok = r.headers().get(PING_HEADER).ok().flatten().is_some();
            (r.status_code(), ok)
        }),
        (Err(e), _) | (_, Err(e)) => Err(e),
    };
    // The relay's own dead-man ping, configured outside Cloudflare's
    // account (off unless HEARTBEAT_ENABLED; the alert feed's latest.json is
    // the dead-man a reader outside Cloudflare reads). It means "the cron ran and
    // the delivery path works as far as the relay knows": a busy tick (the
    // tick under way pings), a stopped one, a store error, a refused
    // configuration or deliveries failing for the undelivered hold skip it,
    // so the external monitor pages.
    match answered {
        Ok((200, true)) => {
            if let Ok(Config {
                heartbeat: Some(url),
                ..
            }) = config::parse(|name| var_or_secret(&env, name))
            {
                ping(&url).await;
            }
        }
        Ok((200, false)) => {}
        Ok((code, _)) => console_error!("relay: tick answered {code}"),
        Err(_) => console_error!("relay: tick did not answer"),
    }
}
