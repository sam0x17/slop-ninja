//! The fetch Worker's arena routes, footprints and the isolate's fetch
//! memory. Compiled natively and for wasm32, so the numbers the tests check
//! against the memory model are the ones the Worker uses.
//!
//! **Fetch memory.** Requests run concurrently in one isolate. Each keeps
//! its route's fixed control footprint in its own scope, and reserves its
//! body-dependent work from one pool, the fetch memory: a page fetch takes
//! one share ([`FETCH_SHARE_BYTES`]) at intake and gives back what its page
//! does not need once the strip pass has counted it; a robots read takes
//! [`ROBOTS_WORK_BYTES`]. A request that does not fit answers 503 before
//! any outbound call (the detect Worker retries once). The pool is a byte
//! count, as the detect Worker's scan memory.

use crate::bound;
use slop_ninja_arena::{AsyncRoute, limits, model};

pub mod id {
    pub const FETCH: u8 = 1;
    pub const ROBOTS: u8 = 2;
    pub const OTHER: u8 = 3;
    pub const STATICS: u8 = 4;
}

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;

/// The longest documented Workers wall limit stands in as the abandonment
/// deadline, as in the other Workers; every await also has the request's
/// own budget.
pub const FETCH_MS: u64 = limits::ALARM_MS;

pub const R_FETCH: AsyncRoute = AsyncRoute::new(id::FETCH, "fetch/page", FETCH_MS);
pub const R_ROBOTS: AsyncRoute = AsyncRoute::new(id::ROBOTS, "fetch/robots", FETCH_MS);
pub const R_OTHER: AsyncRoute = AsyncRoute::new(id::OTHER, "fetch/other", FETCH_MS);
/// The home of the extraction stack's lazy statics (never closed).
pub const R_STATICS: slop_ninja_arena::Route = slop_ninja_arena::Route::new(id::STATICS, "fetch/statics");

/// A page fetch's fixed part: the request (at most the wire's
/// `REQUEST_BYTES`), each hop's URL and request headers, the response heads
/// (at most `net::MAX_HEADER_BYTES` each), the signing key and one signature,
/// the strip pass's handlers and metadata (each value at most
/// `bound::META_BYTES`, JSON-LD at most `bound::LD_JSON_BYTES`), the body
/// window and the workers-rs request and response objects. Unmeasured as a
/// sum (about 200 KiB at the bounds); rounded up to 512 KiB as the stated
/// margin until G2 measures it.
pub const FETCH_CONTROL_BYTES: usize = 512 * KIB;
/// A robots read's fixed part: the request, hop URLs, heads and the answer
/// envelope (the parse is work, reserved from the fetch memory).
pub const ROBOTS_CONTROL_BYTES: usize = 256 * KIB;
/// A 404: nothing beyond the route's own state.
pub const OTHER_CONTROL_BYTES: usize = slop_ninja_arena::PAGE;

pub const MIN_CONTROL_BYTES: usize = OTHER_CONTROL_BYTES;
pub const LARGEST_INVOCATION_BYTES: usize = FETCH_CONTROL_BYTES + slop_ninja_arena::PAGE;

/// The robots work: a 500 KiB body, its parse at the worst rule density and
/// the answer's JSON: 8 MiB measured by
/// `arena_witness::arena_witness_robots_at_the_parse_bound` (64,000 rules),
/// rounded up to 12 MiB.
pub const ROBOTS_WORK_BYTES: usize = 12 * MIB;

/// The statics home: the extraction stack's lazy statics (75 KB live) plus
/// the warm-up pages' transient peak, two pages measured by
/// `arena_witness::home`, rounded up to 512 KiB.
pub const STATICS_HOME_BYTES: usize = 512 * KIB;

/// Page fetches one isolate holds side by side at their bound.
pub const FETCHES_PER_ISOLATE: usize = 2;

/// The isolate's fetch memory: the model's linear memory less the stack,
/// the glue partition, the statics home, and the fixed parts of as many
/// concurrent requests as the glue admits at the smallest route (bounded by
/// the fetch route's control for the first [`FETCHES_PER_ISOLATE`] and a
/// page each for the rest).
pub const FETCH_MEMORY_BYTES: usize = model::LINEAR_MEMORY_BYTES
    .saturating_sub(model::STACK_BYTES)
    .saturating_sub(model::GLUE_BUDGET_BYTES)
    .saturating_sub(STATICS_HOME_BYTES)
    .saturating_sub(FETCH_CONTROL_BYTES * FETCHES_PER_ISOLATE)
    .saturating_sub(OTHER_CONTROL_BYTES * 16);

/// One page fetch's share of the fetch memory.
pub const FETCH_SHARE_BYTES: usize = FETCH_MEMORY_BYTES / FETCHES_PER_ISOLATE;

const _: () = assert!(ROBOTS_WORK_BYTES <= FETCH_SHARE_BYTES, "a redirect's robots read runs inside a page fetch's share");

/// The isolate's fetch-memory reservations (wasm32 isolates are
/// single-threaded).
#[derive(Debug, Default)]
pub struct FetchMemory {
    held: std::cell::Cell<usize>,
}

/// The pool cannot hold a request now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Busy;

impl FetchMemory {
    pub const fn new() -> FetchMemory {
        FetchMemory { held: std::cell::Cell::new(0) }
    }

    pub fn take(&self, bytes: usize) -> Result<(), Busy> {
        let held = self.held.get();
        if bytes > FETCH_MEMORY_BYTES || held.saturating_add(bytes) > FETCH_MEMORY_BYTES {
            return Err(Busy);
        }
        self.held.set(held + bytes);
        Ok(())
    }

    pub fn give_back(&self, bytes: usize) {
        self.held.set(self.held.get().saturating_sub(bytes));
    }

    pub fn held(&self) -> usize {
        self.held.get()
    }
}

/// A page fetch's reservation for a stripped bound of `html_bound` bytes
/// (clamped to [`bound::HTML_BOUND`]).
pub fn fetch_work(html_bound: usize) -> usize {
    let b = html_bound.min(bound::HTML_BOUND);
    // At most one element per three bytes (`<a>`), and never more than a share.
    bound::page_work(b, b / 3 + 1).clamp(ROBOTS_WORK_BYTES, FETCH_SHARE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_matches_the_route_table() {
        assert_eq!(model::MIN_ROUTE_BYTES, MIN_CONTROL_BYTES);
        assert_eq!(model::LARGEST_ROUTE_BYTES, LARGEST_INVOCATION_BYTES);
        const { assert!(model::LARGEST_ROUTE_BYTES < model::LINEAR_MEMORY_BYTES) };
        const { assert!(ROBOTS_CONTROL_BYTES <= FETCH_CONTROL_BYTES) };
    }

    #[test]
    fn two_pages_at_their_bound_fit_side_by_side() {
        let one = fetch_work(bound::HTML_BOUND);
        assert!(one <= FETCH_SHARE_BYTES);
        let m = FetchMemory::new();
        m.take(one).unwrap();
        m.take(one).unwrap();
        // Robots reads still fit beside them only if room is left; a third
        // page does not.
        assert!(m.take(one).is_err());
        m.give_back(2 * one);
        assert_eq!(m.held(), 0);
    }

    #[test]
    fn a_small_bound_reserves_less() {
        assert!(fetch_work(64 * 1024) < fetch_work(bound::HTML_BOUND));
        assert_eq!(fetch_work(usize::MAX), fetch_work(bound::HTML_BOUND));
    }
}
