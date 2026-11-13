//! The card template: a [`Card`] as one 1200 x 630 SVG document, laid out
//! as the detection pages' header block is.
//!
//! - Top left: the wordmark and a "Detector report" pill.
//! - Left: the slop gauge (the dial with its 25, 50 and 75 marks, the lit
//!   arc and needle at the estimated share, the human and slop ends), the
//!   share under the hub, the band chip and the four-band scale. A short
//!   text's gauge has no needle, level or number ("No share") and a two-step
//!   scale; a whole-document check's has a neutral needle and its section ladder in
//!   place of the chip and scale.
//! - Middle: the headline, the confidence line (or the document line), the
//!   source ("a page on <host>", "a post on Bluesky", "a post on X" or the
//!   secret label), the scan date and a free-cut note.
//! - Right: the ninja in its state, on a soft halo and a contact shadow. A
//!   card without a state (generic, whole document) widens the middle column
//!   instead.
//! - Foot: the link as [`CardLink::printed`] prints it (a secret card prints
//!   only the secret host) and the model line.
//!
//! A card shows no text from the page, no rate and no "not proof" line. Card
//! strings enter only as escaped text nodes ([`crate::svg::text`]). The
//! template refuses a card that fails [`Card::check`] or prints a character
//! outside the embedded fonts ([`crate::fonts::supported`]). The output is a
//! pure function of the card, the origins and the ends, so two secret pages
//! whose cards are equal render the same bytes.

use crate::art::{self, Art};
use crate::fonts::{self, ASCENDER, DESCENDER, Face, UPPER_HEIGHT, Weight};
use crate::layout;
use crate::svg::{N, put, text};
use slop_ninja_detect_page::card::{self, Card, CardLink};
use slop_ninja_detect_page::{Band, CardVariant, GaugeEnds, NinjaState, Origins, ShortResult};

pub const WIDTH: u32 = 1200;
pub const HEIGHT: u32 = 630;

/// Why a card was not drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardRefusal {
    /// The card fails its own check (a bound, a form, a variant's fields).
    Invalid(&'static str),
    /// A string holds a character the embedded fonts cannot print.
    Unsupported,
    /// The configured ends cannot be drawn by the card renderer (only
    /// [`GaugeEnds::Icons`] and [`GaugeEnds::Letters`]).
    Ends,
    /// The embedded fonts or art did not parse (a build defect).
    Assets,
}

// ------------------------------------------------------------------ colours

const INK: &str = "#0b1014";
const WHITE: &str = "#fcfcfd";
const MINT: &str = "#28faba";
const MINT_TEXT: &str = "#7bffd6";
const GOLD: &str = "#ffb224";
const GOLD_TEXT: &str = "#ffc857";
const GREY: &str = "#8da0a7";
const NEUTRAL: &str = "#b9c6cb";
const COBALT_TEXT: &str = "#b9ccff";
const MUTED: &str = "#a6b8c1";
const SOFT: &str = "#c7dcda";

/// What colours a card: the band's, a short result's, or the neutral of a
/// whole-document check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Mint,
    Grey,
    Gold,
    Hot,
    Neutral,
}

impl Tone {
    fn of_band(b: Band) -> Tone {
        match b {
            Band::Weak => Tone::Mint,
            Band::Unclear => Tone::Grey,
            Band::Strong => Tone::Gold,
            Band::VeryStrong => Tone::Hot,
        }
    }

    /// The level arc, needle tip and hub dot.
    fn level(self) -> &'static str {
        match self {
            Tone::Mint => MINT,
            Tone::Grey => GREY,
            Tone::Gold => GOLD,
            Tone::Hot => GOLD_TEXT,
            Tone::Neutral => NEUTRAL,
        }
    }

    /// The card's rim and its inner top highlight (colour, opacity).
    fn rim(self) -> ((&'static str, f32), (&'static str, f32)) {
        match self {
            Tone::Mint => ((MINT, 0.25), (MINT_TEXT, 0.2)),
            Tone::Grey => ((GREY, 0.35), (SOFT, 0.18)),
            Tone::Gold | Tone::Hot => ((GOLD, 0.35), (GOLD_TEXT, 0.28)),
            Tone::Neutral => ((NEUTRAL, 0.3), ("#e6eef0", 0.16)),
        }
    }
}

