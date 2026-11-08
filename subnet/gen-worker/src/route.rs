//! Where a vendor's calls go: the vendor's own API (the default), or a
//! configured endpoint that speaks the vendor's wire format
//! ([`Route::ConfiguredEndpointV1`]).
//!
//! A configured endpoint is one Worker secret per vendor ([`OPENAI_SECRET`],
//! [`ANTHROPIC_SECRET`]), installed from stdin at deploy time and never
//! written to a tracked file (it carries the endpoint's credentials). Its
//! value is a JSON object:
//!
//! ```text
//! {"route": "configured_endpoint_v1",   the manifest's route for the vendor
//!  "endpoint": "https://<host>/<path>",  the full URL of the vendor endpoint
//!  "auth": "bearer" | "x-api-key",       how the key is sent
//!  "key": "<key>",
//!  "headers": [["<name>", "<value>"], ...],  optional further headers
//!  "rate_limit_codes": "<code>,...",         optional further error codes
//!  "unsupported_codes": "<code>,..."}        (ErrorCodes)
//! ```
//!
//! Without the secret the vendor is reached directly: its documented
//! endpoint ([`Vendor::endpoint`]), its key from the Secrets Store binding,
//! and its own key header (`authorization: Bearer` for OpenAI, `x-api-key`
//! for Anthropic). A secret that does not parse or validate makes the
//! vendor's key unavailable (health false, every call `key_unavailable`,
//! which pauses the vendor and alerts): a broken configuration is never
//! replaced by the direct route, since the manifest's request shape is the
//! route's.
//!
//! Nothing here is logged or returned: [`RouteConfig`] has no `Debug`, and
//! its key and header values are zeroed on drop. The endpoint and header
//! values are checked to be well-formed header and URL text before use, so
//! a bad value is a refused configuration, never a fetch error.

use serde::Deserialize;
use slop_ninja_gen_core::sse::ErrorCodes;
use slop_ninja_gen_core::vendor::{Route, Vendor};
use zeroize::Zeroizing;

/// The Worker secrets holding each vendor's endpoint (absent: direct).
pub const OPENAI_SECRET: &str = "GEN_OPENAI_ENDPOINT";
pub const ANTHROPIC_SECRET: &str = "GEN_ANTHROPIC_ENDPOINT";

/// An endpoint secret's value at most: the platform's limit on a Worker
/// secret's (environment variable's) value, 5 KB, read as 5 KiB.
pub const SECRET_BYTES: usize = 5 * 1024;

/// The heap an endpoint secret's parse holds at most: the secret's value and one copy
/// of every field (each field's text is part of the value; JSON unescaping
/// only shrinks it), plus one page for the header list and string headers.
pub const HEAP_BYTES: usize = 2 * SECRET_BYTES + slop_ninja_gen_core::iface::PAGE;

/// The headers the generation Worker sets itself, or that a fetch must not
/// carry: a configuration cannot add or replace them.
const RESERVED: &[&str] = &[
    "accept",
    "anthropic-version",
    "authorization",
    "connection",
    "content-length",
    "content-type",
    "host",
    "te",
    "transfer-encoding",
    "upgrade",
    "x-api-key",
];

/// How the key is sent.
#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Auth {
    /// `authorization: Bearer <key>`.
    #[serde(rename = "bearer")]
    Bearer,
    /// `x-api-key: <key>`.
    #[serde(rename = "x-api-key")]
    XApiKey,
}

impl Auth {
    /// The vendor's own key header.
    pub fn direct(v: Vendor) -> Auth {
        if v == Vendor::Openai { Auth::Bearer } else { Auth::XApiKey }
    }
    pub fn header(self) -> &'static str {
        match self {
            Auth::Bearer => "authorization",
            Auth::XApiKey => "x-api-key",
        }
    }
}

/// Why an endpoint secret was refused (fixed text, never the value).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused(pub &'static str);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    route: Route,
    endpoint: String,
    auth: Auth,
    // Moved into `Zeroizing` right after the parse (no copy is left behind).
    key: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    rate_limit_codes: String,
    #[serde(default)]
    unsupported_codes: String,
}

/// A configured endpoint (from its secret). No `Debug`: it holds a key.
pub struct RouteConfig {
    pub route: Route,
    pub endpoint: String,
    pub auth: Auth,
    pub key: Zeroizing<String>,
    /// Further headers, names lower-cased.
    pub headers: Vec<(String, Zeroizing<String>)>,
    /// Further OpenAI-format error codes (comma-separated, [`ErrorCodes`]).
    pub rate_limit_codes: String,
    pub unsupported_codes: String,
}

