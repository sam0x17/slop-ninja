//! The excerpt layout and the child normaliser (`cc_era_parent_norm_v1`).
//!
//! * **Excerpt layout.** Every pilot excerpt has one form: each line squashed,
//!   empty lines dropped, paragraphs joined by one blank line.
//! * **Child normalisation.** Lines are squashed and blank lines dropped
//!   (trailing and leading spaces, tabs, NBSP and single breaks go). Paired
//!   markdown emphasis markers are removed. Every quote, dash, range and
//!   ellipsis site is rendered in the parent's convention
//!   (`conventions`). Paragraphs are then rebuilt to the parent's structure:
//!   a mirror takes its parent's paragraph break fractions, a mixed child the
//!   break fractions of the human span it replaces, with breaks placed at the
//!   nearest admissible sentence boundaries (minimum total distance, every
//!   paragraph at least `MIN_PARAGRAPH_WORDS`, counting the parent words the
//!   first and last segments join).
//!
//! The normaliser never adds or removes a word token's letters or digits:
//! folded (lowercase alphanumeric) token sequences are invariant. Whitespace
//! word counts can change when a dash style changes spacing.
//!
//! Working memory: `place_breaks` holds two `(k + 1) x (n + 1)` tables for
//! `k` target breaks and `n` sentences; every other step is linear in its
//! input, and every output is linear in the input.
//!
//! Moved verbatim from `grammar-cc-human`'s `era_norm` (v1); the byte-identity
//! tests pin the outputs to those recorded before the move.
use crate::conventions::{Changes, Conventions, render_quotes, render_sites};
use crate::screen::list_like;
use crate::text;
use serde::Serialize;
use std::collections::HashSet;

/// The layout every pilot excerpt has: squashed lines, one blank line between
/// paragraphs, no empty paragraph.
pub const EXCERPT_LAYOUT_ID: &str = "cc_excerpt_layout_v1";
/// The prose screen's paragraph floor (`text::prose_reason`,
/// "short_paragraph"); a test pins the two together.
pub const MIN_PARAGRAPH_WORDS: usize = 8;
/// Tokens whose final period does not end a sentence, for paragraph breaks
/// only (single letters, initials, are refused separately).
pub const ABBREVIATIONS: [&str; 44] = [
    "mr", "mrs", "ms", "dr", "st", "jr", "sr", "prof", "rev", "gen", "col", "lt", "sgt", "capt", "gov", "sen", "rep", "hon", "no", "vs",
    "inc", "ltd", "co", "corp", "dept", "univ", "approx", "fig", "vol", "ch", "pp", "ed", "e.g", "i.e", "a.m", "p.m", "u.s", "u.k", "mt",
    "ft", "jan", "feb", "aug", "sept",
];

/// The excerpt layout: each line squashed (`text::squash`),
/// empty lines dropped, paragraphs joined by one blank line.
pub fn excerpt_layout(t: &str) -> String {
    t.split('\n').map(text::squash).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n\n")
}

/// Squashed non-empty lines of `t`, counting what the layout rule changes:
/// one blank line between two text lines is the separator itself; a single
/// break, extra blank lines and blank lines at either end are changes.
pub fn layout_lines(t: &str, ch: &mut Changes) -> Vec<String> {
    let mut out = Vec::new();
    let mut blank_run = 0usize;
    for l in t.split('\n') {
        let s = text::squash(l);
        if s.is_empty() {
            blank_run += 1;
            continue;
        }
        if out.is_empty() {
            ch.blank_lines_dropped += blank_run;
        } else {
            match blank_run {
                0 => ch.single_breaks += 1,
                1 => {}
                r => ch.blank_lines_dropped += r - 1,
            }
        }
        blank_run = 0;
        if l.ends_with(char::is_whitespace) {
            ch.trailing_space_lines += 1;
        }
        if l.starts_with(char::is_whitespace) {
            ch.leading_space_lines += 1;
        }
        if s.as_str() != l.trim() {
            ch.squashed_lines += 1;
        }
        out.push(s);
    }
    ch.blank_lines_dropped += blank_run;
    out
}

