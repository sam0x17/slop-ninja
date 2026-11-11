//! What the page may say about a text, from A's measured evidence.
//!
//! - **Length.** The four bands' human false-positive rates were measured on
//!   texts of 150 to 450 whitespace words (the human pools' word count). A
//!   longer text is scored with a label. From 24 to 149 words the page gives
//!   one positive result under a per-bin rule measured on excerpts of the
//!   consumed Test and chosen on the Calibration split
//!   (`docs/detect-evidence.md`, short texts); under 24 words, the span floor,
//!   it gives no score.
//! - **Bands.** Four evidence bands on the calibrated involvement
//!   probability, cut at the release's Calibration-split operating points
//!   (`docs/a-baseline-release.md`): the 1% human-FPR point and the 0.1%
//!   point (the top isotonic level), and 50%. The scorer prints
//!   `ai_confidence_percent` as the probability times 100, and multiplying
//!   by 100 is monotone in floating point, so comparing the percentage with
//!   each cut times 100, computed the same way, is the same comparison.
//! - **Short texts.** Below 150 words the release's confidence is not
//!   calibrated and its cuts flag too many human texts, so the result is
//!   "strong signs" only when the document confidence reaches a release
//!   level and a highlighted passage's own confidence (the span confidence
//!   map, a separate calibrated output) reaches a level, both per bin.
//!   Otherwise it is "no strong signs". No percentage, share or very strong
//!   band is shown. The rule reads passages, so with spans off or failed a
//!   short text gets the span-free minimum of 150 words instead.
//! - **Spans.** Passages are shown only in the two strong bands (below 150
//!   words, only in the strong result), only when the span switch is on, and
//!   never under 24 words.
//! - **Notices.** Control markers the tokenizer reads as special tokens, and
//!   invisible or control characters, are named, never removed.

/// The 1% human-FPR operating point on the Calibration split.
pub const STRONG_CUT: f64 = 0.9090909090909091;
/// The 0.1% point, the top clipped isotonic level.
pub const VERY_STRONG_CUT: f64 = 0.9998820476527482;
/// Below this the result is in the weak band.
pub const UNCLEAR_CUT: f64 = 0.5;
/// The span floor: no passage under 24 words is ever shown.
pub const SPAN_MIN_WORDS: u64 = 24;
/// A level of package d24's span confidence map: the passage condition of
/// the short-text rule at 24 to 99 words.
pub const SHORT_SPAN_LEVEL: f64 = 0.9637746785002717;
/// The span confidence map level of the 100-149-word rule.
pub const SHORT_SPAN_LEVEL_100: f64 = 0.9420289855072465;

/// The raw added tokens of the pinned tokenizer (its special tokens), which
/// the scorer splits out of the text before normalization. A test checks
/// the list against the compiled-in table.
pub const MARKERS: &[&str] = &["<|padding|>", "<|endoftext|>", "[UNK]", "[CLS]", "[SEP]", "[PAD]", "[MASK]"];

/// An evidence band, or a short text's result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    Weak,
    Unclear,
    Strong,
    VeryStrong,
    /// A short text (24 to 149 words) that does not meet its bin's rule.
    /// Short texts get this or [`Band::Strong`], never another band.
    NoStrongSigns,
}

impl Band {
    /// The band of a scorer's `ai_confidence_percent`.
    pub fn of_percent(p: f64) -> Option<Band> {
        if !p.is_finite() || !(0.0..=100.0).contains(&p) {
            return None;
        }
        Some(if p >= VERY_STRONG_CUT * 100. {
            Band::VeryStrong
        } else if p >= STRONG_CUT * 100. {
            Band::Strong
        } else if p >= UNCLEAR_CUT * 100. {
            Band::Unclear
        } else {
            Band::Weak
        })
    }

    pub fn code(self) -> &'static str {
        match self {
            Band::VeryStrong => "very_strong",
            Band::Strong => "strong",
            Band::Unclear => "unclear",
            Band::Weak => "weak",
            Band::NoStrongSigns => "no_strong_signs",
        }
    }

    pub fn from_code(s: &str) -> Option<Band> {
        [Band::VeryStrong, Band::Strong, Band::Unclear, Band::Weak, Band::NoStrongSigns].into_iter().find(|b| b.code() == s)
    }

    /// Whether passages may be highlighted in this band.
    pub fn shows_spans(self) -> bool {
        matches!(self, Band::Strong | Band::VeryStrong)
    }
}

/// Where a text's length sits against the measured ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LengthBin {
    /// 24 to 49 words: the short-text rule.
    Short24,
    /// 50 to 99 words: the short-text rule.
    Short50,
    /// 100 to 149 words: the short-text rule.
    Short100,
    /// 150 to 450 words: human false positives measured.
    Measured,
    /// 451 words to 2,048 tokens: scored, labelled.
    Longer,
}

/// A short bin's rule: the result is "strong signs" when the document's
/// `ai_confidence_percent` reaches `confidence_level` × 100 and some
/// passage's `ai_confidence_percent` reaches `span_level` × 100, compared as
/// [`Band::of_percent`] compares. Each pair was chosen on the Calibration
/// split by the release's 1% rule (the most AI excerpts flagged with at most
/// 1% of human excerpts) and measured on the consumed Test. Both levels are
/// package d24's: a release isotonic level and a span confidence map level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShortRule {
    pub confidence_level: f64,
    pub span_level: f64,
}

