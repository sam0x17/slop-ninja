//! Posts on X: the canonical post URL and the two types that carry it.
//!
//! A link to an X post is checked as that post. X's API supplies the text
//! that is scored, and the page is a public URL page (`kind = public_url`,
//! `platform = x`) that keeps no post text and shows none. The default page
//! is link-only: our result and "View on X ↗", a plain link to
//! [`XPost::post_url`]. It carries no quote, handle, author, avatar, media or
//! X mark, and building it makes no request to X (D1 and R2 alone). X's own
//! widget loads only on `/d/<7>/x`, after the reader clicks through the
//! notice on `/d/<7>`, on a sandboxed route.
//!
//! The types:
//! - [`PostUrl`]: the page's link to the post, in the canonical form
//!   `https://x.com/i/status/<id>` only. Its `Deserialize` refuses every other
//!   string, so a renderer that holds one holds a canonical X link.
//! - [`XPost`]: the page field `x` of a live X page, `{post_url}`.
//! - [`XPostUrl`]: an X post's link that travels beside a signed op (`x_url`
//!   of [`crate::api::OpRequest`]) and is never kept. It implements neither
//!   `Serialize` nor `Display`, and its `Debug` prints no digits, so it cannot
//!   reach a journal row, a queue message, a log line or an AE point.
//!
//! What this module does not do: recognise an X post in arbitrary input (rule
//! `x_post_v1`, crate `detect-url`, which turns every accepted form into this
//! canonical one).

use serde::{Deserialize, Serialize};

/// The canonical form of a post's URL: this prefix and the post id. It names
/// no handle, because handles change.
pub const X_CANONICAL_PREFIX: &str = "https://x.com/i/status/";
/// The `sources.host` of every X source.
pub const X_HOST: &str = "x.com";
/// The prefix of an X source's key: `sources.canon_key` is
/// `H("source_v1" ‖ "x:status:<id>")`.
pub const X_SOURCE_KEY_PREFIX: &str = "x:status:";
/// The most digits of a post id (ids are below 2^63).
pub const MAX_ID_DIGITS: usize = 19;

/// Whether X can have issued `id`: 1 to 2^63 - 1.
fn valid_id(id: u64) -> bool {
    id != 0 && id <= i64::MAX as u64
}

/// The canonical URL of post `id`, or `None` for 0 and ids of 2^63 and
/// above, which X never issues.
pub fn x_canonical(id: u64) -> Option<String> {
    valid_id(id).then(|| format!("{X_CANONICAL_PREFIX}{id}"))
}

/// The post id of a canonical URL: exactly [`X_CANONICAL_PREFIX`] and 1 to
/// 19 digits without a leading zero, below 2^63. Anything else, including a
/// handle, `twitter.com`, `http`, a trailing slash, a query or a fragment, is
/// `None`.
pub fn parse_x_canonical(url: &str) -> Option<u64> {
    let digits = url.strip_prefix(X_CANONICAL_PREFIX)?;
    let ok = !digits.is_empty()
        && digits.len() <= MAX_ID_DIGITS
        && !digits.starts_with('0')
        && digits.bytes().all(|b| b.is_ascii_digit());
    if !ok {
        return None;
    }
    digits.parse::<u64>().ok().filter(|id| valid_id(*id))
}

/// The source key of post `id`, `x:status:<id>` (hashed into
/// `sources.canon_key`).
pub fn x_source_key(id: u64) -> String {
    format!("{X_SOURCE_KEY_PREFIX}{id}")
}

/// A post's link on a page: `https://x.com/i/status/<id>` and nothing else.
///
/// It serializes as that string and deserializes only from it, so the type
/// itself is the check (DESIGN 18.2's source test relies on it: `post_url` is
/// the one X URL a page carries, and it can only have this form).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PostUrl(u64);

