//! The public routes' front gate: everything the key-free upload Worker
//! (`subnet/admission-worker`) decides about an untrusted request before it
//! forwards anything, as storage-neutral functions (natively tested, and
//! run by the miner client's mock in its `upload_gate` mode).
//!
//! Order, each step before any later work (decision D6, 2026-11-03), run
//! as one sequence by [`decide`] (the Worker and the miner mock call it):
//!
//! 1. the route parses (`slop_ninja_admission_wire::Route::parse`, which
//!    bounds every segment by its format);
//! 2. the contract's one unsigned route, the cohort read, needs nothing
//!    more (the Worker serves it from the cohort snapshot in storage, never
//!    from the eval Worker); every other route must carry the four
//!    signature headers, and the sr25519 signature must verify over the
//!    request message for `x-sn-hotkey` ([`verify`]): CPU only, no I/O;
//! 3. in `hotkeys` mode the signer must be listed ([`Access`]);
//! 4. header-only checks, still without I/O: a hotkey path segment must
//!    decode as SS58 and name the signer ([`route_fields`], [`OTHER_HOTKEY`]);
//!    a creation's media type; a bodiless route's signed digest must be the
//!    empty string's ([`NOT_EMPTY_DIGEST`]); a declared `Content-Length`
//!    within the route's bound ([`declared_length`]). No route of contract
//!    v2 takes an upload byte: every object goes straight to R2 under a
//!    presigned URL, so the largest body this Worker reads is one
//!    `SubmissionUpload` record;
//! 5. the signer must hold a UID on the subnet at the finalized head
//!    ([`Io::registered`]: the upload Worker's per-isolate roster of the
//!    subnet's hotkeys, read once a block, so the lookup itself is in
//!    memory);
//! 6. only then is a body read, within its route's bound ([`Body`]); a body
//!    read whole must match the signed `x-sn-content-sha256`, and a session
//!    creation's must decode as a canonical `SubmissionUpload`
//!    ([`upload_record`]).
//!
//! `valid_until` is checked only by the cohort authority, against its own
//! cursor and validity window: the Worker's chain head can run ahead of
//! that cursor, so a check against the head could refuse requests the
//! authority accepts.
//!
//! Nothing here is a tuned rate: the bounds are the contract's record size,
//! the artifact spec's object bound and the session protocol (one session
//! per hotkey and commitment, one artifact per hotkey and cohort, enforced
//! by the cohort authority), beside Cloudflare's own DDoS protection.

#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)
)]

use rtao_core::keys;
use slop_ninja_admission_wire::internal::{Reply, Signed};
use slop_ninja_admission_wire::json::{Detail, ErrorBody};

mod decide;
pub use decide::*;
use slop_ninja_admission_wire::{ErrorCode, RequestScope, Route, request_message};
use slop_ninja_protocol::{AccountId, ChainBinding, Digest};

/// Bittensor's SS58 prefix (presentation only).
pub const SS58_FORMAT: u16 = 42;

/// A session-creation body: exactly one fixed-size `SubmissionUpload`
/// record (`slop_ninja_admission_wire::UPLOAD_RECORD_BYTES`); a larger body
/// is refused with `bad_request`.
pub const UPLOAD_BODY_BYTES: usize = slop_ninja_admission_wire::UPLOAD_RECORD_BYTES;
/// A bodiless signed request's body bound: these routes take no body (their
/// signed digest is the empty string's).
pub const EMPTY_BODY_BYTES: usize = 0;

/// A refused request: its contract code and message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: ErrorCode,
    pub message: &'static str,
    pub retry_after: Option<u64>,
}

impl Refusal {
    pub const fn new(code: ErrorCode, message: &'static str) -> Self {
        Self { code, message, retry_after: None }
    }

    pub const fn retry(code: ErrorCode, message: &'static str, secs: u64) -> Self {
        Self { code, message, retry_after: Some(secs) }
    }

    /// The contract's error response.
    pub fn reply(&self) -> Reply {
        let body = serde_json::to_value(ErrorBody::new(self.code, self.message, Detail::default()))
            .unwrap_or(serde_json::Value::Null);
        Reply::new(self.code.http_status().unwrap_or(500), body, self.retry_after)
    }
}

