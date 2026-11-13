//! Base64url without padding (RFC 4648 section 5), strict on decode.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Encodes bytes as base64url without padding.
pub(crate) fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes base64url without padding. Padding, the standard alphabet, white
/// space and non-canonical trailing bits are refused, so one byte string has
/// exactly one accepted text form.
pub(crate) fn decode(text: &str) -> Option<Vec<u8>> {
    let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
    // The engine refuses non-zero trailing bits by default; re-encoding
    // makes the canonical-form rule explicit and independent of that default.
    (encode(&bytes) == text).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_strictness() {
        assert_eq!(encode(b"\xff\xfe"), "__4");
        assert_eq!(decode("__4").as_deref(), Some(&b"\xff\xfe"[..]));
        assert_eq!(decode("__4="), None);
        assert_eq!(decode("//4"), None);
        assert_eq!(decode("__5"), None);
        assert_eq!(decode(" __4"), None);
        assert_eq!(decode(""), Some(Vec::new()));
    }
}
