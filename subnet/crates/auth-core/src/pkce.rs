//! PKCE with S256 (RFC 7636), on every OAuth flow.
//!
//! The verifier is 32 random bytes in base64url (43 characters, the RFC's
//! minimum length and its recommended construction). The flow row stores it;
//! the authorize redirect carries only its S256 challenge; the token exchange
//! sends the verifier. [`verify`] is the provider's side of the check, used by
//! the local auth stub and the tests.

use sha2::{Digest, Sha256};

use crate::b64;

/// The only challenge method used: `S256`. `plain` is never sent.
pub const METHOD: &str = "S256";

/// The shortest and longest verifiers RFC 7636 allows.
pub const VERIFIER_LEN: core::ops::RangeInclusive<usize> = 43..=128;

/// A code verifier.
#[derive(Clone, PartialEq, Eq)]
pub struct Verifier(String);

impl Verifier {
    /// A verifier from 32 bytes drawn by the caller from a CSPRNG.
    #[must_use]
    pub fn from_random(bytes: [u8; 32]) -> Self {
        Self(b64::encode(&bytes))
    }

    /// Parses a stored verifier: 43 to 128 characters of the RFC's
    /// unreserved set (`A-Z a-z 0-9 - . _ ~`).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let ok = VERIFIER_LEN.contains(&text.len())
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'));
        ok.then(|| Self(text.to_owned()))
    }

    /// The verifier text, sent once in the token exchange.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The S256 challenge sent in the authorize redirect.
    #[must_use]
    pub fn challenge(&self) -> String {
        challenge_s256(&self.0)
    }
}

impl core::fmt::Debug for Verifier {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Verifier(..)")
    }
}

/// `BASE64URL(SHA256(ASCII(verifier)))`.
#[must_use]
pub fn challenge_s256(verifier: &str) -> String {
    b64::encode(&Sha256::digest(verifier.as_bytes()))
}

/// The authorization server's check: `verifier` must be well formed and hash
/// to `challenge` under S256.
#[must_use]
pub fn verify(verifier: &str, challenge: &str) -> bool {
    Verifier::parse(verifier).is_some()
        && crate::ct_eq(challenge_s256(verifier).as_bytes(), challenge.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 appendix B, in the octet form the RFC also gives: the
    /// verifier is the base64url of these 32 octets, and its challenge is
    /// the base64url of the SHA-256 octets below.
    #[test]
    fn rfc7636_appendix_b() {
        const VERIFIER_OCTETS: [u8; 32] = [
            116, 24, 223, 180, 151, 153, 224, 37, 79, 250, 96, 125, 216, 173, 187, 186, 22, 212,
            37, 77, 105, 214, 191, 240, 91, 88, 5, 88, 83, 132, 141, 121,
        ];
        const CHALLENGE_OCTETS: [u8; 32] = [
            19, 211, 30, 150, 26, 26, 216, 236, 47, 22, 177, 12, 76, 152, 46, 8, 118, 168, 120,
            173, 109, 241, 68, 86, 110, 225, 137, 74, 203, 112, 249, 195,
        ];
        let v = Verifier::from_random(VERIFIER_OCTETS);
        assert_eq!(v.as_str().len(), 43);
        let challenge = b64::encode(&CHALLENGE_OCTETS);
        assert_eq!(v.challenge(), challenge);
        assert!(verify(v.as_str(), &challenge));
    }

    #[test]
    fn mismatch_and_malformed_refused() {
        let a = Verifier::from_random([1; 32]);
        let b = Verifier::from_random([2; 32]);
        assert_eq!(a.as_str().len(), 43);
        assert!(verify(a.as_str(), &a.challenge()));
        assert!(!verify(b.as_str(), &a.challenge()));
        // `plain` (the verifier itself as the challenge) never passes.
        assert!(!verify(a.as_str(), a.as_str()));
        assert!(Verifier::parse("short").is_none());
        assert!(Verifier::parse(&"a".repeat(129)).is_none());
        assert!(Verifier::parse(&format!("{}+", "a".repeat(43))).is_none());
        assert!(!verify(&"a".repeat(42), &challenge_s256(&"a".repeat(42))));
    }
}