/// Removes paired `**` and `__` emphasis markers within one line.
pub fn strip_emphasis(l: &str, ch: &mut Changes) -> String {
    let mut s = l.to_string();
    for m in ["**", "__"] {
        let idx: Vec<usize> = s.match_indices(m).map(|x| x.0).collect();
        let pairs = idx.len() / 2;
        if pairs == 0 {
            continue;
        }
        ch.markdown_emphasis_pairs += pairs;
        let drop: HashSet<usize> = idx[..pairs * 2].iter().copied().collect();
        let mut o = String::with_capacity(s.len());
        let mut i = 0;
        while i < s.len() {
            if drop.contains(&i) {
                i += m.len();
                continue;
            }
            let c = s[i..].chars().next().unwrap();
            o.push(c);
            i += c.len_utf8();
        }
        s = text::squash(&o);
    }
    s
}

// ------------------------------------------------------------------ structure

/// Paragraph structure to rebuild: break positions as fractions of the
/// replaced words, and the parent words the first and last segments join.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Structure {
    pub fractions: Vec<f64>,
    pub pre_words: usize,
    pub post_words: usize,
}

fn fractions_of(t: &str) -> Vec<f64> {
    let w: Vec<usize> = t.split('\n').map(text::words).filter(|&x| x > 0).collect();
    let total: usize = w.iter().sum();
    let mut out = Vec::new();
    let mut cum = 0;
    for x in w.iter().take(w.len().saturating_sub(1)) {
        cum += x;
        out.push(cum as f64 / total.max(1) as f64);
    }
    out
}

impl Structure {
    /// A mirror: its parent's paragraphs.
    pub fn whole(parent: &str) -> Structure {
        Structure { fractions: fractions_of(parent), pre_words: 0, post_words: 0 }
    }
    /// A mixed child: the paragraphs of the replaced span `parent[s..e]`.
    pub fn span(parent: &str, s: usize, e: usize) -> Structure {
        let pre = parent[..s].rsplit('\n').next().unwrap_or("");
        let post = parent[e..].split('\n').next().unwrap_or("");
        Structure { fractions: fractions_of(&parent[s..e]), pre_words: text::words(pre), post_words: text::words(post) }
    }
}

pub fn ends_with_abbreviation(s: &str) -> bool {
    let last = s.split_whitespace().last().unwrap_or("");
    let core = last.trim_end_matches(['"', '\'', '\u{201D}', '\u{2019}', ')', ']', '\u{BB}']);
    let Some(stem) = core.strip_suffix('.') else { return false };
    let stem = stem.trim_start_matches(['"', '\'', '\u{201C}', '\u{2018}', '(', '[']).to_lowercase();
    (stem.chars().count() == 1 && stem.chars().all(char::is_alphabetic)) || ABBREVIATIONS.contains(&stem.as_str())
}

pub fn starts_sentence(s: &str) -> bool {
    s.trim_start_matches(['"', '\'', '\u{201C}', '\u{2018}', '(', '[']).chars().next().is_some_and(|c| c.is_uppercase() || c.is_ascii_digit())
}

