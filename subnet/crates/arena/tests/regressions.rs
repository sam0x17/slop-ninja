//! The arena review's reproductions (data/task-roots/testnet/workers-arena/
//! review/tests at 58d4de9d, on the retained `workers-arena` evidence branch), re-run against the fixed allocator. Each test
//! keeps the reviewed inputs and asserts the fixed behaviour.
//! Run with one test thread: they assert on isolate-wide counters.

use std::cell::RefCell;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use slop_ninja_arena::{
    ArenaAlloc, AsyncRoute, Closed, Mode, PAGE, RefusalKind, Route, SEGMENT_OVERHEAD, Scope, enter, glue, limits, model,
    reset_latches, set_mode, stats,
};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

const R: Route = Route::new(40, "review");
const RA: AsyncRoute = AsyncRoute::new(40, "review", limits::WAIT_UNTIL_MS);

fn bytes_for_pages(pages: u32) -> usize {
    (pages as usize - 1) * PAGE - SEGMENT_OVERHEAD
}

fn restore() {
    let _ = slop_ninja_arena::set_page_limit(slop_ninja_arena::MAX_PAGES as u32);
    let full = model::pool_pages(stats().static_bytes).min(slop_ninja_arena::MAX_PAGES) as u32;
    let _ = slop_ninja_arena::set_page_limit(full);
    reset_latches();
    set_mode(Mode::Enforce);
}

// ------------------------------------------------------------------ F-SELECT

struct JsHeld(#[allow(dead_code)] Rc<RefCell<Option<Waker>>>);

/// Reviewed: a dropped pending JsFuture (select + Delay) quarantined the
/// round scope's whole reservation until the promise settled. Fixed: the
/// quarantine keeps only pages holding live bytes (the heap is trimmed) and
/// is counted against the budget; it releases when the callbacks drop.
#[test]
fn a_timed_out_fetch_pins_only_its_live_pages() {
    restore();
    let base = stats();
    let mut js_callbacks: Option<JsHeld> = None;
    let slot = &mut js_callbacks;
    let e = enter(RA, 4096, move |_| async move {
        // A round's buffers, freed before the round ends.
        let shard = vec![1u8; 2 << 20];
        drop(shard);
        let inner: Rc<RefCell<Option<Waker>>> = Rc::new(RefCell::new(None));
        *slot = Some(JsHeld(inner.clone()));
        drop(inner);
        0u32
    })
    .unwrap();
    let mut e = Box::pin(e);
    let mut cx = Context::from_waker(Waker::noop());
    let Poll::Ready((_, closed)) = e.as_mut().poll(&mut cx) else { panic!("completes") };
    drop(e);
    let mid = stats();
    eprintln!("F-SELECT closed={closed:?} pinned_delta={}", mid.pinned_pages - base.pinned_pages);
    let Closed::Quarantined { pages, .. } = closed else { panic!("quarantined: {closed:?}") };
    assert!(pages <= 2, "only the pages holding the callbacks' state stay pinned");
    assert!(mid.pinned_pages - base.pinned_pages <= 2);
    drop(js_callbacks);
    let end = stats();
    assert_eq!(end.quarantined_scopes, base.quarantined_scopes);
    assert_eq!(end.free_pages, base.free_pages);
}

// ------------------------------------------------------------------ F-GLUE / F-KIND

/// Reviewed: with scopes holding every page but one headroom page, only 5 of
/// 8 concurrent 20 KiB glue allocations were served. Fixed: glue has its own
/// partition, so concurrent invocations' glue (within the ticket budget) is
/// served whatever the scopes hold.
#[test]
fn glue_is_served_from_its_partition_while_scopes_hold_the_pool() {
    restore();
    let big = Scope::open(R, bytes_for_pages(stats().free_pages)).expect("fill the scope partition");
    let n = (model::GLUE_TICKETS as usize).min(8);
    let mut tickets = Vec::new();
    let mut glue_bufs: Vec<Vec<u8>> = Vec::with_capacity(n);
    let per = model::GLUE_TICKET_BYTES.min(20 * 1024);
    for _ in 0..n {
        tickets.push(glue::Ticket::admit().expect("ticket"));
        let mut g = Vec::new();
        g.try_reserve_exact(per).expect("glue within its partition is served");
        glue_bufs.push(g);
    }
    let after = stats();
    eprintln!("F-GLUE served {n} x {per} B, glue pages {}/{}", after.glue_used_pages, after.glue_budget_pages);
    assert!(after.glue_used_pages <= after.glue_budget_pages);
    drop(glue_bufs);
    drop(tickets);
    drop(big);
}

/// Reviewed: a glue burst turned a need that fits the idle pool into
/// `Capacity`, and the burst's retained glue pages kept it so. Fixed: scope
/// admission never depends on glue usage.
#[test]
fn a_glue_burst_never_changes_scope_admission() {
    restore();
    let need = stats().free_pages;
    let mut glue_bufs: [Vec<u8>; 6] = Default::default();
    for g in glue_bufs.iter_mut() {
        g.try_reserve_exact(model::GLUE_TICKET_BYTES.min(60 * 1024)).expect("glue served");
    }
    let during = Scope::open(R, bytes_for_pages(need));
    assert!(during.is_ok(), "the whole free partition is admitted during the burst: {:?}", during.as_ref().err());
    drop(during);
    drop(glue_bufs);
    let after = Scope::open(R, bytes_for_pages(need));
    assert!(after.is_ok(), "and after it");
    drop(after);
    match Scope::open(R, bytes_for_pages(need + 1)) {
        Err(r) if r.kind == RefusalKind::Capacity || r.kind == RefusalKind::Retry => {}
        other => panic!("one page more than the partition holds: {:?}", other.map(|_| ())),
    }
}

/// Tickets are refused at the derived limit, allocation-free, as `Retry`.
#[test]
fn glue_tickets_refuse_at_the_derived_limit() {
    restore();
    let mut held = Vec::with_capacity(model::GLUE_TICKETS as usize);
    for _ in 0..model::GLUE_TICKETS {
        held.push(glue::Ticket::admit().expect("ticket"));
    }
    let before = stats().allocs;
    let r = glue::Ticket::admit();
    assert!(matches!(r, Err(e) if e.kind == RefusalKind::Retry && e.reason == slop_ninja_arena::Reason::Glue));
    assert_eq!(stats().allocs, before, "the refusal allocated nothing");
    drop(held);
    assert!(glue::Ticket::admit().is_ok(), "released tickets are reusable");
}
