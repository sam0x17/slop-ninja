//! The artifact object and its exact mapping to the scorer's parts manifest.
//!
//! **Object.** The protocol artifact object, `u32_le(len(H)) || H || files`,
//! with header `H` an `artifact-header` v1 record: contract
//! [`spec::CONTRACT_ID`] and exactly the six files of [`spec::ROLES`], sorted
//! by name. Its inference-content digest is what a miner commits.
//!
//! **Parts manifest.** `slop_ninja_a_artifact_files_v1`, as the scorer parses
//! it (`inference/a-container-serve/src/artifact.rs`). It is a function of
//! the object and its graph class only:
//!
//! - `schema`: [`spec::PARTS_SCHEMA`];
//! - `profile_id`: `"<spec id>/<graph class id>"`;
//! - `contract`: the header's contract;
//! - `graph`: [`spec::GRAPH_FILE`];
//! - `files`: in file-table order (the scorer's file order), each with the
//!   header's name, size and SHA-256, the role string of the file table, and
//!   its parts: consecutive slices of [`spec::PART_BYTES`] (the last one
//!   shorter), each with its SHA-256 and size;
//! - bytes: `serde_json` pretty printing with a final newline, as the scorer's
//!   packer writes them. The package digest is the SHA-256 of those bytes.
//!
//! The scorer accepts at most [`spec::MAX_PARTS_PER_FILE`] parts per file and
//! a manifest of at most [`spec::MAX_MANIFEST_BYTES`]; [`check_header`]
//! refuses a file the mapping could not express, before any byte is read.

use serde::Serialize;
use sha2::{Digest as _, Sha256};
use slop_ninja_protocol::{ArtifactHeader, ArtifactReader, FileRole, Record};

use crate::Refusal;
use crate::onnx::{GraphReport, GraphScan};
use crate::roles::{RoleFiles, RolesReport, check_roles_with, check_tokenizer};
use crate::spec::{self, GraphClass, Version};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Part {
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PartsFile {
    pub name: String,
    pub role: String,
    pub bytes: u64,
    pub sha256: String,
    pub parts: Vec<Part>,
}

/// The scorer's `ArtifactFiles`, field for field and in the same order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PartsManifest {
    pub schema: String,
    pub profile_id: String,
    pub contract: String,
    pub graph: String,
    pub files: Vec<PartsFile>,
}

impl PartsManifest {
    /// The manifest bytes the scorer receives; their SHA-256 is the package digest.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(self).expect("manifest serializes");
        bytes.push(b'\n');
        bytes
    }
}

/// Profile id of a graph class in the parts manifest (spec v1).
pub fn profile_id(class: &GraphClass) -> String {
    profile_id_for(class, Version::V1)
}

/// Profile id of a graph class in the parts manifest of `version`.
pub fn profile_id_for(class: &GraphClass, version: Version) -> String {
    format!("{}/{}", version.id(), class.id)
}

/// Builds the parts manifest from a checked header, the part digests of each
/// header file (in header order) and the graph class.
pub fn parts_manifest(
    header: &ArtifactHeader,
    parts: &[Vec<Part>],
    profile_id: &str,
) -> Result<PartsManifest, Refusal> {
    parts_manifest_for(header, parts, profile_id, Version::V1)
}

