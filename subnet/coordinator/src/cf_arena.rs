//! The Workers exports on the bounded arena (wasm32 only).
//!
//! `ArenaAlloc` is the global allocator: every heap byte lives in a scope
//! arena, in the glue partition, or in the isolate's home scope
//! (`arena_routes::home`). Every export is hand-written (design §2.5) instead
//! of a worker-macros export, so that nothing allocates before the export
//! has admitted itself:
//!
//! 1. it takes a glue ticket (allocation-free); with every ticket in flight
//!    it answers a JS-only 503 (an alarm rejects so the platform retries it,
//!    a queue batch is retried, a cron tick resolves);
//! 2. it sets the arena clock and sweeps for abandoned scopes at this safe
//!    point, before any state changes. A poisoned pool is reported (health
//!    and the poisoned-sweep counter on `/status`) but not recycled: the
//!    recycle is a deliberate trap that aborts co-resident invocations, and
//!    it ships only with its durable recycle intent, step-crash attribution
//!    and per-cause stop rule (design section 2.6, not implemented yet). The
//!    abort hook still copies the postmortem out of an instance that dies;
//! 3. it opens the invocation's scope (`slop_ninja_arena::enter`) with the
//!    route's declared footprint, converts the request inside it, runs the
//!    handler, and turns the result into a JS `Response` before the scope
//!    closes. A scope the pool cannot admit is a JS-only 503.
//!
//! The Durable Object classes `ChainJournal` and `EvalCohort` are
//! hand-written wasm-bindgen classes holding an `Rc` handle to the object
//! (allocated in the home scope) and a resident token; each `fetch` and
//! `alarm` takes its own ticket. The fan-out lane objects in `cf_lanes.rs`
//! keep their worker-macros exports for now: their handlers run with no
//! scope current and their allocations spill out of the glue partition,
//! counted (`glue_spills`), until they are wrapped the same way. So every
//! hand-written export allocates in its scope; the lane objects and the
//! constructor fallbacks below spill or overdraw, counted.

use super::*;
use crate::arena_routes::{self, home};
use slop_ninja_arena::{ArenaAlloc, AsyncRoute, Refusal, RefusalKind, ScopeRef, glue};
use wasm_bindgen::prelude::wasm_bindgen;
use worker::worker_sys::{self, web_sys};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

// ------------------------------------------------------------- JS helpers

#[wasm_bindgen(inline_js = "
export function arena_pm_store(a, b, c, d, e, f, g, h, i, j, k, l, m, n) {
  globalThis.__arena_postmortem = [a, b, c, d, e, f, g, h, i, j, k, l, m, n];
}
export function arena_pm_word(i) {
  const p = globalThis.__arena_postmortem;
  return p ? p[i] : -1;
}
export function arena_route_class(req) {
  let u;
  try { u = new URL(req.url); } catch (e) { return 0; }
  const p = u.pathname;
  if (p === '/hook') return 1;
  if (p === '/status') return 3;
  if (p === '/fault/clear' || p.startsWith('/signing/')) return 4;
  if (p.startsWith('/eval/')) return 5;
  if (p.startsWith('/internal/admission/') && u.host === 'eval-admission.internal') return 9;
  return 0;
}
export function arena_journal_class(req) {
  let p = '';
  try { p = new URL(req.url).pathname; } catch (e) { return 0; }
  if (p === '/sign-auto') return 1;
  return 0;
}
export function arena_cohort_class(req) {
  let p = '';
  try { p = new URL(req.url).pathname; } catch (e) { return 0; }
  if (p.startsWith('/commitments/')) return 1;
  if (p.startsWith('/admission-work/')) return 2;
  return 0;
}
export function arena_hook_shed(req, env) {
  let sub = undefined;
  try { sub = env.RTAO_WEBHOOK_SUBSCRIPTION_ID; } catch (e) { return false; }
  if (typeof sub !== 'string' || sub.trim() === '') return false;
  const h = req.headers.get('x-rtao-signature');
  if (h === null || h.length > 4096) return true;
  return !h.toLowerCase().includes(sub.trim().toLowerCase());
}
")]
extern "C" {
    #[allow(clippy::too_many_arguments)]
    fn arena_pm_store(a: u32, b: u32, c: u32, d: u32, e: u32, f: u32, g: u32, h: u32, i: u32, j: u32, k: u32, l: u32, m: u32, n: u32);
    fn arena_pm_word(i: u32) -> f64;
    /// The Worker route of a request, read in JS (no Rust allocation).
    fn arena_route_class(req: &web_sys::Request) -> u32;
    fn arena_cohort_class(req: &web_sys::Request) -> u32;
    /// The ChainJournal route of a request: the automatic-handoff pass has
    /// its own derived footprint (`arena_routes::JOURNAL_AUTO_BYTES`).
    fn arena_journal_class(req: &web_sys::Request) -> u32;
    /// Load shedding for `/hook`, in JS before any ticket, scope or Rust
    /// allocation: `true` when the request carries no signature header (or
    /// one longer than `webhook::MAX_SIGNATURE_HEADER_BYTES`) naming this
    /// receiver's subscription id, so it can never authenticate. Only a
    /// shed: the Rust receiver still checks every field
    /// (`webhook::precheck`) and the HMAC over the body
    /// (`webhook::authenticate`). With the subscription unconfigured nothing
    /// is shed, so the receiver answers `not_configured`.
    fn arena_hook_shed(req: &web_sys::Request, env: &Env) -> bool;
}

