//! Layout measurement for `cc_cohort_profile_v2` (protocol A2 §6.2, as
//! amended by amendments 7, 10 and 13): `layout_measure_v2`, the
//! whitespace-layout properties `layout_transfer_v2` copies from a source,
//! and the counts the layout audit (`r2_layout_audit_v1`), the pilot gate
//! and the form-injection twins read. Records built from it hold counts,
//! widths, reason codes, escaped whitespace strings and digests, never text.
//!
//! **Lines.** A text is split at `'\n'`. A line holding only whitespace is
//! blank; every other line is a content line with its leading whitespace,
//! its body and its trailing whitespace (Unicode `White_Space`, as
//! `str::trim`; a `'\r'` before the newline is trailing whitespace). The
//! blank lines before the first content line are the head, and everything
//! after the last content line's trailing whitespace is the tail.
//!
//! **Widths are columns** (amendment 10 item 10.1). A line's width is the
//! number of Unicode scalar values of its leading whitespace and its body
//! together; trailing whitespace is not counted. One width serves the fit
//! test, W, the second pass and the unwrapped-line test. A tab counts as one
//! column, and an indent trimmed before measurement is invisible (stated
//! limits). `Measure::of_rule(.., WidthRule::Body)` measures body widths
//! instead, for the audit's column-versus-body comparison only.
//!
//! **Gaps.** Between two consecutive content lines lies a gap: the exact
//! bytes from the newline ending the first line to the start of the second
//! (`"\n"`, or `"\n\n"`, `"\n  \n"` and so on with blank lines). A gap with one
//! newline is a single break; a gap with blank lines is a blank-line run.
//! A **block** is a maximal run of content lines joined by single breaks.
//!
//! **Sentence ends.** A line ends a sentence when the last sentence of its
//! body, by the sealed splitter `a_comp_sentences_v1` (`text::sentences`),
//! is punctuated: a terminal run (`. ! ? …`) with any closers ends the body.
//! A single break after a line that does not end a sentence falls inside a
//! sentence.
//!
//! **First pass** (amendment 7 item 7.2, as amended by amendment 10 item
//! 10.2(a) and amendment 13 items 13.2-13.5). A run is a maximal run of
//! content lines joined by single breaks inside a sentence; a candidate run
//! holds a break. Its breaks are first-pass (wrap) evidence when a greedy
//! wrapper could have made every one of them and the run is prose:
//! * (fit) for each break, after line i: line i's width, one space and the
//!   next line's first word are wider than the fit bound of line i's block,
//!   its widest line holding more than one word among the lines of its
//!   candidate runs (a single-line run or a one-word line never raises it;
//!   a block without such a line gives no evidence); and
//! * (prose) the run's first line is not `screen::list_like` and no two
//!   consecutive lines of it are, its last line ends a sentence, and its
//!   block holds at least `MIN_PARAGRAPH_WORDS` (inherited) whitespace
//!   words.
//!
//! When a candidate run's first break fails the fit and its first line
//! starts its block, holds fewer than `MIN_PARAGRAPH_WORDS` words and is not
//! `list_like` (a greeting or title), the run is tested again from its
//! second line, once; each of its breaks must also pass the fit against the
//! whole text's bound (the widest block bound).
//!
//! The first pass alone decides hard wrapping: the text is hard-wrapped iff
//! some break is first-pass evidence, and its width W is the widest line
//! holding more than one word among the lines of the runs that give
//! first-pass evidence, their last lines included. A text with no first-pass
//! evidence has no W: every single break inside a sentence is a line break
//! and every single break after a sentence end a sentence break (item 7.2 as
//! written).
//!
//! **Hard-wrapped texts** (amendment 10 item 10.2(b)-(d)):
//! * a line is **unwrapped** when it lies in no run that gives first-pass
//!   evidence, holds more than one whitespace word and is still wider than
//!   W less its last word and one space (amendment 13 item 13.5);
//! * the **continuation lead** is the most common leading-whitespace string
//!   of the lines that first-pass evidence precedes (ties: the shortest,
//!   then the first seen). A single break before a line whose lead differs
//!   from it is a boundary (a lead change), first-pass evidence included:
//!   a sentence break after a sentence end, otherwise a line break;
//! * otherwise a first-pass evidence break is a wrap, and any other single
//!   break (**second pass**) is a wrap iff the line's width, one space and
//!   the next line's first word are wider than W, neither line is
//!   `list_like` or unwrapped and, for a break inside a sentence, its block
//!   holds at least `MIN_PARAGRAPH_WORDS` words; else it is a line break
//!   (inside a sentence) or a sentence break (after a sentence end). The
//!   second pass and the lead changes never set or change W.
//!
//! **Boundaries.** A gap is a paragraph boundary unless it is a wrap: every
//! blank-line run, sentence break and line break is a boundary.
//!
//! **Paragraphs** are the maximal runs of content lines between boundaries.
//! Each is classified with the form screen's `screen::classify` (no site
//! template). A region passes the screen when all its paragraphs pass it,
//! and it is line-form (amendment 7 item 7.1, as amended by amendment 10
//! item 10.2(e)) when more than half of its content lines lie in failing
//! paragraphs that hold no wrap.
//!
//! **Edge frames** (amendment 10 item 10.4, `Measure::frames`). The head
//! frame is the maximal run of first paragraphs each holding fewer than
//! `MIN_PARAGRAPH_WORDS` words and no `list_like` line; the tail frame is
//! the same run at the end. A text whose body would be empty has no frame.
//!
//! **Emphasis.** Paired `**` or `__` markers within one line body, counted
//! as `layout::strip_emphasis` pairs them.
//!
//! **Diagnostics** (`MeasureDiag`, amendment 10 item 10.12's counts): the
//! candidate runs and why each failed the first pass, the second-pass wraps,
//! the lead changes, unwrapped lines, tab-led lines, the frames and the
//! sign-off proxy, as counts.
//!
//! Working memory and output are linear in the input.
use crate::layout::MIN_PARAGRAPH_WORDS;
use crate::screen::{classify, list_like};
use crate::text;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

/// The measurement rule's id (recorded with every count block).
pub const MEASURE_ID: &str = "layout_measure_v2";

