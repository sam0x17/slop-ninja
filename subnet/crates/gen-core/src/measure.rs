//! The measurement run's compiled synthetic set (design section 12.3).
//!
//! Before a manifest can be `measured`, every (model, effort) cell is called
//! [`CALLS_PER_CELL`] times on synthetic text only (`measurement_set_v2`):
//! 10 rewrites (5 light, 5 heavy, voices covering every category, `none`
//! and match-surrounding included) of spans of [`long_document`], a
//! synthetic document near the scorer's token limit, at every span class
//! of the manifest (each class at both intensities, so the widest window a
//! cohort item can have is measured), and 10 scratch documents over every
//! scratch length (the largest included). The text below was written for
//! this set; no pool, held-out, CAL or Dev-select text enters it. A
//! measurement route accepts only a cell, a call index and an attempt
//! index, so it cannot carry arbitrary text: the request is built here.
//!
//! Only content outcomes ([`OutcomeCode::uses_attempt`]) are measurements:
//! a throttle, outage or other non-content answer is retried by the
//! operator's script and never enters a cell's figures.

use crate::manifest::{Intensity, Manifest, Measured, Refused, Register, Status, Unusable};
use crate::outcome::OutcomeCode;
use crate::request::{GenTarget, VendorRequest, build};
use crate::rotation::{GenAssignment, Positions, template_for};
use crate::spend::Usage;
use crate::vendor::{DocKind, Effort};
use crate::voice::{Category, Library, VoiceChoice};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const CALLS_PER_CELL: u32 = 20;
/// The set's version (bound into the measurement report by its digest).
pub const SET_VERSION: &str = "measurement_set_v2";
/// Paragraphs of [`long_document`]: the ten synthetic paragraphs in rounds,
/// as many as keep the document within the scorer's token limit
/// (`iface::DOC_TOKENS`) and above nine tenths of it (the coordinator's
/// tests count it with the pinned scorer tokenizer).
pub const LONG_PARAGRAPHS: usize = 37;
/// The pseudo-domain of the measurement prompts (its register is compiled in).
pub const DOMAIN: &str = "synthetic_measurement";
const REGISTER: &str = "a piece of plain English prose written for a test";

/// Synthetic rewrite sources: (before, passage, after).
pub const SOURCES: [(&str, &str, &str); 10] = [
    ("The harbour town of Elsmere sits at the mouth of a slow brown river.", "Every spring the ferry company repaints its two boats, and the work takes about three weeks. Volunteers from the rowing club help with the sanding.", "By May the boats are back in service."),
    ("The library opened a small repair desk last year.", "Visitors bring broken lamps, torn coats and the occasional radio, and a rota of retired engineers tries to fix them. Most repairs take less than an hour.", "The desk is open on Saturday mornings."),
    ("A new footpath now links the two villages.", "It follows an old drainage ditch for most of its length and crosses the stream on a timber bridge. The council expects walkers and cyclists to share it.", "Signs at each end show the distance."),
    ("The bakery on Mill Lane changed hands in the autumn.", "The new owners kept the old oven but replaced the counter, and they now sell soup at lunchtime. Queues have grown since the change.", "Bread still sells out by noon."),
    ("Researchers counted moths in the park over one summer.", "They set traps on twelve nights and recorded more than forty species, several of them new to the county list. Warm, cloudy nights gave the largest catches.", "The results will be shared at a public talk."),
    ("The school choir travelled to the regional festival.", "They sang three pieces, one of them written by a former pupil, and placed second in their class. The judges praised their clear diction.", "The choir rehearses twice a week."),
    ("A storm brought down an old beech tree near the station.", "Crews cleared the road within a few hours, and the timber was offered to local woodworkers. Several benches made from it now stand in the square.", "A young beech has been planted in its place."),
    ("The town museum has a new display case.", "It holds tools from the old rope works, including a spinning wheel and a set of wooden gauges. Labels explain how each tool was used.", "Entry to the museum is free."),
    ("Allotment holders voted on a shared water tank.", "The proposal passed by a narrow margin after a long meeting, and the tank will be installed before summer. Each plot will pay a small fee.", "A rota will manage the filling."),
    ("The cinema club screened a silent film last month.", "A pianist improvised the score from a few notes, and the audience stayed for a discussion afterwards. Attendance was the highest of the season.", "Another silent film is planned for spring."),
];

