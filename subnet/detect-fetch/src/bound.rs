//! The fetch Worker's bounds, each derived from a measurement or a
//! structure and versioned with the extractor (risks F11, F13, F14).
//!
//! **One page's memory.** A fetch reserves one share of the isolate's fetch
//! memory ([`crate::routes::FETCH_SHARE_BYTES`]) before it reads a byte. The
//! strip pass holds at most [`HTML_BOUND`] bytes of stripped HTML, and
//! lol_html's own buffer is held to the same bound, so the raw body streams
//! through in constant memory and a compression bomb is caught by the
//! decoded count of what is kept. Building the DOM and extracting then
//! costs, by the native arena witnesses on the pinned versions,
//! [`EXTRACT_BYTES_PER_HTML_BYTE`] per stripped byte plus
//! [`BYTES_PER_ELEMENT`] per element ([`page_work`]). So
//! `HTML_BOUND = share / EXTRACT_BYTES_PER_HTML_BYTE` (the design's formula)
//! and `MAX_ELEMENTS = share / BYTES_PER_ELEMENT`, and after the strip pass
//! has counted both, before any DOM exists, a page whose work exceeds the
//! share is refused ("This page is too large to read"), never truncated
//! mid-parse. dom_smoothie's `max_elements_to_parse` is set to
//! [`MAX_ELEMENTS`] as a second check.
//!
//! **Depth.** A DOM-building extractor recurses over the tree, and the wasm32
//! stack is 1 MiB outside every arena. [`DEPTH_BOUND`] is a quarter of a
//! nesting the extractor survives natively on a 1 MiB thread stack (native
//! frames are larger than wasm32's, so it bounds wasm32 from above): the
//! strip pass refuses deeper pages before extraction.
//!
//! Measured 2026-11-13 (native, `arena_witness`, debug build, 64-bit):
//! text-dense pages 7.4 to 7.8 B per stripped byte (100 KB to 912 KB);
//! element-dense pages about 630 B per element beyond the text term
//! (60,003 `div`/`span` elements) and 1,390 B per element for 40,003 links;
//! the extractor survived nesting 2,048 deep on a 1 MiB stack
//! (`extract::tests::the_depth_bound_fits_the_stack`).

use crate::routes::FETCH_SHARE_BYTES;

/// The extractor's version, stored with every run.
pub const EXTRACT_V: &str = "extract_v1 lol_html=3.0.3 dom_smoothie=0.19.0";

/// Peak arena bytes per byte of stripped HTML for text-dense pages (the
/// strip output, the DOM, the extraction, the text and the answer):
/// measured 7.8, rounded up with a margin to 10.
pub const EXTRACT_BYTES_PER_HTML_BYTE: usize = 10;

/// Peak arena bytes per element beyond the text term: measured 1,390 at the
/// worst (link-dense pages), rounded up to 2,048.
pub const BYTES_PER_ELEMENT: usize = 2048;

/// A nesting the extractor survives on a 1 MiB native stack, measured by
/// `extract::tests::the_depth_bound_fits_the_stack`.
pub const SURVIVED_DEPTH: usize = 2048;

/// The nesting bound: a quarter of [`SURVIVED_DEPTH`].
pub const DEPTH_BOUND: usize = SURVIVED_DEPTH / 4;

/// The stripped HTML bound.
pub const HTML_BOUND: usize = FETCH_SHARE_BYTES / EXTRACT_BYTES_PER_HTML_BYTE;

/// The element bound.
pub const MAX_ELEMENTS: usize = FETCH_SHARE_BYTES / BYTES_PER_ELEMENT;

/// The work a page of `stripped` bytes and `elements` elements needs.
pub const fn page_work(stripped: usize, elements: usize) -> usize {
    stripped.saturating_mul(EXTRACT_BYTES_PER_HTML_BYTE).saturating_add(elements.saturating_mul(BYTES_PER_ELEMENT))
}

/// Whether a stripped page fits one share.
pub const fn fits(stripped: usize, elements: usize) -> bool {
    page_work(stripped, elements) <= FETCH_SHARE_BYTES
}

/// One metadata value kept from a page (title, author, ...), at most; longer
/// values are cut at a character boundary.
pub const META_BYTES: usize = 2 * 1024;

/// JSON-LD read for the paywall marker, at most per page.
pub const LD_JSON_BYTES: usize = 64 * 1024;

/// The first bytes read before the charset is decided (the HTML standard's
/// prescan window).
pub const PRESCAN_BYTES: usize = 1024;

/// A DNS-over-HTTPS answer, at most.
pub const DOH_BYTES: usize = 64 * 1024;

/// Fewer extracted words than this, with a password field on the page,
/// reads as a login wall.
pub const LOGIN_WALL_WORDS: usize = 150;

const _: () = assert!(HTML_BOUND >= 2 * 1024 * 1024, "a stripped page of 2 MiB must fit");
const _: () = assert!(MAX_ELEMENTS >= 16_000, "a long page's elements must fit");
const _: () = assert!(fits(HTML_BOUND, 0) && fits(0, MAX_ELEMENTS));
const _: () = assert!(DEPTH_BOUND >= 256, "real pages nest far less than 256");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_are_derived() {
        assert_eq!(HTML_BOUND, FETCH_SHARE_BYTES / EXTRACT_BYTES_PER_HTML_BYTE);
        assert_eq!(MAX_ELEMENTS, FETCH_SHARE_BYTES / BYTES_PER_ELEMENT);
        assert!(fits(HTML_BOUND / 2, MAX_ELEMENTS / 2));
        assert!(!fits(HTML_BOUND, MAX_ELEMENTS / 2));
    }
}