/// [`parts_manifest`] for a header checked under `version`.
pub fn parts_manifest_for(
    header: &ArtifactHeader,
    parts: &[Vec<Part>],
    profile_id: &str,
    version: Version,
) -> Result<PartsManifest, Refusal> {
    check_header_for(header, version)?;
    if parts.len() != header.files.len() {
        return Err(Refusal::Object("part list per file".into()));
    }
    let mut files = Vec::new();
    for role in &spec::ROLES {
        let (i, f) = header
            .files
            .iter()
            .enumerate()
            .find(|(_, f)| f.name == role.name)
            .ok_or(Refusal::MissingFile(role.name))?;
        let total: u64 = parts[i].iter().map(|p| p.bytes).sum();
        if total != f.len
            || parts[i].is_empty()
            || parts[i].len() as u64 > spec::MAX_PARTS_PER_FILE
            || parts[i]
                .iter()
                .any(|p| p.bytes == 0 || p.bytes > spec::PART_BYTES)
        {
            return Err(Refusal::Object(format!("{}: parts", f.name)));
        }
        files.push(PartsFile {
            name: f.name.clone(),
            role: role.scorer_role.into(),
            bytes: f.len,
            sha256: hex::encode(f.sha256.0),
            parts: parts[i].clone(),
        });
    }
    let m = PartsManifest {
        schema: spec::PARTS_SCHEMA.into(),
        profile_id: profile_id.into(),
        contract: header.contract.clone(),
        graph: spec::GRAPH_FILE.into(),
        files,
    };
    if m.to_bytes().len() as u64 > spec::MAX_MANIFEST_BYTES {
        return Err(Refusal::Object(
            "parts manifest past the scorer's manifest limit".into(),
        ));
    }
    Ok(m)
}

/// The file-table checks a header alone decides (spec v1): the contract,
/// exactly the declared names with their roles, the tokenizer byte-match, and
/// every declared length against the runtime that reads the file. The graph
/// must have the exact length of an allow-listed class the scorer serves.
pub fn check_header(header: &ArtifactHeader) -> Result<(), Refusal> {
    check_header_for(header, Version::V1)
}

/// [`check_header`] under `version`: the classes it defines and serves, and
/// the bound of spec v2 and later on the JSON role files' total length
/// (`json_budget`).
pub fn check_header_for(header: &ArtifactHeader, version: Version) -> Result<(), Refusal> {
    if header.contract != spec::CONTRACT_ID {
        return Err(Refusal::Contract(header.contract.clone()));
    }
    for f in &header.files {
        let role =
            spec::role_by_name(&f.name).ok_or_else(|| Refusal::UndeclaredFile(f.name.clone()))?;
        if f.role != role.file_role {
            return Err(Refusal::Role {
                name: f.name.clone(),
                expected: role.file_role,
            });
        }
        if f.role == FileRole::Tokenizer {
            check_tokenizer(&hex::encode(f.sha256.0), f.len)?;
        }
    }
    for role in &spec::ROLES {
        if !header.files.iter().any(|f| f.name == role.name) {
            return Err(Refusal::MissingFile(role.name));
        }
    }
    for f in &header.files {
        let role = spec::role_by_name(&f.name).expect("checked");
        if f.len == 0 || f.len > role.max_bytes {
            return Err(Refusal::FileSize {
                name: f.name.clone(),
                len: f.len,
                max: role.max_bytes,
            });
        }
        if f.name == spec::GRAPH_FILE {
            match version.graph_class_by_len(f.len) {
                None => return Err(Refusal::GraphSize(f.len)),
                Some(c) if !version.serves(c) => return Err(Refusal::GraphClassNotServed(c.id)),
                Some(_) => {}
            }
        }
    }
    if let Some(max) = version.max_json_bytes() {
        let need = json_buffer_bytes(header);
        if need > max {
            return Err(Refusal::JsonBudget { need, budget: max });
        }
    }
    Ok(())
}

