//! `ScopeRef::child_physical`: refused in every mode, `Observe` included,
//! when the pool cannot serve the footprint even scattered, classed by
//! cause; a plain `child` in `Observe` is admitted with its excess counted.
//! Run with one test thread: the cases assert on isolate-wide counters.

use slop_ninja_arena::{ArenaAlloc, Mode, PAGE, RefusalKind, Route, Scope, reset_latches, set_mode, stats};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

const PARENT: Route = Route::new(51, "parent");
const OTHER: Route = Route::new(52, "other");
const STEP: Route = Route::new(53, "step");

#[test]
fn observe_refuses_a_physical_child_the_pool_cannot_serve() {
    reset_latches();
    set_mode(Mode::Observe);
    let parent = Scope::open(PARENT, 64 * 1024).expect("parent");
    // Another open scope holds pages a wait would free.
    let other = Scope::open(OTHER, 8 * PAGE).expect("other");
    let free = stats().free_pages as usize;
    // More than is free, less than free plus the other scope's pages: Retry.
    let r = parent.child_physical(STEP, (free + 2) * PAGE).err().expect("refused in Observe");
    assert_eq!(r.kind, RefusalKind::Retry, "{r:?}");
    // A plain child of the same size is admitted in Observe (counted).
    let observed = stats().observed_refusals;
    let c = parent.child(STEP, (free + 2) * PAGE).expect("plain child admitted in Observe");
    assert!(stats().observed_refusals > observed, "the excess is counted");
    drop(c);
    // A footprint within the free pages is admitted physically.
    let ok = parent.child_physical(STEP, (free / 2) * PAGE).expect("servable");
    drop(ok);
    drop(other);
    // With nothing else open, more than the free pages is a Capacity
    // refusal: only the requester's own chain holds the rest.
    let free = stats().free_pages as usize;
    let r = parent.child_physical(STEP, (free + 2) * PAGE).err().expect("refused");
    assert_eq!(r.kind, RefusalKind::Capacity, "{r:?}");
    drop(parent);
    set_mode(Mode::Enforce);
}
