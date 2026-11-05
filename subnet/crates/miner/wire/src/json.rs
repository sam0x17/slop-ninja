//! JSON response and error shapes of `slop_ninja.admission/v2`.
//!
//! Digests and keys are lowercase hex strings without a prefix (parsers accept
//! `0x`), addresses are SS58, and states and codes stay strings so a client
//! keeps working when a later coordinator adds one. Unknown fields are
//! ignored.
//!
//! These are owned types: they allocate, so they serve the native client and
//! the test mock. Workers code inside a bounded arena writes the same shapes
//! from borrowed data instead.

use serde::{Deserialize, Serialize};
use slop_ninja_protocol::{ChainBinding, Digest};

use crate::{ErrorCode, SessionState, parse_hex32};

pub const SCHEMA_COHORT: &str = "slop_ninja.admission.cohort/v2";
pub const SCHEMA_SESSION: &str = "slop_ninja.admission.session/v2";
pub const SCHEMA_HOTKEY: &str = "slop_ninja.admission.hotkey/v2";
pub const SCHEMA_ERROR: &str = "slop_ninja.admission.error/v2";
/// The authorization pointer (`docs/testnet/public-layout.md`,
/// "Authorizations"), private and public.
pub const AUTHORIZATION_POINTER_SCHEMA: &str = "slop_ninja.public.authorization/v2";

/// A chain binding as JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chain {
    pub genesis: String,
    pub netuid: u16,
    pub mechanism: u8,
}

impl Chain {
    pub fn from_binding(chain: &ChainBinding) -> Self {
        Self {
            genesis: chain.genesis.hex(),
            netuid: chain.netuid,
            mechanism: chain.mechanism,
        }
    }
    pub fn binding(&self) -> Option<ChainBinding> {
        Some(ChainBinding {
            genesis: Digest(parse_hex32(&self.genesis)?),
            netuid: self.netuid,
            mechanism: self.mechanism,
        })
    }
}

/// `GET /admission/v2/cohort`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortInfo {
    pub schema: String,
    pub contract: String,
    pub chain: Chain,
    pub finalized_block: u64,
    /// How far past the cursor a request's `valid_until` may reach.
    pub request_validity_blocks: u64,
    #[serde(default)]
    pub paused: bool,
    pub admitting: Option<Admitting>,
    pub next: Option<NextCohort>,
    /// The subnet epoch index (`SubnetEpochIndex`) the coordinator last
    /// observed at a finalized block, when it has observed one: the epoch
    /// that `admitting` and `next` epochs are measured from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed_epoch: Option<u64>,
}

/// The admitting cohort and its parameters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Admitting {
    pub cohort: u64,
    pub label: String,
    pub opened_epoch: u64,
    pub open_block: u64,
    pub planned_cutoff_epoch: u64,
    #[serde(default)]
    pub closing: bool,
    pub output_contract: String,
    pub roster_cap: u16,
    pub selection: String,
    pub max_object_len: u64,
    /// The URL policy that sets each presigned URL's life.
    pub upload_policy: UploadPolicy,
    pub limits: String,
    /// The artifact spec whose checks admission runs.
    #[serde(default)]
    pub artifact_spec: Option<ArtifactSpecRef>,
}

/// The upload URL policy (`slop_ninja_upload_url_policy_v1`): a URL lives
/// `ceil(L_total / b_min) + skew_secs` seconds (at most R2's 7 days), where
/// `L_total` is the artifact's committed size. Expiry ends one URL only: a
/// client asks for fresh ones whenever it needs them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadPolicy {
    pub id: String,
    pub b_min: u64,
    pub skew_secs: u64,
}

/// An artifact spec by id and digest (`slop_ninja_artifact_check spec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSpecRef {
    pub id: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextCohort {
    pub cohort: u64,
    /// A lower bound: the cohort opens at the boundary that closes the one
    /// before it, which slow work or a coordinator outage moves later.
    #[serde(default)]
    pub earliest_open_epoch: Option<u64>,
    /// How many tempos its admission window lasts once it opens (its plan's
    /// `admission_tempos`), when the coordinator knows the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_tempos: Option<u64>,
    /// The output contract its plan names, when the coordinator knows the
    /// plan: what `check` compares the header with between windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_contract: Option<String>,
    /// Its admitted object's byte bound, when the plan is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_object_len: Option<u64>,
    /// The artifact spec its admission runs, when the plan is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_spec: Option<ArtifactSpecRef>,
}

/// One object of a session: the header (`kind` `header`) or a file, by its
/// committed SHA-256 and length, and whether admission recorded it present.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectState {
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    pub sha256: String,
    pub len: u64,
    pub present: bool,
}

/// One presigned upload: PUT the object's exact bytes to `url` with exactly
/// these headers (and `host`), nothing else `x-amz-*`, never chunked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upload {
    pub kind: String,
    pub sha256: String,
    pub len: u64,
    pub url: String,
    pub content_length: String,
    pub if_none_match: String,
    pub checksum_sha256: String,
    /// Unix seconds of the signature, and how long the URL lives.
    pub signed_at: u64,
    pub valid_secs: u32,
}

/// A commitment's admission position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub block: u64,
    pub extrinsic: u32,
}

/// Structured detail of an error or refusal; every field is optional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detail {
    /// The admitting cohort, for `wrong_cohort`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitting: Option<u64>,
    /// The next cohort, for `not_admitting`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<NextCohort>,
    /// The SHA-256 of every object still missing, for `objects_missing`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing: Option<Vec<String>>,
    /// The cohort the on-chain payload names, for `commitment_mismatch`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_cohort: Option<u64>,
    /// The artifact check's own code, for `check_failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// A short machine-readable reason, for example a protocol error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The live session, for `second_artifact`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The coordinator's finalized cursor, for `request_expired`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalized_block: Option<u64>,
}

