//! `DetectGate` (wasm32 only): the one admission object. Memory only: it
//! stores nothing, so a restart forgets every ticket (their invocations then
//! see `gone` and the page asks the reader to try again) and its keys.
//!
//! Internal routes, called only by the detect Worker:
//! - `/enter {prefix, hold_ms}`: a new ticket. The client key is an
//!   HMAC-SHA-256 of the address prefix under a key held only here and
//!   replaced each UTC day; neither the prefix nor the key is kept.
//! - `/ask {ticket, hold_ms, version}`: renews the ticket and answers its
//!   status; while nothing has changed since `version`, the answer waits
//!   for a change or for `hold_ms`, so positions update without polling.
//! - `/done {ticket, served_ms?, band?, length?}`: frees the slot; with a
//!   band, answers a feedback receipt.
//! - `/leave {ticket}`, `/warm`, `/feedback {receipt, claim}`.
//!
//! No route receives or answers any text.

use crate::cf::{self, GateWarm, LANE, ms_var, switch_on};
use crate::cf_io::{self, now_ms};
use crate::gate::{Config, Gate, Status};
use crate::policy::{Band, LengthBin};
use crate::receipt::{self, Claim};
use crate::{routes, silence_panics};
use futures_channel::oneshot;
use futures_util::future::select;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::time::Duration;
use worker::{Delay, DurableObject, Env, Method, Request, Response, Result, State, durable_object};

/// Scans that may run at once: one Container instance (`max_instances`
/// 1) with one session. Widening is a deployment decision.
const SLOTS: usize = 1;
/// One UTC day: the client and receipt keys' lifetime.
const DAY_MS: u64 = 86_400_000;
/// Internal request bodies are a few hundred bytes.
const BODY_BYTES: usize = 4 * 1024;

struct Keys {
    day: u64,
    client: [u8; 32],
    receipt: [u8; 32],
    used: BTreeSet<[u8; 16]>,
}

fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    cf::random_bytes(N as u32).copy_to(&mut out);
    out
}

impl Keys {
    fn fresh(day: u64) -> Keys {
        Keys { day, client: random(), receipt: random(), used: BTreeSet::new() }
    }

    /// Replaces both keys at the first call of a new UTC day.
    fn today(&mut self, now: u64) -> &mut Keys {
        let day = now / DAY_MS;
        if day != self.day {
            *self = Keys::fresh(day);
        }
        self
    }

    fn client_key(&self, prefix: &str) -> u64 {
        let Ok(mut m) = Hmac::<Sha256>::new_from_slice(&self.client) else { return 0 };
        m.update(b"slop_ninja_detect_client_v1\0");
        m.update(prefix.as_bytes());
        let d = m.finalize().into_bytes();
        let mut k = [0u8; 8];
        k.copy_from_slice(d.get(..8).unwrap_or(&[0u8; 8]));
        u64::from_le_bytes(k)
    }
}

#[durable_object]
pub struct DetectGate {
    env: Env,
    gate: RefCell<Gate>,
    keys: RefCell<Keys>,
    waiters: RefCell<Vec<oneshot::Sender<()>>>,
}

#[derive(Deserialize)]
struct EnterReq {
    prefix: String,
    hold_ms: u64,
}

#[derive(Deserialize)]
struct AskReq {
    ticket: String,
    hold_ms: u64,
    version: u64,
}

#[derive(Deserialize)]
struct DoneReq {
    ticket: String,
    #[serde(default)]
    served_ms: Option<u64>,
    #[serde(default)]
    band: Option<String>,
    #[serde(default)]
    length: Option<String>,
}

#[derive(Deserialize)]
struct FeedbackReq {
    receipt: String,
    claim: String,
}

