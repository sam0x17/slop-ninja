//! The upload URL policy, versioned and frozen in each cohort plan
//! (`slop_ninja_upload_url_policy_v1`): how long one presigned URL lives.
//!
//! `valid_secs = ceil(L_total / B_MIN) + SKEW_SECS`, where `L_total` is the
//! committed size of the whole artifact (header plus every file, so a client
//! that takes every URL at once and starts them in sequence at [`B_MIN`] is
//! covered).
//!
//! R2 checks a presigned URL's expiry when the request starts, not when its
//! body ends (qualification G6, 2026-11-05: a 40.7 s body that ended about
//! 30 s after expiry was stored). A PUT that started in time therefore
//! completes at any rate, and no transfer rate is a condition of progress:
//! [`B_MIN`] only sets how long an unused URL waits. Expiry ends one URL
//! only, and a miner whose commitment is valid gets a fresh one on every
//! request, with no count or time refusal. A request started just before
//! expiry may trickle on for as long as its sender likes, so nothing may
//! assume a bounded request lifetime (orphan reconciliation never ends).

/// Policy identifier.
pub const POLICY_ID: &str = "slop_ninja_upload_url_policy_v1";
/// The slowest uplink one URL life covers: 1 Mbit/s. A policy value, not a
/// cap on miners: R2 checks a URL's expiry when a request starts
/// (qualification G6b-c), so a slower transfer started in time completes,
/// and a miner whose URL expired before it started asks for a fresh one.
pub const B_MIN: u64 = 125_000;
/// The margin from minting to the request's start, plus clock skew: SigV4's
/// usual 15-minute request-time tolerance, which R2 matches (qualification
/// G6d, 2026-11-05: a future-dated `X-Amz-Date` up to about 900 s was
/// accepted, beyond it refused `RequestTimeTooSkewed`).
pub const SKEW_SECS: u64 = 900;
/// R2's longest presigned URL life (7 days).
pub const R2_MAX_VALID_SECS: u64 = 604_800;
/// R2's single-part upload limit: 5 GiB less 5 MiB.
pub const R2_SINGLE_PUT_MAX: u64 = 5 * 1024 * 1024 * 1024 - 5 * 1024 * 1024;
/// The largest committed artifact the policy can serve at [`B_MIN`]:
/// beyond it a URL at `B_MIN` would outlive R2's maximum, which needs a new
/// policy version (a static test pins every compiled spec below it).
pub const MAX_L_AT_B_MIN: u64 = (R2_MAX_VALID_SECS - SKEW_SECS) * B_MIN;

/// Why no validity was computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyError {
    /// `L_total` is zero.
    Empty,
    /// `L_total` exceeds [`MAX_L_AT_B_MIN`].
    BeyondPolicy,
}

/// The validity of one URL for an artifact of `l_total` committed bytes.
pub fn valid_secs(l_total: u64) -> Result<u32, PolicyError> {
    if l_total == 0 {
        return Err(PolicyError::Empty);
    }
    if l_total > MAX_L_AT_B_MIN {
        return Err(PolicyError::BeyondPolicy);
    }
    let secs = l_total.div_ceil(B_MIN) + SKEW_SECS;
    // At most R2's maximum, since `l_total <= MAX_L_AT_B_MIN`.
    Ok(u32::try_from(secs.min(R2_MAX_VALID_SECS)).unwrap_or(R2_MAX_VALID_SECS as u32))
}
