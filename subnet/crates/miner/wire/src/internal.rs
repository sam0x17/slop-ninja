//! The internal interface between the two admission Workers (feature
//! `json`): the key-free upload Worker `slop-ninja-admission-testnet`, which
//! alone serves the public `/admission/v2/` routes, and the key-holding eval
//! Worker, which keeps the cohort authority's state and verdicts. No upload
//! byte crosses either Worker: miners PUT straight to R2 under presigned
//! URLs that the internal signer Worker mints for the eval Worker.
//!
//! - **The cohort snapshot**: the cohort authority writes the contract's
//!   unsigned cohort response, with the time it was written and the
//!   hotkeys whose sessions wait for a repair, to [`COHORT_SNAPSHOT_KEY`]
//!   in the eval bucket (no presigning credential reaches it) at every
//!   heartbeat. The
//!   upload Worker has no R2 binding: it serves `GET /admission/v2/cohort`
//!   from its one-block edge copy of that object, and refreshes the copy
//!   through the interface operation [`op::SNAPSHOT`], which the eval Worker
//!   answers from the object itself without reaching the cohort object. So
//!   no unsigned request reaches the cohort authority, and the eval Worker
//!   sees at most one snapshot read per block per edge location.
//! - **Upload Worker to eval Worker** (`UPLOAD → EVAL`, the upload Worker's
//!   service binding): a request it has decoded and checked (signature, body
//!   digest, registration on the subnet, the spec-derived size bound) is
//!   forwarded as one bounded JSON message, [`Forward`], to
//!   `EVAL_INTERFACE_ORIGIN` + [`EVAL_PATH_PREFIX`] + the operation. The eval
//!   Worker answers with the cohort authority's [`Reply`].
//! - **Eval Worker to upload Worker** (`EVAL → UPLOAD`, the eval Worker's
//!   service binding): the cohort authority asks the upload Worker to decode
//!   and check one uploaded header (its bytes, at most
//!   `MAX_ARTIFACT_HEADER_LEN`, read from R2 by the eval Worker, to
//!   `UPLOAD_INTERFACE_ORIGIN` + [`UPLOAD_HEADER_PATH`] with the spec id in
//!   [`SPEC_HEADER`]); the answer is
//!   `slop_ninja_artifact_check::admission::HeaderOutcome`. The eval Worker
//!   never decodes an uploaded header itself, and recomputes the content
//!   digest from the returned table.
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
pub const EVAL_PATH_PREFIX: &str = "/internal/admission/v2/";
/// The upload Worker's header-check route.
pub const UPLOAD_HEADER_PATH: &str = "/internal/upload/v2/header";
/// The header naming the cohort's artifact spec id on a header check.
pub const SPEC_HEADER: &str = "x-sn-artifact-spec";

/// The eval Worker's interface operations: the cohort authority's
/// `/admission/<op>` routes, one per public route step.
pub mod op {
    /// The cohort read's plan entry. Not an interface operation: the upload
    /// Worker serves the cohort read from the snapshot
    /// ([`super::COHORT_SNAPSHOT_KEY`]), so it is not in [`ALL`].
    pub const COHORT: &str = "cohort";
    pub const CREATE: &str = "create";
    pub const READ: &str = "read";
    /// Fresh presigned URLs for the objects not yet recorded.
    pub const URLS: &str = "urls";
    pub const COMPLETE: &str = "complete";
    /// A URL for a recorded object that went missing.
    pub const REPAIR: &str = "repair";
    pub const HOTKEY: &str = "hotkey";
    /// Every operation the interface forwards to the cohort object.
    pub const ALL: &[&str] = &[CREATE, READ, URLS, COMPLETE, REPAIR, HOTKEY];
    /// The cohort snapshot's bytes ([`super::CohortSnapshot`], at most
    /// [`super::cohort_snapshot_limit`]): `POST` with the body `{}` and the
    /// interface token, answered by the eval Worker from
    /// [`super::COHORT_SNAPSHOT_KEY`] in the upload bucket, 200 with the
    /// object's exact bytes, or 503 while it is not written. Not in [`ALL`]:
    /// it never reaches the cohort object. The upload Worker calls it only
    /// on an edge-cache miss of the cohort read.
    pub const SNAPSHOT: &str = "snapshot";
}

/// A signed request as the upload Worker forwards it: method and path
/// exactly as sent, the four signature headers, and for a body read whole,
/// its SHA-256.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signed {
    pub method: String,
    pub path: String,
    pub hotkey: Option<String>,
    pub valid_until: Option<String>,
    pub content_sha256: Option<String>,
    pub signature: Option<String>,
    /// SHA-256 of a body the upload Worker read whole (hex).
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
    pub cohort: u64,
    #[serde(default)]
    pub hotkey: String,
}

