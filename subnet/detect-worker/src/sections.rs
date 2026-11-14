//! The paid whole-document check's sections (pure): rules W1 to W6.
//!
//! A paid check scores a long document as overlapping sections, each inside
//! the measured range (at most 1,000 words and the scorer's 2,046 content
//! tokens), cut with the free check's own rule (`crate::cut`):
//!
//! - **W1 (the first section)** is exactly the free cut: the same start and
//!   the same C1/C2 end, so its result is the free check's, which is not
//!   scored or billed again.
//! - **W2 (each section's end)** is the C1/C2 cut from the section's start:
//!   the last unit end (or, failing one, line end) that leaves at most 1,000
//!   words and 2,046 content tokens.
//! - **W3 (the next start)** is the last unit start after the section's
//!   start and at or before the first of its last [`OVERLAP_WORDS`] words, so
//!   consecutive sections overlap by at least two span floors. With no unit
//!   start there, the next section starts at the first word after this one's
//!   end, with no overlap.
//! - **W4 (the last section)**: when the rest of the document from the next
//!   start fits one section, the last section is instead the longest tail
//!   that fits, starting at the earliest unit start after the previous
//!   section's start from which the rest fits. No short remnant is scored.
//! - **W5 (no unit end)**: when neither a unit end nor a line end lies
//!   within a section's reach of its start, the stretch to the next unit end
//!   or line end is not scored and not billed; scoring resumes at the next
//!   word. A section shorter than the measured range's floor
//!   ([`MEASURED_MIN_WORDS`]), or larger than the scan memory lets one
//!   section hold, is not scored either (listed the same way). The quote
//!   lists every such stretch before any charge.
//! - **W6 (ownership)**: each overlap is split at its middle word; the words
//!   before it belong to the earlier section, the rest to the later one.
//!   Owned words partition the scored words; they set each section's range
//!   and its weight in the document's share.
//!
//! **What is billed** is the content tokens of the scored text past the free
//! cut, counted once: each section's part beyond what earlier sections
//! covered, each counted on its own. Every such piece starts at a section's
//! end or start, both whitespace boundaries where the tokenizer splits, so
//! the pieces sum to the count of the scored text past the cut
//! (`tests::the_pieces_count_the_text_past_the_cut_once`). Overlaps are
//! SlopNinja's cost, never the reader's.
//!
//! Offsets are byte offsets into the canonical text (`canon_v1`); the page
//! takes UTF-16 offsets ([`utf16_at`]). Nothing here reads a clock or
//! allocates beyond the plan and the counts' `admit`.

use serde::{Deserialize, Serialize};
use slop_ninja_scorer_tokens::Admit;

use crate::bound::{FRAME_TOKENS, MEASURED_MIN_WORDS};
use crate::cut::{self, NoCut, word_start};
use crate::policy::{self, words};

/// The least overlap between consecutive sections: two span floors, so an
/// AI passage of 24 words or more that crosses a boundary lies whole in one
/// of them.
pub const OVERLAP_WORDS: usize = 2 * policy::SPAN_MIN_WORDS as usize;

/// One scored section.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    /// From 1.
    pub index: u32,
    /// Byte offsets into the canonical text.
    pub start: usize,
    pub end: usize,
    /// The section's exact token count, frame included.
    pub tokens: u64,
    pub words: u32,
    /// The words it owns (W6), as byte offsets and word numbers from 1.
    pub owned_start: usize,
    pub owned_end: usize,
    pub owned_from_word: u32,
    pub owned_to_word: u32,
}

/// Why a stretch is not scored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// W5: no unit end or line end within a section's reach.
    NoSentenceEnd,
    /// The rules left a section under the measured floor.
    TooShort,
    /// One section would not fit the scan memory.
    TooLarge,
}

/// A stretch the check does not score or bill.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unscored {
    pub start: usize,
    pub end: usize,
    pub from_word: u32,
    pub to_word: u32,
    pub why: Why,
}

/// A document's sections.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub sections: Vec<Section>,
    #[serde(default)]
    pub unscored: Vec<Unscored>,
    pub total_words: u32,
    /// Content tokens of the scored text past the free cut (before the
    /// billing minimum).
    pub paid_tokens: u64,
}

