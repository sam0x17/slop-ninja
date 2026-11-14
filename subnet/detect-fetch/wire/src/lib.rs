//! The contract between the detect Worker and the fetch Worker
//! (`slop_ninja_detect_fetch_v1`), shared by path.
//!
//! The detect Worker reaches the fetch Worker only through its service
//! binding `DETECT_FETCH`:
//!
//! - `POST /robots` [`RobotsRequest`] → [`RobotsAnswer`]: one robots.txt
//!   read for an origin, asked by the `DetectHost` object when its cached
//!   rules are stale. The answer carries the parsed group that applies to
//!   SlopNinjaBot ([`robots::RobotsRules`]), which the object stores and
//!   evaluates per path ([`robots::RobotsRules::decide`]).
//! - `POST /fetch` [`FetchRequest`] → [`FetchAnswer`]: one page fetch and
//!   extraction under the politeness slot `DetectHost` handed out, after the
//!   robots rules allowed the URL. Redirect hops are re-checked from the top
//!   (the URL rules, the security screen, and robots.txt for every new
//!   origin) by the fetch Worker itself.
//!
//! - `POST /x/post` [`XPostRequest`] → [`XPostAnswer`]: one check of a post
//!   on X from its link, through X's free embed (oEmbed): the text of the
//!   embed's quote when the embed shows the post in full (rule `x_text_v1`),
//!   or why not. The only X host the fetch Worker ever requests is
//!   `publish.x.com`, and only through this route and the next.
//! - `POST /x/check` [`XCheckRequest`] → [`XCheckAnswer`]: the daily
//!   existence check of a batch of posts, called at most once per sweep
//!   message: the two control posts first, then each id in the order given,
//!   at most [`X_CHECK_K`] requests in flight, starting none after the first
//!   `unexpected` or `failed` answer.
//!
//! Every answer is content-free except the extracted text, the page's own
//! metadata and its final and canonical URLs. A refusal never echoes input.
//! An HTTP status other than 200 means the call itself failed (400 a
//! malformed request, 404 an unknown route, 503 busy or not configured):
//! the caller may retry once.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod robots;

use serde::{Deserialize, Serialize};

/// The contract's version.
pub const FETCH_WIRE_V: &str = "slop_ninja_detect_fetch_v1";
/// The binding-only origin the detect Worker addresses (service bindings
/// ignore the host; the fetch Worker checks it anyway).
pub const FETCH_ORIGIN: &str = "https://detect-fetch.internal";
pub const FETCH_HOST: &str = "detect-fetch.internal";
pub const FETCH_PATH: &str = "/fetch";
pub const ROBOTS_PATH: &str = "/robots";
pub const X_POST_PATH: &str = "/x/post";
pub const X_CHECK_PATH: &str = "/x/check";
/// Workers' limit on simultaneous outgoing connections: the existence check
/// keeps at most this many oEmbed requests in flight.
pub const X_CHECK_K: usize = 6;

/// The crawler's product token (robots.txt groups, `X-Robots-Tag`, meta).
pub const PRODUCT_TOKEN: &str = "SlopNinjaBot";
/// The crawler's user agent.
pub const USER_AGENT: &str = "SlopNinjaBot/1.0 (+https://slop.ninja/bot/)";
/// The fixed `Accept` header of page fetches.
pub const ACCEPT: &str = "text/html, application/xhtml+xml, text/plain";

/// The largest request body either route accepts.
pub const REQUEST_BYTES: usize = 16 * 1024;
/// The longest slot id.
pub const SLOT_BYTES: usize = 64;
/// The most redirect hops of a page fetch (the Fetch standard's 20).
pub const MAX_HOPS: u32 = 20;
/// The most redirect hops of a robots.txt fetch (RFC 9309 section 2.3.1.2).
pub const ROBOTS_HOPS: u32 = 5;

/// Limits the caller sets for one fetch. The fetch Worker clamps each to
/// its own bound (`html_bound` to its `HTML_BOUND`, `hops` to [`MAX_HOPS`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// The largest stripped HTML (bytes) the extraction may hold.
    pub html_bound: u64,
    /// One wall-clock budget for the whole fetch: connect, hops, robots of
    /// new origins, body and extraction (risk F12).
    pub wait_ms: u64,
    pub hops: u32,
}