/// The band chip: glyph, words, and (border, border opacity, text, fill,
/// fill opacity).
struct Chip {
    glyph: &'static str,
    words: &'static str,
    border: (&'static str, f32),
    ink: &'static str,
    fill: (&'static str, f32),
    glow: bool,
}

fn band_chip(b: Band) -> Chip {
    match b {
        Band::VeryStrong => Chip {
            glyph: "4",
            words: "Very strong evidence of AI",
            border: (GOLD, 0.6),
            ink: GOLD_TEXT,
            fill: (GOLD, 0.08),
            glow: true,
        },
        Band::Strong => Chip {
            glyph: "3",
            words: "Strong evidence of AI",
            border: (GOLD, 0.4),
            ink: GOLD_TEXT,
            fill: (GOLD, 0.05),
            glow: false,
        },
        Band::Unclear => Chip {
            glyph: "2",
            words: "Unclear evidence",
            border: ("#5a6b72", 1.0),
            ink: SOFT,
            fill: (INK, 0.0),
            glow: false,
        },
        Band::Weak => Chip {
            glyph: "1",
            words: "No strong signs of AI",
            border: (MINT, 0.4),
            ink: MINT_TEXT,
            fill: (MINT, 0.06),
            glow: false,
        },
    }
}

fn short_chip(r: ShortResult) -> Chip {
    match r {
        ShortResult::Strong => Chip { glyph: "!", words: "Strong signs of AI involvement", ..band_chip(Band::Strong) },
        ShortResult::None => {
            Chip { glyph: "\u{00B7}", words: "No strong signs of AI involvement", ..band_chip(Band::Weak) }
        }
    }
}

/// A scale segment's base colour.
fn segment_fill(b: Band) -> &'static str {
    match b {
        Band::Weak => "#2f4249",
        Band::Unclear => "#4a6169",
        Band::Strong => "#8a5f14",
        Band::VeryStrong => GOLD,
    }
}

/// The lit segment: its fill and its ring (colour, opacity).
fn segment_here(b: Band) -> (&'static str, (&'static str, f32)) {
    match b {
        Band::Weak => (MINT, (MINT, 0.5)),
        Band::Unclear => (GREY, (SOFT, 0.4)),
        Band::Strong => (GOLD, (GOLD, 0.5)),
        Band::VeryStrong => (GOLD, (GOLD, 0.67)),
    }
}