impl Plan {
    /// Words the sections score (owned words).
    pub fn scored_words(&self) -> u32 {
        self.sections.iter().map(|s| s.owned_to_word + 1 - s.owned_from_word).sum()
    }
}

fn u32_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The first non-whitespace character at or after `at`.
pub fn next_word(text: &str, at: usize) -> Option<usize> {
    text.get(at..)?.find(|c: char| !c.is_whitespace()).map(|i| at + i)
}

/// The unit starts of `text`: the first word after each unit end, ascending.
pub fn unit_starts(text: &str) -> impl Iterator<Item = usize> + '_ {
    let mut last = 0usize;
    cut::unit_ends(text).filter_map(move |e| {
        let s = next_word(text, e)?;
        (s > last).then(|| {
            last = s;
            s
        })
    })
}

/// W3: the last unit start after `s` and at or before the first of the last
/// [`OVERLAP_WORDS`] words of the section `[s, e)`.
fn overlap_start(text: &str, s: usize, e: usize) -> Option<usize> {
    let sec = text.get(s..e)?;
    let n = words(sec);
    if n <= OVERLAP_WORDS {
        return None;
    }
    let p = word_start(sec, n - OVERLAP_WORDS)?;
    unit_starts(sec).take_while(|&u| u <= p).last().filter(|&u| u > 0).map(|u| s + u)
}

/// W4: the earliest start in `(s, next]` (a unit start, or `next` itself)
/// from which the rest of `text` fits one section. Later starts leave less,
/// so the fit is monotone and a binary search finds it.
fn earliest_fitting(text: &str, s: usize, next: usize, admit: Admit<'_>) -> Result<usize, NoCut> {
    let rest = text.get(s..).unwrap_or("");
    let mut cands: Vec<usize> = unit_starts(rest).map(|u| s + u).take_while(|&u| u < next).filter(|&u| u > s).collect();
    cands.push(next);
    let (mut lo, mut hi) = (0usize, cands.len() - 1);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let at = cands.get(mid).copied().unwrap_or(next);
        if cut::fits_whole(text.get(at..).unwrap_or(""), &mut *admit)? {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Ok(cands.get(lo).copied().unwrap_or(next))
}

/// W5: where an unscorable stretch from `at` ends: the first unit end or
/// line end after it, or the text's end.
fn stretch_end(text: &str, at: usize) -> usize {
    let rest = text.get(at..).unwrap_or("");
    let u = cut::unit_ends(rest).next();
    let l = cut::line_ends(rest).next();
    match (u, l) {
        (Some(a), Some(b)) => at + a.min(b),
        (Some(a), None) | (None, Some(a)) => at + a,
        (None, None) => text.len(),
    }
}

/// The exact count of `piece`, frame included.
fn count(piece: &str, admit: Admit<'_>) -> Result<u64, NoCut> {
    slop_ninja_scorer_tokens::count(piece, admit).map_err(NoCut::Count)
}

/// Counts the words before byte offsets, mostly moving forward.
struct WordCursor<'a> {
    text: &'a str,
    at: usize,
    n: usize,
}

impl WordCursor<'_> {
    /// Words that start before `x` (`x` at a word start or a word's end).
    fn before(&mut self, x: usize) -> usize {
        if x < self.at {
            self.at = 0;
            self.n = 0;
        }
        self.n += words(self.text.get(self.at..x).unwrap_or(""));
        self.at = x;
        self.n
    }
}

