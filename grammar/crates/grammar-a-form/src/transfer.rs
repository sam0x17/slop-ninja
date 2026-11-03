//! The parent-conditioned layout transfer `layout_transfer_v2` of
//! `cc_cohort_profile_v2` (protocol A2 §6.2, as amended by amendments 7, 10
//! and 13).
//!
//! A child takes every whitespace-layout property from its source, measured
//! exactly (`measure`, `layout_measure_v2`). The source of a mirror is the
//! whole parent; the source of a mixed child is the replaced span's
//! paragraphs and lines:
//! * the paragraphs the span overlaps (whole) give the line-form test, the
//!   licence (amendment 10 item 10.5) and paired emphasis;
//! * hard wrapping and W are the whole parent's (one W per document;
//!   amendment 7 item 7.3);
//! * the lines the span overlaps give the whitespace pools;
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
//! 1. **Edge frames** (amendment 10 item 10.4; whole-text sources only).
//!    When the source has a head or tail frame, the raw child is measured;
//!    on each side where the source has a frame, the child's outermost
//!    `frame_like` paragraphs are its frame, unless every child paragraph is
//!    `frame_like` (then the child has no frame: the source rule, amendment
//!    13 item 13.8). Frame lines are squashed, emphasis and
//!    conventions handled as for a line-form child, and never re-paragraphed
//!    or wrapped. The break fractions come from the source's body only.
//! 2. **The body.** With no kept unit, v1's rebuild (`layout::rebuild_child`)
//!    exactly: lines squashed and blank lines dropped, paired emphasis
//!    stripped unless the source uses it, sites and quotes rendered,
//!    paragraphs at the source's break fractions at sentence boundaries, each
//!    at least `MIN_PARAGRAPH_WORDS`. **Kept units** arise only for licensed
//!    reasons (amendment 10 item 10.6(b)): with `list_marker` or
//!    `quoted_line` in the licence, each raw child body paragraph holding a
//!    line with that reason keeps its own lines, each its own paragraph;
//!    with `no_sentence_end` in it, each raw child body paragraph of at least
//!    `MIN_PARAGRAPH_WORDS` words whose end is unpunctuated closes its
//!    paragraph there. Kept units are fixed boundaries: each break target
//!    goes to the prose segment holding its fraction, where `place_breaks`
//!    runs, and a target on a fixed boundary or inside a kept unit is
//!    realised by it (counted; positions compared in exact integer
//!    arithmetic).
//! 3. **Wrapping** (amendment 10 items 10.1 and 10.3). A child body
//!    paragraph takes the wrap flag of the source body paragraph holding its
//!    midpoint word fraction; a source body paragraph is wrapped when the
//!    source is hard-wrapped and it holds no unwrapped line. Frame lines and
//!    kept list lines are never wrapped. A wrapped paragraph is greedy-wrapped
//!    on word boundaries: each line's budget is W less the columns of the
//!    lead it takes, a mixed child's first line's W less its column (the
//!    lead and body characters before the span), and a word longer than its
//!    budget stands alone. A mixed child's last line shares its line with
//!    the parent text after the span (`tail_column`): when the two would be
//!    wider than W, the line's maximal suffix of words that fits with that
//!    text moves to a new last line (amendment 13 item 13.7, counted).
//! 4. **Separators** (amendment 10 item 10.4, as amended by amendment 13
//!    item 13.8). The boundary between the head frame and the body takes the
//!    source's last head-side boundary, and the one between the body and the
//!    tail frame the source's first tail-side boundary. Frame-internal
//!    boundaries take the source's frame-internal ones, cycled from the
//!    frame's outer edge (in order on the head side, from the end on the
//!    tail side), or reuse the body-adjacent one when the source frame has
//!    none (counted). Body boundaries take the source's body boundaries,
//!    cycled. Wrapped lines are joined by one newline.
//! 5. **Whitespace by role** (amendment 10 item 10.6(c)). Paragraph-first
//!    lines take the source's paragraph-first leads, continuation lines its
//!    continuation leads, lines followed by a wrap its wrap-followed trails
//!    and paragraph-final lines its paragraph-final trails, each pool cycled;
//!    frame lines take the source's frame lines' strings. An empty pool gives
//!    `""` (counted). A mirror also takes the parent's head and tail. A mixed
//!    child's first line takes no leading and its last line no trailing
//!    whitespace: the parent's own bytes around the span supply them, and the
//!    splice needs a trimmed replacement (amendment 7 item 7.6).
//!
//! **Line-form sources** (more than half of the source's lines in failing
//! paragraphs that hold no wrap: IRC, verse, transcripts, lists): the child
//! keeps its own line breaks, which are content (amendment 7 item 7.4,
//! unchanged). Each line is squashed, emphasis is handled as above and sites
//! and quotes are rendered; the child is neither re-paragraphed nor wrapped.
//! Its blank lines are layout, not content: each break between two of its
//! lines takes the source's boundary separators in order, cycled, so a
//! source with no blank line gives a child with none. A source with no
//! boundary (one line) joins the child's lines by one newline
//! (`LINE_FORM_FALLBACK`, counted). Child line j takes source line (j mod
//! L)'s whitespace, and a mirror the head and tail.
//!
//! On a CC excerpt-layout parent (squashed lines, `"\n\n"` separators, no
//! head, tail, per-line whitespace, hard wrap or paired emphasis, every
//! paragraph passing the screen, so no frame and an empty licence) every
//! step past the rebuild is the identity, so the result is v1's
//! `normalise_child` byte for byte (amendment 7 item 7.7, unchanged by
//! amendment 10). When the source uses paired emphasis, step 2 keeps the
//! child's, so v2 then differs from v1 exactly when the child has paired
//! emphasis.
//!
//! Records (`TransferRecord`, `LayoutCounts`) hold counts, widths, classes,
//! reason names, escaped whitespace strings and digests, never text.
use crate::conventions::{Changes, Conventions, render_quotes, render_sites};
use crate::layout::{Layout, MIN_PARAGRAPH_WORDS, ParagraphSource, Structure, layout_lines, rebuild_child, strip_emphasis};
use crate::measure::{GapKind, LayoutClass, LayoutCounts, Measure};
use crate::screen::{LIST_MARKER, QUOTED_LINE, classify_all, list_reason};
use crate::text;
use serde::Serialize;
use std::collections::BTreeSet;

