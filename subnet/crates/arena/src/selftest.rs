//! Allocator self-test cases, shared by the native suite (`tests/selftest.rs`)
//! and the wasm32 build run in local workerd by the drill. They need
//! [`ArenaAlloc`](crate::ArenaAlloc) as the global allocator and one thread:
//! they assert on isolate-wide counters.
//!
//! Cases named after a review finding (`f_…`) reproduce the reviewed defect's
//! inputs and assert the fixed behaviour.

use core::fmt::Write;
use core::future::Future;
use core::pin::{Pin, pin};
use core::task::{Context, Poll, Waker};

use crate::io::{BoundedVec, ChunkSink, Count, Enc, ExactBuf, FmtBuf, Hash, Ingest, Kinds, Recheck, Sink, Tee, encoded_len, hex};
use crate::{
    AsyncRoute, Closed, Footprint, Health, MAX_PAGES, Mode, Owner, PAGE, PinKind, Reason, RefusalKind, Route,
    SEGMENT_OVERHEAD, STATE_BYTES, Scope, ScopeRef, block_bytes, child_bytes, enter, glue, limits, model, owner_of,
    page_limit, postmortem, reset_latches, route_stats, set_mode, set_page_limit, stats, strict, tick, wait_until,
};

type CaseResult = Result<(), &'static str>;
pub type Case = (&'static str, fn() -> CaseResult);

const R: Route = Route::new(250, "selftest");
const R2: Route = Route::new(251, "selftest-b");
/// The same identities as async routes (a 30 s deadline, the `waitUntil`
/// limit: any non-zero bound serves the cases).
const RA: AsyncRoute = AsyncRoute::new(250, "selftest", limits::WAIT_UNTIL_MS);
const RA2: AsyncRoute = AsyncRoute::new(251, "selftest-b", limits::WAIT_UNTIL_MS);
const N: Kinds = Kinds::NETWORK;
const E: Kinds = Kinds::ENCODER;

macro_rules! ensure {
    ($c:expr, $m:expr) => {
        if !$c {
            return Err($m);
        }
    };
}

pub const CASES: &[Case] = &[
    ("model_ceiling", model_ceiling),
    ("fixed_linear_memory_never_grows", fixed_linear_memory_never_grows),
    ("current_scope_admits_an_exact_block", current_scope_admits_an_exact_block),
    ("scope_owns_its_allocations", scope_owns_its_allocations),
    ("free_routes_to_owner", free_routes_to_owner),
    ("quarantine_until_last_free", quarantine_until_last_free),
    ("realloc_charges_owner", realloc_charges_owner),
    ("admission_retry_then_capacity", admission_retry_then_capacity),
    ("one_byte_short_refuses_without_allocating", one_byte_short_refuses_without_allocating),
    ("refusals_allocate_nothing", refusals_allocate_nothing),
    ("overdraft_recorded_in_enforce", overdraft_recorded_in_enforce),
    ("strict_refuses_overdraft_and_unscoped", strict_refuses_overdraft_and_unscoped),
    ("exhaustion_reported_not_aborted", exhaustion_reported_not_aborted),
    ("observe_records_without_refusing", observe_records_without_refusing),
    ("scoped_futures_interleave", scoped_futures_interleave),
    ("enter_boxes_future_in_scope", enter_boxes_future_in_scope),
    ("repeated_scopes_leave_memory_flat", repeated_scopes_leave_memory_flat),
    ("segment_trim_returns_pages", segment_trim_returns_pages),
    ("block_bound_holds", block_bound_holds),
    ("stats_report_allocation_free", stats_report_allocation_free),
    ("two_pass_encode_and_hash", two_pass_encode_and_hash),
    ("ingest_bounds", ingest_bounds),
    ("bounded_vec_never_grows", bounded_vec_never_grows),
    ("hex_roundtrip", hex_roundtrip),
    ("f_glue_partition_is_bounded_and_admitted", f_glue_partition_is_bounded_and_admitted),
    ("f_abandoned_scope_counted_recovered_reclaimed", f_abandoned_scope_counted_recovered_reclaimed),
    ("f_pinned_pages_poison_not_retry", f_pinned_pages_poison_not_retry),
    ("f_footprint_covers_small_allocations", f_footprint_covers_small_allocations),
    ("f_admission_holds_contiguous_runs", f_admission_holds_contiguous_runs),
    ("f_quarantine_trims_to_live_pages", f_quarantine_trims_to_live_pages),
    ("f_ingest_refuses_overlong_streams", f_ingest_refuses_overlong_streams),
    ("f_child_carves_from_parent", f_child_carves_from_parent),
    ("f_recheck_refuses_changed_rows", f_recheck_refuses_changed_rows),
    ("f_observe_overflow_is_recorded_not_wrapped", f_observe_overflow_is_recorded_not_wrapped),
    ("f_refusal_kind_by_cause", f_refusal_kind_by_cause),
    ("f_js_string_admission", f_js_string_admission),
    ("f2_out_of_order_blocks_are_served", f2_out_of_order_blocks_are_served),
    ("f2_admitted_allocations_never_fail_fuzz", f2_admitted_allocations_never_fail_fuzz),
    ("f2_stale_handle_never_reaches_another_scope", f2_stale_handle_never_reaches_another_scope),
    ("f2_need_blocked_by_own_pages_is_capacity", f2_need_blocked_by_own_pages_is_capacity),
    ("f2_child_carves_only_lent_pages", f2_child_carves_only_lent_pages),
    ("f2_child_takes_its_own_route_deadline", f2_child_takes_its_own_route_deadline),
    ("f2_async_scopes_always_have_deadlines", f2_async_scopes_always_have_deadlines),
    ("f2_wait_until_continuation", f2_wait_until_continuation),
    ("f2_poison_cause_names_the_pinning_route", f2_poison_cause_names_the_pinning_route),
    ("f2_postmortem_names_the_current_route", f2_postmortem_names_the_current_route),
    ("f2_route_bound_excess_is_recorded", f2_route_bound_excess_is_recorded),
    ("f2_short_streams_are_aborted", f2_short_streams_are_aborted),
    ("f2_stack_is_measured", f2_stack_is_measured),
];

/// Runs every case with the mode, page limit and latches restored between
/// cases and writes one line per case. Returns `(passed, failed)`.
pub fn run_all(report: &mut impl Write) -> (u32, u32) {
    let full = page_limit();
    let (mut pass, mut fail) = (0, 0);
    for (name, case) in CASES {
        set_mode(Mode::Enforce);
        reset_latches();
        let before = stats();
        let mut r = case();
        set_mode(Mode::Enforce);
        let _ = set_page_limit(full);
        reset_latches();
        let _ = strict::take();
        let after = stats();
        if r.is_ok()
            && (after.open_scopes != before.open_scopes
                || after.quarantined_scopes != before.quarantined_scopes
                || after.abandoned_scopes != before.abandoned_scopes
                || after.tickets_in_flight != before.tickets_in_flight)
        {
            r = Err("case left a scope open, quarantined or abandoned, or a ticket in flight");
        }
        match r {
            Ok(()) => {
                pass += 1;
                let _ = writeln!(report, "ok   {name}");
            }
            Err(m) => {
                fail += 1;
                let _ = writeln!(report, "FAIL {name}: {m}");
            }
        }
    }
    (pass, fail)
}

fn available() -> u32 {
    stats().free_pages
}

/// Control bytes whose `open` needs exactly `pages` pages (state included).
fn bytes_for_pages(pages: u32) -> usize {
    (pages as usize - 1) * PAGE - SEGMENT_OVERHEAD
}

/// Lowers the pool so that `free` scope pages remain free.
fn shrink_to_free(free: u32) -> Result<u32, &'static str> {
    crate::shrink_free_to(free).map_err(|_| "lower the limit")
}

fn model_ceiling() -> CaseResult {
    let s = stats();
    ensure!(MAX_PAGES == model::ISOLATE_BYTES / PAGE, "static tables sized from isolate_bytes");
    let expect = model::pool_pages(s.static_bytes).min(MAX_PAGES);
    ensure!(s.limit_pages as usize == expect, "limit is floor(heap_ceiling / page)");
    ensure!(model::JS_RESERVE_BYTES > 0, "the JS reserve is never zero");
    ensure!(
        s.static_bytes + s.limit_pages as usize * PAGE <= model::ISOLATE_BYTES - model::JS_RESERVE_BYTES,
        "pool fits under the isolate minus the JS reserve"
    );
    ensure!(s.glue_budget_pages == model::GLUE_BUDGET_PAGES, "glue partition from the model");
    ensure!(model::GLUE_BUDGET_PAGES as usize * PAGE >= model::GLUE_BUDGET_BYTES, "partition holds the derived glue budget");
    ensure!(STATE_BYTES < PAGE, "scope state fits its first page");
    ensure!(block_bytes(1, 1) == PAGE && block_bytes(PAGE, 1) == 2 * PAGE, "block rounding");
    Ok(())
}

/// The pool fits the model's fixed linear memory, and under a module linked
/// with it (`initial = maximum = LINEAR_MEMORY_BYTES`, wasm32) every pool page
/// is present from the start: filling the whole pool never grows memory.
fn fixed_linear_memory_never_grows() -> CaseResult {
    let s = stats();
    ensure!(model::LINEAR_MEMORY_BYTES.is_multiple_of(PAGE), "linear memory is whole pages");
    ensure!(model::LINEAR_MEMORY_BYTES == model::LINEAR_MEMORY_PAGES * PAGE, "linear memory pages");
    ensure!(
        model::LINEAR_MEMORY_BYTES <= model::ISOLATE_BYTES - model::JS_RESERVE_BYTES
            && model::ISOLATE_BYTES - model::JS_RESERVE_BYTES - model::LINEAR_MEMORY_BYTES < PAGE,
        "linear memory is floor((isolate - js_reserve) / page) pages"
    );
    ensure!(s.static_bytes + s.limit_pages as usize * PAGE <= model::LINEAR_MEMORY_BYTES, "static data and the pool fit the linear memory");
    if !cfg!(target_arch = "wasm32") || s.memory_bytes != model::LINEAR_MEMORY_BYTES {
        // Native, or a module linked without the fixed memory (it grows).
        return Ok(());
    }
    // Hold every free scope page at once, then give them back.
    let free = s.free_pages;
    ensure!(free > 2, "free pages to hold");
    let scope = Scope::open(R, bytes_for_pages(free)).map_err(|_| "the whole free pool is admitted")?;
    let held = stats();
    let _ = scope.close();
    ensure!(held.memory_bytes == model::LINEAR_MEMORY_BYTES, "holding the whole pool did not grow memory");
    ensure!(stats().memory_bytes == model::LINEAR_MEMORY_BYTES, "memory stays fixed");
    Ok(())
}

