//! A public run's document (`r/<run_id>`) from the scored text: what the
//! widest display policy (`display_v1`) may show, and nothing more. The
//! page's full text is never stored.
//!
//! - **A web page in a strong band** (and a short text with strong signs):
//!   the highlighted passages, most confident first, each with one sentence
//!   of greyed context (the sentence before it, or after it when it opens
//!   the text), up to about a quarter of the scored words; the first passage
//!   is always quoted. Every other passage is listed by its word range, its
//!   confidence and a text-fragment link to the source.
//! - **A web page in a weak or unclear band** (and a short text with no
//!   strong signs): at most the first sentence, when it fits the same
//!   quarter.
//! - **A post on Bluesky**: the whole post, as the platform's terms allow.
//!
//! Passages and anchors are UTF-16 offsets into the document's `body`; word
//! ranges count the source page's whitespace words from one. A text
//! fragment (`#:~:text=` value) names the first and last
//! [`FRAGMENT_WORDS`] words of its range, percent-encoded.

use slop_ninja_detect_page::codes::AnchorRole;
use slop_ninja_detect_page::page::{Anchor, FurtherPassage, Span};

use crate::doc::{DOC_V, RunDoc, StoredResult};

/// The version of the document's building rule (`runs.display_v` stays the
/// display policy's own).
pub const EXCERPT_V: &str = "excerpt_v1";
/// Quoted text is at most `1 / QUOTE_DIVISOR` of the scored words.
pub const QUOTE_DIVISOR: usize = 4;
/// Words at each end of a text fragment.
pub const FRAGMENT_WORDS: usize = 4;

/// What kind of source the text came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Web,
    /// A post on a social platform whose terms allow showing it whole.
    SocialPost,
}

