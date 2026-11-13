//! The two cookies, both host-only on the site host.
//!
//! | Cookie | Attributes | Content |
//! |---|---|---|
//! | `__Host-sn_session` | `Secure; HttpOnly; SameSite=Lax; Path=/`, no `Domain` | 256-bit token; storage holds its SHA-256 |
//! | `__Host-sn_flow` | the same | random binding for one sign-in flow or one pending link |
//!
//! The `__Host-` prefix keeps both off the secret host and stops a subdomain
//! from planting them. `SameSite=Lax`, not `Strict`, because an OAuth
//! callback is a cross-site top-level `GET` that must carry the flow cookie.
//! No other cookie is set by SlopNinja's code.

use crate::token::Token;

/// The session cookie's name.
pub const SESSION: &str = "__Host-sn_session";
/// The flow cookie's name.
pub const FLOW: &str = "__Host-sn_flow";

/// The longest `Cookie` header read; past it the request is treated as
/// carrying neither cookie (a browser keeps one cookie under 4 KiB, and
/// this host sets two of 43-character values).
pub const COOKIE_HEADER_MAX: usize = 16 * 1024;

/// A `Set-Cookie` value carrying `token` for `max_age_s` seconds.
#[must_use]
pub fn set(name: &str, token: &Token, max_age_s: u64) -> String {
    format!(
        "{name}={}; Max-Age={max_age_s}; Path=/; Secure; HttpOnly; SameSite=Lax",
        token.encode()
    )
}

/// A `Set-Cookie` value clearing `name`.
#[must_use]
pub fn clear(name: &str) -> String {
    format!("{name}=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Lax")
}

/// What a `Cookie` header holds for one name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup<'a> {
    /// Not present.
    Absent,
    /// Present once.
    One(&'a str),
    /// Present more than once: treated as absent, never resolved by order.
    Ambiguous,
}

/// Looks `name` up in a `Cookie` header.
#[must_use]
pub fn lookup<'a>(header: &'a str, name: &str) -> Lookup<'a> {
    if header.len() > COOKIE_HEADER_MAX {
        return Lookup::Absent;
    }
    let mut found = Lookup::Absent;
    for pair in header.split(';') {
        let pair = pair.trim_matches(' ');
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if k == name {
            found = match found {
                Lookup::Absent => Lookup::One(v),
                _ => return Lookup::Ambiguous,
            };
        }
    }
    found
}

/// The value of `name` when it is present exactly once.
#[must_use]
pub fn get<'a>(header: Option<&'a str>, name: &str) -> Option<&'a str> {
    match lookup(header?, name) {
        Lookup::One(v) => Some(v),
        Lookup::Absent | Lookup::Ambiguous => None,
    }
}

/// The token in `name`, when present exactly once and well formed.
#[must_use]
pub fn token(header: Option<&str>, name: &str) -> Option<Token> {
    Token::parse(get(header, name)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_rules() {
        let h = "a=1; __Host-sn_flow=xyz;__Host-sn_session=abc";
        assert_eq!(lookup(h, FLOW), Lookup::One("xyz"));
        assert_eq!(lookup(h, SESSION), Lookup::One("abc"));
        assert_eq!(lookup(h, "b"), Lookup::Absent);
        assert_eq!(lookup("x=1; x=2", "x"), Lookup::Ambiguous);
        assert_eq!(get(Some("x=1; x=2"), "x"), None);
        assert_eq!(get(None, "x"), None);
        // A name only matches whole.
        assert_eq!(lookup("sn_flow=1; __Host-sn_flowX=2", FLOW), Lookup::Absent);
        let long = format!("{}; {FLOW}=1", "y=1".repeat(COOKIE_HEADER_MAX));
        assert_eq!(lookup(&long, FLOW), Lookup::Absent);
    }
}
