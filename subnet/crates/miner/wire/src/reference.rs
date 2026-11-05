//! The reference staging contract `slop_ninja_reference_staging_v1`: the
//! eval Worker's two operator routes that stage the operator's reference
//! packages through the internal signer, and the operator's client
//! (`slop_ninja_reference_stage`). Both sides use these paths and shapes,
//! so a rename on one side is a compile error on the other.
//!
//! - `POST` [`URLS_PATH`]: an [`UrlsRequest`] answered by an [`UrlsReply`]
//!   (one presigned [`Upload`] of kind [`UPLOAD_KIND`] per file, in request
//!   order).
//! - `POST` [`COMMIT_PATH`]`?spec=<spec id>`: the unpinned files manifest's
//!   exact bytes, answered by a [`CommitReply`].
//!
//! Owned serde types (feature `json`): the Worker builds them only inside
//! its route's bounded arena, whose footprint counts their largest
//! serialization.

use serde::{Deserialize, Serialize};

use crate::json::Upload;

/// The staging rule's version, named in every answer.
pub const STAGING_RULE: &str = "slop_ninja_reference_staging_v1";
/// The operator routes' common prefix.
pub const PREFIX: &str = "/eval/reference/";
/// Presigned PUTs for a package's files.
pub const URLS_PATH: &str = "/eval/reference/v2/urls";
/// The pinned manifest's write.
pub const COMMIT_PATH: &str = "/eval/reference/v2/commit";
/// The `kind` of every reference [`Upload`].
pub const UPLOAD_KIND: &str = "reference";

const URLS_FRAME: &str = r#"{"files":[]}"#;
const URLS_ENTRY: &str = r#"{"sha256":"","bytes":},"#;

/// The exact bound of a compact `urls` body naming `files` files: the frame
/// and per file a 64-digit digest and a `u64` in at most 20 digits (with a
/// comma after every entry, one more byte than the longest body).
pub const fn urls_body_max(files: usize) -> usize {
    URLS_FRAME.len() + files * (URLS_ENTRY.len() + 64 + 20)
}

/// The `urls` request: each distinct file by digest and length.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UrlsRequest {
    pub files: Vec<FileRequest>,
}

/// One file of a [`UrlsRequest`]: its lowercase hex SHA-256 and length.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRequest {
    pub sha256: String,
    pub bytes: u64,
}

/// The `urls` answer: one upload per requested file, in request order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UrlsReply {
    pub rule: String,
    pub uploads: Vec<Upload>,
}

/// The `commit` answer: the package (the pinned manifest's SHA-256, the
/// digest a template's `reference.package_sha256` names), its manifest key
/// in the eval bucket, the pinned manifest's length and each pinned file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitReply {
    pub rule: String,
    pub package_sha256: String,
    pub manifest_key: String,
    pub manifest_bytes: u64,
    pub files: Vec<PinnedFile>,
}

/// One file of a [`CommitReply`], with the entity tag the commit pinned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedFile {
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
    pub key: String,
    pub etag: String,
}