/// [`check_header_for`] on a file listing (name, lowercase SHA-256 hex,
/// length) instead of a protocol header: for a holder of the scorer's parts
/// manifest, which lists the same files with the scorer's roles. Each name's
/// header role comes from the file table.
pub fn check_listing(
    contract: &str,
    files: &[(&str, &str, u64)],
    version: Version,
) -> Result<(), Refusal> {
    let mut list = Vec::with_capacity(files.len());
    for &(name, sha, len) in files {
        let role = spec::role_by_name(name).ok_or_else(|| Refusal::UndeclaredFile(name.into()))?;
        let mut digest = [0u8; 32];
        hex::decode_to_slice(sha, &mut digest)
            .map_err(|_| Refusal::Object(format!("{name}: digest")))?;
        list.push(slop_ninja_protocol::ArtifactFile {
            name: name.into(),
            role: role.file_role,
            sha256: slop_ninja_protocol::Digest(digest),
            len,
        });
    }
    list.sort_by(|a, b| a.name.cmp(&b.name));
    if list.windows(2).any(|w| w[0].name == w[1].name) {
        return Err(Refusal::Object("duplicate file".into()));
    }
    check_header_for(
        &ArtifactHeader {
            contract: contract.into(),
            files: list,
        },
        version,
    )
}

/// Bytes [`ObjectCheck`] buffers for the JSON role files of `header`: their
/// declared lengths. Known from the header, before any file byte.
pub fn json_buffer_bytes(header: &ArtifactHeader) -> u64 {
    header
        .files
        .iter()
        .filter(|f| JSON_ROLES.contains(&f.name.as_str()))
        .map(|f| f.len)
        .sum()
}

/// What an admitted object is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ObjectReport {
    /// The spec version's id.
    pub spec: &'static str,
    /// Plain SHA-256 and length of the whole object.
    pub object_sha256: String,
    pub object_len: u64,
    /// The canonical inference-content digest (what the miner commits).
    pub content_sha256: String,
    pub graph_class: &'static str,
    pub graph: GraphReport,
    pub roles: RolesReport,
    /// Graph plus head parameters.
    pub parameters: u64,
    /// SHA-256 of the parts manifest bytes.
    pub package_sha256: String,
    #[serde(skip)]
    pub parts_manifest: PartsManifest,
}

/// Index of the JSON role buffers.
const JSON_ROLES: [&str; 4] = [
    "heads.json",
    "contract.json",
    "calibration.json",
    "manifest.json",
];

/// Streams a whole artifact object and runs every check of the spec.
///
/// Memory: the header, the four JSON role files, one part hasher and the
/// graph walk. The graph and tokenizer are never buffered. Each JSON role
/// file is buffered at exactly its declared length ([`json_buffer_bytes`]),
/// allocated with `try_reserve_exact` when its first byte arrives; a caller
/// with a memory budget passes it to [`ObjectCheck::with_json_budget`] and
/// the header is refused before any file byte if the files need more. Parsing
/// the JSON files is bounded by their value cost, checked before any parse
/// against the spec's value budget and the caller's
/// ([`ObjectCheck::with_json_value_budget`]).
pub struct ObjectCheck {
    version: Version,
    reader: ArtifactReader,
    head: Vec<u8>,
    header: Option<ArtifactHeader>,
    index: usize,
    remaining: u64,
    part: Sha256,
    part_len: u64,
    parts: Vec<Vec<Part>>,
    graph: Option<GraphScan<'static>>,
    graph_report: Option<GraphReport>,
    json: [Vec<u8>; 4],
    json_budget: Option<u64>,
    json_value_budget: Option<u64>,
    failed: Option<Refusal>,
}

impl Default for ObjectCheck {
    fn default() -> Self {
        Self::new()
    }
}

impl ObjectCheck {
    /// An object check under spec v1.
    pub fn new() -> Self {
        Self::for_spec(Version::V1)
    }

    /// An object check under `version`.
    pub fn for_spec(version: Version) -> Self {
        Self {
            version,
            reader: ArtifactReader::new(),
            head: Vec::new(),
            header: None,
            index: 0,
            remaining: 0,
            part: Sha256::new(),
            part_len: 0,
            parts: Vec::new(),
            graph: None,
            graph_report: None,
            json: Default::default(),
            json_budget: None,
            json_value_budget: None,
            failed: None,
        }
    }

    /// An object check that refuses a header whose JSON role files need more
    /// than `budget` bytes of buffer (`json_budget`).
    pub fn with_json_budget(budget: u64) -> Self {
        Self::for_spec_with_json_budget(Version::V1, budget)
    }

