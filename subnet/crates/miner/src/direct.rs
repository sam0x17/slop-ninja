//! Direct PUTs to R2 under presigned URLs (`docs/testnet/admission-api.md`
//! section 6).
//!
//! The coordinator mints each URL; the client never builds one, and never
//! trusts one blindly. Before it sends a byte it checks the URL
//! ([`check_upload`], `slop_ninja_r2_presign::check`): `https`, on the trust
//! file's pinned upload host or else an R2 S3 host, addressing exactly the
//! key the client derives itself from (cohort, commitment, digest), with
//! exactly the seven SigV4 parameters and the four signed headers. It then
//! sends exactly three headers computed from its own committed values
//! (`content-length`, `if-none-match: *` and `x-amz-checksum-sha256`; the
//! HTTP stack adds `host`), never another `x-amz-*` header, never
//! `content-encoding`, never chunked transfer encoding, and the object's
//! exact byte range streamed from disk through one buffer. R2 hashes the body
//! as it arrives and refuses a mismatch with `BadDigest`, storing nothing.
//!
//! [`put_object`] makes one attempt and classifies R2's answer
//! ([`PutError`]); the retry policy is the upload loop's
//! ([`crate::upload`]).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use slop_ninja_admission_wire::json::Upload;
use slop_ninja_r2_presign::check::{check_put_url, put_headers};
use slop_ninja_r2_presign::layout::{ObjectId, ObjectKind};
use url::Url;

/// One streaming read: it bounds the client's memory, not the object's size.
pub const STREAM_CHUNK: usize = 1 << 20;

/// The longest R2 error body read for its code: an S3 XML error document
/// (`<Error><Code>…</Code><Message>…</Message>…</Error>`) is far shorter.
pub const ERROR_BODY_LIMIT: usize = 8 * 1024;

/// How presigned URLs are checked, and where their PUTs go.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PutPolicy {
    /// The trust file's pinned R2 S3 host (`upload_host`): every URL must
    /// name exactly it. Without a pin, any `*.r2.cloudflarestorage.com` host.
    pub pinned_host: Option<String>,
    /// The trust file's host pin by digest (`upload_host_sha256`): the
    /// SHA-256 of the URL's host name must be it.
    pub pinned_host_sha256: Option<[u8; 32]>,
    /// The trust file's bucket pin (`upload_bucket`).
    pub bucket: Option<String>,
    /// Testing only: send each PUT to this loopback `http` endpoint instead
    /// of the URL's host, keeping the URL's `host` header, path and query (so
    /// a local stub can check the signature). The URL itself is still checked
    /// in full. Refused unless the host is loopback.
    pub endpoint_override: Option<Url>,
}

impl PutPolicy {
    /// A policy with a loopback endpoint override (tests only).
    pub fn with_endpoint_override(mut self, endpoint: &str) -> anyhow::Result<Self> {
        let url = Url::parse(endpoint)?;
        let loopback = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(name)) => name == "localhost",
            None => false,
        };
        anyhow::ensure!(
            loopback && url.scheme() == "http" && url.path() == "/" && url.query().is_none(),
            "the R2 endpoint override must be a plain http loopback origin (testing only)"
        );
        self.endpoint_override = Some(url);
        Ok(self)
    }
}

/// One object to PUT: its identity (which fixes its key), its committed
/// SHA-256 and length, and where its bytes are in the local object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutTarget {
    pub id: ObjectId,
    pub sha256: [u8; 32],
    pub len: u64,
    /// The local artifact object and the byte offset of this object in it.
    pub path: PathBuf,
    pub offset: u64,
    /// For reports: `header` or the file's name.
    pub name: String,
}

impl PutTarget {
    pub fn is_header(&self) -> bool {
        self.id.kind == ObjectKind::Header
    }
}

