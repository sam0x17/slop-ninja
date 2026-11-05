//! Hidden evaluation and serverless challenge generation (milestone M4, testnet).
//!
//! The `EvalCohort` Durable Object owns cohort authority. Each heartbeat (a
//! webhook delivery or a watchdog tick) records the RPC-confirmed block and the
//! subnet epoch index ([`chain`]) and arms one bounded step; a step claims a
//! piece of work in a transaction, performs its I/O (eval bucket, Workers AI,
//! the scorer Container, the receipts bucket), and commits the result in a
//! second transaction ([`engine`]). Long work is many such steps across
//! invocations, so an eviction, redeploy or outage resumes the same cohort.
//!
//! - [`plan`]: the frozen cohort plan (admitted roster, or the testnet
//!   operator-roster shortcut).
//! - [`pool`]: the hidden human pool and the never-reuse whole-group draw.
//! - [`textgen`]: prompts, output validation and exact-span splicing.
//! - [`score`]: scorer results to outcomes, and content-free components.
//! - [`receipts`]: content-free chunked attempt receipts and the index.
//! - [`package`]: package inputs and the pluggable reward rule (v0 on testnet).
//! - [`lanes`]: the fan-out lanes' claims, commits, sweeps and status.
//! - [`cost`]: the lane planner and the scoring cost model.
//! - [`limiter`]: the shared Workers AI token buckets.
//! - [`ledger`]: the deployment's monthly spend ledger.
//! - [`v3`]: `reward-v1` cohorts (plan v3) and fingerprint probe builds.
//! - [`scorer_client`]: what the scoring lanes read from and send to a scorer.
//! - [`admission`]: coordinator admission of a cohort's roster (contract
//!   `slop_ninja.admission/v1`): sessions, framing, verdicts and the cutoff.
//! - [`commitments`]: the finalized commitment log admission reads.
//! - [`calendar`]: the standing plan template and the cohort calendar.
//! - [`succession`]: durable succession between main authorities.
//! - [`retention`]: detailed receipt deletion after N+1 and N+2 complete.
//! - [`probe_cut`]: the operator's preparation of a same-authority probe
//!   cut and the template reinstall naming its version.
//! - [`boundaries`]: the boundary journal that dates every stage by tempo
//!   boundaries for the public dashboard.

pub mod admission;
pub mod boundaries;
pub mod calendar;
pub mod chain;
pub mod commitments;
pub mod cost;
pub mod engine;
pub mod lane_logic;
pub mod lanes;
pub mod ledger;
pub mod limiter;
pub mod textgen;
pub mod package;
pub mod plan;
pub mod pool;
pub mod probe_cut;
pub mod receipts;
pub mod retention;
pub mod score;
pub mod scorer_client;
pub mod succession;
pub mod v3;

#[cfg(test)]
mod tests;
