//! SigV4 against the published AWS examples, and the presigned PUT form
//! against R2's documented form and an independent implementation.

use sha2::{Digest, Sha256};
use slop_ninja_r2_presign::buf::{Buf, base64_32, hex32};
use slop_ninja_r2_presign::check::{UrlError, check_put_url, put_headers};
use slop_ninja_r2_presign::layout::{ObjectId, ObjectKind};
use slop_ninja_r2_presign::presign::{Binding, Credentials, PresignError, Target, presign_put};
use slop_ninja_r2_presign::sigv4::{self, Canonical, CredentialScope, EMPTY_SHA256, UNSIGNED_PAYLOAD};
use slop_ninja_r2_presign::time::amz_dates;

/// AWS SigV4 documentation, "Examples of how to derive a signing key".
#[test]
fn aws_signing_key_example() {
    let scope = CredentialScope { date: "20120215", region: "us-east-1", service: "iam" };
    let key = sigv4::signing_key(b"wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", &scope);
    assert_eq!(hex32(&key).as_str(), "f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d");
}

const S3_SECRET: &[u8] = b"wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const S3_SCOPE: CredentialScope<'static> = CredentialScope { date: "20130524", region: "us-east-1", service: "s3" };
const S3_HOST: &str = "examplebucket.s3.amazonaws.com";

fn s3_signature(c: &Canonical<'_>) -> String {
    let digest = sigv4::canonical_digest(c).expect("canonical");
    sigv4::signature(S3_SECRET, "20130524T000000Z", &S3_SCOPE, &digest).expect("signature").as_str().to_owned()
}

/// Amazon S3 API reference, "Authenticating Requests: Using Query
/// Parameters (AWS Signature Version 4)": the presigned GET example.
#[test]
fn aws_s3_presigned_get_example() {
    let query = [
        ("X-Amz-Algorithm", "AWS4-HMAC-SHA256"),
        ("X-Amz-Credential", "AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request"),
        ("X-Amz-Date", "20130524T000000Z"),
        ("X-Amz-Expires", "86400"),
        ("X-Amz-SignedHeaders", "host"),
    ];
    let headers = [("host", S3_HOST)];
    let c = Canonical { method: "GET", path: "/test.txt", query: &query, headers: &headers, payload: UNSIGNED_PAYLOAD };
    let mut text: Buf<1024> = Buf::new();
    sigv4::write_canonical(&mut text, &c).expect("write");
    assert_eq!(
        text.as_str(),
        "GET\n/test.txt\nX-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&X-Amz-Date=20130524T000000Z&X-Amz-Expires=86400&X-Amz-SignedHeaders=host\nhost:examplebucket.s3.amazonaws.com\n\nhost\nUNSIGNED-PAYLOAD"
    );
    let digest = sigv4::canonical_digest(&c).expect("digest");
    let mut sts: Buf<512> = Buf::new();
    sigv4::write_string_to_sign(&mut sts, "20130524T000000Z", &S3_SCOPE, &digest).expect("sts");
    assert_eq!(
        sts.as_str(),
        "AWS4-HMAC-SHA256\n20130524T000000Z\n20130524/us-east-1/s3/aws4_request\n3bfa292879f6447bbcda7001decf97f4a54dc650c8942174ae0a9121cf58ad04"
    );
    assert_eq!(s3_signature(&c), "aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404");
}

/// Same reference, "Signature Calculations for the Authorization Header":
/// GET Object with a range.
#[test]
fn aws_s3_header_get_object_example() {
    let headers = [
        ("host", S3_HOST),
        ("range", "bytes=0-9"),
        ("x-amz-content-sha256", EMPTY_SHA256),
        ("x-amz-date", "20130524T000000Z"),
    ];
    let c = Canonical { method: "GET", path: "/test.txt", query: &[], headers: &headers, payload: EMPTY_SHA256 };
    let sig = s3_signature(&c);
    assert_eq!(sig, "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41");
    let mut auth: Buf<512> = Buf::new();
    sigv4::write_authorization(&mut auth, "AKIAIOSFODNN7EXAMPLE", &S3_SCOPE, &headers, &sig).expect("auth");
    assert_eq!(
        auth.as_str(),
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
    );
}

