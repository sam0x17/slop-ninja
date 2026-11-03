//! Layout measurement for `cc_cohort_profile_v2` (protocol A2 §6.2, as
//! amended by amendment 6): the whitespace-layout properties
//! `layout_transfer_v1` copies from a source, and the counts the layout
//! audit (`r2_layout_audit_v1`), the pilot gate and the form-injection twins
//! read. Records built from it hold counts, widths, reason codes, escaped
//! whitespace strings and digests, never text.
//!
//! **Lines.** A text is split at `'\n'`. A line holding only whitespace is
//! blank; every other line is a content line with its leading whitespace,
//! its body and its trailing whitespace (Unicode `White_Space`, as
//! `str::trim`; a `'\r'` before the newline is trailing whitespace). The
//! blank lines before the first content line are the head, and everything
//! after the last content line's trailing whitespace is the tail. Widths
//! are body lengths in Unicode scalar values (characters); leading and
//! trailing whitespace are measured separately, as per-line strings.
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
//! **Hard wrapping** (amendment 6 item 6.2). A run is a maximal run of
//! content lines joined by single breaks inside a sentence. Its breaks are
//! wrap evidence when a greedy wrapper could have made every one of them and
//! the run is prose:
//! * (fit) for each break, after line i: line i's body, one space and the
//!   next line's first word are longer than the longest body in line i's
//!   block (the next word did not fit); and
//! * (prose) the run is not list-like (`screen::list_like`), its last line
//!   ends a sentence, and its block holds at least `MIN_PARAGRAPH_WORDS`
//!   (inherited) whitespace words.
//!
//! The text is hard-wrapped iff some break is wrap evidence; its width W is
//! the longest body of a line followed by wrap evidence. A single break
//! inside a sentence that is not wrap evidence (a title or lead-in before a
//! longer line, verse lines, a signature, list items) is a line break.
//!
//! **Boundaries.** A single break is a paragraph boundary unless it is a
//! wrap, as every line break is in v1: every blank-line run and every line
//! break is a boundary. A single break after a sentence end is a boundary
//! when the text is not hard-wrapped; in a hard-wrapped text it is one only
//! if the line's body, one space and the next line's first word fit within W
//! (a wrapper would not have broken there), and otherwise a wrap.
//!
//! **Paragraphs** are the maximal runs of content lines between boundaries.
//! Each is classified with the form screen's `screen::classify` (no site
//! template). A region passes the screen when all its paragraphs pass it,
//! and it is line-form (amendment 6 item 6.1) when more than half of its
//! content lines lie in paragraphs that fail it.
//!
//! **Emphasis.** Paired `**` or `__` markers within one line body, counted
//! as `layout::strip_emphasis` pairs them.
//!
//! Working memory and output are linear in the input.
use crate::layout::MIN_PARAGRAPH_WORDS;
use crate::screen::{classify, list_like};
use crate::text;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ops::Range;

/// The measurement rule's id (recorded with every count block).
pub const MEASURE_ID: &str = "layout_measure_v1";

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
    /// A single break inside a paragraph that a wrapper made: wrap evidence
    /// inside a sentence, or a break after a sentence end where the next
    /// word would not have fit within W.
    Wrap,
    /// A single break inside a sentence that is not wrap evidence (a title
    /// or lead-in before a longer line, verse, a signature, list items): a
    /// paragraph boundary, as every line break is in v1.
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
}

