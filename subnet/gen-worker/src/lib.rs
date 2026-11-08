//! The internal generation Worker of SlopNinja testnet evaluation
//! (`slop-ninja-gen-testnet`).
//!
//! It makes the frontier half of generation v2's vendor calls (OpenAI and
//! Anthropic) for the eval Worker, which reaches it only through a service
//! binding: it has no route, no `workers.dev` name, no preview URL, no
//! custom domain, no cron, no Durable Object and no bucket. It holds the two
//! vendor keys, bound from the account's Secrets Store (or, per vendor, a
//! configured endpoint with its own key, a Worker secret), and nothing else:
//! the eval Worker keeps the package and receipt keys and gains no secret.
//! It is stateless; every durable fact (claims, attempts, vendor pauses,
//! spend) lives in the eval Worker's cohort object.
//!
//! - [`drive`]: one call over a transport: the key header, the Flex
//!   standard retry, the classification of a non-200 answer, and the
//!   bounded stream read of a 200 answer.
//! - [`route`]: where a vendor's calls go: its own API, or a configured
//!   endpoint from a Worker secret.
//! - [`routes`]: the arena routes and footprints of its `fetch` export.
//! - `cf` (wasm32): the export, the interface check, the keys, and the
//!   fetch transport.
//!
//! Routes ([`slop_ninja_gen_core::iface`]): `POST /internal/gen/v1/generate`
//! and `GET /internal/gen/v1/health` on the internal host with the
//! interface marker; anything else is a 404. Nothing here logs a request,
//! a response, a header or a key.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)
)]

pub mod drive;
pub mod route;
pub mod routes;

#[cfg(target_arch = "wasm32")]
mod cf;
// The coordinator's bounded body readers and platform-cancelled timeouts,
// shared as source (this Worker uses a subset).
#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
#[path = "../../coordinator/src/cf_io.rs"]
mod cf_io;

// Native witness tests on the bounded arena: the arena is the test binary's
// global allocator (see `arena_witness`).
#[cfg(all(test, feature = "arena-witness"))]
#[global_allocator]
static WITNESS_ALLOC: slop_ninja_arena::ArenaAlloc = slop_ninja_arena::ArenaAlloc;
#[cfg(all(test, feature = "arena-witness"))]
mod arena_witness;