/// `admit_current_block` admits in the scope being run (a helper that learns
/// an exact size without the scope handle); with no scope current it admits
/// nothing and reports so.
fn current_scope_admits_an_exact_block() -> CaseResult {
    ensure!(crate::admit_current_block(4096, 1) == Ok(false), "no scope current: nothing admitted");
    let scope = Scope::open(R, 4096).map_err(|_| "open")?;
    let len = 3 * PAGE + 17;
    let before = stats();
    let ok = scope.enter(|| {
        let admitted = crate::admit_current_block(len, 1);
        let v = vec![0u8; len];
        let served = stats().blocks_served > before.blocks_served;
        let owned = matches!(owner_of(v.as_ptr()), Some(Owner::Scope(id)) if id == scope.id());
        drop(v);
        admitted == Ok(true) && served && owned
    });
    let st = scope.stats();
    let closed = scope.close();
    ensure!(ok, "the exact allocation is served from the admitted block of the current scope");
    ensure!(st.overdraft_pages == 0, "no overdraft");
    ensure!(matches!(closed, Closed::Released { .. }), "released");
    Ok(())
}

fn scope_owns_its_allocations() -> CaseResult {
    let base = stats();
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    let buf = ExactBuf::admit(&s, 100_000, N).map_err(|_| "admit")?;
    ensure!(owner_of(buf.as_slice().as_ptr()) == Some(Owner::Scope(s.id())), "buffer page owned by the scope");
    ensure!(s.stats().live >= 100_000, "live counts the buffer");
    ensure!(stats().in_use_pages > base.in_use_pages, "pages taken from the pool");
    drop(buf);
    ensure!(s.stats().live == 0, "live back to zero");
    ensure!(matches!(s.close(), Closed::Released { .. }), "released");
    let after = stats();
    ensure!(after.in_use_pages == base.in_use_pages, "every page returned");
    ensure!(after.free_pages == base.free_pages && after.held_pages == base.held_pages, "holds returned");
    Ok(())
}

fn free_routes_to_owner() -> CaseResult {
    let a = Scope::open(R, 4096).map_err(|_| "open a")?;
    let b = Scope::open(R2, 4096).map_err(|_| "open b")?;
    let v = a.enter(|| Box::new([1u8; 512]));
    ensure!(a.stats().live == 512, "a holds the box");
    b.enter(|| drop(v));
    ensure!(a.stats().live == 0, "free went to the owner");
    ensure!(b.stats().allocs == 0 && b.stats().live == 0, "b untouched");
    ensure!(matches!(a.close(), Closed::Released { .. }) && matches!(b.close(), Closed::Released { .. }), "both released");
    Ok(())
}

fn quarantine_until_last_free() -> CaseResult {
    let base = stats();
    let s = Scope::open(R, 4096).map_err(|_| "open")?;
    let v = s.enter(|| Box::new([0u8; 256]));
    let closed = s.close();
    ensure!(matches!(closed, Closed::Quarantined { live: 256, .. }), "quarantined with the live box");
    let mid = stats();
    ensure!(mid.quarantined_scopes == base.quarantined_scopes + 1, "quarantine counted");
    ensure!(mid.quarantines == base.quarantines + 1, "leak recorded");
    ensure!(mid.pinned_pages > base.pinned_pages, "quarantined pages are pinned");
    ensure!(v[0] == 0, "value still valid after close");
    drop(v);
    let after = stats();
    ensure!(after.quarantined_scopes == base.quarantined_scopes, "released at the last free");
    ensure!(after.in_use_pages == base.in_use_pages && after.free_pages == base.free_pages, "pages returned");
    ensure!(after.quarantine_releases == base.quarantine_releases + 1, "release counted");
    Ok(())
}

fn realloc_charges_owner() -> CaseResult {
    let a = Scope::open(R, 4096).map_err(|_| "open a")?;
    a.reserve(1 << 20).map_err(|_| "reserve a")?;
    let mut v: Vec<u8> = a.enter(|| Vec::with_capacity(16));
    let b = Scope::open(R2, 0).map_err(|_| "open b")?;
    b.enter(|| v.try_reserve_exact(1 << 20)).map_err(|_| "grow")?;
    ensure!(a.stats().live >= 1 << 20, "growth charged to the owner");
    ensure!(b.stats().live == 0 && b.stats().used_pages == 1, "current scope untouched");
    ensure!(owner_of(v.as_ptr()) == Some(Owner::Scope(a.id())), "still owned by a");
    drop(v);
    ensure!(matches!(a.close(), Closed::Released { .. }) && matches!(b.close(), Closed::Released { .. }), "released");
    Ok(())
}

fn admission_retry_then_capacity() -> CaseResult {
    let part = stats().limit_pages - stats().glue_budget_pages;
    match Scope::open(R, bytes_for_pages(part + 1)) {
        Err(r) if r.kind == RefusalKind::Capacity => {}
        _ => return Err("a need beyond the scope partition is Capacity"),
    }
    let avail = available();
    let big = Scope::open(R, bytes_for_pages(avail)).map_err(|_| "a need of exactly the free pages fits")?;
    ensure!(available() == 0, "big holds every free page");
    match Scope::open(R2, 0) {
        Err(r) if r.kind == RefusalKind::Retry => {}
        _ => return Err("pages held by an open scope give Retry"),
    }
    drop(big);
    let s = Scope::open(R2, 0).map_err(|_| "admitted once big closed")?;
    drop(s);
    Ok(())
}

fn one_byte_short_refuses_without_allocating() -> CaseResult {
    let avail = available();
    let fit = bytes_for_pages(avail);
    let before = stats().allocs;
    ensure!(Scope::open(R, fit + 1).is_err(), "one byte more than the pool holds refuses");
    ensure!(stats().allocs == before, "the refusal allocated nothing");
    let s = Scope::open(R, fit).map_err(|_| "the exact fit is admitted")?;
    drop(s);
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    let before = stats().allocs;
    match Ingest::admit(&s, Some(1001), 1000, N) {
        Err(r) if r.reason == Reason::Bound => {}
        _ => return Err("declared one byte over the bound refuses"),
    }
    ensure!(stats().allocs == before, "bound refusal allocated nothing");
    let mut ok = Ingest::admit(&s, Some(1000), 1000, N).map_err(|_| "declared at the bound is admitted")?;
    ok.push(&[0u8; 1000]).map_err(|_| "fills")?;
    let before = stats().allocs;
    ensure!(ok.push(&[0u8; 1]).is_err(), "one byte past the declared length refuses");
    ensure!(stats().allocs == before, "overflow refusal allocated nothing");
    drop(ok);
    drop(s);
    Ok(())
}

fn refusals_allocate_nothing() -> CaseResult {
    let s = Scope::open(R, 4096).map_err(|_| "open")?;
    let mut buf = ExactBuf::admit(&s, 8, E).map_err(|_| "admit")?;
    let mut bv = BoundedVec::<u32>::admit(&s, 1, E).map_err(|_| "admit vec")?;
    bv.try_push(1).map_err(|_| "push")?;
    let short = Ingest::admit(&s, Some(16), 16, N).map_err(|_| "ingest")?;
    let chunk = ExactBuf::admit(&s, 4, E).map_err(|_| "chunk")?;
    let mut cs = ChunkSink::new(chunk, 6, |_b: &[u8]| Ok(()), |_| {});
    let before = stats().allocs;
    ensure!(Scope::open(R2, usize::MAX / 2).is_err(), "open refusal");
    ensure!(s.reserve(usize::MAX / 2).is_err(), "reserve refusal");
    ensure!(s.admit_block(page_limit() as usize * PAGE, 1).is_err(), "block refusal");
    ensure!(buf.put(&[0; 9]).is_err(), "overflow refusal");
    ensure!(bv.try_push(2).is_err(), "full vec refusal");
    ensure!(short.finish().is_err(), "truncation refusal");
    ensure!(cs.put(&[0; 7]).is_err(), "chunk sink overflow refusal");
    ensure!(Ingest::admit(&s, Some(2), 1, N).is_err(), "bound refusal");
    ensure!(stats().allocs == before, "no refusal allocated");
    drop((buf, bv, cs));
    drop(s);
    Ok(())
}

fn overdraft_recorded_in_enforce() -> CaseResult {
    set_mode(Mode::Enforce);
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    let before = stats().overdrafts;
    let mut v: Vec<u8> = Vec::new();
    s.enter(|| v.try_reserve_exact(4 * PAGE)).map_err(|_| "enforce mode serves an overdraft from free pages")?;
    ensure!(stats().overdrafts > before, "overdraft counted");
    ensure!(s.stats().overdraft_pages > 0, "overdraft pages on the scope");
    ensure!(route_stats(R.id).overdrafts > 0, "overdraft on the route");
    drop(v);
    ensure!(matches!(s.close(), Closed::Released { .. }), "released");
    Ok(())
}

fn strict_refuses_overdraft_and_unscoped() -> CaseResult {
    set_mode(Mode::Strict);
    let _ = strict::take();
    let (unscoped, overdraft, small) = {
        let _armed = strict::arm();
        let mut v: Vec<u8> = Vec::new();
        let unscoped = v.try_reserve(64).is_err();
        let s = Scope::open(R, 1024).map_err(|_| "open")?;
        let mut w: Vec<u8> = Vec::new();
        let overdraft = s.enter(|| w.try_reserve_exact(4 * PAGE)).is_err();
        let mut x: Vec<u8> = Vec::new();
        let small = s.enter(|| x.try_reserve_exact(512)).is_ok();
        drop((w, x));
        drop(s);
        (unscoped, overdraft, small)
    };
    let viol = strict::take();
    set_mode(Mode::Enforce);
    ensure!(unscoped, "an allocation outside every scope is refused");
    ensure!(overdraft, "an overdraft is refused");
    ensure!(small, "an allocation within the reservation is served");
    ensure!(viol.unscoped >= 1 && viol.overdrafts >= 1, "both violations recorded");
    ensure!(stats().unscoped_denied >= 1 && stats().overdrafts_denied >= 1, "denials counted");
    Ok(())
}

