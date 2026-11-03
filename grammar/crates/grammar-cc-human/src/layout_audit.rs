//! The layout audit `r2_layout_audit_v1` (protocol A2 §4.3, with amendment 7
//! item 7.1's figures, and amendment 10 item 10.12's "before" figures and
//! stop invariants as amended by amendment 13 item 13.10, read by amendment
//! 14; build B12), on the landed measurement and transfer API
//! (`grammar_a_form::measure`, `layout_measure_v2`;
//! `grammar_a_form::transfer`, `layout_transfer_v2`). It copies no rule:
//! every figure is read off `Measure`, `MeasureDiag`, `LayoutCounts`,
//! `SourceLayout::whole` and `transfer`.
//!
//! Per block × domain × class (`a_gates`'s classes: pooled, RAID, Enron,
//! stage-3 and CC humans; mixed and model-only, every source included),
//! with the same counts by class alone, by block × domain × origin × source,
//! and overall. Each row is a source, measured whole.
//!
//! **Item 7.1's figures** (`CellCounts`):
//! * the share of content lines with trailing and with leading whitespace;
//! * single `"\n"` breaks per document and blank-line runs per document;
//! * the hard-wrapped share (W's distribution beside it, in columns);
//! * the boundary separator-string distribution (escaped whitespace);
//! * the line-form share and the share of documents that pass the form
//!   screen (§6.2's whole-text screen), and the layout classes of `measure`.
//!
//! **Item 10.12's figures 1-7 and item 13.10's figures 2-5 and 7-9**
//! (`Figures`, as amendment 14 item 14.3 reads them; `FIGURE_FIELDS` maps
//! each figure to its fields):
//! 1. layout class shares; documents whose body licence L is empty; per
//!    reason, documents whose L (and whose frame licence L_f) holds it;
//! 2. the frames `layout_transfer_v2` uses (sources that are not line-form):
//!    documents with a head frame, a tail frame and both, their frame lines,
//!    sources screened only because of frames (they fail the whole-text
//!    screen, their body licence is empty), and edge lines that are
//!    `list_like` (`--` included);
//! 3. candidate and first-pass runs, rejections by cause, second-pass wraps
//!    in a sentence and after a sentence end, sources with candidate runs but
//!    no W, and first breaks failing the fit before a lowercase line;
//! 4. documents with two or more first-pass block widths and with unequal
//!    leading whitespace, breaks whose kind differs between column and body
//!    widths (`WidthRule::Body`, gap by gap), tab-led lines, lead-change
//!    boundaries;
//! 5. unwrapped lines, the paragraphs holding one, breaks next to one;
//! 6. the sign-off proxy;
//! 7. self-identity: the source transferred onto itself under identity
//!    conventions must give the same `skeleton_sha256` and the same squashed
//!    lines; failures by first differing part, in document order;
//! * 13.10's 2-5: blocks wider than their fit bound, inner `list_like`
//!   lines in fitting runs and evidence runs holding one, retested runs and
//!   those giving evidence, lines within one word of W and one-word lines
//!   wider than W;
//! * 13.10's 7-9: sources that would be line-form if second-pass wraps did
//!   not exempt a paragraph, verse-like hard-wrapped sources (a lead-change
//!   boundary and most lines in failing paragraphs), and line-form sources
//!   with a frame (beside it, those whose frame lines hold most lines).
//!
//! **Stop invariants on sources** (`StopCounts`, item 13.10 as amendment 14
//! item 14.4 reads them): a wrap next to an unwrapped line; a second-pass
//! wrap next to a `list_like` line; a first-pass wrap next to a `list_like`
//! line that is its evidence run's first line or has a `list_like`
//! neighbour in that run; a self-identity failure on an excerpt-layout
//! source that passes the screen. Each must be zero before any keyed pilot.
//!
//! Item 10.12's figure 8 (the re-verdict dry run), item 13.10's figures 1
//! and 6 and the child-side stop invariants are properties of re-verdicted
//! children, taken from that dry run's child records (amendment 14 item
//! 14.1), not here. The figures "at `ed25a148`" are this audit as reviewed,
//! built at B12's pre-rebase head (item 14.2); its record names
//! `layout_measure_v1`.
//!
//! It runs on R0's raw Train (the "before" figures), on the keyed pilots,
//! on Train-2 and on every population (G0a, G0b). Records hold counts,
//! shares, reason names and escaped whitespace strings, never text.
use crate::a_gates::{self, ARow, ORIGINS};
use crate::measure::file_sha256;
use crate::store;
use anyhow::{Result, ensure};
use grammar_a_form::conventions::{Choice, Conventions};
use grammar_a_form::measure::{GapKind, LayoutClass, LayoutCounts, MEASURE_ID, Measure, WidthRule};
use grammar_a_form::screen::list_like;
use grammar_a_form::text;
use grammar_a_form::transfer::{LAYOUT_TRANSFER_ID, SourceLayout, transfer};
use rayon::prelude::*;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const AUDIT_SCHEMA: &str = "slop_ninja_a_r2_layout_audit_v1";
pub const AUDIT_ID: &str = "r2_layout_audit_v1";
/// The protocol readings the added figures follow.
pub const FIGURES_RULE: &str = "PROTOCOL-A2 amendment 10 item 10.12 (figures 1-7) and amendment 13 item 13.10 (figures 2-5, 7-9 and the lowercase count) as read by amendment 14 items 14.1-14.4; item 10.12's figure 8 and item 13.10's figures 1 and 6 are taken from the re-verdict dry run's child records (item 14.1)";

/// Each figure of items 10.12 and 13.10 this audit takes, with its fields
/// in `Figures` (the layout classes and W histogram are `CellCounts`').
pub const FIGURE_FIELDS: &[(&str, &[&str])] = &[
    ("10.12.1 classes", &["layout_classes", "body_passes_screen", "licence_reasons", "frame_licence_reasons"]),
    ("10.12.2 frames", &["head_frames", "tail_frames", "both_frames", "frame_lines", "screened_only_by_frames", "edge_list_lines"]),
    (
        "10.12.3 runs",
        &[
            "candidate_runs",
            "first_pass_runs",
            "first_pass_breaks",
            "rejected_fit_first",
            "rejected_fit_later",
            "rejected_list_like",
            "rejected_unpunctuated",
            "rejected_short_block",
            "second_pass_wraps_in_sentence",
            "second_pass_wraps_after_sentence_end",
            "candidate_runs_without_w",
            "first_break_fit_fail_lowercase",
        ],
    ),
    ("10.12.4 widths", &["widths", "two_or_more_block_widths", "unequal_leading_ws", "body_width_kind_differs", "tab_led_lines", "lead_change_boundaries"]),
    ("10.12.5 unwrapped lines", &["unwrapped_lines", "unwrapped_paragraphs", "breaks_next_to_unwrapped"]),
    ("10.12.6 sign-off proxy", &["sign_off_proxy"]),
    ("10.12.7 self-identity", &["self_identity_failures", "self_identity_first_difference"]),
    ("13.10.2 fit bound", &["blocks_widest_outside_runs"]),
    ("13.10.3 list test", &["inner_list_like_lines_fitting", "evidence_runs_with_inner_list_like"]),
    ("13.10.4 retest", &["retested_runs", "retested_runs_evidence"]),
    ("13.10.5 unwrapped lines", &["unwrapped_within_one_word", "wide_one_word_lines"]),
    ("13.10.7 line-form", &["line_form_by_evidence"]),
    ("13.10.8 verse-like hard-wrapped", &["verse_like_hard_wrapped"]),
    ("13.10.9 short emails", &["line_form_with_frame", "line_form_frame_majority"]),
];

