//! A span localization (protocol spans-v1, Section 4.2): the spans-v1 token
//! head and the emission rule that turns its per-token AI logits into words,
//! sentences and spans with exact offsets. f64 throughout; no tokenizer,
//! encoder, filesystem or platform-specific operator. The Python reference
//! (`grammar-eval/python/full_a_spans.py`) implements the same rules; the
//! golden vectors in `tests/golden/span-golden-v1.json` bind the two.
//!
//! The text is only segmented (whitespace words and the sealed sentence
//! splitter); no prose is generated or altered. Offsets are into the exact
//! request text: `*_char` Unicode scalar indices, `*_byte` UTF-8 byte offsets,
//! half-open.
//!
//! - Words: maximal runs of non-[`is_space`] characters (Python `str.isspace`).
//! - Word probability: mean sigmoid(token logit) over the word-bearing tokens
//!   whose non-whitespace characters overlap the word; a word without one makes
//!   emission unsupported for that request.
//! - Share tilt: b with mean_w sigmoid(logit(p_w) + b) = clamp(s, 1e-6),
//!   p_w clamped to [1e-9, 1-1e-9]; bisection on [-40, 40], 200 iterations.
//! - Sentences: the sealed `a_comp_sentences_v1` splitter (Rust
//!   `char::is_whitespace`); sentences without words are not emitted.
//! - Sentence share = mean q_w; confidence = min(map(max q_w), document confidence).
//! - Spans: [`Decoder`] on q_w; share = mean q_w; confidence =
//!   min(map(mean q_w), document confidence).
use crate::involvement_calibration::{Knot, isotonic_predict_score};
use crate::shared::{Linear, Tensor, sigmoid};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

pub const TOKEN_HEAD_RULE: &str =
    "slop_ninja_token_head_v1:second(gelu_tanh(first(final_norm_token_state)))";
pub const LABEL_POLICY: &str = "slop_ninja_token_span_label_v1";
pub const WORD_RULE: &str = "slop_ninja_share_words_v1:maximal_runs_of_non_python_isspace";
pub const WORD_PROBABILITY_RULE: &str =
    "slop_ninja_word_probability_v1:mean_sigmoid_token_logit_over_overlapping_word_tokens";
pub const TILT_RULE: &str = "slop_ninja_share_tilt_v1:mean_w_sigmoid(logit(clamp(p_w,1e-9))+b)=clamp(s,1e-6);bisection_b_in_[-40,40]_200_iterations";
pub const SPLITTER_ID: &str =
    "a_comp_sentences_v1:terminal_punct_closers_then_whitespace_or_blank_line_or_end";
pub const DECODER_RULE: &str = "slop_ninja_span_decoder_v1";
pub const EMISSION_RULE: &str = "slop_ninja_span_emission_v1";
pub const SCORE_MAP_BOUND: &str = "every PAV level and every output clamped to [lo,hi]";
const P_CLAMP: f64 = 1e-9;
const S_CLAMP: f64 = 1e-6;
const TILT_BOUND: f64 = 40.;
const TILT_ITERATIONS: usize = 200;
pub const THRESHOLD_MARGIN: f64 = 1e-6;

/// Python `str.isspace`: Unicode White_Space plus U+001C..U+001F.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn gelu_tanh(x: f64) -> f64 {
    const K: f64 = 0.7978845608028654; // sqrt(2/pi)
    0.5 * x * (1. + (K * (x + 0.044715 * x * x * x)).tanh())
}

/// spans-v1 token head: t = w2 . gelu_tanh(W1 x + b1) + b2 on final-norm token states.
pub struct TokenHead {
    first: Linear,
    second: Linear,
    pub hidden: usize,
}

impl TokenHead {
    pub(crate) fn take(
        weights: &mut BTreeMap<String, Tensor>,
        hidden: usize,
        input: usize,
    ) -> Result<Self> {
        ensure!((1..=4096).contains(&hidden), "token head width");
        Ok(Self {
            first: Linear::take(weights, "token_head.first", hidden, input)?,
            second: Linear::take(weights, "token_head.second", 1, hidden)?,
            hidden,
        })
    }

    pub fn logit(&self, x: &[f64]) -> f64 {
        let h = self
            .first
            .apply(x)
            .into_iter()
            .map(gelu_tanh)
            .collect::<Vec<_>>();
        self.second.apply(&h)[0]
    }

