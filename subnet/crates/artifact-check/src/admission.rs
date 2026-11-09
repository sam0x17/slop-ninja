//! Admission bounds and the header check of a direct upload, shared by the
//! eval Worker (which decides) and the key-free upload Worker (which decodes
//! the one untrusted structure admission reads, the header record).
//!
//! - [`SpecBounds`]: the largest file total and the largest object an
//!   artifact of a spec can have, derived from the spec alone.
//! - [`check_header_bytes`]: the header bytes `H` of an upload (at most
//!   `MAX_ARTIFACT_HEADER_LEN`, already verified by the object store against
//!   the envelope's `header_sha256`) decoded and checked against the spec's
//!   file table, as the bounded [`HeaderOutcome`] the upload Worker returns
//!   to the eval Worker: the contract, the derived content digest and the
//!   file table. No file byte is read and nothing is hashed; the eval Worker
//!   recomputes the content digest from the returned table (a small record)
//!   and never decodes the header itself (finding 24 of the first admission
//!   review).

use serde::{Deserialize, Serialize};
use slop_ninja_protocol::{ArtifactFile, ArtifactHeader, Digest, FileRole, MAX_ARTIFACT_HEADER_LEN, Record};

use crate::mapping;
use crate::spec::{self, Version};

// ------------------------------------------------------------- spec bounds

/// The object bound admission derives from a cohort's artifact spec
/// (spec v2, v3 or v4): the largest file total and the largest object any
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
/// lists the spec admits by it (spec v3 admits the span tower class; spec v4
/// admits what v3 admits, its graph class rule stated as a byte comparison;
/// spec v5 admits what v4 admits that carries a token head with the 24-word
/// floor decoder).
pub const ADMISSION_SPECS: &[&str] = &[spec::SPEC_ID_V2, spec::SPEC_ID_V3, spec::SPEC_ID_V4, spec::SPEC_ID_V5];

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

// ------------------------------------------------------------ header check

/// The schema of [`HeaderOutcome`].
pub const HEADER_OUTCOME_SCHEMA: &str = "slop_ninja.admission.header_outcome/v1";

/// One file of a checked header.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderFile {
    pub name: String,
    /// The protocol file role's code (`FileRole` tag).
    pub role: u8,
    /// SHA-256, lowercase hex.
    pub sha256: String,
    pub len: u64,
}

/// The header check's verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HeaderResult {
    /// The header decoded, validated and passed the spec's file-table checks.
    Checked {
        contract: String,
        /// The derived inference-content digest (hex).
        content: String,
        /// The file table in header order (at most the spec's file count).
        files: Vec<HeaderFile>,
    },
    /// The bytes are not a valid artifact header (`artifact_invalid`).
    Invalid { reason: String },
    /// A header-only spec check refused it (`check_failed`).
    Check { code: String },
}

/// The upload Worker's answer to the eval Worker's header check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderOutcome {
    pub schema: String,
    pub result: HeaderResult,
}

/// The file count an encoded header record declares, read before anything
/// is decoded into owned values: the record kind and version, the contract
/// string and the sequence prefix (review finding R7).
pub fn declared_file_count(bytes: &[u8]) -> Option<u32> {
    let u32_at = |at: usize| -> Option<u32> { Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?)) };
    let kind_len = u32_at(0)? as usize;
    let at = 4usize.checked_add(kind_len)?.checked_add(2)?;
    let contract_len = u32_at(at)? as usize;
    u32_at(at.checked_add(4)?.checked_add(contract_len)?)
}

fn invalid(reason: impl Into<String>) -> HeaderOutcome {
    HeaderOutcome { schema: HEADER_OUTCOME_SCHEMA.into(), result: HeaderResult::Invalid { reason: reason.into() } }
}

/// The longest reason text an invalid outcome carries.
pub const MAX_REASON_BYTES: usize = 256;

