//! The signer's one decision, as a pure function natively tested: a mint
//! request in, a reply out, with every bound derived from the protocol and
//! the compiled admission specs.
//!
//! - **Entries.** At most [`MAX_ENTRIES`]: one header and the largest file
//!   table of any admission spec, so one `urls` call is one request.
//! - **Header** entries: length `1..=MAX_ARTIFACT_HEADER_LEN`.
//! - **File** entries: length `1..=` the largest object of any compiled
//!   admission spec (a file never exceeds its object), and never above R2's
//!   single-PUT limit.
//! - **Canary** entries: the fixed canary body's length and digest only, on
//!   the `health/` prefix, so the canary cannot mint a miner key.
//! - **Reference** entries (`slop_ninja_reference_staging_v1`): an operator-staged
//!   reference file, length `1..=` the same largest admission object, on
//!   `reference/v2/files/<sha256>`; `cohort` and `commitment` must be zero,
//!   so a reference entry cannot address a miner key either.
//! - Validity `1..=604,800` s (R2's range). The eval side computes it under
//!   the cohort's URL policy; the signer refuses only what R2 would.
//!
//! Every key is derived here from `(cohort, commitment, kind, digest)`;
//! the signer never takes a key, a method, a host or a bucket from a
//! request. It signs `PUT` only.
//!
//! [`copy`] answers the publisher's copy request (publication at scoring
//! start, `ARTIFACT_COPY = "server"`): one presigned `CopyObject` from the
//! admission bucket to the public bucket, with keys derived the same way,
//! signed with a separate copy credential and reached through a separate
//! route and token, so the upload credential never signs a copy and the
//! copy credential never signs an upload.

use sha2::{Digest as _, Sha256};
use slop_ninja_artifact_check::admission::largest_admission_object;
use slop_ninja_artifact_check::spec;
use slop_ninja_protocol::MAX_ARTIFACT_HEADER_LEN;
use slop_ninja_r2_presign::layout::{self, CANARY_LEN, ObjectId, ObjectKind};
use slop_ninja_r2_presign::policy::{R2_MAX_VALID_SECS, R2_SINGLE_PUT_MAX};
use slop_ninja_r2_presign::copy::{CopyBuckets, presign_copy};
use slop_ninja_r2_presign::presign::{Binding, Credentials, PresignError, Target, presign_put};
use slop_ninja_r2_presign::wire::{self, Entry, Op, ReplyEntry, Status};

/// One header and the spec's whole file table.
pub const MAX_ENTRIES: usize = 1 + spec::ROLES.len();
/// The exact length bound of a request.
pub const REQUEST_BYTES: usize = wire::request_len(MAX_ENTRIES);
/// The largest reply.
pub const REPLY_BYTES: usize = wire::reply_max(MAX_ENTRIES);

/// The largest file length the signer binds: the largest object of any
/// compiled admission spec, never above R2's single PUT.
pub fn max_file_len() -> u64 {
    largest_admission_object().min(R2_SINGLE_PUT_MAX)
}

/// The signer's configuration: where objects go and the credential.
#[derive(Clone, Copy, Debug)]
pub struct Config<'a> {
    pub host: &'a str,
    pub bucket: &'a str,
    pub cred: Credentials<'a>,
}

/// A request that is refused whole (400): wrong length, version or count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Malformed;

fn entry_status(e: &Entry, max_file: u64) -> Result<(), Status> {
    let len_ok = match e.op {
        Op::Header => (1..=u64::from(MAX_ARTIFACT_HEADER_LEN)).contains(&e.len),
        Op::File => (1..=max_file).contains(&e.len),
        Op::Reference => {
            if e.cohort != 0 || e.commitment != [0; 32] {
                return Err(Status::Malformed);
            }
            (1..=max_file).contains(&e.len)
        }
        Op::Canary => {
            let digest: [u8; 32] = Sha256::digest(layout::canary_body(&e.commitment)).into();
            if e.len != CANARY_LEN as u64 || digest != e.sha256 {
                return Err(Status::Canary);
            }
            true
        }
    };
    if !len_ok {
        return Err(Status::Length);
    }
    if e.valid_secs == 0 || u64::from(e.valid_secs) > R2_MAX_VALID_SECS {
        return Err(Status::Validity);
    }
    Ok(())
}

fn status_of(e: PresignError) -> Status {
    match e {
        PresignError::Length => Status::Length,
        PresignError::Validity => Status::Validity,
        PresignError::Format => Status::Format,
        PresignError::Clock => Status::Clock,
    }
}