/// How a line's width is counted: columns (leading whitespace and body,
/// amendment 10 item 10.1), or the body alone (amendment 7's reading, kept
/// only for the audit's comparison of the two).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WidthRule {
    #[default]
    Column,
    Body,
}

/// One content line, as byte offsets: leading whitespace `start..body.0`,
/// body `body.0..body.1`, trailing whitespace `body.1..end`; `end` is the
/// offset of the newline that ends the line, or the text length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    pub start: usize,
    pub body: (usize, usize),
    pub end: usize,
}

/// How a gap between two content lines is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// A blank-line run: always a paragraph boundary.
    BlankRun,
    /// A single break after a sentence end that is a paragraph boundary.
    SentenceBreak,
    /// A single break inside a paragraph that a wrapper made: first-pass
    /// evidence, or a second-pass wrap in a hard-wrapped text.
    Wrap,
    /// A single break inside a sentence that is not a wrap (a title or
    /// lead-in before a longer line, verse, a signature, list items, a lead
    /// change): a paragraph boundary, as every line break is in v1.
    LineBreak,
}

/// The gap after content line `i`, before line `i + 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    /// The separator bytes: from the newline ending line `i` to the start
    /// of line `i + 1`.
    pub range: (usize, usize),
    pub newlines: usize,
    /// Line `i` ends a sentence.
    pub after_sentence_end: bool,
    pub kind: GapKind,
    /// First-pass (wrap) evidence.
    pub evidence: bool,
    /// A single break before a line whose lead differs from the
    /// continuation lead (a boundary in a hard-wrapped text).
    pub lead_change: bool,
}

impl Gap {
    pub fn boundary(&self) -> bool {
        self.kind != GapKind::Wrap
    }
    /// A single break inside a sentence (a wrap or a line break).
    pub fn in_sentence(&self) -> bool {
        self.newlines == 1 && !self.after_sentence_end
    }
}

/// A paragraph: content lines `lines` and the byte range from the first
/// body's start to the last body's end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paragraph {
    pub lines: Range<usize>,
    pub range: (usize, usize),
    pub words: usize,
    /// `screen::classify`'s reason when the paragraph fails the form screen.
    pub reason: Option<&'static str>,
    /// The paragraph holds a wrap gap.
    pub wrapped: bool,
    /// The paragraph holds a wrap gap that is first-pass evidence (amendment
    /// 13 item 13.9's count).
    pub evidence_wrapped: bool,
    /// The paragraph holds an unwrapped line (amendment 10 item 10.3).
    pub unwrapped_line: bool,
    /// Some line of the paragraph is `list_like`.
    pub list_line: bool,
}

impl Paragraph {
    /// Fewer than `MIN_PARAGRAPH_WORDS` words and no `list_like` line: a
    /// paragraph an edge frame can hold (amendment 10 item 10.4).
    pub fn frame_like(&self) -> bool {
        self.words < MIN_PARAGRAPH_WORDS && !self.list_line
    }
}

/// The layout class of a measured region (records and audits).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutClass {
    /// No content line.
    #[default]
    Empty,
    /// More than half of the region's content lines lie in paragraphs that
    /// fail the form screen and hold no wrap (IRC, verse, transcripts,
    /// lists).
    LineForm,
    /// Prose in a hard-wrapped text (some single break is first-pass
    /// evidence).
    HardWrapped,
    /// Prose in `layout::excerpt_layout` form: squashed lines, one empty
    /// line between lines, no head or tail.
    Excerpt,
    /// Any other prose layout (single-newline paragraphs, per-line
    /// whitespace, other separators).
    Prose,
}

/// The edge frames of a whole text, as paragraph counts (amendment 10 item
/// 10.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Frames {
    pub head: usize,
    pub tail: usize,
}

/// Text-free diagnostics of one measurement (amendment 10 item 10.12's
/// counts). Whole-text properties, also for a region's counts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MeasureDiag {
    pub width_rule: WidthRule,
    /// Runs holding at least one break (first-pass candidates).
    pub candidate_runs: usize,
    /// Candidate runs that give first-pass evidence, and their breaks.
    pub first_pass_runs: usize,
    pub first_pass_breaks: usize,
    /// Candidate runs rejected, by cause (a run counts under every cause
    /// that holds): the fit fails at its first break; the fit holds at the
    /// first break and fails at a later one; a line is `list_like`; the
    /// last line does not end a sentence; the block holds fewer than
    /// `MIN_PARAGRAPH_WORDS` words.
    pub rejected_fit_first: usize,
    pub rejected_fit_later: usize,
    pub rejected_list_like: usize,
    pub rejected_unpunctuated: usize,
    pub rejected_short_block: usize,
    /// Candidate runs whose first break fails the fit while the next line
    /// starts lowercase (the proxy for the exemption not adopted).
    pub first_break_fit_fail_lowercase: usize,
    /// Distinct widest-evidence-line widths over the blocks holding
    /// first-pass evidence.
    pub first_pass_block_widths: usize,
    /// Second-pass wraps inside a sentence and after a sentence end.
    pub second_pass_wraps_in_sentence: usize,
    pub second_pass_wraps_after_sentence_end: usize,
    pub lead_change_boundaries: usize,
    pub unwrapped_lines: usize,
    pub unwrapped_paragraphs: usize,
    /// Single breaks with an unwrapped line on either side.
    pub breaks_next_to_unwrapped: usize,
    /// Content lines whose leading whitespace holds a tab.
    pub tab_led_lines: usize,
    /// Content lines differ in their leading whitespace.
    pub unequal_leading_ws: bool,
    /// Sentence-end wraps whose next line ends its paragraph unpunctuated
    /// (the sign-off proxy).
    pub sign_off_proxy: usize,
    pub frames: Frames,
    pub head_frame_lines: usize,
    pub tail_frame_lines: usize,
    /// Of the first and last content lines, those that are `list_like`
    /// (a `--` signature delimiter included).
    pub edge_list_lines: usize,
    /// Blocks holding a candidate run whose widest line is wider than their
    /// fit bound (amendment 13 item 13.2: the line lies outside every
    /// candidate run or holds one word).
    pub blocks_widest_outside_runs: usize,
    /// `list_like` lines after a run's first line, in candidate runs whose
    /// fit holds (item 13.3).
    pub inner_list_like_lines_fitting: usize,
    /// Runs giving first-pass evidence that hold a `list_like` line after
    /// their first (item 13.3).
    pub evidence_runs_with_inner_list_like: usize,
    /// Candidate runs tested again from their second line, and those that
    /// then give first-pass evidence (item 13.4).
    pub retested_runs: usize,
    pub retested_runs_evidence: usize,
    /// Lines outside every evidence run, holding more than one word, wider
    /// than W but within their last word and one space of it: not
    /// unwrapped (item 13.5).
    pub unwrapped_within_one_word: usize,
    /// One-word lines wider than W (never unwrapped; a single break next to
    /// one can be a second-pass wrap: amendment 13 item 13.10's count).
    pub wide_one_word_lines: usize,
}