impl ShortRule {
    /// Whether a document confidence and its passages' confidences meet
    /// the rule.
    pub fn strong(self, confidence_percent: f64, span_percents: impl IntoIterator<Item = f64>) -> bool {
        confidence_percent >= self.confidence_level * 100.
            && span_percents.into_iter().any(|p| p >= self.span_level * 100.)
    }
}

impl LengthBin {
    pub fn code(self) -> &'static str {
        match self {
            LengthBin::Short24 => "24_49",
            LengthBin::Short50 => "50_99",
            LengthBin::Short100 => "100_149",
            LengthBin::Measured => "150_450",
            LengthBin::Longer => "over_450",
        }
    }

    pub fn from_code(s: &str) -> Option<LengthBin> {
        [LengthBin::Short24, LengthBin::Short50, LengthBin::Short100, LengthBin::Measured, LengthBin::Longer]
            .into_iter()
            .find(|b| b.code() == s)
    }

    /// The short-text rule of a bin under 150 words, `None` from 150 on.
    pub fn short_rule(self) -> Option<ShortRule> {
        match self {
            LengthBin::Short24 | LengthBin::Short50 => {
                Some(ShortRule { confidence_level: VERY_STRONG_CUT, span_level: SHORT_SPAN_LEVEL })
            }
            LengthBin::Short100 => Some(ShortRule { confidence_level: STRONG_CUT, span_level: SHORT_SPAN_LEVEL_100 }),
            LengthBin::Measured | LengthBin::Longer => None,
        }
    }

    pub fn is_short(self) -> bool {
        self.short_rule().is_some()
    }
}

/// Whitespace words, the human pools' count (`grammar_a_form::text::words`).
pub fn words(s: &str) -> usize {
    s.split_whitespace().count()
}

/// The length decision before any scoring: `Err(words)` when too short.
pub fn length_bin(words: usize) -> Result<LengthBin, usize> {
    use crate::bound::{MEASURED_MAX_WORDS, MEASURED_MIN_WORDS, MIN_WORDS};
    Ok(match words {
        w if w < MIN_WORDS => return Err(words),
        w if w < 50 => LengthBin::Short24,
        w if w < 100 => LengthBin::Short50,
        w if w < MEASURED_MIN_WORDS => LengthBin::Short100,
        w if w <= MEASURED_MAX_WORDS => LengthBin::Measured,
        _ => LengthBin::Longer,
    })
}

/// The fewest words the page scores: 24 with spans on, and 150 with spans
/// off, since the short-text rule reads passages.
pub fn min_words(spans_on: bool) -> usize {
    if spans_on { crate::bound::MIN_WORDS } else { crate::bound::MEASURED_MIN_WORDS }
}

/// The control markers present in `s`, in [`MARKERS`] order.
pub fn markers_in(s: &str) -> impl Iterator<Item = &'static str> + '_ {
    MARKERS.iter().copied().filter(move |m| s.contains(m))
}

