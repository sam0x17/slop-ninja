//! workers-rs 0.8.7 entry points under the arena allocator, for measuring
//! platform glue in local workerd (design §2.2 and §5.1, migration step 0).
//!
//! Every handler body runs inside `arena::enter`, so whatever the arena
//! records as glue is what workers-rs, worker-macros, js-sys and
//! wasm-bindgen allocate around a handler: the exported future and its
//! js-sys `Task`, the `worker::Request` conversion, the queue batch wrapper,
//! each Durable Object instance's `Rc<WasmRefCell<T>>`.
//!
//! Routes (Worker fetch): `/stats`, `/reset`, `/hold?ms=`, `/do/hold?ms=&name=`,
//! `/do/alarm?ms=&name=`, `/do/ping?name=`, `/wait?ms=` (a `waitUntil`
//! continuation), `/trap`, `/instance`. The queue and scheduled handlers hold
//! for the milliseconds in the message body or for `HOLD_MS`. Every hold is
//! at most `MAX_HOLD_MS`, the bound the routes' deadlines are derived from.
//!
//! With the `admitted` feature every export is hand-written (design §2.5):
//! the Worker `fetch`, `queue` and `scheduled` functions and the Durable
//! Object class (constructor, `fetch`, `alarm`). Each takes its own glue
//! ticket before anything allocates (the constructor a resident token), so a
//! Worker-to-Durable-Object hop takes one ticket per entry and no ticket ever
//! covers a call chain; answers a JS-only refusal when none is free; recycles
//! a poisoned instance at that safe point; and converts its input inside the
//! handler's scope. Each export also installs the arena's abort hook, which
//! copies the postmortem out of a dying instance into a JS global before
//! worker's own re-instantiation runs; the next instance reports it.
#![cfg(target_arch = "wasm32")]
// The wasm32 stack is linear memory outside every arena: frames, futures and
// stack arrays stay below the measured bound (clippy.toml, design §5.1).
#![warn(clippy::large_stack_frames, clippy::large_futures, clippy::large_stack_arrays)]

use core::cell::UnsafeCell;
use core::fmt::Write as _;
use core::time::Duration;

use slop_ninja_arena::io::FmtBuf;
use slop_ninja_arena::{ArenaAlloc, AsyncRoute, PAGE, child_bytes, limits, stats};
use worker::wasm_bindgen::JsValue;
use worker::worker_sys::web_sys;
use worker::*;

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

/// The longest hold any probe route accepts (the driver's longest is
/// 1,500 ms); a longer `ms` is refused with 400, so every await below is
/// bounded by it.
const MAX_HOLD_MS: u64 = 2_000;
const HOLD_MS: u64 = 400;
/// Worker fetch: two sequential awaits (the hold, or the Durable Object
/// hop's `fetch` and its body), each bounded by `MAX_HOLD_MS`, plus the
/// request CPU limit.
const R_FETCH: AsyncRoute = AsyncRoute::new(1, "probe/fetch", 2 * MAX_HOLD_MS + limits::REQUEST_CPU_MS);
/// Durable Object fetch: at most two sequential awaits (`/alarm`: a storage
/// put and `setAlarm`; `/hold`: one delay), each bounded by `MAX_HOLD_MS`,
/// plus the request CPU limit.
const R_DO: AsyncRoute = AsyncRoute::new(2, "probe/do-fetch", 2 * MAX_HOLD_MS + limits::REQUEST_CPU_MS);
const R_ALARM: AsyncRoute = AsyncRoute::new(3, "probe/alarm", limits::ALARM_MS);
const R_QUEUE: AsyncRoute = AsyncRoute::new(4, "probe/queue", limits::QUEUE_MS);
const R_SCHED: AsyncRoute = AsyncRoute::new(5, "probe/scheduled", limits::CRON_MS);
const R_WAIT: AsyncRoute = AsyncRoute::new(6, "probe/wait-until", limits::WAIT_UNTIL_MS);
/// Control footprint for every probe route: the probe's handlers allocate
/// little; witness: `/stats` reports any overdraft.
const CONTROL: usize = 256 * 1024;

struct Static<T>(UnsafeCell<T>);
unsafe impl<T> Sync for Static<T> {}
static TEXT: Static<[u8; 8192]> = Static(UnsafeCell::new([0; 8192]));
static INSTANCE: Static<f64> = Static(UnsafeCell::new(0.0));

