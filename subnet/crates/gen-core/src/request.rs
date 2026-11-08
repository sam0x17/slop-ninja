//! Prompts from the manifest's templates, and the vendor request of an
//! assignment.
//!
//! Request shapes (vendor pages and the claude-api reference, 2026-11-04):
//! - **OpenAI** (`POST /v1/responses`): `store: false`, `stream: true`,
//!   `reasoning: {effort}`, `max_output_tokens` ([`max_tokens`]), `service_tier: "flex"` (Batch
//!   prices, synchronous). Never `temperature` or `top_p` (accepted only at
//!   `none`; omitted everywhere so decoding is the vendor default), no
//!   verbosity.
//! - **Anthropic** (`POST /v1/messages`, the manifest's pinned
//!   `anthropic-version`): `stream: true`, never `temperature`, `top_p`,
//!   `top_k` or `budget_tokens`, no assistant prefill, no tools, and never the
//!   server-side `fallbacks` parameter (a fallback would answer from another
//!   model and mislabel the generator). Thinking per model
//!   ([`crate::vendor::AnthropicThinking`]).
//! - **Workers AI** (the eval Worker's `AI.run`): today's `temperature` 0.7 /
//!   `top_p` 0.9 and an explicit `max_tokens` ([`max_tokens`]), and the
//!   reasoning setting only on the models that document one.
//!
//! At a configured endpoint ([`crate::vendor::Route::ConfiguredEndpointV1`],
//! the manifest's per-vendor route) the shapes differ in two ways only, both
//! recorded by the manifest and the request: OpenAI sends no `service_tier`
//! (the endpoint has no Flex), and when the manifest gives an Anthropic
//! system prefix, `system` is two text blocks, that prefix then the
//! template's system prompt. Both are priced [`Tier::Endpoint`].
//!
//! The body is canonical JSON (sorted keys) and holds no key: its digest is
//! the request digest recorded with every attempt.

use crate::manifest::{Intensity, Manifest, Refused, WaiReasoning};
use crate::rotation::GenAssignment;
use crate::vendor::{AnthropicThinking, DocKind, Effort, Route, Tier, Vendor, anthropic_thinking};
use crate::voice::{Library, VoiceChoice};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::ops::Range;

/// Today's Workers AI sampling (`textgen::TEMPERATURE`, `TOP_P`), kept for
/// the Workers AI half.
pub const WAI_TEMPERATURE: f64 = 0.7;
pub const WAI_TOP_P: f64 = 0.9;

pub fn words(s: &str) -> usize {
    s.split_whitespace().count()
}

/// What a request is built for. The composer (later) supplies a composite's
/// skeleton as `parent` and its other runs as `context`; this layer knows
/// nothing of how a parent was built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenTarget<'a> {
    Rewrite {
        domain: &'a str,
        parent: &'a str,
        span: Range<usize>,
        /// The same document's text before and after the span: given to the
        /// model only with the match-surrounding template.
        context: Option<(Range<usize>, Range<usize>)>,
    },
    Scratch { domain: &'a str, topic: &'a str, target_words: u32 },
}

/// How a composite's pieces retry (the composer's choice).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryPolicy {
    /// Mixed today: the next model of the lane ([`crate::rotation::retry_cell`]).
    RotateModelInLane,
    /// Model-only composite pieces: the same model and effort.
    SameModel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub system: String,
    pub user: String,
}

/// One vendor call, ready to send (the key is added by the generation
/// Worker, never here).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VendorRequest {
    pub vendor: Vendor,
    /// How the call reaches the vendor (the manifest's route for it): the
    /// generation Worker sends it only over a route configured the same.
    pub route: Route,
    pub model: String,
    pub effort: Effort,
    pub tier: Tier,
    /// Canonical JSON body.
    pub body: String,
    /// The visible text this call can have accepted (`stream_bound_v1`).
    pub visible_bound: u32,
    /// The longest SSE line (the visible bound plus the manifest's envelope);
    /// also the bound of an error body.
    pub line_bound: u32,
    /// The validator's word window `[min, max]` for this item.
    pub word_window: (u32, u32),
    /// The output token limit the body requests ([`max_tokens`]).
    pub max_tokens: u32,
    pub request_sha256: String,
    /// `anthropic-version`, Anthropic only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_version: Option<String>,
}

