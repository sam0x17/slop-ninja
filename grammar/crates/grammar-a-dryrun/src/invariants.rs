//! The child-side stop invariants of item 14.1 that index 60 did not count
//! (items 10.12, 13.7, 13.10 and 14.1), on one child's landed transfer
//! (`Transferred` with its `TransferDiag`; amendments 10 and 13 define them,
//! so they are taken at the landed code only, item 14.2):
//!
//! * **Count 4: a wrapped line over its budget that holds more than one
//!   word.** Each line of a wrapped body paragraph is measured in columns
//!   (item 10.1: the Unicode scalar values of its leading whitespace and
//!   body; trailing whitespace not counted) from the laid-out text itself. A
//!   line's budget is W less its lead (item 10.1), so it is over when its
//!   columns exceed W. A mixed child's or insertion's first line starts at
//!   the source's `column` (the parent's lead and body before the span, or
//!   in an item the document line's text before it; amendment 29 item
//!   29.1), unless the replacement starts it on a new line (no room on a
//!   shared line), and its last line, when its last unit is wrapped, is
//!   measured with the parent text after the span, the source's
//!   `tail_column` (item 13.7). Two readings (BUILD-PLAN 12.3 open item (c)):
//!   - `over_budget`: on the replacement, with `column` and `tail_column`
//!     (item 13.7's text); a line whose own text holds one word is exempt;
//!   - `over_budget_document`: in the final document, the whole document
//!     line holding the child's line (they differ only where another edit
//!     shares the line). Protocol A2 amendment 29 item 29.2: the exemption
//!     is read on the document line, so a document line over W is exempt
//!     only when it holds one word, whoever wrote the words
//!     (`by_document_line_words` names the lines it adds).
//! * **Count 5: a kept unit that shares a unit with other text** (item
//!   13.10, read on the transfer's units): a kept unit (a kept list line, or
//!   a closed unpunctuated paragraph) and any unit of the laid-out child
//!   that draws on it and on raw text outside it. Splits of a closed
//!   paragraph into several units are counted beside it (no rule names
//!   them).
//! * **Count 6: a body paragraph of fewer than 8 words made by a placed
//!   break** (item 10.12): a body paragraph with a placed break on either
//!   edge (two body paragraphs of one prose segment; a frame edge, a kept
//!   unit's edge and a closed paragraph's start and end are fixed
//!   boundaries, items 10.6(b) and 13.11, amendment 30 item 30.3). The
//!   readings here are item 28.9's, reported beside the gate:
//!   - `short_alone`: the paragraph's own words;
//!   - `short_final`: with the parent words a span child's first and last
//!     paragraphs join in the final document (`pre_words`, `post_words`, as
//!     `place_breaks` counts them).
//!
//!   Count 6's gate reads item 13.1's constructed final paragraph
//!   (amendment 31 item 31.1; `crate::count6`).
//!
//! A transfer whose diagnostics do not cover its units, or a kept unit that
//! no unit draws on, is inconsistent: the caller stops.
use grammar_a_form::layout::MIN_PARAGRAPH_WORDS;
use grammar_a_form::text;
use grammar_a_form::transfer::{SourceLayout, Transferred, UnitRole};

/// One line of a laid-out child: its unit and line indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineAt {
    pub unit: usize,
    pub line: usize,
    /// The line is measured with the parent text after the span.
    pub with_tail: bool,
    /// The byte offset in the final document of the start of the document
    /// line holding it (None without a final document): distinct document
    /// lines are counted beside child-lines (amendment 29 item 29.2).
    pub doc_line: Option<usize>,
}

/// The child's place in its final document: the document and the child's
/// byte range in it.
#[derive(Clone, Copy, Debug)]
pub struct Final<'a> {
    pub doc: &'a str,
    pub fin: (usize, usize),
}

/// Counts 4-6 on one child.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Invariants {
    pub wrapped_lines: usize,
    /// Wrapped lines measured with the parent text after the span.
    pub tail_lines: usize,
    pub over_budget: Vec<LineAt>,
    pub over_budget_document: Vec<LineAt>,
    /// The document-reading lines whose own text holds one word: counted by
    /// item 29.2's exemption on the document line only.
    pub by_document_line_words: usize,
    pub kept_units: usize,
    pub kept_list_lines: usize,
    pub closed_units: usize,
    /// Indices into `TransferDiag::kept` of the kept units that share a unit.
    pub kept_shared: Vec<usize>,
    pub closed_split: usize,
    pub placed_paragraphs: usize,
    /// Unit indices of the placed-break paragraphs under 8 words.
    pub short_alone: Vec<usize>,
    pub short_final: Vec<usize>,
    /// Why the diagnostics cannot be read (the caller stops).
    pub inconsistent: Option<&'static str>,
}