pub const LAYOUT_TRANSFER_ID: &str = "layout_transfer_v2";
/// The separator between a line-form child's blocks when its source has no
/// paragraph boundary (and so no blank line): one newline.
pub const LINE_FORM_FALLBACK: &str = "\n";
/// The separator between a prose child's paragraphs when its source has no
/// boundary of that kind (v1's paragraph separator, counted).
pub const PROSE_FALLBACK: &str = "\n\n";
/// The licence reason that closes an unpunctuated body paragraph.
pub const NO_SENTENCE_END: &str = "no_sentence_end";
/// Reasons every frame licence holds (amendment 10 item 10.5).
pub const FRAME_LICENCE_BASE: [&str; 2] = ["short_paragraph", NO_SENTENCE_END];

/// A source's whitespace pools by role (amendment 10 item 10.6(c)), in
/// source line order.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RolePools {
    /// Leads of lines that start a paragraph.
    pub first_leads: Vec<String>,
    /// Leads of lines that follow a wrap.
    pub cont_leads: Vec<String>,
    /// Trails of lines followed by a wrap.
    pub wrap_trails: Vec<String>,
    /// Trails of lines that end a paragraph.
    pub final_trails: Vec<String>,
}

/// A whole-text source's edge frames (amendment 10 item 10.4).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SourceFrames {
    pub head: bool,
    pub tail: bool,
    /// Each head frame line's (leading, trailing) whitespace, in order.
    pub head_ws: Vec<(String, String)>,
    /// Each tail frame line's whitespace, in order (read from the end).
    pub tail_ws: Vec<(String, String)>,
    /// The boundary after each head frame paragraph, in order.
    pub head_seps: Vec<String>,
    /// The boundary before each tail frame paragraph, in order (read from
    /// the end).
    pub tail_seps: Vec<String>,
}

/// A source's layout properties: whitespace strings, counts, fractions,
/// widths and reason names, never text.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SourceLayout {
    /// A mixed child's span (false: a mirror's parent or a donor).
    pub mixed: bool,
    /// The measured properties of the source region.
    pub counts: LayoutCounts,
    pub line_form: bool,
    /// Every source paragraph passes the form screen (then the licence is
    /// empty and there is no frame, so the child gets §6.2's screen).
    pub passes_screen: bool,
    /// W in columns when the source text is hard-wrapped (the whole
    /// parent's for a span).
    pub width: Option<usize>,
    /// The source region uses paired markdown emphasis.
    pub emphasis: bool,
    /// Body boundary separators, in order (every boundary of the region
    /// when no frame is used).
    pub separators: Vec<String>,
    /// Leading and trailing whitespace of each source line, in order (the
    /// line-form path's (j mod L)).
    pub line_ws: Vec<(String, String)>,
    /// A mirror's head and tail ("" for a span).
    pub head: String,
    pub tail: String,
    /// Break fractions (of the body) and the parent words the first and
    /// last paragraphs join.
    pub structure: Structure,
    /// A span's starting column: the lead and body characters of its line
    /// before it (for an insertion, plus its separator's characters, or the
    /// characters after its separator's last newline).
    pub column: usize,
    /// The columns of the parent text after a mixed child's span (or an
    /// insertion's point) on the same line, up to its body's end: the
    /// replacement's last line shares that line (amendment 13 item 13.7). 0
    /// for a whole-text source.
    pub tail_column: usize,
    /// The source body paragraphs in word order: their words and whether a
    /// child paragraph at their midpoint is wrapped (amendment 10 item
    /// 10.3).
    pub body_wrap: Vec<(usize, bool)>,
    /// The whitespace pools by role of the body lines.
    pub pools: RolePools,
    /// Edge frames (whole-text prose sources only).
    pub frames: SourceFrames,
    /// The body licence L: `classify_all` over the body paragraphs.
    pub licence: Vec<String>,
    /// The frame licence L_f (empty without a frame).
    pub frame_licence: Vec<String>,
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

fn sorted_set<'s>(it: impl Iterator<Item = &'s str>) -> Vec<String> {
    it.collect::<BTreeSet<&str>>().into_iter().map(str::to_string).collect()
}

/// The licence of the given paragraphs of `m`: the union of `classify_all`.
fn licence_of(m: &Measure, paras: std::ops::Range<usize>) -> Vec<String> {
    sorted_set(paras.flat_map(|k| classify_all(&m.text[m.paragraphs[k].range.0..m.paragraphs[k].range.1])))
}

/// The pools of the lines `ix` of `m`, by each line's role in `m`.
fn pools_of(m: &Measure, ix: impl Iterator<Item = usize>) -> RolePools {
    let mut p = RolePools::default();
    let n = m.lines.len();
    for i in ix {
        if i == 0 || m.gaps[i - 1].boundary() {
            p.first_leads.push(m.lead(i).to_string());
        } else {
            p.cont_leads.push(m.lead(i).to_string());
        }
        if i + 1 < n && m.gaps[i].kind == GapKind::Wrap {
            p.wrap_trails.push(m.trail(i).to_string());
        } else {
            p.final_trails.push(m.trail(i).to_string());
        }
    }
    p
}

fn wrap_flag(m: &Measure, k: usize) -> bool {
    m.width.is_some() && !m.paragraphs[k].unwrapped_line
}

/// The columns of the parent text after byte offset `e` on the line holding
/// it, up to that line's body end (0 when `e` ends the body or lies on no
/// line's body).
fn tail_columns(m: &Measure, parent: &str, e: usize) -> usize {
    m.lines.iter().find(|l| l.body.0 < e && e <= l.body.1).map_or(0, |l| chars(&parent[e..l.body.1]))
}

