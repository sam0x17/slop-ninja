//! One vendor call, from a [`GenCall`] to a [`GenReply`], over any
//! [`Transport`] (the Workers `fetch` on wasm32, a mock in tests).
//!
//! The call sends the request the eval Worker built, unchanged, with the
//! endpoint's key header added here. A non-200 answer is classified from
//! its status, `retry-after`, the presence of rate-limit headers and the
//! structured fields of an error body read within the request's line bound
//! ([`outcome::classify_http`]); the body itself is dropped. There is no
//! retry here: every class goes back to the eval Worker, which decides
//! (throttle, vendor pause, cell refusal, outage) from the class alone. A
//! 200 answer is read as SSE through the bounded reader ([`SseReader`]).
//!
//! The call's whole heap ([`iface::call_heap_bytes`]: the reader's buffers,
//! which also bound an error body, the read buffer and the reply) is
//! admitted once, before the key is used or anything is sent: a call the
//! isolate cannot hold answers a throttle without reaching the vendor, so
//! no vendor charge is ever incurred for an answer that is then dropped.
//!
//! The call goes to its [`Target`], the vendor's configured endpoint
//! ([`crate::route`]) with its URL, key header style, further headers and
//! error codes; without one nothing is sent
//! ([`OutcomeCode::KeyUnavailable`]). A request built for another route
//! than [`route::ROUTE`], this Worker's only one, is refused before anything
//! is sent ([`OutcomeCode::InterfaceMismatch`]: the manifest and this
//! Worker disagree, a deploy fault), since its shape is the route's.
//!
//! The key (and any further header value) is borrowed and only reaches the
//! transport's header list; nothing returned or recorded carries it, and no
//! vendor text is kept except the visible text of an answer.

use crate::route::{self, Auth, Target};
use slop_ninja_gen_core::iface;
use slop_ninja_gen_core::outcome::{self, GenCall, GenReply, OutcomeCode};
use slop_ninja_gen_core::request::VendorRequest;
use slop_ninja_gen_core::sse::SseReader;
use slop_ninja_gen_core::vendor::Vendor;

/// Why a transport step failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The connection, the request or a body read failed.
    Transport,
    /// The call's deadline passed.
    Timeout,
}

/// The status line and the headers a call reads of an answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Head {
    pub status: u16,
    pub retry_after: Option<String>,
    /// Any `x-ratelimit-*` or `anthropic-ratelimit-*` header is present.
    pub rate_headers: bool,
}

/// An answer's body, read into the caller's buffer.
#[allow(async_fn_in_trait)]
pub trait Body {
    /// Fills `out` from the body and returns the bytes written; 0 at the end.
    async fn read(&mut self, out: &mut [u8]) -> Result<usize, Fault>;
    /// Stops reading (the rest of the body is never read).
    fn cancel(&mut self);
}

/// Where a call goes.
#[allow(async_fn_in_trait)]
pub trait Transport {
    type Body: Body;
    /// `POST url` with `headers` and `body`, until `deadline_ms`.
    async fn post(&mut self, url: &str, headers: &[(&str, &str)], body: &str, deadline_ms: u64) -> Result<(Head, Self::Body), Fault>;
    /// The clock (milliseconds).
    fn now_ms(&self) -> u64;
    /// Admits `bytes` of heap in the call's scope, before anything is sent;
    /// `false` when the isolate cannot hold them (the call answers
    /// [`OutcomeCode::Throttle`] without contacting the vendor, no attempt
    /// used).
    fn admit(&mut self, bytes: usize) -> bool;
}

/// The read buffer: one WebAssembly page, the unit the arena allocates in.
pub const READ_BUFFER_BYTES: usize = iface::READ_BUFFER_BYTES;

/// Whether `key` can be a header value as it is: visible ASCII only (no
/// space, control character or non-ASCII byte). A key pasted with interior
/// whitespace or a line break is unavailable, never a header error.
pub fn key_ok(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|b| b.is_ascii_graphic())
}

/// The header list of one call. The key (and the configuration's further
/// header values) appear only here.
fn headers<'a>(r: &'a VendorRequest, t: &'a Target<'_>, auth: &'a str) -> Vec<(&'a str, &'a str)> {
    let mut h = vec![("content-type", "application/json"), ("accept", "text/event-stream"), (t.auth.header(), auth)];
    if r.vendor == Vendor::Anthropic {
        h.push(("anthropic-version", r.anthropic_version.as_deref().unwrap_or("")));
    }
    h.extend(t.headers.iter().map(|(n, v)| (n.as_str(), v.as_str())));
    h
}