fn exhaustion_reported_not_aborted() -> CaseResult {
    set_mode(Mode::Enforce);
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    let want = page_limit() as usize * PAGE;
    let before = stats().alloc_failures;
    let mut v: Vec<u8> = Vec::new();
    ensure!(s.enter(|| v.try_reserve_exact(want)).is_err(), "more than the pool fails cleanly");
    let st = stats();
    ensure!(st.alloc_failures == before + 1, "failure counted once");
    let f = st.last_failure;
    ensure!(f.size == want && f.route == R.id && !f.glue, "failure names the size and route");
    ensure!(route_stats(R.id).alloc_failures > 0, "failure on the route");
    drop(s);
    Ok(())
}

fn observe_records_without_refusing() -> CaseResult {
    set_mode(Mode::Observe);
    let before = stats();
    let s = Scope::open(R, page_limit() as usize * PAGE);
    set_mode(Mode::Enforce);
    let s = s.map_err(|_| "observe mode admits")?;
    let after = stats();
    ensure!(after.observed_refusals == before.observed_refusals + 1, "would-be refusal recorded");
    ensure!(after.observed_excess_pages > before.observed_excess_pages, "the excess is recorded, not held");
    drop(s);
    Ok(())
}

struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Poll::Pending
        }
    }
}

fn scoped_futures_interleave() -> CaseResult {
    let a = Scope::open(RA, 4096).map_err(|_| "open a")?;
    let b = Scope::open(RA2, 4096).map_err(|_| "open b")?;
    let body = || async {
        let x = Box::new(1u64);
        YieldOnce(false).await;
        let y = Box::new(2u64);
        (owner_of(&*x), owner_of(&*y))
    };
    let mut fa = pin!(a.scoped(body()).map_err(|_| "a has a deadline")?);
    let mut fb = pin!(b.scoped(body()).map_err(|_| "b has a deadline")?);
    let mut cx = Context::from_waker(Waker::noop());
    ensure!(fa.as_mut().poll(&mut cx).is_pending() && fb.as_mut().poll(&mut cx).is_pending(), "both suspend");
    let Poll::Ready(ra) = fa.as_mut().poll(&mut cx) else { return Err("a completes") };
    let Poll::Ready(rb) = fb.as_mut().poll(&mut cx) else { return Err("b completes") };
    ensure!(ra == (Some(Owner::Scope(a.id())), Some(Owner::Scope(a.id()))), "a's allocations in a");
    ensure!(rb == (Some(Owner::Scope(b.id())), Some(Owner::Scope(b.id()))), "b's allocations in b");
    ensure!(a.stats().allocs >= 2 && b.stats().allocs >= 2, "both counted");
    ensure!(matches!(a.close(), Closed::Released { .. }) && matches!(b.close(), Closed::Released { .. }), "released");
    Ok(())
}

fn enter_boxes_future_in_scope() -> CaseResult {
    let base = stats();
    let released = route_stats(R.id).released;
    let e = enter(RA, 4096, |_| async {
        let v = Box::new([3u8; 64]);
        YieldOnce(false).await;
        v[0] as u32
    })
    .map_err(|_| "admitted")?;
    let mut e = pin!(e);
    let mut cx = Context::from_waker(Waker::noop());
    let mut out = None;
    for _ in 0..4 {
        if let Poll::Ready(v) = e.as_mut().poll(&mut cx) {
            out = Some(v);
            break;
        }
    }
    let Some((v, closed)) = out else { return Err("completes") };
    ensure!(v == 3, "output");
    ensure!(matches!(closed, Closed::Released { .. }), "scope released after the future");
    ensure!(route_stats(R.id).released == released + 1, "route release counted");
    ensure!(stats().in_use_pages == base.in_use_pages, "pages returned");
    Ok(())
}

/// A deterministic mixed workload: many small and medium allocations with
/// interleaved frees, one growing vector and one large block, admitted by
/// footprint.
fn workload(s: &Scope, seed: u32) -> CaseResult {
    let mut x = seed.wrapping_mul(2_654_435_761) | 1;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x
    };
    let large = 64 * 1024 + (next() as usize % (512 * 1024));
    let block = ExactBuf::admit(s, large, E).map_err(|_| "admit large")?;
    // At most 64 held vectors of up to 2 KiB, their holder, and a `u32`
    // vector growing to 16 Ki entries.
    let f = Footprint::ZERO
        .allocs(2048, 1, 64)
        .alloc(64 * size_of::<Vec<u8>>(), align_of::<Vec<u8>>())
        .growth(16 * 1024 * 4, 4);
    s.admit_footprint(f).map_err(|_| "reserve control")?;
    s.enter(|| {
        let mut held: Vec<Vec<u8>> = Vec::with_capacity(64);
        let mut grow: Vec<u32> = Vec::new();
        for _ in 0..200 {
            let n = 1 + next() as usize % 2048;
            if held.len() == held.capacity() || next() % 3 == 0 {
                if !held.is_empty() {
                    let i = next() as usize % held.len();
                    held.swap_remove(i);
                }
            } else {
                held.push(vec![0u8; n]);
            }
            if grow.len() < 16 * 1024 {
                grow.push(n as u32);
            }
        }
    });
    drop(block);
    Ok(())
}

fn repeated_scopes_leave_memory_flat() -> CaseResult {
    let base = stats();
    let mut settled = None;
    for i in 0..300u32 {
        let s = Scope::open(R, 0).map_err(|_| "open")?;
        workload(&s, i % 10)?;
        ensure!(s.stats().overdraft_pages == 0, "workload stays within its footprint");
        ensure!(matches!(s.close(), Closed::Released { .. }), "every scope released");
        let st = stats();
        ensure!(st.in_use_pages == base.in_use_pages, "pool pages back to idle after each scope");
        ensure!(st.free_pages == base.free_pages, "holds back to idle");
        ensure!(st.quarantined_scopes == 0, "no quarantine");
        if i == 9 {
            settled = Some((st.high_water_pages, st.memory_bytes));
        }
        if let Some((hw, mem)) = settled
            && i > 9
        {
            ensure!(st.high_water_pages == hw, "high-water flat after the first cycle");
            ensure!(st.memory_bytes == mem, "memory size flat after the first cycle");
        }
    }
    Ok(())
}

fn segment_trim_returns_pages() -> CaseResult {
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    s.reserve(8 << 20).map_err(|_| "reserve")?;
    let v: Vec<u8> = s.enter(|| vec![1u8; 8 << 20]);
    let peak = s.stats().used_pages;
    drop(v);
    let after = s.stats();
    ensure!(after.used_pages < peak && after.used_pages <= 3, "trimmed pages leave the heap");
    ensure!(after.held_pages + after.used_pages == after.reserved_pages, "and stay held for the scope");
    let w: Vec<u8> = s.enter(|| vec![2u8; 8 << 20]);
    ensure!(s.stats().overdraft_pages == 0, "re-growth inside the reservation needs no free page");
    ensure!(w[999] == 2, "the scope still allocates after a trim");
    drop(w);
    ensure!(matches!(s.close(), Closed::Released { .. }), "released");
    Ok(())
}

#[repr(align(64))]
struct Wide([u8; 64]);

fn block_bound_holds() -> CaseResult {
    for len in [1, 100, 4096, PAGE - 1, PAGE, PAGE + 1, 3 * PAGE + 17, 1 << 20, (1 << 20) + 7, 5_000_000] {
        let s = Scope::open(R, 0).map_err(|_| "open")?;
        let b = ExactBuf::admit(&s, len, E).map_err(|_| "admit")?;
        let st = s.stats();
        ensure!(st.overdraft_pages == 0 && st.used_pages <= st.reserved_pages, "one block fits its block_bytes");
        drop(b);
        drop(s);
    }
    let s = Scope::open_footprint(R, Footprint::ZERO.allocs(48, 1, 64).alloc(64 * 8, 8)).map_err(|_| "open")?;
    let small: Vec<Box<[u8; 48]>> = s.enter(|| (0..64).map(|_| Box::new([0u8; 48])).collect());
    let a = BoundedVec::<Wide>::admit(&s, 1000, E).map_err(|_| "aligned block")?;
    let b = ExactBuf::admit(&s, 300_000, E).map_err(|_| "second block")?;
    let c = BoundedVec::<u64>::admit(&s, 70_000, E).map_err(|_| "third block")?;
    let st = s.stats();
    ensure!(st.overdraft_pages == 0 && st.peak_pages <= st.reserved_pages, "mixed blocks fit their reservation");
    ensure!((a.as_slice().as_ptr() as usize).is_multiple_of(64) || a.is_empty(), "alignment honoured");
    let _ = a.first().map(|w| w.0[0]);
    drop((small, a, b, c));
    drop(s);
    Ok(())
}

/// The report buffer, in static memory (the case's frame stays small).
static REPORT: crate::sys::Racy<[u8; 4096]> = crate::sys::Racy::new([0; 4096]);

fn stats_report_allocation_free() -> CaseResult {
    // SAFETY: the cases run on one thread.
    let raw = unsafe { &mut *REPORT.get() };
    let before = stats().allocs;
    let mut w = FmtBuf::new(raw);
    stats().write_json(&mut w).map_err(|_| "report fits its buffer")?;
    let n = w.len();
    ensure!(stats().allocs == before, "report allocated nothing");
    ensure!(n > 20 && raw.starts_with(b"{\"model\":\""), "report shape");
    let mut tiny = [0u8; 8];
    ensure!(stats().write_json(&mut FmtBuf::new(&mut tiny)).is_err(), "overflow is an error, not truncation");
    Ok(())
}

fn encode_record(e: &mut Enc<impl Sink>, i: u32) -> Result<(), crate::Refusal> {
    e.str("attempt-receipt")?;
    e.u16(1)?;
    e.raw(&[i as u8; 96])?;
    e.u8(1)?;
    e.u8(1)?;
    e.raw(&[!(i as u8); 32])
}