impl SourceLayout {
    /// A mirror's source (or a donor's): the whole text.
    pub fn whole(parent: &str) -> SourceLayout {
        let m = Measure::of(parent);
        let counts = m.counts();
        let n = m.lines.len();
        let np = m.paragraphs.len();
        let line_form = counts.class == LayoutClass::LineForm;
        let f = m.frames();
        let framed = !line_form && (f.head > 0 || f.tail > 0);
        let (h, t) = if framed { (f.head, f.tail) } else { (0, 0) };
        let body = h..np - t;
        let boundaries: Vec<usize> = (0..m.gaps.len()).filter(|&i| m.gaps[i].boundary()).collect();
        // Boundary k lies between paragraphs k and k + 1.
        let sep = |k: usize| m.separator(boundaries[k]).to_string();
        let body_lines = if body.is_empty() { 0..0 } else { m.paragraphs[body.start].lines.start..m.paragraphs[body.end - 1].lines.end };
        let ws = |k: usize| (m.lead(k).to_string(), m.trail(k).to_string());
        let frames = if framed {
            SourceFrames {
                head: h > 0,
                tail: t > 0,
                head_ws: m.paragraphs[..h].iter().flat_map(|p| p.lines.clone()).map(ws).collect(),
                tail_ws: m.paragraphs[np - t..].iter().flat_map(|p| p.lines.clone()).map(ws).collect(),
                head_seps: (0..h).map(sep).collect(),
                tail_seps: (np - t - 1..np - 1).map(sep).collect(),
            }
        } else {
            SourceFrames::default()
        };
        let frame_licence = if framed {
            sorted_set((0..h).chain(np - t..np).flat_map(|k| classify_all(&parent[m.paragraphs[k].range.0..m.paragraphs[k].range.1])).chain(FRAME_LICENCE_BASE))
        } else {
            Vec::new()
        };
        SourceLayout {
            mixed: false,
            line_form,
            passes_screen: counts.passes_screen,
            width: counts.width,
            emphasis: counts.emphasis_pairs > 0,
            separators: if body.is_empty() { Vec::new() } else { (body.start..body.end - 1).map(sep).collect() },
            line_ws: (0..n).map(ws).collect(),
            head: m.head().to_string(),
            tail: m.tail().to_string(),
            structure: Structure { fractions: fractions_of(&m.paragraphs[body.clone()].iter().map(|p| p.words).collect::<Vec<_>>()), pre_words: 0, post_words: 0 },
            column: 0,
            tail_column: 0,
            body_wrap: body.clone().map(|k| (m.paragraphs[k].words, wrap_flag(&m, k))).collect(),
            pools: pools_of(&m, body_lines),
            frames,
            licence: licence_of(&m, body),
            frame_licence,
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
        let mut seg_wrap = Vec::new();
        let para_at = |x: usize| m.paragraphs.iter().position(|p| p.range.0 <= x && x <= p.range.1).or_else(|| m.paragraphs.iter().position(|p| x <= p.range.1));
        let mut cur = s;
        for &i in inside.iter().filter(|&&i| m.gaps[i].boundary()) {
            seg_words.push(text::words(&parent[cur..m.gaps[i].range.0]));
            seg_wrap.push(para_at(cur).is_some_and(|k| wrap_flag(&m, k)));
            cur = m.gaps[i].range.1;
        }
        seg_words.push(text::words(&parent[cur..e]));
        seg_wrap.push(para_at(cur).is_some_and(|k| wrap_flag(&m, k)));
        let pre = m.paragraphs.iter().find(|p| p.range.0 <= s && s <= p.range.1);
        let post = m.paragraphs.iter().find(|p| p.range.0 <= e && e <= p.range.1);
        let column = m.lines.iter().find(|l| l.body.0 <= s && s <= l.body.1).map_or(0, |l| chars(&parent[l.start..l.body.0]) + chars(&parent[l.body.0..s]));
        let tail_column = tail_columns(&m, parent, e);
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
            tail_column,
            body_wrap: seg_words.into_iter().zip(seg_wrap).collect(),
            pools: pools_of(&m, lines.iter().copied()),
            frames: SourceFrames::default(),
            licence: licence_of(&m, pr),
            frame_licence: Vec::new(),
            counts,
        })
    }

    /// An insertion's source: the paragraph holding the insertion point `s`
    /// (the last paragraph starting at or before it, else the first), with
    /// the parent's W and the whitespace of the line holding `s` (likewise).
    /// The inserted text is one paragraph: no break fraction and no
    /// separator of its own. When `sep` (the separator assembly puts before
    /// the new text) holds no newline, the text continues the line, so its
    /// column is the line's lead and body characters before `s` plus the
    /// separator's; otherwise it is the characters after the separator's
    /// last newline. None when `s` is not a character boundary of the parent.
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
            None => {
                li.map_or(0, |i| chars(m.lead(i)) + chars(&parent[m.lines[i].body.0..s.max(m.lines[i].body.0).min(m.lines[i].body.1)])) + chars(sep)
            }
        };
        let para = pi.map(|k| m.paragraphs[k].range);
        let tail_column = tail_columns(&m, parent, s);
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
            tail_column,
            body_wrap: pi.map(|k| (m.paragraphs[k].words, wrap_flag(&m, k))).into_iter().collect(),
            pools: pools_of(&m, li.into_iter()),
            frames: SourceFrames::default(),
            licence: pi.map_or_else(Vec::new, |k| licence_of(&m, k..k + 1)),
            frame_licence: Vec::new(),
            counts,
        })
    }

    fn licensed(&self, r: &str) -> bool {
        self.licence.iter().any(|x| x == r)
    }

    /// The break fractions as exact parts: each fraction's cumulative word
    /// count and the total, recomputed from `body_wrap` (the list
    /// `structure.fractions` was made from). None when they do not give the
    /// recorded fractions exactly (a hand-built source), and the floating
    /// fractions are used.
    fn exact_fractions(&self) -> Option<(Vec<usize>, usize)> {
        let w: Vec<usize> = self.body_wrap.iter().map(|x| x.0).filter(|&x| x > 0).collect();
        let total: usize = w.iter().sum();
        let mut cums = Vec::new();
        let mut cum = 0;
        for x in w.iter().take(w.len().saturating_sub(1)) {
            cum += x;
            cums.push(cum);
        }
        let same = cums.len() == self.structure.fractions.len()
            && cums.iter().zip(&self.structure.fractions).all(|(&c, &f)| c as f64 / total.max(1) as f64 == f);
        same.then_some((cums, total))
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
    /// Child head and tail frame lines (amendment 10 item 10.4).
    pub head_frame_lines: usize,
    pub tail_frame_lines: usize,
    /// Kept units (amendment 10 item 10.6(b)): list lines kept as their own
    /// paragraphs, and unpunctuated body paragraphs closed where they end.
    pub kept_list_lines: usize,
    pub closed_paragraphs: usize,
    /// Break targets realised by a fixed boundary (on one, or inside a kept
    /// unit).
    pub targets_on_fixed_boundaries: usize,
    /// Child body paragraphs wrapped and not wrapped (hard-wrapped sources).
    pub wrapped_paragraphs: usize,
    pub unwrapped_paragraphs: usize,
    /// Whitespace picks from an empty pool (each gave `""`).
    pub ws_pool_fallbacks: usize,
    /// Frame-internal boundaries that reused the side's body-adjacent
    /// boundary because the source frame on that side has no internal one
    /// (amendment 13 item 13.8).
    pub frame_internal_reused: usize,
    /// A mixed child's last wrapped line split so that it and the parent
    /// text after the span fit within W (amendment 13 item 13.7).
    pub tail_budget_splits: usize,
}

/// What kind of unit of a laid-out child a unit is (a diagnostic).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitRole {
    /// A head frame line (amendment 10 item 10.4).
    HeadFrame,
    /// A tail frame line.
    TailFrame,
    /// A body paragraph of the prose path.
    Body,
    /// A kept list line: its own paragraph, never wrapped (item 10.6(b)).
    KeptLine,
    /// A line of a line-form child.
    Line,
}