/// Synthetic scratch topics.
pub const TOPICS: [&str; 10] = [
    "repainting a small ferry",
    "running a community repair desk",
    "a new footpath between two villages",
    "a bakery that changed owners",
    "counting moths in a park",
    "a school choir at a festival",
    "a fallen tree turned into benches",
    "tools from an old rope works",
    "a shared water tank for allotments",
    "a silent film night",
];

/// The voices of the ten rewrite calls and the ten scratch calls (`None`:
/// no profile): every category appears, `none` and match-surrounding too.
const REWRITE_VOICES: [Option<Category>; 10] = [
    None,
    Some(Category::Persona),
    Some(Category::Register),
    Some(Category::Tone),
    Some(Category::Era),
    Some(Category::MatchSurrounding),
    Some(Category::Evasion),
    None,
    Some(Category::Evasion),
    Some(Category::MatchSurrounding),
];
const SCRATCH_VOICES: [Option<Category>; 10] = [
    None,
    Some(Category::Persona),
    Some(Category::Register),
    Some(Category::Tone),
    Some(Category::Era),
    Some(Category::Evasion),
    None,
    Some(Category::Persona),
    Some(Category::Evasion),
    Some(Category::Tone),
];

/// The manifest with the measurement register added (requests only).
fn with_register(m: &Manifest) -> Manifest {
    let mut m = m.clone();
    if m.register(DOMAIN).is_none() {
        m.registers.push(Register { domain: DOMAIN.into(), register: REGISTER.into(), topics: TOPICS.iter().map(|t| (*t).to_owned()).collect() });
    }
    m
}

fn voice(lib: &Library, kind: DocKind, cat: Option<Category>, call: u32) -> VoiceChoice {
    let Some(c) = cat else { return VoiceChoice::None };
    let pool: Vec<&str> = lib.entries.iter().filter(|e| e.category == c && e.kinds.contains(&kind)).map(|e| e.id.as_str()).collect();
    match pool.get(call as usize % pool.len().max(1)) {
        Some(id) => VoiceChoice::Profile { category: c, id: (*id).to_owned() },
        None => VoiceChoice::None,
    }
}

