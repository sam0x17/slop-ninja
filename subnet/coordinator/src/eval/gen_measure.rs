//! Generation v2's eval-side logic that needs no platform: the measurement
//! route's request parsing, verdict, charge, observation and ledger key, and
//! the classification of a generation-Worker interface failure. The wasm32
//! client (`cf_gen.rs`) does only the I/O around these, so everything it
//! decides is tested natively here.
//!
//! - [`MeasureCall::parse`]: the measurement body is exactly `{manifest,
//!   model, effort, call, attempt}`; any other field (text among them) is
//!   refused, so no pool, held-out or customer text can reach a vendor
//!   through the route.
//! - [`assemble`]: the operator's answer for one measured call. Every call
//!   the vendor may have billed is charged (`spend::charge`), the reservation
//!   bound when the vendor reported no final usage; the answer carries the
//!   outcome classes, the charge and the synthetic output, never a key or a
//!   vendor error text (a [`GenReply`] holds neither).
//! - [`ledger_key`]: one key per vendor call, unique and idempotent: the
//!   cell, the call index and the operator's attempt index.
//! - [`interface_failure`]: what the eval Worker records when the generation
//!   Worker's answer is not a reply.

use crate::eval::textgen;
use serde::Deserialize;
use serde_json::{Value, json};
use slop_ninja_gen_core::manifest::{Manifest, ModelEntry, ObjectRef};
use slop_ninja_gen_core::measure::{self, Observation};
use slop_ninja_gen_core::outcome::{GenReply, OutcomeCode};
use slop_ninja_gen_core::request::VendorRequest;
use slop_ninja_gen_core::spend;
use slop_ninja_gen_core::vendor::Effort;

/// One measurement call as the operator's script sends it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasureCall {
    pub manifest: ObjectRef,
    pub model: String,
    pub effort: String,
    pub call: u32,
    /// The operator's attempt index of this call (0 first; a retry after a
    /// non-content answer is the next): part of the ledger key.
    pub attempt: u32,
}

/// The longest model id the route accepts (the manifest's bound).
const MODEL_ID_BYTES: usize = 96;

impl MeasureCall {
    /// The body's call, or the refusal's code.
    pub fn parse(body: &Value) -> Result<(Self, Effort), &'static str> {
        let c: MeasureCall = serde_json::from_value(body.clone()).map_err(|_| "body (exactly manifest, model, effort, call and attempt)")?;
        c.manifest.validate("manifest").map_err(|_| "manifest")?;
        let effort = Effort::parse(&c.effort).ok_or("effort")?;
        if c.model.is_empty() || c.model.len() > MODEL_ID_BYTES {
            return Err("model");
        }
        if c.call >= measure::CALLS_PER_CELL {
            return Err("call");
        }
        Ok((c, effort))
    }
}

/// The SpendLedger key of one measured vendor call.
pub fn ledger_key(entry: &ModelEntry, effort: Effort, call: u32, attempt: u32) -> String {
    format!("probe:gen-measure-{}:{}:{}:{call}:{attempt}", entry.vendor.code(), entry.id, effort.code())
}

/// What one measured call produced.
#[derive(Clone, Debug, PartialEq)]
pub struct Measured {
    /// The operator's answer.
    pub answer: Value,
    /// The charge, micro-USD (rounded up).
    pub microusd: u64,
    pub ledger_key: String,
}

