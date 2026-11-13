//! Native witness tests on the bounded arena, and the measurements behind
//! [`crate::bound`] (`cargo test --features arena-witness arena_witness --
//! --test-threads=1 --nocapture`).
//!
//! The arena is this test binary's global allocator (lib.rs). Each witness
//! runs a heavy native path under `Mode::Strict` inside one scope and fails
//! on any allocation outside it or anything left live at the end. Native
//! sizes are 64-bit, so they bound the wasm32 sizes of the same structures
//! from above.
//!
//! - The statics home: the warm-up initializes the extraction stack's lazy
//!   statics inside [`STATICS_HOME_BYTES`].
//! - The extraction phase (strip, shape, extract, the answer's JSON) on
//!   text-dense, element-dense, attribute-heavy, deep and bound-sized pages:
//!   its peak footprint stays within the fetch route's control plus
//!   [`crate::bound::page_work`] of the page's stripped bytes and elements.
//!   The implied factors are printed for re-measurement after a dependency
//!   update.
//! - Pages the DOM build refuses (trees deeper than their tags, formatting
//!   elements reconstructed into every block with and without many
//!   attributes, attribute-dense tags): refused within the fetch route's
//!   control plus one share.
//! - A robots.txt at the parse bound with the densest rules: within the
//!   robots route's control plus [`ROBOTS_WORK_BYTES`].

use crate::bound::{BYTES_PER_ELEMENT, DEPTH_BOUND, EXTRACT_BYTES_PER_HTML_BYTE, HTML_BOUND, page_work};
use crate::extract::{self, tests::article};
use crate::routes::{
    FETCH_CONTROL_BYTES, FETCH_SHARE_BYTES, R_FETCH, R_ROBOTS, R_STATICS, ROBOTS_CONTROL_BYTES, ROBOTS_WORK_BYTES, STATICS_HOME_BYTES,
};
use crate::strip::{self, Stripper};
use slop_ninja_arena::{Closed, Mode, PAGE, Route, Scope, set_mode, strict};
use slop_ninja_detect_fetch_wire::robots::{ROBOTS_PARSE_BYTES, RobotsRules};
use slop_ninja_detect_fetch_wire::{PRODUCT_TOKEN, RobotsAnswer, RobotsOutcome};
use std::cell::RefCell;

thread_local! {
    static HOME: RefCell<Option<Scope>> = const { RefCell::new(None) };
}

/// Opens the statics home and warms the statics in it, once per process,
/// witnessing that the warm-up fits its footprint.
fn home() {
    HOME.with(|h| {
        if h.borrow().is_some() {
            return;
        }
        let prev = set_mode(Mode::Strict);
        let _ = strict::take();
        let armed = strict::arm();
        let s = Scope::open(R_STATICS, STATICS_HOME_BYTES).expect("the statics home is admitted");
        s.set_permanent();
        s.enter(extract::warm_up);
        let st = s.stats();
        drop(armed);
        set_mode(prev);
        let v = strict::take();
        eprintln!("arena witness: statics home peak {} pages, {} B live after warm-up", st.peak_pages, st.live);
        assert!(v.is_clean(), "the warm-up broke the statics home's footprint of {STATICS_HOME_BYTES} B: {v:?}");
        *h.borrow_mut() = Some(s);
    });
}

/// Runs `f` in a strict scope of `control` bytes; its result and the
/// scope's peak footprint in bytes.
fn witness<R>(route: Route, control: usize, f: impl FnOnce() -> R) -> (R, usize) {
    home();
    let prev = set_mode(Mode::Strict);
    let _ = strict::take();
    let armed = strict::arm();
    let scope = Scope::open(route, control).unwrap_or_else(|r| panic!("route {} admitted: {r:?}", route.name));
    let out = scope.enter(f);
    let st = scope.stats();
    let closed = scope.close();
    drop(armed);
    set_mode(prev);
    let v = strict::take();
    assert!(v.is_clean(), "route {} broke its footprint of {control} B: {v:?}", route.name);
    assert!(matches!(closed, Closed::Released { .. }), "route {} left values live: {closed:?}", route.name);
    (out, st.peak_pages as usize * PAGE)
}

/// The scope a measurement runs in: room for any page at the bound.
const MEASURE_BYTES: usize = 72 * 1024 * 1024;

struct Page {
    stripped: usize,
    elements: usize,
}

