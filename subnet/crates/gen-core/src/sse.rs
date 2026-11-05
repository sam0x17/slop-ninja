//! The bounded stream reader of both vendors' SSE event grammars.
//!
//! The reader holds two buffers, both sized when it is made: the partial
//! line (at most the request's `line_bound`) and the visible text (at most
//! its `visible_bound`, `stream_bound_v1`). It never grows either: a line
//! longer than its bound is a transport fault ([`StreamFault::LineBytes`]),
//! and visible text past its bound cannot be accepted (it is over the word
//! window), so reading stops with [`StreamFault::LengthBytes`], a content
//! outcome. Thinking and reasoning deltas are counted and dropped, never
//! buffered: their text fields are not even deserialized.
//!
//! Anthropic (`/v1/messages`, `stream: true`): `message_start` (model,
//! input usage), `content_block_delta` (`text_delta`, `thinking_delta`,
//! `signature_delta`), `message_delta` (`stop_reason`, `stop_details`, output
//! usage), `message_stop`, `ping`, `error`.
//! OpenAI (`/v1/responses`, `stream: true`): `response.created` /
//! `response.in_progress` (model), `response.output_text.delta`,
//! `response.refusal.delta`, `response.reasoning_*`, and the terminal
//! `response.completed`, `response.incomplete`, `response.failed`, `error`.

use crate::spend::Usage;
use crate::vendor::{Tier, Vendor};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// How a stream ended, content-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    /// `end_turn` / `response.completed`.
    Completed,
    /// The output token limit (`max_tokens`, incomplete `max_output_tokens`).
    MaxTokens,
    /// A safety refusal (Anthropic `stop_reason: "refusal"`, an OpenAI
    /// refusal part).
    Refusal,
    /// OpenAI incomplete `content_filter`.
    ContentFilter,
    /// Another incomplete reason or stop reason.
    Incomplete,
    /// OpenAI `response.failed`.
    Failed,
    /// A stream `error` event.
    StreamError(StreamErrorKind),
    /// The stream ended without a terminal event.
    Truncated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamErrorKind {
    Overloaded,
    RateLimit,
    Api,
    Other,
}

/// A refusal category (Anthropic `stop_details.category`), mapped to a fixed
/// set: no vendor text is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCategory {
    Cyber,
    Bio,
    FrontierLlm,
    ReasoningExtraction,
    GeneralHarms,
    Other,
    Unstated,
}

impl RefusalCategory {
    fn of(s: Option<&str>) -> Self {
        match s {
            None => RefusalCategory::Unstated,
            Some("cyber") => RefusalCategory::Cyber,
            Some("bio") => RefusalCategory::Bio,
            Some("frontier_llm") => RefusalCategory::FrontierLlm,
            Some("reasoning_extraction") => RefusalCategory::ReasoningExtraction,
            Some("general_harms") => RefusalCategory::GeneralHarms,
            Some(_) => RefusalCategory::Other,
        }
    }
}

/// Why reading stopped early.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamFault {
    /// One SSE line past the line bound: a transport fault.
    LineBytes,
    /// Visible text past the visible bound: a content outcome.
    LengthBytes,
    /// A data line that is not the vendor's JSON: a transport fault.
    Malformed,
}

/// The longest reported model id kept; a longer one is recorded as absent.
pub const MODEL_ID_BYTES: usize = 128;

/// What a stream produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamEnd {
    pub visible: String,
    pub model_reported: Option<String>,
    pub usage: Usage,
    /// Whether the vendor reported the final usage (OpenAI's terminal
    /// response, Anthropic's `message_delta`); a stream that broke before it
    /// has not, whatever partial counts it carried.
    pub usage_reported: bool,
    pub stop: Stop,
    pub refusal: Option<RefusalCategory>,
    pub tier_reported: Option<Tier>,
    pub thinking_events: u32,
    pub fault: Option<StreamFault>,
    /// The longest line less the visible text it carried.
    pub envelope_bytes_max: u32,
}

/// The incremental reader.
pub struct SseReader {
    vendor: Vendor,
    line: Vec<u8>,
    line_bound: usize,
    visible: String,
    visible_bound: usize,
    end: StreamEnd,
    stopped: bool,
}

