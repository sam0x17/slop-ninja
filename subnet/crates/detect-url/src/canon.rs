//! `canon_url_v1`: the canonical form of a checked URL.
//!
//! - scheme and host lowercased, the host in IDNA A-label form (both done by
//!   the WHATWG parser), its trailing dot removed;
//! - default port and fragment removed;
//! - tracking parameters removed by the versioned list [`TRACKING_V`]
//!   (`utm_*` and the click identifiers below, and X's `s` and `t` on
//!   status URLs);
//! - every other parameter kept byte for byte and in its order, because
//!   sorting changes meaning on some sites; empty pieces (`&&`) dropped, and
//!   an empty query removed.
//!
//! The form is idempotent: canonicalizing a canonical URL changes nothing.

use url::Url;

/// The tracking-parameter list's version.
pub const TRACKING_V: &str = "tracking_v1";

/// Parameter names removed everywhere (compared lowercased, after
/// percent-decoding), besides every name starting with `utm_`.
pub const TRACKING_PARAMS: &[&str] = &[
    "fbclid", "gclid", "dclid", "gbraid", "wbraid", "msclkid", "mc_cid", "mc_eid", "igshid", "ref_src", "yclid", "twclid", "ttclid", "_ga",
    "_gl",
];

/// Parameter names removed on X status URLs only.
pub const X_STATUS_TRACKING: &[&str] = &["s", "t"];

/// The canonical form of `url` (`canon_url_v1`).
pub fn canon_url_v1(url: &Url) -> Url {
    let mut out = url.clone();
    out.set_fragment(None);
    if let Some(url::Host::Domain(d)) = url.host()
        && let Some(stripped) = d.strip_suffix('.')
    {
        let _ = out.set_host(Some(stripped));
    }
    let x_status = is_x_status(&out);
    let query = out.query().map(|q| {
        q.split('&').filter(|piece| !piece.is_empty() && !tracking(param_name(piece).as_str(), x_status)).collect::<Vec<_>>().join("&")
    });
    match query {
        Some(q) if !q.is_empty() => out.set_query(Some(&q)),
        _ => out.set_query(None),
    }
    out
}

/// A query piece's name, percent-decoded (`+` read as a space) and
/// lowercased.
pub(crate) fn param_name(piece: &str) -> String {
    let name = piece.split('=').next().unwrap_or("");
    let name = name.replace('+', " ");
    percent_encoding::percent_decode_str(&name).decode_utf8_lossy().to_ascii_lowercase()
}

fn tracking(name: &str, x_status: bool) -> bool {
    name.starts_with("utm_") || TRACKING_PARAMS.contains(&name) || (x_status && X_STATUS_TRACKING.contains(&name))
}

/// An X post URL: `x.com`, `twitter.com` or `mobile.twitter.com` (or
/// `www.`), path `/<user>/status/<digits>`.
pub(crate) fn is_x_status(url: &Url) -> bool {
    crate::platform::x_status_id(url).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon(s: &str) -> String {
        canon_url_v1(&Url::parse(s).unwrap()).to_string()
    }

    #[test]
    fn tracking_parameters_go_and_order_stays() {
        assert_eq!(canon("https://example.com/a?b=2&utm_source=x&a=1&UTM_Medium=y&fbclid=z&c=3"), "https://example.com/a?b=2&a=1&c=3");
        assert_eq!(canon("https://example.com/a?utm_campaign=x&gclid=1"), "https://example.com/a");
        assert_eq!(canon("https://example.com/a?z=1&a=2"), "https://example.com/a?z=1&a=2");
        assert_eq!(canon("https://example.com/a?q=a%20b&r=c+d&&s"), "https://example.com/a?q=a%20b&r=c+d&s");
        assert_eq!(canon("https://example.com/a?%75tm_source=x&k=1"), "https://example.com/a?k=1");
        // `s` and `t` are kept off X status URLs.
        assert_eq!(canon("https://example.com/a?s=1&t=2"), "https://example.com/a?s=1&t=2");
        assert_eq!(
            canon("https://x.com/someone/status/1790000000000000000?s=20&t=abc"),
            "https://x.com/someone/status/1790000000000000000"
        );
        assert_eq!(
            canon("https://mobile.twitter.com/someone/status/1790000000000000000?t=abc&lang=en"),
            "https://mobile.twitter.com/someone/status/1790000000000000000?lang=en"
        );
    }

    #[test]
    fn fragment_port_case_and_trailing_dot() {
        assert_eq!(canon("HTTPS://Example.COM.:443/Path/?a=1#frag"), "https://example.com/Path/?a=1");
        assert_eq!(canon("http://example.com:80"), "http://example.com/");
        assert_eq!(canon("https://example.com/a/./b/../c"), "https://example.com/a/c");
    }

    #[test]
    fn canonical_form_is_idempotent() {
        for s in [
            "https://example.com/a?b=2&utm_source=x&a=1#f",
            "https://ex.com./?",
            "https://ex.com/%7Euser/?q=%E2%82%AC&x",
            "https://x.com/u/status/1?s=1&t=2&x=3",
        ] {
            let once = canon(s);
            assert_eq!(canon(&once), once, "{s}");
        }
    }
}