struct Static<T>(core::cell::UnsafeCell<T>);
// SAFETY: wasm32 without atomics is single-threaded.
unsafe impl<T> Sync for Static<T> {}

/// worker's own abort handler (it schedules the re-instantiation), chained.
static PREV_ABORT: Static<Option<fn()>> = Static(core::cell::UnsafeCell::new(None));
static HOOKED: Static<bool> = Static(core::cell::UnsafeCell::new(false));

/// Runs in a dying instance: copies the postmortem out as scalars
/// (allocation-free), then hands over to worker's re-instantiation.
fn arena_on_abort() {
    let w = slop_ninja_arena::postmortem().words();
    arena_pm_store(w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9], w[10], w[11], w[12], w[13]);
    // SAFETY: single-threaded; set once in `install_abort_hook`.
    if let Some(prev) = unsafe { *PREV_ABORT.0.get() } {
        prev();
    }
}

/// Installs the abort hook once per instance, chained to worker's.
fn install_abort_hook() {
    // SAFETY: single-threaded.
    unsafe {
        if !*HOOKED.0.get() {
            *PREV_ABORT.0.get() = wasm_bindgen::handler::set_on_abort(arena_on_abort);
            *HOOKED.0.get() = true;
        }
    }
}

/// Every export's safe point: the clock and the abandonment sweep, before
/// any state changes. A poisoned pool is counted (`poisoned_sweeps`) and
/// reported, never recycled here: `slop_ninja_arena::recycle` traps the
/// instance and aborts co-resident invocations, so it waits for the durable
/// recycle intent and stop rule it must ship with (module docs).
fn entry_check() {
    install_abort_hook();
    let _ = slop_ninja_arena::tick(now_ms());
}

/// A JS response from static text: JS copies it out of linear memory, so it
/// holds no Rust heap and needs no scope.
fn js_text(text: &str, status: u16) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    if let Ok(h) = web_sys::Headers::new() {
        let _ = h.set("Cache-Control", "no-store");
        init.set_headers(&h);
    }
    match web_sys::Response::new_with_opt_str_and_init(Some(text), &init) {
        Ok(r) => r,
        // Only an invalid status can fail, and every status here is valid.
        Err(_) => web_sys::Response::error(),
    }
}

/// Every glue ticket is in flight.
fn busy() -> js_sys::Promise {
    let v: JsValue = js_text("arena_busy", 503).into();
    js_sys::Promise::resolve(&v)
}

/// The scope could not be admitted: 503 for any kind (a refused request is
/// retried by its caller: the provider redelivers, callers retry), with the
/// refusal's static code.
fn refused(r: Refusal) -> web_sys::Response {
    let code = match r.kind {
        RefusalKind::Retry => "arena_retry",
        RefusalKind::Capacity => "arena_capacity",
        RefusalKind::Poisoned => "arena_poisoned",
        RefusalKind::Invalid => "arena_invalid",
        RefusalKind::Defect => "arena_defect",
    };
    js_text(code, 503)
}

/// A handler's result as a JS response, inside the scope (the body is copied
/// into JS). Errors answer 500 as the worker-macros export did.
fn to_js(result: Result<Response>) -> web_sys::Response {
    match result {
        Ok(res) => match worker::IntoResponse::into_raw(res) {
            Ok(raw) => raw,
            Err(_) => js_text("INTERNAL SERVER ERROR", 500),
        },
        Err(e) => {
            worker::console_error!("{}", e);
            js_text("INTERNAL SERVER ERROR", 500)
        }
    }
}

