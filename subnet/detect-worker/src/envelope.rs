//! The score request: exactly `{"text":"…","turnstile":"…"}`, as the page's
//! `JSON.stringify` writes it, in either key order.
//!
//! Strict on purpose, so the body bound (`bound::RAW_BODY_BYTES`) is exact:
//! no insignificant whitespace, no other keys, no escapes in keys or in the
//! Turnstile token, and the token is printable ASCII of at most
//! `bound::TURNSTILE_TOKEN_BYTES` bytes. The text is a JSON string: it is
//! borrowed from the body when it holds no escape, and otherwise unescaped
//! once into a buffer no longer than its escaped form. A lone surrogate
//! escape, a raw control character or a malformed escape refuses the
//! request. Nothing here keeps or reports any part of the text.

use std::borrow::Cow;

/// A parsed request.
#[derive(Debug, PartialEq, Eq)]
pub struct Envelope<'a> {
    pub text: Cow<'a, str>,
    pub turnstile: &'a str,
}

/// Why a body is not a request (fixed codes, never echoing input).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformed {
    /// Not the exact envelope shape.
    Shape,
    /// The text is not a valid JSON string of Unicode scalar values.
    Text,
    /// The Turnstile token is missing, escaped, too long or not printable ASCII.
    Token,
}

impl Malformed {
    pub fn code(self) -> &'static str {
        match self {
            Malformed::Shape => "malformed_request",
            Malformed::Text => "invalid_text",
            Malformed::Token => "invalid_token",
        }
    }
}

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
    if body.first() != Some(&b'{') || body.last() != Some(&b'}') {
        return Err(Malformed::Shape);
    }
    let mut text: Option<Cow<'_, str>> = None;
    let mut token: Option<&str> = None;
    let mut at = 1;
    for field in 0..2 {
        if field == 1 {
            if body.get(at) != Some(&b',') {
                return Err(Malformed::Shape);
            }
            at += 1;
        }
        let (key, key_escaped, after) = raw_string(body, at).ok_or(Malformed::Shape)?;
        if key_escaped || body.get(after) != Some(&b':') {
            return Err(Malformed::Shape);
        }
        at = after + 1;
        match key {
            b"text" if text.is_none() => {
                let (raw, escaped, after) = raw_string(body, at).ok_or(Malformed::Text)?;
                text = Some(if escaped {
                    Cow::Owned(unescape(raw).ok_or(Malformed::Text)?)
                } else {
                    Cow::Borrowed(std::str::from_utf8(raw).map_err(|_| Malformed::Text)?)
                });
                at = after;
            }
            b"turnstile" if token.is_none() => {
                let (raw, escaped, after) = raw_string(body, at).ok_or(Malformed::Token)?;
                let ok = !escaped
                    && !raw.is_empty()
                    && raw.len() as u64 <= crate::bound::TURNSTILE_TOKEN_BYTES
                    && raw.iter().all(|c| c.is_ascii_graphic());
                if !ok {
                    return Err(Malformed::Token);
                }
                token = Some(std::str::from_utf8(raw).map_err(|_| Malformed::Token)?);
                at = after;
            }
            _ => return Err(Malformed::Shape),
        }
    }
    if at + 1 != body.len() {
        return Err(Malformed::Shape);
    }
    match (text, token) {
        (Some(text), Some(turnstile)) => Ok(Envelope { text, turnstile }),
        _ => Err(Malformed::Shape),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(text: &str, token: &str) -> String {
        serde_json::json!({"text": text, "turnstile": token}).to_string()
    }

    #[test]
    fn parses_what_json_stringify_writes() {
        for text in ["plain", "tab\tnew\nline \"quoted\" back\\slash", "astral 😀 and é", "\u{1}\u{1f}", ""] {
            let b = body(text, "0.abc_DEF-123");
            let e = parse(b.as_bytes()).unwrap();
            assert_eq!(e.text, text);
            assert_eq!(e.turnstile, "0.abc_DEF-123");
        }
        let swapped = r#"{"turnstile":"tok","text":"hi"}"#;
        assert_eq!(parse(swapped.as_bytes()).unwrap().text, "hi");
    }

    #[test]
    fn borrows_unescaped_text() {
        let b = body("no escapes here", "t");
        assert!(matches!(parse(b.as_bytes()).unwrap().text, Cow::Borrowed(_)));
    }

    #[test]
    fn surrogate_pairs_and_refusals() {
        assert_eq!(parse(br#"{"text":"\ud83d\ude00","turnstile":"t"}"#).unwrap().text, "😀");
        assert_eq!(parse(br#"{"text":"\ud83d","turnstile":"t"}"#), Err(Malformed::Text));
        assert_eq!(parse(br#"{"text":"\ude00","turnstile":"t"}"#), Err(Malformed::Text));
        assert_eq!(parse(br#"{"text":"\x","turnstile":"t"}"#), Err(Malformed::Text));
        assert_eq!(parse(b"{\"text\":\"a\x01b\",\"turnstile\":\"t\"}"), Err(Malformed::Text));
        assert_eq!(parse(br#"{"text":"a","turnstile":"t\u0041"}"#), Err(Malformed::Token));
        assert_eq!(parse(br#"{"text":"a","turnstile":""}"#), Err(Malformed::Token));
        assert_eq!(parse(br#"{"text":"a","turnstile":"t t"}"#), Err(Malformed::Token));
        assert_eq!(parse(br#"{ "text":"a","turnstile":"t"}"#), Err(Malformed::Shape));
        assert_eq!(parse(br#"{"text":"a","turnstile":"t","x":1}"#), Err(Malformed::Shape));
        assert_eq!(parse(br#"{"text":"a","text":"b"}"#), Err(Malformed::Shape));
        assert_eq!(parse(br#"{"te\u0078t":"a","turnstile":"t"}"#), Err(Malformed::Shape));
        assert_eq!(parse(br#"{"text":"a"}"#), Err(Malformed::Shape));
        assert_eq!(parse(b""), Err(Malformed::Shape));
        let long = "x".repeat(crate::bound::TURNSTILE_TOKEN_BYTES as usize + 1);
        assert_eq!(parse(body("a", &long).as_bytes()), Err(Malformed::Token));
    }

    #[test]
    fn the_fixed_bytes_are_the_bounds() {
        let b = body("", "t");
        assert_eq!(b.len() as u64, crate::bound::ENVELOPE_FIXED_BYTES + 1);
    }

    #[test]
    fn invalid_utf8_is_refused() {
        let mut b = br#"{"text":"ab","turnstile":"t"}"#.to_vec();
        b[9] = 0xff;
        assert_eq!(parse(&b), Err(Malformed::Text));
    }
}