    /// An object check under `version` with the caller's JSON budget (on top
    /// of the version's own bound, if any).
    pub fn for_spec_with_json_budget(version: Version, budget: u64) -> Self {
        Self {
            json_budget: Some(budget),
            ..Self::for_spec(version)
        }
    }

    /// This check with the caller's bound on the JSON role files' summed
    /// value cost (`json_value_budget`, on top of the version's own bound, if
    /// any): a caller with a memory budget for parsing passes it here, since
    /// the byte budget alone does not bound what a parse holds.
    pub fn with_json_value_budget(mut self, budget: u64) -> Self {
        self.json_value_budget = Some(budget);
        self
    }

    /// The spec version this check applies.
    pub fn version(&self) -> Version {
        self.version
    }

    /// The header, once its bytes have arrived and passed the file-table checks.
    pub fn header(&self) -> Option<&ArtifactHeader> {
        self.header.as_ref()
    }

    pub fn update(&mut self, chunk: &[u8]) -> Result<(), Refusal> {
        if let Some(r) = &self.failed {
            return Err(r.clone());
        }
        let result = self.consume(chunk);
        if let Err(r) = &result {
            self.failed = Some(r.clone());
        }
        result
    }

    fn consume(&mut self, chunk: &[u8]) -> Result<(), Refusal> {
        // The protocol reader verifies every file digest and the header bound;
        // it sees each chunk first.
        self.reader
            .update(chunk)
            .map_err(|e| Refusal::Object(e.to_string()))?;
        let mut rest = chunk;
        while self.header.is_none() && !rest.is_empty() {
            let need = if self.head.len() < 4 {
                4
            } else {
                4 + u32::from_le_bytes(self.head[..4].try_into().expect("4 bytes")) as usize
            };
            let take = (need - self.head.len()).min(rest.len());
            self.head.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.head.len() >= 4 {
                let need =
                    4 + u32::from_le_bytes(self.head[..4].try_into().expect("4 bytes")) as usize;
                if self.head.len() == need {
                    self.open_header()?;
                }
            }
        }
        if self.header.is_some() {
            self.files(rest)
        } else {
            Ok(())
        }
    }

    fn open_header(&mut self) -> Result<(), Refusal> {
        let header = ArtifactHeader::from_bytes(&self.head[4..])
            .map_err(|e| Refusal::Object(e.to_string()))?;
        check_header_for(&header, self.version)?;
        if let Some(budget) = self.json_budget {
            let need = json_buffer_bytes(&header);
            if need > budget {
                return Err(Refusal::JsonBudget { need, budget });
            }
        }
        self.parts = vec![Vec::new(); header.files.len()];
        self.index = 0;
        self.remaining = header.files.first().map_or(0, |f| f.len);
        self.head = Vec::new();
        self.header = Some(header);
        self.start_file();
        Ok(())
    }

    fn start_file(&mut self) {
        let header = self.header.as_ref().expect("header");
        let Some(f) = header.files.get(self.index) else {
            return;
        };
        if f.name == spec::GRAPH_FILE {
            // The header check found the class of this length; its parameter
            // bound applies from the walk's first payload.
            let class = self.version.graph_class_by_len(f.len);
            let policy = class.map_or(spec::GRAPH_POLICY, spec::graph_policy);
            self.graph = Some(GraphScan::new(f.len, policy));
        }
        if let Some(j) = JSON_ROLES.iter().position(|n| *n == f.name) {
            // Allocated at its first byte, at exactly the declared length.
            self.json[j] = Vec::new();
        }
    }

