//! The form screen: the FineWeb line classifier the human excerpts pass
//! (list markers, boilerplate terms, a site-template flag, then the shared
//! prose screen `text::Para::classify` with `Screen::MARKUP`), and the
//! paragraph screen that applies it to every blank-line paragraph of a
//! document or to those overlapping given spans.
//!
//! `classify_all` gives every reason of a paragraph, for profile v2's
//! licence screen (protocol A2 amendment 10 item 10.5).
//!
//! `BOILERPLATE`, `list_like`, `boilerplate` and `classify` moved verbatim
//! from `grammar_a_pools::fineweb`, which re-exports them; `paragraph_screen`
//! moved verbatim from `grammar-cc-human`'s `era_norm` (v1).
use crate::layout::paragraph_ranges;
use crate::text::{self, Para, Screen};

/// Lowercase substrings that mark a navigation, legal, commerce or comment
/// boilerplate paragraph (the paragraph becomes a break).
pub const BOILERPLATE: &[&str] = &[
    "cookie", "privacy policy", "terms of use", "terms of service", "terms and conditions", "all rights reserved",
    "copyright", "\u{a9}", "click here", "subscribe", "sign up", "log in", "newsletter", "share this", "posted by",
    "posted on", "leave a comment", "leave a reply", "read more", "add to cart", "javascript", "please enable",
    "your email", "email address", "e-mail address", "skip to", "powered by", "comments are closed",
];

/// A list item, table row or similar line that is not running prose.
pub fn list_like(t: &str) -> bool {
    let s = t.trim_start();
    s.starts_with(['-', '*', '\u{2022}', '\u{b7}', '>', '|', '#', '\u{25aa}', '\u{25cf}', '\u{2013}', '\u{2014}'])
        || s.split_once(['.', ')']).is_some_and(|(a, _)| !a.is_empty() && a.len() <= 3 && a.chars().all(|c| c.is_ascii_digit()))
}

pub fn boilerplate(t: &str) -> bool {
    let l = t.to_lowercase();
    BOILERPLATE.iter().any(|b| l.contains(b)) || l.contains('@')
}

/// Classify one FineWeb text line.
pub fn classify(line: &str, repeated_on_site: bool) -> Para {
    let t = text::squash(line);
    if list_like(&t) {
        return Para::non_prose(t, "list_like_line");
    }
    if boilerplate(&t) {
        return Para::non_prose(t, "boilerplate_term");
    }
    if repeated_on_site {
        return Para::non_prose(t, "repeated_on_site_template");
    }
    Para::classify(t, Screen::MARKUP)
}

/// The licence screen's id (protocol A2 amendment 10 item 10.5): profile
/// v2's form screen on the prose path.
pub const FORM_LICENCE_SCREEN_ID: &str = "cc_form_licence_screen_v1";

/// `classify`'s list test split in two (amendment 10 item 10.5): a line
/// starting with `>` (after its leading whitespace).
pub const QUOTED_LINE: &str = "quoted_line";
/// Any other `list_like` line start.
pub const LIST_MARKER: &str = "list_marker";

/// The licence reason of one line's list test: `quoted_line`,
/// `list_marker`, or none.
pub fn list_reason(line: &str) -> Option<&'static str> {
    if !list_like(line) {
        None
    } else if line.trim_start().starts_with('>') {
        Some(QUOTED_LINE)
    } else {
        Some(LIST_MARKER)
    }
}

/// The v1 reason name a licence reason is reported under: both halves of
/// the list test are `classify`'s `list_like_line`.
pub fn v1_reason(r: &str) -> &str {
    if r == QUOTED_LINE || r == LIST_MARKER { "list_like_line" } else { r }
}

/// `classify_all` (amendment 10 item 10.5): every reason `classify`'s tests
/// would give paragraph `p` (no site template), each test run on its own,
/// in `classify`'s order. The list test is split into `quoted_line` and
/// `list_marker` and run on every line of `p`; the other tests read the
/// squashed paragraph, as `classify` does. Empty exactly when `classify`
/// passes a paragraph whose lines after the first are not `list_like` (every
/// profile v2 paragraph: a wrap never joins a `list_like` line), and then
/// its first reason, under `v1_reason`, is `classify`'s.
pub fn classify_all(p: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    let (mut quoted, mut marker) = (false, false);
    for l in p.split('\n') {
        match list_reason(l) {
            Some(QUOTED_LINE) => quoted = true,
            Some(_) => marker = true,
            None => {}
        }
    }
    if quoted {
        out.push(QUOTED_LINE);
    }
    if marker {
        out.push(LIST_MARKER);
    }
    let t = text::squash(p);
    if boilerplate(&t) {
        out.push("boilerplate_term");
    }
    if t.contains(text::GAP) {
        out.push("removed_inline_content");
    }
    if t.contains('\u{FFFD}') {
        out.push("replacement_character");
    }
    if t.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
        out.push("control_character");
    }
    if text::words(&t) < 8 {
        out.push("short_paragraph");
    }
    if !text::ends_sentence(&t) {
        out.push("no_sentence_end");
    }
    if text::has_url(&t) {
        out.push("url");
    }
    if ["{{", "}}", "[[", "]]", "{|", "|}", "''", "__"].iter().any(|m| t.contains(m)) || t.contains('<') || t.contains('>') {
        out.push("residual_markup");
    }
    if text::ascii_letter_ratio(&t) < 0.9 {
        out.push("non_ascii_letters");
    }
    if text::long_quotation(&t) {
        out.push("long_quotation");
    }
    out
}

/// The excerpt paragraph screen (`classify`, no site-template rule)
/// on every paragraph overlapping one of `spans` (all paragraphs when None).
pub fn paragraph_screen(t: &str, spans: Option<&[(usize, usize)]>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (a, b) in paragraph_ranges(t) {
        if let Some(sp) = spans
            && !sp.iter().any(|&(s, e)| s < b && a < e)
        {
            continue;
        }
        let p = classify(&t[a..b], false);
        if let Some(r) = p.reason {
            out.push(format!("form_paragraph_{r}"));
        }
    }
    out.sort();
    out.dedup();
    out
}
