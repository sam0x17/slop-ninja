//! Presigned R2 `CopyObject` requests: publication at scoring start, copied
//! inside R2 from the private admission bucket to the public bucket, so no
//! byte passes through any of our Workers and nothing is hashed on our side.
//!
//! The request is a `PUT` of the destination with no body. Its signature
//! binds seven headers the sender must send exactly:
//!
//! - `cache-control` and `content-type`: fixed values, written with
//!   `x-amz-metadata-directive: REPLACE`, so no miner-chosen metadata
//!   (a `Content-Encoding`, say) reaches the public copy;
//! - `host`;
//! - `x-amz-copy-source`: `<admission bucket>/<source key>`, a derived key;
//! - `x-amz-copy-source-if-match`: the source etag the publisher read, so
//!   an object replaced since then is not copied;
//! - `x-amz-metadata-directive: REPLACE`;
//! - `x-amz-storage-class: STANDARD`, so a source stored as Infrequent
//!   Access never makes the public copy carry retrieval fees.
//!
//! Both keys are derived from protocol values: the source from `(cohort,
//! commitment, kind, digest)` ([`crate::layout::ObjectId`]), the destination
//! from the digest alone ([`crate::layout::public_header_key`],
//! [`crate::layout::public_blob_key`]). For a file the source key carries the
//! same digest as the destination, so R2's ingest check fixes the bytes on
//! both sides. A header's source key carries no digest: the publisher reads
//! the source's stored checksum before it asks and compares the copy's etag
//! with the source's after (the publisher's run, `artifacts.rs`).
//!
//! Whether R2 carries the stored SHA-256 over to the copy is unqualified
//! (design G7); the publisher records it either way.

use core::fmt::Write;

use crate::buf::{Buf, decimal, write_hex};
use crate::layout::{self, MAX_KEY_LEN, ObjectId, ObjectKind};
use crate::policy::R2_MAX_VALID_SECS;
use crate::presign::{
    Credentials, MAX_ACCESS_KEY_ID_LEN, MAX_BUCKET_LEN, MAX_HOST_LEN, PresignError, REGION, SERVICE,
    valid_access_key_id, valid_bucket, valid_host, valid_key,
};
use crate::sigv4::{self, ALGORITHM, Canonical, CredentialScope, UNSIGNED_PAYLOAD};
use crate::time::amz_dates;

/// The signed headers, in canonical order.
pub const COPY_SIGNED_HEADERS: &str = "cache-control;content-type;host;x-amz-copy-source;x-amz-copy-source-if-match;x-amz-metadata-directive;x-amz-storage-class";
/// Every published artifact object's media type.
pub const CONTENT_TYPE: &str = "application/octet-stream";
/// Every published artifact object is immutable: its key names its digest.
pub const CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
/// The metadata directive: the copy takes the fixed values above, never the
/// source's metadata.
pub const METADATA_DIRECTIVE: &str = "REPLACE";
/// The copy's storage class.
pub const STORAGE_CLASS: &str = "STANDARD";

/// The length of `"<32 hex>"`.
pub const IF_MATCH_LEN: usize = 34;
/// `<bucket>/<key>`.
pub const MAX_COPY_SOURCE_LEN: usize = MAX_BUCKET_LEN + 1 + MAX_KEY_LEN;

/// The longest presigned copy URL: scheme, host, bucket, the longest
/// destination key and the seven query parameters at their format bounds.
pub const MAX_COPY_URL_LEN: usize = "https://".len()
    + MAX_HOST_LEN
    + 1
    + MAX_BUCKET_LEN
    + 1
    + MAX_KEY_LEN
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
    + "&X-Amz-SignedHeaders=".len()
    // Each `;` becomes `%3B`.
    + COPY_SIGNED_HEADERS.len()
    + 2 * 6;

/// What one copy moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyKind {
    /// The header bytes `H`, published under the header's digest.
    Header,
    /// One file, published under its digest.
    File,
}

/// One copy: the admission object of `(cohort, commitment, kind, sha256)`
/// to its public key, if its etag is still `etag`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopySpec {
    pub cohort: u64,
    pub commitment: [u8; 32],
    pub kind: CopyKind,
    /// The header's digest for [`CopyKind::Header`], the file's for
    /// [`CopyKind::File`].
    pub sha256: [u8; 32],
    /// The source's etag (a single-part object's: the MD5 R2 computed).
    pub etag: [u8; 16],
}

impl CopySpec {
    /// The admission object's key.
    pub fn source_key(&self) -> Buf<MAX_KEY_LEN> {
        let kind = match self.kind {
            CopyKind::Header => ObjectKind::Header,
            CopyKind::File => ObjectKind::File(self.sha256),
        };
        ObjectId { cohort: self.cohort, commitment: self.commitment, kind }.key()
    }

    /// The public key.
    pub fn destination_key(&self) -> Buf<MAX_KEY_LEN> {
        match self.kind {
            CopyKind::Header => layout::public_header_key(&self.sha256),
            CopyKind::File => layout::public_blob_key(&self.sha256),
        }
    }

    /// `x-amz-copy-source`: `<source bucket>/<source key>`.
    pub fn copy_source(&self, source_bucket: &str) -> Buf<MAX_COPY_SOURCE_LEN> {
        let mut out = Buf::new();
        let _ = write!(out, "{}/{}", source_bucket, self.source_key().as_str());
        out
    }