/// Whether `s` holds an invisible or control character other than tab,
/// line feed and carriage return: C0 and C1 controls, and the format
/// characters the page cannot show (zero-width, direction marks, word
/// joiner, byte-order mark, soft hyphen).
pub fn has_invisible(s: &str) -> bool {
    s.chars().any(|c| {
        let u = c as u32;
        (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
            || matches!(u, 0xAD | 0x180E | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0xFEFF)
    })
}

/// A percentage floored to tenths, as an integer number of tenths.
pub fn tenths_floor(p: f64) -> u16 {
    if !p.is_finite() || p <= 0.0 {
        return 0;
    }
    let t = (p * 10.0).floor();
    if t >= 1000.0 { 1000 } else { t as u16 }
}

/// The confidence as the page prints it: floored to one decimal, "above
/// 99.9%" at the top and never 100%, "below 1%" at the bottom.
pub fn confidence_text(p: f64) -> String {
    let t = tenths_floor(p);
    if p > 99.9 {
        "above 99.9%".into()
    } else if t < 10 {
        "below 1%".into()
    } else {
        format!("{}.{}%", t / 10, t % 10)
    }
}

/// The share as the page prints it: whole percent, rounded down so the
/// share never reads above the confidence, "below 1%" under one.
pub fn share_text(share: f64, confidence: f64) -> String {
    let s = if share.is_finite() { share.min(confidence).max(0.0) } else { 0.0 };
    if s < 1.0 { "below 1%".into() } else { format!("about {}%", s.floor() as u32) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_cut_at_the_operating_points() {
        assert_eq!(Band::of_percent(STRONG_CUT * 100.), Some(Band::Strong));
        assert_eq!(Band::of_percent(VERY_STRONG_CUT * 100.), Some(Band::VeryStrong));
        // The scorer prints probability × 100; the top level lands in the top band.
        assert_eq!(Band::of_percent(0.9998820476527482 * 100.), Some(Band::VeryStrong));
        assert_eq!(Band::of_percent(0.9090909090909091 * 100.), Some(Band::Strong));
        assert_eq!(Band::of_percent(90.90), Some(Band::Unclear));
        assert_eq!(Band::of_percent(50.0), Some(Band::Unclear));
        assert_eq!(Band::of_percent(49.99), Some(Band::Weak));
        assert_eq!(Band::of_percent(0.0), Some(Band::Weak));
        assert_eq!(Band::of_percent(f64::NAN), None);
        assert_eq!(Band::of_percent(100.5), None);
        assert!(Band::Strong.shows_spans() && Band::VeryStrong.shows_spans());
        assert!(!Band::Unclear.shows_spans() && !Band::Weak.shows_spans());
        for b in [Band::VeryStrong, Band::Strong, Band::Unclear, Band::Weak, Band::NoStrongSigns] {
            assert_eq!(Band::from_code(b.code()), Some(b));
        }
    }

    #[test]
    fn length_policy() {
        assert_eq!(length_bin(0), Err(0));
        assert_eq!(length_bin(23), Err(23));
        assert_eq!(length_bin(24), Ok(LengthBin::Short24));
        assert_eq!(length_bin(49), Ok(LengthBin::Short24));
        assert_eq!(length_bin(50), Ok(LengthBin::Short50));
        assert_eq!(length_bin(99), Ok(LengthBin::Short50));
        assert_eq!(length_bin(100), Ok(LengthBin::Short100));
        assert_eq!(length_bin(149), Ok(LengthBin::Short100));
        assert_eq!(length_bin(150), Ok(LengthBin::Measured));
        assert_eq!(length_bin(450), Ok(LengthBin::Measured));
        assert_eq!(length_bin(451), Ok(LengthBin::Longer));
        assert_eq!(words("  one\ttwo\nthree  "), 3);
        assert_eq!((min_words(true), min_words(false)), (24, 150));
        for b in [LengthBin::Short24, LengthBin::Short50, LengthBin::Short100, LengthBin::Measured, LengthBin::Longer] {
            assert_eq!(LengthBin::from_code(b.code()), Some(b));
            assert_eq!(b.is_short(), !matches!(b, LengthBin::Measured | LengthBin::Longer));
        }
        // The span floor is the shortest scored text, so a text under 48
        // words holds at most one passage.
        assert_eq!(crate::bound::MIN_WORDS as u64, SPAN_MIN_WORDS);
    }

    /// The short rules as measured (`docs/detect-evidence.md`): both levels
    /// compared as the bands compare, each condition necessary.
    #[test]
    fn short_rules() {
        let top = VERY_STRONG_CUT * 100.;
        let span = SHORT_SPAN_LEVEL * 100.;
        // The scorer prints level × 100, the same product.
        assert_eq!(span, 96.37746785002716);
        let below = |x: f64| f64::from_bits(x.to_bits() - 1);
        for b in [LengthBin::Short24, LengthBin::Short50] {
            let r = b.short_rule().unwrap();
            assert!(r.strong(top, [span]));
            assert!(r.strong(top, [10.0, span, 50.0]));
            assert!(!r.strong(below(top), [span]), "{b:?}: the document must reach the top level");
            assert!(!r.strong(top, [below(span)]), "{b:?}: a passage must reach the span level");
            assert!(!r.strong(top, []), "{b:?}: no passage, no strong result");
            assert!(!r.strong(STRONG_CUT * 100., [99.0]));
        }
        let r = LengthBin::Short100.short_rule().unwrap();
        let span100 = SHORT_SPAN_LEVEL_100 * 100.;
        assert_eq!(span100, 94.20289855072464);
        assert!(r.strong(STRONG_CUT * 100., [span100]));
        assert!(!r.strong(below(STRONG_CUT * 100.), [span100]));
        assert!(!r.strong(STRONG_CUT * 100., [below(span100)]));
        assert!(!r.strong(99.99, []));
        assert!(LengthBin::Measured.short_rule().is_none() && LengthBin::Longer.short_rule().is_none());
        assert!(!Band::NoStrongSigns.shows_spans());
    }

    #[test]
    fn display_rules() {
        assert_eq!(confidence_text(97.36), "97.3%");
        assert_eq!(confidence_text(VERY_STRONG_CUT * 100.), "above 99.9%");
        assert_eq!(confidence_text(99.9), "99.9%");
        assert_eq!(confidence_text(0.4), "below 1%");
        assert_eq!(confidence_text(1.0), "1.0%");
        assert_eq!(share_text(34.7, 97.0), "about 34%");
        assert_eq!(share_text(50.0, 40.0), "about 40%");
        assert_eq!(share_text(0.3, 10.0), "below 1%");
        assert_eq!(tenths_floor(100.0), 1000);
    }

    #[test]
    fn notices() {
        assert_eq!(markers_in("a [SEP] b [MASK]").collect::<Vec<_>>(), vec!["[SEP]", "[MASK]"]);
        assert!(has_invisible("a\u{200B}b"));
        assert!(has_invisible("a\u{1}b"));
        assert!(!has_invisible("a\tb\nc\r\n"));
    }
}
