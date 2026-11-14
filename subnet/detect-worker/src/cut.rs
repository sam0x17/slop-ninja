//! The free check's cut: which part of a sent text is scored.
//!
//! The free check scores at most the first [`FREE_MAX_WORDS`] whitespace
//! words of a text, and never stops inside a sentence. The detector reads a
//! text that stops mid-sentence as AI-like: cut at exactly word 1,000, joined
//! human test texts reached the strong band about half the time, against
//! about 1% for the same texts cut back to their last sentence end
//! (`docs/detect-evidence.md`, "Long texts"). So, in this order:
//!
//! - **C1 (the word cut).** A text of more than 1,000 words is scored from
//!   its first character to the last unit end at or before the start of word
//!   1,001, so the scored part holds at most 1,000 words. If the first 1,000
//!   words hold no unit end, the cut falls at the last line end there instead
//!   (C1b: chat logs and other text written one line per utterance).
//! - **C2 (the token cut).** If the part C1 leaves (the whole text when C1
//!   did not apply) has more than the scorer's [`MAX_TOKENS`] tokens with the
//!   frame, it is cut back to the last unit end inside them (the last line end
//!   if no unit end fits). At a unit end or a line end the next character is
//!   whitespace, where the tokenizer always splits, so a prefix's exact count
//!   grows with the prefix and a binary search over the candidates finds the
//!   last one that fits. A prefix longer than [`MAX_TEXT_BYTES`] cannot fit
//!   and is never tokenized.
//! - **C3 (never inside a sentence).** If C1 or C2 finds neither a unit end
//!   nor a line end, nothing is scored ([`NoCut::NoSentenceEnd`]). A cut that
//!   leaves fewer than 24 words gets the too-short answer, as any short text
//!   does: the length rule reads the scored part's own word count.
//!
//! Definitions (the rule the long-text measurement cut its texts with):
//! - a **word** is a maximal run of non-whitespace characters
//!   ([`crate::policy::words`]);
//! - a **sentence end** is ".", "?" or "!" followed by whitespace or the end
//!   of the text. `said."` has none between the full stop and the quote, and
//!   "…" is not one;
//! - a **paragraph break** is a line feed, then any spaces, tabs, carriage
//!   returns, vertical tabs or form feeds, then another line feed. Its unit
//!   end is the end of the last non-whitespace character before it;
//! - a **unit end** is a sentence end or a paragraph break's unit end;
//! - a **line end** is the end of the last non-whitespace character before a
//!   line feed. A single line feed is not a unit end: hard-wrapped text
//!   breaks lines inside sentences.
//!
//! The rule is a product rule (1,000 words is not a measured bound), and the
//! rates the page cites for its scored part were measured on texts cut by it.
//! Scanning allocates nothing; C2's counts allocate only through the caller's
//! `admit`.

use crate::bound::{FREE_MAX_WORDS, MAX_TEXT_BYTES, MAX_TOKENS};
use crate::policy::words;
use slop_ninja_scorer_tokens::{Admit, Refused};

/// How the scored part was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The whole text is scored.
    Whole,
    /// C1: the first 1,000 words, cut back to a unit end (or a line end).
    FirstWords,
    /// C2: cut back further to fit the scorer's tokens.
    TokenLimit,
}

impl Kind {
    pub fn code(self) -> &'static str {
        match self {
            Kind::Whole => "none",
            Kind::FirstWords => "first_1000_words",
            Kind::TokenLimit => "token_limit",
        }
    }

    pub fn from_code(s: &str) -> Option<Kind> {
        [Kind::Whole, Kind::FirstWords, Kind::TokenLimit].into_iter().find(|k| k.code() == s)
    }
}

/// The part of a sent text the free check scores.
#[derive(Debug, PartialEq)]
pub struct Cut<'a> {
    /// A prefix of the sent text: the whole text, or one ending at a unit end
    /// (or a line end) when cut.
    pub scored: &'a str,
    pub kind: Kind,
    /// Whitespace words of the whole sent text.
    pub total_words: usize,
    /// Whitespace words of the scored part.
    pub scored_words: usize,
    /// The scored part's exact token count, frame included (at most
    /// [`MAX_TOKENS`]).
    pub tokens: u64,
}