/// A measured text. Offsets index `text`.
#[derive(Clone, Debug)]
pub struct Measure<'a> {
    pub text: &'a str,
    pub lines: Vec<Line>,
    /// `gaps[i]` lies between `lines[i]` and `lines[i + 1]`.
    pub gaps: Vec<Gap>,
    pub paragraphs: Vec<Paragraph>,
    pub hard_wrapped: bool,
    pub width: Option<usize>,
    /// Per line: unwrapped (amendment 10 item 10.2(b)).
    pub unwrapped: Vec<bool>,
    /// The continuation lead of a hard-wrapped text.
    pub continuation_lead: Option<&'a str>,
    pub diag: MeasureDiag,
}

/// Whether a line body ends a sentence (the sealed splitter's last sentence
/// is punctuated).
pub fn ends_sentence(body: &str) -> bool {
    text::sentences(body).last().is_some_and(|s| s.punctuated)
}

/// Paired emphasis markers in one line, as `layout::strip_emphasis` counts
/// them (`**` and `__` separately, pairs of non-overlapping occurrences).
pub fn emphasis_pairs(line: &str) -> usize {
    ["**", "__"].iter().map(|m| line.matches(m).count() / 2).sum()
}

/// A whitespace string written so a record can hold it: `\n`, `\r`, `\t`
/// and `\s` (space) for those characters, `\u{XXXX}` for any other.
pub fn escape_ws(s: &str) -> String {
    let mut o = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        match c {
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            ' ' => o.push_str("\\s"),
            c => o.push_str(&format!("\\u{{{:04x}}}", c as u32)),
        }
    }
    o
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

