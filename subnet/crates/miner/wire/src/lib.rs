//! Wire helpers for the admission API contract `slop_ninja.admission/v1`
//! (`docs/testnet/admission-api.md`).
//!
//! Shared by the native miner client and the Workers coordinator.
//!
//! **Arena-safe items** allocate nothing, keep no global state and borrow
//! their inputs, so Workers code can use them inside a bounded memory arena:
//!
//! - [`request_message`]: the 32-byte message a hotkey signs for a request;
//! - [`Route::parse`]: the zero-copy route parser for a method and path;
//! - the `path::write_*` functions, which write a route into a
//!   [`core::fmt::Write`] sink;
//! - [`PartPlan`], including [`PartPlan::missing`], an iterator;
//! - [`ErrorCode`] and [`SessionState`]: the stable codes and states;
//! - [`parse_hex32`], [`parse_signature`] and [`parse_valid_until`]: borrowed
//!   parsers for the header values;
//! - [`response_limit`]: the bound on a response body.
//!
//! **Allocating items**, for the native client and the test mock only:
//!
//! - the `String`-returning `path` builders;
//! - [`session_id`], which encodes the protocol record;
//! - the [`json`] module (feature `json`): owned serde shapes of the
//!   responses and errors, and the authorization pointer;
//! - the [`receipts`] module (feature `json`): the private receipts-bucket
//!   contract the coordinator and the publisher share (pointer checks and
//!   bounds, the receipt-retirement marker).
//!
//! Every deployment bound is a parameter the coordinator announces, never a
//! constant here.

use sha2::{Digest as _, Sha256};
use slop_ninja_protocol::submission::SubmissionUpload;
use slop_ninja_protocol::{AccountId, ChainBinding, Digest, Record};

/// The contract identifier the coordinator reports.
pub const CONTRACT: &str = "slop_ninja.admission/v1";
/// Domain of the signed request message.
pub const REQUEST_DOMAIN: &str = "slop_ninja.admission/request/v1";
/// Media type of the session-creation body: canonical `SubmissionUpload` bytes.
pub const UPLOAD_MEDIA_TYPE: &str = "application/vnd.slop-ninja.submission-upload.v1";
/// The first `selection` rule: earliest commitment position, one per hotkey.
pub const SELECTION_EARLIEST_COMMITMENT_V1: &str = "earliest_commitment/v1";

/// Request header names.
pub mod header {
    pub const HOTKEY: &str = "x-sn-hotkey";
    pub const VALID_UNTIL: &str = "x-sn-valid-until";
    pub const CONTENT_SHA256: &str = "x-sn-content-sha256";
    pub const SIGNATURE: &str = "x-sn-signature";
    /// Optional on session creation and reopening, outside the signature:
    /// the block of the miner's own confirmed commitment (decimal). The
    /// coordinator treats it as an untrusted hint (a not-yet commitment
    /// answer waits until its reader has polled at or after this block).
    pub const COMMITMENT_BLOCK: &str = "x-sn-commitment-block";
}

/// Route paths. Ids and addresses are path segments; no route uses a query.
pub mod path {
    use core::fmt::{self, Write};

    pub const COHORT: &str = "/admission/v1/cohort";
    pub const SESSIONS: &str = "/admission/v1/sessions";

    /// `/admission/v1/sessions/{session}`.
    pub fn write_session(out: &mut impl Write, session: &str) -> fmt::Result {
        write!(out, "{SESSIONS}/{session}")
    }
    /// `/admission/v1/sessions/{session}/parts/{part}`.
    pub fn write_part(out: &mut impl Write, session: &str, part: u32) -> fmt::Result {
        write!(out, "{SESSIONS}/{session}/parts/{part}")
    }
    /// `/admission/v1/sessions/{session}/complete`.
    pub fn write_complete(out: &mut impl Write, session: &str) -> fmt::Result {
        write!(out, "{SESSIONS}/{session}/complete")
    }
    /// `/admission/v1/sessions/{session}/attempts/{attempt}`: `DELETE` aborts
    /// that attempt, `POST` reopens an aborted session as that attempt.
    pub fn write_attempt(out: &mut impl Write, session: &str, attempt: u32) -> fmt::Result {
        write!(out, "{SESSIONS}/{session}/attempts/{attempt}")
    }
    /// `/admission/v1/cohorts/{cohort}/hotkeys/{hotkey}`.
    pub fn write_hotkey(out: &mut impl Write, cohort: u64, hotkey_ss58: &str) -> fmt::Result {
        write!(out, "/admission/v1/cohorts/{cohort}/hotkeys/{hotkey_ss58}")
    }

