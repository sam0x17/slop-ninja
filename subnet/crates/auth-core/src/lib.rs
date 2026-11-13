//! Pure sign-in pieces for the SlopNinja account Worker.
//!
//! Everything here is storage-neutral and I/O-free: the Worker owns D1, the
//! Durable Object quota counter, the network fetches and the clock, and calls
//! these functions with what it read. Every input is bounded before it is
//! parsed, every random value comes from the caller (the Worker draws it from
//! the runtime's `crypto.getRandomValues`), and every time is a millisecond
//! count the caller passes in, so each check is a plain function that native
//! tests drive directly.
//!
//! Modules:
//!
//! - [`token`]: 256-bit random tokens (`state`, flow binding, session, email
//!   link, wallet challenge id) and their stored SHA-256 hashes;
//! - [`pkce`]: S256 code verifiers and challenges;
//! - [`state`]: the OAuth callback's query and the single-use flow check;
//! - [`oidc`]: Google ID token claim checks (`iss` in both forms, `aud`,
//!   `exp`, `nonce`);
//! - [`providers`]: Google, GitHub and X endpoint and parameter tables as
//!   constants, request builders and bounded response parsers;
//! - `siws` (feature `wallet`): the wallet sign-in message and sr25519
//!   verification through `rtao_core::keys::verify_strict`;
//! - [`cookie`]: the two `__Host-` cookies;
//! - [`csrf`]: Fetch Metadata plus `Origin`, against origins from config;
//! - [`email_addr`]: ASCII address rules, A-label domains and `email_norm`;
//! - [`return_to`]: the post-sign-in destination, parsed against the
//!   configured origin and a prefix list;
//! - [`policy`]: `auth-policy/v1`, each value with its reason;
//! - [`mail`]: the mail kinds and their copy, the [`mail::Mailer`] seam, the
//!   Cloudflare Email Sending implementation (wasm32) and a capturing
//!   implementation (feature `dev`).
//!
//! The shared contracts come from the account and detection contract
//! crates, re-exported here as [`account_wire`] and [`detect_page`]: the
//! origins from config ([`Origins`], from `SITE_ORIGIN` and `SECRET_ORIGIN`),
//! the refusal codes every check here maps its refusals to
//! ([`RefusalCode`]), the forwarded route table whose `csrf` column is the
//! CSRF scope, the cookie names, the sign-in enums ([`account_wire::Provider`],
//! [`account_wire::MailIntent`], [`account_wire::WalletMode`],
//! [`account_wire::AvatarType`]) and the quota buckets
//! ([`account_wire::mail_buckets`]). This crate adds the decisions and none
//! of the vocabulary.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod cookie;
pub mod csrf;
pub mod email_addr;
pub mod mail;
pub mod oidc;
pub mod pkce;
pub mod policy;
pub mod providers;
pub mod return_to;
#[cfg(feature = "wallet")]
pub mod siws;
pub mod state;
pub mod time;
pub mod token;

mod b64;

pub use slop_ninja_account_wire as account_wire;
pub use slop_ninja_detect_page as detect_page;
pub use slop_ninja_detect_page::origins::OriginError;
pub use slop_ninja_detect_page::{HostRole, Origin, Origins, RefusalCode, RouteId};

/// Constant-time equality of two byte strings of the same length.
///
/// Used wherever a stored hash or a server-issued value is compared with what
/// a request carried, so the comparison's time does not depend on where the
/// first difference is.
#[must_use]
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}