/// Whether `v` can be a header value as it is: visible ASCII with interior
/// spaces, no leading or trailing space, no control character.
fn value_ok(v: &str) -> bool {
    !v.is_empty() && !v.starts_with(' ') && !v.ends_with(' ') && v.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
}

/// Whether `n` is a header name (an RFC 9110 token, here letters, digits
/// and `-`) that a configuration may set.
fn name_ok(n: &str) -> bool {
    !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && !RESERVED.contains(&n)
}

/// Whether `u` is an `https` URL with a host and a path, and nothing a
/// fetch could read as credentials, a query or a fragment.
fn endpoint_ok(u: &str) -> bool {
    let Some(rest) = u.strip_prefix("https://") else { return false };
    let Some(slash) = rest.find('/') else { return false };
    let (host, path) = rest.split_at(slash);
    !host.is_empty()
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b':')
        && path.len() > 1
        && u.bytes().all(|b| b.is_ascii_graphic() && !matches!(b, b'@' | b'?' | b'#' | b'\\'))
}

/// Whether `l` is a list of error codes: empty, or comma-separated codes
/// of letters, digits, `_`, `.` and `-`, none empty.
fn codes_ok(l: &str) -> bool {
    l.is_empty() || l.split(',').all(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')))
}

impl RouteConfig {
    /// Parses an endpoint secret's value (at most [`SECRET_BYTES`]).
    pub fn parse(value: &str) -> Result<RouteConfig, Refused> {
        if value.len() > SECRET_BYTES {
            return Err(Refused("route (over the secret size)"));
        }
        let raw: Raw = serde_json::from_str(value).map_err(|_| Refused("route (json)"))?;
        let key = Zeroizing::new(raw.key);
        let given: Vec<(String, Zeroizing<String>)> = raw.headers.into_iter().map(|(n, v)| (n, Zeroizing::new(v))).collect();
        if !endpoint_ok(&raw.endpoint) {
            return Err(Refused("route (endpoint: an https URL with a host and a path, no credentials, query or fragment)"));
        }
        if !crate::drive::key_ok(&key) {
            return Err(Refused("route (key)"));
        }
        let mut headers = Vec::with_capacity(given.len());
        for (n, v) in given {
            let n = n.to_ascii_lowercase();
            if !name_ok(&n) || !value_ok(&v) || headers.iter().any(|(m, _): &(String, Zeroizing<String>)| *m == n) {
                return Err(Refused("route (headers: distinct, well-formed, none the Worker sets)"));
            }
            headers.push((n, v));
        }
        if !codes_ok(&raw.rate_limit_codes) || !codes_ok(&raw.unsupported_codes) {
            return Err(Refused("route (error codes: comma-separated codes of letters, digits, _ . -)"));
        }
        Ok(RouteConfig {
            route: raw.route,
            endpoint: raw.endpoint,
            auth: raw.auth,
            key,
            headers,
            rate_limit_codes: raw.rate_limit_codes,
            unsupported_codes: raw.unsupported_codes,
        })
    }
}

/// Where one call goes, borrowed for the call: the route it was configured
/// with, the URL, how the key is sent, the key, any further headers and
/// error codes.
pub struct Target<'a> {
    pub route: Route,
    pub url: &'a str,
    pub auth: Auth,
    pub key: &'a str,
    pub headers: &'a [(String, Zeroizing<String>)],
    pub codes: ErrorCodes<'a>,
}