#[derive(Serialize)]
struct StatusReply {
    ticket: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimate_ms: Option<u64>,
    version: u64,
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
    fn notify(&self) {
        for w in self.waiters.borrow_mut().drain(..) {
            let _ = w.send(());
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
        ok_json(&StatusReply {
            ticket: ticket_hex(ticket),
            status,
            position,
            estimate_ms: position.and_then(|p| g.estimate_ms(p)),
            version: g.version(),
            awake: g.awake_until() > now,
            tool: switch_on(&self.env, "DETECT_TOOL"),
        })
    }

    /// Asks the lane to start its instance (it answers once the start is
    /// requested; the load runs in its alarm).
    async fn start_lane(&self) -> bool {
        let Ok(ns) = self.env.durable_object(LANE) else { return false };
        let Ok(id) = ns.id_from_name("lane-0") else { return false };
        let Ok(stub) = id.get_stub() else { return false };
        let Ok(req) = cf_io::internal_request("https://lane/warm", Method::Post, None, Some(10_000)) else { return false };
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

impl DurableObject for DetectGate {
    fn new(state: State, env: Env) -> Self {
        silence_panics();
        let _ = state;
        let cfg = Config {
            slots: SLOTS,
            scan_lease_ms: ms_var(&env, "SCAN_LEASE_MS").unwrap_or(300_000),
            idle_ms: ms_var(&env, "IDLE_MS").unwrap_or(600_000),
            max_tickets: routes::MAX_TICKETS,
        };
        Self {
            env,
            gate: RefCell::new(Gate::new(cfg)),
            keys: RefCell::new(Keys::fresh(now_ms() / DAY_MS)),
            waiters: RefCell::new(Vec::new()),
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
                let Ok(e) = cf_io::request_json::<EnterReq>(&mut req, BODY_BYTES).await else { return status_code(400) };
                let client = self.keys.borrow_mut().today(now).client_key(&e.prefix);
                drop(e.prefix);
                let ticket = u128::from_le_bytes(random());
                let st = self.gate.borrow_mut().enter(ticket, client, now, e.hold_ms.saturating_mul(2));
                self.notify();
                match st {
                    Some(st) => self.reply(ticket, st),
                    None => ok_json(&StatusReply {
                        ticket: String::new(),
                        status: "busy",
                        position: None,
                        estimate_ms: None,
                        version: 0,
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
                let unchanged = self.gate.borrow().version() == a.version;
                if matches!(st, Status::Queued { .. }) && unchanged {
                    let (tx, rx) = oneshot::channel();
                    self.waiters.borrow_mut().push(tx);
                    let _ = select(Box::pin(rx), Box::pin(Delay::from(Duration::from_millis(a.hold_ms)))).await;
                    let now = now_ms();
                    let st = self.gate.borrow_mut().ask(ticket, now, lease);
                    self.notify_if_changed(a.version);
                    return self.reply(ticket, st);
                }
                self.notify_if_changed(a.version);
                self.reply(ticket, st)
            }
            "/done" => {
                let Ok(d) = cf_io::request_json::<DoneReq>(&mut req, BODY_BYTES).await else { return status_code(400) };
                let Some(ticket) = parse_ticket(&d.ticket) else { return status_code(400) };
                let known = self.gate.borrow_mut().done(ticket, now, d.served_ms);
                self.notify();
                let band = d.band.as_deref().and_then(Band::from_code);
                let length = d.length.as_deref().and_then(LengthBin::from_code);
                let receipt = match (known, d.served_ms, band, length) {
                    (true, Some(_), Some(b), Some(l)) => {
                        let id: [u8; 16] = random();
                        let mut keys = self.keys.borrow_mut();
                        receipt::issue(&keys.today(now).receipt, &id, b, l)
                    }
                    _ => None,
                };
                ok_json(&ReceiptReply { receipt })
            }
            "/leave" => {
                let Ok(d) = cf_io::request_json::<DoneReq>(&mut req, BODY_BYTES).await else { return status_code(400) };
                if let Some(ticket) = parse_ticket(&d.ticket) {
                    self.gate.borrow_mut().leave(ticket, now);
                    self.notify();
                }
                ok_json(&ReceiptReply { receipt: None })
            }
            "/warm" => {
                if !switch_on(&self.env, "DETECT_TOOL") {
                    let g = self.gate.borrow();
                    return ok_json(&WarmReply { start: false, awake_until: g.awake_until() });
                }
                let w = self.gate.borrow_mut().warm(now);
                if w.start && !self.start_lane().await {
                    self.gate.borrow_mut().asleep();
                }
                ok_json(&WarmReply { start: w.start, awake_until: w.awake_until })
            }
            "/feedback" => {
                let Ok(f) = cf_io::request_json::<FeedbackReq>(&mut req, BODY_BYTES).await else { return status_code(400) };
                if Claim::from_code(&f.claim).is_none() {
                    return status_code(400);
                }
                let mut keys = self.keys.borrow_mut();
                let k = keys.today(now);
                match receipt::verify(&k.receipt, &f.receipt) {
                    Some((id, band, length)) if k.used.insert(id) => {
                        ok_json(&FeedbackReply { band: band.code(), length: length.code() })
                    }
                    _ => status_code(409),
                }
            }
            _ => status_code(404),
        }
    }
}

impl DetectGate {
    fn notify_if_changed(&self, seen: u64) {
        if self.gate.borrow().version() != seen {
            self.notify();
        }
    }
}

#[derive(Serialize)]
struct ReceiptReply {
    #[serde(skip_serializing_if = "Option::is_none")]
    receipt: Option<String>,
}

#[derive(Serialize)]
struct WarmReply {
    start: bool,
    awake_until: u64,
}

#[derive(Serialize)]
struct FeedbackReply {
    band: &'static str,
    length: &'static str,
}

// The Worker reads the warm reply with this shape.
const _: fn(GateWarm) = |_| {};
