//! Generation v2 on the eval Worker (wasm32 only): the generation Worker's
//! interface client, Workers AI runs of v2 requests, and the operator's
//! measurement calls (design section 12.3).
//!
//! - [`health`]: the generation Worker's `/health` (whether each key
//!   resolves), through the service binding `GEN_TESTNET`.
//! - [`frontier`]: one frontier call through the service binding. The
//!   message is checked against the interface's bounds before it is sent,
//!   and the reply is read within its request's reply bound. A failure of
//!   the interface itself is classified by `gen_measure::interface_failure`:
//!   a 409 is an interface version skew (refused before any vendor call),
//!   another status a refusal before sending, no reply (a timeout, an
//!   unreadable reply) a sent call charged at its reservation bound.
//! - [`workers_ai`]: one Workers AI run of a v2 request body (the model's
//!   own reasoning setting included), raced against its timeout in JS.
//! - [`measure`]: one call of the compiled synthetic measurement set
//!   (`slop_ninja_gen_core::measure`): the route takes a manifest binding, a
//!   cell and a call index, never text, so no pool, held-out or customer
//!   text can reach a vendor through it. Its output is synthetic and goes
//!   back to the operator only; its spend is charged to the ledger under
//!   one key per vendor call (`gen_measure::ledger_key`).
//!
//! Everything these decide without the platform lives in
//! `eval::gen_measure` and `slop_ninja_gen_core` (tested natively).
//!
//! The generation lanes call [`frontier`] and [`workers_ai`] for v2 items
//! (`cf_lanes::gen_call`); the draw reads the bound objects itself. Plans
//! naming generation v2 are still refused at validation
//! (`plan::GENERATION_V2_REFUSAL`), so no cohort reaches them yet.

use super::cf_io::{self, Settled};
use crate::eval::gen_measure::{self, MeasureCall};
use crate::eval::plan::GEN_V2_LANE_BOUNDS;
use crate::eval::textgen;
use serde_json::{Value, json};
use slop_ninja_gen_core::iface;
use slop_ninja_gen_core::manifest::{Manifest, ObjectRef};
use slop_ninja_gen_core::measure;
use slop_ninja_gen_core::outcome::{self, GenCall, GenReply, OutcomeCode};
use slop_ninja_gen_core::plan::Health;
use slop_ninja_gen_core::request::VendorRequest;
use slop_ninja_gen_core::vendor::Lane;
use slop_ninja_gen_core::voice::Library;
use wasm_bindgen::{JsCast, JsValue};
use worker::{Env, Method};

/// The service binding to the generation Worker (eval Worker only).
pub const GEN_SERVICE: &str = "GEN_TESTNET";

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

/// An interface request with the marker header, under an
/// `AbortSignal.timeout` the platform enforces on the call and its reply.
fn interface_request(path: &str, method: Method, body: Option<&str>, timeout_ms: u64) -> Option<worker::Request> {
    let req = cf_io::internal_request(&format!("{}{path}", iface::ORIGIN), method, body, Some(timeout_ms)).ok()?;
    req.headers().set(iface::MARKER_HEADER, iface::MARKER_VALUE).ok()?;
    if body.is_some() {
        req.headers().set("Content-Type", "application/json").ok()?;
    }
    Some(req)
}

/// The generation Worker's key health; `None` when it did not answer (no
/// binding, not deployed, a fault). Bounded by the lanes' commit margin.
pub async fn health(env: &Env) -> Option<Health> {
    let svc = env.service(GEN_SERVICE).ok()?;
    let req = interface_request(iface::HEALTH_PATH, Method::Get, None, GEN_V2_LANE_BOUNDS.commit_margin_ms)?;
    let mut r = svc.fetch_request(req).await.ok()?;
    if r.status_code() != 200 {
        cf_io::discard(&mut r);
        return None;
    }
    cf_io::response_json::<Health>(&mut r, iface::PAGE).await.ok()
}

