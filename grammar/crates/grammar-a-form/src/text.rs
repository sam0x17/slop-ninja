//! Pure text rules shared by the form path: whitespace words and the
//! whitespace normalisation, the shared prose screen (`Para`, `Screen`,
//! `prose_reason` and its parts), the 64-bit key hash, and the sealed
//! sentence splitter `a_comp_sentences_v1`.
//!
//! The prose-screen items, `words`, `normalized`, `squash`, `h64` and the
//! admitted-document bounds moved verbatim from `grammar_a_pools::text`,
//! which re-exports them. The sentence splitter came from
//! `grammar_a_positives::text`, which now re-exports this one.
use sha2::{Digest, Sha256};

/// Minimum and maximum whitespace words for an admitted human document.
pub const MIN_WORDS: usize = 150;
pub const MAX_WORDS: usize = 450;

/// First eight bytes of SHA-256, big-endian. Index keys only; a collision
/// can only exclude a candidate, never admit one.
pub fn h64(b: &[u8]) -> u64 {
    let d = Sha256::digest(b);
    u64::from_be_bytes(d[..8].try_into().expect("digest length"))
}

pub fn words(s: &str) -> usize {
    s.split_whitespace().count()
}

pub fn normalized(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Characters that indicate decoding damage or non-text payloads.
pub fn damaged(s: &str) -> Option<&'static str> {
    if s.contains('\u{FFFD}') {
        return Some("replacement_character");
    }
    if s
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Some("control_character");
    }
    None
}

/// Share of ASCII letters among all alphabetic characters.
pub fn ascii_letter_ratio(s: &str) -> f64 {
    let (mut a, mut all) = (0usize, 0usize);
    for c in s.chars().filter(|c| c.is_alphabetic()) {
        all += 1;
        if c.is_ascii_alphabetic() {
            a += 1;
        }
    }
    if all == 0 { 0.0 } else { a as f64 / all as f64 }
}

/// A quoted span of eight or more words (the Wikipedia admission screen).
pub fn long_quotation(s: &str) -> bool {
    let pairs = [('"', '"'), ('“', '”'), ('«', '»')];
    for (open, close) in pairs {
        let mut rest = s;
        while let Some(i) = rest.find(open) {
            let after = &rest[i + open.len_utf8()..];
            match after.find(close) {
                Some(j) => {
                    let inner = &after[..j];
                    if !inner.contains('\n') && inner.split_whitespace().count() >= 8 {
                        return true;
                    }
                    rest = &after[j + close.len_utf8()..];
                }
                None => break,
            }
        }
    }
    false
}

/// Ends with sentence-final punctuation, optionally followed by closing
/// quotes or brackets.
pub fn ends_sentence(s: &str) -> bool {
    let t = s.trim_end().trim_end_matches(['"', '\'', '”', '’', ')', ']', '»']);
    t.ends_with(['.', '!', '?'])
}

pub fn has_url(s: &str) -> bool {
    let l = s.to_lowercase();
    l.contains("http://") || l.contains("https://") || l.contains("www.")
}

/// One rendered paragraph. `prose` is false for headings, lists, quotations,
/// code, tables, unresolved markup or any gap left by removed content.
#[derive(Clone, Debug, PartialEq)]
pub struct Para {
    pub text: String,
    pub prose: bool,
    pub reason: Option<&'static str>,
}

impl Para {
    pub fn non_prose(text: impl Into<String>, reason: &'static str) -> Self {
        Para {
            text: text.into(),
            prose: false,
            reason: Some(reason),
        }
    }
    /// Classify rendered paragraph text with the shared prose screen.
    pub fn classify(text: impl Into<String>, screen: Screen) -> Self {
        let text: String = text.into();
        let reason = prose_reason(&text, screen);
        Para {
            prose: reason.is_none(),
            reason,
            text,
        }
    }
}

/// Source-specific parts of the prose screen.
#[derive(Clone, Copy, Debug)]
pub struct Screen {
    /// Treat a quoted span of eight or more words as a break.
    pub quotes: bool,
    /// Treat `<` and `>` as residual markup (wikitext/HTML renderers only;
    /// XML parsers have already removed tags, so these are literal text).
    pub angle_brackets: bool,
}