/// Runs one invocation in its own scope.
async fn in_scope<F>(route: AsyncRoute, control: usize, make: impl FnOnce(ScopeRef) -> F) -> web_sys::Response
where
    F: core::future::Future<Output = web_sys::Response>,
{
    match slop_ninja_arena::enter(route, control, make) {
        Ok(e) => e.await.0,
        Err(r) => refused(r),
    }
}

/// The arena counters and the previous instance's postmortem (if it died)
/// as one JSON object, for the status routes. Written into a fixed buffer
/// first, so the snapshot is taken before this call allocates.
pub(super) fn arena_status() -> serde_json::Value {
    let st = slop_ninja_arena::stats();
    let mut buf = [0u8; 8192];
    let mut w = slop_ninja_arena::io::FmtBuf::new(&mut buf);
    let _ = st.write_json(&mut w);
    let mut v: serde_json::Value = serde_json::from_str(w.as_str()).unwrap_or(serde_json::Value::Null);
    if let Some(map) = v.as_object_mut() {
        let pm: Vec<f64> = (0..slop_ninja_arena::POSTMORTEM_WORDS as u32).map(arena_pm_word).collect();
        map.insert("prev_postmortem".into(), serde_json::json!(pm));
        let homes: serde_json::Map<String, serde_json::Value> = home::ALL
            .iter()
            .filter_map(|&h| {
                home::stats(h).map(|st| {
                    (
                        h.route().name.to_owned(),
                        serde_json::json!({"reserved_pages": st.reserved_pages, "used_pages": st.used_pages, "peak_pages": st.peak_pages, "live": st.live, "overdraft_pages": st.overdraft_pages}),
                    )
                })
            })
            .collect();
        map.insert("homes".into(), serde_json::Value::Object(homes));
        let failures: serde_json::Map<String, serde_json::Value> =
            home::ALL.iter().map(|&h| (h.route().name.to_owned(), serde_json::json!(home::open_failures(h)))).collect();
        map.insert("home_open_failures".into(), serde_json::Value::Object(failures));
        // A content-free fault an operator alert can key on: a scope that
        // stopped being polled past its deadline (its pages, and for a
        // continuation its glue ticket, stay held until the instance is
        // evicted), or a pool whose pinned pages passed the quarantine budget.
        let fault = if !st.healthy {
            Some("arena_poisoned")
        } else if st.abandoned_scopes > 0 {
            Some("arena_abandoned_scopes")
        } else {
            None
        };
        map.insert("fault".into(), serde_json::json!(fault));
        let routes: serde_json::Map<String, serde_json::Value> = ROUTE_IDS
            .iter()
            .map(|&id| {
                let r = slop_ninja_arena::route_stats(id);
                (
                    r.name.to_owned(),
                    serde_json::json!({
                        "opened": r.opened, "peak_pages": r.peak_pages, "max_reserved_bytes": r.max_reserved_bytes,
                        "overdrafts": r.overdrafts, "refusals_retry": r.refusals_retry,
                        "refusals_capacity": r.refusals_capacity, "refusals_poisoned": r.refusals_poisoned,
                        "quarantined": r.quarantined, "abandoned": r.abandoned, "alloc_failures": r.alloc_failures,
                    }),
                )
            })
            .filter(|(name, _)| !name.is_empty())
            .collect();
        map.insert("routes".into(), serde_json::Value::Object(routes));
    }
    v
}

const ROUTE_IDS: [u8; 27] = {
    use arena_routes::id::*;
    [
        HOOK, HOOK_CONTINUATION, STATUS, OPERATOR, EVAL, OTHER, SCHEDULED, QUEUE, QUEUE_ADMISSION, ADMISSION, JOURNAL_NEW, JOURNAL_FETCH, JOURNAL_AUTO, COHORT_NEW, COHORT_FETCH,
        COHORT_ALARM, COHORT_SEAL, COHORT_DRAW, COHORT_PACKAGE, COHORT_CLOSE, COHORT_LOAD_SEAL, COHORT_COMMITMENTS, COHORT_ADMISSION, COHORT_WORK, HOME_STATE,
        HOME_RUNTIMES, HOME_PLANS,
    ]
};

// ------------------------------------------------------------ Worker exports

