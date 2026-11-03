//! Native-only behaviour: threads, unwinding and the strict harness.
//! Run with one test thread.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;

use slop_ninja_arena::io::{ExactBuf, Kinds, Sink};
use slop_ninja_arena::strict::{self, StrictFailure};
use slop_ninja_arena::{ArenaAlloc, Closed, Mode, Owner, Route, Scope, owner_of, set_mode, stats, strict_test};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

const R: Route = Route::new(1, "native");
const R2: Route = Route::new(2, "native-b");

#[test]
fn current_scope_is_per_thread() {
    let a = Scope::open(R, 4096).unwrap();
    let (tx, rx) = mpsc::channel();
    let t = std::thread::spawn(move || {
        let glue = Box::new([0u8; 64]);
        let b = Scope::open(R2, 4096).unwrap();
        let mine = b.enter(|| Box::new([0u8; 64]));
        let owners = (owner_of(&*glue), owner_of(&*mine), b.id());
        drop(mine);
        drop(b);
        tx.send(owners).unwrap();
    });
    let here = a.enter(|| Box::new([0u8; 64]));
    t.join().unwrap();
    let (glue, theirs, b_id) = rx.recv().unwrap();
    assert_eq!(owner_of(&*here), Some(Owner::Scope(a.id())));
    assert_eq!(glue, Some(Owner::Glue), "another thread outside a scope allocates in glue");
    assert_eq!(theirs, Some(Owner::Scope(b_id)), "another thread's scope is its own");
    drop(here);
    assert!(matches!(a.close(), Closed::Released { .. }));
}

#[test]
fn free_on_another_thread_returns_to_the_owner() {
    let s = Scope::open(R, 4096).unwrap();
    let v = s.enter(|| Box::new([9u8; 512]));
    assert_eq!(s.stats().live, 512);
    std::thread::spawn(move || drop(v)).join().unwrap();
    assert_eq!(s.stats().live, 0);
    assert!(matches!(s.close(), Closed::Released { .. }));
}

#[test]
fn panic_inside_a_scope_quarantines_then_releases() {
    let base = stats();
    // The default hook writes into the test harness's capture buffer, which
    // would then hold scope memory; silence it for this test.
    let hook = std::panic::take_hook();
    #[allow(clippy::disallowed_methods)] // test: silences the default hook for this test
    std::panic::set_hook(Box::new(|_| {}));
    let s = Scope::open(R, 4096).unwrap();
    let caught = catch_unwind(AssertUnwindSafe(|| {
        s.enter(|| {
            let _v = Box::new([1u8; 100]);
            std::panic::panic_any(7u32)
        })
    }));
    #[allow(clippy::disallowed_methods)] // test: restores the previous hook
    std::panic::set_hook(hook);
    let payload = caught.unwrap_err();
    // The boxed payload was allocated inside the scope and is still alive.
    let closed = s.close();
    assert!(matches!(closed, Closed::Quarantined { .. }), "{closed:?}");
    assert_eq!(stats().quarantined_scopes, base.quarantined_scopes + 1);
    assert_eq!(payload.downcast_ref::<u32>(), Some(&7));
    drop(payload);
    let after = stats();
    assert_eq!(after.quarantined_scopes, base.quarantined_scopes);
    assert_eq!(after.free_pages, base.free_pages);
}

strict_test!(strict_test_macro_runs_a_bounded_body, R, 4096, |scope| {
    let mut b = ExactBuf::admit(scope, 1024, Kinds::ENCODER).unwrap();
    b.put(&[1; 1024]).unwrap();
    assert!(b.is_full());
});

#[test]
fn strict_run_reports_a_value_that_outlives_its_scope() {
    match strict::run_strict(R, 4096, |s| s.enter(|| Box::new(5u8))) {
        Err(StrictFailure::Violated { violations, closed, .. }) => {
            assert_eq!(violations.leaks, 1);
            assert!(matches!(closed, Closed::Quarantined { live: 1, .. }));
        }
        other => panic!("expected a leak, got {:?}", other.map(|b| *b)),
    }
    assert_eq!(stats().quarantined_scopes, 0, "released once the returned value dropped");
}

#[test]
fn strict_run_reports_an_overdraft_in_enforce_mode() {
    set_mode(Mode::Enforce);
    let r = strict::run(R, 0, |s| {
        let mut v: Vec<u8> = Vec::new();
        s.enter(|| v.try_reserve_exact(1 << 20)).is_ok()
    });
    match r {
        Err(StrictFailure::Violated { violations, stats, .. }) => {
            assert!(violations.overdrafts >= 1);
            assert!(stats.overdraft_pages > 0);
        }
        other => panic!("expected an overdraft, got {other:?}"),
    }
}

#[test]
fn strict_mode_refuses_unscoped_allocation_on_an_armed_thread_only() {
    set_mode(Mode::Strict);
    let armed = {
        let _a = strict::arm();
        let mut v: Vec<u8> = Vec::new();
        v.try_reserve(32).is_err()
    };
    let unarmed = {
        let mut v: Vec<u8> = Vec::new();
        v.try_reserve(32).is_ok()
    };
    set_mode(Mode::Enforce);
    let _ = strict::take();
    assert!(armed, "armed thread: refused");
    assert!(unarmed, "unarmed thread (platform glue): served");
}
