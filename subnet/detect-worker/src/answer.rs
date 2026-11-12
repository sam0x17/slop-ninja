//! The response allow-list: what of the scorer's reply reaches the browser.
//!
//! The scorer returns, per document, `text_sha256`, `output_sha256`,
//! `span_output_sha256`, timing and the full prediction (origin,
//! attribution, sentences). A text hash is a linkable identifier, and
//! attribution is not ready for a public page, so only named fields are
//! read here and only these leave: the band, the two percentages as the page
//! prints them, the word and token counts, the length label, the free cut
//! (the sent text's words, which part was scored and where it ends),
//! notices, and the spans when the band and the span switch allow them. A short text
//! (24 to 149 words) gets only its result, "strong" or "no_strong_signs",
//! with no percentage, share or passage confidence. Each span carries
//! UTF-16 offsets into the exact text the page sent (the scored part is its
//! prefix), computed here from the scorer's character offsets (and checked
//! against its byte offsets), so
//! the page never splits a surrogate pair. Any inconsistency drops every
//! span; nothing is repaired.

use crate::cut::Cut;
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
    /// "AI-like passage: 92%": whole percent, floored; absent for a short
    /// text, which shows no percentage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<u32>,
}

/// Why no passage is drawn.
#[derive(Serialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum SpansNote {
    /// The document is not in a strong band (or a short text has no strong
    /// result).
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
    /// The percentages, from 150 words only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_tenths: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_text: Option<String>,
    /// Whitespace words scored (the length bin's count).
    pub words: usize,
    /// Whitespace words of the whole text sent.
    pub total_words: usize,
    /// How the scored part was chosen: `none` (the whole text),
    /// `first_1000_words` or `token_limit` (`crate::cut`).
    pub cut: &'static str,
    /// Where the scored part ends, in UTF-16 units of the text sent.
    pub scored_end: usize,
    /// The scored part's tokens, frame included.
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

/// A scored text, as the page may see it.
#[derive(Debug, PartialEq)]
pub struct Scored {
    pub band: Band,
    /// The scorer's (confidence, share) percentages; `None` for a short
    /// text, which shows neither.
    pub percents: Option<(f64, f64)>,
    pub tokens: u64,
    pub spans: Result<Vec<Span>, SpansNote>,
}

/// What the scorer's reply came to.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Scored(Scored),
    /// A short text whose passages are off or unavailable: its rule cannot
    /// be applied, so it is refused under the span-free minimum.
    ShortWithoutSpans,
    /// The scorer declined the text (a fixed code for the page).
    Declined(&'static str),
    /// The reply is not what was asked for: never shown.
    Fault,
}

/// Reads a scorer reply for `package` (the load digest sent) about `text`,
/// the scored part, of length bin `length`.
pub fn read(reply: &[u8], package: &str, text: &str, spans_on: bool, length: LengthBin) -> Outcome {
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
    let spans_ok = d.span_status.as_ref().and_then(|v| v.as_str()) == Some("ok");
    if let Some(rule) = length.short_rule() {
        // Every passage must check out, as in a strong band; the rule then
        // reads their confidences, which are never shown.
        let checked = match p.spans.as_deref() {
            Some(s) if spans_on && spans_ok => utf16_spans(text, s).map(|v| (s, v)),
            _ => None,
        };
        let Some((raw, mut shown)) = checked else { return Outcome::ShortWithoutSpans };
        let strong = rule.strong(p.ai_confidence_percent, raw.iter().map(|s| s.ai_confidence_percent));
        for s in &mut shown {
            s.confidence = None;
        }
        return Outcome::Scored(if strong {
            Scored { band: Band::Strong, percents: None, tokens, spans: Ok(shown) }
        } else {
            Scored { band: Band::NoStrongSigns, percents: None, tokens, spans: Err(SpansNote::StrongBandsOnly) }
        });
    }
    let spans = if !band.shows_spans() {
        Err(SpansNote::StrongBandsOnly)
    } else if !spans_on {
        Err(SpansNote::Off)
    } else {
        match p.spans {
            Some(s) if spans_ok => utf16_spans(text, &s).ok_or(SpansNote::Unavailable),
            _ => Err(SpansNote::Unavailable),
        }
    };
    Outcome::Scored(Scored { band, percents: Some((p.ai_confidence_percent, p.ai_share_percent)), tokens, spans })
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
        out.push(Span { start: u0, end: u1, words: s.words, confidence: Some(s.ai_confidence_percent.floor() as u32) });
    }
    Some(out)
}