/// The extraction phase on `html`, as the Worker runs it after the body
/// arrives in page-sized windows.
fn pipeline(html: &[u8]) -> Page {
    let mut s = Stripper::new(HTML_BOUND, None);
    for w in html.chunks(PAGE - 64) {
        assert!(s.write(w));
    }
    let st = s.finish().expect("stripped");
    let shape = strip::shape(&st.html, DEPTH_BOUND, usize::MAX).expect("within the depth bound");
    let ex = extract::extract_unbounded(&st.html, "https://example.com/page");
    let text = ex.map(|e| e.text).unwrap_or_default();
    let answer = serde_json::to_vec(&text).expect("json");
    assert!(answer.len() >= text.len());
    Page { stripped: st.html.len(), elements: shape.elements }
}

/// Measures one page: asserts its peak fits the model and prints the
/// implied per-byte factor.
fn measure(name: &str, html: &str) -> (usize, Page) {
    let (page, peak) = witness(R_FETCH.route(), MEASURE_BYTES, || pipeline(html.as_bytes()));
    let model = FETCH_CONTROL_BYTES + page_work(page.stripped, page.elements);
    eprintln!(
        "arena witness: {name}: html {} B, stripped {} B, {} elements, peak {} B ({:.1} B per stripped byte), model {} B",
        html.len(),
        page.stripped,
        page.elements,
        peak,
        peak as f64 / page.stripped.max(1) as f64,
        model
    );
    assert!(peak <= model, "{name}: peak {peak} B exceeds the model's {model} B");
    (peak, page)
}

fn element_dense(n: usize) -> String {
    format!("<html><body><main>{}</main></body></html>", "<div><span>w</span></div>\n".repeat(n))
}

#[test]
fn arena_witness_extraction_factors() {
    let (small, _) = measure("text 100 KB", &article(160, 100));
    let (large, page) = measure("text 1 MB", &article(1600, 100));
    let per_byte = (large - small) as f64 / (page.stripped as f64 * 0.9);
    eprintln!("arena witness: text factor about {per_byte:.1} B per stripped byte (model {EXTRACT_BYTES_PER_HTML_BYTE})");
    for (name, html) in [
        ("element-dense", element_dense(30_000)),
        ("anchors", format!("<html><body><p>{}</p></body></html>", "<a href=\"/x\">w</a> ".repeat(40_000))),
        (
            "attributes",
            format!(
                "<html><body><article>{}</article></body></html>",
                "<p class=\"a b c d\" id=\"p\" title=\"some title text\" lang=\"en\" dir=\"ltr\">the quiet harbour keeps its boats.</p>\n"
                    .repeat(6_000)
            ),
        ),
    ] {
        let (peak, page) = measure(name, &html);
        let per_element = peak.saturating_sub(page.stripped * EXTRACT_BYTES_PER_HTML_BYTE) / page.elements.max(1);
        eprintln!("arena witness: {name}: about {per_element} B per element beyond the text term (model {BYTES_PER_ELEMENT})");
    }
    let deep = format!(
        "<html><body>{}{}{}</body></html>",
        "<div class=\"content\">".repeat(DEPTH_BOUND - 8),
        article(40, 60),
        "</div>".repeat(DEPTH_BOUND - 8)
    );
    measure("deep", &deep);
}

#[test]
fn arena_witness_a_page_at_the_bound_fits_its_share() {
    // A text page whose stripped size is just under the bound.
    let mut n = 1000;
    let mut html = article(n, 100);
    while html.len() < HTML_BOUND * 9 / 10 {
        n += 500;
        html = article(n, 100);
    }
    let (_, page) = measure("text at the bound", &html);
    assert!(page.stripped <= HTML_BOUND);
}