/// Checks a minted upload against what the client itself committed to:
/// the object's kind, digest and length, the URL
/// (`slop_ninja_r2_presign::check::check_put_url` against the derived key
/// and the pinned host), and the three headers it names.
pub fn check_upload(upload: &Upload, target: &PutTarget, policy: &PutPolicy) -> Result<(), String> {
    let kind_ok = (upload.kind == "header") == target.is_header();
    if !kind_ok || upload.sha256 != hex::encode(target.sha256) || upload.len != target.len {
        return Err(format!(
            "the coordinator minted a URL for {} {} ({} bytes), not for {} ({} bytes)",
            upload.kind, upload.sha256, upload.len, target.name, target.len
        ));
    }
    let key = target.id.key();
    let checked = check_put_url(
        &upload.url,
        policy.pinned_host.as_deref(),
        policy.bucket.as_deref(),
        key.as_str(),
    )
    .map_err(|error| format!("refusing the presigned URL for {}: {error:?}", target.name))?;
    if let Some(pin) = &policy.pinned_host_sha256
        && !pinned_host_digest_ok(checked.host, pin)
    {
        return Err(format!(
            "refusing the presigned URL for {}: its host is not the trust file's pinned upload host",
            target.name
        ));
    }
    let (length, if_none_match, checksum) = put_headers(target.len, &target.sha256);
    if upload.content_length != length.as_str()
        || upload.if_none_match != if_none_match
        || upload.checksum_sha256 != checksum.as_str()
    {
        return Err(format!(
            "the presigned URL for {} names other headers than the committed length and digest",
            target.name
        ));
    }
    Ok(())
}

/// Whether `host` is the host the trust file pins by digest. The one hash
/// the client computes besides the envelope digest: over a host name (a
/// few dozen bytes of the URL), never a model byte.
pub(crate) fn pinned_host_digest_ok(host: &str, pin: &[u8; 32]) -> bool {
    slop_ninja_protocol::object_sha256(host.as_bytes()).0 == *pin
}

/// A PUT that R2 accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PutOk {
    /// R2 stored the object (it verified the checksum).
    Stored,
    /// 412: the object already exists under its key (for example after a
    /// lost success answer). The key admits only the committed bytes;
    /// `complete` confirms it by `head()`.
    AlreadyPresent,
}

/// Why a PUT did not store the object.
#[derive(Debug)]
pub enum PutError {
    /// 403 `ExpiredRequest`: the URL's life ended before the PUT started
    /// (R2 checks expiry at the request's start, qualification G6). Ask for
    /// a fresh one (no backoff); `sent` bytes of the body went out before
    /// the answer.
    Expired { sent: u64 },
    /// A transport error, a 5xx, 429 or another answer a fresh URL and a
    /// later attempt can fix.
    Retry(String),
    /// 400 `BadDigest`: the local bytes no longer match the header's digest.
    /// Re-pack from the same sources (the commitment binds the digests).
    BadDigest(String),
    /// 403 `SignatureDoesNotMatch`: the client sent other headers than the
    /// URL signs, a client defect.
    Signature(String),
    /// Something no retry can fix (a redirect, 501 `NotImplemented`, an
    /// unreadable local file).
    Fatal(String),
}

impl std::fmt::Display for PutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Expired { sent, .. } => {
                write!(f, "the presigned URL expired ({sent} bytes sent)")
            }
            Self::Retry(why) | Self::Fatal(why) => f.write_str(why),
            Self::BadDigest(why) => write!(
                f,
                "R2 refused the bytes: they do not match the committed SHA-256 ({why}). The local \
                 object changed since pack; re-pack it from the same sources (the commitment binds \
                 the digests), or commit again for the new object"
            ),
            Self::Signature(why) => write!(
                f,
                "R2 refused the request signature ({why}): a client defect; report it with the \
                 client version"
            ),
        }
    }
}

/// The S3 error code in an XML error body, if any.
pub fn s3_error_code(body: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(body).ok()?;
    let start = text.find("<Code>")? + "<Code>".len();
    let end = start + text[start..].find("</Code>")?;
    Some(&text[start..end])
}

/// Classifies an R2 answer to a presigned PUT.
///
/// 501 `NotImplemented` is a refusal like 403, never retried: R2 answers it,
/// before checking the signature, for a URL carrying a parameter it does not
/// take on a presigned PUT (`partNumber` with `uploadId`, `versionId`;
/// qualification G11), and stores nothing. The URL check refuses such a URL
/// before any byte is sent, so a 501 means R2 or the URL changed.
pub fn classify(status: u16, body: &[u8], sent: u64) -> Result<PutOk, PutError> {
    let code = s3_error_code(body).unwrap_or("");
    match status {
        200..=299 => Ok(PutOk::Stored),
        412 => Ok(PutOk::AlreadyPresent),
        300..=399 => Err(PutError::Fatal(format!(
            "R2 answered {status}, a redirect; redirects are never followed"
        ))),
        400 if code == "BadDigest" => Err(PutError::BadDigest(code.into())),
        403 if code == "SignatureDoesNotMatch" => Err(PutError::Signature(code.into())),
        403 if code == "ExpiredRequest" || code == "RequestExpired" => {
            Err(PutError::Expired { sent })
        }
        501 => Err(PutError::Fatal(format!(
            "R2 refused the PUT as not implemented (501{}): the URL asks for something a \
             presigned PUT does not take; nothing was stored",
            if code.is_empty() {
                String::new()
            } else {
                format!(" {code}")
            }
        ))),
        _ => Err(PutError::Retry(format!(
            "R2 answered {status}{}",
            if code.is_empty() {
                String::new()
            } else {
                format!(" {code}")
            }
        ))),
    }
}

