//! One document's result, in the real scorer's shape
//! (`inference/a-container-serve`, `Model::score_with_sentences`): `id`,
//! `status`, `text_sha256`, `token_count`, `prediction` (origin,
//! `ai_confidence_percent`, `ai_share_percent`, attribution, and with spans
//! asked for and a token head, `sentences` and `spans`), `output_sha256`,
//! `span_status`, `span_output_sha256` and `ms`. The values come from the
//! text's markers ([`crate::markers`]); the token count is the scorer's own,
//! from the compiled-in tokenizer table.

use crate::markers::{self, Directives, DocStatus, SpanMode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The real scorer's per-document token limit, frame included.
pub const MAX_TOKENS: u64 = 2048;
/// The span floor: the decoder never emits a passage under 24 words.
pub const SPAN_MIN_WORDS: usize = 24;
/// The confidence from which the auto mode draws a passage: the detect
/// Worker's strong cut.
pub const STRONG_PERCENT: f64 = 90.909_090_909_090_9;
/// A passage's default confidence: above both short-text passage levels.
pub const SPAN_CONF: f64 = 99.0;

pub fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// How the request asked.
#[derive(Clone, Copy, Debug)]
pub struct Ask {
    pub spans: bool,
    pub sentences: bool,
    /// Whether the loaded package has a token head (spans are emitted only
    /// with one).
    pub token_head: bool,
}

/// The exact token count of `text` under the pinned tokenizer, frame included.
pub fn tokens(text: &str) -> Option<u64> {
    slop_ninja_scorer_tokens::count(text, &mut |_, _| true).ok()
}

/// A whitespace word: character and byte offsets.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Word {
    c0: usize,
    c1: usize,
    b0: usize,
    b1: usize,
    ends_sentence: bool,
}

fn words(text: &str) -> Vec<Word> {
    let mut out = Vec::new();
    let mut cur: Option<(usize, usize)> = None;
    let mut last: Option<char> = None;
    let mut ci = 0usize;
    for (b, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some((c0, b0)) = cur.take() {
                out.push(Word { c0, c1: ci, b0, b1: b, ends_sentence: matches!(last, Some('.' | '!' | '?')) });
            }
        } else if cur.is_none() {
            cur = Some((ci, b));
        }
        last = Some(c);
        ci += 1;
    }
    if let Some((c0, b0)) = cur {
        out.push(Word { c0, c1: ci, b0, b1: text.len(), ends_sentence: matches!(last, Some('.' | '!' | '?')) });
    }
    out
}

fn passage(ws: &[Word], share: f64, conf: f64) -> Option<Value> {
    let (first, last) = (ws.first()?, ws.last()?);
    Some(json!({"start_char": first.c0, "end_char": last.c1, "start_byte": first.b0, "end_byte": last.b1,
        "words": ws.len(), "ai_share_percent": share, "ai_confidence_percent": conf}))
}

/// The passages the markers ask for.
fn spans(text: &str, d: &Directives, conf: f64) -> Vec<Value> {
    let ws = words(text);
    let span_conf = d.span_conf.unwrap_or(SPAN_CONF);
    let mut groups: Vec<Vec<Word>> = if d.regions.is_empty() {
        if conf >= STRONG_PERCENT || d.spans == SpanMode::Short { vec![ws.clone()] } else { Vec::new() }
    } else {
        d.regions.iter().map(|r| ws.iter().copied().filter(|w| w.c0 >= r.after_begin && w.c1 <= r.before_end).collect()).collect()
    };
    if d.spans == SpanMode::Short {
        for g in &mut groups {
            g.truncate(SPAN_MIN_WORDS - 1);
        }
    } else {
        groups.retain(|g| g.len() >= SPAN_MIN_WORDS);
    }
    let mut out: Vec<Value> = groups.iter().filter_map(|g| passage(g, 90.0, span_conf)).collect();
    if d.spans == SpanMode::Bad
        && let Some(first) = out.first_mut()
    {
        first["start_byte"] = json!(first["start_byte"].as_u64().unwrap_or(0) + 1);
    }
    out
}

