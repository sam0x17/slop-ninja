//! The `QuotaLimiter` Durable Object's logic: one token bucket per object.
//!
//! An object is named `HMAC(K_quota, "<kind>:<key>")` (hex), so its name
//! says nothing about the address, account or key it counts. Each object
//! holds one bucket's state; a full bucket is the same as no state, so the
//! object deletes its state when the bucket would be full again (its alarm)
//! and after a reset. The bucket's size comes with each call from the
//! Worker's policy (`auth-policy/v1`), so a policy change needs no migration.
//! A Durable Object runs one call at a time, so takes are strict: the count
//! the Rate Limiting binding keeps per location is not.

use serde::{Deserialize, Serialize};
use slop_ninja_auth_core::policy::{BucketState, Take, TokenBucket};

use crate::host::QuotaTake;

/// One call to an object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuotaCall {
    Take { capacity: u32, refill_every_s: u64 },
    Reset,
}

/// The answer of a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaAnswer {
    pub taken: bool,
    pub emptied: bool,
}

impl From<QuotaAnswer> for QuotaTake {
    fn from(a: QuotaAnswer) -> QuotaTake {
        QuotaTake { taken: a.taken, emptied: a.emptied }
    }
}

/// What the object stores next and when its alarm fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Next {
    /// `None`: delete the state (a full bucket).
    pub state: Option<BucketState>,
    /// When the bucket is full again (the alarm that deletes it).
    pub full_at_ms: Option<u64>,
    pub answer: QuotaAnswer,
}

/// When a bucket in `state` is full again.
pub fn full_at(size: &TokenBucket, state: &BucketState) -> Option<u64> {
    let missing = u64::from(size.capacity.saturating_sub(state.tokens));
    (missing > 0).then(|| state.refilled_at_ms + missing * size.refill_every_s.saturating_mul(1000).max(1))
}

/// Applies one call to the stored state.
pub fn apply(stored: Option<BucketState>, call: QuotaCall, now_ms: u64) -> Next {
    match call {
        QuotaCall::Reset => {
            Next { state: None, full_at_ms: None, answer: QuotaAnswer { taken: false, emptied: false } }
        }
        QuotaCall::Take { capacity, refill_every_s } => {
            let size = TokenBucket { capacity, refill_every_s };
            let (state, answer) = match size.take(stored, now_ms) {
                Take::Taken { state, emptied } => (state, QuotaAnswer { taken: true, emptied }),
                Take::Empty { state } => (state, QuotaAnswer { taken: false, emptied: false }),
            };
            let full_at_ms = full_at(&size, &state);
            Next { state: full_at_ms.map(|_| state), full_at_ms, answer }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: QuotaCall = QuotaCall::Take { capacity: 10, refill_every_s: 8640 };

    #[test]
    fn ten_a_day_then_one_every_two_point_four_hours() {
        let mut st = None;
        let t0 = 1_000_000;
        for i in 0..10 {
            let n = apply(st, SIZE, t0);
            assert!(n.answer.taken);
            assert_eq!(n.answer.emptied, i == 9);
            st = n.state;
        }
        let n = apply(st, SIZE, t0 + 1);
        assert!(!n.answer.taken);
        let n = apply(st, SIZE, t0 + 8_640_000);
        assert!(n.answer.taken && n.answer.emptied);
        assert_eq!(apply(st, QuotaCall::Reset, t0).state, None);
    }

    #[test]
    fn a_full_bucket_stores_nothing_and_the_alarm_is_when_it_refills() {
        let n = apply(None, SIZE, 5);
        assert_eq!(n.full_at_ms, Some(5 + 8_640_000));
        let refilled = apply(n.state, SIZE, 5 + 8_640_000);
        // Taken one again, so it is one short of full once more.
        assert_eq!(refilled.full_at_ms, Some(5 + 2 * 8_640_000));
        let body = serde_json::to_string(&SIZE).unwrap();
        assert_eq!(body, r#"{"op":"take","capacity":10,"refill_every_s":8640}"#);
        assert!(serde_json::from_str::<QuotaCall>(r#"{"op":"take","capacity":1,"refill_every_s":1,"x":1}"#).is_err());
    }
}
