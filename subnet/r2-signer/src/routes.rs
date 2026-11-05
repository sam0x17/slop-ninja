//! The signer's arena routes: identities, deadlines and control footprints
//! of its one export, `fetch`. Compiled natively and for wasm32, so the
//! footprint the build checks against the memory model is the one the
//! Worker admits.

use slop_ninja_arena::{AsyncRoute, limits};

pub mod id {
    pub const MINT: u8 = 1;
    pub const OTHER: u8 = 2;
    pub const COPY: u8 = 3;
}

const KIB: usize = 1024;

/// A mint finishes in one request; the longest documented Workers wall
/// limit stands in as the abandonment deadline, as the other Workers'.
pub const FETCH_MS: u64 = limits::ALARM_MS;

pub const R_MINT: AsyncRoute = AsyncRoute::new(id::MINT, "signer/mint", FETCH_MS);
pub const R_OTHER: AsyncRoute = AsyncRoute::new(id::OTHER, "signer/other", FETCH_MS);
pub const R_COPY: AsyncRoute = AsyncRoute::new(id::COPY, "signer/copy", FETCH_MS);

/// The mint route: one request (at most [`crate::mint::REQUEST_BYTES`]),
/// one reply buffer ([`crate::mint::REPLY_BYTES`]), the two credential
/// strings and the host, one presigned URL at a time, and the workers-rs
/// request and response objects. Unmeasured: the sum is about 12 KiB;
/// rounded up to 256 KiB as the stated margin until it is measured.
pub const MINT_CONTROL_BYTES: usize = 256 * KIB;
/// The copy route: one copy request ([`crate::mint::COPY_REQUEST_BYTES`]),
/// one reply ([`crate::mint::COPY_REPLY_BYTES`]), the copy credential's two
/// strings, the host and two bucket names, and the workers-rs request and
/// response objects: less than the mint route holds, so it is given the
/// mint route's footprint and the largest footprint is unchanged.
pub const COPY_CONTROL_BYTES: usize = MINT_CONTROL_BYTES;
/// A 404: nothing beyond the route's own state.
pub const OTHER_CONTROL_BYTES: usize = slop_ninja_arena::PAGE;

pub const MIN_CONTROL_BYTES: usize = OTHER_CONTROL_BYTES;
pub const LARGEST_INVOCATION_BYTES: usize = MINT_CONTROL_BYTES + slop_ninja_arena::PAGE;

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_arena::model;

    #[test]
    fn the_model_matches_the_route_table() {
        assert_eq!(model::MIN_ROUTE_BYTES, MIN_CONTROL_BYTES);
        assert_eq!(model::LARGEST_ROUTE_BYTES, LARGEST_INVOCATION_BYTES);
        const { assert!(crate::mint::REQUEST_BYTES + crate::mint::REPLY_BYTES + 4 * KIB < MINT_CONTROL_BYTES) };
        const { assert!(crate::mint::COPY_REQUEST_BYTES + crate::mint::COPY_REPLY_BYTES + 4 * KIB < COPY_CONTROL_BYTES) };
        const { assert!(COPY_CONTROL_BYTES <= MINT_CONTROL_BYTES) };
        const { assert!(model::LARGEST_ROUTE_BYTES < model::LINEAR_MEMORY_BYTES) };
    }
}
