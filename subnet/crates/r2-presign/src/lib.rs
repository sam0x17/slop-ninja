//! Direct-to-R2 upload signing for SlopNinja admission v2.
//!
//! Miners upload each artifact object (the header, then each file) straight
//! to R2 with one presigned `PutObject` each; no upload byte passes through
//! any of our Workers. Each URL binds the derived key, the committed length,
//! the committed SHA-256 (verified by R2 as the body arrives) and
//! create-only, so the miner's pack-time hash is the only SHA-256 any party
//! computes over model bytes, and R2's ingest check is the only verification.
//!
//! - [`sigv4`]: AWS Signature Version 4, streamed into SHA-256 (no
//!   canonical-request buffer), checked against the published AWS vectors;
//! - [`presign`]: the `PutObject` URL, binding four signed headers;
//! - [`copy`]: the `CopyObject` request of publication at scoring start,
//!   binding the derived source and destination, the source etag, fixed
//!   metadata and the storage class;
//! - [`layout`]: the derived object keys (`admission/v2/`), the canary keys
//!   and the public layout;
//! - [`policy`]: the versioned URL validity policy;
//! - [`check`]: the client's check of a URL before it sends a byte;
//! - [`wire`]: the internal signer Worker's fixed-size messages;
//! - [`buf`], [`time`]: fixed buffers and UTC date stamps.
//!
//! Nothing here allocates (the crate is `no_std` outside its tests), so the
//! signer Worker, the eval Worker and the miner share it in any arena.
#![cfg_attr(not(test), no_std)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod buf;
pub mod check;
pub mod copy;
pub mod layout;
pub mod policy;
pub mod presign;
pub mod sigv4;
pub mod time;
pub mod wire;

pub use buf::Buf;
