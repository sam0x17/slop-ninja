//! Item 10.12 figure 8's greeting and sign-off merges (count 2) and list
//! markers left mid-line (count 3), on one child under one code. The
//! definitions (BUILD-PLAN 12.3 open item (a)) are the counting tool's,
//! taken from amendment 10's trigger (problem 1) and `R2D/a7check/a7check.rs`
//! (P2/P3), and read the same raw child under both codes:
//!
//! * **Greeting and sign-off.** The raw child's own edge frames, measured
//!   by `layout_measure_v2` (`Measure::of(raw).frames()`): the maximal runs
//!   of first and last paragraphs that are `frame_like` (fewer than
//!   `MIN_PARAGRAPH_WORDS` words, no `list_like` line), none when every
//!   paragraph is (item 13.8's source rule). Each of their lines is a
//!   greeting (head) or sign-off (tail) line. A line **stands as its own
//!   unit** when one unit of the laid-out child holds exactly its letters
//!   and digits, and that unit is not the first or last unit of a span child
//!   that joins the parent's text before or after it in the final document
//!   (item 13.1: an edit's first and last units join the parent text up to
//!   the parent's nearest boundaries). Otherwise the line is **merged**. A
//!   child counts once per side when any of its lines on that side is
//!   merged.
//! * **List markers.** A raw child line whose squashed text is `list_like`
//!   (`screen::list_reason`: `list_marker`, with `quoted_line` counted
//!   beside it). Its marker **stands at a line start** in the laid-out child
//!   when the laid-out line holding the raw line's first letter or digit has
//!   no letter or digit before it and, when the raw marker holds a
//!   non-space character before that letter, holds one too. For a span
//!   child's first line the parent's text before the span on that line comes
//!   first. Otherwise the marker is **left mid-line**.
//!
//! Positions are compared on the letters and digits of the two texts
//! (lowercased): the normaliser never adds or removes a token's letters or
//! digits (`layout`'s module notes), so the raw child and the laid-out child
//! hold the same sequence. A child where they differ is counted
//! (`fold_mismatch`) and not read further; a greeting, sign-off or list line
//! with no letter or digit is counted as unanchored.
use crate::engine::Laid;
use grammar_a_form::measure::Measure;
use grammar_a_form::screen::{QUOTED_LINE, list_reason};
use grammar_a_form::text;

/// A text's letters and digits, lowercased, with each one's byte offset.
pub struct Folded {
    pub chars: Vec<char>,
    pub at: Vec<usize>,
}

/// The letters and digits of `s`, lowercased, with their byte offsets.
pub fn fold(s: &str) -> Folded {
    let mut chars = Vec::new();
    let mut at = Vec::new();
    for (i, c) in s.char_indices() {
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                chars.push(lc);
                at.push(i);
            }
        }
    }
    Folded { chars, at }
}

impl Folded {
    /// The number of folded characters before byte offset `b`.
    pub fn index(&self, b: usize) -> usize {
        self.at.partition_point(|&x| x < b)
    }
    /// The folded range of byte range `(a, b)`.
    pub fn range(&self, (a, b): (usize, usize)) -> (usize, usize) {
        (self.index(a), self.index(b))
    }
}

/// What sits beside a child in its final document: the parent's text on
/// the child's first line before it (a span's or insertion's), and whether
/// its first and last units join parent words (`pre_words`, `post_words`
/// of the landed source layout).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Context<'a> {
    pub pre_line: &'a str,
    pub pre_words: usize,
    pub post_words: usize,
}

/// One side's edge frame (greeting or sign-off) of a child.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edge {
    /// Lines with a letter or digit.
    pub lines: usize,
    pub merged_lines: usize,
    /// Lines with no letter or digit (not read).
    pub unanchored: usize,
}

impl Edge {
    pub fn present(&self) -> bool {
        self.lines + self.unanchored > 0
    }
    pub fn merged(&self) -> bool {
        self.merged_lines > 0
    }
}

/// Counts 2 and 3 on one child.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FormFigures {
    /// The raw child and the laid-out child hold different letters and
    /// digits: nothing else is read.
    pub fold_mismatch: bool,
    pub greeting: Edge,
    pub sign_off: Edge,
    pub list_marker_lines: usize,
    pub list_marker_mid_line: usize,
    pub quoted_lines: usize,
    pub quoted_mid_line: usize,
    /// `list_like` lines with no letter or digit (not read).
    pub unanchored_list_lines: usize,
}

/// The byte ranges of `t`'s lines (split on `'\n'`, newline excluded).
fn lines(t: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for l in t.split('\n') {
        out.push((at, at + l.len()));
        at += l.len() + 1;
    }
    out
}

