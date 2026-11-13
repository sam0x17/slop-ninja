//! The card Worker's arena routes and footprints (the bounded-arena rule).
//! Compiled natively and for wasm32, so the numbers the tests check against
//! the memory model are the ones the Worker uses.
//!
//! - A `POST /render` invocation runs in a scope of [`RENDER_CONTROL_BYTES`]:
//!   the request body (at most [`crate::wire::REQUEST_BYTES`]), the parsed
//!   card and the workers-rs request and response objects.
//! - The render itself (the SVG document, resvg's tree, the 1200 x 630
//!   pixmap, filter buffers and the PNG encoder) runs synchronously in a
//!   child scope of [`RENDER_WORK_BYTES`], admitted physically before it
//!   starts (a refusal answers 503, never a trap). The PNG is copied to a JS
//!   buffer before the child closes, so nothing outlives it. A render has no
//!   await, so an isolate holds at most one render's work at a time.
//! - Anything else (404, 405, a header refusal) runs in one page.

use slop_ninja_arena::{AsyncRoute, Route, limits, model};

pub mod id {
    pub const RENDER: u8 = 1;
    pub const RENDER_WORK: u8 = 2;
    pub const OTHER: u8 = 3;
}

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;

/// The longest documented Workers wall limit stands in as the abandonment
/// deadline, as in the other Workers.
pub const RENDER_MS: u64 = limits::ALARM_MS;

pub const R_RENDER: AsyncRoute = AsyncRoute::new(id::RENDER, "og/render", RENDER_MS);
pub const R_RENDER_WORK: Route = Route::new(id::RENDER_WORK, "og/render-work");
pub const R_OTHER: AsyncRoute = AsyncRoute::new(id::OTHER, "og/other", RENDER_MS);

/// A render request's fixed part: the body (64 KiB at most), its parsed card
/// (bounded strings and a ladder of at most one entry per 7 body bytes) and
/// the request and response objects; rounded up to 256 KiB as the stated
/// margin until G2 measures it.
pub const RENDER_CONTROL_BYTES: usize = 256 * KIB;
/// A 404: nothing beyond the route's own state.
pub const OTHER_CONTROL_BYTES: usize = slop_ninja_arena::PAGE;

pub const MIN_CONTROL_BYTES: usize = OTHER_CONTROL_BYTES;
pub const LARGEST_INVOCATION_BYTES: usize = RENDER_CONTROL_BYTES + slop_ninja_arena::PAGE;

/// One render's work: 9.0 MiB measured by `arena_witness` (resvg 0.50.0,
/// 2026-11-13) as the largest peak over every fixture card with either
/// ends and the paid fixture with the most ladder cells a request body can
/// carry (8,126 sections alternating `weak` and `strong`, so that no run
/// merges, in a 65,532-byte body), rounded up to 12 MiB as the stated
/// margin. The witness fails when a render exceeds it.
pub const RENDER_WORK_BYTES: usize = 12 * MIB;

/// Requests one isolate holds side by side at the render route's control
/// footprint while one render runs.
pub const REQUESTS_PER_ISOLATE: usize = 16;

/// What the isolate needs at its bound: the stack, the glue partition, one
/// render's work and the control parts of [`REQUESTS_PER_ISOLATE`] requests.
pub const ISOLATE_NEED_BYTES: usize =
    model::STACK_BYTES + model::GLUE_BUDGET_BYTES + RENDER_WORK_BYTES + REQUESTS_PER_ISOLATE * LARGEST_INVOCATION_BYTES;

const _: () =
    assert!(ISOLATE_NEED_BYTES < model::LINEAR_MEMORY_BYTES, "one render and its requests fit the linear memory");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_matches_the_route_table() {
        assert_eq!(model::MIN_ROUTE_BYTES, MIN_CONTROL_BYTES);
        assert_eq!(model::LARGEST_ROUTE_BYTES, LARGEST_INVOCATION_BYTES);
        const { assert!(model::LARGEST_ROUTE_BYTES < model::LINEAR_MEMORY_BYTES) };
        const { assert!(crate::wire::REQUEST_BYTES < RENDER_CONTROL_BYTES / 2) };
    }
}