/// Item 10.12's and item 13.10's figures (amendment 14 item 14.3), summed
/// over documents: a field named for documents counts each document once;
/// the others are counts of runs, breaks, lines, paragraphs or blocks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(serde::Deserialize))]
pub struct Figures {
    /// Documents whose body licence L (`SourceLayout::whole`) is empty.
    pub body_passes_screen: usize,
    /// Per reason, documents whose L holds it, and documents whose frame
    /// licence L_f holds it.
    pub licence_reasons: BTreeMap<String, usize>,
    pub frame_licence_reasons: BTreeMap<String, usize>,
    /// Documents with a head frame, a tail frame and both (the frames of a
    /// source that is not line-form).
    pub head_frames: usize,
    pub tail_frames: usize,
    pub both_frames: usize,
    /// Content lines in those frames.
    pub frame_lines: usize,
    /// Documents with a frame that fail the whole-text screen and whose
    /// body licence is empty.
    pub screened_only_by_frames: usize,
    /// First and last content lines that are `list_like`.
    pub edge_list_lines: usize,
    pub candidate_runs: usize,
    pub first_pass_runs: usize,
    pub first_pass_breaks: usize,
    pub rejected_fit_first: usize,
    pub rejected_fit_later: usize,
    pub rejected_list_like: usize,
    pub rejected_unpunctuated: usize,
    pub rejected_short_block: usize,
    pub second_pass_wraps_in_sentence: usize,
    pub second_pass_wraps_after_sentence_end: usize,
    /// Documents with a candidate run and no W.
    pub candidate_runs_without_w: usize,
    pub first_break_fit_fail_lowercase: usize,
    /// Documents with two or more first-pass block widths.
    pub two_or_more_block_widths: usize,
    /// Documents whose content lines differ in leading whitespace.
    pub unequal_leading_ws: usize,
    /// Gaps whose kind under body widths differs from their kind under
    /// columns.
    pub body_width_kind_differs: usize,
    pub tab_led_lines: usize,
    pub lead_change_boundaries: usize,
    pub unwrapped_lines: usize,
    pub unwrapped_paragraphs: usize,
    pub breaks_next_to_unwrapped: usize,
    pub sign_off_proxy: usize,
    /// Documents that fail self-identity, and the same by first differing
    /// part (`IDENTITY_PARTS`).
    pub self_identity_failures: usize,
    pub self_identity_first_difference: BTreeMap<String, usize>,
    pub blocks_widest_outside_runs: usize,
    pub inner_list_like_lines_fitting: usize,
    pub evidence_runs_with_inner_list_like: usize,
    pub retested_runs: usize,
    pub retested_runs_evidence: usize,
    pub unwrapped_within_one_word: usize,
    pub wide_one_word_lines: usize,
    /// Documents not line-form whose lines in failing paragraphs without
    /// first-pass evidence are more than half their lines.
    pub line_form_by_evidence: usize,
    /// Hard-wrapped documents with a lead-change boundary whose lines in
    /// failing paragraphs are more than half their lines.
    pub verse_like_hard_wrapped: usize,
    /// Line-form documents with a frame (`Measure::frames`), and those whose
    /// frame lines are more than half their lines.
    pub line_form_with_frame: usize,
    pub line_form_frame_majority: usize,
}

/// The source-level stop invariants (amendment 14 item 14.4); each must be
/// zero before any keyed pilot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StopCounts {
    /// Wrap gaps with an unwrapped line on either side.
    pub wrap_next_to_unwrapped: usize,
    /// Second-pass wraps (not first-pass evidence) with a `list_like` line
    /// on either side.
    pub second_pass_wrap_next_to_list_like: usize,
    /// First-pass wraps with a `list_like` line on either side that is its
    /// evidence run's first line or has a `list_like` neighbour in the run.
    pub first_pass_wrap_next_to_list_item: usize,
    /// Excerpt-layout documents passing the screen that fail self-identity.
    pub self_identity_excerpt_passing: usize,
}

impl StopCounts {
    fn add(&mut self, o: &StopCounts) {
        self.wrap_next_to_unwrapped += o.wrap_next_to_unwrapped;
        self.second_pass_wrap_next_to_list_like += o.second_pass_wrap_next_to_list_like;
        self.first_pass_wrap_next_to_list_item += o.first_pass_wrap_next_to_list_item;
        self.self_identity_excerpt_passing += o.self_identity_excerpt_passing;
    }

    pub fn total(&self) -> usize {
        self.wrap_next_to_unwrapped + self.second_pass_wrap_next_to_list_like + self.first_pass_wrap_next_to_list_item + self.self_identity_excerpt_passing
    }
}

fn add_map(a: &mut BTreeMap<String, usize>, b: &BTreeMap<String, usize>) {
    for (k, v) in b {
        *a.entry(k.clone()).or_default() += v;
    }
}

macro_rules! sum_fields {
    ($a:expr, $b:expr; $($f:ident),* $(,)?) => { $( $a.$f += $b.$f; )* };
}

impl Figures {
    fn add(&mut self, o: &Figures) {
        sum_fields!(self, o; body_passes_screen, head_frames, tail_frames, both_frames, frame_lines, screened_only_by_frames, edge_list_lines,
            candidate_runs, first_pass_runs, first_pass_breaks, rejected_fit_first, rejected_fit_later, rejected_list_like, rejected_unpunctuated,
            rejected_short_block, second_pass_wraps_in_sentence, second_pass_wraps_after_sentence_end, candidate_runs_without_w,
            first_break_fit_fail_lowercase, two_or_more_block_widths, unequal_leading_ws, body_width_kind_differs, tab_led_lines,
            lead_change_boundaries, unwrapped_lines, unwrapped_paragraphs, breaks_next_to_unwrapped, sign_off_proxy, self_identity_failures,
            blocks_widest_outside_runs, inner_list_like_lines_fitting, evidence_runs_with_inner_list_like, retested_runs, retested_runs_evidence,
            unwrapped_within_one_word, wide_one_word_lines, line_form_by_evidence, verse_like_hard_wrapped, line_form_with_frame,
            line_form_frame_majority);
        add_map(&mut self.licence_reasons, &o.licence_reasons);
        add_map(&mut self.frame_licence_reasons, &o.frame_licence_reasons);
        add_map(&mut self.self_identity_first_difference, &o.self_identity_first_difference);
    }
}

/// The parts self-identity compares, in document order (amendment 14 item
/// 14.3): the head; per content line, whether both texts hold it, its
/// squashed body, its leading and its trailing whitespace; per gap, its
/// separator and its kind; the tail.
pub const IDENTITY_PARTS: [&str; 8] = ["head", "lines", "text", "lead", "trail", "separator", "gap_kind", "tail"];

/// Conventions that render nothing: every choice `none`, so a transfer
/// changes only layout.
pub fn identity_conventions() -> Conventions {
    fn c<K>() -> Choice<K> {
        Choice { value: None, source: "none" }
    }
    Conventions { double: c(), single: c(), dash: c(), range: c(), ellipsis: c() }
}

/// The first part in which `b` differs from `a` (`IDENTITY_PARTS`), or None.
pub fn first_difference(a: &Measure, b: &Measure) -> Option<&'static str> {
    if a.head() != b.head() {
        return Some("head");
    }
    for i in 0..a.lines.len().max(b.lines.len()) {
        if i >= a.lines.len() || i >= b.lines.len() {
            return Some("lines");
        }
        if text::squash(a.body(i)) != text::squash(b.body(i)) {
            return Some("text");
        }
        if a.lead(i) != b.lead(i) {
            return Some("lead");
        }
        if a.trail(i) != b.trail(i) {
            return Some("trail");
        }
        if i < a.gaps.len() && i < b.gaps.len() {
            if a.separator(i) != b.separator(i) {
                return Some("separator");
            }
            if a.gaps[i].kind != b.gaps[i].kind {
                return Some("gap_kind");
            }
        }
    }
    (a.tail() != b.tail()).then_some("tail")
}

