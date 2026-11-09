//! Input bounds, derived from the scorer's contract, the pinned tokenizer
//! and the scorer's Unicode tables. None of them is tuned.
//!
//! - **Tokens.** The scorer reads at most [`MAX_TOKENS`] tokens including the
//!   `[CLS]`/`[SEP]` frame (`inference/a-container-serve`, `engine::MAX_TOKENS`),
//!   counted exactly here by `slop-ninja-scorer-tokens` under the same table.
//! - **Bytes per token.** No vocabulary entry of the pinned byte-level BPE
//!   tokenizer is longer than [`MAX_TOKEN_TEXT_BYTES`] (a whitespace run), and
//!   no added token is longer. A test recomputes both from the compiled-in
//!   table. One exception: an added token with `lstrip` also takes the
//!   whitespace before it, however long. A text whose length rests on such
//!   whitespace is refused by the body bound as too long, which only ever
//!   refuses more.
//! - **Escaping and NFC.** The body is JSON and the scorer normalizes NFC
//!   inside tokenization. At most [`NFC_ESCAPE_FACTOR`] bytes of escaped
//!   JSON can become one byte of NFC text: U+01D5 (2 bytes) arrives as
//!   U+0055 U+0308 U+0304, each written as a `\u` escape: 18 bytes. A test
//!   recomputes the factor over every code point with the scorer's own NFC
//!   crate and version (Unicode 9.0 tables).
//! - **Envelope.** The request is exactly `{"text":"…","turnstile":"…"}`
//!   (either key order, no insignificant whitespace, no escapes in keys or in
//!   the Turnstile token), so its fixed bytes and the token's documented
//!   maximum length complete the bound.
//!
//! So a body over [`RAW_BODY_BYTES`] cannot hold a text of at most
//! [`MAX_TOKENS`] tokens (lstrip whitespace aside), and it is refused with
//! 413 before a byte of it is read into memory.

/// The scorer's token limit, frame included (`engine::MAX_TOKENS`).
pub const MAX_TOKENS: u64 = 2048;
/// The frame: `[CLS]` and `[SEP]` (`scorer_tokens::frame_tokens`, checked).
pub const FRAME_TOKENS: u64 = 2;
/// Content tokens a scored text can hold.
pub const CONTENT_TOKENS: u64 = MAX_TOKENS - FRAME_TOKENS;
/// The longest token of the pinned tokenizer, in bytes of normalized text
/// (recomputed from `scorer_tokens::TABLE` by a test).
pub const MAX_TOKEN_TEXT_BYTES: u64 = 512;
/// Escaped JSON bytes per byte of NFC text, at most (recomputed by a test from
/// `unicode-normalization-alignments` 0.1.12, the scorer's NFC crate).
pub const NFC_ESCAPE_FACTOR: u64 = 9;
/// Bytes of UTF-8 text per byte of its NFC form, at most (rounded up;
/// recomputed by a test): a decomposed character is at most this many times
/// its composed bytes. A text longer than this many times the content
/// tokens' bytes cannot fit the token limit, so it is refused without being
/// tokenized.
pub const TEXT_BYTES_PER_NFC_BYTE: u64 = 3;
/// The longest text that can fit the token limit.
pub const MAX_TEXT_BYTES: u64 = CONTENT_TOKENS * MAX_TOKEN_TEXT_BYTES * TEXT_BYTES_PER_NFC_BYTE;
/// The text field's raw bytes, at most: 2,046 × 512 × 9 = 9,427,968.
pub const TEXT_FIELD_BYTES: u64 = CONTENT_TOKENS * MAX_TOKEN_TEXT_BYTES * NFC_ESCAPE_FACTOR;
/// A Turnstile response token's documented maximum length (2,048
/// characters, all ASCII; escapes refused).
pub const TURNSTILE_TOKEN_BYTES: u64 = 2048;
/// The envelope's fixed bytes: `{"text":""` + `,"turnstile":""` + `}`.
pub const ENVELOPE_FIXED_BYTES: u64 = 26;
/// The largest request body that can hold a scorable text.
pub const RAW_BODY_BYTES: u64 = TEXT_FIELD_BYTES + TURNSTILE_TOKEN_BYTES + ENVELOPE_FIXED_BYTES;
/// The scorer's own body limit (`limits::MAX_SCORE_BODY`, 8 MiB): the body
/// forwarded to it is built here and refused above this, never truncated.
pub const SCORER_MAX_BODY: u64 = 8 << 20;
/// Whitespace words below which no score is shown: human false-positive
/// rates were measured only from 150 to 450 words.
pub const MIN_WORDS: usize = 150;
/// The top of the measured length range; longer texts carry a label.
pub const MEASURED_MAX_WORDS: usize = 450;

const _: () = assert!(RAW_BODY_BYTES == 9_427_968 + 2048 + 26);