/// Who the public routes serve (`ADMISSION_ROUTES`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    /// Absent or `off`: the routes do not exist (404).
    Off,
    /// Only the listed hotkeys (`ADMISSION_HOTKEYS`, SS58, comma
    /// separated): operator drills.
    Hotkeys(Vec<String>),
    /// Every hotkey registered on the subnet.
    Open,
}

impl Access {
    pub fn parse(routes: Option<&str>, hotkeys: Option<&str>) -> Self {
        match routes.map(str::trim) {
            Some("open") => Access::Open,
            Some("hotkeys") => Access::Hotkeys(
                hotkeys
                    .unwrap_or_default()
                    .split(',')
                    .map(str::trim)
                    .filter(|h| !h.is_empty())
                    .map(str::to_owned)
                    .collect(),
            ),
            _ => Access::Off,
        }
    }

    /// Whether `signer` may use the routes (in `hotkeys` mode an unlisted
    /// signer sees no route).
    pub fn admits(&self, signer: &AccountId) -> bool {
        match self {
            Access::Off => false,
            Access::Open => true,
            Access::Hotkeys(list) => {
                let ss58 = keys::ss58_from_public(signer.0, SS58_FORMAT);
                list.contains(&ss58)
            }
        }
    }
}

/// The request headers the gate reads, as sent (all untrusted).
#[derive(Clone, Debug, Default)]
pub struct Headers {
    pub hotkey: Option<String>,
    pub valid_until: Option<String>,
    pub content_sha256: Option<String>,
    pub signature: Option<String>,
    pub commitment_block: Option<String>,
}

/// The signed fields of a request, exactly as sent.
pub fn signed_of(method: &str, path: &str, h: &Headers) -> Signed {
    Signed {
        method: method.to_owned(),
        path: path.to_owned(),
        hotkey: h.hotkey.clone(),
        valid_until: h.valid_until.clone(),
        content_sha256: h.content_sha256.clone(),
        signature: h.signature.clone(),
        body_sha256: None,
        body: None,
        // Untrusted; anything but a decimal block is no hint.
        commitment_block: h.commitment_block.as_deref().and_then(|v| v.trim().parse().ok()),
    }
}

/// A request whose signature verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verified {
    pub signer: AccountId,
    /// The signed `x-sn-content-sha256`.
    pub content: [u8; 32],
    pub valid_until: u64,
}

/// The stateless request check: the headers parse and the sr25519
/// signature verifies over the request message (chain, hotkey, validity
/// bound, method, path, content digest). The cohort authority runs the same
/// check again with the cursor and validity window. Every header must parse
/// exactly as sent (no trimming), so a forwarded request's fields are
/// bounded by their formats and the authority parses them the same way.
pub fn verify(chain: &ChainBinding, r: &Signed) -> Result<Verified, Refusal> {
    let hotkey = r
        .hotkey
        .as_deref()
        .and_then(|h| keys::public_key_from_ss58(h).ok())
        .ok_or(Refusal::new(ErrorCode::BadSignature, "x-sn-hotkey is not an SS58 account"))?;
    let valid_until = r
        .valid_until
        .as_deref()
        .and_then(slop_ninja_admission_wire::parse_valid_until)
        .ok_or(Refusal::new(ErrorCode::BadRequest, "x-sn-valid-until"))?;
    let content = r
        .content_sha256
        .as_deref()
        .and_then(slop_ninja_admission_wire::parse_hex32)
        .ok_or(Refusal::new(ErrorCode::BadRequest, "x-sn-content-sha256"))?;
    let signature = r
        .signature
        .as_deref()
        .and_then(slop_ninja_admission_wire::parse_signature)
        .ok_or(Refusal::new(ErrorCode::BadSignature, "x-sn-signature"))?;
    let account = AccountId(hotkey);
    let message = request_message(&RequestScope {
        chain,
        hotkey: &account,
        valid_until,
        method: &r.method,
        path: &r.path,
        content_sha256: &Digest(content),
    });
    let ss58 = keys::ss58_from_public(hotkey, SS58_FORMAT);
    if !keys::verify_strict(&message, &signature, &ss58, keys::CRYPTO_SR25519).unwrap_or(false) {
        return Err(Refusal::new(
            ErrorCode::BadSignature,
            "the signature does not verify for x-sn-hotkey",
        ));
    }
    Ok(Verified {
        signer: account,
        content,
        valid_until,
    })
}

