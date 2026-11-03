//! The parent-conditioned layout transfer `layout_transfer_v1` of
//! `cc_cohort_profile_v2` (protocol A2 §6.2, as amended by amendment 6).
//!
//! A child takes every whitespace-layout property from its source, measured
//! exactly (`measure`). The source of a mirror is the whole parent; the
//! source of a mixed child is the replaced span's paragraphs and lines:
//! * the paragraphs the span overlaps (whole) give the line-form test, the
//!   form screen's applicability and paired emphasis;
//! * hard wrapping and W are the whole parent's (a wrapper wraps every
//!   paragraph; amendment 6 item 6.3);
//! * the lines the span overlaps give the per-line leading and trailing
//!   whitespace;
//! * the paragraph boundaries inside the span give the separators and, with
//!   the parent words the span's first and last paragraphs join, the break
//!   fractions (as v1's `Structure::span`, by paragraph).
//!
//! An insertion (`SourceLayout::insertion`) is a mixed child whose source is
//! the paragraph holding its point: one rebuilt paragraph, no separator of
//! its own, the parent's W, the line holding the point for its whitespace.
//!
//! A layout donor (a parentless row) is a source like a mirror's parent:
//! `SourceLayout` holds the donor's layout properties only, never its text.
//!
//! **Prose sources** (not line-form):
//! 1. v1's rebuild (`layout::rebuild_child`): lines squashed and blank lines
//!    dropped, paired emphasis stripped unless the source uses it, sites and
//!    quotes rendered in the conventions, paragraphs at the source's break
//!    fractions at sentence boundaries, each at least `MIN_PARAGRAPH_WORDS`.
//! 2. A hard-wrapped source's W greedy-wraps every child paragraph on word
//!    boundaries (lines of at most W characters unless one word is longer;
//!    a mixed child's first line starts at the column where the span starts,
//!    counted in characters of the line's body).
//! 3. Paragraphs are joined by the source's boundary separators in order,
//!    cycled when the child has more; wrapped lines by one newline.
//! 4. Child line j takes the leading and trailing whitespace of source line
//!    (j mod L). A mirror also takes the parent's head and tail. A mixed
//!    child's first line takes no leading and its last line no trailing
//!    whitespace: the parent's own bytes around the span supply them, and the
//!    splice needs a trimmed replacement (amendment 6 item 6.6).
//!
//! **Line-form sources** (more than half of the source's lines in
//! paragraphs failing the screen: IRC, verse, transcripts, lists): the child
//! keeps its own line breaks, which are content. Each line is squashed,
//! emphasis is handled as above and sites and quotes are rendered; the child
//! is neither re-paragraphed nor wrapped. Its blank lines are layout, not
//! content: each break between two of its lines takes the source's boundary
//! separators in order, cycled, as paragraphs do for prose (amendment 6 item
//! 6.4), so a source with no blank line gives a child with none. A source
//! with no boundary (one line) joins the child's lines by one newline
//! (`LINE_FORM_FALLBACK`, counted). Per-line whitespace, head and tail are as
//! for prose.
//!
//! On a CC excerpt-layout parent (squashed lines, `"\n\n"` separators, no
//! head, tail, per-line whitespace, hard wrap or paired emphasis, every
//! paragraph passing the screen) every step past the rebuild is the
//! identity, so the result is v1's `normalise_child` byte for byte. When the
//! source uses paired emphasis, step 1 keeps the child's (amendment 6 item
//! 6.7), so v2 then differs from v1 exactly when the child has paired
//! emphasis.
//!
//! Records (`TransferRecord`, `LayoutCounts`) hold counts, widths, classes,
//! escaped whitespace strings and digests, never text.
use crate::conventions::{Changes, Conventions, render_quotes, render_sites};
use crate::layout::{Layout, Structure, rebuild_child, strip_emphasis};
use crate::measure::{LayoutClass, LayoutCounts, Measure};
use crate::text;
use serde::Serialize;

pub const LAYOUT_TRANSFER_ID: &str = "layout_transfer_v1";
/// The separator between a line-form child's blocks when its source has no
/// paragraph boundary (and so no blank line): one newline.
pub const LINE_FORM_FALLBACK: &str = "\n";
/// The separator between a prose child's paragraphs when its source has no
/// boundary (v1's paragraph separator). A measured source never needs it:
/// it has one boundary per break fraction.
pub const PROSE_FALLBACK: &str = "\n\n";

