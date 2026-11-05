//! The presigned `CopyObject` request of publication at scoring start.

use sha2::{Digest, Sha256};
use slop_ninja_r2_presign::copy::{
    COPY_SIGNED_HEADERS, CopyBuckets, CopyKind, CopySpec, MAX_COPY_URL_LEN, copy_headers, parse_etag, presign_copy,
};
use slop_ninja_r2_presign::layout::{ObjectId, ObjectKind, public_blob_key, public_header_key};
use slop_ninja_r2_presign::presign::{Credentials, PresignError};
use slop_ninja_r2_presign::wire::{
    COPY_REPLY_MAX, COPY_REQUEST_LEN, CopyReply, Status, decode_copy_reply, decode_copy_request, encode_copy_reply,
    encode_copy_request,
};

const HOST: &str = "0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com";
const BUCKETS: CopyBuckets<'static> =
    CopyBuckets { host: HOST, source: "slop-ninja-admission-testnet", destination: "public-bucket-testnet" };
/// 2025-12-01T18:05:12Z.
const NOW: u64 = 1_764_612_312;
const CRED: Credentials<'static> = Credentials {
    access_key_id: "CFEXAMPLEKEY12345",
    secret_access_key: b"examplesecret0123456789abcdef0123456789abcdef0123456789abcdef0123",
};

fn hello() -> [u8; 32] {
    Sha256::digest(b"hello").into()
}

/// MD5 of `hello`, the etag R2 gives a single-part object of those bytes.
const HELLO_ETAG: &str = "5d41402abc4b2a76b9719d911017c592";

fn file_spec() -> CopySpec {
    CopySpec {
        cohort: 42,
        commitment: [0xab; 32],
        kind: CopyKind::File,
        sha256: hello(),
        etag: parse_etag(HELLO_ETAG).unwrap(),
    }
}

/// Against an independent implementation (Python's `hmac`, `hashlib` and
/// `urllib.parse` over the same canonical request, 2026-11-05): the URL byte
/// for byte.
#[test]
fn presigned_copy_matches_an_independent_implementation() {
    let p = presign_copy(&CRED, &BUCKETS, &file_spec(), NOW, 900).unwrap();
    assert_eq!(
        p.url.as_str(),
        "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com/public-bucket-testnet/blobs/sha256/2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Content-Sha256=UNSIGNED-PAYLOAD&X-Amz-Credential=CFEXAMPLEKEY12345%2F20251201%2Fauto%2Fs3%2Faws4_request&X-Amz-Date=20251201T180512Z&X-Amz-Expires=900&X-Amz-Signature=3eaf04394b1b0cf231b8ac62a01e5a8a3b7e4bcc3e69fed37cc9967ef29afccf&X-Amz-SignedHeaders=cache-control%3Bcontent-type%3Bhost%3Bx-amz-copy-source%3Bx-amz-copy-source-if-match%3Bx-amz-metadata-directive%3Bx-amz-storage-class"
    );
    assert_eq!(
        p.copy_source.as_str(),
        "slop-ninja-admission-testnet/admission/v2/c42/abababababababababababababababababababababababababababababababab/f/2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert_eq!(p.if_match.as_str(), "\"5d41402abc4b2a76b9719d911017c592\"");
    assert_eq!((p.signed_at, p.valid_secs), (NOW, 900));
    let names: Vec<&str> = p.headers().iter().map(|h| h.0).collect();
    let mut signed: Vec<&str> = COPY_SIGNED_HEADERS.split(';').filter(|h| *h != "host").collect();
    signed.sort_unstable();
    assert_eq!(names, signed);
    let values = p.headers();
    assert!(values.contains(&("x-amz-metadata-directive", "REPLACE")));
    assert!(values.contains(&("x-amz-storage-class", "STANDARD")));
    assert!(values.contains(&("content-type", "application/octet-stream")));
}

#[test]
fn keys_are_derived_from_protocol_values() {
    let f = file_spec();
    assert_eq!(
        f.source_key().as_str(),
        ObjectId { cohort: 42, commitment: [0xab; 32], kind: ObjectKind::File(hello()) }.key().as_str()
    );
    assert_eq!(f.destination_key().as_str(), public_blob_key(&hello()).as_str());
    let h = CopySpec { kind: CopyKind::Header, sha256: [0x11; 32], ..f };
    assert_eq!(
        h.source_key().as_str(),
        ObjectId { cohort: 42, commitment: [0xab; 32], kind: ObjectKind::Header }.key().as_str()
    );
    assert_eq!(h.destination_key().as_str(), public_header_key(&[0x11; 32]).as_str());
    let p = presign_copy(&CRED, &BUCKETS, &h, NOW, 900).unwrap();
    assert!(p.url.as_str().contains(&format!("/public-bucket-testnet/artifacts/v2/{}/header?", "11".repeat(32))));
}