/// The operator's answer to measured call `c` of `entry` at `effort`: its
/// request `r`, the synthetic source passage (`source`, empty for a scratch
/// call) and the generation reply `g`.
#[allow(clippy::too_many_arguments)]
pub fn assemble(m: &Manifest, entry: &ModelEntry, effort: Effort, c: &MeasureCall, r: &VendorRequest, source: &str, g: &GenReply, call_timeout_ms: u64) -> Measured {
    let verdict = match (&g.text, g.outcome) {
        (Some(t), OutcomeCode::Answer) => match measure::target_words(m, c.call) {
            None => textgen::validate_rewrite(source, t).map(|_| ()),
            Some(n) => textgen::validate_scratch(t, n as usize).map(|_| ()),
        }
        .map_err(textgen::Reject::code),
        _ => Err(g.outcome.code()),
    };
    let charge = spend::charge(entry, r, g);
    let obs = Observation {
        model: entry.id.clone(),
        effort,
        call: c.call,
        outcome: g.outcome,
        accepted: verdict.is_ok(),
        usage: g.usage,
        latency_ms: g.latency_ms,
        first_token_ms: g.first_token_ms,
        envelope_bytes_max: g.envelope_bytes_max,
        window_words: r.word_window.1,
        unmeasured: charge.unmeasured,
    };
    let answer = json!({
        "observation": obs,
        // Only content outcomes are measurements; the script retries the rest.
        "content": g.outcome.uses_attempt(),
        "verdict": verdict.err(),
        "http_status": g.http_status,
        "tier": g.tier,
        "flex_unavailable": g.flex_unavailable,
        "retry_after_ms": g.retry_after_ms,
        "stop": g.stop,
        "refusal": g.refusal,
        "request_sha256": g.request_sha256,
        "max_tokens": r.max_tokens,
        "cost_nanousd": charge.cost_nanousd,
        "unmeasured": charge.unmeasured,
        "call_timeout_ms": call_timeout_ms,
        "attempt": c.attempt,
        "synthetic_output": g.text,
    });
    Measured { answer, microusd: spend::microusd(charge.cost_nanousd), ledger_key: ledger_key(entry, effort, c.call, c.attempt) }
}