/// Counts body bytes as they stream: per object and in total.
pub trait Progress: Send + Sync {
    /// A run will send `objects` objects of `bytes` bytes in all (at most:
    /// a retry resends one object from its first byte).
    fn plan(&self, _objects: usize, _bytes: u64) {}
    /// An attempt at one object starts (from byte 0).
    fn start(&self, _name: &str, _len: u64, _attempt: u32) {}
    /// `bytes` more of the current object's body went out.
    fn advance(&self, _bytes: u64) {}
    /// The current object's attempt ended: `ok` when R2 holds it.
    fn finish(&self, _name: &str, _ok: bool, _why: &str) {}
}

/// No progress output.
pub struct Silent;
impl Progress for Silent {}

/// The object's bytes, streamed from disk through one buffer.
fn body_stream(
    path: PathBuf,
    offset: u64,
    len: u64,
    progress: Arc<dyn Progress>,
    sent: Arc<std::sync::atomic::AtomicU64>,
) -> impl futures::Stream<Item = std::io::Result<Bytes>> + Send + 'static {
    futures::stream::try_unfold((None::<File>, len), move |(file, remaining)| {
        let path = path.clone();
        let progress = progress.clone();
        let sent = sent.clone();
        async move {
            if remaining == 0 {
                return Ok(None);
            }
            let n = remaining.min(STREAM_CHUNK as u64) as usize;
            let (file, buffer) =
                tokio::task::spawn_blocking(move || -> std::io::Result<(File, Vec<u8>)> {
                    let mut file = match file {
                        Some(file) => file,
                        None => {
                            let mut file = File::open(&path)?;
                            file.seek(SeekFrom::Start(offset))?;
                            file
                        }
                    };
                    let mut buffer = vec![0u8; n];
                    file.read_exact(&mut buffer)?;
                    Ok((file, buffer))
                })
                .await
                .map_err(std::io::Error::other)??;
            progress.advance(n as u64);
            sent.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
            Ok(Some((
                Bytes::from(buffer),
                (Some(file), remaining - n as u64),
            )))
        }
    })
}

/// One presigned PUT of `target` to `upload` (checked first). `timeout`
/// bounds the whole request.
pub async fn put_object(
    http: &reqwest::Client,
    policy: &PutPolicy,
    upload: &Upload,
    target: &PutTarget,
    timeout: Duration,
    progress: Arc<dyn Progress>,
) -> Result<PutOk, PutError> {
    check_upload(upload, target, policy).map_err(PutError::Fatal)?;
    let body = Body { path: &target.path, offset: target.offset, len: target.len, sha256: &target.sha256, name: &target.name };
    send_put(http, policy, &upload.url, &body, timeout, progress).await
}

/// What one PUT sends: the byte range of a local file, its committed
/// length and SHA-256, and its name for reports.
#[derive(Clone, Copy, Debug)]
pub struct Body<'a> {
    pub path: &'a Path,
    pub offset: u64,
    pub len: u64,
    pub sha256: &'a [u8; 32],
    pub name: &'a str,
}

