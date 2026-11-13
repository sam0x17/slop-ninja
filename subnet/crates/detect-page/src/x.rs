//! Posts on X: the canonical post URL and the static quote a page shows.
//!
//! A link to an X post is checked as that post: X's API supplies the text
//! that is scored, the page is a public URL page (`kind = public_url`,
//! `platform = x`) that keeps no post text, and the post itself is shown
//! through X's own embed markup. The detect Worker reads that markup once per
//! page-cache miss (`publish.x.com/oembed`, through the fetch Worker) and
//! hands the renderer a typed [`XEmbed`]: only text, links and line breaks
//! survive, and every link points at an X host. The renderer draws it as X's
//! fallback blockquote; nothing on the page asks X for anything until the
//! reader chooses "Show this post as X displays it" (`/d/<7>/x`).
//!
//! What this module does not do: recognise an X post in arbitrary input
//! (rule `x_post_v1`, crate `detect-url`) or parse X's markup (the fetch
//! Worker). It fixes the shapes both sides exchange and the checks a
//! consumer may rely on.

use serde::{Deserialize, Serialize};

/// The canonical form of a post's URL: this prefix and the post id. It names
/// no handle, because handles change.
pub const X_CANONICAL_PREFIX: &str = "https://x.com/i/status/";
/// The `sources.host` of every X source.
pub const X_HOST: &str = "x.com";
/// The hosts an [`XEmbed`] link may point at (`http://t.co` is upgraded to
/// `https` before the embed is built).
pub const X_LINK_HOSTS: &[&str] = &["x.com", "twitter.com", "t.co"];
/// The most digits of a post id (ids are below 2^63).
pub const MAX_ID_DIGITS: usize = 19;
/// The longest handle.
pub const MAX_HANDLE: usize = 15;

/// The canonical URL of post `id`, or `None` for 0 and ids of 2^63 and
/// above, which X never issues.
pub fn x_canonical(id: u64) -> Option<String> {
    (id != 0 && id <= i64::MAX as u64).then(|| format!("{X_CANONICAL_PREFIX}{id}"))
}

/// The post id of a canonical URL: exactly [`X_CANONICAL_PREFIX`] and 1 to
/// 19 digits without a leading zero, below 2^63. Anything else, including a
/// trailing slash, a query or another host, is `None`.
pub fn parse_x_canonical(url: &str) -> Option<u64> {
    let digits = url.strip_prefix(X_CANONICAL_PREFIX)?;
    let ok = !digits.is_empty()
        && digits.len() <= MAX_ID_DIGITS
        && !digits.starts_with('0')
        && digits.bytes().all(|b| b.is_ascii_digit());
    if !ok {
        return None;
    }
    digits.parse::<u64>().ok().filter(|id| *id <= i64::MAX as u64)
}

/// Whether `h` has a handle's form: 1 to 15 of `A-Z`, `a-z`, `0-9` and `_`.
pub fn is_handle(h: &str) -> bool {
    !h.is_empty() && h.len() <= MAX_HANDLE && h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Whether `href` may appear in an [`XEmbed`]: `https://` on exactly one of
/// [`X_LINK_HOSTS`] (lowercase, no port, no userinfo), followed by nothing or
/// by `/`, `?` or `#`, in printable ASCII with no space, quote, angle
/// bracket, backslash or backtick.
pub fn is_x_href(href: &str) -> bool {
    let Some(rest) = href.strip_prefix("https://") else {
        return false;
    };
    let printable = href.bytes().all(|b| (0x21..0x7f).contains(&b) && !b"\"'<>\\`".contains(&b));
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    printable && X_LINK_HOSTS.contains(&&rest[..host_end])
}

crate::code_enum! {
    /// A text direction, as X's markup gives it (`dir`).
    pub enum TextDir {
        Ltr = "ltr",
        Rtl = "rtl",
        /// The markup gave none: the browser decides.
        Auto = "auto",
    }
}

/// One piece of a post's quoted text, in order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum XPart {
    /// Plain text, exactly as X's markup carries it after entity decoding.
    /// The renderer escapes it.
    Text { text: String },
    /// A link (a mention, a hashtag, a `t.co` link): its target and its text.
    Link { href: String, text: String },
    /// A line break (`<br>`).
    Break,
}

/// A post as X's own fallback markup shows it: the quoted text, then
/// "— ‹author_name› (@‹handle›)" and the date linked to the post. The
/// renderer draws it as `<blockquote class="twitter-tweet" data-dnt="true"
/// data-conversation="none" data-cards="hidden">` with no avatar and no
/// media, so the browser requests nothing from X.
///
/// It is never stored: the detect Worker builds it on a page-cache miss and
/// the rendered page lives in the page cache for at most `PAGE_CACHE_S`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct XEmbed {
    /// The post's language tag (`lang`), when the markup gives one.
    pub lang: Option<String>,
    pub dir: TextDir,
    /// The quoted text. X cuts a long post at about 277 characters and ends
    /// it with "…"; the embed keeps X's cut.
    pub parts: Vec<XPart>,
    /// The author's display name, as the markup prints it.
    pub author_name: String,
    /// The author's profile link, `https://twitter.com/<handle>` or
    /// `https://x.com/<handle>`.
    pub author_url: String,
    /// The date as the markup prints it ("November 11, 2026").
    pub date_text: String,
    /// The post's link from the markup (its date link); "View on X" uses the
    /// source's canonical URL.
    pub permalink: String,
}

/// A broken [`XEmbed`] form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbedError(pub &'static str);

impl XEmbed {
    /// The handle in [`XEmbed::author_url`].
    pub fn handle(&self) -> Option<&str> {
        let rest = self.author_url.strip_prefix("https://")?;
        let path = rest.strip_prefix("twitter.com/").or_else(|| rest.strip_prefix("x.com/"))?;
        is_handle(path).then_some(path)
    }

    /// Checks the form a consumer relies on: at least one part; every link,
    /// the author link and the permalink pass [`is_x_href`]; the author link
    /// names a handle; `lang` is 1 to 35 of `A-Z`, `a-z`, `0-9` and `-`; the
    /// author name and date are not empty.
    pub fn check(&self) -> Result<(), EmbedError> {
        let need = |ok: bool, what: &'static str| if ok { Ok(()) } else { Err(EmbedError(what)) };
        need(!self.parts.is_empty(), "an embed has text")?;
        for p in &self.parts {
            if let XPart::Link { href, .. } = p {
                need(is_x_href(href), "links point at X")?;
            }
        }
        need(is_x_href(&self.author_url) && self.handle().is_some(), "author link")?;
        need(is_x_href(&self.permalink), "permalink")?;
        if let Some(l) = &self.lang {
            need(!l.is_empty() && l.len() <= 35 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'), "lang")?;
        }
        need(!self.author_name.is_empty() && !self.date_text.is_empty(), "author and date")?;
        Ok(())
    }
}