fn instance_id() -> f64 {
    let v = unsafe { &mut *INSTANCE.0.get() };
    if *v == 0.0 {
        *v = js_sys::Date::now() + js_sys::Math::random();
    }
    *v
}

// ------------------------------------------------------------------ abort hook

#[wasm_bindgen::prelude::wasm_bindgen(inline_js = "
export function arena_pm_store(a, b, c, d, e, f, g, h, i, j, k, l, m, n) {
  globalThis.__arena_postmortem = [a, b, c, d, e, f, g, h, i, j, k, l, m, n];
}
export function arena_pm_word(i) {
  const p = globalThis.__arena_postmortem;
  return p ? p[i] : -1;
}
")]
extern "C" {
    #[allow(clippy::too_many_arguments)]
    fn arena_pm_store(a: u32, b: u32, c: u32, d: u32, e: u32, f: u32, g: u32, h: u32, i: u32, j: u32, k: u32, l: u32, m: u32, n: u32);
    fn arena_pm_word(i: u32) -> f64;
}

/// worker's own abort handler (`on_abort` -> `schedule_reinit`), chained.
static PREV_ABORT: Static<Option<fn()>> = Static(UnsafeCell::new(None));
static HOOKED: Static<bool> = Static(UnsafeCell::new(false));

/// Runs in the dying instance when it traps: copies the postmortem out
/// (scalar arguments, allocation-free), then hands over to worker's
/// re-instantiation.
fn arena_on_abort() {
    let w = slop_ninja_arena::postmortem().words();
    arena_pm_store(w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9], w[10], w[11], w[12], w[13]);
    if let Some(prev) = unsafe { *PREV_ABORT.0.get() } {
        prev();
    }
}

/// Installs the abort hook once per instance, after worker's start function
/// registered its own (which it chains to). Allocation-free.
fn install_abort_hook() {
    unsafe {
        if !*HOOKED.0.get() {
            *PREV_ABORT.0.get() = wasm_bindgen::handler::set_on_abort(arena_on_abort);
            *HOOKED.0.get() = true;
        }
    }
}

// ------------------------------------------------------------------ responses

/// A JS response from a `&str`: JS copies it out of linear memory, so the
/// response holds no Rust heap.
fn text_response(s: &str, status: u16) -> web_sys::Response {
    let init = web_sys::ResponseInit::new();
    init.set_status(status);
    web_sys::Response::new_with_opt_str_and_init(Some(s), &init).expect("response")
}

/// The arena counters, snapshotted before this request allocates anything
/// more, and the previous instance's postmortem if one died, as JSON in a
/// static buffer.
fn stats_response(tag: &str) -> web_sys::Response {
    let st = stats();
    let buf = unsafe { &mut *TEXT.0.get() };
    let mut w = FmtBuf::new(buf);
    let _ = write!(w, "{{\"tag\":\"{tag}\",\"instance\":{},\"arena\":", instance_id());
    let _ = st.write_json(&mut w);
    let _ = w.write_str(",\"prev_postmortem\":[");
    for i in 0..slop_ninja_arena::POSTMORTEM_WORDS as u32 {
        let v = arena_pm_word(i);
        let _ = write!(w, "{}{}", if i > 0 { "," } else { "" }, v);
    }
    let _ = w.write_str("]}");
    text_response(w.as_str(), 200)
}

fn query_u64(url: &Url, key: &str, default: u64) -> u64 {
    url.query_pairs().find(|(k, _)| k == key).and_then(|(_, v)| v.parse().ok()).unwrap_or(default)
}

fn query_name(url: &Url) -> String {
    url.query_pairs().find(|(k, _)| k == "name").map(|(_, v)| v.into_owned()).unwrap_or_else(|| "a".to_owned())
}

