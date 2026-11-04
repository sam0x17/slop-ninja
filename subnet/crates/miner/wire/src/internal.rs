//! The internal interface between the two admission Workers (feature
//! `json`): the key-free upload Worker `slop-ninja-admission-testnet`, which
//! alone serves the public `/admission/v1/` routes, and the key-holding eval
//! Worker, which keeps the cohort authority's state and verdicts.
//!
//! - **Eval Worker to the upload bucket** (the cohort snapshot): the cohort
//!   authority writes the contract's unsigned cohort response, with the time
//!   it was written, to [`COHORT_SNAPSHOT_KEY`] in the upload bucket at every
//!   heartbeat. The upload Worker serves `GET /admission/v1/cohort` from that
//!   object (and its one-block edge copy), so no unsigned request reaches the
//!   eval Worker.
//! - **Upload Worker to eval Worker** (`UPLOAD → EVAL`, the upload Worker's
//!   service binding): a request it has decoded and checked (signature, body
//!   digest, registration on the subnet, the spec-derived size bound) is
//!   forwarded as one bounded JSON message, [`Forward`], to
//!   `EVAL_INTERFACE_ORIGIN` + [`EVAL_PATH_PREFIX`] + the operation. The eval
//!   Worker answers with the cohort authority's [`Reply`] (or, for a part,
//!   the [`PartTarget`] the body streams to).
//! - **Eval Worker to upload Worker** (`EVAL → UPLOAD`, the eval Worker's
//!   service binding): the cohort authority's framing step asks the upload
//!   Worker to frame one uploaded object ([`FrameRequest`] to
//!   `UPLOAD_INTERFACE_ORIGIN` + [`UPLOAD_FRAME_PATH`]); the answer is
//!   `slop_ninja_artifact_check::admission::FrameOutcome`. The eval Worker
//!   never decodes an uploaded object.
//!
//! Neither interface has a public route. Each Worker serves its interface
//! only for requests addressed to its internal origin (a host name no
//! public route of either Worker can carry: the upload Worker's only public
//! ingress is its custom domain, the eval Worker's its `workers.dev` name)
//! that also carry the shared interface token ([`TOKEN_HEADER`], a Worker
//! secret of both Workers, compared in constant time), so another Worker
//! with a binding to the eval Worker (the publisher's status reads) cannot
//! reach the admission interface either. Every other request to an
//! interface path answers 404.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The eval Worker's internal admission origin (service binding only).
pub const EVAL_INTERFACE_ORIGIN: &str = "https://eval-admission.internal";
/// Host of [`EVAL_INTERFACE_ORIGIN`].
pub const EVAL_INTERFACE_HOST: &str = "eval-admission.internal";
/// The upload Worker's internal origin (service binding only).
pub const UPLOAD_INTERFACE_ORIGIN: &str = "https://admission-upload.internal";
/// Host of [`UPLOAD_INTERFACE_ORIGIN`].
pub const UPLOAD_INTERFACE_HOST: &str = "admission-upload.internal";
/// The header carrying the shared interface token.
pub const TOKEN_HEADER: &str = "x-sn-interface-token";
/// Path prefix of the eval Worker's interface: `<prefix><op>`.
pub const EVAL_PATH_PREFIX: &str = "/internal/admission/v1/";
/// The upload Worker's framing route.
pub const UPLOAD_FRAME_PATH: &str = "/internal/upload/v1/frame";

/// The eval Worker's interface operations: the cohort authority's
/// `/admission/<op>` routes, one per public route step.
pub mod op {
    /// The cohort read's plan entry. Not an interface operation: the upload
    /// Worker serves the cohort read from the snapshot
    /// ([`super::COHORT_SNAPSHOT_KEY`]), so it is not in [`ALL`].
    pub const COHORT: &str = "cohort";
    pub const CREATE: &str = "create";
    pub const READ: &str = "read";
    /// Before a part's body streams: the target it streams to.
    pub const PART_BEGIN: &str = "part-begin";
    /// After it streamed: the part's R2 tag.
    pub const PART_RECORD: &str = "part-record";
    pub const COMPLETE: &str = "complete";
    pub const ABORT: &str = "abort";
    pub const REOPEN: &str = "reopen";
    pub const HOTKEY: &str = "hotkey";
    /// Every operation the interface serves.
    pub const ALL: &[&str] = &[CREATE, READ, PART_BEGIN, PART_RECORD, COMPLETE, ABORT, REOPEN, HOTKEY];
}

