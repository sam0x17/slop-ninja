//! The two embedded fonts (an Inter subset under the SIL Open Font License
//! 1.1, `src/fonts/`) and the metrics the layout measures text with.
//!
//! The renderer loads these two faces and nothing else: no system font, no
//! file and no fetch. The subset holds printable ASCII, the middle dot and
//! the ellipsis ([`supported`]); a card with any other character is refused
//! before it is drawn. The metrics read the subset's own `cmap` (format 4),
//! `hmtx` and `kern` (format 0) tables in place, without allocating; the
//! `kern` table carries the source's GPOS kerning, which the shaper applies
//! the same way (a test compares the two).

/// Inter 400 (the subset).
pub static REGULAR: &[u8] = include_bytes!("fonts/inter-400.ttf");
/// Inter 700 (the subset).
pub static BOLD: &[u8] = include_bytes!("fonts/inter-700.ttf");
/// The family name both faces carry.
pub const FAMILY: &str = "Inter";

/// Inter's ascender and descender (1984 and 494 of 2048 units), as
/// fractions of the font size: the layout places baselines with them as a
/// browser places a line box.
pub const ASCENDER: f32 = 1984.0 / 2048.0;
pub const DESCENDER: f32 = 494.0 / 2048.0;
/// The height of uppercase letters (1490 of 2048 units), to set them in the
/// middle of a box.
pub const UPPER_HEIGHT: f32 = 1490.0 / 2048.0;

/// Whether the subset can print `c`.
pub fn supported(c: char) -> bool {
    matches!(c, ' '..='~' | '\u{00B7}' | '\u{2026}')
}

/// Whether every character of `s` is in the subset.
pub fn all_supported(s: &str) -> bool {
    s.chars().all(supported)
}

/// A weight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Bold,
}

impl Weight {
    /// The CSS weight usvg matches against the faces.
    pub fn css(self) -> u16 {
        match self {
            Weight::Regular => 400,
            Weight::Bold => 700,
        }
    }
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    let s = b.get(o..o.checked_add(2)?)?;
    Some(u16::from_be_bytes([*s.first()?, *s.get(1)?]))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    let s = b.get(o..o.checked_add(4)?)?;
    Some(u32::from_be_bytes([*s.first()?, *s.get(1)?, *s.get(2)?, *s.get(3)?]))
}

/// One face's metric tables, located in its bytes.
#[derive(Clone, Copy, Debug)]
pub struct Face {
    data: &'static [u8],
    upem: f32,
    /// The format 4 subtable.
    cmap: usize,
    hmtx: usize,
    n_hmetrics: u16,
    /// The first pair of the format 0 `kern` subtable, and the pair count.
    kern: usize,
    n_kern: usize,
}

impl Face {
    /// The face of a weight. `None` only if the embedded bytes are not the
    /// subset (a test asserts both parse).
    pub fn of(weight: Weight) -> Option<Face> {
        Face::parse(match weight {
            Weight::Regular => REGULAR,
            Weight::Bold => BOLD,
        })
    }

    fn parse(data: &'static [u8]) -> Option<Face> {
        let n = u16_at(data, 4)? as usize;
        let table = |tag: &[u8; 4]| -> Option<usize> {
            (0..n).find_map(|i| {
                let rec = 12 + 16 * i;
                (data.get(rec..rec + 4)? == tag).then(|| u32_at(data, rec + 8).map(|o| o as usize)).flatten()
            })
        };
        let head = table(b"head")?;
        let hhea = table(b"hhea")?;
        let hmtx = table(b"hmtx")?;
        let cmap_t = table(b"cmap")?;
        let kern_t = table(b"kern")?;
        let records = u16_at(data, cmap_t + 2)? as usize;
        let cmap = (0..records).find_map(|i| {
            let rec = cmap_t + 4 + 8 * i;
            let (pid, eid) = (u16_at(data, rec)?, u16_at(data, rec + 2)?);
            let off = cmap_t + u32_at(data, rec + 4)? as usize;
            (pid == 3 && eid == 1 && u16_at(data, off)? == 4).then_some(off)
        })?;
        // kern version 0, one subtable of format 0 (coverage 0x0001).
        if u16_at(data, kern_t)? != 0 || u16_at(data, kern_t + 2)? == 0 || u16_at(data, kern_t + 8)? != 0x0001 {
            return None;
        }
        let n_kern = u16_at(data, kern_t + 10)? as usize;
        Some(Face {
            data,
            upem: f32::from(u16_at(data, head + 18)?),
            cmap,
            hmtx,
            n_hmetrics: u16_at(data, hhea + 34)?,
            kern: kern_t + 18,
            n_kern,
        })
    }