/// One unit of a laid-out child (a diagnostic; no rule reads it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnitDiag {
    pub role: UnitRole,
    /// A body paragraph that is wrapped (item 10.3).
    pub wrapped: bool,
    /// For a body paragraph, the prose segment whose `place_breaks` made it:
    /// two body paragraphs of one segment are separated by a placed break;
    /// every other boundary is fixed (a frame edge, a kept unit's edge, a
    /// closed paragraph's end; item 10.6(b)). `None` for other units.
    pub segment: Option<usize>,
    /// Where the unit comes from in the raw child (byte offsets of `t`).
    pub source: ParagraphSource,
}

/// A kept unit of the raw child (item 10.6(b)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeptKind {
    /// A line of a raw child body paragraph that holds a licensed list or
    /// quoted line; each such line is its own unit.
    ListLine,
    /// A raw child body paragraph of at least `MIN_PARAGRAPH_WORDS` words
    /// whose end is unpunctuated, closed where it ends.
    Closed,
}

/// A kept unit and its byte range in the raw child, from its first line's
/// start to its last line's end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeptUnit {
    pub kind: KeptKind,
    pub raw: (usize, usize),
}

/// Diagnostics of one transfer for the §4.3 dry run's counts (B23; protocol
/// A2 items 10.12, 13.10 and 14.1): the units' roles, the segments that tell
/// placed breaks from fixed boundaries, and the kept units. No rule and no
/// record reads them; the text and every other field are unchanged by them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferDiag {
    /// One per unit of `Transferred::paragraphs`, in order.
    pub units: Vec<UnitDiag>,
    /// The raw child's kept units, in order.
    pub kept: Vec<KeptUnit>,
}

/// A transferred child.
#[derive(Clone, Debug)]
pub struct Transferred {
    pub text: String,
    pub changes: Changes,
    /// v1's rebuild record (the line-form path places no breaks).
    pub layout: Layout,
    /// Line-level form failures of the raw child's body lines whose reason
    /// is not in the licence (the prose path; amendment 10 item 10.5).
    pub line_reasons: Vec<String>,
    /// The unlicensed reasons behind `line_reasons` (`list_marker`,
    /// `quoted_line`, `no_sentence_end`).
    pub line_unlicensed: Vec<String>,
    /// Byte ranges in `text` of each unit (frame line, body paragraph or
    /// kept line; each line of a line-form child), first body start to last
    /// body end.
    pub paragraphs: Vec<(usize, usize)>,
    /// The byte range in `text` from the first body unit's start to the last
    /// one's end (None for an empty child).
    pub body: Option<(usize, usize)>,
    /// Byte ranges in `text` of the frame lines.
    pub frame_lines: Vec<(usize, usize)>,
    pub record: TransferRecord,
    /// The dry run's diagnostics (`TransferDiag`): read by no rule.
    pub diag: TransferDiag,
}

/// Greedy wrap of one squashed paragraph at `w` characters, the first line
/// starting at column `col`; a word longer than its line's budget stands
/// alone.
pub fn wrap(p: &str, w: usize, col: usize) -> Vec<String> {
    wrap_with(p, |k| if k == 0 { w.saturating_sub(col) } else { w })
}

/// Greedy wrap of one squashed paragraph on word boundaries: line k holds at
/// most `budget(k)` characters unless its one word is longer. `budget` is
/// called once per line, in order, as each line starts.
pub fn wrap_with(p: &str, mut budget: impl FnMut(usize) -> usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut len = 0;
    let mut limit = 0;
    for word in p.split_whitespace() {
        let wl = chars(word);
        if cur.is_empty() {
            if out.is_empty() {
                limit = budget(0);
            }
            cur.push_str(word);
            len = wl;
        } else if len + 1 + wl <= limit {
            cur.push(' ');
            cur.push_str(word);
            len += 1 + wl;
        } else {
            out.push(std::mem::take(&mut cur));
            limit = budget(out.len());
            cur.push_str(word);
            len = wl;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// One unit of a laid-out prose child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// Head frame line k.
    Head(usize),
    /// A body paragraph (wrapped or not).
    Body(bool),
    /// A kept list line (its own paragraph, never wrapped).
    Kept,
    /// Tail frame line k.
    Tail(usize),
}

/// Cycles a pool; an empty pool gives `""` and counts a fallback.
fn pick<'s>(pool: &'s [String], i: &mut usize, fallbacks: &mut usize) -> &'s str {
    let k = *i;
    *i += 1;
    if pool.is_empty() {
        *fallbacks += 1;
        ""
    } else {
        pool[k % pool.len()].as_str()
    }
}

/// The child's lines after the line steps (squash, emphasis, sites and
/// quotes), as `rebuild_child` and the line-form path render them.
fn render_line(l: &str, conv: &Conventions, keep_emphasis: bool, ch: &mut Changes) -> Option<String> {
    let s = text::squash(l);
    if s.is_empty() {
        return None;
    }
    ch.trailing_space_lines += l.ends_with(char::is_whitespace) as usize;
    ch.leading_space_lines += l.starts_with(char::is_whitespace) as usize;
    ch.squashed_lines += (s.as_str() != l.trim()) as usize;
    let s = if keep_emphasis { s } else { strip_emphasis(&s, ch) };
    if s.is_empty() {
        return None;
    }
    let s = render_sites(&s, conv, ch);
    Some(render_quotes(&s, conv, ch))
}

fn add_changes(a: &mut Changes, b: &Changes) {
    a.trailing_space_lines += b.trailing_space_lines;
    a.leading_space_lines += b.leading_space_lines;
    a.squashed_lines += b.squashed_lines;
    a.blank_lines_dropped += b.blank_lines_dropped;
    a.single_breaks += b.single_breaks;
    a.markdown_emphasis_pairs += b.markdown_emphasis_pairs;
    a.double_quotes += b.double_quotes;
    a.single_quotes += b.single_quotes;
    a.dashes += b.dashes;
    a.ranges += b.ranges;
    a.ellipses += b.ellipses;
}

/// The raw child's body lines as `rebuild_child` renders them, with the
/// licence-filtered line reasons: `form_line_list_like` for a list line
/// whose reason (`quoted_line` or `list_marker`) is not licensed, and
/// `form_line_no_sentence_end` when `no_sentence_end` is not.
pub(crate) fn licensed_line_reasons(body: &str, conv: &Conventions, src: &SourceLayout) -> (Vec<String>, Vec<String>) {
    let mut scratch = Changes::default();
    let mut reasons = BTreeSet::new();
    let mut unlicensed = BTreeSet::new();
    for l in layout_lines(body, &mut scratch) {
        let l = if src.emphasis { l } else { strip_emphasis(&l, &mut scratch) };
        if l.is_empty() {
            continue;
        }
        let l = render_quotes(&render_sites(&l, conv, &mut scratch), conv, &mut scratch);
        if let Some(r) = list_reason(&l)
            && !src.licensed(r)
        {
            reasons.insert("form_line_list_like".to_string());
            unlicensed.insert(r.to_string());
        }
        if !text::ends_sentence(&l) && !src.licensed(NO_SENTENCE_END) {
            reasons.insert("form_line_no_sentence_end".to_string());
            unlicensed.insert(NO_SENTENCE_END.to_string());
        }
    }
    (reasons.into_iter().collect(), unlicensed.into_iter().collect())
}