    fn owned(write: impl FnOnce(&mut String) -> fmt::Result) -> String {
        let mut out = String::new();
        write(&mut out).expect("writing to a String cannot fail");
        out
    }

    /// Allocating form of [`write_session`].
    pub fn session(session: &str) -> String {
        owned(|out| write_session(out, session))
    }
    /// Allocating form of [`write_part`].
    pub fn part(session: &str, part: u32) -> String {
        owned(|out| write_part(out, session, part))
    }
    /// Allocating form of [`write_complete`].
    pub fn complete(session: &str) -> String {
        owned(|out| write_complete(out, session))
    }
    /// Allocating form of [`write_attempt`].
    pub fn attempt(session: &str, attempt: u32) -> String {
        owned(|out| write_attempt(out, session, attempt))
    }
    /// Allocating form of [`write_hotkey`].
    pub fn hotkey(cohort: u64, hotkey_ss58: &str) -> String {
        owned(|out| write_hotkey(out, cohort, hotkey_ss58))
    }
}

/// One request of the contract, parsed from its method and path without
/// copying. Session ids are 64 lowercase hex digits and numbers canonical
/// decimals; the hotkey segment is returned as sent for the caller to parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route<'a> {
    /// `GET /admission/v1/cohort` (unsigned).
    ReadCohort,
    /// `POST /admission/v1/sessions`.
    CreateSession,
    /// `GET /admission/v1/sessions/{session}`.
    ReadSession { session: &'a str },
    /// `PUT /admission/v1/sessions/{session}/parts/{part}`.
    PutPart { session: &'a str, part: u32 },
    /// `POST /admission/v1/sessions/{session}/complete`.
    Complete { session: &'a str },
    /// `DELETE /admission/v1/sessions/{session}/attempts/{attempt}`.
    Abort { session: &'a str, attempt: u32 },
    /// `POST /admission/v1/sessions/{session}/attempts/{attempt}`.
    Reopen { session: &'a str, attempt: u32 },
    /// `GET /admission/v1/cohorts/{cohort}/hotkeys/{hotkey}`.
    ReadHotkey { cohort: u64, hotkey: &'a str },
}

impl<'a> Route<'a> {
    /// The route for `method` and `path`, or `None` when the contract has no
    /// such request.
    pub fn parse(method: &str, path: &'a str) -> Option<Self> {
        let rest = path.strip_prefix("/admission/v1/")?;
        let mut segments = rest.split('/');
        let route = match (method, segments.next()?) {
            ("GET", "cohort") => Self::ReadCohort,
            (_, "sessions") => match (method, segments.next(), segments.next()) {
                ("POST", None, _) => Self::CreateSession,
                (_, Some(session), next) if is_session_id(session) => {
                    match (method, next, segments.next()) {
                        ("GET", None, _) => Self::ReadSession { session },
                        ("POST", Some("complete"), None) => Self::Complete { session },
                        ("PUT", Some("parts"), Some(n)) => Self::PutPart {
                            session,
                            part: u32::try_from(parse_decimal(n)?).ok()?,
                        },
                        (_, Some("attempts"), Some(n)) => {
                            let attempt = u32::try_from(parse_decimal(n)?).ok()?;
                            match method {
                                "DELETE" => Self::Abort { session, attempt },
                                "POST" => Self::Reopen { session, attempt },
                                _ => return None,
                            }
                        }
                        _ => return None,
                    }
                }
                _ => return None,
            },
            ("GET", "cohorts") => {
                let cohort = parse_decimal(segments.next()?)?;
                if segments.next()? != "hotkeys" {
                    return None;
                }
                let hotkey = segments.next()?;
                if hotkey.is_empty() {
                    return None;
                }
                Self::ReadHotkey { cohort, hotkey }
            }
            _ => return None,
        };
        if segments.next().is_some() {
            return None;
        }
        Some(route)
    }

    /// Whether the request must carry a hotkey signature.
    pub fn signed(&self) -> bool {
        !matches!(self, Self::ReadCohort)
    }
}

/// Whether `text` is a session id: 64 lowercase hex digits.
pub fn is_session_id(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn parse_decimal(text: &str) -> Option<u64> {
    parse_valid_until(text)
}

/// Everything a request signature binds.
#[derive(Clone, Copy, Debug)]
pub struct RequestScope<'a> {
    /// The coordinator's chain and subnet.
    pub chain: &'a ChainBinding,
    /// Public key of the signing hotkey.
    pub hotkey: &'a AccountId,
    /// Finalized block after which the coordinator refuses the request.
    pub valid_until: u64,
    /// HTTP method, for example `PUT`.
    pub method: &'a str,
    /// Request path exactly as sent, without scheme, host or query.
    pub path: &'a str,
    /// SHA-256 of the request body; of the empty string when there is none.
    pub content_sha256: &'a Digest,
}

/// SHA-256 of the empty string, the content digest of a bodiless request.
pub fn empty_sha256() -> Digest {
    Digest(Sha256::digest([]).into())
}

/// The 32-byte message the hotkey signs:
/// `domain_hash(REQUEST_DOMAIN, body)` with
/// `body = genesis || u16(netuid) || u8(mechanism) || hotkey || u64(valid_until)
///  || str(method) || str(path) || content_sha256`, integers little-endian and
/// strings `u32_le(len) || bytes`, as the protocol codec encodes them.
pub fn request_message(scope: &RequestScope<'_>) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(len32(REQUEST_DOMAIN.len()));
    h.update(REQUEST_DOMAIN.as_bytes());
    h.update(scope.chain.genesis.0);
    h.update(scope.chain.netuid.to_le_bytes());
    h.update([scope.chain.mechanism]);
    h.update(scope.hotkey.0);
    h.update(scope.valid_until.to_le_bytes());
    h.update(len32(scope.method.len()));
    h.update(scope.method.as_bytes());
    h.update(len32(scope.path.len()));
    h.update(scope.path.as_bytes());
    h.update(scope.content_sha256.0);
    h.finalize().into()
}

fn len32(len: usize) -> [u8; 4] {
    // Methods and paths are short; a length past u32 cannot be a request.
    u32::try_from(len).unwrap_or(u32::MAX).to_le_bytes()
}

/// The session id of an upload envelope: its protocol record digest.
pub fn session_id(upload: &SubmissionUpload) -> slop_ninja_protocol::Result<Digest> {
    upload.digest()
}

/// Why a part plan cannot be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    EmptyObject,
    ZeroPartSize,
    /// The object needs more parts than the coordinator allows.
    TooManyParts {
        needed: u64,
        max_parts: u32,
    },
}

impl core::fmt::Display for PlanError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EmptyObject => f.write_str("the object is empty"),
            Self::ZeroPartSize => f.write_str("the part size is zero"),
            Self::TooManyParts { needed, max_parts } => {
                write!(
                    f,
                    "the object needs {needed} parts; at most {max_parts} are allowed"
                )
            }
        }
    }
}

impl std::error::Error for PlanError {}

/// Part arithmetic: part `n` (1-based) covers bytes
/// `[(n-1)·part_size, min(n·part_size, len))`, so every part but the last has
/// exactly `part_size` bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartPlan {
    len: u64,
    part_size: u64,
    count: u32,
}