/// Fills `template`'s `{name}` placeholders in one pass over the template:
/// a value is never scanned for placeholders.
pub fn fill(template: &str, values: &[(&str, &str)]) -> Result<String, Refused> {
    let mut out = String::with_capacity(template.len() + values.iter().map(|(_, v)| v.len()).sum::<usize>());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(rest.get(..open).ok_or(Refused("template"))?);
        let after = rest.get(open + 1..).ok_or(Refused("template"))?;
        let close = after.find('}').ok_or(Refused("template (unbalanced braces)"))?;
        let name = after.get(..close).ok_or(Refused("template"))?;
        let v = values.iter().find(|(k, _)| *k == name).ok_or(Refused("template (a placeholder without a value)"))?;
        out.push_str(v.1);
        rest = after.get(close + 1..).ok_or(Refused("template"))?;
    }
    out.push_str(rest);
    Ok(out)
}

/// The validator's word window (`textgen::validate_rewrite` and
/// `validate_scratch`): rewrite `[0.5a, 1.8a + 4]`, scratch `[max(0.6t, 80),
/// 1.8t]`, in whole words.
pub fn word_window(kind: DocKind, n: u32) -> (u32, u32) {
    let n64 = u64::from(n);
    match kind {
        DocKind::Mixed => (n64.div_ceil(2) as u32, ((18 * n64 + 40) / 10) as u32),
        DocKind::ModelOnly => (((6 * n64).div_ceil(10)).max(80) as u32, ((18 * n64) / 10) as u32),
    }
}

/// The prompt of `a` for `target`.
pub fn prompt(m: &Manifest, lib: &Library, a: &GenAssignment, target: &GenTarget<'_>) -> Result<Prompt, Refused> {
    let t = m.template(&a.template).ok_or(Refused("request (template)"))?;
    if t.kind != a.kind || crate::rotation::template_for(a.kind, &a.voice) != a.template {
        return Err(Refused("request (template does not match the assignment)"));
    }
    let voice = match &a.voice {
        VoiceChoice::None => "",
        VoiceChoice::Profile { id, .. } => lib.entry(id).map(|e| e.text.as_str()).ok_or(Refused("request (voice entry)"))?,
    };
    match (target, a.kind) {
        (GenTarget::Rewrite { domain, parent, span, context }, DocKind::Mixed) => {
            let passage = parent.get(span.clone()).ok_or(Refused("request (span)"))?;
            let register = m.register(domain).ok_or(Refused("request (no register for the domain)"))?;
            let intensity = a.intensity.unwrap_or(Intensity::Light);
            let op = a
                .operation
                .as_deref()
                .and_then(|id| m.operations.of(intensity).iter().find(|o| o.id == id))
                .ok_or(Refused("request (operation)"))?;
            let n = words(passage).to_string();
            let match_ctx = a.template == "rewrite_v2_match_context";
            let (before, after) = match (match_ctx, context) {
                (true, Some((b, f))) => {
                    let ok = b.end <= span.start && f.start >= span.end;
                    if !ok {
                        return Err(Refused("request (context overlaps the span)"));
                    }
                    (parent.get(b.clone()).ok_or(Refused("request (context)"))?, parent.get(f.clone()).ok_or(Refused("request (context)"))?)
                }
                (true, None) => return Err(Refused("request (match-context needs the surrounding text)")),
                // Context enters a prompt only with the match-context template.
                (false, _) => ("", ""),
            };
            let values = [
                ("voice", voice),
                ("operation", op.text.as_str()),
                ("register", register.register.as_str()),
                ("n_words", n.as_str()),
                ("passage", passage),
                ("context_before", before),
                ("context_after", after),
            ];
            Ok(Prompt { system: fill(&t.system, &values)?, user: fill(&t.user, &values)? })
        }
        (GenTarget::Scratch { domain, topic, target_words }, DocKind::ModelOnly) => {
            let register = m.register(domain).ok_or(Refused("request (no register for the domain)"))?;
            let w = target_words.to_string();
            let values = [("voice", voice), ("register", register.register.as_str()), ("topic", *topic), ("target_words", w.as_str())];
            Ok(Prompt { system: fill(&t.system, &values)?, user: fill(&t.user, &values)? })
        }
        _ => Err(Refused("request (target does not match the kind)")),
    }
}

/// Today's visible-length budget for Workers AI (`textgen`): rewrite `3n`
/// in `[96, 1024]`, scratch `3t` in `[256, 1200]`.
fn wai_visible_tokens(kind: DocKind, n: u32) -> u32 {
    match kind {
        DocKind::Mixed => n.saturating_mul(3).clamp(96, 1024),
        DocKind::ModelOnly => n.saturating_mul(3).clamp(256, 1200),
    }
}

/// The output-limit rule this build implements (the limit itself is in the
/// body, so in the request digest of every attempt).
pub const MAX_TOKENS_RULE: &str = "max_tokens_v1";

