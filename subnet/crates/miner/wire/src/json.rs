//! JSON response and error shapes of `slop_ninja.admission/v1`.
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

pub const SCHEMA_COHORT: &str = "slop_ninja.admission.cohort/v1";
pub const SCHEMA_SESSION: &str = "slop_ninja.admission.session/v1";
pub const SCHEMA_HOTKEY: &str = "slop_ninja.admission.hotkey/v1";
pub const SCHEMA_ERROR: &str = "slop_ninja.admission.error/v1";
/// The authorization pointer (`docs/testnet/public-layout.md`,
/// "Authorizations"), private and public.
pub const AUTHORIZATION_POINTER_SCHEMA: &str = "slop_ninja.public.authorization/v1";

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

/// `GET /admission/v1/cohort`.
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
    pub part_size: u64,
    pub max_parts: u32,
    pub limits: String,
    /// The artifact spec whose checks admission runs.
    #[serde(default)]
    pub artifact_spec: Option<ArtifactSpecRef>,
}

/// An artifact spec by id and digest (`slop_ninja_artifact_check spec`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSpecRef {
    pub id: String,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextCohort {
    pub cohort: u64,
    #[serde(default)]
    pub earliest_open_epoch: Option<u64>,
}

/// One recorded part.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartRecord {
    pub part: u32,
    pub len: u64,
    pub sha256: String,
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
    /// Missing part numbers, for `parts_missing`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing: Option<Vec<u32>>,
    /// The cohort the on-chain payload names, for `commitment_mismatch`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_cohort: Option<u64>,
    /// The artifact check's own code, for `check_failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// A short machine-readable reason, for example a protocol error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The live session, for `second_artifact`; the session, for
    /// `stale_attempt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The session's current attempt, for `stale_attempt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
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

/// `POST /admission/v1/sessions` and `GET /admission/v1/sessions/{session}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub schema: String,
    pub session: String,
    pub cohort: u64,
    pub hotkey: String,
    pub commitment: String,
    pub artifact_sha256: String,
    pub artifact_len: u64,
    pub part_size: u64,
    pub part_count: u32,
    /// Starts at 1 and grows by one each time an aborted session reopens.
    pub attempt: u32,
    #[serde(default)]
    pub parts: Vec<PartRecord>,
    pub state: String,
    #[serde(default)]
    pub refusal: Option<Refusal>,
    #[serde(default)]
    pub position: Option<Position>,
    #[serde(default)]
    pub next_cohort: Option<u64>,
    pub finalized_block: u64,
}

impl Session {
    pub fn state(&self) -> Option<SessionState> {
        SessionState::parse(&self.state)
    }
}

/// `PUT /admission/v1/sessions/{session}/parts/{n}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartAck {
    pub part: u32,
    pub len: u64,
    pub sha256: String,
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
    pub attempt: Option<u32>,
    #[serde(default)]
    pub refusal: Option<Refusal>,
}

/// The latest refused `POST /admission/v1/sessions` or reopen by the hotkey
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

/// `GET /admission/v1/cohorts/{cohort}/hotkeys/{hotkey}`.
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
/// hint, and the latest signed revocation the coordinator holds, each a
/// content-addressed signed object. An authorization never expires, so the
/// pointer has no validity window: it stays current until the coordinator
/// writes another. Unlike the admission shapes, unknown fields are refused:
/// the coordinator and the publisher share this exact type.
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
    /// The latest signed revocation; absent when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation: Option<ObjectRef>,
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
