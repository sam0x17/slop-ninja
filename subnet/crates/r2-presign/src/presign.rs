//! Presigned R2 `PutObject` URLs that bind the exact object.
//!
//! Path style on the account's S3 host, region `auto`, service `s3`, with
//! `X-Amz-Content-Sha256=UNSIGNED-PAYLOAD` in the query as R2's own example
//! has it. The signature binds four headers the client must send exactly:
//!
//! - `content-length`: the committed length;
//! - `host`;
//! - `if-none-match: *`: create only, never overwrite;
//! - `x-amz-checksum-sha256`: the committed SHA-256 (base64), which R2
//!   verifies over the body as it arrives, storing nothing on a mismatch.
//!
//! The payload is unsigned, so the signer never sees or hashes the body. The
//! method is a constant: nothing here signs GET, HEAD, DELETE, POST,
//! multipart or copy requests for a client.

use core::fmt::Write;

use crate::buf::{Buf, base64_32, decimal};
use crate::policy::{R2_MAX_VALID_SECS, R2_SINGLE_PUT_MAX};
use crate::sigv4::{self, ALGORITHM, Canonical, CredentialScope, UNSIGNED_PAYLOAD};
use crate::time::amz_dates;

pub const REGION: &str = "auto";
pub const SERVICE: &str = "s3";
/// The signed headers, in canonical order.
pub const SIGNED_HEADERS: &str = "content-length;host;if-none-match;x-amz-checksum-sha256";
/// The one value of `if-none-match`.
pub const IF_NONE_MATCH: &str = "*";

/// The longest access key id: IAM's documented `AccessKeyId` maximum (R2's
/// are 32 characters).
pub const MAX_ACCESS_KEY_ID_LEN: usize = 128;
/// The longest DNS host name.
pub const MAX_HOST_LEN: usize = 253;
/// The longest S3 bucket name.
pub const MAX_BUCKET_LEN: usize = 63;

/// The longest presigned URL this crate writes: scheme, host, bucket, the
/// longest admission key (no byte of a derived key needs escaping) and the
/// seven query parameters at their format bounds.
pub const MAX_URL_LEN: usize = "https://".len()
    + MAX_HOST_LEN
    + 1
    + MAX_BUCKET_LEN
    + 1
    + crate::layout::MAX_KEY_LEN
    + "?X-Amz-Algorithm=".len()
    + ALGORITHM.len()
    + "&X-Amz-Content-Sha256=".len()
    + UNSIGNED_PAYLOAD.len()
    + "&X-Amz-Credential=".len()
    + MAX_ACCESS_KEY_ID_LEN
    + "%2F20251201%2Fauto%2Fs3%2Faws4_request".len()
    + "&X-Amz-Date=20251201T180512Z".len()
    + "&X-Amz-Expires=604800".len()
    + "&X-Amz-Signature=".len()
    + 64
    + "&X-Amz-SignedHeaders=content-length%3Bhost%3Bif-none-match%3Bx-amz-checksum-sha256".len();

/// The S3 credential (a parent key; temporary credentials are never used for
/// URLs, since their session token would reveal the derived secret).
#[derive(Clone, Copy)]
pub struct Credentials<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a [u8],
}

impl core::fmt::Debug for Credentials<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Credentials(..)")
    }
}

/// Where the object goes.
#[derive(Clone, Copy, Debug)]
pub struct Target<'a> {
    /// The account's S3 host, `<account>.r2.cloudflarestorage.com`.
    pub host: &'a str,
    pub bucket: &'a str,
    /// A key of [`crate::layout`].
    pub key: &'a str,
}

/// What the URL binds the body to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub len: u64,
    pub sha256: [u8; 32],
}

/// Why no URL was signed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresignError {
    /// Length zero or above R2's single-PUT limit.
    Length,
    /// Validity outside 1 s to 7 days.
    Validity,
    /// A host, bucket, key or access key id outside its format.
    Format,
    /// A clock outside SigV4's four-digit years.
    Clock,
}

/// A signed URL and the headers that must accompany it.
#[derive(Clone, Debug)]
pub struct Presigned {
    pub url: Buf<MAX_URL_LEN>,
    /// `x-amz-checksum-sha256`.
    pub checksum: Buf<44>,
    pub content_length: u64,
    /// Unix seconds of `X-Amz-Date`.
    pub signed_at: u64,
    pub valid_secs: u32,
}