/// A signed request as the upload Worker forwards it: method and path
/// exactly as sent, the four signature headers, and for a body read whole,
/// its SHA-256 (a part's digest is the claimed one; the part streams to R2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signed {
    pub method: String,
    pub path: String,
    pub hotkey: Option<String>,
    pub valid_until: Option<String>,
    pub content_sha256: Option<String>,
    pub signature: Option<String>,
    /// SHA-256 of a body the upload Worker read whole (hex); `None` for a part.
    #[serde(default)]
    pub body_sha256: Option<String>,
    /// A small body, hex (session creation only).
    #[serde(default)]
    pub body: Option<String>,
    /// The block of the miner's own confirmed commitment, from
    /// `x-sn-commitment-block` at session creation or reopening. Unsigned
    /// and untrusted: it only makes a not-yet commitment answer for this
    /// hotkey wait until the reader has polled at or after that block; a
    /// false one delays only this hotkey's own answer, never past the
    /// window's close.
    #[serde(default)]
    pub commitment_block: Option<u64>,
}

/// One forwarded request: the signed fields and the route's parameters.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forward {
    #[serde(default)]
    pub signed: Signed,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub attempt: Option<u32>,
    #[serde(default)]
    pub part: u32,
    /// A part's declared `Content-Length`.
    #[serde(default)]
    pub content_length: Option<u64>,
    #[serde(default)]
    pub cohort: u64,
    #[serde(default)]
    pub hotkey: String,
    /// `part-record`: the target the part streamed to and its R2 tag.
    #[serde(default)]
    pub target: Option<PartTarget>,
    #[serde(default)]
    pub etag: String,
}

/// The room of a [`Forward`]'s JSON beside a creation body: every field
/// at its format bound (method and path as [`crate::Route::parse`] accepts
/// them, the four signature headers as the gate's `verify` parses them, a
/// session id, the route's integers, an SS58 hotkey, an object key and a
/// SHA-256), under 2 KiB (tested), plus R2's multipart upload id and part
/// tag, which R2 documents no length for: they get the rest. That remainder
/// is an allowance, not a derived bound (version
/// [`INTERFACE_BOUNDS_VERSION`]): the live drill records the longest upload
/// id and tag R2 returns, and a later version replaces the allowance with
/// that measured bound. A forward over the bound is refused, never cut.
pub const FORWARD_FIELDS_BYTES: usize = 8 * 1024;

/// The version of this module's message bounds ([`FORWARD_FIELDS_BYTES`],
/// [`FRAME_REQUEST_BYTES`]); a change to any of them is a new version.
pub const INTERFACE_BOUNDS_VERSION: &str = "admission-interface-bounds-2026-11-03-r2";

/// The bound of one forwarded message: [`FORWARD_FIELDS_BYTES`] and a
/// session-creation body as hex ([`crate::UPLOAD_RECORD_BYTES`]).
pub const FORWARD_BYTES: usize = FORWARD_FIELDS_BYTES + 2 * crate::UPLOAD_RECORD_BYTES;

/// Where one part's body streams: the attempt's object key and multipart
/// upload, the part number, its exact length and declared digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartTarget {
    pub key: String,
    pub upload_id: String,
    pub attempt: u32,
    pub part: u32,
    pub len: u64,
    pub sha256: String,
}

/// A reply the upload Worker sends as is: HTTP status and JSON body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
    #[serde(default)]
    pub retry_after: Option<u64>,
}

impl Reply {
    pub fn new(status: u16, body: Value, retry_after: Option<u64>) -> Self {
        Self { status, body, retry_after }
    }
}

/// The eval Worker's answer to `part-begin`: the target, or the reply that
/// refuses the part.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartBegin {
    #[serde(default)]
    pub target: Option<PartTarget>,
    #[serde(default)]
    pub reply: Option<Reply>,
}

/// The framing step's request to the upload Worker: the object's key, the
/// length its envelope declares, and the cohort's artifact spec id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRequest {
    pub key: String,
    pub len: u64,
    pub spec: String,
}

/// The longest object key the framing route reads:
/// `admission/<label>/<session>/a<attempt>.object` with the protocol's
/// longest cohort label, a 64-hex session id and a `u32` attempt.
pub const OBJECT_KEY_MAX_BYTES: usize =
    "admission/".len() + slop_ninja_protocol::MAX_LABEL_LEN + 1 + 64 + "/a".len() + 10 + ".object".len();
/// The longest artifact spec id: `slop_ninja_a_artifact_spec_v` and a
/// version number of at most three digits (the upload Worker's tests check
/// every spec it defines against it).
pub const SPEC_ID_MAX_BYTES: usize = "slop_ninja_a_artifact_spec_v".len() + 3;

/// The bound of a [`FrameRequest`] body, from its format: the field names
/// and punctuation (`{"key":"`, `","len":`, `,"spec":"`, `"}`), the longest
/// object key, a `u64` and the longest spec id (no field needs escaping:
/// keys and spec ids are ASCII without quotes or backslashes). Tested
/// exact.
pub const FRAME_REQUEST_BYTES: usize =
    r#"{"key":""#.len() + OBJECT_KEY_MAX_BYTES + r#"","len":"#.len() + 20 + r#","spec":""#.len() + SPEC_ID_MAX_BYTES + r#""}"#.len();