/// The plan of the canonical text `text`. `max_section_bytes` is the
/// largest section one scan may hold; `admit` is offered every allocation
/// the counts make. `NoCut::NoSentenceEnd` when the free cut itself finds
/// none (the free check refused such a text).
pub fn plan(text: &str, max_section_bytes: usize, admit: Admit<'_>) -> Result<Plan, NoCut> {
    #[derive(Clone, Copy)]
    enum Part {
        Scored(usize, usize, u64),
        Gap(usize, usize, Why),
    }
    let first = cut::cut_end(text, &mut *admit)?;
    let mut parts = vec![Part::Scored(0, first.end, first.tokens)];
    // A start after a stretch that is not scored: no overlap to look for.
    let mut forced: Option<usize> = None;
    loop {
        let (next, prev) = match (forced.take(), parts.last().copied()) {
            (Some(f), _) => (f, None),
            (None, Some(Part::Scored(s, e, _))) => match overlap_start(text, s, e).or_else(|| next_word(text, e)) {
                Some(n) => (n, Some(s)),
                None => break,
            },
            _ => break,
        };
        let rest = text.get(next..).unwrap_or("");
        let (start, end, tokens) = if cut::fits_whole(rest, &mut *admit)? {
            // W4: the longest tail that fits.
            let u = match prev {
                Some(s) => earliest_fitting(text, s, next, &mut *admit)?,
                None => next,
            };
            (u, text.len(), count(text.get(u..).unwrap_or(""), &mut *admit)?)
        } else {
            match cut::cut_end(rest, &mut *admit) {
                Ok(c) => (next, next + c.end, c.tokens),
                Err(NoCut::NoSentenceEnd) => {
                    let end = stretch_end(text, next);
                    parts.push(Part::Gap(next, end, Why::NoSentenceEnd));
                    match next_word(text, end) {
                        Some(n) => {
                            forced = Some(n);
                            continue;
                        }
                        None => break,
                    }
                }
                Err(e) => return Err(e),
            }
        };
        let n = words(text.get(start..end).unwrap_or(""));
        let why = if n < MEASURED_MIN_WORDS {
            Some(Why::TooShort)
        } else if end - start > max_section_bytes {
            Some(Why::TooLarge)
        } else {
            None
        };
        match why {
            None => parts.push(Part::Scored(start, end, tokens)),
            Some(w) => {
                // Never overlapping what an earlier section scored.
                let from = match parts.last() {
                    Some(Part::Scored(_, e, _)) => start.max(*e),
                    _ => start,
                };
                parts.push(Part::Gap(from, end, w));
                forced = next_word(text, end);
            }
        }
        if end >= text.len() || (forced.is_none() && matches!(parts.last(), Some(Part::Gap(..)))) {
            break;
        }
    }

    // Words, ownership (W6) and the billed pieces.
    let mut cursor = WordCursor { text, at: 0, n: 0 };
    let mut sections: Vec<Section> = Vec::new();
    let mut unscored: Vec<Unscored> = Vec::new();
    let mut covered = first.end;
    let mut paid = 0u64;
    // Whether the previous part was a scored section (an overlap may follow).
    let mut adjacent = false;
    for part in &parts {
        match *part {
            Part::Gap(s, e, why) => {
                let from = cursor.before(s);
                let n = words(text.get(s..e).unwrap_or(""));
                if n > 0 {
                    unscored.push(Unscored {
                        start: s,
                        end: e,
                        from_word: u32_of(from + 1),
                        to_word: u32_of(from + n),
                        why,
                    });
                }
                adjacent = false;
            }
            Part::Scored(s, e, tokens) => {
                let n = words(text.get(s..e).unwrap_or(""));
                let mut owned_start = s;
                if let (true, Some(prev)) = (adjacent, sections.last_mut()) {
                    // The boundary: the overlap's middle word, or this start.
                    let boundary = if s < prev.end {
                        let ov = text.get(s..prev.end).unwrap_or("");
                        s + word_start(ov, words(ov) / 2).unwrap_or(0)
                    } else {
                        s
                    };
                    let prev_end = text.get(..boundary).map_or(boundary, |t| t.trim_end().len()).max(prev.owned_start);
                    prev.owned_end = prev_end;
                    let prev_from = prev.owned_from_word as usize;
                    let before = cursor.before(boundary);
                    prev.owned_to_word = u32_of(before.max(prev_from));
                    owned_start = boundary;
                }
                let from = cursor.before(owned_start);
                if s > 0 {
                    let a = s.max(covered);
                    if a < e {
                        paid += count(text.get(a..e).unwrap_or(""), &mut *admit)?.saturating_sub(FRAME_TOKENS);
                        covered = e;
                    }
                }
                let to = from + words(text.get(owned_start..e).unwrap_or(""));
                sections.push(Section {
                    index: u32_of(sections.len() + 1),
                    start: s,
                    end: e,
                    tokens,
                    words: u32_of(n),
                    owned_start,
                    owned_end: e,
                    owned_from_word: u32_of(from + 1),
                    owned_to_word: u32_of(to),
                });
                adjacent = true;
            }
        }
    }
    Ok(Plan { sections, unscored, total_words: u32_of(words(text)), paid_tokens: paid })
}