/// The Worker fetch export. A `/hook` request that can never authenticate
/// (no signature header naming this receiver's subscription) is refused in
/// JS before it takes a ticket or a scope, so unauthenticated clients that
/// hold requests open cannot occupy the tickets and pages deliveries and the
/// watchdog need.
#[wasm_bindgen]
pub fn fetch(req: web_sys::Request, env: Env, ctx: worker_sys::Context) -> js_sys::Promise {
    let class = arena_route_class(&req);
    if class == 1 && arena_hook_shed(&req, &env) {
        let v: JsValue = js_text("unauthenticated", 401).into();
        return js_sys::Promise::resolve(&v);
    }
    let Ok(ticket) = glue::Ticket::admit() else { return busy() };
    entry_check();
    let (route, control) = match class {
        1 => (arena_routes::R_HOOK, arena_routes::HOOK_CONTROL_BYTES),
        3 => (arena_routes::R_STATUS, arena_routes::WORKER_CONTROL_BYTES),
        4 => (arena_routes::R_OPERATOR, arena_routes::WORKER_CONTROL_BYTES),
        5 => (arena_routes::R_EVAL, arena_routes::WORKER_CONTROL_BYTES),
        9 => (arena_routes::R_ADMISSION, arena_routes::WORKER_CONTROL_BYTES),
        _ => (arena_routes::R_OTHER, arena_routes::WORKER_CONTROL_BYTES),
    };
    wasm_bindgen_futures::future_to_promise(async move {
        let _ticket = ticket;
        let res = in_scope(route, control, move |scope| async move {
            let ctx = Context::new(ctx);
            to_js(super::fetch(Request::from(req), env, ctx, &scope).await)
        })
        .await;
        Ok(res.into())
    })
}

/// The cron export: refused (no ticket, or no scope), the tick resolves and
/// the next minute's tick does the work.
#[wasm_bindgen]
pub fn scheduled(_event: worker_sys::ScheduledEvent, env: Env, _ctx: worker_sys::ScheduleContext) -> js_sys::Promise {
    let Ok(ticket) = glue::Ticket::admit() else { return js_sys::Promise::resolve(&JsValue::UNDEFINED) };
    entry_check();
    wasm_bindgen_futures::future_to_promise(async move {
        let _ticket = ticket;
        if let Ok(e) = slop_ninja_arena::enter(arena_routes::R_SCHEDULED, arena_routes::CONTINUATION_CONTROL_BYTES, move |_s| async move {
            super::scheduled(env).await;
        }) {
            let _ = e.await;
        }
        Ok(JsValue::UNDEFINED)
    })
}

/// The lane-wakeup queue export: refused, or on a handler error, the batch is
/// retried (allocation-free) and the export resolves; it never rejects or
/// panics.
#[wasm_bindgen]
pub fn queue(batch: worker_sys::MessageBatch, env: Env, _ctx: worker_sys::Context) -> js_sys::Promise {
    let Ok(ticket) = glue::Ticket::admit() else {
        let _ = batch.retry_all(JsValue::UNDEFINED);
        return js_sys::Promise::resolve(&JsValue::UNDEFINED);
    };
    entry_check();
    let retry = batch.clone();
    wasm_bindgen_futures::future_to_promise(async move {
        let _ticket = ticket;
        let ok = match slop_ninja_arena::enter(arena_routes::R_QUEUE, arena_routes::QUEUE_CONTROL_BYTES, move |s| async move {
            super::queue(batch, env, s).await.is_ok()
        }) {
            Ok(e) => e.await.0,
            Err(_) => false,
        };
        if !ok {
            let _ = retry.retry_all(JsValue::UNDEFINED);
        }
        Ok(JsValue::UNDEFINED)
    })
}

// ------------------------------------------------------- Durable Objects

/// Builds a Durable Object's state in the home scope (it outlives every
/// invocation), else, when the home scope cannot be admitted, with no scope
/// current (glue, counted). The fallback is deliberate: a constructor runs
/// once per instance, so refusing it would leave the object unusable for its
/// whole life (design section 2.5, constructor row).
fn construct<T>(make: impl FnOnce() -> T) -> Rc<T> {
    match home::enter(home::Home::State, move || Rc::new(make())) {
        Ok(rc) => rc,
        Err(build) => build(),
    }
}

/// The ChainJournal class (see `super::ChainJournal`).
#[wasm_bindgen(js_name = ChainJournal)]
pub struct ChainJournalExport {
    inner: Rc<super::ChainJournal>,
    _resident: glue::Resident,
}