impl Cut<'_> {
    /// Where the scored part ends, in UTF-16 units of the sent text (the
    /// scored part is its prefix), so the page can grey the rest without
    /// counting words itself.
    pub fn scored_end(&self) -> usize {
        self.scored.encode_utf16().count()
    }
}

/// Why nothing is scored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoCut {
    /// C3: no unit end or line end where the cut must fall.
    NoSentenceEnd,
    /// The token count's scratch was refused (the request's reservation), or
    /// the table failed.
    Count(Refused),
}

/// Where a cut by rules C1 to C3 ends in `text`, how it was chosen and the
/// scored part's exact token count, frame included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct End {
    /// A byte offset: `text.len()`, or a unit end (or a line end).
    pub end: usize,
    pub kind: Kind,
    pub tokens: u64,
}

/// The free check's scored part of `text` (rules C1 to C3), with its exact
/// token count. `admit` is offered every allocation the counts make.
pub fn free_cut<'a>(text: &'a str, admit: Admit<'_>) -> Result<Cut<'a>, NoCut> {
    let End { end, kind, tokens } = cut_end(text, admit)?;
    let scored = text.get(..end).ok_or(NoCut::NoSentenceEnd)?;
    Ok(Cut { scored, kind, total_words: words(text), scored_words: words(scored), tokens })
}

/// Rules C1 to C3 on `text`, read no further than they need: the scan stops
/// at word 1,001 and the counts at the cut, so a paid check cuts each
/// section of a long document with the free check's own rule (section 1 is
/// exactly the free cut) without reading the rest of the document.
pub fn cut_end(text: &str, admit: Admit<'_>) -> Result<End, NoCut> {
    let (mut end, mut kind) = (text.len(), Kind::Whole);
    // The start of word 1,001, when the text has more than 1,000 words.
    if let Some(limit) = word_start(text, FREE_MAX_WORDS) {
        end = last_at_or_before(unit_ends(text), limit)
            .or_else(|| last_at_or_before(line_ends(text), limit))
            .ok_or(NoCut::NoSentenceEnd)?;
        kind = Kind::FirstWords;
    }
    let mut tokens = count_if_fits(text, end, admit)?;
    if tokens.is_none() {
        end = match last_fitting(text, unit_ends, end, admit)? {
            Some(e) => e,
            None => last_fitting(text, line_ends, end, admit)?.ok_or(NoCut::NoSentenceEnd)?,
        };
        kind = Kind::TokenLimit;
        tokens = count_if_fits(text, end, admit)?;
    }
    // The searched end fits by construction; a miss is still never scored.
    let tokens = tokens.ok_or(NoCut::NoSentenceEnd)?;
    Ok(End { end, kind, tokens })
}

/// Whether the whole of `text` fits one cut: at most 1,000 words and the
/// scorer's tokens. The word scan stops at word 1,001 and the count past
/// the limit.
pub fn fits_whole(text: &str, admit: Admit<'_>) -> Result<bool, NoCut> {
    if word_start(text, FREE_MAX_WORDS).is_some() {
        return Ok(false);
    }
    fits_at(text, text.len(), admit)
}

/// The exact count of `text[..end]` when it fits the scorer's tokens.
fn count_if_fits(text: &str, end: usize, admit: Admit<'_>) -> Result<Option<u64>, NoCut> {
    if end as u64 > MAX_TEXT_BYTES {
        return Ok(None);
    }
    let Some(prefix) = text.get(..end) else { return Ok(None) };
    let n = slop_ninja_scorer_tokens::count(prefix, admit).map_err(NoCut::Count)?;
    Ok((n <= MAX_TOKENS).then_some(n))
}