/// A source's layout properties: whitespace strings, counts, fractions and
/// widths, never text.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SourceLayout {
    /// A mixed child's span (false: a mirror's parent or a donor).
    pub mixed: bool,
    /// The measured properties of the source region.
    pub counts: LayoutCounts,
    pub line_form: bool,
    /// Every source paragraph passes the form screen, so the screen applies
    /// to the child (§6.2).
    pub passes_screen: bool,
    /// W when the source text is hard-wrapped (the whole parent's for a
    /// span).
    pub width: Option<usize>,
    /// The source region uses paired markdown emphasis.
    pub emphasis: bool,
    /// Boundary separators, in order.
    pub separators: Vec<String>,
    /// Leading and trailing whitespace of each source line, in order.
    pub line_ws: Vec<(String, String)>,
    /// A mirror's head and tail ("" for a span).
    pub head: String,
    pub tail: String,
    /// Break fractions and the parent words the first and last paragraphs join.
    pub structure: Structure,
    /// A span's starting column: characters of its line's body before it
    /// (for an insertion, plus its separator's characters after any newline).
    pub column: usize,
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

fn fractions_of(words: &[usize]) -> Vec<f64> {
    let w: Vec<usize> = words.iter().copied().filter(|&x| x > 0).collect();
    let total: usize = w.iter().sum();
    let mut out = Vec::new();
    let mut cum = 0;
    for x in w.iter().take(w.len().saturating_sub(1)) {
        cum += x;
        out.push(cum as f64 / total.max(1) as f64);
    }
    out
}

impl SourceLayout {
    /// A mirror's source (or a donor's): the whole text.
    pub fn whole(parent: &str) -> SourceLayout {
        let m = Measure::of(parent);
        let counts = m.counts();
        let n = m.lines.len();
        SourceLayout {
            mixed: false,
            line_form: counts.class == LayoutClass::LineForm,
            passes_screen: counts.passes_screen,
            width: counts.width,
            emphasis: counts.emphasis_pairs > 0,
            separators: (0..m.gaps.len()).filter(|&i| m.gaps[i].boundary()).map(|i| m.separator(i).to_string()).collect(),
            line_ws: (0..n).map(|i| (m.lead(i).to_string(), m.trail(i).to_string())).collect(),
            head: m.head().to_string(),
            tail: m.tail().to_string(),
            structure: Structure { fractions: fractions_of(&m.paragraphs.iter().map(|p| p.words).collect::<Vec<_>>()), pre_words: 0, post_words: 0 },
            column: 0,
            counts,
        }
    }

    /// A mixed child's source: the span `parent[s..e]`'s paragraphs and
    /// lines. None when the span is not a character-boundary range of the
    /// parent.
    pub fn span(parent: &str, s: usize, e: usize) -> Option<SourceLayout> {
        if s > e || parent.get(s..e).is_none() {
            return None;
        }
        let m = Measure::of(parent);
        let pr = m.paragraphs_overlapping(s, e);
        let region = if pr.is_empty() { 0..0 } else { m.paragraphs[pr.start].lines.start..m.paragraphs[pr.end - 1].lines.end };
        let counts = m.counts_over(region, false);
        let lines: Vec<usize> = (0..m.lines.len()).filter(|&i| m.lines[i].body.0 < e.max(s + 1) && s < m.lines[i].body.1).collect();
        let inside: Vec<usize> = (0..m.gaps.len()).filter(|&i| m.gaps[i].range.0 >= s && m.gaps[i].range.1 <= e).collect();
        let mut seg_words = Vec::new();
        let mut cur = s;
        for &i in inside.iter().filter(|&&i| m.gaps[i].boundary()) {
            seg_words.push(text::words(&parent[cur..m.gaps[i].range.0]));
            cur = m.gaps[i].range.1;
        }
        seg_words.push(text::words(&parent[cur..e]));
        let pre = m.paragraphs.iter().find(|p| p.range.0 <= s && s <= p.range.1);
        let post = m.paragraphs.iter().find(|p| p.range.0 <= e && e <= p.range.1);
        let column = m.lines.iter().find(|l| l.body.0 <= s && s <= l.body.1).map_or(0, |l| chars(&parent[l.body.0..s]));
        Some(SourceLayout {
            mixed: true,
            line_form: counts.class == LayoutClass::LineForm,
            passes_screen: counts.passes_screen,
            width: counts.width,
            emphasis: counts.emphasis_pairs > 0,
            separators: inside.iter().filter(|&&i| m.gaps[i].boundary()).map(|&i| m.separator(i).to_string()).collect(),
            line_ws: lines.iter().map(|&i| (m.lead(i).to_string(), m.trail(i).to_string())).collect(),
            head: String::new(),
            tail: String::new(),
            structure: Structure {
                fractions: fractions_of(&seg_words),
                pre_words: pre.map_or(0, |p| text::words(&parent[p.range.0..s])),
                post_words: post.map_or(0, |p| text::words(&parent[e..p.range.1])),
            },
            column,
            counts,
        })
    }

