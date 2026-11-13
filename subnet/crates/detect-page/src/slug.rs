//! Slugs and tokens: drawing, the public blocklist, folding and the page
//! paths with their 301 forms.
//!
//! - Every value is lowercase Crockford base32 (`0-9a-z` without `i`, `l`,
//!   `o` and `u`), drawn at exactly 5 bits per character from a uniform byte
//!   stream (most significant bits first), so there is no modulo bias. The
//!   caller supplies the bytes (`crypto.getRandomValues` in a Worker).
//! - Public slugs are 7 characters (35 bits). A draw that contains a
//!   [`BLOCKLIST_V1`] entry is redrawn.
//! - Secret slugs are 20 characters (100 bits, with the server-held
//!   pepper). Manage tokens, removal status tokens and wallet nonces are 26
//!   characters (130 bits).
//! - **Folding.** Uppercase folds to lowercase, `i` and `l` to `1`, `o` to
//!   `0`; a folded path answers 301 to the canonical one. A segment of the
//!   wrong length or with any other character matches nothing (404, with no
//!   store read).

use crate::codes::HostRole;

/// The alphabet, in value order.
pub const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
/// A public page's slug length.
pub const PUBLIC_LEN: usize = 7;
/// A secret page's slug length.
pub const SECRET_LEN: usize = 20;
/// Manage tokens, removal status tokens and wallet nonces.
pub const TOKEN_LEN: usize = 26;
/// The blocklist's version name.
pub const BLOCKLIST_NAME: &str = "slug_blocklist_v1";
/// Substrings a public slug may not contain, in canonical (folded) form. A
/// public slug is read by people, so a draw that spells one of these is
/// redrawn. Secret slugs and tokens are never shown on their own and are not
/// screened. The list changes only by a new version.
pub const BLOCKLIST_V1: &[&str] = &[
    "fck", "fvck", "fxck", "fag", "f4g", "sh1t", "cvnt", "kkk", "n1gg", "n1g3r", "naz1", "sex", "s3x", "p0rn", "pr0n",
    "c0ck", "d1ck", "cvm", "j1zz", "twat", "tw4t", "wh0r", "rape", "r4pe", "ana1", "an41", "b1tch", "wank", "w4nk",
    "s1vt", "dyke", "retard", "r3tard", "t1ts", "kys",
];
/// Consecutive blocked public draws after which the random source is
/// treated as broken. A uniform draw is blocked well under 1% of the time
/// (a test measures it), so 64 in a row do not happen with a working source.
pub const PUBLIC_REDRAWS: usize = 64;

/// The value of an alphabet byte.
pub fn value(c: u8) -> Option<u8> {
    ALPHABET.iter().position(|&a| a == c).map(|p| p as u8)
}

/// Draws `len` characters from `fill`'s bytes, 5 bits each.
pub fn draw(len: usize, fill: &mut dyn FnMut(&mut [u8])) -> String {
    let mut buf = vec![0u8; (len * 5).div_ceil(8)];
    fill(&mut buf);
    let mut out = String::with_capacity(len);
    let (mut acc, mut bits, mut next) = (0u32, 0u32, 0usize);
    while out.len() < len {
        if bits < 5 {
            acc = (acc << 8) | u32::from(buf[next]);
            next += 1;
            bits += 8;
        }
        bits -= 5;
        out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        acc &= (1 << bits) - 1;
    }
    out
}

/// Whether a canonical public slug contains a blocklist entry.
pub fn is_blocked(slug: &str) -> bool {
    BLOCKLIST_V1.iter().any(|w| slug.contains(w))
}

/// The random source returned blocked slugs [`PUBLIC_REDRAWS`] times running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrokenRandom;

/// A public slug: 7 characters, redrawn while it contains a blocklist entry.
pub fn draw_public(fill: &mut dyn FnMut(&mut [u8])) -> Result<String, BrokenRandom> {
    for _ in 0..PUBLIC_REDRAWS {
        let s = draw(PUBLIC_LEN, fill);
        if !is_blocked(&s) {
            return Ok(s);
        }
    }
    Err(BrokenRandom)
}

