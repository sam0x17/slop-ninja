//! Native witness tests on the bounded arena, and the measurement behind
//! [`crate::routes::RENDER_WORK_BYTES`] (`cargo test --features
//! arena-witness arena_witness -- --test-threads=1 --nocapture`).
//!
//! The arena is this test binary's global allocator (lib.rs). Each witness
//! renders a card under `Mode::Strict` inside one scope of the render work's
//! footprint and fails on any allocation outside it or anything left live
//! at the end. Native sizes are 64-bit, so they bound the wasm32 sizes of
//! the same structures from above.

use crate::render::card_png;
use crate::routes::{R_RENDER_WORK, RENDER_WORK_BYTES};
use crate::wire::{REQUEST_BYTES, RenderRequest};
use slop_ninja_arena::{Closed, Mode, PAGE, Scope, set_mode, strict};
use slop_ninja_detect_page::{Band, Card, GaugeEnds, Origins};

fn origins() -> Origins {
    Origins::from_vars("https://slop.ninja", "https://s.slop.ninja").unwrap()
}

/// Renders `card` in a strict scope of the render work's footprint; the
/// PNG's length and the scope's peak in bytes.
fn witness(name: &str, card: &Card, ends: GaugeEnds) -> (usize, usize) {
    let prev = set_mode(Mode::Strict);
    let _ = strict::take();
    let armed = strict::arm();
    let scope = Scope::open(R_RENDER_WORK, RENDER_WORK_BYTES).expect("the render work is admitted");
    let len = scope.enter(|| card_png(card, &origins(), ends).map(|p| p.len()));
    let st = scope.stats();
    let closed = scope.close();
    drop(armed);
    set_mode(prev);
    let v = strict::take();
    assert!(v.is_clean(), "{name}: the render broke its footprint of {RENDER_WORK_BYTES} B: {v:?}");
    assert!(matches!(closed, Closed::Released { .. }), "{name}: the render left values live: {closed:?}");
    let len = len.unwrap_or_else(|e| panic!("{name}: {e:?}"));
    (len, st.peak_pages as usize * PAGE)
}

fn fixture_cards() -> Vec<(String, Card)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../crates/detect-page/tests/fixtures/cards");
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        out.push((name, serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn body_len(c: &Card) -> usize {
    serde_json::to_vec(&serde_json::json!({ "card": c })).unwrap().len()
}

/// The section ladder that draws the most cells a request body can carry:
/// `base` with a ladder alternating the two shortest band names, `weak` and
/// `strong` (8 body bytes an entry on average), so that no two neighbours
/// merge into one run, filled to the last entry that fits in
/// [`REQUEST_BYTES`].
fn longest_ladder(base: &Card) -> Card {
    let band = |i: usize| if i.is_multiple_of(2) { Band::Weak } else { Band::Strong };
    // An entry adds its quoted name and a comma: 7 bytes for `weak`, 9 for
    // `strong`. The first adds no comma, hence the `- 1`.
    let entry = |i: usize| if i.is_multiple_of(2) { 7 } else { 9 };
    let mut c = base.clone();
    c.ladder = Vec::new();
    let mut len = body_len(&c) - 1;
    let mut n = 0usize;
    while len + entry(n) <= REQUEST_BYTES {
        len += entry(n);
        n += 1;
    }
    c.ladder = (0..n).map(band).collect();
    assert_eq!(body_len(&c), len);
    let mut more = c.clone();
    more.ladder.push(band(n));
    assert!(body_len(&more) > REQUEST_BYTES, "the ladder holds the last entry that fits");
    c
}

#[test]
fn arena_witness_render_footprint() {
    let mut peak_max = 0usize;
    let mut cards = fixture_cards();
    let paid = cards.iter().find(|(n, _)| n.starts_with("paid")).map(|(_, c)| c.clone()).expect("a paid fixture");
    let long = longest_ladder(&paid);
    let body = serde_json::to_vec(&serde_json::json!({ "card": long })).unwrap();
    let _: RenderRequest = serde_json::from_slice(&body).unwrap();
    eprintln!("arena witness: longest ladder {} sections in {} B", long.ladder.len(), body.len());
    cards.push(("paid-longest-ladder".into(), long));
    for (name, card) in &cards {
        for ends in [GaugeEnds::Icons, GaugeEnds::Letters] {
            let (len, peak) = witness(name, card, ends);
            eprintln!("arena witness: {name} ({}): png {len} B, peak {peak} B", ends.as_str());
            peak_max = peak_max.max(peak);
        }
    }
    eprintln!("arena witness: largest render peak {peak_max} B of the model's {RENDER_WORK_BYTES} B");
    assert!(peak_max <= RENDER_WORK_BYTES);
}
