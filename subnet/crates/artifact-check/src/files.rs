//! The spec's whole check of an artifact whose header the object store
//! verified, reading its files without hashing any of them.
//!
//! Admission v2 uploads the header and each file as separate objects, each
//! verified at ingest against its committed SHA-256 (the header's own
//! digest, and each file's digest from the header). Whoever later reads the
//! files (the scorer Container's admission load, every scoring load, the
//! miner's `check` of its own object) takes the digests from the header and
//! never recomputes them:
//!
//! - the header-only file-table checks ([`check_header_for`]);
//! - every file present at exactly its declared length (a length check, not
//!   a read);
//! - the graph's streaming pre-parse ([`GraphScan`]) and its allow-listed
//!   class;
//! - the four JSON role files read at exactly their declared lengths, their
//!   schemas and the scorer's load guards, with the export manifest's
//!   digests compared against the header's ([`RoleDigests`]).
//!
//! **The one remaining hash** is the graph allow-list's structure digest: a
//! SHA-256 of the graph's structure stream, every byte except the FLOAT
//! initializer payloads ([`crate::onnx`]). Replacing it with a byte
//! comparison against each class's pinned reference structure stream is the
//! user's decision D5 (design section 15); until then it is the one
//! documented exception, named in the `no_object_hash` lint.

use std::io::Read;

use serde::Serialize;
use slop_ninja_protocol::{ArtifactFile, ArtifactHeader, Digest, FileRole, Record};

use crate::Refusal;
use crate::mapping::{check_header_for, profile_id_for};
use crate::onnx::{GraphReport, GraphScan};
use crate::roles::{RoleDigests, RoleFiles, RolesReport, check_roles_with};
use crate::spec::{self, Version};

/// What a checked artifact is: no digest in it was computed by the check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FilesReport {
    /// The spec version's id.
    pub spec: &'static str,
    /// The canonical inference-content digest, derived from the header.
    pub content_sha256: String,
    pub graph_class: &'static str,
    /// `"<spec id>/<graph class id>"`.
    pub profile_id: String,
    pub graph: GraphReport,
    pub roles: RolesReport,
    /// Graph plus head parameters.
    pub parameters: u64,
}

const JSON_ROLES: [&str; 4] = ["heads.json", "contract.json", "calibration.json", "manifest.json"];

fn read_exact_len(name: &str, len: u64, mut r: impl Read) -> Result<Vec<u8>, Refusal> {
    let cannot = || Refusal::Object(format!("{name}: cannot buffer {len} bytes"));
    let n = usize::try_from(len).map_err(|_| cannot())?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(n).map_err(|_| cannot())?;
    (&mut r)
        .take(len)
        .read_to_end(&mut buf)
        .map_err(|e| Refusal::Object(format!("{name}: {e}")))?;
    let mut extra = [0u8; 1];
    let more = r.read(&mut extra).map_err(|e| Refusal::Object(format!("{name}: {e}")))?;
    if buf.len() as u64 != len || more != 0 {
        return Err(Refusal::Object(format!("{name}: not its declared length")));
    }
    Ok(buf)
}

/// Checks the files of `header` under `version`. `open` gives a reader of
/// exactly one file's bytes and its length on storage; nothing is hashed.
pub fn check_files_with<R: Read>(
    header: &ArtifactHeader,
    version: Version,
    json_value_budget: Option<u64>,
    mut open: impl FnMut(&ArtifactFile) -> Result<(R, u64), Refusal>,
) -> Result<FilesReport, Refusal> {
    check_header_for(header, version)?;
    let content = header
        .content()
        .and_then(|c| c.digest())
        .map_err(|e| Refusal::Object(e.to_string()))?;
    let mut json: [Vec<u8>; 4] = Default::default();
    let mut graph: Option<GraphReport> = None;
    for f in &header.files {
        let (mut reader, stored_len) = open(f)?;
        if stored_len != f.len {
            return Err(Refusal::Object(format!("{}: stored length differs from the header", f.name)));
        }
        if f.name == spec::GRAPH_FILE {
            let class = version.graph_class_by_len(f.len);
            let policy = class.map_or(spec::GRAPH_POLICY, spec::graph_policy);
            let mut scan = GraphScan::new(f.len, policy);
            let mut buf = vec![0u8; 1 << 20];
            let mut seen = 0u64;
            loop {
                let n = reader.read(&mut buf).map_err(|e| Refusal::Object(format!("{}: {e}", f.name)))?;
                if n == 0 {
                    break;
                }
                seen += n as u64;
                if seen > f.len {
                    return Err(Refusal::Object(format!("{}: longer than declared", f.name)));
                }
                scan.update(&buf[..n])?;
            }
            if seen != f.len {
                return Err(Refusal::Object(format!("{}: shorter than declared", f.name)));
            }
            graph = Some(scan.finish()?);
        } else if let Some(j) = JSON_ROLES.iter().position(|n| *n == f.name) {
            json[j] = read_exact_len(&f.name, f.len, reader)?;
        } else if f.role != FileRole::Tokenizer {
            return Err(Refusal::UndeclaredFile(f.name.clone()));
        }
        // The tokenizer is checked by its header digest and length against
        // the pinned one (check_header_for); its bytes are not read here.
    }
    let graph = graph.ok_or(Refusal::MissingFile(spec::GRAPH_FILE))?;
    let class = version
        .graph_class(&graph.structure_sha256)
        .ok_or_else(|| Refusal::GraphNotAllowed { structure_sha256: graph.structure_sha256.clone() })?;
    let graph_len = header.files.iter().find(|f| f.name == spec::GRAPH_FILE).map(|f| f.len);
    if graph_len != Some(class.bytes) {
        return Err(Refusal::GraphNotAllowed { structure_sha256: graph.structure_sha256.clone() });
    }
    if !version.serves(class) {
        return Err(Refusal::GraphClassNotServed(class.id));
    }
    let hex = RoleDigests::hex_of(header)?;
    let [heads, contract, calibration, manifest] = &json;
    let roles = check_roles_with(
        &RoleFiles { heads, contract, calibration, manifest, digests: RoleDigests::of(&hex) },
        Some(class),
        version,
        json_value_budget,
    )?;
    let parameters = graph.parameters + roles.heads_parameters;
    if parameters > class.max_parameters {
        return Err(Refusal::TooManyParameters { count: parameters, max: class.max_parameters });
    }
    Ok(FilesReport {
        spec: version.id(),
        content_sha256: hex::encode(content.0),
        graph_class: class.id,
        profile_id: profile_id_for(class, version),
        graph,
        roles,
        parameters,
    })
}

