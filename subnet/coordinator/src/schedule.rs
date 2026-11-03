//! The bounded scheduling step each block drives.
//!
//! Every new delivery, and every fallback round that advances the cursor, runs
//! one step inside the same storage transaction as the ingest, so the step's
//! decisions commit or roll back with the block that triggered them. A step
//! only decides which operations are due and claims them (see
//! [`crate::claims`]); the claimed work itself runs after the acknowledgment,
//! in resumable pieces that fit one invocation.
//!
//! Due operations are judged only at the RPC-confirmed watermark
//! ([`crate::journal::confirmed`]), never at the provisional cursor that
//! authenticated deliveries advance: notifications alone establish neither
//! finality nor authorization. A cohort boundary therefore starts work at most
//! about one watchdog tick after its delivery, when the tick's head audit (or a
//! fallback hash list) confirms it, and the extra RPC cost is nil.
//!
//! No operation is due yet: cohort transitions and scoring arrive with M4.
//! The coordinator's own weight commits are planned by `crate::signing`, under
//! the separate `signer` claim. The hook is wired and counted so its
//! transaction boundary and claim path are exercised now.

use crate::claims::{self, ClaimOutcome, ClaimToken};
use crate::sql::{Sql, StoreError};
use crate::wire::BlockRef;

/// Upper bound on operations one step claims.
pub const MAX_CLAIMS_PER_STEP: usize = 4;

/// Operations due at the RPC-confirmed watermark `confirmed`. Empty until
/// cohort scheduling exists.
pub fn due_operations(_confirmed: Option<BlockRef>) -> Vec<String> {
    Vec::new()
}

/// Runs one bounded step for `due` and returns the claims this invocation now
/// holds. Operations held by another invocation or already done are skipped.
pub fn step(
    sql: &impl Sql,
    due: &[String],
    holder: &str,
    lease_ms: u64,
    now_ms: u64,
) -> Result<Vec<ClaimToken>, StoreError> {
    crate::journal::bump(sql, "schedule_steps")?;
    let mut held = Vec::new();
    for op in due.iter().take(MAX_CLAIMS_PER_STEP) {
        if let ClaimOutcome::Claimed(token) = claims::claim(sql, op, holder, lease_ms, now_ms)? {
            held.push(token);
        }
    }
    Ok(held)
}