    /// An insertion's source: the paragraph holding the insertion point `s`
    /// (the last paragraph starting at or before it, else the first), with
    /// the parent's W and the whitespace of the line holding `s` (likewise).
    /// The inserted text is one paragraph: no break fraction and no
    /// separator of its own. When `sep` (the separator assembly puts before
    /// the new text) holds no newline, the text continues the line, so its
    /// column is the line's body characters before `s` plus the separator's;
    /// otherwise it is the characters after the separator's last newline.
    /// None when `s` is not a character boundary of the parent.
    pub fn insertion(parent: &str, s: usize, sep: &str) -> Option<SourceLayout> {
        if !parent.is_char_boundary(s) {
            return None;
        }
        let m = Measure::of(parent);
        let pi = m.paragraphs.iter().rposition(|p| p.range.0 <= s).or((!m.paragraphs.is_empty()).then_some(0));
        let region = pi.map_or(0..0, |k| m.paragraphs[k].lines.clone());
        let counts = m.counts_over(region, false);
        let li = m.lines.iter().rposition(|l| l.body.0 <= s).or((!m.lines.is_empty()).then_some(0));
        let inline = !sep.contains('\n');
        let column = match sep.rfind('\n') {
            Some(k) => chars(&sep[k + 1..]),
            None => li.map_or(0, |i| chars(&parent[m.lines[i].body.0..s.max(m.lines[i].body.0).min(m.lines[i].body.1)])) + chars(sep),
        };
        let para = pi.map(|k| m.paragraphs[k].range);
        Some(SourceLayout {
            mixed: true,
            line_form: counts.class == LayoutClass::LineForm,
            passes_screen: counts.passes_screen,
            width: counts.width,
            emphasis: counts.emphasis_pairs > 0,
            separators: Vec::new(),
            line_ws: li.map(|i| (m.lead(i).to_string(), m.trail(i).to_string())).into_iter().collect(),
            head: String::new(),
            tail: String::new(),
            structure: Structure {
                fractions: Vec::new(),
                pre_words: para.filter(|_| inline).map_or(0, |(a, b)| text::words(&parent[a.min(s)..s.min(b).max(a.min(s))])),
                post_words: para.filter(|_| inline).map_or(0, |(a, b)| text::words(&parent[s.max(a).min(b)..b])),
            },
            column,
            counts,
        })
    }
}

/// Which construction ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Path {
    Prose,
    LineForm,
}

/// What the transfer applied (no text).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TransferRecord {
    pub id: &'static str,
    pub path: Path,
    pub source_class: LayoutClass,
    pub mixed: bool,
    pub emphasis_kept: bool,
    pub wrapped_at: Option<usize>,
    pub child_lines: usize,
    pub child_paragraphs: usize,
    /// Separators taken from the source, and fallbacks used.
    pub separators_applied: usize,
    pub separator_fallbacks: usize,
    /// Child lines given non-empty leading or trailing whitespace.
    pub leading_ws_lines: usize,
    pub trailing_ws_lines: usize,
    pub head_applied: bool,
    pub tail_applied: bool,
}

/// A transferred child.
#[derive(Clone, Debug)]
pub struct Transferred {
    pub text: String,
    pub changes: Changes,
    /// v1's rebuild record (the line-form path places no breaks).
    pub layout: Layout,
    /// Line-level form failures (the prose path, where the screen applies).
    pub line_reasons: Vec<String>,
    /// Byte ranges in `text` of each paragraph, first body start to last
    /// body end (the prose path; each line of a line-form child).
    pub paragraphs: Vec<(usize, usize)>,
    pub record: TransferRecord,
}