/// Sentences: runs of words ending at `.`, `!` or `?`.
fn sentences(text: &str, share: f64, conf: f64) -> Vec<Value> {
    let ws = words(text);
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, w) in ws.iter().enumerate() {
        if w.ends_sentence || i + 1 == ws.len() {
            let run = &ws[start..=i];
            if let Some(mut s) = passage(run, share, conf) {
                s["punctuated"] = json!(w.ends_sentence);
                out.push(s);
            }
            start = i + 1;
        }
    }
    out
}

fn refused(id: &str, text_sha: &str, status: &str, reason: &str, tokens: Option<u64>) -> Value {
    let mut r = json!({"id": id, "status": status, "reason": reason, "text_sha256": text_sha, "prediction": null});
    if let Some(n) = tokens {
        r["token_count"] = json!(n);
    }
    r
}

/// The result for one document.
pub fn document(id: &str, text: &str, ask: Ask) -> Value {
    let d = markers::parse(text);
    let text_sha = sha(text.as_bytes());
    if text.trim().is_empty() || d.status == Some(DocStatus::EmptyText) {
        return refused(id, &text_sha, "unsupported_input", "empty_text", None);
    }
    let Some(n) = tokens(text) else { return refused(id, &text_sha, "error", "tokenize", None) };
    match d.status {
        Some(DocStatus::ErrorTokenize) => return refused(id, &text_sha, "error", "tokenize", None),
        Some(DocStatus::UnsupportedLength) => return refused(id, &text_sha, "unsupported_input", "unsupported_length", Some(n)),
        Some(DocStatus::NoWordTokens) => return refused(id, &text_sha, "unsupported_input", "no_word_tokens", Some(n)),
        Some(DocStatus::ErrorModel) => return refused(id, &text_sha, "error", "model", Some(n)),
        _ => {}
    }
    if n > MAX_TOKENS {
        return refused(id, &text_sha, "unsupported_input", "unsupported_length", Some(n));
    }
    if !text.chars().any(char::is_alphanumeric) {
        return refused(id, &text_sha, "unsupported_input", "no_word_tokens", Some(n));
    }
    let (conf, share) = d.percents();
    let c = conf / 100.0;
    let model_only = (share / 100.0 * 0.5).min(c);
    let mut prediction = json!({
        "origin": {"human_only": 1.0 - c, "model_only": model_only, "mixed": c - model_only},
        "ai_confidence_percent": conf,
        "ai_share_percent": share,
        "attribution": {"human": {"profile": "other_human"}, "ai": {"profile": "other_ai"}},
    });
    let output_sha = sha(prediction.to_string().as_bytes());
    let mut r = json!({"id": id, "status": "ok", "text_sha256": text_sha, "token_count": n, "output_sha256": output_sha});
    if ask.spans && ask.token_head {
        if d.spans == SpanMode::Off {
            prediction["sentences"] = Value::Null;
            prediction["spans"] = Value::Null;
            r["span_status"] = json!({"unsupported": "stub: spans off"});
        } else {
            let spans = if d.spans == SpanMode::None { Vec::new() } else { spans(text, &d, conf) };
            prediction["sentences"] = json!(sentences(text, share, conf));
            prediction["spans"] = json!(spans);
            r["span_status"] = json!("ok");
            let both = json!({"sentences": prediction["sentences"], "spans": prediction["spans"]});
            r["span_output_sha256"] = json!(sha(both.to_string().as_bytes()));
            if !ask.sentences
                && let Some(p) = prediction.as_object_mut()
            {
                p.remove("sentences");
            }
        }
    }
    r["prediction"] = prediction;
    r["ms"] = json!(1.0);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASK: Ask = Ask { spans: true, sentences: true, token_head: true };

    fn prose(n: usize) -> String {
        (0..n).map(|i| if i % 10 == 9 { "word." } else { "word" }).collect::<Vec<_>>().join(" ")
    }

    /// The detect Worker's own check of a passage (answer.rs `utf16_spans`):
    /// byte offsets agree with character offsets.
    fn offsets_agree(text: &str, s: &Value) -> bool {
        let b = |c: u64| text.char_indices().nth(c as usize).map_or(text.len(), |(b, _)| b) as u64;
        s["start_byte"].as_u64() == Some(b(s["start_char"].as_u64().unwrap_or(0)))
            && s["end_byte"].as_u64() == Some(b(s["end_char"].as_u64().unwrap_or(0)))
    }

    #[test]
    fn the_default_is_weak_with_no_passage() {
        let t = prose(200);
        let r = document("d", &t, ASK);
        assert_eq!(r["status"], "ok");
        assert_eq!(r["prediction"]["ai_confidence_percent"], 12.5);
        assert_eq!(r["prediction"]["spans"], json!([]));
        assert_eq!(r["span_status"], "ok");
        assert_eq!(r["token_count"].as_u64(), tokens(&t));
        assert!(r["prediction"]["sentences"].as_array().is_some_and(|s| s.len() == 20));
    }

    #[test]
    fn a_strong_band_draws_one_passage_with_exact_offsets() {
        let t = format!("Ünïcode 🎉 sn-stub:band=strong {}", prose(60));
        let r = document("d", &t, ASK);
        let spans = r["prediction"]["spans"].as_array().cloned().unwrap_or_default();
        assert_eq!(spans.len(), 1);
        assert!(offsets_agree(&t, &spans[0]));
        assert_eq!(spans[0]["words"], 63);
        assert_eq!(spans[0]["ai_confidence_percent"], SPAN_CONF);
    }

    #[test]
    fn regions_bad_short_off_and_none() {
        let t = format!("Lead in. sn-stub:span-begin {} sn-stub:span-end tail {}", prose(30), prose(5));
        let spans = document("d", &t, ASK)["prediction"]["spans"].as_array().cloned().unwrap_or_default();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0]["words"], 30);
        assert!(offsets_agree(&t, &spans[0]));
        // A region under the floor is never emitted.
        let t2 = format!("sn-stub:span-begin {} sn-stub:span-end", prose(23));
        assert_eq!(document("d", &t2, ASK)["prediction"]["spans"], json!([]));
        let bad = format!("sn-stub:band=strong sn-stub:spans=bad {}", prose(40));
        let s = &document("d", &bad, ASK)["prediction"]["spans"][0];
        assert!(!offsets_agree(&bad, s));
        let short = format!("sn-stub:spans=short {}", prose(40));
        assert_eq!(document("d", &short, ASK)["prediction"]["spans"][0]["words"], 23);
        let off = document("d", &format!("sn-stub:band=strong sn-stub:spans=off {}", prose(40)), ASK);
        assert_eq!(off["prediction"]["spans"], Value::Null);
        assert!(off["span_status"]["unsupported"].is_string());
        let none = document("d", &format!("sn-stub:band=strong sn-stub:spans=none {}", prose(40)), ASK);
        assert_eq!(none["prediction"]["spans"], json!([]));
    }

    #[test]
    fn no_spans_unless_asked_and_a_token_head() {
        let t = format!("sn-stub:band=strong {}", prose(40));
        for ask in [Ask { spans: false, ..ASK }, Ask { token_head: false, ..ASK }] {
            let r = document("d", &t, ask);
            assert!(r.get("span_status").is_none() && r["prediction"].get("spans").is_none());
        }
        let r = document("d", &t, Ask { sentences: false, ..ASK });
        assert!(r["prediction"].get("sentences").is_none() && r["prediction"]["spans"].is_array());
    }

    #[test]
    fn statuses_follow_the_real_scorer() {
        assert_eq!(document("d", "  \n", ASK)["reason"], "empty_text");
        assert_eq!(document("d", "?? !! ..", ASK)["reason"], "no_word_tokens");
        let long = prose(3000);
        let r = document("d", &long, ASK);
        assert_eq!((r["status"].as_str(), r["reason"].as_str()), (Some("unsupported_input"), Some("unsupported_length")));
        assert!(r["token_count"].as_u64().is_some_and(|n| n > MAX_TOKENS));
        for (m, status, reason) in [
            ("unsupported_length", "unsupported_input", "unsupported_length"),
            ("no_word_tokens", "unsupported_input", "no_word_tokens"),
            ("error_tokenize", "error", "tokenize"),
            ("error_model", "error", "model"),
            ("empty_text", "unsupported_input", "empty_text"),
        ] {
            let r = document("d", &format!("sn-stub:status={m} {}", prose(50)), ASK);
            assert_eq!((r["status"].as_str(), r["reason"].as_str()), (Some(status), Some(reason)), "{m}");
            assert_eq!(r["prediction"], Value::Null);
        }
    }
}
