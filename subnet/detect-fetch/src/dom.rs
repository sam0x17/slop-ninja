//! The DOM the extractor reads, built by html5ever into dom_query's tree
//! with the options dom_smoothie's own `Readability::new` uses (scripting
//! off), but bounded while it grows (risks F11 and F13).
//!
//! The strip pass's [`crate::strip::shape`] sees only tags. The tree
//! builder decides the nesting with its scopes, so a page can nest far
//! deeper than its tags suggest (an `li` does not close an `li` across a
//! `section`, nor a `p` close a `p` across a `marquee`), and it creates
//! elements the page has no tags for: it reconstructs open formatting
//! elements, attributes and all, in every later block, and the adoption
//! agency clones them. So the build here checks the tree itself:
//!
//! - The stripped HTML is fed in pieces of at most one tag and the text
//!   after it. Its sink wraps dom_query's and counts every element and
//!   attribute it creates.
//! - After each piece the counts are checked against [`DomBounds`]; past one
//!   the build stops with [`DomError::TooLarge`]. Within one piece the tree
//!   grows only by what one tag and the text after it make: implied
//!   elements, reconstructions of the open formatting elements (at most one
//!   element per formatting tag of the page each) and one run of the
//!   adoption agency. An element created after a bound was crossed gets no
//!   attributes.
//! - After each piece the newest element's depth is read from its parent
//!   links, and after the last piece an iterative walk (no recursion) finds
//!   the deepest node, template contents included: past the depth bound the
//!   build stops with [`DomError::TooDeep`], before any extraction
//!   recurses over the tree.
//! - Parse errors are dropped rather than kept (dom_query keeps one entry
//!   per error, and nothing reads them).

use crate::bound::DomBounds;
use dom_query::{Document, NodeId, NodeRef, Tree};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tokenizer::TokenizerOpts;
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeBuilderOpts, TreeSink};
use html5ever::{Attribute, ParseOpts, QualName, parse_document};
use std::borrow::Cow;
use std::cell::{Cell, Ref};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomError {
    /// A node lies deeper than the depth bound.
    TooDeep,
    /// The tree holds more elements, nodes or attributes than its bounds.
    TooLarge,
}

/// A DOM within its bounds, and what it holds.
pub struct Built {
    pub doc: Document,
    pub elements: usize,
    /// Nodes of every kind, the document node included.
    pub nodes: usize,
    pub attributes: usize,
    /// The deepest node's depth (the document's children are at depth 1).
    pub depth: usize,
}

impl Built {
    /// The elements this DOM counts as ([`crate::bound::counted_elements`]).
    pub fn counted_elements(&self) -> usize {
        crate::bound::counted_elements(self.elements, self.nodes, self.attributes)
    }
}

/// dom_query's sink, counting what the tree builder creates.
struct Counting {
    doc: Document,
    bounds: DomBounds,
    elements: Cell<usize>,
    attributes: Cell<usize>,
    newest: Cell<Option<NodeId>>,
    over: Cell<bool>,
}

impl Counting {
    fn over(&self) -> bool {
        self.over.get() || self.doc.tree.len() > self.bounds.nodes
    }
}

impl TreeSink for Counting {
    type Handle = NodeId;
    type Output = Self;
    type ElemName<'a>
        = Ref<'a, QualName>
    where
        Self: 'a;

