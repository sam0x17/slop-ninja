//! Finding 10 (no libtest harness: the reviewed failure mode was a hang).
//! In Observe mode an impossible `reserve` must return, record the excess,
//! and leave every counter sane. The reviewed build panicked under the pool
//! lock in debug (the panic re-entered the allocator and spun on the lock)
//! and wrapped the claim counter in release. Exit 0 = fixed, 2 = a counter
//! moved, 3 = deadlock.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use slop_ninja_arena::{ArenaAlloc, Mode, Route, Scope, set_mode, stats};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

static RETURNED: AtomicBool = AtomicBool::new(false);

fn main() {
    let _watchdog = std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(10));
        if !RETURNED.load(Ordering::SeqCst) {
            let _ = std::io::stderr().write_all(b"observe_overflow: deadlock (reserve did not return within 10 s)\n");
            std::process::exit(3);
        }
    });
    set_mode(Mode::Observe);
    let a = Scope::open(Route::new(1, "a"), 0).unwrap();
    let s = Scope::open(Route::new(2, "b"), 0).unwrap();
    let before = stats();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let x = s.reserve(usize::MAX / 2);
        let y = s.reserve(usize::MAX);
        let z = s.admit_block(usize::MAX - 7, 1 << 20);
        (x, y, z)
    }));
    RETURNED.store(true, Ordering::SeqCst);
    let after = stats();
    eprintln!(
        "observe_overflow: returned panicked={} held {}->{} free {}->{} observed_excess_pages {}->{}",
        r.is_err(),
        before.held_pages,
        after.held_pages,
        before.free_pages,
        after.free_pages,
        before.observed_excess_pages,
        after.observed_excess_pages
    );
    let ok = matches!(r, Ok((Ok(()), Ok(()), Ok(()))))
        && after.held_pages == before.held_pages
        && after.free_pages == before.free_pages
        && after.observed_excess_pages > before.observed_excess_pages;
    drop((s, a));
    set_mode(Mode::Enforce);
    std::process::exit(if ok { 0 } else { 2 });
}
