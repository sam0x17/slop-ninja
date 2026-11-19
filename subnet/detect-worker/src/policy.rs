//! What the page may say about a text, from A's measured evidence.
//!
//! - **Length.** The four bands' human false-positive rates were measured on
//!   texts of 150 to 450 whitespace words (the human pools' word count), and
//!   for texts that end at a sentence end they were measured to hold from 451
//!   to 1,000 words; those get their own bin, so the page can say so and add
//!   that a small AI part is caught less often in a longer text. The free
//!   check scores at most the first 1,000 words, cut back to a sentence end
//!   (`crate::cut`), and the bin is that of the words scored. From 24 to 149
//!   words, in three bins (24-49, 50-99, 100-149), the confidence comes from
//!   the bin's own calibration map ([`ShortMaps`]); under 24 words, the span
//!   floor, the page gives no score.
//! - **Bands.** Four evidence bands on the calibrated involvement
//!   probability, cut at the release's Calibration-split operating points
//!   (`docs/a-baseline-release.md`): the 1% human-FPR point and the 0.1%
//!   point (the top isotonic level), and 50%. The scorer prints
//!   `ai_confidence_percent` as the probability times 100, and multiplying
//!   by 100 is monotone in floating point, so comparing the percentage with
//!   each cut times 100, computed the same way, is the same comparison.
//! - **Short texts.** Below 150 words the release's confidence is not
//!   calibrated and its cuts flag too many human texts
//!   (`docs/detect-evidence.md`, short texts). Each short bin has its own
//!   isotonic map, fitted on Calibration-split excerpts of that length, from
//!   the scorer's `calibration_input` (the release map's own input) to the
//!   confidence the page prints. The current product labels are unclear from
//!   50% and strong from 70%, retaining each bin's measured very-strong cut.
//!   Historical 1%-FPR strong cuts remain in the frozen maps as evidence;
//!   those rates do not describe the current 70% label. The maps belong to one
//!   package (span package d24, by its published digest) and are bound by
//!   their file's digest ([`SHORT_MAPS_SHA256`]); under another package, or
//!   without the input, a short text gets the measured minimum of 150 words
//!   instead. The share is not shown below 150 words: on short excerpts it
//!   read 25% or more on a third of human texts of 24 to 49 words and was
//!   never recalibrated.
//! - **Spans.** Passages are shown only in the two strong bands, only when
//!   the span switch is on, and never under 24 words. Below 150 words they
//!   carry no confidence of their own: the span confidence map was not
//!   measured on short texts.
//! - **Notices.** Control markers the tokenizer reads as special tokens, and
//!   invisible or control characters, are named, never removed.

use std::sync::OnceLock;

/// The 1% human-FPR operating point on the Calibration split.
pub const STRONG_CUT: f64 = 0.9090909090909091;
/// The 0.1% point, the top clipped isotonic level.
pub const VERY_STRONG_CUT: f64 = 0.9998820476527482;
/// Below this the result is in the weak band.
pub const UNCLEAR_CUT: f64 = 0.5;
/// Product label for short texts, independent of the historical FPR operating points.
pub const SHORT_STRONG_CUT: f64 = 0.70;
/// The span floor: no passage under 24 words is ever shown.
pub const SPAN_MIN_WORDS: u64 = 24;

/// The short-text maps (schema `slop_ninja_detect_short_maps_v1`): one
/// isotonic map and its cuts per short bin, fitted for one package. Only
/// numbers: knots, levels and bounds, no text.
pub const SHORT_MAPS_JSON: &str = include_str!("../short-maps-d24.json");
/// SHA-256 of [`SHORT_MAPS_JSON`], the digest the evidence names (a test
/// recomputes it).
pub const SHORT_MAPS_SHA256: &str = "121d8d1c3623e914813685f6ab3ad0f44b9e9377ad872faa9f03a7b4aff11dbc";

/// The raw added tokens of the pinned tokenizer (its special tokens), which
/// the scorer splits out of the text before normalization. A test checks
/// the list against the compiled-in table.
pub const MARKERS: &[&str] = &["<|padding|>", "<|endoftext|>", "[UNK]", "[CLS]", "[SEP]", "[PAD]", "[MASK]"];

