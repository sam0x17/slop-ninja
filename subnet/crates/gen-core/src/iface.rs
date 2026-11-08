//! The internal interface between the eval Worker and the generation Worker
//! (`slop-ninja-gen-testnet`), reached only through the eval Worker's
//! service binding: no route, no `workers.dev` name, no custom domain.
//!
//! A request must name the internal host, the exact path and method, and
//! carry the interface marker header. The generation Worker answers anything
//! else with a 404, so a misrouted request never reaches a vendor. The
//! service binding is the only way in: only a Worker whose configuration
//! binds this service can reach it at all.
//!
//! Bounds (no tuned value; each derives from the scorer's document limit or
//! from the request itself):
//! - a call body is at most [`CALL_BODY_BYTES`], refused before it is read;
//! - a reply is at most [`reply_bound`] of its request;
//! - a call's heap ([`call_heap_bytes`]) is admitted once, before the key is
//!   read or anything is sent, so a call the isolate cannot hold never
//!   reaches (or bills at) the vendor.
//!
//! Versioning: [`MARKER_VALUE`] is the interface version. Any change to the
//! wire types ([`crate::outcome::GenCall`], [`crate::outcome::GenReply`],
//! [`crate::request::VendorRequest`], [`crate::plan::Health`]) bumps it (the
//! test `the_wire_schema_is_pinned_to_the_marker` fails until it does). The
//! generation Worker answers a request carrying another version with 409
//! `interface_version` before reading its body, and `/health` names the
//! version it speaks, so a skew between the two separately deployed Workers
//! is a distinct, content-free outcome
//! ([`crate::outcome::OutcomeCode::InterfaceMismatch`]) that costs no vendor
//! call. Deploy order: the generation Worker first, then the eval Worker;
//! never roll the generation Worker back below the eval Worker's version.

use crate::request::VendorRequest;

/// The host the eval Worker addresses the generation Worker by (the service
/// binding ignores it; the generation Worker requires it).
pub const HOST: &str = "gen.internal";
/// The origin of every interface URL.
pub const ORIGIN: &str = "https://gen.internal";
/// `POST`: one vendor call ([`crate::outcome::GenCall`] in,
/// [`crate::outcome::GenReply`] out).
pub const GENERATE_PATH: &str = "/internal/gen/v1/generate";
/// `GET`: whether each key resolves ([`crate::plan::Health`]).
pub const HEALTH_PATH: &str = "/internal/gen/v1/health";
/// The interface marker header and its value.
pub const MARKER_HEADER: &str = "x-slop-ninja-gen";
pub const MARKER_VALUE: &str = "v2";

/// The scorer's document limit in tokens, `[CLS]` and `[SEP]` included
/// (`slop_ninja_artifact_check::text::MAX_TOKENS_INCLUDING_SPECIALS`; the
/// coordinator's tests check the two are equal).
pub const DOC_TOKENS: usize = 2048;
/// The tokenizer's longest word in characters (`max_input_chars_per_word`),
/// at four UTF-8 bytes each.
pub const WORD_BYTES: usize = 100 * 4;
/// A document's text at its byte bound: every token a whole word at the
/// word limit, with one separator. A rewrite carries its passage and, for
/// match-context, the rest of the same document: together at most one
/// document.
pub const DOC_TEXT_BYTES: usize = DOC_TOKENS * (WORD_BYTES + 1);
/// A call body: a request whose prompts hold one document at its bound,
/// doubled for JSON escapes of printable text (`"`, `\` and the newline
/// escape to two bytes), rounded up to the next power of two, which leaves
/// the templates, the voice and the call's envelope well inside it.
/// [`crate::request::build`] refuses a body past it, so the bound is
/// enforced where the request is made, not assumed.
pub const CALL_BODY_BYTES: usize = (2 * DOC_TEXT_BYTES).next_power_of_two();

