//! Admission bounds and framing of an uploaded artifact object, shared by
//! the eval Worker (which decides) and the key-free upload Worker (which
//! takes the untrusted upload traffic and frames the uploaded objects).
//!
//! - [`SpecBounds`]: the largest file total and the largest object an
//!   artifact of a spec can have, derived from the spec alone.
//! - [`Framer`]: the streaming derivation of an uploaded object's scorer
//!   slices and parts manifest. The protocol reader is the only parser: it
//!   decodes the header record once and checks the object and every file
//!   against it. The upload Worker runs it; the eval Worker never decodes an
//!   uploaded object (finding 24 of the first admission review).
//! - [`FrameOutcome`]: what framing derived, as the bounded JSON message the
//!   upload Worker returns to the eval Worker's framing step. It carries the
//!   header's contract and the derived content digest, never the header
//!   record itself, so the eval Worker decodes no artifact bytes.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use slop_ninja_protocol::{ArtifactReader, Digest, MAX_ARTIFACT_HEADER_LEN, VerifiedArtifact};

use crate::mapping::{self, Part};
use crate::spec::{self, Version};

// ------------------------------------------------------------- spec bounds

/// The object bound admission derives from a cohort's artifact spec
/// (spec v2 or v3): the largest file total and the largest object any
/// artifact of the spec can have. Nothing here is tuned: the graph term is
/// the largest served class's fixed length, the tokenizer is pinned, the
/// JSON role files share the spec's own budget, and the header is bounded by
/// the protocol record's limit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SpecBounds {
    pub spec_id: &'static str,
    pub spec_sha256: String,
    /// Largest graph class length the spec serves.
    pub graph_bytes: u64,
    /// The six files' largest total: graph, tokenizer and the JSON budget.
    pub files_bytes: u64,
    /// `4 + MAX_ARTIFACT_HEADER_LEN + files_bytes`.
    pub object_bytes: u64,
}

impl SpecBounds {
    pub fn of(v: Version) -> Option<Self> {
        if !engine_admits(v) {
            return None;
        }
        let graph = graph_bytes(v)?;
        let files = files_bytes(v)?;
        let object = files.checked_add(4 + u64::from(MAX_ARTIFACT_HEADER_LEN))?;
        Some(Self {
            spec_id: v.id(),
            spec_sha256: v.spec_sha256(),
            graph_bytes: graph,
            files_bytes: files,
            object_bytes: object,
        })
    }

    pub fn of_id(id: &str) -> Option<Self> {
        Version::from_id(id).and_then(Self::of)
    }
}

/// Largest graph class length spec `v` serves.
fn graph_bytes(v: Version) -> Option<u64> {
    v.classes().iter().filter(|c| v.serves(c)).map(|c| c.bytes).max()
}

/// The six files' largest total under spec `v` (graph, tokenizer and the JSON
/// budget), from its class and JSON bounds alone: unlike the spec digest it
/// never rebuilds the spec record, so a lane's limits read it per report.
pub fn files_bytes(v: Version) -> Option<u64> {
    if !engine_admits(v) {
        return None;
    }
    graph_bytes(v)?.checked_add(spec::TOKENIZER_BYTES)?.checked_add(v.max_json_bytes()?)
}

/// The specs an admission run can load by: every admission run names the
/// cohort's spec digest on its load (`AScorer` `/admit`), so a scorer that
/// lists the spec admits by it (spec v3 admits the span tower class).
pub const ADMISSION_SPECS: &[&str] = &[spec::SPEC_ID_V2, spec::SPEC_ID_V3];

/// Specs the scorer admits a load by: those that bound the JSON role files.
fn engine_admits(v: Version) -> bool {
    v.max_json_bytes().is_some() && v.max_json_value_bytes().is_some()
}