/// An evidence band. From 150 words it is cut at the release's points
/// ([`Band::of_percent`]); from 24 to 149 words, at the bin's own
/// ([`ShortMap::band`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Band {
    Weak,
    Unclear,
    Strong,
    VeryStrong,
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
        }
    }

    pub fn from_code(s: &str) -> Option<Band> {
        [Band::VeryStrong, Band::Strong, Band::Unclear, Band::Weak].into_iter().find(|b| b.code() == s)
    }

    /// Whether passages may be highlighted in this band.
    pub fn shows_spans(self) -> bool {
        matches!(self, Band::Strong | Band::VeryStrong)
    }
}

/// Where a text's length sits against the measured ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LengthBin {
    /// 24 to 49 words: its own map and cuts.
    Short24,
    /// 50 to 99 words: its own map and cuts.
    Short50,
    /// 100 to 149 words: its own map and cuts.
    Short100,
    /// 150 to 450 words: the band table.
    Measured,
    /// 451 to 1,000 scored words: the band table's human rates measured to
    /// hold; the AI side caught less often when the AI part is small.
    Long,
}

impl LengthBin {
    pub fn code(self) -> &'static str {
        match self {
            LengthBin::Short24 => "24_49",
            LengthBin::Short50 => "50_99",
            LengthBin::Short100 => "100_149",
            LengthBin::Measured => "150_450",
            LengthBin::Long => "451_1000",
        }
    }

    pub fn from_code(s: &str) -> Option<LengthBin> {
        [LengthBin::Short24, LengthBin::Short50, LengthBin::Short100, LengthBin::Measured, LengthBin::Long]
            .into_iter()
            .find(|b| b.code() == s)
    }

    pub fn is_short(self) -> bool {
        matches!(self, LengthBin::Short24 | LengthBin::Short50 | LengthBin::Short100)
    }

    /// The bin's map under the package published as `published` (a parts
    /// manifest digest): `None` from 150 words, and for a short bin when
    /// the maps are not that package's (or do not check out).
    pub fn short_map(self, published: &str) -> Option<&'static ShortMap> {
        let maps = ShortMaps::get()?;
        if maps.published_digest != published {
            return None;
        }
        maps.bins.iter().find(|m| m.length == self)
    }
}

/// One short bin's calibration map and cuts.
#[derive(Clone, Debug, PartialEq)]
pub struct ShortMap {
    pub length: LengthBin,
    pub lo: f64,
    pub hi: f64,
    /// The bin's cuts, as probabilities: each a level of the map.
    pub strong: f64,
    pub very_strong: f64,
    pub unclear: f64,
    /// `(input, confidence)`, inputs strictly increasing.
    pub knots: Vec<(f64, f64)>,
}

impl ShortMap {
    /// The calibrated involvement probability of a scorer's
    /// `calibration_input` `x`: linear between adjacent knots, constant
    /// outside them, as the release's map applies its own, computed in the
    /// order the measurement computed it (`None` for an input that is not a
    /// probability).
    pub fn confidence(&self, x: f64) -> Option<f64> {
        if !x.is_finite() || !(0.0..=1.0).contains(&x) {
            return None;
        }
        let (first, last) = (self.knots.first()?, self.knots.last()?);
        if x <= first.0 {
            return Some(first.1);
        }
        if x >= last.0 {
            return Some(last.1);
        }
        let upper = self.knots.partition_point(|k| k.0 <= x);
        let (&(x0, y0), &(x1, y1)) = (self.knots.get(upper.checked_sub(1)?)?, self.knots.get(upper)?);
        Some(y0 + (y1 - y0) * (x - x0) / (x1 - x0))
    }

    /// The current display band: strong from 70%, with the bin's existing very-strong ceiling.
    pub fn band(&self, p: f64) -> Band {
        if p >= self.very_strong {
            Band::VeryStrong
        } else if p >= SHORT_STRONG_CUT {
            Band::Strong
        } else if p >= self.unclear {
            Band::Unclear
        } else {
            Band::Weak
        }
    }
}

/// The maps file as stored; its prose fields are not read.
#[derive(serde::Deserialize)]
struct MapsFile {
    schema: String,
    package: MapsPackage,
    bins: Vec<MapsBin>,
}

#[derive(serde::Deserialize)]
struct MapsPackage {
    published_digest: String,
}

#[derive(serde::Deserialize)]
struct MapsBin {
    length: String,
    min_words: u64,
    max_words: u64,
    lo: f64,
    hi: f64,
    strong: f64,
    very_strong: f64,
    unclear: f64,
    knots: Vec<[f64; 2]>,
}

