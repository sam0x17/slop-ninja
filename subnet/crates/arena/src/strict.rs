//! Strict enforcement (design §3.1).
//!
//! Arming a thread (natively) or the instance (wasm32) records three
//! violations wherever they happen:
//!
//! 1. an allocation while no scope is current (it would go to glue);
//! 2. an overdraft past a scope's reservation;
//! 3. a scope closing with live bytes (quarantine).
//!
//! In [`Mode::Strict`] the first two also fail: the allocation returns null,
//! so fallible APIs (`try_reserve`) see an error and infallible ones abort.
//! [`run`] and [`strict_test!`](crate::strict_test) arm, run a body inside a
//! declared scope and turn any violation into an error.

#[cfg(feature = "knobs")]
use crate::pool::Mode;
use crate::scope::{Closed, Refusal, Route, Scope, ScopeStats};
use crate::sys::current;

/// Violations recorded on the armed thread (wasm32: instance) since the last
/// [`take`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Violations {
    pub unscoped: u32,
    pub overdrafts: u32,
    pub leaks: u32,
}

impl Violations {
    pub fn is_clean(&self) -> bool {
        *self == Violations::default()
    }
}

/// Arms the current thread until dropped.
#[must_use = "the thread is disarmed when the guard drops"]
pub struct Armed {
    prev: bool,
}

pub fn arm() -> Armed {
    Armed { prev: current::set_armed(true) }
}

impl Drop for Armed {
    fn drop(&mut self) {
        current::set_armed(self.prev);
    }
}

pub fn is_armed() -> bool {
    current::armed()
}

/// Returns and clears the recorded violations.
pub fn take() -> Violations {
    let [unscoped, overdrafts, leaks] = current::take_violations();
    Violations { unscoped, overdrafts, leaks }
}

/// Why a strict run failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrictFailure {
    /// The declared scope was not admitted.
    Refused(Refusal),
    /// The body broke a bound. `stats` is the scope just before it closed.
    Violated { violations: Violations, closed: Closed, stats: ScopeStats },
}

/// Arms the thread, opens a scope with the declared control bytes, runs `f`
/// inside it and closes it. Fails on any violation, on quarantine, and on
/// usage beyond the reservation. `R` must hold no scope heap.
pub fn run<R>(route: Route, control_bytes: usize, f: impl FnOnce(&Scope) -> R) -> Result<R, StrictFailure> {
    let _ = take();
    let armed = arm();
    let scope = Scope::open(route, control_bytes).map_err(StrictFailure::Refused)?;
    let out = scope.enter(|| f(&scope));
    let stats = scope.stats();
    let closed = scope.close();
    drop(armed);
    let violations = take();
    let within = stats.peak_pages <= stats.reserved_pages;
    if violations.is_clean() && within && matches!(closed, Closed::Released { .. }) {
        Ok(out)
    } else {
        Err(StrictFailure::Violated { violations, closed, stats })
    }
}

/// [`run`] with [`Mode::Strict`] set for its duration.
#[cfg(feature = "knobs")]
pub fn run_strict<R>(route: Route, control_bytes: usize, f: impl FnOnce(&Scope) -> R) -> Result<R, StrictFailure> {
    let prev = crate::set_mode(Mode::Strict);
    let out = run(route, control_bytes, f);
    crate::set_mode(prev);
    out
}

/// A `#[test]` whose body runs under [`run_strict`] with a declared bound.
///
/// ```ignore
/// slop_ninja_arena::strict_test!(decode_fits, ROUTE, 4096, |scope| {
///     let buf = ExactBuf::admit(scope, 1024).unwrap();
///     assert_eq!(buf.capacity(), 1024);
/// });
/// ```
#[cfg(feature = "knobs")]
#[macro_export]
macro_rules! strict_test {
    ($name:ident, $route:expr, $bytes:expr, |$scope:ident| $body:block) => {
        #[test]
        fn $name() {
            if let Err(e) = $crate::strict::run_strict($route, $bytes, |$scope| $body) {
                panic!("{}: {:?}", stringify!($name), e);
            }
        }
    };
}
