//! The coordinator migration review's arena findings (2026-11-01), re-run
//! against the fixed allocator. Run with one test thread (`.cargo/config.toml`
//! sets it): they assert on isolate-wide counters.
//!
//! - A scope that reads many objects one after another, each admitted as its
//!   own exact block of a different size, held one new run per object (a
//!   freed run stayed held for its exact layout only). Fixed: an admission
//!   re-keys a returned run of the scope that holds it.
//! - A refusal caused by an instance-lifetime home scope was classed `Retry`
//!   (waiting frees it), so a step blocked by it retried forever. Fixed: a
//!   scope marked permanent counts as never freed by waiting (`Capacity`).

use slop_ninja_arena::{ArenaAlloc, Closed, Mode, PAGE, RefusalKind, Route, SEGMENT_OVERHEAD, Scope, model, reset_latches, set_mode, stats};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

const R: Route = Route::new(41, "review2");
const R2: Route = Route::new(42, "review2-b");

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

#[test]
fn sequential_exact_blocks_of_distinct_sizes_reuse_one_run() {
    restore();
    let before = stats().blocks_reused;
    let s = Scope::open(R, 4096).expect("scope");
    let reserved_at_start = s.stats().reserved_pages;
    // 2,000 objects of distinct sizes (20,000 to about 94,000 bytes: one or
    // two pages each), each admitted, read, and dropped before the next.
    for i in 0..2_000usize {
        let len = 20_000 + i * 37;
        s.admit_block(len, 1).expect("block admitted");
        let v: Vec<u8> = s.enter(|| {
            let mut v = Vec::with_capacity(len);
            v.resize(len, 7u8);
            v
        });
        assert_eq!(v.len(), len);
        drop(v);
    }
    let st = s.stats();
    // The scope holds at most one run of each size class it needed (one and
    // two pages), not one per object.
    assert!(st.reserved_pages <= reserved_at_start + 3, "reserved {} pages after 2,000 reads (started at {reserved_at_start})", st.reserved_pages);
    assert!(stats().blocks_reused >= before + 1_990, "admissions re-keyed returned runs: {}", stats().blocks_reused - before);
    assert_eq!(st.overdraft_pages, 0, "every read was served from its admitted block");
    assert!(matches!(s.close(), Closed::Released { .. }));
    restore();
}

#[test]
fn a_refusal_caused_by_a_permanent_home_scope_is_capacity() {
    restore();
    let st = stats();
    let lim = st.glue_budget_pages + 64;
    assert!(slop_ninja_arena::set_page_limit(lim).is_ok(), "the test pool is free above its first 64 scope pages");

    // A scope that is open but not permanent: waiting may free its pages.
    let open = Scope::open(R, bytes_for_pages(40)).expect("40 pages");
    let Err(r) = Scope::open(R2, bytes_for_pages(30)) else { panic!("30 pages do not fit beside 40 of 64") };
    assert_eq!(r.kind, RefusalKind::Retry, "an ordinary open scope's pages are freed by waiting");
    assert!(matches!(open.close(), Closed::Released { .. }));

    // The same pages held by a home scope (instance lifetime): no wait frees them.
    let home = Scope::open(R, bytes_for_pages(40)).expect("40 pages");
    home.set_permanent();
    let Err(r) = Scope::open(R2, bytes_for_pages(30)) else { panic!("30 pages do not fit beside the home") };
    assert_eq!(r.kind, RefusalKind::Capacity, "a home scope's pages never free by waiting");
    assert!(matches!(home.close(), Closed::Released { .. }));
    restore();
}