/// The framing key of the link probe: a framing request for it, addressed
/// and authenticated as the eval Worker's, is answered `{"probe": "ok"}`
/// without reading anything, so the eval Worker can check the service
/// binding and the shared token without an upload ([`is_object_key`] never
/// admits it).
pub const FRAME_PROBE_KEY: &str = "probe";

/// The header of a framing answer that failed for the one reason that is
/// the platform's, the object read from R2 ([`FRAME_OBJECT_READ`], status
/// 503). The eval Worker counts every other framing failure against the
/// session's attempts.
pub const FRAME_FAILURE_HEADER: &str = "x-sn-frame-failure";
/// [`FRAME_FAILURE_HEADER`]'s value for a failed object read.
pub const FRAME_OBJECT_READ: &str = "object-read";
/// [`FRAME_FAILURE_HEADER`]'s value for any other failure of the upload
/// Worker's own platform (its arena refused the framing scope, the request
/// body could not be read): retried without counting, as an object read.
/// The framing route is reached only through the eval Worker's binding with
/// the token, so no untrusted request can produce it.
pub const FRAME_PLATFORM: &str = "platform";

/// The upload bucket key of the cohort snapshot ([`CohortSnapshot`]). Not
/// an uploaded object's key ([`is_object_key`] never admits it).
pub const COHORT_SNAPSHOT_KEY: &str = "cohort/v1/current.json";
/// The schema of [`CohortSnapshot`].
pub const COHORT_SNAPSHOT_SCHEMA: &str = "slop_ninja.admission.cohort_snapshot/v1";
/// How long a snapshot answers as written: the coordinator's heartbeat
/// freshness (`engine::HEARTBEAT_FRESH_MS`, 10 minutes; a coordinator test
/// checks they are equal). An older snapshot answers `paused: true`, as the
/// cohort authority itself answers once its heartbeat is that old.
pub const COHORT_SNAPSHOT_FRESH_MS: u64 = 600_000;

/// The cohort response as the cohort authority wrote it, and when (its
/// clock, milliseconds).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortSnapshot {
    pub schema: String,
    pub written_ms: u64,
    pub cohort: crate::json::CohortInfo,
}

/// The snapshot's envelope around the cohort response, from its format:
/// `{"schema":"<schema>","written_ms":<u64>,"cohort":` and the closing
/// brace (tested exact).
pub const COHORT_SNAPSHOT_ENVELOPE_BYTES: usize = r#"{"schema":""#.len()
    + COHORT_SNAPSHOT_SCHEMA.len()
    + r#"","written_ms":"#.len()
    + 20
    + r#","cohort":"#.len()
    + "}".len();

/// The bound the upload Worker reads a snapshot within: the cohort response
/// at the client's limit for it ([`crate::response_limit`] before the
/// cohort's parameters are known) and the envelope.
pub fn cohort_snapshot_limit() -> usize {
    crate::response_limit(None) + COHORT_SNAPSHOT_ENVELOPE_BYTES
}

impl CohortSnapshot {
    /// The cohort response to serve at `now_ms`: as written, but `paused`
    /// once the snapshot is older than [`COHORT_SNAPSHOT_FRESH_MS`] (the
    /// authority has stopped writing it). A clock behind the write time
    /// serves the snapshot as written.
    pub fn serve(mut self, now_ms: u64) -> crate::json::CohortInfo {
        if now_ms.saturating_sub(self.written_ms) >= COHORT_SNAPSHOT_FRESH_MS {
            self.cohort.paused = true;
        }
        self.cohort
    }
}

/// Whether `key` names an uploaded object: the only keys the framing route
/// reads (`admission/<label>/<session>/a<attempt>.object`, written by the
/// eval Worker's session records).
pub fn is_object_key(key: &str) -> bool {
    key.starts_with("admission/")
        && key.ends_with(".object")
        && !key.contains("..")
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.'))
}

/// The shortest interface token accepted (as the coordinator's bearer
/// tokens): 32 bytes.
pub const MIN_TOKEN_BYTES: usize = 32;

