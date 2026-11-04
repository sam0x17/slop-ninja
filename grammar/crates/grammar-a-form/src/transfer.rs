//! The parent-conditioned layout transfer `layout_transfer_v6` of
//! `cc_cohort_profile_v2` (protocol A2 §6.2, as amended by amendments 7, 10,
//! 13, 29, 30, 38 and 42; `layout_transfer_v3` was amendment 29's alone,
//! `layout_transfer_v4` added amendment 30's and `layout_transfer_v5`
//! amendment 38's).
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
//!    `MIN_PARAGRAPH_WORDS` words whose end is unpunctuated is a closed
//!    paragraph: a kept unit of its own, wrapped like other body paragraphs
//!    and never merged, its start and its end both fixed boundaries
//!    (amendment 30 item 30.1). Kept units are fixed boundaries: each break
//!    target goes to the prose segment holding its fraction, where
//!    `place_breaks` runs, and a target on a fixed boundary or inside a kept
//!    unit is realised by it (counted; positions compared in exact integer
//!    arithmetic). The text between two fixed boundaries is a prose segment
//!    of its own; one under 8 words is one paragraph (amendment 30 item
//!    30.2).
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
//!    text moves to a new last line (amendment 13 item 13.7, counted). In an
//!    item with several edits (`profile::lay_out_item`, amendment 29 item
//!    29.1) the column and `tail_column` are measured on the item assembled
//!    so far: a later edit's column holds an earlier edit's laid-out text on
//!    its line (`sibling_column`), and an earlier edit's tail counts only
//!    the parent text up to the next edit on its line. When such a later
//!    edit's first unit is wrapped and its first word does not fit in W
//!    less its column, its text starts on a new line with a continuation
//!    lead (`sibling_column_wraps`, counted): the replacement then begins
//!    with that line break and lead. When no suffix fits (not even the last
//!    word) and the boundary break below exists on that document line, the
//!    last line is not split (amendment 42 item 42.3): its words stay, and
//!    the boundary break is taken on the assembled line. Without a boundary
//!    break there (an insertion whose following parent text starts without
//!    whitespace), one word moves as before.
//!
//!    **The split on the assembled document line** (amendment 38 item 38.1,
//!    as amendment 39 item 39.1 reads it; `split_document_lines`). After the
//!    layout above, each line of the document that holds a word of the
//!    child's wrapped unit, holds more than one word and is wider than W
//!    (item 10.1's columns) is split. W is the smallest W of the edits whose
//!    wrapped words share the line (an item's earlier edits keep theirs,
//!    `LineContext::widths`). It may break only inside the child's laid-out
//!    text: between two of its words on the line, or just before its first
//!    word on the line when that word is the replacement's first and some
//!    word precedes it on the line (the replacement then begins with the
//!    line break and the lead, as in item 29.1's no-room case). The part of
//!    the line after the break moves to a new line, whoever wrote its words,
//!    with the next continuation lead of the child's source (the pool cycled
//!    on from its own layout's picks); the line left before a break between
//!    two words takes the source's next wrap trail (item 10.6(c), cycled on
//!    from the layout's picks, an empty pool counted), and a break before
//!    the first word takes none (the bytes before it are fixed). The break
//!    is the earliest at which the moved part fits in the child's own W (the
//!    only W on the new line; item 13.7's maximal suffix); when none fits,
//!    the **boundary break** (amendment 42 item 42.1); and only when there is
//!    no boundary break, the last. The boundary break lies just after the
//!    edit's last word on the line, and exists only when parent words follow
//!    that word on the line (for an edit of an item, the text read after it,
//!    up to the next edit) and the bytes right after it begin with a
//!    whitespace run (the maximal run of non-line-break whitespace; Unicode
//!    White_Space, followed by a byte that is not Python whitespace). The
//!    construction is (a-ii) (item 42.2): the parent's whitespace run stays
//!    on the line before the break as its trailing whitespace, then one line
//!    break and the edit's next continuation lead, then the parent text from
//!    its first non-whitespace byte, not re-wrapped. The replacement then
//!    ends with that run, the line break and the lead: the edit's assembled
//!    range ends after the parent's whitespace run, and its final span at
//!    its last word (`profile::replacement_tail`). No wrap trail is added and
//!    no fit is asked of the moved parent text. Each boundary break is
//!    counted (`tail_boundary_breaks`), and each edit whose end line needed
//!    one but whose following parent text starts without whitespace is
//!    counted once (`tail_boundary_unavailable`), both recorded only when not
//!    0. The transfer exposes the break read-only (`Transferred::tail_break`,
//!    `last_line_shares_parent_line`) for the dry run's counting tool.
//!    The rule is applied again to the shortened line, then to the new one,
//!    until no line qualifies; one-word lines are exempt, and a line whose
//!    overflow lies in parent text or that has no break point is left
//!    (count 4 reads it). Parent bytes are never changed. The document lines
//!    are read from `SourceLayout::line`: for a single edit, its parent's
//!    text before it on its first line and after it on its last; for an edit
//!    of an item (`profile::lay_out_item`), the item assembled so far before
//!    it (earlier edits' text final) and, when the next edit starts on its
//!    last line, the parent text up to that edit only (item 29.1's tail),
//!    or, when the next edit is an insertion whose separator breaks the
//!    line, the parent text up to its point and the separator up to its
//!    first line break (the assembled line). So an edit's text never depends
//!    on a later sibling's text, and an earlier edit is never changed by a
//!    later one. Each application is counted (`document_line_splits`,
//!    recorded only when not 0).
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
//!    splice needs a trimmed replacement (amendment 7 item 7.6). The
//!    exceptions are step 3's line break with no room on a shared line
//!    (amendment 29 item 29.1) and step 3's boundary break (amendment 42
//!    item 42.2), which the assemblers accept.
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

pub const LAYOUT_TRANSFER_ID: &str = "layout_transfer_v6";
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