/// A raw child's edge frames on the sides where `src` has one (amendment 10
/// item 10.4, read by amendment 13 item 13.8 as the source rule): the
/// outermost `frame_like` paragraphs of `mc` on each such side; none at all
/// when they would leave no body paragraph (every paragraph `frame_like`).
fn child_frames(mc: &Measure, src: &SourceLayout) -> (usize, usize) {
    let n = mc.paragraphs.len();
    let h = if src.frames.head { mc.paragraphs.iter().take_while(|p| p.frame_like()).count() } else { 0 };
    let t = if src.frames.tail { mc.paragraphs.iter().rev().take_while(|p| p.frame_like()).count() } else { 0 };
    if h + t >= n { (0, 0) } else { (h, t) }
}

/// The rebuilt body: units in order (text, kept-list flag), the change
/// counts, the layout record, the licence-filtered line reasons and the
/// kept-unit counts.
struct Body {
    units: Vec<(String, bool)>,
    changes: Changes,
    layout: Layout,
    line_reasons: Vec<String>,
    line_unlicensed: Vec<String>,
    kept_list_lines: usize,
    closed: usize,
    on_fixed: usize,
    /// Per unit (diagnostic): its prose segment (`None` for a kept list
    /// line) and its source in `t`.
    sources: Vec<(Option<usize>, ParagraphSource)>,
    /// The kept units (diagnostic).
    kept: Vec<KeptUnit>,
}

/// `rebuild_child`'s paragraph sources for input `t[off..]`, as offsets of
/// `t`, in segment `seg`.
fn offset_sources(r: &crate::layout::Rebuilt, off: usize, seg: usize) -> Vec<(Option<usize>, ParagraphSource)> {
    r.sources.iter().map(|s| (Some(seg), ParagraphSource { raw: (s.raw.0 + off, s.raw.1 + off), ..*s })).collect()
}

/// The child body (amendment 10 item 10.6(a) and (b)): `t[a..b]` is the raw
/// child's body; `kept` its measure (of all of `t`) and body paragraphs,
/// given when kept units may arise.
fn build_body(t: &str, (a, b): (usize, usize), conv: &Conventions, src: &SourceLayout, kept: Option<(&Measure, std::ops::Range<usize>)>) -> Body {
    let body = &t[a..b];
    let keep_list = src.licensed(LIST_MARKER) || src.licensed(QUOTED_LINE);
    let close = src.licensed(NO_SENTENCE_END);
    // Segments over `t`: (start, end, words, kept list).
    let mut segs: Vec<(usize, usize, usize, bool)> = Vec::new();
    let mut closed = 0;
    let mut closed_units: Vec<KeptUnit> = Vec::new();
    if let Some((mc, paras)) = kept.filter(|_| keep_list || close) {
        let mut cur: Option<(usize, usize, usize)> = None;
        for k in paras {
            let p = &mc.paragraphs[k];
            let (pa, pb) = (mc.lines[p.lines.start].start, mc.lines[p.lines.end - 1].end);
            if p.lines.clone().any(|i| list_reason(mc.body(i)).is_some_and(|r| src.licensed(r))) {
                if let Some(c) = cur.take() {
                    segs.push((c.0, c.1, c.2, false));
                }
                segs.push((pa, pb, p.words, true));
                continue;
            }
            let c = cur.get_or_insert((pa, pb, 0));
            c.1 = pb;
            c.2 += p.words;
            if close && p.words >= MIN_PARAGRAPH_WORDS && !text::ends_sentence(&text::squash(&mc.text[p.range.0..p.range.1])) {
                closed += 1;
                closed_units.push(KeptUnit { kind: KeptKind::Closed, raw: (pa, pb) });
                let c = cur.take().expect("current segment");
                segs.push((c.0, c.1, c.2, false));
            }
        }
        if let Some(c) = cur.take() {
            segs.push((c.0, c.1, c.2, false));
        }
    }
    let licensed_any = !src.licence.is_empty();
    let (filtered, line_unlicensed) = if licensed_any { licensed_line_reasons(body, conv, src) } else { (Vec::new(), Vec::new()) };
    if !segs.iter().any(|s| s.3) && closed == 0 {
        // (a) No kept unit: v1's rebuild exactly.
        let r = rebuild_child(body, conv, &src.structure, src.emphasis);
        let sources = offset_sources(&r, a, 0);
        let line_unlicensed = if licensed_any {
            line_unlicensed
        } else {
            let mut u: Vec<String> = Vec::new();
            if r.line_reasons.iter().any(|x| x == "form_line_list_like") {
                u.extend(rendered_list_reasons(body, conv, src));
            }
            if r.line_reasons.iter().any(|x| x == "form_line_no_sentence_end") {
                u.push(NO_SENTENCE_END.to_string());
            }
            u.sort();
            u.dedup();
            u
        };
        let line_reasons = if licensed_any { filtered } else { r.line_reasons };
        return Body {
            units: r.paragraphs.into_iter().map(|p| (p, false)).collect(),
            changes: r.changes,
            layout: r.layout,
            line_reasons,
            line_unlicensed,
            kept_list_lines: 0,
            closed: 0,
            on_fixed: 0,
            sources,
            kept: Vec::new(),
        };
    }
    // (b) Kept units are fixed boundaries; each target goes to the prose
    // segment holding its fraction.
    let total: usize = segs.iter().map(|s| s.2).sum();
    let mut lo = vec![0usize; segs.len()];
    let mut cum = 0;
    for (j, s) in segs.iter().enumerate() {
        lo[j] = cum;
        cum += s.2;
    }
    // Each target's position in the child body is its fraction times the
    // child's words, compared with the segment edges in exact integer
    // arithmetic when the fractions' parts are known: a target exactly on a
    // fixed boundary is realised by it.
    let exact = src.exact_fractions();
    let mut targets: Vec<Vec<f64>> = vec![Vec::new(); segs.len()];
    let mut on_fixed = 0;
    for (x, &f) in src.structure.fractions.iter().enumerate() {
        let hit = match &exact {
            Some((cums, den)) => {
                let (num, den) = (cums[x] as u128 * total as u128, *den as u128);
                (0..segs.len())
                    .find(|&j| !segs[j].3 && lo[j] as u128 * den < num && num < (lo[j] + segs[j].2) as u128 * den)
                    .map(|j| (j, (num - lo[j] as u128 * den) as f64 / (den * segs[j].2 as u128) as f64))
            }
            None => {
                let pos = f * total as f64;
                (0..segs.len()).find(|&j| !segs[j].3 && (lo[j] as f64) < pos && pos < (lo[j] + segs[j].2) as f64).map(|j| (j, (pos - lo[j] as f64) / segs[j].2 as f64))
            }
        };
        match hit {
            Some((j, g)) => targets[j].push(g),
            None => on_fixed += 1,
        }
    }
    let mut changes = Changes::default();
    let mut units = Vec::new();
    let mut layout = Layout {
        own_lines: 0,
        sentences: 0,
        target_breaks: src.structure.fractions.len(),
        realised_breaks: 0,
        dropped_breaks: 0,
        pre_words: src.structure.pre_words,
        post_words: src.structure.post_words,
    };
    let mut kept_list_lines = 0;
    let mut sources: Vec<(Option<usize>, ParagraphSource)> = Vec::new();
    let mut kept_units: Vec<KeptUnit> = Vec::new();
    let mut closed_iter = closed_units.into_iter().peekable();
    let last = segs.len() - 1;
    for (j, &(sa, sb, _, is_list)) in segs.iter().enumerate() {
        let seg = &t[sa..sb];
        // the closed units inside this segment, in order (diagnostic)
        while let Some(k) = closed_iter.next_if(|k| k.raw.0 < sb) {
            kept_units.push(k);
        }
        if is_list {
            let mut at = sa;
            for l in seg.split('\n') {
                let raw = (at, at + l.len());
                at += l.len() + 1;
                if let Some(s) = render_line(l, conv, src.emphasis, &mut changes) {
                    layout.own_lines += 1;
                    layout.sentences += text::sentences(&s).len();
                    kept_list_lines += 1;
                    units.push((s, true));
                    sources.push((None, ParagraphSource { raw, starts_mid_line: false, ends_mid_line: false }));
                    kept_units.push(KeptUnit { kind: KeptKind::ListLine, raw });
                }
            }
            continue;
        }
        let st = Structure {
            fractions: targets[j].clone(),
            pre_words: if j == 0 { src.structure.pre_words } else { 0 },
            post_words: if j == last { src.structure.post_words } else { 0 },
        };
        let r = rebuild_child(seg, conv, &st, src.emphasis);
        add_changes(&mut changes, &r.changes);
        layout.own_lines += r.layout.own_lines;
        layout.sentences += r.layout.sentences;
        layout.realised_breaks += r.layout.realised_breaks;
        layout.dropped_breaks += r.layout.dropped_breaks;
        sources.extend(offset_sources(&r, sa, j));
        units.extend(r.paragraphs.into_iter().map(|p| (p, false)));
    }
    kept_units.extend(closed_iter);
    Body { units, changes, layout, line_reasons: filtered, line_unlicensed, kept_list_lines, closed, on_fixed, sources, kept: kept_units }
}

