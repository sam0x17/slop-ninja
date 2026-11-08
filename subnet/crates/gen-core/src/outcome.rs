//! Outcome classes of a vendor call, and the internal interface between the
//! eval Worker and the generation Worker.
//!
//! The classifier reads only structured fields (the HTTP status,
//! `retry-after`, the rate-limit headers' presence, `error.type`,
//! `error.code`, `error.details.error_code`, and one documented message
//! prefix) through a borrowed view of a bounded body, and returns a fixed
//! enum: no vendor message text is stored or returned. Vendor error bodies
//! can echo key fragments; none of their text leaves this module.

use crate::request::VendorRequest;
use crate::spend::Usage;
use crate::sse::{RefusalCategory, Stop, StreamEnd, StreamErrorKind, StreamFault};
use crate::vendor::{Tier, Vendor};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// The fixed outcome of one call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeCode {
    /// Visible text to validate (accept or content-reject).
    Answer,
    /// A safety refusal (Anthropic `refusal`, OpenAI refusal or `content_filter`).
    Refusal,
    /// The output token limit cut the answer.
    LengthTokensOut,
    /// Visible text past the stream bound (over the word window).
    LengthBytes,
    /// The vendor reported another model than the one requested.
    ModelMismatch,
    /// A rate limit with (or without) a retry hint: no attempt used.
    Throttle,
    /// OpenAI Flex's "429 Resource Unavailable" (not charged): retried at
    /// once on the standard tier, no attempt used.
    FlexUnavailable,
    /// The vendor's spend-cap or quota answer: the vendor pauses.
    SpendCap,
    /// The key did not resolve, or the vendor refused it (401/403).
    KeyUnavailable,
    /// A 400 for this (model, effort) cell: disabled for the cohort.
    CellRefused,
    /// 5xx, 529, transport, timeout, a broken or truncated stream.
    Outage,
    /// The two Workers speak different interface versions
    /// ([`crate::iface::MARKER_VALUE`]): refused before any vendor call, no
    /// attempt used, and alerted (a deploy-order fault, never a vendor's).
    InterfaceMismatch,
}

impl OutcomeCode {
    /// Whether the attempt counts (a content outcome): the item retries per
    /// the rotation. Every other class uses no attempt.
    pub fn uses_attempt(self) -> bool {
        matches!(self, Self::Answer | Self::Refusal | Self::LengthTokensOut | Self::LengthBytes | Self::ModelMismatch)
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::Refusal => "refusal",
            Self::LengthTokensOut => "length_tokens_out",
            Self::LengthBytes => "length_bytes",
            Self::ModelMismatch => "model_mismatch",
            Self::Throttle => "throttle",
            Self::FlexUnavailable => "flex_unavailable",
            Self::SpendCap => "spend_cap",
            Self::KeyUnavailable => "key_unavailable",
            Self::CellRefused => "cell_refused",
            Self::Outage => "outage",
            Self::InterfaceMismatch => "interface_mismatch",
        }
    }
}

#[derive(Deserialize, Default)]
struct Details<'a> {
    #[serde(borrow, default)]
    error_code: Option<Cow<'a, str>>,
}

#[derive(Deserialize, Default)]
struct ErrorObj<'a> {
    #[serde(rename = "type", borrow, default)]
    ty: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    code: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    details: Option<Details<'a>>,
    #[serde(borrow, default)]
    message: Option<Cow<'a, str>>,
}

#[derive(Deserialize, Default)]
struct ErrorBody<'a> {
    #[serde(borrow, default)]
    error: Option<ErrorObj<'a>>,
}

/// The documented prefix of Anthropic's answer when a spend limit the
/// organization set itself is reached (HTTP 400 `invalid_request_error`).
const ANTHROPIC_OWN_LIMIT: &str = "You have reached your specified";

/// `retry-after` in milliseconds (seconds, integer or decimal); an HTTP date
/// is not converted (the throttle backoff applies instead), but its presence
/// still marks a throttle in [`classify_http`].
pub fn retry_after_ms(v: Option<&str>) -> Option<u64> {
    let s = v?.trim();
    if let Ok(n) = s.parse::<u64>() {
        return Some(n.saturating_mul(1000));
    }
    let f: f64 = s.parse().ok()?;
    (f.is_finite() && f >= 0.0).then(|| (f * 1000.0).ceil() as u64)
}