impl PostUrl {
    /// The link to post `id`; `None` for an id X never issues.
    pub fn new(id: u64) -> Option<PostUrl> {
        valid_id(id).then_some(PostUrl(id))
    }

    /// The link a canonical URL names ([`parse_x_canonical`]).
    pub fn parse(url: &str) -> Option<PostUrl> {
        parse_x_canonical(url).map(PostUrl)
    }

    /// The post id.
    pub fn id(self) -> u64 {
        self.0
    }

    /// The URL, `https://x.com/i/status/<id>`.
    pub fn url(self) -> String {
        format!("{X_CANONICAL_PREFIX}{}", self.0)
    }
}

impl std::fmt::Debug for PostUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PostUrl({X_CANONICAL_PREFIX}{})", self.0)
    }
}

impl std::fmt::Display for PostUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{X_CANONICAL_PREFIX}{}", self.0)
    }
}

impl Serialize for PostUrl {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PostUrl {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        PostUrl::parse(&s).ok_or_else(|| serde::de::Error::custom("post_url must be https://x.com/i/status/<id>"))
    }
}

/// The page field `x` of a live X page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct XPost {
    /// "View on X ↗" (new tab, `rel="nofollow ugc noopener"`), the
    /// click-to-load route's blockquote link and nothing else.
    pub post_url: PostUrl,
}

impl XPost {
    /// The field for post `id`; `None` for an id X never issues.
    pub fn new(id: u64) -> Option<XPost> {
        PostUrl::new(id).map(|post_url| XPost { post_url })
    }
}

/// An X post's link sent beside a signed op, never kept.
///
/// `restore {case_id}` on an X page is a fresh check of the post, and the op
/// names no post: the operator's CLI sends the link as `x_url` beside the
/// signed envelope ([`crate::api::OpRequest`]). The Worker accepts it only
/// when [`XPostUrl::source_key`] hashes to the tombstone's `canon_key`, runs
/// the lookup, and drops it. It is never journaled, logged, queued or written
/// to AE: it has no `Serialize` and no `Display`, and its `Debug` prints no
/// digits. It deserializes only from the canonical form (the CLI canonicalises
/// the link through `detect-url`'s `x_post_v1` first).
#[derive(Clone, PartialEq, Eq)]
pub struct XPostUrl(u64);

impl XPostUrl {
    /// The link a canonical URL names.
    pub fn parse(url: &str) -> Option<XPostUrl> {
        parse_x_canonical(url).map(XPostUrl)
    }

    /// The post id, for the lookup.
    pub fn id(&self) -> u64 {
        self.0
    }

    /// The source key, `x:status:<id>`, to hash and compare with the
    /// tombstone's `canon_key`.
    pub fn source_key(&self) -> String {
        x_source_key(self.0)
    }

    /// The page's link to the post, once the fresh check has found it.
    pub fn post_url(&self) -> PostUrl {
        PostUrl(self.0)
    }
}

impl std::fmt::Debug for XPostUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("XPostUrl(…)")
    }
}

impl<'de> Deserialize<'de> for XPostUrl {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        XPostUrl::parse(&s).ok_or_else(|| serde::de::Error::custom("x_url must be https://x.com/i/status/<id>"))
    }
}

/// Compile-time proof that [`XPostUrl`] implements neither `Serialize` nor
/// `Display`: if either were implemented, the probe below would match two
/// impls and fail to compile (the technique of `static_assertions`'
/// `assert_not_impl_any!`).
mod not_kept {
    use super::XPostUrl;

    trait Probe<A> {
        fn probe() {}
    }
    impl<T: ?Sized> Probe<()> for T {}
    struct IsSerialize;
    impl<T: ?Sized + serde::Serialize> Probe<IsSerialize> for T {}
    struct IsDisplay;
    impl<T: ?Sized + std::fmt::Display> Probe<IsDisplay> for T {}

    const _: fn() = || {
        let _ = <XPostUrl as Probe<_>>::probe;
    };
}