/// Validators for a re-check ("Check the page again").
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conditional {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchRequest {
    /// The canonical URL the detect Worker admitted.
    pub url: String,
    /// The `DetectHost` slot this fetch runs under (opaque, content-free).
    pub slot: String,
    pub limits: Limits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditional: Option<Conditional>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RobotsRequest {
    /// `scheme://host` of the origin (no path, port or userinfo). An origin
    /// on an X host (`x.com`, `twitter.com`, `t.co`, `twimg.com` or a
    /// subdomain) is refused with 400 and never requested.
    pub origin: String,
    pub wait_ms: u64,
}

/// How a page fetch ended. [`Outcome::refusal`] maps it to the code the
/// pages print; [`Outcome::code`] is the content-free metrics value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Text extracted.
    Ok,
    /// A conditional re-check answered 304.
    NotModified,
    /// robots.txt disallows a hop, or asks for no AI input.
    Robots,
    /// The page asks not to be indexed (meta, `X-Robots-Tag`, `noai`, a TDM
    /// reservation, `search=no`) or its author opted out of unauthenticated
    /// viewers.
    PrivateSource,
    /// A login wall, paywall or bot challenge.
    LoginWall,
    /// 404 or 410.
    NotFound,
    /// Any other failure to get the page (status, transport, a site that
    /// blocks Cloudflare's network).
    FetchFailed,
    /// The wall-clock budget ran out.
    TooSlow,
    UnsupportedType,
    /// The stripped HTML exceeds the bound.
    PageTooLarge,
    /// The HTML nests deeper than the depth bound.
    TooDeep,
    TooManyRedirects,
    /// A redirect hop fails the URL rules.
    RedirectRefused,
    /// An `https` hop redirects to `http`.
    Downgrade,
    /// A redirect hop's host is listed as malware or phishing.
    SecurityRisk,
    /// No readable main text.
    ExtractFailed,
    /// The request's URL fails the URL rules (the detect Worker checks first,
    /// so this is a caller defect).
    BadUrl,
    /// A platform the fetch Worker does not read by URL (X before phase 2,
    /// paste-only platforms), including a URL or redirect hop on any X host
    /// (`x.com`, `twitter.com`, `t.co`, `twimg.com` or a subdomain), which
    /// the web fetcher never requests.
    Unsupported,
}

impl Outcome {
    pub fn code(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::NotModified => "not_modified",
            Outcome::Robots => "robots",
            Outcome::PrivateSource => "private_source",
            Outcome::LoginWall => "login_wall",
            Outcome::NotFound => "not_found",
            Outcome::FetchFailed => "fetch_failed",
            Outcome::TooSlow => "too_slow",
            Outcome::UnsupportedType => "unsupported_type",
            Outcome::PageTooLarge => "page_too_large",
            Outcome::TooDeep => "too_deep",
            Outcome::TooManyRedirects => "too_many_redirects",
            Outcome::RedirectRefused => "redirect_refused",
            Outcome::Downgrade => "downgrade",
            Outcome::SecurityRisk => "security_risk",
            Outcome::ExtractFailed => "extract_failed",
            Outcome::BadUrl => "bad_url",
            Outcome::Unsupported => "unsupported",
        }
    }

    /// The refusal code the pages print (DESIGN 4.3's URL rows), or `None`
    /// for a fetch that produced text or found the page unchanged.
    pub fn refusal(self) -> Option<&'static str> {
        Some(match self {
            Outcome::Ok | Outcome::NotModified => return None,
            Outcome::Robots => "robots",
            Outcome::PrivateSource => "private_source",
            Outcome::LoginWall => "login_wall",
            Outcome::TooSlow => "too_slow",
            Outcome::UnsupportedType => "unsupported_type",
            Outcome::PageTooLarge | Outcome::TooDeep => "page_too_large",
            Outcome::SecurityRisk => "security_risk",
            Outcome::BadUrl => "bad_url",
            Outcome::Unsupported => "paste_only",
            Outcome::NotFound
            | Outcome::FetchFailed
            | Outcome::TooManyRedirects
            | Outcome::RedirectRefused
            | Outcome::Downgrade
            | Outcome::ExtractFailed => "fetch_failed",
        })
    }
}