/// The largest object any admission spec allows ([`ADMISSION_SPECS`]): the
/// upload Worker's spec-derived bound on a declared body before it forwards
/// anything (the eval Worker checks each part's exact length against the
/// cohort's own plan).
pub fn largest_admission_object() -> u64 {
    ADMISSION_SPECS
        .iter()
        .filter_map(|id| SpecBounds::of_id(id))
        .map(|b| b.object_bytes)
        .max()
        .unwrap_or(0)
}

// ----------------------------------------------------------------- framing

/// The scorer parts and manifest of an uploaded object, derived while it
/// streams. The protocol reader is the only parser: it decodes the header
/// record once (the length prefix, then at most `MAX_ARTIFACT_HEADER_LEN`
/// bytes) and checks the object and every file against it; the framer takes
/// the header and the files' offset from the reader and cuts each file into
/// the scorer's content-addressed slices. A header listing more files than
/// the spec's file table is refused before any slice is cut.
pub struct Framer {
    /// The whole object's SHA-256 and length, kept to the end even after a
    /// refusal, so a changed part reports `object_mismatch`.
    object: Sha256,
    len: u64,
    failed: Option<FrameError>,
    reader: ArtifactReader,
    /// Per header file: its length; filled once the reader has the header.
    files: Vec<u64>,
    /// The object offset of the first file (`None` until the header is read).
    files_at: Option<u64>,
    slices: Vec<Vec<Part>>,
    slice: Sha256,
    slice_len: u64,
    file: usize,
    file_left: u64,
}

/// One content-addressed slice to copy from the object to the blob store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slice {
    pub sha256: String,
    pub offset: u64,
    pub len: u64,
}

/// Why framing refused an object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The protocol reader refused it (`artifact_invalid`, the reason).
    Reader(String),
    /// A header-only check of the spec refused it (`check_failed`, the code).
    Check(String),
}

/// A reader refusal's text: the error's `Debug` form, or its stable code
/// if that form were ever longer than a refusal's room
/// ([`refusal_text_bytes`]).
fn reader_reason(e: &slop_ninja_protocol::Error) -> String {
    let text = format!("{e:?}");
    if text.len() <= refusal_text_bytes() { text } else { e.code().to_owned() }
}

/// What framing derived.
#[derive(Clone, Debug)]
pub struct Framed {
    pub artifact: VerifiedArtifact,
    pub manifest: Vec<u8>,
    pub package_sha256: String,
    /// The graph file's SHA-256 (the record's model digest).
    pub model_sha256: String,
    pub slices: Vec<Slice>,
}

impl Default for Framer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framer {
    pub fn new() -> Self {
        Self {
            object: Sha256::new(),
            len: 0,
            failed: None,
            reader: ArtifactReader::new(),
            files: Vec::new(),
            files_at: None,
            slices: Vec::new(),
            slice: Sha256::new(),
            slice_len: 0,
            file: 0,
            file_left: 0,
        }
    }

    /// Feeds the next chunk. A refusal is kept for [`Framer::finish`]; the
    /// object's digest is computed to the end either way. Every call consumes
    /// its whole chunk.
    pub fn update(&mut self, chunk: &[u8]) {
        let start = self.len;
        self.object.update(chunk);
        self.len = self.len.saturating_add(chunk.len() as u64);
        if self.failed.is_some() {
            return;
        }
        if let Err(e) = self.reader.update(chunk) {
            self.failed = Some(FrameError::Reader(reader_reason(&e)));
            return;
        }
        if self.files_at.is_none() {
            let Some((h, at)) = self.reader.header() else { return };
            // The spec's file table names every file an artifact may hold;
            // a longer list has an undeclared one (the code the header check
            // would give), refused before any slice is cut.
            if h.files.len() > spec::ROLES.len() {
                self.failed = Some(FrameError::Check("undeclared_file".into()));
                return;
            }
            self.files = h.files.iter().map(|f| f.len).collect();
            self.slices = vec![Vec::new(); self.files.len()];
            self.file_left = self.files.first().copied().unwrap_or(0);
            self.files_at = Some(at);
        }
        let at = self.files_at.unwrap_or(u64::MAX);
        // The part of this chunk past the header (the reader refuses
        // trailing bytes, so every byte here belongs to a file).
        let skip = usize::try_from(at.saturating_sub(start)).unwrap_or(usize::MAX).min(chunk.len());
        let mut rest = chunk.get(skip..).unwrap_or_default();
        while !rest.is_empty() && self.file < self.files.len() {
            let room = (spec::PART_BYTES - self.slice_len).min(self.file_left);
            let take = usize::try_from(room).unwrap_or(usize::MAX).min(rest.len());
            let (head, tail) = rest.split_at(take);
            self.slice.update(head);
            self.slice_len += take as u64;
            self.file_left -= take as u64;
            rest = tail;
            if self.slice_len == spec::PART_BYTES || self.file_left == 0 {
                self.close_slice();
            }
            if self.file_left == 0 {
                self.file += 1;
                self.file_left = self.files.get(self.file).copied().unwrap_or(0);
            }
        }
    }

