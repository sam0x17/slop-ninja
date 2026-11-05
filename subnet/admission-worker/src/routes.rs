//! The upload Worker's arena routes: identities, abandonment deadlines and
//! control footprints of its one export, `fetch`, by request class.
//! Compiled natively and for wasm32, so the footprint the build checks
//! against the memory model is the one the Worker admits.
//!
//! No route carries an upload byte (contract `slop_ninja.admission/v2`:
//! every object goes straight to R2 under a presigned URL). The largest body
//! is the header route's, one header record of at most the protocol's
//! `MAX_ARTIFACT_HEADER_LEN` (1 MiB); a public request's body is at most one
//! `SubmissionUpload` record. Each footprint below is derived from those
//! formats and the contract's response limit, and the native witnesses
//! (`arena_witness`, `--features arena-witness`) run each route's heaviest
//! path under `Mode::Strict` in exactly that footprint. Bounds version
//! [`ROUTE_BOUNDS_VERSION`].

use slop_ninja_admission_wire::internal as iface;
use slop_ninja_arena::{AsyncRoute, limits};

pub mod id {
    pub const PUBLIC: u8 = 1;
    pub const HEADER: u8 = 2;
    pub const OTHER: u8 = 3;
}

/// The version of this table's footprints; a change to any of them is a new
/// version (and a new `memory-model.json` version).
pub const ROUTE_BOUNDS_VERSION: &str = "admission-upload-routes-2026-11-05-v3";

/// The arena deadline of a Worker fetch's scopes: 15 minutes, borrowed from
/// the longest documented Workers wall limit (an alarm's), as the
/// coordinator's `UNBOUNDED_FETCH_MS`. A fetch itself has no documented
/// wall limit; a scope not polled for this long counts as abandoned. No
/// route streams a body at a miner's pace any more.
pub const UNBOUNDED_FETCH_MS: u64 = limits::ALARM_MS;

/// The public `/admission/v2/` routes.
pub const R_PUBLIC: AsyncRoute = AsyncRoute::new(id::PUBLIC, "upload/public", UNBOUNDED_FETCH_MS);
/// The internal header route (the eval Worker's service binding only).
pub const R_HEADER: AsyncRoute = AsyncRoute::new(id::HEADER, "upload/header", UNBOUNDED_FETCH_MS);
/// Anything else (404).
pub const R_OTHER: AsyncRoute = AsyncRoute::new(id::OTHER, "upload/other", UNBOUNDED_FETCH_MS);

/// The largest single value the public route builds from a bounded input:
/// the contract's response limit (64 KiB, the eval Worker's reply body is
/// within it plus a small envelope), the cohort snapshot (that limit, its
/// envelope and the repair list at its bound, one roster of hotkeys:
/// `iface::cohort_snapshot_limit`), the roster's per-call RPC reply
/// (`registry::MAX_RESPONSE_BYTES`, 64 KiB), or one forwarded message
/// (`iface::FORWARD_BYTES`), whichever is largest.
pub const PUBLIC_UNIT_BYTES: usize = {
    let reply = slop_ninja_admission_wire::RESPONSE_BASE_BYTES;
    let snapshot = iface::cohort_snapshot_limit();
    let rpc = crate::registry::MAX_RESPONSE_BYTES;
    let forward = iface::FORWARD_BYTES;
    let a = if reply > rpc { reply } else { rpc };
    let b = if snapshot > forward { snapshot } else { forward };
    if a > b { a } else { b }
};

/// The size of one parsed JSON value (`serde_json::Value`) natively, an
/// upper bound of its wasm32 size (tested).
pub const JSON_VALUE_BYTES: usize = 32;

/// The densest JSON tree one roster RPC reply (at most
/// `registry::MAX_RESPONSE_BYTES`) can parse into: one value per two bytes
/// (`0,`), each [`JSON_VALUE_BYTES`], held in one vector whose old and new
/// capacities coexist while it doubles (one and a half times the tree),
/// rounded up to twice the tree: `32 x 64 KiB` = 2 MiB. The reply's
/// `result` is moved out of the parsed reply, never cloned.
pub const RPC_TREE_BYTES: usize = 2 * (crate::registry::MAX_RESPONSE_BYTES / 2) * JSON_VALUE_BYTES;

/// The public route's control footprint: the densest RPC reply tree
/// ([`RPC_TREE_BYTES`]) and four values of [`PUBLIC_UNIT_BYTES`] beside it
/// (the roster call's key list and request, the forwarded message, a
/// relayed reply's body, which is passed on as raw JSON and never parsed
/// into a tree, or the snapshot's typed parse and the cohort response it
/// serves (or, for a session read or repair the roster did not pass, the
/// repair list's membership read from the same parse), and the workers-rs
/// copies of them). The read buffers themselves
/// are admitted as their own blocks of their exact lengths (the bounded
/// readers, `cf_io`). The witnesses run each path at its bound under
/// `Strict` in this footprint. A layout miss adds two admissions of their
/// own, sized from what the reply declares: the metadata reply as one block
/// of its length (at most `registry::MAX_LARGE_RESPONSE_BYTES`), and its
/// parse as a heap run of [`metadata_parse_bytes`] for the decoded length,
/// admitted before the parse allocates.
pub const PUBLIC_CONTROL_BYTES: usize = RPC_TREE_BYTES + 4 * PUBLIC_UNIT_BYTES;

