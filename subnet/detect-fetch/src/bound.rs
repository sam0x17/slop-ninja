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
//! and `MAX_ELEMENTS = share / BYTES_PER_ELEMENT`. After the strip pass has
//! counted the stripped bytes and the start tags, a page whose work exceeds
//! the share is refused ("This page is too large to read"), never truncated
//! mid-parse.
//!
//! **The DOM.** The tree builder can create far more than the page's tags:
//! it reconstructs open formatting elements, attributes and all, in every
//! later block, and the adoption agency clones them. So the DOM build
//! ([`crate::dom`]) counts the elements, nodes and attributes it creates as
//! it creates them and stops once they pass what the page's reservation
//! allows ([`DomBounds::within`]), before any extraction; the reservation is
//! then shrunk to the DOM's [`counted_elements`]. dom_smoothie's
//! `max_elements_to_parse` is set to [`MAX_ELEMENTS`] as a further check.
//!
//! **Depth.** A DOM-building extractor recurses over the tree, and the wasm32
//! stack is 1 MiB outside every arena. [`DEPTH_BOUND`] is a quarter of a
//! nesting the extractor survives natively on a 1 MiB thread stack (native
//! frames are larger than wasm32's, so it bounds wasm32 from above). The
//! tree builder's scopes decide the nesting, so no tag-level estimate bounds
//! it: the DOM build measures the built tree by an iterative walk and refuses
//! a deeper page before extraction.
//!
//! Measured 2026-11-13 (native, `arena_witness`, debug build, 64-bit):
//! text-dense pages 7.4 to 7.8 B per stripped byte (100 KB to 4.56 MB);
//! beyond the text term, 630 B per element before the bounded DOM build and
//! 750 B after it for element-dense pages (60,003 `div`/`span` elements),
//! 1,390 B and 1,345 B for 40,003 links, and about 600 B with five
//! attributes on every element; pages the DOM build refuses peaked at 5 to
//! 10 MiB; the extractor survived nesting 2,048 deep on a 1 MiB stack
//! (`extract::tests::the_depth_bound_fits_the_stack`). Every nesting shape
//! probed (`div`, `li` and `section`, `marquee`, `table` cells, `b`, `span`,
//! `font`, `blockquote`, lists, `div` and `span`) overflowed between 3,076 and
//! 4,100 deep in a debug build and between 8,196 and 16,388 in a release
//! build.

use crate::routes::FETCH_SHARE_BYTES;

/// The extractor's version, stored with every run.
pub const EXTRACT_V: &str = "extract_v1 lol_html=3.0.3 dom_smoothie=0.19.0";

/// Peak arena bytes per byte of stripped HTML for text-dense pages (the
/// strip output, the DOM, the extraction, the text and the answer):
/// measured 7.8, rounded up with a margin to 10.
pub const EXTRACT_BYTES_PER_HTML_BYTE: usize = 10;

/// Peak arena bytes per element beyond the text term: measured 1,345 to
/// 1,390 at the worst (link-dense pages), rounded up to 2,048.
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

/// Attributes per element the per-element factor was measured with: the
/// `attributes` witness page carries five on every element.
pub const ATTRIBUTES_PER_ELEMENT: usize = 5;

/// Nodes per element a DOM holds at most, besides [`OTHER_NODES`]. A text
/// node follows another text node only where the adoption agency moved an
/// element out from between them, which creates an element, and the strip
/// pass removes comments and templates; so each element brings at most three
/// text nodes (a first child, one after it, one where a moved element was).
/// The per-element factor was measured with two (the `anchors` witness);
/// its rounding margin, at least 650 B per element, covers the third.
pub const NODES_PER_ELEMENT: usize = 4;

/// The document node, a doctype, and the text nodes the document and
/// doctype bring.
pub const OTHER_NODES: usize = 4;

/// What a DOM may hold, checked as the tree builder creates it
/// ([`crate::dom`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DomBounds {
    pub elements: usize,
    /// Nodes of every kind, the document node included.
    pub nodes: usize,
    pub attributes: usize,
    /// The deepest node (the document's children are at depth 1).
    pub depth: usize,
}

impl DomBounds {
    /// No bound (measurements and tests only; never compiled into the
    /// Worker).
    #[cfg(test)]
    pub const NONE: DomBounds = DomBounds { elements: usize::MAX, nodes: usize::MAX, attributes: usize::MAX, depth: usize::MAX };

    /// The DOM of a page of `stripped` bytes within `work` bytes of
    /// extraction work: the elements [`page_work`] leaves room for, the nodes
    /// and attributes they cover, and [`DEPTH_BOUND`].
    pub const fn within(stripped: usize, work: usize) -> DomBounds {
        let elements = work.saturating_sub(stripped.saturating_mul(EXTRACT_BYTES_PER_HTML_BYTE)) / BYTES_PER_ELEMENT;
        DomBounds {
            elements,
            nodes: elements.saturating_mul(NODES_PER_ELEMENT).saturating_add(OTHER_NODES),
            attributes: elements.saturating_mul(ATTRIBUTES_PER_ELEMENT),
            depth: DEPTH_BOUND,
        }
    }
}

/// The elements a built DOM counts as in [`page_work`]: its elements, or
/// more when its nodes or attributes exceed what that many elements cover
/// ([`DomBounds::within`]).
pub const fn counted_elements(elements: usize, nodes: usize, attributes: usize) -> usize {
    let by_nodes = nodes.saturating_sub(OTHER_NODES).div_ceil(NODES_PER_ELEMENT);
    let by_attributes = attributes.div_ceil(ATTRIBUTES_PER_ELEMENT);
    let m = if by_nodes > elements { by_nodes } else { elements };
    if by_attributes > m { by_attributes } else { m }
}

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

    #[test]
    fn dom_bounds_are_derived() {
        let b = DomBounds::within(0, FETCH_SHARE_BYTES);
        assert_eq!(b.elements, MAX_ELEMENTS);
        assert_eq!(b.nodes, NODES_PER_ELEMENT * MAX_ELEMENTS + OTHER_NODES);
        assert_eq!(b.attributes, ATTRIBUTES_PER_ELEMENT * MAX_ELEMENTS);
        assert_eq!(b.depth, DEPTH_BOUND);
        let s = 1_000_000;
        let b = DomBounds::within(s, FETCH_SHARE_BYTES);
        assert!(fits(s, b.elements) && !fits(s, b.elements + 1));
        // A DOM within the bounds counts as no more than they allow.
        assert_eq!(counted_elements(b.elements, b.nodes, b.attributes), b.elements);
        assert!(fits(s, counted_elements(b.elements, b.nodes, b.attributes)));
        // Nodes or attributes beyond what the elements cover count.
        assert_eq!(counted_elements(10, 4 * 100 + OTHER_NODES, 0), 100);
        assert_eq!(counted_elements(10, 0, 5 * 300), 300);
        assert_eq!(DomBounds::within(HTML_BOUND * 2, FETCH_SHARE_BYTES).elements, 0);
    }
}
