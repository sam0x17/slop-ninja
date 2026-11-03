//! Artifact objects and their canonical inference-content identity.
//!
//! An artifact object is `u32_le(len(H)) || H || files`, where `H` is the
//! canonical bytes of an [`ArtifactHeader`] and the files follow in header
//! order with nothing after the last. The header lists each file's name, role,
//! SHA-256 and length, sorted by name, so one set of files has exactly one
//! object. [`ArtifactHeader::content`] derives the [`InferenceContent`] from
//! the header: the contract and the inference files by role, digest and
//! length. File names, owner metadata, publisher signatures and reference test
//! vectors are excluded.
//!
//! [`ArtifactReader`] streams an object, checks every file against the header
//! and the whole object's SHA-256 and length, and yields a
//! [`VerifiedArtifact`]. The coordinator uses it at admission and validators
//! use it on published objects, so the inference-content digest a miner
//! commits on chain is checked against the artifact bytes rather than
//! asserted. Qualification of the files themselves is separate.

use sha2::{Digest as _, Sha256};

use crate::codec::{Encoder, canonical_struct};
use crate::records_v2::{tagged_enum, validate_contract};
use crate::sign::KeyRole;
use crate::{Component, ComponentRole, Digest, Error, InferenceContent, Record, Result};

/// Largest header accepted, in bytes.
pub const MAX_ARTIFACT_HEADER_LEN: u32 = 1 << 20;
/// Longest file name, in bytes.
pub const MAX_FILE_NAME_LEN: usize = 255;

tagged_enum! {
    /// Role of one file in an artifact object. The first five are inference
    /// components; the rest are excluded from content identity.
    pub enum FileRole ("file_role") {
        Graph = 1,
        Weights = 2,
        Tokenizer = 3,
        Calibration = 4,
        OutputSchema = 5,
        ReferenceVectors = 6,
        OwnerMetadata = 7,
        PublisherSignature = 8,
    }
}

impl FileRole {
    /// The inference component role, or `None` for a file that content
    /// identity excludes.
    pub fn component(self) -> Option<ComponentRole> {
        match self {
            Self::Graph => Some(ComponentRole::Graph),
            Self::Weights => Some(ComponentRole::Weights),
            Self::Tokenizer => Some(ComponentRole::Tokenizer),
            Self::Calibration => Some(ComponentRole::Calibration),
            Self::OutputSchema => Some(ComponentRole::OutputSchema),
            Self::ReferenceVectors | Self::OwnerMetadata | Self::PublisherSignature => None,
        }
    }
}

canonical_struct! {
    /// One file of an artifact object.
    pub struct ArtifactFile {
        /// Relative path: `/`-separated segments of `[A-Za-z0-9_.-]`, none
        /// of them `.` or `..`, at most 255 bytes.
        pub name: String,
        pub role: FileRole,
        pub sha256: Digest,
        /// Nonzero.
        pub len: u64,
    }
}

canonical_struct! {
    /// The file table at the start of an artifact object.
    pub struct ArtifactHeader {
        /// The output contract the artifact implements.
        pub contract: String,
        /// Sorted by name, without duplicates; the object holds the files in
        /// this order.
        pub files: Vec<ArtifactFile>,
    }
}

fn validate_file_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= MAX_FILE_NAME_LEN
        && name.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        });
    if !valid {
        return Err(Error::Invalid("artifact file name"));
    }
    Ok(())
}

impl Record for ArtifactHeader {
    const KIND: &'static str = "artifact-header";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = None;
    fn validate(&self) -> Result<()> {
        validate_contract(&self.contract)?;
        let names: Vec<&str> = self.files.iter().map(|f| f.name.as_str()).collect();
        crate::records::strictly_increasing(&names, "artifact files")?;
        for file in &self.files {
            validate_file_name(&file.name)?;
            if file.len == 0 {
                return Err(Error::Invalid("empty artifact file"));
            }
        }
        self.files_len()?;
        self.derive_content().map(|_| ())
    }
}

impl ArtifactHeader {
    fn derive_content(&self) -> Result<InferenceContent> {
        let mut components: Vec<Component> = self
            .files
            .iter()
            .filter_map(|f| {
                f.role.component().map(|role| Component {
                    role,
                    sha256: f.sha256,
                    len: f.len,
                })
            })
            .collect();
        components.sort();
        let content = InferenceContent {
            contract: self.contract.clone(),
            components,
        };
        content.validate()?;
        Ok(content)
    }

    /// The canonical inference content of every object with this header.
    pub fn content(&self) -> Result<InferenceContent> {
        self.validate()?;
        self.derive_content()
    }
    /// Total length of the files after the header.
    pub fn files_len(&self) -> Result<u64> {
        self.files.iter().try_fold(0u64, |total, f| {
            total
                .checked_add(f.len)
                .ok_or(Error::Overflow("artifact length"))
        })
    }
}

/// Assembles the canonical artifact object for `files` of
/// `(name, role, bytes)`, in any order. Miner tooling and fixtures use it; the
/// coordinator and validators only read objects.
pub fn artifact_object(contract: &str, files: &[(&str, FileRole, &[u8])]) -> Result<Vec<u8>> {
    let mut sorted: Vec<_> = files.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let header = ArtifactHeader {
        contract: contract.into(),
        files: sorted
            .iter()
            .map(|(name, role, bytes)| ArtifactFile {
                name: (*name).into(),
                role: *role,
                sha256: crate::object_sha256(bytes),
                len: bytes.len() as u64,
            })
            .collect(),
    };
    let header_bytes = header.to_bytes()?;
    let header_len = u32::try_from(header_bytes.len())
        .ok()
        .filter(|&len| len <= MAX_ARTIFACT_HEADER_LEN)
        .ok_or(Error::LengthLimit {
            field: "artifact header",
            len: header_bytes.len() as u64,
        })?;
    let mut e = Encoder::new();
    e.u32(header_len);
    e.raw(&header_bytes);
    for (_, _, bytes) in sorted {
        e.raw(bytes);
    }
    Ok(e.into_bytes())
}

