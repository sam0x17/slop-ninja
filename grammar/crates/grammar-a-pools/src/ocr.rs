//! OCR and transcription normalisation and quality measures shared by the
//! historical adapters (American Stories, British Library Heritage Made
//! Digital newspapers, Library of Congress By the People transcriptions).
//!
//! * **Lexicon.** A plain word list (one lowercase ASCII word per line),
//!   built by `ocr-lexicon` from cleanly typeset Train-side text (the
//!   admitted Project Gutenberg pool). It only measures OCR quality and
//!   chooses between keeping and dropping a line-end hyphen; it never edits
//!   a word.
//! * **Normalisation** (`ocr_line_unwrap_dehyphenate_v1`). Inside one
//!   paragraph block, hard line breaks become single spaces. A line that ends
//!   in `letter-` followed by a line that starts with a lowercase letter is a
//!   line-end hyphenation: the hyphen is dropped when the joined word is in
//!   the lexicon or either part is not, and kept (a real compound) when both
//!   parts are lexicon words and the joined form is not. Ligatures and the
//!   long s are mapped to their plain letters, and soft hyphens are removed.
//!   Every change is counted per document. No other character is altered:
//!   OCR misreadings stay in the text and are measured instead.
//! * **Quality** (`ocr_quality_v1`). Over whitespace tokens with surrounding
//!   punctuation stripped: the lexicon share of alphabetic tokens (all, and
//!   lowercase-initial only), the share of odd tokens (letters and digits
//!   mixed other than ordinals, or no letter or digit at all apart from
//!   ordinary dashes and symbols), and the share of characters outside the
//!   ordinary printed repertoire. These are reported per document so that a
//!   residual-artefact rate can be matched on the generated side; a gate on
//!   them is a quality screen, never a model or detector score.
use serde_json::{Value, json};
use std::collections::HashSet;

pub const NORMALISATION_METHOD: &str = "ocr_line_unwrap_dehyphenate_v1";
pub const QUALITY_METHOD: &str = "ocr_quality_v1";

/// Lowercase ASCII word list.
pub struct Lexicon {
    words: HashSet<String>,
    pub sha256: String,
}

impl Lexicon {
    pub fn parse(text: &str) -> Self {
        let words = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_owned).collect();
        Lexicon { words, sha256: crate::text::sha_hex(text.as_bytes()) }
    }
    pub fn from_words<I: IntoIterator<Item = S>, S: Into<String>>(it: I) -> Self {
        Lexicon { words: it.into_iter().map(Into::into).collect(), sha256: String::new() }
    }
    pub fn len(&self) -> usize {
        self.words.len()
    }
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }
    /// Case-folded lookup of an alphabetic word (internal apostrophes kept).
    pub fn contains(&self, w: &str) -> bool {
        self.words.contains(&w.to_ascii_lowercase())
    }
}

/// Lexicon tokens of clean text: maximal runs of ASCII letters with
/// internal apostrophes, lowercased.
pub fn lexicon_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let s = s.replace('\u{2019}', "'");
    for raw in s.split(|c: char| !(c.is_ascii_alphabetic() || c == '\'')) {
        let t = raw.trim_matches('\'');
        if !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphabetic() || b == b'\'') {
            out.push(t.to_ascii_lowercase());
        }
    }
    out
}

/// Per-document counts of normalisation edits.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Normalisation {
    pub line_unwraps: u64,
    pub hyphen_joins_dropped: u64,
    pub hyphen_joins_kept: u64,
    pub char_maps: u64,
    pub soft_hyphens_removed: u64,
    pub block_continuations_joined: u64,
}

impl Normalisation {
    pub fn add(&mut self, o: &Normalisation) {
        self.line_unwraps += o.line_unwraps;
        self.hyphen_joins_dropped += o.hyphen_joins_dropped;
        self.hyphen_joins_kept += o.hyphen_joins_kept;
        self.char_maps += o.char_maps;
        self.soft_hyphens_removed += o.soft_hyphens_removed;
        self.block_continuations_joined += o.block_continuations_joined;
    }
    pub fn to_value(&self) -> Value {
        json!({"method": NORMALISATION_METHOD, "line_unwraps": self.line_unwraps, "hyphen_joins_dropped": self.hyphen_joins_dropped,
               "hyphen_joins_kept": self.hyphen_joins_kept, "char_maps": self.char_maps, "soft_hyphens_removed": self.soft_hyphens_removed,
               "block_continuations_joined": self.block_continuations_joined})
    }
}