/// The heap a parse of `metadata_len` bytes of V15 metadata needs: the
/// recorded parse peaks of the same rtao-core parser, scaled to the length
/// at the steepest measured ratio (`slop_ninja_arena::measured`, shared
/// with the coordinator, whose witness measures them).
pub const fn metadata_parse_bytes(metadata_len: usize) -> usize {
    slop_ninja_arena::measured::runtime_parse_bytes(metadata_len)
}

/// The largest metadata the registration read can parse: a `0x` hex reply
/// within `registry::MAX_LARGE_RESPONSE_BYTES` carries at most half that.
pub const MAX_METADATA_BYTES: usize = crate::registry::MAX_LARGE_RESPONSE_BYTES / 2;

/// The header route's body: one header record, admitted as its own block
/// of its exact length by the bounded body reader (`cf_io`), at most
/// [`iface::HEADER_REQUEST_BYTES`].
pub const HEADER_BODY_BYTES: usize = iface::HEADER_REQUEST_BYTES;

/// The header route's control footprint: the decoded header (the decoder
/// copies the contract string and each file name out of the body once, so
/// at most one body's bytes and the spec's file count of entries), the
/// inference content and its record encoding (built only after the record
/// validated its contract id and names, so small), the outcome and its JSON
/// (`admission::header_outcome_bound`, under 4 KiB), and the workers-rs
/// response copy. Two header lengths: the decoded strings at the body's
/// length, and the same again as the stated room for dlmalloc's large-run
/// footprint and the small values beside it. The witness runs the worst
/// header shapes (a 1 MiB contract string, the most entries with the
/// longest names, a valid table, a malformed one) under `Strict` in this
/// footprint.
pub const HEADER_CONTROL_BYTES: usize = 2 * HEADER_BODY_BYTES;

/// A 404: the static body and the workers-rs response, nothing else.
pub const OTHER_CONTROL_BYTES: usize = slop_ninja_arena::PAGE;

/// The smallest route control footprint (`min_route_bytes` of the model).
pub const MIN_CONTROL_BYTES: usize = OTHER_CONTROL_BYTES;

/// The largest invocation: the public route's footprint with its metadata
/// block and that metadata's parse at the reply bound, or the header
/// route's footprint with its body block, whichever is larger, plus the
/// scope's state page (`largest_route_bytes` of the model).
pub const LARGEST_INVOCATION_BYTES: usize = {
    let public =
        PUBLIC_CONTROL_BYTES + crate::registry::MAX_LARGE_RESPONSE_BYTES + metadata_parse_bytes(MAX_METADATA_BYTES);
    let header = HEADER_CONTROL_BYTES + HEADER_BODY_BYTES;
    (if public > header { public } else { header }) + slop_ninja_arena::PAGE
};

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_arena::model;

    const KIB: usize = 1024;

    #[test]
    fn the_model_matches_the_route_table() {
        assert_eq!(model::MIN_ROUTE_BYTES, MIN_CONTROL_BYTES);
        assert_eq!(model::LARGEST_ROUTE_BYTES, LARGEST_INVOCATION_BYTES);
        const { assert!(model::LARGEST_ROUTE_BYTES < model::LINEAR_MEMORY_BYTES) };
        const { assert!(OTHER_CONTROL_BYTES <= PUBLIC_CONTROL_BYTES && OTHER_CONTROL_BYTES <= HEADER_CONTROL_BYTES) };
    }

    #[test]
    fn the_footprints_follow_their_formats() {
        // The interface reply, the snapshot and the forward fit the public unit.
        assert!(crate::front::reply_limit() <= PUBLIC_UNIT_BYTES + KIB);
        assert!(iface::cohort_snapshot_limit() <= PUBLIC_UNIT_BYTES + KIB);
        const { assert!(iface::FORWARD_BYTES <= PUBLIC_UNIT_BYTES) };
        assert!(core::mem::size_of::<serde_json::Value>() <= JSON_VALUE_BYTES);
        // The header body is the protocol's header limit; the outcome is tiny beside it.
        const { assert!(HEADER_BODY_BYTES == slop_ninja_protocol::MAX_ARTIFACT_HEADER_LEN as usize) };
        assert!(slop_ninja_artifact_check::admission::header_outcome_bound() < 4 * KIB);
    }

    #[test]
    fn the_model_records_this_table_version() {
        let model = include_str!("../memory-model.json");
        assert!(model.contains(ROUTE_BOUNDS_VERSION), "memory-model.json names {ROUTE_BOUNDS_VERSION}");
    }
}