/// A header built from a listing of `(name, lowercase SHA-256 hex, length)`
/// (a files manifest's, whose roles come from the file table), sorted by
/// name; refuses an undeclared name, a malformed digest or a duplicate.
pub fn header_from_listing(contract: &str, files: &[(&str, &str, u64)]) -> Result<ArtifactHeader, Refusal> {
    let mut list = Vec::with_capacity(files.len());
    for &(name, sha, len) in files {
        let role = spec::role_by_name(name).ok_or_else(|| Refusal::UndeclaredFile(name.into()))?;
        let mut digest = [0u8; 32];
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(Refusal::Object(format!("{name}: digest")));
        }
        hex::decode_to_slice(sha, &mut digest).map_err(|_| Refusal::Object(format!("{name}: digest")))?;
        list.push(ArtifactFile { name: name.into(), role: role.file_role, sha256: Digest(digest), len });
    }
    list.sort_by(|a, b| a.name.cmp(&b.name));
    if list.windows(2).any(|w| w[0].name == w[1].name) {
        return Err(Refusal::Object("duplicate file".into()));
    }
    let header = ArtifactHeader { contract: contract.into(), files: list };
    header.validate().map_err(|e| Refusal::Object(e.to_string()))?;
    Ok(header)
}

#[cfg(feature = "native")]
mod native {
    use super::*;
    use std::fs;
    use std::io::{Seek, SeekFrom};
    use std::path::Path;

    /// Checks the files of `header` in directory `dir` (each file under its
    /// header name), hashing nothing: the scorer's admission and scoring
    /// loads, after its files arrived at the header's lengths.
    pub fn check_files_for(dir: &Path, header: &ArtifactHeader, version: Version) -> Result<FilesReport, Refusal> {
        check_files_with(header, version, None, |f| {
            let path = dir.join(&f.name);
            let file = fs::File::open(&path).map_err(|e| Refusal::Object(format!("{}: {e}", path.display())))?;
            let len = file.metadata().map_err(|e| Refusal::Object(format!("{}: {e}", path.display())))?.len();
            Ok((file, len))
        })
    }

    /// The header of an artifact object file: the length prefix and the
    /// header record (a range read of at most `4 + MAX_ARTIFACT_HEADER_LEN`
    /// bytes, never the files), with the raw header bytes.
    pub fn object_header(path: &Path) -> Result<(ArtifactHeader, Vec<u8>), Refusal> {
        let io = |e: std::io::Error| Refusal::Object(format!("{}: {e}", path.display()));
        let mut f = fs::File::open(path).map_err(io)?;
        let mut prefix = [0u8; 4];
        f.read_exact(&mut prefix).map_err(io)?;
        let len = u32::from_le_bytes(prefix);
        if len == 0 || len > slop_ninja_protocol::MAX_ARTIFACT_HEADER_LEN {
            return Err(Refusal::Object("artifact header length".into()));
        }
        let bytes = read_exact_len("artifact header", u64::from(len), (&mut f).take(u64::from(len)))?;
        let header = ArtifactHeader::from_bytes(&bytes).map_err(|e| Refusal::Object(e.to_string()))?;
        Ok((header, bytes))
    }

    /// Checks an artifact object file in place under `version`: its header
    /// (a range read) and each file at its offset, hashing nothing. The
    /// object's total length must be exactly the prefix, the header and the
    /// files. For the miner's `check` of its own packed object: the digests
    /// in the header are the ones `pack` computed while writing.
    pub fn check_object_files(path: &Path, version: Version) -> Result<FilesReport, Refusal> {
        let (header, bytes) = object_header(path)?;
        let io = |e: std::io::Error| Refusal::Object(format!("{}: {e}", path.display()));
        let total = fs::metadata(path).map_err(io)?.len();
        let files_at = 4 + bytes.len() as u64;
        let files_len = header.files_len().map_err(|e| Refusal::Object(e.to_string()))?;
        if files_at.checked_add(files_len) != Some(total) {
            return Err(Refusal::Object("object length differs from its header".into()));
        }
        let mut offset = files_at;
        let mut offsets = std::collections::BTreeMap::new();
        for f in &header.files {
            offsets.insert(f.name.clone(), offset);
            offset += f.len;
        }
        check_files_with(&header, version, None, |f| {
            let mut file = fs::File::open(path).map_err(io)?;
            let at = offsets.get(&f.name).copied().ok_or_else(|| Refusal::Object("offset".into()))?;
            file.seek(SeekFrom::Start(at)).map_err(io)?;
            Ok((file.take(f.len), f.len))
        })
    }
}

#[cfg(feature = "native")]
pub use native::{check_files_for, check_object_files, object_header};