/// Self-identity of one source (item 10.12's figure 7): `t` transferred
/// onto itself (`SourceLayout::whole`, identity conventions). None when the
/// result's skeleton and squashed lines equal the source's, else the first
/// differing part.
pub fn self_identity(t: &str, m: &Measure, c: &LayoutCounts, src: &SourceLayout) -> Option<&'static str> {
    let out = transfer(t, &identity_conventions(), src);
    let mo = Measure::of(&out.text);
    let first = first_difference(m, &mo);
    let same = mo.counts().skeleton_sha256 == c.skeleton_sha256 && first.is_none();
    // The parts cover the skeleton and the squashed lines exactly, so a
    // skeleton difference always has a first differing part.
    if same { None } else { Some(first.unwrap_or("skeleton")) }
}

/// The source-level stop invariants of one measured text (item 14.4).
pub fn stop_counts(m: &Measure) -> StopCounts {
    let mut s = StopCounts::default();
    let n = m.lines.len();
    let list = |i: usize| list_like(m.body(i));
    for (i, g) in m.gaps.iter().enumerate() {
        if g.kind != GapKind::Wrap {
            continue;
        }
        s.wrap_next_to_unwrapped += (m.unwrapped[i] || m.unwrapped[i + 1]) as usize;
        if !g.evidence {
            s.second_pass_wrap_next_to_list_like += (list(i) || list(i + 1)) as usize;
            continue;
        }
        // The evidence run: the lines its first-pass evidence gaps join.
        let item = |k: usize| {
            let first = k == 0 || !m.gaps[k - 1].evidence;
            let prev = k > 0 && m.gaps[k - 1].evidence && list(k - 1);
            let next = k + 1 < n && m.gaps[k].evidence && list(k + 1);
            list(k) && (first || prev || next)
        };
        s.first_pass_wrap_next_to_list_item += (item(i) || item(i + 1)) as usize;
    }
    s
}

/// One audited document: its counts, figures and stop counts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Doc {
    pub counts: LayoutCounts,
    pub figures: Figures,
    pub stop: StopCounts,
}

/// Measures one source (item 7.1's counts, the figures and the stop
/// invariants).
pub fn audit_doc(t: &str) -> Doc {
    let m = Measure::of(t);
    let c = m.counts();
    let d = &c.diag;
    let src = SourceLayout::whole(t);
    let body = Measure::of_rule(t, WidthRule::Body);
    let doc = |b: bool| b as usize;
    let line_form = c.class == LayoutClass::LineForm;
    let framed = src.frames.head || src.frames.tail;
    let measured_frame = d.frames.head > 0 || d.frames.tail > 0;
    let identity = self_identity(t, &m, &c, &src);
    let figures = Figures {
        body_passes_screen: doc(src.licence.is_empty()),
        licence_reasons: src.licence.iter().map(|r| (r.clone(), 1)).collect(),
        frame_licence_reasons: src.frame_licence.iter().map(|r| (r.clone(), 1)).collect(),
        head_frames: doc(src.frames.head),
        tail_frames: doc(src.frames.tail),
        both_frames: doc(src.frames.head && src.frames.tail),
        frame_lines: if framed { d.head_frame_lines + d.tail_frame_lines } else { 0 },
        screened_only_by_frames: doc(framed && !c.passes_screen && src.licence.is_empty()),
        edge_list_lines: d.edge_list_lines,
        candidate_runs: d.candidate_runs,
        first_pass_runs: d.first_pass_runs,
        first_pass_breaks: d.first_pass_breaks,
        rejected_fit_first: d.rejected_fit_first,
        rejected_fit_later: d.rejected_fit_later,
        rejected_list_like: d.rejected_list_like,
        rejected_unpunctuated: d.rejected_unpunctuated,
        rejected_short_block: d.rejected_short_block,
        second_pass_wraps_in_sentence: d.second_pass_wraps_in_sentence,
        second_pass_wraps_after_sentence_end: d.second_pass_wraps_after_sentence_end,
        candidate_runs_without_w: doc(d.candidate_runs > 0 && c.width.is_none()),
        first_break_fit_fail_lowercase: d.first_break_fit_fail_lowercase,
        two_or_more_block_widths: doc(d.first_pass_block_widths >= 2),
        unequal_leading_ws: doc(d.unequal_leading_ws),
        body_width_kind_differs: m.gaps.iter().zip(&body.gaps).filter(|(a, b)| a.kind != b.kind).count(),
        tab_led_lines: d.tab_led_lines,
        lead_change_boundaries: d.lead_change_boundaries,
        unwrapped_lines: d.unwrapped_lines,
        unwrapped_paragraphs: d.unwrapped_paragraphs,
        breaks_next_to_unwrapped: d.breaks_next_to_unwrapped,
        sign_off_proxy: d.sign_off_proxy,
        self_identity_failures: doc(identity.is_some()),
        self_identity_first_difference: identity.map(|p| (p.to_string(), 1)).into_iter().collect(),
        blocks_widest_outside_runs: d.blocks_widest_outside_runs,
        inner_list_like_lines_fitting: d.inner_list_like_lines_fitting,
        evidence_runs_with_inner_list_like: d.evidence_runs_with_inner_list_like,
        retested_runs: d.retested_runs,
        retested_runs_evidence: d.retested_runs_evidence,
        unwrapped_within_one_word: d.unwrapped_within_one_word,
        wide_one_word_lines: d.wide_one_word_lines,
        line_form_by_evidence: doc(!line_form && 2 * c.line_form_fail_lines_evidence > c.lines),
        verse_like_hard_wrapped: doc(c.class == LayoutClass::HardWrapped && d.lead_change_boundaries > 0 && 2 * c.screen_fail_lines > c.lines),
        line_form_with_frame: doc(line_form && measured_frame),
        line_form_frame_majority: doc(line_form && measured_frame && 2 * (d.head_frame_lines + d.tail_frame_lines) > c.lines),
    };
    let mut stop = stop_counts(&m);
    stop.self_identity_excerpt_passing = doc(c.class == LayoutClass::Excerpt && c.passes_screen && identity.is_some());
    Doc { counts: c, figures, stop }
}

/// Summed layout counts of a set of documents.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CellCounts {
    pub documents: usize,
    pub lines: usize,
    pub trailing_ws_lines: usize,
    pub leading_ws_lines: usize,
    pub documents_with_trailing_ws: usize,
    pub documents_with_leading_ws: usize,
    pub single_breaks: usize,
    pub blank_line_runs: usize,
    pub boundaries: usize,
    pub hard_wrapped: usize,
    pub line_form: usize,
    pub passes_screen: usize,
    /// Boundary separators (escaped) and their counts.
    pub separators: BTreeMap<String, usize>,
    /// `measure`'s layout class of each document.
    pub layout_classes: BTreeMap<String, usize>,
    /// W of each hard-wrapped document, in columns (amendment 10 item 10.1).
    pub widths: BTreeMap<usize, usize>,
    /// Item 10.12's and item 13.10's figures.
    pub figures: Figures,
    /// The source-level stop invariants.
    pub stop_invariants: StopCounts,
}