/// The text on an edit's first and last document lines, which amendment
/// 38's split reads (item 38.1, as amendment 39 item 39.1 reads it): the
/// text before the edit on the line holding its start (an insertion's
/// separator included, or the part after its last newline; for an edit of
/// an item, the item assembled so far, earlier edits' laid-out text
/// included) and the text after it on the line holding its end (the parent
/// to that line's end, or for an edit of an item the parent text up to the
/// next edit, `profile::lay_out_item`). Empty for a whole-text source.
/// Never recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineContext {
    pub before: String,
    pub after: String,
    /// The byte ranges in `before` of earlier edits' wrapped units, each
    /// with that edit's W: a line that holds their words is measured
    /// against the smallest W of the edits whose words share it (item
    /// 38.1). Empty for a single edit.
    pub widths: Vec<((usize, usize), usize)>,
}

/// A source's layout properties: whitespace strings, counts, fractions,
/// widths and reason names; never text, except the unrecorded
/// `LineContext` of a span or insertion.
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
    /// for a whole-text source. In an item whose next edit starts on the
    /// same document line, only the parent text between the two edits
    /// (amendment 29 item 29.1; `profile::lay_out_item`).
    pub tail_column: usize,
    /// `column` holds an earlier edit's laid-out text on the same document
    /// line (amendment 29 item 29.1; set by `profile::lay_out_item` only):
    /// a wrapped first unit whose first word does not fit in W less the
    /// column starts on a new line with a continuation lead. Not a measured
    /// property, so not serialised.
    #[serde(skip_serializing)]
    pub sibling_column: bool,
    /// The text on the child's first and last document lines, for amendment
    /// 38's split, which `transfer` applies to the child once it is laid out
    /// (`split_alone`); `profile::lay_out_item` sets it for each edit of an
    /// item from the item assembled so far. None: no split (no constructor
    /// leaves it None; tests use it for the layout before amendment 38).
    /// Not serialised.
    #[serde(skip_serializing)]
    pub line: Option<LineContext>,
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

/// The start of the `'\n'`-line holding byte offset `x`.
fn line_start(t: &str, x: usize) -> usize {
    t[..x].rfind('\n').map_or(0, |k| k + 1)
}

/// The end of the `'\n'`-line holding byte offset `x` (its newline excluded).
fn line_end(t: &str, x: usize) -> usize {
    t[x..].find('\n').map_or(t.len(), |k| x + k)
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
            sibling_column: false,
            line: Some(LineContext::default()),
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
        let line = Some(LineContext { before: parent[line_start(parent, s)..s].to_string(), after: parent[e..line_end(parent, e)].to_string(), widths: Vec::new() });
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
            sibling_column: false,
            line,
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
        let before = match sep.rfind('\n') {
            Some(k) => sep[k + 1..].to_string(),
            None => format!("{}{sep}", &parent[line_start(parent, s)..s]),
        };
        let line = Some(LineContext { before, after: parent[s..line_end(parent, s)].to_string(), widths: Vec::new() });
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
            sibling_column: false,
            line,
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
    /// An item edit whose wrapped first unit had no room on the line it
    /// shares with an earlier edit, so its text starts on a new line with a
    /// continuation lead (amendment 29 item 29.1). Recorded only when not 0,
    /// so the records of every other child keep their fields.
    #[serde(skip_serializing_if = "is_zero")]
    pub sibling_column_wraps: usize,
    /// Applications of the split on the assembled document line (amendment
    /// 38 item 38.1). Recorded only when not 0, so the record of every child
    /// the rule does not change keeps its bytes.
    #[serde(skip_serializing_if = "is_zero")]
    pub document_line_splits: usize,
    /// Boundary breaks taken (amendment 42 item 42.1): the parent text after
    /// the edit's last word moved to a new line. Recorded only when not 0.
    #[serde(skip_serializing_if = "is_zero")]
    pub tail_boundary_breaks: usize,
    /// The edit's last line was read with no inside break that fits and no
    /// boundary break, because the parent text after the edit starts without
    /// whitespace (an insertion; amendment 42 item 42.1), counted once per
    /// edit. Recorded only when not 0.
    #[serde(skip_serializing_if = "is_zero")]
    pub tail_boundary_unavailable: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
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
    /// For a body paragraph, the segment that made it: two body paragraphs
    /// of one segment are separated by a placed break; every other boundary
    /// is fixed (a frame edge, a kept unit's edge, a closed paragraph's start
    /// and end; item 10.6(b), amendment 30 item 30.1). A closed paragraph is
    /// a segment of its own. `None` for other units.
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
    /// whose end is unpunctuated: one unit, its start and end fixed
    /// (amendment 30 item 30.1).
    Closed,
}

/// A segment of a child body between fixed boundaries (item 10.6(b)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seg {
    /// Prose, where `place_breaks` runs.
    Prose,
    /// A raw child body paragraph holding a licensed list line: each line
    /// its own unit.
    List,
    /// A closed paragraph: one unit, never split or merged.
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
    /// One per unit of `paragraphs`: the unit is a wrapped body paragraph
    /// (amendment 38's split reads only these).
    pub unit_wrapped: Vec<bool>,
    /// The continuation leads the layout has taken from the source's pool
    /// (amendment 38's split cycles on from here).
    pub cont_picks: usize,
    /// The wrap trails the layout has taken from the source's pool (amendment
    /// 39 item 39.3: a split between two words cycles on from here).
    pub trail_picks: usize,
    /// The boundary break the split took (amendment 42 item 42.2), if any.
    /// Read through `tail_break` and `last_line_shares_parent_line`.
    tail_break: Option<TailBreak>,
}

/// A boundary break at the end of a replacement (amendment 42 item 42.2):
/// `text` ends with the parent's whitespace run after the edit (`run`
/// bytes, the parent's own), one line break and the continuation lead
/// (`lead` bytes, no line break).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TailBreak {
    pub run: usize,
    pub lead: usize,
}

impl Transferred {
    /// The boundary break at the end of the replacement, if the split took
    /// one (amendment 42 item 42.2). Read-only: the dry run's counting tool
    /// reads the transfer's result here and copies no rule (item 42.5).
    pub fn tail_break(&self) -> Option<TailBreak> {
        self.tail_break
    }

    /// Whether a mixed child's last line still shares its document line with
    /// the parent text after the span (item 13.7's `tail_column`): false
    /// after a boundary break (amendment 42 item 42.5) and for a whole text.
    pub fn last_line_shares_parent_line(&self) -> bool {
        self.record.mixed && self.tail_break.is_none()
    }
}

