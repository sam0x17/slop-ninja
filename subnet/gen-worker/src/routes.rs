//! The generation Worker's arena routes: identities, abandonment deadlines
//! and control footprints of its one export, `fetch`, by request class.
//! Compiled natively and for wasm32, so the footprint the build checks
//! against the memory model is the one the Worker admits.
//!
//! A call's own memory is admitted from what it declares, inside its scope,
//! before anything is allocated: the body as one block of its declared
//! length (refused above [`iface::CALL_BODY_BYTES`] before it is read), its
//! parse as heap of the body's length, then, before the vendor is
//! contacted, the call's heap ([`iface::call_heap_bytes`]: the stream
//! reader's two buffers, the read buffer, the reply at its bound and a Flex
//! retry's body). A call the isolate cannot hold answers a throttle (no
//! attempt used, nothing sent) rather than allocate.

use slop_ninja_arena::{AsyncRoute, limits};
use slop_ninja_gen_core::iface;

pub mod id {
    pub const GENERATE: u8 = 1;
    pub const HEALTH: u8 = 2;
    pub const OTHER: u8 = 3;
}

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;

/// The arena deadline of a call's scope: 15 minutes, the longest documented
/// Workers wall limit (an alarm's), as the coordinator's and the upload
/// Worker's. A frontier call at the highest effort may take minutes; the
/// eval Worker's lease bounds it first.
pub const UNBOUNDED_FETCH_MS: u64 = limits::ALARM_MS;

pub const R_GENERATE: AsyncRoute = AsyncRoute::new(id::GENERATE, "gen/generate", UNBOUNDED_FETCH_MS);
pub const R_HEALTH: AsyncRoute = AsyncRoute::new(id::HEALTH, "gen/health", UNBOUNDED_FETCH_MS);
pub const R_OTHER: AsyncRoute = AsyncRoute::new(id::OTHER, "gen/other", UNBOUNDED_FETCH_MS);

/// A route's fixed part: the request and response handles, the header list,
/// the futures of one fetch and its reads, the secret's string. Unmeasured
/// on this Worker: one MiB, the unit the upload Worker's smallest
/// unmeasured footprints round to.
pub const CONTROL_BYTES: usize = MIB;

/// The most one call admits beyond its control footprint: the message
/// block at the interface bound, its parse (the request body's string and
/// one page of fields), its vendor's endpoint secret parse
/// ([`crate::route::HEAP_BYTES`], when the vendor has one), and its heap ([`iface::call_heap_bytes`] at the
/// interface's bounds: the reader's visible and line buffers with the read
/// buffer, the reply, and a Flex standard retry's body).
pub const CALL_ADMISSIONS_MAX: usize = iface::CALL_MESSAGE_BYTES
    + parse_bytes(iface::CALL_BODY_BYTES)
    + crate::route::HEAP_BYTES
    + iface::VISIBLE_BYTES_MAX
    + iface::LINE_BYTES_MAX
    + crate::drive::READ_BUFFER_BYTES
    + 2 * iface::VISIBLE_BYTES_MAX
    + iface::PAGE
    + iface::CALL_BODY_BYTES;

/// The heap a call message's parse admits: its request body as one string
/// (JSON unescaping only shrinks it) and one page for the other fields.
pub const fn parse_bytes(body_bytes: usize) -> usize {
    body_bytes + iface::PAGE
}

/// The smallest route control footprint (`min_route_bytes` of the model).
pub const MIN_CONTROL_BYTES: usize = CONTROL_BYTES;
/// The largest invocation: a call at every interface bound, plus the
/// scope's state page (`largest_route_bytes` of the model).
pub const LARGEST_INVOCATION_BYTES: usize = CONTROL_BYTES + CALL_ADMISSIONS_MAX + slop_ninja_arena::PAGE;

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_arena::model;

    #[test]
    fn the_model_matches_the_route_table() {
        assert_eq!(model::MIN_ROUTE_BYTES, MIN_CONTROL_BYTES);
        assert_eq!(model::LARGEST_ROUTE_BYTES, LARGEST_INVOCATION_BYTES);
        const { assert!(model::LARGEST_ROUTE_BYTES < model::LINEAR_MEMORY_BYTES) };
    }
}