    /// `x-amz-copy-source-if-match`: the quoted etag.
    pub fn if_match(&self) -> Buf<IF_MATCH_LEN> {
        let mut out = Buf::new();
        let _ = out.push("\"");
        let _ = write_hex(&mut out, &self.etag);
        let _ = out.push("\"");
        out
    }
}

/// The two buckets of a copy.
#[derive(Clone, Copy, Debug)]
pub struct CopyBuckets<'a> {
    pub host: &'a str,
    pub source: &'a str,
    pub destination: &'a str,
}

/// A signed copy request and the header values that must accompany it.
#[derive(Clone, Debug)]
pub struct PresignedCopy {
    pub url: Buf<MAX_COPY_URL_LEN>,
    pub copy_source: Buf<MAX_COPY_SOURCE_LEN>,
    pub if_match: Buf<IF_MATCH_LEN>,
    /// Unix seconds of `X-Amz-Date`.
    pub signed_at: u64,
    pub valid_secs: u32,
}

impl PresignedCopy {
    /// The headers to send besides `host`, in canonical order.
    pub fn headers(&self) -> [(&'static str, &str); 6] {
        copy_headers(self.copy_source.as_str(), self.if_match.as_str())
    }
}

/// The headers of a copy besides `host`, in canonical order, from the values
/// the sender derives itself.
pub fn copy_headers<'a>(copy_source: &'a str, if_match: &'a str) -> [(&'static str, &'a str); 6] {
    [
        ("cache-control", CACHE_CONTROL),
        ("content-type", CONTENT_TYPE),
        ("x-amz-copy-source", copy_source),
        ("x-amz-copy-source-if-match", if_match),
        ("x-amz-metadata-directive", METADATA_DIRECTIVE),
        ("x-amz-storage-class", STORAGE_CLASS),
    ]
}

/// Signs the copy `spec` between `buckets`, valid for `valid_secs` from
/// `now_unix`.
pub fn presign_copy(
    cred: &Credentials<'_>,
    buckets: &CopyBuckets<'_>,
    spec: &CopySpec,
    now_unix: u64,
    valid_secs: u32,
) -> Result<PresignedCopy, PresignError> {
    if valid_secs == 0 || u64::from(valid_secs) > R2_MAX_VALID_SECS {
        return Err(PresignError::Validity);
    }
    let dst = spec.destination_key();
    if !valid_host(buckets.host)
        || !valid_bucket(buckets.source)
        || !valid_bucket(buckets.destination)
        || buckets.source == buckets.destination
        || !valid_key(dst.as_str())
        || !valid_access_key_id(cred.access_key_id)
    {
        return Err(PresignError::Format);
    }
    let copy_source = spec.copy_source(buckets.source);
    let if_match = spec.if_match();
    if copy_source.overflowed() || if_match.overflowed() || dst.overflowed() {
        return Err(PresignError::Format);
    }
    let (date, stamp) = amz_dates(now_unix).ok_or(PresignError::Clock)?;
    let scope = CredentialScope { date: date.as_str(), region: REGION, service: SERVICE };

    let mut credential: Buf<{ MAX_ACCESS_KEY_ID_LEN + 1 + sigv4::MAX_SCOPE_LEN }> = Buf::new();
    write!(credential, "{}/", cred.access_key_id).map_err(|_| PresignError::Format)?;
    scope.write(&mut credential).map_err(|_| PresignError::Format)?;
    let expires = decimal(u64::from(valid_secs));

    let mut path: Buf<{ 2 + MAX_BUCKET_LEN + MAX_KEY_LEN }> = Buf::new();
    write!(path, "/{}/{}", buckets.destination, dst.as_str()).map_err(|_| PresignError::Format)?;

    let query = [
        ("X-Amz-Algorithm", ALGORITHM),
        ("X-Amz-Content-Sha256", UNSIGNED_PAYLOAD),
        ("X-Amz-Credential", credential.as_str()),
        ("X-Amz-Date", stamp.as_str()),
        ("X-Amz-Expires", expires.as_str()),
        ("X-Amz-SignedHeaders", COPY_SIGNED_HEADERS),
    ];
    let [cc, ct, src, ifm, md, sc] = copy_headers(copy_source.as_str(), if_match.as_str());
    let headers = [cc, ct, ("host", buckets.host), src, ifm, md, sc];
    let canonical =
        Canonical { method: "PUT", path: path.as_str(), query: &query, headers: &headers, payload: UNSIGNED_PAYLOAD };
    let digest = sigv4::canonical_digest(&canonical).map_err(|_| PresignError::Format)?;
    let signature =
        sigv4::signature(cred.secret_access_key, stamp.as_str(), &scope, &digest).map_err(|_| PresignError::Format)?;

    let mut url = Buf::new();
    let write = (|| {
        write!(url, "https://{}", buckets.host)?;
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
    if url.overflowed() {
        return Err(PresignError::Format);
    }
    Ok(PresignedCopy { url, copy_source, if_match, signed_at: now_unix, valid_secs })
}

/// Parses a single-part etag (32 lowercase or uppercase hex digits, quoted
/// or not) into its 16 bytes; `None` for anything else, a multipart etag
/// (`<hex>-<parts>`) included.
pub fn parse_etag(text: &str) -> Option<[u8; 16]> {
    let t = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')).unwrap_or(text);
    let b = t.as_bytes();
    if b.len() != 32 {
        return None;
    }
    let nib = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (nib(b[2 * i])? << 4) | nib(b[2 * i + 1])?;
    }
    Some(out)
}