/// An artifact object whose every file matched its header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedArtifact {
    /// Plain SHA-256 of the whole object, as an object store reports it.
    pub sha256: Digest,
    pub len: u64,
    pub header: ArtifactHeader,
    /// Digest of the derived [`InferenceContent`]: the canonical
    /// inference-content digest.
    pub content: Digest,
}

impl VerifiedArtifact {
    /// Verifies a whole object held in memory.
    pub fn from_bytes(object: &[u8]) -> Result<Self> {
        let mut reader = ArtifactReader::new();
        reader.update(object)?;
        reader.finish()
    }
}

enum ReadState {
    /// Buffering the length prefix and header.
    Header(Vec<u8>),
    /// Hashing file `index`, of which `remaining` bytes are still due. The
    /// files begin at object offset `files_at` (the prefix and the header).
    Files {
        header: ArtifactHeader,
        files_at: u64,
        index: usize,
        remaining: u64,
        file: Sha256,
    },
    /// An error was returned; the reader accepts nothing more.
    Failed,
}

/// Streams an artifact object in chunks of any size, so a large object never
/// needs to be held in memory. After an error the reader refuses every later
/// call.
pub struct ArtifactReader {
    object: Sha256,
    len: u64,
    state: ReadState,
}

impl Default for ArtifactReader {
    fn default() -> Self {
        Self::new()
    }
}

impl ArtifactReader {
    pub fn new() -> Self {
        Self {
            object: Sha256::new(),
            len: 0,
            state: ReadState::Header(Vec::new()),
        }
    }

    /// Consumes the next `chunk` of the object.
    pub fn update(&mut self, chunk: &[u8]) -> Result<()> {
        let result = self.consume(chunk);
        if result.is_err() {
            self.state = ReadState::Failed;
        }
        result
    }

    fn consume(&mut self, chunk: &[u8]) -> Result<()> {
        if matches!(self.state, ReadState::Failed) {
            return Err(Error::Invalid("artifact reader failed"));
        }
        self.object.update(chunk);
        self.len = self
            .len
            .checked_add(chunk.len() as u64)
            .ok_or(Error::Overflow("artifact length"))?;
        let mut rest = chunk;
        loop {
            match &mut self.state {
                ReadState::Failed => unreachable!("checked above"),
                ReadState::Header(buffer) => {
                    let need = if buffer.len() < 4 {
                        4
                    } else {
                        let len = u32::from_le_bytes(buffer[..4].try_into().expect("4 bytes"));
                        if len > MAX_ARTIFACT_HEADER_LEN {
                            return Err(Error::LengthLimit {
                                field: "artifact header",
                                len: len.into(),
                            });
                        }
                        // No header encodes in zero bytes; refusing it here
                        // also keeps every later step consuming input.
                        if len == 0 {
                            return Err(Error::Invalid("artifact header length"));
                        }
                        4 + len as usize
                    };
                    let take = (need - buffer.len()).min(rest.len());
                    buffer.extend_from_slice(&rest[..take]);
                    rest = &rest[take..];
                    if buffer.len() < need {
                        return Ok(());
                    }
                    if need == 4 {
                        // The prefix is complete and names a non-empty
                        // header: the next pass buffers it.
                        continue;
                    }
                    let header = ArtifactHeader::from_bytes(&buffer[4..])?;
                    let remaining = header.files.first().map_or(0, |f| f.len);
                    self.state = ReadState::Files {
                        header,
                        files_at: need as u64,
                        index: 0,
                        remaining,
                        file: Sha256::new(),
                    };
                }
                ReadState::Files {
                    header,
                    index,
                    remaining,
                    file,
                    ..
                } => {
                    if *index == header.files.len() {
                        if rest.is_empty() {
                            return Ok(());
                        }
                        return Err(Error::TrailingBytes);
                    }
                    let take = usize::try_from(*remaining)
                        .unwrap_or(usize::MAX)
                        .min(rest.len());
                    file.update(&rest[..take]);
                    rest = &rest[take..];
                    *remaining -= take as u64;
                    if *remaining == 0 {
                        let digest = Digest(core::mem::take(file).finalize().into());
                        if digest != header.files[*index].sha256 {
                            return Err(Error::Mismatch("artifact file"));
                        }
                        *index += 1;
                        if let Some(next) = header.files.get(*index) {
                            *remaining = next.len;
                        }
                    }
                    if rest.is_empty() && *index < header.files.len() {
                        return Ok(());
                    }
                }
            }
        }
    }

    /// The header once it has been read, with the object offset at which
    /// its files begin (the length prefix and the header before them).
    pub fn header(&self) -> Option<(&ArtifactHeader, u64)> {
        match &self.state {
            ReadState::Files {
                header, files_at, ..
            } => Some((header, *files_at)),
            _ => None,
        }
    }

    /// Ends the object: every file must be complete.
    pub fn finish(self) -> Result<VerifiedArtifact> {
        match self.state {
            ReadState::Files { header, index, .. } if index == header.files.len() => {
                let content = header.content()?.digest()?;
                Ok(VerifiedArtifact {
                    sha256: Digest(self.object.finalize().into()),
                    len: self.len,
                    header,
                    content,
                })
            }
            ReadState::Failed => Err(Error::Invalid("artifact reader failed")),
            _ => Err(Error::UnexpectedEnd),
        }
    }
}