fn two_pass_encode_and_hash() -> CaseResult {
    use sha2::Digest;
    let len = encoded_len(|e| encode_record(e, 7)).map_err(|_| "count")?;
    ensure!(len == 151, "receipt-shaped record is 151 bytes");
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    let mut buf = ExactBuf::admit(&s, len as usize, E).map_err(|_| "admit")?;
    let mut h = Hash::new();
    let mut e = Enc(Tee(&mut buf, &mut h));
    encode_record(&mut e, 7).map_err(|_| "encode")?;
    let got = h.finish();
    let want: [u8; 32] = sha2::Sha256::digest(buf.finish().map_err(|_| "exact")?).into();
    ensure!(got == want, "streamed digest equals one-shot digest");
    // A chunked stream reassembles to the same bytes.
    let total = 10 * len as usize;
    let mut out = ExactBuf::admit(&s, total, E).map_err(|_| "out")?;
    let chunk = ExactBuf::admit(&s, 64, E).map_err(|_| "chunk")?;
    let mut flushes = 0;
    let mut cs = ChunkSink::new(
        chunk,
        total as u64,
        |b: &[u8]| {
            flushes += 1;
            out.put(b)
        },
        |_| {},
    );
    for i in 0..10 {
        encode_record(&mut Enc(&mut cs), i).map_err(|_| "chunked encode")?;
    }
    ensure!(cs.finish().map_err(|_| "finish")? == total as u64, "total");
    ensure!(flushes == total.div_ceil(64), "one flush per chunk");
    let mut again = Count::default();
    for i in 0..10 {
        encode_record(&mut Enc(&mut again), i).map_err(|_| "count again")?;
    }
    ensure!(out.len() as u64 == again.bytes, "reassembled length");
    ensure!(out.as_slice()[..15 + 4] == *b"\x0f\x00\x00\x00attempt-receipt", "reassembled bytes");
    drop((buf, out));
    drop(s);
    Ok(())
}

fn pull_from(src: &[u8], step: usize) -> impl FnMut(&mut [core::mem::MaybeUninit<u8>]) -> usize + '_ {
    let mut off = 0;
    move |dst| {
        let n = dst.len().min(step).min(src.len() - off);
        for (d, b) in dst[..n].iter_mut().zip(&src[off..off + n]) {
            d.write(*b);
        }
        off += n;
        n
    }
}

fn ingest_bounds() -> CaseResult {
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    let src: [u8; 1000] = core::array::from_fn(|i| i as u8);
    let mut ing = Ingest::admit(&s, Some(src.len()), 4096, N).map_err(|_| "admit")?;
    ing.pull(pull_from(&src, 64)).map_err(|_| "pull")?;
    let body = ing.finish().map_err(|_| "complete body")?;
    ensure!(body.as_slice() == src, "pulled bytes equal the source");
    let mut short = Ingest::admit(&s, Some(10), 4096, N).map_err(|_| "admit short")?;
    short.push(&[1; 9]).map_err(|_| "push")?;
    match short.finish() {
        Err(r) if r.reason == Reason::Truncated && r.kind == RefusalKind::Retry => {}
        _ => return Err("short network body refused as truncated, Retry"),
    }
    let mut open_len = Ingest::admit(&s, None, 16, N).map_err(|_| "undeclared")?;
    open_len.push(&[1; 16]).map_err(|_| "up to the bound")?;
    ensure!(open_len.push(&[1]).is_err(), "never buffered past the bound");
    drop((body, open_len));
    drop(s);
    Ok(())
}

fn bounded_vec_never_grows() -> CaseResult {
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    let mut v = BoundedVec::<u64>::admit(&s, 10, E).map_err(|_| "admit")?;
    let p = v.as_slice().as_ptr();
    for i in 0..10 {
        v.try_push(i).map_err(|_| "within capacity")?;
    }
    ensure!(v.try_push(10).is_err(), "full");
    ensure!(v.capacity() == 10 && v.as_slice().as_ptr() == p, "never reallocated");
    drop(v);
    drop(s);
    Ok(())
}

fn hex_roundtrip() -> CaseResult {
    let d: [u8; 32] = core::array::from_fn(|i| (i * 37) as u8);
    let h = hex::encode32(&d);
    ensure!(hex::decode32(&h) == Some(d), "roundtrip");
    ensure!(hex::decode32(&h[..63]).is_none(), "length checked");
    let mut bad = h;
    bad[5] = b'g';
    ensure!(hex::decode32(&bad).is_none(), "digits checked");
    Ok(())
}

// ------------------------------------------------------------------ review findings

/// Finding 1 (glue unbounded) and finding 3 (glue changes refusal kinds):
/// glue has its own fixed partition, scopes never take it, glue never leaves
/// it, a scope refusal's kind does not depend on glue usage, and entries are
/// admitted by allocation-free tickets.
fn f_glue_partition_is_bounded_and_admitted() -> CaseResult {
    let st = stats();
    let part = st.limit_pages - st.glue_budget_pages;
    // A scope admitted for every free page leaves the glue partition alone.
    let big = Scope::open(R, bytes_for_pages(available())).map_err(|_| "fill the scope partition")?;
    let g: Box<[u8; 1024]> = Box::new([5u8; 1024]);
    ensure!(owner_of(&*g) == Some(Owner::Glue), "glue still served from its partition");
    ensure!(stats().glue_used_pages <= stats().glue_budget_pages, "glue inside its partition");
    // While glue is in use, a need that fits the idle partition is Retry (an
    // open scope holds it), never Capacity.
    match Scope::open(R2, bytes_for_pages(part - 1)) {
        Err(r) if r.kind == RefusalKind::Retry => {}
        _ => return Err("a fitting need is Retry while glue is in use"),
    }
    drop(g);
    drop(big);
    // Glue never grows into scope pages: an allocation past the partition
    // fails cleanly (fallible API) and is counted.
    let mut v: Vec<u8> = Vec::new();
    let over = (st.glue_budget_pages as usize + 1) * PAGE;
    ensure!(v.try_reserve_exact(over).is_err(), "glue past its partition is refused");
    ensure!(stats().last_failure.glue, "the failure is attributed to glue");
    // Tickets: allocation-free admission, refused at the derived limit.
    let before = stats().allocs;
    let t = glue::Ticket::admit().map_err(|_| "first ticket")?;
    ensure!(stats().allocs == before, "a ticket allocates nothing");
    ensure!(stats().tickets_in_flight >= 1, "ticket counted");
    drop(t);
    ensure!(model::GLUE_TICKETS >= 1, "at least one ticket");
    Ok(())
}

/// A future that is never ready (an awaited promise that never settles).
struct Never;
impl Future for Never {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }
}

/// A future ready once its flag is set (a promise settling late).
struct Flag<'a>(&'a core::cell::Cell<bool>);
impl Future for Flag<'_> {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.0.get() { Poll::Ready(()) } else { Poll::Pending }
    }
}

/// Finding 2: an invocation whose future stops being polled past its route's
/// deadline is counted as abandoned (pinned, against the quarantine budget);
/// a late poll recovers it; dropping it (the drop guard) reclaims it.
fn f_abandoned_scope_counted_recovered_reclaimed() -> CaseResult {
    let route = RA;
    let t0 = stats().clock_ms + 1_000_000;
    let _ = tick(t0);
    let base = stats();
    let settled = core::cell::Cell::new(false);
    let e = enter(route, 4096, |_| async {
        let v = Box::new([7u8; 128]);
        Flag(&settled).await;
        v[0]
    })
    .map_err(|_| "admitted")?;
    let mut e = core::pin::pin!(e);
    let mut cx = Context::from_waker(Waker::noop());
    ensure!(e.as_mut().poll(&mut cx).is_pending(), "awaits");
    let _ = tick(t0 + 29_000);
    ensure!(stats().abandoned_scopes == base.abandoned_scopes, "not abandoned before its deadline");
    let _ = tick(t0 + 31_000);
    let mid = stats();
    ensure!(mid.abandoned_scopes == base.abandoned_scopes + 1, "counted as abandoned past its deadline");
    ensure!(mid.pinned_pages > base.pinned_pages, "its pages are pinned");
    ensure!(route_stats(R.id).abandoned >= 1, "abandonment on the route");
    // The promise settles late: the next poll recovers and completes it.
    settled.set(true);
    let Poll::Ready((v, closed)) = e.as_mut().poll(&mut cx) else { return Err("completes after a late settle") };
    ensure!(v == 7 && matches!(closed, Closed::Released { .. }), "late completion releases the scope");
    let after = stats();
    ensure!(after.abandon_recovered == base.abandon_recovered + 1, "recovery counted");
    ensure!(after.abandoned_scopes == base.abandoned_scopes && after.pinned_pages == base.pinned_pages, "unpinned");
    // Abandoned for good, then dropped by the platform: the drop guard closes it.
    let e2 = enter(route, 4096, |_| async {
        let _v = Box::new([1u8; 64]);
        Never.await;
    })
    .map_err(|_| "admitted again")?;
    let mut e2 = Box::pin(e2);
    ensure!(e2.as_mut().poll(&mut cx).is_pending(), "awaits forever");
    let _ = tick(t0 + 70_000);
    ensure!(stats().abandoned_scopes == base.abandoned_scopes + 1, "abandoned");
    drop(e2);
    let end = stats();
    ensure!(end.abandoned_scopes == base.abandoned_scopes, "dropped: no longer abandoned");
    ensure!(end.abandon_reclaimed == base.abandon_reclaimed + 1, "reclaim counted");
    ensure!(end.free_pages == base.free_pages, "every page returned");
    Ok(())
}