/// Same reference: PUT Object (a path byte that needs escaping).
#[test]
fn aws_s3_header_put_object_example() {
    let payload = "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072";
    let mut h = Sha256::new();
    h.update(b"Welcome to Amazon S3.");
    assert_eq!(hex32(&h.finalize().into()).as_str(), payload);
    let headers = [
        ("date", "Fri, 24 May 2013 00:00:00 GMT"),
        ("host", S3_HOST),
        ("x-amz-content-sha256", payload),
        ("x-amz-date", "20130524T000000Z"),
        ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
    ];
    let c = Canonical { method: "PUT", path: "/test$file.text", query: &[], headers: &headers, payload };
    let mut text: Buf<1024> = Buf::new();
    sigv4::write_canonical(&mut text, &c).expect("write");
    assert!(text.as_str().starts_with("PUT\n/test%24file.text\n\n"));
    assert_eq!(s3_signature(&c), "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd");
}

/// Same reference: GET Bucket lifecycle and GET Bucket (list objects), the
/// query canonicalization.
#[test]
fn aws_s3_header_query_examples() {
    let headers = [("host", S3_HOST), ("x-amz-content-sha256", EMPTY_SHA256), ("x-amz-date", "20130524T000000Z")];
    let lifecycle = Canonical { method: "GET", path: "/", query: &[("lifecycle", "")], headers: &headers, payload: EMPTY_SHA256 };
    assert_eq!(s3_signature(&lifecycle), "fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543");
    let list = Canonical {
        method: "GET",
        path: "/",
        query: &[("max-keys", "2"), ("prefix", "J")],
        headers: &headers,
        payload: EMPTY_SHA256,
    };
    assert_eq!(s3_signature(&list), "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7");
}

#[test]
fn canonical_refuses_what_it_cannot_sign_exactly() {
    let bad_order = Canonical { method: "GET", path: "/", query: &[("b", ""), ("a", "")], headers: &[], payload: "" };
    assert_eq!(sigv4::canonical_digest(&bad_order), Err(sigv4::CanonicalError::QueryOrder));
    let upper = Canonical { method: "GET", path: "/", query: &[], headers: &[("Host", "x")], payload: "" };
    assert_eq!(sigv4::canonical_digest(&upper), Err(sigv4::CanonicalError::Header));
    let spaced = Canonical { method: "GET", path: "/", query: &[], headers: &[("host", " x")], payload: "" };
    assert_eq!(sigv4::canonical_digest(&spaced), Err(sigv4::CanonicalError::Header));
    let relative = Canonical { method: "GET", path: "x", query: &[], headers: &[], payload: "" };
    assert_eq!(sigv4::canonical_digest(&relative), Err(sigv4::CanonicalError::Path));
}

const HOST: &str = "0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com";
const BUCKET: &str = "slop-ninja-admission-testnet";
/// 2025-12-01T18:05:12Z, the date of R2's documented example.
const NOW: u64 = 1_764_612_312;
const CRED: Credentials<'static> = Credentials {
    access_key_id: "CFEXAMPLEKEY12345",
    // 64 characters: with "AWS4" longer than SHA-256's block, so the
    // hashed-key path of HMAC is the one checked.
    secret_access_key: b"examplesecret0123456789abcdef0123456789abcdef0123456789abcdef0123",
};

fn hello() -> [u8; 32] {
    Sha256::digest(b"hello").into()
}

fn file_key() -> Buf<{ slop_ninja_r2_presign::layout::MAX_KEY_LEN }> {
    ObjectId { cohort: 42, commitment: [0xab; 32], kind: ObjectKind::File(hello()) }.key()
}

/// The presigned PUT, against an independent implementation (Python's
/// `hmac` and `hashlib` over the same canonical request, 2026-11-04): the
/// URL byte for byte.
#[test]
fn presigned_put_matches_an_independent_implementation() {
    let key = file_key();
    let p = presign_put(&CRED, &Target { host: HOST, bucket: BUCKET, key: key.as_str() }, &Binding { len: 5, sha256: hello() }, NOW, 3600)
        .expect("presign");
    assert_eq!(p.checksum.as_str(), "LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=");
    assert_eq!(
        p.url.as_str(),
        "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com/slop-ninja-admission-testnet/admission/v2/c42/abababababababababababababababababababababababababababababababab/f/2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Content-Sha256=UNSIGNED-PAYLOAD&X-Amz-Credential=CFEXAMPLEKEY12345%2F20251201%2Fauto%2Fs3%2Faws4_request&X-Amz-Date=20251201T180512Z&X-Amz-Expires=3600&X-Amz-Signature=47a586d56ba18530a2d698213772a1423372e76ee1e070889ac2a55b9eb5dc78&X-Amz-SignedHeaders=content-length%3Bhost%3Bif-none-match%3Bx-amz-checksum-sha256"
    );
    assert_eq!((p.content_length, p.signed_at, p.valid_secs), (5, NOW, 3600));
}