/// Whether `text[..end]` fits the scorer's tokens (the count stops past the
/// limit).
fn fits_at(text: &str, end: usize, admit: Admit<'_>) -> Result<bool, NoCut> {
    if end as u64 > MAX_TEXT_BYTES {
        return Ok(false);
    }
    let Some(prefix) = text.get(..end) else { return Ok(false) };
    slop_ninja_scorer_tokens::fits(prefix, MAX_TOKENS, admit).map_err(NoCut::Count)
}

/// The last candidate of `ends(text)` before `end` whose prefix fits, by
/// binary search over the candidates (a prefix's count grows with it). The
/// candidates are rescanned per probe rather than collected, so the search
/// allocates nothing.
fn last_fitting<'t, I: Iterator<Item = usize>>(
    text: &'t str,
    ends: fn(&'t str) -> I,
    end: usize,
    admit: Admit<'_>,
) -> Result<Option<usize>, NoCut> {
    let n = ends(text).take_while(|&e| e < end).count();
    let (mut lo, mut hi, mut best) = (0usize, n, None);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let Some(e) = ends(text).nth(mid) else { break };
        if fits_at(text, e, &mut *admit)? {
            best = Some(e);
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    Ok(best)
}

/// The last offset of `ends` at or before `limit`.
fn last_at_or_before(ends: impl Iterator<Item = usize>, limit: usize) -> Option<usize> {
    ends.take_while(|&e| e <= limit).last()
}

/// The byte offset where word `n + 1` starts (words counted from one), or
/// `None` when the text has at most `n` words.
pub fn word_start(text: &str, n: usize) -> Option<usize> {
    let (mut seen, mut prev_ws) = (0usize, true);
    for (i, c) in text.char_indices() {
        let ws = c.is_whitespace();
        if prev_ws && !ws {
            if seen == n {
                return Some(i);
            }
            seen += 1;
        }
        prev_ws = ws;
    }
    None
}

/// The spaces a paragraph break may hold between its two line feeds.
fn blank_line_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\u{b}' | '\u{c}')
}

/// The unit ends of `text`, ascending, each once.
pub fn unit_ends(text: &str) -> Ends<'_> {
    Ends { chars: text.char_indices().peekable(), last_nonws_end: 0, line_feed: false, last: 0, lines: false }
}

/// The line ends of `text`, ascending, each once.
pub fn line_ends(text: &str) -> Ends<'_> {
    Ends { chars: text.char_indices().peekable(), last_nonws_end: 0, line_feed: false, last: 0, lines: true }
}

/// A scan for unit ends or line ends, as byte offsets into the text.
pub struct Ends<'a> {
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    /// The end of the last non-whitespace character seen.
    last_nonws_end: usize,
    /// A line feed followed so far only by blank-line spaces.
    line_feed: bool,
    /// The last offset yielded.
    last: usize,
    /// Line ends instead of unit ends.
    lines: bool,
}

impl Iterator for Ends<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        while let Some((i, c)) = self.chars.next() {
            let found = if c == '\n' {
                let end = (self.lines || self.line_feed).then_some(self.last_nonws_end);
                self.line_feed = true;
                end
            } else if blank_line_space(c) {
                None
            } else {
                self.line_feed = false;
                if c.is_whitespace() {
                    None
                } else {
                    self.last_nonws_end = i + c.len_utf8();
                    let sentence = !self.lines
                        && matches!(c, '.' | '?' | '!')
                        && self.chars.peek().is_none_or(|&(_, n)| n.is_whitespace());
                    sentence.then_some(self.last_nonws_end)
                }
            };
            if let Some(e) = found.filter(|&e| e > self.last) {
                self.last = e;
                return Some(e);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admit_all() -> impl FnMut(usize, usize) -> bool {
        |_, _| true
    }

    fn cut(text: &str) -> Result<Cut<'_>, NoCut> {
        free_cut(text, &mut admit_all())
    }

    fn count(text: &str) -> u64 {
        slop_ninja_scorer_tokens::count(text, &mut admit_all()).unwrap()
    }