    fn close_slice(&mut self) {
        let sha = hex::encode(std::mem::take(&mut self.slice).finalize());
        if let Some(v) = self.slices.get_mut(self.file) {
            v.push(Part {
                sha256: sha,
                bytes: self.slice_len,
            });
        }
        self.slice_len = 0;
    }

    /// Finishes framing under `version`: the object must verify, its header
    /// pass the spec's file-table checks, and its graph length name a class.
    /// The object's SHA-256 and length, and what framing derived from it.
    pub fn finish(self, version: Version) -> (Digest, u64, Result<Framed, FrameError>) {
        let digest = Digest(self.object.clone().finalize().into());
        let len = self.len;
        (digest, len, self.derive(version))
    }

    fn derive(self, version: Version) -> Result<Framed, FrameError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        let files_at = self.files_at.ok_or_else(|| FrameError::Reader("no header".into()))?;
        let artifact = self.reader.finish().map_err(|e| FrameError::Reader(reader_reason(&e)))?;
        let header = &artifact.header;
        // The header-only file-table checks first (contract, names, roles,
        // tokenizer, lengths): their codes are the spec's.
        mapping::check_header_for(header, version).map_err(|r| FrameError::Check(r.code().to_owned()))?;
        let graph = header
            .files
            .iter()
            .find(|f| f.name == spec::GRAPH_FILE)
            .ok_or_else(|| FrameError::Check("missing_file".into()))?;
        let class = version
            .graph_class_by_len(graph.len)
            .ok_or_else(|| FrameError::Check("graph_size".into()))?;
        let profile = mapping::profile_id_for(class, version);
        let m = mapping::parts_manifest_for(header, &self.slices, &profile, version)
            .map_err(|r| FrameError::Check(r.code().to_owned()))?;
        let manifest = m.to_bytes();
        let package_sha256 = hex::encode(Sha256::digest(&manifest));
        let model_sha256 = graph.sha256.hex();
        let mut slices = Vec::new();
        let mut at = files_at;
        for parts in &self.slices {
            for p in parts {
                slices.push(Slice {
                    sha256: p.sha256.clone(),
                    offset: at,
                    len: p.bytes,
                });
                at += p.bytes;
            }
        }
        Ok(Framed {
            model_sha256,
            artifact,
            manifest,
            package_sha256,
            slices,
        })
    }
}

// --------------------------------------------------------- framing message

/// The schema of [`FrameOutcome`].
pub const FRAME_OUTCOME_SCHEMA: &str = "slop_ninja.admission.frame_outcome/v1";

