//! The client's check of a presigned URL before it sends any byte.
//!
//! The client never builds a URL itself, and never trusts one blindly: the
//! URL must be `https`, on the pinned S3 host (or, with no pin, an R2 S3
//! host), address exactly the key the client derives itself, and carry
//! exactly the seven SigV4 query parameters with the four signed headers of
//! [`crate::presign`]. The client then sends exactly the three headers of
//! [`put_headers`] (plus `host`), computed from its own committed values.

use crate::buf::{Buf, base64_32, decimal};
use crate::policy::R2_MAX_VALID_SECS;
use crate::presign::{IF_NONE_MATCH, SIGNED_HEADERS, valid_bucket, valid_host};
use crate::sigv4::{ALGORITHM, UNSIGNED_PAYLOAD};

/// The suffix of every R2 S3 API host.
pub const R2_S3_SUFFIX: &str = ".r2.cloudflarestorage.com";

/// Why a URL was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlError {
    Scheme,
    Host,
    Path,
    Query,
}

/// What the check learned from a URL it accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckedUrl<'a> {
    pub host: &'a str,
    pub bucket: &'a str,
    pub expires: u32,
    /// `yyyymmddThhmmssZ`.
    pub date: &'a str,
}

/// Checks `url` against the key the client derived (`expected_key`), the
/// pinned host if any, and the bucket if the client knows it.
pub fn check_put_url<'a>(
    url: &'a str,
    pinned_host: Option<&str>,
    expected_bucket: Option<&str>,
    expected_key: &str,
) -> Result<CheckedUrl<'a>, UrlError> {
    let rest = url.strip_prefix("https://").ok_or(UrlError::Scheme)?;
    let (authority, rest) = rest.split_once('/').ok_or(UrlError::Path)?;
    if !valid_host(authority) {
        return Err(UrlError::Host);
    }
    match pinned_host {
        Some(pin) if authority != pin => return Err(UrlError::Host),
        None if !authority.ends_with(R2_S3_SUFFIX) || authority.len() == R2_S3_SUFFIX.len() => {
            return Err(UrlError::Host);
        }
        _ => {}
    }
    let (path, query) = rest.split_once('?').ok_or(UrlError::Query)?;
    let (bucket, key) = path.split_once('/').ok_or(UrlError::Path)?;
    if !valid_bucket(bucket) || expected_bucket.is_some_and(|b| b != bucket) || key != expected_key {
        return Err(UrlError::Path);
    }
    if query.contains('#') {
        return Err(UrlError::Query);
    }
    let mut seen = 0u8;
    let mut expires = 0u32;
    let mut date = "";
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').ok_or(UrlError::Query)?;
        let bit = match name {
            "X-Amz-Algorithm" if value == ALGORITHM => 1,
            "X-Amz-Content-Sha256" if value == UNSIGNED_PAYLOAD => 2,
            "X-Amz-Credential" if credential_shape(value) => 4,
            "X-Amz-Date" if date_shape(value) => {
                date = value;
                8
            }
            "X-Amz-Expires" => {
                expires = crate::layout::parse_decimal(value)
                    .filter(|&e| (1..=R2_MAX_VALID_SECS).contains(&e))
                    .and_then(|e| u32::try_from(e).ok())
                    .ok_or(UrlError::Query)?;
                16
            }
            "X-Amz-Signature" if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) => 32,
            "X-Amz-SignedHeaders" if signed_headers_match(value) => 64,
            _ => return Err(UrlError::Query),
        };
        if seen & bit != 0 {
            return Err(UrlError::Query);
        }
        seen |= bit;
    }
    if seen != 127 {
        return Err(UrlError::Query);
    }
    Ok(CheckedUrl { host: authority, bucket, expires, date })
}

fn signed_headers_match(encoded: &str) -> bool {
    let mut parts = encoded.split("%3B");
    let mut want = SIGNED_HEADERS.split(';');
    loop {
        match (parts.next(), want.next()) {
            (None, None) => return true,
            (Some(a), Some(b)) if a == b => {}
            _ => return false,
        }
    }
}

fn date_shape(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() == 16 && b[8] == b'T' && b[15] == b'Z' && b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit)
}

fn credential_shape(v: &str) -> bool {
    let mut parts = v.split("%2F");
    let ok = parts.next().is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()))
        && parts.next().is_some_and(|d| d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()))
        && parts.next() == Some("auto")
        && parts.next() == Some("s3")
        && parts.next() == Some("aws4_request");
    ok && parts.next().is_none()
}

/// The headers a client sends with a presigned PUT, from its own committed
/// length and digest: `content-length`, `if-none-match` and
/// `x-amz-checksum-sha256` (the HTTP stack adds `host`). Nothing else
/// `x-amz-*`, no `content-encoding`, and never chunked transfer encoding.
pub fn put_headers(len: u64, sha256: &[u8; 32]) -> (Buf<20>, &'static str, Buf<44>) {
    (decimal(len), IF_NONE_MATCH, base64_32(sha256))
}
