//! The whole front sequence of one public request, in the order the upload
//! Worker runs it, with its I/O behind [`Io`]. The Worker (`cf.rs`) and the
//! miner client's mock both call [`decide`], so the order the tests check is
//! the order the deployed Worker runs, and a test can count the I/O each
//! refusal costs (this module's tests).
//!
//! I/O happens at two points only, each after every check that needs
//! nothing but the request line and headers: the registration lookup
//! ([`Io::registered`], answered from the per-isolate roster of the subnet's
//! hotkeys at the finalized head, read once a block), then the body read
//! ([`Io::read_body`], within the route's bound). An unsigned, unlisted,
//! malformed or oversized request costs no I/O; an unregistered one costs
//! the lookup only.

use crate::{
    Access, BODY_TOO_LARGE, Body, CHAIN_UNAVAILABLE, EMPTY_BODY_BYTES, Headers, NOT_REGISTERED, Refusal, SS58_FORMAT,
    UPLOAD_BODY_BYTES, admit, body_matches, declared_length, plan, route_fields, signed_of, upload_record,
};
use rtao_core::keys;
use slop_ninja_admission_wire::internal::Forward;
use slop_ninja_admission_wire::{ErrorCode, Route, UPLOAD_MEDIA_TYPE};
use slop_ninja_protocol::{AccountId, ChainBinding};

/// One public request as received: the request line and the headers the
/// sequence reads (all untrusted).
#[derive(Clone, Debug, Default)]
pub struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub headers: Headers,
    pub content_length: Option<&'a str>,
    pub content_encoding: Option<&'a str>,
    pub content_type: Option<&'a str>,
}

/// Why a body read failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyFailure {
    /// The body is longer than the route takes: the client's.
    TooLarge,
    /// The platform's (an arena refusal, the transport, a timeout): the
    /// client sends the same request again.
    Unavailable,
}

/// The answer to a body the platform could not read: retryable.
pub const BODY_UNAVAILABLE: Refusal = Refusal::retry(ErrorCode::Unavailable, "the body could not be read; send it again", 2);

/// The I/O of the sequence.
#[allow(async_fn_in_trait)]
pub trait Io {
    /// Whether `who` holds a UID on the subnet at the finalized head.
    /// `Err`: the chain could not be read (the client retries).
    async fn registered(&mut self, who: &AccountId) -> Result<bool, ()>;
    /// The request body, at most `bound` bytes.
    async fn read_body(&mut self, bound: usize) -> Result<Vec<u8>, BodyFailure>;
}

/// What the Worker does with a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Answer with this refusal; nothing is forwarded.
    Refuse(Refusal),
    /// The contract's one unsigned read, the cohort response.
    Cohort,
    /// A part: `part-begin` with this message, then the body (`len` bytes,
    /// never read by the Worker) streams to R2.
    Part { forward: Forward, len: u64 },
    /// One message to the interface operation `op`.
    Forward { op: &'static str, forward: Forward },
}

/// The refusal of a bodiless route whose signed digest is not the empty
/// string's: such a request can never match its body.
pub const NOT_EMPTY_DIGEST: Refusal =
    Refusal::new(ErrorCode::ContentDigestMismatch, "this route takes no body; x-sn-content-sha256 must be the empty string's");
/// The refusal of a hotkey read for another hotkey than the signer.
pub const OTHER_HOTKEY: Refusal = Refusal::new(ErrorCode::Forbidden, "the signer is not the hotkey the path names");
/// The refusal of a creation whose media type is not the record's.
pub const NOT_UPLOAD_MEDIA: Refusal = Refusal::new(ErrorCode::UnsupportedMediaType, "the body is not a submission-upload record");

