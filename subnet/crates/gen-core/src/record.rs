//! The private per-document generation record (`slop_ninja_generation_record_v1`).
//!
//! One line per generated document in the cohort's private generation shards
//! (eval bucket), bound by digest in the private batch index. Never
//! published: none of it enters the package, receipts content, the public
//! summary, the leaderboard or the dashboard. It holds every choice and every
//! attempt; it holds no key, no vendor message text and no prompt text (the
//! request digest stands for the request).

use crate::manifest::Manifest;
use crate::outcome::{GenReply, OutcomeCode};
use crate::rotation::GenAssignment;
use crate::spend::Usage;
use crate::sse::{RefusalCategory, Stop};
use crate::vendor::{Effort, Tier, Vendor};
use crate::RECORD_SCHEMA;
use serde::{Deserialize, Serialize};

/// A reported model id as recorded: the id when the manifest knows it, else
/// `"other"` (no arbitrary vendor string is kept).
pub fn recorded_model(m: &Manifest, reported: Option<&str>) -> Option<String> {
    reported.map(|r| if m.known_model(r) { r.to_owned() } else { "other".to_owned() })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRecord {
    pub attempt: u32,
    pub vendor: Vendor,
    pub model_requested: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_reported: Option<String>,
    pub effort: Effort,
    pub tier: Tier,
    pub flex_unavailable: bool,
    pub request_sha256: String,
    pub http_status: u16,
    pub outcome: OutcomeCode,
    /// The validator's reject code of an answer (`textgen::Reject::code`,
    /// `context_echo`, `length_tokens`), when it was rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Stop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<RefusalCategory>,
    pub usage: Usage,
    /// Charged at the reservation bound (no usage reported).
    pub unmeasured: bool,
    pub cost_nanousd: u64,
    pub latency_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    pub visible_bytes: u32,
    pub visible_words: u32,
}

impl AttemptRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn from_reply(m: &Manifest, attempt: u32, vendor: Vendor, model: &str, effort: Effort, r: &GenReply, charge: crate::spend::Charge, reject: Option<&str>) -> Self {
        AttemptRecord {
            attempt,
            vendor,
            model_requested: model.to_owned(),
            model_reported: recorded_model(m, r.model_reported.as_deref()),
            effort,
            tier: r.tier,
            flex_unavailable: r.flex_unavailable,
            request_sha256: r.request_sha256.clone(),
            http_status: r.http_status,
            outcome: r.outcome,
            reject: reject.map(str::to_owned),
            stop: r.stop,
            refusal: r.refusal,
            usage: r.usage,
            unmeasured: charge.unmeasured,
            cost_nanousd: charge.cost_nanousd,
            latency_ms: r.latency_ms,
            first_token_ms: r.first_token_ms,
            visible_bytes: r.visible_bytes,
            visible_words: r.text.as_deref().map_or(0, |t| crate::request::words(t) as u32),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalRecord {
    pub attempt: u32,
    pub text_sha256: String,
    /// The model text's final byte spans.
    pub ai_spans: Vec<(u64, u64)>,
    pub words: u64,
    pub ai_words: u64,
    pub share_ppb: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenRecord {
    pub schema: String,
    pub doc: String,
    pub manifest_sha256: String,
    pub voice_library_sha256: String,
    pub assignment: GenAssignment,
    /// Mixed: the parent's key (or the composer's skeleton record digest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Mixed with the match-surrounding template: the context ranges given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<((u64, u64), (u64, u64))>,
    pub attempts: Vec<AttemptRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_: Option<FinalRecord>,
}

impl GenRecord {
    pub fn new(doc: &str, manifest_sha256: &str, voice_library_sha256: &str, assignment: GenAssignment) -> Self {
        GenRecord {
            schema: RECORD_SCHEMA.into(),
            doc: doc.into(),
            manifest_sha256: manifest_sha256.into(),
            voice_library_sha256: voice_library_sha256.into(),
            assignment,
            parent: None,
            context: None,
            attempts: Vec::new(),
            final_: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::fixture::planned;
    use crate::outcome::GenCall;
    use crate::request::VendorRequest;
    use crate::rotation::{Avoid, StreamState, assign, stream_key};
    use crate::sse::{SseReader, fixture};
    use crate::vendor::DocKind;
    use crate::voice::fixture::library;

    const KEY: &str = "sk-ant-api03-ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ";

    #[test]
    fn records_hold_no_key_and_no_vendor_text() {
        let m = planned();
        let lib = library();
        let mut st = StreamState::default();
        let a = assign(&mut st, &m, &lib, crate::voice::VoiceSplit::NoneHalf, &[1; 32], &stream_key(1, DocKind::ModelOnly, "wikipedia"), DocKind::ModelOnly, "doc", &Avoid::default(), &|_| None).unwrap();
        let req = VendorRequest {
            vendor: Vendor::Anthropic,
            route: crate::vendor::Route::Direct,
            model: "claude-opus-5-5".into(),
            effort: Effort::Low,
            tier: Tier::Standard,
            body: "{}".into(),
            visible_bound: 4096,
            line_bound: 8192,
            word_window: (1, 900),
            max_tokens: 0,
            request_sha256: "11".repeat(32),
            anthropic_version: None,
        };
        let call = GenCall { request: req.clone(), accepted_models: vec!["claude-opus-5-5".into()], timeout_ms: 1 };
        // A stream that reports a key-like model id and refuses with vendor text.
        let body = fixture::anthropic(KEY, "", "refusal", Some("cyber"));
        let mut r = SseReader::new(Vendor::Anthropic, 4096, 8192);
        r.feed(body.as_bytes());
        let reply = GenReply::from_stream(r.finish(), &call, &req, 200, false, 5, None);
        let mut rec = GenRecord::new("doc", &"22".repeat(32), &"33".repeat(32), a);
        rec.attempts.push(AttemptRecord::from_reply(&m, 0, Vendor::Anthropic, "claude-opus-5-5", Effort::Low, &reply, crate::spend::Charge { cost_nanousd: 9, unmeasured: false }, None));
        let s = serde_json::to_string(&rec).unwrap();
        assert!(!s.contains("sk-ant") && !s.contains("vendor text") && !s.contains("TEST"), "{s}");
        assert_eq!(recorded_model(&m, Some(KEY)).as_deref(), Some("other"));
        assert_eq!(recorded_model(&m, Some("gpt-6-luna")).as_deref(), Some("gpt-6-luna"));
        let back: GenRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(back, rec);
    }
}