/// The value of the key header: `Bearer <key>` for [`Auth::Bearer`], the
/// key itself for [`Auth::XApiKey`]. Zeroed on drop.
pub fn auth_value(auth: Auth, key: &str) -> zeroize::Zeroizing<String> {
    let mut s = zeroize::Zeroizing::new(String::with_capacity(key.len() + 7));
    if auth == Auth::Bearer {
        s.push_str("Bearer ");
    }
    s.push_str(key);
    s
}

/// Reads an error body within `bound` (admitted with the call); `None`
/// past it or on a fault.
async fn error_body<B: Body>(b: &mut B, bound: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    out.try_reserve_exact(bound).ok()?;
    let mut buf = vec![0u8; READ_BUFFER_BYTES];
    loop {
        match b.read(&mut buf).await {
            Ok(0) => return Some(out),
            Ok(n) => {
                if out.len() + n > bound {
                    b.cancel();
                    return None;
                }
                out.extend_from_slice(buf.get(..n)?);
            }
            Err(_) => return None,
        }
    }
}

/// Runs `call` over `t` to `target` (`None`: the vendor has no valid
/// configured endpoint).
pub async fn drive<T: Transport>(t: &mut T, call: &GenCall, target: Option<&Target<'_>>) -> GenReply {
    let t0 = t.now_ms();
    // The vendor's deadline leaves the reply its allowance within the
    // call's timeout, which the eval Worker enforces on the service call.
    let deadline = t0.saturating_add(iface::vendor_deadline_ms(call.timeout_ms));
    let r = &call.request;
    if r.vendor == Vendor::WorkersAi {
        // Workers AI runs in the eval Worker, never here.
        return GenReply::without_answer(OutcomeCode::CellRefused, r, 0);
    }
    let Some(target) = target.filter(|x| key_ok(x.key)) else {
        return GenReply::without_answer(OutcomeCode::KeyUnavailable, r, 0);
    };
    if r.route != route::ROUTE {
        // Built for another route than this Worker's only one: never sent.
        return GenReply::without_answer(OutcomeCode::InterfaceMismatch, r, 0);
    }
    if !iface::within(r) {
        return GenReply::without_answer(OutcomeCode::CellRefused, r, 0);
    }
    if !t.admit(iface::call_heap_bytes(r)) {
        return GenReply::without_answer(OutcomeCode::Throttle, r, 0);
    }
    let auth = auth_value(target.auth, target.key);
    let since = |t: &T| t.now_ms().saturating_sub(t0);
    let (head, mut body) = match t.post(target.url, &headers(r, target, &auth), &r.body, deadline).await {
        Ok(x) => x,
        Err(_) => {
            // Sent, and the vendor may have started: charged by the eval
            // Worker at its reservation bound.
            let mut g = GenReply::without_answer(OutcomeCode::Outage, r, since(t));
            g.sent = true;
            return g;
        }
    };
    if head.status != 200 {
        let err = error_body(&mut body, r.line_bound as usize).await;
        let (class, after) = outcome::classify_http(r.vendor, r.tier, head.status, head.retry_after.as_deref(), head.rate_headers, err.as_deref());
        let mut g = GenReply::without_answer(class, r, since(t));
        g.http_status = head.status;
        g.retry_after_ms = after;
        g.sent = true;
        return g;
    }
    let mut reader = SseReader::new(r.vendor, r.visible_bound, r.line_bound).with_codes(target.codes);
    let mut buf = vec![0u8; READ_BUFFER_BYTES];
    let mut first_token_ms = None;
    loop {
        match body.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let p = reader.feed(buf.get(..n).unwrap_or(&[]));
                if p.first_text && first_token_ms.is_none() {
                    first_token_ms = Some(since(t));
                }
                if p.done {
                    body.cancel();
                    break;
                }
            }
        }
    }
    let end = reader.finish();
    GenReply::from_stream(end, call, r, head.status, false, since(t), first_token_ms)
}

#[cfg(test)]
pub mod mock {
    //! A scripted vendor: each `post` takes the next scripted answer and
    //! records what was sent (the key header's value kept apart, so tests
    //! can check where it went).
    use super::*;

    pub struct Answer {
        pub head: Head,
        pub chunks: Vec<Vec<u8>>,
        /// The body read fails after the chunks.
        pub fail_after: bool,
    }