/// The room of a [`Forward`]'s JSON beside a creation body: every field at
/// its format bound (method and path as [`crate::Route::parse`] accepts
/// them, the four signature headers as the gate's `verify` parses them, a
/// session id, the route's integers and an SS58 hotkey), measured under
/// 2 KiB (tested); no field comes from R2 any more, so the bound is the
/// format's alone (version [`INTERFACE_BOUNDS_VERSION`]). A forward over the
/// bound is refused, never cut.
pub const FORWARD_FIELDS_BYTES: usize = 2 * 1024;

/// The version of this module's message bounds ([`FORWARD_FIELDS_BYTES`],
/// [`HEADER_REQUEST_BYTES`], [`cohort_snapshot_limit`]); a change to any of
/// them is a new version. v3: the snapshot's repair list.
pub const INTERFACE_BOUNDS_VERSION: &str = "admission-interface-bounds-2026-11-05-v3";

/// The bound of one forwarded message: [`FORWARD_FIELDS_BYTES`] and a
/// session-creation body as hex ([`crate::UPLOAD_RECORD_BYTES`]).
pub const FORWARD_BYTES: usize = FORWARD_FIELDS_BYTES + 2 * crate::UPLOAD_RECORD_BYTES;

/// The bound of a header-check request body: one header, at most the
/// protocol's `MAX_ARTIFACT_HEADER_LEN`, as raw bytes.
pub const HEADER_REQUEST_BYTES: usize = slop_ninja_protocol::MAX_ARTIFACT_HEADER_LEN as usize;

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

/// The longest artifact spec id: `slop_ninja_a_artifact_spec_v` and a
/// version number of at most three digits.
pub const SPEC_ID_MAX_BYTES: usize = "slop_ninja_a_artifact_spec_v".len() + 3;

/// The header of an answer from the upload Worker's header route that failed
/// for its own platform's reason (its arena refused the scope, the body
/// could not be read): retried without counting against the miner. The
/// route is reached only through the eval Worker's binding with the token,
/// so no untrusted request can produce it.
pub const HEADER_FAILURE_HEADER: &str = "x-sn-header-failure";

/// The request header that marks a header-route call as the eval Worker's
/// link probe (out of band, never in the body: a miner's header bytes can
/// never be taken for the probe). The upload Worker answers it without
/// reading the body: `{"probe": "ok"}` once its own registration read (the
/// roster at the finalized head, the read every signed route needs) works,
/// so the canary also covers the front's chain read.
pub const HEADER_PROBE_HEADER: &str = "x-sn-header-probe";

/// The eval bucket key of the cohort snapshot ([`CohortSnapshot`]). It is
/// written to the eval bucket, which neither R2 S3 credential reaches, so a
/// compromised signer cannot rewrite the cohort response every miner reads
/// (review round 2). Not an uploaded object's key (no `admission/v2/` key
/// parses as it).
pub const COHORT_SNAPSHOT_KEY: &str = "cohort/v1/current.json";
/// The schema of [`CohortSnapshot`].
pub const COHORT_SNAPSHOT_SCHEMA: &str = "slop_ninja.admission.cohort_snapshot/v1";
/// How long a snapshot answers as written: the coordinator's heartbeat
/// freshness (`engine::HEARTBEAT_FRESH_MS`, 10 minutes; a coordinator test
/// checks they are equal). An older snapshot answers `paused: true`, as the
/// cohort authority itself answers once its heartbeat is that old.
pub const COHORT_SNAPSHOT_FRESH_MS: u64 = 600_000;

/// The cohort response as the cohort authority wrote it, and when (its
/// clock, milliseconds), with the repair list the upload Worker's gate
/// reads ([`CohortSnapshot::may_repair`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortSnapshot {
    pub schema: String,
    pub written_ms: u64,
    pub cohort: crate::json::CohortInfo,
    /// The hotkeys (64 hex) of the sessions waiting for the miner's repair
    /// (stage `repair`: an admitted entrant's, or a verifying session's), at
    /// most [`REPAIR_LIST_MAX`]. The gate requires registration on the
    /// subnet for a session read or a repair unless the signer is listed, so
    /// an entrant deregistered since admission can still repair (design R4)
    /// while a throwaway key's signed traffic never reaches the cohort
    /// authority (review round 2). Not served to clients.
    #[serde(default)]
    pub repair: Vec<String>,
    /// More sessions wait for a repair than [`REPAIR_LIST_MAX`] can name:
    /// every signer may then read and repair unregistered, as if listed (the
    /// list never refuses a repair it could not name).
    #[serde(default)]
    pub repair_all: bool,
}

