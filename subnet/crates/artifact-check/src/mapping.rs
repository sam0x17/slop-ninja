//! The artifact object's header checks and the whole-object check.
//!
//! **Object.** The protocol artifact object, `u32_le(len(H)) || H || files`,
//! with header `H` an `artifact-header` v1 record: contract
//! [`spec::CONTRACT_ID`] and exactly the six files of [`spec::ROLES`], sorted
//! by name. Its inference-content digest is what a miner commits.
//!
//! [`check_header_for`] decides everything a header alone can, before any
//! file byte. [`ObjectCheck`] streams a whole reassembled object for tools
//! that hold one (the command-line `check-object`, tests); admission and
//! scoring never use it: they check the files of a store-verified header
//! with [`crate::files`], hashing nothing.

use serde::Serialize;
use slop_ninja_protocol::{ArtifactHeader, ArtifactReader, FileRole, Record};

use crate::Refusal;
use crate::files::{graph_walk, walked_class};
use crate::onnx::{GraphReport, GraphScan};
use crate::roles::{RoleDigests, RoleFiles, RolesReport, check_roles_with, check_tokenizer};
use crate::spec::{self, GraphClass, Version};

/// Profile id of a graph class (spec v1).
pub fn profile_id(class: &GraphClass) -> String {
    profile_id_for(class, Version::V1)
}

/// Profile id of a graph class under `version`.
pub fn profile_id_for(class: &GraphClass, version: Version) -> String {
    format!("{}/{}", version.id(), class.id)
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
    /// `"<spec id>/<graph class id>"`.
    pub profile_id: String,
}

/// Index of the JSON role buffers.
const JSON_ROLES: [&str; 4] = [
    "heads.json",
    "contract.json",
    "calibration.json",
    "manifest.json",
];

/// Streams a whole artifact object and runs every check of the spec. A tool
/// check: the protocol reader verifies the object and every file against
/// the header as it streams.
///
/// Memory: the header, the four JSON role files and the graph walk. The graph and tokenizer are never buffered. Each JSON role
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
            // bound applies from the walk's first payload, and its reference
            // structure stream is compared as the walk goes.
            self.graph = Some(graph_walk(self.version, f.len));
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
            let take = self.remaining.min(rest.len() as u64) as usize;
            let bytes = &rest[..take];
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
        let graph_len = header
            .files
            .iter()
            .find(|f| f.name == spec::GRAPH_FILE)
            .map_or(0, |f| f.len);
        let class = walked_class(self.version, graph_len, &graph)?;
        if !self.version.serves(class) {
            return Err(Refusal::GraphClassNotServed(class.id));
        }
        let [heads, contract, calibration, manifest] = &self.json;
        let hex = RoleDigests::hex_of(&header)?;
        let roles = check_roles_with(
            &RoleFiles {
                heads,
                contract,
                calibration,
                manifest,
                digests: RoleDigests::of(&hex),
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
        Ok(ObjectReport {
            spec: self.version.id(),
            object_sha256: hex::encode(verified.sha256.0),
            object_len: verified.len,
            content_sha256: hex::encode(verified.content.0),
            graph_class: class.id,
            graph,
            roles,
            parameters,
            profile_id: profile_id_for(class, self.version),
        })
    }
}