const REFUSED: ReplyEntry<'static> =
    ReplyEntry { status: Status::Malformed, signed_at: 0, valid_secs: 0, content_length: 0, checksum: "", url: "" };

/// Answers one request into `out` (at least [`REPLY_BYTES`]), returning the
/// reply's length. Entries are answered in order; one refused entry does
/// not refuse the others.
pub fn mint(req: &[u8], cfg: &Config<'_>, now_unix: u64, out: &mut [u8]) -> Result<usize, Malformed> {
    if req.len() > REQUEST_BYTES || out.len() < REPLY_BYTES {
        return Err(Malformed);
    }
    let entries = wire::decode_request(req, MAX_ENTRIES).ok_or(Malformed)?;
    let n = req.get(1).copied().ok_or(Malformed)?;
    let max_file = max_file_len();
    let mut at = wire::encode_reply_head(out, n).ok_or(Malformed)?;
    for entry in entries {
        let Some(e) = entry else {
            at = wire::encode_reply_entry(out, at, &REFUSED).ok_or(Malformed)?;
            continue;
        };
        if let Err(status) = entry_status(&e, max_file) {
            at = wire::encode_reply_entry(out, at, &ReplyEntry { status, ..REFUSED }).ok_or(Malformed)?;
            continue;
        }
        let key = match e.op {
            Op::Header => ObjectId { cohort: e.cohort, commitment: e.commitment, kind: ObjectKind::Header }.key(),
            Op::File => ObjectId { cohort: e.cohort, commitment: e.commitment, kind: ObjectKind::File(e.sha256) }.key(),
            Op::Canary => layout::health_key(e.cohort, &e.commitment),
            Op::Reference => layout::reference_key(&e.sha256),
        };
        let target = Target { host: cfg.host, bucket: cfg.bucket, key: key.as_str() };
        let binding = Binding { len: e.len, sha256: e.sha256 };
        at = match presign_put(&cfg.cred, &target, &binding, now_unix, e.valid_secs) {
            Ok(p) => wire::encode_reply_entry(
                out,
                at,
                &ReplyEntry {
                    status: Status::Ok,
                    signed_at: p.signed_at,
                    valid_secs: p.valid_secs,
                    content_length: p.content_length,
                    checksum: p.checksum.as_str(),
                    url: p.url.as_str(),
                },
            ),
            Err(err) => wire::encode_reply_entry(out, at, &ReplyEntry { status: status_of(err), ..REFUSED }),
        }
        .ok_or(Malformed)?;
    }
    Ok(at)
}

/// The exact length of a copy request.
pub const COPY_REQUEST_BYTES: usize = wire::COPY_REQUEST_LEN;
/// The largest copy reply.
pub const COPY_REPLY_BYTES: usize = wire::COPY_REPLY_MAX;

/// The copy configuration: the host, the two buckets and the copy
/// credential (Object Read & Write on exactly those two buckets).
#[derive(Clone, Copy, Debug)]
pub struct CopyConfig<'a> {
    pub host: &'a str,
    pub source_bucket: &'a str,
    pub destination_bucket: &'a str,
    pub cred: Credentials<'a>,
}