/// One frontier call through the generation Worker.
pub async fn frontier(env: &Env, call: &GenCall) -> GenReply {
    let t0 = now_ms();
    let r = &call.request;
    let since = |t0: u64| now_ms().saturating_sub(t0);
    // Before sending: nothing reached the generation Worker.
    let unsent = |t0: u64| GenReply::without_answer(OutcomeCode::Outage, r, since(t0));
    if !iface::within(r) {
        return GenReply::without_answer(OutcomeCode::CellRefused, r, 0);
    }
    let Ok(body) = serde_json::to_string(call) else { return unsent(t0) };
    if body.len() > iface::CALL_MESSAGE_BYTES {
        return GenReply::without_answer(OutcomeCode::CellRefused, r, 0);
    }
    let Ok(svc) = env.service(GEN_SERVICE) else { return unsent(t0) };
    // The call's own timeout bounds the whole service call, so the reply
    // and the commit fit the rest of the claim lease (`GEN_REPORT_MS`)
    // and no other lane can re-claim the item while its reply is in
    // flight. The generation Worker's own vendor deadline is the timeout
    // less the reply's allowance (`iface::vendor_deadline_ms`), so a vendor
    // call that runs to its deadline still answers with its outcome and
    // usage; only a reply later than the allowance ends here as an outage
    // charged at its reservation bound.
    let timeout = call.timeout_ms;
    let Some(req) = interface_request(iface::GENERATE_PATH, Method::Post, Some(&body), timeout) else { return unsent(t0) };
    drop(body);
    let Ok(mut resp) = svc.fetch_request(req).await else { return gen_measure::interface_failure(None, r, since(t0)) };
    if resp.status_code() != 200 {
        let status = resp.status_code();
        cf_io::discard(&mut resp);
        return gen_measure::interface_failure(Some(status), r, since(t0));
    }
    match cf_io::response_json::<GenReply>(&mut resp, iface::reply_bound(r)).await {
        Ok(g) => g,
        Err(_) => gen_measure::interface_failure(None, r, since(t0)),
    }
}

/// One Workers AI run of `r` (a v2 request body), raced against
/// `timeout_ms` in JS so the awaited promise always settles.
pub async fn workers_ai(env: &Env, r: &VendorRequest, timeout_ms: u64) -> GenReply {
    let t0 = now_ms();
    let deadline = t0.saturating_add(timeout_ms);
    let since = |t0: u64| now_ms().saturating_sub(t0);
    let reply: Result<Value, String> = async {
        let ai = js_sys::Reflect::get(env.as_ref(), &JsValue::from_str("AI"))
            .ok()
            .filter(|v| !v.is_undefined())
            .ok_or_else(|| "ai binding".to_owned())?;
        let run: js_sys::Function = js_sys::Reflect::get(&ai, &JsValue::from_str("run"))
            .ok()
            .and_then(|f| f.dyn_into().ok())
            .ok_or_else(|| "ai binding".to_owned())?;
        let input = js_sys::JSON::parse(&r.body).map_err(|_| "ai input".to_owned())?;
        let promise: js_sys::Promise = match run.call2(&ai, &JsValue::from_str(&r.model), &input) {
            Ok(p) => p.dyn_into().map_err(|_| "ai run".to_owned())?,
            Err(e) => return Err(worker::Error::from(e).to_string()),
        };
        match cf_io::settle(promise, Some(deadline)).await {
            Settled::Value(v) => {
                let text: String = js_sys::JSON::stringify(&v).map(String::from).map_err(|_| "ai reply".to_owned())?;
                serde_json::from_str::<Value>(&text).map_err(|_| "ai reply".to_owned())
            }
            Settled::Error(e) => Err(worker::Error::from(e).to_string()),
            Settled::Timeout => Err("timeout".to_owned()),
        }
    }
    .await;
    match reply {
        Ok(v) => outcome::workers_ai_reply(&v, r, since(t0)),
        Err(e) => {
            // The binding exposes text only (`textgen::classify_ai_error`).
            let outcome = match textgen::classify_ai_error(&e) {
                textgen::Reject::Throttled => OutcomeCode::Throttle,
                textgen::Reject::ModelUnavailable => OutcomeCode::CellRefused,
                _ => OutcomeCode::Outage,
            };
            let mut g = GenReply::without_answer(outcome, r, since(t0));
            // An outage after the run started (a timeout, a failed or
            // unreadable reply) may have been billed: charged at its
            // reservation bound. A run that never started was not.
            g.sent = outcome == OutcomeCode::Outage && !matches!(e.as_str(), "ai binding" | "ai input" | "ai run");
            g
        }
    }
}