#[wasm_bindgen(js_class = ChainJournal)]
impl ChainJournalExport {
    /// Past the resident count the constructor throws (a JS string) and the
    /// caller retries.
    #[wasm_bindgen(constructor)]
    pub fn new(state: worker_sys::DurableObjectState, env: Env) -> core::result::Result<ChainJournalExport, JsValue> {
        install_abort_hook();
        let resident = glue::Resident::admit().map_err(|_| JsValue::from_str("arena: resident limit"))?;
        let inner = construct(move || <super::ChainJournal as DurableObject>::new(State::from(state), env));
        Ok(ChainJournalExport { inner, _resident: resident })
    }

    #[wasm_bindgen(js_name = fetch)]
    pub fn fetch(&self, req: web_sys::Request) -> js_sys::Promise {
        let Ok(ticket) = glue::Ticket::admit() else { return busy() };
        entry_check();
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let _ticket = ticket;
            let (route, control) = match arena_journal_class(&req) {
                1 => (arena_routes::R_JOURNAL_AUTO, arena_routes::JOURNAL_AUTO_BYTES),
                _ => (arena_routes::R_JOURNAL_FETCH, arena_routes::JOURNAL_CONTROL_BYTES),
            };
            let res = in_scope(route, control, move |_s| async move {
                to_js(<super::ChainJournal as DurableObject>::fetch(&inner, Request::from(req)).await)
            })
            .await;
            Ok(res.into())
        })
    }
}

/// The EvalCohort class (see `super::eval_do::EvalCohort`).
#[wasm_bindgen(js_name = EvalCohort)]
pub struct EvalCohortExport {
    inner: Rc<super::eval_do::EvalCohort>,
    _resident: glue::Resident,
}

#[wasm_bindgen(js_class = EvalCohort)]
impl EvalCohortExport {
    #[wasm_bindgen(constructor)]
    pub fn new(state: worker_sys::DurableObjectState, env: Env) -> core::result::Result<EvalCohortExport, JsValue> {
        install_abort_hook();
        let resident = glue::Resident::admit().map_err(|_| JsValue::from_str("arena: resident limit"))?;
        let inner = construct(move || <super::eval_do::EvalCohort as DurableObject>::new(State::from(state), env));
        Ok(EvalCohortExport { inner, _resident: resident })
    }

    #[wasm_bindgen(js_name = fetch)]
    pub fn fetch(&self, req: web_sys::Request) -> js_sys::Promise {
        let Ok(ticket) = glue::Ticket::admit() else { return busy() };
        entry_check();
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let _ticket = ticket;
            let (route, control) = match arena_cohort_class(&req) {
                1 => (arena_routes::R_COHORT_COMMITMENTS, arena_routes::COMMITMENTS_CONTROL_BYTES),
                2 => (arena_routes::R_COHORT_WORK, arena_routes::cohort_work_bytes()),
                _ => (arena_routes::R_COHORT_FETCH, arena_routes::COHORT_CONTROL_BYTES),
            };
            let res = in_scope(route, control, move |_s| async move {
                to_js(<super::eval_do::EvalCohort as DurableObject>::fetch(&inner, Request::from(req)).await)
            })
            .await;
            Ok(res.into())
        })
    }

    /// Past the tickets, when the alarm's scope cannot be admitted, or when
    /// the alarm fails (a re-arm that did not persist), the alarm rejects so
    /// the platform retries it, as the worker-macros export did.
    #[wasm_bindgen(js_name = alarm)]
    pub fn alarm(&self) -> js_sys::Promise {
        let Ok(ticket) = glue::Ticket::admit() else {
            return js_sys::Promise::reject(&JsValue::from_str("arena_busy"));
        };
        entry_check();
        let inner = self.inner.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let _ticket = ticket;
            // The handler's result becomes a JS value (a response, or a static
            // rejection code) inside its scope.
            match slop_ninja_arena::enter(arena_routes::R_COHORT_ALARM, arena_routes::ALARM_CONTROL_BYTES, move |scope| async move {
                match inner.alarm_in(&scope).await {
                    Ok(res) => match worker::IntoResponse::into_raw(res) {
                        Ok(raw) => Ok(JsValue::from(raw)),
                        Err(_) => Err("alarm_response"),
                    },
                    Err(e) => {
                        worker::console_error!("{}", e);
                        Err("alarm_failed")
                    }
                }
            }) {
                Ok(e) => match e.await.0 {
                    Ok(v) => Ok(v),
                    Err(code) => Err(JsValue::from_str(code)),
                },
                Err(_) => Err(JsValue::from_str("arena_refused")),
            }
        })
    }
}
