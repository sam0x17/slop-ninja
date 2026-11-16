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
//! (24 to 149 words) gets its band and confidence from its bin's own map
//! (`policy::ShortMap`), applied to the scorer's `calibration_input`, and no
//! share or passage confidence. Each span carries
//! UTF-16 offsets into the exact text the page sent (the scored part is its
//! prefix), computed here from the scorer's character offsets (and checked
//! against its byte offsets), so
//! the page never splits a surrogate pair. Any inconsistency drops every
//! span; nothing is repaired.

use crate::cut::Cut;
use crate::policy::{self, Band, LengthBin, ShortMap};
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
    /// The calibration map's input, asked for short texts only.
    #[serde(default)]
    calibration_input: Option<f64>,
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
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Span {
    /// UTF-16 offsets into the text the page sent.
    pub start: usize,
    pub end: usize,
    pub words: u64,
    /// "AI-like passage: 92%": whole percent, floored; absent for a short
    /// text (the span confidence map was not measured below 150 words).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<u32>,
}

/// Why no passage is drawn.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
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
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
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
    /// The confidence; the share from 150 words only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_tenths: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_text: Option<String>,
    /// The dial's number: the share as `share_text` prints it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_pct: Option<u8>,
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
    /// The result's seal (`crate::seal`), on this final line only; absent
    /// when the store secret is not configured, so the result cannot be
    /// shared or quoted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seal: Option<String>,
}

/// The result as a seal carries it (`result_full`): the answer line's
/// fields without its `state`, `receipt` and `seal`, under the same names.
/// Its offsets (each passage's and `scored_end`) are UTF-16 offsets into
/// `canon_v1` of the text, the string the scorer read and the one a link
/// stores and a preview renders ([`sealed_result`]); the line's own are the
/// sent string's.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct ResultFull {
    pub band: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_tenths: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_pct: Option<u8>,
    pub words: usize,
    pub total_words: usize,
    pub cut: String,
    pub scored_end: usize,
    pub tokens: u64,
    pub length: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spans: Option<Vec<Span>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spans_note: Option<SpansNote>,
    pub markers: Vec<String>,
    pub invisible: bool,
    pub package: PackageLine,
}

impl Answer {
    /// The result a seal carries.
    pub fn result_full(&self) -> ResultFull {
        ResultFull {
            band: self.band.to_owned(),
            confidence_tenths: self.confidence_tenths,
            confidence_text: self.confidence_text.clone(),
            share_text: self.share_text.clone(),
            share_pct: self.share_pct,
            words: self.words,
            total_words: self.total_words,
            cut: self.cut.to_owned(),
            scored_end: self.scored_end,
            tokens: self.tokens,
            length: self.length.to_owned(),
            spans: self.spans.clone(),
            spans_note: self.spans_note,
            markers: self.markers.iter().map(|m| (*m).to_owned()).collect(),
            invisible: self.invisible,
            package: self.package.clone(),
        }
    }
}

/// A scored text, as the page may see it.
#[derive(Debug, PartialEq)]
pub struct Scored {
    pub band: Band,
    /// The confidence percentage: the scorer's from 150 words, the bin's
    /// map's below.
    pub confidence: f64,
    /// The scorer's share percentage; `None` for a short text, which shows
    /// none.
    pub share: Option<f64>,
    pub tokens: u64,
    pub spans: Result<Vec<Span>, SpansNote>,
}

/// What the scorer's reply came to.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Scored(Scored),
    /// A short text with no map to read it (another package's maps, or no
    /// valid `calibration_input` in the reply): refused under the measured
    /// minimum of 150 words.
    ShortUnmapped,
    /// The scorer declined the text (a fixed code for the page).
    Declined(&'static str),
    /// The reply is not what was asked for: never shown.
    Fault,
}

