//! The scorer's files manifest v2 (`slop_ninja_a_artifact_files_v2`): what
//! a scorer load reads, file by file, straight from object storage.
//!
//! Each file is listed with its name, scorer role, length and SHA-256 (the
//! header's, verified by the object store at ingest), and where it is
//! stored: the bucket (`admission` for a miner's admitted artifact, `eval`
//! for an operator-staged reference package), the key, and the object's
//! entity tag when one was recorded. A load reads each file by ranged,
//! entity-tag-pinned reads and never hashes it. The manifest's exact bytes
//! are the package: their SHA-256 (a small record's) is the package digest
//! the coordinator and the scorer bind.

use serde::{Deserialize, Serialize};
use slop_ninja_protocol::ArtifactHeader;

use crate::Refusal;
use crate::files::header_from_listing;
use crate::mapping::check_header_for;
use crate::spec::{self, Version};

pub const FILES_SCHEMA_V2: &str = "slop_ninja_a_artifact_files_v2";
/// Largest manifest: the scorer's `MAX_MANIFEST_BYTES`.
pub const MAX_MANIFEST_BYTES: usize = spec::MAX_MANIFEST_BYTES as usize;
/// The buckets a manifest may name.
pub const BUCKETS: [&str; 2] = ["admission", "eval"];
/// The longest key a manifest may name: R2's key limit.
pub const MAX_KEY_BYTES: usize = 1024;
/// The longest entity tag a manifest may name: R2's quoted MD5 forms (a
/// single-part tag is 32 hex digits; a multipart one adds `-<count>`), with
/// room for quotes.
pub const MAX_ETAG_BYTES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesManifest {
    pub schema: String,
    pub profile_id: String,
    pub contract: String,
    /// Name of the encoder graph among `files`.
    pub graph: String,
    /// In the spec's file-table order.
    pub files: Vec<FileEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub name: String,
    /// The scorer role string of the file table.
    pub role: String,
    pub bytes: u64,
    pub sha256: String,
    pub bucket: String,
    pub key: String,
    /// The object's entity tag when admission recorded it; a load pins
    /// every read to it.
    pub etag: Option<String>,
}

fn package_digest(b: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(b))
}

impl FilesManifest {
    /// A manifest for a checked `header`, in file-table order, with each
    /// file's location from `locate(name, sha256 hex)`.
    pub fn for_header(
        header: &ArtifactHeader,
        version: Version,
        profile_id: &str,
        mut locate: impl FnMut(&str, &str) -> (String, String, Option<String>),
    ) -> Result<Self, Refusal> {
        check_header_for(header, version)?;
        let mut files = Vec::with_capacity(spec::ROLES.len());
        for role in &spec::ROLES {
            let f = header.files.iter().find(|f| f.name == role.name).ok_or(Refusal::MissingFile(role.name))?;
            let sha = hex::encode(f.sha256.0);
            let (bucket, key, etag) = locate(&f.name, &sha);
            files.push(FileEntry {
                name: f.name.clone(),
                role: role.scorer_role.into(),
                bytes: f.len,
                sha256: sha,
                bucket,
                key,
                etag,
            });
        }
        let m = Self {
            schema: FILES_SCHEMA_V2.into(),
            profile_id: profile_id.into(),
            contract: header.contract.clone(),
            graph: spec::GRAPH_FILE.into(),
            files,
        };
        m.check_shape()?;
        Ok(m)
    }

    /// The exact bytes: pretty JSON with a final newline. Their SHA-256 is
    /// the package digest.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(self).unwrap_or_default();
        bytes.push(b'\n');
        bytes
    }

    /// The package digest of these bytes (a small record).
    pub fn package_of(bytes: &[u8]) -> String {
        package_digest(bytes)
    }

    /// Parses manifest bytes, requires them to hash to `package`, and checks
    /// their shape and file table under `version`.
    pub fn parse(bytes: &[u8], package: &str, version: Version) -> Result<Self, Refusal> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Refusal::Object("files manifest too large".into()));
        }
        if package_digest(bytes) != package {
            return Err(Refusal::Object("files manifest bytes do not hash to the package digest".into()));
        }
        let m: Self = serde_json::from_slice(bytes).map_err(|e| Refusal::Object(format!("files manifest: {e}")))?;
        m.check_shape()?;
        check_header_for(&m.header()?, version)?;
        Ok(m)
    }

    fn check_shape(&self) -> Result<(), Refusal> {
        let bad = |what: &str| Err(Refusal::Object(format!("files manifest: {what}")));
        if self.schema != FILES_SCHEMA_V2 {
            return bad("schema");
        }
        if self.graph != spec::GRAPH_FILE {
            return bad("graph");
        }
        if self.files.len() != spec::ROLES.len() {
            return bad("file count");
        }
        for (f, role) in self.files.iter().zip(spec::ROLES.iter()) {
            if f.name != role.name || f.role != role.scorer_role {
                return bad("file order or role");
            }
            if !BUCKETS.contains(&f.bucket.as_str())
                || f.key.is_empty()
                || f.key.len() > MAX_KEY_BYTES
                || f.etag.as_ref().is_some_and(|e| e.is_empty() || e.len() > MAX_ETAG_BYTES)
            {
                return bad("location");
            }
        }
        if self.to_bytes().len() > MAX_MANIFEST_BYTES {
            return bad("too large");
        }
        Ok(())
    }

    /// The header the manifest lists (its digests are the store-verified
    /// ones).
    pub fn header(&self) -> Result<ArtifactHeader, Refusal> {
        let list: Vec<(&str, &str, u64)> =
            self.files.iter().map(|f| (f.name.as_str(), f.sha256.as_str(), f.bytes)).collect();
        header_from_listing(&self.contract, &list)
    }
}
