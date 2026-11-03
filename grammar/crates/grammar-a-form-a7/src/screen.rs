//! The form screen: the FineWeb line classifier the human excerpts pass
//! (list markers, boilerplate terms, a site-template flag, then the shared
//! prose screen `text::Para::classify` with `Screen::MARKUP`), and the
//! paragraph screen that applies it to every blank-line paragraph of a
//! document or to those overlapping given spans.
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
