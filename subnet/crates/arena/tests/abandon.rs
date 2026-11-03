//! Finding 2, the review's F-ABANDON reproduction re-run against the fixed
//! allocator. Separate binary: the leak is permanent by construction (the
//! js-sys task cycle), as it would be in a Worker isolate until recycled.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use slop_ninja_arena::{
    ArenaAlloc, AsyncRoute, Health, Mode, PAGE, PinKind, RefusalKind, Route, SEGMENT_OVERHEAD, Scope, enter, health,
    limits, model, reset_latches, set_mode, shrink_free_to, stats, tick,
};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

const RA: AsyncRoute = AsyncRoute::new(42, "review-abandon", limits::WAIT_UNTIL_MS);
const R2: Route = Route::new(43, "review-abandon-b");

fn bytes_for_pages(pages: u32) -> usize {
    (pages as usize - 1) * PAGE - SEGMENT_OVERHEAD
}

fn restore() {
    reset_latches();
    set_mode(Mode::Enforce);
}

type BoxFut = Pin<Box<dyn Future<Output = ()>>>;

/// js-sys 0.3.106 `Task`: holds its future and its own waker (a cycle).
struct Task {
    inner: RefCell<Option<(BoxFut, Waker)>>,
}

unsafe fn clone(p: *const ()) -> RawWaker {
    unsafe { Rc::increment_strong_count(p as *const Task) };
    RawWaker::new(p, &VTABLE)
}
unsafe fn wake(p: *const ()) {
    unsafe { Rc::decrement_strong_count(p as *const Task) };
}
unsafe fn wake_by_ref(_p: *const ()) {}
unsafe fn drop_w(p: *const ()) {
    unsafe { Rc::decrement_strong_count(p as *const Task) };
}
static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop_w);

fn task_waker(t: Rc<Task>) -> Waker {
    unsafe { Waker::from_raw(RawWaker::new(Rc::into_raw(t) as *const (), &VTABLE)) }
}

struct NeverSettles;
impl Future for NeverSettles {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }
}

/// Reviewed: an invocation whose promise never settles kept its scope open
/// with its whole reservation forever, unrecorded. Fixed: past its route's
/// deadline it is counted as abandoned and pinned; past the quarantine budget
/// the pool is `Poisoned` (durable fault + recycle) instead of answering
/// `Retry` forever.
#[test]
fn an_abandoned_invocation_is_counted_and_poisons_past_the_budget() {
    restore();
    let route = RA;
    let t0 = stats().clock_ms + 10_000_000;
    tick(t0);
    // Leave the largest route plus a few pages, so the budget is small.
    let largest = model::LARGEST_ROUTE_BYTES.div_ceil(PAGE) as u32 + 1;
    shrink_free_to(largest + 6).unwrap();
    let base = stats();
    let mut leaked = Vec::new();
    // Each invocation: a js-sys task whose awaited promise never settles.
    for i in 0..4u64 {
        let t = Rc::new(Task { inner: RefCell::new(None) });
        let w = task_waker(t.clone());
        tick(t0 + i);
        let entered = enter(route, 64 * 1024, |_| async {
            let _buf = vec![0u8; 32 * 1024];
            NeverSettles.await;
        })
        .expect("admitted");
        let outer: BoxFut = Box::pin(async move {
            let _ = entered.await;
        });
        *t.inner.borrow_mut() = Some((outer, w));
        {
            let mut g = t.inner.borrow_mut();
            let (f, w) = g.as_mut().unwrap();
            let mut cx = Context::from_waker(w);
            assert!(f.as_mut().poll(&mut cx).is_pending());
        }
        // The platform abandons it: only the task's own cycle holds it.
        leaked.push(Rc::downgrade(&t));
        drop(t);
    }
    let during = stats();
    assert_eq!(during.abandoned_scopes, base.abandoned_scopes, "not abandoned before the deadline");
    tick(t0 + 31_000);
    let after = stats();
    eprintln!(
        "F-ABANDON abandoned {}->{} pinned {}->{} budget {} healthy {}",
        base.abandoned_scopes, after.abandoned_scopes, base.pinned_pages, after.pinned_pages, after.quarantine_budget_pages, after.healthy
    );
    assert_eq!(after.abandoned_scopes, base.abandoned_scopes + 4, "every abandoned invocation is counted");
    assert!(after.pinned_pages > base.pinned_pages, "and pinned");
    assert!(after.abandoned >= base.abandoned + 4, "abandonment recorded");
    match health() {
        Health::Poisoned { cause, .. } => {
            assert_eq!((cause.route, cause.kind), (RA.id(), PinKind::Abandoned), "the cause names the hung route: {cause:?}")
        }
        Health::Healthy => panic!("past the budget the pool is poisoned"),
    }
    // The largest route is refused as Poisoned, never Retry.
    match Scope::open(R2, bytes_for_pages(largest - 1)) {
        Err(r) => assert_eq!(r.kind, RefusalKind::Poisoned, "{r:?}"),
        Ok(_) => panic!("the largest route should not fit beside the pinned pages"),
    }
    assert!(leaked.iter().all(|w| w.upgrade().is_some()), "the tasks are leaked by their own cycle");
    // In production the host now records `arena_poisoned` and recycles the
    // instance; this test binary keeps the leak (its process ends).
    #[allow(clippy::disallowed_methods)] // the test keeps the leak, as the isolate would
    std::mem::forget(leaked);
}