/// Map ligatures and the long s; remove soft hyphens.
pub fn map_chars(s: &str, n: &mut Normalisation) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{017F}' => { out.push('s'); n.char_maps += 1; }
            '\u{FB00}' => { out.push_str("ff"); n.char_maps += 1; }
            '\u{FB01}' => { out.push_str("fi"); n.char_maps += 1; }
            '\u{FB02}' => { out.push_str("fl"); n.char_maps += 1; }
            '\u{FB03}' => { out.push_str("ffi"); n.char_maps += 1; }
            '\u{FB04}' => { out.push_str("ffl"); n.char_maps += 1; }
            '\u{FB05}' | '\u{FB06}' => { out.push_str("st"); n.char_maps += 1; }
            '\u{00AD}' => n.soft_hyphens_removed += 1,
            _ => out.push(c),
        }
    }
    out
}

fn trailing_word(s: &str) -> &str {
    let i = s.char_indices().rev().find(|(_, c)| !c.is_ascii_alphabetic()).map_or(0, |(i, c)| i + c.len_utf8());
    &s[i..]
}

fn leading_word(s: &str) -> &str {
    let i = s.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(s.len());
    &s[..i]
}

/// Join the lines of one paragraph block into a single line (see the module
/// rules). Blank lines must already have been used to split blocks.
pub fn unwrap_lines(lines: &[&str], lex: &Lexicon, n: &mut Normalisation) -> String {
    let mut out = String::new();
    for raw in lines {
        let line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            continue;
        }
        if out.is_empty() {
            out = line;
            continue;
        }
        let hyphen = out.ends_with('-') && !out.ends_with("--") && out[..out.len() - 1].chars().last().is_some_and(char::is_alphabetic);
        let next_lower = line.chars().next().is_some_and(|c| c.is_ascii_lowercase());
        if hyphen && next_lower {
            let head = trailing_word(&out[..out.len() - 1]).to_owned();
            let tail = leading_word(&line);
            let joined = format!("{head}{tail}");
            let keep = !head.is_empty() && !tail.is_empty() && lex.contains(&head) && lex.contains(tail) && !lex.contains(&joined);
            if keep {
                n.hyphen_joins_kept += 1;
            } else {
                out.pop();
                n.hyphen_joins_dropped += 1;
            }
            out.push_str(&line);
        } else {
            n.line_unwraps += 1;
            out.push(' ');
            out.push_str(&line);
        }
    }
    out
}

/// A block that starts with a lowercase letter continues the previous
/// block when that block does not end a sentence (a column, box or page
/// break inside a sentence): the two are joined with the line rules.
pub fn join_continuations(blocks: Vec<String>, lex: &Lexicon, n: &mut Normalisation) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for b in blocks {
        let cont = b.chars().next().is_some_and(char::is_lowercase) && out.last().is_some_and(|p| !crate::text::ends_sentence(p) && !p.contains(crate::text::GAP));
        if cont {
            let last = out.last_mut().expect("previous block");
            *last = unwrap_lines(&[last.as_str(), b.as_str()], lex, n);
            n.block_continuations_joined += 1;
        } else {
            out.push(b);
        }
    }
    out
}

/// Shared prose screen plus: a block that still starts with a lowercase
/// letter begins mid-sentence and is never the start of an excerpt.
pub fn classify_block(t: String, screen: crate::text::Screen) -> crate::text::Para {
    if t.chars().next().is_some_and(char::is_lowercase) {
        return crate::text::Para::non_prose(t, "starts_mid_sentence");
    }
    crate::text::Para::classify(t, screen)
}

