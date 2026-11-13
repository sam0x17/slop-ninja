//! The account Worker's arena routes and footprints (Δ21). Compiled natively
//! and for wasm32, so the numbers the model test checks are the ones the
//! Worker admits.
//!
//! Every invocation runs in its own bounded scope. The request body is read
//! with a bound ([`BODY_BYTES`], [`crate::ops::OP_BODY_MAX`]) and every
//! provider answer with auth-core's bounds (`TOKEN_BODY_MAX`,
//! `PROFILE_BODY_MAX`, `AVATAR_MAX`), so the fixed footprints below hold the
//! largest of them with room for the parse. The list routes (account views,
//! export, signed ops) hold an account's rows; an account is small (a
//! handful of identities and sessions, its saved links). Each footprint is a
//! stated margin until the inspector measures it on this build, which is why
//! the model is unmeasured and the arena runs in `observe` mode.

use slop_ninja_arena::{AsyncRoute, PAGE, limits};

pub mod id {
    pub const API: u8 = 1;
    pub const LIST: u8 = 2;
    pub const OTHER: u8 = 3;
    pub const SCHEDULED: u8 = 4;
    pub const QUOTA: u8 = 5;
    pub const QUOTA_ALARM: u8 = 6;
}

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;

/// The longest request body read on a browser or internal route: a form,
/// a sign-in body or an internal call is a few hundred bytes.
pub const BODY_BYTES: usize = 64 * KIB;

/// The abandonment deadline (the longest documented Workers wall limit).
pub const ROUTE_MS: u64 = limits::ALARM_MS;

pub const R_API: AsyncRoute = AsyncRoute::new(id::API, "account/api", ROUTE_MS);
pub const R_LIST: AsyncRoute = AsyncRoute::new(id::LIST, "account/list", ROUTE_MS);
pub const R_OTHER: AsyncRoute = AsyncRoute::new(id::OTHER, "account/other", ROUTE_MS);
pub const R_SCHEDULED: AsyncRoute = AsyncRoute::new(id::SCHEDULED, "account/scheduled", ROUTE_MS);
pub const R_QUOTA: AsyncRoute = AsyncRoute::new(id::QUOTA, "account/quota", ROUTE_MS);
pub const R_QUOTA_ALARM: AsyncRoute = AsyncRoute::new(id::QUOTA_ALARM, "account/quota_alarm", ROUTE_MS);

/// A sign-in, session or saved-link route: the body, at most three provider
/// answers (16 KiB token, 64 KiB profile, 64 KiB avatar), their parses, the
/// D1 rows of one flow and the response: about 300 KiB at the bounds,
/// rounded up to 512 KiB.
pub const API_CONTROL_BYTES: usize = 512 * KIB;
/// The list routes: an account's rows and their JSON.
pub const LIST_CONTROL_BYTES: usize = 2 * MIB;
/// A cron run: one batch of at most `sweep::PER_RUN` rows' answers.
pub const SCHEDULED_CONTROL_BYTES: usize = 512 * KIB;
/// A 404, and one `QuotaLimiter` call or alarm: one bucket's state.
pub const SMALL_CONTROL_BYTES: usize = PAGE;

pub const MIN_CONTROL_BYTES: usize = SMALL_CONTROL_BYTES;
pub const LARGEST_INVOCATION_BYTES: usize = LIST_CONTROL_BYTES + PAGE;

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_arena::model;

    #[test]
    fn the_model_matches_the_route_table() {
        assert_eq!(model::MIN_ROUTE_BYTES, MIN_CONTROL_BYTES);
        assert_eq!(model::LARGEST_ROUTE_BYTES, LARGEST_INVOCATION_BYTES);
        const { assert!(model::LARGEST_ROUTE_BYTES < model::LINEAR_MEMORY_BYTES) };
        const { assert!(API_CONTROL_BYTES <= LIST_CONTROL_BYTES && SCHEDULED_CONTROL_BYTES <= LIST_CONTROL_BYTES) };
        const { assert!(BODY_BYTES + 3 * 64 * KIB < API_CONTROL_BYTES) };
    }
}