/// Decodes and checks the header bytes `H` of a direct upload under
/// `version` (module docs). Refuses, before decoding, a length outside
/// `1..=MAX_ARTIFACT_HEADER_LEN` and a declared file count above the spec's
/// file table; refuses a table that gives one digest two lengths.
pub fn check_header_bytes(bytes: &[u8], version: Version) -> HeaderOutcome {
    if bytes.is_empty() || bytes.len() > MAX_ARTIFACT_HEADER_LEN as usize {
        return invalid("artifact header length");
    }
    match declared_file_count(bytes) {
        Some(n) if n as usize <= spec::ROLES.len() => {}
        Some(_) => return HeaderOutcome {
            schema: HEADER_OUTCOME_SCHEMA.into(),
            result: HeaderResult::Check { code: crate::Refusal::UndeclaredFile(String::new()).code().into() },
        },
        None => return invalid("artifact header prefix"),
    }
    let header = match ArtifactHeader::from_bytes(bytes) {
        Ok(h) => h,
        Err(e) => {
            let mut reason = format!("{e:?}");
            reason.truncate(MAX_REASON_BYTES);
            return invalid(reason);
        }
    };
    let content = match header.content().and_then(|c| c.digest()) {
        Ok(c) => c,
        Err(e) => {
            let mut reason = format!("{e:?}");
            reason.truncate(MAX_REASON_BYTES);
            return invalid(reason);
        }
    };
    if let Err(r) = mapping::check_header_for(&header, version) {
        return HeaderOutcome { schema: HEADER_OUTCOME_SCHEMA.into(), result: HeaderResult::Check { code: r.code().into() } };
    }
    for (i, a) in header.files.iter().enumerate() {
        if header.files[i + 1..].iter().any(|b| b.sha256 == a.sha256 && b.len != a.len) {
            return invalid("one digest with two lengths");
        }
    }
    HeaderOutcome {
        schema: HEADER_OUTCOME_SCHEMA.into(),
        result: HeaderResult::Checked {
            contract: header.contract.clone(),
            content: content.hex(),
            files: header
                .files
                .iter()
                .map(|f| HeaderFile { name: f.name.clone(), role: f.role.tag(), sha256: hex::encode(f.sha256.0), len: f.len })
                .collect(),
        },
    }
}

/// The header a checked outcome lists, rebuilt and re-validated by the
/// eval Worker: its content digest is recomputed from the returned table (a
/// small record), so an upload Worker that returned a fabricated table
/// cannot pass a mismatched content (skeptic K6). Returns the header and its
/// content digest.
pub fn rebuild_header(contract: &str, files: &[HeaderFile]) -> Option<(ArtifactHeader, Digest)> {
    if files.len() > spec::ROLES.len() {
        return None;
    }
    let mut list = Vec::with_capacity(files.len());
    for f in files {
        let role = FileRole::from_tag(f.role)?;
        list.push(ArtifactFile { name: f.name.clone(), role, sha256: parse_digest(&f.sha256)?, len: f.len });
    }
    let header = ArtifactHeader { contract: contract.into(), files: list };
    let content = header.content().ok()?.digest().ok()?;
    Some((header, content))
}