/// Break choice: indices j (break after the first j sentences), minimum total
/// |cum_j / C - f_i| with every paragraph at least `MIN_PARAGRAPH_WORDS`
/// (first and last counting the joined parent words); ties go to the
/// earliest boundaries. Targets are dropped (the one whose two parent
/// paragraphs are smallest together, earliest on ties) until a placement
/// exists. Returns (breaks, dropped targets).
pub fn place_breaks(words: &[usize], allowed: &[bool], st: &Structure) -> (Vec<usize>, usize) {
    let n = words.len();
    let mut cum = vec![0usize; n + 1];
    for i in 0..n {
        cum[i + 1] = cum[i] + words[i];
    }
    let total = cum[n].max(1) as f64;
    let mut f = st.fractions.clone();
    let mut dropped = 0;
    loop {
        let k = f.len();
        if k == 0 {
            return (vec![], dropped);
        }
        // cost[i][j]: best cost with break i (1-based) after j sentences.
        let inf = f64::INFINITY;
        let mut cost = vec![vec![inf; n + 1]; k + 1];
        let mut back = vec![vec![usize::MAX; n + 1]; k + 1];
        for j in 1..n {
            if allowed[j] && cum[j] + st.pre_words >= MIN_PARAGRAPH_WORDS {
                cost[1][j] = (cum[j] as f64 / total - f[0]).abs();
            }
        }
        for i in 2..=k {
            for j in 1..n {
                if !allowed[j] {
                    continue;
                }
                let d = (cum[j] as f64 / total - f[i - 1]).abs();
                for jp in 1..j {
                    if cost[i - 1][jp] < inf && cum[j] - cum[jp] >= MIN_PARAGRAPH_WORDS && cost[i - 1][jp] + d < cost[i][j] {
                        cost[i][j] = cost[i - 1][jp] + d;
                        back[i][j] = jp;
                    }
                }
            }
        }
        let mut best: Option<usize> = None;
        for j in 1..n {
            if cost[k][j] < inf && cum[n] - cum[j] + st.post_words >= MIN_PARAGRAPH_WORDS && best.is_none_or(|b| cost[k][j] < cost[k][b]) {
                best = Some(j);
            }
        }
        if let Some(mut j) = best {
            let mut out = vec![0; k];
            for i in (1..=k).rev() {
                out[i - 1] = j;
                j = back[i][j];
            }
            return (out, dropped);
        }
        let mut drop_i = 0;
        let mut drop_w = f64::INFINITY;
        for i in 0..k {
            let lo = if i == 0 { 0.0 } else { f[i - 1] };
            let hi = if i + 1 == k { 1.0 } else { f[i + 1] };
            if hi - lo < drop_w {
                drop_w = hi - lo;
                drop_i = i;
            }
        }
        f.remove(drop_i);
        dropped += 1;
    }
}

// ------------------------------------------------------------------ child

#[derive(Clone, Debug, Serialize)]
pub struct Layout {
    pub own_lines: usize,
    pub sentences: usize,
    pub target_breaks: usize,
    pub realised_breaks: usize,
    pub dropped_breaks: usize,
    pub pre_words: usize,
    pub post_words: usize,
}

pub struct Normalised {
    pub text: String,
    pub changes: Changes,
    pub layout: Layout,
    /// Line-level form failures (list-like or unterminated generated lines).
    pub line_reasons: Vec<String>,
}

/// Normalises one generated text to the conventions and paragraph structure.
pub fn normalise_child(t: &str, conv: &Conventions, st: &Structure) -> Normalised {
    let r = rebuild_child(t, conv, st, false);
    Normalised { text: r.paragraphs.join("\n\n"), changes: r.changes, layout: r.layout, line_reasons: r.line_reasons }
}

/// The child normaliser's steps before the paragraphs are joined: the
/// rebuilt paragraphs (each one squashed line, none empty), with the change
/// counts, the layout record and the line-level form failures.
pub struct Rebuilt {
    pub paragraphs: Vec<String>,
    pub changes: Changes,
    pub layout: Layout,
    pub line_reasons: Vec<String>,
    /// Where each paragraph of `paragraphs` comes from in the input, in
    /// order. A diagnostic for the §4.3 dry run's counts (B23; protocol A2
    /// items 10.12, 13.10 and 14.1): no rule reads it, and every other field
    /// is unchanged by it.
    pub sources: Vec<ParagraphSource>,
}

/// Where a rebuilt paragraph's text comes from in `rebuild_child`'s input
/// (a diagnostic; no rule reads it). Every boundary between two paragraphs
/// of one `rebuild_child` call is a break `place_breaks` placed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParagraphSource {
    /// The byte range of the input from the start of the first line the
    /// paragraph draws a sentence from to the end of the last (the line's
    /// newline excluded).
    pub raw: (usize, usize),
    /// The paragraph starts after a sentence of its first line: a break
    /// placed inside a line.
    pub starts_mid_line: bool,
    /// The paragraph ends before a sentence of its last line.
    pub ends_mid_line: bool,
}

/// The byte ranges of `t`'s lines (split on `'\n'`, the newline excluded)
/// that `layout_lines` keeps, in its order.
fn kept_line_ranges(t: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for l in t.split('\n') {
        if !text::squash(l).is_empty() {
            out.push((at, at + l.len()));
        }
        at += l.len() + 1;
    }
    out
}