/// Steps 2 and 3: the signature, then the access mode.
pub fn admit(access: &Access, chain: &ChainBinding, r: &Signed) -> Result<Verified, Refusal> {
    let v = verify(chain, r)?;
    if !access.admits(&v.signer) {
        return Err(Refusal::new(ErrorCode::NotFound, "no such route"));
    }
    Ok(v)
}

/// The refusal of a signer that holds no UID on the subnet.
pub const NOT_REGISTERED: Refusal = Refusal::new(
    ErrorCode::NotRegistered,
    "the hotkey holds no UID on the subnet at the finalized head",
);

/// The answer when the chain cannot be read: retry, never a refusal.
pub const CHAIN_UNAVAILABLE: Refusal =
    Refusal::retry(ErrorCode::Unavailable, "the chain could not be read; try again", 12);

/// Which body a route takes, and the interface operation it forwards to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    /// The unsigned cohort read: no body is read.
    None,
    /// A `SubmissionUpload` record, read whole within [`UPLOAD_BODY_BYTES`].
    Upload,
    /// An empty body, read within [`EMPTY_BODY_BYTES`] (its digest signed).
    Empty,
}

/// The route's body kind and interface operation.
pub fn plan(route: &Route<'_>) -> (Body, &'static str) {
    use slop_ninja_admission_wire::internal::op;
    match route {
        Route::ReadCohort => (Body::None, op::COHORT),
        Route::CreateSession => (Body::Upload, op::CREATE),
        Route::ReadSession { .. } => (Body::Empty, op::READ),
        Route::Urls { .. } => (Body::Empty, op::URLS),
        Route::Complete { .. } => (Body::Empty, op::COMPLETE),
        Route::Repair { .. } => (Body::Empty, op::REPAIR),
        Route::ReadHotkey { .. } => (Body::Empty, op::HOTKEY),
    }
}

/// Step 6 for a body read whole: its SHA-256 must be the signed digest.
pub fn body_matches(v: &Verified, body: &[u8]) -> Result<String, Refusal> {
    use sha2::{Digest as _, Sha256};
    let sha: [u8; 32] = Sha256::digest(body).into();
    if sha != v.content {
        return Err(Refusal::new(
            ErrorCode::ContentDigestMismatch,
            "the body does not match x-sn-content-sha256",
        ));
    }
    Ok(hex::encode(sha))
}

/// Step 6 for a session creation: the body must decode as a canonical
/// `SubmissionUpload` record. The cohort authority decodes the same bytes
/// again (the record is its session state); the only uploaded bytes it ever
/// decodes are this fixed-shape record of at most [`UPLOAD_BODY_BYTES`],
/// already decoded here.
pub fn upload_record(body: &[u8]) -> Result<(), Refusal> {
    use slop_ninja_protocol::Record;
    slop_ninja_protocol::submission::SubmissionUpload::from_bytes(body)
        .map(|_| ())
        .map_err(|_| Refusal::new(ErrorCode::BadRequest, "the body is not a submission-upload record"))
}

/// Step 4 for the route's parameters: a hotkey segment (bounded by the
/// parser's SS58 shape) must decode as an SS58 account, so a forwarded
/// hotkey is one the cohort authority can read.
pub fn route_fields(route: &Route<'_>) -> Result<(), Refusal> {
    match route {
        Route::ReadHotkey { hotkey, .. } if keys::public_key_from_ss58(hotkey).is_err() => {
            Err(Refusal::new(ErrorCode::BadRequest, "the hotkey segment is not an SS58 account"))
        }
        _ => Ok(()),
    }
}

/// The refusal of a body longer than its route takes.
pub const BODY_TOO_LARGE: Refusal = Refusal::new(ErrorCode::BadRequest, "the body is larger than this route takes");