impl Detail {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A session refusal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Detail::is_empty")]
    pub detail: Detail,
}

impl Refusal {
    pub fn code(&self) -> Option<ErrorCode> {
        ErrorCode::parse(&self.code)
    }
}

/// `POST /admission/v2/sessions`, `GET /admission/v2/sessions/{session}`,
/// and the answers of `urls`, `complete` and `repair`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub schema: String,
    pub session: String,
    pub cohort: u64,
    pub hotkey: String,
    pub commitment: String,
    pub header_sha256: String,
    pub header_len: u32,
    /// The header, then each file once its header is recorded.
    #[serde(default)]
    pub objects: Vec<ObjectState>,
    pub state: String,
    #[serde(default)]
    pub refusal: Option<Refusal>,
    #[serde(default)]
    pub position: Option<Position>,
    #[serde(default)]
    pub next_cohort: Option<u64>,
    pub finalized_block: u64,
    /// Fresh presigned URLs, on the answers that mint them.
    #[serde(default)]
    pub uploads: Vec<Upload>,
    /// Recorded objects that went missing after admission recorded them:
    /// `repair` mints a URL for each (the same key, so the same bytes).
    #[serde(default)]
    pub repair_needed: Vec<String>,
}

impl Session {
    pub fn state(&self) -> Option<SessionState> {
        SessionState::parse(&self.state)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub uid: u16,
    pub registration_block: u64,
}

/// The hotkey's commitment that counts for the cohort, from the
/// coordinator's commitment log: its latest one while the cohort admits, its
/// last one before the cutoff block once admission has closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitmentView {
    pub block: u64,
    /// Hash of the commitment's block, as `0x`-less hex.
    #[serde(default)]
    pub block_hash: Option<String>,
    pub extrinsic: u32,
    pub payload: String,
    #[serde(default)]
    pub payload_cohort: Option<u64>,
    pub in_window: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session: String,
    pub state: String,
    #[serde(default)]
    pub refusal: Option<Refusal>,
    /// Recorded objects that went missing (`repair` restores them).
    #[serde(default)]
    pub repair_needed: Vec<String>,
}

/// The latest refused `POST /admission/v2/sessions` by the hotkey
/// for the cohort. Content-free.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRefusal {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Detail::is_empty")]
    pub detail: Detail,
    /// The coordinator's finalized cursor when it refused the request.
    pub finalized_block: u64,
    /// The refused envelope's session id (its digest).
    pub session: String,
    /// The cohort the refused opening names.
    pub opening_cohort: u64,
}

impl LastRefusal {
    pub fn code(&self) -> Option<ErrorCode> {
        ErrorCode::parse(&self.code)
    }
}

/// `GET /admission/v2/cohorts/{cohort}/hotkeys/{hotkey}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyView {
    pub schema: String,
    pub cohort: u64,
    pub hotkey: String,
    pub finalized_block: u64,
    pub registration: Option<Registration>,
    pub commitment: Option<CommitmentView>,
    /// The cohort's cutoff block, once admission has closed.
    #[serde(default)]
    pub cutoff_block: Option<u64>,
    #[serde(default)]
    pub sessions: Vec<SessionSummary>,
    #[serde(default)]
    pub last_refusal: Option<LastRefusal>,
}

/// Every error response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub schema: String,
    pub error: ErrorJson,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorJson {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Detail::is_empty")]
    pub detail: Detail,
}

impl ErrorBody {
    pub fn new(code: ErrorCode, message: impl Into<String>, detail: Detail) -> Self {
        Self {
            schema: SCHEMA_ERROR.into(),
            error: ErrorJson {
                code: code.as_str().into(),
                message: message.into(),
                retryable: code.retryable(),
                detail,
            },
        }
    }
}

/// `authorizations/current.json`, written by the coordinator in the private
/// receipts bucket, and its public copy `v1/authorizations/current.json`: the
/// authorization validators commit under now, any refresh of its renew-by
/// hint, and the revocation set, each a content-addressed signed object. An
/// authorization never expires, so the pointer has no validity window: it
/// stays current until the coordinator writes another. Unlike the admission
/// shapes, unknown fields are refused: the coordinator and the publisher
/// share this exact type. Schema v2 replaced v1's single latest revocation
/// with the set, so a validator that missed a revocation still journals it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationPointer {
    pub schema: String,
    pub chain: Chain,
    /// `launch` or `cohort`.
    pub kind: String,
    pub authorization: ObjectRef,
    #[serde(default)]
    pub renewal: Option<ObjectRef>,
    /// The revocation set: the latest signed revocation of every revoked
    /// target, oldest first, concatenated, each of the schema's fixed signed
    /// length (so its length is a whole multiple of that length); absent
    /// when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocations: Option<ObjectRef>,
    /// The coordinator's latest signed row (`slop_ninja_protocol::CoordinatorRow`),
    /// signed when it committed that row under this authorization: what
    /// every other validator commits (the copy model). Absent until the
    /// coordinator first commits under a build that signs rows. A reader
    /// built before this field refuses a pointer that carries it (unknown
    /// fields are refused), so readers deploy before the writer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<ObjectRef>,
}

/// A content-addressed object a document names: its key, SHA-256 (hex) and
/// length.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectRef {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}