#[test]
fn every_signed_value_is_bound() {
    let base = presign_copy(&CRED, &BUCKETS, &file_spec(), NOW, 900).unwrap();
    let sig = |p: &slop_ninja_r2_presign::copy::PresignedCopy| {
        p.url.as_str().split("X-Amz-Signature=").nth(1).unwrap()[..64].to_owned()
    };
    let other_etag = CopySpec { etag: [0; 16], ..file_spec() };
    let other_cohort = CopySpec { cohort: 43, ..file_spec() };
    let other_buckets = CopyBuckets { source: "eval-bucket-testnet", ..BUCKETS };
    for p in [
        presign_copy(&CRED, &BUCKETS, &other_etag, NOW, 900).unwrap(),
        presign_copy(&CRED, &BUCKETS, &other_cohort, NOW, 900).unwrap(),
        presign_copy(&CRED, &other_buckets, &file_spec(), NOW, 900).unwrap(),
        presign_copy(&CRED, &BUCKETS, &file_spec(), NOW + 1, 900).unwrap(),
        presign_copy(&CRED, &BUCKETS, &file_spec(), NOW, 901).unwrap(),
    ] {
        assert_ne!(sig(&p), sig(&base));
    }
}

#[test]
fn refusals() {
    let f = file_spec();
    assert_eq!(presign_copy(&CRED, &BUCKETS, &f, NOW, 0).unwrap_err(), PresignError::Validity);
    assert_eq!(presign_copy(&CRED, &BUCKETS, &f, NOW, 604_801).unwrap_err(), PresignError::Validity);
    let same = CopyBuckets { destination: BUCKETS.source, ..BUCKETS };
    assert_eq!(presign_copy(&CRED, &same, &f, NOW, 900).unwrap_err(), PresignError::Format);
    let bad_host = CopyBuckets { host: "Evil.example", ..BUCKETS };
    assert_eq!(presign_copy(&CRED, &bad_host, &f, NOW, 900).unwrap_err(), PresignError::Format);
    let bad_id = Credentials { access_key_id: "bad id", ..CRED };
    assert_eq!(presign_copy(&bad_id, &BUCKETS, &f, NOW, 900).unwrap_err(), PresignError::Format);
}

#[test]
fn a_full_url_fits_its_bound() {
    let host = "a".repeat(253 - ".r2.cloudflarestorage.com".len()) + ".r2.cloudflarestorage.com";
    let src = "s".repeat(63);
    let dst = "d".repeat(63);
    let b = CopyBuckets { host: &host, source: &src, destination: &dst };
    let cred = Credentials { access_key_id: &"K".repeat(128), secret_access_key: b"s" };
    let p = presign_copy(&cred, &b, &CopySpec { cohort: u64::MAX, ..file_spec() }, NOW, 604_800).unwrap();
    assert!(p.url.len() <= MAX_COPY_URL_LEN);
}

#[test]
fn etags_parse_single_part_only() {
    assert_eq!(parse_etag(HELLO_ETAG), parse_etag(&format!("\"{HELLO_ETAG}\"")));
    assert_eq!(parse_etag(&HELLO_ETAG.to_uppercase()), parse_etag(HELLO_ETAG));
    assert_eq!(parse_etag(&format!("{HELLO_ETAG}-3")), None);
    assert_eq!(parse_etag("xyz"), None);
    assert_eq!(parse_etag(""), None);
}

#[test]
fn copy_messages_round_trip_and_refuse_other_lengths() {
    let f = file_spec();
    let req = encode_copy_request(&f, 900);
    assert_eq!(req.len(), COPY_REQUEST_LEN);
    assert_eq!(decode_copy_request(&req), Some((f, 900)));
    assert_eq!(decode_copy_request(&req[..COPY_REQUEST_LEN - 1]), None);
    let mut bad = req;
    bad[1] = 9;
    assert_eq!(decode_copy_request(&bad), None);
    bad = req;
    bad[0] = 2;
    assert_eq!(decode_copy_request(&bad), None);

    let p = presign_copy(&CRED, &BUCKETS, &f, NOW, 900).unwrap();
    let mut out = vec![0u8; COPY_REPLY_MAX];
    let r = CopyReply { status: Status::Ok, signed_at: p.signed_at, valid_secs: p.valid_secs, url: p.url.as_str() };
    let n = encode_copy_reply(&mut out, &r).unwrap();
    assert_eq!(decode_copy_reply(&out[..n]), Some(r));
    assert_eq!(decode_copy_reply(&out[..n - 1]), None);
    let mut longer = out[..n].to_vec();
    longer.push(b'x');
    assert_eq!(decode_copy_reply(&longer), None);
    // The headers the sender derives itself are the signed ones.
    let (source, if_match) = (f.copy_source(BUCKETS.source), f.if_match());
    let derived = copy_headers(source.as_str(), if_match.as_str());
    assert_eq!(derived, p.headers());
}
