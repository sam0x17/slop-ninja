//! Redirects (risk F2): followed manually, each hop re-checked from the top.
//!
//! The `Location` is resolved against the current URL and the result passes
//! the same URL rules as a submitted URL: for a page, the input checks, the
//! canonical form, the platform rule and the capability rule
//! ([`slop_ninja_detect_url::admit`]); for robots.txt, the input checks
//! ([`slop_ninja_detect_url::check`]; RFC 9309 robots files are never
//! capability links). An `https` URL that redirects to `http` ends the chain
//! (the downgrade stop), as do a loop and the hop limit. A hop to an X host
//! (`x.com`, `twitter.com`, `t.co`, `twimg.com` or a subdomain;
//! [`slop_ninja_detect_url::platform::is_x_host`]) ends the chain for both
//! kinds before the URL rules run, so it is never screened or requested. The
//! caller then screens the new host and, for a page on a new origin, reads
//! its robots.txt.

use slop_ninja_detect_fetch_wire::Outcome;
use slop_ninja_detect_url::platform::is_x_url;
use slop_ninja_detect_url::{Admitted, Checked, Platform, Policy, Refusal, admit, check};
use url::Url;

/// What a redirect leads to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Follow to this URL (canonical for pages); `host` is its host.
    Go {
        url: Url,
        host: String,
    },
    Stop(Outcome),
}

/// Whether a status is a redirect the fetch follows.
pub fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Which rules a hop must pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Page,
    Robots,
}

/// The next hop from `current` given its `location`, after `hops_done` hops
/// of at most `max_hops`, never revisiting a URL in `seen`.
pub fn next(current: &Url, location: Option<&str>, policy: &Policy, kind: Kind, hops_done: u32, max_hops: u32, seen: &[String]) -> Next {
    if hops_done >= max_hops {
        return Next::Stop(Outcome::TooManyRedirects);
    }
    let Some(location) = location.map(str::trim).filter(|l| !l.is_empty()) else {
        return Next::Stop(Outcome::FetchFailed);
    };
    let Ok(joined) = current.join(location) else { return Next::Stop(Outcome::RedirectRefused) };
    if current.scheme() == "https" && joined.scheme() == "http" {
        return Next::Stop(Outcome::Downgrade);
    }
    // The web fetcher never requests an X host (a page or a robots.txt).
    if is_x_url(&joined) {
        return Next::Stop(Outcome::Unsupported);
    }
    let (url, host) = match kind {
        Kind::Page => match admit(joined.as_str(), policy) {
            Ok(Admitted::Fetch(t)) if t.platform == Platform::Web => (t.url, t.host),
            Ok(Admitted::Fetch(_)) | Err(Refusal::PasteOnly | Refusal::XNotAPost) => return Next::Stop(Outcome::Unsupported),
            Ok(Admitted::Slug(_)) | Err(_) => return Next::Stop(Outcome::RedirectRefused),
        },
        Kind::Robots => match check(joined.as_str(), policy) {
            Ok(Checked::Url(c)) => {
                let mut u = c.url;
                u.set_fragment(None);
                (u, c.host)
            }
            Ok(Checked::Slug(_)) | Err(_) => return Next::Stop(Outcome::RedirectRefused),
        },
    };
    if seen.iter().any(|s| s == url.as_str()) {
        return Next::Stop(Outcome::TooManyRedirects);
    }
    Next::Go { url, host }
}

/// `scheme://host` of a URL (default ports only, so no port).
pub fn origin(url: &Url) -> String {
    format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""))
}