/// Split text into blocks at blank lines (a line with only whitespace).
pub fn blocks(s: &str) -> Vec<Vec<&str>> {
    let mut out: Vec<Vec<&str>> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for l in s.split('\n') {
        let l = l.trim_end_matches('\r');
        if l.trim().is_empty() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(l);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Per-document OCR quality measures.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Quality {
    pub tokens: u64,
    pub alpha_tokens: u64,
    pub alpha_in_lexicon: u64,
    pub lower_tokens: u64,
    pub lower_in_lexicon: u64,
    pub odd_tokens: u64,
    pub chars: u64,
    pub odd_chars: u64,
}

fn ordinary_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || c.is_whitespace()
        || ".,;:!?'\"()-&$%/".contains(c)
        || matches!(c, '\u{2018}' | '\u{2019}' | '\u{201C}' | '\u{201D}' | '\u{2013}' | '\u{2014}' | '\u{00A3}' | '\u{00BD}' | '\u{00BC}' | '\u{00BE}')
}

fn ordinal(core: &str) -> bool {
    let d = core.bytes().take_while(u8::is_ascii_digit).count();
    d > 0 && matches!(core[d..].to_ascii_lowercase().as_str(), "st" | "nd" | "rd" | "th" | "d" | "s")
}

impl Quality {
    pub fn measure(s: &str, lex: &Lexicon) -> Self {
        let mut q = Quality::default();
        for c in s.chars() {
            if !c.is_whitespace() {
                q.chars += 1;
                if !ordinary_char(c) {
                    q.odd_chars += 1;
                }
            }
        }
        for tok in s.split_whitespace() {
            q.tokens += 1;
            let core = tok.trim_matches(|c: char| !c.is_alphanumeric());
            if core.is_empty() {
                if !tok.chars().all(|c| matches!(c, '-' | '\u{2013}' | '\u{2014}' | '&' | '$' | '%' | '/')) {
                    q.odd_tokens += 1;
                }
                continue;
            }
            let letters = core.chars().any(char::is_alphabetic);
            let digits = core.chars().any(|c| c.is_ascii_digit());
            if letters && digits {
                if !ordinal(core) {
                    q.odd_tokens += 1;
                }
                continue;
            }
            if !letters {
                continue;
            }
            for part in core.split(['-', '\u{2013}', '\u{2014}']).filter(|p| !p.is_empty()) {
                let w = part.trim_end_matches("'s").trim_end_matches("\u{2019}s");
                if !w.chars().all(|c| c.is_ascii_alphabetic() || c == '\'' || c == '\u{2019}') {
                    q.alpha_tokens += 1;
                    q.odd_tokens += 1;
                    continue;
                }
                let w = w.replace('\u{2019}', "'");
                let hit = lex.contains(&w) || lex.contains(w.trim_matches('\''));
                q.alpha_tokens += 1;
                q.alpha_in_lexicon += hit as u64;
                if w.chars().next().is_some_and(|c| c.is_ascii_lowercase()) {
                    q.lower_tokens += 1;
                    q.lower_in_lexicon += hit as u64;
                }
            }
        }
        q
    }
    fn r(a: u64, b: u64) -> f64 {
        if b == 0 { 0.0 } else { a as f64 / b as f64 }
    }
    pub fn lexicon_rate(&self) -> f64 {
        Self::r(self.alpha_in_lexicon, self.alpha_tokens)
    }
    pub fn lower_lexicon_rate(&self) -> f64 {
        Self::r(self.lower_in_lexicon, self.lower_tokens)
    }
    pub fn odd_token_rate(&self) -> f64 {
        Self::r(self.odd_tokens, self.tokens)
    }
    pub fn odd_char_rate(&self) -> f64 {
        Self::r(self.odd_chars, self.chars)
    }
    pub fn to_value(&self) -> Value {
        json!({"method": QUALITY_METHOD, "tokens": self.tokens, "alpha_tokens": self.alpha_tokens, "lexicon_rate": round4(self.lexicon_rate()),
               "lowercase_lexicon_rate": round4(self.lower_lexicon_rate()), "odd_token_rate": round4(self.odd_token_rate()),
               "odd_char_rate": round4(self.odd_char_rate()), "out_of_lexicon_alpha_tokens": self.alpha_tokens - self.alpha_in_lexicon,
               "odd_tokens": self.odd_tokens, "odd_chars": self.odd_chars})
    }
}

pub fn round4(x: f64) -> f64 {
    (x * 10000.0).round() / 10000.0
}

/// Quality gate thresholds (recorded in every report).
#[derive(Clone, Copy, Debug)]
pub struct Gate {
    pub min_lexicon_rate: f64,
    pub min_lowercase_lexicon_rate: f64,
    pub max_odd_token_rate: f64,
    pub max_odd_char_rate: f64,
}

impl Gate {
    pub const NEWSPAPER: Gate = Gate { min_lexicon_rate: 0.90, min_lowercase_lexicon_rate: 0.95, max_odd_token_rate: 0.02, max_odd_char_rate: 0.005 };
    pub fn check(&self, q: &Quality) -> Option<&'static str> {
        if q.lexicon_rate() < self.min_lexicon_rate {
            Some("ocr_lexicon_rate_below_gate")
        } else if q.lower_lexicon_rate() < self.min_lowercase_lexicon_rate {
            Some("ocr_lowercase_lexicon_rate_below_gate")
        } else if q.odd_token_rate() > self.max_odd_token_rate {
            Some("ocr_odd_token_rate_above_gate")
        } else if q.odd_char_rate() > self.max_odd_char_rate {
            Some("ocr_odd_char_rate_above_gate")
        } else {
            None
        }
    }
    pub fn to_value(&self) -> Value {
        json!({"min_lexicon_rate": self.min_lexicon_rate, "min_lowercase_lexicon_rate": self.min_lowercase_lexicon_rate,
               "max_odd_token_rate": self.max_odd_token_rate, "max_odd_char_rate": self.max_odd_char_rate})
    }
}