/// Pages the DOM build refuses are refused within the fetch route's
/// control plus one share: the build stops while the tree grows, so a page
/// that would build hundreds of megabytes never does.
#[test]
fn arena_witness_refused_doms_stay_within_the_share() {
    use crate::dom::tests::{li_section_page, p_marquee_page, reconstruction_page};
    let tag = format!("<p{}>x</p>", (0..1000).map(|a| format!(" a{a}")).collect::<String>());
    for (name, html, want) in [
        ("li-section", li_section_page(), extract::ExtractError::TooDeep),
        ("p-marquee", p_marquee_page(), extract::ExtractError::TooDeep),
        // 20,504 tags nesting 504 deep; 10 million elements if built.
        ("reconstruction", reconstruction_page(500, 0, 20_000), extract::ExtractError::TooManyElements),
        // 2,001 tags; 2 million attributes if built.
        ("reconstructed attributes", reconstruction_page(1, 1000, 2000), extract::ExtractError::TooManyElements),
        ("attribute-dense", tag.repeat(800), extract::ExtractError::TooManyElements),
    ] {
        let ((stripped, elements, err), peak) = witness(R_FETCH.route(), MEASURE_BYTES, || {
            let mut s = Stripper::new(HTML_BOUND, None);
            for w in html.as_bytes().chunks(PAGE - 64) {
                assert!(s.write(w));
            }
            let st = s.finish().expect("stripped");
            let shape = strip::shape(&st.html, DEPTH_BOUND, crate::bound::MAX_ELEMENTS).expect("within the estimate's bounds");
            assert!(crate::bound::fits(st.html.len(), shape.elements));
            let err = extract::extract(&st.html, "https://example.com/page").err();
            (st.html.len(), shape.elements, err)
        });
        eprintln!("arena witness: refused {name}: stripped {stripped} B, {elements} tags, {err:?}, peak {peak} B");
        assert_eq!(err, Some(want), "{name}");
        assert!(peak <= FETCH_CONTROL_BYTES + FETCH_SHARE_BYTES, "{name}: peak {peak} B exceeds one share");
    }
}

#[test]
fn arena_witness_robots_at_the_parse_bound() {
    let body = format!("User-agent: *\n{}", "Allow:/\n".repeat((ROBOTS_PARSE_BYTES - 16) / 8)).into_bytes();
    let ((rules, json_len), peak) = witness(R_ROBOTS.route(), MEASURE_BYTES, || {
        let rules = RobotsRules::parse(&body, PRODUCT_TOKEN);
        let n = rules.rules.len();
        let answer = RobotsAnswer { v: "v".into(), outcome: RobotsOutcome::Fetched, rules, cache_ms: 0, hops: 0 };
        let json = serde_json::to_vec(&answer).expect("json");
        (n, json.len())
    });
    eprintln!("arena witness: robots {} B, {rules} rules, answer {json_len} B, peak {peak} B (work {ROBOTS_WORK_BYTES} B)", body.len());
    assert!(rules > 60_000);
    assert!(peak <= ROBOTS_CONTROL_BYTES + ROBOTS_WORK_BYTES, "robots peak {peak} B exceeds the model");
}

/// After only the production warm-up, typical page shapes extract with no
/// value outliving their scope: the warm-up initialized the lazy statics
/// their paths use. (A static first reached later pins at most the page
/// holding it, once per isolate; the arena counts it.)
#[test]
fn arena_witness_the_warm_up_covers_typical_pages() {
    let para = "<p>The quiet harbour keeps its boats, and the evening light settles on the water as the town goes still.</p>";
    let shapes = [
        ("article", article(40, 60)),
        (
            "news",
            format!(
                "<html lang=\"en\"><head><title>T</title><meta name=\"author\" content=\"A\"></head><body><header><nav><a href=\"/\">Home</a></nav></header>\
                 <main><article><h1>Headline</h1><div class=\"byline\">By A</div>{}<h2>Section</h2>{}<figure><figcaption>Photo</figcaption></figure>{}</article>\
                 <aside>Related</aside></main><footer>F</footer></body></html>",
                para.repeat(8),
                para.repeat(6),
                para.repeat(4)
            ),
        ),
        ("lists", format!("<html><body><div class=\"content\"><h1>List</h1>{}<ul>{}</ul><ol>{}</ol>{}</div></body></html>", para.repeat(3), "<li>An item of the list with several words in it.</li>".repeat(30), "<li><p>Numbered item paragraph with words.</p></li>".repeat(10), para.repeat(3))),
        ("tables", format!("<html><body><div id=\"post\">{}<table><tr><th>A</th><th>B</th></tr>{}</table>{}</div></body></html>", para.repeat(4), "<tr><td>one</td><td>two</td></tr>".repeat(30), para.repeat(4))),
        ("divs", format!("<html><body>{}</body></html>", "<div class=\"text\"><span>The quiet harbour keeps its boats as the evening light settles on the water and the town goes still.</span></div>".repeat(40))),
        ("short", "<html><body><p>Too short to be an article.</p></body></html>".to_owned()),
    ];
    for (name, html) in shapes {
        let ((), peak) = witness(R_FETCH.route(), MEASURE_BYTES, || {
            pipeline(html.as_bytes());
        });
        eprintln!("arena witness: warm-up coverage: {name} ok, peak {peak} B");
    }
}