/// What framing derived from an object the header accepted: the header's
/// output contract and the derived inference-content digest (what the eval
/// Worker compares with the cohort and the commitment), and the parts
/// manifest and slices it stores and copies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FramedSummary {
    /// The header's output contract.
    pub contract: String,
    /// The derived inference-content digest (hex).
    pub content: String,
    /// The parts manifest, hex (at most [`spec::MAX_MANIFEST_BYTES`]).
    pub manifest_hex: String,
    /// SHA-256 of the manifest bytes (hex).
    pub package_sha256: String,
    /// The graph file's SHA-256 (hex).
    pub model_sha256: String,
    pub slices: Vec<Slice>,
}

/// Framing's verdict on the header and files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FrameResult {
    Framed(FramedSummary),
    /// The protocol reader refused the object (`artifact_invalid`).
    Reader { reason: String },
    /// A header-only spec check refused it (`check_failed`).
    Check { code: String },
}

/// The framing step's message from the upload Worker to the eval Worker:
/// the whole object's SHA-256 and length (an absent object frames as zero
/// bytes) and the verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameOutcome {
    pub schema: String,
    pub object_sha256: String,
    pub object_len: u64,
    pub result: FrameResult,
}

impl From<(Digest, u64, Result<Framed, FrameError>)> for FrameOutcome {
    fn from((digest, len, framed): (Digest, u64, Result<Framed, FrameError>)) -> Self {
        let result = match framed {
            Ok(f) => FrameResult::Framed(FramedSummary {
                contract: f.artifact.header.contract.clone(),
                content: f.artifact.content.hex(),
                manifest_hex: hex::encode(&f.manifest),
                package_sha256: f.package_sha256,
                model_sha256: f.model_sha256,
                slices: f.slices,
            }),
            Err(FrameError::Reader(reason)) => FrameResult::Reader { reason },
            Err(FrameError::Check(code)) => FrameResult::Check { code },
        };
        Self {
            schema: FRAME_OUTCOME_SCHEMA.into(),
            object_sha256: digest.hex(),
            object_len: len,
            result,
        }
    }
}

/// One slice's JSON at its largest, measured on the type itself: a 64-hex
/// digest and two `u64` decimals at their maximum, with the field names.
fn slice_json_bytes() -> usize {
    let max = Slice { sha256: "f".repeat(64), offset: u64::MAX, len: u64::MAX };
    serde_json::to_vec(&max).map_or(usize::MAX, |v| v.len())
}

/// The fixed fields of a framed [`FrameOutcome`] at their largest, measured
/// on the type itself: the schema, the 64-hex digests, a `u64` at its
/// maximum, the field names, and the contract a framed object carries (it
/// passed the header check, so it is [`spec::CONTRACT_ID`]); the manifest
/// and slices are counted apart. A refusal's text (a protocol error's
/// `Debug` form or a spec refusal code, built from static strings and
/// integers) takes the room of the manifest a refused outcome never
/// carries ([`refusal_text_bytes`]).
fn outcome_fixed_bytes() -> usize {
    let digest = "f".repeat(64);
    let max = FrameOutcome {
        schema: FRAME_OUTCOME_SCHEMA.into(),
        object_sha256: digest.clone(),
        object_len: u64::MAX,
        result: FrameResult::Framed(FramedSummary {
            contract: spec::CONTRACT_ID.into(),
            content: digest.clone(),
            manifest_hex: String::new(),
            package_sha256: digest.clone(),
            model_sha256: digest,
            slices: Vec::new(),
        }),
    };
    serde_json::to_vec(&max).map_or(usize::MAX, |v| v.len())
}

/// The most text a refusal carries: the room a framed outcome's manifest
/// takes (twice [`spec::MAX_MANIFEST_BYTES`] as hex). Framing replaces a
/// longer reason with the error's stable code (never expected: the
/// protocol's error forms are a few dozen bytes).
pub fn refusal_text_bytes() -> usize {
    2 * spec::MAX_MANIFEST_BYTES as usize
}