/// The longest a measured call may take and still fit a frontier lease:
/// the alarm wall less one lease step (the plan's lease ceiling), less the
/// commit margin. A slower cell can never be used (the measured manifest
/// marks it unusable), so waiting longer measures nothing.
pub const MEASURE_TIMEOUT_MS: u64 = GEN_V2_LANE_BOUNDS.call_timeout_ceiling_ms();

/// An R2 object of the eval bucket, at most its binding's declared bytes
/// (refused before it is read), whose digest is its binding's.
async fn bound_object(env: &Env, r: &ObjectRef) -> Result<Vec<u8>, &'static str> {
    let bucket = env.bucket(super::eval_do::EVAL_BUCKET).map_err(|_| "eval bucket")?;
    let deadline = now_ms().saturating_add(GEN_V2_LANE_BOUNDS.commit_margin_ms);
    let bound = usize::try_from(r.bytes).map_err(|_| "object size")?;
    let bytes = cf_io::r2_bytes(&bucket, &r.key, bound, Some(deadline))
        .await
        .map_err(|_| "object read")?
        .ok_or("object missing")?;
    if !r.matches(&bytes) {
        return Err("object digest");
    }
    Ok(bytes)
}

/// One measurement call: `{manifest: {key, sha256, bytes}, model, effort,
/// call, attempt}` and nothing else. Returns the operator's answer, the
/// call's charge (micro-USD) and its ledger key (`None`: nothing was called).
pub async fn measure(env: &Env, body: &Value) -> (Value, u64, Option<String>) {
    let fail = |e: &str| (json!({"error": e}), 0, None);
    let (c, effort) = match MeasureCall::parse(body) {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    let manifest = match bound_object(env, &c.manifest).await {
        Ok(b) => match Manifest::parse_bound(&b, &c.manifest) {
            Ok(m) => m,
            Err(e) => return fail(e.0),
        },
        Err(e) => return fail(e),
    };
    let lref = ObjectRef { key: manifest.voice_library.key.clone(), sha256: manifest.voice_library.sha256.clone(), bytes: manifest.voice_library.bytes };
    let library = match bound_object(env, &lref).await {
        Ok(b) => match Library::parse_bound(&b, &manifest.voice_library) {
            Ok(l) => l,
            Err(e) => return fail(e.0),
        },
        Err(e) => return fail(e),
    };
    let Some(entry) = manifest.model(&c.model) else { return fail("model") };
    let (request, source) = match measure::request(&manifest, &library, &c.model, effort, c.call) {
        Ok(x) => x,
        Err(e) => return fail(e.0),
    };
    let t_call = now_ms();
    let reply = match entry.lane {
        Lane::Frontier => {
            let mut accepted_models = vec![entry.id.clone()];
            accepted_models.extend(entry.accepted_aliases.iter().cloned());
            frontier(env, &GenCall { request: request.clone(), accepted_models, timeout_ms: MEASURE_TIMEOUT_MS }).await
        }
        Lane::WorkersAi => workers_ai(env, &request, MEASURE_TIMEOUT_MS).await,
    };
    let wall_ms = now_ms().saturating_sub(t_call);
    let mut out = gen_measure::assemble(&manifest, entry, effort, &c, &request, &source, &reply, MEASURE_TIMEOUT_MS);
    if entry.lane == Lane::Frontier
        && let Some(a) = out.answer.as_object_mut()
    {
        // The service call's overhead beyond the vendor call, the measure
        // of `iface::REPLY_ALLOWANCE_MS`.
        a.insert("service_ms".into(), json!(wall_ms.saturating_sub(reply.latency_ms)));
    }
    (out.answer, out.microusd, Some(out.ledger_key))
}
