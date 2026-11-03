//! The shared allocator self-tests, natively. The same cases run as wasm32 in
//! local workerd through the drill (`drill/run.sh selftest`).
//!
//! Run with one test thread: the cases assert on isolate-wide counters.

use slop_ninja_arena::io::FmtBuf;
use slop_ninja_arena::{ArenaAlloc, selftest};

#[global_allocator]
static ALLOC: ArenaAlloc = ArenaAlloc;

#[test]
fn arena_selftest() {
    // A fixed report buffer: no allocation between cases.
    let mut raw = [0u8; 8192];
    let mut w = FmtBuf::new(&mut raw);
    let (pass, fail) = selftest::run_all(&mut w);
    let report = w.as_str();
    println!("{report}");
    assert_eq!(pass as usize, selftest::CASES.len(), "{report}");
    assert_eq!(fail, 0, "{report}");
}
