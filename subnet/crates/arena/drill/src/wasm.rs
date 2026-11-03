//! wasm32 exports for the local workerd drill. No wasm-bindgen: the module's
//! imports are `drill.read_chunk` (copy body bytes from JS into an admitted
//! buffer), `drill.write_chunk` and `drill.abort_stream` (hand bundle bytes to
//! JS, or abort it: the stand-in for a `FixedLengthStream` writer),
//! `drill.recycle_intent` (the durable recycle intent the host writes before
//! a recycle) and `drill.postmortem` (what the abort hook copies out of a
//! dying instance).
//!
//! After a trap the JS host plays worker-build 0.8.7: it calls the abort
//! hook export `drill_on_abort` in the dying instance (as `re()` calls
//! `__abort_handler`), which hands the [`postmortem`](slop_ninja_arena::postmortem)
//! words to `drill.postmortem`, and then never reads that instance's memory
//! again: the next call gets a fresh instance.
//!
//! Results go into fixed static arrays that JS reads from linear memory, so
//! reporting never allocates.
//!
//! `drill_invoke(len, receipts, flags, now_ms, js_need, target)`:
//!
//! - `flags & 1`: stream the bundle in chunks;
//! - `flags & 2`: leak the body into a count-bounded cache (quarantine);
//! - `flags & 4`: async: run the handler, then await a host promise with the
//!   scope (and the body) still open; the pending future is kept in a slot,
//!   as the js-sys task cycle keeps it;
//! - `flags & 8`: trap inside the scope after the handler (an infallible
//!   allocation failing, or any other trap);
//! - `flags & 16`: settle the promise of pending slot `target`, then poll it;
//! - `flags & 32`: drop pending slot `target` (the platform cancels it).
//!
//! `now_ms` is the host clock for the abandonment sweep. `js_need` is the JS
//! memory the invocation causes (input plus sink); one past the model's
//! measured JS envelope is refused before anything allocates.

use core::cell::UnsafeCell;
use core::fmt::Write as _;

use slop_ninja_arena::io::FmtBuf;

use crate::{Ctx, Fail, Output};

#[link(wasm_import_module = "drill")]
unsafe extern "C" {
    fn read_chunk(dst: *mut u8, offset: u32, max: u32) -> u32;
    fn write_chunk(src: *const u8, len: u32);
    fn abort_stream(code: u32);
    #[cfg_attr(not(feature = "arena"), allow(dead_code))]
    fn recycle_intent(route: u32, kind: u32, pages: u32);
    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(not(feature = "arena"), allow(dead_code))]
    fn postmortem(w0: u32, w1: u32, w2: u32, w3: u32, w4: u32, w5: u32, w6: u32, w7: u32, w8: u32, w9: u32, w10: u32, w11: u32, w12: u32, w13: u32);
}

struct Static<T>(UnsafeCell<T>);
// SAFETY: single-threaded wasm32.
unsafe impl<T> Sync for Static<T> {}

/// Words: `[status, count, bundle_len, index_len, peak_live, reserved_bytes,
/// peak_pages, closed (0 released, 1 quarantined, 2 not opened, 3 pending),
/// live_at_close, bundle_sha256 (8 words), index_root (8 words), recycle,
/// slot, settled_status, settled_closed, dropped]`.
pub const OUT_WORDS: usize = 30;
static OUT: Static<[u32; OUT_WORDS]> = Static(UnsafeCell::new([0; OUT_WORDS]));
/// UTF-8 text for JS (stats JSON, self-test report).
static TEXT: Static<[u8; 16384]> = Static(UnsafeCell::new([0; 16384]));
static TEXT_LEN: Static<u32> = Static(UnsafeCell::new(0));

pub const ST_PENDING: u32 = 0x7001;
// 0x7002 (recycled) and 0x7005 (trapped) are set by the JS host from the
// recycle intent and the abort hook: a dying instance's memory is not read.
#[cfg_attr(not(feature = "arena"), allow(dead_code))]
pub const ST_NO_SLOT: u32 = 0x7003;
/// `flags & 64`: deliver the settle/drop fates only (a promise settling is a
/// JS event, not a request), then return.
pub const FATES_ONLY: u32 = 64;
pub const ST_JS_ENVELOPE: u32 = 0x7004;

fn out() -> &'static mut [u32; OUT_WORDS] {
    unsafe { &mut *OUT.0.get() }
}

