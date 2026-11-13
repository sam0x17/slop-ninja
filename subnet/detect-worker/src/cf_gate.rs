//! `DetectGate` (wasm32 only): the one admission object. Its tickets and
//! keys live in memory, so a restart forgets every ticket (their invocations
//! then see `gone` and the page asks the reader to try again) and its keys.
//! Its hourly scan counts live in its own SQLite (`crate::counters`), so a
//! restart loses none, and its alarm flushes them to the detect store.
//!
//! Internal routes, called only by the detect Worker:
//! - `/enter {client: {kind, value}, class, hold_ms}`: a new ticket of a
//!   ticket class. The client key is an HMAC-SHA-256 of the kind (`prefix`,
//!   `owner` or `api`) and the value (the address prefix, the owner hash or
//!   the API key hash) under a key held only here and replaced each UTC day;
//!   neither the value nor the key is kept.
//! - `/ask {ticket, hold_ms}`: renews the ticket and answers its status.
//!   While it is queued, the answer waits until the ticket is admitted (or
//!   gone) or `hold_ms` ends, then carries the current position and the hold
//!   for the next ask (the position-scaled hold): a queue change wakes only
//!   the ticket it admits, and a ticket far back asks rarely.
//! - `/done {ticket, served_ms?, count?}`: frees the slot. A `count`
//!   (channel, package, outcome) is added to the hour's row before the
//!   answer leaves, and arms the flush alarm when none is set.
//! - `/leave {ticket}`, `/warm`.
//!
//! No route receives or answers any text. Feedback never comes here: it has
//! its own object (`DetectFeedback`), because its route needs no challenge.

use crate::cf::{self, GateWarm, LANE, STORE, var_switch_on, waits, within};
use crate::cf_io::{self, now_ms};
use crate::counters::{self, Count};
use crate::gate::{Config, Gate, Status, Ticket};
use crate::{routes, silence_panics};
use futures_channel::oneshot;
use futures_util::future::select;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use slop_ninja_detect_page::{TicketClass, ae};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::Duration;
use wasm_bindgen::JsValue;
use worker::{
    Delay, DurableObject, Env, Method, Request, Response, Result, ScheduledTime, SqlStorageValue, State, durable_object,
};

/// Scans that may run at once: one Container instance (`max_instances`
/// 1) with one session. Widening is a deployment decision.
const SLOTS: usize = 1;
/// One UTC day: the client key's lifetime.
const DAY_MS: u64 = 86_400_000;
/// Internal request bodies are a few hundred bytes.
const BODY_BYTES: usize = 4 * 1024;

struct Keys {
    day: u64,
    client: [u8; 32],
}

fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    cf::random_bytes(N as u32).copy_to(&mut out);
    out
}

impl Keys {
    fn fresh(day: u64) -> Keys {
        Keys { day, client: random() }
    }

    /// Replaces the key at the first call of a new UTC day.
    fn today(&mut self, now: u64) -> &mut Keys {
        let day = now / DAY_MS;
        if day != self.day {
            *self = Keys::fresh(day);
        }
        self
    }

    fn client_key(&self, kind: ClientKind, value: &str) -> u64 {
        let Ok(mut m) = Hmac::<Sha256>::new_from_slice(&self.client) else { return 0 };
        m.update(b"slop_ninja_detect_client_v2\0");
        m.update(kind.tag());
        m.update(b"\0");
        m.update(value.as_bytes());
        let d = m.finalize().into_bytes();
        let mut k = [0u8; 8];
        k.copy_from_slice(d.get(..8).unwrap_or(&[0u8; 8]));
        u64::from_le_bytes(k)
    }
}

/// What a client key names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ClientKind {
    /// An anonymous request's address prefix.
    Prefix,
    /// A signed-in paid account's owner hash.
    Owner,
    /// An API key's hash.
    Api,
}

impl ClientKind {
    fn tag(self) -> &'static [u8] {
        match self {
            ClientKind::Prefix => b"prefix",
            ClientKind::Owner => b"owner",
            ClientKind::Api => b"api",
        }
    }
}

