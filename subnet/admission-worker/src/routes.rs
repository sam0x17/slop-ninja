//! The upload Worker's arena routes: identities, abandonment deadlines and
//! control footprints of its one export, `fetch`, by request class.
//! Compiled natively and for wasm32, so the footprint the build checks
//! against the memory model is the one the Worker admits.

use slop_ninja_arena::{AsyncRoute, limits};

pub mod id {
    pub const PUBLIC: u8 = 1;
    pub const FRAME: u8 = 2;
    pub const OTHER: u8 = 3;
    pub const STREAM: u8 = 4;
}

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;

/// The arena deadline of a Worker fetch's scopes: 15 minutes, borrowed from
/// the longest documented Workers wall limit (an alarm's), as the
/// coordinator's `UNBOUNDED_FETCH_MS`. A fetch itself has no documented
/// wall limit; a scope not polled for this long counts as abandoned. A
/// part's body streams to R2 inside the stream route, at the miner's pace.
pub const UNBOUNDED_FETCH_MS: u64 = limits::ALARM_MS;

/// The public `/admission/v1/` routes.
pub const R_PUBLIC: AsyncRoute = AsyncRoute::new(id::PUBLIC, "upload/public", UNBOUNDED_FETCH_MS);
/// The internal framing route (the eval Worker's service binding only).
pub const R_FRAME: AsyncRoute = AsyncRoute::new(id::FRAME, "upload/frame", UNBOUNDED_FETCH_MS);
/// Anything else (404).
pub const R_OTHER: AsyncRoute = AsyncRoute::new(id::OTHER, "upload/other", UNBOUNDED_FETCH_MS);
/// A part's stream: the scope a part's body streams to R2 in, between its
/// `part-begin` and `part-record` scopes (both [`R_PUBLIC`]).
pub const R_STREAM: AsyncRoute = AsyncRoute::new(id::STREAM, "upload/stream", UNBOUNDED_FETCH_MS);

/// The public route: the signed fields, a body of at most a
/// `SubmissionUpload` record, the small RPC replies (a roster load reads
/// them one at a time), the forwarded message and the eval Worker's reply
/// (at most the contract's response limit at R2's part maximum, 2.6 MiB),
/// and on a layout miss the metadata parse
/// (the coordinator measured 34 pages for the parse of the same metadata,
/// `RUNTIME_PARSED_BYTES`). The metadata reply itself is admitted as its
/// own block of its declared length (at most
/// `registry::MAX_LARGE_RESPONSE_BYTES`). Unmeasured on this Worker: the
/// sum rounded up to whole MiB.
pub const PUBLIC_CONTROL_BYTES: usize = 4 * MIB;
/// The framing route: one 1 MiB buffer (`cf::FRAME_CHUNK_BYTES`), the
/// header record (at most the protocol's 1 MiB `MAX_ARTIFACT_HEADER_LEN`),
/// the parts manifest (at most the scorer's 1 MiB) and the outcome's JSON
/// (`admission::frame_outcome_bound`, about 2.1 MiB at the spec's largest
/// object). Unmeasured: the sum rounded up to 8 MiB.
pub const FRAME_CONTROL_BYTES: usize = 8 * MIB;
/// A 404: nothing beyond the route's own state.
pub const OTHER_CONTROL_BYTES: usize = PUBLIC_CONTROL_BYTES;
/// A part's stream: the `JsFuture` of the one `uploadPart` promise and its
/// callbacks (a few hundred bytes; the body itself streams in JS and never
/// enters Rust), held for as long as the miner sends. Unmeasured: one
/// WebAssembly page. A slow part therefore holds this, not the public
/// footprint.
pub const STREAM_CONTROL_BYTES: usize = slop_ninja_arena::PAGE;

/// The smallest route control footprint (`min_route_bytes` of the model).
pub const MIN_CONTROL_BYTES: usize = STREAM_CONTROL_BYTES;
/// The largest invocation: the framing route's footprint, or the public
/// route's with its metadata block, whichever is larger, plus the scope's
/// state page (`largest_route_bytes` of the model).
pub const LARGEST_INVOCATION_BYTES: usize = {
    let public = PUBLIC_CONTROL_BYTES + crate::registry::MAX_LARGE_RESPONSE_BYTES;
    let frame = FRAME_CONTROL_BYTES;
    (if public > frame { public } else { frame }) + slop_ninja_arena::PAGE
};

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_arena::model;

    #[test]
    fn the_model_matches_the_route_table() {
        assert_eq!(model::MIN_ROUTE_BYTES, MIN_CONTROL_BYTES);
        assert_eq!(model::LARGEST_ROUTE_BYTES, LARGEST_INVOCATION_BYTES);
        assert!(FRAME_CONTROL_BYTES >= slop_ninja_artifact_check::admission::frame_outcome_bound() + 3 * MIB);
        const { assert!(STREAM_CONTROL_BYTES < PUBLIC_CONTROL_BYTES && STREAM_CONTROL_BYTES <= OTHER_CONTROL_BYTES) };
        const { assert!(model::LARGEST_ROUTE_BYTES < model::LINEAR_MEMORY_BYTES) };
    }
}
