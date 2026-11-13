//! Dev only. The stub Workers of the local dev session (`stub-scorer`,
//! `auth-stub`, `fixture-site`) answer from pure functions of a [`Call`],
//! so native tests cover every answer; the [`cf`] adapter (wasm32) turns a
//! Workers request into a call and a [`Reply`] into a response. Nothing here
//! is deployed: the stubs exist only in `wrangler dev` sessions.

use serde::Serialize;

/// One request, as the stubs read it.
#[derive(Clone, Debug, Default)]
pub struct Call {
    pub method: String,
    /// The URL's host, lowercase, without the port (`""` when absent).
    pub host: String,
    pub path: String,
    /// Decoded query pairs, in order.
    pub query: Vec<(String, String)>,
    /// Header names lowercase, in order.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Call {
    pub fn new(method: &str, path: &str) -> Call {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        Call {
            method: method.to_owned(),
            path: path.to_owned(),
            query: pairs(query.as_bytes()),
            ..Call::default()
        }
    }

    pub fn host(mut self, host: &str) -> Call {
        self.host = host.to_ascii_lowercase();
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Call {
        self.headers.push((name.to_ascii_lowercase(), value.to_owned()));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Call {
        self.body = body.into();
        self
    }

    /// A form body (`application/x-www-form-urlencoded`).
    pub fn form(self, fields: &[(&str, &str)]) -> Call {
        let body = form_urlencoded::Serializer::new(String::new()).extend_pairs(fields).finish();
        self.header("content-type", "application/x-www-form-urlencoded").body(body)
    }

    /// A JSON body.
    pub fn json(self, v: &serde_json::Value) -> Call {
        self.header("content-type", "application/json").body(v.to_string())
    }

    /// The first value of query key `k`.
    pub fn q(&self, k: &str) -> Option<&str> {
        self.query.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    /// The first header named `name` (any case).
    pub fn h(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str())
    }

    /// The body's form fields, when the body is form-encoded.
    pub fn form_fields(&self) -> Vec<(String, String)> {
        pairs(&self.body)
    }

    /// The JSON body, if it parses.
    pub fn json_body(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

/// Decoded `application/x-www-form-urlencoded` pairs.
pub fn pairs(raw: &[u8]) -> Vec<(String, String)> {
    form_urlencoded::parse(raw).map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

/// The first value of `k` in decoded pairs.
pub fn field<'a>(fields: &'a [(String, String)], k: &str) -> Option<&'a str> {
    fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

/// Form-encodes pairs for a URL query or a body.
pub fn encode(fields: &[(&str, &str)]) -> String {
    form_urlencoded::Serializer::new(String::new()).extend_pairs(fields).finish()
}

/// A reply body.
#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    Bytes(Vec<u8>),
    /// `head`, then `unit` repeated and cut so the whole body is exactly
    /// `len` bytes, then nothing: large bodies are streamed, never built.
    Sized { head: Vec<u8>, unit: Vec<u8>, len: u64 },
}

impl Body {
    pub fn len(&self) -> u64 {
        match self {
            Body::Bytes(b) => b.len() as u64,
            Body::Sized { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The whole body (tests; small bodies only).
    pub fn to_vec(&self) -> Vec<u8> {
        match self {
            Body::Bytes(b) => b.clone(),
            Body::Sized { .. } => {
                let mut out = Vec::new();
                for c in self.chunks(64 * 1024) {
                    out.extend_from_slice(&c);
                }
                out
            }
        }
    }

    /// The body in pieces of at most `max` bytes.
    pub fn chunks(&self, max: usize) -> Vec<Vec<u8>> {
        match self {
            Body::Bytes(b) => b.chunks(max.max(1)).map(<[u8]>::to_vec).collect(),
            Body::Sized { head, unit, len } => {
                let max = max.max(1) as u64;
                let mut out = Vec::new();
                let mut at = 0u64;
                while at < *len {
                    let n = (*len - at).min(max);
                    out.push((at..at + n).map(|i| byte_at(head, unit, i)).collect());
                    at += n;
                }
                out
            }
        }
    }
}

fn byte_at(head: &[u8], unit: &[u8], i: u64) -> u8 {
    let h = head.len() as u64;
    if i < h {
        return head[i as usize];
    }
    if unit.is_empty() {
        return b' ';
    }
    unit[((i - h) % unit.len() as u64) as usize]
}

/// One answer: status, headers, body and an optional delay before it.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Body,
    pub delay_ms: u64,
}

impl Reply {
    pub fn new(status: u16) -> Reply {
        Reply { status, headers: vec![("x-content-type-options".into(), "nosniff".into())], body: Body::Bytes(Vec::new()), delay_ms: 0 }
    }

    pub fn json<T: Serialize>(status: u16, v: &T) -> Reply {
        let body = serde_json::to_vec(v).unwrap_or_default();
        Reply::new(status).with_header("content-type", "application/json").with_body(body)
    }

    pub fn text(status: u16, s: &str) -> Reply {
        Reply::new(status).with_header("content-type", "text/plain; charset=utf-8").with_body(s.as_bytes().to_vec())
    }

    pub fn html(status: u16, s: &str) -> Reply {
        Reply::new(status).with_header("content-type", "text/html; charset=utf-8").with_body(s.as_bytes().to_vec())
    }

    pub fn redirect(status: u16, location: &str) -> Reply {
        Reply::new(status).with_header("location", location)
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.to_ascii_lowercase(), value.to_owned()));
        self
    }

    pub fn with_body(mut self, body: Vec<u8>) -> Reply {
        self.body = Body::Bytes(body);
        self
    }

    pub fn with_delay(mut self, ms: u64) -> Reply {
        self.delay_ms = ms;
        self
    }

    /// The first header named `name`.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str())
    }

    /// The body as JSON (tests).
    pub fn body_json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body.to_vec()).unwrap_or(serde_json::Value::Null)
    }