/// R2's documented presigned URL (presigned-urls page, "Presigned URL
/// example"): the same parameter names in the same sorted order, with
/// `X-Amz-Content-Sha256=UNSIGNED-PAYLOAD` in the query and the credential
/// scope `<id>/<date>/auto/s3/aws4_request`. That example is a GET in
/// virtual-host style; ours are PUTs in path style (the AWS CLI form the
/// same page shows), so only the shared parameters are compared.
#[test]
fn presigned_put_follows_r2_documented_form() {
    const R2_EXAMPLE: &str = "https://my-bucket.123456789abcdef0123456789abcdef.r2.cloudflarestorage.com/photos/cat.png?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Content-Sha256=UNSIGNED-PAYLOAD&X-Amz-Credential=CFEXAMPLEKEY12345%2F20251201%2Fauto%2Fs3%2Faws4_request&X-Amz-Date=20251201T180512Z&X-Amz-Expires=3600&X-Amz-Signature=8c3ac40fa6c83d64b4516e0c9e5fa94c998bb79131be9ddadf90cefc5ec31033&X-Amz-SignedHeaders=host&x-amz-checksum-mode=ENABLED&x-id=GetObject";
    let names = |url: &str| -> Vec<String> {
        url.split_once('?').unwrap().1.split('&').map(|p| p.split_once('=').unwrap().0.to_owned()).collect()
    };
    let value = |url: &str, n: &str| -> String {
        url.split_once('?').unwrap().1.split('&').find_map(|p| p.strip_prefix(&format!("{n}="))).unwrap().to_owned()
    };
    let key = file_key();
    let ours = presign_put(&CRED, &Target { host: HOST, bucket: BUCKET, key: key.as_str() }, &Binding { len: 5, sha256: hello() }, NOW, 3600)
        .expect("presign");
    let theirs: Vec<String> = names(R2_EXAMPLE).into_iter().filter(|n| n.starts_with("X-Amz-")).collect();
    assert_eq!(names(ours.url.as_str()), theirs);
    for n in ["X-Amz-Algorithm", "X-Amz-Content-Sha256", "X-Amz-Credential", "X-Amz-Date", "X-Amz-Expires"] {
        assert_eq!(value(ours.url.as_str(), n), value(R2_EXAMPLE, n), "{n}");
    }
    assert!(ours.url.as_str().starts_with(&format!("https://{HOST}/{BUCKET}/admission/v2/")));
}

#[test]
fn presign_refusals() {
    let key = file_key();
    let t = Target { host: HOST, bucket: BUCKET, key: key.as_str() };
    let b = Binding { len: 5, sha256: hello() };
    assert_eq!(presign_put(&CRED, &t, &Binding { len: 0, ..b }, NOW, 60).unwrap_err(), PresignError::Length);
    assert_eq!(presign_put(&CRED, &t, &Binding { len: 5_363_466_241, ..b }, NOW, 60).unwrap_err(), PresignError::Length);
    assert!(presign_put(&CRED, &t, &Binding { len: 5_363_466_240, ..b }, NOW, 60).is_ok());
    assert_eq!(presign_put(&CRED, &t, &b, NOW, 0).unwrap_err(), PresignError::Validity);
    assert_eq!(presign_put(&CRED, &t, &b, NOW, 604_801).unwrap_err(), PresignError::Validity);
    assert!(presign_put(&CRED, &t, &b, NOW, 604_800).is_ok());
    let bad_host = Target { host: "evil.example/x", ..t };
    assert_eq!(presign_put(&CRED, &bad_host, &b, NOW, 60).unwrap_err(), PresignError::Format);
    let bad_key = Target { key: "a b", ..t };
    assert_eq!(presign_put(&CRED, &bad_key, &b, NOW, 60).unwrap_err(), PresignError::Format);
    let bad_bucket = Target { bucket: "Upper", ..t };
    assert_eq!(presign_put(&CRED, &bad_bucket, &b, NOW, 60).unwrap_err(), PresignError::Format);
}

