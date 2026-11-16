//! The calibration input the stub returns when a score request asks for it
//! (`calibration_input`): the value the detect Worker's short-text maps read
//! for a text of 24 to 149 words (`subnet/detect-worker/short-maps-d24.json`,
//! the same file the Worker compiles in). The stub picks an input whose map
//! value in the text's length bin lands in the band its markers ask for, so a
//! `band=` marker gives that band on a short text too; `input=<0..1>` sets the
//! value exactly.

use crate::markers::{Band, Directives};
use serde_json::Value;
use std::sync::OnceLock;

/// The detect Worker's maps file, read from its tree.
const MAPS_JSON: &str = include_str!("../../../detect-worker/short-maps-d24.json");

/// One bin's map: its word range, cuts and knots.
struct Bin {
    min_words: usize,
    max_words: usize,
    unclear: f64,
    strong: f64,
    very_strong: f64,
    knots: Vec<(f64, f64)>,
}

impl Bin {
    /// The map at `x`, computed as the Worker computes it
    /// (`policy::ShortMap::confidence`): linear between adjacent knots,
    /// constant outside them.
    fn at(&self, x: f64) -> f64 {
        let (first, last) = (self.knots[0], self.knots[self.knots.len() - 1]);
        if x <= first.0 {
            return first.1;
        }
        if x >= last.0 {
            return last.1;
        }
        let upper = self.knots.partition_point(|k| k.0 <= x);
        let ((x0, y0), (x1, y1)) = (self.knots[upper - 1], self.knots[upper]);
        y0 + (y1 - y0) * (x - x0) / (x1 - x0)
    }

    /// The smallest input (to the bisection's precision) whose map value
    /// reaches `level`.
    fn first_reaching(&self, level: f64) -> f64 {
        if let Some(k) = self.knots.iter().find(|k| k.1 == level) {
            return k.0;
        }
        let (mut lo, mut hi) = (0.0f64, 1.0f64);
        for _ in 0..200 {
            let mid = lo + (hi - lo) / 2.0;
            if self.at(mid) >= level { hi = mid } else { lo = mid }
        }
        hi
    }

    /// An input whose map value is in `band` under this bin's cuts.
    fn input_for(&self, band: Band) -> f64 {
        match band {
            Band::Weak => 0.0,
            Band::Unclear => self.first_reaching(self.unclear),
            Band::Strong => self.first_reaching(self.strong),
            Band::VeryStrong => self.first_reaching(self.very_strong),
        }
    }
}

fn bins() -> &'static [Bin] {
    static BINS: OnceLock<Vec<Bin>> = OnceLock::new();
    BINS.get_or_init(|| {
        let v: Value = serde_json::from_str(MAPS_JSON).expect("the detect Worker's short-text maps");
        let f = |b: &Value, k: &str| b[k].as_f64().expect("a number");
        v["bins"]
            .as_array()
            .expect("bins")
            .iter()
            .map(|b| Bin {
                min_words: b["min_words"].as_u64().expect("min_words") as usize,
                max_words: b["max_words"].as_u64().expect("max_words") as usize,
                unclear: f(b, "unclear"),
                strong: f(b, "strong"),
                very_strong: f(b, "very_strong"),
                knots: b["knots"]
                    .as_array()
                    .expect("knots")
                    .iter()
                    .map(|k| (k[0].as_f64().expect("input"), k[1].as_f64().expect("level")))
                    .collect(),
            })
            .collect()
    })
}

/// The band the markers ask for: a `conf=` read as the Worker reads a
/// percentage from 150 words (the release's cuts, each times 100), else the
/// `band=` marker.
fn band_of(d: &Directives) -> Band {
    match d.conf {
        Some(c) if c >= 0.9998820476527482 * 100.0 => Band::VeryStrong,
        Some(c) if c >= 0.9090909090909091 * 100.0 => Band::Strong,
        Some(c) if c >= 50.0 => Band::Unclear,
        Some(_) => Band::Weak,
        None => d.band.unwrap_or(Band::Weak),
    }
}

/// The calibration input for `text`: `input=` when given; for a text of 24
/// to 149 whitespace words, one in the band the markers ask for under that
/// bin's map; otherwise the confidence as a fraction.
pub fn input(text: &str, d: &Directives) -> f64 {
    if let Some(x) = d.input {
        return x;
    }
    let words = text.split_whitespace().count();
    match bins().iter().find(|b| (b.min_words..=b.max_words).contains(&words)) {
        Some(bin) => bin.input_for(band_of(d)),
        None => d.percents().0 / 100.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markers::parse;

    fn text(words: usize, marker: &str) -> String {
        let mut t = marker.to_owned();
        for _ in 1..words {
            t.push_str(" word");
        }
        t
    }

    /// The band a bin's map gives the input, by its own cuts.
    fn band_at(bin: &Bin, x: f64) -> Band {
        let p = bin.at(x);
        if p >= bin.very_strong {
            Band::VeryStrong
        } else if p >= bin.strong {
            Band::Strong
        } else if p >= bin.unclear {
            Band::Unclear
        } else {
            Band::Weak
        }
    }

    #[test]
    fn each_band_marker_lands_in_its_band_in_every_short_bin() {
        assert_eq!(bins().len(), 3);
        for bin in bins() {
            for n in [bin.min_words, bin.max_words] {
                for band in [Band::Weak, Band::Unclear, Band::Strong, Band::VeryStrong] {
                    let t = text(n, &format!("sn-stub:band={}", band.as_str()));
                    let x = input(&t, &parse(&t));
                    assert!((0.0..=1.0).contains(&x));
                    assert_eq!(band_at(bin, x), band, "{n} words, {band}");
                }
            }
        }
        // `conf=` reads as the release's band; `input=` is exact.
        let t = text(30, "sn-stub:conf=95");
        assert_eq!(band_at(&bins()[0], input(&t, &parse(&t))), Band::Strong);
        let t = text(30, "sn-stub:input=0.25");
        assert_eq!(input(&t, &parse(&t)), 0.25);
        // From 150 words: the confidence as a fraction.
        let t = text(200, "sn-stub:band=strong");
        assert_eq!(input(&t, &parse(&t)), 0.95);
    }
}