/// The list reasons (`quoted_line`, `list_marker`) of the rendered lines.
fn rendered_list_reasons(body: &str, conv: &Conventions, src: &SourceLayout) -> Vec<String> {
    let mut scratch = Changes::default();
    let mut out = BTreeSet::new();
    for l in layout_lines(body, &mut scratch) {
        let l = if src.emphasis { l } else { strip_emphasis(&l, &mut scratch) };
        if let Some(r) = list_reason(&render_quotes(&render_sites(&l, conv, &mut scratch), conv, &mut scratch)) {
            out.insert(r.to_string());
        }
    }
    out.into_iter().collect()
}

/// The wrap flag of a child body unit at word positions `[c, c + w)` of `n`
/// (amendment 10 item 10.3): the flag of the source body paragraph holding
/// its midpoint fraction.
fn wrapped_at(src: &SourceLayout, c: usize, w: usize, n: usize) -> bool {
    if src.width.is_none() {
        return false;
    }
    let total: usize = src.body_wrap.iter().map(|x| x.0).sum();
    if src.body_wrap.is_empty() || n == 0 {
        return src.width.is_some();
    }
    let mid = (2 * c + w) as f64 / (2 * n) as f64;
    let mut cum = 0usize;
    for (k, &(words, flag)) in src.body_wrap.iter().enumerate() {
        cum += words;
        if k + 1 == src.body_wrap.len() || mid < cum as f64 / total.max(1) as f64 {
            return flag;
        }
    }
    src.width.is_some()
}