impl<'a> Measure<'a> {
    /// Measures `text` with column widths (see the module notes).
    pub fn of(text: &'a str) -> Measure<'a> {
        Measure::of_rule(text, WidthRule::Column)
    }

    /// Measures `text` with the given width rule (`Column` is the rule;
    /// `Body` serves the audit's comparison only).
    pub fn of_rule(text: &'a str, rule: WidthRule) -> Measure<'a> {
        let mut lines = Vec::new();
        let mut start = 0;
        loop {
            let end = text[start..].find('\n').map_or(text.len(), |k| start + k);
            let seg = &text[start..end];
            let lead = seg.len() - seg.trim_start().len();
            if lead < seg.len() {
                let body_end = start + seg.trim_end().len();
                lines.push(Line { start, body: (start + lead, body_end), end });
            }
            if end == text.len() {
                break;
            }
            start = end + 1;
        }
        let n = lines.len();
        let mut gaps: Vec<Gap> = lines
            .windows(2)
            .map(|w| {
                let range = (w[0].end, w[1].start);
                Gap {
                    range,
                    newlines: text[range.0..range.1].matches('\n').count(),
                    after_sentence_end: ends_sentence(&text[w[0].body.0..w[0].body.1]),
                    kind: GapKind::Wrap,
                    evidence: false,
                    lead_change: false,
                }
            })
            .collect();
        let body = |i: usize| &text[lines[i].body.0..lines[i].body.1];
        let lead = |i: usize| &text[lines[i].start..lines[i].body.0];
        let col = |i: usize| match rule {
            WidthRule::Column => chars(lead(i)) + chars(body(i)),
            WidthRule::Body => chars(body(i)),
        };
        let first_word = |i: usize| body(i).split_whitespace().next().map_or(0, chars);
        let last_word = |i: usize| body(i).split_whitespace().last().map_or(0, chars);
        let multi = |i: usize| body(i).split_whitespace().nth(1).is_some();
        let list: Vec<bool> = (0..n).map(|i| list_like(body(i))).collect();
        let mut diag = MeasureDiag { width_rule: rule, ..MeasureDiag::default() };
        // Blocks (lines joined by single breaks): their words and index.
        let mut block_words = vec![0usize; n];
        let mut block_of = vec![0usize; n];
        let mut blocks: Vec<(usize, usize)> = Vec::new();
        let mut a = 0;
        for i in 0..n {
            if i + 1 == n || gaps[i].newlines >= 2 {
                let words = text::words(&text[lines[a].body.0..lines[i].body.1]);
                for k in a..=i {
                    block_words[k] = words;
                    block_of[k] = blocks.len();
                }
                blocks.push((a, i));
                a = i + 1;
            }
        }
        // Runs: maximal runs of lines joined by single breaks inside a
        // sentence, as (first line, last line).
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut a = 0;
        let run_ends = |i: usize| i + 1 == n || !gaps[i].in_sentence();
        for i in (0..n).filter(|&i| run_ends(i)) {
            runs.push((a, i));
            a = i + 1;
        }
        // The fit bound of a block (amendment 13 item 13.2): its widest line
        // holding more than one word among the lines of its candidate runs
        // (runs holding a break). A single-line run or a one-word line is no
        // wrap evidence and never raises the bound; a block without such a
        // line gives no first-pass evidence.
        let mut bound: Vec<Option<usize>> = vec![None; blocks.len()];
        for &(a, z) in runs.iter().filter(|r| r.1 > r.0) {
            for i in (a..=z).filter(|&i| multi(i)) {
                let b = &mut bound[block_of[i]];
                *b = Some(b.map_or(col(i), |x| x.max(col(i))));
            }
        }
        for (k, &(a, z)) in blocks.iter().enumerate() {
            let holds_candidate = runs.iter().any(|r| r.1 > r.0 && r.0 >= a && r.1 <= z);
            let widest = (a..=z).map(col).max().unwrap_or(0);
            diag.blocks_widest_outside_runs += (holds_candidate && bound[k].is_none_or(|b| widest > b)) as usize;
        }
        // First pass, run by run (amendment 10 item 10.2(a), as amended by
        // amendment 13 items 13.2-13.4).
        let fits = |i: usize| bound[block_of[i]].is_some_and(|b| col(i) + 1 + first_word(i + 1) > b);
        // A retested run (item 13.4) must clear the whole text's bound: the
        // widest of the blocks' bounds.
        let text_bound = bound.iter().flatten().copied().max();
        let fits_text = |i: usize| text_bound.is_some_and(|b| col(i) + 1 + first_word(i + 1) > b);
        // A run from line a to z is list-like when its first line is, or two
        // consecutive lines of it are (item 13.3): a list_like line after a
        // break inside a sentence with no list_like neighbour in the run is a
        // continuation, not an item.
        let list_run = |a: usize, z: usize| list[a] || (a..z).any(|k| list[k] && list[k + 1]);
        let mut evidence = vec![false; gaps.len()];
        let mut in_run = vec![false; n];
        for &(a, z) in runs.iter().filter(|r| r.1 > r.0) {
            diag.candidate_runs += 1;
            let fit_first = fits(a);
            let fit_all = |a: usize| (a..z).all(fits);
            let ends = ends_sentence(body(z));
            let enough = block_words[z] >= MIN_PARAGRAPH_WORDS;
            let passes = |a: usize| fit_all(a) && !list_run(a, z) && ends && enough;
            if fit_all(a) {
                diag.inner_list_like_lines_fitting += (a + 1..=z).filter(|&k| list[k]).count();
            }
            // Item 13.4: when the first break fails the fit and the first
            // line starts its block, holds fewer than MIN_PARAGRAPH_WORDS
            // words and is not list_like (a greeting or title), the run is
            // tested again from its second line, each break also against the
            // whole text's bound.
            let retest = !fit_first && a + 1 < z && blocks[block_of[a]].0 == a && text::words(body(a)) < MIN_PARAGRAPH_WORDS && !list[a];
            diag.retested_runs += retest as usize;
            let start = if passes(a) {
                Some(a)
            } else if retest && passes(a + 1) && (a + 1..z).all(fits_text) {
                diag.retested_runs_evidence += 1;
                Some(a + 1)
            } else {
                None
            };
            match start {
                Some(s) => {
                    diag.first_pass_runs += 1;
                    diag.first_pass_breaks += z - s;
                    evidence[s..z].iter_mut().for_each(|x| *x = true);
                    in_run[s..=z].iter_mut().for_each(|x| *x = true);
                    diag.evidence_runs_with_inner_list_like += (s + 1..=z).any(|k| list[k]) as usize;
                }
                None => {
                    diag.rejected_fit_first += !fit_first as usize;
                    diag.rejected_fit_later += (fit_first && !fit_all(a)) as usize;
                    diag.rejected_list_like += list_run(a, z) as usize;
                    diag.rejected_unpunctuated += !ends as usize;
                    diag.rejected_short_block += !enough as usize;
                }
            }
            diag.first_break_fit_fail_lowercase += (!fit_first && body(a + 1).chars().next().is_some_and(char::is_lowercase)) as usize;
        }
        // W (item 13.5): the widest line holding more than one word among the
        // lines of the runs that give first-pass evidence, each run's last
        // line included (the widest of their lines when every one holds a
        // single word).
        let width: Option<usize> = (0..n).filter(|&i| in_run[i] && multi(i)).map(col).max().or_else(|| (0..n).filter(|&i| in_run[i]).map(col).max());
        let mut block_evidence_width: BTreeMap<usize, usize> = BTreeMap::new();
        for i in (0..n).filter(|&i| in_run[i] && multi(i)) {
            let e = block_evidence_width.entry(block_of[i]).or_default();
            *e = (*e).max(col(i));
        }
        diag.first_pass_block_widths = block_evidence_width.values().collect::<BTreeSet<_>>().len();
        let mut unwrapped = vec![false; n];
        let mut continuation_lead: Option<&'a str> = None;
        if let Some(w) = width {
            // Item 13.5: a line outside every evidence run that holds more
            // than one word is unwrapped when it is still wider than W less
            // its last word and one space (a greedy wrapper at W would have
            // broken it before its last word too).
            for i in 0..n {
                let wider = !in_run[i] && multi(i) && col(i) > w;
                unwrapped[i] = wider && col(i) > w + 1 + last_word(i);
                diag.unwrapped_within_one_word += (wider && !unwrapped[i]) as usize;
                diag.wide_one_word_lines += (!multi(i) && col(i) > w) as usize;
            }
            // The continuation lead: the most common lead after first-pass
            // evidence; ties go to the shortest, then the first seen.
            let mut tally: Vec<(&'a str, usize)> = Vec::new();
            for i in (0..gaps.len()).filter(|&i| evidence[i]) {
                let l: &'a str = &text[lines[i + 1].start..lines[i + 1].body.0];
                match tally.iter_mut().find(|(s, _)| *s == l) {
                    Some(e) => e.1 += 1,
                    None => tally.push((l, 1)),
                }
            }
            let mut best: Option<(&'a str, usize)> = None;
            for &(s, c) in &tally {
                if best.is_none_or(|(bs, bc)| c > bc || (c == bc && chars(s) < chars(bs))) {
                    best = Some((s, c));
                }
            }
            continuation_lead = best.map(|b| b.0);
        }
        for i in 0..gaps.len() {
            let g = &mut gaps[i];
            g.evidence = evidence[i];
            let boundary = if g.after_sentence_end { GapKind::SentenceBreak } else { GapKind::LineBreak };
            g.kind = if g.newlines >= 2 {
                GapKind::BlankRun
            } else if let Some(w) = width {
                if continuation_lead.is_some_and(|c| lead(i + 1) != c) {
                    g.lead_change = true;
                    diag.lead_change_boundaries += 1;
                    boundary
                } else if evidence[i] {
                    GapKind::Wrap
                } else if col(i) + 1 + first_word(i + 1) > w
                    && !list[i]
                    && !list[i + 1]
                    && !unwrapped[i]
                    && !unwrapped[i + 1]
                    && (g.after_sentence_end || block_words[i] >= MIN_PARAGRAPH_WORDS)
                {
                    if g.after_sentence_end {
                        diag.second_pass_wraps_after_sentence_end += 1;
                    } else {
                        diag.second_pass_wraps_in_sentence += 1;
                    }
                    GapKind::Wrap
                } else {
                    boundary
                }
            } else {
                boundary
            };
            if g.newlines == 1 && (unwrapped[i] || unwrapped[i + 1]) {
                diag.breaks_next_to_unwrapped += 1;
            }
        }
        let mut paragraphs = Vec::new();
        let mut a = 0;
        for i in 0..n {
            if i + 1 == n || gaps[i].boundary() {
                let range = (lines[a].body.0, lines[i].body.1);
                let t = &text[range.0..range.1];
                paragraphs.push(Paragraph {
                    lines: a..i + 1,
                    range,
                    words: text::words(t),
                    reason: classify(t, false).reason,
                    wrapped: (a..i).any(|k| gaps[k].kind == GapKind::Wrap),
                    evidence_wrapped: (a..i).any(|k| gaps[k].kind == GapKind::Wrap && gaps[k].evidence),
                    unwrapped_line: (a..=i).any(|k| unwrapped[k]),
                    list_line: (a..=i).any(|k| list[k]),
                });
                a = i + 1;
            }
        }
        diag.unwrapped_lines = unwrapped.iter().filter(|&&x| x).count();
        diag.unwrapped_paragraphs = paragraphs.iter().filter(|p| p.unwrapped_line).count();
        diag.tab_led_lines = (0..n).filter(|&i| lead(i).contains('\t')).count();
        diag.unequal_leading_ws = (1..n).any(|i| lead(i) != lead(0));
        diag.edge_list_lines = match n {
            0 => 0,
            1 => list[0] as usize,
            _ => list[0] as usize + list[n - 1] as usize,
        };
        // The sign-off proxy: a paragraph's last gap is a sentence-end wrap
        // and its last line is unpunctuated.
        for p in paragraphs.iter().filter(|p| p.lines.len() >= 2) {
            let k = p.lines.end - 2;
            if gaps[k].kind == GapKind::Wrap && gaps[k].after_sentence_end && !ends_sentence(body(k + 1)) {
                diag.sign_off_proxy += 1;
            }
        }
        let mut m = Measure { text, lines, gaps, paragraphs, hard_wrapped: width.is_some(), width, unwrapped, continuation_lead, diag };
        let f = m.frames();
        m.diag.frames = f;
        m.diag.head_frame_lines = m.paragraphs[..f.head].iter().map(|p| p.lines.len()).sum();
        m.diag.tail_frame_lines = m.paragraphs[m.paragraphs.len() - f.tail..].iter().map(|p| p.lines.len()).sum();
        m
    }

    pub fn lead(&self, i: usize) -> &'a str {
        &self.text[self.lines[i].start..self.lines[i].body.0]
    }
    pub fn trail(&self, i: usize) -> &'a str {
        &self.text[self.lines[i].body.1..self.lines[i].end]
    }
    pub fn body(&self, i: usize) -> &'a str {
        &self.text[self.lines[i].body.0..self.lines[i].body.1]
    }
    /// Line `i`'s width in columns (leading whitespace and body).
    pub fn columns(&self, i: usize) -> usize {
        chars(self.lead(i)) + chars(self.body(i))
    }
    pub fn separator(&self, i: usize) -> &'a str {
        &self.text[self.gaps[i].range.0..self.gaps[i].range.1]
    }
    /// The blank lines before the first content line (the whole text when
    /// it has none).
    pub fn head(&self) -> &'a str {
        self.lines.first().map_or(self.text, |l| &self.text[..l.start])
    }
    /// Everything after the last content line's trailing whitespace.
    pub fn tail(&self) -> &'a str {
        self.lines.last().map_or("", |l| &self.text[l.end..])
    }
    /// Whether the text is line-form: more than half of its content lines
    /// lie in failing paragraphs that hold no wrap (amendment 7 item 7.1,
    /// as amended by amendment 10 item 10.2(e)).
    pub fn line_form(&self) -> bool {
        self.counts().class == LayoutClass::LineForm
    }

    /// The edge frames of the whole text (amendment 10 item 10.4): the
    /// maximal runs of first and last paragraphs that are `frame_like`;
    /// none when the body would be empty.
    pub fn frames(&self) -> Frames {
        let n = self.paragraphs.len();
        let head = self.paragraphs.iter().take_while(|p| p.frame_like()).count();
        let tail = self.paragraphs.iter().rev().take_while(|p| p.frame_like()).count();
        if head + tail >= n { Frames::default() } else { Frames { head, tail } }
    }

    /// Indices of the paragraphs whose range overlaps `[s, e)`.
    pub fn paragraphs_overlapping(&self, s: usize, e: usize) -> Range<usize> {
        let first = self.paragraphs.iter().position(|p| p.range.0 < e.max(s + 1) && s < p.range.1);
        match first {
            None => 0..0,
            Some(a) => {
                let mut b = a + 1;
                while b < self.paragraphs.len() && self.paragraphs[b].range.0 < e {
                    b += 1;
                }
                a..b
            }
        }
    }

    /// The counts of the whole text.
    pub fn counts(&self) -> LayoutCounts {
        self.counts_over(0..self.lines.len(), true)
    }

    /// The counts over content lines `lines` and the gaps between them; the
    /// head and tail count only when `ends` (the whole text). Hard wrapping,
    /// W and the diagnostics are the whole text's (one W per document); the
    /// line-form test and the screen are the region's paragraphs'.
    pub fn counts_over(&self, lines: Range<usize>, ends: bool) -> LayoutCounts {
        let mut c = LayoutCounts { id: MEASURE_ID, ..LayoutCounts::default() };
        let gaps = if lines.is_empty() { 0..0 } else { lines.start..lines.end - 1 };
        c.lines = lines.len();
        let mut excerpt = !ends || (self.head().is_empty() && self.tail().is_empty());
        let mut skel = Skeleton { head: String::new(), lines: Vec::new(), gaps: Vec::new(), tail: String::new() };
        if ends {
            c.head = escape_ws(self.head());
            c.tail = escape_ws(self.tail());
            skel.head = c.head.clone();
            skel.tail = c.tail.clone();
        }
        for i in lines.clone() {
            let (l, t) = (self.lead(i), self.trail(i));
            c.leading_ws_lines += !l.is_empty() as usize;
            c.trailing_ws_lines += !t.is_empty() as usize;
            if !l.is_empty() {
                *c.leading_ws.entry(escape_ws(l)).or_default() += 1;
            }
            if !t.is_empty() {
                *c.trailing_ws.entry(escape_ws(t)).or_default() += 1;
            }
            let b = self.body(i);
            c.emphasis_pairs += emphasis_pairs(b);
            excerpt &= l.is_empty() && t.is_empty() && text::squash(b) == b;
            skel.lines.push([escape_ws(l), escape_ws(t)]);
        }
        for i in gaps {
            let g = &self.gaps[i];
            let sep = escape_ws(self.separator(i));
            excerpt &= self.separator(i) == "\n\n";
            if g.newlines == 1 {
                c.single_breaks += 1;
            } else {
                c.blank_line_runs += 1;
                c.blank_lines += g.newlines - 1;
            }
            match g.kind {
                GapKind::Wrap => c.wrap_breaks += 1,
                GapKind::SentenceBreak => c.sentence_breaks += 1,
                GapKind::LineBreak => c.line_breaks += 1,
                GapKind::BlankRun => {}
            }
            if g.boundary() {
                c.boundaries += 1;
                *c.separators.entry(sep.clone()).or_default() += 1;
            }
            skel.gaps.push([sep, format!("{:?}", g.kind)]);
        }
        c.hard_wrapped = self.hard_wrapped;
        c.width = self.width;
        let paras: Vec<&Paragraph> = self.paragraphs.iter().filter(|p| p.lines.start >= lines.start && p.lines.end <= lines.end && !lines.is_empty()).collect();
        c.paragraphs = paras.len();
        let mut reasons: Vec<String> = paras.iter().filter_map(|p| p.reason.map(|r| format!("form_paragraph_{r}"))).collect();
        reasons.sort();
        reasons.dedup();
        c.screen_reasons = reasons;
        c.passes_screen = c.screen_reasons.is_empty();
        c.screen_fail_lines = paras.iter().filter(|p| p.reason.is_some()).map(|p| p.lines.len()).sum();
        c.line_form_fail_lines = paras.iter().filter(|p| p.reason.is_some() && !p.wrapped).map(|p| p.lines.len()).sum();
        c.line_form_fail_lines_evidence = paras.iter().filter(|p| p.reason.is_some() && !p.evidence_wrapped).map(|p| p.lines.len()).sum();
        c.diag = self.diag.clone();
        c.class = if c.lines == 0 {
            LayoutClass::Empty
        } else if 2 * c.line_form_fail_lines > c.lines {
            LayoutClass::LineForm
        } else if c.hard_wrapped {
            LayoutClass::HardWrapped
        } else if excerpt {
            LayoutClass::Excerpt
        } else {
            LayoutClass::Prose
        };
        c.skeleton_sha256 = format!("{:x}", Sha256::digest(serde_json::to_string(&skel).expect("skeleton json").as_bytes()));
        c
    }
}