/// `normalise_child` up to the join: lines squashed and blank lines dropped,
/// paired emphasis stripped unless `keep_emphasis`, sites and quotes
/// rendered, then paragraphs rebuilt at sentence boundaries to `st`.
/// `normalise_child` is this with `keep_emphasis = false`, its paragraphs
/// joined by one blank line; profile v2 (`transfer`) keeps the paragraphs
/// and lays them out like its source.
pub fn rebuild_child(t: &str, conv: &Conventions, st: &Structure, keep_emphasis: bool) -> Rebuilt {
    let mut ch = Changes::default();
    let mut lines: Vec<String> = Vec::new();
    // each kept line's byte range in `t` (the `sources` diagnostic)
    let mut line_raw: Vec<(usize, usize)> = Vec::new();
    let ranges = kept_line_ranges(t);
    for (k, l) in layout_lines(t, &mut ch).into_iter().enumerate() {
        let l = if keep_emphasis { l } else { strip_emphasis(&l, &mut ch) };
        if l.is_empty() {
            continue;
        }
        let l = render_sites(&l, conv, &mut ch);
        lines.push(render_quotes(&l, conv, &mut ch));
        line_raw.push(ranges.get(k).copied().unwrap_or_default());
    }
    let mut line_reasons = Vec::new();
    for l in &lines {
        if list_like(l) {
            line_reasons.push("form_line_list_like".to_string());
        }
        if !text::ends_sentence(l) {
            line_reasons.push("form_line_no_sentence_end".to_string());
        }
    }
    line_reasons.sort();
    line_reasons.dedup();
    // Sentences per line; a boundary between lines is always admissible.
    let mut sents: Vec<(String, bool)> = Vec::new();
    let mut sent_line: Vec<usize> = Vec::new();
    for (li, l) in lines.iter().enumerate() {
        for (k, s) in text::sentences(l).into_iter().enumerate() {
            sents.push((l[s.start..s.end].to_string(), k == 0));
            sent_line.push(li);
        }
    }
    let words: Vec<usize> = sents.iter().map(|s| text::words(&s.0)).collect();
    let allowed: Vec<bool> = (0..=sents.len())
        .map(|j| j > 0 && j < sents.len() && (sents[j].1 || (!ends_with_abbreviation(&sents[j - 1].0) && starts_sentence(&sents[j].0))))
        .collect();
    let (breaks, dropped) = place_breaks(&words, &allowed, st);
    let mut paras: Vec<String> = Vec::new();
    let mut sources: Vec<ParagraphSource> = Vec::new();
    let mut start = 0;
    for &b in breaks.iter().chain(std::iter::once(&sents.len())) {
        paras.push(sents[start..b].iter().map(|s| s.0.as_str()).collect::<Vec<_>>().join(" "));
        sources.push(if start < b {
            ParagraphSource {
                raw: (line_raw[sent_line[start]].0, line_raw[sent_line[b - 1]].1),
                starts_mid_line: !sents[start].1,
                ends_mid_line: b < sents.len() && !sents[b].1,
            }
        } else {
            ParagraphSource::default()
        });
        start = b;
    }
    let sources = paras.iter().zip(sources).filter(|(p, _)| !p.is_empty()).map(|(_, s)| s).collect();
    Rebuilt {
        sources,
        paragraphs: paras.into_iter().filter(|p| !p.is_empty()).collect(),
        changes: ch,
        layout: Layout {
            own_lines: lines.len(),
            sentences: sents.len(),
            target_breaks: st.fractions.len(),
            realised_breaks: breaks.len(),
            dropped_breaks: dropped,
            pre_words: st.pre_words,
            post_words: st.post_words,
        },
        line_reasons,
    }
}

/// Byte ranges of the blank-line-separated paragraphs of `t`.
pub fn paragraph_ranges(t: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, _) in t.match_indices("\n\n") {
        out.push((start, i));
        start = i + 2;
    }
    out.push((start, t.len()));
    out.into_iter().filter(|(a, b)| !t[*a..*b].trim().is_empty()).collect()
}