/// Progress after one chunk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// Visible text arrived in this chunk for the first time.
    pub first_text: bool,
    /// The stream reached a terminal event or a fault: read no further.
    pub done: bool,
}

#[derive(Deserialize)]
struct AUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct AMessage<'a> {
    #[serde(borrow, default)]
    model: Option<Cow<'a, str>>,
    #[serde(default)]
    usage: Option<AUsage>,
}

#[derive(Deserialize)]
struct AStopDetails<'a> {
    #[serde(borrow, default)]
    category: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct ADelta<'a> {
    #[serde(rename = "type", borrow, default)]
    ty: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    text: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    stop_reason: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    stop_details: Option<AStopDetails<'a>>,
}

#[derive(Deserialize)]
struct AErr<'a> {
    #[serde(rename = "type", borrow, default)]
    ty: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct AEvent<'a> {
    #[serde(rename = "type", borrow)]
    ty: Cow<'a, str>,
    #[serde(borrow, default)]
    message: Option<AMessage<'a>>,
    #[serde(borrow, default)]
    delta: Option<ADelta<'a>>,
    #[serde(default)]
    usage: Option<AUsage>,
    #[serde(borrow, default)]
    error: Option<AErr<'a>>,
}

#[derive(Deserialize)]
struct OInDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct OOutDetails {
    #[serde(default)]
    reasoning_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct OUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    input_tokens_details: Option<OInDetails>,
    #[serde(default)]
    output_tokens_details: Option<OOutDetails>,
}

#[derive(Deserialize)]
struct OIncomplete<'a> {
    #[serde(borrow, default)]
    reason: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct OResponse<'a> {
    #[serde(borrow, default)]
    model: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    service_tier: Option<Cow<'a, str>>,
    #[serde(default)]
    usage: Option<OUsage>,
    #[serde(borrow, default)]
    incomplete_details: Option<OIncomplete<'a>>,
}

#[derive(Deserialize)]
struct OEvent<'a> {
    #[serde(rename = "type", borrow)]
    ty: Cow<'a, str>,
    #[serde(borrow, default)]
    delta: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    response: Option<OResponse<'a>>,
    #[serde(borrow, default)]
    code: Option<Cow<'a, str>>,
}

impl SseReader {
    /// The bytes the reader's two buffers take, to admit before [`new`](Self::new).
    pub fn bytes(visible_bound: u32, line_bound: u32) -> usize {
        visible_bound as usize + line_bound as usize
    }

    pub fn new(vendor: Vendor, visible_bound: u32, line_bound: u32) -> Self {
        SseReader {
            vendor,
            line: Vec::with_capacity(line_bound as usize),
            line_bound: line_bound as usize,
            visible: String::with_capacity(visible_bound as usize),
            visible_bound: visible_bound as usize,
            end: StreamEnd {
                visible: String::new(),
                model_reported: None,
                usage: Usage::default(),
                usage_reported: false,
                stop: Stop::Truncated,
                refusal: None,
                tier_reported: None,
                thinking_events: 0,
                fault: None,
                envelope_bytes_max: 0,
            },
            stopped: false,
        }
    }

    /// Feeds one chunk of the body, of any length and split anywhere.
    pub fn feed(&mut self, chunk: &[u8]) -> Progress {
        let mut p = Progress::default();
        if self.stopped {
            p.done = true;
            return p;
        }
        let had_text = !self.visible.is_empty();
        let mut rest = chunk;
        while !rest.is_empty() && !self.stopped {
            let (seg, tail, eol) = match rest.iter().position(|b| *b == b'\n') {
                Some(i) => (rest.get(..i).unwrap_or(&[]), rest.get(i + 1..).unwrap_or(&[]), true),
                None => (rest, &[][..], false),
            };
            if self.line.len() + seg.len() > self.line_bound {
                self.fault(StreamFault::LineBytes);
                break;
            }
            self.line.extend_from_slice(seg);
            rest = tail;
            if eol {
                let line = std::mem::take(&mut self.line);
                self.process(&line);
                // Reuse the allocation: clear and hand it back.
                let mut line = line;
                line.clear();
                self.line = line;
            }
        }
        p.first_text = !had_text && !self.visible.is_empty();
        p.done = self.stopped;
        p
    }