/// Visible tokens per word of the validator's window: today's `textgen`
/// basis (`3n` tokens for an `n`-word passage), applied to the window's
/// maximum, so every answer the validator could accept fits.
pub const VISIBLE_TOKENS_PER_WORD: u32 = 3;

/// The output token limit of a call (`max_tokens_v1`):
/// - a Workers AI model without a reasoning setting: today's `textgen`
///   budget, unchanged;
/// - every other model (frontier, and Workers AI models that reason) before
///   its cell is measured: the model's documented maximum (the measurement
///   run needs the room, and records what it used);
/// - measured: the window's visible tokens ([`VISIBLE_TOKENS_PER_WORD`] per
///   word of its maximum) plus the cell's measured headroom doubled and
///   rounded up to a power of two (the envelope bound's margin rule), at
///   most the documented maximum. The headroom is the reported reasoning
///   maximum where the vendor reports one, else the whole measured output
///   maximum (Anthropic counts thinking inside output and reports no split;
///   some Workers AI models report no detail): never zero for a cell that
///   produced output.
///
/// Both vendors count a request's output limit against throughput limits, so
/// a measured limit, not the model's maximum, is what a cohort sends.
pub fn max_tokens(m: &Manifest, entry: &crate::manifest::ModelEntry, effort: Effort, kind: DocKind, n: u32, window_max: u32) -> u32 {
    if entry.vendor == Vendor::WorkersAi && entry.wai_reasoning.is_none() {
        return wai_visible_tokens(kind, n);
    }
    let measured = match (m.status, entry.measured(effort)) {
        (crate::manifest::Status::Measured, Some(x)) => x,
        _ => return entry.max_output_tokens,
    };
    let headroom = if measured.reasoning_tokens_max > 0 { measured.reasoning_tokens_max } else { measured.output_tokens_max };
    let headroom = u32::try_from(headroom).unwrap_or(u32::MAX).max(1).saturating_mul(2).checked_next_power_of_two().unwrap_or(u32::MAX);
    let visible = window_max.saturating_mul(VISIBLE_TOKENS_PER_WORD);
    visible.saturating_add(headroom).min(entry.max_output_tokens)
}

