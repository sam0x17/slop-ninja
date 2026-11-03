//! Parent conventions (`cc_era_parent_norm_v1`): quote, dash, numeric-range
//! and ellipsis sites, a parent's own majority, the batch prior and its keyed
//! draws, and rendering a line in the resolved conventions.
//!
//! * **Parent profile.** Counts of curly and ASCII double and single quotes,
//!   dash punctuation by style (em closed or spaced, spaced en dash, spaced
//!   hyphen, double hyphen closed or spaced), numeric ranges (en dash or
//!   hyphen) and ellipses (character or three dots). Each convention is the
//!   parent's majority; a tie or no evidence falls back to the parent's other
//!   quote class (quotes only), then to a keyed draw from the batch prior: the
//!   share of each convention among batch parents that show it. Draws are
//!   keyed by `rank(seed, KEY_SCOPE, doc_id:field)`, so every attempt and the
//!   top-up of one document see the same choice.
//! * **Rendering.** Every quote, dash, range and ellipsis site is rendered in
//!   the resolved convention; ASCII quotes become curly by context.
//!
//! Moved verbatim from `grammar-cc-human`'s `era_norm` (v1); the byte-identity
//! tests pin the outputs to those recorded before the move.
use crate::rank::rank;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const KEY_SCOPE: &str = "cc_era_parent_norm_key_v1";
/// Word-initial elisions that take an apostrophe, not an opening quote.
pub const ELISIONS: [&str; 6] = ["tis", "twas", "til", "em", "cause", "n"];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quote {
    Curly,
    Ascii,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dash {
    EmClosed,
    EmSpaced,
    EnSpaced,
    HyphenSpaced,
    DoubleClosed,
    DoubleSpaced,
}
pub const DASHES: [Dash; 6] = [Dash::EmClosed, Dash::EmSpaced, Dash::EnSpaced, Dash::HyphenSpaced, Dash::DoubleClosed, Dash::DoubleSpaced];

impl Dash {
    pub fn render(self) -> &'static str {
        match self {
            Dash::EmClosed => "\u{2014}",
            Dash::EmSpaced => " \u{2014} ",
            Dash::EnSpaced => " \u{2013} ",
            Dash::HyphenSpaced => " - ",
            Dash::DoubleClosed => "--",
            Dash::DoubleSpaced => " -- ",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Range {
    En,
    Hyphen,
}
pub const RANGES: [Range; 2] = [Range::En, Range::Hyphen];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ellipsis {
    Char,
    Dots,
}
pub const ELLIPSES: [Ellipsis; 2] = [Ellipsis::Char, Ellipsis::Dots];

// ------------------------------------------------------------------ sites

/// One replaceable site: byte range (spaces of spaced dashes included) and its style.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Site {
    Dash(usize, usize, Dash),
    Range(usize, usize, Range),
    Ellipsis(usize, usize, Ellipsis),
}

/// Whether the whitespace token holding byte `i` is a plain numeric range
/// `\d{1,4}<dash>\d{1,4}`, with optional opening bracket and closing punctuation.
fn numeric_range_token(t: &str, i: usize, dash_len: usize) -> bool {
    let ts = t[..i].rfind(char::is_whitespace).map_or(0, |k| k + t[k..].chars().next().map_or(1, char::len_utf8));
    let te = t[i + dash_len..].find(char::is_whitespace).map_or(t.len(), |k| i + dash_len + k);
    let a = t[ts..i].trim_start_matches(['(', '[']);
    let b = t[i + dash_len..te].trim_end_matches([')', ']', ',', '.', ';', ':', '!', '?']);
    let digits = |s: &str| (1..=4).contains(&s.len()) && s.chars().all(|c| c.is_ascii_digit());
    digits(a) && digits(b)
}

/// Dash, range and ellipsis sites of one line (no newlines inside a site).
pub fn sites(t: &str) -> Vec<Site> {
    let cs: Vec<(usize, char)> = t.char_indices().collect();
    let n = cs.len();
    let at = |k: usize| cs.get(k).map(|x| x.1);
    let pos = |k: usize| cs.get(k).map_or(t.len(), |x| x.0);
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = cs[i].1;
        let prev = if i > 0 { at(i - 1) } else { None };
        match c {
            '\u{2014}' | '\u{2015}' => {
                let mut j = i;
                while j < n && matches!(cs[j].1, '\u{2014}' | '\u{2015}') {
                    j += 1;
                }
                let next = at(j);
                if j - i == 1
                    && let (Some(p), Some(q)) = (prev, next)
                {
                    if p == ' ' && q == ' ' && i >= 2 && at(i - 2).is_some_and(|x| x != ' ') && at(j + 1).is_some_and(|x| x != ' ') {
                        out.push(Site::Dash(pos(i - 1), pos(j + 1), Dash::EmSpaced));
                    } else if !p.is_whitespace() && !q.is_whitespace() {
                        out.push(Site::Dash(pos(i), pos(j), Dash::EmClosed));
                    }
                }
                i = j;
                continue;
            }
            '\u{2013}' => {
                let next = at(i + 1);
                if prev.is_some_and(|p| p.is_ascii_digit()) && next.is_some_and(|q| q.is_ascii_digit()) {
                    if numeric_range_token(t, cs[i].0, c.len_utf8()) {
                        out.push(Site::Range(pos(i), pos(i + 1), Range::En));
                    }
                } else if prev == Some(' ') && next == Some(' ') && i >= 2 && at(i - 2).is_some_and(|x| x != ' ') && at(i + 2).is_some_and(|x| x != ' ') {
                    out.push(Site::Dash(pos(i - 1), pos(i + 2), Dash::EnSpaced));
                }
            }
            '-' => {
                let mut j = i;
                while j < n && cs[j].1 == '-' {
                    j += 1;
                }
                let next = at(j);
                if j - i == 2 {
                    if let (Some(p), Some(q)) = (prev, next) {
                        if p == ' ' && q == ' ' && i >= 2 && at(i - 2).is_some_and(|x| x != ' ') && at(j + 1).is_some_and(|x| x != ' ') {
                            out.push(Site::Dash(pos(i - 1), pos(j + 1), Dash::DoubleSpaced));
                        } else if !p.is_whitespace() && !q.is_whitespace() {
                            out.push(Site::Dash(pos(i), pos(j), Dash::DoubleClosed));
                        }
                    }
                } else if j - i == 1 {
                    if prev == Some(' ') && next == Some(' ') && i >= 2 && at(i - 2).is_some_and(|x| x != ' ') && at(i + 2).is_some_and(|x| x != ' ') {
                        out.push(Site::Dash(pos(i - 1), pos(i + 2), Dash::HyphenSpaced));
                    } else if prev.is_some_and(|p| p.is_ascii_digit()) && next.is_some_and(|q| q.is_ascii_digit()) && numeric_range_token(t, cs[i].0, 1) {
                        out.push(Site::Range(pos(i), pos(i + 1), Range::Hyphen));
                    }
                }
                i = j;
                continue;
            }
            '\u{2026}' => out.push(Site::Ellipsis(pos(i), pos(i + 1), Ellipsis::Char)),
            '.' => {
                let mut j = i;
                while j < n && cs[j].1 == '.' {
                    j += 1;
                }
                if j - i == 3 {
                    out.push(Site::Ellipsis(pos(i), pos(j), Ellipsis::Dots));
                }
                i = j;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

// ------------------------------------------------------------------ profile

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Counts {
    pub dq_curly: usize,
    pub dq_ascii: usize,
    pub sq_curly: usize,
    pub sq_ascii: usize,
    pub dash: BTreeMap<Dash, usize>,
    pub range: BTreeMap<Range, usize>,
    pub ellipsis: BTreeMap<Ellipsis, usize>,
}

pub fn counts(t: &str) -> Counts {
    let mut c = Counts::default();
    for ch in t.chars() {
        match ch {
            '"' => c.dq_ascii += 1,
            '\u{201C}' | '\u{201D}' => c.dq_curly += 1,
            '\'' => c.sq_ascii += 1,
            '\u{2018}' | '\u{2019}' => c.sq_curly += 1,
            _ => {}
        }
    }
    for line in t.split('\n') {
        for s in sites(line) {
            match s {
                Site::Dash(_, _, d) => *c.dash.entry(d).or_default() += 1,
                Site::Range(_, _, r) => *c.range.entry(r).or_default() += 1,
                Site::Ellipsis(_, _, e) => *c.ellipsis.entry(e).or_default() += 1,
            }
        }
    }
    c
}

/// Strict majority of a two-way count; a tie or no evidence gives None.
fn majority2(curly: usize, ascii: usize) -> Option<Quote> {
    match curly.cmp(&ascii) {
        std::cmp::Ordering::Greater => Some(Quote::Curly),
        std::cmp::Ordering::Less => Some(Quote::Ascii),
        std::cmp::Ordering::Equal => None,
    }
}

/// Unique most frequent key; a tie or no evidence gives None.
fn majority<K: Copy + Ord>(m: &BTreeMap<K, usize>) -> Option<K> {
    let best = m.values().copied().max().filter(|&x| x > 0)?;
    let top: Vec<K> = m.iter().filter(|(_, v)| **v == best).map(|(k, _)| *k).collect();
    (top.len() == 1).then(|| top[0])
}

/// A parent's own conventions (None where it shows no clear majority).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Own {
    pub double: Option<Quote>,
    pub single: Option<Quote>,
    pub quotes: Option<Quote>,
    pub dash: Option<Dash>,
    pub range: Option<Range>,
    pub ellipsis: Option<Ellipsis>,
}

pub fn own(c: &Counts) -> Own {
    Own {
        double: majority2(c.dq_curly, c.dq_ascii),
        single: majority2(c.sq_curly, c.sq_ascii),
        quotes: majority2(c.dq_curly + c.sq_curly, c.dq_ascii + c.sq_ascii),
        dash: majority(&c.dash),
        range: majority(&c.range),
        ellipsis: majority(&c.ellipsis),
    }
}

/// Batch prior: how many batch parents show each convention as their own.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Prior {
    pub parents: usize,
    pub quotes: BTreeMap<Quote, usize>,
    pub dash: BTreeMap<Dash, usize>,
    pub range: BTreeMap<Range, usize>,
    pub ellipsis: BTreeMap<Ellipsis, usize>,
}

impl Prior {
    pub fn of<'a>(texts: impl Iterator<Item = &'a str>) -> Prior {
        let mut p = Prior::default();
        for t in texts {
            p.parents += 1;
            let o = own(&counts(t));
            if let Some(q) = o.quotes {
                *p.quotes.entry(q).or_default() += 1;
            }
            if let Some(d) = o.dash {
                *p.dash.entry(d).or_default() += 1;
            }
            if let Some(r) = o.range {
                *p.range.entry(r).or_default() += 1;
            }
            if let Some(e) = o.ellipsis {
                *p.ellipsis.entry(e).or_default() += 1;
            }
        }
        p
    }
}

fn unit(seed: &str, doc: &str, field: &str) -> f64 {
    (rank(seed, KEY_SCOPE, &format!("{doc}:{field}")) as f64 + 0.5) / 18446744073709551616.0
}

/// Keyed categorical draw in the fixed order of `keys`; None when the prior is empty.
fn draw<K: Copy + Ord>(keys: &[K], tally: &BTreeMap<K, usize>, u: f64) -> Option<K> {
    let total: usize = keys.iter().map(|k| tally.get(k).copied().unwrap_or(0)).sum();
    if total == 0 {
        return None;
    }
    let mut cum = 0.0;
    for k in keys {
        cum += tally.get(k).copied().unwrap_or(0) as f64 / total as f64;
        if u <= cum {
            return Some(*k);
        }
    }
    keys.iter().rev().find(|k| tally.get(k).copied().unwrap_or(0) > 0).copied()
}

/// One resolved convention and where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Choice<K> {
    pub value: Option<K>,
    /// parent | parent_other_quote | keyed_prior | none
    pub source: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Conventions {
    pub double: Choice<Quote>,
    pub single: Choice<Quote>,
    pub dash: Choice<Dash>,
    pub range: Choice<Range>,
    pub ellipsis: Choice<Ellipsis>,
}

fn choose<K: Copy + Ord>(own: Option<K>, keys: &[K], tally: &BTreeMap<K, usize>, u: f64) -> Choice<K> {
    match own {
        Some(v) => Choice { value: Some(v), source: "parent" },
        None => match draw(keys, tally, u) {
            Some(v) => Choice { value: Some(v), source: "keyed_prior" },
            None => Choice { value: None, source: "none" },
        },
    }
}

/// Conventions for document `doc` from its parent's counts and the prior.
pub fn resolve(c: &Counts, prior: &Prior, seed: &str, doc: &str) -> Conventions {
    let o = own(c);
    let uq = unit(seed, doc, "quotes");
    let quote = |mine: Option<Quote>, other: Option<Quote>| match (mine, other) {
        (Some(v), _) => Choice { value: Some(v), source: "parent" },
        (None, Some(v)) => Choice { value: Some(v), source: "parent_other_quote" },
        (None, None) => match o.quotes {
            Some(v) => Choice { value: Some(v), source: "parent" },
            None => match draw(&[Quote::Curly, Quote::Ascii], &prior.quotes, uq) {
                Some(v) => Choice { value: Some(v), source: "keyed_prior" },
                None => Choice { value: None, source: "none" },
            },
        },
    };
    Conventions {
        double: quote(o.double, o.single),
        single: quote(o.single, o.double),
        dash: choose(o.dash, &DASHES, &prior.dash, unit(seed, doc, "dash")),
        range: choose(o.range, &RANGES, &prior.range, unit(seed, doc, "range")),
        ellipsis: choose(o.ellipsis, &ELLIPSES, &prior.ellipsis, unit(seed, doc, "ellipsis")),
    }
}

// ------------------------------------------------------------------ rendering

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Changes {
    pub trailing_space_lines: usize,
    pub leading_space_lines: usize,
    pub squashed_lines: usize,
    pub blank_lines_dropped: usize,
    pub single_breaks: usize,
    pub markdown_emphasis_pairs: usize,
    pub double_quotes: usize,
    pub single_quotes: usize,
    pub dashes: usize,
    pub ranges: usize,
    pub ellipses: usize,
}

fn opens_after(p: Option<char>) -> bool {
    p.is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | '{' | '<' | '\u{2014}' | '\u{2013}' | '-' | '/' | '\u{2018}' | '\u{201C}'))
}