/// One PUT of `target` to the already checked presigned `url`: exactly
/// the three headers of [`put_headers`] and the object's byte range
/// streamed from disk. Callers check the URL against the key they derive
/// first ([`check_upload`]; reference staging's own check in
/// [`crate::reference`]).
pub async fn send_put(
    http: &reqwest::Client,
    policy: &PutPolicy,
    url: &str,
    target: &Body<'_>,
    timeout: Duration,
    progress: Arc<dyn Progress>,
) -> Result<PutOk, PutError> {
    let on_disk = std::fs::metadata(target.path)
        .map_err(|e| PutError::Fatal(format!("{}: {e}", target.path.display())))?
        .len();
    if target
        .offset
        .checked_add(target.len)
        .is_none_or(|end| end > on_disk)
    {
        return Err(PutError::BadDigest(format!(
            "{} is shorter than its header describes",
            target.path.display()
        )));
    }
    let url = Url::parse(url).map_err(|e| PutError::Fatal(format!("presigned URL: {e}")))?;
    let host = url.host_str().unwrap_or_default().to_owned();
    // For error messages: the object and the host, never the presigned
    // query (a bearer capability until it expires) or the path.
    let shown = format!("{} on {host}", target.name);
    let send_to = match &policy.endpoint_override {
        None => url.clone(),
        Some(endpoint) => {
            let mut to = endpoint.clone();
            to.set_path(url.path());
            to.set_query(url.query());
            to
        }
    };
    let (length, if_none_match, checksum) = put_headers(target.len, target.sha256);
    let sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let body = reqwest::Body::wrap_stream(body_stream(
        target.path.to_path_buf(),
        target.offset,
        target.len,
        progress,
        sent.clone(),
    ));
    let mut request = http
        .put(send_to)
        .timeout(timeout)
        .header(reqwest::header::CONTENT_LENGTH, length.as_str())
        .header(reqwest::header::IF_NONE_MATCH, if_none_match)
        .header("x-amz-checksum-sha256", checksum.as_str())
        .body(body);
    if policy.endpoint_override.is_some() {
        request = request.header(reqwest::header::HOST, host);
    }
    let mut response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            // Review round 2: reqwest's error names the full URL, which
            // carries the signature; it is stripped before anything prints.
            let kind = if error.is_timeout() {
                "timed out"
            } else if error.is_connect() {
                "could not connect"
            } else {
                "transport error"
            };
            return Err(PutError::Retry(format!(
                "the PUT of {shown} did not complete ({kind}): {}",
                error.without_url()
            )));
        }
    };
    let status = response.status().as_u16();
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        let room = ERROR_BODY_LIMIT.saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if room <= chunk.len() {
            break;
        }
    }
    classify(status, &body, sent.load(std::sync::atomic::Ordering::Relaxed))
}