/// Runs the front sequence (module docs of [`crate`] for the order). The
/// request's validity window (`valid_until`) is the cohort authority's to
/// check against its own cursor: the Worker reads the chain head for the
/// roster only, and a check against it could refuse a request the authority
/// accepts whenever the authority's cursor lags the head.
pub async fn decide(io: &mut impl Io, access: &Access, chain: &ChainBinding, r: &Request<'_>) -> Decision {
    use Decision::Refuse;
    let Some(route) = Route::parse(r.method, r.path) else {
        return Refuse(Refusal::new(ErrorCode::BadRequest, "no such route or method"));
    };
    let (body, op) = plan(&route);
    if body == Body::None {
        return Decision::Cohort;
    }
    // Signature and access mode: CPU only.
    let mut signed = signed_of(r.method, r.path, &r.headers);
    let v = match admit(access, chain, &signed) {
        Ok(v) => v,
        Err(e) => return Refuse(e),
    };
    // Header-only checks, still before any I/O.
    if let Err(e) = route_fields(&route) {
        return Refuse(e);
    }
    let declared = match declared_length(body, r.content_length, r.content_encoding) {
        Ok(n) => n,
        Err(e) => return Refuse(e),
    };
    match (body, &route) {
        (Body::Upload, _) if r.content_type.unwrap_or_default().split(';').next().map(str::trim) != Some(UPLOAD_MEDIA_TYPE) => {
            return Refuse(NOT_UPLOAD_MEDIA);
        }
        (Body::Empty, _) if v.content != slop_ninja_admission_wire::empty_sha256().0 => return Refuse(NOT_EMPTY_DIGEST),
        _ => {}
    }
    if let Route::ReadHotkey { hotkey, .. } = &route
        && *hotkey != keys::ss58_from_public(v.signer.0, SS58_FORMAT)
    {
        return Refuse(OTHER_HOTKEY);
    }
    // Registration at the finalized head (the roster, in memory).
    match io.registered(&v.signer).await {
        Ok(true) => {}
        Ok(false) => return Refuse(NOT_REGISTERED),
        Err(()) => return Refuse(CHAIN_UNAVAILABLE),
    }
    let mut f = Forward::default();
    match &route {
        Route::PutPart { session, part } => {
            let len = declared.unwrap_or(0);
            f.signed = signed;
            f.session = (*session).to_owned();
            f.part = *part;
            f.content_length = Some(len);
            return Decision::Part { forward: f, len };
        }
        Route::ReadSession { session } | Route::Complete { session } => f.session = (*session).to_owned(),
        Route::Abort { session, attempt } | Route::Reopen { session, attempt } => {
            f.session = (*session).to_owned();
            f.attempt = Some(*attempt);
        }
        Route::ReadHotkey { cohort, hotkey } => {
            f.cohort = *cohort;
            f.hotkey = (*hotkey).to_owned();
        }
        Route::CreateSession | Route::ReadCohort => {}
    }
    let bound = if body == Body::Upload { UPLOAD_BODY_BYTES } else { EMPTY_BODY_BYTES };
    let bytes = match io.read_body(bound).await {
        Ok(b) => b,
        Err(BodyFailure::TooLarge) => return Refuse(BODY_TOO_LARGE),
        Err(BodyFailure::Unavailable) => return Refuse(BODY_UNAVAILABLE),
    };
    match body_matches(&v, &bytes) {
        Ok(sha) => signed.body_sha256 = Some(sha),
        Err(e) => return Refuse(e),
    }
    if body == Body::Upload {
        if let Err(e) = upload_record(&bytes) {
            return Refuse(e);
        }
        signed.body = Some(hex::encode(&bytes));
    }
    f.signed = signed;
    Decision::Forward { op, forward: f }
}