    impl Answer {
        pub fn ok(sse: &str, chunk: usize) -> Self {
            let chunks = sse.as_bytes().chunks(chunk.max(1)).map(<[u8]>::to_vec).collect();
            Answer { head: Head { status: 200, ..Head::default() }, chunks, fail_after: false }
        }
        pub fn status(status: u16, retry_after: Option<&str>, rate: bool, body: &str) -> Self {
            Answer {
                head: Head { status, retry_after: retry_after.map(str::to_owned), rate_headers: rate },
                chunks: vec![body.as_bytes().to_vec()],
                fail_after: false,
            }
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Sent {
        pub url: String,
        /// Every header but the key header.
        pub headers: Vec<(String, String)>,
        pub auth: Option<(String, String)>,
        pub body: String,
    }

    pub struct MockBody {
        chunks: std::collections::VecDeque<Vec<u8>>,
        fail_after: bool,
        pub reads: std::rc::Rc<std::cell::Cell<usize>>,
        pub cancelled: std::rc::Rc<std::cell::Cell<bool>>,
    }

    impl Body for MockBody {
        async fn read(&mut self, out: &mut [u8]) -> Result<usize, Fault> {
            self.reads.set(self.reads.get() + 1);
            match self.chunks.front_mut() {
                None if self.fail_after => Err(Fault::Transport),
                None => Ok(0),
                Some(c) => {
                    let n = c.len().min(out.len());
                    out[..n].copy_from_slice(&c[..n]);
                    c.drain(..n);
                    if c.is_empty() {
                        self.chunks.pop_front();
                    }
                    Ok(n)
                }
            }
        }
        fn cancel(&mut self) {
            self.cancelled.set(true);
        }
    }

    pub struct Mock {
        pub script: std::collections::VecDeque<Result<Answer, Fault>>,
        pub sent: Vec<Sent>,
        /// Each POST's deadline.
        pub deadlines: Vec<u64>,
        pub clock: std::cell::Cell<u64>,
        pub step_ms: u64,
        pub admit_ok: bool,
        pub admitted: Vec<usize>,
        pub reads: std::rc::Rc<std::cell::Cell<usize>>,
        pub cancelled: std::rc::Rc<std::cell::Cell<bool>>,
    }

    impl Mock {
        pub fn new(script: Vec<Result<Answer, Fault>>) -> Self {
            Mock {
                script: script.into(),
                sent: Vec::new(),
                deadlines: Vec::new(),
                clock: std::cell::Cell::new(1_000),
                step_ms: 7,
                admit_ok: true,
                admitted: Vec::new(),
                reads: Default::default(),
                cancelled: Default::default(),
            }
        }
    }

    impl Transport for Mock {
        type Body = MockBody;
        async fn post(&mut self, url: &str, headers: &[(&str, &str)], body: &str, deadline_ms: u64) -> Result<(Head, MockBody), Fault> {
            self.deadlines.push(deadline_ms);
            let key_header = |n: &str| n == "authorization" || n == "x-api-key";
            self.sent.push(Sent {
                url: url.to_owned(),
                headers: headers.iter().filter(|(n, _)| !key_header(n)).map(|(n, v)| ((*n).to_owned(), (*v).to_owned())).collect(),
                auth: headers.iter().find(|(n, _)| key_header(n)).map(|(n, v)| ((*n).to_owned(), (*v).to_owned())),
                body: body.to_owned(),
            });
            let a = self.script.pop_front().unwrap_or(Err(Fault::Transport))?;
            Ok((
                a.head,
                MockBody { chunks: a.chunks.into(), fail_after: a.fail_after, reads: self.reads.clone(), cancelled: self.cancelled.clone() },
            ))
        }
        fn now_ms(&self) -> u64 {
            let t = self.clock.get();
            self.clock.set(t + self.step_ms);
            t
        }
        fn admit(&mut self, bytes: usize) -> bool {
            self.admitted.push(bytes);
            self.admit_ok
        }
    }

    /// Polls a future that never waits (the mock answers at once).
    pub fn run<F: std::future::Future>(f: F) -> F::Output {
        let mut f = std::pin::pin!(f);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match f.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(v) => v,
            std::task::Poll::Pending => panic!("the mock transport never waits"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::{Answer, Mock, run};
    use super::*;
    use crate::route::{Configured, RouteConfig};
    use slop_ninja_gen_core::manifest::fixture::{SYSTEM_PREFIX, configured, measured, planned};
    use slop_ninja_gen_core::sse::fixture as sse;
    use slop_ninja_gen_core::vendor::{Effort, Route, Tier};
    use slop_ninja_gen_core::voice::fixture::library;

    const KEY: &str = "sk-TESTKEY-0123456789abcdefghijklmnopqrstuvwxyz";
    const OPENAI_URL: &str = "https://frontier.example.test/v1/responses";
    const ANTHROPIC_URL: &str = "https://frontier.example.test/v1/messages";

    /// A call built from a manifest whose vendors are both at a configured
    /// endpoint, this Worker's only route.
    fn call(model: &str, effort: Effort, n: u32) -> GenCall {
        let m = measured(configured(planned()));
        let (request, _) = slop_ninja_gen_core::measure::request(&m, &library(), model, effort, n).unwrap();
        GenCall { accepted_models: vec![model.to_owned()], request, timeout_ms: 285_000 }
    }

    /// A call built for the vendors' own APIs (the direct route), which this
    /// Worker never serves.
    fn direct_call(model: &str, effort: Effort, n: u32) -> GenCall {
        let m = measured(planned());
        let (request, _) = slop_ninja_gen_core::measure::request(&m, &library(), model, effort, n).unwrap();
        GenCall { accepted_models: vec![model.to_owned()], request, timeout_ms: 285_000 }
    }

    /// An endpoint secret with `auth` and no further headers or codes.
    fn plain(url: &str, auth: &str, key: &str) -> RouteConfig {
        RouteConfig::parse(&format!(r#"{{"route":"configured_endpoint_v1","endpoint":"{url}","auth":"{auth}","key":"{key}"}}"#)).unwrap()
    }

    /// The vendor's endpoint for `c`, bearer key, no further headers.
    fn endpoint_for(c: &GenCall) -> RouteConfig {
        plain(if c.request.vendor == Vendor::Openai { OPENAI_URL } else { ANTHROPIC_URL }, "bearer", KEY)
    }

    /// An endpoint with a further header and further error codes.
    fn endpoint(url: &str) -> RouteConfig {
        RouteConfig::parse(&format!(
            r#"{{"route":"configured_endpoint_v1","endpoint":"{url}","auth":"bearer","key":"{KEY}","headers":[["X-Endpoint-Auth","{KEY}"]],"rate_limit_codes":"test_slow_down","unsupported_codes":"test_not_offered"}}"#
        ))
        .unwrap()
    }

    /// Runs `c` against its vendor's endpoint.
    fn go(t: &mut Mock, c: &GenCall) -> GenReply {
        let ep = endpoint_for(c);
        run(drive(t, c, Some(&Target::of(&ep))))
    }

    fn json(g: &GenReply) -> String {
        serde_json::to_string(g).unwrap()
    }

    /// The vendor deadline leaves the reply its allowance within the call's
    /// timeout (the eval Worker's bound on the service call), so a vendor
    /// call that runs to it still answers with its own outcome.
    #[test]
    fn the_vendor_deadline_leaves_the_reply_its_allowance() {
        let c = call("gpt-6-luna", Effort::High, 2);
        let mut t = Mock::new(vec![Err(Fault::Timeout)]);
        let g = go(&mut t, &c);
        assert_eq!(t.deadlines, vec![1_000 + c.timeout_ms - iface::REPLY_ALLOWANCE_MS]);
        assert!(t.deadlines[0] < 1_000 + c.timeout_ms);
        assert_eq!(g.outcome, OutcomeCode::Outage);
        assert!(g.sent);
    }

    #[test]
    fn openai_golden_request_and_answer() {
        let c = call("gpt-6-luna", Effort::High, 2);
        let text = "Each spring the ferry firm repaints both of its boats, a job of roughly three weeks, with rowing club volunteers helping sand them down.";
        let mut t = Mock::new(vec![Ok(Answer::ok(&sse::openai("gpt-6-luna", text, "response.completed", None, "default"), 37))]);
        let g = go(&mut t, &c);
        assert_eq!(g.outcome, OutcomeCode::Answer, "{g:?}");
        assert_eq!(g.text.as_deref(), Some(text));
        assert_eq!(g.tier, Tier::Endpoint, "an endpoint call is priced at the endpoint tier");
        assert!(!g.flex_unavailable);
        assert_eq!(g.request_sha256, c.request.request_sha256);
        assert!(g.first_token_ms.is_some() && g.latency_ms > 0);
        let s = &t.sent[0];
        assert_eq!(s.url, OPENAI_URL);
        assert_eq!(s.headers, vec![("content-type".to_owned(), "application/json".to_owned()), ("accept".to_owned(), "text/event-stream".to_owned())]);
        assert_eq!(s.auth, Some(("authorization".to_owned(), format!("Bearer {KEY}"))));
        // The body is the eval Worker's request byte for byte.
        assert_eq!(s.body, c.request.body);
        let b: serde_json::Value = serde_json::from_str(&s.body).unwrap();
        assert!(b.get("service_tier").is_none(), "no Flex at a configured endpoint");
        assert_eq!(b["store"], false);
        assert_eq!(b["stream"], true);
        assert_eq!(b["reasoning"]["effort"], "high");
        assert!(b.get("temperature").is_none() && b.get("top_p").is_none());
        assert!(!json(&g).contains(KEY));
    }

    #[test]
    fn anthropic_golden_request_and_refusal() {
        let c = call("claude-opus-5-5", Effort::Max, 14);
        let ep = plain(ANTHROPIC_URL, "x-api-key", KEY);
        let mut t = Mock::new(vec![Ok(Answer::ok(&sse::anthropic("claude-opus-5-5", "", "refusal", Some("cyber")), 11))]);
        let g = run(drive(&mut t, &c, Some(&Target::of(&ep))));
        assert_eq!(g.outcome, OutcomeCode::Refusal);
        assert!(g.text.is_none());
        let s = &t.sent[0];
        assert_eq!(s.url, ANTHROPIC_URL);
        assert_eq!(s.auth, Some(("x-api-key".to_owned(), KEY.to_owned())));
        assert!(s.headers.contains(&("anthropic-version".to_owned(), "2026-10-21".to_owned())));
        let b: serde_json::Value = serde_json::from_str(&s.body).unwrap();
        assert_eq!(b["output_config"]["effort"], "max");
        for absent in ["thinking", "temperature", "top_p", "top_k", "fallbacks", "budget_tokens"] {
            assert!(b.get(absent).is_none(), "{absent}");
        }
        assert_eq!(b["messages"].as_array().unwrap().len(), 1, "no assistant prefill");
    }

    /// No call is ever repeated here: a 429 at the endpoint is classified
    /// once (no Flex tier, so no standard retry), and a request built for
    /// the vendor's own API, Flex included, is never sent.
    #[test]
    fn a_429_is_classified_once_and_a_direct_request_is_never_sent() {
        let c = call("gpt-6.1-sol", Effort::Low, 0);
        assert_eq!(c.request.tier, Tier::Endpoint);
        let unavailable = r#"{"error":{"type":"rate_limit_error","message":"Resource Unavailable"}}"#;
        for rate in [false, true] {
            let mut t = Mock::new(vec![Ok(Answer::status(429, None, rate, unavailable)), Ok(Answer::status(200, None, false, ""))]);
            let g = go(&mut t, &c);
            let want = outcome::classify_http(Vendor::Openai, Tier::Endpoint, 429, None, rate, Some(unavailable.as_bytes())).0;
            assert_eq!((g.outcome, t.sent.len(), g.flex_unavailable), (want, 1, false), "{rate}");
            assert_ne!(g.outcome, OutcomeCode::FlexUnavailable);
        }
        let d = direct_call("gpt-6.1-sol", Effort::Low, 0);
        assert_eq!((d.request.route, d.request.tier), (Route::Direct, Tier::Flex));
        let mut t = Mock::new(vec![]);
        let g = go(&mut t, &d);
        assert_eq!((g.outcome, g.sent, g.flex_unavailable), (OutcomeCode::InterfaceMismatch, false, false));
        assert!(t.sent.is_empty() && t.admitted.is_empty());
    }

    #[test]
    fn caps_quotas_keys_and_throttles_are_classified_without_vendor_text() {
        let echo = format!(r#"{{"error":{{"type":"invalid_request_error","message":"Incorrect API key provided: {KEY}"}}}}"#);
        let cases: Vec<(&str, Answer, OutcomeCode, Option<u64>)> = vec![
            (
                "claude-haiku-5-5",
                Answer::status(429, None, false, r#"{"type":"error","error":{"type":"rate_limit_error","message":"x","details":{"error_code":"enforced_spend_limit_reached"}}}"#),
                OutcomeCode::SpendCap,
                None,
            ),
            ("claude-haiku-5-5", Answer::status(429, Some("12"), true, r#"{"error":{"type":"rate_limit_error"}}"#), OutcomeCode::Throttle, Some(12_000)),
            ("gpt-6-luna", Answer::status(429, None, false, r#"{"error":{"type":"insufficient_quota","code":"insufficient_quota"}}"#), OutcomeCode::SpendCap, None),
            (
                "gpt-6-luna",
                Answer::status(429, Some("3"), true, r#"{"error":{"type":"requests","code":"rate_limit_exceeded"}}"#),
                OutcomeCode::Throttle,
                Some(3_000),
            ),
            (
                "claude-haiku-5-5",
                Answer::status(429, Some("Wed, 21 Oct 2026 07:28:00 GMT"), true, r#"{"error":{"type":"rate_limit_error"}}"#),
                OutcomeCode::Throttle,
                None,
            ),
            (
                "claude-fable-5-1",
                Answer::status(403, None, false, r#"{"type":"error","error":{"type":"permission_error","message":"no access"}}"#),
                OutcomeCode::CellRefused,
                None,
            ),
            ("claude-sonnet-5-5", Answer::status(301, None, false, ""), OutcomeCode::Outage, None),
            ("gpt-6-luna", Answer::status(302, None, false, ""), OutcomeCode::Outage, None),
            ("gpt-6-luna", Answer::status(401, None, false, &echo), OutcomeCode::KeyUnavailable, None),
            ("claude-sonnet-5-5", Answer::status(403, None, false, &echo), OutcomeCode::KeyUnavailable, None),
            ("claude-sonnet-5-5", Answer::status(400, None, false, &echo), OutcomeCode::CellRefused, None),
            ("claude-sonnet-5-5", Answer::status(529, None, false, r#"{"error":{"type":"overloaded_error"}}"#), OutcomeCode::Outage, None),
            ("gpt-6-luna", Answer::status(503, None, false, "<html>"), OutcomeCode::Outage, None),
        ];
        for (model, a, want, after) in cases {
            let effort = if model == "gpt-6-luna" { Effort::Medium } else { Effort::Low };
            let mut t = Mock::new(vec![Ok(a)]);
            let g = go(&mut t, &call(model, effort, 1));
            assert_eq!((g.outcome, g.retry_after_ms), (want, after), "{model}");
            assert_eq!(t.sent.len(), 1, "{model}: one POST, never a retry or a followed redirect");
            assert!(g.text.is_none() && !json(&g).contains("TESTKEY") && !json(&g).contains("Incorrect"));
        }
    }

    #[test]
    fn an_oversized_error_body_is_not_kept() {
        let c = call("gpt-6-luna", Effort::Low, 1);
        let big = "x".repeat(c.request.line_bound as usize + 1);
        let mut t = Mock::new(vec![Ok(Answer::status(400, None, false, &big))]);
        let g = go(&mut t, &c);
        assert_eq!(g.outcome, OutcomeCode::CellRefused);
        assert!(t.cancelled.get());
    }

    #[test]
    fn the_visible_bound_stops_reading() {
        let c = call("claude-sonnet-5-5", Effort::High, 12);
        let long = "word ".repeat(c.request.visible_bound as usize);
        let mut sse = sse::anthropic("claude-sonnet-5-5", &long, "end_turn", None);
        sse.push_str(&"data: {\"type\":\"ping\"}\n\n".repeat(10_000));
        let mut t = Mock::new(vec![Ok(Answer::ok(&sse, 1000))]);
        let g = go(&mut t, &c);
        assert_eq!(g.outcome, OutcomeCode::LengthBytes);
        assert!(t.cancelled.get(), "the rest of the stream is never read");
        assert!(t.reads.get() < sse.len() / 1000);
    }

    #[test]
    fn transport_faults_admission_and_keys() {
        let c = call("gpt-6-astra", Effort::Xhigh, 4);
        // A stream that breaks before its terminal event is an outage.
        let mut a = Answer::ok(&sse::openai("gpt-6-astra", "Partial text", "response.completed", None, "default"), 9);
        a.chunks.truncate(3);
        a.fail_after = true;
        let mut t = Mock::new(vec![Ok(a)]);
        assert_eq!(go(&mut t, &c).outcome, OutcomeCode::Outage);
        // The isolate cannot hold the call: a throttle before anything is
        // sent, so the vendor never bills an answer that would be dropped.
        let mut t = Mock::new(vec![Ok(Answer::ok(&sse::openai("gpt-6-astra", "x", "response.completed", None, "default"), 9))]);
        t.admit_ok = false;
        let g = go(&mut t, &c);
        assert_eq!((g.outcome, g.sent), (OutcomeCode::Throttle, false));
        assert!(t.sent.is_empty() && t.reads.get() == 0);
        assert_eq!(t.admitted, vec![slop_ninja_gen_core::iface::call_heap_bytes(&c.request)], "one admission, up front");
        // A key that cannot be a header value is unavailable, never sent
        // (the endpoint secret refuses it; a target holding one is refused here too).
        let ep = endpoint_for(&c);
        for bad in ["sk-with space", "sk-line\nbreak", "sk-\u{e9}", ""] {
            let target = Target { key: bad, ..Target::of(&ep) };
            let mut t = Mock::new(vec![]);
            assert_eq!(run(drive(&mut t, &c, Some(&target))).outcome, OutcomeCode::KeyUnavailable);
            assert!(t.sent.is_empty());
        }
        // A malformed anthropic-version is refused before anything is sent.
        let mut a = call("claude-opus-5-5", Effort::Low, 1);
        a.request.anthropic_version = Some("2026-10-21\r\nx-evil: 1".into());
        let mut t = Mock::new(vec![]);
        assert_eq!(go(&mut t, &a).outcome, OutcomeCode::CellRefused);
        assert!(t.sent.is_empty());
        // Sent calls are marked sent; a timeout after sending is too.
        let mut t = Mock::new(vec![Err(Fault::Timeout)]);
        let g = go(&mut t, &c);
        assert_eq!((g.outcome, g.sent, g.http_status), (OutcomeCode::Outage, true, 0));
        // Another model answered: never a label.
        let mut t = Mock::new(vec![Ok(Answer::ok(&sse::openai("gpt-6-luna", "Some text here.", "response.completed", None, "default"), 9))]);
        let g = go(&mut t, &c);
        assert_eq!(g.outcome, OutcomeCode::ModelMismatch);
        assert!(g.text.is_none() && g.model_reported.is_none());
        // Workers AI never runs here.
        let w = call("@cf/meta/llama-3.2-3b-instruct", Effort::Default, 0);
        let mut t = Mock::new(vec![]);
        assert_eq!(go(&mut t, &w).outcome, OutcomeCode::CellRefused);
        assert!(t.sent.is_empty());
    }

    /// A vendor without a valid endpoint secret is unavailable: every call
    /// answers `key_unavailable` and nothing is sent, whatever route the
    /// request was built for. There is no other route to fall back to.
    #[test]
    fn a_vendor_without_its_endpoint_is_unavailable_and_nothing_is_sent() {
        let broken = format!(r#"{{"route":"direct","endpoint":"{OPENAI_URL}","auth":"bearer","key":"{KEY}"}}"#);
        for secret in [None, Some("not json"), Some(broken.as_str())] {
            let cfg = Configured::from_secret(secret);
            for c in [call("gpt-6-luna", Effort::Low, 1), call("claude-haiku-5-5", Effort::Low, 1), direct_call("gpt-6-luna", Effort::Low, 1), direct_call("claude-haiku-5-5", Effort::Low, 1)] {
                let mut t = Mock::new(vec![Ok(Answer::ok(&sse::openai("gpt-6-luna", "x", "response.completed", None, "default"), 9))]);
                let g = run(drive(&mut t, &c, cfg.target().as_ref()));
                assert_eq!((g.outcome, g.sent), (OutcomeCode::KeyUnavailable, false), "{secret:?} {:?}", c.request.route);
                assert!(t.sent.is_empty() && t.admitted.is_empty() && t.deadlines.is_empty());
            }
        }
    }

    #[test]
    fn every_frontier_cell_sends_its_own_body_and_header_set() {
        let m = measured(configured(planned()));
        for x in m.lane_models(slop_ninja_gen_core::vendor::Lane::Frontier) {
            for e in &x.effort_levels {
                let c = call(&x.id, *e, 0);
                let mut t = Mock::new(vec![Ok(Answer::status(503, None, false, ""))]);
                go(&mut t, &c);
                assert_eq!(t.sent[0].body, c.request.body, "{} {e:?}", x.id);
                let names: Vec<&str> = t.sent[0].headers.iter().map(|(n, _)| n.as_str()).collect();
                match x.vendor {
                    Vendor::Openai => assert_eq!(names, ["content-type", "accept"]),
                    _ => assert_eq!(names, ["content-type", "accept", "anthropic-version"]),
                }
                // The direct shape of the same cell is never sent.
                let d = direct_call(&x.id, *e, 0);
                let mut t = Mock::new(vec![]);
                assert_eq!(go(&mut t, &d).outcome, OutcomeCode::InterfaceMismatch, "{} {e:?}", x.id);
                assert!(t.sent.is_empty());
            }
        }
    }

    #[test]
    fn a_configured_endpoint_gets_its_url_auth_and_headers_with_the_body_unchanged() {
        let ep = endpoint(ANTHROPIC_URL);
        let c = call("claude-sonnet-5-5", Effort::High, 3);
        let text = "Every spring the ferry company repaints its two boats over about three weeks, with volunteers sanding them.";
        let mut t = Mock::new(vec![Ok(Answer::ok(&sse::anthropic("claude-sonnet-5-5", text, "end_turn", None), 50))]);
        let g = run(drive(&mut t, &c, Some(&Target::of(&ep))));
        assert_eq!((g.outcome, g.tier), (OutcomeCode::Answer, Tier::Endpoint), "{g:?}");
        let s = &t.sent[0];
        assert_eq!(s.url, ANTHROPIC_URL);
        assert_eq!(s.auth, Some(("authorization".to_owned(), format!("Bearer {KEY}"))));
        let names: Vec<&str> = s.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["content-type", "accept", "anthropic-version", "x-endpoint-auth"]);
        assert_eq!(s.body, c.request.body);
        let b: serde_json::Value = serde_json::from_str(&s.body).unwrap();
        assert_eq!(b["system"][0]["text"], SYSTEM_PREFIX);
        assert!(!json(&g).contains(KEY));
        // OpenAI at a configured endpoint: no Flex, so a 429 is never retried on standard.
        let ep = endpoint(OPENAI_URL);
        let c = call("gpt-6.1-sol", Effort::Low, 0);
        assert!(!c.request.body.contains("service_tier"));
        let mut t = Mock::new(vec![Ok(Answer::status(429, None, false, "{}")), Ok(Answer::status(429, None, false, "{}"))]);
        let g = run(drive(&mut t, &c, Some(&Target::of(&ep))));
        assert_eq!((g.outcome, t.sent.len(), g.flex_unavailable), (OutcomeCode::SpendCap, 1, false));
        assert_eq!(t.admitted, vec![slop_ninja_gen_core::iface::call_heap_bytes(&c.request)], "no retry body admitted");
    }

    /// An endpoint's limits never end a call in a loop: a 503 with a hint is
    /// a throttle with its hint (the eval Worker's bounded throttle, then the
    /// outage path), an unserved cell is refused, and a stream-reported rate
    /// limit (a documented or a configured code) is a throttle.
    #[test]
    fn endpoint_limits_are_classified_once() {
        let ep = endpoint(ANTHROPIC_URL);
        let c = call("claude-opus-5-5", Effort::Max, 2);
        let busy = r#"{"type":"error","error":{"type":"unavailable","message":"Try again later.","detail":"someone"}}"#;
        let mut t = Mock::new(vec![Ok(Answer::status(503, Some("600"), false, busy))]);
        let g = run(drive(&mut t, &c, Some(&Target::of(&ep))));
        assert_eq!((g.outcome, g.retry_after_ms, t.sent.len()), (OutcomeCode::Throttle, Some(600_000), 1));
        assert!(!json(&g).contains("someone"));
        let mut t = Mock::new(vec![Ok(Answer::status(503, None, false, r#"{"type":"error","error":{"type":"service_unavailable_error"}}"#))]);
        assert_eq!(run(drive(&mut t, &c, Some(&Target::of(&ep)))).outcome, OutcomeCode::Outage);
        let ep = endpoint(OPENAI_URL);
        let c = call("gpt-6-luna", Effort::Low, 1);
        let failed = |code: &str| {
            format!(
                "event: response.created\ndata: {{\"type\":\"response.created\",\"response\":{{\"model\":null}}}}\n\nevent: response.failed\ndata: {{\"type\":\"response.failed\",\"response\":{{\"status\":\"failed\",\"error\":{{\"code\":\"{code}\",\"message\":\"m\",\"type\":\"server_error\"}}}}}}\n\n"
            )
        };
        for (code, want) in [
            ("test_not_offered", OutcomeCode::CellRefused),
            ("test_slow_down", OutcomeCode::Throttle),
            ("rate_limit_exceeded", OutcomeCode::Throttle),
            ("other", OutcomeCode::Outage),
        ] {
            let mut t = Mock::new(vec![Ok(Answer::ok(&failed(code), 40))]);
            let g = run(drive(&mut t, &c, Some(&Target::of(&ep))));
            assert_eq!((g.outcome, t.sent.len()), (want, 1), "{code}");
        }
        // The configured codes are the endpoint's own: an endpoint without
        // them keeps the documented mapping only.
        let mut t = Mock::new(vec![Ok(Answer::ok(&failed("test_slow_down"), 40))]);
        assert_eq!(go(&mut t, &c).outcome, OutcomeCode::Outage);
    }
}