/// What follows an edit's last word on its document line (amendment 42 item
/// 42.1), read on `after`, the text after the edit up to the line's end (or
/// for an edit of an item, up to the next edit).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Boundary {
    /// No parent word follows on the line: no boundary break.
    None,
    /// A whitespace run of this many bytes, then a parent byte that is not
    /// whitespace: the boundary break exists.
    Run(usize),
    /// Parent text follows without a whitespace run (an insertion), or
    /// after a run holding a character that is Python whitespace but not
    /// Unicode White_Space (U+001C..U+001F): no boundary break.
    Unavailable,
}

pub(crate) fn boundary_of(after: &str) -> Boundary {
    let rest = after.trim_start_matches(|c: char| crate::profile::py_space(c) && c != '\n');
    let run = after.len() - rest.len();
    match rest.chars().next() {
        None => Boundary::None,
        Some('\n') => Boundary::None,
        Some(_) if run == 0 => Boundary::Unavailable,
        Some(_) if !after[..run].chars().all(char::is_whitespace) => Boundary::Unavailable,
        Some(_) => Boundary::Run(run),
    }
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
    // Segments over `t`: (start, end, words, kind).
    let mut segs: Vec<(usize, usize, usize, Seg)> = Vec::new();
    let mut closed = 0;
    let mut closed_units: Vec<KeptUnit> = Vec::new();
    if let Some((mc, paras)) = kept.filter(|_| keep_list || close) {
        let mut cur: Option<(usize, usize, usize)> = None;
        for k in paras {
            let p = &mc.paragraphs[k];
            let (pa, pb) = (mc.lines[p.lines.start].start, mc.lines[p.lines.end - 1].end);
            let list = p.lines.clone().any(|i| list_reason(mc.body(i)).is_some_and(|r| src.licensed(r)));
            // Amendment 30 item 30.1: a closed paragraph is a kept unit of
            // its own, both of its boundaries fixed; the text before it is a
            // prose segment of its own (item 30.2).
            let closes = !list && close && p.words >= MIN_PARAGRAPH_WORDS && !text::ends_sentence(&text::squash(&mc.text[p.range.0..p.range.1]));
            if list || closes {
                if let Some(c) = cur.take() {
                    segs.push((c.0, c.1, c.2, Seg::Prose));
                }
                if closes {
                    closed += 1;
                    closed_units.push(KeptUnit { kind: KeptKind::Closed, raw: (pa, pb) });
                }
                segs.push((pa, pb, p.words, if list { Seg::List } else { Seg::Closed }));
                continue;
            }
            let c = cur.get_or_insert((pa, pb, 0));
            c.1 = pb;
            c.2 += p.words;
        }
        if let Some(c) = cur.take() {
            segs.push((c.0, c.1, c.2, Seg::Prose));
        }
    }
    let licensed_any = !src.licence.is_empty();
    let (filtered, line_unlicensed) = if licensed_any { licensed_line_reasons(body, conv, src) } else { (Vec::new(), Vec::new()) };
    if !segs.iter().any(|s| s.3 == Seg::List) && closed == 0 {
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
                    .find(|&j| segs[j].3 == Seg::Prose && lo[j] as u128 * den < num && num < (lo[j] + segs[j].2) as u128 * den)
                    .map(|j| (j, (num - lo[j] as u128 * den) as f64 / (den * segs[j].2 as u128) as f64))
            }
            None => {
                let pos = f * total as f64;
                (0..segs.len()).find(|&j| segs[j].3 == Seg::Prose && (lo[j] as f64) < pos && pos < (lo[j] + segs[j].2) as f64).map(|j| (j, (pos - lo[j] as f64) / segs[j].2 as f64))
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
    for (j, &(sa, sb, _, kind)) in segs.iter().enumerate() {
        let seg = &t[sa..sb];
        // the closed units inside this segment, in order (diagnostic)
        while let Some(k) = closed_iter.next_if(|k| k.raw.0 < sb) {
            kept_units.push(k);
        }
        if kind == Seg::List {
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
        // A closed paragraph takes no target (each was realised by its
        // fixed boundaries), so it is rebuilt as one paragraph.
        let st = Structure {
            fractions: if kind == Seg::Closed { Vec::new() } else { targets[j].clone() },
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
fn lay_out_prose(units: &[(String, Role)], src: &SourceLayout, rec: &mut TransferRecord) -> (String, Vec<(usize, usize)>, (usize, usize)) {
    let mut out = String::new();
    let mut ranges = Vec::new();
    if units.is_empty() {
        return (out, ranges, (0, 0));
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
        // Amendment 29 item 29.1: the unit starts on a new line (no room on
        // the line it shares with an earlier edit of its item).
        let mut break_first = false;
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
                // Amendment 29 item 29.1: a later edit of an item whose first
                // word does not fit in W less its column (which holds an
                // earlier edit's text) starts on a new line with a
                // continuation lead, its budget W less that lead.
                break_first = first_line
                    && src.mixed
                    && src.sibling_column
                    && w.is_some_and(|w| p.split_whitespace().next().is_some_and(|x| chars(x) > w.saturating_sub(src.column)));
                rec.sibling_column_wraps += break_first as usize;
                let at_column = first_line && src.mixed && !break_first;
                let mut leads: Vec<String> = Vec::new();
                let wl = wrap_with(p, |k| {
                    let lead = if k == 0 && !break_first { pick(&src.pools.first_leads, &mut fi, &mut fb) } else { pick(&src.pools.cont_leads, &mut ci, &mut fb) };
                    leads.push(lead.to_string());
                    let w = w.unwrap_or(usize::MAX);
                    if k == 0 && at_column { w.saturating_sub(src.column) } else { w.saturating_sub(chars(lead)) }
                });
                let mut wl = wl;
                // Amendment 13 item 13.7: a mixed child's last line shares its
                // line with the parent text after the span; when the two would
                // be wider than W, the line's maximal suffix of words that fits
                // with that text moves to a new last line (a word longer than
                // its budget stands alone).
                if let (Some(w), true) = (w, src.mixed && src.tail_column > 0 && u + 1 == total_units) {
                    let k = wl.len() - 1;
                    let start = if k == 0 && at_column { src.column } else { chars(&leads[k]) };
                    let words: Vec<&str> = wl[k].split(' ').collect();
                    // Amendment 42 item 42.3: when no suffix fits (not even
                    // the last word, with the lead the split would take) and
                    // the boundary break exists on this document line, the
                    // line is not split and takes no lead; the split on the
                    // assembled line then takes the boundary break.
                    let none_fits = || {
                        let pool = &src.pools.cont_leads;
                        let lead = if pool.is_empty() { "" } else { pool[ci % pool.len()].as_str() };
                        words.last().is_some_and(|x| chars(x) > w.saturating_sub(chars(lead) + src.tail_column))
                    };
                    let boundary = src.line.as_ref().is_some_and(|l| matches!(boundary_of(&l.after), Boundary::Run(_)));
                    if words.len() > 1 && start + chars(&wl[k]) + src.tail_column > w && !(boundary && none_fits()) {
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
            if i > 0 || break_first {
                out.push('\n');
            }
            let lead = if src.mixed && lines_out == 0 && !break_first { "" } else { lead.as_str() };
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
    (out, ranges, (ci, wi))
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

/// `layout_transfer_v5`: the raw child `t` laid out like `src`, in the
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
        sibling_column_wraps: 0,
        document_line_splits: 0,
        tail_boundary_breaks: 0,
        tail_boundary_unavailable: 0,
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
        let (text, paragraphs, (cont_picks, trail_picks)) = lay_out_prose(&units, src, &mut rec);
        let mut changes = body.changes;
        add_changes(&mut changes, &frame_changes);
        let body_ix: Vec<usize> = (0..units.len()).filter(|&k| matches!(units[k].1, Role::Body(_) | Role::Kept)).collect();
        let body_range = match (body_ix.first(), body_ix.last()) {
            (Some(&a), Some(&b)) => Some((paragraphs[a].0, paragraphs[b].1)),
            _ => None,
        };
        let frame_lines = (0..units.len()).filter(|&k| matches!(units[k].1, Role::Head(_) | Role::Tail(_))).map(|k| paragraphs[k]).collect();
        let diag = if diag_units.len() == paragraphs.len() { TransferDiag { units: diag_units, kept: body.kept } } else { TransferDiag::default() };
        let unit_wrapped = units.iter().map(|u| u.1 == Role::Body(true)).collect();
        let mut t = Transferred {
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
            unit_wrapped,
            cont_picks,
            trail_picks,
            tail_break: None,
        };
        split_alone(&mut t, src);
        return t;
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
        unit_wrapped: vec![false; blocks.len()],
        cont_picks: 0,
        trail_picks: 0,
        tail_break: None,
    }
}

// ------------------------------------------------------------------ amendment 38

/// An edit as amendment 38's split reads it in its document line context
/// (item 38.1, as amendment 39 item 39.1 reads it): its replacement's bytes,
/// its wrapped units' byte ranges, its W, earlier edits' wrapped units with
/// their W (`LineContext::widths`), and its source's continuation leads and
/// wrap trails with the picks already taken.
struct LineEdit<'a> {
    range: (usize, usize),
    wrapped: Vec<(usize, usize)>,
    width: usize,
    others: Vec<((usize, usize), usize)>,
    cont_leads: &'a [String],
    cursor: usize,
    wrap_trails: &'a [String],
    trail_cursor: usize,
}

impl LineEdit<'_> {
    fn next_lead(&self) -> &str {
        if self.cont_leads.is_empty() { "" } else { self.cont_leads[self.cursor % self.cont_leads.len()].as_str() }
    }

    fn next_trail(&self) -> &str {
        if self.wrap_trails.is_empty() { "" } else { self.wrap_trails[self.trail_cursor % self.wrap_trails.len()].as_str() }
    }
}

/// One application of the split on an edit's replacement, in the order
/// applied: the replacement's bytes `at.0..at.1` (as it stands then; the
/// whitespace between two of its words, or the empty range before its first
/// word) become `trail`, a line break and `lead`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LineSplit {
    at: (usize, usize),
    lead: String,
    /// The continuation pool was empty (the lead is `""`, counted as `pick`
    /// counts it).
    fallback: bool,
    /// The wrap trail the line left before a break between two words takes
    /// (amendment 39 item 39.3); none before the edit's first word.
    trail: String,
    /// A break between two words found the wrap-trail pool empty (counted).
    trail_fallback: bool,
    /// A boundary break (amendment 42 item 42.1): the parent's whitespace
    /// run after the edit, which the line before the break keeps; `at` is
    /// the empty range just after it, past the replacement's own bytes.
    boundary: Option<String>,
}

/// Byte ranges of the maximal runs of non-whitespace characters of `s`,
/// offset by `off` (the words `str::split_whitespace` gives).
fn word_ranges(s: &str, off: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            if let Some(a) = start.take() {
                out.push((off + a, off + i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(a) = start {
        out.push((off + a, off + s.len()));
    }
    out
}

/// Amendment 38 item 38.1 for edit `e` in `doc`, as amendment 39 item 39.1
/// reads it (see the module notes, step 3): lines are read in document
/// order, and a split line is read again, then the line it made. A line is
/// over when it is wider than the smallest W of the edits whose wrapped
/// words it holds; the moved part must fit in the edit's own W, the only W
/// on the new line (it holds the edit's words and parent text alone).
/// When no inside break fits, the boundary break after the edit's last word
/// on the line is taken if it exists (amendment 42 item 42.1), before the
/// last inside break. Returns the edit's applications, in order, and whether
/// the edit's end line needed a boundary break that its following text
/// does not allow (`tail_boundary_unavailable`). Each inside application
/// moves at least one word to a new line and leaves at least one before it;
/// a boundary break moves parent text only and leaves no parent word after
/// the edit on its line, so it is taken at most once; the loop ends.
fn split_document_lines(doc: &str, e: &mut LineEdit) -> (Vec<LineSplit>, bool) {
    let mut doc = doc.to_string();
    let mut out = Vec::new();
    let mut unavailable = false;
    let mut ls = 0usize;
    loop {
        let le = line_end(&doc, ls);
        let words = word_ranges(&doc[ls..le], ls);
        let own = |&(a, b): &(usize, usize)| e.wrapped.iter().any(|&(x, y)| x <= a && b <= y);
        // the line holds the edit's end, in a word of a wrapped unit (a word
        // the parent's text is glued to included)
        let at_end = |&(a, b): &(usize, usize)| a < e.range.1 && e.range.1 <= b && e.wrapped.iter().any(|&(x, y)| x < e.range.1 && e.range.1 <= y);
        let read = words.iter().any(own);
        let holds_end = words.iter().any(at_end);
        let mut chosen: Option<Brk> = None;
        if words.len() > 1 && (read || holds_end) {
            let line_cols = chars(&doc[ls..words[words.len() - 1].1]);
            // the earlier edits' words keep their bytes (their ranges lie
            // before the edit's), so their W is read where they stand
            let line_w = words
                .iter()
                .filter_map(|&(a, b)| e.others.iter().find(|((x, y), _)| *x <= a && b <= *y).map(|o| o.1))
                .fold(e.width, usize::min);
            if line_cols > line_w {
                let mut last = None;
                for j in 1..words.len() {
                    if !own(&words[j]) {
                        continue;
                    }
                    let inside = e.range.0 <= words[j - 1].0 && words[j - 1].1 <= e.range.1;
                    if !(inside || words[j].0 == e.range.0) {
                        continue;
                    }
                    last = Some(Brk::Inside(j, inside));
                    if chars(e.next_lead()) + line_cols - chars(&doc[ls..words[j].0]) <= e.width {
                        chosen = last;
                        break;
                    }
                }
                if chosen.is_none() && holds_end {
                    match boundary_of(&doc[e.range.1..le]) {
                        Boundary::Run(run) if read => chosen = Some(Brk::Boundary(run)),
                        Boundary::Unavailable => unavailable = true,
                        _ => {}
                    }
                }
                chosen = chosen.or(last);
            }
        }
        let Some(brk) = chosen else {
            if le >= doc.len() {
                break;
            }
            ls = le + 1;
            continue;
        };
        let (j, inside) = match brk {
            Brk::Inside(j, inside) => (j, inside),
            Brk::Boundary(run) => {
                // amendment 42 item 42.2 (a-ii): the run stays before the
                // break; the line break and the lead follow it
                let x = e.range.1 + run;
                let lead = e.next_lead().to_string();
                let fallback = e.cont_leads.is_empty();
                e.cursor += 1;
                let ins = format!("\n{lead}");
                out.push(LineSplit {
                    at: (x - e.range.0, x - e.range.0),
                    lead,
                    fallback,
                    trail: String::new(),
                    trail_fallback: false,
                    boundary: Some(doc[e.range.1..x].to_string()),
                });
                doc.insert_str(x, &ins);
                continue;
            }
        };
        let (x, y) = (if inside { words[j - 1].1 } else { words[j].0 }, words[j].0);
        let lead = e.next_lead().to_string();
        let fallback = e.cont_leads.is_empty();
        e.cursor += 1;
        let (trail, trail_fallback) = if inside {
            let t = e.next_trail().to_string();
            let f = e.wrap_trails.is_empty();
            e.trail_cursor += 1;
            (t, f)
        } else {
            (String::new(), false)
        };
        let ins = format!("{trail}\n{lead}");
        out.push(LineSplit { at: (x - e.range.0, y - e.range.0), lead, fallback, trail, trail_fallback, boundary: None });
        doc.replace_range(x..y, &ins);
        let map = |p: usize| if p >= y { p + ins.len() - (y - x) } else { p };
        // the line break before the edit's first word is the edit's own
        e.range.1 = map(e.range.1);
        for r in &mut e.wrapped {
            *r = (map(r.0), map(r.1));
        }
    }
    (out, unavailable)
}

/// A break point `split_document_lines` takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Brk {
    /// (word index, the break lies between two of the edit's words).
    Inside(usize, bool),
    /// The boundary break after a whitespace run of this many bytes.
    Boundary(usize),
}

/// Applies an edit's splits (`split_document_lines`) to its transfer: the
/// text, the unit, body and frame-line ranges, and the record's counts.
fn apply_line_splits(t: &mut Transferred, splits: &[LineSplit]) {
    for s in splits {
        if let Some(run) = &s.boundary {
            // amendment 42 item 42.2: the replacement ends with the parent's
            // run, one line break and the lead; the units, the body and the
            // frame lines end before it. The new line holds parent text only:
            // it is no child line, and no wrap trail is added. The lead is a
            // pick of the continuation pool, an empty pool counted.
            debug_assert_eq!(s.at.0, t.text.len() + run.len());
            t.text.push_str(run);
            t.text.push('\n');
            t.text.push_str(&s.lead);
            t.tail_break = Some(TailBreak { run: run.len(), lead: s.lead.len() });
            t.record.tail_boundary_breaks += 1;
            t.record.ws_pool_fallbacks += usize::from(s.fallback);
            t.cont_picks += 1;
            continue;
        }
        let (x, y) = s.at;
        let ins = format!("{}\n{}", s.trail, s.lead);
        t.text.replace_range(x..y, &ins);
        let map = |p: usize| if p >= y { p + ins.len() - (y - x) } else { p };
        let mr = |r: &mut (usize, usize)| *r = (map(r.0), map(r.1));
        t.paragraphs.iter_mut().for_each(mr);
        t.frame_lines.iter_mut().for_each(mr);
        if let Some(b) = t.body.as_mut() {
            mr(b);
        }
        t.record.document_line_splits += 1;
        t.record.child_lines += usize::from(x < y);
        t.record.leading_ws_lines += usize::from(!s.lead.is_empty());
        t.record.trailing_ws_lines += usize::from(!s.trail.is_empty());
        t.record.ws_pool_fallbacks += usize::from(s.fallback) + usize::from(s.trail_fallback);
        t.cont_picks += 1;
        t.trail_picks += usize::from(x < y);
    }
}

/// Amendment 38's split for a laid-out child, on the document lines its
/// text shares with its context (`SourceLayout::line`): break points in its
/// own text only (amendment 39 item 39.1).
fn split_alone(t: &mut Transferred, src: &SourceLayout) {
    let (Some(ctx), Some(width)) = (&src.line, src.width) else { return };
    if !t.unit_wrapped.contains(&true) {
        return;
    }
    let at = ctx.before.len();
    let doc = format!("{}{}{}", ctx.before, t.text, ctx.after);
    let wrapped = t.paragraphs.iter().zip(&t.unit_wrapped).filter(|x| *x.1).map(|(&(a, b), _)| (at + a, at + b)).collect();
    let mut e = LineEdit {
        range: (at, at + t.text.len()),
        wrapped,
        width,
        others: ctx.widths.clone(),
        cont_leads: &src.pools.cont_leads,
        cursor: t.cont_picks,
        wrap_trails: &src.pools.wrap_trails,
        trail_cursor: t.trail_picks,
    };
    let (splits, unavailable) = split_document_lines(&doc, &mut e);
    apply_line_splits(t, &splits);
    t.record.tail_boundary_unavailable += usize::from(unavailable);
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

    // ---------------------------------------------------------- amendment 38

    /// An edit laid at `at` with one wrapped unit over its whole replacement.
    fn edit<'a>(range: (usize, usize), width: usize, leads: &'a [String], cursor: usize, trails: &'a [String]) -> LineEdit<'a> {
        LineEdit { range, wrapped: vec![range], width, others: Vec::new(), cont_leads: leads, cursor, wrap_trails: trails, trail_cursor: 0 }
    }

    /// One edit, wrapped as one unit, laid at `before.len()` in
    /// `before + rep + after`, with earlier edits' wrapped words `others`
    /// (ranges in `before`, each with its W): the split's result and the
    /// replacement after the edit's applications.
    #[allow(clippy::too_many_arguments)]
    fn split_with(before: &str, rep: &str, after: &str, w: usize, leads: &[String], cursor: usize, trails: &[String], others: Vec<((usize, usize), usize)>) -> (String, Vec<LineSplit>) {
        let doc = format!("{before}{rep}{after}");
        let at = before.len();
        let mut e = edit((at, at + rep.len()), w, leads, cursor, trails);
        e.others = others;
        let (sp, _) = split_document_lines(&doc, &mut e);
        assert_eq!(e.cursor, cursor + sp.len());
        assert_eq!(e.trail_cursor, sp.iter().filter(|x| x.at.0 < x.at.1).count());
        let mut r = rep.to_string();
        for x in &sp {
            match &x.boundary {
                // the boundary break: past the replacement's bytes, after the run
                Some(run) => {
                    assert_eq!(x.at, (r.len() + run.len(), r.len() + run.len()));
                    r.push_str(&format!("{run}\n{}", x.lead));
                }
                None => r.replace_range(x.at.0..x.at.1, &format!("{}\n{}", x.trail, x.lead)),
            }
        }
        (r, sp)
    }

    fn split_one(before: &str, rep: &str, after: &str, w: usize, leads: &[String], cursor: usize) -> (String, Vec<LineSplit>) {
        split_with(before, rep, after, w, leads, cursor, &flush(), Vec::new())
    }

    fn flush() -> Vec<String> {
        vec![String::new()]
    }

    /// An application with no wrap trail.
    fn at(at: (usize, usize), lead: &str, fallback: bool) -> LineSplit {
        LineSplit { at, lead: lead.into(), fallback, trail: String::new(), trail_fallback: false, boundary: None }
    }

    #[test]
    fn a38_a_first_word_with_no_room_after_the_parents_column_moves_to_a_new_line() {
        // 20 columns of parent text, then the edit's first word alone (item
        // 10.1): the document line holds five words over W 20.
        let (r, sp) = split_one("aaaa bbbb cccc dddd ", "eeee\nffff gggg", "", 20, &flush(), 0);
        assert_eq!(r, "\neeee\nffff gggg");
        assert_eq!(sp, vec![at((0, 0), "", false)]);
    }

    #[test]
    fn a38_the_moved_part_takes_the_parent_text_after_the_edit_with_it() {
        // A one-word edit mid-line: the break before it moves it and the
        // parent text after it (item 13.7's case on the document line).
        let (r, _) = split_one("aaaa bbbb cccc dddd ", "eeee", " ff", 20, &flush(), 0);
        assert_eq!(r, "\neeee");
        // With no room for the word and that text (layout_transfer_v5 took
        // the last break here and left the new line over W), amendment 42
        // takes the boundary break first: the parent text moves alone, the
        // shortened line is read again and its word then fits after the
        // break before it.
        let tail = format!(" {}", "x".repeat(18));
        let (r, sp) = split_one("aaaa bbbb cccc dddd ", "eeee", &tail, 20, &flush(), 0);
        assert_eq!(r, "\neeee \n");
        assert_eq!(sp, vec![bnd(5, " ", "", false), at((0, 0), "", false)]);
    }

    /// A boundary break after a run `run`, at `at` past the replacement's
    /// bytes.
    fn bnd(at: usize, run: &str, lead: &str, fallback: bool) -> LineSplit {
        LineSplit { at: (at, at), lead: lead.into(), fallback, trail: String::new(), trail_fallback: false, boundary: Some(run.into()) }
    }

    #[test]
    fn a42_the_boundary_break_moves_the_parent_text_after_the_edits_last_word() {
        // One child word at the line's start, then the parent's tail over W
        // (count 4's 74 lines): no inside break point, so layout_transfer_v5
        // left the line. The boundary break keeps the parent's run as the
        // line's trailing whitespace and starts the tail on a new line with
        // the next continuation lead; the new line holds parent text only.
        let tail = format!(" {}", "x".repeat(24));
        assert_eq!(split_one("", "eeee", &tail, 20, &flush(), 0), ("eeee \n".to_string(), vec![bnd(5, " ", "", false)]));
        assert_eq!(split_one("ab\n", "eeee", &tail, 20, &flush(), 0).0, "eeee \n");
        // a 2-space run stays whole before the break, byte for byte
        let tail2 = format!("  {}", "x".repeat(24));
        assert_eq!(split_one("", "eeee", &tail2, 20, &flush(), 0), ("eeee  \n".to_string(), vec![bnd(6, "  ", "", false)]));
        // a tab and a no-break space are whitespace runs too
        assert_eq!(split_one("", "eeee", "\t\u{a0}zzzz zzzz zzzz zzzz", 20, &flush(), 0).0, "eeee\t\u{a0}\n");
        // the lead is the next continuation lead, cycled on from the layout's
        // picks; an empty pool gives "" and is counted
        let leads = vec!["".to_string(), "  ".to_string()];
        assert_eq!(split_one("", "eeee", &tail, 20, &leads, 1), ("eeee \n  ".to_string(), vec![bnd(5, " ", "  ", false)]));
        assert_eq!(split_one("", "eeee", &tail, 20, &[], 0).1, vec![bnd(5, " ", "", true)]);
        // a tail wider than W on its own still moves, and its line stays
        // over W (parent text only: item 7.3's stated limit)
        let wide = format!(" {}", "y".repeat(30));
        assert_eq!(split_one("", "eeee", &wide, 20, &flush(), 0).0, "eeee \n");
        // a line within W is not read
        assert_eq!(split_one("", "eeee", " ff", 20, &flush(), 0).1, vec![]);
        // a blank line above: the edit's last unit is one word that starts a
        // paragraph; its line is exempt once the tail has moved
        let (r, sp) = split_one("", "aaaa bbbb\n\neeee", &tail, 20, &flush(), 0);
        assert_eq!((r.as_str(), sp), ("aaaa bbbb\n\neeee \n", vec![bnd(16, " ", "", false)]));
    }

    #[test]
    fn a42_an_inside_break_that_fits_comes_first_and_the_boundary_before_the_last() {
        // An inside break that fits is taken as before (item 38.1, step 1).
        let (r, sp) = split_one("pp ", "aaaaa bbbbb", " mm cccccccc ttttt", 30, &flush(), 0);
        assert_eq!((r.as_str(), sp.len()), ("\naaaaa bbbbb", 1));
        // When none fits, the boundary break (step 2), not the last inside
        // break (v5's fallback): with W 20 neither moved part fits (29, 23).
        let (r, sp) = split_one("pp ", "aaaaa bbbbb", " mm cccccccc ttttt", 20, &flush(), 0);
        assert_eq!((r.as_str(), sp), ("aaaaa bbbbb \n", vec![bnd(12, " ", "", false)]));
        // Start-side and end-side breaks on one edit: after the boundary
        // break the line left (14 columns) is still over W 12 and is read
        // again; its earliest fitting break is before the edit's first word.
        let (r, sp) = split_one("pp ", "aaaaa bbbbb", " mm cccccccc ttttt", 12, &flush(), 0);
        assert_eq!(r, "\naaaaa bbbbb \n");
        assert_eq!(sp, vec![bnd(12, " ", "", false), at((0, 0), "", false)]);
        // With no boundary break (the edit ends its line), the last inside
        // break is still the fallback.
        let (r, _) = split_one(&format!("{} ", "p".repeat(18)), &format!("{} {}", "a".repeat(18), "b".repeat(18)), "", 20, &flush(), 0);
        assert_eq!(r, format!("\n{}\n{}", "a".repeat(18), "b".repeat(18)));
    }

    #[test]
    fn a42_no_boundary_break_without_a_parent_word_or_a_whitespace_run() {
        let tail = format!(" {}", "x".repeat(24));
        // an insertion at the end of a line: no parent word follows
        assert_eq!(split_one(&format!("{} ", "p".repeat(24)), "eeee", "", 20, &flush(), 0).1, vec![at((0, 0), "", false)]);
        assert_eq!(split_one("", &format!("eeee {}", "q".repeat(24)), "  ", 20, &flush(), 0).1.iter().filter(|x| x.boundary.is_some()).count(), 0);
        // parent text glued to the edit's last word (an insertion whose
        // following parent text starts without whitespace): no boundary
        // break; the line is left and the edit is counted once
        let glued = "x".repeat(24);
        let doc = format!("eeee{glued} zz");
        let mut e = edit((0, 4), 20, &[], 0, &[]);
        assert_eq!(split_document_lines(&doc, &mut e), (vec![], true));
        // with an inside break that does not fit, the last one is taken and
        // the edit is still counted once
        let doc = format!("cccc dddd eeee{glued}");
        let mut e = edit((0, 14), 20, &[], 0, &[]);
        let (sp, un) = split_document_lines(&doc, &mut e);
        assert_eq!((sp.len(), un), (1, true));
        assert!(sp[0].boundary.is_none());
        // a run holding U+001C (Python whitespace, not Unicode White_Space):
        // no boundary break, counted
        let doc = format!("eeee \u{1c}{glued}");
        let mut e = edit((0, 4), 20, &[], 0, &[]);
        assert_eq!(split_document_lines(&doc, &mut e), (vec![], true));
        drop(tail);
        assert_eq!(boundary_of(""), Boundary::None);
        assert_eq!(boundary_of("   "), Boundary::None);
        assert_eq!(boundary_of(" x"), Boundary::Run(1));
        assert_eq!(boundary_of("x"), Boundary::Unavailable);
    }

    #[test]
    fn a38_the_earliest_fitting_break_is_taken() {
        // Parent text on both sides (an item's earlier edit's text before,
        // item 29.1, reads the same): the earliest break point at which the
        // moved part fits is before the edit's first word.
        let (r, sp) = split_one("pp ", "aaaaa bbbbb", " mm cccccccc ttttt", 30, &flush(), 0);
        assert_eq!(r, "\naaaaa bbbbb");
        assert_eq!(sp, vec![at((0, 0), "", false)]);
        // With a two-column lead the whole moved part (31) no longer fits:
        // the next break, between the edit's words, is taken.
        let (r, sp) = split_one("pp ", "aaaaa bbbbb", " mm cccccccc ttttt", 30, &["  ".to_string()], 0);
        assert_eq!(r, "aaaaa\n  bbbbb");
        assert_eq!(sp, vec![at((5, 6), "  ", false)]);
    }

    #[test]
    fn a38_a_moved_part_of_exactly_w_columns_fits() {
        // Item 13.7's "fits" includes W itself: with a one-column lead the
        // moved part from the edit's first word is 1 + 29 = 30 = W, so that
        // break is the earliest fitting one.
        let (r, sp) = split_one("pp ", "aaaaa bbbbb", " mm cccccccc ttttt", 30, &[" ".to_string()], 0);
        assert_eq!(r, "\n aaaaa bbbbb");
        assert_eq!(sp, vec![at((0, 0), " ", false)]);
        // One column less and it no longer fits: the next break is taken.
        let (r, _) = split_one("pp ", "aaaaa bbbbb", " mm cccccccc ttttt", 29, &[" ".to_string()], 0);
        assert_eq!(r, "aaaaa\n bbbbb");
    }

    #[test]
    fn a38_a_shared_line_is_measured_against_the_smallest_w_of_its_edits() {
        // An earlier edit's wrapped word "yy" (W 12) shares the line with the
        // edit (W 30). The line (15 columns) is within the edit's W but over
        // the earlier edit's: it is split, and the moved part need only fit
        // in the edit's own W (the new line holds no earlier edit's word).
        let (r, sp) = split_with("xx yy ", "aaaa bbbb", "", 30, &flush(), 0, &flush(), vec![((3, 5), 12)]);
        assert_eq!(r, "\naaaa bbbb");
        assert_eq!(sp, vec![at((0, 0), "", false)]);
        // Without that word on the line, the edit's own W applies: no split.
        assert_eq!(split_with("xx yy ", "aaaa bbbb", "", 30, &flush(), 0, &flush(), vec![]).1, vec![]);
        // An earlier edit whose words lie on an earlier line does not count.
        assert_eq!(split_with("zz\nxx yy ", "aaaa bbbb", "", 30, &flush(), 0, &flush(), vec![((0, 2), 1)]).1, vec![]);
        // The moved part need fit only the edit's own W: from the edit's first
        // word it is 14 columns, over the earlier edit's W but within the
        // edit's, so that earliest break is taken.
        let (r, _) = split_with("xx yy ", "aaaa bbbb cccc", "", 30, &flush(), 0, &flush(), vec![((3, 5), 12)]);
        assert_eq!(r, "\naaaa bbbb cccc");
        // A larger W of an earlier edit does not raise the edit's own.
        assert_eq!(split_with("xx yy ", "aaaa bbbb", "", 12, &flush(), 0, &flush(), vec![((3, 5), 40)]).1.len(), 1);
    }

    #[test]
    fn a38_a_break_between_two_words_takes_the_next_wrap_trail() {
        // Item 10.6(c): the line left before a break between two of the
        // edit's words takes the source's next wrap trail, cycled on from
        // the layout's picks; a break before the edit's first word takes
        // none and draws none.
        let (p, a, b) = ("p".repeat(18), "a".repeat(10), "b".repeat(10));
        let leads = vec!["".to_string(), "    ".to_string()];
        let trails = vec![" ".to_string(), "\t".to_string()];
        let (r, sp) = split_with(&format!("{p} "), &format!("{a} {b}"), "", 20, &leads, 1, &trails, vec![]);
        assert_eq!(r, format!("\n{a} \n    {b}"));
        assert_eq!(
            sp,
            vec![
                LineSplit { at: (10, 11), lead: "    ".into(), fallback: false, trail: " ".into(), trail_fallback: false, boundary: None },
                at((0, 0), "", false),
            ]
        );
        // An empty pool gives no trail and is counted.
        let (_, sp) = split_with(&format!("{p} "), &format!("{a} {b}"), "", 20, &leads, 1, &[], vec![]);
        assert_eq!((sp[0].trail.as_str(), sp[0].trail_fallback, sp[1].trail_fallback), ("", true, false));
    }

    #[test]
    fn a38_overflow_in_parent_text_and_one_word_lines_are_left() {
        // (The edit's word starting the line, followed by parent text over W,
        // now takes the boundary break: `a42_...`.)
        // A one-word line wider than W is exempt.
        assert_eq!(split_one("", &"z".repeat(25), "", 20, &flush(), 0).1, vec![]);
        assert_eq!(split_one("p\n", &"z".repeat(25), "\nq", 20, &flush(), 0).1, vec![]);
        // A line at W exactly is not over.
        assert_eq!(split_one("aaaa bbbb cccc ", "dddd", "", 19, &flush(), 0).1, vec![]);
        assert_eq!(split_one("aaaa bbbb cccc ", "dddd", "", 18, &flush(), 0).1.len(), 1);
        // Only words of the edit's wrapped units are break points: a line of
        // parent words alone is never read.
        let doc = "aaaa bbbb cccc dddd eeee ffff\nzz";
        let mut e = edit((30, 32), 10, &[], 0, &[]);
        assert_eq!(split_document_lines(doc, &mut e), (vec![], false));
        // nor are an edit's unwrapped units (no budget)
        let doc = "aaaa bbbb cccc dddd eeee ffff";
        let mut e = edit((20, 29), 10, &[], 0, &[]);
        e.wrapped.clear();
        assert_eq!(split_document_lines(doc, &mut e), (vec![], false));
    }

    #[test]
    fn a38_the_rule_repeats_over_three_lines() {
        // P(18) A(18) B(18) at W 20: the moved part from A does not fit, so
        // B moves first; the shortened line is read again and A moves.
        let (p, a, b) = ("p".repeat(18), "a".repeat(18), "b".repeat(18));
        let (r, sp) = split_one(&format!("{p} "), &format!("{a} {b}"), "", 20, &flush(), 0);
        assert_eq!(r, format!("\n{a}\n{b}"));
        assert_eq!(sp.iter().map(|x| x.at).collect::<Vec<_>>(), vec![(18, 19), (0, 0)]);
    }

    #[test]
    fn a38_leads_cycle_on_from_the_layouts_picks_and_an_empty_pool_is_counted() {
        let leads = vec!["".to_string(), "    ".to_string()];
        let (p, a, b) = ("p".repeat(18), "a".repeat(10), "b".repeat(10));
        let (r, sp) = split_one(&format!("{p} "), &format!("{a} {b}"), "", 20, &leads, 1);
        // cursor 1: the first split takes "    ", the second ""
        assert_eq!(sp.iter().map(|x| x.lead.as_str()).collect::<Vec<_>>(), vec!["    ", ""]);
        assert_eq!(r, format!("\n{a}\n    {b}"));
        let (_, sp) = split_one("aaaa bbbb cccc dddd ", "eeee", "", 20, &[], 0);
        assert_eq!(sp, vec![at((0, 0), "", true)]);
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
