//! The direct-upload object layout (`admission/v2/`) in the private
//! admission bucket, the health canary's keys, and the public layout that
//! publication copies into.
//!
//! Every key is derived from protocol values, never chosen by a client:
//!
//! ```text
//! admission/v2/c<cohort decimal>/<commitment hex64>/h                     header bytes H
//! admission/v2/c<cohort decimal>/<commitment hex64>/f/<file sha256 hex64> one file
//! health/c<cohort decimal>/<nonce hex64>                                  the upload-path canary
//! reference/v2/files/<file sha256 hex64>                                  one operator-staged reference file
//! ```
//!
//! A presigned PUT binds the key and the SHA-256 R2 must verify, and the
//! file key carries that same digest, so under one commitment a key can only
//! ever hold bytes whose SHA-256 the key names (the header key's digest is
//! the envelope's `header_sha256`).
//!
//! Reference files (the operator's `R_k` packages, `slop_ninja_reference_staging_v1`)
//! sit in the same bucket under [`REFERENCE_PREFIX`], keyed by their digest
//! alone: outside [`ADMISSION_PREFIX`], so no object-create event, close
//! sweep or orphan reconciliation names them, and shared by every package
//! that lists the same file.

use core::fmt::Write;

use crate::buf::{Buf, decimal, hex32, parse_hex32};

/// Prefix of every uploaded artifact object (the R2 event rule's prefix).
pub const ADMISSION_PREFIX: &str = "admission/v2/";
/// Prefix of the canary objects (outside [`ADMISSION_PREFIX`], so no
/// object-create event names them).
pub const HEALTH_PREFIX: &str = "health/";

/// Prefix of the operator-staged reference files (outside
/// [`ADMISSION_PREFIX`] and [`HEALTH_PREFIX`]).
pub const REFERENCE_PREFIX: &str = "reference/v2/files/";

/// The longest key: the prefix, `c` and a 20-digit `u64`, `/`, the
/// commitment, `/f/` and a file digest.
pub const MAX_KEY_LEN: usize = ADMISSION_PREFIX.len() + 1 + 20 + 1 + 64 + 3 + 64;
const _: () = assert!(MAX_KEY_LEN <= 1024, "R2's key limit is 1,024 bytes");
const _: () = assert!(REFERENCE_PREFIX.len() + 64 <= MAX_KEY_LEN);

/// What one admission object holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    /// The header bytes `H`.
    Header,
    /// One file, by its SHA-256.
    File([u8; 32]),
}

/// One admission object's identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectId {
    pub cohort: u64,
    pub commitment: [u8; 32],
    pub kind: ObjectKind,
}

impl ObjectId {
    /// The object's key.
    pub fn key(&self) -> Buf<MAX_KEY_LEN> {
        let mut out = Buf::new();
        let _ = write_session_prefix(&mut out, self.cohort, &self.commitment);
        let _ = match self.kind {
            ObjectKind::Header => out.push("h"),
            ObjectKind::File(sha) => out.push("f/").and_then(|_| out.push(hex32(&sha).as_str())),
        };
        out
    }

    /// Parses a key of this layout; `None` for anything else (including a
    /// non-canonical cohort decimal).
    pub fn parse(key: &str) -> Option<Self> {
        let rest = key.strip_prefix(ADMISSION_PREFIX)?.strip_prefix('c')?;
        let (cohort, rest) = rest.split_once('/')?;
        let cohort = parse_decimal(cohort)?;
        let (commitment, rest) = rest.split_once('/')?;
        let commitment = parse_hex32(commitment)?;
        let kind = match rest {
            "h" => ObjectKind::Header,
            _ => ObjectKind::File(parse_hex32(rest.strip_prefix("f/")?)?),
        };
        Some(Self { cohort, commitment, kind })
    }
}

/// Writes `admission/v2/c<cohort>/`.
pub fn write_cohort_prefix(out: &mut impl Write, cohort: u64) -> core::fmt::Result {
    write!(out, "{ADMISSION_PREFIX}c{}/", decimal(cohort).as_str())
}

/// Writes `admission/v2/c<cohort>/<commitment>/`.
pub fn write_session_prefix(out: &mut impl Write, cohort: u64, commitment: &[u8; 32]) -> core::fmt::Result {
    write_cohort_prefix(out, cohort)?;
    write!(out, "{}/", hex32(commitment).as_str())
}

/// Canonical decimal: digits only, no leading zero except `0` itself.
pub fn parse_decimal(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.is_empty() || b.len() > 20 || (b.len() > 1 && b[0] == b'0') || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    text.parse().ok()
}

/// The canary's key: `health/c<cohort>/<nonce hex>`.
pub fn health_key(cohort: u64, nonce: &[u8; 32]) -> Buf<MAX_KEY_LEN> {
    let mut out = Buf::new();
    let _ = write!(out, "{HEALTH_PREFIX}c{}/{}", decimal(cohort).as_str(), hex32(nonce).as_str());
    out
}

/// A reference file's key: `reference/v2/files/<sha256 hex>`.
pub fn reference_key(sha256: &[u8; 32]) -> Buf<MAX_KEY_LEN> {
    let mut out = Buf::new();
    let _ = write!(out, "{REFERENCE_PREFIX}{}", hex32(sha256).as_str());
    out
}

/// The digest a reference file's key names; `None` for any other key
/// (including upper-case hex).
pub fn parse_reference_key(key: &str) -> Option<[u8; 32]> {
    parse_hex32(key.strip_prefix(REFERENCE_PREFIX)?)
}

/// Domain of the canary body.
pub const CANARY_DOMAIN: &[u8; 32] = b"slop_ninja.upload-canary/v1\0\0\0\0\0";
/// The canary body: the domain, then the nonce.
pub const CANARY_LEN: usize = 64;

/// The fixed canary body for `nonce` (our own bytes, never a miner's).
pub fn canary_body(nonce: &[u8; 32]) -> [u8; CANARY_LEN] {
    let mut out = [0u8; CANARY_LEN];
    out[..32].copy_from_slice(CANARY_DOMAIN);
    out[32..].copy_from_slice(nonce);
    out
}

/// The public bucket's layout v2: the header object by its digest.
pub fn public_header_key(header_sha256: &[u8; 32]) -> Buf<MAX_KEY_LEN> {
    let mut out = Buf::new();
    let _ = write!(out, "artifacts/v2/{}/header", hex32(header_sha256).as_str());
    out
}

/// The public bucket's layout v2: one file, shared by digest.
pub fn public_blob_key(sha256: &[u8; 32]) -> Buf<MAX_KEY_LEN> {
    let mut out = Buf::new();
    let _ = write!(out, "blobs/sha256/{}", hex32(sha256).as_str());
    out
}