fn put_output(o: &Output) {
    let w = out();
    (w[1], w[2], w[3]) = (o.count, o.bundle_len, o.index_len);
    for (i, c) in o.bundle_sha256.as_chunks::<4>().0.iter().enumerate() {
        w[9 + i] = u32::from_le_bytes(*c);
    }
    for (i, c) in o.index_root.as_chunks::<4>().0.iter().enumerate() {
        w[17 + i] = u32::from_le_bytes(*c);
    }
}

/// Ingests `len` body bytes from JS through the bounded reader, then runs
/// the handler.
fn run(cx: &Ctx, len: u32, receipts: u32, flags: u32) -> Result<(Output, slop_ninja_arena::io::ExactBuf), Fail> {
    let mut ing = cx.ingest(len as usize, receipts as usize)?;
    let mut off = 0u32;
    ing.pull(|dst| {
        let max = dst.len().min(crate::CHUNK) as u32;
        // SAFETY: JS writes `n <= max` bytes at `dst`, inside the spare tail
        // (or the one-byte probe).
        let n = unsafe { read_chunk(dst.as_mut_ptr().cast(), off, max) };
        off += n;
        n as usize
    })?;
    let body = ing.finish()?;
    let mut emit = |b: &[u8]| {
        unsafe { write_chunk(b.as_ptr(), b.len() as u32) };
        Ok(())
    };
    let mut abort = |r: slop_ninja_arena::Refusal| unsafe { abort_stream(r.code()) };
    let o = crate::handle(cx, body.as_slice(), flags & 1 == 1, &mut emit, &mut abort)?;
    Ok((o, body))
}

/// The JS memory envelope the model's JS reserve was measured for.
fn js_outside_envelope(js_need: u32) -> bool {
    js_need as usize > slop_ninja_arena::model::JS_ENVELOPE_BYTES
}

// ------------------------------------------------------------------ arena build

#[cfg(feature = "arena")]
mod build {
    use super::*;
    use crate::{CONTROL_BYTES, ROUTE, ROUTE_ASYNC};
    use core::future::Future;
    use core::pin::Pin;
    use core::task::{Context, Poll, Waker};
    use slop_ninja_arena::{ArenaAlloc, Closed, Health, Scope, enter, glue, io::ExactBuf, model, stats, strict, tick};