    /// Ends the stream (the body is exhausted) and returns what it produced.
    pub fn finish(mut self) -> StreamEnd {
        if !self.stopped && !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            self.process(&line);
        }
        let mut end = self.end;
        end.visible = self.visible;
        end
    }

    fn fault(&mut self, f: StreamFault) {
        self.end.fault = Some(f);
        self.stopped = true;
    }

    fn terminal(&mut self, s: Stop) {
        self.end.stop = s;
        self.stopped = true;
    }

    fn envelope(&mut self, line_len: usize, carried: usize) {
        let e = line_len.saturating_sub(carried) as u32;
        self.end.envelope_bytes_max = self.end.envelope_bytes_max.max(e);
    }

    fn push_text(&mut self, t: &str) {
        if self.visible.len() + t.len() > self.visible_bound {
            self.fault(StreamFault::LengthBytes);
            return;
        }
        self.visible.push_str(t);
    }

    fn model(&mut self, m: Option<Cow<'_, str>>) {
        if let Some(m) = m
            && self.end.model_reported.is_none()
            && m.len() <= MODEL_ID_BYTES
        {
            self.end.model_reported = Some(m.into_owned());
        }
    }

    fn process(&mut self, raw: &[u8]) {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        let Some(data) = line.strip_prefix(b"data:") else {
            // `event:`, `id:`, `retry:`, comments and blank lines.
            return;
        };
        let data = data.strip_prefix(b" ").unwrap_or(data);
        if data == b"[DONE]" {
            return;
        }
        match self.vendor {
            Vendor::Anthropic => self.anthropic(line.len(), data),
            Vendor::Openai => self.openai(line.len(), data),
            Vendor::WorkersAi => self.fault(StreamFault::Malformed),
        }
    }

    fn anthropic(&mut self, line_len: usize, data: &[u8]) {
        let Ok(ev) = serde_json::from_slice::<AEvent<'_>>(data) else {
            return self.fault(StreamFault::Malformed);
        };
        // `message_start` carries the input usage and a placeholder output
        // count; only `message_delta` carries the final output count, so
        // only it marks the usage reported (a stream cut before it is
        // charged its reservation bound).
        let usage = |s: &mut Self, u: &AUsage| {
            if let Some(i) = u.input_tokens {
                s.end.usage.input_tokens = i + u.cache_creation_input_tokens.unwrap_or(0);
            }
            if let Some(c) = u.cache_read_input_tokens {
                s.end.usage.cached_input_tokens = c;
            }
            if let Some(o) = u.output_tokens {
                s.end.usage.output_tokens = o;
            }
        };
        match ev.ty.as_ref() {
            "message_start" => {
                if let Some(m) = ev.message {
                    self.model(m.model);
                    if let Some(u) = &m.usage {
                        usage(self, u);
                    }
                }
                self.envelope(line_len, 0);
            }
            "content_block_delta" => {
                let d = ev.delta;
                match d.as_ref().and_then(|d| d.ty.as_deref()) {
                    Some("text_delta") => {
                        let t = d.as_ref().and_then(|d| d.text.as_deref()).unwrap_or("");
                        self.envelope(line_len, t.len());
                        self.push_text(t);
                    }
                    Some("thinking_delta") | Some("signature_delta") => self.end.thinking_events += 1,
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(u) = &ev.usage {
                    usage(self, u);
                    self.end.usage_reported = true;
                }
                self.envelope(line_len, 0);
                if let Some(d) = ev.delta {
                    match d.stop_reason.as_deref() {
                        Some("end_turn") | Some("stop_sequence") => self.end.stop = Stop::Completed,
                        Some("max_tokens") => self.end.stop = Stop::MaxTokens,
                        Some("refusal") => {
                            self.end.stop = Stop::Refusal;
                            self.end.refusal = Some(RefusalCategory::of(d.stop_details.as_ref().and_then(|s| s.category.as_deref())));
                        }
                        Some(_) => self.end.stop = Stop::Incomplete,
                        None => {}
                    }
                }
            }
            "message_stop" => {
                let s = if self.end.stop == Stop::Truncated { Stop::Incomplete } else { self.end.stop };
                self.terminal(s);
            }
            "error" => {
                let k = match ev.error.as_ref().and_then(|e| e.ty.as_deref()) {
                    Some("overloaded_error") => StreamErrorKind::Overloaded,
                    Some("rate_limit_error") => StreamErrorKind::RateLimit,
                    Some("api_error") => StreamErrorKind::Api,
                    _ => StreamErrorKind::Other,
                };
                self.terminal(Stop::StreamError(k));
            }
            _ => {}
        }
    }

    fn openai(&mut self, line_len: usize, data: &[u8]) {
        let Ok(ev) = serde_json::from_slice::<OEvent<'_>>(data) else {
            return self.fault(StreamFault::Malformed);
        };
        let ty = ev.ty.as_ref();
        if ty == "response.output_text.delta" {
            let t = ev.delta.as_deref().unwrap_or("");
            self.envelope(line_len, t.len());
            return self.push_text(t);
        }
        if ty.starts_with("response.reasoning") {
            self.end.thinking_events += 1;
            return;
        }
        if ty == "response.refusal.delta" {
            // The refusal text is never kept.
            self.end.refusal = Some(RefusalCategory::Unstated);
            return;
        }
        if ty == "error" {
            let k = match ev.code.as_deref() {
                Some("rate_limit_exceeded") => StreamErrorKind::RateLimit,
                Some("server_error") => StreamErrorKind::Api,
                _ => StreamErrorKind::Other,
            };
            return self.terminal(Stop::StreamError(k));
        }
        let Some(r) = ev.response else { return };
        self.model(r.model);
        if let Some(t) = r.service_tier.as_deref() {
            self.end.tier_reported = match t {
                "flex" => Some(Tier::Flex),
                "default" | "auto" | "standard" => Some(Tier::Standard),
                "batch" => Some(Tier::Batch),
                _ => self.end.tier_reported,
            };
        }
        if let Some(u) = r.usage {
            self.end.usage_reported = true;
            let input = u.input_tokens.unwrap_or(0);
            let cached = u.input_tokens_details.and_then(|d| d.cached_tokens).unwrap_or(0).min(input);
            self.end.usage.input_tokens = input - cached;
            self.end.usage.cached_input_tokens = cached;
            self.end.usage.output_tokens = u.output_tokens.unwrap_or(0);
            self.end.usage.reasoning_tokens = u.output_tokens_details.and_then(|d| d.reasoning_tokens);
        }
        // The terminal events echo the output text.
        let carried = self.visible.len();
        match ty {
            "response.completed" => {
                self.envelope(line_len, carried);
                let s = if self.end.refusal.is_some() { Stop::Refusal } else { Stop::Completed };
                self.terminal(s);
            }
            "response.incomplete" => {
                self.envelope(line_len, carried);
                let s = match r.incomplete_details.as_ref().and_then(|d| d.reason.as_deref()) {
                    Some("max_output_tokens") => Stop::MaxTokens,
                    Some("content_filter") => Stop::ContentFilter,
                    _ => Stop::Incomplete,
                };
                self.terminal(s);
            }
            "response.failed" => self.terminal(Stop::Failed),
            _ => self.envelope(line_len, 0),
        }
    }
}