/// The largest [`HeaderOutcome`] JSON: a checked outcome with the spec's
/// whole file table at its format bounds (the longest file name, a 64-hex
/// digest, a `u64` length), measured on the type itself, or an invalid one
/// at [`MAX_REASON_BYTES`], whichever is larger.
pub fn header_outcome_bound() -> usize {
    let file = HeaderFile {
        name: "n".repeat(slop_ninja_protocol::artifact::MAX_FILE_NAME_LEN),
        role: u8::MAX,
        sha256: "f".repeat(64),
        len: u64::MAX,
    };
    let checked = HeaderOutcome {
        schema: HEADER_OUTCOME_SCHEMA.into(),
        result: HeaderResult::Checked {
            contract: spec::CONTRACT_ID.into(),
            content: "f".repeat(64),
            files: vec![file; spec::ROLES.len()],
        },
    };
    let refused = invalid("x".repeat(MAX_REASON_BYTES));
    let len = |o: &HeaderOutcome| serde_json::to_vec(o).map_or(usize::MAX, |v| v.len());
    len(&checked).max(len(&refused))
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

    /// The spec's six names with placeholder contents (header-valid apart
    /// from the graph and tokenizer checks, which need the real lengths).
    fn header_bytes(graph_len: usize, tokenizer: &[u8]) -> Vec<u8> {
        let graph = vec![0u8; graph_len];
        let files: Vec<(&str, FileRole, &[u8])> = vec![
            ("a-fp32band.onnx", FileRole::Graph, &graph),
            ("calibration.json", FileRole::Calibration, b"{}"),
            ("contract.json", FileRole::OutputSchema, b"{\"c\":1}"),
            ("heads.json", FileRole::Weights, b"{\"h\":1}"),
            ("manifest.json", FileRole::OwnerMetadata, b"{\"m\":1}"),
            ("tokenizer.json", FileRole::Tokenizer, tokenizer),
        ];
        let obj = artifact_object(spec::CONTRACT_ID, &files).unwrap();
        let len = u32::from_le_bytes(obj[..4].try_into().unwrap()) as usize;
        obj[4..4 + len].to_vec()
    }

    #[test]
    fn a_header_is_decoded_and_checked_without_any_file_byte() {
        let h = header_bytes(10, b"tok");
        // Wrong graph length and tokenizer: a spec refusal, not a decode error.
        match check_header_bytes(&h, Version::V3).result {
            HeaderResult::Check { code } => assert!(code == "graph_size" || code == "tokenizer_mismatch", "{code}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(check_header_bytes(&[], Version::V3).result, HeaderResult::Invalid { .. }));
        assert!(matches!(check_header_bytes(&h[..h.len() - 1], Version::V3).result, HeaderResult::Invalid { .. }));
        let mut long = h.clone();
        long.resize(MAX_ARTIFACT_HEADER_LEN as usize + 1, 0);
        assert!(matches!(check_header_bytes(&long, Version::V3).result, HeaderResult::Invalid { .. }));
    }

    #[test]
    fn a_file_count_above_the_table_is_refused_before_decoding() {
        let h = header_bytes(10, b"tok");
        assert_eq!(declared_file_count(&h), Some(6));
        // Rewrite the count prefix to the spec's table plus one.
        let kind = u32::from_le_bytes(h[..4].try_into().unwrap()) as usize;
        let at = 4 + kind + 2;
        let contract = u32::from_le_bytes(h[at..at + 4].try_into().unwrap()) as usize;
        let count_at = at + 4 + contract;
        let mut more = h.clone();
        more[count_at..count_at + 4].copy_from_slice(&(spec::ROLES.len() as u32 + 1).to_le_bytes());
        assert_eq!(check_header_bytes(&more, Version::V3).result, HeaderResult::Check { code: "undeclared_file".into() });
        let mut huge = h;
        huge[count_at..count_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(check_header_bytes(&huge, Version::V3).result, HeaderResult::Check { .. }));
    }

    #[test]
    fn the_rebuilt_header_recomputes_the_content() {
        let h = slop_ninja_protocol::ArtifactHeader::from_bytes(&header_bytes(10, b"tok")).unwrap();
        let files: Vec<HeaderFile> = h
            .files
            .iter()
            .map(|f| HeaderFile { name: f.name.clone(), role: f.role.tag(), sha256: hex::encode(f.sha256.0), len: f.len })
            .collect();
        let (back, content) = rebuild_header(&h.contract, &files).unwrap();
        assert_eq!(back, h);
        assert_eq!(content, h.content().unwrap().digest().unwrap());
        let mut forged = files.clone();
        forged[0].len += 1;
        assert_ne!(rebuild_header(&h.contract, &forged).unwrap().1, content);
        let mut seven = files.clone();
        seven.push(files[0].clone());
        assert!(rebuild_header(&h.contract, &seven).is_none());
    }

    #[test]
    fn the_outcome_bound_is_its_types_at_their_largest() {
        let b = header_outcome_bound();
        assert!(b < 4096, "{b}");
        let h = header_bytes(10, b"tok");
        let o = check_header_bytes(&h, Version::V3);
        assert!(serde_json::to_vec(&o).unwrap().len() <= b);
    }

    #[test]
    fn the_largest_object_fits_one_r2_put_and_the_url_policy() {
        // R2's single-PUT limit, and the URL policy's B_MIN limit (design
        // section 3.3): every compiled admission spec's largest object fits
        // both, so no file needs multipart and no URL is truncated.
        let r2_single_put: u64 = 5 * 1024 * 1024 * 1024 - 5 * 1024 * 1024;
        let policy_max: u64 = (604_800 - 900) * 125_000;
        for id in ADMISSION_SPECS {
            let b = SpecBounds::of_id(id).unwrap();
            assert!(b.object_bytes <= r2_single_put && b.object_bytes <= policy_max, "{id}");
        }
        assert_eq!(largest_admission_object(), 4 + u64::from(MAX_ARTIFACT_HEADER_LEN) + SpecBounds::of(Version::V3).unwrap().files_bytes);
    }
}