/// Reads a scorer reply for `package` (the load digest sent) about `text`,
/// the scored part, of length bin `length`. A short bin reads its map
/// `short`, the serving package's ([`LengthBin::short_map`]).
pub fn read(
    reply: &[u8],
    package: &str,
    text: &str,
    spans_on: bool,
    length: LengthBin,
    short: Option<&ShortMap>,
) -> Outcome {
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
    let Some(released) = Band::of_percent(p.ai_confidence_percent) else { return Outcome::Fault };
    if !p.ai_share_percent.is_finite() || !(0.0..=100.0).contains(&p.ai_share_percent) {
        return Outcome::Fault;
    }
    // From 150 words, the release's confidence and bands; below, the bin's
    // map on the release map's own input, and no share.
    let (band, confidence, share) = if length.is_short() {
        let Some(m) = short.filter(|m| m.length == length) else { return Outcome::ShortUnmapped };
        let Some(c) = d.calibration_input.and_then(|x| m.confidence(x)) else { return Outcome::ShortUnmapped };
        (m.band(c), c * 100., None)
    } else {
        (released, p.ai_confidence_percent, Some(p.ai_share_percent))
    };
    let spans_ok = d.span_status.as_ref().and_then(|v| v.as_str()) == Some("ok");
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
    // Below 150 words a passage shows no confidence of its own.
    let spans = spans.map(|mut v| {
        if share.is_none() {
            for s in &mut v {
                s.confidence = None;
            }
        }
        v
    });
    Outcome::Scored(Scored { band, confidence, share, tokens, spans })
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
    let Scored { band, confidence, share, tokens, spans } = scored;
    let text = cut.scored;
    let (spans, spans_note) = match spans {
        Ok(s) => (Some(s), None),
        Err(n) => (None, Some(n)),
    };
    Answer {
        state: "result",
        band: band.code(),
        confidence_tenths: Some(policy::tenths_floor(confidence)),
        confidence_text: Some(policy::confidence_text(confidence)),
        share_text: share.map(|s| policy::share_text(s, confidence)),
        share_pct: share.map(|s| policy::share_pct(s, confidence)),
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
        seal: None,
    }
}

/// Maps the line's offsets (each passage's and the scored part's end),
/// computed in `canonical` (= `canon_v1(text)`, what the cut and the scorer
/// read), to UTF-16 offsets into `text`, the string the page sent
/// (`crate::canon::utf16_to_original`). Passages that do not map are all
/// dropped (`spans_note: unavailable`); nothing is repaired.
pub fn to_original(line: &mut Answer, text: &str, canonical: &str) {
    if text == canonical {
        return;
    }
    let mut offsets: Vec<usize> = line.spans.iter().flatten().flat_map(|s| [s.start, s.end]).collect();
    offsets.push(line.scored_end);
    if let Some(mapped) = crate::canon::utf16_to_original(text, canonical, &offsets) {
        let mut m = mapped.into_iter();
        for s in line.spans.iter_mut().flatten() {
            s.start = m.next().unwrap_or(s.start);
            s.end = m.next().unwrap_or(s.end);
        }
        line.scored_end = m.next().unwrap_or(line.scored_end);
        return;
    }
    if line.spans.take().is_some() {
        line.spans_note = Some(SpansNote::Unavailable);
    }
    line.scored_end = crate::canon::utf16_to_original(text, canonical, &[line.scored_end])
        .and_then(|v| v.first().copied())
        .unwrap_or_else(|| text.encode_utf16().count());
}