    /// `n` words of plain prose in sentences of `per` words, each ending
    /// with a full stop, numbered so every word is distinct.
    fn sentences(n: usize, per: usize) -> String {
        let mut s = String::new();
        for i in 0..n {
            if i > 0 {
                s.push(' ');
            }
            s.push_str(if i % 2 == 0 { "the" } else { "river" });
            if (i + 1) % per == 0 {
                s.push('.');
            }
        }
        s
    }

    #[test]
    fn up_to_1000_words_is_scored_whole() {
        for n in [24, 999, 1000] {
            let text = sentences(n, 10);
            let c = cut(&text).unwrap();
            assert_eq!((c.kind, c.scored, c.total_words, c.scored_words), (Kind::Whole, text.as_str(), n, n));
            assert_eq!(c.tokens, count(&text));
            assert_eq!(c.scored_end(), text.encode_utf16().count());
        }
        // Without any punctuation too: nothing to cut.
        let bare = vec!["word"; 1000].join(" ");
        assert_eq!(cut(&bare).unwrap().kind, Kind::Whole);
    }

    #[test]
    fn word_1001_cuts_at_a_sentence_end_on_word_1000() {
        // Sentences of 10 words: word 1,000 ends one, word 1,001 starts the next.
        let text = sentences(1001, 10);
        let c = cut(&text).unwrap();
        assert_eq!(c.kind, Kind::FirstWords);
        assert_eq!((c.total_words, c.scored_words), (1001, 1000));
        assert!(c.scored.ends_with("river."));
        assert_eq!(c.scored, sentences(1000, 10));
        assert_eq!(c.tokens, count(c.scored));
    }

    #[test]
    fn a_sentence_crossing_word_1000_is_left_out() {
        // Sentences of 7 words: 142 end by word 994, the 143rd runs 995-1,001.
        let text = sentences(1001, 7);
        let c = cut(&text).unwrap();
        assert_eq!((c.kind, c.scored_words), (Kind::FirstWords, 994));
        assert!(c.scored.ends_with('.'));
        // The same with a long document after it.
        let long = sentences(3214, 7);
        let c = cut(&long).unwrap();
        assert_eq!((c.total_words, c.scored_words), (3214, 994));
        assert_eq!(c.scored, sentences(994, 7));
    }

    /// `n` words of plain prose without punctuation, about one token a word.
    fn plain(n: usize) -> Vec<String> {
        const W: [&str; 10] = ["the", "river", "ran", "past", "old", "stone", "walls", "and", "into", "town"];
        (0..n).map(|i| W[i % W.len()].to_string()).collect()
    }

    /// 1,000 words whose last full stop is `back` words before word 1,000,
    /// then more words.
    fn last_end_back(back: usize) -> String {
        let mut w: Vec<String> = plain(1500);
        w[1000 - back - 1].push('.');
        w.join(" ")
    }

    #[test]
    fn the_last_unit_end_far_back() {
        for back in [1, 97, 300] {
            let text = last_end_back(back);
            let c = cut(&text).unwrap();
            assert_eq!(c.scored_words, 1000 - back, "back {back}");
            assert!(c.scored.ends_with('.'));
        }
        // The last unit end at word 1,000 itself.
        let text = last_end_back(0);
        assert_eq!(cut(&text).unwrap().scored_words, 1000);
    }