    fn files(&mut self, mut rest: &[u8]) -> Result<(), Refusal> {
        while !rest.is_empty() {
            let header = self.header.as_ref().expect("header");
            let Some(f) = header.files.get(self.index) else {
                return Err(Refusal::Object("bytes after the last file".into()));
            };
            let name = f.name.clone();
            let to_part = spec::PART_BYTES - self.part_len;
            let take = self.remaining.min(to_part).min(rest.len() as u64) as usize;
            let bytes = &rest[..take];
            self.part.update(bytes);
            self.part_len += take as u64;
            if name == spec::GRAPH_FILE {
                self.graph.as_mut().expect("graph walk").update(bytes)?;
            } else if let Some(j) = JSON_ROLES.iter().position(|n| *n == name) {
                let buf = &mut self.json[j];
                if buf.capacity() == 0 && take > 0 {
                    let len = self.remaining;
                    let cannot = || Refusal::Object(format!("{name}: cannot buffer {len} bytes"));
                    let n = usize::try_from(len).map_err(|_| cannot())?;
                    buf.try_reserve_exact(n).map_err(|_| cannot())?;
                }
                buf.extend_from_slice(bytes);
            }
            self.remaining -= take as u64;
            rest = &rest[take..];
            if self.part_len == spec::PART_BYTES || self.remaining == 0 {
                let digest = std::mem::take(&mut self.part).finalize();
                self.parts[self.index].push(Part {
                    sha256: hex::encode(digest),
                    bytes: self.part_len,
                });
                self.part_len = 0;
            }
            if self.remaining == 0 {
                if name == spec::GRAPH_FILE {
                    let scan = self.graph.take().expect("graph walk");
                    self.graph_report = Some(scan.finish()?);
                }
                self.index += 1;
                let header = self.header.as_ref().expect("header");
                self.remaining = header.files.get(self.index).map_or(0, |f| f.len);
                self.start_file();
            }
        }
        Ok(())
    }

    /// Ends the object and runs the checks that need every file.
    pub fn finish(mut self) -> Result<ObjectReport, Refusal> {
        if let Some(r) = self.failed.take() {
            return Err(r);
        }
        let verified = self
            .reader
            .finish()
            .map_err(|e| Refusal::Object(e.to_string()))?;
        let header = self
            .header
            .take()
            .ok_or_else(|| Refusal::Object("no header".into()))?;
        let graph = self
            .graph_report
            .take()
            .ok_or(Refusal::MissingFile(spec::GRAPH_FILE))?;
        let class = self
            .version
            .graph_class(&graph.structure_sha256)
            .ok_or_else(|| Refusal::GraphNotAllowed {
                structure_sha256: graph.structure_sha256.clone(),
            })?;
        let graph_len = header
            .files
            .iter()
            .find(|f| f.name == spec::GRAPH_FILE)
            .map(|f| f.len);
        if graph_len != Some(class.bytes) {
            return Err(Refusal::GraphNotAllowed {
                structure_sha256: graph.structure_sha256.clone(),
            });
        }
        if !self.version.serves(class) {
            return Err(Refusal::GraphClassNotServed(class.id));
        }
        let [heads, contract, calibration, manifest] = &self.json;
        let roles = check_roles_with(
            &RoleFiles {
                heads,
                contract,
                calibration,
                manifest,
            },
            Some(class),
            self.version,
            self.json_value_budget,
        )?;
        let parameters = graph.parameters + roles.heads_parameters;
        if parameters > class.max_parameters {
            return Err(Refusal::TooManyParameters {
                count: parameters,
                max: class.max_parameters,
            });
        }
        let parts_manifest = parts_manifest_for(
            &header,
            &self.parts,
            &profile_id_for(class, self.version),
            self.version,
        )?;
        let package_sha256 = crate::sha256_hex(&parts_manifest.to_bytes());
        Ok(ObjectReport {
            spec: self.version.id(),
            object_sha256: hex::encode(verified.sha256.0),
            object_len: verified.len,
            content_sha256: hex::encode(verified.content.0),
            graph_class: class.id,
            graph,
            roles,
            parameters,
            package_sha256,
            parts_manifest,
        })
    }
}