/// The most hotkeys [`CohortSnapshot::repair`] names: one roster
/// (`slop_ninja_protocol::MAX_ROSTER`), the most entrants one cohort admits.
/// Past it the snapshot says [`CohortSnapshot::repair_all`] instead.
pub const REPAIR_LIST_MAX: usize = slop_ninja_protocol::MAX_ROSTER as usize;

/// The repair list's bytes at its bound: [`REPAIR_LIST_MAX`] quoted 64-hex
/// hotkeys and the commas between them.
pub const REPAIR_LIST_BYTES: usize = REPAIR_LIST_MAX * (64 + 2) + (REPAIR_LIST_MAX - 1);

/// The snapshot's envelope around the cohort response, from its format:
/// `{"schema":"<schema>","written_ms":<u64>,"cohort":`, then
/// `,"repair":[<list>],"repair_all":false}` with the list at its bound
/// (tested exact).
pub const COHORT_SNAPSHOT_ENVELOPE_BYTES: usize = r#"{"schema":""#.len()
    + COHORT_SNAPSHOT_SCHEMA.len()
    + r#"","written_ms":"#.len()
    + 20
    + r#","cohort":"#.len()
    + r#","repair":["#.len()
    + REPAIR_LIST_BYTES
    + r#"],"repair_all":false"#.len()
    + "}".len();

/// The bound the upload Worker reads a snapshot within: the cohort response
/// at the client's limit for it ([`crate::response_limit`] before the
/// cohort's parameters are known) and the envelope.
pub const fn cohort_snapshot_limit() -> usize {
    crate::response_limit() + COHORT_SNAPSHOT_ENVELOPE_BYTES
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

    /// Whether `hotkey` may read its session or repair without being
    /// registered: listed, or the list could not name everyone.
    pub fn may_repair(&self, hotkey: &slop_ninja_protocol::AccountId) -> bool {
        self.repair_all || self.repair.contains(&hex::encode(hotkey.0))
    }
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
    fn a_forward_at_its_format_bounds_fits_its_bound() {
        let sid = "ab".repeat(32);
        let path = format!("/admission/v2/sessions/{sid}/complete");
        let hotkey = "5".repeat(crate::SS58_ACCOUNT_MAX_CHARS);
        let signed = Signed {
            method: "POST".into(),
            path,
            hotkey: Some(hotkey.clone()),
            valid_until: Some(u64::MAX.to_string()),
            content_sha256: Some(format!("0x{}", "f".repeat(64))),
            signature: Some(format!("0x{}", "f".repeat(128))),
            body_sha256: Some("f".repeat(64)),
            body: Some("ff".repeat(crate::UPLOAD_RECORD_BYTES)),
            commitment_block: Some(u64::MAX),
        };
        let f = Forward { signed, session: sid, cohort: u64::MAX, hotkey };
        let fields = serde_json::to_string(&f).unwrap().len() - 2 * crate::UPLOAD_RECORD_BYTES;
        assert!(fields < FORWARD_FIELDS_BYTES, "{fields}");
        assert!(serde_json::to_string(&f).unwrap().len() <= FORWARD_BYTES);
        assert_eq!(HEADER_REQUEST_BYTES, 1 << 20);
    }

    #[test]
    fn the_snapshot_read_never_reaches_the_cohort_object() {
        assert!(!op::ALL.contains(&op::SNAPSHOT) && !op::ALL.contains(&op::COHORT));
        assert!(serde_json::from_str::<Forward>("{}").is_ok(), "the snapshot request body parses as an empty forward");
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
        let listed = slop_ninja_protocol::AccountId([0xab; 32]);
        let max = CohortSnapshot {
            schema: COHORT_SNAPSHOT_SCHEMA.into(),
            written_ms: u64::MAX,
            cohort: info.clone(),
            repair: vec![hex::encode(listed.0); REPAIR_LIST_MAX],
            repair_all: false,
        };
        assert_eq!(serde_json::to_string(&max).unwrap().len(), info_len + COHORT_SNAPSHOT_ENVELOPE_BYTES);
        assert!(max.may_repair(&listed) && !max.may_repair(&slop_ninja_protocol::AccountId([1; 32])));
        let all = CohortSnapshot { repair: Vec::new(), repair_all: true, ..max };
        assert!(all.may_repair(&slop_ninja_protocol::AccountId([1; 32])));
        let snap = CohortSnapshot {
            schema: COHORT_SNAPSHOT_SCHEMA.into(),
            written_ms: 1_000,
            cohort: info,
            repair: Vec::new(),
            repair_all: false,
        };
        assert!(!snap.may_repair(&listed));
        assert!(!snap.clone().serve(1_000 + COHORT_SNAPSHOT_FRESH_MS - 1).paused);
        assert!(snap.clone().serve(1_000 + COHORT_SNAPSHOT_FRESH_MS).paused);
        assert!(!snap.serve(0).paused, "a clock behind the write");
    }

}