    #[test]
    fn sentence_end_marks() {
        // "?" and "!" end sentences; a closing quote after the stop and the
        // ellipsis character do not, nor does a stop inside a word.
        let ends = |s: &str| unit_ends(s).collect::<Vec<_>>();
        assert_eq!(ends("Why? Now! Done."), vec![4, 9, 15]);
        assert_eq!(ends(r#"He said." Then"#), Vec::<usize>::new());
        assert_eq!(ends("Wait… then 3.5 and e.g.x"), Vec::<usize>::new());
        assert_eq!(ends("Mr. Smith"), vec![3]);
        // `said."` at the cut point: the cut falls at the previous end.
        let mut w: Vec<String> = plain(1200);
        w[949].push('.');
        w[999] = "said.\"".into();
        let text = w.join(" ");
        assert_eq!(cut(&text).unwrap().scored_words, 950);
    }

    #[test]
    fn paragraph_breaks_and_line_feeds() {
        let ends = |s: &str| unit_ends(s).collect::<Vec<_>>();
        // A blank line, with CR LF and spaces between the line feeds: the end
        // of the last character before it.
        assert_eq!(ends("one two\r\n  \t\r\nthree"), vec![7]);
        assert_eq!(ends("one two  \n\n\nthree"), vec![7]);
        // A full stop then a blank line: one unit end.
        assert_eq!(ends("one.\n\ntwo"), vec![4]);
        // A single (hard-wrap) line feed is not a unit end, and a non-break
        // space between line feeds is not a blank line.
        assert_eq!(ends("one two\nthree four"), Vec::<usize>::new());
        assert_eq!(ends("one\n\u{a0}\ntwo"), Vec::<usize>::new());
        // A leading blank line has no character before it.
        assert_eq!(ends("\n\none"), Vec::<usize>::new());
        // Line ends: every line feed after some text.
        assert_eq!(line_ends("a b\nc \r\nd\n\n").collect::<Vec<_>>(), vec![3, 5, 9]);
        // At the cut: a hard-wrapped line feed near word 1,000 is not a cut
        // point; the paragraph break further back is.
        let mut w: Vec<String> = plain(1100);
        w[899] = "marker\n\n".into();
        w[998].push('\n');
        let text = w.join(" ");
        let c = cut(&text).unwrap();
        assert_eq!(c.scored_words, 900);
        assert!(c.scored.ends_with("marker"));
    }

    #[test]
    fn a_chat_log_is_cut_at_its_last_line_end() {
        // 1,500 words, one five-word utterance a line, no punctuation: C1b.
        let lines: Vec<String> = (0..300).map(|i| format!("nick{i} says hello there friend")).collect();
        let text = lines.join("\n");
        let c = cut(&text).unwrap();
        assert_eq!((c.kind, c.total_words, c.scored_words), (Kind::FirstWords, 1500, 1000));
        assert_eq!(c.scored, lines[..200].join("\n"));
        // A line crossing word 1,000 is left out.
        let lines: Vec<String> = (0..300).map(|i| format!("nick{i} says hello there my friend")).collect();
        let text = lines.join("\n");
        let c = cut(&text).unwrap();
        assert_eq!(c.scored_words, 996);
    }

    #[test]
    fn no_place_to_cut_scores_nothing() {
        // No unit end and no line feed in the first 1,000 words (C3).
        let text = vec!["word"; 1001].join(" ");
        assert_eq!(cut(&text), Err(NoCut::NoSentenceEnd));
        let late = format!("{} end. more", vec!["word"; 1200].join(" "));
        assert_eq!(cut(&late), Err(NoCut::NoSentenceEnd));
        // A unit end in the first words only: the cut leaves them, and the
        // length rule then refuses the text as too short.
        let early = format!("Hello there. {}", vec!["word"; 1200].join(" "));
        let c = cut(&early).unwrap();
        assert_eq!((c.scored, c.scored_words), ("Hello there.", 2));
        assert!(crate::policy::length_bin(c.scored_words).is_err());
    }

    /// Dense words: about one token per character.
    fn dense_sentences(n: usize, per: usize) -> String {
        let mut s = String::new();
        for i in 0..n {
            if i > 0 {
                s.push(' ');
            }
            s.push_str("q1w2e3r4");
            if (i + 1) % per == 0 {
                s.push('.');
            }
        }
        s
    }

    #[test]
    fn a_dense_text_is_cut_inside_the_token_limit() {
        // Under 1,000 words but over 2,048 tokens: C2 at the last sentence end
        // that fits, checked against the scorer's own count.
        let text = dense_sentences(600, 10);
        assert!(count(&text) > MAX_TOKENS, "{}", count(&text));
        let c = cut(&text).unwrap();
        assert_eq!((c.kind, c.total_words), (Kind::TokenLimit, 600));
        assert!(c.scored.ends_with('.') && c.scored_words.is_multiple_of(10));
        assert_eq!(c.tokens, count(c.scored));
        assert!(c.tokens <= MAX_TOKENS);
        // The next sentence end would not fit.
        let next = dense_sentences(c.scored_words + 10, 10);
        assert!(count(&next) > MAX_TOKENS);
        // Over 1,000 words and still too dense after C1: C2 wins.
        let long = dense_sentences(1500, 10);
        let c2 = cut(&long).unwrap();
        assert_eq!((c2.kind, c2.total_words, c2.scored), (Kind::TokenLimit, 1500, c.scored));
        // Dense with no unit end inside the limit: the last line end, else nothing.
        let lines: Vec<String> = (0..60).map(|_| ["q1w2e3r4"; 10].join(" ")).collect();
        let chat = lines.join("\n");
        let c3 = cut(&chat).unwrap();
        assert_eq!(c3.kind, Kind::TokenLimit);
        assert!(c3.tokens <= MAX_TOKENS && c3.scored.ends_with("r4"));
        assert_eq!(cut(&vec!["q1w2e3r4"; 600].join(" ")), Err(NoCut::NoSentenceEnd));
    }

    #[test]
    fn a_prefix_too_long_to_fit_is_never_tokenized() {
        // A word longer than any text that could fit: only the prefix before
        // it is a candidate, and the counts never see the long word.
        let giant = "a".repeat(MAX_TEXT_BYTES as usize + 1);
        let text = format!("Short sentence here. {giant}. More. End.");
        let mut largest = 0usize;
        let mut admit = |bytes: usize, _: usize| {
            largest = largest.max(bytes);
            true
        };
        let c = free_cut(&text, &mut admit).unwrap();
        assert_eq!((c.kind, c.scored), (Kind::TokenLimit, "Short sentence here."));
        // Counting the long word would take over 12 bytes of scratch per byte.
        assert!(largest < 1 << 20, "a count allocated {largest} bytes");
    }

    #[test]
    fn refused_scratch_is_reported() {
        let text = format!("{} end.", "é".repeat(64));
        let decomposed: String = text.replace('é', "e\u{301}");
        let mut refuse = |_: usize, _: usize| false;
        assert!(matches!(free_cut(&decomposed, &mut refuse), Err(NoCut::Count(Refused::Scratch(_)))));
    }

    #[test]
    fn utf16_end_counts_astral_characters() {
        // Astral characters before the cut are two UTF-16 units each.
        let mut w: Vec<String> = plain(1100);
        w[0] = "😀😀".into();
        w[499].push('.');
        let text = w.join(" ");
        let c = cut(&text).unwrap();
        assert_eq!(c.scored_words, 500);
        let units: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(c.scored_end(), c.scored.chars().count() + 2);
        assert_eq!(String::from_utf16(&units[..c.scored_end()]).unwrap(), c.scored);
    }

    /// The page sends the whole document: one of about 100,000 words fits a
    /// score request's reservation alone, and its cut is its first 1,000
    /// words.
    #[test]
    fn a_long_document_is_cut_to_its_first_1000_words() {
        let text = sentences(100_000, 10);
        let body = text.len() as u64 + crate::bound::ENVELOPE_FIXED_BYTES;
        assert!(body <= crate::bound::RAW_BODY_BYTES);
        assert!(crate::routes::scan_footprint(Some(body)) <= crate::routes::SCAN_MEMORY_BYTES, "body {body}");
        let c = cut(&text).unwrap();
        assert_eq!((c.kind, c.total_words, c.scored_words), (Kind::FirstWords, 100_000, 1000));
        assert_eq!(c.scored, sentences(1000, 10));
    }

    #[test]
    fn word_starts_and_codes() {
        assert_eq!(word_start("  a bb\tccc", 0), Some(2));
        assert_eq!(word_start("  a bb\tccc", 2), Some(7));
        assert_eq!(word_start("  a bb\tccc", 3), None);
        for k in [Kind::Whole, Kind::FirstWords, Kind::TokenLimit] {
            assert_eq!(Kind::from_code(k.code()), Some(k));
        }
        const { assert!(FREE_MAX_WORDS == 1000) };
    }
}
