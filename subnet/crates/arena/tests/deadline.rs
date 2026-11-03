//! Second review, finding 3: a route declared with the default constructor
//! had no deadline, so an invocation whose promise never settles kept its
//! whole reservation as an open scope forever (the pool stayed `Healthy` and
//! every need it blocked was `Retry`). Now an `enter` handler's route is an
//! `AsyncRoute`, whose deadline is mandatory (a zero deadline does not
//! compile as a constant): the hung invocation is abandoned and pinned at its
//! deadline, and a need it blocks is `Poisoned` (durable fault and recycle,
//! keyed by its cause), never `Retry`. Separate binary: the leak is
//! permanent by construction (the js-sys task cycle keeps it).

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use slop_ninja_arena::{
    ArenaAlloc, AsyncRoute, Health, PAGE, PinKind, RefusalKind, Route, SEGMENT_OVERHEAD, Scope, enter, limits, stats,
    tick,
};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

struct NeverSettles;
impl Future for NeverSettles {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }
}

fn bytes_for_pages(pages: u32) -> usize {
    (pages as usize - 1) * PAGE - SEGMENT_OVERHEAD
}

#[test]
fn a_hung_async_route_is_abandoned_at_its_deadline_and_poisons_not_retries() {
    // The glue probe's Worker fetch route: one await of at most 2 s plus the
    // request CPU limit.
    const ROUTE: AsyncRoute = AsyncRoute::new(1, "probe/fetch", 2_000 + limits::REQUEST_CPU_MS);
    let t0 = 1_000_000;
    tick(t0);
    let free = stats().free_pages;
    let e = enter(ROUTE, bytes_for_pages(free - 6), |_| async {
        let _v = vec![0u8; 4096];
        NeverSettles.await;
    })
    .unwrap();
    let mut task = Box::pin(e);
    let mut cx = Context::from_waker(Waker::noop());
    assert!(task.as_mut().poll(&mut cx).is_pending());
    #[allow(clippy::disallowed_methods)] // the js-sys task cycle keeps it; nothing drops or polls it again
    std::mem::forget(task);
    assert_eq!(tick(t0 + ROUTE.abandon_after_ms() - 1), Health::Healthy, "within its deadline");
    let h = tick(t0 + ROUTE.abandon_after_ms() + 1);
    let s = stats();
    eprintln!("deadline: open {} abandoned {} pinned {} health {h:?}", s.open_scopes, s.abandoned_scopes, s.pinned_pages);
    assert_eq!(s.abandoned_scopes, 1, "abandoned at its deadline");
    assert!(s.pinned_pages > s.quarantine_budget_pages, "its pages are pinned");
    match h {
        Health::Poisoned { cause, .. } => assert_eq!((cause.route, cause.kind), (ROUTE.id(), PinKind::Abandoned)),
        Health::Healthy => panic!("pinned past the budget: poisoned"),
    }
    let r = Scope::open(Route::new(2, "next"), bytes_for_pages(16)).err().map(|r| r.kind);
    assert_eq!(r, Some(RefusalKind::Poisoned), "a need only the hung invocation blocks is Poisoned, never Retry");
}
