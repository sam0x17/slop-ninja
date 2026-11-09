//! The response allow-list: what of the scorer's reply reaches the browser.
//!
//! The scorer returns, per document, `text_sha256`, `output_sha256`,
//! `span_output_sha256`, timing and the full prediction (origin,
//! attribution, sentences). A text hash is a linkable identifier, and
//! attribution is not ready for a public page, so only named fields are
//! read here and only these leave: the band, the two percentages as the page
//! prints them, the word and token counts, the length label, notices, and
//! the spans when the band and the span switch allow them. Each span carries
//! UTF-16 offsets into the exact text the page sent, computed here from the
//! scorer's character offsets (and checked against its byte offsets), so
//! the page never splits a surrogate pair. Any inconsistency drops every
//! span; nothing is repaired.

use crate::policy::{self, Band, LengthBin};
use serde::{Deserialize, Serialize};

/// The fields read from the scorer's `/v1/score` reply; serde ignores the
/// rest, so nothing else can be forwarded by accident.
#[derive(Deserialize)]
struct Reply {
    package: String,
    results: Vec<Doc>,
}

#[derive(Deserialize)]
struct Doc {
    status: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    token_count: Option<u64>,
    #[serde(default)]
    prediction: Option<Prediction>,
    #[serde(default)]
    span_status: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Prediction {
    ai_confidence_percent: f64,
    ai_share_percent: f64,
    #[serde(default)]
    spans: Option<Vec<ScorerSpan>>,
}

#[derive(Deserialize, Clone, Copy)]
struct ScorerSpan {
    start_char: usize,
    end_char: usize,
    start_byte: usize,
    end_byte: usize,
    words: u64,
    ai_confidence_percent: f64,
}

/// One highlighted passage as the page receives it.
#[derive(Serialize, Debug, PartialEq)]
pub struct Span {
    /// UTF-16 offsets into the text the page sent.
    pub start: usize,
    pub end: usize,
    pub words: u64,
    /// "AI-like passage: 92%": whole percent, floored.
    pub confidence: u32,
}

/// Why no passage is drawn.
#[derive(Serialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum SpansNote {
    /// The document is not in a strong band.
    StrongBandsOnly,
    /// The operator turned spans off.
    Off,
    /// The scorer gave none, or they did not check out.
    Unavailable,
}

/// The package line.
#[derive(Serialize, Debug, PartialEq, Clone)]
pub struct PackageLine {
    /// The published package digest (parts-manifest SHA-256), shown by prefix.
    pub digest: String,
    pub publication_pending: bool,
}

/// The browser's result.
#[derive(Serialize, Debug, PartialEq)]
pub struct Answer {
    pub state: &'static str,
    pub band: &'static str,
    pub confidence_tenths: u16,
    pub confidence_text: String,
    pub share_text: String,
    pub words: usize,
    pub tokens: u64,
    pub length: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spans: Option<Vec<Span>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spans_note: Option<SpansNote>,
    pub markers: Vec<&'static str>,
    pub invisible: bool,
    pub package: PackageLine,
    pub receipt: String,
}

/// What the scorer's reply came to.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Scored { band: Band, confidence: f64, share: f64, tokens: u64, spans: Result<Vec<Span>, SpansNote> },
    /// The scorer declined the text (a fixed code for the page).
    Declined(&'static str),
    /// The reply is not what was asked for: never shown.
    Fault,
}

/// Reads a scorer reply for `package` (the load digest sent) about `text`.
pub fn read(reply: &[u8], package: &str, text: &str, spans_on: bool) -> Outcome {
    let Ok(r) = serde_json::from_slice::<Reply>(reply) else { return Outcome::Fault };
    if r.package != package || r.results.len() != 1 {
        return Outcome::Fault;
    }
    let Some(d) = r.results.into_iter().next() else { return Outcome::Fault };
    if d.status != "ok" {
        return match (d.status.as_str(), d.reason.as_deref()) {
            ("unsupported_input", Some("unsupported_length")) => Outcome::Declined("too_long"),
            ("unsupported_input", Some("empty_text")) => Outcome::Declined("empty"),
            ("unsupported_input", Some("no_word_tokens")) => Outcome::Declined("no_words"),
            _ => Outcome::Fault,
        };
    }
    let (Some(p), Some(tokens)) = (d.prediction, d.token_count) else { return Outcome::Fault };
    let Some(band) = Band::of_percent(p.ai_confidence_percent) else { return Outcome::Fault };
    if !p.ai_share_percent.is_finite() || !(0.0..=100.0).contains(&p.ai_share_percent) {
        return Outcome::Fault;
    }
    let spans = if !band.shows_spans() {
        Err(SpansNote::StrongBandsOnly)
    } else if !spans_on {
        Err(SpansNote::Off)
    } else {
        let ok = d.span_status.as_ref().and_then(|v| v.as_str()) == Some("ok");
        match p.spans {
            Some(s) if ok => utf16_spans(text, &s).ok_or(SpansNote::Unavailable),
            _ => Err(SpansNote::Unavailable),
        }
    };
    Outcome::Scored { band, confidence: p.ai_confidence_percent, share: p.ai_share_percent, tokens, spans }
}