    /// The glyph of `c`, or `None` when the subset lacks it.
    pub fn glyph(&self, c: char) -> Option<u16> {
        let code = u16::try_from(c as u32).ok()?;
        let d = self.data;
        let seg_x2 = u16_at(d, self.cmap + 6)? as usize;
        let ends = self.cmap + 14;
        let starts = ends + seg_x2 + 2;
        let deltas = starts + seg_x2;
        let ranges = deltas + seg_x2;
        for s in 0..seg_x2 / 2 {
            let end = u16_at(d, ends + 2 * s)?;
            if code > end {
                continue;
            }
            let start = u16_at(d, starts + 2 * s)?;
            if code < start {
                return None;
            }
            let delta = u16_at(d, deltas + 2 * s)?;
            let ro = u16_at(d, ranges + 2 * s)? as usize;
            let g = if ro == 0 {
                code.wrapping_add(delta)
            } else {
                let g = u16_at(d, ranges + 2 * s + ro + 2 * usize::from(code - start))?;
                if g == 0 { 0 } else { g.wrapping_add(delta) }
            };
            return (g != 0).then_some(g);
        }
        None
    }

    fn advance_units(&self, g: u16) -> u16 {
        let i = usize::from(g.min(self.n_hmetrics.saturating_sub(1)));
        u16_at(self.data, self.hmtx + 4 * i).unwrap_or(0)
    }

    fn kern_units(&self, left: u16, right: u16) -> i16 {
        let key = (u32::from(left) << 16) | u32::from(right);
        let (mut lo, mut hi) = (0usize, self.n_kern);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let rec = self.kern + 6 * mid;
            let Some(k) = u32_at(self.data, rec) else { return 0 };
            match k.cmp(&key) {
                std::cmp::Ordering::Equal => return u16_at(self.data, rec + 4).map_or(0, |v| v as i16),
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        0
    }

    /// The kerning between two characters at `size`, in pixels.
    pub fn kern(&self, left: char, right: char, size: f32) -> f32 {
        match (self.glyph(left), self.glyph(right)) {
            (Some(l), Some(r)) => f32::from(self.kern_units(l, r)) * size / self.upem,
            _ => 0.0,
        }
    }

    /// The advance of `text` at `size` with `spacing` added after each
    /// character, kerning included; characters outside the subset count as
    /// nothing (the card check refuses them before layout).
    pub fn width(&self, text: &str, size: f32, spacing: f32) -> f32 {
        let mut w = 0.0f32;
        let mut prev: Option<u16> = None;
        for c in text.chars() {
            let Some(g) = self.glyph(c) else {
                prev = None;
                continue;
            };
            if let Some(p) = prev {
                w += f32::from(self.kern_units(p, g)) * size / self.upem;
            }
            w += f32::from(self.advance_units(g)) * size / self.upem + spacing;
            prev = Some(g);
        }
        w
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_faces_parse_and_cover_the_subset() {
        for w in [Weight::Regular, Weight::Bold] {
            let f = Face::of(w).expect("the embedded subset parses");
            for c in (' '..='~').chain(['\u{00B7}', '\u{2026}']) {
                assert!(f.glyph(c).is_some(), "{w:?} lacks {c:?}");
                assert!(supported(c));
            }
            for c in ['é', '\u{202E}', '\u{200B}', '\u{00A0}', '\u{FEFF}', '🙂', '\t', '\n'] {
                assert!(f.glyph(c).is_none() || c == '\u{00A0}', "{w:?} maps {c:?}");
                assert!(!supported(c), "{c:?}");
            }
            assert_eq!(f.upem, 2048.0);
        }
    }

    #[test]
    fn kerning_is_present_and_tightens_known_pairs() {
        let f = Face::of(Weight::Bold).unwrap();
        assert!(f.n_kern > 500);
        for (a, b) in [('A', 'V'), ('T', 'o'), ('V', 'a'), ('y', '.')] {
            assert!(f.kern(a, b, 100.0) < 0.0, "{a}{b}");
        }
        assert!(f.width("AV", 100.0, 0.0) < f.width("A", 100.0, 0.0) + f.width("V", 100.0, 0.0));
    }
}