/// Which adapter read the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Adapter {
    Web,
    Bluesky,
}

/// The page's own metadata (hostile input to every renderer: risk F16).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// As the page wrote it (`article:published_time`, JSON-LD, or the
    /// post's `createdAt`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// `nosnippet`: no excerpt may be shown.
    #[serde(default)]
    pub nosnippet: bool,
    #[serde(default)]
    pub noarchive: bool,
    /// The Bluesky author's DID (the source key `bsky:<did>/<rkey>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub did: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchAnswer {
    pub v: String,
    pub outcome: Outcome,
    pub adapter: Adapter,
    /// The URL whose body was read (after redirects), canonical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    /// The page's `<link rel=canonical>`, when it passes the URL rules
    /// (canonical form); the detect Worker uses it only on the same
    /// registrable domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    /// Paragraphs joined by a blank line, whitespace collapsed inside them;
    /// the detect Worker applies `canon_v1`. Only with `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    /// The response's freshness lifetime (RFC 9111: `max-age`, `Expires`, or
    /// 10% of the age since `Last-Modified`), when it states one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_ms: Option<u64>,
    /// A `Retry-After` the site sent (politeness for `DetectHost`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    pub hops: u32,
    /// Decoded body bytes read and stripped HTML bytes held.
    pub body_bytes: u64,
    pub stripped_bytes: u64,
    /// The extractor's version (stored with the run).
    pub extract_v: String,
}

/// How a robots.txt read ended (RFC 9309 section 2.3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RobotsOutcome {
    /// 2xx: the parsed rules apply.
    Fetched,
    /// 4xx ("unavailable"): everything is allowed.
    Unavailable,
    /// 5xx, a network failure or the budget ran out ("unreachable"):
    /// everything is disallowed.
    Unreachable,
    /// A redirect hop failed the URL rules or the security screen: the
    /// origin is treated as unreachable.
    Refused,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RobotsAnswer {
    pub v: String,
    pub outcome: RobotsOutcome,
    /// The rules to store and evaluate: parsed (fetched), allow-all
    /// (unavailable) or disallow-all (unreachable, refused).
    pub rules: robots::RobotsRules,
    /// How long the answer may be cached (RFC 9309 section 2.4: at most
    /// 24 hours, less when the response says so).
    pub cache_ms: u64,
    pub hops: u32,
}

/// `/x/post`: one post, by id. `deadline_ms` is the time the request may
/// take (`X_CALL_MS`, the URL flow's fetch wait); the request aborts past it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XPostRequest {
    pub id: u64,
    pub deadline_ms: u64,
}

/// What X's embed showed of one post.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum XPostAnswer {
    /// The embed showed the post in full: the text of its quote (rule
    /// `x_text_v1`: reply mentions and trailing media links cut, nothing
    /// after the quote's paragraph read). It never holds the author's name,
    /// handle or date line.
    Text { text: String },
    /// The quote ends in "…": the embed cut the post. No text is returned.
    Long,
    /// HTTP 404 or 403 with the embed's own JSON error.
    Unavailable,
    /// A 429, a 5xx, the deadline or a network error.
    Busy,
    /// Anything else: HTML, another status or JSON shape, a body past the
    /// bound, or markup the rule does not know.
    Unexpected,
}

/// `/x/check`: a batch of posts by id, in the order to ask them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XCheckRequest {
    pub ids: Vec<u64>,
    /// The time each request may take (`X_CALL_MS`).
    pub deadline_ms: u64,
}

/// Which control failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum XControl {
    /// The live control (`X_CONTROL_ID`) did not answer `present`, or its id
    /// is missing or a placeholder.
    Live,
    /// The gone control (`X_GONE_CONTROL_ID`) did not answer `gone`, or its
    /// id is missing or a placeholder.
    Gone,
}

/// How a failed request failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum XFailure {
    Timeout,
    #[serde(rename = "429")]
    TooMany,
    #[serde(rename = "5xx")]
    Server,
    Network,
}