/// Step 4 for the declared length: a route's declared `Content-Length`,
/// when it describes the bytes (no `Content-Encoding`), must be within the
/// route's bound. The body reader enforces the same bound on the bytes (a
/// body without a declared length included).
pub fn declared_length(body: Body, content_length: Option<&str>, content_encoding: Option<&str>) -> Result<Option<u64>, Refusal> {
    let bound = match body {
        Body::None => return Ok(None),
        Body::Upload => UPLOAD_BODY_BYTES,
        Body::Empty => EMPTY_BODY_BYTES,
    };
    let encoded = content_encoding.is_some_and(|e| {
        let e = e.trim();
        !e.is_empty() && !e.eq_ignore_ascii_case("identity")
    });
    match content_length.and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(n) if !encoded && n > bound as u64 => Err(BODY_TOO_LARGE),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtao_core::keys::Keypair;
    use slop_ninja_protocol::TESTNET_GENESIS;

    pub(crate) fn chain() -> ChainBinding {
        ChainBinding {
            genesis: TESTNET_GENESIS,
            netuid: 601,
            mechanism: 0,
        }
    }

    fn pair(uri: &str) -> Keypair {
        Keypair::from_uri(uri, keys::CRYPTO_SR25519).unwrap()
    }

    pub(crate) fn signed(kp: &Keypair, method: &str, path: &str, body: &[u8], valid_until: u64) -> Signed {
        use sha2::{Digest as _, Sha256};
        let hotkey = AccountId(kp.public_key_bytes());
        let content: [u8; 32] = Sha256::digest(body).into();
        let message = request_message(&RequestScope {
            chain: &chain(),
            hotkey: &hotkey,
            valid_until,
            method,
            path,
            content_sha256: &Digest(content),
        });
        let sig = kp.sign(&message).unwrap();
        signed_of(
            method,
            path,
            &Headers {
                hotkey: Some(keys::ss58_from_public(hotkey.0, SS58_FORMAT)),
                valid_until: Some(valid_until.to_string()),
                content_sha256: Some(hex::encode(content)),
                signature: Some(hex::encode(sig)),
                commitment_block: Some("17".into()),
            },
        )
    }

    #[test]
    fn a_signed_request_verifies_and_any_change_refuses_it() {
        let kp = pair("//Alice");
        let r = signed(&kp, "GET", "/admission/v2/sessions/{S}", &[], 100);
        let v = verify(&chain(), &r).unwrap();
        assert_eq!(v.signer.0, kp.public_key_bytes());
        assert_eq!(r.commitment_block, Some(17));
        assert!(body_matches(&v, &[]).is_ok());
        assert_eq!(body_matches(&v, b"x").unwrap_err().code, ErrorCode::ContentDigestMismatch);
        for tamper in [
            |r: &mut Signed| r.path.push('x'),
            |r: &mut Signed| r.method = "POST".into(),
            |r: &mut Signed| r.valid_until = Some("101".into()),
            |r: &mut Signed| r.content_sha256 = Some(hex::encode([1u8; 32])),
        ] {
            let mut t = r.clone();
            tamper(&mut t);
            assert_eq!(verify(&chain(), &t).unwrap_err().code, ErrorCode::BadSignature);
        }
        let other = ChainBinding { netuid: 602, ..chain() };
        assert_eq!(verify(&other, &r).unwrap_err().code, ErrorCode::BadSignature);
    }

    #[test]
    fn unsigned_and_malformed_requests_are_refused_before_any_io() {
        let empty = signed_of("POST", "/admission/v2/sessions", &Headers::default());
        assert_eq!(verify(&chain(), &empty).unwrap_err().code, ErrorCode::BadSignature);
        let kp = pair("//Bob");
        let mut r = signed(&kp, "POST", "/admission/v2/sessions", b"body", 9);
        r.signature = Some("0x00".into());
        assert_eq!(verify(&chain(), &r).unwrap_err().code, ErrorCode::BadSignature);
        let mut r = signed(&kp, "POST", "/admission/v2/sessions", b"body", 9);
        r.hotkey = r.hotkey.map(|h| format!(" {h}"));
        assert_eq!(verify(&chain(), &r).unwrap_err().code, ErrorCode::BadSignature, "parsed as sent");
        let mut r = signed(&kp, "POST", "/admission/v2/sessions", b"body", 9);
        r.valid_until = Some("soon".into());
        assert_eq!(verify(&chain(), &r).unwrap_err().code, ErrorCode::BadRequest);
    }

    #[test]
    fn access_modes() {
        let alice = pair("//Alice");
        let bob = pair("//Bob");
        let a = AccountId(alice.public_key_bytes());
        let b = AccountId(bob.public_key_bytes());
        assert!(!Access::parse(None, None).admits(&a));
        assert!(!Access::parse(Some("off"), None).admits(&a));
        assert!(Access::parse(Some("open"), None).admits(&a));
        let list = keys::ss58_from_public(a.0, SS58_FORMAT);
        let h = Access::parse(Some("hotkeys"), Some(&format!(" {list} ,")));
        assert!(h.admits(&a) && !h.admits(&b));
        let r = signed(&bob, "GET", "/admission/v2/sessions/{S}", &[], 5);
        assert_eq!(admit(&h, &chain(), &r).unwrap_err().code, ErrorCode::NotFound);
    }

    #[test]
    fn header_only_checks_bound_every_route_before_any_io() {
        // Declared lengths: a creation within one record, a bodiless route
        // none; an encoded body's length is the reader's to bound. No route
        // takes more than one record: every upload byte goes to R2.
        let n = |v: usize| v.to_string();
        assert_eq!(declared_length(Body::Upload, Some(&n(UPLOAD_BODY_BYTES)), None), Ok(None));
        assert_eq!(declared_length(Body::Upload, Some(&n(UPLOAD_BODY_BYTES + 1)), None), Err(BODY_TOO_LARGE));
        assert_eq!(declared_length(Body::Upload, Some(&n(UPLOAD_BODY_BYTES + 1)), Some("gzip")), Ok(None));
        assert_eq!(declared_length(Body::Upload, Some(&n(UPLOAD_BODY_BYTES + 1)), Some("identity")), Err(BODY_TOO_LARGE));
        assert_eq!(declared_length(Body::Empty, Some("0"), None), Ok(None));
        assert_eq!(declared_length(Body::Empty, None, None), Ok(None));
        assert_eq!(declared_length(Body::Empty, Some("1"), None), Err(BODY_TOO_LARGE));
        assert_eq!(UPLOAD_BODY_BYTES, slop_ninja_protocol::submission::SubmissionUpload::ENCODED_LEN);
        // A hotkey segment must decode as SS58.
        let alice = keys::ss58_from_public(pair("//Alice").public_key_bytes(), SS58_FORMAT);
        let ok = format!("/admission/v2/cohorts/7/hotkeys/{alice}");
        assert_eq!(route_fields(&Route::parse("GET", &ok).unwrap()), Ok(()));
        let mut broken = alice.clone();
        broken.pop();
        broken.push(if alice.ends_with('2') { '3' } else { '2' });
        let bad = format!("/admission/v2/cohorts/7/hotkeys/{broken}");
        assert_eq!(route_fields(&Route::parse("GET", &bad).unwrap()).unwrap_err().code, ErrorCode::BadRequest);
    }

    #[test]
    fn a_creation_body_must_be_a_canonical_upload_record() {
        assert_eq!(upload_record(b"not a record").unwrap_err().code, ErrorCode::BadRequest);
        assert_eq!(upload_record(&[]).unwrap_err().code, ErrorCode::BadRequest);
    }

    #[test]
    fn refusals_are_contract_errors() {
        let r = NOT_REGISTERED.reply();
        assert_eq!(Some(r.status), ErrorCode::NotRegistered.http_status());
        assert_eq!(r.body["error"]["code"], "not_registered");
        let u = CHAIN_UNAVAILABLE.reply();
        assert_eq!(u.status, 503);
        assert_eq!(u.retry_after, Some(12));
    }

    #[test]
    fn every_route_has_a_body_kind_and_an_interface_operation() {
        use slop_ninja_admission_wire::internal::op;
        let routes = [
            ("GET", "/admission/v2/cohort"),
            ("POST", "/admission/v2/sessions"),
            ("GET", "/admission/v2/sessions/{S}"),
            ("POST", "/admission/v2/sessions/{S}/urls"),
            ("POST", "/admission/v2/sessions/{S}/complete"),
            ("POST", "/admission/v2/sessions/{S}/repair"),
        ];
        let sid = "ab".repeat(32);
        for (m, p) in routes {
            let p = p.replace("{S}", &sid);
            let route = Route::parse(m, &p).unwrap_or_else(|| panic!("{m} {p}"));
            let (body, o) = plan(&route);
            // The cohort read is served from the snapshot, never forwarded.
            assert_eq!(op::ALL.contains(&o), body != Body::None, "{m} {p}");
        }
    }
}