    fn finish(self) -> Self {
        self
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> NodeId {
        TreeSink::get_document(&self.doc)
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> Ref<'a, QualName> {
        TreeSink::elem_name(&self.doc, target)
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> NodeId {
        let attrs = if self.over.get() { Vec::new() } else { attrs };
        let elements = self.elements.get().saturating_add(1);
        let attributes = self.attributes.get().saturating_add(attrs.len());
        self.elements.set(elements);
        self.attributes.set(attributes);
        if elements > self.bounds.elements || attributes > self.bounds.attributes || self.doc.tree.len() > self.bounds.nodes {
            self.over.set(true);
        }
        let id = TreeSink::create_element(&self.doc, name, attrs, flags);
        self.newest.set(Some(id));
        id
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        TreeSink::create_comment(&self.doc, text)
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> NodeId {
        TreeSink::create_pi(&self.doc, target, data)
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        TreeSink::append(&self.doc, parent, child);
    }

    fn append_based_on_parent_node(&self, element: &NodeId, prev_element: &NodeId, child: NodeOrText<NodeId>) {
        TreeSink::append_based_on_parent_node(&self.doc, element, prev_element, child);
    }

    fn append_doctype_to_document(&self, name: StrTendril, public_id: StrTendril, system_id: StrTendril) {
        TreeSink::append_doctype_to_document(&self.doc, name, public_id, system_id);
    }

    fn mark_script_already_started(&self, node: &NodeId) {
        TreeSink::mark_script_already_started(&self.doc, node);
    }

    fn pop(&self, node: &NodeId) {
        TreeSink::pop(&self.doc, node);
    }

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        TreeSink::get_template_contents(&self.doc, target)
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        TreeSink::same_node(&self.doc, x, y)
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        TreeSink::set_quirks_mode(&self.doc, mode);
    }

    fn append_before_sibling(&self, sibling: &NodeId, new_node: NodeOrText<NodeId>) {
        TreeSink::append_before_sibling(&self.doc, sibling, new_node);
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        let attributes = self.attributes.get().saturating_add(attrs.len());
        self.attributes.set(attributes);
        if attributes > self.bounds.attributes {
            self.over.set(true);
            return;
        }
        TreeSink::add_attrs_if_missing(&self.doc, target, attrs);
    }

    fn associate_with_form(&self, target: &NodeId, form: &NodeId, nodes: (&NodeId, Option<&NodeId>)) {
        TreeSink::associate_with_form(&self.doc, target, form, nodes);
    }

    fn remove_from_parent(&self, target: &NodeId) {
        TreeSink::remove_from_parent(&self.doc, target);
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        TreeSink::reparent_children(&self.doc, node, new_parent);
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &NodeId) -> bool {
        TreeSink::is_mathml_annotation_xml_integration_point(&self.doc, handle)
    }

    fn set_current_line(&self, line_number: u64) {
        TreeSink::set_current_line(&self.doc, line_number);
    }

    fn allow_declarative_shadow_roots(&self, intended_parent: &NodeId) -> bool {
        TreeSink::allow_declarative_shadow_roots(&self.doc, intended_parent)
    }

    fn attach_declarative_shadow(&self, location: &NodeId, template: &NodeId, attrs: &[Attribute]) -> bool {
        TreeSink::attach_declarative_shadow(&self.doc, location, template, attrs)
    }

    fn maybe_clone_an_option_into_selectedcontent(&self, option: &NodeId) {
        TreeSink::maybe_clone_an_option_into_selectedcontent(&self.doc, option);
    }
}

/// `html` in pieces that each start at a tag (a `<` before an ASCII letter,
/// `/`, `!` or `?`, the tokenizer's tag openings) and hold at most one.
fn pieces(html: &str) -> impl Iterator<Item = &str> {
    let b = html.as_bytes();
    let opens = move |i: usize| b.get(i) == Some(&b'<') && b.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic() || b"/!?".contains(c));
    let mut start = 0usize;
    std::iter::from_fn(move || {
        if start >= b.len() {
            return None;
        }
        let mut end = start + 1;
        loop {
            match b.get(end..).and_then(|s| s.iter().position(|&c| c == b'<')) {
                Some(p) if opens(end + p) => {
                    end += p;
                    break;
                }
                Some(p) => end += p + 1,
                None => {
                    end = b.len();
                    break;
                }
            }
        }
        // Every split is at an ASCII '<', so at a character boundary.
        let piece = html.get(start..end);
        start = end;
        piece
    })
}

/// The depth of `id` from its parent links (the document's children are at
/// depth 1), counted up to `limit + 1` at most.
fn depth_of(tree: &Tree, id: NodeId, limit: usize) -> usize {
    let mut d = 0usize;
    let mut cur = id;
    while let Some(p) = tree.parent_of(&cur) {
        d += 1;
        if d > limit {
            break;
        }
        cur = p.id;
    }
    d
}

/// The depth of the deepest node of `doc` (the document's children are at
/// depth 1, a template's contents one below the template), walked in tree
/// order by the parent, child and sibling links, never by recursion. The
/// walk stops once it passes `limit`, with a depth above it.
pub fn depth(doc: &Document, limit: usize) -> usize {
    let mut max = 0usize;
    // Subtrees still to walk: the document, then each template's contents,
    // with the depth of the node they hang below.
    let mut roots: Vec<(NodeId, usize)> = vec![(doc.root().id, 0)];
    while let Some((root, base)) = roots.pop() {
        let mut node = NodeRef::new(root, &doc.tree).first_child();
        let mut d = base + 1;
        while let Some(n) = node {
            if d > max {
                max = d;
                if max > limit {
                    return max;
                }
            }
            if let Some(contents) = n.element_ref().and_then(|e| e.template_contents) {
                roots.push((contents, d));
            }
            if let Some(c) = n.first_child() {
                node = Some(c);
                d += 1;
                continue;
            }
            let mut cur = n;
            node = loop {
                if let Some(s) = cur.next_sibling() {
                    break Some(s);
                }
                if d <= base + 1 {
                    break None;
                }
                match cur.parent() {
                    Some(p) => {
                        cur = p;
                        d -= 1;
                    }
                    None => break None,
                }
            };
        }
    }
    max
}

/// Builds the DOM of `html` within `bounds`.
pub fn build(html: &str, bounds: &DomBounds) -> Result<Built, DomError> {
    let sink = Counting {
        doc: Document::default(),
        bounds: *bounds,
        elements: Cell::new(0),
        attributes: Cell::new(0),
        newest: Cell::new(None),
        over: Cell::new(false),
    };
    // dom_query's `Document::from` options.
    let opts = ParseOpts {
        tokenizer: TokenizerOpts::default(),
        tree_builder: TreeBuilderOpts { scripting_enabled: false, ..TreeBuilderOpts::default() },
    };
    let mut parser = parse_document(sink, opts);
    let mut checked: Option<NodeId> = None;
    for piece in pieces(html) {
        parser.process(StrTendril::from_slice(piece));
        let sink = &parser.tokenizer.sink.sink;
        if sink.over() {
            return Err(DomError::TooLarge);
        }
        let newest = sink.newest.get();
        if newest != checked {
            checked = newest;
            if let Some(id) = newest
                && depth_of(&sink.doc.tree, id, bounds.depth) > bounds.depth
            {
                return Err(DomError::TooDeep);
            }
        }
    }
    let sink = parser.finish();
    if sink.over() {
        return Err(DomError::TooLarge);
    }
    let depth = depth(&sink.doc, bounds.depth);
    if depth > bounds.depth {
        return Err(DomError::TooDeep);
    }
    Ok(Built { elements: sink.elements.get(), nodes: sink.doc.tree.len(), attributes: sink.attributes.get(), depth, doc: sink.doc })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::bound::{DEPTH_BOUND, HTML_BOUND, MAX_ELEMENTS, fits};
    use crate::routes::FETCH_SHARE_BYTES;
    use crate::strip::{Stripper, shape};

    /// Deep page 1: `li` and `section` alternating 10,000 times. An `li`
    /// closes an open `li` only when no special element other than
    /// `address`, `div` or `p` lies between them, and `section` is special,
    /// so the DOM nests 20,005 deep; a tag-level estimate that pops to any
    /// open `li` sees 6.
    pub(crate) fn li_section_page() -> String {
        format!(
            "<!doctype html><html><head><title>t</title></head><body><ul>{}<p>{}</p></body></html>",
            "<li><section>".repeat(10_000),
            "word ".repeat(400)
        )
    }

    /// Deep page 2: a `p`, 140 `marquee`s and a `div`, 140 times. The `div`
    /// closes no `p`, which lies outside button scope behind the
    /// `marquee`s, so the DOM nests 19,884 deep; the estimate sees 282.
    pub(crate) fn p_marquee_page() -> String {
        let mut s = String::from("<!doctype html><html><head><title>t</title></head><body>");
        for _ in 0..140 {
            s.push_str("<p>");
            s.push_str(&"<marquee>".repeat(140));
            s.push_str("<div>");
        }
        s.push_str("<p>");
        s.push_str(&"word ".repeat(400));
        s.push_str("</p></body></html>");
        s
    }

    /// `n` distinct formatting elements left open, then `blocks` blocks
    /// with text: the tree builder reconstructs all `n` in every block.
    pub(crate) fn reconstruction_page(n: usize, attrs: usize, blocks: usize) -> String {
        let mut s = String::from("<!doctype html><html><body><div>");
        for i in 0..n {
            s.push_str(&format!("<b id=b{i}"));
            for a in 0..attrs {
                s.push_str(&format!(" a{a}"));
            }
            s.push('>');
        }
        s.push_str("</div>");
        s.push_str(&"<div>x</div>".repeat(blocks));
        s.push_str("</body></html>");
        s
    }

    pub(crate) fn stripped(html: &str) -> String {
        let mut st = Stripper::new(HTML_BOUND, None);
        assert!(st.write(html.as_bytes()));
        st.finish().unwrap().html
    }

    fn page_bounds(html: &str) -> DomBounds {
        DomBounds::within(html.len(), FETCH_SHARE_BYTES)
    }

    /// The deepest node, by recursion over the children and template
    /// contents (small pages only), independent of [`depth`].
    fn reference_depth(doc: &Document) -> usize {
        fn below(n: NodeRef<'_>, d: usize) -> usize {
            let mut m = d;
            for c in n.children_it(false) {
                m = m.max(below(c, d + 1));
            }
            if let Some(t) = n.element_ref().and_then(|e| e.template_contents) {
                m = m.max(below(NodeRef::new(t, n.tree), d));
            }
            m
        }
        below(doc.root(), 0)
    }

    /// The deepest node by counting each node's ancestors (the tree without
    /// template contents).
    fn ancestors_depth(doc: &Document) -> usize {
        doc.root().descendants_it().map(|n| n.ancestors_it(None).count()).max().unwrap_or(0)
    }

    const PAGES: &[&str] = &[
        "<html><body><div><p>a<p>b<p>c</div><ul><li>x<li>y</ul></body></html>",
        "<p>unclosed <b>bold <i>both</b> italic</i> after</p><p>next",
        "<div><b><p>misnested</b> tail</p></div>",
        "<table><tr><td>a<td>b<tr><td>c</table><table><caption>c<tr><th>h</table>",
        "<table>foster <b>parented</b><tr><td>x</td></tr></table>",
        "<ul><li><section><li><section><li>deep</ul>",
        "<p><marquee><div>inside</div></marquee><div>after</div>",
        "<dl><dt>a<dd>b<dt>c<blockquote><dd>d</blockquote></dl>",
        "<select><option>a<option>b<optgroup><option>c</select>",
        "<a href=/x>one<a href=/y>two</a> <nobr>n<nobr>m",
        "<h1>a<h2>b</h1>c<button>d<button>e",
        "<title>a <b> b</title><textarea><div></textarea><p>&amp;&lt;&#x41;&notanentity;",
        "line\r\nbreaks\rand\ncarriage<br/>returns<img src=x><hr>",
        "<!-- c --><?pi x?><!x><![CDATA[y]]></ a><p>after",
        "<svg><g><text>v</text></g></svg><math><mi>x</mi></math><p>p",
        "<template><div><p>t</p></div></template><p>after",
        "<frameset><frame></frameset>",
        "<plaintext><div>not a tag",
        "a<b<c <d e=\"<f>\" g='h>i'><<>>",
        "",
    ];

    #[test]
    fn the_build_is_the_parse_dom_smoothie_makes() {
        let warm = crate::extract::tests::warm_pages();
        let pages = PAGES.iter().map(|p| (*p).to_owned()).chain(warm).chain([crate::extract::tests::article(20, 40)]);
        for html in pages {
            let built = build(&html, &DomBounds::NONE).unwrap();
            let whole = Document::from(html.as_str());
            assert_eq!(built.doc.html(), whole.html(), "{html}");
            assert_eq!(built.nodes, whole.tree.len(), "{html}");
            assert_eq!(built.depth, reference_depth(&whole), "{html}");
        }
    }

    #[test]
    fn pieces_hold_one_tag_each() {
        let v: Vec<&str> = pieces("a<b>c</d> e<!--f--><?g><<h<1 i").collect();
        assert_eq!(v, ["a", "<b>c", "</d> e", "<!--f-->", "<?g><", "<h<1 i"]);
        assert_eq!(pieces("").count(), 0);
        assert_eq!(pieces("<<<<").collect::<Vec<_>>(), ["<<<<"]);
        assert_eq!(pieces("é<p>ü").collect::<Vec<_>>(), ["é", "<p>ü"]);
    }

    #[test]
    fn the_walk_is_the_real_depth() {
        for html in PAGES.iter().map(|p| (*p).to_owned()).chain([
            "<ul>".to_owned() + &"<li><section>".repeat(40),
            "<p>".to_owned() + &"<marquee>".repeat(30) + "<div>x",
            "<div>".repeat(60) + &"<b>".repeat(10) + "</div>" + &"<p>y".repeat(3),
            "<table>".repeat(20) + &"<tr><td>".repeat(20),
        ]) {
            let doc = Document::from(html.as_str());
            assert_eq!(depth(&doc, usize::MAX), reference_depth(&doc), "{html}");
            if !html.contains("<template>") {
                assert_eq!(depth(&doc, usize::MAX), ancestors_depth(&doc), "{html}");
            }
        }
        // Template contents count below their template (html 1, body 2,
        // template 3, its contents from 4: the text at 7).
        let doc = Document::from("<body><template><div><div><p>t</p></div></div></template></body>");
        assert_eq!(ancestors_depth(&doc), 3);
        assert_eq!(depth(&doc, usize::MAX), 7);
        // The walk stops past its limit.
        let doc = Document::from("<div>".repeat(100).as_str());
        assert_eq!(depth(&doc, 10), 11);
        assert_eq!(depth(&doc, usize::MAX), 102);
    }

    #[test]
    fn deep_pages_the_estimate_misses_are_refused() {
        for (name, html) in [("li-section", li_section_page()), ("p-marquee", p_marquee_page())] {
            let html = stripped(&html);
            // The tag-level estimate passes them, within every pre-DOM bound.
            let s = shape(&html, DEPTH_BOUND, MAX_ELEMENTS).unwrap();
            assert!(s.depth <= DEPTH_BOUND && fits(html.len(), s.elements), "{name}: {s:?}");
            // The built tree is far deeper; the build stops early.
            assert_eq!(build(&html, &DomBounds::NONE).unwrap().depth, if name == "li-section" { 20_005 } else { 19_884 });
            assert_eq!(build(&html, &page_bounds(&html)).err(), Some(DomError::TooDeep), "{name}");
        }
        // At the bound it builds.
        let html = "<div>".repeat(DEPTH_BOUND - 2);
        let b = DomBounds { depth: DEPTH_BOUND, ..DomBounds::NONE };
        assert_eq!(build(&html, &b).unwrap().depth, DEPTH_BOUND);
        assert_eq!(build(&format!("{html}<div>"), &b).err(), Some(DomError::TooDeep));
        // Depth reached only by a text node, found by the final walk (the
        // newest element is shallow by then).
        let html = "<div>".repeat(DEPTH_BOUND - 3) + "x" + &"</div>".repeat(DEPTH_BOUND - 3) + "<p>shallow";
        assert_eq!(build(&html, &b).unwrap().depth, DEPTH_BOUND);
        assert_eq!(build(&html.replacen("x", "<span>x</span>", 1), &b).err(), Some(DomError::TooDeep));
    }

    #[test]
    fn tree_builder_growth_is_refused() {
        // 300 tags; the tree builder makes 30,000 elements of them.
        let html = stripped(&reconstruction_page(100, 0, 300));
        let s = shape(&html, DEPTH_BOUND, MAX_ELEMENTS).unwrap();
        assert!(s.elements < 500 && fits(html.len(), s.elements), "{s:?}");
        assert!(build(&html, &DomBounds::NONE).unwrap().elements > 30_000);
        let b = page_bounds(&html);
        assert!(b.elements < 30_000);
        assert_eq!(build(&html, &b).err(), Some(DomError::TooLarge));
        // Attributes cloned with each reconstruction: 1,001 tags, a million
        // attributes.
        let html = stripped(&reconstruction_page(1, 1000, 1000));
        assert!(build(&html, &DomBounds::NONE).unwrap().attributes > 1_000_000);
        assert_eq!(build(&html, &page_bounds(&html)).err(), Some(DomError::TooLarge));
        // The page's own attributes count as well.
        let tag = format!("<p{}>x</p>", (0..1000).map(|a| format!(" a{a}")).collect::<String>());
        let html = tag.repeat(200);
        assert_eq!(build(&html, &page_bounds(&html)).err(), Some(DomError::TooLarge));
        // In a body, stray `</p>` end tags each make a `p` element, and the
        // node bound holds text between elements.
        let b = DomBounds { elements: 1000, nodes: 1000, ..DomBounds::NONE };
        let stray = "<body>".to_owned() + &"</p>".repeat(2000);
        assert!(build(&stray, &DomBounds::NONE).unwrap().elements > 2000);
        assert_eq!(build(&stray, &b).err(), Some(DomError::TooLarge));
        assert_eq!(build(&"x<br>".repeat(600), &b).err(), Some(DomError::TooLarge));
        assert!(build(&"x<br>".repeat(300), &b).is_ok());
    }

    #[test]
    fn ordinary_pages_build_within_their_bounds() {
        let html = stripped(&crate::extract::tests::article(200, 60));
        let built = build(&html, &page_bounds(&html)).unwrap();
        assert!(built.elements >= 200 && built.depth <= 8);
        assert!(built.counted_elements() >= built.elements);
        let dense = "<html><body><main>".to_owned() + &"<div><span>w</span></div>\n".repeat(10_000) + "</main></body></html>";
        let built = build(&dense, &page_bounds(&dense)).unwrap();
        assert_eq!(built.elements, 20_004);
        assert!(built.nodes <= 4 * built.elements + 4);
        assert_eq!(built.counted_elements(), built.elements);
    }
}
