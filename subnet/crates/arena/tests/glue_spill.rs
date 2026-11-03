//! The transitional `glue-spill` feature: glue segments the partition cannot
//! hold go to free scope pages, counted, and come back on free; nothing
//! spills in `Strict` mode or on an armed thread, so strict tests still fail
//! on any unscoped allocation. Built only with the feature
//! (`cargo test --features glue-spill --test glue_spill`).
//! Run with one test thread: the cases assert on isolate-wide counters.

use slop_ninja_arena::{ArenaAlloc, Mode, PAGE, Route, Scope, model, reset_latches, set_mode, stats, strict};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

const R: Route = Route::new(41, "spill");

fn restore() {
    reset_latches();
    set_mode(Mode::Enforce);
}

/// Several times the glue partition, so it cannot hold the segment.
fn beyond_glue() -> usize {
    (model::GLUE_BUDGET_PAGES as usize + 3) * PAGE
}

#[test]
fn unscoped_allocations_beyond_the_partition_spill_and_return() {
    restore();
    let before = stats();
    let mut v: Vec<u8> = Vec::new();
    v.try_reserve_exact(beyond_glue()).expect("spilled into free scope pages");
    v.resize(beyond_glue(), 7);
    let during = stats();
    assert!(during.glue_spills > before.glue_spills, "the spill is counted");
    assert!(during.glue_spill_pages as usize * PAGE >= beyond_glue(), "spilled pages are live");
    assert!(during.unscoped_allocs > before.unscoped_allocs, "it is still an unscoped allocation");
    assert_eq!(during.glue_used_pages, before.glue_used_pages, "the partition itself did not grow");
    // Scopes keep working beside the spilled glue.
    let s = Scope::open(R, 64 * 1024).expect("a scope opens beside spilled glue");
    let x = s.enter(|| vec![1u8; 4096]);
    drop(x);
    assert!(matches!(s.close(), slop_ninja_arena::Closed::Released { .. }));
    drop(v);
    let after = stats();
    assert_eq!(after.glue_spill_pages, 0, "spilled pages go back to the free pool");
    assert_eq!(after.free_pages, before.free_pages, "every spilled page is free again");
    assert!(after.glue_spill_peak_pages >= during.glue_spill_pages);
}

#[test]
fn strict_mode_never_spills() {
    restore();
    set_mode(Mode::Strict);
    let before = stats();
    let mut v: Vec<u8> = Vec::new();
    let refused = v.try_reserve_exact(beyond_glue()).is_err();
    set_mode(Mode::Enforce);
    assert!(refused, "strict mode refuses the segment instead of spilling");
    assert_eq!(stats().glue_spills, before.glue_spills);
}

#[test]
fn an_armed_thread_never_spills_and_records_the_violation() {
    restore();
    let before = stats();
    let _ = strict::take();
    let refused = {
        let _armed = strict::arm();
        let mut v: Vec<u8> = Vec::new();
        v.try_reserve_exact(beyond_glue()).is_err()
    };
    let violations = strict::take();
    assert!(refused, "an armed thread does not spill");
    assert!(violations.unscoped >= 1, "the unscoped allocation is a violation");
    assert_eq!(stats().glue_spills, before.glue_spills);
}

#[test]
fn a_refusal_spilled_glue_causes_is_retry() {
    // Spilled glue goes back to the pool when its allocation is freed, so a
    // scope the spill blocks waits (`Retry`); it is never a durable
    // `Capacity` pause.
    restore();
    let mut v: Vec<u8> = Vec::new();
    v.try_reserve_exact(beyond_glue()).expect("spilled into free scope pages");
    let during = stats();
    assert!(during.glue_spill_pages >= 2, "the spill holds scope pages");
    // One page more than is free: only the spilled pages stand in the way.
    let want = (during.free_pages as usize + 1) * PAGE;
    let r = Scope::open(R, want).err().expect("the spill blocks the scope");
    drop(v);
    assert_eq!(r.kind, slop_ninja_arena::RefusalKind::Retry, "{r:?}");
    let s = Scope::open(R, want.min((stats().free_pages as usize).saturating_sub(2) * PAGE)).expect("the freed spill admits it");
    drop(s);
}