/// The local file holding the objects, checked for its length.
pub fn object_path_ok(path: &Path, object_len: u64) -> Result<(), String> {
    let on_disk = std::fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    if on_disk != object_len {
        return Err(format!(
            "{} is {on_disk} bytes; its header describes {object_len}: the object changed since pack",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_r2_presign::presign::{Binding, Credentials, Target, presign_put};

    fn target(kind: ObjectKind, sha: [u8; 32], len: u64) -> PutTarget {
        PutTarget {
            id: ObjectId {
                cohort: 7,
                commitment: [3; 32],
                kind,
            },
            sha256: sha,
            len,
            path: PathBuf::from("o"),
            offset: 4,
            name: "x".into(),
        }
    }

    fn minted(t: &PutTarget, host: &str) -> Upload {
        let key = t.id.key();
        let p = presign_put(
            &Credentials {
                access_key_id: "AKIDEXAMPLE",
                secret_access_key: b"secret",
            },
            &Target {
                host,
                bucket: "bucket-a",
                key: key.as_str(),
            },
            &Binding {
                len: t.len,
                sha256: t.sha256,
            },
            1_793_846_400,
            909,
        )
        .unwrap();
        Upload {
            kind: if t.is_header() {
                "header".into()
            } else {
                "file".into()
            },
            sha256: hex::encode(t.sha256),
            len: t.len,
            url: p.url.as_str().into(),
            content_length: t.len.to_string(),
            if_none_match: "*".into(),
            checksum_sha256: p.checksum.as_str().into(),
            signed_at: p.signed_at,
            valid_secs: p.valid_secs,
        }
    }

    #[test]
    fn a_minted_url_passes_and_any_change_is_refused() {
        let policy = PutPolicy::default();
        let header = target(ObjectKind::Header, [9; 32], 120);
        let host = "acct.r2.cloudflarestorage.com";
        let up = minted(&header, host);
        check_upload(&up, &header, &policy).unwrap();
        // Another key (another commitment), another digest, another length,
        // another kind, another header value: each refused.
        let other = target(ObjectKind::Header, [9; 32], 120);
        let other = PutTarget {
            id: ObjectId {
                commitment: [4; 32],
                ..other.id
            },
            ..other
        };
        assert!(check_upload(&up, &other, &policy).is_err());
        assert!(check_upload(&up, &target(ObjectKind::Header, [8; 32], 120), &policy).is_err());
        assert!(check_upload(&up, &target(ObjectKind::Header, [9; 32], 121), &policy).is_err());
        assert!(
            check_upload(
                &up,
                &target(ObjectKind::File([9; 32]), [9; 32], 120),
                &policy
            )
            .is_err()
        );
        let mut bad = up.clone();
        bad.checksum_sha256 = "AAAA".into();
        assert!(check_upload(&bad, &header, &policy).is_err());
        let mut bad = up.clone();
        bad.url = bad.url.replacen("https://", "http://", 1);
        assert!(check_upload(&bad, &header, &policy).is_err());
        let mut bad = up.clone();
        bad.url.push_str("&x-id=PutObject");
        assert!(check_upload(&bad, &header, &policy).is_err());
        // A host outside R2's S3 domain, and a host other than the pinned one.
        assert!(check_upload(&minted(&header, "evil.example.com"), &header, &policy).is_err());
        let pinned = PutPolicy {
            pinned_host: Some("other.r2.cloudflarestorage.com".into()),
            ..Default::default()
        };
        assert!(check_upload(&up, &header, &pinned).is_err());
        let pinned = PutPolicy {
            pinned_host: Some(host.into()),
            ..Default::default()
        };
        check_upload(&up, &header, &pinned).unwrap();
        // The release trust file's pins (decision D1, skeptic K12): the
        // host by its name's digest, and the bucket. A URL on any other
        // host, or for another bucket, is refused.
        let digest = |h: &str| slop_ninja_protocol::object_sha256(h.as_bytes()).0;
        let by_digest = PutPolicy { pinned_host_sha256: Some(digest(host)), ..Default::default() };
        check_upload(&up, &header, &by_digest).unwrap();
        let other = PutPolicy { pinned_host_sha256: Some(digest("other.r2.cloudflarestorage.com")), ..Default::default() };
        assert!(check_upload(&up, &header, &other).unwrap_err().contains("pinned upload host"));
        assert!(check_upload(&minted(&header, "other.r2.cloudflarestorage.com"), &header, &by_digest).is_err());
        let bucket = PutPolicy { bucket: Some("bucket-a".into()), ..Default::default() };
        check_upload(&up, &header, &bucket).unwrap();
        let bucket = PutPolicy { bucket: Some("bucket-b".into()), ..Default::default() };
        assert!(check_upload(&up, &header, &bucket).is_err());
        // A file URL addresses f/<its digest>.
        let file = target(ObjectKind::File([5; 32]), [5; 32], 77);
        check_upload(&minted(&file, host), &file, &policy).unwrap();
    }

    #[test]
    fn r2_answers_are_classified() {
        let xml = |code: &str| {
            format!("<?xml version=\"1.0\"?><Error><Code>{code}</Code><Message>m</Message></Error>")
        };
        assert_eq!(classify(200, b"", 0).unwrap(), PutOk::Stored);
        assert_eq!(
            classify(412, xml("PreconditionFailed").as_bytes(), 0).unwrap(),
            PutOk::AlreadyPresent
        );
        assert!(matches!(
            classify(400, xml("BadDigest").as_bytes(), 0),
            Err(PutError::BadDigest(_))
        ));
        assert!(matches!(
            classify(403, xml("SignatureDoesNotMatch").as_bytes(), 0),
            Err(PutError::Signature(_))
        ));
        assert!(matches!(
            classify(403, xml("ExpiredRequest").as_bytes(), 5),
            Err(PutError::Expired { sent: 5 })
        ));
        for status in [429, 500, 503] {
            assert!(matches!(
                classify(status, b"", 0),
                Err(PutError::Retry(_))
            ));
        }
        assert!(matches!(classify(307, b"", 0), Err(PutError::Fatal(_))));
        // G11: 501 for a multipart or version parameter is a refusal.
        assert!(matches!(
            classify(501, xml("NotImplemented").as_bytes(), 0),
            Err(PutError::Fatal(_))
        ));
        assert_eq!(s3_error_code(b"no xml"), None);
    }

    #[test]
    fn the_endpoint_override_is_loopback_only() {
        let p = PutPolicy::default();
        assert!(
            p.clone()
                .with_endpoint_override("http://127.0.0.1:9000/")
                .is_ok()
        );
        assert!(
            p.clone()
                .with_endpoint_override("http://localhost:9000/")
                .is_ok()
        );
        assert!(
            p.clone()
                .with_endpoint_override("http://10.0.0.1:9000/")
                .is_err()
        );
        assert!(
            p.clone()
                .with_endpoint_override("https://127.0.0.1:9000/")
                .is_err()
        );
        assert!(p.with_endpoint_override("http://127.0.0.1:9000/x").is_err());
    }
}