/// A secret slug: 20 characters.
pub fn draw_secret(fill: &mut dyn FnMut(&mut [u8])) -> String {
    draw(SECRET_LEN, fill)
}

/// A manage token, removal status token or wallet nonce: 26 characters.
pub fn draw_token(fill: &mut dyn FnMut(&mut [u8])) -> String {
    draw(TOKEN_LEN, fill)
}

/// A segment after folding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Folded {
    /// Already canonical.
    Canonical,
    /// Folds to this canonical slug (answer 301).
    Fold(String),
    /// Not a slug of this length.
    Invalid,
}

/// Folds `segment` as a slug of `len` characters.
pub fn fold(segment: &str, len: usize) -> Folded {
    if segment.len() != len {
        return Folded::Invalid;
    }
    let mut out = String::with_capacity(len);
    for b in segment.bytes() {
        let c = match b.to_ascii_lowercase() {
            b'i' | b'l' => b'1',
            b'o' => b'0',
            c => c,
        };
        if value(c).is_none() {
            return Folded::Invalid;
        }
        out.push(c as char);
    }
    if out == segment { Folded::Canonical } else { Folded::Fold(out) }
}

/// Whether `s` is a canonical slug of `len` characters.
pub fn is_canonical(s: &str, len: usize) -> bool {
    fold(s, len) == Folded::Canonical
}

/// The slug length of a host's pages.
pub fn slug_len(host: HostRole) -> usize {
    match host {
        HostRole::Site => PUBLIC_LEN,
        HostRole::Secret => SECRET_LEN,
    }
}

/// What a slug path serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlugRoute {
    /// The page: `/d/<7>` or `/<20>`.
    Page,
    /// Its card: `/d/<7>/card.png` or `/<20>/card.png`.
    Card,
    /// The secret host's manage shell: `/<20>/manage`.
    Manage,
}

/// The result of matching a request path (without its query).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathMatch {
    /// Serve `route` for this canonical slug.
    Serve { route: SlugRoute, slug: String },
    /// Answer 301 to this canonical path (a folded slug, or a trailing
    /// slash).
    Redirect(String),
    /// Not a slug path of this host: the caller's other routes or a 404,
    /// with no store read.
    NoMatch,
}

/// The canonical path of a slug route.
pub fn path(host: HostRole, route: SlugRoute, slug: &str) -> String {
    let base = match host {
        HostRole::Site => format!("/d/{slug}"),
        HostRole::Secret => format!("/{slug}"),
    };
    match route {
        SlugRoute::Page => base,
        SlugRoute::Card => format!("{base}/card.png"),
        SlugRoute::Manage => format!("{base}/manage"),
    }
}

/// Matches a request path on `host` against the slug routes.
pub fn match_path(host: HostRole, path_: &str) -> PathMatch {
    let rest = match host {
        HostRole::Site => match path_.strip_prefix("/d/") {
            Some(r) => r,
            None => return PathMatch::NoMatch,
        },
        HostRole::Secret => match path_.strip_prefix('/') {
            Some(r) => r,
            None => return PathMatch::NoMatch,
        },
    };
    let (segment, tail) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (route, trailing) = match (host, tail) {
        (_, "") => (SlugRoute::Page, false),
        (_, "/") => (SlugRoute::Page, true),
        (_, "/card.png") => (SlugRoute::Card, false),
        (HostRole::Secret, "/manage") => (SlugRoute::Manage, false),
        _ => return PathMatch::NoMatch,
    };
    match fold(segment, slug_len(host)) {
        Folded::Invalid => PathMatch::NoMatch,
        Folded::Canonical if !trailing => PathMatch::Serve { route, slug: segment.to_owned() },
        Folded::Canonical => PathMatch::Redirect(path(host, route, segment)),
        Folded::Fold(c) => PathMatch::Redirect(path(host, route, &c)),
    }
}
