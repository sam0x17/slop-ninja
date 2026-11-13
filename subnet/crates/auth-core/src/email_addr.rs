//! Email addresses: ASCII rules, A-label domains and `email_norm`.
//!
//! [`parse`] accepts an address only when:
//!
//! - after trimming ASCII white space at both ends, it is at most 254 octets
//!   and holds no control character;
//! - it has exactly one `@`;
//! - the local part is an ASCII dot-atom (RFC 5321 `atext` and single inner
//!   dots) of at most 64 octets: no quoted strings, comments or non-ASCII
//!   characters (SMTPUTF8 is not accepted);
//! - the domain is not an IP literal, goes through UTS 46 to A-labels and
//!   comes out as at least two ASCII LDH labels, the last not all digits.
//!
//! `email_norm` is the ASCII-lowercased local part, `@`, and the lower-case
//! A-label domain. Nothing else is folded: no Unicode normalization or case
//! mapping in the local part (so the Kelvin sign U+212A, a full-width letter,
//! a dotless ı or a long s U+017F can never fold into an ASCII letter, they
//! are refused), and no dot or plus folding. `email_norm` is the identity's
//! subject **and the only address mail is ever sent to**: [`EmailNorm`] is
//! the type the mailer takes as its recipient, and it is built only here.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use slop_ninja_detect_page::RefusalCode;

/// The longest address accepted, in octets (RFC 5321's path limit less the
/// angle brackets).
pub const ADDRESS_MAX: usize = 254;
/// The longest local part, in octets (RFC 5321).
pub const LOCAL_MAX: usize = 64;

/// A normalized address: the identity subject and the only recipient form.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EmailNorm(String);

impl EmailNorm {
    /// The normalized text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Re-validates a stored subject (for example `email_tokens.email_norm`
    /// read back from D1): it must parse and already be in normal form.
    #[must_use]
    pub fn from_stored(stored: &str) -> Option<Self> {
        let parsed = parse(stored).ok()?;
        (parsed.norm.0 == stored).then_some(parsed.norm)
    }

    /// `addr_h = HMAC-SHA-256(K_addr, email_norm)`: the key under which
    /// spacing, quotas and suppression are counted, so those tables hold no
    /// address.
    #[must_use]
    pub fn keyed_hash(&self, k_addr: &[u8]) -> [u8; 32] {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(k_addr)
            .expect("HMAC accepts a key of any length");
        mac.update(self.0.as_bytes());
        mac.finalize().into_bytes().into()
    }
}

/// An accepted address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmailAddr {
    /// The normal form.
    pub norm: EmailNorm,
    /// The trimmed form as typed, shown only on its holder's own pages
    /// (inside `<bdi>`), never a recipient and never in a mail.
    pub typed: String,
}

impl EmailAddr {
    /// The normalized domain (A-labels, lower case).
    #[must_use]
    pub fn domain(&self) -> &str {
        self.norm.0.rsplit_once('@').map_or("", |(_, d)| d)
    }
}

/// Why an address was refused. Every refusal answers the reader like an
/// accepted address and sends nothing; only the `auth` metrics point
/// records it, content-free ([`EmailError::code`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmailError {
    /// Empty, or longer than [`ADDRESS_MAX`].
    Length,
    /// A control character.
    Control,
    /// Not exactly one `@`.
    At,
    /// The local part is not an ASCII dot-atom of at most [`LOCAL_MAX`].
    Local,
    /// The domain fails UTS 46, is an IP literal or address, or is not LDH.
    Domain,
}

impl EmailError {
    /// The `reason` of the `auth` point (`step` `failed`):
    /// `malformed_request`. It is never answered: the reader gets the same
    /// 303 or 202 as for an accepted address.
    #[must_use]
    pub const fn code(self) -> RefusalCode {
        match self {
            Self::Length | Self::Control | Self::At | Self::Local | Self::Domain => {
                RefusalCode::MalformedRequest
            }
        }
    }
}

/// Parses and normalizes an address (see the module text for the rules).
pub fn parse(input: &str) -> Result<EmailAddr, EmailError> {
    let typed = input.trim_matches(|c: char| c.is_ascii_whitespace());
    if typed.is_empty() || typed.len() > ADDRESS_MAX {
        return Err(EmailError::Length);
    }
    if typed.chars().any(char::is_control) {
        return Err(EmailError::Control);
    }
    let mut parts = typed.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(EmailError::At);
    };
    if !is_dot_atom(local) {
        return Err(EmailError::Local);
    }
    let domain = ascii_domain(domain).ok_or(EmailError::Domain)?;
    let norm = format!("{}@{domain}", local.to_ascii_lowercase());
    if norm.len() > ADDRESS_MAX {
        return Err(EmailError::Length);
    }
    Ok(EmailAddr {
        norm: EmailNorm(norm),
        typed: typed.to_owned(),
    })
}

/// RFC 5321 `atext`: ASCII letters, digits and ``!#$%&'*+-/=?^_`{|}~``.
fn is_atext(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~".contains(&b)
}

fn is_dot_atom(local: &str) -> bool {
    !local.is_empty()
        && local.len() <= LOCAL_MAX
        && local
            .split('.')
            .all(|atom| !atom.is_empty() && atom.bytes().all(is_atext))
}

fn ascii_domain(domain: &str) -> Option<String> {
    if domain.is_empty() || domain.starts_with('[') {
        return None;
    }
    let ascii = idna::domain_to_ascii_strict(domain)
        .ok()?
        .to_ascii_lowercase();
    let labels: Vec<&str> = ascii.split('.').collect();
    let ok = labels.len() >= 2
        && ascii.len() <= 253
        && labels.iter().all(|l| {
            (1..=63).contains(&l.len())
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !l.starts_with('-')
                && !l.ends_with('-')
        })
        && !labels
            .last()
            .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()));
    ok.then_some(ascii)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_atom_rules() {
        assert!(is_dot_atom("a.b+c"));
        assert!(!is_dot_atom(".a"));
        assert!(!is_dot_atom("a."));
        assert!(!is_dot_atom("a..b"));
        assert!(!is_dot_atom("\"a b\""));
        assert!(!is_dot_atom("a(comment)"));
        assert!(!is_dot_atom(&"a".repeat(65)));
    }

    #[test]
    fn keyed_hash_depends_on_key_and_norm() {
        let a = parse("Kate@Example.com").unwrap().norm;
        let b = parse("kate@example.com").unwrap().norm;
        assert_eq!(a.keyed_hash(b"k1"), b.keyed_hash(b"k1"));
        assert_ne!(a.keyed_hash(b"k1"), a.keyed_hash(b"k2"));
    }
}