/// Whether a 403 names one model rather than the key: Anthropic's
/// `permission_error` (the organization or workspace may not use the
/// requested model) and OpenAI's `model_not_found`. Any other 403 is the key's.
fn model_scoped_403(vendor: Vendor, ty: Option<&str>, code: Option<&str>) -> bool {
    match vendor {
        Vendor::Anthropic => ty == Some("permission_error"),
        Vendor::Openai => code == Some("model_not_found"),
        Vendor::WorkersAi => false,
    }
}

/// What a non-200 answer means. `rate_headers` is whether the response
/// carries the vendor's rate-limit headers (`x-ratelimit-*`,
/// `anthropic-ratelimit-*`); `body` is the error body, read within the
/// request's line bound (`None` when it was longer or unreadable).
///
/// Rules (`classify_http_v2`):
/// - 401, and a 403 that is not model-scoped: the key ([`OutcomeCode::KeyUnavailable`]);
///   a model-scoped 403 disables that cell only ([`OutcomeCode::CellRefused`]).
/// - Anthropic 429: `enforced_spend_limit_reached` is the spend cap; any
///   `retry-after` (seconds or an HTTP date) is a throttle; a 429 with
///   neither stops the vendor rather than retrying in a loop.
/// - OpenAI 429: `insufficient_quota` is the spend cap. On Flex only the
///   documented `rate_limit_exceeded` code is a throttle: every other Flex
///   429 is Flex's "429 Resource Unavailable" (documented without a code or
///   type, and not charged, whatever rate-limit headers ride on it) and runs
///   once on standard. On standard, `rate_limit_exceeded`, `retry-after` or
///   rate-limit headers mark a throttle; an undocumented 429 stops the vendor.
/// - A configured endpoint ([`Tier::Endpoint`], `classify_http_v3`): the
///   rules above, and a 503 with `retry-after` is a throttle until the hint,
///   so the item waits within the plan's throttle bound and then follows the
///   outage path (its frontier call moves to Workers AI at the outage
///   bound); the cohort is never skipped and never waits on the endpoint. A
///   503 without `retry-after` stays an outage.
pub fn classify_http(vendor: Vendor, tier: Tier, status: u16, retry_after: Option<&str>, rate_headers: bool, body: Option<&[u8]>) -> (OutcomeCode, Option<u64>) {
    let parsed: ErrorBody<'_> = body.and_then(|b| serde_json::from_slice(b).ok()).unwrap_or_default();
    let err = parsed.error.unwrap_or_default();
    let ty = err.ty.as_deref();
    let code = err.code.as_deref();
    let detail = err.details.as_ref().and_then(|d| d.error_code.as_deref());
    let after = retry_after_ms(retry_after);
    let after_present = retry_after.is_some_and(|v| !v.trim().is_empty());
    let rate_code = code == Some("rate_limit_exceeded");
    let class = match status {
        403 if model_scoped_403(vendor, ty, code) => OutcomeCode::CellRefused,
        401 | 403 => OutcomeCode::KeyUnavailable,
        402 => OutcomeCode::SpendCap,
        429 => match vendor {
            Vendor::Anthropic if detail == Some("enforced_spend_limit_reached") => OutcomeCode::SpendCap,
            // A rate limit carries retry-after; the spend cap never does.
            Vendor::Anthropic if after_present => OutcomeCode::Throttle,
            Vendor::Anthropic => OutcomeCode::SpendCap,
            Vendor::Openai if code == Some("insufficient_quota") || ty == Some("insufficient_quota") => OutcomeCode::SpendCap,
            Vendor::Openai if tier == Tier::Flex && rate_code => OutcomeCode::Throttle,
            Vendor::Openai if tier == Tier::Flex => OutcomeCode::FlexUnavailable,
            Vendor::Openai if rate_code || after_present || rate_headers => OutcomeCode::Throttle,
            // The hard spend limit's 429 names no code: every standard-tier
            // 429 that is not a documented throttle stops the vendor rather
            // than retrying in a loop.
            Vendor::Openai => OutcomeCode::SpendCap,
            Vendor::WorkersAi => OutcomeCode::Throttle,
        },
        400 if vendor == Vendor::Anthropic && err.message.as_deref().is_some_and(|m| m.starts_with(ANTHROPIC_OWN_LIMIT)) => OutcomeCode::SpendCap,
        400 | 404 | 413 | 422 => OutcomeCode::CellRefused,
        503 if tier == Tier::Endpoint && after_present => OutcomeCode::Throttle,
        _ => OutcomeCode::Outage,
    };
    (class, if class == OutcomeCode::Throttle { after } else { None })
}