impl<'a> Target<'a> {
    /// The vendor's own API with `key` (`None` for Workers AI, which never
    /// runs here).
    pub fn direct(v: Vendor, key: &'a str) -> Option<Target<'a>> {
        Some(Target { route: Route::Direct, url: v.endpoint()?, auth: Auth::direct(v), key, headers: &[], codes: ErrorCodes::NONE })
    }
    pub fn of(c: &'a RouteConfig) -> Target<'a> {
        Target {
            route: c.route,
            url: &c.endpoint,
            auth: c.auth,
            key: &c.key,
            headers: &c.headers,
            codes: ErrorCodes { rate_limit: &c.rate_limit_codes, unsupported: &c.unsupported_codes },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "test-endpoint-key";

    fn cfg(endpoint: &str, headers: &str) -> String {
        format!(r#"{{"route":"configured_endpoint_v1","endpoint":"{endpoint}","auth":"bearer","key":"{KEY}","headers":{headers}}}"#)
    }

    #[test]
    fn an_endpoint_secret_parses_and_validates() {
        let c = RouteConfig::parse(&cfg("https://frontier.example.test/v1/messages", &format!(r#"[["X-Endpoint-Auth","{KEY}"]]"#))).unwrap();
        assert_eq!((c.route, c.endpoint.as_str(), c.auth == Auth::Bearer), (Route::ConfiguredEndpointV1, "https://frontier.example.test/v1/messages", true));
        assert_eq!(c.headers.len(), 1);
        assert_eq!((c.headers[0].0.as_str(), c.headers[0].1.as_str()), ("x-endpoint-auth", KEY));
        let t = Target::of(&c);
        assert_eq!((t.route, t.url, t.key), (Route::ConfiguredEndpointV1, c.endpoint.as_str(), KEY));
        assert_eq!(t.codes, ErrorCodes::NONE, "no further codes unless configured");
        // Headers and codes are optional; the direct auth styles are the vendors'.
        assert!(RouteConfig::parse(&format!(r#"{{"route":"direct","endpoint":"https://gw.example.test/v1/responses","auth":"x-api-key","key":"{KEY}"}}"#)).is_ok());
        assert_eq!(Auth::direct(Vendor::Openai).header(), "authorization");
        assert_eq!(Auth::direct(Vendor::Anthropic).header(), "x-api-key");
        assert!(Target::direct(Vendor::WorkersAi, KEY).is_none());
        let d = Target::direct(Vendor::Anthropic, KEY).unwrap();
        assert_eq!((d.url, d.codes), ("https://api.anthropic.com/v1/messages", ErrorCodes::NONE));
        // Further error codes reach the call's target.
        let c = RouteConfig::parse(&format!(
            r#"{{"route":"configured_endpoint_v1","endpoint":"https://frontier.example.test/v1/responses","auth":"bearer","key":"{KEY}","rate_limit_codes":"test_slow_down,test.try-later","unsupported_codes":"test_not_offered"}}"#
        ))
        .unwrap();
        assert_eq!(Target::of(&c).codes, ErrorCodes { rate_limit: "test_slow_down,test.try-later", unsupported: "test_not_offered" });
    }

    #[test]
    fn a_bad_endpoint_secret_is_refused_without_its_text() {
        let bad_endpoints = [
            "http://frontier.example.test/v1/messages",
            "https://frontier.example.test",
            "https://frontier.example.test/",
            "https://user:pw@frontier.example.test/v1/messages",
            "https://frontier.example.test/v1/messages?x=1",
            "https://frontier.example.test/v1/messages#f",
            "https://frontier example.test/v1/messages",
            "https:///v1/messages",
        ];
        for e in bad_endpoints {
            assert!(RouteConfig::parse(&cfg(e, "[]")).is_err(), "{e}");
        }
        let bad_headers = [
            r#"[["authorization","x"]]"#,
            r#"[["X-API-KEY","x"]]"#,
            r#"[["content-type","x"]]"#,
            r#"[["host","x"]]"#,
            r#"[["x-a","b"],["X-A","c"]]"#,
            r#"[["x a","b"]]"#,
            r#"[["x-a",""]]"#,
            r#"[["x-a"," b"]]"#,
            r#"[["x-a","b\r\nx-evil: 1"]]"#,
            r#"[["x-a","é"]]"#,
        ];
        for h in bad_headers {
            assert!(RouteConfig::parse(&cfg("https://frontier.example.test/v1/messages", h)).is_err(), "{h}");
        }
        for codes in ["a,,b", ",a", "a,", "a b", "a;b", "é"] {
            let v = format!(r#"{{"route":"configured_endpoint_v1","endpoint":"https://a.test/v1","auth":"bearer","key":"{KEY}","rate_limit_codes":"{codes}"}}"#);
            assert!(RouteConfig::parse(&v).is_err(), "{codes:?}");
            let v = format!(r#"{{"route":"configured_endpoint_v1","endpoint":"https://a.test/v1","auth":"bearer","key":"{KEY}","unsupported_codes":"{codes}"}}"#);
            assert!(RouteConfig::parse(&v).is_err(), "{codes:?}");
        }
        for v in [
            r#"{"route":"elsewhere","endpoint":"https://a.test/v1","auth":"bearer","key":"k"}"#.to_owned(),
            format!(r#"{{"route":"direct","endpoint":"https://a.test/v1","auth":"basic","key":"{KEY}"}}"#),
            r#"{"route":"direct","endpoint":"https://a.test/v1","auth":"bearer","key":"has space"}"#.to_owned(),
            format!(r#"{{"route":"direct","endpoint":"https://a.test/v1","auth":"bearer","key":"{KEY}","extra":1}}"#),
            "not json".to_owned(),
            format!("{}{}", cfg("https://a.test/v1", "[]"), " ".repeat(SECRET_BYTES)),
        ] {
            match RouteConfig::parse(&v) {
                Ok(_) => panic!("accepted"),
                // The reason is fixed text: neither the key nor any value.
                Err(Refused(why)) => assert!(!why.contains(KEY) && !why.contains("a.test")),
            }
        }
    }
}
