//! The source a canonical URL names, and its source key.
//!
//! - Bluesky posts (`bsky.app/profile/<actor>/post/<rkey>`) are read through
//!   the public AppView by the fetch Worker's Bluesky adapter.
//! - X posts map every URL variant to `x:status:<id>`; they are not read by
//!   URL until the X adapter exists (phase 2), so the URL-page flow refuses
//!   them.
//! - Every other link on an X host ([`X_HOSTS`]: profiles, search, `t.co`
//!   short links, `twimg.com` media) is not a post. The web fetcher never
//!   requests an X host ([`is_x_host`]), on the submitted URL, any redirect
//!   hop or any robots.txt read.
//! - Reddit, LinkedIn, Facebook, Instagram and Threads are paste-only (risk
//!   F15): login walls and platform terms rule out fetching them.
//! - Everything else is a web page.

use crate::check::under;
use url::Url;

/// Registrable domains whose pages are paste-only (risk F15).
pub const PASTE_ONLY: &[&str] = &[
    "reddit.com",
    "redd.it",
    "linkedin.com",
    "lnkd.in",
    "facebook.com",
    "fb.com",
    "fb.me",
    "fb.watch",
    "instagram.com",
    "instagr.am",
    "threads.net",
    "threads.com",
];

/// X's hosts: its two site domains, its link shortener and its media host.
/// Each covers its subdomains, matched on whole labels.
pub const X_HOSTS: &[&str] = &["x.com", "twitter.com", "t.co", "twimg.com"];

/// Whether `host` (as the URL parser leaves it: lowercase A-label, an
/// optional trailing dot) is an X host or a subdomain of one. The web
/// fetcher refuses every such host before any request; this rule is
/// separate from the input checks, so X post links still reach the
/// platform rule.
pub fn is_x_host(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    X_HOSTS.iter().any(|x| under(host, x))
}

/// Whether `url`'s host is an X host ([`is_x_host`]).
pub fn is_x_url(url: &Url) -> bool {
    url.host_str().is_some_and(is_x_host)
}

/// The Bluesky web app's host.
pub const BSKY_HOST: &str = "bsky.app";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Platform {
    Web,
    /// `actor` is a handle or a DID as written in the URL; `rkey` the post's
    /// record key.
    Bluesky {
        actor: String,
        rkey: String,
    },
    X {
        status: String,
    },
    PasteOnly,
}

impl Platform {
    /// The `sources.adapter` value (`web`, `bluesky` or `x`); `None` for a
    /// paste-only platform.
    pub fn adapter(&self) -> Option<&'static str> {
        match self {
            Platform::Web => Some("web"),
            Platform::Bluesky { .. } => Some("bluesky"),
            Platform::X { .. } => Some("x"),
            Platform::PasteOnly => None,
        }
    }
}

/// The platform of a canonical URL whose registrable domain is `registrable`.
pub fn platform(url: &Url, registrable: &str) -> Platform {
    if PASTE_ONLY.contains(&registrable) {
        return Platform::PasteOnly;
    }
    if is_x_url(url) {
        return match x_status_id(url) {
            Some(id) => Platform::X { status: id.to_owned() },
            None => Platform::PasteOnly,
        };
    }
    if url.host_str() == Some(BSKY_HOST) {
        return match bluesky_post(url) {
            Some((actor, rkey)) => Platform::Bluesky { actor: actor.to_owned(), rkey: rkey.to_owned() },
            // Profiles and feeds are not posts; the app's HTML has no text.
            None => Platform::PasteOnly,
        };
    }
    Platform::Web
}

