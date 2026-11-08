//! The SlopNinja miner client (`slop_ninja_miner`): pack an artifact object
//! (hashing each file once), check it, commit its content on chain through
//! rtao, upload its header and files straight to R2 under presigned URLs the
//! coordinator mints (admission contract v2,
//! `docs/testnet/admission-api.md`) and follow its admission.
//!
//! Linux and macOS only: openings are protected with Unix file modes.

#[cfg(not(unix))]
compile_error!("slop_ninja_miner supports Linux and macOS only");

pub mod chain;
pub mod check;
pub mod client;
pub mod commit;
pub mod direct;
pub mod object;
pub mod opening;
pub mod provenance;
pub mod public;
pub mod reference;
pub mod trust;
pub mod upload;

pub use slop_ninja_admission_wire as wire;
