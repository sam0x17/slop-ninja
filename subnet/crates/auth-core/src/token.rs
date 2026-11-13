//! 256-bit random tokens and their stored hashes.
//!
//! One type serves every random value the sign-in flows hand to a browser or
//! a mailbox: the OAuth `state`, the OIDC `nonce`, the `__Host-sn_flow`
//! binding, the `__Host-sn_session` token, the email link token and the
//! wallet challenge id. Each is 32 bytes from the caller's CSPRNG, written as
//! 43 characters of base64url without padding. Storage holds only
//! [`Token::hash`], the SHA-256 of the 32 bytes, so a database read never
//! yields a usable token.

use sha2::{Digest, Sha256};

use crate::b64;

/// The length of a token's text form: 32 bytes in base64url, no padding.
pub const TEXT_LEN: usize = 43;

/// A 256-bit random token.
#[derive(Clone, PartialEq, Eq)]
pub struct Token([u8; 32]);

impl Token {
    /// Wraps 32 bytes drawn by the caller from a CSPRNG.
    #[must_use]
    pub const fn from_random(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Parses the text form. Anything but exactly 43 canonical base64url
    /// characters is refused.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() != TEXT_LEN {
            return None;
        }
        let bytes = b64::decode(text)?;
        let bytes: [u8; 32] = bytes.try_into().ok()?;
        Some(Self(bytes))
    }

    /// The text form handed to the browser or put in a mail.
    #[must_use]
    pub fn encode(&self) -> String {
        b64::encode(&self.0)
    }

    /// The stored form: SHA-256 of the 32 bytes.
    #[must_use]
    pub fn hash(&self) -> [u8; 32] {
        Sha256::digest(self.0).into()
    }

    /// The stored form, as lower-case hex.
    #[must_use]
    pub fn hash_hex(&self) -> String {
        hex::encode(self.hash())
    }

    /// Whether this token hashes to `stored`, compared in constant time.
    #[must_use]
    pub fn matches(&self, stored: &[u8; 32]) -> bool {
        crate::ct_eq(&self.hash(), stored)
    }
}

impl core::fmt::Debug for Token {
    // A token is a credential: never print it.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Token(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_form_is_strict() {
        let t = Token::from_random([7; 32]);
        let text = t.encode();
        assert_eq!(text.len(), TEXT_LEN);
        assert_eq!(Token::parse(&text), Some(t.clone()));
        assert_eq!(Token::parse(&text[..42]), None);
        assert_eq!(Token::parse(&format!("{text}A")), None);
        assert_eq!(Token::parse(&format!("{text}=")), None);
        let mut plus = text.clone();
        plus.replace_range(0..1, "+");
        assert_eq!(Token::parse(&plus), None);
        assert!(t.matches(&t.hash()));
        assert!(!Token::from_random([8; 32]).matches(&t.hash()));
        assert_eq!(format!("{t:?}"), "Token(..)");
    }
}