#[durable_object]
pub struct DetectGate {
    state: State,
    env: Env,
    gate: RefCell<Gate>,
    keys: RefCell<Keys>,
    /// The waiting ask of each queued ticket (at most one per ticket: a new
    /// ask replaces the entry of one that returned at its hold's end), woken
    /// when the ticket settles.
    waiters: RefCell<BTreeMap<Ticket, oneshot::Sender<()>>>,
}

#[derive(Deserialize)]
struct ClientIn {
    kind: ClientKind,
    value: String,
}

#[derive(Deserialize)]
struct EnterReq {
    client: ClientIn,
    class: String,
    hold_ms: u64,
}

#[derive(Deserialize)]
struct AskReq {
    ticket: String,
    hold_ms: u64,
}

#[derive(Deserialize)]
struct DoneReq {
    ticket: String,
    #[serde(default)]
    served_ms: Option<u64>,
    #[serde(default)]
    count: Option<Count>,
}

#[derive(Serialize)]
struct StatusReply {
    ticket: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimate_ms: Option<u64>,
    /// The hold the next ask carries, while queued.
    #[serde(skip_serializing_if = "Option::is_none")]
    hold_ms: Option<u64>,
    awake: bool,
    tool: bool,
}

fn ok_json<T: Serialize>(v: &T) -> Result<Response> {
    let mut r = Response::from_bytes(serde_json::to_vec(v).unwrap_or_default())?;
    r.headers_mut().set("Content-Type", "application/json")?;
    Ok(r)
}

fn status_code(code: u16) -> Result<Response> {
    Ok(Response::empty()?.with_status(code))
}

fn parse_ticket(hex_id: &str) -> Option<u128> {
    if hex_id.len() != 32 {
        return None;
    }
    let mut b = [0u8; 16];
    hex::decode_to_slice(hex_id, &mut b).ok()?;
    Some(u128::from_le_bytes(b))
}

fn ticket_hex(t: u128) -> String {
    hex::encode(t.to_le_bytes())
}

impl DetectGate {
    /// Wakes the asks of the tickets the last change settled (admitted, or
    /// dropped from the line) and forgets their entries.
    fn wake_settled(&self) {
        let settled = self.gate.borrow_mut().take_settled();
        let mut ws = self.waiters.borrow_mut();
        for t in settled {
            if let Some(tx) = ws.remove(&t) {
                let _ = tx.send(());
            }
        }
    }

    fn reply(&self, ticket: u128, st: Status) -> Result<Response> {
        let g = self.gate.borrow();
        let now = now_ms();
        let (status, position) = match st {
            Status::Admitted => ("admitted", None),
            Status::Queued { position } => ("queued", Some(position)),
            Status::Gone => ("gone", None),
        };
        let base = waits(&self.env).hold_ms;
        ok_json(&StatusReply {
            ticket: ticket_hex(ticket),
            status,
            position,
            estimate_ms: position.and_then(|p| g.estimate_ms(p)),
            hold_ms: position.map(|p| g.hold_ms(p, base)),
            awake: g.awake_until() > now,
            tool: var_switch_on(&self.env, "DETECT_TOOL"),
        })
    }

    /// Adds one to the count's hour row, before the release is answered,
    /// and arms the flush alarm when none is set. A count the gate does not
    /// recognise is dropped (and noted), never written.
    async fn count(&self, c: &Count, now: u64) {
        if !c.valid() {
            cf::point(&self.env, ae::detect::ERROR, &["count_invalid"], 1.0);
            return;
        }
        let sql = self.state.storage().sql();
        let binds: Vec<SqlStorageValue> = vec![
            counters::hour_key(now).into(),
            c.channel.clone().into(),
            c.package_load.clone().into(),
            c.outcome.clone().into(),
        ];
        if sql.exec(counters::INCREMENT, binds).is_err() {
            cf::point(&self.env, ae::detect::ERROR, &["count_write"], 1.0);
            return;
        }
        let storage = self.state.storage();
        if matches!(storage.get_alarm().await, Ok(None)) {
            let _ = storage.set_alarm(at(counters::alarm_at(now))).await;
        }
    }