/// The browser's result for a scored text: `cut` is the free check's scored
/// part of what was sent. Notices name what is in the scored part.
pub fn answer(scored: Scored, length: LengthBin, cut: &Cut<'_>, package: PackageLine, receipt: String) -> Answer {
    let Scored { band, percents, tokens, spans } = scored;
    let text = cut.scored;
    let (spans, spans_note) = match spans {
        Ok(s) => (Some(s), None),
        Err(n) => (None, Some(n)),
    };
    Answer {
        state: "result",
        band: band.code(),
        confidence_tenths: percents.map(|(c, _)| policy::tenths_floor(c)),
        confidence_text: percents.map(|(c, _)| policy::confidence_text(c)),
        share_text: percents.map(|(c, s)| policy::share_text(s, c)),
        words: cut.scored_words,
        total_words: cut.total_words,
        cut: cut.kind.code(),
        scored_end: cut.scored_end(),
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
    use crate::policy::{SHORT_SPAN_LEVEL, SHORT_SPAN_LEVEL_100, STRONG_CUT, VERY_STRONG_CUT};
    use serde_json::json;

    const PKG: &str = "ab12";
    const M: LengthBin = LengthBin::Measured;

    fn reply_with(conf: f64, share: f64, spans: serde_json::Value, span_status: &str) -> Vec<u8> {
        json!({"package": PKG, "results": [{
            "id": "d", "status": "ok", "text_sha256": "f00d", "token_count": 300,
            "prediction": {"origin": {"human": 0.1}, "ai_confidence_percent": conf, "ai_share_percent": share,
                "attribution": {"ai": {"profile": "other_ai"}}, "sentences": [], "spans": spans},
            "output_sha256": "beef", "span_status": span_status, "span_output_sha256": "cafe", "ms": 12.5
        }]})
        .to_string()
        .into_bytes()
    }

    fn reply(conf: f64, share: f64, spans: serde_json::Value) -> Vec<u8> {
        reply_with(conf, share, spans, "ok")
    }

    fn words(n: usize) -> String {
        vec!["word"; n].join(" ")
    }

    fn span_conf(text: &str, start_char: usize, end_char: usize, words: u64, conf: f64) -> serde_json::Value {
        let b = |c: usize| text.char_indices().nth(c).map_or(text.len(), |(b, _)| b);
        json!({"start_char": start_char, "end_char": end_char, "start_byte": b(start_char), "end_byte": b(end_char),
            "words": words, "ai_share_percent": 90.0, "ai_confidence_percent": conf})
    }

    fn span_json(text: &str, start_char: usize, end_char: usize, words: u64) -> serde_json::Value {
        span_conf(text, start_char, end_char, words, 92.7)
    }

    fn scored(o: Outcome) -> Scored {
        match o {
            Outcome::Scored(s) => s,
            other => panic!("{other:?}"),
        }
    }

    fn pkg() -> PackageLine {
        PackageLine { digest: "d".into(), publication_pending: true }
    }

    /// A text scored whole.
    fn whole(text: &str) -> Cut<'_> {
        let n = policy::words(text);
        Cut { scored: text, kind: crate::cut::Kind::Whole, total_words: n, scored_words: n, tokens: 300 }
    }

    #[test]
    fn hashes_and_attribution_never_leave() {
        let text = words(200);
        let sc = scored(read(&reply(97.3, 34.0, json!([])), PKG, &text, true, M));
        let a = answer(sc, M, &whole(&text), pkg(), "r".into());
        let s = serde_json::to_string(&a).unwrap();
        for leak in ["f00d", "beef", "cafe", "text_sha256", "attribution", "origin", "other_ai", "\"ms\"", "sentences"] {
            assert!(!s.contains(leak), "{leak} in {s}");
        }
        assert_eq!(a.band, "strong");
        assert_eq!(a.confidence_text.as_deref(), Some("97.3%"));
        assert_eq!(a.share_text.as_deref(), Some("about 34%"));
    }

    #[test]
    fn spans_only_in_strong_bands_and_when_on() {
        let text = words(200);
        let sp = json!([span_json(&text, 0, 30 * 5 - 1, 30)]);
        let weak = scored(read(&reply(60.0, 10.0, sp.clone()), PKG, &text, true, M));
        assert_eq!(weak.spans, Err(SpansNote::StrongBandsOnly));
        let off = scored(read(&reply(99.99, 60.0, sp.clone()), PKG, &text, false, M));
        assert_eq!(off.spans, Err(SpansNote::Off));
        let on = scored(read(&reply(99.99, 60.0, sp), PKG, &text, true, M));
        assert_eq!(on.band, Band::VeryStrong);
        assert_eq!(on.spans, Ok(vec![Span { start: 0, end: 149, words: 30, confidence: Some(92) }]));
    }

    #[test]
    fn utf16_offsets_skip_astral_characters() {
        let text = format!("😀é {}", words(40));
        let start = 3; // after the emoji, é and the space
        let end = text.chars().count();
        let sp = json!([span_json(&text, start, end, 40)]);
        let Ok(s) = scored(read(&reply(95.0, 50.0, sp), PKG, &text, true, M)).spans else { panic!() };
        // The emoji is two UTF-16 units: offsets 0-1; é is 2; space is 3.
        assert_eq!(s[0].start, 4);
        let units: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(s[0].end, units.len());
    }

    #[test]
    fn inconsistent_spans_drop_all() {
        let text = words(200);
        let unavailable = |sp: serde_json::Value| {
            scored(read(&reply(95.0, 50.0, sp), PKG, &text, true, M)).spans == Err(SpansNote::Unavailable)
        };
        let mut bad = span_json(&text, 0, 100, 25);
        bad["start_byte"] = json!(1);
        assert!(unavailable(json!([bad])));
        assert!(unavailable(json!([span_json(&text, 0, 50, 10)])));
        assert!(unavailable(json!([span_json(&text, 0, 5000, 30)])));
        assert!(unavailable(json!([span_json(&text, 0, 150, 30), span_json(&text, 100, 300, 30)])));
    }

    #[test]
    fn faults_and_declines() {
        let text = words(200);
        assert_eq!(read(b"not json", PKG, &text, true, M), Outcome::Fault);
        assert_eq!(read(&reply(95.0, 50.0, json!([])), "other", &text, true, M), Outcome::Fault);
        assert_eq!(read(&reply(101.0, 50.0, json!([])), PKG, &text, true, M), Outcome::Fault);
        let long = json!({"package": PKG, "results": [{"status": "unsupported_input", "reason": "unsupported_length",
            "text_sha256": "x", "token_count": 3000, "prediction": null}]});
        assert_eq!(read(long.to_string().as_bytes(), PKG, &text, true, M), Outcome::Declined("too_long"));
        // A short text's reply is checked the same way.
        let short = words(30);
        let s24 = LengthBin::Short24;
        assert_eq!(read(&reply(101.0, 50.0, json!([])), PKG, &short, true, s24), Outcome::Fault);
        assert_eq!(read(&reply(99.99, 101.0, json!([])), PKG, &short, true, s24), Outcome::Fault);
    }

    /// A 30-word text with one passage over all of it.
    fn short_case(conf: f64, span: f64) -> (String, Vec<u8>) {
        let text = words(30);
        let end = text.chars().count();
        let r = reply(conf, 80.0, json!([span_conf(&text, 0, end, 30, span)]));
        (text, r)
    }

    #[test]
    fn short_texts_get_one_result_without_percentages() {
        let top = VERY_STRONG_CUT * 100.;
        let level = SHORT_SPAN_LEVEL * 100.;
        let s24 = LengthBin::Short24;
        let (text, r) = short_case(top, level);
        let sc = scored(read(&r, PKG, &text, true, s24));
        assert_eq!(sc.band, Band::Strong);
        assert_eq!(sc.percents, None);
        assert_eq!(sc.spans, Ok(vec![Span { start: 0, end: text.len(), words: 30, confidence: None }]));
        let a = answer(sc, s24, &whole(&text), pkg(), "r".into());
        assert_eq!(a.length, "24_49");
        let s = serde_json::to_value(&a).unwrap();
        for absent in ["confidence_tenths", "confidence_text", "share_text", "spans_note"] {
            assert!(s.get(absent).is_none(), "{absent} in {s}");
        }
        assert!(s["spans"][0].get("confidence").is_none());
        assert_eq!(s["band"], "strong");

        // Either condition short of its level: no strong signs, no passage.
        let below = |x: f64| f64::from_bits(x.to_bits() - 1);
        for (conf, span) in [(below(top), level), (top, below(level)), (99.0, 99.0)] {
            let (text, r) = short_case(conf, span);
            let sc = scored(read(&r, PKG, &text, true, s24));
            assert_eq!(sc.band, Band::NoStrongSigns, "{conf} {span}");
            assert_eq!(sc.spans, Err(SpansNote::StrongBandsOnly));
            let a = answer(sc, s24, &whole(&text), pkg(), "r".into());
            let s = serde_json::to_value(&a).unwrap();
            assert_eq!(s["band"], "no_strong_signs");
            assert!(s.get("spans").is_none() && s.get("confidence_text").is_none() && s.get("share_text").is_none());
        }
        // No passage at all: no strong signs, even at the top level.
        let text = words(30);
        let sc = scored(read(&reply(top, 80.0, json!([])), PKG, &text, true, s24));
        assert_eq!(sc.band, Band::NoStrongSigns);
    }

    #[test]
    fn short_bins_use_their_own_rule() {
        let strong = STRONG_CUT * 100.;
        let (l96, l94) = (SHORT_SPAN_LEVEL * 100., SHORT_SPAN_LEVEL_100 * 100.);
        let text = words(120);
        let end = text.chars().count();
        let case = |conf: f64, span: f64, bin: LengthBin| {
            let r = reply(conf, 50.0, json!([span_conf(&text, 0, end, 120, span)]));
            scored(read(&r, PKG, &text, true, bin)).band
        };
        assert_eq!(case(strong, l94, LengthBin::Short100), Band::Strong);
        assert_eq!(case(strong, l94, LengthBin::Short50), Band::NoStrongSigns);
        assert_eq!(case(VERY_STRONG_CUT * 100., l96, LengthBin::Short50), Band::Strong);
        // From 150 words the bands apply, with percentages.
        let sc = scored(read(&reply(strong, 50.0, json!([])), PKG, &text, true, M));
        assert_eq!((sc.band, sc.percents.is_some()), (Band::Strong, true));
    }

    #[test]
    fn short_texts_need_spans() {
        let (text, r) = short_case(99.999, 99.0);
        let s24 = LengthBin::Short24;
        // Switch off, scorer span failure, missing or inconsistent spans.
        assert_eq!(read(&r, PKG, &text, false, s24), Outcome::ShortWithoutSpans);
        let failed = reply_with(99.999, 80.0, json!([]), "failed");
        assert_eq!(read(&failed, PKG, &text, true, s24), Outcome::ShortWithoutSpans);
        let none = reply_with(99.999, 80.0, serde_json::Value::Null, "ok");
        assert_eq!(read(&none, PKG, &text, true, s24), Outcome::ShortWithoutSpans);
        let mut bad = span_conf(&text, 0, 20, 24, 99.0);
        bad["end_byte"] = json!(3);
        assert_eq!(read(&reply(99.999, 80.0, json!([bad])), PKG, &text, true, s24), Outcome::ShortWithoutSpans);
        let under_floor = span_conf(&text, 0, 20, 4, 99.0);
        assert_eq!(read(&reply(99.999, 80.0, json!([under_floor])), PKG, &text, true, s24), Outcome::ShortWithoutSpans);
    }

    /// A long text's first 1,000 words, cut at a sentence end: the answer
    /// carries both counts, the cut and where it ends, and its passages'
    /// offsets index the text as sent.
    #[test]
    fn a_cut_result_names_its_part() {
        let mut w: Vec<String> = (0..1500).map(|i| ["the", "river", "ran", "past"][i % 4].to_string()).collect();
        w[0] = "😀".into();
        w[985].push('.');
        let sent = w.join(" ");
        let c = crate::cut::free_cut(&sent, &mut |_, _| true).unwrap();
        assert_eq!((c.kind, c.scored_words, c.total_words), (crate::cut::Kind::FirstWords, 986, 1500));
        let length = policy::length_bin(c.scored_words).unwrap();
        assert_eq!(length, LengthBin::Long);
        let end = c.scored.chars().count();
        let sp = json!([span_json(c.scored, end - 200, end, 30)]);
        let sc = scored(read(&reply(99.99, 60.0, sp), PKG, c.scored, true, length));
        let a = answer(sc, length, &c, pkg(), "r".into());
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!((v["words"].as_u64(), v["total_words"].as_u64()), (Some(986), Some(1500)));
        assert_eq!((v["cut"].as_str(), v["length"].as_str()), (Some("first_1000_words"), Some("451_1000")));
        let units: Vec<u16> = sent.encode_utf16().collect();
        let scored_end = v["scored_end"].as_u64().unwrap() as usize;
        assert_eq!(String::from_utf16(&units[..scored_end]).unwrap(), c.scored);
        // The passage ends where the scored part ends, in the sent text's units.
        assert_eq!(a.spans.as_ref().unwrap()[0].end, scored_end);
        // A whole text says so.
        let text = words(200);
        let sc = scored(read(&reply(20.0, 5.0, json!([])), PKG, &text, true, M));
        let v = serde_json::to_value(answer(sc, M, &whole(&text), pkg(), "r".into())).unwrap();
        assert_eq!((v["cut"].as_str(), v["words"].as_u64(), v["total_words"].as_u64()), (Some("none"), Some(200), Some(200)));
        assert_eq!(v["scored_end"].as_u64(), Some(text.len() as u64));
    }
}