impl PartPlan {
    pub fn new(len: u64, part_size: u64, max_parts: u32) -> Result<Self, PlanError> {
        if len == 0 {
            return Err(PlanError::EmptyObject);
        }
        if part_size == 0 {
            return Err(PlanError::ZeroPartSize);
        }
        let needed = len.div_ceil(part_size);
        let count = u32::try_from(needed)
            .ok()
            .filter(|&count| count <= max_parts)
            .ok_or(PlanError::TooManyParts { needed, max_parts })?;
        Ok(Self {
            len,
            part_size,
            count,
        })
    }

    pub fn len(&self) -> u64 {
        self.len
    }
    /// Always false: a plan covers a nonempty object.
    pub fn is_empty(&self) -> bool {
        false
    }
    pub fn part_size(&self) -> u64 {
        self.part_size
    }
    pub fn count(&self) -> u32 {
        self.count
    }

    /// Byte offset and exact length of part `n`, or `None` outside `1..=count`.
    pub fn range(&self, part: u32) -> Option<(u64, u64)> {
        if part == 0 || part > self.count {
            return None;
        }
        let offset = u64::from(part - 1) * self.part_size;
        Some((offset, (self.len - offset).min(self.part_size)))
    }

    /// Part numbers in `1..=count` for which `recorded` is false, in order,
    /// without allocating.
    pub fn missing<F: Fn(u32) -> bool>(&self, recorded: F) -> impl Iterator<Item = u32> {
        (1..=self.count).filter(move |part| !recorded(*part))
    }
}

