//! AWS Signature Version 4, allocation-free.
//!
//! The canonical request is never materialized: [`write_canonical`] writes it
//! into any [`fmt::Write`] sink, and [`canonical_digest`] streams it straight
//! into SHA-256. Tests write it into a [`crate::Buf`] to compare it with the
//! published AWS examples.
//!
//! Only the forms this crate signs are supported: a path of `/`-separated
//! segments, query parameters given already sorted by their encoded name,
//! and lowercase header names given sorted with trimmed single-line values.
//! [`write_canonical`] refuses anything else rather than signing a request
//! the server would canonicalize differently.

use core::fmt::{self, Write};

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::buf::{Buf, write_hex};

pub const ALGORITHM: &str = "AWS4-HMAC-SHA256";
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
pub const TERMINATOR: &str = "aws4_request";
/// SHA-256 of the empty string, hex: the payload hash of a bodiless request.
pub const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

type HmacSha256 = Hmac<Sha256>;

/// A request to canonicalize. `path` is the raw (unencoded) absolute path;
/// query names and values are raw too, and are URI-encoded here.
#[derive(Clone, Copy, Debug)]
pub struct Canonical<'a> {
    pub method: &'a str,
    pub path: &'a str,
    /// Sorted by encoded name (for the names this crate uses, the raw and
    /// encoded orders agree; [`write_canonical`] checks the raw order).
    pub query: &'a [(&'a str, &'a str)],
    /// Lowercase names, sorted, unique; values trimmed, without line breaks.
    pub headers: &'a [(&'a str, &'a str)],
    /// The payload hash line: lowercase hex, or [`UNSIGNED_PAYLOAD`].
    pub payload: &'a str,
}

/// The credential scope: `yyyymmdd/region/service/aws4_request`.
#[derive(Clone, Copy, Debug)]
pub struct CredentialScope<'a> {
    pub date: &'a str,
    pub region: &'a str,
    pub service: &'a str,
}

impl CredentialScope<'_> {
    pub fn write(&self, out: &mut impl Write) -> fmt::Result {
        write!(out, "{}/{}/{}/{TERMINATOR}", self.date, self.region, self.service)
    }
}

fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

/// URI-encodes `s` as SigV4 requires: every byte except the unreserved set
/// (and `/` when `keep_slash`) as `%XX` with uppercase hex.
pub fn uri_encode(out: &mut impl Write, s: &str, keep_slash: bool) -> fmt::Result {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if unreserved(b) || (keep_slash && b == b'/') {
            continue;
        }
        out.write_str(s.get(start..i).ok_or(fmt::Error)?)?;
        let esc = [b'%', HEX[(b >> 4) as usize], HEX[(b & 15) as usize]];
        out.write_str(core::str::from_utf8(&esc).map_err(|_| fmt::Error)?)?;
        start = i + 1;
    }
    out.write_str(s.get(start..).ok_or(fmt::Error)?)
}

/// Why a request was not canonicalized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanonicalError {
    /// A path that is not absolute.
    Path,
    /// Query parameters out of order or repeated.
    QueryOrder,
    /// A header name not lowercase, out of order or repeated, or a value with
    /// surrounding space or a line break.
    Header,
    /// The sink refused (a fixed buffer was too small).
    Sink,
}

impl From<fmt::Error> for CanonicalError {
    fn from(_: fmt::Error) -> Self {
        Self::Sink
    }
}

fn check(c: &Canonical<'_>) -> Result<(), CanonicalError> {
    if !c.path.starts_with('/') {
        return Err(CanonicalError::Path);
    }
    for pair in c.query.windows(2) {
        if pair[0].0 >= pair[1].0 {
            return Err(CanonicalError::QueryOrder);
        }
    }
    for (name, value) in c.headers {
        let lower = !name.is_empty() && name.bytes().all(|b| !b.is_ascii_uppercase() && b > b' ' && b != b':');
        let trimmed = value.trim() == *value && !value.bytes().any(|b| b == b'\n' || b == b'\r');
        if !lower || !trimmed {
            return Err(CanonicalError::Header);
        }
    }
    for pair in c.headers.windows(2) {
        if pair[0].0 >= pair[1].0 {
            return Err(CanonicalError::Header);
        }
    }
    Ok(())
}

/// Writes `name;name;...` of the request's headers.
pub fn write_signed_headers(out: &mut impl Write, headers: &[(&str, &str)]) -> fmt::Result {
    for (i, (name, _)) in headers.iter().enumerate() {
        if i > 0 {
            out.write_str(";")?;
        }
        out.write_str(name)?;
    }
    Ok(())
}

/// Writes the canonical request.
pub fn write_canonical(out: &mut impl Write, c: &Canonical<'_>) -> Result<(), CanonicalError> {
    check(c)?;
    out.write_str(c.method)?;
    out.write_str("\n")?;
    uri_encode(out, c.path, true)?;
    out.write_str("\n")?;
    for (i, (name, value)) in c.query.iter().enumerate() {
        if i > 0 {
            out.write_str("&")?;
        }
        uri_encode(out, name, false)?;
        out.write_str("=")?;
        uri_encode(out, value, false)?;
    }
    out.write_str("\n")?;
    for (name, value) in c.headers {
        writeln!(out, "{name}:{value}")?;
    }
    out.write_str("\n")?;
    write_signed_headers(out, c.headers)?;
    out.write_str("\n")?;
    out.write_str(c.payload)?;
    Ok(())
}