#[derive(Serialize)]
struct Skeleton {
    head: String,
    lines: Vec<[String; 2]>,
    gaps: Vec<[String; 2]>,
    tail: String,
}

/// Text-free layout counts of a measured region: what the layout audit
/// counts and what a profile v2 record holds for its source.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LayoutCounts {
    pub id: &'static str,
    pub class: LayoutClass,
    /// Content lines.
    pub lines: usize,
    /// Blank lines inside gaps (the head and tail are recorded as strings).
    pub blank_lines: usize,
    pub paragraphs: usize,
    pub boundaries: usize,
    /// Gaps with one newline (single `"\n"` per region).
    pub single_breaks: usize,
    /// Single breaks read as paragraph boundaries.
    pub sentence_breaks: usize,
    /// Single breaks read as wraps.
    pub wrap_breaks: usize,
    /// Single breaks inside a sentence that are not wraps.
    pub line_breaks: usize,
    /// Gaps with blank lines.
    pub blank_line_runs: usize,
    pub leading_ws_lines: usize,
    pub trailing_ws_lines: usize,
    /// The whole text is hard-wrapped (for a region too: wrapping is a
    /// property of the whole text).
    pub hard_wrapped: bool,
    /// W in columns, when hard-wrapped (the whole text's).
    pub width: Option<usize>,
    pub emphasis_pairs: usize,
    /// Boundary separator strings (escaped) and their counts.
    pub separators: BTreeMap<String, usize>,
    /// Leading and trailing whitespace strings (escaped) and their counts.
    pub leading_ws: BTreeMap<String, usize>,
    pub trailing_ws: BTreeMap<String, usize>,
    /// The head and tail (escaped; whole texts only).
    pub head: String,
    pub tail: String,
    /// Form-screen reasons of the region's failing paragraphs.
    pub screen_reasons: Vec<String>,
    /// Every region paragraph passes the form screen.
    pub passes_screen: bool,
    /// Content lines in region paragraphs that fail the form screen.
    pub screen_fail_lines: usize,
    /// Content lines in failing region paragraphs that hold no wrap: the
    /// region is line-form when they are more than half of its lines
    /// (amendment 10 item 10.2(e)).
    pub line_form_fail_lines: usize,
    /// Content lines in failing region paragraphs that hold no first-pass
    /// evidence wrap (amendment 13 item 13.9's count: the region would be
    /// line-form if a second-pass wrap did not exempt its paragraph).
    pub line_form_fail_lines_evidence: usize,
    /// The whole text's first-pass, second-pass, lead-change, unwrapped-line
    /// and frame counts (amendment 10 item 10.12).
    pub diag: MeasureDiag,
    /// SHA-256 of the whitespace skeleton: head, each line's leading and
    /// trailing strings, each gap's separator and kind, tail (escaped, JSON).
    pub skeleton_sha256: String,
}