/// Percent-encodes one text fragment term (everything but ASCII letters,
/// digits and `._~`, so `-`, `,` and `&` never appear raw).
pub fn encode_term(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// The `#:~:text=` value of a passage: the whole passage when it is short,
/// else its first and last [`FRAGMENT_WORDS`] words as a range.
pub fn fragment(passage: &str) -> String {
    let words: Vec<&str> = passage.split_whitespace().collect();
    if words.len() <= 2 * FRAGMENT_WORDS {
        return encode_term(&words.join(" "));
    }
    let start = words.get(..FRAGMENT_WORDS).map(|w| w.join(" ")).unwrap_or_default();
    let end = words.get(words.len() - FRAGMENT_WORDS..).map(|w| w.join(" ")).unwrap_or_default();
    format!("{},{}", encode_term(&start), encode_term(&end))
}

/// The byte offset of UTF-16 offset `u` in `s` (`None` past the end or
/// inside a surrogate pair).
fn byte_of_utf16(s: &str, u: u32) -> Option<usize> {
    let mut seen = 0u32;
    for (i, c) in s.char_indices() {
        if seen == u {
            return Some(i);
        }
        seen += c.len_utf16() as u32;
        if seen > u {
            return None;
        }
    }
    (seen == u).then_some(s.len())
}

fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// Words of `s` before byte `at`, and words up to byte `end` (one-based
/// range `from..=to` of the words the bytes `at..end` touch).
fn word_range(s: &str, at: usize, end: usize) -> (u32, u32) {
    let before = s.get(..at).map_or(0, |p| p.split_whitespace().count());
    let upto = s.get(..end).map_or(0, |p| p.split_whitespace().count());
    // A range that starts inside a word counts that word as its first.
    let mid = s.get(..at).is_some_and(|p| !p.is_empty() && !p.ends_with(char::is_whitespace))
        && s.get(at..).is_some_and(|r| r.starts_with(|c: char| !c.is_whitespace()));
    let from = if mid { before } else { before + 1 };
    (from as u32, upto.max(from) as u32)
}

/// The sentence (unit) bounds of `s`: `[(start, end)]` in bytes, trimmed.
fn sentences(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for end in crate::cut::unit_ends(s) {
        if let Some(piece) = s.get(start..end) {
            let lead = piece.len() - piece.trim_start().len();
            if start + lead < end {
                out.push((start + lead, end));
            }
        }
        start = end;
    }
    if let Some(rest) = s.get(start..) {
        let lead = rest.len() - rest.trim_start().len();
        let t = rest.trim_end();
        if lead < t.len() {
            out.push((start + lead, start + t.len()));
        }
    }
    out
}

struct Passage {
    span: Span,
    at: usize,
    end: usize,
}

/// Builds the document of a run whose scored text is `scored` (`canon_v1`
/// of the extracted text, cut by the free cut) and whose result's spans are
/// UTF-16 offsets into it.
pub fn run_doc(
    scored: &str,
    kind: SourceKind,
    page_words: u32,
    page_hash: Option<String>,
    result: &StoredResult,
) -> RunDoc {
    let mut out = RunDoc {
        v: DOC_V,
        body: String::new(),
        anchors: Vec::new(),
        further: Vec::new(),
        page_words,
        page_hash,
        result_full: result.clone(),
    };
    if kind == SourceKind::SocialPost {
        out.body = scored.to_owned();
        return out;
    }
    out.result_full.spans.clear();
    let words = scored.split_whitespace().count();
    let budget = (words / QUOTE_DIVISOR).max(1);
    if !result.is_strong() {
        // At most the first sentence, inside the quarter.
        if let Some(&(a, e)) = sentences(scored).first()
            && let Some(first) = scored.get(a..e)
            && first.split_whitespace().count() <= budget
        {
            let (from_word, to_word) = word_range(scored, a, e);
            out.body = first.to_owned();
            out.anchors.push(Anchor {
                start: 0,
                end: utf16_len(first),
                role: AnchorRole::FirstSentence,
                from_word,
                to_word,
                fragment: Some(fragment(first)),
            });
        }
        return out;
    }
    let mut passages: Vec<Passage> = result
        .spans
        .iter()
        .filter_map(|sp| {
            let at = byte_of_utf16(scored, sp.start)?;
            let end = byte_of_utf16(scored, sp.end)?;
            (at < end).then_some(Passage { span: *sp, at, end })
        })
        .collect();
    // Most confident first; a short result's passages (no confidence) by
    // position.
    passages.sort_by(|a, b| b.span.confidence.cmp(&a.span.confidence).then(a.at.cmp(&b.at)));
    let units = sentences(scored);
    let mut quoted: Vec<(Passage, Option<(usize, usize)>)> = Vec::new();
    let mut listed: Vec<Passage> = Vec::new();
    let mut used = 0usize;
    for p in passages {
        let pw = scored.get(p.at..p.end).map_or(0, |x| x.split_whitespace().count());
        if !quoted.is_empty() && used + pw > budget {
            listed.push(p);
            continue;
        }
        used += pw;
        // One sentence of context: the one ending at or before the passage,
        // else the one starting at or after its end.
        let ctx = units
            .iter()
            .rev()
            .find(|(_, e)| *e <= p.at)
            .or_else(|| units.iter().find(|(a, _)| *a >= p.end))
            .copied()
            .filter(|(a, e)| {
                let cw = scored.get(*a..*e).map_or(0, |x| x.split_whitespace().count());
                let fits = used + cw <= budget;
                if fits {
                    used += cw;
                }
                fits
            });
        quoted.push((p, ctx));
    }
    // The quoted pieces in document order, each its passage and context.
    quoted.sort_by_key(|(p, _)| p.at);
    let mut body = String::new();
    let mut spans = Vec::new();
    for (p, ctx) in &quoted {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        let (seg_a, seg_e) = match ctx {
            Some((a, e)) => ((*a).min(p.at), (*e).max(p.end)),
            None => (p.at, p.end),
        };
        let Some(seg) = scored.get(seg_a..seg_e) else { continue };
        let base = utf16_len(&body);
        body.push_str(seg);
        let off = |b: usize| base + scored.get(seg_a..b).map_or(0, utf16_len);
        if let Some((a, e)) = ctx {
            let (from_word, to_word) = word_range(scored, *a, *e);
            out.anchors.push(Anchor {
                start: off(*a),
                end: off(*e),
                role: AnchorRole::Context,
                from_word,
                to_word,
                fragment: None,
            });
        }
        let (from_word, to_word) = word_range(scored, p.at, p.end);
        let text = scored.get(p.at..p.end).unwrap_or_default();
        out.anchors.push(Anchor {
            start: off(p.at),
            end: off(p.end),
            role: AnchorRole::Passage,
            from_word,
            to_word,
            fragment: Some(fragment(text)),
        });
        spans.push(Span { start: off(p.at), end: off(p.end), ..p.span });
    }
    out.anchors.sort_by_key(|a| (a.start, a.end));
    listed.sort_by_key(|p| p.at);
    for p in &listed {
        let (from_word, to_word) = word_range(scored, p.at, p.end);
        out.further.push(FurtherPassage {
            from_word,
            to_word,
            fragment: fragment(scored.get(p.at..p.end).unwrap_or_default()),
            confidence: p.span.confidence,
        });
    }
    out.body = body;
    out.result_full.spans = spans;
    out
}

/// An X post's document: the document-level result only (no passages,
/// anchors, offsets, hashes or span data).
pub fn x_run_doc(result: &StoredResult) -> RunDoc {
    let mut r = result.clone();
    r.spans.clear();
    r.spans_note = Some(slop_ninja_detect_page::codes::SpansNote::XPost);
    RunDoc {
        v: DOC_V,
        body: String::new(),
        anchors: Vec::new(),
        further: Vec::new(),
        page_words: r.words,
        page_hash: None,
        result_full: r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_detect_page::codes::{Band, CutKind, ShortResult};

    fn result(band: Option<Band>, short: Option<ShortResult>, spans: Vec<Span>, words: u32) -> StoredResult {
        StoredResult {
            scored_at: 1,
            package_load: "l".into(),
            package_label: "span package d24".into(),
            package_pub: "p".repeat(64),
            publication_pending: false,
            scorer_build: "7030a86f".into(),
            policy: "long-v1".into(),
            evidence: "e".into(),
            band,
            short,
            confidence_tenths: band.map(|_| 973),
            confidence_text: band.map(|_| "97.3%".into()),
            share_pct: band.map(|_| 43),
            share_text: band.map(|_| "about 43%".into()),
            words,
            total_words: words,
            cut: CutKind::None,
            tokens: words * 2,
            length: None,
            spans,
            spans_note: None,
            markers: Vec::new(),
            invisible: false,
            raw: None,
        }
    }

    fn span_of(text: &str, needle: &str, conf: Option<u8>) -> Span {
        let at = text.find(needle).unwrap();
        let start = text[..at].encode_utf16().count() as u32;
        let end = start + needle.encode_utf16().count() as u32;
        Span { start, end, words: needle.split_whitespace().count() as u32, confidence: conf }
    }

    const H: &str = "We planted the tomatoes late this year because the frost hung around until May.";
    const A1: &str = "In today's fast-paced world, community gardens serve as vibrant hubs that foster connection, \
promote sustainability, and empower residents to take meaningful ownership of their shared green spaces.";
    const A2: &str = "Moreover, by leveraging innovative composting strategies and data-driven planting schedules, \
organizers can unlock unprecedented yields while ensuring that every volunteer feels seen and inspired.";

    fn page(n: usize) -> String {
        let mut s = String::new();
        for i in 0..n {
            if i == 3 {
                s.push_str(A1);
            } else if i == 9 {
                s.push_str(A2);
            } else {
                s.push_str(H);
            }
            s.push(' ');
        }
        s.trim_end().to_owned()
    }

    #[test]
    fn strong_pages_quote_passages_with_context_and_list_the_rest() {
        let text = page(12);
        let r = result(Some(Band::Strong), None, vec![span_of(&text, A1, Some(91)), span_of(&text, A2, Some(97))], 300);
        let d = run_doc(&text, SourceKind::Web, 300, Some("h".into()), &r);
        // The most confident passage is quoted; the budget (a quarter) left
        // the other one listed.
        let words = text.split_whitespace().count();
        assert!(d.body.split_whitespace().count() <= words / QUOTE_DIVISOR);
        assert!(d.body.contains(A2) && !d.body.contains(A1), "{}", d.body);
        assert_eq!(d.result_full.spans.len(), 1);
        let units: Vec<u16> = d.body.encode_utf16().collect();
        let s = &d.result_full.spans[0];
        assert_eq!(String::from_utf16(&units[s.start as usize..s.end as usize]).unwrap(), A2);
        assert_eq!(d.result_full.spans[0].confidence, Some(97));
        // Its context sentence is the one before it, greyed.
        let ctx = d.anchors.iter().find(|a| a.role == AnchorRole::Context).unwrap();
        assert_eq!(String::from_utf16(&units[ctx.start as usize..ctx.end as usize]).unwrap(), H);
        assert!(ctx.fragment.is_none());
        let pa = d.anchors.iter().find(|a| a.role == AnchorRole::Passage).unwrap();
        assert_eq!((pa.start, pa.end), (s.start, s.end));
        let first = text[..text.find(A2).unwrap()].split_whitespace().count() as u32 + 1;
        assert_eq!((pa.from_word, pa.to_word), (first, first + A2.split_whitespace().count() as u32 - 1));
        // The other passage is listed by position, with its fragment.
        assert_eq!(d.further.len(), 1);
        let f = &d.further[0];
        assert_eq!(f.confidence, Some(91));
        assert_eq!(f.fragment, "In%20today%27s%20fast%2Dpaced%20world%2C,their%20shared%20green%20spaces.");
        // Never the full text.
        assert!(d.body.len() < text.len() / 2);
    }

    #[test]
    fn weak_pages_keep_at_most_the_first_sentence() {
        let text = page(12);
        let d = run_doc(&text, SourceKind::Web, 200, None, &result(Some(Band::Weak), None, Vec::new(), 200));
        assert_eq!(d.body, H);
        assert_eq!(d.anchors.len(), 1);
        assert_eq!(d.anchors[0].role, AnchorRole::FirstSentence);
        assert_eq!((d.anchors[0].from_word, d.anchors[0].to_word), (1, H.split_whitespace().count() as u32));
        assert!(d.further.is_empty() && d.result_full.spans.is_empty());
        // A first sentence longer than the quarter is not quoted.
        let long = format!("{} {}", A1.trim_end_matches('.'), page(1));
        let d = run_doc(&long, SourceKind::Web, 60, None, &result(Some(Band::Unclear), None, Vec::new(), 60));
        assert!(d.body.is_empty() && d.anchors.is_empty());
    }

    #[test]
    fn short_strong_texts_quote_their_passage_and_social_posts_stay_whole() {
        let text = format!("{H} {A1}");
        let r = result(None, Some(ShortResult::Strong), vec![span_of(&text, A1, None)], 60);
        let d = run_doc(&text, SourceKind::Web, 60, None, &r);
        assert!(d.body.contains(A1));
        assert_eq!(d.result_full.spans[0].confidence, None);
        let d = run_doc(&text, SourceKind::SocialPost, 60, None, &r);
        assert_eq!(d.body, text);
        assert_eq!(d.result_full.spans, r.spans);
    }

    #[test]
    fn fragments_are_percent_encoded_ranges() {
        assert_eq!(fragment("a b"), "a%20b");
        assert_eq!(encode_term("x-y,z&w é"), "x%2Dy%2Cz%26w%20%C3%A9");
        let f = fragment("one two three four five six seven eight nine ten");
        assert_eq!(f, "one%20two%20three%20four,seven%20eight%20nine%20ten");
        assert_eq!(f.matches(',').count(), 1);
    }

    #[test]
    fn x_documents_hold_the_result_only() {
        let text = page(2);
        let r = result(None, Some(ShortResult::Strong), vec![span_of(&text, H, None)], 30);
        let d = x_run_doc(&r);
        assert!(d.body.is_empty() && d.anchors.is_empty() && d.further.is_empty() && d.page_hash.is_none());
        assert!(d.result_full.spans.is_empty());
        let json = String::from_utf8(d.encode()).unwrap();
        assert!(!json.contains("tomatoes"));
    }

    #[test]
    fn offsets_survive_astral_characters() {
        let text = format!("\u{1F331} {H} {A1}");
        let r = result(Some(Band::VeryStrong), None, vec![span_of(&text, A1, Some(99))], 60);
        let d = run_doc(&text, SourceKind::Web, 60, None, &r);
        let units: Vec<u16> = d.body.encode_utf16().collect();
        let s = &d.result_full.spans[0];
        assert_eq!(String::from_utf16(&units[s.start as usize..s.end as usize]).unwrap(), A1);
    }
}