fn class_name(c: &LayoutCounts) -> String {
    serde_json::to_value(c.class).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

impl CellCounts {
    pub fn add(&mut self, doc: &Doc) {
        let c = &doc.counts;
        self.documents += 1;
        self.lines += c.lines;
        self.trailing_ws_lines += c.trailing_ws_lines;
        self.leading_ws_lines += c.leading_ws_lines;
        self.documents_with_trailing_ws += (c.trailing_ws_lines > 0) as usize;
        self.documents_with_leading_ws += (c.leading_ws_lines > 0) as usize;
        self.single_breaks += c.single_breaks;
        self.blank_line_runs += c.blank_line_runs;
        self.boundaries += c.boundaries;
        self.hard_wrapped += c.hard_wrapped as usize;
        self.line_form += (c.class == LayoutClass::LineForm) as usize;
        self.passes_screen += c.passes_screen as usize;
        for (k, v) in &c.separators {
            *self.separators.entry(k.clone()).or_default() += v;
        }
        *self.layout_classes.entry(class_name(c)).or_default() += 1;
        if let Some(w) = c.width {
            *self.widths.entry(w).or_default() += 1;
        }
        self.figures.add(&doc.figures);
        self.stop_invariants.add(&doc.stop);
    }

    /// The counts with the audit's shares and per-document rates.
    pub fn record(&self) -> Value {
        let share = |a: usize, b: usize| if b == 0 { Value::Null } else { json!(a as f64 / b as f64) };
        let shares = |m: &BTreeMap<String, usize>| json!(m.iter().map(|(k, &n)| (k.clone(), share(n, self.documents))).collect::<BTreeMap<_, _>>());
        let mut v = serde_json::to_value(self).expect("counts serialise");
        v["trailing_ws_line_share"] = share(self.trailing_ws_lines, self.lines);
        v["leading_ws_line_share"] = share(self.leading_ws_lines, self.lines);
        v["single_breaks_per_document"] = share(self.single_breaks, self.documents);
        v["blank_line_runs_per_document"] = share(self.blank_line_runs, self.documents);
        v["hard_wrapped_share"] = share(self.hard_wrapped, self.documents);
        v["line_form_share"] = share(self.line_form, self.documents);
        v["passes_screen_share"] = share(self.passes_screen, self.documents);
        v["separator_shares"] = json!(self.separators.iter().map(|(k, &n)| (k.clone(), n as f64 / self.boundaries.max(1) as f64)).collect::<BTreeMap<_, _>>());
        // Item 10.12's figure 1, as shares of documents.
        v["layout_class_shares"] = shares(&self.layout_classes);
        v["body_passes_screen_share"] = share(self.figures.body_passes_screen, self.documents);
        v["licence_reason_shares"] = shares(&self.figures.licence_reasons);
        v["frame_licence_reason_shares"] = shares(&self.figures.frame_licence_reasons);
        v["stop_invariants_zero"] = json!(self.stop_invariants.total() == 0);
        v
    }
}

/// Measures every row (in parallel; the order of `docs` is the rows').
pub fn audit_docs(rows: &[ARow]) -> Vec<Doc> {
    rows.par_iter().map(|r| audit_doc(&r.text)).collect()
}

/// The audit of a set of rows (pure; rows must carry text).
pub fn audit_rows(rows: &[ARow]) -> Value {
    let docs = audit_docs(rows);
    let mut by_cell: BTreeMap<String, CellCounts> = BTreeMap::new();
    let mut by_class: BTreeMap<String, CellCounts> = BTreeMap::new();
    let mut by_source: BTreeMap<String, CellCounts> = BTreeMap::new();
    let mut overall = CellCounts::default();
    for (r, d) in rows.iter().zip(&docs) {
        by_cell.entry(format!("{}|{}|{}", r.block.id(), r.domain, r.class)).or_default().add(d);
        by_class.entry(format!("{}|{}", r.block.id(), r.class)).or_default().add(d);
        by_source.entry(format!("{}|{}|{}|{}", r.block.id(), r.domain, ORIGINS[r.origin], r.source)).or_default().add(d);
        overall.add(d);
    }
    let rec = |m: BTreeMap<String, CellCounts>| m.into_iter().map(|(k, v)| (k, v.record())).collect::<BTreeMap<_, _>>();
    let stop = json!({
        "rule": "PROTOCOL-A2 amendment 13 item 13.10 (item 10.12's list as restated) on sources, as amendment 14 item 14.4 reads it; each must be zero before any keyed pilot",
        "counts": overall.stop_invariants, "all_zero": overall.stop_invariants.total() == 0,
        "child_side": "an accepted wrapped line wider than allowed (a span's last line included), an accepted child frame on a side where the source has none, a kept unit sharing a unit with other text, a body paragraph under 8 words made by a placed break: counted on the re-verdict dry run's children (amendment 14 item 14.1); the v2/v1 parity invariant is the parity re-run's",
    });
    json!({
        "schema": AUDIT_SCHEMA, "audit": AUDIT_ID, "measure": MEASURE_ID, "transfer": LAYOUT_TRANSFER_ID,
        "figures_rule": FIGURES_RULE,
        "figure_fields": FIGURE_FIELDS.iter().map(|(k, f)| (k.to_string(), json!(f))).collect::<BTreeMap<_, _>>(),
        "self_identity": {"parts": IDENTITY_PARTS, "conventions": "identity (every choice none)", "source": "SourceLayout::whole"},
        "keys": {"by_block_domain_class": "block|domain|class", "by_block_class": "block|class", "by_block_domain_origin_source": "block|domain|origin|source"},
        "classes": "human_<source> for humans (pool, raid, enron, stage3, cc, ...); mixed and model_only for positives of every source (a_gates)",
        "rows": rows.len(),
        "by_block_domain_class": rec(by_cell), "by_block_class": rec(by_class), "by_block_domain_origin_source": rec(by_source),
        "overall": overall.record(),
        "stop_invariants": stop,
    })
}

pub struct AuditArgs {
    pub rows: PathBuf,
    pub out: PathBuf,
}

/// The `audit` command: writes `layout-audit.json` under `out`.
pub fn audit(a: &AuditArgs) -> Result<Value> {
    let (rows, defaulted) = a_gates::read_rows(&a.rows, true)?;
    ensure!(!rows.is_empty(), "no rows in {}", a.rows.display());
    let mut out = audit_rows(&rows);
    out["inputs"] = json!({"rows": a.rows, "rows_sha256": file_sha256(&a.rows)?, "rows_count": rows.len(), "rows_default_block_t": defaulted});
    store::write_json(&a.out.join("layout-audit.json"), &out)?;
    Ok(
        json!({"rows": rows.len(), "cells": out["by_block_domain_class"].as_object().map_or(0, |m| m.len()), "stop_invariants_zero": out["stop_invariants"]["all_zero"]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a_gates::{Block, class_of};
    use grammar_a_form::transfer::wrap;

    fn row(id: &str, origin: usize, domain: &str, source: &str, text: &str) -> ARow {
        ARow {
            id: id.into(),
            text: text.into(),
            origin,
            domain: domain.into(),
            group: None,
            block: Block::T,
            weight: 1.0,
            source: source.into(),
            class: class_of(origin, source),
        }
    }

    /// Synthetic prose (one sentence each, more than 8 words).
    const P1: &str = "The committee met on Monday to discuss the plan for the harbour and the new bridge, and agreed that the work should start in the spring of next year when the weather is better.";
    const P2: &str = "They also asked the office to write to every family in the village about the road closures, the bus diversions and the new parking rules near the school gates.";

    fn figures(t: &str) -> Figures {
        audit_doc(t).figures
    }

    #[test]
    fn counts_per_domain_and_class_are_exact_and_text_free() {
        let rows = vec![
            // RAID-like human: trailing whitespace on two of three lines, single newlines.
            row("a", 0, "news", "raid", "The council met on Tuesday evening to vote. \nIt approved the new budget for roads. \nResidents spoke for an hour."),
            // Pooled human in excerpt layout.
            row("b", 0, "news", "pool", "The council met on Tuesday evening to vote on roads.\n\nIt approved the new budget after a long debate."),
            // Model-only child with a blank-line run of two blank lines and a leading indent.
            row("c", 2, "news", "local", "  The council met on Tuesday evening to vote on roads.\n\n\nIt approved the new budget after a long debate."),
            // A hard-wrapped human in another domain.
            row("d", 0, "email", "enron", "The quick brown fox jumps over\nthe lazy dog. It ran.\nThen it slept under the old\noak tree for an hour."),
        ];
        let v = audit_rows(&rows);
        assert_eq!((v["measure"].as_str(), v["transfer"].as_str()), (Some("layout_measure_v2"), Some("layout_transfer_v5")));
        let raid = &v["by_block_domain_class"]["T|news|human_raid"];
        assert_eq!(raid["documents"], 1);
        assert_eq!(raid["lines"], 3);
        assert_eq!(raid["trailing_ws_lines"], 2);
        assert_eq!(raid["single_breaks"], 2);
        assert_eq!(raid["blank_line_runs"], 0);
        assert!((raid["trailing_ws_line_share"].as_f64().unwrap() - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!(raid["separators"]["\\n"], 2);
        let pool = &v["by_block_domain_class"]["T|news|human_pool"];
        assert_eq!((pool["blank_line_runs"].as_u64(), pool["single_breaks"].as_u64()), (Some(1), Some(0)));
        assert_eq!(pool["layout_classes"]["excerpt"], 1);
        assert_eq!(pool["passes_screen"], 1);
        let model = &v["by_block_domain_class"]["T|news|model_only"];
        assert_eq!(model["leading_ws_lines"], 1);
        assert_eq!(model["leading_ws_line_share"], 0.5);
        assert_eq!(model["separators"]["\\n\\n\\n"], 1);
        assert_eq!(model["documents_with_leading_ws"], 1);
        // The Enron fixture's W, re-checked under columns (amendment 10 item
        // 10.1) and amendment 13 items 13.2-13.5: its lines have no leading
        // whitespace, so columns are body characters, and W is the widest
        // line of more than one word in its evidence runs, last lines
        // included: the first line, 30. Unchanged from amendment 7.
        let enron = &v["by_block_domain_class"]["T|email|human_enron"];
        assert_eq!((enron["hard_wrapped"].as_u64(), enron["widths"]["30"].as_u64()), (Some(1), Some(1)));
        assert_eq!(enron["hard_wrapped_share"], 1.0);
        assert_eq!(enron["figures"]["first_pass_runs"], 2);
        assert_eq!(enron["figures"]["body_width_kind_differs"], 0);
        assert_eq!((enron["figures"]["candidate_runs"].as_u64(), enron["figures"]["candidate_runs_without_w"].as_u64()), (Some(2), Some(0)));
        assert_eq!(v["by_block_domain_origin_source"]["T|news|model_only|local"]["documents"], 1);
        assert_eq!(v["by_block_class"]["T|human_raid"]["documents"], 1);
        assert_eq!(v["overall"]["documents"], 4);
        // Single breaks: 2 (RAID-like), 0, 0 and 3 (hard-wrapped).
        assert_eq!(v["overall"]["single_breaks_per_document"], 5.0 / 4.0);
        assert_eq!(v["stop_invariants"]["all_zero"], true);
        assert_eq!(v["overall"]["stop_invariants_zero"], true);
        // No text: no word of any input appears in the record.
        let raw = v.to_string();
        for w in ["council", "budget", "Residents", "fox", "Tuesday"] {
            assert!(!raw.contains(w), "{w}");
        }
    }

    #[test]
    fn widths_are_columns() {
        // The same hard wrap indented by two spaces: W counts the lead
        // (amendment 10 item 10.1), and body widths read every break alike.
        let t = "  The quick brown fox jumps over\n  the lazy dog. It ran.\n  Then it slept under the old\n  oak tree for an hour.";
        let d = audit_doc(t);
        assert_eq!(d.counts.width, Some(32));
        assert_eq!(Measure::of_rule(t, WidthRule::Body).width, Some(30));
        assert_eq!(d.figures.body_width_kind_differs, 0);
        assert_eq!(d.figures.unequal_leading_ws, 0);
        let v = audit_rows(&[row("i", 0, "email", "enron", t)]);
        assert_eq!(v["overall"]["widths"]["32"], 1);
    }

    #[test]
    fn line_form_and_screen_shares_follow_item_7_1() {
        let rows = vec![
            row("irc", 0, "irc", "pool", "<ann> hi all\n<bob> hey ann, how is it going today?\n<ann> fine"),
            // A greeting does not make prose line-form; the source still fails the screen.
            row(
                "greet",
                0,
                "irc",
                "pool",
                "Hi Bob,\n\nThe museum reopened last week after two years of repairs to the roof.\nVisitors queued from early morning and the staff handed out maps at the door.\nThe new hall holds the collection of old maps from the city archive.\n\nThanks",
            ),
        ];
        let v = audit_rows(&rows);
        let c = &v["by_block_domain_class"]["T|irc|human_pool"];
        assert_eq!(c["documents"], 2);
        assert_eq!(c["line_form"], 1);
        assert_eq!(c["line_form_share"], 0.5);
        assert_eq!(c["passes_screen"], 0);
        assert_eq!(c["passes_screen_share"], 0.0);
        assert_eq!(c["layout_class_shares"]["line_form"], 0.5);
        // The greeting and sign-off are frames (item 10.4): the body passes
        // with an empty licence, so the source is screened only because of
        // its frames (item 10.12's figure 2).
        let f = &c["figures"];
        assert_eq!((f["head_frames"].as_u64(), f["tail_frames"].as_u64(), f["both_frames"].as_u64()), (Some(1), Some(1), Some(1)));
        assert_eq!((f["frame_lines"].as_u64(), f["screened_only_by_frames"].as_u64()), (Some(2), Some(1)));
        assert_eq!(f["body_passes_screen"], 1);
        assert_eq!(c["body_passes_screen_share"], 0.5);
        assert_eq!(f["frame_licence_reasons"]["short_paragraph"], 1);
        assert_eq!(f["frame_licence_reasons"]["no_sentence_end"], 1);
        assert_eq!(c["frame_licence_reason_shares"]["short_paragraph"], 0.5);
        // The IRC log is line-form, with a licence of its own. Its short first
        // and last lines are frames by `Measure::frames` and hold most of its
        // lines, so item 13.10's figure 9 counts it beside short emails.
        assert!(f["licence_reasons"].as_object().is_some_and(|m| !m.is_empty()));
        assert_eq!((f["line_form_with_frame"].as_u64(), f["line_form_frame_majority"].as_u64()), (Some(1), Some(1)));
        assert_eq!(f["line_form_by_evidence"], 0, "a line-form source is not counted");
    }

    #[test]
    fn empty_cells_give_null_shares() {
        let mut c = CellCounts::default();
        let r = c.record();
        assert!(r["trailing_ws_line_share"].is_null() && r["hard_wrapped_share"].is_null() && r["body_passes_screen_share"].is_null());
        c.add(&audit_doc(""));
        c.add(&audit_doc(" \n "));
        assert_eq!(c.record()["layout_classes"]["empty"], 2);
        assert!(c.record()["trailing_ws_line_share"].is_null());
        // A source of whitespace alone transfers to an empty text: it differs
        // at its head (class empty, so no stop invariant).
        assert_eq!((c.figures.self_identity_failures, c.figures.self_identity_first_difference.get("head")), (1, Some(&1)));
        assert_eq!(audit_doc("").figures.self_identity_failures, 0);
        assert_eq!(c.stop_invariants, StopCounts::default());
    }

    #[test]
    fn run_figures_follow_the_landed_measurement() {
        // A greeting on a single newline above a wrapped paragraph: the run
        // is tested again from its second line and gives evidence (item
        // 13.4); the greeting is a head frame.
        let g = format!("Hi Ann,\n{}", wrap(P1, 60, 0).join("\n"));
        let f = figures(&g);
        assert_eq!((f.candidate_runs, f.retested_runs, f.retested_runs_evidence, f.first_pass_runs), (1, 1, 1, 1));
        assert_eq!((f.rejected_fit_first, f.first_break_fit_fail_lowercase), (0, 0));
        assert_eq!((f.head_frames, f.tail_frames, f.both_frames, f.screened_only_by_frames), (1, 0, 0, 1));
        // A framed source whose body holds a short paragraph has a licence of
        // its own: it is not screened only because of its frames.
        let framed = format!("Hi Ann,\n\n{}\n\nNobody cared.\n\n{}\n\nThanks", wrap(P1, 60, 0).join("\n"), wrap(P2, 60, 0).join("\n"));
        let ff = figures(&framed);
        assert_eq!((ff.head_frames, ff.tail_frames, ff.both_frames, ff.screened_only_by_frames, ff.body_passes_screen), (1, 1, 1, 0, 0));
        assert_eq!(ff.licence_reasons.get("short_paragraph"), Some(&1));
        // Its cell reports L's reasons and L_f's apart: the frames' no_sentence_end is not the body's.
        let mut fc = CellCounts::default();
        fc.add(&audit_doc(&framed));
        let fr = fc.record();
        assert_eq!(fr["licence_reason_shares"], json!({"short_paragraph": 1.0}));
        assert_eq!(fr["frame_licence_reason_shares"]["no_sentence_end"], 1.0);
        // A wrapped "--" continuation is not a list item (item 13.3).
        let dash = "The plan was simple and the cost was low\n-- or so the council said at the time,\nand the work began in the spring.";
        let fd = figures(dash);
        assert_eq!((fd.first_pass_runs, fd.inner_list_like_lines_fitting, fd.evidence_runs_with_inner_list_like), (1, 1, 1));
        assert_eq!(audit_doc(dash).stop, StopCounts::default());
        // Unpunctuated verse: candidate runs, no W, rejected as unpunctuated.
        let verse =
            "we walked out past the station wall\nand down the road to where the river\nbends around the old stone bridge\nand nobody said a word at all";
        let fv = figures(verse);
        assert_eq!((fv.candidate_runs, fv.candidate_runs_without_w, fv.rejected_unpunctuated, fv.first_pass_runs), (1, 1, 1, 0));
        // A first break that fails the fit before a lowercase line.
        let titled = "A short title here\nwe walked along the river path until the light began to fade behind the hills.";
        let ft = figures(titled);
        assert_eq!((ft.rejected_fit_first, ft.first_break_fit_fail_lowercase, ft.rejected_short_block), (1, 1, 0));
        // The same before an uppercase line is not the lowercase count.
        let upper = "A short title here\nWe walked along the river path until the light began to fade behind the hills.";
        let fup = figures(upper);
        assert_eq!((fup.rejected_fit_first, fup.first_break_fit_fail_lowercase), (1, 0));
        // Two blocks wrapped at different widths.
        let two = format!("{}\n\n{}", wrap(P1, 40, 0).join("\n"), wrap(P2, 60, 0).join("\n"));
        assert_eq!(figures(&two).two_or_more_block_widths, 1);
        assert_eq!(figures(&wrap(P1, 40, 0).join("\n")).two_or_more_block_widths, 0);
    }

    #[test]
    fn second_pass_unwrapped_and_sign_off_figures() {
        // A sentence-end wrap before a short sign-off after a long last line
        // (item 10.2's stated limit): a second-pass wrap and the proxy.
        let signed = "The council met on Tuesday evening and\nagreed to repair the old bridge before\nthe winter storms arrive in the valley.\nBest";
        let f = figures(signed);
        assert_eq!((f.second_pass_wraps_after_sentence_end, f.second_pass_wraps_in_sentence, f.sign_off_proxy), (1, 0, 1));
        // Long single-newline lines under a wrapped paragraph are unwrapped.
        let long1 = "The lane behind the mill was closed all week while the workers relaid the stones and fixed the drains.";
        let long2 = "Nobody minded much, because the weather was fine and the walk round by the church was pleasant enough.";
        let u = format!("{}\n\n{long1}\n{long2}", wrap(P1, 40, 0).join("\n"));
        let fu = figures(&u);
        assert_eq!((fu.unwrapped_lines, fu.unwrapped_paragraphs, fu.breaks_next_to_unwrapped), (2, 2, 1));
        assert_eq!(audit_doc(&u).stop, StopCounts::default());
        // A line within one word of W is not unwrapped (item 13.5).
        let near = "Aaaa bbbb cc dd ee ff gg.\nHhhhhhhhhhhh iiiiiiiiiii\njj.";
        let fnear = figures(near);
        assert_eq!((fnear.unwrapped_within_one_word, fnear.unwrapped_lines), (1, 0));
        // A one-word line wider than W.
        let url = format!("{}\n\nwww.example.org/a/very/long/path/that/never/ends/at/all", wrap(P1, 40, 0).join("\n"));
        assert_eq!(figures(&url).wide_one_word_lines, 1);
        // Tab-led lines.
        assert_eq!(figures("\tThe museum reopened last week after two years of work.\n\tVisitors queued from early morning to see it.").tab_led_lines, 2);
    }

    #[test]
    fn verse_line_form_and_short_email_figures() {
        // The alternate-indent verse fixture (amendment 13 item 13.6's
        // `verse_uneven`): hard-wrapped at W 33 with two lead changes. Its
        // lead changes split each stanza into two paragraphs; the second of
        // each ends a sentence and passes, so exactly half its lines fail and
        // item 13.10's figure 8 (more than half) does not count it
        // (amendment 14 item 14.3). Under a one-line title five of nine lines
        // fail and it is counted. With body widths it is line-form, so four
        // breaks change kind.
        let uneven = "The lamps are lit along the quay,\n  the boats are drawn up high,\nthe nets are hung to dry tonight,\n  and gulls go wheeling by.\n\nThe tide comes in across the sand,\n  the wind is soft and low,\nthe harbour bell rings out at nine,\n  and home the sailors go.";
        let d = audit_doc(uneven);
        assert_eq!((d.counts.class, d.counts.width, d.counts.paragraphs), (LayoutClass::HardWrapped, Some(33), 4));
        assert_eq!((d.counts.screen_fail_lines, d.counts.lines), (4, 8));
        let f = &d.figures;
        assert_eq!((f.verse_like_hard_wrapped, f.lead_change_boundaries, f.unequal_leading_ws, f.body_width_kind_differs), (0, 2, 1, 4));
        assert_eq!(f.line_form_by_evidence, 0);
        let titled = audit_doc(&format!("The Harbour\n\n{uneven}"));
        assert_eq!(
            (titled.counts.class, titled.counts.width, titled.counts.screen_fail_lines, titled.counts.lines),
            (LayoutClass::HardWrapped, Some(33), 5, 9)
        );
        assert_eq!((titled.figures.verse_like_hard_wrapped, titled.figures.lead_change_boundaries), (1, 2));
        // The same verse with every stanza paragraph unpunctuated is not
        // hard-wrapped, so figure 8 cannot count it (amendment 16 item
        // 16.2): each stanza is one run inside a sentence, and a run whose
        // last line does not end a sentence gives no first-pass evidence.
        let bare = audit_doc(&uneven.replace("by.", "by").replace("go.", "go"));
        assert_eq!((bare.counts.class, bare.counts.width, bare.counts.screen_fail_lines, bare.counts.lines), (LayoutClass::LineForm, None, 8, 8));
        assert_eq!((bare.figures.first_pass_runs, bare.figures.rejected_unpunctuated, bare.figures.verse_like_hard_wrapped), (0, 2, 0));
        // Without a lead change, the same share of failing lines is not verse-like.
        let even = "The woods are quiet, dark and deep,\nthe path runs on beneath the hill,\nand all the birds have gone to sleep\n\nThe moon comes up above the mill,\nthe gate is shut, the yard is still,\nand no one walks the lane tonight.";
        let de = audit_doc(&format!("The Wood\n\n{even}"));
        assert_eq!((de.counts.class, de.counts.screen_fail_lines, de.counts.lines), (LayoutClass::HardWrapped, 4, 7));
        assert_eq!((de.figures.lead_change_boundaries, de.figures.verse_like_hard_wrapped), (0, 0));
        // Its unpunctuated stanza holds only second-pass wraps: figure 7.
        assert_eq!((de.figures.second_pass_wraps_in_sentence, de.figures.line_form_by_evidence), (2, 1));
        // Paragraphs holding only second-pass wraps would make this source
        // line-form (item 13.9; amendment 13's pin): figure 7.
        let p34 = "aaaa bbbb cccc dddd eeee ffff gggg\nhhhh iiii jjjj kkkk llll mmmm nnnn.";
        let t = format!(
            "{p34}\n\noooo pppp qqqq rrrr ssss tttt uuuu\nvvvv wwww xxxx yyyy zzzz aaaa bbbb\n\ncccc dddd eeee ffff gggg hhhh iiii\njjjj kkkk llll mmmm nnnn oooo pppp"
        );
        let ft = audit_doc(&t);
        assert_eq!((ft.counts.class, ft.figures.line_form_by_evidence, ft.figures.verse_like_hard_wrapped), (LayoutClass::HardWrapped, 1, 0));
        // With one such paragraph they hold exactly half its lines: not more than half, not counted.
        let half = audit_doc(&format!("{p34}\n\noooo pppp qqqq rrrr ssss tttt uuuu\nvvvv wwww xxxx yyyy zzzz aaaa bbbb"));
        assert_eq!(
            (half.counts.class, half.counts.line_form_fail_lines_evidence, half.counts.lines, half.figures.line_form_by_evidence),
            (LayoutClass::HardWrapped, 2, 4, 0)
        );
        // A short email whose frame lines hold most of its lines is
        // line-form (figure 9): its frames are counted there, not as the
        // transfer's frames.
        let email = "Hi Bob,\n\nThe meeting moved to the big room on the second floor today.\n\nThanks,\nSam\nAcme Corp\nPhone 555";
        let fe = audit_doc(email);
        assert_eq!(fe.counts.class, LayoutClass::LineForm);
        assert_eq!((fe.figures.line_form_with_frame, fe.figures.line_form_frame_majority, fe.figures.head_frames, fe.figures.frame_lines), (1, 1, 0, 0));
        // A list under a greeting: line-form with frames that hold a minority
        // of its lines.
        let listed = "Hi Bob,\n\n- a torch\n- two warm hats\n- the old map\n- a flask of tea\n\nThanks";
        let fls = audit_doc(listed);
        assert_eq!(fls.counts.class, LayoutClass::LineForm);
        assert_eq!((fls.figures.line_form_with_frame, fls.figures.line_form_frame_majority), (1, 0));
        // Two items: the frames hold exactly half its lines, not most of them.
        let pair = audit_doc("Hi Bob,\n\n- a torch\n- two warm hats\n\nThanks");
        assert_eq!((pair.counts.class, pair.counts.lines), (LayoutClass::LineForm, 4));
        assert_eq!((pair.figures.line_form_with_frame, pair.figures.line_form_frame_majority), (1, 0));
        // A list's last line is an edge list line.
        let list = "Bring these things for the walk on the hills this weekend please.\n\n- a torch\n- two warm hats\n- the old map";
        let fl = figures(list);
        assert_eq!((fl.edge_list_lines, fl.licence_reasons.get("list_marker").copied()), (1, Some(1)));
        assert_eq!(fl.body_passes_screen, 0);
    }

    #[test]
    fn self_identity_and_its_first_difference() {
        // Excerpt-layout and hard-wrapped sources transfer onto themselves.
        for s in [
            "The ferry left the harbour at dawn and the gulls followed it out to sea.\n\nBy noon the wind had dropped and the passengers came up on deck to eat.",
            "The quick brown fox jumps over\nthe lazy dog. It ran.\nThen it slept under the old\noak tree for an hour.",
            &format!("{}\n\n{}\n", wrap(P1, 60, 0).join("\n"), wrap(P2, 60, 0).join("\n")),
        ] {
            let d = audit_doc(s);
            assert_eq!(d.figures.self_identity_failures, 0, "{s}");
            assert_eq!(d.stop.self_identity_excerpt_passing, 0);
        }
        // A short body paragraph cannot be rebuilt (each placed paragraph
        // holds at least MIN_PARAGRAPH_WORDS words): its line merges.
        let short = "The ferry left the harbour at dawn and the gulls followed it out to sea.\n\nNobody cared.\n\nBy noon the wind had dropped and the passengers came up on deck to eat.";
        let d = audit_doc(short);
        assert_eq!((d.figures.self_identity_failures, d.figures.self_identity_first_difference.get("text").copied()), (1, Some(1)));
        assert_eq!(d.stop.self_identity_excerpt_passing, 0, "the source fails the screen");
        // first_difference walks the parts in document order.
        let a = "A line here.\n\nAnother line.\n";
        let cases = [
            ("\nA line here.\n\nAnother line.\n", "head"),
            ("A line here.\n\nAnother line.\n\nThird.\n", "lines"),
            ("A line there.\n\nAnother line.\n", "text"),
            (" A line here.\n\nAnother line.\n", "lead"),
            ("A line here.\n\n Another line.\n", "lead"),
            ("A line here. \n\nAnother line.\n", "trail"),
            ("A line here.\n\n\nAnother line.\n", "separator"),
            ("A line here.\n\nAnother line.", "tail"),
        ];
        let ma = Measure::of(a);
        assert_eq!(first_difference(&ma, &ma), None);
        assert_eq!(first_difference(&ma, &Measure::of("A  line   here.\n\nAnother line.\n")), None, "squashed bodies");
        for (b, part) in cases {
            assert_eq!(first_difference(&ma, &Measure::of(b)), Some(part), "{b:?}");
        }
        // A gap whose kind differs with equal separators.
        let w = "The quick brown fox jumps over\nthe lazy dog.";
        let s = "The quick brown fox jumps.\nThe lazy dog.";
        let (mw, ms) = (Measure::of(w), Measure::of(s));
        assert_eq!((mw.gaps[0].kind, ms.gaps[0].kind), (GapKind::Wrap, GapKind::SentenceBreak));
        assert_eq!(first_difference(&mw, &ms), Some("text"));
        let mut forced = Measure::of(w);
        forced.gaps[0].kind = GapKind::LineBreak;
        assert_eq!(first_difference(&mw, &forced), Some("gap_kind"));
        assert!(IDENTITY_PARTS.contains(&"gap_kind") && IDENTITY_PARTS.len() == 8);
    }

    #[test]
    fn stop_invariants_count_each_violation() {
        // The landed measurement never makes these; forged gaps check the counts.
        let t = format!("{}\n\n- one item here\n- another item", wrap(P1, 40, 0).join("\n"));
        let m = Measure::of(&t);
        assert_eq!(stop_counts(&m), StopCounts::default());
        let n = m.lines.len();
        // A wrap next to an unwrapped line.
        let mut a = m.clone();
        a.unwrapped[1] = true;
        assert_eq!(stop_counts(&a).wrap_next_to_unwrapped, 2, "the gaps on both sides of line 1");
        // A second-pass wrap next to a list_like line.
        let mut b = m.clone();
        b.gaps[n - 2].kind = GapKind::Wrap;
        assert_eq!(stop_counts(&b).second_pass_wrap_next_to_list_like, 1);
        assert_eq!(stop_counts(&b).first_pass_wrap_next_to_list_item, 0);
        // A first-pass wrap whose run starts with a list line, and one with
        // two consecutive list lines; an isolated continuation is not counted.
        let mut c = m.clone();
        c.gaps[n - 2].kind = GapKind::Wrap;
        c.gaps[n - 2].evidence = true;
        assert_eq!(stop_counts(&c).first_pass_wrap_next_to_list_item, 1, "a run's list-like first line");
        assert_eq!(stop_counts(&c).second_pass_wrap_next_to_list_like, 0);
        let dash = "The plan was simple and the cost was low\n-- or so the council said at the time,\nand the work began in the spring.";
        let md = Measure::of(dash);
        assert!(md.gaps.iter().all(|g| g.evidence && g.kind == GapKind::Wrap));
        assert_eq!(stop_counts(&md), StopCounts::default());
        // A run that gives evidence from its list line on (as a retested run
        // starts at its second line): the list line is its first line.
        let mut from_list = Measure::of(dash);
        from_list.gaps[0].evidence = false;
        assert_eq!(stop_counts(&from_list).first_pass_wrap_next_to_list_item, 1, "the evidence run starts at its list line");
        assert_eq!(stop_counts(&from_list).second_pass_wrap_next_to_list_like, 1);
        let two = "The plan was simple and the cost was low\n-- or so the council said at the time,\n-- and the work began in the spring.";
        let mut m2 = Measure::of(two);
        for g in m2.gaps.iter_mut() {
            g.kind = GapKind::Wrap;
            g.evidence = true;
        }
        assert_eq!(stop_counts(&m2).first_pass_wrap_next_to_list_item, 2, "consecutive list-like lines in the run");
        let mut prev_only = Measure::of(two);
        prev_only.gaps[0].kind = GapKind::Wrap;
        prev_only.gaps[0].evidence = false;
        prev_only.gaps[1].kind = GapKind::Wrap;
        prev_only.gaps[1].evidence = true;
        // Gap 0 is a second-pass wrap before a list line; gap 1 a first-pass
        // wrap whose run starts at line 1, a list line.
        assert_eq!(stop_counts(&prev_only).second_pass_wrap_next_to_list_like, 1);
        assert_eq!(stop_counts(&prev_only).first_pass_wrap_next_to_list_item, 1);
        // A list line after the run's last evidence gap is not in the run: the
        // run's own list line is an isolated continuation and is not counted.
        let mut outside = Measure::of(two);
        outside.gaps[0].kind = GapKind::Wrap;
        outside.gaps[0].evidence = true;
        outside.gaps[1].kind = GapKind::LineBreak;
        outside.gaps[1].evidence = false;
        assert_eq!(stop_counts(&outside), StopCounts::default());
        // A wrap after the second of two list lines counts by its earlier neighbour.
        let four = "The plan was simple and the cost was low\n-- or so the council said at the time,\n-- and the work began in the spring,\nwhen the weather was fine.";
        let mut m4 = Measure::of(four);
        for g in m4.gaps.iter_mut() {
            g.kind = GapKind::Wrap;
            g.evidence = true;
        }
        assert_eq!(stop_counts(&m4).first_pass_wrap_next_to_list_item, 3);
        let mut s = StopCounts::default();
        assert_eq!(s.total(), 0);
        s.add(&StopCounts {
            wrap_next_to_unwrapped: 1,
            second_pass_wrap_next_to_list_like: 2,
            first_pass_wrap_next_to_list_item: 3,
            self_identity_excerpt_passing: 4,
        });
        assert_eq!(s.total(), 10);
    }

    #[test]
    fn cells_sum_documents_and_name_every_figure_field() {
        let texts = [
            "Hi Bob,\n\nThe museum reopened last week after two years of repairs to the roof.\nVisitors queued from early morning and the staff handed out maps at the door.\nThe new hall holds the collection of old maps from the city archive.\n\nThanks",
            "The quick brown fox jumps over\nthe lazy dog. It ran.\nThen it slept under the old\noak tree for an hour.",
            "The ferry left the harbour at dawn and the gulls followed it out to sea.\n\nNobody cared.\n\nBy noon the wind had dropped and the passengers came up on deck to eat.",
        ];
        let docs: Vec<Doc> = texts.iter().map(|t| audit_doc(t)).collect();
        let mut c = CellCounts::default();
        for d in &docs {
            c.add(d);
        }
        // The cell's figures equal the documents' summed from their
        // serialised fields and map entries, independently of `Figures::add`.
        let mut want: BTreeMap<String, u64> = BTreeMap::new();
        for d in &docs {
            for (k, n) in figure_numbers(&d.figures) {
                *want.entry(k).or_default() += n;
            }
        }
        assert_eq!(figure_numbers(&c.figures), want);
        assert_eq!((c.stop_invariants.total(), c.record()["stop_invariants_zero"].as_bool()), (0, Some(true)));
        // A cell sums its documents' stop counts.
        c.add(&Doc { stop: StopCounts { wrap_next_to_unwrapped: 2, ..StopCounts::default() }, ..Doc::default() });
        assert_eq!((c.stop_invariants.wrap_next_to_unwrapped, c.record()["stop_invariants_zero"].as_bool()), (2, Some(false)));
        assert_eq!(c.figures.first_pass_runs, docs[1].figures.first_pass_runs);
        assert_eq!(c.figures.frame_licence_reasons.get("short_paragraph"), Some(&1));
        assert_eq!(c.figures.licence_reasons.get("short_paragraph"), Some(&1));
        assert_eq!((c.figures.self_identity_failures, c.figures.self_identity_first_difference.get("text")), (1, Some(&1)));
        // Every field FIGURE_FIELDS names is in the cell record.
        let r = c.record();
        for (fig, fields) in FIGURE_FIELDS {
            for f in *fields {
                assert!(r.get(*f).is_some() || r["figures"].get(*f).is_some(), "{fig}: {f}");
            }
        }
        // And every Figures field is named by some figure.
        let named: Vec<&str> = FIGURE_FIELDS.iter().flat_map(|(_, f)| f.iter().copied()).collect();
        for k in r["figures"].as_object().unwrap().keys() {
            assert!(named.contains(&k.as_str()), "{k}");
        }
    }

    /// Every count of a serialised `Figures`: each scalar field as `field`,
    /// each map entry as `field.entry` (read without `Figures::add`).
    fn figure_numbers(f: &Figures) -> BTreeMap<String, u64> {
        let mut out = BTreeMap::new();
        for (k, v) in serde_json::to_value(f).unwrap().as_object().unwrap() {
            match v {
                Value::Number(n) => {
                    out.insert(k.clone(), n.as_u64().unwrap());
                }
                Value::Object(m) => {
                    for (e, n) in m {
                        out.insert(format!("{k}.{e}"), n.as_u64().unwrap());
                    }
                }
                _ => panic!("{k}: neither a count nor a map of counts"),
            }
        }
        out
    }

    #[test]
    fn figures_add_sums_every_field() {
        // A Figures whose every field (and one entry of every map) holds a
        // distinct non-zero count, built from the serialised field names, so
        // a field added to the struct is covered without editing this test.
        let mut obj = serde_json::Map::new();
        let mut n = 0u64;
        for (k, v) in serde_json::to_value(Figures::default()).unwrap().as_object().unwrap() {
            n += 1;
            obj.insert(k.clone(), if v.is_object() { json!({"entry": n}) } else { json!(n) });
        }
        let f: Figures = serde_json::from_value(Value::Object(obj)).unwrap();
        let once = figure_numbers(&f);
        assert_eq!(once.len() as u64, n);
        assert!(once.values().all(|&x| x > 0));
        let doubled: BTreeMap<String, u64> = once.iter().map(|(k, &x)| (k.clone(), 2 * x)).collect();
        // Figures::add sums every field and map entry.
        let mut twice = f.clone();
        twice.add(&f);
        assert_eq!(figure_numbers(&twice), doubled);
        // So does a cell over two documents carrying it.
        let mut c = CellCounts::default();
        for _ in 0..2 {
            c.add(&Doc { figures: f.clone(), ..Doc::default() });
        }
        assert_eq!(figure_numbers(&c.figures), doubled);
        assert_eq!(c.documents, 2);
    }
}
