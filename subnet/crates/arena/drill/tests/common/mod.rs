//! Test data lives in a scope too: the glue partition is sized for platform
//! glue only (`build.rs`), so a test's large inputs (generated JSON bodies,
//! reference encodings, sinks) are built inside a harness scope reserved for
//! them, and the route under test reads them by reference.

use slop_ninja_arena::{Route, Scope};

pub const HARNESS: Route = Route::new(200, "test-harness");

/// A scope for test data with `bytes` of footprint.
pub struct Harness(pub Scope);

impl Harness {
    pub fn new(bytes: usize) -> Harness {
        Harness(Scope::open(HARNESS, bytes).expect("harness scope"))
    }

    /// Builds test data inside the harness scope.
    pub fn data<T>(&self, f: impl FnOnce() -> T) -> T {
        self.0.enter(f)
    }
}