/// The request of `a` for `target`.
pub fn build(m: &Manifest, lib: &Library, a: &GenAssignment, target: &GenTarget<'_>) -> Result<VendorRequest, Refused> {
    let entry = m.model(&a.model).ok_or(Refused("request (model)"))?;
    if !entry.effort_levels.contains(&a.effort) {
        return Err(Refused("request (effort is not a documented level of the model)"));
    }
    let p = prompt(m, lib, a, target)?;
    let n = match target {
        GenTarget::Rewrite { parent, span, .. } => words(parent.get(span.clone()).unwrap_or("")) as u32,
        GenTarget::Scratch { target_words, .. } => *target_words,
    };
    let window = word_window(a.kind, n);
    let limit = max_tokens(m, entry, a.effort, a.kind, n, window.1);
    let visible_bound = window.1.saturating_mul(m.stream_bound.word_bytes_max.saturating_add(2));
    let line_bound = visible_bound.saturating_add(m.stream_bound.envelope_bytes);
    let route = m.routes.of(entry.vendor);
    let configured = route == Route::ConfiguredEndpointV1;
    let (tier, mut body) = match entry.vendor {
        Vendor::Openai => {
            let mut b = json!({
                "model": entry.id,
                "instructions": p.system,
                "input": [{"role": "user", "content": p.user}],
                "max_output_tokens": limit,
                "reasoning": {"effort": a.effort.code()},
                "service_tier": "flex",
                "store": false,
                "stream": true,
            });
            let o = b.as_object_mut().ok_or(Refused("request"))?;
            if a.effort == Effort::Default {
                o.remove("reasoning");
            }
            if configured {
                o.remove("service_tier");
                (Tier::Endpoint, b)
            } else {
                (Tier::Flex, b)
            }
        }
        Vendor::Anthropic => {
            let rule = anthropic_thinking(&entry.id).ok_or(Refused("request (Anthropic thinking rule)"))?;
            let system = match m.routes.anthropic_system_prefix.as_deref() {
                Some(prefix) if configured => json!([{"type": "text", "text": prefix}, {"type": "text", "text": p.system}]),
                _ => json!(p.system),
            };
            let mut b = json!({
                "model": entry.id,
                "max_tokens": limit,
                "system": system,
                "messages": [{"role": "user", "content": p.user}],
                "stream": true,
            });
            let o = b.as_object_mut().ok_or(Refused("request"))?;
            match (rule, a.effort) {
                (AnthropicThinking::DisabledAtNone, Effort::None) => {
                    o.insert("thinking".into(), json!({"type": "disabled"}));
                }
                (AnthropicThinking::BetweenToolsAtNone, Effort::None) => {
                    o.insert("thinking".into(), json!({"type": "between_tools"}));
                }
                (AnthropicThinking::AlwaysOn, Effort::None) | (_, Effort::Default) => {
                    return Err(Refused("request (no such Anthropic level)"));
                }
                (_, e) => {
                    o.insert("output_config".into(), json!({"effort": e.code()}));
                }
            }
            (if configured { Tier::Endpoint } else { Tier::Standard }, b)
        }
        Vendor::WorkersAi => {
            let mut b = json!({
                "messages": [{"role": "system", "content": p.system}, {"role": "user", "content": p.user}],
                "max_tokens": limit,
                "temperature": WAI_TEMPERATURE,
                "top_p": WAI_TOP_P,
            });
            let o = b.as_object_mut().ok_or(Refused("request"))?;
            match (entry.wai_reasoning, a.effort) {
                (_, Effort::Default) => {}
                (Some(WaiReasoning::ReasoningObject), e) => {
                    o.insert("reasoning".into(), json!({"effort": e.code()}));
                }
                (Some(WaiReasoning::ReasoningEffort), e) => {
                    o.insert("reasoning_effort".into(), json!(e.code()));
                }
                (None, _) => return Err(Refused("request (a level without a reasoning setting)")),
            }
            (Tier::Standard, b)
        }
    };
    // Canonical JSON with sorted keys in every build: serde_json's map keeps
    // insertion order when any crate in the build enables `preserve_order`
    // (rtao's clients do), and the body's bytes are its `request_sha256` and
    // the shape `standard_retry` edits. Without that feature the map is
    // already sorted and this does nothing.
    body.sort_all_objects();
    let body = serde_json::to_string(&body).map_err(|_| Refused("request json"))?;
    if body.len() > crate::iface::CALL_BODY_BYTES {
        return Err(Refused("request (body over the interface bound)"));
    }
    if visible_bound as usize > crate::iface::VISIBLE_BYTES_MAX {
        return Err(Refused("request (visible bound over the interface bound)"));
    }
    Ok(VendorRequest {
        vendor: entry.vendor,
        route,
        model: entry.id.clone(),
        effort: a.effort,
        tier,
        request_sha256: crate::hash::sha256_hex(body.as_bytes()),
        body,
        visible_bound,
        line_bound,
        word_window: window,
        max_tokens: limit,
        anthropic_version: (entry.vendor == Vendor::Anthropic).then(|| m.anthropic_version.clone()),
    })
}

