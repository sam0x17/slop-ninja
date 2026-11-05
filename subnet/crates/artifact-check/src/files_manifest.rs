//! The scorer's files manifest v2 (`slop_ninja_a_artifact_files_v2`): what
//! a scorer load reads, file by file, straight from object storage.
//!
//! Each file is listed with its name, scorer role, length and SHA-256 (the
//! header's, verified by the object store at ingest), and where it is
//! stored: the bucket (`admission` for a miner's admitted artifact and for
//! an operator-staged reference file under `reference/v2/files/`; `eval`
//! for a reference file kept in the eval bucket), the key, and the object's
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

/// Where operator-staged reference files are stored (the staging rule
/// `slop_ninja_reference_staging_v1`): the admission bucket, beside admitted
/// artifacts, under their own prefix. The signer presigns each PUT there
/// bound to the file's length and SHA-256, so R2 verifies every reference
/// file as it does a miner's.
pub const REFERENCE_BUCKET: &str = "admission";
/// The prefix of reference file keys (`slop_ninja_r2_presign::layout::
/// REFERENCE_PREFIX`; the coordinator's tests check that the two agree).
pub const REFERENCE_FILES_PREFIX: &str = "reference/v2/files/";

/// The bucket key of a reference package file:
/// `reference/v2/files/<sha256>` in [`REFERENCE_BUCKET`].
pub fn reference_file_key(sha256: &str) -> String {
    format!("{REFERENCE_FILES_PREFIX}{sha256}")
}

/// The longest profile id a staged reference manifest may carry: a
/// protocol label (`MAX_LABEL_LEN`) in label characters (ASCII letters,
/// digits and `._:/-`), so it needs no JSON escape. `pack` writes either
/// `<spec id>/<graph class id>` or the served profile's id, both labels.
pub const REFERENCE_PROFILE_ID_BYTES: usize = slop_ninja_protocol::MAX_LABEL_LEN;

/// Whether `text` is a non-empty run of at most `max` label characters
/// (ASCII letters, digits and `._:/-`): the staged reference profile id and
/// entity tag rule, which needs no JSON escape.
pub fn reference_label_ok(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && text.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
}

/// The longest pinned reference manifest, counted from its exact
/// pretty-JSON layout: the frame with the fixed schema, contract and graph
/// strings and the longest profile id, and per file of the spec's table its
/// name and scorer role, at most 20 digits of length, the digest, the
/// reference bucket and key, and an entity tag of at most
/// [`MAX_ETAG_BYTES`] label characters. Every location is fixed by the
/// staging rule, so this is far below the scorer's general
/// [`MAX_MANIFEST_BYTES`]. The unpinned manifest (no `etag` fields) the
/// operator commits is shorter, so this also bounds the commit body.
pub const REFERENCE_MANIFEST_MAX_BYTES: usize = reference_manifest_max();

const fn reference_manifest_max() -> usize {
    let frame = "{\n  \"schema\": \"\",\n  \"profile_id\": \"\",\n  \"contract\": \"\",\n  \"graph\": \"\",\n  \"files\": [\n  ]\n}\n";
    let entry = "    {\n      \"name\": \"\",\n      \"role\": \"\",\n      \"bytes\": ,\n      \"sha256\": \"\",\n      \"bucket\": \"\",\n      \"key\": \"\",\n      \"etag\": \"\"\n    },\n";
    let mut n = frame.len() + FILES_SCHEMA_V2.len() + REFERENCE_PROFILE_ID_BYTES + spec::CONTRACT_ID.len() + spec::GRAPH_FILE.len();
    let per_file = entry.len() + 20 + 64 + REFERENCE_BUCKET.len() + REFERENCE_FILES_PREFIX.len() + 64 + MAX_ETAG_BYTES;
    let mut i = 0;
    while i < spec::ROLES.len() {
        n += per_file + spec::ROLES[i].name.len() + spec::ROLES[i].scorer_role.len();
        i += 1;
    }
    n
}
const _: () = assert!(REFERENCE_MANIFEST_MAX_BYTES <= MAX_MANIFEST_BYTES);

/// Why an unpinned reference manifest is outside the staging rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReferenceRefusal {
    /// The profile id is not a label of at most
    /// [`REFERENCE_PROFILE_ID_BYTES`].
    ProfileId,
    /// A file is not in [`REFERENCE_BUCKET`] at [`reference_file_key`] with
    /// no entity tag.
    Location,
}

impl ReferenceRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProfileId => "profile_id",
            Self::Location => "location",
        }
    }
}

/// The staging rule's checks of an unpinned reference manifest beyond its
/// shape (which [`FilesManifest::parse_unbound`] checks): the profile id
/// rule and every file at its reference location with no entity tag. The
/// eval Worker's commit and the operator's client run the same check, the
/// client before any upload.
pub fn check_unpinned_reference(m: &FilesManifest) -> Result<(), ReferenceRefusal> {
    if !reference_label_ok(&m.profile_id, REFERENCE_PROFILE_ID_BYTES) {
        return Err(ReferenceRefusal::ProfileId);
    }
    for f in &m.files {
        if f.bucket != REFERENCE_BUCKET || f.key != reference_file_key(&f.sha256) || f.etag.is_some() {
            return Err(ReferenceRefusal::Location);
        }
    }
    Ok(())
}

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
        Self::parse_unbound(bytes, version)
    }

    /// Parses manifest bytes and checks their shape and file table under
    /// `version`, without binding them to a package digest: for a staging
    /// request whose package is fixed only once its entity tags are pinned
    /// (`slop_ninja_reference_staging_v1`). Refused above [`MAX_MANIFEST_BYTES`] before
    /// any parse.
    pub fn parse_unbound(bytes: &[u8], version: Version) -> Result<Self, Refusal> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Refusal::Object("files manifest too large".into()));
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