impl Screen {
    pub const MARKUP: Screen = Screen { quotes: true, angle_brackets: true };
    pub const XML: Screen = Screen { quotes: true, angle_brackets: false };
}

/// Why a rendered paragraph is not admissible prose, if it is not.
pub fn prose_reason(t: &str, screen: Screen) -> Option<&'static str> {
    if t.contains(GAP) {
        return Some("removed_inline_content");
    }
    if let Some(r) = damaged(t) {
        return Some(r);
    }
    if words(t) < 8 {
        return Some("short_paragraph");
    }
    if !ends_sentence(t) {
        return Some("no_sentence_end");
    }
    if has_url(t) {
        return Some("url");
    }
    for m in ["{{", "}}", "[[", "]]", "{|", "|}", "''", "__"] {
        if t.contains(m) {
            return Some("residual_markup");
        }
    }
    if screen.angle_brackets && (t.contains('<') || t.contains('>')) {
        return Some("residual_markup");
    }
    if ascii_letter_ratio(t) < 0.9 {
        return Some("non_ascii_letters");
    }
    if screen.quotes && long_quotation(t) {
        return Some("long_quotation");
    }
    None
}

/// Sentinel placed where removed inline content would have rendered text.
pub const GAP: char = '\u{E000}';

/// Collapse internal whitespace of a rendered paragraph.
pub fn squash(s: &str) -> String {
    normalized(s)
}

// ------------------------------------------------------------------ sentences

pub const SPLITTER_ID: &str = "a_comp_sentences_v1:terminal_punct_closers_then_whitespace_or_blank_line_or_end";

/// One sentence: byte range without surrounding whitespace, and whether it
/// ends with terminal punctuation (as opposed to a blank line or text end).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sentence {
    pub start: usize,
    pub end: usize,
    pub punctuated: bool,
}

fn is_terminal(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…')
}
fn is_closer(c: char) -> bool {
    matches!(c, '"' | '\'' | ')' | ']' | '”' | '’' | '»' | '}')
}

/// Sealed sentence splitter. A sentence ends after a run of terminal
/// punctuation (`. ! ? …`) and any closing quotes or brackets when the next
/// character is whitespace or the text ends; at a blank line (a newline, any
/// whitespace, then another newline); or at the end of the text. Sentences
/// exclude surrounding whitespace, so every boundary sits on whitespace and
/// splices never join tokens.
pub fn sentences(text: &str) -> Vec<Sentence> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut last_non_ws_end = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let (pos, c) = chars[i];
        if c.is_whitespace() {
            if c == '\n' && start.is_some() {
                // blank line?
                let mut k = i + 1;
                let mut blank = false;
                while k < chars.len() && chars[k].1.is_whitespace() {
                    if chars[k].1 == '\n' {
                        blank = true;
                        break;
                    }
                    k += 1;
                }
                if blank {
                    out.push(Sentence { start: start.take().unwrap(), end: last_non_ws_end, punctuated: false });
                }
            }
            i += 1;
            continue;
        }
        if start.is_none() {
            start = Some(pos);
        }
        last_non_ws_end = pos + c.len_utf8();
        if is_terminal(c) {
            let mut k = i + 1;
            while k < chars.len() && (is_terminal(chars[k].1) || is_closer(chars[k].1)) {
                k += 1;
            }
            let end = if k < chars.len() { chars[k].0 } else { text.len() };
            if k == chars.len() || chars[k].1.is_whitespace() {
                out.push(Sentence { start: start.take().unwrap(), end, punctuated: true });
                last_non_ws_end = end;
                i = k;
                continue;
            }
            // not a boundary: advance over the run as ordinary characters
            last_non_ws_end = end;
            i = k;
            continue;
        }
        i += 1;
    }
    if let Some(s) = start {
        out.push(Sentence { start: s, end: last_non_ws_end, punctuated: false });
    }
    out
}
