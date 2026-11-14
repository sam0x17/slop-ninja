//! URL rules of the public URL check (release R2).
//!
//! A submitted URL passes three versioned steps before any slug is drawn, and
//! the fetch Worker runs the same steps again on every redirect hop:
//!
//! 1. [`check`] (`url_check_v1`): the input checks. The WHATWG parser (the
//!    `url` crate) applies IDNA, so every comparison sees the A-label host;
//!    `http` and `https` only, default ports only, no userinfo, no IP literal
//!    in any form (the parser turns decimal, octal, hex and short IPv4 forms
//!    into an IPv4 host, and IPv4-mapped IPv6 stays IPv6), no single-label
//!    host, no reserved suffix, no host under a top-level label the Public
//!    Suffix List does not know, no host that is itself a public suffix, and
//!    none of our own hosts, except that a `/d/<slug>` link on the site
//!    answers with that slug. At most 8,000 octets.
//! 2. [`canon`] (`canon_url_v1`): the canonical form (fragment and default
//!    port removed, trailing host dot removed, tracking parameters removed,
//!    other parameters kept in their order).
//! 3. [`capability`] (`capability_url_v1`): share links, credential-named
//!    parameters, side-effect paths and random-looking runs of 80 bits or
//!    more are refused, with a versioned allow-list for hosts whose public
//!    ids exceed the bound.
//!
//! [`platform`] maps a canonical URL to its source (web page, Bluesky post,
//! X post, or a paste-only platform) and its source key, and names X's hosts
//! ([`platform::X_HOSTS`]: `x.com`, `twitter.com`, `t.co`, `twimg.com` and
//! their subdomains), which the web fetcher never requests. [`psl`] gives the
//! registrable domain from the compiled Public Suffix List (no allocation at
//! run time). [`fixture`] is the staging-only fixture allowance.
//!
//! [`admit`] runs every step for the URL-page flow.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod canon;
pub mod capability;
pub mod check;
pub mod fixture;
pub mod platform;
pub mod psl;

pub use canon::canon_url_v1;
pub use capability::{Capability, capability_url_v1};
pub use check::{Checked, MAX_URL_BYTES, Policy, Refusal, check};
pub use fixture::FixturePrefix;
pub use platform::Platform;

/// The input checks' version.
pub const CHECK_V: &str = "url_check_v1";
/// The canonical form's version.
pub const CANON_V: &str = "canon_url_v1";
/// The capability rule's version.
pub const CAPABILITY_V: &str = "capability_url_v1";

/// A URL the URL-page flow may fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The canonical URL (`canon_url_v1`).
    pub url: url::Url,
    /// The host, A-label form, without a trailing dot.
    pub host: String,
    /// The registrable domain (the `DetectHost` key).
    pub registrable: String,
    /// The source the URL names.
    pub platform: Platform,
    /// Admitted by the staging fixture allowance.
    pub fixture: bool,
}

/// What [`admit`] answers for an acceptable input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admitted {
    /// A URL to fetch.
    Fetch(Target),
    /// A link to one of our own public pages: the slug as written (lowercase;
    /// the detect Worker folds it with `detect-page`).
    Slug(String),
}