/// Fixed-width histogram of a rate in [0, 1] (20 bins), for reports.
#[derive(Clone, Debug, Default)]
pub struct Hist(pub [u64; 21]);

impl Hist {
    pub fn add(&mut self, x: f64) {
        let i = (x.clamp(0.0, 1.0) * 20.0).floor() as usize;
        self.0[i.min(20)] += 1;
    }
    pub fn to_value(&self) -> Value {
        let m: serde_json::Map<String, Value> = self.0.iter().enumerate().filter(|(_, n)| **n > 0).map(|(i, n)| (format!("{:.2}", i as f64 / 20.0), json!(n))).collect();
        Value::Object(m)
    }
}

/// Distributions of the quality measures over the documents of a run.
#[derive(Clone, Debug, Default)]
pub struct QualityStats {
    pub lexicon: Hist,
    pub lowercase_lexicon: Hist,
    pub odd_token: Hist,
    pub documents: u64,
    pub sum: Quality,
}

impl QualityStats {
    pub fn add(&mut self, q: &Quality) {
        self.documents += 1;
        self.lexicon.add(q.lexicon_rate());
        self.lowercase_lexicon.add(q.lower_lexicon_rate());
        self.odd_token.add((q.odd_token_rate() * 10.0).min(1.0));
        let s = &mut self.sum;
        s.tokens += q.tokens;
        s.alpha_tokens += q.alpha_tokens;
        s.alpha_in_lexicon += q.alpha_in_lexicon;
        s.lower_tokens += q.lower_tokens;
        s.lower_in_lexicon += q.lower_in_lexicon;
        s.odd_tokens += q.odd_tokens;
        s.chars += q.chars;
        s.odd_chars += q.odd_chars;
    }
    pub fn to_value(&self) -> Value {
        json!({"documents": self.documents, "pooled": self.sum.to_value(), "lexicon_rate_hist": self.lexicon.to_value(),
               "lowercase_lexicon_rate_hist": self.lowercase_lexicon.to_value(),
               "odd_token_rate_hist_x10": self.odd_token.to_value(),
               "hist_note": "bin lower edges of width 0.05; odd_token_rate is multiplied by 10 before binning"})
    }
}

