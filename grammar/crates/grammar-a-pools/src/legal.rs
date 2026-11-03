//! Shared screens for the legal and government sources (court opinions,
//! hearings, presidential documents, FOMC transcripts).
//!
//! * **OCR artefacts.** Scanned sources carry recognition noise that would
//!   make "human" trivially separable from model text. `ocr_stats` counts
//!   hard glyph artefacts (geometric shapes, block elements, broken bars,
//!   not-signs) and soft token artefacts (letters glued to digits outside
//!   ordinal and reporter-series shapes, a word-final full stop followed by
//!   a lowercase word, a line-break hyphen left before a lowercase word,
//!   isolated lowercase letters). Adapters record both per document and
//!   reject any hard artefact or a soft rate above `MAX_SOFT_PER_100`.
//!   Prose is never repaired.
//! * **Transcription markup.** Audience-reaction markers (`[Laughter]`,
//!   `[Applause]`, ...) are removed and counted; any other bracketed
//!   annotation (`[inaudible]`, `[The information follows:]`, ...) makes its
//!   paragraph a non-prose break, so no gap or editorial insertion is ever
//!   admitted.
//! * **Judges and speakers.** `judge_surname` reduces a printed opinion
//!   author line to one surname or refuses (per curiam, several authors,
//!   unparsed); `speaker_surname` does the same for transcript labels.
use crate::text::{self, Para, Screen};
use serde_json::{Value, json};

/// Soft OCR artefacts per 100 whitespace words above which a document is
/// rejected.
pub const MAX_SOFT_PER_100: f64 = 1.0;
pub const OCR_SCREEN_METHOD: &str = "slop_ninja_ocr_artifact_screen_v1;hard_glyphs_reject;soft_tokens_max_1_per_100_words";

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OcrStats {
    pub words: usize,
    pub hard: usize,
    pub glued_alnum: usize,
    pub stop_lower: usize,
    pub break_hyphen: usize,
    pub lone_letter: usize,
}

impl OcrStats {
    pub fn soft(&self) -> usize {
        self.glued_alnum + self.stop_lower + self.break_hyphen + self.lone_letter
    }
    pub fn soft_per_100(&self) -> f64 {
        if self.words == 0 { 0.0 } else { self.soft() as f64 * 100.0 / self.words as f64 }
    }
    /// Rejection reason under the screen, if any.
    pub fn reason(&self) -> Option<&'static str> {
        if self.hard > 0 {
            Some("ocr_glyph_artifact")
        } else if self.soft_per_100() > MAX_SOFT_PER_100 {
            Some("ocr_soft_artifact_rate_above_1_per_100_words")
        } else {
            None
        }
    }
    pub fn to_value(&self) -> Value {
        json!({"method": OCR_SCREEN_METHOD, "words": self.words, "hard_glyphs": self.hard,
               "soft": {"glued_letter_digit": self.glued_alnum, "full_stop_then_lowercase": self.stop_lower,
                        "line_break_hyphen": self.break_hyphen, "isolated_lowercase_letter": self.lone_letter},
               "soft_per_100_words": (self.soft_per_100() * 1000.0).round() / 1000.0})
    }
}

fn hard_glyph(c: char) -> bool {
    let u = c as u32;
    (0x25A0..=0x25FF).contains(&u) || (0x2580..=0x259F).contains(&u) || matches!(c, '¦' | '¬' | '¤' | '|' | '\u{FFFD}')
}

/// Abbreviations that legitimately end in a full stop before a lowercase
/// word in legal and official prose.
const ABBREVIATIONS: &[&str] = &[
    "v", "vs", "mr", "mrs", "ms", "dr", "no", "nos", "co", "inc", "corp", "ltd", "jr", "sr", "st", "id", "ibid", "cf", "eg", "ie",
    "etc", "al", "seq", "ann", "stat", "stats", "gen", "rev", "app", "ct", "supp", "ch", "sec", "secs", "art", "arts", "p", "pp",
    "vol", "cl", "para", "par", "subd", "div", "dept", "gov", "govt", "admin", "assn", "ass'n", "bros", "cir", "dist", "fed",
    "reg", "regs", "civ", "crim", "proc", "evid", "const", "amend", "rep", "sen", "hon", "esq", "approx", "e.g", "i.e", "viz",
    "u.s", "u.s.c", "et", "seq", "ed", "eds", "pt", "pts", "fig", "ex", "exs", "tr", "op", "ops", "atty", "attys", "msp",
    "a.m", "p.m", "am", "pm", "jan", "feb", "mar", "apr", "jun", "jul", "aug", "sept", "sep", "oct", "nov", "dec", "approx",
    "cong", "sess", "res", "h.r", "s", "r", "j", "c.j", "l", "n", "c", "e", "w", "ss", "sub", "subsec", "cl", "pl",
];

