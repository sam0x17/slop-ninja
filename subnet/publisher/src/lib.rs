//! The Slop Ninja testnet publisher: a separate cron Worker that reads the
//! coordinator's private buckets and status through bindings and writes
//! static public objects into a separate public-candidate bucket.
//!
//! Every minute it:
//!
//! 1. writes the redacted status snapshot and evaluates the alert rules,
//!    posting transitions to the configured sink ([`status`], [`alerts`]);
//! 2. verifies the next closed `reward-v1` cohort of the public record in the
//!    private receipts bucket under the pinned trust roots, exactly as a
//!    validator would (records, receipt chunks, the published index, the
//!    recomputed outcome and the closure), copies its public objects byte for
//!    byte, and verifies the copies again ([`publish`], [`verify`]);
//! 3. publishes artifact objects after their cohort closed, default-deny:
//!    only an admitted outside upload, never a reference (references are
//!    published as staged reference packages), an operator's or a copy
//!    ([`publish::static_withhold`]);
//! 4. writes the cohort index, the leaderboard, the reference pointers (each
//!    linking its staged reference package, when one is staged) and the
//!    current validator authorization ([`index`], [`leaderboard`]);
//! 5. pings the dead-man heartbeat when one is enabled.
//!
//! The public host serves those objects directly; the Worker has no request
//! route. All logic except `cf.rs` is plain Rust over the [`store::Store`]
//! trait and is tested natively; `cf.rs` holds the Workers entry points and
//! compiles only for `wasm32`.

pub mod alerts;
pub mod arena;
pub mod config;
pub mod index;
pub mod layout;
pub mod leaderboard;
pub mod publish;
pub mod redact;
pub mod ss58;
pub mod status;
pub mod store;
pub mod verify;

#[cfg(target_arch = "wasm32")]
mod cf;