/// Byte ranges of the paragraphs of `t` (the profile v2 paragraph rule).
pub fn paragraph_ranges(t: &str) -> Vec<(usize, usize)> {
    Measure::of(t).paragraphs.iter().map(|p| p.range).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_gaps_head_and_tail_are_exact() {
        let t = "\n  First line here.  \n\tSecond line.\n \n\nThird.\r\n\n";
        let m = Measure::of(t);
        assert_eq!(m.lines.len(), 3);
        assert_eq!((m.lead(0), m.body(0), m.trail(0)), ("  ", "First line here.", "  "));
        assert_eq!((m.lead(1), m.body(1), m.trail(1)), ("\t", "Second line.", ""));
        assert_eq!((m.lead(2), m.body(2), m.trail(2)), ("", "Third.", "\r"));
        assert_eq!(m.separator(0), "\n");
        assert_eq!(m.separator(1), "\n \n\n");
        assert_eq!((m.head(), m.tail()), ("\n", "\n\n"));
        assert_eq!(m.gaps[1].newlines, 3);
        // Every byte is accounted for: head, lines, gaps, tail.
        let mut rebuilt = m.head().to_string();
        for i in 0..m.lines.len() {
            rebuilt.push_str(&t[m.lines[i].start..m.lines[i].end]);
            if i < m.gaps.len() {
                rebuilt.push_str(m.separator(i));
            }
        }
        rebuilt.push_str(m.tail());
        assert_eq!(rebuilt, t);
    }

    #[test]
    fn excerpt_layout_is_its_own_class() {
        let t = "One two three four five six seven eight.\n\nNine ten eleven twelve thirteen fourteen fifteen sixteen.";
        let c = Measure::of(t).counts();
        assert_eq!(c.class, LayoutClass::Excerpt);
        assert_eq!((c.lines, c.paragraphs, c.boundaries, c.single_breaks, c.blank_line_runs), (2, 2, 1, 0, 1));
        assert_eq!(c.separators, BTreeMap::from([("\\n\\n".to_string(), 1)]));
        assert!(!c.hard_wrapped && c.width.is_none());
        assert_eq!(crate::layout::excerpt_layout(t), t);
        let c2 = Measure::of(&format!("{t} ")).counts();
        assert_eq!(c2.class, LayoutClass::Prose);
        assert_eq!(c2.trailing_ws, BTreeMap::from([("\\s".to_string(), 1)]));
    }

    #[test]
    fn hard_wrap_width_and_the_fit_rule() {
        // Width 30: the first line ends inside a sentence, and "over" plus
        // "the" (30 + 1 + 3) would not have fit within the block's longest line.
        let t = "The quick brown fox jumps over\nthe lazy dog. It ran.\nThen it slept under the old\noak tree for an hour.";
        let m = Measure::of(t);
        assert!(m.hard_wrapped);
        assert_eq!(m.width, Some(30));
        assert_eq!(m.gaps[0].kind, GapKind::Wrap);
        // "the lazy dog. It ran." (21) + 1 + "Then" (4) = 26 <= 30: a boundary.
        assert_eq!(m.gaps[1].kind, GapKind::SentenceBreak);
        assert_eq!(m.gaps[2].kind, GapKind::Wrap);
        assert_eq!(m.paragraphs.len(), 2);
        // A sentence end whose next word would not fit is a wrap.
        // "the lazy dog and the cat ran." (29) + 1 + "Then" (4) = 34 > 30.
        let u = "The quick brown fox jumps over\nthe lazy dog and the cat ran.\nThen it slept.";
        let mu = Measure::of(u);
        assert_eq!(mu.width, Some(30));
        assert!(mu.gaps[1].after_sentence_end);
        assert_eq!(mu.gaps.iter().map(|g| g.kind).collect::<Vec<_>>(), vec![GapKind::Wrap, GapKind::Wrap]);
        assert_eq!(mu.paragraphs.len(), 1);
        // The fit rule is inclusive: body, one space and the next word at exactly W.
        let x = format!("The quick brown fox jumps over\nthe dog.\n{} rests.", "y".repeat(30 - "the dog.".len() - 1));
        let mx = Measure::of(&x);
        assert_eq!(mx.width, Some(30));
        assert_eq!(mx.gaps[1].kind, GapKind::SentenceBreak);
        let x2 = format!("The quick brown fox jumps over\nthe dog.\n{} rests.", "y".repeat(30 - "the dog.".len()));
        assert_eq!(Measure::of(&x2).gaps[1].kind, GapKind::Wrap);
        // Without an in-sentence break every single break after a sentence end is a boundary.
        let v = "First paragraph ends here.\nSecond one too.";
        let mv = Measure::of(v);
        assert!(!mv.hard_wrapped);
        assert_eq!(mv.gaps[0].kind, GapKind::SentenceBreak);
        // W is the longest line before wrap evidence, wherever it is.
        let w = "The quick brown fox jumps\noverhead while the lazy dogs\nslept under the oak tree.";
        let mw = Measure::of(w);
        assert_eq!(mw.gaps.iter().map(|g| g.kind).collect::<Vec<_>>(), vec![GapKind::Wrap, GapKind::Wrap]);
        assert_eq!(mw.width, Some(28));
    }

    #[test]
    fn wrap_evidence_needs_the_fit_and_a_prose_run() {
        // A title before a longer line: the next word would have fit, so the
        // break is a line break and the text is not hard-wrapped.
        let titled = "A Walk by the River\nWe walked along the river path until the light began to fade behind the hills.\nThe dogs ran ahead of us and waited at every gate along the way home.";
        let m = Measure::of(titled);
        assert!(!m.hard_wrapped && m.width.is_none());
        assert_eq!(m.gaps.iter().map(|g| g.kind).collect::<Vec<_>>(), vec![GapKind::LineBreak, GapKind::SentenceBreak]);
        assert_eq!(m.counts().line_breaks, 1);
        // A short sign-off block a wrapper at its own width could have made is
        // not prose: no wrap evidence.
        let signed = "The report is attached and the figures for the third quarter are in the second table.\n\nThanks,\nSam";
        let ms = Measure::of(signed);
        assert!(!ms.hard_wrapped);
        assert_eq!(ms.gaps[1].kind, GapKind::LineBreak);
        // The fit is strict: line (15) + 1 + next word (2) = 18 is not longer
        // than a block whose longest line is 18, and is longer than one of 17.
        let eq = "Aaaa bbbb cc dd\nee ff gg hh ii jj.";
        assert_eq!(Measure::of(eq).gaps[0].kind, GapKind::LineBreak);
        let gt = "Aaaa bbbb cc dd\nee ff gg hh ii j.";
        let mg = Measure::of(gt);
        assert_eq!(mg.gaps[0].kind, GapKind::Wrap);
        // W counts the run's last line too (amendment 13 item 13.5).
        assert_eq!(mg.width, Some(17));
        // The run must read as prose: the same geometry with a list item is
        // a line break, and so is a short block ending in a sentence.
        let item = "- Aaaa bbbb cc dd\nee ff gg hh ii k.";
        assert_eq!(Measure::of(item).gaps[0].kind, GapKind::LineBreak);
        let open = "Aaaa bbbb cc dd\nee ff gg hh ii jj";
        assert_eq!(Measure::of(open).gaps[0].kind, GapKind::LineBreak, "the run must end a sentence");
        let short = "The report is attached and the figures are in the second table of it.\n\nThanks for\neverything.";
        let mshort = Measure::of(short);
        assert_eq!((mshort.gaps[1].kind, mshort.hard_wrapped), (GapKind::LineBreak, false));
        // A short wrapped sentence inside a longer block is a wrap: the run
        // "Hhh… iii… jj." holds three words, its block nine; W is 24 and the
        // first line (22) is no wider, so the sentence-end break is a wrap too.
        let inner = "Aaaa bbbb cc dd ee ff.\nHhhhhhhhhhhh iiiiiiiiiii\njj.";
        assert_eq!(Measure::of(inner).gaps.iter().map(|g| g.kind).collect::<Vec<_>>(), vec![GapKind::Wrap, GapKind::Wrap]);
        // A first line wider than W outside every evidence run: amendment
        // 10 item 10.2(b) made it unwrapped at 25 columns against W 24.
        // Under amendment 13 item 13.5 it is unwrapped only when it is wider
        // than W by more than its last word and one space, so at 25 the
        // sentence-end break after it is a wrap again (amendment 7's
        // reading), and at 31 (more than 24 + 1 + 3) it is unwrapped and the
        // break a boundary.
        let wide = "Aaaa bbbb cc dd ee ff gg.\nHhhhhhhhhhhh iiiiiiiiiii\njj.";
        let mw = Measure::of(wide);
        assert_eq!(mw.gaps.iter().map(|g| g.kind).collect::<Vec<_>>(), vec![GapKind::Wrap, GapKind::Wrap]);
        assert_eq!((mw.width, mw.unwrapped.clone(), mw.diag.unwrapped_within_one_word), (Some(24), vec![false, false, false], 1));
        let wider = "Aaaa bbbb cc dd ee ff gg hh ii.\nHhhhhhhhhhhh iiiiiiiiiii\njj.";
        let mx = Measure::of(wider);
        assert_eq!(mx.gaps.iter().map(|g| g.kind).collect::<Vec<_>>(), vec![GapKind::SentenceBreak, GapKind::Wrap]);
        assert_eq!((mx.width, mx.unwrapped.clone()), (Some(24), vec![true, false, false]));
    }

    #[test]
    fn line_form_needs_most_lines_in_failing_paragraphs() {
        let prose = "The museum reopened last week after two years of repairs to the roof and hall.\nVisitors queued from early morning and the staff handed out maps at the door.\nWorth a visit!";
        let c = Measure::of(prose).counts();
        assert_eq!((c.lines, c.screen_fail_lines, c.passes_screen), (3, 1, false));
        assert_eq!(c.class, LayoutClass::Prose);
        let lines = "Bring these things for the walk on the hills this weekend please.\n\n- a torch\n- two warm hats\n- the old map";
        let l = Measure::of(lines).counts();
        assert_eq!((l.lines, l.screen_fail_lines), (4, 3));
        assert_eq!(l.class, LayoutClass::LineForm);
        // Exactly half is not more than half.
        let half = "Bring these things for the walk on the hills this weekend please.\n\n- a torch";
        assert_eq!(Measure::of(half).counts().class, LayoutClass::Excerpt);
    }

    #[test]
    fn sentence_ends_follow_the_sealed_splitter() {
        assert!(ends_sentence("It ended."));
        assert!(ends_sentence("He said \u{201C}stop.\u{201D}"));
        assert!(ends_sentence("Wait\u{2026}"));
        assert!(ends_sentence("(see above.)"));
        assert!(!ends_sentence("A title line"));
        assert!(!ends_sentence("ends with a comma,"));
        assert!(!ends_sentence("3.14"));
    }

    #[test]
    fn line_form_and_emphasis() {
        let irc = "<ann> hi all\n<bob> hey ann, how is it going today?\n<ann> fine";
        let c = Measure::of(irc).counts();
        assert_eq!(c.class, LayoutClass::LineForm);
        assert!(!c.screen_reasons.is_empty());
        assert_eq!(emphasis_pairs("a **b** and __c__ **d"), 2);
        let e = "A **bold** claim was made by the speaker at the start of the long meeting.";
        assert_eq!(Measure::of(e).counts().emphasis_pairs, 1);
    }

    #[test]
    fn escaping_is_exact_and_text_free() {
        assert_eq!(escape_ws("\n \t\r\u{a0}"), "\\n\\s\\t\\r\\u{00a0}");
        assert_eq!(Measure::of("").counts().class, LayoutClass::Empty);
        assert_eq!(Measure::of(" \n ").counts().head, "\\s\\n\\s");
    }

    #[test]
    fn overlapping_paragraphs() {
        let t = "Alpha one two three four five six.\n\nBeta one two three four five six.\n\nGamma one two three four five six.";
        let m = Measure::of(t);
        let b = t.find("Beta").unwrap();
        assert_eq!(m.paragraphs_overlapping(b, b + 4), 1..2);
        assert_eq!(m.paragraphs_overlapping(0, t.len()), 0..3);
        let g = t.find("Gamma").unwrap();
        assert_eq!(m.paragraphs_overlapping(b + 6, g + 3), 1..3);
        // An insertion at a paragraph start overlaps that paragraph.
        assert_eq!(m.paragraphs_overlapping(b, b), 1..2);
    }
}
