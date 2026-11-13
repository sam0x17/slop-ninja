//! The stored result and the documents the store keeps in R2.
//!
//! - [`StoredResult`] (`result_full`) is a run as the page prints it, fixed
//!   at scoring time: the band or short result, the printed figures, the
//!   counts, the passages and the package line. The raw floats are kept for
//!   the internal record and never rendered. Passage offsets are UTF-16
//!   offsets into the document's text: the whole text of a secret page, or
//!   the quoted body of a public run.
//! - [`SecretDoc`] is a secret page's document, `{text, result_full,
//!   source_check}`, encrypted under the page's own key at `p/<hex id_h>`.
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
use slop_ninja_detect_page::codes::{Band, CutKind, LengthBin, ShortResult, SpansNote};
use slop_ninja_detect_page::page::{Anchor, FurtherPassage, PackageLine, Run, Sections, Span};

/// The document version.
pub const DOC_V: u8 = 1;

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
    pub short: Option<ShortResult>,
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
            short: self.short,
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

    /// Whether the result is in a strong band (or the short strong result).
    pub fn is_strong(&self) -> bool {
        self.band.is_some_and(Band::is_strong) || self.short == Some(ShortResult::Strong)
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

/// A secret page's document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretDoc {
    pub v: u8,
    pub text: String,
    pub result_full: StoredResult,
    pub source_check: Option<SourceCheck>,
    /// A paid whole document's sections (written when the job assembles).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<Sections>,
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
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn decode(b: &[u8]) -> Result<SecretDoc, BadDoc> {
        let d: SecretDoc = serde_json::from_slice(b).map_err(|_| BadDoc)?;
        if d.v == DOC_V { Ok(d) } else { Err(BadDoc) }
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
