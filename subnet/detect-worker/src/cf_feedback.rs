//! `DetectFeedback` (wasm32 only): the feedback receipts' ledger
//! ([`receipt::Ledger`]), in memory only. It stores nothing, so a restart
//! forgets its key and older receipts stop verifying, which only loses votes.
//!
//! The feedback route needs no challenge, so its traffic has its own object
//! and never reaches the admission gate: a flood of junk or replayed receipts
//! loads this object alone, and a scan only waits a bounded time for its
//! receipt (it answers without one when this object is slow).
//!
//! Internal routes, called only by the detect Worker:
//! - `/issue {band, length}`: a receipt for one scored result;
//! - `/vote {receipt, claim}`: accepts an unused valid receipt once and
//!   answers its band and length bin; anything else gets 409.
//!
//! No route receives or answers any text.

use crate::cf;
use crate::cf_io::{self, now_ms};
use crate::policy::{Band, LengthBin};
use crate::receipt::{Claim, Ledger};
use crate::{routes, silence_panics};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use worker::{DurableObject, Env, Method, Request, Response, Result, State, durable_object};

/// One UTC day: a key's longest life.
const DAY_MS: u64 = 86_400_000;
/// Internal request bodies are a few hundred bytes.
const BODY_BYTES: usize = 4 * 1024;

fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    cf::random_bytes(N as u32).copy_to(&mut out);
    out
}

#[durable_object]
pub struct DetectFeedback {
    ledger: RefCell<Ledger>,
}

#[derive(Deserialize)]
struct IssueReq {
    band: String,
    length: String,
}

#[derive(Deserialize)]
struct VoteReq {
    receipt: String,
    claim: String,
}

#[derive(Serialize)]
struct IssueReply {
    #[serde(skip_serializing_if = "Option::is_none")]
    receipt: Option<String>,
}

#[derive(Serialize)]
struct VoteReply {
    band: &'static str,
    length: &'static str,
}

fn ok_json<T: Serialize>(v: &T) -> Result<Response> {
    let mut r = Response::from_bytes(serde_json::to_vec(v).unwrap_or_default())?;
    r.headers_mut().set("Content-Type", "application/json")?;
    Ok(r)
}

fn status_code(code: u16) -> Result<Response> {
    Ok(Response::empty()?.with_status(code))
}

impl DetectFeedback {
    /// Today's ledger: a new UTC day or a full used-id set starts it over.
    fn today(&self) -> std::cell::RefMut<'_, Ledger> {
        let day = now_ms() / DAY_MS;
        let mut l = self.ledger.borrow_mut();
        if l.stale(day) {
            l.renew(day, random());
        }
        l
    }
}

impl DurableObject for DetectFeedback {
    fn new(state: State, env: Env) -> Self {
        silence_panics();
        let _ = (state, env);
        Self { ledger: RefCell::new(Ledger::new(now_ms() / DAY_MS, random(), routes::MAX_USED_IDS)) }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        silence_panics();
        if req.method() != Method::Post {
            return status_code(405);
        }
        match req.path().as_str() {
            "/issue" => {
                let Ok(i) = cf_io::request_json::<IssueReq>(&mut req, BODY_BYTES).await else { return status_code(400) };
                let (Some(band), Some(length)) = (Band::from_code(&i.band), LengthBin::from_code(&i.length)) else {
                    return status_code(400);
                };
                let id: [u8; 16] = random();
                let receipt = self.today().issue(&id, band, length);
                ok_json(&IssueReply { receipt })
            }
            "/vote" => {
                let Ok(v) = cf_io::request_json::<VoteReq>(&mut req, BODY_BYTES).await else { return status_code(400) };
                if Claim::from_code(&v.claim).is_none() {
                    return status_code(400);
                }
                match self.today().vote(&v.receipt) {
                    Some((band, length)) => ok_json(&VoteReply { band: band.code(), length: length.code() }),
                    None => status_code(409),
                }
            }
            _ => status_code(404),
        }
    }
}
