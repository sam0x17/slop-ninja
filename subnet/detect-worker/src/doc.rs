//! The stored result and the documents the store keeps in R2.
//!
//! - [`StoredResult`] (`result_full`) is a run as the page prints it, fixed
//!   at scoring time: the band, the printed figures, the
//!   counts, the passages and the package line. The raw floats are kept for
//!   the internal record and never rendered. Passage offsets are UTF-16
//!   offsets into the document's text: the whole text of a secret page, or
//!   the quoted body of a public run.
//! - [`SecretDoc`] is a secret page's document, encrypted under the page's
//!   own key at `p/<hex id_h>`. A pasted text's page writes version 2,
//!   `{v: 2, text, runs, source_check, sections}`, with every run it was
//!   checked under, oldest first (a newer model's run is added to the same
//!   page). A paid page writes version 1, `{v: 1, text, result_full,
//!   source_check, sections}`, and a version 1 document (paid pages, and
//!   text pages made before `slop_ninja_detect_store_v2`) reads as one run.
//!   `text` is the exact string that was scored: `canon_v1` of the text
//!   sent, in whose UTF-16 units a seal's passages are.
//! - [`RunDoc`] is a public run's document at `r/<run_id>` (plain JSON: its
//!   text is public): the passages and context under the display policy's
//!   maximum, their anchors, the passages listed by position, and the
//!   result. It never holds the page's full text.
//!
//! Every document has a version `v` and refuses unknown fields, so a reader
//! never guesses at a shape.

use serde::{Deserialize, Serialize};
use slop_ninja_detect_page::codes::{Band, CutKind, LengthBin, SpansNote};
use slop_ninja_detect_page::page::{Anchor, FurtherPassage, PackageLine, Run, Sections, Span};

/// The document version of public runs and paid pages (one run).
pub const DOC_V: u8 = 1;
/// The document version of pasted-text pages (every run).
pub const DOC_V2: u8 = 2;

/// The raw figures, for the internal record only.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawFigures {
    pub confidence: f64,
    pub share: f64,
}

/// A run as the page prints it (`result_full`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredResult {
    pub scored_at: u64,
    /// The files manifest digest the scorer loaded (the re-run test).
    pub package_load: String,
    pub package_label: String,
    /// The published package digest, 64 hex digits.
    pub package_pub: String,
    /// Publication state when scored; the page takes the serving state when
    /// the run's package is the serving one.
    pub publication_pending: bool,
    pub scorer_build: String,
    pub policy: String,
    pub evidence: String,
    pub band: Option<Band>,
    /// The earlier short-text rule's result field (`null` on every other
    /// result): read and ignored, never written, so documents stored before
    /// a short text got its band still open. Such a short result has no
    /// band and its page does not render.
    #[serde(default, skip_serializing, rename = "short")]
    pub legacy_short: Option<serde::de::IgnoredAny>,
    pub confidence_tenths: Option<u16>,
    pub confidence_text: Option<String>,
    pub share_pct: Option<u8>,
    pub share_text: Option<String>,
    pub words: u32,
    pub total_words: u32,
    pub cut: CutKind,
    pub tokens: u32,
    pub length: Option<LengthBin>,
    #[serde(default)]
    pub spans: Vec<Span>,
    pub spans_note: Option<SpansNote>,
    #[serde(default)]
    pub markers: Vec<String>,
    pub invisible: bool,
    pub raw: Option<RawFigures>,
}

/// The package serving now (from the deployment's package vars).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Serving {
    /// The files manifest digest the scorer loads (`PACKAGE_SHA256`).
    pub load: String,
    /// "span package d24" (`PACKAGE_LABEL`).
    pub label: String,
    /// The published package digest (`PUBLISHED_PACKAGE_DIGEST`).
    pub published: String,
    pub pending: bool,
    /// The scorer build's short digest: the first 8 hex digits of
    /// `SCORER_BINARY_SHA256`.
    pub scorer_build: String,
    /// The evidence table this package's rates come from (`EVIDENCE_ID`).
    pub evidence: String,
}

impl Serving {
    /// The page's serving line, for a page whose run used `run_load`.
    pub fn line(&self, run_load: Option<&str>) -> slop_ninja_detect_page::page::Serving {
        slop_ninja_detect_page::page::Serving {
            package_label: self.label.clone(),
            package_pub_short: self.published.chars().take(8).collect(),
            publication_pending: self.pending,
            differs_from_run: run_load.is_some_and(|l| l != self.load),
        }
    }
}

impl StoredResult {
    /// The page's run block.
    pub fn to_run(&self, serving: &Serving) -> Run {
        let pending = if self.package_pub == serving.published { serving.pending } else { self.publication_pending };
        Run {
            scored_at: self.scored_at,
            package: PackageLine {
                label: self.package_label.clone(),
                published_digest: self.package_pub.clone(),
                publication_pending: pending,
            },
            scorer_build: self.scorer_build.clone(),
            policy: self.policy.clone(),
            evidence: self.evidence.clone(),
            band: self.band,
            confidence_tenths: self.confidence_tenths,
            confidence_text: self.confidence_text.clone(),
            share_pct: self.share_pct,
            share_text: self.share_text.clone(),
            words: self.words,
            total_words: self.total_words,
            cut: self.cut,
            tokens: self.tokens,
            length: self.length,
            spans: self.spans.clone(),
            spans_note: self.spans_note,
            markers: self.markers.clone(),
            invisible: self.invisible,
        }
    }