/// The exact envelope prefix and separators [`ENVELOPE_FIXED_BYTES`] counts.
pub const fn envelope_fixed() -> u64 {
    (r#"{"text":"#.len() + 2 + r#","turnstile":"#.len() + 2 + "}".len()) as u64
}

const _: () = assert!(envelope_fixed() == ENVELOPE_FIXED_BYTES);

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_normalization_alignments::UnicodeNormalization;

    fn u32_at(b: &[u8], at: usize) -> usize {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize
    }
    fn u16_at(b: &[u8], at: usize) -> usize {
        u16::from_le_bytes(b[at..at + 2].try_into().unwrap()) as usize
    }

    /// The added tokens of one section: (content, flags).
    fn added(b: &[u8], mut at: usize, n: usize) -> (Vec<(Vec<u8>, u8)>, usize) {
        let mut out = Vec::new();
        for _ in 0..n {
            let len = u16_at(b, at);
            let flags = b[at + 2];
            out.push((b[at + 3..at + 3 + len].to_vec(), flags));
            at += 3 + len;
        }
        (out, at)
    }

    /// Token byte lengths from the table layout `scorer_tokens::Table::parse`
    /// documents: byte ids, then merges applied in rank order.
    type Added = Vec<(Vec<u8>, u8)>;

    fn table_lengths() -> (usize, Added, Added, u64) {
        let b = slop_ninja_scorer_tokens::TABLE;
        let frame = u32_at(b, 40) as u64;
        let merges_n = u32_at(b, 44);
        let (raw_n, norm_n) = (u32_at(b, 48), u32_at(b, 56));
        let byte_ids = &b[76..76 + 512];
        let mut len = vec![0usize; 1 << 16];
        for i in 0..256 {
            let id = u16_at(byte_ids, 2 * i);
            if id != 0xffff {
                len[id] = 1;
            }
        }
        let m0 = 76 + 512;
        let mut merges: Vec<(usize, usize, usize, usize)> = (0..merges_n)
            .map(|i| {
                let at = m0 + 8 * i;
                let key = u32_at(b, at);
                (u16_at(b, at + 4), key >> 16, key & 0xffff, u16_at(b, at + 6))
            })
            .collect();
        merges.sort();
        for (_, l, r, new) in merges {
            assert!(len[l] > 0 && len[r] > 0, "a merge uses an id made by a later rank");
            len[new] = len[l] + len[r];
        }
        let (raw, at) = added(b, m0 + 8 * merges_n, raw_n);
        let (norm, _) = added(b, at, norm_n);
        (*len.iter().max().unwrap(), raw, norm, frame)
    }

    #[test]
    fn the_token_bound_is_the_tables() {
        let (merged, raw, norm, frame) = table_lengths();
        assert_eq!(frame, FRAME_TOKENS);
        assert_eq!(slop_ninja_scorer_tokens::frame_tokens().unwrap(), FRAME_TOKENS);
        assert_eq!(merged as u64, MAX_TOKEN_TEXT_BYTES, "longest merged vocabulary entry");
        for (content, _) in raw.iter().chain(&norm) {
            assert!(content.len() as u64 <= MAX_TOKEN_TEXT_BYTES);
        }
    }

    /// The raw added tokens the policy names as literal control markers.
    #[test]
    fn the_marker_list_is_the_tables() {
        let (_, raw, _, _) = table_lengths();
        let mut table: Vec<String> = raw.iter().map(|(c, _)| String::from_utf8(c.clone()).unwrap()).collect();
        table.sort();
        let mut ours: Vec<String> = crate::policy::MARKERS.iter().map(|s| s.to_string()).collect();
        ours.sort();
        assert_eq!(table, ours);
    }

    fn esc(c: char) -> u64 {
        if (c as u32) > 0xFFFF { 12 } else { 6 }
    }

    /// The worst escaped-input-to-NFC-output ratio over every code point:
    /// a character written as its full canonical decomposition, each part a
    /// `\u` escape, and a character written alone as an escape.
    #[test]
    fn the_escape_factor_is_the_nfc_crates() {
        let mut worst = (0u64, 1u64, '\0');
        for c in (0u32..=0x10FFFF).filter_map(char::from_u32) {
            let s = c.to_string();
            let nfc: String = s.as_str().nfc().map(|(ch, _)| ch).collect();
            let out = nfc.len() as u64;
            let decomposed: u64 = s.as_str().nfd().map(|(ch, _)| esc(ch)).sum();
            for input in [decomposed, esc(c)] {
                if input * worst.1 > worst.0 * out {
                    worst = (input, out, c);
                }
            }
        }
        let factor = worst.0.div_ceil(worst.1);
        assert_eq!(factor, NFC_ESCAPE_FACTOR, "worst case U+{:04X}: {} / {}", worst.2 as u32, worst.0, worst.1);
    }

    #[test]
    fn the_text_factor_is_the_nfc_crates() {
        let mut worst = (0u64, 1u64);
        for c in (0u32..=0x10FFFF).filter_map(char::from_u32) {
            let s = c.to_string();
            let out = s.as_str().nfc().map(|(ch, _)| ch.len_utf8() as u64).sum::<u64>();
            let decomposed: u64 = s.as_str().nfd().map(|(ch, _)| ch.len_utf8() as u64).sum();
            for input in [decomposed, s.len() as u64] {
                if input * worst.1 > worst.0 * out {
                    worst = (input, out);
                }
            }
        }
        assert_eq!(worst.0.div_ceil(worst.1), TEXT_BYTES_PER_NFC_BYTE, "{worst:?}");
    }

    #[test]
    fn the_forwarded_body_fits_the_scorer_when_tokens_fit() {
        // Every forwarded text has at most CONTENT_TOKENS × 512 bytes of NFC
        // text; its raw form is at most 3 bytes per NFC byte (a composed
        // character never has more than three times its bytes in
        // decomposed UTF-8), each escaped at most 6 bytes per byte by the
        // minimal JSON escapes, but the Worker measures the body it builds
        // and refuses above SCORER_MAX_BODY rather than relying on this.
        const { assert!(CONTENT_TOKENS * MAX_TOKEN_TEXT_BYTES <= SCORER_MAX_BODY) };
    }
}