/// The parsed short-text maps: the package they were fitted for and one map
/// per short bin, in bin order.
#[derive(Debug, PartialEq)]
pub struct ShortMaps {
    pub published_digest: String,
    pub bins: Vec<ShortMap>,
}

impl ShortMaps {
    /// [`SHORT_MAPS_JSON`], parsed and checked once; `None` when it does not
    /// check out ([`ShortMaps::parse`]), which leaves every short bin
    /// unscored.
    pub fn get() -> Option<&'static ShortMaps> {
        static MAPS: OnceLock<Option<ShortMaps>> = OnceLock::new();
        MAPS.get_or_init(|| ShortMaps::parse(SHORT_MAPS_JSON)).as_ref()
    }

    /// Parses a maps file and checks it: the schema, a 64-hex package digest,
    /// exactly the three short bins in order with their word ranges, inputs
    /// strictly increasing inside [0, 1], levels non-decreasing inside
    /// [lo, hi], and cuts that are levels of the map with
    /// unclear < strong < very strong.
    pub fn parse(json: &str) -> Option<ShortMaps> {
        let file: MapsFile = serde_json::from_str(json).ok()?;
        if file.schema != "slop_ninja_detect_short_maps_v1" {
            return None;
        }
        let published_digest = file.package.published_digest;
        if published_digest.len() != 64 || !published_digest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let want = [(LengthBin::Short24, 24, 49), (LengthBin::Short50, 50, 99), (LengthBin::Short100, 100, 149)];
        if file.bins.len() != want.len() {
            return None;
        }
        let mut out = Vec::with_capacity(want.len());
        for (b, (length, min, max)) in file.bins.into_iter().zip(want) {
            let words_ok = b.min_words == min
                && b.max_words == max
                && length_bin(min as usize) == Ok(length)
                && length_bin(max as usize) == Ok(length);
            if b.length != length.code() || !words_ok {
                return None;
            }
            let m = ShortMap {
                length,
                lo: b.lo,
                hi: b.hi,
                strong: b.strong,
                very_strong: b.very_strong,
                unclear: b.unclear,
                knots: b.knots.into_iter().map(|[x, y]| (x, y)).collect(),
            };
            let pairs = || m.knots.iter().zip(m.knots.iter().skip(1));
            let inputs_ok = pairs().all(|(a, b)| a.0 < b.0)
                && m.knots.iter().all(|k| k.0.is_finite() && (0.0..=1.0).contains(&k.0));
            let levels_ok = pairs().all(|(a, b)| a.1 <= b.1)
                && m.knots.iter().all(|k| k.1.is_finite() && m.lo <= k.1 && k.1 <= m.hi);
            let is_level = |c: f64| m.knots.iter().any(|k| k.1 == c);
            let cuts_ok = [m.lo, m.hi, m.unclear, m.strong, m.very_strong].iter().all(|c| c.is_finite())
                && 0.0 < m.lo
                && m.hi < 1.0
                && m.unclear < m.strong
                && m.strong < m.very_strong
                && is_level(m.strong)
                && is_level(m.very_strong);
            if m.knots.len() < 2 || !inputs_ok || !levels_ok || !cuts_ok {
                return None;
            }
            out.push(m);
        }
        Some(ShortMaps { published_digest, bins: out })
    }
}

/// Whitespace words, the human pools' count (`grammar_a_form::text::words`).
pub fn words(s: &str) -> usize {
    s.split_whitespace().count()
}

/// The length decision before scoring, on the scored part's words:
/// `Err(words)` when too short. The free cut scores at most
/// [`crate::bound::FREE_MAX_WORDS`] words, inside the measured range, so
/// [`LengthBin::Long`] never holds more.
pub fn length_bin(words: usize) -> Result<LengthBin, usize> {
    use crate::bound::{BAND_TABLE_MAX_WORDS, MEASURED_MIN_WORDS, MIN_WORDS};
    Ok(match words {
        w if w < MIN_WORDS => return Err(words),
        w if w < 50 => LengthBin::Short24,
        w if w < 100 => LengthBin::Short50,
        w if w < MEASURED_MIN_WORDS => LengthBin::Short100,
        w if w <= BAND_TABLE_MAX_WORDS => LengthBin::Measured,
        _ => LengthBin::Long,
    })
}