/// Base allowance of a response body: every field other than the part list.
pub const RESPONSE_BASE_BYTES: usize = 64 * 1024;
/// Allowance per part record. The largest compact record,
/// `{"part":4294967295,"len":18446744073709551615,"sha256":"<64 hex>"}`, has
/// 122 bytes; the allowance leaves room for formatting.
pub const RESPONSE_PART_BYTES: usize = 256;

/// The largest response body a client accepts: [`RESPONSE_BASE_BYTES`] plus
/// [`RESPONSE_PART_BYTES`] for each of the cohort's `max_parts` parts, the
/// most part records (or missing part numbers) any response lists. Before the
/// cohort parameters are known, `max_parts` is `None` and only the base
/// applies.
pub fn response_limit(max_parts: Option<u32>) -> usize {
    let parts = max_parts.map_or(0, |n| n as usize);
    RESPONSE_BASE_BYTES.saturating_add(parts.saturating_mul(RESPONSE_PART_BYTES))
}

macro_rules! codes {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $text:literal),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),* }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),* }
            }
            /// Parses a code; `None` for one this build does not know.
            pub fn parse(text: &str) -> Option<Self> {
                match text { $($text => Some(Self::$variant),)* _ => None }
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

codes! {
    /// Error and refusal codes of `slop_ninja.admission/v1`.
    ErrorCode {
        BadRequest = "bad_request",
        UnsupportedMediaType = "unsupported_media_type",
        BadSignature = "bad_signature",
        RequestExpired = "request_expired",
        RequestValidityTooLong = "request_validity_too_long",
        ContentDigestMismatch = "content_digest_mismatch",
        Forbidden = "forbidden",
        NotFound = "not_found",
        NotAdmitting = "not_admitting",
        AfterCutoff = "after_cutoff",
        WrongChain = "wrong_chain",
        WrongCohort = "wrong_cohort",
        HotkeyMismatch = "hotkey_mismatch",
        NotRegistered = "not_registered",
        RegistrationMismatch = "registration_mismatch",
        CommitmentNotFound = "commitment_not_found",
        CommitmentUnresolvable = "commitment_unresolvable",
        CommitmentBeforeOpen = "commitment_before_open",
        CommitmentMismatch = "commitment_mismatch",
        ObjectTooLarge = "object_too_large",
        SecondArtifact = "second_artifact",
        PartOutOfRange = "part_out_of_range",
        PartLengthMismatch = "part_length_mismatch",
        PartDigestMismatch = "part_digest_mismatch",
        SessionNotUploading = "session_not_uploading",
        StaleAttempt = "stale_attempt",
        PartsMissing = "parts_missing",
        RateLimited = "rate_limited",
        Paused = "paused",
        Unavailable = "unavailable",
        Internal = "internal",
        ObjectMismatch = "object_mismatch",
        ArtifactInvalid = "artifact_invalid",
        ContractMismatch = "contract_mismatch",
        ContentMismatch = "content_mismatch",
        CheckFailed = "check_failed",
        CommitmentOverwritten = "commitment_overwritten",
        IncompleteAtCutoff = "incomplete_at_cutoff",
    }
}

impl ErrorCode {
    /// HTTP status of a request error; `None` for a code that appears only as
    /// a session refusal.
    pub fn http_status(self) -> Option<u16> {
        use ErrorCode::*;
        Some(match self {
            BadRequest | RequestValidityTooLong | ContentDigestMismatch => 400,
            BadSignature | RequestExpired => 401,
            Forbidden => 403,
            NotFound => 404,
            NotAdmitting | AfterCutoff | CommitmentNotFound | SecondArtifact
            | SessionNotUploading | StaleAttempt | PartsMissing => 409,
            ObjectTooLarge => 413,
            UnsupportedMediaType => 415,
            WrongChain | WrongCohort | HotkeyMismatch | NotRegistered | RegistrationMismatch
            | CommitmentUnresolvable | CommitmentBeforeOpen | CommitmentMismatch | PartOutOfRange | PartLengthMismatch
            | PartDigestMismatch => 422,
            RateLimited => 429,
            Internal => 500,
            Paused | Unavailable => 503,
            ObjectMismatch
            | ArtifactInvalid
            | ContractMismatch
            | ContentMismatch
            | CheckFailed
            | CommitmentOverwritten
            | IncompleteAtCutoff => return None,
        })
    }

    /// Whether a client may retry the request (re-signing or after a wait).
    pub fn retryable(self) -> bool {
        use ErrorCode::*;
        matches!(
            self,
            RequestExpired
                | NotAdmitting
                | CommitmentNotFound
                | PartDigestMismatch
                | PartsMissing
                | RateLimited
                | Paused
                | Unavailable
                | Internal
        )
    }
}

codes! {
    /// Upload session states.
    SessionState {
        Uploading = "uploading",
        Verifying = "verifying",
        Verified = "verified",
        Refused = "refused",
        Admitted = "admitted",
        Overflow = "overflow",
        Superseded = "superseded",
        Aborted = "aborted",
    }
}

impl SessionState {
    /// Whether the session can still change without the miner acting.
    pub fn pending(self) -> bool {
        matches!(self, Self::Verifying | Self::Verified)
    }
    /// Whether the session is over for this cohort.
    pub fn ended(self) -> bool {
        matches!(
            self,
            Self::Refused | Self::Admitted | Self::Overflow | Self::Superseded | Self::Aborted
        )
    }
}

/// Parses 32 bytes of hex, with or without a `0x` prefix.
pub fn parse_hex32(text: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    hex::decode_to_slice(text.strip_prefix("0x").unwrap_or(text), &mut out).ok()?;
    Some(out)
}

/// Parses an `x-sn-signature` value.
pub fn parse_signature(text: &str) -> Option<[u8; 64]> {
    let mut out = [0u8; 64];
    hex::decode_to_slice(text, &mut out).ok()?;
    Some(out)
}

/// Parses an `x-sn-valid-until` value: a canonical decimal block number.
pub fn parse_valid_until(text: &str) -> Option<u64> {
    let canonical = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    if !canonical {
        return None;
    }
    text.parse().ok()
}

#[cfg(feature = "json")]
pub mod json;
#[cfg(feature = "json")]
pub mod receipts;

#[cfg(test)]
mod tests;
