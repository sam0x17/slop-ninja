//! Slop Ninja Workers coordinator (testnet).
//!
//! Storage-neutral logic (webhook authentication, the per-block heartbeat path,
//! the finalized cursor, durable faults, the cron watchdog, durable claims, the
//! RPC budget, RPC encoding, schema migrations, chain encoding and the signing
//! journal) compiles and is tested natively. The Workers entry points and Durable Objects in `cf`
//! compile only for wasm32.
//!
//! Memory: every hand-written export of both Workers allocates in its own
//! bounded scope of `slop-ninja-arena` (`arena_routes` declares every entry
//! point's route and footprint; `cf` installs the allocator and opens one
//! scope per invocation), over a fixed linear memory sized by
//! `memory-model.json`; the fan-out lane objects and the constructor
//! fallbacks spill or overdraw, counted. The build runs in `Observe` mode
//! until its model is measured.
//!
//! Outside tests the crate denies `unwrap`, `expect`, `panic!` and unchecked
//! indexing: a panic in the ChainJournal's Durable Object would trap the
//! instance that the heartbeat, the watchdog and signing all depend on. The
//! hidden-evaluation code (`eval`, and its Durable Objects and routes in `cf`)
//! was written without that rule and is exempt until it is converted; it runs
//! in its own Durable Objects and Worker routes, never in the ChainJournal.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

// The content-free status documents are large `json!` literals.
#![recursion_limit = "256"]

pub mod arena_routes;
pub mod chain;
pub mod claims;
pub mod config;
#[cfg_attr(
    not(test),
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]
pub mod eval;
pub mod heartbeat;
pub mod journal;
pub mod liveness;
pub mod rpc;
pub mod schedule;
pub mod signing;
pub mod sql;
pub mod target;
pub mod watchdog;
pub mod webhook;
pub mod wire;

#[cfg(target_arch = "wasm32")]
mod cf;

#[cfg(test)]
mod testing;

// Native witness tests on the bounded arena: the arena is the test binary's
// global allocator (see `arena_witness`).
#[cfg(all(test, feature = "arena-witness"))]
#[global_allocator]
static WITNESS_ALLOC: slop_ninja_arena::ArenaAlloc = slop_ninja_arena::ArenaAlloc;
#[cfg(all(test, feature = "arena-witness"))]
mod arena_witness;