/// The standard-tier retry of an OpenAI Flex request (after Flex's "429
/// Resource Unavailable", which is not charged): the same body without
/// `service_tier`. The body is canonical JSON with sorted keys, so the
/// member is the one unescaped `"service_tier":"flex",` (inside a string
/// value every quote is escaped); it is removed in place, without parsing,
/// so the retry costs one copy of the body ([`crate::iface::call_heap_bytes`]
/// admits it).
pub fn standard_retry(r: &VendorRequest) -> Result<VendorRequest, Refused> {
    const MEMBER: &str = "\"service_tier\":\"flex\",";
    if r.vendor != Vendor::Openai || r.tier != Tier::Flex {
        return Err(Refused("request (only an OpenAI flex request has a standard retry)"));
    }
    let at = r.body.find(MEMBER).ok_or(Refused("request (no service_tier member)"))?;
    let mut body = String::new();
    body.try_reserve_exact(r.body.len() - MEMBER.len()).map_err(|_| Refused("request (retry body)"))?;
    body.push_str(r.body.get(..at).ok_or(Refused("request json"))?);
    body.push_str(r.body.get(at + MEMBER.len()..).ok_or(Refused("request json"))?);
    Ok(VendorRequest { tier: Tier::Standard, request_sha256: crate::hash::sha256_hex(body.as_bytes()), body, ..r.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::fixture::{measured, planned};
    use crate::rotation::{Avoid, Positions, assign, stream_key};
    use crate::vendor::{Lane, documented_efforts};
    use crate::voice::Category;
    use crate::voice::fixture::library;
    use serde_json::Value;

    const PARENT: &str = "Alpha beta gamma delta. The council met on Monday to discuss the bridge. It voted to repair it. Work starts soon.";

    fn assignment(model: &str, effort: Effort, kind: DocKind, voice: VoiceChoice) -> GenAssignment {
        let lane = if model.starts_with("@cf/") { Lane::WorkersAi } else { Lane::Frontier };
        GenAssignment {
            rotation_version: crate::ROTATION_VERSION.into(),
            manifest_version: 1,
            stream: stream_key(1, kind, "wikipedia"),
            kind,
            lane,
            model: model.into(),
            effort,
            span_percent: (kind == DocKind::Mixed).then_some(40),
            intensity: (kind == DocKind::Mixed).then_some(Intensity::Heavy),
            operation: (kind == DocKind::Mixed).then(|| "paraphrase".to_owned()),
            scratch_words: (kind == DocKind::ModelOnly).then_some(200),
            template: crate::rotation::template_for(kind, &voice).into(),
            voice,
            voice_library_version: 1,
            positions: Positions::default(),
            fallback: None,
        }
    }

    fn span() -> Range<usize> {
        let s = PARENT.find("The council").unwrap();
        s..PARENT.find(" Work").unwrap()
    }

    fn rewrite() -> GenTarget<'static> {
        let s = span();
        GenTarget::Rewrite { domain: "wikipedia", parent: PARENT, span: s.clone(), context: Some((0..s.start - 1, s.end + 1..PARENT.len())) }
    }

    /// The parsed body, after checking that the bytes are canonical: the
    /// body re-serialized with every object's keys sorted is the body
    /// itself, whatever map order serde_json has in this build.
    fn body(r: &VendorRequest) -> Value {
        let mut v: Value = serde_json::from_str(&r.body).unwrap();
        v.sort_all_objects();
        assert_eq!(serde_json::to_string(&v).unwrap(), r.body, "{} body is not canonical", r.model);
        v
    }

    #[test]
    fn fill_is_one_pass() {
        assert_eq!(fill("a {x} b {y}", &[("x", "{y}"), ("y", "2")]).unwrap(), "a {y} b 2");
        assert!(fill("a {z}", &[("x", "1")]).is_err());
    }

    #[test]
    fn openai_requests_per_effort_are_golden() {
        let m = planned();
        let lib = library();
        for id in ["gpt-6-luna", "gpt-6.1-terra", "gpt-6.1-sol", "gpt-6-astra"] {
            for e in documented_efforts(id).unwrap() {
                let r = build(&m, &lib, &assignment(id, *e, DocKind::Mixed, VoiceChoice::None), &rewrite()).unwrap();
                let b = body(&r);
                assert_eq!(r.tier, Tier::Flex);
                assert_eq!(b["model"], id);
                assert_eq!(b["store"], false);
                assert_eq!(b["stream"], true);
                assert_eq!(b["service_tier"], "flex");
                assert_eq!(b["reasoning"]["effort"], e.code());
                assert_eq!(b["max_output_tokens"], 128_000);
                for banned in ["temperature", "top_p", "top_k", "seed", "text", "fallbacks"] {
                    assert!(b.get(banned).is_none(), "{id} {banned}");
                }
                assert_eq!(r.request_sha256, crate::hash::sha256_hex(r.body.as_bytes()));
                let std = standard_retry(&r).unwrap();
                assert_eq!(std.tier, Tier::Standard);
                assert!(body(&std).get("service_tier").is_none());
                // The in-place removal is the canonical body without the member.
                // (`remove` is a swap-remove under `preserve_order`; re-sort.)
                let mut v = body(&r);
                v.as_object_mut().unwrap().remove("service_tier");
                v.sort_all_objects();
                assert_eq!(std.body, serde_json::to_string(&v).unwrap());
                assert_ne!(std.request_sha256, r.request_sha256);
            }
        }
        // The golden body of one cell, byte for byte.
        let r = build(&m, &lib, &assignment("gpt-6-luna", Effort::None, DocKind::Mixed, VoiceChoice::None), &rewrite()).unwrap();
        assert_eq!(
            r.body,
            r#"{"input":[{"content":"From a TEST encyclopedia lead. About 14 words.\n\nThe council met on Monday to discuss the bridge. It voted to repair it.","role":"user"}],"instructions":"TEST plain. TEST operation paraphrase. Reply with the passage only.","max_output_tokens":128000,"model":"gpt-6-luna","reasoning":{"effort":"none"},"service_tier":"flex","store":false,"stream":true}"#
        );
    }

    /// At a configured endpoint: OpenAI without `service_tier` (no Flex,
    /// so no standard retry), Anthropic with the manifest's system prefix as
    /// a separate first system block; both priced at the endpoint tier, and
    /// everything else the direct body's.
    #[test]
    fn configured_endpoint_requests_are_golden() {
        use crate::manifest::fixture::{SYSTEM_PREFIX, configured};
        let direct = planned();
        let m = configured(planned());
        let lib = library();
        for id in ["gpt-6-luna", "gpt-6.1-terra", "gpt-6.1-sol", "gpt-6-astra"] {
            for e in documented_efforts(id).unwrap() {
                let a = assignment(id, *e, DocKind::Mixed, VoiceChoice::None);
                let (r, d) = (build(&m, &lib, &a, &rewrite()).unwrap(), build(&direct, &lib, &a, &rewrite()).unwrap());
                assert_eq!((r.route, r.tier), (Route::ConfiguredEndpointV1, Tier::Endpoint));
                assert_eq!(d.route, Route::Direct);
                let mut want = body(&d);
                want.as_object_mut().unwrap().remove("service_tier");
                want.sort_all_objects();
                assert_eq!(body(&r), want, "{id} {e:?}");
                assert!(standard_retry(&r).is_err(), "no Flex, so no standard retry");
            }
        }
        let r = build(&m, &lib, &assignment("gpt-6-luna", Effort::None, DocKind::Mixed, VoiceChoice::None), &rewrite()).unwrap();
        assert_eq!(
            r.body,
            r#"{"input":[{"content":"From a TEST encyclopedia lead. About 14 words.\n\nThe council met on Monday to discuss the bridge. It voted to repair it.","role":"user"}],"instructions":"TEST plain. TEST operation paraphrase. Reply with the passage only.","max_output_tokens":128000,"model":"gpt-6-luna","reasoning":{"effort":"none"},"store":false,"stream":true}"#
        );
        for id in ["claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5", "claude-fable-5-1"] {
            for e in documented_efforts(id).unwrap() {
                let a = assignment(id, *e, DocKind::ModelOnly, VoiceChoice::None);
                let t = GenTarget::Scratch { domain: "fineweb", topic: "a bicycle", target_words: 200 };
                let (r, d) = (build(&m, &lib, &a, &t).unwrap(), build(&direct, &lib, &a, &t).unwrap());
                assert_eq!((r.route, r.tier, d.tier), (Route::ConfiguredEndpointV1, Tier::Endpoint, Tier::Standard));
                let (b, mut want) = (body(&r), body(&d));
                assert_eq!(b["system"], json!([{"type": "text", "text": SYSTEM_PREFIX}, {"type": "text", "text": want["system"].clone()}]));
                want["system"] = b["system"].clone();
                assert_eq!(b, want, "{id} {e:?}: only the system prompt differs");
                assert_eq!(r.anthropic_version, d.anthropic_version);
            }
        }
        // Without a prefix, a configured Anthropic endpoint gets the direct
        // body (the system prompt as one string), priced at the endpoint tier.
        let mut np = configured(planned());
        np.routes.anthropic_system_prefix = None;
        let a = assignment("claude-haiku-5-5", Effort::Low, DocKind::Mixed, VoiceChoice::None);
        let (r, d) = (build(&np, &lib, &a, &rewrite()).unwrap(), build(&direct, &lib, &a, &rewrite()).unwrap());
        assert_eq!((r.route, r.tier, body(&r)), (Route::ConfiguredEndpointV1, Tier::Endpoint, body(&d)));
        // Workers AI is never routed.
        let w = build(&m, &lib, &assignment("@cf/meta/llama-3.2-3b-instruct", Effort::Default, DocKind::Mixed, VoiceChoice::None), &rewrite()).unwrap();
        assert_eq!((w.route, w.tier), (Route::Direct, Tier::Standard));
    }

    #[test]
    fn anthropic_requests_per_effort_are_golden() {
        let m = planned();
        let lib = library();
        for id in ["claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5", "claude-fable-5-1"] {
            for e in documented_efforts(id).unwrap() {
                let r = build(&m, &lib, &assignment(id, *e, DocKind::ModelOnly, VoiceChoice::None), &GenTarget::Scratch { domain: "fineweb", topic: "a bicycle", target_words: 200 }).unwrap();
                let b = body(&r);
                assert_eq!(r.anthropic_version.as_deref(), Some("2026-10-21"));
                assert_eq!(b["stream"], true);
                assert_eq!(b["max_tokens"], 128_000);
                for banned in ["temperature", "top_p", "top_k", "fallbacks", "tools", "service_tier"] {
                    assert!(b.get(banned).is_none(), "{id} {banned}");
                }
                assert!(!r.body.contains("budget_tokens"));
                let msgs = b["messages"].as_array().unwrap();
                assert_eq!(msgs.len(), 1);
                assert_eq!(msgs[0]["role"], "user", "no assistant prefill");
                match (id, e) {
                    ("claude-haiku-5-5", Effort::None) => {
                        assert_eq!(b["thinking"], json!({"type": "disabled"}));
                        assert!(b.get("output_config").is_none());
                    }
                    ("claude-sonnet-5-5", Effort::None) => {
                        assert_eq!(b["thinking"], json!({"type": "between_tools"}));
                        assert!(b.get("output_config").is_none());
                    }
                    _ => {
                        assert!(b.get("thinking").is_none(), "{id} {e:?}: thinking omitted (adaptive)");
                        assert_eq!(b["output_config"]["effort"], e.code());
                    }
                }
            }
        }
        // Opus and Fable have no `none`; Workers AI levels are not Anthropic's.
        let a = assignment("claude-opus-5-5", Effort::None, DocKind::Mixed, VoiceChoice::None);
        assert!(build(&m, &lib, &a, &rewrite()).is_err());
    }

    #[test]
    fn workers_ai_requests_keep_todays_shape() {
        let m = planned();
        let lib = library();
        let r = build(&m, &lib, &assignment("@cf/meta/llama-3.2-3b-instruct", Effort::Default, DocKind::Mixed, VoiceChoice::None), &rewrite()).unwrap();
        let b = body(&r);
        assert_eq!(b["temperature"], WAI_TEMPERATURE);
        assert_eq!(b["top_p"], WAI_TOP_P);
        assert!(b.get("reasoning").is_none() && b.get("model").is_none());
        assert_eq!(b["messages"][0]["role"], "system");
        // A model without a reasoning setting: today's textgen budget (3n in
        // [96, 1024]), planned or measured. A reasoning model: its maximum
        // before measurement, then the visible window plus its measured
        // headroom.
        assert_eq!(b["max_tokens"], 96);
        let r = build(&m, &lib, &assignment("@cf/openai/gpt-oss-20b", Effort::High, DocKind::Mixed, VoiceChoice::None), &rewrite()).unwrap();
        assert_eq!(body(&r)["max_tokens"], 4_096);
        let mut mm = measured(planned());
        mm.models.iter_mut().find(|x| x.id == "@cf/openai/gpt-oss-20b").unwrap().max_output_tokens = 100_000;
        let r = build(&mm, &lib, &assignment("@cf/openai/gpt-oss-20b", Effort::High, DocKind::Mixed, VoiceChoice::None), &rewrite()).unwrap();
        let b = body(&r);
        assert_eq!(b["reasoning"]["effort"], "high");
        // Measured: the window's visible tokens (29 words x 3) plus twice the
        // measured reasoning maximum (5,000) rounded up to a power of two.
        assert_eq!(b["max_tokens"], 29 * 3 + 16_384);
        assert_eq!(r.max_tokens, 29 * 3 + 16_384);
        let r = build(&mm, &lib, &assignment("@cf/meta/llama-3.2-3b-instruct", Effort::Default, DocKind::Mixed, VoiceChoice::None), &rewrite()).unwrap();
        assert_eq!(body(&r)["max_tokens"], 96);
    }

    #[test]
    fn measured_frontier_limits_follow_the_window_not_the_model_maximum() {
        let lib = library();
        let mut m = measured(planned());
        // Planned: the documented maximum (the measurement run's room).
        let p = build(&planned(), &lib, &assignment("claude-fable-5-1", Effort::Max, DocKind::Mixed, VoiceChoice::None), &rewrite()).unwrap();
        assert_eq!(p.max_tokens, 128_000);
        // Measured, reasoning reported (OpenAI): visible + 2 x reasoning, power of two.
        let r = build(&m, &lib, &assignment("gpt-6-astra", Effort::Xhigh, DocKind::Mixed, VoiceChoice::None), &rewrite()).unwrap();
        assert_eq!((r.max_tokens, body(&r)["max_output_tokens"].as_u64()), (29 * 3 + 16_384, Some(29 * 3 + 16_384)));
        // No reasoning detail (Anthropic counts thinking in output): the
        // whole measured output maximum is the headroom.
        let fable = m.models.iter_mut().find(|x| x.id == "claude-fable-5-1").unwrap();
        fable.measured.as_mut().unwrap().get_mut("max").unwrap().reasoning_tokens_max = 0;
        let r = build(&m, &lib, &assignment("claude-fable-5-1", Effort::Max, DocKind::Mixed, VoiceChoice::None), &rewrite()).unwrap();
        assert_eq!((r.max_tokens, body(&r)["max_tokens"].as_u64()), (29 * 3 + 16_384, Some(29 * 3 + 16_384)));
        // Never past the documented maximum.
        let fable = m.models.iter_mut().find(|x| x.id == "claude-fable-5-1").unwrap();
        fable.measured.as_mut().unwrap().get_mut("max").unwrap().output_tokens_max = 200_000;
        let r = build(&m, &lib, &assignment("claude-fable-5-1", Effort::Max, DocKind::Mixed, VoiceChoice::None), &rewrite()).unwrap();
        assert_eq!(r.max_tokens, 128_000);
    }

    #[test]
    fn voice_choices_shape_the_prompt() {
        let m = planned();
        let lib = library();
        let id = "claude-sonnet-5-5";
        // None: the plain template, no voice text and no context.
        let p = prompt(&m, &lib, &assignment(id, Effort::Low, DocKind::Mixed, VoiceChoice::None), &rewrite()).unwrap();
        assert!(p.system.starts_with("TEST plain.") && !p.system.contains("TEST voice"));
        assert!(!p.user.contains("Alpha beta") && !p.user.contains("Work starts"));
        // A profile: the voiced template, the entry first, still no context.
        let v = VoiceChoice::Profile { category: Category::Evasion, id: "evasion_1".into() };
        let p = prompt(&m, &lib, &assignment(id, Effort::Low, DocKind::Mixed, v), &rewrite()).unwrap();
        assert!(p.system.starts_with("TEST voice evasion 1.") && !p.user.contains("Alpha beta"));
        // Match-surrounding: the same document's text around the span only.
        let v = VoiceChoice::Profile { category: Category::MatchSurrounding, id: "match_surrounding_0".into() };
        let a = assignment(id, Effort::Low, DocKind::Mixed, v);
        let p = prompt(&m, &lib, &a, &rewrite()).unwrap();
        assert!(p.user.contains("Before:\nAlpha beta gamma delta.") && p.user.contains("After:\nWork starts soon."));
        let s = span();
        let no_ctx = GenTarget::Rewrite { domain: "wikipedia", parent: PARENT, span: s.clone(), context: None };
        assert!(prompt(&m, &lib, &a, &no_ctx).is_err());
        let overlap = GenTarget::Rewrite { domain: "wikipedia", parent: PARENT, span: s.clone(), context: Some((0..s.start + 3, s.end..PARENT.len())) };
        assert!(prompt(&m, &lib, &a, &overlap).is_err());
        // Scratch: the voiced or plain scratch template, no source text.
        let v = VoiceChoice::Profile { category: Category::Era, id: "era_0".into() };
        let t = GenTarget::Scratch { domain: "fineweb", topic: "a bicycle", target_words: 250 };
        let p = prompt(&m, &lib, &assignment(id, Effort::Low, DocKind::ModelOnly, v), &t).unwrap();
        assert!(p.system.starts_with("TEST voice era 0.") && p.user.contains("a bicycle") && p.user.contains("250"));
        // A domain without a register is refused (plan admission checks every pool domain).
        let t = GenTarget::Scratch { domain: "nowhere", topic: "x", target_words: 150 };
        assert_eq!(prompt(&m, &lib, &assignment(id, Effort::Low, DocKind::ModelOnly, VoiceChoice::None), &t).unwrap_err().0, "request (no register for the domain)");
    }

    #[test]
    fn bounds_follow_the_word_window() {
        assert_eq!(word_window(DocKind::Mixed, 14), (7, 29));
        assert_eq!(word_window(DocKind::ModelOnly, 200), (120, 360));
        assert_eq!(word_window(DocKind::ModelOnly, 100), (80, 180));
        let m = planned();
        let lib = library();
        let r = build(&m, &lib, &assignment("gpt-6-astra", Effort::Low, DocKind::Mixed, VoiceChoice::None), &rewrite()).unwrap();
        assert_eq!(r.visible_bound, 29 * 402);
        assert_eq!(r.line_bound, 29 * 402 + 4096);
    }

    #[test]
    fn every_drawn_assignment_builds() {
        let m = planned();
        let lib = library();
        let mut st = Default::default();
        for kind in [DocKind::Mixed, DocKind::ModelOnly] {
            let s = stream_key(1, kind, "wikipedia");
            for i in 0..300 {
                let a = assign(&mut st, &m, &lib, crate::voice::VoiceSplit::NoneHalf, &[1; 32], &s, kind, &format!("d{i}"), &Avoid::default(), &|_| None).unwrap();
                let t = match kind {
                    DocKind::Mixed => rewrite(),
                    DocKind::ModelOnly => GenTarget::Scratch { domain: "wikipedia", topic: "a river", target_words: a.scratch_words.unwrap() },
                };
                let r = build(&m, &lib, &a, &t).unwrap();
                assert!(!r.body.contains("TEST voice") || a.voice != VoiceChoice::None);
            }
        }
    }
}
