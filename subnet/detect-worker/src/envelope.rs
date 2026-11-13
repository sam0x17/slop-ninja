//! The score request: exactly `{"text":"…"}`, as the page's
//! `JSON.stringify({text})` writes it.
//!
//! Strict on purpose, so the body bound (`bound::RAW_BODY_BYTES`) is exact:
//! no insignificant whitespace, no other key, no escape in the key. The text
//! is a JSON string: it is borrowed from the body when it holds no escape,
//! and otherwise unescaped once into a buffer no longer than its escaped
//! form. A lone surrogate escape, a raw control character or a malformed
//! escape refuses the request. Nothing here keeps or reports any part of the
//! text.

use slop_ninja_detect_page::RefusalCode;
use std::borrow::Cow;

/// A parsed request.
#[derive(Debug, PartialEq, Eq)]
pub struct Envelope<'a> {
    pub text: Cow<'a, str>,
}

/// Why a body is not a request (fixed codes, never echoing input).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformed {
    /// Not the exact envelope shape.
    Shape,
    /// The text is not a valid JSON string of Unicode scalar values.
    Text,
}

impl Malformed {
    /// `malformed_request` or `bad_text` (both 400).
    pub fn code(self) -> RefusalCode {
        match self {
            Malformed::Shape => RefusalCode::MalformedRequest,
            Malformed::Text => RefusalCode::BadText,
        }
    }
}

/// The envelope's opening, up to the text's opening quote.
const OPEN: &[u8] = br#"{"text":"#;

/// Splits one JSON string starting at `at` (on its opening quote): returns
/// the raw contents, whether it held an escape, and the index after the
/// closing quote.
fn raw_string(b: &[u8], at: usize) -> Option<(&[u8], bool, usize)> {
    if b.get(at) != Some(&b'"') {
        return None;
    }
    let start = at + 1;
    let mut i = start;
    let mut escaped = false;
    loop {
        match *b.get(i)? {
            b'"' => return Some((b.get(start..i)?, escaped, i + 1)),
            b'\\' => {
                escaped = true;
                i += 2;
            }
            c if c < 0x20 => return None,
            _ => i += 1,
        }
    }
}

fn hex4(b: &[u8]) -> Option<u32> {
    if b.len() != 4 {
        return None;
    }
    b.iter().try_fold(0u32, |acc, c| Some(acc * 16 + char::from(*c).to_digit(16)?))
}

/// Unescapes a JSON string's raw contents (already split by [`raw_string`]).
fn unescape(raw: &[u8]) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let rest = raw.get(i..)?;
        let next = rest.iter().position(|c| *c == b'\\').unwrap_or(rest.len());
        out.push_str(std::str::from_utf8(rest.get(..next)?).ok()?);
        i += next;
        if i >= raw.len() {
            break;
        }
        let c = *raw.get(i + 1)?;
        i += 2;
        let ch = match c {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let hi = hex4(raw.get(i..i + 4)?)?;
                i += 4;
                if (0xD800..0xDC00).contains(&hi) {
                    if raw.get(i..i + 2)? != b"\\u" {
                        return None;
                    }
                    let lo = hex4(raw.get(i + 2..i + 6)?)?;
                    if !(0xDC00..0xE000).contains(&lo) {
                        return None;
                    }
                    i += 6;
                    char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00))?
                } else {
                    char::from_u32(hi)?
                }
            }
            _ => return None,
        };
        out.push(ch);
    }
    Some(out)
}

/// Parses the request body.
pub fn parse(body: &[u8]) -> Result<Envelope<'_>, Malformed> {
    if !body.starts_with(OPEN) || body.get(OPEN.len()) != Some(&b'"') || body.last() != Some(&b'}') {
        return Err(Malformed::Shape);
    }
    let (raw, escaped, after) = raw_string(body, OPEN.len()).ok_or(Malformed::Text)?;
    if after + 1 != body.len() {
        return Err(Malformed::Shape);
    }
    let text = if escaped {
        Cow::Owned(unescape(raw).ok_or(Malformed::Text)?)
    } else {
        Cow::Borrowed(std::str::from_utf8(raw).map_err(|_| Malformed::Text)?)
    };
    Ok(Envelope { text })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(text: &str) -> String {
        serde_json::json!({ "text": text }).to_string()
    }

    #[test]
    fn parses_what_json_stringify_writes() {
        for text in ["plain", "tab\tnew\nline \"quoted\" back\\slash", "astral 😀 and é", "\u{1}\u{1f}", ""] {
            let b = body(text);
            assert_eq!(parse(b.as_bytes()).unwrap().text, text);
        }
    }

    #[test]
    fn borrows_unescaped_text() {
        let b = body("no escapes here");
        assert!(matches!(parse(b.as_bytes()).unwrap().text, Cow::Borrowed(_)));
    }

    #[test]
    fn surrogate_pairs_and_refusals() {
        assert_eq!(parse(br#"{"text":"\ud83d\ude00"}"#).unwrap().text, "😀");
        assert_eq!(parse(br#"{"text":"\ud83d"}"#), Err(Malformed::Text));
        assert_eq!(parse(br#"{"text":"\ude00"}"#), Err(Malformed::Text));
        assert_eq!(parse(br#"{"text":"\x"}"#), Err(Malformed::Text));
        assert_eq!(parse(b"{\"text\":\"a\x01b\"}"), Err(Malformed::Text));
        assert_eq!(parse(br#"{"text":"a"#), Err(Malformed::Shape));
        assert_eq!(parse(br#"{"text":"a}"#), Err(Malformed::Text));
        assert_eq!(parse(br#"{ "text":"a"}"#), Err(Malformed::Shape));
        assert_eq!(parse(br#"{"text": "a"}"#), Err(Malformed::Shape));
        assert_eq!(parse(br#"{"text":"a" }"#), Err(Malformed::Shape));
        assert_eq!(parse(br#"{"text":"a","x":1}"#), Err(Malformed::Shape));
        assert_eq!(parse(br#"{"text":"a","text":"b"}"#), Err(Malformed::Shape));
        assert_eq!(parse(br#"{"te\u0078t":"a"}"#), Err(Malformed::Shape));
        assert_eq!(parse(br#"{"x":"a","text":"b"}"#), Err(Malformed::Shape));
        assert_eq!(parse(br#"{"text":1}"#), Err(Malformed::Shape));
        assert_eq!(parse(br#"{}"#), Err(Malformed::Shape));
        assert_eq!(parse(b""), Err(Malformed::Shape));
    }

    /// The fields of the envelope that carried a challenge token are gone:
    /// a body with one is not a request.
    #[test]
    fn a_turnstile_field_is_refused() {
        assert_eq!(parse(br#"{"text":"a","turnstile":"t"}"#), Err(Malformed::Shape));
        assert_eq!(parse(br#"{"turnstile":"t","text":"a"}"#), Err(Malformed::Shape));
    }

    #[test]
    fn the_fixed_bytes_are_the_bounds() {
        let b = body("");
        assert_eq!(b.len() as u64, crate::bound::ENVELOPE_FIXED_BYTES);
        assert_eq!(OPEN.len() + 2 + 1, b.len());
    }

    #[test]
    fn invalid_utf8_is_refused() {
        let mut b = br#"{"text":"ab"}"#.to_vec();
        b[9] = 0xff;
        assert_eq!(parse(&b), Err(Malformed::Text));
    }

    #[test]
    fn codes() {
        assert_eq!(Malformed::Shape.code().as_str(), "malformed_request");
        assert_eq!(Malformed::Text.code().as_str(), "bad_text");
        assert_eq!(Malformed::Text.code().status(), 400);
    }
}