/// The Worker's routes, all inside one scope. The request is produced by
/// `make` inside the scope, so even its conversion (`worker::Request::from`,
/// which copies the URL into Rust strings) allocates there, not in glue.
/// `/stats` arrives as OPTIONS (the router's mapping).
async fn route(make: impl FnOnce() -> Request, env: Env, ctx: Option<worker::worker_sys::Context>) -> web_sys::Response {
    install_abort_hook();
    let _ = slop_ninja_arena::tick(js_sys::Date::now() as u64);
    let entered = slop_ninja_arena::enter(R_FETCH, CONTROL, move |s| async move {
        let req = make();
        if req.method() == Method::Options {
            return stats_response("worker");
        }
        let url = match req.url() {
            Ok(u) => u,
            Err(_) => return text_response("bad url", 400),
        };
        let ms = query_u64(&url, "ms", HOLD_MS);
        if ms > MAX_HOLD_MS {
            return text_response("hold longer than MAX_HOLD_MS", 400);
        }
        match url.path() {
            "/reset" => {
                slop_ninja_arena::reset_latches();
                text_response("reset", 200)
            }
            "/instance" => {
                let mut b = [0u8; 64];
                let mut w = FmtBuf::new(&mut b);
                let _ = write!(w, "{}", instance_id());
                text_response(w.as_str(), 200)
            }
            "/hold" => {
                Delay::from(Duration::from_millis(ms)).await;
                text_response("held", 200)
            }
            "/wait" => {
                // A `waitUntil` continuation: admitted (ticket, child scope
                // carved from what this scope lends, future box) before it
                // is spawned; refused, nothing is spawned.
                let Some(ctx) = ctx else { return text_response("no context", 500) };
                if s.lend(child_bytes(0) + PAGE).is_err() {
                    return text_response("wait refused", 503);
                }
                let cont = slop_ninja_arena::wait_until(&s, R_WAIT, 0, js_sys::Date::now() as u64, move |_c| async move {
                    Delay::from(Duration::from_millis(ms)).await;
                });
                match cont {
                    Ok(c) => {
                        c.spawn(|c| {
                            let p = wasm_bindgen_futures::future_to_promise(async move {
                                let _ = c.await;
                                Ok(JsValue::UNDEFINED)
                            });
                            let _ = ctx.wait_until(&p);
                        });
                        text_response("spawned", 200)
                    }
                    Err(_) => text_response("wait refused", 503),
                }
            }
            "/trap" => {
                let b = core::hint::black_box(vec![7u8; 4096]);
                if b[0] == 7 {
                    core::arch::wasm32::unreachable();
                }
                text_response("unreachable", 500)
            }
            p if p.starts_with("/do/") => {
                let name = query_name(&url);
                let inner = format!("https://probe-do{}?{}", &p[3..], url.query().unwrap_or(""));
                let out = async {
                    let stub = env.durable_object("PROBE_DO")?.get_by_name(&name)?;
                    let mut res = stub.fetch_with_str(&inner).await?;
                    // Probe only: the Durable Object's own replies are a few bytes.
                    #[allow(clippy::disallowed_methods)]
                    let body = res.text().await?;
                    Ok::<String, Error>(body)
                }
                .await;
                match out {
                    Ok(body) => text_response(&body, 200),
                    Err(_) => text_response("do error", 502),
                }
            }
            _ => text_response("not found", 404),
        }
    });
    match entered {
        Ok(e) => e.await.0,
        Err(r) => {
            let mut b = [0u8; 160];
            let mut w = FmtBuf::new(&mut b);
            let _ = write!(w, "arena refusal {:#x}", r.code());
            text_response(w.as_str(), 503)
        }
    }
}

/// The Durable Object's fetch routes, inside one scope.
async fn do_route(make: impl FnOnce() -> Request, state: &worker::durable::State) -> web_sys::Response {
    install_abort_hook();
    let _ = slop_ninja_arena::tick(js_sys::Date::now() as u64);
    let e = slop_ninja_arena::enter(R_DO, CONTROL, move |_s| async move {
        let req = make();
        if req.method() == Method::Options {
            return stats_response("do");
        }
        let path = req.path();
        let url = req.url().ok();
        let ms = url.as_ref().map_or(HOLD_MS, |u| query_u64(u, "ms", HOLD_MS));
        if ms > MAX_HOLD_MS {
            return text_response("hold longer than MAX_HOLD_MS", 400);
        }
        match path.as_str() {
            "/hold" => {
                Delay::from(Duration::from_millis(ms)).await;
                text_response("held", 200)
            }
            "/alarm" => {
                let _ = state.storage().put("ms", ms).await;
                let _ = state.storage().set_alarm(Duration::from_millis(1)).await;
                text_response("alarm set", 200)
            }
            "/ping" => text_response("pong", 200),
            _ => text_response("not found", 404),
        }
    });
    match e {
        Ok(e) => e.await.0,
        Err(_) => text_response("arena refusal", 503),
    }
}