/// The reply the eval Worker records when the generation Worker did not
/// answer with a reply to `r`: `status` is its HTTP status (`None`: no
/// answer, a timeout, or a 200 whose reply could not be read).
/// - 409: the two Workers speak different interface versions
///   ([`OutcomeCode::InterfaceMismatch`]), refused before any vendor call;
/// - another status: the generation Worker refused the call before sending
///   it (a bad request, a refused admission);
/// - no reply: the call may have reached the vendor, so it is marked sent
///   and charged its reservation bound.
pub fn interface_failure(status: Option<u16>, r: &VendorRequest, latency_ms: u64) -> GenReply {
    match status {
        Some(slop_ninja_gen_core::iface::VERSION_MISMATCH_STATUS) => GenReply::without_answer(OutcomeCode::InterfaceMismatch, r, latency_ms),
        Some(_) => GenReply::without_answer(OutcomeCode::Outage, r, latency_ms),
        None => {
            let mut g = GenReply::without_answer(OutcomeCode::Outage, r, latency_ms);
            g.sent = true;
            g
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_gen_core::manifest::fixture::{measured, planned};
    use slop_ninja_gen_core::voice::fixture::library;

    fn mref() -> Value {
        json!({"key": "generation/manifests/generator-manifest-v1.planned.json", "sha256": "ab".repeat(32), "bytes": 100})
    }

    #[test]
    fn the_body_carries_a_cell_and_indices_never_text() {
        let ok = json!({"manifest": mref(), "model": "gpt-6-luna", "effort": "high", "call": 3, "attempt": 0});
        let (c, e) = MeasureCall::parse(&ok).unwrap();
        assert_eq!((c.call, c.attempt, e), (3, 0, Effort::High));
        for extra in ["text", "passage", "prompt", "system", "input"] {
            let mut b = ok.clone();
            b[extra] = json!("Any customer text here.");
            assert!(MeasureCall::parse(&b).is_err(), "{extra}");
        }
        let mut b = ok.clone();
        b["call"] = json!(measure::CALLS_PER_CELL);
        assert_eq!(MeasureCall::parse(&b).unwrap_err(), "call");
        let mut b = ok.clone();
        b["effort"] = json!("ultra");
        assert_eq!(MeasureCall::parse(&b).unwrap_err(), "effort");
        let mut b = ok;
        b.as_object_mut().unwrap().remove("attempt");
        assert!(MeasureCall::parse(&b).is_err());
    }

    #[test]
    fn every_outcome_is_charged_when_the_vendor_may_have_billed_and_keys_are_unique() {
        let m = measured(planned());
        let lib = library();
        let entry = m.model("claude-fable-5-1").unwrap();
        let (r, source) = measure::request(&m, &lib, &entry.id, Effort::Max, 2).unwrap();
        let c = MeasureCall { manifest: serde_json::from_value(mref()).unwrap(), model: entry.id.clone(), effort: "max".into(), call: 2, attempt: 1 };
        let reserve = spend::microusd(spend::reservation_nanousd(entry, r.tier, &r));
        // Not sent, refused by the generation Worker, or an interface version skew: nothing.
        for status in [Some(400), Some(503), Some(409)] {
            let g = interface_failure(status, &r, 5);
            let out = assemble(&m, entry, Effort::Max, &c, &r, &source, &g, 1);
            assert_eq!(out.microusd, 0, "{status:?}");
            assert_eq!(out.answer["content"], false);
        }
        assert_eq!(interface_failure(Some(409), &r, 5).outcome, OutcomeCode::InterfaceMismatch);
        // No reply (timeout, unreadable reply): the reservation bound, unmeasured.
        let out = assemble(&m, entry, Effort::Max, &c, &r, &source, &interface_failure(None, &r, 5), 1);
        assert_eq!((out.microusd, out.answer["unmeasured"].as_bool()), (reserve, Some(true)));
        assert!(reserve > 0);
        // Each vendor outcome after a 200: charged.
        for outcome in [OutcomeCode::Answer, OutcomeCode::Refusal, OutcomeCode::LengthTokensOut, OutcomeCode::LengthBytes, OutcomeCode::ModelMismatch, OutcomeCode::Outage] {
            let mut g = GenReply::without_answer(outcome, &r, 9);
            g.sent = true;
            g.http_status = 200;
            let out = assemble(&m, entry, Effort::Max, &c, &r, &source, &g, 1);
            assert!(out.microusd > 0, "{outcome:?}");
            assert_eq!(out.answer["observation"]["unmeasured"], true);
            assert_eq!(out.answer["observation"]["window_words"], r.word_window.1);
        }
        // The answer never carries key material or vendor error text.
        let s = out_text(&m, entry, &c, &r, &source);
        assert!(!s.contains("sk-") && !s.contains("x-api-key") && !s.contains("Bearer"));
        // Keys: one per cell, call and attempt.
        assert_eq!(ledger_key(entry, Effort::Max, 2, 1), "probe:gen-measure-anthropic:claude-fable-5-1:max:2:1");
        assert_ne!(ledger_key(entry, Effort::Max, 2, 1), ledger_key(entry, Effort::Max, 2, 2));
        assert_ne!(ledger_key(entry, Effort::Max, 2, 1), ledger_key(entry, Effort::High, 2, 1));
    }

    fn out_text(m: &Manifest, entry: &ModelEntry, c: &MeasureCall, r: &VendorRequest, source: &str) -> String {
        let mut g = GenReply::without_answer(OutcomeCode::Throttle, r, 1);
        g.sent = true;
        g.http_status = 429;
        g.retry_after_ms = Some(2000);
        let out = assemble(m, entry, Effort::Max, c, r, source, &g, 1);
        assert_eq!(out.microusd, 0, "a vendor 429 bills nothing");
        out.answer.to_string()
    }

    #[test]
    fn the_long_measurement_document_sits_just_under_the_scorer_limit() {
        let doc = measure::long_document();
        let n = slop_ninja_scorer_tokens::count(&doc, &mut |_, _| true).unwrap();
        let limit = slop_ninja_artifact_check::text::MAX_TOKENS_INCLUDING_SPECIALS;
        assert!(n <= limit && n * 10 >= limit * 9, "{n} tokens of {limit}");
    }
}
