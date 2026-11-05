//! The key-free upload Worker of Slop Ninja testnet admission
//! (`slop-ninja-admission-testnet`, served at `testnet-admission.slop.ninja`).
//!
//! It fronts every public route of the admission contract
//! `slop_ninja.admission/v2` (`docs/testnet/admission-api.md`) and holds no
//! signing key and no R2 binding: no chain hotkey, no package, receipt,
//! authorization or object-store key. Its only secret is the interface
//! token it shares with the eval Worker, and its only binding the eval
//! Worker's internal admission interface. No upload byte passes through
//! it: miners PUT every artifact object straight to R2 under presigned URLs.
//!
//! - [`gate`]: what it decides about a request before it forwards anything
//!   (signature, access mode, header checks, registration, body bounds), in
//!   that order, as one sequence (`gate::decide`) the miner mock runs too.
//! - [`front`]: this Worker's own decisions beside the gate (the cohort
//!   read's cache key and the snapshot it serves, the header route's
//!   admission and answer, the reply relay, interface failures, static
//!   contract bodies).
//! - [`registry`]: the roster of the subnet's hotkeys at the finalized head
//!   (read once a block per isolate, its storage keys composed by name from
//!   live metadata), held in a static array.
//! - [`routes`]: the arena routes and footprints of its `fetch` export.
//! - `cf` (wasm32): the export, the public routes, the cohort read, and the
//!   internal header route the eval Worker calls.
//!
//! The eval Worker keeps every decision that needs cohort state (sessions,
//! the commitment log, URL minting through the signer, R2 heads, verdicts,
//! the cutoff) and never decodes an uploaded header: the header check
//! (`slop_ninja_artifact_check::admission::check_header_bytes`) runs here,
//! on header bytes the eval Worker read and sends over its binding.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)
)]

pub use slop_ninja_admission_gate as gate;
pub mod front;
pub mod registry;
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