/// Greedy wrap of one squashed paragraph at `w` characters, the first line
/// starting at column `col`.
pub fn wrap(p: &str, w: usize, col: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut len = 0;
    let mut limit = w.saturating_sub(col);
    for word in p.split_whitespace() {
        let wl = chars(word);
        if cur.is_empty() {
            cur.push_str(word);
            len = wl;
        } else if len + 1 + wl <= limit {
            cur.push(' ');
            cur.push_str(word);
            len += 1 + wl;
        } else {
            out.push(std::mem::take(&mut cur));
            cur.push_str(word);
            len = wl;
            limit = w;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Lays out `lines` (blocks of child lines) with the source's whitespace:
/// within a block, lines are joined by one newline; between blocks, the
/// separator `sep(k)` for the k-th block break. Returns the text and each
/// block's byte range (first line's body start to last line's body end).
fn lay_out(blocks: &[Vec<String>], src: &SourceLayout, mut sep: impl FnMut(usize) -> String, rec: &mut TransferRecord) -> (String, Vec<(usize, usize)>) {
    let total: usize = blocks.iter().map(Vec::len).sum();
    let mut out = String::new();
    let mut ranges = Vec::new();
    if total == 0 {
        return (out, ranges);
    }
    if !src.mixed && !src.head.is_empty() {
        out.push_str(&src.head);
        rec.head_applied = true;
    }
    let l = src.line_ws.len();
    let mut j = 0;
    for (k, b) in blocks.iter().enumerate() {
        if k > 0 {
            out.push_str(&sep(k - 1));
        }
        let mut start = 0;
        for (i, line) in b.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let (lead, trail) = if l == 0 { ("", "") } else { (src.line_ws[j % l].0.as_str(), src.line_ws[j % l].1.as_str()) };
            let lead = if src.mixed && j == 0 { "" } else { lead };
            let trail = if src.mixed && j + 1 == total { "" } else { trail };
            out.push_str(lead);
            if i == 0 {
                start = out.len();
            }
            out.push_str(line);
            let end = out.len();
            out.push_str(trail);
            rec.leading_ws_lines += !lead.is_empty() as usize;
            rec.trailing_ws_lines += !trail.is_empty() as usize;
            if i + 1 == b.len() {
                ranges.push((start, end));
            }
            j += 1;
        }
    }
    if !src.mixed && !src.tail.is_empty() {
        out.push_str(&src.tail);
        rec.tail_applied = true;
    }
    rec.child_lines = total;
    rec.child_paragraphs = blocks.len();
    (out, ranges)
}

/// `layout_transfer_v1`: the raw child `t` laid out like `src`, in the
/// conventions `conv` (see the module notes).
pub fn transfer(t: &str, conv: &Conventions, src: &SourceLayout) -> Transferred {
    let mut rec = TransferRecord {
        id: LAYOUT_TRANSFER_ID,
        path: if src.line_form { Path::LineForm } else { Path::Prose },
        source_class: src.counts.class,
        mixed: src.mixed,
        emphasis_kept: src.emphasis,
        wrapped_at: None,
        child_lines: 0,
        child_paragraphs: 0,
        separators_applied: 0,
        separator_fallbacks: 0,
        leading_ws_lines: 0,
        trailing_ws_lines: 0,
        head_applied: false,
        tail_applied: false,
    };
    if !src.line_form {
        let r = rebuild_child(t, conv, &src.structure, src.emphasis);
        let blocks: Vec<Vec<String>> = r
            .paragraphs
            .iter()
            .enumerate()
            .map(|(k, p)| match src.width {
                Some(w) => wrap(p, w, if k == 0 { src.column } else { 0 }),
                None => vec![p.clone()],
            })
            .collect();
        rec.wrapped_at = src.width;
        let n = src.separators.len();
        let mut applied = 0;
        let mut fallbacks = 0;
        let (text, paragraphs) = lay_out(
            &blocks,
            src,
            |k| {
                if n == 0 {
                    fallbacks += 1;
                    PROSE_FALLBACK.to_string()
                } else {
                    applied += 1;
                    src.separators[k % n].clone()
                }
            },
            &mut rec,
        );
        rec.separators_applied = applied;
        rec.separator_fallbacks = fallbacks;
        return Transferred { text, changes: r.changes, layout: r.layout, line_reasons: r.line_reasons, paragraphs, record: rec };
    }
    // Line-form: the child's own lines in order, each its own block, so every
    // break between them takes the source's boundary separators in order.
    let mut ch = Changes::default();
    let mut blocks: Vec<Vec<String>> = Vec::new();
    let mut sentences = 0;
    for l in t.split('\n') {
        let s = text::squash(l);
        if s.is_empty() {
            continue;
        }
        ch.trailing_space_lines += l.ends_with(char::is_whitespace) as usize;
        ch.leading_space_lines += l.starts_with(char::is_whitespace) as usize;
        ch.squashed_lines += (s.as_str() != l.trim()) as usize;
        let s = if src.emphasis { s } else { strip_emphasis(&s, &mut ch) };
        if s.is_empty() {
            continue;
        }
        let s = render_sites(&s, conv, &mut ch);
        let s = render_quotes(&s, conv, &mut ch);
        sentences += text::sentences(&s).len();
        blocks.push(vec![s]);
    }
    let n = src.separators.len();
    let mut applied = 0;
    let mut fallbacks = 0;
    let (text, paragraphs) = lay_out(
        &blocks,
        src,
        |k| {
            if n == 0 {
                fallbacks += 1;
                LINE_FORM_FALLBACK.to_string()
            } else {
                applied += 1;
                src.separators[k % n].clone()
            }
        },
        &mut rec,
    );
    rec.separators_applied = applied;
    rec.separator_fallbacks = fallbacks;
    let own_lines = blocks.iter().map(Vec::len).sum();
    Transferred {
        text,
        changes: ch,
        layout: Layout {
            own_lines,
            sentences,
            target_breaks: src.structure.fractions.len(),
            realised_breaks: 0,
            dropped_breaks: 0,
            pre_words: src.structure.pre_words,
            post_words: src.structure.post_words,
        },
        line_reasons: Vec::new(),
        paragraphs,
        record: rec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conventions::Choice;

    fn none() -> Conventions {
        fn c<K>() -> Choice<K> {
            Choice { value: None, source: "none" }
        }
        Conventions { double: c(), single: c(), dash: c(), range: c(), ellipsis: c() }
    }

    #[test]
    fn greedy_wrap_fills_lines_to_the_width() {
        assert_eq!(wrap("aa bb cc dd", 5, 0), vec!["aa bb", "cc dd"]);
        assert_eq!(wrap("aa bb cc dd", 8, 0), vec!["aa bb cc", "dd"]);
        // The first line starts at a column.
        assert_eq!(wrap("aa bb cc dd", 5, 3), vec!["aa", "bb cc", "dd"]);
        // A word longer than the width sits alone.
        assert_eq!(wrap("a verylongword b", 4, 0), vec!["a", "verylongword", "b"]);
        assert!(wrap("", 10, 0).is_empty());
    }

    #[test]
    fn a_single_paragraph_source_gives_one_paragraph() {
        let parent = "One two three four five six seven eight nine ten eleven twelve.";
        let child = "Alpha beta gamma delta epsilon zeta eta theta.\n\nIota kappa lambda mu nu xi omicron pi.";
        let t = transfer(child, &none(), &SourceLayout::whole(parent));
        assert_eq!(t.text, "Alpha beta gamma delta epsilon zeta eta theta. Iota kappa lambda mu nu xi omicron pi.");
        assert_eq!(t.record.child_paragraphs, 1);
        assert_eq!(t.record.separators_applied, 0);
    }

    #[test]
    fn span_sources_are_the_replaced_paragraphs_and_lines() {
        let parent = "Alpha one two three four five six. Beta one two three four five six.\n\nGamma one two three four five six. Delta one two three four five.";
        let s = parent.find("Beta").unwrap();
        let e = parent.find("Delta").unwrap() - 1;
        let src = SourceLayout::span(parent, s, e).unwrap();
        // As v1's Structure::span on this excerpt-layout parent.
        assert_eq!(src.structure, crate::layout::Structure::span(parent, s, e));
        assert_eq!(src.separators, vec!["\n\n".to_string()]);
        assert_eq!(src.column, chars("Alpha one two three four five six. "));
        assert_eq!(src.counts.class, LayoutClass::Excerpt);
        assert!(src.passes_screen && !src.line_form);
        assert!(SourceLayout::span(parent, 5, 3).is_none());
        assert!(SourceLayout::span(parent, 0, parent.len() + 1).is_none());
    }
}