/// Splits a scored line computed in `canonical` (= `canon_v1(text)`): the
/// result a seal carries keeps `canonical`'s offsets, and the line's are
/// mapped to `text`, the string the page sent ([`to_original`]). Passages
/// the line drops (they do not map) are dropped from the seal too, so a
/// link or a preview never draws a passage the result did not show.
pub fn sealed_result(line: &mut Answer, text: &str, canonical: &str) -> ResultFull {
    let mut full = line.result_full();
    to_original(line, text, canonical);
    if line.spans.is_none() {
        full.spans = None;
        full.spans_note = line.spans_note;
    }
    full
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// `result_full` is the answer line's JSON less `state`, `receipt` and
    /// `seal`, and reads back as itself.
    #[test]
    fn the_sealed_result_is_the_answer_line() {
        let text = words(200);
        let s = text.find("word word").unwrap();
        let sc = scored(read(&reply(97.3, 34.0, json!([span_json(&text, s, s + 150, 30)])), PKG, &text, true, M, None));
        let mut a = answer(sc, M, &whole(&text), pkg(), "r".into());
        a.seal = Some("sealed".into());
        let mut line = serde_json::to_value(&a).unwrap();
        let obj = line.as_object_mut().unwrap();
        assert_eq!(obj.remove("state"), Some(json!("result")));
        assert_eq!(obj.remove("receipt"), Some(json!("r")));
        assert_eq!(obj.remove("seal"), Some(json!("sealed")));
        let full = a.result_full();
        assert_eq!(serde_json::to_value(&full).unwrap(), line);
        let back: ResultFull = serde_json::from_value(line).unwrap();
        assert_eq!(back, full);
        a.seal = None;
        assert!(serde_json::to_value(&a).unwrap().get("seal").is_none());
    }

    /// The scorer reads the canonical text; the page highlights the string
    /// it sent.
    #[test]
    fn offsets_map_back_to_the_sent_string() {
        let canonical = format!("caf\u{e9} {}", words(199));
        let sent = format!("\r\n  cafe\u{301} {}  \r\n", words(199));
        assert_eq!(&*crate::canon::canon_v1(&sent), canonical.as_str());
        let sp = json!([span_json(&canonical, 5, 5 + 30 * 5 - 1, 30)]);
        let sc = scored(read(&reply(99.99, 60.0, sp), PKG, &canonical, true, M, None));
        let mut a = answer(sc, M, &whole(&canonical), pkg(), "r".into());
        assert_eq!((a.spans.as_ref().unwrap()[0].start, a.scored_end), (5, canonical.len() - 1));
        to_original(&mut a, &sent, &canonical);
        let s16: Vec<u16> = sent.encode_utf16().collect();
        let span = &a.spans.as_ref().unwrap()[0];
        // Four leading units, and the decomposed accent's extra one.
        assert_eq!((span.start, span.end), (4 + 6, 4 + 6 + 149));
        assert_eq!(String::from_utf16(&s16[span.start..span.start + 4]).unwrap(), "word");
        assert_eq!(a.scored_end, sent.trim_end().encode_utf16().count());
        // A canonical text is left as it is.
        let before = a.scored_end;
        to_original(&mut a, &canonical, &canonical);
        assert_eq!(a.scored_end, before);
        // Offsets that do not map drop every passage.
        let mut bad = answer(
            scored(read(&reply(99.99, 60.0, json!([span_json(&canonical, 5, 154, 30)])), PKG, &canonical, true, M, None)),
            M,
            &whole(&canonical),
            pkg(),
            "r".into(),
        );
        bad.spans.as_mut().unwrap()[0].end = usize::MAX;
        to_original(&mut bad, &sent, &canonical);
        assert_eq!((bad.spans.is_none(), bad.spans_note), (true, Some(SpansNote::Unavailable)));
        assert_eq!(bad.scored_end, sent.trim_end().encode_utf16().count());
    }

    /// The seal keeps the canonical text's offsets while the line gets the
    /// sent string's; a passage the line drops is not in the seal either.
    #[test]
    fn the_seal_keeps_the_canonical_offsets() {
        let canonical = format!("caf\u{e9} {}", words(199));
        let sent = format!("\r\n  cafe\u{301} {}  \r\n", words(199));
        let line = || {
            let sp = json!([span_json(&canonical, 5, 5 + 30 * 5 - 1, 30)]);
            let sc = scored(read(&reply(99.99, 60.0, sp), PKG, &canonical, true, M, None));
            answer(sc, M, &whole(&canonical), pkg(), "r".into())
        };
        let mut a = line();
        let full = sealed_result(&mut a, &sent, &canonical);
        let c16: Vec<u16> = canonical.encode_utf16().collect();
        let s16: Vec<u16> = sent.encode_utf16().collect();
        let (fs, ls) = (&full.spans.as_ref().unwrap()[0], &a.spans.as_ref().unwrap()[0]);
        assert_eq!((fs.start, fs.end, full.scored_end), (5, 5 + 149, c16.len()));
        assert_eq!((ls.start, ls.end, a.scored_end), (4 + 6, 4 + 6 + 149, sent.trim_end().encode_utf16().count()));
        // Both draw the same words, each in its own string.
        assert_eq!(c16[fs.start..fs.end], s16[ls.start..ls.end]);
        // Apart from the offsets, the seal is the line.
        let mut mapped = full.clone();
        (mapped.spans, mapped.scored_end) = (a.spans.clone(), a.scored_end);
        assert_eq!(mapped, a.result_full());
        // A canonical text: the seal is the line.
        let mut same = line();
        assert_eq!(sealed_result(&mut same, &canonical, &canonical), same.result_full());
        // Passages that do not map leave both.
        let mut bad = line();
        bad.spans.as_mut().unwrap()[0].end = usize::MAX;
        let full = sealed_result(&mut bad, &sent, &canonical);
        assert_eq!((full.spans, full.spans_note), (None, Some(SpansNote::Unavailable)));
        assert_eq!(bad.spans_note, Some(SpansNote::Unavailable));
        assert_eq!(full.scored_end, c16.len());
    }

    #[test]
    fn hashes_and_attribution_never_leave() {
        let text = words(200);
        let sc = scored(read(&reply(97.3, 34.0, json!([])), PKG, &text, true, M, None));
        let a = answer(sc, M, &whole(&text), pkg(), "r".into());
        let s = serde_json::to_string(&a).unwrap();
        for leak in ["f00d", "beef", "cafe", "text_sha256", "attribution", "origin", "other_ai", "\"ms\"", "sentences"]
        {
            assert!(!s.contains(leak), "{leak} in {s}");
        }
        assert_eq!(a.band, "strong");
        assert_eq!(a.confidence_text.as_deref(), Some("97.3%"));
        assert_eq!(a.share_text.as_deref(), Some("about 34%"));
        assert_eq!(a.share_pct, Some(34));
    }

    #[test]
    fn spans_only_in_strong_bands_and_when_on() {
        let text = words(200);
        let sp = json!([span_json(&text, 0, 30 * 5 - 1, 30)]);
        let weak = scored(read(&reply(60.0, 10.0, sp.clone()), PKG, &text, true, M, None));
        assert_eq!(weak.spans, Err(SpansNote::StrongBandsOnly));
        let off = scored(read(&reply(99.99, 60.0, sp.clone()), PKG, &text, false, M, None));
        assert_eq!(off.spans, Err(SpansNote::Off));
        let on = scored(read(&reply(99.99, 60.0, sp), PKG, &text, true, M, None));
        assert_eq!(on.band, Band::VeryStrong);
        assert_eq!(on.spans, Ok(vec![Span { start: 0, end: 149, words: 30, confidence: Some(92) }]));
    }

    #[test]
    fn utf16_offsets_skip_astral_characters() {
        let text = format!("😀é {}", words(40));
        let start = 3; // after the emoji, é and the space
        let end = text.chars().count();
        let sp = json!([span_json(&text, start, end, 40)]);
        let Ok(s) = scored(read(&reply(95.0, 50.0, sp), PKG, &text, true, M, None)).spans else { panic!() };
        // The emoji is two UTF-16 units: offsets 0-1; é is 2; space is 3.
        assert_eq!(s[0].start, 4);
        let units: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(s[0].end, units.len());
    }

    #[test]
    fn inconsistent_spans_drop_all() {
        let text = words(200);
        let unavailable = |sp: serde_json::Value| {
            scored(read(&reply(95.0, 50.0, sp), PKG, &text, true, M, None)).spans == Err(SpansNote::Unavailable)
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
        assert_eq!(read(b"not json", PKG, &text, true, M, None), Outcome::Fault);
        assert_eq!(read(&reply(95.0, 50.0, json!([])), "other", &text, true, M, None), Outcome::Fault);
        assert_eq!(read(&reply(101.0, 50.0, json!([])), PKG, &text, true, M, None), Outcome::Fault);
        let long = json!({"package": PKG, "results": [{"status": "unsupported_input", "reason": "unsupported_length",
            "text_sha256": "x", "token_count": 3000, "prediction": null}]});
        assert_eq!(read(long.to_string().as_bytes(), PKG, &text, true, M, None), Outcome::Declined("too_long"));
        // A short text's reply is checked the same way.
        let short = words(30);
        let s24 = LengthBin::Short24;
        let m = s24.short_map(D24);
        assert_eq!(read(&reply(101.0, 50.0, json!([])), PKG, &short, true, s24, m), Outcome::Fault);
        assert_eq!(read(&reply(99.99, 101.0, json!([])), PKG, &short, true, s24, m), Outcome::Fault);
    }

    /// Span package d24's published digest, the maps' package.
    const D24: &str = "4bc3d49ce183d4a9ab150867eb4ec23280565351e07c041d161c9e728e19eac3";

    fn map(b: LengthBin) -> &'static ShortMap {
        b.short_map(D24).unwrap()
    }

    /// The input of the first knot at `level` in `b`'s map.
    fn input_at(b: LengthBin, level: f64) -> f64 {
        map(b).knots.iter().find(|k| k.1 == level).unwrap().0
    }

    /// A reply for an `n`-word text with one passage over all of it, the
    /// scorer's own figures `conf` and `share`, and `input` as the
    /// calibration input.
    fn short_case(n: usize, conf: f64, input: serde_json::Value) -> (String, Vec<u8>) {
        let text = words(n);
        let end = text.chars().count();
        let mut v: serde_json::Value =
            serde_json::from_slice(&reply(conf, 80.0, json!([span_conf(&text, 0, end, n as u64, 97.0)]))).unwrap();
        v["results"][0]["calibration_input"] = input;
        (text, v.to_string().into_bytes())
    }

    fn short_read(n: usize, conf: f64, input: f64, b: LengthBin, spans_on: bool) -> Outcome {
        let (text, r) = short_case(n, conf, json!(input));
        read(&r, PKG, &text, spans_on, b, Some(map(b)))
    }

    /// A short text prints its bin's confidence and band, never the share or
    /// a passage's confidence; the scorer's own confidence is not used.
    #[test]
    fn short_texts_get_their_bins_confidence_and_band() {
        let s24 = LengthBin::Short24;
        let m = map(s24);
        for (level, band) in [(m.strong, Band::Strong), (m.very_strong, Band::VeryStrong)] {
            // The scorer's own confidence would be weak: the map decides.
            let sc = scored(short_read(30, 12.0, input_at(s24, level), s24, true));
            assert_eq!((sc.band, sc.confidence, sc.share), (band, level * 100., None));
            assert_eq!(sc.spans, Ok(vec![Span { start: 0, end: words(30).len(), words: 30, confidence: None }]));
            let a = answer(sc, s24, &whole(&words(30)), pkg(), "r".into());
            let v = serde_json::to_value(&a).unwrap();
            assert_eq!((v["band"].as_str(), v["length"].as_str()), (Some(band.code()), Some("24_49")));
            assert_eq!(v["confidence_text"], json!(policy::confidence_text(level * 100.)));
            assert_eq!(v["confidence_tenths"], json!(policy::tenths_floor(level * 100.)));
            for absent in ["share_text", "share_pct", "spans_note"] {
                assert!(v.get(absent).is_none(), "{absent} in {v}");
            }
            assert!(v["spans"][0].get("confidence").is_none());
        }
        assert_eq!(policy::confidence_text(m.strong * 100.), "93.7%");
        assert_eq!(policy::confidence_text(m.very_strong * 100.), "98.8%");
        // Below the strong cut: no passage, whatever the scorer's own band.
        let sc = scored(short_read(30, 99.99, 0.5, s24, true));
        assert_eq!((sc.band, sc.spans.clone()), (Band::Weak, Err(SpansNote::StrongBandsOnly)));
        let v = serde_json::to_value(answer(sc, s24, &whole(&words(30)), pkg(), "r".into())).unwrap();
        assert_eq!((v["band"].as_str(), v["confidence_text"].as_str()), (Some("weak"), Some("26.6%")));
        assert!(v.get("spans").is_none() && v.get("share_text").is_none());
    }

    /// One input, three bins, three readings; from 150 words the input is
    /// ignored and the scorer's own figures apply.
    #[test]
    fn short_bins_use_their_own_map() {
        let case = |n: usize, b: LengthBin| {
            let sc = scored(short_read(n, 50.0, 0.999, b, true));
            (sc.band, sc.confidence)
        };
        assert_eq!(case(30, LengthBin::Short24), (Band::Strong, 0.956931583582583 * 100.));
        assert_eq!(case(60, LengthBin::Short50), (Band::Strong, 0.9749711066165848 * 100.));
        assert_eq!(case(120, LengthBin::Short100), (Band::VeryStrong, 0.999869007073618 * 100.));
        let (text, r) = short_case(200, 50.0, json!(0.999));
        let sc = scored(read(&r, PKG, &text, true, M, None));
        assert_eq!((sc.band, sc.confidence, sc.share), (Band::Unclear, 50.0, Some(80.0)));
    }

    /// Without its map (another package's, or another bin's) or a valid
    /// input, a short text is refused under the measured minimum.
    #[test]
    fn short_texts_need_their_map_and_input() {
        let s24 = LengthBin::Short24;
        let x = input_at(s24, map(s24).strong);
        for input in [serde_json::Value::Null, json!(1.5), json!(-0.1), json!(f64::MAX)] {
            let (text, r) = short_case(30, 99.0, input.clone());
            assert_eq!(read(&r, PKG, &text, true, s24, Some(map(s24))), Outcome::ShortUnmapped, "{input}");
        }
        // An older scorer's reply has no such field.
        let (text, r) = short_case(30, 99.0, json!(x));
        let mut v: serde_json::Value = serde_json::from_slice(&r).unwrap();
        v["results"][0].as_object_mut().unwrap().remove("calibration_input");
        assert_eq!(read(v.to_string().as_bytes(), PKG, &text, true, s24, Some(map(s24))), Outcome::ShortUnmapped);
        // A value of another type is not the scorer's reply.
        let (text, r) = short_case(30, 99.0, json!("0.99"));
        assert_eq!(read(&r, PKG, &text, true, s24, Some(map(s24))), Outcome::Fault);
        let (text, r) = short_case(30, 99.0, json!(x));
        assert_eq!(read(&r, PKG, &text, true, s24, None), Outcome::ShortUnmapped);
        assert_eq!(read(&r, PKG, &text, true, s24, Some(map(LengthBin::Short50))), Outcome::ShortUnmapped);
        // A reply that is not the scorer's is still a fault.
        assert_eq!(read(&reply(101.0, 50.0, json!([])), PKG, &text, true, s24, Some(map(s24))), Outcome::Fault);
    }

    /// Spans off or failed leave a short text scored, without passages; no
    /// passage under 24 words is ever drawn.
    #[test]
    fn short_texts_do_not_need_spans() {
        let s24 = LengthBin::Short24;
        let x = input_at(s24, map(s24).strong);
        let sc = scored(short_read(30, 99.0, x, s24, false));
        assert_eq!((sc.band, sc.spans), (Band::Strong, Err(SpansNote::Off)));
        let text = words(30);
        let mut failed: serde_json::Value =
            serde_json::from_slice(&reply_with(99.0, 80.0, serde_json::Value::Null, "failed")).unwrap();
        failed["results"][0]["calibration_input"] = json!(x);
        let sc = scored(read(failed.to_string().as_bytes(), PKG, &text, true, s24, Some(map(s24))));
        assert_eq!((sc.band, sc.spans), (Band::Strong, Err(SpansNote::Unavailable)));
        let mut under: serde_json::Value =
            serde_json::from_slice(&reply(99.0, 80.0, json!([span_conf(&text, 0, 20, 4, 99.0)]))).unwrap();
        under["results"][0]["calibration_input"] = json!(x);
        let sc = scored(read(under.to_string().as_bytes(), PKG, &text, true, s24, Some(map(s24))));
        assert_eq!((sc.band, sc.spans), (Band::Strong, Err(SpansNote::Unavailable)));
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
        let sc = scored(read(&reply(99.99, 60.0, sp), PKG, c.scored, true, length, None));
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
        let sc = scored(read(&reply(20.0, 5.0, json!([])), PKG, &text, true, M, None));
        let v = serde_json::to_value(answer(sc, M, &whole(&text), pkg(), "r".into())).unwrap();
        assert_eq!(
            (v["cut"].as_str(), v["words"].as_u64(), v["total_words"].as_u64()),
            (Some("none"), Some(200), Some(200))
        );
        assert_eq!(v["scored_end"].as_u64(), Some(text.len() as u64));
    }
}