/// Whether `presented` is the configured interface token. The comparison
/// takes the same time for every presented value of the configured length;
/// an unset or short configured token matches nothing, so a deployment
/// without the secret serves no interface request.
pub fn token_matches(configured: Option<&str>, presented: Option<&str>) -> bool {
    let (Some(want), Some(got)) = (configured.map(str::trim), presented.map(str::trim)) else {
        return false;
    };
    if want.len() < MIN_TOKEN_BYTES || want.len() != got.len() {
        return false;
    }
    want.bytes().zip(got.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_match_only_the_configured_value() {
        let t = "0123456789abcdef0123456789abcdef";
        assert!(token_matches(Some(t), Some(t)));
        assert!(!token_matches(Some(t), Some("0123456789abcdef0123456789abcdeX")));
        assert!(!token_matches(Some(t), None));
        assert!(!token_matches(None, Some(t)));
        assert!(!token_matches(Some("short"), Some("short")));
        assert!(!token_matches(Some(t), Some(&t[1..])));
    }

    #[test]
    fn a_forward_at_its_format_bounds_fits_with_room_for_r2s_values() {
        let sid = "ab".repeat(32);
        let path = format!("/admission/v1/sessions/{sid}/attempts/{}", u32::MAX);
        let hotkey = "5".repeat(crate::SS58_ACCOUNT_MAX_CHARS);
        let signed = Signed {
            method: "DELETE".into(),
            path,
            hotkey: Some(hotkey.clone()),
            valid_until: Some(u64::MAX.to_string()),
            content_sha256: Some(format!("0x{}", "f".repeat(64))),
            signature: Some(format!("0x{}", "f".repeat(128))),
            body_sha256: Some("f".repeat(64)),
            body: Some("ff".repeat(crate::UPLOAD_RECORD_BYTES)),
            commitment_block: Some(u64::MAX),
        };
        let key = format!("admission/{}/{sid}/a{}.object", "l".repeat(64), u32::MAX);
        let mut f = Forward {
            signed,
            session: sid,
            attempt: Some(u32::MAX),
            part: u32::MAX,
            content_length: Some(u64::MAX),
            cohort: u64::MAX,
            hotkey,
            target: Some(PartTarget {
                key,
                upload_id: String::new(),
                attempt: u32::MAX,
                part: u32::MAX,
                len: u64::MAX,
                sha256: "f".repeat(64),
            }),
            etag: String::new(),
        };
        let fields = serde_json::to_string(&f).unwrap().len() - 2 * crate::UPLOAD_RECORD_BYTES;
        assert!(fields < 2 * 1024, "{fields}");
        // R2's upload id and part tag share the remaining room.
        let room = (FORWARD_FIELDS_BYTES - fields) / 2;
        f.target.as_mut().unwrap().upload_id = "u".repeat(room);
        f.etag = "e".repeat(room);
        assert!(serde_json::to_string(&f).unwrap().len() <= FORWARD_BYTES);
        assert!(!is_object_key(FRAME_PROBE_KEY));
    }

    #[test]
    fn a_frame_request_at_its_format_bounds_is_exactly_its_bound() {
        let key = format!("admission/{}/{}/a{}.object", "l".repeat(slop_ninja_protocol::MAX_LABEL_LEN), "ab".repeat(32), u32::MAX);
        assert_eq!(key.len(), OBJECT_KEY_MAX_BYTES);
        assert!(is_object_key(&key));
        let f = FrameRequest { key, len: u64::MAX, spec: format!("slop_ninja_a_artifact_spec_v{}", 999) };
        assert_eq!(serde_json::to_string(&f).unwrap().len(), FRAME_REQUEST_BYTES);
        assert!(!is_object_key(COHORT_SNAPSHOT_KEY));
    }

    #[test]
    fn a_stale_snapshot_answers_paused() {
        let info: crate::json::CohortInfo = serde_json::from_value(serde_json::json!({
            "schema": crate::json::SCHEMA_COHORT, "contract": crate::CONTRACT,
            "chain": {"genesis": "00", "netuid": 601, "mechanism": 0},
            "finalized_block": 9, "request_validity_blocks": 360, "paused": false,
            "admitting": null, "next": null
        }))
        .unwrap();
        let info_len = serde_json::to_string(&info).unwrap().len();
        let max = CohortSnapshot { schema: COHORT_SNAPSHOT_SCHEMA.into(), written_ms: u64::MAX, cohort: info.clone() };
        assert_eq!(serde_json::to_string(&max).unwrap().len(), info_len + COHORT_SNAPSHOT_ENVELOPE_BYTES);
        let snap = CohortSnapshot { schema: COHORT_SNAPSHOT_SCHEMA.into(), written_ms: 1_000, cohort: info };
        assert!(!snap.clone().serve(1_000 + COHORT_SNAPSHOT_FRESH_MS - 1).paused);
        assert!(snap.clone().serve(1_000 + COHORT_SNAPSHOT_FRESH_MS).paused);
        assert!(!snap.serve(0).paused, "a clock behind the write");
    }

    #[test]
    fn object_keys_are_the_session_records_shape() {
        assert!(is_object_key("admission/testnet-r0-19/0a1b/a1.object"));
        assert!(!is_object_key("artifacts/blobs/00"));
        assert!(!is_object_key("admission/../pool/x.object"));
        assert!(!is_object_key("admission/x y.object"));
    }
}
