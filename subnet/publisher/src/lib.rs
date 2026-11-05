//! The Slop Ninja testnet publisher: a separate cron Worker that reads the
//! coordinator's private buckets and status through bindings and writes
//! static public objects into a separate public-candidate bucket.
//!
//! Every minute it:
//!
//! 1. writes the redacted status snapshot and evaluates the alert rules,
//!    posting transitions to the configured sink ([`status`], [`alerts`]);
//!    once that phase has saved its state, it posts the alerts relay's
//!    dead-man heartbeat (decision D18), beside the next steps;
//! 2. verifies the next closed `reward-v1` cohort of the public record in the
//!    private receipts bucket under the pinned trust roots (records, the
//!    published index, the recomputed outcome and the closure) and publishes
//!    only its allow-listed summary ([`publish`], [`verify`], [`summary`],
//!    [`allowlist`]): the signed records, indexes, components and receipts
//!    stay coordinator-private;
//! 3. publishes each admitted entrant's artifact (its header and files) at
//!    scoring start, copied from the private admission bucket inside R2 or
//!    by binding stream, with no hash on our side, never a reference's
//!    (references are published as staged reference packages) or an
//!    operator's ([`artifacts`], [`publish::static_withhold`]); a closed
//!    cohort's record reports it, checked against the package's header
//!    digests;
//! 4. writes the cohort index, the leaderboard, the reference pointers (each
//!    linking its staged reference package, when one is staged), the
//!    current validator authorization and the coordinator's signed row that
//!    validators commit ([`index`], [`leaderboard`]);
//! 5. writes the public dashboard's documents (`dashboard/v1`) into their own
//!    bucket, read only by the private dashboard feed Worker
//!    ([`dashboard`]);
//! 6. pings an external dead-man URL when one is enabled, after a run that
//!    finished both phases.
//!
//! The public host serves those objects directly; the Worker has no request
//! route. All logic except `cf.rs` is plain Rust over the [`store::Store`]
//! trait and is tested natively; `cf.rs` holds the Workers entry points and
//! compiles only for `wasm32`.

pub mod alerts;
pub mod allowlist;
pub mod artifacts;
pub mod arena;
pub mod config;
pub mod copy_wait;
pub mod dashboard;
pub mod durable;
pub mod index;
pub mod layout;
pub mod leaderboard;
pub mod publish;
pub mod redact;
pub mod ss58;
pub mod status;
pub mod store;
pub mod summary;
pub mod verify;

#[cfg(target_arch = "wasm32")]
mod cf;