/// The spans with UTF-16 offsets, or `None` if any span is out of order,
/// out of range, under the 24-word floor, or its byte offsets disagree with
/// its character offsets.
fn utf16_spans(text: &str, spans: &[ScorerSpan]) -> Option<Vec<Span>> {
    let mut out = Vec::with_capacity(spans.len());
    let mut chars = text.char_indices().peekable();
    let (mut ci, mut u16i) = (0usize, 0usize);
    let mut prev_end = 0usize;
    // Advance to character index `target`, returning (byte, utf16) there.
    let mut seek = |target: usize, ci: &mut usize, u16i: &mut usize| -> Option<(usize, usize)> {
        while *ci < target {
            let (_, c) = chars.next()?;
            *ci += 1;
            *u16i += c.len_utf16();
        }
        let byte = chars.peek().map_or(text.len(), |(b, _)| *b);
        Some((byte, *u16i))
    };
    for s in spans {
        let conf_ok = s.ai_confidence_percent.is_finite() && (0.0..=100.0).contains(&s.ai_confidence_percent);
        if s.start_char < prev_end || s.start_char >= s.end_char || s.words < policy::SPAN_MIN_WORDS || !conf_ok {
            return None;
        }
        let (b0, u0) = seek(s.start_char, &mut ci, &mut u16i)?;
        let (b1, u1) = seek(s.end_char, &mut ci, &mut u16i)?;
        if b0 != s.start_byte || b1 != s.end_byte {
            return None;
        }
        prev_end = s.end_char;
        out.push(Span { start: u0, end: u1, words: s.words, confidence: s.ai_confidence_percent.floor() as u32 });
    }
    Some(out)
}