/// What a 200 stream means. `accepted` is the requested id and its aliases.
pub fn classify_stream(end: &StreamEnd, accepted: &[String]) -> OutcomeCode {
    match end.fault {
        Some(StreamFault::LengthBytes) => return OutcomeCode::LengthBytes,
        Some(StreamFault::LineBytes | StreamFault::Malformed) => return OutcomeCode::Outage,
        None => {}
    }
    match end.stop {
        Stop::Refusal | Stop::ContentFilter => OutcomeCode::Refusal,
        Stop::MaxTokens => OutcomeCode::LengthTokensOut,
        Stop::StreamError(StreamErrorKind::RateLimit) => OutcomeCode::Throttle,
        Stop::StreamError(StreamErrorKind::Unsupported) => OutcomeCode::CellRefused,
        Stop::StreamError(_) | Stop::Failed | Stop::Truncated | Stop::Incomplete => OutcomeCode::Outage,
        Stop::Completed => match &end.model_reported {
            Some(m) if accepted.iter().any(|a| a == m) => OutcomeCode::Answer,
            _ => OutcomeCode::ModelMismatch,
        },
    }
}

/// One call, from the eval Worker to the generation Worker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenCall {
    pub request: VendorRequest,
    /// The requested id and the manifest's accepted aliases.
    pub accepted_models: Vec<String>,
    /// The call's wall budget (the lane's call timeout).
    pub timeout_ms: u64,
}

/// The generation Worker's answer: content-free except the visible text of
/// an `answer` (the generated text the eval Worker validates).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenReply {
    pub outcome: OutcomeCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// HTTP status of the last vendor answer (0: no answer).
    pub http_status: u16,
    /// The tier the vendor reported (else the one requested).
    pub tier: Tier,
    /// Flex was unavailable and the call ran again on standard.
    pub flex_unavailable: bool,
    /// The reported model, only when it is an accepted id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_reported: Option<String>,
    pub usage: Usage,
    /// Whether the vendor reported the call's final usage (OpenAI's terminal
    /// response, Anthropic's `message_delta` with its stop reason, a Workers
    /// AI reply's `usage`). A call that was sent and ends without it is
    /// charged its reservation bound, marked unmeasured
    /// ([`crate::spend::charge`]).
    pub usage_reported: bool,
    /// Whether the request reached the vendor's endpoint (a POST was issued;
    /// `false` for a refusal before sending: no key, a refused admission, an
    /// interface mismatch).
    pub sent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Stop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<RefusalCategory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    /// The request digest actually sent (the standard retry's when it ran).
    pub request_sha256: String,
    pub latency_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    pub visible_bytes: u32,
    pub thinking_events: u32,
    pub envelope_bytes_max: u32,
}

impl GenReply {
    /// A reply with no vendor answer (a missing key, a transport failure,
    /// a refused admission).
    pub fn without_answer(outcome: OutcomeCode, r: &VendorRequest, latency_ms: u64) -> Self {
        GenReply {
            outcome,
            text: None,
            http_status: 0,
            tier: r.tier,
            flex_unavailable: false,
            model_reported: None,
            usage: Usage::default(),
            usage_reported: false,
            sent: false,
            stop: None,
            refusal: None,
            retry_after_ms: None,
            request_sha256: r.request_sha256.clone(),
            latency_ms,
            first_token_ms: None,
            visible_bytes: 0,
            thinking_events: 0,
            envelope_bytes_max: 0,
        }
    }