    /// The flush: pending rows to D1 in one batch, then marked and pruned.
    /// When it is done: a full flush's rows (a backlog) continue at once,
    /// increments that arrived meanwhile within the liveness interval.
    async fn flush(&self, now: u64) -> std::result::Result<Option<u64>, ()> {
        let sql = self.state.storage().sql();
        let rows: Vec<counters::Row> = sql
            .exec(counters::PENDING, vec![SqlStorageValue::Integer(counters::ROWS_PER_FLUSH as i64)])
            .and_then(|c| c.to_array())
            .map_err(|_| ())?;
        if !rows.is_empty() {
            let db = self.env.d1(STORE).map_err(|_| ())?;
            let mut statements = Vec::new();
            for chunk in counters::statements(&rows) {
                let mut binds: Vec<JsValue> = Vec::with_capacity(chunk.len() * counters::PARAMS_PER_ROW);
                for r in chunk {
                    binds.extend([
                        JsValue::from_str(&r.hour),
                        JsValue::from_str(&r.channel),
                        JsValue::from_str(&r.package_load),
                        JsValue::from_str(&r.outcome),
                        JsValue::from_f64(r.n as f64),
                    ]);
                }
                statements.push(db.prepare(counters::flush_sql(chunk.len())).bind(&binds).map_err(|_| ())?);
            }
            let wait = waits(&self.env).hold_ms;
            within(wait, db.batch(statements)).await.ok_or(())?.map_err(|_| ())?;
            for r in &rows {
                let binds: Vec<SqlStorageValue> = vec![
                    SqlStorageValue::Integer(r.n),
                    r.hour.clone().into(),
                    r.channel.clone().into(),
                    r.package_load.clone().into(),
                    r.outcome.clone().into(),
                    SqlStorageValue::Integer(r.n),
                ];
                sql.exec(counters::MARK_FLUSHED, binds).map_err(|_| ())?;
            }
        }
        sql.exec(counters::DELETE_CLOSED, vec![counters::hour_key(now).into()]).map_err(|_| ())?;
        #[derive(Deserialize)]
        struct C {
            c: i64,
        }
        let left = sql.exec(counters::ANY_PENDING, None).and_then(|c| c.one::<C>()).map_err(|_| ())?;
        Ok(match (rows.len() >= counters::ROWS_PER_FLUSH, left.c > 0) {
            (_, false) => None,
            (true, true) => Some(now),
            (false, true) => Some(counters::alarm_at(now)),
        })
    }

    /// Asks the lane to start its instance (it answers once the start is
    /// requested; the load runs in its alarm).
    async fn start_lane(&self) -> bool {
        let Ok(ns) = self.env.durable_object(LANE) else { return false };
        let Ok(id) = ns.id_from_name("lane-0") else { return false };
        let Ok(stub) = id.get_stub() else { return false };
        let Ok(req) = cf_io::internal_request("https://lane/warm", Method::Post, None, Some(10_000)) else {
            return false;
        };
        match stub.fetch_with_request(req).await {
            Ok(mut r) => {
                let ok = matches!(r.status_code(), 200 | 202);
                cf_io::discard(&mut r);
                ok
            }
            Err(_) => false,
        }
    }
}

/// An absolute alarm time.
fn at(ms: u64) -> ScheduledTime {
    ScheduledTime::new(js_sys::Date::new(&JsValue::from_f64(ms as f64)))
}