/// Host names: lowercase letters, digits, `.` and `-`.
pub fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= MAX_HOST_LEN
        && host.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        && !host.starts_with('.')
        && !host.ends_with('.')
}

/// S3 bucket names: 3 to 63 of lowercase letters, digits and `-`, starting
/// and ending alphanumeric (R2 allows no `.`).
pub fn valid_bucket(bucket: &str) -> bool {
    let b = bucket.as_bytes();
    (3..=MAX_BUCKET_LEN).contains(&b.len())
        && b.iter().all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
}

pub(crate) fn valid_access_key_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ACCESS_KEY_ID_LEN && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub(crate) fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= crate::layout::MAX_KEY_LEN
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b"/-._".contains(&b))
        && !key.starts_with('/')
}

/// Signs a `PutObject` URL for `target` bound to `binding`, valid for
/// `valid_secs` from `now_unix`.
pub fn presign_put(
    cred: &Credentials<'_>,
    target: &Target<'_>,
    binding: &Binding,
    now_unix: u64,
    valid_secs: u32,
) -> Result<Presigned, PresignError> {
    if binding.len == 0 || binding.len > R2_SINGLE_PUT_MAX {
        return Err(PresignError::Length);
    }
    if valid_secs == 0 || u64::from(valid_secs) > R2_MAX_VALID_SECS {
        return Err(PresignError::Validity);
    }
    if !valid_host(target.host)
        || !valid_bucket(target.bucket)
        || !valid_key(target.key)
        || !valid_access_key_id(cred.access_key_id)
    {
        return Err(PresignError::Format);
    }
    let (date, stamp) = amz_dates(now_unix).ok_or(PresignError::Clock)?;
    let scope = CredentialScope { date: date.as_str(), region: REGION, service: SERVICE };

    let mut credential: Buf<{ MAX_ACCESS_KEY_ID_LEN + 1 + sigv4::MAX_SCOPE_LEN }> = Buf::new();
    write!(credential, "{}/", cred.access_key_id).map_err(|_| PresignError::Format)?;
    scope.write(&mut credential).map_err(|_| PresignError::Format)?;
    let expires = decimal(u64::from(valid_secs));
    let checksum = base64_32(&binding.sha256);
    let length = decimal(binding.len);

    let mut path: Buf<{ 2 + MAX_BUCKET_LEN + crate::layout::MAX_KEY_LEN }> = Buf::new();
    write!(path, "/{}/{}", target.bucket, target.key).map_err(|_| PresignError::Format)?;

    let query = [
        ("X-Amz-Algorithm", ALGORITHM),
        ("X-Amz-Content-Sha256", UNSIGNED_PAYLOAD),
        ("X-Amz-Credential", credential.as_str()),
        ("X-Amz-Date", stamp.as_str()),
        ("X-Amz-Expires", expires.as_str()),
        ("X-Amz-SignedHeaders", SIGNED_HEADERS),
    ];
    let headers = [
        ("content-length", length.as_str()),
        ("host", target.host),
        ("if-none-match", IF_NONE_MATCH),
        ("x-amz-checksum-sha256", checksum.as_str()),
    ];
    let canonical =
        Canonical { method: "PUT", path: path.as_str(), query: &query, headers: &headers, payload: UNSIGNED_PAYLOAD };
    let digest = sigv4::canonical_digest(&canonical).map_err(|_| PresignError::Format)?;
    let signature =
        sigv4::signature(cred.secret_access_key, stamp.as_str(), &scope, &digest).map_err(|_| PresignError::Format)?;

    // The URL lists the parameters sorted by name, signature included, as
    // R2's documented example does.
    let mut url = Buf::new();
    let write = (|| {
        write!(url, "https://{}", target.host)?;
        sigv4::uri_encode(&mut url, path.as_str(), true)?;
        for (i, (name, value)) in query.iter().enumerate() {
            url.push(if i == 0 { "?" } else { "&" })?;
            if *name == "X-Amz-SignedHeaders" {
                write!(url, "X-Amz-Signature={}&", signature.as_str())?;
            }
            sigv4::uri_encode(&mut url, name, false)?;
            url.push("=")?;
            sigv4::uri_encode(&mut url, value, false)?;
        }
        Ok::<(), core::fmt::Error>(())
    })();
    write.map_err(|_| PresignError::Format)?;
    Ok(Presigned { url, checksum, content_length: binding.len, signed_at: now_unix, valid_secs })
}