/// The fewest words the page scores under the package published as
/// `published`: 24 when the short-text maps are that package's, and 150,
/// where the measured bands start, otherwise.
pub fn min_words(published: &str) -> usize {
    if LengthBin::Short24.short_map(published).is_some() {
        crate::bound::MIN_WORDS
    } else {
        crate::bound::MEASURED_MIN_WORDS
    }
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
    let s = shown_share(share, confidence);
    if s < 1.0 { "below 1%".into() } else { format!("about {}%", s.floor() as u32) }
}

/// The dial's bare number: the shown share floored (0 when it reads "below
/// 1%"), the same number [`share_text`] prints.
pub fn share_pct(share: f64, confidence: f64) -> u8 {
    shown_share(share, confidence).floor().min(100.0) as u8
}

/// The share as shown: never above the confidence, never below 0.
fn shown_share(share: f64, confidence: f64) -> f64 {
    if share.is_finite() { share.min(confidence).max(0.0) } else { 0.0 }
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
        for b in [Band::VeryStrong, Band::Strong, Band::Unclear, Band::Weak] {
            assert_eq!(Band::from_code(b.code()), Some(b));
        }
        assert_eq!(Band::from_code("no_strong_signs"), None);
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
        assert_eq!(length_bin(451), Ok(LengthBin::Long));
        assert_eq!(length_bin(1000), Ok(LengthBin::Long));
        assert_eq!(LengthBin::Long.code(), "451_1000");
        assert_eq!(LengthBin::from_code("over_450"), None);
        assert_eq!(words("  one\ttwo\nthree  "), 3);
        assert_eq!((min_words(D24), min_words(&"0".repeat(64))), (24, 150));
        for b in [LengthBin::Short24, LengthBin::Short50, LengthBin::Short100, LengthBin::Measured, LengthBin::Long] {
            assert_eq!(LengthBin::from_code(b.code()), Some(b));
            assert_eq!(b.is_short(), !matches!(b, LengthBin::Measured | LengthBin::Long));
        }
        // The span floor is the shortest scored text, so a text under 48
        // words holds at most one passage.
        assert_eq!(crate::bound::MIN_WORDS as u64, SPAN_MIN_WORDS);
    }

    /// Span package d24's published digest, the maps' package.
    const D24: &str = "4bc3d49ce183d4a9ab150867eb4ec23280565351e07c041d161c9e728e19eac3";

    /// The levels the measurement chose on the Calibration split
    /// (`docs/detect-evidence.md`, short texts): (bin, strong, very strong).
    const CUTS: [(LengthBin, f64, f64); 3] = [
        (LengthBin::Short24, 0.9374105837343678, 0.9885387244785764),
        (LengthBin::Short50, 0.9696816574027286, 0.9998631261976457),
        (LengthBin::Short100, 0.8959405374499715, 0.999869007073618),
    ];

    fn below(x: f64) -> f64 {
        f64::from_bits(x.to_bits() - 1)
    }

    #[test]
    fn the_maps_are_bound_by_digest() {
        use sha2::{Digest, Sha256};
        assert_eq!(hex::encode(Sha256::digest(SHORT_MAPS_JSON.as_bytes())), SHORT_MAPS_SHA256);
        let maps = ShortMaps::get().expect("the maps check out");
        assert_eq!(maps.published_digest, D24);
        assert_eq!(maps.bins.len(), 3);
        // Only under their own package, only for the short bins.
        for (b, strong, very_strong) in CUTS {
            let m = b.short_map(D24).unwrap();
            assert_eq!((m.length, m.strong, m.very_strong, m.unclear), (b, strong, very_strong, 0.5));
            assert_eq!(b.short_map(&"a".repeat(64)), None);
        }
        assert!(LengthBin::Measured.short_map(D24).is_none() && LengthBin::Long.short_map(D24).is_none());
    }

    /// Each bin's map at, between and outside its knots, and its cuts at
    /// their edges: the input of the first knot at a cut's level is in that
    /// band, the largest input below it is not.
    #[test]
    fn each_bin_has_its_own_map_and_cuts() {
        for (b, _strong, very_strong) in CUTS {
            let m = b.short_map(D24).unwrap();
            let (first, last) = (m.knots[0], *m.knots.last().unwrap());
            assert_eq!(m.confidence(0.0), Some(first.1));
            assert_eq!(m.confidence(first.0), Some(first.1));
            assert_eq!(m.confidence(1.0), Some(last.1));
            assert_eq!((first.1, last.1), (m.lo, m.hi));
            for x in [f64::NAN, -0.1, 1.5, f64::INFINITY] {
                assert_eq!(m.confidence(x), None);
            }
            for w in m.knots.windows(2) {
                let ((x0, y0), (x1, y1)) = (w[0], w[1]);
                assert_eq!(m.confidence(x0), Some(y0));
                let mid = m.confidence(x0 + (x1 - x0) / 2.).unwrap();
                assert!(y0 <= mid && mid <= y1, "{b:?}: {mid} outside [{y0}, {y1}]");
            }
            {
                let (cut, band) = (very_strong, Band::VeryStrong);
                let at = m.knots.iter().position(|k| k.1 == cut).unwrap();
                let x = m.knots[at].0;
                assert_eq!(m.band(m.confidence(x).unwrap()), band, "{b:?} at {cut}");
                let x_before = m.knots[at - 1].0;
                let p_between = m.confidence(x_before + (x - x_before) / 2.).unwrap();
                assert!(m.band(p_between) < band, "{b:?} below {cut}: {p_between}");
                assert_eq!(m.band(cut), band);
                assert!(m.band(below(cut)) < band);
            }
            assert_eq!(m.band(below(SHORT_STRONG_CUT)), Band::Unclear);
            assert_eq!(m.band(SHORT_STRONG_CUT), Band::Strong);
            assert_eq!(m.band(0.80), Band::Strong);
            assert_eq!(m.band(0.5), Band::Unclear);
            assert_eq!(m.band(below(0.5)), Band::Weak);
            assert_eq!(m.band(m.hi), Band::VeryStrong);
            assert_eq!(m.band(m.lo), Band::Weak);
        }
        // Strong starts at 70% for every short bin; very strong still differs.
        let p = 0.95;
        let bands: Vec<Band> = CUTS.iter().map(|(b, ..)| b.short_map(D24).unwrap().band(p)).collect();
        assert_eq!(bands, [Band::Strong, Band::Strong, Band::Strong]);
        assert_eq!(Band::of_percent(p * 100.), Some(Band::Strong));
        let q = 0.99;
        let bands: Vec<Band> = CUTS.iter().map(|(b, ..)| b.short_map(D24).unwrap().band(q)).collect();
        assert_eq!(bands, [Band::VeryStrong, Band::Strong, Band::Strong]);
    }

    /// A maps file that does not check out leaves every short bin unscored.
    #[test]
    fn a_maps_file_that_does_not_check_out_is_refused() {
        let good: serde_json::Value = serde_json::from_str(SHORT_MAPS_JSON).unwrap();
        assert!(ShortMaps::parse(SHORT_MAPS_JSON).is_some());
        let bad = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut v = good.clone();
            edit(&mut v);
            ShortMaps::parse(&v.to_string()).is_none()
        };
        assert!(bad(&|v| v["schema"] = "slop_ninja_detect_short_maps_v0".into()));
        assert!(bad(&|v| v["package"]["published_digest"] = "ABC".into()));
        assert!(bad(&|v| {
            v["bins"].as_array_mut().unwrap().pop();
        }));
        assert!(bad(&|v| v["bins"].as_array_mut().unwrap().swap(0, 1)));
        assert!(bad(&|v| v["bins"][0]["min_words"] = 23.into()));
        assert!(bad(&|v| v["bins"][2]["max_words"] = 150.into()));
        assert!(bad(&|v| v["bins"][1]["strong"] = 0.97.into()));
        assert!(bad(&|v| v["bins"][1]["very_strong"] = v["bins"][1]["strong"].clone()));
        assert!(bad(&|v| v["bins"][0]["knots"].as_array_mut().unwrap().swap(3, 4)));
        assert!(bad(&|v| v["bins"][0]["knots"][5][1] = 0.0.into()));
        assert!(bad(&|v| v["bins"][0]["knots"][5] = serde_json::json!([0.5])));
        assert!(bad(&|v| v["bins"][0]["lo"] = 0.0.into()));
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
        // The dial's number is the printed one.
        assert_eq!((share_pct(34.7, 97.0), share_pct(50.0, 40.0), share_pct(0.3, 10.0)), (34, 40, 0));
        assert_eq!((share_pct(f64::NAN, 50.0), share_pct(100.0, 100.0), share_pct(-3.0, 50.0)), (0, 100, 0));
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