fn letters_digits(t: &str) -> (bool, bool) {
    (t.chars().any(char::is_alphabetic), t.chars().any(|c| c.is_ascii_digit()))
}

/// Ordinals (`1st`, `2d`, `21st`), reporter series (`F.2d`, `So.3d`,
/// `N.E.2d`, `A2d`), statute subsections (`12b`, `401k`), decades
/// (`1990s`) and hyphenated codes (`W-2`, `I-95`) are ordinary.
fn ordinary_alnum(t: &str) -> bool {
    let lower = t.to_lowercase();
    let b = lower.as_bytes();
    let digits_then = |s: &[u8]| {
        let d = s.iter().take_while(|c| c.is_ascii_digit()).count();
        d > 0 && s[d..].len() <= 2 && s[d..].iter().all(|c| c.is_ascii_lowercase())
    };
    if digits_then(b) {
        return true;
    }
    if t.contains('-') || t.contains('/') || t.contains('§') || t.contains('(') {
        return true;
    }
    // Letters (with dots) then digits then up to two letters: F.2d, A.3d, N.E.2d, So.2d.
    let first_digit = b.iter().position(|c| c.is_ascii_digit()).unwrap_or(0);
    let head = &b[..first_digit];
    if !head.is_empty() && head.iter().all(|c| c.is_ascii_lowercase() || *c == b'.') && digits_then(&b[first_digit..]) {
        return true;
    }
    false
}

fn core(tok: &str) -> &str {
    tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '-' && c != '§' && c != '\'')
}

pub fn ocr_stats(s: &str) -> OcrStats {
    let toks: Vec<&str> = s.split_whitespace().collect();
    let mut st = OcrStats { words: toks.len(), ..Default::default() };
    st.hard = s.chars().filter(|c| hard_glyph(*c)).count();
    for (i, tok) in toks.iter().enumerate() {
        let next_lower = toks.get(i + 1).and_then(|n| n.chars().next()).is_some_and(|c| c.is_lowercase());
        let c = core(tok);
        let (l, d) = letters_digits(c);
        if l && d && !ordinary_alnum(c.trim_end_matches('.')) {
            st.glued_alnum += 1;
        }
        if next_lower && tok.ends_with('.') && !tok.ends_with("..") {
            let w = tok.trim_end_matches('.').trim_start_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
            let is_word = w.len() >= 2 && w.chars().all(|c| c.is_ascii_lowercase());
            if is_word && !ABBREVIATIONS.contains(&w.as_str()) {
                st.stop_lower += 1;
            }
        }
        if next_lower && tok.len() > 2 && tok.ends_with('-') && !tok.ends_with("--") && tok.chars().rev().nth(1).is_some_and(char::is_alphabetic) {
            st.break_hyphen += 1;
        }
        if tok.len() == 1 && tok.chars().all(|c| c.is_ascii_lowercase()) && *tok != "a" {
            st.lone_letter += 1;
        }
    }
    st
}

/// Audience-reaction markers removed from transcripts (lowercased inner text).
pub const REACTION_MARKERS: &[&str] = &[
    "laughter", "applause", "laughs", "laughing", "cheers", "cheering", "boos", "booing", "applause.", "laughter.",
];
pub const MARKUP_METHOD: &str = "slop_ninja_transcript_markup_v1;reaction_markers_removed_and_counted;other_bracketed_annotations_break_paragraph";