#[test]
fn signatures_bind_every_signed_value() {
    let key = file_key();
    let t = Target { host: HOST, bucket: BUCKET, key: key.as_str() };
    let base = presign_put(&CRED, &t, &Binding { len: 5, sha256: hello() }, NOW, 3600).unwrap();
    let sig = |p: &slop_ninja_r2_presign::presign::Presigned| p.url.as_str().split("X-Amz-Signature=").nth(1).unwrap()[..64].to_owned();
    let other_len = presign_put(&CRED, &t, &Binding { len: 6, sha256: hello() }, NOW, 3600).unwrap();
    let other_sha = presign_put(&CRED, &t, &Binding { len: 5, sha256: [1; 32] }, NOW, 3600).unwrap();
    let other_exp = presign_put(&CRED, &t, &Binding { len: 5, sha256: hello() }, NOW, 3601).unwrap();
    let key2 = ObjectId { cohort: 43, commitment: [0xab; 32], kind: ObjectKind::File(hello()) }.key();
    let other_key = presign_put(&CRED, &Target { key: key2.as_str(), ..t }, &Binding { len: 5, sha256: hello() }, NOW, 3600).unwrap();
    for p in [&other_len, &other_sha, &other_exp, &other_key] {
        assert_ne!(sig(p), sig(&base));
    }
}

#[test]
fn client_check_accepts_only_the_expected_url() {
    let key = file_key();
    let p = presign_put(&CRED, &Target { host: HOST, bucket: BUCKET, key: key.as_str() }, &Binding { len: 5, sha256: hello() }, NOW, 3600)
        .unwrap();
    let url = p.url.as_str();
    let ok = check_put_url(url, None, Some(BUCKET), key.as_str()).expect("accepted");
    assert_eq!((ok.host, ok.bucket, ok.expires, ok.date), (HOST, BUCKET, 3600, "20251201T180512Z"));
    assert!(check_put_url(url, Some(HOST), None, key.as_str()).is_ok());
    assert_eq!(check_put_url(url, Some("other.r2.cloudflarestorage.com"), None, key.as_str()), Err(UrlError::Host));
    assert_eq!(check_put_url(&url.replacen("https", "http", 1), None, None, key.as_str()), Err(UrlError::Scheme));
    let evil = url.replacen(HOST, "evil.example.com", 1);
    assert_eq!(check_put_url(&evil, None, None, key.as_str()), Err(UrlError::Host));
    let other = ObjectId { cohort: 42, commitment: [0xab; 32], kind: ObjectKind::Header }.key();
    assert_eq!(check_put_url(url, None, None, other.as_str()), Err(UrlError::Path));
    assert_eq!(check_put_url(url, None, Some("other-bucket"), key.as_str()), Err(UrlError::Path));
    assert_eq!(check_put_url(&format!("{url}&partNumber=1"), None, None, key.as_str()), Err(UrlError::Query));
    assert_eq!(check_put_url(&format!("{url}&X-Amz-Expires=60"), None, None, key.as_str()), Err(UrlError::Query));
    let fewer = url.replace("%3Bif-none-match", "");
    assert_eq!(check_put_url(&fewer, None, None, key.as_str()), Err(UrlError::Query));
    let no_alg = url.replace("X-Amz-Algorithm=AWS4-HMAC-SHA256&", "");
    assert_eq!(check_put_url(&no_alg, None, None, key.as_str()), Err(UrlError::Query));
    let userinfo = url.replacen(HOST, &format!("x@{HOST}"), 1);
    assert_eq!(check_put_url(&userinfo, None, None, key.as_str()), Err(UrlError::Host));
    let port = url.replacen(HOST, &format!("{HOST}:8443"), 1);
    assert_eq!(check_put_url(&port, None, None, key.as_str()), Err(UrlError::Host));
}

#[test]
fn put_headers_are_the_signed_values() {
    let (len, inm, checksum) = put_headers(5, &hello());
    assert_eq!((len.as_str(), inm, checksum.as_str()), ("5", "*", "LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ="));
    assert_eq!(base64_32(&Sha256::digest(b"").into()).as_str(), "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=");
}

#[test]
fn utc_date_stamps() {
    let (d, s) = amz_dates(1_369_353_600).unwrap();
    assert_eq!((d.as_str(), s.as_str()), ("20130524", "20130524T000000Z"));
    let (_, s) = amz_dates(1_709_164_799).unwrap();
    assert_eq!(s.as_str(), "20240228T235959Z");
    let (_, s) = amz_dates(1_709_164_800).unwrap();
    assert_eq!(s.as_str(), "20240229T000000Z");
    let (_, s) = amz_dates(951_868_800).unwrap();
    assert_eq!(s.as_str(), "20000301T000000Z");
    let (_, s) = amz_dates(0).unwrap();
    assert_eq!(s.as_str(), "19700101T000000Z");
    assert!(amz_dates(253_402_300_799).is_some());
    assert!(amz_dates(253_402_300_800).is_none());
}