/// Polls `f` once with a no-op waker: the result of a future whose I/O is
/// already in hand (the mock's registry and body, the tests), or `None`.
pub fn ready<F: core::future::Future>(f: F) -> Option<F::Output> {
    let mut f = core::pin::pin!(f);
    let mut cx = core::task::Context::from_waker(core::task::Waker::noop());
    match f.as_mut().poll(&mut cx) {
        core::task::Poll::Ready(v) => Some(v),
        core::task::Poll::Pending => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{chain, signed};
    use rtao_core::keys::Keypair;
    use slop_ninja_admission_wire::internal::{Signed, op};

    /// Counts every I/O call.
    #[derive(Default)]
    struct Counting {
        registered: bool,
        chain_down: bool,
        body: Vec<u8>,
        body_fails: Option<BodyFailure>,
        lookups: u32,
        reads: u32,
    }

    impl Io for Counting {
        async fn registered(&mut self, _who: &AccountId) -> Result<bool, ()> {
            self.lookups += 1;
            if self.chain_down { Err(()) } else { Ok(self.registered) }
        }
        async fn read_body(&mut self, bound: usize) -> Result<Vec<u8>, BodyFailure> {
            self.reads += 1;
            if let Some(f) = self.body_fails {
                return Err(f);
            }
            if self.body.len() > bound {
                return Err(BodyFailure::TooLarge);
            }
            Ok(self.body.clone())
        }
    }

    fn pair(uri: &str) -> Keypair {
        Keypair::from_uri(uri, keys::CRYPTO_SR25519).unwrap()
    }

    fn headers(s: &Signed) -> Headers {
        Headers {
            hotkey: s.hotkey.clone(),
            valid_until: s.valid_until.clone(),
            content_sha256: s.content_sha256.clone(),
            signature: s.signature.clone(),
            commitment_block: s.commitment_block.map(|b| b.to_string()),
        }
    }

    /// Runs the sequence; returns the decision and the I/O it cost.
    fn run(io: &mut Counting, access: &Access, r: &Request<'_>) -> (Decision, u32, u32) {
        let d = ready(decide(io, access, &chain(), r)).expect("ready");
        (d, io.lookups, io.reads)
    }

    fn refused(d: &Decision) -> ErrorCode {
        match d {
            Decision::Refuse(r) => r.code,
            other => panic!("not refused: {other:?}"),
        }
    }

    #[test]
    fn every_refusal_costs_only_the_io_before_it() {
        let alice = pair("//Alice");
        let sid = "ab".repeat(32);
        let read_path = format!("/admission/v1/sessions/{sid}");
        let open = Access::Open;
        let reg = || Counting { registered: true, ..Counting::default() };

        // Unsigned: no I/O.
        let r = Request { method: "GET", path: &read_path, ..Request::default() };
        let (d, l, b) = run(&mut reg(), &open, &r);
        assert_eq!((refused(&d), l, b), (ErrorCode::BadSignature, 0, 0));

        // Unlisted in hotkeys mode: no I/O.
        let s = signed(&alice, "GET", &read_path, &[], 100);
        let r = Request { method: "GET", path: &read_path, headers: headers(&s), ..Request::default() };
        let (d, l, b) = run(&mut reg(), &Access::Hotkeys(vec![]), &r);
        assert_eq!((refused(&d), l, b), (ErrorCode::NotFound, 0, 0));

        // Unregistered: the lookup only.
        let (d, l, b) = run(&mut Counting::default(), &open, &r);
        assert_eq!((refused(&d), l, b), (ErrorCode::NotRegistered, 1, 0));

        // The chain unreadable: retryable, the lookup only.
        let mut down = Counting { chain_down: true, ..Counting::default() };
        let (d, l, b) = run(&mut down, &open, &r);
        assert_eq!((refused(&d), l, b), (ErrorCode::Unavailable, 1, 0));

        // Expired by any clock: the authority's to decide against its own
        // cursor, so the gate forwards it (never stricter than the authority).
        let s0 = signed(&alice, "GET", &read_path, &[], 0);
        let r0 = Request { method: "GET", path: &read_path, headers: headers(&s0), ..Request::default() };
        let (d, l, b) = run(&mut reg(), &open, &r0);
        assert!(matches!(d, Decision::Forward { op: op::READ, .. }), "{d:?}");
        assert_eq!((l, b), (1, 1));

        // Oversized by its declared length: no I/O.
        let create = "/admission/v1/sessions";
        let body = vec![0u8; UPLOAD_BODY_BYTES + 1];
        let s = signed(&alice, "POST", create, &body, 100);
        let too_long = (UPLOAD_BODY_BYTES + 1).to_string();
        let r = Request {
            method: "POST",
            path: create,
            headers: headers(&s),
            content_length: Some(&too_long),
            content_type: Some(UPLOAD_MEDIA_TYPE),
            ..Request::default()
        };
        let (d, l, b) = run(&mut reg(), &open, &r);
        assert_eq!((refused(&d), l, b), (ErrorCode::BadRequest, 0, 0));
        // Oversized with no declared length: the reader's bound.
        let r = Request { content_length: None, ..r };
        let mut big = Counting { registered: true, body, ..Counting::default() };
        let (d, l, b) = run(&mut big, &open, &r);
        assert_eq!((refused(&d), l, b), (ErrorCode::BadRequest, 1, 1));
        // A creation of another media type: no I/O.
        let r = Request { content_type: Some("application/json"), ..r };
        let (d, l, b) = run(&mut reg(), &open, &r);
        assert_eq!((refused(&d), l, b), (ErrorCode::UnsupportedMediaType, 0, 0));
    }

    #[test]
    fn header_only_mismatches_are_refused_before_the_lookup() {
        let alice = pair("//Alice");
        let bob = keys::ss58_from_public(pair("//Bob").public_key_bytes(), SS58_FORMAT);
        let own = keys::ss58_from_public(alice.public_key_bytes(), SS58_FORMAT);
        let sid = "ab".repeat(32);
        let open = Access::Open;
        // A bodiless route signed over a non-empty digest.
        let p = format!("/admission/v1/sessions/{sid}/complete");
        let s = signed(&alice, "POST", &p, b"x", 100);
        let r = Request { method: "POST", path: &p, headers: headers(&s), ..Request::default() };
        let (d, l, b) = run(&mut Counting { registered: true, ..Counting::default() }, &open, &r);
        assert_eq!((refused(&d), l, b), (ErrorCode::ContentDigestMismatch, 0, 0));
        // A hotkey read for another hotkey.
        let p = format!("/admission/v1/cohorts/7/hotkeys/{bob}");
        let s = signed(&alice, "GET", &p, &[], 100);
        let r = Request { method: "GET", path: &p, headers: headers(&s), ..Request::default() };
        let (d, l, b) = run(&mut Counting { registered: true, ..Counting::default() }, &open, &r);
        assert_eq!((refused(&d), l, b), (ErrorCode::Forbidden, 0, 0));
        // The signer's own hotkey read is forwarded.
        let p = format!("/admission/v1/cohorts/7/hotkeys/{own}");
        let s = signed(&alice, "GET", &p, &[], 100);
        let r = Request { method: "GET", path: &p, headers: headers(&s), ..Request::default() };
        let (d, _, _) = run(&mut Counting { registered: true, ..Counting::default() }, &open, &r);
        assert!(matches!(d, Decision::Forward { op: op::HOTKEY, .. }), "{d:?}");
    }

    #[test]
    fn a_body_the_platform_could_not_read_is_retried_and_a_part_is_never_read() {
        let alice = pair("//Alice");
        let sid = "ab".repeat(32);
        let open = Access::Open;
        let p = format!("/admission/v1/sessions/{sid}");
        let s = signed(&alice, "GET", &p, &[], 100);
        let r = Request { method: "GET", path: &p, headers: headers(&s), ..Request::default() };
        let mut io = Counting { registered: true, body_fails: Some(BodyFailure::Unavailable), ..Counting::default() };
        let (d, _, _) = run(&mut io, &open, &r);
        let Decision::Refuse(e) = d else { panic!("{d:?}") };
        assert!(e.code.retryable() && e.retry_after.is_some());
        // A part: the lookup, then the part-begin message; its body is R2's.
        let p = format!("/admission/v1/sessions/{sid}/parts/3");
        let s = signed(&alice, "PUT", &p, b"part bytes", 100);
        let r = Request { method: "PUT", path: &p, headers: headers(&s), content_length: Some("10"), ..Request::default() };
        let (d, l, b) = run(&mut Counting { registered: true, ..Counting::default() }, &open, &r);
        let Decision::Part { forward, len } = d else { panic!("{d:?}") };
        assert_eq!((len, forward.part, forward.content_length, l, b), (10, 3, Some(10), 1, 0));
        // The cohort read: no I/O here (its storage read is the Worker's).
        let r = Request { method: "GET", path: "/admission/v1/cohort", ..Request::default() };
        let (d, l, b) = run(&mut Counting::default(), &open, &r);
        assert_eq!((d, l, b), (Decision::Cohort, 0, 0));
    }
}