/// Every step of the URL-page flow: [`check`], [`canon_url_v1`], the
/// platform rule (paste-only platforms and X posts are refused until their
/// adapters exist, and any other link on an X host is `x_not_a_post`) and
/// [`capability_url_v1`]. No URL on an X host is ever admitted as a web page.
pub fn admit(input: &str, policy: &Policy) -> Result<Admitted, Refusal> {
    let checked = match check(input, policy)? {
        Checked::Slug(s) => return Ok(Admitted::Slug(s)),
        Checked::Url(c) => c,
    };
    let url = canon_url_v1(&checked.url);
    let platform = platform::platform(&url, &checked.registrable);
    match platform {
        Platform::Web | Platform::Bluesky { .. } => {}
        Platform::X { .. } => return Err(Refusal::PasteOnly),
        Platform::PasteOnly if platform::is_x_host(&checked.host) || platform::is_x_mirror(&checked.host) => return Err(Refusal::XNotAPost),
        Platform::PasteOnly => return Err(Refusal::PasteOnly),
    }
    let skip = if checked.fixture { policy.fixture().map_or(0, FixturePrefix::path_len) } else { 0 };
    capability::check_from(&url, &checked.host, skip).map_err(Refusal::Capability)?;
    Ok(Admitted::Fetch(Target { url, host: checked.host, registrable: checked.registrable, platform, fixture: checked.fixture }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admit_runs_every_step() {
        let p = Policy::production();
        let Admitted::Fetch(t) = admit("HTTPS://Example.COM./a/b?utm_source=x&id=7#frag", &p).unwrap() else { panic!() };
        assert_eq!(t.url.as_str(), "https://example.com/a/b?id=7");
        assert_eq!(t.host, "example.com");
        assert_eq!(t.registrable, "example.com");
        assert_eq!(t.platform, Platform::Web);
        assert_eq!(admit("https://slop.ninja/d/AbC1234", &p), Ok(Admitted::Slug("abc1234".into())));
        assert_eq!(admit("https://www.reddit.com/r/x/comments/abc/title/", &p), Err(Refusal::PasteOnly));
        assert_eq!(admit("https://x.com/someone/status/1790000000000000000", &p), Err(Refusal::PasteOnly));
        assert!(matches!(admit("https://example.com/reset/abc", &p), Err(Refusal::Capability(_))));
        let Admitted::Fetch(b) = admit("https://bsky.app/profile/someone.bsky.social/post/3l6oveex3ii2l", &p).unwrap() else { panic!() };
        assert!(matches!(b.platform, Platform::Bluesky { .. }));
    }

    #[test]
    fn no_x_host_is_admitted_as_a_web_page() {
        let p = Policy::production();
        for u in [
            "https://t.co/abc123",
            "http://t.co/abc123",
            "https://T.CO./abc123",
            "https://t%2Eco/abc123",
            "https://pbs.twimg.com/media/abc.jpg",
            "https://abs.twimg.com/",
            "https://twimg.com/",
            "https://x.com/someone",
            "https://\u{ff58}.com/someone",
            "https://twitter.com/search?q=a",
            "https://mobile.twitter.com/someone",
            "https://publish.x.com/oembed?url=https://twitter.com/i/status/1",
            "https://api.x.com/2/tweets/1",
        ] {
            assert_eq!(admit(u, &p), Err(Refusal::XNotAPost), "{u}");
        }
        assert_eq!(Refusal::XNotAPost.public_code(), "x_not_a_post");
        // Mirrors of X posts are not posts either.
        for u in ["https://fxtwitter.com/a/status/1790000000000000000", "https://vxtwitter.com/a/status/1"] {
            assert_eq!(admit(u, &p), Err(Refusal::XNotAPost), "{u}");
        }
        // X posts are still recognised (refused until the X adapter exists).
        assert_eq!(admit("https://twitter.com/someone/status/1790000000000000000", &p), Err(Refusal::PasteOnly));
        // Look-alike hosts on whole labels are web pages.
        for u in ["https://notx.com/a", "https://t.co.example.com/a", "https://twimg.com.example.net/a"] {
            let Admitted::Fetch(t) = admit(u, &p).unwrap() else { panic!("{u}") };
            assert_eq!(t.platform, Platform::Web, "{u}");
        }
    }

    #[test]
    fn the_fixture_prefix_skips_its_own_token_but_not_the_rest() {
        let prefix = "https://slop.ninja/preview/fixturetoken0001/fixtures/";
        let p = Policy::production().with_fixture(FixturePrefix::parse(prefix).unwrap());
        let Admitted::Fetch(t) = admit(&format!("{prefix}plain.html"), &p).unwrap() else { panic!() };
        assert!(t.fixture);
        assert!(matches!(admit(&format!("{prefix}page.html?token=1"), &p), Err(Refusal::Capability(_))));
        // Without the allowance the same URL is our own host.
        assert_eq!(admit(&format!("{prefix}plain.html"), &Policy::production()), Err(Refusal::OwnHost));
        // Another preview path is still our own host.
        assert_eq!(admit("https://slop.ninja/preview/other/fixtures/plain.html", &p), Err(Refusal::OwnHost));
    }
}
