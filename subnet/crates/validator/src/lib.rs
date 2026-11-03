//! Native Slop Ninja validator.
//!
//! Startup opens the native weight journal (persisting pause), records which
//! signed operations the live metadata can encode, reconciles finalized
//! history without a signer, checks that commit/reveal is enabled at finalized
//! state, and resumes explicitly. No runtime version is compared: a runtime
//! upgrade never halts startup.
//!
//! The application stage then verifies the coordinator's signed score package
//! (a bundle file the fetcher installs from the public status host, or that
//! the operator installs under testnet shortcut T12), derives the native row
//! with the shared protocol crate, and enqueues it as journal work. By default it only
//! prepares the unsigned weight commit and prints its hash. `run --dispatch`
//! loads the configured hotkey and drives rtao's journaled scheduler.

pub mod app;
pub mod config;
pub mod faults;
pub mod fetch;
pub mod fixture;
pub mod guard;
pub mod live;
pub mod logging;
pub mod package;
pub mod package_v2;
pub mod reveals;
pub mod payload;
pub mod startup;
pub mod supersede;

#[cfg(test)]
mod fetch_tests;
#[cfg(test)]
mod package_tests;
#[cfg(test)]
mod payload_tests;
#[cfg(test)]
mod storage_tests;
#[cfg(test)]
mod tests;