    /// The reply of a stream that ended.
    pub fn from_stream(end: StreamEnd, call: &GenCall, sent: &VendorRequest, status: u16, flex_unavailable: bool, latency_ms: u64, first_token_ms: Option<u64>) -> Self {
        let outcome = classify_stream(&end, &call.accepted_models);
        let model_reported = end.model_reported.filter(|m| call.accepted_models.contains(m));
        GenReply {
            outcome,
            visible_bytes: end.visible.len() as u32,
            text: (outcome == OutcomeCode::Answer).then_some(end.visible),
            http_status: status,
            // An endpoint call is priced at the endpoint tier, whatever tier
            // its stream reports.
            tier: if sent.tier == Tier::Endpoint { Tier::Endpoint } else { end.tier_reported.unwrap_or(sent.tier) },
            flex_unavailable,
            model_reported,
            usage: end.usage,
            usage_reported: end.usage_reported,
            sent: true,
            stop: Some(end.stop),
            refusal: end.refusal,
            retry_after_ms: None,
            request_sha256: sent.request_sha256.clone(),
            latency_ms,
            first_token_ms,
            thinking_events: end.thinking_events,
            envelope_bytes_max: end.envelope_bytes_max,
        }
    }
}

/// The text of a Workers AI reply (its two documented shapes: `response`,
/// or OpenAI-compatible `choices[0].message.content`).
pub fn workers_ai_text(v: &serde_json::Value) -> Option<&str> {
    v.get("response").and_then(serde_json::Value::as_str).or_else(|| v.get("choices")?.get(0)?.get("message")?.get("content")?.as_str())
}