/// Synthetic event streams in each vendor's grammar, for tests.
#[cfg(any(test, feature = "fixtures"))]
pub mod fixture {
    /// An Anthropic stream of `text` in `parts` deltas, with thinking first.
    pub fn anthropic(model: &str, text: &str, stop: &str, category: Option<&str>) -> String {
        let mut s = format!(
            "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg_x\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"{model}\",\"content\":[],\"usage\":{{\"input_tokens\":120,\"cache_read_input_tokens\":0,\"output_tokens\":1}}}}}}\n\n"
        );
        s.push_str("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n");
        s.push_str("event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"\"}}\n\n");
        s.push_str("event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig\"}}\n\n");
        s.push_str("event: ping\ndata: {\"type\":\"ping\"}\n\n");
        for w in text.split_inclusive(' ') {
            let j = serde_json::to_string(w).unwrap_or_default();
            s.push_str(&format!("event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":1,\"delta\":{{\"type\":\"text_delta\",\"text\":{j}}}}}\n\n"));
        }
        let details = match category {
            Some(c) => format!(",\"stop_details\":{{\"type\":\"refusal\",\"category\":\"{c}\",\"explanation\":\"vendor text\"}}"),
            None => String::new(),
        };
        s.push_str(&format!(
            "event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"{stop}\"{details}}},\"usage\":{{\"output_tokens\":345}}}}\n\n"
        ));
        s.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        s
    }

