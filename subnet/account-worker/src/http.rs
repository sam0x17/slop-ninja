//! Requests and answers as the handlers see them, and their builders.
//!
//! Every answer leaves with `Cache-Control: private, no-store`,
//! `X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer`
//! unless it sets its own `Cache-Control` (the avatar). API refusals are
//! one JSON object ([`ApiRefusal`]); form endpoints answer 303 to their page
//! with `?e=<code>`. Every `Location` is the configured site origin plus a
//! path: no answer redirects off the origin.

use std::collections::BTreeMap;

use serde::Serialize;
use slop_ninja_detect_page::{ApiRefusal, Origin, RefusalCode};

/// One request.
#[derive(Clone, Debug, Default)]
pub struct Req {
    pub method: String,
    pub path: String,
    /// The raw query string, without `?`.
    pub query: String,
    /// Header names lowercase, in order.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Req {
    /// The value of a header (any case), when it appears exactly once.
    pub fn header(&self, name: &str) -> Option<&str> {
        let mut it = self.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name));
        let first = it.next()?;
        if it.next().is_some() { None } else { Some(first.1.as_str()) }
    }

    pub fn has_header(&self, name: &str) -> bool {
        self.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
    }

    pub fn cookie(&self) -> Option<&str> {
        self.header("cookie")
    }

    /// The decoded query pairs (repeats refused: `None`).
    pub fn query_pairs(&self) -> Option<BTreeMap<String, String>> {
        pairs(self.query.as_bytes())
    }
}

/// One answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn empty(status: u16) -> Resp {
        Resp { status, headers: Vec::new(), body: Vec::new() }.finish()
    }

    pub fn json<T: Serialize>(status: u16, value: &T) -> Resp {
        let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
        Resp { status, headers: vec![("content-type".into(), "application/json; charset=utf-8".into())], body }.finish()
    }

    /// An API refusal with its code's status.
    pub fn refusal(code: RefusalCode) -> Resp {
        Resp::json(code.status(), &ApiRefusal::new(code))
    }

    /// A redirect to `path` on the site origin.
    pub fn redirect(status: u16, site: &Origin, path: &str) -> Resp {
        Resp { status, headers: vec![("location".into(), site.url(path))], body: Vec::new() }.finish()
    }

    /// A redirect to an absolute URL built elsewhere (a provider's
    /// authorize endpoint).
    pub fn redirect_to(url: String) -> Resp {
        Resp { status: 302, headers: vec![("location".into(), url)], body: Vec::new() }.finish()
    }

    /// 303 to `page` with `?e=<code>` (or `&e=` when `page` has a query).
    pub fn form_error(site: &Origin, page: &str, code: RefusalCode) -> Resp {
        let sep = if page.contains('?') { '&' } else { '?' };
        Resp::redirect(303, site, &format!("{page}{sep}e={}", code.as_str()))
    }

    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Resp {
        let name = name.to_ascii_lowercase();
        self.headers.retain(|(k, _)| *k != name || name == "set-cookie");
        self.headers.push((name, value.into()));
        self
    }

    pub fn with_cookie(mut self, set_cookie: String) -> Resp {
        self.headers.push(("set-cookie".into(), set_cookie));
        self
    }

    pub fn with_cookies(mut self, set_cookies: impl IntoIterator<Item = String>) -> Resp {
        for c in set_cookies {
            self.headers.push(("set-cookie".into(), c));
        }
        self
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn set_cookies(&self) -> Vec<&str> {
        self.headers.iter().filter(|(k, _)| k == "set-cookie").map(|(_, v)| v.as_str()).collect()
    }

    /// The default headers, where not already set.
    fn finish(mut self) -> Resp {
        for (k, v) in [
            ("cache-control", "private, no-store"),
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
        ] {
            if !self.headers.iter().any(|(h, _)| h == k) {
                self.headers.push((k.into(), v.into()));
            }
        }
        self
    }
}

/// Decodes `application/x-www-form-urlencoded` pairs; a repeated name
/// refuses the whole body (`None`).
pub fn pairs(bytes: &[u8]) -> Option<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (k, v) in url::form_urlencoded::parse(bytes) {
        if out.insert(k.into_owned(), v.into_owned()).is_some() {
            return None;
        }
    }
    Some(out)
}

/// A form body whose fields are all in `allowed`: any other field, a
/// repeated one or a body that is not form encoding is refused.
pub fn form(body: &[u8], allowed: &[&str]) -> Result<BTreeMap<String, String>, RefusalCode> {
    let fields = pairs(body).ok_or(RefusalCode::MalformedRequest)?;
    if fields.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(RefusalCode::MalformedRequest);
    }
    Ok(fields)
}

/// A JSON body of type `T` (unknown fields are the type's own rule).
pub fn json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, RefusalCode> {
    serde_json::from_slice(body).map_err(|_| RefusalCode::MalformedRequest)
}

/// The UTC day of a Unix millisecond instant, `YYYY-MM-DD`.
pub fn utc_day(ms: u64) -> String {
    slop_ninja_auth_core::time::rfc3339_ms(ms)[..10].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_carry_the_default_headers_and_one_location_origin() {
        let site = Origin::parse("https://site.example").unwrap();
        let r = Resp::form_error(&site, "/signin/", RefusalCode::State);
        assert_eq!(r.status, 303);
        assert_eq!(r.header("location"), Some("https://site.example/signin/?e=state"));
        assert_eq!(r.header("cache-control"), Some("private, no-store"));
        assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
        let r = Resp::form_error(&site, "/signin/?step_up=1", RefusalCode::Session);
        assert_eq!(r.header("location"), Some("https://site.example/signin/?step_up=1&e=session"));
    }

    #[test]
    fn forms_refuse_repeats_and_strangers() {
        assert!(form(b"action=connect", &["action"]).is_ok());
        assert_eq!(form(b"action=connect&action=merge", &["action"]), Err(RefusalCode::MalformedRequest));
        assert_eq!(form(b"action=connect&provider=x", &["action"]), Err(RefusalCode::MalformedRequest));
        assert_eq!(utc_day(1_794_547_800_000), "2026-11-13");
    }
}