/// Renders the dash, range and ellipsis sites of one line in the target styles.
pub fn render_sites(l: &str, conv: &Conventions, ch: &mut Changes) -> String {
    let mut out = String::with_capacity(l.len() + 8);
    let mut cur = 0;
    for s in sites(l) {
        let (a, b, rep, kind): (usize, usize, Option<&str>, u8) = match s {
            Site::Dash(a, b, d) => (a, b, conv.dash.value.filter(|t| *t != d).map(Dash::render), 0),
            Site::Range(a, b, r) => (a, b, conv.range.value.filter(|t| *t != r).map(|t| if t == Range::En { "\u{2013}" } else { "-" }), 1),
            Site::Ellipsis(a, b, e) => (a, b, conv.ellipsis.value.filter(|t| *t != e).map(|t| if t == Ellipsis::Char { "\u{2026}" } else { "..." }), 2),
        };
        if let Some(r) = rep {
            out.push_str(&l[cur..a]);
            out.push_str(r);
            cur = b;
            match kind {
                0 => ch.dashes += 1,
                1 => ch.ranges += 1,
                _ => ch.ellipses += 1,
            }
        }
    }
    out.push_str(&l[cur..]);
    out
}

/// Renders the quotes of one line in the target conventions.
pub fn render_quotes(l: &str, conv: &Conventions, ch: &mut Changes) -> String {
    let cs: Vec<char> = l.chars().collect();
    let mut out = String::with_capacity(l.len() + 8);
    for (i, &c) in cs.iter().enumerate() {
        let prev = if i > 0 { Some(cs[i - 1]) } else { None };
        let next = cs.get(i + 1).copied();
        let r = match c {
            '"' if conv.double.value == Some(Quote::Curly) => {
                if opens_after(prev) { '\u{201C}' } else { '\u{201D}' }
            }
            '\u{201C}' | '\u{201D}' if conv.double.value == Some(Quote::Ascii) => '"',
            '\'' if conv.single.value == Some(Quote::Curly) => {
                if !opens_after(prev) || next.is_none_or(|q| q.is_whitespace() || q.is_ascii_digit()) {
                    '\u{2019}'
                } else {
                    let w: String = cs[i + 1..].iter().take_while(|c| c.is_alphabetic()).collect::<String>().to_lowercase();
                    let after = cs.get(i + 1 + w.chars().count()).copied();
                    if ELISIONS.contains(&w.as_str()) && after.is_none_or(|a| !a.is_alphabetic()) { '\u{2019}' } else { '\u{2018}' }
                }
            }
            '\u{2018}' | '\u{2019}' if conv.single.value == Some(Quote::Ascii) => '\'',
            c => c,
        };
        if r != c {
            if matches!(c, '"' | '\u{201C}' | '\u{201D}') {
                ch.double_quotes += 1;
            } else {
                ch.single_quotes += 1;
            }
        }
        out.push(r);
    }
    out
}