    /// The body as text (tests).
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body.to_vec()).into_owned()
    }
}

/// Escapes text for HTML element content and attribute values.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(target_arch = "wasm32")]
pub mod cf {
    //! The Workers adapter.
    use super::{Body, Call, Reply};
    use std::time::Duration;
    use worker::{Delay, Headers, Request, Response, ResponseBuilder, Result};

    /// Streamed bodies go out in pieces of this size.
    const PIECE: usize = 64 * 1024;

    /// The call a request carries; a body over `max_body` bytes is cut to
    /// nothing and `oversize` set.
    pub async fn call_of(req: &mut Request, max_body: usize) -> Result<(Call, bool)> {
        let url = req.url()?;
        let headers = req.headers().entries().map(|(k, v)| (k.to_ascii_lowercase(), v)).collect::<Vec<_>>();
        let mut body = if matches!(req.method(), worker::Method::Get | worker::Method::Head) { Vec::new() } else { req.bytes().await.unwrap_or_default() };
        let oversize = body.len() > max_body;
        if oversize {
            body.clear();
        }
        Ok((
            Call {
                method: req.method().to_string(),
                host: url.host_str().unwrap_or("").to_ascii_lowercase(),
                path: url.path().to_owned(),
                query: url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect(),
                headers,
                body,
            },
            oversize,
        ))
    }

    /// The response for `reply`, after its delay.
    pub async fn response_of(reply: Reply) -> Result<Response> {
        if reply.delay_ms > 0 {
            Delay::from(Duration::from_millis(reply.delay_ms)).await;
        }
        let headers = Headers::new();
        for (k, v) in &reply.headers {
            headers.append(k, v)?;
        }
        let builder = ResponseBuilder::new().with_status(reply.status).with_headers(headers);
        match reply.body {
            Body::Bytes(b) if b.is_empty() => Ok(builder.empty()),
            Body::Bytes(b) => Ok(builder.fixed(b)),
            sized @ Body::Sized { .. } => {
                let pieces = sized.chunks(PIECE).into_iter().map(Ok::<Vec<u8>, worker::Error>);
                builder.from_stream(futures_util::stream::iter(pieces))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sized_bodies_are_exact_and_streamed_in_pieces() {
        let b = Body::Sized { head: b"<p>".to_vec(), unit: b"ab".to_vec(), len: 10 };
        assert_eq!(b.to_vec(), b"<p>ababab".iter().chain(b"a").copied().collect::<Vec<_>>());
        assert_eq!(b.chunks(4).iter().map(Vec::len).collect::<Vec<_>>(), vec![4, 4, 2]);
        let big = Body::Sized { head: Vec::new(), unit: b"x".to_vec(), len: 200_001 };
        assert_eq!(big.chunks(65_536).iter().map(|c| c.len() as u64).sum::<u64>(), 200_001);
    }

    #[test]
    fn calls_read_queries_forms_and_headers() {
        let c = Call::new("POST", "/token?a=1&b=x%20y").header("Content-Type", "x").form(&[("code", "a b&c")]);
        assert_eq!(c.q("b"), Some("x y"));
        assert_eq!(c.h("content-type"), Some("x"));
        assert_eq!(field(&c.form_fields(), "code"), Some("a b&c"));
        assert_eq!(escape("<a href=\"x\">'&'</a>"), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;");
    }
}