/// The synthetic long document of the rewrite calls: the ten paragraphs
/// (`before passage after`) in rounds, each round starting three paragraphs
/// later, [`LONG_PARAGRAPHS`] in all, one blank line apart.
pub fn long_document() -> String {
    let paras: Vec<String> = SOURCES.iter().map(|(b, p, a)| format!("{b} {p} {a}")).collect();
    let n = paras.len();
    (0..LONG_PARAGRAPHS)
        .filter_map(|i| paras.get((i % n + 3 * (i / n)) % n).cloned())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The sentence ranges of `doc` (each ends after `.` and before the next
/// whitespace).
fn sentences(doc: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    let b = doc.as_bytes();
    for (i, c) in b.iter().enumerate() {
        let end = i + 1;
        if *c == b'.' && b.get(end).is_none_or(|x| x.is_ascii_whitespace()) {
            out.push(start..end);
            start = b.get(end..).map_or(end, |rest| end + rest.iter().take_while(|x| x.is_ascii_whitespace()).count());
        }
    }
    out
}

/// The span of rewrite call `k` covering `pct` percent of `doc`'s words
/// (whole sentences, at least one, rounded half up as the draw does),
/// starting at a sentence that varies with `k` and leaves room for it.
fn span_of(doc: &str, pct: u32, k: usize) -> Option<std::ops::Range<usize>> {
    let sents = sentences(doc);
    let words: Vec<usize> = sents.iter().map(|r| doc.get(r.clone()).map_or(0, crate::request::words)).collect();
    let total: usize = words.iter().sum();
    let want = (total * pct as usize).div_ceil(100).max(1);
    // The fewest sentences from the end that reach `want`: the latest start.
    let mut acc = 0;
    let mut latest = sents.len();
    while latest > 0 && acc < want {
        latest -= 1;
        acc += words.get(latest).copied().unwrap_or(0);
    }
    let first = (k * 7) % (latest + 1);
    let mut acc = 0;
    let mut last = first;
    while last < sents.len() && acc < want {
        acc += words.get(last).copied().unwrap_or(0);
        last += 1;
    }
    Some(sents.get(first)?.start..sents.get(last.checked_sub(1)?)?.end)
}

/// The request of measurement call `call` (0..20) of cell (`model`, `effort`).
pub fn request(m: &Manifest, lib: &Library, model: &str, effort: Effort, call: u32) -> Result<(VendorRequest, String), Refused> {
    if call >= CALLS_PER_CELL {
        return Err(Refused("measure (call index)"));
    }
    let entry = m.model(model).ok_or(Refused("measure (model)"))?;
    if !entry.effort_levels.contains(&effort) {
        return Err(Refused("measure (effort)"));
    }
    let mm = with_register(m);
    let k = (call % 10) as usize;
    let rewrite = call < 10;
    let kind = if rewrite { DocKind::Mixed } else { DocKind::ModelOnly };
    let cat = if rewrite { REWRITE_VOICES.get(k).copied().flatten() } else { SCRATCH_VOICES.get(k).copied().flatten() };
    let v = voice(lib, kind, cat, call);
    let intensity = if k < 5 { Intensity::Light } else { Intensity::Heavy };
    let ops = m.operations.of(intensity);
    let spans = &m.span_classes_percent;
    let span_percent = if rewrite { Some(spans.get(k % spans.len().max(1)).copied().ok_or(Refused("measure (span classes)"))?) } else { None };
    let a = GenAssignment {
        rotation_version: "measurement".into(),
        manifest_version: m.version,
        stream: format!("measurement/{model}/{}", effort.code()),
        kind,
        lane: entry.lane,
        model: model.into(),
        effort,
        span_percent,
        intensity: rewrite.then_some(intensity),
        operation: if rewrite { ops.get(k % ops.len().max(1)).map(|o| o.id.clone()) } else { None },
        scratch_words: target_words(m, call),
        template: template_for(kind, &v).into(),
        voice: v,
        voice_library_version: lib.header.version,
        positions: Positions::default(),
        fallback: None,
    };
    if rewrite {
        let doc = long_document();
        let span = span_of(&doc, span_percent.unwrap_or(40), k).ok_or(Refused("measure (span)"))?;
        let passage = doc.get(span.clone()).ok_or(Refused("measure (span)"))?.to_owned();
        let before = 0..doc.get(..span.start).map_or(0, |b| b.trim_end().len());
        let after = doc.get(span.end..).map_or(doc.len(), |a| doc.len() - a.trim_start().len())..doc.len();
        let t = GenTarget::Rewrite { domain: DOMAIN, parent: &doc, span, context: Some((before, after)) };
        Ok((build(&mm, lib, &a, &t)?, passage))
    } else {
        let topic = TOPICS.get(k).copied().unwrap_or("a walk");
        Ok((build(&mm, lib, &a, &GenTarget::Scratch { domain: DOMAIN, topic, target_words: a.scratch_words.unwrap_or(200) })?, String::new()))
    }
}

/// The target length of scratch call `call` (10..20), `None` for a rewrite
/// call (its window is the passage's).
pub fn target_words(m: &Manifest, call: u32) -> Option<u32> {
    (10..CALLS_PER_CELL).contains(&call).then(|| m.scratch_words.get((call % 10) as usize % m.scratch_words.len().max(1)).copied().unwrap_or(200))
}

/// Every measurement call of a manifest, in order: (model, effort, call).
pub fn calls(m: &Manifest) -> Vec<(String, Effort, u32)> {
    m.models.iter().flat_map(|x| x.effort_levels.iter().flat_map(move |e| (0..CALLS_PER_CELL).map(move |c| (x.id.clone(), *e, c)))).collect()
}

/// One measured call (content-free).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub model: String,
    pub effort: Effort,
    pub call: u32,
    pub outcome: OutcomeCode,
    /// The validator accepted the answer.
    pub accepted: bool,
    pub usage: Usage,
    pub latency_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    pub envelope_bytes_max: u32,
    /// The call's validator window maximum (words).
    pub window_words: u32,
    /// The call ended without the vendor's final usage and was charged its
    /// reservation bound ([`crate::spend::charge`]).
    #[serde(default)]
    pub unmeasured: bool,
}

/// Per cell (`model/effort`), the measured figures of its observations.
/// Only content outcomes count: anything else is not a measurement.
pub fn summarize(obs: &[Observation]) -> BTreeMap<(String, Effort), Measured> {
    let mut out: BTreeMap<(String, Effort), Measured> = BTreeMap::new();
    for o in obs.iter().filter(|o| o.outcome.uses_attempt()) {
        let x = out.entry((o.model.clone(), o.effort)).or_default();
        x.calls += 1;
        x.accepted += u32::from(o.accepted);
        x.refusals += u32::from(o.outcome == OutcomeCode::Refusal);
        x.input_tokens_max = x.input_tokens_max.max(o.usage.input_tokens + o.usage.cached_input_tokens);
        x.output_tokens_max = x.output_tokens_max.max(o.usage.output_tokens);
        x.reasoning_tokens_max = x.reasoning_tokens_max.max(o.usage.reasoning_tokens.unwrap_or(0));
        x.latency_ms_max = x.latency_ms_max.max(o.latency_ms);
        x.first_token_ms_max = x.first_token_ms_max.max(o.first_token_ms.unwrap_or(0));
        x.envelope_bytes_max = x.envelope_bytes_max.max(o.envelope_bytes_max);
        x.window_words_max = x.window_words_max.max(o.window_words);
    }
    out
}

/// The measured manifest of `planned`: every cell's figures, the envelope
/// bound (twice the largest measured envelope, rounded up to a power of two),
/// unusable cells marked with their reason (recorded, never dropped
/// silently): no accepted answer, or (frontier) a slowest call past the
/// vendor deadline ([`crate::iface::vendor_deadline_ms`]) of
/// `call_timeout_ceiling_ms`, the longest call timeout any frontier lease
/// the lane bounds allow ([`crate::plan::LaneBounds::call_timeout_ceiling_ms`]);
/// the report digest, and the new version.
pub fn apply(planned: &Manifest, summary: &BTreeMap<(String, Effort), Measured>, report_sha256: &str, version: u32, call_timeout_ceiling_ms: u64) -> Result<Manifest, Refused> {
    let mut m = planned.clone();
    let mut envelope = 0u32;
    for x in &mut m.models {
        let mut cells = BTreeMap::new();
        let mut unusable = Vec::new();
        let mut reasons = BTreeMap::new();
        for e in &x.effort_levels {
            let s = summary.get(&(x.id.clone(), *e)).copied().ok_or(Refused("measure (a cell without content observations)"))?;
            let why = if s.accepted == 0 {
                Some(Unusable::NoAcceptedAnswer)
            } else if x.lane == crate::vendor::Lane::Frontier && s.latency_ms_max > crate::iface::vendor_deadline_ms(call_timeout_ceiling_ms) {
                Some(Unusable::SlowerThanLeaseCeiling)
            } else {
                None
            };
            if let Some(w) = why {
                unusable.push(*e);
                reasons.insert(e.code().to_owned(), w);
            }
            envelope = envelope.max(s.envelope_bytes_max);
            cells.insert(e.code().to_owned(), s);
        }
        x.measured = Some(cells);
        x.unusable = unusable;
        x.unusable_reasons = reasons;
    }
    m.stream_bound.envelope_bytes = envelope.max(1).saturating_mul(2).next_power_of_two();
    m.stream_bound.envelope_measured = true;
    m.status = Status::Measured;
    m.measurement_report_sha256 = Some(report_sha256.to_owned());
    m.version = version;
    m.validate()?;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::fixture::planned;
    use crate::voice::fixture::library;

    #[test]
    fn the_set_covers_every_cell_and_category_with_synthetic_text_only() {
        let m = planned();
        let lib = library();
        let all = calls(&m);
        assert_eq!(all.len(), 66 * 20);
        let mut cats = std::collections::BTreeSet::new();
        let mut none = 0;
        for (model, effort, call) in all.iter().filter(|(m, _, _)| m == "claude-sonnet-5-5") {
            let (r, _) = request(&m, &lib, model, *effort, *call).unwrap();
            assert!(!r.body.is_empty());
            if *call < 10 {
                let p: serde_json::Value = serde_json::from_str(&r.body).unwrap();
                let sys = p["system"].as_str().unwrap();
                match sys.strip_prefix("TEST voice ") {
                    Some(rest) => {
                        cats.insert(rest.split(' ').next().unwrap().to_owned());
                    }
                    None => none += 1,
                }
            }
        }
        assert!(cats.contains("match_surrounding") && cats.contains("evasion") && cats.len() == 6 && none > 0, "{cats:?}");
        assert!(request(&m, &lib, "gpt-6.1-sol", Effort::None, 0).is_err());
        assert!(request(&m, &lib, "gpt-6-luna", Effort::None, 20).is_err());
        // Deterministic: the same call is the same request.
        assert_eq!(request(&m, &lib, "gpt-6-luna", Effort::Max, 7).unwrap(), request(&m, &lib, "gpt-6-luna", Effort::Max, 7).unwrap());
    }

    #[test]
    fn measurements_make_a_measured_manifest() {
        let m = planned();
        let obs: Vec<Observation> = calls(&m)
            .into_iter()
            .map(|(model, effort, call)| Observation {
                accepted: !(model == "gpt-6-astra" && effort == Effort::Max),
                outcome: if call == 3 { OutcomeCode::Refusal } else { OutcomeCode::Answer },
                usage: Usage { input_tokens: 200, cached_input_tokens: 0, output_tokens: 300 + u64::from(call), reasoning_tokens: Some(100) },
                latency_ms: 1000 + u64::from(call),
                first_token_ms: Some(100),
                envelope_bytes_max: 700,
                window_words: 2_000,
                unmeasured: false,
                model,
                effort,
                call,
            })
            .collect();
        let s = summarize(&obs);
        let out = apply(&m, &s, &"ef".repeat(32), 2, 825_000).unwrap();
        assert_eq!(out.status, Status::Measured);
        assert_eq!(out.version, 2);
        assert_eq!(out.stream_bound.envelope_bytes, 2048);
        let astra = out.model("gpt-6-astra").unwrap();
        assert_eq!(astra.unusable, vec![Effort::Max]);
        assert_eq!(astra.unusable_reasons.get("max"), Some(&Unusable::NoAcceptedAnswer));
        assert_eq!(astra.measured(Effort::Low).unwrap().output_tokens_max, 319);
        assert_eq!(astra.measured(Effort::Low).unwrap().refusals, 1);
        // A missing cell is refused.
        let partial: Vec<Observation> = obs.iter().filter(|o| o.model != "gpt-6-luna").cloned().collect();
        assert!(apply(&m, &summarize(&partial), &"ef".repeat(32), 2, 825_000).is_err());
        // Non-content answers are not measurements: a cell measured only by
        // throttles and outages has no observations, and an outlier timeout
        // never sets a cell's latency.
        let mut noisy = obs.clone();
        for o in noisy.iter_mut().filter(|o| o.model == "gpt-6-luna" && o.effort == Effort::Low) {
            o.outcome = if o.call % 2 == 0 { OutcomeCode::Throttle } else { OutcomeCode::Outage };
        }
        assert!(apply(&m, &summarize(&noisy), &"ef".repeat(32), 2, 825_000).is_err());
        let mut slow = obs.clone();
        slow.push(Observation { outcome: OutcomeCode::Outage, latency_ms: 825_000, ..obs[0].clone() });
        assert_eq!(summarize(&slow), summarize(&obs));
        // A frontier cell slower than the lease ceiling is unusable, recorded.
        let mut late = obs.clone();
        for o in late.iter_mut().filter(|o| o.model == "claude-fable-5-1" && o.effort == Effort::Max) {
            o.latency_ms = 900_000;
        }
        let out = apply(&m, &summarize(&late), &"ef".repeat(32), 2, 825_000).unwrap();
        let fable = out.model("claude-fable-5-1").unwrap();
        assert_eq!(fable.unusable, vec![Effort::Max]);
        assert_eq!(fable.unusable_reasons.get("max"), Some(&Unusable::SlowerThanLeaseCeiling));
    }

    /// The measurement run on Workers AI, exactly as the eval Worker reads
    /// its replies (`outcome::workers_ai_reply`), gives a measured manifest
    /// whose reasoning cells keep room for reasoning.
    #[test]
    fn workers_ai_observations_keep_reasoning_headroom() {
        use crate::outcome::workers_ai_reply;
        let m = planned();
        let lib = library();
        let obs: Vec<Observation> = calls(&m)
            .into_iter()
            .map(|(model, effort, call)| {
                let (r, _) = request(&m, &lib, &model, effort, call).unwrap();
                // A reasoning model reports 600 completion tokens and no detail.
                let v = serde_json::json!({
                    "choices": [{"message": {"content": "Some synthetic answer text."}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 300, "completion_tokens": 600},
                });
                let g = workers_ai_reply(&v, &r, 1_000);
                Observation {
                    model,
                    effort,
                    call,
                    outcome: g.outcome,
                    accepted: true,
                    usage: g.usage,
                    latency_ms: g.latency_ms,
                    first_token_ms: g.first_token_ms,
                    envelope_bytes_max: g.envelope_bytes_max,
                    window_words: r.word_window.1,
                    unmeasured: false,
                }
            })
            .collect();
        let out = apply(&m, &summarize(&obs), &"ef".repeat(32), 2, 825_000).unwrap();
        let oss = out.model("@cf/openai/gpt-oss-120b").unwrap();
        assert_eq!(oss.measured(Effort::High).unwrap().reasoning_tokens_max, 0, "no detail reported");
        let (r, _) = request(&out, &lib, "@cf/openai/gpt-oss-120b", Effort::High, 0).unwrap();
        let visible = r.word_window.1 * crate::request::VISIBLE_TOKENS_PER_WORD;
        assert!(r.max_tokens >= visible.saturating_add(1200).min(oss.max_output_tokens), "{} {visible}", r.max_tokens);
        assert!(r.max_tokens > 96, "never the visible budget alone");
        // A cut reasoning answer is a length outcome, never an empty answer.
        let cut = serde_json::json!({"choices": [{"message": {"content": ""}, "finish_reason": "length"}], "usage": {"prompt_tokens": 1, "completion_tokens": 5}});
        assert_eq!(workers_ai_reply(&cut, &r, 1).outcome, OutcomeCode::LengthTokensOut);
        let full = serde_json::json!({"response": "x", "usage": {"prompt_tokens": 1, "completion_tokens": r.max_tokens}});
        assert_eq!(workers_ai_reply(&full, &r, 1).outcome, OutcomeCode::LengthTokensOut);
        let detail = serde_json::json!({"response": "Fine.", "usage": {"prompt_tokens": 1, "completion_tokens": 50, "completion_tokens_details": {"reasoning_tokens": 40}}});
        let g = workers_ai_reply(&detail, &r, 1);
        assert_eq!((g.outcome, g.usage.reasoning_tokens, g.usage_reported, g.sent), (OutcomeCode::Answer, Some(40), true, true));
    }

    #[test]
    fn the_rewrites_cover_every_span_class_of_the_long_document() {
        let m = planned();
        let lib = library();
        let doc = long_document();
        let total = crate::request::words(&doc);
        let mut seen = std::collections::BTreeSet::new();
        for call in 0..10 {
            let (r, passage) = request(&m, &lib, "gpt-6-luna", Effort::Low, call).unwrap();
            assert!(doc.contains(&passage));
            let pct = m.span_classes_percent[call as usize % 3];
            let n = crate::request::words(&passage);
            assert!(n * 100 >= total * pct as usize, "call {call}: {n} of {total} words for {pct}%");
            assert_eq!(r.word_window, crate::request::word_window(DocKind::Mixed, n as u32));
            seen.insert((pct, call < 5));
        }
        assert_eq!(seen.len(), 6, "each span class at both intensities");
        let largest = *m.scratch_words.iter().max().unwrap();
        assert!((10..20).any(|c| target_words(&m, c) == Some(largest)));
    }
}