async fn alarm_route(state: &worker::durable::State) -> web_sys::Response {
    install_abort_hook();
    let _ = slop_ninja_arena::tick(js_sys::Date::now() as u64);
    let e = slop_ninja_arena::enter(R_ALARM, CONTROL, move |_s| async move {
        let ms: u64 = state.storage().get("ms").await.ok().flatten().unwrap_or(HOLD_MS).min(MAX_HOLD_MS);
        Delay::from(Duration::from_millis(ms)).await;
        text_response("alarm done", 200)
    });
    match e {
        Ok(e) => e.await.0,
        Err(_) => text_response("arena refusal", 503),
    }
}

/// Holds a queue batch for its message's milliseconds; never `Err` (a
/// refusal retries the batch). The batch is converted by `make` inside the
/// scope.
async fn queue_route(make: impl FnOnce() -> MessageBatch<u64>, retry: impl FnOnce()) {
    install_abort_hook();
    let _ = slop_ninja_arena::tick(js_sys::Date::now() as u64);
    let e = slop_ninja_arena::enter(R_QUEUE, CONTROL, move |_s| async move {
        let batch = make();
        let ms = batch.messages().ok().and_then(|m| m.first().map(|m| *m.body())).unwrap_or(HOLD_MS).min(MAX_HOLD_MS);
        Delay::from(Duration::from_millis(ms)).await;
        batch.ack_all();
    });
    match e {
        Ok(e) => {
            e.await;
        }
        Err(_) => retry(),
    }
}

async fn scheduled_route() {
    install_abort_hook();
    let _ = slop_ninja_arena::tick(js_sys::Date::now() as u64);
    if let Ok(e) = slop_ninja_arena::enter(R_SCHED, CONTROL, |_s| async move {
        Delay::from(Duration::from_millis(HOLD_MS)).await;
    }) {
        e.await;
    }
}

// ------------------------------------------------------------------ worker-macros exports (comparison build)

#[cfg(not(feature = "admitted"))]
mod macro_exports {
    use super::*;

    #[event(fetch)]
    async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
        // The macro already converted the request in glue (the reason for the
        // hand-written export).
        Ok(Response::from(route(move || req, env, None).await))
    }

    #[durable_object(alarm)]
    pub struct ProbeDo {
        state: State,
        #[allow(dead_code)]
        env: Env,
    }

    impl DurableObject for ProbeDo {
        fn new(state: State, env: Env) -> Self {
            Self { state, env }
        }

        async fn fetch(&self, req: Request) -> Result<Response> {
            Ok(Response::from(do_route(move || req, &self.state).await))
        }

        async fn alarm(&self) -> Result<Response> {
            Ok(Response::from(alarm_route(&self.state).await))
        }
    }

    #[event(queue)]
    async fn queue(batch: MessageBatch<u64>, _env: Env, _ctx: Context) -> Result<()> {
        // The macro converted the batch in glue already; this comparison
        // build cannot retry it after handing it over.
        queue_route(move || batch, || {}).await;
        // Never `Err` from a queue handler: worker-macros turns it into a panic
        // (a trap), so refusals retry the batch instead.
        Ok(())
    }

    #[event(scheduled)]
    async fn scheduled(_event: ScheduledEvent, _env: Env, _ctx: ScheduleContext) {
        scheduled_route().await;
    }
}

// ------------------------------------------------------------------ hand-written exports

#[cfg(feature = "admitted")]
mod admitted {
    use super::*;
    use slop_ninja_arena::{Health, glue};
    use worker::wasm_bindgen::prelude::*;
    use worker::worker_sys::{DurableObjectState, MessageBatch as MessageBatchSys, ScheduleContext as ScheduleContextSys, ScheduledEvent as ScheduledEventSys};

    /// Health check at a safe point (after the ticket, before any state
    /// change): a poisoned pool is recycled. A coordinator first writes its
    /// durable recycle intent keyed by `cause` and applies the stop rule.
    fn recycle_if_poisoned() {
        install_abort_hook();
        if let Health::Poisoned { cause, .. } = slop_ninja_arena::tick(js_sys::Date::now() as u64) {
            slop_ninja_arena::recycle(cause);
        }
    }

    fn busy() -> js_sys::Promise {
        let v: JsValue = text_response("arena: every glue ticket is in flight", 503).into();
        js_sys::Promise::resolve(&v)
    }

