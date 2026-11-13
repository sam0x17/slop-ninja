//! The SlopNinja account Worker.
//!
//! Sign-in by email link, Google, GitHub, X and a Bittensor wallet; sessions;
//! identities, linking and merging; saved links ("my detections"); export and
//! deletion; and the signed account ops. Its stores are the D1
//! `account-store` (`migrations/0001_account_store.sql`) and the
//! `QuotaLimiter` Durable Object. It binds no other Worker: work it starts
//! on detections (an account's deletion, a merge) goes on the `detect-jobs`
//! queue as content-free messages that carry user ids only.
//!
//! The contracts it serves are fixed in `slop-ninja-account-wire` (views,
//! saved links, sign-in bodies, quota buckets), `slop-ninja-detect-page`
//! (refusal codes, `X-SN-*` headers, origins, queue messages, metric points,
//! switches) and `slop-ninja-signed-op` (signed ops). The pure sign-in pieces
//! are `slop-ninja-auth-core`'s.
//!
//! Layout:
//!
//! - [`routes`]: the one entry, internal and forwarded routes, the CSRF and
//!   content-type checks;
//! - sign-in: [`flows`] (OAuth start and callback, X's avatar), [`email`],
//!   [`wallet`], and [`signin`] (what a proven identity does under each
//!   intent), [`sessions`];
//! - account data: [`link`] (connect, merge, unlink), [`owned`] (saved
//!   links), [`views`] (the account pages' data), [`account`] (sign out,
//!   sessions, export, deletion), [`delete`];
//! - [`quota`] (the `QuotaLimiter` object's logic), [`ops`] (signed ops),
//!   [`sweep`] (the crons), [`journal`], [`metrics`];
//! - [`host`]: the platform seam the handlers run on; `cf.rs` (wasm32) is
//!   its Cloudflare implementation and the hand-written exports on the
//!   bounded arena ([`memory`], Δ21).
//!
//! **Privacy.** No answer, metric or journal row carries a token, an
//! address, a handle, a slug or a provider subject, except an account's own
//! data in its own views and export. Panics write nothing
//! ([`silence_panics`]); the Worker writes no console line.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing))]

pub mod account;
pub mod config;
pub mod delete;
pub mod email;
pub mod flows;
pub mod host;
pub mod http;
pub mod journal;
pub mod keys;
pub mod link;
pub mod memory;
pub mod metrics;
pub mod ops;
pub mod owned;
pub mod quota;
pub mod routes;
pub mod sessions;
pub mod signin;
pub mod store;
pub mod sweep;
pub mod views;
pub mod wallet;

#[cfg(target_arch = "wasm32")]
mod cf;
#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
#[path = "../../coordinator/src/cf_io.rs"]
mod cf_io;

pub use config::{Config, ConfigError};
pub use http::{Req, Resp};
pub use routes::handle;

thread_local! {
    static SILENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Replaces the panic hook with one that writes nothing, once per instance.
/// workers-rs installs a hook that prints the panic message; every entry
/// calls this first, so a panic never writes an address or a token
/// anywhere. A panic still aborts the instance.
pub fn silence_panics() {
    SILENT.with(|s| {
        if !s.get() {
            std::panic::set_hook(Box::new(|_| {}));
            s.set(true);
        }
    });
}

/// 32 lowercase hex digits as 16 bytes.
pub fn hex16(s: &str) -> Option<[u8; 16]> {
    slop_ninja_detect_page::UserId::parse(s).map(|u| u.0)
}