    /// The abort hook (worker-build's `__abort_handler` stand-in): copies the
    /// postmortem out of this dying instance through an import.
    /// Allocation-free.
    #[unsafe(no_mangle)]
    pub extern "C" fn drill_on_abort() {
        let w = slop_ninja_arena::postmortem().words();
        unsafe { postmortem(w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7], w[8], w[9], w[10], w[11], w[12], w[13]) };
    }

    #[global_allocator]
    static ALLOC: ArenaAlloc = ArenaAlloc;

    /// A per-isolate cache bounded only by count, like the coordinator's
    /// `PLANS` (cleared above 64 entries). Used by the quarantine scenario.
    // scan-ok: the deliberate leak the quarantine scenario measures.
    static CACHE: Static<Option<Vec<ExactBuf>>> = Static(UnsafeCell::new(None));

    type Pending = Pin<Box<dyn Future<Output = (u32, Closed)>>>;
    type Slots = [Option<Pending>; SLOTS];
    const SLOTS: usize = model::GLUE_TICKETS as usize;
    /// Pending invocations, as the js-sys executor's task cycle keeps them.
    /// One per glue ticket: a slot holds its ticket until it completes or is
    /// dropped.
    // scan-ok: the js-sys task cycle the abandonment scenario models, one
    // slot per glue ticket.
    static PENDING: Static<Slots> = Static(UnsafeCell::new([const { None }; SLOTS]));
    /// Whether slot `i`'s host promise has settled.
    static SETTLED: Static<[bool; SLOTS]> = Static(UnsafeCell::new([false; SLOTS]));

    /// A host promise: ready once its slot is settled.
    struct HostPromise(usize);
    impl Future for HostPromise {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
            if unsafe { (*SETTLED.0.get())[self.0] } { Poll::Ready(()) } else { Poll::Pending }
        }
    }

    fn pending() -> &'static mut Slots {
        unsafe { &mut *PENDING.0.get() }
    }

    fn settle(target: usize) {
        let Some(slot) = pending().get_mut(target) else { return };
        let Some(mut fut) = slot.take() else { return };
        unsafe { (*SETTLED.0.get())[target] = true };
        let mut cx = Context::from_waker(Waker::noop());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready((status, closed)) => {
                out()[27] = status;
                out()[28] = matches!(closed, Closed::Quarantined { .. }) as u32;
                drop(fut);
                glue::release();
            }
            Poll::Pending => *slot = Some(fut),
        }
    }

    fn drop_slot(target: usize) {
        let Some(slot) = pending().get_mut(target) else { return };
        if let Some(fut) = slot.take() {
            // The platform drops the task: the drop guard closes the scope.
            drop(fut);
            glue::release();
            out()[29] = 1;
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn drill_invoke(len: u32, receipts: u32, flags: u32, now_ms: u32, js_need: u32, target: u32) -> u32 {
        *out() = [0; OUT_WORDS];
        // 1. The clock and the abandonment sweep, before anything allocates.
        //    A poisoned pool is recycled at this safe point (the JS host
        //    records the durable fault from the RECYCLE status and
        //    re-instantiates, as worker-build 0.8.7's reinit does).
        if let Health::Poisoned { cause, .. } = tick(now_ms as u64) {
            // The durable recycle intent (route, cause) goes to the host
            // before the trap; the host applies the stop rule.
            unsafe { recycle_intent(cause.route as u32, cause.kind as u32, cause.pages) };
            slop_ninja_arena::recycle(cause);
        }
        if flags & 16 != 0 {
            settle(target as usize);
        }
        if flags & 32 != 0 {
            drop_slot(target as usize);
        }
        if flags & FATES_ONLY != 0 {
            return 0;
        }
        // 2. The JS envelope (plan-level admission for JS memory).
        if js_outside_envelope(js_need) {
            out()[0] = ST_JS_ENVELOPE;
            out()[7] = 2;
            return ST_JS_ENVELOPE;
        }
        // 3. The entry's glue ticket.
        let ticket = match glue::Ticket::admit() {
            Ok(t) => t,
            Err(r) => {
                out()[0] = r.code();
                out()[7] = 2;
                return r.code();
            }
        };
        if flags & 4 != 0 {
            return begin_async(ticket, len, receipts, flags);
        }
        // Every allocation of a synchronous invocation is in its scope: the
        // instance is armed, so anything outside one is a recorded violation.
        let _armed = strict::arm();
        let scope = match Scope::open(ROUTE, CONTROL_BYTES) {
            Ok(s) => s,
            Err(r) => {
                out()[0] = r.code();
                out()[7] = 2;
                return r.code();
            }
        };
        let cx = Ctx::Scoped(&scope);
        let status = match scope.enter(|| run(&cx, len, receipts, flags)) {
            Ok((o, body)) => {
                put_output(&o);
                if flags & 8 != 0 {
                    // A trap with scope memory live (an infallible allocation
                    // failing would end the same way).
                    scope.enter(|| {
                        let _b = core::hint::black_box(Box::new([0u8; 256]));
                        core::arch::wasm32::unreachable()
                    });
                }
                if flags & 2 != 0 {
                    // Quarantine scenario: the body outlives its scope.
                    let cache = unsafe { &mut *CACHE.0.get() };
                    scope.enter(|| {
                        let c = cache.get_or_insert_with(|| Vec::with_capacity(65));
                        if c.len() >= 64 {
                            c.clear();
                        }
                        c.push(body);
                    });
                } else {
                    scope.enter(|| drop(body));
                }
                0
            }
            Err(f) => f.code(),
        };
        let st = scope.stats();
        let closed = scope.close();
        drop(ticket);
        let w = out();
        w[0] = status;
        w[4] = st.peak_live as u32;
        w[5] = st.reserved_bytes as u32;
        w[6] = st.peak_pages;
        match closed {
            Closed::Released { .. } => w[7] = 0,
            Closed::Quarantined { live, .. } => {
                w[7] = 1;
                w[8] = live as u32;
            }
        }
        status
    }

    /// Starts an async invocation: the handler runs, then awaits a host
    /// promise with its body live. The pending future goes to a glue slot.
    fn begin_async(ticket: glue::Ticket, len: u32, receipts: u32, flags: u32) -> u32 {
        let Some(i) = pending().iter().position(Option::is_none) else {
            out()[0] = ST_NO_SLOT;
            return ST_NO_SLOT;
        };
        unsafe { (*SETTLED.0.get())[i] = false };
        let e = enter(ROUTE_ASYNC, CONTROL_BYTES, move |sref| async move {
            let cx = Ctx::Scoped(&sref);
            match run(&cx, len, receipts, flags) {
                Ok((o, body)) => {
                    put_output(&o);
                    HostPromise(i).await;
                    drop(body);
                    0u32
                }
                Err(f) => f.code(),
            }
        });
        let e = match e {
            Ok(e) => e,
            Err(r) => {
                out()[0] = r.code();
                out()[7] = 2;
                return r.code();
            }
        };
        // The task box is glue (no scope is current), under the ticket.
        let mut fut: Pending = Box::pin(e);
        let mut cx = Context::from_waker(Waker::noop());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready((status, closed)) => {
                drop(fut);
                out()[0] = status;
                out()[7] = matches!(closed, Closed::Quarantined { .. }) as u32;
                status
            }
            Poll::Pending => {
                pending()[i] = Some(fut);
                ticket.into_raw();
                let w = out();
                w[0] = ST_PENDING;
                w[7] = 3;
                w[26] = i as u32;
                ST_PENDING
            }
        }
    }

    /// Isolate counters as JSON into `TEXT`; returns its length.
    #[unsafe(no_mangle)]
    pub extern "C" fn drill_stats() -> u32 {
        let buf = unsafe { &mut *TEXT.0.get() };
        let mut w = FmtBuf::new(buf);
        let v = strict::take();
        let pend = pending().iter().filter(|p| p.is_some()).count();
        let ok = w.write_str("{\"arena\":").is_ok()
            && stats().write_json(&mut w).is_ok()
            && write!(
                w,
                ",\"pending\":{pend},\"violations\":{{\"unscoped\":{},\"overdrafts\":{},\"leaks\":{}}}}}",
                v.unscoped, v.overdrafts, v.leaks
            )
            .is_ok();
        let n = if ok { w.len() as u32 } else { 0 };
        unsafe { *TEXT_LEN.0.get() = n };
        n
    }

    /// Runs the allocator self-tests in this (fresh) instance. Returns the
    /// number of failures; the report is in `TEXT`.
    #[unsafe(no_mangle)]
    pub extern "C" fn selftest_run() -> u32 {
        let buf = unsafe { &mut *TEXT.0.get() };
        let mut w = FmtBuf::new(buf);
        let (pass, fail) = slop_ninja_arena::selftest::run_all(&mut w);
        let _ = writeln!(w, "{pass} passed, {fail} failed");
        unsafe { *TEXT_LEN.0.get() = w.len() as u32 };
        fail
    }
}