fn cols(s: &str) -> usize {
    s.trim_end().chars().count()
}

/// Counts 4-6 on `t`, laid out from `src`; `fin` places it in its final
/// document (the document reading of count 4).
pub fn invariants(src: &SourceLayout, t: &Transferred, fin: Option<Final>) -> Invariants {
    let mut out = Invariants::default();
    let units = &t.diag.units;
    let n = t.paragraphs.len();
    if units.len() != n {
        out.inconsistent = Some("diagnostics do not cover the units");
        return out;
    }
    if let Some(f) = fin
        && f.doc.get(f.fin.0..f.fin.1) != Some(t.text.as_str())
    {
        out.inconsistent = Some("the final document does not hold the child at its range");
        return out;
    }
    // count 4
    if let Some(w) = src.width {
        for (u, d) in units.iter().enumerate() {
            if !(d.role == UnitRole::Body && d.wrapped) {
                continue;
            }
            let (s, e) = t.paragraphs[u];
            let ls = t.text[..s].rfind('\n').map_or(0, |i| i + 1);
            let pieces: Vec<&str> = t.text[ls..e].split('\n').collect();
            let mut at = ls;
            for (i, piece) in pieces.iter().enumerate() {
                let (pa, pb) = (at, at + piece.len());
                at = pb + 1;
                out.wrapped_lines += 1;
                let mut c = cols(piece);
                // a first line after the replacement's own line break
                // (amendment 29 item 29.1) does not start at the column
                if src.mixed && u == 0 && i == 0 && ls == 0 {
                    c += src.column;
                }
                let with_tail = src.mixed && u + 1 == n && i + 1 == pieces.len();
                if with_tail {
                    c += src.tail_column;
                    out.tail_lines += 1;
                }
                let many = piece.split_whitespace().count() > 1;
                let mut here = LineAt { unit: u, line: i, with_tail, doc_line: None };
                // the document line holding the child's line, and whether
                // the document reading counts it: over W, with more than one
                // word on the whole document line (item 29.2)
                let doc_over = match fin {
                    Some(f) => {
                        let (da, db) = (f.fin.0 + pa, f.fin.0 + pb);
                        let l0 = f.doc[..da].rfind('\n').map_or(0, |k| k + 1);
                        let l1 = f.doc[db..].find('\n').map_or(f.doc.len(), |k| db + k);
                        here.doc_line = Some(l0);
                        let line = &f.doc[l0..l1];
                        line.split_whitespace().count() > 1 && cols(line) > w
                    }
                    None => many && c > w,
                };
                if many && c > w {
                    out.over_budget.push(here);
                }
                if doc_over {
                    out.over_budget_document.push(here);
                    out.by_document_line_words += usize::from(!many);
                }
            }
        }
    }
    // count 5
    for (ki, k) in t.diag.kept.iter().enumerate() {
        out.kept_units += 1;
        match k.kind {
            grammar_a_form::transfer::KeptKind::ListLine => out.kept_list_lines += 1,
            grammar_a_form::transfer::KeptKind::Closed => out.closed_units += 1,
        }
        let over: Vec<&grammar_a_form::transfer::UnitDiag> =
            units.iter().filter(|u| u.source.raw.0 < k.raw.1 && k.raw.0 < u.source.raw.1).collect();
        if over.is_empty() {
            out.inconsistent = Some("a kept unit that no unit draws on");
            return out;
        }
        if over.iter().any(|u| u.source.raw.0 < k.raw.0 || u.source.raw.1 > k.raw.1) {
            out.kept_shared.push(ki);
        }
        if k.kind == grammar_a_form::transfer::KeptKind::Closed && over.len() > 1 {
            out.closed_split += 1;
        }
    }
    // count 6
    let placed = |a: usize, b: usize| units[a].role == UnitRole::Body && units[b].role == UnitRole::Body && units[a].segment.is_some() && units[a].segment == units[b].segment;
    for (u, d) in units.iter().enumerate() {
        if d.role != UnitRole::Body {
            continue;
        }
        let left = u > 0 && placed(u - 1, u);
        let right = u + 1 < n && placed(u, u + 1);
        if !(left || right) {
            continue;
        }
        out.placed_paragraphs += 1;
        let (s, e) = t.paragraphs[u];
        let alone = text::words(&t.text[s..e]);
        let mut with = alone;
        if src.mixed && u == 0 {
            with += src.structure.pre_words;
        }
        if src.mixed && u + 1 == n {
            with += src.structure.post_words;
        }
        if alone < MIN_PARAGRAPH_WORDS {
            out.short_alone.push(u);
        }
        if with < MIN_PARAGRAPH_WORDS {
            out.short_final.push(u);
        }
    }
    out
}
