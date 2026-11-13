//! Fitting the card's text into its boxes: SVG text does not wrap, so the
//! template breaks and sizes every string itself, from the embedded fonts'
//! metrics ([`crate::fonts::Face`]).
//!
//! - [`headline`]: the headline in at most three balanced lines, the size
//!   stepping down from 42 px until it fits (as CSS `text-wrap: balance`
//!   does on the pages).
//! - [`fit`]: one line, the size stepping down to a floor, then cut in the
//!   middle with an ellipsis, so a long host or model line keeps both ends.

use crate::fonts::Face;

/// Lines of a wrap, as slices of the input.
pub type Lines<'a> = Vec<&'a str>;

/// Greedy wrap of `text` at spaces into lines no wider than `max` (a word
/// wider than `max` gets a line of its own).
pub fn wrap<'a>(face: &Face, text: &'a str, size: f32, spacing: f32, max: f32) -> Lines<'a> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut last_fit: Option<usize> = None;
    let mut i = 0usize;
    let bytes = text.as_bytes();
    while i <= text.len() {
        let at_break = i == text.len() || bytes.get(i) == Some(&b' ');
        if at_break {
            let candidate = text.get(start..i).unwrap_or("");
            if face.width(candidate, size, spacing) <= max || last_fit.is_none() {
                last_fit = Some(i);
            } else if let Some(end) = last_fit {
                lines.push(text.get(start..end).unwrap_or(""));
                start = end + 1;
                last_fit = None;
                // Re-measure the current word on the new line.
                continue;
            }
            if i == text.len() {
                if let Some(end) = last_fit {
                    lines.push(text.get(start..end).unwrap_or(""));
                }
                break;
            }
        }
        i += 1;
    }
    lines.retain(|l| !l.is_empty());
    lines
}

/// The narrowest wrap with as few lines as the greedy wrap at `max`
/// (balanced lines), or `None` when that takes more than `max_lines`.
pub fn balanced<'a>(
    face: &Face,
    text: &'a str,
    size: f32,
    spacing: f32,
    max: f32,
    max_lines: usize,
) -> Option<Lines<'a>> {
    let greedy = wrap(face, text, size, spacing, max);
    if greedy.len() > max_lines {
        return None;
    }
    if greedy.len() <= 1 {
        return Some(greedy);
    }
    let n = greedy.len();
    let (mut lo, mut hi) = (max / 3.0, max);
    for _ in 0..16 {
        let mid = (lo + hi) / 2.0;
        if wrap(face, text, size, spacing, mid).len() <= n {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let best = wrap(face, text, size, spacing, hi);
    Some(if best.len() == n { best } else { greedy })
}

/// The headline: its size and lines, three at most, within `max` pixels.
pub fn headline<'a>(face: &Face, text: &'a str, max: f32) -> (f32, Lines<'a>) {
    let mut size = 42.0f32;
    loop {
        let spacing = -0.05 * size;
        if let Some(lines) = balanced(face, text, size, spacing, max, 3) {
            return (size, lines);
        }
        if size <= 26.0 {
            return (size, wrap(face, text, size, spacing, max).into_iter().take(3).collect());
        }
        size -= 2.0;
    }
}

/// One line fitted into `max` pixels: the largest size from `size` down to
/// `floor` (whole pixels) at which it fits, else `floor` with the middle
/// cut out and an ellipsis in its place.
pub fn fit(face: &Face, text: &str, size: f32, floor: f32, spacing_em: f32, max: f32) -> (f32, String) {
    let mut s = size;
    while s > floor && face.width(text, s, spacing_em * s) > max {
        s -= 1.0;
    }
    let s = s.max(floor);
    if face.width(text, s, spacing_em * s) <= max {
        return (s, text.to_owned());
    }
    let chars: Vec<char> = text.chars().collect();
    let mut keep = chars.len();
    while keep > 1 {
        keep -= 1;
        let head = keep.div_ceil(2);
        let tail = keep - head;
        let mut out: String = chars.iter().take(head).collect();
        out.push('\u{2026}');
        out.extend(chars.iter().skip(chars.len() - tail));
        if face.width(&out, s, spacing_em * s) <= max {
            return (s, out);
        }
    }
    (s, "\u{2026}".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::Weight;

    fn bold() -> Face {
        Face::of(Weight::Bold).unwrap()
    }

    #[test]
    fn wrap_keeps_every_word_in_order() {
        let f = bold();
        let t = "Strong signs of AI involvement in 1 of 3 sections.";
        for max in [120.0, 250.0, 370.0, 640.0, 2000.0] {
            let lines = wrap(&f, t, 42.0, -2.1, max);
            assert_eq!(lines.join(" "), t, "{max}");
            for l in &lines {
                assert!(f.width(l, 42.0, -2.1) <= max || !l.contains(' '), "{l:?} at {max}");
            }
        }
        assert_eq!(wrap(&f, "", 42.0, 0.0, 100.0), Vec::<&str>::new());
        assert_eq!(wrap(&f, "Supercalifragilistic", 42.0, 0.0, 10.0), vec!["Supercalifragilistic"]);
    }

    #[test]
    fn headlines_fit_three_balanced_lines() {
        let f = bold();
        for t in [
            "Very strong evidence of AI involvement.",
            "No strong signs of AI involvement.",
            "Unclear evidence of AI involvement.",
            "Strong signs of AI involvement.",
            "Strong signs of AI involvement in 4294967295 of 4294967295 sections.",
            "No strong signs of AI involvement in any of the 4294967295 sections.",
        ] {
            for max in [370.0, 640.0] {
                let (size, lines) = headline(&f, t, max);
                assert!(lines.len() <= 3 && !lines.is_empty(), "{t}");
                assert_eq!(lines.join(" "), t, "{t} at {max}");
                for l in &lines {
                    assert!(f.width(l, size, -0.05 * size) <= max, "{l:?}");
                }
            }
        }
        // Balanced: the two lines of a short headline are close in width.
        let (size, lines) = headline(&f, "No strong signs of AI involvement.", 370.0);
        assert_eq!(size, 42.0);
        let w: Vec<f32> = lines.iter().map(|l| f.width(l, size, -0.05 * size)).collect();
        assert!(
            w.iter().cloned().fold(0.0, f32::max) - w.iter().cloned().fold(f32::MAX, f32::min) < 120.0,
            "{lines:?}"
        );
    }

    #[test]
    fn fit_shrinks_then_cuts_the_middle() {
        let f = Face::of(Weight::Regular).unwrap();
        let (s, t) = fit(&f, "garden-diary.example", 26.0, 18.0, 0.0, 370.0);
        assert_eq!((s, t.as_str()), (26.0, "garden-diary.example"));
        let long = format!("{}.example", "a".repeat(63) + "." + &"b".repeat(63) + "." + &"c".repeat(60));
        let (s, t) = fit(&f, &long, 26.0, 18.0, 0.0, 370.0);
        assert_eq!(s, 18.0);
        assert!(t.contains('\u{2026}') && t.starts_with("aaa") && t.ends_with(".example"), "{t}");
        assert!(f.width(&t, s, 0.0) <= 370.0);
        let (_, t) = fit(&f, "x", 26.0, 18.0, 0.0, 1.0);
        assert_eq!(t, "\u{2026}");
    }
}