/// Lays out a prose child's units with the source's whitespace by role,
/// budgets and separators. Returns the text and each unit's byte range.
fn lay_out_prose(units: &[(String, Role)], src: &SourceLayout, rec: &mut TransferRecord) -> (String, Vec<(usize, usize)>) {
    let mut out = String::new();
    let mut ranges = Vec::new();
    if units.is_empty() {
        return (out, ranges);
    }
    if !src.mixed && !src.head.is_empty() {
        out.push_str(&src.head);
        rec.head_applied = true;
    }
    let n_head = units.iter().filter(|u| matches!(u.1, Role::Head(_))).count();
    let n_tail = units.iter().filter(|u| matches!(u.1, Role::Tail(_))).count();
    let (mut fi, mut ci, mut wi, mut ti, mut bi) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut fb = 0usize;
    let mut lines_out = 0usize;
    let total_units = units.len();
    let w = src.width;
    for (u, (p, role)) in units.iter().enumerate() {
        if u > 0 {
            // Amendment 13 item 13.8: the boundary next to the body takes the
            // source's body-adjacent boundary on that side (the last head-side
            // one, the first tail-side one); frame-internal boundaries take
            // the source's frame-internal ones, cycled from the frame's outer
            // edge, or reuse the body-adjacent one when the source has none.
            let sep: Option<String> = match units[u].1 {
                Role::Tail(k) => {
                    let s = &src.frames.tail_seps;
                    if k == 0 || s.len() <= 1 {
                        rec.frame_internal_reused += (k > 0 && s.len() == 1) as usize;
                        s.first().cloned()
                    } else {
                        // tail_seps[0] is body-adjacent; the rest, from the end.
                        let j = n_tail - 1 - k;
                        Some(s[s.len() - 1 - (j % (s.len() - 1))].clone())
                    }
                }
                _ => match units[u - 1].1 {
                    Role::Head(k) => {
                        let s = &src.frames.head_seps;
                        if k + 1 == n_head || s.len() <= 1 {
                            rec.frame_internal_reused += (k + 1 < n_head && s.len() == 1) as usize;
                            s.last().cloned()
                        } else {
                            // head_seps' last is body-adjacent; the rest, in order.
                            Some(s[k % (s.len() - 1)].clone())
                        }
                    }
                    _ => {
                        let k = bi;
                        bi += 1;
                        (!src.separators.is_empty()).then(|| src.separators[k % src.separators.len()].clone())
                    }
                },
            };
            match sep {
                Some(x) => {
                    rec.separators_applied += 1;
                    out.push_str(&x);
                }
                None => {
                    rec.separator_fallbacks += 1;
                    out.push_str(PROSE_FALLBACK);
                }
            }
        }
        // Lines and their whitespace.
        let mut lines: Vec<(String, String, String)> = Vec::new(); // (lead, line, trail)
        match *role {
            Role::Head(k) | Role::Tail(k) => {
                let (pool, ix) = match *role {
                    Role::Head(_) => (&src.frames.head_ws, k),
                    _ => {
                        let len = src.frames.tail_ws.len().max(1);
                        (&src.frames.tail_ws, len - 1 - ((n_tail - 1 - k) % len))
                    }
                };
                let (l, t) = if pool.is_empty() {
                    fb += 1;
                    (String::new(), String::new())
                } else {
                    pool[ix % pool.len()].clone()
                };
                lines.push((l, p.clone(), t));
            }
            Role::Kept | Role::Body(false) => {
                let lead = pick(&src.pools.first_leads, &mut fi, &mut fb).to_string();
                let trail = pick(&src.pools.final_trails, &mut ti, &mut fb).to_string();
                lines.push((lead, p.clone(), trail));
            }
            Role::Body(true) => {
                let first_line = lines_out == 0;
                let mut leads: Vec<String> = Vec::new();
                let wl = wrap_with(p, |k| {
                    let lead = if k == 0 { pick(&src.pools.first_leads, &mut fi, &mut fb) } else { pick(&src.pools.cont_leads, &mut ci, &mut fb) };
                    leads.push(lead.to_string());
                    let w = w.unwrap_or(usize::MAX);
                    if k == 0 && src.mixed && first_line { w.saturating_sub(src.column) } else { w.saturating_sub(chars(lead)) }
                });
                let mut wl = wl;
                // Amendment 13 item 13.7: a mixed child's last line shares its
                // line with the parent text after the span; when the two would
                // be wider than W, the line's maximal suffix of words that fits
                // with that text moves to a new last line (a word longer than
                // its budget stands alone).
                if let (Some(w), true) = (w, src.mixed && src.tail_column > 0 && u + 1 == total_units) {
                    let k = wl.len() - 1;
                    let start = if k == 0 && first_line { src.column } else { chars(&leads[k]) };
                    let words: Vec<&str> = wl[k].split(' ').collect();
                    if words.len() > 1 && start + chars(&wl[k]) + src.tail_column > w {
                        let lead = pick(&src.pools.cont_leads, &mut ci, &mut fb).to_string();
                        let budget = w.saturating_sub(chars(&lead) + src.tail_column);
                        let mut cut = words.len() - 1;
                        while cut > 1 && chars(&words[cut - 1..].join(" ")) <= budget {
                            cut -= 1;
                        }
                        let (head, tail) = (words[..cut].join(" "), words[cut..].join(" "));
                        wl[k] = head;
                        wl.push(tail);
                        leads.push(lead);
                        rec.tail_budget_splits += 1;
                    }
                }
                let nl = wl.len();
                for (k, line) in wl.into_iter().enumerate() {
                    let trail = if k + 1 < nl { pick(&src.pools.wrap_trails, &mut wi, &mut fb) } else { pick(&src.pools.final_trails, &mut ti, &mut fb) };
                    lines.push((leads[k].clone(), line, trail.to_string()));
                }
            }
        }
        let nl = lines.len();
        let mut start = 0;
        for (i, (lead, line, trail)) in lines.into_iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let lead = if src.mixed && lines_out == 0 { "" } else { lead.as_str() };
            let last_line = u + 1 == total_units && i + 1 == nl;
            let trail = if src.mixed && last_line { "" } else { trail.as_str() };
            out.push_str(lead);
            if i == 0 {
                start = out.len();
            }
            out.push_str(&line);
            let end = out.len();
            out.push_str(trail);
            rec.leading_ws_lines += !lead.is_empty() as usize;
            rec.trailing_ws_lines += !trail.is_empty() as usize;
            if i + 1 == nl {
                ranges.push((start, end));
            }
            lines_out += 1;
        }
    }
    if !src.mixed && !src.tail.is_empty() {
        out.push_str(&src.tail);
        rec.tail_applied = true;
    }
    rec.child_lines = lines_out;
    rec.child_paragraphs = units.len();
    rec.head_frame_lines = n_head;
    rec.tail_frame_lines = n_tail;
    rec.ws_pool_fallbacks = fb;
    (out, ranges)
}

/// Lays out a line-form child's lines (blocks of one line each) with the
/// source's whitespace (j mod L) and separators in order.
fn lay_out_lines(blocks: &[String], src: &SourceLayout, rec: &mut TransferRecord) -> (String, Vec<(usize, usize)>) {
    let mut out = String::new();
    let mut ranges = Vec::new();
    if blocks.is_empty() {
        return (out, ranges);
    }
    if !src.mixed && !src.head.is_empty() {
        out.push_str(&src.head);
        rec.head_applied = true;
    }
    let l = src.line_ws.len();
    let total = blocks.len();
    let n = src.separators.len();
    for (j, line) in blocks.iter().enumerate() {
        if j > 0 {
            if n == 0 {
                rec.separator_fallbacks += 1;
                out.push_str(LINE_FORM_FALLBACK);
            } else {
                rec.separators_applied += 1;
                out.push_str(&src.separators[(j - 1) % n]);
            }
        }
        let (lead, trail) = if l == 0 { ("", "") } else { (src.line_ws[j % l].0.as_str(), src.line_ws[j % l].1.as_str()) };
        let lead = if src.mixed && j == 0 { "" } else { lead };
        let trail = if src.mixed && j + 1 == total { "" } else { trail };
        out.push_str(lead);
        let start = out.len();
        out.push_str(line);
        ranges.push((start, out.len()));
        out.push_str(trail);
        rec.leading_ws_lines += !lead.is_empty() as usize;
        rec.trailing_ws_lines += !trail.is_empty() as usize;
    }
    if !src.mixed && !src.tail.is_empty() {
        out.push_str(&src.tail);
        rec.tail_applied = true;
    }
    rec.child_lines = total;
    rec.child_paragraphs = total;
    (out, ranges)
}

