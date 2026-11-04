//! The key-free upload Worker of Slop Ninja testnet admission
//! (`slop-ninja-admission-testnet`, served at `testnet-admission.slop.ninja`).
//!
//! It takes all untrusted upload traffic of the admission contract
//! `slop_ninja.admission/v1` (`docs/testnet/admission-api.md`) and holds no
//! signing key: no chain hotkey, no package, receipt or authorization key.
//! Its only secret is the interface token it shares with the eval Worker.
//! Its bindings: the private upload bucket (uploaded objects only), and a
//! service binding to the eval Worker's internal admission interface.
//!
//! - [`gate`]: what it decides about a request before it forwards anything
//!   (signature, access mode, header checks, registration, body bounds), in
//!   that order, as one sequence (`gate::decide`) the miner mock runs too.
//! - [`front`]: this Worker's own decisions beside the gate (the cohort
//!   read's cache key, the framing route's admission, the part target check,
//!   interface failures, static contract bodies).
//! - [`registry`]: the roster of the subnet's hotkeys at the finalized head
//!   (read once a block per isolate, its storage keys composed by name from
//!   live metadata), held in a static array.
//! - [`routes`]: the arena routes and footprints of its `fetch` export.
//! - `cf` (wasm32): the export, the public routes, the cohort read from the
//!   snapshot, part streaming to R2 and the internal framing route the eval
//!   Worker's framing step calls.
//!
//! The eval Worker keeps every decision that needs cohort state (sessions,
//! the commitment log, verdicts, the cutoff) and never decodes an uploaded
//! object: framing (`slop_ninja_artifact_check::admission::Framer`) runs
//! here.
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
