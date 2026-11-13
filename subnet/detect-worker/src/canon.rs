//! `canon_v1`, the canonical form of a pasted text, and its alignment back
//! to the exact string the page sent.
//!
//! The canonical form is chosen so that the scorer's outputs do not change:
//! 1. CRLF and a lone CR become LF;
//! 2. leading and trailing whitespace (Unicode `White_Space`, as
//!    `str::trim`) is trimmed;
//! 3. NFC, under the scorer's own normalizer and version
//!    (`unicode-normalization-alignments` 0.1.12), which the tokenizer
//!    applies before anything else, so an NFC-equal text gets the same token
//!    ids and outputs.
//!
//! Inner whitespace is kept: collapsing it would change tokens. A seal binds
//! `SHA-256(canon_v1(text))`, so a text that differs only by line endings,
//! surrounding whitespace or normalization shares a seal, and a text with any
//! other edit does not.
//!
//! [`to_original`] maps character offsets into the canonical text (the
//! scorer's span offsets) to UTF-16 offsets into the original string. NFC
//! may reorder combining marks inside one combining sequence; the mapping is
//! exact at every position NFC leaves in place, which includes every
//! whitespace boundary, and a passage always starts and ends at a word
//! boundary.

use std::borrow::Cow;

use sha2::{Digest, Sha256};
use unicode_normalization_alignments::{IsNormalized, UnicodeNormalization, is_nfc_quick};

/// The rule's version name (`runs.canon_v`).
pub const CANON_V: &str = "canon_v1";

/// Step 1 and 2 as an iterator over `(char, utf16 start, utf16 end)` of the
/// original: CRLF and a lone CR yield one LF covering both units; leading
/// and trailing whitespace are skipped.
fn pre_chars(text: &str) -> Vec<(char, u32, u32)> {
    let mut out: Vec<(char, u32, u32)> = Vec::with_capacity(text.len());
    let mut pos = 0u32;
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        let start = pos;
        pos += c.len_utf16() as u32;
        if c == '\r' {
            if it.peek() == Some(&'\n') {
                it.next();
                pos += 1;
            }
            out.push(('\n', start, pos));
        } else {
            out.push((c, start, pos));
        }
    }
    let lead = out.iter().take_while(|x| x.0.is_whitespace()).count();
    let trail = out.iter().rev().take_while(|x| x.0.is_whitespace()).count();
    if lead == out.len() {
        return Vec::new();
    }
    out.truncate(out.len() - trail);
    out.drain(..lead);
    out
}

/// Whether `text` is already canonical: no CR, no surrounding whitespace,
/// NFC by the quick check.
fn is_canonical(text: &str) -> bool {
    !text.contains('\r') && text.trim().len() == text.len() && is_nfc_quick(text.chars()) == IsNormalized::Yes
}

/// The canonical form of `text`.
pub fn canon_v1(text: &str) -> Cow<'_, str> {
    if is_canonical(text) {
        return Cow::Borrowed(text);
    }
    let pre = pre_chars(text);
    Cow::Owned(pre.iter().map(|x| x.0).nfc().map(|(c, _)| c).collect())
}

/// `SHA-256(canon_v1(text))`, the hash a seal binds.
pub fn text_sha256(text: &str) -> [u8; 32] {
    canonical_sha256(&canon_v1(text))
}

/// The same hash of a text already in canonical form (`canon_v1` is
/// idempotent), without a second pass over it.
pub fn canonical_sha256(canonical: &str) -> [u8; 32] {
    Sha256::digest(canonical.as_bytes()).into()
}

/// Maps ascending UTF-16 offsets into `canonical` (= `canon_v1(original)`)
/// to UTF-16 offsets into `original`, through [`to_original`]. `None` when
/// an offset is out of order, past the end, or inside a surrogate pair.
pub fn utf16_to_original(original: &str, canonical: &str, offsets: &[usize]) -> Option<Vec<usize>> {
    let mut chars = Vec::with_capacity(offsets.len());
    let mut want = offsets.iter().copied().peekable();
    let (mut ci, mut u16i) = (0usize, 0usize);
    let mut it = canonical.chars();
    while let Some(&w) = want.peek() {
        if w < u16i {
            return None;
        }
        if w == u16i {
            chars.push(ci);
            want.next();
            continue;
        }
        let c = it.next()?;
        ci += 1;
        u16i += c.len_utf16();
    }
    to_original(original, &chars).map(|v| v.into_iter().map(|x| x as usize).collect())
}