/// The browser's result for a scored text.
#[allow(clippy::too_many_arguments)]
pub fn answer(
    band: Band,
    confidence: f64,
    share: f64,
    words: usize,
    tokens: u64,
    length: LengthBin,
    spans: Result<Vec<Span>, SpansNote>,
    text: &str,
    package: PackageLine,
    receipt: String,
) -> Answer {
    let (spans, spans_note) = match spans {
        Ok(s) => (Some(s), None),
        Err(n) => (None, Some(n)),
    };
    Answer {
        state: "result",
        band: band.code(),
        confidence_tenths: policy::tenths_floor(confidence),
        confidence_text: policy::confidence_text(confidence),
        share_text: policy::share_text(share, confidence),
        words,
        tokens,
        length: length.code(),
        spans,
        spans_note,
        markers: policy::markers_in(text).collect(),
        invisible: policy::has_invisible(text),
        package,
        receipt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PKG: &str = "ab12";

    fn reply(conf: f64, share: f64, spans: serde_json::Value) -> Vec<u8> {
        json!({"package": PKG, "results": [{
            "id": "d", "status": "ok", "text_sha256": "f00d", "token_count": 300,
            "prediction": {"origin": {"human": 0.1}, "ai_confidence_percent": conf, "ai_share_percent": share,
                "attribution": {"ai": {"profile": "other_ai"}}, "sentences": [], "spans": spans},
            "output_sha256": "beef", "span_status": "ok", "span_output_sha256": "cafe", "ms": 12.5
        }]})
        .to_string()
        .into_bytes()
    }

    fn words(n: usize) -> String {
        vec!["word"; n].join(" ")
    }

    fn span_json(text: &str, start_char: usize, end_char: usize, words: u64) -> serde_json::Value {
        let b = |c: usize| text.char_indices().nth(c).map_or(text.len(), |(b, _)| b);
        json!({"start_char": start_char, "end_char": end_char, "start_byte": b(start_char), "end_byte": b(end_char),
            "words": words, "ai_share_percent": 90.0, "ai_confidence_percent": 92.7})
    }

    #[test]
    fn hashes_and_attribution_never_leave() {
        let text = words(200);
        let out = read(&reply(97.3, 34.0, json!([])), PKG, &text, true);
        let Outcome::Scored { band, confidence, share, tokens, spans } = out else { panic!() };
        let a = answer(band, confidence, share, 200, tokens, LengthBin::Measured, spans, &text,
            PackageLine { digest: "d".into(), publication_pending: true }, "r".into());
        let s = serde_json::to_string(&a).unwrap();
        for leak in ["f00d", "beef", "cafe", "text_sha256", "attribution", "origin", "other_ai", "\"ms\"", "sentences"] {
            assert!(!s.contains(leak), "{leak} in {s}");
        }
        assert_eq!(a.band, "strong");
        assert_eq!(a.confidence_text, "97.3%");
        assert_eq!(a.share_text, "about 34%");
    }

    #[test]
    fn spans_only_in_strong_bands_and_when_on() {
        let text = words(200);
        let sp = json!([span_json(&text, 0, 30 * 5 - 1, 30)]);
        let weak = read(&reply(60.0, 10.0, sp.clone()), PKG, &text, true);
        assert!(matches!(weak, Outcome::Scored { spans: Err(SpansNote::StrongBandsOnly), .. }));
        let off = read(&reply(99.99, 60.0, sp.clone()), PKG, &text, false);
        assert!(matches!(off, Outcome::Scored { spans: Err(SpansNote::Off), .. }));
        let on = read(&reply(99.99, 60.0, sp), PKG, &text, true);
        let Outcome::Scored { spans: Ok(s), band, .. } = on else { panic!() };
        assert_eq!(band, Band::VeryStrong);
        assert_eq!(s, vec![Span { start: 0, end: 149, words: 30, confidence: 92 }]);
    }

    #[test]
    fn utf16_offsets_skip_astral_characters() {
        let text = format!("😀é {}", words(40));
        let start = 3; // after the emoji, é and the space
        let end = text.chars().count();
        let sp = json!([span_json(&text, start, end, 40)]);
        let Outcome::Scored { spans: Ok(s), .. } = read(&reply(95.0, 50.0, sp), PKG, &text, true) else { panic!() };
        // The emoji is two UTF-16 units: offsets 0-1; é is 2; space is 3.
        assert_eq!(s[0].start, 4);
        let units: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(s[0].end, units.len());
    }

    #[test]
    fn inconsistent_spans_drop_all() {
        let text = words(200);
        let mut bad = span_json(&text, 0, 100, 25);
        bad["start_byte"] = json!(1);
        let r = read(&reply(95.0, 50.0, json!([bad])), PKG, &text, true);
        assert!(matches!(r, Outcome::Scored { spans: Err(SpansNote::Unavailable), .. }));
        let short = span_json(&text, 0, 50, 10);
        let r = read(&reply(95.0, 50.0, json!([short])), PKG, &text, true);
        assert!(matches!(r, Outcome::Scored { spans: Err(SpansNote::Unavailable), .. }));
        let past = span_json(&text, 0, 5000, 30);
        let r = read(&reply(95.0, 50.0, json!([past])), PKG, &text, true);
        assert!(matches!(r, Outcome::Scored { spans: Err(SpansNote::Unavailable), .. }));
        let overlap = json!([span_json(&text, 0, 150, 30), span_json(&text, 100, 300, 30)]);
        let r = read(&reply(95.0, 50.0, overlap), PKG, &text, true);
        assert!(matches!(r, Outcome::Scored { spans: Err(SpansNote::Unavailable), .. }));
    }

    #[test]
    fn faults_and_declines() {
        let text = words(200);
        assert_eq!(read(b"not json", PKG, &text, true), Outcome::Fault);
        assert_eq!(read(&reply(95.0, 50.0, json!([])), "other", &text, true), Outcome::Fault);
        assert_eq!(read(&reply(101.0, 50.0, json!([])), PKG, &text, true), Outcome::Fault);
        let long = json!({"package": PKG, "results": [{"status": "unsupported_input", "reason": "unsupported_length",
            "text_sha256": "x", "token_count": 3000, "prediction": null}]});
        assert_eq!(read(long.to_string().as_bytes(), PKG, &text, true), Outcome::Declined("too_long"));
    }
}