/// Finding 3: pages pinned by quarantined scopes make a fitting need
/// `Poisoned` (recycle), not `Retry` forever; the pool reports it.
fn f_pinned_pages_poison_not_retry() -> CaseResult {
    let st = stats();
    let largest = (model::LARGEST_ROUTE_BYTES.div_ceil(PAGE) + 1) as u32;
    // Pool: the glue partition, the largest route, and 8 more pages, so the
    // quarantine budget is small.
    let free = shrink_to_free(largest + 8)?;
    ensure!(free >= largest + 8, "precondition: free pages");
    let budget = stats().quarantine_budget_pages;
    // Pin pages with quarantined scopes until past the budget.
    let mut pins: [Option<Box<u8>>; 64] = [const { None }; 64];
    let mut n = 0;
    while stats().pinned_pages <= budget && n < pins.len() {
        let s = Scope::open(R, 0).map_err(|_| "pin scope")?;
        pins[n] = Some(s.enter(|| Box::new(1u8)));
        ensure!(matches!(s.close(), Closed::Quarantined { .. }), "quarantined");
        n += 1;
    }
    ensure!(matches!(crate::health(), Health::Poisoned { .. }), "pinned past the budget: poisoned");
    ensure!(!stats().healthy, "stats report it");
    // The largest route no longer fits beside the pinned pages: Poisoned.
    match Scope::open(R2, largest as usize * PAGE - 2 * PAGE) {
        Err(r) if r.kind == RefusalKind::Poisoned => {}
        Err(_) => return Err("a need blocked only by pinned pages is Poisoned"),
        Ok(_) => return Err("precondition: the need should not fit"),
    }
    // An open scope holding pages instead gives Retry.
    for p in pins.iter_mut() {
        *p = None;
    }
    reset_latches();
    let avail = available();
    let holder = Scope::open(R, bytes_for_pages(avail)).map_err(|_| "holder")?;
    match Scope::open(R2, 0) {
        Err(r) if r.kind == RefusalKind::Retry => {}
        _ => return Err("pages held by an open scope give Retry"),
    }
    drop(holder);
    let _ = st;
    Ok(())
}

/// Finding 5: admission by dlmalloc footprint covers many small allocations
/// that a byte count would undercount (16 Ki 16-byte boxes).
fn f_footprint_covers_small_allocations() -> CaseResult {
    const COUNT: usize = 16 * 1024;
    let f = Footprint::ZERO.allocs(16, 8, COUNT);
    ensure!(f.chunks() >= COUNT * 16 * 2 || size_of::<usize>() == 4, "a 16-byte box takes a 32-byte chunk on 64-bit");
    set_mode(Mode::Strict);
    let r = strict::run(R, f.bytes(), |s| {
        let mut v: BoundedVec<Box<[u8; 16]>> = BoundedVec::admit(s, COUNT, E).map_err(|_| "admit holder")?;
        for _ in 0..COUNT {
            v.try_push(Box::new([0u8; 16])).map_err(|_| "push")?;
        }
        Ok::<(), &'static str>(())
    });
    set_mode(Mode::Enforce);
    match r {
        Ok(Ok(())) => Ok(()),
        Ok(Err(m)) => Err(m),
        Err(_) => Err("the footprint admission holds every box with no overdraft"),
    }
}

/// Finding 6: admission holds a contiguous run per block, so an allocation
/// inside an admitted reservation succeeds even after other scopes fragment
/// the pool; when no run exists, fragmentation caused by pinned pages is
/// `Poisoned`, not `Retry`.
fn f_admission_holds_contiguous_runs() -> CaseResult {
    shrink_to_free(64)?;
    // Admitted first: four pages, physically held.
    let x = Scope::open(R2, bytes_for_pages(4)).map_err(|_| "four-page scope")?;
    // Then the rest of the pool is fragmented by quarantined one-page pins,
    // every other one freed.
    let mut pins: [Option<Box<u8>>; 96] = [const { None }; 96];
    let mut n = 0;
    while n < pins.len() {
        let Ok(s) = Scope::open(R, 0) else { break };
        pins[n] = Some(s.enter(|| Box::new(1u8)));
        let _ = s.close();
        n += 1;
    }
    for (i, p) in pins.iter_mut().enumerate() {
        if i % 2 == 0 {
            *p = None;
        }
    }
    let frag = stats();
    ensure!(frag.largest_free_run < 4, "precondition: fragmented");
    // The allocation inside x's reservation needs three contiguous pages.
    let mut v: Vec<u8> = Vec::new();
    ensure!(x.enter(|| v.try_reserve_exact(2 * PAGE)).is_ok(), "allocation inside the reservation succeeds");
    ensure!(x.stats().overdraft_pages == 0, "inside its held run");
    drop(v);
    // A new four-page need finds no free run, but closing x would free one:
    // Retry, not Poisoned.
    match Scope::open(R2, bytes_for_pages(4)) {
        Err(r) if r.kind == RefusalKind::Retry => {}
        _ => return Err("a run that an open scope holds is Retry"),
    }
    drop(x);
    // With x closed, only the pins block an eight-page run: Poisoned.
    match Scope::open(R2, bytes_for_pages(8)) {
        Err(r) if r.kind == RefusalKind::Poisoned && r.reason == Reason::Contiguity => {}
        _ => return Err("fragmentation caused by pinned pages is Poisoned/Contiguity"),
    }
    for p in pins.iter_mut() {
        *p = None;
    }
    Ok(())
}

/// Finding 7: quarantine keeps only pages that hold live bytes (the heap is
/// trimmed and held pages go back), not the whole reservation.
fn f_quarantine_trims_to_live_pages() -> CaseResult {
    let base = stats();
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    s.reserve(3 << 20).map_err(|_| "reserve")?;
    let keep = s.enter(|| {
        let b = Box::new(1u8);
        let v = vec![1u8; 3 << 20];
        drop(v);
        b
    });
    let closed = s.close();
    let Closed::Quarantined { live: 1, pages } = closed else { return Err("quarantined with one live byte") };
    ensure!(pages <= 2, "only the pages holding the live byte stay");
    ensure!(stats().pinned_pages - base.pinned_pages <= 2, "pinned pages are the live ones");
    drop(keep);
    ensure!(stats().free_pages == base.free_pages, "released at the last free");
    Ok(())
}

/// Finding 7 (ingest): a stream longer than its bound or declared length is
/// refused, with or without a declared length, never accepted truncated.
fn f_ingest_refuses_overlong_streams() -> CaseResult {
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    let src = [7u8; 32];
    let mut a = Ingest::admit(&s, None, 16, N).map_err(|_| "undeclared")?;
    match a.pull(pull_from(&src, 8)) {
        Err(r) if r.reason == Reason::Bound && r.kind == RefusalKind::Invalid => {}
        _ => return Err("an undeclared stream past its bound is refused Bound/Invalid"),
    }
    let mut b = Ingest::admit(&s, Some(10), 4096, N).map_err(|_| "declared")?;
    match b.pull(pull_from(&src, 8)) {
        Err(r) if r.reason == Reason::Overflow && r.kind == RefusalKind::Invalid => {}
        _ => return Err("a stream past its declared length is refused Overflow/Invalid"),
    }
    // Exactly at each limit is accepted; one byte over is not.
    let mut c = Ingest::admit(&s, None, 32, N).map_err(|_| "at bound")?;
    c.pull(pull_from(&src, 5)).map_err(|_| "exactly the bound is accepted")?;
    let c = c.finish().map_err(|_| "finish")?;
    ensure!(c.len() == 32, "whole body");
    let src33 = [7u8; 33];
    let mut d = Ingest::admit(&s, None, 32, N).map_err(|_| "at bound")?;
    ensure!(d.pull(pull_from(&src33, 64)).is_err(), "one byte over the bound is refused");
    let mut e = Ingest::admit(&s, Some(32), 4096, N).map_err(|_| "declared 32")?;
    ensure!(e.pull(pull_from(&src33, 64)).is_err(), "one byte over the declared length is refused");
    drop((a, b, c, d, e));
    drop(s);
    Ok(())
}

/// Finding 6 (hierarchy): a child is carved from the pages its parent lent,
/// so a step under a parent that lent its step budget is admitted while the
/// pool is empty; its pages go back to the parent's lent pages at close.
fn f_child_carves_from_parent() -> CaseResult {
    let avail = available();
    let parent = Scope::open(R, 0).map_err(|_| "parent")?;
    let own = parent.stats().reserved_pages;
    parent.lend((avail - own) as usize * PAGE).map_err(|_| "parent lends every other free page")?;
    let ps = parent.stats();
    ensure!(available() == 0, "pool empty");
    ensure!(ps.lent_pages == avail - own, "lent pages counted");
    let child = parent.child(R2, 0).map_err(|_| "child carved from the lent pages")?;
    ensure!(child.stats().carved_pages >= 2, "child pages carved");
    ensure!(parent.stats().reserved_pages == ps.reserved_pages - child.stats().carved_pages, "parent's claim shrinks");
    let grand = child.child(R, 0);
    // The child lent nothing: a grandchild falls back to the empty pool,
    // which only the chain itself occupies (no wait frees it): Capacity.
    ensure!(matches!(grand, Err(r) if r.kind == RefusalKind::Capacity), "no lent pages, no pool: Capacity");
    let v = child.enter(|| Box::new([9u8; 256]));
    drop(v);
    ensure!(matches!(child.close(), Closed::Released { .. }), "child released");
    let after = parent.stats();
    ensure!(after.reserved_pages == ps.reserved_pages && after.held_pages == ps.held_pages, "pages back to the parent");
    ensure!(after.lent_pages == ps.lent_pages, "as lent pages");
    ensure!(available() == 0, "not to the pool");
    // A child that lends in turn carves grandchildren.
    let c2 = parent.child(R2, 0).map_err(|_| "child")?;
    c2.lend(child_bytes(0)).map_err(|_| "the child lends from the parent's lent pages")?;
    let g2 = c2.child(R, 0).map_err(|_| "grandchild carved from the child")?;
    ensure!(g2.stats().carved_pages >= 2, "grandchild carved");
    drop(g2);
    drop(c2);
    ensure!(parent.stats().reserved_pages == ps.reserved_pages, "nested pages back to the parent");
    drop(parent);
    ensure!(available() == avail, "and to the pool when the parent closes");
    Ok(())
}