impl DurableObject for DetectGate {
    fn new(state: State, env: Env) -> Self {
        silence_panics();
        // The hour counts' table; an object that cannot create it still
        // admits tickets, and each release then notes the failed write.
        let _ = state.storage().sql().exec(counters::GATE_SCHEMA, None);
        let cfg = Config {
            slots: SLOTS,
            scan_lease_ms: waits(&env).scan_lease_ms(),
            idle_ms: cf::ms_var(&env, "IDLE_MS").unwrap_or(600_000),
            max_tickets: routes::MAX_TICKETS,
        };
        Self {
            state,
            env,
            gate: RefCell::new(Gate::new(cfg)),
            keys: RefCell::new(Keys::fresh(now_ms() / DAY_MS)),
            waiters: RefCell::new(BTreeMap::new()),
        }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        silence_panics();
        if req.method() != Method::Post {
            return status_code(405);
        }
        let now = now_ms();
        match req.path().as_str() {
            "/enter" => {
                let Ok(e) = cf_io::request_json::<EnterReq>(&mut req, BODY_BYTES).await else {
                    return status_code(400);
                };
                let Some(class) = TicketClass::parse(&e.class) else { return status_code(400) };
                let client = self.keys.borrow_mut().today(now).client_key(e.client.kind, &e.client.value);
                drop(e.client);
                let ticket = u128::from_le_bytes(random());
                let st = self.gate.borrow_mut().enter(ticket, client, class, now, e.hold_ms.saturating_mul(2));
                self.wake_settled();
                match st {
                    Some(st) => self.reply(ticket, st),
                    None => ok_json(&StatusReply {
                        ticket: String::new(),
                        status: "busy",
                        position: None,
                        estimate_ms: None,
                        hold_ms: None,
                        awake: false,
                        tool: true,
                    }),
                }
            }
            "/ask" => {
                let Ok(a) = cf_io::request_json::<AskReq>(&mut req, BODY_BYTES).await else { return status_code(400) };
                let Some(ticket) = parse_ticket(&a.ticket) else { return status_code(400) };
                let lease = a.hold_ms.saturating_mul(2);
                let st = self.gate.borrow_mut().ask(ticket, now, lease);
                self.wake_settled();
                if matches!(st, Status::Queued { .. }) {
                    let (tx, rx) = oneshot::channel();
                    self.waiters.borrow_mut().insert(ticket, tx);
                    let _ = select(Box::pin(rx), Box::pin(Delay::from(Duration::from_millis(a.hold_ms)))).await;
                    let st = self.gate.borrow_mut().ask(ticket, now_ms(), lease);
                    self.wake_settled();
                    return self.reply(ticket, st);
                }
                self.reply(ticket, st)
            }
            "/done" => {
                let Ok(d) = cf_io::request_json::<DoneReq>(&mut req, BODY_BYTES).await else { return status_code(400) };
                let Some(ticket) = parse_ticket(&d.ticket) else { return status_code(400) };
                // Counted before the release answers: durable once acknowledged.
                if let Some(c) = &d.count {
                    self.count(c, now).await;
                }
                self.gate.borrow_mut().done(ticket, now, d.served_ms);
                self.wake_settled();
                status_code(204)
            }
            "/leave" => {
                let Ok(d) = cf_io::request_json::<DoneReq>(&mut req, BODY_BYTES).await else { return status_code(400) };
                if let Some(ticket) = parse_ticket(&d.ticket) {
                    self.gate.borrow_mut().leave(ticket, now);
                    self.wake_settled();
                }
                status_code(204)
            }
            "/warm" => {
                if !var_switch_on(&self.env, "DETECT_TOOL") {
                    let g = self.gate.borrow();
                    return ok_json(&WarmReply { start: false, awake_until: g.awake_until() });
                }
                let w = self.gate.borrow_mut().warm(now);
                // An expiry inside `warm` may admit a waiting ticket.
                self.wake_settled();
                if w.start && !self.start_lane().await {
                    self.gate.borrow_mut().asleep();
                }
                ok_json(&WarmReply { start: w.start, awake_until: w.awake_until })
            }
            _ => status_code(404),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        silence_panics();
        let now = now_ms();
        // A failed flush leaves every row; the next alarm tries again. A
        // backlog past one flush's rows continues at once.
        let next = match self.flush(now).await {
            Ok(next) => next,
            Err(()) => {
                cf::point(&self.env, ae::detect::ERROR, &["d1"], 1.0);
                Some(counters::alarm_at(now))
            }
        };
        if let Some(t) = next {
            let _ = self.state.storage().set_alarm(at(t)).await;
        }
        Response::empty()
    }
}

#[derive(Serialize)]
struct WarmReply {
    start: bool,
    awake_until: u64,
}

// The Worker reads the warm reply with this shape.
const _: fn(GateWarm) = |_| {};