/// The reply of one Workers AI run of `r` that answered `v` (pure: the eval
/// Worker's binding call hands its result here, and tests run it natively).
///
/// - Usage: `prompt_tokens`, `completion_tokens` and, where the model
///   reports it, `completion_tokens_details.reasoning_tokens`.
/// - `finish_reason` `length` (top level or `choices[0]`), or a reply whose
///   completion tokens reached the request's `max_tokens`, is
///   [`OutcomeCode::LengthTokensOut`]: a cut answer never passes as text.
/// - Text past the visible bound is [`OutcomeCode::LengthBytes`].
pub fn workers_ai_reply(v: &serde_json::Value, r: &VendorRequest, latency_ms: u64) -> GenReply {
    use serde_json::Value;
    let u = v.get("usage").filter(|u| u.is_object());
    let count = |k: &str| u.and_then(|u| u.get(k)).and_then(Value::as_u64);
    let reasoning = u.and_then(|u| u.get("completion_tokens_details")?.get("reasoning_tokens")?.as_u64());
    let usage = Usage {
        input_tokens: count("prompt_tokens").unwrap_or(0),
        cached_input_tokens: 0,
        output_tokens: count("completion_tokens").unwrap_or(0),
        reasoning_tokens: reasoning,
    };
    let finish = v
        .get("finish_reason")
        .and_then(Value::as_str)
        .or_else(|| v.get("choices")?.get(0)?.get("finish_reason")?.as_str());
    let cut = finish == Some("length") || (r.max_tokens > 0 && usage.output_tokens >= u64::from(r.max_tokens));
    let mut g = GenReply::without_answer(OutcomeCode::Answer, r, latency_ms);
    g.http_status = 200;
    g.sent = true;
    g.usage_reported = u.is_some();
    g.usage = usage;
    g.model_reported = Some(r.model.clone());
    let text = workers_ai_text(v);
    g.visible_bytes = text.map_or(0, |t| t.len().min(u32::MAX as usize) as u32);
    if cut {
        g.outcome = OutcomeCode::LengthTokensOut;
        g.stop = Some(Stop::MaxTokens);
        return g;
    }
    match text {
        Some(t) if t.len() <= r.visible_bound as usize => {
            g.text = Some(t.to_owned());
            g.stop = Some(Stop::Completed);
        }
        Some(_) => g.outcome = OutcomeCode::LengthBytes,
        None => {
            g.text = Some(String::new());
            g.stop = Some(Stop::Completed);
        }
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::{SseReader, fixture};

    const KEYISH: &str = "sk-proj-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    #[test]
    fn http_answers_map_to_fixed_classes() {
        let spend = br#"{"type":"error","error":{"type":"rate_limit_error","message":"spend","details":{"error_code":"enforced_spend_limit_reached"}}}"#;
        assert_eq!(classify_http(Vendor::Anthropic, Tier::Standard, 429, None, true, Some(spend)).0, OutcomeCode::SpendCap);
        let rate = br#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;
        assert_eq!(classify_http(Vendor::Anthropic, Tier::Standard, 429, Some("7"), true, Some(rate)), (OutcomeCode::Throttle, Some(7000)));
        assert_eq!(classify_http(Vendor::Anthropic, Tier::Standard, 429, None, true, Some(rate)).0, OutcomeCode::SpendCap);
        let own = br#"{"type":"error","error":{"type":"invalid_request_error","message":"You have reached your specified API usage limits. You will regain access on ..."}}"#;
        assert_eq!(classify_http(Vendor::Anthropic, Tier::Standard, 400, None, false, Some(own)).0, OutcomeCode::SpendCap);
        let quota = br#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#;
        assert_eq!(classify_http(Vendor::Openai, Tier::Flex, 429, Some("1"), true, Some(quota)).0, OutcomeCode::SpendCap);
        assert_eq!(classify_http(Vendor::Openai, Tier::Flex, 429, None, false, None).0, OutcomeCode::FlexUnavailable);
        assert_eq!(classify_http(Vendor::Openai, Tier::Standard, 429, None, false, None).0, OutcomeCode::SpendCap);
        assert_eq!(classify_http(Vendor::Openai, Tier::Standard, 429, None, true, None).0, OutcomeCode::Throttle);
        let rate_code = br#"{"error":{"message":"Rate limit reached","type":"requests","code":"rate_limit_exceeded"}}"#;
        assert_eq!(classify_http(Vendor::Openai, Tier::Flex, 429, Some("0.5"), true, Some(rate_code)), (OutcomeCode::Throttle, Some(500)));
        // Flex's "429 Resource Unavailable" has no documented code: rate-limit
        // headers or a retry-after on it never turn it into a throttle.
        let unavailable = br#"{"error":{"message":"Resource Unavailable","type":"invalid_request_error"}}"#;
        assert_eq!(classify_http(Vendor::Openai, Tier::Flex, 429, None, true, Some(unavailable)).0, OutcomeCode::FlexUnavailable);
        assert_eq!(classify_http(Vendor::Openai, Tier::Flex, 429, Some("2"), true, None).0, OutcomeCode::FlexUnavailable);
        assert_eq!(classify_http(Vendor::Openai, Tier::Standard, 429, None, false, Some(rate_code)).0, OutcomeCode::Throttle);
        // Any retry-after marks an Anthropic throttle, an HTTP date included.
        assert_eq!(classify_http(Vendor::Anthropic, Tier::Standard, 429, Some("Wed, 21 Oct 2026 07:28:00 GMT"), true, Some(rate)), (OutcomeCode::Throttle, None));
        // A model-scoped 403 disables the cell; any other 403 and every 401 is the key's.
        let perm = br#"{"type":"error","error":{"type":"permission_error","message":"no access to this model"}}"#;
        assert_eq!(classify_http(Vendor::Anthropic, Tier::Standard, 403, None, false, Some(perm)).0, OutcomeCode::CellRefused);
        assert_eq!(classify_http(Vendor::Anthropic, Tier::Standard, 401, None, false, Some(perm)).0, OutcomeCode::KeyUnavailable);
        let missing = br#"{"error":{"message":"The model does not exist or you do not have access to it.","type":"invalid_request_error","code":"model_not_found"}}"#;
        assert_eq!(classify_http(Vendor::Openai, Tier::Flex, 403, None, false, Some(missing)).0, OutcomeCode::CellRefused);
        assert_eq!(classify_http(Vendor::Openai, Tier::Flex, 403, None, false, None).0, OutcomeCode::KeyUnavailable);
        // A redirect is never followed and is an outage.
        assert_eq!(classify_http(Vendor::Anthropic, Tier::Standard, 307, None, false, None).0, OutcomeCode::Outage);
        assert_eq!(classify_http(Vendor::Anthropic, Tier::Standard, 529, None, false, None).0, OutcomeCode::Outage);
        assert_eq!(classify_http(Vendor::Openai, Tier::Flex, 503, None, false, None).0, OutcomeCode::Outage);
        let unauth = format!(r#"{{"error":{{"message":"Incorrect API key provided: {KEYISH}","type":"invalid_request_error","code":"invalid_api_key"}}}}"#);
        assert_eq!(classify_http(Vendor::Openai, Tier::Flex, 401, None, false, Some(unauth.as_bytes())).0, OutcomeCode::KeyUnavailable);
        assert_eq!(classify_http(Vendor::Anthropic, Tier::Standard, 403, None, false, None).0, OutcomeCode::KeyUnavailable);
        let bad = br#"{"type":"error","error":{"type":"invalid_request_error","message":"thinking.type: between_tools is not supported at effort xhigh"}}"#;
        assert_eq!(classify_http(Vendor::Anthropic, Tier::Standard, 400, None, false, Some(bad)).0, OutcomeCode::CellRefused);
        assert_eq!(classify_http(Vendor::Anthropic, Tier::Standard, 400, None, false, Some(b"not json")).0, OutcomeCode::CellRefused);
        assert_eq!(retry_after_ms(Some("Wed, 21 Oct 2026 07:28:00 GMT")), None);
        assert!(!OutcomeCode::Throttle.uses_attempt() && OutcomeCode::Refusal.uses_attempt() && !OutcomeCode::SpendCap.uses_attempt());
    }

    fn end(body: &str, vendor: Vendor) -> StreamEnd {
        let mut r = SseReader::new(vendor, 4096, 8192);
        r.feed(body.as_bytes());
        r.finish()
    }

    /// A configured endpoint's answers (generic error bodies).
    #[test]
    fn configured_endpoint_answers_map_to_fixed_classes() {
        let ep = Tier::Endpoint;
        // A 503 with retry-after is a throttle until the hint, for either vendor.
        let busy = br#"{"type":"error","error":{"type":"unavailable","message":"Try again later."}}"#;
        assert_eq!(classify_http(Vendor::Anthropic, ep, 503, Some("600"), false, Some(busy)), (OutcomeCode::Throttle, Some(600_000)));
        assert_eq!(classify_http(Vendor::Openai, ep, 503, Some("30"), false, None), (OutcomeCode::Throttle, Some(30_000)));
        // No hint: an outage (the outage path, then Workers AI).
        let down = br#"{"type":"error","error":{"type":"service_unavailable_error","message":"Service temporarily unavailable. Please try again later."}}"#;
        assert_eq!(classify_http(Vendor::Anthropic, ep, 503, None, true, Some(down)).0, OutcomeCode::Outage);
        // Direct vendors keep v2's rule: a 503 is an outage whatever its headers.
        assert_eq!(classify_http(Vendor::Anthropic, Tier::Standard, 503, Some("5"), false, None).0, OutcomeCode::Outage);
        // An endpoint 429: with a hint a throttle, without one the vendor pauses
        // until the operator's vendor clear (no probe auto-resume yet), never a loop.
        let rate = br#"{"type":"error","error":{"type":"rate_limit_error","message":"x"}}"#;
        assert_eq!(classify_http(Vendor::Anthropic, ep, 429, Some("12"), true, Some(rate)), (OutcomeCode::Throttle, Some(12_000)));
        assert_eq!(classify_http(Vendor::Anthropic, ep, 429, None, true, Some(rate)).0, OutcomeCode::SpendCap);
        assert_eq!(classify_http(Vendor::Openai, ep, 429, None, false, None).0, OutcomeCode::SpendCap);
        assert_eq!(classify_http(Vendor::Openai, ep, 429, Some("4"), false, None), (OutcomeCode::Throttle, Some(4_000)));
        // The endpoint refusing its key is the key's; an unserved model is the cell's.
        assert_eq!(classify_http(Vendor::Anthropic, ep, 401, None, false, Some(b"unauthorized")).0, OutcomeCode::KeyUnavailable);
        let unsupported = br#"{"error":{"message":"The requested model is not available.","type":"invalid_request_error","code":"model_not_found"}}"#;
        assert_eq!(classify_http(Vendor::Openai, ep, 400, None, false, Some(unsupported)).0, OutcomeCode::CellRefused);
        assert_eq!(classify_http(Vendor::Anthropic, ep, 404, None, false, Some(br#"{"type":"error","error":{"type":"not_found_error"}}"#)).0, OutcomeCode::CellRefused);
        // An OpenAI-format endpoint may report errors inside a 200 stream as
        // response.failed: documented codes, then the endpoint's configured ones.
        let codes = crate::sse::ErrorCodes { rate_limit: "test_slow_down", unsupported: "test_not_offered" };
        let failed = |code: &str| {
            let mut r = SseReader::new(Vendor::Openai, 4096, 8192).with_codes(codes);
            r.feed(b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"model\":null}}\n\n");
            r.feed(format!("event: response.failed\ndata: {{\"type\":\"response.failed\",\"response\":{{\"status\":\"failed\",\"error\":{{\"code\":\"{code}\",\"message\":\"m\",\"type\":\"server_error\"}}}}}}\n\n").as_bytes());
            classify_stream(&r.finish(), &["gpt-6-luna".to_owned()])
        };
        assert_eq!(failed("test_not_offered"), OutcomeCode::CellRefused);
        assert_eq!(failed("model_not_found"), OutcomeCode::CellRefused);
        assert_eq!(failed("test_slow_down"), OutcomeCode::Throttle);
        assert_eq!(failed("rate_limit_exceeded"), OutcomeCode::Throttle);
        assert_eq!(failed("server_is_overloaded"), OutcomeCode::Outage);
        assert_eq!(failed("something_new"), OutcomeCode::Outage);
        // An endpoint reply is priced at the endpoint tier whatever tier its stream reports.
        let m = crate::manifest::fixture::configured(crate::manifest::fixture::planned());
        let (req, _) = crate::measure::request(&crate::manifest::fixture::measured(m), &crate::voice::fixture::library(), "gpt-6-luna", crate::vendor::Effort::Low, 0).unwrap();
        assert_eq!(req.tier, Tier::Endpoint);
        let call = GenCall { accepted_models: vec!["gpt-6-luna".into()], request: req.clone(), timeout_ms: 1 };
        let mut r = SseReader::new(Vendor::Openai, req.visible_bound, req.line_bound);
        r.feed(fixture::openai("gpt-6-luna", "Some text here.", "response.completed", None, "default").as_bytes());
        let g = GenReply::from_stream(r.finish(), &call, &req, 200, false, 5, None);
        assert_eq!((g.outcome, g.tier), (OutcomeCode::Answer, Tier::Endpoint));
    }

    #[test]
    fn streams_map_to_fixed_classes_and_replies_carry_no_vendor_text() {
        let call = |id: &str| GenCall {
            request: VendorRequest {
                vendor: Vendor::Anthropic,
                route: crate::vendor::Route::Direct,
                model: id.into(),
                effort: crate::vendor::Effort::Low,
                tier: Tier::Standard,
                body: "{}".into(),
                visible_bound: 4096,
                line_bound: 8192,
                word_window: (1, 100),
                max_tokens: 0,
                request_sha256: "00".repeat(32),
                anthropic_version: Some("2026-10-21".into()),
            },
            accepted_models: vec![id.into()],
            timeout_ms: 1,
        };
        let c = call("claude-opus-5-5");
        let ok = end(&fixture::anthropic("claude-opus-5-5", "Fine text.", "end_turn", None), Vendor::Anthropic);
        let r = GenReply::from_stream(ok, &c, &c.request, 200, false, 10, Some(3));
        assert_eq!((r.outcome, r.text.as_deref()), (OutcomeCode::Answer, Some("Fine text.")));
        // A silent substitution never becomes an answer, and its id is not kept.
        let sub = end(&fixture::anthropic(KEYISH, "Fine text.", "end_turn", None), Vendor::Anthropic);
        let r = GenReply::from_stream(sub, &c, &c.request, 200, false, 10, None);
        assert_eq!((r.outcome, r.text.as_deref(), r.model_reported.as_deref()), (OutcomeCode::ModelMismatch, None, None));
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("sk-proj") && !s.contains("Fine text"));
        let refused = end(&fixture::anthropic("claude-opus-5-5", "", "refusal", Some("bio")), Vendor::Anthropic);
        let r = GenReply::from_stream(refused, &c, &c.request, 200, false, 10, None);
        assert_eq!((r.outcome, r.refusal), (OutcomeCode::Refusal, Some(RefusalCategory::Bio)));
        assert!(!serde_json::to_string(&r).unwrap().contains("vendor text"));
        let cut = end(&fixture::openai("gpt-6-luna", "abc", "response.incomplete", Some("max_output_tokens"), "flex"), Vendor::Openai);
        assert_eq!(classify_stream(&cut, &["gpt-6-luna".into()]), OutcomeCode::LengthTokensOut);
        let filt = end(&fixture::openai("gpt-6-luna", "abc", "response.incomplete", Some("content_filter"), "flex"), Vendor::Openai);
        assert_eq!(classify_stream(&filt, &["gpt-6-luna".into()]), OutcomeCode::Refusal);
        let broken = end("data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}\n", Vendor::Anthropic);
        assert_eq!(classify_stream(&broken, &[]), OutcomeCode::Outage);
    }
}
