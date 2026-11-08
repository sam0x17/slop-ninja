//! The internal signer Worker of SlopNinja testnet admission v2
//! (`slop-ninja-r2-signer-testnet`).
//!
//! It holds two R2 S3 credentials, each read from Secrets Store per request,
//! and serves two routes. The mint route (the presign key, scoped to the
//! admission bucket) presigns the direct-to-R2 artifact uploads: one
//! `PutObject` URL per header and per file, each binding the derived key,
//! the committed length, the committed SHA-256 (R2 verifies it as the body
//! arrives) and create-only. Its only caller is the eval Worker, through a
//! service binding with `SIGNER_INTERFACE_TOKEN`, after the cohort authority
//! has authorized the request. The same route, for the eval Worker's
//! operator route only, presigns the operator's reference files
//! (`slop_ninja_reference_staging_v1`: `reference/v2/files/<sha256>` in
//! the admission bucket, bound to the pack's length and SHA-256, so R2
//! verifies each one as it does a miner's). It never touches an object: no R2 binding,
//! no route, no `workers.dev` name, no preview URLs; the public upload
//! Worker has no binding to it.
//!
//! The copy route serves publication at scoring start when the publisher
//! runs with `ARTIFACT_COPY = "server"` (the default is `"stream"` until a
//! cross-bucket copy is qualified, probe G7-x): reached only through the
//! publisher's service binding with a second token
//! (`COPY_INTERFACE_TOKEN`), it presigns one `CopyObject` from the
//! admission bucket to the public bucket with the separate copy credential.
//! While that credential is not bound the route answers 503 and the
//! publisher waits.
//!
//! - [`mint`]: the decision, natively tested;
//! - [`routes`]: the arena routes and footprints of its `fetch` export;
//! - `cf` (wasm32): the export, the interface token check and the Secrets
//!   Store reads.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)
)]

pub mod mint;
pub mod routes;

/// The signer's internal origin (service binding only).
pub const SIGNER_ORIGIN: &str = "https://r2-signer.internal";
/// Host of [`SIGNER_ORIGIN`].
pub const SIGNER_HOST: &str = "r2-signer.internal";
/// The mint route.
pub const MINT_PATH: &str = "/internal/r2-signer/v1/mint";
/// The copy route (publication at scoring start; the publisher's service
/// binding only, with its own token `COPY_INTERFACE_TOKEN`).
pub const COPY_PATH: &str = "/internal/r2-signer/v1/copy";
/// Media type of copy requests and replies.
pub const COPY_MEDIA_TYPE: &str = "application/vnd.slop-ninja.r2-copy.v1";
/// The header carrying the interface token the signer shares with the eval
/// Worker (`SIGNER_INTERFACE_TOKEN`, a Worker secret of both).
pub const TOKEN_HEADER: &str = "x-sn-interface-token";
/// Media type of mint requests and replies.
pub const MEDIA_TYPE: &str = "application/vnd.slop-ninja.r2-mint.v1";
/// The shortest interface token either side accepts.
pub const MIN_TOKEN_BYTES: usize = 32;

/// Whether `presented` is the configured token, in time independent of
/// where they differ; an unset or short configured token matches nothing.
pub fn token_matches(configured: Option<&str>, presented: Option<&str>) -> bool {
    let (Some(want), Some(got)) = (configured.map(str::trim), presented.map(str::trim)) else {
        return false;
    };
    if want.len() < MIN_TOKEN_BYTES || want.len() != got.len() {
        return false;
    }
    want.bytes().zip(got.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

#[cfg(target_arch = "wasm32")]
mod cf;

// Native witness tests on the bounded arena: the arena is the test binary's
// global allocator (see `arena_witness`).
#[cfg(all(test, feature = "arena-witness"))]
#[global_allocator]
static WITNESS_ALLOC: slop_ninja_arena::ArenaAlloc = slop_ninja_arena::ArenaAlloc;
#[cfg(all(test, feature = "arena-witness"))]
mod arena_witness;
#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
#[path = "../../coordinator/src/cf_io.rs"]
mod cf_io;

#[cfg(test)]
mod tests {
    #[test]
    fn tokens_match_only_the_configured_value() {
        let t = "0123456789abcdef0123456789abcdef";
        assert!(super::token_matches(Some(t), Some(t)));
        assert!(!super::token_matches(Some(t), Some("0123456789abcdef0123456789abcdeX")));
        assert!(!super::token_matches(Some(t), None));
        assert!(!super::token_matches(None, Some(t)));
        assert!(!super::token_matches(Some("short"), Some("short")));
    }
}