/// The status id of an X post URL: host `x.com`, `twitter.com` or
/// `mobile.twitter.com` (or `www.` of either), path `/<user>/status/<id>`
/// or `/i/web/status/<id>`, with an optional trailing segment such as
/// `/photo/1`.
pub fn x_status_id(url: &Url) -> Option<&str> {
    let host = url.host_str()?;
    if !matches!(host, "x.com" | "www.x.com" | "twitter.com" | "www.twitter.com" | "mobile.twitter.com") {
        return None;
    }
    let mut seg = url.path_segments()?;
    let first = seg.next()?;
    let (second, third) = (seg.next()?, seg.next()?);
    let id = match (first, second) {
        ("i", "web") if third == "status" => seg.next()?,
        (_, "status" | "statuses") if !first.is_empty() => third,
        _ => return None,
    };
    (!id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
}

/// The actor and record key of a Bluesky post URL.
pub fn bluesky_post(url: &Url) -> Option<(&str, &str)> {
    if url.host_str() != Some(BSKY_HOST) {
        return None;
    }
    let path = url.path().strip_suffix('/').unwrap_or(url.path());
    let mut seg = path.strip_prefix('/')?.split('/');
    let (p, actor, post, rkey) = (seg.next()?, seg.next()?, seg.next()?, seg.next()?);
    if seg.next().is_some() || p != "profile" || post != "post" {
        return None;
    }
    let actor_ok = !actor.is_empty() && actor.len() <= 253 && actor.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_:%".contains(&b));
    let rkey_ok = !rkey.is_empty()
        && rkey.len() <= 512
        && rkey != "."
        && rkey != ".."
        && rkey.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:~-".contains(&b));
    (actor_ok && rkey_ok).then_some((actor, rkey))
}

/// The source key's input (`sources.canon_key = H("source_v1" ‖ key)`):
/// `url:<canonical>` for a web page, `bsky:<did>/<rkey>` for a Bluesky post
/// once its DID is known (the fetch answer carries it), `x:status:<id>`.
pub fn source_key(url: &Url, platform: &Platform) -> String {
    match platform {
        Platform::Bluesky { actor, rkey } => bluesky_key(actor, rkey),
        Platform::X { status } => format!("x:status:{status}"),
        Platform::Web | Platform::PasteOnly => format!("url:{}", url.as_str()),
    }
}

/// The key of a Bluesky post by its author's DID.
pub fn bluesky_key(did: &str, rkey: &str) -> String {
    format!("bsky:{did}/{rkey}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Platform {
        let u = Url::parse(s).unwrap();
        let reg = crate::psl::registrable_domain(u.host_str().unwrap()).unwrap().to_owned();
        platform(&u, &reg)
    }

    #[test]
    fn platforms() {
        assert_eq!(p("https://example.com/a"), Platform::Web);
        assert_eq!(p("https://old.reddit.com/r/x/"), Platform::PasteOnly);
        assert_eq!(p("https://www.linkedin.com/posts/x"), Platform::PasteOnly);
        assert_eq!(p("https://m.facebook.com/story.php"), Platform::PasteOnly);
        assert_eq!(p("https://www.instagram.com/p/abc/"), Platform::PasteOnly);
        assert_eq!(p("https://x.com/someone"), Platform::PasteOnly);
        assert_eq!(p("https://twitter.com/someone/status/1790000000000000000"), Platform::X { status: "1790000000000000000".into() });
        assert_eq!(p("https://x.com/i/web/status/12345"), Platform::X { status: "12345".into() });
        assert_eq!(p("https://x.com/u/status/12345/photo/1"), Platform::X { status: "12345".into() });
        assert_eq!(
            p("https://bsky.app/profile/did:plc:z72i7hdynmk6r22z27h6tvur/post/3l6oveex3ii2l"),
            Platform::Bluesky { actor: "did:plc:z72i7hdynmk6r22z27h6tvur".into(), rkey: "3l6oveex3ii2l".into() }
        );
        assert_eq!(p("https://bsky.app/profile/someone.bsky.social"), Platform::PasteOnly);
        assert_eq!(p("https://bsky.app/profile/someone.bsky.social/post/../x"), Platform::PasteOnly);
    }

    #[test]
    fn x_hosts_match_whole_labels() {
        for h in [
            "x.com",
            "www.x.com",
            "mobile.x.com",
            "publish.x.com",
            "api.x.com",
            "twitter.com",
            "www.twitter.com",
            "mobile.twitter.com",
            "t.co",
            "twimg.com",
            "pbs.twimg.com",
            "abs.twimg.com",
            "video.twimg.com",
            "x.com.",
            "t.co.",
        ] {
            assert!(is_x_host(h), "{h}");
        }
        let others = ["notx.com", "x.com.example.com", "nottwitter.com", "at.co", "t.co.example.org", "twimg.com.example.net", "co", ""];
        for h in others {
            assert!(!is_x_host(h), "{h}");
        }
        // Hosts are compared as the URL parser leaves them.
        for u in ["https://T.CO./abc123", "https://\u{ff58}.com/a", "https://t%2Eco/abc123", "https://PBS.twimg.com/media/a.jpg"] {
            assert!(is_x_url(&Url::parse(u).unwrap()), "{u}");
        }
        // Every X host maps to the X family: a post or not a post, never a web page.
        assert_eq!(p("https://t.co/abc123"), Platform::PasteOnly);
        assert_eq!(p("https://pbs.twimg.com/media/abc.jpg"), Platform::PasteOnly);
        assert_eq!(p("https://publish.x.com/oembed"), Platform::PasteOnly);
        assert_eq!(p("https://notx.com/u/status/1"), Platform::Web);
    }

    #[test]
    fn source_keys() {
        let u = Url::parse("https://example.com/a?b=1").unwrap();
        assert_eq!(source_key(&u, &Platform::Web), "url:https://example.com/a?b=1");
        assert_eq!(source_key(&u, &Platform::X { status: "9".into() }), "x:status:9");
        assert_eq!(bluesky_key("did:plc:abc", "3l6"), "bsky:did:plc:abc/3l6");
    }
}