/// Finding 12: pass B re-hashes the record and refuses before the signature
/// when a row changed after pass A.
fn f_recheck_refuses_changed_rows() -> CaseResult {
    use sha2::Digest;
    let rows: [[u8; 8]; 4] = [[1; 8], [2; 8], [3; 8], [4; 8]];
    // Pass A: the root over the rows.
    let mut a = Hash::new();
    for r in &rows {
        a.put(r).map_err(|_| "hash")?;
    }
    let root = a.finish();
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    let total = 4 + rows.len() * 8 + 64;
    // Unchanged rows: the whole envelope is written.
    let mut out = ExactBuf::admit(&s, total, E).map_err(|_| "out")?;
    let mut b = Recheck::new(&mut out, R.id);
    b.raw(&[0xEE; 4]).map_err(|_| "header")?;
    for r in &rows {
        b.put(r).map_err(|_| "row")?;
    }
    b.finish(&root, &[0x55; 64]).map_err(|_| "unchanged rows pass")?;
    ensure!(out.finish().is_ok(), "complete envelope");
    // A row changed between the passes: refused before the tail.
    let mut out2 = ExactBuf::admit(&s, total, E).map_err(|_| "out2")?;
    let mut b2 = Recheck::new(&mut out2, R.id);
    b2.raw(&[0xEE; 4]).map_err(|_| "header")?;
    for (i, r) in rows.iter().enumerate() {
        let changed = if i == 2 { [9u8; 8] } else { *r };
        b2.put(&changed).map_err(|_| "row")?;
    }
    match b2.finish(&root, &[0x55; 64]) {
        Err(r) if r.kind == RefusalKind::Defect => {}
        _ => return Err("a changed row is a Defect before the signature"),
    }
    ensure!(out2.finish().is_err(), "the stream stays short of its declared length");
    let _ = sha2::Sha256::digest(b"");
    drop((out, out2));
    drop(s);
    Ok(())
}

/// Finding 10: in Observe mode an enormous reserve neither wraps a counter
/// nor panics under the pool lock; the excess is recorded separately.
fn f_observe_overflow_is_recorded_not_wrapped() -> CaseResult {
    set_mode(Mode::Observe);
    let a = Scope::open(R, 0).map_err(|_| "open a")?;
    let b = Scope::open(R2, 0).map_err(|_| "open b")?;
    let before = stats();
    let r = b.reserve(usize::MAX / 2);
    let r2 = b.reserve(usize::MAX);
    let after = stats();
    set_mode(Mode::Enforce);
    ensure!(r.is_ok() && r2.is_ok(), "observe mode records instead of refusing");
    ensure!(after.held_pages == before.held_pages, "nothing is held for an impossible reserve");
    ensure!(after.observed_excess_pages > before.observed_excess_pages, "the excess is recorded");
    ensure!(after.free_pages == before.free_pages, "free pages unchanged (no wrap)");
    drop((b, a));
    Ok(())
}

/// Finding 8: refusal kinds follow the buffer's cause.
fn f_refusal_kind_by_cause() -> CaseResult {
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    let mut net = ExactBuf::admit(&s, 4, Kinds::NETWORK).map_err(|_| "net")?;
    let mut plan = ExactBuf::admit(&s, 4, Kinds::PLAN).map_err(|_| "plan")?;
    let mut enc = ExactBuf::admit(&s, 4, Kinds::ENCODER).map_err(|_| "enc")?;
    ensure!(matches!(net.put(&[0; 5]), Err(r) if r.kind == RefusalKind::Invalid), "network overflow is Invalid");
    ensure!(matches!(plan.put(&[0; 5]), Err(r) if r.kind == RefusalKind::Capacity), "plan overflow is Capacity");
    ensure!(matches!(enc.put(&[0; 5]), Err(r) if r.kind == RefusalKind::Defect), "encoder overflow is Defect");
    ensure!(matches!(net.finish(), Err(r) if r.kind == RefusalKind::Retry), "network truncation is Retry");
    ensure!(matches!(enc.finish(), Err(r) if r.kind == RefusalKind::Defect), "encoder short total is Defect");
    let chunk = ExactBuf::admit(&s, 4, Kinds::ENCODER).map_err(|_| "chunk")?;
    let mut flushed = 0usize;
    let aborted = core::cell::Cell::new(0u32);
    let mut cs = ChunkSink::new(
        chunk,
        10,
        |b: &[u8]| {
            flushed += b.len();
            Ok(())
        },
        |_| aborted.set(aborted.get() + 1),
    );
    cs.put(&[1; 6]).map_err(|_| "put")?;
    match cs.finish() {
        Err(r) if r.kind == RefusalKind::Defect && r.reason == Reason::Truncated => {}
        _ => return Err("a short encoder total is a Defect"),
    }
    ensure!(flushed == 4, "the short tail is never flushed");
    ensure!(aborted.get() == 1, "and the stream is aborted, never left short");
    drop((net, plan, enc));
    drop(s);
    Ok(())
}

/// Finding 5 (JS strings): a JS string's transient `passStringToWasm0`
/// growth is admitted as the original and the grown block.
fn f_js_string_admission() -> CaseResult {
    let len = 100_000;
    set_mode(Mode::Strict);
    let r = strict::run(R, 0, |s| {
        s.admit_js_string(len).map_err(|_| "admit")?;
        // Model `passStringToWasm0`: malloc(len), realloc(len + 3·len), shrink.
        let mut v: Vec<u8> = Vec::new();
        v.try_reserve_exact(len).map_err(|_| "malloc")?;
        v.try_reserve_exact(4 * len).map_err(|_| "realloc")?;
        v.shrink_to(len);
        Ok::<(), &'static str>(())
    });
    match r {
        Ok(Ok(())) => {}
        Ok(Err(m)) => return Err(m),
        Err(_) => return Err("the string's transient growth fits its admission"),
    }
    // The real sequence grows to `offset + 3 × rest`, at most `4 × len` but
    // rarely exactly: the grown block moves to the smallest admitted run that
    // holds it.
    let r = strict::run(R, 0, |s| {
        s.admit_js_string(len).map_err(|_| "admit")?;
        let mut v: Vec<u8> = Vec::new();
        v.try_reserve_exact(len).map_err(|_| "malloc")?;
        v.extend_from_slice(&[b'a'; 17]);
        v.try_reserve_exact(17 + 3 * (len - 17) - 17).map_err(|_| "realloc")?;
        v.shrink_to(len - 3);
        Ok::<(), &'static str>(())
    });
    set_mode(Mode::Enforce);
    match r {
        Ok(Ok(())) => Ok(()),
        Ok(Err(m)) => Err(m),
        Err(_) => Err("the string's inexact growth fits its admission"),
    }
}

// ------------------------------------------------------------------ second review

/// Restores the pool to the model's full limit.
fn restore_limit() {
    let full = model::pool_pages(stats().static_bytes).min(MAX_PAGES) as u32;
    let _ = set_page_limit(full);
}

fn try_vec(s: &ScopeRef, len: usize) -> Option<Vec<u8>> {
    let mut v: Vec<u8> = Vec::new();
    s.enter(|| v.try_reserve_exact(len)).ok().map(|_| v)
}

/// Finding 2-1 (design §2.6, guarantee 2): two admitted blocks with another
/// scope opened between their admissions, the second allocated first. Each
/// block is served from its own run, so neither takes the other's.
fn f2_out_of_order_blocks_are_served() -> CaseResult {
    let a_len = 8 * PAGE - 256;
    let b_len = 5 * PAGE - 256;
    for strict_mode in [true, false] {
        set_mode(if strict_mode { Mode::Strict } else { Mode::Enforce });
        let s = Scope::open(R, 0).map_err(|_| "open")?;
        s.admit_block(a_len, 1).map_err(|_| "admit a")?;
        let other = Scope::open(R2, 0).map_err(|_| "other")?;
        s.admit_block(b_len, 1).map_err(|_| "admit b")?;
        if !strict_mode {
            // Enforce with no free page left: any overdraft would fail.
            shrink_to_free(0)?;
        }
        let before = stats().overdrafts;
        let b = try_vec(&s, b_len);
        let a = try_vec(&s, a_len);
        let ok = a.is_some() && b.is_some();
        let clean = stats().overdrafts == before && s.stats().overdraft_pages == 0;
        drop((a, b));
        drop((s, other));
        set_mode(Mode::Enforce);
        restore_limit();
        ensure!(ok, "an admitted block allocated after another admitted block was refused");
        ensure!(clean, "and served without an overdraft");
    }
    Ok(())
}

const FUZZ_TRIALS: u64 = 4000;
const MAXA: usize = 1024;
const MAXB: usize = 32;

/// The fuzz plan, in static memory (keeps the case's frame small).
struct Plan {
    n: usize,
    size: [usize; MAXA],
    align: [usize; MAXA],
    ptr: [*mut u8; MAXA],
    order: [u16; MAXA],
}

static PLAN: crate::sys::Racy<Plan> =
    crate::sys::Racy::new(Plan { n: 0, size: [0; MAXA], align: [0; MAXA], ptr: [core::ptr::null_mut(); MAXA], order: [0; MAXA] });

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }
    fn align(&mut self) -> usize {
        match self.below(100) {
            0..=59 => 1 << self.below(4),
            60..=89 => 16,
            90..=94 => 32,
            95..=97 => 64,
            _ => 4096,
        }
    }
    fn small(&mut self, big: bool) -> (usize, usize) {
        let size = match self.below(100) {
            0..=39 => self.range(1, 64),
            40..=79 => self.range(65, 2048),
            80..=94 => self.range(2049, 40_000),
            _ if big => self.range(40_001, 140_000),
            _ => self.range(2049, 40_000),
        } as usize;
        (size, self.align())
    }
}