/// The layout class of a measured region (records and audits).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutClass {
    /// No content line.
    #[default]
    Empty,
    /// More than half of the region's content lines lie in paragraphs that
    /// fail the form screen (IRC, verse, transcripts, lists).
    LineForm,
    /// Prose in a hard-wrapped text (some single break is wrap evidence).
    HardWrapped,
    /// Prose in `layout::excerpt_layout` form: squashed lines, one empty
    /// line between lines, no head or tail.
    Excerpt,
    /// Any other prose layout (single-newline paragraphs, per-line
    /// whitespace, other separators).
    Prose,
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
    /// Measures `text` (see the module notes).
    pub fn of(text: &'a str) -> Measure<'a> {
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
        let mut gaps: Vec<Gap> = lines
            .windows(2)
            .map(|w| {
                let range = (w[0].end, w[1].start);
                Gap {
                    range,
                    newlines: text[range.0..range.1].matches('\n').count(),
                    after_sentence_end: ends_sentence(&text[w[0].body.0..w[0].body.1]),
                    kind: GapKind::Wrap,
                }
            })
            .collect();
        let body_chars = |l: &Line| chars(&text[l.body.0..l.body.1]);
        let first_word = |l: &Line| text[l.body.0..l.body.1].split_whitespace().next().map_or(0, chars);
        // Blocks (lines joined by single breaks): their longest bodies and
        // their words.
        let mut block_width = vec![0usize; lines.len()];
        let mut block_words = vec![0usize; lines.len()];
        let mut a = 0;
        for i in 0..lines.len() {
            if i + 1 == lines.len() || gaps[i].newlines >= 2 {
                let w = (a..=i).map(|k| body_chars(&lines[k])).max().unwrap_or(0);
                let n = text::words(&text[lines[a].body.0..lines[i].body.1]);
                block_width[a..=i].iter_mut().for_each(|x| *x = w);
                block_words[a..=i].iter_mut().for_each(|x| *x = n);
                a = i + 1;
            }
        }
        // Wrap evidence, run by run: a run (lines joined by single breaks
        // inside a sentence) is wrapped when a greedy wrapper could have made
        // every one of its breaks (the next word did not fit within the
        // block's longest line) and it reads as running prose: not a list
        // item, ending a sentence, in a block of at least
        // `MIN_PARAGRAPH_WORDS` words.
        let fits = |i: usize| body_chars(&lines[i]) + 1 + first_word(&lines[i + 1]) > block_width[i];
        let mut evidence = vec![false; gaps.len()];
        let mut a = 0;
        for i in 0..lines.len() {
            if i + 1 == lines.len() || !gaps[i].in_sentence() {
                let run = &text[lines[a].body.0..lines[i].body.1];
                let prose = !list_like(run) && ends_sentence(&text[lines[i].body.0..lines[i].body.1]) && block_words[i] >= MIN_PARAGRAPH_WORDS;
                if i > a && prose && (a..i).all(fits) {
                    evidence[a..i].iter_mut().for_each(|x| *x = true);
                }
                a = i + 1;
            }
        }
        let width: Option<usize> = (0..gaps.len()).filter(|&i| evidence[i]).map(|i| body_chars(&lines[i])).max();
        let hard_wrapped = width.is_some();
        for (i, g) in gaps.iter_mut().enumerate() {
            g.kind = if g.newlines >= 2 {
                GapKind::BlankRun
            } else if !g.after_sentence_end {
                if evidence[i] { GapKind::Wrap } else { GapKind::LineBreak }
            } else if let Some(w) = width {
                if body_chars(&lines[i]) + 1 + first_word(&lines[i + 1]) <= w { GapKind::SentenceBreak } else { GapKind::Wrap }
            } else {
                GapKind::SentenceBreak
            };
        }
        let mut paragraphs = Vec::new();
        let mut a = 0;
        for i in 0..lines.len() {
            if i + 1 == lines.len() || gaps[i].boundary() {
                let range = (lines[a].body.0, lines[i].body.1);
                let t = &text[range.0..range.1];
                paragraphs.push(Paragraph { lines: a..i + 1, range, words: text::words(t), reason: classify(t, false).reason });
                a = i + 1;
            }
        }
        Measure { text, lines, gaps, paragraphs, hard_wrapped, width }
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
    /// lie in paragraphs that fail the screen (amendment 6 item 6.1).
    pub fn line_form(&self) -> bool {
        self.counts().class == LayoutClass::LineForm
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
    /// head and tail count only when `ends` (the whole text). Hard wrapping
    /// and W are the whole text's (a wrapper wraps every paragraph); the
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
        c.class = if c.lines == 0 {
            LayoutClass::Empty
        } else if 2 * c.screen_fail_lines > c.lines {
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
    /// Single breaks inside a sentence that are not wrap evidence.
    pub line_breaks: usize,
    /// Gaps with blank lines.
    pub blank_line_runs: usize,
    pub leading_ws_lines: usize,
    pub trailing_ws_lines: usize,
    /// The whole text is hard-wrapped (for a region too: wrapping is a
    /// property of the whole text).
    pub hard_wrapped: bool,
    /// W, when hard-wrapped (the whole text's).
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
    /// Content lines in region paragraphs that fail the form screen (the
    /// region is line-form when they are more than half of its lines).
    pub screen_fail_lines: usize,
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
        assert_eq!(mg.width, Some(15));
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
        // "Hhh… iii… jj." holds three words, its block ten.
        let inner = "Aaaa bbbb cc dd ee ff gg.\nHhhhhhhhhhhh iiiiiiiiiii\njj.";
        assert_eq!(Measure::of(inner).gaps.iter().map(|g| g.kind).collect::<Vec<_>>(), vec![GapKind::Wrap, GapKind::Wrap]);
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