/// The UTF-16 offset of byte offset `at` in `text`.
pub fn utf16_at(text: &str, at: usize) -> usize {
    text.get(..at).map_or(0, |t| t.encode_utf16().count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bound::{FREE_MAX_WORDS, MAX_TOKENS};

    fn admit_all() -> impl FnMut(usize, usize) -> bool {
        |_, _| true
    }

    /// `n` words in sentences of `per` words, every word distinct enough
    /// that the tokenizer splits them as prose.
    fn prose(n: usize, per: usize) -> String {
        const W: &[&str] = &["the", "river", "kept", "its", "slow", "course", "past", "old", "mills", "and"];
        let mut s = String::new();
        for i in 0..n {
            if i > 0 {
                s.push(if i % (per * 6) == 0 { '\n' } else { ' ' });
                if i % (per * 6) == 0 {
                    s.push('\n');
                }
            }
            s.push_str(W[i % W.len()]);
            if (i + 1) % per == 0 {
                s.push('.');
            }
        }
        s.push('.');
        s
    }

    fn plan_of(text: &str) -> Plan {
        plan(text, usize::MAX, &mut admit_all()).unwrap()
    }

    fn count_all(t: &str) -> u64 {
        slop_ninja_scorer_tokens::count(t, &mut admit_all()).unwrap()
    }

    #[test]
    fn the_first_section_is_the_free_cut() {
        let text = prose(3_214, 17);
        let p = plan_of(&text);
        let free = cut::free_cut(&text, &mut admit_all()).unwrap();
        let s1 = p.sections[0];
        assert_eq!((s1.start, s1.end, s1.tokens), (0, free.scored.len(), free.tokens));
        assert_eq!(s1.words as usize, free.scored_words);
        assert_eq!(p.total_words, 3_214);
    }

    #[test]
    fn sections_fit_overlap_and_end_with_the_longest_tail() {
        for (n, per) in [(1_001, 13), (2_500, 9), (3_214, 17), (9_999, 21), (20_000, 11)] {
            let text = prose(n, per);
            let p = plan_of(&text);
            assert!(p.unscored.is_empty(), "{n}");
            assert!(p.sections.len() >= 2, "{n}");
            for (i, s) in p.sections.iter().enumerate() {
                let t = &text[s.start..s.end];
                assert!(words(t) <= FREE_MAX_WORDS && s.tokens <= MAX_TOKENS, "{n} section {i}");
                assert_eq!(s.tokens, count_all(t), "exact count");
                assert!(s.words as usize >= MEASURED_MIN_WORDS);
                if let Some(next) = p.sections.get(i + 1) {
                    assert!(next.start > s.start && next.start < s.end, "consecutive sections overlap");
                    let overlap = words(&text[next.start..s.end]);
                    assert!(overlap >= OVERLAP_WORDS, "{n} overlap {overlap}");
                    // The next section starts at a unit start.
                    assert!(text[..next.start].trim_end().ends_with('.'), "starts after a sentence end");
                }
            }
            let last = p.sections.last().unwrap();
            assert_eq!(last.end, text.len(), "the last section reaches the end");
            // The longest tail: one unit start earlier (after the previous
            // section's start) would not fit.
            let prev = p.sections[p.sections.len() - 2];
            if let Some(earlier) = unit_starts(&text[..last.start]).filter(|&u| u > prev.start).last() {
                assert!(!cut::fits_whole(&text[earlier..], &mut admit_all()).unwrap(), "{n}");
            }
            assert!(cut::fits_whole(&text[last.start..], &mut admit_all()).unwrap());
        }
    }

    #[test]
    fn owned_words_partition_the_document() {
        for (n, per) in [(1_500, 12), (5_000, 19), (12_345, 7)] {
            let text = prose(n, per);
            let p = plan_of(&text);
            let mut next = 1;
            for s in &p.sections {
                assert_eq!(s.owned_from_word, next, "{n}");
                assert!(s.owned_to_word >= s.owned_from_word);
                assert_eq!(
                    words(&text[s.owned_start..s.owned_end]) as u32,
                    s.owned_to_word - s.owned_from_word + 1,
                    "{n}: the byte range holds the owned words"
                );
                assert!(s.start <= s.owned_start && s.owned_end <= s.end);
                next = s.owned_to_word + 1;
            }
            assert_eq!(next - 1, p.total_words);
            assert_eq!(p.scored_words(), p.total_words);
            // Every owned word has at least 24 words of context toward a
            // boundary it shares with another section.
            for w in p.sections.windows(2) {
                let before = words(&text[w[1].start..w[0].owned_end]);
                let after = words(&text[w[1].owned_start..w[0].end]);
                assert!(before >= OVERLAP_WORDS / 2 && after >= OVERLAP_WORDS / 2, "{n}: {before} {after}");
            }
        }
    }

    #[test]
    fn the_pieces_count_the_text_past_the_cut_once() {
        for (n, per) in [(1_200, 10), (4_000, 15), (10_000, 23)] {
            let text = prose(n, per);
            let p = plan_of(&text);
            let free = cut::free_cut(&text, &mut admit_all()).unwrap();
            let whole = count_all(&text) - FRAME_TOKENS;
            let cut_part = free.tokens - FRAME_TOKENS;
            assert_eq!(p.paid_tokens, whole - cut_part, "{n}: the document's tokens past the free cut");
        }
    }

    #[test]
    fn a_stretch_without_a_sentence_end_is_listed_and_skipped() {
        // 1,200 words of prose, 1,300 words with no unit or line end, 1,200
        // more words of prose.
        let mut text = prose(1_200, 12);
        text.push(' ');
        for i in 0..1_300 {
            text.push_str(if i % 2 == 0 { "word" } else { "more" });
            text.push(' ');
        }
        text.push_str("ends here.");
        text.push('\n');
        text.push('\n');
        text.push_str(&prose(1_200, 12));
        let p = plan_of(&text);
        assert_eq!(p.unscored.len(), 1, "{:?}", p.unscored);
        let u = p.unscored[0];
        assert_eq!(u.why, Why::NoSentenceEnd);
        assert!(text[u.start..u.end].contains("word more"));
        assert!(text[u.start..u.end].ends_with("ends here."));
        // Scored words and unscored words together cover the document.
        let gap_words = u.to_word - u.from_word + 1;
        assert_eq!(p.scored_words() + gap_words, p.total_words);
        // No section reaches into the stretch.
        for s in &p.sections {
            assert!(s.end <= u.start || s.start >= u.end);
        }
        // The stretch is not billed.
        let free = cut::free_cut(&text, &mut admit_all()).unwrap();
        assert!(p.paid_tokens < count_all(&text) - free.tokens);
    }

    #[test]
    fn a_section_larger_than_one_scan_may_hold_is_not_scored() {
        let text = prose(3_000, 12);
        let p = plan(&text, 1_000, &mut admit_all()).unwrap();
        assert_eq!(p.sections.len(), 1, "only the free cut, which the free check scored");
        assert!(p.unscored.iter().any(|u| u.why == Why::TooLarge));
        // Nothing past the free cut is scored, and the stretches cover it.
        assert!(p.unscored.iter().all(|u| matches!(u.why, Why::TooLarge | Why::TooShort)));
        let listed: u32 = p.unscored.iter().map(|u| u.to_word + 1 - u.from_word).sum();
        assert_eq!(p.scored_words() + listed, p.total_words);
        assert_eq!(p.paid_tokens, 0);
    }

    #[test]
    fn utf16_offsets() {
        assert_eq!(utf16_at("a😀b", 5), 3);
        assert_eq!(utf16_at("abc", 2), 2);
        assert_eq!(next_word("a  b", 1), Some(3));
        assert_eq!(next_word("a  ", 1), None);
        assert_eq!(unit_starts("One. Two.\n\nThree").collect::<Vec<_>>(), vec![5, 11]);
    }
}