/// Remove audience-reaction markers. Returns the cleaned paragraph, the
/// number removed, and whether any other bracketed annotation remains.
pub fn strip_reactions(p: &str) -> (String, usize, bool) {
    let mut out = String::with_capacity(p.len());
    let mut removed = 0usize;
    let mut other = false;
    let mut rest = p;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(j) = after.find(']') else {
            other = true;
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let inner = after[..j].trim().to_lowercase();
        let inner = inner.trim_end_matches('.');
        if inner.split(|c: char| c == ' ' || c == ',' || c == '/').filter(|w| !w.is_empty()).all(|w| REACTION_MARKERS.contains(&w) || w == "and") && !inner.is_empty() {
            removed += 1;
        } else {
            other = true;
            out.push('[');
            out.push_str(&after[..=j]);
        }
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    (text::normalized(&out), removed, other || out.contains(']'))
}

/// Classify one transcript paragraph after reaction removal.
pub fn transcript_para(raw: &str, removed: &mut usize) -> Para {
    let (t, n, other) = strip_reactions(raw);
    *removed += n;
    if t.is_empty() {
        return Para::non_prose(String::new(), "reaction_marker_only");
    }
    if other {
        return Para::non_prose(t, "transcription_bracket_annotation");
    }
    Para::classify(t, Screen::XML)
}

/// A paragraph that is a footnote body or a page/star-paging marker.
pub fn footnote_like(p: &str) -> bool {
    let t = p.trim_start();
    let b = t.as_bytes();
    let d = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if (1..=3).contains(&d) && b.get(d).is_some_and(|c| *c == b' ' || *c == b'.' || *c == b')') && !t[d..].trim_start_matches(['.', ')']).trim_start().starts_with(|c: char| c.is_ascii_digit()) {
        // "1 See ...", "12. The ..." at paragraph start: footnote bodies or numbered points.
        return true;
    }
    t.starts_with('*') || t.starts_with('†') || t.starts_with("[*") || t.starts_with("[fn") || t.starts_with("FN")
}

const NAME_STOP: &[&str] = &[
    "mr", "mrs", "ms", "miss", "dr", "hon", "justice", "justices", "chief", "judge", "judges", "j", "c", "p", "cj", "pj", "ch", "jj",
    "presiding", "associate", "assoc", "delivered", "the", "opinion", "opinions", "of", "court", "dissenting", "dissents", "dissent",
    "concurring", "concurs", "concur", "concurrence", "in", "part", "judgment", "result", "results", "specially", "for", "special",
    "by", "commissioner", "vice", "chancellor", "acting", "senior", "circuit", "district", "magistrate", "retired", "sitting",
    "designation", "speaking", "writing", "majority", "filed", "with", "whom", "joins", "join", "joined", "announced", "an",
    "a", "chairman", "chairwoman", "member", "referee", "auditor", "master", "surrogate", "president", "governor", "officer",
    "reporter", "per", "curiam", "jr", "sr", "ii", "iii", "iv", "esq", "s", "on", "rehearing", "motion", "denial", "order",
    "statement", "memorandum", "decision", "case", "this", "is", "was", "who", "as", "to", "it", "said", "after", "stating",
    "facts", "above", "foregoing", "rendered", "following",
];
/// Words that mean the author line names no single judge.
const NO_SINGLE_AUTHOR: &[&str] = &["curiam", "and", "&", "jj", "justices", "judges", "court", "unanimous", "all", "en", "banc"];

/// One surname from a printed opinion author line, or `None` (per curiam,
/// several authors, unparsed).
pub fn judge_surname(author: &str) -> Option<String> {
    let lower = author.to_lowercase();
    let toks = text::folded_tokens(&lower.replace('\'', ""));
    if toks.is_empty() {
        return None;
    }
    let and_count = lower.split_whitespace().filter(|w| *w == "and" || *w == "&").count();
    if lower.contains("curiam") || lower.contains("justices") || lower.contains("judges") || and_count > 0 && !lower.contains("with whom") || lower.contains(" jj") {
        return None;
    }
    let _ = NO_SINGLE_AUTHOR;
    // Keep the head before "with whom", "dissenting" lists of others are not authors.
    let head = lower.split("with whom").next().unwrap_or(&lower);
    let toks: Vec<String> = text::folded_tokens(&head.replace('\'', "")).into_iter().filter(|t| !NAME_STOP.contains(&t.as_str()) && !t.chars().all(|c| c.is_ascii_digit())).collect();
    if toks.is_empty() || toks.len() > 3 {
        return None;
    }
    let s = toks.last()?.clone();
    (s.len() >= 2 && s.chars().all(|c| c.is_alphabetic())).then_some(s)
}

const SPEAKER_TITLES: &[&str] = &[
    "chairman", "chairwoman", "chair", "vice", "mr", "ms", "mrs", "miss", "dr", "president", "governor", "senator", "representative",
    "congressman", "congresswoman", "delegate", "commissioner", "secretary", "general", "admiral", "ambassador", "judge", "justice",
    "hon", "the", "honorable", "chief", "ranking", "member", "acting", "deputy", "assistant", "director", "mayor", "professor",
];

/// Surname from a transcript speaker label (`CHAIRMAN GREENSPAN`, `MR. KOHN`,
/// `Senator Dorgan`); `None` for unnamed labels (`The Chairman`, `Voice`).
pub fn speaker_surname(label: &str) -> Option<String> {
    let toks: Vec<String> = text::folded_tokens(&label.to_lowercase().replace('\'', ""))
        .into_iter()
        .filter(|t| !SPEAKER_TITLES.contains(&t.as_str()))
        .collect();
    if toks.is_empty() || toks.len() > 3 {
        return None;
    }
    let s = toks.last()?.clone();
    (s.len() >= 2 && s.chars().all(|c| c.is_alphabetic())).then_some(s)
}

/// Rights block for US public-domain government works and judicial edicts.
pub fn public_domain_rights(license: &str, attribution: String, evidence_url: &str, notes: &str, cohort: &str) -> Value {
    let mut r = crate::record::rights(license, attribution, evidence_url, notes);
    r["admission_decision"] = json!("2026-10-27: US public-domain government work or judicial opinion; the classifier-training basis of 2026-10-27 is recorded as well; exact text kept in ignored storage");
    r["basis"] = json!("public domain (17 U.S.C. 105 or the government edicts doctrine); classifier-training use, user decision 2026-10-27");
    r["cohort_id"] = json!(cohort);
    r["removable_cohort"] = json!(true);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocr_screen_flags_glyphs_and_glued_tokens_but_not_citations() {
        let clean = "The court in Smith v. Jones, 123 F.2d 456 (2d Cir. 1941), held that the 1st and 21st claims under 42 U.S.C. § 1983 and Form W-2 failed in the 1990s.";
        let s = ocr_stats(clean);
        assert_eq!(s.hard, 0);
        assert_eq!(s.soft(), 0, "{s:?}");
        let noisy = "The defendant. entered upon the land \u{25a0} and the c1aim was t he same as. we said.";
        let n = ocr_stats(noisy);
        assert_eq!(n.hard, 1);
        assert_eq!(n.glued_alnum, 1);
        assert_eq!(n.stop_lower, 2);
        assert_eq!(n.lone_letter, 1);
        assert_eq!(n.reason(), Some("ocr_glyph_artifact"));
        let soft = ocr_stats(&format!("{} pros- ecution", vec!["word"; 50].join(" ")));
        assert_eq!(soft.break_hyphen, 1);
        assert!(soft.reason().is_some());
        assert!(ocr_stats(&format!("{} pros- ecution", vec!["word"; 150].join(" "))).reason().is_none());
    }

    #[test]
    fn reaction_markers_are_removed_and_other_brackets_break() {
        let (t, n, other) = strip_reactions("We will win this. [Laughter] And we will keep going. [Applause.]");
        assert_eq!(t, "We will win this. And we will keep going.");
        assert_eq!((n, other), (2, false));
        let (_, n, other) = strip_reactions("The rate was [inaudible] last year.");
        assert_eq!((n, other), (0, true));
        let mut removed = 0;
        assert_eq!(transcript_para("[Laughter]", &mut removed).reason, Some("reaction_marker_only"));
        assert_eq!(transcript_para("[The information follows:]", &mut removed).reason, Some("transcription_bracket_annotation"));
        assert_eq!(removed, 1);
    }

    #[test]
    fn judge_and_speaker_names() {
        assert_eq!(judge_surname("McCLELLAN, J.").as_deref(), Some("mcclellan"));
        assert_eq!(judge_surname("Mr. Justice Holmes delivered the opinion of the court.").as_deref(), Some("holmes"));
        assert_eq!(judge_surname("STONE, C. J.").as_deref(), Some("stone"));
        assert_eq!(judge_surname("Robert H. Jackson, Justice").as_deref(), Some("jackson"));
        assert_eq!(judge_surname("O'CONNOR, J., dissenting").as_deref(), Some("oconnor"));
        assert_eq!(judge_surname("Per Curiam."), None);
        assert_eq!(judge_surname("STONE, C. J., and BLACK, J."), None);
        assert_eq!(judge_surname(""), None);
        assert_eq!(judge_surname("BLACK, J., with whom DOUGLAS, J., joins, dissenting").as_deref(), Some("black"));
        assert_eq!(speaker_surname("CHAIRMAN GREENSPAN").as_deref(), Some("greenspan"));
        assert_eq!(speaker_surname("VICE CHAIRMAN McDONOUGH").as_deref(), Some("mcdonough"));
        assert_eq!(speaker_surname("The Chairman"), None);
        assert_eq!(speaker_surname("Senator Dorgan").as_deref(), Some("dorgan"));
    }

    #[test]
    fn footnotes_are_recognised() {
        assert!(footnote_like("1 See Smith v. Jones, 12 Ala. 4."));
        assert!(footnote_like("* Judge Roe sat by designation."));
        assert!(!footnote_like("The appellant argues that the court erred."));
        assert!(!footnote_like("1990 was the year the statute changed."));
    }
}