/// Maps ascending character offsets into `canon_v1(original)` (each at most
/// its length in characters) to UTF-16 offsets into `original`. `None` when
/// an offset is out of order or past the end.
pub fn to_original(original: &str, canon_chars: &[usize]) -> Option<Vec<u32>> {
    if !canon_chars.is_sorted() {
        return None;
    }
    let pre = pre_chars(original);
    let end16 = pre.last().map_or(0, |x| x.2);
    // The original UTF-16 offset where pre-character `i` starts (or the end).
    let at = |i: usize| pre.get(i).map_or(end16, |x| x.1);
    let mut out = Vec::with_capacity(canon_chars.len());
    let mut want = canon_chars.iter().copied().peekable();
    // `consumed` counts the pre-characters the output so far stands for;
    // an output character's change is 1 when NFC added it, 0 when it stands
    // for one pre-character and -n when it also stands for n removed ones.
    let mut consumed: usize = 0;
    let mut k = 0usize;
    for (_, change) in pre.iter().map(|x| x.0).nfc() {
        while want.peek() == Some(&k) {
            want.next();
            out.push(at(consumed));
        }
        let used = 1isize - change;
        consumed = consumed.saturating_add(usize::try_from(used.max(0)).ok()?);
        k += 1;
    }
    while want.peek() == Some(&k) {
        want.next();
        out.push(at(consumed));
    }
    if want.next().is_some() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn line_ends_whitespace_and_nfc() {
        assert_eq!(canon_v1("a\r\nb\rc\n"), "a\nb\nc");
        assert_eq!(canon_v1("  \t hello  world \n"), "hello  world");
        assert_eq!(canon_v1("cafe\u{301}"), "caf\u{e9}");
        assert_eq!(canon_v1("\r\n \r\n"), "");
        assert_eq!(canon_v1(""), "");
        assert!(matches!(canon_v1("already canonical"), Cow::Borrowed(_)));
        // Inner whitespace is kept.
        assert_eq!(canon_v1("a \t\u{a0} b"), "a \t\u{a0} b");
    }

    #[test]
    fn equal_canonical_forms_share_a_hash() {
        assert_eq!(text_sha256("caf\u{e9} au lait\r\n"), text_sha256("  cafe\u{301} au lait"));
        assert_ne!(text_sha256("cafe au lait"), text_sha256("cafe  au lait"));
        assert_ne!(text_sha256("cafe au lait."), text_sha256("cafe au lait"));
    }

    #[test]
    fn offsets_map_back_to_the_exact_string() {
        let original = "  \u{1F345} Tomato\r\nnotes: cafe\u{301} and \u{e9}t\u{e9}.\r\n";
        let canon = canon_v1(original);
        let cc: Vec<char> = canon.chars().collect();
        // Every word start and end in the canonical text maps to the same
        // word in the original.
        let mut bounds = Vec::new();
        let mut prev_ws = true;
        for (i, c) in cc.iter().enumerate() {
            if prev_ws && !c.is_whitespace() {
                bounds.push(i);
            }
            if !prev_ws && c.is_whitespace() {
                bounds.push(i);
            }
            prev_ws = c.is_whitespace();
        }
        bounds.push(cc.len());
        let mapped = to_original(original, &bounds).unwrap();
        let o16 = utf16(original);
        let words_c: Vec<String> = canon.split_whitespace().map(str::to_owned).collect();
        let words_o: Vec<String> = bounds
            .chunks(2)
            .zip(mapped.chunks(2))
            .filter(|(b, _)| b.len() == 2)
            .map(|(_, m)| String::from_utf16(&o16[m[0] as usize..m[1] as usize]).unwrap())
            .collect();
        assert_eq!(words_c.len(), words_o.len());
        for (c, o) in words_c.iter().zip(&words_o) {
            assert_eq!(&*canon_v1(o), c.as_str(), "word {o:?}");
        }
        // The emoji is a surrogate pair: its word starts at UTF-16 offset 2.
        assert_eq!(mapped[0], 2);
        // The end of the text maps to the end of the last kept character.
        assert_eq!(*mapped.last().unwrap() as usize, original.trim_end().encode_utf16().count());
    }

    #[test]
    fn utf16_offsets_map_back_through_the_characters() {
        let original = "\r\n  caf\u{65}\u{301} \u{1F345} jam\r\nend ";
        let canon = canon_v1(original);
        assert_eq!(&*canon, "caf\u{e9} \u{1F345} jam\nend");
        // "jam" starts after the emoji's surrogate pair: canonical UTF-16 8,
        // original 13 (the four leading units, and the decomposed accent's
        // extra unit).
        let c16 = |s: &str| s.encode_utf16().count();
        let at_jam = c16("caf\u{e9} \u{1F345} ");
        assert_eq!(at_jam, 8);
        let mapped = utf16_to_original(original, &canon, &[0, at_jam, c16(&canon)]).unwrap();
        let o16: Vec<u16> = original.encode_utf16().collect();
        assert_eq!(String::from_utf16(&o16[mapped[1]..mapped[1] + 3]).unwrap(), "jam");
        assert_eq!((mapped[0], mapped[1]), (4, 13));
        assert_eq!(mapped[2], c16(original.trim_end()));
        // Inside the surrogate pair, out of order, or past the end.
        assert_eq!(utf16_to_original(original, &canon, &[c16("caf\u{e9} ") + 1]), None);
        assert_eq!(utf16_to_original(original, &canon, &[3, 2]), None);
        assert_eq!(utf16_to_original(original, &canon, &[c16(&canon) + 1]), None);
        // A canonical text maps to itself.
        assert_eq!(utf16_to_original("ab c", "ab c", &[0, 2, 4]), Some(vec![0, 2, 4]));
        assert_eq!(canonical_sha256(&canon), text_sha256(original));
    }

    #[test]
    fn offsets_out_of_order_or_past_the_end_are_refused() {
        assert_eq!(to_original("ab", &[2, 1]), None);
        assert_eq!(to_original("ab", &[3]), None);
        assert_eq!(to_original("ab", &[0, 2]), Some(vec![0, 2]));
        assert_eq!(to_original(" ab", &[0, 2]), Some(vec![1, 3]));
        assert_eq!(to_original("a\r\nb", &[1, 2, 3]), Some(vec![1, 3, 4]));
        assert_eq!(to_original("", &[0]), Some(vec![0]));
    }
}