/// Build a lexicon from the `text` field of JSONL files: tokens (see
/// `lexicon_tokens`) seen at least `min_count` times in at least `min_docs`
/// documents. Writes `lexicon.txt` (sorted, one per line) and `report.json`.
pub fn build_lexicon(inputs: &[std::path::PathBuf], min_count: u64, min_docs: u64, out: &std::path::Path) -> anyhow::Result<Value> {
    use std::collections::HashMap;
    crate::io::fresh_dir(out)?;
    let mut counts: HashMap<String, (u64, u64)> = HashMap::new();
    let mut files = Vec::new();
    let mut docs = 0u64;
    for p in inputs {
        let (sha, bytes) = crate::io::file_sha(p)?;
        let mut n = 0u64;
        for row in crate::io::jsonl_rows(p)? {
            let row = row?;
            let Some(t) = row["text"].as_str() else { continue };
            n += 1;
            let mut seen: HashSet<String> = HashSet::new();
            for w in lexicon_tokens(t) {
                let e = counts.entry(w.clone()).or_default();
                e.0 += 1;
                if seen.insert(w) {
                    e.1 += 1;
                }
            }
        }
        docs += n;
        files.push(json!({"path": p, "sha256": sha, "bytes": bytes, "documents": n}));
    }
    let mut words: Vec<&String> = counts.iter().filter(|(_, (c, d))| *c >= min_count && *d >= min_docs).map(|(w, _)| w).collect();
    words.sort();
    let mut body = String::new();
    for w in &words {
        body.push_str(w);
        body.push('\n');
    }
    std::fs::write(out.join("lexicon.txt"), &body)?;
    let report = json!({"schema": "slop_ninja_a_pool_ocr_lexicon_v1", "inputs": files, "documents": docs, "distinct_tokens": counts.len(),
                        "lexicon_words": words.len(), "min_count": min_count, "min_docs": min_docs, "lexicon_sha256": crate::text::sha_hex(body.as_bytes()),
                        "token_rule": "maximal runs of ASCII letters and internal apostrophes (U+2019 mapped to '), lowercased",
                        "use": "OCR quality measurement and line-end hyphen decisions only; never edits a word", "finished_utc": crate::io::now_utc()});
    crate::io::write_json(&out.join("report.json"), &report)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lex() -> Lexicon {
        Lexicon::from_words(["the", "report", "reported", "was", "well", "known", "man", "a", "of", "in", "he", "said", "to", "it"])
    }
    #[test]
    fn unwrap_and_dehyphenate() {
        let l = lex();
        let mut n = Normalisation::default();
        let s = unwrap_lines(&["The death was re-", "ported   yesterday by a well-", "known man."], &l, &mut n);
        assert_eq!(s, "The death was reported yesterday by a well-known man.");
        assert_eq!((n.hyphen_joins_dropped, n.hyphen_joins_kept, n.line_unwraps), (1, 1, 0));
        let mut n = Normalisation::default();
        assert_eq!(unwrap_lines(&["It was in Balti-", "more that he said so"], &l, &mut n), "It was in Baltimore that he said so");
        assert_eq!(n.hyphen_joins_dropped, 1);
        let mut n = Normalisation::default();
        assert_eq!(unwrap_lines(&["He said --", "nothing at all.", "Then left."], &l, &mut n), "He said -- nothing at all. Then left.");
        assert_eq!(n.line_unwraps, 2);
        let mut n = Normalisation::default();
        assert_eq!(unwrap_lines(&["a caf\u{e9}-", "s and r\u{e9}sum\u{e9}-", "ed"], &l, &mut n), "a caf\u{e9}s and r\u{e9}sum\u{e9}ed");
        let mut n = Normalisation::default();
        assert_eq!(map_chars("con\u{017F}equence \u{FB01}ne co\u{00AD}operate", &mut n), "consequence fine cooperate");
        assert_eq!((n.char_maps, n.soft_hyphens_removed), (2, 1));
    }
    #[test]
    fn blocks_split_on_blank_lines() {
        let b = blocks("one\r\ntwo\r\n\r\n  \nthree\n\n\nfour\n");
        assert_eq!(b, vec![vec!["one", "two"], vec!["three"], vec!["four"]]);
        let l = lex();
        let mut n = Normalisation::default();
        let j = join_continuations(vec!["He said that the".into(), "man was well.".into(), "it is new.".into()], &l, &mut n);
        assert_eq!(j, vec!["He said that the man was well.".to_string(), "it is new.".to_string()]);
        assert_eq!(n.block_continuations_joined, 1);
        assert_eq!(classify_block("it is new and it is a long enough sentence.".into(), crate::text::Screen::XML).reason, Some("starts_mid_sentence"));
    }
    #[test]
    fn quality_measures() {
        let l = lex();
        let q = Quality::measure("The report was well-known; he said it 4th of lS1 ~~ Smith.", &l);
        // alpha parts: The report was well known he said it of Smith -> 10, 9 in lexicon (Smith not)
        assert_eq!(q.alpha_tokens, 10);
        assert_eq!(q.alpha_in_lexicon, 9);
        assert_eq!(q.odd_tokens, 2, "lS1 and ~~ are odd; 4th is an ordinal");
        assert!(q.odd_char_rate() > 0.0);
        assert!(Gate::NEWSPAPER.check(&q).is_some());
        let clean = Quality::measure("The man said it was well known in the report.", &l);
        assert_eq!(clean.lexicon_rate(), 1.0);
        assert_eq!(Gate::NEWSPAPER.check(&clean), None);
        assert_eq!(lexicon_tokens("Don't stop, O'Brien\u{2019}s men-of 1900 war!"), vec!["don't", "stop", "o'brien's", "men", "of", "war"]);
    }
}