/// A [`crate::outcome::GenCall`] message at most: the request body, which
/// is JSON, escaped once more as a string field (its quotes, backslashes
/// and escapes double at most), plus the call's other fields (one page).
pub const CALL_MESSAGE_BYTES: usize = 2 * CALL_BODY_BYTES + PAGE;

/// The visible text a call can have accepted at most (`stream_bound_v1` at
/// the interface's bounds): the widest validator window, a rewrite's
/// `1.8 a + 4` words with `a` at most one word per token of the document
/// limit, at the word limit plus two bytes of separator. A scratch window
/// (`1.8 x target`) is far below it.
pub const VISIBLE_BYTES_MAX: usize = (DOC_TOKENS * 9 / 5 + 4) * (WORD_BYTES + 2);
/// The longest SSE line a call may declare: its visible text plus an
/// envelope, which the manifest bounds by [`VISIBLE_BYTES_MAX`].
pub const LINE_BYTES_MAX: usize = 2 * VISIBLE_BYTES_MAX;

/// The interface's answer to a request with another marker value.
pub const VERSION_MISMATCH_STATUS: u16 = 409;

/// Whether `v` is an `anthropic-version` value of the documented form
/// (`YYYY-MM-DD`): the only caller-supplied header value besides the key.
pub fn anthropic_version_ok(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() == 10 && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
}

/// Whether `r`'s declared bounds are within the interface's and its header
/// values are well formed: the generation Worker refuses any other call
/// before it admits anything.
pub fn within(r: &VendorRequest) -> bool {
    let version_ok = match r.vendor {
        crate::vendor::Vendor::Anthropic => r.anthropic_version.as_deref().is_some_and(anthropic_version_ok),
        _ => r.anthropic_version.is_none(),
    };
    version_ok && r.body.len() <= CALL_BODY_BYTES && r.visible_bound as usize <= VISIBLE_BYTES_MAX && r.line_bound as usize <= LINE_BYTES_MAX && r.line_bound >= r.visible_bound
}

/// What the generation Worker leaves of a call's timeout
/// ([`crate::outcome::GenCall::timeout_ms`], also the eval Worker's own
/// bound on the whole service call) for everything around the vendor call:
/// the service binding's dispatch and return, the message read, the key's
/// resolution and the reply. The generation Worker's vendor deadline is the
/// call's timeout less this ([`vendor_deadline_ms`]), so a vendor call that
/// runs to the deadline ends there with the generation Worker's own outcome
/// and usage, and that reply reaches the eval Worker before it aborts the
/// service call. Part of the interface ([`MARKER_VALUE`]) and not yet
/// measured: the measurement run records each frontier call's service
/// overhead (`service_ms`: the eval Worker's wall time less the reply's
/// latency) to qualify it; a larger measured overhead needs a new interface
/// version, not a silent overrun.
pub const REPLY_ALLOWANCE_MS: u64 = 2_000;

/// The generation Worker's vendor deadline for a call of `timeout_ms`
/// ([`REPLY_ALLOWANCE_MS`]). A measured cell is usable only if its slowest
/// call fits it.
pub const fn vendor_deadline_ms(timeout_ms: u64) -> u64 {
    timeout_ms.saturating_sub(REPLY_ALLOWANCE_MS)
}

/// The read buffer of a body read: one WebAssembly page, the unit the arena
/// allocates in.
pub const READ_BUFFER_BYTES: usize = PAGE;

/// The heap one call holds at most beyond its message and parse, admitted
/// before anything is sent: the stream reader's two buffers (which also
/// bound an error body, at most the line bound) and the read buffer, the
/// reply at its bound, and for an OpenAI Flex request the body of its
/// standard retry ([`crate::request::standard_retry`]: one copy).
pub fn call_heap_bytes(r: &VendorRequest) -> usize {
    let retry = if r.vendor == crate::vendor::Vendor::Openai && r.tier == crate::vendor::Tier::Flex { r.body.len() } else { 0 };
    crate::sse::SseReader::bytes(r.visible_bound, r.line_bound).saturating_add(READ_BUFFER_BYTES).saturating_add(reply_bound(r)).saturating_add(retry)
}