/// One fuzz trial: blockers fragment the pool, then a subject scope opens
/// with a `Footprint` control set, adds block and footprint admissions (with
/// blockers opened between them so runs are not adjacent), and makes exactly
/// the admitted allocations, all live at once, in random order, in Strict
/// mode. Returns `Err(seed)` if an admitted allocation was refused.
fn fuzz_trial(seed: u64, plan: &mut Plan) -> Result<bool, u64> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let full = model::pool_pages(stats().static_bytes).min(MAX_PAGES) as u32;
    let _ = set_page_limit(full);
    reset_latches();
    set_mode(Mode::Strict);
    let mut blockers: [Option<Scope>; MAXB] = [const { None }; MAXB];
    let nb = rng.below(20) as usize;
    for b in blockers.iter_mut().take(nb) {
        *b = Scope::open(R2, bytes_for_pages(rng.range(2, 14) as u32)).ok();
    }
    for b in blockers.iter_mut().take(nb) {
        if rng.below(2) == 0 {
            *b = None;
        }
    }
    plan.n = 0;
    let mut f = Footprint::ZERO;
    for _ in 0..rng.below(80) {
        let (s, a) = rng.small(true);
        f = f.alloc(s, a);
        (plan.size[plan.n], plan.align[plan.n]) = (s, a);
        plan.n += 1;
    }
    let Ok(scope) = Scope::open_footprint(R, f) else {
        set_mode(Mode::Enforce);
        return Ok(false);
    };
    for _ in 0..rng.below(6) {
        if rng.below(2) == 0
            && let Some(slot) = blockers.iter_mut().find(|b| b.is_none())
        {
            *slot = Scope::open(R2, bytes_for_pages(rng.range(2, 6) as u32)).ok();
        }
        if rng.below(2) == 0 {
            let len = rng.range(1, 600_000) as usize;
            let a = rng.align();
            if plan.n >= MAXA || scope.admit_block(len, a).is_err() {
                break;
            }
            (plan.size[plan.n], plan.align[plan.n]) = (len, a);
            plan.n += 1;
        } else {
            let k = rng.range(1, 120) as usize;
            if plan.n + k > MAXA {
                break;
            }
            let mut f = Footprint::ZERO;
            for j in 0..k {
                let (s, a) = rng.small(false);
                f = f.alloc(s, a);
                (plan.size[plan.n + j], plan.align[plan.n + j]) = (s, a);
            }
            if scope.admit_footprint(f).is_err() {
                break;
            }
            plan.n += k;
        }
    }
    for i in 0..plan.n {
        plan.order[i] = i as u16;
    }
    for i in (1..plan.n).rev() {
        let j = rng.below(i as u64 + 1) as usize;
        plan.order.swap(i, j);
    }
    let mut failed = false;
    for k in 0..plan.n {
        let i = plan.order[k] as usize;
        let Ok(l) = core::alloc::Layout::from_size_align(plan.size[i], plan.align[i]) else { return Err(seed) };
        let p = scope.enter(|| unsafe { std::alloc::alloc(l) });
        plan.ptr[i] = p;
        if p.is_null() {
            failed = true;
            break;
        }
    }
    for i in 0..plan.n {
        let p = core::mem::replace(&mut plan.ptr[i], core::ptr::null_mut());
        if !p.is_null()
            && let Ok(l) = core::alloc::Layout::from_size_align(plan.size[i], plan.align[i])
        {
            unsafe { std::alloc::dealloc(p, l) };
        }
    }
    let released = matches!(scope.close(), Closed::Released { .. });
    drop(blockers);
    set_mode(Mode::Enforce);
    if failed || !released { Err(seed) } else { Ok(true) }
}

/// Finding 2-1, randomized: 4,000 seeds of fragmenting blockers, control,
/// block and footprint admissions, every admitted allocation live at once
/// in random order, Strict mode (an overdraft is refused).
fn f2_admitted_allocations_never_fail_fuzz() -> CaseResult {
    // SAFETY: the cases run on one thread.
    let plan = unsafe { &mut *PLAN.get() };
    let mut ran = 0u64;
    for seed in 1..=FUZZ_TRIALS {
        match fuzz_trial(seed, plan) {
            Ok(true) => ran += 1,
            Ok(false) => {}
            Err(_) => return Err("an admitted allocation was refused (or its scope not released) in a fuzz trial"),
        }
    }
    ensure!(ran >= FUZZ_TRIALS * 9 / 10, "most trials ran");
    Ok(())
}

/// Finding 2-7: a handle that outlives its scope never allocates into a
/// later scope that reuses its page, nor into its own quarantined scope;
/// its admissions are refused, not trapped.
fn f2_stale_handle_never_reaches_another_scope() -> CaseResult {
    let mut cx = Context::from_waker(Waker::noop());
    let mut slot: Option<ScopeRef> = None;
    let e = enter(RA, 4096, |h| {
        slot = Some(h);
        async {}
    })
    .map_err(|_| "enter")?;
    let mut e = pin!(e);
    let Poll::Ready(((), Closed::Released { .. })) = e.as_mut().poll(&mut cx) else { return Err("completes, released") };
    let stale = slot.take().ok_or("handle escaped")?;
    let b = Scope::open(R2, 4096).map_err(|_| "open b")?;
    let before = stats();
    let v = try_vec(&stale, 1000);
    let in_b = b.stats().live != 0 || b.stats().allocs != 0;
    let refused_admission = matches!(stale.admit_block(100, 1), Err(r) if r.kind == RefusalKind::Defect && r.reason == Reason::Stale);
    let after = stats();
    let closed = b.close();
    ensure!(v.is_none(), "a stale handle's allocation fails");
    ensure!(!in_b, "and lands nowhere in the later scope");
    ensure!(matches!(closed, Closed::Released { .. }), "the later scope is not quarantined for it");
    ensure!(refused_admission, "a stale admission is refused Defect/Stale, not a trap");
    ensure!(after.stale_allocs == before.stale_allocs + 1, "the stale allocation is recorded");
    ensure!(stale.stats().status == 0, "the stale handle reports its scope gone");
    // A handle into its own quarantined scope.
    let e = enter(RA, 4096, |h| {
        slot = Some(h);
        async { Box::new(1u8) }
    })
    .map_err(|_| "enter 2")?;
    let mut e = pin!(e);
    let Poll::Ready((kept, Closed::Quarantined { .. })) = e.as_mut().poll(&mut cx) else { return Err("quarantined") };
    let h = slot.take().ok_or("handle escaped")?;
    let pinned = stats().pinned_pages;
    let grown = try_vec(&h, 1 << 20);
    let pinned_after = stats().pinned_pages;
    let refused = h.reserve(64).is_err();
    drop(kept);
    ensure!(grown.is_none(), "a quarantined scope takes no new allocation");
    ensure!(pinned_after == pinned, "its pinned pages do not grow");
    ensure!(refused, "its admissions are refused");
    Ok(())
}

/// Finding 2-5: a need blocked only by the requester's own pages (or its
/// ancestors') is `Capacity`, never `Retry` (retrying the same invocation
/// reproduces it); a need other open scopes block is `Retry`.
fn f2_need_blocked_by_own_pages_is_capacity() -> CaseResult {
    let part = available();
    let half = part / 2 + 2;
    let s = Scope::open(R, bytes_for_pages(half)).map_err(|_| "open")?;
    let r = s.admit_block(half as usize * PAGE - 256, 1);
    drop(s);
    match r {
        Err(e) if e.kind == RefusalKind::Capacity => {}
        Err(e) if e.kind == RefusalKind::Retry => return Err("a need blocked only by the scope's own pages is Retry"),
        Err(_) => return Err("other refusal"),
        Ok(()) => return Err("precondition: should not fit"),
    }
    // Another open scope blocks it: Retry.
    let own = (part - 20) / 2;
    let s = Scope::open(R, bytes_for_pages(own)).map_err(|_| "open own")?;
    let other = Scope::open(R2, bytes_for_pages(20)).map_err(|_| "other")?;
    let r = s.admit_block((own as usize + 9) * PAGE - 256, 1);
    drop(s);
    drop(other);
    ensure!(matches!(r, Err(e) if e.kind == RefusalKind::Retry), "pages of another open scope give Retry");
    // A child under a parent holding the pool, with nothing lent: Capacity.
    let parent = Scope::open(R, bytes_for_pages(available())).map_err(|_| "parent")?;
    let c = parent.child(R2, 0);
    drop(parent);
    ensure!(matches!(c, Err(e) if e.kind == RefusalKind::Capacity), "a child blocked by its own ancestors is Capacity");
    Ok(())
}

/// Finding 2-6: a child carves only pages its parent lent, never the
/// parent's admitted blocks or heap pages, so the parent's own admitted
/// allocations succeed while the child is open (Strict, and Enforce with no
/// free page).
fn f2_child_carves_only_lent_pages() -> CaseResult {
    let blk = 256 * 1024;
    for strict_mode in [true, false] {
        set_mode(if strict_mode { Mode::Strict } else { Mode::Enforce });
        let p = Scope::open(R, 4096).map_err(|_| "parent")?;
        p.admit_block(blk, 1).map_err(|_| "the parent's own block")?;
        let held = p.stats().held_pages;
        p.lend(child_bytes(200 * 1024)).map_err(|_| "lend")?;
        let c = p.child(R2, 200 * 1024).map_err(|_| "a step's child")?;
        let carved = c.stats().carved_pages;
        let held_during = p.stats().held_pages;
        if !strict_mode {
            shrink_to_free(0)?;
        }
        let v = try_vec(&p, blk);
        let clean = p.stats().overdraft_pages == 0;
        drop(v);
        drop(c);
        let lent_back = p.stats().lent_pages;
        drop(p);
        // A parent that lent nothing loses nothing to a child.
        let untouched = if strict_mode {
            let q = Scope::open(R, 4096).map_err(|_| "second parent")?;
            q.admit_block(blk, 1).map_err(|_| "its block")?;
            let held_q = q.stats().held_pages;
            let c2 = q.child(R2, 0).map_err(|_| "child from the pool")?;
            c2.stats().carved_pages == 0 && q.stats().held_pages == held_q
        } else {
            true
        };
        set_mode(Mode::Enforce);
        restore_limit();
        ensure!(carved > 0, "the child is carved from the lent pages");
        ensure!(held_during == held, "the parent's block stays held while the child is open");
        ensure!(clean, "the parent's admitted block is served with no overdraft");
        ensure!(lent_back == pages_of_bytes(child_bytes(200 * 1024)), "the child's pages go back as lent pages");
        ensure!(untouched, "a child of a parent that lent nothing comes from the pool");
    }
    Ok(())
}

fn pages_of_bytes(b: usize) -> u32 {
    b.div_ceil(PAGE) as u32
}