// ------------------------------------------------------------------ default build

#[cfg(not(feature = "arena"))]
mod build {
    use super::*;
    use core::alloc::{GlobalAlloc, Layout};
    use std::alloc::System;

    /// std's allocator (dlmalloc on wasm32) with a live-byte counter.
    struct Counting;
    static LIVE: Static<usize> = Static(UnsafeCell::new(0));
    static PEAK: Static<usize> = Static(UnsafeCell::new(0));

    fn bump(add: usize, sub: usize) {
        unsafe {
            let l = &mut *LIVE.0.get();
            *l = *l + add - sub;
            let p = &mut *PEAK.0.get();
            *p = (*p).max(*l);
        }
    }

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let p = unsafe { System.alloc(l) };
            if !p.is_null() {
                bump(l.size(), 0);
            }
            p
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) };
            bump(0, l.size());
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
            let q = unsafe { System.realloc(p, l, new) };
            if !q.is_null() {
                bump(new, l.size());
            }
            q
        }
    }

    #[global_allocator]
    static ALLOC: Counting = Counting;

    // scan-ok: the deliberate leak the quarantine scenario measures.
    static CACHE: Static<Option<Vec<slop_ninja_arena::io::ExactBuf>>> = Static(UnsafeCell::new(None));
    /// Pending async invocations' bodies, keyed by slot (no arena: nothing
    /// bounds or reclaims them).
    // scan-ok: the abandonment scenario's comparison build.
    static PENDING: Static<Option<Vec<Option<slop_ninja_arena::io::ExactBuf>>>> = Static(UnsafeCell::new(None));

    #[unsafe(no_mangle)]
    pub extern "C" fn drill_invoke(len: u32, receipts: u32, flags: u32, _now_ms: u32, js_need: u32, target: u32) -> u32 {
        *out() = [0; OUT_WORDS];
        let pend = unsafe { (*PENDING.0.get()).get_or_insert_with(Vec::new) };
        if flags & (16 | 32) != 0
            && let Some(slot) = pend.get_mut(target as usize)
        {
            *slot = None;
        }
        if flags & FATES_ONLY != 0 {
            return 0;
        }
        if js_outside_envelope(js_need) {
            out()[0] = ST_JS_ENVELOPE;
            out()[7] = 2;
            return ST_JS_ENVELOPE;
        }
        let base = unsafe { *LIVE.0.get() };
        unsafe { *PEAK.0.get() = base };
        let status = match run(&Ctx::Unscoped, len, receipts, flags) {
            Ok((o, body)) => {
                put_output(&o);
                if flags & 8 != 0 {
                    core::arch::wasm32::unreachable();
                }
                if flags & 4 != 0 {
                    let i = pend.iter().position(Option::is_none).unwrap_or(pend.len());
                    if i == pend.len() {
                        pend.push(None);
                    }
                    pend[i] = Some(body);
                    out()[26] = i as u32;
                    out()[7] = 3;
                    ST_PENDING
                } else {
                    if flags & 2 != 0 {
                        let cache = unsafe { &mut *CACHE.0.get() };
                        let c = cache.get_or_insert_with(|| Vec::with_capacity(65));
                        if c.len() >= 64 {
                            c.clear();
                        }
                        c.push(body);
                    }
                    0
                }
            }
            Err(f) => f.code(),
        };
        let w = out();
        w[0] = status;
        w[4] = unsafe { *PEAK.0.get() - base } as u32;
        w[8] = unsafe { *LIVE.0.get() - base } as u32;
        status
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn drill_stats() -> u32 {
        let buf = unsafe { &mut *TEXT.0.get() };
        let mut w = FmtBuf::new(buf);
        let (live, peak) = unsafe { (*LIVE.0.get(), *PEAK.0.get()) };
        let mem = core::arch::wasm32::memory_size(0) * 65536;
        let pend = unsafe { (*PENDING.0.get()).as_ref().map_or(0, |p| p.iter().filter(|x| x.is_some()).count()) };
        let ok = write!(w, "{{\"memory_bytes\":{mem},\"live\":{live},\"peak\":{peak},\"pending\":{pend}}}").is_ok();
        let n = if ok { w.len() as u32 } else { 0 };
        unsafe { *TEXT_LEN.0.get() = n };
        n
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn selftest_run() -> u32 {
        u32::MAX
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn drill_out() -> *const u32 {
    OUT.0.get().cast()
}

#[unsafe(no_mangle)]
pub extern "C" fn drill_text() -> *const u8 {
    TEXT.0.get().cast()
}

#[unsafe(no_mangle)]
pub extern "C" fn drill_text_len() -> u32 {
    unsafe { *TEXT_LEN.0.get() }
}

#[unsafe(no_mangle)]
pub extern "C" fn drill_arena() -> u32 {
    cfg!(feature = "arena") as u32
}

/// The model constants the runner needs (JS envelope, tickets), as JSON.
#[unsafe(no_mangle)]
pub extern "C" fn drill_model() -> u32 {
    use slop_ninja_arena::model as m;
    let buf = unsafe { &mut *TEXT.0.get() };
    let mut w = FmtBuf::new(buf);
    let ok = write!(
        w,
        "{{\"version\":\"{}\",\"js_reserve_bytes\":{},\"js_envelope_bytes\":{},\"glue_tickets\":{},\"glue_budget_pages\":{},\"largest_route_bytes\":{},\"control_bytes\":{}}}",
        m::VERSION,
        m::JS_RESERVE_BYTES,
        m::JS_ENVELOPE_BYTES,
        m::GLUE_TICKETS,
        m::GLUE_BUDGET_PAGES,
        m::LARGEST_ROUTE_BYTES,
        crate::CONTROL_BYTES
    )
    .is_ok();
    let n = if ok { w.len() as u32 } else { 0 };
    unsafe { *TEXT_LEN.0.get() = n };
    n
}
