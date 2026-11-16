//! Slugs and tokens: drawing, the public blocklist, folding and the page
//! paths with their 301 forms.
//!
//! - Every value is lowercase Crockford base32 (`0-9a-z` without `i`, `l`,
//!   `o` and `u`), drawn at exactly 5 bits per character from a uniform byte
//!   stream (most significant bits first), so there is no modulo bias. The
//!   caller supplies the bytes (`crypto.getRandomValues` in a Worker).
//! - Public slugs are 7 characters (35 bits). A draw that contains a
//!   [`BLOCKLIST_V1`] entry is redrawn.
//! - Drawn secret slugs (paid pages, and pasted-text pages made before
//!   `permalink-policy/v1`) are 20 characters (100 bits, with the
//!   server-held pepper). Manage tokens, removal status tokens and wallet
//!   nonces are 26 characters (130 bits).
//! - **Text slugs** (`permalink-policy/v1`): a pasted text's page has a
//!   slug of [`TEXT_MIN_LEN`] to [`TEXT_MAX_LEN`] base62 characters
//!   ([`ALPHABET62`]), derived by the detect Worker from a keyed hash of the
//!   text ([`text_chars`] is the character rule). It is case-sensitive and
//!   never folded. The two secret forms cannot meet: text slugs stop at 19
//!   characters, drawn ones are 20.
//! - A new text's slug has [`TEXT_DEFAULT_LEN`] characters (10: 62^10, about
//!   8.4 × 10^17 or 59.5 bits), and a text keeps the length it got, so a page
//!   made at 8 keeps its link. An unlisted page is not unguessable: with a
//!   million pages, one network trying 1,000 slugs a second for a year finds
//!   about 0.04 of them, many networks together find more, and nothing limits
//!   wrong guesses.
//! - **Folding** (the Crockford forms only). Uppercase folds to lowercase,
//!   `i` and `l` to `1`, `o` to `0`; a folded path answers 301 to the
//!   canonical one. A segment of the wrong length or with any other
//!   character matches nothing (404, with no store read).

use crate::codes::HostRole;

/// The alphabet, in value order.
pub const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
/// A public page's slug length.
pub const PUBLIC_LEN: usize = 7;
/// A drawn secret page's slug length (paid pages; pasted-text pages made
/// before `permalink-policy/v1`).
pub const SECRET_LEN: usize = 20;
/// A text slug's alphabet (base62), in value order.
pub const ALPHABET62: &[u8; 62] = &alphabet62();

/// `0-9`, then `A-Z`, then `a-z`.
const fn alphabet62() -> [u8; 62] {
    let mut out = [0u8; 62];
    let mut i = 0;
    while i < 62 {
        out[i] = match i {
            0..=9 => b'0' + i as u8,
            10..=35 => b'A' + (i - 10) as u8,
            _ => b'a' + (i - 36) as u8,
        };
        i += 1;
    }
    out
}

/// The shortest text slug (`permalink-policy/v1`'s floor, which pages made
/// at 8 characters keep).
pub const TEXT_MIN_LEN: usize = 8;
/// The longest text slug: one short of [`SECRET_LEN`], so the two secret
/// forms never meet.
pub const TEXT_MAX_LEN: usize = 19;
/// The policy's default length (`DETECT_TEXT_SLUG_LEN`): 10 characters,
/// about 59.5 bits.
pub const TEXT_DEFAULT_LEN: usize = 10;
/// The derivation keeps a stream byte below this bound only: 248 = 4 × 62,
/// so every character of [`ALPHABET62`] is equally likely.
pub const TEXT_BYTE_BOUND: u8 = 248;
/// Segments of the secret host that are not page slugs, though they have a
/// text slug's form: a derived slug equal to one (in any case) is skipped
/// for the next length, and a path segment equal to one never reads the
/// store.
pub const RESERVED_SEGMENTS: &[&str] = &[
    "manifest",
    "security",
    "internal",
    "accounts",
    "settings",
    "download",
    "detections",
    "wellknown",
    "healthcheck",
    "robotstxt",
    "sitemaps",
    "favicons",
];
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

/// The characters of a text slug from a derivation stream: each byte below
/// [`TEXT_BYTE_BOUND`] gives `ALPHABET62[b mod 62]`, and every other byte is
/// skipped, so no character is biased. A slug of length L is the first L
/// characters, so a shorter candidate is a prefix of a longer one.
pub fn text_chars(stream: &[u8]) -> String {
    stream.iter().filter(|&&b| b < TEXT_BYTE_BOUND).map(|&b| ALPHABET62[usize::from(b % 62)] as char).collect()
}

/// Whether `s` is a reserved segment of the secret host (any case).
pub fn is_reserved(s: &str) -> bool {
    RESERVED_SEGMENTS.iter().any(|r| r.eq_ignore_ascii_case(s))
}

/// Whether `s` has a text slug's form: [`TEXT_MIN_LEN`] to
/// [`TEXT_MAX_LEN`] base62 characters, and not a reserved segment. Case
/// matters; nothing folds.
pub fn is_text_slug(s: &str) -> bool {
    (TEXT_MIN_LEN..=TEXT_MAX_LEN).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric()) && !is_reserved(s)
}

/// Whether `s` is a secret page's slug in either form: a text slug, or a
/// canonical 20-character drawn slug.
pub fn is_secret_slug(s: &str) -> bool {
    is_text_slug(s) || is_canonical(s, SECRET_LEN)
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

/// The slug length of a host's pages (on the secret host, the drawn form;
/// a text slug is [`is_text_slug`]).
pub fn slug_len(host: HostRole) -> usize {
    match host {
        HostRole::Site => PUBLIC_LEN,
        HostRole::Secret => SECRET_LEN,
    }
}

/// What a slug path serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlugRoute {
    /// The page: `/d/<7>`, or `/<slug>` on the secret host (a text slug or
    /// a 20-character drawn one).
    Page,
    /// Its card: `/d/<7>/card.png` or `/<slug>/card.png`.
    Card,
    /// The secret host's manage shell: `/<slug>/manage`.
    Manage,
    /// An X page with X's own widget, loaded on the reader's click:
    /// `/d/<7>/x` on the site host (`noindex`, canonical to `/d/<7>`, its own
    /// cache key and CSP). The only route that loads X's script.
    XWidget,
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
        SlugRoute::XWidget => format!("{base}/x"),
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
        (HostRole::Site, "/x") => (SlugRoute::XWidget, false),
        _ => return PathMatch::NoMatch,
    };
    // A text slug is looked up exactly as it is: case-sensitive, never
    // folded.
    if host == HostRole::Secret && is_text_slug(segment) {
        return if trailing {
            PathMatch::Redirect(path(host, route, segment))
        } else {
            PathMatch::Serve { route, slug: segment.to_owned() }
        };
    }
    match fold(segment, slug_len(host)) {
        Folded::Invalid => PathMatch::NoMatch,
        Folded::Canonical if !trailing => PathMatch::Serve { route, slug: segment.to_owned() },
        Folded::Canonical => PathMatch::Redirect(path(host, route, segment)),
        Folded::Fold(c) => PathMatch::Redirect(path(host, route, &c)),
    }
}