/// The reply to `r` at most: its visible text doubled for JSON escapes, plus
/// the reply's fixed fields (one WebAssembly page).
pub fn reply_bound(r: &VendorRequest) -> usize {
    (r.visible_bound as usize).saturating_mul(2).saturating_add(PAGE)
}

/// One WebAssembly page.
pub const PAGE: usize = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bounds_derive_from_the_document_limit() {
        assert_eq!(DOC_TEXT_BYTES, 821_248);
        assert_eq!(CALL_BODY_BYTES, 2 * 1024 * 1024);
        const { assert!(CALL_BODY_BYTES >= 2 * DOC_TEXT_BYTES) };
        assert!(ORIGIN.ends_with(HOST));
        assert_eq!(VISIBLE_BYTES_MAX, 1_483_380);
        assert!(anthropic_version_ok("2026-10-21"));
        for bad in ["", "2026-10-2", "2026-10-21\r\nx: y", "2026/10/21", "2026-10-21 "] {
            assert!(!anthropic_version_ok(bad), "{bad:?}");
        }
    }

    /// The wire types' field sets, pinned to [`MARKER_VALUE`]: a field change
    /// fails here until the marker is bumped and this list updated with it.
    #[test]
    fn the_wire_schema_is_pinned_to_the_marker() {
        use crate::outcome::{GenCall, GenReply, OutcomeCode};
        use crate::vendor::{Effort, Tier, Vendor};
        let r = VendorRequest {
            vendor: Vendor::Anthropic,
            route: crate::vendor::Route::Direct,
            model: "m".into(),
            effort: Effort::Low,
            tier: Tier::Standard,
            body: "{}".into(),
            visible_bound: 1,
            line_bound: 1,
            word_window: (1, 1),
            max_tokens: 1,
            request_sha256: String::new(),
            anthropic_version: Some("2026-10-21".into()),
        };
        let mut g = GenReply::without_answer(OutcomeCode::Answer, &r, 0);
        g.text = Some(String::new());
        g.model_reported = Some(String::new());
        g.stop = Some(crate::sse::Stop::Completed);
        g.refusal = Some(crate::sse::RefusalCategory::Unstated);
        g.retry_after_ms = Some(0);
        g.first_token_ms = Some(0);
        // Each schema's member names, sorted: serde_json's map keeps insertion
        // (field) order when another crate in the build enables
        // `preserve_order`, and the pin is of the names, not their order.
        let keys = |v: serde_json::Value| {
            v.as_object()
                .map(|o| {
                    let mut k = o.keys().cloned().collect::<Vec<_>>();
                    k.sort_unstable();
                    k.join(",")
                })
                .unwrap_or_default()
        };
        let call = GenCall { request: r.clone(), accepted_models: vec![], timeout_ms: 0 };
        let health = crate::plan::Health::default();
        let schema = [keys(serde_json::to_value(&r).unwrap()), keys(serde_json::to_value(&call).unwrap()), keys(serde_json::to_value(&g).unwrap()), keys(serde_json::to_value(&health).unwrap())].join(";");
        let pinned = (
            "v2",
            "anthropic_version,body,effort,line_bound,max_tokens,model,request_sha256,route,tier,vendor,visible_bound,word_window;\
             accepted_models,request,timeout_ms;\
             envelope_bytes_max,first_token_ms,flex_unavailable,http_status,latency_ms,model_reported,outcome,refusal,request_sha256,retry_after_ms,sent,stop,text,thinking_events,tier,usage,usage_reported,visible_bytes;\
             anthropic,anthropic_route,interface,openai,openai_route",
        );
        assert_eq!((MARKER_VALUE, schema.as_str()), (pinned.0, pinned.1.replace(' ', "").as_str()), "a wire change bumps MARKER_VALUE");
    }
}