/// One id's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case", deny_unknown_fields)]
pub enum XExists {
    /// HTTP 200, `application/json`, an oEmbed object of type `rich` with an
    /// `html` string (not read).
    Present,
    /// HTTP 404 or 403 whose body is the embed's own JSON error.
    Gone,
    /// Any other answer.
    Unexpected,
    Failed {
        why: XFailure,
    },
    /// No request was made for it: the route stopped after an `unexpected`
    /// or `failed` answer.
    NotChecked,
}

/// The batch's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum XCheckAnswer {
    /// A control did not answer as it must: no other request was made.
    ControlsFailed { control: XControl },
    /// Both controls passed: one answer per id, in the order asked.
    Checked { answers: Vec<XExists> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_and_refuse_unknown_fields() {
        let r = FetchRequest {
            url: "https://example.com/a".into(),
            slot: "s1".into(),
            limits: Limits { html_bound: 1_000_000, wait_ms: 15_000, hops: 20 },
            conditional: None,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"url":"https://example.com/a","slot":"s1","limits":{"html_bound":1000000,"wait_ms":15000,"hops":20}}"#);
        assert_eq!(serde_json::from_str::<FetchRequest>(&s).unwrap(), r);
        assert!(
            serde_json::from_str::<FetchRequest>(r#"{"url":"u","slot":"s","limits":{"html_bound":1,"wait_ms":1,"hops":1},"x":1}"#).is_err()
        );
    }

    #[test]
    fn every_outcome_has_a_code_and_a_page_refusal() {
        use Outcome::*;
        for o in [
            Ok,
            NotModified,
            Robots,
            PrivateSource,
            LoginWall,
            NotFound,
            FetchFailed,
            TooSlow,
            UnsupportedType,
            PageTooLarge,
            TooDeep,
            TooManyRedirects,
            RedirectRefused,
            Downgrade,
            SecurityRisk,
            ExtractFailed,
            BadUrl,
            Unsupported,
        ] {
            let json = serde_json::to_string(&o).unwrap();
            assert_eq!(json, format!("\"{}\"", o.code()));
            assert_eq!(o.refusal().is_none(), matches!(o, Ok | NotModified));
        }
    }

    #[test]
    fn the_x_routes_round_trip() {
        let r = XPostRequest { id: 2, deadline_ms: 7000 };
        assert_eq!(serde_json::to_string(&r).unwrap(), r#"{"id":2,"deadline_ms":7000}"#);
        for (a, json) in [
            (XPostAnswer::Text { text: "a b".into() }, r#"{"outcome":"text","text":"a b"}"#),
            (XPostAnswer::Long, r#"{"outcome":"long"}"#),
            (XPostAnswer::Unavailable, r#"{"outcome":"unavailable"}"#),
            (XPostAnswer::Busy, r#"{"outcome":"busy"}"#),
            (XPostAnswer::Unexpected, r#"{"outcome":"unexpected"}"#),
        ] {
            assert_eq!(serde_json::to_string(&a).unwrap(), json);
            assert_eq!(serde_json::from_str::<XPostAnswer>(json).unwrap(), a);
        }
        let c = XCheckAnswer::Checked {
            answers: vec![
                XExists::Present,
                XExists::Gone,
                XExists::Unexpected,
                XExists::Failed { why: XFailure::Server },
                XExists::Failed { why: XFailure::TooMany },
                XExists::NotChecked,
            ],
        };
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(
            json,
            r#"{"outcome":"checked","answers":[{"answer":"present"},{"answer":"gone"},{"answer":"unexpected"},{"answer":"failed","why":"5xx"},{"answer":"failed","why":"429"},{"answer":"not_checked"}]}"#
        );
        assert_eq!(serde_json::from_str::<XCheckAnswer>(&json).unwrap(), c);
        let f = XCheckAnswer::ControlsFailed { control: XControl::Gone };
        assert_eq!(serde_json::to_string(&f).unwrap(), r#"{"outcome":"controls_failed","control":"gone"}"#);
        assert!(serde_json::from_str::<XPostAnswer>(r#"{"outcome":"text","text":"a","author":"b"}"#).is_err());
        assert!(serde_json::from_str::<XCheckRequest>(r#"{"ids":[1],"deadline_ms":1,"url":"x"}"#).is_err());
    }
}