/// The largest [`FrameOutcome`] JSON a spec-admitted object can produce:
/// the manifest as hex (twice [`spec::MAX_MANIFEST_BYTES`]), one slice per
/// [`spec::PART_BYTES`] of the largest admission object plus one per file
/// (each file's last slice may be short) with the comma between slices,
/// and the fixed fields, each measured on its type at its largest. The eval Worker reads the message
/// within this bound.
pub fn frame_outcome_bound() -> usize {
    let object = largest_admission_object();
    let slices = object.div_ceil(spec::PART_BYTES) as usize + spec::ROLES.len();
    refusal_text_bytes()
        .saturating_add(slices.saturating_mul(slice_json_bytes() + 1))
        .saturating_add(outcome_fixed_bytes())
}

/// Parses a 64-hex digest.
pub fn parse_digest(hex_text: &str) -> Option<Digest> {
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex_text, &mut out).ok()?;
    Some(Digest(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_protocol::{FileRole, artifact_object};

    /// A small object with the spec's six file names (not spec-valid: the
    /// files are placeholders).
    fn object(tag: u8) -> Vec<u8> {
        let files: Vec<(&str, FileRole, Vec<u8>)> = vec![
            ("a-fp32band.onnx", FileRole::Graph, vec![tag; 300]),
            ("calibration.json", FileRole::Calibration, vec![b'{', tag, b'}']),
            ("contract.json", FileRole::OutputSchema, vec![b'c', tag]),
            ("heads.json", FileRole::Weights, vec![b'h', tag]),
            ("manifest.json", FileRole::OwnerMetadata, vec![b'm', tag]),
            ("tokenizer.json", FileRole::Tokenizer, vec![b't', tag]),
        ];
        let refs: Vec<(&str, FileRole, &[u8])> = files.iter().map(|(n, r, b)| (*n, *r, b.as_slice())).collect();
        artifact_object(spec::CONTRACT_ID, &refs).unwrap()
    }

    #[test]
    fn framer_slices_each_file_at_the_scorer_part_size() {
        // Slicing is per file, at PART_BYTES, the last slice of a file shorter:
        // checked on a small object through the private slice table.
        let obj = object(0x42);
        let mut f = Framer::new();
        for chunk in obj.chunks(7) {
            f.update(chunk);
        }
        assert!(f.failed.is_none());
        let header = f.reader.header().map(|(h, _)| h.clone()).unwrap();
        assert_eq!(f.slices.len(), header.files.len());
        for (file, parts) in header.files.iter().zip(&f.slices) {
            assert_eq!(parts.len(), 1, "files under 64 MiB are one slice");
            assert_eq!(parts[0].bytes, file.len);
            assert_eq!(
                parts[0].sha256,
                file.sha256.hex(),
                "a whole-file slice hashes as the file"
            );
        }
        // Offsets follow the header order after the length prefix and header,
        // as the reader reports them (the framer parses nothing itself).
        let header_len = u32::from_le_bytes(obj[..4].try_into().unwrap()) as u64;
        assert_eq!(f.files_at, Some(4 + header_len));
        let (_, _, framed) = f.finish(Version::V2);
        assert!(
            matches!(framed, Err(FrameError::Check(_))),
            "the test object is not spec-valid"
        );
    }

    #[test]
    fn a_zero_header_length_is_refused_by_the_one_reader_framing_uses() {
        use std::sync::mpsc;
        use std::time::Duration;
        let (tx1, rx1) = mpsc::channel();
        std::thread::spawn(move || {
            let mut f = Framer::new();
            f.update(&[0u8, 0, 0, 0, 1, 2, 3]);
            f.update(&[4, 5]);
            let _ = tx1.send(f.finish(Version::V2).2.err());
        });
        let got = rx1.recv_timeout(Duration::from_secs(10)).expect("framing returns");
        assert!(
            matches!(&got, Some(FrameError::Reader(r)) if r.contains("artifact header length")),
            "{got:?}"
        );
    }

    #[test]
    fn a_header_listing_more_files_than_the_spec_is_refused_before_slicing() {
        // The test object's six files and one more than the spec's file table.
        let files: Vec<(&str, FileRole, Vec<u8>)> = vec![
            ("a-fp32band.onnx", FileRole::Graph, vec![1; 300]),
            ("calibration.json", FileRole::Calibration, b"{1}".to_vec()),
            ("contract.json", FileRole::OutputSchema, b"c1".to_vec()),
            ("extra.json", FileRole::OwnerMetadata, b"e1".to_vec()),
            ("heads.json", FileRole::Weights, b"h1".to_vec()),
            ("manifest.json", FileRole::OwnerMetadata, b"m1".to_vec()),
            ("tokenizer.json", FileRole::Tokenizer, b"t1".to_vec()),
        ];
        assert_eq!(files.len(), spec::ROLES.len() + 1);
        let refs: Vec<(&str, FileRole, &[u8])> = files.iter().map(|(n, r, b)| (*n, *r, b.as_slice())).collect();
        let obj = artifact_object(spec::CONTRACT_ID, &refs).unwrap();
        let mut f = Framer::new();
        f.update(&obj);
        assert!(f.slices.is_empty(), "nothing sliced");
        assert!(matches!(f.finish(Version::V2).2, Err(FrameError::Check(c)) if c == "undeclared_file"));
    }

    #[test]
    fn outcomes_carry_digests_and_verdicts_within_their_bound() {
        let obj = object(0x42);
        let mut f = Framer::new();
        f.update(&obj);
        let o: FrameOutcome = f.finish(Version::V2).into();
        assert_eq!(o.schema, FRAME_OUTCOME_SCHEMA);
        assert_eq!(o.object_len, obj.len() as u64);
        assert_eq!(o.object_sha256, hex::encode(Sha256::digest(&obj)));
        assert!(matches!(o.result, FrameResult::Check { .. }), "{:?}", o.result);
        let text = serde_json::to_string(&o).unwrap();
        assert_eq!(serde_json::from_str::<FrameOutcome>(&text).unwrap(), o);
        assert!(text.len() < frame_outcome_bound());
        // The bound covers the manifest as hex and one slice per part of the
        // largest admission object.
        assert!(frame_outcome_bound() > 2 * spec::MAX_MANIFEST_BYTES as usize);
        assert!(largest_admission_object() >= SpecBounds::of(Version::V3).unwrap().object_bytes);
    }

    #[test]
    fn the_outcome_bound_is_its_types_at_their_largest() {
        let d = "f".repeat(64);
        let n = largest_admission_object().div_ceil(spec::PART_BYTES) as usize + spec::ROLES.len();
        let o = FrameOutcome {
            schema: FRAME_OUTCOME_SCHEMA.into(),
            object_sha256: d.clone(),
            object_len: u64::MAX,
            result: FrameResult::Framed(FramedSummary {
                contract: spec::CONTRACT_ID.into(),
                content: d.clone(),
                manifest_hex: "f".repeat(2 * spec::MAX_MANIFEST_BYTES as usize),
                package_sha256: d.clone(),
                model_sha256: d.clone(),
                slices: vec![Slice { sha256: d.clone(), offset: u64::MAX, len: u64::MAX }; n],
            }),
        };
        let len = serde_json::to_vec(&o).unwrap().len();
        assert!(len <= frame_outcome_bound(), "{len}");
        assert_eq!(frame_outcome_bound() - len, 1, "derived, not padded");
        // A refusal's text takes the manifest's room: the longest reason
        // fits with every fixed field.
        let refused = FrameOutcome {
            result: FrameResult::Reader { reason: "r".repeat(refusal_text_bytes()) },
            ..o
        };
        assert!(serde_json::to_vec(&refused).unwrap().len() < frame_outcome_bound());
        let e = slop_ninja_protocol::Error::LengthLimit { field: "a_long_static_field_name", len: u64::MAX };
        assert_eq!(reader_reason(&e), format!("{e:?}"));
    }
}