    /// An OpenAI Responses stream of `text`, ending with `terminal`
    /// (`response.completed` or `response.incomplete` with `reason`).
    pub fn openai(model: &str, text: &str, terminal: &str, reason: Option<&str>, tier: &str) -> String {
        let mut s = format!(
            "event: response.created\ndata: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{\"id\":\"resp_x\",\"status\":\"in_progress\",\"model\":\"{model}\",\"service_tier\":\"{tier}\"}}}}\n\n"
        );
        s.push_str("event: response.reasoning_summary_part.added\ndata: {\"type\":\"response.reasoning_summary_part.added\",\"sequence_number\":1}\n\n");
        for w in text.split_inclusive(' ') {
            let j = serde_json::to_string(w).unwrap_or_default();
            s.push_str(&format!("event: response.output_text.delta\ndata: {{\"type\":\"response.output_text.delta\",\"item_id\":\"msg_x\",\"output_index\":1,\"content_index\":0,\"delta\":{j}}}\n\n"));
        }
        let full = serde_json::to_string(text).unwrap_or_default();
        let inc = match reason {
            Some(r) => format!(",\"incomplete_details\":{{\"reason\":\"{r}\"}}"),
            None => String::new(),
        };
        s.push_str(&format!(
            "event: {terminal}\ndata: {{\"type\":\"{terminal}\",\"response\":{{\"id\":\"resp_x\",\"status\":\"done\",\"model\":\"{model}\",\"service_tier\":\"{tier}\",\"output\":[{{\"type\":\"message\",\"content\":[{{\"type\":\"output_text\",\"text\":{full}}}]}}],\"usage\":{{\"input_tokens\":150,\"input_tokens_details\":{{\"cached_tokens\":20}},\"output_tokens\":900,\"output_tokens_details\":{{\"reasoning_tokens\":700}}}}{inc}}}}}\n\n"
        ));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    fn read(vendor: Vendor, body: &str, chunk: usize, visible: u32, line: u32) -> StreamEnd {
        let mut r = SseReader::new(vendor, visible, line);
        for c in body.as_bytes().chunks(chunk) {
            if r.feed(c).done {
                break;
            }
        }
        r.finish()
    }

    const TEXT: &str = "On Monday the council voted to repair the bridge.";

    #[test]
    fn anthropic_streams_read_the_same_in_any_chunking() {
        let body = anthropic("claude-opus-5-5", TEXT, "end_turn", None);
        let whole = read(Vendor::Anthropic, &body, body.len(), 4096, 4096);
        assert_eq!(whole.visible, TEXT);
        assert_eq!(whole.stop, Stop::Completed);
        assert_eq!(whole.model_reported.as_deref(), Some("claude-opus-5-5"));
        assert_eq!((whole.usage.input_tokens, whole.usage.output_tokens), (120, 345));
        assert!(whole.usage_reported && whole.fault.is_none());
        assert_eq!(whole.thinking_events, 2);
        for chunk in [1, 2, 3, 7, 64, 333] {
            assert_eq!(read(Vendor::Anthropic, &body, chunk, 4096, 4096), whole, "chunk {chunk}");
        }
        // CRLF line ends read the same.
        assert_eq!(read(Vendor::Anthropic, &body.replace('\n', "\r\n"), 5, 4096, 4096).visible, TEXT);
    }

    #[test]
    fn refusals_carry_a_fixed_category_only() {
        let body = anthropic("claude-sonnet-5-5", "", "refusal", Some("cyber"));
        let e = read(Vendor::Anthropic, &body, 9, 4096, 4096);
        assert_eq!((e.stop, e.refusal), (Stop::Refusal, Some(RefusalCategory::Cyber)));
        let body = anthropic("claude-sonnet-5-5", "", "refusal", Some("something new"));
        assert_eq!(read(Vendor::Anthropic, &body, 9, 4096, 4096).refusal, Some(RefusalCategory::Other));
        let body = anthropic("claude-haiku-5-5", TEXT, "max_tokens", None);
        assert_eq!(read(Vendor::Anthropic, &body, 9, 4096, 4096).stop, Stop::MaxTokens);
    }

    #[test]
    fn openai_streams_read_usage_tier_and_terminals() {
        let body = openai("gpt-6.1-sol", TEXT, "response.completed", None, "flex");
        let e = read(Vendor::Openai, &body, 11, 4096, 4096);
        assert_eq!(e.visible, TEXT);
        assert_eq!(e.stop, Stop::Completed);
        assert_eq!(e.tier_reported, Some(Tier::Flex));
        assert_eq!((e.usage.input_tokens, e.usage.cached_input_tokens, e.usage.output_tokens, e.usage.reasoning_tokens), (130, 20, 900, Some(700)));
        assert_eq!(e.thinking_events, 1);
        let body = openai("gpt-6.1-sol", TEXT, "response.incomplete", Some("max_output_tokens"), "default");
        let e = read(Vendor::Openai, &body, 11, 4096, 4096);
        assert_eq!((e.stop, e.tier_reported), (Stop::MaxTokens, Some(Tier::Standard)));
        let body = openai("gpt-6.1-sol", TEXT, "response.incomplete", Some("content_filter"), "flex");
        assert_eq!(read(Vendor::Openai, &body, 11, 4096, 4096).stop, Stop::ContentFilter);
        let err = "event: error\ndata: {\"type\":\"error\",\"code\":\"server_error\",\"message\":\"secret-ish\"}\n\n";
        assert_eq!(read(Vendor::Openai, err, 4, 4096, 4096).stop, Stop::StreamError(StreamErrorKind::Api));
        let err = "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        assert_eq!(read(Vendor::Anthropic, err, 4, 4096, 4096).stop, Stop::StreamError(StreamErrorKind::Overloaded));
    }

    #[test]
    fn bounds_stop_reading_without_growing() {
        // Visible text past its bound: a content outcome, reading stops.
        let body = anthropic("claude-opus-5-5", TEXT, "end_turn", None);
        let e = read(Vendor::Anthropic, &body, 13, 20, 4096);
        assert_eq!(e.fault, Some(StreamFault::LengthBytes));
        assert!(e.visible.len() <= 20);
        // A line past its bound: a transport fault, before anything grows.
        let e = read(Vendor::Anthropic, &body, 13, 4096, 100);
        assert_eq!(e.fault, Some(StreamFault::LineBytes));
        // An OpenAI terminal event echoes the text: it needs visible + envelope.
        let body = openai("gpt-6-luna", TEXT, "response.completed", None, "flex");
        let e = read(Vendor::Openai, &body, 13, TEXT.len() as u32, 4096);
        assert_eq!((e.fault, e.stop), (None, Stop::Completed));
        assert!(e.envelope_bytes_max > 100 && (e.envelope_bytes_max as usize) < 4096);
        // Capacities never grew past what was admitted.
        let mut r = SseReader::new(Vendor::Openai, 64, 512);
        r.feed(body.as_bytes());
        assert!(r.line.capacity() <= 512 && r.visible.capacity() <= 64);
    }

    #[test]
    fn truncated_and_malformed_streams() {
        let body = anthropic("claude-opus-5-5", TEXT, "end_turn", None);
        let cut = &body[..body.find("message_delta").unwrap()];
        let e = read(Vendor::Anthropic, cut, 17, 4096, 4096);
        assert_eq!(e.stop, Stop::Truncated);
        assert!(!e.usage_reported, "message_start's placeholder output count is not the final usage");
        assert_eq!(e.usage.input_tokens, 120, "its input usage is kept");
        assert_eq!(read(Vendor::Openai, "data: {not json\n", 3, 4096, 4096).fault, Some(StreamFault::Malformed));
        assert!(read(Vendor::Openai, ": keepalive\n\n", 3, 4096, 4096).fault.is_none());
    }
}