/// A section chip's (border, border opacity, text, fill, fill opacity).
fn section_chip(b: Band) -> ((&'static str, f32), &'static str, (&'static str, f32)) {
    match b {
        Band::Weak => ((MINT, 0.4), MINT_TEXT, (MINT, 0.06)),
        Band::Unclear => (("#5a6b72", 1.0), SOFT, ("#0b1316", 1.0)),
        Band::Strong => ((GOLD, 0.4), GOLD_TEXT, (GOLD, 0.05)),
        Band::VeryStrong => ((GOLD, 0.6), GOLD_TEXT, (GOLD, 0.13)),
    }
}

// ------------------------------------------------------------------ geometry

/// The dial's own units (the pages' 340 x 278 gauge), drawn 400 px wide.
const CX: f32 = 170.0;
const CY: f32 = 172.0;
const R: f32 = 120.0;
const GAUGE_X: f32 = 48.0;
const GAUGE_Y: f32 = 118.0;
const GAUGE_W: f32 = 400.0;
const GAUGE_SCALE: f32 = GAUGE_W / 340.0;
const GAUGE_H: f32 = 278.0 * GAUGE_SCALE;
/// The gauge column's middle line.
const MID: f32 = GAUGE_X + GAUGE_W / 2.0;
const COPY_X: f32 = 470.0;
const COPY_Y: f32 = 150.0;
const COPY_W: f32 = 370.0;
const COPY_W_WIDE: f32 = 640.0;
const NINJA_X: f32 = 820.0;
const NINJA_Y: f32 = 112.0;
const NINJA_SIZE: f32 = 340.0;
const FOOT_LEFT: f32 = 64.0;
const FOOT_RIGHT: f32 = 1136.0;
const FOOT_SIZE: f32 = 22.0;

fn pol(angle_deg: f32, r: f32) -> (f32, f32) {
    let a = angle_deg.to_radians();
    (CX + r * a.cos(), CY + r * a.sin())
}

/// An arc at radius `r` clockwise from `from` to `to` degrees.
fn arc(r: f32, from: f32, to: f32) -> String {
    let (sx, sy) = pol(from, r);
    let (ex, ey) = pol(to, r);
    let large = u8::from(to - from > 180.0);
    format!("M{} {} A{} {} 0 {large} 1 {} {}", N(sx), N(sy), N(r), N(r), N(ex), N(ey))
}

/// The baseline of a line box of `line_height` (a multiple of `size`)
/// whose top is `top`, as a browser places it.
fn baseline(top: f32, size: f32, line_height: f32) -> f32 {
    top + size * (line_height - ASCENDER - DESCENDER) / 2.0 + size * ASCENDER
}

/// "20 Nov 2026" from "2026-11-20".
fn display_date(iso: &str) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let y = iso.get(0..4).unwrap_or("");
    let m =
        iso.get(5..7).and_then(|m| m.parse::<usize>().ok()).and_then(|m| m.checked_sub(1)).and_then(|m| MONTHS.get(m));
    let d = iso.get(8..10).and_then(|d| d.parse::<u32>().ok());
    match (m, d) {
        (Some(m), Some(d)) => format!("{d} {m} {y}"),
        _ => iso.to_owned(),
    }
}

/// One `<text>` element with a single run of card text.
#[allow(clippy::too_many_arguments)]
fn text_el(
    out: &mut String,
    x: f32,
    y: f32,
    size: f32,
    weight: Weight,
    spacing: f32,
    fill: &str,
    anchor: &str,
    s: &str,
) {
    put(
        out,
        format_args!(
            r#"<text x="{}" y="{}" font-family="{}" font-size="{}" font-weight="{}" letter-spacing="{}" fill="{fill}" text-anchor="{anchor}">"#,
            N(x),
            N(y),
            fonts::FAMILY,
            N(size),
            weight.css(),
            N(spacing)
        ),
    );
    text(out, s);
    out.push_str("</text>");
}

/// A `<tspan>` run inside an open `<text>`.
fn tspan(out: &mut String, size: f32, weight: Weight, spacing: f32, fill: &str, s: &str) {
    put(
        out,
        format_args!(
            r#"<tspan font-size="{}" font-weight="{}" letter-spacing="{}" fill="{fill}">"#,
            N(size),
            weight.css(),
            N(spacing)
        ),
    );
    text(out, s);
    out.push_str("</tspan>");
}

fn open_text(out: &mut String, x: f32, y: f32, anchor: &str) {
    put(
        out,
        format_args!(r#"<text x="{}" y="{}" font-family="{}" text-anchor="{anchor}">"#, N(x), N(y), fonts::FAMILY),
    );
}

struct Faces {
    regular: Face,
    bold: Face,
}

/// Every string a card prints, for the character check.
fn strings(c: &Card) -> impl Iterator<Item = &str> {
    [
        Some(c.headline.as_str()),
        Some(c.source_label.as_str()),
        Some(c.date.as_str()),
        Some(c.model_line.as_str()),
        c.confidence_text.as_deref(),
        c.share_text.as_deref(),
        c.free_cut_note.as_deref(),
        c.document_note.as_deref(),
    ]
    .into_iter()
    .flatten()
}

/// The card as an SVG document.
pub fn svg(c: &Card, origins: &Origins, ends: GaugeEnds) -> Result<String, CardRefusal> {
    c.check().map_err(|e| CardRefusal::Invalid(e.0))?;
    let link = c.link.printed(origins);
    if !strings(c).all(fonts::all_supported) || !fonts::all_supported(&link) {
        return Err(CardRefusal::Unsupported);
    }
    if ends == GaugeEnds::Emoji {
        return Err(CardRefusal::Ends);
    }
    let faces = Faces {
        regular: Face::of(Weight::Regular).ok_or(CardRefusal::Assets)?,
        bold: Face::of(Weight::Bold).ok_or(CardRefusal::Assets)?,
    };
    let tone = match (c.variant, c.band, c.gauge.short) {
        (CardVariant::PaidDocument, _, _) => Tone::Neutral,
        (CardVariant::Short, _, Some(ShortResult::Strong)) => Tone::Gold,
        (CardVariant::Short, _, _) => Tone::Mint,
        (_, Some(b), _) => Tone::of_band(b),
        _ => Tone::Grey,
    };
    let mut out = String::with_capacity(160 * 1024);
    put(
        &mut out,
        format_args!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" viewBox="0 0 {WIDTH} {HEIGHT}">"#
        ),
    );
    background(&mut out, tone);
    top(&mut out, &faces).ok_or(CardRefusal::Assets)?;
    gauge(&mut out, c, tone, ends, &faces).ok_or(CardRefusal::Assets)?;
    let ninja = c.ninja.filter(|_| matches!(c.variant, CardVariant::Band | CardVariant::Short));
    copy(&mut out, c, &faces, if ninja.is_some() { COPY_W } else { COPY_W_WIDE });
    if let Some(state) = ninja {
        figure(&mut out, state).ok_or(CardRefusal::Assets)?;
    }
    foot(&mut out, c, origins, &faces);
    out.push_str("</svg>");
    Ok(out)
}

fn background(out: &mut String, tone: Tone) {
    // linear-gradient(145deg, #182024, #0e1519 58%, #0a1014) over the card,
    // a mint glow behind the figure and a gold glow under the gauge.
    out.push_str(concat!(
        r##"<defs><linearGradient id="c-bg" gradientUnits="userSpaceOnUse" x1="254.6" y1="-178.3" x2="945.4" y2="808.3">"##,
        r##"<stop offset="0" stop-color="#182024"/><stop offset=".58" stop-color="#0e1519"/><stop offset="1" stop-color="#0a1014"/></linearGradient>"##,
        r##"<radialGradient id="c-glow-m" gradientUnits="userSpaceOnUse" cx="0" cy="0" r="1" gradientTransform="translate(1008 302.4) scale(408 378)">"##,
        r##"<stop offset="0" stop-color="#28faba" stop-opacity=".18"/><stop offset=".4" stop-color="#28faba" stop-opacity=".07"/><stop offset=".7" stop-color="#28faba" stop-opacity="0"/></radialGradient>"##,
        r##"<radialGradient id="c-glow-g" gradientUnits="userSpaceOnUse" cx="0" cy="0" r="1" gradientTransform="translate(216 504) scale(480 315)">"##,
        r##"<stop offset="0" stop-color="#ffb224" stop-opacity=".12"/><stop offset=".7" stop-color="#ffb224" stop-opacity="0"/></radialGradient>"##,
        r##"</defs>"##,
        r##"<rect width="1200" height="630" fill="#0b1014"/><rect width="1200" height="630" fill="url(#c-bg)"/>"##,
        r##"<rect width="1200" height="630" fill="url(#c-glow-m)"/><rect width="1200" height="630" fill="url(#c-glow-g)"/>"##,
    ));
    let ((rim, ro), (hi, ho)) = tone.rim();
    put(
        out,
        format_args!(
            r#"<rect x="1" y="1" width="1198" height="628" fill="none" stroke="{rim}" stroke-opacity="{}" stroke-width="2"/><rect x="2" y="2" width="1196" height="2" fill="{hi}" fill-opacity="{}"/>"#,
            N(ro),
            N(ho)
        ),
    );
}

fn top(out: &mut String, f: &Faces) -> Option<()> {
    let (x, y, w) = (64.0, 48.0, 236.0);
    let h = w * 208.0 / 952.0;
    art::WORDMARK.nested(out, x, y, w, h)?;
    // The pill: 17 px bold uppercase letters, tracked .1em, 14 px side padding.
    let label = "DETECTOR REPORT";
    let (size, spacing) = (17.0, 1.7);
    let tw = f.bold.width(label, size, spacing) - spacing;
    let px = x + w + 18.0;
    let (ph, cy) = (36.6, y + h / 2.0);
    put(
        out,
        format_args!(
            r##"<rect x="{}" y="{}" width="{}" height="{}" rx="{}" fill="none" stroke="#456168" stroke-width="1"/>"##,
            N(px + 0.5),
            N(cy - ph / 2.0),
            N(tw + 28.0),
            N(ph),
            N(ph / 2.0)
        ),
    );
    text_el(
        out,
        px + 14.0 + tw / 2.0,
        cy + size * UPPER_HEIGHT / 2.0,
        size,
        Weight::Bold,
        spacing,
        "#c0d7d4",
        "middle",
        label,
    );
    Some(())
}

/// The dial, its number, chip and scale.
fn gauge(out: &mut String, c: &Card, tone: Tone, ends: GaugeEnds, f: &Faces) -> Option<()> {
    let level = c.gauge.level.filter(|_| c.variant != CardVariant::Short);
    let short = c.variant == CardVariant::Short;
    let colour = tone.level();
    put(out, format_args!(r#"<g transform="translate({} {}) scale({})">"#, N(GAUGE_X), N(GAUGE_Y), GAUGE_SCALE));
    out.push_str(concat!(
        r##"<defs><linearGradient id="c-hot" gradientUnits="userSpaceOnUse" x1="60" y1="230" x2="280" y2="230">"##,
        r##"<stop offset="0" stop-color="#ffb224"/><stop offset=".55" stop-color="#ffc857"/><stop offset="1" stop-color="#ffe29a"/></linearGradient>"##,
        r##"<radialGradient id="c-face" cx=".5" cy=".6" r=".6"><stop offset="0" stop-color="#121c21"/><stop offset="1" stop-color="#0b1014" stop-opacity="0"/></radialGradient>"##,
        r##"<filter id="c-level-glow" color-interpolation-filters="sRGB" x="-20%" y="-20%" width="140%" height="140%"><feDropShadow dx="0" dy="0" stdDeviation="5" flood-color="#ffb224" flood-opacity=".67"/></filter>"##,
        r##"</defs>"##,
        r##"<circle cx="170" cy="172" r="108" fill="url(#c-face)"/>"##,
    ));
    let track = arc(R, 150.0, 390.0);
    put(
        out,
        format_args!(r##"<path d="{}" fill="none" stroke="#30454d" stroke-width="1"/>"##, arc(R + 11.0, 150.0, 390.0)),
    );
    put(out, format_args!(r##"<path d="{track}" fill="none" stroke="#1c2a30" stroke-width="18"/>"##));
    if let Some(l) = level.filter(|l| *l > 0) {
        let d = arc(R, 150.0, 150.0 + 2.4 * f32::from(l.min(100)));
        let (stroke, filter) =
            if tone == Tone::Hot { ("url(#c-hot)", r#" filter="url(#c-level-glow)""#) } else { (colour, "") };
        put(out, format_args!(r#"<path d="{d}" fill="none" stroke="{stroke}" stroke-width="18"{filter}/>"#));
    }
    put(
        out,
        format_args!(r##"<path d="{}" fill="none" stroke="#30454d" stroke-width="1"/>"##, arc(R - 11.0, 150.0, 390.0)),
    );
    // Ticks: minor every 10%, major and labelled at 25, 50 and 75 and major
    // at the ends (hidden on a short gauge).
    let tick = |out: &mut String, angle: f32, major: bool| {
        let (x1, y1) = pol(angle, R - 16.0);
        let (x2, y2) = pol(angle, R - if major { 28.0 } else { 23.0 });
        let (stroke, width) = if major { (GREY, 2.5) } else { ("#4a5e66", 2.0) };
        put(
            out,
            format_args!(
                r#"<line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{stroke}" stroke-width="{}" stroke-linecap="round"/>"#,
                N(x1),
                N(y1),
                N(x2),
                N(y2),
                N(width)
            ),
        );
    };
    for i in 0..=10u8 {
        let end = i == 0 || i == 10;
        if end && short {
            continue;
        }
        tick(out, 150.0 + f32::from(i) * 24.0, i % 5 == 0);
    }
    for pct in [25.0f32, 75.0] {
        tick(out, 150.0 + pct * 2.4, true);
    }
    for (pct, label) in [(25.0f32, "25"), (50.0, "50"), (75.0, "75")] {
        let (x, y) = pol(150.0 + pct * 2.4, R - 40.0);
        text_el(out, x, y + 4.0, 11.0, Weight::Bold, 0.44, GREY, "middle", label);
    }
    gauge_ends(out, ends)?;
    if level.is_some() {
        let rot = -120.0 + 2.4 * f32::from(level.unwrap_or(0).min(100));
        put(
            out,
            format_args!(
                r##"<g transform="rotate({} 170 172)"><path d="M166 172 L170 68 L174 172 Z" fill="#f2f6f7"/><path d="M167.4 100 L170 68 L172.6 100 Z" fill="{colour}"/></g>"##,
                N(rot)
            ),
        );
    }
    put(
        out,
        format_args!(
            r##"<circle cx="170" cy="172" r="11" fill="#0b1014" stroke="#5a6b72" stroke-width="2"/><circle cx="170" cy="172" r="4" fill="{colour}"/>"##
        ),
    );
    out.push_str("</g>");

    // The number and its label, in card pixels.
    if let Some(l) = level {
        let top = GAUGE_Y + 0.67 * GAUGE_H;
        open_text(out, MID, baseline(top, 72.0, 0.95), "middle");
        tspan(out, 72.0, Weight::Bold, -4.32, WHITE, &l.min(100).to_string());
        tspan(out, 36.0, Weight::Bold, -0.72, SOFT, "%");
        out.push_str("</text>");
        let label_top = top + 72.0 * 0.95 + 6.0;
        text_el(
            out,
            MID,
            baseline(label_top, 17.0, 1.21),
            17.0,
            Weight::Bold,
            2.04,
            "#a7bbc2",
            "middle",
            "ESTIMATED AI SHARE",
        );
    } else if short {
        let top = GAUGE_Y + 0.71 * GAUGE_H;
        text_el(out, MID, baseline(top, 17.0, 1.21), 17.0, Weight::Bold, 2.72, SOFT, "middle", "NO SHARE");
    }

    let chip_top = GAUGE_Y + GAUGE_H + 10.0;
    match (c.variant, c.band, c.gauge.short) {
        (CardVariant::PaidDocument, _, _) => sections(out, &c.ladder, chip_top),
        (CardVariant::Short, _, Some(r)) => {
            let bottom = chip(out, &short_chip(r), chip_top, &f.bold);
            short_scale(out, r, bottom + 12.0);
        }
        (_, Some(b), _) => {
            let bottom = chip(out, &band_chip(b), chip_top, &f.bold);
            band_scale(out, b, bottom + 12.0);
        }
        _ => {}
    }
    Some(())
}

/// The ends, in dial units: the busts clipped to light discs (default) or
/// H and S.
fn gauge_ends(out: &mut String, ends: GaugeEnds) -> Option<()> {
    let (ex, ey) = pol(150.0, R + 26.0);
    let (fx, fy) = pol(30.0, R + 26.0);
    match ends {
        GaugeEnds::Letters => {
            text_el(out, ex, ey + 6.0, 16.0, Weight::Bold, -0.32, SOFT, "middle", GaugeEnds::LETTERS.0);
            text_el(out, fx, fy + 6.0, 16.0, Weight::Bold, -0.32, SOFT, "middle", GaugeEnds::LETTERS.1);
        }
        GaugeEnds::Icons | GaugeEnds::Emoji => {
            for (x, y, a, id) in
                [(ex - 16.0, ey + 4.0, art::HUMAN, "c-clip-h"), (fx + 16.0, fy + 4.0, art::ROBOT, "c-clip-r")]
            {
                bust(out, x, y + 2.0, a, id)?;
            }
        }
    }
    Some(())
}

fn bust(out: &mut String, x: f32, cy: f32, a: Art, id: &str) -> Option<()> {
    let r = 16.0;
    put(
        out,
        format_args!(
            r##"<clipPath id="{id}"><circle cx="{}" cy="{}" r="16"/></clipPath><circle cx="{}" cy="{}" r="16" fill="#c9d6db"/><g clip-path="url(#{id})">"##,
            N(x),
            N(cy),
            N(x),
            N(cy)
        ),
    );
    a.nested(out, x - r, cy - r, 2.0 * r, 2.0 * r)?;
    put(
        out,
        format_args!(
            r##"</g><circle cx="{}" cy="{}" r="15.5" fill="none" stroke="#1ee8b0" stroke-opacity=".45" stroke-width="1"/>"##,
            N(x),
            N(cy)
        ),
    );
    Some(())
}

/// The chip under the gauge; returns its bottom.
fn chip(out: &mut String, ch: &Chip, top: f32, bold: &Face) -> f32 {
    let size = 17.0;
    let spacing = 0.34;
    let tw = bold.width(ch.words, size, spacing) - spacing;
    let h = 42.6;
    let w = 18.0 + 20.0 + 9.0 + tw + 18.0;
    let x = MID - w / 2.0;
    let cy = top + h / 2.0;
    if ch.glow {
        put(
            out,
            format_args!(
                r##"<filter id="c-chip-glow" color-interpolation-filters="sRGB" x="-30%" y="-60%" width="160%" height="220%"><feGaussianBlur stdDeviation="9"/></filter><rect x="{}" y="{}" width="{}" height="{}" rx="{}" fill="#ffb224" fill-opacity=".5" filter="url(#c-chip-glow)"/>"##,
                N(x + 6.0),
                N(top + 6.0),
                N(w - 12.0),
                N(h - 12.0),
                N((h - 12.0) / 2.0)
            ),
        );
        put(
            out,
            format_args!(
                r##"<rect x="{}" y="{}" width="{}" height="{}" rx="{}" fill="#0b1014"/>"##,
                N(x),
                N(top),
                N(w),
                N(h),
                N(h / 2.0)
            ),
        );
    }
    let ((border, bo), (fill, fo)) = (ch.border, ch.fill);
    put(
        out,
        format_args!(
            r#"<rect x="{}" y="{}" width="{}" height="{}" rx="{}" fill="{fill}" fill-opacity="{}" stroke="{border}" stroke-opacity="{}" stroke-width="1"/>"#,
            N(x + 0.5),
            N(top + 0.5),
            N(w - 1.0),
            N(h - 1.0),
            N((h - 1.0) / 2.0),
            N(fo),
            N(bo)
        ),
    );
    let gx = x + 18.0 + 10.0;
    put(
        out,
        format_args!(
            r#"<circle cx="{}" cy="{}" r="9.5" fill="none" stroke="{}" stroke-width="1"/>"#,
            N(gx),
            N(cy),
            ch.ink
        ),
    );
    text_el(out, gx, cy + 11.0 * UPPER_HEIGHT / 2.0, 11.0, Weight::Bold, 0.0, ch.ink, "middle", ch.glyph);
    text_el(out, x + 18.0 + 20.0 + 9.0, cy + size * 0.36, size, Weight::Bold, spacing, ch.ink, "start", ch.words);
    top + h
}

/// The four-band scale, the card's band lit, glyphs under it.
fn band_scale(out: &mut String, here: Band, top: f32) {
    let (w, gap, h) = (300.0f32, 3.0f32, 10.0f32);
    let seg = (w - 3.0 * gap) / 4.0;
    let x0 = MID - w / 2.0;
    for (i, b) in Band::ALL.iter().copied().enumerate() {
        let x = x0 + i as f32 * (seg + gap);
        let lit = b == here;
        if lit {
            let (fill, (ring, ro)) = segment_here(b);
            if matches!(b, Band::Weak | Band::VeryStrong) {
                put(
                    out,
                    format_args!(
                        r#"<filter id="c-seg-glow" color-interpolation-filters="sRGB" x="-50%" y="-200%" width="200%" height="500%"><feGaussianBlur stdDeviation="6"/></filter><rect x="{}" y="{}" width="{}" height="{}" rx="3" fill="{fill}" fill-opacity=".45" filter="url(#c-seg-glow)"/>"#,
                        N(x),
                        N(top),
                        N(seg),
                        N(h)
                    ),
                );
            }
            put(
                out,
                format_args!(
                    r##"<rect x="{}" y="{}" width="{}" height="{}" rx="4" fill="{ring}" fill-opacity="{}"/><rect x="{}" y="{}" width="{}" height="{}" rx="3" fill="#0b1014"/><rect x="{}" y="{}" width="{}" height="{}" rx="2" fill="{fill}"/>"##,
                    N(x - 2.0),
                    N(top - 2.0),
                    N(seg + 4.0),
                    N(h + 4.0),
                    N(ro),
                    N(x - 1.0),
                    N(top - 1.0),
                    N(seg + 2.0),
                    N(h + 2.0),
                    N(x),
                    N(top),
                    N(seg),
                    N(h)
                ),
            );
        } else {
            put(
                out,
                format_args!(
                    r##"<rect x="{}" y="{}" width="{}" height="{}" rx="2" fill="{}"/><rect x="{}" y="{}" width="{}" height="{}" rx="2" fill="#0b1014" fill-opacity=".45"/>"##,
                    N(x),
                    N(top),
                    N(seg),
                    N(h),
                    segment_fill(b),
                    N(x),
                    N(top),
                    N(seg),
                    N(h)
                ),
            );
        }
        let glyph = match b {
            Band::Weak => "1",
            Band::Unclear => "2",
            Band::Strong => "3",
            Band::VeryStrong => "4",
        };
        text_el(
            out,
            x + seg / 2.0,
            baseline(top + 14.0, 15.0, 1.21),
            15.0,
            Weight::Bold,
            0.3,
            if lit { SOFT } else { GREY },
            "middle",
            glyph,
        );
    }
}

/// A short text's two-step scale.
fn short_scale(out: &mut String, r: ShortResult, top: f32) {
    let (w, gap, h) = (300.0f32, 3.0f32, 10.0f32);
    let seg = (w - gap) / 2.0;
    let x0 = MID - w / 2.0;
    for (i, (b, words, lit)) in [
        (Band::Weak, "no strong signs", r == ShortResult::None),
        (Band::Strong, "strong signs", r == ShortResult::Strong),
    ]
    .into_iter()
    .enumerate()
    {
        let x = x0 + i as f32 * (seg + gap);
        if lit {
            let (fill, (ring, ro)) = segment_here(b);
            put(
                out,
                format_args!(
                    r##"<rect x="{}" y="{}" width="{}" height="{}" rx="4" fill="{ring}" fill-opacity="{}"/><rect x="{}" y="{}" width="{}" height="{}" rx="3" fill="#0b1014"/><rect x="{}" y="{}" width="{}" height="{}" rx="2" fill="{fill}"/>"##,
                    N(x - 2.0),
                    N(top - 2.0),
                    N(seg + 4.0),
                    N(h + 4.0),
                    N(ro),
                    N(x - 1.0),
                    N(top - 1.0),
                    N(seg + 2.0),
                    N(h + 2.0),
                    N(x),
                    N(top),
                    N(seg),
                    N(h)
                ),
            );
        } else {
            put(
                out,
                format_args!(
                    r##"<rect x="{}" y="{}" width="{}" height="{}" rx="2" fill="{}"/><rect x="{}" y="{}" width="{}" height="{}" rx="2" fill="#0b1014" fill-opacity=".45"/>"##,
                    N(x),
                    N(top),
                    N(seg),
                    N(h),
                    segment_fill(b),
                    N(x),
                    N(top),
                    N(seg),
                    N(h)
                ),
            );
        }
        text_el(
            out,
            x + seg / 2.0,
            baseline(top + 14.0, 15.0, 1.21),
            15.0,
            Weight::Bold,
            0.0,
            if lit { SOFT } else { GREY },
            "middle",
            words,
        );
    }
}

/// Section chips that fit in one row of this many.
const SECTION_ROW: usize = 9;
/// Up to two rows of numbered chips; a longer document draws one strip with
/// a cell per section (runs of one band merged).
const SECTION_CHIPS: usize = 2 * SECTION_ROW;

/// A whole-document check's section ladder in place of the chip and scale.
fn sections(out: &mut String, ladder: &[Band], top: f32) {
    let (w, h) = (300.0f32, 28.0f32);
    let x0 = MID - w / 2.0;
    if ladder.len() <= SECTION_CHIPS {
        let gap = 6.0;
        let cw = 28.0;
        for (row, chunk) in ladder.chunks(SECTION_ROW).enumerate() {
            let n = chunk.len() as f32;
            let row_w = n * cw + (n - 1.0) * gap;
            let y = top + row as f32 * (h + gap);
            for (i, b) in chunk.iter().copied().enumerate() {
                let x = MID - row_w / 2.0 + i as f32 * (cw + gap);
                let ((border, bo), ink, (fill, fo)) = section_chip(b);
                put(
                    out,
                    format_args!(
                        r#"<rect x="{}" y="{}" width="{}" height="{}" rx="6" fill="{fill}" fill-opacity="{}" stroke="{border}" stroke-opacity="{}" stroke-width="1"/>"#,
                        N(x + 0.5),
                        N(y + 0.5),
                        N(cw - 1.0),
                        N(h - 1.0),
                        N(fo),
                        N(bo)
                    ),
                );
                let n = (row * SECTION_ROW + i + 1).to_string();
                text_el(
                    out,
                    x + cw / 2.0,
                    y + h / 2.0 + 13.0 * UPPER_HEIGHT / 2.0,
                    13.0,
                    Weight::Bold,
                    0.0,
                    ink,
                    "middle",
                    &n,
                );
            }
        }
        return;
    }
    // A strip: each section a cell of the 300 px width, runs merged.
    let cell = w / ladder.len() as f32;
    let y = top + 8.0;
    let mut i = 0usize;
    while i < ladder.len() {
        let b = ladder.get(i).copied().unwrap_or(Band::Weak);
        let mut j = i + 1;
        while ladder.get(j) == Some(&b) {
            j += 1;
        }
        let ((_, _), ink, _) = section_chip(b);
        let fill = if b == Band::Unclear { GREY } else { ink };
        put(
            out,
            format_args!(
                r#"<rect x="{}" y="{}" width="{}" height="12" fill="{fill}"/>"#,
                N(x0 + i as f32 * cell),
                N(y),
                N((j - i) as f32 * cell)
            ),
        );
        i = j;
    }
}

/// The middle column.
fn copy(out: &mut String, c: &Card, f: &Faces, width: f32) {
    let (size, lines) = layout::headline(&f.bold, &c.headline, width);
    let lh = 1.02 * size;
    let mut y = COPY_Y;
    for (i, l) in lines.iter().enumerate() {
        text_el(
            out,
            COPY_X,
            baseline(y + i as f32 * lh, size, 1.02),
            size,
            Weight::Bold,
            -0.05 * size,
            WHITE,
            "start",
            l,
        );
    }
    y += lines.len() as f32 * lh;

    match (c.variant, c.confidence_text.as_deref(), c.document_note.as_deref()) {
        (CardVariant::PaidDocument, _, Some(note)) => {
            let top = y + 16.0;
            let (s, t) = layout::fit(&f.bold, note, 26.0, 18.0, -0.02, width);
            text_el(out, COPY_X, baseline(top, s, 1.15), s, Weight::Bold, -0.02 * s, "#e6eef0", "start", &t);
            y = top + 1.15 * 26.0;
        }
        (_, Some(conf), _) => {
            let top = y + 16.0;
            let lead = "Confidence ";
            let mut s = 30.0f32;
            while s > 22.0 && f.bold.width(&format!("{lead}{conf}"), s, -0.03 * s) > width {
                s -= 1.0;
            }
            open_text(out, COPY_X, baseline(top, s, 1.15), "start");
            tspan(out, s, Weight::Bold, -0.03 * s, "#e6eef0", lead);
            tspan(out, s, Weight::Bold, -0.03 * s, WHITE, conf);
            out.push_str("</text>");
            y = top + 1.15 * 30.0;
        }
        _ => {}
    }

    // The source, the date and a free-cut note: 26 px, 1.35 line height.
    let mut src: Vec<String> = Vec::new();
    let fixed = [card::SECRET_LABEL, card::BLUESKY_LABEL, card::X_LABEL];
    if fixed.contains(&c.source_label.as_str()) || c.source_label.is_empty() {
        if !c.source_label.is_empty() {
            src.push(c.source_label.clone());
        }
    } else {
        let one = format!("a page on {}", c.source_label);
        if f.regular.width(&one, 26.0, -0.26) <= width {
            src.push(one);
        } else {
            src.push("a page on".to_owned());
            src.push(c.source_label.clone());
        }
    }
    src.push(format!("checked {}", display_date(&c.date)));
    if let Some(n) = c.free_cut_note.as_deref() {
        src.push(n.to_owned());
    }
    let top = y + 20.0;
    for (i, line) in src.iter().enumerate() {
        let (s, t) = layout::fit(&f.regular, line, 26.0, 18.0, -0.01, width);
        text_el(
            out,
            COPY_X,
            baseline(top + i as f32 * 35.1, 26.0, 1.35),
            s,
            Weight::Regular,
            -0.01 * s,
            MUTED,
            "start",
            &t,
        );
    }
}

/// The ninja, with its halo and contact shadow.
fn figure(out: &mut String, state: NinjaState) -> Option<()> {
    let (sx, sw) = art::sole(state);
    let cx = NINJA_X + sx * NINJA_SIZE;
    let cy = NINJA_Y + 0.969 * NINJA_SIZE;
    let rx = sw * NINJA_SIZE / 2.0;
    put(
        out,
        format_args!(
            concat!(
                r##"<filter id="c-sole-a" color-interpolation-filters="sRGB" x="-50%" y="-200%" width="200%" height="500%"><feGaussianBlur stdDeviation="7"/></filter>"##,
                r##"<filter id="c-sole-b" color-interpolation-filters="sRGB" x="-50%" y="-200%" width="200%" height="500%"><feGaussianBlur stdDeviation="10"/></filter>"##,
                r##"<ellipse cx="{cx}" cy="{cyb}" rx="{rxb}" ry="20" fill="#0b1014" fill-opacity=".8" filter="url(#c-sole-b)"/>"##,
                r##"<ellipse cx="{cx}" cy="{cy}" rx="{rx}" ry="12" fill="#05080a" filter="url(#c-sole-a)"/>"##,
                r##"<filter id="c-halo" color-interpolation-filters="sRGB" x="-20%" y="-20%" width="140%" height="140%">"##,
                r##"<feDropShadow dx="0" dy="0" stdDeviation="5" flood-color="#0b1014"/><feDropShadow dx="0" dy="0" stdDeviation="14" flood-color="#0b1014"/></filter>"##,
                r##"<g filter="url(#c-halo)">"##
            ),
            cx = N(cx),
            cy = N(cy),
            cyb = N(cy + 6.0),
            rx = N(rx + 4.0),
            rxb = N(rx + 12.0),
        ),
    );
    art::ninja(state).nested(out, NINJA_X, NINJA_Y, NINJA_SIZE, NINJA_SIZE)?;
    out.push_str("</g>");
    Some(())
}

/// The foot: the link and the model line on one baseline.
fn foot(out: &mut String, c: &Card, origins: &Origins, f: &Faces) {
    let y = baseline(630.0 - 40.0 - FOOT_SIZE * 1.21, FOOT_SIZE, 1.21);
    let (lead, tail, tail_fill) = match &c.link {
        CardLink::Public { slug } => (format!("{}/", origins.site.authority()), format!("d/{slug}"), MINT_TEXT),
        CardLink::SecretHost => (String::new(), origins.secret.authority().to_owned(), COBALT_TEXT),
    };
    let span = FOOT_RIGHT - FOOT_LEFT;
    // The link keeps at most 60% of the foot; the model line the rest.
    let whole = format!("{lead}{tail}");
    let mut s = FOOT_SIZE;
    while s > 14.0 && f.regular.width(&lead, s, 0.0) + f.bold.width(&tail, s, 0.0) > 0.6 * span {
        s -= 1.0;
    }
    let link_w = f.regular.width(&lead, s, 0.0) + f.bold.width(&tail, s, 0.0);
    if link_w <= 0.6 * span {
        open_text(out, FOOT_LEFT, y, "start");
        tspan(out, s, Weight::Regular, 0.0, MUTED, &lead);
        tspan(out, s, Weight::Bold, 0.0, tail_fill, &tail);
        out.push_str("</text>");
    } else {
        let (s2, t) = layout::fit(&f.bold, &whole, s, 14.0, 0.0, 0.6 * span);
        text_el(out, FOOT_LEFT, y, s2, Weight::Bold, 0.0, tail_fill, "start", &t);
    }
    let room = span - link_w.min(0.6 * span) - 40.0;
    let (ms, mt) = layout::fit(&f.regular, &c.model_line, FOOT_SIZE, 14.0, 0.0, room);
    text_el(out, FOOT_RIGHT, y, ms, Weight::Regular, 0.0, MUTED, "end", &mt);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_print_as_day_month_year() {
        assert_eq!(display_date("2026-11-20"), "20 Nov 2026");
        assert_eq!(display_date("2026-01-05"), "5 Jan 2026");
        assert_eq!(display_date("2026-13-05"), "2026-13-05");
    }

    #[test]
    fn the_dial_matches_the_page_gauge_geometry() {
        assert_eq!(arc(R, 150.0, 390.0), "M66.08 232 A120 120 0 1 1 273.92 232");
        assert_eq!(arc(R + 11.0, 150.0, 390.0), "M56.55 237.5 A131 131 0 1 1 283.45 237.5");
        let (ex, ey) = pol(150.0, R + 26.0);
        assert_eq!((N(ex - 16.0).to_string(), N(ey + 6.0).to_string()), ("27.56".into(), "251".into()));
        assert!((GAUGE_H - 327.06).abs() < 0.01);
    }
}