/// A [`fmt::Write`] sink that hashes what is written.
pub struct HashWriter(pub Sha256);

impl Write for HashWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.update(s.as_bytes());
        Ok(())
    }
}

/// SHA-256 of the canonical request, streamed (no buffer).
pub fn canonical_digest(c: &Canonical<'_>) -> Result<[u8; 32], CanonicalError> {
    let mut w = HashWriter(Sha256::new());
    write_canonical(&mut w, c)?;
    Ok(w.0.finalize().into())
}

/// Writes the string to sign.
pub fn write_string_to_sign(
    out: &mut impl Write,
    amz_date: &str,
    scope: &CredentialScope<'_>,
    canonical_sha256: &[u8; 32],
) -> fmt::Result {
    write!(out, "{ALGORITHM}\n{amz_date}\n")?;
    scope.write(out)?;
    out.write_str("\n")?;
    write_hex(out, canonical_sha256)
}

fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
    // HMAC accepts a key of any length, so this never takes the error arm;
    // if it did, an all-zero MAC fails verification at the server (closed).
    let Ok(mut mac) = <HmacSha256 as Mac>::new_from_slice(key) else {
        return [0; 32];
    };
    mac.update(msg);
    mac.finalize().into_bytes().into()
}

/// HMAC-SHA256 keyed by `"AWS4" || secret`, without concatenating into a
/// heap buffer: a key longer than SHA-256's 64-byte block is replaced by its
/// digest, exactly as HMAC itself does.
fn hmac_aws4(secret: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    if 4 + secret.len() <= BLOCK {
        let mut key = [0u8; BLOCK];
        key[..4].copy_from_slice(b"AWS4");
        key[4..4 + secret.len()].copy_from_slice(secret);
        let out = hmac(&key[..4 + secret.len()], msg);
        key.fill(0);
        out
    } else {
        let mut h = Sha256::new();
        h.update(b"AWS4");
        h.update(secret);
        let mut key: [u8; 32] = h.finalize().into();
        let out = hmac(&key, msg);
        key.fill(0);
        out
    }
}

/// The SigV4 signing key for `secret` and a credential scope.
pub fn signing_key(secret: &[u8], scope: &CredentialScope<'_>) -> [u8; 32] {
    let k_date = hmac_aws4(secret, scope.date.as_bytes());
    let k_region = hmac(&k_date, scope.region.as_bytes());
    let k_service = hmac(&k_region, scope.service.as_bytes());
    hmac(&k_service, TERMINATOR.as_bytes())
}

/// The longest string to sign: algorithm, a 16-character date, a scope of
/// at most [`MAX_SCOPE_LEN`] and a 64-character digest, with three line
/// breaks.
pub const MAX_SCOPE_LEN: usize = 8 + 1 + MAX_REGION_LEN + 1 + MAX_SERVICE_LEN + 1 + TERMINATOR.len();
/// Regions and services this crate signs for are short fixed names (`auto`
/// and `s3` for R2; the AWS vectors use `us-east-1` and `iam`).
pub const MAX_REGION_LEN: usize = 32;
pub const MAX_SERVICE_LEN: usize = 32;
const STRING_TO_SIGN_LEN: usize = ALGORITHM.len() + 1 + 16 + 1 + MAX_SCOPE_LEN + 1 + 64;

/// The hex signature of a canonical request digest under `secret`.
pub fn signature(
    secret: &[u8],
    amz_date: &str,
    scope: &CredentialScope<'_>,
    canonical_sha256: &[u8; 32],
) -> Result<Buf<64>, CanonicalError> {
    if scope.region.len() > MAX_REGION_LEN || scope.service.len() > MAX_SERVICE_LEN || scope.date.len() != 8 {
        return Err(CanonicalError::Sink);
    }
    let mut sts: Buf<STRING_TO_SIGN_LEN> = Buf::new();
    write_string_to_sign(&mut sts, amz_date, scope, canonical_sha256)?;
    let mut key = signing_key(secret, scope);
    let mac = hmac(&key, sts.as_bytes());
    key.fill(0);
    let mut out = Buf::new();
    write_hex(&mut out, &mac)?;
    Ok(out)
}

/// The `Authorization` header value of a header-signed request.
pub fn write_authorization(
    out: &mut impl Write,
    access_key_id: &str,
    scope: &CredentialScope<'_>,
    headers: &[(&str, &str)],
    signature: &str,
) -> fmt::Result {
    write!(out, "{ALGORITHM} Credential={access_key_id}/")?;
    scope.write(out)?;
    out.write_str(",SignedHeaders=")?;
    write_signed_headers(out, headers)?;
    write!(out, ",Signature={signature}")
}
