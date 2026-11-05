//! Native witness tests for the generation route (`--features
//! arena-witness`, one test thread: `cargo test --features arena-witness
//! arena_witness -- --test-threads=1`).
//!
//! The arena is this test binary's global allocator (lib.rs). Each witness
//! runs the generate route's native path (the message block and its parse,
//! the call over a replaying transport, the reply's encoding) under
//! `Mode::Strict` inside a scope of exactly the route's control footprint,
//! with the call's own admissions made as the Worker makes them (the
//! message block, its parse, then the call heap inside `drive`), and fails
//! on any allocation outside the scope, any overdraft past what was
//! admitted, and anything left live at the end. The replayed vendor bytes
//! are borrowed from outside the scope, as the Worker's come from JS.

use crate::drive::{self, Body, Fault, Head, Transport};
use crate::routes;
use slop_ninja_arena::{Closed, Mode, Scope, admit_current_block, admit_current_heap, set_mode, strict};
use slop_ninja_gen_core::iface;
use slop_ninja_gen_core::manifest::fixture::{measured, planned};
use slop_ninja_gen_core::outcome::{GenCall, OutcomeCode};
use slop_ninja_gen_core::sse::fixture as sse;
use slop_ninja_gen_core::vendor::Effort;
use slop_ninja_gen_core::voice::fixture::library;

/// A vendor that answers each post with the next borrowed answer.
struct Replay<'a> {
    answers: &'a [(u16, &'a [u8])],
    next: usize,
}

struct ReplayBody<'a> {
    rest: &'a [u8],
}

/// The chunk length the replay hands out: odd, so lines split anywhere.
const REPLAY_CHUNK: usize = 997;

impl Body for ReplayBody<'_> {
    async fn read(&mut self, out: &mut [u8]) -> Result<usize, Fault> {
        let n = self.rest.len().min(out.len()).min(REPLAY_CHUNK);
        let (head, tail) = self.rest.split_at(n);
        out[..n].copy_from_slice(head);
        self.rest = tail;
        Ok(n)
    }
    fn cancel(&mut self) {
        self.rest = &[];
    }
}

impl<'a> Transport for Replay<'a> {
    type Body = ReplayBody<'a>;
    async fn post(&mut self, _url: &str, _headers: &[(&str, &str)], _body: &str, _deadline_ms: u64) -> Result<(Head, ReplayBody<'a>), Fault> {
        let (status, body) = *self.answers.get(self.next).ok_or(Fault::Transport)?;
        self.next += 1;
        Ok((Head { status, retry_after: None, rate_headers: false }, ReplayBody { rest: body }))
    }
    fn now_ms(&self) -> u64 {
        0
    }
    fn admit(&mut self, bytes: usize) -> bool {
        admit_current_heap(bytes).is_ok()
    }
}

/// The generate route on `message` (a call's JSON) against `answers`, in a
/// witness scope: the Worker's sequence of admissions and allocations.
fn witness(message: &[u8], answers: &[(u16, &[u8])]) -> OutcomeCode {
    let prev = set_mode(Mode::Strict);
    let _ = strict::take();
    let armed = strict::arm();
    let route = routes::R_GENERATE.route();
    let scope = Scope::open(route, routes::CONTROL_BYTES).unwrap_or_else(|r| panic!("route admitted: {r:?}"));
    let out = scope.enter(|| {
        // cf_io's read: one block of the declared length.
        assert_eq!(admit_current_block(message.len(), 1), Ok(true));
        let mut raw = Vec::with_capacity(message.len());
        raw.extend_from_slice(message);
        assert_eq!(admit_current_heap(routes::parse_bytes(raw.len())), Ok(true));
        let call: GenCall = serde_json::from_slice(&raw).expect("call");
        drop(raw);
        assert!(iface::within(&call.request));
        let mut t = Replay { answers, next: 0 };
        // drive() admits the call's heap (reader, reply, a Flex retry's
        // body) before it sends anything.
        let reply = drive::mock::run(drive::drive(&mut t, &call, "sk-WITNESS-KEY"));
        let json = serde_json::to_string(&reply).expect("reply");
        assert!(json.len() <= iface::reply_bound(&call.request));
        reply.outcome
    });
    let st = scope.stats();
    let closed = scope.close();
    drop(armed);
    set_mode(prev);
    let violations = strict::take();
    eprintln!(
        "arena witness: route {} peak {} pages of {} reserved, peak live {} B",
        route.name, st.peak_pages, st.reserved_pages, st.peak_live
    );
    assert!(violations.is_clean(), "the generate route broke its admissions: {violations:?}");
    assert!(matches!(closed, Closed::Released { .. }), "the generate route left values live: {closed:?}");
    out
}

fn message(model: &str, effort: Effort, call: u32) -> (Vec<u8>, u32) {
    let m = measured(planned());
    let (request, _) = slop_ninja_gen_core::measure::request(&m, &library(), model, effort, call).expect("request");
    let visible = request.visible_bound;
    let c = GenCall { accepted_models: vec![model.to_owned()], request, timeout_ms: 285_000 };
    (serde_json::to_vec(&c).expect("json"), visible)
}

#[test]
fn arena_witness_generate_answer() {
    set_mode(Mode::Enforce);
    let text = "Each spring the ferry company repaints both boats over three weeks, helped by volunteers from the rowing club who sand them down. ".repeat(4);
    let (msg, _) = message("claude-sonnet-5-5", Effort::High, 3);
    let body = sse::anthropic("claude-sonnet-5-5", text.trim_end(), "end_turn", None);
    assert_eq!(witness(&msg, &[(200, body.as_bytes())]), OutcomeCode::Answer);
}

#[test]
fn arena_witness_generate_flex_retry_and_errors() {
    set_mode(Mode::Enforce);
    let (msg, _) = message("gpt-6.1-terra", Effort::None, 14);
    let text = "A short scratch text about counting moths in a park on warm cloudy nights. ".repeat(3);
    let body = sse::openai("gpt-6.1-terra", text.trim_end(), "response.completed", None, "default");
    let flex = br#"{"error":{"type":"rate_limit_error","message":"Resource Unavailable"}}"#;
    assert_eq!(witness(&msg, &[(429, flex), (200, body.as_bytes())]), OutcomeCode::Answer);
    let cap = br#"{"type":"error","error":{"type":"rate_limit_error","details":{"error_code":"enforced_spend_limit_reached"}}}"#;
    let (msg, _) = message("claude-haiku-5-5", Effort::None, 2);
    assert_eq!(witness(&msg, &[(429, cap)]), OutcomeCode::SpendCap);
}

#[test]
fn arena_witness_generate_over_the_visible_bound() {
    set_mode(Mode::Enforce);
    let (msg, visible) = message("claude-fable-5-1", Effort::Max, 6);
    let long = "word ".repeat(visible as usize);
    let body = sse::anthropic("claude-fable-5-1", &long, "end_turn", None);
    assert_eq!(witness(&msg, &[(200, body.as_bytes())]), OutcomeCode::LengthBytes);
}
