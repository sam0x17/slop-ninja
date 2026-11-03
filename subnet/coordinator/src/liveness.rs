//! The ChainJournal's external liveness record.
//!
//! The heartbeat, the watchdog and signing all go through the one
//! ChainJournal Durable Object, so a wedged instance stalls all three, and
//! nothing inside it can report that. The cron Worker therefore times its
//! `/tick` call to the journal (bounded by [`TIMEOUT_MS`]) and keeps the
//! outcome outside the Durable Object, as one small R2 object ([`key`]).
//! `/status` reads that object with its own bounded journal call, so a wedged
//! journal shows as `unresponsive` (or a stopped cron as `stale`) instead of
//! a hung request.
//!
//! Each deployment probes its own ChainJournal, and the coordinator and the
//! evaluation Worker bind the same evaluation bucket, so each keeps its own
//! record. Sharing one would let either Worker's answered tick reset the
//! other's failures and hide a stopped cron.

use crate::config::Role;
use serde::{Deserialize, Serialize};

/// The R2 key (in the evaluation bucket) holding `role`'s record. The
/// coordinator's key is the one its `/status`, runbook and drill records use.
pub fn key(role: Role) -> &'static str {
    match role {
        Role::Coordinator => "ops/chain-journal-liveness.json",
        Role::Eval => "ops/eval/chain-journal-liveness.json",
    }
}
/// Longest wait for one journal call before it counts as a failure.
pub const TIMEOUT_MS: u64 = 10_000;

/// The largest stored record: every field at its widest JSON encoding (the
/// three `u64` times and counters at 20 digits, the two optional times, a
/// `u16` status), well under this bound (`the_record_bound_holds_the_widest_record`).
pub const MAX_RECORD_BYTES: usize = 1024;
/// An answered call slower than this is `slow`.
pub const SLOW_MS: u64 = 3_000;
/// A record older than this means the cron watchdog itself is not running.
pub const STALE_MS: u64 = 180_000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Liveness {
    pub checked_at_ms: u64,
    /// The journal answered the tick with HTTP 200 within the timeout.
    pub ok: bool,
    pub latency_ms: u64,
    /// The journal's HTTP status; `None` on a timeout or transport error.
    pub status: Option<u16>,
    pub consecutive_failures: u64,
    pub first_failure_ms: Option<u64>,
    pub last_ok_ms: Option<u64>,
}

/// The record after one timed journal call.
pub fn next(
    prev: Option<&Liveness>,
    now_ms: u64,
    ok: bool,
    latency_ms: u64,
    status: Option<u16>,
) -> Liveness {
    let failures = if ok {
        0
    } else {
        prev.map_or(0, |p| p.consecutive_failures).saturating_add(1)
    };
    Liveness {
        checked_at_ms: now_ms,
        ok,
        latency_ms,
        status,
        consecutive_failures: failures,
        first_failure_ms: if ok {
            None
        } else {
            prev.and_then(|p| p.first_failure_ms).or(Some(now_ms))
        },
        last_ok_ms: if ok {
            Some(now_ms)
        } else {
            prev.and_then(|p| p.last_ok_ms)
        },
    }
}

