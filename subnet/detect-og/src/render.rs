//! Rasterizing a card: resvg 0.50 in the Worker's own memory, then PNG.
//!
//! The SVG comes only from [`crate::card::svg`]. The parser options load
//! the two embedded faces and nothing else (no system fonts), set no
//! resources directory and resolve no `<image>` reference of any kind, data
//! URLs included, so rendering reads no file and fetches nothing. The PNG
//! is tiny-skia's encoding of the 1200 x 630 RGBA pixmap, with no text, time
//! or other metadata chunk, so equal cards give equal bytes.

use crate::card::{self, CardRefusal, HEIGHT, WIDTH};
use crate::fonts;
use resvg::{tiny_skia, usvg};
use slop_ninja_detect_page::{Card, GaugeEnds, Origins};
use std::sync::Arc;

/// Why a render failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderError {
    /// The card was refused before drawing.
    Card(CardRefusal),
    /// resvg could not parse the template's output (a template defect).
    Parse,
    /// The pixmap could not be allocated.
    Pixmap,
    /// PNG encoding failed.
    Encode,
}

/// The parser options: the embedded faces only, no file and no reference
/// resolution.
pub fn options() -> usvg::Options<'static> {
    let mut opt = usvg::Options {
        resources_dir: None,
        font_family: fonts::FAMILY.to_owned(),
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    };
    let db = opt.fontdb_mut();
    for face in [fonts::REGULAR, fonts::BOLD] {
        db.load_font_source(usvg::fontdb::Source::Binary(Arc::new(face)));
    }
    db.set_sans_serif_family(fonts::FAMILY);
    opt
}

/// Rasterizes an SVG document of the card's size into PNG bytes.
pub fn png(svg: &str) -> Result<Vec<u8>, RenderError> {
    let tree = usvg::Tree::from_str(svg, &options()).map_err(|_| RenderError::Parse)?;
    let mut pixmap = tiny_skia::Pixmap::new(WIDTH, HEIGHT).ok_or(RenderError::Pixmap)?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    drop(tree);
    pixmap.encode_png().map_err(|_| RenderError::Encode)
}

/// A card's PNG.
pub fn card_png(c: &Card, origins: &Origins, ends: GaugeEnds) -> Result<Vec<u8>, RenderError> {
    let svg = card::svg(c, origins, ends).map_err(RenderError::Card)?;
    png(&svg)
}