/// Counts 2 and 3 on one child: `raw` the raw child, `laid` its laid-out
/// child under one code, `ctx` its place in the final document.
pub fn form_figures(raw: &str, laid: &Laid, ctx: &Context) -> FormFigures {
    let fr = fold(raw);
    let fl = fold(&laid.text);
    let mut out = FormFigures::default();
    if fr.chars != fl.chars {
        out.fold_mismatch = true;
        return out;
    }
    // count 2: the raw child's own edge frames against the laid-out units
    let units: Vec<(usize, usize)> = laid.units.iter().map(|&u| fl.range(u)).collect();
    let n = units.len();
    let joins = |k: usize| (k == 0 && ctx.pre_words > 0) || (k + 1 == n && ctx.post_words > 0);
    let m = Measure::of(raw);
    let f = m.frames();
    let np = m.paragraphs.len();
    let side = |ps: &[grammar_a_form::measure::Paragraph]| {
        let mut e = Edge::default();
        for p in ps {
            for i in p.lines.clone() {
                let r = fr.range((m.lines[i].start, m.lines[i].end));
                if r.0 == r.1 {
                    e.unanchored += 1;
                    continue;
                }
                e.lines += 1;
                let own = units.iter().position(|&u| u == r).is_some_and(|k| !joins(k));
                e.merged_lines += usize::from(!own);
            }
        }
        e
    };
    out.greeting = side(&m.paragraphs[..f.head]);
    out.sign_off = side(&m.paragraphs[np - f.tail..]);
    // count 3: each list-like raw line's marker in the laid-out child
    for (a, b) in lines(raw) {
        let line = &raw[a..b];
        let Some(reason) = list_reason(&text::squash(line)) else { continue };
        let quoted = reason == QUOTED_LINE;
        let Some(off) = line.char_indices().find(|(_, c)| c.is_alphanumeric()).map(|(i, _)| i) else {
            out.unanchored_list_lines += 1;
            continue;
        };
        let marker = line[..off].chars().any(|c| !c.is_whitespace());
        let k = fr.index(a + off);
        let q = fl.at[k];
        let ls = laid.text[..q].rfind('\n').map_or(0, |i| i + 1);
        let before: String = if ls == 0 { format!("{}{}", ctx.pre_line, &laid.text[..q]) } else { laid.text[ls..q].to_string() };
        let at_start = !before.chars().any(char::is_alphanumeric) && (!marker || before.chars().any(|c| !c.is_whitespace()));
        if quoted {
            out.quoted_lines += 1;
            out.quoted_mid_line += usize::from(!at_start);
        } else {
            out.list_marker_lines += 1;
            out.list_marker_mid_line += usize::from(!at_start);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn laid(text: &str, units: &[&str]) -> Laid {
        let mut out = Vec::new();
        let mut from = 0;
        for u in units {
            let s = from + text[from..].find(u).unwrap();
            out.push((s, s + u.len()));
            from = s + u.len();
        }
        Laid { text: text.to_string(), units: out }
    }

    const BODY: &str = "The council met on Tuesday evening to discuss the plan for the town centre.";

    #[test]
    fn a_greeting_kept_as_its_own_unit_is_not_merged() {
        let raw = format!("Hello Ann,\n\n{BODY}\n\nBest regards,\nSam");
        let t = format!("Hello Ann,\n\n{BODY}\n\nBest regards,\nSam");
        let f = form_figures(&raw, &laid(&t, &["Hello Ann,", BODY, "Best regards,", "Sam"]), &Context::default());
        assert!(!f.fold_mismatch);
        assert_eq!((f.greeting.lines, f.greeting.merged_lines), (1, 0));
        assert_eq!((f.sign_off.lines, f.sign_off.merged_lines), (2, 0));
    }

    #[test]
    fn a_greeting_joined_to_the_first_paragraph_is_merged() {
        let raw = format!("Hello Ann,\n\n{BODY}\n\nBest regards,\nSam");
        let t = format!("Hello Ann, {BODY} Best regards, Sam");
        let f = form_figures(&raw, &laid(&t, &[t.as_str()]), &Context::default());
        assert!(f.greeting.merged() && f.sign_off.merged());
        assert_eq!((f.greeting.lines, f.greeting.merged_lines, f.sign_off.merged_lines), (1, 1, 2));
    }

    #[test]
    fn near_miss_short_paragraphs_with_no_body_are_no_frame() {
        // every paragraph is frame-like: no frame (the source rule, item 13.8)
        let raw = "Hi there,\n\nSee you soon";
        let t = "Hi there, See you soon";
        let f = form_figures(raw, &laid(t, &[t]), &Context::default());
        assert!(!f.greeting.present() && !f.sign_off.present());
        // a short sentence on the body's own line is no frame either
        let raw = format!("{BODY} Thanks.");
        let f = form_figures(&raw, &laid(&raw, &[raw.as_str()]), &Context::default());
        assert!(!f.greeting.present() && !f.sign_off.present());
    }

    #[test]
    fn a_span_childs_first_unit_joining_parent_words_is_merged() {
        let raw = format!("Hello Ann,\n\n{BODY}");
        let t = format!("Hello Ann,\n\n{BODY}");
        let l = laid(&t, &["Hello Ann,", BODY]);
        let kept = form_figures(&raw, &l, &Context { pre_line: "", pre_words: 0, post_words: 0 });
        assert_eq!(kept.greeting.merged_lines, 0);
        let joined = form_figures(&raw, &l, &Context { pre_line: "Earlier words ", pre_words: 2, post_words: 0 });
        assert_eq!(joined.greeting.merged_lines, 1);
    }

    #[test]
    fn a_span_childs_last_unit_joining_parent_words_after_it_is_merged() {
        let raw = format!("{BODY}\n\nBest regards,\nSam");
        let t = format!("{BODY}\n\nBest regards,\nSam");
        let l = laid(&t, &[BODY, "Best regards,", "Sam"]);
        let kept = form_figures(&raw, &l, &Context { pre_line: "", pre_words: 3, post_words: 0 });
        assert_eq!((kept.sign_off.lines, kept.sign_off.merged_lines), (2, 0));
        let joined = form_figures(&raw, &l, &Context { pre_line: "", pre_words: 0, post_words: 4 });
        assert_eq!((joined.sign_off.lines, joined.sign_off.merged_lines), (2, 1), "the last unit joins the parent words after the span");
    }

    #[test]
    fn list_markers_left_mid_line_are_counted_and_kept_ones_are_not() {
        let raw = "We still need these before the meeting:\n- the signed contract\n- two printed copies\n> quoted reply";
        let kept = "We still need these before the meeting:\n- the signed contract\n- two printed copies\n> quoted reply";
        let f = form_figures(raw, &laid(kept, &[kept]), &Context::default());
        assert_eq!((f.list_marker_lines, f.list_marker_mid_line, f.quoted_lines, f.quoted_mid_line), (2, 0, 1, 0));
        let joined = "We still need these before the meeting: - the signed contract - two printed copies > quoted reply";
        let f = form_figures(raw, &laid(joined, &[joined]), &Context::default());
        assert_eq!((f.list_marker_lines, f.list_marker_mid_line, f.quoted_lines, f.quoted_mid_line), (2, 2, 1, 1));
    }

    #[test]
    fn near_miss_a_marker_wrapped_to_the_end_of_the_line_before_is_mid_line() {
        let raw = "A line of text\n- item one here";
        // the marker sits at the end of the first line and the item's word starts the next
        let t = "A line of text -\nitem one here";
        let f = form_figures(raw, &laid(t, &[t]), &Context::default());
        assert_eq!(f.list_marker_mid_line, 1);
        // a numbered marker is its own first digit
        let raw = "Intro words here\n1. first item";
        let ok = "Intro words here\n1. first item";
        assert_eq!(form_figures(raw, &laid(ok, &[ok]), &Context::default()).list_marker_mid_line, 0);
    }

    #[test]
    fn a_span_childs_first_line_follows_the_parent_text_on_its_line() {
        let raw = "- the signed contract";
        let t = "- the signed contract";
        let at_start = form_figures(raw, &laid(t, &[t]), &Context { pre_line: "   ", pre_words: 0, post_words: 0 });
        assert_eq!(at_start.list_marker_mid_line, 0);
        let mid = form_figures(raw, &laid(t, &[t]), &Context { pre_line: "We need ", pre_words: 2, post_words: 0 });
        assert_eq!(mid.list_marker_mid_line, 1);
    }

    #[test]
    fn a_child_whose_letters_differ_is_not_read() {
        let f = form_figures("- alpha", &laid("- beta", &["- beta"]), &Context::default());
        assert!(f.fold_mismatch);
        assert_eq!(f.list_marker_lines, 0);
    }

    #[test]
    fn lines_without_letters_are_unanchored() {
        let raw = format!("* * *\n\n{BODY}\n\n--");
        let t = raw.clone();
        let f = form_figures(&raw, &laid(&t, &["* * *", BODY, "--"]), &Context::default());
        assert_eq!(f.unanchored_list_lines, 2);
    }
}