/// The path and query a robots rule is matched against.
pub fn path_and_query(url: &Url) -> String {
    match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn go(from: &str, loc: &str) -> Next {
        next(&Url::parse(from).unwrap(), Some(loc), &Policy::production(), Kind::Page, 0, 20, &[])
    }

    #[test]
    fn hops_are_rechecked_from_the_top() {
        assert_eq!(
            go("http://example.com/a", "https://www.example.com/a?utm_source=x#top"),
            Next::Go { url: Url::parse("https://www.example.com/a").unwrap(), host: "www.example.com".into() }
        );
        assert_eq!(
            go("https://example.com/a/b", "../c"),
            Next::Go { url: Url::parse("https://example.com/c").unwrap(), host: "example.com".into() }
        );
        for (loc, want) in [
            ("http://127.0.0.1/", Outcome::Downgrade),
            ("https://127.0.0.1/", Outcome::RedirectRefused),
            ("https://[::1]/", Outcome::RedirectRefused),
            ("https://localhost/", Outcome::RedirectRefused),
            ("https://metadata.internal/", Outcome::RedirectRefused),
            ("https://slop.ninja/", Outcome::RedirectRefused),
            ("https://slop.ninja/d/abc1234", Outcome::RedirectRefused),
            ("https://x.workers.dev/", Outcome::RedirectRefused),
            ("https://example.com:8443/", Outcome::RedirectRefused),
            ("https://user@example.com/", Outcome::RedirectRefused),
            ("https://example.com/reset/abc", Outcome::RedirectRefused),
            ("https://example.com/a?token=x", Outcome::RedirectRefused),
            ("https://docs.google.com/document/d/x", Outcome::RedirectRefused),
            ("https://www.reddit.com/r/x", Outcome::Unsupported),
            ("https://bsky.app/profile/a.bsky.social/post/3l6oveex3ii2l", Outcome::Unsupported),
            ("ftp://example.com/", Outcome::RedirectRefused),
            ("javascript:alert(1)", Outcome::RedirectRefused),
            ("http://example.com/plain", Outcome::Downgrade),
        ] {
            assert_eq!(go("https://example.com/a", loc), Next::Stop(want), "{loc}");
        }
    }

    #[test]
    fn hops_to_x_hosts_stop_for_pages_and_robots() {
        let p = Policy::production();
        for (from, kind) in [("https://example.com/a", Kind::Page), ("https://example.com/robots.txt", Kind::Robots)] {
            let cur = Url::parse(from).unwrap();
            for loc in [
                "https://t.co/abc123",
                "//t.co/abc123",
                "https://T.CO./abc123",
                "https://t%2Eco/abc123",
                "https://pbs.twimg.com/media/abc.jpg",
                "https://abs.twimg.com/robots.txt",
                "https://x.com/robots.txt",
                "https://x.com/u/status/1",
                "https://\u{ff58}.com/someone",
                "https://www.twitter.com/someone",
                "https://mobile.twitter.com/robots.txt",
                "https://publish.x.com/oembed",
            ] {
                assert_eq!(next(&cur, Some(loc), &p, kind, 0, 20, &[]), Next::Stop(Outcome::Unsupported), "{kind:?} {loc}");
            }
            // Look-alikes on whole labels are ordinary hosts.
            for loc in ["https://notx.com/robots.txt", "https://t.co.example.com/robots.txt"] {
                assert!(matches!(next(&cur, Some(loc), &p, kind, 0, 20, &[]), Next::Go { .. }), "{kind:?} {loc}");
            }
        }
    }

    #[test]
    fn limits_loops_and_missing_locations() {
        let cur = Url::parse("https://example.com/a").unwrap();
        let p = Policy::production();
        assert_eq!(next(&cur, Some("/b"), &p, Kind::Page, 20, 20, &[]), Next::Stop(Outcome::TooManyRedirects));
        assert_eq!(next(&cur, None, &p, Kind::Page, 0, 20, &[]), Next::Stop(Outcome::FetchFailed));
        assert_eq!(next(&cur, Some("  "), &p, Kind::Page, 0, 20, &[]), Next::Stop(Outcome::FetchFailed));
        let seen = vec!["https://example.com/b".to_owned()];
        assert_eq!(next(&cur, Some("/b"), &p, Kind::Page, 1, 20, &seen), Next::Stop(Outcome::TooManyRedirects));
    }

    #[test]
    fn robots_hops_skip_the_capability_rule_only() {
        let cur = Url::parse("https://example.com/robots.txt").unwrap();
        let p = Policy::production();
        assert!(matches!(next(&cur, Some("https://cdn.example.net/robots.txt?sig=abc"), &p, Kind::Robots, 0, 5, &[]), Next::Go { .. }));
        assert_eq!(next(&cur, Some("https://10.0.0.1/robots.txt"), &p, Kind::Robots, 0, 5, &[]), Next::Stop(Outcome::RedirectRefused));
        assert_eq!(next(&cur, Some("http://example.com/robots.txt"), &p, Kind::Robots, 0, 5, &[]), Next::Stop(Outcome::Downgrade));
    }

    #[test]
    fn origins_and_paths() {
        let u = Url::parse("https://Example.com/a/b?c=1#f").unwrap();
        assert_eq!(origin(&u), "https://example.com");
        assert_eq!(path_and_query(&u), "/a/b?c=1");
    }
}
