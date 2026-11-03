//! The Slop Ninja miner client (`slop_ninja_miner`): pack an artifact object,
//! check it, commit its digest on chain through rtao, upload it to the
//! coordinator under the admission contract (`docs/testnet/admission-api.md`)
//! and follow its admission.
//!
//! Linux and macOS only: openings are protected with Unix file modes.

#[cfg(not(unix))]
compile_error!("slop_ninja_miner supports Linux and macOS only");

pub mod chain;
pub mod check;
pub mod client;
pub mod commit;
pub mod object;
pub mod opening;
pub mod public;
pub mod trust;
pub mod upload;

pub use slop_ninja_admission_wire as wire;