/// `layout_transfer_v2`: the raw child `t` laid out like `src`, in the
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
        head_frame_lines: 0,
        tail_frame_lines: 0,
        kept_list_lines: 0,
        closed_paragraphs: 0,
        targets_on_fixed_boundaries: 0,
        wrapped_paragraphs: 0,
        unwrapped_paragraphs: 0,
        ws_pool_fallbacks: 0,
        frame_internal_reused: 0,
        tail_budget_splits: 0,
    };
    if !src.line_form {
        let framed = !src.mixed && (src.frames.head || src.frames.tail);
        let kept_possible = src.licensed(LIST_MARKER) || src.licensed(QUOTED_LINE) || src.licensed(NO_SENTENCE_END);
        let mc = (framed || kept_possible).then(|| Measure::of(t));
        let (h, tl) = match &mc {
            Some(m) if framed => child_frames(m, src),
            _ => (0, 0),
        };
        let mut frame_changes = Changes::default();
        let mut head_lines: Vec<String> = Vec::new();
        let mut tail_lines: Vec<String> = Vec::new();
        // each frame line's byte range in `t` (diagnostic)
        let mut head_src: Vec<(usize, usize)> = Vec::new();
        let mut tail_src: Vec<(usize, usize)> = Vec::new();
        let body = match &mc {
            Some(m) if h + tl > 0 => {
                let np = m.paragraphs.len();
                let mut frame = |ps: &[crate::measure::Paragraph], out: &mut Vec<String>, at: &mut Vec<(usize, usize)>| {
                    for p in ps {
                        for i in p.lines.clone() {
                            if let Some(s) = render_line(&t[m.lines[i].start..m.lines[i].end], conv, src.emphasis, &mut frame_changes) {
                                out.push(s);
                                at.push((m.lines[i].start, m.lines[i].end));
                            }
                        }
                    }
                };
                frame(&m.paragraphs[..h], &mut head_lines, &mut head_src);
                frame(&m.paragraphs[np - tl..], &mut tail_lines, &mut tail_src);
                let range = (m.lines[m.paragraphs[h].lines.start].start, m.lines[m.paragraphs[np - tl - 1].lines.end - 1].end);
                build_body(t, range, conv, src, Some((m, h..np - tl)))
            }
            _ => build_body(t, (0, t.len()), conv, src, mc.as_ref().map(|m| (m, 0..m.paragraphs.len()))),
        };
        let n_words: usize = body.units.iter().map(|u| text::words(&u.0)).sum();
        let frame_diag = |role: UnitRole, raw: (usize, usize)| UnitDiag { role, wrapped: false, segment: None, source: ParagraphSource { raw, starts_mid_line: false, ends_mid_line: false } };
        let mut diag_units: Vec<UnitDiag> = head_src.iter().map(|&r| frame_diag(UnitRole::HeadFrame, r)).collect();
        let mut units: Vec<(String, Role)> = head_lines.into_iter().enumerate().map(|(k, s)| (s, Role::Head(k))).collect();
        let mut c = 0;
        for ((p, kept), &(segment, source)) in body.units.into_iter().zip(&body.sources) {
            let w = text::words(&p);
            let role = if kept {
                Role::Kept
            } else {
                let f = wrapped_at(src, c, w, n_words);
                if src.width.is_some() {
                    rec.wrapped_paragraphs += f as usize;
                    rec.unwrapped_paragraphs += !f as usize;
                }
                Role::Body(f)
            };
            diag_units.push(UnitDiag {
                role: if kept { UnitRole::KeptLine } else { UnitRole::Body },
                wrapped: role == Role::Body(true),
                segment,
                source,
            });
            c += w;
            units.push((p, role));
        }
        diag_units.extend(tail_src.iter().map(|&r| frame_diag(UnitRole::TailFrame, r)));
        units.extend(tail_lines.into_iter().enumerate().map(|(k, s)| (s, Role::Tail(k))));
        rec.kept_list_lines = body.kept_list_lines;
        rec.closed_paragraphs = body.closed;
        rec.targets_on_fixed_boundaries = body.on_fixed;
        rec.wrapped_at = src.width;
        let (text, paragraphs) = lay_out_prose(&units, src, &mut rec);
        let mut changes = body.changes;
        add_changes(&mut changes, &frame_changes);
        let body_ix: Vec<usize> = (0..units.len()).filter(|&k| matches!(units[k].1, Role::Body(_) | Role::Kept)).collect();
        let body_range = match (body_ix.first(), body_ix.last()) {
            (Some(&a), Some(&b)) => Some((paragraphs[a].0, paragraphs[b].1)),
            _ => None,
        };
        let frame_lines = (0..units.len()).filter(|&k| matches!(units[k].1, Role::Head(_) | Role::Tail(_))).map(|k| paragraphs[k]).collect();
        let diag = if diag_units.len() == paragraphs.len() { TransferDiag { units: diag_units, kept: body.kept } } else { TransferDiag::default() };
        return Transferred {
            text,
            changes,
            layout: body.layout,
            line_reasons: body.line_reasons,
            line_unlicensed: body.line_unlicensed,
            paragraphs,
            body: body_range,
            frame_lines,
            record: rec,
            diag,
        };
    }
    // Line-form: the child's own lines in order, each its own block, so every
    // break between them takes the source's boundary separators in order.
    let mut ch = Changes::default();
    let mut blocks: Vec<String> = Vec::new();
    let mut sentences = 0;
    let mut diag_units: Vec<UnitDiag> = Vec::new();
    let mut at = 0usize;
    for l in t.split('\n') {
        let raw = (at, at + l.len());
        at += l.len() + 1;
        if let Some(s) = render_line(l, conv, src.emphasis, &mut ch) {
            sentences += text::sentences(&s).len();
            blocks.push(s);
            diag_units.push(UnitDiag { role: UnitRole::Line, wrapped: false, segment: None, source: ParagraphSource { raw, starts_mid_line: false, ends_mid_line: false } });
        }
    }
    let (text, paragraphs) = lay_out_lines(&blocks, src, &mut rec);
    let diag = if diag_units.len() == paragraphs.len() { TransferDiag { units: diag_units, kept: Vec::new() } } else { TransferDiag::default() };
    let body = match (paragraphs.first(), paragraphs.last()) {
        (Some(a), Some(b)) => Some((a.0, b.1)),
        _ => None,
    };
    Transferred {
        text,
        changes: ch,
        layout: Layout {
            own_lines: blocks.len(),
            sentences,
            target_breaks: src.structure.fractions.len(),
            realised_breaks: 0,
            dropped_breaks: 0,
            pre_words: src.structure.pre_words,
            post_words: src.structure.post_words,
        },
        line_reasons: Vec::new(),
        line_unlicensed: Vec::new(),
        paragraphs,
        body,
        frame_lines: Vec::new(),
        record: rec,
        diag,
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
