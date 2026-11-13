//! Writing the card's SVG: numbers in one fixed form, and card strings
//! escaped into text nodes.
//!
//! The template writes every element and attribute itself. A string that
//! comes from the card JSON (a host, a figure, a note, the model line) only
//! ever enters through [`text`], as the content of a `<text>` or `<tspan>`
//! element, escaped; no card string becomes an attribute, an id, a style or a
//! reference.

use std::fmt::{self, Display, Write};

/// A number written with at most two decimals and no trailing zeros.
#[derive(Clone, Copy, Debug)]
pub struct N(pub f32);

impl Display for N {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = if self.0.is_finite() { self.0 } else { 0.0 };
        let hundredths = (f64::from(v) * 100.0).round() as i64;
        let (sign, abs) = if hundredths < 0 { ("-", -hundredths) } else { ("", hundredths) };
        let (int, frac) = (abs / 100, abs % 100);
        match frac {
            0 => write!(f, "{sign}{int}"),
            x if x % 10 == 0 => write!(f, "{sign}{int}.{}", x / 10),
            x => write!(f, "{sign}{int}.{x:02}"),
        }
    }
}

/// Appends `s` escaped for a text node: `&`, `<`, `>`, and both quotes.
pub fn text(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

/// Appends formatted template text (never card strings).
pub fn put(out: &mut String, args: fmt::Arguments<'_>) {
    let _ = out.write_fmt(args);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_have_one_form() {
        let s = |v: f32| N(v).to_string();
        assert_eq!(s(0.0), "0");
        assert_eq!(s(12.0), "12");
        assert_eq!(s(12.5), "12.5");
        assert_eq!(s(12.345), "12.35");
        assert_eq!(s(-3.1), "-3.1");
        assert_eq!(s(-0.001), "0");
        assert_eq!(s(f32::NAN), "0");
    }

    #[test]
    fn text_escapes_markup() {
        let mut o = String::new();
        text(&mut o, r#"<a href="x">&'</a>"#);
        assert_eq!(o, "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;");
    }
}