/// `ok`, `slow`, `unresponsive`, `stale` or `unknown` (no record yet).
pub fn verdict(record: Option<&Liveness>, now_ms: u64) -> &'static str {
    match record {
        None => "unknown",
        Some(r) if now_ms.saturating_sub(r.checked_at_ms) > STALE_MS => "stale",
        Some(r) if !r.ok => "unresponsive",
        Some(r) if r.latency_ms > SLOW_MS => "slow",
        Some(_) => "ok",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_accumulate_until_an_answer() {
        let t = 1_793_000_000_000;
        let a = next(None, t, true, 40, Some(200));
        assert_eq!(verdict(Some(&a), t), "ok");
        let b = next(Some(&a), t + 60_000, false, TIMEOUT_MS, None);
        let c = next(Some(&b), t + 120_000, false, TIMEOUT_MS, None);
        assert_eq!(c.consecutive_failures, 2);
        assert_eq!(c.first_failure_ms, Some(t + 60_000));
        assert_eq!(c.last_ok_ms, Some(t));
        assert_eq!(verdict(Some(&c), t + 120_000), "unresponsive");
        let d = next(Some(&c), t + 180_000, true, 4_000, Some(200));
        assert_eq!((d.consecutive_failures, d.first_failure_ms), (0, None));
        assert_eq!(verdict(Some(&d), t + 180_000), "slow");
        assert_eq!(verdict(Some(&d), t + 180_000 + STALE_MS + 1), "stale");
        assert_eq!(verdict(None, t), "unknown");
    }

    #[test]
    fn each_deployment_keeps_its_own_record() {
        assert_eq!(key(Role::Coordinator), "ops/chain-journal-liveness.json");
        assert_ne!(key(Role::Eval), key(Role::Coordinator));
        assert!(key(Role::Eval).starts_with("ops/"));
    }

    /// Both Workers' cron ticks each minute, recorded the way `scheduled`
    /// records them: read this role's record, apply `next`, write it back.
    #[test]
    fn the_evaluation_worker_never_masks_a_coordinator_outage() {
        use std::collections::BTreeMap;
        let mut bucket: BTreeMap<&'static str, Liveness> = BTreeMap::new();
        let tick =
            |bucket: &mut BTreeMap<&'static str, Liveness>, role: Role, now: u64, ok: bool| {
                let prev = bucket.get(key(role)).cloned();
                let latency = if ok { 40 } else { TIMEOUT_MS };
                let status = ok.then_some(200);
                bucket.insert(key(role), next(prev.as_ref(), now, ok, latency, status));
            };
        let t = 1_793_000_000_000;
        for m in 0..3 {
            tick(&mut bucket, Role::Coordinator, t + m * 60_000, true);
            tick(&mut bucket, Role::Eval, t + m * 60_000 + 1_000, true);
        }
        // The coordinator's journal wedges; the evaluation Worker's answers.
        for m in 3..6 {
            tick(&mut bucket, Role::Coordinator, t + m * 60_000, false);
            tick(&mut bucket, Role::Eval, t + m * 60_000 + 1_000, true);
        }
        let now = t + 5 * 60_000 + 2_000;
        let coordinator = bucket.get(key(Role::Coordinator));
        assert_eq!(verdict(coordinator, now), "unresponsive");
        assert_eq!(coordinator.map(|r| r.consecutive_failures), Some(3));
        assert_eq!(verdict(bucket.get(key(Role::Eval)), now), "ok");
        // The coordinator's cron stops; the evaluation Worker's keeps running.
        for m in 6..10 {
            tick(&mut bucket, Role::Eval, t + m * 60_000 + 1_000, true);
        }
        let now = t + 9 * 60_000 + 2_000;
        assert_eq!(verdict(bucket.get(key(Role::Coordinator)), now), "stale");
        assert_eq!(verdict(bucket.get(key(Role::Eval)), now), "ok");
    }

    /// The Workers code (`cf.rs`) compiles only for wasm32, so this checks
    /// its source: every read and write of the record uses the invocation's
    /// own deployment role, which comes from `EVAL_ROLE`.
    #[test]
    fn the_worker_reads_and_writes_only_its_own_role_record() {
        let cf = include_str!("cf.rs");
        let uses: Vec<&str> = cf
            .match_indices("liveness::key(")
            .map(|(i, _)| cf.get(i..).and_then(|r| r.split(')').next()).unwrap_or(""))
            .collect();
        assert_eq!(uses, ["liveness::key(d.role", "liveness::key(d.role"]);
        assert!(!cf.contains("chain-journal-liveness"));
        assert_eq!(cf.matches("role: role(env),").count(), 1);
        assert_eq!(
            cf.matches("config::role(var(env, \"EVAL_ROLE\").as_deref())")
                .count(),
            1
        );
        let eval = include_str!("cf_eval.rs");
        assert!(eval.contains("super::role(env) == crate::config::Role::Eval"));
    }

    #[test]
    fn the_record_bound_holds_the_widest_record() {
        let widest = Liveness {
            checked_at_ms: u64::MAX,
            ok: false,
            latency_ms: u64::MAX,
            status: Some(u16::MAX),
            consecutive_failures: u64::MAX,
            first_failure_ms: Some(u64::MAX),
            last_ok_ms: Some(u64::MAX),
        };
        let n = serde_json::to_vec(&widest).unwrap().len();
        assert!(n * 2 <= MAX_RECORD_BYTES, "widest record {n} B");
    }
}