    /// Whether the result is in a strong band.
    pub fn is_strong(&self) -> bool {
        self.band.is_some_and(Band::is_strong)
    }
}

/// What a pasted text's submitter named as its source: only the host, never
/// checked at launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCheck {
    /// The host in A-label form.
    pub host: String,
    pub checked: bool,
}

/// A secret page's document: its text, its runs (oldest first, at least
/// one) and, on a paid page, its sections.
#[derive(Clone, Debug, PartialEq)]
pub struct SecretDoc {
    /// The version it is written as: [`DOC_V`] (paid pages; one run) or
    /// [`DOC_V2`] (pasted-text pages).
    pub v: u8,
    pub text: String,
    pub runs: Vec<StoredResult>,
    /// A page made before the "Where is this text from?" field was retired
    /// keeps its submitter's host; nothing writes a new one.
    pub source_check: Option<SourceCheck>,
    /// A paid whole document's sections (written when the job assembles).
    pub sections: Option<Sections>,
}

/// Version 1 on the wire.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretDocV1 {
    v: u8,
    text: String,
    result_full: StoredResult,
    source_check: Option<SourceCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sections: Option<Sections>,
}

/// Version 2 on the wire.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretDocV2 {
    v: u8,
    text: String,
    runs: Vec<StoredResult>,
    source_check: Option<SourceCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sections: Option<Sections>,
}

/// A document's version, read before its shape.
#[derive(Deserialize)]
struct Version {
    v: u8,
}

/// A public run's document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunDoc {
    pub v: u8,
    /// The quoted passages and their context, joined; the anchors say what
    /// each range is.
    pub body: String,
    #[serde(default)]
    pub anchors: Vec<Anchor>,
    /// Passages listed by position and linked, not quoted.
    #[serde(default)]
    pub further: Vec<FurtherPassage>,
    /// The source page's words.
    pub page_words: u32,
    /// The fetched page's hash, for the record.
    pub page_hash: Option<String>,
    /// The result, with passage offsets into `body`.
    pub result_full: StoredResult,
}

/// A document that does not parse as its version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadDoc;

impl SecretDoc {
    /// A pasted text's first document (version 2, one run).
    pub fn text_page(text: String, result: StoredResult) -> SecretDoc {
        SecretDoc { v: DOC_V2, text, runs: vec![result], source_check: None, sections: None }
    }

    /// The newest run: the one the page shows.
    pub fn latest(&self) -> Option<&StoredResult> {
        self.runs.last()
    }

    /// The document's bytes. Version 1 holds exactly one run; a version 1
    /// document given more is written as version 2, so no run is dropped.
    pub fn encode(&self) -> Vec<u8> {
        let out = match (self.v, self.runs.as_slice()) {
            (DOC_V, [one]) => serde_json::to_vec(&SecretDocV1 {
                v: DOC_V,
                text: self.text.clone(),
                result_full: one.clone(),
                source_check: self.source_check.clone(),
                sections: self.sections.clone(),
            }),
            _ => serde_json::to_vec(&SecretDocV2 {
                v: DOC_V2,
                text: self.text.clone(),
                runs: self.runs.clone(),
                source_check: self.source_check.clone(),
                sections: self.sections.clone(),
            }),
        };
        out.unwrap_or_default()
    }

    /// Reads a version 1 or 2 document; anything else, or a document with
    /// no run, is refused.
    pub fn decode(b: &[u8]) -> Result<SecretDoc, BadDoc> {
        let v: Version = serde_json::from_slice(b).map_err(|_| BadDoc)?;
        match v.v {
            DOC_V => {
                let d: SecretDocV1 = serde_json::from_slice(b).map_err(|_| BadDoc)?;
                Ok(SecretDoc {
                    v: DOC_V,
                    text: d.text,
                    runs: vec![d.result_full],
                    source_check: d.source_check,
                    sections: d.sections,
                })
            }
            DOC_V2 => {
                let d: SecretDocV2 = serde_json::from_slice(b).map_err(|_| BadDoc)?;
                if d.runs.is_empty() || d.v != DOC_V2 {
                    return Err(BadDoc);
                }
                Ok(SecretDoc {
                    v: DOC_V2,
                    text: d.text,
                    runs: d.runs,
                    source_check: d.source_check,
                    sections: d.sections,
                })
            }
            _ => Err(BadDoc),
        }
    }
}

impl RunDoc {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn decode(b: &[u8]) -> Result<RunDoc, BadDoc> {
        let d: RunDoc = serde_json::from_slice(b).map_err(|_| BadDoc)?;
        if d.v == DOC_V { Ok(d) } else { Err(BadDoc) }
    }
}