/// Answers one copy request into `out` (at least [`COPY_REPLY_BYTES`]),
/// returning the reply's length. The source and destination keys are
/// derived from the request's `(cohort, commitment, kind, digest)`; the
/// request names neither a key nor a bucket.
pub fn copy(req: &[u8], cfg: &CopyConfig<'_>, now_unix: u64, out: &mut [u8]) -> Result<usize, Malformed> {
    if req.len() != COPY_REQUEST_BYTES || out.len() < COPY_REPLY_BYTES {
        return Err(Malformed);
    }
    let (spec, valid_secs) = wire::decode_copy_request(req).ok_or(Malformed)?;
    let buckets = CopyBuckets { host: cfg.host, source: cfg.source_bucket, destination: cfg.destination_bucket };
    let reply = match presign_copy(&cfg.cred, &buckets, &spec, now_unix, valid_secs) {
        Ok(p) => wire::encode_copy_reply(
            out,
            &wire::CopyReply { status: Status::Ok, signed_at: p.signed_at, valid_secs: p.valid_secs, url: p.url.as_str() },
        ),
        Err(e) => wire::encode_copy_reply(out, &wire::CopyReply { status: status_of(e), signed_at: 0, valid_secs: 0, url: "" }),
    };
    reply.ok_or(Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_r2_presign::check::check_put_url;

    const CFG: Config<'static> = Config {
        host: "0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com",
        bucket: "slop-ninja-admission-testnet",
        cred: Credentials { access_key_id: "CFEXAMPLEKEY12345", secret_access_key: b"not-a-real-secret" },
    };
    const NOW: u64 = 1_764_612_312;

    fn run(entries: &[Entry]) -> Vec<(Status, String)> {
        let mut req = vec![0u8; wire::request_len(entries.len())];
        wire::encode_request(entries, &mut req).unwrap();
        let mut out = vec![0u8; REPLY_BYTES];
        let n = mint(&req, &CFG, NOW, &mut out).unwrap();
        wire::decode_reply(&out[..n], entries.len()).unwrap().map(|e| e.unwrap()).map(|e| (e.status, e.url.to_owned())).collect()
    }

    fn file(len: u64) -> Entry {
        Entry { op: Op::File, cohort: 9, commitment: [7; 32], sha256: [8; 32], len, valid_secs: 8_537 }
    }

    #[test]
    fn mints_the_derived_keys_only() {
        let header = Entry { op: Op::Header, len: 900, sha256: [6; 32], ..file(1) };
        let got = run(&[header, file(681_508_964)]);
        assert!(got.iter().all(|(s, _)| *s == Status::Ok));
        let hk = ObjectId { cohort: 9, commitment: [7; 32], kind: ObjectKind::Header }.key();
        let fk = ObjectId { cohort: 9, commitment: [7; 32], kind: ObjectKind::File([8; 32]) }.key();
        assert!(check_put_url(&got[0].1, Some(CFG.host), Some(CFG.bucket), hk.as_str()).is_ok());
        assert!(check_put_url(&got[1].1, Some(CFG.host), Some(CFG.bucket), fk.as_str()).is_ok());
    }

    #[test]
    fn bounds_come_from_the_protocol_and_the_specs() {
        assert_eq!(MAX_ENTRIES, 7);
        assert!(max_file_len() >= 954_576_228 - 4 - u64::from(MAX_ARTIFACT_HEADER_LEN));
        let header_too_long = Entry { op: Op::Header, len: u64::from(MAX_ARTIFACT_HEADER_LEN) + 1, ..file(1) };
        let header_max = Entry { op: Op::Header, len: u64::from(MAX_ARTIFACT_HEADER_LEN), ..file(1) };
        let got = run(&[file(0), file(max_file_len() + 1), file(max_file_len()), header_too_long, header_max]);
        let s: Vec<Status> = got.iter().map(|g| g.0).collect();
        assert_eq!(s, [Status::Length, Status::Length, Status::Ok, Status::Length, Status::Ok]);
        let got = run(&[Entry { valid_secs: 0, ..file(5) }, Entry { valid_secs: 604_801, ..file(5) }]);
        assert!(got.iter().all(|g| g.0 == Status::Validity));
    }

    #[test]
    fn the_canary_mints_only_its_own_body_on_the_health_prefix() {
        let nonce = [3u8; 32];
        let sha: [u8; 32] = Sha256::digest(layout::canary_body(&nonce)).into();
        let ok = Entry { op: Op::Canary, cohort: 9, commitment: nonce, sha256: sha, len: CANARY_LEN as u64, valid_secs: 960 };
        let got = run(&[ok, Entry { sha256: [0; 32], ..ok }, Entry { len: 65, ..ok }]);
        assert_eq!(got[0].0, Status::Ok);
        assert!(check_put_url(&got[0].1, Some(CFG.host), None, layout::health_key(9, &nonce).as_str()).is_ok());
        assert_eq!((got[1].0, got[2].0), (Status::Canary, Status::Canary));
    }

    #[test]
    fn references_mint_only_their_digest_key_with_zero_cohort_and_commitment() {
        let ok = Entry { op: Op::Reference, cohort: 0, commitment: [0; 32], sha256: [8; 32], len: 681_508_964, valid_secs: 8_537 };
        let got = run(&[
            ok,
            Entry { cohort: 9, ..ok },
            Entry { commitment: [7; 32], ..ok },
            Entry { len: 0, ..ok },
            Entry { len: max_file_len() + 1, ..ok },
            Entry { len: max_file_len(), ..ok },
        ]);
        let s: Vec<Status> = got.iter().map(|g| g.0).collect();
        assert_eq!(s, [Status::Ok, Status::Malformed, Status::Malformed, Status::Length, Status::Length, Status::Ok]);
        let key = layout::reference_key(&[8; 32]);
        assert!(check_put_url(&got[0].1, Some(CFG.host), Some(CFG.bucket), key.as_str()).is_ok());
        // Never a miner key: the derived admission keys do not match.
        let fk = ObjectId { cohort: 0, commitment: [0; 32], kind: ObjectKind::File([8; 32]) }.key();
        assert!(check_put_url(&got[0].1, Some(CFG.host), Some(CFG.bucket), fk.as_str()).is_err());
        assert!(got[0].1.contains("x-amz-checksum-sha256"));
    }

    #[test]
    fn malformed_requests_are_refused_whole() {
        let mut out = vec![0u8; REPLY_BYTES];
        assert_eq!(mint(&[], &CFG, NOW, &mut out), Err(Malformed));
        let entries = vec![file(5); MAX_ENTRIES + 1];
        let mut req = vec![0u8; wire::request_len(entries.len())];
        wire::encode_request(&entries, &mut req).unwrap();
        assert_eq!(mint(&req, &CFG, NOW, &mut out), Err(Malformed));
        let mut one = vec![0u8; wire::request_len(1)];
        wire::encode_request(&[file(5)], &mut one).unwrap();
        one[2] = 42;
        let n = mint(&one, &CFG, NOW, &mut out).unwrap();
        let e = wire::decode_reply(&out[..n], 1).unwrap().next().unwrap().unwrap();
        assert_eq!(e.status, Status::Malformed);
        assert_eq!(mint(&one[..one.len() - 1], &CFG, NOW, &mut out), Err(Malformed));
    }

    #[test]
    fn copies_are_signed_for_derived_keys_with_the_copy_credential() {
        use slop_ninja_r2_presign::copy::{CopyKind, CopySpec};
        let cfg = CopyConfig {
            host: CFG.host,
            source_bucket: "slop-ninja-admission-testnet",
            destination_bucket: "public-bucket-testnet",
            cred: Credentials { access_key_id: "CFCOPYKEY0001", secret_access_key: b"not-a-real-copy-secret" },
        };
        let spec = CopySpec { cohort: 9, commitment: [7; 32], kind: CopyKind::File, sha256: [8; 32], etag: [5; 16] };
        let mut out = vec![0u8; COPY_REPLY_BYTES];
        let n = copy(&wire::encode_copy_request(&spec, 900), &cfg, NOW, &mut out).unwrap();
        let r = wire::decode_copy_reply(&out[..n]).unwrap();
        assert_eq!(r.status, Status::Ok);
        assert!(r.url.starts_with(&format!("https://{}/public-bucket-testnet/blobs/sha256/{}?", CFG.host, "08".repeat(32))));
        assert!(r.url.contains("X-Amz-Credential=CFCOPYKEY0001%2F"));
        assert!(r.url.contains("x-amz-copy-source%3Bx-amz-copy-source-if-match%3Bx-amz-metadata-directive%3Bx-amz-storage-class"));
        // Validity outside R2's range is refused per request, not whole.
        let n = copy(&wire::encode_copy_request(&spec, 0), &cfg, NOW, &mut out).unwrap();
        assert_eq!(wire::decode_copy_reply(&out[..n]).unwrap().status, Status::Validity);
        // Anything but the exact request length is malformed.
        let req = wire::encode_copy_request(&spec, 900);
        assert_eq!(copy(&req[..req.len() - 1], &cfg, NOW, &mut out), Err(Malformed));
        assert_eq!(copy(&[], &cfg, NOW, &mut out), Err(Malformed));
    }

    #[test]
    fn a_full_reply_fits_its_bound() {
        let entries = vec![file(681_508_964); MAX_ENTRIES];
        let mut req = vec![0u8; wire::request_len(entries.len())];
        wire::encode_request(&entries, &mut req).unwrap();
        let long_host = "a".repeat(253 - ".r2.cloudflarestorage.com".len()) + ".r2.cloudflarestorage.com";
        let cfg = Config { host: &long_host, bucket: &"b".repeat(63), cred: Credentials { access_key_id: &"K".repeat(128), secret_access_key: b"s" } };
        let mut out = vec![0u8; REPLY_BYTES];
        let n = mint(&req, &cfg, NOW, &mut out).unwrap();
        assert!(n <= REPLY_BYTES);
        assert!(wire::decode_reply(&out[..n], MAX_ENTRIES).unwrap().all(|e| e.unwrap().status == Status::Ok));
    }
}