    /// The Worker fetch export: ticket first (allocation-free), then the
    /// health check, then the task. The JS-only 503 allocates nothing.
    #[wasm_bindgen]
    pub fn fetch(req: web_sys::Request, env: worker::Env, ctx: worker::worker_sys::Context) -> js_sys::Promise {
        let Ok(ticket) = glue::Ticket::admit() else { return busy() };
        recycle_if_poisoned();
        wasm_bindgen_futures::future_to_promise(async move {
            let _ticket = ticket;
            // The request is converted inside the handler's scope.
            let res = route(move || Request::from(req), env, Some(ctx)).await;
            Ok(res.into())
        })
    }

    /// The queue export: past the tickets, the batch is retried
    /// (allocation-free) and the export resolves.
    #[wasm_bindgen]
    pub fn queue(batch: MessageBatchSys, _env: worker::Env, _ctx: worker::worker_sys::Context) -> js_sys::Promise {
        let Ok(ticket) = glue::Ticket::admit() else {
            let _ = batch.retry_all(JsValue::UNDEFINED);
            return js_sys::Promise::resolve(&JsValue::UNDEFINED);
        };
        recycle_if_poisoned();
        let retry = batch.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            let _ticket = ticket;
            queue_route(
                move || worker::MessageBatch::<u64>::from(batch),
                move || {
                    let _ = retry.retry_all(JsValue::UNDEFINED);
                },
            )
            .await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The scheduled export: past the tickets, it resolves (the next
    /// minute's tick does the work).
    #[wasm_bindgen]
    pub fn scheduled(_event: ScheduledEventSys, _env: worker::Env, _ctx: ScheduleContextSys) -> js_sys::Promise {
        let Ok(ticket) = glue::Ticket::admit() else { return js_sys::Promise::resolve(&JsValue::UNDEFINED) };
        recycle_if_poisoned();
        wasm_bindgen_futures::future_to_promise(async move {
            let _ticket = ticket;
            scheduled_route().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The Durable Object class, hand-written: its fields are JS handles and
    /// a resident token (the instance's `Rc<WasmRefCell<T>>` is glue covered
    /// by it).
    #[wasm_bindgen]
    pub struct ProbeDo {
        state: DurableObjectState,
        _resident: glue::Resident,
    }

    impl ProbeDo {
        /// A `State` over a second handle to the same JS object (a JS
        /// handle clone, no Rust heap) for the task to own.
        fn state_handle(&self) -> worker::durable::State {
            use worker::wasm_bindgen::JsCast;
            let v: &JsValue = self.state.as_ref();
            worker::durable::State::from(v.clone().unchecked_into::<DurableObjectState>())
        }
    }

    #[wasm_bindgen]
    impl ProbeDo {
        /// Past the resident count the constructor throws (a JS string, no
        /// Rust heap) and the caller retries after an idle instance goes.
        #[wasm_bindgen(constructor)]
        pub fn new(state: DurableObjectState, _env: worker::Env) -> core::result::Result<ProbeDo, JsValue> {
            let Ok(resident) = glue::Resident::admit() else { return Err(JsValue::from_str("arena: resident limit")) };
            Ok(ProbeDo { state, _resident: resident })
        }

        /// Its own ticket: a Worker-to-Durable-Object hop takes one per entry.
        #[wasm_bindgen(js_name = fetch)]
        pub fn fetch(&self, req: web_sys::Request) -> js_sys::Promise {
            let Ok(ticket) = glue::Ticket::admit() else { return busy() };
            recycle_if_poisoned();
            let state = self.state_handle();
            wasm_bindgen_futures::future_to_promise(async move {
                let _ticket = ticket;
                let res = do_route(move || Request::from(req), &state).await;
                Ok(res.into())
            })
        }

        /// Past the tickets the alarm rejects, so the platform retries it.
        #[wasm_bindgen(js_name = alarm)]
        pub fn alarm(&self) -> js_sys::Promise {
            let Ok(ticket) = glue::Ticket::admit() else { return js_sys::Promise::reject(&JsValue::from_str("arena: every glue ticket is in flight")) };
            recycle_if_poisoned();
            let state = self.state_handle();
            wasm_bindgen_futures::future_to_promise(async move {
                let _ticket = ticket;
                let res = alarm_route(&state).await;
                Ok(res.into())
            })
        }
    }
}