/// Finding 2-4: a child's deadline is its own route's when declared (a
/// `waitUntil` continuation's 30 s, even under a parent with none or a
/// shorter one), else its parent's.
fn f2_child_takes_its_own_route_deadline() -> CaseResult {
    let cont = AsyncRoute::new(252, "selftest-cont", limits::WAIT_UNTIL_MS);
    let short = AsyncRoute::new(250, "selftest-1s", 1_000);
    let t0 = stats().clock_ms + 50_000_000;
    let _ = tick(t0);
    let base = stats().abandoned_scopes;
    // Under a parent with no deadline (a synchronous scope).
    let p = Scope::open(R, 0).map_err(|_| "parent")?;
    p.lend(child_bytes(0)).map_err(|_| "lend")?;
    let c = p.child(cont, 0).map_err(|_| "child")?;
    ensure!(c.has_deadline(), "the continuation has a deadline");
    drop(p);
    let _ = tick(t0 + 29_000);
    ensure!(stats().abandoned_scopes == base, "within its own 30 s");
    let _ = tick(t0 + 31_000);
    ensure!(stats().abandoned_scopes == base + 1, "abandoned after its own 30 s");
    drop(c);
    // Under a 1 s parent.
    let t1 = t0 + 100_000;
    let _ = tick(t1);
    let p = Scope::open(short, 0).map_err(|_| "1 s parent")?;
    p.lend(2 * child_bytes(0)).map_err(|_| "lend")?;
    let c = p.child(cont, 0).map_err(|_| "30 s child")?;
    let inherits = p.child(R2, 0).map_err(|_| "sync child")?;
    ensure!(inherits.has_deadline() && inherits.stats().deadline_ms == p.stats().deadline_ms, "a sync child inherits");
    let _ = tick(t1 + 2_000);
    // The parent and the inheriting child are abandoned; the 30 s child is not.
    ensure!(stats().abandoned_scopes == base + 2, "only the parent and its inheriting child after 2 s");
    let _ = tick(t1 + 31_000);
    ensure!(stats().abandoned_scopes == base + 3, "the continuation after its own 30 s");
    drop(inherits);
    drop(c);
    drop(p);
    reset_latches();
    Ok(())
}

/// Finding 2-3: a scope that holds a future always has a deadline. A
/// synchronous scope refuses `scoped`; an `enter` handler's route is an
/// `AsyncRoute` (a zero deadline does not compile), so a never-settling await
/// is abandoned and pinned, never an open scope forever.
fn f2_async_scopes_always_have_deadlines() -> CaseResult {
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    ensure!(matches!(s.scoped(async {}), Err(r) if r.reason == Reason::Deadline), "a sync scope refuses a future");
    drop(s);
    let route = AsyncRoute::new(250, "selftest-await", 2_000);
    let t0 = stats().clock_ms + 3_000_000;
    let _ = tick(t0);
    let base = stats();
    let e = enter(route, 4096, |_| async {
        let _v = Box::new([1u8; 64]);
        Never.await;
    })
    .map_err(|_| "enter")?;
    let mut e = Box::pin(e);
    let mut cx = Context::from_waker(Waker::noop());
    ensure!(e.as_mut().poll(&mut cx).is_pending(), "awaits");
    let _ = tick(t0 + 1_999);
    ensure!(stats().abandoned_scopes == base.abandoned_scopes, "not before its deadline");
    let _ = tick(t0 + 2_001);
    let mid = stats();
    ensure!(mid.abandoned_scopes == base.abandoned_scopes + 1 && mid.pinned_pages > base.pinned_pages, "abandoned and pinned");
    drop(e);
    reset_latches();
    Ok(())
}

/// Finding 2-4: `wait_until` admits the continuation (ticket, child scope,
/// future box) before it is spawned, with the 30 s limit from the clock at
/// spawn; the spawner's task box is glue, and the continuation outlives its
/// parent.
fn f2_wait_until_continuation() -> CaseResult {
    const WU: AsyncRoute = AsyncRoute::new(253, "selftest-wait-until", limits::WAIT_UNTIL_MS);
    let flag = &core::cell::Cell::new(false);
    let tickets0 = stats().tickets_in_flight;
    let t0 = stats().clock_ms + 7_000_000;
    let parent = Scope::open(RA, 0).map_err(|_| "parent")?;
    parent.lend(child_bytes(0) + block_bytes(256, 16)).map_err(|_| "lend")?;
    let cont = wait_until(&parent, WU, 0, t0, |h| async move {
        Flag(flag).await;
        h.id()
    })
    .map_err(|_| "admitted")?;
    let st = cont.scope().ok_or("child")?.stats();
    ensure!(st.deadline_ms == t0 + limits::WAIT_UNTIL_MS, "deadline from the clock at spawn");
    ensure!(st.carved_pages > 0, "carved from the parent's lent pages");
    ensure!(stats().tickets_in_flight == tickets0 + 1, "its own glue ticket");
    let mut task: Option<Box<crate::Continuation<_>>> = None;
    let mut glue_box = false;
    parent.enter(|| {
        cont.spawn(|c| {
            let b = Box::new(c);
            glue_box = owner_of(&*b) == Some(Owner::Glue);
            task = Some(b);
        })
    });
    ensure!(glue_box, "the task box is glue, not the spawning scope's");
    drop(parent);
    let mut cx = Context::from_waker(Waker::noop());
    let t = task.as_mut().ok_or("spawned")?;
    ensure!(Pin::new(&mut **t).poll(&mut cx).is_pending(), "pending after its parent closed");
    flag.set(true);
    let Poll::Ready((_, closed)) = Pin::new(&mut **t).poll(&mut cx) else { return Err("completes") };
    ensure!(matches!(closed, Closed::Released { .. }), "released");
    drop(task);
    ensure!(stats().tickets_in_flight == tickets0, "ticket released");
    Ok(())
}

/// Finding 2-2: a poisoned pool names its cause (the route pinning the most
/// pages, and how), the key of the host's durable stop rule.
fn f2_poison_cause_names_the_pinning_route() -> CaseResult {
    let largest = (model::LARGEST_ROUTE_BYTES.div_ceil(PAGE) + 1) as u32;
    shrink_to_free(largest + 8)?;
    let budget = stats().quarantine_budget_pages;
    let mut pins: [Option<Box<u8>>; 64] = [const { None }; 64];
    let mut n = 0;
    while stats().pinned_pages <= budget && n < pins.len() {
        let s = Scope::open(R2, 0).map_err(|_| "pin scope")?;
        pins[n] = Some(s.enter(|| Box::new(1u8)));
        let _ = s.close();
        n += 1;
    }
    let h = crate::health();
    for p in pins.iter_mut() {
        *p = None;
    }
    match h {
        Health::Poisoned { cause, .. } if cause.route == R2.id && cause.kind == PinKind::Quarantined && cause.pages > 0 => Ok(()),
        Health::Poisoned { .. } => Err("the cause names the pinning route and kind"),
        Health::Healthy => Err("pinned past the budget: poisoned"),
    }
}

/// Finding 2-12: the postmortem an abort hook copies out names the scope and
/// route current at the trap.
fn f2_postmortem_names_the_current_route() -> CaseResult {
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    let before = stats().allocs;
    let pm = s.enter(postmortem);
    ensure!(stats().allocs == before, "allocation-free");
    ensure!(pm.scope == s.id() && pm.route == Some(R.id), "names the current scope and route");
    let w = pm.words();
    ensure!(w[0] == 1 && w[1] == s.id() as u32 && w[2] == R.id as u32 + 1, "and so do its words");
    drop(s);
    ensure!(postmortem().route.is_none(), "outside a scope: none");
    Ok(())
}

/// Finding 2-5: an invocation admitting more than the model's
/// `largest_route_bytes` is recorded as a defect, once per scope.
fn f2_route_bound_excess_is_recorded() -> CaseResult {
    let before = stats().route_bound_exceeded;
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    let have = s.stats().reserved_pages as usize * PAGE;
    let more = model::LARGEST_ROUTE_BYTES.saturating_sub(have) + PAGE;
    s.reserve(more).map_err(|_| "room for the excess")?;
    s.reserve(64).map_err(|_| "a second admission")?;
    let after = stats().route_bound_exceeded;
    let max = route_stats(R.id).max_invocation_bytes;
    drop(s);
    ensure!(after == before + 1, "recorded once");
    ensure!(max > model::LARGEST_ROUTE_BYTES as u64, "with its size");
    Ok(())
}

/// Finding 2-8: a stream with a declared length is never left short: a
/// refused or abandoned `ChunkSink`, and a `Recheck` mismatch, abort it.
fn f2_short_streams_are_aborted() -> CaseResult {
    use sha2::Digest;
    let s = Scope::open(R, 0).map_err(|_| "open")?;
    let aborts = core::cell::Cell::new(0u32);
    {
        // Dropped unfinished after an early return.
        let chunk = ExactBuf::admit(&s, 4, E).map_err(|_| "chunk")?;
        let mut cs = ChunkSink::new(chunk, 10, |_: &[u8]| Ok(()), |_| aborts.set(aborts.get() + 1));
        cs.put(&[1; 6]).map_err(|_| "put")?;
    }
    ensure!(aborts.get() == 1, "dropped short: aborted");
    {
        let chunk = ExactBuf::admit(&s, 4, E).map_err(|_| "chunk")?;
        let mut cs = ChunkSink::new(chunk, 10, |_: &[u8]| Ok(()), |_| aborts.set(aborts.get() + 1));
        ensure!(cs.put(&[1; 11]).is_err(), "overflow refused");
    }
    ensure!(aborts.get() == 2, "overflow: aborted once");
    {
        let chunk = ExactBuf::admit(&s, 4, E).map_err(|_| "chunk")?;
        let cs = ChunkSink::new(chunk, 4, |_: &[u8]| Ok(()), |_| aborts.set(aborts.get() + 1));
        let mut rc = Recheck::new(cs, R.id);
        rc.put(&[7; 4]).map_err(|_| "row")?;
        let root: [u8; 32] = sha2::Sha256::digest([8u8; 4]).into();
        ensure!(rc.finish(&root, &[]).is_err(), "mismatch refused");
    }
    ensure!(aborts.get() == 3, "a Recheck mismatch aborts the stream");
    {
        let chunk = ExactBuf::admit(&s, 4, E).map_err(|_| "chunk")?;
        let mut cs = ChunkSink::new(chunk, 4, |_: &[u8]| Ok(()), |_| aborts.set(aborts.get() + 1));
        cs.put(&[1; 4]).map_err(|_| "put")?;
        cs.finish().map_err(|_| "complete")?;
    }
    ensure!(aborts.get() == 3, "a complete stream is not aborted");
    drop(s);
    Ok(())
}

/// Finding 2-10: on wasm32 the linked stack is the model's `stack_bytes`
/// and its painted high-water is measured.
fn f2_stack_is_measured() -> CaseResult {
    let st = stats();
    if cfg!(target_arch = "wasm32") {
        ensure!(st.stack_bytes == model::STACK_BYTES, "the linked stack is the model's stack_bytes");
        ensure!(st.stack_high_water > 0 && st.stack_high_water < st.stack_bytes, "the high-water is measured");
    } else {
        ensure!(st.stack_high_water == 0 && st.stack_bytes == 0, "natively the OS owns the stack");
    }
    Ok(())
}