    /// Per-position logits for row-major `tokens` (count x width); positions
    /// outside `word_mask` are not evaluated and return None.
    pub fn logits(&self, tokens: &[f64], width: usize, word_mask: &[bool]) -> Result<Vec<Option<f64>>> {
        ensure!(
            tokens.len() == word_mask.len() * width,
            "token state shape"
        );
        tokens
            .chunks_exact(width)
            .zip(word_mask)
            .map(|(x, keep)| {
                if !keep {
                    return Ok(None);
                }
                ensure!(x.iter().all(|v| v.is_finite()), "nonfinite token state");
                let t = self.logit(x);
                ensure!(t.is_finite(), "nonfinite token logit");
                Ok(Some(t))
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Segment {
    pub start_char: usize,
    pub end_char: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

/// Character index -> byte offset table (len chars + 1).
fn byte_starts(text: &str) -> Vec<usize> {
    let mut out = Vec::with_capacity(text.len() + 1);
    for (b, _) in text.char_indices() {
        out.push(b);
    }
    out.push(text.len());
    out
}

/// Share words: maximal runs of non-[`is_space`] characters.
pub fn words(text: &str) -> Vec<Segment> {
    let starts = byte_starts(text);
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.chars().enumerate() {
        if is_space(c) {
            if let Some(s) = start.take() {
                out.push(segment(&starts, s, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push(segment(&starts, s, starts.len() - 1));
    }
    out
}

fn segment(starts: &[usize], a: usize, b: usize) -> Segment {
    Segment {
        start_char: a,
        end_char: b,
        start_byte: starts[a],
        end_byte: starts[b],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sentence {
    pub start: usize,
    pub end: usize,
    pub punctuated: bool,
}

fn is_terminal(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…')
}
fn is_closer(c: char) -> bool {
    matches!(c, '"' | '\'' | ')' | ']' | '”' | '’' | '»' | '}')
}

/// Sealed sentence splitter `a_comp_sentences_v1`, byte ranges. A verbatim
/// copy of `grammar_a_positives::text::sentences` (equivalence is tested in
/// that crate): a sentence ends after a run of terminal punctuation and any
/// closers when whitespace or the text end follows; at a blank line; or at the
/// end. Sentences exclude surrounding whitespace.
pub fn sentences(text: &str) -> Vec<Sentence> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut last_non_ws_end = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let (pos, c) = chars[i];
        if c.is_whitespace() {
            if c == '\n' && start.is_some() {
                let mut k = i + 1;
                let mut blank = false;
                while k < chars.len() && chars[k].1.is_whitespace() {
                    if chars[k].1 == '\n' {
                        blank = true;
                        break;
                    }
                    k += 1;
                }
                if blank {
                    out.push(Sentence {
                        start: start.take().unwrap(),
                        end: last_non_ws_end,
                        punctuated: false,
                    });
                }
            }
            i += 1;
            continue;
        }
        if start.is_none() {
            start = Some(pos);
        }
        last_non_ws_end = pos + c.len_utf8();
        if is_terminal(c) {
            let mut k = i + 1;
            while k < chars.len() && (is_terminal(chars[k].1) || is_closer(chars[k].1)) {
                k += 1;
            }
            let end = if k < chars.len() { chars[k].0 } else { text.len() };
            if k == chars.len() || chars[k].1.is_whitespace() {
                out.push(Sentence {
                    start: start.take().unwrap(),
                    end,
                    punctuated: true,
                });
                last_non_ws_end = end;
                i = k;
                continue;
            }
            last_non_ws_end = end;
            i = k;
            continue;
        }
        i += 1;
    }
    if let Some(s) = start {
        out.push(Sentence {
            start: s,
            end: last_non_ws_end,
            punctuated: false,
        });
    }
    out
}

/// Per word, the positions of word-bearing tokens overlapping its characters.
/// `offsets` are character offsets (the tokenizer's char offsets).
pub fn word_groups(
    text_chars: usize,
    words: &[Segment],
    offsets: &[(usize, usize)],
    word_mask: &[bool],
) -> Result<Vec<Vec<usize>>> {
    ensure!(offsets.len() == word_mask.len(), "token/offset length mismatch");
    let mut owner = vec![usize::MAX; text_chars];
    for (w, s) in words.iter().enumerate() {
        ensure!(s.end_char <= text_chars, "word beyond text");
        owner[s.start_char..s.end_char].fill(w);
    }
    let mut groups = vec![Vec::new(); words.len()];
    for (j, (&(a, b), keep)) in offsets.iter().zip(word_mask).enumerate() {
        ensure!(a <= b && b <= text_chars, "token offset out of range");
        if !keep {
            continue;
        }
        let mut seen: Vec<usize> = Vec::new();
        for &w in &owner[a..b] {
            if w != usize::MAX && !seen.contains(&w) {
                seen.push(w);
            }
        }
        for w in seen {
            groups[w].push(j);
        }
    }
    ensure!(
        groups.iter().all(|g| !g.is_empty()),
        "a word has no word-bearing token (span emission unsupported)"
    );
    Ok(groups)
}

/// Per emitted sentence (those with words): (sentence index, first word, end word).
pub fn sentence_word_ranges(
    sentences: &[Sentence],
    words: &[Segment],
) -> Result<Vec<(usize, usize, usize)>> {
    let mut out = Vec::new();
    let mut w = 0;
    for (s, sent) in sentences.iter().enumerate() {
        let first = w;
        while w < words.len() && words[w].start_byte < sent.end {
            ensure!(
                words[w].start_byte >= sent.start && words[w].end_byte <= sent.end,
                "word outside its sentence"
            );
            w += 1;
        }
        if w > first {
            out.push((s, first, w));
        }
    }
    ensure!(w == words.len(), "word outside every sentence");
    Ok(out)
}

fn logit(p: f64) -> f64 {
    p.ln() - (-p).ln_1p()
}

/// Share tilt: (b, clamped share target, tilted word probabilities).
pub fn tilt(p: &[f64], share: f64) -> Result<(f64, f64, Vec<f64>)> {
    ensure!(
        !p.is_empty() && p.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
        "word probabilities"
    );
    ensure!(share.is_finite(), "document share");
    let s = share.clamp(S_CLAMP, 1. - S_CLAMP);
    let lg = p
        .iter()
        .map(|v| logit(v.clamp(P_CLAMP, 1. - P_CLAMP)))
        .collect::<Vec<_>>();
    let (mut lo, mut hi) = (-TILT_BOUND, TILT_BOUND);
    for _ in 0..TILT_ITERATIONS {
        let mid = 0.5 * (lo + hi);
        let f = lg.iter().map(|v| sigmoid(v + mid)).sum::<f64>() / lg.len() as f64;
        if f < s {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let b = 0.5 * (lo + hi);
    Ok((b, s, lg.iter().map(|v| sigmoid(v + b)).collect()))
}

/// `slop_ninja_span_decoder_v1` parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoder {
    pub tau_hi: f64,
    pub delta: f64,
    pub min_words: usize,
    pub gap: usize,
    pub snap: Option<f64>,
}

impl Decoder {
    pub fn from_json(v: &Value) -> Result<Self> {
        let o = v.as_object().context("decoder object")?;
        ensure!(
            o.len() == 6
                && v["rule"] == DECODER_RULE
                && ["tau_hi", "delta", "min_words", "gap", "snap"]
                    .iter()
                    .all(|k| o.contains_key(*k)),
            "decoder fields"
        );
        let tau_hi = v["tau_hi"].as_f64().context("tau_hi")?;
        let delta = v["delta"].as_f64().context("delta")?;
        ensure!(
            tau_hi > 0. && tau_hi < 1. && delta >= 0. && delta < tau_hi,
            "decoder thresholds"
        );
        let min_words = v["min_words"].as_u64().context("min_words")? as usize;
        let gap = v["gap"].as_u64().context("gap")? as usize;
        ensure!(min_words >= 1, "decoder lengths");
        let snap = match &v["snap"] {
            Value::Null => None,
            x => {
                let s = x.as_f64().context("snap")?;
                ensure!(s > 0. && s <= 1., "decoder snap");
                Some(s)
            }
        };
        Ok(Self {
            tau_hi,
            delta,
            min_words,
            gap,
            snap,
        })
    }

    pub fn tau_lo(&self) -> f64 {
        self.tau_hi - self.delta
    }

    /// Word runs [first, end): hysteresis, gap merge, minimum length, snap.
    pub fn decode(&self, q: &[f64], sentence_ranges: &[(usize, usize, usize)]) -> Vec<(usize, usize)> {
        let (hi, lo, n) = (self.tau_hi, self.tau_lo(), q.len());
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut i = 0;
        while i < n {
            if q[i] >= lo {
                let mut j = i;
                while j < n && q[j] >= lo {
                    j += 1;
                }
                if q[i..j].iter().any(|v| *v >= hi) {
                    runs.push((i, j));
                }
                i = j;
            } else {
                i += 1;
            }
        }
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for r in runs {
            match merged.last_mut() {
                Some(last) if r.0 - last.1 <= self.gap => last.1 = r.1,
                _ => merged.push(r),
            }
        }
        let runs = merged
            .into_iter()
            .filter(|r| r.1 - r.0 >= self.min_words)
            .collect::<Vec<_>>();
        let Some(snap) = self.snap else {
            return runs;
        };
        if runs.is_empty() {
            return runs;
        }
        let mut covered = vec![false; n];
        for &(a, b) in &runs {
            covered[a..b].fill(true);
        }
        for &(_, sa, sb) in sentence_ranges {
            let size = (sb - sa) as f64;
            for &(a, b) in &runs {
                let c = b.min(sb) as isize - a.max(sa) as isize;
                if c > 0 && c as f64 >= snap * size {
                    covered[sa..sb].fill(true);
                }
            }
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < n {
            if covered[i] {
                let mut j = i;
                while j < n && covered[j] {
                    j += 1;
                }
                out.push((i, j));
                i = j;
            } else {
                i += 1;
            }
        }
        out
    }

    /// A decision value within 1e-6 of tau_hi or tau_lo (excluded from R1 offset equality).
    pub fn near_threshold(&self, q: &[f64]) -> bool {
        let (hi, lo) = (self.tau_hi, self.tau_lo());
        q.iter()
            .any(|v| (v - hi).abs() < THRESHOLD_MARGIN || (v - lo).abs() < THRESHOLD_MARGIN)
    }
}

/// Sentence / span confidence map: identity, or a clipped isotonic score map.
#[derive(Clone, Debug, PartialEq)]
pub enum ScoreMap {
    Identity,
    Clipped { knots: Vec<Knot>, lo: f64, hi: f64 },
}

impl ScoreMap {
    pub fn from_json(v: &Value) -> Result<Self> {
        let o = v.as_object().context("score map object")?;
        match v["method"].as_str() {
            Some("identity_v1") => {
                ensure!(o.len() == 1, "identity map fields");
                Ok(Self::Identity)
            }
            Some("isotonic_score_v1_clipped") => {
                ensure!(
                    o.len() == 7
                        && v["bound"] == SCORE_MAP_BOUND
                        && v["input"].is_string()
                        && v["target"].is_string(),
                    "clipped score map fields"
                );
                let lo = v["lo"].as_f64().context("lo")?;
                let hi = v["hi"].as_f64().context("hi")?;
                ensure!(lo > 0. && lo <= 0.5 && hi >= 0.5 && hi < 1., "score map bounds");
                let mut knots = Vec::new();
                for k in v["knots"].as_array().context("knots")? {
                    let ko = k.as_object().context("knot")?;
                    ensure!(ko.len() == 2, "knot fields");
                    knots.push(Knot {
                        score: k["score"].as_f64().context("score")?,
                        involvement: k["value"].as_f64().context("value")?,
                        weight: 0.,
                        weighted_positives: 0.,
                        rows: 0,
                    });
                }
                ensure!(
                    !knots.is_empty()
                        && knots.windows(2).all(|w| w[0].score < w[1].score
                            && w[0].involvement <= w[1].involvement)
                        && knots.iter().all(|k| k.score.is_finite()
                            && k.involvement >= lo
                            && k.involvement <= hi),
                    "score map knots must be sorted, monotone and inside [lo,hi]"
                );
                Ok(Self::Clipped { knots, lo, hi })
            }
            _ => bail!("unsupported score map"),
        }
    }

    pub fn apply(&self, x: f64) -> f64 {
        match self {
            Self::Identity => x,
            Self::Clipped { knots, lo, hi } => isotonic_predict_score(knots, x).clamp(*lo, *hi),
        }
    }
}

/// The span emission spec carried by heads v2 (`span_emission`).
#[derive(Clone, Debug)]
pub struct Spec {
    pub decoder: Decoder,
    pub sentence_confidence: ScoreMap,
    pub span_confidence: ScoreMap,
    pub description: Value,
}

impl Spec {
    pub fn from_json(v: &Value) -> Result<Self> {
        let o = v.as_object().context("span emission object")?;
        ensure!(
            o.len() == 8
                && v["rule"] == EMISSION_RULE
                && v["word_rule"] == WORD_RULE
                && v["word_probability_rule"] == WORD_PROBABILITY_RULE
                && v["tilt_rule"] == TILT_RULE
                && v["splitter"] == SPLITTER_ID,
            "span emission rule identifiers"
        );
        Ok(Self {
            decoder: Decoder::from_json(&v["decoder"])?,
            sentence_confidence: ScoreMap::from_json(&v["sentence_confidence"])?,
            span_confidence: ScoreMap::from_json(&v["span_confidence"])?,
            description: v.clone(),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SentenceOutput {
    pub start_char: usize,
    pub end_char: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub words: usize,
    pub punctuated: bool,
    pub ai_share_percent: f64,
    pub ai_confidence_percent: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SpanOutput {
    pub start_char: usize,
    pub end_char: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub words: usize,
    pub ai_share_percent: f64,
    pub ai_confidence_percent: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Diagnostics {
    pub word_offsets: Vec<[usize; 2]>,
    pub token_probabilities: Vec<Option<f64>>,
    pub word_probabilities_raw: Vec<f64>,
    pub word_probabilities: Vec<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Emission {
    pub rule: &'static str,
    pub words: usize,
    pub tilt_bias: f64,
    pub share_target: f64,
    pub near_threshold: bool,
    pub sentences: Vec<SentenceOutput>,
    pub spans: Vec<SpanOutput>,
    pub diagnostics: Diagnostics,
}

/// Spans and sentences for one request. `token_logits` holds a logit for every
/// word-bearing position (None elsewhere); `share` and `confidence` are the
/// emitted document share and AI confidence on the 0..1 scale.
pub fn emit(
    text: &str,
    offsets: &[(usize, usize)],
    word_mask: &[bool],
    token_logits: &[Option<f64>],
    share: f64,
    confidence: f64,
    spec: &Spec,
) -> Result<Emission> {
    ensure!(
        token_logits.len() == offsets.len() && offsets.len() == word_mask.len(),
        "token arrays differ in length"
    );
    ensure!(
        confidence.is_finite() && (0.0..=1.0).contains(&confidence),
        "document confidence"
    );
    let n_chars = text.chars().count();
    let ws = words(text);
    ensure!(!ws.is_empty(), "no words");
    let groups = word_groups(n_chars, &ws, offsets, word_mask)?;
    let token_probabilities = token_logits
        .iter()
        .zip(word_mask)
        .map(|(t, keep)| match (t, keep) {
            (Some(t), true) => Ok(Some(sigmoid(*t))),
            (None, false) => Ok(None),
            _ => bail!("token logits must cover exactly the word-bearing positions"),
        })
        .collect::<Result<Vec<_>>>()?;
    let p = groups
        .iter()
        .map(|g| {
            g.iter()
                .map(|j| token_probabilities[*j].unwrap())
                .sum::<f64>()
                / g.len() as f64
        })
        .collect::<Vec<_>>();
    let (b, target, q) = tilt(&p, share)?;
    let sents = sentences(text);
    let ranges = sentence_word_ranges(&sents, &ws)?;
    let starts = byte_starts(text);
    let char_of = |byte: usize| starts.partition_point(|b| *b < byte);
    let mut sentence_rows = Vec::new();
    for &(s, a, e) in &ranges {
        let sent = sents[s];
        let mean = q[a..e].iter().sum::<f64>() / (e - a) as f64;
        let max = q[a..e].iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let conf = spec.sentence_confidence.apply(max).min(confidence);
        sentence_rows.push(SentenceOutput {
            start_char: char_of(sent.start),
            end_char: char_of(sent.end),
            start_byte: sent.start,
            end_byte: sent.end,
            words: e - a,
            punctuated: sent.punctuated,
            ai_share_percent: 100. * mean,
            ai_confidence_percent: 100. * conf,
        });
    }
    let mut span_rows = Vec::new();
    for (a, e) in spec.decoder.decode(&q, &ranges) {
        let mean = q[a..e].iter().sum::<f64>() / (e - a) as f64;
        let conf = spec.span_confidence.apply(mean).min(confidence);
        span_rows.push(SpanOutput {
            start_char: ws[a].start_char,
            end_char: ws[e - 1].end_char,
            start_byte: ws[a].start_byte,
            end_byte: ws[e - 1].end_byte,
            words: e - a,
            ai_share_percent: 100. * mean,
            ai_confidence_percent: 100. * conf,
        });
    }
    Ok(Emission {
        rule: EMISSION_RULE,
        words: ws.len(),
        tilt_bias: b,
        share_target: target,
        near_threshold: spec.decoder.near_threshold(&q),
        sentences: sentence_rows,
        spans: span_rows,
        diagnostics: Diagnostics {
            word_offsets: ws.iter().map(|w| [w.start_char, w.end_char]).collect(),
            token_probabilities,
            word_probabilities_raw: p,
            word_probabilities: q,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const GOLDEN: &str = include_str!("../tests/golden/span-golden-v1.json");

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn whitespace_rules_match_python() {
        for c in ['\u{1c}', '\u{1d}', '\u{1e}', '\u{1f}'] {
            assert!(is_space(c) && !c.is_whitespace());
        }
        for c in ['\u{200b}', '\u{feff}', '\u{180e}', 'a'] {
            assert!(!is_space(c));
        }
        for c in [' ', '\t', '\n', '\r', '\u{85}', '\u{a0}', '\u{2028}', '\u{3000}'] {
            assert!(is_space(c));
        }
    }

    #[test]
    fn words_and_sentences_on_synthetic_text() {
        let text = "  ab c\u{1f}d\n";
        let w = words(text);
        assert_eq!(
            w.iter().map(|s| (s.start_char, s.end_char)).collect::<Vec<_>>(),
            vec![(2, 4), (5, 6), (7, 8)]
        );
        let t = "One. Two!\" (Three?) Four\n\nFive.Six\r\n \r\nSeven";
        let got = sentences(t)
            .iter()
            .map(|s| (&t[s.start..s.end], s.punctuated))
            .collect::<Vec<_>>();
        assert_eq!(
            got,
            vec![
                ("One.", true),
                ("Two!\"", true),
                ("(Three?)", true),
                ("Four", false),
                ("Five.Six", false),
                ("Seven", false)
            ]
        );
    }

    #[test]
    fn tilt_hits_the_share() {
        let p = [0.01, 0.2, 0.5, 0.8, 0.999999];
        for s in [0., 1e-7, 0.1, 0.5, 0.93, 1.] {
            let (_, target, q) = tilt(&p, s).unwrap();
            assert!(close(q.iter().sum::<f64>() / q.len() as f64, target, 1e-12));
            assert!(q.windows(2).all(|w| w[0] <= w[1]));
        }
        assert!(tilt(&[], 0.5).is_err() && tilt(&[0.5], f64::NAN).is_err());
    }

    #[test]
    fn decoder_hysteresis_gap_min_and_snap() {
        let d = |hi: f64, delta: f64, m: usize, g: usize, snap: Option<f64>| Decoder {
            tau_hi: hi,
            delta,
            min_words: m,
            gap: g,
            snap,
        };
        let q = [0.1, 0.6, 0.2, 0.7, 0.7, 0.1, 0.1, 0.1, 0.55];
        let one = [(0, 0, 9)];
        assert_eq!(d(0.5, 0., 1, 0, None).decode(&q, &one), vec![(1, 2), (3, 5), (8, 9)]);
        assert_eq!(d(0.5, 0., 1, 1, None).decode(&q, &one), vec![(1, 5), (8, 9)]);
        assert_eq!(d(0.5, 0., 1, 3, None).decode(&q, &one), vec![(1, 9)]);
        assert_eq!(d(0.5, 0., 2, 0, None).decode(&q, &one), vec![(3, 5)]);
        assert_eq!(d(0.6, 0.4, 1, 0, None).decode(&q, &one), vec![(1, 5)]);
        let q = [0.9, 0.9, 0.9, 0.9, 0.1, 0.1, 0.9];
        let snap = d(0.5, 0., 1, 0, Some(0.8));
        assert_eq!(snap.decode(&q, &[(0, 0, 5), (1, 5, 7)]), vec![(0, 5), (6, 7)]);
        assert_eq!(snap.decode(&q, &[(0, 0, 6), (1, 6, 7)]), vec![(0, 4), (6, 7)]);
        assert!(d(0.5, 0., 1, 0, None).near_threshold(&[0.5000004]));
        assert!(!d(0.5, 0., 1, 0, None).near_threshold(&[0.50001]));
    }

    #[test]
    fn rejects_bad_specs_and_uncovered_words() {
        assert!(Decoder::from_json(&json!({"rule":DECODER_RULE,"tau_hi":0.5,"delta":0.5,
            "min_words":1,"gap":0,"snap":null}))
        .is_err());
        assert!(ScoreMap::from_json(&json!({"method":"platt"})).is_err());
        let w = words("ab cd");
        assert!(word_groups(5, &w, &[(0, 0), (0, 2), (0, 0)], &[false, true, false]).is_err());
        assert_eq!(
            word_groups(5, &w, &[(0, 0), (0, 2), (2, 5), (0, 0)], &[false, true, true, false])
                .unwrap(),
            vec![vec![1], vec![2]]
        );
    }

    /// Cross-language golden vectors from the Python reference (synthetic text and logits).
    #[test]
    fn golden_vectors_match_the_python_reference() {
        let g: Value = serde_json::from_str(GOLDEN).unwrap();
        assert_eq!(g["schema"], "slop_ninja_span_emission_golden_v1");
        assert_eq!(g["rules"]["emission"], EMISSION_RULE);
        assert_eq!(g["rules"]["tilt"], TILT_RULE);
        let cases = g["cases"].as_array().unwrap();
        assert!(cases.len() >= 36);
        let mut with_spans = 0;
        for c in cases {
            let id = c["id"].as_str().unwrap();
            let text = c["text"].as_str().unwrap();
            let offsets = c["token_offsets"]
                .as_array()
                .unwrap()
                .iter()
                .map(|o| (o[0].as_u64().unwrap() as usize, o[1].as_u64().unwrap() as usize))
                .collect::<Vec<_>>();
            let mask = c["word_mask"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_bool().unwrap())
                .collect::<Vec<_>>();
            let logits = c["token_logits"]
                .as_array()
                .unwrap()
                .iter()
                .zip(&mask)
                .map(|(v, k)| k.then(|| v.as_f64().unwrap()))
                .collect::<Vec<_>>();
            let spec = Spec::from_json(&c["spec"]).unwrap();
            let e = emit(
                text,
                &offsets,
                &mask,
                &logits,
                c["share"].as_f64().unwrap(),
                c["confidence"].as_f64().unwrap(),
                &spec,
            )
            .unwrap();
            let x = &c["expected"];
            assert_eq!(e.words as u64, x["words"].as_u64().unwrap(), "{id}");
            assert!(!e.near_threshold && x["near_threshold"] == false, "{id}");
            assert!(close(e.tilt_bias, x["tilt_bias"].as_f64().unwrap(), 1e-9), "{id}");
            let xd = &x["diagnostics"];
            assert_eq!(serde_json::to_value(&e.diagnostics.word_offsets).unwrap(), xd["word_offsets"], "{id}");
            for (a, b) in e
                .diagnostics
                .word_probabilities
                .iter()
                .zip(xd["word_probabilities"].as_array().unwrap())
            {
                assert!(close(*a, b.as_f64().unwrap(), 1e-12), "{id}");
            }
            for (a, b) in e
                .diagnostics
                .token_probabilities
                .iter()
                .zip(xd["token_probabilities"].as_array().unwrap())
            {
                match (a, b.as_f64()) {
                    (Some(a), Some(b)) => assert!(close(*a, b, 1e-15), "{id}"),
                    (None, None) => {}
                    _ => panic!("{id}: token probability presence"),
                }
            }
            for (rows, key) in [
                (serde_json::to_value(&e.sentences).unwrap(), "sentences"),
                (serde_json::to_value(&e.spans).unwrap(), "spans"),
            ] {
                let (rows, want) = (rows.as_array().unwrap().clone(), x[key].as_array().unwrap());
                assert_eq!(rows.len(), want.len(), "{id} {key}");
                for (r, w) in rows.iter().zip(want) {
                    for k in ["start_char", "end_char", "start_byte", "end_byte", "words"] {
                        assert_eq!(r[k], w[k], "{id} {key} {k}");
                    }
                    for k in ["ai_share_percent", "ai_confidence_percent"] {
                        assert!(close(r[k].as_f64().unwrap(), w[k].as_f64().unwrap(), 1e-10), "{id} {key} {k}");
                    }
                }
            }
            with_spans += usize::from(!e.spans.is_empty());
        }
        assert!(with_spans > 0 && with_spans < cases.len());
    }
}
